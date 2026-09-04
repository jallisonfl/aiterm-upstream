package com.adroited.aiterm.security

import java.math.BigInteger
import java.security.interfaces.ECPublicKey

/** Byte width of a P-256 field element. */
private const val P256_FIELD_BYTES = 32

/**
 * The device public key in SEC1 compressed form: one parity byte followed by
 * the 32-byte x coordinate.
 *
 * Java only hands out X.509 SubjectPublicKeyInfo, which carries the curve OID
 * and an uncompressed point. The desktop stores a 33-byte device identity, so
 * the conversion has to happen here rather than on the wire.
 */
fun ECPublicKey.compressedSec1(): ByteArray {
    val point = w
    val prefix = if (point.affineY.testBit(0)) 0x03.toByte() else 0x02.toByte()
    return byteArrayOf(prefix) + point.affineX.toFixedWidth(P256_FIELD_BYTES)
}

/**
 * BigInteger.toByteArray() is signed and variable width: it prepends a zero
 * byte for values with the high bit set and drops leading zeros otherwise.
 * SEC1 wants exactly [width] big-endian bytes.
 */
private fun BigInteger.toFixedWidth(width: Int): ByteArray {
    val signed = toByteArray()
    return when {
        signed.size == width -> signed
        signed.size > width -> signed.copyOfRange(signed.size - width, signed.size)
        else -> ByteArray(width - signed.size) + signed
    }
}
