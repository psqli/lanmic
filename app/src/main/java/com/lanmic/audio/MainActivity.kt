package com.lanmic.audio

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import java.net.Inet4Address
import java.net.NetworkInterface

private val Bg = Color(0xFF101418)
private val CardBg = Color(0xFF181F26)
private val Accent = Color(0xFF4ADE80)

private val DarkScheme = darkColorScheme(
    primary = Accent,
    onPrimary = Color(0xFF07130B),
    background = Bg,
    surface = CardBg,
    onSurface = Color(0xFFE2E8F0),
    surfaceVariant = Color(0xFF223040),
    onSurfaceVariant = Color(0xFFCBD5E1)
)

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme(colorScheme = DarkScheme) {
                Surface(Modifier.fillMaxSize(), color = Bg) { AppRoot() }
            }
        }
    }
}

private enum class Mode { MIC, SERVER }

@Composable
private fun AppRoot() {
    val ctx = LocalContext.current
    val prefs = remember { ctx.getSharedPreferences("lanmic", Context.MODE_PRIVATE) }
    var mode by remember { mutableStateOf(Mode.MIC) }

    val notifPermission = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) {}
    LaunchedEffect(Unit) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(ctx, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            notifPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    Column(
        Modifier
            .fillMaxSize()
            .padding(horizontal = 16.dp)
            .verticalScroll(rememberScrollState())
    ) {
        Spacer(Modifier.height(24.dp))
        Text(
            "LAN Mic",
            fontSize = 26.sp,
            fontWeight = FontWeight.Bold,
            color = Color(0xFFF1F5F9)
        )
        Text(
            "48 kHz PCM over UDP - no codec, no buffering games",
            fontSize = 12.sp,
            color = Color(0xFF64748B)
        )
        Spacer(Modifier.height(16.dp))

        var running by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) {
        while (true) {
            running = NativeAudio.isTransmitting() || NativeAudio.isServing()
            delay(150)
        }
    }
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
            SegmentedButton(
                selected = mode == Mode.MIC,
                onClick = { if (!running) mode = Mode.MIC },
                shape = SegmentedButtonDefaults.itemShape(0, 2),
                enabled = !running
            ) { Text("Microphone") }
            SegmentedButton(
                selected = mode == Mode.SERVER,
                onClick = { if (!running) mode = Mode.SERVER },
                shape = SegmentedButtonDefaults.itemShape(1, 2),
                enabled = !running
            ) { Text("Mixer / server") }
        }
        Spacer(Modifier.height(16.dp))

        when (mode) {
            Mode.MIC -> MicScreen(prefs)
            Mode.SERVER -> ServerScreen(prefs)
        }
        Spacer(Modifier.height(32.dp))
    }
}

@Composable
private fun Panel(title: String, content: @Composable ColumnScope.() -> Unit) {
    Column(
        Modifier
            .fillMaxWidth()
            .background(CardBg, RoundedCornerShape(14.dp))
            .padding(14.dp)
    ) {
        Text(
            title.uppercase(),
            fontSize = 11.sp,
            letterSpacing = 1.sp,
            color = Color(0xFF64748B),
            fontWeight = FontWeight.SemiBold
        )
        Spacer(Modifier.height(10.dp))
        content()
    }
}

/* ------------------------------------------------------------------ */
/* Microphone / transmitter                                            */
/* ------------------------------------------------------------------ */

