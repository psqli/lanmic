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

namespace {
// Largest input burst the callback will handle without allocating. 8192 frames
// is 170 ms at 48 kHz - orders of magnitude above any low-latency burst size.
constexpr size_t kMaxCallbackFrames = 8192;
}  // namespace

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

    if (port < 1 || port > 65535) {
        LAU_LOGE("refusing to send to port %d: outside 1..65535", port);
        return false;
    }
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

    // A session starts unmuted and with clean counters. Without this a mute
    // left over from the previous session survives into the new one while the
    // UI, whose own state did reset, shows the microphone as live.
    muted_.store(false, std::memory_order_relaxed);
    peakMilli_.store(0, std::memory_order_relaxed);
    packetsSent_.store(0, std::memory_order_relaxed);
    framesDropped_.store(0, std::memory_order_relaxed);
    sendErrors_.store(0, std::memory_order_relaxed);
    pendingGap_.store(0, std::memory_order_relaxed);

    if (!socket_.openSender(host_, static_cast<uint16_t>(port_))) return false;

    // Half a second of headroom is plenty; if we ever fill it the network is
    // gone and stale audio is worthless anyway.
    ring_.init(static_cast<size_t>(kSampleRate / 2));
    // Sized for any burst a low-latency input stream can plausibly hand us;
    // the callback drops a larger one rather than allocating (see onAudioReady).
    cbScratch_.assign(kMaxCallbackFrames, 0);
    txPacket_.assign(kMaxPacketBytes, 0);

    // Sent before the sender thread exists: seq_/timestamp_ belong to that
    // thread once it is running, and reading them from here would race.
    sendControl(kTypeHello, 3);

    const uint32_t epoch = streamEpoch_.fetch_add(1, std::memory_order_acq_rel) + 1;
    running_.store(true, std::memory_order_release);
    sender_ = std::thread([this] { senderLoop(); });

    if (!openStream(inputPreset, epoch)) {
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

bool Transmitter::openStream(int inputPreset, uint32_t epoch) {
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
    // Install only if this session is still the current one. Two live input
    // streams would both feed the ring, and the ring has exactly one producer.
    {
        std::lock_guard<std::mutex> lock(streamLock_);
        if (streamEpoch_.load(std::memory_order_acquire) == epoch &&
            running_.load(std::memory_order_acquire)) {
            stream_ = stream;
            return true;
        }
    }
    LAU_LOGW("input stream superseded while opening - discarding it");
    stream->requestStop();
    stream->close();
    return false;
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

    streamEpoch_.fetch_add(1, std::memory_order_acq_rel);
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

    if (frames > cbScratch_.size()) {
        // Should never happen; drop the burst rather than allocate on the audio
        // thread, which is what the equivalent path in the receiver does.
        framesDropped_.fetch_add(frames, std::memory_order_relaxed);
        pendingGap_.fetch_add(static_cast<uint32_t>(frames), std::memory_order_relaxed);
        return oboe::DataCallbackResult::Continue;
    }
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
        // Account for the hole in the capture timeline as well as counting it:
        // the sender thread folds it into the next packet's timestamp so the
        // receiver conceals a gap instead of splicing two unrelated instants
        // together and running permanently early.
        framesDropped_.fetch_add(frames - written, std::memory_order_relaxed);
        pendingGap_.fetch_add(static_cast<uint32_t>(frames - written),
                              std::memory_order_relaxed);
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
            timestamp_ += pendingGap_.exchange(0, std::memory_order_relaxed);
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
    // Retry with backoff - the new route is often not ready on the first try,
    // and giving up once leaves the phone "live" with a dead microphone.
    const uint32_t epoch = streamEpoch_.load(std::memory_order_acquire);
    std::thread([this, epoch] {
        int delayMs = 200;
        for (int attempt = 0; attempt < 5; ++attempt) {
            std::this_thread::sleep_for(std::chrono::milliseconds(delayMs));
            if (!running_.load(std::memory_order_acquire) ||
                streamEpoch_.load(std::memory_order_acquire) != epoch) {
                return;  // stopped, or a newer session owns the microphone now
            }
            if (openStream(inputPreset_, epoch)) return;
            delayMs *= 2;
        }
        LAU_LOGE("input stream did not come back after 5 attempts");
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
