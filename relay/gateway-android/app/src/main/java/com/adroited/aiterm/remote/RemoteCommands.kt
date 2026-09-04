package com.adroited.aiterm.remote

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.cbor.ByteString
import kotlinx.serialization.cbor.Cbor

@Serializable
data class RemoteSession(
    val id: String,
    val agent: String,
    val title: String,
    @SerialName("project_path") val projectPath: String,
    @SerialName("group_path") val groupPath: String,
    val branch: String? = null,
    val forked: Boolean,
    val background: Boolean,
    @SerialName("fork_parent") val forkParent: String? = null,
    @SerialName("last_active") val lastActive: Long,
)

data class RemoteSessionRoster(
    val sessions: List<RemoteSession>,
    val withFiles: Set<String>,
    val stars: Set<String>,
    val broughtIn: Map<String, String>,
    val activity: Map<String, String>,
)

@Serializable
data class RemoteUsageBar(
    val kind: String,
    val label: String,
    val percent: Double,
    val severity: String,
    @SerialName("resets_at") val resetsAt: String,
)

@Serializable
data class RemoteUsageAmount(
    val label: String,
    val amount: Double,
    val of: Double? = null,
    val currency: String,
    val sense: String,
)

@Serializable
data class RemoteUsageSource(
    val id: String,
    val name: String,
    val state: String,
    val detail: String,
    val plan: String,
    val account: String,
    val bars: List<RemoteUsageBar>,
    val amounts: List<RemoteUsageAmount>,
    val notes: List<String>,
)

data class AttachedTerminal(
    val tabId: String,
    val attachmentId: String,
    val hasFocus: Boolean,
    val title: String,
)

data class RemoteUploadBegan(val uploadId: String, val nextChunk: Int, val path: String?)

data class RemoteFocusEvent(
    val tabId: String,
    val attachmentId: String,
    val focus: FocusOwner,
    val size: TerminalSize,
)

data class RemoteTitleEvent(val tabId: String, val attachmentId: String, val title: String)
data class RemoteTerminalExitEvent(val tabId: String, val attachmentId: String, val exit: RemoteTabExit)
data class RemoteGatewayRoutes(
    val hosts: List<String>,
    val port: Int,
    val relayHost: String?,
    val relayPort: Int?,
)
@Serializable data class RemotePreviewMessage(val role: String, val text: String, val at: String? = null)
@Serializable
data class RemoteSessionChange(
    val path: String,
    val name: String,
    val kind: String,
    val at: Long,
    @SerialName("session_id") val sessionId: String? = null,
    val bytes: Long,
)

data class RemoteFileChunk(
    val path: String,
    val mime: String,
    val offset: Long,
    val total: Long,
    val eof: Boolean,
    val data: ByteArray,
)

data class RemoteWebPreview(
    val available: Boolean,
    val path: String?,
)

@Serializable
data class RemoteModelOption(
    val id: String,
    @SerialName("display_name") val displayName: String,
    val efforts: List<String>,
    @SerialName("default_effort") val defaultEffort: String? = null,
)

@Serializable
data class RemoteAgentChoice(
    val id: String,
    @SerialName("display_name") val displayName: String,
    val models: List<RemoteModelOption>,
    @SerialName("mints_session_id") val mintsSessionId: Boolean,
)

@Serializable
data class RemoteAgentCaps(
    val fork: Boolean,
    val clear: Boolean,
    val resume: Boolean,
    @SerialName("tui_drive") val tuiDrive: Boolean,
    val panels: Boolean,
    val tasks: Boolean,
    val delete: Boolean,
    val config: Boolean,
    @SerialName("roster_liveness") val rosterLiveness: Boolean,
)

data class RemoteAgentRoster(
    val agents: List<RemoteAgentChoice>,
    val caps: Map<String, RemoteAgentCaps>,
)

data class RemoteDirectOffer(
    val id: String,
    val cookie: String,
    val host: String,
    val port: Int,
    val expiresInMillis: Long,
)

@OptIn(ExperimentalSerializationApi::class)
object RemoteCommands {
    const val MAX_UPLOAD_BYTES = 12 * 1_024 * 1_024L
    const val MAX_UPLOAD_CHUNK_BYTES = 256 * 1_024
    const val MAX_UPLOADS_PER_SUBMISSION = 4
    const val MAX_SUBMISSION_BYTES = 48 * 1_024 * 1_024L
    const val MAX_SUBMISSION_ID_BYTES = 128
    const val MAX_IDENTIFIER_BYTES = 4 * 1_024
    const val MAX_PATH_BYTES = 4 * 1_024

