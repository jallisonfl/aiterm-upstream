package com.adroited.aiterm.remote

import com.adroited.aiterm.terminal.CursorState
import com.adroited.aiterm.terminal.ScreenCell
import com.adroited.aiterm.terminal.ScreenRow
import com.adroited.aiterm.terminal.TerminalModes
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class TerminalTransferAssemblerTest {

    @Test
    fun snapshotRowsAreInvisibleUntilTheOrderedTransferCompletes() {
        val assembler = TerminalTransferAssembler()

        val first = assembler.accept(snapshotChunk(index = 0, row = 0, text = "one"))
        assertTrue(first is TerminalTransferResult.Pending)

        val complete = assembler.accept(snapshotChunk(index = 1, row = 1, text = "two"))
        val snapshot = (complete as TerminalTransferResult.Snapshot).snapshot
        assertEquals(11L, snapshot.revision)
        assertEquals(listOf("one", "two"), snapshot.visible.map(ScreenRow::plainText))
        assertEquals(0, assembler.pendingCount)
    }

    @Test
    fun gapOrChangedSemanticMetadataDiscardsTheWholeTransfer() {
        val assembler = TerminalTransferAssembler()
        assembler.accept(snapshotChunk(index = 0, row = 0, text = "one"))

        val result = assembler.accept(
            snapshotChunk(index = 1, row = 1, text = "two").copy(finalRevision = 12),
        )

        assertTrue(result is TerminalTransferResult.Recover)
        assertEquals(0, assembler.pendingCount)
    }

    @Test
    fun anIncompleteTransferCanBeExplicitlyDiscardedOnDisconnect() {
        val assembler = TerminalTransferAssembler()
        assembler.accept(snapshotChunk(index = 0, row = 0, text = "one"))
        assembler.clear()

        assertNull(assembler.activeTransferId)
        assertEquals(0, assembler.pendingCount)
    }

    @Test
    fun aCellCannotSmuggleMultipleBaseScalarsIntoOneGridColumn() {
        val assembler = TerminalTransferAssembler()
        val invalid = snapshotChunk(index = 0, row = 0, text = "one").copy(
            part = TerminalTransferPart.Snapshot(
                cols = 10,
                rows = 2,
                visible = listOf(ScreenRow(listOf(ScreenCell("ab")))),
                cursor = CursorState(0, 0, true),
                modes = TerminalModes(),
            ),
        )

        assertTrue(assembler.accept(invalid) is TerminalTransferResult.Recover)
    }

    @Test
    fun scrollbackRowBudgetIsEnforcedBeforeAnIncompleteTransferCanAccumulate() {
        val assembler = TerminalTransferAssembler(maxScrollbackRows = 1)
        val oversizedFirstChunk = TerminalTransferChunk(
            transferId = "history",
            tabId = "tab-1",
            attachmentId = "attachment-1",
            kind = TerminalTransferKind.Scrollback,
            baseRevision = 11,
            finalRevision = 11,
            rowStart = 0,
            rowEnd = 2,
            index = 0,
            total = 2,
            requestId = 4,
            part = TerminalTransferPart.Scrollback(
                listOf(
                    ScreenRow(listOf(ScreenCell("a"))),
                    ScreenRow(listOf(ScreenCell("b"))),
                ),
            ),
        )

        assertTrue(assembler.accept(oversizedFirstChunk) is TerminalTransferResult.Recover)
        assertEquals(0, assembler.pendingCount)
    }

    private fun snapshotChunk(index: Int, row: Int, text: String) = TerminalTransferChunk(
        transferId = "11111111-1111-4111-8111-111111111111",
        tabId = "tab-1",
        attachmentId = "attachment-1",
        kind = TerminalTransferKind.Snapshot,
        baseRevision = 11,
        finalRevision = 11,
        rowStart = row,
        rowEnd = row + 1,
        index = index,
        total = 2,
        requestId = 4,
        part = TerminalTransferPart.Snapshot(
            cols = 10,
            rows = 2,
            visible = listOf(ScreenRow(text.map { ScreenCell(it.toString()) })),
            cursor = CursorState(0, 0, true),
            modes = TerminalModes(lineWrap = true),
        ),
    )
}
