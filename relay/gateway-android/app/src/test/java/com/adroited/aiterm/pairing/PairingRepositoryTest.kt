package com.adroited.aiterm.pairing

import com.adroited.aiterm.security.SpkiFingerprint
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.async
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.Response
import okhttp3.TlsVersion
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okhttp3.tls.HandshakeCertificates
import okhttp3.tls.HeldCertificate
import okio.ByteString
import okio.ByteString.Companion.toByteString
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * Enrollment is the only moment this client decides who to trust, so these
 * tests are written against the failure modes rather than the happy path: a
 * payload the phone cannot vouch for must be refused before a socket exists,
 * and a server whose key is not the pinned one must never see the secret.
 */
class PairingRepositoryTest {

    private lateinit var server: MockWebServer
    private lateinit var serverCertificate: HeldCertificate
    private lateinit var store: FakePairedDesktopStore
    private lateinit var deviceKeys: FakeDeviceKeys

    private val scannedAt = 1_700_000_000_000L
    private val deviceName = "Pixel 9"

    @Before
    fun setUp() {
        serverCertificate = HeldCertificate.Builder()
            .addSubjectAlternativeName("localhost")
            .ecdsa256()
            .build()
        val serverCertificates = HandshakeCertificates.Builder()
            .heldCertificate(serverCertificate)
            .build()
        server = MockWebServer()
        server.useHttps(serverCertificates.sslSocketFactory())
        server.start()
        store = FakePairedDesktopStore()
        deviceKeys = FakeDeviceKeys()
    }

    @After
    fun tearDown() {
        server.close()
    }

    // ---- Payload validation: everything here happens before a socket exists ----

    @Test
    fun unknownPayloadVersion_isRejectedOutright() {
        val result = PairingPayload.parse(pairingUri(version = "4"), scannedAt)

        assertEquals(
            PairingPayloadResult.Rejected(PairingFailure.UNSUPPORTED_VERSION),
            result,
        )
    }

    @Test
    fun combinedQrFields_areSkippedNotRejected() {
        // A combined QR carries a second listener's payload under its own
        // names (tp/tt/tf/z). This app reads only its own fields; the rest
        // must be skipped, not treated as malformed.
        val combined = pairingUri() + "&tp=8877&tt=sometoken&tf=${"ab".repeat(32)}&z=${"cd".repeat(32)}"
        val result = PairingPayload.parse(combined, scannedAt)
        assertTrue(
            "expected combined payload to parse, got $result",
            result is PairingPayloadResult.Parsed,
        )
        assertEquals(8443, (result as PairingPayloadResult.Parsed).payload.port)
    }

    @Test
    fun malformedPayloads_areRejected() {
        val badPayloads = mapOf(
            "wrong scheme" to "https://example.com/pair?v=1",
            "no version" to pairingUri().replace("v=1&", ""),
            "no host" to pairingUri(hosts = emptyList()),
            "empty host" to pairingUri(hosts = listOf("")),
            "port out of range" to pairingUri(port = 70000),
            "fingerprint not base64url" to pairingUri(fingerprint = "not base64!!"),
            "fingerprint wrong length" to pairingUri(fingerprint = ByteArray(16).toBase64Url()),
            "secret wrong length" to pairingUri(secret = ByteArray(8)),
            "no name" to pairingUri(name = ""),
            "duplicate port" to pairingUri() + "&p=9999",
            "duplicate version" to pairingUri() + "&v=1",
            "duplicate fingerprint" to pairingUri() + "&f=${ByteArray(32) { 9 }.toBase64Url()}",
            "duplicate secret" to pairingUri() + "&s=${ByteArray(32) { 9 }.toBase64Url()}",
            "duplicate name" to pairingUri() + "&n=Other",
            "fingerprint padding" to pairingUri(fingerprint = ByteArray(32) { 7 }.toBase64Url() + "="),
            "malformed percent escape" to pairingUri(name = "Bad%ZZname"),
            "invalid UTF-8 escape" to pairingUri(name = "%FF"),
            "path after pair authority" to pairingUri().replace("aiterm://pair?", "aiterm://pair/extra?"),
            "fragment" to pairingUri() + "#ignored",
            "host with whitespace" to pairingUri(hosts = listOf("bad host")),
            "host with user info" to pairingUri(hosts = listOf("user@desktop.local")),
            "host with a path" to pairingUri(hosts = listOf("desktop.local/path")),
            "host with its own port" to pairingUri(hosts = listOf("desktop.local:9443")),
        )

        badPayloads.forEach { (why, uri) ->
            val result = PairingPayload.parse(uri, scannedAt)
            assertTrue(
                "expected $why to be rejected, got $result",
                result is PairingPayloadResult.Rejected,
            )
        }
    }

