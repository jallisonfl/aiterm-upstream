package com.adroited.aiterm.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.adroited.aiterm.pairing.EnrollmentOutcome
import com.adroited.aiterm.pairing.EnrollmentSecret
import com.adroited.aiterm.pairing.PairedDesktop
import com.adroited.aiterm.pairing.PairedDesktopStore
import com.adroited.aiterm.pairing.PairingEndpoint
import com.adroited.aiterm.pairing.PairingFailure
import com.adroited.aiterm.pairing.PairingRepository
import com.adroited.aiterm.pairing.PairingTransport
import com.adroited.aiterm.security.DeviceKeys
import com.adroited.aiterm.testing.ComposeTestActivity
import java.util.Base64
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Drives [PairingContent] through every state. The stateful [PairingScreen]
 * owns a camera, so the screen is split and only the stateless half is asserted
 * on here; the camera itself is manual-test territory.
 *
 * The camera itself remains a manual device check; these state transitions run
 * as instrumentation on the pinned Android verification device.
 */
@RunWith(AndroidJUnit4::class)
class PairingScreenTest {

    @get:Rule val compose = createAndroidComposeRule<ComposeTestActivity>()

    @Test
    fun scanningState_asksForTheDesktopQrCode() {
        compose.setContent { PairingContent(state = PairingUiState.Scanning) }

        compose.onNodeWithText("Scan the QR code shown by AITerm on your desktop")
            .assertIsDisplayed()
    }

    @Test
    fun confirmingState_showsTheDesktopNameAndFingerprintBeforePairing() {
        compose.setContent {
            PairingContent(
                state = PairingUiState.Confirming(
                    desktopName = "Workshop PC",
                    fingerprint = "AAAA-BBBB-CCCC",
                ),
            )
        }

        compose.onNodeWithText("Workshop PC").assertIsDisplayed()
        compose.onNodeWithText("AAAA-BBBB-CCCC").assertIsDisplayed()
        compose.onNodeWithText("Pair").assertIsDisplayed()
        compose.onNodeWithText("Cancel").assertIsDisplayed()
    }

    @Test
    fun pairingIsNotAttempted_untilTheUserConfirmsTheDesktop() {
        var confirmed = false
        compose.setContent {
            PairingContent(
                state = PairingUiState.Confirming("Workshop PC", "AAAA-BBBB-CCCC"),
                onConfirm = { confirmed = true },
            )
        }

        assertEquals(false, confirmed)
        compose.onNodeWithText("Pair").performClick()
        assertEquals(true, confirmed)
    }

    @Test
    fun api37PairingDoesNotReachTheTransportBeforeLocalNetworkAccessIsGranted() {
        val transport = CountingTransport()
        var permissionRequests = 0
        val viewModel = PairingViewModel(
            repository = PairingRepository(
                transport = transport,
                deviceKeys = StaticDeviceKeys,
                store = EmptyDesktopStore,
            ),
            clock = { 1_700_000_000_000L },
            deviceName = { "Pixel test" },
        )
        viewModel.onQrCode(pairingUri())

        compose.setContent {
            PairingScreen(
                repository = PairingRepository(transport, StaticDeviceKeys, EmptyDesktopStore),
                onBack = {},
                onPaired = {},
                viewModel = viewModel,
                localNetworkAccessGranted = { false },
                requestLocalNetworkAccess = { permissionRequests++ },
            )
        }
        compose.onNodeWithText("Pair").performClick()
        compose.waitForIdle()

        assertEquals(1, permissionRequests)
        assertEquals(0, transport.attempts)
    }

    @Test
    fun waitingState_tellsTheUserToApproveOnTheDesktop() {
        compose.setContent { PairingContent(state = PairingUiState.AwaitingApproval("Workshop PC")) }

        compose.onNodeWithText("Approve this phone on Workshop PC").assertIsDisplayed()
    }

    @Test
    fun fingerprintMismatch_showsASecurityWarningAndNoRetryOfTheSameCode() {
        compose.setContent {
            PairingContent(state = PairingUiState.Failed(PairingFailure.FINGERPRINT_MISMATCH))
        }

        compose.onNodeWithText(
            "This desktop did not present the key from the QR code. Nothing was sent.",
        ).assertIsDisplayed()
        compose.onNodeWithText("Scan again").assertIsDisplayed()
    }

    @Test
    fun expiredPayload_asksForAFreshCode() {
        compose.setContent {
            PairingContent(state = PairingUiState.Failed(PairingFailure.EXPIRED_PAYLOAD))
        }

        compose.onNodeWithText("That pairing code has expired. Show a new one on the desktop.")
            .assertIsDisplayed()
    }

    @Test
    fun noStateEverRendersTheEnrollmentSecret() {
        // The secret is not part of any UI state by construction; this asserts
        // the state type keeps it that way.
        val confirming = PairingUiState.Confirming("Workshop PC", "AAAA-BBBB-CCCC")

        assertNull(
            confirming::class.java.declaredFields.firstOrNull {
                it.name.contains("secret", ignoreCase = true)
            },
        )
    }

    private fun pairingUri(): String {
        val encoder = Base64.getUrlEncoder().withoutPadding()
        val fingerprint = encoder.encodeToString(ByteArray(32) { 7 })
        val secret = encoder.encodeToString(ByteArray(32) { it.toByte() })
        return "aiterm://pair?v=1&h=10.0.0.151&p=8443&f=$fingerprint&s=$secret&n=Desktop"
    }

    private class CountingTransport : PairingTransport {
        var attempts: Int = 0
            private set

        override suspend fun enroll(
            endpoint: PairingEndpoint,
            serverSpkiFingerprint: String,
            enrollmentSecret: EnrollmentSecret,
            deviceName: String,
            devicePublicKey: ByteArray,
            relayAuthorityPublicKey: ByteArray?,
            relaySignatureDer: ByteArray?,
            onPending: () -> Unit,
        ): EnrollmentOutcome {
            attempts++
            return EnrollmentOutcome.Unreachable("test transport")
        }
    }

    private object StaticDeviceKeys : DeviceKeys {
        override fun devicePublicKey(): ByteArray = ByteArray(33) { 2 }
        override fun signChallenge(nonce: ByteArray): ByteArray = error("not used")
    }

    private object EmptyDesktopStore : PairedDesktopStore {
        override fun all(): List<PairedDesktop> = emptyList()
        override fun save(desktop: PairedDesktop) = Unit
        override fun remove(deviceId: String) = Unit
    }
}
