package network.spike.connect

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
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
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.ui.graphics.Color
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.MultiFormatReader
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.common.HybridBinarizer
import java.io.File
import java.util.concurrent.Executors

// ---- camera scanning of QR codes ---------------------------------------------------------------------

private fun decodeQr(image: ImageProxy, reader: MultiFormatReader): String? {
    val plane = image.planes[0]
    val buffer = plane.buffer
    val rowStride = plane.rowStride
    val data = ByteArray(buffer.remaining()).also { buffer.get(it) }
    val w = image.width
    val h = image.height
    return try {
        val source = PlanarYUVLuminanceSource(data, rowStride, h, 0, 0, w, h, false)
        reader.decodeWithState(BinaryBitmap(HybridBinarizer(source))).text
    } catch (_: Exception) {
        null // no code in this frame
    } finally {
        reader.reset()
    }
}

/**
 * The camera, looking for a QR code. Calls [onResult] once with its text. Nothing leaves the phone: the
 * frames are read here, by a plain Java decoder, and thrown away.
 */
@Composable
fun QrScanner(modifier: Modifier = Modifier, onResult: (String) -> Unit) {
    val ctx = LocalContext.current
    val owner = LocalLifecycleOwner.current
    var granted by remember { mutableStateOf(ContextCompat.checkSelfPermission(ctx, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) }
    var asked by remember { mutableStateOf(false) }
    val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted = it; asked = true }
    LaunchedEffect(Unit) { if (!granted) ask.launch(Manifest.permission.CAMERA) }

    if (!granted) {
        Box(modifier.clip(RoundedCornerShape(16.dp)).background(C.AvatarBg).padding(20.dp), contentAlignment = Alignment.Center) {
            Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Muted(if (asked) "The camera is off for Spike Connect. Allow it in the phone's settings to scan, or paste the card instead." else "Waiting for permission to use the camera…", 14.sp)
                if (asked) OutlineButton("Allow the camera") { ask.launch(Manifest.permission.CAMERA) }
            }
        }
        return
    }

    val executor = remember { Executors.newSingleThreadExecutor() }
    var done by remember { mutableStateOf(false) }
    val reader = remember {
        MultiFormatReader().apply {
            setHints(mapOf(DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE), DecodeHintType.TRY_HARDER to true))
        }
    }
    DisposableEffect(Unit) { onDispose { executor.shutdown() } }

    AndroidView(
        modifier = modifier.clip(RoundedCornerShape(16.dp)),
        factory = { c ->
            val view = PreviewView(c).apply { scaleType = PreviewView.ScaleType.FILL_CENTER }
            val future = ProcessCameraProvider.getInstance(c)
            future.addListener({
                val provider = future.get()
                val preview = Preview.Builder().build().also { it.setSurfaceProvider(view.surfaceProvider) }
                // The default analysis size (640x480) is too coarse for a dense card code seen on a screen.
                val hd = ResolutionSelector.Builder()
                    .setResolutionStrategy(ResolutionStrategy(android.util.Size(1920, 1080), ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER))
                    .build()
                val analysis = ImageAnalysis.Builder()
                    .setResolutionSelector(hd)
                    .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                    .build()
                analysis.setAnalyzer(executor) { image ->
                    if (!done) {
                        val text = decodeQr(image, reader)
                        if (text != null) {
                            done = true
                            view.post { onResult(text) }
                        }
                    }
                    image.close()
                }
                try {
                    provider.unbindAll()
                    provider.bindToLifecycle(owner, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
                } catch (_: Exception) {
                    // No camera here (an emulator without one): the paste path still works.
                }
            }, ContextCompat.getMainExecutor(c))
            view
        },
    )
}

// ---- 6 · link a new device ------------------------------------------------------------------------------

private fun spaced(code: String) = code.chunked(4).joinToString(" ")

