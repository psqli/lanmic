//! The two sessions, cpal where the phone has Oboe.
//!
//! This is `rust/src/android.rs` with the streams swapped and nothing else
//! moved: the same [`lanmic::supervisor::supervise`] lifecycle, the same
//! network and sender threads, the same `*Shared` atomics, the same HELLO on
//! start and BYE on stop. What differs is only what a desktop has and a phone
//! does not - a device to choose, several output channels, and no notion of an
//! input preset.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle, Thread};
use std::time::Duration;

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{ErrorKind, SampleFormat};

use lanmic::mixer::{Mixer, Table};
use lanmic::protocol::{PacketType, DEFAULT_AUDIO_PORT, DISCOVERY_PORT, SAMPLE_RATE};
use lanmic::receiver::{self, RxShared, RxStats};
use lanmic::supervisor::supervise;
use lanmic::transmitter::{self, CaptureEncoder, TxShared, TxStats};
use lanmic::util;

use crate::audio::{self, Chosen, Direction};
use crate::discovery;

/// How long the sender thread sleeps if the audio callback never wakes it.
/// Same value, same reason, as on the phone.
const SENDER_PARK: Duration = Duration::from_millis(100);
/// cpal waits this long for a backend to bring a stream up before giving up,
/// rather than blocking a start forever on a wedged sound server.
const OPEN_TIMEOUT: Duration = Duration::from_secs(2);

/// 5 ms at 48 kHz: the packet size the whole system is tuned around.
pub const DEFAULT_BLOCK_FRAMES: u32 = 240;

fn cpal_err(what: &str, e: cpal::Error) -> io::Error {
    io::Error::other(format!("{what}: {e}"))
}

/// How an error from a live stream should be treated. cpal distinguishes the
/// glitch that is worth a counter from the failure that needs a new stream,
/// which Oboe does not, so the desktop side can be less blunt than the phone's
/// "any error closes the stream".
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Fault {
    /// A dropout. Count it and carry on; the stream is still good.
    Xrun,
    /// The stream is finished: reopen it.
    Reopen,
    /// Worth a line in the log and nothing else.
    Note,
}

fn classify(kind: ErrorKind) -> Fault {
    match kind {
        ErrorKind::Xrun => Fault::Xrun,
        // The backend followed the default device somewhere else and the stream
        // is still live; rebuilding it would be a dropout for no reason.
        ErrorKind::DeviceChanged => Fault::Note,
        // Audio still plays, it is just not promoted to a realtime thread.
        ErrorKind::RealtimeDenied => Fault::Note,
        _ => Fault::Reopen,
    }
}

/// Applies a stream error to the counters both sessions publish. `xruns` and
/// `restart` are the same atomics the Oboe callbacks set on the phone.
fn record_fault(what: &str, e: &cpal::Error, xruns: &Counter, restart: &AtomicBool) {
    match classify(e.kind()) {
        Fault::Xrun => {
            xruns.bump();
        }
        Fault::Note => log::info!("{what}: {e}"),
        Fault::Reopen => {
            log::warn!("{what} closed on error: {e}");
            restart.store(true, Ordering::Release);
        }
    }
}

/// A running total the error callback owns. The engine's `*Shared` structs hold
/// xruns as a value to be set rather than incremented - which is right for
/// Oboe, where the stream is asked for its total - so the count is kept here
/// and pushed through `set_xruns`.
#[derive(Debug, Default)]
struct Counter(std::sync::atomic::AtomicU32);

impl Counter {
    fn bump(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn get(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Milliseconds between a callback and the audio it writes reaching the DAC -
/// the desktop's answer to Oboe's `calculate_latency_millis`.
fn output_latency_ms(info: &cpal::OutputCallbackInfo) -> f32 {
    let t = info.timestamp();
    t.playback
        .checked_duration_since(t.callback)
        .map(|d| d.as_secs_f32() * 1000.0)
        .unwrap_or(0.0)
}

fn input_latency_ms(info: &cpal::InputCallbackInfo) -> f32 {
    let t = info.timestamp();
    t.callback
        .checked_duration_since(t.capture)
        .map(|d| d.as_secs_f32() * 1000.0)
        .unwrap_or(0.0)
}

// ---------------------------------------------------------------------------
// Mixer / server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub discovery_port: u16,
    pub jitter_ms: i32,
    pub name: String,
    /// `None` means the host's default output.
    pub device: Option<String>,
    pub block_frames: u32,
    pub discovery: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_AUDIO_PORT,
            discovery_port: DISCOVERY_PORT,
            jitter_ms: 15,
            name: default_server_name(),
            device: None,
            block_frames: DEFAULT_BLOCK_FRAMES,
            discovery: true,
        }
    }
}

