// Adapted from fly-on-the-wall (MIT) — https://github.com/swarnavspujari/fly-on-the-wall/blob/5f6f56d3ed86b835ab2f1598c8ff444ed5d1bd93/crates/fly-audio/src/win_volume.rs
//
//! Windows-only: read the default render endpoint's master volume + mute.
//!
//! Loopback records what the system actually plays — if the output is muted
//! or at 0 %, the system track may be silence and the user only finds out
//! after the meeting. The capture thread polls this to raise
//! `CaptureWarning::OutputMuted` while recording.

use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

/// `(volume_scalar 0..1, muted)` of the default output device, or `None` if
/// anything in the COM chain fails (no default device, service down, …).
/// The calling thread must have initialized COM.
pub fn default_output_volume() -> Option<(f32, bool)> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).ok()?;
        let scalar = volume.GetMasterVolumeLevelScalar().ok()?;
        let muted = volume.GetMute().ok()?.as_bool();
        Some((scalar, muted))
    }
}

/// Whether the default output is effectively silent (muted or below 1 %).
pub fn output_muted() -> bool {
    matches!(default_output_volume(), Some((volume, muted)) if muted || volume < 0.01)
}
