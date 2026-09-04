package com.adroited.aiterm.ui

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ExperimentalGetImage
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.lifecycle.viewmodel.compose.viewModel
import com.adroited.aiterm.R
import com.adroited.aiterm.pairing.PairingFailure
import com.adroited.aiterm.pairing.PairingPayload
import com.adroited.aiterm.pairing.PairingPayloadResult
import com.adroited.aiterm.pairing.PairingRepository
import com.adroited.aiterm.pairing.PairingResult
import com.google.mlkit.vision.barcode.BarcodeScannerOptions
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.common.InputImage
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

sealed interface PairingUiState {
    data object Scanning : PairingUiState
    data class Confirming(val desktopName: String, val fingerprint: String) : PairingUiState
    data class Connecting(val desktopName: String) : PairingUiState
    data class AwaitingApproval(val desktopName: String) : PairingUiState
    data class Paired(val desktopName: String) : PairingUiState
    data class Failed(val failure: PairingFailure) : PairingUiState
}

class PairingViewModel(
    private val repository: PairingRepository,
    private val clock: () -> Long = System::currentTimeMillis,
    private val deviceName: () -> String = {
        Build.MODEL?.take(128)?.ifBlank { "Android phone" } ?: "Android phone"
    },
) : ViewModel() {

    private val mutableState = MutableStateFlow<PairingUiState>(PairingUiState.Scanning)
    val state: StateFlow<PairingUiState> = mutableState.asStateFlow()

    private var pendingPayload: PairingPayload? = null

    fun onQrCode(rawValue: String) {
        if (mutableState.value != PairingUiState.Scanning) return
        when (val parsed = PairingPayload.parse(rawValue, clock())) {
            is PairingPayloadResult.Parsed -> {
                pendingPayload = parsed.payload
                mutableState.value = PairingUiState.Confirming(
                    desktopName = parsed.payload.desktopName,
                    fingerprint = parsed.payload.serverSpkiFingerprint.chunked(4).joinToString("-"),
                )
            }
            is PairingPayloadResult.Rejected -> {
                mutableState.value = PairingUiState.Failed(parsed.failure)
            }
        }
    }

    fun confirm() {
        val payload = pendingPayload ?: return
        if (mutableState.value !is PairingUiState.Confirming) return
        val desktopName = payload.desktopName
        mutableState.value = PairingUiState.Connecting(desktopName)
        viewModelScope.launch {
            val result = repository.pair(
                payload = payload,
                deviceName = deviceName().take(128).ifBlank { "Android phone" },
                nowEpochMillis = clock(),
                onAwaitingApproval = {
                    mutableState.value = PairingUiState.AwaitingApproval(desktopName)
                },
            )
            pendingPayload = null
            mutableState.value = when (result) {
                is PairingResult.Paired -> PairingUiState.Paired(result.desktop.displayName)
                is PairingResult.Rejected -> PairingUiState.Failed(result.failure)
            }
        }
    }

    fun discardPendingCode() {
        pendingPayload?.discard()
        pendingPayload = null
    }

    fun scanAgain() {
        discardPendingCode()
        mutableState.value = PairingUiState.Scanning
    }

    override fun onCleared() {
        discardPendingCode()
    }

    companion object {
        fun factory(repository: PairingRepository): ViewModelProvider.Factory = viewModelFactory {
            initializer { PairingViewModel(repository) }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PairingScreen(
    repository: PairingRepository,
    onBack: () -> Unit,
    onPaired: () -> Unit,
    viewModel: PairingViewModel = viewModel(factory = PairingViewModel.factory(repository)),
    localNetworkAccessGranted: (() -> Boolean)? = null,
    requestLocalNetworkAccess: (((Boolean) -> Unit) -> Unit)? = null,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    val confirmAfterPermission: (Boolean) -> Unit = { granted ->
        if (granted) viewModel.confirm()
    }
    val localNetworkPermission = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
        confirmAfterPermission,
    )
    val confirmWithLocalNetworkAccess = {
        val granted = localNetworkAccessGranted?.invoke() ?: (
            Build.VERSION.SDK_INT < 37 ||
                ContextCompat.checkSelfPermission(
                    context,
                    Manifest.permission.ACCESS_LOCAL_NETWORK,
                ) == PackageManager.PERMISSION_GRANTED
            )
        if (granted) {
            viewModel.confirm()
        } else {
            requestLocalNetworkAccess?.invoke(confirmAfterPermission)
                ?: localNetworkPermission.launch(Manifest.permission.ACCESS_LOCAL_NETWORK)
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.pairing_title)) },
                navigationIcon = {
                    TextButton(
                        onClick = {
                            viewModel.discardPendingCode()
                            onBack()
                        },
                    ) { Text(stringResource(R.string.action_back)) }
                },
            )
        },
    ) { innerPadding ->
        PairingContent(
            state = state,
            onConfirm = confirmWithLocalNetworkAccess,
            onCancel = {
                viewModel.discardPendingCode()
                onBack()
            },
            onScanAgain = viewModel::scanAgain,
            onDone = onPaired,
            scanner = {
                CameraPermissionGate(onQrCode = viewModel::onQrCode)
            },
            modifier = Modifier.fillMaxSize().padding(innerPadding),
        )
    }
}

