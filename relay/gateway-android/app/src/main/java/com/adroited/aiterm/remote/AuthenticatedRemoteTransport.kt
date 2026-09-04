package com.adroited.aiterm.remote

import android.util.Log
import com.adroited.aiterm.pairing.AuthChallengeFrame
import com.adroited.aiterm.pairing.PairedDesktop
import com.adroited.aiterm.pairing.PairingFrames
import com.adroited.aiterm.pairing.PairingProtocolException
import com.adroited.aiterm.security.DeviceKeys
import com.adroited.aiterm.security.AppLock
import java.util.concurrent.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull

interface RemoteBinarySocket {
    val endpoint: RemoteEndpoint?
        get() = null
    suspend fun receive(): ByteArray
    fun send(bytes: ByteArray): Boolean
    fun close()
}

interface RemoteSocketDialer {
    suspend fun open(desktop: PairedDesktop): RemoteBinarySocket
}

interface DirectRemoteSocketDialer : RemoteSocketDialer {
    suspend fun openDirect(desktop: PairedDesktop, offer: RemoteDirectOffer): RemoteBinarySocket
}

enum class RemotePath { LAN, VPN, RELAY, DIRECT, UNKNOWN }

data class RemoteEndpoint(
    val host: String,
    val port: Int,
    val path: RemotePath = RemotePath.UNKNOWN,
)

