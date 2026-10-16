//! Repair of WAV files left unfinalized by a crash.
//!
//! The meeting capture writes tracks incrementally; if the process dies
//! between two header updates, the RIFF and `data` chunk sizes are stale (or
//! zero) while the samples on disk are intact. Since the `data` chunk is the
//! last one written, its real size is simply "file length − data offset".

use anyhow::{anyhow, bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavInfo {
    pub data_bytes: u64,
    pub duration_ms: u64,
    /// True when the header had to be rewritten.
    pub repaired: bool,
}

struct Layout {
    sample_rate: u32,
    block_align: u16,
    /// Offset of the `data` chunk payload.
    data_offset: u64,
    /// Size currently declared in the `data` chunk header.
    declared_data: u32,
    declared_riff: u32,
}

fn read_u32(buf: &[u8]) -> u32 {
    u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])
}

fn read_u16(buf: &[u8]) -> u16 {
    u16::from_le_bytes([buf[0], buf[1]])
}

fn parse_layout(file: &mut File, file_len: u64) -> Result<Layout> {
    let mut riff = [0u8; 12];
    file.read_exact(&mut riff)
        .context("file too short for a WAV header")?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        bail!("not a RIFF/WAVE file");
    }
    let declared_riff = read_u32(&riff[4..8]);

    let mut fmt: Option<(u16, u32, u16)> = None;
    let mut offset = 12u64;
    loop {
        if offset + 8 > file_len {
            bail!("no data chunk found");
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut header = [0u8; 8];
        file.read_exact(&mut header)?;
        let id = &header[0..4];
        let size = read_u32(&header[4..8]);
        let payload = offset + 8;

        if id == b"fmt " {
            let mut body = [0u8; 16];
            file.read_exact(&mut body).context("truncated fmt chunk")?;
            fmt = Some((
                read_u16(&body[2..4]),
                read_u32(&body[4..8]),
                read_u16(&body[12..14]),
            ));
        } else if id == b"data" {
            let (channels, sample_rate, block_align) =
                fmt.ok_or_else(|| anyhow!("data chunk before fmt chunk"))?;
            if channels == 0 || sample_rate == 0 || block_align == 0 {
                bail!("invalid fmt chunk");
            }
            return Ok(Layout {
                sample_rate,
                block_align,
                data_offset: payload,
                declared_data: size,
                declared_riff,
            });
        }
        // Chunks are word-aligned.
        offset = payload + size as u64 + (size as u64 & 1);
    }
}

fn duration_ms(data_bytes: u64, sample_rate: u32, block_align: u16) -> u64 {
    let frames = data_bytes / block_align as u64;
    frames * 1000 / sample_rate as u64
}

/// Rewrites the RIFF and `data` sizes from the file length. A trailing
/// partial frame is truncated. Leaves a consistent file untouched.
pub fn repair(path: &Path) -> Result<WavInfo> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let file_len = file.metadata()?.len();
    let layout = parse_layout(&mut file, file_len)?;

    let available = file_len.saturating_sub(layout.data_offset);
    let max_data = (u32::MAX as u64).saturating_sub(layout.data_offset);
    let mut data_bytes = available.min(max_data);
    data_bytes -= data_bytes % layout.block_align as u64;
    let new_len = layout.data_offset + data_bytes;
    let riff_size = (new_len - 8) as u32;

    let repaired = layout.declared_data as u64 != data_bytes
        || layout.declared_riff != riff_size
        || new_len != file_len;
    if repaired {
        file.set_len(new_len)?;
        file.seek(SeekFrom::Start(4))?;
        file.write_all(&riff_size.to_le_bytes())?;
        file.seek(SeekFrom::Start(layout.data_offset - 4))?;
        file.write_all(&(data_bytes as u32).to_le_bytes())?;
        file.sync_all()?;
    }

    Ok(WavInfo {
        data_bytes,
        duration_ms: duration_ms(data_bytes, layout.sample_rate, layout.block_align),
        repaired,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, samples: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..samples {
            writer.write_sample((i % 100) as i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    /// Simulates a crash: header sizes never updated after creation.
    fn zero_sizes(path: &Path) {
        let mut file = OpenOptions::new()
            .write(true)
            .read(true)
            .open(path)
            .unwrap();
        let len = file.metadata().unwrap().len();
        let layout = parse_layout(&mut file, len).unwrap();
        file.seek(SeekFrom::Start(4)).unwrap();
        file.write_all(&0u32.to_le_bytes()).unwrap();
        file.seek(SeekFrom::Start(layout.data_offset - 4)).unwrap();
        file.write_all(&0u32.to_le_bytes()).unwrap();
    }

    #[test]
    fn repairs_zeroed_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mic.wav");
        write_wav(&path, 32_000);
        zero_sizes(&path);

        let info = repair(&path).unwrap();
        assert!(info.repaired);
        assert_eq!(info.duration_ms, 2_000);

        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.len(), 32_000);
    }

    #[test]
    fn truncates_partial_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mic.wav");
        write_wav(&path, 1_600);
        zero_sizes(&path);
        // Half a sample left over by the crash.
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&[0x7f]).unwrap();
        drop(file);

        let info = repair(&path).unwrap();
        assert_eq!(info.data_bytes, 3_200);
        assert_eq!(info.duration_ms, 100);
        assert_eq!(hound::WavReader::open(&path).unwrap().len(), 1_600);
    }

    #[test]
    fn leaves_valid_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mic.wav");
        write_wav(&path, 16_000);
        let info = repair(&path).unwrap();
        assert!(!info.repaired);
        assert_eq!(info.duration_ms, 1_000);
    }

    #[test]
    fn rejects_non_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.wav");
        std::fs::write(&path, b"definitely not a wav file").unwrap();
        assert!(repair(&path).is_err());
    }
}
