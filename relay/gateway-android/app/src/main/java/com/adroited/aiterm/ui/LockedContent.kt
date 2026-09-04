package com.adroited.aiterm.ui

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/** Contains no paired metadata by design. */
@Composable
fun LockedContent(
    onUnlock: () -> Unit,
    modifier: Modifier = Modifier,
    error: String? = null,
) {
    WelcomeScreen(onUnlock = onUnlock, unlockError = error, modifier = modifier)
}
