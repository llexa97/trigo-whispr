//! Meeting transcription (#5): finished meetings wait in a FIFO and are
//! decoded in the background, one at a time, track by track and chunk by
//! chunk. Dictation comes first: before each chunk the meeting waits for any
//! dictation to finish, so none ever waits longer than one chunk.
//!
//! Where meetings and their status are stored is the `MeetingManager`'s
//! business (#4); it implements [`MeetingStore`] and feeds the queue.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use transcribe_cpp::Transcript;

use super::{
    effective_language_for_model, post_process_transcription_text, run_setup, RunSetup,
    TranscriptionManager,
};
use crate::audio_toolkit::chunking::Chunker;
use crate::engine_supervisor::{CancelMark, EngineError};
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

/// Counts a dictation in flight, from its transcription's start to its end.
pub(super) struct ForegroundRun(Arc<AtomicUsize>);

impl ForegroundRun {
    pub(super) fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::AcqRel);
        Self(Arc::clone(count))
    }
}

impl Drop for ForegroundRun {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Keeps the model loaded while a meeting is transcribed; afterwards the
/// usual unload rules apply again.
struct MeetingActive<'a>(&'a TranscriptionManager);

impl<'a> MeetingActive<'a> {
    fn new(tm: &'a TranscriptionManager) -> Self {
        tm.meeting_active.store(true, Ordering::Release);
        Self(tm)
    }
}

impl Drop for MeetingActive<'_> {
    fn drop(&mut self) {
        self.0.meeting_active.store(false, Ordering::Release);
        self.0.touch_activity();
        self.0.maybe_unload_immediately("meeting transcription");
    }
}

impl TranscriptionManager {
    /// Whether a dictation is recording or being transcribed: a meeting's
    /// transcription gives way to it.
    pub fn dictation_busy(&self) -> bool {
        self.foreground_runs.load(Ordering::Acquire) > 0
            || self.is_streaming()
            || self
                .app_handle
                .try_state::<Arc<AudioRecordingManager>>()
                .is_some_and(|audio| audio.is_recording())
    }

    /// Whether a meeting is being transcribed.
    pub fn is_transcribing_meeting(&self) -> bool {
        self.meeting_active.load(Ordering::Acquire)
    }

    /// Loads the selected model if none is, waiting for a load under way.
    fn ensure_model_loaded(&self) -> Result<()> {
        self.initiate_model_load();
        let mut is_loading = self.is_loading.lock().unwrap();
        while *is_loading {
            is_loading = self.loading_condvar.wait(is_loading).unwrap();
        }
        drop(is_loading);
        if !self.is_model_loaded() {
            bail!("Model failed to load. Please check your model settings.");
        }
        Ok(())
    }

    /// Transcribes one meeting with the selected model and language, never
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

        let _active = MeetingActive::new(self);
        self.ensure_model_loaded()?;
        let mut settings = get_settings(&self.app_handle);
        settings.translate_to_english = false;
        let model_id = self
            .get_current_model()
            .unwrap_or_else(|| settings.selected_model.clone());
        let language =
            effective_language_for_model(&settings, self.model_manager.as_ref(), &model_id);
        let info = self
            .engine
            .loaded()
            .ok_or_else(|| anyhow!("Model is not loaded for transcription."))?;
        let setup = run_setup(&info, &settings, &language);
        info!(
            "Transcribing meeting '{}' with '{}' ({:.0}s of audio)",
            job.meeting.id,
            model_id,
            total as f64 / 16_000.0
        );

        let mut run = MeetingRun {
            tm: self,
            setup: &setup,
            model_id: &model_id,
            cancelled,
            mark: self.engine.cancel_mark(),
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

/// One meeting's decode on the engine.
struct MeetingRun<'a> {
    tm: &'a TranscriptionManager,
    setup: &'a RunSetup,
    model_id: &'a str,
    cancelled: &'a AtomicBool,
    /// Engine cancels after this stop the chunk being decoded.
    mark: CancelMark,
    /// Samples done, over every track.
    done: u64,
    on_progress: &'a mut dyn FnMut(u64),
}

impl MeetingRun<'_> {
    fn track(&mut self, path: &Path, settings: &AppSettings) -> Result<TrackTranscript> {
        let backend = settings.vad_backend;
        let app = self.tm.app_handle.clone();
        // Long-form chunking regardless of the model or setting: a meeting is
        // too long for one run, and chunks are where dictation gets its turn.
        let chunker = Chunker::new(Box::new(move || create_detector(backend, &app)));
        let gate = SpeechGate::new(create_detector(backend, &self.tm.app_handle));
        decode_track(path, chunker, gate, self)
    }

    fn check_cancelled(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(EngineError::Cancelled.into());
        }
        Ok(())
    }
}

