//! Transcriptions of imported audio and video files (the Transcriptions
//! page): import, persistence (`transcriptions.db` + one folder per file under
//! `<app_data_dir>/transcriptions/`), re-runs, deletion, crash recovery and
//! the audio retention policy.
//!
//! The work itself is the `FileTranscriber`'s
//! (`managers::transcription::file`), which registers itself with
//! [`FileTranscriptionManager::set_queue_listener`] and reads and updates the
//! transcriptions through this manager's [`FileTranscriptionStore`]
//! implementation.

pub mod store;

use crate::audio_toolkit::audio::file::{is_supported_extension, AudioFileDecoder, DecodeError};
use crate::managers::transcription::file::{
    FileJob, FileTranscript, FileTranscriptionStore, AUDIO_FILE_NAME,
};
use crate::settings::{get_settings, MeetingAudioRetention};
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use log::{debug, error, info, warn};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use store::{
    FileTranscription, FileTranscriptionDetail, FileTranscriptionErrorCode,
    FileTranscriptionStatus, PaginatedFileTranscriptions, TranscriptResult,
};
use tauri::{AppHandle, Manager};
use tauri_specta::Event;

const RETENTION_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

/// Same model as `MeetingsUpdated`.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(tag = "action")]
pub enum FileTranscriptionsUpdated {
    #[serde(rename = "added")]
    Added { transcription: FileTranscription },
    #[serde(rename = "updated")]
    Updated { transcription: FileTranscription },
    #[serde(rename = "deleted")]
    Deleted { id: String },
}

/// How the files of one import are to be transcribed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
pub struct FileImportOptions {
    /// Forced language; `None` uses the selected one.
    pub language: Option<String>,
    /// Run the transcript through LLM post-processing.
    pub post_process: bool,
}

/// A file that was not imported, and why.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct RejectedFile {
    pub path: String,
    pub file_name: String,
    pub code: FileTranscriptionErrorCode,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
pub struct FileImportOutcome {
    pub added: Vec<FileTranscription>,
    pub rejected: Vec<RejectedFile>,
    /// Nothing was imported because no model is downloaded.
    pub no_model: bool,
}

type QueueListener = Arc<dyn Fn(&str) + Send + Sync>;

pub struct FileTranscriptionManager {
    app_handle: AppHandle,
    dir: PathBuf,
    db_path: PathBuf,
    queue_listener: Mutex<Option<QueueListener>>,
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Whether a transcription's decoded audio is past the retention window.
/// Audio still waiting for (or undergoing) transcription is always kept.
fn audio_expired(t: &FileTranscription, policy: MeetingAudioRetention, now_ms: i64) -> bool {
    use FileTranscriptionStatus::*;
    let finished = matches!(t.status, Done | Failed | Cancelled);
    let max_age_days = match policy {
        MeetingAudioRetention::Forever => return false,
        MeetingAudioRetention::DeleteAfterTranscription => return t.status == Done,
        MeetingAudioRetention::Days7 => 7,
        MeetingAudioRetention::Days30 => 30,
        MeetingAudioRetention::Days90 => 90,
    };
    let reference = t.completed_at.unwrap_or(t.created_at);
    finished && now_ms - reference >= max_age_days * DAY_MS
}

/// Checks a file before it is queued: an extension we decode, a file that
/// is there, and a container that opens with an audio track. Returns the
/// length, when the container says.
fn probe(path: &Path) -> Result<Option<i64>, (FileTranscriptionErrorCode, String)> {
    if !is_supported_extension(path) {
        return Err((
            FileTranscriptionErrorCode::UnsupportedFormat,
            "unsupported file type".into(),
        ));
    }
    if !path.is_file() {
        return Err((
            FileTranscriptionErrorCode::FileMissing,
            "the file does not exist".into(),
        ));
    }
    match AudioFileDecoder::open(path) {
        Ok(decoder) => Ok(decoder.duration_ms().map(|ms| ms as i64)),
        Err(e) => {
            let code = match &e {
                DecodeError::Unsupported(_) => FileTranscriptionErrorCode::UnsupportedFormat,
                DecodeError::NoAudioTrack => FileTranscriptionErrorCode::NoAudioTrack,
                DecodeError::Missing => FileTranscriptionErrorCode::FileMissing,
                DecodeError::Corrupt(_) => FileTranscriptionErrorCode::CorruptFile,
                DecodeError::Io(_) | DecodeError::Cancelled => FileTranscriptionErrorCode::Failed,
            };
            Err((code, e.to_string()))
        }
    }
}

impl FileTranscriptionManager {
    pub fn new(app_handle: &AppHandle) -> Result<Self> {
        let app_data_dir = crate::portable::app_data_dir(app_handle)?;
        let dir = app_data_dir.join("transcriptions");
        fs::create_dir_all(&dir)?;
        let manager = Self {
            app_handle: app_handle.clone(),
            dir,
            db_path: app_data_dir.join("transcriptions.db"),
            queue_listener: Mutex::new(None),
        };
        let conn = manager.init_database()?;
        manager.recover_interrupted(&conn);
        Ok(manager)
    }

