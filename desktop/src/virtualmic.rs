//! The mix, offered to the rest of this machine as a microphone.
//!
//! A mixer plugged into a PA plays the mix out of a speaker. A mixer on the
//! laptop that is also running the video call wants the same mix to arrive
//! somewhere a *different* program can open it - and no operating system lets
//! one process simply hand another its output as a capture device.
//!
//! On PulseAudio and PipeWire one thing comes close: `module-pipe-source`
//! creates a real source - a microphone, as far as every other program is
//! concerned - that reads raw PCM out of a FIFO. So that is what this is:
//!
//! ```text
//!   Mixer::render ──▶ output callback ──▶ TapSink (SPSC ring)
//!                                              │
//!                                       writer thread
//!                                              │  s16le @ 48 kHz mono
//!                                              ▼
//!                                       /tmp/lanmic-<uid>.source  (FIFO)
//!                                              │
//!                                     module-pipe-source ──▶ "LAN_Mic"
//! ```
//!
//! The tap is downstream of everything: master gain, the feedback shifter and
//! the limiter have all run, so what a video call hears is what the room hears.
//! If the shifter is on, the call gets the shift too - which is right for a
//! room being reinforced and wrong for a call, so turn it off when the mix is
//! going down the wire rather than into a speaker.
//!
//! Nothing here runs on the audio thread except [`TapSink::push`], which is a
//! bounded copy into a wait-free ring and drops rather than blocks. The FIFO,
//! the process spawns and the blocking are all on the writer thread.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rtrb::Producer;

use lanmic::protocol::SAMPLE_RATE;

/// What other programs see the source called. Underscores rather than spaces
/// on purpose: this ends up inside a PulseAudio property list nested inside a
/// module argument string, and the two parsers between here and there do not
/// agree about quoting. [`sanitise_name`] enforces it.
pub const DEFAULT_NAME: &str = "LAN_Mic";

/// The source's PulseAudio name - what `pactl` and `pw-link` address it by.
/// Fixed rather than generated, so a script written for one gig works at the
/// next; a leftover module from a crashed run is swept before a new one loads.
const SOURCE_NAME: &str = "lanmic";

/// A quarter of a second of mix between the audio callback and the FIFO. Deep
/// enough to ride out the writer thread being descheduled, shallow enough that
/// a stalled reader cannot hide a quarter-second of latency in it.
const TAP_FRAMES: usize = SAMPLE_RATE as usize / 4;

/// Frames the writer moves from the ring to the FIFO in one go: 10 ms.
const CHUNK_FRAMES: usize = 480;

/// The FIFO is 64 kB and the kernel will happily let us fill it, which would be
/// two thirds of a second of latency on the far end. Past this, the oldest
/// audio is dropped instead - a click now beats a permanent delay.
const MAX_PENDING_BYTES: usize = SAMPLE_RATE as usize / 5 * 2;

// ---------------------------------------------------------------------------
// The tap
// ---------------------------------------------------------------------------

/// What the window and the status line report. All published from the two
/// threads that own the halves of the ring, and never read by either.
#[derive(Debug, Default)]
pub struct Stats {
    frames_in: AtomicU64,
    frames_out: AtomicU64,
    frames_dropped: AtomicU64,
    alive: AtomicBool,
}

impl Stats {
    /// Frames the mixer handed the tap.
    pub fn frames_in(&self) -> u64 {
        self.frames_in.load(Ordering::Relaxed)
    }

    /// Frames that reached the FIFO.
    pub fn frames_out(&self) -> u64 {
        self.frames_out.load(Ordering::Relaxed)
    }

    /// Frames lost, either because the ring was full or because the FIFO
    /// backed up past [`MAX_PENDING_BYTES`]. Anything but zero is audible.
    pub fn frames_dropped(&self) -> u64 {
        self.frames_dropped.load(Ordering::Relaxed)
    }

    /// False once the writer thread has stopped - normally because the source
    /// was unloaded from underneath us.
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    /// A frame count as the seconds of audio it is, which is the only form in
    /// which "in" and "out" drifting apart means anything to a reader.
    pub fn seconds(frames: u64) -> f32 {
        frames as f32 / SAMPLE_RATE as f32
    }
}

