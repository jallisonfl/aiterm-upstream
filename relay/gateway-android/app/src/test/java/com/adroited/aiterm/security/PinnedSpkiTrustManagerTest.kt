package com.adroited.aiterm.security

import java.security.MessageDigest
import java.security.cert.CertificateException
import java.util.Base64
import okhttp3.tls.HeldCertificate
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The desktop certificate is self-signed, so the platform trust store can only
 * ever say "no". These tests assert the replacement rule: the pinned SPKI hash
 * is the entire trust decision, and a certificate that a public CA would happily
 * vouch for is still refused when its key is not the pinned one.
 */
class PinnedSpkiTrustManagerTest {

    private val desktop = HeldCertificate.Builder().addSubjectAlternativeName("localhost").ecdsa256().build()
    private val pin = SpkiFingerprint.of(desktop.certificate)

    @Test
    fun fingerprint_isBase64UrlSha256OfTheSubjectPublicKeyInfo() {
        val expected = Base64.getUrlEncoder().withoutPadding().encodeToString(
            MessageDigest.getInstance("SHA-256").digest(desktop.certificate.publicKey.encoded),
        )

        assertEquals(expected, pin)
        assertTrue("base64url carries no padding", !pin.contains('='))
        assertTrue("base64url uses -_ not +/", !pin.contains('+') && !pin.contains('/'))
    }

    @Test
    fun matchingLeafKey_isAccepted() {
        PinnedSpkiTrustManager(pin).checkServerTrusted(arrayOf(desktop.certificate), "EC")
    }

    @Test
    fun differentLeafKey_isRejected() {
        val impostor = HeldCertificate.Builder().addSubjectAlternativeName("localhost").ecdsa256().build()

        assertThrows(SpkiPinMismatchException::class.java) {
            PinnedSpkiTrustManager(pin).checkServerTrusted(arrayOf(impostor.certificate), "EC")
        }
    }

    @Test
    fun certificateSignedByATrustedAuthority_isStillRejectedWhenTheKeyDiffers() {
        // A chain that ordinary path validation would accept must not sneak
        // past: pinning is not an extra check layered on top of default trust,
        // it is the only check.
        val authority = HeldCertificate.Builder().certificateAuthority(0).build()
        val signed = HeldCertificate.Builder()
            .addSubjectAlternativeName("localhost")
            .signedBy(authority)
            .ecdsa256()
            .build()

        assertThrows(SpkiPinMismatchException::class.java) {
            PinnedSpkiTrustManager(pin)
                .checkServerTrusted(arrayOf(signed.certificate, authority.certificate), "EC")
        }
    }

    @Test
    fun emptyChain_isRejected() {
        assertThrows(CertificateException::class.java) {
            PinnedSpkiTrustManager(pin).checkServerTrusted(emptyArray(), "EC")
        }
    }

    @Test
    fun clientAuthentication_isNeverAccepted() {
        assertThrows(CertificateException::class.java) {
            PinnedSpkiTrustManager(pin).checkClientTrusted(arrayOf(desktop.certificate), "EC")
        }
    }

    @Test
    fun noIssuerIsAdvertised_becauseNoCaCanVouchForTheDesktop() {
        assertArrayEquals(emptyArray(), PinnedSpkiTrustManager(pin).acceptedIssuers)
    }

    @Test
    fun mismatchMessage_carriesNeitherKeyMaterialNorTheSecret() {
        val impostor = HeldCertificate.Builder().ecdsa256().build()
        val thrown = assertThrows(SpkiPinMismatchException::class.java) {
            PinnedSpkiTrustManager(pin).checkServerTrusted(arrayOf(impostor.certificate), "EC")
        }

        assertTrue(thrown.message.orEmpty().isNotEmpty())
        assertTrue(!thrown.message.orEmpty().contains(pin))
    }
}
