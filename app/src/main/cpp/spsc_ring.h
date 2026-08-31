// Wait-free single-producer / single-consumer ring buffer.
// One writer thread, one reader thread, no locks, no allocation after init().
#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <vector>

namespace lau {

template <typename T>
class SpscRing {
public:
    // capacity is rounded up to a power of two. This is the only way to rewind
    // the cursors, and it is safe only before either thread is running: there
    // is deliberately no runtime reset, because a producer-side one would move
    // the consumer's cursor out from under it. Use skipTo() instead.
    void init(size_t capacity) {
        size_t cap = 1;
        while (cap < capacity) cap <<= 1;
        buf_.assign(cap, T{});
        mask_ = cap - 1;
        w_.store(0, std::memory_order_relaxed);
        r_.store(0, std::memory_order_relaxed);
    }

    size_t capacity() const { return buf_.size(); }

    // Readable elements. Exact on either owning thread; on any third thread
    // (the UI polling for stats) w_ and r_ are read one after the other and the
    // consumer can overtake the sampled w_ in between, so clamp rather than let
    // the unsigned subtraction wrap into a nonsense depth.
    size_t size() const {
        const uint64_t w = w_.load(std::memory_order_acquire);
        const uint64_t r = r_.load(std::memory_order_acquire);
        return w > r ? static_cast<size_t>(w - r) : 0;
    }

    size_t space() const { return capacity() - size(); }

    // Absolute stream positions. Both counters are 64-bit and monotonic, so a
    // position stays meaningful for the life of the ring.
    uint64_t writeCursor() const { return w_.load(std::memory_order_acquire); }
    uint64_t readCursor() const { return r_.load(std::memory_order_acquire); }

    // ---- producer side ----

    size_t write(const T* src, size_t n) {
        const uint64_t w = w_.load(std::memory_order_relaxed);
        const uint64_t r = r_.load(std::memory_order_acquire);
        size_t free = capacity() - static_cast<size_t>(w - r);
        if (n > free) n = free;
        size_t idx = static_cast<size_t>(w) & mask_;
        size_t first = capacity() - idx;
        if (first > n) first = n;
        std::memcpy(&buf_[idx], src, first * sizeof(T));
        if (n > first) std::memcpy(&buf_[0], src + first, (n - first) * sizeof(T));
        w_.store(w + n, std::memory_order_release);
        return n;
    }

    size_t writeZeros(size_t n) {
        const uint64_t w = w_.load(std::memory_order_relaxed);
        const uint64_t r = r_.load(std::memory_order_acquire);
        size_t free = capacity() - static_cast<size_t>(w - r);
        if (n > free) n = free;
        size_t idx = static_cast<size_t>(w) & mask_;
        size_t first = capacity() - idx;
        if (first > n) first = n;
        std::memset(&buf_[idx], 0, first * sizeof(T));
        if (n > first) std::memset(&buf_[0], 0, (n - first) * sizeof(T));
        w_.store(w + n, std::memory_order_release);
        return n;
    }

    // ---- consumer side ----

    size_t read(T* dst, size_t n) {
        const uint64_t r = r_.load(std::memory_order_relaxed);
        const uint64_t w = w_.load(std::memory_order_acquire);
        size_t avail = static_cast<size_t>(w - r);
        if (n > avail) n = avail;
        size_t idx = static_cast<size_t>(r) & mask_;
        size_t first = capacity() - idx;
        if (first > n) first = n;
        std::memcpy(dst, &buf_[idx], first * sizeof(T));
        if (n > first) std::memcpy(dst + first, &buf_[0], (n - first) * sizeof(T));
        r_.store(r + n, std::memory_order_release);
        return n;
    }

    // Discard the n oldest elements. Consumer side only.
    size_t skip(size_t n) {
        const uint64_t r = r_.load(std::memory_order_relaxed);
        const uint64_t w = w_.load(std::memory_order_acquire);
        size_t avail = static_cast<size_t>(w - r);
        if (n > avail) n = avail;
        r_.store(r + n, std::memory_order_release);
        return n;
    }

    // Discard everything written before absolute position `pos`. Consumer side
    // only. Idempotent and monotonic: replaying an old `pos` is a no-op, so the
    // producer can publish one and never has to hear back.
    size_t skipTo(uint64_t pos) {
        const uint64_t r = r_.load(std::memory_order_relaxed);
        if (pos <= r) return 0;
        const uint64_t w = w_.load(std::memory_order_acquire);
        const uint64_t to = pos < w ? pos : w;
        if (to <= r) return 0;
        r_.store(to, std::memory_order_release);
        return static_cast<size_t>(to - r);
    }

private:
    std::vector<T>        buf_;
    size_t                mask_ = 0;
    std::atomic<uint64_t> w_{0};
    std::atomic<uint64_t> r_{0};
};

}  // namespace lau
