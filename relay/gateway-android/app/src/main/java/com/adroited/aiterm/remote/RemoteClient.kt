package com.adroited.aiterm.remote

import com.adroited.aiterm.terminal.TerminalScreenStore
import com.adroited.aiterm.terminal.ApplyResult
import com.adroited.aiterm.terminal.ScreenRow
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.Serializable
import java.io.File
import java.io.FileInputStream
import java.util.UUID

@Serializable
data class TerminalSize(val cols: Int, val rows: Int) {
    init {
        require(cols in 1..512 && rows in 1..512)
    }
}

enum class FocusOwner { Self, Other, Unowned }
enum class ConnectionState { Disconnected, Connecting, Connected, Reconnecting, Locked, Revoked }

data class RemoteClientState(
    val connection: ConnectionState = ConnectionState.Disconnected,
    val focus: FocusOwner = FocusOwner.Unowned,
    val readOnly: Boolean = true,
    val showTakeFocus: Boolean = false,
    val pendingTransfers: Int = 0,
    val tabs: List<RemoteTab> = emptyList(),
    val sessions: List<RemoteSession> = emptyList(),
    val sessionsWithFiles: Set<String> = emptySet(),
    val starredSessions: Set<String> = emptySet(),
    val broughtInSessions: Map<String, String> = emptyMap(),
    val sessionActivity: Map<String, String> = emptyMap(),
    val usage: List<RemoteUsageSource> = emptyList(),
    val previewSessionId: String? = null,
    val previewMessages: List<RemotePreviewMessage> = emptyList(),
    val previewLoadingSessionId: String? = null,
    val previewError: String? = null,
    val agents: List<RemoteAgentChoice> = emptyList(),
    val agentCaps: Map<String, RemoteAgentCaps> = emptyMap(),
    val activeTabId: String? = null,
    val activeTitle: String? = null,
    val lastError: String? = null,
    val connectedEndpoint: RemoteEndpoint? = null,
)

data class RemoteRequest(val requestId: Long, val kind: String, val payload: ByteArray)

data class RemoteRequestInput(
    val kind: String,
    val payload: ByteArray,
    val onAssigned: (Long) -> Unit = {},
)

data class RemoteUploadSource(
    val id: String,
    val file: File,
    val length: Long,
    val sha256: ByteArray,
)

data class RemoteUploadProgress(val sourceId: String, val sent: Long, val total: Long)

internal data class RemoteUploadSubmission(val count: Int, val bytes: Long)

internal fun validateRemoteUploadSources(sources: List<RemoteUploadSource>): RemoteUploadSubmission {
    if (sources.isEmpty() || sources.size > RemoteCommands.MAX_UPLOADS_PER_SUBMISSION) {
        throw RemoteUploadException(null, "choose between one and four images")
    }
    val ids = hashSetOf<String>()
    var total = 0L
    for (source in sources) {
        if (source.id.isBlank() || source.id.encodeToByteArray().size > RemoteCommands.MAX_IDENTIFIER_BYTES ||
            !ids.add(source.id) || !source.file.isFile || source.file.length() != source.length ||
            source.length !in 1..RemoteCommands.MAX_UPLOAD_BYTES || source.sha256.size != 32
        ) {
            throw RemoteUploadException(null, "the selected image is invalid or changed")
        }
        total += source.length
        if (total > RemoteCommands.MAX_SUBMISSION_BYTES) {
            throw RemoteUploadException(null, "selected images exceed the 48 MiB upload limit")
        }
    }
    return RemoteUploadSubmission(sources.size, total)
}

class RemoteUploadException(val code: String?, message: String, cause: Throwable? = null) :
    Exception(message, cause)

sealed interface RemoteResponse {
    val requestId: Long
    data class Success(
        override val requestId: Long,
        val kind: String,
        val payload: ByteArray,
    ) : RemoteResponse
    data class Error(
        override val requestId: Long,
        val code: String,
        val message: String,
    ) : RemoteResponse
}

sealed interface RemoteServerEvent {
    data class FocusChanged(
        val tabId: String,
        val attachmentId: String,
        val focus: FocusOwner,
        val size: TerminalSize,
    ) : RemoteServerEvent
    data class TransferStarted(val transferId: String) : RemoteServerEvent
    data class TransferFinished(val transferId: String) : RemoteServerEvent
    data class TerminalChunk(val chunk: TerminalTransferChunk) : RemoteServerEvent
    data class RosterChunk(val chunk: StateSnapshotChunk) : RemoteServerEvent
    data class Raw(val kind: String, val payload: ByteArray) : RemoteServerEvent
    data class Failure(val code: String, val message: String) : RemoteServerEvent
    data object Revoked : RemoteServerEvent
}

sealed interface RemoteTransportTerminalOutcome {
    data class Recoverable(val message: String) : RemoteTransportTerminalOutcome
    data object Revoked : RemoteTransportTerminalOutcome
}

class RemoteTransportTerminatedException(
    val outcome: RemoteTransportTerminalOutcome,
) : Exception(
    when (outcome) {
        is RemoteTransportTerminalOutcome.Recoverable -> outcome.message
        RemoteTransportTerminalOutcome.Revoked -> "remote access was revoked"
    },
)

interface RemoteTransport {
    val events: Flow<RemoteServerEvent>
    val endpoint: RemoteEndpoint?
        get() = null
    suspend fun connect()
    /** Enqueues in caller order; the transport assigns the wire id when its writer dequeues it. */
    fun request(
        kind: String,
        payload: ByteArray,
        onAssigned: (Long) -> Unit = {},
    ): Deferred<RemoteResponse>
    /** Atomically reserves queue capacity and enqueues every request in caller order, or none. */
    fun requestBatch(requests: List<RemoteRequestInput>): List<Deferred<RemoteResponse>>?
    /** Stops tracking an unanswered request when its caller no longer owns the result. */
    fun abandonRequest(request: Deferred<RemoteResponse>) = Unit
    suspend fun completeAttachment(requestId: Long, publishEvents: Boolean) = Unit
    fun close()
}

