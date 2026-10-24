package network.spike.connect

import android.content.Context
import android.content.Intent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CheckboxDefaults
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.FileProvider
import org.json.JSONObject
import java.io.File
import java.text.DateFormat
import java.util.Date

// Backup and recovery of the identity (ADR 0029). The engine seals and opens the file; this is the
// screens and the hand-over to the system: "Save to…" (the system file picker) and the share sheet.

const val BACKUP_WARNING =
    "This file is a copy of your identity. Anyone who has it and knows its passphrase can pretend to be you and will " +
        "receive your new messages. Keep it somewhere only you can reach, and keep the passphrase separate from the file. " +
        "There is no copy on any server: if you lose both this file and your devices, your identity cannot be recovered. " +
        "A restored account starts new conversations with each contact, and messages sent to you while you were locked out " +
        "of every device may be lost."

private const val EXPORT_NAME = "spike-identity-backup.spikebackup"

/** Where sealed files wait while the person chooses where to put them. Cache, so it is never backed up by the OS. */
private fun backupDir(ctx: Context) = File(ctx.cacheDir, "backup").apply { mkdirs() }

private fun stamp(ms: Long): String = DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT).format(Date(ms))

/** Settings: make a backup file. */
@Composable
fun BackupCard(m: AppModel) {
    val ctx = LocalContext.current
    var status by remember { mutableStateOf<JSONObject?>(null) }
    var key by remember { mutableStateOf<String?>(null) }
    var ack by remember { mutableStateOf(false) }
    var own by remember { mutableStateOf("") }
    var history by remember { mutableStateOf(false) }
    var ready by remember { mutableStateOf<File?>(null) }
    LaunchedEffect(Unit) { status = try { Bridge.obj("backup_status") } catch (_: Exception) { null } }
    // A sealed file left in the cache is a copy of the identity: do not leave it there.
    DisposableEffect(Unit) { onDispose { File(backupDir(ctx), EXPORT_NAME).delete() } }
    val saveTo = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri ->
        val f = ready
        if (uri != null && f != null) {
            try {
                ctx.contentResolver.openOutputStream(uri)?.use { out -> f.inputStream().use { it.copyTo(out) } }
                f.delete()
                ready = null
                m.notice = "Backup saved. Keep it where only you can reach it."
            } catch (e: Exception) {
                m.notice = "Could not save the file: ${e.message}"
            }
        }
    }

    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text("Backup and recovery", fontWeight = FontWeight.SemiBold)
        CardBox {
            val s = status
            if (s != null) {
                val last = if (s.isNull("last_ms")) null else s.getLong("last_ms")
                Text(if (last == null) "No backup yet" else "Last backup: ${stamp(last)}", fontWeight = FontWeight.Medium)
                if (s.optBoolean("stale") && !s.isNull("reason")) {
                    Text(s.getString("reason") + " Make a new one and delete the old file.", fontSize = 13.sp, color = C.AmberText)
                }
            }
            Muted(BACKUP_WARNING, 12.5.sp)
            val f = ready
            if (f != null) {
                Text("Your backup is sealed. Choose where to keep it.", fontWeight = FontWeight.Medium)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlineButton("Save to…", small = true) { saveTo.launch(EXPORT_NAME) }
                    OutlineButton("Share…", small = true) {
                        val uri = FileProvider.getUriForFile(ctx, "${ctx.packageName}.files", f)
                        val send = Intent(Intent.ACTION_SEND).setType("application/octet-stream")
                            .putExtra(Intent.EXTRA_STREAM, uri).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                        ctx.startActivity(Intent.createChooser(send, "Share backup").addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                    }
                    OutlineButton("Discard", small = true, danger = true) { f.delete(); ready = null }
                }
            } else {
                val k = key
                if (k != null) {
                    Muted("Your backup key. It is shown once and is not stored. Write it down and keep it apart from the file:", 13.sp)
                    Text(k, fontFamily = Mono, fontSize = 18.sp)
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Checkbox(ack, { ack = it }, colors = CheckboxDefaults.colors(checkedColor = C.Accent))
                        Text("I wrote the key down", fontSize = 14.sp)
                    }
                } else {
                    OutlineButton("Make a backup key (recommended)", small = true) {
                        m.run { key = Bridge.call("backup_new_key") as String; ack = false }
                    }
                }
                Field(null, own, { own = it }, password = true, placeholder = "or type your own passphrase (14+ characters)")
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text("Include my message history", fontSize = 14.sp)
                        Muted("Makes the file larger and more valuable to anyone who gets it. Files are not included.", 12.5.sp)
                    }
                    Switch(history, { history = it }, colors = SwitchDefaults.colors(checkedTrackColor = C.Accent))
                }
                val pass = own.ifEmpty { key ?: "" }
                PrimaryButton("Save backup file", enabled = !m.busy && pass.isNotEmpty() && (own.isNotEmpty() || ack)) {
                    m.run {
                        val out = File(backupDir(ctx), EXPORT_NAME)
                        Bridge.call("backup_export", args("path" to out.absolutePath, "passphrase" to pass, "history" to history))
                        ready = out
                        key = null; own = ""; ack = false
                        status = Bridge.obj("backup_status")
                    }
                }
            }
        }
    }
}

