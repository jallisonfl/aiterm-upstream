package com.adroited.aiterm.remote

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.cbor.ByteString
import kotlinx.serialization.cbor.Cbor

class RemoteProtocolException(message: String, cause: Throwable? = null) : Exception(message, cause)
class RemoteAccessRevokedException : Exception("remote access was revoked")

data class RemoteEventEnvelope(
    val requestId: Long,
    val kind: String,
    val payload: ByteArray,
)

@OptIn(ExperimentalSerializationApi::class)
object RemoteWireCodec {
    const val MAX_FRAME_BYTES = 1_024 * 1_024

    private val cbor = Cbor {
        encodeDefaults = true
        ignoreUnknownKeys = false
        useDefiniteLengthEncoding = true
    }

    fun encodeRequest(request: RemoteRequest): ByteArray {
        require(request.requestId > 0) { "request id must be positive" }
        require(request.kind.isNotEmpty() && request.kind.encodeToByteArray().size <= 64)
        val encoded = cbor.encodeToByteArray(
            RequestWire.serializer(),
            RequestWire(requestId = request.requestId, kind = request.kind, payload = request.payload),
        )
        if (encoded.size >= MAX_FRAME_BYTES) throw RemoteProtocolException("request frame is too large")
        return encoded
    }

    fun decodeEvent(bytes: ByteArray): RemoteEventEnvelope {
        validate(bytes)
        val wire = decode(EventWire.serializer(), bytes)
        if (wire.version != 1 || wire.requestId < 0 || wire.kind.isEmpty() ||
            wire.kind.encodeToByteArray().size > 64
        ) {
            throw RemoteProtocolException("invalid remote event envelope")
        }
        return RemoteEventEnvelope(wire.requestId, wire.kind, wire.payload)
    }

    fun encodeAuthProof(deviceId: String, signatureDer: ByteArray): ByteArray {
        val encoded = cbor.encodeToByteArray(
            AuthProofWire.serializer(),
            AuthProofWire(deviceId = deviceId, signatureDer = signatureDer),
        )
        if (encoded.size >= MAX_FRAME_BYTES) throw RemoteProtocolException("auth proof is too large")
        return encoded
    }

    fun encodeTerminalResumePayload(tabId: String, attachmentId: String, revision: Long): ByteArray {
        require(tabId.isNotEmpty() && attachmentId.isNotEmpty() && revision >= 0)
        return cbor.encodeToByteArray(
            TerminalResumeWire.serializer(),
            TerminalResumeWire(tabId, attachmentId, revision),
        ).also { if (it.size >= MAX_FRAME_BYTES) throw RemoteProtocolException("resume payload is too large") }
    }

    fun decodeAuthOk(payload: ByteArray) {
        validate(payload)
        val reply = decode(AuthReplyWire.serializer(), payload)
        if (reply.kind == "auth.denied") throw RemoteAccessRevokedException()
        if (reply.kind != "auth.ok") throw RemoteProtocolException("authentication was not accepted")
    }

    fun decodeStateSnapshot(payload: ByteArray): StateSnapshotChunk {
        validate(payload)
        return decode(StateSnapshotChunk.serializer(), payload).also { chunk ->
            if (chunk.transferId.length !in 1..64 || chunk.total !in 1..128 ||
                chunk.index !in 0 until chunk.total || chunk.tabs.size > 128
            ) {
                throw RemoteProtocolException("invalid roster transfer chunk")
            }
        }
    }

    fun decodeError(payload: ByteArray): RemoteErrorPayload {
        validate(payload)
        return decode(RemoteErrorPayload.serializer(), payload)
    }

    fun decodeTerminalChunk(payload: ByteArray, expectedRequestId: Long): TerminalTransferChunk {
        validate(payload)
        return TerminalWireDecoder.decode(payload, expectedRequestId)
    }

    internal fun validateCborPayload(payload: ByteArray) = validate(payload)

    private fun validate(bytes: ByteArray) {
        if (bytes.isEmpty() || bytes.size >= MAX_FRAME_BYTES) {
            throw RemoteProtocolException("invalid remote frame size")
        }
        StrictCborValidator(bytes).validate()
    }

    private fun <T> decode(serializer: kotlinx.serialization.KSerializer<T>, bytes: ByteArray): T =
        try {
            cbor.decodeFromByteArray(serializer, bytes)
        } catch (error: SerializationException) {
            throw RemoteProtocolException("malformed remote frame", error)
        } catch (error: IllegalArgumentException) {
            throw RemoteProtocolException("malformed remote frame", error)
        }

    @Serializable
    private data class RequestWire(
        val version: Int = 1,
        @SerialName("request_id") val requestId: Long,
        val kind: String,
        @ByteString val payload: ByteArray,
    )

    @Serializable
    private data class EventWire(
        val version: Int,
        @SerialName("request_id") val requestId: Long,
        val kind: String,
        @ByteString val payload: ByteArray,
    )

    @Serializable
    private data class AuthProofWire(
        val kind: String = "auth.proof",
        @SerialName("device_id") val deviceId: String,
        @SerialName("signature_der") @ByteString val signatureDer: ByteArray,
    )