/// The hostname, so a phone's server list says something a human recognises
/// rather than one more identical entry.
///
/// `HOSTNAME` is not exported by every shell, so `/etc/hostname` is the second
/// try; `COMPUTERNAME` is the Windows spelling. Failing all three is not worth
/// a syscall through `libc::gethostname` - a fixed name still works, it is just
/// less helpful when two of these are on one LAN.
pub fn default_server_name() -> String {
    let clean = |s: String| {
        let trimmed = s.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    };
    std::env::var("HOSTNAME")
        .ok()
        .and_then(clean)
        .or_else(|| std::env::var("COMPUTERNAME").ok().and_then(clean))
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .and_then(clean)
        })
        .unwrap_or_else(|| "LAN Mic desktop".into())
}

/// A live mixer: bound port, one jitter buffer per microphone, one output
/// stream playing the sum.
pub struct Server {
    shared: Arc<RxShared>,
    table: Arc<Table>,
    threads: Vec<JoinHandle<()>>,
    /// The discovery responder's own stop flag. It is not part of `RxShared`
    /// because discovery is optional and knows nothing about audio; `Drop`
    /// clears it alongside `running`.
    discovery_running: Arc<AtomicBool>,
    device_name: String,
    output_channels: usize,
}

impl Server {
    pub fn start(config: &ServerConfig) -> io::Result<Self> {
        let shared = Arc::new(RxShared::default());
        shared.reset_for_session();
        let (mut router, mixer, table) =
            receiver::build(config.port, config.jitter_ms, shared.clone())?;
        let mixer = Arc::new(Mutex::new(mixer));

        // Resolved once so a start fails here, with a name to report, rather
        // than inside the supervisor where it would only be logged.
        let device = audio::open_device(Direction::Output, config.device.as_deref())?;
        let chosen = audio::config_for(&device, Direction::Output, config.block_frames)?;
        let device_name = audio::device_name(&device);
        let output_channels = chosen.channels();
        shared.running.store(true, Ordering::Release);

        // Threads join the session as they are spawned, so an early return from
        // here drops it and stops the ones already running rather than leaving
        // them holding the port.
        let mut engine = Server {
            shared: shared.clone(),
            table,
            threads: Vec::new(),
            discovery_running: Arc::new(AtomicBool::new(true)),
            device_name,
            output_channels,
        };

        engine
            .threads
            .push(thread::Builder::new().name("lau-rx-net".into()).spawn({
                let shared = shared.clone();
                move || {
                    util::raise_thread_priority();
                    while shared.running.load(Ordering::Acquire) {
                        router.poll(util::now_ms());
                    }
                }
            })?);

        if config.discovery {
            match discovery::respond(
                config.discovery_port,
                config.name.clone(),
                engine.discovery_running.clone(),
            ) {
                Ok(handle) => engine.threads.push(handle),
                // Not fatal: anyone told the address by hand is unaffected.
                Err(e) => log::warn!("discovery disabled: {e}"),
            }
        }

        let (ready_tx, ready_rx) = mpsc::channel();
        engine
            .threads
            .push(thread::Builder::new().name("lau-rx-stream".into()).spawn({
                let shared = shared.clone();
                let device_choice = config.device.clone();
                let block_frames = config.block_frames;
                move || {
                    let xruns = Arc::new(Counter::default());
                    supervise(
                        || shared.running.load(Ordering::Acquire),
                        || shared.restart_requested.swap(false, Ordering::AcqRel),
                        || {
                            // Re-resolved on every reopen: the device that came
                            // back may not be the one that went away.
                            let device =
                                audio::open_device(Direction::Output, device_choice.as_deref())?;
                            let chosen =
                                audio::config_for(&device, Direction::Output, block_frames)?;
                            open_output(&device, &chosen, &mixer, &shared, &xruns)
                        },
                        {
                            let shared = shared.clone();
                            let xruns = xruns.clone();
                            move |_: &mut cpal::Stream| shared.set_xruns(xruns.get())
                        },
                        ready_tx,
                    )
                }
            })?);

        match ready_rx.recv() {
            Ok(Ok(())) => {
                log::info!(
                    "server listening on udp/{}, jitter {} ms, out {} ({} ch)",
                    config.port,
                    config.jitter_ms,
                    engine.device_name,
                    engine.output_channels
                );
                Ok(engine)
            }
            Ok(Err(e)) => Err(e),
            // The supervisor died without reporting, which it cannot normally
            // do; treat it as a failed start rather than hanging here forever.
            Err(_) => Err(io::Error::other("stream supervisor never reported")),
        }
        // On both error paths `engine` drops, which stops and joins.
    }

