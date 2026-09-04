package com.adroited.aiterm.security

import org.junit.Assert.assertEquals
import org.junit.Test

class UnlockPolicyTest {

    @Test
    fun api30AndNewer_useTheCombinedSystemPrompt() {
        assertEquals(
            UnlockRoute.COMBINED_PROMPT,
            chooseUnlockRoute(
                sdkInt = 30,
                combinedAuthenticationAvailable = true,
                strongBiometricAvailable = true,
                deviceSecure = true,
            ),
        )
    }

    @Test
    fun olderAndroid_usesStrongBiometricWithCredentialFallback() {
        assertEquals(
            UnlockRoute.STRONG_BIOMETRIC_PROMPT,
            chooseUnlockRoute(
                sdkInt = 29,
                combinedAuthenticationAvailable = false,
                strongBiometricAvailable = true,
                deviceSecure = true,
            ),
        )
        assertEquals(
            UnlockRoute.DEVICE_CREDENTIAL,
            chooseUnlockRoute(
                sdkInt = 28,
                combinedAuthenticationAvailable = false,
                strongBiometricAvailable = false,
                deviceSecure = true,
            ),
        )
    }

    @Test
    fun noAllowedAuthenticator_failsClosed() {
        assertEquals(
            UnlockRoute.UNAVAILABLE,
            chooseUnlockRoute(
                sdkInt = 29,
                combinedAuthenticationAvailable = false,
                strongBiometricAvailable = false,
                deviceSecure = false,
            ),
        )
    }
}
