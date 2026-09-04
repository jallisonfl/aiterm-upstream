package com.adroited.aiterm.pairing

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.cbor.ByteString
import kotlinx.serialization.cbor.Cbor

sealed interface PairingFrame

data class AuthChallengeFrame(val nonce: ByteArray) : PairingFrame
data class PairRequestFrame(
    val enrollmentSecret: ByteArray,
    val deviceName: String,
    val publicKey: ByteArray,
    val relayAuthorityPublicKey: ByteArray? = null,
    val relaySignatureDer: ByteArray? = null,
) : PairingFrame

data class PairPendingFrame(val requestId: String) : PairingFrame
data class PairApprovedFrame(val deviceId: String) : PairingFrame
class PairDeniedFrame : PairingFrame {
    override fun equals(other: Any?): Boolean = other is PairDeniedFrame
    override fun hashCode(): Int = javaClass.hashCode()
    override fun toString(): String = "PairDeniedFrame"
}
class PairExpiredFrame : PairingFrame {
    override fun equals(other: Any?): Boolean = other is PairExpiredFrame
    override fun hashCode(): Int = javaClass.hashCode()
    override fun toString(): String = "PairExpiredFrame"
}

class PairingProtocolException(message: String, cause: Throwable? = null) : Exception(message, cause)

@OptIn(ExperimentalSerializationApi::class)
object PairingFrames {

    private const val MAX_PAIRING_FRAME_BYTES = 16 * 1_024

    private val cbor = Cbor {
        encodeDefaults = false
        ignoreUnknownKeys = false
        useDefiniteLengthEncoding = true
    }

    fun encode(frame: PairingFrame): ByteArray {
        val wire = when (frame) {
            is AuthChallengeFrame -> WireFrame(kind = "auth.challenge", nonce = frame.nonce)
            is PairRequestFrame -> WireFrame(
                kind = "pair.request",
                enrollmentSecret = frame.enrollmentSecret,
                deviceName = frame.deviceName,
                publicKey = frame.publicKey,
                relayAuthorityPublicKey = frame.relayAuthorityPublicKey,
                relaySignatureDer = frame.relaySignatureDer,
            )
            is PairPendingFrame -> WireFrame(kind = "pair.pending", requestId = frame.requestId)
            is PairApprovedFrame -> WireFrame(kind = "pair.approved", deviceId = frame.deviceId)
            is PairDeniedFrame -> WireFrame(kind = "pair.denied")
            is PairExpiredFrame -> WireFrame(kind = "pair.expired")
        }
        return cbor.encodeToByteArray(WireFrame.serializer(), wire)
    }

    fun decode(bytes: ByteArray): PairingFrame {
        if (bytes.isEmpty() || bytes.size > MAX_PAIRING_FRAME_BYTES) {
            throw PairingProtocolException("invalid pairing frame size")
        }
        try {
            rejectDuplicateTopLevelKeys(bytes)
        } catch (error: PairingProtocolException) {
            throw error
        } catch (error: Exception) {
            throw PairingProtocolException("malformed pairing frame", error)
        }
        val wire = try {
            cbor.decodeFromByteArray(WireFrame.serializer(), bytes)
        } catch (error: SerializationException) {
            throw PairingProtocolException("malformed pairing frame", error)
        } catch (error: IllegalArgumentException) {
            throw PairingProtocolException("malformed pairing frame", error)
        }
        return wire.toFrame()
    }

    @Serializable
    private data class WireFrame(
        val kind: String,
        @SerialName("enrollment_secret") @ByteString
        val enrollmentSecret: ByteArray? = null,
        @SerialName("device_name")
        val deviceName: String? = null,
        @SerialName("public_key") @ByteString
        val publicKey: ByteArray? = null,
        @SerialName("relay_authority_public_key") @ByteString
        val relayAuthorityPublicKey: ByteArray? = null,
        @SerialName("relay_signature_der") @ByteString
        val relaySignatureDer: ByteArray? = null,
        @SerialName("request_id")
        val requestId: String? = null,
        @SerialName("device_id")
        val deviceId: String? = null,
        @ByteString
        val nonce: ByteArray? = null,
    ) {
        fun toFrame(): PairingFrame = when (kind) {
            "auth.challenge" -> {
                requireFields("nonce")
                val challengeNonce = nonce ?: invalidText()
                if (challengeNonce.size != AUTH_CHALLENGE_NONCE_BYTES) invalidText()
                AuthChallengeFrame(challengeNonce)
            }
            "pair.request" -> {
                val fields = mutableListOf("enrollment_secret", "device_name", "public_key")
                if (relayAuthorityPublicKey != null) fields += "relay_authority_public_key"
                if (relaySignatureDer != null) fields += "relay_signature_der"
                requireFields(*fields.toTypedArray())
                PairRequestFrame(
                    enrollmentSecret = enrollmentSecret!!,
                    deviceName = deviceName!!,
                    publicKey = publicKey!!,
                    relayAuthorityPublicKey = relayAuthorityPublicKey,
                    relaySignatureDer = relaySignatureDer,
                )
            }
            "pair.pending" -> {
                requireFields("request_id")
                PairPendingFrame(requestId!!)
            }
            "pair.approved" -> {
                requireFields("device_id")
                PairApprovedFrame(deviceId!!)
            }
            "pair.denied" -> {
                requireFields()
                PairDeniedFrame()
            }
            "pair.expired" -> {
                requireFields()
                PairExpiredFrame()
            }
            else -> throw PairingProtocolException("unknown pairing frame kind")
        }

        private fun requireFields(vararg required: String) {
            val populated = buildSet {
                if (enrollmentSecret != null) add("enrollment_secret")
                if (deviceName != null) add("device_name")
                if (publicKey != null) add("public_key")
                if (relayAuthorityPublicKey != null) add("relay_authority_public_key")
                if (relaySignatureDer != null) add("relay_signature_der")
                if (requestId != null) add("request_id")
                if (deviceId != null) add("device_id")
                if (nonce != null) add("nonce")
            }
            if (populated != required.toSet()) {
                throw PairingProtocolException("pairing frame has missing or unexpected fields")
            }
            when (kind) {
                "pair.pending" -> if (requestId.isNullOrBlank() || requestId.length > 128) invalidText()
                "pair.approved" -> if (deviceId.isNullOrBlank() || deviceId.length > 128) invalidText()
                "pair.request" -> if (
                    deviceName.isNullOrBlank() ||
                    deviceName.length > 128 ||
                    enrollmentSecret?.size != 32 ||
                    publicKey?.size != 33 ||
                    ((relayAuthorityPublicKey == null) != (relaySignatureDer == null)) ||
                    (relayAuthorityPublicKey != null && relayAuthorityPublicKey.size != 33) ||
                    (relaySignatureDer != null && relaySignatureDer.size !in 8..80)
                ) invalidText()
            }
        }

        private fun invalidText(): Nothing =
            throw PairingProtocolException("pairing frame contains an invalid field")
    }

