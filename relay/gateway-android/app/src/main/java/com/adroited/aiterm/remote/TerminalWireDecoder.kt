package com.adroited.aiterm.remote

import com.adroited.aiterm.terminal.CellAttributes
import com.adroited.aiterm.terminal.CursorShape
import com.adroited.aiterm.terminal.CursorState
import com.adroited.aiterm.terminal.RowPatch
import com.adroited.aiterm.terminal.ScreenCell
import com.adroited.aiterm.terminal.ScreenRow
import com.adroited.aiterm.terminal.TerminalColor
import com.adroited.aiterm.terminal.TerminalModes

internal object TerminalWireDecoder {
    fun decode(bytes: ByteArray, expectedRequestId: Long): TerminalTransferChunk {
        val root = CborValueReader(bytes).read() as? CborValue.MapValue ?: malformed()
        root.exact(
            "transfer_id",
            "tab_id",
            "attachment_id",
            "kind",
            "base_revision",
            "final_revision",
            "row_start",
            "row_end",
            "index",
            "total",
            "request_id",
            "payload",
        )
        val requestId = root.unsigned("request_id")
        if (requestId != expectedRequestId) malformed()
        val kind = when (root.text("kind")) {
            "snapshot" -> TerminalTransferKind.Snapshot
            "diff" -> TerminalTransferKind.Diff
            "scrollback" -> TerminalTransferKind.Scrollback
            else -> malformed()
        }
        val payload = root.bytes("payload")
        if (payload.isEmpty() || payload.size >= RemoteWireCodec.MAX_FRAME_BYTES) malformed()
        val part = CborValueReader(payload).read() as? CborValue.MapValue ?: malformed()
        return TerminalTransferChunk(
            transferId = root.text("transfer_id"),
            tabId = root.text("tab_id"),
            attachmentId = root.optionalText("attachment_id"),
            kind = kind,
            baseRevision = root.unsigned("base_revision"),
            finalRevision = root.unsigned("final_revision"),
            rowStart = root.int("row_start"),
            rowEnd = root.int("row_end"),
            index = root.int("index"),
            total = root.int("total"),
            requestId = requestId,
            part = when (kind) {
                TerminalTransferKind.Snapshot -> snapshot(part)
                TerminalTransferKind.Diff -> diff(part)
                TerminalTransferKind.Scrollback -> scrollback(part)
            },
        )
    }

    private fun snapshot(value: CborValue.MapValue): TerminalTransferPart.Snapshot {
        value.exact("cols", "rows", "visible", "cursor", "modes")
        return TerminalTransferPart.Snapshot(
            cols = value.int("cols"),
            rows = value.int("rows"),
            visible = value.array("visible").map(::row),
            cursor = cursor(value.map("cursor")),
            modes = modes(value.map("modes")),
        )
    }

    private fun diff(value: CborValue.MapValue): TerminalTransferPart.Diff {
        value.exact("rows", "cursor", "modes")
        return TerminalTransferPart.Diff(
            patches = value.array("rows").map { entry ->
                val patch = entry as? CborValue.MapValue ?: malformed()
                patch.exact("row", "content")
                RowPatch(patch.int("row"), row(patch.map("content")))
            },
            cursor = value.optionalMap("cursor")?.let(::cursor),
            modes = value.optionalMap("modes")?.let(::modes),
        )
    }

    private fun scrollback(value: CborValue.MapValue): TerminalTransferPart.Scrollback {
        value.exact("rows")
        return TerminalTransferPart.Scrollback(value.array("rows").map(::row))
    }

    private fun row(value: CborValue): ScreenRow {
        val map = value as? CborValue.MapValue ?: malformed()
        map.exact("cells", "wrapped")
        return ScreenRow(map.array("cells").map(::cell), map.boolean("wrapped"))
    }

    private fun cell(value: CborValue): ScreenCell {
        val map = value as? CborValue.MapValue ?: malformed()
        map.exact("text", "width", "continuation", "foreground", "background", "attributes")
        return ScreenCell(
            text = map.text("text"),
            width = map.int("width"),
            continuation = map.boolean("continuation"),
            foreground = color(map.required("foreground")),
            background = color(map.required("background")),
            attributes = attributes(map.map("attributes")),
        )
    }

    private fun color(value: CborValue): TerminalColor = when (value) {
        is CborValue.Text -> if (value.value == "Default") TerminalColor.Default else malformed()
        is CborValue.MapValue -> {
            if (value.values.size != 1) malformed()
            when (value.values.keys.single()) {
                "Indexed" -> TerminalColor.Indexed(value.int("Indexed").also { if (it !in 0..255) malformed() })
                "Rgb" -> {
                    val rgb = value.map("Rgb")
                    rgb.exact("r", "g", "b")
                    TerminalColor.Rgb(
                        rgb.int("r").component(),
                        rgb.int("g").component(),
                        rgb.int("b").component(),
                    )
                }
                else -> malformed()
            }
        }
        else -> malformed()
    }

    private fun Int.component(): Int = also { if (it !in 0..255) malformed() }

    private fun attributes(value: CborValue.MapValue): CellAttributes {
        value.exact("bold", "faint", "italic", "underline", "inverse", "hidden", "strikethrough")
        return CellAttributes(
            bold = value.boolean("bold"),
            faint = value.boolean("faint"),
            italic = value.boolean("italic"),
            underline = value.boolean("underline"),
            inverse = value.boolean("inverse"),
            hidden = value.boolean("hidden"),
            strikethrough = value.boolean("strikethrough"),
        )
    }

