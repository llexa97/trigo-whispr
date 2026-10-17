//! Imported-file transcription (Transcriptions page): files wait in a FIFO
//! and are handled in the background, one at a time. Each is first decoded
//! to a 16 kHz mono WAV in its folder (so the source may move afterwards, and
//! a re-run needs no re-import), then transcribed chunk by chunk like a
//! meeting track: dictation comes first, and a cancel stops it within a chunk.
//!
//! Where transcriptions and their status are stored is the
//! `FileTranscriptionManager`'s business; it implements
//! [`FileTranscriptionStore`] and feeds the queue.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;

use super::meeting::{progress_and_eta, BackgroundActive, BackgroundRun};
use super::queue::{JobQueue, RunningJob};
use super::TranscriptionManager;
use super::{effective_language_for_model, post_process_transcription_text, run_setup};
use crate::audio_toolkit::audio::file::{decode_to_wav, DecodeError, DecodeHooks};
use crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE;
use crate::engine_supervisor::EngineError;
use crate::managers::file_transcription::store::{
    FileTranscriptionErrorCode, FileTranscriptionSegment, FileTranscriptionStatus,
};
use crate::managers::model::ModelManager;
use crate::meeting::track::track_samples;
use crate::settings::get_settings;

/// The decoded audio, in each transcription's folder.
pub const AUDIO_FILE_NAME: &str = "audio.wav";

/// An imported file to transcribe.
#[derive(Clone, Debug)]
pub struct FileJob {
    pub id: String,
    /// The file as imported; only read if [`AUDIO_FILE_NAME`] is not in
    /// `dir` yet.
    pub source: PathBuf,
    pub dir: PathBuf,
    /// Forced language; `None` uses the selected one.
    pub language: Option<String>,
    pub post_process: bool,
}

/// A finished transcript.
#[derive(Clone, Debug)]
pub struct FileTranscript {
    pub segments: Vec<FileTranscriptionSegment>,
    pub model_id: String,
    pub language: Option<String>,
    pub post_processed_text: Option<String>,
}

/// The transcriptions' storage, as the queue reads and updates it.
pub trait FileTranscriptionStore: Send + Sync {
    /// The transcription to run, as it is now.
    fn job(&self, id: &str) -> Result<FileJob>;
    /// Records a status change; `error` is set for
    /// [`FileTranscriptionStatus::Failed`].
    fn set_status(
        &self,
        id: &str,
        status: FileTranscriptionStatus,
        error: Option<(FileTranscriptionErrorCode, &str)>,
    );
    /// The file was decoded to [`AUDIO_FILE_NAME`]: its exact length.
    fn audio_decoded(&self, id: &str, duration_ms: i64);
    /// Saves a finished transcript, before its status turns `Done`.
    fn save_transcript(&self, id: &str, transcript: &FileTranscript) -> Result<()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum FileTranscriptionStage {
    Decoding,
    Transcribing,
}

/// Emitted while a file is decoded and after each chunk of its transcription.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct FileTranscriptionProgress {
    pub id: String,
    pub stage: FileTranscriptionStage,
    /// 0 to 1, when known (a file whose length its container does not say
    /// decodes without a percentage).
    pub progress: Option<f64>,
    /// Estimated time left, once a chunk is transcribed.
    pub eta_ms: Option<u64>,
}

/// A failed transcription, as the UI explains it.
#[derive(Debug)]
struct JobError {
    code: FileTranscriptionErrorCode,
    detail: String,
}

impl JobError {
    fn new(code: FileTranscriptionErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    fn failed(e: anyhow::Error) -> Self {
        Self::new(FileTranscriptionErrorCode::Failed, format!("{e:#}"))
    }
}

impl From<DecodeError> for JobError {
    fn from(e: DecodeError) -> Self {
        let code = match &e {
            DecodeError::Unsupported(_) => FileTranscriptionErrorCode::UnsupportedFormat,
            DecodeError::NoAudioTrack => FileTranscriptionErrorCode::NoAudioTrack,
            DecodeError::Corrupt(_) => FileTranscriptionErrorCode::CorruptFile,
            DecodeError::Missing => FileTranscriptionErrorCode::FileMissing,
            DecodeError::Io(_) | DecodeError::Cancelled => FileTranscriptionErrorCode::Failed,
        };
        Self::new(code, e.to_string())
    }
}

/// Whether the selected model can be loaded: one is downloaded and selected.
pub fn model_available(app: &AppHandle) -> bool {
    let selected = get_settings(app).selected_model;
    !selected.is_empty()
        && app
            .try_state::<Arc<ModelManager>>()
            .is_some_and(|mm| mm.get_model_path(&selected).is_ok())
}

impl TranscriptionManager {
    /// Transcribes a decoded file (16 kHz mono WAV) with the selected model,
    /// in `language` if given, never translated. `on_progress` gets the
    /// samples done so far, out of `total`, after each chunk.
    fn transcribe_file(
        &self,
        wav: &Path,
        language: Option<&str>,
        cancelled: &AtomicBool,
        on_progress: &mut dyn FnMut(u64, u64),
    ) -> Result<FileTranscript> {
        let total = track_samples(wav)?;
        let _active = BackgroundActive::new(self);
        self.ensure_model_loaded()?;
        let mut settings = get_settings(&self.app_handle);
        settings.translate_to_english = false;
        if let Some(language) = language {
            settings.selected_language = language.to_string();
        }
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
            "Transcribing imported file {} with '{}' ({:.0}s of audio)",
            wav.display(),
            model_id,
            total as f64 / f64::from(WHISPER_SAMPLE_RATE)
        );

