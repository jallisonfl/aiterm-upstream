package com.adroited.aiterm.pairing

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class PairedDesktopJsonTest {

    private val validFingerprint = ByteArray(32) { 7 }.toBase64Url()

    @Test
    fun strictVersionedJson_decodesPrivateMetadata() {
        val decoded = PairedDesktopJson.decode(
            """{"version":1,"desktops":[{"device_id":"device-7","display_name":"Workshop PC","hosts":["192.168.1.7","desktop.local"],"port":8443,"server_spki_fingerprint":"$validFingerprint","last_seen_epoch_millis":null}]}""",
        )

        assertEquals(
            PairedDesktop(
                deviceId = "device-7",
                displayName = "Workshop PC",
                hosts = listOf("192.168.1.7", "desktop.local"),
                port = 8443,
                serverSpkiFingerprint = validFingerprint,
                lastSeenEpochMillis = null,
            ),
            decoded.single(),
        )
    }

    @Test
    fun corruptedUnknownVersionOrInvalidMetadata_failsExplicitly() {
        val invalidDocuments = listOf(
            "not-json",
            """{"version":2,"desktops":[]}""",
            """{"version":1,"desktops":[],"secret":"must-not-be-accepted"}""",
            """{"version":1,"desktops":[{"device_id":"","display_name":"PC","hosts":["desktop.local"],"port":8443,"server_spki_fingerprint":"$validFingerprint","last_seen_epoch_millis":null}]}""",
            """{"version":1,"desktops":[{"device_id":"device-7","display_name":"PC","hosts":["desktop.local"],"port":8443,"server_spki_fingerprint":"bad","last_seen_epoch_millis":null}]}""",
        )

        invalidDocuments.forEach { document ->
            assertThrows(PairedDesktopStoreException::class.java) {
                PairedDesktopJson.decode(document)
            }
        }
    }
}
