//! The detection popup (`src/meeting-prompt/`): "Teams meeting detected —
//! Record / Ignore", "Stop the recording?", the auto-stop countdown when the
//! meeting pill is hidden, and the "recording started" confirmation.
//!
//! A custom window rather than a notification: Windows toast actions are not
//! reliable through the notification plugin on desktop. Like the meeting
//! pill it is borderless, stays on top and never takes focus, so it does not
//! steal the keyboard from the meeting app. It sits bottom right on Windows
//! (above the pill's default spot), top right on macOS. One prompt at a
//! time: showing one replaces the previous.

use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager, PhysicalPosition, WebviewWindow};
use tauri_specta::Event;

use log::{debug, error, warn};

pub const LABEL: &str = "meeting-prompt";

const WIDTH: f64 = 360.0;
const HEIGHT: f64 = 64.0;
/// Distance from the work area's edges (logical).
const MARGIN: f64 = 24.0;
/// Room left under the prompt on Windows for the meeting pill (logical).
#[cfg(not(target_os = "macos"))]
const PILL_ROOM: f64 = 56.0;
/// The "recording started" confirmation closes by itself.
const CONFIRMATION_DURATION: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeetingPrompt {
    /// A meeting app is using the microphone: Record / Ignore.
    Detected { app_id: String, app_name: String },
    /// The detected meeting looks over: Stop / Keep recording.
    Stop { app_name: String },
    /// Auto-stop countdown, shown here when the meeting pill is hidden.
    AutoStop { app_name: String },
    /// A detected meeting started recording without asking.
    Started { app_name: String },
}

/// The prompt to show, `None` to hide the popup.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct MeetingPromptChanged {
    pub prompt: Option<MeetingPrompt>,
}

static CURRENT: Mutex<Option<MeetingPrompt>> = Mutex::new(None);
/// Bumped on every change, so a delayed close only closes its own prompt.
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn current() -> Option<MeetingPrompt> {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Creates the popup window, hidden. No-op where meetings cannot be
/// recorded.
pub fn create(app: &AppHandle) {
    if !crate::meeting::control::manual_recording_supported() {
        return;
    }

    let mut builder = tauri::WebviewWindowBuilder::new(
        app,
        LABEL,
        tauri::WebviewUrl::App("src/meeting-prompt/index.html".into()),
    )
    .title("Meeting detected")
    .inner_size(WIDTH, HEIGHT)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .closable(false)
    .decorations(false)
    .shadow(false)
    .transparent(true)
    .always_on_top(true)
    .visible_on_all_workspaces(true)
    .skip_taskbar(true)
    .accept_first_mouse(true)
    .focusable(false)
    .focused(false)
    .visible(false);

    if let Some(data_dir) = crate::portable::data_dir() {
        builder = builder.data_directory(data_dir.join("webview"));
    }

    match builder.build() {
        Ok(_) => debug!("Meeting prompt window created (hidden)"),
        Err(e) => error!("Failed to create the meeting prompt window: {e}"),
    }
}

/// Shows `prompt`, replacing whatever the popup showed.
pub fn show(app: &AppHandle, prompt: MeetingPrompt) {
    let confirmation = matches!(prompt, MeetingPrompt::Started { .. });
    let generation = set(app, Some(prompt));
    if confirmation {
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(CONFIRMATION_DURATION);
            if GENERATION.load(Ordering::SeqCst) == generation {
                set(&app, None);
            }
        });
    }
}

/// Hides the popup.
pub fn hide(app: &AppHandle) {
    set(app, None);
}

/// Hides the popup if it currently shows a prompt matching `filter`.
pub fn hide_if(app: &AppHandle, filter: impl Fn(&MeetingPrompt) -> bool) {
    if current().as_ref().is_some_and(filter) {
        hide(app);
    }
}

fn set(app: &AppHandle, prompt: Option<MeetingPrompt>) -> u64 {
    let visible = prompt.is_some();
    let generation = {
        let mut current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
        *current = prompt.clone();
        GENERATION.fetch_add(1, Ordering::SeqCst) + 1
    };
    if let Err(e) = (MeetingPromptChanged { prompt }).emit(app) {
        error!("Failed to emit the meeting prompt: {e}");
    }

    let handle = app.clone();
    // Monitor queries and window geometry belong on the main thread.
    let _ = app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window(LABEL) else {
            return;
        };
        let shown = window.is_visible().unwrap_or(false);
        if visible && !shown {
            place(&handle, &window);
            if let Err(e) = window.show() {
                error!("Failed to show the meeting prompt: {e}");
            }
            #[cfg(target_os = "windows")]
            crate::meeting::indicator::force_topmost(&window);
        } else if !visible && shown {
            if let Err(e) = window.hide() {
                error!("Failed to hide the meeting prompt: {e}");
            }
        }
    });
    generation
}

/// Bottom right of the primary monitor's work area (above the pill's
/// default spot) on Windows, top right on macOS.
fn place(app: &AppHandle, window: &WebviewWindow) {
    let Some(monitor) = app.primary_monitor().ok().flatten().or_else(|| {
        app.available_monitors()
            .ok()
            .and_then(|m| m.into_iter().next())
    }) else {
        warn!("No monitor found to place the meeting prompt");
        return;
    };
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let size = window.outer_size().unwrap_or(tauri::PhysicalSize::new(
        (WIDTH * scale) as u32,
        (HEIGHT * scale) as u32,
    ));
    let margin = (MARGIN * scale).round() as i32;
    let x = area.position.x + area.size.width as i32 - size.width as i32 - margin;
    #[cfg(target_os = "macos")]
    let y = area.position.y + margin;
    #[cfg(not(target_os = "macos"))]
    let y = area.position.y + area.size.height as i32
        - size.height as i32
        - margin
        - (PILL_ROOM * scale).round() as i32;

    if let Err(e) = window.set_position(PhysicalPosition::new(x, y)) {
        error!("Failed to place the meeting prompt: {e}");
    }
}