    @Test
    fun validPayload_keepsHostOrderAndDecodesTheDisplayName() {
        val payload = parsedPayload(
            pairingUri(hosts = listOf("192.168.1.20", "fe80::1", "desktop.local")),
            scannedAt,
        )

        assertEquals(listOf("192.168.1.20", "fe80::1", "desktop.local"), payload.hosts)
        assertEquals("Matt's desktop", payload.desktopName)
    }

    @Test
    fun relayPayload_requiresVersionTwoAndACompleteEndpoint() {
        val parsed = parsedPayload(
            pairingUri(
                version = "2",
                relayHost = "desk-1234.relay.example.com",
                relayPort = 443,
            ),
            scannedAt,
        )
        assertEquals(PairingEndpoint("desk-1234.relay.example.com", 443), parsed.relayEndpoint)

        assertTrue(
            PairingPayload.parse(
                pairingUri(version = "1", relayHost = "desk-1234.relay.example.com", relayPort = 443),
                scannedAt,
            ) is PairingPayloadResult.Rejected,
        )
        assertTrue(
            PairingPayload.parse(
                pairingUri(version = "2", relayHost = "desk-1234.relay.example.com"),
                scannedAt,
            ) is PairingPayloadResult.Rejected,
        )
    }

    @Test
    fun versionThreeRelayPairing_isAuthorizedByTheDedicatedPhoneKey() = runBlocking {
        val digest = ByteArray(32) { (it + 11).toByte() }
        val payload = parsedPayload(
            pairingUri(
                version = "3",
                relayHost = "desktop-1234.relay.example.com",
                relayPort = 443,
                relayAuthorizationDigest = digest,
            ),
            scannedAt,
        )
        val transport = RecordingPairingTransport(
            outcomes = mapOf("localhost" to EnrollmentOutcome.Approved("device-v3")),
        )
        val authority = FakeDeviceKeys()
        val result = PairingRepository(
            transport = transport,
            deviceKeys = deviceKeys,
            store = store,
            relayAuthorityKeys = authority,
        ).pair(payload, deviceName, scannedAt)

        assertTrue(result is PairingResult.Paired)
        assertEquals(1, authority.publicKeyRequests)
        assertEquals(33, transport.lastRelayAuthorityPublicKey?.size)
        assertTrue(transport.lastRelaySignatureDer?.size in 8..80)
    }

    @Test
    fun plusInDisplayName_isAUriLiteralAndNotFormEncodedSpace() {
        val payload = parsedPayload(pairingUri(name = "Workshop+PC"), scannedAt)

        assertEquals("Workshop+PC", payload.desktopName)
    }

    @Test
    fun pairingPayload_neverPrintsTheEnrollmentSecret() {
        val secret = ByteArray(32) { 0x41 }
        val payload = parsedPayload(pairingUri(secret = secret), scannedAt)

        assertFalse(payload.toString().contains(secret.toBase64Url()))
        assertFalse(payload.secret.toString().contains(secret.toBase64Url()))
    }

    @Test
    fun expiredPayload_isRejectedBeforeAnyNetworkCall() = runBlocking {
        val payload = parsedPayload(
            pairingUri(hosts = listOf(server.hostName), port = server.port),
            scannedAt,
        )
        val repository = repositoryWith(OkHttpPairingTransport())

        val result = repository.pair(
            payload = payload,
            deviceName = deviceName,
            // one millisecond past the five-minute window
            nowEpochMillis = scannedAt + PairingPayload.LIFETIME_MILLIS + 1,
        )

        assertEquals(PairingResult.Rejected(PairingFailure.EXPIRED_PAYLOAD), result)
        assertEquals(0, server.requestCount)
        assertEquals(emptyList<PairedDesktop>(), store.all())
        assertEquals(0, deviceKeys.publicKeyRequests)
    }

    @Test
    fun payloadAtTheFiveMinuteBoundary_isAlreadyExpired() = runBlocking {
        val payload = parsedPayload(
            pairingUri(hosts = listOf(server.hostName), port = server.port),
            scannedAt,
        )

        val result = repositoryWith(RecordingPairingTransport(emptyMap())).pair(
            payload = payload,
            deviceName = deviceName,
            nowEpochMillis = scannedAt + PairingPayload.LIFETIME_MILLIS,
        )

        assertEquals(PairingResult.Rejected(PairingFailure.EXPIRED_PAYLOAD), result)
        assertEquals(0, deviceKeys.publicKeyRequests)
    }

