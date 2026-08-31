//! Oboe streams and the threads around them. The only part of the engine that
//! a phone is required to run.
//!
//! One rule shapes this whole module: **exactly one thread ever creates,
//! starts or destroys an audio stream.** The supervisor owns its stream from
//! `start` to `stop`, including reopening it after a device change. Nothing
//! else can install a stream, so two live streams - two producers on a
//! single-producer ring - are not a race to be guarded against but a state
//! that cannot be constructed.

use std::io;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread::{self, JoinHandle, Thread};
use std::time::Duration;

use oboe::{
    AudioInputCallback, AudioInputStreamSafe, AudioOutputCallback, AudioOutputStreamSafe,
    AudioStream, AudioStreamAsync, AudioStreamBuilder, AudioStreamSafe, ContentType,
    DataCallbackResult, Error as OboeError, Input, InputPreset, Mono, Output, PerformanceMode,
    SampleRateConversionQuality, SharingMode, Stereo, Usage,
};

use crate::mixer::{Mixer, Table};
use crate::protocol::{PacketType, SAMPLE_RATE};
use crate::receiver::{self, RxShared, RxStats};
use crate::transmitter::{self, CaptureEncoder, TxShared, TxStats};
use crate::util;

/// How often the supervisor wakes to refresh stats and notice a dead stream.
const SUPERVISOR_TICK: Duration = Duration::from_millis(200);
/// Backoff ceiling for reopening a stream that will not come back.
const MAX_REOPEN_BACKOFF: Duration = Duration::from_millis(3200);
/// How long the sender thread sleeps if the audio callback never wakes it.
const SENDER_PARK: Duration = Duration::from_millis(100);

fn oboe_err(what: &str, e: OboeError) -> io::Error {
    io::Error::other(format!("{what}: {e:?}"))
}

// ---------------------------------------------------------------------------
// Why the audio-thread state sits behind a Mutex
//
// `oboe` takes the callback by value and boxes it, and never reclaims that box
// when the stream is dropped. So the callback cannot own the mixer or the
// capture encoder: every stream we opened would keep one alive for the life of
// the process. It holds a `Weak` instead, and the strong reference stays with
// the supervisor.
//
// That leaves needing `&mut` through a shared reference, hence the Mutex. It is
// only ever locked by the audio thread - the supervisor never takes it - so the
// `try_lock` below is an uncontended atomic, never a wait. Should it ever fail,
// which would mean two callbacks overlapping across a stream swap, a block of
// silence is the correct answer and the one this returns.
// ---------------------------------------------------------------------------

struct OutputCallback {
    mixer: Weak<Mutex<Mixer>>,
    shared: Weak<RxShared>,
}

impl AudioOutputCallback for OutputCallback {
    type FrameType = (i16, Stereo);

    fn on_audio_ready(
        &mut self,
        _stream: &mut dyn AudioOutputStreamSafe,
        frames: &mut [(i16, i16)],
    ) -> DataCallbackResult {
        let Some(mixer) = self.mixer.upgrade() else {
            // The engine is gone; this is a callback from a stream that is on
            // its way out.
            frames.fill((0, 0));
            return DataCallbackResult::Stop;
        };
        match mixer.try_lock() {
            Ok(mut mixer) => {
                let mix = mixer.render(frames.len());
                receiver::write_stereo(mix, frames);
            }
            Err(_) => frames.fill((0, 0)),
        }
        DataCallbackResult::Continue
    }

    fn on_error_after_close(&mut self, _stream: &mut dyn AudioOutputStreamSafe, e: OboeError) {
        log::warn!("output stream closed on error: {e:?}");
        if let Some(shared) = self.shared.upgrade() {
            shared.restart_requested.store(true, Ordering::Release);
        }
    }
}

/// Capture-side state the input callback reaches through a `Weak`.
struct CaptureSide {
    encoder: Mutex<CaptureEncoder>,
    /// Set once by the sender thread; the callback pokes it after each burst.
    sender: OnceLock<Thread>,
}

struct InputCallback {
    capture: Weak<CaptureSide>,
    shared: Weak<TxShared>,
}

impl AudioInputCallback for InputCallback {
    type FrameType = (i16, Mono);

    fn on_audio_ready(
        &mut self,
        _stream: &mut dyn AudioInputStreamSafe,
        frames: &[i16],
    ) -> DataCallbackResult {
        let Some(capture) = self.capture.upgrade() else {
            return DataCallbackResult::Stop;
        };
        if let Ok(mut encoder) = capture.encoder.try_lock() {
            // The stream is opened as mono, so a frame is a sample.
            encoder.push(frames, 1);
        }
        // Wait-free wake-up, the same job the POSIX semaphore did before.
        if let Some(sender) = capture.sender.get() {
            sender.unpark();
        }
        DataCallbackResult::Continue
    }

