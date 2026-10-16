use crate::managers::audio::AudioRecordingManager;
use crate::meeting::capture::{self, CaptureConfig, CaptureOutput};
use serde::Serialize;
use specta::Type;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};

const MAX_TEST_SECONDS: u32 = 60;

#[derive(Serialize, Type)]
pub struct MeetingCaptureTestReport {
    /// Temporary directory holding `mic.wav` / `system.wav`.
    pub out_dir: String,
    pub output: CaptureOutput,
}

/// Debug: record a short meeting capture (microphone + system audio) into a
/// fresh temporary directory and report where the tracks went.
#[tauri::command]
#[specta::specta]
pub async fn test_meeting_capture(
    app: AppHandle,
    seconds: u32,
) -> Result<MeetingCaptureTestReport, String> {
    let mic_device = app
        .try_state::<Arc<AudioRecordingManager>>()
        .and_then(|manager| manager.preferred_microphone_name());
    let out_dir = std::env::temp_dir().join(format!(
        "trigo-whispr-meeting-test-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    let seconds = seconds.clamp(1, MAX_TEST_SECONDS);

    tauri::async_runtime::spawn_blocking(move || {
        let session = capture::start_capture(CaptureConfig {
            mic_device,
            capture_system: capture::system_capture_supported(),
            out_dir: out_dir.clone(),
        })
        .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_secs(seconds as u64));
        let output = session.stop().map_err(|e| e.to_string())?;
        log::info!(
            "meeting capture test: {} ms in {}",
            output.duration_ms,
            out_dir.display()
        );
        Ok(MeetingCaptureTestReport {
            out_dir: out_dir.to_string_lossy().into_owned(),
            output,
        })
    })
    .await
    .map_err(|e| format!("meeting capture test panicked: {e}"))?
}
