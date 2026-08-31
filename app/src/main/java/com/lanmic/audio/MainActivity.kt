package com.lanmic.audio

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import kotlinx.coroutines.delay

/** How often every screen re-reads the native counters. */
const val UI_POLL_MS = 80L

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme(colorScheme = LanMicColors) {
                Surface(Modifier.fillMaxSize(), color = Palette.Background) { AppRoot() }
            }
        }
    }
}

enum class Mode { MIC, SERVER }

@Composable
private fun AppRoot() {
    val ctx = LocalContext.current
    val settings = remember { Settings.of(ctx) }
    var mode by remember { mutableStateOf(Mode.MIC) }

    // The engine can be started from the notification as well as from here, so
    // the mode toggle follows the engine rather than the other way round.
    var running by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) {
        while (true) {
            running = NativeAudio.isTransmitting() || NativeAudio.isServing()
            delay(150)
        }
    }

    RequestNotificationPermission()

    Column(
        Modifier
            .fillMaxSize()
            .padding(horizontal = 16.dp)
            .verticalScroll(rememberScrollState())
    ) {
        Spacer(Modifier.height(24.dp))
        Text("LAN Mic", fontSize = 26.sp, fontWeight = FontWeight.Bold, color = Palette.TextPrimary)
        Text(
            "48 kHz PCM over UDP - no codec, no buffering games",
            fontSize = 12.sp,
            color = Palette.TextFaint
        )
        Spacer(Modifier.height(16.dp))

        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
            Mode.entries.forEachIndexed { i, m ->
                SegmentedButton(
                    selected = mode == m,
                    onClick = { if (!running) mode = m },
                    shape = SegmentedButtonDefaults.itemShape(i, Mode.entries.size),
                    enabled = !running
                ) { Text(if (m == Mode.MIC) "Microphone" else "Mixer / server") }
            }
        }
        Spacer(Modifier.height(16.dp))

        when (mode) {
            Mode.MIC -> MicScreen(settings)
            Mode.SERVER -> ServerScreen(settings)
        }
        Spacer(Modifier.height(32.dp))
    }
}

/** Asked once on launch; the foreground service is useless without it. */
@Composable
private fun RequestNotificationPermission() {
    val ctx = LocalContext.current
    val launcher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) {}
    LaunchedEffect(Unit) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(ctx, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            launcher.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }
}
