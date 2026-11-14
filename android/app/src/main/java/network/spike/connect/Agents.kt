package network.spike.connect

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.input.KeyboardType
import org.json.JSONArray
import org.json.JSONObject
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp
import java.text.DateFormat
import java.util.Date

// ---- agents, as in the design: the tab lists requests and grants; a request, a grant and the log each open on their own

/** An amount in the request's currency; a grant's limits have none, so they show as plain units. */
private fun amt(cents: Long, cur: String? = null): String = String.format(java.util.Locale.US, "%d.%02d", cents / 100, cents % 100) + if (cur != null) " $cur" else ""

private fun grantLine(g: Grant) = if (g.maxCents == null) "${g.scopes.joinToString(", ")} · no payments" else "Pay up to ${amt(g.maxCents)} · ${g.perHour} an hour"

/** A typed payment leads with what and how much, and says to whom underneath ("Pay 286.00 EUR" / "to harbor-travel"). */
private fun headline(r: AgentAsk): Pair<String, String?> {
    val s = r.summary ?: return "“${r.note}”" to null
    val m = Regex("^(.+?) to (\\S+)$").find(s) ?: return s to null
    return m.groupValues[1] to "to ${m.groupValues[2]}"
}

private fun inWords(ms: Long): String {
    val m = ((ms - System.currentTimeMillis()) / 60_000).toInt()
    return when { m <= 0 -> "now"; m < 60 -> "in $m min"; m < 1440 -> "in ${(m + 30) / 60} h"; else -> "in ${(m + 720) / 1440} days" }
}

@Composable
private fun Cap(text: String, modifier: Modifier = Modifier) =
    Text(text.uppercase(), fontFamily = Mono, fontWeight = FontWeight.Medium, fontSize = 11.sp, letterSpacing = 0.08.em, color = C.Muted, modifier = modifier)

/** An agent's mark: the sparkle in a rounded square, as agents are drawn everywhere. */
@Composable
private fun AgentTile(size: Dp = 42.dp) =
    Box(Modifier.size(size).clip(RoundedCornerShape(size * 0.3f)).background(C.AvatarBg), contentAlignment = Alignment.Center) { Ico(Ic.Sparkle, C.AvatarText, size * 0.45f) }

@Composable
private fun AgentRow(name: String, preview: String, amber: Boolean, time: String, waiting: Boolean, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).clickable(onClick = onClick).padding(horizontal = 10.dp, vertical = 9.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        AgentTile()
        Column(Modifier.weight(1f)) {
            Text(name, fontSize = 14.sp, fontWeight = if (waiting) FontWeight.SemiBold else FontWeight.Medium, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(preview, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.Ellipsis, color = if (amber) C.AmberText else C.Muted, fontWeight = if (amber) FontWeight.Medium else FontWeight.Normal)
        }
        Column(horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(5.dp), modifier = Modifier.align(Alignment.Top).padding(top = 2.dp)) {
            Text(time, fontSize = 11.5.sp, color = C.Muted)
            if (waiting) Box(Modifier.size(8.dp).clip(CircleShape).background(C.AmberText).semantics { contentDescription = "Waiting for you" })
        }
    }
}

