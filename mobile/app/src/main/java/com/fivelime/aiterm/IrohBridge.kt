package com.fivelime.aiterm

import android.content.Context
import android.util.Base64
import android.util.Log
import computer.iroh.Connection
import computer.iroh.Endpoint
import computer.iroh.EndpointAddr
import computer.iroh.EndpointId
import computer.iroh.EndpointOptions
import computer.iroh.SecretKey
import computer.iroh.presetN0
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket

/** Reach a desktop with no reachable address: iroh dials its node id (the
 *  `z` of the pairing QR) and finds a path — direct when hole punching
 *  lands, a blind relay when it does not. Nothing above this layer changes:
 *  the bridge listens on loopback and copies bytes between each TCP
 *  connection and one iroh stream, so OkHttp speaks the same pinned TLS to
 *  `https://127.0.0.1:<port>` that it speaks to the desktop's LAN address,
 *  and the relay carries only ciphertext. */
object IrohBridge {
    private val ALPN = "aiterm/remote/0".toByteArray(Charsets.UTF_8)
    private const val TAG = "IrohBridge"

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val lock = Mutex()
    private var endpoint: Endpoint? = null
    /** node id → the loopback port bridging to it. */
    private val ports = HashMap<String, Int>()
    /** node id → the QUIC connection its streams ride, while it lasts. */
    private val conns = HashMap<String, Connection>()

    /** The local URL that reaches the given desktop, starting the bridge on
     *  first use. Deterministic port per node id, so a stored baseUrl from a
     *  previous run still points at the right desktop. */
    suspend fun urlFor(context: Context, nodeId: String): String = kotlinx.coroutines.withContext(Dispatchers.IO) {
        lock.withLock {
            ports[nodeId]?.let { return@withContext "https://127.0.0.1:$it" }
            val ep = bind(context)
            val preferred = 40000 + (nodeId.hashCode() and 0x7fffffff) % 20000
            val server = runCatching { ServerSocket(preferred, 16, InetAddress.getByName("127.0.0.1")) }
                .getOrElse { ServerSocket(0, 16, InetAddress.getByName("127.0.0.1")) }
            ports[nodeId] = server.localPort
            scope.launch { acceptLoop(server, ep, nodeId) }
            // Dial now rather than on the first TCP connection: discovery
            // plus the relay round trip can outlast a probe's patience, and
            // a warm QUIC connection turns the probe into one stream open.
            scope.launch {
                runCatching { connectionTo(ep, nodeId) }
                    .onFailure { Diag.log("iroh", "pre-dial of ${nodeId.take(8)} failed: ${it.message}") }
            }
            Diag.log("iroh", "bridging 127.0.0.1:${server.localPort} → ${nodeId.take(8)}")
            "https://127.0.0.1:${server.localPort}"
        }
    }

    private suspend fun bind(context: Context): Endpoint {
        endpoint?.let { return it }
        val ep = Endpoint.bind(
            EndpointOptions(preset = presetN0(), secretKey = secret(context), alpns = listOf(ALPN)),
        )
        endpoint = ep
        return ep
    }

    /** This phone's own iroh identity — stable so the desktop's logs can
     *  recognize it. Not a secret shared with anyone. */
    private fun secret(context: Context): ByteArray {
        val prefs = context.getSharedPreferences("aiterm.iroh", Context.MODE_PRIVATE)
        prefs.getString("secret", null)?.let {
            runCatching { return SecretKey.fromBytes(Base64.decode(it, Base64.NO_WRAP)).toBytes() }
        }
        val fresh = SecretKey.generate().toBytes()
        prefs.edit().putString("secret", Base64.encodeToString(fresh, Base64.NO_WRAP)).apply()
        return fresh
    }

    private suspend fun acceptLoop(server: ServerSocket, ep: Endpoint, nodeId: String) {
        while (true) {
            val socket = runCatching { server.accept() }.getOrNull() ?: return
            scope.launch {
                try {
                    bridge(ep, nodeId, socket)
                } catch (t: Throwable) {
                    Diag.log("iroh", "bridge to ${nodeId.take(8)} failed: ${t.message}")
                    runCatching { socket.close() }
                }
            }
        }
    }

    /** One QUIC connection per desktop, redialed when it has died; one
     *  bi-stream per TCP connection. */
    private suspend fun connectionTo(ep: Endpoint, nodeId: String): Connection {
        lock.withLock {
            conns[nodeId]?.let { c ->
                if (c.closeReason() == null) return c
                conns.remove(nodeId)
            }
            Diag.log("iroh", "dialing ${nodeId.take(8)}…")
            val addr = EndpointAddr(EndpointId.fromString(nodeId), null, emptyList())
            val c = kotlinx.coroutines.withTimeout(20_000) { ep.connect(addr, ALPN) }
            Diag.log("iroh", "dialed ${nodeId.take(8)}: ${c.paths().joinToString { p -> if (p.isRelay) "relay" else "direct" }}")
            conns[nodeId] = c
            return c
        }
    }

    private suspend fun bridge(ep: Endpoint, nodeId: String, socket: Socket) {
        val bi = try {
            connectionTo(ep, nodeId).openBi()
        } catch (t: Throwable) {
            // The cached connection may have died without saying so; one
            // fresh dial before giving up on this TCP connection.
            lock.withLock { conns.remove(nodeId) }
            connectionTo(ep, nodeId).openBi()
        }
        socket.tcpNoDelay = true
        val up = scope.launch {
            val input = socket.getInputStream()
            val buf = ByteArray(16 * 1024)
            try {
                while (true) {
                    val n = input.read(buf)
                    if (n < 0) break
                    if (n > 0) bi.send().writeAll(buf.copyOf(n))
                }
                bi.send().finish()
            } catch (_: Throwable) {
            }
        }
        val down = scope.launch {
            val output = socket.getOutputStream()
            try {
                while (true) {
                    val chunk = bi.recv().read(64u * 1024u)
                    if (chunk.isEmpty()) break
                    output.write(chunk)
                    output.flush()
                }
            } catch (_: Throwable) {
            } finally {
                runCatching { socket.close() }
            }
        }
        up.join()
        down.join()
        runCatching { socket.close() }
    }
}
