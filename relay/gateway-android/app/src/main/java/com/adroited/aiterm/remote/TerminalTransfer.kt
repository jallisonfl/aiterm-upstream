package com.adroited.aiterm.remote

import com.adroited.aiterm.terminal.CursorState
import com.adroited.aiterm.terminal.RowPatch
import com.adroited.aiterm.terminal.ScreenDiff
import com.adroited.aiterm.terminal.ScreenRow
import com.adroited.aiterm.terminal.ScreenSnapshot
import com.adroited.aiterm.terminal.TerminalModes
import com.adroited.aiterm.terminal.TerminalColor
import com.adroited.aiterm.terminal.CellAttributes

enum class TerminalTransferKind { Snapshot, Diff, Scrollback }

sealed interface TerminalTransferPart {
    val transferRows: List<ScreenRow>

    data class Snapshot(
        val cols: Int,
        val rows: Int,
        val visible: List<ScreenRow>,
        val cursor: CursorState,
        val modes: TerminalModes,
    ) : TerminalTransferPart {
        override val transferRows: List<ScreenRow> get() = visible
    }

    data class Diff(
        val patches: List<RowPatch>,
        val cursor: CursorState?,
        val modes: TerminalModes?,
    ) : TerminalTransferPart {
        override val transferRows: List<ScreenRow> get() = patches.map(RowPatch::content)
    }

    data class Scrollback(
        val rows: List<ScreenRow>,
    ) : TerminalTransferPart {
        override val transferRows: List<ScreenRow> get() = rows
    }
}

data class TerminalTransferChunk(
    val transferId: String,
    val tabId: String,
    val attachmentId: String?,
    val kind: TerminalTransferKind,
    val baseRevision: Long,
    val finalRevision: Long,
    val rowStart: Int,
    val rowEnd: Int,
    val index: Int,
    val total: Int,
    val requestId: Long,
    val part: TerminalTransferPart,
)

sealed interface TerminalTransferResult {
    data object Pending : TerminalTransferResult
    data object Recover : TerminalTransferResult
    data class Snapshot(val snapshot: ScreenSnapshot, val attachmentId: String?) : TerminalTransferResult
    data class Diff(val diff: ScreenDiff, val attachmentId: String?) : TerminalTransferResult
    data class Scrollback(
        val tabId: String,
        val revision: Long,
        val rows: List<ScreenRow>,
        val attachmentId: String?,
    ) : TerminalTransferResult
}

/**
 * Buffers exactly one semantic terminal transfer for the selected tab. A
 * transfer is published only after every descriptor-bound row range arrives
 * in order with identical semantic metadata.
 */