    fn on_error_after_close(&mut self, _stream: &mut dyn AudioInputStreamSafe, e: OboeError) {
        log::warn!("input stream closed on error: {e:?}");
        if let Some(shared) = self.shared.upgrade() {
            shared.restart_requested.store(true, Ordering::Release);
        }
    }
}

type OutputStream = AudioStreamAsync<Output, OutputCallback>;
type InputStream = AudioStreamAsync<Input, InputCallback>;

fn open_output(shared: &Arc<RxShared>, mixer: &Arc<Mutex<Mixer>>) -> io::Result<OutputStream> {
    let mut stream = AudioStreamBuilder::default()
        .set_performance_mode(PerformanceMode::LowLatency)
        .set_sharing_mode(SharingMode::Exclusive)
        .set_sample_rate(SAMPLE_RATE as i32)
        .set_sample_rate_conversion_quality(SampleRateConversionQuality::Medium)
        .set_format_conversion_allowed(true)
        .set_channel_conversion_allowed(true)
        .set_usage(Usage::Media)
        .set_content_type(ContentType::Speech)
        .set_format::<i16>()
        .set_stereo()
        .set_callback(OutputCallback {
            mixer: Arc::downgrade(mixer),
            shared: Arc::downgrade(shared),
        })
        .open_stream()
        .map_err(|e| oboe_err("open output stream", e))?;

    // Two bursts is the sweet spot: one is glitchy on most devices, three adds
    // a burst of latency for nothing.
    let burst = stream.get_frames_per_burst();
    let _ = stream.set_buffer_size_in_frames(burst * 2);
    stream
        .start()
        .map_err(|e| oboe_err("start output stream", e))?;
    log::info!("output stream up: burst {burst} frames");
    Ok(stream)
}

fn open_input(
    shared: &Arc<TxShared>,
    capture: &Arc<CaptureSide>,
    preset: InputPreset,
) -> io::Result<InputStream> {
    let mut stream = AudioStreamBuilder::default()
        .set_performance_mode(PerformanceMode::LowLatency)
        .set_sharing_mode(SharingMode::Exclusive)
        .set_sample_rate(SAMPLE_RATE as i32)
        .set_sample_rate_conversion_quality(SampleRateConversionQuality::Medium)
        .set_format_conversion_allowed(true)
        .set_channel_conversion_allowed(true)
        .set_input_preset(preset)
        .set_format::<i16>()
        .set_mono()
        .set_input()
        .set_callback(InputCallback {
            capture: Arc::downgrade(capture),
            shared: Arc::downgrade(shared),
        })
        .open_stream()
        .map_err(|e| oboe_err("open input stream", e))?;

    let burst = stream.get_frames_per_burst();
    let _ = stream.set_buffer_size_in_frames(burst * 2);
    stream
        .start()
        .map_err(|e| oboe_err("start input stream", e))?;
    log::info!("input stream up: burst {burst} frames");
    Ok(stream)
}