@Composable
fun AgentsTab(m: AppModel) {
    val a = m.agents
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).statusBarsPadding().padding(bottom = 110.dp)) {
        Box(Modifier.padding(start = 20.dp, end = 20.dp, top = 14.dp, bottom = 10.dp)) { H1("Agents", 28.sp) }
        if (a == null) { Muted("Loading…", 14.sp, Modifier.padding(20.dp)); return@Column }
        if (a.requests.isEmpty() && a.grants.isEmpty()) {
            EmptyState(Ic.Sparkle, "No agents yet. Give an agent a standing permission first; its requests then appear here for you to approve.")
            if (a.approver) PrimaryButton("Give an agent a grant", modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp)) { m.open(Screen.NewGrant) }
            return@Column
        }
        val waiting = a.requests.filter { it.status == "waiting" }
        val done = a.requests.filter { it.status != "waiting" }.take(10)
        Cap("Waiting for you", Modifier.padding(start = 20.dp, end = 20.dp, bottom = 6.dp))
        Column(Modifier.padding(horizontal = 10.dp)) {
            if (waiting.isEmpty()) Muted("Nothing waiting", 13.sp, Modifier.padding(horizontal = 10.dp, vertical = 4.dp))
            for (r in waiting) AgentRow(r.from, r.summary ?: "“${r.note}”", true, listTime(r.received), true) { m.open(Screen.AgentRequest(r.id)) }
        }
        Cap("Allowed", Modifier.padding(start = 20.dp, end = 20.dp, top = 16.dp, bottom = 6.dp))
        Column(Modifier.padding(horizontal = 10.dp)) {
            if (a.grants.isEmpty()) Muted("No agent may ask yet", 13.sp, Modifier.padding(horizontal = 10.dp, vertical = 4.dp))
            for (g in a.grants) AgentRow(g.agent, grantLine(g), false, "", false) { m.open(Screen.AgentGrant(g.agent)) }
            if (a.approver) Text(
                "Give an agent a grant", fontSize = 14.sp, fontWeight = FontWeight.Medium,
                modifier = Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).clickable { m.open(Screen.NewGrant) }.padding(horizontal = 10.dp, vertical = 12.dp),
            )
        }
        if (done.isNotEmpty()) {
            Cap("Decided", Modifier.padding(start = 20.dp, end = 20.dp, top = 16.dp, bottom = 6.dp))
            Column(Modifier.padding(horizontal = 10.dp)) {
                for (r in done) AgentRow(r.from, "${headline(r).first} · ${if (r.status == "approved") "approved" else "denied"}", false, listTime(r.received), false) { m.open(Screen.AgentRequest(r.id)) }
            }
        }
        if (a.logOk != null) OutlineButton("Activity log", Modifier.fillMaxWidth().padding(start = 20.dp, end = 20.dp, top = 16.dp)) { m.open(Screen.AgentLog) }
    }
}

/** A request, as the design's approval sheet: who asks, the signed action large, its facts, the agent's own words set
 *  apart as unverified, and Approve and Deny at the bottom. */
@Composable
fun AgentRequestScreen(m: AppModel, id: String) {
    val a = m.agents
    val r = a?.requests?.firstOrNull { it.id == id }
    if (r == null) {
        StepPage({ m.open(null) }) { Muted(if (a == null) "Loading…" else "This request is no longer here.", 14.sp) }
        return
    }
    val cur = r.typed["currency"]
    val g = a.grants.firstOrNull { it.agent == r.from }
    val waiting = r.status == "waiting"
    val (head, sub) = headline(r)
    fun decide(approve: Boolean) = m.run { m.notice = Bridge.call("decide", args("id" to r.id, "approve" to approve)) as? String; m.refresh() }
    StepPage(
        { m.open(null) },
        actions = {
            if (waiting) {
                PrimaryButton(if (r.amountCents != null) "Approve ${amt(r.amountCents, cur)}" else "Approve", enabled = !m.busy) { decide(true) }
                Text(
                    "Deny", fontWeight = FontWeight.Medium, fontSize = 15.sp, textAlign = TextAlign.Center,
                    modifier = Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).clickable(enabled = !m.busy) { decide(false) }.padding(vertical = 12.dp),
                )
            }
        },
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            AgentTile(28.dp)
            Text("${r.from} asks to", color = C.AvatarText, fontSize = 13.sp)
        }
        Column {
            Text(head, fontFamily = Geist, fontWeight = FontWeight.SemiBold, fontSize = if (r.summary != null) 32.sp else 22.sp, letterSpacing = (-0.04).em, lineHeight = if (r.summary != null) 35.sp else 28.sp)
            if (sub != null) Text(sub, color = C.AvatarText, fontSize = 15.sp, modifier = Modifier.padding(top = 6.dp))
        }
        if (!waiting) {
            val ok = r.status == "approved"
            Text(
                (if (ok) "Approved" else "Denied") + (r.by?.let { " by $it" } ?: ""), fontSize = 12.5.sp, fontWeight = FontWeight.Medium,
                color = if (ok) C.Green else C.RedText,
                modifier = Modifier.clip(RoundedCornerShape(999.dp)).background(if (ok) C.Subtle else C.RedBg).padding(horizontal = 10.dp, vertical = 3.dp),
            )
        }
        Column {
            // A grant's ceiling refuses anything above it without asking; below the threshold it approves alone.
            if (g?.maxCents != null) KvRow("Your limit", "${amt(g.maxCents, cur)} a request")
            if (g?.maxCents != null && r.amountCents != null) KvRow("Asks you above", g.autoBelowCents?.let { amt(it, cur) } ?: "Every payment")
            KvRow("Request", if (r.summary != null) "Signed, typed action" else "Free text, not checked")
            for ((k, v) in r.typed) if (k != "currency") KvRow(k.replaceFirstChar { it.uppercase() }, v)
            KvRow(if (waiting) "Expires" else "Scope", if (waiting) inWords(r.expires) else r.scope)
        }
        if (r.summary != null) {
            Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).background(C.Subtle).padding(horizontal = 12.dp, vertical = 10.dp)) {
                Cap("Agent says · not verified", Modifier.padding(bottom = 4.dp))
                Text(r.note, fontSize = 13.sp, color = C.AvatarText)
            }
        }
        Muted(
            if (waiting) (if (r.forwarded) "Your main device records your answer and sends the signed receipt." else "Your answer is signed by this phone and added to the activity log.")
            else if (r.summary != null) "The service must match the signed fields exactly." else "These were the agent's own words. Only the scope and amount were checked.",
            12.5.sp,
        )
    }
}

