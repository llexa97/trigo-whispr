//! Windows process loopback: capture everything the system plays *except*
//! Trigo Whispr's own process tree, so the app's feedback sounds never land
//! in `system.wav`.
//!
//! cpal does not expose this mode, so it is driven directly through WASAPI:
//! `ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, …)`
//! with `PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE` on our own PID,
//! then a regular event-driven `IAudioCaptureClient` loop on a dedicated
//! thread. Modelled on Microsoft's *ApplicationLoopback* sample
//! (Windows-classic-samples). The virtual device follows default-output
//! changes by itself.
//!
//! Requires Windows 10 build 20348 / Windows 11; [`supported`] checks the
//! build, and any activation failure makes the caller fall back to endpoint
//! loopback.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::core::{implement, Interface, Ref, HRESULT};
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX,
};
use windows::Win32::Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT;
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, IAgileObject, IAgileObject_Impl, BLOB, COINIT_MULTITHREADED,
};
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;

use super::source::{SourceStats, SourceTap};

/// Format requested from the virtual device (AUTOCONVERTPCM converts the
/// mix to it); the track writer resamples to 16 kHz mono.
pub const CHANNELS: u16 = 2;
pub const SAMPLE_RATE: u32 = 48_000;

/// First build with `PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE`.
const MIN_BUILD: u32 = 20_348;
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(5);
/// 100 ms shared-mode buffer, in 100 ns units.
const BUFFER_DURATION_HNS: i64 = 1_000_000;

/// Windows `(major, build)` from `RtlGetVersion` (not subject to the
/// manifest-based lies of `GetVersionEx`).
pub fn windows_version() -> Option<(u32, u32)> {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    let status = unsafe { RtlGetVersion(&mut info) };
    status
        .is_ok()
        .then_some((info.dwMajorVersion, info.dwBuildNumber))
}

/// Whether this Windows build has process loopback with process-tree
/// exclusion.
pub fn supported() -> bool {
    match windows_version() {
        Some((major, build)) => major > 10 || (major == 10 && build >= MIN_BUILD),
        None => false,
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct ActivationHandler {
    done: Mutex<Option<mpsc::Sender<()>>>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        _operation: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        if let Some(tx) = self.done.lock().ok().and_then(|mut d| d.take()) {
            let _ = tx.send(());
        }
        Ok(())
    }
}

impl IAgileObject_Impl for ActivationHandler_Impl {}

/// A running process-loopback capture. Stops (and joins its thread) on drop.
pub struct ProcessLoopback {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ProcessLoopback {
    /// Activate the virtual loopback device and start capturing into `tap`
    /// (which must be sized for [`CHANNELS`] × [`SAMPLE_RATE`]). Returns once
    /// the stream is running, or with the activation error.
    pub fn start(tap: SourceTap) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let thread_stop = stop.clone();
        let thread = std::thread::Builder::new()
            .name("meeting-process-loopback".into())
            .spawn(move || capture_thread(tap, thread_stop, ready_tx))
            .map_err(|e| format!("spawn process loopback thread: {e}"))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err("process loopback thread exited during startup".into())
            }
        }
    }
}

impl Drop for ProcessLoopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Started {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
}

fn capture_thread(
    mut tap: SourceTap,
    stop: Arc<AtomicBool>,
    ready_tx: mpsc::Sender<Result<(), String>>,
) {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if com.is_err() {
        let _ = ready_tx.send(Err(format!("CoInitializeEx: {com:?}")));
        return;
    }

    let started = match unsafe { activate_and_start() } {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            unsafe { CoUninitialize() };
            return;
        }
    };
    let _ = ready_tx.send(Ok(()));

    let stats: Arc<SourceStats> = tap.stats().clone();
    if let Err(e) = unsafe { pump(&started, &mut tap, &stop) } {
        log::warn!("meeting capture: process loopback stream failed: {e}");
        stats.failed.store(true, Ordering::SeqCst);
    }

    unsafe {
        let _ = started.client.Stop();
        let _ = CloseHandle(started.event);
    }
    drop(started);
    unsafe { CoUninitialize() };
}

unsafe fn activate_and_start() -> Result<Started, String> {
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: std::process::id(),
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // A VT_BLOB pointing at `params`, which outlives the activation call. The
    // PROPVARIANT does not own the blob, so it must not be PropVariantClear'd.
    let prop = PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: &mut params as *mut _ as *mut u8,
                    },
                },
            }),
        },
    };

    let (done_tx, done_rx) = mpsc::channel();
    let handler: IActivateAudioInterfaceCompletionHandler = ActivationHandler {
        done: Mutex::new(Some(done_tx)),
    }
    .into();

    let operation = ActivateAudioInterfaceAsync(
        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        &IAudioClient::IID,
        Some(&prop),
        &handler,
    )
    .map_err(|e| format!("ActivateAudioInterfaceAsync: {e}"))?;
    done_rx
        .recv_timeout(ACTIVATION_TIMEOUT)
        .map_err(|_| "process loopback activation timed out".to_string())?;

    let mut activate_hr = HRESULT(0);
    let mut activated = None;
    operation
        .GetActivateResult(&mut activate_hr, &mut activated)
        .map_err(|e| format!("GetActivateResult: {e}"))?;
    activate_hr
        .ok()
        .map_err(|e| format!("process loopback activation: {e}"))?;
    let client: IAudioClient = activated
        .ok_or("process loopback activation returned no interface")?
        .cast()
        .map_err(|e| format!("IAudioClient cast: {e}"))?;

    let block_align = CHANNELS * 4;
    let format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
        nChannels: CHANNELS,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * block_align as u32,
        nBlockAlign: block_align,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    client
        .Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            BUFFER_DURATION_HNS,
            0,
            &format,
            None,
        )
        .map_err(|e| format!("IAudioClient::Initialize: {e}"))?;

    let event = CreateEventW(None, false, false, None).map_err(|e| format!("CreateEvent: {e}"))?;
    let setup = (|| -> windows::core::Result<IAudioCaptureClient> {
        client.SetEventHandle(event)?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        Ok(capture)
    })();
    match setup {
        Ok(capture) => Ok(Started {
            client,
            capture,
            event,
        }),
        Err(e) => {
            let _ = CloseHandle(event);
            Err(format!("process loopback start: {e}"))
        }
    }
}

/// Drain packets until `stop` is raised or the stream fails.
unsafe fn pump(
    started: &Started,
    tap: &mut SourceTap,
    stop: &AtomicBool,
) -> windows::core::Result<()> {
    let channels = CHANNELS as usize;
    while !stop.load(Ordering::SeqCst) {
        // Timeout so `stop` is honoured even when nothing plays.
        let wait = WaitForSingleObject(started.event, 100);
        if wait != WAIT_OBJECT_0 {
            continue;
        }
        loop {
            let packet = started.capture.GetNextPacketSize()?;
            if packet == 0 {
                break;
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames: u32 = 0;
            let mut flags: u32 = 0;
            started
                .capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
            let len = frames as usize * channels;
            if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                tap.push_silence(len);
            } else {
                let samples = std::slice::from_raw_parts(data as *const f32, len);
                tap.push(len, samples.iter().copied());
            }
            started.capture.ReleaseBuffer(frames)?;
        }
    }
    Ok(())
}