    private val cbor = Cbor {
        encodeDefaults = true
        ignoreUnknownKeys = false
        useDefiniteLengthEncoding = true
    }

    fun tab(tabId: String): ByteArray = encode(TabIdPayload.serializer(), TabIdPayload(tabId))
    fun gatewayRoutes(payload: ByteArray): RemoteGatewayRoutes =
        decode(GatewayRoutesReply.serializer(), payload).let {
            if (
                it.hosts.isEmpty() || it.hosts.size > 16 ||
                it.hosts.any { host -> host.isBlank() } ||
                it.port !in 1024..65_535 ||
                ((it.relayHost == null) != (it.relayPort == null)) ||
                it.relayPort?.let { port -> port !in 1..65_535 } == true
            ) malformed()
            RemoteGatewayRoutes(it.hosts, it.port, it.relayHost, it.relayPort)
        }
    fun directOffer(payload: ByteArray): RemoteDirectOffer =
        decode(DirectOfferReply.serializer(), payload).let {
            if (
                it.id.isBlank() || it.cookie.isBlank() || it.host.isBlank() ||
                it.id.length > 128 || it.cookie.length > 128 || it.host.length > 253 ||
                it.port !in 1..65_535 || it.expiresInMillis !in 1..60_000
            ) malformed()
            RemoteDirectOffer(
                it.id,
                it.cookie,
                it.host,
                it.port,
                it.expiresInMillis,
            )
        }
    fun attachment(tabId: String, attachmentId: String): ByteArray =
        encode(AttachmentPayload.serializer(), AttachmentPayload(tabId, attachmentId))
    fun input(tabId: String, attachmentId: String, data: ByteArray): ByteArray =
        encode(InputPayload.serializer(), InputPayload(tabId, attachmentId, data))
    fun sized(tabId: String, attachmentId: String, size: TerminalSize): ByteArray =
        encode(SizedPayload.serializer(), SizedPayload(tabId, attachmentId, size))
    fun scrollback(tabId: String, attachmentId: String, offset: Int, count: Int): ByteArray =
        encode(ScrollbackPayload.serializer(), ScrollbackPayload(tabId, attachmentId, offset, count))
    fun session(sessionId: String): ByteArray =
        encode(SessionIdPayload.serializer(), SessionIdPayload(sessionId))
    fun previewSession(sessionId: String): ByteArray = session(sessionId)
    fun conversation(sessionId: String, maxChars: Int = 512 * 1_024): ByteArray =
        encode(SessionConversationPayload.serializer(), SessionConversationPayload(sessionId, maxChars))
    fun webPreview(sessionId: String, open: Boolean): ByteArray {
        requireIdentifier(sessionId)
        return encode(
            SessionWebPreviewRequest.serializer(),
            SessionWebPreviewRequest(sessionId, open),
        )
    }
    fun fileRead(sessionId: String, path: String, offset: Long, count: Int): ByteArray {
        requireIdentifier(sessionId)
        if (path.isBlank() || path.encodeToByteArray().size > MAX_PATH_BYTES || offset < 0 ||
            count !in 1..MAX_UPLOAD_CHUNK_BYTES
        ) malformed()
        return encode(FileReadRequest.serializer(), FileReadRequest(sessionId, path, offset, count))
    }
    fun openSession(sessionId: String, size: TerminalSize): ByteArray =
        encode(SessionOpenPayload.serializer(), SessionOpenPayload(sessionId, size))
    fun closeSession(sessionId: String, tabId: String?): ByteArray =
        encode(SessionClosePayload.serializer(), SessionClosePayload(sessionId, tabId))
    fun shell(projectPath: String?, title: String?, size: TerminalSize): ByteArray =
        encode(TabOpenPayload.serializer(), TabOpenPayload(projectPath = projectPath, title = title, size = size))
    fun startAgent(
        agentId: String,
        model: String?,
        effort: String?,
        cwd: String,
        title: String,
        size: TerminalSize,
    ): ByteArray = encode(
        AgentStartPayload.serializer(),
        AgentStartPayload(agentId = agentId, model = model, effort = effort, cwd = cwd, title = title, size = size),
    )

