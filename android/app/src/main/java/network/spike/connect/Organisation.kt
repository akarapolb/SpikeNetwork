package network.spike.connect

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.Stable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.launch
import java.text.DateFormat
import java.util.Date

/** What joining would do, as `preview_join` answers it (ADR 0023). */
data class JoinPreview(val tenant: String, val relay: String, val relayHost: String, val validUntilMs: Long)

/** The join step's state, kept by the Home relay page: closed, the form, or the confirmation. */
@Stable
class JoinState {
    var open by mutableStateOf(false)
    var token by mutableStateOf("")
    var url by mutableStateOf("wss://")
    var err by mutableStateOf<String?>(null)
    var preview by mutableStateOf<JoinPreview?>(null)
    var busy by mutableStateOf(false)
    var scanning by mutableStateOf(false)
    fun close() { open = false; token = ""; url = "wss://"; err = null; preview = null; busy = false; scanning = false }
}

private fun copyText(ctx: Context, text: String) {
    val cm = ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    cm.setPrimaryClip(ClipData.newPlainText("Spike", text))
}

/**
 * Below the home relay: the organisation this phone is enrolled with, or the way to join one. The relay
 * learns that this device id belongs to the organisation, and nothing about the person.
 */
@Composable
fun OrganisationSection(m: AppModel, o: Overview, j: JoinState) {
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val org = o.organisation
    if (org != null) {
        CardBox {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Column(Modifier.weight(1f)) {
                    Text("Organisation · $org", fontWeight = FontWeight.Medium)
                    Muted("This phone is enrolled on this relay. The relay knows it belongs to $org, and nothing about you.", 12.5.sp)
                }
            }
            OutlineButton("Forget token", small = true) {
                m.run { Bridge.call("leave_relay"); m.notice = "Token forgotten. The organisation can remove this phone from its relay."; m.refresh() }
            }
        }
        return
    }
    CardBox {
        if (!j.open) {
            Text("Join an organisation's relay", fontWeight = FontWeight.Medium)
            Muted("For a relay your organisation runs. Its admin gives you a token made for this phone.", 12.5.sp)
            OutlineButton("Join", small = true) { j.open = true }
            return@CardBox
        }
        Text("1 · This phone's device id", fontFamily = Mono, fontSize = 11.sp, color = C.Muted, letterSpacing = 0.8.sp)
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                o.me.deviceId, fontFamily = Mono, fontSize = 12.5.sp, color = C.AvatarText,
                modifier = Modifier.weight(1f).clip(RoundedCornerShape(10.dp)).background(C.Subtle).padding(horizontal = 12.dp, vertical = 9.dp),
            )
            OutlineButton("Copy", small = true) { copyText(ctx, o.me.deviceId); m.notice = "Device id copied. Send it to your organisation's admin." }
        }
        Muted("Send it to your organisation's admin. A token works for one device only, so it is useless to anyone else.", 12.5.sp)
        Text("2 · The token and relay they send back", fontFamily = Mono, fontSize = 11.sp, color = C.Muted, letterSpacing = 0.8.sp, modifier = Modifier.padding(top = 6.dp))
        if (j.scanning) {
            QrScanner(Modifier.fillMaxWidth().height(260.dp)) { text -> j.scanning = false; j.token = text.trim() }
            OutlineButton("Stop scanning", small = true) { j.scanning = false }
        } else {
            Field(null, j.token, { j.token = it }, placeholder = "spike-enroll1:…", singleLine = false, minHeight = 72.dp, maxLines = 4)
            OutlineButton("Scan the token's code", small = true) { j.scanning = true }
        }
        Field("Organisation relay address", j.url, { j.url = it }, placeholder = "wss://relay.example")
        j.err?.let { Text(it, color = C.Danger, fontSize = 13.sp) }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Box(
                Modifier.height(36.dp).clip(RoundedCornerShape(10.dp)).background(C.Accent).clickable {
                    scope.launch {
                        try {
                            j.err = null
                            val p = Bridge.obj("preview_join", args("token" to j.token.trim(), "url" to j.url.trim()))
                            j.token = j.token.trim()
                            j.preview = JoinPreview(p.getString("tenant"), p.getString("relay"), p.getString("relay_host"), p.getLong("valid_until_ms"))
                        } catch (e: Exception) { j.err = e.message }
                    }
                }.padding(horizontal = 16.dp),
                contentAlignment = Alignment.Center,
            ) { Text("Continue", color = C.OnAccent, fontWeight = FontWeight.SemiBold, fontSize = 13.sp) }
            OutlineButton("Cancel", small = true) { j.close() }
        }
    }
}

/** The design's "Joining an organisation's relay" screen: who, where, what it means, then "Use this relay". */
@Composable
fun JoinConfirm(m: AppModel, j: JoinState) {
    val p = j.preview ?: return
    val scope = rememberCoroutineScope()
    val until = remember(p.validUntilMs) { DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT).format(Date(p.validUntilMs)) }
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        // Back from the confirmation returns to the form, not out of Settings.
        androidx.activity.compose.BackHandler { j.preview = null }
        Column(Modifier.fillMaxWidth().padding(bottom = 6.dp), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Avatar(p.tenant, 64.dp, square = true)
            Text("Join ${p.tenant}'s relay", fontSize = 20.sp, fontWeight = FontWeight.SemiBold, textAlign = TextAlign.Center)
            Muted(p.relayHost, 12.5.sp)
        }
        Column(Modifier.fillMaxWidth()) {
            KvRow("Enrols", "This phone only")
            KvRow("Token valid until", until)
            KvRow("${p.tenant} can see", "That this device exists")
            KvRow("${p.tenant} cannot see", "Your name, contacts, messages")
        }
        j.err?.let { Text(it, color = C.Danger, fontSize = 13.sp) }
        Spacer(Modifier.height(8.dp))
        PrimaryButton(if (j.busy) "Joining…" else "Use this relay", enabled = !j.busy) {
            j.busy = true
            scope.launch {
                try {
                    val t = Bridge.call("join_relay", args("token" to j.token, "url" to p.relay)) as String
                    j.close()
                    m.notice = "Joined $t's relay. It is your home relay now."
                    m.relayOk = true
                    m.refresh()
                } catch (e: Exception) {
                    j.busy = false; j.preview = null; j.err = e.message
                }
            }
        }
        TextLink("Not now") { j.close() }
    }
}
