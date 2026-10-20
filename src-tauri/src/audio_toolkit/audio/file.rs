//! Decoding an imported audio or video file to 16 kHz mono, block by block:
//! a file of several hours never has to fit in memory.
//!
//! Containers and codecs are symphonia's (WAV, MP3, M4A/AAC/ALAC, FLAC,
//! Ogg/Vorbis, MP4/MOV, MKV/WebM); Opus, which symphonia does not decode, goes
//! through libopus, straight to 16 kHz mono. Video tracks are ignored:
//! only the first decodable audio track is read. Every channel is mixed down
//! and the result resampled to [`WHISPER_SAMPLE_RATE`].

use std::fmt;
use std::fs::File;
use std::io::ErrorKind;
use std::path::Path;

use hound::{SampleFormat, WavSpec, WavWriter};
use log::{debug, warn};
use rubato::audioadapter_buffers::direct::SequentialSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler, WindowFunction};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL, CODEC_TYPE_OPUS};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, Track};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE;

const RATE: usize = WHISPER_SAMPLE_RATE as usize;
/// Output samples gathered before a block is handed out.
const BLOCK_SAMPLES: usize = 10 * RATE;
/// Input frames per resampler run.
const RESAMPLER_CHUNK: usize = 1024;
/// Corrupt packets tolerated in a row before the file is given up on.
const MAX_CONSECUTIVE_ERRORS: usize = 64;

/// Extensions accepted for import, lower case. The frontend refuses anything
/// else before it reaches the backend.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "wav", "wave", "mp3", "m4a", "aac", "flac", "ogg", "oga", "opus", "webm", "mp4", "m4v", "mov",
    "mkv", "mka",
];

/// Whether `path` has an extension the decoder accepts.
pub fn is_supported_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SUPPORTED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Why a file could not be decoded.
#[derive(Debug)]
pub enum DecodeError {
    /// Not a format or codec this decoder knows.
    Unsupported(String),
    /// A readable file with no audio track (a silent video, an image…).
    NoAudioTrack,
    /// The file is damaged or truncated before any audio.
    Corrupt(String),
    /// The file is gone (moved or deleted).
    Missing,
    /// Any other I/O failure, reading the file or writing the output.
    Io(std::io::Error),
    /// Stopped by the caller.
    Cancelled,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Unsupported(what) => write!(f, "unsupported format: {what}"),
            DecodeError::NoAudioTrack => write!(f, "the file has no audio track"),
            DecodeError::Corrupt(what) => write!(f, "the file is damaged: {what}"),
            DecodeError::Missing => write!(f, "the file was moved or deleted"),
            DecodeError::Io(e) => write!(f, "I/O error: {e}"),
            DecodeError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<hound::Error> for DecodeError {
    fn from(e: hound::Error) -> Self {
        match e {
            hound::Error::IoError(e) => DecodeError::Io(e),
            other => DecodeError::Io(std::io::Error::other(other.to_string())),
        }
    }
}

fn io_error(path: &Path, e: std::io::Error) -> DecodeError {
    if e.kind() == ErrorKind::NotFound || !path.exists() {
        DecodeError::Missing
    } else {
        DecodeError::Io(e)
    }
}

/// Maps a symphonia error met while opening the file.
fn open_error(path: &Path, e: SymphoniaError) -> DecodeError {
    match e {
        SymphoniaError::IoError(e) if e.kind() == ErrorKind::UnexpectedEof => {
            DecodeError::Corrupt("unexpected end of file".into())
        }
        SymphoniaError::IoError(e) => io_error(path, e),
        SymphoniaError::Unsupported(what) => DecodeError::Unsupported(what.to_string()),
        other => DecodeError::Corrupt(other.to_string()),
    }
}

/// Mixes interleaved frames down to mono.
fn mix_down(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels <= 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    let scale = 1.0 / channels as f32;
    out.extend(
        interleaved
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() * scale),
    );
}

/// Mono resampling to 16 kHz, fed any number of samples at a time.
struct MonoResampler {
    resampler: Fft<f32>,
    in_buf: Vec<f32>,
    out_buf: Vec<f32>,
    in_hz: usize,
    in_count: usize,
    /// Output samples produced so far, the leading delay included.
    out_count: usize,
    /// Output samples still to drop: the resampler's delay, so the output
    /// starts with the file's first sample and timestamps line up.
    skip: usize,
}