/** How the scopes read on the limits screen; anything else shows as its own name. */
private val SCOPE_WORDS = mapOf(
    "payments.send" to "Pay", "files.read" to "Read files", "files.write" to "Write files",
    "files.delete" to "Delete files", "messages.send" to "Send messages",
)
private val KNOWN_SCOPES = SCOPE_WORDS.keys.toList()
fun scopeWords(s: String) = SCOPE_WORDS[s] ?: s

/** "500", "500.5" or "500.00" as cents; blank is none. The first is false when the text is not an amount. */
private fun cents(s: String): Pair<Boolean, Long?> {
    val t = s.trim().replace(",", "")
    if (t.isEmpty()) return true to null
    val m = Regex("^(\\d{1,9})(?:\\.(\\d{1,2}))?$").find(t) ?: return false to null
    return true to (m.groupValues[1].toLong() * 100 + m.groupValues[2].padEnd(2, '0').toLong())
}

/** A number in a row of the limits screen, right-aligned in the mono face as in the design. */
@Composable
private fun NumBox(value: String, onChange: (String) -> Unit, placeholder: String, enabled: Boolean, label: String, decimal: Boolean = true) {
    Box(
        Modifier.width(128.dp).clip(RoundedCornerShape(10.dp)).background(C.Surface)
            .border(1.dp, if (enabled) C.BorderStrong else C.Border, RoundedCornerShape(10.dp)).padding(horizontal = 12.dp, vertical = 9.dp),
        contentAlignment = Alignment.CenterEnd,
    ) {
        BasicTextField(
            value, onChange, enabled = enabled, singleLine = true,
            textStyle = TextStyle(fontFamily = Mono, fontSize = 14.sp, color = C.Text, textAlign = TextAlign.End),
            cursorBrush = SolidColor(C.Accent),
            keyboardOptions = KeyboardOptions(keyboardType = if (decimal) KeyboardType.Decimal else KeyboardType.Number),
            modifier = Modifier.fillMaxWidth().semantics { contentDescription = label },
            decorationBox = { inner ->
                Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.CenterEnd) {
                    if (value.isEmpty()) Text(placeholder, color = C.Muted, fontSize = 13.sp, textAlign = TextAlign.End, maxLines = 1)
                    inner()
                }
            },
        )
    }
}

