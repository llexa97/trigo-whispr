// Adapted from fly-on-the-wall (MIT) — https://github.com/swarnavspujari/fly-on-the-wall/blob/5f6f56d3ed86b835ab2f1598c8ff444ed5d1bd93/crates/fly-audio/src/cpal_backend.rs
//
//! The meeting capture engine. cpal opens the microphone (and, on Windows
//! before build 20348, the endpoint loopback of the default output: an
//! *input* stream on an *output* device is WASAPI loopback); the Windows
//! process loopback runs on its own thread (`win_process_loopback`).
//!
//! cpal streams are `!Send`, so one dedicated thread owns them for the whole
//! session and the [`CaptureSession`] handle only talks to it over channels.
//! That thread also does all the non-real-time work: every tick it drains
//! the ring buffers fed by the callbacks, downmixes/resamples/writes the
//! tracks, pads idle tracks with silence to keep them on the shared clock,
//! reopens failed or replaced devices, and refreshes levels and warnings.
//!
//! Differences from fly: no pause, two fixed 16 kHz files instead of
//! native-rate files plus a post-hoc mixdown, incremental crash-safe writes,
//! device-change recovery, and typed warnings.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::source::{self, SourceFeed};
use super::track::{frames_for_ms, TrackWriter};
use super::{
    CaptureConfig, CaptureError, CaptureOutput, CaptureSession, CaptureWarning, Result,
    SystemCaptureMode, MIC_FILE_NAME, SYSTEM_FILE_NAME, TRACK_SAMPLE_RATE,
};

/// How often the capture thread services the tracks.
const TICK: Duration = Duration::from_millis(20);
/// A source silent for longer than this is padded up to the clock. A source
/// that keeps delivering is never padded mid-stream (that would punch holes
/// in live audio), so the residual misalignment between two live tracks is
/// their device clocks' drift (typically tens of ppm, a few ms per minute);
/// both are padded to the same length at stop.
const IDLE_GAP: Duration = Duration::from_millis(200);
/// A microphone delivering nothing for this long is considered dead.
const MIC_STALL: Duration = Duration::from_secs(3);
/// Delay between reopen attempts of a lost device.
const REOPEN_RETRY: Duration = Duration::from_secs(1);
/// Header rewrite period, bounding what a crash can lose before repair.
const FLUSH_EVERY: Duration = Duration::from_secs(5);
/// Default-device and output-volume polling period.
const POLL_EVERY: Duration = Duration::from_secs(2);
/// A system track with nothing but digital silence after this long raises
/// `SystemAudioSilent`.
const SILENT_AFTER: Duration = Duration::from_secs(60);

fn device_name(d: &cpal::Device) -> Option<String> {
    d.description().ok().map(|desc| desc.name().to_string())
}

fn device_id(d: &cpal::Device) -> Option<String> {
    d.id().ok().map(|id| id.to_string())
}

/// State shared between the capture thread and the session handle.
#[derive(Default)]
struct Shared {
    sticky: Mutex<Vec<CaptureWarning>>,
    output_muted: AtomicBool,
    system_silent: AtomicBool,
    mic_level: AtomicU32,
    system_level: AtomicU32,
    system_mode: Mutex<Option<SystemCaptureMode>>,
}

impl Shared {
    fn add_warning(&self, w: CaptureWarning) {
        let mut sticky = self.sticky.lock().unwrap();
        if !sticky.contains(&w) {
            sticky.push(w);
        }
    }

    fn warnings(&self) -> Vec<CaptureWarning> {
        let mut all = self.sticky.lock().unwrap().clone();
        if self.output_muted.load(Ordering::Relaxed) {
            all.push(CaptureWarning::OutputMuted);
        }
        if self.system_silent.load(Ordering::Relaxed) {
            all.push(CaptureWarning::SystemAudioSilent);
        }
        all
    }

    fn system_mode(&self) -> Option<SystemCaptureMode> {
        *self.system_mode.lock().unwrap()
    }
}

