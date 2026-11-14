package network.spike.connect

import android.Manifest
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.os.Build
import android.provider.MediaStore
import android.webkit.MimeTypeMap
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File

// Pictures in a conversation (shown in the bubble, opened full screen) and "Send to my devices".

/** Icons the prototype uses on these screens, in the same 24px line style as [Ic]. */
object IcX {
    private fun icon(name: String, vararg paths: String) = androidx.compose.ui.graphics.vector.ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f).apply {
        for (p in paths) addPath(
            pathData = androidx.compose.ui.graphics.vector.addPathNodes(p), fill = null, stroke = SolidColor(Color.Black), strokeLineWidth = 2f,
            strokeLineCap = androidx.compose.ui.graphics.StrokeCap.Round, strokeLineJoin = androidx.compose.ui.graphics.StrokeJoin.Round,
        )
    }.build()

    val Download = icon("Download", "M12 4v11", "M7 10l5 5 5-5", "M5 20h14")
    val Picture = icon("Picture", "M5 5h14a1 1 0 0 1 1 1v12a1 1 0 0 1 -1 1h-14a1 1 0 0 1 -1 -1v-12a1 1 0 0 1 1 -1z", "M4 16l5-5 4 4 3-3 4 4", "M15 9h.01")
    val Camera = icon("Camera", "M4 8h3l2-3h6l2 3h3a1 1 0 0 1 1 1v9a1 1 0 0 1 -1 1h-16a1 1 0 0 1 -1 -1v-9a1 1 0 0 1 1 -1z", "M9 13a3 3 0 1 0 6 0a3 3 0 1 0 -6 0z")
}

private val PICTURE_TYPES = setOf("jpg", "jpeg", "png", "webp", "gif", "bmp", "heic", "heif")

/** A file the phone can draw as a picture, by its name. */
fun isPicture(name: String?): Boolean = name != null && name.substringAfterLast('.', "").lowercase() in PICTURE_TYPES

/**
 * A message's picture as a file in the app's own folder: fetched and checked by the engine once, then reused.
 * It lives under files/received (the FileProvider shares from there, for Share).
 */
suspend fun pictureFile(ctx: Context, messageId: String): File = withContext(Dispatchers.IO) {
    val dir = File(ctx.filesDir, "received/pictures/$messageId")
    dir.listFiles()?.firstOrNull { it.isFile && it.length() > 0 }?.let { return@withContext it }
    File(Bridge.call("save_attachment", args("id" to messageId, "dir" to dir.absolutePath)) as String)
}

/** Decode a picture no larger than [max] pixels on its long side (enough for the screen, without running out of memory). */
private fun decode(f: File, max: Int): Bitmap? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeFile(f.absolutePath, bounds)
    if (bounds.outWidth <= 0) return null
    var sample = 1
    while (maxOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= max) sample *= 2
    return BitmapFactory.decodeFile(f.absolutePath, BitmapFactory.Options().apply { inSampleSize = sample })
}

@Composable
private fun rememberPicture(messageId: String, max: Int): Pair<Bitmap?, Boolean> {
    val ctx = LocalContext.current
    val state = produceState<Pair<Bitmap?, Boolean>>(null to false, messageId, max) {
        value = try {
            withContext(Dispatchers.IO) { decode(pictureFile(ctx, messageId), max) } to true
        } catch (_: Exception) { null to true }
    }
    return state.value
}

/** The picture inside its bubble, as in the design: 250 wide, rounded, cropped to fit. Tap opens the viewer. */
@Composable
fun PictureInBubble(msg: Message, onOpen: () -> Unit, onFallback: @Composable () -> Unit) {
    val (bmp, done) = rememberPicture(msg.id, 720)
    val shape = RoundedCornerShape(12.dp)
    if (done && bmp == null) { onFallback(); return }
    val ratio = if (bmp != null && bmp.width > 0) (bmp.height.toFloat() / bmp.width).coerceIn(0.5f, 1.4f) else 0.6f
    Box(
        Modifier.width(250.dp).height((250 * ratio).dp).clip(shape)
            .background(Brush.linearGradient(listOf(Color(0xFF8FB0AC), Color(0xFFD8C6A6))))
            .clickable(onClick = onOpen).semantics { contentDescription = "Picture ${msg.fileName}" },
    ) {
        if (bmp != null) Image(bmp.asImageBitmap(), null, Modifier.fillMaxSize(), contentScale = ContentScale.Crop)
    }
}