    fun uploadBegin(
        tabId: String,
        attachmentId: String,
        submissionId: String,
        submissionCount: Int,
        memberIndex: Int = 0,
        submissionBytes: Long,
        length: Long,
        sha256: ByteArray,
    ): ByteArray {
        requireIdentifier(tabId)
        requireIdentifier(attachmentId)
        requireSubmissionId(submissionId)
        if (submissionCount !in 1..MAX_UPLOADS_PER_SUBMISSION ||
            memberIndex !in 0 until submissionCount ||
            submissionBytes !in 1..MAX_SUBMISSION_BYTES ||
            length !in 1..MAX_UPLOAD_BYTES ||
            length > submissionBytes ||
            sha256.size != 32
        ) malformed()
        return encode(
            UploadBeginPayload.serializer(),
            UploadBeginPayload(
                tabId = tabId,
                attachmentId = attachmentId,
                submissionId = submissionId,
                submissionCount = submissionCount,
                memberIndex = memberIndex,
                submissionBytes = submissionBytes,
                length = length,
                sha256 = sha256,
            ),
        )
    }

    fun uploadChunk(uploadId: String, index: Int, data: ByteArray): ByteArray {
        requireIdentifier(uploadId)
        if (index < 0 || data.isEmpty() || data.size > MAX_UPLOAD_CHUNK_BYTES) malformed()
        return encode(UploadChunkPayload.serializer(), UploadChunkPayload(uploadId, index, data))
    }

    fun uploadFinish(uploadId: String): ByteArray {
        requireIdentifier(uploadId)
        return encode(UploadIdPayload.serializer(), UploadIdPayload(uploadId))
    }

    fun uploadCancel(uploadId: String): ByteArray = uploadFinish(uploadId)

    fun uploadBegan(payload: ByteArray): RemoteUploadBegan =
        decode(UploadBeginReply.serializer(), payload).let {
            if (it.uploadId.isBlank() || it.uploadId.encodeToByteArray().size > MAX_IDENTIFIER_BYTES ||
                it.nextChunk < 0
            ) malformed()
            if (it.path != null && (it.path.isBlank() || it.path.encodeToByteArray().size > MAX_PATH_BYTES)) malformed()
            RemoteUploadBegan(it.uploadId, it.nextChunk, it.path)
        }

    fun uploadedPath(payload: ByteArray): String = decode(UploadFinishReply.serializer(), payload).path.also {
        if (it.isBlank() || it.encodeToByteArray().size > MAX_PATH_BYTES) malformed()
    }

    fun uploadAcknowledged(payload: ByteArray) {
        if (!decode(UploadSuccessReply.serializer(), payload).ok) malformed()
    }

    fun sessions(payload: ByteArray): List<RemoteSession> = sessionRoster(payload).sessions

    fun sessionRoster(payload: ByteArray): RemoteSessionRoster =
        decode(SessionListReply.serializer(), payload).let { reply ->
            val ids = reply.sessions.mapTo(hashSetOf()) { it.id }
            if (reply.sessions.size > 4_096 || reply.sessions.any { it.id.length !in 1..512 } ||
                reply.withFiles.size > 4_096 || reply.stars.size > 4_096 ||
                reply.broughtIn.size > 4_096 || reply.activity.size > 4_096 ||
                reply.withFiles.any { it !in ids } || reply.stars.any { it !in ids } ||
                reply.broughtIn.any { (child, parent) -> child !in ids || parent !in ids } ||
                reply.activity.any { (id, activity) ->
                    id !in ids || activity !in setOf("output", "idle", "attention")
                }
            ) malformed()
            RemoteSessionRoster(
                sessions = reply.sessions,
                withFiles = reply.withFiles.toSet(),
                stars = reply.stars.toSet(),
                broughtIn = reply.broughtIn,
                activity = reply.activity,
            )
        }

    fun usage(payload: ByteArray): List<RemoteUsageSource> =
        decode(UsageReply.serializer(), payload).sources.also { sources ->
            if (sources.size > 128 || sources.any { source ->
                    source.id.length !in 1..512 || source.name.length !in 1..512 ||
                        source.bars.size > 64 || source.amounts.size > 64 || source.notes.size > 64 ||
                        source.bars.any { !it.percent.isFinite() || it.percent !in 0.0..100.0 } ||
                        source.amounts.any { !it.amount.isFinite() || it.of?.isFinite() == false }
                }
            ) malformed()
        }