    @Test
    fun consumedPayload_cannotBeSubmittedAgain() = runBlocking {
        val payload = parsedPayload(pairingUri(), scannedAt)
        val transport = RecordingPairingTransport(
            outcomes = mapOf("localhost" to EnrollmentOutcome.Denied),
        )
        val repository = repositoryWith(transport)

        assertEquals(
            PairingResult.Rejected(PairingFailure.DENIED_BY_DESKTOP),
            repository.pair(payload, deviceName, scannedAt),
        )
        assertEquals(
            PairingResult.Rejected(PairingFailure.CONSUMED_PAYLOAD),
            repository.pair(payload, deviceName, scannedAt),
        )
        assertEquals(listOf("localhost"), transport.attempted.map { it.host })
        assertEquals(1, deviceKeys.publicKeyRequests)
    }

    // ---- Pinning ----

    @Test
    fun certificateFingerprintMismatch_sendsNoSecretAndPersistsNothing() = runBlocking {
        val someoneElsesKey = HeldCertificate.Builder().ecdsa256().build().certificate
        val payload = parsedPayload(
            pairingUri(
                hosts = listOf(server.hostName),
                port = server.port,
                fingerprint = SpkiFingerprint.of(someoneElsesKey),
            ),
            scannedAt,
        )
        server.enqueue(MockResponse.Builder().webSocketUpgrade(RecordingServer()).build())

        val result = repositoryWith(OkHttpPairingTransport()).pair(payload, deviceName, scannedAt)

        assertEquals(PairingResult.Rejected(PairingFailure.FINGERPRINT_MISMATCH), result)
        // The TLS handshake must have been abandoned before an HTTP request,
        // which is the only way the secret could have reached the peer.
        assertEquals(0, server.requestCount)
        assertEquals(emptyList<PairedDesktop>(), store.all())
    }

    @Test
    fun fingerprintMismatchOnTheFirstHost_doesNotFallThroughToOtherCandidates() = runBlocking {
        val someoneElsesKey = HeldCertificate.Builder().ecdsa256().build().certificate
        val payload = parsedPayload(
            pairingUri(
                hosts = listOf(server.hostName, "10.0.0.9"),
                port = server.port,
                fingerprint = SpkiFingerprint.of(someoneElsesKey),
            ),
            scannedAt,
        )
        val transport = RecordingPairingTransport(
            outcomes = emptyMap(),
            delegate = OkHttpPairingTransport(),
        )

        val result = repositoryWith(transport).pair(payload, deviceName, scannedAt)

        assertEquals(PairingResult.Rejected(PairingFailure.FINGERPRINT_MISMATCH), result)
        assertEquals(listOf(server.hostName), transport.attempted.map { it.host })
    }

    @Test
    fun hostnameMismatch_sendsNoSecretAndPersistsNothing() = runBlocking {
        val payload = parsedPayload(
            pairingUri(
                hosts = listOf("127.0.0.1"),
                port = server.port,
                fingerprint = SpkiFingerprint.of(serverCertificate.certificate),
            ),
            scannedAt,
        )
        server.enqueue(MockResponse.Builder().webSocketUpgrade(RecordingServer()).build())

        val result = repositoryWith(OkHttpPairingTransport()).pair(payload, deviceName, scannedAt)

        assertEquals(PairingResult.Rejected(PairingFailure.TLS_IDENTITY_MISMATCH), result)
        assertEquals(0, server.requestCount)
        assertEquals(emptyList<PairedDesktop>(), store.all())
    }

    // ---- Candidate hosts ----

    @Test
    fun hostCandidates_areTriedInPayloadOrder() = runBlocking {
        val payload = parsedPayload(
            pairingUri(hosts = listOf("first.local", "fe80::2", "third.local")),
            scannedAt,
        )
        val transport = RecordingPairingTransport(
            outcomes = mapOf("third.local" to EnrollmentOutcome.Approved("device-7")),
        )

        val result = repositoryWith(transport).pair(payload, deviceName, scannedAt)

        assertEquals(
            listOf("first.local", "fe80::2", "third.local"),
            transport.attempted.map { it.host },
        )
        assertTrue(result is PairingResult.Paired)
        // The address that worked is remembered first for the next reconnect.
        assertEquals("third.local", store.all().single().hosts.first())
    }

