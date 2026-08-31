# Architecture

How the code is laid out, which thread touches what, and the invariants that
keep the audio path from glitching. For the wire format see
[PROTOCOL.md](../PROTOCOL.md); for operating the thing see the
[README](../README.md).

## The shape of it

One APK contains both ends. A phone is either a **microphone** (capture → UDP)
or a **mixer** (UDP → jitter buffers → sum → speaker), never both at once. The
Python server is a second, independent implementation of the mixer half that
speaks the same protocol.

```
MICROPHONE                                   MIXER
                        UDP 48 kHz mono PCM
AAudio in ─▶ ring ─▶ sender ══════════════▶ network ─▶ jitter buffer ─┐
 (audio cb)          (thread)               (thread)   (one per ssrc) │
                                                                      ▼
                                                       AAudio out ◀── sum ─▶ limiter
                                                        (audio cb)
```

Both directions share one rule: **the audio callback never does I/O**. It moves
samples in and out of lock-free rings and returns. Sockets, allocation and
logging live on ordinary threads.

## Module map

### Engine — `rust/`

One crate. Everything except `android.rs` and `bridge.rs` is portable, which is
what lets `cargo test` exercise the whole path - capture, packetise, socket,
conceal, mix - on a desktop, with no device and no Oboe.

| File | What it is |
|---|---|
| `protocol.rs` | LAU1 header encode/decode and PCM↔wire conversion. No allocation, no platform calls, no `unsafe`. |
| `jitter.rs` | One buffer per source: timestamp alignment, concealment, trimming, loss counters. Split into `JitterWriter` and `JitterReader`. |
| `mixer.rs` | `Table` (fixed 8 slots, no allocation) split into `SourceWriter` and `Mixer`, plus the lookahead-free `Limiter`. |
| `meter.rs` | Peak meter ballistics shared by all three meters. |
| `net.rs` | UDP helpers over `socket2`: DSCP EF so Wi-Fi treats the flow as voice, and the port range check. |
| `util.rs` | Monotonic clock, milli-unit atomics, best-effort thread priority. |
| `transmitter.rs` | `CaptureEncoder` (downmix, gain, meter) and `Packetiser` (ring → wire). Portable. |
| `receiver.rs` | `PacketRouter` (socket → source table) and the stereo interleave. Portable. |
| `android.rs` | The two Oboe streams and the threads around them. The only device-specific code. |
| `bridge.rs` | The whole JNI surface. Two engines behind two mutexes. |

Dependencies are deliberately few: `rtrb` for the wait-free SPSC rings,
`socket2` for the socket options `std` does not expose, `oboe` for the audio
streams, `jni`, and `log`/`android_logger`.

### Android app — `app/src/main/java/com/lanmic/audio/`

| File | What it is |
|---|---|
| `MainActivity.kt` | Compose entry point, mode toggle, permission prompt. |
| `MicScreen.kt` / `ServerScreen.kt` | The two screens; each polls native counters every `UI_POLL_MS`. |
| `Components.kt` | `LevelMeter` (dBFS scale) and `StatRow`. |
| `Theme.kt` | `Palette`, the colour scheme and the `Panel` card. |
| `Settings.kt` | Typed wrapper over the one `SharedPreferences` file. |
| `NativeAudio.kt` | JNI facade plus the `TxStats` / `RxStats` / `SourceInfo` data classes. |
| `Discovery.kt` | DISCOVER/ANNOUNCE, in Kotlin because broadcast sockets are easier here. |
| `AudioService.kt` | Foreground service; holds the Wi-Fi and wake locks. |
| `Net.kt` | Local IPv4 address enumeration. |

### Desktop server — `server/`

`lan_audio_server.py` mirrors the receive half in Python: `Source` is the
jitter buffer, `Limiter` the bus limiter, `Server` the network/audio/discovery
threads. It is a second, independent implementation of the same protocol, and
it is deliberately not a port of the Rust one.
`test_mic_client.py` is a transmitter for testing either server without a phone.

## Threading

Six threads matter. Nothing else may touch the marked structures.

