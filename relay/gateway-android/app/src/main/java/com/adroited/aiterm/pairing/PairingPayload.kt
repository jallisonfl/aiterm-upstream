package com.adroited.aiterm.pairing

import java.io.ByteArrayOutputStream
import java.net.URI
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.util.Base64
import okhttp3.HttpUrl

enum class PairingFailure {
    UNSUPPORTED_VERSION,
    MALFORMED_PAYLOAD,
    EXPIRED_PAYLOAD,
    CONSUMED_PAYLOAD,
    FINGERPRINT_MISMATCH,
    TLS_IDENTITY_MISMATCH,
    UNREACHABLE,
    ENROLLMENT_STATE_UNKNOWN,
    DENIED_BY_DESKTOP,
    PROTOCOL_ERROR,
    KEY_UNAVAILABLE,
    STORAGE_FAILURE,
}

sealed interface PairingPayloadResult {
    data class Parsed(val payload: PairingPayload) : PairingPayloadResult
    data class Rejected(val failure: PairingFailure) : PairingPayloadResult
}

/**
 * Mutable solely so the enrollment secret can be claimed once and zeroed as
 * soon as that attempt finishes. No byte accessor is public and toString is
 * permanently redacted.
 */
class EnrollmentSecret private constructor(private var bytes: ByteArray?) {

    internal sealed interface Consumption<out T> {
        data class Used<T>(val value: T) : Consumption<T>
        data object AlreadyConsumed : Consumption<Nothing>
    }

    /**
     * Claims the secret at the protocol boundary where it is first placed in
     * a pair.request. The callback is deliberately synchronous so callers
     * cannot retain the mutable bytes across a suspension point.
     */
    internal fun <T> consume(block: (ByteArray) -> T): Consumption<T> {
        val claimed = synchronized(this) {
            bytes?.also { bytes = null }
        } ?: return Consumption.AlreadyConsumed

        return try {
            Consumption.Used(block(claimed))
        } finally {
            claimed.fill(0)
        }
    }

    internal fun isAvailable(): Boolean = synchronized(this) { bytes != null }

    internal fun discard() {
        val discarded = synchronized(this) {
            bytes?.also { bytes = null }
        }
        discarded?.fill(0)
    }

    override fun toString(): String = "<redacted enrollment secret>"

    companion object {
        internal fun takeOwnership(bytes: ByteArray): EnrollmentSecret = EnrollmentSecret(bytes)
    }
}

