//! Runs the detection: a background thread polls the probe every
//! [`POLL_INTERVAL`] while `meeting_detection_enabled` is on (and the
//! platform has a backend), feeds the [`Engine`] and carries out its
//! actions. Turning the setting off stops the thread.

use super::catalog::{self, Platform};
use super::engine::{Action, Answer, Engine, Recording, Settings, POLL_INTERVAL};
use super::prompt::{self, MeetingPrompt};
use super::{platform_probe, MicUsageProbe};
use crate::managers::meeting::{MeetingManager, MeetingSource};
use crate::settings::{self, AppSettings, MeetingAutoStop};
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

/// Auto-stop countdown of meeting `meeting_id`, shown in the meeting pill
/// (or the popup when the pill is hidden). `remaining_secs` is `None` once
/// it ended or was cancelled.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct MeetingAutoStopCountdown {
    pub remaining_secs: Option<u32>,
    pub meeting_id: Option<String>,
}

/// The user's answer in the popup (or the pill). Each one names the app or
/// meeting it answers for, so an answer to a prompt that has since been
/// replaced is dropped instead of acting on something else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeetingPromptResponse {
    /// Detected meeting: record it.
    Record { app_id: String },
    /// Detected meeting: not this call.
    Ignore { app_id: String },
    /// Meeting over: stop the recording now.
    Stop { meeting_id: String },
    /// Meeting over: keep recording (also cancels the countdown).
    KeepRecording { meeting_id: String },
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
    /// Meeting the pending stop offer (prompt or countdown) is for.
    stop_offer: Mutex<Option<String>>,
}

/// Takes the pending stop offer if it is for `meeting_id`. Only one taker
/// wins (the user's answer or the end of the countdown), and an answer to an
/// offer withdrawn or replaced since gets `false`.
fn claim_offer(offer: &mut Option<String>, meeting_id: &str) -> bool {
    if offer.as_deref() == Some(meeting_id) {
        *offer = None;
        true
    } else {
        false
    }
}

impl DetectionService {
    fn claim_stop_offer(&self, meeting_id: &str) -> bool {
        claim_offer(
            &mut self.stop_offer.lock().unwrap_or_else(|e| e.into_inner()),
            meeting_id,
        )
    }

