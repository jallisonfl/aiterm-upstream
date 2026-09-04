package com.adroited.aiterm

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Guards the published application id. The desktop pairing flow and any future
 * install instructions name this package explicitly, so a rename must be a
 * deliberate change and not a side effect of a build edit.
 */
class AppIdentityTest {

    @Test
    fun packageName_isAdroitedAiterm() {
        assertEquals("com.adroited.aiterm", BuildConfig.APPLICATION_ID)
    }
}
