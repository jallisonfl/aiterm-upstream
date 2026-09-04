package com.adroited.aiterm.remote

import com.adroited.aiterm.pairing.AuthChallengeFrame
import com.adroited.aiterm.pairing.PairedDesktop
import com.adroited.aiterm.pairing.PairingFrames
import com.adroited.aiterm.security.DeviceKeys
import com.adroited.aiterm.security.AppLock
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.take
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.ByteArrayOutputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.thread

@OptIn(ExperimentalCoroutinesApi::class)
class AuthenticatedRemoteTransportTest {

    @Test
    fun aLockedAppNeverSignsTheOpeningChallenge() = runTest {
        val socket = FakeBinarySocket().apply {
            incoming.trySend(PairingFrames.encode(AuthChallengeFrame(ByteArray(32) { 7 })))
        }
        val keys = RecordingDeviceKeys()
        val transport = AuthenticatedRemoteTransport(
            desktop = desktop(),
            deviceKeys = keys,
            appLock = unlockedAppLock().apply { lockNow() },
            dialer = FakeDialer(socket),
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )

        var rejected = false
        try {
            transport.connect()
        } catch (_: RemoteProtocolException) {
            rejected = true
        }
        assertTrue(rejected)
        assertEquals(0, keys.signCount)
        assertTrue(socket.closed)
    }

    @Test
    fun anUnlockedClientSignsOnceAndRequiresAuthOk() = runTest {
        val socket = FakeBinarySocket().apply {
            incoming.trySend(PairingFrames.encode(AuthChallengeFrame(ByteArray(32) { 3 })))
            incoming.trySend(hex("a1646b696e6467617574682e6f6b"))
        }
        val keys = RecordingDeviceKeys()
        val transport = AuthenticatedRemoteTransport(
            desktop = desktop(),
            deviceKeys = keys,
            appLock = unlockedAppLock(),
            dialer = FakeDialer(socket),
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )

        transport.connect()

        assertEquals(1, keys.signCount)
        assertEquals(1, socket.sent.size)
        assertTrue(socket.sent.single().isNotEmpty())
        transport.close()
    }

    @Test
    fun anAuthenticatedRelayConnectionUpgradesBeforePublishingTheTransport() = runTest {
        val relayEndpoint = RemoteEndpoint("desktop.relay.example", 443)
        val relay = FakeBinarySocket(relayEndpoint).apply {
            incoming.trySend(PairingFrames.encode(AuthChallengeFrame(ByteArray(32) { 3 })))
            incoming.trySend(hex("a1646b696e6467617574682e6f6b"))
            incoming.trySend(
                cborFixture(
                    linkedMapOf(
                        "version" to 1,
                        "request_id" to 1,
                        "kind" to "transport.direct",
                        "payload" to cborFixture(
                            linkedMapOf(
                                "id" to "direct-id",
                                "cookie" to "direct-cookie",
                                "host" to "rendezvous.example",
                                "port" to 443,
                                "expires_in_millis" to 30_000,
                            ),
                        ),
                    ),
                ),
            )
        }
        val direct = FakeBinarySocket(relayEndpoint).apply {
            incoming.trySend(PairingFrames.encode(AuthChallengeFrame(ByteArray(32) { 4 })))
            incoming.trySend(hex("a1646b696e6467617574682e6f6b"))
        }
        val keys = RecordingDeviceKeys()
        val dialer = FakeDirectDialer(relay, direct)
        val transport = AuthenticatedRemoteTransport(
            desktop = desktop().copy(relayHost = relayEndpoint.host, relayPort = relayEndpoint.port),
            deviceKeys = keys,
            appLock = unlockedAppLock(),
            dialer = dialer,
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )

        transport.connect()

        assertEquals(2, keys.signCount)
        assertEquals("rendezvous.example", dialer.offer?.host)
        assertEquals(2, relay.sent.size)
        assertEquals(1, direct.sent.size)
        assertTrue(relay.closed)
        assertFalse(direct.closed)
        transport.close()
    }