/** This phone is the new device: it shows a code, the existing device enters it, both show a safety code. */
@Composable
fun LinkNewScreen(m: AppModel) {
    val ctx = LocalContext.current
    var step by remember { mutableStateOf(1) }
    var code by remember { mutableStateOf("") }
    var qr by remember { mutableStateOf<List<String>?>(null) }
    var safety by remember { mutableStateOf("") }
    var failed by remember { mutableStateOf<String?>(null) }
    var tries by remember { mutableStateOf(0) }

    DisposableEffect(Unit) {
        Listener.paused = true
        onDispose { Listener.paused = false; Listener.update(ctx) }
    }
    LaunchedEffect(tries) {
        failed = null
        step = 1
        try {
            code = Bridge.call("link_offer") as String
            try {
                val r = Bridge.obj("qr_matrix", args("text" to code))
                qr = (0 until r.getJSONArray("rows").length()).map { r.getJSONArray("rows").getString(it) }
            } catch (_: Exception) {}
            safety = Bridge.call("link_wait") as String
            step = 2
        } catch (e: Exception) {
            failed = e.message ?: "Linking did not start."
        }
    }
    fun leave() { m.run { try { Bridge.call("link_cancel") } catch (_: Exception) {}; m.open(null) } }

    // The second step after the passphrase ("1 of 2"), in the design's step layout.
    when {
        failed != null -> StepPage({ leave() }, "2 of 2", "Linking stopped", failed, actions = { PrimaryButton("Start again") { tries++ } }) {}
        step == 1 -> StepPage(
            { leave() }, "2 of 2", "Link this phone",
            "On a device you already use, open Devices and choose Link a new device. Then scan this, or type the code under it.",
            actions = { Muted("Waiting for the other device…", 13.sp, Modifier.fillMaxWidth().padding(bottom = 8.dp)) },
        ) {
            // The code in a white rounded frame (white in dark mode too, so any camera reads it), as in the design.
            val frame = RoundedCornerShape(18.dp)
            Box(Modifier.fillMaxWidth().padding(top = 8.dp), contentAlignment = Alignment.Center) {
                Box(Modifier.size(232.dp).clip(frame).background(Color.White).border(1.dp, C.BorderStrong, frame).padding(14.dp), contentAlignment = Alignment.Center) {
                    val q = qr
                    if (q != null) QrCode(q, Modifier.fillMaxSize()) else Muted("Making your code…")
                }
            }
            Text(code, fontFamily = Mono, fontSize = 11.sp, color = C.Muted, lineHeight = 15.sp)
        }
        step == 2 -> StepPage(
            { leave() }, "2 of 2", "Compare this code", "The device you are linking from shows a code too. Link only if both are the same.",
            actions = {
                PrimaryButton("The codes match", enabled = !m.busy) {
                    m.run {
                        step = 3
                        val r = Bridge.obj("link_receive")
                        m.notice = "Linked: ${r.getInt("contacts")} contacts and ${r.getInt("messages")} messages copied."
                        m.loadStatus()
                        m.open(null)
                    }
                }
                TextLink("They differ, cancel", danger = true) { leave() }
            },
        ) { CodePanel(safety) }
        else -> StepPage({ leave() }, "2 of 2", "Receiving", "Your identity and history are arriving, encrypted, from your other device.") {}
    }
}