/** What an agent may do, as the design's "Agent limits": each limit editable in place, saved as a new signed grant.
 *  With no grant yet it is the same form, empty, ending in "Allow". Off the main device it only shows. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun AgentGrantScreen(m: AppModel, agent: String) {
    val a = m.agents
    val g = a?.grants?.firstOrNull { it.agent == agent }
    val canEdit = a?.approver == true
    var stopping by remember { mutableStateOf(false) }
    var types by remember { mutableStateOf(KNOWN_SCOPES) }
    LaunchedEffect(Unit) {
        runCatching { Bridge.arr("action_types") }.getOrNull()?.let { arr -> types = (0 until arr.length()).map { arr.getJSONObject(it).getString("name") } }
    }
    // The form starts from the grant as it is (or empty), and starts again whenever the signed grant changes.
    val daysLeft = g?.let { (((it.expires - System.currentTimeMillis()) + 86_399_999) / 86_400_000).coerceIn(1, 365) } ?: 30L
    var scopes by remember(g) { mutableStateOf(g?.scopes ?: emptyList()) }
    var max by remember(g) { mutableStateOf(g?.maxCents?.let { amt(it) } ?: "") }
    var auto by remember(g) { mutableStateOf(g?.autoBelowCents?.let { amt(it) } ?: "") }
    var daily by remember(g) { mutableStateOf(g?.dailyCents?.let { amt(it) } ?: "") }
    var perHour by remember(g) { mutableStateOf((g?.perHour ?: 10).toString()) }
    var typedOnly by remember(g) { mutableStateOf(g?.typedOnly ?: true) }
    var days by remember(g) { mutableStateOf(daysLeft.toString()) }
    var error by remember(g) { mutableStateOf<String?>(null) }
    val dirty = g == null || scopes.toSet() != g.scopes.toSet() || max != (g.maxCents?.let { amt(it) } ?: "") ||
        auto != (g.autoBelowCents?.let { amt(it) } ?: "") || daily != (g.dailyCents?.let { amt(it) } ?: "") ||
        perHour != g.perHour.toString() || typedOnly != g.typedOnly || days != daysLeft.toString()

    fun save() {
        val (okMax, maxC) = cents(max)
        val (okAuto, autoC) = cents(auto)
        val (okDaily, dailyC) = cents(daily)
        val ph = perHour.trim().toIntOrNull()
        val d = days.trim().toLongOrNull()
        error = when {
            scopes.isEmpty() -> "Choose at least one thing it may do."
            !okMax || !okAuto || !okDaily -> "Write amounts as numbers, like 200 or 200.00."
            maxC == null && (autoC != null || dailyC != null) -> "Set an amount for each request first, or leave all amounts empty for no money."
            autoC != null && maxC != null && autoC > maxC -> "Without asking cannot be more than each request."
            dailyC != null && maxC != null && dailyC < maxC -> "In any 24 hours cannot be less than each request."
            ph == null || ph !in 1..1000 -> "Requests an hour must be between 1 and 1000."
            d == null || d !in 1L..365L -> "A grant lasts 1 to 365 days."
            typedOnly && scopes.any { it !in types } -> "Typed actions only works with the listed actions; turn it off to allow ${scopes.first { it !in types }}."
            else -> null
        }
        if (error != null) return
        m.run {
            Bridge.call("grant", args("request" to JSONObject().apply {
                put("agent", agent); put("scopes", JSONArray(scopes)); put("days", d)
                put("max_cents", maxC ?: JSONObject.NULL); put("auto_below_cents", autoC ?: JSONObject.NULL)
                put("per_hour", ph); put("daily_cents", dailyC ?: JSONObject.NULL); put("typed_only", typedOnly)
            }))
            m.notice = if (g == null) "$agent is allowed." else "Limits saved."
            m.refresh()
        }
    }

    Page {
        TopBar(agent, { m.open(null) }, sub = g?.let { "Allowed until " + DateFormat.getDateInstance(DateFormat.MEDIUM).format(Date(it.expires)) } ?: "Not allowed yet")
        if (a == null) { Muted("Loading…", 14.sp, Modifier.padding(20.dp)); return@Page }
        if (g == null && !canEdit) { Muted("This agent has no grant. Grants are kept on your main device.", 14.sp, Modifier.padding(20.dp)); return@Page }
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState())) {
            @Composable
            fun Line() = Box(Modifier.fillMaxWidth().padding(horizontal = 20.dp).height(1.dp).background(C.Border))

            @Composable
            fun Set(label: String, sub: String?, content: @Composable () -> Unit) {
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    Column(Modifier.weight(1f)) {
                        Text(label, fontSize = 14.sp, fontWeight = FontWeight.Medium)
                        if (sub != null) Text(sub, fontSize = 12.sp, color = C.Muted)
                    }
                    content()
                }
                Line()
            }
            if (g == null) Text(
                "An agent can do nothing until you set its limits. Choose what it may do; amounts left empty mean no money.",
                fontSize = 13.5.sp, color = C.AvatarText, modifier = Modifier.padding(horizontal = 20.dp, vertical = 12.dp),
            )
            Column(Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("May", fontSize = 14.sp, fontWeight = FontWeight.Medium)
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    for (s in (types + scopes).distinct()) {
                        val on = s in scopes
                        Row(
                            Modifier.clip(RoundedCornerShape(999.dp)).border(1.dp, if (on) C.Text else C.BorderStrong, RoundedCornerShape(999.dp))
                                .clickable(enabled = canEdit) { scopes = if (on) scopes - s else scopes + s; error = null }
                                .semantics { selected = on }.padding(horizontal = 11.dp, vertical = 5.dp),
                            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.dp),
                        ) {
                            if (on) Ico(Ic.Check, C.Text, 12.dp)
                            Text(SCOPE_WORDS[s] ?: s, fontSize = 12.5.sp, color = if (on) C.Text else C.AvatarText)
                        }
                    }
                }
            }
            Line()
            Set("Each request", "Above it, refused") { NumBox(max, { max = it; error = null }, "No money", canEdit, "Each request") }
            Set("Without asking", "Up to it, it acts alone") { NumBox(auto, { auto = it; error = null }, "Always asks", canEdit, "Without asking") }
            Set("In any 24 hours", null) { NumBox(daily, { daily = it; error = null }, "No cap", canEdit, "In any 24 hours") }
            Set("Requests an hour", null) { NumBox(perHour, { perHour = it; error = null }, "10", canEdit, "Requests an hour", decimal = false) }
            Set("Typed actions only", "Requests the app checks field by field") {
                Switch(typedOnly, { typedOnly = it; error = null }, enabled = canEdit, colors = SwitchDefaults.colors(checkedTrackColor = C.Accent))
            }
            Set("How long", "Days from now") { NumBox(days, { days = it; error = null }, "30", canEdit, "How long", decimal = false) }
            error?.let {
                Text(
                    it, color = C.RedText, fontSize = 13.sp,
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 10.dp).fillMaxWidth().clip(RoundedCornerShape(12.dp)).background(C.RedBg).padding(12.dp),
                )
            }
            Muted(
                if (canEdit) "Up to the amount without asking, it acts alone. Above it and up to each request, it asks you first. Above that it is refused. Saving signs a new grant that replaces this one."
                else "Up to the amount without asking, it acts alone. Above it, it asks you first. Limits are changed on your main device.",
                12.5.sp, Modifier.padding(horizontal = 20.dp, vertical = 10.dp),
            )
        }
        if (canEdit) Column(Modifier.navigationBarsPadding().padding(start = 24.dp, end = 24.dp, bottom = 12.dp, top = 4.dp)) {
            if (g == null) PrimaryButton("Allow $agent", enabled = !m.busy) { save() }
            else if (dirty) PrimaryButton("Save limits", enabled = !m.busy) { save() }
            if (g != null) TextLink("Stop this agent", danger = true) { stopping = true }
        }
    }
    if (stopping) AlertDialog(
        onDismissRequest = { stopping = false },
        title = { Text("Stop $agent?") },
        text = { Text("It can no longer ask you for anything. Approvals already given stay valid until they expire.") },
        confirmButton = { TextButton({ stopping = false; m.run { Bridge.call("revoke", args("name" to agent)); m.notice = "$agent stopped."; m.open(null) } }) { Text("Stop", color = C.Danger) } },
        dismissButton = { TextButton({ stopping = false }) { Text("Cancel", color = C.Text) } },
    )
}

/** Give a new agent a grant: pick the contact. Agents come first; any contact can be allowed, as on the desktop. */
@Composable
fun NewGrantScreen(m: AppModel) {
    val a = m.agents
    val granted = a?.grants?.map { it.agent }?.toSet() ?: emptySet()
    val contacts = (m.overview?.contacts ?: emptyList()).filter { it.name !in granted }.sortedBy { !it.agent }
    Page {
        TopBar("Give an agent a grant", { m.open(null) }, sub = "Then set what it may do")
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 10.dp, vertical = 8.dp)) {
            AgentRow("Find an agent", "Listed agents, verified by listers you trust", false, "", false) { m.open(Screen.Directory) }
            Cap("Your contacts", Modifier.padding(start = 10.dp, end = 10.dp, top = 14.dp, bottom = 6.dp))
            if (contacts.isEmpty()) {
                Muted("Add the agent as a contact first: scan or paste its card.", 14.sp, Modifier.padding(10.dp))
                OutlineButton("Add a contact", Modifier.fillMaxWidth().padding(10.dp)) { m.addTab = 2; m.open(Screen.AddContact) }
            }
            for (c in contacts) AgentRow(c.name, if (c.agent) "Agent" else if (c.verified) "Contact · verified" else "Contact", false, "", false) { m.open(Screen.AgentGrant(c.name)) }
        }
    }
}

