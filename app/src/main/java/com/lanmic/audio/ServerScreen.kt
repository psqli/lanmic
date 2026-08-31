package com.lanmic.audio

import android.os.Build
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import kotlin.math.log10

/** Buffer depth beyond which a source strip flags itself in the UI. */
private const val BUFFER_WARN_MS = 80

@Composable
fun ServerScreen(settings: Settings) {
    val ctx = LocalContext.current

    var port by remember { mutableStateOf(settings.port.toString()) }
    var jitterMs by remember { mutableIntStateOf(settings.jitterMs) }
    var master by remember { mutableFloatStateOf(settings.masterGain) }
    var stats by remember { mutableStateOf(RxStats()) }
    var sources by remember { mutableStateOf<List<SourceInfo>>(emptyList()) }
    val addresses = remember { localIpv4Addresses() }

    LaunchedEffect(Unit) {
        while (true) {
            stats = NativeAudio.serverStats()
            sources = NativeAudio.serverSources()
            delay(UI_POLL_MS)
        }
    }

    Panel("Listening on") {
        if (addresses.isEmpty()) {
            Text("No network interface", fontSize = 13.sp, color = Palette.Alert)
        } else {
            addresses.forEach {
                Text(
                    "$it : $port",
                    fontFamily = FontFamily.Monospace,
                    fontSize = 16.sp,
                    color = Palette.Accent
                )
            }
        }
        Spacer(Modifier.height(8.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = port,
                onValueChange = {
                    port = it.filter { c -> c.isDigit() }.take(5)
                    port.toIntOrNull()?.let { p -> settings.port = p }
                },
                label = { Text("Port") },
                singleLine = true,
                enabled = !stats.running,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                modifier = Modifier.width(130.dp)
            )
        }
    }

    Spacer(Modifier.height(12.dp))

    Panel("Buffering") {
        Text("Jitter buffer  $jitterMs ms", fontSize = 12.sp, color = Palette.TextMuted)
        Slider(
            value = jitterMs.toFloat(),
            onValueChange = { jitterMs = it.toInt(); settings.jitterMs = jitterMs },
            valueRange = 5f..60f,
            steps = 10,
            enabled = !stats.running
        )
        Text(
            "Lower is tighter; raise it if you hear dropouts. Takes effect on start.",
            fontSize = 11.sp,
            color = Palette.TextFaint
        )
        Spacer(Modifier.height(10.dp))
        Text("Master  ${"%.1f".format(master)}x", fontSize = 12.sp, color = Palette.TextMuted)
        Slider(
            value = master,
            onValueChange = { master = it; NativeAudio.setMasterGain(it); settings.masterGain = it },
            valueRange = 0f..4f
        )
    }

    Spacer(Modifier.height(12.dp))

    Panel("Mix bus") {
        LevelMeter(stats.masterPeak, heightDp = 14)
        Spacer(Modifier.height(10.dp))
        StatRow("Output latency", "%.1f ms".format(stats.latencyMs))
        StatRow("Sources", stats.activeSources.toString())
        StatRow("Packets received", stats.packets.toString())
        StatRow("Malformed", stats.badPackets.toString(), stats.badPackets > 0)
        StatRow("Audio glitches", stats.xruns.toString(), stats.xruns > 0)
        StatRow("Limiter", limiterLabel(stats.limiterGain), stats.limiterGain < 0.9f)
    }

    Spacer(Modifier.height(12.dp))

    Panel("Microphones (${sources.size})") {
        if (sources.isEmpty()) {
            Text(
                if (stats.running) "Waiting for microphones..." else "Server stopped",
                fontSize = 13.sp,
                color = Palette.TextFaint
            )
        }
        sources.forEach { SourceRow(it) }
    }

    Spacer(Modifier.height(16.dp))

    Button(
        onClick = {
            if (stats.running) {
                AudioService.stop(ctx)
            } else {
                AudioService.startServer(
                    ctx, port.toIntOrNull() ?: NativeAudio.DEFAULT_PORT, jitterMs, Build.MODEL
                )
            }
        },
        modifier = Modifier
            .fillMaxWidth()
            .height(54.dp),
        colors = ButtonDefaults.buttonColors(
            containerColor = if (stats.running) Palette.Danger else Palette.Accent,
            contentColor = Palette.OnAccent
        )
    ) {
        Text(if (stats.running) "STOP SERVER" else "START SERVER", fontWeight = FontWeight.Bold)
    }
}

/** "open" while the limiter is idle, otherwise how much it is pulling down. */
private fun limiterLabel(gain: Float): String =
    if (gain > 0.99f) "open" else "-%.1f dB".format(-20f * log10(gain.coerceAtLeast(0.001f)))

/** One microphone's channel strip: level, buffer depth, gain, mute, counters. */
@Composable
private fun SourceRow(s: SourceInfo) {
    var gain by remember(s.ssrc) { mutableFloatStateOf(s.gain) }
    Column(Modifier.padding(vertical = 8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                s.label,
                fontFamily = FontFamily.Monospace,
                fontSize = 13.sp,
                color = Palette.TextBody,
                modifier = Modifier.weight(1f)
            )
            Text(
                "${s.bufferMs} ms buf",
                fontSize = 11.sp,
                color = if (s.bufferMs > BUFFER_WARN_MS) Palette.Alert else Palette.TextFaint
            )
            Spacer(Modifier.width(10.dp))
            TextButton(onClick = { NativeAudio.setSourceMuted(s.ssrc, !s.muted) }) {
                Text(if (s.muted) "unmute" else "mute", fontSize = 12.sp)
            }
        }
        LevelMeter(s.peak)
        Slider(
            value = gain,
            onValueChange = { gain = it; NativeAudio.setSourceGain(s.ssrc, it) },
            valueRange = 0f..2f
        )
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Counter("pkts", s.packets, warn = false)
            Counter("lost", s.lost, warn = s.lost > 0)
            Counter("under", s.underruns, warn = s.underruns > 0, warnColor = Palette.Warning)
        }
    }
    HorizontalDivider(color = Palette.Divider)
}

@Composable
private fun Counter(label: String, value: Long, warn: Boolean, warnColor: Color = Palette.Alert) {
    Text(
        "$label $value",
        fontSize = 10.sp,
        color = if (warn) warnColor else Palette.TextFaint
    )
}
