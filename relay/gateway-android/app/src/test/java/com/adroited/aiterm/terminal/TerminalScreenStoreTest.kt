package com.adroited.aiterm.terminal

import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Test

class TerminalScreenStoreTest {

    @Test
    fun revisionMismatchRequestsSnapshotWithoutMutatingTheCurrentScreen() {
        val store = DefaultTerminalScreenStore()
        val snapshot = snapshot(revision = 7, text = "ready")
        store.replace(snapshot)

        val result = store.apply(
            ScreenDiff(
                tabId = "tab-1",
                baseRevision = 6,
                revision = 8,
                rows = listOf(RowPatch(0, row("wrong"))),
            ),
        )

        assertEquals(ApplyResult.NeedsSnapshot, result)
        assertSame(snapshot, store.screen.value)
    }

    @Test
    fun matchingDiffAppliesRowsCursorAndRevisionAtomically() {
        val store = DefaultTerminalScreenStore()
        store.replace(snapshot(revision = 7, text = "ready"))

        val result = store.apply(
            ScreenDiff(
                tabId = "tab-1",
                baseRevision = 7,
                revision = 8,
                rows = listOf(RowPatch(0, row("done"))),
                cursor = CursorState(col = 4, row = 0, visible = true),
            ),
        )

        assertEquals(ApplyResult.Applied, result)
        assertEquals(8L, store.screen.value?.revision)
        assertEquals("done", store.screen.value?.visible?.single()?.plainText())
        assertEquals(4, store.screen.value?.cursor?.col)
    }

    private fun snapshot(revision: Long, text: String) = ScreenSnapshot(
        tabId = "tab-1",
        revision = revision,
        cols = 8,
        rows = 1,
        visible = listOf(row(text)),
        cursor = CursorState(col = 0, row = 0, visible = true),
    )

    private fun row(text: String) = ScreenRow(
        cells = text.map { ScreenCell(text = it.toString()) },
    )
}