/** This phone already has the identity: enter (or scan) the new device's code, compare, choose what to copy. */
@Composable
fun LinkOldScreen(m: AppModel) {
    val ctx = LocalContext.current
    var step by remember { mutableStateOf(1) }
    var code by remember { mutableStateOf("") }
    var safety by remember { mutableStateOf("") }
    var scanning by remember { mutableStateOf(false) }
    var history by remember { mutableStateOf("recent") }

    DisposableEffect(Unit) {
        Listener.paused = true
        onDispose { Listener.paused = false; Listener.update(ctx) }
    }
    fun leave() { m.run { try { Bridge.call("link_cancel") } catch (_: Exception) {}; m.open(null) } }
    fun begin(c: String) {
        m.run {
            safety = Bridge.call("link_begin", args("code" to c.trim())) as String
            step = 2
        }
    }

    when (step) {
        1 -> StepPage(
            { leave() }, "1 of 3", "Scan the new device’s code",
            "On the new device choose “I already use Spike on another device”. It shows a code: scan it, or type it here.",
            actions = {
                if (scanning) OutlineButton("Type the code instead", Modifier.fillMaxWidth()) { scanning = false }
                else PrimaryButton("Continue", enabled = code.isNotBlank()) { begin(code) }
            },
        ) {
            if (scanning) {
                QrScanner(Modifier.fillMaxWidth().height(280.dp).clip(RoundedCornerShape(16.dp))) { scanning = false; code = it; begin(it) }
            } else {
                OutlineButton("Scan the code", Modifier.fillMaxWidth()) { scanning = true }
                Field("Or type the code", code, { code = it }, singleLine = false, minHeight = 96.dp, maxLines = 4)
            }
        }
        2 -> StepPage(
            { leave() }, "2 of 3", "Compare this code", "Your new device shows a code too. Link only if both are the same.",
            actions = {
                PrimaryButton("The codes match", enabled = !m.busy) { step = 3 }
                TextLink("They differ, cancel", danger = true) { leave() }
            },
        ) { CodePanel(safety) }
        else -> StepPage(
            { step = 2 }, "3 of 3", "What should it receive?",
            actions = {
                Muted("Sent encrypted, straight to the new device. Deleted and expired messages are never copied.", 12.5.sp, Modifier.padding(bottom = 8.dp))
                PrimaryButton("Link device", enabled = !m.busy) {
                    m.run {
                        val r = Bridge.obj("link_finish", args("history" to history, "days" to 30))
                        m.notice = "Linked. ${r.getInt("contacts")} contacts and ${r.getInt("messages")} messages sent, encrypted to the new device."
                        m.open(null)
                    }
                }
            },
        ) {
            Column(Modifier.padding(top = 2.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                for ((key, title, desc) in listOf(
                    Triple("recent", "Last 30 days", "Recent chats, so it is useful right away"),
                    Triple("all", "Everything", "All contacts, messages and files. Larger and slower"),
                    Triple("none", "Contacts only", "No old messages; new ones arrive as usual"),
                )) OptionRow(title, desc, history == key) { history = key }
            }
        }
    }
}

// ---- files -----------------------------------------------------------------------------------------------

/** Copy something the person picked into the app's cache so the engine can read it by path. */
fun copyPicked(ctx: Context, uri: Uri): Pair<String, File>? {
    var name = "file"
    ctx.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use {
        if (it.moveToFirst() && !it.isNull(0)) name = it.getString(0)
    }
    val dir = File(ctx.cacheDir, "outgoing").apply { mkdirs() }
    val out = File(dir, "${System.nanoTime()}-${name.replace('/', '_')}")
    ctx.contentResolver.openInputStream(uri)?.use { input -> out.outputStream().use { input.copyTo(it) } } ?: return null
    return name to out
}

fun sizeText(bytes: Long): String = when {
    bytes < 1024 -> "$bytes B"
    bytes < 1024 * 1024 -> "${bytes / 1024} KB"
    else -> String.format("%.1f MB", bytes / 1048576.0)
}

/** Fetch a received file (checked by the engine), keep it in the app's folder, and offer to open it. */
suspend fun saveAndOpen(ctx: Context, messageId: String): String {
    val dir = File(ctx.filesDir, "received")
    val path = Bridge.call("save_attachment", args("id" to messageId, "dir" to dir.absolutePath)) as String
    val file = File(path)
    val uri = FileProvider.getUriForFile(ctx, "${ctx.packageName}.files", file)
    val ext = MimeTypeMap.getFileExtensionFromUrl(file.name.replace(" ", "_"))
    val mime = MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext.lowercase()) ?: "*/*"
    val view = Intent(Intent.ACTION_VIEW).setDataAndType(uri, mime).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    try {
        ctx.startActivity(Intent.createChooser(view, file.name).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    } catch (_: Exception) {}
    return path
}