    pub fn table(&self) -> &Arc<Table> {
        &self.table
    }

    pub fn stats(&self) -> RxStats {
        self.shared.stats(&self.table)
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn output_channels(&self) -> usize {
        self.output_channels
    }

    pub fn is_running(&self) -> bool {
        self.shared.running.load(Ordering::Acquire)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::Release);
        self.discovery_running.store(false, Ordering::Release);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        log::info!("server stopped");
    }
}

fn open_output(
    device: &cpal::Device,
    chosen: &Chosen,
    mixer: &Arc<Mutex<Mixer>>,
    shared: &Arc<RxShared>,
    xruns: &Arc<Counter>,
) -> io::Result<cpal::Stream> {
    let channels = chosen.channels();
    let errors = {
        let shared = shared.clone();
        let xruns = xruns.clone();
        move |e: cpal::Error| {
            record_fault("output stream", &e, &xruns, &shared.restart_requested);
        }
    };

    // The callback holds the mixer, so it is released when the stream is
    // dropped; the supervisor's own reference is what keeps it alive across a
    // reopen. `try_lock` can only fail if two streams overlapped across a swap,
    // and a block of silence is the right answer to that.
    macro_rules! stream {
        ($sample:ty, $write:path) => {{
            let mixer = mixer.clone();
            let shared = shared.clone();
            device.build_output_stream(
                chosen.config,
                move |out: &mut [$sample], info: &cpal::OutputCallbackInfo| {
                    shared.set_latency_ms(output_latency_ms(info));
                    match mixer.try_lock() {
                        Ok(mut mixer) => {
                            let frames = out.len() / channels.max(1);
                            let mix = mixer.render(frames);
                            $write(mix, out, channels);
                        }
                        Err(_) => out.fill(Default::default()),
                    }
                },
                errors,
                Some(OPEN_TIMEOUT),
            )
        }};
    }

    let stream = match chosen.format {
        SampleFormat::I16 => stream!(i16, audio::write_mix_i16),
        SampleFormat::F32 => stream!(f32, audio::write_mix_f32),
        other => {
            return Err(io::Error::other(format!(
                "output sample format {other:?} is not one this build converts"
            )))
        }
    }
    .map_err(|e| cpal_err("open output stream", e))?;

    stream
        .play()
        .map_err(|e| cpal_err("start output stream", e))?;
    Ok(stream)
}

// ---------------------------------------------------------------------------
// Microphone
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct MicConfig {
    pub host: String,
    pub port: u16,
    pub frames_per_packet: usize,
    /// `None` means the host's default input.
    pub device: Option<String>,
}

impl Default for MicConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: DEFAULT_AUDIO_PORT,
            frames_per_packet: DEFAULT_BLOCK_FRAMES as usize,
            device: None,
        }
    }
}

/// Capture-side state the input callback reaches, exactly as on the phone: the
/// encoder behind a mutex, and the sender thread's handle so a burst can wake
/// it without a syscall.
struct CaptureSide {
    encoder: Mutex<CaptureEncoder>,
    sender: std::sync::OnceLock<Thread>,
}

/// A live microphone: captures, packetises and ships to one mixer.
pub struct Microphone {
    shared: Arc<TxShared>,
    threads: Vec<JoinHandle<()>>,
    device_name: String,
    input_channels: usize,
    ssrc: u32,
    target: String,
}

