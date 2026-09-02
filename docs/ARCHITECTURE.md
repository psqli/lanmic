# Architecture

How the code is laid out, which thread touches what, and the invariants that
keep the audio path from glitching. For the wire format see
[PROTOCOL.md](../PROTOCOL.md); for operating the thing see the
[README](../README.md).

## The shape of it

One APK contains both ends. A phone is either a **microphone** (capture → UDP)
or a **mixer** (UDP → jitter buffers → sum → speaker), never both at once. The
desktop app in `desktop/` is the same two ends again, linking the same engine
crate with cpal streams and a GPUI window in place of Oboe and Compose. The
Python server is a second, *independent* implementation of the mixer half that
speaks the same protocol - deliberately not a port, so the wire format has two
readings of it.

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

One crate, linked two ways: as a `cdylib` the APK loads, and as an `rlib` the
desktop binary links. Everything except `android.rs` and `bridge.rs` is
portable, which is what lets `cargo test` exercise the whole path - capture,
packetise, socket, conceal, mix - on a desktop with no device and no Oboe, and
what lets the desktop front-end be a front-end rather than a second engine.

| File | What it is |
|---|---|
| `protocol.rs` | LAU1 header encode/decode and PCM↔wire conversion. No allocation, no platform calls, no `unsafe`. |
| `jitter.rs` | One buffer per source: timestamp alignment, concealment, trimming, loss counters. Split into `JitterWriter` and `JitterReader`. |
| `mixer.rs` | `Table` (fixed 8 slots, no allocation) split into `SourceWriter` and `Mixer`, plus the lookahead-free `Limiter`. |
| `meter.rs` | Peak meter ballistics shared by all three meters. |
| `feedback.rs` | Single-sideband frequency shifter: the anti-howlround measure on the mix bus. |
| `net.rs` | UDP helpers over `socket2`: DSCP EF so Wi-Fi treats the flow as voice, and the port range check. |
| `util.rs` | Monotonic clock, milli-unit atomics, best-effort thread priority. |
| `transmitter.rs` | `CaptureEncoder` (downmix, gain, meter) and `Packetiser` (ring → wire). Portable. |
| `receiver.rs` | `PacketRouter` (socket → source table) and the stereo interleave. Portable. |
| `supervisor.rs` | The one-thread-owns-one-stream lifecycle, shared by the Oboe and cpal front-ends. Portable. |
| `android.rs` | The two Oboe streams and the threads around them. The only device-specific code. |
| `bridge.rs` | The whole JNI surface. Two engines behind two mutexes. |
| `build.rs` | Android link configuration: `libc++abi`, and `-Wl,--no-undefined` so a missing symbol fails the build instead of `dlopen`. |

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

### Desktop app — `desktop/`

A separate crate with a path dependency on `rust/`, so `rust/` keeps its own
lockfile and the Gradle build that reads it. It supplies exactly what a phone
supplies and no more: audio streams, and a UI.

| File | What it is |
|---|---|
| `main.rs` | Mode selection: window, `--headless` mixer, `--headless --mic`, `--list-devices`. |
| `args.rs` | The command line: a `clap` derive struct, and the mapping from it onto the two config structs. Flags match `lan_audio_server.py` where they overlap. |
| `audio.rs` | cpal: device enumeration, 48 kHz config selection, and the four format conversions between cpal buffers and what the engine takes. |
| `engine.rs` | `Server` and `Microphone`: `android.rs` with cpal streams. Same supervisor, same threads, same `*Shared` atomics. |
| `discovery.rs` | DISCOVER/ANNOUNCE, both halves, encoded with `lanmic::protocol`. |
| `console.rs` | The `--headless` status line, for a machine in a rack. |
| `ui/` | The GPUI window: `mod.rs` the view and its state, `mixer.rs` and `mic.rs` the two panels, `widgets.rs` the peak meter and the readouts, `theme.rs` colours. |

Extra dependencies over the engine's: `gpui`, `gpui-component`, `cpal`, `clap`,
`if-addrs`, `env_logger`.

**The engine is not modified for the desktop.** The desktop's whole job is to
push `i16` into `CaptureEncoder::push` and pull `f32` out of `Mixer::render`;
everything between those two calls is the code the phone runs.

### Desktop server (Python) — `server/`