impl MonoResampler {
    fn new(in_hz: usize) -> Result<Self, DecodeError> {
        let resampler = Fft::<f32>::new_custom(
            in_hz,
            RATE,
            RESAMPLER_CHUNK,
            1,
            1,
            WindowFunction::BlackmanHarris2,
            FixedSync::Input,
        )
        .map_err(|e| DecodeError::Unsupported(format!("{in_hz} Hz audio: {e}")))?;
        let out_max = resampler.output_frames_max();
        let skip = resampler.output_delay();
        Ok(Self {
            resampler,
            in_buf: Vec::with_capacity(RESAMPLER_CHUNK),
            out_buf: vec![0.0; out_max],
            in_hz,
            in_count: 0,
            out_count: 0,
            skip,
        })
    }

    fn push(&mut self, mut src: &[f32], out: &mut Vec<f32>) -> Result<(), DecodeError> {
        self.in_count += src.len();
        while !src.is_empty() {
            let take = (RESAMPLER_CHUNK - self.in_buf.len()).min(src.len());
            self.in_buf.extend_from_slice(&src[..take]);
            src = &src[take..];
            if self.in_buf.len() == RESAMPLER_CHUNK {
                let frames = self.process()?;
                self.emit(frames, out);
            }
        }
        Ok(())
    }

    /// Flushes what the resampler still holds, zero-padded, keeping only
    /// output that stems from real input.
    fn finish(&mut self, out: &mut Vec<f32>) -> Result<(), DecodeError> {
        let expected = self.in_count * RATE / self.in_hz + self.resampler.output_delay();
        // Each round pushes a whole chunk of zeros through; a few are enough
        // to empty the delay line.
        for _ in 0..8 {
            if self.out_count >= expected {
                break;
            }
            let frames = self.process()?;
            let frames = frames.min(expected - self.out_count);
            self.emit(frames, out);
        }
        Ok(())
    }

    /// Resamples `in_buf` (zero-padded to a whole chunk) into `out_buf`.
    fn process(&mut self) -> Result<usize, DecodeError> {
        let in_frames = self.in_buf.len();
        self.in_buf.resize(RESAMPLER_CHUNK, 0.0);
        let out_frames = self.out_buf.len();
        let indexing = Indexing::new().partial_len(in_frames);
        let input = SequentialSlice::new(&self.in_buf, 1, RESAMPLER_CHUNK)
            .map_err(|e| DecodeError::Corrupt(e.to_string()))?;
        let mut output = SequentialSlice::new_mut(&mut self.out_buf, 1, out_frames)
            .map_err(|e| DecodeError::Corrupt(e.to_string()))?;
        let result = self
            .resampler
            .process_into_buffer(&input, &mut output, Some(&indexing))
            .map_err(|e| DecodeError::Corrupt(format!("resampling failed: {e}")));
        self.in_buf.clear();
        Ok(result?.1)
    }

    fn emit(&mut self, frames: usize, out: &mut Vec<f32>) {
        self.out_count += frames;
        let skip = self.skip.min(frames);
        self.skip -= skip;
        out.extend_from_slice(&self.out_buf[skip..frames]);
    }
}

/// Opus packets, decoded straight to 16 kHz mono (libopus mixes and
/// resamples itself).
struct OpusTrack {
    decoder: opus::Decoder,
    pcm: Vec<f32>,
    /// Output samples still to drop: the encoder's pre-skip.
    skip: usize,
}