    @Test
    fun everyHostUnreachable_reportsUnreachableAndPersistsNothing() = runBlocking {
        val payload = parsedPayload(pairingUri(hosts = listOf("a.local", "b.local")), scannedAt)

        val result = repositoryWith(RecordingPairingTransport(emptyMap()))
            .pair(payload, deviceName, scannedAt)

        assertEquals(PairingResult.Rejected(PairingFailure.UNREACHABLE), result)
        assertEquals(emptyList<PairedDesktop>(), store.all())
    }

    @Test
    fun relayIsTriedAfterEveryDirectAddressAndIsRememberedSeparately() = runBlocking {
        val relayHost = "desk-1234.relay.example.com"
        val payload = parsedPayload(
            pairingUri(
                version = "2",
                hosts = listOf("192.168.1.20", "100.90.1.2"),
                relayHost = relayHost,
                relayPort = 443,
            ),
            scannedAt,
        )
        val transport = RecordingPairingTransport(
            outcomes = mapOf(relayHost to EnrollmentOutcome.Approved("device-7")),
        )

        val result = repositoryWith(transport).pair(payload, deviceName, scannedAt)

        assertTrue(result is PairingResult.Paired)
        assertEquals(listOf("192.168.1.20", "100.90.1.2", relayHost), transport.attempted.map { it.host })
        assertEquals(listOf("192.168.1.20", "100.90.1.2"), store.all().single().hosts)
        assertEquals(relayHost, store.all().single().relayHost)
        assertEquals(443, store.all().single().relayPort)
    }

    @Test
    fun disconnectAfterSecretWasSent_doesNotTryAnotherCandidate() = runBlocking {
        val payload = parsedPayload(
            pairingUri(hosts = listOf("first.local", "second.local")),
            scannedAt,
        )
        val transport = RecordingPairingTransport(
            outcomes = mapOf(
                "first.local" to EnrollmentOutcome.Indeterminate("connection closed"),
                "second.local" to EnrollmentOutcome.Approved("must-not-run"),
            ),
        )

        val result = repositoryWith(transport).pair(payload, deviceName, scannedAt)

        assertEquals(PairingResult.Rejected(PairingFailure.ENROLLMENT_STATE_UNKNOWN), result)
        assertEquals(listOf("first.local"), transport.attempted.map { it.host })
        assertEquals(emptyList<PairedDesktop>(), store.all())
    }

    @Test
    fun deadFirstCandidate_fallsThroughToTheLiveDesktop() = runBlocking {
        // 127.0.0.2 is on this machine's loopback but nothing listens there, so
        // it refuses instantly - the shape of a stale LAN address in the QR.
        val payload = parsedPayload(
            pairingUri(
                hosts = listOf("127.0.0.2", server.hostName),
                port = server.port,
                fingerprint = SpkiFingerprint.of(serverCertificate.certificate),
            ),
            scannedAt,
        )
        server.enqueue(MockResponse.Builder().webSocketUpgrade(RecordingServer()).build())

        val result = repositoryWith(OkHttpPairingTransport()).pair(payload, deviceName, scannedAt)

        assertTrue("expected a pairing, got $result", result is PairingResult.Paired)
        assertEquals(server.hostName, store.all().single().hosts.first())
    }

    @Test
    fun stalledFirstCandidate_timesOutBeforeChallengeThenFallsThroughWithOriginalSecret() =
        runBlocking {
            val secret = ByteArray(32) { (it + 41).toByte() }
            val payload = parsedPayload(
                pairingUri(
                    hosts = listOf("LOCALHOST", "localhost"),
                    port = server.port,
                    fingerprint = SpkiFingerprint.of(serverCertificate.certificate),
                    secret = secret,
                ),
                scannedAt,
            )
            val stalled = StalledOpeningServer()
            val live = RecordingServer()
            server.enqueue(MockResponse.Builder().webSocketUpgrade(stalled).build())
            server.enqueue(MockResponse.Builder().webSocketUpgrade(live).build())
            val transport = OkHttpPairingTransport(
                openingChallengeTimeoutMillis = 500,
                approvalTimeoutMillis = 5_000,
            )

            val result = withTimeoutOrNull(2_500) {
                repositoryWith(transport).pair(payload, deviceName, scannedAt)
            }

            assertTrue("expected fallback pairing, got $result", result is PairingResult.Paired)
            assertTrue(stalled.awaitOpen())
            assertFalse(
                "the stalled candidate received enrollment material",
                stalled.awaitRequest(100, TimeUnit.MILLISECONDS),
            )
            assertTrue(live.awaitRequestFrame())
            assertArrayEqualsBytes(secret, live.pairRequest!!.enrollmentSecret)
            assertEquals(2, server.requestCount)
            assertEquals("localhost", store.all().single().hosts.first())
        }

