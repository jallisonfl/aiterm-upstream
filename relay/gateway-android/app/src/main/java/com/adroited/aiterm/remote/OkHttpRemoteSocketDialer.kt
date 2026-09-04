package com.adroited.aiterm.remote

import com.adroited.aiterm.pairing.PairedDesktop
import com.adroited.aiterm.pairing.tls13Context
import com.adroited.aiterm.security.PinnedSpkiTrustManager
import java.security.SecureRandom
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketAddress
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.ConnectionSpec
import okhttp3.HttpUrl
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.TlsVersion
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import okio.ByteString.Companion.toByteString
import javax.net.SocketFactory
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

/** Opens only TLS 1.3 WebSockets whose SPKI and hostname both match pairing. */
class OkHttpRemoteSocketDialer : DirectRemoteSocketDialer {
    override suspend fun open(desktop: PairedDesktop): RemoteBinarySocket = coroutineScope {
        require(desktop.hosts.isNotEmpty())
        val client = pinnedClient(desktop.serverSpkiFingerprint)
        val endpoints = orderedEndpoints(desktop)
        val winner = CompletableDeferred<RemoteBinarySocket>()
        val failures = AtomicInteger()
        // Happy-Eyeballs-style route staggering keeps LAN first without making a cellular
        // client wait for every unreachable private address to time out before trying relay.
        val attempts = endpoints.map { candidate ->
            launch {
                var opened: RemoteBinarySocket? = null
                try {
                    delay(routeDelayMillis(candidate.route))
                    opened = openEndpoint(
                        client,
                        endpoint(candidate.host, candidate.port),
                        candidate.route.remotePath,
                    )
                    if (winner.complete(opened)) opened = null
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (error: Exception) {
                    if (failures.incrementAndGet() == endpoints.size) {
                        winner.completeExceptionally(
                            RemoteProtocolException("no paired desktop address was reachable", error),
                        )
                    }
                } finally {
                    opened?.close()
                }
            }
        }
        try {
            winner.await()
        } finally {
            attempts.forEach { attempt ->
                attempt.cancelAndJoin()
            }
        }
    }

    override suspend fun openDirect(
        desktop: PairedDesktop,
        offer: RemoteDirectOffer,
    ): RemoteBinarySocket = withContext(Dispatchers.IO) {
        val relayHost = desktop.relayHost
            ?: throw RemoteProtocolException("a relay route is required for a direct connection")
        val relayPort = desktop.relayPort
            ?: throw RemoteProtocolException("a relay route is required for a direct connection")
        val tunnel = try {
            NativeDirectTunnel.start(desktop, offer)
        } catch (error: UnsatisfiedLinkError) {
            throw RemoteProtocolException("direct QUIC is not available on this device", error)
        }
        try {
            val client = pinnedClient(
                desktop.serverSpkiFingerprint,
                RedirectingSocketFactory(tunnel.localPort),
            )
            val socket = openEndpoint(client, endpoint(relayHost, relayPort), RemotePath.DIRECT)
            TunnelOwnedSocket(socket, tunnel)
        } catch (error: Exception) {
            tunnel.close()
            throw error
        }
    }

    internal fun routeDelayMillis(route: Route): Long = when (route) {
        Route.LAN -> 0L
        Route.VPN -> VPN_FALLBACK_DELAY_MILLIS
        Route.RELAY -> RELAY_FALLBACK_DELAY_MILLIS
    }

    internal data class Endpoint(val host: String, val port: Int, val route: Route)
    internal enum class Route { LAN, VPN, RELAY }

    private val Route.remotePath: RemotePath
        get() = when (this) {
            Route.LAN -> RemotePath.LAN
            Route.VPN -> RemotePath.VPN
            Route.RELAY -> RemotePath.RELAY
        }

    internal fun orderedEndpoints(desktop: PairedDesktop): List<Endpoint> {
        val direct = desktop.hosts.map { host ->
            Endpoint(host, desktop.port, if (isLanHost(host)) Route.LAN else Route.VPN)
        }.sortedBy { it.route.ordinal }
        val relay = desktop.relayHost?.let { host ->
            desktop.relayPort?.let { port -> Endpoint(host, port, Route.RELAY) }
        }
        return direct + listOfNotNull(relay)
    }

    private fun isLanHost(host: String): Boolean {
        val normalized = host.lowercase()
        if (normalized.startsWith("fc") || normalized.startsWith("fd")) return true
        val octets = host.split('.').mapNotNull(String::toIntOrNull)
        if (octets.size != 4 || octets.any { it !in 0..255 }) return false
        return octets[0] == 10 ||
            (octets[0] == 172 && octets[1] in 16..31) ||
            (octets[0] == 192 && octets[1] == 168) ||
            octets[0] == 127
    }

    private fun pinnedClient(
        fingerprint: String,
        socketFactory: SocketFactory = SocketFactory.getDefault(),
    ): OkHttpClient {
        val trustManager = PinnedSpkiTrustManager(fingerprint)
        val sslContext = tls13Context().apply {
            init(null, arrayOf(trustManager), SecureRandom())
        }
        val tls13Only = ConnectionSpec.Builder(ConnectionSpec.RESTRICTED_TLS)
            .tlsVersions(TlsVersion.TLS_1_3)
            .build()
        // Intentionally do not install a custom HostnameVerifier. The pin
        // replaces CA path validation, never endpoint-name validation.
        return OkHttpClient.Builder()
            .connectTimeout(5, TimeUnit.SECONDS)
            .socketFactory(socketFactory)
            .sslSocketFactory(sslContext.socketFactory, trustManager)
            .connectionSpecs(listOf(tls13Only))
            .build()
    }

    private class TunnelOwnedSocket(
        private val delegate: RemoteBinarySocket,
        private val tunnel: NativeDirectTunnel,
    ) : RemoteBinarySocket by delegate {
        override fun close() {
            delegate.close()
            tunnel.close()
        }
    }

    private class RedirectingSocketFactory(private val localPort: Int) : SocketFactory() {
        override fun createSocket(): Socket = RedirectingSocket(localPort)

        override fun createSocket(host: String, port: Int): Socket =
            createSocket().apply { connect(InetSocketAddress(host, port)) }

        override fun createSocket(host: String, port: Int, localHost: InetAddress, localPort: Int): Socket =
            createSocket().apply {
                bind(InetSocketAddress(localHost, localPort))
                connect(InetSocketAddress(host, port))
            }

        override fun createSocket(host: InetAddress, port: Int): Socket =
            createSocket().apply { connect(InetSocketAddress(host, port)) }

        override fun createSocket(
            address: InetAddress,
            port: Int,
            localAddress: InetAddress,
            localPort: Int,
        ): Socket = createSocket().apply {
            bind(InetSocketAddress(localAddress, localPort))
            connect(InetSocketAddress(address, port))
        }
    }

    private class RedirectingSocket(private val localPort: Int) : Socket() {
        override fun connect(endpoint: SocketAddress?) = connect(endpoint, 0)

        override fun connect(endpoint: SocketAddress?, timeout: Int) {
            val ipv4Loopback = InetAddress.getByAddress(byteArrayOf(127, 0, 0, 1))
            super.connect(InetSocketAddress(ipv4Loopback, localPort), timeout)
        }
    }

    private fun endpoint(host: String, port: Int): HttpUrl = HttpUrl.Builder()
        .scheme("https")
        .host(host)
        .port(port)
        .addPathSegment("v1")
        .addPathSegment("ws")
        .build()

    private suspend fun openEndpoint(
        client: OkHttpClient,
        url: HttpUrl,
        path: RemotePath,
    ): RemoteBinarySocket =
        suspendCancellableCoroutine { continuation ->
            val incoming = Channel<ByteArray>(MAX_QUEUED_FRAMES)
            val reference = AtomicReference<WebSocket?>()
            val opened = AtomicReference<OkHttpBinarySocket?>()
            val listener = object : WebSocketListener() {
                override fun onOpen(webSocket: WebSocket, response: Response) {
                    reference.set(webSocket)
                    val socket = OkHttpBinarySocket(
                        reference,
                        incoming,
                        RemoteEndpoint(url.host, url.port, path),
                    )
                    if (opened.compareAndSet(null, socket) && continuation.isActive) {
                        continuation.resume(socket)
                    }
                }

                override fun onMessage(webSocket: WebSocket, text: String) {
                    incoming.close(RemoteProtocolException("text remote frame received"))
                    webSocket.cancel()
                }

                override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
                    if (bytes.size <= 0 || bytes.size >= RemoteWireCodec.MAX_FRAME_BYTES ||
                        incoming.trySend(bytes.toByteArray()).isFailure
                    ) {
                        incoming.close(RemoteProtocolException("remote frame queue or size bound exceeded"))
                        webSocket.cancel()
                    }
                }

                override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                    incoming.close(RemoteProtocolException("desktop closed the remote connection"))
                    webSocket.cancel()
                }

                override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                    incoming.close(t)
                    if (continuation.isActive) continuation.resumeWithException(t)
                }
            }
            reference.set(client.newWebSocket(Request.Builder().url(url).build(), listener))
            continuation.invokeOnCancellation {
                reference.get()?.cancel()
                incoming.close()
            }
        }

    private class OkHttpBinarySocket(
        private val socket: AtomicReference<WebSocket?>,
        private val incoming: Channel<ByteArray>,
        override val endpoint: RemoteEndpoint,
    ) : RemoteBinarySocket {
        override suspend fun receive(): ByteArray {
            val result = incoming.receiveCatching()
            return result.getOrNull()
                ?: throw RemoteProtocolException("remote socket closed", result.exceptionOrNull())
        }

        override fun send(bytes: ByteArray): Boolean =
            bytes.size in 1 until RemoteWireCodec.MAX_FRAME_BYTES &&
                socket.get()?.send(bytes.toByteString()) == true

        override fun close() {
            socket.getAndSet(null)?.cancel()
            incoming.close()
        }
    }

    private companion object {
        const val MAX_QUEUED_FRAMES = 64
        const val VPN_FALLBACK_DELAY_MILLIS = 350L
        const val RELAY_FALLBACK_DELAY_MILLIS = 700L
    }
}
