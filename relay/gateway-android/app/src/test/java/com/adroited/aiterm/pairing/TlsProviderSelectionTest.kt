package com.adroited.aiterm.pairing

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TlsProviderSelectionTest {

    @Test
    fun api26Through28AndroidRuntimes_useTheBundledTls13Provider() {
        assertTrue(shouldUseBundledTls13Provider(isAndroidRuntime = true, sdkInt = 26))
        assertTrue(shouldUseBundledTls13Provider(isAndroidRuntime = true, sdkInt = 28))
        assertFalse(shouldUseBundledTls13Provider(isAndroidRuntime = true, sdkInt = 29))
        assertFalse(shouldUseBundledTls13Provider(isAndroidRuntime = false, sdkInt = 26))
    }
}
