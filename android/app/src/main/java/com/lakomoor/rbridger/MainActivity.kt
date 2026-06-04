package com.lakomoor.rbridger

import android.Manifest
import android.content.pm.PackageManager
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.lifecycle.lifecycleScope
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import com.lakomoor.rbridger.ui.theme.RBridgerTheme
import kotlinx.coroutines.launch
import java.util.concurrent.Executors

class MainActivity : ComponentActivity() {

    private val cameraExecutor = Executors.newSingleThreadExecutor()
    private var previewViewRef: PreviewView? = null

    private var connState by mutableStateOf(ConnState.DISCONNECTED)
    private var hintMsg by mutableStateOf("")
    private var faceFound by mutableStateOf(false)
    private var loadProgress by mutableStateOf(0f)   // -1 = ready, 0..1 = downloading
    private var loadError by mutableStateOf<String?>(null)

    private lateinit var vtsClient: VtsClient
    private lateinit var tracker: FaceTracker

    private val cameraPermission = registerForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { granted ->
        if (granted) previewViewRef?.let { bindCamera(it) }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        vtsClient = VtsClient(
            context = this,
            onState = { connState = it },
            onHint  = { hintMsg = it },
        )
        tracker = FaceTracker(this) { data ->
            faceFound = data != null
            data?.let { vtsClient.injectFace(it) }
        }

        setContent {
            RBridgerTheme {
                Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                    when {
                        loadError != null       -> ErrorScreen(loadError!!) { retryDownload() }
                        loadProgress >= 0f      -> LoadingScreen(loadProgress)
                        else                    -> MainScreen()
                    }
                }
            }
        }

