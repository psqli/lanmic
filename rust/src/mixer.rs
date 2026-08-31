//! Fixed-slot source table, summing mixer and a lookahead-free limiter.
//!
//! Slots are claimed and retired by the network thread and read by the audio
//! callback. As with the jitter buffer, the two roles are separate types:
//! [`SourceWriter`] holds every buffer's write half, [`Mixer`] every read half,
//! and the metadata they share sits in [`Table`] as atomics. The table never
//! allocates and never grows - eight slots, and a ninth microphone is refused
//! rather than accommodated.

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;

use crate::jitter::{self, JitterReader, JitterShared, JitterWriter};
use crate::meter::{update_peak_meter, METER_CEILING, METER_FULL_SCALE};

pub const MAX_SOURCES: usize = 8;

/// Largest block the audio callback will be handed without falling back to
/// silence. 8192 frames is 170 ms at 48 kHz, far above any low-latency burst.
pub const MAX_CALLBACK_FRAMES: usize = 8192;

const SLOT_FREE: u32 = 0;
const SLOT_ACTIVE: u32 = 1;

#[derive(Debug, Default, Clone, Copy)]
pub struct SourceSnapshot {
    pub ssrc: u32,
    /// 0..=[`METER_CEILING`] in milli-units; 1000 is full scale.
    pub peak_milli: u32,
    pub buffer_frames: u32,
    pub packets: u32,
    pub lost: u32,
    pub underruns: u32,
    /// Milliseconds since the last packet.
    pub age_ms: u32,
    pub muted: bool,
    /// 0..=[`METER_CEILING`] in milli-units; 1000 is unity.
    pub gain_milli: u32,
}

struct Slot {
    state: AtomicU32,
    /// Written by the network thread on claim, read by the UI thread. Ordered
    /// against `state`, but atomic in its own right so those reads are not a
    /// plain race on a slot being torn down.
    ssrc: AtomicU32,
    last_seen_ms: AtomicI64,
    peak_milli: AtomicU32,
    gain_milli: AtomicU32,
    muted: AtomicU32,
    jitter: JitterShared,
}

impl Slot {
    fn new(target_frames: usize) -> Self {
        Self {
            state: AtomicU32::new(SLOT_FREE),
            ssrc: AtomicU32::new(0),
            last_seen_ms: AtomicI64::new(0),
            peak_milli: AtomicU32::new(0),
            gain_milli: AtomicU32::new(METER_FULL_SCALE),
            muted: AtomicU32::new(0),
            jitter: JitterShared::new(target_frames),
        }
    }

    fn is(&self, ssrc: u32) -> bool {
        self.state.load(Ordering::Acquire) == SLOT_ACTIVE
            && self.ssrc.load(Ordering::Relaxed) == ssrc
    }
}

/// Everything both threads and the UI can see. Held behind an `Arc` by all
/// three; nothing in here needs a lock.
pub struct Table {
    slots: [Slot; MAX_SOURCES],
    master_gain_milli: AtomicU32,
    master_peak_milli: AtomicU32,
    limiter_gain_milli: AtomicU32,
    active_sources: AtomicU32,
}

impl Table {
    fn new(target_frames: usize) -> Self {
        Self {
            slots: std::array::from_fn(|_| Slot::new(target_frames)),
            master_gain_milli: AtomicU32::new(METER_FULL_SCALE),
            master_peak_milli: AtomicU32::new(0),
            limiter_gain_milli: AtomicU32::new(METER_FULL_SCALE),
            active_sources: AtomicU32::new(0),
        }
    }

    /// Gains arrive from the UI across a language boundary, so they are clamped
    /// here rather than trusted.
    fn clamp_gain(gain: f32) -> u32 {
        if gain.is_nan() || gain <= 0.0 {
            return 0;
        }
        ((gain * METER_FULL_SCALE as f32) as u32).min(METER_CEILING)
    }

    pub fn set_master_gain(&self, gain: f32) {
        self.master_gain_milli
            .store(Self::clamp_gain(gain), Ordering::Relaxed);
    }

