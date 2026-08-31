package com.lanmic.audio

import android.content.Context
import android.content.SharedPreferences

/**
 * Typed wrapper over the single preferences file. Screens read and write named
 * properties instead of each repeating the key strings and the edit/apply
 * dance - which is how the port ended up being loaded but never saved.
 */
class Settings private constructor(private val p: SharedPreferences) {

    companion object {
        private const val FILE = "lanmic"

        fun of(ctx: Context): Settings =
            Settings(ctx.getSharedPreferences(FILE, Context.MODE_PRIVATE))
    }

    /** Server address the microphone sends to. */
    var host: String
        get() = p.getString("host", "").orEmpty()
        set(v) = p.edit().putString("host", v).apply()

    /**
     * Audio port, shared by both modes on purpose: a phone used as a
     * microphone and the same phone used as a mixer want the same number.
     */
    var port: Int
        get() = p.getInt("port", NativeAudio.DEFAULT_PORT)
        set(v) = p.edit().putInt("port", v).apply()

    /** Capture packet size in frames: 120, 240 or 480. */
    var packetFrames: Int
        get() = p.getInt("packetFrames", 240)
        set(v) = p.edit().putInt("packetFrames", v).apply()

    /** 0 = VoicePerformance, 1 = Unprocessed, 2 = VoiceRecognition. */
    var inputPreset: Int
        get() = p.getInt("preset", 0)
        set(v) = p.edit().putInt("preset", v).apply()

    var txGain: Float
        get() = p.getFloat("txGain", 1f)
        set(v) = p.edit().putFloat("txGain", v).apply()

    /** Jitter buffer target depth in milliseconds. */
    var jitterMs: Int
        get() = p.getInt("jitterMs", 15)
        set(v) = p.edit().putInt("jitterMs", v).apply()

    var masterGain: Float
        get() = p.getFloat("master", 1f)
        set(v) = p.edit().putFloat("master", v).apply()

    /**
     * Feedback-suppression depth in Hz on the mix bus; 0 is off. On by default,
     * because a microphone and a loudspeaker in one room is the normal case for
     * this app rather than the exception.
     */
    var feedbackShiftHz: Float
        get() = p.getFloat("feedbackShift", NativeAudio.DEFAULT_FEEDBACK_SHIFT_HZ)
        set(v) = p.edit().putFloat("feedbackShift", v).apply()
}