impl Microphone {
    pub fn start(config: &MicConfig) -> io::Result<Self> {
        let shared = Arc::new(TxShared::default());
        shared.reset_for_session();

        // Address first, device second. Resolving a name is cheap and a typo in
        // it is the common failure; opening a capture device is neither, and on
        // macOS it is what raises the microphone permission prompt. Nobody
        // should be asked for the microphone only to be told the address was
        // empty.
        let socket = lanmic::net::open_sender(&config.host, config.port)?;

        let device = audio::open_device(Direction::Input, config.device.as_deref())?;
        let chosen = audio::config_for(&device, Direction::Input, config.frames_per_packet as u32)?;
        let device_name = audio::device_name(&device);
        let input_channels = chosen.channels();
        let (encoder, mut packetiser) =
            transmitter::build(socket, config.frames_per_packet, shared.clone());
        let capture = Arc::new(CaptureSide {
            encoder: Mutex::new(encoder),
            sender: std::sync::OnceLock::new(),
        });

        // Sent before the sender thread exists: seq and timestamp belong to
        // that thread once it is running, and reading them here would race.
        packetiser.send_control(PacketType::Hello, 3);
        let ssrc = packetiser.ssrc();
        shared.running.store(true, Ordering::Release);

        let mut engine = Microphone {
            shared: shared.clone(),
            threads: Vec::new(),
            device_name,
            input_channels,
            ssrc,
            target: format!("{}:{}", config.host, config.port),
        };

        engine
            .threads
            .push(thread::Builder::new().name("lau-tx-send".into()).spawn({
                let shared = shared.clone();
                let capture = capture.clone();
                move || {
                    util::raise_thread_priority();
                    // Publishing this is what lets the audio callback wake us.
                    let _ = capture.sender.set(thread::current());
                    while shared.running.load(Ordering::Acquire) {
                        packetiser.pump();
                        // A timeout as well as the wake-up, so a stream that has
                        // stopped calling back cannot park this thread forever.
                        thread::park_timeout(SENDER_PARK);
                    }
                    // The session is down; say goodbye so the mixer drops the
                    // strip now rather than after the two-second timeout.
                    packetiser.send_control(PacketType::Bye, 3);
                }
            })?);

        let (ready_tx, ready_rx) = mpsc::channel();
        engine
            .threads
            .push(thread::Builder::new().name("lau-tx-stream".into()).spawn({
                let shared = shared.clone();
                let device_choice = config.device.clone();
                let block_frames = config.frames_per_packet as u32;
                move || {
                    let xruns = Arc::new(Counter::default());
                    supervise(
                        || shared.running.load(Ordering::Acquire),
                        || shared.restart_requested.swap(false, Ordering::AcqRel),
                        || {
                            let device =
                                audio::open_device(Direction::Input, device_choice.as_deref())?;
                            let chosen =
                                audio::config_for(&device, Direction::Input, block_frames)?;
                            open_input(&device, &chosen, &capture, &shared, &xruns)
                        },
                        {
                            let shared = shared.clone();
                            let xruns = xruns.clone();
                            move |_: &mut cpal::Stream| shared.set_xruns(xruns.get())
                        },
                        ready_tx,
                    )
                }
            })?);

        match ready_rx.recv() {
            Ok(Ok(())) => {
                log::info!(
                    "microphone up -> {}, ssrc {ssrc:08x}, in {} ({} ch)",
                    engine.target,
                    engine.device_name,
                    engine.input_channels
                );
                Ok(engine)
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(io::Error::other("stream supervisor never reported")),
        }
    }

    pub fn shared(&self) -> &Arc<TxShared> {
        &self.shared
    }

    pub fn stats(&self) -> TxStats {
        self.shared.stats()
    }

    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn input_channels(&self) -> usize {
        self.input_channels
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn is_running(&self) -> bool {
        self.shared.running.load(Ordering::Acquire)
    }
}

impl Drop for Microphone {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::Release);
        for t in self.threads.drain(..) {
            t.thread().unpark(); // in case the sender is parked
            let _ = t.join();
        }
        log::info!("microphone stopped");
    }
}

