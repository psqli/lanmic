package com.lanmic.audio

import android.content.Context
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.os.Handler
import android.os.Looper

/**
 * One routing choice, as the two screens draw it and as the service resolves it.
 *
 * Oboe takes an `AudioDeviceInfo.getId()`, which is assigned when a device
 * appears and is not stable: unplug a USB interface and plug it back in and it
 * has a new id. So what is remembered in [Settings] is [key], which is built
 * out of the type and the product name and does survive that - and the id is
 * looked up again at the moment a stream is opened.
 */
data class AudioEndpoint(
    /** `AudioDeviceInfo.getId()`, or [AudioDevices.UNSPECIFIED] for "Automatic". */
    val id: Int,
    /** Stable across a reboot and a replug, which [id] is not. */
    val key: String,
    val label: String
)

/**
 * The input and output devices worth offering, and the lookup from a remembered
 * choice back to an id the engine can use.
 *
 * The lists are built from an allow-list of `AudioDeviceInfo` types rather than
 * a deny-list, for two reasons. Bluetooth is the first: A2DP and SCO both cost
 * more latency than this whole app's budget, so offering them would be offering
 * a way to make it not work. The second is that everything else on the list is
 * a jack, a socket or a speaker someone can point at - telephony endpoints,
 * remote submixes and echo references are neither, and a menu of them is worse
 * than a menu without them.
 */
object AudioDevices {

    /** Oboe's `kUnspecified`: let Android route the stream itself. */
    const val UNSPECIFIED = 0

    /** The key of the "Automatic" row, and of a choice that no longer exists. */
    const val AUTOMATIC = ""

    /**
     * Every type below is API 26 or older, which is this app's `minSdk`, so
     * these are compile-time constants on every device that runs it.
     */
    private val INPUT_TYPES = setOf(
        AudioDeviceInfo.TYPE_BUILTIN_MIC,
        AudioDeviceInfo.TYPE_WIRED_HEADSET,
        AudioDeviceInfo.TYPE_USB_DEVICE,
        AudioDeviceInfo.TYPE_USB_ACCESSORY,
        AudioDeviceInfo.TYPE_USB_HEADSET,
        AudioDeviceInfo.TYPE_DOCK,
        AudioDeviceInfo.TYPE_LINE_ANALOG,
        AudioDeviceInfo.TYPE_LINE_DIGITAL,
        AudioDeviceInfo.TYPE_AUX_LINE
    )

    private val OUTPUT_TYPES = setOf(
        AudioDeviceInfo.TYPE_BUILTIN_SPEAKER,
        AudioDeviceInfo.TYPE_BUILTIN_EARPIECE,
        AudioDeviceInfo.TYPE_WIRED_HEADSET,
        AudioDeviceInfo.TYPE_WIRED_HEADPHONES,
        AudioDeviceInfo.TYPE_USB_DEVICE,
        AudioDeviceInfo.TYPE_USB_ACCESSORY,
        AudioDeviceInfo.TYPE_USB_HEADSET,
        AudioDeviceInfo.TYPE_HDMI,
        AudioDeviceInfo.TYPE_HDMI_ARC,
        AudioDeviceInfo.TYPE_DOCK,
        AudioDeviceInfo.TYPE_LINE_ANALOG,
        AudioDeviceInfo.TYPE_LINE_DIGITAL,
        AudioDeviceInfo.TYPE_AUX_LINE
    )

    /** Types whose product name is the phone itself, and so says nothing. */
    private val BUILT_IN = setOf(
        AudioDeviceInfo.TYPE_BUILTIN_MIC,
        AudioDeviceInfo.TYPE_BUILTIN_SPEAKER,
        AudioDeviceInfo.TYPE_BUILTIN_EARPIECE
    )

    fun inputs(ctx: Context): List<AudioEndpoint> =
        list(ctx, AudioManager.GET_DEVICES_INPUTS, INPUT_TYPES)

