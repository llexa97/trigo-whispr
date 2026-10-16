//! One meeting track on disk: raw interleaved samples in, 16 kHz mono i16
//! WAV out, written incrementally.
//!
//! A track's timeline is wall-clock time since the capture started. Sources
//! that go quiet without delivering anything (WASAPI loopback while nothing
//! plays, a device being reopened) are topped up with silence via
//! [`TrackWriter::pad_to`], so both tracks stay aligned sample for sample.

use std::fs::File;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};

use super::mix::{downmix_into, rms, to_i16, StreamResampler};
use super::TRACK_SAMPLE_RATE;

/// Below this (≈ −80 dBFS) a sample counts as digital silence.
const SIGNAL_FLOOR: f32 = 1e-4;

type Wav = hound::WavWriter<BufWriter<File>>;

pub struct TrackWriter {
    writer: Option<Wav>,
    path: PathBuf,
    /// 16 kHz frames written so far (real audio + padding).
    frames: u64,
    in_channels: usize,
    resampler: StreamResampler,
    mono: Vec<f32>,
    resampled: Vec<f32>,
    last_rms: f32,
    heard_signal: bool,
}

fn hound_to_io(e: hound::Error) -> io::Error {
    match e {
        hound::Error::IoError(e) => e,
        other => io::Error::other(other.to_string()),
    }
}

impl TrackWriter {
    /// Create (truncate) `path` as a 16 kHz mono 16-bit WAV.
    pub fn create(path: &Path) -> io::Result<Self> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: TRACK_SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(path, spec).map_err(hound_to_io)?;
        Ok(Self {
            writer: Some(writer),
            path: path.to_path_buf(),
            frames: 0,
            in_channels: 1,
            resampler: StreamResampler::new(TRACK_SAMPLE_RATE, TRACK_SAMPLE_RATE),
            mono: Vec::new(),
            resampled: Vec::new(),
            last_rms: 0.0,
            heard_signal: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Declare the format of the samples that follow. Called when a source is
    /// (re)opened; resets the resampler, since the old stream is gone.
    pub fn set_input_format(&mut self, channels: u16, sample_rate: u32) {
        self.in_channels = channels.max(1) as usize;
        self.resampler = StreamResampler::new(sample_rate, TRACK_SAMPLE_RATE);
    }

    /// Append interleaved samples in the current input format.
    pub fn push_interleaved(&mut self, data: &[f32]) -> io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.mono.clear();
        downmix_into(data, self.in_channels, &mut self.mono);
        self.resampled.clear();
        self.resampler.process(&self.mono, &mut self.resampled);
        self.last_rms = rms(&self.resampled);
        if !self.heard_signal {
            self.heard_signal = self.resampled.iter().any(|s| s.abs() > SIGNAL_FLOOR);
        }
        let Some(writer) = self.writer.as_mut() else {
            return Ok(());
        };
        let mut i16_writer = writer.get_i16_writer(self.resampled.len() as u32);
        for &s in &self.resampled {
            i16_writer.write_sample(to_i16(s));
        }
        i16_writer.flush().map_err(hound_to_io)?;
        self.frames += self.resampled.len() as u64;
        Ok(())
    }

    /// Append silence until the track holds `target_frames` frames.
    pub fn pad_to(&mut self, target_frames: u64) -> io::Result<()> {
        if target_frames <= self.frames {
            return Ok(());
        }
        let missing = target_frames - self.frames;
        self.last_rms = 0.0;
        let Some(writer) = self.writer.as_mut() else {
            return Ok(());
        };
        // Bounded chunks: a long gap must not allocate one huge buffer.
        let mut left = missing;
        while left > 0 {
            let n = left.min(TRACK_SAMPLE_RATE as u64) as u32;
            let mut i16_writer = writer.get_i16_writer(n);
            for _ in 0..n {
                i16_writer.write_sample(0);
            }
            i16_writer.flush().map_err(hound_to_io)?;
            left -= n as u64;
        }
        self.frames = target_frames;
        Ok(())
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// RMS of the most recent block written (0 after padding).
    pub fn level(&self) -> f32 {
        self.last_rms
    }

    /// Whether any non-silent sample was ever written.
    pub fn heard_signal(&self) -> bool {
        self.heard_signal
    }

    /// Push buffered samples to disk and rewrite the header sizes, so a crash
    /// after this point leaves a WAV readable up to here.
    pub fn flush(&mut self) -> io::Result<()> {
        match self.writer.as_mut() {
            Some(w) => w.flush().map_err(hound_to_io),
            None => Ok(()),
        }
    }

    pub fn finalize(mut self) -> io::Result<()> {
        match self.writer.take() {
            Some(w) => w.finalize().map_err(hound_to_io),
            None => Ok(()),
        }
    }
}

/// 16 kHz frames covering `elapsed_ms` of wall-clock time.
pub fn frames_for_ms(elapsed_ms: u64) -> u64 {
    elapsed_ms * TRACK_SAMPLE_RATE as u64 / 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn duration_ms(path: &Path) -> u64 {
        let reader = hound::WavReader::open(path).unwrap();
        assert_eq!(reader.spec().sample_rate, TRACK_SAMPLE_RATE);
        assert_eq!(reader.spec().channels, 1);
        reader.duration() as u64 * 1000 / TRACK_SAMPLE_RATE as u64
    }

    #[test]
    fn stereo_48k_is_written_as_mono_16k() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("system.wav");
        let mut t = TrackWriter::create(&path).unwrap();
        t.set_input_format(2, 48_000);
        // 1 s of stereo in 10 ms callbacks.
        let block = vec![0.25f32; 480 * 2];
        for _ in 0..100 {
            t.push_interleaved(&block).unwrap();
        }
        assert!((t.frames() as i64 - 16_000).abs() <= 1);
        assert!(t.heard_signal());
        t.finalize().unwrap();
        assert!((duration_ms(&path) as i64 - 1000).abs() <= 1);
    }

    #[test]
    fn pad_to_fills_with_silence_and_never_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mic.wav");
        let mut t = TrackWriter::create(&path).unwrap();
        t.pad_to(frames_for_ms(2_500)).unwrap();
        assert_eq!(t.frames(), 40_000);
        t.pad_to(10).unwrap();
        assert_eq!(t.frames(), 40_000);
        assert!(!t.heard_signal());
        t.finalize().unwrap();
        let mut reader = hound::WavReader::open(&path).unwrap();
        assert!(reader.samples::<i16>().all(|s| s.unwrap() == 0));
    }

