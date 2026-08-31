package com.lanmic.audio

/** Thin JNI facade over the C++ audio engine. All calls are cheap and non-blocking. */
object NativeAudio {

    init {
        System.loadLibrary("lanmic")
    }

    const val DEFAULT_PORT = 45678
    const val DISCOVERY_PORT = 45679
    const val SAMPLE_RATE = 48000

    // ---- transmitter ----

    private external fun nativeStartTransmitter(
        host: String,
        port: Int,
        framesPerPacket: Int,
        inputPreset: Int
    ): Boolean

    private external fun nativeStopTransmitter()
    private external fun nativeIsTransmitting(): Boolean
    private external fun nativeSetTxGain(gain: Float)
    private external fun nativeSetTxMuted(muted: Boolean)
    private external fun nativeTxStats(): DoubleArray

    fun startTransmitter(host: String, port: Int, packetFrames: Int, inputPreset: Int) =
        nativeStartTransmitter(host, port, packetFrames, inputPreset)

    fun stopTransmitter() = nativeStopTransmitter()
    fun isTransmitting() = nativeIsTransmitting()
    fun setTxGain(gain: Float) = nativeSetTxGain(gain)
    fun setTxMuted(muted: Boolean) = nativeSetTxMuted(muted)

    fun txStats(): TxStats {
        val v = nativeTxStats()
        if (v.size < 7) return TxStats()
        return TxStats(
            packetsSent = v[0].toLong(),
            framesDropped = v[1].toLong(),
            sendErrors = v[2].toLong(),
            xruns = v[3].toInt(),
            peak = v[4].toFloat(),
            latencyMs = v[5].toFloat(),
            running = v[6] > 0.5
        )
    }

    // ---- server ----

    private external fun nativeStartServer(port: Int, jitterMs: Int): Boolean
    private external fun nativeStopServer()
    private external fun nativeIsServing(): Boolean
    private external fun nativeSetMasterGain(gain: Float)
    private external fun nativeSetSourceGain(ssrc: Long, gain: Float)
    private external fun nativeSetSourceMuted(ssrc: Long, muted: Boolean)
    private external fun nativeServerStats(): DoubleArray
    private external fun nativeServerSources(): DoubleArray

    fun startServer(port: Int, jitterMs: Int) = nativeStartServer(port, jitterMs)
    fun stopServer() = nativeStopServer()
    fun isServing() = nativeIsServing()
    fun setMasterGain(gain: Float) = nativeSetMasterGain(gain)
    fun setSourceGain(ssrc: Long, gain: Float) = nativeSetSourceGain(ssrc, gain)
    fun setSourceMuted(ssrc: Long, muted: Boolean) = nativeSetSourceMuted(ssrc, muted)

    fun serverStats(): RxStats {
        val v = nativeServerStats()
        if (v.size < 8) return RxStats()
        return RxStats(
            packets = v[0].toLong(),
            badPackets = v[1].toLong(),
            activeSources = v[2].toInt(),
            xruns = v[3].toInt(),
            masterPeak = v[4].toFloat(),
            limiterGain = v[5].toFloat(),
            latencyMs = v[6].toFloat(),
            running = v[7] > 0.5
        )
    }

    fun serverSources(): List<SourceInfo> {
        val v = nativeServerSources()
        val out = ArrayList<SourceInfo>(v.size / 9)
        var i = 0
        while (i + 8 < v.size) {
            out += SourceInfo(
                ssrc = v[i].toLong(),
                peak = v[i + 1].toFloat(),
                bufferFrames = v[i + 2].toInt(),
                packets = v[i + 3].toLong(),
                lost = v[i + 4].toLong(),
                underruns = v[i + 5].toLong(),
                ageMs = v[i + 6].toInt(),
                muted = v[i + 7] > 0.5,
                gain = v[i + 8].toFloat()
            )
            i += 9
        }
        return out
    }
}

data class TxStats(
    val packetsSent: Long = 0,
    val framesDropped: Long = 0,
    val sendErrors: Long = 0,
    val xruns: Int = 0,
    val peak: Float = 0f,
    val latencyMs: Float = 0f,
    val running: Boolean = false
)

data class RxStats(
    val packets: Long = 0,
    val badPackets: Long = 0,
    val activeSources: Int = 0,
    val xruns: Int = 0,
    val masterPeak: Float = 0f,
    val limiterGain: Float = 1f,
    val latencyMs: Float = 0f,
    val running: Boolean = false
)

data class SourceInfo(
    val ssrc: Long,
    val peak: Float,
    val bufferFrames: Int,
    val packets: Long,
    val lost: Long,
    val underruns: Long,
    val ageMs: Int,
    val muted: Boolean,
    val gain: Float
) {
    val label: String get() = "MIC-%04X".format(ssrc.toInt() and 0xFFFF)
    val bufferMs: Int get() = bufferFrames * 1000 / NativeAudio.SAMPLE_RATE
}
