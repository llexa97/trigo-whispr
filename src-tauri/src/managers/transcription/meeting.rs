//! Meeting transcription (#5): finished meetings wait in a FIFO and are
//! decoded in the background, one at a time, track by track and chunk by
//! chunk. Dictation comes first: before each chunk the meeting waits for any
//! dictation to finish, so none ever waits longer than one chunk.
//!
//! Where meetings and their status are stored is the `MeetingManager`'s
//! business (#4); it implements [`MeetingStore`] and feeds the queue.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use transcribe_cpp::Transcript;

use super::queue::{JobQueue, RunningJob};
use super::{
    effective_language_for_model, post_process_transcription_text, run_setup, ModelUsage, RunSetup,
    TranscriptionManager,
};
use crate::audio_toolkit::chunking::Chunker;
use crate::engine_supervisor::{CancelMark, EngineError, LoadedInfo};
use crate::managers::audio::{create_detector, AudioRecordingManager};
use crate::managers::meeting::store::MeetingStatus;
use crate::meeting::capture::{MIC_FILE_NAME, SYSTEM_FILE_NAME};
use crate::meeting::crosstalk::CrosstalkOptions;
use crate::meeting::track::{decode_track, track_samples, SpeechGate, TrackHooks};
use crate::meeting::transcript::{
    merge_tracks, renumber, write_transcript_files, MeetingInfo, MeetingSegment, TrackTranscript,
    TranscriptFile,
};
use crate::settings::{get_settings, AppSettings};

/// How often a meeting waiting on a dictation checks again.
const DICTATION_POLL: Duration = Duration::from_millis(50);
/// Cancelling a dictation stops the engine's work in progress, a meeting's
/// chunk included. Such a chunk is decoded again, up to this many times.
const MAX_CHUNK_ATTEMPTS: usize = 3;

/// A finished meeting to transcribe: what the transcript says about it, and
/// the folder holding its tracks ([`MIC_FILE_NAME`], [`SYSTEM_FILE_NAME`] if
/// system audio was captured) where the transcript files are written.
#[derive(Clone, Debug)]
pub struct MeetingJob {
    pub meeting: MeetingInfo,
    pub dir: PathBuf,
}

/// The meetings' storage, as the transcription queue reads and updates it.
pub trait MeetingStore: Send + Sync {
    /// The meeting to transcribe, as it is now (its title may have changed
    /// while it waited).
    fn job(&self, meeting_id: &str) -> Result<MeetingJob>;
    /// Records a status change; `error` is set for [`MeetingStatus::Failed`].
    fn set_status(&self, meeting_id: &str, status: MeetingStatus, error: Option<&str>);
    /// Saves a finished transcript, before its status turns `Done`.
    fn save_transcript(&self, meeting_id: &str, transcript: &TranscriptFile) -> Result<()>;
}

/// Emitted after each chunk of a meeting's transcription.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct MeetingTranscriptionProgress {
    pub meeting_id: String,
    /// 0 to 1.
    pub progress: f64,
    /// Estimated time left, once a chunk is done.
    pub eta_ms: Option<u64>,
}

/// A transcription outside the meeting queue (a dictation's, a history
/// entry's, a CLI file's), from its start to its end.
pub(super) struct ForegroundRun<'a>(&'a TranscriptionManager);

impl<'a> ForegroundRun<'a> {
    pub(super) fn new(tm: &'a TranscriptionManager) -> Self {
        tm.lock_users().add_foreground();
        Self(tm)
    }
}

impl Drop for ForegroundRun<'_> {
    fn drop(&mut self) {
        self.0
            .update_users("transcription", |users| users.remove_foreground());
    }
}

/// Keeps the model loaded while a meeting (or an imported file) is
/// transcribed; afterwards it is unloaded per the usual rules, once no
/// dictation uses it either.
pub(super) struct BackgroundActive<'a>(&'a TranscriptionManager);

impl<'a> BackgroundActive<'a> {
    pub(super) fn new(tm: &'a TranscriptionManager) -> Self {
        tm.lock_users().add_background();
        Self(tm)
    }
}

