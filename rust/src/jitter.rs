//! Per-source jitter buffer. Alignment is by packet timestamp, never by
//! arrival time.
//!
//! The network thread and the audio callback each own one half of an
//! [`rtrb`] ring, so which thread may call what is a property of the types
//! rather than a comment: a [`JitterWriter`] cannot read and a [`JitterReader`]
//! cannot write, and neither can be used from two threads at once. What both
//! need to see - the counters, and the "drop everything before here" mark a
//! rejoining source leaves behind - lives in [`JitterShared`] as atomics.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use rtrb::{Consumer, Producer, RingBuffer};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JitterStats {
    pub packets: u32,
    /// Gaps in `seq`.
    pub lost: u32,
    /// Arrived after their slot had been written.
    pub late: u32,
    /// Frames of silence inserted for gaps.
    pub concealed: u32,
    /// Callbacks that found the buffer empty.
    pub underruns: u32,
    /// Times accumulated latency was shed.
    pub trims: u32,
    pub resyncs: u32,
    /// Current playable depth, in frames.
    pub fill_frames: u32,
}

/// Config and counters both halves can see.
#[derive(Debug)]
pub struct JitterShared {
    /// Nominal depth in frames.
    target: usize,
    /// Depth above which the reader sheds latency.
    max_fill: usize,
    /// Timestamp jump beyond which a sender counts as restarted.
    max_gap: u32,
    /// Ring capacity in frames.
    capacity: usize,

    /// Absolute frame positions. Monotonic and 64-bit, so they never wrap and
    /// a position stays meaningful for the life of the buffer. `written` is
    /// published by the writer, `read` by the reader, `drop_before` by the
    /// writer for the reader to act on.
    written: AtomicU64,
    read: AtomicU64,
    drop_before: AtomicU64,

    resync: AtomicBool,
    packets: AtomicU32,
    lost: AtomicU32,
    late: AtomicU32,
    concealed: AtomicU32,
    underruns: AtomicU32,
    trims: AtomicU32,
    resyncs: AtomicU32,
}

impl JitterShared {
    pub fn new(target_frames: usize) -> Self {
        let target = target_frames.max(1);
        Self {
            target,
            max_fill: target * 4,
            max_gap: (target * 4) as u32,
            capacity: (target * 8).max(4096),
            written: AtomicU64::new(0),
            read: AtomicU64::new(0),
            drop_before: AtomicU64::new(0),
            resync: AtomicBool::new(false),
            packets: AtomicU32::new(0),
            lost: AtomicU32::new(0),
            late: AtomicU32::new(0),
            concealed: AtomicU32::new(0),
            underruns: AtomicU32::new(0),
            trims: AtomicU32::new(0),
            resyncs: AtomicU32::new(0),
        }
    }