    pub fn set_source_gain(&self, ssrc: u32, gain: f32) {
        let g = Self::clamp_gain(gain);
        for slot in &self.slots {
            if slot.is(ssrc) {
                slot.gain_milli.store(g, Ordering::Relaxed);
            }
        }
    }

    pub fn set_source_muted(&self, ssrc: u32, muted: bool) {
        for slot in &self.slots {
            if slot.is(ssrc) {
                slot.muted.store(muted as u32, Ordering::Relaxed);
            }
        }
    }

    pub fn master_peak(&self) -> f32 {
        self.master_peak_milli.load(Ordering::Relaxed) as f32 / METER_FULL_SCALE as f32
    }

    pub fn limiter_gain(&self) -> f32 {
        self.limiter_gain_milli.load(Ordering::Relaxed) as f32 / METER_FULL_SCALE as f32
    }

    pub fn active_sources(&self) -> u32 {
        self.active_sources.load(Ordering::Relaxed)
    }

    /// One entry per active source, for the UI to poll.
    pub fn snapshot(&self, now_ms: i64, out: &mut Vec<SourceSnapshot>) {
        out.clear();
        for slot in &self.slots {
            if slot.state.load(Ordering::Acquire) != SLOT_ACTIVE {
                continue;
            }
            let js = slot.jitter.stats();
            out.push(SourceSnapshot {
                ssrc: slot.ssrc.load(Ordering::Relaxed),
                peak_milli: slot.peak_milli.load(Ordering::Relaxed),
                buffer_frames: js.fill_frames,
                packets: js.packets,
                lost: js.lost,
                underruns: js.underruns,
                age_ms: now_ms
                    .saturating_sub(slot.last_seen_ms.load(Ordering::Relaxed))
                    .clamp(0, u32::MAX as i64) as u32,
                muted: slot.muted.load(Ordering::Relaxed) != 0,
                gain_milli: slot.gain_milli.load(Ordering::Relaxed),
            });
        }
    }
}

/// Builds a table and its two halves. Every ring is allocated here, once:
/// claiming a slot later reuses one rather than making a new one, which is why
/// [`JitterWriter::reset`] exists.
pub fn build(target_frames: usize) -> (Arc<Table>, SourceWriter, Mixer) {
    let table = Arc::new(Table::new(target_frames));
    let mut writers = Vec::with_capacity(MAX_SOURCES);
    let mut readers = Vec::with_capacity(MAX_SOURCES);
    for slot in &table.slots {
        let (w, r) = jitter::pair(&slot.jitter);
        writers.push(w);
        readers.push(r);
    }
    fn to_array<T>(v: Vec<T>) -> [T; MAX_SOURCES] {
        v.try_into()
            .unwrap_or_else(|_| unreachable!("built exactly MAX_SOURCES halves"))
    }
    let writer = SourceWriter {
        table: table.clone(),
        writers: to_array(writers),
    };
    let mixer = Mixer {
        table: table.clone(),
        readers: to_array(readers),
        scratch: vec![0i16; MAX_CALLBACK_FRAMES],
        mix: vec![0.0f32; MAX_CALLBACK_FRAMES],
        limiter: Limiter::new(0.98, crate::protocol::SAMPLE_RATE, 150.0),
    };
    (table, writer, mixer)
}

/// Network-thread half: claims slots and writes packets into them.
pub struct SourceWriter {
    table: Arc<Table>,
    writers: [JitterWriter; MAX_SOURCES],
}

impl SourceWriter {
    pub fn table(&self) -> &Arc<Table> {
        &self.table
    }

    /// The slot this source's packets belong in, claiming a free one if the
    /// source is new. `None` when all eight are taken.
    pub fn acquire(&mut self, ssrc: u32, now_ms: i64) -> Option<usize> {
        for (i, slot) in self.table.slots.iter().enumerate() {
            if slot.is(ssrc) {
                slot.last_seen_ms.store(now_ms, Ordering::Relaxed);
                return Some(i);
            }
        }
        for (i, slot) in self.table.slots.iter().enumerate() {
            if slot.state.load(Ordering::Acquire) == SLOT_FREE {
                // Reset before publishing: the release store below is what
                // makes the fresh ssrc and the retired audio visible together.
                self.writers[i].reset(&slot.jitter);
                slot.ssrc.store(ssrc, Ordering::Relaxed);
                slot.peak_milli.store(0, Ordering::Relaxed);
                slot.muted.store(0, Ordering::Relaxed);
                slot.gain_milli.store(METER_FULL_SCALE, Ordering::Relaxed);
                slot.last_seen_ms.store(now_ms, Ordering::Relaxed);
                slot.state.store(SLOT_ACTIVE, Ordering::Release);
                return Some(i);
            }
        }
        None
    }

