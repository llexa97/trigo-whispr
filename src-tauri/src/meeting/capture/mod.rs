// Adapted from fly-on-the-wall (MIT) — https://github.com/swarnavspujari/fly-on-the-wall/blob/5f6f56d3ed86b835ab2f1598c8ff444ed5d1bd93/crates/fly-audio/src/lib.rs
//
//! Meeting capture: two raw tracks — the microphone and the system output
//! (the other participants) — aligned on wall-clock time and streamed to disk
//! while recording.
//!
//! This is deliberately a separate path from dictation: dictation goes
//! through `handy-recorder` + `audio_toolkit::audio::sink`, which keeps only
//! VAD-selected speech in memory. A meeting needs the opposite (everything,
//! on disk, crash-safe), so it opens its own streams; the microphone is
//! opened in shared mode, so dictation keeps working during a meeting.
//!
//! On disk: `mic.wav` and `system.wav` in [`CaptureConfig::out_dir`], mono
//! 16 kHz 16-bit PCM (≈ 115 MB per hour per track). Headers are rewritten
//! every few seconds; a file left behind by a crash is readable up to the
//! last flush, and [`repair_wav`] recovers the rest.
//!
//! Platforms:
//! - Windows: microphone + system audio (process loopback excluding Trigo
//!   Whispr on build 20348+, endpoint loopback of the default output before).
//! - macOS 14.6+: microphone + system audio via a Core Audio process tap on
//!   the default output-only device. Older macOS versions record mic only.
//! - Linux: unsupported ([`start_capture`] returns [`CaptureError::Unsupported`]).

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use specta::Type;

// Compiled everywhere so it stays type-checked; Linux never starts it.
#[cfg_attr(target_os = "linux", allow(dead_code))]
mod cpal_backend;
pub mod mix;
#[cfg_attr(target_os = "linux", allow(dead_code))]
mod source;
#[cfg_attr(target_os = "linux", allow(dead_code))]
mod track;
#[cfg(target_os = "windows")]
mod win_process_loopback;
#[cfg(target_os = "windows")]
mod win_volume;

pub use mix::repair_wav;

/// Sample rate of both tracks on disk — what the ASR engines take natively.
pub const TRACK_SAMPLE_RATE: u32 = 16_000;
pub const MIC_FILE_NAME: &str = "mic.wav";
pub const SYSTEM_FILE_NAME: &str = "system.wav";

#[derive(Debug)]
pub enum CaptureError {
    /// Meeting capture is not available on this platform.
    Unsupported,
    /// Neither the microphone nor the system audio could be opened.
    NoInput(String),
    Io(std::io::Error),
    Backend(String),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => write!(f, "meeting capture is not supported on this platform"),
            Self::NoInput(why) => write!(f, "no audio input could be opened: {why}"),
            Self::Io(e) => write!(f, "meeting capture I/O error: {e}"),
            Self::Backend(e) => write!(f, "meeting capture error: {e}"),
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<std::io::Error> for CaptureError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, CaptureError>;

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Microphone name as listed by the recorder (`selected_microphone`);
    /// `None` or an unplugged device = system default microphone.
    pub mic_device: Option<String>,
    /// Record the system output as its own track.
    pub capture_system: bool,
    /// Directory `mic.wav` / `system.wav` are written into (created if needed).
    pub out_dir: PathBuf,
}

/// Something that degrades a capture. Startup conditions and one-off events
/// (`MicUnavailable`, `SystemAudioUnavailable`, `DeviceChanged`) stick for the
/// whole session; `OutputMuted` and `SystemAudioSilent` reflect the current
/// state and clear when it is fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CaptureWarning {
    /// The system track could not be opened: only the microphone is recorded.
    SystemAudioUnavailable,
    /// The system track has recorded nothing but digital silence for a while.
    SystemAudioSilent,
    /// The default output is muted or at 0 %, so loopback may record silence.
    OutputMuted,
    /// An input device failed or the default device changed; it was reopened
    /// and the gap filled with silence.
    DeviceChanged,
    /// The microphone could not be opened (missing, or access denied in the
    /// Windows privacy settings): only the system audio is recorded.
    MicUnavailable,
}

/// How the system track is captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SystemCaptureMode {
    /// Windows 10 build 20348+ / Windows 11: every process except Trigo
    /// Whispr's own tree (its feedback sounds stay out of the recording);
    /// follows default-output changes by itself.
    ProcessLoopback,
    /// Older Windows: loopback of the default output endpoint (includes
    /// Trigo Whispr's own sounds).
    EndpointLoopback,
    /// macOS 14.6+: Core Audio process tap on the default output device.
    /// Includes Trigo Whispr's own sounds.
    CoreAudioTap,
}

/// What a finished capture produced. Paths are absolute; both tracks have
/// the same duration.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct CaptureOutput {
    pub mic_path: Option<PathBuf>,
    pub system_path: Option<PathBuf>,
    pub duration_ms: u64,
    pub system_mode: Option<SystemCaptureMode>,
    pub warnings: Vec<CaptureWarning>,
}

/// A live meeting capture, obtained from [`start_capture`]. Dropping it
/// without calling [`CaptureSession::stop`] still finalizes the files.
pub trait CaptureSession: Send {
    /// Stop recording, pad both tracks to the same length and finalize them.
    fn stop(self: Box<Self>) -> Result<CaptureOutput>;
    /// Wall-clock time since the capture started.
    fn elapsed_ms(&self) -> u64;
    /// Current warnings (sticky ones plus the live state).
    fn warnings(&self) -> Vec<CaptureWarning>;
    /// Recent RMS level of `(mic, system)`, 0.0–1.0.
    fn levels(&self) -> (f32, f32);
    /// How the system track is being captured, if it is.
    fn system_mode(&self) -> Option<SystemCaptureMode>;
}

/// Whether this platform can record the system output.
pub fn system_capture_supported() -> bool {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSOperatingSystemVersion, NSProcessInfo};
        NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
            majorVersion: 14,
            minorVersion: 6,
            patchVersion: 0,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        cfg!(target_os = "windows")
    }
}

/// Whether meeting capture (at least the microphone track) is available.
pub fn capture_supported() -> bool {
    cfg!(any(target_os = "windows", target_os = "macos"))
}

/// Start a meeting capture. Fails only when nothing at all can be recorded;
/// a missing track is reported through [`CaptureSession::warnings`].
pub fn start_capture(cfg: CaptureConfig) -> Result<Box<dyn CaptureSession>> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        cpal_backend::start(cfg)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = cfg;
        Err(CaptureError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warnings_serialize_as_snake_case() {
        let json = serde_json::to_string(&[
            CaptureWarning::SystemAudioUnavailable,
            CaptureWarning::OutputMuted,
            CaptureWarning::DeviceChanged,
        ])
        .unwrap();
        assert_eq!(
            json,
            r#"["system_audio_unavailable","output_muted","device_changed"]"#
        );
        assert_eq!(
            serde_json::to_string(&SystemCaptureMode::ProcessLoopback).unwrap(),
            r#""process_loopback""#
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_is_unsupported() {
        assert!(!system_capture_supported());
        let err = start_capture(CaptureConfig {
            mic_device: None,
            capture_system: true,
            out_dir: std::env::temp_dir(),
        })
        .err()
        .expect("capture must not start on Linux");
        assert!(matches!(err, CaptureError::Unsupported));
    }
}