@Composable
private fun MicScreen(prefs: android.content.SharedPreferences) {
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    var host by remember { mutableStateOf(prefs.getString("host", "") ?: "") }
    var port by remember { mutableStateOf(prefs.getInt("port", NativeAudio.DEFAULT_PORT).toString()) }
    var packetFrames by remember { mutableIntStateOf(prefs.getInt("packetFrames", 240)) }
    var preset by remember { mutableIntStateOf(prefs.getInt("preset", 0)) }
    var gain by remember { mutableFloatStateOf(prefs.getFloat("txGain", 1f)) }
    var muted by remember { mutableStateOf(false) }
    var scanning by remember { mutableStateOf(false) }
    var found by remember { mutableStateOf<List<Discovery.Found>>(emptyList()) }
    var stats by remember { mutableStateOf(TxStats()) }

    val micPermission = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { granted ->
        if (granted) {
            AudioService.startTransmitter(
                ctx, host.trim(), port.toIntOrNull() ?: NativeAudio.DEFAULT_PORT,
                packetFrames, preset
            )
        }
    }

    LaunchedEffect(Unit) {
        while (true) {
            stats = NativeAudio.txStats()
            delay(80)
        }
    }

    Panel("Server") {
        Row(verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = host,
                onValueChange = { host = it; prefs.edit().putString("host", it).apply() },
                label = { Text("Address") },
                singleLine = true,
                enabled = !stats.running,
                modifier = Modifier.weight(1f)
            )
            Spacer(Modifier.width(8.dp))
            OutlinedTextField(
                value = port,
                onValueChange = { port = it.filter { c -> c.isDigit() }.take(5) },
                label = { Text("Port") },
                singleLine = true,
                enabled = !stats.running,
                keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                    keyboardType = KeyboardType.Number
                ),
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
                        if (found.size == 1) {
                            host = found[0].address
                            prefs.edit().putString("host", host).apply()
                        }
                        scanning = false
                    }
                },
                enabled = !scanning && !stats.running
            ) { Text(if (scanning) "Scanning..." else "Find server") }
            Spacer(Modifier.width(10.dp))
            if (found.size > 1) {
                Text("${found.size} found", fontSize = 12.sp, color = Color(0xFF94A3B8))
            }
        }
        found.forEach { f ->
            TextButton(onClick = {
                host = f.address
                prefs.edit().putString("host", host).apply()
            }) { Text("${f.name}  ${f.address}", fontSize = 12.sp) }
        }
    }

    Spacer(Modifier.height(12.dp))

    Panel("Capture") {
        Text("Packet size", fontSize = 12.sp, color = Color(0xFF94A3B8))
        Spacer(Modifier.height(6.dp))
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
            listOf(120 to "2.5 ms", 240 to "5 ms", 480 to "10 ms").forEachIndexed { i, (fr, label) ->
                SegmentedButton(
                    selected = packetFrames == fr,
                    onClick = {
                        packetFrames = fr
                        prefs.edit().putInt("packetFrames", fr).apply()
                    },
                    shape = SegmentedButtonDefaults.itemShape(i, 3),
                    enabled = !stats.running
                ) { Text(label, fontSize = 12.sp) }
            }
        }
        Spacer(Modifier.height(10.dp))
        Text("Input processing", fontSize = 12.sp, color = Color(0xFF94A3B8))
        Spacer(Modifier.height(6.dp))
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
            listOf("Voice perf", "Raw", "Voice rec").forEachIndexed { i, label ->
                SegmentedButton(
                    selected = preset == i,
                    onClick = { preset = i; prefs.edit().putInt("preset", i).apply() },
                    shape = SegmentedButtonDefaults.itemShape(i, 3),
                    enabled = !stats.running
                ) { Text(label, fontSize = 12.sp) }
            }
        }
        Spacer(Modifier.height(12.dp))
        Text("Gain  ${"%.1f".format(gain)}x", fontSize = 12.sp, color = Color(0xFF94A3B8))
        Slider(
            value = gain,
            onValueChange = {
                gain = it
                NativeAudio.setTxGain(it)
                prefs.edit().putFloat("txGain", it).apply()
            },
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
                if (stats.running) {
                    AudioService.stop(ctx)
                } else {
                    if (ContextCompat.checkSelfPermission(ctx, Manifest.permission.RECORD_AUDIO)
                        == PackageManager.PERMISSION_GRANTED
                    ) {
                        AudioService.startTransmitter(
                            ctx, host.trim(), port.toIntOrNull() ?: NativeAudio.DEFAULT_PORT,
                            packetFrames, preset
                        )
                    } else {
                        micPermission.launch(Manifest.permission.RECORD_AUDIO)
                    }
                }
            },
            enabled = host.isNotBlank(),
            modifier = Modifier
                .weight(1f)
                .height(54.dp),
            colors = ButtonDefaults.buttonColors(
                containerColor = if (stats.running) Color(0xFFEF4444) else Accent,
                contentColor = Color(0xFF07130B)
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

/* ------------------------------------------------------------------ */
/* Mixer / server                                                      */
/* ------------------------------------------------------------------ */

@Composable
private fun ServerScreen(prefs: android.content.SharedPreferences) {
    val ctx = LocalContext.current

    var port by remember { mutableStateOf(prefs.getInt("port", NativeAudio.DEFAULT_PORT).toString()) }
    var jitterMs by remember { mutableIntStateOf(prefs.getInt("jitterMs", 15)) }
    var master by remember { mutableFloatStateOf(prefs.getFloat("master", 1f)) }
    var stats by remember { mutableStateOf(RxStats()) }
    var sources by remember { mutableStateOf<List<SourceInfo>>(emptyList()) }
    val ips = remember { localIpv4Addresses() }

    LaunchedEffect(Unit) {
        while (true) {
            stats = NativeAudio.serverStats()
            sources = NativeAudio.serverSources()
            delay(80)
        }
    }

    Panel("Listening on") {
        if (ips.isEmpty()) {
            Text("No network interface", fontSize = 13.sp, color = Color(0xFFF87171))
        } else {
            ips.forEach {
                Text(
                    "$it : ${port}",
                    fontFamily = FontFamily.Monospace,
                    fontSize = 16.sp,
                    color = Accent
                )
            }
        }
        Spacer(Modifier.height(8.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = port,
                onValueChange = { port = it.filter { c -> c.isDigit() }.take(5) },
                label = { Text("Port") },
                singleLine = true,
                enabled = !stats.running,
                keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                    keyboardType = KeyboardType.Number
                ),
                modifier = Modifier.width(130.dp)
            )
        }
    }

    Spacer(Modifier.height(12.dp))

    Panel("Buffering") {
        Text(
            "Jitter buffer  ${jitterMs} ms",
            fontSize = 12.sp,
            color = Color(0xFF94A3B8)
        )
        Slider(
            value = jitterMs.toFloat(),
            onValueChange = {
                jitterMs = it.toInt()
                prefs.edit().putInt("jitterMs", jitterMs).apply()
            },
            valueRange = 5f..60f,
            steps = 10,
            enabled = !stats.running
        )
        Text(
            "Lower is tighter; raise it if you hear dropouts. Takes effect on start.",
            fontSize = 11.sp,
            color = Color(0xFF64748B)
        )
        Spacer(Modifier.height(10.dp))
        Text("Master  ${"%.1f".format(master)}x", fontSize = 12.sp, color = Color(0xFF94A3B8))
        Slider(
            value = master,
            onValueChange = {
                master = it
                NativeAudio.setMasterGain(it)
                prefs.edit().putFloat("master", it).apply()
            },
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
        StatRow(
            "Limiter",
            if (stats.limiterGain > 0.99f) "open" else "-%.1f dB".format(
                -20f * kotlin.math.log10(stats.limiterGain.coerceAtLeast(0.001f))
            ),
            stats.limiterGain < 0.9f
        )
    }

    Spacer(Modifier.height(12.dp))

    Panel("Microphones (${sources.size})") {
        if (sources.isEmpty()) {
            Text(
                if (stats.running) "Waiting for microphones..." else "Server stopped",
                fontSize = 13.sp,
                color = Color(0xFF64748B)
            )
        }
        sources.forEach { s -> SourceRow(s) }
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
            containerColor = if (stats.running) Color(0xFFEF4444) else Accent,
            contentColor = Color(0xFF07130B)
        )
    ) { Text(if (stats.running) "STOP SERVER" else "START SERVER", fontWeight = FontWeight.Bold) }
}

