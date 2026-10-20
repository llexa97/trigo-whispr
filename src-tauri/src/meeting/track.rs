//! Decoding one meeting track: its WAV read block by block (a 2 h track is
//! ~460 MB as f32, too much to hold), cut at pauses by the long-form
//! [`Chunker`], silent chunks skipped, and each chunk's transcript put back on
//! the track's timeline.
//!
//! The decoder is passed in, so this runs the same against the engine and in
//! tests.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{bail, Context, Result};
use hound::{SampleFormat, WavReader};
use log::{debug, warn};
use transcribe_cpp::Transcript;

use super::transcript::TrackTranscript;
use crate::audio_toolkit::chunking::Chunker;
use crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE;
use crate::audio_toolkit::SpeechScorer;

const RATE: usize = WHISPER_SAMPLE_RATE as usize;
/// Samples read from the WAV at a time.
const READ_BLOCK_SAMPLES: usize = 10 * RATE;
/// A frame at or above this probability is speech (Silero's threshold).
const SPEECH_THRESHOLD: f32 = 0.5;
/// A chunk with less speech than this is not decoded: on the mic track,
/// whole chunks are silence while the user listens, and decoding silence
/// only invites hallucinations.
const MIN_SPEECH_SAMPLES: usize = RATE / 4;

/// Samples in a track, from its header.
pub fn track_samples(path: &Path) -> Result<u64> {
    let reader = open(path)?;
    Ok(u64::from(reader.duration()))
}

/// A whole track, as f32 in [-1, 1]. For the diarizer, which takes the
/// recording in one run; it runs in the worker process, so the app itself
/// never holds a long track.
pub fn read_track(path: &Path) -> Result<Vec<f32>> {
    let mut reader = open(path)?;
    let samples = reader.duration() as usize;
    read_block(&mut reader, samples)
}

fn open(path: &Path) -> Result<WavReader<BufReader<File>>> {
    let reader =
        WavReader::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != WHISPER_SAMPLE_RATE {
        bail!(
            "{} is {} Hz with {} channel(s); expected 16 kHz mono",
            path.display(),
            spec.sample_rate,
            spec.channels
        );
    }
    Ok(reader)
}

/// Up to `max` samples, as f32 in [-1, 1]. Empty at the end of the track.
fn read_block<R: Read>(reader: &mut WavReader<R>, max: usize) -> Result<Vec<f32>> {
    let spec = reader.spec();
    let mut block = Vec::with_capacity(max);
    match spec.sample_format {
        SampleFormat::Float => {
            for sample in reader.samples::<f32>().take(max) {
                block.push(sample?);
            }
        }
        SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            for sample in reader.samples::<i32>().take(max) {
                block.push(sample? as f32 * scale);
            }
        }
    }
    Ok(block)
}

/// Tells chunks with speech from silent ones, scoring the track in order.
pub struct SpeechGate {
    /// `None` once scoring failed, or if it never could: then every chunk is
    /// decoded, as it would be without the gate.
    scorer: Option<Box<dyn SpeechScorer>>,
    /// Samples short of a whole frame, scored with the next chunk.
    carry: Vec<f32>,
}

impl SpeechGate {
    pub fn new(scorer: Result<Box<dyn SpeechScorer>>) -> Self {
        let scorer = scorer
            .inspect_err(|e| warn!("No VAD for meeting transcription; decoding every chunk: {e:#}"))
            .ok();
        Self {
            scorer,
            carry: Vec::new(),
        }
    }

    /// Whether `chunk`, the next of the track, holds any speech.
    pub fn has_speech(&mut self, chunk: &[f32]) -> bool {
        let Some(scorer) = self.scorer.as_mut() else {
            return true;
        };
        let frame = scorer.frame_samples();
        let mut audio = std::mem::take(&mut self.carry);
        audio.extend_from_slice(chunk);
        let whole = audio.len() / frame * frame;
        self.carry = audio.split_off(whole);
        match scorer.score(&audio) {
            Ok(probs) => {
                let speech = probs.iter().filter(|&&p| p >= SPEECH_THRESHOLD).count() * frame;
                speech >= MIN_SPEECH_SAMPLES
            }
            Err(e) => {
                warn!("VAD failed during meeting transcription; decoding every chunk: {e:#}");
                self.scorer = None;
                true
            }
        }
    }
}

/// What a track decode calls around each chunk.
pub trait TrackHooks {
    /// Fails once the decode should stop (a cancel). Checked for every block
    /// read and every chunk, silent ones included.
    fn check_cancelled(&self) -> Result<()>;
    /// Before decoding a chunk: wait for the engine to be free, or fail to
    /// stop (a cancel).
    fn before_chunk(&mut self) -> Result<()>;
    /// Decodes one chunk.
    fn decode(&mut self, pcm: Vec<f32>) -> Result<Transcript>;
    /// After a chunk of `samples` is done, decoded or skipped as silent.
    fn after_chunk(&mut self, samples: usize);
}

