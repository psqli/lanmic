//! Mixer end: bind the audio port, keep one jitter buffer per source, sum them.
//!
//! As on the transmitter side, only the Oboe output stream is Android-specific.
//! The packet router below runs anywhere, so the whole network path - parse,
//! route, conceal, mix - is exercised by the tests in this file over a real
//! loopback socket.

use std::io;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::mixer::{self, Mixer, SourceWriter, Table};
use crate::net;
use crate::protocol::{
    wire_to_pcm, Header, PacketType, HEADER_BYTES, MAX_FRAMES_PER_PACKET, MAX_PACKET_BYTES,
    SAMPLE_RATE, WIRE_CHANNELS,
};
use crate::util::{load_milli, store_milli};

/// A source with nothing to say for this long is retired, whether or not it
/// managed a BYE.
pub const SOURCE_TIMEOUT_MS: i64 = 2000;
const REAP_INTERVAL_MS: i64 = 500;
/// Short enough that a stop is noticed promptly, long enough to stay asleep.
const RECV_TIMEOUT: Duration = Duration::from_millis(200);

pub const MIN_JITTER_MS: i32 = 5;
pub const MAX_JITTER_MS: i32 = 200;

#[derive(Debug, Default, Clone, Copy)]
pub struct RxStats {
    pub packets: u64,
    pub bad_packets: u64,
    pub active_sources: u32,
    pub xruns: u32,
    pub master_peak: f32,
    pub limiter_gain: f32,
    pub latency_ms: f32,
    pub running: bool,
}

#[derive(Debug, Default)]
pub struct RxShared {
    pub running: AtomicBool,
    packets: AtomicU64,
    bad_packets: AtomicU64,
    xruns: AtomicU32,
    latency_milli: AtomicU32,
    /// Set by the Oboe error callback, acted on by the supervisor thread.
    pub restart_requested: AtomicBool,
}

impl RxShared {
    pub fn reset_for_session(&self) {
        self.packets.store(0, Ordering::Relaxed);
        self.bad_packets.store(0, Ordering::Relaxed);
        self.xruns.store(0, Ordering::Relaxed);
        self.latency_milli.store(0, Ordering::Relaxed);
        self.restart_requested.store(false, Ordering::Relaxed);
    }

    pub fn set_xruns(&self, xruns: u32) {
        self.xruns.store(xruns, Ordering::Relaxed);
    }

    pub fn set_latency_ms(&self, ms: f32) {
        store_milli(&self.latency_milli, ms);
    }

    pub fn stats(&self, table: &Table) -> RxStats {
        RxStats {
            packets: self.packets.load(Ordering::Relaxed),
            bad_packets: self.bad_packets.load(Ordering::Relaxed),
            active_sources: table.active_sources(),
            xruns: self.xruns.load(Ordering::Relaxed),
            master_peak: table.master_peak(),
            limiter_gain: table.limiter_gain(),
            latency_ms: load_milli(&self.latency_milli),
            running: self.running.load(Ordering::Acquire),
        }
    }
}

/// What one call to [`PacketRouter::poll`] did, mostly so tests can assert on
/// it without reaching into counters.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Polled {
    /// Nothing arrived before the read timeout.
    Idle,
    Audio {
        ssrc: u32,
        frames: usize,
    },
    Hello(u32),
    Bye(u32),
    /// Not LAU1, or LAU1 that makes no sense.
    Malformed,
    /// A well-formed packet this version has no use for.
    Ignored,
    /// All eight slots are taken and this source is not one of them.
    TableFull(u32),
}

/// Network-thread half: one socket, one source table.
pub struct PacketRouter {
    socket: UdpSocket,
    sources: SourceWriter,
    shared: Arc<RxShared>,
    buf: Vec<u8>,
    pcm: Vec<i16>,
    last_reap_ms: i64,
}