impl OpusTrack {
    fn new(track: &Track) -> Result<Self, DecodeError> {
        let decoder = opus::Decoder::new(WHISPER_SAMPLE_RATE, opus::Channels::Mono)
            .map_err(|e| DecodeError::Unsupported(format!("Opus: {e}")))?;
        // Pre-skip is counted at 48 kHz: from the codec delay (Ogg), else
        // the `OpusHead` the container carries (Matroska).
        let pre_skip_48k = track.codec_params.delay.map(|d| d as usize).or_else(|| {
            track
                .codec_params
                .extra_data
                .as_deref()
                .filter(|head| head.len() >= 12 && head.starts_with(b"OpusHead"))
                .map(|head| u16::from_le_bytes([head[10], head[11]]) as usize)
        });
        // The longest packet: 120 ms.
        let pcm = vec![0.0; RATE * 120 / 1_000];
        Ok(Self {
            decoder,
            pcm,
            skip: pre_skip_48k.unwrap_or(0) * RATE / 48_000,
        })
    }

    fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<(), String> {
        let samples = self
            .decoder
            .decode_float(packet, &mut self.pcm, false)
            .map_err(|e| e.to_string())?;
        let skip = self.skip.min(samples);
        self.skip -= skip;
        out.extend_from_slice(&self.pcm[skip..samples]);
        Ok(())
    }
}

enum Codec {
    Symphonia {
        decoder: Box<dyn Decoder>,
        /// Created from the first decoded buffer, which says the rate.
        resampler: Option<Box<MonoResampler>>,
        /// Whether the source is already at 16 kHz.
        native: bool,
        sample_buf: Option<SampleBuffer<f32>>,
        mono: Vec<f32>,
    },
    Opus(OpusTrack),
}

/// An open file, read as 16 kHz mono blocks.
pub struct AudioFileDecoder {
    format: Box<dyn FormatReader>,
    track_id: u32,
    codec: Codec,
    /// Frames at the source rate, when the container says.
    total_frames: Option<u64>,
    source_rate: Option<u32>,
    /// Source frames decoded so far (for progress).
    decoded_frames: u64,
    /// Output samples handed out so far.
    produced: u64,
    pending: Vec<f32>,
    ended: bool,
    errors_in_a_row: usize,
    /// Packets decoded, corrupt ones excluded.
    good_packets: usize,
    path: std::path::PathBuf,
}

