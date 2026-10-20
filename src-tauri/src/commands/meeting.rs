use crate::managers::audio::AudioRecordingManager;
use crate::managers::meeting::folder::RecoveryReport;
use crate::managers::meeting::store::{MeetingDetail, PaginatedMeetings};
use crate::managers::meeting::{
    MeetingAudioPaths, MeetingManager, MeetingSource, MeetingStateChanged,
};
use crate::managers::transcription::meeting::MeetingTranscriber;
use crate::meeting::capture::{self, CaptureConfig, CaptureOutput};
use crate::meeting::detection::prompt::MeetingPrompt;
use crate::meeting::detection::service::{
    self as detection, MeetingDetectionApp, MeetingPromptResponse, MicUsageReport,
};
use crate::settings::{MeetingAudioRetention, MeetingAutoStop};
use serde::Serialize;
use specta::Type;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;

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

/// Starts a meeting recording the way the tray and the shortcut do: the
/// consent reminder first, and no concurrent start or stop. Returns at once;
/// the recording shows up through `MeetingStateChanged`, a failure in a
/// dialog.
#[tauri::command]
#[specta::specta]
pub fn request_meeting_start(app: AppHandle) {
    crate::meeting::control::start_meeting(&app, "meetings section");
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

/// Meetings whose title or transcript contains `query`, newest first.
#[tauri::command]
#[specta::specta]
pub async fn search_meetings(
    meeting_manager: State<'_, Arc<MeetingManager>>,
    query: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<PaginatedMeetings, String> {
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE);
    blocking(&meeting_manager, move |m| m.search(&query, offset, limit)).await
}

#[tauri::command]
#[specta::specta]
pub async fn open_meeting_folder(
    app: AppHandle,
    meeting_manager: State<'_, Arc<MeetingManager>>,
    id: String,
) -> Result<(), String> {
    let folder = blocking(&meeting_manager, move |m| m.folder_path(&id)).await?;
    app.opener()
        .open_path(folder.to_string_lossy(), None::<String>)
        .map_err(|e| format!("Failed to open meeting folder: {e}"))
}

/// The meeting's transcript in the `transcript.md` format.
#[tauri::command]
#[specta::specta]
pub async fn get_meeting_markdown(
    meeting_manager: State<'_, Arc<MeetingManager>>,
    id: String,
) -> Result<String, String> {
    blocking(&meeting_manager, move |m| m.markdown(&id)).await
}

#[tauri::command]
#[specta::specta]
pub async fn get_meeting(
    meeting_manager: State<'_, Arc<MeetingManager>>,
    id: String,
) -> Result<Option<MeetingDetail>, String> {
    blocking(&meeting_manager, move |m| m.get(&id)).await
}

/// Names a person the speaker detection found in a meeting; an empty name
/// goes back to "Speaker N".
#[tauri::command]
#[specta::specta]
pub async fn rename_meeting_speaker(
    meeting_manager: State<'_, Arc<MeetingManager>>,
    id: String,
    speaker_id: u32,
    name: String,
) -> Result<(), String> {
    blocking(&meeting_manager, move |m| {
        m.rename_speaker(&id, speaker_id, &name)
    })
    .await
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

/// Stops a meeting's transcription (or takes it out of the queue); it is left
/// `pending`, to be retried. Returns whether it was queued or running.
#[tauri::command]
#[specta::specta]
pub fn cancel_meeting_transcription(
    transcriber: State<'_, MeetingTranscriber>,
    id: String,
) -> bool {
    transcriber.cancel(&id)
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

/// Applies `change` to the settings and saves them.
fn update_settings(app: &AppHandle, change: impl FnOnce(&mut crate::settings::AppSettings)) {
    let mut settings = crate::settings::get_settings(app);
    change(&mut settings);
    crate::settings::write_settings(app, settings);
}

#[tauri::command]
#[specta::specta]
pub fn change_meeting_detection_enabled_setting(app: AppHandle, enabled: bool) {
    update_settings(&app, |s| s.meeting_detection_enabled = enabled);
    detection::sync(&app);
}

#[tauri::command]
#[specta::specta]
pub fn change_speaker_detection_enabled_setting(app: AppHandle, enabled: bool) {
    update_settings(&app, |s| s.speaker_detection_enabled = enabled);
    if enabled {
        if let Some(models) = app.try_state::<Arc<crate::managers::model::ModelManager>>() {
            models.ensure_diarization_model();
        }
    }
}

/// Per-app overrides of the detection catalog's defaults.
#[tauri::command]
#[specta::specta]
pub fn change_meeting_detection_apps_setting(app: AppHandle, apps: HashMap<String, bool>) {
    update_settings(&app, |s| s.meeting_detection_apps = apps);
    detection::settings_changed(&app);
}

#[tauri::command]
#[specta::specta]
pub fn change_meeting_auto_start_setting(app: AppHandle, enabled: bool) {
    update_settings(&app, |s| s.meeting_auto_start = enabled);
    detection::settings_changed(&app);
}

#[tauri::command]
#[specta::specta]
pub fn change_meeting_auto_stop_setting(app: AppHandle, mode: MeetingAutoStop) {
    update_settings(&app, |s| s.meeting_auto_stop = mode);
    // Withdraw or re-offer a pending stop in the new mode right away.
    detection::settings_changed(&app);
}

#[tauri::command]
#[specta::specta]
pub fn change_meeting_capture_system_audio_setting(app: AppHandle, enabled: bool) {
    update_settings(&app, |s| s.meeting_capture_system_audio = enabled);
}

#[tauri::command]
#[specta::specta]
pub fn change_meeting_prompt_sound_setting(app: AppHandle, enabled: bool) {
    update_settings(&app, |s| s.meeting_prompt_sound = enabled);
}

/// The meeting apps detection knows on this platform.
#[tauri::command]
#[specta::specta]
pub fn get_meeting_detection_apps() -> Vec<MeetingDetectionApp> {
    detection::apps()
}

/// Whether this platform has a backend to detect meetings yet.
#[tauri::command]
#[specta::specta]
pub fn is_meeting_detection_supported() -> bool {
    detection::supported()
}

/// Debug: the apps the platform probe sees using the microphone right now.
#[tauri::command]
#[specta::specta]
pub fn get_mic_usage() -> MicUsageReport {
    detection::mic_usage_report()
}

/// Whether this machine can record the system track of a meeting (macOS
/// version, platform). Whether the tap really delivers audio is only known
/// once a capture runs.
#[tauri::command]
#[specta::specta]
pub fn get_system_audio_capture_support() -> capture::SystemAudioCaptureSupport {
    capture::system_audio_capture_support()
}

/// What the detection popup should show (it may load after the event).
#[tauri::command]
#[specta::specta]
pub fn get_meeting_prompt() -> Option<MeetingPrompt> {
    crate::meeting::detection::prompt::current()
}

#[tauri::command]
#[specta::specta]
pub fn respond_meeting_prompt(app: AppHandle, response: MeetingPromptResponse) {
    detection::respond(&app, response);
}
