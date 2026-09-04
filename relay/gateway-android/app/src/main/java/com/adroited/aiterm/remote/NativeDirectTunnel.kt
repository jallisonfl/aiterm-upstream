package com.adroited.aiterm.remote

import android.util.Base64
import com.adroited.aiterm.pairing.PairedDesktop
import java.io.Closeable

internal class NativeDirectTunnel private constructor(
    val localPort: Int,
    private val handle: Long,
) : Closeable {
    override fun close() = stopNative(handle)

    companion object {
        init {
            System.loadLibrary("aiterm_quic")
        }

        fun start(desktop: PairedDesktop, offer: RemoteDirectOffer): NativeDirectTunnel {
            val serverName = desktop.relayHost
                ?: throw RemoteProtocolException("a relay hostname is required for direct connection setup")
            val values = startNative(
                offer.host,
                offer.port,
                decode(offer.id),
                decode(offer.cookie),
                decode(desktop.serverSpkiFingerprint),
                serverName,
            )
            if (values.size != 2 || values[0] <= 0 || values[1] !in 1..65_535) {
                throw RemoteProtocolException("the native direct tunnel returned invalid details")
            }
            return NativeDirectTunnel(values[1].toInt(), values[0])
        }

        private fun decode(value: String): ByteArray = try {
            Base64.decode(value, Base64.URL_SAFE or Base64.NO_PADDING or Base64.NO_WRAP)
        } catch (error: IllegalArgumentException) {
            throw RemoteProtocolException("direct connection details were malformed", error)
        }

        @JvmStatic
        private external fun startNative(
            host: String,
            port: Int,
            id: ByteArray,
            cookie: ByteArray,
            fingerprint: ByteArray,
            serverName: String,
        ): LongArray

        @JvmStatic
        private external fun stopNative(handle: Long)
    }
}
