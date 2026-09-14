package com.lanmic.audio

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlin.math.log10

// Green until it is loud, yellow approaching the ceiling, red at it.
private val MeterStops = arrayOf(
    0.00f to Palette.Ok,
    0.70f to Palette.Ok,
    0.86f to Palette.Warning,
    1.00f to Palette.Danger
)

/** Peak meter on a dBFS scale (-60..0 dB), which is how ears and mixers think. */
@Composable
fun LevelMeter(peak: Float, modifier: Modifier = Modifier, heightDp: Int = 10) {
    val db = if (peak <= 0.0001f) -60f else (20f * log10(peak)).coerceIn(-60f, 0f)
    val frac = ((db + 60f) / 60f).coerceIn(0f, 1f)
    Canvas(
        modifier
            .fillMaxWidth()
            .height(heightDp.dp)
    ) {
        val r = CornerRadius(size.height / 2f, size.height / 2f)
        drawRoundRect(color = Palette.MeterTrack, cornerRadius = r)
        if (frac > 0.001f) {
            drawRoundRect(
                brush = Brush.horizontalGradient(
                    *MeterStops, startX = 0f, endX = size.width
                ),
                size = Size(size.width * frac, size.height),
                cornerRadius = r
            )
        }
    }
}

@Composable
fun StatRow(label: String, value: String, warn: Boolean = false) {
    Row(
        Modifier
            .fillMaxWidth()
            .padding(vertical = 2.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically
    ) {
        Text(label, style = MaterialTheme.typography.bodySmall, color = Palette.TextMuted)
        Text(
            value,
            style = MaterialTheme.typography.bodySmall,
            color = if (warn) Palette.Alert else Palette.TextBody
        )
    }
}

/**
 * The device menu, drawn inline. A list beats a dropdown for something chosen
 * once at the start of a gig and then looked at to check it is still right.
 *
 * "Automatic" is always the first row and is what an unrecognised choice falls
 * back to, so the list can never leave nothing selected - including when the
 * remembered device is in the other bag.
 */
@Composable
fun DevicePicker(
    endpoints: List<AudioEndpoint>,
    selectedKey: String,
    enabled: Boolean,
    onSelect: (String) -> Unit
) {
    val known = endpoints.any { it.key == selectedKey }
    Column(Modifier.fillMaxWidth()) {
        DeviceRow("Automatic", selectedKey == AudioDevices.AUTOMATIC || !known, enabled) {
            onSelect(AudioDevices.AUTOMATIC)
        }
        endpoints.forEach { endpoint ->
            Spacer(Modifier.height(6.dp))
            DeviceRow(endpoint.label, endpoint.key == selectedKey, enabled) {
                onSelect(endpoint.key)
            }
        }
        if (selectedKey != AudioDevices.AUTOMATIC && !known) {
            Spacer(Modifier.height(6.dp))
            Text(
                "The device you chose is not plugged in; Android is picking one.",
                fontSize = 11.sp,
                color = Palette.Warning
            )
        }
    }
}

@Composable
private fun DeviceRow(label: String, selected: Boolean, enabled: Boolean, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .background(
                if (selected) Palette.MeterTrack else Palette.Card,
                RoundedCornerShape(10.dp)
            )
            .border(
                1.dp,
                if (selected) Palette.Accent else Palette.Divider,
                RoundedCornerShape(10.dp)
            )
            .clickable(enabled = enabled, onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Text(
            label,
            fontSize = 13.sp,
            color = when {
                !enabled -> Palette.TextFaint
                selected -> Palette.TextPrimary
                else -> Palette.TextBody
            },
            modifier = Modifier.weight(1f)
        )
        if (selected) {
            Text("selected", fontSize = 11.sp, color = Palette.Accent)
        }
    }
}
