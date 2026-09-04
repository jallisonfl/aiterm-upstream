package com.fivelime.aiterm

/** The QR: `aiterm://pair?v=1&p=<port>&t=<token>&n=<name>&h=<addr>&h=<addr>…`.
 *  Hosts repeat, best first. Anything we don't understand is a refusal, not
 *  a guess — the payload decides what we trust.
 *
 *  A combined QR (one code pairing either phone app) carries the gateway's
 *  fields under `p`/`f`/`s` and ours under `tp`/`tt`/`tf` — when `tt` is
 *  present it is our payload and `p`/`f` belong to the other app. Our own
 *  host list rides as repeated `th`; when it is there `h` is the gateway's
 *  and we leave it alone.
 *
 *  Roads (docs/remote/remote-roads.md): `tr`/`tq` name a live or drafted relay
 *  route; `ta` is the enrollment digest the phone signs to make a draft
 *  live. Each is optional and each is dropped, not guessed at, when
 *  malformed. */
data class PairLink(
    val hosts: List<String>, val port: Int, val token: String, val name: String, val fingerprint: String,
    /** iroh node id — the reach-from-anywhere address; "" when the desktop predates it. */
    val iroh: String = "",
    /** AITerm Relay public host and port; "" / 0 when the desktop offers none. */
    val relayHost: String = "",
    val relayPort: Int = 0,
    /** The 32-byte enrollment digest awaiting this phone's signature, or
     *  null when there is nothing to enroll (route already live, or no relay). */
    val relayAuthorization: ByteArray? = null,
) {
    val candidates: List<String> get() = hosts.map { "https://$it:$port" }

    companion object {
        fun parse(raw: String): PairLink? {
            val q = Query.parse(raw.trim()) ?: return null
            if (q.first("v") != "1") return null
            val own = q.all("th").filter { it.isNotBlank() }
            val hosts = own.ifEmpty { q.all("h").filter { it.isNotBlank() } }
            val combined = q.first("tt")?.isNotBlank() == true
            val port = q.first(if (combined) "tp" else "p")?.toIntOrNull() ?: return null
            val token = q.first(if (combined) "tt" else "t")?.takeIf { it.isNotBlank() } ?: return null
            val name = q.first("n")?.takeIf { it.isNotBlank() } ?: "Desktop"
            val fp = q.first(if (combined) "tf" else "f")?.takeIf { it.length == 64 } ?: return null
            if (hosts.isEmpty()) return null
            val iroh = q.first("z")?.takeIf { it.length == 64 } ?: ""
            // Both halves of the route or neither: a host with no port (or
            // the reverse) dials nothing.
            val tr = q.first("tr")?.takeIf { it.isNotBlank() }
            val tq = q.first("tq")?.toIntOrNull()?.takeIf { it in 1..65535 }
            val (relayHost, relayPort) = if (tr != null && tq != null) tr to tq else "" to 0
            val ta = q.first("ta")?.let { decodeBase64Url(it) }?.takeIf { it.size == 32 }
            return PairLink(hosts, port, token, name, fp, iroh, relayHost, relayPort, ta)
        }

        /** base64url, no padding — the QR's byte fields. Null when the text
         *  is not that (padding, the wrong alphabet, a stray byte). */
        internal fun decodeBase64Url(s: String): ByteArray? {
            if (s.isEmpty() || s.contains('=')) return null
            return runCatching { java.util.Base64.getUrlDecoder().decode(s) }.getOrNull()
        }
    }

    // A ByteArray field makes the generated equals/hashCode identity-based;
    // spell them out so two parses of one QR compare equal.
    override fun equals(other: Any?): Boolean = other is PairLink &&
        hosts == other.hosts && port == other.port && token == other.token && name == other.name &&
        fingerprint == other.fingerprint && iroh == other.iroh && relayHost == other.relayHost &&
        relayPort == other.relayPort && relayAuthorization.contentEquals(other.relayAuthorization)
    override fun hashCode(): Int = listOf(hosts, port, token, name, fingerprint, iroh, relayHost, relayPort,
        relayAuthorization?.contentHashCode() ?: 0).hashCode()
}

/** The query of an `aiterm://pair?…` link, parsed without android.net.Uri
 *  so the parser runs on the plain JVM too. Same semantics as Uri's query
 *  reading: percent-decoding, `+` left alone, repeated keys kept in order. */
internal class Query private constructor(private val pairs: List<Pair<String, String>>) {
    fun first(key: String): String? = pairs.firstOrNull { it.first == key }?.second
    fun all(key: String): List<String> = pairs.filter { it.first == key }.map { it.second }

    companion object {
        private const val PREFIX = "aiterm://pair"

        fun parse(raw: String): Query? {
            if (!raw.startsWith(PREFIX)) return null
            val rest = raw.substring(PREFIX.length)
            // After the host: nothing, a path, a query or a fragment — never
            // more host (aiterm://pairing is a different place).
            if (rest.isNotEmpty() && rest[0] !in "/?#") return null
            val query = rest.substringAfter('?', "").substringBefore('#')
            val pairs = query.split('&').filter { it.isNotEmpty() }.map { part ->
                val eq = part.indexOf('=')
                if (eq < 0) decode(part) to "" else decode(part.substring(0, eq)) to decode(part.substring(eq + 1))
            }
            return Query(pairs)
        }

        private fun decode(s: String): String {
            if (!s.contains('%')) return s
            val out = java.io.ByteArrayOutputStream(s.length)
            var i = 0
            while (i < s.length) {
                val c = s[i]
                if (c == '%' && i + 2 < s.length) {
                    val hex = s.substring(i + 1, i + 3).toIntOrNull(16)
                    if (hex != null) { out.write(hex); i += 3; continue }
                }
                out.write(c.toString().toByteArray(Charsets.UTF_8))
                i++
            }
            return out.toString("UTF-8")
        }
    }
}
