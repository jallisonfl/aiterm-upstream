package com.adroited.aiterm.ui

import com.adroited.aiterm.pairing.PairedDesktop
import org.junit.Assert.assertEquals
import org.junit.Test

class AitermAppTest {
    @Test
    fun exactlyOneDesktopOpensItsSessionsDirectly() {
        assertEquals(TerminalRoute("desktop-1"), initialDestination(listOf(desktop("desktop-1"))))
    }

    @Test
    fun zeroOrMultipleDesktopsOpenTheDesktopList() {
        assertEquals(DesktopsRoute, initialDestination(emptyList()))
        assertEquals(
            DesktopsRoute,
            initialDestination(listOf(desktop("desktop-1"), desktop("desktop-2"))),
        )
    }

    private fun desktop(id: String) = PairedDesktop(
        deviceId = id,
        displayName = id,
        hosts = listOf("10.0.0.151"),
        port = 8443,
        serverSpkiFingerprint = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        lastSeenEpochMillis = null,
    )
}
