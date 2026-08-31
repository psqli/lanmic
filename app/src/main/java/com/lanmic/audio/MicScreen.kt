package com.lanmic.audio

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
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
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/** frames per packet -> label. 240 frames = 5 ms at 48 kHz. */
private val PacketSizes = listOf(120 to "2.5 ms", 240 to "5 ms", 480 to "10 ms")

/** Index matches the `inputPreset` argument of the native transmitter. */
private val InputPresets = listOf("Voice perf", "Raw", "Voice rec")

@Composable
fun MicScreen(settings: Settings) {
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var host by remember { mutableStateOf(settings.host) }
    var port by remember { mutableStateOf(settings.port.toString()) }
    var packetFrames by remember { mutableIntStateOf(settings.packetFrames) }
    var preset by remember { mutableIntStateOf(settings.inputPreset) }
    var gain by remember { mutableFloatStateOf(settings.txGain) }
    var muted by remember { mutableStateOf(false) }
    var scanning by remember { mutableStateOf(false) }
    var found by remember { mutableStateOf<List<Discovery.Found>>(emptyList()) }
    var stats by remember { mutableStateOf(TxStats()) }

    fun portOrDefault() = port.toPortOrNull() ?: NativeAudio.DEFAULT_PORT

    fun goLive() =
        AudioService.startTransmitter(ctx, host.trim(), portOrDefault(), packetFrames, preset)

    fun hasMicPermission() = ContextCompat.checkSelfPermission(
        ctx, Manifest.permission.RECORD_AUDIO
    ) == PackageManager.PERMISSION_GRANTED

    val micPermission = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { granted -> if (granted) goLive() }

    LaunchedEffect(Unit) {
        var wasRunning = false
        while (true) {
            val s = NativeAudio.txStats()
            if (s.running && !wasRunning) {
                // A fresh engine comes up at unity gain and unmuted. These
                // controls were restored from preferences, so push them back
                // down: otherwise the slider reads 2.5x while the microphone is
                // actually running at 1x until someone happens to nudge it.
                muted = false
                NativeAudio.setTxGain(gain)
            }
            wasRunning = s.running
            stats = s
            delay(UI_POLL_MS)
        }
    }

    Panel("Server") {
        Row(verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = host,
                onValueChange = { host = it; settings.host = it },
                label = { Text("Address") },
                singleLine = true,
                enabled = !stats.running,
                modifier = Modifier.weight(1f)
            )
            Spacer(Modifier.width(8.dp))
            OutlinedTextField(
                value = port,
                onValueChange = {
                    port = it.filter { c -> c.isDigit() }.take(5)
                    port.toPortOrNull()?.let { p -> settings.port = p }
                },
                label = { Text("Port") },
                singleLine = true,
                enabled = !stats.running,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                modifier = Modifier.width(110.dp)
            )
        }
        Spacer(Modifier.height(8.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            OutlinedButton(
                onClick = {
                    scanning = true
                    scope.launch {
                        found = Discovery.findServers()
                        // One answer is unambiguous, so take it; several means
                        // the operator has to choose.
                        if (found.size == 1) {
                            host = found[0].address
                            settings.host = host
                        }
                        scanning = false
                    }
                },
                enabled = !scanning && !stats.running
            ) { Text(if (scanning) "Scanning..." else "Find server") }
            Spacer(Modifier.width(10.dp))
            if (found.size > 1) {
                Text("${found.size} found", fontSize = 12.sp, color = Palette.TextMuted)
            }
        }
        found.forEach { f ->
            TextButton(onClick = { host = f.address; settings.host = host }) {
                Text("${f.name}  ${f.address}", fontSize = 12.sp)
            }
        }
    }

    Spacer(Modifier.height(12.dp))

    Panel("Capture") {
        Text("Packet size", fontSize = 12.sp, color = Palette.TextMuted)
        Spacer(Modifier.height(6.dp))
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
            PacketSizes.forEachIndexed { i, (frames, label) ->
                SegmentedButton(
                    selected = packetFrames == frames,
                    onClick = { packetFrames = frames; settings.packetFrames = frames },
                    shape = SegmentedButtonDefaults.itemShape(i, PacketSizes.size),
                    enabled = !stats.running
                ) { Text(label, fontSize = 12.sp) }
            }
        }
        Spacer(Modifier.height(10.dp))
        Text("Input processing", fontSize = 12.sp, color = Palette.TextMuted)
        Spacer(Modifier.height(6.dp))
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
            InputPresets.forEachIndexed { i, label ->
                SegmentedButton(
                    selected = preset == i,
                    onClick = { preset = i; settings.inputPreset = i },
                    shape = SegmentedButtonDefaults.itemShape(i, InputPresets.size),
                    enabled = !stats.running
                ) { Text(label, fontSize = 12.sp) }
            }
        }
        Spacer(Modifier.height(12.dp))
        Text("Gain  ${"%.1f".format(gain)}x", fontSize = 12.sp, color = Palette.TextMuted)
        Slider(
            value = gain,
            onValueChange = { gain = it; NativeAudio.setTxGain(it); settings.txGain = it },
            valueRange = 0f..4f
        )
    }

    Spacer(Modifier.height(12.dp))

    Panel("Level") {
        LevelMeter(stats.peak, heightDp = 14)
        Spacer(Modifier.height(10.dp))
        StatRow("Capture latency", "%.1f ms".format(stats.latencyMs))
        StatRow("Packets sent", stats.packetsSent.toString())
        StatRow("Dropped frames", stats.framesDropped.toString(), stats.framesDropped > 0)
        StatRow("Send errors", stats.sendErrors.toString(), stats.sendErrors > 0)
        StatRow("Audio glitches", stats.xruns.toString(), stats.xruns > 0)
    }

    Spacer(Modifier.height(16.dp))

    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
        Button(
            onClick = {
                when {
                    stats.running -> AudioService.stop(ctx)
                    hasMicPermission() -> goLive()
                    else -> micPermission.launch(Manifest.permission.RECORD_AUDIO)
                }
            },
            enabled = host.isNotBlank(),
            modifier = Modifier
                .weight(1f)
                .height(54.dp),
            colors = ButtonDefaults.buttonColors(
                containerColor = if (stats.running) Palette.Danger else Palette.Accent,
                contentColor = Palette.OnAccent
            )
        ) { Text(if (stats.running) "STOP" else "GO LIVE", fontWeight = FontWeight.Bold) }

        FilledTonalButton(
            onClick = { muted = !muted; NativeAudio.setTxMuted(muted) },
            enabled = stats.running,
            modifier = Modifier
                .width(110.dp)
                .height(54.dp)
        ) { Text(if (muted) "UNMUTE" else "MUTE") }
    }
}