        let mut run = BackgroundRun {
            tm: self,
            setup: &setup,
            model_id: &model_id,
            cancelled,
            mark: self.engine.cancel_mark(),
            done: 0,
            on_progress: &mut |done| on_progress(done, total),
        };
        let track = run.track(wav, &settings)?;

        // The model's own segments (sentences, for whisper) rather than the
        // meetings' pause grouping: a monologue would otherwise be one
        // segment, useless as subtitles.
        let output_language = setup.output_language(&settings, track.language.clone());
        let segments = track
            .segments
            .iter()
            .filter_map(|segment| {
                let text = post_process_transcription_text(
                    segment.text.trim().to_string(),
                    &settings,
                    setup.custom_words_prompted(),
                    &output_language,
                    &setup.languages,
                );
                let text = text.trim();
                (!text.is_empty()).then(|| (segment, text.to_string()))
            })
            .enumerate()
            .map(|(idx, (segment, text))| FileTranscriptionSegment {
                idx: idx as i64,
                start_ms: segment.start_ms as i64,
                end_ms: segment.end_ms as i64,
                text,
            })
            .collect();
        Ok(FileTranscript {
            segments,
            model_id,
            language: track
                .language
                .clone()
                .or_else(|| setup.applied_language_hint.clone()),
            post_processed_text: None,
        })
    }
}

/// The queue of files to transcribe, and its worker thread.
#[derive(Clone)]
pub struct FileTranscriber {
    queue: JobQueue,
}

impl FileTranscriber {
    pub fn new(app: &AppHandle, store: Arc<dyn FileTranscriptionStore>) -> Self {
        let worker = Worker {
            app: app.clone(),
            store,
        };
        let queue = JobQueue::spawn(app, "file-transcriber", move |job| {
            match worker.store.job(job.id) {
                Ok(file) => worker.run(&file, job),
                Err(e) => warn!("File transcription '{}' can no longer run: {e:#}", job.id),
            }
        });
        Self { queue }
    }

    /// Queues a transcription, unless it is already queued or running.
    pub fn enqueue(&self, id: &str) {
        self.queue.enqueue(id);
    }

    /// Stops a transcription, or takes it out of the queue; it is then left
    /// `cancelled`. Returns false if it was not queued, or its result is
    /// already being saved.
    pub fn cancel(&self, id: &str) -> bool {
        self.queue.cancel(id)
    }

    /// Whether `id` is the transcription running now.
    pub fn is_current(&self, id: &str) -> bool {
        self.queue.is_current(id)
    }

    /// Cancels `id` and waits (a few seconds at most) for its worker to let
    /// go of its files. Returns whether it is no longer running.
    pub fn stop(&self, id: &str) -> bool {
        self.queue.cancel(id);
        self.queue
            .wait_until_not_current(id, std::time::Duration::from_secs(10))
    }
}

struct Worker {
    app: AppHandle,
    store: Arc<dyn FileTranscriptionStore>,
}

/// Decode hooks: the job's cancel flag, and progress events.
struct Decoding<'a> {
    worker: &'a Worker,
    id: &'a str,
    cancelled: &'a AtomicBool,
}

impl DecodeHooks for Decoding<'_> {
    fn check_cancelled(&self) -> Result<(), DecodeError> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(DecodeError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn progress(&mut self, done: Option<f64>) {
        self.worker
            .progress(self.id, FileTranscriptionStage::Decoding, done, None);
    }
}

