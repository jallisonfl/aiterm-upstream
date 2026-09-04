package com.adroited.aiterm.pairing

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class PairingFramesTest {

    @Test
    fun requestFrame_matchesTheDesktopCborShape() {
        val encoded = PairingFrames.encode(
            PairRequestFrame(
                enrollmentSecret = byteArrayOf(1, 2),
                deviceName = "Pixel",
                publicKey = byteArrayOf(2, 3, 4),
            ),
        )

        assertArrayEquals(
            hex(
                "a4" +
                    "646b696e646c706169722e72657175657374" +
                    "71656e726f6c6c6d656e745f736563726574420102" +
                    "6b6465766963655f6e616d6565506978656c" +
                    "6a7075626c69635f6b657943020304",
            ),
            encoded,
        )
    }

    @Test
    fun relayAuthorizationProof_roundTripsAsBoundedByteStrings() {
        val authority = ByteArray(33) { (it + 1).toByte() }
        val signature = ByteArray(70) { (it + 2).toByte() }
        val decoded = PairingFrames.decode(
            PairingFrames.encode(
                PairRequestFrame(
                    enrollmentSecret = ByteArray(32) { 3 },
                    deviceName = "Pixel",
                    publicKey = ByteArray(33) { 4 },
                    relayAuthorityPublicKey = authority,
                    relaySignatureDer = signature,
                ),
            ),
        ) as PairRequestFrame

        assertArrayEquals(authority, decoded.relayAuthorityPublicKey)
        assertArrayEquals(signature, decoded.relaySignatureDer)
    }

    @Test
    fun desktopResponseFixtures_decodeWithoutUsingTheEncoderUnderTest() {
        assertEquals(
            PairPendingFrame("request-1"),
            PairingFrames.decode(
                hex("a2646b696e646c706169722e70656e64696e676a726571756573745f696469726571756573742d31"),
            ),
        )
        assertEquals(
            PairApprovedFrame("device-42"),
            PairingFrames.decode(
                hex("a2646b696e646d706169722e617070726f766564696465766963655f6964696465766963652d3432"),
            ),
        )
        assertEquals(
            PairDeniedFrame(),
            PairingFrames.decode(hex("a1646b696e646b706169722e64656e696564")),
        )
        assertEquals(
            PairExpiredFrame(),
            PairingFrames.decode(hex("a1646b696e646c706169722e65787069726564")),
        )
    }

    @Test
    fun openingChallengeFixture_requiresExactlyA32ByteNonce() {
        val frame = PairingFrames.decode(
            hex(
                "a2" +
                    "646b696e646e617574682e6368616c6c656e6765" +
                    "656e6f6e63655820" +
                    "000102030405060708090a0b0c0d0e0f" +
                    "101112131415161718191a1b1c1d1e1f",
            ),
        )

        assertTrue(frame is AuthChallengeFrame)
        assertArrayEquals(ByteArray(32) { it.toByte() }, (frame as AuthChallengeFrame).nonce)
    }

    @Test
    fun malformedExtraOrDuplicateChallengeFields_areRejected() {
        val invalidChallenges = listOf(
            // 31-byte nonce.
            "a2" +
                "646b696e646e617574682e6368616c6c656e6765" +
                "656e6f6e6365581f" + "00".repeat(31),
            // Unknown field.
            "a3" +
                "646b696e646e617574682e6368616c6c656e6765" +
                "656e6f6e63655820" + "00".repeat(32) +
                "656578747261f5",
            // Duplicate nonce map key.
            "a3" +
                "646b696e646e617574682e6368616c6c656e6765" +
                "656e6f6e63655820" + "00".repeat(32) +
                "656e6f6e63655820" + "01".repeat(32),
        )

        invalidChallenges.forEach { bytes ->
            assertThrows(PairingProtocolException::class.java) {
                PairingFrames.decode(hex(bytes))
            }
        }
    }

    @Test
    fun unknownOrMalformedFrames_areRejected() {
        assertThrows(PairingProtocolException::class.java) {
            PairingFrames.decode(hex("a1646b696e646c706169722e756e6b6e6f776e"))
        }
        assertThrows(PairingProtocolException::class.java) {
            PairingFrames.decode(ByteArray(0))
        }
        assertThrows(PairingProtocolException::class.java) {
            // A malformed UTF-8 map key must stay inside the explicit
            // protocol-failure boundary rather than escaping the listener.
            PairingFrames.decode(hex("a161fff5"))
        }
        assertThrows(PairingProtocolException::class.java) {
            PairingFrames.decode(
                hex(
                    "a3" +
                        "646b696e646c706169722e70656e64696e67" +
                        "6a726571756573745f69646473616d65" +
                        "6b6465766963655f6e616d656473616d65",
                ),
            )
        }
    }

    private fun hex(value: String): ByteArray =
        value.chunked(2).map { it.toInt(16).toByte() }.toByteArray()
}
