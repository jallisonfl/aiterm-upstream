package com.adroited.aiterm.ui

import androidx.compose.ui.graphics.Color
import org.junit.Assert.assertEquals
import org.junit.Test

class TerminalRenderingTest {
    @Test
    fun indexedTerminalColorsCoverAnsiCubeAndGrayscale() {
        assertEquals(Color(0xFF07111B), terminalIndexedColor(0))
        assertEquals(Color(0xFF000000), terminalIndexedColor(16))
        assertEquals(Color(0xFF0000FF), terminalIndexedColor(21))
        assertEquals(Color(0xFFFFFFFF), terminalIndexedColor(231))
        assertEquals(Color(0xFF080808), terminalIndexedColor(232))
        assertEquals(Color(0xFFEEEEEE), terminalIndexedColor(255))
    }
}
