//! Manual start/stop of a meeting recording, shared by the tray, the global
//! shortcut, the `--toggle-meeting` CLI flag and the indicator pill.
//!
//! Every entry point goes through [`toggle_meeting`], so they all behave the
//! same: the first recording shows the "tell the participants" reminder, a
//! start failure is reported in a dialog, and the tray and the pill follow
//! through `MeetingStateChanged` whatever started the recording.

use crate::managers::meeting::{MeetingManager, MeetingSource, MeetingStateChanged};
use crate::meeting::capture;
use crate::settings;
use crate::tray_i18n::{get_tray_translations, TrayStrings};
use log::{error, info, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};
use tauri_specta::Event;

/// A toggle (reminder dialog, capture start or stop) is in progress. Further
/// toggles are dropped meanwhile, so a double press or a CLI call racing the
/// tray cannot start two recordings or stop one that is just starting.
static TOGGLE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

struct InProgress;

impl InProgress {
    fn acquire() -> Option<Self> {
        // Lazily: a guard built for a refused toggle would be dropped at once
        // and release the lock still held by the running one.
        (!TOGGLE_IN_PROGRESS.swap(true, Ordering::SeqCst)).then(|| InProgress)
    }
}

impl Drop for InProgress {
    fn drop(&mut self) {
        TOGGLE_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

/// Whether meetings can be recorded on this platform (not on Linux).
pub fn manual_recording_supported() -> bool {
    capture::capture_supported()
}

/// Starts a meeting recording if none is running, stops it otherwise.
/// Returns immediately: the blocking work runs on its own thread.
pub fn toggle_meeting(app: &AppHandle, trigger: &'static str) {
    run_manual(app, trigger, false);
}

/// Starts a meeting recording (after the reminder) unless one is already
/// running, in which case nothing happens. For buttons that only mean
/// "start", so a stale click never stops a recording.
pub fn start_meeting(app: &AppHandle, trigger: &'static str) {
    run_manual(app, trigger, true);
}

fn run_manual(app: &AppHandle, trigger: &'static str, start_only: bool) {
    if !manual_recording_supported() {
        warn!("Meeting recording toggled from {trigger}, but it is not supported here");
        return;
    }
    let Some(guard) = InProgress::acquire() else {
        info!("Meeting toggle from {trigger} ignored: another one is in progress");
        return;
    };
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("meeting-toggle".into())
        .spawn(move || {
            let _guard = guard;
            let Some(manager) = app
                .try_state::<Arc<MeetingManager>>()
                .map(|m| m.inner().clone())
            else {
                warn!("Meeting toggle from {trigger} before the meeting manager exists");
                return;
            };
            if manager.is_recording() && start_only {
                info!("Meeting start from {trigger} ignored: already recording");
            } else if manager.is_recording() {
                info!("Stopping the meeting recording ({trigger})");
                if let Err(e) = manager.stop() {
                    error!("Failed to stop the meeting recording: {e}");
                }
            } else {
                start_with_reminder(&app, &manager, trigger);
            }
        });
    if let Err(e) = spawned {
        error!("Failed to spawn the meeting toggle thread: {e}");
    }
}

fn start_with_reminder(app: &AppHandle, manager: &MeetingManager, trigger: &str) {
    let current = settings::get_settings(app);
    let strings = get_tray_translations(Some(current.app_language));

    if current.meeting_consent_reminder {
        match ask_consent_reminder(app, &strings) {
            Reminder::Start => {}
            Reminder::StartAndDontShowAgain => {
                let mut s = settings::get_settings(app);
                s.meeting_consent_reminder = false;
                settings::write_settings(app, s);
            }
            Reminder::Cancel => {
                info!("Meeting recording cancelled at the reminder ({trigger})");
                return;
            }
        }
    }

    info!("Starting a meeting recording ({trigger})");
    if let Err(e) = manager.start(MeetingSource::Manual) {
        error!("Failed to start the meeting recording: {e}");
        app.dialog()
            .message(e.to_string())
            .title(strings.meeting_start_failed_title)
            .kind(MessageDialogKind::Error)
            .blocking_show();
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Reminder {
    Start,
    StartAndDontShowAgain,
    Cancel,
}

/// "Let the participants know you are recording." Native dialogs have no
/// checkbox, so "don't show again" is a button of its own.
fn ask_consent_reminder(app: &AppHandle, strings: &TrayStrings) -> Reminder {
    let start = strings.meeting_reminder_start.clone();
    let dont_show = strings.meeting_reminder_dont_show.clone();
    let result = app
        .dialog()
        .message(&strings.meeting_reminder_message)
        .title(&strings.meeting_reminder_title)
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::YesNoCancelCustom(
            start.clone(),
            dont_show.clone(),
            strings.cancel.clone(),
        ))
        .blocking_show_with_result();
    reminder_choice(&result, &start, &dont_show)
}

fn reminder_choice(result: &MessageDialogResult, start: &str, dont_show: &str) -> Reminder {
    match result {
        MessageDialogResult::Yes => Reminder::Start,
        MessageDialogResult::No => Reminder::StartAndDontShowAgain,
        MessageDialogResult::Custom(label) if label == start => Reminder::Start,
        MessageDialogResult::Custom(label) if label == dont_show => Reminder::StartAndDontShowAgain,
        _ => Reminder::Cancel,
    }
}

/// Keeps the tray and the indicator pill in sync with the meeting state,
/// whatever started or stopped the recording.
pub fn init(app: &AppHandle) {
    crate::meeting::indicator::create(app);

    let handle = app.clone();
    MeetingStateChanged::listen_any(app, move |event| {
        let snapshot = event.payload;
        crate::tray::set_meeting_state(&handle, snapshot.state, snapshot.elapsed_ms);
        crate::meeting::indicator::sync(&handle, snapshot.state);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminder_maps_custom_labels_and_platform_results() {
        let choice = |r: MessageDialogResult| reminder_choice(&r, "Start", "Start, don't show");
        assert_eq!(choice(MessageDialogResult::Yes), Reminder::Start);
        assert_eq!(
            choice(MessageDialogResult::No),
            Reminder::StartAndDontShowAgain
        );
        assert_eq!(
            choice(MessageDialogResult::Custom("Start".into())),
            Reminder::Start
        );
        assert_eq!(
            choice(MessageDialogResult::Custom("Start, don't show".into())),
            Reminder::StartAndDontShowAgain
        );
        assert_eq!(
            choice(MessageDialogResult::Custom("Cancel".into())),
            Reminder::Cancel
        );
        assert_eq!(choice(MessageDialogResult::Cancel), Reminder::Cancel);
    }

    #[test]
    fn toggles_are_dropped_while_one_runs() {
        let first = InProgress::acquire();
        assert!(first.is_some());
        // Every refusal must leave the running toggle's lock in place.
        assert!(InProgress::acquire().is_none());
        assert!(InProgress::acquire().is_none());
        assert!(InProgress::acquire().is_none());
        drop(first);
        assert!(InProgress::acquire().is_some());
    }
}