| Thread | Owns | Never does |
|---|---|---|
| Oboe input callback | `CaptureEncoder` (the ring's write half) | Syscalls, allocation, blocking |
| Sender | `Packetiser`: seq, timestamp, the socket | Blocking sends (the socket is non-blocking) |
| Oboe output callback | `Mixer`: every jitter buffer's read half, the scratch buffers, the `Limiter` | Syscalls, allocation, blocking |
| Network (mixer) | `PacketRouter`: every jitter buffer's write half, slot lifecycle | Blocking on the audio thread |
| Stream supervisor | The Oboe stream itself, and nothing else does | Touching audio data |
| Discovery | Its own socket | Anything on the audio path |
| Service worker | `AudioService` locks, responder, engine start/stop | Nothing - but it is the *only* thread allowed to do them |
| UI / JNI | Reads counters through relaxed atomics | Blocking (stat calls use `try_lock`) |

**This table is documentation of what the types already enforce.** Every
lock-free buffer is split into a writer half and a reader half that are
separate, non-`Clone` types. `JitterWriter` has no read method; `JitterReader`
has no write method; `SourceWriter` holds all eight write halves and `Mixer`
all eight read halves. Moving one to a thread moves it away from the other, and
sharing one with two threads does not compile. What genuinely crosses threads -
counters, gains, mute flags, the drop mark - is atomics in the `*Shared`
structs. A torn read of a statistic costs a wrong number on screen for 80 ms,
and a lock there would cost a dropout.

### Why `AudioService` does everything on one worker

Starting an engine opens an AAudio stream; stopping one closes it, joins the
network thread and joins the discovery thread. Together that is comfortably
into ANR territory, so none of it may run on the main thread - including from
`onDestroy`, which is where it is easiest to get wrong. Every engine and lock
operation is queued to a single-threaded executor, which also makes those
fields single-threaded and removes the need to lock them. Ordering between a
start and the stop that overtakes it is settled by a generation counter: a
start whose generation is stale abandons itself, and one that finishes after a
stop tears its own engine back down.

### Why one thread owns each audio stream

Opening, starting and closing an audio stream is done by a supervisor thread
and by nothing else, from `start` through to `stop`, including reopening after
a device change. Two live input streams would be two producers on a
single-producer ring; two live output streams would be two readers. Because
only one thread can install a stream, that is not a race to be guarded against
with generation counters - it is a state that cannot be constructed.

The supervisor also polls the stream for xruns and latency on its 200 ms tick,
and retries a failed reopen with backoff rather than giving up once and leaving
a live session with dead audio.

### Why the audio-thread state sits behind a `Mutex`

`oboe` takes the data callback by value and boxes it, and never reclaims that
box when the stream is dropped. So the callback cannot own the mixer or the
capture encoder: every stream ever opened would keep one alive for the life of
the process. It holds a `Weak` instead, and the strong reference stays with the
supervisor - which leaves needing `&mut` through a shared reference.

That `Mutex` is only ever locked by the audio thread; the supervisor never
takes it. So the `try_lock` in the callback is an uncontended atomic, never a
wait. If it ever did fail - two callbacks overlapping across a stream swap - a
block of silence is the correct answer, and the one it returns.

### Why the stat calls use `try_lock`

`nativeTxStats` and friends are polled 12x a second from the UI thread while
`nativeStartTransmitter` may be holding the engine lock through a stream open
that takes tens of milliseconds. Blocking would jank the UI, so the stat
readers take the lock only if it is free and otherwise return the zero value
for that frame.

## Invariants

These are the properties the tests actually check. Break one and you get
clicks, not a crash.

1. **Timestamps place audio; arrival times never do.** A packet's `timestamp`
   decides where its samples land. Late means "behind the write cursor", not
   "arrived late".
2. **Latency is shed by dropping the oldest audio, never by resampling.**
   Clock drift between sender and receiver is absorbed by the reader-side trim.
3. **The buffer returns to target after an underrun.** Underrun sets a resync
   flag and the writer re-primes, rather than letting the buffer hover at zero
   and click on every callback.
4. **A full ring drops a whole packet and leaves the write cursor alone**, so
   the loss shows up as a concealed gap instead of silently shortening the
   stream.
5. **The source table never allocates and never grows.** Eight fixed slots; a
   ninth microphone is refused, not accommodated.
6. **Sources are identified by `ssrc`, not by address**, so a phone roaming
   between APs keeps its channel strip.
7. **The producer never moves the consumer's cursor.** A source rejoining
   cannot rewind the ring under a live audio callback; it publishes an absolute
   "drop everything before here" position and the reader acts on it. Crossed
   cursors are not a transient glitch - they make the ring report a depth
   larger than it can hold, for good.
8. **One audio stream per session.** Starting and stopping bump an epoch, and a
   reopen that was sleeping across a stop/start pair stands down instead of
   installing a second stream over the live one. Two input streams would be two
   producers on a single-producer ring.

## Testing

```bash
tools/run_tests.sh              # Rust engine + Python server
tools/run_tests.sh --strict     # also rustfmt and clippy, as CI does
```

`cargo test` covers everything except the two Oboe streams — protocol, jitter
buffer, meter, mixer, limiter, sockets, and an end-to-end suite that runs a
ramp from the capture encoder, over a real loopback socket, through the router
and out of the mixer, sample-accurate. `test_python_server.py` covers the same
behaviours in the Python implementation, so the two stay in step. CI runs
both on every push, and builds the APK.

What used to need ASan and TSan to check is now checked by the compiler: the
lock-free buffers cannot be reached from the wrong thread, because the halves
are separate types with separate owners. There is no `unsafe` in the engine
outside one `setpriority` call.

## Known limitations

Real, understood, and deliberately not fixed.

* **No encryption, no authentication.** Anyone on the LAN can send audio to a
  mixer, and `ssrc` is self-assigned. This is a private-AP design.
* **The Python limiter still works a block at a time**, not per sample like the
  Rust one. Attack is applied flat across the whole block so the ceiling holds,
  but the gain reduction from a transient is spread over the samples in front
  of it rather than starting exactly at the peak.
* **Clock drift is shed, not tracked.** The trim drops the oldest audio every
  few minutes instead of resampling to the receiver's clock.
* **Each stream open leaks a small callback object**, because `oboe` boxes the
  callback and never reclaims it. What leaks is a `Weak` handle, a few dozen
  bytes per reopen; the buffers it points at are freed with the engine.
* **No acoustic echo cancellation**, and none is wanted: this is
  reinforcement, not conferencing.