        retryDownload()
    }

    private fun retryDownload() {
        loadError = null
        loadProgress = 0f
        lifecycleScope.launch {
            runCatching {
                val modelFile = ModelManager.ensureModel(this@MainActivity) { p -> loadProgress = p }
                tracker.initialize(modelFile)
                loadProgress = -1f
            }.onFailure {
                loadError = it.message ?: "Download failed"
            }
        }
    }

    @Composable
    private fun LoadingScreen(progress: Float) {
        Column(
            modifier = Modifier.fillMaxSize().padding(32.dp),
            verticalArrangement = Arrangement.Center,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text("Downloading face model…", style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(16.dp))
            LinearProgressIndicator(progress = { progress }, modifier = Modifier.fillMaxWidth())
            Spacer(Modifier.height(8.dp))
            Text("${(progress * 100).toInt()}%", style = MaterialTheme.typography.bodySmall)
        }
    }

    @Composable
    private fun ErrorScreen(msg: String, onRetry: () -> Unit) {
        Column(
            modifier = Modifier.fillMaxSize().padding(32.dp),
            verticalArrangement = Arrangement.Center,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text("Failed to load model", style = MaterialTheme.typography.titleMedium, color = Color(0xFFF44336))
            Spacer(Modifier.height(8.dp))
            Text(msg, style = MaterialTheme.typography.bodySmall)
            Spacer(Modifier.height(16.dp))
            Button(onClick = onRetry) { Text("Retry") }
        }
    }

    @Composable
    private fun MainScreen() {
        var host by remember { mutableStateOf("192.168.") }
        var port by remember { mutableStateOf("8001") }

        val isConnected   = connState == ConnState.CONNECTED
        val isApproving   = connState == ConnState.AWAITING_APPROVAL
        val isBusy        = connState == ConnState.CONNECTING || connState == ConnState.AUTHENTICATING || isApproving
        val isDisconnected = connState == ConnState.DISCONNECTED || connState == ConnState.ERROR

        Column(Modifier.fillMaxSize()) {
            // Camera preview
            AndroidView(
                factory = { ctx ->
                    PreviewView(ctx).also { pv ->
                        previewViewRef = pv
                        if (ContextCompat.checkSelfPermission(ctx, Manifest.permission.CAMERA)
                            == PackageManager.PERMISSION_GRANTED
                        ) bindCamera(pv)
                        else cameraPermission.launch(Manifest.permission.CAMERA)
                    }
                },
                modifier = Modifier.fillMaxWidth().weight(1f),
            )

            // Controls
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(MaterialTheme.colorScheme.surface)
                    .padding(16.dp),
            ) {
                // Status row
                Row(verticalAlignment = Alignment.CenterVertically) {
                    val dotColor = when (connState) {
                        ConnState.CONNECTED        -> Color(0xFF4CAF50)
                        ConnState.CONNECTING,
                        ConnState.AUTHENTICATING,
                        ConnState.AWAITING_APPROVAL -> Color(0xFFFFEB3B)
                        ConnState.ERROR            -> Color(0xFFF44336)
                        ConnState.DISCONNECTED     -> Color(0xFF757575)
                    }
                    Box(Modifier.size(10.dp).background(dotColor, CircleShape))
                    Spacer(Modifier.width(8.dp))
                    Text(
                        when (connState) {
                            ConnState.AWAITING_APPROVAL -> "Waiting for approval…"
                            else -> connState.name.lowercase().replaceFirstChar { it.uppercase() }
                        },
                        style = MaterialTheme.typography.bodySmall,
                    )
                    Spacer(Modifier.weight(1f))
                    if (faceFound && isConnected) {
                        Text("Face ✓", style = MaterialTheme.typography.bodySmall, color = Color(0xFF4CAF50))
                    }
                }

                // Hint / error message
                if (hintMsg.isNotEmpty() && !isConnected) {
                    Spacer(Modifier.height(4.dp))
                    Text(
                        hintMsg,
                        style = MaterialTheme.typography.labelSmall,
                        color = if (connState == ConnState.ERROR) Color(0xFFF44336) else Color(0xFFFFEB3B),
                    )
                }

                Spacer(Modifier.height(10.dp))

                OutlinedTextField(
                    value = host,
                    onValueChange = { host = it },
                    label = { Text("VTube Studio PC IP") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                    enabled = isDisconnected,
                )
                Spacer(Modifier.height(6.dp))
                OutlinedTextField(
                    value = port,
                    onValueChange = { port = it },
                    label = { Text("Port") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                    enabled = isDisconnected,
                )

                Spacer(Modifier.height(10.dp))

                Button(
                    onClick = {
                        if (!isDisconnected) {
                            hintMsg = ""
                            vtsClient.disconnect()
                        } else {
                            hintMsg = ""
                            vtsClient.connect(host.trim(), port.trim().toIntOrNull() ?: 8001)
                        }
                    },
                    modifier = Modifier.fillMaxWidth().height(48.dp),
                    colors = ButtonDefaults.buttonColors(
                        containerColor = if (isConnected || isApproving) Color(0xFFF44336)
                        else MaterialTheme.colorScheme.primary,
                    ),
                ) {
                    if (isBusy && !isApproving) {
                        CircularProgressIndicator(
                            modifier = Modifier.size(20.dp),
                            strokeWidth = 2.dp,
                            color = MaterialTheme.colorScheme.onPrimary,
                        )
                        Spacer(Modifier.width(8.dp))
                    }
                    Text(when (connState) {
                        ConnState.CONNECTED         -> "Disconnect"
                        ConnState.CONNECTING        -> "Connecting…"
                        ConnState.AUTHENTICATING    -> "Authenticating…"
                        ConnState.AWAITING_APPROVAL -> "Cancel"
                        else                        -> "Connect to VTube Studio"
                    })
                }

                Spacer(Modifier.height(4.dp))
                Text(
                    "PC and phone must be on the same Wi-Fi. Enable Plugin API in VTS Settings.",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.45f),
                )
            }
        }
    }

    private fun bindCamera(previewView: PreviewView) {
        val future = ProcessCameraProvider.getInstance(this)
        future.addListener({
            val provider = future.get()
            val preview = Preview.Builder().build().also {
                it.setSurfaceProvider(previewView.surfaceProvider)
            }
            val analysis = ImageAnalysis.Builder()
                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                .build()
                .also { ia ->
                    ia.setAnalyzer(cameraExecutor) { img ->
                        if (loadProgress < 0f) tracker.process(img) else img.close()
                    }
                }
            runCatching {
                provider.unbindAll()
                provider.bindToLifecycle(this, CameraSelector.DEFAULT_FRONT_CAMERA, preview, analysis)
            }
        }, ContextCompat.getMainExecutor(this))
    }

    override fun onDestroy() {
        super.onDestroy()
        tracker.close()
        vtsClient.disconnect()
        cameraExecutor.shutdown()
    }
}