    fun starSession(sessionId: String, on: Boolean): ByteArray {
        requireIdentifier(sessionId)
        return encode(SessionStarPayload.serializer(), SessionStarPayload(sessionId, on))
    }

    fun renameSession(sessionId: String, title: String): ByteArray {
        requireIdentifier(sessionId)
        require(title.encodeToByteArray().size <= MAX_PATH_BYTES)
        return encode(SessionRenamePayload.serializer(), SessionRenamePayload(sessionId, title))
    }

    fun bringInSession(
        sessionId: String,
        agentId: String,
        model: String?,
        effort: String?,
        focus: String,
        rounds: Int,
        auto: Boolean,
    ): ByteArray {
        requireIdentifier(sessionId)
        requireIdentifier(agentId)
        model?.let(::requireIdentifier)
        effort?.let(::requireIdentifier)
        require(focus.encodeToByteArray().size <= MAX_PATH_BYTES)
        require(rounds in 1..3)
        return encode(
            SessionBringInPayload.serializer(),
            SessionBringInPayload(sessionId, agentId, model, effort, focus, rounds, auto),
        )
    }

    fun tabs(payload: ByteArray): List<RemoteTab> = decode(TabListReply.serializer(), payload).tabs.also {
        if (it.size > 128) malformed()
    }
    fun agents(payload: ByteArray): RemoteAgentRoster = decode(AgentListReply.serializer(), payload).let {
        if (it.agents.size > 64 || it.caps.size > 64) malformed()
        RemoteAgentRoster(it.agents, it.caps)
    }

    fun sessionPreview(payload: ByteArray): List<RemotePreviewMessage> =
        decode(SessionPreviewReply.serializer(), payload).messages.also { messages ->
            if (messages.size > 512 || messages.any {
                    it.role.length !in 1..64 || it.text.encodeToByteArray().size > 64 * 1_024
                } || messages.sumOf { it.text.encodeToByteArray().size } >= RemoteWireCodec.MAX_FRAME_BYTES
            ) malformed()
        }

    fun sessionChanges(payload: ByteArray): List<RemoteSessionChange> =
        decode(SessionChangesReply.serializer(), payload).changes.also { changes ->
            if (changes.size > 5_000 || changes.any {
                    it.path.isBlank() || it.path.encodeToByteArray().size > MAX_PATH_BYTES ||
                        it.name.encodeToByteArray().size > MAX_PATH_BYTES
                }
            ) malformed()
        }

    fun webPreview(payload: ByteArray): RemoteWebPreview =
        decode(SessionWebPreviewReply.serializer(), payload).let { reply ->
            if (
                reply.path != null && (!reply.available || !WEB_PREVIEW_PATH.matches(reply.path))
            ) malformed()
            RemoteWebPreview(reply.available, reply.path)
        }

    fun fileChunk(payload: ByteArray): RemoteFileChunk =
        decode(FileReadReply.serializer(), payload).let {
            if (it.path.isBlank() || it.path.encodeToByteArray().size > MAX_PATH_BYTES ||
                it.mime.length !in 1..128 || it.offset < 0 || it.total < 0 ||
                it.offset > it.total || it.data.size > MAX_UPLOAD_CHUNK_BYTES ||
                it.offset + it.data.size > it.total || it.eof != (it.offset + it.data.size == it.total)
            ) malformed()
            RemoteFileChunk(it.path, it.mime, it.offset, it.total, it.eof, it.data.copyOf())
        }

    fun attached(payload: ByteArray): AttachedTerminal = decode(AttachedReply.serializer(), payload).let {
        if (it.tabId.isBlank() || it.attachmentId.isBlank() || it.title.length > 4_096) malformed()
        AttachedTerminal(it.tabId, it.attachmentId, it.hasFocus, it.title)
    }

    fun openedTab(payload: ByteArray): String = decode(TabOpenedReply.serializer(), payload).tabId
    fun openedSessionTab(payload: ByteArray): String = decode(SessionOpenedReply.serializer(), payload).tabId
    fun startedAgentTab(payload: ByteArray): String = decode(AgentStartedReply.serializer(), payload).tabId

