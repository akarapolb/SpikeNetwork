package network.spike.connect

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
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
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.json.JSONObject
import java.text.DateFormat
import java.util.Calendar
import java.util.Date

/*
 * Organisations (ADR 0035) on the phone: joining an organisation's relay with a token, and the rules it set,
 * which the app carries out here. The organisation sees that this device exists, never names, contacts or
 * messages.
 */

/** The organisation's "lock when idle" rule, for the auto-lock in MainActivity. Null: no rule. */
object OrgRules {
    @Volatile var lockMinutes: Int? = null
}

data class OrgInfo(
    val orgName: String, val relay: String, val fingerprint: String, val removed: Boolean, val previousRelay: String,
    val lock: Int?, val keepDays: Int?, val backups: String?, val agents: String?, val version: Long,
)

private fun JSONObject.optIntOrNull(k: String): Int? = if (has(k) && !isNull(k)) getInt(k) else null
private fun JSONObject.optStrOrNull(k: String): String? = if (has(k) && !isNull(k)) getString(k) else null

suspend fun loadOrg(): OrgInfo? {
    val o = Bridge.call("org_status") as? JSONObject ?: run { OrgRules.lockMinutes = null; return null }
    val r = if (o.isNull("rules")) null else o.getJSONObject("rules")
    val info = OrgInfo(
        o.getString("org_name"), o.getString("relay"), o.getString("admin_fingerprint"), o.getBoolean("removed"),
        o.optString("previous_relay"), r?.optIntOrNull("lock_idle_minutes"), r?.optIntOrNull("keep_messages_days"),
        r?.optStrOrNull("backups"), r?.optStrOrNull("agents"), r?.optLong("version") ?: 0,
    )
    OrgRules.lockMinutes = if (info.removed) null else info.lock
    return info
}

private fun untilText(secs: Long): String {
    val d = Date(secs * 1000)
    val now = Calendar.getInstance()
    val c = Calendar.getInstance().apply { time = d }
    val t = DateFormat.getTimeInstance(DateFormat.SHORT).format(d)
    val sameDay = { a: Calendar, b: Calendar -> a.get(Calendar.YEAR) == b.get(Calendar.YEAR) && a.get(Calendar.DAY_OF_YEAR) == b.get(Calendar.DAY_OF_YEAR) }
    val tomorrow = (now.clone() as Calendar).apply { add(Calendar.DAY_OF_YEAR, 1) }
    return when {
        sameDay(c, now) -> "Today $t"
        sameDay(c, tomorrow) -> "Tomorrow $t"
        else -> DateFormat.getDateInstance(DateFormat.MEDIUM).format(d) + " " + t
    }
}

