package com.fivelime.aiterm

/** The ways a phone can reach a desktop (docs/remote/remote-roads.md). Every road
 *  carries the same pinned TLS; they differ only in how the bytes get there
 *  and which the person would rather use first. */
enum class Road(val id: String, val title: String, val blurb: String) {
    LAN("lan", "LAN", "Same network, straight to the desktop"),
    VPN("vpn", "VPN", "Tailscale, WireGuard or any VPN address"),
    RELAY("relay", "AITerm Relay", "A blind relay that routes by name; sees only ciphertext"),
    IROH("iroh", "iroh", "Peer-to-peer, with a relay fallback and no server of ours"),
    ;

    companion object {
        fun byId(id: String): Road? = entries.firstOrNull { it.id == id }
    }
}

/** The order a fresh desktop tries its roads: nearest first, the ones that
 *  need someone else's server last. */
val DEFAULT_ROAD_ORDER: List<String> = listOf("lan", "vpn", "relay", "iroh")

/** One URL to probe, and the road it rides. */
data class Candidate(val road: Road, val url: String) {
    /** How long a probe waits before calling this address dead. The iroh
     *  bridge's first reach includes discovery and the relay handshake, so
     *  it gets more patience than a plain address. */
    val patienceSeconds: Long get() = if (road == Road.IROH) 15L else 4L
}

/** Road classification and candidate building, in one place, so pairing,
 *  the connect sprint and the drawer's reachability probe all agree on
 *  what is "local" and what is tried first. Pure Kotlin: no Android, so
 *  the rules are unit-testable. */
object Roads {
    /** The roads named in `order`, deduplicated, unknown names dropped.
     *  Roads it leaves out are never dialed. */
    fun order(order: List<String>): List<Road> = order.mapNotNull { Road.byId(it) }.distinct()

    /** Every road, each once — the only order worth adopting from a
     *  desktop: anything less would leave a road undialed. */
    fun isComplete(order: List<String>): Boolean =
        order.size == Road.entries.size && order(order).size == Road.entries.size

    /** Which road a bare host rides when dialed directly. RFC1918 and
     *  link-local are `lan`; 100.64/10 (Tailscale/CGNAT), fc00::/7 (ULA —
     *  Tailscale's v6) and any other host, name or public address, are
     *  `vpn`: they are reached over something, not the local wire. Loopback
     *  is nobody's host — it is the iroh bridge — and classifies as null. */
    fun classifyHost(host: String): Road? {
        val h = host.trim().removePrefix("[").removeSuffix("]").substringBefore('%')
        val v4 = h.split('.')
        if (v4.size == 4 && v4.all { it.toIntOrNull()?.let { n -> n in 0..255 } == true }) {
            val o = v4.map { it.toInt() }
            return when {
                o[0] == 127 -> null
                o[0] == 10 -> Road.LAN
                o[0] == 172 && o[1] in 16..31 -> Road.LAN
                o[0] == 192 && o[1] == 168 -> Road.LAN
                o[0] == 169 && o[1] == 254 -> Road.LAN
                else -> Road.VPN // 100.64/10 and public alike: reached over something
            }
        }
        if (h.contains(':')) {
            val lower = h.lowercase()
            return when {
                lower == "::1" -> null
                lower.startsWith("fe8") || lower.startsWith("fe9") || lower.startsWith("fea") || lower.startsWith("feb") -> Road.LAN
                else -> Road.VPN // fc00::/7 and everything else
            }
        }
        return Road.VPN // a name: MagicDNS, a VPN hostname, a DNS record
    }

    /** The host inside an `https://host:port` candidate. */
    fun hostOf(url: String): String {
        val rest = url.substringAfter("://").substringBefore('/')
        if (rest.startsWith("[")) return rest.substringBefore(']').removePrefix("[")
        return rest.substringBefore(':')
    }

    fun classifyUrl(url: String): Road? = classifyHost(hostOf(url))

    /** The direct (lan/vpn) addresses a desktop knows, last winner first,
     *  with the bridge and relay dials — which carry other ports — left out. */
    fun directUrls(d: Desktop): List<String> {
        val relay = d.relayUrl
        return d.ordered.filter { it != relay && classifyUrl(it) != null }
    }

    /** Everything worth dialing for `d`, in its road order; inside a road,
     *  the address that answered last comes first, then the QR's order.
     *  `irohUrl` is the bridge's loopback URL when the bridge is up, null
     *  when it is not (or the desktop has no node id). A road with nothing
     *  to dial contributes nothing. */
    fun candidates(d: Desktop, irohUrl: String?): List<Candidate> {
        val out = ArrayList<Candidate>()
        val relay = d.relayUrl
        for (road in order(d.roadOrder)) {
            when (road) {
                Road.LAN, Road.VPN -> d.ordered.filter { it != relay && classifyUrl(it) == road }.forEach { out += Candidate(road, it) }
                Road.RELAY -> relay?.let { out += Candidate(road, it) }
                Road.IROH -> if (d.iroh.isNotEmpty() && irohUrl != null) out += Candidate(road, irohUrl)
            }
        }
        return out.distinctBy { it.url }
    }

    /** Whether this desktop has anything to dial on the road — hosts of
     *  that class, an enrolled relay, an iroh node id. */
    fun offers(d: Desktop, road: Road): Boolean = when (road) {
        Road.LAN, Road.VPN -> directUrls(d).any { classifyUrl(it) == road }
        Road.RELAY -> d.relayUrl != null
        Road.IROH -> d.iroh.isNotEmpty()
    }

    /** Position of a road in the desktop's preference; roads not listed
     *  rank after every listed one. */
    fun rank(d: Desktop, road: Road): Int = order(d.roadOrder).indexOf(road).let { if (it < 0) Int.MAX_VALUE else it }
}