    pub fn target(&self) -> usize {
        self.target
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Playable depth in frames: what the reader will hand to the mixer, which
    /// excludes audio already marked stale and not yet dropped.
    ///
    /// Safe to call from any thread. The two cursors are sampled one after the
    /// other and may disagree by a block, but `saturating_sub` means the worst
    /// case is an answer that is briefly low - never one that has wrapped.
    pub fn fill(&self) -> usize {
        let written = self.written.load(Ordering::Acquire);
        let base = self
            .read
            .load(Ordering::Acquire)
            .max(self.drop_before.load(Ordering::Acquire));
        written.saturating_sub(base) as usize
    }

    pub fn stats(&self) -> JitterStats {
        JitterStats {
            packets: self.packets.load(Ordering::Relaxed),
            lost: self.lost.load(Ordering::Relaxed),
            late: self.late.load(Ordering::Relaxed),
            concealed: self.concealed.load(Ordering::Relaxed),
            underruns: self.underruns.load(Ordering::Relaxed),
            trims: self.trims.load(Ordering::Relaxed),
            resyncs: self.resyncs.load(Ordering::Relaxed),
            fill_frames: self.fill() as u32,
        }
    }

    fn clear_counters(&self) {
        for c in [
            &self.packets,
            &self.lost,
            &self.late,
            &self.concealed,
            &self.underruns,
            &self.trims,
            &self.resyncs,
        ] {
            c.store(0, Ordering::Relaxed);
        }
    }
}

/// Builds the two halves of one buffer, sized from `shared`.
pub fn pair(shared: &JitterShared) -> (JitterWriter, JitterReader) {
    let (producer, consumer) = RingBuffer::new(shared.capacity);
    (
        JitterWriter {
            ring: producer,
            written: 0,
            primed: false,
            have_seq: false,
            write_ts: 0,
            expected_seq: 0,
        },
        JitterReader {
            ring: consumer,
            read: 0,
        },
    )
}

/// Network-thread half.
pub struct JitterWriter {
    ring: Producer<i16>,
    /// Mirror of `shared.written`, owned by this thread.
    written: u64,
    primed: bool,
    have_seq: bool,
    write_ts: u32,
    expected_seq: u32,
}

impl JitterWriter {
    /// Retires whatever the previous occupant of this buffer left behind and
    /// starts a fresh session.
    ///
    /// Safe to call while the audio callback is reading. The ring is not
    /// rewound - that would move the reader's cursor out from under it - the
    /// stale audio is marked instead, and the reader drops it on its next pass.
    pub fn reset(&mut self, shared: &JitterShared) {
        shared.drop_before.store(self.written, Ordering::Release);
        shared.resync.store(false, Ordering::Relaxed);
        shared.clear_counters();
        self.primed = false;
        self.have_seq = false;
        self.write_ts = 0;
        self.expected_seq = 0;
    }

