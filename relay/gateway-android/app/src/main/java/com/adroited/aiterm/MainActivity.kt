package com.adroited.aiterm

import android.app.Activity
import android.app.KeyguardManager
import android.os.Build
import android.os.Bundle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import com.adroited.aiterm.security.UnlockRoute
import com.adroited.aiterm.security.chooseUnlockRoute
import com.adroited.aiterm.ui.AitermApp

/**
 * The only Activity in the app. Every screen is a Compose destination inside
 * [AitermApp]. Terminal and conversation UI are native Compose; the only
 * WebView is the explicit preview for a webpage built by a remote session.
 */
class MainActivity : FragmentActivity() {

    private lateinit var biometricPrompt: BiometricPrompt
    private var unlockError: String? by mutableStateOf(null)
    private val deviceCredentialResult = registerForActivityResult(
        ActivityResultContracts.StartActivityForResult(),
    ) { result ->
        if (result.resultCode == Activity.RESULT_OK) completeUnlock()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        biometricPrompt = BiometricPrompt(
            this,
            ContextCompat.getMainExecutor(this),
            object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(
                    result: BiometricPrompt.AuthenticationResult,
                ) {
                    completeUnlock()
                }

                override fun onAuthenticationFailed() {
                    unlockError = "Authentication was not recognized. Try again."
                }

                override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                    when {
                        errorCode == BiometricPrompt.ERROR_NEGATIVE_BUTTON &&
                            Build.VERSION.SDK_INT < Build.VERSION_CODES.R ->
                            requestDeviceCredential()
                        errorCode == BiometricPrompt.ERROR_USER_CANCELED ||
                            errorCode == BiometricPrompt.ERROR_NEGATIVE_BUTTON ||
                            errorCode == BiometricPrompt.ERROR_CANCELED -> Unit
                        else -> unlockError = "AITerm stayed locked: $errString"
                    }
                }
            },
        )
        setContent {
            AitermApp(
                onRequestUnlock = ::requestUnlock,
                unlockError = unlockError,
            )
        }
    }

    private fun requestUnlock() {
        unlockError = null
        val manager = BiometricManager.from(this)
        val keyguard = getSystemService(KeyguardManager::class.java)
        val route = chooseUnlockRoute(
            sdkInt = Build.VERSION.SDK_INT,
            combinedAuthenticationAvailable =
                Build.VERSION.SDK_INT >= Build.VERSION_CODES.R &&
                    manager.canAuthenticate(REQUIRED_AUTHENTICATORS) ==
                    BiometricManager.BIOMETRIC_SUCCESS,
            strongBiometricAvailable =
                manager.canAuthenticate(BiometricManager.Authenticators.BIOMETRIC_STRONG) ==
                    BiometricManager.BIOMETRIC_SUCCESS,
            deviceSecure = keyguard.isDeviceSecure,
        )
        when (route) {
            UnlockRoute.COMBINED_PROMPT -> biometricPrompt.authenticate(
                promptBuilder().setAllowedAuthenticators(REQUIRED_AUTHENTICATORS).build(),
            )
            UnlockRoute.STRONG_BIOMETRIC_PROMPT -> biometricPrompt.authenticate(
                promptBuilder()
                    .setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_STRONG)
                    .setNegativeButtonText("Use device PIN")
                    .build(),
            )
            UnlockRoute.DEVICE_CREDENTIAL -> requestDeviceCredential()
            UnlockRoute.UNAVAILABLE ->
                unlockError = "Set up a strong biometric or device screen lock to unlock AITerm."
        }
    }

    private fun promptBuilder() = BiometricPrompt.PromptInfo.Builder()
        .setTitle("Unlock AITerm")
        .setSubtitle("Authenticate before desktop data is shown")

    @Suppress("DEPRECATION")
    private fun requestDeviceCredential() {
        val intent = getSystemService(KeyguardManager::class.java)
            .createConfirmDeviceCredentialIntent(
                "Unlock AITerm",
                "Authenticate before desktop data is shown",
            )
        if (intent == null) {
            unlockError = "Device-PIN authentication is unavailable."
        } else {
            deviceCredentialResult.launch(intent)
        }
    }

    private fun completeUnlock() {
        unlockError = null
        (application as AitermApplication).container.appLock.unlock()
    }

    companion object {
        private const val REQUIRED_AUTHENTICATORS =
            BiometricManager.Authenticators.BIOMETRIC_STRONG or
                BiometricManager.Authenticators.DEVICE_CREDENTIAL
    }
}
