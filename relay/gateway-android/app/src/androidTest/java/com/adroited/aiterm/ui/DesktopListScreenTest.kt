package com.adroited.aiterm.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.adroited.aiterm.pairing.PairedDesktop
import com.adroited.aiterm.pairing.PairedDesktopStore
import com.adroited.aiterm.testing.ComposeTestActivity
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class DesktopListScreenTest {
    @get:Rule val compose = createAndroidComposeRule<ComposeTestActivity>()

    @Test
    fun populatedListStillOffersPairingAnotherDesktop() {
        val store = MemoryDesktopStore(listOf(desktop()))
        val viewModel = DesktopListViewModel(store)
        var pairRequests = 0
        compose.setContent {
            DesktopListScreen(
                store = store,
                onPairDesktop = { pairRequests++ },
                viewModel = viewModel,
            )
        }

        compose.onNodeWithText("Pair a desktop").assertIsDisplayed().performClick()

        compose.runOnIdle { assertEquals(1, pairRequests) }
    }

    @Test
    fun forgettingRequiresConfirmationBeforeTheStoredIdentityIsRemoved() {
        val store = MemoryDesktopStore(listOf(desktop()))
        val viewModel = DesktopListViewModel(store)
        compose.setContent {
            DesktopListScreen(
                store = store,
                onPairDesktop = {},
                viewModel = viewModel,
            )
        }

        compose.onNodeWithText("Forget").performClick()
        compose.onNodeWithText("Forget Workshop PC?").assertIsDisplayed()
        assertEquals(1, store.all().size)

        compose.onNodeWithText("Forget desktop").performClick()

        compose.onNodeWithText("No desktops paired").assertIsDisplayed()
        assertEquals(emptyList<PairedDesktop>(), store.all())
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