    fun focus(payload: ByteArray): RemoteFocusEvent = decode(FocusReply.serializer(), payload).let {
        RemoteFocusEvent(
            it.tabId,
            it.attachmentId,
            when (it.focus) {
                WireFocusOwner.Self -> FocusOwner.Self
                WireFocusOwner.Other -> FocusOwner.Other
                WireFocusOwner.Unowned -> FocusOwner.Unowned
            },
            it.size,
        )
    }

    fun title(payload: ByteArray): RemoteTitleEvent = decode(TitleReply.serializer(), payload).let {
        if (it.title.length > 4_096) malformed()
        RemoteTitleEvent(it.tabId, it.attachmentId, it.title)
    }

    fun terminalExited(payload: ByteArray): RemoteTerminalExitEvent =
        decode(TerminalExitedReply.serializer(), payload).let {
            if (it.tabId.isBlank() || it.attachmentId.isBlank()) malformed()
            RemoteTerminalExitEvent(it.tabId, it.attachmentId, it.exit)
        }

    private fun <T> encode(serializer: kotlinx.serialization.KSerializer<T>, value: T): ByteArray =
        cbor.encodeToByteArray(serializer, value).also {
            if (it.isEmpty() || it.size >= RemoteWireCodec.MAX_FRAME_BYTES) malformed()
        }

    private fun <T> decode(serializer: kotlinx.serialization.KSerializer<T>, payload: ByteArray): T =
        try {
            RemoteWireCodec.validateCborPayload(payload)
            cbor.decodeFromByteArray(serializer, payload)
        } catch (error: Exception) {
            throw RemoteProtocolException("malformed remote operation payload", error)
        }

    private fun malformed(): Nothing = throw RemoteProtocolException("invalid remote operation payload")

    private fun requireIdentifier(value: String) {
        if (value.isBlank() || value.encodeToByteArray().size > MAX_IDENTIFIER_BYTES) malformed()
    }

    private fun requireSubmissionId(value: String) {
        if (value.isBlank() || value.encodeToByteArray().size > MAX_SUBMISSION_ID_BYTES) malformed()
    }

