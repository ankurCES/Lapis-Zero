package dev.lapiszero.app.ui.onboarding

import android.Manifest
import android.content.pm.PackageManager
import android.util.Size
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.lapiszero.app.domain.QrDecoder
import dev.lapiszero.app.theme.LapisType
import dev.lapiszero.app.theme.lapis
import dev.lapiszero.app.ui.components.KeyValueRow
import dev.lapiszero.app.ui.components.LapisCard
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/** Path B: scan the QR on the device LCD, confirm, exchange the nonce for a token. */
@Composable
fun QrPairScreen(vm: PairingViewModel, onBack: () -> Unit, onFinished: () -> Unit) {
    val ui by vm.ui.collectAsStateWithLifecycle()
    val context = LocalContext.current
    var cameraGranted by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED,
        )
    }
    var asked by remember { mutableStateOf(false) }
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        cameraGranted = granted
        asked = true
    }
    LaunchedEffect(Unit) {
        if (!cameraGranted) launcher.launch(Manifest.permission.CAMERA)
    }
    var pasted by remember { mutableStateOf("") }

    BackHandler(onBack = if (ui is PairUi.Done) onFinished else onBack)
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
        WizardHeader("Scan pairing code", onBack.takeIf { ui !is PairUi.Done })
        PairOutcome(ui, onRetry = vm::reset, onFinished = onFinished)
        if (ui.isTerminal()) return@Column

        val confirm = ui as? PairUi.Confirm
        if (confirm != null) {
            Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text("Pair with this device?", style = MaterialTheme.typography.titleLarge)
                LapisCard(Modifier.fillMaxWidth(), borderColor = lapis.blue) {
                    KeyValueRow("Device", confirm.payload.host)
                    KeyValueRow("Transport", if (confirm.payload.tls) "HTTPS over Tailscale" else "HTTP (development)")
                    Text("PIN shown on the device", style = MaterialTheme.typography.bodyMedium, color = lapis.muted)
                    Text(confirm.payload.pin, style = LapisType.MonoLarge)
                }
                Text(
                    "Continue only if the PIN matches the one next to the QR code on the device screen.",
                    style = MaterialTheme.typography.bodyMedium,
                )
                Button(onClick = vm::confirm, modifier = Modifier.fillMaxWidth()) { Text("Pair") }
                OutlinedButton(onClick = vm::reset, modifier = Modifier.fillMaxWidth()) { Text("Scan again") }
            }
            return@Column
        }

        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(
                "On a CardputerZero choose Menu -> Pair Android App (QR); on a uConsole show the pairing QR in [7] SYSTEM. Then point the camera at its screen.",
                style = MaterialTheme.typography.bodyMedium,
            )
            Box(
                Modifier
                    .fillMaxWidth()
                    .aspectRatio(1f)
                    .border(2.dp, lapis.blue, MaterialTheme.shapes.medium),
                contentAlignment = Alignment.Center,
            ) {
                if (cameraGranted) {
                    QrCamera(onDecoded = { vm.onScanned(it) }, modifier = Modifier.fillMaxSize().padding(2.dp))
                } else {
                    Column(Modifier.padding(24.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                        Text(
                            if (asked) {
                                "Camera access was denied. Allow it in system settings, or paste the code below."
                            } else {
                                "Camera access is needed to scan the code."
                            },
                            style = MaterialTheme.typography.bodyMedium,
                        )
                        OutlinedButton(onClick = { launcher.launch(Manifest.permission.CAMERA) }) { Text("Allow camera") }
                    }
                }
            }
            OutlinedTextField(
                value = pasted,
                onValueChange = { pasted = it.trim() },
                label = { Text("Or paste a lapis://pair code") },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, autoCorrectEnabled = false),
                modifier = Modifier.fillMaxWidth(),
            )
            Button(
                onClick = { vm.onScanned(pasted) },
                enabled = pasted.isNotBlank(),
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Use pasted code") }
        }
    }
}

/** CameraX preview with a ZXing analyzer on the luminance plane. */
@Composable
private fun QrCamera(onDecoded: (String) -> Unit, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val owner = LocalLifecycleOwner.current
    val callback by rememberUpdatedState(onDecoded)
    val executor: ExecutorService = remember { Executors.newSingleThreadExecutor() }
    var provider by remember { mutableStateOf<ProcessCameraProvider?>(null) }

    DisposableEffect(Unit) {
        onDispose {
            provider?.unbindAll()
            executor.shutdown()
        }
    }

    AndroidView(
        modifier = modifier,
        factory = { ctx ->
            val view = PreviewView(ctx).apply { scaleType = PreviewView.ScaleType.FILL_CENTER }
            val future = ProcessCameraProvider.getInstance(ctx)
            future.addListener(
                {
                    val cameraProvider = try {
                        future.get()
                    } catch (e: Exception) {
                        return@addListener // no usable camera: the paste field still works
                    }
                    provider = cameraProvider
                    val preview = Preview.Builder().build().also { it.surfaceProvider = view.surfaceProvider }
                    // The LCD is small; a 1080p analysis stream keeps its modules resolvable.
                    val analysis = ImageAnalysis.Builder()
                        .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                        .setResolutionSelector(
                            ResolutionSelector.Builder()
                                .setResolutionStrategy(
                                    ResolutionStrategy(
                                        Size(1920, 1080),
                                        ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER,
                                    ),
                                )
                                .build(),
                        )
                        .build()
                    analysis.setAnalyzer(executor) { image -> analyze(image) { text -> view.post { callback(text) } } }
                    try {
                        cameraProvider.unbindAll()
                        cameraProvider.bindToLifecycle(owner, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
                    } catch (e: IllegalStateException) {
                        // Lifecycle already destroyed.
                    } catch (e: IllegalArgumentException) {
                        // The phone has no back camera.
                    }
                },
                ContextCompat.getMainExecutor(ctx),
            )
            view
        },
    )
}

private fun analyze(image: ImageProxy, onText: (String) -> Unit) {
    try {
        val plane = image.planes.firstOrNull() ?: return
        val buffer = plane.buffer
        val data = ByteArray(buffer.remaining())
        buffer.get(data)
        QrDecoder.decode(data, plane.rowStride, image.width, image.height)?.let(onText)
    } finally {
        image.close()
    }
}