/// The audio thread's half. Held in the [`Tap`] slot so that installing and
/// removing it never touches the mixer or its stream.
pub struct TapSink {
    producer: Producer<f32>,
    stats: Arc<Stats>,
}

impl TapSink {
    /// One callback's worth of finished mix. Wait-free: what does not fit is
    /// counted and dropped, because the alternative on an audio thread is to
    /// block, and the mix reaching the speakers matters more than the mix
    /// reaching a video call.
    pub fn push(&mut self, mix: &[f32]) {
        let room = mix.len().min(self.producer.slots());
        let written = match self.producer.write_chunk_uninit(room) {
            Ok(chunk) => chunk.fill_from_iter(mix.iter().copied()),
            Err(_) => 0,
        };
        self.stats
            .frames_in
            .fetch_add(written as u64, Ordering::Relaxed);
        if written < mix.len() {
            self.stats
                .frames_dropped
                .fetch_add((mix.len() - written) as u64, Ordering::Relaxed);
        }
    }
}

/// Where the output callback looks for a tap.
///
/// A `Mutex` the callback only ever `try_lock`s, so installing or removing a
/// tap from the UI thread cannot stall the stream: the worst it can cost is one
/// block that the virtual microphone does not get.
pub type Tap = Mutex<Option<TapSink>>;

// ---------------------------------------------------------------------------
// Pure helpers - the parts that can be tested without a sound server
// ---------------------------------------------------------------------------

/// Makes a description that survives the trip through `pactl`'s argument
/// joining, PulseAudio's module-argument parser and its property-list parser.
///
/// Whitespace becomes an underscore and anything outside a conservative set is
/// dropped, because a quote or a space in there does not fail loudly: it
/// silently loads a module with the wrong description, or none at all.
pub fn sanitise_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_underscore = false;
    for c in name.trim().chars() {
        let mapped = if c.is_whitespace() {
            '_'
        } else if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
            c
        } else {
            continue;
        };
        if mapped == '_' && last_was_underscore {
            continue;
        }
        last_was_underscore = mapped == '_';
        out.push(mapped);
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        DEFAULT_NAME.to_string()
    } else {
        out
    }
}

/// Module indices in `pactl list short modules` output that are a pipe source
/// of ours - a run that was killed rather than stopped leaves one behind, and
/// the next load would fail on the name it is still holding.
///
/// The match is on the whole `source_name=` token, so a source called
/// `lanmic_2` belonging to somebody else is left alone.
pub fn stale_module_ids(listing: &str, source_name: &str) -> Vec<String> {
    let wanted = format!("source_name={source_name}");
    listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let index = fields.next()?.trim();
            let module = fields.next()?.trim();
            let argument = fields.next().unwrap_or("");
            let ours = module == "module-pipe-source"
                && argument.split_whitespace().any(|token| token == wanted);
            (ours && !index.is_empty()).then(|| index.to_string())
        })
        .collect()
}