    @Serializable private data class TabIdPayload(@SerialName("tab_id") val tabId: String)
    @Serializable private data class GatewayRoutesReply(
        val hosts: List<String>,
        val port: Int,
        @SerialName("relay_host") val relayHost: String? = null,
        @SerialName("relay_port") val relayPort: Int? = null,
    )
    @Serializable private data class DirectOfferReply(
        val id: String,
        val cookie: String,
        val host: String,
        val port: Int,
        @SerialName("expires_in_millis") val expiresInMillis: Long,
    )
    @Serializable private data class AttachmentPayload(
        @SerialName("tab_id") val tabId: String,
        @SerialName("attachment_id") val attachmentId: String,
    )
    @Serializable private data class InputPayload(
        @SerialName("tab_id") val tabId: String,
        @SerialName("attachment_id") val attachmentId: String,
        @ByteString val data: ByteArray,
    )
    @Serializable private data class SizedPayload(
        @SerialName("tab_id") val tabId: String,
        @SerialName("attachment_id") val attachmentId: String,
        val size: TerminalSize,
    )
    @Serializable private data class ScrollbackPayload(
        @SerialName("tab_id") val tabId: String,
        @SerialName("attachment_id") val attachmentId: String,
        val offset: Int,
        val count: Int,
    )
    @Serializable private data class SessionIdPayload(@SerialName("session_id") val sessionId: String)
    @Serializable private data class SessionConversationPayload(
        @SerialName("session_id") val sessionId: String,
        @SerialName("max_chars") val maxChars: Int,
    )
    @Serializable private data class SessionWebPreviewRequest(
        @SerialName("session_id") val sessionId: String,
        val open: Boolean,
    )
    @Serializable private data class FileReadRequest(
        @SerialName("session_id") val sessionId: String,
        val path: String,
        val offset: Long,
        val count: Int,
    )
    @Serializable private data class SessionOpenPayload(
        @SerialName("session_id") val sessionId: String,
        val size: TerminalSize,
    )
    @Serializable private data class SessionClosePayload(
        @SerialName("session_id") val sessionId: String,
        @SerialName("tab_id") val tabId: String?,
    )
    @Serializable private data class TabOpenPayload(
        val kind: String = "shell",
        @SerialName("project_path") val projectPath: String?,
        val title: String?,
        val size: TerminalSize,
    )
    @Serializable private data class AgentStartPayload(
        val action: String = "start",
        @SerialName("agent_id") val agentId: String,
        val model: String?,
        val effort: String?,
        val cwd: String,
        val title: String,
        val size: TerminalSize,
    )
    @Serializable private data class UploadBeginPayload(
        @SerialName("tab_id") val tabId: String,
        @SerialName("attachment_id") val attachmentId: String,
        @SerialName("submission_id") val submissionId: String,
        @SerialName("submission_count") val submissionCount: Int,
        @SerialName("member_index") val memberIndex: Int,
        @SerialName("submission_bytes") val submissionBytes: Long,
        val length: Long,
        @SerialName("media_type") val mediaType: String = "image/jpeg",
        @ByteString val sha256: ByteArray,
    )
    @Serializable private data class UploadChunkPayload(
        @SerialName("upload_id") val uploadId: String,
        val index: Int,
        @ByteString val data: ByteArray,
    )
    @Serializable private data class UploadIdPayload(@SerialName("upload_id") val uploadId: String)
    @Serializable private data class UploadBeginReply(
        @SerialName("upload_id") val uploadId: String,
        @SerialName("next_chunk") val nextChunk: Int,
        val path: String? = null,
    )
    @Serializable private data class UploadFinishReply(val path: String)
    @Serializable private data class UploadSuccessReply(val ok: Boolean)
    @Serializable private data class SessionListReply(
        val sessions: List<RemoteSession>,
        @SerialName("with_files") val withFiles: List<String> = emptyList(),
        val stars: List<String> = emptyList(),
        @SerialName("brought_in") val broughtIn: Map<String, String> = emptyMap(),
        val activity: Map<String, String> = emptyMap(),
    )
    @Serializable private data class UsageReply(val sources: List<RemoteUsageSource>)
    @Serializable private data class SessionStarPayload(
        @SerialName("session_id") val sessionId: String,
        val on: Boolean,
    )
    @Serializable private data class SessionRenamePayload(
        @SerialName("session_id") val sessionId: String,
        val title: String,
    )
    @Serializable private data class SessionBringInPayload(
        @SerialName("session_id") val sessionId: String,
        @SerialName("agent_id") val agentId: String,
        val model: String?,
        val effort: String?,
        val focus: String,
        val rounds: Int,
        val auto: Boolean,
    )
    @Serializable private data class SessionPreviewReply(val messages: List<RemotePreviewMessage>)
    @Serializable private data class SessionChangesReply(val changes: List<RemoteSessionChange>)
    @Serializable private data class SessionWebPreviewReply(
        val available: Boolean,
        val path: String? = null,
    )
    @Serializable private data class FileReadReply(
        val path: String,
        val mime: String,
        val offset: Long,
        val total: Long,
        val eof: Boolean,
        @ByteString val data: ByteArray,
    )
    @Serializable private data class TabListReply(val tabs: List<RemoteTab>)
    @Serializable private data class AgentListReply(
        val agents: List<RemoteAgentChoice>,
        val caps: Map<String, RemoteAgentCaps>,
    )
    @Serializable private data class AttachedReply(
        @SerialName("tab_id") val tabId: String,
        @SerialName("attachment_id") val attachmentId: String,
        @SerialName("has_focus") val hasFocus: Boolean,
        val title: String,
    )
    @Serializable private data class TabOpenedReply(@SerialName("tab_id") val tabId: String)
    @Serializable private data class SessionOpenedReply(
        @SerialName("tab_id") val tabId: String,
        @SerialName("selected_existing") val selectedExisting: Boolean,
    )
    @Serializable private data class AgentStartedReply(
        @SerialName("tab_id") val tabId: String,
        @SerialName("session_id") val sessionId: String?,
    )
    @Serializable private data class FocusReply(
        @SerialName("tab_id") val tabId: String,
        @SerialName("attachment_id") val attachmentId: String,
        val focus: WireFocusOwner,
        val size: TerminalSize,
    )
    @Serializable private data class TitleReply(
        @SerialName("tab_id") val tabId: String,
        @SerialName("attachment_id") val attachmentId: String,
        val title: String,
    )
    @Serializable private data class TerminalExitedReply(
        @SerialName("tab_id") val tabId: String,
        @SerialName("attachment_id") val attachmentId: String,
        val exit: RemoteTabExit,
    )

    private val WEB_PREVIEW_PATH = Regex("/v1/preview/[A-Za-z0-9_-]{43}/")
}
