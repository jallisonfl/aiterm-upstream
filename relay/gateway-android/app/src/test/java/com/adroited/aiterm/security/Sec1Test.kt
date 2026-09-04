package com.adroited.aiterm.security

import java.security.KeyPairGenerator
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The desktop expects a 33-byte SEC1 compressed point, not a Java-flavoured
 * X.509 encoding, so the conversion is pinned down here.
 */
class Sec1Test {

    private fun p256KeyPair() = KeyPairGenerator.getInstance("EC").apply {
        initialize(ECGenParameterSpec("secp256r1"))
    }.generateKeyPair()

    @Test
    fun compressedPoint_is33BytesWithAParityPrefix() {
        repeat(20) {
            val encoded = (p256KeyPair().public as ECPublicKey).compressedSec1()

            assertEquals(33, encoded.size)
            assertTrue(
                "prefix ${encoded[0]} is not a compressed-point marker",
                encoded[0] == 0x02.toByte() || encoded[0] == 0x03.toByte(),
            )
        }
    }

    @Test
    fun compressedPoint_carriesTheAffineXCoordinate() {
        val key = p256KeyPair().public as ECPublicKey
        val encoded = key.compressedSec1()
        val raw = key.w.affineX.toByteArray()
        // BigInteger.toByteArray() is signed and variable width; SEC1 is a fixed
        // 32-byte big-endian field element.
        val expectedX = when {
            raw.size > 32 -> raw.copyOfRange(raw.size - 32, raw.size)
            else -> ByteArray(32 - raw.size) + raw
        }

        assertArrayEquals(expectedX, encoded.copyOfRange(1, 33))
    }

    @Test
    fun parityPrefix_followsTheYCoordinate() {
        repeat(20) {
            val key = p256KeyPair().public as ECPublicKey
            val expected = if (key.w.affineY.testBit(0)) 0x03.toByte() else 0x02.toByte()

            assertEquals(expected, key.compressedSec1()[0])
        }
    }
}