    fun outputs(ctx: Context): List<AudioEndpoint> =
        list(ctx, AudioManager.GET_DEVICES_OUTPUTS, OUTPUT_TYPES)

    /**
     * The id to hand the engine for a remembered choice: the device if it is
     * plugged in, and [UNSPECIFIED] - which is "whatever Android would have
     * picked" - if it is not. A microphone that starts on the built-in mic
     * because the interface was left in the other bag is a better outcome than
     * one that refuses to start.
     */
    fun resolve(ctx: Context, key: String, input: Boolean): Int {
        if (key == AUTOMATIC) return UNSPECIFIED
        val devices = if (input) inputs(ctx) else outputs(ctx)
        return devices.firstOrNull { it.key == key }?.id ?: UNSPECIFIED
    }

    private fun list(ctx: Context, flags: Int, types: Set<Int>): List<AudioEndpoint> {
        val am = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        val out = ArrayList<AudioEndpoint>()
        val seen = HashSet<String>()
        for (device in am.getDevices(flags)) {
            if (device.type !in types) continue
            val key = keyOf(device)
            // Phones report the built-in microphone once per physical capsule;
            // they are one choice as far as this app is concerned.
            if (!seen.add(key)) continue
            out += AudioEndpoint(id = device.id, key = key, label = labelOf(device))
        }
        return out
    }

    private fun keyOf(device: AudioDeviceInfo): String =
        "${device.type}/${product(device)}"

    private fun labelOf(device: AudioDeviceInfo): String {
        val type = typeName(device.type)
        val product = product(device)
        return if (product.isEmpty() || device.type in BUILT_IN) type else "$type - $product"
    }

    private fun product(device: AudioDeviceInfo): String =
        device.productName?.toString()?.trim().orEmpty()

    private fun typeName(type: Int): String = when (type) {
        AudioDeviceInfo.TYPE_BUILTIN_MIC -> "Phone microphone"
        AudioDeviceInfo.TYPE_BUILTIN_SPEAKER -> "Phone speaker"
        AudioDeviceInfo.TYPE_BUILTIN_EARPIECE -> "Earpiece"
        AudioDeviceInfo.TYPE_WIRED_HEADSET -> "Wired headset"
        AudioDeviceInfo.TYPE_WIRED_HEADPHONES -> "Wired headphones"
        AudioDeviceInfo.TYPE_USB_DEVICE -> "USB audio"
        AudioDeviceInfo.TYPE_USB_ACCESSORY -> "USB accessory"
        AudioDeviceInfo.TYPE_USB_HEADSET -> "USB headset"
        AudioDeviceInfo.TYPE_HDMI -> "HDMI"
        AudioDeviceInfo.TYPE_HDMI_ARC -> "HDMI ARC"
        AudioDeviceInfo.TYPE_DOCK -> "Dock"
        AudioDeviceInfo.TYPE_LINE_ANALOG -> "Line in"
        AudioDeviceInfo.TYPE_LINE_DIGITAL -> "Digital line"
        AudioDeviceInfo.TYPE_AUX_LINE -> "Aux"
        else -> "Audio device"
    }

    /**
     * Calls [onChange] whenever something is plugged in or pulled out. Returned
     * so the caller can unregister it; a callback outliving its screen would
     * keep recomposing one that is gone.
     */
    fun watch(ctx: Context, onChange: () -> Unit): AudioDeviceCallback {
        val am = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        val callback = object : AudioDeviceCallback() {
            override fun onAudioDevicesAdded(addedDevices: Array<out AudioDeviceInfo>?) =
                onChange()

            override fun onAudioDevicesRemoved(removedDevices: Array<out AudioDeviceInfo>?) =
                onChange()
        }
        am.registerAudioDeviceCallback(callback, Handler(Looper.getMainLooper()))
        return callback
    }

    fun unwatch(ctx: Context, callback: AudioDeviceCallback) {
        val am = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        am.unregisterAudioDeviceCallback(callback)
    }
}