    private fun cursor(value: CborValue.MapValue): CursorState {
        value.exact("col", "row", "visible", "shape")
        val shape = when (value.text("shape")) {
            "Block" -> CursorShape.Block
            "Beam" -> CursorShape.Beam
            "Underline" -> CursorShape.Underline
            else -> malformed()
        }
        return CursorState(value.int("col"), value.int("row"), value.boolean("visible"), shape)
    }

    private fun modes(value: CborValue.MapValue): TerminalModes {
        value.exact("application_cursor", "bracketed_paste", "line_wrap", "alternate_screen")
        return TerminalModes(
            applicationCursor = value.boolean("application_cursor"),
            bracketedPaste = value.boolean("bracketed_paste"),
            lineWrap = value.boolean("line_wrap"),
            alternateScreen = value.boolean("alternate_screen"),
        )
    }

    private fun malformed(): Nothing = throw RemoteProtocolException("malformed terminal frame")
}

private sealed interface CborValue {
    data class Unsigned(val value: ULong) : CborValue
    data class Bytes(val value: ByteArray) : CborValue
    data class Text(val value: String) : CborValue
    data class ArrayValue(val values: List<CborValue>) : CborValue
    data class MapValue(val values: Map<String, CborValue>) : CborValue
    data class Bool(val value: Boolean) : CborValue
    data object Null : CborValue
}

private class CborValueReader(private val bytes: ByteArray) {
    private var position = 0
    private var items = 0

    fun read(): CborValue {
        val value = value(0)
        if (position != bytes.size) malformed()
        return value
    }

    private fun value(depth: Int): CborValue {
        if (depth > 32 || ++items > 300_000 || position >= bytes.size) malformed()
        val first = bytes[position++].toInt() and 0xff
        val major = first ushr 5
        val additional = first and 0x1f
        if (major == 7) {
            return when (additional) {
                20 -> CborValue.Bool(false)
                21 -> CborValue.Bool(true)
                22 -> CborValue.Null
                else -> malformed()
            }
        }
        val length = length(additional)
        return when (major) {
            0 -> CborValue.Unsigned(length)
            2 -> CborValue.Bytes(take(length))
            3 -> CborValue.Text(
                try {
                    take(length).decodeToString(throwOnInvalidSequence = true)
                } catch (_: Exception) {
                    malformed()
                },
            )
            4 -> CborValue.ArrayValue(List(collectionCount(length)) { value(depth + 1) })
            5 -> {
                val result = LinkedHashMap<String, CborValue>()
                repeat(collectionCount(length)) {
                    val key = value(depth + 1) as? CborValue.Text ?: malformed()
                    if (result.put(key.value, value(depth + 1)) != null) malformed()
                }
                CborValue.MapValue(result)
            }
            else -> malformed()
        }
    }

    private fun length(additional: Int): ULong = when (additional) {
        in 0..23 -> additional.toULong()
        24 -> unsigned(1).also { if (it < 24u) malformed() }
        25 -> unsigned(2).also { if (it <= UByte.MAX_VALUE.toULong()) malformed() }
        26 -> unsigned(4).also { if (it <= UShort.MAX_VALUE.toULong()) malformed() }
        27 -> unsigned(8).also { if (it <= UInt.MAX_VALUE.toULong()) malformed() }
        else -> malformed()
    }

    private fun unsigned(count: Int): ULong {
        if (position + count > bytes.size) malformed()
        var value = 0uL
        repeat(count) { value = (value shl 8) or (bytes[position++].toInt() and 0xff).toULong() }
        return value
    }

    private fun collectionCount(value: ULong): Int {
        if (value > 300_000u) malformed()
        return value.toInt()
    }

    private fun take(length: ULong): ByteArray {
        if (length > (bytes.size - position).toULong()) malformed()
        val count = length.toInt()
        return bytes.copyOfRange(position, position + count).also { position += count }
    }

    private fun malformed(): Nothing = throw RemoteProtocolException("malformed terminal frame")
}

private fun CborValue.MapValue.exact(vararg names: String) {
    if (values.keys != names.toSet()) malformedMap()
}

private fun CborValue.MapValue.required(name: String): CborValue = values[name] ?: malformedMap()
private fun CborValue.MapValue.text(name: String): String =
    (required(name) as? CborValue.Text)?.value ?: malformedMap()
private fun CborValue.MapValue.optionalText(name: String): String? = when (val value = required(name)) {
    CborValue.Null -> null
    is CborValue.Text -> value.value
    else -> malformedMap()
}
private fun CborValue.MapValue.unsigned(name: String): Long {
    val value = (required(name) as? CborValue.Unsigned)?.value ?: malformedMap()
    if (value > Long.MAX_VALUE.toULong()) malformedMap()
    return value.toLong()
}
private fun CborValue.MapValue.int(name: String): Int {
    val value = unsigned(name)
    if (value > Int.MAX_VALUE) malformedMap()
    return value.toInt()
}
private fun CborValue.MapValue.boolean(name: String): Boolean =
    (required(name) as? CborValue.Bool)?.value ?: malformedMap()
private fun CborValue.MapValue.bytes(name: String): ByteArray =
    (required(name) as? CborValue.Bytes)?.value ?: malformedMap()
private fun CborValue.MapValue.array(name: String): List<CborValue> =
    (required(name) as? CborValue.ArrayValue)?.values ?: malformedMap()
private fun CborValue.MapValue.map(name: String): CborValue.MapValue =
    required(name) as? CborValue.MapValue ?: malformedMap()
private fun CborValue.MapValue.optionalMap(name: String): CborValue.MapValue? = when (val value = required(name)) {
    CborValue.Null -> null
    is CborValue.MapValue -> value
    else -> malformedMap()
}
private fun malformedMap(): Nothing = throw RemoteProtocolException("malformed terminal frame")