@Composable
private fun OrgKv(k: String, v: String) {
    Box(Modifier.fillMaxWidth().height(1.dp).background(C.Border))
    Row(Modifier.fillMaxWidth().padding(vertical = 10.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Muted(k, 14.sp, Modifier.weight(1f))
        Text(v, fontSize = 14.sp, textAlign = TextAlign.End)
    }
}

/** On the Home relay page: join an organisation, or see who manages this phone. */
@Composable
fun OrgRelayEntry(m: AppModel, open: (String) -> Unit) {
    var org by remember { mutableStateOf<OrgInfo?>(null) }
    var loaded by remember { mutableStateOf(false) }
    LaunchedEffect(m.overview) { org = try { loadOrg() } catch (_: Exception) { null }; loaded = true }
    if (!loaded) return
    CardBox {
        val o = org
        if (o == null) {
            Text("Join an organisation", fontWeight = FontWeight.Medium)
            Muted("Scan or paste the token your organisation sent you. It enrols this phone only.", 13.sp)
            OutlineButton("Join…", small = true) { open("orgjoin") }
        } else {
            Text(if (o.removed) "${o.orgName} removed this phone" else "Managed by ${o.orgName}", fontWeight = FontWeight.Medium, color = if (o.removed) C.Danger else C.Text)
            Muted(if (o.removed) "Its rules no longer apply. Leave to go back to the relay you used before." else "Rules your organisation set for this phone.", 13.sp)
            OutlineButton(if (o.removed) "Leave ${o.orgName}'s relay" else "See rules", small = true, danger = o.removed) { open("orgrules") }
        }
    }
}

/** "Joining an organisation's relay": paste or scan the token, then the sheet from the design. */
@Composable
fun OrgJoinPage(m: AppModel, done: () -> Unit) {
    var token by remember { mutableStateOf("") }
    var scanning by remember { mutableStateOf(false) }
    var info by remember { mutableStateOf<JSONObject?>(null) }
    var err by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    val ctx = LocalContext.current
    val i = info
    if (i == null) {
        Muted("Nothing is sent until you choose to use the relay.", 14.sp)
        if (scanning) {
            QrScanner(Modifier.fillMaxWidth().heightIn(min = 280.dp)) { t ->
                token = t; scanning = false
                m.run { try { info = Bridge.obj("org_inspect", args("token" to t)); err = null } catch (e: BridgeError) { err = e.message } }
            }
            OutlineButton("Paste the token instead", Modifier.fillMaxWidth()) { scanning = false }
        } else {
            Field("Organisation token", token, { token = it }, singleLine = false, minHeight = 96.dp, placeholder = "spikeorg1:…")
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlineButton("Paste", small = true) {
                    val cm = ctx.getSystemService(android.content.Context.CLIPBOARD_SERVICE) as android.content.ClipboardManager
                    cm.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(ctx)?.toString()?.trim()?.let { token = it }
                }
                OutlineButton("Scan", small = true) { scanning = true }
                OutlineButton("Continue", small = true, enabled = token.isNotBlank()) {
                    m.run { try { info = Bridge.obj("org_inspect", args("token" to token.trim())); err = null } catch (e: BridgeError) { err = e.message } }
                }
            }
        }
        err?.let { Text(it, color = C.Danger, fontSize = 13.5.sp) }
        return
    }
    val name = i.getString("org_name")
    Column(Modifier.fillMaxWidth().padding(top = 8.dp), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Avatar(name, 64.dp, square = true)
        Text("Join $name's relay", fontSize = 20.sp, fontWeight = FontWeight.SemiBold)
        Muted(i.getString("relay").removePrefix("wss://").removePrefix("ws://"), 13.sp)
    }
    Column {
        OrgKv("Enrols", "This phone only")
        OrgKv("Token valid until", untilText(i.getLong("valid_until")))
        OrgKv("$name can see", "That this device exists")
        OrgKv("$name cannot see", "Your name, contacts, messages")
    }
    Muted("The name is the one the organisation gave itself. Its key: ${i.getString("admin_fingerprint")}.", 12.5.sp)
    err?.let { Text(it, color = C.Danger, fontSize = 13.5.sp) }
    PrimaryButton(if (busy) "Joining…" else "Use this relay", enabled = !busy) {
        busy = true
        m.run {
            try {
                Bridge.call("org_join", args("token" to token.trim()))
                loadOrg()
                m.notice = "This phone now uses $name's relay."
                m.refresh()
                done()
            } catch (e: BridgeError) { err = e.message } finally { busy = false }
        }
    }
    Box(Modifier.fillMaxWidth().clickable { done() }.padding(vertical = 12.dp), contentAlignment = Alignment.Center) { Text("Not now", fontSize = 15.sp, color = C.Muted) }
}

private fun ruleText(kind: String, v: String?): String = when (kind) {
    "backups" -> when (v) { "allowed" -> "Allowed"; "key_required" -> "Allowed, key required"; "off" -> "Not allowed"; else -> "No rule" }
    else -> when (v) { "any" -> "Any"; "verified_only" -> "Verified only"; "off" -> "Not allowed"; else -> "No rule" }
}

/** "Organisation rules": what the organisation set for this phone, and leaving. */
@Composable
fun OrgRulesPage(m: AppModel, done: () -> Unit) {
    var org by remember { mutableStateOf<OrgInfo?>(null) }
    LaunchedEffect(Unit) { org = try { loadOrg() } catch (_: Exception) { null } }
    val o = org ?: return
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Text("Managed by ${o.orgName}", fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
        Muted("Rules your organisation set for this phone", 13.sp)
    }
    if (o.removed) Text("${o.orgName} removed this phone. Its rules no longer apply.", color = C.Danger, fontSize = 14.sp)
    Column {
        OrgKv("Lock when idle", o.lock?.let { if (it >= 60 && it % 60 == 0) "${it / 60} hour${if (it > 60) "s" else ""}" else "$it minute${if (it > 1) "s" else ""}" } ?: "No rule")
        OrgKv("Messages kept on this phone", o.keepDays?.let { "$it days" } ?: "No rule")
        OrgKv("Backups", ruleText("backups", o.backups))
        OrgKv("Agents", ruleText("agents", o.agents))
        Box(Modifier.fillMaxWidth().height(1.dp).background(C.Border))
    }
    Muted("These rules are enforced by the app on your device. ${o.orgName} cannot read your messages or see who you talk to.", 13.sp)
    Muted("Signed by ${o.fingerprint}${if (o.version > 0) " · version ${o.version}" else ""}", 12.sp)
    Spacer(Modifier.height(24.dp))
    Box(Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).clickable {
        m.run {
            Bridge.call("org_leave", args("next" to null))
            loadOrg()
            m.notice = "Left ${o.orgName}'s relay."
            m.refresh()
            done()
        }
    }.padding(vertical = 14.dp), contentAlignment = Alignment.Center) { Text("Leave ${o.orgName}'s relay", color = C.Danger, fontSize = 15.sp, fontWeight = FontWeight.Medium) }
}
