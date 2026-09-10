package com.fivelime.aiterm

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json

/** One desktop this phone is paired with. The fingerprint doubles as its
 *  identity: addresses move, names repeat, the certificate does neither. */
@Serializable
data class Desktop(
    val baseUrl: String, val token: String, val name: String,
    val candidates: List<String> = listOf(baseUrl),
    /** SHA-256 of the desktop's certificate, hex. The only thing we trust. */
    val fingerprint: String = "",
    /** iroh node id, for reaching this desktop when no address works. */
    val iroh: String = "",
    /** AITerm Relay route for this desktop's phone listener; "" / 0 = none
     *  enrolled. Refreshed from every status answer. */
    val relayHost: String = "",
    val relayPort: Int = 0,
    /** The roads to try, most preferred first — see Roads.kt. A desktop
     *  stored before roads existed gets the default and never notices. */
    val roadOrder: List<String> = DEFAULT_ROAD_ORDER,
    /** The person set `roadOrder` here, on the phone. Until they do, the
     *  order follows what the desktop publishes in every status answer;
     *  "Use desktop's order" in Settings clears this. Entries stored
     *  before the flag existed follow the desktop. */
    val roadOrderCustom: Boolean = false,
    /** What the person calls this desktop, on this phone. `name` is what
     *  the desktop calls itself and follows it; this one is theirs. */
    val friendlyName: String = "",
) {
    /** The name to show: the person's, else the desktop's own. */
    val label: String get() = friendlyName.ifBlank { name }
    /** The address that answered last, then the rest in the QR's order. */
    val ordered: List<String> get() = listOf(baseUrl) + candidates.filter { it != baseUrl }
    /** The relay dial, or null when no route is enrolled. */
    val relayUrl: String? get() = if (relayHost.isNotEmpty() && relayPort in 1..65535) "https://${bracket(relayHost)}:$relayPort" else null

    private fun bracket(host: String) = if (host.contains(':') && !host.startsWith("[")) "[$host]" else host
}

/** Private app storage. The tokens are the only secrets; they never leave
 *  here except as request headers. */
class Store(context: Context) {
    private val prefs = context.getSharedPreferences("aiterm", Context.MODE_PRIVATE)
    /** Preferences live apart from pairing, so forgetting a desktop keeps
     *  the person's theme and time zone. */
    private val settings = context.getSharedPreferences("aiterm.settings", Context.MODE_PRIVATE)
    private val json = Json { ignoreUnknownKeys = true }

    var theme: String
        get() = settings.getString("theme", "dark") ?: "dark"
        set(v) { settings.edit().putString("theme", v).apply() }

    /** IANA zone id; empty = the phone's own zone. */
    var timeZone: String
        get() = settings.getString("tz", "") ?: ""
        set(v) { settings.edit().putString("tz", v).apply() }

    /** Masters whose brought-in crew is folded away in the list. */
    var foldedCrews: Set<String>
        get() = settings.getStringSet("folded_crews", emptySet()) ?: emptySet()
        set(v) { settings.edit().putStringSet("folded_crews", v).apply() }

    /** Require fingerprint (or device credential) to open the app. */
    var biometric: Boolean
        get() = settings.getBoolean("biometric", false)
        set(v) { settings.edit().putBoolean("biometric", v).apply() }

    /** Fingerprint of the desktop the app is showing. */
    var activeFingerprint: String
        get() = prefs.getString("active", "") ?: ""
        set(v) { prefs.edit().putString("active", v).apply() }

    /** Every paired desktop. Reading also migrates the storage this app
     *  used when it knew only one desktop, so an update never asks anyone
     *  to pair again. */
    fun loadAll(): List<Desktop> {
        prefs.getString("desktops", null)?.let { raw ->
            return runCatching { json.decodeFromString(ListSerializer(Desktop.serializer()), raw) }
                .getOrDefault(emptyList())
        }
        val legacy = loadLegacy() ?: return emptyList()
        saveAll(listOf(legacy))
        activeFingerprint = legacy.fingerprint
        prefs.edit().remove("url").remove("token").remove("name").remove("urls").remove("fp").apply()
        return listOf(legacy)
    }

    private fun loadLegacy(): Desktop? {
        val url = prefs.getString("url", null) ?: return null
        val token = prefs.getString("token", null) ?: return null
        val candidates = prefs.getString("urls", null)?.split('\n')?.filter { it.isNotBlank() } ?: listOf(url)
        val fp = prefs.getString("fp", null) ?: return null // paired before pinning existed: pair again
        return Desktop(url, token, prefs.getString("name", null) ?: "Desktop", candidates, fp)
    }

    fun saveAll(list: List<Desktop>) {
        prefs.edit().putString("desktops", json.encodeToString(ListSerializer(Desktop.serializer()), list)).apply()
    }
}