    /// One track receives audio for N seconds, the other receives nothing
    /// (WASAPI loopback while nothing plays): after the end-of-capture
    /// padding both files must have the same duration (±50 ms).
    #[test]
    fn idle_track_is_aligned_with_active_track() {
        let dir = tempfile::tempdir().unwrap();
        let mic_path = dir.path().join("mic.wav");
        let sys_path = dir.path().join("system.wav");
        let mut mic = TrackWriter::create(&mic_path).unwrap();
        mic.set_input_format(1, 44_100);
        let mut sys = TrackWriter::create(&sys_path).unwrap();
        sys.set_input_format(2, 48_000);

        let seconds = 5;
        let block = vec![0.1f32; 441];
        for _ in 0..(seconds * 100) {
            mic.push_interleaved(&block).unwrap();
        }
        // Stop: every track is padded to the longest one / the clock.
        let target = mic
            .frames()
            .max(sys.frames())
            .max(frames_for_ms(seconds * 1000));
        mic.pad_to(target).unwrap();
        sys.pad_to(target).unwrap();
        mic.finalize().unwrap();
        sys.finalize().unwrap();

        let mic_ms = duration_ms(&mic_path);
        let sys_ms = duration_ms(&sys_path);
        assert!(
            (mic_ms as i64 - sys_ms as i64).abs() <= 50,
            "{mic_ms} vs {sys_ms}"
        );
        assert!((mic_ms as i64 - 5_000).abs() <= 50, "mic {mic_ms} ms");
    }

    /// A source that stops mid-capture (device unplugged, then reopened at a
    /// different rate) keeps its timeline thanks to the gap padding.
    #[test]
    fn gap_while_reopening_keeps_timeline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mic.wav");
        let mut t = TrackWriter::create(&path).unwrap();
        t.set_input_format(1, 48_000);
        t.push_interleaved(&vec![0.2; 48_000]).unwrap(); // 0–1 s
        t.pad_to(frames_for_ms(3_000)).unwrap(); // 1–3 s: device gone
        t.set_input_format(2, 44_100);
        t.push_interleaved(&vec![0.2; 44_100 * 2]).unwrap(); // 3–4 s
        assert!((t.frames() as i64 - 64_000).abs() <= 2, "{}", t.frames());
        t.finalize().unwrap();
        assert!((duration_ms(&path) as i64 - 4_000).abs() <= 1);
    }

    #[test]
    fn flushed_track_survives_without_finalize() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mic.wav");
        let mut t = TrackWriter::create(&path).unwrap();
        t.push_interleaved(&vec![0.3; 16_000]).unwrap();
        t.flush().unwrap();
        // Simulate kill -9: the writer is never finalized nor dropped.
        std::mem::forget(t);
        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.duration(), 16_000);
    }
}
