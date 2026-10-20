//! Speaker diarization of a meeting track or an imported file (#30), on a
//! second engine: its own worker process, on CPU, holding only the
//! diarization model. The transcription engine keeps its model, so a
//! dictation meanwhile is never slowed by a reload; the diarizer leaves it
//! half the cores.
//!
//! Diarization is a bonus: when it is off, its model is missing or it
//! fails, the transcript is saved without speakers. Only the user's cancel
//! stops the job.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use log::{info, warn};
use transcribe_cpp::Backend;

use super::TranscriptionManager;
use crate::diarization::{turns_from, Turn};
use crate::engine_supervisor::{
    CancelMark, DeviceSelector, EngineError, EngineSupervisor, LoadSpec,
};
use crate::meeting::track::track_samples;
use crate::settings::get_settings;

/// The diarization engine. Meetings and imported files take turns on it.
#[derive(Clone)]
pub(super) struct Diarizer {
    engine: EngineSupervisor,
    /// Held for a whole diarization, load to unload.
    running: Arc<Mutex<()>>,
}

impl Diarizer {
    pub(super) fn new() -> Self {
        Self {
            // CPU-only: no GPU driver in this worker, and the GPU stays free
            // for the transcription engine.
            engine: EngineSupervisor::new(false),
            running: Arc::new(Mutex::new(())),
        }
    }

    /// Diarizes `wav`, loading the model for the run and unloading it after
    /// (it is used once per job; between jobs it would only hold memory).
    /// Any cancel since `mark` stops it.
    fn run(
        &self,
        mark: CancelMark,
        model: PathBuf,
        wav: &Path,
        samples: usize,
    ) -> Result<Vec<Turn>, EngineError> {
        let _running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let result = self
            .engine
            .load(LoadSpec {
                path: model,
                backend: Backend::Cpu,
                device: DeviceSelector::Auto,
            })
            .and_then(|_| self.engine.diarize_since(mark, wav.to_path_buf(), samples));
        self.engine.unload().wait();
        result.map(|segments| turns_from(&segments))
    }
}

impl TranscriptionManager {
    /// Stops a diarization in progress (a cancelled meeting or file).
    pub(super) fn cancel_diarization(&self) {
        self.diarizer.engine.cancel();
    }

    /// The speaker turns of a 16 kHz mono WAV, or `None` when speaker
    /// detection is off, its model is not downloaded, or it failed. Fails
    /// only when `cancelled`.
    pub(super) fn diarize(&self, wav: &Path, cancelled: &AtomicBool) -> Result<Option<Vec<Turn>>> {
        if !get_settings(&self.app_handle).speaker_detection_enabled {
            return Ok(None);
        }
        let Some(model) = self.model_manager.diarization_model_path() else {
            return Ok(None);
        };
        let samples = match track_samples(wav) {
            Ok(samples) => samples as usize,
            Err(e) => {
                warn!("Speaker detection skipped for {}: {e:#}", wav.display());
                return Ok(None);
            }
        };
        // A dictation goes first, as before each transcription chunk.
        self.wait_for_dictation(cancelled)?;
        info!(
            "Detecting speakers in {} ({:.0}s of audio)",
            wav.display(),
            samples as f64 / 16_000.0
        );
        // Another job's cancel stops this run too: run it again then.
        for attempt in 1..=2 {
            // Taken before the flag is read: a cancel after the read bumps
            // the engine past it.
            let mark = self.diarizer.engine.cancel_mark();
            if cancelled.load(Ordering::Acquire) {
                return Err(EngineError::Cancelled.into());
            }
            match self.diarizer.run(mark, model.clone(), wav, samples) {
                Ok(turns) => {
                    let speakers = {
                        let mut ids: Vec<u32> = turns.iter().map(|t| t.speaker).collect();
                        ids.sort_unstable();
                        ids.dedup();
                        ids.len()
                    };
                    info!(
                        "Found {speakers} speaker(s) in {} ({} turns)",
                        wav.display(),
                        turns.len()
                    );
                    return Ok(Some(turns));
                }
                Err(EngineError::Cancelled) if cancelled.load(Ordering::Acquire) => {
                    return Err(EngineError::Cancelled.into());
                }
                Err(EngineError::Cancelled) if attempt == 1 => {
                    info!("Speaker detection stopped by another job's cancel; running it again");
                }
                Err(e) => {
                    warn!("Speaker detection failed for {}: {e}", wav.display());
                    return Ok(None);
                }
            }
        }
        Ok(None)
    }
}