    @Test
    fun openingChallengeTimeout_isFallbackSafeOnlyWhileSecretRemainsUnclaimed() = runBlocking {
        val payload = parsedPayload(
            pairingUri(
                hosts = listOf(server.hostName),
                port = server.port,
                fingerprint = SpkiFingerprint.of(serverCertificate.certificate),
            ),
            scannedAt,
        )
        val stalled = StalledOpeningServer()
        server.enqueue(MockResponse.Builder().webSocketUpgrade(stalled).build())
        val transport = OkHttpPairingTransport(
            openingChallengeTimeoutMillis = 250,
            approvalTimeoutMillis = 5_000,
        )

        try {
            val outcome = withTimeoutOrNull(1_500) {
                transport.enroll(
                    endpoint = PairingEndpoint(server.hostName, server.port),
                    serverSpkiFingerprint = SpkiFingerprint.of(serverCertificate.certificate),
                    enrollmentSecret = payload.secret,
                    deviceName = deviceName,
                    devicePublicKey = deviceKeys.devicePublicKey(),
                    relayAuthorityPublicKey = null,
                    relaySignatureDer = null,
                    onPending = {},
                )
            }

            assertTrue(
                "expected fallback-safe timeout, got $outcome",
                outcome is EnrollmentOutcome.Unreachable,
            )
            assertTrue(stalled.awaitOpen())
            assertFalse(stalled.awaitRequest(100, TimeUnit.MILLISECONDS))
            assertTrue("timeout claimed the enrollment secret", payload.secret.isAvailable())
        } finally {
            payload.discard()
        }
    }

    // ---- Opening authentication challenge ----

    @Test
    fun enrollmentSecret_isNotSentUntilTheRustOpeningChallengeIsValidated() = runBlocking {
        val secret = ByteArray(32) { (it + 17).toByte() }
        val payload = parsedPayload(
            pairingUri(
                hosts = listOf(server.hostName),
                port = server.port,
                fingerprint = SpkiFingerprint.of(serverCertificate.certificate),
                secret = secret,
            ),
            scannedAt,
        )
        val recordingServer = ChallengeGateServer()
        server.enqueue(MockResponse.Builder().webSocketUpgrade(recordingServer).build())

        val pairing = async(Dispatchers.IO) {
            repositoryWith(OkHttpPairingTransport()).pair(payload, deviceName, scannedAt)
        }
        assertTrue(recordingServer.awaitOpen())
        assertFalse(
            "pair.request leaked before auth.challenge",
            recordingServer.awaitRequest(300, TimeUnit.MILLISECONDS),
        )

        recordingServer.sendChallenge()
        val result = pairing.await()

        assertTrue("expected approved pairing, got $result", result is PairingResult.Paired)
        assertTrue(recordingServer.awaitRequest(5, TimeUnit.SECONDS))
        assertArrayEqualsBytes(secret, recordingServer.pairRequest!!.enrollmentSecret)
    }

    @Test
    fun malformedOrOutOfOrderOpeningChallenge_sendsNoEnrollmentMaterial() = runBlocking {
        val openingFrames = listOf(
            malformedChallengeBytes().toByteString(),
            pendingFixtureBytes().toByteString(),
        )

        openingFrames.forEach { openingFrame ->
            val payload = parsedPayload(
                pairingUri(
                    hosts = listOf(server.hostName),
                    port = server.port,
                    fingerprint = SpkiFingerprint.of(serverCertificate.certificate),
                ),
                scannedAt,
            )
            val openingServer = OpeningFrameServer(openingFrame)
            server.enqueue(MockResponse.Builder().webSocketUpgrade(openingServer).build())

            val result = repositoryWith(OkHttpPairingTransport()).pair(payload, deviceName, scannedAt)

            assertEquals(PairingResult.Rejected(PairingFailure.PROTOCOL_ERROR), result)
            assertFalse(
                "pair.request leaked after an invalid opening frame",
                openingServer.awaitRequest(300, TimeUnit.MILLISECONDS),
            )
        }
    }

    @Test
    fun duplicateOpeningChallenge_isAProtocolFailureAndNeverPersists() = runBlocking {
        val payload = parsedPayload(
            pairingUri(
                hosts = listOf(server.hostName),
                port = server.port,
                fingerprint = SpkiFingerprint.of(serverCertificate.certificate),
            ),
            scannedAt,
        )
        val duplicateServer = DuplicateChallengeServer()
        server.enqueue(MockResponse.Builder().webSocketUpgrade(duplicateServer).build())

        val result = repositoryWith(OkHttpPairingTransport()).pair(payload, deviceName, scannedAt)

        assertEquals(PairingResult.Rejected(PairingFailure.PROTOCOL_ERROR), result)
        assertTrue(duplicateServer.awaitRequest())
        assertEquals(emptyList<PairedDesktop>(), store.all())
    }

