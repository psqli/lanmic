# LAN Mic

> **A message from a human**
>
> Around ten years ago (~2017) I was working on a small project called _IPMic_
> for allowing Android phones to be used as real-time / live microphones on
> Wi-Fi LAN. It took a few months for a prototype that worked on a terminal of
> a rooted Android phone. The server was a CLI app on a Linux machine.
>
> Today (31/08/2026), with a simple description, Claude Opus 5 developed a
> fully functional app in a few minutes.

Wireless microphones for live presentations, over your own Wi-Fi, with no codec
in the path. An Android phone becomes a microphone; another phone (or a laptop)
becomes the mixer plugged into the PA.

Two modes in one APK:

1. **Microphone** — captures with AAudio in low-latency exclusive mode and ships
   5 ms packets of 48 kHz 16-bit PCM over UDP.
2. **Mixer / server** — receives from up to 8 microphones, runs one jitter
   buffer per source, sums them, limits the bus, and plays out through AAudio.

A desktop app (`desktop/`) is the same two modes again for Linux, macOS and
Windows: the identical Rust engine, with cpal where the phone has Oboe and a
GPUI window where it has Compose. A Python server
(`server/lan_audio_server.py`) speaks the same protocol as a second,
independent implementation. Either way, a laptop can be the mixer instead —
usually the better choice when you want to feed a real interface or a house
console.

Typical end-to-end delay is **33–47 ms**. The wire format and the latency
budget are in [PROTOCOL.md](PROTOCOL.md); how the code is put together is in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

```
  phone (mic)  ─┐
  phone (mic)  ─┼─ UDP 48 kHz PCM ─▶  mixer  ─▶  PA / speaker
  laptop (mic) ─┘                    (phone or laptop)
```

Three front ends, one engine:

```
        Android app            desktop app            Python server
        Compose UI             GPUI window            terminal
        Oboe streams           cpal streams           sounddevice
             └──────── rust/ (the LAU1 engine) ────────┘        │
                                                    a separate implementation
                                                    of the same wire format
```

## Layout

```
rust/                   audio engine: protocol, jitter buffers, mixer, UDP,
                        the stream supervisor, the Oboe streams, the JNI surface
app/src/main/java/      Kotlin UI, foreground service, discovery
desktop/                the same engine behind cpal and a GPUI window
server/                 Python mixing server + a test transmitter
tools/                  test runner for every suite
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
a workflow artifact - it also runs every test suite on every push.

`minSdk` is 26. AAudio's low-latency MMAP path arrives properly in API 27+; on
older devices Oboe falls back to OpenSL ES and capture latency roughly doubles.

## Run the desktop app

```bash
cd desktop
cargo run --release                     # the window: mixer and microphone
cargo run --release -- --list-devices   # what this machine has
```

The window has the same two modes as the phone. **Mixer** binds the audio port,
answers discovery so a phone's *Find server* lands on it, and gives you a
channel strip per microphone with a meter, a fader, a mute, and that source's
buffer depth and loss counters — plus the master fader, the feedback shifter and
the limiter's gain reduction. **Microphone** finds a mixer (or takes an address
typed in), picks an input, and ships.

The window is built on
[gpui-component](https://github.com/longbridge/gpui-component) — its buttons,
sliders, text inputs, titlebar and window frame, painted with this app's own
palette. On Linux that frame gives the window a titlebar, border and resize
edges where the desktop does not provide them — which is every GNOME Wayland
session, because Mutter does not implement server-side decorations. Drag the
title to move, double-click it to maximise, right-click it for the window menu,
and use the buttons at its right to minimise, maximise or close. Where the platform does
draw them — macOS, Windows, X11 under a window manager — you get the native
ones instead.

No screen, or no GPU for GPUI to talk to? The same binary runs on a terminal:

```bash
cargo run --release -- --headless --jitter 15 --output "USB Audio"
cargo run --release -- --headless --mic 192.168.1.50 --input "Scarlett"
```

Flags mirror the Python server's where they overlap: `--port`, `--jitter`,
`--blocksize`, `--name`, `--discovery-port`, `--no-discovery`, plus `--output`
and `--input` (a substring of the device name is enough), `--virtual-mic`,
`--mic HOST` and `--packet`. `lanmic --help` lists them all.

On Linux the build needs ALSA and the GPUI system libraries:

```bash
sudo apt install libasound2-dev libxkbcommon-dev libxkbcommon-x11-dev \
                 libwayland-dev libx11-dev libxcb1-dev libfontconfig1-dev
