use crate::managers::meeting::folder::RecoveryReport;
use crate::managers::meeting::store::{MeetingDetail, PaginatedMeetings};
use crate::managers::meeting::{
    MeetingAudioPaths, MeetingManager, MeetingSource, MeetingStateChanged,
};
use crate::settings::MeetingAudioRetention;
use std::sync::Arc;
use tauri::{AppHandle, State};

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
