//! The bridge between a real-time audio callback and the capture thread.
//!
//! The callback side ([`SourceTap`]) only copies samples into a lock-free
//! ring buffer — no allocation, no lock, no I/O, no DSP. Downmixing,
//! resampling and writing happen on the capture thread, which drains the
//! other side ([`SourceFeed`]).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Seconds of audio the ring buffer absorbs if the capture thread stalls.
const RING_SECONDS: usize = 2;

/// Flags the callback side raises for the capture thread.
#[derive(Default)]
pub struct SourceStats {
    /// The stream reported an error (device unplugged, invalidated…).
    pub failed: AtomicBool,
    /// Set on every delivered packet, cleared by the capture thread.
    pub got_data: AtomicBool,
    /// Samples lost because the ring buffer was full.
    pub dropped: AtomicU64,
    /// Buffer under/overruns the stream reported. Non-fatal glitches: the
    /// stream keeps running, so they are only counted.
    pub xruns: AtomicU64,
}

/// Real-time side: owned by the stream callback.
pub struct SourceTap {
    producer: rtrb::Producer<f32>,
    channels: usize,
    stats: Arc<SourceStats>,
}

impl SourceTap {
    /// Append `len` interleaved samples. Only whole frames are written; what
    /// does not fit is counted as dropped.
    pub fn push<I: IntoIterator<Item = f32>>(&mut self, len: usize, samples: I) {
        let room = self.producer.slots().min(len);
        let n = room - room % self.channels;
        if n > 0 {
            if let Ok(chunk) = self.producer.write_chunk_uninit(n) {
                chunk.fill_from_iter(samples.into_iter().take(n));
            }
        }
        if n < len {
            self.stats
                .dropped
                .fetch_add((len - n) as u64, Ordering::Relaxed);
        }
        self.stats.got_data.store(true, Ordering::Relaxed);
    }

    /// Append `len` samples of silence (a packet flagged silent by WASAPI).
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn push_silence(&mut self, len: usize) {
        self.push(len, std::iter::repeat_n(0.0, len));
    }

    pub fn stats(&self) -> &Arc<SourceStats> {
        &self.stats
    }
}

/// Capture-thread side.
pub struct SourceFeed {
    consumer: rtrb::Consumer<f32>,
    pub channels: u16,
    pub sample_rate: u32,
    pub stats: Arc<SourceStats>,
}

impl SourceFeed {
    /// Move everything buffered so far into `out`.
    pub fn drain_into(&mut self, out: &mut Vec<f32>) {
        let n = self.consumer.slots();
        if n == 0 {
            return;
        }
        if let Ok(chunk) = self.consumer.read_chunk(n) {
            let (a, b) = chunk.as_slices();
            out.extend_from_slice(a);
            out.extend_from_slice(b);
            chunk.commit_all();
        }
    }
}

/// A connected tap/feed pair for a stream of `channels` × `sample_rate`.
pub fn channel(channels: u16, sample_rate: u32) -> (SourceTap, SourceFeed) {
    let channels = channels.max(1);
    let capacity = sample_rate.max(8_000) as usize * channels as usize * RING_SECONDS;
    let (producer, consumer) = rtrb::RingBuffer::new(capacity);
    let stats = Arc::new(SourceStats::default());
    (
        SourceTap {
            producer,
            channels: channels as usize,
            stats: stats.clone(),
        },
        SourceFeed {
            consumer,
            channels,
            sample_rate,
            stats,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_to_feed_roundtrip() {
        let (mut tap, mut feed) = channel(2, 8_000);
        tap.push(4, [0.1, 0.2, 0.3, 0.4]);
        let mut out = Vec::new();
        feed.drain_into(&mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3, 0.4]);
        assert!(feed.stats.got_data.load(Ordering::Relaxed));
    }

    #[test]
    fn overflow_drops_whole_frames_only() {
        let (mut tap, mut feed) = channel(2, 8_000);
        let capacity = 8_000 * 2 * RING_SECONDS;
        tap.push(capacity - 1, std::iter::repeat_n(0.5, capacity - 1));
        tap.push(4, [1.0; 4]);
        let mut out = Vec::new();
        feed.drain_into(&mut out);
        // First push: one odd sample left over; second: room for one frame.
        assert_eq!(out.len(), capacity);
        assert_eq!(feed.stats.dropped.load(Ordering::Relaxed), 1 + 2);
    }
}
