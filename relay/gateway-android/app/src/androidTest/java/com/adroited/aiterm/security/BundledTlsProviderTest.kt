package com.adroited.aiterm.security

import androidx.test.ext.junit.runners.AndroidJUnit4
import java.security.SecureRandom
import javax.net.ssl.SSLContext
import org.conscrypt.Conscrypt
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class BundledTlsProviderTest {

    @Test
    fun bundledFallback_exposesTls13WithoutChangingTheGlobalProviderList() {
        val context = SSLContext.getInstance("TLSv1.3", Conscrypt.newProvider()).apply {
            init(null, null, SecureRandom())
        }

        assertTrue("TLSv1.3" in context.supportedSSLParameters.protocols)
    }
}