    pub fn on_packet(
        &mut self,
        shared: &JitterShared,
        seq: u32,
        timestamp: u32,
        pcm: Option<&[i16]>,
        frames: usize,
        muted: bool,
    ) {
        shared.packets.fetch_add(1, Ordering::Relaxed);

        if shared.resync.swap(false, Ordering::AcqRel) {
            self.primed = false;
        }

        if self.have_seq {
            let gap = seq.wrapping_sub(self.expected_seq) as i32;
            if gap > 0 {
                shared.lost.fetch_add(gap as u32, Ordering::Relaxed);
            }
        }
        self.expected_seq = seq.wrapping_add(1);
        self.have_seq = true;

        if !self.primed {
            self.prime(shared, timestamp);
        } else {
            let delta = timestamp.wrapping_sub(self.write_ts) as i32;
            if delta < 0 {
                // Duplicate, or reordered behind the write cursor.
                shared.late.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let delta = delta as u32;
            if delta > shared.max_gap {
                // Sender restarted, or we were away too long.
                self.prime(shared, timestamp);
            } else if delta > 0 {
                self.push_silence(shared, delta as usize);
                shared.concealed.fetch_add(delta, Ordering::Relaxed);
            }
        }

        if frames == 0 {
            return;
        }
        // A packet that will not fit is dropped whole and the write cursor
        // stays put, so the next one arrives as a timestamp gap and is
        // concealed rather than silently shortening the stream. This only
        // happens when the reader has stalled, at which point the audio
        // already in the ring is worthless anyway.
        if self.ring.slots() < frames {
            return;
        }
        // Exactly `frames` go in, whatever happens, or `write_ts` below would
        // promise audio that is not there and every later packet would land in
        // the wrong place.
        let written = match pcm {
            Some(pcm) if !muted => self.push(shared, &pcm[..frames.min(pcm.len())]),
            _ => 0,
        };
        if written < frames {
            self.push_silence(shared, frames - written);
        }
        self.write_ts = timestamp.wrapping_add(frames as u32);
    }

    /// Brings the buffer up to `target` frames of depth. Only what the reader
    /// will actually play counts, so audio already marked stale is not depth.
    /// Topping up rather than always appending a full target keeps a re-prime
    /// from stacking more latency onto a buffer that is already deep.
    fn prime(&mut self, shared: &JitterShared, timestamp: u32) {
        let base = shared
            .read
            .load(Ordering::Acquire)
            .max(shared.drop_before.load(Ordering::Relaxed));
        let have = self.written.saturating_sub(base) as usize;
        if have < shared.target {
            self.push_silence(shared, shared.target - have);
        }
        self.write_ts = timestamp;
        self.primed = true;
        shared.resyncs.fetch_add(1, Ordering::Relaxed);
    }

    fn push(&mut self, shared: &JitterShared, data: &[i16]) -> usize {
        let (pushed, _remainder) = self.ring.push_partial_slice(data);
        let n = pushed.len();
        self.advance(shared, n);
        n
    }

    fn push_silence(&mut self, shared: &JitterShared, frames: usize) -> usize {
        let n = frames.min(self.ring.slots());
        if n == 0 {
            return 0;
        }
        // `write_chunk` hands back slots already filled with T::default().
        let written = match self.ring.write_chunk(n) {
            Ok(chunk) => {
                chunk.commit_all();
                n
            }
            Err(_) => 0,
        };
        self.advance(shared, written);
        written
    }

    fn advance(&mut self, shared: &JitterShared, frames: usize) {
        self.written += frames as u64;
        shared.written.store(self.written, Ordering::Release);
    }
}

/// Audio-thread half.
pub struct JitterReader {
    ring: Consumer<i16>,
    /// Mirror of `shared.read`, owned by this thread.
    read: u64,
}

impl JitterReader {
    /// Fills `dst` completely, zero-padding on underrun.
    pub fn read(&mut self, shared: &JitterShared, dst: &mut [i16]) {
        // Anything a rejoining source marked stale goes first.
        let drop_before = shared.drop_before.load(Ordering::Acquire);
        if drop_before > self.read {
            let stale = (drop_before - self.read) as usize;
            self.skip(shared, stale);
        }

        // Shed accumulated latency - always by dropping the oldest audio.
        // This is where clock drift between the two ends goes.
        let avail = self.ring.slots();
        if avail > shared.max_fill {
            self.skip(shared, avail - shared.target);
            shared.trims.fetch_add(1, Ordering::Relaxed);
        }

        let (popped, remainder) = self.ring.pop_partial_slice(dst);
        let got = popped.len();
        self.advance(shared, got);
        if !remainder.is_empty() {
            remainder.fill(0);
            shared.underruns.fetch_add(1, Ordering::Relaxed);
            // Ask the writer to re-prime, so the buffer returns to its target
            // depth instead of hovering at zero and clicking every callback.
            shared.resync.store(true, Ordering::Release);
        }
    }

    fn skip(&mut self, shared: &JitterShared, frames: usize) -> usize {
        let n = frames.min(self.ring.slots());
        if n == 0 {
            return 0;
        }
        let skipped = match self.ring.read_chunk(n) {
            Ok(chunk) => {
                chunk.commit_all();
                n
            }
            Err(_) => 0,
        };
        self.advance(shared, skipped);
        skipped
    }

