package com.adroited.aiterm.remote

import com.adroited.aiterm.pairing.AuthChallengeFrame
import com.adroited.aiterm.pairing.PairedDesktop
import com.adroited.aiterm.pairing.PairingFrames
import com.adroited.aiterm.security.AppLock
import com.adroited.aiterm.security.DeviceKeys
import com.adroited.aiterm.terminal.DefaultTerminalScreenStore
import com.adroited.aiterm.terminal.CursorState
import com.adroited.aiterm.terminal.ScreenCell
import com.adroited.aiterm.terminal.ScreenRow
import com.adroited.aiterm.terminal.ScreenSnapshot
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.emitAll
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.cbor.ByteString
import kotlinx.serialization.cbor.Cbor
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.RandomAccessFile
import java.nio.file.Files
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

@OptIn(ExperimentalCoroutinesApi::class, ExperimentalSerializationApi::class)
class RemoteClientTest {

    @Test
    fun conversationFailureStopsLoadingAndSuppressesDuplicateRequests() = runTest {
        val transport = FakeRemoteTransport()
        val pending = CompletableDeferred<RemoteResponse>()
        transport.responseFor = { request ->
            if (request.kind == "session.conversation") pending
            else CompletableDeferred(RemoteResponse.Success(request.requestId, request.kind, byteArrayOf()))
        }
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        client.connect()

        client.previewSession("session-1")
        client.previewSession("session-1")
        runCurrent()

        assertEquals("session-1", client.state.value.previewLoadingSessionId)
        assertEquals(1, transport.requests.count { it.kind == "session.conversation" })
        val request = transport.requests.single { it.kind == "session.conversation" }
        pending.complete(RemoteResponse.Error(request.requestId, "conversation.failed", "Could not load it"))
        advanceUntilIdle()

        assertEquals(null, client.state.value.previewLoadingSessionId)
        assertEquals("Could not load it", client.state.value.previewError)
        assertEquals(null, client.state.value.lastError)
        assertEquals(ConnectionState.Connected, client.state.value.connection)
        client.lock()
    }

    @Test
    fun conversationSuccessPublishesMessagesAndStopsLoading() = runTest {
        val transport = FakeRemoteTransport()
        transport.responseFor = { request ->
            CompletableDeferred(
                RemoteResponse.Success(
                    request.requestId,
                    request.kind,
                    if (request.kind == "session.conversation") conversationReply("hello") else byteArrayOf(),
                ),
            )
        }
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        client.connect()

        client.previewSession("session-1")
        advanceUntilIdle()

        assertEquals("session-1", client.state.value.previewSessionId)
        assertEquals(listOf(RemotePreviewMessage("assistant", "hello")), client.state.value.previewMessages)
        assertEquals(null, client.state.value.previewLoadingSessionId)
        assertEquals(null, client.state.value.previewError)
        client.lock()
    }

    @Test
    fun orderedInputBatchIsAcceptedOnceAndQueuedInOrderForTheSameTerminalAttachment() = runTest {
        val transport = FakeRemoteTransport()
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()

        val accepted = client.sendInputs("tab-1", listOf("prompt with image paths", "\r"))
        advanceUntilIdle()

        assertTrue(accepted)
        assertEquals(listOf("terminal.input", "terminal.input"), transport.requests.map { it.kind })
        val expected = listOf(
            RemoteCommands.input("tab-1", "attachment-1", "prompt with image paths".encodeToByteArray()),
            RemoteCommands.input("tab-1", "attachment-1", "\r".encodeToByteArray()),
        )
        assertTrue(expected.zip(transport.requests.map { it.payload }).all { (a, b) -> a.contentEquals(b) })
        client.lock()
    }

    @Test
    fun terminalSubmissionWaitsForPasteAcceptanceAndSettleBeforeSendingEnter() = runTest {
        val transport = FakeRemoteTransport()
        val pasteAccepted = CompletableDeferred<RemoteResponse>()
        transport.responseFor = { request ->
            if (request.kind == "terminal.input" && transport.requests.count { it.kind == "terminal.input" } == 1) {
                pasteAccepted
            } else {
                CompletableDeferred(RemoteResponse.Success(request.requestId, request.kind, byteArrayOf()))
            }
        }
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()

        val submission = async { client.submitInputs("tab-1", listOf("hello", "\r")) }
        runCurrent()

        assertEquals(1, transport.requests.size)
        val paste = transport.requests.single()
        pasteAccepted.complete(RemoteResponse.Success(paste.requestId, paste.kind, byteArrayOf()))
        runCurrent()
        assertEquals(1, transport.requests.size)

        advanceTimeBy(75)
        runCurrent()

        assertTrue(submission.await())
        assertEquals(listOf("terminal.input", "terminal.input"), transport.requests.map { it.kind })
        assertArrayEquals(
            RemoteCommands.input("tab-1", "attachment-1", "\r".encodeToByteArray()),
            transport.requests.last().payload,
        )
        client.lock()
    }

    @Test
    fun orderedInputBatchRejectsEverythingWhenAnyInputCannotBeAccepted() = runTest {
        val transport = FakeRemoteTransport()
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()

        val accepted = client.sendInputs("tab-1", listOf("valid", "x".repeat(1_048_577)))
        advanceUntilIdle()

        assertFalse(accepted)
        assertTrue(transport.requests.isEmpty())
        client.lock()
    }

    @Test
    fun orderedInputBatchRejectsAnOldTabAfterSelectionChanges() = runTest {
        val transport = FakeRemoteTransport()
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()

        val accepted = client.sendInputs("old-tab", listOf("old image paths", "\r"))
        advanceUntilIdle()

        assertFalse(accepted)
        assertTrue(transport.requests.isEmpty())
        client.lock()
    }

    @Test
    fun orderedInputBatchRejectsEverythingAfterFocusIsLost() = runTest {
        val transport = FakeRemoteTransport()
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        client.acceptForTest(
            RemoteServerEvent.FocusChanged(
                "tab-1",
                "attachment-1",
                FocusOwner.Other,
                TerminalSize(80, 24),
            ),
        )
        transport.requests.clear()

        val accepted = client.sendInputs("tab-1", listOf("old image paths", "\r"))
        advanceUntilIdle()

        assertFalse(accepted)
        assertTrue(transport.requests.isEmpty())
        assertTrue(client.state.value.showTakeFocus)
        client.lock()
    }