/// The shape both supervisors share: open once, report whether that worked,
/// then keep the stream alive until asked to stop.
///
/// `open` is called on this thread and nowhere else, which is the whole point.
fn supervise<S, O, P>(
    running: impl Fn() -> bool,
    restart_requested: impl Fn() -> bool,
    mut open: O,
    mut publish: P,
    ready: mpsc::Sender<io::Result<()>>,
) where
    O: FnMut() -> io::Result<S>,
    P: FnMut(&mut S),
{
    let mut stream = match open() {
        Ok(s) => {
            let _ = ready.send(Ok(()));
            Some(s)
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };

    let mut backoff = SUPERVISOR_TICK;
    while running() {
        thread::sleep(SUPERVISOR_TICK);
        if !running() {
            break;
        }

        if let Some(s) = stream.as_mut() {
            publish(s);
        }

        if restart_requested() {
            // Oboe has already stopped and closed it; letting our handle go is
            // all that is left before reopening.
            stream = None;
        }
        if stream.is_none() {
            match open() {
                Ok(s) => {
                    stream = Some(s);
                    backoff = SUPERVISOR_TICK;
                }
                Err(e) => {
                    // The device that took the route away is often still busy
                    // on the first try, and giving up once would leave a live
                    // session with dead audio.
                    log::warn!("stream did not come back: {e}; retrying");
                    thread::sleep(backoff);
                    backoff = (backoff * 2).min(MAX_REOPEN_BACKOFF);
                }
            }
        }
    }
    // Explicit, so the stream is closed - and its callbacks quiesced - before
    // the state they reach through drops.
    drop(stream);
}

/// The mixer end: binds the audio port, sums every source, plays the result.
pub struct Receiver {
    shared: Arc<RxShared>,
    table: Arc<Table>,
    threads: Vec<JoinHandle<()>>,
}

impl Receiver {
    pub fn start(port: u16, jitter_ms: i32) -> io::Result<Self> {
        let shared = Arc::new(RxShared::default());
        shared.reset_for_session();
        let (mut router, mixer, table) = receiver::build(port, jitter_ms, shared.clone())?;
        let mixer = Arc::new(Mutex::new(mixer));
        shared.running.store(true, Ordering::Release);

        // Threads go into the engine as they are spawned, so an early return
        // from here drops it and stops the ones already running rather than
        // leaving them holding the port.
        let mut engine = Receiver {
            shared: shared.clone(),
            table,
            threads: Vec::new(),
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

        let (ready_tx, ready_rx) = mpsc::channel();
        engine
            .threads
            .push(thread::Builder::new().name("lau-rx-stream".into()).spawn({
                let shared = shared.clone();
                let mixer = mixer.clone();
                move || {
                    supervise(
                        || shared.running.load(Ordering::Acquire),
                        || shared.restart_requested.swap(false, Ordering::AcqRel),
                        || open_output(&shared, &mixer),
                        |s: &mut OutputStream| {
                            if let Ok(x) = s.get_xrun_count() {
                                shared.set_xruns(x.max(0) as u32);
                            }
                            if let Ok(l) = s.calculate_latency_millis() {
                                shared.set_latency_ms(l as f32);
                            }
                        },
                        ready_tx,
                    )
                }
            })?);

        match ready_rx.recv() {
            Ok(Ok(())) => {
                log::info!("server listening, jitter target {jitter_ms} ms");
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

    pub fn is_running(&self) -> bool {
        self.shared.running.load(Ordering::Acquire)
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::Release);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        log::info!("server stopped");
    }
}

/// The microphone end: captures, packetises and ships.
pub struct Transmitter {
    shared: Arc<TxShared>,
    threads: Vec<JoinHandle<()>>,
}

impl Transmitter {
    pub fn start(
        host: &str,
        port: u16,
        frames_per_packet: usize,
        input_preset: i32,
    ) -> io::Result<Self> {
        let shared = Arc::new(TxShared::default());
        shared.reset_for_session();

        let socket = crate::net::open_sender(host, port)?;
        let (encoder, mut packetiser) =
            transmitter::build(socket, frames_per_packet, shared.clone());
        let capture = Arc::new(CaptureSide {
            encoder: Mutex::new(encoder),
            sender: OnceLock::new(),
        });

        // Sent before the sender thread exists: seq and timestamp belong to
        // that thread once it is running, and reading them here would race.
        packetiser.send_control(PacketType::Hello, 3);
        let ssrc = packetiser.ssrc();
        shared.running.store(true, Ordering::Release);

        let mut engine = Transmitter {
            shared: shared.clone(),
            threads: Vec::new(),
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
                    // The engine is down; say goodbye so the mixer drops the strip
                    // now rather than after the two-second timeout.
                    packetiser.send_control(PacketType::Bye, 3);
                }
            })?);

        let preset = match input_preset {
            1 => InputPreset::Unprocessed,
            2 => InputPreset::VoiceRecognition,
            _ => InputPreset::VoicePerformance,
        };
        let (ready_tx, ready_rx) = mpsc::channel();
        engine
            .threads
            .push(thread::Builder::new().name("lau-tx-stream".into()).spawn({
                let shared = shared.clone();
                let capture = capture.clone();
                move || {
                    supervise(
                        || shared.running.load(Ordering::Acquire),
                        || shared.restart_requested.swap(false, Ordering::AcqRel),
                        || open_input(&shared, &capture, preset),
                        |s: &mut InputStream| {
                            if let Ok(x) = s.get_xrun_count() {
                                shared.set_xruns(x.max(0) as u32);
                            }
                            if let Ok(l) = s.calculate_latency_millis() {
                                shared.set_latency_ms(l as f32);
                            }
                        },
                        ready_tx,
                    )
                }
            })?);

        match ready_rx.recv() {
            Ok(Ok(())) => {
                log::info!("transmitter up -> {host}:{port}, ssrc {ssrc:08x}");
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

    pub fn is_running(&self) -> bool {
        self.shared.running.load(Ordering::Acquire)
    }
}

impl Drop for Transmitter {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::Release);
        for t in self.threads.drain(..) {
            t.thread().unpark(); // in case the sender is parked
            let _ = t.join();
        }
        log::info!("transmitter stopped");
    }
}