    fn advance(&mut self, shared: &JitterShared, frames: usize) {
        self.read += frames as u64;
        shared.read.store(self.read, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: usize = 240; // 5 ms at 48 kHz

    struct Buf {
        shared: JitterShared,
        w: JitterWriter,
        r: JitterReader,
    }

    impl Buf {
        fn new(target: usize) -> Self {
            let shared = JitterShared::new(target);
            let (w, r) = pair(&shared);
            Buf { shared, w, r }
        }

        fn feed(&mut self, seq: u32, ts: u32, value: i16, frames: usize) {
            let pcm = vec![value; frames];
            self.w
                .on_packet(&self.shared, seq, ts, Some(&pcm), frames, false);
        }

        fn read(&mut self, frames: usize) -> Vec<i16> {
            let mut out = vec![0i16; frames];
            self.r.read(&self.shared, &mut out);
            out
        }

        fn fill(&self) -> usize {
            self.shared.fill()
        }
    }

    #[test]
    fn first_packet_primes_with_target_of_silence() {
        let mut b = Buf::new(TARGET);
        b.feed(0, 0, 1000, 120);
        assert_eq!(b.fill(), TARGET + 120);

        assert!(b.read(120).iter().all(|&s| s == 0));
        assert!(b.read(120).iter().all(|&s| s == 0));
        assert_eq!(b.fill(), 120);
        assert!(b.read(120).iter().all(|&s| s == 1000));
    }

    #[test]
    fn an_in_order_stream_keeps_its_depth() {
        let mut b = Buf::new(TARGET);
        for i in 0..20u32 {
            b.feed(i, i * 120, (i + 1) as i16, 120);
        }
        assert_eq!(b.fill(), TARGET + 20 * 120);
    }

    #[test]
    fn a_missing_packet_becomes_exactly_one_packet_of_silence() {
        let mut b = Buf::new(TARGET);
        b.feed(0, 0, 5, 120);
        b.feed(2, 240, 7, 120); // packet 1 (ts=120) never arrived

        b.read(120);
        b.read(120); // priming silence
        assert_eq!(b.read(120)[0], 5);
        assert_eq!(b.read(120)[0], 0, "the gap is concealed");
        assert_eq!(b.read(120)[0], 7);

        let s = b.shared.stats();
        assert_eq!(s.lost, 1);
        assert_eq!(s.concealed, 120);
    }

    #[test]
    fn a_late_duplicate_is_dropped_not_played() {
        let mut b = Buf::new(TARGET);
        b.feed(0, 0, 5, 120);
        b.feed(1, 120, 6, 120);
        let before = b.fill();
        b.feed(2, 0, 9, 120); // arrives after its slot was written
        assert_eq!(b.fill(), before);
        assert_eq!(b.shared.stats().late, 1);
    }

    #[test]
    fn a_runaway_buffer_is_trimmed_back_to_target() {
        let mut b = Buf::new(TARGET);
        for i in 0..40u32 {
            b.feed(i, i * 120, (100 + i) as i16, 120);
        }
        assert!(b.fill() > TARGET * 4);
        b.read(120);
        assert!(b.fill() <= TARGET);
        assert_eq!(b.shared.stats().trims, 1);
    }

    #[test]
    fn underrun_conceals_then_re_primes_on_the_next_packet() {
        let mut b = Buf::new(TARGET);
        b.feed(0, 0, 3, 120);
        let mut last = Vec::new();
        for _ in 0..10 {
            last = b.read(120);
        }
        assert!(b.shared.stats().underruns > 0);
        assert!(last.iter().all(|&s| s == 0));

        b.feed(1, 120, 4, 120);
        assert_eq!(b.fill(), TARGET + 120, "re-primed");
    }

    #[test]
    fn a_re_prime_tops_up_rather_than_stacking() {
        // A restart-sized timestamp jump primes back up to target. Appending a
        // whole target onto a buffer that is already deep would leave that
        // latency sitting there until the trim ceiling was crossed.
        let mut b = Buf::new(TARGET);
        for i in 0..8u32 {
            b.feed(i, i * 120, 3, 120);
        }
        assert_eq!(b.fill(), TARGET + 8 * 120);
        b.feed(8, 5_000_000, 4, 120); // far beyond max_gap: sender restarted
        assert_eq!(b.fill(), TARGET + 9 * 120);
    }

    #[test]
    fn reset_drops_the_previous_session_but_not_the_next_one() {
        let mut b = Buf::new(TARGET);
        for i in 0..6u32 {
            b.feed(i, i * 240, 9000, 240);
        }
        b.read(240); // the reader is live on this buffer

        b.w.reset(&b.shared);
        assert_eq!(b.fill(), 0, "none of the old session is ours");

        b.feed(0, 0, 1234, 240);
        assert_eq!(b.fill(), TARGET + 240);
        assert!(b.read(240).iter().all(|&s| s == 0), "priming silence");
        assert!(b.read(240).iter().all(|&s| s == 1234), "then the new audio");
    }

    #[test]
    fn a_full_ring_drops_the_packet_and_leaves_the_cursor_alone() {
        let mut b = Buf::new(TARGET);
        b.feed(0, 0, 1, 120);
        // Fill the ring without ever reading.
        let mut seq = 1u32;
        let mut ts = 120u32;
        while b.shared.capacity() - b.fill() >= 120 {
            b.feed(seq, ts, 2, 120);
            seq += 1;
            ts += 120;
        }
        let (before_fill, before_ts) = (b.fill(), b.w.write_ts);
        b.feed(seq, ts, 3, 120);
        assert_eq!(b.fill(), before_fill, "the payload is dropped whole");
        assert_eq!(b.w.write_ts, before_ts, "so the next packet reads as a gap");
    }

    /// The writer resets while the audio callback is reading. Crossed cursors
    /// are not a transient glitch: they make the buffer report a depth larger
    /// than it can physically hold, permanently. This is the test that fails
    /// against a producer-side rewind.
    #[test]
    fn reset_under_a_live_reader_never_reports_an_impossible_depth() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;

        let shared = Arc::new(JitterShared::new(240));
        let (mut w, mut r) = pair(&shared);
        let capacity = shared.capacity();

        let stop = Arc::new(AtomicBool::new(false));
        let bogus = Arc::new(AtomicUsize::new(0));

        let reader = std::thread::spawn({
            let (shared, stop, bogus) = (shared.clone(), stop.clone(), bogus.clone());
            move || {
                let mut out = [0i16; 120];
                while !stop.load(Ordering::Relaxed) {
                    r.read(&shared, &mut out);
                    if shared.fill() > capacity || shared.stats().fill_frames as usize > capacity {
                        bogus.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });

        let pcm = [777i16; 120];
        let mut ts = 0u32;
        for i in 0..5000u32 {
            w.on_packet(&shared, i, ts, Some(&pcm), 120, false);
            ts = ts.wrapping_add(120);
            if i % 50 == 0 {
                // The same microphone leaving and rejoining.
                w.reset(&shared);
                ts = 0;
            }
        }
        stop.store(true, Ordering::Relaxed);
        reader.join().unwrap();
        assert_eq!(bogus.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn muted_packets_write_silence_and_still_hold_the_timeline() {
        let mut b = Buf::new(TARGET);
        let pcm = [9999i16; 120];
        b.w.on_packet(&b.shared, 0, 0, Some(&pcm), 120, true);
        b.w.on_packet(&b.shared, 1, 120, Some(&pcm), 120, true);
        assert_eq!(b.fill(), TARGET + 240);
        for _ in 0..4 {
            assert!(b.read(120).iter().all(|&s| s == 0));
        }
        assert_eq!(b.shared.stats().concealed, 0, "muted is not a gap");
    }

    #[test]
    fn a_short_payload_is_padded_rather_than_shifting_the_timeline() {
        // The router never does this, but the timeline invariant should not
        // depend on that: `frames` is what the header promised, and exactly
        // that much has to go in.
        let mut b = Buf::new(TARGET);
        let short = [5i16; 60];
        b.w.on_packet(&b.shared, 0, 0, Some(&short), 120, false);
        b.w.on_packet(&b.shared, 1, 120, Some(&short), 120, false);
        assert_eq!(b.fill(), TARGET + 240);
        assert_eq!(b.shared.stats().concealed, 0, "not a gap, a short packet");
    }

    #[test]
    fn sequence_and_timestamp_wraparound_are_not_treated_as_loss() {
        let mut b = Buf::new(TARGET);
        let base_ts = u32::MAX - 119;
        b.feed(u32::MAX - 1, base_ts, 1, 120);
        b.feed(u32::MAX, base_ts.wrapping_add(120), 2, 120);
        b.feed(0, base_ts.wrapping_add(240), 3, 120);
        let s = b.shared.stats();
        assert_eq!(s.lost, 0);
        assert_eq!(s.late, 0);
        assert_eq!(b.fill(), TARGET + 360);
    }
}