    pub fn on_packet(
        &mut self,
        slot: usize,
        seq: u32,
        timestamp: u32,
        pcm: Option<&[i16]>,
        frames: usize,
        muted: bool,
    ) {
        self.writers[slot].on_packet(
            &self.table.slots[slot].jitter,
            seq,
            timestamp,
            pcm,
            frames,
            muted,
        );
    }

    pub fn retire(&mut self, ssrc: u32) {
        for slot in &self.table.slots {
            if slot.is(ssrc) {
                slot.state.store(SLOT_FREE, Ordering::Release);
            }
        }
    }

    /// Drops sources that have gone quiet. A microphone that leaves without a
    /// BYE - out of range, battery flat - is retired this way.
    pub fn reap_stale(&mut self, now_ms: i64, timeout_ms: i64) {
        for slot in &self.table.slots {
            if slot.state.load(Ordering::Acquire) != SLOT_ACTIVE {
                continue;
            }
            if now_ms - slot.last_seen_ms.load(Ordering::Relaxed) > timeout_ms {
                slot.state.store(SLOT_FREE, Ordering::Release);
            }
        }
    }
}

/// Audio-thread half: sums every active source, applies master gain and the
/// limiter, and publishes the meters. Allocates nothing.
pub struct Mixer {
    table: Arc<Table>,
    readers: [JitterReader; MAX_SOURCES],
    scratch: Vec<i16>,
    mix: Vec<f32>,
    limiter: Limiter,
}

impl Mixer {
    /// One audio-callback pass. Returns the finished mono bus, which is
    /// shorter than `frames` only if asked for more than
    /// [`MAX_CALLBACK_FRAMES`] - the caller pads with silence rather than
    /// having this allocate.
    pub fn render(&mut self, frames: usize) -> &[f32] {
        let n = frames.min(self.mix.len());
        let mix = &mut self.mix[..n];
        mix.fill(0.0);

        let mut active = 0u32;
        for (reader, slot) in self.readers.iter_mut().zip(&self.table.slots) {
            if slot.state.load(Ordering::Acquire) != SLOT_ACTIVE {
                continue;
            }
            active += 1;
            let scratch = &mut self.scratch[..n];
            reader.read(&slot.jitter, scratch);

            // A muted source is still drained, so it does not build a backlog
            // that plays out when it is unmuted.
            let gain = if slot.muted.load(Ordering::Relaxed) != 0 {
                0.0
            } else {
                slot.gain_milli.load(Ordering::Relaxed) as f32 / METER_FULL_SCALE as f32
            };

            let mut peak = 0.0f32;
            for (out, &s) in mix.iter_mut().zip(scratch.iter()) {
                let v = s as f32 * (1.0 / 32768.0) * gain;
                *out += v;
                peak = peak.max(v.abs());
            }
            update_peak_meter(&slot.peak_milli, peak);
        }
        self.table.active_sources.store(active, Ordering::Relaxed);

        let master =
            self.table.master_gain_milli.load(Ordering::Relaxed) as f32 / METER_FULL_SCALE as f32;
        if master != 1.0 {
            for s in mix.iter_mut() {
                *s *= master;
            }
        }

        self.limiter.process(mix);
        self.table.limiter_gain_milli.store(
            (self.limiter.gain() * METER_FULL_SCALE as f32) as u32,
            Ordering::Relaxed,
        );

        let peak = mix.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
        update_peak_meter(&self.table.master_peak_milli, peak);

        &self.mix[..n]
    }
}

