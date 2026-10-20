//! Trigo's two default models, installed without asking.
//!
//! Trigo only ever uses Parakeet TDT 0.6B v3 (dictation) and Whisper Large v3
//! Turbo (meetings and imported files), so instead of a model picker the app
//! downloads both on first launch, and again at any later launch where one is
//! missing (deleted, or a download cut short by quitting). Parakeet goes first:
//! it is the smaller of the two and the one dictation needs, so the app is
//! usable as soon as it lands.
//!
//! Downloads go through [`ModelManager::download_model`], so they share its HF
//! cache lookup (nothing is fetched again when the file is already cached at
//! the catalog revision), retries, resume and mirror fallback. Progress still
//! arrives through the usual `model-download-progress` events; this module only
//! adds the per-model state the setup screen needs, broadcast as
//! `default-models-status`.

use crate::managers::model::ModelManager;
use crate::settings::{
    get_settings, write_settings, DEFAULT_DICTATION_MODEL, DEFAULT_LONG_FORM_MODEL,
};
use log::{error, info, warn};
use serde::Serialize;
use specta::Type;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// Download order: dictation first, so the app is usable sooner.
pub const DEFAULT_MODEL_IDS: [&str; 2] = [DEFAULT_DICTATION_MODEL, DEFAULT_LONG_FORM_MODEL];

/// What a default model is installed for, so the UI can label it.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum DefaultModelRole {
    Dictation,
    /// Meetings and imported files.
    Meetings,
}

