package network.spike.connect

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp
import java.text.DateFormat
import java.util.Date

/**
 * When we last heard from a device, from the local arrival time of its last message (0: never).
 * Only what this phone itself received: nothing comes from the relay.
 */
internal fun heardLabel(ms: Long, now: Long = System.currentTimeMillis()): String {
    if (ms <= 0L) return "Not heard from yet"
    val ago = (now - ms).coerceAtLeast(0)
    val min = ago / 60_000
    return when {
        min < 2 -> "active now"
        min < 60 -> "$min min ago"
        min < 24 * 60 -> "${min / 60} h ago"
        min < 48 * 60 -> "yesterday"
        else -> DateFormat.getDateInstance(DateFormat.MEDIUM).format(Date(ms))
    }
}

/** As in the design's "Contact details": who they are, how to reach them, the safety number and each of their devices. */
@Composable
fun ContactInfoScreen(m: AppModel, name: String) {
    val o = m.overview ?: return
    val c = o.contacts.firstOrNull { it.name == name } ?: run { m.open(null); return }
    Page(C.Surface) {
        Row(
            Modifier.fillMaxWidth().statusBarsPadding().padding(start = 4.dp, end = 12.dp, top = 4.dp, bottom = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(Modifier.size(42.dp).clip(CircleShape).clickable { m.open(Screen.Chat(name)) }.semantics { contentDescription = "Back" }, contentAlignment = Alignment.Center) { Ico(Ic.Back) }
        }
        Column(Modifier.weight(1f).fillMaxWidth().verticalScroll(rememberScrollState())) {
            Column(
                Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 8.dp),
                horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Avatar(name, 72.dp)
                Text(name, fontFamily = Geist, fontWeight = FontWeight.SemiBold, fontSize = 20.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                Row(
                    Modifier.clip(RoundedCornerShape(99.dp)).background(C.Subtle).padding(horizontal = 10.dp, vertical = 4.dp),
                    verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.dp),
                ) {
                    if (c.verified) Ico(Ic.ShieldCheck, C.Green, 15.dp)
                    Text(if (c.verified) "Verified" else "Not verified", fontSize = 12.5.sp, color = if (c.verified) C.Green else C.Danger)
                }
            }
            // Message, call, video: as in the design's quick actions.
            Row(Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 14.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                @Composable
                fun Quick(label: String, icon: androidx.compose.ui.graphics.vector.ImageVector, onClick: () -> Unit) {
                    Column(
                        Modifier.weight(1f).clip(RoundedCornerShape(13.dp)).background(C.Subtle).clickable(onClick = onClick).padding(vertical = 12.dp),
                        horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(4.dp),
                    ) { Ico(icon, C.Text, 20.dp); Text(label, fontSize = 12.5.sp, fontWeight = FontWeight.Medium) }
                }
                Quick("Message", Ic.Chat) { m.open(Screen.Chat(name)) }
                Quick("Call", IcHandset) { CallManager.call(name, c.verified, false) }
                Quick("Video", Ic.Video) { CallManager.call(name, c.verified, true) }
            }
            Box(Modifier.fillMaxWidth().height(1.dp).background(C.Border))
            Row(
                Modifier.fillMaxWidth().clickable { m.open(Screen.Verify(name)) }.padding(horizontal = 20.dp, vertical = 14.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text("Safety number", fontWeight = FontWeight.Medium, modifier = Modifier.weight(1f))
                Ico(Ic.Chevron, C.Muted, 18.dp)
            }
            Box(Modifier.fillMaxWidth().height(1.dp).background(C.Border))
            Text(
                "DEVICES · ${c.devices}", fontFamily = Mono, fontWeight = FontWeight.Medium, fontSize = 11.sp, letterSpacing = 0.08.em, color = C.Muted,
                modifier = Modifier.padding(start = 20.dp, end = 20.dp, top = 18.dp, bottom = 4.dp),
            )
            // The signed list has keys only, no names or types: each device is shown by its short id.
            for (d in c.deviceList) {
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    Box(Modifier.size(38.dp).clip(RoundedCornerShape(12.dp)).background(C.Subtle), contentAlignment = Alignment.Center) { Ico(Ic.PhoneDevice, C.AvatarText, 18.dp) }
                    Column(Modifier.weight(1f)) {
                        Text("Device ${d.id.take(6)}", fontWeight = FontWeight.Medium)
                        Text(d.id.chunked(4).joinToString(" "), fontFamily = Mono, fontSize = 11.5.sp, color = C.Muted)
                    }
                    Text(heardLabel(d.lastHeard), fontSize = 13.sp, color = if (d.lastHeard > 0) C.AvatarText else C.Muted)
                }
            }
            Muted(
                "Last heard is when a message from that device last reached this phone. The relay is never asked.",
                12.5.sp, Modifier.padding(horizontal = 20.dp, vertical = 10.dp),
            )
        }
    }
}
