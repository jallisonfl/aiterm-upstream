package com.adroited.aiterm.security

enum class UnlockRoute {
    COMBINED_PROMPT,
    STRONG_BIOMETRIC_PROMPT,
    DEVICE_CREDENTIAL,
    UNAVAILABLE,
}

internal fun chooseUnlockRoute(
    sdkInt: Int,
    combinedAuthenticationAvailable: Boolean,
    strongBiometricAvailable: Boolean,
    deviceSecure: Boolean,
): UnlockRoute = when {
    sdkInt >= 30 && combinedAuthenticationAvailable -> UnlockRoute.COMBINED_PROMPT
    sdkInt < 30 && strongBiometricAvailable -> UnlockRoute.STRONG_BIOMETRIC_PROMPT
    deviceSecure -> UnlockRoute.DEVICE_CREDENTIAL
    else -> UnlockRoute.UNAVAILABLE
}