/** Full screen, dark, as in the design: back, who sent it and when, Save; pinch or double tap to zoom; Share. */
@Composable
fun PictureViewer(msg: Message, who: String, onClose: () -> Unit) {
    val ctx = LocalContext.current
    val (bmp, done) = rememberPicture(msg.id, 2048)
    var scale by remember { mutableStateOf(1f) }
    var pan by remember { mutableStateOf(Offset.Zero) }
    var note by remember { mutableStateOf<String?>(null) }
    val ink = Color(0xFFF2F2EF)
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        Column(Modifier.fillMaxSize().background(Color(0xFF0E1012)).semantics { contentDescription = "Picture viewer" }) {
            Row(
                Modifier.fillMaxWidth().statusBarsPadding().padding(horizontal = 4.dp, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                Box(Modifier.size(44.dp).clip(CircleShape).clickable(onClick = onClose).semantics { contentDescription = "Close the picture" }, contentAlignment = Alignment.Center) { Ico(Ic.Back, ink) }
                Column(Modifier.weight(1f).padding(start = 4.dp)) {
                    Text(who, color = ink, fontWeight = FontWeight.SemiBold, fontSize = 14.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text("${dayLabel(msg.ts)} ${clock(msg.ts)} · ${msg.fileName ?: ""}", color = ink.copy(alpha = 0.6f), fontSize = 11.5.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
                Box(
                    Modifier.size(44.dp).clip(CircleShape).clickable(enabled = bmp != null) {
                        note = try { savePicture(ctx, msg); "Saved to Pictures" } catch (e: Exception) { e.message ?: "Could not save the picture" }
                    }.semantics { contentDescription = "Save the picture" },
                    contentAlignment = Alignment.Center,
                ) { Ico(IcX.Download, ink, 20.dp) }
                Box(
                    Modifier.size(44.dp).clip(CircleShape).clickable(enabled = bmp != null) { sharePicture(ctx, msg) }.semantics { contentDescription = "Share the picture" },
                    contentAlignment = Alignment.Center,
                ) { Ico(Ic.Share, ink, 20.dp) }
            }
            Box(
                Modifier.weight(1f).fillMaxWidth().clipToBounds()
                    .pointerInput(Unit) {
                        detectTransformGestures { _, p, zoom, _ ->
                            scale = (scale * zoom).coerceIn(1f, 6f)
                            pan = if (scale == 1f) Offset.Zero else pan + p
                        }
                    }
                    .pointerInput(Unit) {
                        detectTapGestures(onDoubleTap = { if (scale > 1f) { scale = 1f; pan = Offset.Zero } else scale = 2.5f })
                    },
                contentAlignment = Alignment.Center,
            ) {
                when {
                    bmp != null -> Image(
                        bmp.asImageBitmap(), msg.fileName,
                        Modifier.fillMaxSize().padding(horizontal = 10.dp)
                            .graphicsLayer { scaleX = scale; scaleY = scale; translationX = pan.x; translationY = pan.y },
                        contentScale = ContentScale.Fit,
                    )
                    done -> Text("This picture could not be opened.", color = ink.copy(alpha = 0.7f), fontSize = 14.sp)
                    else -> Text("Opening…", color = ink.copy(alpha = 0.7f), fontSize = 14.sp)
                }
            }
            Text(
                note ?: if (scale > 1f) "Double tap to fit" else "Pinch to zoom", color = ink.copy(alpha = 0.6f), fontSize = 12.5.sp,
                modifier = Modifier.align(Alignment.CenterHorizontally).navigationBarsPadding().padding(top = 14.dp, bottom = 26.dp),
            )
        }
    }
}

private fun mimeOf(name: String): String =
    MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substringAfterLast('.', "").lowercase()) ?: "image/*"

/** Copy the picture into the phone's Pictures/Spike Connect folder (Android 10 and later need no permission). */
private fun savePicture(ctx: Context, msg: Message) {
    val f = File(ctx.filesDir, "received/pictures/${msg.id}").listFiles()?.firstOrNull() ?: error("The picture is not on this phone yet")
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) error("Saving to Pictures needs Android 10 or later; use Share instead")
    val values = ContentValues().apply {
        put(MediaStore.Images.Media.DISPLAY_NAME, f.name)
        put(MediaStore.Images.Media.MIME_TYPE, mimeOf(f.name))
        put(MediaStore.Images.Media.RELATIVE_PATH, "Pictures/Spike Connect")
    }
    val uri = ctx.contentResolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values) ?: error("Could not save the picture")
    ctx.contentResolver.openOutputStream(uri)?.use { out -> f.inputStream().use { it.copyTo(out) } }
}

