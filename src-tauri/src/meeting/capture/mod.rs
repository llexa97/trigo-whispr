//! Dual-track meeting capture (microphone + system audio).
//!
//! This module only defines the contract the `MeetingManager` depends on.
//! The real backends (cpal microphone track, WASAPI process/endpoint loopback
//! on Windows, CoreAudio tap on macOS) are implemented by issue #3 (T1) and
//! #11 (T9). Until they land, [`start_capture`] returns
//! [`CaptureError::NotImplemented`] so the rest of the meeting pipeline
//! (storage, crash recovery, retention) can be built and tested against it.
//!
//! On-disk contract shared with crash recovery: each track is a mono 16 kHz
//! i16 WAV written incrementally (`hound::WavWriter` flushed periodically), so
//! a crash leaves a file whose header sizes may be stale but whose samples are
//! intact. `crate::meeting::wav_repair` recomputes the sizes from the file
//! length.

// The backends that construct and consume most of these types land with #3.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::PathBuf;

/// File name of the microphone track inside a meeting folder.
pub const MIC_FILE: &str = "mic.wav";
/// File name of the system-audio track inside a meeting folder.
pub const SYSTEM_FILE: &str = "system.wav";

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Microphone name as stored in `selected_microphone` (None = default).
    pub mic_device: Option<String>,
    /// Whether to also record the system output (loopback / tap).
    pub capture_system: bool,
    /// Folder the track files are written into.
    pub out_dir: PathBuf,
}

/// How the system track is being captured. Decides whether dictation
/// feedback sounds would leak into the "others" track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SystemCaptureMode {
    /// Windows 10 20348+/11: everything except Trigo Whispr's own process tree.
    ProcessLoopback,
    /// Older Windows: loopback of the default output endpoint (captures our
    /// own feedback sounds).
    EndpointLoopback,
    /// macOS CoreAudio process tap (#11), excludes our own process.
    CoreAudioTap,
}

impl SystemCaptureMode {
    /// True when sounds played by this app end up in the system track.
    pub fn captures_own_sounds(self) -> bool {
        matches!(self, SystemCaptureMode::EndpointLoopback)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CaptureWarning {
    SystemAudioUnavailable,
    SystemAudioSilent,
    OutputMuted,
    DeviceChanged,
    MicUnavailable,
}

#[derive(Debug, Clone)]
pub struct CaptureOutput {
    pub mic_path: Option<PathBuf>,
    pub system_path: Option<PathBuf>,
    pub duration_ms: u64,
    pub warnings: Vec<CaptureWarning>,
    pub system_mode: Option<SystemCaptureMode>,
}

#[derive(Debug)]
pub enum CaptureError {
    Unsupported,
    NotImplemented,
    Other(String),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::Unsupported => {
                write!(f, "meeting capture is not supported on this platform")
            }
            CaptureError::NotImplemented => write!(
                f,
                "meeting capture backend is not implemented yet (see issue #3)"
            ),
            CaptureError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// A running capture. Owned by the `MeetingManager` for the whole meeting.
pub trait CaptureSession: Send {
    /// Stops the streams, pads both tracks to the same length and finalizes
    /// the WAV files.
    fn stop(self: Box<Self>) -> anyhow::Result<CaptureOutput>;
    fn elapsed_ms(&self) -> u64;
    fn warnings(&self) -> Vec<CaptureWarning>;
    /// RMS level per track: (mic, system), 0.0..=1.0.
    fn levels(&self) -> (f32, f32);
    /// Mode used for the system track, None when it is not captured.
    fn system_mode(&self) -> Option<SystemCaptureMode>;
}

/// Whether this platform can capture system audio at all.
pub fn system_capture_supported() -> bool {
    false
}

/// Whether meetings can be recorded on this platform (Windows and macOS).
pub fn meetings_supported() -> bool {
    cfg!(any(target_os = "windows", target_os = "macos"))
}

pub fn start_capture(_cfg: CaptureConfig) -> Result<Box<dyn CaptureSession>, CaptureError> {
    if !meetings_supported() {
        return Err(CaptureError::Unsupported);
    }
    Err(CaptureError::NotImplemented)
}