impl Worker {
    fn run(&self, job: &FileJob, running: &RunningJob<'_>) {
        let id = job.id.as_str();
        let started = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.transcribe(job, running)
        }))
        .unwrap_or_else(|panic| {
            Err(JobError::failed(anyhow!(
                "file transcription panicked: {}",
                super::panic_payload_message(panic.as_ref())
            )))
        })
        .and_then(|transcript| {
            // Nothing is saved for a transcription cancelled before this
            // point, and no cancel is accepted after it.
            if !running.commit() {
                return Err(JobError::failed(EngineError::Cancelled.into()));
            }
            self.store
                .save_transcript(id, &transcript)
                .map_err(JobError::failed)?;
            Ok(transcript)
        });

        match result {
            Ok(transcript) => {
                info!(
                    "File transcription '{id}' done in {:.1}s: {} segments",
                    started.elapsed().as_secs_f64(),
                    transcript.segments.len()
                );
                self.progress(id, FileTranscriptionStage::Transcribing, Some(1.0), Some(0));
                self.store
                    .set_status(id, FileTranscriptionStatus::Done, None);
            }
            Err(_) if running.is_cancelled() => {
                info!("File transcription '{id}' cancelled");
                self.store
                    .set_status(id, FileTranscriptionStatus::Cancelled, None);
            }
            Err(e) => {
                warn!(
                    "File transcription '{id}' failed ({}): {}",
                    e.code.as_str(),
                    e.detail
                );
                self.store.set_status(
                    id,
                    FileTranscriptionStatus::Failed,
                    Some((e.code, &e.detail)),
                );
            }
        }
    }

    fn transcribe(
        &self,
        job: &FileJob,
        running: &RunningJob<'_>,
    ) -> Result<FileTranscript, JobError> {
        let id = job.id.as_str();
        if !model_available(&self.app) {
            return Err(JobError::new(
                FileTranscriptionErrorCode::NoModel,
                "No transcription model is downloaded",
            ));
        }
        let Some(tm) = self.app.try_state::<Arc<TranscriptionManager>>() else {
            return Err(JobError::failed(anyhow!("No transcription manager")));
        };

        let wav = job.dir.join(AUDIO_FILE_NAME);
        if !wav.exists() {
            self.store
                .set_status(id, FileTranscriptionStatus::Decoding, None);
            self.progress(id, FileTranscriptionStage::Decoding, Some(0.0), None);
            let decode_started = Instant::now();
            let samples = decode_to_wav(
                &job.source,
                &wav,
                &mut Decoding {
                    worker: self,
                    id,
                    cancelled: running.cancelled,
                },
            )?;
            let duration_ms = samples * 1_000 / u64::from(WHISPER_SAMPLE_RATE);
            info!(
                "Decoded '{}' ({:.0}s of audio) in {:.1}s",
                job.source.display(),
                duration_ms as f64 / 1_000.0,
                decode_started.elapsed().as_secs_f64()
            );
            self.store.audio_decoded(id, duration_ms as i64);
            if samples == 0 {
                return Err(JobError::new(
                    FileTranscriptionErrorCode::NoSpeech,
                    "The file holds no audio",
                ));
            }
        }

        self.store
            .set_status(id, FileTranscriptionStatus::Transcribing, None);
        self.progress(id, FileTranscriptionStage::Transcribing, Some(0.0), None);
        let started = Instant::now();
        let mut transcript = tm
            .transcribe_file(
                &wav,
                job.language.as_deref(),
                running.cancelled,
                &mut |done, total| {
                    let (progress, eta_ms) = progress_and_eta(started, done, total);
                    self.progress(
                        id,
                        FileTranscriptionStage::Transcribing,
                        Some(progress),
                        eta_ms,
                    );
                },
            )
            .map_err(JobError::failed)?;
        if transcript.segments.is_empty() {
            return Err(JobError::new(
                FileTranscriptionErrorCode::NoSpeech,
                "No speech was detected",
            ));
        }

        if job.post_process && !running.is_cancelled() {
            let text = transcript
                .segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let processed = tauri::async_runtime::block_on(
                crate::actions::process_transcription_output(&self.app, &text, true),
            );
            match processed.post_processed_text {
                Some(text) => transcript.post_processed_text = Some(text),
                None => warn!("Post-processing of file transcription '{id}' produced nothing"),
            }
        }
        Ok(transcript)
    }

    fn progress(
        &self,
        id: &str,
        stage: FileTranscriptionStage,
        progress: Option<f64>,
        eta_ms: Option<u64>,
    ) {
        if let Err(e) = (FileTranscriptionProgress {
            id: id.to_string(),
            stage,
            progress: progress.map(|p| p.clamp(0.0, 1.0)),
            eta_ms,
        })
        .emit(&self.app)
        {
            error!("Failed to emit file transcription progress: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_errors_keep_their_meaning() {
        let code = |e| JobError::from(e).code;
        assert_eq!(
            code(DecodeError::Unsupported("x".into())),
            FileTranscriptionErrorCode::UnsupportedFormat
        );
        assert_eq!(
            code(DecodeError::NoAudioTrack),
            FileTranscriptionErrorCode::NoAudioTrack
        );
        assert_eq!(
            code(DecodeError::Corrupt("x".into())),
            FileTranscriptionErrorCode::CorruptFile
        );
        assert_eq!(
            code(DecodeError::Missing),
            FileTranscriptionErrorCode::FileMissing
        );
    }
}
