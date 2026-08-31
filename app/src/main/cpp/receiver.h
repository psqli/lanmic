#pragma once

#include <oboe/Oboe.h>

#include <atomic>
#include <memory>
#include <mutex>
#include <thread>
#include <vector>

#include "mixer.h"
#include "protocol.h"
#include "udp_socket.h"
#include "util.h"

namespace lau {

struct RxStats {
    uint64_t packets       = 0;
    uint64_t badPackets    = 0;
    uint32_t activeSources = 0;
    uint32_t xruns         = 0;
    float    masterPeak    = 0.0f;
    float    limiterGain   = 1.0f;
    float    latencyMs     = 0.0f;
};

// Binds the audio port, keeps one jitter buffer per source, and sums them in
// the output callback. The network thread never blocks the audio thread and
// vice versa: they meet only in the lock-free jitter buffers.
class Receiver : public oboe::AudioStreamDataCallback,
                 public oboe::AudioStreamErrorCallback {
public:
    ~Receiver() override;

    bool start(int port, int jitterMs);
    void stop();
    bool isRunning() const { return running_.load(std::memory_order_acquire); }

    // Gains arrive from the UI through JNI; clamp rather than trust them.
    void setMasterGain(float g) {
        masterGain_.store(g > 0.0f ? (g < 8.0f ? g : 8.0f) : 0.0f, std::memory_order_relaxed);
    }
    void setSourceGain(uint32_t ssrc, float g) {
        sources_.setGain(ssrc, static_cast<int>(g * 1000.0f));
    }
    void setSourceMuted(uint32_t ssrc, bool m) { sources_.setMuted(ssrc, m); }

    int     snapshot(SourceSnapshot* dst, int maxOut) const {
        return sources_.snapshot(dst, maxOut, nowMs());
    }
    RxStats stats() const;

    oboe::DataCallbackResult onAudioReady(oboe::AudioStream* stream, void* audioData,
                                          int32_t numFrames) override;
    void onErrorAfterClose(oboe::AudioStream* stream, oboe::Result error) override;

private:
    // `epoch` is the session the caller opened for. openStream refuses to
    // install a stream whose session has already been torn down.
    bool openStream(uint32_t epoch);
    void closeStream();
    void networkLoop();

    mutable std::mutex                 streamLock_;
    std::shared_ptr<oboe::AudioStream> stream_;
    UdpSocket                          socket_;
    SourceTable                        sources_;
    Limiter                            limiter_;

    std::thread       netThread_;
    std::atomic<bool> running_{false};
    // Bumped on every start and stop. An error-recovery reopen that was
    // sleeping across a stop/start pair sees the change and stands down rather
    // than installing a second output stream over the live one.
    std::atomic<uint32_t> streamEpoch_{0};
    int               port_     = kDefaultAudioPort;
    int               jitterMs_ = 15;

    std::atomic<float>    masterGain_{1.0f};
    std::atomic<uint32_t> masterPeakMilli_{0};
    std::atomic<uint32_t> limiterGainMilli_{1000};
    std::atomic<uint32_t> activeSources_{0};
    std::atomic<uint64_t> packets_{0};
    std::atomic<uint64_t> badPackets_{0};

    // Audio-thread scratch, sized once at start().
    std::vector<float>   mixBuf_;
    std::vector<int16_t> srcBuf_;
};

}  // namespace lau