/** The create screen's other way in: bring an identity back from a backup file. */
@Composable
fun RestoreScreen(m: AppModel, onBack: () -> Unit) {
    val ctx = LocalContext.current
    var picked by remember { mutableStateOf<File?>(null) }
    var pickedName by remember { mutableStateOf("") }
    var pass by remember { mutableStateOf("") }
    var summary by remember { mutableStateOf<JSONObject?>(null) }
    var relay by remember { mutableStateOf("") }
    var dev1 by remember { mutableStateOf("") }
    var dev2 by remember { mutableStateOf("") }
    val inbox = File(ctx.cacheDir, "restore").apply { mkdirs() }
    DisposableEffect(Unit) { onDispose { inbox.listFiles()?.forEach { it.delete() } } }
    val pick = rememberLauncherForActivityResult(ActivityResultContracts.GetContent()) { uri ->
        if (uri != null) {
            try {
                val out = File(inbox, "backup.spikebackup")
                ctx.contentResolver.openInputStream(uri)?.use { input -> out.outputStream().use { input.copyTo(it) } }
                picked = out
                pickedName = uri.lastPathSegment?.substringAfterLast('/') ?: "backup file"
            } catch (e: Exception) {
                m.notice = "Could not read that file: ${e.message}"
            }
        }
    }
    fun leave() {
        m.run { Bridge.call("backup_cancel") }
        onBack()
    }

    Page {
        TopBar("Restore from a backup", { leave() })
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).navigationBarsPadding().padding(horizontal = 20.dp, vertical = 16.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            val s = summary
            if (s == null) {
                Text("Choose the backup file you saved and type its key or passphrase.")
                OutlineButton(if (picked == null) "Choose backup file" else "File: $pickedName", Modifier.fillMaxWidth()) { pick.launch("*/*") }
                Field("Backup key or passphrase", pass, { pass = it }, password = true)
                PrimaryButton("Open backup", enabled = !m.busy && picked != null && pass.isNotEmpty()) {
                    m.run {
                        val o = Bridge.obj("backup_open", args("path" to picked!!.absolutePath, "passphrase" to pass))
                        // The engine holds what it needs now; the copy of the file goes.
                        picked?.delete()
                        relay = o.optString("relay")
                        summary = o
                    }
                }
            } else {
                val n = { k: String -> s.getInt(k) }
                Text(
                    "Backup made ${stamp(s.getLong("created_ms"))}: ${n("contacts")} contact${if (n("contacts") == 1) "" else "s"}, " +
                        "${n("groups")} group${if (n("groups") == 1) "" else "s"}, ${n("messages")} message${if (n("messages") == 1) "" else "s"}.",
                )
                CardBox {
                    Text("Restoring signs out every other device that uses this identity, including the one you lost. You can link them again afterwards. This needs the relay to be reachable.", fontSize = 13.5.sp)
                }
                Field("Relay", relay, { relay = it })
                Field("Choose a passphrase for this phone", dev1, { dev1 = it }, password = true, helper = "10 characters or more. A new passphrase for this phone, separate from the backup's.")
                Field("Passphrase again", dev2, { dev2 = it }, password = true)
                PrimaryButton("Restore my identity", enabled = !m.busy && dev1.isNotEmpty()) {
                    if (dev1 != dev2) {
                        m.notice = "The two passphrases differ."
                    } else {
                        m.run {
                            Bridge.call("backup_restore", args("passphrase" to dev1, "relay" to relay.trim()))
                            m.loadStatus()
                        }
                    }
                }
            }
            OutlineButton("Cancel", Modifier.fillMaxWidth()) { leave() }
        }
    }
}