    /// Opens and migrates `transcriptions.db`. A database that cannot be
    /// opened is moved aside and recreated (the transcripts it held are lost,
    /// the app stays usable).
    fn init_database(&self) -> Result<Connection> {
        let open = |path: &Path| -> Result<Connection> {
            let mut conn = store::open(path)?;
            store::migrate(&mut conn)?;
            Ok(conn)
        };
        match open(&self.db_path) {
            Ok(conn) => Ok(conn),
            Err(e) => {
                let aside = self
                    .db_path
                    .with_extension(format!("db.corrupt-{}", now_ms()));
                error!(
                    "transcriptions.db unusable ({e}); moving it to {} and starting over",
                    aside.display()
                );
                fs::rename(&self.db_path, &aside)?;
                open(&self.db_path)
            }
        }
    }

    /// Transcriptions the app quit (or crashed) in the middle of are queued
    /// again, from the start; a partial decode is thrown away.
    fn recover_interrupted(&self, conn: &Connection) {
        let interrupted = match store::list_by_status(
            conn,
            &[
                FileTranscriptionStatus::Decoding,
                FileTranscriptionStatus::Transcribing,
            ],
        ) {
            Ok(rows) => rows,
            Err(e) => {
                error!("Failed to list interrupted file transcriptions: {e}");
                return;
            }
        };
        for t in interrupted {
            let _ = fs::remove_file(
                self.dir
                    .join(&t.folder)
                    .join(AUDIO_FILE_NAME)
                    .with_extension("wav.part"),
            );
            info!(
                "Resuming file transcription '{}' interrupted by a quit",
                t.id
            );
            if let Err(e) = store::set_status(conn, &t.id, FileTranscriptionStatus::Pending, None) {
                error!("Failed to requeue file transcription '{}': {e}", t.id);
            }
        }
    }

    fn conn(&self) -> Result<Connection> {
        store::open(&self.db_path)
    }

    fn get_row(&self, id: &str) -> Result<FileTranscription> {
        store::get(&self.conn()?, id)?.ok_or_else(|| anyhow!("Transcription {id} not found"))
    }

    fn emit_updated(&self, event: FileTranscriptionsUpdated) {
        if let Err(e) = event.emit(&self.app_handle) {
            error!("Failed to emit file-transcriptions-updated event: {e}");
        }
    }

    fn emit_row(&self, id: &str) {
        match self.conn().and_then(|c| store::get(&c, id)) {
            Ok(Some(transcription)) => {
                self.emit_updated(FileTranscriptionsUpdated::Updated { transcription })
            }
            Ok(None) => {}
            Err(e) => error!("Failed to load file transcription {id}: {e}"),
        }
    }

    /* ---------- queue ------------------------------------------------------ */

    /// Called with the id whenever a transcription is queued. The ones queued
    /// before registration are listed by [`Self::queued`].
    pub fn set_queue_listener(&self, listener: impl Fn(&str) + Send + Sync + 'static) {
        *self.queue_listener.lock().unwrap() = Some(Arc::new(listener));
    }

    fn notify_queue(&self, id: &str) {
        let listener = self.queue_listener.lock().unwrap().clone();
        match listener {
            Some(listener) => listener(id),
            None => debug!("File transcription {id} queued (no transcriber registered)"),
        }
    }

    /// Transcriptions waiting to run, oldest first.
    pub fn queued(&self) -> Result<Vec<FileTranscription>> {
        store::list_by_status(&self.conn()?, &[FileTranscriptionStatus::Pending])
    }

    /* ---------- import ----------------------------------------------------- */

    /// Queues each file that can be decoded; the others are returned with
    /// the reason. Nothing is imported while no model is downloaded.
    pub fn import(&self, paths: &[PathBuf], options: &FileImportOptions) -> FileImportOutcome {
        let mut outcome = FileImportOutcome::default();
        if !crate::managers::transcription::file::model_available(&self.app_handle) {
            outcome.no_model = true;
            return outcome;
        }
        for path in paths {
            match self.import_one(path, options) {
                Ok(transcription) => outcome.added.push(transcription),
                Err((code, detail)) => {
                    info!("Not importing {}: {detail}", path.display());
                    outcome.rejected.push(RejectedFile {
                        path: path.to_string_lossy().into_owned(),
                        file_name: file_name_of(path),
                        code,
                        detail,
                    });
                }
            }
        }
        outcome
    }