/// The mix as the FIFO wants it: interleaved little-endian 16-bit mono.
/// Clamped rather than wrapped - master gain is applied after the limiter, so
/// the bus can arrive here above full scale.
pub fn encode_i16le(mix: &[f32], out: &mut Vec<u8>) {
    out.reserve(mix.len() * 2);
    for &s in mix {
        let v = (s * 32767.0).clamp(-32768.0, 32767.0) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
}

/// Drops whole frames off the front of a backlog that has grown past `cap`,
/// and says how many frames went. Frames, not bytes: half a sample handed to
/// the FIFO would swap the endianness of everything after it.
pub fn trim_pending(pending: &mut Vec<u8>, cap: usize) -> u64 {
    if pending.len() <= cap {
        return 0;
    }
    // Round the survivors down to a whole number of frames.
    let keep = (cap / 2) * 2;
    let dropped = pending.len() - keep;
    pending.drain(..dropped);
    (dropped / 2) as u64
}

// ---------------------------------------------------------------------------
// The source itself
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod pipe {
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write};
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use rtrb::{Consumer, RingBuffer};

    use lanmic::protocol::SAMPLE_RATE;

    use super::{
        encode_i16le, sanitise_name, stale_module_ids, trim_pending, Stats, Tap, TapSink,
        CHUNK_FRAMES, MAX_PENDING_BYTES, SOURCE_NAME, TAP_FRAMES,
    };

    /// How long the writer waits when there is nothing to move, or when the
    /// FIFO is full. Well under the ring's depth, so neither costs frames.
    const IDLE: Duration = Duration::from_millis(2);

    /// The source's read end opens a moment after `load-module` returns; until
    /// it does, opening the write end of a FIFO is `ENXIO`.
    const OPEN_ATTEMPTS: u32 = 50;
    const OPEN_RETRY: Duration = Duration::from_millis(20);

    /// A live source: a loaded module, a FIFO, and the thread that feeds it.
    ///
    /// Every one of those is released by `Drop`, in the order that leaves
    /// nothing dangling: the tap first so the audio thread stops writing, then
    /// the thread, then the module, then the FIFO.
    pub struct VirtualMic {
        name: String,
        module: String,
        path: PathBuf,
        running: Arc<AtomicBool>,
        stats: Arc<Stats>,
        tap: Arc<Tap>,
        thread: Option<JoinHandle<()>>,
    }

    impl VirtualMic {
        /// Loads the source and starts feeding it whatever `tap`'s output
        /// callback pushes.
        pub fn start(name: &str, tap: &Arc<Tap>) -> io::Result<Self> {
            let name = sanitise_name(name);
            let path = pipe_path();

            // A session that was killed rather than stopped leaves its module
            // loaded and holding the name. Clearing it is not optional: the
            // load below would fail, and it would keep failing until somebody
            // found `pactl unload-module` for themselves.
            sweep_stale();
            let _ = std::fs::remove_file(&path);

            let module = pactl(&[
                "load-module",
                "module-pipe-source",
                &format!("source_name={SOURCE_NAME}"),
                &format!("file={}", path.display()),
                "format=s16le",
                &format!("rate={SAMPLE_RATE}"),
                "channels=1",
                &format!("source_properties=device.description={name}"),
            ])?;
            let module = module.trim().to_string();
            if module.is_empty() {
                return Err(io::Error::other(
                    "pactl loaded the source but did not report a module index",
                ));
            }

            let stats = Arc::new(Stats::default());
            let mut engine = VirtualMic {
                name,
                module,
                path,
                running: Arc::new(AtomicBool::new(true)),
                stats,
                tap: tap.clone(),
                thread: None,
            };

            // From here on every failure path drops `engine`, which unloads the
            // module it has just loaded rather than leaving a dead source on the
            // machine.
            let file = open_pipe(&engine.path)?;
            let (producer, consumer) = RingBuffer::<f32>::new(TAP_FRAMES);
            engine.stats.alive.store(true, Ordering::Release);

            engine.thread = Some(
                thread::Builder::new()
                    .name("lau-vmic".into())
                    .spawn({
                        let running = engine.running.clone();
                        let stats = engine.stats.clone();
                        move || feed(file, consumer, &running, &stats)
                    })
                    .inspect_err(|_| engine.stats.alive.store(false, Ordering::Release))?,
            );

            // Installed last: until the thread exists there is nobody to drain
            // the ring, and a ring that fills before the first read would hand
            // the source a quarter second of stale mix.
            if let Ok(mut slot) = engine.tap.lock() {
                *slot = Some(TapSink {
                    producer,
                    stats: engine.stats.clone(),
                });
            }

            log::info!(
                "virtual microphone '{}' up: pulseaudio source '{SOURCE_NAME}', module {}",
                engine.name,
                engine.module
            );
            Ok(engine)
        }

        /// The description other programs list it under.
        pub fn name(&self) -> &str {
            &self.name
        }

        pub fn stats(&self) -> &Arc<Stats> {
            &self.stats
        }
    }

    impl Drop for VirtualMic {
        fn drop(&mut self) {
            if let Ok(mut slot) = self.tap.lock() {
                *slot = None;
            }
            self.running.store(false, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            if let Err(e) = pactl(&["unload-module", &self.module]) {
                log::warn!("could not unload the virtual microphone: {e}");
            }
            let _ = std::fs::remove_file(&self.path);
            log::info!("virtual microphone '{}' stopped", self.name);
        }
    }

    /// One fixed path per user, so a stale FIFO from a killed run is ours to
    /// remove rather than somebody else's to be refused by.
    fn pipe_path() -> PathBuf {
        // SAFETY: getuid cannot fail and touches no memory we own.
        let uid = unsafe { libc::getuid() };
        std::env::temp_dir().join(format!("lanmic-{uid}.source"))
    }

    fn pactl(args: &[&str]) -> io::Result<String> {
        let output = Command::new("pactl").args(args).output().map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "pactl was not found: the virtual microphone needs PulseAudio or PipeWire",
                )
            } else {
                e
            }
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(io::Error::other(format!(
                "pactl {}: {}",
                args.first().copied().unwrap_or(""),
                stderr.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Best effort, and deliberately not fatal: if the listing cannot be read
    /// the load below will say so, with a better message than this could.
    fn sweep_stale() {
        let Ok(listing) = pactl(&["list", "short", "modules"]) else {
            return;
        };
        for id in stale_module_ids(&listing, SOURCE_NAME) {
            log::info!("unloading a virtual microphone left over from an earlier run: {id}");
            let _ = pactl(&["unload-module", &id]);
        }
    }

    /// Opens the write end. `O_NONBLOCK` for two reasons: without it the open
    /// itself blocks until a reader arrives, and with it a write to a full FIFO
    /// returns rather than parking the writer thread on a stalled consumer.
    fn open_pipe(path: &Path) -> io::Result<File> {
        let mut last = io::Error::other("the source's pipe never opened");
        for _ in 0..OPEN_ATTEMPTS {
            match OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)
            {
                Ok(file) => return Ok(file),
                Err(e) => {
                    last = e;
                    thread::sleep(OPEN_RETRY);
                }
            }
        }
        Err(io::Error::new(
            last.kind(),
            format!("{}: {last}", path.display()),
        ))
    }

    /// Moves the mix from the ring into the FIFO until the session ends.
    ///
    /// `pending` is what has been taken off the ring and not yet accepted by
    /// the kernel. It exists because a FIFO takes partial writes, and dropping
    /// the remainder of one would shift every sample after it by half a frame.
    fn feed(mut file: File, mut ring: Consumer<f32>, running: &AtomicBool, stats: &Stats) {
        let mut mix = Vec::with_capacity(CHUNK_FRAMES);
        let mut pending: Vec<u8> = Vec::with_capacity(MAX_PENDING_BYTES);

        while running.load(Ordering::Acquire) {
            if pending.len() < MAX_PENDING_BYTES {
                mix.clear();
                let n = CHUNK_FRAMES.min(ring.slots());
                if let Ok(chunk) = ring.read_chunk(n) {
                    let (first, second) = chunk.as_slices();
                    mix.extend_from_slice(first);
                    mix.extend_from_slice(second);
                    chunk.commit_all();
                }
                encode_i16le(&mix, &mut pending);
            }

            if pending.is_empty() {
                thread::sleep(IDLE);
                continue;
            }

            match file.write(&pending) {
                Ok(0) => thread::sleep(IDLE),
                Ok(written) => {
                    pending.drain(..written);
                    stats
                        .frames_out
                        .fetch_add((written / 2) as u64, Ordering::Relaxed);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(IDLE),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    // Normally a broken pipe: somebody unloaded the source. The
                    // session stays up and says so rather than dying with it.
                    log::warn!("virtual microphone stopped: {e}");
                    break;
                }
            }

            let lost = trim_pending(&mut pending, MAX_PENDING_BYTES);
            if lost > 0 {
                stats.frames_dropped.fetch_add(lost, Ordering::Relaxed);
            }
        }
        stats.alive.store(false, Ordering::Release);
    }
}

#[cfg(not(unix))]
mod pipe {
    use std::io;
    use std::sync::Arc;

    use super::{Stats, Tap};

    /// Windows has no equivalent of `module-pipe-source`: a virtual microphone
    /// there is a kernel driver somebody else installs. The honest answer is to
    /// say so and point at the thing that does work - a loopback device such as
    /// VB-Cable, chosen from the output list like any other device.
    pub struct VirtualMic {
        stats: Arc<Stats>,
        name: String,
    }

    impl VirtualMic {
        pub fn start(_name: &str, _tap: &Arc<Tap>) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this platform has no virtual microphone to create: install a loopback \
                 device (VB-Cable, BlackHole) and pick it as the output instead",
            ))
        }

        pub fn name(&self) -> &str {
            &self.name
        }

        pub fn stats(&self) -> &Arc<Stats> {
            &self.stats
        }
    }
}