/// How often a model someone else is already downloading (the Models page) is
/// checked on, rather than starting a second transfer of the same file.
const BUSY_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum DefaultModelState {
    /// On disk (models dir or HF cache).
    Ready,
    /// Missing, waiting for its turn (or for a retry).
    Pending,
    Downloading,
    Failed,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DefaultModelError {
    /// Not enough free space where the model is written.
    DiskSpace { required_mb: u64, available_mb: u64 },
    /// The download was cancelled (from the Models page).
    Cancelled,
    /// Every attempt failed: Hugging Face, retries and mirrors. Usually no
    /// connection; `detail` is the backend message, for the logs and the UI.
    Download { detail: String },
}

#[derive(Serialize, Clone, Debug, Type)]
pub struct DefaultModelStatus {
    pub model_id: String,
    pub role: DefaultModelRole,
    pub name: String,
    pub size_mb: u64,
    pub state: DefaultModelState,
    pub error: Option<DefaultModelError>,
}

#[derive(Serialize, Clone, Debug, Type)]
pub struct DefaultModelsStatus {
    pub models: Vec<DefaultModelStatus>,
    /// Whether an install pass is under way.
    pub running: bool,
}

pub struct DefaultModelsInstaller {
    app: AppHandle,
    errors: Mutex<Vec<(String, DefaultModelError)>>,
    /// The model the running pass is downloading, if any.
    current: Mutex<Option<String>>,
    running: AtomicBool,
}

/// Resets the running flag and broadcasts the final state, however the pass
/// ends.
struct RunGuard(Arc<DefaultModelsInstaller>);
impl Drop for RunGuard {
    fn drop(&mut self) {
        *self.0.current.lock().unwrap() = None;
        self.0.running.store(false, Ordering::Release);
        self.0.emit_status();
    }
}

impl DefaultModelsInstaller {
    pub fn new(app: &AppHandle) -> Self {
        Self {
            app: app.clone(),
            errors: Mutex::new(Vec::new()),
            current: Mutex::new(None),
            running: AtomicBool::new(false),
        }
    }

    /// Download whichever default models are missing, in the background.
    /// Does nothing when a pass is already running; a failed model is tried
    /// again by the next call (Retry, or the next launch).
    pub fn ensure_installed(self: &Arc<Self>) {
        if self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        let installer = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let _guard = RunGuard(Arc::clone(&installer));
            installer.run().await;
        });
    }

    /// Whether a pass is downloading (or about to download) a default model,
    /// so a dictation started before any model is usable can say one is on
    /// its way.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    pub fn status(&self) -> DefaultModelsStatus {
        let model_manager = self.app.state::<Arc<ModelManager>>();
        let current = self.current.lock().unwrap().clone();
        let errors = self.errors.lock().unwrap().clone();
        let models = DEFAULT_MODEL_IDS
            .iter()
            .map(|&id| {
                let info = model_manager.get_model_info(id);
                let error = errors
                    .iter()
                    .find(|(model_id, _)| model_id == id)
                    .map(|(_, e)| e.clone());
                let downloaded = info.as_ref().is_some_and(|m| m.is_downloaded);
                let downloading = current.as_deref() == Some(id)
                    || info.as_ref().is_some_and(|m| m.is_downloading);
                let state = if downloaded {
                    DefaultModelState::Ready
                } else if downloading {
                    DefaultModelState::Downloading
                } else if error.is_some() {
                    DefaultModelState::Failed
                } else {
                    DefaultModelState::Pending
                };
                DefaultModelStatus {
                    model_id: id.to_string(),
                    role: if id == DEFAULT_DICTATION_MODEL {
                        DefaultModelRole::Dictation
                    } else {
                        DefaultModelRole::Meetings
                    },
                    name: info.as_ref().map(|m| m.name.clone()).unwrap_or_default(),
                    size_mb: info.as_ref().map(|m| m.size_mb).unwrap_or(0),
                    state,
                    error: if downloaded { None } else { error },
                }
            })
            .collect();
        DefaultModelsStatus {
            models,
            running: self.running.load(Ordering::Acquire),
        }
    }

    fn emit_status(&self) {
        let _ = self.app.emit("default-models-status", self.status());
    }

    fn set_error(&self, model_id: &str, error: Option<DefaultModelError>) {
        let mut errors = self.errors.lock().unwrap();
        errors.retain(|(id, _)| id != model_id);
        if let Some(error) = error {
            errors.push((model_id.to_string(), error));
        }
    }

    async fn run(&self) {
        let model_manager = self.app.state::<Arc<ModelManager>>().inner().clone();
        for model_id in DEFAULT_MODEL_IDS {
            let Some(info) = model_manager.get_model_info(model_id) else {
                // The bundled catalog always has both; only a catalog
                // regeneration that drops one could get here.
                error!("Default model {} is missing from the catalog", model_id);
                continue;
            };

            // Already being downloaded from the Models page: wait for that
            // transfer instead of starting a second one into the same file.
            if info.is_downloading {
                *self.current.lock().unwrap() = Some(model_id.to_string());
                self.emit_status();
                while model_manager
                    .get_model_info(model_id)
                    .is_some_and(|m| m.is_downloading)
                {
                    tokio::time::sleep(BUSY_POLL_INTERVAL).await;
                }
                *self.current.lock().unwrap() = None;
                // That transfer was the user's: if it ended without the file
                // (cancelled, or failed and already reported), leave it until
                // Retry or the next launch rather than restarting it at once.
                if !model_manager
                    .get_model_info(model_id)
                    .is_some_and(|m| m.is_downloaded)
                {
                    self.set_error(model_id, Some(DefaultModelError::Cancelled));
                    self.emit_status();
                    continue;
                }
            }

            if !model_manager
                .get_model_info(model_id)
                .is_some_and(|m| m.is_downloaded)
            {
                self.set_error(model_id, None);
                *self.current.lock().unwrap() = Some(model_id.to_string());
                self.emit_status();
                let outcome = self.download(&model_manager, model_id, info.size_mb).await;
                *self.current.lock().unwrap() = None;
                self.set_error(model_id, outcome.err());
            }

            if model_manager
                .get_model_info(model_id)
                .is_some_and(|m| m.is_downloaded)
            {
                self.set_error(model_id, None);
                self.select_for_dictation(model_id).await;
            }
            self.emit_status();
        }
    }

    async fn download(
        &self,
        model_manager: &ModelManager,
        model_id: &str,
        size_mb: u64,
    ) -> Result<(), DefaultModelError> {
        // Checked up front: a full disk otherwise surfaces as an opaque write
        // error well into the transfer. Bytes already fetched by an earlier,
        // interrupted run are not discounted, so this errs on the safe side.
        if let Some(available) = available_space(&model_manager.download_dir()) {
            let required = size_mb * 1024 * 1024;
            if available < required {
                warn!(
                    "Not enough disk space for {}: {} MB needed, {} MB free",
                    model_id,
                    size_mb,
                    available / (1024 * 1024)
                );
                return Err(DefaultModelError::DiskSpace {
                    required_mb: size_mb,
                    available_mb: available / (1024 * 1024),
                });
            }
        }

        info!("Installing default model {}", model_id);
        match model_manager.download_model(model_id).await {
            // `Ok` also covers a cancel from the Models page; only the file
            // tells the two apart.
            Ok(())
                if model_manager
                    .get_model_info(model_id)
                    .is_some_and(|m| m.is_downloaded) =>
            {
                info!("Default model {} installed", model_id);
                Ok(())
            }
            Ok(()) => {
                info!("Default model {} download was cancelled", model_id);
                Err(DefaultModelError::Cancelled)
            }
            Err(e) => {
                error!("Default model {} download failed: {:#}", model_id, e);
                // Same event as the `download_model` command, so the Models
                // page stops showing the transfer as running. `silent`: the
                // setup screen and the background toast report it already.
                let _ = self.app.emit(
                    "model-download-failed",
                    serde_json::json!({
                        "model_id": model_id,
                        "error": e.to_string(),
                        "silent": true,
                    }),
                );
                Err(DefaultModelError::Download {
                    detail: e.to_string(),
                })
            }
        }
    }

    /// Make `model_id` the dictation model when none usable is selected.
    /// Parakeet comes first, so it wins whenever it is available; Whisper only
    /// stands in when Parakeet couldn't be installed, and hands dictation back
    /// to it once a later pass installs it.
    async fn select_for_dictation(&self, model_id: &str) {
        let settings = get_settings(&self.app);
        let model_manager = self.app.state::<Arc<ModelManager>>();
        let selection_usable = !settings.selected_model.is_empty()
            && model_manager
                .get_model_info(&settings.selected_model)
                .is_some_and(|m| m.is_downloaded);
        let replaces_fallback = model_id == DEFAULT_DICTATION_MODEL
            && settings.dictation_model_is_fallback
            && settings.selected_model == DEFAULT_LONG_FORM_MODEL;
        if selection_usable && !replaces_fallback {
            return;
        }
        let is_fallback = model_id != DEFAULT_DICTATION_MODEL;

        if !settings.onboarding_completed {
            // The setup screen loads it (and completes onboarding) when the
            // user moves on; loading it now would compete with the download
            // of the second model for nothing.
            info!("Selecting default model {} (onboarding pending)", model_id);
            let mut settings = settings;
            settings.selected_model = model_id.to_string();
            settings.dictation_model_is_fallback = is_fallback;
            write_settings(&self.app, settings);
            let _ = self.app.emit("models-updated", ());
            return;
        }

        if is_fallback {
            // Written first: the flag must hold as soon as the fallback does.
            let mut settings = settings;
            settings.dictation_model_is_fallback = true;
            write_settings(&self.app, settings);
        }
        info!("Selecting default model {} for dictation", model_id);
        let app = self.app.clone();
        let id = model_id.to_string();
        // Loading the model blocks until it is in memory.
        let result = tauri::async_runtime::spawn_blocking(move || {
            crate::commands::models::switch_active_model(&app, &id)
        })
        .await;
        match result {
            Ok(Ok(())) => {
                if !is_fallback {
                    clear_dictation_fallback(&self.app);
                }
                crate::tray::update_tray_menu(&self.app);
            }
            Ok(Err(e)) => error!("Failed to select default model {}: {}", model_id, e),
            Err(e) => error!("Default model selection task failed: {}", e),
        }
    }
}