private val LOG_WORDS = mapOf(
    "request" to Triple("asked", "Asked", false), "approved" to Triple("was allowed", "Allowed", false), "denied" to Triple("was denied", "Denied", true),
    "grant" to Triple("was given a grant", "Granted", false), "revoke" to Triple("was stopped", "Revoked", true),
)

/** The signed, hash-chained activity log, as the design: a filter, then one line per event with its hash. */
@Composable
fun AgentLogScreen(m: AppModel) {
    val a = m.agents
    var filter by remember { mutableStateOf("all") }
    Page {
        TopBar("Activity log", { m.open(null) }, sub = "Hash-chained, no message content")
        Row(Modifier.padding(start = 16.dp, end = 16.dp, top = 10.dp, bottom = 10.dp), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            for ((k, label) in listOf("all" to "All", "approved" to "Allowed", "denied" to "Denied", "grant" to "Grants")) {
                val on = filter == k
                Text(
                    label, fontSize = 12.5.sp, fontWeight = FontWeight.Medium, color = if (on) C.OnAccent else C.AvatarText,
                    modifier = Modifier.clip(RoundedCornerShape(999.dp)).background(if (on) C.Accent else C.Subtle).clickable { filter = k }.padding(horizontal = 11.dp, vertical = 5.dp),
                )
            }
        }
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState())) {
            when {
                a == null -> Muted("Loading…", 14.sp, Modifier.padding(20.dp))
                a.logOk == false -> Text(
                    "The activity log failed its check: an entry was edited, removed or reordered on this phone. Decisions still work; the log is not shown.",
                    color = C.RedText, fontSize = 13.5.sp, modifier = Modifier.padding(20.dp).clip(RoundedCornerShape(12.dp)).background(C.RedBg).padding(12.dp),
                )
                a.logOk == null -> Muted("The activity log is kept on your main device.", 14.sp, Modifier.padding(20.dp))
                else -> {
                    val shown = a.log.filter { filter == "all" || it.event == filter || (filter == "grant" && it.event == "revoke") }
                    if (shown.isEmpty()) Muted("Nothing here yet.", 14.sp, Modifier.padding(20.dp))
                    for (e in shown) {
                        val (verb, res, bad) = LOG_WORDS[e.event] ?: Triple(e.event, e.event, false)
                        val detail = e.detail.replace(Regex(" (\\d+)$")) { " " + amt(it.groupValues[1].toLong()) }
                        Box(Modifier.fillMaxWidth().height(1.dp).background(C.Border))
                        Row(Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 10.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                            Text(listTime(e.ts), fontFamily = Mono, fontSize = 11.5.sp, color = C.Muted, modifier = Modifier.width(52.dp).padding(top = 2.dp))
                            Column(Modifier.weight(1f)) {
                                Text("${e.agent} $verb", fontSize = 13.sp)
                                Text("$detail · ${e.hash.take(4)}…${e.hash.takeLast(4)}", fontFamily = Mono, fontSize = 11.5.sp, color = C.Muted)
                            }
                            Text(res, fontSize = 12.sp, color = if (bad) C.Danger else C.AvatarText)
                        }
                    }
                }
            }
        }
    }
}
