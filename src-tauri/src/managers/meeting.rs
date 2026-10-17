//! Meeting lifecycle: start/stop/cancel a dual-track recording, persist it
//! (folder + manifest + `meetings.db`), queue it for transcription and make
//! sure no meeting is lost across crashes or a corrupted database.
//!
//! The capture itself lives in `crate::meeting::capture` (#3); queued
//! meetings (status `pending` or `recovered`) are transcribed by the
//! `MeetingTranscriber` (#5), which registers itself with
//! [`MeetingManager::set_transcription_listener`] and reads and updates the
//! meetings through this manager's [`MeetingStore`] implementation.

pub mod folder;
pub mod store;

use crate::managers::audio::AudioRecordingManager;
use crate::managers::transcription::meeting::{MeetingJob, MeetingStore};
use crate::meeting::capture::{
    self, CaptureConfig, CaptureError, CaptureSession, CaptureWarning, SystemCaptureMode,
    MIC_FILE_NAME, SYSTEM_FILE_NAME,
};
use crate::meeting::power::SleepGuard;
use crate::meeting::transcript::{render_markdown, MeetingInfo, TranscriptFile};
use crate::settings::{get_settings, MeetingAudioRetention};
use crate::tray_i18n::get_tray_translations;
use anyhow::{anyhow, bail, Result};
use chrono::{Local, Utc};
use folder::{Manifest, RecoveryReport};
use log::{debug, error, info, warn};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use store::{Meeting, MeetingDetail, MeetingSourceKind, MeetingStatus, PaginatedMeetings};
use tauri::{AppHandle, Manager};
use tauri_specta::Event;

pub type MeetingId = String;