    // ---- Approval ----

    @Test
    fun approvedPairing_persistsOnlyNonSecretMetadata() = runBlocking {
        val secret = ByteArray(32) { (it * 3).toByte() }
        val payload = parsedPayload(
            pairingUri(
                hosts = listOf(server.hostName),
                port = server.port,
                fingerprint = SpkiFingerprint.of(serverCertificate.certificate),
                secret = secret,
                name = "Workshop%20PC",
            ),
            scannedAt,
        )
        val recordingServer = RecordingServer()
        server.enqueue(MockResponse.Builder().webSocketUpgrade(recordingServer).build())

        val result = repositoryWith(OkHttpPairingTransport()).pair(payload, deviceName, scannedAt)

        assertTrue("expected a pairing, got $result", result is PairingResult.Paired)
        assertTrue(recordingServer.awaitRequestFrame())

        val request = recordingServer.pairRequest!!
        assertEquals(deviceName, request.deviceName)
        assertArrayEqualsBytes(secret, request.enrollmentSecret)
        assertEquals(33, request.publicKey.size)

        val stored = store.all().single()
        assertEquals("device-42", stored.deviceId)
        assertEquals("Workshop PC", stored.displayName)
        assertEquals(SpkiFingerprint.of(serverCertificate.certificate), stored.serverSpkiFingerprint)
        assertEquals(server.port, stored.port)
        assertEquals(server.hostName, stored.hosts.first())
        val serialised = PairedDesktopJson.encode(store.all())
        assertFalse(
            "the enrollment secret must never reach storage",
            serialised.contains(secret.toBase64Url()) || serialised.contains(secret.toHexString()),
        )
    }

    @Test
    fun deniedPairing_persistsNothing() = runBlocking {
        val payload = parsedPayload(
            pairingUri(
                hosts = listOf(server.hostName),
                port = server.port,
                fingerprint = SpkiFingerprint.of(serverCertificate.certificate),
            ),
            scannedAt,
        )
        server.enqueue(MockResponse.Builder().webSocketUpgrade(RecordingServer(approve = false)).build())

        val result = repositoryWith(OkHttpPairingTransport()).pair(payload, deviceName, scannedAt)

        assertEquals(PairingResult.Rejected(PairingFailure.DENIED_BY_DESKTOP), result)
        assertEquals(emptyList<PairedDesktop>(), store.all())
    }

    @Test
    fun desktopPairExpiredResponse_mapsToExpiredPayloadAndPersistsNothing() = runBlocking {
        val payload = parsedPayload(
            pairingUri(
                hosts = listOf(server.hostName),
                port = server.port,
                fingerprint = SpkiFingerprint.of(serverCertificate.certificate),
            ),
            scannedAt,
        )
        val expiredServer = ExpiredServer()
        server.enqueue(MockResponse.Builder().webSocketUpgrade(expiredServer).build())

        val result = repositoryWith(OkHttpPairingTransport()).pair(payload, deviceName, scannedAt)

        assertEquals(PairingResult.Rejected(PairingFailure.EXPIRED_PAYLOAD), result)
        assertTrue(expiredServer.awaitRequest())
        assertEquals(emptyList<PairedDesktop>(), store.all())
    }

    @Test
    fun approvalDeadline_startsWhenPairRequestMayHaveBeenSent() = runBlocking {
        val payload = parsedPayload(
            pairingUri(
                hosts = listOf(server.hostName),
                port = server.port,
                fingerprint = SpkiFingerprint.of(serverCertificate.certificate),
            ),
            scannedAt,
        )
        val delayed = DelayedApprovalServer(
            challengeDelayMillis = 250,
            decisionDelayMillis = 250,
        )
        server.enqueue(MockResponse.Builder().webSocketUpgrade(delayed).build())
        val transport = OkHttpPairingTransport(
            openingChallengeTimeoutMillis = 500,
            approvalTimeoutMillis = 400,
        )

        val result = repositoryWith(transport).pair(payload, deviceName, scannedAt)

        assertTrue("expected approved pairing, got $result", result is PairingResult.Paired)
        assertTrue(delayed.awaitRequest())
    }