/// Decodes the track at `path`, chunk by chunk.
pub fn decode_track(
    path: &Path,
    mut chunker: Chunker,
    mut gate: SpeechGate,
    hooks: &mut dyn TrackHooks,
) -> Result<TrackTranscript> {
    let mut reader = open(path)?;
    let mut track = TrackTranscript::default();
    let mut offset = 0usize;
    loop {
        hooks.check_cancelled()?;
        let block = read_block(&mut reader, READ_BLOCK_SAMPLES)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let end = block.is_empty();
        let chunks = if end {
            chunker.finish()
        } else {
            chunker.push(block)
        };
        for chunk in chunks {
            hooks.check_cancelled()?;
            let len = chunk.len();
            if gate.has_speech(&chunk) {
                hooks.before_chunk()?;
                let transcript = hooks.decode(chunk)?;
                track.push_chunk(&transcript, samples_to_ms(offset), samples_to_ms(len));
            } else {
                debug!(
                    "Skipping silent meeting chunk at {:.1}s",
                    offset as f64 / RATE as f64
                );
            }
            offset += len;
            hooks.after_chunk(len);
        }
        if end {
            return Ok(track);
        }
    }
}

fn samples_to_ms(samples: usize) -> u64 {
    samples as u64 * 1_000 / RATE as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_toolkit::vad::{VadFrame, VoiceActivityDetector};
    use transcribe_cpp::{Segment, Word};

    const FRAME: usize = 512;

    /// Speech wherever the signal is loud.
    struct LoudnessScorer;

    impl VoiceActivityDetector for LoudnessScorer {
        fn push_frame<'a>(&'a mut self, _frame: &'a [f32]) -> Result<VadFrame<'a>> {
            unreachable!("meeting decoding only scores")
        }

        fn frame_samples(&self) -> usize {
            FRAME
        }
    }

    impl SpeechScorer for LoudnessScorer {
        fn score(&mut self, frames: &[f32]) -> Result<Vec<f32>> {
            Ok(frames
                .chunks(FRAME)
                .map(|f| {
                    if f.iter().any(|s| s.abs() > 0.1) {
                        1.0
                    } else {
                        0.0
                    }
                })
                .collect())
        }
    }

    fn scorer() -> Result<Box<dyn SpeechScorer>> {
        Ok(Box::new(LoudnessScorer))
    }

    fn chunker() -> Chunker {
        Chunker::new(Box::new(scorer))
    }

    /// `(seconds, loud)` spans, written as a 16-bit WAV.
    fn write_track(dir: &Path, spans: &[(usize, bool)]) -> std::path::PathBuf {
        let path = dir.join("track.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: WHISPER_SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for &(seconds, loud) in spans {
            for i in 0..seconds * RATE {
                let sample = if loud {
                    if i % 2 == 0 {
                        16_000
                    } else {
                        -16_000
                    }
                } else {
                    0
                };
                writer.write_sample(sample as i16).unwrap();
            }
        }
        writer.finalize().unwrap();
        path
    }

    /// Answers each chunk with one word and one segment spanning it, and
    /// records what it was asked.
    #[derive(Default)]
    struct FakeDecoder {
        decoded: Vec<usize>,
        progressed: usize,
        waits: usize,
        fail_before: Option<usize>,
        /// Cancelled once this many chunks were decoded.
        cancel_after: Option<usize>,
    }

    impl TrackHooks for FakeDecoder {
        fn check_cancelled(&self) -> Result<()> {
            if self.cancel_after.is_some_and(|n| self.decoded.len() >= n) {
                bail!("cancelled");
            }
            Ok(())
        }

        fn before_chunk(&mut self) -> Result<()> {
            if self.fail_before == Some(self.waits) {
                bail!("cancelled");
            }
            self.waits += 1;
            Ok(())
        }

        fn decode(&mut self, pcm: Vec<f32>) -> Result<Transcript> {
            let ms = samples_to_ms(pcm.len()) as i64;
            let text = format!("chunk{}", self.decoded.len());
            self.decoded.push(pcm.len());
            Ok(Transcript {
                text: text.clone(),
                segments: vec![Segment {
                    text: text.clone(),
                    t0_ms: 0,
                    t1_ms: ms,
                    ..Default::default()
                }],
                words: vec![Word {
                    text,
                    t0_ms: 10,
                    t1_ms: ms,
                    ..Default::default()
                }],
                ..Default::default()
            })
        }

        fn after_chunk(&mut self, samples: usize) {
            self.progressed += samples;
        }
    }

    #[test]
    fn a_short_track_is_one_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_track(dir.path(), &[(12, true)]);
        let mut hooks = FakeDecoder::default();
        let track = decode_track(&path, chunker(), SpeechGate::new(scorer()), &mut hooks).unwrap();
        assert_eq!(hooks.decoded, vec![12 * RATE]);
        assert_eq!(hooks.progressed, 12 * RATE);
        assert_eq!(track.words[0].start_ms, 10);
    }

    #[test]
    fn chunks_keep_their_place_on_the_track() {
        let dir = tempfile::tempdir().unwrap();
        // 35 s speech, 2 s pause, 35 s speech: cut in the pause.
        let path = write_track(dir.path(), &[(35, true), (2, false), (35, true)]);
        let mut hooks = FakeDecoder::default();
        let track = decode_track(&path, chunker(), SpeechGate::new(scorer()), &mut hooks).unwrap();
        assert_eq!(hooks.decoded.len(), 2);
        assert_eq!(hooks.decoded.iter().sum::<usize>(), 72 * RATE);
        assert_eq!(hooks.progressed, 72 * RATE);
        let first = samples_to_ms(hooks.decoded[0]);
        assert!(
            (35_000..=37_000).contains(&first),
            "cut in the pause: {first}"
        );
        assert_eq!(track.words[1].start_ms, first + 10);
        assert_eq!(track.segments[1].start_ms, first);
        assert_eq!(track.segments[1].end_ms, 72_000);
    }

    #[test]
    fn silent_chunks_are_skipped_but_counted() {
        let dir = tempfile::tempdir().unwrap();
        // 35 s speech, then a minute of silence the user spent listening.
        let path = write_track(dir.path(), &[(35, true), (1, false), (60, false)]);
        let mut hooks = FakeDecoder::default();
        let track = decode_track(&path, chunker(), SpeechGate::new(scorer()), &mut hooks).unwrap();
        assert_eq!(hooks.decoded.len(), 1);
        assert_eq!(hooks.waits, 1);
        assert_eq!(hooks.progressed, 96 * RATE);
        assert_eq!(track.words.len(), 1);
    }

    #[test]
    fn without_a_vad_every_chunk_is_decoded() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_track(dir.path(), &[(10, false)]);
        let mut hooks = FakeDecoder::default();
        let gate = SpeechGate::new(Err(anyhow::anyhow!("no VAD")));
        decode_track(&path, chunker(), gate, &mut hooks).unwrap();
        assert_eq!(hooks.decoded.len(), 1);
    }

    #[test]
    fn a_hook_failure_stops_the_track() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_track(dir.path(), &[(35, true), (2, false), (35, true)]);
        let mut hooks = FakeDecoder {
            fail_before: Some(1),
            ..Default::default()
        };
        let result = decode_track(&path, chunker(), SpeechGate::new(scorer()), &mut hooks);
        assert!(result.is_err());
        assert_eq!(hooks.decoded.len(), 1);
    }

    #[test]
    fn a_cancel_stops_a_silent_track() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_track(dir.path(), &[(90, false)]);
        let mut hooks = FakeDecoder {
            cancel_after: Some(0),
            ..Default::default()
        };
        let result = decode_track(&path, chunker(), SpeechGate::new(scorer()), &mut hooks);
        assert!(result.is_err());
        assert_eq!(hooks.progressed, 0);
    }

    #[test]
    fn a_cancel_after_the_last_spoken_chunk_stops_the_silent_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_track(dir.path(), &[(35, true), (1, false), (60, false)]);
        let mut hooks = FakeDecoder {
            cancel_after: Some(1),
            ..Default::default()
        };
        let result = decode_track(&path, chunker(), SpeechGate::new(scorer()), &mut hooks);
        assert!(result.is_err());
        assert_eq!(hooks.decoded.len(), 1);
        assert!(hooks.progressed < 96 * RATE);
    }

    #[test]
    fn stereo_or_resampled_tracks_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stereo.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        hound::WavWriter::create(&path, spec)
            .unwrap()
            .finalize()
            .unwrap();
        assert!(track_samples(&path).is_err());
    }

    #[test]
    fn float_tracks_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("float.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: WHISPER_SAMPLE_RATE,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..RATE {
            writer
                .write_sample(if i % 2 == 0 { 0.5f32 } else { -0.5 })
                .unwrap();
        }
        writer.finalize().unwrap();
        assert_eq!(track_samples(&path).unwrap(), RATE as u64);
        let mut reader = open(&path).unwrap();
        let block = read_block(&mut reader, 4).unwrap();
        assert_eq!(block, vec![0.5, -0.5, 0.5, -0.5]);
    }
}
