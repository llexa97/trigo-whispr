//! Runs the detection: a background thread polls the probe every
//! [`POLL_INTERVAL`] while `meeting_detection_enabled` is on (and the
//! platform has a backend), feeds the [`Engine`] and carries out its
//! actions. Turning the setting off stops the thread.

use super::catalog::{self, Platform};
use super::engine::{Action, Answer, Engine, Recording, Settings, POLL_INTERVAL};
use super::prompt::{self, MeetingPrompt};
use super::{platform_probe, MicUsageProbe};
use crate::managers::meeting::{MeetingManager, MeetingSource};
use crate::settings::{self, AppSettings};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tauri_specta::Event;

/// Length of the cancellable auto-stop countdown.
pub const AUTO_STOP_COUNTDOWN_SECS: u32 = 10;

/// Auto-stop countdown, shown in the meeting pill (or the popup when the
/// pill is hidden). `remaining_secs` is `None` once it ended or was
/// cancelled.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct MeetingAutoStopCountdown {
    pub remaining_secs: Option<u32>,
}

/// The user's answer in the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingPromptResponse {
    /// Detected meeting: record it.
    Record,
    /// Detected meeting: not this call.
    Ignore,
    /// Meeting over: stop the recording now.
    Stop,
    /// Meeting over: keep recording (also cancels the countdown).
    KeepRecording,
    /// Close the confirmation.
    Dismiss,
}

struct Poller {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

pub struct DetectionService {
    engine: Mutex<Engine>,
    poller: Mutex<Option<Poller>>,
    /// Bumped to cancel the running auto-stop countdown.
    countdown: AtomicU64,
}

/// Registers the service, creates the popup window and starts polling if
/// detection is enabled.
pub fn init(app: &AppHandle) {
    app.manage(Arc::new(DetectionService {
        engine: Mutex::new(Engine::new()),
        poller: Mutex::new(None),
        countdown: AtomicU64::new(0),
    }));
    prompt::create(app);
    sync(app);
}

fn service(app: &AppHandle) -> Option<Arc<DetectionService>> {
    app.try_state::<Arc<DetectionService>>()
        .map(|s| s.inner().clone())
}

/// Whether this platform can detect meetings (has a microphone-usage
/// backend).
pub fn supported() -> bool {
    crate::meeting::control::manual_recording_supported() && platform_probe().supported()
}

/// Starts or stops the polling thread to match `meeting_detection_enabled`.
pub fn sync(app: &AppHandle) {
    let Some(service) = service(app) else {
        return;
    };
    let enabled = settings::get_settings(app).meeting_detection_enabled;
    let mut poller = service.poller.lock().unwrap_or_else(|e| e.into_inner());

    if !enabled {
        if let Some(Poller { stop, thread }) = poller.take() {
            stop.store(true, Ordering::SeqCst);
            thread.thread().unpark();
            // The thread may be the one calling us; never join it.
            info!("Meeting detection stopped");
            *service.engine.lock().unwrap_or_else(|e| e.into_inner()) = Engine::new();
            prompt::hide_if(app, |p| matches!(p, MeetingPrompt::Detected { .. }));
            withdraw_stop_offer(app, &service);
        }
        return;
    }
    if poller.as_ref().is_some_and(|p| !p.thread.is_finished()) {
        return;
    }
    if !crate::meeting::control::manual_recording_supported() {
        return;
    }
    let probe = platform_probe();
    if !probe.supported() {
        info!("Meeting detection is not available on this platform yet");
        return;
    }

    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let handle = app.clone();
    match std::thread::Builder::new()
        .name("meeting-detection".into())
        .spawn(move || poll_loop(handle, probe, thread_stop))
    {
        Ok(thread) => {
            info!("Meeting detection started");
            *poller = Some(Poller { stop, thread });
        }
        Err(e) => error!("Failed to spawn the meeting detection thread: {e}"),
    }
}

fn poll_loop(app: AppHandle, mut probe: Box<dyn MicUsageProbe>, stop: Arc<AtomicBool>) {
    let mut last_error: Option<String> = None;
    while !stop.load(Ordering::SeqCst) {
        let Some(service) = service(&app) else {
            return;
        };
        match probe.active_mic_users() {
            Ok(users) => {
                last_error = None;
                let settings = settings::get_settings(&app);
                let recording = current_recording(&app);
                let actions = service
                    .engine
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .tick(
                        Instant::now(),
                        &users,
                        &recording,
                        &engine_settings(&settings),
                    );
                run_actions(&app, &service, &settings, actions);
            }
            // Logged once per distinct error, not every two seconds.
            Err(e) => {
                let message = e.to_string();
                if last_error.as_deref() != Some(message.as_str()) {
                    warn!("Meeting detection probe failed: {message}");
                    last_error = Some(message);
                }
            }
        }
        std::thread::park_timeout(POLL_INTERVAL);
    }
    debug!("Meeting detection thread exited");
}

fn engine_settings(settings: &AppSettings) -> Settings<'_> {
    Settings {
        platform: Platform::current(),
        app_overrides: &settings.meeting_detection_apps,
        auto_start: settings.meeting_auto_start,
        auto_stop: settings.meeting_auto_stop,
    }
}

