package com.adroited.aiterm.remote

import com.adroited.aiterm.terminal.TerminalColor
import java.io.ByteArrayOutputStream
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class RemoteWireCodecTest {

    @Test
    fun requestEnvelopeMatchesTheRustCborShape() {
        val encoded = RemoteWireCodec.encodeRequest(
            RemoteRequest(requestId = 1, kind = "tab.list", payload = byteArrayOf()),
        )

        assertArrayEquals(
            hex(
                "a4" +
                    "6776657273696f6e01" +
                    "6a726571756573745f696401" +
                    "646b696e64687461622e6c697374" +
                    "677061796c6f616440",
            ),
            encoded,
        )
    }

    @Test
    fun rustEventFixtureDecodesWithCorrelationAndByteStringPayload() {
        val event = RemoteWireCodec.decodeEvent(
            hex(
                "a4" +
                    "6776657273696f6e01" +
                    "6a726571756573745f696407" +
                    "646b696e64687461622e6c697374" +
                    "677061796c6f616443010203",
            ),
        )

        assertEquals(7L, event.requestId)
        assertEquals("tab.list", event.kind)
        assertArrayEquals(byteArrayOf(1, 2, 3), event.payload)
    }

    @Test
    fun duplicateEnvelopeFieldsAndTrailingCborAreRejected() {
        assertThrows(RemoteProtocolException::class.java) {
            RemoteWireCodec.decodeEvent(
                hex(
                    "a5" +
                        "6776657273696f6e01" +
                        "6776657273696f6e01" +
                        "6a726571756573745f696407" +
                        "646b696e64687461622e6c697374" +
                        "677061796c6f616440",
                ),
            )
        }
        assertThrows(RemoteProtocolException::class.java) {
            RemoteWireCodec.decodeEvent(
                hex(
                    "a4" +
                        "6776657273696f6e01" +
                        "6a726571756573745f696407" +
                        "646b696e64687461622e6c697374" +
                        "677061796c6f616440" +
                        "f6",
                ),
            )
        }
    }

    @Test
    fun rustShapedSnapshotChunkDecodesIntoNativeTerminalCells() {
        val attributes = linkedMapOf(
            "bold" to true,
            "faint" to false,
            "italic" to false,
            "underline" to false,
            "inverse" to false,
            "hidden" to false,
            "strikethrough" to false,
        )
        val cell = linkedMapOf(
            "text" to "λ",
            "width" to 1,
            "continuation" to false,
            "foreground" to "Default",
            "background" to linkedMapOf("Indexed" to 4),
            "attributes" to attributes,
        )
        val part = fixture(
            linkedMapOf(
                "cols" to 1,
                "rows" to 1,
                "visible" to listOf(linkedMapOf("cells" to listOf(cell), "wrapped" to false)),
                "cursor" to linkedMapOf(
                    "col" to 0,
                    "row" to 0,
                    "visible" to true,
                    "shape" to "Beam",
                ),
                "modes" to linkedMapOf(
                    "application_cursor" to false,
                    "bracketed_paste" to true,
                    "line_wrap" to true,
                    "alternate_screen" to false,
                ),
            ),
        )
        val encoded = fixture(
            linkedMapOf(
                "transfer_id" to "transfer-1",
                "tab_id" to "tab-1",
                "attachment_id" to "attachment-1",
                "kind" to "snapshot",
                "base_revision" to 9,
                "final_revision" to 9,
                "row_start" to 0,
                "row_end" to 1,
                "index" to 0,
                "total" to 1,
                "request_id" to 7,
                "payload" to part,
            ),
        )

        val chunk = RemoteWireCodec.decodeTerminalChunk(encoded, expectedRequestId = 7)

        assertEquals(TerminalTransferKind.Snapshot, chunk.kind)
        val snapshot = chunk.part as TerminalTransferPart.Snapshot
        assertEquals("λ", snapshot.visible.single().cells.single().text)
        assertEquals(TerminalColor.Indexed(4), snapshot.visible.single().cells.single().background)
        assertEquals(true, snapshot.visible.single().cells.single().attributes.bold)
    }

    @Test
    fun snapshotChunkAcceptsAValidPayloadLargerThanTheCollectionItemLimit() {
        val attributes = linkedMapOf(
            "bold" to false,
            "faint" to false,
            "italic" to false,
            "underline" to false,
            "inverse" to false,
            "hidden" to false,
            "strikethrough" to false,
        )
        val cell = linkedMapOf(
            "text" to "x",
            "width" to 1,
            "continuation" to false,
            "foreground" to "Default",
            "background" to "Default",
            "attributes" to attributes,
        )
        val visible = List(64) {
            linkedMapOf("cells" to List(80) { cell }, "wrapped" to false)
        }
        val part = fixture(
            linkedMapOf(
                "cols" to 80,
                "rows" to 64,
                "visible" to visible,
                "cursor" to linkedMapOf(
                    "col" to 0,
                    "row" to 0,
                    "visible" to true,
                    "shape" to "Block",
                ),
                "modes" to linkedMapOf(
                    "application_cursor" to false,
                    "bracketed_paste" to false,
                    "line_wrap" to true,
                    "alternate_screen" to false,
                ),
            ),
        )
        val encoded = fixture(
            linkedMapOf(
                "transfer_id" to "transfer-large",
                "tab_id" to "tab-1",
                "attachment_id" to "attachment-1",
                "kind" to "snapshot",
                "base_revision" to 9,
                "final_revision" to 9,
                "row_start" to 0,
                "row_end" to 64,
                "index" to 0,
                "total" to 1,
                "request_id" to 7,
                "payload" to part,
            ),
        )
        require(part.size > 300_000)
        require(encoded.size < RemoteWireCodec.MAX_FRAME_BYTES)

        val chunk = RemoteWireCodec.decodeTerminalChunk(encoded, expectedRequestId = 7)

        val snapshot = chunk.part as TerminalTransferPart.Snapshot
        assertEquals(64, snapshot.visible.size)
        assertEquals(80, snapshot.visible.first().cells.size)
    }

    @Test
    fun rosterDescriptorUsesTheRustCamelCaseFieldContract() {
        val payload = fixture(
            linkedMapOf(
                "transfer_id" to "roster-1",
                "revision" to 12,
                "index" to 0,
                "total" to 1,
                "tabs" to listOf(
                    linkedMapOf(
                        "id" to "tab-1",
                        "title" to "Shell",
                        "sessionId" to "session-1",
                        "size" to linkedMapOf("cols" to 80, "rows" to 24),
                        "focus" to "self",
                        "state" to "running",
                    ),
                ),
            ),
        )

        val chunk = RemoteWireCodec.decodeStateSnapshot(payload)

        assertEquals("session-1", chunk.tabs.single().sessionId)
        assertEquals(WireFocusOwner.Self, chunk.tabs.single().focus)
    }

    @Test
    fun terminalImageUploadPayloadsMatchTheRustCborContract() {
        val digest = ByteArray(32) { it.toByte() }

        assertArrayEquals(
            hex(
                "a9" +
                    "667461625f6964" + "63746162" +
                    "6d6174746163686d656e745f6964" + "6a6174746163686d656e74" +
                    "6d7375626d697373696f6e5f6964" + "6a7375626d697373696f6e" +
                    "707375626d697373696f6e5f636f756e74" + "02" +
                    "6c6d656d6265725f696e646578" + "00" +
                    "707375626d697373696f6e5f6279746573" + "19012c" +
                    "666c656e677468" + "182a" +
                    "6a6d656469615f74797065" + "6a696d6167652f6a706567" +
                    "66736861323536" + "5820" +
                    "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            ),
            RemoteCommands.uploadBegin(
                tabId = "tab",
                attachmentId = "attachment",
                submissionId = "submission",
                submissionCount = 2,
                submissionBytes = 300,
                length = 42,
                sha256 = digest,
            ),
        )
        assertArrayEquals(
            hex("a36975706c6f61645f69646675706c6f616465696e64657807646461746143010203"),
            RemoteCommands.uploadChunk("upload", 7, byteArrayOf(1, 2, 3)),
        )
        assertArrayEquals(
            hex("a16975706c6f61645f69646675706c6f6164"),
            RemoteCommands.uploadFinish("upload"),
        )
        assertArrayEquals(
            hex("a16975706c6f61645f69646675706c6f6164"),
            RemoteCommands.uploadCancel("upload"),
        )
    }

    @Test
    fun terminalImageUploadRepliesAreStrictAndBounded() {
        val began = RemoteCommands.uploadBegan(
            fixture(linkedMapOf("upload_id" to "upload-1", "next_chunk" to 0)),
        )
        assertEquals("upload-1", began.uploadId)
        assertEquals(0, began.nextChunk)
        assertEquals(
            "/project/.aiterm/attachments/upload.jpg",
            RemoteCommands.uploadedPath(
                fixture(linkedMapOf("path" to "/project/.aiterm/attachments/upload.jpg")),
            ),
        )
        RemoteCommands.uploadAcknowledged(fixture(linkedMapOf("ok" to true)))

        assertThrows(RemoteProtocolException::class.java) {
            RemoteCommands.uploadBegan(
                fixture(linkedMapOf("upload_id" to "upload-1", "next_chunk" to 0, "unexpected" to true)),
            )
        }
        assertThrows(RemoteProtocolException::class.java) {
            RemoteCommands.uploadedPath(fixture(linkedMapOf("path" to "x".repeat(4_097))))
        }
        assertThrows(RemoteProtocolException::class.java) {
            RemoteCommands.uploadAcknowledged(fixture(linkedMapOf("ok" to true, "unexpected" to true)))
        }
    }

    @Test
    fun terminalImageUploadCommandsRejectMalformedOrOverBoundValues() {
        val digest = ByteArray(32)

        assertThrows(RemoteProtocolException::class.java) {
            RemoteCommands.uploadBegin(
                tabId = "x".repeat(4_097),
                attachmentId = "attachment",
                submissionId = "submission",
                submissionCount = 1,
                submissionBytes = 1,
                length = 1,
                sha256 = digest,
            )
        }
        assertThrows(RemoteProtocolException::class.java) {
            RemoteCommands.uploadBegin(
                tabId = "tab",
                attachmentId = "attachment",
                submissionId = "submission",
                submissionCount = 1,
                submissionBytes = 1,
                length = 1,
                sha256 = ByteArray(31),
            )
        }
        assertThrows(RemoteProtocolException::class.java) {
            RemoteCommands.uploadChunk("x".repeat(4_097), 0, byteArrayOf(1))
        }
        assertThrows(RemoteProtocolException::class.java) {
            RemoteCommands.uploadChunk("upload", 0, ByteArray(256 * 1_024 + 1))
        }
    }

    @Test
    fun terminalImageUploadSubmissionIdAccepts128Utf8BytesAndRejects129() {
        val digest = ByteArray(32)

        RemoteCommands.uploadBegin(
            tabId = "tab",
            attachmentId = "attachment",
            submissionId = "é".repeat(64),
            submissionCount = 1,
            submissionBytes = 1,
            length = 1,
            sha256 = digest,
        )
        assertThrows(RemoteProtocolException::class.java) {
            RemoteCommands.uploadBegin(
                tabId = "tab",
                attachmentId = "attachment",
                submissionId = "é".repeat(65),
                submissionCount = 1,
                submissionBytes = 1,
                length = 1,
                sha256 = digest,
            )
        }
    }

    @Test
    fun sessionChangesAndFileChunksUseStrictBoundedShapes() {
        val changes = RemoteCommands.sessionChanges(
            fixture(
                linkedMapOf(
                    "changes" to listOf(
                        linkedMapOf(
                            "path" to "/project/src/Main.kt",
                            "name" to "Main.kt",
                            "kind" to "modified",
                            "at" to 42L,
                            "session_id" to "session-1",
                            "bytes" to 12L,
                        ),
                    ),
                ),
            ),
        )
        assertEquals("/project/src/Main.kt", changes.single().path)
        assertEquals("modified", changes.single().kind)

        val chunk = RemoteCommands.fileChunk(
            fixture(
                linkedMapOf(
                    "path" to "/project/src/Main.kt",
                    "mime" to "text/plain",
                    "offset" to 4L,
                    "total" to 7L,
                    "eof" to true,
                    "data" to byteArrayOf(5, 6, 7),
                ),
            ),
        )
        assertEquals(4L, chunk.offset)
        assertArrayEquals(byteArrayOf(5, 6, 7), chunk.data)

        assertThrows(RemoteProtocolException::class.java) {
            RemoteCommands.fileChunk(
                fixture(
                    linkedMapOf(
                        "path" to "/project/src/Main.kt",
                        "mime" to "text/plain",
                        "offset" to 4L,
                        "total" to 7L,
                        "eof" to false,
                        "data" to byteArrayOf(5, 6, 7),
                    ),
                ),
            )
        }
    }

    @Test
    fun uploadMemberIndexMustBelongToTheDeclaredSubmission() {
        assertThrows(RemoteProtocolException::class.java) {
            RemoteCommands.uploadBegin(
                tabId = "tab",
                attachmentId = "attachment",
                submissionId = "submission",
                submissionCount = 2,
                memberIndex = 2,
                submissionBytes = 2,
                length = 1,
                sha256 = ByteArray(32),
            )
        }
    }

    private fun fixture(value: Any?): ByteArray {
        val output = ByteArrayOutputStream()
        fun writeHeader(major: Int, size: Int) {
            if (size < 24) {
                output.write((major shl 5) or size)
            } else if (size <= 0xff) {
                output.write((major shl 5) or 24)
                output.write(size)
            } else if (size <= 0xffff) {
                output.write((major shl 5) or 25)
                output.write(size ushr 8)
                output.write(size)
            } else {
                output.write((major shl 5) or 26)
                output.write(size ushr 24)
                output.write(size ushr 16)
                output.write(size ushr 8)
                output.write(size)
            }
        }
        fun write(item: Any?) {
            when (item) {
                null -> output.write(0xf6)
                is Boolean -> output.write(if (item) 0xf5 else 0xf4)
                is Int -> {
                    writeHeader(0, item)
                }
                is Long -> {
                    require(item <= Int.MAX_VALUE)
                    writeHeader(0, item.toInt())
                }
                is String -> {
                    val bytes = item.encodeToByteArray()
                    writeHeader(3, bytes.size)
                    output.write(bytes)
                }
                is ByteArray -> {
                    writeHeader(2, item.size)
                    output.write(item)
                }
                is List<*> -> {
                    writeHeader(4, item.size)
                    item.forEach(::write)
                }
                is Map<*, *> -> {
                    writeHeader(5, item.size)
                    item.forEach { (key, entry) ->
                        write(key as String)
                        write(entry)
                    }
                }
                else -> error("unsupported fixture type ${item::class}")
            }
        }
        write(value)
        return output.toByteArray()
    }

    private fun hex(value: String): ByteArray =
        value.chunked(2).map { it.toInt(16).toByte() }.toByteArray()
}
