package com.fivelime.aiterm

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import java.util.concurrent.TimeUnit

@Serializable
data class Session(
    val id: String,
    val agent: String,
    val title: String,
    val project_path: String,
    val group_path: String,
    val branch: String? = null,
    val forked: Boolean = false,
    val background: Boolean = false,
    val fork_parent: String? = null,
    val last_active: Long = 0,
)

@Serializable
data class SessionsResponse(
    val sessions: List<Session>,
    val running: List<String>,
    val open: List<String>,
    /** session id → "working" | "attention" | "idle", for sessions open on the desktop. */
    val activity: Map<String, String> = emptyMap(),
    /** Sessions that produced at least one file, per the desktop's ledger. */
    val with_files: List<String> = emptyList(),
    /** session id → ports its process tree is listening on (dev servers). */
    val ports: Map<String, List<Int>> = emptyMap(),
    /** Starred sessions — kept on top, synced with the desktop. */
    val stars: List<String> = emptyList(),
    /** Brought-in session → the master it joined, from relay lineage. */
    val brought_in: Map<String, String> = emptyMap(),
)

@Serializable
data class TerminalOpenBody(val cwd: String? = null, val cols: Int? = null, val rows: Int? = null)

@Serializable
data class TerminalOpened(val tab_id: String, val title: String, val cwd: String)

@Serializable
data class TerminalScreenData(val lines: List<String>, val cols: Int, val rows: Int)

@Serializable
data class UsageBar(val kind: String, val label: String, val percent: Double, val severity: String = "", val resets_at: String = "")

@Serializable
data class UsageAmount(val label: String, val amount: Double, val of: Double? = null, val currency: String? = null)

@Serializable
data class UsageSource(
    val id: String, val name: String, val state: String, val detail: String = "", val plan: String = "",
    val bars: List<UsageBar> = emptyList(), val amounts: List<UsageAmount> = emptyList(),
)

@Serializable
data class Turn(val role: String, val text: String)

@Serializable
data class ModelOption(val id: String, val display_name: String, val efforts: List<String> = emptyList(), val default_effort: String? = null)

@Serializable
data class Agent(val id: String, val display_name: String, val models: List<ModelOption> = emptyList())

/** A file the desktop now holds, by the path the agent will read it at. */
data class Attachment(val name: String, val path: String)

/** One entry of a workspace directory, as the desktop's explorer lists it. */
@Serializable
data class DirEntry(val name: String, val path: String, val is_dir: Boolean)

/** A file a session produced, as the desktop lists it. */
@Serializable
data class FileEntry(val path: String, val name: String, val bytes: Long, val modified: Long, val via: String) {
    val ext: String get() = name.substringAfterLast('.', "").lowercase()
    val kind: String get() = when (ext) {
        "png", "jpg", "jpeg", "webp", "gif", "bmp" -> "image"
        "mp4", "webm", "mov", "mkv", "m4v" -> "video"
        "mp3", "wav", "m4a", "ogg", "flac" -> "audio"
        "md", "txt", "json", "yaml", "yml", "toml", "csv", "log", "html", "css", "js", "ts", "tsx", "jsx", "py", "rs", "kt", "kts", "java", "sh", "sql", "xml", "svg", "env", "ini", "cfg" -> "text"
        "pdf" -> "pdf"
        else -> "other"
    }
}

@Serializable
data class Status(
    val api: Int, val name: String, val version: String,
    /** Every address the desktop answers on right now, LAN first, public
     *  last. Fresher than what the QR carried at pairing time. */
    val hosts: List<String> = emptyList(),
    /** iroh node id, when this desktop can be dialed by key. */
    val iroh: String? = null,
    /** The live AITerm Relay route, never a draft; null when the relay road
     *  is off or nothing is enrolled yet. */
    val relay: RelayRoute? = null,
    /** An enrollment draft waiting for any paired phone to sign — the same
     *  digest a QR carries as `ta`. Present only while no route lives;
     *  the phone signs it once and the route comes back live. */
    val relay_enroll: RelayEnroll? = null,
    /** Why no draft is waiting: the desktop could not reach the relay. */
    val relay_error: String? = null,
    /** Which roads the desktop has switched on. */
    val roads: RoadFlags? = null,
    /** The desktop's own road order, most preferred first. A phone that
     *  has not set its own order follows it. */
    val road_order: List<String>? = null,
)

