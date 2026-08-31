#pragma once

#include <pthread.h>
#include <sys/resource.h>
#include <time.h>
#include <unistd.h>

#include <cstdint>

namespace lau {

inline int64_t nowMs() {
    timespec ts{};
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return static_cast<int64_t>(ts.tv_sec) * 1000 + ts.tv_nsec / 1000000;
}

// Best effort: push a helper thread up so the network path is not scheduled
// behind UI work. Failures are ignored - this is an optimisation, not a
// requirement.
inline void raiseThreadPriority() {
    sched_param sp{};
    sp.sched_priority = 2;
    if (pthread_setschedparam(pthread_self(), SCHED_FIFO, &sp) != 0) {
        setpriority(PRIO_PROCESS, 0, -16);  // ANDROID_PRIORITY_AUDIO
    }
}

}  // namespace lau
