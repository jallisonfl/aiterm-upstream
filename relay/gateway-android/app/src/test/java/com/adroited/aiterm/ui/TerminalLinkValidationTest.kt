package com.adroited.aiterm.ui

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TerminalLinkValidationTest {
    @Test
    fun onlyHostBoundHttpLinksWithoutUserInfoAreInteractive() {
        assertTrue(isSafeRemoteLink("https://example.com/path?q=1"))
        assertFalse(isSafeRemoteLink("javascript:alert(1)"))
        assertFalse(isSafeRemoteLink("https://user@example.com/secret"))
        assertFalse(isSafeRemoteLink("https:///missing-host"))
    }
}
