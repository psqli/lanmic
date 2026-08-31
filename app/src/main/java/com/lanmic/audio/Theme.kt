package com.lanmic.audio

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The app is dark-only by design: it is used in blacked-out rooms next to a
 * mixing desk, where a white screen is genuinely unwelcome.
 */
object Palette {
    val Background = Color(0xFF101418)
    val Card = Color(0xFF181F26)
    val Accent = Color(0xFF4ADE80)
    val OnAccent = Color(0xFF07130B)

    val TextPrimary = Color(0xFFF1F5F9)
    val TextBody = Color(0xFFE2E8F0)
    val TextMuted = Color(0xFF94A3B8)
    val TextFaint = Color(0xFF64748B)

    val Divider = Color(0xFF223040)
    val Danger = Color(0xFFEF4444)
    val Warning = Color(0xFFFACC15)
    val Alert = Color(0xFFF87171)
    val Ok = Color(0xFF22C55E)
    val MeterTrack = Color(0xFF1F2933)
}

val LanMicColors = darkColorScheme(
    primary = Palette.Accent,
    onPrimary = Palette.OnAccent,
    background = Palette.Background,
    surface = Palette.Card,
    onSurface = Palette.TextBody,
    surfaceVariant = Color(0xFF223040),
    onSurfaceVariant = Color(0xFFCBD5E1)
)

/** A titled card. Every section of both screens is one of these. */
@Composable
fun Panel(title: String, content: @Composable ColumnScope.() -> Unit) {
    Column(
        Modifier
            .fillMaxWidth()
            .background(Palette.Card, RoundedCornerShape(14.dp))
            .padding(14.dp)
    ) {
        Text(
            title.uppercase(),
            fontSize = 11.sp,
            letterSpacing = 1.sp,
            color = Palette.TextFaint,
            fontWeight = FontWeight.SemiBold
        )
        Spacer(Modifier.height(10.dp))
        content()
    }
}
