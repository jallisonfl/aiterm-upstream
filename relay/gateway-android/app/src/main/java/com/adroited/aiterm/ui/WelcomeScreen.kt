package com.adroited.aiterm.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

private val TerminalNavy = Color(0xFF10283D)
private val TerminalText = Color(0xFFEAF3F6)
private val TerminalMuted = Color(0xFF91A8B7)
private val TerminalSignal = Color(0xFF72D5B5)

@Composable
fun WelcomeScreen(
    onUnlock: () -> Unit,
    unlockError: String? = null,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier,
        containerColor = MaterialTheme.colorScheme.background,
        bottomBar = {
            Surface(
                color = MaterialTheme.colorScheme.background,
                shadowElevation = 8.dp,
            ) {
                Box(
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 12.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Column(
                        modifier = Modifier.fillMaxWidth().widthIn(max = 552.dp),
                    ) {
                        Text(
                            text = "AITerm is locked",
                            color = MaterialTheme.colorScheme.onSurface,
                            style = MaterialTheme.typography.titleMedium,
                            fontWeight = FontWeight.SemiBold,
                        )
                        Text(
                            text = "Unlock with a strong biometric or your device PIN.",
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            style = MaterialTheme.typography.bodySmall,
                        )
                        unlockError?.let { error ->
                            Spacer(Modifier.height(6.dp))
                            Text(
                                text = error,
                                color = MaterialTheme.colorScheme.error,
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                        Spacer(Modifier.height(10.dp))
                        Button(
                            onClick = onUnlock,
                            modifier = Modifier.fillMaxWidth().height(54.dp),
                            shape = RoundedCornerShape(15.dp),
                            colors = ButtonDefaults.buttonColors(
                                containerColor = MaterialTheme.colorScheme.primary,
                            ),
                        ) {
                            Text("Unlock AITerm", style = MaterialTheme.typography.labelLarge)
                        }
                    }
                }
            }
        },
    ) { innerPadding ->
        Box(
            modifier = Modifier.fillMaxSize().padding(innerPadding),
            contentAlignment = Alignment.TopCenter,
        ) {
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .widthIn(max = 600.dp)
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = 24.dp, vertical = 22.dp),
            ) {
                BrandMark()
                Spacer(Modifier.height(34.dp))
                Text(
                    text = "Leave the desk.\nKeep the session.",
                    color = MaterialTheme.colorScheme.onBackground,
                    fontSize = 40.sp,
                    lineHeight = 43.sp,
                    fontWeight = FontWeight.SemiBold,
                )
                Spacer(Modifier.height(14.dp))
                Text(
                    text = "AITerm puts your live desktop sessions in your hand without starting over.",
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    style = MaterialTheme.typography.bodyLarge,
                    lineHeight = 25.sp,
                )
                Spacer(Modifier.height(28.dp))
                LiveSessionPreview()
                Spacer(Modifier.height(30.dp))
                FeatureLine(
                    glyph = ">_",
                    title = "Continue live work",
                    body = "Read the conversation, type into the terminal, and move between active sessions.",
                )
                FeatureLine(
                    glyph = "+",
                    title = "Send what the task needs",
                    body = "Attach photos from your camera or gallery alongside your prompt.",
                )
                FeatureLine(
                    glyph = "◇",
                    title = "Connect your way",
                    body = "AITerm tries your local network first, then your VPN or encrypted relay.",
                )
                Spacer(Modifier.height(14.dp))
                Surface(
                    color = MaterialTheme.colorScheme.tertiary.copy(alpha = 0.10f),
                    shape = RoundedCornerShape(14.dp),
                ) {
                    Text(
                        text = "One QR scan pairs this phone. Your desktop identity stays pinned here.",
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 13.dp),
                        color = MaterialTheme.colorScheme.onSurface,
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
                Spacer(Modifier.height(14.dp))
            }
        }
    }
}

