// Peak meter ballistics, shared by the transmitter, the mix bus and every
// source strip. Header-only, no platform dependencies.
#pragma once

#include <atomic>
#include <cstdint>

namespace lau {

// Levels are published as milli-units of full scale (1000 = 0 dBFS) in a
// relaxed atomic: the audio thread stores, the UI polls, neither blocks.
inline constexpr uint32_t kMeterFullScale = 1000;

// Per-source and master gains reach 2x, so the meter has to be able to report
// above full scale rather than pinning silently.
inline constexpr uint32_t kMeterCeiling = 2000;

// Instant attack, ~1/16-per-block decay, which is slow enough to read on a
// 80 ms UI poll and fast enough to follow speech.
//
// `level` is linear with 1.0 == full scale.
inline void updatePeakMeter(std::atomic<uint32_t>& meter, float level) {
    const uint32_t cur = meter.load(std::memory_order_relaxed);

    uint32_t next = level > 0.0f ? static_cast<uint32_t>(level * kMeterFullScale) : 0u;
    if (next > kMeterCeiling) next = kMeterCeiling;

    if (next < cur) {
        // Decay by 1/16 but never by less than one step. A bare `cur >> 4`
        // reaches zero only from above 16 and otherwise stalls, parking the
        // meter at -36 dBFS through dead silence.
        const uint32_t shift = cur >> 4;
        const uint32_t step  = shift > 0 ? shift : 1u;
        next = cur > step ? cur - step : 0u;
    }
    meter.store(next, std::memory_order_relaxed);
}

}  // namespace lau
