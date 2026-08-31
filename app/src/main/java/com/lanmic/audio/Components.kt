package com.lanmic.audio

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.unit.dp
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