/// Zero-latency peak limiter: instant attack, exponential release, no
/// lookahead and so no added delay. Applied per sample, which is what lets it
/// promise the ceiling rather than approach it.
pub struct Limiter {
    ceiling: f32,
    release: f32,
    gain: f32,
}

impl Limiter {
    pub fn new(ceiling: f32, sample_rate: u32, release_ms: f32) -> Self {
        Self {
            ceiling,
            release: (-1.0 / (sample_rate as f32 * release_ms * 0.001)).exp(),
            gain: 1.0,
        }
    }

    pub fn process(&mut self, x: &mut [f32]) {
        for s in x.iter_mut() {
            let a = s.abs();
            let wanted = if a > self.ceiling {
                self.ceiling / a
            } else {
                1.0
            };
            self.gain = if wanted < self.gain {
                wanted
            } else {
                wanted + (self.gain - wanted) * self.release
            };
            *s *= self.gain;
        }
    }

    pub fn gain(&self) -> f32 {
        self.gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: usize = 480; // 10 ms

    /// Feeds `packets` packets of `frames` frames into a slot and drains the
    /// priming silence, leaving the mixer pointed at real audio.
    fn feed(w: &mut SourceWriter, slot: usize, value: i16, packets: u32, frames: usize) {
        let pcm = vec![value; frames];
        for i in 0..packets {
            w.on_packet(slot, i, i * frames as u32, Some(&pcm), frames, false);
        }
    }

    #[test]
    fn sources_sum_and_respond_to_gain_and_mute() {
        let (table, mut w, mut m) = build(TARGET);
        let a = w.acquire(0xAAAA, 0).unwrap();
        let b = w.acquire(0xBBBB, 0).unwrap();
        assert_eq!(w.acquire(0xAAAA, 0), Some(a), "same ssrc, same slot");

        feed(&mut w, a, 8000, 5, 240);
        feed(&mut w, b, 8000, 5, 240);

        // Two blocks of priming silence, then the audio.
        assert_eq!(m.render(240).len(), 240);
        assert_eq!(table.active_sources(), 2);
        assert!(m.render(240)[0].abs() < 1e-6);

        let expect = 2.0 * 8000.0 / 32768.0;
        assert!((m.render(240)[0] - expect).abs() < 1e-3);

        table.set_source_muted(0xBBBB, true);
        assert!((m.render(240)[0] - expect / 2.0).abs() < 1e-3);

        table.set_source_muted(0xBBBB, false);
        table.set_source_gain(0xAAAA, 0.5);
        assert!((m.render(240)[0] - expect * 0.75).abs() < 1e-3);
    }

    #[test]
    fn retiring_and_reaping_free_the_slot() {
        let (table, mut w, mut m) = build(TARGET);
        w.acquire(0xAAAA, 0).unwrap();
        w.acquire(0xBBBB, 0).unwrap();
        m.render(240);
        assert_eq!(table.active_sources(), 2);

        w.retire(0xBBBB);
        m.render(240);
        assert_eq!(table.active_sources(), 1);

        let mut snaps = Vec::new();
        table.snapshot(10, &mut snaps);
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].ssrc, 0xAAAA);
        assert_eq!(snaps[0].age_ms, 10);