@Serializable
data class RelayRoute(val host: String, val port: Int)

/** A waiting enrollment draft: its 32-byte digest, base64url no padding. */
@Serializable
data class RelayEnroll(val digest: String)

@Serializable
data class RoadFlags(val lan: Boolean = false, val vpn: Boolean = false, val relay: Boolean = false, val iroh: Boolean = false)

/** What `POST /v1/relay/enroll` hands back: the route that just went live. */
@Serializable
data class RelayEnrolled(val host: String, val port: Int)

class ApiError(val code: Int, message: String) : Exception(message)

/** The desktop's remote API. Plain HTTP with a bearer token; see the
 *  desktop's remote.rs for what each call does there. */
class Api(val baseUrl: String, private val token: String, fingerprint: String, connectSeconds: Long = 4) {
    private val json = Json { ignoreUnknownKeys = true }
    private val http = pinnedClient(
        fingerprint,
        OkHttpClient.Builder()
            .connectTimeout(connectSeconds, TimeUnit.SECONDS)
            .readTimeout(30, TimeUnit.SECONDS)
            .pingInterval(25, TimeUnit.SECONDS),
    )

    private fun req(path: String) = Request.Builder().url(baseUrl + path).header("Authorization", "Bearer $token")
        .header("X-Aiterm-Device", DEVICE).header("X-Aiterm-Os", OS).header("X-Aiterm-App", APP_VERSION)

    private suspend fun call(b: Request.Builder): String = withContext(Dispatchers.IO) {
        http.newCall(b.build()).execute().use { r ->
            val body = r.body.string()
            if (!r.isSuccessful) {
                val msg = runCatching { json.parseToJsonElement(body).jsonObject["error"]?.jsonPrimitive?.content }.getOrNull()
                throw ApiError(r.code, msg ?: "HTTP ${r.code}")
            }
            body
        }
    }

    private fun jsonBody(s: String) = s.toRequestBody("application/json".toMediaType())
    private val empty = ByteArray(0).toRequestBody(null)

    suspend fun status(): Status = json.decodeFromString(call(req("/v1/status")))
    /** Sign the desktop's pending relay draft into a live route. Both
     *  fields base64url, no padding: the phone's compressed P-256 key and a
     *  DER ECDSA signature over the draft digest (the QR's `ta`, or
     *  `relay_enroll.digest` from status). 409 = no draft waiting, 400 =
     *  the signature does not fit (or the draft was replaced — read status
     *  again), 502 = the relay said no. */
    suspend fun relayEnroll(authorityPublicKeyB64Url: String, signatureDerB64Url: String): RelayEnrolled {
        val body = json.encodeToString(RelayEnrollBody.serializer(), RelayEnrollBody(authorityPublicKeyB64Url, signatureDerB64Url))
        return json.decodeFromString(call(req("/v1/relay/enroll").post(jsonBody(body))))
    }
    suspend fun sessions(): SessionsResponse = json.decodeFromString(call(req("/v1/sessions")))
    /** The old whole-transcript read. Kept only as the fallback for a
     *  desktop too old to serve /v1/spine. */
    suspend fun conversation(id: String): List<Turn> = json.decodeFromString(call(req("/v1/sessions/$id/conversation")))
    /** Everything on this session's spine after `after` (0 = all). Asking
     *  is also how the desktop learns a phone is watching: it starts (or
     *  keeps) the adapter tail. See docs/architecture/spine.md. */
    suspend fun spine(id: String, after: Long): SpineResponse =
        SpineResponse.parse(json.parseToJsonElement(call(req("/v1/sessions/$id/spine?after=$after"))).jsonObject)
    suspend fun agents(): List<Agent> = json.decodeFromString(call(req("/v1/agents")))
    suspend fun usage(): List<UsageSource> = json.decodeFromString(call(req("/v1/usage")))
    suspend fun search(q: String): List<Session> = json.decodeFromString(call(req("/v1/search?q=" + java.net.URLEncoder.encode(q, "UTF-8"))))
    suspend fun files(id: String): List<FileEntry> = json.decodeFromString(call(req("/v1/sessions/$id/files")))
    suspend fun browse(path: String): List<DirEntry> = json.decodeFromString(call(req("/v1/browse?path=" + java.net.URLEncoder.encode(path, "UTF-8"))))

