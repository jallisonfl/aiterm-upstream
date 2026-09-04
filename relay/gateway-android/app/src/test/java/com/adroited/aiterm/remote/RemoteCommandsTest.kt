package com.adroited.aiterm.remote

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.cbor.Cbor
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class RemoteCommandsTest {
    @Test
    fun terminalInputUsesTheExactStrictRustPayloadShape() {
        assertArrayEquals(
            hex(
                "a3" +
                    "667461625f69646174" +
                    "6d6174746163686d656e745f69646161" +
                    "64646174614178",
            ),
            RemoteCommands.input("t", "a", byteArrayOf('x'.code.toByte())),
        )
    }

    @Test
    fun sessionPreviewDecodesTheExactRustMessageShape() {
        val messages = RemoteCommands.sessionPreview(
            hex(
                "a1" +
                    "686d65737361676573" +
                    "81a3" +
                    "64726f6c656475736572" +
                    "64746578746568656c6c6f" +
                    "626174f6",
            ),
        )

        assertEquals(listOf(RemotePreviewMessage("user", "hello")), messages)
    }

    @Test
    fun sessionPreviewAcceptsTheRustTimestampField() {
        val messages = RemoteCommands.sessionPreview(
            hex(
                "a1" +
                    "686d65737361676573" +
                    "81a3" +
                    "64726f6c656475736572" +
                    "64746578746568656c6c6f" +
                    "62617474323032362d30382d32395432333a34323a30305a",
            ),
        )

        assertEquals("2026-08-29T23:42:00Z", messages.single().at)
    }

    @OptIn(ExperimentalSerializationApi::class)
    @Test
    fun webPreviewReplyAcceptsOnlyAnAvailableTicketPath() {
        val cbor = Cbor {
            encodeDefaults = true
            useDefiniteLengthEncoding = true
        }
        val ticket = "/v1/preview/${"a".repeat(43)}/"
        val available = RemoteCommands.webPreview(
            cbor.encodeToByteArray(WebPreviewWire.serializer(), WebPreviewWire(true, ticket)),
        )
        assertEquals(RemoteWebPreview(true, ticket), available)
        assertEquals(
            RemoteWebPreview(false, null),
            RemoteCommands.webPreview(
                cbor.encodeToByteArray(WebPreviewWire.serializer(), WebPreviewWire(false, null)),
            ),
        )
        assertThrows(RemoteProtocolException::class.java) {
            RemoteCommands.webPreview(
                cbor.encodeToByteArray(
                    WebPreviewWire.serializer(),
                    WebPreviewWire(true, "/v1/preview/../../secret/"),
                ),
            )
        }
    }

    @OptIn(ExperimentalSerializationApi::class)
    @Test
    fun sessionRosterDecodesNativeMetadataWithoutChangingTheSessionShape() {
        val session = RemoteSession(
            id = "session-1",
            agent = "codex",
            title = "Native roster",
            projectPath = "/work/aiterm",
            groupPath = "/work/aiterm",
            branch = "main",
            forked = false,
            background = false,
            forkParent = null,
            lastActive = 10,
        )
        val payload = Cbor {
            encodeDefaults = true
            useDefiniteLengthEncoding = true
        }.encodeToByteArray(
            SessionRosterWire.serializer(),
            SessionRosterWire(
                sessions = listOf(session),
                withFiles = listOf(session.id),
                stars = listOf(session.id),
                broughtIn = emptyMap(),
                activity = mapOf(session.id to "output"),
            ),
        )

        val roster = RemoteCommands.sessionRoster(payload)

        assertEquals(listOf(session), roster.sessions)
        assertEquals(setOf(session.id), roster.withFiles)
        assertEquals(setOf(session.id), roster.stars)
        assertEquals(mapOf(session.id to "output"), roster.activity)
    }

    @Serializable
    private data class SessionRosterWire(
        val sessions: List<RemoteSession>,
        @SerialName("with_files") val withFiles: List<String>,
        val stars: List<String>,
        @SerialName("brought_in") val broughtIn: Map<String, String>,
        val activity: Map<String, String>,
    )

    @Serializable
    private data class WebPreviewWire(
        val available: Boolean,
        val path: String?,
    )

    private fun hex(value: String): ByteArray =
        value.chunked(2).map { it.toInt(16).toByte() }.toByteArray()
}