class RemoteClient(
    private val transportFactory: () -> RemoteTransport,
    private val screenStore: TerminalScreenStore,
    private val isUnlocked: () -> Boolean,
    private val scope: CoroutineScope,
    private val dispatcher: CoroutineDispatcher = Dispatchers.IO,
) {
    private val mutableState = MutableStateFlow(RemoteClientState())
    val state: StateFlow<RemoteClientState> = mutableState.asStateFlow()
    val screen = screenStore.screen
    private val mutableScrollback = MutableStateFlow<List<ScreenRow>>(emptyList())
    val scrollback: StateFlow<List<ScreenRow>> = mutableScrollback.asStateFlow()
    private val lifecycleLock = Any()
    private val selectionMutex = Mutex()
    private val terminalSubmissionMutex = Mutex()
    private val transfers = linkedSetOf<String>()
    private val terminalAssembler = TerminalTransferAssembler()
    private val rosterAssembler = RosterTransferAssembler()
    private var transport: RemoteTransport? = null
    private var eventJob: Job? = null
    private var reconnectJob: Job? = null
    private var recoveryRequested = false
    private var scrollbackRequest: ScrollbackRequest? = null
    private var activeAttachmentId: String? = null
    private var activeAttachmentTabId: String? = null
    private var lifecycleGeneration = 0L
    private var selectionGeneration = 0L
    private val ownedJobs = linkedSetOf<Job>()

    suspend fun connect(): Boolean {
        synchronized(lifecycleLock) {
            reconnectJob?.cancel()
            reconnectJob = null
        }
        val connected = connectOnce(ConnectionState.Connecting)
        if (!connected && synchronized(lifecycleLock) {
                mutableState.value.connection == ConnectionState.Disconnected && isUnlocked()
            }
        ) {
            synchronized(lifecycleLock) {
                mutableState.value = mutableState.value.copy(connection = ConnectionState.Reconnecting)
            }
            scheduleReconnect()
        }
        return connected
    }

    private suspend fun connectOnce(connectingState: ConnectionState): Boolean {
        if (!isUnlocked()) {
            lock()
            return false
        }
        val candidate = transportFactory()
        val generation = beginConnection(candidate, connectingState)
        return try {
            candidate.connect()
            val selectedTab = synchronized(lifecycleLock) {
                if (!isCurrent(generation, candidate) || !isUnlocked()) return@synchronized null
                mutableState.value = mutableState.value.copy(
                    connection = ConnectionState.Connected,
                    connectedEndpoint = candidate.endpoint,
                )
                mutableState.value.activeTabId
            }
            if (!isCurrent(generation, candidate) || !isUnlocked()) {
                candidate.close()
                return false
            }
            val collector = scope.launch(dispatcher, start = CoroutineStart.LAZY) {
                try {
                    candidate.events.collect { event -> accept(generation, event, candidate) }
                    acceptTerminalOutcome(
                        generation,
                        candidate,
                        RemoteTransportTerminalOutcome.Recoverable("Connection ended"),
                    )
                } catch (error: kotlinx.coroutines.CancellationException) {
                    throw error
                } catch (error: RemoteTransportTerminatedException) {
                    acceptTerminalOutcome(generation, candidate, error.outcome)
                } catch (error: Exception) {
                    acceptTerminalOutcome(
                        generation,
                        candidate,
                        RemoteTransportTerminalOutcome.Recoverable(error.message ?: "Connection ended"),
                    )
                }
            }
            val published = synchronized(lifecycleLock) {
                if (!isCurrent(generation, candidate)) {
                    collector.cancel()
                    false
                } else {
                    eventJob = collector
                    ownedJobs += collector
                    collector.invokeOnCompletion { synchronized(lifecycleLock) { ownedJobs -= collector } }
                    collector.start()
                    true
                }
            }
            if (!published) return false
            selectedTab?.let(::selectTab)
            if (synchronized(lifecycleLock) { mutableState.value.sessions.isNotEmpty() }) refreshSessions()
            synchronized(lifecycleLock) { isCurrent(generation, candidate) }
        } catch (error: Exception) {
            candidate.close()
            if (error is RemoteAccessRevokedException) {
                accept(generation, RemoteServerEvent.Revoked, candidate)
                return false
            }
            synchronized(lifecycleLock) {
                if (isCurrent(generation, candidate)) {
                    transport = null
                    mutableState.value = mutableState.value.copy(
                        connection = if (connectingState == ConnectionState.Reconnecting) {
                            ConnectionState.Reconnecting
                        } else {
                            ConnectionState.Disconnected
                        },
                        lastError = error.message ?: "Connection failed",
                        connectedEndpoint = null,
                    )
                }
            }
            false
        }
    }

    fun sendInput(text: String): Boolean {
        val target = synchronized(lifecycleLock) {
            if (text.isEmpty() || mutableState.value.focus != FocusOwner.Self) {
                mutableState.value = mutableState.value.copy(readOnly = true, showTakeFocus = true)
                return false
            }
            val tabId = mutableState.value.activeTabId ?: return false
            val attachmentId = activeAttachmentId ?: return false
            if (activeAttachmentTabId != tabId) return false
            tabId to attachmentId
        }
        val data = text.encodeToByteArray()
        if (data.size > MAX_INPUT_BYTES) return false
        launchRequest("terminal.input", RemoteCommands.input(target.first, target.second, data))
        return true
    }

    /**
     * Reserves one bounded transport batch for an ordered terminal submission. Success means all
     * sibling inputs were accepted locally for the same authorized terminal attachment.
     */
    fun sendInputs(tabId: String, texts: List<String>): Boolean {
        if (tabId.isBlank()) return false
        if (texts.isEmpty()) return false
        val encoded = texts.map { text ->
            if (text.isEmpty()) return false
            text.encodeToByteArray().also { if (it.size > MAX_INPUT_BYTES) return false }
        }
        val batch = synchronized(lifecycleLock) {
            if (mutableState.value.focus != FocusOwner.Self) {
                mutableState.value = mutableState.value.copy(readOnly = true, showTakeFocus = true)
                return false
            }
            if (mutableState.value.activeTabId != tabId) return false
            val attachmentId = activeAttachmentId ?: return false
            if (activeAttachmentTabId != tabId) return false
            val active = transport ?: return false
            val generation = lifecycleGeneration
            val responses = active.requestBatch(
                encoded.map { data ->
                    RemoteRequestInput(
                        kind = "terminal.input",
                        payload = RemoteCommands.input(tabId, attachmentId, data),
                    )
                },
            ) ?: return false
            RequestBatchContext(generation, active, responses)
        }
        batch.responses.forEach { response ->
            observeAcceptedResponse(batch.lifecycleGeneration, batch.transport, response)
        }
        return true
    }

    /**
     * Sends a composed terminal submission one part at a time, waiting for the desktop to accept
     * the pasted text before sending Enter. Interactive TUIs may process a paste asynchronously;
     * pacing the submit key prevents it from being consumed before the paste becomes editable.
     */
    suspend fun submitInputs(tabId: String, texts: List<String>): Boolean {
        if (tabId.isBlank() || texts.isEmpty()) return false
        val encoded = texts.map { text ->
            if (text.isEmpty()) return false
            text.encodeToByteArray().also { if (it.size > MAX_INPUT_BYTES) return false }
        }
        return terminalSubmissionMutex.withLock {
            encoded.forEachIndexed { index, data ->
                val input = synchronized(lifecycleLock) {
                    if (mutableState.value.focus != FocusOwner.Self) {
                        mutableState.value = mutableState.value.copy(readOnly = true, showTakeFocus = true)
                        return@withLock false
                    }
                    if (mutableState.value.activeTabId != tabId) return@withLock false
                    val attachmentId = activeAttachmentId ?: return@withLock false
                    if (activeAttachmentTabId != tabId) return@withLock false
                    val active = transport ?: return@withLock false
                    TerminalInputContext(lifecycleGeneration, active, attachmentId)
                }
                val response = input.transport.request(
                    "terminal.input",
                    RemoteCommands.input(tabId, input.attachmentId, data),
                ).await()
                val accepted = response is RemoteResponse.Success && response.kind == "terminal.input"
                if (!accepted || synchronized(lifecycleLock) {
                        !isCurrent(input.lifecycleGeneration, input.transport) ||
                            mutableState.value.activeTabId != tabId ||
                            activeAttachmentTabId != tabId ||
                            activeAttachmentId != input.attachmentId
                    }
                ) {
                    return@withLock false
                }
                if (index < encoded.lastIndex) delay(TERMINAL_SUBMIT_SETTLE_MILLIS)
            }
            true
        }
    }

    /**
     * Stages normalized local images on the active desktop terminal without writing terminal input.
     * The caller owns prompt submission and local draft-file deletion after this succeeds.
     */
    suspend fun uploadImages(
        expectedTabId: String,
        sources: List<RemoteUploadSource>,
        onProgress: (RemoteUploadProgress) -> Unit = {},
    ): Result<List<String>> = withContext(dispatcher) {
        var context = synchronized(lifecycleLock) { activeUploadContext() }
            ?: return@withContext Result.failure(RemoteUploadException(null, "terminal focus is required to upload images"))
        if (expectedTabId.isBlank() || context.tabId != expectedTabId) {
            return@withContext Result.failure(
                RemoteUploadException(null, "terminal tab changed before images could upload"),
            )
        }
        val begunUploadIds = linkedSetOf<String>()
        try {
            val submission = validateRemoteUploadSources(sources)
            val submissionId = UUID.randomUUID().toString()
            val paths = ArrayList<String>(sources.size)

            for ((memberIndex, source) in sources.withIndex()) {
                image@ while (true) {
                    val began = try {
                        val beganPayload = requestUpload(
                            context,
                            "terminal.upload.begin",
                            RemoteCommands.uploadBegin(
                                tabId = context.tabId,
                                attachmentId = context.attachmentId,
                                submissionId = submissionId,
                                submissionCount = submission.count,
                                memberIndex = memberIndex,
                                submissionBytes = submission.bytes,
                                length = source.length,
                                sha256 = source.sha256,
                            ),
                        )
                        RemoteCommands.uploadBegan(beganPayload)
                    } catch (error: Exception) {
                        context = resumeUploadContext(expectedTabId, context, error) ?: throw error
                        continue@image
                    }
                    begunUploadIds += began.uploadId
                    began.path?.let {
                        paths += it
                        onProgress(RemoteUploadProgress(source.id, source.length, source.length))
                        break@image
                    }

                    val maximumChunk = ((source.length + RemoteCommands.MAX_UPLOAD_CHUNK_BYTES - 1) /
                        RemoteCommands.MAX_UPLOAD_CHUNK_BYTES).toInt()
                    if (began.nextChunk !in 0..maximumChunk) {
                        throw RemoteProtocolException("desktop returned an invalid upload resume offset")
                    }
                    var sent = minOf(
                        source.length,
                        began.nextChunk.toLong() * RemoteCommands.MAX_UPLOAD_CHUNK_BYTES,
                    )
                    onProgress(RemoteUploadProgress(source.id, sent, source.length))

                    try {
                        FileInputStream(source.file).use { input ->
                            input.channel.position(sent)
                            val buffer = ByteArray(RemoteCommands.MAX_UPLOAD_CHUNK_BYTES)
                            var index = began.nextChunk
                            while (sent < source.length) {
                                requireCurrentUploadContext(context)
                                val requested = minOf(buffer.size.toLong(), source.length - sent).toInt()
                                val count = input.read(buffer, 0, requested)
                                if (count <= 0) {
                                    throw RemoteUploadException(null, "the selected image changed while it was uploading")
                                }
                                val chunkPayload = requestUpload(
                                    context,
                                    "terminal.upload.chunk",
                                    RemoteCommands.uploadChunk(began.uploadId, index, buffer.copyOf(count)),
                                )
                                RemoteCommands.uploadAcknowledged(chunkPayload)
                                sent += count
                                index += 1
                                onProgress(RemoteUploadProgress(source.id, sent, source.length))
                            }
                            if (input.read() != -1) {
                                throw RemoteUploadException(null, "the selected image changed while it was uploading")
                            }
                        }

                        val finishedPayload = requestUpload(
                            context,
                            "terminal.upload.finish",
                            RemoteCommands.uploadFinish(began.uploadId),
                        )
                        paths += RemoteCommands.uploadedPath(finishedPayload)
                        break@image
                    } catch (error: Exception) {
                        context = resumeUploadContext(expectedTabId, context, error) ?: throw error
                    }
                }
            }
            Result.success(paths)
        } catch (error: kotlinx.coroutines.CancellationException) {
            // A caller cancellation must leave its local draft intact, but should still stop desktop staging.
            withContext(NonCancellable) { cancelBegunUploads(context, begunUploadIds) }
            throw error
        } catch (error: Exception) {
            withContext(NonCancellable) { cancelBegunUploads(context, begunUploadIds) }
            Result.failure(error)
        }
    }

    fun selectTab(tabId: String) {
        val selection = synchronized(lifecycleLock) {
            if (tabId == mutableState.value.activeTabId && activeAttachmentId != null &&
                activeAttachmentTabId == tabId
            ) return
            val active = transport ?: return
            selectionGeneration += 1
            val previous = activeAttachmentTabId?.let { previousTab ->
                activeAttachmentId?.let { previousTab to it }
            }
            activeAttachmentId = null
            activeAttachmentTabId = null
            screenStore.clear()
            mutableScrollback.value = emptyList()
            scrollbackRequest = null
            terminalAssembler.clear()
            recoveryRequested = false
            mutableState.value = mutableState.value.copy(
                activeTabId = tabId,
                activeTitle = null,
                focus = FocusOwner.Unowned,
                readOnly = true,
                showTakeFocus = false,
                pendingTransfers = 0,
            )
            Selection(lifecycleGeneration, selectionGeneration, tabId, active, previous)
        }
        launchOwned(selection.lifecycleGeneration) {
            try {
                selectionMutex.withLock { runSelection(selection) }
            } catch (error: kotlinx.coroutines.CancellationException) {
                throw error
            } catch (error: Exception) {
                acceptRequestFailure(
                    selection.lifecycleGeneration,
                    selection.transport,
                    error,
                )
            }
        }
    }

    fun takeFocus(size: TerminalSize): Boolean {
        val (tabId, attachmentId) = synchronized(lifecycleLock) { activeTarget() } ?: return false
        launchRequest("terminal.focus", RemoteCommands.sized(tabId, attachmentId, size))
        return true
    }

    fun resize(size: TerminalSize): Boolean {
        val (tabId, attachmentId) = synchronized(lifecycleLock) {
            if (mutableState.value.focus != FocusOwner.Self) return false
            activeTarget()
        } ?: return false
        launchRequest("terminal.resize", RemoteCommands.sized(tabId, attachmentId, size))
        return true
    }

    fun requestScrollback(offset: Int, count: Int): Boolean {
        if (offset < 0 || count !in 1..512) return false
        val context = synchronized(lifecycleLock) {
            if (scrollbackRequest != null || offset != mutableScrollback.value.size) return false
            val (tabId, attachmentId) = activeTarget() ?: return false
            val active = transport ?: return false
            ScrollbackRequest(lifecycleGeneration, active, tabId, attachmentId, offset).also {
                scrollbackRequest = it
            }
        }
        val response = context.transport.request(
            "terminal.scrollback",
            RemoteCommands.scrollback(context.tabId, context.attachmentId, offset, count),
        ) { requestId ->
            synchronized(lifecycleLock) {
                if (scrollbackRequest === context) context.requestId = requestId
            }
        }
        val launched = launchOwned(context.lifecycleGeneration) {
            try {
                when (val result = response.await()) {
                    is RemoteResponse.Error -> {
                        synchronized(lifecycleLock) {
                            if (scrollbackRequest === context) scrollbackRequest = null
                        }
                        accept(
                            context.lifecycleGeneration,
                            RemoteServerEvent.Failure(result.code, result.message),
                            context.transport,
                        )
                    }
                    is RemoteResponse.Success -> Unit
                }
            } catch (error: kotlinx.coroutines.CancellationException) {
                throw error
            } catch (error: Exception) {
                synchronized(lifecycleLock) {
                    if (scrollbackRequest === context) scrollbackRequest = null
                }
                acceptRequestFailure(
                    context.lifecycleGeneration,
                    context.transport,
                    error,
                )
            }
        }
        if (!launched) {
            response.cancel()
            synchronized(lifecycleLock) { if (scrollbackRequest === context) scrollbackRequest = null }
            return false
        }
        return true
    }

    fun requestNextScrollbackPage(count: Int = 128): Boolean =
        requestScrollback(mutableScrollback.value.size, count)

    fun refreshSessions() {
        launchRequest("session.roster", byteArrayOf()) { payload ->
            val roster = RemoteCommands.sessionRoster(payload)
            mutableState.value = mutableState.value.copy(
                sessions = roster.sessions,
                sessionsWithFiles = roster.withFiles,
                starredSessions = roster.stars,
                broughtInSessions = roster.broughtIn,
                sessionActivity = roster.activity,
            )
        }
    }

    fun refreshUsage() {
        launchRequest("usage.report", byteArrayOf()) { payload ->
            mutableState.value = mutableState.value.copy(usage = RemoteCommands.usage(payload))
        }
    }

    fun starSession(sessionId: String, on: Boolean) {
        launchRequest("session.star", RemoteCommands.starSession(sessionId, on)) { refreshSessions() }
    }

    fun renameSession(sessionId: String, title: String) {
        launchRequest("session.rename", RemoteCommands.renameSession(sessionId, title)) { refreshSessions() }
    }

    fun bringInSession(
        sessionId: String,
        agentId: String,
        model: String?,
        effort: String?,
        focus: String,
        rounds: Int,
        auto: Boolean,
    ) {
        val request = {
            launchRequest(
                "session.bring_in",
                RemoteCommands.bringInSession(sessionId, agentId, model, effort, focus, rounds, auto),
            ) { refreshSessions() }
        }
        if (mutableState.value.tabs.any { it.sessionId == sessionId }) {
            request()
        } else {
            launchRequest("session.open", RemoteCommands.openSession(sessionId, TerminalSize(80, 24))) { payload ->
                selectTab(RemoteCommands.openedSessionTab(payload))
                request()
            }
        }
    }

    suspend fun gatewayRoutes(): RemoteGatewayRoutes =
        RemoteCommands.gatewayRoutes(requestResource("gateway.routes", byteArrayOf()))

    fun refreshTabs() {
        launchRequest("tab.list", byteArrayOf()) { payload ->
            mutableState.value = mutableState.value.copy(tabs = RemoteCommands.tabs(payload))
        }
    }

    fun refreshAgents() {
        launchRequest("agent.list", byteArrayOf()) { payload ->
            val roster = RemoteCommands.agents(payload)
            mutableState.value = mutableState.value.copy(agents = roster.agents, agentCaps = roster.caps)
        }
    }

    fun openSession(sessionId: String, size: TerminalSize) {
        launchRequest("session.open", RemoteCommands.openSession(sessionId, size)) { payload ->
            selectTab(RemoteCommands.openedSessionTab(payload))
        }
    }

    fun previewSession(sessionId: String) {
        synchronized(lifecycleLock) {
            if (mutableState.value.previewLoadingSessionId == sessionId) return
            mutableState.value = mutableState.value.copy(
                previewLoadingSessionId = sessionId,
                previewError = null,
            )
        }
        val started = launchRequest(
            "session.conversation",
            RemoteCommands.conversation(sessionId),
            onError = { _, message ->
                mutableState.value = mutableState.value.copy(
                    previewLoadingSessionId = null,
                    previewError = message,
                )
            },
            onSuccess = { payload ->
                mutableState.value = mutableState.value.copy(
                    previewSessionId = sessionId,
                    previewMessages = RemoteCommands.sessionPreview(payload),
                    previewLoadingSessionId = null,
                    previewError = null,
                )
            },
        )
        if (!started) {
            synchronized(lifecycleLock) {
                if (mutableState.value.previewLoadingSessionId == sessionId) {
                    mutableState.value = mutableState.value.copy(
                        previewLoadingSessionId = null,
                        previewError = if (mutableState.value.connection == ConnectionState.Connected) {
                            "Conversation refresh is busy. Retrying…"
                        } else {
                            "The desktop is disconnected."
                        },
                    )
                }
            }
        }
    }

    suspend fun sessionChanges(sessionId: String): List<RemoteSessionChange> =
        RemoteCommands.sessionChanges(requestResource("session.changes", RemoteCommands.session(sessionId)))

    suspend fun webPreview(sessionId: String, open: Boolean): RemoteWebPreview =
        RemoteCommands.webPreview(
            requestResource("session.web_preview", RemoteCommands.webPreview(sessionId, open)),
        )

    suspend fun readFileChunk(
        sessionId: String,
        path: String,
        offset: Long,
        count: Int = RemoteCommands.MAX_UPLOAD_CHUNK_BYTES,
    ): RemoteFileChunk = RemoteCommands.fileChunk(
        requestResource("file.read", RemoteCommands.fileRead(sessionId, path, offset, count)),
    )

    fun closeSession(sessionId: String) {
        val tabId = synchronized(lifecycleLock) {
            mutableState.value.tabs.singleOrNull { it.sessionId == sessionId }?.id
        }
        launchRequest("session.close", RemoteCommands.closeSession(sessionId, tabId)) {
            refreshSessions()
            refreshTabs()
        }
    }

    fun deleteSession(sessionId: String) = sessionMutation("session.delete", sessionId)
    fun forkSession(sessionId: String) = sessionMutation("session.fork", sessionId)
    fun stopSession(sessionId: String) = sessionMutation("session.stop", sessionId)

    fun closeTab(tabId: String) {
        launchRequest("tab.close", RemoteCommands.tab(tabId))
    }

    fun openShell(projectPath: String?, size: TerminalSize) {
        launchRequest("tab.open", RemoteCommands.shell(projectPath, null, size)) { payload ->
            selectTab(RemoteCommands.openedTab(payload))
        }
    }

    fun startAgent(
        agent: RemoteAgentChoice,
        modelId: String?,
        effort: String?,
        cwd: String,
        size: TerminalSize,
    ) {
        val model = modelId?.let { selected -> agent.models.singleOrNull { it.id == selected } }
        if (modelId != null && model == null) return
        if (effort != null && model?.efforts?.contains(effort) != true) return
        launchRequest(
            "agent.action",
            RemoteCommands.startAgent(
                agentId = agent.id,
                model = model?.id,
                effort = effort ?: model?.defaultEffort,
                cwd = cwd,
                title = agent.displayName,
                size = size,
            ),
        ) { payload -> selectTab(RemoteCommands.startedAgentTab(payload)) }
    }

    private fun sessionMutation(kind: String, sessionId: String) {
        launchRequest(kind, RemoteCommands.session(sessionId)) { refreshSessions() }
    }

    fun lock() {
        synchronized(lifecycleLock) {
            reconnectJob?.cancel()
            reconnectJob = null
        }
        closeTransport()
        synchronized(lifecycleLock) {
            transfers.clear()
            terminalAssembler.clear()
            rosterAssembler.clear()
            recoveryRequested = false
            activeAttachmentId = null
            activeAttachmentTabId = null
            screenStore.clear()
            mutableScrollback.value = emptyList()
            scrollbackRequest = null
            mutableState.value = RemoteClientState(connection = ConnectionState.Locked)
        }
    }

    internal fun acceptForTest(event: RemoteServerEvent) = accept(null, event)

    private fun acceptTerminalOutcome(
        expectedGeneration: Long,
        candidate: RemoteTransport,
        outcome: RemoteTransportTerminalOutcome,
    ) {
        var closing: ClosingTransport? = null
        var reconnect = false
        synchronized(lifecycleLock) {
            if (!isCurrent(expectedGeneration, candidate)) return
            reconnectJob?.cancel()
            reconnectJob = null
            closing = detachTransportLocked()
            clearActiveTerminalLocked()
            when (outcome) {
                is RemoteTransportTerminalOutcome.Recoverable -> {
                    mutableState.value = mutableState.value.copy(
                        connection = ConnectionState.Reconnecting,
                        focus = FocusOwner.Unowned,
                        readOnly = true,
                        showTakeFocus = false,
                        pendingTransfers = 0,
                        lastError = outcome.message,
                        connectedEndpoint = null,
                    )
                    reconnect = true
                }
                RemoteTransportTerminalOutcome.Revoked -> {
                    mutableState.value = RemoteClientState(connection = ConnectionState.Revoked)
                }
            }
        }
        closing?.let(::finishTransportClose)
        if (reconnect) scheduleReconnect()
    }

    private fun accept(
        expectedGeneration: Long?,
        event: RemoteServerEvent,
        expectedTransport: RemoteTransport? = null,
    ) {
        var closing: ClosingTransport? = null
        var reconnect = false
        synchronized(lifecycleLock) {
            if (expectedGeneration != null &&
                (expectedGeneration != lifecycleGeneration ||
                    expectedTransport != null && transport !== expectedTransport)
            ) return
            when (event) {
                is RemoteServerEvent.FocusChanged -> mutableState.value = mutableState.value.copy(
                    focus = event.focus,
                    readOnly = event.focus != FocusOwner.Self,
                    showTakeFocus = event.focus != FocusOwner.Self,
                )
                is RemoteServerEvent.TransferStarted -> {
                    if (transfers.size >= MAX_PENDING_TRANSFERS) {
                        closing = detachTransportLocked()
                        transfers.clear()
                        mutableState.value = mutableState.value.copy(
                            connection = ConnectionState.Disconnected,
                            pendingTransfers = 0,
                            lastError = "Too many pending terminal transfers",
                            connectedEndpoint = null,
                        )
                    } else {
                        transfers += event.transferId
                        mutableState.value = mutableState.value.copy(pendingTransfers = transfers.size)
                    }
                }
                is RemoteServerEvent.TransferFinished -> {
                    transfers -= event.transferId
                    mutableState.value = mutableState.value.copy(pendingTransfers = transfers.size)
                }
                is RemoteServerEvent.TerminalChunk -> acceptTerminalChunk(event.chunk)
                is RemoteServerEvent.RosterChunk -> {
                    val roster = try {
                        rosterAssembler.accept(event.chunk)
                    } catch (error: RemoteProtocolException) {
                        mutableState.value = mutableState.value.copy(lastError = error.message)
                        null
                    }
                    if (roster != null) mutableState.value = mutableState.value.copy(tabs = roster.tabs)
                }
                is RemoteServerEvent.Raw -> acceptRaw(event)
                is RemoteServerEvent.Failure -> {
                    val lostFocus = event.code == "terminal.input_not_owned"
                    val disconnected = event.code == "transport.disconnected"
                    mutableState.value = mutableState.value.copy(
                        connection = if (disconnected) ConnectionState.Reconnecting else mutableState.value.connection,
                        focus = if (lostFocus) FocusOwner.Other else mutableState.value.focus,
                        readOnly = if (lostFocus) true else mutableState.value.readOnly,
                        showTakeFocus = if (lostFocus) true else mutableState.value.showTakeFocus,
                        lastError = event.message,
                        connectedEndpoint = if (disconnected) null else mutableState.value.connectedEndpoint,
                    )
                    if (disconnected) {
                        closing = detachTransportLocked()
                        clearActiveTerminalLocked()
                        reconnect = true
                    }
                }
                RemoteServerEvent.Revoked -> {
                    reconnectJob?.cancel()
                    reconnectJob = null
                    closing = detachTransportLocked()
                    clearActiveTerminalLocked()
                    mutableState.value = RemoteClientState(connection = ConnectionState.Revoked)
                }
            }
        }
        closing?.let(::finishTransportClose)
        if (reconnect) scheduleReconnect()
    }

    private fun acceptRaw(event: RemoteServerEvent.Raw) {
        when (event.kind) {
            "terminal.focus_changed" -> {
                val focus = RemoteCommands.focus(event.payload)
                if (focus.attachmentId == activeAttachmentId && focus.tabId == mutableState.value.activeTabId) {
                    accept(null, RemoteServerEvent.FocusChanged(focus.tabId, focus.attachmentId, focus.focus, focus.size))
                }
            }
            "session.changed" -> {
                refreshSessions()
                mutableState.value.previewSessionId?.let(::previewSession)
            }
            "agent.changed" -> refreshAgents()
            "tab.changed" -> refreshTabs()
            "terminal.title" -> {
                val title = RemoteCommands.title(event.payload)
                if (title.attachmentId == activeAttachmentId && title.tabId == mutableState.value.activeTabId) {
                    mutableState.value = mutableState.value.copy(activeTitle = title.title)
                }
            }
            "terminal.exited" -> {
                val exited = RemoteCommands.terminalExited(event.payload)
                if (exited.tabId == activeAttachmentTabId && exited.attachmentId == activeAttachmentId) {
                    activeAttachmentId = null
                    activeAttachmentTabId = null
                    terminalAssembler.clear()
                    screenStore.clear()
                    mutableScrollback.value = emptyList()
                    mutableState.value = mutableState.value.copy(
                        focus = FocusOwner.Unowned,
                        readOnly = true,
                        showTakeFocus = false,
                        pendingTransfers = 0,
                        lastError = "Terminal exited",
                    )
                    refreshTabs()
                }
            }
            else -> Unit
        }
    }

    private fun launchRequest(
        kind: String,
        payload: ByteArray,
        onError: ((String, String) -> Unit)? = null,
        onSuccess: (ByteArray) -> Unit = {},
    ): Boolean {
        val requestContext = synchronized(lifecycleLock) {
            val active = transport ?: return false
            RequestContext(lifecycleGeneration, active)
        }
        val response = requestContext.transport.request(kind, payload)
        return observeAcceptedResponse(
            generation = requestContext.lifecycleGeneration,
            active = requestContext.transport,
            response = response,
            onSuccess = onSuccess,
            onError = onError,
        )
    }

    private suspend fun requestResource(kind: String, payload: ByteArray): ByteArray {
        val requestContext = synchronized(lifecycleLock) {
            val active = transport
                ?: throw RemoteProtocolException("remote transport is disconnected")
            RequestContext(lifecycleGeneration, active)
        }
        return when (val response = requestContext.transport.request(kind, payload).await()) {
            is RemoteResponse.Success -> {
                if (response.kind != kind) throw RemoteProtocolException("unexpected remote resource response")
                response.payload
            }
            is RemoteResponse.Error -> throw RemoteUploadException(response.code, response.message)
        }
    }

    private fun observeAcceptedResponse(
        generation: Long,
        active: RemoteTransport,
        response: Deferred<RemoteResponse>,
        onSuccess: (ByteArray) -> Unit = {},
        onError: ((String, String) -> Unit)? = null,
    ): Boolean {
        val accepted = launchOwned(generation) {
            try {
                when (val result = response.await()) {
                    is RemoteResponse.Error -> {
                        synchronized(lifecycleLock) {
                            if (isCurrent(generation, active)) onError?.invoke(result.code, result.message)
                        }
                        if (onError == null) {
                            accept(
                                generation,
                                RemoteServerEvent.Failure(result.code, result.message),
                                active,
                            )
                        }
                    }
                    is RemoteResponse.Success -> synchronized(lifecycleLock) {
                        if (isCurrent(generation, active)) {
                            onSuccess(result.payload)
                        }
                    }
                }
            } catch (_: kotlinx.coroutines.CancellationException) {
                throw kotlinx.coroutines.CancellationException("remote request canceled")
            } catch (error: Exception) {
                synchronized(lifecycleLock) {
                    if (isCurrent(generation, active)) {
                        onError?.invoke("protocol.invalid_response", error.message ?: "Invalid desktop response")
                    }
                }
                if (onError == null || error is RemoteTransportTerminatedException) {
                    acceptRequestFailure(
                        generation,
                        active,
                        error,
                    )
                }
            }
        }
        if (!accepted) active.abandonRequest(response)
        return accepted
    }

    private fun acceptRequestFailure(
        expectedGeneration: Long,
        candidate: RemoteTransport,
        error: Exception,
    ) {
        if (error is RemoteTransportTerminatedException) {
            acceptTerminalOutcome(expectedGeneration, candidate, error.outcome)
        } else {
            accept(
                expectedGeneration,
                RemoteServerEvent.Failure("transport.disconnected", error.message ?: "Connection ended"),
                candidate,
            )
        }
    }

    private fun scheduleReconnect() {
        val job = synchronized(lifecycleLock) {
            if (reconnectJob?.isActive == true || !isUnlocked()) return
            scope.launch(dispatcher, start = CoroutineStart.LAZY) {
                var attempt = 0
                while (true) {
                    val delayMillis = RECONNECT_DELAYS_MILLIS[
                        attempt.coerceAtMost(RECONNECT_DELAYS_MILLIS.lastIndex)
                    ]
                    delay(delayMillis)
                    if (!isUnlocked() || mutableState.value.connection == ConnectionState.Revoked ||
                        mutableState.value.connection == ConnectionState.Locked
                    ) return@launch
                    if (connectOnce(ConnectionState.Reconnecting)) return@launch
                    attempt += 1
                }
            }.also { reconnectJob = it }
        }
        job.start()
    }

    private fun acceptTerminalChunk(chunk: TerminalTransferChunk) {
        if (chunk.tabId != mutableState.value.activeTabId || chunk.tabId != activeAttachmentTabId ||
            chunk.attachmentId != activeAttachmentId
        ) return
        if (chunk.kind == TerminalTransferKind.Scrollback) {
            val paging = scrollbackRequest ?: return
            if (paging.tabId != chunk.tabId || paging.attachmentId != chunk.attachmentId ||
                paging.requestId == null || paging.requestId != chunk.requestId
            ) return
        }
        when (val result = terminalAssembler.accept(chunk)) {
            TerminalTransferResult.Pending -> mutableState.value = mutableState.value.copy(pendingTransfers = 1)
            TerminalTransferResult.Recover -> requestRecovery(chunk.tabId, chunk.attachmentId)
            is TerminalTransferResult.Snapshot -> {
                try {
                    if (screenStore.screen.value?.tabId != result.snapshot.tabId) {
                        mutableScrollback.value = emptyList()
                    }
                    screenStore.replace(result.snapshot)
                    recoveryRequested = false
                    mutableState.value = mutableState.value.copy(pendingTransfers = 0)
                } catch (_: IllegalArgumentException) {
                    requestRecovery(chunk.tabId, result.attachmentId)
                }
            }
            is TerminalTransferResult.Diff -> {
                mutableState.value = mutableState.value.copy(pendingTransfers = 0)
                if (screenStore.apply(result.diff) == ApplyResult.NeedsSnapshot) {
                    requestRecovery(chunk.tabId, result.attachmentId)
                }
            }
            is TerminalTransferResult.Scrollback -> {
                mutableState.value = mutableState.value.copy(pendingTransfers = 0)
                val paging = scrollbackRequest
                if (paging != null && result.tabId == screenStore.screen.value?.tabId &&
                    paging.offset == mutableScrollback.value.size
                ) {
                    mutableScrollback.value = (mutableScrollback.value + result.rows).take(MAX_SCROLLBACK_ROWS)
                }
                scrollbackRequest = null
            }
        }
    }

    private fun requestRecovery(tabId: String, attachmentId: String?) {
        terminalAssembler.clear()
        mutableState.value = mutableState.value.copy(pendingTransfers = 0)
        val active = transport ?: return
        if (!isUnlocked() || attachmentId == null || recoveryRequested) return
        recoveryRequested = true
        val revision = screenStore.screen.value?.revision ?: 0
        val response = active.request(
            "terminal.resume",
            RemoteWireCodec.encodeTerminalResumePayload(tabId, attachmentId, revision),
        )
        val generation = lifecycleGeneration
        launchOwned(generation) {
            try {
                when (val result = response.await()) {
                    is RemoteResponse.Error -> accept(
                        generation,
                        RemoteServerEvent.Failure(result.code, result.message),
                        active,
                    )
                    is RemoteResponse.Success -> Unit
                }
            } catch (error: kotlinx.coroutines.CancellationException) {
                throw error
            } catch (error: Exception) {
                acceptRequestFailure(
                    generation,
                    active,
                    error,
                )
            }
        }
    }

    private fun closeTransport() {
        val closing = synchronized(lifecycleLock) { detachTransportLocked() }
        finishTransportClose(closing)
    }

    private fun detachTransportLocked(): ClosingTransport {
        lifecycleGeneration += 1
        selectionGeneration += 1
        val jobs = ownedJobs.toList()
        ownedJobs.clear()
        val active = transport
        eventJob = null
        transport = null
        terminalAssembler.clear()
        rosterAssembler.clear()
        recoveryRequested = false
        scrollbackRequest = null
        activeAttachmentId = null
        activeAttachmentTabId = null
        return ClosingTransport(active, jobs)
    }

    private fun clearActiveTerminalLocked() {
        transfers.clear()
        terminalAssembler.clear()
        rosterAssembler.clear()
        recoveryRequested = false
        scrollbackRequest = null
        activeAttachmentId = null
        activeAttachmentTabId = null
        screenStore.clear()
        mutableScrollback.value = emptyList()
    }

    private fun finishTransportClose(closing: ClosingTransport) {
        closing.jobs.forEach(Job::cancel)
        closing.transport?.close()
    }

    private fun beginConnection(candidate: RemoteTransport, connectingState: ConnectionState): Long {
        val closing = synchronized(lifecycleLock) {
            lifecycleGeneration += 1
            selectionGeneration += 1
            val jobs = ownedJobs.toList()
            ownedJobs.clear()
            val previous = transport
            eventJob = null
            transport = candidate
            activeAttachmentId = null
            activeAttachmentTabId = null
            terminalAssembler.clear()
            rosterAssembler.clear()
            recoveryRequested = false
            scrollbackRequest = null
            mutableState.value = mutableState.value.copy(
                connection = connectingState,
                connectedEndpoint = null,
            )
            Triple(previous, jobs, lifecycleGeneration)
        }
        closing.second.forEach(Job::cancel)
        closing.first?.close()
        return closing.third
    }

    private fun launchOwned(generation: Long, block: suspend () -> Unit): Boolean {
        lateinit var job: Job
        job = scope.launch(dispatcher, start = CoroutineStart.LAZY) { block() }
        val accepted = synchronized(lifecycleLock) {
            if (generation != lifecycleGeneration || ownedJobs.size >= MAX_OWNED_JOBS) false
            else {
                ownedJobs += job
                job.invokeOnCompletion { synchronized(lifecycleLock) { ownedJobs -= job } }
                true
            }
        }
        if (accepted) job.start() else job.cancel()
        return accepted
    }

    private suspend fun runSelection(selection: Selection) {
        val previous = selection.previousAttachment
        if (previous != null && synchronized(lifecycleLock) {
                isCurrent(selection.lifecycleGeneration, selection.transport)
            }
        ) {
            requestIgnoringError(
                selection.transport,
                "terminal.detach",
                RemoteCommands.attachment(previous.first, previous.second),
            )
        }
        if (!isSelectionCurrent(selection)) return
        val response = selection.transport.request("terminal.attach", RemoteCommands.tab(selection.tabId)).await()
        val attachRequestId = response.requestId
        if (response is RemoteResponse.Error) {
            selection.transport.completeAttachment(attachRequestId, false)
            accept(
                selection.lifecycleGeneration,
                RemoteServerEvent.Failure(response.code, response.message),
                selection.transport,
            )
            return
        }
        val attached = RemoteCommands.attached((response as RemoteResponse.Success).payload)
        if (!isSelectionCurrent(selection) || attached.tabId != selection.tabId) {
            selection.transport.completeAttachment(attachRequestId, false)
            requestIgnoringError(
                selection.transport,
                "terminal.detach",
                RemoteCommands.attachment(attached.tabId, attached.attachmentId),
            )
            return
        }
        val committed = synchronized(lifecycleLock) {
            if (!isSelectionCurrent(selection)) false
            else {
                activeAttachmentId = attached.attachmentId
                activeAttachmentTabId = attached.tabId
                mutableState.value = mutableState.value.copy(
                    activeTabId = attached.tabId,
                    activeTitle = attached.title,
                    focus = if (attached.hasFocus) FocusOwner.Self else FocusOwner.Other,
                    readOnly = !attached.hasFocus,
                    showTakeFocus = !attached.hasFocus,
                )
                true
            }
        }
        selection.transport.completeAttachment(attachRequestId, committed)
        if (!committed) {
            requestIgnoringError(
                selection.transport,
                "terminal.detach",
                RemoteCommands.attachment(attached.tabId, attached.attachmentId),
            )
        }
    }

    private suspend fun requestIgnoringError(transport: RemoteTransport, kind: String, payload: ByteArray) {
        try {
            transport.request(kind, payload).await()
        } catch (error: kotlinx.coroutines.CancellationException) {
            throw error
        } catch (_: Exception) {
            // Connection teardown owns cleanup when the detach cannot be delivered.
        }
    }

    private fun activeUploadContext(): UploadContext? {
        if (mutableState.value.connection != ConnectionState.Connected || mutableState.value.focus != FocusOwner.Self) {
            return null
        }
        val (tabId, attachmentId) = activeTarget() ?: return null
        val active = transport ?: return null
        return UploadContext(lifecycleGeneration, active, tabId, attachmentId)
    }

    private fun requireCurrentUploadContext(context: UploadContext) {
        val current = synchronized(lifecycleLock) {
            isCurrent(context.lifecycleGeneration, context.transport) &&
                mutableState.value.connection == ConnectionState.Connected &&
                mutableState.value.focus == FocusOwner.Self &&
                activeTarget() == (context.tabId to context.attachmentId)
        }
        if (!current) throw RemoteUploadException(null, "terminal focus changed while images were uploading")
    }

    private suspend fun requestUpload(context: UploadContext, kind: String, payload: ByteArray): ByteArray {
        requireCurrentUploadContext(context)
        return when (val response = context.transport.request(kind, payload).await()) {
            is RemoteResponse.Success -> {
                if (response.kind != kind) {
                    throw RemoteProtocolException("unexpected terminal image upload response")
                }
                response.payload
            }
            is RemoteResponse.Error -> throw RemoteUploadException(response.code, response.message)
        }
    }

    private suspend fun resumeUploadContext(
        tabId: String,
        previous: UploadContext,
        error: Exception,
    ): UploadContext? {
        val requestTimedOut = error is RemoteProtocolException &&
            error.message.orEmpty().contains("timed out", ignoreCase = true)
        val transportFailure = synchronized(lifecycleLock) {
            !isCurrent(previous.lifecycleGeneration, previous.transport) ||
                mutableState.value.connection != ConnectionState.Connected
        } || (error is RemoteProtocolException &&
            error.message.orEmpty().contains("transport", ignoreCase = true)) || requestTimedOut
        if (!transportFailure) return null

        val deadline = System.nanoTime() + UPLOAD_RESUME_TIMEOUT_MILLIS * 1_000_000
        while (System.nanoTime() < deadline) {
            val resumed = synchronized(lifecycleLock) { activeUploadContext() }
            if (resumed != null && resumed.tabId == tabId &&
                (requestTimedOut || resumed.lifecycleGeneration != previous.lifecycleGeneration ||
                    resumed.transport !== previous.transport)
            ) return resumed
            if (mutableState.value.connection == ConnectionState.Revoked ||
                mutableState.value.connection == ConnectionState.Locked
            ) return null
            delay(100)
        }
        throw RemoteUploadException(null, "image upload could not resume after the desktop connection ended", error)
    }

    private suspend fun cancelBegunUploads(context: UploadContext, uploadIds: Set<String>) {
        withTimeoutOrNull(UPLOAD_CLEANUP_TIMEOUT_MILLIS) {
            for (uploadId in uploadIds.toList()) {
                val sameConnection = synchronized(lifecycleLock) {
                    isCurrent(context.lifecycleGeneration, context.transport)
                }
                if (!sameConnection) return@withTimeoutOrNull
                val cancel = context.transport.request(
                    "terminal.upload.cancel",
                    RemoteCommands.uploadCancel(uploadId),
                )
                var completed = false
                try {
                    when (val response = cancel.await()) {
                        is RemoteResponse.Success -> {
                            if (response.kind == "terminal.upload.cancel") {
                                RemoteCommands.uploadAcknowledged(response.payload)
                            }
                        }
                        is RemoteResponse.Error -> Unit
                    }
                    completed = true
                } catch (error: kotlinx.coroutines.CancellationException) {
                    throw error
                } catch (_: Exception) {
                    // The disconnected transport owns server-side cleanup when a cancel frame cannot be delivered.
                } finally {
                    if (!completed) context.transport.abandonRequest(cancel)
                }
            }
        }
    }

    private fun activeTarget(): Pair<String, String>? {
        val tabId = mutableState.value.activeTabId ?: return null
        if (activeAttachmentTabId != tabId) return null
        return tabId to (activeAttachmentId ?: return null)
    }

    private fun isCurrent(generation: Long, candidate: RemoteTransport): Boolean =
        generation == lifecycleGeneration && transport === candidate

    private fun isSelectionCurrent(selection: Selection): Boolean = synchronized(lifecycleLock) {
        isCurrent(selection.lifecycleGeneration, selection.transport) &&
            selection.selectionGeneration == selectionGeneration &&
            mutableState.value.activeTabId == selection.tabId &&
            mutableState.value.connection == ConnectionState.Connected
    }

    private data class RequestContext(val lifecycleGeneration: Long, val transport: RemoteTransport)
    private data class TerminalInputContext(
        val lifecycleGeneration: Long,
        val transport: RemoteTransport,
        val attachmentId: String,
    )
    private data class RequestBatchContext(
        val lifecycleGeneration: Long,
        val transport: RemoteTransport,
        val responses: List<Deferred<RemoteResponse>>,
    )
    private data class UploadContext(
        val lifecycleGeneration: Long,
        val transport: RemoteTransport,
        val tabId: String,
        val attachmentId: String,
    )
    private data class ClosingTransport(val transport: RemoteTransport?, val jobs: List<Job>)
    private data class ScrollbackRequest(
        val lifecycleGeneration: Long,
        val transport: RemoteTransport,
        val tabId: String,
        val attachmentId: String,
        val offset: Int,
        var requestId: Long? = null,
    )
    private data class Selection(
        val lifecycleGeneration: Long,
        val selectionGeneration: Long,
        val tabId: String,
        val transport: RemoteTransport,
        val previousAttachment: Pair<String, String>?,
    )

    private companion object {
        const val MAX_PENDING_TRANSFERS = 4
        const val MAX_INPUT_BYTES = 64 * 1_024
        const val MAX_SCROLLBACK_ROWS = 5_000
        const val MAX_OWNED_JOBS = 64
        const val UPLOAD_CLEANUP_TIMEOUT_MILLIS = 2_000L
        const val UPLOAD_RESUME_TIMEOUT_MILLIS = 2 * 60_000L
        const val TERMINAL_SUBMIT_SETTLE_MILLIS = 75L
        val RECONNECT_DELAYS_MILLIS = longArrayOf(1_000, 2_000, 4_000, 8_000, 16_000)
    }
}