pub fn start(cfg: CaptureConfig) -> Result<Box<dyn CaptureSession>> {
    std::fs::create_dir_all(&cfg.out_dir)?;
    let shared = Arc::new(Shared::default());
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<Instant>>();
    let (done_tx, done_rx) = mpsc::channel::<Result<CaptureOutput>>();

    let thread_shared = shared.clone();
    std::thread::Builder::new()
        .name("meeting-capture".into())
        .spawn(move || capture_thread(cfg, thread_shared, stop_rx, ready_tx, done_tx))
        .map_err(|e| CaptureError::Backend(format!("spawn capture thread: {e}")))?;

    // Stream construction errors come back synchronously; a degraded start
    // (one track missing) comes back as warnings.
    let started = ready_rx
        .recv()
        .map_err(|_| CaptureError::Backend("capture thread died during startup".into()))??;

    Ok(Box::new(CpalSession {
        stop_tx,
        done_rx,
        shared,
        started,
    }))
}

struct CpalSession {
    /// Dropping it (session dropped without `stop`) also stops the thread.
    stop_tx: Sender<()>,
    done_rx: Receiver<Result<CaptureOutput>>,
    shared: Arc<Shared>,
    started: Instant,
}

impl CaptureSession for CpalSession {
    fn stop(self: Box<Self>) -> Result<CaptureOutput> {
        let _ = self.stop_tx.send(());
        self.done_rx
            .recv()
            .map_err(|_| CaptureError::Backend("capture thread died before finishing".into()))?
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn warnings(&self) -> Vec<CaptureWarning> {
        self.shared.warnings()
    }

    fn levels(&self) -> (f32, f32) {
        (
            f32::from_bits(self.shared.mic_level.load(Ordering::Relaxed)),
            f32::from_bits(self.shared.system_level.load(Ordering::Relaxed)),
        )
    }

    fn system_mode(&self) -> Option<SystemCaptureMode> {
        self.shared.system_mode()
    }
}

/// What keeps a source alive: a cpal stream, or the process-loopback thread.
enum Stream {
    Cpal(#[allow(dead_code)] cpal::Stream),
    #[cfg(target_os = "windows")]
    ProcessLoopback(#[allow(dead_code)] super::win_process_loopback::ProcessLoopback),
}

/// An open source feeding a track.
struct Input {
    feed: SourceFeed,
    _stream: Stream,
    /// Set when the input follows the system default device: the default's
    /// ID at open time, compared on every poll to detect a switch.
    default_id: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TrackKind {
    Mic,
    System,
}

struct Track {
    kind: TrackKind,
    writer: TrackWriter,
    input: Option<Input>,
    last_data: Instant,
    next_reopen: Instant,
    /// Windows: the process-loopback mode failed at runtime; reopen with
    /// endpoint loopback instead.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    process_loopback_failed: bool,
    /// Xruns of the inputs already detached, so `finish` reports the
    /// track's total across reopens.
    xruns: u64,
}

impl Track {
    fn new(kind: TrackKind, writer: TrackWriter, input: Input) -> Self {
        let now = Instant::now();
        let mut track = Self {
            kind,
            writer,
            input: None,
            last_data: now,
            next_reopen: now,
            process_loopback_failed: false,
            xruns: 0,
        };
        track.attach(input);
        track
    }

    fn attach(&mut self, input: Input) {
        self.writer
            .set_input_format(input.feed.channels, input.feed.sample_rate);
        self.input = Some(input);
        self.last_data = Instant::now();
    }

    fn detach(&mut self, why: &str, shared: &Shared) {
        if let Some(input) = self.input.take() {
            self.xruns += input.feed.stats.xruns.load(Ordering::Relaxed);
            log::warn!(
                "meeting capture: {:?} track lost its device ({why}); reopening",
                self.kind
            );
            shared.add_warning(CaptureWarning::DeviceChanged);
        }
        self.next_reopen = Instant::now();
    }

    fn drain(&mut self, scratch: &mut Vec<f32>) {
        let Some(input) = self.input.as_mut() else {
            return;
        };
        scratch.clear();
        input.feed.drain_into(scratch);
        if input.feed.stats.got_data.swap(false, Ordering::Relaxed) || !scratch.is_empty() {
            self.last_data = Instant::now();
        }
        if !scratch.is_empty() {
            if let Err(e) = self.writer.push_interleaved(scratch) {
                log::error!(
                    "meeting capture: writing {} failed: {e}",
                    self.writer.path().display()
                );
            }
        }
    }

    /// One tick: drain, detect failures, reopen, pad.
    fn service(
        &mut self,
        host: &cpal::Host,
        cfg: &CaptureConfig,
        shared: &Shared,
        target_frames: u64,
        scratch: &mut Vec<f32>,
    ) {
        self.drain(scratch);
        let now = Instant::now();

        if let Some(input) = &self.input {
            if input.feed.stats.failed.load(Ordering::Relaxed) {
                #[cfg(target_os = "windows")]
                if matches!(input._stream, Stream::ProcessLoopback(_)) {
                    self.process_loopback_failed = true;
                }
                self.detach("stream error", shared);
            } else if self.kind == TrackKind::Mic && now - self.last_data > MIC_STALL {
                self.detach("no data", shared);
            }
        }

        if self.input.is_none() && now >= self.next_reopen {
            let reopened = match self.kind {
                TrackKind::Mic => open_mic(host, cfg.mic_device.as_deref()),
                TrackKind::System => {
                    open_system(host, self.process_loopback_failed).map(|(input, mode)| {
                        *shared.system_mode.lock().unwrap() = Some(mode);
                        input
                    })
                }
            };
            match reopened {
                Ok(input) => {
                    log::info!("meeting capture: {:?} track reopened", self.kind);
                    self.attach(input);
                }
                Err(e) => {
                    log::debug!("meeting capture: reopening {:?} failed: {e}", self.kind);
                    self.next_reopen = now + REOPEN_RETRY;
                }
            }
        }

        // Keep an idle source on the clock (loopback delivers nothing while
        // nothing plays; a lost device delivers nothing until reopened).
        if now - self.last_data > IDLE_GAP {
            if let Err(e) = self.writer.pad_to(target_frames) {
                log::error!(
                    "meeting capture: padding {} failed: {e}",
                    self.writer.path().display()
                );
            }
        }
    }

    /// Reopen the input if it follows the default device and that changed.
    fn check_default_device(&mut self, host: &cpal::Host, shared: &Shared) {
        let Some(opened) = self.input.as_ref().and_then(|i| i.default_id.clone()) else {
            return;
        };
        let current = match self.kind {
            TrackKind::Mic => host.default_input_device(),
            TrackKind::System => host.default_output_device(),
        }
        .and_then(|d| device_id(&d));
        if current.is_some_and(|c| c != opened) {
            self.detach("default device changed", shared);
        }
    }
}

/// Whether a stream error means the stream is dead and must be reopened.
/// The rest leave it running: an xrun is a glitch, `DeviceChanged` means cpal
/// already rerouted the stream, and `RealtimeDenied` only costs latency.
/// Reopening on those made each new stream xrun again, in a loop.
fn is_fatal(kind: cpal::ErrorKind) -> bool {
    !matches!(
        kind,
        cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied
    )
}

/// Build an input stream on `device` whose callback only copies samples
/// into a ring buffer.
fn build_input(
    device: &cpal::Device,
    config: cpal::SupportedStreamConfig,
    label: &'static str,
) -> std::result::Result<(cpal::Stream, SourceFeed), String> {
    let stream_config: cpal::StreamConfig = config.config();
    let (tap, feed) = source::channel(stream_config.channels, stream_config.sample_rate);
    let stats = tap.stats().clone();
    let err_fn = move |e: cpal::Error| match e.kind() {
        // Can fire on every buffer: counted, and reported once by `finish`.
        cpal::ErrorKind::Xrun => {
            stats.xruns.fetch_add(1, Ordering::Relaxed);
        }
        kind if is_fatal(kind) => {
            log::warn!("meeting capture: {label} stream error: {e}");
            stats.failed.store(true, Ordering::Relaxed);
        }
        _ => log::info!("meeting capture: {label} stream notice: {e}"),
    };

    macro_rules! build_for {
        ($t:ty) => {{
            let mut tap = tap;
            device.build_input_stream(
                stream_config.clone(),
                move |data: &[$t], _: &cpal::InputCallbackInfo| {
                    tap.push(
                        data.len(),
                        data.iter()
                            .map(|&s| <f32 as cpal::FromSample<$t>>::from_sample_(s)),
                    )
                },
                err_fn,
                None,
            )
        }};
    }

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build_for!(f32),
        cpal::SampleFormat::I16 => build_for!(i16),
        cpal::SampleFormat::U16 => build_for!(u16),
        cpal::SampleFormat::I32 => build_for!(i32),
        other => return Err(format!("unsupported sample format {other:?}")),
    }
    .map_err(|e| format!("build {label} stream: {e}"))?;
    stream
        .play()
        .map_err(|e| format!("start {label} stream: {e}"))?;
    Ok((stream, feed))
}

/// Open the named microphone, or the default one when unnamed or missing
/// (same fallback as dictation's `AudioRecordingManager`).
fn open_mic(host: &cpal::Host, name: Option<&str>) -> std::result::Result<Input, String> {
    let named = match name {
        Some(name) => host
            .input_devices()
            .map_err(|e| format!("list microphones: {e}"))?
            .find(|d| device_name(d).as_deref() == Some(name)),
        None => None,
    };
    if let (Some(name), None) = (name, &named) {
        log::warn!("meeting capture: microphone '{name}' not found, using the default");
    }
    let follows_default = named.is_none();
    let device = match named {
        Some(d) => d,
        None => host.default_input_device().ok_or("no default microphone")?,
    };
    let config = device
        .default_input_config()
        .map_err(|e| format!("microphone config: {e}"))?;
    log::info!(
        "meeting capture: microphone '{}' ({} ch, {} Hz)",
        device_name(&device).unwrap_or_default(),
        config.channels(),
        config.sample_rate()
    );
    let (stream, feed) = build_input(&device, config, "microphone")?;
    Ok(Input {
        feed,
        _stream: Stream::Cpal(stream),
        default_id: if follows_default {
            device_id(&device)
        } else {
            None
        },
    })
}

/// Open the system-audio source with the best mode this platform offers.
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
fn open_system(
    host: &cpal::Host,
    process_loopback_failed: bool,
) -> std::result::Result<(Input, SystemCaptureMode), String> {
    #[cfg(target_os = "windows")]
    {
        use super::win_process_loopback as process;
        if !process_loopback_failed && process::supported() {
            let (tap, feed) = source::channel(process::CHANNELS, process::SAMPLE_RATE);
            match process::ProcessLoopback::start(tap) {
                Ok(capture) => {
                    return Ok((
                        Input {
                            feed,
                            _stream: Stream::ProcessLoopback(capture),
                            default_id: None,
                        },
                        SystemCaptureMode::ProcessLoopback,
                    ));
                }
                Err(e) => log::warn!(
                    "meeting capture: process loopback unavailable ({e}), \
                     falling back to endpoint loopback"
                ),
            }
        }
        open_endpoint_loopback(host).map(|input| (input, SystemCaptureMode::EndpointLoopback))
    }
    #[cfg(target_os = "macos")]
    {
        if !super::system_capture_supported() {
            return Err("system audio capture requires macOS 14.6 or later".into());
        }
        open_endpoint_loopback(host).map(|input| (input, SystemCaptureMode::CoreAudioTap))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Err("system audio capture is not supported on this platform".into())
    }
}

/// Loopback of the default output: WASAPI on Windows, Core Audio process tap
/// on macOS 14.6+. Includes Trigo Whispr's own sounds. CPAL owns and destroys
/// the private tap/aggregate device when the stream is dropped.
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn open_endpoint_loopback(host: &cpal::Host) -> std::result::Result<Input, String> {
    let device = host
        .default_output_device()
        .ok_or("no default output device")?;
    // CPAL chooses ordinary microphone input for duplex devices instead of
    // creating a process tap. Never silently label that as system audio.
    #[cfg(target_os = "macos")]
    if device.supports_input() {
        return Err(
            "system audio loopback requires an output-only device; select the Mac speakers or an output-only device in Sound settings".into(),
        );
    }
    // Loopback uses the output device's render format for an input stream.
    let config = device
        .default_output_config()
        .map_err(|e| format!("output config: {e}"))?;
    log::info!(
        "meeting capture: endpoint loopback of '{}' ({} ch, {} Hz)",
        device_name(&device).unwrap_or_default(),
        config.channels(),
        config.sample_rate()
    );
    let (stream, feed) = build_input(&device, config, "loopback")?;
    Ok(Input {
        feed,
        _stream: Stream::Cpal(stream),
        default_id: device_id(&device),
    })
}

/// Windows privacy setting "Let desktop apps access your microphone".
fn microphone_access_denied() -> bool {
    #[cfg(target_os = "windows")]
    {
        use crate::commands::audio::{get_windows_microphone_permission_status, PermissionAccess};
        get_windows_microphone_permission_status().overall_access == PermissionAccess::Denied
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

fn create_track(
    kind: TrackKind,
    path: &Path,
    input: Input,
) -> std::result::Result<Track, CaptureError> {
    let writer = TrackWriter::create(path)?;
    Ok(Track::new(kind, writer, input))
}

/// COM for the volume polling on the capture thread (cpal copes with an
/// already-initialized apartment).
#[cfg(target_os = "windows")]
struct ComGuard(bool);

#[cfg(target_os = "windows")]
impl ComGuard {
    fn init() -> Self {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        Self(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

#[cfg(target_os = "windows")]
impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

fn capture_thread(
    cfg: CaptureConfig,
    shared: Arc<Shared>,
    stop_rx: Receiver<()>,
    ready_tx: Sender<Result<Instant>>,
    done_tx: Sender<Result<CaptureOutput>>,
) {
    #[cfg(target_os = "windows")]
    let _com = ComGuard::init();

    // The shared clock: both tracks are aligned on time since this instant.
    let started = Instant::now();
    let host = cpal::default_host();

    // --- microphone track ---
    let mut errors = Vec::new();
    let mic = if microphone_access_denied() {
        errors.push("microphone access denied in Windows privacy settings".to_string());
        None
    } else {
        match open_mic(&host, cfg.mic_device.as_deref()) {
            Ok(input) => {
                match create_track(TrackKind::Mic, &cfg.out_dir.join(MIC_FILE_NAME), input) {
                    Ok(track) => Some(track),
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                }
            }
            Err(e) => {
                errors.push(e);
                None
            }
        }
    };
    if mic.is_none() {
        log::warn!(
            "meeting capture: microphone unavailable ({}), recording system audio only",
            errors.join("; ")
        );
        shared.add_warning(CaptureWarning::MicUnavailable);
    }

    // --- system track (best effort; the degradation is reported) ---
    let system = if cfg.capture_system {
        match open_system(&host, false) {
            Ok((input, mode)) => {
                log::info!("meeting capture: system audio via {mode:?}");
                *shared.system_mode.lock().unwrap() = Some(mode);
                match create_track(
                    TrackKind::System,
                    &cfg.out_dir.join(SYSTEM_FILE_NAME),
                    input,
                ) {
                    Ok(track) => Some(track),
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                }
            }
            Err(e) => {
                log::warn!("meeting capture: system audio unavailable ({e}), microphone only");
                errors.push(e);
                shared.add_warning(CaptureWarning::SystemAudioUnavailable);
                None
            }
        }
    } else {
        None
    };

    let mut tracks: Vec<Track> = mic.into_iter().chain(system).collect();
    if tracks.is_empty() {
        let _ = ready_tx.send(Err(CaptureError::NoInput(errors.join("; "))));
        return;
    }
    let _ = ready_tx.send(Ok(started));

    let mut scratch = Vec::new();
    let mut last_flush = Instant::now();
    let mut last_poll = Instant::now();
    // Runs until a stop is requested or the session handle is dropped.
    while let Err(RecvTimeoutError::Timeout) = stop_rx.recv_timeout(TICK) {
        let elapsed = started.elapsed();
        let target = frames_for_ms(elapsed.as_millis() as u64);
        for track in &mut tracks {
            track.service(&host, &cfg, &shared, target, &mut scratch);
        }

        if last_poll.elapsed() >= POLL_EVERY {
            last_poll = Instant::now();
            for track in &mut tracks {
                track.check_default_device(&host, &shared);
            }
            #[cfg(target_os = "windows")]
            if cfg.capture_system {
                shared
                    .output_muted
                    .store(super::win_volume::output_muted(), Ordering::Relaxed);
            }
        }

        if last_flush.elapsed() >= FLUSH_EVERY {
            last_flush = Instant::now();
            for track in &mut tracks {
                if let Err(e) = track.writer.flush() {
                    log::error!(
                        "meeting capture: flushing {} failed: {e}",
                        track.writer.path().display()
                    );
                }
            }
        }

        for track in &tracks {
            let level = track.writer.level().to_bits();
            match track.kind {
                TrackKind::Mic => shared.mic_level.store(level, Ordering::Relaxed),
                TrackKind::System => {
                    shared.system_level.store(level, Ordering::Relaxed);
                    shared.system_silent.store(
                        elapsed >= SILENT_AFTER && !track.writer.heard_signal(),
                        Ordering::Relaxed,
                    );
                }
            }
        }
    }

    let _ = done_tx.send(finish(tracks, started, &shared));
}

/// Stop the streams, write what they still buffered, pad every track to the
/// same length and finalize the files.
fn finish(mut tracks: Vec<Track>, started: Instant, shared: &Shared) -> Result<CaptureOutput> {
    let elapsed_frames = frames_for_ms(started.elapsed().as_millis() as u64);
    let mut scratch = Vec::new();
    for track in &mut tracks {
        // Drop the stream first so no packet lands after the final drain.
        if let Some(input) = track.input.take() {
            let Input {
                mut feed, _stream, ..
            } = input;
            drop(_stream);
            track.xruns += feed.stats.xruns.load(Ordering::Relaxed);
            let dropped = feed.stats.dropped.load(Ordering::Relaxed);
            if dropped > 0 {
                log::warn!(
                    "meeting capture: {:?} track dropped {dropped} samples (writer too slow)",
                    track.kind
                );
            }
            scratch.clear();
            feed.drain_into(&mut scratch);
            if let Err(e) = track.writer.push_interleaved(&scratch) {
                log::error!("meeting capture: final write failed: {e}");
            }
        }
        if track.xruns > 0 {
            log::warn!(
                "meeting capture: {:?} track had {} buffer under/overruns",
                track.kind,
                track.xruns
            );
        }
    }

    let target = tracks
        .iter()
        .map(|t| t.writer.frames())
        .fold(elapsed_frames, u64::max);
    let mut mic_path: Option<PathBuf> = None;
    let mut system_path: Option<PathBuf> = None;
    let mut first_error = None;
    for mut track in tracks {
        let path = track.writer.path().to_path_buf();
        let result = track
            .writer
            .pad_to(target)
            .and_then(|_| track.writer.finalize());
        if let Err(e) = result {
            log::error!("meeting capture: finalizing {} failed: {e}", path.display());
            first_error.get_or_insert(e);
        }
        match track.kind {
            TrackKind::Mic => mic_path = Some(path),
            TrackKind::System => system_path = Some(path),
        }
    }
    if let Some(e) = first_error {
        return Err(CaptureError::Io(e));
    }

    let duration_ms = target * 1000 / TRACK_SAMPLE_RATE as u64;
    log::info!("meeting capture: stopped after {duration_ms} ms");
    Ok(CaptureOutput {
        mic_path,
        system_path,
        duration_ms,
        system_mode: shared.system_mode(),
        warnings: shared.warnings(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glitches_and_reroutes_keep_the_stream() {
        assert!(!is_fatal(cpal::ErrorKind::Xrun));
        assert!(!is_fatal(cpal::ErrorKind::DeviceChanged));
        assert!(!is_fatal(cpal::ErrorKind::RealtimeDenied));
    }

    #[test]
    fn lost_or_broken_streams_are_reopened() {
        for kind in [
            cpal::ErrorKind::DeviceNotAvailable,
            cpal::ErrorKind::StreamInvalidated,
            cpal::ErrorKind::BackendError,
            cpal::ErrorKind::DeviceBusy,
            cpal::ErrorKind::Other,
        ] {
            assert!(is_fatal(kind), "{kind:?} should be fatal");
        }
    }
}