impl Drop for BackgroundActive<'_> {
    fn drop(&mut self) {
        self.0.touch_activity();
        self.0.update_users("background transcription", |users| {
            users.remove_background()
        });
    }
}

impl TranscriptionManager {
    /// Whether a dictation is recording or being transcribed: a meeting's
    /// transcription gives way to it.
    pub fn dictation_busy(&self) -> bool {
        self.lock_users().dictating()
            || self.is_streaming()
            || self
                .app_handle
                .try_state::<Arc<AudioRecordingManager>>()
                .is_some_and(|audio| audio.is_recording())
    }

    /// Waits until no dictation uses the engine, or fails if `cancelled`.
    pub(super) fn wait_for_dictation(&self, cancelled: &AtomicBool) -> Result<()> {
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(EngineError::Cancelled.into());
            }
            if !self.dictation_busy() {
                return Ok(());
            }
            thread::sleep(DICTATION_POLL);
        }
    }

    /// Loads the long transcriptions' model for a meeting or an imported
    /// file, once no dictation needs the engine: its model and the loaded
    /// engine.
    pub(super) fn load_long_form_model(
        &self,
        cancelled: &AtomicBool,
    ) -> Result<(String, LoadedInfo)> {
        let model_id = self.model_for(ModelUsage::LongForm);
        loop {
            self.wait_for_dictation(cancelled)?;
            self.ensure_model_loaded(&model_id, ModelUsage::LongForm)?;
            // A dictation that started meanwhile may have replaced it.
            if !self.dictation_busy() && self.has_loaded(&model_id) {
                break;
            }
        }
        let info = self
            .engine
            .loaded()
            .ok_or_else(|| anyhow!("Model is not loaded for transcription."))?;
        Ok((model_id, info))
    }

    /// Transcribes one meeting with the long transcriptions' model and the
    /// selected language, never
    /// translated. `on_progress` gets the samples done so far, out of
    /// `total`, after each chunk.
    fn transcribe_meeting(
        &self,
        job: &MeetingJob,
        cancelled: &AtomicBool,
        on_progress: &mut dyn FnMut(u64, u64),
    ) -> Result<TranscriptFile> {
        let mic = Some(job.dir.join(MIC_FILE_NAME)).filter(|p| p.exists());
        let system = Some(job.dir.join(SYSTEM_FILE_NAME)).filter(|p| p.exists());
        if mic.is_none() && system.is_none() {
            bail!("No audio track in {}", job.dir.display());
        }
        let mut total = 0;
        for path in mic.iter().chain(system.iter()) {
            total += track_samples(path)?;
        }

        let _active = BackgroundActive::new(self);
        let (model_id, info) = self.load_long_form_model(cancelled)?;
        let mut settings = get_settings(&self.app_handle);
        settings.translate_to_english = false;
        let language =
            effective_language_for_model(&settings, self.model_manager.as_ref(), &model_id);
        let setup = run_setup(&info, &settings, &language);
        info!(
            "Transcribing meeting '{}' with '{}' ({:.0}s of audio)",
            job.meeting.id,
            model_id,
            total as f64 / 16_000.0
        );

        let mut run = BackgroundRun {
            tm: self,
            setup: &setup,
            model_id: &model_id,
            cancelled,
            mark: self.engine.cancel_mark(),
            preemptions: self.preemption_count(),
            done: 0,
            on_progress: &mut |done| on_progress(done, total),
        };
        let mic = mic.map(|path| run.track(&path, &settings)).transpose()?;
        let system = system.map(|path| run.track(&path, &settings)).transpose()?;

        let merged = merge_tracks(mic.as_ref(), system.as_ref(), &CrosstalkOptions::default());
        let detected = [&mic, &system]
            .into_iter()
            .flatten()
            .find_map(|track| track.language.clone());
        let segments = post_process_segments(merged, &setup, &settings, detected.clone());
        Ok(TranscriptFile {
            version: TranscriptFile::VERSION,
            meeting: job.meeting.clone(),
            model_id,
            language: detected.or_else(|| setup.applied_language_hint.clone()),
            segments,
        })
    }
}

