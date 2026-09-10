package com.fivelime.aiterm

import android.app.Application
import android.net.ConnectivityManager
import android.net.Network
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.WebSocket
import java.io.IOException

/** Where a second-agent relay stands, as the desktop reports it. */
data class RelayInfo(
    val bName: String,
    val bSessionId: String?,
    val phase: String,
    val round: Int,
    val rounds: Int,
    val note: String,
)

/** What a session is doing, as the phone shows it. Order matters for sorting. */
enum class SessionState { Working, NeedsYou, OnDesktop, Running, Idle }

/** All state the screens read. The desktop is the source of truth; this is
 *  a cache of it plus what the person is doing right now. Nothing here needs
 *  saving: coming back to the app re-reads everything. */
/** How many pages one catch-up walks before letting the WebSocket take over. */
private const val MAX_CATCH_UP_PAGES = 64

class AppViewModel(app: Application) : AndroidViewModel(app) {
    private val store = Store(app)

    /** Every desktop this phone is paired with, in pairing order. */
    var desktops by mutableStateOf(store.loadAll()); private set
    /** The one being shown. Everything below caches its state. */
    var desktop by mutableStateOf(desktops.find { it.fingerprint == store.activeFingerprint } ?: desktops.firstOrNull()); private set
    var connected by mutableStateOf(false); private set
    var pairing by mutableStateOf(false); private set
    var sessions by mutableStateOf<List<Session>>(emptyList()); private set
    var running by mutableStateOf<Set<String>>(emptySet()); private set
    var open by mutableStateOf<Set<String>>(emptySet()); private set
    var activity by mutableStateOf<Map<String, String>>(emptyMap()); private set
    var usage by mutableStateOf<List<UsageSource>>(emptyList()); private set
    var query by mutableStateOf("")
        private set
    /** What the desktop's index found for `query`; null while it has not answered. */
    var results by mutableStateOf<List<Session>?>(null); private set
    private var searchJob: Job? = null
    var files by mutableStateOf<List<FileEntry>>(emptyList()); private set
    var loadingFiles by mutableStateOf(false); private set
    /** A produced file open full-screen, with its local copy. */
    var viewing by mutableStateOf<Pair<FileEntry, java.io.File>?>(null)
    var opening by mutableStateOf<String?>(null); private set
    var showFiles by mutableStateOf(false)
    /** Files view: what the session produced, or the workspace folder tree. */
    var browsing by mutableStateOf(false)
    var browsePath by mutableStateOf("")
    var browseEntries by mutableStateOf<List<DirEntry>>(emptyList()); private set
    var browseLoading by mutableStateOf(false); private set

    val browseRoot: String get() = selected?.group_path ?: ""

    fun browseTo(path: String) {
        val a = api ?: return
        browsePath = path
        viewModelScope.launch {
            browseLoading = true
            try { browseEntries = a.browse(path).sortedWith(compareBy({ !it.is_dir }, { it.name.lowercase() })) }
            catch (e: Exception) { notice = describe(e) }
            browseLoading = false
        }
    }

    /** Up one folder; false when already at the workspace root. */
    fun browseUp(): Boolean {
        if (browsePath.isEmpty() || browsePath == browseRoot) return false
        browseTo(browsePath.substringBeforeLast('/').ifEmpty { "/" })
        return true
    }

    /** Subfolders of a remote path, for the new-session folder picker. */
    suspend fun listDirs(path: String): List<DirEntry> =
        api?.browse(path)?.filter { it.is_dir }?.sortedBy { it.name.lowercase() } ?: emptyList()

    /** Make a folder on the desktop; throws on refusal so the caller can say so. */
    suspend fun createDir(path: String) { api?.mkdir(path) }

    fun openBrowsed(e: DirEntry) {
        if (e.is_dir) browseTo(e.path)
        else open(FileEntry(e.path, e.name, 0, 0, "browsed"))
    }
    /** The new-session page is up. */
    var composingNew by mutableStateOf(false)
    /** Files uploaded for the message being written, in either composer. */
    var attachments by mutableStateOf<List<Attachment>>(emptyList()); private set
    var uploading by mutableStateOf(false); private set
    /** Set when a message goes out; cleared when the transcript grows past it
     *  or the desktop reports activity. Bridges the gap before the agent's
     *  first progress report so "working" shows immediately. */
    private var sentAt = 0L
    private var awaitingReply = false
    var agents by mutableStateOf<List<Agent>>(emptyList()); private set
    var selected by mutableStateOf<Session?>(null); private set

    /** The selected session's spine: what the screen draws, fed by the GET
     *  once and by "spine" WebSocket frames after. Held here (not in a
     *  Composable) so a rotation or a trip to Files keeps the transcript. */
    private val spine = ConversationStore()
    /** The store's rows, republished after every apply — an immutable list
     *  of equal data classes, so only the row that moved recomposes. */
    var items by mutableStateOf<List<Item>>(emptyList()); private set
    /** The selected session's phase, straight off the spine. */
    var phase by mutableStateOf(SpinePhase.Idle); private set
    var phaseDetail by mutableStateOf(""); private set
    /** When the last spine event or fetch landed — the safety net's clock. */
    private var lastSpineAt = 0L
    /** A desktop too old to serve /v1/spine (404): fall back to the whole
     *  transcript on a slow poll, mapped into the same rows. */
    private var noSpine = false
    private var fetchingSpine = false
    private var refetchWanted = false
    var loadingTurns by mutableStateOf(false); private set
    var sending by mutableStateOf(false); private set
    /** A one-line message for the snackbar. The UI clears it after showing. */
    var notice by mutableStateOf<String?>(null)

    private var ws: WebSocket? = null
    private var foreground = false
    private var refreshJob: Job? = null
    private var connectJob: Job? = null
    /** Bumped by every connect(); a late better-route switch from an older
     *  connect must not clobber a newer one. */
    private var connectGen = 0
    private val api: Api? get() = desktop?.let { Api(it.baseUrl, it.token, it.fingerprint) }

