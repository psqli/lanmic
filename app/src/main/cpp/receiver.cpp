#include "receiver.h"

#include <chrono>
#include <cstring>

#include "log.h"
#include "meter.h"
#include "util.h"

namespace lau {

namespace {
constexpr int    kMaxCallbackFrames = 8192;
constexpr int64_t kSourceTimeoutMs  = 2000;
}  // namespace

Receiver::~Receiver() { stop(); }

bool Receiver::start(int port, int jitterMs) {
    if (running_.load(std::memory_order_acquire)) return true;

    if (jitterMs < 5) jitterMs = 5;
    if (jitterMs > 200) jitterMs = 200;
    port_     = port;
    jitterMs_ = jitterMs;

    const int targetFrames = kSampleRate * jitterMs / 1000;
    sources_.configure(targetFrames);
    limiter_.configure(0.98f, kSampleRate, 150.0f);

    mixBuf_.assign(kMaxCallbackFrames, 0.0f);
    srcBuf_.assign(kMaxCallbackFrames, 0);

    if (!socket_.openReceiver(static_cast<uint16_t>(port_), 200)) return false;

    running_.store(true, std::memory_order_release);
    netThread_ = std::thread([this] { networkLoop(); });

    if (!openStream()) {
        running_.store(false, std::memory_order_release);
        if (netThread_.joinable()) netThread_.join();
        socket_.close();
        return false;
    }
    LAU_LOGI("server listening on udp/%d, jitter target %d ms", port_, jitterMs_);
    return true;
}

bool Receiver::openStream() {
    oboe::AudioStreamBuilder b;
    b.setDirection(oboe::Direction::Output)
        ->setPerformanceMode(oboe::PerformanceMode::LowLatency)
        ->setSharingMode(oboe::SharingMode::Exclusive)
        ->setFormat(oboe::AudioFormat::I16)
        ->setFormatConversionAllowed(true)
        ->setChannelConversionAllowed(true)
        ->setSampleRateConversionQuality(oboe::SampleRateConversionQuality::Medium)
        ->setChannelCount(oboe::ChannelCount::Stereo)
        ->setSampleRate(kSampleRate)
        ->setUsage(oboe::Usage::Media)
        ->setContentType(oboe::ContentType::Speech)
        ->setDataCallback(this)
        ->setErrorCallback(this);

    std::shared_ptr<oboe::AudioStream> stream;
    const oboe::Result r = b.openStream(stream);
    if (r != oboe::Result::OK) {
        LAU_LOGE("failed to open output stream: %s", oboe::convertToText(r));
        return false;
    }
    stream->setBufferSizeInFrames(stream->getFramesPerBurst() * 2);

    const oboe::Result s = stream->requestStart();
    if (s != oboe::Result::OK) {
        LAU_LOGE("failed to start output stream: %s", oboe::convertToText(s));
        stream->close();
        return false;
    }
    LAU_LOGI("output stream: rate=%d ch=%d fmt=%d burst=%d", stream->getSampleRate(),
             stream->getChannelCount(), static_cast<int>(stream->getFormat()),
             stream->getFramesPerBurst());
    {
        std::lock_guard<std::mutex> lock(streamLock_);
        stream_ = stream;
    }
    return true;
}

void Receiver::closeStream() {
    std::shared_ptr<oboe::AudioStream> stream;
    {
        std::lock_guard<std::mutex> lock(streamLock_);
        stream = stream_;
        stream_.reset();
    }
    if (stream) {
        stream->requestStop();
        stream->close();
    }
}

void Receiver::stop() {
    if (!running_.exchange(false, std::memory_order_acq_rel)) return;
    closeStream();
    if (netThread_.joinable()) netThread_.join();
    socket_.close();
    LAU_LOGI("server stopped");
}

oboe::DataCallbackResult Receiver::onAudioReady(oboe::AudioStream* stream, void* audioData,
                                                int32_t numFrames) {
    const size_t frames   = static_cast<size_t>(numFrames);
    const int    channels = stream->getChannelCount();

    if (frames > mixBuf_.size()) {
        // Should never happen; fail safe with silence rather than allocating
        // on the audio thread.
        const size_t bytes = frames * channels *
                             (stream->getFormat() == oboe::AudioFormat::Float ? 4 : 2);
        std::memset(audioData, 0, bytes);
        return oboe::DataCallbackResult::Continue;
    }

    float* mix = mixBuf_.data();
    const int active = sources_.mix(mix, frames, srcBuf_.data());
    activeSources_.store(static_cast<uint32_t>(active), std::memory_order_relaxed);

    const float mg = masterGain_.load(std::memory_order_relaxed);
    if (mg != 1.0f) {
        for (size_t i = 0; i < frames; ++i) mix[i] *= mg;
    }
    limiter_.process(mix, frames);
    limiterGainMilli_.store(static_cast<uint32_t>(limiter_.gain() * 1000.0f),
                            std::memory_order_relaxed);

    float peak = 0.0f;
    for (size_t i = 0; i < frames; ++i) {
        const float a = mix[i] < 0 ? -mix[i] : mix[i];
        if (a > peak) peak = a;
    }
    updatePeakMeter(masterPeakMilli_, peak);

    if (stream->getFormat() == oboe::AudioFormat::Float) {
        float* out = static_cast<float*>(audioData);
        for (size_t i = 0; i < frames; ++i)
            for (int c = 0; c < channels; ++c) out[i * channels + c] = mix[i];
    } else {
        int16_t* out = static_cast<int16_t*>(audioData);
        for (size_t i = 0; i < frames; ++i) {
            float v = mix[i] * 32767.0f;
            if (v > 32767.0f) v = 32767.0f;
            if (v < -32768.0f) v = -32768.0f;
            const int16_t s = static_cast<int16_t>(v);
            for (int c = 0; c < channels; ++c) out[i * channels + c] = s;
        }
    }
    return oboe::DataCallbackResult::Continue;
}

void Receiver::networkLoop() {
    raiseThreadPriority();
    std::vector<uint8_t> buf(kMaxPacketBytes);
    std::vector<int16_t> pcm(kMaxFramesPerPacket);
    int64_t lastReap = nowMs();

    while (running_.load(std::memory_order_acquire)) {
        const int n = socket_.receive(buf.data(), buf.size());
        const int64_t now = nowMs();

        if (now - lastReap > 500) {
            sources_.reapStale(now, kSourceTimeoutMs);
            lastReap = now;
        }
        if (n <= 0) continue;

        Header h;
        if (!parse_header(buf.data(), static_cast<size_t>(n), &h)) {
            badPackets_.fetch_add(1, std::memory_order_relaxed);
            continue;
        }
        packets_.fetch_add(1, std::memory_order_relaxed);

        if (h.type == kTypeBye) {
            sources_.retire(h.ssrc);
            continue;
        }
        if (h.type == kTypeHello) {
            sources_.acquire(h.ssrc, now);
            continue;
        }
        if (h.type != kTypeAudio) continue;

        const size_t payload = static_cast<size_t>(n) - kHeaderBytes;
        const size_t frames  = payload / 2;
        if (frames == 0 || frames > static_cast<size_t>(kMaxFramesPerPacket) ||
            h.channels != 1) {
            badPackets_.fetch_add(1, std::memory_order_relaxed);
            continue;
        }

        JitterBuffer* jb = sources_.acquire(h.ssrc, now);
        if (jb == nullptr) continue;  // table full

        const bool muted = (h.flags & kFlagMuted) != 0;
        if (!muted) wire_to_pcm(pcm.data(), buf.data() + kHeaderBytes, frames);
        jb->onPacket(h.seq, h.timestamp, muted ? nullptr : pcm.data(), frames, muted);
    }
}

void Receiver::onErrorAfterClose(oboe::AudioStream* /*stream*/, oboe::Result error) {
    LAU_LOGW("output stream error: %s - reopening", oboe::convertToText(error));
    if (!running_.load(std::memory_order_acquire)) return;
    std::thread([this] {
        std::this_thread::sleep_for(std::chrono::milliseconds(200));
        if (!running_.load(std::memory_order_acquire)) return;
        if (openStream() && !running_.load(std::memory_order_acquire)) {
            // stop() ran while we were reopening and saw no stream to close.
            closeStream();
        }
    }).detach();
}

RxStats Receiver::stats() const {
    RxStats s;
    s.packets       = packets_.load(std::memory_order_relaxed);
    s.badPackets    = badPackets_.load(std::memory_order_relaxed);
    s.activeSources = activeSources_.load(std::memory_order_relaxed);
    s.masterPeak    = masterPeakMilli_.load(std::memory_order_relaxed) * 0.001f;
    s.limiterGain   = limiterGainMilli_.load(std::memory_order_relaxed) * 0.001f;
    std::shared_ptr<oboe::AudioStream> stream;
    {
        std::lock_guard<std::mutex> lock(streamLock_);
        stream = stream_;
    }
    if (stream) {
        auto xr = stream->getXRunCount();
        if (xr) s.xruns = static_cast<uint32_t>(xr.value());
        auto lat = stream->calculateLatencyMillis();
        if (lat) s.latencyMs = static_cast<float>(lat.value());
    }
    return s;
}

}  // namespace lau