const STATE_TICK: Duration = Duration::from_secs(1);
const RETENTION_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeetingSource {
    Manual,
    Detected { app_id: String, app_name: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingPhase {
    Idle,
    Recording,
    Stopping,
}

/// Emitted on every state change and every second while recording (timer
/// and level meters).
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct MeetingStateChanged {
    pub state: MeetingPhase,
    pub meeting_id: Option<String>,
    pub elapsed_ms: u64,
    pub warnings: Vec<CaptureWarning>,
    pub mic_level: f32,
    pub system_level: f32,
}

impl MeetingStateChanged {
    fn idle() -> Self {
        Self {
            state: MeetingPhase::Idle,
            meeting_id: None,
            elapsed_ms: 0,
            warnings: Vec::new(),
            mic_level: 0.0,
            system_level: 0.0,
        }
    }
}

/// Same model as `HistoryUpdatePayload`.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(tag = "action")]
pub enum MeetingsUpdated {
    #[serde(rename = "added")]
    Added { meeting: Meeting },
    #[serde(rename = "updated")]
    Updated { meeting: Meeting },
    #[serde(rename = "deleted")]
    Deleted { id: String },
}

/// Emitted when meetings were recovered after a crash or rebuilt from their
/// folders. Also available through `take_meeting_recovery_notice` for the
/// startup case, where it fires before the UI listens.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct MeetingsRecovered {
    pub report: RecoveryReport,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct MeetingAudioPaths {
    pub mic: Option<String>,
    pub system: Option<String>,
}

struct ActiveMeeting {
    id: MeetingId,
    folder: PathBuf,
    started_at: i64,
    session: Box<dyn CaptureSession>,
    manifest: Manifest,
    // Released when the meeting ends.
    _sleep: SleepGuard,
}

enum State {
    Idle,
    Recording(Box<ActiveMeeting>),
    Stopping { id: MeetingId, started_at: i64 },
}

type TranscriptionListener = Arc<dyn Fn(&str) + Send + Sync>;

pub struct MeetingManager {
    app_handle: AppHandle,
    meetings_dir: PathBuf,
    db_path: PathBuf,
    state: Mutex<State>,
    // Lock-free mirrors of `state`, read from the dictation and feedback-sound
    // paths.
    recording: AtomicBool,
    suppress_feedback: AtomicBool,
    recovery_notice: Mutex<Option<RecoveryReport>>,
    transcription_listener: Mutex<Option<TranscriptionListener>>,
}

/// Whether sounds played by this app end up in the system track (the
/// process loopback excludes our own process tree; the other modes do not).
fn captures_own_sounds(mode: SystemCaptureMode) -> bool {
    !matches!(mode, SystemCaptureMode::ProcessLoopback)
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

impl MeetingManager {
    pub fn new(app_handle: &AppHandle) -> Result<Self> {
        let app_data_dir = crate::portable::app_data_dir(app_handle)?;
        let meetings_dir = app_data_dir.join("meetings");
        fs::create_dir_all(&meetings_dir)?;

        let manager = Self {
            app_handle: app_handle.clone(),
            meetings_dir,
            db_path: app_data_dir.join("meetings.db"),
            state: Mutex::new(State::Idle),
            recording: AtomicBool::new(false),
            suppress_feedback: AtomicBool::new(false),
            recovery_notice: Mutex::new(None),
            transcription_listener: Mutex::new(None),
        };

        let mut conn = manager.init_database()?;
        match folder::self_heal(&manager.meetings_dir, &mut conn) {
            Ok(report) => {
                if report != RecoveryReport::default() {
                    info!("Meeting self-heal: {:?}", report);
                }
                if report.should_notify() {
                    *manager.recovery_notice.lock().unwrap() = Some(report.clone());
                    let _ = MeetingsRecovered { report }.emit(app_handle);
                }
            }
            Err(e) => error!("Meeting self-heal failed: {e}"),
        }

        Ok(manager)
    }

    /// Opens and migrates `meetings.db`. A database that cannot be opened is
    /// moved aside and recreated: `self_heal` then rebuilds it from the
    /// meeting folders.
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
                    "meetings.db unusable ({e}); moving it to {} and rebuilding",
                    aside.display()
                );
                fs::rename(&self.db_path, &aside)?;
                open(&self.db_path)
            }
        }
    }

    fn conn(&self) -> Result<Connection> {
        store::open(&self.db_path)
    }

    /* ---------- state ------------------------------------------------------ */

    /// True while a meeting is being recorded (including while stopping).
    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::SeqCst)
    }

    /// True when dictation feedback sounds would be captured into the system
    /// track (endpoint loopback mode), so they must not be played.
    pub fn suppress_feedback_sounds(&self) -> bool {
        self.suppress_feedback.load(Ordering::SeqCst)
    }

    pub fn state(&self) -> MeetingStateChanged {
        match &*self.state.lock().unwrap() {
            State::Idle => MeetingStateChanged::idle(),
            State::Recording(active) => {
                let (mic_level, system_level) = active.session.levels();
                MeetingStateChanged {
                    state: MeetingPhase::Recording,
                    meeting_id: Some(active.id.clone()),
                    elapsed_ms: active.session.elapsed_ms(),
                    warnings: active.session.warnings(),
                    mic_level,
                    system_level,
                }
            }
            State::Stopping { id, started_at } => MeetingStateChanged {
                state: MeetingPhase::Stopping,
                meeting_id: Some(id.clone()),
                elapsed_ms: (now_ms() - started_at).max(0) as u64,
                warnings: Vec::new(),
                mic_level: 0.0,
                system_level: 0.0,
            },
        }
    }

    fn emit_state(&self) {
        if let Err(e) = self.state().emit(&self.app_handle) {
            error!("Failed to emit meeting state: {e}");
        }
    }

    fn emit_updated(&self, event: MeetingsUpdated) {
        if let Err(e) = event.emit(&self.app_handle) {
            error!("Failed to emit meetings-updated event: {e}");
        }
    }

    fn emit_meeting(&self, id: &str, added: bool) {
        match self.conn().and_then(|c| store::get(&c, id)) {
            Ok(Some(meeting)) if added => self.emit_updated(MeetingsUpdated::Added { meeting }),
            Ok(Some(meeting)) => self.emit_updated(MeetingsUpdated::Updated { meeting }),
            Ok(None) => {}
            Err(e) => error!("Failed to load meeting {id}: {e}"),
        }
    }

    /* ---------- lifecycle -------------------------------------------------- */

    fn default_title(&self, source_app: Option<&str>) -> String {
        let locale = get_settings(&self.app_handle).app_language;
        let strings = get_tray_translations(Some(locale));
        let english = get_tray_translations(Some("en".to_string()));
        let pick = |s: String, fallback: String| if s.is_empty() { fallback } else { s };
        let date = Local::now().format("%Y-%m-%d %H:%M").to_string();
        match source_app {
            Some(app) => pick(
                strings.meeting_title_detected,
                english.meeting_title_detected,
            )
            .replace("{{app}}", app)
            .replace("{{date}}", &date),
            None => pick(strings.meeting_title_manual, english.meeting_title_manual)
                .replace("{{date}}", &date),
        }
    }

    pub fn start(&self, source: MeetingSource) -> Result<MeetingId> {
        if !capture::capture_supported() {
            bail!("{}", CaptureError::Unsupported);
        }
        let mut state = self.state.lock().unwrap();
        if !matches!(*state, State::Idle) {
            bail!("A meeting is already being recorded");
        }

        let (kind, source_app) = match source {
            MeetingSource::Manual => (MeetingSourceKind::Manual, None),
            MeetingSource::Detected { app_name, .. } => {
                (MeetingSourceKind::Detected, Some(app_name))
            }
        };
        let started_at = now_ms();
        let title = self.default_title(source_app.as_deref());
        let date_prefix = Local::now().format("%Y-%m-%d %H%M").to_string();
        let folder_name = folder::unique_folder_name(&self.meetings_dir, &date_prefix, &title);
        let folder_path = self.meetings_dir.join(&folder_name);
        fs::create_dir_all(&folder_path)?;

        let capture_system = capture::system_capture_supported();
        let mut meeting = Meeting {
            id: uuid::Uuid::new_v4().to_string(),
            title,
            folder: folder_name,
            started_at,
            ended_at: None,
            duration_ms: None,
            source: kind,
            source_app,
            has_system_audio: capture_system,
            has_audio: true,
            status: MeetingStatus::Recording,
            error: None,
            model_id: None,
            created_at: started_at,
        };

        // Manifest first: it is what crash recovery relies on.
        let mut manifest = Manifest::for_meeting(&meeting, capture_system);
        folder::write_manifest(&folder_path, &manifest)?;

        let session = match capture::start_capture(CaptureConfig {
            mic_device: self
                .app_handle
                .try_state::<Arc<AudioRecordingManager>>()
                .and_then(|rm| rm.preferred_microphone_name()),
            capture_system,
            out_dir: folder_path.clone(),
        }) {
            Ok(session) => session,
            Err(e) => {
                let _ = fs::remove_dir_all(&folder_path);
                return Err(anyhow!("Failed to start meeting capture: {e}"));
            }
        };

        let system_mode = session.system_mode();
        if system_mode.is_none() && capture_system {
            manifest.tracks.system = None;
            meeting.has_system_audio = false;
        }
        manifest.system_mode = system_mode;
        if let Err(e) = folder::write_manifest(&folder_path, &manifest) {
            warn!("Failed to update meeting manifest: {e}");
        }

        // A database failure must not stop the recording: the manifest lets
        // `self_heal` recreate the row on next launch.
        if let Err(e) = self.conn().and_then(|c| store::insert(&c, &meeting)) {
            error!("Failed to insert meeting {} in database: {e}", meeting.id);
        }

        self.recording.store(true, Ordering::SeqCst);
        self.suppress_feedback.store(
            system_mode.is_some_and(captures_own_sounds),
            Ordering::SeqCst,
        );
        // A dictation already in progress may have muted the output; the
        // meeting needs to hear (and record) it.
        if let Some(rm) = self.app_handle.try_state::<Arc<AudioRecordingManager>>() {
            rm.remove_mute();
        }

        let id = meeting.id.clone();
        *state = State::Recording(Box::new(ActiveMeeting {
            id: id.clone(),
            folder: folder_path,
            started_at,
            session,
            manifest,
            _sleep: SleepGuard::acquire("Trigo Whispr is recording a meeting"),
        }));
        drop(state);

        info!("Meeting {id} started");
        self.emit_updated(MeetingsUpdated::Added { meeting });
        self.emit_state();
        self.spawn_state_ticker(id.clone());
        Ok(id)
    }

    fn spawn_state_ticker(&self, id: MeetingId) {
        let app = self.app_handle.clone();
        let spawned = std::thread::Builder::new()
            .name("meeting-state-ticker".into())
            .spawn(move || loop {
                std::thread::sleep(STATE_TICK);
                let Some(manager) = app.try_state::<Arc<MeetingManager>>() else {
                    return;
                };
                let snapshot = manager.state();
                if snapshot.state != MeetingPhase::Recording
                    || snapshot.meeting_id.as_deref() != Some(id.as_str())
                {
                    return;
                }
                let _ = snapshot.emit(&app);
            });
        if let Err(e) = spawned {
            warn!("Failed to spawn meeting state ticker: {e}");
        }
    }

    fn take_active(&self) -> Result<Box<ActiveMeeting>> {
        let mut state = self.state.lock().unwrap();
        match std::mem::replace(&mut *state, State::Idle) {
            State::Recording(active) => {
                *state = State::Stopping {
                    id: active.id.clone(),
                    started_at: active.started_at,
                };
                Ok(active)
            }
            other => {
                *state = other;
                bail!("No meeting is being recorded")
            }
        }
    }

    fn end_active(&self) {
        *self.state.lock().unwrap() = State::Idle;
        self.recording.store(false, Ordering::SeqCst);
        self.suppress_feedback.store(false, Ordering::SeqCst);
        self.emit_state();
    }

    /// Stops the capture, finalizes the files and queues the meeting for
    /// transcription. Blocking.
    pub fn stop(&self) -> Result<MeetingId> {
        let active = self.take_active()?;
        self.emit_state();
        let ActiveMeeting {
            id,
            folder: folder_path,
            started_at,
            session,
            mut manifest,
            _sleep,
            ..
        } = *active;

        let result = self.finalize(&id, &folder_path, started_at, session, &mut manifest);
        drop(_sleep);
        self.end_active();

        match result {
            Ok(()) => info!("Meeting {id} stopped"),
            Err(e) => error!("Meeting {id} could not be finalized cleanly: {e}"),
        }
        self.emit_meeting(&id, false);
        self.notify_transcription(&id);
        Ok(id)
    }

    fn finalize(
        &self,
        id: &str,
        folder_path: &Path,
        started_at: i64,
        session: Box<dyn CaptureSession>,
        manifest: &mut Manifest,
    ) -> Result<()> {
        let conn = self.conn()?;
        let output = match session.stop() {
            Ok(output) => output,
            Err(e) => {
                // The streams died but the WAVs on disk are still usable.
                warn!("Capture stop failed for meeting {id}: {e}; recovering from disk");
                if let Some(m) = store::get(&conn, id)? {
                    folder::recover_interrupted(&self.meetings_dir, &conn, &m)?;
                }
                return Ok(());
            }
        };

        let ended_at = now_ms();
        let duration_ms = if output.duration_ms > 0 {
            output.duration_ms as i64
        } else {
            ended_at - started_at
        };
        let has_system = output.system_path.as_deref().is_some_and(Path::exists);

        // Manifest before the database, so a crash in between is recoverable.
        manifest.ended_at = Some(ended_at);
        manifest.duration_ms = Some(duration_ms);
        manifest.system_mode = output.system_mode.or(manifest.system_mode);
        if !has_system {
            manifest.tracks.system = None;
        }
        folder::write_manifest(folder_path, manifest)?;

        store::finish(
            &conn,
            id,
            ended_at,
            duration_ms,
            has_system,
            MeetingStatus::Pending,
        )?;
        Ok(())
    }

    /// Stops the recording and deletes its files and row. The UI asks for
    /// confirmation first.
    pub fn cancel(&self) -> Result<()> {
        let active = self.take_active()?;
        self.emit_state();
        let ActiveMeeting {
            id,
            folder: folder_path,
            session,
            ..
        } = *active;
        if let Err(e) = session.stop() {
            warn!("Capture stop failed while cancelling meeting {id}: {e}");
        }
        let result = self.conn().and_then(|c| {
            folder::discard_cancelled(&c, &id, || folder::remove_folder(&folder_path))
        });
        // The capture is stopped either way: free the state.
        self.end_active();
        match result {
            Ok(()) => {
                info!("Meeting {id} cancelled");
                self.emit_updated(MeetingsUpdated::Deleted { id });
                Ok(())
            }
            Err(e) => {
                error!("Cancelling meeting {id} left files behind: {e}");
                self.emit_meeting(&id, false);
                Err(e)
            }
        }
    }

    fn active_id(&self) -> Option<MeetingId> {
        match &*self.state.lock().unwrap() {
            State::Idle => None,
            State::Recording(active) => Some(active.id.clone()),
            State::Stopping { id, .. } => Some(id.clone()),
        }
    }

    /* ---------- queries & edits -------------------------------------------- */

    pub fn list(&self, offset: usize, limit: usize) -> Result<PaginatedMeetings> {
        store::list(&self.conn()?, offset, limit)
    }

    pub fn search(&self, query: &str, offset: usize, limit: usize) -> Result<PaginatedMeetings> {
        store::search(&self.conn()?, query, offset, limit)
    }

    /// Absolute path of the meeting's folder.
    pub fn folder_path(&self, id: &str) -> Result<PathBuf> {
        let meeting =
            store::get(&self.conn()?, id)?.ok_or_else(|| anyhow!("Meeting {id} not found"))?;
        Ok(self.meetings_dir.join(&meeting.folder))
    }

    pub fn get(&self, id: &str) -> Result<Option<MeetingDetail>> {
        let conn = self.conn()?;
        let Some(meeting) = store::get(&conn, id)? else {
            return Ok(None);
        };
        let segments = store::segments(&conn, id)?;
        Ok(Some(MeetingDetail { meeting, segments }))
    }

    /// The transcript rendered as `transcript.md`.
    pub fn markdown(&self, id: &str) -> Result<String> {
        let conn = self.conn()?;
        let m = store::get(&conn, id)?.ok_or_else(|| anyhow!("Meeting {id} not found"))?;
        let segments = store::segments(&conn, id)?;
        Ok(render_markdown(&meeting_info(m), &segments))
    }

    pub fn rename(&self, id: &str, title: &str) -> Result<()> {
        let title = title.trim();
        if title.is_empty() {
            bail!("Title cannot be empty");
        }
        let conn = self.conn()?;
        let meeting = store::get(&conn, id)?.ok_or_else(|| anyhow!("Meeting {id} not found"))?;
        store::rename(&conn, id, title)?;

        // Keep the manifest in sync so a rebuilt database keeps the new
        // title. The folder itself is not renamed.
        let folder_path = self.meetings_dir.join(&meeting.folder);
        let mut state = self.state.lock().unwrap();
        match &mut *state {
            State::Recording(active) if active.id == id => {
                active.manifest.title = title.to_string();
                folder::write_manifest(&folder_path, &active.manifest)?;
            }
            _ => {
                if let Ok(mut manifest) = folder::read_manifest(&folder_path) {
                    manifest.title = title.to_string();
                    folder::write_manifest(&folder_path, &manifest)?;
                }
            }
        }
        drop(state);
        self.emit_meeting(id, false);
        Ok(())
    }

    /// Deletes the meeting and its folder.
    pub fn delete(&self, id: &str) -> Result<()> {
        if self.active_id().as_deref() == Some(id) {
            bail!("Cannot delete a meeting while it is being recorded");
        }
        let conn = self.conn()?;
        let meeting = store::get(&conn, id)?.ok_or_else(|| anyhow!("Meeting {id} not found"))?;
        let folder_path = self.meetings_dir.join(&meeting.folder);
        match fs::remove_dir_all(&folder_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        store::delete(&conn, id)?;
        self.emit_updated(MeetingsUpdated::Deleted { id: id.to_string() });
        Ok(())
    }

    pub fn audio_paths(&self, id: &str) -> Result<MeetingAudioPaths> {
        let meeting =
            store::get(&self.conn()?, id)?.ok_or_else(|| anyhow!("Meeting {id} not found"))?;
        let folder_path = self.meetings_dir.join(&meeting.folder);
        let existing = |name: &str| {
            let path = folder_path.join(name);
            path.exists().then(|| path.to_string_lossy().into_owned())
        };
        Ok(MeetingAudioPaths {
            mic: existing(MIC_FILE_NAME),
            system: existing(SYSTEM_FILE_NAME),
        })
    }

    /* ---------- transcription queue (#5) ----------------------------------- */

    /// Meetings waiting for transcription, oldest first.
    pub fn queued_meetings(&self) -> Result<Vec<Meeting>> {
        store::list_by_status(
            &self.conn()?,
            &[MeetingStatus::Pending, MeetingStatus::Recovered],
        )
    }

    /// Called with the meeting id whenever a meeting is queued. Meetings
    /// queued before registration (e.g. recovered at startup) are listed by
    /// [`Self::queued_meetings`].
    pub fn set_transcription_listener(&self, listener: impl Fn(&str) + Send + Sync + 'static) {
        *self.transcription_listener.lock().unwrap() = Some(Arc::new(listener));
    }

    fn notify_transcription(&self, id: &str) {
        let listener = self.transcription_listener.lock().unwrap().clone();
        match listener {
            Some(listener) => listener(id),
            None => debug!("Meeting {id} queued for transcription (no transcriber registered)"),
        }
    }

    pub fn retry_transcription(&self, id: &str) -> Result<()> {
        let conn = self.conn()?;
        let meeting = store::get(&conn, id)?.ok_or_else(|| anyhow!("Meeting {id} not found"))?;
        match meeting.status {
            MeetingStatus::Recording | MeetingStatus::Transcribing => {
                bail!("Meeting {id} is {}", meeting.status.as_str())
            }
            _ if !meeting.has_audio => bail!("The audio of meeting {id} was deleted"),
            _ => {}
        }
        store::set_status(&conn, id, MeetingStatus::Pending, None)?;
        self.emit_meeting(id, false);
        self.notify_transcription(id);
        Ok(())
    }

    /* ---------- recovery & retention --------------------------------------- */

    /// Returns (once) the report of meetings recovered at startup.
    pub fn take_recovery_notice(&self) -> Option<RecoveryReport> {
        self.recovery_notice.lock().unwrap().take()
    }

    pub fn apply_retention(&self) -> Result<()> {
        let policy: MeetingAudioRetention = get_settings(&self.app_handle).meeting_audio_retention;
        let removed = folder::apply_retention(&self.meetings_dir, &self.conn()?, policy, now_ms())?;
        if !removed.is_empty() {
            info!(
                "Meeting retention removed the audio of {} meeting(s)",
                removed.len()
            );
        }
        for id in removed {
            self.emit_meeting(&id, false);
        }
        Ok(())
    }

    /// Applies the retention policy now and then every 24 h.
    pub fn start_retention_timer(&self) {
        let app = self.app_handle.clone();
        let spawned = std::thread::Builder::new()
            .name("meeting-retention".into())
            .spawn(move || loop {
                if let Some(manager) = app.try_state::<Arc<MeetingManager>>() {
                    if let Err(e) = manager.apply_retention() {
                        error!("Meeting retention failed: {e}");
                    }
                }
                std::thread::sleep(RETENTION_INTERVAL);
            });
        if let Err(e) = spawned {
            warn!("Failed to spawn meeting retention timer: {e}");
        }
    }
}