    @Test
    fun failedQuicUpgradeKeepsTheAuthenticatedRelayAndAdvancesItsRequestId() = runTest {
        val relayEndpoint = RemoteEndpoint("desktop.relay.example", 443)
        val relay = FakeBinarySocket(relayEndpoint).apply {
            incoming.trySend(PairingFrames.encode(AuthChallengeFrame(ByteArray(32) { 3 })))
            incoming.trySend(hex("a1646b696e6467617574682e6f6b"))
            incoming.trySend(
                cborFixture(
                    linkedMapOf(
                        "version" to 1,
                        "request_id" to 1,
                        "kind" to "transport.direct",
                        "payload" to cborFixture(
                            linkedMapOf(
                                "id" to "direct-id",
                                "cookie" to "direct-cookie",
                                "host" to "rendezvous.example",
                                "port" to 443,
                                "expires_in_millis" to 30_000,
                            ),
                        ),
                    ),
                ),
            )
        }
        val transport = AuthenticatedRemoteTransport(
            desktop = desktop().copy(relayHost = relayEndpoint.host, relayPort = relayEndpoint.port),
            deviceKeys = RecordingDeviceKeys(),
            appLock = unlockedAppLock(),
            dialer = FakeDirectDialer(relay, null),
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        transport.connect()
        val response = transport.request("tab.list", byteArrayOf())
        runCurrent()
        transport.acceptEnvelopeForTest(RemoteEventEnvelope(2, "tab.list", byteArrayOf()))

        assertEquals(2L, (response.await() as RemoteResponse.Success).requestId)
        assertFalse(relay.closed)
        assertEquals(3, relay.sent.size)
        transport.close()
    }

    @Test
    fun aSlowDirectAttemptDrainsRelayEventsBeforeFallingBack() = runTest {
        val relayEndpoint = RemoteEndpoint("desktop.relay.example", 443, RemotePath.RELAY)
        val relay = FakeBinarySocket(relayEndpoint).apply {
            incoming.trySend(PairingFrames.encode(AuthChallengeFrame(ByteArray(32) { 3 })))
            incoming.trySend(hex("a1646b696e6467617574682e6f6b"))
            incoming.trySend(
                cborFixture(
                    linkedMapOf(
                        "version" to 1,
                        "request_id" to 1,
                        "kind" to "transport.direct",
                        "payload" to cborFixture(
                            linkedMapOf(
                                "id" to "direct-id",
                                "cookie" to "direct-cookie",
                                "host" to "rendezvous.example",
                                "port" to 443,
                                "expires_in_millis" to 30_000,
                            ),
                        ),
                    ),
                ),
            )
        }
        val direct = CompletableDeferred<RemoteBinarySocket>()
        val transport = AuthenticatedRemoteTransport(
            desktop = desktop().copy(relayHost = relayEndpoint.host, relayPort = relayEndpoint.port),
            deviceKeys = RecordingDeviceKeys(),
            appLock = unlockedAppLock(),
            dialer = DeferredDirectDialer(relay, direct),
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )

        val connecting = async { transport.connect() }
        runCurrent()
        repeat(20) {
            relay.incoming.trySend(
                cborFixture(
                    linkedMapOf(
                        "version" to 1,
                        "request_id" to 0,
                        "kind" to "session.changed",
                        "payload" to byteArrayOf(),
                    ),
                ),
            )
        }
        runCurrent()
        direct.completeExceptionally(RemoteProtocolException("direct path unavailable"))
        advanceTimeBy(51)
        runCurrent()
        connecting.await()

        val events = withTimeout(1_000) { transport.events.take(20).toList() }
        assertEquals(20, events.size)
        assertEquals(relayEndpoint, transport.endpoint)
        assertFalse(relay.closed)
        transport.close()
    }

    @Test
    fun closeWhileDialingPreventsTheLateSocketFromSigningOrPublishing() = runTest {
        val socket = FakeBinarySocket().apply {
            incoming.trySend(PairingFrames.encode(AuthChallengeFrame(ByteArray(32) { 5 })))
            incoming.trySend(hex("a1646b696e6467617574682e6f6b"))
        }
        val dialer = DeferredDialer()
        val keys = RecordingDeviceKeys()
        val transport = AuthenticatedRemoteTransport(
            desktop = desktop(),
            deviceKeys = keys,
            appLock = unlockedAppLock(),
            dialer = dialer,
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )

        val connecting = async { runCatching { transport.connect() } }
        runCurrent()
        transport.close()
        dialer.socket.complete(socket)
        runCurrent()

        assertTrue(connecting.await().isFailure)
        assertEquals(0, keys.signCount)
        assertTrue(socket.closed)
    }

    @Test
    fun lockDuringKeystoreSignatureDiscardsProofAndClosesCandidate() = runTest {
        val socket = FakeBinarySocket().apply {
            incoming.trySend(PairingFrames.encode(AuthChallengeFrame(ByteArray(32) { 6 })))
            incoming.trySend(hex("a1646b696e6467617574682e6f6b"))
        }
        val appLock = unlockedAppLock()
        val keys = RecordingDeviceKeys { appLock.lockNow() }
        val transport = AuthenticatedRemoteTransport(
            desktop = desktop(),
            deviceKeys = keys,
            appLock = appLock,
            dialer = FakeDialer(socket),
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )

        val result = runCatching { transport.connect() }

        assertTrue(result.isFailure)
        assertEquals(1, keys.signCount)
        assertEquals(0, socket.sent.size)
        assertTrue(socket.closed)
    }

    @Test
    fun explicitAuthenticationDenialIsReportedAsRevocation() = runTest {
        val socket = FakeBinarySocket().apply {
            incoming.trySend(PairingFrames.encode(AuthChallengeFrame(ByteArray(32) { 8 })))
            incoming.trySend(hex("a1646b696e646b617574682e64656e696564"))
        }
        val transport = AuthenticatedRemoteTransport(
            desktop = desktop(),
            deviceKeys = RecordingDeviceKeys(),
            appLock = unlockedAppLock(),
            dialer = FakeDialer(socket),
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )

        val result = runCatching { transport.connect() }

        assertTrue(result.exceptionOrNull() is RemoteAccessRevokedException)
        assertTrue(socket.closed)
    }

    @Test
    fun responsesCompleteOnlyTheirCorrelatedPendingRequest() = runTest {
        val socket = FakeBinarySocket().apply {
            incoming.trySend(PairingFrames.encode(AuthChallengeFrame(ByteArray(32) { 3 })))
            incoming.trySend(hex("a1646b696e6467617574682e6f6b"))
        }
        val transport = AuthenticatedRemoteTransport(
            desktop = desktop(),
            deviceKeys = RecordingDeviceKeys(),
            appLock = unlockedAppLock(),
            dialer = FakeDialer(socket),
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        transport.connect()

        val response = async {
            transport.request("tab.list", byteArrayOf()).await()
        }
        runCurrent()
        socket.incoming.send(
            hex(
                "a4" +
                    "6776657273696f6e01" +
                    "6a726571756573745f696401" +
                    "646b696e64687461622e6c697374" +
                    "677061796c6f616440",
            ),
        )
        runCurrent()

        assertEquals("tab.list", (response.await() as RemoteResponse.Success).kind)
        transport.close()
    }

    @Test
    fun abandoningAQueuedRequestDropsItBeforeTheWriterConsumesIt() = runTest {
        val socket = authenticatedSocket()
        val transport = transport(socket, backgroundScope, StandardTestDispatcher(testScheduler))
        transport.connect()

        val abandoned = transport.request("tab.list", byteArrayOf(1))
        transport.abandonRequest(abandoned)
        val successor = transport.request("tab.list", byteArrayOf(2))
        runCurrent()

        assertTrue(abandoned.isCancelled)
        assertEquals(2, socket.sent.size)
        transport.acceptEnvelopeForTest(RemoteEventEnvelope(1, "tab.list", byteArrayOf()))
        assertEquals(1L, (successor.await() as RemoteResponse.Success).requestId)
        transport.close()
    }

    @Test
    fun abandoningAnAssignedRequestReleasesItsSlotAndDiscardsALateResponse() = runTest {
        val socket = authenticatedSocket()
        val transport = transport(socket, backgroundScope, StandardTestDispatcher(testScheduler))
        transport.connect()

        val accepted = (1..64).map { transport.request("tab.list", byteArrayOf()) }
        runCurrent()
        transport.abandonRequest(accepted.first())
        val successor = transport.request("tab.list", byteArrayOf())
        runCurrent()

        assertTrue(accepted.first().isCancelled)
        assertFalse(successor.isCompleted)
        transport.acceptEnvelopeForTest(RemoteEventEnvelope(1, "tab.list", byteArrayOf()))
        transport.acceptEnvelopeForTest(
            RemoteEventEnvelope(1, "error", cborFixture(linkedMapOf("code" to "cancelled", "message" to "late"))),
        )
        transport.acceptEnvelopeForTest(RemoteEventEnvelope(65, "tab.list", byteArrayOf()))
        assertEquals(65L, (successor.await() as RemoteResponse.Success).requestId)
        transport.close()
    }

    @Test
    fun requestBatchReservesCapacityForEveryRequestOrRejectsTheWholeBatch() = runTest {
        val socket = authenticatedSocket()
        val transport = transport(socket, backgroundScope, StandardTestDispatcher(testScheduler))
        transport.connect()
        val existing = (1..63).map { transport.request("tab.list", byteArrayOf(it.toByte())) }

        val rejected = transport.requestBatch(
            listOf(
                RemoteRequestInput("terminal.input", byteArrayOf(1)),
                RemoteRequestInput("terminal.input", byteArrayOf(2)),
            ),
        )

        assertEquals(null, rejected)
        runCurrent()
        assertEquals("auth plus only the 63 earlier requests", 64, socket.sent.size)
        existing.forEach { it.cancel() }
        transport.close()
    }

    @Test
    fun acceptedRequestBatchPublishesEveryRequestInCallerOrder() = runTest {
        val socket = authenticatedSocket()
        val transport = transport(socket, backgroundScope, StandardTestDispatcher(testScheduler))
        transport.connect()

        val accepted = transport.requestBatch(
            listOf(
                RemoteRequestInput("terminal.input", byteArrayOf(11)),
                RemoteRequestInput("terminal.input", byteArrayOf(22)),
            ),
        )
        runCurrent()

        assertEquals(2, accepted?.size)
        assertEquals(3, socket.sent.size)
        assertTrue(
            socket.sent[1].contentEquals(
                RemoteWireCodec.encodeRequest(RemoteRequest(1, "terminal.input", byteArrayOf(11))),
            ),
        )
        assertTrue(
            socket.sent[2].contentEquals(
                RemoteWireCodec.encodeRequest(RemoteRequest(2, "terminal.input", byteArrayOf(22))),
            ),
        )
        accepted?.forEach { it.cancel() }
        transport.close()
    }

    @Test
    fun closeRacingBatchReservationRejectsTheWholeBatch() = runTest {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val transport = AuthenticatedRemoteTransport(
            desktop = desktop(),
            deviceKeys = RecordingDeviceKeys(),
            appLock = unlockedAppLock(),
            dialer = FakeDialer(authenticatedSocket()),
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
            beforeRequestEnqueue = {
                entered.countDown()
                release.await(2, TimeUnit.SECONDS)
            },
        )
        transport.connect()
        val result = AtomicReference<List<kotlinx.coroutines.Deferred<RemoteResponse>>?>()
        val requester = thread(start = true) {
            result.set(
                transport.requestBatch(
                    listOf(
                        RemoteRequestInput("terminal.input", byteArrayOf(1)),
                        RemoteRequestInput("terminal.input", byteArrayOf(2)),
                    ),
                ),
            )
        }
        assertTrue(entered.await(2, TimeUnit.SECONDS))

        transport.close()
        release.countDown()
        requester.join(2_000)

        assertEquals(null, result.get())
    }

    @Test
    fun outboundRequestsCannotOvertakeAnEarlierBlockedSend() = runTest {
        val socket = ReorderingBinarySocket().apply {
            incoming.trySend(PairingFrames.encode(AuthChallengeFrame(ByteArray(32) { 3 })))
            incoming.trySend(hex("a1646b696e6467617574682e6f6b"))
        }
        val transport = AuthenticatedRemoteTransport(
            desktop = desktop(),
            deviceKeys = RecordingDeviceKeys(),
            appLock = unlockedAppLock(),
            dialer = FakeDialer(socket),
            scope = backgroundScope,
            dispatcher = kotlinx.coroutines.Dispatchers.Default,
        )
        transport.connect()

        val first = transport.request("tab.list", byteArrayOf())
        assertTrue(socket.firstRequestEntered.await(2, TimeUnit.SECONDS))
        val second = transport.request("agent.list", byteArrayOf())
        assertFalse(socket.secondRequestSent.await(100, TimeUnit.MILLISECONDS))
        socket.releaseFirstRequest.countDown()
        assertTrue(socket.secondRequestSent.await(2, TimeUnit.SECONDS))

        assertEquals(listOf(1, 2), socket.requestSendOrder)
        transport.close()
        first.cancel()
        second.cancel()
    }

    @Test
    fun closeRacingAnAcceptedEnqueueCompletesTheDeferredExceptionally() = runTest {
        val accepted = CountDownLatch(1)
        val releaseEnqueue = CountDownLatch(1)
        val transport = AuthenticatedRemoteTransport(
            desktop = desktop(),
            deviceKeys = RecordingDeviceKeys(),
            appLock = unlockedAppLock(),
            dialer = FakeDialer(authenticatedSocket()),
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
            beforeRequestEnqueue = {
                accepted.countDown()
                releaseEnqueue.await(2, TimeUnit.SECONDS)
            },
        )
        transport.connect()
        val response = AtomicReference<CompletableDeferred<RemoteResponse>>()
        val requester = thread(start = true) {
            response.set(transport.request("tab.list", byteArrayOf()))
        }
        assertTrue(accepted.await(2, TimeUnit.SECONDS))

        transport.close()
        releaseEnqueue.countDown()
        requester.join(2_000)

        val failure = runCatching { withTimeout(1_000) { response.get().await() } }.exceptionOrNull()
        assertTrue("accepted request must fail on close, got $failure", failure is RemoteProtocolException)
    }

    @Test
    fun requestAssignmentCallbackNeverRunsUnderTransportStateLock() = runTest {
        val clientLock = ReentrantLock()
        val closerHasClientLock = CountDownLatch(1)
        val callbackEntered = CountDownLatch(1)
        val callbackFinished = CountDownLatch(1)
        val callbackAcquiredClientLock = AtomicBoolean(false)
        val transport = AuthenticatedRemoteTransport(
            desktop = desktop(),
            deviceKeys = RecordingDeviceKeys(),
            appLock = unlockedAppLock(),
            dialer = FakeDialer(authenticatedSocket()),
            scope = backgroundScope,
            dispatcher = kotlinx.coroutines.Dispatchers.Default,
        )
        transport.connect()
        val closer = thread(start = true) {
            clientLock.lock()
            try {
                closerHasClientLock.countDown()
                callbackEntered.await(2, TimeUnit.SECONDS)
                transport.close()
            } finally {
                clientLock.unlock()
            }
        }
        assertTrue(closerHasClientLock.await(2, TimeUnit.SECONDS))

        val response = transport.request("tab.list", byteArrayOf()) {
            callbackEntered.countDown()
            val acquired = clientLock.tryLock(1, TimeUnit.SECONDS)
            callbackAcquiredClientLock.set(acquired)
            if (acquired) clientLock.unlock()
            callbackFinished.countDown()
        }

        assertTrue(callbackFinished.await(3, TimeUnit.SECONDS))
        closer.join(3_000)
        response.cancel()
        assertTrue("assignment callback was lock-inverted with close", callbackAcquiredClientLock.get())
    }

    @Test
    fun attachSnapshotWaitsForTheClientToCommitItsAttachment() = runTest {
        val socket = FakeBinarySocket().apply {
            incoming.trySend(PairingFrames.encode(AuthChallengeFrame(ByteArray(32) { 3 })))
            incoming.trySend(hex("a1646b696e6467617574682e6f6b"))
        }
        val transport = AuthenticatedRemoteTransport(
            desktop = desktop(),
            deviceKeys = RecordingDeviceKeys(),
            appLock = unlockedAppLock(),
            dialer = FakeDialer(socket),
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        transport.connect()
        val response = transport.request("terminal.attach", byteArrayOf())
        val delivered = async { transport.events.first() }
        runCurrent()

        transport.acceptEnvelopeForTest(RemoteEventEnvelope(1, "terminal.attach", byteArrayOf()))
        transport.acceptEnvelopeForTest(RemoteEventEnvelope(1, "terminal.snapshot", terminalSnapshotFixture(1)))
        runCurrent()
        response.await()
        assertFalse(delivered.isCompleted)

        transport.completeAttachment(1, true)
        runCurrent()
        assertTrue(delivered.await() is RemoteServerEvent.TerminalChunk)
        transport.close()
    }

    @Test
    fun maximumValidEventBurstBackpressuresWithoutClosingTheTransport() = runTest {
        val socket = authenticatedSocket()
        val transport = transport(socket, backgroundScope, StandardTestDispatcher(testScheduler))
        transport.connect()

        val produced = async {
            repeat(128) {
                transport.acceptEnvelopeForTest(RemoteEventEnvelope(0, "tab.changed", byteArrayOf()))
            }
        }
        val received = async { transport.events.take(128).toList() }
        runCurrent()

        assertEquals(128, received.await().size)
        produced.await()
        assertFalse(socket.closed)
        transport.close()
    }

    @Test
    fun maximumValidHeldAttachmentBurstDrainsWithoutClosingTheTransport() = runTest {
        val socket = authenticatedSocket()
        val transport = transport(socket, backgroundScope, StandardTestDispatcher(testScheduler))
        transport.connect()
        val attachment = transport.request("terminal.attach", byteArrayOf())
        runCurrent()
        transport.acceptEnvelopeForTest(RemoteEventEnvelope(1, "terminal.attach", byteArrayOf()))
        repeat(512) { index ->
            transport.acceptEnvelopeForTest(
                RemoteEventEnvelope(
                    1,
                    "terminal.snapshot",
                    terminalSnapshotFixture(1, index = index, total = 512),
                ),
            )
        }

        val received = async { transport.events.take(512).toList() }
        val drain = async { transport.completeAttachment(1, true) }
        runCurrent()

        assertEquals(512, received.await().size)
        drain.await()
        attachment.await()
        assertFalse(socket.closed)
        transport.close()
    }

    @Test
    fun attachmentCorrelationRemainsPinnedPastCompletedRequestEviction() = runTest {
        val transport = transport(
            authenticatedSocket(),
            backgroundScope,
            StandardTestDispatcher(testScheduler),
        )
        transport.connect()
        val attachment = transport.request("terminal.attach", byteArrayOf())
        runCurrent()
        transport.acceptEnvelopeForTest(RemoteEventEnvelope(1, "terminal.attach", byteArrayOf()))
        transport.acceptEnvelopeForTest(
            RemoteEventEnvelope(1, "terminal.snapshot", terminalSnapshotFixture(1, index = 0, total = 2)),
        )

        repeat(65) { index ->
            val response = transport.request("tab.list", byteArrayOf())
            runCurrent()
            val requestId = index.toLong() + 2
            transport.acceptEnvelopeForTest(RemoteEventEnvelope(requestId, "tab.list", byteArrayOf()))
            response.await()
        }

        transport.acceptEnvelopeForTest(
            RemoteEventEnvelope(1, "terminal.snapshot", terminalSnapshotFixture(1, index = 1, total = 2)),
        )
        assertEquals(1L, attachment.await().requestId)
        transport.close()
    }

    @Test
    fun attachmentDrainCannotBeOvertakenByANewChunk() = runTest {
        val transport = transport(
            authenticatedSocket(),
            backgroundScope,
            StandardTestDispatcher(testScheduler),
        )
        transport.connect()
        val attachment = transport.request("terminal.attach", byteArrayOf())
        runCurrent()
        transport.acceptEnvelopeForTest(RemoteEventEnvelope(1, "terminal.attach", byteArrayOf()))
        transport.acceptEnvelopeForTest(
            RemoteEventEnvelope(1, "terminal.snapshot", terminalSnapshotFixture(1, index = 0, total = 2)),
        )
        repeat(64) {
            transport.acceptEnvelopeForTest(RemoteEventEnvelope(0, "tab.changed", byteArrayOf()))
        }

        val drain = async { transport.completeAttachment(1, true) }
        val arrival = async {
            transport.acceptEnvelopeForTest(
                RemoteEventEnvelope(1, "terminal.snapshot", terminalSnapshotFixture(1, index = 1, total = 2)),
            )
        }
        runCurrent()
        val received = async { transport.events.take(66).toList() }
        runCurrent()

        assertEquals(
            listOf(0, 1),
            received.await().filterIsInstance<RemoteServerEvent.TerminalChunk>().map { it.chunk.index },
        )
        drain.await()
        arrival.await()
        attachment.await()
        transport.close()
    }

    @Test
    fun protocolFailureClosesEvenWhenFailureNotificationCannotBeQueued() = runTest {
        val socket = authenticatedSocket()
        val transport = transport(socket, backgroundScope, StandardTestDispatcher(testScheduler))
        transport.connect()
        repeat(64) {
            transport.acceptEnvelopeForTest(RemoteEventEnvelope(0, "tab.changed", byteArrayOf()))
        }

        socket.incoming.send(byteArrayOf(0xff.toByte()))
        runCurrent()

        assertTrue(socket.closed)
        val delivered = mutableListOf<RemoteServerEvent>()
        val terminal = runCatching {
            withTimeout(1_000) { transport.events.toList(delivered) }
        }.exceptionOrNull() as RemoteTransportTerminatedException
        assertEquals(64, delivered.size)
        assertTrue(terminal.outcome is RemoteTransportTerminalOutcome.Recoverable)
    }

    @Test
    fun revocationWithAFullEventQueueStillTerminatesCollectors() = runTest {
        val socket = authenticatedSocket()
        val transport = transport(socket, backgroundScope, StandardTestDispatcher(testScheduler))
        transport.connect()
        repeat(64) {
            transport.acceptEnvelopeForTest(RemoteEventEnvelope(0, "tab.changed", byteArrayOf()))
        }

        transport.acceptEnvelopeForTest(RemoteEventEnvelope(0, "auth.revoked", byteArrayOf()))
        runCurrent()

        assertTrue(socket.closed)
        val delivered = mutableListOf<RemoteServerEvent>()
        val terminal = runCatching {
            withTimeout(1_000) { transport.events.toList(delivered) }
        }.exceptionOrNull() as RemoteTransportTerminatedException
        assertEquals(64, delivered.size)
        assertEquals(RemoteTransportTerminalOutcome.Revoked, terminal.outcome)
    }

    private fun authenticatedSocket() = FakeBinarySocket().apply {
        incoming.trySend(PairingFrames.encode(AuthChallengeFrame(ByteArray(32) { 3 })))
        incoming.trySend(hex("a1646b696e6467617574682e6f6b"))
    }

    private fun transport(
        socket: RemoteBinarySocket,
        scope: CoroutineScope,
        dispatcher: CoroutineDispatcher,
    ) = AuthenticatedRemoteTransport(
        desktop = desktop(),
        deviceKeys = RecordingDeviceKeys(),
        appLock = unlockedAppLock(),
        dialer = FakeDialer(socket),
        scope = scope,
        dispatcher = dispatcher,
    )

    private fun desktop() = PairedDesktop(
        deviceId = "device-1",
        displayName = "Desktop",
        hosts = listOf("desktop.local"),
        port = 43871,
        serverSpkiFingerprint = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        lastSeenEpochMillis = null,
    )

    private fun unlockedAppLock() = AppLock(clock = { 0L })

    private fun hex(value: String): ByteArray =
        value.chunked(2).map { it.toInt(16).toByte() }.toByteArray()
}

private fun terminalSnapshotFixture(requestId: Int, index: Int = 0, total: Int = 1): ByteArray {
    val attributes = linkedMapOf(
        "bold" to false, "faint" to false, "italic" to false, "underline" to false,
        "inverse" to false, "hidden" to false, "strikethrough" to false,
    )
    val cell = linkedMapOf(
        "text" to "x", "width" to 1, "continuation" to false,
        "foreground" to "Default", "background" to "Default", "attributes" to attributes,
    )
    val part = cborFixture(
        linkedMapOf(
            "cols" to 1, "rows" to 1,
            "visible" to listOf(linkedMapOf("cells" to listOf(cell), "wrapped" to false)),
            "cursor" to linkedMapOf("col" to 0, "row" to 0, "visible" to true, "shape" to "Block"),
            "modes" to linkedMapOf(
                "application_cursor" to false, "bracketed_paste" to false,
                "line_wrap" to true, "alternate_screen" to false,
            ),
        ),
    )
    return cborFixture(
        linkedMapOf(
            "transfer_id" to "transfer-1", "tab_id" to "tab-1",
            "attachment_id" to "attachment-1", "kind" to "snapshot",
            "base_revision" to 1, "final_revision" to 1, "row_start" to 0,
            "row_end" to 1, "index" to index, "total" to total,
            "request_id" to requestId, "payload" to part,
        ),
    )
}

private fun cborFixture(value: Any?): ByteArray {
    val output = ByteArrayOutputStream()
    fun header(major: Int, size: Int) {
        if (size < 24) output.write((major shl 5) or size)
        else if (size <= 0xff) { output.write((major shl 5) or 24); output.write(size) }
        else { output.write((major shl 5) or 25); output.write(size ushr 8); output.write(size) }
    }
    fun write(item: Any?) {
        when (item) {
            null -> output.write(0xf6)
            is Boolean -> output.write(if (item) 0xf5 else 0xf4)
            is Int -> header(0, item)
            is String -> item.encodeToByteArray().let { header(3, it.size); output.write(it) }
            is ByteArray -> { header(2, item.size); output.write(item) }
            is List<*> -> { header(4, item.size); item.forEach(::write) }
            is Map<*, *> -> { header(5, item.size); item.forEach { (key, entry) -> write(key); write(entry) } }
            else -> error("unsupported fixture value")
        }
    }
    write(value)
    return output.toByteArray()
}

private class RecordingDeviceKeys(private val afterSign: () -> Unit = {}) : DeviceKeys {
    var signCount = 0
    override fun devicePublicKey(): ByteArray = ByteArray(33)
    override fun signChallenge(nonce: ByteArray): ByteArray {
        signCount++
        afterSign()
        return byteArrayOf(1, 2, 3)
    }
}

private class DeferredDialer : RemoteSocketDialer {
    val socket = CompletableDeferred<RemoteBinarySocket>()
    override suspend fun open(desktop: PairedDesktop): RemoteBinarySocket = socket.await()
}

private class FakeDialer(private val socket: RemoteBinarySocket) : RemoteSocketDialer {
    override suspend fun open(desktop: PairedDesktop): RemoteBinarySocket = socket
}

private class FakeBinarySocket(override val endpoint: RemoteEndpoint? = null) : RemoteBinarySocket {
    val incoming = Channel<ByteArray>(Channel.UNLIMITED)
    val sent = mutableListOf<ByteArray>()
    var closed = false

    override suspend fun receive(): ByteArray = incoming.receive()
    override fun send(bytes: ByteArray): Boolean {
        sent += bytes.copyOf()
        return true
    }
    override fun close() {
        closed = true
        incoming.close()
    }
}

private class FakeDirectDialer(
    private val relay: RemoteBinarySocket,
    private val direct: RemoteBinarySocket?,
) : DirectRemoteSocketDialer {
    var offer: RemoteDirectOffer? = null

    override suspend fun open(desktop: PairedDesktop): RemoteBinarySocket = relay

    override suspend fun openDirect(
        desktop: PairedDesktop,
        offer: RemoteDirectOffer,
    ): RemoteBinarySocket {
        this.offer = offer
        return direct ?: throw RemoteProtocolException("direct path unavailable")
    }
}

private class DeferredDirectDialer(
    private val relay: RemoteBinarySocket,
    private val direct: CompletableDeferred<RemoteBinarySocket>,
) : DirectRemoteSocketDialer {
    override suspend fun open(desktop: PairedDesktop): RemoteBinarySocket = relay

    override suspend fun openDirect(
        desktop: PairedDesktop,
        offer: RemoteDirectOffer,
    ): RemoteBinarySocket = direct.await()
}

private class ReorderingBinarySocket : RemoteBinarySocket {
    val incoming = Channel<ByteArray>(Channel.UNLIMITED)
    val firstRequestEntered = CountDownLatch(1)
    val secondRequestSent = CountDownLatch(1)
    val releaseFirstRequest = CountDownLatch(1)
    val requestSendOrder = mutableListOf<Int>()
    private val sends = AtomicInteger()

    override suspend fun receive(): ByteArray = incoming.receive()

    override fun send(bytes: ByteArray): Boolean {
        val call = sends.getAndIncrement()
        if (call == 0) return true // authentication proof
        val request = call
        if (request == 1) {
            firstRequestEntered.countDown()
            releaseFirstRequest.await(2, TimeUnit.SECONDS)
        }
        synchronized(requestSendOrder) { requestSendOrder += request }
        if (request == 2) secondRequestSent.countDown()
        return true
    }

    override fun close() {
        releaseFirstRequest.countDown()
        incoming.close()
    }
}