/// The dictation's text clean-up, segment by segment (no LLM post-processing).
fn post_process_segments(
    segments: Vec<MeetingSegment>,
    setup: &RunSetup,
    settings: &AppSettings,
    detected_language: Option<String>,
) -> Vec<MeetingSegment> {
    let output_language = setup.output_language(settings, detected_language);
    let segments = segments
        .into_iter()
        .map(|mut segment| {
            segment.text = post_process_transcription_text(
                segment.text,
                settings,
                setup.custom_words_prompted(),
                &output_language,
                &setup.languages,
            );
            segment
        })
        .collect();
    renumber(segments)
}

/// One meeting's (or imported file's) decode on the engine.
pub(super) struct BackgroundRun<'a> {
    pub tm: &'a TranscriptionManager,
    pub setup: &'a RunSetup,
    pub model_id: &'a str,
    pub cancelled: &'a AtomicBool,
    /// Engine cancels after this stop the chunk being decoded.
    pub mark: CancelMark,
    /// [`TranscriptionManager::preemption_count`] when `mark` was taken.
    pub preemptions: u64,
    /// Samples done, over every track.
    pub done: u64,
    pub on_progress: &'a mut dyn FnMut(u64),
}

impl BackgroundRun<'_> {
    pub fn track(&mut self, path: &Path, settings: &AppSettings) -> Result<TrackTranscript> {
        let backend = settings.vad_backend;
        let app = self.tm.app_handle.clone();
        // Long-form chunking regardless of the model or setting: a meeting is
        // too long for one run, and chunks are where dictation gets its turn.
        let chunker = Chunker::new(Box::new(move || create_detector(backend, &app)));
        let gate = SpeechGate::new(create_detector(backend, &self.tm.app_handle));
        decode_track(path, chunker, gate, self)
    }
}

impl TrackHooks for BackgroundRun<'_> {
    fn check_cancelled(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(EngineError::Cancelled.into());
        }
        Ok(())
    }

    /// Waits for any dictation, then reloads this run's model if a
    /// dictation replaced it with its own.
    fn before_chunk(&mut self) -> Result<()> {
        loop {
            self.tm.wait_for_dictation(self.cancelled)?;
            // Taken before the model check: a dictation that loads its model
            // after it also cancels this chunk, so the chunk is never decoded
            // with the dictation's model.
            let preemptions = self.tm.preemption_count();
            let mark = self.tm.engine.cancel_mark();
            if self.tm.has_loaded(self.model_id) {
                self.preemptions = preemptions;
                self.mark = mark;
                return Ok(());
            }
            info!(
                "Reloading '{}' to resume the background transcription",
                self.model_id
            );
            self.tm
                .ensure_model_loaded(self.model_id, ModelUsage::LongForm)?;
        }
    }

    fn decode(&mut self, pcm: Vec<f32>) -> Result<Transcript> {
        let mut attempt = 1;
        loop {
            let result = self.tm.run_batch(
                self.mark,
                pcm.clone(),
                &self.setup.run_options,
                self.model_id,
            );
            match result {
                // A cancel that could not stop the engine (a dictation was
                // using it) lands here, once the chunk is decoded.
                Ok(transcript) => {
                    self.check_cancelled()?;
                    return Ok(transcript);
                }
                Err(EngineError::Cancelled)
                    if !self.cancelled.load(Ordering::Acquire)
                        && self.tm.preemption_count() != self.preemptions =>
                {
                    // A dictation took the engine for its own model: decode
                    // the chunk again once it is done, however often that
                    // happens.
                    info!("Background chunk interrupted by a dictation; decoding it again");
                    self.before_chunk()?;
                }
                Err(EngineError::Cancelled)
                    if !self.cancelled.load(Ordering::Acquire) && attempt < MAX_CHUNK_ATTEMPTS =>
                {
                    // Someone else's cancel (a dictation's): decode the chunk
                    // again once the engine is free.
                    info!("Background chunk stopped by another cancel; decoding it again");
                    attempt += 1;
                    self.before_chunk()?;
                }
                Err(EngineError::Cancelled) => return Err(EngineError::Cancelled.into()),
                Err(e) => bail!("transcribe-cpp transcription failed: {e}"),
            }
        }
    }

    fn after_chunk(&mut self, samples: usize) {
        self.done += samples as u64;
        (self.on_progress)(self.done);
    }
}

