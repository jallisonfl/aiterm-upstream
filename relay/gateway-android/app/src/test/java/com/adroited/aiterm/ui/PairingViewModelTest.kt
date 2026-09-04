package com.adroited.aiterm.ui

import com.adroited.aiterm.pairing.EnrollmentOutcome
import com.adroited.aiterm.pairing.EnrollmentSecret
import com.adroited.aiterm.pairing.FakeDeviceKeys
import com.adroited.aiterm.pairing.FakePairedDesktopStore
import com.adroited.aiterm.pairing.PairingEndpoint
import com.adroited.aiterm.pairing.PairingFailure
import com.adroited.aiterm.pairing.PairingRepository
import com.adroited.aiterm.pairing.PairingTransport
import com.adroited.aiterm.pairing.pairingUri
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class PairingViewModelTest {

    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUpMainDispatcher() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun resetMainDispatcher() {
        Dispatchers.resetMain()
    }

    @Test
    fun malformedQr_stopsBeforeAnyKeyOrTransportUse() {
        val keys = FakeDeviceKeys()
        val transport = WaitingTransport()
        val viewModel = viewModel(transport, keys, FakePairedDesktopStore())

        viewModel.onQrCode("https://not-aiterm.example/pair")

        assertEquals(PairingUiState.Failed(PairingFailure.MALFORMED_PAYLOAD), viewModel.state.value)
        assertEquals(0, keys.publicKeyRequests)
        assertEquals(0, transport.attempts)
    }

    @Test
    fun approvedResponse_isTheFirstPointThatUpdatesTheStoredDesktop() = runTest(dispatcher) {
        val gate = CompletableDeferred<Unit>()
        val store = FakePairedDesktopStore()
        val transport = WaitingTransport(gate)
        val viewModel = viewModel(transport, FakeDeviceKeys(), store)

        viewModel.onQrCode(pairingUri(name = "Workshop%20PC"))
        assertTrue(viewModel.state.value is PairingUiState.Confirming)
        viewModel.confirm()
        runCurrent()

        assertEquals(PairingUiState.AwaitingApproval("Workshop PC"), viewModel.state.value)
        assertEquals(emptyList<Any>(), store.all())

        gate.complete(Unit)
        advanceUntilIdle()

        assertEquals(PairingUiState.Paired("Workshop PC"), viewModel.state.value)
        assertEquals("device-approved", store.all().single().deviceId)
    }

    private fun viewModel(
        transport: PairingTransport,
        keys: FakeDeviceKeys,
        store: FakePairedDesktopStore,
    ) = PairingViewModel(
        repository = PairingRepository(transport, keys, store),
        clock = { 1_700_000_000_000L },
        deviceName = { "Pixel test" },
    )

    private class WaitingTransport(
        private val approvalGate: CompletableDeferred<Unit> = CompletableDeferred(),
    ) : PairingTransport {
        var attempts = 0
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
            val consumption = enrollmentSecret.consume { Unit }
            if (consumption is EnrollmentSecret.Consumption.AlreadyConsumed) {
                return EnrollmentOutcome.ConsumedPayload
            }
            onPending()
            approvalGate.await()
            return EnrollmentOutcome.Approved("device-approved")
        }
    }
}