fn open_input(
    device: &cpal::Device,
    chosen: &Chosen,
    capture: &Arc<CaptureSide>,
    shared: &Arc<TxShared>,
    xruns: &Arc<Counter>,
) -> io::Result<cpal::Stream> {
    let channels = chosen.channels();
    let errors = {
        let shared = shared.clone();
        let xruns = xruns.clone();
        move |e: cpal::Error| {
            record_fault("input stream", &e, &xruns, &shared.restart_requested);
        }
    };

    let stream = match chosen.format {
        SampleFormat::I16 => {
            let capture = capture.clone();
            let shared = shared.clone();
            device.build_input_stream(
                chosen.config,
                move |input: &[i16], info: &cpal::InputCallbackInfo| {
                    shared.set_latency_ms(input_latency_ms(info));
                    if let Ok(mut encoder) = capture.encoder.try_lock() {
                        encoder.push(input, channels);
                    }
                    if let Some(sender) = capture.sender.get() {
                        sender.unpark();
                    }
                },
                errors,
                Some(OPEN_TIMEOUT),
            )
        }
        SampleFormat::F32 => {
            let capture = capture.clone();
            let shared = shared.clone();
            // Sized once, here, so the callback never allocates.
            let mut scratch = Vec::with_capacity(SAMPLE_RATE as usize / 4);
            device.build_input_stream(
                chosen.config,
                move |input: &[f32], info: &cpal::InputCallbackInfo| {
                    shared.set_latency_ms(input_latency_ms(info));
                    audio::capture_f32_to_i16(input, &mut scratch);
                    if let Ok(mut encoder) = capture.encoder.try_lock() {
                        encoder.push(&scratch, channels);
                    }
                    if let Some(sender) = capture.sender.get() {
                        sender.unpark();
                    }
                },
                errors,
                Some(OPEN_TIMEOUT),
            )
        }
        other => {
            return Err(io::Error::other(format!(
                "input sample format {other:?} is not one this build converts"
            )))
        }
    }
    .map_err(|e| cpal_err("open input stream", e))?;

    stream
        .play()
        .map_err(|e| cpal_err("start input stream", e))?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropout_is_counted_and_a_dead_device_is_reopened() {
        let xruns = Counter::default();
        let restart = AtomicBool::new(false);

        record_fault("out", &cpal::Error::new(ErrorKind::Xrun), &xruns, &restart);
        record_fault("out", &cpal::Error::new(ErrorKind::Xrun), &xruns, &restart);
        assert_eq!(xruns.get(), 2);
        assert!(!restart.load(Ordering::Acquire), "an xrun is not a restart");

        record_fault(
            "out",
            &cpal::Error::new(ErrorKind::DeviceNotAvailable),
            &xruns,
            &restart,
        );
        assert!(restart.load(Ordering::Acquire));
        assert_eq!(xruns.get(), 2);
    }

    #[test]
    fn a_reroute_or_a_refused_promotion_leaves_the_stream_alone() {
        for kind in [ErrorKind::DeviceChanged, ErrorKind::RealtimeDenied] {
            let xruns = Counter::default();
            let restart = AtomicBool::new(false);
            record_fault("out", &cpal::Error::new(kind), &xruns, &restart);
            assert!(!restart.load(Ordering::Acquire), "{kind:?} forced a reopen");
            assert_eq!(xruns.get(), 0, "{kind:?} was counted as a dropout");
        }
    }

    #[test]
    fn every_other_failure_asks_for_a_new_stream() {
        for kind in [
            ErrorKind::StreamInvalidated,
            ErrorKind::DeviceBusy,
            ErrorKind::BackendError,
            ErrorKind::Other,
        ] {
            assert_eq!(classify(kind), Fault::Reopen, "{kind:?}");
        }
    }

    #[test]
    fn a_microphone_with_no_server_address_fails_before_touching_the_device() {
        let started = Microphone::start(&MicConfig {
            host: String::new(),
            ..Default::default()
        });
        // `Microphone` has no `Debug` - it owns join handles and a socket - so
        // this unwraps the error rather than the session.
        let Err(e) = started else {
            panic!("a microphone with no address should not have started");
        };
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    }
}