impl AudioFileDecoder {
    pub fn open(path: &Path) -> Result<Self, DecodeError> {
        let file = File::open(path).map_err(|e| io_error(path, e))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(&ext.to_ascii_lowercase());
        }
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions {
                    enable_gapless: true,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .map_err(|e| open_error(path, e))?;
        let format = probed.format;

        // The first audio track something here can decode. Video and
        // subtitle tracks have no codec symphonia knows (CODEC_TYPE_NULL) or
        // no sample rate.
        let audio_tracks: Vec<&Track> = format
            .tracks()
            .iter()
            .filter(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .filter(|t| t.codec_params.sample_rate.is_some() || t.codec_params.channels.is_some())
            .collect();
        if audio_tracks.is_empty() {
            return Err(DecodeError::NoAudioTrack);
        }
        let mut last_error = None;
        let mut chosen = None;
        for track in audio_tracks {
            let codec = if track.codec_params.codec == CODEC_TYPE_OPUS {
                OpusTrack::new(track).map(Codec::Opus)
            } else {
                symphonia::default::get_codecs()
                    .make(&track.codec_params, &DecoderOptions::default())
                    .map(|decoder| Codec::Symphonia {
                        decoder,
                        resampler: None,
                        native: false,
                        sample_buf: None,
                        mono: Vec::new(),
                    })
                    .map_err(|e| open_error(path, e))
            };
            match codec {
                Ok(codec) => {
                    chosen = Some((track.id, codec, track.codec_params.clone()));
                    break;
                }
                Err(e) => last_error = Some(e),
            }
        }
        let Some((track_id, codec, params)) = chosen else {
            return Err(last_error.unwrap_or(DecodeError::NoAudioTrack));
        };

        let source_rate = match &codec {
            Codec::Opus(_) => Some(48_000),
            Codec::Symphonia { .. } => params.sample_rate,
        };
        debug!(
            "Decoding {} (codec {:?}, {:?} Hz, {:?} frames)",
            path.display(),
            params.codec,
            source_rate,
            params.n_frames
        );
        Ok(Self {
            format,
            track_id,
            codec,
            total_frames: params.n_frames,
            source_rate,
            decoded_frames: 0,
            produced: 0,
            pending: Vec::new(),
            ended: false,
            errors_in_a_row: 0,
            good_packets: 0,
            path: path.to_path_buf(),
        })
    }

    /// The file's length, when its container says.
    pub fn duration_ms(&self) -> Option<u64> {
        let frames = self.total_frames?;
        let rate = u64::from(self.source_rate?);
        (rate > 0).then(|| frames * 1_000 / rate)
    }

    /// Share of the file decoded so far (0 to 1), when its length is known.
    pub fn progress(&self) -> Option<f64> {
        let total = self.total_frames.filter(|&t| t > 0)?;
        Some((self.decoded_frames as f64 / total as f64).min(1.0))
    }

    /// The next block of 16 kHz mono samples, `None` at the end of the file.
    pub fn next_block(&mut self) -> Result<Option<Vec<f32>>, DecodeError> {
        while self.pending.len() < BLOCK_SAMPLES && !self.ended {
            self.decode_packet()?;
        }
        if self.pending.is_empty() {
            return Ok(None);
        }
        let take = self.pending.len().min(BLOCK_SAMPLES);
        let rest = self.pending.split_off(take);
        let block = std::mem::replace(&mut self.pending, rest);
        self.produced += block.len() as u64;
        Ok(Some(block))
    }

    /// Reads and decodes one packet into `pending`, or marks the end.
    fn decode_packet(&mut self) -> Result<(), DecodeError> {
        let packet = match self.format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => {
                return self.end();
            }
            Err(SymphoniaError::IoError(e)) => return Err(io_error(&self.path, e)),
            Err(SymphoniaError::ResetRequired) => return self.end(),
            Err(e) => return self.stream_error(e.to_string()),
        };
        if packet.track_id() != self.track_id {
            return Ok(());
        }

        let result = match &mut self.codec {
            Codec::Opus(opus) => {
                let before = self.pending.len();
                let result = opus.decode(&packet.data, &mut self.pending);
                self.decoded_frames += ((self.pending.len() - before) * 48_000 / RATE) as u64
                    + packet.trim_start as u64;
                result
            }
            Codec::Symphonia {
                decoder,
                resampler,
                native,
                sample_buf,
                mono,
            } => match decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    let frames = decoded.frames();
                    self.decoded_frames += frames as u64;
                    if frames == 0 {
                        Ok(())
                    } else {
                        let buf = match sample_buf {
                            Some(buf)
                                if buf.capacity() >= decoded.capacity() * spec.channels.count() =>
                            {
                                buf
                            }
                            _ => sample_buf
                                .insert(SampleBuffer::new(decoded.capacity() as u64, spec)),
                        };
                        buf.copy_interleaved_ref(decoded);
                        mono.clear();
                        mix_down(buf.samples(), spec.channels.count(), mono);
                        if resampler.is_none() && !*native {
                            if spec.rate == WHISPER_SAMPLE_RATE {
                                *native = true;
                            } else {
                                *resampler =
                                    Some(Box::new(MonoResampler::new(spec.rate as usize)?));
                            }
                            self.source_rate.get_or_insert(spec.rate);
                        }
                        match resampler {
                            Some(r) => r.push(mono, &mut self.pending)?,
                            None => self.pending.extend_from_slice(mono),
                        }
                        Ok(())
                    }
                }
                Err(SymphoniaError::DecodeError(e)) => Err(e.to_string()),
                Err(SymphoniaError::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => {
                    return self.end();
                }
                Err(SymphoniaError::IoError(e)) => return Err(io_error(&self.path, e)),
                Err(e) => return self.stream_error(e.to_string()),
            },
        };
        match result {
            Ok(()) => {
                self.errors_in_a_row = 0;
                self.good_packets += 1;
                Ok(())
            }
            Err(e) => {
                // A damaged packet is skipped (it costs a few ms of audio);
                // a long run of them means the file is not what it claims.
                self.errors_in_a_row += 1;
                debug!("Skipping a corrupt packet: {e}");
                if self.errors_in_a_row >= MAX_CONSECUTIVE_ERRORS {
                    return self.stream_error(e);
                }
                Ok(())
            }
        }
    }

    /// An unrecoverable error mid-file: the audio decoded so far is kept,
    /// unless there is none.
    fn stream_error(&mut self, what: String) -> Result<(), DecodeError> {
        if self.good_packets == 0 {
            return Err(DecodeError::Corrupt(what));
        }
        warn!(
            "Stopped decoding {} after an error: {what}",
            self.path.display()
        );
        self.end()
    }

    fn end(&mut self) -> Result<(), DecodeError> {
        if self.ended {
            return Ok(());
        }
        self.ended = true;
        if let Codec::Symphonia {
            resampler: Some(r), ..
        } = &mut self.codec
        {
            r.finish(&mut self.pending)?;
        }
        if self.good_packets == 0 && self.pending.is_empty() {
            // A container with a track but no packet we could decode.
            return Err(DecodeError::Corrupt("no decodable audio".into()));
        }
        Ok(())
    }
}

