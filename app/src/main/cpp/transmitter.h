#pragma once

#include <oboe/Oboe.h>

#include <atomic>
#include <memory>
#include <mutex>
#include <semaphore.h>
#include <string>
#include <thread>
#include <vector>

#include "protocol.h"
#include "spsc_ring.h"
#include "udp_socket.h"

namespace lau {

struct TxStats {
    uint64_t packetsSent   = 0;
    uint64_t framesDropped = 0;   // ring overflow: the network thread fell behind
    uint32_t sendErrors    = 0;
    uint32_t xruns         = 0;
    float    peak          = 0.0f;
    float    latencyMs     = 0.0f;
};

// Captures with AAudio (via Oboe) in low-latency exclusive mode and ships fixed
// size packets to the server. The audio callback never touches the socket: it
// writes to a wait-free ring and posts a semaphore; a dedicated sender thread
// does the syscalls.
class Transmitter : public oboe::AudioStreamDataCallback,
                    public oboe::AudioStreamErrorCallback {
public:
    Transmitter();
    ~Transmitter() override;

    // framesPerPacket: 120 / 240 / 480 (2.5 / 5 / 10 ms at 48 kHz)
    // inputPreset: 0 = VoicePerformance (default), 1 = Unprocessed, 2 = VoiceRecognition
    bool start(const std::string& host, int port, int framesPerPacket, int inputPreset);
    void stop();
    bool isRunning() const { return running_.load(std::memory_order_acquire); }

    void  setGain(float g) { gain_.store(g, std::memory_order_relaxed); }
    void  setMuted(bool m) { muted_.store(m, std::memory_order_relaxed); }
    bool  isMuted() const { return muted_.load(std::memory_order_relaxed); }
    TxStats stats() const;

    oboe::DataCallbackResult onAudioReady(oboe::AudioStream* stream, void* audioData,
                                          int32_t numFrames) override;
    void onErrorAfterClose(oboe::AudioStream* stream, oboe::Result error) override;

private:
    bool openStream(int inputPreset);
    void senderLoop();
    void sendControl(uint8_t type, int repeats);

    mutable std::mutex                 streamLock_;
    std::shared_ptr<oboe::AudioStream> stream_;
    UdpSocket                          socket_;
    std::string                        host_;
    int                                port_            = kDefaultAudioPort;
    int                                framesPerPacket_ = 240;
    int                                inputPreset_     = 0;

    SpscRing<int16_t> ring_;
    sem_t             sem_{};
    std::thread       sender_;
    std::atomic<bool> running_{false};

    std::atomic<float>    gain_{1.0f};
    std::atomic<bool>     muted_{false};
    std::atomic<uint32_t> peakMilli_{0};
    std::atomic<uint64_t> packetsSent_{0};
    std::atomic<uint64_t> framesDropped_{0};
    std::atomic<uint32_t> sendErrors_{0};

    uint32_t ssrc_      = 0;
    uint32_t seq_       = 0;
    uint32_t timestamp_ = 0;

    // Scratch owned by the audio callback only.
    std::vector<int16_t> cbScratch_;
    // Scratch owned by the sender thread only.
    std::vector<uint8_t> txPacket_;
};

}  // namespace lau