    /** The default network changing (home Wi‑Fi ↔ cellular) is the one moment
     *  the current address is most likely wrong — and the one moment nothing
     *  else notices: a WebSocket opened over cellular stays pinned to cellular
     *  and keeps answering pings after Wi‑Fi takes over, so the app looks
     *  connected while every new request times out against the public IP. */
    private val connectivity = app.getSystemService(ConnectivityManager::class.java)
    private var lastNetwork: Network? = null
    private val netCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            val changed = lastNetwork != null && lastNetwork != network
            lastNetwork = network
            if (!changed) return // initial callback: onStart's connect covers it
            viewModelScope.launch {
                delay(500) // let routes settle
                if (foreground && desktop != null) { connectJob?.cancel(); connect() }
            }
        }
    }

    init { runCatching { connectivity.registerDefaultNetworkCallback(netCallback) } }
    override fun onCleared() { runCatching { connectivity.unregisterNetworkCallback(netCallback) } }

    // ---- terminal: a plain shell on the desktop, driven from here

    /** Tab id of the open remote terminal; a screen shows while set. */
    var terminalTab by mutableStateOf<String?>(null)
    var terminalTitle by mutableStateOf("Terminal"); private set
    var terminalLines by mutableStateOf<List<String>>(emptyList()); private set
    var terminalOpening by mutableStateOf(false); private set

    /** A shell on the desktop — in `cwd` when a session asks for one, so a
     *  terminal opened from a conversation starts in that session's folder. */
    fun openTerminal(cwd: String? = null) {
        val a = api ?: return
        if (terminalOpening) return
        viewModelScope.launch {
            terminalOpening = true
            try {
                val t = a.terminalOpen(cols = 60, rows = 24, cwd = cwd?.takeIf { it.isNotBlank() })
                terminalTitle = t.title
                terminalLines = emptyList()
                terminalTab = t.tab_id
            } catch (e: Exception) { notice = describe(e) }
            finally { terminalOpening = false }
        }
    }

    suspend fun pollTerminal(): Boolean {
        val a = api ?: return false
        val tab = terminalTab ?: return false
        return try {
            terminalLines = a.terminalScreen(tab).lines
            true
        } catch (e: ApiError) {
            // The tab ended on the desktop; the screen has nothing to show.
            if (e.code == 404) { terminalTab = null; false } else true
        } catch (_: Exception) { true }
    }

    fun sendTerminal(text: String, enter: Boolean = true) {
        val a = api ?: return
        val tab = terminalTab ?: return
        viewModelScope.launch {
            try { a.terminalInput(tab, text, enter) }
            catch (e: Exception) { notice = describe(e) }
        }
    }

    /** Done with it: ends the shell and removes the tab on the desktop too. */
    fun closeTerminal() {
        val a = api ?: return
        val tab = terminalTab ?: return
        terminalTab = null
        viewModelScope.launch { runCatching { a.terminalClose(tab) } }
    }

    // ---- settings

    var showSettings by mutableStateOf(false)
    /** The drawer's usage section is folded until asked for, every launch. */
    var usageOpen by mutableStateOf(false)
    var themeName by mutableStateOf(store.theme); private set
    var timeZone by mutableStateOf(store.timeZone); private set
    var biometric by mutableStateOf(store.biometric); private set
    /** The app is showing its lock screen; a successful prompt clears it. */
    var locked by mutableStateOf(store.biometric && store.loadAll().isNotEmpty())
    private var pausedAt = 0L

    fun setBiometricEnabled(on: Boolean) {
        biometric = on; store.biometric = on
        if (!on) locked = false
    }

    fun setTheme(name: String) {
        themeName = name; store.theme = name
        com.fivelime.aiterm.ui.setPalette(name)
    }

    fun setTz(zone: String) {
        timeZone = zone; store.timeZone = zone
        com.fivelime.aiterm.ui.displayZone = zone.takeIf { it.isNotEmpty() }?.let { java.util.TimeZone.getTimeZone(it) }
    }

    init { setTheme(store.theme); setTz(store.timeZone) }

    // ---- pairing

    /** The pair screen is up over an already-paired app, to add a desktop. */
    var showPair by mutableStateOf(false)

    fun pair(raw: String) {
        val link = PairLink.parse(raw)
        if (link == null) { notice = "That is not an AITerm pairing code"; return }
        viewModelScope.launch {
            pairing = true
            try {
                // Every road the QR offers, in the default order: its
                // addresses, the relay route it names, and — when the
                // desktop has one — its iroh node id via the local bridge,
                // so pairing succeeds even on a network where no address is
                // reachable at all.
                val bridge = if (link.iroh.isNotEmpty()) IrohBridge.urlFor(getApplication(), link.iroh) else null
                val draft = Desktop(
                    link.candidates.first(), link.token, link.name, link.candidates, link.fingerprint, link.iroh,
                    relayHost = link.relayHost, relayPort = link.relayPort,
                )
                for (c in Roads.candidates(draft, bridge)) {
                    val url = c.url
                    val t0 = System.currentTimeMillis()
                    val status = try { Api(url, link.token, link.fingerprint, c.patienceSeconds).status() } catch (e: IOException) {
                        android.util.Log.i("Aiterm", "pair probe $url → ${e.javaClass.simpleName}: ${e.message} in ${System.currentTimeMillis() - t0}ms")
                        continue
                    } catch (e: ApiError) {
                        notice = if (e.code == 401) "The desktop refused this code — show a fresh QR" else e.message; return@launch
                    }
                    android.util.Log.i("Aiterm", "pair probe $url → ok in ${System.currentTimeMillis() - t0}ms")
                    if (status.api != 1) { notice = "This desktop speaks a newer protocol — update the app"; return@launch }
                    // The relay route: what the desktop reports live, else
                    // what the QR named; then, when the QR carried a draft,
                    // sign it so the route goes live now. A refusal here is
                    // not a failed pairing — the desktop is reached, and the
                    // next status answer offers the draft again.
                    var relayHost = status.relay?.host ?: link.relayHost
                    var relayPort = status.relay?.port ?: link.relayPort
                    link.relayAuthorization?.let { digest ->
                        enrollTried[link.fingerprint] = b64url(digest)
                        enrollRelay(Api(url, link.token, link.fingerprint, c.patienceSeconds), digest)?.let { relayHost = it.host; relayPort = it.port }
                    }
                    val d = draft.copy(
                        baseUrl = url, name = status.name.ifBlank { link.name },
                        relayHost = relayHost, relayPort = relayPort,
                        roadOrder = status.road_order?.takeIf { Roads.isComplete(it) }?.let { Roads.order(it).map { r -> r.id } } ?: draft.roadOrder,
                    )
                    adopt(d)
                    return@launch
                }
                notice = "Could not reach ${link.name} at ${link.hosts.joinToString()} — same Wi‑Fi or Tailscale?"
            } finally { pairing = false }
        }
    }

    private fun b64url(bytes: ByteArray): String = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)

    // ---- the relay road, enrolled over the pairing that already exists

    /** Sign a relay enrollment digest and hand it to the desktop, which
     *  registers the route and answers with it live. The one function for
     *  both ways a digest reaches the phone: the QR's `ta` at pairing, and
     *  `relay_enroll` in any status answer afterwards. Null when it did not
     *  go through — never fatal to anything around it. */
    private suspend fun enrollRelay(api: Api, digest: ByteArray): RelayEnrolled? = try {
        val key = b64url(RelayAuthority.publicKeyCompressed())
        val sig = b64url(RelayAuthority.sign(digest))
        api.relayEnroll(key, sig).also { android.util.Log.i("Aiterm", "relay enrolled: ${it.host}:${it.port}") }
    } catch (e: Exception) {
        android.util.Log.w("Aiterm", "relay enrollment failed: ${e.javaClass.simpleName}: ${e.message}")
        null
    }

    /** Per desktop (by fingerprint), the last digest this phone signed — a
     *  digest the desktop refused is not signed again on every status read;
     *  a fresh draft is a fresh digest and gets its one attempt. */
    private val enrollTried = HashMap<String, String>()

    /** Per desktop, the road order it last published — what "Use desktop's
     *  order" goes back to before the next status answer arrives. */
    private val publishedOrder = HashMap<String, List<String>>()

    /** A draft waiting in a status answer, and no live route: sign it and
     *  keep the route that comes back. This is how a phone paired over any
     *  road — iroh, the LAN — gains the relay road with no new QR. Runs
     *  off every status result: the connect sprint, `status_changed`, the
     *  drawer's reachability probe. */
    private fun enrollFromStatus(api: Api, fingerprint: String, status: Status) {
        val digest = status.relay_enroll?.digest ?: return
        if (status.relay != null || enrollTried[fingerprint] == digest) return
        enrollTried[fingerprint] = digest
        val bytes = PairLink.decodeBase64Url(digest)?.takeIf { it.size == 32 }
        if (bytes == null) { android.util.Log.w("Aiterm", "relay enrollment digest unreadable; ignoring"); return }
        android.util.Log.i("Aiterm", "relay enrollment offered in status; signing it")
        viewModelScope.launch {
            val r = enrollRelay(api, bytes) ?: return@launch
            val cur = desktops.find { it.fingerprint == fingerprint } ?: return@launch
            replace(cur.copy(relayHost = r.host, relayPort = r.port))
        }
    }

    /** What every status answer teaches about a desktop's roads: its name,
     *  iroh node, live relay route, and — unless the person set their own
     *  here — its road order. The caller stores the copy. */
    private fun roadsFrom(d: Desktop, status: Status): Desktop {
        val published = status.road_order?.takeIf { Roads.isComplete(it) }?.let { Roads.order(it).map { r -> r.id } }
        if (published != null) publishedOrder[d.fingerprint] = published
        return d.copy(
            iroh = status.iroh ?: d.iroh,
            name = status.name.ifBlank { d.name },
            relayHost = status.relay?.host ?: "",
            relayPort = status.relay?.port ?: 0,
            roadOrder = if (!d.roadOrderCustom && published != null) published else d.roadOrder,
        )
    }

    /** One desktop's entry, replaced and saved; shown too when it is the
     *  one on screen. */
    private fun replace(nd: Desktop) {
        desktops = desktops.map { if (it.fingerprint == nd.fingerprint) nd else it }
        store.saveAll(desktops)
        if (desktop?.fingerprint == nd.fingerprint) desktop = nd
    }

    /** The order this desktop's roads are tried, from the settings screen.
     *  Saved on the desktop's entry as the person's own — the desktop's
     *  published order no longer applies — and used at once: the next
     *  sprint (now) commits and upgrades by the new order. */
    fun setRoadOrder(d: Desktop, order: List<String>) {
        val clean = Roads.order(order).map { it.id }
        if (clean == d.roadOrder && d.roadOrderCustom) return
        replace(d.copy(roadOrder = clean, roadOrderCustom = true))
        if (desktop?.fingerprint == d.fingerprint && foreground && clean != d.roadOrder) { connectJob?.cancel(); connect() }
    }

    /** Back to following the desktop: the flag clears, the order it last
     *  published applies now, and every status answer keeps it current. */
    fun useDesktopRoadOrder(d: Desktop) {
        val published = publishedOrder[d.fingerprint]
        val nd = d.copy(roadOrderCustom = false, roadOrder = published ?: d.roadOrder)
        if (nd == d) return
        replace(nd)
        if (desktop?.fingerprint == d.fingerprint && foreground && nd.roadOrder != d.roadOrder) { connectJob?.cancel(); connect() }
    }

    /** Rename a session everywhere at once: optimistically here, durably on
     *  the desktop (its override store), and the refresh squares the rest. */
    fun rename(s: Session, title: String) {
        val a = api ?: return
        viewModelScope.launch {
            try {
                a.rename(s.id, title)
                val t = title.trim()
                if (t.isNotEmpty()) {
                    sessions = sessions.map { if (it.id == s.id) it.copy(title = t) else it }
                    if (selected?.id == s.id) selected = selected?.copy(title = t)
                }
                refreshNow()
            } catch (e: Exception) { notice = describe(e) }
        }
    }

    /** A fresh pairing: remember it (re-pairing the same desktop replaces
     *  its entry — the fingerprint is the identity) and show it. */
    private fun adopt(d: Desktop) {
        disconnect()
        desktops = desktops.filter { it.fingerprint != d.fingerprint } + d
        store.saveAll(desktops)
        store.activeFingerprint = d.fingerprint
        desktop = d
        showPair = false
        resetDesktopState()
        connect()
    }

    /** Call a paired desktop what you like. Blank goes back to the name the
     *  desktop gives itself. Kept on this phone only. */
    fun renameDesktop(d: Desktop, friendly: String) {
        val clean = friendly.trim().filterNot { it.isISOControl() }.take(64)
        desktops = desktops.map { if (it.fingerprint == d.fingerprint) it.copy(friendlyName = clean) else it }
        if (desktop?.fingerprint == d.fingerprint) desktop = desktops.find { it.fingerprint == d.fingerprint }
        store.saveAll(desktops)
    }

    /** Show another paired desktop. Everything cached belongs to the old
     *  one, so it all goes; the connect re-reads the new one's truth. */
    fun switchTo(d: Desktop) {
        if (d.fingerprint == desktop?.fingerprint) return
        disconnect()
        store.activeFingerprint = d.fingerprint
        desktop = desktops.find { it.fingerprint == d.fingerprint } ?: d
        resetDesktopState()
        connect()
    }

    /** fingerprint → whether that desktop answered its last status probe.
     *  The shown desktop's truth is `connected`; this covers the rest of the
     *  drawer list, so its dots mean "up right now", not "the one shown". */
    var reachable by mutableStateOf<Map<String, Boolean>>(emptyMap()); private set
    private var reachJob: Job? = null

    /** Probe every desktop not being shown, the drawer's moment. The last
     *  answer stands while a re-probe runs, so a dot never blinks gray on
     *  every open; first address to answer wins, bridge included. */
    fun checkDesktops() {
        if (reachJob?.isActive == true) return
        reachJob = viewModelScope.launch {
            desktops.filter { it.fingerprint != desktop?.fingerprint }.forEach { d ->
                launch {
                    val bridge = if (d.iroh.isNotEmpty())
                        runCatching { IrohBridge.urlFor(getApplication(), d.iroh) }.getOrNull() else null
                    val urls = Roads.candidates(d, bridge).map { it.url }
                    if (urls.isEmpty()) { reachable = reachable + (d.fingerprint to false); return@launch }
                    val answers = kotlinx.coroutines.channels.Channel<Boolean>(urls.size)
                    urls.forEach { url ->
                        viewModelScope.launch {
                            val api = Api(url, d.token, d.fingerprint)
                            val r = runCatching { api.status() }
                            // A status answer is a status answer: the
                            // desktop's roads (and a waiting relay draft)
                            // are taken up even for a desktop not shown.
                            r.getOrNull()?.let { s ->
                                val cur = desktops.find { it.fingerprint == d.fingerprint } ?: d
                                val nd = roadsFrom(cur, s)
                                if (nd != cur) replace(nd)
                                enrollFromStatus(api, d.fingerprint, s)
                            }
                            answers.send(r.isSuccess)
                        }
                    }
                    repeat(urls.size) {
                        if (answers.receive()) { reachable = reachable + (d.fingerprint to true); return@launch }
                    }
                    reachable = reachable + (d.fingerprint to false)
                }
            }
        }
    }

    /** Unpair one desktop. Forgetting the shown one falls back to the next;
     *  forgetting the last returns the app to the pair screen. */
    fun forget(d: Desktop? = null) {
        val gone = d ?: desktop ?: return
        desktops = desktops.filter { it.fingerprint != gone.fingerprint }
        store.saveAll(desktops)
        if (gone.fingerprint == desktop?.fingerprint) {
            disconnect()
            desktop = desktops.firstOrNull()
            store.activeFingerprint = desktop?.fingerprint ?: ""
            resetDesktopState()
            if (desktop != null) connect()
        }
    }

    /** Every cache below the desktop, back to empty — the screens must never
     *  show one desktop's sessions under another's name. */
    private fun resetDesktopState() {
        sessions = emptyList(); running = emptySet(); open = emptySet(); activity = emptyMap()
        usage = emptyList(); query = ""; results = null; searchJob?.cancel()
        files = emptyList(); loadingFiles = false; viewing = null; opening = null; showFiles = false
        browsing = false; browsePath = ""; browseEntries = emptyList()
        composingNew = false; attachments = emptyList()
        sentAt = 0L; awaitingReply = false
        agents = emptyList(); selected = null; loadingTurns = false
        spine.clear(); publishSpine(); noSpine = false
        relays = emptyMap(); previewUrl = null; inlineFiles = emptyMap()
        withFiles = emptySet(); ports = emptyMap(); stars = emptySet(); broughtIn = emptyMap()
        agentFilter = null; filesOnly = false; activeOnly = false
    }

    // ---- connection lifecycle: the activity calls these

    fun onStart() {
        foreground = true
        // Away long enough that whoever holds the phone might not be you.
        if (biometric && desktop != null && pausedAt > 0 && System.currentTimeMillis() - pausedAt > 5 * 60_000) {
            locked = true
        }
        connect()
    }
    fun onStop() { foreground = false; pausedAt = System.currentTimeMillis(); disconnect() }

    /** Which road is "more local": the desktop's own road order, LAN →
     *  VPN → relay → iroh by default. "Last good" is no tiebreak worth
     *  having: after a day out it is the public IP, and from inside the LAN
     *  most routers refuse to hairpin their own port mapping, so the LAN
     *  address must win whenever it answers. */
    private fun rank(d: Desktop, c: Candidate): Int = Roads.rank(d, c.road)

    private fun connect() {
        val d = desktop ?: return
        if (connectJob?.isActive == true) return
        ws?.cancel()
        connectJob = viewModelScope.launch {
            // The desktop may be on a different address than last time — home
            // Wi‑Fi, USB, Tailscale. Probe every known address at once and
            // commit to the most local one that answers. The iroh bridge is
            // the always-answering last resort: anything more direct wins.
            val bridge = if (d.iroh.isNotEmpty())
                runCatching { IrohBridge.urlFor(getApplication(), d.iroh) }.getOrNull() else null
            // The route that won last time goes FIRST, whatever its rank: on
            // client-isolated office Wi‑Fi the bridge answers in ~0.7s while
            // the doomed LAN probe eats its full 4s timeout — and rank-order
            // committing made every office connect pay that wait [observed:
            // probe log 2026-08-31, LAN 4015ms timeout vs bridge 665ms ok].
            // Locality still wins the day: losing probes keep running below,
            // and a more local answer switches the connection live.
            val myGen = ++connectGen
            val byRoad = Roads.candidates(d, bridge)
            val last = byRoad.firstOrNull { it.url == d.baseUrl }
            val cands = listOfNotNull(last) + byRoad.filter { it !== last }
            val urls = cands.map { it.url }
            // Probes live on the outer scope, not this coroutine: a losing
            // probe blocks in OkHttp until its own timeout, and it must not
            // hold up committing to the address that already answered.
            val probes = urls.map { url ->
                viewModelScope.async {
                    val t0 = System.currentTimeMillis()
                    val r = runCatching { Api(url, d.token, d.fingerprint).status() }
                    android.util.Log.i("Aiterm", "probe $url → ${r.exceptionOrNull()?.let { it.javaClass.simpleName + ": " + it.message } ?: "ok"} in ${System.currentTimeMillis() - t0}ms")
                    r.getOrNull()
                }
            }
            var chosen: Pair<Candidate, Status>? = null
            for ((i, p) in probes.withIndex()) { val s = p.await(); if (s != null) { chosen = cands[i] to s; break } }
            // Probes past the winner stay alive — see the better-route watch
            // at the bottom of this function.
            if (chosen == null) { android.util.Log.i("Aiterm", "no address reachable; retry in 3s"); connected = false; scheduleRetry(); return@launch }
            val (won, status) = chosen
            val reachable = won.url
            android.util.Log.i("Aiterm", "using $reachable (${won.road.id})")
            // The desktop reports every address it answers on right now;
            // adopt that list so a DHCP move or new public IP never strands
            // us with only the addresses the QR knew at pairing time.
            // The bridge and the relay answer on ports of their own, not the
            // desktop's; fresh addresses keep the desktop's real port instead,
            // and only a direct winner joins the address list — the other
            // roads are rebuilt from the node id and the relay route.
            val direct = won.road == Road.LAN || won.road == Road.VPN
            val port = if (direct) reachable.substringAfterLast(':')
            else Roads.directUrls(d).firstOrNull()?.substringAfterLast(':') ?: reachable.substringAfterLast(':')
            val fresh = status.hosts.map { "https://${if (it.contains(':')) "[$it]" else it}:$port" }
            val candidates = (fresh.ifEmpty { d.candidates } + listOfNotNull(reachable.takeIf { direct })).distinct()
            val nd = roadsFrom(d, status).copy(baseUrl = reachable, candidates = candidates)
            if (nd != d) {
                desktops = desktops.map { if (it.fingerprint == nd.fingerprint) nd else it }
                store.saveAll(desktops)
                desktop = nd
            }
            // A relay draft waiting over there is signed now, over this
            // pairing — the route goes live without a new QR.
            enrollFromStatus(Api(reachable, d.token, d.fingerprint), d.fingerprint, status)
            if (!foreground) return@launch // backgrounded while probing
            openEvents(Api(reachable, d.token, d.fingerprint))
            // Better-route watch: the remembered winner got us on fast, but a
            // strictly more local route that answers late (LAN at home, after
            // the office bridge won the sprint) takes over — connection and
            // remembered winner both. One switch at most; a newer connect()
            // makes this one stand down.
            for ((i, p) in probes.withIndex()) {
                val s = runCatching { p.await() }.getOrNull() ?: continue
                val url = urls[i]
                if (url == reachable || rank(d, cands[i]) >= rank(d, won)) continue
                if (myGen != connectGen || !foreground) return@launch
                android.util.Log.i("Aiterm", "more local $url answered after commit; switching from $reachable")
                val cur = desktops.find { it.fingerprint == d.fingerprint } ?: return@launch
                val nd2 = cur.copy(baseUrl = url)
                desktops = desktops.map { if (it.fingerprint == nd2.fingerprint) nd2 else it }
                store.saveAll(desktops)
                if (desktop?.fingerprint == nd2.fingerprint) desktop = nd2
                ws?.cancel()
                openEvents(Api(url, d.token, d.fingerprint))
                return@launch
            }
        }
    }

    private fun scheduleRetry() {
        viewModelScope.launch { delay(3000); if (foreground && desktop != null) connect() }
    }

    private fun openEvents(a: Api) {
        refreshNow()
        loadUsage()
        // The roster of who can be brought in changes on the desktop
        // (models starred, providers added); re-read it on every connect
        // rather than trusting the first answer forever.
        viewModelScope.launch { runCatching { agents = a.agents() } }
        ws = a.events(
            onOpen = {
                viewModelScope.launch {
                    connected = true
                    // A dropped WebSocket loses every event it was carrying;
                    // the first thing a new one owes the screen is the gap.
                    selected?.let { fetchSpine(it.id) }
                }
            },
            onEvent = { type, obj ->
                viewModelScope.launch {
                    when (type) {
                        "sessions_changed", "session_exit" -> refresh()
                        "status_changed" -> {
                            // A road was switched, a relay draft prepared or
                            // a route enrolled, or the road order edited
                            // over there; re-read what this desktop offers
                            // so the next dial (and the Connection order
                            // notes) see it without a reconnect — and sign
                            // a waiting draft while we are here.
                            val cur = desktop ?: return@launch
                            val status = runCatching { a.status() }.getOrNull() ?: return@launch
                            val nd = roadsFrom(cur, status)
                            if (nd != cur) replace(nd)
                            enrollFromStatus(a, cur.fingerprint, status)
                            // The desktop's order changed and this phone
                            // follows it: the next sprint dials by it.
                            if (nd.roadOrder != cur.roadOrder && foreground) { connectJob?.cancel(); connect() }
                        }
                        "relay" -> {
                            val sid = obj["session_id"]?.jsonPrimitive?.content ?: return@launch
                            relays = relays + (sid to RelayInfo(
                                bName = obj["b_name"]?.jsonPrimitive?.content ?: "second agent",
                                bSessionId = obj["b_session_id"]?.jsonPrimitive?.content,
                                phase = obj["phase"]?.jsonPrimitive?.content ?: "",
                                round = obj["round"]?.jsonPrimitive?.content?.toIntOrNull() ?: 1,
                                rounds = obj["rounds"]?.jsonPrimitive?.content?.toIntOrNull() ?: 1,
                                note = obj["note"]?.jsonPrimitive?.content ?: "",
                            ))
                            refresh()
                        }
                        "spine" -> {
                            // Every session with a running tail is on this
                            // stream; the phone only draws the one it is
                            // looking at.
                            val sid = obj["session_id"]?.jsonPrimitive?.content ?: return@launch
                            if (sid != selected?.id) return@launch
                            val ev = SpineEvent.parse(obj) ?: return@launch
                            lastSpineAt = System.currentTimeMillis()
                            when (spine.offer(ev)) {
                                Offer.Applied -> { publishSpine(); afterApplied(ev) }
                                // A seq was missed, or the desktop restarted:
                                // ask for what we are short of rather than
                                // drawing a transcript with a hole in it.
                                Offer.Gap -> fetchSpine(sid)
                                Offer.EpochChanged -> { spine.clear(); publishSpine(); fetchSpine(sid, from = 0) }
                                Offer.Stale -> {}
                            }
                        }
                        "file_changed" -> {
                            // The conversation shows produced files inline,
                            // so keep them fresh whether or not the Files
                            // view is up.
                            val id = obj["session_id"]?.jsonPrimitive?.content
                            if (id != null && id == selected?.id) loadFiles()
                        }
                        "activity" -> {
                            val id = obj["session_id"]?.jsonPrimitive?.content ?: return@launch
                            val a = obj["activity"]?.jsonPrimitive?.content ?: return@launch
                            activity = activity + (id to a)
                            if (a != "idle") awaitingReply = false
                        }
                        "renamed" -> {
                            // The desktop's name was edited over there; wear
                            // it everywhere at once.
                            val n = obj["name"]?.jsonPrimitive?.content?.takeIf { it.isNotBlank() } ?: return@launch
                            val cur = desktop ?: return@launch
                            if (cur.name != n) {
                                val nd = cur.copy(name = n)
                                desktops = desktops.map { if (it.fingerprint == nd.fingerprint) nd else it }
                                store.saveAll(desktops)
                                desktop = nd
                            }
                        }
                        "attention" -> {
                            val t = obj["title"]?.jsonPrimitive?.content
                            val b = obj["body"]?.jsonPrimitive?.content
                            notice = listOfNotNull(t, b).joinToString(" — ")
                            refresh()
                        }
                    }
                }
            },
            onClosed = {
                viewModelScope.launch {
                    connected = false
                    if (ws != null) scheduleRetry()
                }
            },
        )
    }

    private fun disconnect() {
        connectJob?.cancel()
        ws?.cancel(); ws = null
        connected = false
    }

    // ---- reading

    /** Debounced: a burst of events becomes one re-read. */
    fun refresh() {
        refreshJob?.cancel()
        refreshJob = viewModelScope.launch { delay(400); load() }
    }

    fun refreshNow() {
        refreshJob?.cancel()
        refreshJob = viewModelScope.launch { load() }
    }

    private suspend fun load() {
        val a = api ?: return
        try {
            val r = a.sessions()
            sessions = r.sessions.sortedByDescending { it.last_active }
            // The session this phone just started: open it the moment
            // discovery shows it, rather than leaving a new top row to be
            // scrolled for. Same seconds-or-millis fold as relativeTime.
            pendingOpen?.let { po ->
                if (System.currentTimeMillis() - po.at > 30_000) {
                    pendingOpen = null
                    if (starting != null) { starting = null; notice = "The desktop did not report the session — check its list" }
                    return@let
                }
                val born = sessions.find { s ->
                    val ms = if (s.last_active > 100_000_000_000L) s.last_active else s.last_active * 1000
                    s.project_path.trimEnd('/') == po.cwd.trimEnd('/') &&
                        ms >= po.at - 5_000 &&
                        (po.agentId.startsWith("api:") || s.agent == po.agentId)
                }
                if (born != null && selected == null && !composingNew) {
                    pendingOpen = null
                    starting = null
                    select(born)
                }
            }
            running = r.running.toSet()
            open = r.open.toSet()
            activity = r.activity
            withFiles = r.with_files.toSet()
            ports = r.ports
            stars = r.stars.toSet()
            broughtIn = r.brought_in
            selected?.let { cur -> sessions.find { it.id == cur.id }?.let { selected = it } }
            if (agents.isEmpty()) agents = runCatching { a.agents() }.getOrDefault(emptyList())
            // The transcript rides the spine now; a list refresh only
            // re-reads what the list itself shows.
            selected?.let {
                if (showFiles) files = runCatching { a.files(it.id) }.getOrDefault(files)
            }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e // a newer refresh superseded this one; not an error
        } catch (e: Exception) {
            android.util.Log.w("Aiterm", "load failed: ${e.javaClass.simpleName}: ${e.message}")
            notice = describe(e)
            // A request that cannot reach the desktop while we think we are
            // connected means the saved address went stale under us (the
            // WebSocket can outlive its network) — re-probe rather than keep
            // timing out against it.
            if (e is IOException) { connected = false; connect() }
        }
    }

    /** Slow on the desktop's side (it asks each service), so only on
     *  connect and on request. */
    fun loadUsage(retry: Boolean = true) {
        val a = api ?: return
        viewModelScope.launch {
            runCatching { a.usage() }.onSuccess { fresh ->
                // A source that is rate-limited this minute still had a number a
                // minute ago; keep showing it rather than blinking the chip away.
                val last = usage.associateBy { it.id }
                usage = fresh.map { u -> if (u.state == "ok") u else last[u.id]?.takeIf { it.state == "ok" } ?: u }
                // A source that failed this round (slow upstream, the desktop
                // just restarted with a cold cache) usually answers the next
                // ask; one quiet retry keeps the strip complete.
                if (retry && fresh.any { it.state != "ok" }) {
                    delay(5000)
                    loadUsage(retry = false)
                }
            }
        }
    }

    fun stateOf(s: Session): SessionState {
        val a = activity[s.id]
        val pendingHere = selected?.id == s.id && awaitingReply && System.currentTimeMillis() - sentAt < 90_000
        return when {
            a == "working" || pendingHere -> SessionState.Working
            a == "attention" -> SessionState.NeedsYou
            s.id in open -> SessionState.OnDesktop
            s.id in running -> SessionState.Running
            else -> SessionState.Idle
        }
    }

    /** Typing asks the desktop's full-text index — the same search the
     *  sidebar runs — after a short pause, so a word finds the sessions that
     *  talked about it, not only the ones titled with it. */
    fun search(q: String) {
        query = q
        searchJob?.cancel()
        val trimmed = q.trim()
        if (trimmed.isEmpty()) { results = null; return }
        searchJob = viewModelScope.launch {
            delay(350)
            results = runCatching { api?.search(trimmed) }.getOrNull()
        }
    }

    /** The list as shown: the index's answer while searching, else everything. */
    /** Home-screen filters: one engine, only sessions with files, only ones
     *  alive right now. Cheap to apply, cheap to clear. */
    var agentFilter by mutableStateOf<String?>(null)
    var filesOnly by mutableStateOf(false)
    var activeOnly by mutableStateOf(false)
    var withFiles by mutableStateOf<Set<String>>(emptySet()); private set
    /** session id → dev-server ports on the desktop, for live previews. */
    var ports by mutableStateOf<Map<String, List<Int>>>(emptyMap()); private set
    /** Starred sessions — stay on top, synced through the desktop. */
    var stars by mutableStateOf<Set<String>>(emptySet()); private set
    /** Brought-in session → its master, for grouping the workspace crew. */
    var broughtIn by mutableStateOf<Map<String, String>>(emptyMap()); private set
    /** Masters whose crew is folded away in the list — remembered. */
    var foldedCrews by mutableStateOf(store.foldedCrews); private set
    fun toggleCrew(masterId: String) {
        foldedCrews = if (masterId in foldedCrews) foldedCrews - masterId else foldedCrews + masterId
        store.foldedCrews = foldedCrews
    }
    /** Live second-agent relays, keyed by the first agent's session. */
    var relays by mutableStateOf<Map<String, RelayInfo>>(emptyMap()); private set
    fun dismissRelay(sessionId: String) { relays = relays - sessionId }
    /** A page being previewed full screen: absolute https URL on the desktop. */
    var previewUrl by mutableStateOf<String?>(null)

    val visibleSessions: List<Session>
        get() {
            val q = query.trim().lowercase()
            val base = if (q.isEmpty()) sessions else results?.let { r ->
                // The index knows content; the title filter catches a session
                // typed a second ago that it has not seen yet.
                val ids = r.map { it.id }.toSet()
                r + sessions.filter { it.id !in ids && it.title.lowercase().contains(q) }
            } ?: sessions.filter { it.title.lowercase().contains(q) || it.group_path.lowercase().contains(q) }
            return base
                .filter { s ->
                    (agentFilter == null || s.agent == agentFilter) &&
                        (!filesOnly || s.id in withFiles) &&
                        (!activeOnly || s.id in running || s.id in open)
                }
                // Stars stay on top; open-on-desktop next; then recency.
                .sortedWith(
                    compareByDescending<Session> { it.id in stars }
                        .thenByDescending { it.id in open }
                        .thenByDescending { it.last_active },
                )
                // A brought-in agent belongs under its master, not loose in
                // the list — glue satellites directly beneath, in order.
                .let { sorted ->
                    val out = ArrayList<Session>(sorted.size)
                    val placed = HashSet<String>()
                    for (s in sorted) {
                        if (s.id in placed) continue
                        if (broughtIn[s.id] != null && sorted.any { it.id == broughtIn[s.id] }) continue
                        out.add(s); placed.add(s.id)
                        for (k in sorted) {
                            if (broughtIn[k.id] == s.id && k.id !in placed) {
                                placed.add(k.id)
                                if (s.id !in foldedCrews) out.add(k)
                            }
                        }
                    }
                    out
                }
        }

    fun loadFiles() {
        val s = selected ?: return
        val a = api ?: return
        viewModelScope.launch {
            loadingFiles = true
            try { files = a.files(s.id) } catch (e: Exception) { notice = describe(e) }
            loadingFiles = false
        }
    }

    fun open(entry: FileEntry) {
        // An HTML file is a page — render it (with its folder's assets)
        // rather than showing source.
        if (entry.ext == "html" || entry.ext == "htm") { previewFile(entry.path); return }
        val a = api ?: return
        viewModelScope.launch {
            opening = entry.path
            try { viewing = entry to a.download(entry, getApplication<Application>().cacheDir) }
            catch (e: Exception) { notice = describe(e) }
            finally { opening = null }
        }
    }

    /** Bring a second agent into this session. The desktop runs the relay;
     *  their exchange lands in this conversation as it happens. Opens the
     *  session in a desktop tab first when it isn't already. */
    fun bringIn(s: Session, agentId: String, model: String?, focus: String, rounds: Int, auto: Boolean) {
        val a = api ?: return
        viewModelScope.launch {
            try {
                if (s.id !in open) {
                    notice = "Opening the session on the desktop first…"
                    a.open(s.id)
                    delay(3000)
                }
                Diag.log("bring-in", "${s.id.take(8)} <- $agentId model=$model rounds=$rounds auto=$auto")
                a.bringIn(s.id, agentId, model, focus, rounds, auto)
                relays = relays + (s.id to RelayInfo(agentId.removePrefix("api:"), null, "opening", 1, rounds, ""))
                notice = "They're in — the exchange shows up right here"
            } catch (e: Exception) { notice = describe(e) }
        }
    }

    fun toggleStar(s: Session) {
        val a = api ?: return
        val on = s.id !in stars
        stars = if (on) stars + s.id else stars - s.id
        viewModelScope.launch { runCatching { a.star(s.id, on) }.onFailure { notice = describe(Exception(it)) } }
    }

    /** See a dev server the session started, through the desktop. */
    fun previewPort(port: Int) {
        val a = api ?: return
        val base = desktop?.baseUrl ?: return
        viewModelScope.launch {
            runCatching { a.makePreview(port = port) }
                .onSuccess { previewUrl = base + it }
                .onFailure { notice = "Preview failed: ${it.message}" }
        }
    }

    /** Render an agent-built page (a folder with an index.html) live. */
    fun previewFile(path: String) {
        val a = api ?: return
        val base = desktop?.baseUrl ?: return
        val dir = path.substringBeforeLast('/')
        val name = path.substringAfterLast('/')
        viewModelScope.launch {
            runCatching { a.makePreview(dir = dir) }
                .onSuccess { previewUrl = base + it + name }
                .onFailure { notice = "Preview failed: ${it.message}" }
        }
    }

    /** A file the transcript mentions by path. The ledger may know it — then
     *  real metadata rides along — otherwise a minimal entry is enough: the
     *  desktop serves any file an agent could have produced. */
    fun openMentioned(path: String) {
        open(files.find { it.path == path } ?: FileEntry(path, path.substringAfterLast('/'), 0, 0, "mentioned"))
    }

    /** Local copies of files previewed inline in the conversation, by path.
     *  Fetched once and kept for the session; the full viewer re-downloads
     *  through its own cache. */
    var inlineFiles by mutableStateOf<Map<String, java.io.File>>(emptyMap()); private set
    fun fetchInline(entry: FileEntry) {
        if (inlineFiles.containsKey(entry.path)) return
        val a = api ?: return
        viewModelScope.launch {
            runCatching { a.download(entry, getApplication<Application>().cacheDir) }
                .onSuccess { inlineFiles = inlineFiles + (entry.path to it) }
        }
    }

    /** Guards the per-selection spine work: bumped on every select, so a
     *  stale watcher stands down instead of writing over a newer one. */
    private var selectGen = 0

    fun select(s: Session?) {
        Diag.log("select", if (s == null) "none" else "${s.id.take(8)} ${s.agent} open=${s.id in open} running=${s.id in running}")
        selected = s
        selectGen++
        spine.clear(); publishSpine(); noSpine = false; refetchWanted = false; lastSpineAt = 0L
        files = emptyList()
        showFiles = false
        browsing = false
        browsePath = ""
        browseEntries = emptyList()
        viewing = null
        if (s == null) return
        val myGen = selectGen
        viewModelScope.launch {
            loadingTurns = true
            // The opening fetch used to be one shot: one relay hiccup and the
            // screen sat on "Nothing here yet" until some event happened to
            // reload it — a person opening a mid-turn session saw its whole
            // HISTORY as blank [observed 2026-08-31]. Retry, briefly.
            for (attempt in 1..3) {
                try {
                    val r = api?.spine(s.id, 0) ?: break
                    if (myGen != selectGen) return@launch
                    spine.replay(r); publishSpine()
                    Diag.log("spine", "${s.id.take(8)} replay ${r.events.size} events live=${r.live} -> ${spine.items.size} rows, phase=${spine.phase} (try $attempt)")
                    lastSpineAt = System.currentTimeMillis()
                    break
                } catch (e: ApiError) {
                    // A desktop that predates the spine has no such route and
                    // its 404 carries no message; one that HAS the route says
                    // "no such session" and means it. Only the first is a
                    // reason to fall back to the old whole-transcript poll.
                    Diag.log("spine", "${s.id.take(8)} replay failed (try $attempt): ${e.code} ${e.message}")
                    if (e.code == 404 && e.message?.contains("session") != true) { noSpine = true; break }
                    if (attempt == 3) notice = describe(e) else delay(1200)
                } catch (e: Exception) {
                    Diag.log("spine", "${s.id.take(8)} replay failed (try $attempt): ${e.javaClass.simpleName} ${e.message}")
                    if (attempt == 3) notice = describe(e) else delay(1200)
                }
            }
            loadingTurns = false
            if (noSpine) { legacyPoll(s, myGen); return@launch }
            // The spine arrives over the WebSocket; this is only the safety
            // net. A WebSocket can die without saying so (the phone changes
            // network, the desktop's relay hiccups) and the screen would sit
            // frozen mid-turn, so a working session that has gone quiet for
            // 20 s gets asked directly.
            while (myGen == selectGen && selected?.id == s.id) {
                delay(5000)
                if (myGen != selectGen || selected?.id != s.id) break
                val quiet = System.currentTimeMillis() - lastSpineAt
                if (quiet > 20_000 && spine.phase == SpinePhase.Working) fetchSpine(s.id)
            }
        }
        loadFiles() // the conversation shows what the session made, inline
    }

    /** Republish the store for Compose. One assignment per apply: the rows
     *  are equal data classes, so the list changing identity costs the one
     *  row that actually changed. */
    private fun publishSpine() {
        items = spine.items
        phase = spine.phase
        phaseDetail = spine.phaseDetail
    }

    /** Ask for everything after `from` (default: what we hold) and merge.
     *  One at a time — a burst of gaps is one question. */
    private fun fetchSpine(id: String, from: Long? = null) {
        if (noSpine) return
        // An event that gaps while a fetch is already in the air would be
        // dropped and never asked for — the answer was computed before it
        // existed. Remember that, and go round once more.
        if (fetchingSpine) { refetchWanted = true; return }
        val a = api ?: return
        viewModelScope.launch {
            fetchingSpine = true
            try {
                var r = a.spine(id, from ?: spine.lastSeq)
                if (selected?.id != id) return@launch
                var advanced = spine.replay(r); publishSpine()
                lastSpineAt = System.currentTimeMillis()
                // A long session comes in pages. Keep asking from what we
                // now hold while the desktop says there is more — but only
                // while a page moves the cursor, so an empty or repeated
                // page cannot spin us.
                var pages = 0
                while (r.hasMore && advanced && pages++ < MAX_CATCH_UP_PAGES) {
                    r = a.spine(id, spine.lastSeq)
                    if (selected?.id != id) return@launch
                    advanced = spine.replay(r); publishSpine()
                    lastSpineAt = System.currentTimeMillis()
                }
            } catch (e: Exception) {
                android.util.Log.w("Aiterm", "spine fetch failed: ${e.message}")
            } finally {
                fetchingSpine = false
                if (refetchWanted && selected?.id == id) { refetchWanted = false; fetchSpine(id) }
            }
        }
    }

    /** What an applied event means beyond the transcript. A file write is
     *  the only thing the ledger will not tell us in time: a scratchpad
     *  write gets no file_changed event (the watcher covers workspaces, not
     *  /tmp), so the globe for a built page never appeared until reopen
     *  [observed 2026-08-31: car-listing.html, via "wrote", invisible]. */
    private fun afterApplied(e: SpineEvent) {
        val k = e.kind
        if (awaitingReply && (k is SpineKind.AgentText || k is SpineKind.ToolCall)) awaitingReply = false
        val wroteSomething = when (k) {
            is SpineKind.ToolCallUpdate -> k.status == ToolStatus.Completed && spine.tool(k.id)?.category == ToolCategory.Edit
            is SpineKind.ToolCall -> k.status == ToolStatus.Completed && k.category == ToolCategory.Edit
            else -> false
        }
        if (wroteSomething || k is SpineKind.TurnEnded) loadFiles()
    }

    /** The pre-spine path, for a desktop that has not been updated: the
     *  whole transcript every 3 s, mapped onto the same rows by ordinal. */
    private suspend fun legacyPoll(s: Session, myGen: Int) {
        var last: List<Turn> = emptyList()
        while (myGen == selectGen && selected?.id == s.id) {
            val fresh = runCatching { api?.conversation(s.id) }.getOrNull()
            if (fresh != null && fresh != last) {
                last = fresh
                spine.legacy(fresh); publishSpine()
                loadFiles()
            }
            delay(3000)
        }
    }

    // ---- acting

    /** Brought-in agents of this session currently waiting on a person —
     *  the master's screen shows them, because their own dialog is invisible
     *  here and "working" was what a parked approval used to read as. */
    fun crewNeedsYou(master: Session): List<Session> =
        broughtIn.filterValues { it == master.id }.keys
            .filter { activity[it] == "attention" }
            .mapNotNull { id -> sessions.find { it.id == id } }

    /** Raw keystrokes for the terminal's own dialogs. No Enter is appended —
     *  Enter is a key here ("\r"). */
    fun sendKeys(s: Session, keys: String) {
        val a = api ?: return
        viewModelScope.launch {
            try { a.inputKeys(s.id, keys) } catch (e: Exception) { notice = describe(e) }
        }
    }

    fun send(text: String) {
        val s = selected ?: return
        val a = api ?: return
        viewModelScope.launch {
            sending = true
            Diag.log("send", "${s.id.take(8)} open=${s.id in open} ${text.length} chars, ${attachments.size} attachments")
            try {
                if (s.id !in open) {
                    a.open(s.id)
                    if (!waitUntilOpen(a, s.id)) { notice = "The desktop did not open the session"; return@launch }
                }
                val full = withAttachments(text)
                a.input(s.id, full)
                attachments = emptyList()
                // The bubble appears on tap; the desktop's own user_message
                // retires the echo when it comes round.
                spine.echoUser(full, System.currentTimeMillis())
                publishSpine()
                sentAt = System.currentTimeMillis(); awaitingReply = true
            } catch (e: Exception) {
                notice = describe(e)
            } finally { sending = false }
        }
    }

    /** Opening is the desktop's job and takes as long as its tab takes to
     *  spawn. Poll the list rather than guess. */
    private suspend fun waitUntilOpen(a: Api, id: String): Boolean {
        repeat(30) {
            delay(500)
            val r = runCatching { a.sessions() }.getOrNull() ?: return@repeat
            open = r.open.toSet(); running = r.running.toSet()
            if (id in open) return true
        }
        return false
    }

    fun openOnDesktop(s: Session) = act { Diag.log("open", "${s.id.take(8)} asked the desktop"); it.open(s.id); notice = "Opening on ${desktop?.name}" }

    /** Read a picked file and hand it to the desktop. The path comes back and
     *  rides in the message as text — the agent reads it from there. */
    fun attach(uri: android.net.Uri) {
        val a = api ?: return
        val app = getApplication<Application>()
        viewModelScope.launch {
            uploading = true
            try {
                val (name, bytes) = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                    val name = app.contentResolver.query(uri, null, null, null, null)?.use { c ->
                        val i = c.getColumnIndex(android.provider.OpenableColumns.DISPLAY_NAME)
                        if (c.moveToFirst() && i >= 0) c.getString(i) else null
                    } ?: (uri.lastPathSegment ?: "file")
                    name to (app.contentResolver.openInputStream(uri)?.use { it.readBytes() } ?: ByteArray(0))
                }
                if (bytes.isEmpty()) { notice = "Could not read that file"; return@launch }
                if (bytes.size > 25 * 1024 * 1024) { notice = "25 MB at most"; return@launch }
                attachments = attachments + a.upload(name, bytes)
            } catch (e: Exception) { notice = describe(e) } finally { uploading = false }
        }
    }

    /** Bytes the app already holds — a screenshot of itself — sent the same
     *  way a picked file is. */
    fun attachBytes(name: String, bytes: ByteArray) {
        val a = api ?: return
        viewModelScope.launch {
            uploading = true
            try {
                if (bytes.isEmpty()) { notice = "Nothing to attach"; return@launch }
                if (bytes.size > 25 * 1024 * 1024) { notice = "25 MB at most"; return@launch }
                attachments = attachments + a.upload(name, bytes)
            } catch (e: Exception) { notice = describe(e) } finally { uploading = false }
        }
    }
    fun removeAttachment(att: Attachment) { attachments = attachments - att }

    /** What actually goes to the agent: the text, then the files by path. */
    private fun withAttachments(text: String): String {
        if (attachments.isEmpty()) return text
        val files = attachments.joinToString("\n") { "- ${it.path}" }
        val lead = if (text.isBlank()) "Please look at the attached file(s):" else text.trimEnd() + "\n\nAttached file(s):"
        return "$lead\n$files"
    }
    /** Escape: ends the agent's turn, keeps the session. */
    fun interrupt(s: Session) = act { it.interrupt(s.id); awaitingReply = false }
    fun stop(s: Session) = act { it.stop(s.id); refresh() }
    /** A session this phone just asked for: the next refresh that shows a
     *  session born in that folder (for that agent, when the id names one —
     *  an api:<provider> launch surfaces under whichever engine ran it)
     *  opens it directly, instead of leaving the person to spot a new row
     *  at the top of the list seconds later. */
    private data class PendingOpen(val agentId: String, val cwd: String, val at: Long)
    private var pendingOpen: PendingOpen? = null

    /** What the screen shows between "start" tapped and the session existing
     *  on disk — the engine needs a few seconds to be discoverable, and dead
     *  air reads as broken. The ask is echoed like a sent message; the real
     *  session replaces this the moment discovery finds it. */
    data class Starting(val agentId: String, val agentName: String, val cwd: String, val prompt: String?, val at: Long)
    var starting by mutableStateOf<Starting?>(null); private set
    /** Back from the starting screen. The desktop still starts the session —
     *  only the wait is dismissed; the row lands in the list as ever. */
    fun cancelStarting() { starting = null; pendingOpen = null }

    fun newSession(agentId: String, cwd: String, prompt: String?, model: String?, effort: String?, title: String?) = act {
        val text = withAttachments(prompt ?: "").takeIf { p -> p.isNotBlank() }
        it.newSession(agentId, cwd, text, model, effort, title?.takeIf { t -> t.isNotBlank() })
        attachments = emptyList()
        composingNew = false
        pendingOpen = PendingOpen(agentId, cwd, System.currentTimeMillis())
        starting = Starting(
            agentId,
            agents.find { a -> a.id == agentId }?.display_name ?: agentId.removePrefix("api:"),
            cwd,
            text,
            System.currentTimeMillis(),
        )
        refresh()
        // Poll while the starting screen is up: the swap to the real session
        // must not hinge on catching one sessions_changed event. Cheap now —
        // a warm sessions poll is ~70ms on the desktop's side.
        viewModelScope.launch {
            while (starting != null) {
                delay(1500)
                if (starting != null) refreshNow()
            }
        }
    }

    private fun act(block: suspend (Api) -> Unit) {
        val a = api ?: return
        viewModelScope.launch { try { block(a) } catch (e: Exception) { notice = describe(e) } }
    }

    private fun describe(e: Exception): String = when (e) {
        is ApiError -> e.message ?: "HTTP ${e.code}"
        is IOException -> "Can't reach ${desktop?.name ?: "the desktop"}"
        else -> e.message ?: e.toString()
    }
}
