# LAN Mic

Wireless microphones for live presentations, over your own Wi-Fi, with no codec
in the path. An Android phone becomes a microphone; another phone (or a laptop)
becomes the mixer plugged into the PA.

Two modes in one APK:

1. **Microphone** — captures with AAudio in low-latency exclusive mode and ships
   5 ms packets of 48 kHz 16-bit PCM over UDP.
2. **Mixer / server** — receives from up to 8 microphones, runs one jitter
   buffer per source, sums them, limits the bus, and plays out through AAudio.

A Python server (`server/lan_audio_server.py`) speaks the same protocol, so a
laptop can be the mixer instead — usually the better choice when you want to
feed a real interface or a house console.

Typical end-to-end delay is **33–47 ms**. The wire format and the latency
budget are in [PROTOCOL.md](PROTOCOL.md); how the code is put together is in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

```
  phone (mic)  ─┐
  phone (mic)  ─┼─ UDP 48 kHz PCM ─▶  mixer  ─▶  PA / speaker
  laptop (mic) ─┘                    (phone or laptop)
```

## Layout

```
rust/                   audio engine: protocol, jitter buffers, mixer, UDP,
                        the Oboe streams and the JNI surface
app/src/main/java/      Kotlin UI, foreground service, discovery
server/                 desktop mixing server + a test transmitter
tools/                  test runner for both suites
PROTOCOL.md             wire format, jitter strategy, latency budget
docs/ARCHITECTURE.md    module map, threading model, invariants
```

## Build the app

Requires Android Studio (Ladybug or newer) with the NDK installed, plus a Rust
toolchain and `cargo-ndk`:

```bash
rustup target add aarch64-linux-android armv7-linux-androideabi \
                  x86_64-linux-android
cargo install cargo-ndk
```

Then, from the project root — the Gradle wrapper is included:

```bash
./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

Gradle builds the Rust engine for each packaged ABI and drops the resulting
`liblanmic.so` into the APK; there is nothing to run by hand. The engine is
always built in release, even for a debug APK, because an unoptimised realtime
audio path misses its deadlines.

First build pulls the Gradle distribution, the AndroidX/Compose artifacts, the
NDK, and the Rust crates — Oboe among them, as vendored C++ sources that
`oboe-sys` compiles and links into the same `.so`. Budget a few GB and a
coffee; after that it is incremental.

No local Android SDK? Push the repo to GitHub and
`.github/workflows/android.yml` builds `app-debug.apk` for you and uploads it as
a workflow artifact - it also runs both test suites on every push.

`minSdk` is 26. AAudio's low-latency MMAP path arrives properly in API 27+; on
older devices Oboe falls back to OpenSL ES and capture latency roughly doubles.

## Run the desktop server

```bash
cd server
pip install -r requirements.txt
python3 lan_audio_server.py --list-devices
python3 lan_audio_server.py --jitter 15 --device 3 --blocksize 240
```

Useful flags: `--jitter` (buffer target, ms), `--blocksize` (output block in
frames; 240 = 5 ms), `--channels`, `--gain`, `--port`, `--no-discovery`.

Send it a test tone without touching a phone:

```bash
python3 test_mic_client.py --tone 440          # finds the server by broadcast
python3 test_mic_client.py --host 192.168.1.50 --mic
```

## Using it at a venue

1. Start the mixer first (app in **Mixer / server** mode, or the Python server).
   The Android screen shows the IP addresses it is listening on.
2. On each microphone phone: **Find server**, or type the address, then **GO
   LIVE**. Grant the microphone permission once.
3. Speak. The mixer lists each microphone with a level meter, its current
   buffer depth, and its loss counters.

The phones keep running with the screen off — a foreground service holds a
Wi-Fi low-latency lock and a partial wake lock. Do not swipe the app away.

## Making the Wi-Fi behave

The network dominates the latency budget. In rough order of impact:

* **Use a dedicated AP on 5 GHz.** A cheap travel router with nothing else
  attached beats the venue's guest network every time.
* **Turn off the internet uplink** on that AP, or at least keep other clients
  off it. One phone syncing photos will bury a 1.5 Mbps audio stream.
* **Disable AP power-save / WMM power-save** if the router exposes it.
* **Keep the phones in sight of the AP.** Airtime lost to retries is the main
  source of the 20 ms spikes that force you to raise the jitter buffer.
* Packets are tagged DSCP EF (0xB8), which maps to the WMM voice queue. Most
  consumer APs honour this; some strip it. If yours strips it, prefer an AP
  with fewer clients over trying to fix the marking.

Wired is better still: an Ethernet-connected laptop as the mixer removes half
the variance immediately.

## Tuning

| Symptom | What to change |
|---|---|
| Occasional clicks, `under` counter rising | Raise the jitter buffer 5 ms at a time |
| Buffer depth creeping up, then a glitch | Normal clock drift; the trim is doing its job |
| `lost` counter rising steadily | Wi-Fi congestion — move the AP, drop other clients |
| Latency feels high but counters are clean | Lower the jitter buffer, try 2.5 ms packets |
| Distortion when several people talk | Pull the master down; the limiter is clamping |
| Feedback howl | The mixer's speaker is feeding the mics — that's physics, not software |

Packet size is a direct trade: 2.5 ms shaves latency but triples the packet
rate (400/s per mic), which some cheap APs handle badly. 5 ms is the default
for a reason.

## Design notes

* **No codec.** Opus would cut bandwidth 25x and add 5–10 ms plus a decode
  step. On a LAN you have the bandwidth; you do not have the milliseconds.
  1.5 Mbps per microphone is nothing for 5 GHz Wi-Fi.
* **The audio callback never calls the network.** Capture writes into a
  wait-free SPSC ring and posts a semaphore; a dedicated sender thread does the
  `sendto`. Same on the receive side: the network thread only ever writes into
  per-source rings.
* **Timestamps, not arrival times.** Every packet carries the sender's frame
  index, so a late packet lands in the right place or is dropped — it never
  shifts the stream.
* **Latency is shed by dropping the oldest audio**, never by resampling. Sender
  and receiver clocks drift tens of ppm apart; over an hour that is a few
  hundred milliseconds, absorbed as one inaudible trim every several minutes.
* **Zero-latency limiter.** Instant attack, 150 ms release, no lookahead —
  because lookahead is latency.

The threading model and the invariants behind these choices are written up in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Tests

```bash
tools/run_tests.sh              # Rust engine + Python server
tools/run_tests.sh --strict     # the same, plus rustfmt and clippy
```

`cargo test` covers everything except the two Oboe streams, including an
end-to-end suite that runs a ramp from the capture encoder, over a real
loopback socket, through the jitter buffers and out of the mixer, and checks it
sample by sample. No device required. The Python suite covers the same
behaviours in `lan_audio_server.py`, so the two implementations stay in step.
CI runs both on every push.

## Limits

* 8 simultaneous microphones on the Android mixer (`MAX_SOURCES` in
  `rust/src/mixer.rs`).
* Mono on the wire; the mixer plays the same mix to every output channel.
* No encryption and no authentication. Anyone on the LAN can send audio to the
  mixer. Use a private AP.
* No AEC — this is reinforcement, not a conference call. Keep microphones out
  of the speakers' throw.