    fn import_one(
        &self,
        path: &Path,
        options: &FileImportOptions,
    ) -> Result<FileTranscription, (FileTranscriptionErrorCode, String)> {
        let duration_ms = probe(path)?;
        let failed = |e: anyhow::Error| (FileTranscriptionErrorCode::Failed, format!("{e:#}"));
        let id = uuid::Uuid::new_v4().to_string();
        // Named after the id: the file name may hold characters a folder
        // cannot, and two imports of one file must not collide.
        let folder = id.clone();
        fs::create_dir_all(self.dir.join(&folder)).map_err(|e| failed(e.into()))?;
        let source = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let transcription = FileTranscription {
            id: id.clone(),
            file_name: file_name_of(path),
            source_path: source.to_string_lossy().into_owned(),
            folder,
            duration_ms,
            created_at: now_ms(),
            completed_at: None,
            status: FileTranscriptionStatus::Pending,
            error_code: None,
            error: None,
            model_id: None,
            language: options.language.clone().filter(|l| !l.is_empty()),
            detected_language: None,
            post_process: options.post_process,
            post_processed_text: None,
            has_audio: false,
        };
        if let Err(e) = self.conn().and_then(|c| store::insert(&c, &transcription)) {
            let _ = fs::remove_dir_all(self.dir.join(&transcription.folder));
            return Err(failed(e));
        }
        info!("Imported {} as file transcription '{id}'", path.display());
        self.emit_updated(FileTranscriptionsUpdated::Added {
            transcription: transcription.clone(),
        });
        self.notify_queue(&id);
        Ok(transcription)
    }

    /* ---------- queries & edits -------------------------------------------- */

    pub fn list(&self, offset: usize, limit: usize) -> Result<PaginatedFileTranscriptions> {
        store::list(&self.conn()?, offset, limit)
    }

    pub fn search(
        &self,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> Result<PaginatedFileTranscriptions> {
        store::search(&self.conn()?, query, offset, limit)
    }

    pub fn get(&self, id: &str) -> Result<Option<FileTranscriptionDetail>> {
        let conn = self.conn()?;
        let Some(transcription) = store::get(&conn, id)? else {
            return Ok(None);
        };
        let segments = store::segments(&conn, id)?;
        Ok(Some(FileTranscriptionDetail {
            transcription,
            segments,
        }))
    }

    /// The decoded audio, if it is kept.
    pub fn audio_path(&self, id: &str) -> Result<Option<String>> {
        let t = self.get_row(id)?;
        let path = self.dir.join(&t.folder).join(AUDIO_FILE_NAME);
        Ok((t.has_audio && path.exists()).then(|| path.to_string_lossy().into_owned()))
    }

    pub fn folder_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.dir.join(self.get_row(id)?.folder))
    }

    /// Runs a finished, failed or cancelled transcription again, from the
    /// kept audio or else the original file.
    pub fn retry(&self, id: &str, options: &FileImportOptions) -> Result<()> {
        let t = self.get_row(id)?;
        if t.status == FileTranscriptionStatus::Pending || t.status.is_running() {
            bail!("Transcription {id} is already queued");
        }
        let has_wav = self.dir.join(&t.folder).join(AUDIO_FILE_NAME).exists();
        if !has_wav && !Path::new(&t.source_path).is_file() {
            bail!(
                "The audio is no longer kept and {} was moved or deleted",
                t.source_path
            );
        }
        let language = options.language.as_deref().filter(|l| !l.is_empty());
        store::requeue(&self.conn()?, id, language, options.post_process)?;
        self.emit_row(id);
        self.notify_queue(id);
        Ok(())
    }

    /// Deletes the transcription and its folder. The transcriber must have
    /// let go of it first.
    pub fn delete(&self, id: &str) -> Result<()> {
        let conn = self.conn()?;
        let t = store::get(&conn, id)?.ok_or_else(|| anyhow!("Transcription {id} not found"))?;
        match fs::remove_dir_all(self.dir.join(&t.folder)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        store::delete(&conn, id)?;
        self.emit_updated(FileTranscriptionsUpdated::Deleted { id: id.to_string() });
        Ok(())
    }

    /* ---------- retention -------------------------------------------------- */

    /// Deletes the decoded audio of transcriptions past the retention window;
    /// their transcript is kept.
    pub fn apply_retention(&self) -> Result<()> {
        let policy = get_settings(&self.app_handle).file_transcription_audio_retention;
        let conn = self.conn()?;
        let now = now_ms();
        for t in store::list_with_audio(&conn)? {
            if !audio_expired(&t, policy, now) {
                continue;
            }
            let path = self.dir.join(&t.folder).join(AUDIO_FILE_NAME);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    warn!("Retention: could not delete {}: {e}", path.display());
                    continue;
                }
            }
            store::set_audio(&conn, &t.id, false, None)?;
            debug!(
                "Retention removed the audio of file transcription '{}'",
                t.id
            );
            self.emit_row(&t.id);
        }
        Ok(())
    }

    /// Applies the retention policy now and then every 24 h.
    pub fn start_retention_timer(&self) {
        let app = self.app_handle.clone();
        let spawned = std::thread::Builder::new()
            .name("file-transcription-retention".into())
            .spawn(move || loop {
                if let Some(manager) = app.try_state::<Arc<FileTranscriptionManager>>() {
                    if let Err(e) = manager.apply_retention() {
                        error!("File transcription retention failed: {e}");
                    }
                }
                std::thread::sleep(RETENTION_INTERVAL);
            });
        if let Err(e) = spawned {
            warn!("Failed to spawn file transcription retention timer: {e}");
        }
    }
}