    fn engine(&self) -> std::sync::MutexGuard<'_, Engine> {
        self.engine.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Registers the service, creates the popup window and starts polling if
/// detection is enabled.
pub fn init(app: &AppHandle) {
    app.manage(Arc::new(DetectionService {
        engine: Mutex::new(Engine::new()),
        poller: Mutex::new(None),
        countdown: AtomicU64::new(0),
        stop_offer: Mutex::new(None),
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
            *service.engine() = Engine::new();
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
                let actions = service.engine().tick(
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

/// A detection setting changed: poll now, so the engine applies it at once
/// (a `meeting_auto_stop` change withdraws or re-offers a pending stop).
pub fn settings_changed(app: &AppHandle) {
    let Some(service) = service(app) else {
        return;
    };
    let poller = service.poller.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(poller) = poller.as_ref() {
        poller.thread.thread().unpark();
    }
}

/// The start of a detected meeting for catalog app `app_id` is over
/// (`started`, or cancelled at the reminder, failed or refused).
pub fn start_finished(app: &AppHandle, app_id: &str, started: bool) {
    if let Some(service) = service(app) {
        service.engine().start_finished(app_id, started);
    }
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
                *service.stop_offer.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(meeting_id.clone());
                if auto {
                    start_countdown(app, service, settings, meeting_id, app_name);
                } else {
                    prompt::show(
                        app,
                        MeetingPrompt::Stop {
                            meeting_id,
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

fn emit_countdown(app: &AppHandle, remaining_secs: Option<u32>, meeting_id: Option<&str>) {
    let event = MeetingAutoStopCountdown {
        remaining_secs,
        meeting_id: meeting_id.map(str::to_string),
    };
    if let Err(e) = event.emit(app) {
        error!("Failed to emit the auto-stop countdown: {e}");
    }
}

fn withdraw_stop_offer(app: &AppHandle, service: &DetectionService) {
    *service.stop_offer.lock().unwrap_or_else(|e| e.into_inner()) = None;
    close_stop_offer(app, service);
}

/// Cancels the countdown and closes the stop prompt (the offer itself is
/// already taken or withdrawn).
fn close_stop_offer(app: &AppHandle, service: &DetectionService) {
    service.countdown.fetch_add(1, Ordering::SeqCst);
    emit_countdown(app, None, None);
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
                meeting_id: meeting_id.clone(),
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
                emit_countdown(&app, Some(remaining), Some(&meeting_id));
                std::thread::sleep(Duration::from_secs(1));
            }
            let Some(detection) = service(&app) else {
                return;
            };
            // Re-read the mode: switching away from `auto` cancels the stop
            // even if the engine has not polled since.
            let still_auto =
                settings::get_settings(&app).meeting_auto_stop == MeetingAutoStop::Auto;
            if cancelled() || !still_auto || !detection.claim_stop_offer(&meeting_id) {
                return;
            }
            close_stop_offer(&app, &detection);
            info!("Stopping the detected meeting after the countdown");
            stop_meeting(&app, meeting_id);
        });
    if let Err(e) = spawned {
        error!("Failed to spawn the auto-stop countdown: {e}");
    }
}

/// Stops meeting `meeting_id` if it is still the one being recorded (the
/// check and the stop are atomic in the manager), on its own thread.
fn stop_meeting(app: &AppHandle, meeting_id: String) {
    let Some(manager) = meeting_manager(app) else {
        return;
    };
    std::thread::spawn(move || {
        if let Err(e) = manager.stop_meeting(&meeting_id) {
            warn!("Not stopping meeting {meeting_id}: {e}");
        }
    });
}

/// The user answered the popup (or cancelled the countdown in the pill).
pub fn respond(app: &AppHandle, response: MeetingPromptResponse) {
    let Some(service) = service(app) else {
        return;
    };
    match response {
        MeetingPromptResponse::Record { app_id } | MeetingPromptResponse::Ignore { app_id }
            if !matches!(
                prompt::current(),
                Some(MeetingPrompt::Detected { app_id: ref shown, .. }) if *shown == app_id
            ) =>
        {
            debug!("Dropping a stale answer to the {app_id} prompt");
        }
        MeetingPromptResponse::Record { app_id } => {
            let actions = service.engine().answer(&app_id, Answer::Record);
            run_actions(app, &service, &settings::get_settings(app), actions);
        }
        MeetingPromptResponse::Ignore { app_id } => {
            let actions = service.engine().answer(&app_id, Answer::Ignore);
            run_actions(app, &service, &settings::get_settings(app), actions);
        }
        MeetingPromptResponse::Stop { meeting_id }
        | MeetingPromptResponse::KeepRecording { meeting_id }
            if !service.claim_stop_offer(&meeting_id) =>
        {
            debug!("Dropping a stale answer to the stop offer for {meeting_id}");
        }
        MeetingPromptResponse::Stop { meeting_id } => {
            close_stop_offer(app, &service);
            info!("Stopping the detected meeting at the user's request");
            stop_meeting(app, meeting_id);
        }
        MeetingPromptResponse::KeepRecording { meeting_id } => {
            info!("Keeping the detected meeting recording");
            close_stop_offer(app, &service);
            service.engine().stop_declined(&meeting_id);
        }
        MeetingPromptResponse::Dismiss => {
            prompt::hide_if(app, |p| matches!(p, MeetingPrompt::Started { .. }))
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_offer_is_claimed_once_and_only_for_its_meeting() {
        let mut offer = Some("a".to_string());
        assert!(!claim_offer(&mut offer, "b"));
        assert!(claim_offer(&mut offer, "a"));
        // The countdown and the user's answer cannot both act on it.
        assert!(!claim_offer(&mut offer, "a"));
    }

    #[test]
    fn an_answer_to_a_withdrawn_offer_is_dropped() {
        // Prompt for A; A stopped elsewhere, so the offer was withdrawn;
        // B is now recording when the user clicks "Stop" on A's prompt.
        let mut offer = Some("a".to_string());
        offer.take();
        assert!(!claim_offer(&mut offer, "a"));
        // Even once B has its own offer.
        offer = Some("b".to_string());
        assert!(!claim_offer(&mut offer, "a"));
        assert_eq!(offer.as_deref(), Some("b"));
    }
}
