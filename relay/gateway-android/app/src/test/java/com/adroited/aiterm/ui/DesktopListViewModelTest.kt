package com.adroited.aiterm.ui

import com.adroited.aiterm.pairing.PairedDesktop
import com.adroited.aiterm.pairing.PairedDesktopStore
import com.adroited.aiterm.pairing.PairedDesktopStoreException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class DesktopListViewModelTest {

    @Test
    fun forgetRemovesTheDesktopFromThePublishedList() {
        val store = MemoryDesktopStore(listOf(desktop()))
        val viewModel = DesktopListViewModel(store)

        viewModel.forget("desktop-1")

        assertEquals(emptyList<PairedDesktop>(), viewModel.uiState.value.desktops)
        assertEquals(emptyList<PairedDesktop>(), store.all())
    }

    @Test
    fun failedForgetPreservesThePublishedDesktopAndReportsStorageFailure() {
        val storedDesktop = desktop()
        val store = object : PairedDesktopStore {
            override fun all(): List<PairedDesktop> = listOf(storedDesktop)

            override fun save(desktop: PairedDesktop) = Unit

            override fun remove(deviceId: String) {
                throw PairedDesktopStoreException("Could not remove desktop")
            }
        }
        val viewModel = DesktopListViewModel(store)

        viewModel.forget(storedDesktop.deviceId)

        assertEquals(listOf(storedDesktop), viewModel.uiState.value.desktops)
        assertTrue(viewModel.uiState.value.storageFailure)
    }

    private fun desktop() = PairedDesktop(
        deviceId = "desktop-1",
        displayName = "Workshop PC",
        hosts = listOf("10.0.0.151"),
        port = 8443,
        serverSpkiFingerprint = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        lastSeenEpochMillis = null,
    )

    private class MemoryDesktopStore(seed: List<PairedDesktop>) : PairedDesktopStore {
        private val desktops = seed.toMutableList()

        override fun all(): List<PairedDesktop> = desktops.toList()

        override fun save(desktop: PairedDesktop) {
            desktops.removeAll { it.deviceId == desktop.deviceId }
            desktops += desktop
        }

        override fun remove(deviceId: String) {
            desktops.removeAll { it.deviceId == deviceId }
        }
    }
}