    @Test
    fun inputNotOwnedKeepsTerminalReadOnlyAndOffersTakeFocus() = runTest {
        val transport = FakeRemoteTransport()
        val client = RemoteClient(
            transportFactory = { transport },
            screenStore = DefaultTerminalScreenStore(),
            isUnlocked = { true },
            scope = this,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        client.acceptForTest(
            RemoteServerEvent.FocusChanged(
                tabId = "tab-1",
                attachmentId = "attachment-1",
                focus = FocusOwner.Other,
                size = TerminalSize(80, 24),
            ),
        )

        val sent = client.sendInput("whoami")
        advanceUntilIdle()

        assertFalse(sent)
        assertTrue(client.state.value.showTakeFocus)
        assertTrue(client.state.value.readOnly)
        assertEquals(emptyList<RemoteRequest>(), transport.requests)
    }

    @Test
    fun lockCancelsPendingRequestsTransfersAndConnection() = runTest {
        val transport = FakeRemoteTransport()
        val client = RemoteClient(
            transportFactory = { transport },
            screenStore = DefaultTerminalScreenStore(),
            isUnlocked = { true },
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        client.connect()
        client.acceptForTest(RemoteServerEvent.TransferStarted("transfer-1"))
        client.lock()

        assertEquals(ConnectionState.Locked, client.state.value.connection)
        assertEquals(0, client.state.value.pendingTransfers)
        assertTrue(transport.closed)
    }

    @Test
    fun lockDuringConnectCannotPublishTheLateConnection() = runTest {
        val transport = DeferredRemoteTransport(connectImmediately = false)
        val client = RemoteClient(
            transportFactory = { transport },
            screenStore = DefaultTerminalScreenStore(),
            isUnlocked = { true },
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )

        val connecting = async { client.connect() }
        runCurrent()
        assertEquals(ConnectionState.Connecting, client.state.value.connection)

        client.lock()
        transport.allowConnect.complete(Unit)
        advanceUntilIdle()

        assertFalse(connecting.await())
        assertEquals(ConnectionState.Locked, client.state.value.connection)
        assertTrue(transport.closed)
    }

    @Test
    fun explicitAuthenticationRevocationStopsReconnectAndPurgesState() = runTest {
        val transport = object : RemoteTransport {
            override val events = MutableSharedFlow<RemoteServerEvent>()
            override suspend fun connect() = throw RemoteAccessRevokedException()
            override fun request(kind: String, payload: ByteArray, onAssigned: (Long) -> Unit) =
                CompletableDeferred<RemoteResponse>().also { it.completeExceptionally(IllegalStateException("not connected")) }
            override fun requestBatch(requests: List<RemoteRequestInput>) = null
            override fun close() = Unit
        }
        val client = RemoteClient(
            transportFactory = { transport },
            screenStore = DefaultTerminalScreenStore(),
            isUnlocked = { true },
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )

        assertFalse(client.connect())
        assertEquals(ConnectionState.Revoked, client.state.value.connection)
        advanceTimeBy(32_000)
        runCurrent()
        assertEquals(ConnectionState.Revoked, client.state.value.connection)
    }

    @Test
    fun fullQueueRevocationCompletionPurgesStateAndNeverReconnects() = runTest {
        val socket = roundThreeAuthenticatedSocket()
        val transport = roundThreeAuthenticatedTransport(
            socket,
            backgroundScope,
            StandardTestDispatcher(testScheduler),
        )
        var factoryCalls = 0
        val store = DefaultTerminalScreenStore()
        val client = RemoteClient(
            transportFactory = {
                factoryCalls += 1
                transport
            },
            screenStore = store,
            isUnlocked = { true },
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        assertTrue(client.connect())
        store.replace(roundThreeScreen())
        client.acceptForTest(RemoteServerEvent.TransferStarted("stale-transfer"))
        repeat(64) {
            transport.acceptEnvelopeForTest(RemoteEventEnvelope(0, "error", roundThreeBusyError()))
        }

        transport.acceptEnvelopeForTest(RemoteEventEnvelope(0, "auth.revoked", byteArrayOf()))
        runCurrent()

        assertEquals(ConnectionState.Revoked, client.state.value.connection)
        assertEquals(null, store.screen.value)
        assertEquals(0, client.state.value.pendingTransfers)
        advanceTimeBy(32_000)
        runCurrent()
        assertEquals(1, factoryCalls)
    }

    @Test
    fun pendingRequestFailureCannotWinBeforeFullQueueRevocationOutcome() = runTest {
        val socket = roundThreeAuthenticatedSocket()
        val delegate = roundThreeAuthenticatedTransport(
            socket,
            backgroundScope,
            StandardTestDispatcher(testScheduler),
        )
        val releaseEvents = CompletableDeferred<Unit>()
        val first = GatedEventsRemoteTransport(delegate, releaseEvents)
        val replacement = FakeRemoteTransport()
        var factoryCalls = 0
        val store = DefaultTerminalScreenStore()
        val client = RemoteClient(
            transportFactory = {
                factoryCalls += 1
                if (factoryCalls == 1) first else replacement
            },
            screenStore = store,
            isUnlocked = { true },
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        assertTrue(client.connect())
        runCurrent()
        store.replace(roundThreeScreen())
        client.acceptForTest(RemoteServerEvent.TransferStarted("stale-transfer"))
        client.refreshSessions()
        runCurrent()
        assertEquals("the request must be sent and awaiting its response", 2, socket.sentFrames.size)
        repeat(64) {
            delegate.acceptEnvelopeForTest(RemoteEventEnvelope(0, "error", roundThreeBusyError()))
        }

        delegate.acceptEnvelopeForTest(RemoteEventEnvelope(0, "auth.revoked", byteArrayOf()))
        runCurrent()

        assertEquals(ConnectionState.Revoked, client.state.value.connection)
        assertEquals(null, store.screen.value)
        assertEquals(0, client.state.value.pendingTransfers)
        releaseEvents.complete(Unit)
        advanceTimeBy(32_000)
        runCurrent()
        assertEquals(1, factoryCalls)
    }

    @Test
    fun fullQueueProtocolFailureCompletionPurgesStateAndReconnects() = runTest {
        val socket = roundThreeAuthenticatedSocket()
        val first = roundThreeAuthenticatedTransport(
            socket,
            backgroundScope,
            StandardTestDispatcher(testScheduler),
        )
        val replacement = FakeRemoteTransport()
        var factoryCalls = 0
        val store = DefaultTerminalScreenStore()
        val client = RemoteClient(
            transportFactory = {
                factoryCalls += 1
                if (factoryCalls == 1) first else replacement
            },
            screenStore = store,
            isUnlocked = { true },
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        assertTrue(client.connect())
        store.replace(roundThreeScreen())
        client.acceptForTest(RemoteServerEvent.TransferStarted("stale-transfer"))
        repeat(64) {
            first.acceptEnvelopeForTest(RemoteEventEnvelope(0, "error", roundThreeBusyError()))
        }
        socket.incoming.send(byteArrayOf(0xff.toByte()))

        runCurrent()

        assertEquals(ConnectionState.Reconnecting, client.state.value.connection)
        assertEquals(null, store.screen.value)
        assertEquals(0, client.state.value.pendingTransfers)
        advanceTimeBy(1_000)
        runCurrent()
        assertEquals(2, factoryCalls)
        assertEquals(ConnectionState.Connected, client.state.value.connection)
        client.lock()
    }

    @Test
    fun rapidTabSelectionDetachesStaleAttachmentAndRejectsItsChunks() = runTest {
        val transport = DeferredRemoteTransport()
        val store = DefaultTerminalScreenStore()
        val client = RemoteClient(
            transportFactory = { transport },
            screenStore = store,
            isUnlocked = { true },
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        client.connect()

        client.selectTab("tab-a")
        runCurrent()
        client.selectTab("tab-b")
        runCurrent()
        assertEquals(1, transport.pendingAttachCount())

        transport.completeNextAttach("tab-a", "attachment-a")
        runCurrent()
        assertEquals(listOf("terminal.attach", "terminal.detach", "terminal.attach"), transport.requests.map { it.kind })

        transport.completeNextAttach("tab-b", "attachment-b")
        runCurrent()
        assertEquals("tab-b", client.state.value.activeTabId)

        client.acceptForTest(snapshotChunk("old", "tab-a", "attachment-a", "WRONG"))
        assertEquals(null, store.screen.value)
        client.acceptForTest(snapshotChunk("current", "tab-b", "attachment-b", "RIGHT"))
        assertEquals("RIGHT", store.screen.value?.visible?.single()?.plainText())
        client.lock()
    }

    @Test
    fun disconnectClosesTransportOutsideTheClientLifecycleLock() = runTest {
        lateinit var client: RemoteClient
        val closeObservedUnlockedClient = AtomicBoolean(false)
        val transport = FakeRemoteTransport(
            onClose = {
                val probe = thread(start = true) { client.requestNextScrollbackPage() }
                probe.join(1_000)
                closeObservedUnlockedClient.set(!probe.isAlive)
            },
        )
        client = RemoteClient(
            transportFactory = { transport },
            screenStore = DefaultTerminalScreenStore(),
            isUnlocked = { true },
            scope = this,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        client.connect()

        client.acceptForTest(RemoteServerEvent.Failure("transport.disconnected", "lost"))

        assertTrue("transport close ran while lifecycleLock was held", closeObservedUnlockedClient.get())
        client.lock()
    }

    @Test
    fun selectingANewTabAtomicallyRejectsOldAttachmentDamage() = runTest {
        val transport = DeferredRemoteTransport()
        val store = DefaultTerminalScreenStore()
        val client = RemoteClient(
            transportFactory = { transport },
            screenStore = store,
            isUnlocked = { true },
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        client.connect()
        client.selectTab("tab-a")
        runCurrent()
        transport.completeNextAttach("tab-a", "attachment-a")
        runCurrent()

        client.selectTab("tab-b")
        client.acceptForTest(snapshotChunk("late-a", "tab-a", "attachment-a", "WRONG"))
        assertEquals(null, store.screen.value)

        runCurrent()
        transport.completeNextAttach("tab-b", "attachment-b")
        runCurrent()
        client.acceptForTest(snapshotChunk("current-b", "tab-b", "attachment-b", "RIGHT"))
        assertEquals("RIGHT", store.screen.value?.visible?.single()?.plainText())
        client.lock()
    }

    @Test
    fun supersededSelectionsStillDetachTheCapturedOldAttachment() = runTest {
        val transport = DeferredRemoteTransport()
        val client = RemoteClient(
            transportFactory = { transport },
            screenStore = DefaultTerminalScreenStore(),
            isUnlocked = { true },
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        client.connect()
        client.selectTab("tab-a")
        runCurrent()
        transport.completeNextAttach("tab-a", "attachment-a")
        runCurrent()

        client.selectTab("tab-b")
        client.selectTab("tab-c")
        runCurrent()

        assertEquals(
            listOf("terminal.attach", "terminal.detach", "terminal.attach"),
            transport.requests.map(RemoteRequest::kind),
        )
        transport.completeNextAttach("tab-c", "attachment-c")
        runCurrent()
        assertEquals("tab-c", client.state.value.activeTabId)
        client.lock()
    }

    @Test
    fun revisionMismatchKeepsTheCurrentScreenAndRequestsAuthoritativeRecovery() = runTest {
        val transport = FakeRemoteTransport()
        val store = DefaultTerminalScreenStore()
        val client = RemoteClient(
            transportFactory = { transport },
            screenStore = store,
            isUnlocked = { true },
            scope = this,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        store.replace(
            ScreenSnapshot(
                tabId = "tab-1",
                revision = 5,
                cols = 1,
                rows = 1,
                visible = listOf(ScreenRow(listOf(ScreenCell("old")))),
                cursor = CursorState(0, 0, true),
            ),
        )
        client.acceptForTest(
            RemoteServerEvent.TerminalChunk(
                TerminalTransferChunk(
                    transferId = "transfer-1",
                    tabId = "tab-1",
                    attachmentId = "attachment-1",
                    kind = TerminalTransferKind.Diff,
                    baseRevision = 4,
                    finalRevision = 6,
                    rowStart = 0,
                    rowEnd = 1,
                    index = 0,
                    total = 1,
                    requestId = 0,
                    part = TerminalTransferPart.Diff(
                        patches = listOf(com.adroited.aiterm.terminal.RowPatch(0, ScreenRow(listOf(ScreenCell("new"))))),
                        cursor = null,
                        modes = null,
                    ),
                ),
            ),
        )
        advanceUntilIdle()

        assertEquals("old", store.screen.value?.visible?.single()?.plainText())
        assertEquals("terminal.resume", transport.requests.last().kind)
        client.lock()
    }

    @Test
    fun authenticatedDisconnectReconnectsWithoutChangingTransportSecurityPolicy() = runTest {
        val transports = mutableListOf<FakeRemoteTransport>()
        val client = RemoteClient(
            transportFactory = { FakeRemoteTransport().also(transports::add) },
            screenStore = DefaultTerminalScreenStore(),
            isUnlocked = { true },
            scope = this,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        client.connect()

        client.acceptForTest(RemoteServerEvent.Failure("transport.disconnected", "storm"))
        assertEquals(ConnectionState.Reconnecting, client.state.value.connection)
        advanceTimeBy(1_000)
        runCurrent()

        assertEquals(2, transports.size)
        assertEquals(ConnectionState.Connected, client.state.value.connection)
        client.lock()
    }

    @Test
    fun initialFailureKeepsRetryingPastTheOldFiveAttemptLimit() = runTest {
        var factoryCalls = 0
        val client = RemoteClient(
            transportFactory = {
                factoryCalls += 1
                if (factoryCalls <= 6) FailingRemoteTransport() else FakeRemoteTransport()
            },
            screenStore = DefaultTerminalScreenStore(),
            isUnlocked = { true },
            scope = backgroundScope,
            dispatcher = StandardTestDispatcher(testScheduler),
        )

        assertFalse(client.connect())
        assertEquals(ConnectionState.Reconnecting, client.state.value.connection)
        advanceTimeBy(47_000)
        runCurrent()

        assertEquals(7, factoryCalls)
        assertEquals(ConnectionState.Connected, client.state.value.connection)
        client.lock()
    }

    @Test
    fun completeScrollbackPageIsPublishedOnlyForTheVisibleTab() = runTest {
        val store = DefaultTerminalScreenStore()
        store.replace(
            ScreenSnapshot(
                tabId = "tab-1",
                revision = 5,
                cols = 4,
                rows = 1,
                visible = listOf(ScreenRow(listOf(ScreenCell("live")))),
                cursor = CursorState(0, 0, true),
            ),
        )
        val transport = FakeRemoteTransport()
        val client = RemoteClient(
            transportFactory = { transport },
            screenStore = store,
            isUnlocked = { true },
            scope = this,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        store.replace(
            ScreenSnapshot(
                tabId = "tab-1",
                revision = 5,
                cols = 4,
                rows = 1,
                visible = listOf(ScreenRow(listOf(ScreenCell("live")))),
                cursor = CursorState(0, 0, true),
            ),
        )

        assertTrue(client.requestNextScrollbackPage(128))
        val requestId = transport.requests.last { it.kind == "terminal.scrollback" }.requestId
        client.acceptForTest(
            RemoteServerEvent.TerminalChunk(
                TerminalTransferChunk(
                    transferId = "history-1",
                    tabId = "tab-1",
                    attachmentId = "attachment-1",
                    kind = TerminalTransferKind.Scrollback,
                    baseRevision = 5,
                    finalRevision = 5,
                    rowStart = 0,
                    rowEnd = 1,
                    index = 0,
                    total = 1,
                    requestId = requestId,
                    part = TerminalTransferPart.Scrollback(
                        listOf(ScreenRow("old".map { ScreenCell(it.toString()) })),
                    ),
                ),
            ),
        )

        assertEquals(listOf("old"), client.scrollback.value.map(ScreenRow::plainText))
        client.lock()
    }

    @Test
    fun rapidScrollbackPagingKeepsOnlyOneRequestForTheExpectedOffset() = runTest {
        val transport = FakeRemoteTransport()
        val store = DefaultTerminalScreenStore()
        val client = RemoteClient(
            transportFactory = { transport },
            screenStore = store,
            isUnlocked = { true },
            scope = this,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()

        assertTrue(client.requestNextScrollbackPage(128))
        assertFalse(client.requestNextScrollbackPage(128))
        assertEquals(1, transport.requests.count { it.kind == "terminal.scrollback" })
        client.lock()
    }

    @Test
    fun selectingBDiscardsAPagingTransactionAndAllowsBPaging() = runTest {
        val transport = DeferredRemoteTransport()
        val client = RemoteClient(
            transportFactory = { transport },
            screenStore = DefaultTerminalScreenStore(),
            isUnlocked = { true },
            scope = this,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        client.connect()
        client.selectTab("tab-a")
        runCurrent()
        transport.completeNextAttach("tab-a", "attachment-a")
        advanceUntilIdle()
        assertTrue(client.requestNextScrollbackPage(128))
        val oldRequestId = transport.requests.last { it.kind == "terminal.scrollback" }.requestId

        client.selectTab("tab-b")
        runCurrent()
        transport.completeNextAttach("tab-b", "attachment-b")
        advanceUntilIdle()
        client.acceptForTest(scrollbackChunk(oldRequestId, "stale-a", "tab-a", "attachment-a"))

        assertEquals(emptyList<ScreenRow>(), client.scrollback.value)
        assertTrue(client.requestNextScrollbackPage(128))
        assertEquals(2, transport.requests.count { it.kind == "terminal.scrollback" })
        client.lock()
    }

    @Test
    fun unexpectedScrollbackCorrelationCannotPublishOutOfOrderRows() = runTest {
        val transport = FakeRemoteTransport()
        val store = DefaultTerminalScreenStore()
        val client = RemoteClient(
            transportFactory = { transport },
            screenStore = store,
            isUnlocked = { true },
            scope = this,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        store.replace(
            ScreenSnapshot(
                tabId = "tab-1",
                revision = 1,
                cols = 1,
                rows = 1,
                visible = listOf(ScreenRow(listOf(ScreenCell("x")))),
                cursor = CursorState(0, 0, true),
            ),
        )
        assertTrue(client.requestNextScrollbackPage(128))
        val expectedId = transport.requests.last { it.kind == "terminal.scrollback" }.requestId

        client.acceptForTest(scrollbackChunk(expectedId + 1, "later"))
        assertEquals(emptyList<ScreenRow>(), client.scrollback.value)
        client.acceptForTest(scrollbackChunk(expectedId, "expected"))
        assertEquals(listOf("expected"), client.scrollback.value.map(ScreenRow::plainText))
        client.lock()
    }

    @Test
    fun imageUploadsAreSequentialAndReturnDesktopPathsInSourceOrder() = runTest {
        val transport = FakeRemoteTransport()
        val client = RemoteClient(
            transportFactory = { transport },
            screenStore = DefaultTerminalScreenStore(),
            isUnlocked = { true },
            scope = this,
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        val first = uploadSource("first", byteArrayOf(1, 2, 3))
        val second = uploadSource("second", byteArrayOf(4, 5))
        var finished = 0
        transport.responseFor = { request ->
            val payload = when (request.kind) {
                "terminal.upload.begin" -> uploadBeginReply("upload-${request.requestId}", 0)
                "terminal.upload.chunk", "terminal.upload.cancel" -> uploadSuccessReply()
                "terminal.upload.finish" -> uploadedPathReply("/desktop/${++finished}.jpg")
                else -> byteArrayOf()
            }
            CompletableDeferred(RemoteResponse.Success(request.requestId, request.kind, payload))
        }
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()
        val progress = mutableListOf<RemoteUploadProgress>()

        val operation = async { client.uploadImages("tab-1", listOf(first, second), progress::add) }
        advanceUntilIdle()
        val result = operation.await()

        assertTrue(result.isSuccess)
        assertEquals(listOf("/desktop/1.jpg", "/desktop/2.jpg"), result.getOrThrow())
        val begins = transport.requests
            .filter { it.kind == "terminal.upload.begin" }
            .map { request -> decodeUploadBegin(request.payload) }
        assertEquals(2, begins.size)
        assertEquals(listOf(2, 2), begins.map(UploadBeginWire::submissionCount))
        assertEquals(listOf(0, 1), begins.map(UploadBeginWire::memberIndex))
        assertEquals(listOf(5L, 5L), begins.map(UploadBeginWire::submissionBytes))
        assertEquals(begins.first().submissionId, begins.last().submissionId)
        java.util.UUID.fromString(begins.first().submissionId)
        assertEquals(
            listOf(
                "terminal.upload.begin", "terminal.upload.chunk", "terminal.upload.finish",
                "terminal.upload.begin", "terminal.upload.chunk", "terminal.upload.finish",
            ),
            transport.requests.map(RemoteRequest::kind),
        )
        assertEquals(listOf(0L, 3L), progress.filter { it.sourceId == "first" }.map { it.sent })
        assertEquals(listOf(0L, 2L), progress.filter { it.sourceId == "second" }.map { it.sent })
        cleanupUploadSources(first, second)
        client.lock()
    }

    @Test
    fun timedOutChunkResumesFromTheDesktopAcknowledgedOffsetWithoutRepeatingBytes() = runTest {
        val transport = FakeRemoteTransport()
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        val bytes = ByteArray(RemoteCommands.MAX_UPLOAD_CHUNK_BYTES + 2) { (it % 251).toByte() }
        val source = uploadSource("resume-timeout", bytes)
        var begins = 0
        var chunks = 0
        transport.responseFor = { request ->
            when (request.kind) {
                "terminal.upload.begin" -> CompletableDeferred(
                    RemoteResponse.Success(
                        request.requestId,
                        request.kind,
                        uploadBeginReply("upload-1", if (begins++ == 0) 0 else 1),
                    ),
                )
                "terminal.upload.chunk" -> if (chunks++ == 0) {
                    CompletableDeferred<RemoteResponse>().also {
                        it.completeExceptionally(RemoteProtocolException("remote request timed out"))
                    }
                } else {
                    CompletableDeferred(
                        RemoteResponse.Success(request.requestId, request.kind, uploadSuccessReply()),
                    )
                }
                "terminal.upload.finish" -> CompletableDeferred(
                    RemoteResponse.Success(request.requestId, request.kind, uploadedPathReply("/desktop/resumed.jpg")),
                )
                else -> CompletableDeferred(
                    RemoteResponse.Success(request.requestId, request.kind, byteArrayOf()),
                )
            }
        }
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()

        val operation = async { client.uploadImages("tab-1", listOf(source)) }
        advanceUntilIdle()

        assertEquals(listOf("/desktop/resumed.jpg"), operation.await().getOrThrow())
        assertEquals(
            listOf(
                "terminal.upload.begin",
                "terminal.upload.chunk",
                "terminal.upload.begin",
                "terminal.upload.chunk",
                "terminal.upload.finish",
            ),
            transport.requests.map(RemoteRequest::kind),
        )
        val retriedChunk = transport.requests.filter { it.kind == "terminal.upload.chunk" }.last()
        val decoded = decodeUploadChunk(retriedChunk.payload)
        assertEquals(1, decoded.index)
        assertArrayEquals(bytes.copyOfRange(RemoteCommands.MAX_UPLOAD_CHUNK_BYTES, bytes.size), decoded.data)
        cleanupUploadSources(source)
        client.lock()
    }

    @Test
    fun finishedFirstImageIsCancelledAfterFocusLossAndRetrySucceedsOnTheSameConnection() = runTest {
        val transport = FakeRemoteTransport()
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        val first = uploadSource("first-retry", byteArrayOf(1, 2, 3))
        val second = uploadSource("second-retry", byteArrayOf(4, 5))
        var activeSubmission: String? = null
        var declaredCount = 0
        var finishedCount = 0
        var nextUploadId = 0
        var loseFocusAfterFinish = true
        transport.responseFor = { request ->
            when (request.kind) {
                "terminal.upload.begin" -> {
                    val begin = decodeUploadBegin(request.payload)
                    if (activeSubmission != null && activeSubmission != begin.submissionId) {
                        CompletableDeferred(
                            RemoteResponse.Error(
                                request.requestId,
                                "terminal.upload_invalid_submission",
                                "this connection already has an active submission",
                            ),
                        )
                    } else {
                        if (activeSubmission == null) {
                            activeSubmission = begin.submissionId
                            declaredCount = begin.submissionCount
                            finishedCount = 0
                        }
                        CompletableDeferred(
                            RemoteResponse.Success(
                                request.requestId,
                                request.kind,
                                uploadBeginReply("upload-${++nextUploadId}", 0),
                            ),
                        )
                    }
                }
                "terminal.upload.chunk" -> CompletableDeferred(
                    RemoteResponse.Success(request.requestId, request.kind, uploadSuccessReply()),
                )
                "terminal.upload.finish" -> {
                    finishedCount += 1
                    if (finishedCount == declaredCount) activeSubmission = null
                    if (loseFocusAfterFinish) {
                        loseFocusAfterFinish = false
                        client.acceptForTest(
                            RemoteServerEvent.FocusChanged(
                                "tab-1",
                                "attachment-1",
                                FocusOwner.Other,
                                TerminalSize(80, 24),
                            ),
                        )
                    }
                    CompletableDeferred(
                        RemoteResponse.Success(
                            request.requestId,
                            request.kind,
                            uploadedPathReply("/desktop/finished-$finishedCount.jpg"),
                        ),
                    )
                }
                "terminal.upload.cancel" -> {
                    activeSubmission = null
                    CompletableDeferred(
                        RemoteResponse.Success(request.requestId, request.kind, uploadSuccessReply()),
                    )
                }
                else -> CompletableDeferred(
                    RemoteResponse.Success(request.requestId, request.kind, byteArrayOf()),
                )
            }
        }
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()

        val interrupted = async { client.uploadImages("tab-1", listOf(first, second)) }
        advanceUntilIdle()

        assertTrue(interrupted.await().isFailure)
        assertEquals(
            listOf(
                "terminal.upload.begin",
                "terminal.upload.chunk",
                "terminal.upload.finish",
                "terminal.upload.cancel",
            ),
            transport.requests.map(RemoteRequest::kind),
        )
        client.acceptForTest(
            RemoteServerEvent.FocusChanged(
                "tab-1",
                "attachment-1",
                FocusOwner.Self,
                TerminalSize(80, 24),
            ),
        )
        transport.requests.clear()

        val retried = async { client.uploadImages("tab-1", listOf(first, second)) }
        advanceUntilIdle()

        assertTrue(retried.await().isSuccess)
        assertEquals(2, transport.requests.count { it.kind == "terminal.upload.finish" })
        cleanupUploadSources(first, second)
        client.lock()
    }

    @Test
    fun queuedImageUploadRejectsAStaleDraftTabBeforeAnyBeginRequest() = runTest {
        val transport = DeferredRemoteTransport()
        val dispatcher = StandardTestDispatcher(testScheduler)
        val client = uploadClient(transport, this, dispatcher)
        val source = uploadSource("stale", byteArrayOf(1, 2, 3))
        client.connect()
        client.selectTab("tab-1")
        runCurrent()
        transport.completeNextAttach("tab-1", "attachment-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()

        val releaseQueuedUpload = CompletableDeferred<Unit>()
        val operation = async {
            releaseQueuedUpload.await()
            client.uploadImages("tab-1", listOf(source))
        }
        runCurrent()
        client.selectTab("tab-2")
        runCurrent()
        transport.completeNextAttach("tab-2", "attachment-2")
        advanceUntilIdle()
        client.acceptForTest(
            RemoteServerEvent.FocusChanged(
                "tab-2",
                "attachment-2",
                FocusOwner.Self,
                TerminalSize(80, 24),
            ),
        )
        assertEquals("tab-2", client.state.value.activeTabId)
        assertEquals(FocusOwner.Self, client.state.value.focus)

        releaseQueuedUpload.complete(Unit)
        advanceUntilIdle()

        assertTrue(operation.await().isFailure)
        assertFalse(transport.requests.any { it.kind == "terminal.upload.begin" })
        assertTrue(source.file.exists())
        cleanupUploadSources(source)
        client.lock()
    }

    @Test
    fun imageUploadRejectsAResumeOffsetPastTheImageAndCancelsTheUpload() = runTest {
        val transport = FakeRemoteTransport()
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        val source = uploadSource("one", byteArrayOf(1))
        transport.responseFor = { request ->
            val payload = when (request.kind) {
                "terminal.upload.begin" -> uploadBeginReply("upload-1", 2)
                "terminal.upload.cancel" -> uploadSuccessReply()
                else -> byteArrayOf()
            }
            CompletableDeferred(RemoteResponse.Success(request.requestId, request.kind, payload))
        }
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()

        val operation = async { client.uploadImages("tab-1", listOf(source)) }
        advanceUntilIdle()

        assertTrue(operation.await().isFailure)
        assertEquals(
            listOf("terminal.upload.begin", "terminal.upload.cancel"),
            transport.requests.map(RemoteRequest::kind),
        )
        assertTrue(source.file.exists())
        cleanupUploadSources(source)
        client.lock()
    }

    @Test
    fun uploadServerFailureCancelsBegunWorkAndPreservesTheDraftFile() = runTest {
        val transport = FakeRemoteTransport()
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        val source = uploadSource("one", byteArrayOf(1, 2, 3))
        transport.responseFor = { request ->
            when (request.kind) {
                "terminal.upload.begin" -> CompletableDeferred(
                    RemoteResponse.Success(request.requestId, request.kind, uploadBeginReply("upload-1", 0)),
                )
                "terminal.upload.chunk" -> CompletableDeferred(
                    RemoteResponse.Error(request.requestId, "terminal.upload_failed", "staging failed"),
                )
                "terminal.upload.cancel" -> CompletableDeferred(
                    RemoteResponse.Success(request.requestId, request.kind, uploadSuccessReply()),
                )
                else -> CompletableDeferred(RemoteResponse.Success(request.requestId, request.kind, byteArrayOf()))
            }
        }
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()

        val operation = async { client.uploadImages("tab-1", listOf(source)) }
        advanceUntilIdle()

        val failure = operation.await()
        assertTrue(failure.isFailure)
        assertEquals("terminal.upload_failed", (failure.exceptionOrNull() as RemoteUploadException).code)
        assertEquals(
            listOf("terminal.upload.begin", "terminal.upload.chunk", "terminal.upload.cancel"),
            transport.requests.map(RemoteRequest::kind),
        )
        assertTrue(source.file.exists())
        assertFalse(transport.requests.any { it.kind == "terminal.input" })
        cleanupUploadSources(source)
        client.lock()
    }

    @Test
    fun disconnectedUploadPreservesTheDraftFileAndCleansUpTheServerAttempt() = runTest {
        val transport = FakeRemoteTransport()
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        val source = uploadSource("one", byteArrayOf(1))
        transport.responseFor = { request ->
            when (request.kind) {
                "terminal.upload.begin" -> CompletableDeferred(
                    RemoteResponse.Success(request.requestId, request.kind, uploadBeginReply("upload-1", 0)),
                )
                "terminal.upload.chunk" -> CompletableDeferred<RemoteResponse>().also {
                    it.completeExceptionally(
                        RemoteTransportTerminatedException(
                            RemoteTransportTerminalOutcome.Recoverable("connection ended"),
                        ),
                    )
                }
                "terminal.upload.cancel" -> CompletableDeferred(
                    RemoteResponse.Success(request.requestId, request.kind, uploadSuccessReply()),
                )
                else -> CompletableDeferred(RemoteResponse.Success(request.requestId, request.kind, byteArrayOf()))
            }
        }
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()

        val operation = async { client.uploadImages("tab-1", listOf(source)) }
        advanceUntilIdle()

        assertTrue(operation.await().isFailure)
        assertEquals(
            listOf("terminal.upload.begin", "terminal.upload.chunk", "terminal.upload.cancel"),
            transport.requests.map(RemoteRequest::kind),
        )
        assertTrue(source.file.exists())
        cleanupUploadSources(source)
        client.lock()
    }

    @Test
    fun imageUploadRequiresTheActiveTerminalFocusBeforeItSendsAnyRequest() = runTest {
        val transport = FakeRemoteTransport()
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        val source = uploadSource("one", byteArrayOf(1))
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()
        client.acceptForTest(
            RemoteServerEvent.FocusChanged("tab-1", "attachment-1", FocusOwner.Other, TerminalSize(80, 24)),
        )

        val operation = async { client.uploadImages("tab-1", listOf(source)) }
        advanceUntilIdle()

        assertTrue(operation.await().isFailure)
        assertEquals(emptyList<RemoteRequest>(), transport.requests)
        cleanupUploadSources(source)
        client.lock()
    }

    @Test
    fun imageUploadRejectsClientImageCountAndByteBoundsBeforeAnyRequest() = runTest {
        val transport = FakeRemoteTransport()
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        val source = uploadSource("one", byteArrayOf(1))
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()
        val fifth = listOf("a", "b", "c", "d", "e").map { id -> source.copy(id = id) }
        val overBudget = source.copy(length = 48L * 1_024 * 1_024 + 1)

        val tooMany = async { client.uploadImages("tab-1", fifth) }
        val tooLarge = async { client.uploadImages("tab-1", listOf(overBudget)) }
        advanceUntilIdle()

        assertTrue(tooMany.await().isFailure)
        assertTrue(tooLarge.await().isFailure)
        assertEquals(emptyList<RemoteRequest>(), transport.requests)
        cleanupUploadSources(source)
        client.lock()
    }

    @Test
    fun imageUploadAcceptsTheExactFourImage48MiBAggregateBoundary() {
        val source = sparseUploadSource("boundary", 12L * 1_024 * 1_024)
        try {
            val submission = validateRemoteUploadSources(
                listOf("one", "two", "three", "four").map { source.copy(id = it) },
            )

            assertEquals(4, submission.count)
            assertEquals(48L * 1_024 * 1_024, submission.bytes)
        } finally {
            cleanupUploadSources(source)
        }
    }

    @Test
    fun failedUploadReturnsAfterOneCleanupBudgetWhenCancelNeverReplies() = runTest {
        val transport = FakeRemoteTransport()
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        val source = uploadSource("one", byteArrayOf(1))
        val pendingCancel = CompletableDeferred<RemoteResponse>()
        transport.responseFor = { request ->
            when (request.kind) {
                "terminal.upload.begin" -> CompletableDeferred(
                    RemoteResponse.Success(request.requestId, request.kind, uploadBeginReply("upload-1", 0)),
                )
                "terminal.upload.chunk" -> CompletableDeferred(
                    RemoteResponse.Error(request.requestId, "terminal.upload_failed", "staging failed"),
                )
                "terminal.upload.cancel" -> pendingCancel
                else -> CompletableDeferred(RemoteResponse.Success(request.requestId, request.kind, byteArrayOf()))
            }
        }
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()
        val operation = async { client.uploadImages("tab-1", listOf(source)) }
        try {
            runCurrent()
            advanceTimeBy(2_001)
            runCurrent()

            assertTrue(operation.isCompleted)
            val failure = operation.await()
            assertTrue(failure.isFailure)
            assertEquals("terminal.upload_failed", (failure.exceptionOrNull() as RemoteUploadException).code)
            assertEquals(listOf(pendingCancel), transport.abandonedRequests)
        } finally {
            pendingCancel.complete(RemoteResponse.Success(99, "terminal.upload.cancel", uploadSuccessReply()))
            operation.cancelAndJoin()
            cleanupUploadSources(source)
            client.lock()
        }
    }

    @Test
    fun cancelledUploadRethrowsCancellationAfterOneCleanupBudgetWhenCancelNeverReplies() = runTest {
        val transport = FakeRemoteTransport()
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        val source = uploadSource("one", byteArrayOf(1))
        val pendingChunk = CompletableDeferred<RemoteResponse>()
        val pendingCancel = CompletableDeferred<RemoteResponse>()
        transport.responseFor = { request ->
            when (request.kind) {
                "terminal.upload.begin" -> CompletableDeferred(
                    RemoteResponse.Success(request.requestId, request.kind, uploadBeginReply("upload-1", 0)),
                )
                "terminal.upload.chunk" -> pendingChunk
                "terminal.upload.cancel" -> pendingCancel
                else -> CompletableDeferred(RemoteResponse.Success(request.requestId, request.kind, byteArrayOf()))
            }
        }
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()
        val operation = async { client.uploadImages("tab-1", listOf(source)) }
        try {
            runCurrent()
            operation.cancel()
            runCurrent()
            advanceTimeBy(2_001)
            runCurrent()

            assertTrue(operation.isCompleted)
            assertTrue(operation.isCancelled)
            assertEquals(listOf(pendingCancel), transport.abandonedRequests)
        } finally {
            pendingCancel.complete(RemoteResponse.Success(99, "terminal.upload.cancel", uploadSuccessReply()))
            operation.cancelAndJoin()
            cleanupUploadSources(source)
            client.lock()
        }
    }

    @Test
    fun losingFocusBetweenUploadOperationsCancelsTheBoundUpload() = runTest {
        val transport = FakeRemoteTransport()
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        val source = uploadSource("one", byteArrayOf(1))
        transport.responseFor = { request ->
            when (request.kind) {
                "terminal.upload.begin" -> {
                    client.acceptForTest(
                        RemoteServerEvent.FocusChanged("tab-1", "attachment-1", FocusOwner.Other, TerminalSize(80, 24)),
                    )
                    CompletableDeferred(
                        RemoteResponse.Success(request.requestId, request.kind, uploadBeginReply("upload-1", 0)),
                    )
                }
                "terminal.upload.cancel" -> CompletableDeferred(
                    RemoteResponse.Success(request.requestId, request.kind, uploadSuccessReply()),
                )
                else -> CompletableDeferred(RemoteResponse.Success(request.requestId, request.kind, byteArrayOf()))
            }
        }
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()

        val operation = async { client.uploadImages("tab-1", listOf(source)) }
        advanceUntilIdle()

        assertTrue(operation.await().isFailure)
        assertEquals(
            listOf("terminal.upload.begin", "terminal.upload.cancel"),
            transport.requests.map(RemoteRequest::kind),
        )
        assertTrue(transport.abandonedRequests.isEmpty())
        cleanupUploadSources(source)
        client.lock()
    }

    @Test
    fun cancellingAnUploadCancelsBegunServerWorkWithoutDeletingTheDraftFile() = runTest {
        val transport = FakeRemoteTransport()
        val client = uploadClient(transport, this, StandardTestDispatcher(testScheduler))
        val source = uploadSource("one", byteArrayOf(1, 2, 3))
        val pendingChunk = CompletableDeferred<RemoteResponse>()
        transport.responseFor = { request ->
            when (request.kind) {
                "terminal.upload.begin" -> CompletableDeferred(
                    RemoteResponse.Success(request.requestId, request.kind, uploadBeginReply("upload-1", 0)),
                )
                "terminal.upload.chunk" -> pendingChunk
                "terminal.upload.cancel" -> CompletableDeferred(
                    RemoteResponse.Success(request.requestId, request.kind, uploadSuccessReply()),
                )
                else -> CompletableDeferred(RemoteResponse.Success(request.requestId, request.kind, byteArrayOf()))
            }
        }
        client.connect()
        client.selectTab("tab-1")
        advanceUntilIdle()
        client.grantUploadFocus()
        transport.requests.clear()

        val operation = async { client.uploadImages("tab-1", listOf(source)) }
        runCurrent()
        operation.cancelAndJoin()
        advanceUntilIdle()

        assertTrue(operation.isCancelled)
        assertEquals(
            listOf("terminal.upload.begin", "terminal.upload.chunk", "terminal.upload.cancel"),
            transport.requests.map(RemoteRequest::kind),
        )
        assertTrue(source.file.exists())
        cleanupUploadSources(source)
        client.lock()
    }
}

private fun uploadClient(
    transport: RemoteTransport,
    scope: kotlinx.coroutines.CoroutineScope,
    dispatcher: kotlinx.coroutines.CoroutineDispatcher,
) = RemoteClient(
    transportFactory = { transport },
    screenStore = DefaultTerminalScreenStore(),
    isUnlocked = { true },
    scope = scope,
    dispatcher = dispatcher,
)

private fun RemoteClient.grantUploadFocus() {
    acceptForTest(
        RemoteServerEvent.FocusChanged("tab-1", "attachment-1", FocusOwner.Self, TerminalSize(80, 24)),
    )
}

private fun uploadSource(id: String, bytes: ByteArray): RemoteUploadSource {
    val file = Files.createTempFile("aiterm-upload-$id-", ".jpg").toFile()
    file.writeBytes(bytes)
    return RemoteUploadSource(id, file, bytes.size.toLong(), ByteArray(32) { id.first().code.toByte() })
}

private fun sparseUploadSource(id: String, length: Long): RemoteUploadSource {
    val file = Files.createTempFile("aiterm-upload-$id-", ".jpg").toFile()
    RandomAccessFile(file, "rw").use { it.setLength(length) }
    return RemoteUploadSource(id, file, length, ByteArray(32) { id.first().code.toByte() })
}

private fun cleanupUploadSources(vararg sources: RemoteUploadSource) {
    sources.map(RemoteUploadSource::file).distinct().forEach { it.delete() }
}

private fun uploadBeginReply(uploadId: String, nextChunk: Int): ByteArray =
    cborMap(
        "upload_id" to uploadId,
        "next_chunk" to nextChunk,
    )

private fun uploadedPathReply(path: String): ByteArray = cborMap("path" to path)
private fun uploadSuccessReply(): ByteArray = byteArrayOf(0xa1.toByte(), 0x62, 0x6f, 0x6b, 0xf5.toByte())

private fun cborMap(vararg values: Pair<String, Any>): ByteArray {
    val output = java.io.ByteArrayOutputStream()
    fun header(major: Int, size: Int) {
        when {
            size < 24 -> output.write((major shl 5) or size)
            size <= 0xff -> {
                output.write((major shl 5) or 24)
                output.write(size)
            }
            else -> error("test fixture only supports short values")
        }
    }
    fun text(value: String) {
        val bytes = value.encodeToByteArray()
        header(3, bytes.size)
        output.write(bytes)
    }
    header(5, values.size)
    values.forEach { (key, value) ->
        text(key)
        when (value) {
            is String -> text(value)
            is Int -> header(0, value)
            else -> error("unsupported test fixture value")
        }
    }
    return output.toByteArray()
}

@OptIn(ExperimentalSerializationApi::class)
@Serializable
private data class UploadBeginWire(
    @SerialName("tab_id") val tabId: String,
    @SerialName("attachment_id") val attachmentId: String,
    @SerialName("submission_id") val submissionId: String,
    @SerialName("submission_count") val submissionCount: Int,
    @SerialName("member_index") val memberIndex: Int,
    @SerialName("submission_bytes") val submissionBytes: Long,
    val length: Long,
    @SerialName("media_type") val mediaType: String,
    @ByteString val sha256: ByteArray,
)

@OptIn(ExperimentalSerializationApi::class)
@Serializable
private data class UploadChunkWire(
    @SerialName("upload_id") val uploadId: String,
    val index: Int,
    @ByteString val data: ByteArray,
)

@OptIn(ExperimentalSerializationApi::class)
private val uploadCbor = Cbor {
    ignoreUnknownKeys = false
    useDefiniteLengthEncoding = true
}

@OptIn(ExperimentalSerializationApi::class)
private fun decodeUploadBegin(payload: ByteArray): UploadBeginWire =
    uploadCbor.decodeFromByteArray(UploadBeginWire.serializer(), payload)

@OptIn(ExperimentalSerializationApi::class)
private fun decodeUploadChunk(payload: ByteArray): UploadChunkWire =
    uploadCbor.decodeFromByteArray(UploadChunkWire.serializer(), payload)

private fun snapshotChunk(
    transferId: String,
    tabId: String,
    attachmentId: String,
    text: String,
) = RemoteServerEvent.TerminalChunk(
    TerminalTransferChunk(
        transferId = transferId,
        tabId = tabId,
        attachmentId = attachmentId,
        kind = TerminalTransferKind.Snapshot,
        baseRevision = 1,
        finalRevision = 1,
        rowStart = 0,
        rowEnd = 1,
        index = 0,
        total = 1,
        requestId = 0,
        part = TerminalTransferPart.Snapshot(
            cols = text.length,
            rows = 1,
            visible = listOf(ScreenRow(text.map { ScreenCell(it.toString()) })),
            cursor = CursorState(0, 0, true),
            modes = com.adroited.aiterm.terminal.TerminalModes(),
        ),
    ),
)

private fun scrollbackChunk(
    requestId: Long,
    text: String,
    tabId: String = "tab-1",
    attachmentId: String = "attachment-1",
) = RemoteServerEvent.TerminalChunk(
    TerminalTransferChunk(
        transferId = "history-$requestId",
        tabId = tabId,
        attachmentId = attachmentId,
        kind = TerminalTransferKind.Scrollback,
        baseRevision = 1,
        finalRevision = 1,
        rowStart = 0,
        rowEnd = 1,
        index = 0,
        total = 1,
        requestId = requestId,
        part = TerminalTransferPart.Scrollback(
            listOf(ScreenRow(text.map { ScreenCell(it.toString()) })),
        ),
    ),
)

private class FakeRemoteTransport(private val onClose: () -> Unit = {}) : RemoteTransport {
    override val events = MutableSharedFlow<RemoteServerEvent>(extraBufferCapacity = 8)
    val requests = mutableListOf<RemoteRequest>()
    val abandonedRequests = mutableListOf<Deferred<RemoteResponse>>()
    var responseFor: ((RemoteRequest) -> Deferred<RemoteResponse>)? = null
    var closed = false

    override suspend fun connect() = Unit

    private var nextRequestId = 1L

    override fun request(
        kind: String,
        payload: ByteArray,
        onAssigned: (Long) -> Unit,
    ): Deferred<RemoteResponse> {
        val request = RemoteRequest(nextRequestId++, kind, payload)
        onAssigned(request.requestId)
        requests += request
        if (request.kind == "terminal.attach") {
            return CompletableDeferred(
                RemoteResponse.Success(
                    request.requestId,
                    request.kind,
                    attachedPayload("tab-1", "attachment-1"),
                ),
            )
        }
        return responseFor?.invoke(request)
            ?: CompletableDeferred(RemoteResponse.Success(request.requestId, request.kind, byteArrayOf()))
    }

    override fun requestBatch(requests: List<RemoteRequestInput>): List<Deferred<RemoteResponse>>? =
        requests.map { input -> request(input.kind, input.payload, input.onAssigned) }

    override fun abandonRequest(request: Deferred<RemoteResponse>) {
        abandonedRequests += request
    }

    override fun close() {
        onClose()
        closed = true
    }
}

private class FailingRemoteTransport : RemoteTransport {
    override val events = MutableSharedFlow<RemoteServerEvent>()
    override suspend fun connect(): Unit = throw RemoteProtocolException("offline")
    override fun request(kind: String, payload: ByteArray, onAssigned: (Long) -> Unit) =
        CompletableDeferred<RemoteResponse>().also {
            it.completeExceptionally(RemoteProtocolException("offline"))
        }
    override fun requestBatch(requests: List<RemoteRequestInput>): List<Deferred<RemoteResponse>>? = null
    override fun close() = Unit
}

private class DeferredRemoteTransport(connectImmediately: Boolean = true) : RemoteTransport {
    override val events = MutableSharedFlow<RemoteServerEvent>(extraBufferCapacity = 8)
    val requests = mutableListOf<RemoteRequest>()
    val allowConnect = CompletableDeferred<Unit>().also { if (connectImmediately) it.complete(Unit) }
    private val attaches = ArrayDeque<Pair<RemoteRequest, CompletableDeferred<RemoteResponse>>>()
    private var nextRequestId = 1L
    var closed = false

    override suspend fun connect() {
        allowConnect.await()
    }

    override fun request(
        kind: String,
        payload: ByteArray,
        onAssigned: (Long) -> Unit,
    ): CompletableDeferred<RemoteResponse> {
        val request = RemoteRequest(nextRequestId++, kind, payload)
        onAssigned(request.requestId)
        requests += request
        if (request.kind != "terminal.attach") {
            return CompletableDeferred(RemoteResponse.Success(request.requestId, request.kind, byteArrayOf()))
        }
        val response = CompletableDeferred<RemoteResponse>()
        attaches += request to response
        return response
    }

    override fun requestBatch(requests: List<RemoteRequestInput>): List<Deferred<RemoteResponse>>? =
        requests.map { input -> request(input.kind, input.payload, input.onAssigned) }

    fun pendingAttachCount(): Int = attaches.size

    fun completeNextAttach(tabId: String, attachmentId: String) {
        val (request, response) = attaches.removeFirst()
        response.complete(RemoteResponse.Success(request.requestId, request.kind, attachedPayload(tabId, attachmentId)))
    }

    override fun close() {
        closed = true
    }

}

private class GatedEventsRemoteTransport(
    private val delegate: RemoteTransport,
    private val releaseEvents: CompletableDeferred<Unit>,
) : RemoteTransport {
    override val events = flow {
        releaseEvents.await()
        emitAll(delegate.events)
    }

    override suspend fun connect() = delegate.connect()

    override fun request(
        kind: String,
        payload: ByteArray,
        onAssigned: (Long) -> Unit,
    ) = delegate.request(kind, payload, onAssigned)

    override fun requestBatch(requests: List<RemoteRequestInput>) = delegate.requestBatch(requests)

    override suspend fun completeAttachment(requestId: Long, publishEvents: Boolean) =
        delegate.completeAttachment(requestId, publishEvents)

    override fun close() = delegate.close()
}

private fun attachedPayload(tabId: String, attachmentId: String): ByteArray {
    fun text(value: String): String {
        val bytes = value.encodeToByteArray()
        require(bytes.size < 24)
        return (0x60 + bytes.size).toString(16).padStart(2, '0') + bytes.joinToString("") {
            it.toUByte().toString(16).padStart(2, '0')
        }
    }
    val encoded = "a4" + text("tab_id") + text(tabId) +
        text("attachment_id") + text(attachmentId) +
        text("has_focus") + "f4" + text("title") + text(tabId)
    return encoded.chunked(2).map { it.toInt(16).toByte() }.toByteArray()
}

@Serializable
private data class TestConversationReply(val messages: List<RemotePreviewMessage>)

@OptIn(ExperimentalSerializationApi::class)
private fun conversationReply(text: String): ByteArray = Cbor {
    encodeDefaults = true
    ignoreUnknownKeys = false
    useDefiniteLengthEncoding = true
}.encodeToByteArray(
    TestConversationReply.serializer(),
    TestConversationReply(listOf(RemotePreviewMessage("assistant", text))),
)

private fun roundThreeScreen() = ScreenSnapshot(
    tabId = "tab-1",
    revision = 1,
    cols = 1,
    rows = 1,
    visible = listOf(ScreenRow(listOf(ScreenCell("x")))),
    cursor = CursorState(0, 0, true),
)

private fun roundThreeAuthenticatedSocket() = RoundThreeBinarySocket().apply {
    incoming.trySend(PairingFrames.encode(AuthChallengeFrame(ByteArray(32) { 3 })))
    incoming.trySend(byteArrayOf(0xa1.toByte(), 0x64, 0x6b, 0x69, 0x6e, 0x64, 0x67, 0x61, 0x75, 0x74, 0x68, 0x2e, 0x6f, 0x6b))
}

private fun roundThreeBusyError() = byteArrayOf(
    0xa2.toByte(),
    0x64, 0x63, 0x6f, 0x64, 0x65,
    0x64, 0x62, 0x75, 0x73, 0x79,
    0x67, 0x6d, 0x65, 0x73, 0x73, 0x61, 0x67, 0x65,
    0x64, 0x77, 0x61, 0x69, 0x74,
)

private fun roundThreeAuthenticatedTransport(
    socket: RoundThreeBinarySocket,
    scope: kotlinx.coroutines.CoroutineScope,
    dispatcher: kotlinx.coroutines.CoroutineDispatcher,
) = AuthenticatedRemoteTransport(
    desktop = PairedDesktop(
        deviceId = "device-1",
        displayName = "Desktop",
        hosts = listOf("desktop.local"),
        port = 43871,
        serverSpkiFingerprint = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        lastSeenEpochMillis = null,
    ),
    deviceKeys = object : DeviceKeys {
        override fun devicePublicKey(): ByteArray = ByteArray(33)
        override fun signChallenge(nonce: ByteArray): ByteArray = byteArrayOf(1, 2, 3)
    },
    appLock = AppLock(clock = { 0L }),
    dialer = object : RemoteSocketDialer {
        override suspend fun open(desktop: PairedDesktop): RemoteBinarySocket = socket
    },
    scope = scope,
    dispatcher = dispatcher,
)

private class RoundThreeBinarySocket : RemoteBinarySocket {
    val incoming = Channel<ByteArray>(Channel.UNLIMITED)
    val sentFrames = mutableListOf<ByteArray>()
    var closed = false

    override suspend fun receive(): ByteArray = incoming.receive()
    override fun send(bytes: ByteArray): Boolean {
        sentFrames += bytes.copyOf()
        return true
    }
    override fun close() {
        closed = true
        incoming.close()
    }
}
