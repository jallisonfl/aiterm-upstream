package com.adroited.aiterm.remote

import com.adroited.aiterm.pairing.PairedDesktop
import org.junit.Assert.assertEquals
import org.junit.Test

class RemoteEndpointOrderingTest {
    @Test
    fun directLanThenVpnThenRelayIsDeterministic() {
        val desktop = PairedDesktop(
            deviceId = "device-7",
            displayName = "Desktop",
            hosts = listOf("100.90.1.2", "desktop.vpn", "192.168.1.20", "10.0.0.4"),
            port = 8443,
            serverSpkiFingerprint = "pin",
            lastSeenEpochMillis = null,
            relayHost = "desk-1234.relay.example.com",
            relayPort = 443,
        )

        assertEquals(
            listOf(
                OkHttpRemoteSocketDialer.Endpoint("192.168.1.20", 8443, OkHttpRemoteSocketDialer.Route.LAN),
                OkHttpRemoteSocketDialer.Endpoint("10.0.0.4", 8443, OkHttpRemoteSocketDialer.Route.LAN),
                OkHttpRemoteSocketDialer.Endpoint("100.90.1.2", 8443, OkHttpRemoteSocketDialer.Route.VPN),
                OkHttpRemoteSocketDialer.Endpoint("desktop.vpn", 8443, OkHttpRemoteSocketDialer.Route.VPN),
                OkHttpRemoteSocketDialer.Endpoint("desk-1234.relay.example.com", 443, OkHttpRemoteSocketDialer.Route.RELAY),
            ),
            OkHttpRemoteSocketDialer().orderedEndpoints(desktop),
        )
    }

    @Test
    fun fallbackRoutesUseShortStaggerInsteadOfSerialSocketTimeouts() {
        val dialer = OkHttpRemoteSocketDialer()

        assertEquals(0L, dialer.routeDelayMillis(OkHttpRemoteSocketDialer.Route.LAN))
        assertEquals(350L, dialer.routeDelayMillis(OkHttpRemoteSocketDialer.Route.VPN))
        assertEquals(700L, dialer.routeDelayMillis(OkHttpRemoteSocketDialer.Route.RELAY))
    }
}