    /** Fetch a produced file into the app's cache, through the pinned
     *  connection, and hand back the local copy. Viewers work on files. */
    suspend fun download(entry: FileEntry, cacheDir: java.io.File): java.io.File = withContext(Dispatchers.IO) {
        val dir = java.io.File(cacheDir, "files").apply { mkdirs() }
        val safe = entry.name.replace(Regex("[^A-Za-z0-9._-]"), "_")
        val target = java.io.File(dir, "${entry.path.hashCode().toUInt()}-${entry.modified}-$safe")
        if (target.exists() && target.length() == entry.bytes) return@withContext target
        val url = baseUrl + "/v1/files?path=" + java.net.URLEncoder.encode(entry.path, "UTF-8")
        http.newCall(req("").url(url).build()).execute().use { r ->
            if (!r.isSuccessful) throw ApiError(r.code, "HTTP ${r.code}")
            target.outputStream().use { out -> r.body.byteStream().copyTo(out) }
        }
        target
    }
    /** A preview ticket: an unguessable path on the desktop that serves a
     *  folder of static files or proxies a loopback port — the WebView loads
     *  it with no headers needed. */
    suspend fun makePreview(port: Int? = null, dir: String? = null): String {
        val body = json.encodeToString(PreviewBody.serializer(), PreviewBody(port, dir))
        val r = call(req("/v1/previews").post(jsonBody(body)))
        return json.parseToJsonElement(r).jsonObject["path"]?.jsonPrimitive?.content ?: throw ApiError(500, "no path")
    }

    /** Make a folder (and parents) on the desktop, under its home. */
    suspend fun mkdir(path: String) {
        val body = json.encodeToString(DirBody.serializer(), DirBody(path))
        call(req("/v1/dirs").post(jsonBody(body)))
    }
    /** Ask the desktop to bring a second agent into a session's tab. */
    suspend fun bringIn(id: String, agentId: String, model: String?, focus: String, rounds: Int, auto: Boolean) {
        val body = json.encodeToString(BringInBody.serializer(), BringInBody(agentId, model, focus, rounds, auto))
        call(req("/v1/sessions/$id/bringin").post(jsonBody(body)))
    }
    /** Star or unstar; the desktop keeps the list. */
    suspend fun star(id: String, on: Boolean) {
        val body = json.encodeToString(StarBody.serializer(), StarBody(on))
        call(req("/v1/sessions/$id/star").post(jsonBody(body)))
    }
    /** A person-chosen title; blank restores the engine's own name. */
    suspend fun rename(id: String, title: String) {
        val body = json.encodeToString(RenameBody.serializer(), RenameBody(title))
        call(req("/v1/sessions/$id/rename").post(jsonBody(body)))
    }
    suspend fun interrupt(id: String) { call(req("/v1/sessions/$id/interrupt").post(empty)) }
    suspend fun open(id: String) { call(req("/v1/sessions/$id/open").post(empty)) }
    suspend fun stop(id: String) { call(req("/v1/sessions/$id/stop").post(empty)) }
    suspend fun input(id: String, text: String) {
        val body = json.encodeToString(InputBody.serializer(), InputBody(text))
        call(req("/v1/sessions/$id/input").post(jsonBody(body)))
    }
    /** Raw keystrokes into the session's terminal, no Enter appended — what
     *  answers a TUI dialog (an approval prompt) the conversation view
     *  cannot render. Enter, when wanted, is itself the key ("\r"). */
    suspend fun inputKeys(id: String, keys: String) {
        val body = json.encodeToString(InputBody.serializer(), InputBody(keys, enter = false))
        call(req("/v1/sessions/$id/input").post(jsonBody(body)))
    }
    suspend fun terminalOpen(cols: Int, rows: Int): TerminalOpened {
        val body = json.encodeToString(TerminalOpenBody.serializer(), TerminalOpenBody(cols = cols, rows = rows))
        return json.decodeFromString(call(req("/v1/terminal").post(jsonBody(body))))
    }
    suspend fun terminalScreen(tab: String): TerminalScreenData =
        json.decodeFromString(call(req("/v1/terminal/$tab/screen")))
    suspend fun terminalInput(tab: String, text: String, enter: Boolean = true) {
        val body = json.encodeToString(InputBody.serializer(), InputBody(text, enter = enter))
        call(req("/v1/terminal/$tab/input").post(jsonBody(body)))
    }
    suspend fun terminalClose(tab: String) { call(req("/v1/terminal/$tab/close").post(empty)) }