impl FileTranscriptionStore for FileTranscriptionManager {
    fn job(&self, id: &str) -> Result<FileJob> {
        let t = self.get_row(id)?;
        Ok(FileJob {
            id: t.id,
            source: PathBuf::from(t.source_path),
            dir: self.dir.join(t.folder),
            language: t.language,
            post_process: t.post_process,
        })
    }

    fn set_status(
        &self,
        id: &str,
        status: FileTranscriptionStatus,
        error: Option<(FileTranscriptionErrorCode, &str)>,
    ) {
        if let Err(e) = self
            .conn()
            .and_then(|c| store::set_status(&c, id, status, error))
        {
            error!(
                "Failed to set file transcription {id} to {}: {e}",
                status.as_str()
            );
            return;
        }
        self.emit_row(id);
        // `delete_after_transcription` applies as soon as the transcript is in.
        if status == FileTranscriptionStatus::Done {
            if let Err(e) = self.apply_retention() {
                error!("File transcription retention failed: {e}");
            }
        }
    }

    fn audio_decoded(&self, id: &str, duration_ms: i64) {
        if let Err(e) = self
            .conn()
            .and_then(|c| store::set_audio(&c, id, true, Some(duration_ms)))
        {
            error!("Failed to record the audio of file transcription {id}: {e}");
            return;
        }
        self.emit_row(id);
    }

    fn save_transcript(&self, id: &str, transcript: &FileTranscript) -> Result<()> {
        let mut conn = self.conn()?;
        store::save_result(
            &mut conn,
            id,
            &TranscriptResult {
                segments: &transcript.segments,
                model_id: &transcript.model_id,
                detected_language: transcript.language.as_deref(),
                post_processed_text: transcript.post_processed_text.as_deref(),
                completed_at: now_ms(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::tests::sample;

    #[test]
    fn retention_keeps_audio_still_needed() {
        let now = 100 * DAY_MS;
        let mut t = sample("a", 0, FileTranscriptionStatus::Pending);
        t.has_audio = true;
        for policy in [
            MeetingAudioRetention::Days7,
            MeetingAudioRetention::DeleteAfterTranscription,
        ] {
            assert!(!audio_expired(&t, policy, now), "{policy:?}");
        }
        t.status = FileTranscriptionStatus::Done;
        assert!(audio_expired(
            &t,
            MeetingAudioRetention::DeleteAfterTranscription,
            0
        ));
        assert!(!audio_expired(&t, MeetingAudioRetention::Forever, now));

        t.completed_at = Some(now - 10 * DAY_MS);
        assert!(audio_expired(&t, MeetingAudioRetention::Days7, now));
        assert!(!audio_expired(&t, MeetingAudioRetention::Days30, now));
        t.status = FileTranscriptionStatus::Failed;
        assert!(audio_expired(&t, MeetingAudioRetention::Days7, now));
        assert!(!audio_expired(
            &t,
            MeetingAudioRetention::DeleteAfterTranscription,
            now
        ));
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/audio")
            .join(name)
    }

    #[test]
    fn probe_explains_refusals() {
        let duration = probe(&fixture("tone.flac")).unwrap();
        assert_eq!(duration, Some(2_000));
        let code = |path: &Path| probe(path).unwrap_err().0;
        assert_eq!(
            code(&fixture("README.md")),
            FileTranscriptionErrorCode::UnsupportedFormat
        );
        assert_eq!(
            code(&fixture("absent.mp3")),
            FileTranscriptionErrorCode::FileMissing
        );
        assert_eq!(
            code(&fixture("video-only.mp4")),
            FileTranscriptionErrorCode::NoAudioTrack
        );
        let dir = tempfile::tempdir().unwrap();
        let broken = dir.path().join("cassé.flac");
        fs::write(&broken, b"fLaC but then nothing sensible").unwrap();
        assert!(matches!(
            code(&broken),
            FileTranscriptionErrorCode::CorruptFile | FileTranscriptionErrorCode::UnsupportedFormat
        ));
    }
}
