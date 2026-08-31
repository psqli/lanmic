// Per-source jitter buffer. Network thread writes, audio callback reads.
// Alignment is by packet timestamp, never by arrival time.
#pragma once

#include <algorithm>
#include <atomic>
#include <cstdint>
#include <cstring>

#include "spsc_ring.h"

namespace lau {

struct JitterStats {
    uint32_t packets    = 0;
    uint32_t lost       = 0;   // gaps in seq
    uint32_t late       = 0;   // arrived after their slot had been written
    uint32_t concealed  = 0;   // frames of silence inserted for gaps
    uint32_t underruns  = 0;   // callbacks that found the buffer empty
    uint32_t trims      = 0;   // times we shed accumulated latency
    uint32_t resyncs    = 0;
    uint32_t fillFrames = 0;   // current depth
};

class JitterBuffer {
public:
    // targetFrames: nominal depth. Ring is sized at 8x target (>= 4096).
    void configure(int targetFrames) {
        target_ = static_cast<size_t>(targetFrames);
        maxFill_ = target_ * 4;
        maxGap_  = target_ * 4;
        size_t cap = std::max<size_t>(target_ * 8, 4096);
        ring_.init(cap);
        resetState();
    }

    // Safe to call from the network thread while the audio callback is mixing:
    // the ring is not rewound (that would race the reader's cursor), the stale
    // audio is marked for the reader to drop on its next pass instead.
    void resetState() {
        dropBefore_.store(ring_.writeCursor(), std::memory_order_release);
        primed_      = false;
        writeTs_     = 0;
        expectedSeq_ = 0;
        haveSeq_     = false;
        resync_.store(false, std::memory_order_relaxed);
        packets_.store(0, std::memory_order_relaxed);
        lost_.store(0, std::memory_order_relaxed);
        late_.store(0, std::memory_order_relaxed);
        concealed_.store(0, std::memory_order_relaxed);
        underruns_.store(0, std::memory_order_relaxed);
        trims_.store(0, std::memory_order_relaxed);
        resyncs_.store(0, std::memory_order_relaxed);
    }

    // ---- network thread ----
    void onPacket(uint32_t seq, uint32_t timestamp, const int16_t* pcm, size_t frames,
                  bool muted) {
        packets_.fetch_add(1, std::memory_order_relaxed);

        if (resync_.exchange(false, std::memory_order_acq_rel)) primed_ = false;

        if (haveSeq_) {
            const int32_t d = static_cast<int32_t>(seq - expectedSeq_);
            if (d > 0) lost_.fetch_add(static_cast<uint32_t>(d), std::memory_order_relaxed);
        }
        expectedSeq_ = seq + 1;
        haveSeq_     = true;

        if (!primed_) {
            prime(timestamp);
        } else {
            const int32_t delta = static_cast<int32_t>(timestamp - writeTs_);
            if (delta < 0) {  // duplicate or reordered behind the write cursor
                late_.fetch_add(1, std::memory_order_relaxed);
                return;
            }
            if (static_cast<size_t>(delta) > maxGap_) {
                prime(timestamp);  // sender restarted or we were away too long
            } else if (delta > 0) {
                ring_.writeZeros(static_cast<size_t>(delta));
                concealed_.fetch_add(static_cast<uint32_t>(delta), std::memory_order_relaxed);
            }
        }

        if (frames == 0) return;
        if (ring_.space() < frames) return;  // reader stalled; drop
        if (muted || pcm == nullptr) {
            ring_.writeZeros(frames);
        } else {
            ring_.write(pcm, frames);
        }
        writeTs_ = timestamp + static_cast<uint32_t>(frames);
    }

    // ---- audio thread ----
    // Always fills exactly `frames` samples (zero-padded on underrun).
    void read(int16_t* dst, size_t frames) {
        const uint64_t drop = dropBefore_.load(std::memory_order_acquire);
        if (drop != 0) ring_.skipTo(drop);

        size_t avail = ring_.size();
        if (avail > maxFill_) {
            ring_.skip(avail - target_);
            trims_.fetch_add(1, std::memory_order_relaxed);
        }
        const size_t got = ring_.read(dst, frames);
        if (got < frames) {
            std::memset(dst + got, 0, (frames - got) * sizeof(int16_t));
            underruns_.fetch_add(1, std::memory_order_relaxed);
            resync_.store(true, std::memory_order_release);
        }
    }

    // Playable depth: what the reader will hand to the mixer, which excludes
    // audio already marked stale by resetState() and not yet dropped.
    size_t fill() const {
        const uint64_t drop = dropBefore_.load(std::memory_order_acquire);
        const uint64_t r    = ring_.readCursor();
        const uint64_t base = r > drop ? r : drop;
        const uint64_t w    = ring_.writeCursor();
        return w > base ? static_cast<size_t>(w - base) : 0;
    }
    size_t target() const { return target_; }

    JitterStats stats() const {
        JitterStats s;
        s.packets    = packets_.load(std::memory_order_relaxed);
        s.lost       = lost_.load(std::memory_order_relaxed);
        s.late       = late_.load(std::memory_order_relaxed);
        s.concealed  = concealed_.load(std::memory_order_relaxed);
        s.underruns  = underruns_.load(std::memory_order_relaxed);
        s.trims      = trims_.load(std::memory_order_relaxed);
        s.resyncs    = resyncs_.load(std::memory_order_relaxed);
        s.fillFrames = static_cast<uint32_t>(fill());
        return s;
    }

private:
    // Bring the buffer up to `target_` frames of depth. Only what the reader
    // will actually play counts: audio already marked for dropping is not
    // depth. Topping up rather than always appending `target_` keeps a
    // re-prime from stacking another target on a buffer that is already deep.
    void prime(uint32_t timestamp) {
        const uint64_t drop = dropBefore_.load(std::memory_order_relaxed);
        const uint64_t r    = ring_.readCursor();
        const uint64_t base = r > drop ? r : drop;
        const uint64_t w    = ring_.writeCursor();
        const size_t   have = w > base ? static_cast<size_t>(w - base) : 0;
        if (have < target_) ring_.writeZeros(target_ - have);
        writeTs_ = timestamp;
        primed_  = true;
        resyncs_.fetch_add(1, std::memory_order_relaxed);
    }

    SpscRing<int16_t> ring_;
    size_t            target_  = 720;
    size_t            maxFill_ = 2880;
    size_t            maxGap_  = 2880;

    // network thread only
    bool     primed_      = false;
    bool     haveSeq_     = false;
    uint32_t writeTs_     = 0;
    uint32_t expectedSeq_ = 0;

    // Published by the network thread, consumed by the audio thread: everything
    // written before this absolute position is stale and must not be played.
    std::atomic<uint64_t> dropBefore_{0};
    std::atomic<bool>     resync_{false};
    std::atomic<uint32_t> packets_{0}, lost_{0}, late_{0}, concealed_{0};
    std::atomic<uint32_t> underruns_{0}, trims_{0}, resyncs_{0};
};

}  // namespace lau