/** Authenticated, bounded, request-correlated remote protocol transport. */
class AuthenticatedRemoteTransport(
    private val desktop: PairedDesktop,
    private val deviceKeys: DeviceKeys,
    private val appLock: AppLock,
    private val dialer: RemoteSocketDialer,
    private val scope: CoroutineScope,
    private val dispatcher: CoroutineDispatcher = Dispatchers.IO,
    private val beforeRequestEnqueue: () -> Unit = {},
) : RemoteTransport {
    override val endpoint: RemoteEndpoint?
        get() = synchronized(stateLock) { socket?.endpoint }
    private val eventChannel = Channel<RemoteServerEvent>(
        capacity = MAX_EVENTS,
        onBufferOverflow = BufferOverflow.SUSPEND,
    )
    override val events: Flow<RemoteServerEvent> = eventChannel.receiveAsFlow()
    private val outbound = Channel<OutboundRequest>(MAX_PENDING_REQUESTS)

    private val stateLock = Any()
    private val publicationMutex = Mutex()
    private val pending = LinkedHashMap<Long, PendingRequest>()
    private val acceptedRequests = LinkedHashSet<CompletableDeferred<RemoteResponse>>()
    private val completed = LinkedHashSet<Long>()
    private val heldAttachments = LinkedHashMap<Long, HeldAttachment>()
    private var socket: RemoteBinarySocket? = null
    private var connectingSocket: RemoteBinarySocket? = null
    private var readerJob: Job? = null
    private var writerJob: Job? = null
    private var nextRequestId = 1L
    private var queuedRequests = 0
    private var started = false
    private var closed = false

    override suspend fun connect() {
        synchronized(stateLock) {
            if (started || closed) throw RemoteProtocolException("remote transport is closed")
            started = true
        }
        var candidate = dialer.open(desktop)
        try {
            synchronized(stateLock) {
                if (closed) {
                    candidate.close()
                    throw RemoteProtocolException("remote transport is closed")
                }
                connectingSocket = candidate
            }
            authenticate(candidate)
            val earlyEvents = mutableListOf<RemoteEventEnvelope>()
            val directDialer = dialer as? DirectRemoteSocketDialer
            if (directDialer != null && isRelayEndpoint(candidate.endpoint)) {
                val offer = requestDirectOffer(candidate, earlyEvents)
                nextRequestId = 2L
                if (offer != null) {
                    val direct = attemptDirectUpgrade(
                        directDialer,
                        candidate,
                        offer,
                        earlyEvents,
                    )
                    if (direct != null) {
                        candidate.close()
                        candidate = direct
                        nextRequestId = 1L
                        earlyEvents.clear()
                    }
                }
            }
            ensureOpenAndUnlocked(candidate)
            synchronized(stateLock) {
                if (closed || connectingSocket !== candidate) {
                    throw RemoteProtocolException("remote transport is closed")
                }
                connectingSocket = null
                socket = candidate
            }
            logInfo("remote transport connected over ${candidate.endpoint?.path ?: RemotePath.UNKNOWN}")
            writerJob = scope.launch(dispatcher) { writeLoop() }
            for (event in earlyEvents) accept(event)
            readerJob = scope.launch(dispatcher) { readLoop(candidate) }
        } catch (error: Exception) {
            logWarning("remote transport connection failed", error)
            synchronized(stateLock) {
                if (connectingSocket === candidate) connectingSocket = null
                if (socket === candidate) socket = null
            }
            candidate.close()
            throw error
        }
    }

    /**
     * Keeps consuming the authenticated relay while the optional direct path is negotiated.
     * OkHttp otherwise continues reading into its small socket queue; a busy desktop can fill
     * that queue during a hard NAT timeout and accidentally destroy the healthy fallback.
     */
    private suspend fun attemptDirectUpgrade(
        directDialer: DirectRemoteSocketDialer,
        relay: RemoteBinarySocket,
        offer: RemoteDirectOffer,
        earlyEvents: MutableList<RemoteEventEnvelope>,
    ): RemoteBinarySocket? = coroutineScope {
        val upgrade = async {
            var direct: RemoteBinarySocket? = null
            try {
                val opened = directDialer.openDirect(desktop, offer)
                direct = opened
                synchronized(stateLock) {
                    if (closed || connectingSocket !== relay) {
                        throw RemoteProtocolException("remote transport is closed")
                    }
                    connectingSocket = opened
                }
                authenticate(opened)
                opened
            } catch (cancelled: CancellationException) {
                direct?.close()
                throw cancelled
            } catch (error: Exception) {
                logInfo("direct QUIC unavailable; retaining relay: ${error.message}")
                direct?.close()
                null
            }
        }
        var keepDirect = false
        try {
            var bufferedBytes = earlyEvents.sumOf { it.payload.size.toLong() }
            while (!upgrade.isCompleted) {
                val bytes = withTimeoutOrNull(DIRECT_RELAY_DRAIN_SLICE_MILLIS) {
                    relay.receive()
                } ?: continue
                val event = RemoteWireCodec.decodeEvent(bytes)
                if (event.requestId != 0L) {
                    throw RemoteProtocolException("unexpected response during direct connection setup")
                }
                earlyEvents += event
                bufferedBytes += event.payload.size
                if (earlyEvents.size >= MAX_EARLY_UPGRADE_EVENTS ||
                    bufferedBytes >= MAX_EARLY_UPGRADE_BYTES
                ) {
                    // Preserving the live relay outranks an optimization when the desktop is busy.
                    upgrade.cancelAndJoin()
                    return@coroutineScope null
                }
            }
            upgrade.await().also { keepDirect = it != null }
        } finally {
            if (upgrade.isActive) upgrade.cancelAndJoin()
            if (!keepDirect) {
                synchronized(stateLock) {
                    if (!closed) connectingSocket = relay
                }
            }
        }
    }

    private suspend fun authenticate(candidate: RemoteBinarySocket) {
        val challengeBytes = withTimeout(AUTH_TIMEOUT_MILLIS) { candidate.receive() }
        val challenge = try {
            PairingFrames.decode(challengeBytes) as? AuthChallengeFrame
        } catch (_: PairingProtocolException) {
            null
        } ?: throw RemoteProtocolException("the desktop did not send an authentication challenge")
        // This check is deliberately immediately before the Keystore call.
        // A socket opening while locked must never cause a signature prompt.
        val signature = appLock.signChallengeWhileUnlocked {
            deviceKeys.signChallenge(challenge.nonce)
        } ?: throw RemoteProtocolException("unlock is required before authentication")
        ensureOpenAndUnlocked(candidate)
        val proof = RemoteWireCodec.encodeAuthProof(desktop.deviceId, signature)
        try {
            if (!candidate.send(proof)) throw RemoteProtocolException("authentication proof send failed")
        } finally {
            proof.fill(0)
            signature.fill(0)
            challenge.nonce.fill(0)
        }
        RemoteWireCodec.decodeAuthOk(withTimeout(AUTH_TIMEOUT_MILLIS) { candidate.receive() })
        ensureOpenAndUnlocked(candidate)
    }

    private fun isRelayEndpoint(endpoint: RemoteEndpoint?): Boolean {
        val relayHost = desktop.relayHost ?: return false
        val relayPort = desktop.relayPort ?: return false
        return endpoint?.host?.equals(relayHost, ignoreCase = true) == true && endpoint.port == relayPort
    }

    private suspend fun requestDirectOffer(
        candidate: RemoteBinarySocket,
        earlyEvents: MutableList<RemoteEventEnvelope>,
    ): RemoteDirectOffer? {
        val encoded = RemoteWireCodec.encodeRequest(RemoteRequest(1L, "transport.direct", byteArrayOf()))
        try {
            if (!candidate.send(encoded)) {
                throw RemoteProtocolException("direct connection setup send failed")
            }
        } finally {
            encoded.fill(0)
        }
        return withTimeout(DIRECT_SETUP_TIMEOUT_MILLIS) {
            var complete = false
            var offer: RemoteDirectOffer? = null
            while (!complete) {
                val event = RemoteWireCodec.decodeEvent(candidate.receive())
                if (event.requestId == 1L) {
                    offer = when (event.kind) {
                        "transport.direct" -> RemoteCommands.directOffer(event.payload)
                        "error" -> null
                        else -> throw RemoteProtocolException("invalid direct connection response")
                    }
                    complete = true
                } else if (event.requestId != 0L || earlyEvents.size >= MAX_EARLY_EVENTS) {
                    throw RemoteProtocolException("unexpected response during direct connection setup")
                } else {
                    earlyEvents += event
                }
            }
            offer
        }
    }

    override fun request(
        kind: String,
        payload: ByteArray,
        onAssigned: (Long) -> Unit,
    ): CompletableDeferred<RemoteResponse> {
        return requestBatch(listOf(RemoteRequestInput(kind, payload, onAssigned)))?.single()
            ?: CompletableDeferred<RemoteResponse>().also {
                it.completeExceptionally(RemoteProtocolException("invalid or over-bound remote request"))
            }
    }

    override fun requestBatch(
        requests: List<RemoteRequestInput>,
    ): List<CompletableDeferred<RemoteResponse>>? {
        if (requests.isEmpty() || requests.size > MAX_PENDING_REQUESTS) return null
        val outgoing = requests.map { request ->
            OutboundRequest(
                kind = request.kind,
                payload = request.payload.copyOf(),
                onAssigned = request.onAssigned,
                deferred = CompletableDeferred(),
            )
        }
        beforeRequestEnqueue()
        val sent = ArrayList<OutboundRequest>(outgoing.size)
        val accepted = synchronized(stateLock) {
            if (closed || socket == null ||
                acceptedRequests.size + outgoing.size > MAX_PENDING_REQUESTS
            ) {
                false
            } else {
                var complete = true
                for (request in outgoing) {
                    if (outbound.trySend(request).isSuccess) {
                        sent += request
                    } else {
                        complete = false
                        break
                    }
                }
                if (complete) {
                    queuedRequests += outgoing.size
                    acceptedRequests.addAll(outgoing.map { it.deferred })
                    true
                } else {
                    // The writer cannot pass its state-lock publication point until this block
                    // exits. Any sent siblings remain unaccepted, so the writer drops them.
                    false
                }
            }
        }
        if (!accepted) {
            outgoing.drop(sent.size).forEach { it.payload.fill(0) }
            outgoing.forEach {
                it.deferred.completeExceptionally(
                    RemoteProtocolException("invalid or over-bound remote request"),
                )
            }
            return null
        }
        return outgoing.map { it.deferred }
    }

    override fun abandonRequest(request: Deferred<RemoteResponse>) {
        val abandoned = synchronized(stateLock) {
            val accepted = acceptedRequests.firstOrNull { it === request }
                ?: return@synchronized null
            acceptedRequests.remove(accepted)
            pending.entries.firstOrNull { it.value.deferred === accepted }?.let { (requestId, pendingRequest) ->
                pending.remove(requestId)
                heldAttachments.remove(requestId)
                pendingRequest.timeout?.cancel()
                rememberCompletedLocked(requestId)
            }
            accepted
        }
        abandoned?.cancel()
    }

    private suspend fun writeLoop() {
        for (outgoing in outbound) {
            val prepared = synchronized(stateLock) {
                queuedRequests = (queuedRequests - 1).coerceAtLeast(0)
                val active = socket
                if (!acceptedRequests.contains(outgoing.deferred)) null
                else if (closed || active == null) {
                    acceptedRequests.remove(outgoing.deferred)
                    null
                }
                else {
                    val requestId = nextRequestId++
                    pending[requestId] = PendingRequest(outgoing.kind, outgoing.deferred)
                    if (outgoing.kind == "terminal.attach") heldAttachments[requestId] = HeldAttachment()
                    Triple(active, requestId, RemoteRequest(requestId, outgoing.kind, outgoing.payload))
                }
            }
            if (prepared == null) {
                outgoing.payload.fill(0)
                outgoing.deferred.completeExceptionally(RemoteProtocolException("remote transport is disconnected"))
                continue
            }
            val (active, requestId, request) = prepared
            try {
                outgoing.onAssigned(requestId)
            } catch (_: Exception) {
                failPendingSend(requestId, "remote request assignment callback failed")
                outgoing.payload.fill(0)
                continue
            }
            val stillPending = synchronized(stateLock) {
                pending[requestId]?.deferred === outgoing.deferred && acceptedRequests.contains(outgoing.deferred)
            }
            if (!stillPending) {
                outgoing.payload.fill(0)
                continue
            }
            val encoded = RemoteWireCodec.encodeRequest(request)
            val sent = try {
                active.send(encoded)
            } finally {
                encoded.fill(0)
                outgoing.payload.fill(0)
            }
            if (!sent) {
                failPendingSend(requestId, "remote request send failed")
                continue
            }
            val timeout = scope.launch(dispatcher) {
                delay(REQUEST_TIMEOUT_MILLIS)
                timeoutPending(requestId)
            }
            synchronized(stateLock) {
                val current = pending[requestId]
                if (current == null) timeout.cancel() else current.timeout = timeout
            }
        }
    }

    private fun failPendingSend(requestId: Long, message: String) {
        val request = synchronized(stateLock) {
            heldAttachments.remove(requestId)
            pending.remove(requestId).also { request ->
                request?.let { acceptedRequests.remove(it.deferred) }
            }
        }
        request?.deferred?.completeExceptionally(RemoteProtocolException(message))
    }

    private fun timeoutPending(requestId: Long) {
        val request = synchronized(stateLock) {
            heldAttachments.remove(requestId)
            pending.remove(requestId).also { request ->
                request?.let { acceptedRequests.remove(it.deferred) }
            }
        } ?: return
        rememberCompleted(requestId)
        request.deferred.completeExceptionally(RemoteProtocolException("remote request timed out"))
    }

    override fun close() = closeWithOutcome(null)

    private fun closeWithOutcome(outcome: RemoteTransportTerminalOutcome?) {
        val terminalFailure = outcome?.let(::RemoteTransportTerminatedException)
        val requestFailure = terminalFailure ?: RemoteProtocolException("remote transport disconnected")
        val toFail = synchronized(stateLock) {
            if (closed) return
            closed = true
            outbound.close()
            readerJob?.cancel()
            readerJob = null
            writerJob?.cancel()
            writerJob = null
            connectingSocket?.close()
            connectingSocket = null
            socket?.close()
            socket = null
            pending.values.forEach { it.timeout?.cancel() }
            val accepted = acceptedRequests.toList()
            acceptedRequests.clear()
            pending.clear()
            queuedRequests = 0
            completed.clear()
            heldAttachments.clear()
            accepted
        }
        while (true) {
            val queued = outbound.tryReceive().getOrNull() ?: break
            queued.payload.fill(0)
        }
        // Commit the connection generation's terminal classification before
        // any request waiter can interpret teardown as an ordinary disconnect.
        eventChannel.close(terminalFailure)
        toFail.forEach { it.completeExceptionally(requestFailure) }
    }

    private suspend fun readLoop(active: RemoteBinarySocket) {
        try {
            while (true) accept(RemoteWireCodec.decodeEvent(active.receive()))
        } catch (_: CancellationException) {
            // Explicit close/lock owns teardown.
        } catch (error: Exception) {
            logWarning("remote transport reader ended", error)
            closeWithOutcome(
                RemoteTransportTerminalOutcome.Recoverable(error.message ?: "Connection ended"),
            )
        } finally {
            if (synchronized(stateLock) { !closed }) close()
        }
    }

    private suspend fun accept(event: RemoteEventEnvelope) {
        when (event.kind) {
            "terminal.snapshot", "terminal.diff", "terminal.scrollback" -> {
                publicationMutex.withLock { acceptTerminalEvent(event) }
            }
            "state.snapshot" -> {
                if (event.requestId != 0L) protocolFailure()
                emit(RemoteServerEvent.RosterChunk(RemoteWireCodec.decodeStateSnapshot(event.payload)))
            }
            "auth.revoked" -> {
                if (event.requestId != 0L) protocolFailure()
                closeWithOutcome(RemoteTransportTerminalOutcome.Revoked)
            }
            "error" -> acceptError(event)
            "session.changed", "agent.changed", "tab.changed", "terminal.exited",
            "terminal.title", "terminal.focus_changed" -> {
                requireKnownCorrelation(event.requestId)
                emit(RemoteServerEvent.Raw(event.kind, event.payload))
            }
            else -> acceptResponse(event)
        }
    }

    internal suspend fun acceptEnvelopeForTest(event: RemoteEventEnvelope) = accept(event)

    private fun acceptResponse(event: RemoteEventEnvelope) {
        if (event.requestId <= 0) protocolFailure()
        val request = synchronized(stateLock) {
            pending.remove(event.requestId).also { request ->
                request?.let { acceptedRequests.remove(it.deferred) }
            }
        }
        if (request == null) {
            if (synchronized(stateLock) { completed.contains(event.requestId) }) return
            protocolFailure()
        }
        if (event.kind != request.kind) protocolFailure()
        request.timeout?.cancel()
        rememberCompleted(event.requestId)
        request.deferred.complete(RemoteResponse.Success(event.requestId, event.kind, event.payload))
    }

    private fun completeTransferOnlyRequest(event: RemoteEventEnvelope) {
        val request = synchronized(stateLock) {
            pending.remove(event.requestId).also { request ->
                request?.let { acceptedRequests.remove(it.deferred) }
            }
        }
        if (request == null) {
            if (synchronized(stateLock) { completed.contains(event.requestId) }) return
            protocolFailure()
        }
        if (request.kind != "terminal.scrollback") protocolFailure()
        request.timeout?.cancel()
        rememberCompleted(event.requestId)
        request.deferred.complete(RemoteResponse.Success(event.requestId, request.kind, event.payload))
    }

    private suspend fun acceptError(event: RemoteEventEnvelope) {
        val error = RemoteWireCodec.decodeError(event.payload)
        if (event.requestId == 0L) {
            emit(RemoteServerEvent.Failure(error.code, error.message))
            return
        }
        val request = synchronized(stateLock) {
            pending.remove(event.requestId).also { request ->
                request?.let { acceptedRequests.remove(it.deferred) }
            }
        }
        if (request == null) {
            if (synchronized(stateLock) { completed.contains(event.requestId) }) return
            protocolFailure()
        }
        synchronized(stateLock) { heldAttachments.remove(event.requestId) }
        request.timeout?.cancel()
        rememberCompleted(event.requestId)
        request.deferred.complete(RemoteResponse.Error(event.requestId, error.code, error.message))
    }

    private fun requireKnownCorrelation(requestId: Long) {
        if (requestId == 0L) return
        val known = synchronized(stateLock) {
            pending.containsKey(requestId) || completed.contains(requestId) || heldAttachments.containsKey(requestId)
        }
        if (!known) protocolFailure()
    }

    private fun rememberCompleted(requestId: Long) = synchronized(stateLock) {
        rememberCompletedLocked(requestId)
    }

    private fun rememberCompletedLocked(requestId: Long) {
        completed += requestId
        while (completed.size > MAX_COMPLETED_CORRELATIONS) {
            completed.remove(completed.first())
        }
    }

    private suspend fun emit(event: RemoteServerEvent) = eventChannel.send(event)

    override suspend fun completeAttachment(requestId: Long, publishEvents: Boolean) {
        publicationMutex.withLock {
            val held = synchronized(stateLock) { heldAttachments[requestId] } ?: return@withLock
            held.decision = if (publishEvents) AttachmentDecision.Publish else AttachmentDecision.Discard
            val frames = synchronized(stateLock) {
                held.frames.toList().also {
                    held.frames.clear()
                    held.bytes = 0
                }
            }
            if (publishEvents) frames.forEach { emitTerminalEvent(it) }
            if (held.complete) synchronized(stateLock) { heldAttachments.remove(requestId) }
        }
    }

    private suspend fun acceptTerminalEvent(event: RemoteEventEnvelope) {
        requireKnownCorrelation(event.requestId)
        val chunk = RemoteWireCodec.decodeTerminalChunk(event.payload, event.requestId)
        val held = synchronized(stateLock) { heldAttachments[event.requestId] }
        if (held == null) {
            emitTerminalEvent(event, chunk)
            return
        }
        val complete = chunk.index + 1 == chunk.total
        held.complete = held.complete || complete
        when (held.decision) {
            AttachmentDecision.Pending -> synchronized(stateLock) {
                if (held.frames.size >= MAX_HELD_ATTACH_FRAMES ||
                    held.bytes + event.payload.size > MAX_HELD_ATTACH_BYTES
                ) protocolFailure()
                held.frames += event
                held.bytes += event.payload.size
            }
            AttachmentDecision.Publish -> emitTerminalEvent(event, chunk)
            AttachmentDecision.Discard -> Unit
        }
        if (complete && held.decision != AttachmentDecision.Pending) {
            synchronized(stateLock) { heldAttachments.remove(event.requestId) }
        }
    }

    private suspend fun emitTerminalEvent(
        event: RemoteEventEnvelope,
        chunk: TerminalTransferChunk = RemoteWireCodec.decodeTerminalChunk(event.payload, event.requestId),
    ) {
        emit(RemoteServerEvent.TerminalChunk(chunk))
        if (chunk.kind == TerminalTransferKind.Scrollback && chunk.index + 1 == chunk.total &&
            event.requestId > 0
        ) completeTransferOnlyRequest(event)
    }

    private fun ensureOpenAndUnlocked(candidate: RemoteBinarySocket) {
        if (appLock.isLocked.value || synchronized(stateLock) { closed || connectingSocket !== candidate }) {
            throw RemoteProtocolException("unlock is required before authentication")
        }
    }

    private fun protocolFailure(): Nothing = throw RemoteProtocolException("uncorrelated remote response")

    private fun logInfo(message: String) {
        // android.util.Log is unavailable to the plain JVM unit-test runtime.
        runCatching { Log.i(LOG_TAG, message) }
    }

    private fun logWarning(message: String, error: Throwable) {
        runCatching { Log.w(LOG_TAG, message, error) }
    }

    private data class PendingRequest(
        val kind: String,
        val deferred: CompletableDeferred<RemoteResponse>,
        var timeout: Job? = null,
    )
    private data class OutboundRequest(
        val kind: String,
        val payload: ByteArray,
        val onAssigned: (Long) -> Unit,
        val deferred: CompletableDeferred<RemoteResponse>,
    )
    private data class HeldAttachment(
        val frames: MutableList<RemoteEventEnvelope> = mutableListOf(),
        var bytes: Int = 0,
        var complete: Boolean = false,
        var decision: AttachmentDecision = AttachmentDecision.Pending,
    )
    private enum class AttachmentDecision { Pending, Publish, Discard }

    private companion object {
        const val LOG_TAG = "AITermRemote"
        const val AUTH_TIMEOUT_MILLIS = 10_000L
        const val DIRECT_SETUP_TIMEOUT_MILLIS = 8_000L
        const val DIRECT_RELAY_DRAIN_SLICE_MILLIS = 50L
        // The desktop bounds descriptor-safe session work at 120 seconds.
        // Keep a small transport grace period so its correlated result wins
        // rather than reconnecting while a protected delete is still active.
        const val REQUEST_TIMEOUT_MILLIS = 130_000L
        const val MAX_PENDING_REQUESTS = 64
        const val MAX_COMPLETED_CORRELATIONS = 64
        const val MAX_EVENTS = 64
        const val MAX_EARLY_EVENTS = 16
        const val MAX_EARLY_UPGRADE_EVENTS = 48
        const val MAX_EARLY_UPGRADE_BYTES = 8L * 1_024 * 1_024
        const val MAX_HELD_ATTACH_FRAMES = 512
        const val MAX_HELD_ATTACH_BYTES = 8 * 1_024 * 1_024
    }
}
