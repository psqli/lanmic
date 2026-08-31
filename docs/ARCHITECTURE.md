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

### Native core — `app/src/main/cpp/`

Everything except the two `.cpp` files that touch Oboe is portable, which is
what lets `tools/host_test.cpp` exercise it on a desktop.

| File | What it is |
|---|---|
| `protocol.h` | LAU1 header encode/decode and PCM↔wire conversion. No allocation, no platform calls. |
| `spsc_ring.h` | Wait-free single-producer/single-consumer ring. Power-of-two capacity, `uint64` cursors so wrap arithmetic is free. |
| `jitter_buffer.h` | One per source: timestamp alignment, concealment, trimming, and the loss counters. |
| `mixer.h` | `SourceTable` (fixed 8 slots, no allocation) plus the lookahead-free `Limiter`. |
| `meter.h` | Peak meter ballistics shared by all three meters. |
| `udp_socket.h/.cpp` | Thin POSIX UDP wrapper; sets DSCP EF so Wi-Fi treats the flow as voice. |
| `util.h` | Monotonic clock and best-effort thread priority. |
| `transmitter.h/.cpp` | Oboe input stream + sender thread. |
| `receiver.h/.cpp` | Oboe output stream + network thread + the source table. |
| `jni_bridge.cpp` | The whole JNI surface. Two global engines behind one mutex. |

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

`lan_audio_server.py` mirrors the receive half: `Source` is the jitter buffer,
`Limiter` the bus limiter, `Server` the network/audio/discovery threads.
`test_mic_client.py` is a transmitter for testing either server without a phone.

## Threading

Six threads matter. Nothing else may touch the marked structures.

| Thread | Owns | Never does |
|---|---|---|
| Oboe input callback | `Transmitter::ring_` (write side), `cbScratch_` | Syscalls, allocation, locks |
| Sender | `seq_`, `timestamp_`, `txPacket_`, the socket | Blocking sends (socket is non-blocking) |
| Oboe output callback | Every `JitterBuffer` read side, `mixBuf_`, `srcBuf_`, `Limiter` | Syscalls, allocation, locks |
| Network (mixer) | Every `JitterBuffer` write side, slot lifecycle | Blocking on the audio thread |
| Discovery | Its own socket | Anything on the audio path |
| UI / JNI | Reads counters through relaxed atomics | Blocking (stat calls use `try_lock`) |

The audio callback and its partner thread meet **only** inside an `SpscRing`.
Counters cross as relaxed atomics: a torn read of a statistic costs a wrong
number on screen for 80 ms, and a lock there would cost a dropout.

### Why the stat calls use `try_lock`

`nativeTxStats` and friends are polled 12x a second from the UI thread while
`nativeStartTransmitter` may be holding the global lock through a stream open
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

## Testing

```bash
tools/run_tests.sh              # C++ core + Python server
tools/run_tests.sh --sanitize   # also under ASan/UBSan and TSan
```

`host_test.cpp` covers everything except the Oboe streams — protocol, ring,
jitter buffer, meter, mixer, limiter, and a real UDP loopback that verifies
samples arrive bit-exact. `test_python_server.py` covers the same behaviours
in the Python implementation, so the two stay in step. CI runs both, plus the
sanitizers, on every push.

The sanitizer runs are not decoration: the whole design rests on two threads
meeting in lock-free buffers, and TSan is the only thing that checks the
memory ordering claims.

## Known limitations

Real, understood, and deliberately not fixed.

* **Slot reuse is racy under a fast leave/join.** If a source is retired and a
  different one claims its slot inside a single audio callback, that callback
  can read a jitter buffer that the network thread is concurrently resetting.
  The ring's cursor arithmetic is masked, so the result is at worst one block
  of wrong audio — not a crash or an out-of-bounds read. Fixing it properly
  needs a retiring state with a handshake between the two threads.
* **A device change during stop can leave a stream open.** The Oboe error
  callback reopens the stream from a detached thread; `stop()` racing with that
  reopen is handled by re-checking afterwards and closing, which narrows the
  window but does not close it.
* **No encryption, no authentication.** Anyone on the LAN can send audio to a
  mixer, and `ssrc` is self-assigned. This is a private-AP design.
* **The Python limiter ramps gain across a block** rather than per sample like
  the C++ one, so a very fast transient can exceed the ceiling by a hair
  before the next block catches it.
* **No acoustic echo cancellation**, and none is wanted: this is
  reinforcement, not conferencing.
