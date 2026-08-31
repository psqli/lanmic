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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import kotlin.math.log10

private val MeterStops = arrayOf(
    0.00f to Color(0xFF22C55E),
    0.70f to Color(0xFF22C55E),
    0.86f to Color(0xFFFACC15),
    1.00f to Color(0xFFEF4444)
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
        drawRoundRect(color = Color(0xFF1F2933), cornerRadius = r)
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
        Text(label, style = MaterialTheme.typography.bodySmall, color = Color(0xFF94A3B8))
        Text(
            value,
            style = MaterialTheme.typography.bodySmall,
            color = if (warn) Color(0xFFF87171) else Color(0xFFE2E8F0)
        )
    }
}
