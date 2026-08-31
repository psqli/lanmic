package com.lanmic.audio

import android.content.Context
import android.net.wifi.WifiManager
import android.util.Log
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.net.SocketTimeoutException
import java.nio.charset.StandardCharsets

/**
 * LAU1 discovery. Kept in Kotlin on purpose: it is not on the audio path, and
 * broadcast sockets are far less painful here than in NDK land.
 */
object Discovery {

    private const val TAG = "lanmic-disco"
    private const val HEADER = 20

    private fun header(type: Int, ssrc: Int = 0): ByteArray {
        val b = ByteArray(HEADER)
        b[0] = 'L'.code.toByte(); b[1] = 'A'.code.toByte()
        b[2] = 'U'.code.toByte(); b[3] = '1'.code.toByte()
        b[4] = type.toByte()
        b[5] = 1
        b[8] = (ssrc and 0xFF).toByte()
        b[9] = ((ssrc shr 8) and 0xFF).toByte()
        b[10] = ((ssrc shr 16) and 0xFF).toByte()
        b[11] = ((ssrc ushr 24) and 0xFF).toByte()
        return b
    }

    private fun isLau(b: ByteArray, len: Int) =
        len >= HEADER && b[0] == 'L'.code.toByte() && b[1] == 'A'.code.toByte() &&
            b[2] == 'U'.code.toByte() && b[3] == '1'.code.toByte()

    data class Found(val address: String, val name: String)

    /** Broadcasts DISCOVER and collects ANNOUNCE replies for [timeoutMs]. */
    suspend fun findServers(timeoutMs: Int = 900): List<Found> = withContext(Dispatchers.IO) {
        val found = LinkedHashMap<String, Found>()
        try {
            DatagramSocket().use { sock ->
                sock.broadcast = true
                sock.soTimeout = 200
                val probe = header(3)
                val bcast = InetAddress.getByName("255.255.255.255")
                repeat(3) {
                    try {
                        sock.send(
                            DatagramPacket(
                                probe, probe.size, bcast, NativeAudio.DISCOVERY_PORT
                            )
                        )
                    } catch (e: Exception) {
                        Log.w(TAG, "broadcast failed: ${e.message}")
                    }
                }
                val deadline = System.currentTimeMillis() + timeoutMs
                val buf = ByteArray(256)
                while (System.currentTimeMillis() < deadline) {
                    val pkt = DatagramPacket(buf, buf.size)
                    try {
                        sock.receive(pkt)
                    } catch (e: SocketTimeoutException) {
                        continue
                    }
                    if (!isLau(pkt.data, pkt.length) || pkt.data[4].toInt() != 4) continue
                    val name = if (pkt.length > HEADER) {
                        String(pkt.data, HEADER, pkt.length - HEADER, StandardCharsets.UTF_8)
                    } else {
                        "server"
                    }
                    val ip = pkt.address.hostAddress ?: continue
                    found[ip] = Found(ip, name)
                }
            }
        } catch (e: Exception) {
            Log.w(TAG, "discovery failed: ${e.message}")
        }
        found.values.toList()
    }

    /**
     * Answers DISCOVER probes while the server is running.
     *
     * Holds a [WifiManager.MulticastLock] for its lifetime: Android drops
     * broadcast and multicast datagrams that are not addressed to the device
     * before they reach the socket unless one is held, which makes the mixer
     * undiscoverable with the screen off.
     */
    class Responder(context: Context, private val name: String) {
        private val appContext = context.applicationContext

        @Volatile private var running = false
        private var socket: DatagramSocket? = null
        private var thread: Thread? = null
        private var multicastLock: WifiManager.MulticastLock? = null

        fun start() {
            if (running) return
            running = true
            acquireMulticastLock()
            thread = Thread({
                try {
                    DatagramSocket(NativeAudio.DISCOVERY_PORT).use { sock ->
                        socket = sock
                        sock.broadcast = true
                        sock.soTimeout = 500
                        val payload = name.toByteArray(StandardCharsets.UTF_8)
                        val reply = header(4) + payload
                        val buf = ByteArray(256)
                        while (running) {
                            val pkt = DatagramPacket(buf, buf.size)
                            try {
                                sock.receive(pkt)
                            } catch (e: SocketTimeoutException) {
                                continue
                            }
                            if (!isLau(pkt.data, pkt.length)) continue
                            if (pkt.data[4].toInt() != 3) continue
                            sock.send(
                                DatagramPacket(reply, reply.size, pkt.address, pkt.port)
                            )
                        }
                    }
                } catch (e: Exception) {
                    Log.w(TAG, "responder stopped: ${e.message}")
                } finally {
                    socket = null
                }
            }, "lau-discovery").also { it.isDaemon = true; it.start() }
        }

        fun stop() {
            running = false
            socket?.close()
            thread?.join(1000)
            thread = null
            multicastLock?.let { if (it.isHeld) it.release() }
            multicastLock = null
        }

        private fun acquireMulticastLock() {
            try {
                val wm = appContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
                multicastLock = wm.createMulticastLock("lanmic:discovery").apply {
                    setReferenceCounted(false)
                    acquire()
                }
            } catch (e: Exception) {
                // Discovery still works on networks that do not filter; manual
                // address entry works regardless.
                Log.w(TAG, "multicast lock unavailable: ${e.message}")
            }
        }
    }
}