private fun sharePicture(ctx: Context, msg: Message) {
    val f = File(ctx.filesDir, "received/pictures/${msg.id}").listFiles()?.firstOrNull() ?: return
    val uri = FileProvider.getUriForFile(ctx, "${ctx.packageName}.files", f)
    val send = Intent(Intent.ACTION_SEND).setType(mimeOf(f.name)).putExtra(Intent.EXTRA_STREAM, uri).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    try { ctx.startActivity(Intent.createChooser(send, f.name).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)) } catch (_: Exception) {}
}

// ---- send to my devices -----------------------------------------------------------------------------------------

/** "Send to Laptop": a picture, a photo or a file, or a note, for your other devices only. Below, what went between them. */
@Composable
fun SendToDevicesScreen(m: AppModel) {
    val ctx = LocalContext.current
    val o = m.overview
    val others = o?.me?.deviceList?.filter { !it.self }.orEmpty()
    val title = if (others.size == 1) "Send to Device ${others[0].id.take(6)}" else "Send to my devices"
    var note by remember { mutableStateOf("") }
    var viewing by remember { mutableStateOf<Message?>(null) }
    val sendFile = { name: String, f: File ->
        m.run {
            try { Bridge.call("send_to_devices", args("file_name" to name, "path" to f.absolutePath, "text" to "")) } finally { f.delete() }
            m.notice = "Sent to your other devices"
            m.refresh()
        }
    }
    val pick = { uri: android.net.Uri? ->
        if (uri != null) m.run {
            val p = withContext(Dispatchers.IO) { copyPicked(ctx, uri) } ?: throw BridgeError("could not read that file")
            sendFile(p.first, p.second)
        }
    }
    val pickPicture = rememberLauncherForActivityResult(ActivityResultContracts.GetContent(), pick)
    val pickFile = rememberLauncherForActivityResult(ActivityResultContracts.GetContent(), pick)
    var shotFile by remember { mutableStateOf<File?>(null) }
    val takePicture = rememberLauncherForActivityResult(ActivityResultContracts.TakePicture()) { ok ->
        val f = shotFile
        if (ok && f != null && f.length() > 0) sendFile(f.name, f) else f?.delete()
    }
    val startCamera = {
        val dir = File(ctx.filesDir, "received/camera").apply { mkdirs() }
        val f = File(dir, "IMG_${java.text.SimpleDateFormat("yyyyMMdd_HHmmss", java.util.Locale.ROOT).format(java.util.Date())}.jpg")
        shotFile = f
        try { takePicture.launch(FileProvider.getUriForFile(ctx, "${ctx.packageName}.files", f)) } catch (_: Exception) { m.notice = "No camera app is available" }
    }
    val askCamera = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { ok -> if (ok) startCamera() else m.notice = "Allow the camera for Spike Connect to take a photo" }
    Page {
        TopBar(title, { m.open(null) }, sub = "Only your devices · encrypted for each one")
        Column(Modifier.weight(1f).fillMaxWidth().verticalScroll(rememberScrollState())) {
            if (others.isEmpty()) {
                EmptyState(Ic.Devices, "Link another device first. Notes and files sent here go to your own devices and nobody else.")
                return@Column
            }
            Row(Modifier.fillMaxWidth().padding(start = 20.dp, end = 20.dp, top = 6.dp, bottom = 14.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                for ((label, icon, act) in listOf(
                    Triple("Picture", IcX.Picture) { pickPicture.launch("image/*") },
                    Triple("Camera", IcX.Camera) {
                        if (ContextCompat.checkSelfPermission(ctx, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) startCamera() else askCamera.launch(Manifest.permission.CAMERA)
                    },
                    Triple("File", Ic.File) { pickFile.launch("*/*") },
                )) {
                    val shape = RoundedCornerShape(14.dp)
                    Column(
                        Modifier.weight(1f).clip(shape).border(1.dp, C.BorderStrong, shape).clickable(onClick = act).padding(vertical = 14.dp)
                            .semantics(mergeDescendants = true) { contentDescription = "Send a ${label.lowercase()}" },
                        horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        Ico(icon, C.AvatarText, 22.dp)
                        Text(label, fontSize = 13.sp, fontWeight = FontWeight.Medium)
                    }
                }
            }
            // A note: text only, like a message to yourself.
            val bar = RoundedCornerShape(14.dp)
            Row(
                Modifier.padding(horizontal = 20.dp).fillMaxWidth().clip(bar).border(1.dp, C.BorderStrong, bar).padding(6.dp),
                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Box(Modifier.weight(1f).heightIn(min = 30.dp).padding(start = 8.dp, top = 5.dp, bottom = 5.dp), contentAlignment = Alignment.CenterStart) {
                    BasicTextField(
                        note, { note = it }, cursorBrush = SolidColor(C.Accent), maxLines = 4,
                        textStyle = TextStyle(fontFamily = Geist, fontSize = 14.sp, color = C.Text), modifier = Modifier.fillMaxWidth(),
                        decorationBox = { inner -> if (note.isEmpty()) Text("A note for your devices", color = C.Muted, fontSize = 14.sp); inner() },
                    )
                }
                Box(
                    Modifier.size(30.dp).clip(CircleShape).background(if (note.isBlank()) C.Accent.copy(alpha = 0.35f) else C.Accent)
                        .clickable(enabled = note.isNotBlank()) {
                            val t = note
                            note = ""
                            m.run { Bridge.call("send_to_devices", args("text" to t)); m.refresh() }
                        }.semantics { contentDescription = "Send the note" },
                    contentAlignment = Alignment.Center,
                ) { Ico(Ic.ArrowUp, C.OnAccent, 15.dp) }
            }
            Text("RECENT", fontFamily = Mono, fontSize = 11.sp, color = C.Muted, letterSpacing = 0.8.sp, modifier = Modifier.padding(start = 20.dp, top = 18.dp, bottom = 4.dp))
            if (m.messages.isEmpty()) Muted("Nothing sent between your devices yet.", 13.sp, Modifier.padding(horizontal = 20.dp, vertical = 8.dp))
            val other = if (others.size == 1) "Device ${others[0].id.take(6)}" else "your devices"
            for (n in m.messages.asReversed()) {
                val pic = isPicture(n.fileName)
                val where = if (n.out) "Sent to $other" else "From ${n.from ?: "another device"}"
                Row(
                    Modifier.fillMaxWidth().clickable(enabled = n.fileName != null) {
                        if (pic) viewing = n else m.run { saveAndOpen(ctx, n.id) }
                    }.padding(horizontal = 20.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    Box(Modifier.size(40.dp).clip(RoundedCornerShape(12.dp)).background(C.Subtle), contentAlignment = Alignment.Center) {
                        Ico(if (pic) IcX.Picture else if (n.fileName != null) Ic.File else Ic.Chat, C.AvatarText, 20.dp)
                    }
                    Column(Modifier.weight(1f)) {
                        Text(n.fileName ?: n.body, fontWeight = FontWeight.Medium, fontSize = 14.5.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
                        Muted("$where · ${clock(n.ts)}" + if (n.fileName != null) " · ${sizeText(n.fileSize)}" else "", 12.5.sp)
                    }
                    if (n.out) Ico(Ic.Check, C.Green, 16.dp)
                }
            }
        }
    }
    viewing?.let { v -> PictureViewer(v, if (v.out) "You" else v.from ?: "Your device") { viewing = null } }
}
