package com.adroited.aiterm.ui

import android.annotation.SuppressLint
import android.content.Intent
import android.net.Uri
import android.net.http.SslCertificate
import android.net.http.SslError
import android.os.Build
import android.webkit.SslErrorHandler
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import com.adroited.aiterm.security.SpkiFingerprint
import java.io.ByteArrayInputStream
import java.security.MessageDigest
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate

/**
 * The one deliberate WebView in AITerm: a page the selected session built.
 * Terminal and conversation rendering remain native Compose. The page rides
 * through the same connected gateway endpoint and trusts only its paired SPKI.
 */
@OptIn(ExperimentalMaterial3Api::class)
@SuppressLint("SetJavaScriptEnabled")
@Composable
fun RemoteWebPreviewScreen(
    url: String,
    serverSpkiFingerprint: String,
    onClose: () -> Unit,
) {
    val context = LocalContext.current
    val origin = remember(url) { Uri.parse(url) }
    val ticketRoot = remember(origin) { origin.path.orEmpty() }
    var webView by remember { mutableStateOf<WebView?>(null) }
    var loading by remember(url) { mutableStateOf(true) }
    var error by remember(url) { mutableStateOf<String?>(null) }

    fun goBack() {
        val web = webView
        if (web?.canGoBack() == true) web.goBack() else onClose()
    }

    BackHandler(onBack = ::goBack)
    DisposableEffect(Unit) {
        onDispose {
            webView?.apply {
                stopLoading()
                loadUrl("about:blank")
                clearHistory()
                removeAllViews()
                destroy()
            }
            webView = null
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = {
                    IconButton(onClick = ::goBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
                title = { Text("Web preview") },
                actions = {
                    IconButton(onClick = {
                        error = null
                        loading = true
                        webView?.reload()
                    }) {
                        Icon(Icons.Filled.Refresh, contentDescription = "Refresh webpage")
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.background,
                ),
            )
        },
        containerColor = MaterialTheme.colorScheme.background,
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            if (loading) LinearProgressIndicator(Modifier.fillMaxWidth())
            Box(Modifier.fillMaxSize()) {
                AndroidView(
                    modifier = Modifier.fillMaxSize(),
                    factory = { viewContext ->
                        WebView(viewContext).apply {
                            settings.javaScriptEnabled = true
                            settings.domStorageEnabled = true
                            settings.allowFileAccess = false
                            settings.allowContentAccess = false
                            settings.javaScriptCanOpenWindowsAutomatically = false
                            settings.setSupportMultipleWindows(false)
                            settings.mediaPlaybackRequiresUserGesture = true
                            settings.mixedContentMode = android.webkit.WebSettings.MIXED_CONTENT_NEVER_ALLOW
                            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                                settings.safeBrowsingEnabled = true
                            }
                            webViewClient = PinnedPreviewClient(
                                expectedOrigin = origin,
                                ticketRoot = ticketRoot,
                                expectedSpkiFingerprint = serverSpkiFingerprint,
                                openExternal = { external ->
                                    runCatching {
                                        context.startActivity(Intent(Intent.ACTION_VIEW, external))
                                    }
                                },
                                pageStarted = {
                                    error = null
                                    loading = true
                                },
                                pageFinished = { loading = false },
                                pageFailed = { message ->
                                    error = message
                                    loading = false
                                },
                            )
                            webView = this
                            loadUrl(url)
                        }
                    },
                    update = { web ->
                        if (web.url != url && web.originalUrl != url) web.loadUrl(url)
                    },
                )
                error?.let { message ->
                    Column(
                        modifier = Modifier.align(Alignment.Center).padding(horizontal = 28.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        Text(message, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        TextButton(onClick = {
                            error = null
                            loading = true
                            webView?.reload()
                        }) { Text("Try again") }
                    }
                }
            }
        }
    }
}

private class PinnedPreviewClient(
    private val expectedOrigin: Uri,
    private val ticketRoot: String,
    private val expectedSpkiFingerprint: String,
    private val openExternal: (Uri) -> Unit,
    private val pageStarted: () -> Unit,
    private val pageFinished: () -> Unit,
    private val pageFailed: (String) -> Unit,
) : WebViewClient() {
    override fun onPageStarted(view: WebView?, url: String?, favicon: android.graphics.Bitmap?) {
        pageStarted()
    }

    override fun onPageFinished(view: WebView?, url: String?) {
        pageFinished()
    }

    override fun shouldOverrideUrlLoading(view: WebView?, request: WebResourceRequest): Boolean {
        if (!request.isForMainFrame) return false
        val target = request.url
        if (sameOrigin(expectedOrigin, target) && target.path.orEmpty().startsWith(ticketRoot)) {
            return false
        }
        if (target.scheme == "http" || target.scheme == "https") openExternal(target)
        return true
    }

    override fun onReceivedError(
        view: WebView?,
        request: WebResourceRequest,
        error: WebResourceError,
    ) {
        if (request.isForMainFrame) pageFailed(error.description?.toString() ?: "The webpage could not be loaded.")
    }

    override fun onReceivedSslError(view: WebView?, handler: SslErrorHandler, error: SslError) {
        val target = Uri.parse(error.url)
        val certificate = x509Certificate(error.certificate)
        val matches = sameOrigin(expectedOrigin, target) && certificate?.let { cert ->
            val expected = expectedSpkiFingerprint.toByteArray(Charsets.US_ASCII)
            val presented = SpkiFingerprint.of(cert).toByteArray(Charsets.US_ASCII)
            MessageDigest.isEqual(expected, presented)
        } == true
        if (matches) handler.proceed() else {
            handler.cancel()
            pageFailed("The desktop webpage presented a different security key.")
        }
    }
}

private fun sameOrigin(expected: Uri, actual: Uri): Boolean =
    expected.scheme.equals(actual.scheme, ignoreCase = true) &&
        expected.host.equals(actual.host, ignoreCase = true) &&
        effectivePort(expected) == effectivePort(actual)

private fun effectivePort(uri: Uri): Int = when {
    uri.port >= 0 -> uri.port
    uri.scheme.equals("https", ignoreCase = true) -> 443
    else -> 80
}

@Suppress("DEPRECATION")
private fun x509Certificate(certificate: SslCertificate?): X509Certificate? {
    if (certificate == null) return null
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) return certificate.x509Certificate
    val encoded = SslCertificate.saveState(certificate)?.getByteArray("x509-certificate") ?: return null
    return runCatching {
        CertificateFactory.getInstance("X.509")
            .generateCertificate(ByteArrayInputStream(encoded)) as X509Certificate
    }.getOrNull()
}