impl TrackHooks for MeetingRun<'_> {
    fn before_chunk(&mut self) -> Result<()> {
        loop {
            self.check_cancelled()?;
            if !self.tm.dictation_busy() {
                return Ok(());
            }
            thread::sleep(DICTATION_POLL);
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
                Ok(transcript) => return Ok(transcript),
                Err(EngineError::Cancelled)
                    if !self.cancelled.load(Ordering::Acquire) && attempt < MAX_CHUNK_ATTEMPTS =>
                {
                    // Someone else's cancel (a dictation's): decode the chunk
                    // again once the engine is free.
                    info!("Meeting chunk stopped by another cancel; decoding it again");
                    attempt += 1;
                    self.before_chunk()?;
                    self.mark = self.tm.engine.cancel_mark();
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
    shared: Arc<Shared>,
}

struct Shared {
    app: AppHandle,
    store: Arc<dyn MeetingStore>,
    queue: Mutex<Queue>,
    wake: Condvar,
}

#[derive(Default)]
struct Queue {
    /// Ids of the meetings waiting, oldest first.
    pending: VecDeque<String>,
    /// The meeting being transcribed, and its cancel flag.
    current: Option<(String, Arc<AtomicBool>)>,
}

impl MeetingTranscriber {
    pub fn new(app: &AppHandle, store: Arc<dyn MeetingStore>) -> Self {
        let shared = Arc::new(Shared {
            app: app.clone(),
            store,
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        thread::Builder::new()
            .name("meeting-transcriber".into())
            .spawn(move || worker.run())
            .expect("Failed to spawn the meeting transcription thread");
        Self { shared }
    }

    /// Queues a meeting, unless it is already queued or being transcribed.
    /// Its status is the store's: `pending` or `recovered` until it starts.
    pub fn enqueue(&self, meeting_id: &str) {
        let mut queue = self.shared.lock();
        let queued = queue.pending.iter().any(|id| id == meeting_id)
            || queue
                .current
                .as_ref()
                .is_some_and(|(current, _)| current == meeting_id);
        if queued {
            return;
        }
        queue.pending.push_back(meeting_id.to_string());
        self.shared.wake.notify_one();
    }

    /// Stops a meeting's transcription, or takes it out of the queue; either
    /// way it is left `pending`, to be retried. Returns whether it was queued
    /// or running.
    pub fn cancel(&self, meeting_id: &str) -> bool {
        let mut queue = self.shared.lock();
        if let Some(at) = queue.pending.iter().position(|id| id == meeting_id) {
            queue.pending.remove(at);
            return true;
        }
        let Some((_, cancelled)) = queue
            .current
            .as_ref()
            .filter(|(current, _)| current == meeting_id)
        else {
            return false;
        };
        cancelled.store(true, Ordering::Release);
        drop(queue);
        // Free the engine now, unless that would also stop a dictation: then
        // the meeting stops before its next chunk.
        if let Some(tm) = self.shared.app.try_state::<Arc<TranscriptionManager>>() {
            if !tm.dictation_busy() {
                tm.cancel_transcription();
            }
        }
        true
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn run(&self) {
        loop {
            let (id, cancelled) = {
                let mut queue = self.lock();
                let id = loop {
                    match queue.pending.pop_front() {
                        Some(id) => break id,
                        None => queue = self.wake.wait(queue).unwrap_or_else(|e| e.into_inner()),
                    }
                };
                let cancelled = Arc::new(AtomicBool::new(false));
                queue.current = Some((id.clone(), Arc::clone(&cancelled)));
                (id, cancelled)
            };
            match self.store.job(&id) {
                Ok(job) => self.transcribe(&job, &cancelled),
                Err(e) => warn!("Meeting '{id}' can no longer be transcribed: {e:#}"),
            }
            self.lock().current = None;
        }
    }

    fn transcribe(&self, job: &MeetingJob, cancelled: &AtomicBool) {
        let id = job.meeting.id.as_str();
        let Some(tm) = self.app.try_state::<Arc<TranscriptionManager>>() else {
            error!("No transcription manager for meeting '{id}'");
            return;
        };
        self.store.set_status(id, MeetingStatus::Transcribing, None);
        self.progress(id, 0.0, None);

        let started = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tm.transcribe_meeting(job, cancelled, &mut |done, total| {
                let progress = if total == 0 {
                    1.0
                } else {
                    done as f64 / total as f64
                };
                let eta_ms = (done > 0).then(|| {
                    let elapsed = started.elapsed().as_millis() as f64;
                    (elapsed / done as f64 * total.saturating_sub(done) as f64) as u64
                });
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
            Err(_) if cancelled.load(Ordering::Acquire) => {
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