@Composable
private fun SourceRow(s: SourceInfo) {
    var gain by remember(s.ssrc) { mutableFloatStateOf(s.gain) }
    Column(Modifier.padding(vertical = 8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                s.label,
                fontFamily = FontFamily.Monospace,
                fontSize = 13.sp,
                color = Color(0xFFE2E8F0),
                modifier = Modifier.weight(1f)
            )
            Text(
                "${s.bufferMs} ms buf",
                fontSize = 11.sp,
                color = if (s.bufferMs > 80) Color(0xFFF87171) else Color(0xFF64748B)
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
            Text("pkts ${s.packets}", fontSize = 10.sp, color = Color(0xFF64748B))
            Text(
                "lost ${s.lost}",
                fontSize = 10.sp,
                color = if (s.lost > 0) Color(0xFFF87171) else Color(0xFF64748B)
            )
            Text(
                "under ${s.underruns}",
                fontSize = 10.sp,
                color = if (s.underruns > 0) Color(0xFFFACC15) else Color(0xFF64748B)
            )
        }
    }
    HorizontalDivider(color = Color(0xFF223040))
}

private fun localIpv4Addresses(): List<String> = try {
    NetworkInterface.getNetworkInterfaces().toList()
        .filter { it.isUp && !it.isLoopback }
        .flatMap { it.inetAddresses.toList() }
        .filterIsInstance<Inet4Address>()
        .mapNotNull { it.hostAddress }
} catch (e: Exception) {
    emptyList()
}