        w.reap_stale(5000, 2000);
        table.snapshot(5000, &mut snaps);
        assert!(snaps.is_empty());
    }

    #[test]
    fn a_ninth_microphone_is_refused_rather_than_accommodated() {
        let (_table, mut w, _m) = build(240);
        for i in 0..MAX_SOURCES {
            assert!(w.acquire(1000 + i as u32, 0).is_some());
        }
        assert_eq!(w.acquire(9999, 0), None);
    }

    #[test]
    fn a_rejoining_source_does_not_play_the_previous_session() {
        let (_table, mut w, mut m) = build(240);
        let slot = w.acquire(0xC0DE, 0).unwrap();
        feed(&mut w, slot, 9000, 6, 240);
        m.render(240); // the mixer is live on this slot

        w.retire(0xC0DE);
        let again = w.acquire(0xC0DE, 0).unwrap();
        assert_eq!(again, slot, "the same slot is reused");

        let pcm = [1234i16; 240];
        w.on_packet(again, 0, 0, Some(&pcm), 240, false);
        assert!(
            m.render(240)[0].abs() < 1e-6,
            "priming silence, not the old 9000s"
        );
        assert!((m.render(240)[0] - 1234.0 / 32768.0).abs() < 1e-4);
    }

    #[test]
    fn gains_from_across_the_boundary_are_clamped() {
        let (table, mut w, mut m) = build(240);
        w.acquire(0xAAAA, 0).unwrap();

        table.set_source_gain(0xAAAA, -5.0);
        table.snapshot(0, &mut Vec::new());
        table.set_source_gain(0xAAAA, 1e9);
        let mut snaps = Vec::new();
        table.snapshot(0, &mut snaps);
        assert_eq!(snaps[0].gain_milli, METER_CEILING);

        table.set_source_gain(0xAAAA, f32::NAN);
        table.snapshot(0, &mut snaps);
        assert_eq!(snaps[0].gain_milli, 0);

        // Master gain follows the same rule, and nothing panics on the way.
        table.set_master_gain(f32::NAN);
        table.set_master_gain(-1.0);
        table.set_master_gain(1e9);
        m.render(240);
    }

    #[test]
    fn render_is_clamped_rather_than_allocating_for_an_absurd_block() {
        let (_table, mut w, mut m) = build(240);
        w.acquire(1, 0).unwrap();
        assert_eq!(m.render(MAX_CALLBACK_FRAMES + 1).len(), MAX_CALLBACK_FRAMES);
    }

    #[test]
    fn the_limiter_holds_the_ceiling_from_the_first_sample() {
        let mut lim = Limiter::new(0.98, crate::protocol::SAMPLE_RATE, 150.0);
        let mut hot = vec![3.0f32; 4800];
        lim.process(&mut hot);
        let peak = hot.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        assert!(peak <= 0.981, "peak {peak}");
        assert!(lim.gain() < 0.4);

        let mut quiet = vec![0.05f32; 48000];
        lim.process(&mut quiet);
        assert!(lim.gain() > 0.9, "released back open");

        // A transient arriving into an open limiter is caught on arrival, not
        // one block later.
        let mut spike = vec![5.0f32; 240];
        lim.process(&mut spike);
        let peak = spike.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        assert!(peak <= 0.981, "peak {peak}");
    }

    #[test]
    fn the_bus_stays_bounded_with_every_source_hot_and_loud() {
        // Eight sources at full scale, each at 2x, into a 2x master: 32x over
        // the ceiling. Kept just under the trim threshold (target + 2 packets
        // vs a max_fill of 4x target) so this exercises the limiter rather
        // than the drift trim.
        let (table, mut w, mut m) = build(240);
        table.set_master_gain(2.0);
        for i in 0..MAX_SOURCES {
            let slot = w.acquire(100 + i as u32, 0).unwrap();
            table.set_source_gain(100 + i as u32, 2.0);
            feed(&mut w, slot, i16::MAX, 2, 240);
        }
        m.render(240); // priming silence
        for _ in 0..2 {
            for s in m.render(240) {
                assert!(s.abs() <= 0.981, "limiter let {s} through");
            }
        }
        assert!(table.limiter_gain() < 0.1, "gain {}", table.limiter_gain());
        assert!(table.master_peak() > 0.5);
    }

    #[test]
    fn a_source_running_ahead_is_trimmed_rather_than_allowed_to_lag() {
        // The other half of the story: feeding faster than the reader drains
        // is how clock drift shows up, and it is shed by dropping the oldest
        // audio, capped at four times the target.
        let (table, mut w, mut m) = build(240);
        let slot = w.acquire(1, 0).unwrap();
        feed(&mut w, slot, 1000, 20, 240);

        let mut snaps = Vec::new();
        table.snapshot(0, &mut snaps);
        assert!(snaps[0].buffer_frames > 240 * 4);

        m.render(240);
        table.snapshot(0, &mut snaps);
        assert!(snaps[0].buffer_frames <= 240, "trimmed back to target");
    }
}
