//! LAU1 — the low-latency LAN audio engine behind the LAN Mic app.
//!
//! One crate contains both ends. A phone is either a **microphone** (capture →
//! UDP) or a **mixer** (UDP → jitter buffers → sum → speaker), never both at
//! once.
//!
//! ```text
//! MICROPHONE                                   MIXER
//!                         UDP 48 kHz mono PCM
//! capture ──▶ ring ──▶ sender ═══════════════▶ network ──▶ jitter buffer ─┐
//! (audio cb)           (thread)                (thread)    (one per ssrc) │
//!                                                                         ▼
//!                                                          output ◀── sum ─▶ limiter
//!                                                        (audio cb)
//! ```
//!
//! # Which thread may touch what
//!
//! In the C++ this engine grew from, that question was answered by a table in
//! the documentation. Here it is answered by the types. Every lock-free buffer
//! is split into a writer half and a reader half that are separate,
//! non-[`Clone`] types: [`jitter::JitterWriter`] cannot read,
//! [`jitter::JitterReader`] cannot write, [`mixer::SourceWriter`] holds every
//! write half and [`mixer::Mixer`] every read half, and moving one to a thread
//! moves it away from the other. Sharing one with two threads does not
//! compile. What genuinely crosses threads - counters, gains, mute flags -
//! is atomics, in the `*Shared` structs.
//!
//! # What is portable and what is not
//!
//! Everything except [`android`] runs anywhere, which is what lets the test
//! suite exercise the whole path - capture, packetise, socket, conceal, mix -
//! without a device. Only the two Oboe streams and the JNI surface need a
//! phone.

pub mod jitter;
pub mod meter;
pub mod mixer;
pub mod net;
pub mod protocol;
pub mod receiver;
pub mod transmitter;
pub mod util;

#[cfg(target_os = "android")]
pub mod android;
#[cfg(target_os = "android")]
mod bridge;