    suspend fun newSession(agentId: String, cwd: String, prompt: String?, model: String?, effort: String?, title: String?) {
        val body = json.encodeToString(NewSessionBody.serializer(), NewSessionBody(agentId, cwd, prompt, model, effort, title))
        call(req("/v1/sessions").post(jsonBody(body)))
    }
    suspend fun upload(name: String, bytes: ByteArray): Attachment {
        val r = call(req("/v1/uploads").header("X-Filename", name).post(bytes.toRequestBody(null)))
        val path = json.parseToJsonElement(r).jsonObject["path"]?.jsonPrimitive?.content ?: throw ApiError(500, "no path")
        return Attachment(name, path)
    }

    /** Subscribe to the desktop's event stream. Each event is a nudge to
     *  re-read, never data. */
    fun events(onOpen: () -> Unit, onEvent: (type: String, obj: kotlinx.serialization.json.JsonObject) -> Unit, onClosed: () -> Unit): WebSocket {
        val wsUrl = baseUrl.replaceFirst("https", "wss") + "/v1/events?token=$token"
        val wsReq = Request.Builder().url(wsUrl)
            .header("X-Aiterm-Device", DEVICE).header("X-Aiterm-Os", OS).header("X-Aiterm-App", APP_VERSION).build()
        return http.newWebSocket(wsReq, object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: okhttp3.Response) = onOpen()
            override fun onMessage(webSocket: WebSocket, text: String) {
                val obj = runCatching { json.parseToJsonElement(text).jsonObject }.getOrNull() ?: return
                val type = obj["type"]?.jsonPrimitive?.content ?: return
                onEvent(type, obj)
            }
            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) = onClosed()
            override fun onFailure(webSocket: WebSocket, t: Throwable, response: okhttp3.Response?) = onClosed()
        })
    }

    companion object {
        const val APP_VERSION = "0.2"
        /** What the desktop lists under "Connected now". */
        val DEVICE: String = listOf(android.os.Build.MANUFACTURER, android.os.Build.MODEL)
            .filter { it.isNotBlank() }.joinToString(" ").replaceFirstChar { it.uppercase() }
        val OS: String = "Android ${android.os.Build.VERSION.RELEASE}"
    }

    @Serializable private data class PreviewBody(val port: Int? = null, val dir: String? = null)
    @Serializable private data class DirBody(val path: String)
    @Serializable private data class BringInBody(val agent_id: String, val model: String? = null, val focus: String, val rounds: Int, val auto: Boolean = false)
    @Serializable private data class StarBody(val on: Boolean)
    @Serializable private data class RenameBody(val title: String)
    @Serializable private data class RelayEnrollBody(val authority_public_key: String, val signature_der: String)
    @Serializable private data class InputBody(val text: String, val enter: Boolean = true)
    @Serializable private data class NewSessionBody(
        val agent_id: String, val cwd: String, val prompt: String?,
        val model: String? = null, val effort: String? = null, val title: String? = null,
    )
}