fn meeting_manager(app: &AppHandle) -> Option<Arc<MeetingManager>> {
    app.try_state::<Arc<MeetingManager>>()
        .map(|m| m.inner().clone())
}

fn current_recording(app: &AppHandle) -> Recording {
    let Some(manager) = meeting_manager(app) else {
        return Recording::None;
    };
    match manager.recording_source() {
        Some((meeting_id, MeetingSource::Detected { app_id, .. })) => {
            Recording::Detected { meeting_id, app_id }
        }
        Some((_, MeetingSource::Manual)) => Recording::Other,
        None if manager.is_recording() => Recording::Other,
        None => Recording::None,
    }
}

fn run_actions(
    app: &AppHandle,
    service: &DetectionService,
    settings: &AppSettings,
    actions: Vec<Action>,
) {
    for action in actions {
        debug!("Meeting detection: {action:?}");
        match action {
            Action::Prompt { app_id, app_name } => {
                prompt::show(
                    app,
                    MeetingPrompt::Detected {
                        app_id: app_id.to_string(),
                        app_name: app_name.to_string(),
                    },
                );
                if settings.meeting_prompt_sound {
                    crate::audio_feedback::play_meeting_prompt_sound(app);
                }
            }
            Action::ClosePrompt => {
                prompt::hide_if(app, |p| matches!(p, MeetingPrompt::Detected { .. }));
            }
            Action::StartRecording {
                app_id,
                app_name,
                auto,
            } => {
                info!("Recording the {app_name} meeting detected");
                crate::meeting::control::start_detected_meeting(
                    app,
                    MeetingSource::Detected {
                        app_id: app_id.to_string(),
                        app_name: app_name.to_string(),
                    },
                    auto,
                );
            }
            Action::OfferStop {
                meeting_id,
                app_name,
                auto,
            } => {
                info!("{app_name} released the microphone: offering to stop the meeting");
                if auto {
                    start_countdown(app, service, settings, meeting_id, app_name);
                } else {
                    prompt::show(
                        app,
                        MeetingPrompt::Stop {
                            app_name: app_name.to_string(),
                        },
                    );
                }
            }
            Action::WithdrawStopOffer => withdraw_stop_offer(app, service),
        }
    }
}

fn is_stop_offer(p: &MeetingPrompt) -> bool {
    matches!(
        p,
        MeetingPrompt::Stop { .. } | MeetingPrompt::AutoStop { .. }
    )
}

fn emit_countdown(app: &AppHandle, remaining_secs: Option<u32>) {
    if let Err(e) = (MeetingAutoStopCountdown { remaining_secs }).emit(app) {
        error!("Failed to emit the auto-stop countdown: {e}");
    }
}

