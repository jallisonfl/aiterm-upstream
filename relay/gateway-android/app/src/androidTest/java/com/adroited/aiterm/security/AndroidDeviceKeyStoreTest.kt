package com.adroited.aiterm.security

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyInfo
import android.security.keystore.KeyProperties
import androidx.biometric.BiometricManager
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import java.security.KeyFactory
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.spec.ECGenParameterSpec
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AndroidDeviceKeyStoreTest {

    @Test
    fun generatedP256Key_requiresStrongBiometricOrDeviceCredential() {
        check(Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            "the pinned instrumentation device must expose API 30+ auth metadata"
        }
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        val biometricAuthenticators =
            BiometricManager.Authenticators.BIOMETRIC_STRONG or
                BiometricManager.Authenticators.DEVICE_CREDENTIAL
        assertEquals(
            "the verification device needs an enrolled strong biometric or device credential",
            BiometricManager.BIOMETRIC_SUCCESS,
            BiometricManager.from(context).canAuthenticate(biometricAuthenticators),
        )

        val alias = "aiterm-instrumentation-${System.nanoTime()}"
        val keyStore = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        try {
            val keys = AndroidDeviceKeyStore(alias)
            assertEquals(33, keys.devicePublicKey().size)

            val privateKey = keyStore.getKey(alias, null)
            val keyInfo = KeyFactory.getInstance(privateKey.algorithm, "AndroidKeyStore")
                .getKeySpec(privateKey, KeyInfo::class.java)

            assertEquals(256, keyInfo.keySize)
            assertTrue(keyInfo.isUserAuthenticationRequired)
            assertEquals(
                KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL,
                keyInfo.userAuthenticationType,
            )
            @Suppress("DEPRECATION")
            assertEquals(5 * 60, keyInfo.userAuthenticationValidityDurationSeconds)
        } finally {
            keyStore.deleteEntry(alias)
        }
    }

    @Test
    fun existingKeyWithTheWrongAuthenticationWindow_isRejected() {
        check(Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            "the pinned instrumentation device must expose API 30+ auth metadata"
        }
        val alias = "aiterm-instrumentation-wrong-window-${System.nanoTime()}"
        val keyStore = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        try {
            val spec = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_SIGN)
                .setAlgorithmParameterSpec(ECGenParameterSpec("secp256r1"))
                .setDigests(KeyProperties.DIGEST_SHA256)
                .setUserAuthenticationRequired(true)
                .setUserAuthenticationParameters(
                    60,
                    KeyProperties.AUTH_BIOMETRIC_STRONG or
                        KeyProperties.AUTH_DEVICE_CREDENTIAL,
                )
                .setUnlockedDeviceRequired(true)
                .build()
            KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, "AndroidKeyStore")
                .run {
                    initialize(spec)
                    generateKeyPair()
                }

            assertThrows(DeviceKeyException::class.java) {
                AndroidDeviceKeyStore(alias).devicePublicKey()
            }
        } finally {
            keyStore.deleteEntry(alias)
        }
    }
}
