package com.lanmic.audio

import java.net.Inet4Address
import java.net.NetworkInterface

/**
 * IPv4 addresses a microphone could be pointed at. Every up, non-loopback
 * interface is listed rather than guessed between: on a phone that is usually
 * Wi-Fi plus, sometimes, a mobile interface, and only the operator can tell
 * which one the mixer is on.
 */
fun localIpv4Addresses(): List<String> = try {
    NetworkInterface.getNetworkInterfaces().toList()
        .filter { it.isUp && !it.isLoopback }
        .flatMap { it.inetAddresses.toList() }
        .filterIsInstance<Inet4Address>()
        .mapNotNull { it.hostAddress }
} catch (e: Exception) {
    emptyList()
}
