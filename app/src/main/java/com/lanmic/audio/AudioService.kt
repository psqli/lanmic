package com.lanmic.audio

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.util.Log
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicInteger

/**
 * Keeps the engine alive with the screen off and holds the Wi-Fi radio in a
 * low-latency power state. Without this, Android parks the Wi-Fi chip between
 * beacons and adds 100 ms+ of jitter within seconds of the screen going dark.
 */
class AudioService : Service() {

    companion object {
        const val ACTION_START_TX = "com.lanmic.audio.START_TX"
        const val ACTION_START_SERVER = "com.lanmic.audio.START_SERVER"
        const val ACTION_STOP = "com.lanmic.audio.STOP"

        const val EXTRA_HOST = "host"
        const val EXTRA_PORT = "port"
        const val EXTRA_PACKET_FRAMES = "packetFrames"
        const val EXTRA_INPUT_PRESET = "inputPreset"
        const val EXTRA_JITTER_MS = "jitterMs"
        const val EXTRA_SERVER_NAME = "serverName"

        private const val CHANNEL_ID = "lanmic"
        private const val NOTIF_ID = 42
        private const val TAG = "lanmic-svc"

        fun startTransmitter(
            ctx: Context, host: String, port: Int, packetFrames: Int, inputPreset: Int
        ) {
            val i = Intent(ctx, AudioService::class.java).apply {
                action = ACTION_START_TX
                putExtra(EXTRA_HOST, host)
                putExtra(EXTRA_PORT, port)
                putExtra(EXTRA_PACKET_FRAMES, packetFrames)
                putExtra(EXTRA_INPUT_PRESET, inputPreset)
            }
            ctx.startForegroundService(i)
        }

        fun startServer(ctx: Context, port: Int, jitterMs: Int, name: String) {
            val i = Intent(ctx, AudioService::class.java).apply {
                action = ACTION_START_SERVER
                putExtra(EXTRA_PORT, port)
                putExtra(EXTRA_JITTER_MS, jitterMs)
                putExtra(EXTRA_SERVER_NAME, name)
            }
            ctx.startForegroundService(i)
        }

        fun stop(ctx: Context) {
            ctx.startService(
                Intent(ctx, AudioService::class.java).apply { action = ACTION_STOP }
            )
        }
    }

    // Opening an AAudio stream takes tens of milliseconds and closing one, plus
    // joining the network threads, takes longer still. Every engine and lock
    // operation therefore runs on this one executor - which also means the
    // fields below are touched by exactly one thread and need no locking of
    // their own. Nothing here may be called from the main thread.
    private val worker = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())
    private var wifiLock: WifiManager.WifiLock? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private var responder: Discovery.Responder? = null

    // Bumped on every start and every stop. A start that was still queued when
    // the stop arrived sees the change and abandons itself, instead of bringing
    // the engine up again behind a service that is already going away.
    private val generation = AtomicInteger(0)

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        createChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_START_TX -> {
                val host = intent.getStringExtra(EXTRA_HOST).orEmpty()
                val port = intent.getIntExtra(EXTRA_PORT, NativeAudio.DEFAULT_PORT)
                val frames = intent.getIntExtra(EXTRA_PACKET_FRAMES, 240)
                val preset = intent.getIntExtra(EXTRA_INPUT_PRESET, 0)
                goForeground("Transmitting to $host:$port", microphone = true)
                val gen = generation.incrementAndGet()
                worker.execute { startTx(gen, host, port, frames, preset) }
            }

            ACTION_START_SERVER -> {
                val port = intent.getIntExtra(EXTRA_PORT, NativeAudio.DEFAULT_PORT)
                val jitter = intent.getIntExtra(EXTRA_JITTER_MS, 15)
                val name = intent.getStringExtra(EXTRA_SERVER_NAME) ?: Build.MODEL
                goForeground("Mixing on udp/$port", microphone = false)
                val gen = generation.incrementAndGet()
                worker.execute { startServer(gen, port, jitter, name) }
            }

            ACTION_STOP -> stopEverything()
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        // Deliberately does not tear the engine down inline: closing the audio
        // streams and joining the network and discovery threads can take the
        // better part of a second, and this runs on the main thread.
        generation.incrementAndGet()
        worker.execute { stopEngines() }
        worker.shutdown()
        super.onDestroy()
    }

    /** Tears the engine down off the main thread, then retires the service. */
    private fun stopEverything() {
        generation.incrementAndGet()
        worker.execute { stopEngines() }
        main.post {
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
        }
    }

    // ---- worker thread only ----

    private fun startTx(gen: Int, host: String, port: Int, frames: Int, preset: Int) {
        if (gen != generation.get()) return
        acquireLocks()
        if (NativeAudio.startTransmitter(host, port, frames, preset)) {
            // A stop that arrived while the stream was opening has to win, or
            // the microphone stays live under a notification that is gone.
            if (gen != generation.get()) stopEngines()
            return
        }
        Log.e(TAG, "transmitter failed to start")
        stopEverything()
    }

    private fun startServer(gen: Int, port: Int, jitter: Int, name: String) {
        if (gen != generation.get()) return
        acquireLocks()
        if (!NativeAudio.startServer(port, jitter)) {
            Log.e(TAG, "server failed to start")
            stopEverything()
            return
        }
        if (gen != generation.get()) {
            stopEngines()
            return
        }
        responder = Discovery.Responder(this, name).also { it.start() }
        if (gen != generation.get()) stopEngines()
    }

    private fun stopEngines() {
        responder?.stop()
        responder = null
        NativeAudio.stopTransmitter()
        NativeAudio.stopServer()
        releaseLocks()
    }

    private fun goForeground(text: String, microphone: Boolean) {
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        )
        val stop = PendingIntent.getService(
            this, 1,
            Intent(this, AudioService::class.java).apply { action = ACTION_STOP },
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        )
        val n: Notification = Notification.Builder(this, CHANNEL_ID)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(text)
            .setSmallIcon(android.R.drawable.presence_audio_online)
            .setContentIntent(open)
            .setOngoing(true)
            .addAction(
                Notification.Action.Builder(null as android.graphics.drawable.Icon?, "Stop", stop)
                    .build()
            )
            .build()

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            val type = if (microphone) {
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
            } else {
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
            }
            startForeground(NOTIF_ID, n, type)
        } else {
            startForeground(NOTIF_ID, n)
        }
    }

    @Suppress("DEPRECATION")
    private fun acquireLocks() {
        if (wifiLock == null) {
            val wm = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
            val lockType = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                WifiManager.WIFI_MODE_FULL_LOW_LATENCY
            } else {
                WifiManager.WIFI_MODE_FULL_HIGH_PERF
            }
            wifiLock = wm.createWifiLock(lockType, "lanmic:wifi").apply {
                setReferenceCounted(false)
                acquire()
            }
        }
        if (wakeLock == null) {
            val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
            wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "lanmic:cpu").apply {
                setReferenceCounted(false)
                acquire()
            }
        }
    }

    private fun releaseLocks() {
        wifiLock?.let { if (it.isHeld) it.release() }
        wifiLock = null
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
    }

    private fun createChannel() {
        val nm = getSystemService(NotificationManager::class.java)
        val ch = NotificationChannel(
            CHANNEL_ID, "Live audio", NotificationManager.IMPORTANCE_LOW
        ).apply {
            description = "Shown while capturing or mixing live audio"
            setShowBadge(false)
        }
        nm.createNotificationChannel(ch)
    }
}