    /**
     * kotlinx.serialization accepts a repeated CBOR map key with last-value
     * wins semantics. Trust-bearing protocol frames must instead have one
     * unambiguous interpretation, so reject duplicates before deserializing.
     */
    private fun rejectDuplicateTopLevelKeys(bytes: ByteArray) {
        val cursor = CborCursor(bytes)
        val fieldCount = cursor.readMapLength()
        val keys = HashSet<String>(fieldCount.coerceAtMost(32))
        repeat(fieldCount) {
            val key = cursor.readText()
            if (!keys.add(key)) {
                throw PairingProtocolException("duplicate pairing frame field")
            }
            cursor.skipItem()
        }
        if (!cursor.isAtEnd()) {
            throw PairingProtocolException("malformed pairing frame")
        }
    }

    private class CborCursor(private val bytes: ByteArray) {
        private var position = 0

        fun isAtEnd(): Boolean = position == bytes.size

        fun readMapLength(): Int {
            val (major, length) = readHeader()
            if (major != 5) malformed()
            return length.asCollectionSize()
        }

        fun readText(): String {
            val (major, length) = readHeader()
            if (major != 3) malformed()
            val size = length.asByteCount()
            val start = position
            position += size
            return bytes.decodeToString(start, position, throwOnInvalidSequence = true)
        }

        fun skipItem(depth: Int = 0) {
            if (depth > MAX_CBOR_NESTING) malformed()
            val (major, length) = readHeader()
            when (major) {
                0, 1, 7 -> Unit
                2, 3 -> position += length.asByteCount()
                4 -> repeat(length.asCollectionSize()) { skipItem(depth + 1) }
                5 -> repeat(length.asCollectionSize()) {
                    skipItem(depth + 1)
                    skipItem(depth + 1)
                }
                6 -> skipItem(depth + 1)
                else -> malformed()
            }
        }

        private fun readHeader(): Pair<Int, Long> {
            if (position >= bytes.size) malformed()
            val initial = bytes[position++].toInt() and 0xff
            val major = initial ushr 5
            val additional = initial and 0x1f
            val argument = when (additional) {
                in 0..23 -> additional.toLong()
                24 -> readUnsigned(1)
                25 -> readUnsigned(2)
                26 -> readUnsigned(4)
                27 -> readUnsigned(8)
                // Rust emits definite-length protocol frames. Rejecting
                // indefinite encodings also keeps the duplicate-key scan exact.
                else -> malformed()
            }
            return major to argument
        }

        private fun readUnsigned(byteCount: Int): Long {
            if (position + byteCount > bytes.size) malformed()
            var value = 0L
            repeat(byteCount) {
                if (value > (Long.MAX_VALUE ushr 8)) malformed()
                value = (value shl 8) or (bytes[position++].toLong() and 0xff)
            }
            return value
        }

        private fun Long.asByteCount(): Int {
            if (this > Int.MAX_VALUE || this > bytes.size - position) malformed()
            return toInt()
        }

        private fun Long.asCollectionSize(): Int {
            if (this > MAX_CBOR_COLLECTION_ITEMS) malformed()
            return toInt()
        }

        private fun malformed(): Nothing =
            throw PairingProtocolException("malformed pairing frame")
    }

    private const val AUTH_CHALLENGE_NONCE_BYTES = 32
    private const val MAX_CBOR_NESTING = 16
    private const val MAX_CBOR_COLLECTION_ITEMS = 1_024
}
