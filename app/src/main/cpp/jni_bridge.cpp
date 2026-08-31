#include <jni.h>

#include <memory>
#include <mutex>
#include <string>

#include "log.h"
#include "receiver.h"
#include "transmitter.h"

namespace {

std::mutex                       gLock;
std::unique_ptr<lau::Transmitter> gTx;
std::unique_ptr<lau::Receiver>    gRx;

std::string jstr(JNIEnv* env, jstring s) {
    if (s == nullptr) return {};
    const char* c = env->GetStringUTFChars(s, nullptr);
    std::string out(c ? c : "");
    if (c) env->ReleaseStringUTFChars(s, c);
    return out;
}

jdoubleArray toArray(JNIEnv* env, const double* v, int n) {
    jdoubleArray a = env->NewDoubleArray(n);
    if (a != nullptr) env->SetDoubleArrayRegion(a, 0, n, v);
    return a;
}

}  // namespace

extern "C" {

JNIEXPORT jboolean JNICALL Java_com_lanmic_audio_NativeAudio_nativeStartTransmitter(
    JNIEnv* env, jobject, jstring host, jint port, jint framesPerPacket, jint inputPreset) {
    std::lock_guard<std::mutex> lock(gLock);
    if (!gTx) gTx = std::make_unique<lau::Transmitter>();
    return gTx->start(jstr(env, host), port, framesPerPacket, inputPreset) ? JNI_TRUE
                                                                          : JNI_FALSE;
}

JNIEXPORT void JNICALL Java_com_lanmic_audio_NativeAudio_nativeStopTransmitter(JNIEnv*,
                                                                              jobject) {
    std::lock_guard<std::mutex> lock(gLock);
    if (gTx) gTx->stop();
}

JNIEXPORT jboolean JNICALL Java_com_lanmic_audio_NativeAudio_nativeIsTransmitting(JNIEnv*,
                                                                                 jobject) {
    std::unique_lock<std::mutex> lock(gLock, std::try_to_lock);
    if (!lock.owns_lock()) return JNI_FALSE;  // mid start/stop
    return (gTx && gTx->isRunning()) ? JNI_TRUE : JNI_FALSE;
}

JNIEXPORT void JNICALL Java_com_lanmic_audio_NativeAudio_nativeSetTxGain(JNIEnv*, jobject,
                                                                        jfloat g) {
    std::lock_guard<std::mutex> lock(gLock);
    if (gTx) gTx->setGain(g);
}

JNIEXPORT void JNICALL Java_com_lanmic_audio_NativeAudio_nativeSetTxMuted(JNIEnv*, jobject,
                                                                         jboolean m) {
    std::lock_guard<std::mutex> lock(gLock);
    if (gTx) gTx->setMuted(m == JNI_TRUE);
}

// [packetsSent, framesDropped, sendErrors, xruns, peak, latencyMs, running]
JNIEXPORT jdoubleArray JNICALL
Java_com_lanmic_audio_NativeAudio_nativeTxStats(JNIEnv* env, jobject) {
    lau::TxStats s;
    bool running = false;
    {
        std::unique_lock<std::mutex> lock(gLock, std::try_to_lock);
        if (lock.owns_lock() && gTx) {
            s       = gTx->stats();
            running = gTx->isRunning();
        }
    }
    const double v[7] = {static_cast<double>(s.packetsSent),
                         static_cast<double>(s.framesDropped),
                         static_cast<double>(s.sendErrors),
                         static_cast<double>(s.xruns),
                         static_cast<double>(s.peak),
                         static_cast<double>(s.latencyMs),
                         running ? 1.0 : 0.0};
    return toArray(env, v, 7);
}

JNIEXPORT jboolean JNICALL Java_com_lanmic_audio_NativeAudio_nativeStartServer(
    JNIEnv*, jobject, jint port, jint jitterMs) {
    std::lock_guard<std::mutex> lock(gLock);
    if (!gRx) gRx = std::make_unique<lau::Receiver>();
    return gRx->start(port, jitterMs) ? JNI_TRUE : JNI_FALSE;
}

JNIEXPORT void JNICALL Java_com_lanmic_audio_NativeAudio_nativeStopServer(JNIEnv*, jobject) {
    std::lock_guard<std::mutex> lock(gLock);
    if (gRx) gRx->stop();
}

JNIEXPORT jboolean JNICALL Java_com_lanmic_audio_NativeAudio_nativeIsServing(JNIEnv*,
                                                                            jobject) {
    std::unique_lock<std::mutex> lock(gLock, std::try_to_lock);
    if (!lock.owns_lock()) return JNI_FALSE;
    return (gRx && gRx->isRunning()) ? JNI_TRUE : JNI_FALSE;
}

JNIEXPORT void JNICALL Java_com_lanmic_audio_NativeAudio_nativeSetMasterGain(JNIEnv*,
                                                                            jobject,
                                                                            jfloat g) {
    std::lock_guard<std::mutex> lock(gLock);
    if (gRx) gRx->setMasterGain(g);
}

JNIEXPORT void JNICALL Java_com_lanmic_audio_NativeAudio_nativeSetSourceGain(
    JNIEnv*, jobject, jlong ssrc, jfloat g) {
    std::lock_guard<std::mutex> lock(gLock);
    if (gRx) gRx->setSourceGain(static_cast<uint32_t>(ssrc), g);
}

JNIEXPORT void JNICALL Java_com_lanmic_audio_NativeAudio_nativeSetSourceMuted(
    JNIEnv*, jobject, jlong ssrc, jboolean m) {
    std::lock_guard<std::mutex> lock(gLock);
    if (gRx) gRx->setSourceMuted(static_cast<uint32_t>(ssrc), m == JNI_TRUE);
}

// [packets, badPackets, activeSources, xruns, masterPeak, limiterGain, latencyMs, running]
JNIEXPORT jdoubleArray JNICALL
Java_com_lanmic_audio_NativeAudio_nativeServerStats(JNIEnv* env, jobject) {
    lau::RxStats s;
    bool running = false;
    {
        std::unique_lock<std::mutex> lock(gLock, std::try_to_lock);
        if (lock.owns_lock() && gRx) {
            s       = gRx->stats();
            running = gRx->isRunning();
        }
    }
    const double v[8] = {static_cast<double>(s.packets),
                         static_cast<double>(s.badPackets),
                         static_cast<double>(s.activeSources),
                         static_cast<double>(s.xruns),
                         static_cast<double>(s.masterPeak),
                         static_cast<double>(s.limiterGain),
                         static_cast<double>(s.latencyMs),
                         running ? 1.0 : 0.0};
    return toArray(env, v, 8);
}

// 9 doubles per source:
// [ssrc, peak, bufferFrames, packets, lost, underruns, ageMs, muted, gain]
JNIEXPORT jdoubleArray JNICALL
Java_com_lanmic_audio_NativeAudio_nativeServerSources(JNIEnv* env, jobject) {
    lau::SourceSnapshot snaps[lau::kMaxSources];
    int n = 0;
    {
        std::unique_lock<std::mutex> lock(gLock, std::try_to_lock);
        if (lock.owns_lock() && gRx) n = gRx->snapshot(snaps, lau::kMaxSources);
    }
    double v[lau::kMaxSources * 9];
    for (int i = 0; i < n; ++i) {
        double* d = v + i * 9;
        d[0] = static_cast<double>(snaps[i].ssrc);
        d[1] = snaps[i].peakMilli * 0.001;
        d[2] = static_cast<double>(snaps[i].bufferFrames);
        d[3] = static_cast<double>(snaps[i].packets);
        d[4] = static_cast<double>(snaps[i].lost);
        d[5] = static_cast<double>(snaps[i].underruns);
        d[6] = static_cast<double>(snaps[i].ageMs);
        d[7] = static_cast<double>(snaps[i].muted);
        d[8] = snaps[i].gainMilli * 0.001;
    }
    return toArray(env, v, n * 9);
}

}  // extern "C"
