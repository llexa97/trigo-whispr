// Adapted from fly-on-the-wall (MIT) — https://github.com/swarnavspujari/fly-on-the-wall/blob/5f6f56d3ed86b835ab2f1598c8ff444ed5d1bd93/crates/fly-audio/src/mix.rs
//
//! Pure audio helpers for the meeting tracks: stereo→mono downmix, streaming
//! resampling to the 16 kHz track rate, RMS levels, and repair of a WAV file
//! whose header was never finalized.
//!
//! Everything here runs on the capture's writer thread, never inside a
//! real-time audio callback.
//!
//! Differences from fly: fly resampled whole files after the fact
//! (`resample_linear`); meeting tracks are written incrementally, so the
//! resampler here is *streaming* (its fractional position and filter state
//! carry over between blocks, so the output length never drifts from
//! `input_len * out_rate / in_rate`) and low-pass filters the input before
//! decimating to keep the aliasing out of the speech band.

use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Average interleaved frames down to mono, appending to `out`.
pub fn downmix_into(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    let channels = channels.max(1);
    if channels == 1 {
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

/// Root mean square of a block (0.0 for an empty block).
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

/// f32 in [-1, 1] → i16, clamped.
pub fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// One RBJ biquad section (Direct Form I).
#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    fn low_pass(sample_rate: f32, cutoff: f32, q: f32) -> Self {
        let w0 = std::f32::consts::TAU * cutoff / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b1 = (1.0 - cos) / a0;
        Self {
            b0: b1 / 2.0,
            b1,
            b2: b1 / 2.0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Streaming mono resampler: 4th-order Butterworth low-pass (when
/// downsampling) followed by linear interpolation. Fine for speech, cheap,
/// and — unlike a block resampler — it emits exactly one output sample per
/// `in_rate / out_rate` input samples over the whole stream, which is what
/// keeps the two meeting tracks aligned.
pub struct StreamResampler {
    step: f64,
    /// Position of the next output sample, in input samples, relative to
    /// `prev` (index 0); the current block starts at index 1.
    pos: f64,
    prev: f32,
    filters: Vec<Biquad>,
}

impl StreamResampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let in_rate = in_rate.max(1);
        let out_rate = out_rate.max(1);
        let filters = if in_rate > out_rate {
            // Cut at 90 % of the output Nyquist (7.2 kHz for 16 kHz).
            let cutoff = out_rate as f32 * 0.45;
            // Butterworth Q values for a 4th-order cascade.
            vec![
                Biquad::low_pass(in_rate as f32, cutoff, 0.541_196_1),
                Biquad::low_pass(in_rate as f32, cutoff, 1.306_563),
            ]
        } else {
            Vec::new()
        };
        Self {
            step: in_rate as f64 / out_rate as f64,
            pos: 1.0,
            prev: 0.0,
            filters,
        }
    }

    /// Resample one mono block, appending the output to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        let n = input.len();
        out.reserve((n as f64 / self.step) as usize + 1);
        let mut filtered = Vec::with_capacity(n);
        if self.filters.is_empty() {
            filtered.extend_from_slice(input);
        } else {
            for &x in input {
                let mut y = x;
                for f in &mut self.filters {
                    y = f.process(y);
                }
                filtered.push(y);
            }
        }
        // Virtual sequence: s[0] = prev, s[1..=n] = filtered.
        let at = |i: usize| -> f32 {
            if i == 0 {
                self.prev
            } else {
                filtered[i - 1]
            }
        };
        while self.pos <= n as f64 {
            let i = self.pos.floor() as usize;
            let frac = (self.pos - i as f64) as f32;
            if frac == 0.0 {
                out.push(at(i));
            } else if i < n {
                let a = at(i);
                out.push(a + (at(i + 1) - a) * frac);
            } else {
                // Needs the next block's first sample.
                break;
            }
            self.pos += self.step;
        }
        self.pos -= n as f64;
        self.prev = filtered[n - 1];
    }
}

/// Fix the RIFF and `data` chunk sizes of a WAV whose writer never ran
/// `finalize()` (crash, `kill -9`, power loss), so every sample that reached
/// the disk becomes readable again. The track writers `flush()` every few
/// seconds, which already rewrites the header, so at most the last flush
/// interval is "invisible" before repair — and repair recovers it too.
///
/// Returns the number of audio bytes in the repaired `data` chunk.
pub fn repair_wav(path: &Path) -> io::Result<u64> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let file_len = file.metadata()?.len();
    let invalid = |msg: &str| io::Error::new(io::ErrorKind::InvalidData, msg.to_string());

    let mut riff = [0u8; 12];
    file.read_exact(&mut riff)?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Err(invalid("not a RIFF/WAVE file"));
    }

    // Walk the chunks up to `data` (fmt, and whatever else precedes it).
    let mut offset: u64 = 12;
    let mut block_align: u64 = 1;
    loop {
        if offset + 8 > file_len {
            return Err(invalid("no data chunk"));
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut header = [0u8; 8];
        file.read_exact(&mut header)?;
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as u64;
        match &header[0..4] {
            b"fmt " => {
                let mut fmt = [0u8; 16];
                file.read_exact(&mut fmt)?;
                block_align = u16::from_le_bytes([fmt[12], fmt[13]]).max(1) as u64;
            }
            b"data" => break,
            _ => {}
        }
        // Chunks are word-aligned.
        offset += 8 + size + (size & 1);
    }

    let data_start = offset + 8;
    let available = file_len.saturating_sub(data_start);
    let data_len = (available - available % block_align).min(u32::MAX as u64 - data_start);
    let riff_len = data_start + data_len - 8;

    file.seek(SeekFrom::Start(4))?;
    file.write_all(&(riff_len as u32).to_le_bytes())?;
    file.seek(SeekFrom::Start(offset + 4))?;
    file.write_all(&(data_len as u32).to_le_bytes())?;
    // Drop a trailing partial frame so the file is self-consistent.
    file.set_len(data_start + data_len)?;
    file.sync_all()?;
    Ok(data_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, n: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 * freq * std::f32::consts::TAU / rate as f32).sin() * amp)
            .collect()
    }

    #[test]
    fn downmix_averages_channels() {
        let mut out = Vec::new();
        downmix_into(&[1.0, 0.0, 0.5, 0.5, -1.0, 1.0], 2, &mut out);
        assert_eq!(out, vec![0.5, 0.5, 0.0]);
    }

    #[test]
    fn downmix_mono_is_identity() {
        let mut out = vec![0.9];
        downmix_into(&[0.1, 0.2], 1, &mut out);
        assert_eq!(out, vec![0.9, 0.1, 0.2]);
    }

    #[test]
    fn resample_same_rate_is_identity() {
        let input = vec![0.1, 0.2, 0.3, -0.4];
        let mut r = StreamResampler::new(16_000, 16_000);
        let mut out = Vec::new();
        r.process(&input, &mut out);
        assert_eq!(out, input);
    }

    #[test]
    fn resample_48k_to_16k_length_is_exact_across_blocks() {
        let input = sine(440.0, 48_000, 48_000 * 3, 0.5);
        let mut r = StreamResampler::new(48_000, 16_000);
        let mut out = Vec::new();
        // Odd block sizes, like real device callbacks.
        for block in input.chunks(441) {
            r.process(block, &mut out);
        }
        assert!(
            (out.len() as i64 - 48_000).abs() <= 1,
            "got {} samples",
            out.len()
        );
    }

    #[test]
    fn resample_44k1_has_no_long_term_drift() {
        // 60 s at 44.1 kHz in 10 ms blocks must give 60 s at 16 kHz.
        let mut r = StreamResampler::new(44_100, 16_000);
        let block = vec![0.0f32; 441];
        let mut out = Vec::new();
        for _ in 0..6_000 {
            r.process(&block, &mut out);
        }
        assert!((out.len() as i64 - 960_000).abs() <= 1, "got {}", out.len());
    }

    #[test]
    fn resample_keeps_speech_band_and_rejects_aliases() {
        let n = 48_000;
        // 1 kHz must pass…
        let mut r = StreamResampler::new(48_000, 16_000);
        let mut low = Vec::new();
        r.process(&sine(1_000.0, 48_000, n, 0.5), &mut low);
        let low_rms = rms(&low[1_000..]);
        assert!(low_rms > 0.3, "1 kHz attenuated: rms {low_rms}");

        // …while 15 kHz (which would alias to 1 kHz) must be strongly cut.
        let mut r = StreamResampler::new(48_000, 16_000);
        let mut high = Vec::new();
        r.process(&sine(15_000.0, 48_000, n, 0.5), &mut high);
        let high_rms = rms(&high[1_000..]);
        assert!(high_rms < 0.02, "15 kHz aliased through: rms {high_rms}");
    }

    #[test]
    fn upsampling_does_not_filter() {
        let mut r = StreamResampler::new(8_000, 16_000);
        let mut out = Vec::new();
        r.process(&[0.0, 1.0, 0.0, 1.0], &mut out);
        // The midpoint after the last input waits for the next block.
        assert_eq!(out, vec![0.0, 0.5, 1.0, 0.5, 0.0, 0.5, 1.0]);
    }

    #[test]
    fn rms_of_full_scale_square_is_one() {
        assert_eq!(rms(&[]), 0.0);
        assert!((rms(&[1.0, -1.0, 1.0, -1.0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn to_i16_clamps() {
        assert_eq!(to_i16(2.0), i16::MAX);
        assert_eq!(to_i16(-2.0), -i16::MAX);
        assert_eq!(to_i16(0.0), 0);
    }

    #[test]
    fn repair_recovers_unfinalized_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crash.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        {
            let mut w = hound::WavWriter::create(&path, spec).unwrap();
            for i in 0..16_000 {
                w.write_sample((i % 100) as i16).unwrap();
            }
            w.flush().unwrap();
            // Simulate a crash: skip finalize().
            std::mem::forget(w);
        }
        // Simulate a crash before the last header update: zero both sizes.
        {
            let mut f = OpenOptions::new().write(true).open(&path).unwrap();
            f.seek(SeekFrom::Start(4)).unwrap();
            f.write_all(&0u32.to_le_bytes()).unwrap();
            f.seek(SeekFrom::Start(40)).unwrap();
            f.write_all(&0u32.to_le_bytes()).unwrap();
            // And a torn trailing byte.
            f.seek(SeekFrom::End(0)).unwrap();
            f.write_all(&[7u8]).unwrap();
        }

        let data_len = repair_wav(&path).unwrap();
        assert_eq!(data_len, 32_000);
        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.duration(), 16_000);
    }

    #[test]
    fn repair_rejects_non_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("junk.wav");
        std::fs::write(&path, b"definitely not a wave file").unwrap();
        assert!(repair_wav(&path).is_err());
    }
}