/// The queue of meetings to transcribe, and its worker thread.
#[derive(Clone)]
pub struct MeetingTranscriber {
    queue: JobQueue,
}

impl MeetingTranscriber {
    pub fn new(app: &AppHandle, store: Arc<dyn MeetingStore>) -> Self {
        let worker = Worker {
            app: app.clone(),
            store,
        };
        let queue = JobQueue::spawn(app, "meeting-transcriber", move |job| {
            match worker.store.job(job.id) {
                Ok(meeting) => worker.transcribe(&meeting, job),
                Err(e) => warn!("Meeting '{}' can no longer be transcribed: {e:#}", job.id),
            }
        });
        Self { queue }
    }

    /// Queues a meeting, unless it is already queued or being transcribed.
    /// Its status is the store's: `pending` or `recovered` until it starts.
    pub fn enqueue(&self, meeting_id: &str) {
        self.queue.enqueue(meeting_id);
    }

    /// Stops a meeting's transcription, or takes it out of the queue; either
    /// way it is left `pending`, its tracks kept, to be retried. Returns
    /// false if it was not queued, or its result is already being saved.
    pub fn cancel(&self, meeting_id: &str) -> bool {
        self.queue.cancel(meeting_id)
    }
}

/// What the worker thread needs to transcribe a meeting.
struct Worker {
    app: AppHandle,
    store: Arc<dyn MeetingStore>,
}

impl Worker {
    fn transcribe(&self, job: &MeetingJob, running: &RunningJob<'_>) {
        let id = job.meeting.id.as_str();
        let cancelled = running.cancelled;
        let Some(tm) = self.app.try_state::<Arc<TranscriptionManager>>() else {
            error!("No transcription manager for meeting '{id}'");
            return;
        };
        self.store.set_status(id, MeetingStatus::Transcribing, None);
        self.progress(id, 0.0, None);

        let started = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tm.transcribe_meeting(job, cancelled, &mut |done, total| {
                let (progress, eta_ms) = progress_and_eta(started, done, total);
                self.progress(id, progress, eta_ms);
            })
        }))
        .unwrap_or_else(|panic| {
            Err(anyhow!(
                "meeting transcription panicked: {}",
                super::panic_payload_message(panic.as_ref())
            ))
        })
        .and_then(|file| {
            // Nothing is saved for a meeting cancelled before this point,
            // and no cancel is accepted after it.
            if !running.commit() {
                return Err(EngineError::Cancelled.into());
            }
            write_transcript_files(&job.dir, &file)?;
            self.store.save_transcript(id, &file)?;
            Ok(file)
        });

        match result {
            Ok(file) => {
                info!(
                    "Meeting '{id}' transcribed in {:.1}s: {} segments",
                    started.elapsed().as_secs_f64(),
                    file.segments.len()
                );
                self.progress(id, 1.0, Some(0));
                self.store.set_status(id, MeetingStatus::Done, None);
            }
            Err(_) if running.is_cancelled() => {
                info!("Meeting '{id}' transcription cancelled");
                self.store.set_status(id, MeetingStatus::Pending, None);
            }
            Err(e) => {
                warn!("Meeting '{id}' transcription failed: {e:#}");
                self.store
                    .set_status(id, MeetingStatus::Failed, Some(&format!("{e:#}")));
            }
        }
    }

    fn progress(&self, meeting_id: &str, progress: f64, eta_ms: Option<u64>) {
        let _ = MeetingTranscriptionProgress {
            meeting_id: meeting_id.to_string(),
            progress: progress.clamp(0.0, 1.0),
            eta_ms,
        }
        .emit(&self.app);
    }
}

/// Progress (0 to 1) of `done` samples out of `total`, and the time left
/// estimated from the pace since `started`.
pub(super) fn progress_and_eta(started: Instant, done: u64, total: u64) -> (f64, Option<u64>) {
    let progress = if total == 0 {
        1.0
    } else {
        done as f64 / total as f64
    };
    let eta_ms = (done > 0).then(|| {
        let elapsed = started.elapsed().as_millis() as f64;
        (elapsed / done as f64 * total.saturating_sub(done) as f64) as u64
    });
    (progress, eta_ms)
}
