package com.adroited.aiterm.remote

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Test

class RosterTransferAssemblerTest {

    @Test
    fun rosterIsPublishedOnlyAfterEveryOrderedDescriptorChunkArrives() {
        val assembler = RosterTransferAssembler()
        val first = assembler.accept(chunk(index = 0, total = 2, tabId = "tab-1"))
        assertNull(first)

        val complete = assembler.accept(chunk(index = 1, total = 2, tabId = "tab-2"))

        assertEquals(listOf("tab-1", "tab-2"), complete?.tabs?.map(RemoteTab::id))
        assertEquals(9L, complete?.revision)
    }

    @Test
    fun outOfOrderOrOverBoundRosterIsRejectedAndDiscarded() {
        val assembler = RosterTransferAssembler(maxTabs = 1)
        assertThrows(RemoteProtocolException::class.java) {
            assembler.accept(chunk(index = 1, total = 2, tabId = "tab-2"))
        }
        assembler.accept(chunk(index = 0, total = 2, tabId = "tab-1"))
        assertThrows(RemoteProtocolException::class.java) {
            assembler.accept(chunk(index = 1, total = 2, tabId = "tab-2"))
        }
        assertEquals(0, assembler.pendingCount)
    }

    private fun chunk(index: Int, total: Int, tabId: String) = StateSnapshotChunk(
        transferId = "11111111-1111-4111-8111-111111111111",
        revision = 9,
        index = index,
        total = total,
        tabs = listOf(RemoteTab(id = tabId, title = tabId, size = TerminalSize(80, 24))),
    )
}