/// What [`decode_to_wav`] reports and checks along the way.
pub trait DecodeHooks {
    /// Fails once the decode should stop.
    fn check_cancelled(&self) -> Result<(), DecodeError>;
    /// The share of the file done (0 to 1), when its length is known.
    fn progress(&mut self, done: Option<f64>);
}

/// Decodes `source` into `out` (16 kHz mono 16-bit WAV). Written to a
/// sibling `.part` file first, so `out` only ever holds a whole decode.
/// Returns the number of samples written.
pub fn decode_to_wav(
    source: &Path,
    out: &Path,
    hooks: &mut dyn DecodeHooks,
) -> Result<u64, DecodeError> {
    hooks.check_cancelled()?;
    let mut decoder = AudioFileDecoder::open(source)?;
    let part = out.with_extension("wav.part");
    let spec = WavSpec {
        channels: 1,
        sample_rate: WHISPER_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let result = (|| {
        let mut writer = WavWriter::create(&part, spec)?;
        let mut written = 0u64;
        loop {
            hooks.check_cancelled()?;
            let Some(block) = decoder.next_block()? else {
                break;
            };
            for sample in &block {
                writer.write_sample((sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
            }
            written += block.len() as u64;
            hooks.progress(decoder.progress());
        }
        writer.finalize()?;
        Ok(written)
    })();
    match result {
        Ok(written) => {
            std::fs::rename(&part, out).map_err(DecodeError::Io)?;
            hooks.progress(Some(1.0));
            Ok(written)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/audio")
            .join(name)
    }

    fn decode_all(path: &Path) -> Result<Vec<f32>, DecodeError> {
        let mut decoder = AudioFileDecoder::open(path)?;
        let mut all = Vec::new();
        while let Some(block) = decoder.next_block()? {
            assert!(block.len() <= BLOCK_SAMPLES);
            all.extend(block);
        }
        Ok(all)
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
    }

    /// Every fixture holds 2 s of a 440 Hz tone (see `tests/fixtures/audio`).
    fn assert_tone(name: &str) {
        let samples = decode_all(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let seconds = samples.len() as f64 / RATE as f64;
        // Lossy codecs pad or trim a few frames; MP3/AAC encoders add some.
        assert!(
            (1.9..=2.15).contains(&seconds),
            "{name}: {seconds:.3}s decoded"
        );
        let level = rms(&samples[RATE / 4..RATE]);
        // ffmpeg's sine is at 1/8 of full scale (RMS 0.088), 3 dB lower once
        // spread over two channels (RMS 0.06).
        assert!(level > 0.05, "{name}: the tone is missing (rms {level})");
        assert!(
            samples.iter().all(|s| s.is_finite() && s.abs() <= 1.01),
            "{name}: out of range"
        );
    }

    #[test]
    fn decodes_every_supported_format() {
        for name in [
            "tone-16k-mono.wav",
            "tone-11k-stereo-24bit.wav",
            "tone.mp3",
            "tone.m4a",
            "tone.flac",
            "tone.ogg",
            "tone.opus",
            "tone.webm",
            "tone.mp4",
            "tone.mov",
            "tone.mkv",
        ] {
            assert_tone(name);
        }
    }

    #[test]
    fn a_video_without_audio_has_no_audio_track() {
        assert!(matches!(
            AudioFileDecoder::open(&fixture("video-only.mp4")),
            Err(DecodeError::NoAudioTrack)
        ));
    }

    #[test]
    fn garbage_is_refused_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.mp3");
        std::fs::write(&path, b"definitely not an mp3 file, just some text").unwrap();
        assert!(matches!(
            decode_all(&path),
            Err(DecodeError::Unsupported(_) | DecodeError::Corrupt(_))
        ));
        let empty = dir.path().join("empty.wav");
        std::fs::write(&empty, b"").unwrap();
        assert!(decode_all(&empty).is_err());
    }

    #[test]
    fn a_truncated_file_keeps_what_it_has() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = std::fs::read(fixture("tone.mp3")).unwrap();
        let path = dir.path().join("truncated.mp3");
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        let samples = decode_all(&path).unwrap();
        let seconds = samples.len() as f64 / RATE as f64;
        assert!((0.7..1.3).contains(&seconds), "{seconds:.3}s");
    }

    #[test]
    fn a_missing_file_is_reported_as_such() {
        assert!(matches!(
            AudioFileDecoder::open(Path::new("/nonexistent/dir/é ü.mp3")),
            Err(DecodeError::Missing)
        ));
    }

    #[test]
    fn resampling_keeps_length_and_alignment() {
        for rate in [8_000usize, 22_050, 44_100, 48_000] {
            let mut r = MonoResampler::new(rate).unwrap();
            // 1 s of silence, then a click at exactly 1 s.
            let mut input = vec![0.0f32; rate * 2];
            input[rate] = 1.0;
            let mut out = Vec::new();
            for piece in input.chunks(777) {
                r.push(piece, &mut out).unwrap();
            }
            r.finish(&mut out).unwrap();
            assert!(
                (out.len() as i64 - 2 * RATE as i64).abs() <= 2,
                "{rate} Hz: {} samples",
                out.len()
            );
            let peak = out
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                .unwrap()
                .0;
            assert!(
                (peak as i64 - RATE as i64).abs() <= 2,
                "{rate} Hz: click at {peak}"
            );
        }
    }

    #[test]
    fn stereo_is_mixed_down() {
        let mut out = Vec::new();
        mix_down(&[1.0, 0.0, 0.5, 0.5, -1.0, 1.0], 2, &mut out);
        assert_eq!(out, vec![0.5, 0.5, 0.0]);
    }

    struct Hooks {
        cancel: bool,
        last: Option<f64>,
    }

    impl DecodeHooks for Hooks {
        fn check_cancelled(&self) -> Result<(), DecodeError> {
            if self.cancel {
                Err(DecodeError::Cancelled)
            } else {
                Ok(())
            }
        }
        fn progress(&mut self, done: Option<f64>) {
            self.last = done;
        }
    }

    #[test]
    fn decode_to_wav_writes_a_16k_mono_track() {
        let dir = tempfile::tempdir().unwrap();
        // Accents and spaces, as Windows users name their files.
        let out = dir.path().join("réunion été.wav");
        let mut hooks = Hooks {
            cancel: false,
            last: None,
        };
        let written = decode_to_wav(&fixture("tone.flac"), &out, &mut hooks).unwrap();
        assert_eq!(hooks.last, Some(1.0));
        let reader = hound::WavReader::open(&out).unwrap();
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(reader.spec().sample_rate, WHISPER_SAMPLE_RATE);
        assert_eq!(u64::from(reader.duration()), written);
        assert!(!out.with_extension("wav.part").exists());
    }

    #[test]
    fn a_cancelled_decode_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("audio.wav");
        let mut hooks = Hooks {
            cancel: true,
            last: None,
        };
        assert!(matches!(
            decode_to_wav(&fixture("tone.mp3"), &out, &mut hooks),
            Err(DecodeError::Cancelled)
        ));
        assert!(!out.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn extensions_are_matched_case_insensitively() {
        assert!(is_supported_extension(Path::new(
            "C:\\Users\\Zoë\\Mémo.M4A"
        )));
        assert!(is_supported_extension(Path::new("clip.webm")));
        assert!(!is_supported_extension(Path::new("notes.txt")));
        assert!(!is_supported_extension(Path::new("noextension")));
    }
}
