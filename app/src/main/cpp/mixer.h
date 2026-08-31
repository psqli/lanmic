// Fixed-slot source table + summing mixer + a lookahead-free limiter.
// Slots are created/retired by the network thread only; the audio thread
// reads them through an atomic state word and never allocates.
#pragma once

#include <algorithm>
#include <atomic>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <vector>

#include "jitter_buffer.h"
#include "meter.h"

namespace lau {

inline constexpr int kMaxSources = 8;

struct SourceSnapshot {
    uint32_t ssrc;
    uint32_t peakMilli;    // 0..kMeterCeiling, 1000 = full scale
    uint32_t bufferFrames;
    uint32_t packets;
    uint32_t lost;
    uint32_t underruns;
    uint32_t ageMs;        // since last packet
    uint32_t muted;
    uint32_t gainMilli;    // 0..2000
};

class SourceTable {
public:
    void configure(int targetFrames) {
        for (auto& s : slots_) {
            s.state.store(kFree, std::memory_order_relaxed);
            s.jb.configure(targetFrames);
            s.peakMilli.store(0, std::memory_order_relaxed);
            s.gainMilli.store(1000, std::memory_order_relaxed);
            s.muted.store(0, std::memory_order_relaxed);
        }
    }

    // ---- network thread ----
    // Returns nullptr when the table is full.
    JitterBuffer* acquire(uint32_t ssrc, int64_t nowMs) {
        for (auto& s : slots_) {
            if (s.state.load(std::memory_order_acquire) == kActive &&
                s.ssrc.load(std::memory_order_relaxed) == ssrc) {
                s.lastSeenMs.store(nowMs, std::memory_order_relaxed);
                return &s.jb;
            }
        }
        for (auto& s : slots_) {
            if (s.state.load(std::memory_order_acquire) == kFree) {
                s.jb.resetState();
                s.ssrc.store(ssrc, std::memory_order_relaxed);
                s.peakMilli.store(0, std::memory_order_relaxed);
                s.muted.store(0, std::memory_order_relaxed);
                s.gainMilli.store(1000, std::memory_order_relaxed);
                s.lastSeenMs.store(nowMs, std::memory_order_relaxed);
                s.state.store(kActive, std::memory_order_release);
                return &s.jb;
            }
        }
        return nullptr;
    }

    void retire(uint32_t ssrc) {
        for (auto& s : slots_) {
            if (s.state.load(std::memory_order_acquire) == kActive &&
                s.ssrc.load(std::memory_order_relaxed) == ssrc) {
                s.state.store(kFree, std::memory_order_release);
            }
        }
    }

    void reapStale(int64_t nowMs, int64_t timeoutMs) {
        for (auto& s : slots_) {
            if (s.state.load(std::memory_order_acquire) != kActive) continue;
            if (nowMs - s.lastSeenMs.load(std::memory_order_relaxed) > timeoutMs) {
                s.state.store(kFree, std::memory_order_release);
            }
        }
    }

    // ---- audio thread ----
    // Sums every active source into `out` (mono float, -1..1). scratch must
    // hold at least `frames` int16 samples. Returns the number of sources mixed.
    int mix(float* out, size_t frames, int16_t* scratch) {
        std::memset(out, 0, frames * sizeof(float));
        int active = 0;
        for (auto& s : slots_) {
            if (s.state.load(std::memory_order_acquire) != kActive) continue;
            ++active;
            s.jb.read(scratch, frames);
            const float g = s.muted.load(std::memory_order_relaxed)
                                ? 0.0f
                                : s.gainMilli.load(std::memory_order_relaxed) * 0.001f;
            float peak = 0.0f;
            for (size_t i = 0; i < frames; ++i) {
                const float v = scratch[i] * (1.0f / 32768.0f) * g;
                out[i] += v;
                const float a = v < 0 ? -v : v;
                if (a > peak) peak = a;
            }
            updatePeakMeter(s.peakMilli, peak);
        }
        return active;
    }

    // ---- any thread (UI polling) ----
    int snapshot(SourceSnapshot* dst, int maxOut, int64_t nowMs) const {
        int n = 0;
        for (const auto& s : slots_) {
            if (n >= maxOut) break;
            if (s.state.load(std::memory_order_acquire) != kActive) continue;
            const JitterStats js = s.jb.stats();
            dst[n].ssrc         = s.ssrc.load(std::memory_order_relaxed);
            dst[n].peakMilli    = s.peakMilli.load(std::memory_order_relaxed);
            dst[n].bufferFrames = js.fillFrames;
            dst[n].packets      = js.packets;
            dst[n].lost         = js.lost;
            dst[n].underruns    = js.underruns;
            dst[n].ageMs = static_cast<uint32_t>(
                nowMs - s.lastSeenMs.load(std::memory_order_relaxed));
            dst[n].muted     = s.muted.load(std::memory_order_relaxed);
            dst[n].gainMilli = s.gainMilli.load(std::memory_order_relaxed);
            ++n;
        }
        return n;
    }

    // Clamped here rather than trusted: this arrives from the UI through JNI,
    // and a negative value would wrap into a gain of several million.
    void setGain(uint32_t ssrc, int gainMilli) {
        const uint32_t g =
            static_cast<uint32_t>(std::clamp(gainMilli, 0, static_cast<int>(kMeterCeiling)));
        for (auto& s : slots_) {
            if (s.state.load(std::memory_order_acquire) == kActive &&
                s.ssrc.load(std::memory_order_relaxed) == ssrc)
                s.gainMilli.store(g, std::memory_order_relaxed);
        }
    }

    void setMuted(uint32_t ssrc, bool muted) {
        for (auto& s : slots_) {
            if (s.state.load(std::memory_order_acquire) == kActive &&
                s.ssrc.load(std::memory_order_relaxed) == ssrc)
                s.muted.store(muted ? 1u : 0u, std::memory_order_relaxed);
        }
    }

private:
    static constexpr uint32_t kFree = 0, kActive = 1;

    struct Slot {
        std::atomic<uint32_t> state{kFree};
        // Written by the network thread on acquire, read by the UI thread in
        // snapshot/setGain/setMuted. Ordered against `state`, but atomic in its
        // own right so those reads are not a plain race on a torn-down slot.
        std::atomic<uint32_t> ssrc{0};
        JitterBuffer          jb;
        std::atomic<int64_t>  lastSeenMs{0};
        std::atomic<uint32_t> peakMilli{0};
        std::atomic<uint32_t> gainMilli{1000};
        std::atomic<uint32_t> muted{0};
    };

    Slot slots_[kMaxSources];
};

// Zero-latency peak limiter: instant attack, exponential release. Keeps the
// summed bus below `ceiling` without adding a lookahead delay.
class Limiter {
public:
    void configure(float ceiling, int sampleRate, float releaseMs) {
        ceiling_ = ceiling;
        release_ = std::exp(-1.0f / (sampleRate * releaseMs * 0.001f));
        gain_    = 1.0f;
    }

    void process(float* x, size_t n) {
        for (size_t i = 0; i < n; ++i) {
            const float a      = x[i] < 0 ? -x[i] : x[i];
            const float wanted = (a > ceiling_) ? (ceiling_ / a) : 1.0f;
            if (wanted < gain_) {
                gain_ = wanted;
            } else {
                gain_ = wanted + (gain_ - wanted) * release_;
            }
            x[i] *= gain_;
        }
    }

    float gain() const { return gain_; }

private:
    float ceiling_ = 0.98f;
    float release_ = 0.9999f;
    float gain_    = 1.0f;
};

}  // namespace lau
