package com.adroited.aiterm.pairing

import com.adroited.aiterm.security.DeviceKeys
import com.adroited.aiterm.security.compressedSec1
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import java.util.Base64

/** base64url without padding, exactly what the QR payload carries. */
internal fun ByteArray.toBase64Url(): String =
    Base64.getUrlEncoder().withoutPadding().encodeToString(this)

/**
 * Builds an `aiterm://pair` URI. Every field is a parameter so a test can make
 * exactly one of them wrong.
 */
internal fun pairingUri(
    version: String = "1",
    hosts: List<String> = listOf("localhost"),
    port: Int = 8443,
    fingerprint: String = ByteArray(32) { 7 }.toBase64Url(),
    secret: ByteArray = ByteArray(32) { it.toByte() },
    name: String = "Matt%27s%20desktop",
    relayHost: String? = null,
    relayPort: Int? = null,
    relayAuthorizationDigest: ByteArray? = null,
): String = buildString {
    append("aiterm://pair?v=").append(version)
    hosts.forEach { append("&h=").append(it) }
    append("&p=").append(port)
    append("&f=").append(fingerprint)
    append("&s=").append(secret.toBase64Url())
    append("&n=").append(name)
    relayHost?.let { append("&r=").append(it) }
    relayPort?.let { append("&q=").append(it) }
    relayAuthorizationDigest?.let { append("&a=").append(it.toBase64Url()) }
}

internal fun parsedPayload(
    uri: String = pairingUri(),
    scannedAtEpochMillis: Long = 0L,
): PairingPayload =
    (PairingPayload.parse(uri, scannedAtEpochMillis) as PairingPayloadResult.Parsed).payload

/**
 * A JVM stand-in for the Android Keystore key. Real enrollment never sees the
 * private key bytes; this fake only exists because AndroidKeyStore has no JVM
 * implementation, so the repository must not depend on it directly.
 */
internal class FakeDeviceKeys : DeviceKeys {

    private val keyPair: KeyPair = KeyPairGenerator.getInstance("EC").apply {
        initialize(ECGenParameterSpec("secp256r1"))
    }.generateKeyPair()

    var publicKeyRequests: Int = 0
        private set

    override fun devicePublicKey(): ByteArray {
        publicKeyRequests++
        return (keyPair.public as ECPublicKey).compressedSec1()
    }

    override fun signChallenge(nonce: ByteArray): ByteArray =
        Signature.getInstance("SHA256withECDSA").run {
            initSign(keyPair.private)
            update(nonce)
            sign()
        }
}

/** In-memory replacement for the SharedPreferences-backed store. */
internal class FakePairedDesktopStore : PairedDesktopStore {

    private val saved = mutableListOf<PairedDesktop>()

    override fun all(): List<PairedDesktop> = saved.toList()

    override fun save(desktop: PairedDesktop) {
        saved.removeAll { it.deviceId == desktop.deviceId }
        saved += desktop
    }

    override fun remove(deviceId: String) {
        saved.removeAll { it.deviceId == deviceId }
    }
}

/**
 * Records which candidate endpoints the repository tried, in order, and answers
 * with a scripted outcome per host.
 */
internal class RecordingPairingTransport(
    private val outcomes: Map<String, EnrollmentOutcome>,
    private val fallback: EnrollmentOutcome = EnrollmentOutcome.Unreachable("no route"),
    private val delegate: PairingTransport? = null,
) : PairingTransport {

    val attempted = mutableListOf<PairingEndpoint>()
    var lastRelayAuthorityPublicKey: ByteArray? = null
        private set
    var lastRelaySignatureDer: ByteArray? = null
        private set

    override suspend fun enroll(
        endpoint: PairingEndpoint,
        serverSpkiFingerprint: String,
        enrollmentSecret: EnrollmentSecret,
        deviceName: String,
        devicePublicKey: ByteArray,
        relayAuthorityPublicKey: ByteArray?,
        relaySignatureDer: ByteArray?,
        onPending: () -> Unit,
    ): EnrollmentOutcome {
        attempted += endpoint
        lastRelayAuthorityPublicKey = relayAuthorityPublicKey
        lastRelaySignatureDer = relaySignatureDer
        delegate?.let {
            return it.enroll(
                endpoint,
                serverSpkiFingerprint,
                enrollmentSecret,
                deviceName,
                devicePublicKey,
                relayAuthorityPublicKey,
                relaySignatureDer,
                onPending,
            )
        }
        val outcome = outcomes[endpoint.host] ?: fallback
        if (
            outcome is EnrollmentOutcome.Unreachable ||
            outcome is EnrollmentOutcome.FingerprintMismatch ||
            outcome is EnrollmentOutcome.TlsIdentityMismatch
        ) {
            return outcome
        }
        return when (enrollmentSecret.consume { Unit }) {
            is EnrollmentSecret.Consumption.Used -> outcome
            EnrollmentSecret.Consumption.AlreadyConsumed -> EnrollmentOutcome.ConsumedPayload
        }
    }
}
