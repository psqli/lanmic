#include "transmitter.h"

#include <errno.h>
#include <time.h>

#include <chrono>
#include <cstdlib>
#include <random>

#include "log.h"
#include "meter.h"
#include "util.h"

namespace lau {

Transmitter::Transmitter() {
    sem_init(&sem_, 0, 0);
}

Transmitter::~Transmitter() {
    stop();
    sem_destroy(&sem_);
}

bool Transmitter::start(const std::string& host, int port, int framesPerPacket,
                        int inputPreset) {
    if (running_.load(std::memory_order_acquire)) return true;

    if (framesPerPacket < 60) framesPerPacket = 60;
    if (framesPerPacket > kMaxFramesPerPacket) framesPerPacket = kMaxFramesPerPacket;

    host_            = host;
    port_            = port;
    framesPerPacket_ = framesPerPacket;
    inputPreset_     = inputPreset;

    std::random_device rd;
    ssrc_      = rd() | 1u;
    seq_       = 0;
    timestamp_ = 0;

    if (!socket_.openSender(host_, static_cast<uint16_t>(port_))) return false;

    // Half a second of headroom is plenty; if we ever fill it the network is
    // gone and stale audio is worthless anyway.
    ring_.init(static_cast<size_t>(kSampleRate / 2));
    cbScratch_.assign(static_cast<size_t>(kMaxFramesPerPacket) * 4, 0);
    txPacket_.assign(kMaxPacketBytes, 0);

    // Sent before the sender thread exists: seq_/timestamp_ belong to that
    // thread once it is running, and reading them from here would race.
    sendControl(kTypeHello, 3);

    running_.store(true, std::memory_order_release);
    sender_ = std::thread([this] { senderLoop(); });

    if (!openStream(inputPreset)) {
        running_.store(false, std::memory_order_release);
        sem_post(&sem_);
        if (sender_.joinable()) sender_.join();
        socket_.close();
        return false;
    }

    LAU_LOGI("transmitter started -> %s:%d, %d frames/packet, ssrc=%08x", host_.c_str(),
             port_, framesPerPacket_, ssrc_);
    return true;
}

bool Transmitter::openStream(int inputPreset) {
    oboe::InputPreset preset = oboe::InputPreset::VoicePerformance;
    if (inputPreset == 1) preset = oboe::InputPreset::Unprocessed;
    else if (inputPreset == 2) preset = oboe::InputPreset::VoiceRecognition;

    oboe::AudioStreamBuilder b;
    b.setDirection(oboe::Direction::Input)
        ->setPerformanceMode(oboe::PerformanceMode::LowLatency)
        ->setSharingMode(oboe::SharingMode::Exclusive)
        ->setFormat(oboe::AudioFormat::I16)
        ->setFormatConversionAllowed(true)
        ->setChannelConversionAllowed(true)
        ->setSampleRateConversionQuality(oboe::SampleRateConversionQuality::Medium)
        ->setChannelCount(oboe::ChannelCount::Mono)
        ->setSampleRate(kSampleRate)
        ->setInputPreset(preset)
        ->setDataCallback(this)
        ->setErrorCallback(this);

    std::shared_ptr<oboe::AudioStream> stream;
    const oboe::Result r = b.openStream(stream);
    if (r != oboe::Result::OK) {
        LAU_LOGE("failed to open input stream: %s", oboe::convertToText(r));
        return false;
    }
    // Two bursts is the sweet spot: one is glitchy on most devices, three adds
    // a burst of latency for nothing.
    stream->setBufferSizeInFrames(stream->getFramesPerBurst() * 2);

    const oboe::Result s = stream->requestStart();
    if (s != oboe::Result::OK) {
        LAU_LOGE("failed to start input stream: %s", oboe::convertToText(s));
        stream->close();
        return false;
    }
    LAU_LOGI("input stream: rate=%d ch=%d fmt=%d burst=%d", stream->getSampleRate(),
             stream->getChannelCount(), static_cast<int>(stream->getFormat()),
             stream->getFramesPerBurst());
    {
        std::lock_guard<std::mutex> lock(streamLock_);
        stream_ = stream;
    }
    return true;
}

void Transmitter::closeStream() {
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

void Transmitter::stop() {
    if (!running_.exchange(false, std::memory_order_acq_rel)) return;

    closeStream();
    // Join first, then send: BYE reads seq_/timestamp_, which the sender
    // thread is still advancing until it has actually exited.
    sem_post(&sem_);
    if (sender_.joinable()) sender_.join();
    sendControl(kTypeBye, 3);
    socket_.close();
    LAU_LOGI("transmitter stopped");
}

oboe::DataCallbackResult Transmitter::onAudioReady(oboe::AudioStream* stream,
                                                   void* audioData, int32_t numFrames) {
    const int channels = stream->getChannelCount();
    const size_t frames = static_cast<size_t>(numFrames);
    if (frames == 0) return oboe::DataCallbackResult::Continue;

    if (cbScratch_.size() < frames) cbScratch_.assign(frames * 2, 0);
    int16_t* mono = cbScratch_.data();

    const float gain = muted_.load(std::memory_order_relaxed)
                           ? 0.0f
                           : gain_.load(std::memory_order_relaxed);

    if (stream->getFormat() == oboe::AudioFormat::Float) {
        const float* in = static_cast<const float*>(audioData);
        for (size_t i = 0; i < frames; ++i) {
            float acc = 0.0f;
            for (int c = 0; c < channels; ++c) acc += in[i * channels + c];
            acc = acc / channels * gain * 32767.0f;
            if (acc > 32767.0f) acc = 32767.0f;
            if (acc < -32768.0f) acc = -32768.0f;
            mono[i] = static_cast<int16_t>(acc);
        }
    } else {
        const int16_t* in = static_cast<const int16_t*>(audioData);
        for (size_t i = 0; i < frames; ++i) {
            int32_t acc = 0;
            for (int c = 0; c < channels; ++c) acc += in[i * channels + c];
            float v = static_cast<float>(acc) / channels * gain;
            if (v > 32767.0f) v = 32767.0f;
            if (v < -32768.0f) v = -32768.0f;
            mono[i] = static_cast<int16_t>(v);
        }
    }

    int32_t peak = 0;
    for (size_t i = 0; i < frames; ++i) {
        const int32_t a = mono[i] < 0 ? -static_cast<int32_t>(mono[i]) : mono[i];
        if (a > peak) peak = a;
    }
    updatePeakMeter(peakMilli_, static_cast<float>(peak) * (1.0f / 32768.0f));

    const size_t written = ring_.write(mono, frames);
    if (written < frames) {
        framesDropped_.fetch_add(frames - written, std::memory_order_relaxed);
    }
    sem_post(&sem_);  // wait-free wake-up, safe from the audio callback
    return oboe::DataCallbackResult::Continue;
}

void Transmitter::senderLoop() {
    raiseThreadPriority();
    const size_t fpp = static_cast<size_t>(framesPerPacket_);
    std::vector<int16_t> pcm(fpp);

    while (running_.load(std::memory_order_acquire)) {
        timespec ts{};
        clock_gettime(CLOCK_REALTIME, &ts);
        ts.tv_nsec += 100 * 1000000L;  // 100 ms
        if (ts.tv_nsec >= 1000000000L) {
            ts.tv_nsec -= 1000000000L;
            ts.tv_sec += 1;
        }
        while (sem_timedwait(&sem_, &ts) != 0 && errno == EINTR) {
        }

        while (running_.load(std::memory_order_acquire) && ring_.size() >= fpp) {
            ring_.read(pcm.data(), fpp);
            Header h;
            h.type      = kTypeAudio;
            h.channels  = 1;
            h.flags     = muted_.load(std::memory_order_relaxed) ? kFlagMuted : 0;
            h.ssrc      = ssrc_;
            h.seq       = seq_++;
            h.timestamp = timestamp_;
            timestamp_ += static_cast<uint32_t>(fpp);

            write_header(txPacket_.data(), h);
            pcm_to_wire(txPacket_.data() + kHeaderBytes, pcm.data(), fpp);

            const int n = socket_.send(txPacket_.data(), kHeaderBytes + fpp * 2);
            if (n < 0) {
                sendErrors_.fetch_add(1, std::memory_order_relaxed);
            } else {
                packetsSent_.fetch_add(1, std::memory_order_relaxed);
            }
        }
    }
}

void Transmitter::sendControl(uint8_t type, int repeats) {
    if (!socket_.isOpen()) return;
    uint8_t buf[kHeaderBytes];
    Header  h;
    h.type      = type;
    h.channels  = 1;
    h.ssrc      = ssrc_;
    h.seq       = seq_;
    h.timestamp = timestamp_;
    write_header(buf, h);
    for (int i = 0; i < repeats; ++i) socket_.send(buf, sizeof(buf));
}

void Transmitter::onErrorAfterClose(oboe::AudioStream* /*stream*/, oboe::Result error) {
    LAU_LOGW("input stream error: %s - reopening", oboe::convertToText(error));
    if (!running_.load(std::memory_order_acquire)) return;
    // Device change (headset plugged in, Bluetooth mic connected). Oboe has
    // already closed the stream on its own thread; reopen from a detached one.
    std::thread([this] {
        std::this_thread::sleep_for(std::chrono::milliseconds(200));
        if (!running_.load(std::memory_order_acquire)) return;
        if (openStream(inputPreset_) && !running_.load(std::memory_order_acquire)) {
            // stop() ran while we were reopening and saw no stream to close;
            // the one we just installed would otherwise hold the mic open.
            closeStream();
        }
    }).detach();
}

TxStats Transmitter::stats() const {
    TxStats s;
    s.packetsSent   = packetsSent_.load(std::memory_order_relaxed);
    s.framesDropped = framesDropped_.load(std::memory_order_relaxed);
    s.sendErrors    = sendErrors_.load(std::memory_order_relaxed);
    s.peak          = peakMilli_.load(std::memory_order_relaxed) * 0.001f;
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