pub use pipe::VirtualMic;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_reduced_to_what_survives_two_parsers() {
        assert_eq!(sanitise_name("LAN Mic"), "LAN_Mic");
        assert_eq!(sanitise_name("  Front   of house  "), "Front_of_house");
        assert_eq!(sanitise_name("stage-1.mix"), "stage-1.mix");
        // Quotes and equals signs are what would break the property list.
        assert_eq!(sanitise_name("a\"b=c"), "abc");
        // Nothing usable left is not a source with no name.
        assert_eq!(sanitise_name("   "), DEFAULT_NAME);
        assert_eq!(sanitise_name("\"\""), DEFAULT_NAME);
        assert_eq!(sanitise_name("__"), DEFAULT_NAME);
    }

    #[test]
    fn only_our_own_leftover_modules_are_swept() {
        let listing = "\
0\tmodule-device-restore\t\n\
12\tmodule-pipe-source\tsource_name=lanmic file=/tmp/lanmic-1000.source rate=48000\t\n\
13\tmodule-pipe-source\tsource_name=someone_else file=/tmp/other\t\n\
14\tmodule-null-sink\tsource_name=lanmic\t\n\
15\tmodule-pipe-source\tformat=s16le source_name=lanmic\t";
        assert_eq!(stale_module_ids(listing, "lanmic"), ["12", "15"]);
        assert!(stale_module_ids("", "lanmic").is_empty());
        // A prefix is not a match: `lanmic_2` is somebody else's source.
        assert!(
            stale_module_ids("9\tmodule-pipe-source\tsource_name=lanmic_2\t", "lanmic").is_empty()
        );
    }

    #[test]
    fn the_mix_is_encoded_little_endian_and_clamped() {
        let mut out = Vec::new();
        encode_i16le(&[0.0, 1.0, -1.0, 2.0, -2.0], &mut out);
        assert_eq!(
            out,
            [
                0, 0, // 0
                0xFF, 0x7F, // 32767
                0x01, 0x80, // -32767
                0xFF, 0x7F, // clamped, not wrapped
                0x00, 0x80, // -32768
            ]
        );
    }

    #[test]
    fn a_backlog_is_trimmed_to_whole_frames_from_the_oldest_end() {
        // Under the cap, nothing moves.
        let mut pending = vec![1u8, 2, 3, 4];
        assert_eq!(trim_pending(&mut pending, 8), 0);
        assert_eq!(pending, [1, 2, 3, 4]);

        // Over it, the newest survives and the count is in frames: ten bytes
        // trimmed to a four-byte cap leaves two frames and drops three.
        let mut pending: Vec<u8> = (0..10).collect();
        assert_eq!(trim_pending(&mut pending, 4), 3);
        assert_eq!(pending, [6, 7, 8, 9]);

        // An odd cap still leaves a whole number of frames behind.
        let mut pending: Vec<u8> = (0..10).collect();
        assert_eq!(trim_pending(&mut pending, 5), 3);
        assert_eq!(pending.len() % 2, 0);
    }

    #[test]
    fn the_tap_drops_rather_than_blocking_when_the_ring_is_full() {
        let stats = Arc::new(Stats::default());
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(4);
        let mut sink = TapSink {
            producer,
            stats: stats.clone(),
        };

        sink.push(&[0.1, 0.2]);
        assert_eq!(stats.frames_in(), 2);
        assert_eq!(stats.frames_dropped(), 0);

        // Two more fit; the last two do not, and are counted rather than waited
        // on - this is called from the audio callback.
        sink.push(&[0.3, 0.4, 0.5, 0.6]);
        assert_eq!(stats.frames_in(), 4);
        assert_eq!(stats.frames_dropped(), 2);

        drop(consumer);
    }

    #[test]
    fn a_tap_that_is_being_installed_never_stalls_the_audio_thread() {
        // The output callback only ever try_locks, which is the property that
        // lets the UI thread install and remove taps under a running stream.
        let tap: Arc<Tap> = Arc::new(Mutex::new(None));
        let held = tap.lock().unwrap();
        assert!(tap.try_lock().is_err());
        drop(held);
        assert!(tap.try_lock().is_ok());
    }
}
