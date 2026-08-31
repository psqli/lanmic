//! Microphone end: capture, downmix, packetise, ship.
//!
//! Everything except the Oboe input stream is portable, so the interesting
//! parts - the downmix and gain staging, the drop accounting, and the
//! packetiser's timestamps - are exercised by the tests in this file rather
//! than only on a phone.
//!
//! The two halves meet in an [`rtrb`] ring: the audio callback owns the write
//! end and never touches the socket, and the sender thread owns the read end
//! and does the syscalls.

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::meter::update_peak_meter;
use crate::net;
use crate::protocol::{
    pcm_to_wire, Header, PacketType, FLAG_MUTED, HEADER_BYTES, MAX_FRAMES_PER_PACKET,
    MAX_PACKET_BYTES, SAMPLE_RATE,
};
use crate::util::{load_milli, store_milli};

/// Half a second of capture headroom. If this ever fills, the network is gone
/// and stale audio is worthless anyway.
const RING_FRAMES: usize = (SAMPLE_RATE / 2) as usize;

/// Largest input burst the callback will handle without allocating.
const MAX_CALLBACK_FRAMES: usize = 8192;

pub const MIN_FRAMES_PER_PACKET: usize = 60;

#[derive(Debug, Default, Clone, Copy)]
pub struct TxStats {
    pub packets_sent: u64,
    /// Frames the ring could not take: the sender thread fell behind.
    pub frames_dropped: u64,
    pub send_errors: u32,
    pub xruns: u32,
    pub peak: f32,
    pub latency_ms: f32,
    pub running: bool,
}

/// State every thread in the transmitter can see.
#[derive(Debug)]
pub struct TxShared {
    pub running: AtomicBool,
    muted: AtomicBool,
    gain_milli: AtomicU32,
    peak_milli: AtomicU32,
    packets_sent: AtomicU64,
    frames_dropped: AtomicU64,
    send_errors: AtomicU32,
    xruns: AtomicU32,
    latency_milli: AtomicU32,
    /// Frames the audio callback could not fit in the ring. The sender thread
    /// folds these into the next packet's timestamp, so the receiver conceals
    /// a hole instead of splicing two unrelated instants together and running
    /// permanently early from then on.
    pending_gap: AtomicU32,
    /// Set by the Oboe error callback, acted on by the supervisor thread.
    pub restart_requested: AtomicBool,
}

impl Default for TxShared {
    fn default() -> Self {
        Self {
            running: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            gain_milli: AtomicU32::new(1000),
            peak_milli: AtomicU32::new(0),
            packets_sent: AtomicU64::new(0),
            frames_dropped: AtomicU64::new(0),
            send_errors: AtomicU32::new(0),
            xruns: AtomicU32::new(0),
            latency_milli: AtomicU32::new(0),
            pending_gap: AtomicU32::new(0),
            restart_requested: AtomicBool::new(false),
        }
    }
}

impl TxShared {
    /// A session starts unmuted, at unity, with clean counters. Without this a
    /// mute left over from the previous session survives into the new one
    /// while the UI, whose own state did reset, shows the mic as live.
    pub fn reset_for_session(&self) {
        self.muted.store(false, Ordering::Relaxed);
        self.gain_milli.store(1000, Ordering::Relaxed);
        self.peak_milli.store(0, Ordering::Relaxed);
        self.packets_sent.store(0, Ordering::Relaxed);
        self.frames_dropped.store(0, Ordering::Relaxed);
        self.send_errors.store(0, Ordering::Relaxed);
        self.xruns.store(0, Ordering::Relaxed);
        self.latency_milli.store(0, Ordering::Relaxed);
        self.pending_gap.store(0, Ordering::Relaxed);
        self.restart_requested.store(false, Ordering::Relaxed);
    }

    pub fn set_gain(&self, gain: f32) {
        // Arrives from the UI across a language boundary: clamp, do not trust.
        let g = if gain.is_nan() || gain <= 0.0 {
            0.0
        } else {
            gain.min(8.0)
        };
        store_milli(&self.gain_milli, g);
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    pub fn stats(&self) -> TxStats {
        TxStats {
            packets_sent: self.packets_sent.load(Ordering::Relaxed),
            frames_dropped: self.frames_dropped.load(Ordering::Relaxed),
            send_errors: self.send_errors.load(Ordering::Relaxed),
            xruns: self.xruns.load(Ordering::Relaxed),
            peak: load_milli(&self.peak_milli),
            latency_ms: load_milli(&self.latency_milli),
            running: self.running.load(Ordering::Acquire),
        }
    }

    pub fn set_xruns(&self, xruns: u32) {
        self.xruns.store(xruns, Ordering::Relaxed);
    }

    pub fn set_latency_ms(&self, ms: f32) {
        store_milli(&self.latency_milli, ms);
    }
}

/// Audio-thread half: downmix, gain, meter, and into the ring.
pub struct CaptureEncoder {
    ring: Producer<i16>,
    shared: Arc<TxShared>,
    mono: Vec<i16>,
}

impl CaptureEncoder {
    /// Takes one interleaved burst and returns how many frames reached the
    /// ring. Allocates nothing: a burst larger than the scratch is dropped and
    /// accounted for, exactly as a ring overflow would be.
    pub fn push(&mut self, input: &[i16], channels: usize) -> usize {
        let channels = channels.max(1);
        let frames = input.len() / channels;
        if frames == 0 {
            return 0;
        }
        if frames > self.mono.len() {
            self.drop_frames(frames);
            return 0;
        }

        let gain = if self.shared.muted() {
            0.0
        } else {
            load_milli(&self.shared.gain_milli)
        };

        let mono = &mut self.mono[..frames];
        for (out, frame) in mono.iter_mut().zip(input.chunks_exact(channels)) {
            let sum: i32 = frame.iter().map(|&s| s as i32).sum();
            let v = sum as f32 / channels as f32 * gain;
            *out = v.clamp(-32768.0, 32767.0) as i16;
        }

        let peak = mono.iter().fold(0i32, |a, &s| a.max((s as i32).abs()));
        update_peak_meter(&self.shared.peak_milli, peak as f32 / 32768.0);

        let (pushed, _) = self.ring.push_partial_slice(mono);
        let n = pushed.len();
        if n < frames {
            self.drop_frames(frames - n);
        }
        n
    }

