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
    // capacity is rounded up to a power of two.
    void init(size_t capacity) {
        size_t cap = 1;
        while (cap < capacity) cap <<= 1;
        buf_.assign(cap, T{});
        mask_ = cap - 1;
        w_.store(0, std::memory_order_relaxed);
        r_.store(0, std::memory_order_relaxed);
    }

    size_t capacity() const { return buf_.size(); }

    // Readable elements.
    size_t size() const {
        return static_cast<size_t>(w_.load(std::memory_order_acquire) -
                                   r_.load(std::memory_order_acquire));
    }

    size_t space() const { return capacity() - size(); }

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

    // Only safe while neither thread is running.
    void clear() {
        w_.store(0, std::memory_order_relaxed);
        r_.store(0, std::memory_order_relaxed);
    }

private:
    std::vector<T>        buf_;
    size_t                mask_ = 0;
    std::atomic<uint64_t> w_{0};
    std::atomic<uint64_t> r_{0};
};

}  // namespace lau