class TerminalTransferAssembler(
    private val maxViewportRows: Int = 512,
    private val maxScrollbackRows: Int = 10_000,
    private val maxDecodedBytes: Long = 8L * 1_024 * 1_024,
) {
    private var header: Header? = null
    private var nextIndex = 0
    private var nextRow = 0
    private var snapshotMetadata: TerminalTransferPart.Snapshot? = null
    private var diffMetadata: TerminalTransferPart.Diff? = null
    private val rows = mutableListOf<ScreenRow>()
    private val patches = mutableListOf<RowPatch>()
    private var decodedBytes = 0L

    val activeTransferId: String? get() = header?.transferId
    val pendingCount: Int get() = if (header == null) 0 else 1

    @Synchronized
    fun accept(chunk: TerminalTransferChunk): TerminalTransferResult {
        return try {
            validateChunk(chunk)
            val expected = header
            if (expected == null) {
                if (chunk.index != 0 || chunk.rowStart != 0) invalid()
                header = Header.from(chunk)
                nextIndex = 0
                nextRow = 0
            } else if (!expected.matches(chunk)) {
                invalid()
            }
            if (chunk.index != nextIndex || chunk.rowStart != nextRow) invalid()
            if (chunk.rowEnd - chunk.rowStart != chunk.part.transferRows.size) invalid()
            accountDecodedRows(chunk.part.transferRows)

            when (val part = chunk.part) {
                is TerminalTransferPart.Snapshot -> acceptSnapshotPart(part)
                is TerminalTransferPart.Diff -> acceptDiffPart(part)
                is TerminalTransferPart.Scrollback -> {
                    if (rows.size + part.rows.size > maxScrollbackRows) invalid()
                    rows += part.rows
                }
            }
            nextIndex++
            nextRow = chunk.rowEnd
            if (nextIndex != chunk.total) return TerminalTransferResult.Pending
            complete(chunk)
        } catch (_: RemoteProtocolException) {
            clear()
            TerminalTransferResult.Recover
        }
    }

    @Synchronized
    fun clear() {
        header = null
        nextIndex = 0
        nextRow = 0
        snapshotMetadata = null
        diffMetadata = null
        rows.clear()
        patches.clear()
        decodedBytes = 0L
    }

    private fun acceptSnapshotPart(part: TerminalTransferPart.Snapshot) {
        val metadata = snapshotMetadata
        if (metadata == null) {
            snapshotMetadata = part.copy(visible = emptyList())
        } else if (metadata.cols != part.cols || metadata.rows != part.rows ||
            metadata.cursor != part.cursor || metadata.modes != part.modes
        ) {
            invalid()
        }
        rows += part.visible
        if (rows.size > maxViewportRows) invalid()
    }

    private fun acceptDiffPart(part: TerminalTransferPart.Diff) {
        val metadata = diffMetadata
        if (metadata == null) {
            diffMetadata = part.copy(patches = emptyList())
        } else if (metadata.cursor != part.cursor || metadata.modes != part.modes) {
            invalid()
        }
        patches += part.patches
        if (patches.size > maxViewportRows) invalid()
    }

    private fun complete(chunk: TerminalTransferChunk): TerminalTransferResult {
        val result = when (chunk.kind) {
            TerminalTransferKind.Snapshot -> {
                val metadata = snapshotMetadata ?: invalid()
                if (rows.size != metadata.rows || chunk.rowEnd != metadata.rows) invalid()
                TerminalTransferResult.Snapshot(
                    ScreenSnapshot(
                        tabId = chunk.tabId,
                        revision = chunk.finalRevision,
                        cols = metadata.cols,
                        rows = metadata.rows,
                        visible = rows.toList(),
                        cursor = metadata.cursor,
                        modes = metadata.modes,
                    ),
                    chunk.attachmentId,
                )
            }
            TerminalTransferKind.Diff -> TerminalTransferResult.Diff(
                ScreenDiff(
                    tabId = chunk.tabId,
                    baseRevision = chunk.baseRevision,
                    revision = chunk.finalRevision,
                    rows = patches.toList(),
                    cursor = diffMetadata?.cursor,
                    modes = diffMetadata?.modes,
                ),
                chunk.attachmentId,
            )
            TerminalTransferKind.Scrollback -> {
                TerminalTransferResult.Scrollback(
                    chunk.tabId,
                    chunk.finalRevision,
                    rows.toList(),
                    chunk.attachmentId,
                )
            }
        }
        clear()
        return result
    }

    private fun validateChunk(chunk: TerminalTransferChunk) {
        if (chunk.transferId.length !in 1..64 || chunk.tabId.length !in 1..128 ||
            chunk.attachmentId?.length !in 1..128 && chunk.attachmentId != null ||
            chunk.baseRevision < 0 || chunk.finalRevision < chunk.baseRevision ||
            chunk.requestId < 0 || chunk.index !in 0 until chunk.total ||
            chunk.total !in 1..512 || chunk.rowStart < 0 || chunk.rowEnd < chunk.rowStart
        ) invalid()
        if (chunk.kind == TerminalTransferKind.Snapshot && chunk.baseRevision != chunk.finalRevision) {
            invalid()
        }
        if (chunk.kind == TerminalTransferKind.Diff && chunk.finalRevision <= chunk.baseRevision) {
            invalid()
        }
        when (val part = chunk.part) {
            is TerminalTransferPart.Snapshot -> {
                if (chunk.kind != TerminalTransferKind.Snapshot || part.cols !in 1..512 ||
                    part.rows !in 1..512 || part.cursor.col !in 0 until part.cols ||
                    part.cursor.row !in 0 until part.rows
                ) invalid()
            }
            is TerminalTransferPart.Diff -> if (chunk.kind != TerminalTransferKind.Diff) invalid()
            is TerminalTransferPart.Scrollback -> if (chunk.kind != TerminalTransferKind.Scrollback) invalid()
        }
        chunk.part.transferRows.forEach(::validateRow)
    }

    private fun validateRow(row: ScreenRow) {
        if (row.cells.size > 512) invalid()
        var index = 0
        while (index < row.cells.size) {
            val cell = row.cells[index]
            if (cell.continuation || cell.width !in 1..2 || cell.text.isEmpty() ||
                cell.text.encodeToByteArray().size > 132
            ) invalid()
            val codePoints = cell.text.codePoints().toArray()
            if (codePoints.size !in 1..33 || isCombining(codePoints.first()) ||
                codePoints.drop(1).any { !isCombining(it) }
            ) invalid()
            if (cell.width == 2) {
                val continuation = row.cells.getOrNull(index + 1) ?: invalid()
                if (!continuation.continuation || continuation.width != 0 || continuation.text.isNotEmpty() ||
                    continuation.foreground != TerminalColor.Default ||
                    continuation.background != TerminalColor.Default ||
                    continuation.attributes != CellAttributes()
                ) {
                    invalid()
                }
                index += 2
            } else {
                index++
            }
        }
    }

    private fun accountDecodedRows(incoming: List<ScreenRow>) {
        for (row in incoming) {
            decodedBytes += 16
            for (cell in row.cells) {
                decodedBytes += 32L + cell.text.encodeToByteArray().size
                if (decodedBytes > maxDecodedBytes) invalid()
            }
        }
    }

    private fun invalid(): Nothing = throw RemoteProtocolException("invalid terminal transfer")

    private fun isCombining(codePoint: Int): Boolean = when (Character.getType(codePoint)) {
        Character.NON_SPACING_MARK.toInt(),
        Character.COMBINING_SPACING_MARK.toInt(),
        Character.ENCLOSING_MARK.toInt() -> true
        else -> false
    }

    private data class Header(
        val transferId: String,
        val tabId: String,
        val attachmentId: String?,
        val kind: TerminalTransferKind,
        val baseRevision: Long,
        val finalRevision: Long,
        val total: Int,
        val requestId: Long,
    ) {
        fun matches(chunk: TerminalTransferChunk): Boolean =
            transferId == chunk.transferId && tabId == chunk.tabId &&
                attachmentId == chunk.attachmentId && kind == chunk.kind &&
                baseRevision == chunk.baseRevision && finalRevision == chunk.finalRevision &&
                total == chunk.total && requestId == chunk.requestId

        companion object {
            fun from(chunk: TerminalTransferChunk) = Header(
                chunk.transferId,
                chunk.tabId,
                chunk.attachmentId,
                chunk.kind,
                chunk.baseRevision,
                chunk.finalRevision,
                chunk.total,
                chunk.requestId,
            )
        }
    }
}