@Composable
fun PairingContent(
    state: PairingUiState,
    modifier: Modifier = Modifier,
    onConfirm: () -> Unit = {},
    onCancel: () -> Unit = {},
    onScanAgain: () -> Unit = {},
    onDone: () -> Unit = {},
    scanner: @Composable () -> Unit = {},
) {
    val step = when (state) {
        PairingUiState.Scanning -> PairingStep.Scan
        is PairingUiState.Confirming, is PairingUiState.Connecting -> PairingStep.Verify
        else -> PairingStep.Approve
    }
    Column(
        modifier = modifier.verticalScroll(rememberScrollState()).padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(20.dp),
    ) {
        PairingSignalRail(current = step)

        when (state) {
            PairingUiState.Scanning -> {
                Text(
                    "Scan the QR code shown by AITerm on your desktop",
                    style = MaterialTheme.typography.headlineSmall,
                )
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .aspectRatio(1f)
                        .clip(RoundedCornerShape(24.dp))
                        .background(MaterialTheme.colorScheme.surfaceContainerHighest),
                ) {
                    scanner()
                    ScannerTrustFrame(modifier = Modifier.fillMaxSize().padding(18.dp))
                }
                Text(
                    "The code is checked in memory, then discarded. Pairing data is sent only after the desktop key matches.",
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            is PairingUiState.Confirming -> {
                Text("Verify this desktop", style = MaterialTheme.typography.headlineSmall)
                Surface(
                    color = MaterialTheme.colorScheme.secondaryContainer,
                    shape = RoundedCornerShape(20.dp),
                ) {
                    Column(
                        modifier = Modifier.fillMaxWidth().padding(20.dp),
                        verticalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Text(
                            state.desktopName,
                            style = MaterialTheme.typography.headlineMedium,
                            fontWeight = FontWeight.SemiBold,
                        )
                        Text(
                            "Pinned server key",
                            style = MaterialTheme.typography.labelLarge,
                            color = MaterialTheme.colorScheme.onSecondaryContainer,
                        )
                        Text(
                            state.fingerprint,
                            fontFamily = FontFamily.Monospace,
                            style = MaterialTheme.typography.bodyMedium,
                        )
                    }
                }
                Text(
                    "Confirm the name and key before this phone asks the desktop for approval.",
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(12.dp, Alignment.End),
                ) {
                    OutlinedButton(onClick = onCancel) { Text("Cancel") }
                    Button(onClick = onConfirm) { Text("Pair") }
                }
            }

            is PairingUiState.Connecting -> {
                CenteredPairingStatus {
                    CircularProgressIndicator()
                    Text("Checking ${state.desktopName}'s pinned key")
                    Text(
                        "Nothing is enrolled unless the scanned key matches.",
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            is PairingUiState.AwaitingApproval -> {
                CenteredPairingStatus {
                    CircularProgressIndicator()
                    Text(
                        "Approve this phone on ${state.desktopName}",
                        style = MaterialTheme.typography.headlineSmall,
                        textAlign = TextAlign.Center,
                    )
                    Text(
                        "AITerm will save this desktop only after it returns an approved response.",
                        textAlign = TextAlign.Center,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            is PairingUiState.Paired -> {
                CenteredPairingStatus {
                    Text("Desktop paired", style = MaterialTheme.typography.headlineMedium)
                    Text(
                        "${state.desktopName} now trusts this phone's protected device key.",
                        textAlign = TextAlign.Center,
                    )
                    Button(onClick = onDone) { Text("Done") }
                }
            }

            is PairingUiState.Failed -> {
                CenteredPairingStatus {
                    Text("Pairing stopped", style = MaterialTheme.typography.headlineMedium)
                    Text(
                        failureMessage(state.failure),
                        textAlign = TextAlign.Center,
                        color = if (
                            state.failure == PairingFailure.FINGERPRINT_MISMATCH ||
                            state.failure == PairingFailure.TLS_IDENTITY_MISMATCH
                        ) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface,
                    )
                    Button(onClick = onScanAgain) { Text("Scan again") }
                }
            }
        }
    }
}

@Composable
private fun CenteredPairingStatus(content: @Composable ColumnScope.() -> Unit) {
    Column(
        modifier = Modifier.fillMaxWidth().padding(vertical = 48.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        content = content,
    )
}

private enum class PairingStep { Scan, Verify, Approve }

@Composable
private fun PairingSignalRail(current: PairingStep) {
    val active = MaterialTheme.colorScheme.primary
    val inactive = MaterialTheme.colorScheme.outlineVariant
    val steps = PairingStep.entries
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Canvas(modifier = Modifier.fillMaxWidth().height(18.dp)) {
            val points = steps.indices.map { index ->
                Offset(size.width * (index + 0.5f) / steps.size, size.height / 2)
            }
            drawLine(
                color = inactive,
                start = points.first(),
                end = points.last(),
                strokeWidth = 3.dp.toPx(),
                cap = StrokeCap.Round,
            )
            drawLine(
                color = active,
                start = points.first(),
                end = points[current.ordinal],
                strokeWidth = 3.dp.toPx(),
                cap = StrokeCap.Round,
            )
            points.forEachIndexed { index, point ->
                drawCircle(
                    color = if (index <= current.ordinal) active else inactive,
                    radius = 6.dp.toPx(),
                    center = point,
                )
            }
        }
        Row(modifier = Modifier.fillMaxWidth()) {
            steps.forEach { step ->
                Text(
                    text = step.name,
                    modifier = Modifier.weight(1f),
                    textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.labelMedium,
                    color = if (step.ordinal <= current.ordinal) active else inactive,
                )
            }
        }
    }
}

@Composable
private fun ScannerTrustFrame(modifier: Modifier = Modifier) {
    val color = MaterialTheme.colorScheme.primary
    Canvas(modifier = modifier) {
        val arm = size.minDimension * 0.16f
        val stroke = 4.dp.toPx()
        val corners = listOf(
            Offset(0f, 0f) to Offset(1f, 1f),
            Offset(size.width, 0f) to Offset(-1f, 1f),
            Offset(0f, size.height) to Offset(1f, -1f),
            Offset(size.width, size.height) to Offset(-1f, -1f),
        )
        corners.forEach { (corner, direction) ->
            drawLine(
                color,
                corner,
                Offset(corner.x + arm * direction.x, corner.y),
                stroke,
                StrokeCap.Round,
            )
            drawLine(
                color,
                corner,
                Offset(corner.x, corner.y + arm * direction.y),
                stroke,
                StrokeCap.Round,
            )
        }
    }
}

@Composable
private fun CameraPermissionGate(onQrCode: (String) -> Unit) {
    val context = LocalContext.current
    var granted by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
                PackageManager.PERMISSION_GRANTED,
        )
    }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        granted = it
    }
    LaunchedEffect(Unit) {
        if (!granted) permission.launch(Manifest.permission.CAMERA)
    }

    if (granted) {
        CameraQrPreview(onQrCode = onQrCode)
    } else {
        Column(
            modifier = Modifier.fillMaxSize().padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text("Camera access is used only to scan an AITerm pairing code.", textAlign = TextAlign.Center)
            Button(onClick = { permission.launch(Manifest.permission.CAMERA) }) {
                Text("Allow camera")
            }
        }
    }
}

@androidx.annotation.OptIn(markerClass = [ExperimentalGetImage::class])
@Composable
private fun CameraQrPreview(onQrCode: (String) -> Unit) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val previewView = remember {
        PreviewView(context).apply {
            scaleType = PreviewView.ScaleType.FILL_CENTER
            implementationMode = PreviewView.ImplementationMode.COMPATIBLE
        }
    }

    AndroidView(
        factory = { previewView },
        modifier = Modifier.fillMaxSize(),
    )

    DisposableEffect(lifecycleOwner, previewView) {
        val active = AtomicBoolean(true)
        val processing = AtomicBoolean(false)
        val scanner = BarcodeScanning.getClient(
            BarcodeScannerOptions.Builder()
                .setBarcodeFormats(Barcode.FORMAT_QR_CODE)
                .build(),
        )
        val providerFuture = ProcessCameraProvider.getInstance(context)
        val mainExecutor = ContextCompat.getMainExecutor(context)
        providerFuture.addListener(
            {
                if (!active.get()) return@addListener
                val provider = providerFuture.get()
                val preview = Preview.Builder().build().also {
                    it.surfaceProvider = previewView.surfaceProvider
                }
                val analysis = ImageAnalysis.Builder()
                    .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                    .build()
                analysis.setAnalyzer(mainExecutor) { imageProxy ->
                    if (!processing.compareAndSet(false, true)) {
                        imageProxy.close()
                        return@setAnalyzer
                    }
                    val mediaImage = imageProxy.image
                    if (mediaImage == null) {
                        processing.set(false)
                        imageProxy.close()
                        return@setAnalyzer
                    }
                    val input = InputImage.fromMediaImage(
                        mediaImage,
                        imageProxy.imageInfo.rotationDegrees,
                    )
                    scanner.process(input)
                        .addOnSuccessListener { barcodes ->
                            if (active.get()) {
                                barcodes.firstNotNullOfOrNull { it.rawValue }?.let(onQrCode)
                            }
                        }
                        .addOnCompleteListener {
                            processing.set(false)
                            imageProxy.close()
                        }
                }
                provider.unbindAll()
                provider.bindToLifecycle(
                    lifecycleOwner,
                    CameraSelector.DEFAULT_BACK_CAMERA,
                    preview,
                    analysis,
                )
            },
            mainExecutor,
        )

        onDispose {
            active.set(false)
            if (providerFuture.isDone) {
                runCatching { providerFuture.get().unbindAll() }
            }
            scanner.close()
        }
    }
}

private fun failureMessage(failure: PairingFailure): String = when (failure) {
    PairingFailure.UNSUPPORTED_VERSION ->
        "This pairing code uses a version this app does not understand. Update AITerm and scan again."
    PairingFailure.MALFORMED_PAYLOAD ->
        "That is not a valid AITerm pairing code. Show a new code on the desktop."
    PairingFailure.EXPIRED_PAYLOAD ->
        "That pairing code has expired. Show a new one on the desktop."
    PairingFailure.CONSUMED_PAYLOAD ->
        "That pairing code has already been used. Show a new one on the desktop."
    PairingFailure.FINGERPRINT_MISMATCH ->
        "This desktop did not present the key from the QR code. Nothing was sent."
    PairingFailure.TLS_IDENTITY_MISMATCH ->
        "The desktop certificate does not name this address. Nothing was sent."
    PairingFailure.UNREACHABLE ->
        "The desktop could not be reached at any address in the code. Check LAN or VPN access and scan a new code."
    PairingFailure.ENROLLMENT_STATE_UNKNOWN ->
        "The connection ended after the request may have been sent. Check trusted devices on the desktop, then scan a new code."
    PairingFailure.DENIED_BY_DESKTOP ->
        "The desktop denied this pairing request. Show a new code if you want to try again."
    PairingFailure.PROTOCOL_ERROR ->
        "The desktop returned an invalid pairing response. Nothing was saved."
    PairingFailure.KEY_UNAVAILABLE ->
        "This phone could not create its protected device key. A secure device lock is required."
    PairingFailure.STORAGE_FAILURE ->
        "The desktop approved, but this phone could not save the pairing. Revoke it on the desktop before scanning a new code."
}