impl PacketRouter {
    /// Reads at most one packet and acts on it. Blocks for at most the socket's
    /// read timeout, so a caller can treat this as its loop body and still
    /// notice a shutdown promptly.
    pub fn poll(&mut self, now_ms: i64) -> Polled {
        let received = match self.socket.recv_from(&mut self.buf) {
            Ok((n, _from)) => Some(n),
            Err(ref e) if net::is_would_block(e) => None,
            Err(e) => {
                // Not something to spin on: this loop has no other pacing, and
                // a socket erroring every call would peg a core.
                log::warn!("audio socket error: {e}");
                std::thread::sleep(RECV_TIMEOUT);
                None
            }
        };

        if now_ms - self.last_reap_ms > REAP_INTERVAL_MS {
            self.sources.reap_stale(now_ms, SOURCE_TIMEOUT_MS);
            self.last_reap_ms = now_ms;
        }

        let Some(n) = received else {
            return Polled::Idle;
        };
        let Some(header) = Header::decode(&self.buf[..n]) else {
            self.shared.bad_packets.fetch_add(1, Ordering::Relaxed);
            return Polled::Malformed;
        };
        self.shared.packets.fetch_add(1, Ordering::Relaxed);

        match header.kind() {
            Some(PacketType::Bye) => {
                self.sources.retire(header.ssrc);
                return Polled::Bye(header.ssrc);
            }
            Some(PacketType::Hello) => {
                // Light the channel strip up now rather than on first audio.
                return match self.sources.acquire(header.ssrc, now_ms) {
                    Some(_) => Polled::Hello(header.ssrc),
                    None => Polled::TableFull(header.ssrc),
                };
            }
            Some(PacketType::Audio) => {}
            // Discovery lives on its own port, and an unknown type is a future
            // version's business, not ours.
            _ => return Polled::Ignored,
        }

        let frames = (n - HEADER_BYTES) / 2;
        if frames == 0 || frames > MAX_FRAMES_PER_PACKET || header.channels != WIRE_CHANNELS {
            self.shared.bad_packets.fetch_add(1, Ordering::Relaxed);
            return Polled::Malformed;
        }

        let Some(slot) = self.sources.acquire(header.ssrc, now_ms) else {
            return Polled::TableFull(header.ssrc);
        };

        let muted = header.muted();
        let pcm = if muted {
            None
        } else {
            wire_to_pcm(&mut self.pcm[..frames], &self.buf[HEADER_BYTES..n]);
            Some(&self.pcm[..frames])
        };
        self.sources
            .on_packet(slot, header.seq, header.timestamp, pcm, frames, muted);
        Polled::Audio {
            ssrc: header.ssrc,
            frames,
        }
    }

    pub fn table(&self) -> &Arc<Table> {
        self.sources.table()
    }

    /// The port actually bound, which is only interesting when 0 was asked for.
    pub fn local_port(&self) -> u16 {
        self.socket.local_addr().map(|a| a.port()).unwrap_or(0)
    }
}

/// Binds the audio port and builds the router and the mixer around it.
/// `jitter_ms` is the nominal buffer depth. Port 0 binds an ephemeral port,
/// which is how the tests get one; [`net::validate_port`] is what keeps a 0
/// from reaching here off the UI.
pub fn build(
    port: u16,
    jitter_ms: i32,
    shared: Arc<RxShared>,
) -> io::Result<(PacketRouter, Mixer, Arc<Table>)> {
    let jitter_ms = jitter_ms.clamp(MIN_JITTER_MS, MAX_JITTER_MS);
    let target_frames = (SAMPLE_RATE as i32 * jitter_ms / 1000) as usize;

    let socket = net::open_receiver(port, RECV_TIMEOUT)?;
    let (table, sources, mixer) = mixer::build(target_frames);
    Ok((
        PacketRouter {
            socket,
            sources,
            shared,
            buf: vec![0u8; MAX_PACKET_BYTES],
            pcm: vec![0i16; MAX_FRAMES_PER_PACKET],
            last_reap_ms: 0,
        },
        mixer,
        table,
    ))
}

/// Copies the mono bus into an interleaved stereo frame buffer, padding with
/// silence if the mixer produced less than was asked for.
pub fn write_stereo(mix: &[f32], out: &mut [(i16, i16)]) {
    for (frame, &s) in out.iter_mut().zip(mix) {
        let v = (s * 32767.0).clamp(-32768.0, 32767.0) as i16;
        *frame = (v, v);
    }
    for frame in out.iter_mut().skip(mix.len()) {
        *frame = (0, 0);
    }
}