`lan_audio_server.py` mirrors the receive half in Python: `Source` is the
jitter buffer, `Limiter` the bus limiter, `Server` the network/audio/discovery
threads. It is a second, independent implementation of the same protocol, and
it is deliberately not a port of the Rust one.
`test_mic_client.py` is a transmitter for testing either server without a phone.

## Threading

Six threads matter. Nothing else may touch the marked structures.

| Thread | Owns | Never does |
|---|---|---|
| Input callback (Oboe / cpal) | `CaptureEncoder` (the ring's write half) | Syscalls, allocation, blocking |
| Sender | `Packetiser`: seq, timestamp, the socket | Blocking sends (the socket is non-blocking) |
| Output callback (Oboe / cpal) | `Mixer`: every jitter buffer's read half, the scratch buffers, the `Limiter` | Syscalls, allocation, blocking |
| Network (mixer) | `PacketRouter`: every jitter buffer's write half, slot lifecycle | Blocking on the audio thread |
| Stream supervisor | The Oboe stream itself, and nothing else does | Touching audio data |
| Discovery | Its own socket | Anything on the audio path |
| Service worker (Android) | `AudioService` locks, responder, engine start/stop | Nothing - but it is the *only* thread allowed to do them |
| UI (Compose / GPUI) | Reads counters through relaxed atomics | Blocking (stat calls use `try_lock`) |

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

`supervisor.rs`, used by both front-ends. Opening, starting and closing an audio
stream is done by a supervisor thread and by nothing else, from `start` through
to `stop`, including reopening after a device change. Two live input streams would be two producers on a
single-producer ring; two live output streams would be two readers. Because
only one thread can install a stream, that is not a race to be guarded against
with generation counters - it is a state that cannot be constructed.

The supervisor also polls the stream for whatever the backend will tell it on
its 200 ms tick, and retries a failed reopen with backoff rather than giving up
once and leaving a live session with dead audio. What that poll yields differs:
Oboe answers `getXRunCount` and `calculateLatencyMillis` on demand, while cpal
reports a dropout as an `ErrorKind::Xrun` on the error callback and gives
latency as a per-callback timestamp pair. Both end up in the same two atomics,
so the same UI reads them.

cpal also distinguishes failures Oboe does not, and the desktop side is less
blunt because of it: an `Xrun` is a counter, a `DeviceChanged` reroute and a
`RealtimeDenied` are log lines, and only the rest ask for a new stream.

### Why the app draws its own titlebar

A window needs three things a program cannot supply on its own: somewhere to
drag it by, edges to resize it by, and a button to close it. On macOS, on
Windows, and on X11 under a window manager, the platform draws them. On
Wayland it may not: the `xdg-decoration` protocol is optional and GNOME's
Mutter does not implement it at all, so an application that asks for
server-side decorations there gets a bare rectangle with no way to move,
resize or close it.

So the window asks for client-side decorations, and `gpui-component` draws
them: its `Root` is the window shell - border, shadow, rounded corners and
the resize edges with a cursor for each - and its `TitleBar` carries the drag
to move, the double click to maximise, the right click for the compositor's
menu, and the minimise/maximise/close buttons. Both check
`window.window_decorations()` and draw nothing under `Decorations::Server`, so
macOS and Windows keep their native titlebars rather than growing a second row
of buttons.

The request is Linux-only; macOS and Windows ignore it. X11 honours it only
where a compositor supports it and otherwise falls back to the window
manager's own, logging that it did.

### Why the UI is a component library plus a meter

The first version of this window hand-rolled its own button, slider and text
field, because GPUI ships elements and layout rather than components. The
slider was a canvas, a hitbox and a three-phase drag state machine on the
view; the text field was printable characters and backspace, with a `focused`
field on the view routing keys to one of four of them, and no selection, no
clipboard and no IME.

`gpui-component` pins the same `gpui` this crate does, so its components are
the same types. Adopting it deleted all of that: a slider is an
`Entity<SliderState>` that emits `SliderEvent`, a field is an
`Entity<InputState>` that emits `InputEvent`, and the view subscribes rather
than tracking drags and keystrokes. The window frame went the same way.

What stayed is the part that is about audio rather than about widgets: the
peak meter, whose colour is a function of how close the level is to clipping,
and the fixed-width readouts described below. The theme is still this crate's
- `apply_palette` writes the palette in `theme.rs` into the library's own
theme, so a button and a channel strip belong to the same screen.

### Why every number is in a fixed-width box

Every readout on the mixer changes several times a second, and some of them
change width when they do: a buffer depth going from `9 ms` to `10 ms`, a
packet count reaching ten thousand. In a flex row a wider child pushes
everything after it along, so a mixer left running would have its mute buttons
twitching sideways all evening - which is exactly as distracting as it sounds
when the meters beside them are the thing you are trying to read.

So a number is never laid out by its own width. `widgets::readout` puts it in
a box wide enough for the widest value it can reach and lets it grow inside
that box; `stat` pairs one with its label; `readings` is a row of them. The
widths are the `W_*` constants, and a test asserts each is wide enough for the
widest string it will be asked to hold.

There is a layout test for the effect rather than the mechanism: a channel
strip is rendered with a two-character buffer reading and again with three,
and the last reading in the row has to land on the same pixel both times.

### Why the audio-thread state sits behind a `Mutex`

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
buffer, meter, mixer, limiter, sockets, the stream supervisor, and an
end-to-end suite that runs a ramp from the capture encoder, over a real
loopback socket, through the router and out of the mixer, sample-accurate.
`test_python_server.py` covers the same behaviours in the Python
implementation, so the two stay in step.

The desktop crate's own suite covers everything in it that is not a cpal
stream: config selection against synthetic device capabilities, the four format
conversions, the discovery exchange over a loopback socket, the command line,
the stream-error classification, and the text field's key handling — plus the
UI itself. GPUI ships a test platform with a real window, layout pass and paint
pass and no display behind any of it, so `render` runs under `cargo test` on
both panels, on a full desk of eight strips, and on the error banner. That test
platform always reports server-side decorations, so everything in `frame.rs`
takes the decoration mode as an argument rather than reading it from the
window, and a harness view renders the client-side branch - the one a GNOME
session takes - in CI, where no compositor will ever offer it. That
catches anything in the render path that panics or fails to lay out; it does
not look at pixels, so a control drawn the wrong colour still needs eyes.

The suite needs ALSA and the GPUI system libraries to build, so `run_tests.sh`
skips it with a message where they are missing rather than failing.

CI runs all three suites on every push, and builds the APK.

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
* **The desktop app is 48 kHz only.** There is no resampler anywhere in the
  engine, so a device that will not run at 48 kHz is listed as unusable rather
  than rate-converted behind your back. On Linux that is rarely a real
  restriction: PulseAudio and PipeWire both offer 48 kHz whatever the hardware
  is doing underneath.
* **Starting a desktop session blocks the UI thread** for as long as the
  backend takes to open a stream. That is milliseconds on a working device and
  up to the two-second open timeout on a wedged one.
* **The desktop UI's appearance is not tested.** The render tests prove the
  tree lays out; nothing checks that it looks right.
* **Client-side decorations are asked for on every Linux session, not just the
  ones that need them.** On X11 under a compositor that supports CSD, that
  means the app's own titlebar rather than the window manager's - so its
  theming, and any window-manager gesture bound to a real titlebar, are lost.
  The alternative is sniffing `WAYLAND_DISPLAY` to guess the backend, which
  is a guess; one appearance on every Linux desktop is the more predictable
  trade.
* **Clock drift is shed, not tracked.** The trim drops the oldest audio every
  few minutes instead of resampling to the receiver's clock.
* **The C++ runtime is linked statically.** `oboe-sys` pulls in
  `libc++_static.a`, which leaves the ABI runtime to `libc++abi.a` - linked
  explicitly in `build.rs`, because a shared object may carry undefined symbols
  and the link would otherwise succeed with `__cxa_pure_virtual` missing,
  failing only at `dlopen` on a device. `-Wl,--no-undefined` is there so that
  cannot happen again silently.
* **Each stream open leaks a small callback object**, because `oboe` boxes the
  callback and never reclaims it. What leaks is a `Weak` handle, a few dozen
  bytes per reopen; the buffers it points at are freed with the engine.
* **No acoustic echo cancellation**, and none is possible: the microphone and
  the loudspeaker are separate devices, so the capturing end has no far-end
  reference to cancel against. Feedback is held off instead by shifting the mix
  a few hertz (`feedback.rs`), which denies the loop the phase coherence it
  needs. That is worth 6-10 dB before ringing, not immunity, and it is a
  colouration - inaudible on speech at these depths, audible on music, which is
  why it is adjustable and can be switched off.
