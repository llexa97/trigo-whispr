//! Keeps the system awake while a meeting is being recorded.
//!
//! The assertion is released when the [`SleepGuard`] is dropped.

/// Prevents idle system sleep for as long as it is alive. The display may
/// still turn off.
pub struct SleepGuard {
    #[cfg(target_os = "windows")]
    release: Option<std::sync::mpsc::Sender<()>>,
    #[cfg(target_os = "macos")]
    assertion_id: Option<u32>,
}

impl SleepGuard {
    pub fn acquire(reason: &str) -> Self {
        imp::acquire(reason)
    }
}

impl Drop for SleepGuard {
    fn drop(&mut self) {
        imp::release(self);
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::SleepGuard;
    use windows::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
    };

    // SetThreadExecutionState is per thread: the state is held by a dedicated
    // thread that lives until the guard is dropped.
    pub fn acquire(_reason: &str) -> SleepGuard {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let spawned = std::thread::Builder::new()
            .name("meeting-keep-awake".into())
            .spawn(move || {
                // SAFETY: plain Win32 call with valid flags.
                let prev = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
                if prev.0 == 0 {
                    log::warn!("SetThreadExecutionState failed; system may sleep");
                }
                // Returns when the guard drops its sender.
                let _ = rx.recv();
                // SAFETY: see above.
                unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
            });
        if let Err(e) = spawned {
            log::warn!("Failed to spawn keep-awake thread: {e}");
            return SleepGuard { release: None };
        }
        SleepGuard { release: Some(tx) }
    }

    pub fn release(guard: &mut SleepGuard) {
        // Dropping the sender wakes the keep-awake thread.
        guard.release.take();
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::SleepGuard;
    use std::ffi::{c_char, c_void, CString};

    type CFStringRef = *const c_void;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;
    const K_IO_RETURN_SUCCESS: i32 = 0;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            level: u32,
            name: CFStringRef,
            assertion_id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(assertion_id: u32) -> i32;
    }

    fn cf_string(s: &str) -> Option<CFStringRef> {
        let c = CString::new(s).ok()?;
        // SAFETY: `c` is a valid NUL-terminated UTF-8 string.
        let r = unsafe {
            CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), K_CF_STRING_ENCODING_UTF8)
        };
        (!r.is_null()).then_some(r)
    }

    pub fn acquire(reason: &str) -> SleepGuard {
        // kIOPMAssertionTypePreventUserIdleSystemSleep
        let (Some(kind), Some(name)) = (cf_string("PreventUserIdleSystemSleep"), cf_string(reason))
        else {
            return SleepGuard { assertion_id: None };
        };
        let mut id: u32 = 0;
        // SAFETY: both CFStrings are valid; `id` is a valid out pointer.
        let rc = unsafe {
            let rc = IOPMAssertionCreateWithName(kind, K_IOPM_ASSERTION_LEVEL_ON, name, &mut id);
            CFRelease(kind);
            CFRelease(name);
            rc
        };
        if rc != K_IO_RETURN_SUCCESS {
            log::warn!("IOPMAssertionCreateWithName failed ({rc}); system may sleep");
            return SleepGuard { assertion_id: None };
        }
        SleepGuard {
            assertion_id: Some(id),
        }
    }

    pub fn release(guard: &mut SleepGuard) {
        if let Some(id) = guard.assertion_id.take() {
            // SAFETY: `id` was returned by IOPMAssertionCreateWithName.
            unsafe { IOPMAssertionRelease(id) };
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod imp {
    use super::SleepGuard;

    pub fn acquire(_reason: &str) -> SleepGuard {
        SleepGuard {}
    }

    pub fn release(_guard: &mut SleepGuard) {}
}
