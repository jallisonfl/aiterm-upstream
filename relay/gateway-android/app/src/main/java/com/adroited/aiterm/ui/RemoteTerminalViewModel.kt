package com.adroited.aiterm.ui

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import com.adroited.aiterm.pairing.PairedDesktop
import com.adroited.aiterm.pairing.PairedDesktopStore
import com.adroited.aiterm.remote.AuthenticatedRemoteTransport
import com.adroited.aiterm.remote.AndroidNetworkMonitor
import com.adroited.aiterm.remote.ConnectionState
import com.adroited.aiterm.remote.OkHttpRemoteSocketDialer
import com.adroited.aiterm.remote.RemoteClient
import com.adroited.aiterm.remote.RemoteUploadProgress
import com.adroited.aiterm.remote.RemoteUploadSource
import com.adroited.aiterm.remote.TerminalSize
import com.adroited.aiterm.security.AppLock
import com.adroited.aiterm.security.DeviceKeys
import com.adroited.aiterm.terminal.DefaultTerminalScreenStore
import kotlinx.coroutines.launch
import kotlinx.coroutines.delay
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.withTimeoutOrNull
import java.io.ByteArrayOutputStream
import okhttp3.HttpUrl

class RemoteTerminalViewModel(
    initialDesktop: PairedDesktop,
    deviceKeys: DeviceKeys,
    private val appLock: AppLock,
    private val pairedDesktopStore: PairedDesktopStore,
    context: Context,
) : ViewModel() {
    private var desktop = initialDesktop
    /** Survives terminal tab changes and configuration changes for this ViewModel's lifetime. */
    internal val terminalDrafts = TerminalDraftStore()
    private val screenStore = DefaultTerminalScreenStore()
    private val dialer = OkHttpRemoteSocketDialer()
    private val networkMonitor = AndroidNetworkMonitor(context)
    private var connectJob: Job? = null
    val client = RemoteClient(
        transportFactory = {
            AuthenticatedRemoteTransport(
                desktop = desktop,
                deviceKeys = deviceKeys,
                appLock = appLock,
                dialer = dialer,
                scope = viewModelScope,
            )
        },
        screenStore = screenStore,
        isUnlocked = { !appLock.isLocked.value },
        scope = viewModelScope,
    )

    init {
        reconnect()
        viewModelScope.launch {
            networkMonitor.changes.collectLatest {
                delay(NETWORK_SETTLE_MILLIS)
                if (!appLock.isLocked.value) reconnect()
            }
        }
        viewModelScope.launch {
            client.state.map { it.connection }.distinctUntilChanged().collectLatest { connection ->
                if (connection == ConnectionState.Connected) refreshConnectedDesktop()
            }
        }
        viewModelScope.launch {
            appLock.isLocked.collectLatest { locked ->
                if (locked) {
                    client.lock()
                } else if (client.state.value.connection == com.adroited.aiterm.remote.ConnectionState.Locked) {
                    reconnect()
                }
            }
        }
    }

    fun reconnect() {
        connectJob?.cancel()
        connectJob = viewModelScope.launch { client.connect() }
    }

    private suspend fun refreshConnectedDesktop() {
        runCatching {
            val routes = client.gatewayRoutes()
            desktop = desktop.copy(
                hosts = routes.hosts,
                port = routes.port,
                relayHost = routes.relayHost,
                relayPort = routes.relayPort,
            )
            pairedDesktopStore.save(desktop)
        }
        client.refreshSessions()
        client.refreshAgents()
    }

    fun selectTab(tabId: String) = client.selectTab(tabId)
    fun sendInput(text: String) = client.sendInput(text)
    fun sendInputs(tabId: String, texts: List<String>) = client.sendInputs(tabId, texts)
    /**
     * Uploads normalized drafts only. Prompt formatting and terminal input stay in the UI submit
     * path so failed uploads can never inject a partial prompt.
     */
    suspend fun uploadImages(
        expectedTabId: String,
        images: List<NormalizedTerminalImage>,
        onProgress: (RemoteUploadProgress) -> Unit = {},
    ): Result<List<String>> = client.uploadImages(expectedTabId, images.map(::remoteUploadSource), onProgress)

    /** Upload counterpart for the immutable images retained by [terminalDrafts]. */
    internal suspend fun uploadDraftImages(
        expectedTabId: String,
        images: List<TerminalAttachmentImage>,
        onProgress: (RemoteUploadProgress) -> Unit = {},
    ): Result<List<String>> = client.uploadImages(
        expectedTabId,
        images.map { it.asRemoteUploadSource() },
        onProgress,
    )
    fun takeFocus(cols: Int, rows: Int) = client.takeFocus(TerminalSize(cols, rows))
    fun resize(cols: Int, rows: Int) = client.resize(TerminalSize(cols, rows))
    fun loadOlderScrollback() = client.requestNextScrollbackPage()
    fun openSession(id: String, cols: Int, rows: Int) =
        client.openSession(id, TerminalSize(cols, rows))
    fun previewSession(id: String) = client.previewSession(id)
    suspend fun sessionChanges(id: String): Result<List<com.adroited.aiterm.remote.RemoteSessionChange>> =
        runCatching { client.sessionChanges(id) }

    suspend fun hasWebPreview(id: String): Result<Boolean> =
        runCatching { client.webPreview(id, open = false).available }

    suspend fun openWebPreview(id: String): Result<String> = runCatching {
        val preview = client.webPreview(id, open = true)
        check(preview.available && preview.path != null) {
            "This session does not have a webpage to preview yet."
        }
        val endpoint = client.state.value.connectedEndpoint
            ?: error("The desktop is disconnected.")
        HttpUrl.Builder()
            .scheme("https")
            .host(endpoint.host)
            .port(endpoint.port)
            .encodedPath(preview.path)
            .build()
            .toString()
    }

    fun desktopSpkiFingerprint(): String = desktop.serverSpkiFingerprint

    internal suspend fun sessionFilePreview(
        sessionId: String,
        path: String,
        maxBytes: Int = 8 * 1024 * 1024,
    ): Result<RemoteSessionFilePreview> = runCatching {
        require(maxBytes > 0)
        val output = ByteArrayOutputStream(maxBytes.coerceAtMost(256 * 1024))
        var offset = 0L
        var total = -1L
        var mime = "application/octet-stream"
        var eof = false
        while (!eof && offset < maxBytes) {
            val count = minOf(256 * 1024, maxBytes - offset.toInt())
            val chunk = client.readFileChunk(sessionId, path, offset, count)
            check(chunk.path == path && (total < 0 || chunk.total == total) && chunk.offset == offset) {
                "The desktop returned a different file while reading."
            }
            if (total < 0) {
                total = chunk.total
                mime = chunk.mime
            } else {
                check(chunk.mime == mime) { "The desktop changed the file type while reading." }
            }
            output.write(chunk.data)
            offset += chunk.data.size
            eof = chunk.eof
            check(eof || chunk.data.isNotEmpty()) { "The desktop returned an empty file chunk." }
        }
        RemoteSessionFilePreview(
            path = path,
            mime = mime,
            total = total.coerceAtLeast(0),
            data = output.toByteArray(),
            truncated = !eof,
        )
    }

    /**
     * Sends from the conversation view without introducing a second protocol. Opening the
     * session, attaching, and taking focus use the same authenticated terminal path as the grid.
     */
    suspend fun sendConversationPrompt(
        sessionId: String,
        text: String,
        images: List<TerminalAttachmentImage> = emptyList(),
        onProgress: (RemoteUploadProgress) -> Unit = {},
    ): Result<Unit> {
        val prompt = text.trim()
        if (prompt.isEmpty() && images.isEmpty()) {
            return Result.failure(IllegalArgumentException("Write a message or attach an image first."))
        }
        return try {
            val existing = client.state.value.tabs.firstOrNull { it.sessionId == sessionId }
            if (existing != null) client.selectTab(existing.id)
            else client.openSession(sessionId, TerminalSize(80, 24))

            val activeScreen = withTimeoutOrNull(10_000) {
                client.screen.filterNotNull().first { screen ->
                    client.state.value.tabs.any { it.id == screen.tabId && it.sessionId == sessionId }
                }
            } ?: return Result.failure(IllegalStateException("The session did not open on the desktop."))

            if (client.state.value.focus != com.adroited.aiterm.remote.FocusOwner.Self) {
                if (!client.takeFocus(TerminalSize(activeScreen.cols, activeScreen.rows))) {
                    return Result.failure(IllegalStateException("The terminal is not ready for input."))
                }
                val focused = withTimeoutOrNull(5_000) {
                    client.state.first {
                        it.activeTabId == activeScreen.tabId &&
                            it.focus == com.adroited.aiterm.remote.FocusOwner.Self
                    }
                }
                if (focused == null) {
                    return Result.failure(IllegalStateException("AITerm could not take terminal focus."))
                }
            }

            val latestScreen = client.screen.value
                ?.takeIf { it.tabId == activeScreen.tabId }
                ?: return Result.failure(IllegalStateException("The terminal changed before sending."))
            val paths = if (images.isEmpty()) emptyList() else {
                client.uploadImages(activeScreen.tabId, images.map { it.asRemoteUploadSource() }, onProgress)
                    .getOrElse { return Result.failure(it) }
            }
            val outbound = formatTerminalSubmission(
                text = prompt,
                paths = paths,
                bracketedPaste = latestScreen.modes.bracketedPaste,
            )
            if (!client.submitInputs(activeScreen.tabId, outbound)) {
                return Result.failure(IllegalStateException("The terminal did not accept the message."))
            }
            delay(350)
            client.previewSession(sessionId)
            Result.success(Unit)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            Result.failure(error)
        }
    }
    fun closeSession(id: String) = client.closeSession(id)
    fun stopSession(id: String) = client.stopSession(id)
    fun forkSession(id: String) = client.forkSession(id)
    fun deleteSession(id: String) = client.deleteSession(id)
    fun closeTab(id: String) = client.closeTab(id)
    fun openShell(projectPath: String?, cols: Int, rows: Int) =
        client.openShell(projectPath, TerminalSize(cols, rows))
    fun startAgent(
        agent: com.adroited.aiterm.remote.RemoteAgentChoice,
        modelId: String?,
        effort: String?,
        cwd: String,
        cols: Int,
        rows: Int,
    ) = client.startAgent(agent, modelId, effort, cwd, TerminalSize(cols, rows))

    override fun onCleared() {
        connectJob?.cancel()
        networkMonitor.close()
        client.lock()
    }

    companion object {
        fun factory(
            desktop: PairedDesktop,
            deviceKeys: DeviceKeys,
            appLock: AppLock,
            pairedDesktopStore: PairedDesktopStore,
            context: Context,
        ): ViewModelProvider.Factory = viewModelFactory {
            initializer { RemoteTerminalViewModel(desktop, deviceKeys, appLock, pairedDesktopStore, context) }
        }

        private const val NETWORK_SETTLE_MILLIS = 350L
    }
}

internal data class RemoteSessionFilePreview(
    val path: String,
    val mime: String,
    val total: Long,
    val data: ByteArray,
    val truncated: Boolean,
)

internal fun remoteUploadSource(image: NormalizedTerminalImage): RemoteUploadSource = RemoteUploadSource(
    id = image.id,
    file = image.file,
    length = image.length,
    sha256 = image.sha256.copyOf(),
)