    fn drop_frames(&self, frames: usize) {
        self.shared
            .frames_dropped
            .fetch_add(frames as u64, Ordering::Relaxed);
        self.shared
            .pending_gap
            .fetch_add(frames as u32, Ordering::Relaxed);
    }
}

/// Sender-thread half: whole packets out of the ring and onto the wire.
pub struct Packetiser {
    socket: UdpSocket,
    ring: Consumer<i16>,
    shared: Arc<TxShared>,
    frames_per_packet: usize,
    ssrc: u32,
    seq: u32,
    timestamp: u32,
    packet: Vec<u8>,
    pcm: Vec<i16>,
}

impl Packetiser {
    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    /// Sends every whole packet the ring currently holds. Returns how many.
    pub fn pump(&mut self) -> usize {
        let fpp = self.frames_per_packet;
        let mut sent = 0;
        while self.ring.slots() >= fpp {
            let (popped, _) = self.ring.pop_partial_slice(&mut self.pcm[..fpp]);
            if popped.len() < fpp {
                break;
            }

            // Fold in anything the capture side had to throw away, so the
            // timeline stays honest about frames that were captured and lost.
            self.timestamp = self
                .timestamp
                .wrapping_add(self.shared.pending_gap.swap(0, Ordering::Relaxed));

            let mut header = Header::new(PacketType::Audio);
            header.flags = if self.shared.muted() { FLAG_MUTED } else { 0 };
            header.ssrc = self.ssrc;
            header.seq = self.seq;
            header.timestamp = self.timestamp;
            self.seq = self.seq.wrapping_add(1);
            self.timestamp = self.timestamp.wrapping_add(fpp as u32);

            let len = HEADER_BYTES + fpp * 2;
            self.packet[..HEADER_BYTES].copy_from_slice(&header.to_bytes());
            pcm_to_wire(&mut self.packet[HEADER_BYTES..len], &self.pcm[..fpp]);

            match self.socket.send(&self.packet[..len]) {
                Ok(_) => {
                    self.shared.packets_sent.fetch_add(1, Ordering::Relaxed);
                    sent += 1;
                }
                Err(ref e) if net::is_would_block(e) => {
                    // The radio is busy. Dropping is the right answer here:
                    // the packet behind this one is worth more than this one.
                }
                Err(_) => {
                    self.shared.send_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        sent
    }

    /// HELLO at start, BYE at stop - repeated, because neither is required for
    /// correctness and neither is worth retransmitting properly.
    pub fn send_control(&self, kind: PacketType, repeats: usize) {
        let mut header = Header::new(kind);
        header.ssrc = self.ssrc;
        header.seq = self.seq;
        header.timestamp = self.timestamp;
        let bytes = header.to_bytes();
        for _ in 0..repeats {
            let _ = self.socket.send(&bytes);
        }
    }
}

/// Builds the capture and sender halves around one socket.
pub fn build(
    socket: UdpSocket,
    frames_per_packet: usize,
    shared: Arc<TxShared>,
) -> (CaptureEncoder, Packetiser) {
    let fpp = frames_per_packet.clamp(MIN_FRAMES_PER_PACKET, MAX_FRAMES_PER_PACKET);
    let (producer, consumer) = RingBuffer::new(RING_FRAMES);
    (
        CaptureEncoder {
            ring: producer,
            shared: shared.clone(),
            mono: vec![0i16; MAX_CALLBACK_FRAMES],
        },
        Packetiser {
            socket,
            ring: consumer,
            shared,
            frames_per_packet: fpp,
            ssrc: random_ssrc(),
            seq: 0,
            timestamp: 0,
            packet: vec![0u8; MAX_PACKET_BYTES],
            pcm: vec![0i16; MAX_FRAMES_PER_PACKET],
        },
    )
}

/// A 32-bit id chosen once per capture session. It only has to be unlikely to
/// collide with the handful of other microphones on one LAN, so the hasher the
/// standard library already seeds from the OS is ample and saves a dependency.
/// The low bit is forced so an ssrc is never zero.
fn random_ssrc() -> u32 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut h = RandomState::new().build_hasher();
    h.write_u64(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0),
    );
    (h.finish() as u32) | 1
}