class PairingPayload private constructor(
    val hosts: List<String>,
    val port: Int,
    val serverSpkiFingerprint: String,
    val secret: EnrollmentSecret,
    val desktopName: String,
    val scannedAtEpochMillis: Long,
    val relayHost: String?,
    val relayPort: Int?,
    val relayAuthorizationDigest: ByteArray?,
) {

    val relayEndpoint: PairingEndpoint?
        get() = relayHost?.let { host -> relayPort?.let { port -> PairingEndpoint(host, port) } }

    fun isExpired(nowEpochMillis: Long): Boolean {
        val age = nowEpochMillis - scannedAtEpochMillis
        return age < 0 || age >= LIFETIME_MILLIS
    }

    internal fun discard() = secret.discard()

    override fun toString(): String =
        "PairingPayload(hosts=$hosts, port=$port, " +
            "serverSpkiFingerprint=$serverSpkiFingerprint, secret=$secret, " +
            "desktopName=$desktopName, scannedAtEpochMillis=$scannedAtEpochMillis)"

    companion object {
        const val LIFETIME_MILLIS: Long = 5 * 60 * 1_000L

        private const val MAX_URI_CHARS = 4_096
        // Matches the desktop's MAX_ADVERTISED_HOSTS: a docker-heavy machine
        // legitimately advertises a dozen bridge addresses, and a parser cap
        // below the advertiser's turned its QR into "not valid" outright.
        private const val MAX_HOSTS = 16
        private const val MAX_DISPLAY_NAME_CHARS = 128
        private val base64Url = Regex("^[A-Za-z0-9_-]+$")
        private val requiredSingletonFields = setOf("v", "p", "f", "s", "n")
        private val optionalSingletonFields = setOf("r", "q", "a")
        private val knownFields = requiredSingletonFields + optionalSingletonFields + "h"

        fun parse(raw: String, scannedAtEpochMillis: Long): PairingPayloadResult {
            if (raw.isEmpty() || raw.length > MAX_URI_CHARS || scannedAtEpochMillis < 0) {
                return malformed()
            }

            val uri = try {
                URI(raw)
            } catch (_: Exception) {
                return malformed()
            }
            if (
                uri.scheme != "aiterm" ||
                uri.rawAuthority != "pair" ||
                !uri.rawPath.isNullOrEmpty() ||
                uri.rawFragment != null ||
                uri.rawUserInfo != null
            ) {
                return malformed()
            }

            val query = uri.rawQuery ?: return malformed()
            if (query.isEmpty() || query.startsWith('&') || query.endsWith('&') || "&&" in query) {
                return malformed()
            }

            val fields = linkedMapOf<String, MutableList<String>>()
            for (component in query.split('&')) {
                val separator = component.indexOf('=')
                if (separator <= 0) return malformed()
                val key = component.substring(0, separator)
                // A field this app does not know is another client's, not an
                // attack: a combined QR carries a second listener's payload
                // under its own names (`tp`/`tt`/`tf`/`z`), and this desktop
                // may advertise more transports than this app speaks. Skip
                // it unread — everything this app will trust stays under the
                // strict known fields below.
                if (key !in knownFields) continue
                val value = decodeQueryValue(component.substring(separator + 1)) ?: return malformed()
                fields.getOrPut(key) { mutableListOf() } += value
            }

            if (
                requiredSingletonFields.any { fields[it]?.size != 1 } ||
                optionalSingletonFields.any { fields[it]?.size?.let { count -> count > 1 } == true }
            ) return malformed()
            val version = fields.getValue("v").single()
            if (version !in setOf("1", "2", "3")) {
                return PairingPayloadResult.Rejected(PairingFailure.UNSUPPORTED_VERSION)
            }

            val hosts = fields["h"]?.toList().orEmpty()
            if (hosts.isEmpty() || hosts.size > MAX_HOSTS || hosts.any { !isValidHost(it) }) {
                return malformed()
            }
            val portText = fields.getValue("p").single()
            if (portText.isEmpty() || portText.any { !it.isDigit() }) return malformed()
            val port = portText.toIntOrNull()?.takeIf { it in 1..65_535 } ?: return malformed()

            val fingerprint = fields.getValue("f").single()
            if (decodeBase64Url32(fingerprint) == null) return malformed()
            val secretBytes = decodeBase64Url32(fields.getValue("s").single()) ?: return malformed()
            val name = fields.getValue("n").single()
            if (
                name.isBlank() ||
                name.length > MAX_DISPLAY_NAME_CHARS ||
                name.any(Char::isISOControl)
            ) {
                secretBytes.fill(0)
                return malformed()
            }
            val relayHost = fields["r"]?.singleOrNull()
            val relayPortText = fields["q"]?.singleOrNull()
            if ((relayHost == null) != (relayPortText == null)) {
                secretBytes.fill(0)
                return malformed()
            }
            if ((version in setOf("2", "3")) != (relayHost != null)) {
                secretBytes.fill(0)
                return malformed()
            }
            val relayAuthorizationDigest = fields["a"]?.singleOrNull()?.let(::decodeBase64Url32)
            if ((version == "3") != (relayAuthorizationDigest != null)) {
                secretBytes.fill(0)
                relayAuthorizationDigest?.fill(0)
                return malformed()
            }
            val relayPort = relayPortText?.let { text ->
                if (text.isEmpty() || text.any { !it.isDigit() }) null else text.toIntOrNull()
            }
            if (
                relayHost?.let { !isValidHost(it) } == true ||
                (relayPortText != null && relayPort !in 1..65_535)
            ) {
                secretBytes.fill(0)
                return malformed()
            }

            return PairingPayloadResult.Parsed(
                PairingPayload(
                    hosts = hosts,
                    port = port,
                    serverSpkiFingerprint = fingerprint,
                    secret = EnrollmentSecret.takeOwnership(secretBytes),
                    desktopName = name,
                    scannedAtEpochMillis = scannedAtEpochMillis,
                    relayHost = relayHost,
                    relayPort = relayPort,
                    relayAuthorizationDigest = relayAuthorizationDigest,
                ),
            )
        }

        internal fun isValidHost(host: String): Boolean {
            if (
                host.isEmpty() ||
                host.length > 253 ||
                host.any { it.isWhitespace() || it.isISOControl() } ||
                host.any { it == '@' || it == '/' || it == '\\' || it == '?' || it == '#' } ||
                (host.count { it == ':' } == 1)
            ) {
                return false
            }
            return try {
                HttpUrl.Builder().scheme("https").host(host).port(443).build()
                true
            } catch (_: IllegalArgumentException) {
                false
            }
        }

        internal fun isValidFingerprint(value: String): Boolean = decodeBase64Url32(value) != null

        private fun decodeQueryValue(rawValue: String): String? {
            val bytes = ByteArrayOutputStream(rawValue.length)
            var index = 0
            while (index < rawValue.length) {
                val char = rawValue[index]
                if (char == '%') {
                    if (index + 2 >= rawValue.length) return null
                    val high = Character.digit(rawValue[index + 1], 16)
                    val low = Character.digit(rawValue[index + 2], 16)
                    if (high < 0 || low < 0) return null
                    bytes.write((high shl 4) or low)
                    index += 3
                    continue
                }
                if (
                    Character.isLowSurrogate(char) ||
                    (Character.isHighSurrogate(char) &&
                        (index + 1 >= rawValue.length || !Character.isLowSurrogate(rawValue[index + 1])))
                ) {
                    return null
                }
                val codePoint = Character.codePointAt(rawValue, index)
                val encoded = String(Character.toChars(codePoint)).toByteArray(Charsets.UTF_8)
                bytes.write(encoded, 0, encoded.size)
                index += Character.charCount(codePoint)
            }
            return try {
                Charsets.UTF_8.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .decode(ByteBuffer.wrap(bytes.toByteArray()))
                    .toString()
            } catch (_: Exception) {
                null
            }
        }

        private fun decodeBase64Url32(value: String): ByteArray? {
            if (!base64Url.matches(value)) return null
            val decoded = try {
                Base64.getUrlDecoder().decode(value)
            } catch (_: IllegalArgumentException) {
                return null
            }
            if (decoded.size != 32) {
                decoded.fill(0)
                return null
            }
            val canonical = Base64.getUrlEncoder().withoutPadding().encodeToString(decoded)
            if (canonical != value) {
                decoded.fill(0)
                return null
            }
            return decoded
        }

        private fun malformed() = PairingPayloadResult.Rejected(PairingFailure.MALFORMED_PAYLOAD)
    }
}
