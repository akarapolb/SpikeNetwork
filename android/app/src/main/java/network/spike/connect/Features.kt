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
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.background
import androidx.compose.foundation.border
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
                val analysis = ImageAnalysis.Builder()
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

@Composable
private fun Steps(step: Int) {
    Row(horizontalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.fillMaxWidth()) {
        for ((i, label) in listOf("Scan code", "Compare", "History").withIndex()) {
            val on = step == i + 1
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Box(Modifier.fillMaxWidth().height(3.dp).clip(RoundedCornerShape(2.dp)).background(if (step >= i + 1) C.Accent else C.Border))
                Text("${i + 1} · $label", fontSize = 12.sp, color = if (on) C.Text else C.Muted, fontWeight = if (on) FontWeight.SemiBold else FontWeight.Normal)
            }
        }
    }
}

@Composable
private fun Compare(safety: String, whoLabel: String) {
    CardBox {
        Muted("Same code on both screens:", 13.sp)
        Text(safety, fontFamily = Mono, fontSize = 30.sp, fontWeight = FontWeight.SemiBold)
        Muted(whoLabel, 13.sp)
    }
}

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

    Page {
        TopBar("Link this phone", { leave() })
        Column(
            Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 20.dp, vertical = 16.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Steps(step)
            if (failed != null) {
                CardBox {
                    Text("Linking stopped", fontWeight = FontWeight.SemiBold)
                    Muted(failed!!, 13.sp)
                }
                PrimaryButton("Start again") { tries++ }
            } else if (step == 1) {
                Muted("On a device that already uses your identity, open Devices, choose Link a new device, and scan this code or type it in.", 14.sp)
                CardBox {
                    Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                        val q = qr
                        if (q != null) QrCode(q, Modifier.size(220.dp)) else Muted("Making your code…")
                    }
                    Text(code, fontFamily = Mono, fontSize = 12.sp)
                }
                Muted("Waiting for the other device…", 13.sp)
            } else if (step == 2) {
                Muted("Check that both devices show the same code. If they differ, someone may be in the middle: do not continue.", 14.sp)
                Compare(safety, "The device you are linking from shows the same code.")
                PrimaryButton("Same code", enabled = !m.busy) {
                    m.run {
                        step = 3
                        val r = Bridge.obj("link_receive")
                        m.notice = "Linked: ${r.getInt("contacts")} contacts and ${r.getInt("messages")} messages copied."
                        m.loadStatus()
                        m.open(null)
                    }
                }
                OutlineButton("Different code", Modifier.fillMaxWidth()) { leave() }
            } else {
                Muted("Receiving your identity and history…", 14.sp)
            }
        }
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

    Page {
        TopBar("Link a new device", { leave() })
        Column(
            Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 20.dp, vertical = 16.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Steps(step)
            when (step) {
                1 -> {
                    Muted("On the new device choose “I already use Spike on another device”. It shows a code: scan it, or type it here.", 14.sp)
                    if (scanning) {
                        QrScanner(Modifier.fillMaxWidth().height(280.dp)) { scanning = false; code = it; begin(it) }
                        OutlineButton("Type the code instead", Modifier.fillMaxWidth()) { scanning = false }
                    } else {
                        PrimaryButton("Scan the code", icon = Ic.Link) { scanning = true }
                        Field("Code from the new device", code, { code = it }, singleLine = false, minHeight = 96.dp, maxLines = 4)
                        OutlineButton("Continue", Modifier.fillMaxWidth()) { if (code.isNotBlank()) begin(code) }
                    }
                }
                2 -> {
                    Muted("Check that both devices show the same code. If they differ, someone may be in the middle: do not continue.", 14.sp)
                    Compare(safety, "The new device shows the same code.")
                    PrimaryButton("Same code", enabled = !m.busy) { step = 3 }
                    OutlineButton("Different code", Modifier.fillMaxWidth()) { leave() }
                }
                else -> {
                    Text("What should the new device bring?", fontWeight = FontWeight.SemiBold, fontSize = 17.sp)
                    for ((key, title, desc) in listOf(
                        Triple("recent", "Last 30 days", "Recent chats, so it is useful right away."),
                        Triple("all", "Everything", "All contacts, messages and files. Larger and slower."),
                        Triple("none", "Contacts only", "No old messages; new ones arrive as usual."),
                    )) {
                        Row(
                            Modifier.fillMaxWidth().clip(RoundedCornerShape(14.dp))
                                .border(if (history == key) 2.dp else 1.dp, if (history == key) C.Accent else C.Border, RoundedCornerShape(14.dp))
                                .background(C.Surface).clickable { history = key }.padding(14.dp),
                            horizontalArrangement = Arrangement.spacedBy(12.dp),
                        ) {
                            Column(Modifier.weight(1f)) {
                                Text(title, fontWeight = FontWeight.SemiBold)
                                Muted(desc, 13.sp)
                            }
                            if (history == key) Ico(Ic.Check, C.Accent)
                        }
                    }
                    PrimaryButton("Link device", enabled = !m.busy) {
                        m.run {
                            val r = Bridge.obj("link_finish", args("history" to history, "days" to 30))
                            m.notice = "Linked. ${r.getInt("contacts")} contacts and ${r.getInt("messages")} messages sent, encrypted to the new device."
                            m.open(null)
                        }
                    }
                    Muted("Sent encrypted to the new device only. Deleted and expired messages are never copied.", 12.5.sp)
                }
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