@Composable
private fun BrandMark() {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Surface(
            modifier = Modifier.size(38.dp),
            shape = RoundedCornerShape(11.dp),
            color = MaterialTheme.colorScheme.primary,
        ) {
            Box(contentAlignment = Alignment.Center) {
                Text(
                    text = ">_",
                    color = MaterialTheme.colorScheme.onPrimary,
                    fontFamily = FontFamily.Monospace,
                    fontWeight = FontWeight.Bold,
                    fontSize = 15.sp,
                )
            }
        }
        Spacer(Modifier.width(11.dp))
        Text(
            text = "AITerm",
            color = MaterialTheme.colorScheme.onBackground,
            style = MaterialTheme.typography.titleLarge,
            fontWeight = FontWeight.SemiBold,
        )
    }
}

@Composable
private fun LiveSessionPreview() {
    Surface(
        modifier = Modifier.fillMaxWidth(),
        color = TerminalNavy,
        shape = RoundedCornerShape(22.dp),
        shadowElevation = 2.dp,
    ) {
        Column(Modifier.padding(18.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Row(
                    modifier = Modifier.clearAndSetSemantics {},
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    PreviewDot(Color(0xFFFF8E76))
                    PreviewDot(Color(0xFFFFC857))
                    PreviewDot(TerminalSignal)
                }
                Spacer(Modifier.width(11.dp))
                Text(
                    text = "~/Projects/aiterm",
                    color = TerminalMuted,
                    fontFamily = FontFamily.Monospace,
                    fontSize = 12.sp,
                )
            }
            Spacer(Modifier.height(20.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Surface(
                    modifier = Modifier.size(38.dp),
                    shape = CircleShape,
                    color = Color(0xFF173E59),
                ) {
                    Box(contentAlignment = Alignment.Center) {
                        Text(
                            text = "C",
                            color = Color(0xFF75C8FF),
                            fontWeight = FontWeight.Bold,
                        )
                    }
                }
                Spacer(Modifier.width(12.dp))
                Column(Modifier.weight(1f)) {
                    Text(
                        text = "aiterm",
                        color = TerminalText,
                        style = MaterialTheme.typography.titleMedium,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Text(
                        text = "Codex · active now",
                        color = TerminalMuted,
                        fontFamily = FontFamily.Monospace,
                        fontSize = 12.sp,
                    )
                }
                Box(
                    Modifier.size(8.dp).clip(CircleShape).background(TerminalSignal)
                        .clearAndSetSemantics {},
                )
            }
            Spacer(Modifier.height(18.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    text = ">",
                    color = TerminalSignal,
                    fontFamily = FontFamily.Monospace,
                    fontWeight = FontWeight.Bold,
                )
                Spacer(Modifier.width(8.dp))
                Text(
                    text = "continue from my phone",
                    color = TerminalText,
                    fontFamily = FontFamily.Monospace,
                    fontSize = 13.sp,
                )
                Spacer(Modifier.width(3.dp))
                Box(
                    Modifier.width(7.dp).height(17.dp).background(TerminalSignal)
                        .clearAndSetSemantics {},
                )
            }
        }
    }
}

@Composable
private fun PreviewDot(color: Color) {
    Box(Modifier.size(7.dp).clip(CircleShape).background(color))
}

@Composable
private fun FeatureLine(glyph: String, title: String, body: String) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 10.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Surface(
            modifier = Modifier.size(40.dp).clearAndSetSemantics {},
            shape = RoundedCornerShape(12.dp),
            color = MaterialTheme.colorScheme.surfaceVariant,
        ) {
            Box(contentAlignment = Alignment.Center) {
                Text(
                    text = glyph,
                    color = MaterialTheme.colorScheme.primary,
                    fontFamily = FontFamily.Monospace,
                    fontWeight = FontWeight.Bold,
                )
            }
        }
        Spacer(Modifier.width(14.dp))
        Column {
            Text(
                text = title,
                color = MaterialTheme.colorScheme.onSurface,
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
            )
            Spacer(Modifier.height(3.dp))
            Text(
                text = body,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                style = MaterialTheme.typography.bodyMedium,
                lineHeight = 21.sp,
            )
        }
    }
}