    @Test
    fun pendingApproval_doesNotPersistUntilTheApprovedResponseArrives() = runBlocking {
        val payload = parsedPayload(pairingUri(), scannedAt)
        var pendingWasReported = false
        val transport = object : PairingTransport {
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
                if (enrollmentSecret.consume { Unit } is EnrollmentSecret.Consumption.AlreadyConsumed) {
                    return EnrollmentOutcome.ConsumedPayload
                }
                onPending()
                pendingWasReported = true
                assertEquals(emptyList<PairedDesktop>(), store.all())
                return EnrollmentOutcome.Approved("device-after-approval")
            }
        }

        val result = repositoryWith(transport).pair(
            payload = payload,
            deviceName = deviceName,
            nowEpochMillis = scannedAt,
            onAwaitingApproval = { assertEquals(emptyList<PairedDesktop>(), store.all()) },
        )

        assertTrue(pendingWasReported)
        assertTrue(result is PairingResult.Paired)
        assertEquals("device-after-approval", store.all().single().deviceId)
    }

    @Test
    fun storageFailureAfterApproval_isReportedExplicitly() = runBlocking {
        val payload = parsedPayload(pairingUri(), scannedAt)
        val failingStore = object : PairedDesktopStore {
            override fun all(): List<PairedDesktop> = emptyList()
            override fun save(desktop: PairedDesktop): Unit =
                throw PairedDesktopStoreException("private storage unavailable")
            override fun remove(deviceId: String) = Unit
        }
        val repository = PairingRepository(
            transport = RecordingPairingTransport(
                outcomes = mapOf("localhost" to EnrollmentOutcome.Approved("device-7")),
            ),
            deviceKeys = deviceKeys,
            store = failingStore,
        )

        val result = repository.pair(payload, deviceName, scannedAt)

        assertEquals(PairingResult.Rejected(PairingFailure.STORAGE_FAILURE), result)
    }

    @Test
    fun pairingRequest_reachesTheVersionedWebSocketPath() = runBlocking {
        val payload = parsedPayload(
            pairingUri(
                hosts = listOf(server.hostName),
                port = server.port,
                fingerprint = SpkiFingerprint.of(serverCertificate.certificate),
            ),
            scannedAt,
        )
        server.enqueue(MockResponse.Builder().webSocketUpgrade(RecordingServer()).build())

        repositoryWith(OkHttpPairingTransport()).pair(payload, deviceName, scannedAt)

        assertEquals("/v1/ws", server.takeRequest(5, TimeUnit.SECONDS)?.url?.encodedPath)
    }

    @Test
    fun pairingTransport_negotiatesTls13() = runBlocking {
        val payload = parsedPayload(
            pairingUri(
                hosts = listOf(server.hostName),
                port = server.port,
                fingerprint = SpkiFingerprint.of(serverCertificate.certificate),
            ),
            scannedAt,
        )
        server.enqueue(MockResponse.Builder().webSocketUpgrade(RecordingServer()).build())

        repositoryWith(OkHttpPairingTransport()).pair(payload, deviceName, scannedAt)

        assertEquals(
            TlsVersion.TLS_1_3,
            server.takeRequest(5, TimeUnit.SECONDS)?.handshake?.tlsVersion,
        )
    }

    private fun repositoryWith(transport: PairingTransport) =
        PairingRepository(transport = transport, deviceKeys = deviceKeys, store = store)

    private fun assertArrayEqualsBytes(expected: ByteArray, actual: ByteArray) =
        assertEquals(expected.toBase64Url(), actual.toBase64Url())

    private fun ByteArray.toHexString() = joinToString("") { "%02x".format(it) }

    /**
     * Stands in for the desktop gateway: it decodes the client's `pair.request`
     * and answers with the pending/approved pair the protocol specifies.
     */
    private class RecordingServer(private val approve: Boolean = true) : WebSocketListener() {

        var pairRequest: PairRequestFrame? = null
            private set

        private val received = CountDownLatch(1)

        override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
            val frame = PairingFrames.decode(bytes.toByteArray())
            if (frame is PairRequestFrame) {
                pairRequest = frame
                received.countDown()
                webSocket.send(PairingFrames.encode(PairPendingFrame("request-1")).toByteString())
                val answer = if (approve) PairApprovedFrame("device-42") else PairDeniedFrame()
                webSocket.send(PairingFrames.encode(answer).toByteString())
            }
        }

        override fun onOpen(webSocket: WebSocket, response: Response) {
            webSocket.send(validChallengeBytes().toByteString())
        }

        fun awaitRequestFrame(): Boolean = received.await(5, TimeUnit.SECONDS)
    }

    private class ChallengeGateServer : WebSocketListener() {
        @Volatile
        var pairRequest: PairRequestFrame? = null
            private set

        private val opened = CountDownLatch(1)
        private val received = CountDownLatch(1)
        private lateinit var socket: WebSocket

        override fun onOpen(webSocket: WebSocket, response: Response) {
            socket = webSocket
            opened.countDown()
        }

        override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
            val frame = PairingFrames.decode(bytes.toByteArray())
            if (frame is PairRequestFrame) {
                pairRequest = frame
                received.countDown()
                webSocket.send(pendingFixtureBytes().toByteString())
                webSocket.send(approvedFixtureBytes().toByteString())
            }
        }

        fun awaitOpen(): Boolean = opened.await(5, TimeUnit.SECONDS)

        fun awaitRequest(timeout: Long, unit: TimeUnit): Boolean = received.await(timeout, unit)

        fun sendChallenge() {
            check(opened.count == 0L)
            check(socket.send(validChallengeBytes().toByteString()))
        }
    }

    private class StalledOpeningServer : WebSocketListener() {
        private val opened = CountDownLatch(1)
        private val received = CountDownLatch(1)

        override fun onOpen(webSocket: WebSocket, response: Response) {
            opened.countDown()
        }

        override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
            received.countDown()
        }

        fun awaitOpen(): Boolean = opened.await(5, TimeUnit.SECONDS)

        fun awaitRequest(timeout: Long, unit: TimeUnit): Boolean = received.await(timeout, unit)
    }

    private class DelayedApprovalServer(
        private val challengeDelayMillis: Long,
        private val decisionDelayMillis: Long,
    ) : WebSocketListener() {
        private val received = CountDownLatch(1)

        override fun onOpen(webSocket: WebSocket, response: Response) {
            Thread.sleep(challengeDelayMillis)
            webSocket.send(validChallengeBytes().toByteString())
        }

        override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
            val frame = PairingFrames.decode(bytes.toByteArray())
            if (frame is PairRequestFrame) {
                received.countDown()
                webSocket.send(pendingFixtureBytes().toByteString())
                Thread.sleep(decisionDelayMillis)
                webSocket.send(approvedFixtureBytes().toByteString())
            }
        }

        fun awaitRequest(): Boolean = received.await(5, TimeUnit.SECONDS)
    }

    private class OpeningFrameServer(private val openingFrame: ByteString) : WebSocketListener() {
        private val received = CountDownLatch(1)

        override fun onOpen(webSocket: WebSocket, response: Response) {
            webSocket.send(openingFrame)
        }

        override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
            received.countDown()
        }

        fun awaitRequest(timeout: Long, unit: TimeUnit): Boolean = received.await(timeout, unit)
    }

    private class DuplicateChallengeServer : WebSocketListener() {
        private val received = CountDownLatch(1)

        override fun onOpen(webSocket: WebSocket, response: Response) {
            webSocket.send(validChallengeBytes().toByteString())
        }

        override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
            received.countDown()
            webSocket.send(validChallengeBytes().toByteString())
        }

        fun awaitRequest(): Boolean = received.await(5, TimeUnit.SECONDS)
    }

    private class ExpiredServer : WebSocketListener() {
        private val received = CountDownLatch(1)

        override fun onOpen(webSocket: WebSocket, response: Response) {
            webSocket.send(validChallengeBytes().toByteString())
        }

        override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
            received.countDown()
            webSocket.send(pendingFixtureBytes().toByteString())
            webSocket.send(expiredFixtureBytes().toByteString())
        }

        fun awaitRequest(): Boolean = received.await(5, TimeUnit.SECONDS)
    }
}

private fun validChallengeBytes(): ByteArray = hexBytes(
    "a2" +
        "646b696e646e617574682e6368616c6c656e6765" +
        "656e6f6e63655820" +
        "000102030405060708090a0b0c0d0e0f" +
        "101112131415161718191a1b1c1d1e1f",
)

private fun malformedChallengeBytes(): ByteArray = hexBytes(
    "a2" +
        "646b696e646e617574682e6368616c6c656e6765" +
        "656e6f6e6365581f" + "00".repeat(31),
)

private fun pendingFixtureBytes(): ByteArray =
    hexBytes("a2646b696e646c706169722e70656e64696e676a726571756573745f696469726571756573742d31")

private fun approvedFixtureBytes(): ByteArray =
    hexBytes("a2646b696e646d706169722e617070726f766564696465766963655f6964696465766963652d3432")

private fun expiredFixtureBytes(): ByteArray =
    hexBytes("a1646b696e646c706169722e65787069726564")

private fun hexBytes(value: String): ByteArray =
    value.chunked(2).map { it.toInt(16).toByte() }.toByteArray()
