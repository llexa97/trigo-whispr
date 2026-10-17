//! Floating "recording a meeting" pill: red dot, timer, mic/system level
//! meters, capture warnings and a Stop button (`src/meeting-indicator/`).
//!
//! A small window of its own rather than a state of the dictation overlay,
//! so both can be on screen at once: the pill sits in a bottom corner, away
//! from the centered dictation overlay. It never takes focus, stays on top,
//! can be dragged, and remembers where it was dropped
//! (`meeting_indicator_position`). Optional (`show_meeting_indicator`).

use crate::managers::meeting::{MeetingManager, MeetingPhase};
use crate::settings::{self, WindowPosition};
use log::{debug, error, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager, PhysicalPosition, WebviewWindow, WindowEvent};

pub const LABEL: &str = "meeting_indicator";

const WIDTH: f64 = 248.0;
const HEIGHT: f64 = 44.0;
/// Distance from the work area's edges for the default placement (logical).
const MARGIN: f64 = 24.0;
/// A drag reports many moves; the position is saved once it settles.
const SAVE_DELAY: Duration = Duration::from_millis(600);

/// Last position set by code, so the `Moved` event it triggers is not
/// mistaken for the user dragging the pill.
static PLACED_AT: Mutex<Option<PhysicalPosition<i32>>> = Mutex::new(None);
/// Latest position dragged to, waiting to be saved.
static PENDING_SAVE: Mutex<Option<PhysicalPosition<i32>>> = Mutex::new(None);
static SAVE_SCHEDULED: AtomicBool = AtomicBool::new(false);

/// Creates the pill window, hidden. No-op where meetings cannot be recorded.
pub fn create(app: &AppHandle) {
    if !super::control::manual_recording_supported() {
        return;
    }

    let mut builder = tauri::WebviewWindowBuilder::new(
        app,
        LABEL,
        tauri::WebviewUrl::App("src/meeting-indicator/index.html".into()),
    )
    .title("Meeting recording")
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
        Ok(window) => {
            let app = app.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::Moved(position) = event {
                    on_moved(&app, *position);
                }
            });
            debug!("Meeting indicator window created (hidden)");
        }
        Err(e) => error!("Failed to create the meeting indicator window: {e}"),
    }
}

/// Shows the pill while a meeting records (if enabled), hides it otherwise.
pub fn sync(app: &AppHandle, phase: MeetingPhase) {
    let visible = phase != MeetingPhase::Idle && settings::get_settings(app).show_meeting_indicator;
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
                error!("Failed to show the meeting indicator: {e}");
            }
            #[cfg(target_os = "windows")]
            force_topmost(&window);
        } else if !visible && shown {
            if let Err(e) = window.hide() {
                error!("Failed to hide the meeting indicator: {e}");
            }
        }
    });
}

/// Re-applies the current meeting state after `show_meeting_indicator`
/// changed.
pub fn refresh(app: &AppHandle) {
    let phase = app
        .try_state::<Arc<MeetingManager>>()
        .map(|m| m.state().state)
        .unwrap_or(MeetingPhase::Idle);
    sync(app, phase);
}

/// Saved position when it is still on a connected monitor, else the bottom
/// right corner of the primary monitor's work area.
fn place(app: &AppHandle, window: &WebviewWindow) {
    let saved = settings::get_settings(app).meeting_indicator_position;
    let monitors = app.available_monitors().unwrap_or_default();
    let on_screen = |p: &WindowPosition| {
        monitors.iter().any(|m| {
            let (mp, ms) = (m.position(), m.size());
            p.x >= mp.x
                && p.y >= mp.y
                && p.x < mp.x + ms.width as i32
                && p.y < mp.y + ms.height as i32
        })
    };

    let position = match saved.filter(on_screen) {
        Some(p) => PhysicalPosition::new(p.x, p.y),
        None => {
            let Some(monitor) = app
                .primary_monitor()
                .ok()
                .flatten()
                .or_else(|| monitors.first().cloned())
            else {
                warn!("No monitor found to place the meeting indicator");
                return;
            };
            let scale = monitor.scale_factor();
            let area = monitor.work_area();
            let size = window.outer_size().unwrap_or(tauri::PhysicalSize::new(
                (WIDTH * scale) as u32,
                (HEIGHT * scale) as u32,
            ));
            let margin = (MARGIN * scale).round() as i32;
            PhysicalPosition::new(
                area.position.x + area.size.width as i32 - size.width as i32 - margin,
                area.position.y + area.size.height as i32 - size.height as i32 - margin,
            )
        }
    };

    *PLACED_AT.lock().unwrap_or_else(|e| e.into_inner()) = Some(position);
    if let Err(e) = window.set_position(position) {
        error!("Failed to place the meeting indicator: {e}");
    }
}

fn on_moved(app: &AppHandle, position: PhysicalPosition<i32>) {
    {
        let mut placed = PLACED_AT.lock().unwrap_or_else(|e| e.into_inner());
        if *placed == Some(position) {
            return;
        }
        *placed = None;
    }
    *PENDING_SAVE.lock().unwrap_or_else(|e| e.into_inner()) = Some(position);
    if SAVE_SCHEDULED.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(SAVE_DELAY);
        SAVE_SCHEDULED.store(false, Ordering::SeqCst);
        let Some(position) = PENDING_SAVE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        else {
            return;
        };
        let mut s = settings::get_settings(&app);
        s.meeting_indicator_position = Some(WindowPosition {
            x: position.x,
            y: position.y,
        });
        settings::write_settings(&app, s);
        debug!("Meeting indicator moved to {},{}", position.x, position.y);
    });
}

/// `set_always_on_top` can be overridden on Windows; re-assert the Z-order
/// without activating the window (same as the dictation overlay).
#[cfg(target_os = "windows")]
pub(crate) fn force_topmost(window: &WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };
    if let Ok(hwnd) = window.hwnd() {
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }
}
