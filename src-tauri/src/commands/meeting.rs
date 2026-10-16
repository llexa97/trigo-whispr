use crate::managers::audio::AudioRecordingManager;
use crate::managers::meeting::folder::RecoveryReport;
use crate::managers::meeting::store::{MeetingDetail, PaginatedMeetings};
use crate::managers::meeting::{
    MeetingAudioPaths, MeetingManager, MeetingSource, MeetingStateChanged,
};
use crate::meeting::capture::{self, CaptureConfig, CaptureOutput};
use crate::settings::MeetingAudioRetention;
use serde::Serialize;
use specta::Type;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager, State};

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

const DEFAULT_PAGE_SIZE: usize = 50;

/// Runs a blocking manager call (capture stop, file I/O) off the async
/// runtime's worker threads.
async fn blocking<T: Send + 'static>(
    manager: &Arc<MeetingManager>,
    f: impl FnOnce(&MeetingManager) -> anyhow::Result<T> + Send + 'static,
) -> Result<T, String> {
    let manager = manager.clone();
    tauri::async_runtime::spawn_blocking(move || f(&manager))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn start_meeting(
    meeting_manager: State<'_, Arc<MeetingManager>>,
    source: Option<MeetingSource>,
) -> Result<String, String> {
    let source = source.unwrap_or(MeetingSource::Manual);
    blocking(&meeting_manager, move |m| m.start(source)).await
}

#[tauri::command]
#[specta::specta]
pub async fn stop_meeting(
    meeting_manager: State<'_, Arc<MeetingManager>>,
) -> Result<String, String> {
    blocking(&meeting_manager, |m| m.stop()).await
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_meeting(meeting_manager: State<'_, Arc<MeetingManager>>) -> Result<(), String> {
    blocking(&meeting_manager, |m| m.cancel()).await
}

#[tauri::command]
#[specta::specta]
pub fn get_meeting_state(meeting_manager: State<'_, Arc<MeetingManager>>) -> MeetingStateChanged {
    meeting_manager.state()
}

#[tauri::command]
#[specta::specta]
pub async fn list_meetings(
    meeting_manager: State<'_, Arc<MeetingManager>>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<PaginatedMeetings, String> {
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE);
    blocking(&meeting_manager, move |m| m.list(offset, limit)).await
}

#[tauri::command]
#[specta::specta]
pub async fn get_meeting(
    meeting_manager: State<'_, Arc<MeetingManager>>,
    id: String,
) -> Result<Option<MeetingDetail>, String> {
    blocking(&meeting_manager, move |m| m.get(&id)).await
}

#[tauri::command]
#[specta::specta]
pub async fn rename_meeting(
    meeting_manager: State<'_, Arc<MeetingManager>>,
    id: String,
    title: String,
) -> Result<(), String> {
    blocking(&meeting_manager, move |m| m.rename(&id, &title)).await
}

#[tauri::command]
#[specta::specta]
pub async fn delete_meeting(
    meeting_manager: State<'_, Arc<MeetingManager>>,
    id: String,
) -> Result<(), String> {
    blocking(&meeting_manager, move |m| m.delete(&id)).await
}

#[tauri::command]
#[specta::specta]
pub async fn get_meeting_audio_paths(
    meeting_manager: State<'_, Arc<MeetingManager>>,
    id: String,
) -> Result<MeetingAudioPaths, String> {
    blocking(&meeting_manager, move |m| m.audio_paths(&id)).await
}

#[tauri::command]
#[specta::specta]
pub async fn retry_meeting_transcription(
    meeting_manager: State<'_, Arc<MeetingManager>>,
    id: String,
) -> Result<(), String> {
    blocking(&meeting_manager, move |m| m.retry_transcription(&id)).await
}

/// Report of the meetings recovered at startup, returned only once.
#[tauri::command]
#[specta::specta]
pub fn take_meeting_recovery_notice(
    meeting_manager: State<'_, Arc<MeetingManager>>,
) -> Option<RecoveryReport> {
    meeting_manager.take_recovery_notice()
}

#[tauri::command]
#[specta::specta]
pub async fn update_meeting_audio_retention(
    app: AppHandle,
    meeting_manager: State<'_, Arc<MeetingManager>>,
    retention: MeetingAudioRetention,
) -> Result<(), String> {
    let mut settings = crate::settings::get_settings(&app);
    settings.meeting_audio_retention = retention;
    crate::settings::write_settings(&app, settings);
    blocking(&meeting_manager, |m| m.apply_retention()).await
}

#[tauri::command]
#[specta::specta]
pub fn change_show_meeting_indicator_setting(app: AppHandle, enabled: bool) {
    let mut settings = crate::settings::get_settings(&app);
    settings.show_meeting_indicator = enabled;
    crate::settings::write_settings(&app, settings);
    crate::meeting::indicator::refresh(&app);
}

#[tauri::command]
#[specta::specta]
pub fn change_meeting_consent_reminder_setting(app: AppHandle, enabled: bool) {
    let mut settings = crate::settings::get_settings(&app);
    settings.meeting_consent_reminder = enabled;
    crate::settings::write_settings(&app, settings);
}
