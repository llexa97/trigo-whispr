use crate::audio_toolkit::audio::file::SUPPORTED_EXTENSIONS;
use crate::managers::file_transcription::store::{
    FileTranscriptionDetail, FileTranscriptionStatus, PaginatedFileTranscriptions,
};
use crate::managers::file_transcription::{
    FileImportOptions, FileImportOutcome, FileTranscriptionManager,
};
use crate::managers::transcription::file::{FileTranscriber, FileTranscriptionStore};
use crate::settings::MeetingAudioRetention;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use tauri_specta::Event;

const DEFAULT_PAGE_SIZE: usize = 50;

/// Files imported from outside the Transcriptions page (the tray): the page
/// opens and reports the outcome.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct FilesImported {
    pub outcome: FileImportOutcome,
}

/// Runs a blocking manager call (file I/O, decoding a header) off the async
/// runtime's worker threads.
async fn blocking<T: Send + 'static>(
    manager: &Arc<FileTranscriptionManager>,
    f: impl FnOnce(&FileTranscriptionManager) -> anyhow::Result<T> + Send + 'static,
) -> Result<T, String> {
    let manager = manager.clone();
    tauri::async_runtime::spawn_blocking(move || f(&manager))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

/// The extensions the import accepts, lower case and without the dot.
#[tauri::command]
#[specta::specta]
pub fn get_supported_transcription_extensions() -> Vec<String> {
    SUPPORTED_EXTENSIONS.iter().map(|e| e.to_string()).collect()
}

/// Queues each file for transcription, in order; the ones that cannot be
/// decoded are returned with the reason.
#[tauri::command]
#[specta::specta]
pub async fn import_files_for_transcription(
    manager: State<'_, Arc<FileTranscriptionManager>>,
    paths: Vec<String>,
    options: FileImportOptions,
) -> Result<FileImportOutcome, String> {
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    blocking(&manager, move |m| Ok(m.import(&paths, &options))).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_file_transcriptions(
    manager: State<'_, Arc<FileTranscriptionManager>>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<PaginatedFileTranscriptions, String> {
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE);
    blocking(&manager, move |m| m.list(offset, limit)).await
}

/// Transcriptions whose file name or text contains `query`, newest first.
#[tauri::command]
#[specta::specta]
pub async fn search_file_transcriptions(
    manager: State<'_, Arc<FileTranscriptionManager>>,
    query: String,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<PaginatedFileTranscriptions, String> {
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE);
    blocking(&manager, move |m| m.search(&query, offset, limit)).await
}

#[tauri::command]
#[specta::specta]
pub async fn get_file_transcription(
    manager: State<'_, Arc<FileTranscriptionManager>>,
    id: String,
) -> Result<Option<FileTranscriptionDetail>, String> {
    blocking(&manager, move |m| m.get(&id)).await
}

/// The decoded audio (16 kHz WAV) of a transcription, if it is kept.
#[tauri::command]
#[specta::specta]
pub async fn get_file_transcription_audio_path(
    manager: State<'_, Arc<FileTranscriptionManager>>,
    id: String,
) -> Result<Option<String>, String> {
    blocking(&manager, move |m| m.audio_path(&id)).await
}

/// Runs a transcription again, with another language or the model now
/// selected.
#[tauri::command]
#[specta::specta]
pub async fn retry_file_transcription(
    manager: State<'_, Arc<FileTranscriptionManager>>,
    id: String,
    options: FileImportOptions,
) -> Result<(), String> {
    blocking(&manager, move |m| m.retry(&id, &options)).await
}

/// Names a person the speaker detection found in a transcript; an empty
/// name goes back to "Speaker N".
#[tauri::command]
#[specta::specta]
pub async fn rename_file_transcription_speaker(
    manager: State<'_, Arc<FileTranscriptionManager>>,
    id: String,
    speaker_id: u32,
    name: String,
) -> Result<(), String> {
    blocking(&manager, move |m| m.rename_speaker(&id, speaker_id, &name)).await
}

/// Stops a transcription, or takes it out of the queue. Returns false if it
/// was neither queued nor running (or is already being saved).
#[tauri::command]
#[specta::specta]
pub async fn cancel_file_transcription(
    manager: State<'_, Arc<FileTranscriptionManager>>,
    transcriber: State<'_, FileTranscriber>,
    id: String,
) -> Result<bool, String> {
    let transcriber = transcriber.inner().clone();
    blocking(&manager, move |m| {
        if !transcriber.cancel(&id) {
            return Ok(false);
        }
        // A running job records its own cancel; a dequeued one never ran.
        if !transcriber.is_current(&id)
            && m.get(&id)?
                .is_some_and(|d| d.transcription.status == FileTranscriptionStatus::Pending)
        {
            m.set_status(&id, FileTranscriptionStatus::Cancelled, None);
        }
        Ok(true)
    })
    .await
}

/// Deletes a transcription and its folder, stopping it first if needed.
#[tauri::command]
#[specta::specta]
pub async fn delete_file_transcription(
    manager: State<'_, Arc<FileTranscriptionManager>>,
    transcriber: State<'_, FileTranscriber>,
    id: String,
) -> Result<(), String> {
    let transcriber = transcriber.inner().clone();
    blocking(&manager, move |m| {
        if !transcriber.stop(&id) {
            anyhow::bail!("The transcription is still stopping; try again in a moment");
        }
        m.delete(&id)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn open_file_transcription_folder(
    app: AppHandle,
    manager: State<'_, Arc<FileTranscriptionManager>>,
    id: String,
) -> Result<(), String> {
    let folder = blocking(&manager, move |m| m.folder_path(&id)).await?;
    app.opener()
        .open_path(folder.to_string_lossy(), None::<String>)
        .map_err(|e| format!("Failed to open the transcription folder: {e}"))
}

#[tauri::command]
#[specta::specta]
pub async fn update_file_transcription_audio_retention(
    app: AppHandle,
    manager: State<'_, Arc<FileTranscriptionManager>>,
    retention: MeetingAudioRetention,
) -> Result<(), String> {
    let mut settings = crate::settings::get_settings(&app);
    settings.file_transcription_audio_retention = retention;
    crate::settings::write_settings(&app, settings);
    blocking(&manager, |m| m.apply_retention()).await
}

/// The tray's "Transcribe a file…": a native file picker, then the files are
/// imported with the default options and the Transcriptions page shown.
pub fn pick_and_import(app: &AppHandle) {
    use tauri::Manager;
    use tauri_plugin_dialog::DialogExt;

    let app = app.clone();
    // The picker blocks: keep it off the event loop thread.
    std::thread::spawn(move || {
        let strings = crate::tray_i18n::get_tray_translations(Some(
            crate::settings::get_settings(&app).app_language,
        ));
        let Some(files) = app
            .dialog()
            .file()
            .set_title(strings.transcribe_file.trim_end_matches(['…', '.']))
            .add_filter("Audio / video", SUPPORTED_EXTENSIONS)
            .blocking_pick_files()
        else {
            return;
        };
        let paths: Vec<PathBuf> = files
            .into_iter()
            .filter_map(|f| f.into_path().ok())
            .collect();
        if paths.is_empty() {
            return;
        }
        let Some(manager) = app.try_state::<Arc<FileTranscriptionManager>>() else {
            return;
        };
        let outcome = manager.import(&paths, &FileImportOptions::default());
        crate::show_main_window(&app);
        if let Err(e) = (FilesImported { outcome }).emit(&app) {
            log::error!("Failed to emit files-imported event: {e}");
        }
    });
}