    @Serializable
    private data class TerminalResumeWire(
        @SerialName("tab_id") val tabId: String,
        @SerialName("attachment_id") val attachmentId: String,
        val revision: Long,
    )

    @Serializable
    private data class AuthReplyWire(val kind: String)
}

@Serializable
data class RemoteErrorPayload(val code: String, val message: String)

@Serializable
enum class RemoteTabState {
    @SerialName("running") Running,
    @SerialName("exited") Exited,
}

@Serializable
enum class WireFocusOwner {
    @SerialName("self") Self,
    @SerialName("other") Other,
    @SerialName("unowned") Unowned,
}

@Serializable
data class RemoteTabExit(
    val code: Long? = null,
    val signal: String? = null,
    val requested: Boolean,
)

@Serializable
data class RemoteTab(
    val id: String,
    val title: String,
    val cwd: String? = null,
    val command: String? = null,
    val sessionId: String? = null,
    val resumedId: String? = null,
    val agentId: String? = null,
    val slotId: String = "",
    val fresh: Boolean = false,
    val envProvider: String? = null,
    val envModel: String? = null,
    val size: TerminalSize,
    val focus: WireFocusOwner = WireFocusOwner.Unowned,
    val state: RemoteTabState = RemoteTabState.Running,
    val exit: RemoteTabExit? = null,
)

@Serializable
data class StateSnapshotChunk(
    @SerialName("transfer_id") val transferId: String,
    val revision: Long,
    val index: Int,
    val total: Int,
    val tabs: List<RemoteTab>,
)

data class RemoteRoster(val revision: Long, val tabs: List<RemoteTab>)

class RosterTransferAssembler(private val maxTabs: Int = 128) {
    private var transferId: String? = null
    private var revision = 0L
    private var total = 0
    private var nextIndex = 0
    private val tabs = mutableListOf<RemoteTab>()

    val pendingCount: Int get() = if (transferId == null) 0 else 1

    @Synchronized
    fun accept(chunk: StateSnapshotChunk): RemoteRoster? {
        try {
            if (chunk.total !in 1..128 || chunk.index !in 0 until chunk.total) invalid()
            if (transferId == null) {
                if (chunk.index != 0) invalid()
                transferId = chunk.transferId
                revision = chunk.revision
                total = chunk.total
                nextIndex = 0
            }
            if (chunk.transferId != transferId || chunk.revision != revision ||
                chunk.total != total || chunk.index != nextIndex
            ) invalid()
            if (tabs.size + chunk.tabs.size > maxTabs) invalid()
            tabs += chunk.tabs
            nextIndex++
            if (nextIndex != total) return null
            return RemoteRoster(revision, tabs.toList()).also { clear() }
        } catch (error: RemoteProtocolException) {
            clear()
            throw error
        }
    }

    @Synchronized
    fun clear() {
        transferId = null
        revision = 0
        total = 0
        nextIndex = 0
        tabs.clear()
    }

    private fun invalid(): Nothing = throw RemoteProtocolException("invalid ordered roster transfer")
}

private class StrictCborValidator(private val bytes: ByteArray) {
    private var position = 0
    private var items = 0

    fun validate() {
        scan(0)
        if (position != bytes.size) malformed()
    }

    private fun scan(depth: Int) {
        if (depth > 32 || ++items > 300_000) malformed()
        val (major, length) = header()
        when (major) {
            0, 1, 7 -> Unit
            2, 3 -> advance(length)
            4 -> repeat(collection(length)) { scan(depth + 1) }
            5 -> {
                val keys = HashSet<String>()
                repeat(collection(length)) {
                    val key = text()
                    if (!keys.add(key)) malformed()
                    scan(depth + 1)
                }
            }
            else -> malformed()
        }
    }

    private fun text(): String {
        val (major, length) = header()
        if (major != 3) malformed()
        val size = count(length)
        val start = position
        position += size
        return try {
            bytes.decodeToString(start, position, throwOnInvalidSequence = true)
        } catch (_: Exception) {
            malformed()
        }
    }

    private fun header(): Pair<Int, Long> {
        if (position >= bytes.size) malformed()
        val first = bytes[position++].toInt() and 0xff
        val additional = first and 0x1f
        val value = when (additional) {
            in 0..23 -> additional.toLong()
            24 -> unsigned(1)
            25 -> unsigned(2)
            26 -> unsigned(4)
            27 -> unsigned(8)
            else -> malformed()
        }
        return (first ushr 5) to value
    }

    private fun unsigned(count: Int): Long {
        if (position + count > bytes.size) malformed()
        var value = 0L
        repeat(count) {
            if (value > (Long.MAX_VALUE ushr 8)) malformed()
            value = (value shl 8) or (bytes[position++].toLong() and 0xff)
        }
        return value
    }

    private fun advance(length: Long) {
        position += count(length)
    }

    private fun count(length: Long): Int {
        if (length > Int.MAX_VALUE || length > bytes.size - position) malformed()
        return length.toInt()
    }

    private fun collection(length: Long): Int {
        if (length > 300_000) malformed()
        return length.toInt()
    }

    private fun malformed(): Nothing = throw RemoteProtocolException("malformed remote CBOR")
}
