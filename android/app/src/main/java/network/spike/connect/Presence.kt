package network.spike.connect

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import org.json.JSONObject

// Read receipts, online now and typing (ADR 0037). The engine enforces the rules (off by default, reciprocal,
// online only to a verified contact); these screens only ask, and the engine says no when a switch is off.

/** What a contact shares, as the engine lets this device show it. */
data class PeerShown(val readUpto: Long? = null, val readAt: Long? = null, val online: Boolean = false, val typing: Boolean = false)

private fun JSONObject.optLongOrNull(k: String): Long? = if (isNull(k)) null else optLong(k)

/** The contact's status, read again every two seconds while their conversation is open (typing goes stale by itself). */
@Composable
fun rememberPeerStatus(contact: String?): PeerShown {
    var shown by remember(contact) { mutableStateOf(PeerShown()) }
    if (contact != null) LaunchedEffect(contact) {
        while (true) {
            shown = try {
                val o = Bridge.obj("peer_status", args("name" to contact))
                PeerShown(o.optLongOrNull("read_upto"), o.optLongOrNull("read_at"), o.optBoolean("online"), o.optBoolean("typing"))
            } catch (_: Exception) { PeerShown() }
            delay(2000)
        }
    }
    // "Online": told when the conversation opens and once a minute while it stays open (the engine sends nothing
    // unless the switch is on and the contact is verified).
    if (contact != null) LaunchedEffect(contact) {
        while (true) {
            try { Bridge.call("send_presence", args("name" to contact, "what" to "online")) } catch (_: Exception) {}
            delay(60_000)
        }
    }
    return shown
}

/** Returns a function to call with "I am typing" (true) or "I stopped" (false): at most one signal every five seconds. */
@Composable
fun rememberTypingSender(contact: String?): (Boolean) -> Unit {
    val scope = rememberCoroutineScope()
    var lastOn by remember(contact) { mutableStateOf(0L) }
    return remember(contact) {
        { on: Boolean ->
            if (contact != null) {
                val now = System.currentTimeMillis()
                if (on && now - lastOn > 5000) {
                    lastOn = now
                    scope.launch { try { Bridge.call("send_presence", args("name" to contact, "what" to "typing")) } catch (_: Exception) {} }
                } else if (!on && lastOn != 0L) {
                    lastOn = 0L
                    scope.launch { try { Bridge.call("send_presence", args("name" to contact, "what" to "typing_off")) } catch (_: Exception) {} }
                }
            }
        }
    }
}

/** Settings > What others see: the prototype's three switches, all off until the owner turns them on. */
@Composable
fun WhatOthersSee(m: AppModel) {
    var receipts by remember { mutableStateOf(false) }
    var online by remember { mutableStateOf(false) }
    var typing by remember { mutableStateOf(false) }
    var loaded by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) {
        try {
            val s = Bridge.obj("presence_share")
            receipts = s.optBoolean("receipts"); online = s.optBoolean("online"); typing = s.optBoolean("typing")
        } catch (_: Exception) {}
        loaded = true
    }
    val save = { m.run { Bridge.call("set_presence_share", args("receipts" to receipts, "online" to online, "typing" to typing)) } }
    Muted("All off until you turn them on. You see a contact's only while you share the same.", 13.sp)
    @Composable
    fun Row(title: String, help: String?, on: Boolean, set: (Boolean) -> Unit) {
        CardBox {
            Row(
                Modifier.fillMaxWidth().toggleable(on, enabled = loaded, role = Role.Switch) { set(it); save() },
                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Column(Modifier.weight(1f).padding(end = 4.dp)) {
                    Text(title, fontWeight = FontWeight.Medium)
                    if (help != null) Muted(help, 13.sp)
                }
                Switch(on, null, enabled = loaded, colors = SwitchDefaults.colors(checkedTrackColor = C.Accent))
            }
        }
    }
    Row("Read receipts", "Contacts see when you have read. You see theirs only if they share too.", receipts) { receipts = it }
    Row("Online now", "Shown to verified contacts only, while their conversation is open.", online) { online = it }
    Row("Typing", null, typing) { typing = it }
    Muted("They travel inside encrypted messages. A relay sees only that a message went.", 12.5.sp)
}
