//! Automatic meeting detection: notice that a known meeting app started using
//! the microphone and offer to record it.
//!
//! Platform independent: an OS backend only answers "who is using the
//! microphone right now" through [`MicUsageProbe`]. The [`catalog`] maps
//! those apps to meeting apps, the [`engine`] decides when to prompt (and
//! when a detected meeting has ended), [`service`] runs it on a background
//! thread and [`prompt`] is the popup window.

pub mod catalog;
pub mod engine;
pub mod prompt;
pub mod service;

use anyhow::Result;
use std::path::PathBuf;

/// An app currently using the microphone, as reported by the OS backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicUser {
    /// Normalized per platform: the lowercase executable name
    /// (`ms-teams.exe`) or package family name (`MSTeams_8wekyb3d8bbwe`) on
    /// Windows, the bundle ID on macOS.
    pub app_id: String,
    pub display_name: Option<String>,
    pub pid: Option<u32>,
    pub exe_path: Option<PathBuf>,
}

impl MicUser {
    pub fn new(app_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            display_name: None,
            pid: None,
            exe_path: None,
        }
    }
}

/// Lists the apps using the microphone. Implemented once per OS.
pub trait MicUsageProbe: Send {
    fn active_mic_users(&mut self) -> Result<Vec<MicUser>>;
    /// False when this platform cannot tell (no backend yet): detection then
    /// does not run at all.
    fn supported(&self) -> bool;
}

/// Backend for platforms without one (Linux, and Windows/macOS until their
/// backends land): never sees anyone.
pub struct NullProbe;

impl MicUsageProbe for NullProbe {
    fn active_mic_users(&mut self) -> Result<Vec<MicUser>> {
        Ok(Vec::new())
    }

    fn supported(&self) -> bool {
        false
    }
}

/// The probe for the current platform.
pub fn platform_probe() -> Box<dyn MicUsageProbe> {
    Box::new(NullProbe)
}