fn withdraw_stop_offer(app: &AppHandle, service: &DetectionService) {
    service.countdown.fetch_add(1, Ordering::SeqCst);
    emit_countdown(app, None);
    prompt::hide_if(app, is_stop_offer);
}

/// Counts down in the pill (or the popup), then stops the meeting unless
/// cancelled or already stopped.
fn start_countdown(
    app: &AppHandle,
    detection: &DetectionService,
    settings: &AppSettings,
    meeting_id: String,
    app_name: &str,
) {
    let generation = detection.countdown.fetch_add(1, Ordering::SeqCst) + 1;
    if !settings.show_meeting_indicator {
        prompt::show(
            app,
            MeetingPrompt::AutoStop {
                app_name: app_name.to_string(),
            },
        );
    }
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("meeting-auto-stop".into())
        .spawn(move || {
            let cancelled =
                || service(&app).is_none_or(|s| s.countdown.load(Ordering::SeqCst) != generation);
            for remaining in (1..=AUTO_STOP_COUNTDOWN_SECS).rev() {
                if cancelled() {
                    return;
                }
                emit_countdown(&app, Some(remaining));
                std::thread::sleep(Duration::from_secs(1));
            }
            if cancelled() {
                return;
            }
            emit_countdown(&app, None);
            prompt::hide_if(&app, is_stop_offer);
            let Some(manager) = meeting_manager(&app) else {
                return;
            };
            // Only the meeting the countdown was for.
            if manager
                .recording_source()
                .is_some_and(|(id, _)| id == meeting_id)
            {
                info!("Stopping the detected meeting after the countdown");
                if let Err(e) = manager.stop() {
                    error!("Failed to stop the meeting recording: {e}");
                }
            }
        });
    if let Err(e) = spawned {
        error!("Failed to spawn the auto-stop countdown: {e}");
    }
}

/// The user answered the popup (or cancelled the countdown).
pub fn respond(app: &AppHandle, response: MeetingPromptResponse) {
    let Some(service) = service(app) else {
        return;
    };
    let current = prompt::current();
    match response {
        MeetingPromptResponse::Record | MeetingPromptResponse::Ignore => {
            let Some(MeetingPrompt::Detected { app_id, .. }) = current else {
                return;
            };
            let answer = if response == MeetingPromptResponse::Record {
                Answer::Record
            } else {
                Answer::Ignore
            };
            let actions = service
                .engine
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .answer(&app_id, answer);
            let settings = settings::get_settings(app);
            run_actions(app, &service, &settings, actions);
        }
        MeetingPromptResponse::Stop => {
            withdraw_stop_offer(app, &service);
            if let Some(manager) = meeting_manager(app) {
                std::thread::spawn(move || {
                    if manager.is_recording() {
                        if let Err(e) = manager.stop() {
                            error!("Failed to stop the meeting recording: {e}");
                        }
                    }
                });
            }
        }
        MeetingPromptResponse::KeepRecording => {
            info!("Keeping the detected meeting recording");
            withdraw_stop_offer(app, &service);
        }
        MeetingPromptResponse::Dismiss => prompt::hide(app),
    }
}

/// A catalog app, as listed in the settings.
#[derive(Debug, Clone, Serialize, Type)]
pub struct MeetingDetectionApp {
    pub id: String,
    pub display_name: String,
    pub kind: catalog::AppKind,
    pub default_enabled: bool,
}

/// The catalog apps this platform can detect.
pub fn apps() -> Vec<MeetingDetectionApp> {
    let platform = Platform::current();
    catalog::CATALOG
        .iter()
        .filter(|e| platform == Platform::Other || e.available_on(platform))
        .map(|e| MeetingDetectionApp {
            id: e.id.to_string(),
            display_name: e.display_name.to_string(),
            kind: e.kind,
            default_enabled: e.default_enabled,
        })
        .collect()
}