```

Everything runs at 48 kHz with no resampler anywhere, so a device that will not
do 48 kHz is listed as unusable rather than quietly rate-converted. On Linux
PulseAudio and PipeWire both offer 48 kHz whatever the hardware is doing.

### The mix as a microphone

Under the mixer's output list is a **virtual microphone** switch. Turn it on and
the mix also appears as a capture device called `LAN_Mic`, so a video call, a
recorder or OBS on the same machine can open it as an input - phones on stage,
straight into the meeting. It takes effect immediately, so it can be switched on
mid-session, and `--virtual-mic` turns it on from the command line:

```bash
lanmic --headless --virtual-mic                   # the mix, as LAN_Mic
lanmic --headless --virtual-mic "Front of house"  # under a name of your own
```

This is `module-pipe-source` on PulseAudio and PipeWire, which is every ordinary
Linux desktop and nothing else. On macOS and Windows a virtual microphone is a
driver somebody else installs: get BlackHole or VB-Cable and select it in the
output list instead, which does the same job.

Two things worth knowing. The mix reaches it after the feedback shifter, so turn
the shifter off when the mix is going down a wire rather than into a room. And
it is fed by the mixer's own output callback, so it advances at the rate the
speakers do - a mixer with no working output feeds it nothing.

## Run the Python server

A second implementation of the mixer half, useful for the same job and for
keeping the protocol honest.

```bash
cd server
pip install -r requirements.txt
python3 lan_audio_server.py --list-devices
python3 lan_audio_server.py --jitter 15 --device 3 --blocksize 240
```

Useful flags: `--jitter` (buffer target, ms), `--blocksize` (output block in
frames; 240 = 5 ms), `--channels`, `--gain`, `--port`, `--no-discovery`.

Send either server a test tone without touching a phone:

```bash
python3 test_mic_client.py --tone 440          # finds the server by broadcast
python3 test_mic_client.py --host 192.168.1.50 --mic
```

## Using it at a venue

1. Start the mixer first (phone in **Mixer / server** mode, the desktop app, or
   the Python server). All three show the IP addresses they are listening on.
   Pick the **output** the mix should play out of - a USB interface into the
   desk beats any built-in speaker.
2. On each microphone phone: pick the **input** to capture from, then **Find
   server**, or type the address, then **GO LIVE**. Grant the microphone
   permission once.
3. Speak. The mixer lists each microphone with a level meter, its current
   buffer depth, and its loss counters.

Both device menus list what is plugged in now and follow it as things are
plugged and unplugged. Bluetooth is deliberately absent from both: SCO and A2DP
each cost more delay than this whole app's budget. On the phone, a choice is
remembered by what the device is rather than by the id Android gave it, so it
survives a replug and a reboot; if it is not there when you start, Android
routes the stream and the screen says so.

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
| Feedback howl | Raise the feedback suppression on the mixer; then move mics out of the speakers' throw |

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
tools/run_tests.sh              # engine + desktop app + Python server
tools/run_tests.sh --strict     # the same, plus rustfmt and clippy
```

`cargo test` covers everything except the two Oboe streams, including an
end-to-end suite that runs a ramp from the capture encoder, over a real
loopback socket, through the jitter buffers and out of the mixer, and checks it
sample by sample. No device required. The desktop crate's own suite covers
everything in it that is not a cpal stream or a GPUI element: device-config
selection, the format conversions, the discovery exchange over loopback, the
command line, and the text field. The Python suite covers the same behaviours
as the engine in `lan_audio_server.py`, so the two implementations stay in
step. CI runs all three on every push.

## Limits

* 8 simultaneous microphones on any mixer running the Rust engine
  (`MAX_SOURCES` in `rust/src/mixer.rs`).
* Mono on the wire; the mixer plays the same mix to every output channel.
* The virtual microphone is Linux-only: it is `module-pipe-source`, so it needs
  PulseAudio or PipeWire. Elsewhere, a loopback driver selected as the output
  does the same job.
* No encryption and no authentication. Anyone on the LAN can send audio to the
  mixer. Use a private AP.
* Feedback suppression is a frequency shift, not echo cancellation, and it buys
  roughly 6-10 dB before ringing rather than solving the problem. There is no
  AEC and there cannot be one worth having: the microphone and the loudspeaker
  are different devices, so the capturing end has no reference to cancel
  against. Keep microphones out of the speakers' throw.
