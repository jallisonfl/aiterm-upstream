package com.adroited.aiterm.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight

private val Harbor = Color(0xFF17324D)
private val Signal = Color(0xFF0B7285)
private val Trust = Color(0xFF2E7D6B)
private val Alert = Color(0xFFBA3E45)
private val Frost = Color(0xFFF5F8FA)
private val Night = Color(0xFF09131F)

private val LightColors = lightColorScheme(
    primary = Signal,
    onPrimary = Color.White,
    secondary = Harbor,
    tertiary = Trust,
    error = Alert,
    background = Frost,
    surface = Frost,
    surfaceVariant = Color(0xFFDFE7EC),
)

private val DarkColors = darkColorScheme(
    primary = Color(0xFF63D3E1),
    onPrimary = Color(0xFF00363D),
    secondary = Color(0xFFAFC8E0),
    tertiary = Color(0xFF75D8B4),
    error = Color(0xFFFFB3B6),
    background = Night,
    surface = Color(0xFF0E1B29),
    surfaceVariant = Color(0xFF263442),
)

private val BaseTypography = Typography()
private val AitermTypography = Typography(
    headlineMedium = BaseTypography.headlineMedium.copy(
        fontFamily = FontFamily.SansSerif,
        fontWeight = FontWeight.SemiBold,
    ),
    headlineSmall = BaseTypography.headlineSmall.copy(
        fontFamily = FontFamily.SansSerif,
        fontWeight = FontWeight.SemiBold,
    ),
    bodyLarge = BaseTypography.bodyLarge.copy(fontFamily = FontFamily.SansSerif),
    bodyMedium = BaseTypography.bodyMedium.copy(fontFamily = FontFamily.SansSerif),
    labelLarge = BaseTypography.labelLarge.copy(fontFamily = FontFamily.SansSerif),
    labelMedium = BaseTypography.labelMedium.copy(
        fontFamily = FontFamily.Monospace,
        fontWeight = FontWeight.Medium,
    ),
)

@Composable
fun AitermTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit,
) {
    MaterialTheme(
        colorScheme = if (darkTheme) DarkColors else LightColors,
        typography = AitermTypography,
        content = content,
    )
}