/// The user (or the installer) settled the dictation model: it no longer
/// stands in for Parakeet.
pub fn clear_dictation_fallback(app: &AppHandle) {
    let mut settings = get_settings(app);
    if settings.dictation_model_is_fallback {
        settings.dictation_model_is_fallback = false;
        write_settings(app, settings);
    }
}

/// Free bytes on the volume holding `dir` (or its closest existing ancestor:
/// the HF cache folder may not exist yet on a first launch). `None` when it
/// can't be read, in which case the download is simply attempted.
fn available_space(dir: &Path) -> Option<u64> {
    let existing = dir.ancestors().find(|p| p.exists())?;
    available_space_at(existing)
}

#[cfg(unix)]
fn available_space_at(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is a valid NUL-terminated string and `stat` a properly
    // sized, writable statvfs.
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)] // the field widths differ per platform
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(windows)]
fn available_space_at(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut available: u64 = 0;
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut available), None, None) }.ok()?;
    Some(available)
}

#[cfg(not(any(unix, windows)))]
fn available_space_at(_path: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_models_are_in_the_catalog() {
        for id in DEFAULT_MODEL_IDS {
            assert!(
                crate::catalog::CATALOG.iter().any(|desc| desc.id == id),
                "{id} is not a catalog default quant"
            );
        }
    }

    #[test]
    fn available_space_falls_back_to_an_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not").join("yet").join("created");
        assert!(available_space(&missing).is_some_and(|bytes| bytes > 0));
    }
}