impl MeetingStore for MeetingManager {
    fn job(&self, meeting_id: &str) -> Result<MeetingJob> {
        let m = store::get(&self.conn()?, meeting_id)?
            .ok_or_else(|| anyhow!("Meeting {meeting_id} not found"))?;
        if !m.has_audio {
            bail!("The audio of meeting {meeting_id} was deleted");
        }
        Ok(MeetingJob {
            dir: self.meetings_dir.join(&m.folder),
            meeting: meeting_info(m),
        })
    }

    fn set_status(&self, meeting_id: &str, status: MeetingStatus, error: Option<&str>) {
        if let Err(e) = self
            .conn()
            .and_then(|c| store::set_status(&c, meeting_id, status, error))
        {
            error!(
                "Failed to set meeting {meeting_id} to {}: {e}",
                status.as_str()
            );
            return;
        }
        self.emit_meeting(meeting_id, false);
        // `delete_after_transcription` applies as soon as the transcript is in.
        if status == MeetingStatus::Done {
            if let Err(e) = self.apply_retention() {
                error!("Meeting retention failed: {e}");
            }
        }
    }

    fn save_transcript(&self, meeting_id: &str, transcript: &TranscriptFile) -> Result<()> {
        let mut conn = self.conn()?;
        store::replace_segments(&mut conn, meeting_id, &transcript.segments)?;
        store::set_model_id(&conn, meeting_id, &transcript.model_id)
    }
}

fn meeting_info(m: Meeting) -> MeetingInfo {
    MeetingInfo {
        id: m.id,
        title: m.title,
        started_at: m.started_at,
        duration_ms: m.duration_ms.unwrap_or(0).max(0) as u64,
        source_app: m.source_app,
    }
}
