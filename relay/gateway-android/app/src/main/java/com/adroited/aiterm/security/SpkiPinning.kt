package com.adroited.aiterm.security

import android.annotation.SuppressLint
import java.net.Socket
import java.security.MessageDigest
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import java.util.Base64
import java.util.concurrent.atomic.AtomicBoolean
import javax.net.ssl.SSLEngine
import javax.net.ssl.X509ExtendedTrustManager

/** Raised when the peer's key is not the one the pairing QR named. */
class SpkiPinMismatchException(message: String) : CertificateException(message)

/** SHA-256 of a certificate's SubjectPublicKeyInfo, base64url without padding. */
object SpkiFingerprint {

    fun of(certificate: X509Certificate): String = of(certificate.publicKey.encoded)

    /** [subjectPublicKeyInfo] is the DER SPKI, which is what `PublicKey.getEncoded()` returns. */
    fun of(subjectPublicKeyInfo: ByteArray): String =
        Base64.getUrlEncoder().withoutPadding()
            .encodeToString(MessageDigest.getInstance("SHA-256").digest(subjectPublicKeyInfo))
}

/**
 * The whole trust decision for a paired desktop.
 *
 * The desktop's certificate is self-signed, so no platform trust anchor can
 * ever validate it. The tempting fixes are both wrong: a trust-all manager
 * accepts every attacker, and OkHttp's [okhttp3.CertificatePinner] runs *after*
 * default path validation, so it never gets a chance to speak. Instead this
 * manager replaces path validation outright - the leaf key must hash to the
 * fingerprint scanned from the QR, and nothing else is consulted.
 *
 * Certificate validity dates are deliberately not checked: the pinned key is
 * the identity, and a desktop whose clock or certificate lifetime drifted must
 * not silently become untrusted while its key is unchanged.
 */
@SuppressLint("CustomX509TrustManager")
class PinnedSpkiTrustManager(private val pinnedFingerprint: String) : X509ExtendedTrustManager() {

    private val pinnedBytes = pinnedFingerprint.toByteArray(Charsets.US_ASCII)
    private val mismatchObserved = AtomicBoolean(false)

    internal fun didObserveMismatch(): Boolean = mismatchObserved.get()

    override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) =
        verifyLeafKey(chain)

    override fun checkServerTrusted(
        chain: Array<out X509Certificate>?,
        authType: String?,
        socket: Socket?,
    ) = verifyLeafKey(chain)

    override fun checkServerTrusted(
        chain: Array<out X509Certificate>?,
        authType: String?,
        engine: SSLEngine?,
    ) = verifyLeafKey(chain)

    override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?): Unit =
        throw CertificateException("this client never authenticates a peer as a client")

    override fun checkClientTrusted(
        chain: Array<out X509Certificate>?,
        authType: String?,
        socket: Socket?,
    ): Unit = checkClientTrusted(chain, authType)

    override fun checkClientTrusted(
        chain: Array<out X509Certificate>?,
        authType: String?,
        engine: SSLEngine?,
    ): Unit = checkClientTrusted(chain, authType)

    /** No certificate authority can vouch for a self-signed desktop listener. */
    override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()

    private fun verifyLeafKey(chain: Array<out X509Certificate>?) {
        val leaf = chain?.firstOrNull()
            ?: throw CertificateException("the server presented no certificate")
        val presented = SpkiFingerprint.of(leaf).toByteArray(Charsets.US_ASCII)
        if (!MessageDigest.isEqual(pinnedBytes, presented)) {
            mismatchObserved.set(true)
            // Neither fingerprint appears in the message: this string travels
            // into logs and crash reports.
            throw SpkiPinMismatchException(
                "the desktop presented a different public key than the pairing code",
            )
        }
    }
}
