//! The speaker diarization model (#30): NVIDIA's streaming Sortformer
//! (4 speakers), converted for transcribe.cpp. It transcribes nothing, so it
//! stays out of the model list: it lives in its own folder under the models
//! dir (the custom-model scan skips folders), and its download reports its
//! own events. The default models installer fetches it while speaker
//! detection is on; the settings can download or delete it too.

use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Result};
use hf_hub::api::tokio::CancellationToken;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri_specta::Event;

use super::download::{HttpDownloadEvent, HttpDownloadOutcome};
use super::ModelManager;

const REPO_ID: &str = "handy-computer/diar_streaming_sortformer_4spk-v2.1-gguf";
/// Pinned, so the bytes always match [`SHA256`].
const REVISION: &str = "60ebab3ef7cae9869be0abcde13360347e96ba03";
/// Q8_0: a third of F32's size, within half a point of its error rate.
const FILE_NAME: &str = "diar_streaming_sortformer_4spk-v2.1-Q8_0.gguf";
const SIZE_BYTES: u64 = 139_310_336;
const SHA256: &str = "a5dacdc650790266c7a362e54e6bf51952015487edaa606c4e11632bc32442a9";
/// How the setup screen lists it among the models installed at launch.
pub const DIARIZATION_MODEL_ID: &str = "speaker-detection";
pub const DIARIZATION_MODEL_NAME: &str = "Sortformer 4spk v2.1";
pub const DIARIZATION_MODEL_SIZE_MB: u64 = SIZE_BYTES.div_ceil(1024 * 1024);
/// Under the models dir.
const DIR_NAME: &str = "diarization";
/// Key of its download in the cancel flags, apart from any model id.
const DOWNLOAD_KEY: &str = "\u{0}diarization";

/// Where the diarization model stands.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct DiarizationModelStatus {
    pub downloaded: bool,
    pub downloading: bool,
    pub size_bytes: u64,
}

/// Emitted when the diarization model is downloaded, deleted, or its
/// download starts, stops or fails (`error`).
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct DiarizationModelChanged {
    pub status: DiarizationModelStatus,
    pub error: Option<String>,
}

/// Emitted while the diarization model downloads.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct DiarizationModelProgress {
    pub downloaded: u64,
    pub total: u64,
}

impl ModelManager {
    /// Where the diarization model is written.
    pub fn diarization_dir(&self) -> PathBuf {
        self.models_dir.join(DIR_NAME)
    }

    /// The diarization model, if it is downloaded.
    pub fn diarization_model_path(&self) -> Option<PathBuf> {
        Some(self.diarization_dir().join(FILE_NAME)).filter(|path| path.is_file())
    }

    pub fn diarization_model_status(&self) -> DiarizationModelStatus {
        DiarizationModelStatus {
            downloaded: self.diarization_model_path().is_some(),
            downloading: self.cancel_flags.lock().unwrap().contains_key(DOWNLOAD_KEY),
            size_bytes: SIZE_BYTES,
        }
    }

    fn emit_diarization_status(&self, error: Option<String>) {
        let _ = DiarizationModelChanged {
            status: self.diarization_model_status(),
            error,
        }
        .emit(&self.app_handle);
    }

    /// Downloads the diarization model, resuming a partial download. A
    /// cancelled download keeps its partial file and is not an error.
    pub async fn download_diarization_model(&self) -> Result<()> {
        if self.diarization_model_path().is_some() {
            return Ok(());
        }
        let token = CancellationToken::new();
        {
            let mut flags = self.cancel_flags.lock().unwrap();
            if flags.contains_key(DOWNLOAD_KEY) {
                bail!("The speaker detection model is already downloading");
            }
            flags.insert(DOWNLOAD_KEY.to_string(), token.clone());
        }
        self.emit_diarization_status(None);

        let result = self.fetch_diarization_model(&token).await;
        self.cancel_flags.lock().unwrap().remove(DOWNLOAD_KEY);
        match &result {
            Ok(()) => self.emit_diarization_status(None),
            Err(e) => {
                warn!("Speaker detection model download failed: {e:#}");
                self.emit_diarization_status(Some(format!("{e:#}")));
            }
        }
        result
    }

    async fn fetch_diarization_model(&self, token: &CancellationToken) -> Result<()> {
        let dir = self.diarization_dir();
        fs::create_dir_all(&dir)?;
        let path = dir.join(FILE_NAME);
        let partial = dir.join(format!("{FILE_NAME}.partial"));
        let url = format!("https://huggingface.co/{REPO_ID}/resolve/{REVISION}/{FILE_NAME}");
        let app = self.app_handle.clone();
        let outcome = Self::download_http_resumable_with_events(
            FILE_NAME,
            &url,
            &partial,
            Some(SIZE_BYTES),
            Some(SHA256),
            token,
            &move |event| {
                if let HttpDownloadEvent::Progress(progress) = event {
                    let _ = DiarizationModelProgress {
                        downloaded: progress.downloaded,
                        total: progress.total,
                    }
                    .emit(&app);
                }
            },
        )
        .await?;
        match outcome {
            HttpDownloadOutcome::Cancelled => {
                info!("Speaker detection model download cancelled");
            }
            HttpDownloadOutcome::Completed => {
                fs::rename(&partial, &path)?;
                info!("Speaker detection model downloaded to {}", path.display());
            }
        }
        Ok(())
    }

    pub fn cancel_diarization_model_download(&self) {
        if let Some(token) = self.cancel_flags.lock().unwrap().get(DOWNLOAD_KEY) {
            token.cancel();
        }
    }

    /// Deletes the diarization model and any partial download of it.
    pub fn delete_diarization_model(&self) -> Result<()> {
        self.cancel_diarization_model_download();
        match fs::remove_dir_all(self.diarization_dir()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.emit_diarization_status(None);
        Ok(())
    }
}
