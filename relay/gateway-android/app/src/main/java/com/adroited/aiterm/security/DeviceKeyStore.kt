package com.adroited.aiterm.security

import android.annotation.SuppressLint
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyInfo
import android.security.keystore.KeyProperties
import java.security.KeyFactory
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.PrivateKey
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec

interface DeviceKeys {
    fun devicePublicKey(): ByteArray
    fun signChallenge(nonce: ByteArray): ByteArray
}

class DeviceKeyException(message: String, cause: Throwable? = null) :
    IllegalStateException(message, cause)

/**
 * The private key is generated inside Android Keystore and is never exported.
 * Authentication is valid for the same five-minute window as the foreground
 * app lock, allowing both a strong biometric and the device credential.
 */
class AndroidDeviceKeyStore(private val alias: String = DEFAULT_ALIAS) : DeviceKeys {

    @Synchronized
    override fun devicePublicKey(): ByteArray = try {
        val keyPair = loadOrCreateKeyPair()
        (keyPair.public as? ECPublicKey)?.compressedSec1()
            ?: throw DeviceKeyException("the Android Keystore device key is not P-256")
    } catch (error: DeviceKeyException) {
        throw error
    } catch (error: Exception) {
        throw DeviceKeyException("the Android Keystore device key is unavailable", error)
    }

    @Synchronized
    override fun signChallenge(nonce: ByteArray): ByteArray {
        if (nonce.size != CHALLENGE_BYTES) {
            throw DeviceKeyException("the authentication challenge has an invalid size")
        }
        return try {
            val privateKey = loadOrCreateKeyPair().private
            Signature.getInstance("SHA256withECDSA").run {
                initSign(privateKey)
                update(nonce)
                sign()
            }
        } catch (error: DeviceKeyException) {
            throw error
        } catch (error: Exception) {
            throw DeviceKeyException(
                "the device key could not sign; authentication may be required",
                error,
            )
        }
    }

    private fun loadOrCreateKeyPair(): KeyPair {
        val keyStore = KeyStore.getInstance(ANDROID_KEY_STORE).apply { load(null) }
        if (!keyStore.containsAlias(alias)) {
            generateKeyPair()
        }
        val privateKey = keyStore.getKey(alias, null) as? PrivateKey
            ?: throw DeviceKeyException("the Android Keystore device key is missing")
        val publicKey = keyStore.getCertificate(alias)?.publicKey
            ?: throw DeviceKeyException("the Android Keystore public key is missing")
        verifyAuthenticationPolicy(privateKey)
        return KeyPair(publicKey, privateKey)
    }

    private fun generateKeyPair() {
        val builder = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_SIGN)
            .setAlgorithmParameterSpec(ECGenParameterSpec("secp256r1"))
            .setDigests(KeyProperties.DIGEST_SHA256)
            .setUserAuthenticationRequired(true)

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            builder.setUserAuthenticationParameters(
                AUTH_VALIDITY_SECONDS,
                REQUIRED_AUTHENTICATORS,
            )
        } else {
            @Suppress("DEPRECATION")
            builder.setUserAuthenticationValidityDurationSeconds(AUTH_VALIDITY_SECONDS)
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            builder.setUnlockedDeviceRequired(true)
        }

        KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, ANDROID_KEY_STORE).run {
            initialize(builder.build())
            generateKeyPair()
        }
    }

    private fun verifyAuthenticationPolicy(privateKey: PrivateKey) {
        val info = KeyFactory.getInstance(privateKey.algorithm, ANDROID_KEY_STORE)
            .getKeySpec(privateKey, KeyInfo::class.java)
        if (!info.isUserAuthenticationRequired || info.keySize != 256) {
            throw DeviceKeyException("the existing Android Keystore device key is not compliant")
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            if (info.userAuthenticationType != REQUIRED_AUTHENTICATORS) {
                throw DeviceKeyException("the existing Android Keystore auth policy is not compliant")
            }
        }
        @Suppress("DEPRECATION")
        if (info.userAuthenticationValidityDurationSeconds != AUTH_VALIDITY_SECONDS) {
            throw DeviceKeyException("the existing Android Keystore auth window is not compliant")
        }
    }

    companion object {
        private const val ANDROID_KEY_STORE = "AndroidKeyStore"
        private const val DEFAULT_ALIAS = "aiterm-device-p256-v1"
        private const val CHALLENGE_BYTES = 32
        private const val AUTH_VALIDITY_SECONDS = 5 * 60

        @SuppressLint("InlinedApi")
        private val REQUIRED_AUTHENTICATORS =
            KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL
    }
}
