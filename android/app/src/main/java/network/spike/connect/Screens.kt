package network.spike.connect

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CheckboxDefaults
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.text.DateFormat
import java.util.Calendar
import java.util.Date

// ---- small helpers ------------------------------------------------------------------------------

private fun sameDay(a: Long, b: Long): Boolean {
    val x = Calendar.getInstance().apply { timeInMillis = a }
    val y = Calendar.getInstance().apply { timeInMillis = b }
    return x.get(Calendar.YEAR) == y.get(Calendar.YEAR) && x.get(Calendar.DAY_OF_YEAR) == y.get(Calendar.DAY_OF_YEAR)
}

/** The chat list's time: the clock today, "Yesterday", a weekday this week, else the date. */
private fun listTime(ms: Long): String {
    if (ms == 0L) return ""
    val now = System.currentTimeMillis()
    return when {
        sameDay(ms, now) -> DateFormat.getTimeInstance(DateFormat.SHORT).format(Date(ms))
        sameDay(ms, now - 86_400_000L) -> "Yesterday"
        now - ms < 6 * 86_400_000L -> java.text.SimpleDateFormat("EEE", java.util.Locale.getDefault()).format(Date(ms))
        else -> DateFormat.getDateInstance(DateFormat.SHORT).format(Date(ms))
    }
}

private fun dayLabel(ms: Long): String {
    val now = System.currentTimeMillis()
    return when {
        sameDay(ms, now) -> "Today"
        sameDay(ms, now - 86_400_000L) -> "Yesterday"
        else -> DateFormat.getDateInstance(DateFormat.MEDIUM).format(Date(ms))
    }
}

private fun clock(ms: Long): String = DateFormat.getTimeInstance(DateFormat.SHORT).format(Date(ms))

private fun copy(ctx: Context, text: String) {
    val cm = ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    cm.setPrimaryClip(ClipData.newPlainText("Spike", text))
}

@Composable
internal fun Page(bg: Color = C.Bg, content: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit) {
    Column(Modifier.fillMaxSize().background(bg).imePadding()) { content() }
}

@Composable
private fun Scrolling(modifier: Modifier = Modifier, gap: Int = 14, content: @Composable () -> Unit) {
    Column(
        modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 20.dp, vertical = 16.dp),
        verticalArrangement = Arrangement.spacedBy(gap.dp),
    ) { content() }
}

// ---- 1 · create identity / unlock -----------------------------------------------------------------

@Composable
fun GateScreen(m: AppModel) {
    var pass by remember { mutableStateOf("") }
    var again by remember { mutableStateOf("") }
    var restoring by remember { mutableStateOf(false) }
    val creating = m.status == "new"
    val act = LocalContext.current as androidx.fragment.app.FragmentActivity
    val bioReady = remember { BioLock.available(act) }
    var useBio by remember { mutableStateOf(bioReady) }
    val bioUnlock = !creating && BioLock.enabled(act)

    fun unlockWithFinger() {
        BioLock.unlock(act) { p, err ->
            if (p != null) m.run { Bridge.call("unlock", args("passphrase" to p)); m.loadStatus() }
            else if (err != null && !err.contains("passphrase", true) && !err.contains("cancel", true)) m.notice = err
        }
    }
    // Offer the fingerprint straight away when it is set up.
    LaunchedEffect(bioUnlock) { if (bioUnlock) unlockWithFinger() }

    /** Create the identity; then, when asked, turn on fingerprint unlock and/or start linking. */
    fun create(thenLink: Boolean) {
        if (pass != again) {
            m.notice = "The two passphrases differ."
            return
        }
        m.run {
            Bridge.call("create_identity", args("passphrase" to pass))
            m.loadStatus()
            if (useBio && bioReady) BioLock.enable(act, pass) { err -> if (err != null) m.notice = "Fingerprint unlock was not turned on: $err" }
            if (thenLink) m.open(Screen.LinkNew)
        }
    }

    if (restoring) {
        RestoreScreen(m) { restoring = false }
        return
    }
    Page {
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).statusBarsPadding().navigationBarsPadding()
                .padding(start = 24.dp, end = 24.dp, top = 36.dp, bottom = 28.dp),
            verticalArrangement = Arrangement.spacedBy(20.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Box(Modifier.size(36.dp).clip(RoundedCornerShape(10.dp)).background(C.AvatarBg), contentAlignment = Alignment.Center) {
                    Ico(Ic.Share, C.AvatarText, 20.dp)
                }
                Text("Spike Connect", fontFamily = Grotesk, fontWeight = FontWeight.Bold, fontSize = 18.sp)
            }
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                H1(if (creating) "Your identity lives on this phone" else "Welcome back", 32.sp)
                Text(
                    if (creating) "No phone number, email or account. You are a key that only your devices hold."
                    else "Your keys and history on this phone are sealed with your passphrase.",
                    color = C.Muted, fontSize = 15.sp,
                )
            }
            if (creating) {
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    Bullet(Ic.Lock, "End-to-end encrypted.", "Relays carry your messages but cannot read them.")
                    Bullet(Ic.ShieldCheck, "Verified by people, not servers.", "Compare a code with a contact once.")
                    Bullet(Ic.Devices, "History stays on your devices.", "No server keeps a copy.")
                }
            }
            Column(verticalArrangement = Arrangement.spacedBy(14.dp)) {
                Field(
                    "Passphrase for this phone", pass, { pass = it }, password = true,
                    helper = if (creating) "10 characters or more. It encrypts your keys and history here. If you lose it and have no other device, this identity cannot be recovered." else null,
                )
                if (creating) Field("Passphrase again", again, { again = it }, password = true)
                if (creating && bioReady) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text("Unlock with fingerprint or face", fontSize = 15.sp, fontWeight = FontWeight.Medium)
                            Muted("The passphrase still works. This only saves typing it.", 12.5.sp)
                        }
                        Switch(useBio, { useBio = it }, colors = SwitchDefaults.colors(checkedTrackColor = C.Accent))
                    }
                }
            }
            Spacer(Modifier.weight(1f, fill = false).height(8.dp))
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                PrimaryButton(if (creating) "Create my identity" else "Unlock", enabled = !m.busy && pass.isNotEmpty()) {
                    if (creating) create(false)
                    else m.run {
                        Bridge.call("unlock", args("passphrase" to pass))
                        m.loadStatus()
                    }
                }
                if (bioUnlock) OutlineButton("Unlock with fingerprint or face", Modifier.fillMaxWidth()) { unlockWithFinger() }
                if (creating) {
                    OutlineButton("I already use Spike on another device", Modifier.fillMaxWidth()) {
                        if (pass.isEmpty()) m.notice = "Choose a passphrase for this phone first; then it shows a code to link it."
                        else create(true)
                    }
                    OutlineButton("Restore from a backup file", Modifier.fillMaxWidth()) { restoring = true }
                }
                Muted("Unreviewed prototype software: do not use it for real secrets yet.", 12.5.sp)
            }
        }
    }
}

// ---- 2 · chats -------------------------------------------------------------------------------------

private sealed interface Item {
    val ts: Long
    data class P(val c: Contact) : Item { override val ts get() = c.lastTs }
    data class G(val g: Group) : Item { override val ts get() = g.lastTs }
}

@Composable
fun TabBar(m: AppModel) {
    Row(
        Modifier.fillMaxWidth().background(C.Surface).navigationBarsPadding().padding(start = 8.dp, end = 8.dp, top = 6.dp, bottom = 6.dp),
    ) {
        val tabs = listOf(Tab.Chats to ("Chats" to Ic.Chat), Tab.Agents to ("Agents" to Ic.Sparkle), Tab.Devices to ("Devices" to Ic.Devices), Tab.Settings to ("Settings" to Ic.Sliders))
        for ((tab, p) in tabs) {
            val on = m.tab == tab && m.screen == null
            Column(
                Modifier.weight(1f).heightIn(min = 52.dp).clickable { m.select(tab) },
                horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.Center,
            ) {
                Ico(p.second, if (on) C.Accent else C.Muted)
                Spacer(Modifier.height(3.dp))
                Text(p.first, fontSize = 11.5.sp, fontWeight = if (on) FontWeight.SemiBold else FontWeight.Normal, color = if (on) C.Accent else C.Muted)
            }
        }
    }
    Box(Modifier.fillMaxWidth().height(0.dp))
}

@Composable
fun ChatsTab(m: AppModel) {
    val o = m.overview ?: return
    var query by remember { mutableStateOf("") }
    val items = (o.contacts.map { Item.P(it) } + o.groups.map { Item.G(it) })
        .filter { query.isBlank() || when (it) {
            is Item.P -> it.c.name.contains(query, true)
            is Item.G -> it.g.name.contains(query, true)
        } }
        .sortedByDescending { it.ts }
    val relayHost = o.relay.removePrefix("wss://").removePrefix("ws://")
    Column(Modifier.fillMaxSize()) {
        Column(
            Modifier.statusBarsPadding().padding(start = 20.dp, end = 20.dp, top = 20.dp, bottom = 10.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Box(Modifier.weight(1f)) { H1("Chats") }
                Box(
                    Modifier.size(44.dp).clip(CircleShape).background(C.Surface).border(1.dp, C.Border, CircleShape)
                        .clickable { m.open(Screen.AddContact) }.semantics { contentDescription = "Add a contact" },
                    contentAlignment = Alignment.Center,
                ) { Ico(Ic.AddPerson) }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Chip(
                    "In sync on ${o.me.devices} ${if (o.me.devices == 1) "device" else "devices"}",
                    dot = C.Green,
                ) { m.select(Tab.Devices) }
                Chip(
                    when (m.relayOk) { true -> "Relay connected"; false -> "Relay not reachable"; null -> "Relay: $relayHost" },
                    icon = Ic.Lock,
                ) { m.select(Tab.Settings) }
            }
            Row(
                Modifier.fillMaxWidth().heightIn(min = 44.dp).clip(RoundedCornerShape(12.dp)).background(C.Surface)
                    .border(1.dp, C.Border, RoundedCornerShape(12.dp)).padding(horizontal = 12.dp),
                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Ico(Ic.Search, C.Muted, 18.dp)
                BasicTextField(
                    query, { query = it }, singleLine = true, cursorBrush = SolidColor(C.Accent),
                    textStyle = TextStyle(fontFamily = Plex, fontSize = 15.sp, color = C.Text),
                    modifier = Modifier.weight(1f),
                    decorationBox = { inner ->
                        if (query.isEmpty()) Text("Search chats by name", color = C.Muted, fontSize = 15.sp)
                        inner()
                    },
                )
            }
        }
        LazyColumn(Modifier.weight(1f).padding(horizontal = 8.dp)) {
            if (o.requests > 0) {
                item {
                    Row(
                        Modifier.fillMaxWidth().padding(start = 4.dp, end = 4.dp, bottom = 4.dp).heightIn(min = 48.dp)
                            .clip(RoundedCornerShape(12.dp)).background(C.Surface)
                            .dashedBorder().clickable { m.open(Screen.Requests) }.padding(horizontal = 12.dp, vertical = 8.dp),
                        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Ico(Ic.Request, C.AvatarText)
                        Text(
                            androidx.compose.ui.text.buildAnnotatedString {
                                pushStyle(androidx.compose.ui.text.SpanStyle(fontWeight = FontWeight.SemiBold))
                                append("${o.requests} message ${if (o.requests == 1) "request" else "requests"}")
                                pop()
                                pushStyle(androidx.compose.ui.text.SpanStyle(color = C.Muted))
                                append(" from people not in your contacts")
                            },
                            fontSize = 14.sp, modifier = Modifier.weight(1f),
                        )
                        Ico(Ic.Chevron, C.AvatarText, 18.dp)
                    }
                }
            }
            if (items.isEmpty()) {
                item {
                    Muted(
                        if (query.isBlank()) "No conversations yet. Add a contact with the button at the top, then say hello."
                        else "Nothing matches “$query”.",
                        14.sp, Modifier.padding(24.dp),
                    )
                }
            }
            items(items) { r ->
                when (r) {
                    is Item.P -> ChatRow(
                        r.c.name, square = false,
                        tag = if (r.c.verified) null else "Not verified", tagKind = TagKind.Red, verified = r.c.verified,
                        preview = r.c.last ?: "No messages yet", time = listTime(r.c.lastTs), unread = r.c.unread,
                    ) { m.open(Screen.Chat(r.c.name)) }
                    is Item.G -> ChatRow(
                        r.g.name, square = true,
                        tag = when {
                            r.g.status == "invited" -> "Invite"
                            r.g.status == "left" -> "left"
                            r.g.large -> "Large group"
                            else -> "Group"
                        },
                        tagKind = if (r.g.status == "invited") TagKind.Amber else TagKind.Neutral, verified = false, plainTag = true,
                        preview = if (r.g.status == "invited") "Invite from ${r.g.invitedBy ?: "someone"}"
                        else r.g.last ?: "${r.g.members} people · no messages yet",
                        previewAmber = r.g.status == "invited", time = listTime(r.g.lastTs), unread = r.g.unread,
                    ) { m.open(if (r.g.status == "invited") Screen.Invite(r.g.id) else Screen.GroupChat(r.g.id)) }
                }
            }
        }
    }
}

private fun Modifier.dashedBorder(): Modifier = this.drawWithContent {
    drawContent()
    val stroke = androidx.compose.ui.graphics.drawscope.Stroke(
        width = 1.dp.toPx(),
        pathEffect = androidx.compose.ui.graphics.PathEffect.dashPathEffect(floatArrayOf(6.dp.toPx(), 4.dp.toPx())),
    )
    drawRoundRect(C.BorderStrong, cornerRadius = androidx.compose.ui.geometry.CornerRadius(12.dp.toPx()), style = stroke)
}

@Composable
private fun ChatRow(
    title: String, square: Boolean, tag: String?, tagKind: TagKind, verified: Boolean, preview: String,
    time: String, unread: Int, plainTag: Boolean = false, previewAmber: Boolean = false, onClick: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 64.dp).clip(RoundedCornerShape(14.dp))
            .background(if (unread > 0) C.Surface else Color.Transparent).clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Avatar(title, 46.dp, square)
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(title, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
                if (verified) Ico(Ic.ShieldCheck, C.Green, 16.dp)
                if (tag != null) {
                    if (plainTag && tagKind == TagKind.Neutral) Text(tag, fontSize = 12.5.sp, color = C.Muted) else Tag(tag, tagKind)
                }
            }
            Text(
                preview, fontSize = 14.sp, maxLines = 1, overflow = TextOverflow.Ellipsis,
                color = if (previewAmber) C.AmberText else if (unread > 0) C.Text else C.Muted,
                fontWeight = if (previewAmber || unread > 0) FontWeight.Medium else FontWeight.Normal,
            )
        }
        Column(horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.padding(top = 4.dp)) {
            Text(time, fontSize = 12.sp, color = if (unread > 0) C.Text else C.Muted, fontWeight = if (unread > 0) FontWeight.SemiBold else FontWeight.Normal)
            if (unread > 0) {
                Box(
                    Modifier.heightIn(min = 20.dp).widthIn(min = 20.dp).clip(RoundedCornerShape(10.dp)).background(C.AvatarText).padding(horizontal = 6.dp),
                    contentAlignment = Alignment.Center,
                ) { Text("$unread", color = Color.White, fontSize = 12.sp, fontWeight = FontWeight.SemiBold) }
            }
        }
    }
}

// ---- 3 · conversation (a person or a group) --------------------------------------------------------

@Composable
fun ConversationScreen(
    m: AppModel, title: String, sub: String, verified: Boolean, square: Boolean, caption: String,
    onSend: suspend (String) -> Unit, onInfo: () -> Unit,
    onCall: ((Boolean) -> Unit)? = null,
    attach: (suspend (String, java.io.File) -> Unit)? = null,
) {
    var text by remember { mutableStateOf("") }
    val ctx = LocalContext.current
    val pick = rememberLauncherForActivityResult(ActivityResultContracts.GetContent()) { uri ->
        if (uri != null && attach != null) {
            m.run {
                val picked = withContext(Dispatchers.IO) { copyPicked(ctx, uri) } ?: throw BridgeError("could not read that file")
                try {
                    attach(picked.first, picked.second)
                } finally {
                    picked.second.delete()
                }
                m.refresh()
            }
        }
    }
    val listState = rememberLazyListState()
    LaunchedEffect(m.messages.size) {
        if (m.messages.isNotEmpty()) listState.scrollToItem(m.messages.size)
    }
    Page(C.BgChat) {
        Row(
            Modifier.fillMaxWidth().background(C.Surface).statusBarsPadding().padding(start = 10.dp, end = 10.dp, top = 12.dp, bottom = 10.dp),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Box(Modifier.size(44.dp).clip(CircleShape).clickable { m.open(null) }.semantics { contentDescription = "Back to chats" }, contentAlignment = Alignment.Center) { Ico(Ic.Back) }
            Avatar(title, 40.dp, square)
            Column(Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(title, fontWeight = FontWeight.SemiBold, fontSize = 16.sp, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
                    if (verified) Ico(Ic.ShieldCheck, C.Green, 16.dp)
                }
                Muted(sub, 12.5.sp)
            }
            if (onCall != null) {
                val gate = rememberMicGate { onCall(false) }
                val vgate = rememberMicGate(camera = true) { onCall(true) }
                Box(Modifier.size(44.dp).clip(CircleShape).clickable { gate() }.semantics { contentDescription = "Voice call" }, contentAlignment = Alignment.Center) { Ico(IcHandset) }
                Box(Modifier.size(44.dp).clip(CircleShape).clickable { vgate() }.semantics { contentDescription = "Video call" }, contentAlignment = Alignment.Center) { Ico(IcVideo) }
            }
            Box(Modifier.size(44.dp).clip(CircleShape).clickable(onClick = onInfo).semantics { contentDescription = "Details" }, contentAlignment = Alignment.Center) { Ico(Ic.Info) }
        }
        Box(Modifier.fillMaxWidth().height(1.dp).background(C.Border))
        LazyColumn(
            Modifier.weight(1f).fillMaxWidth().padding(horizontal = 14.dp), state = listState,
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            item { Spacer(Modifier.height(2.dp)) }
            if (m.messages.isEmpty()) {
                item { Muted("No messages yet. Say hello.", 14.sp, Modifier.padding(vertical = 24.dp).fillMaxWidth()) }
            }
            m.messages.forEachIndexed { i, msg ->
                if (i == 0 || !sameDay(m.messages[i - 1].ts, msg.ts)) {
                    item(key = "day-${msg.id}") {
                        Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                            Text(dayLabel(msg.ts), fontSize = 12.sp, color = C.Muted, modifier = Modifier.clip(RoundedCornerShape(999.dp)).background(C.Surface).padding(horizontal = 10.dp, vertical = 3.dp))
                        }
                    }
                }
                item(key = msg.id) { Bubble(msg) { m.run { saveAndOpen(ctx, msg.id) } } }
            }
            item { Spacer(Modifier.height(2.dp)) }
        }
        Column(
            Modifier.fillMaxWidth().background(C.Surface).navigationBarsPadding().padding(start = 10.dp, end = 10.dp, top = 8.dp, bottom = 10.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Box(Modifier.fillMaxWidth().height(0.dp))
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (attach != null) {
                    Box(
                        Modifier.size(44.dp).clip(CircleShape).clickable { pick.launch("*/*") }.semantics { contentDescription = "Attach a file" },
                        contentAlignment = Alignment.Center,
                    ) { Ico(Ic.Clip, C.Muted) }
                }
                Box(
                    Modifier.weight(1f).heightIn(min = 44.dp).clip(RoundedCornerShape(22.dp)).background(C.Bg)
                        .border(1.dp, C.BorderStrong, RoundedCornerShape(22.dp)).padding(horizontal = 14.dp, vertical = 10.dp),
                    contentAlignment = Alignment.CenterStart,
                ) {
                    BasicTextField(
                        text, { text = it }, cursorBrush = SolidColor(C.Accent), maxLines = 4,
                        textStyle = TextStyle(fontFamily = Plex, fontSize = 15.sp, color = C.Text),
                        modifier = Modifier.fillMaxWidth(),
                        decorationBox = { inner ->
                            if (text.isEmpty()) Text("Message $title", color = C.Muted, fontSize = 15.sp, maxLines = 1)
                            inner()
                        },
                    )
                }
                Box(
                    Modifier.size(44.dp).clip(CircleShape).background(if (text.isBlank()) C.Accent.copy(alpha = 0.4f) else C.Accent)
                        .clickable(enabled = text.isNotBlank()) {
                            val t = text
                            text = ""
                            m.run { onSend(t); m.refresh() }
                        }.semantics { contentDescription = "Send" },
                    contentAlignment = Alignment.Center,
                ) { Ico(Ic.Send, Color.White) }
            }
            Muted(caption, 11.5.sp, Modifier.padding(start = 52.dp))
        }
    }
}

@Composable
private fun Bubble(msg: Message, onFile: () -> Unit) {
    Column(
        Modifier.fillMaxWidth(), horizontalAlignment = if (msg.out) Alignment.End else Alignment.Start,
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        if (!msg.out && msg.from != null) {
            Text(msg.from + if (msg.fromVerified) "" else " · not verified", fontSize = 12.sp, color = C.Muted, modifier = Modifier.padding(start = 6.dp))
        }
        val shape = if (msg.out) RoundedCornerShape(18.dp, 18.dp, 6.dp, 18.dp) else RoundedCornerShape(18.dp, 18.dp, 18.dp, 6.dp)
        Box(
            Modifier.widthIn(max = 300.dp).clip(shape)
                .background(if (msg.out) C.Accent else C.Surface)
                .then(if (msg.out) Modifier else Modifier.border(1.dp, C.Border, shape))
                .padding(horizontal = 13.dp, vertical = 9.dp),
        ) {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                if (msg.fileName != null && !msg.deleted) {
                    Row(
                        Modifier.clickable(onClick = onFile).semantics { contentDescription = "File ${msg.fileName}" },
                        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        Ico(Ic.File, if (msg.out) Color.White else C.AvatarText, 26.dp)
                        Column {
                            Text(msg.fileName, fontSize = 15.sp, fontWeight = FontWeight.Medium, color = if (msg.out) Color.White else C.Text, maxLines = 2, overflow = TextOverflow.Ellipsis)
                            Text(sizeText(msg.fileSize) + " · tap to open", fontSize = 12.sp, color = if (msg.out) Color.White.copy(alpha = 0.8f) else C.Muted)
                        }
                    }
                }
                if (msg.deleted || msg.body.isNotEmpty() || msg.fileName == null) {
                    Text(
                        if (msg.deleted) "Message deleted" else msg.body, fontSize = 15.sp,
                        color = if (msg.out) Color.White else C.Text,
                        fontStyle = if (msg.deleted) androidx.compose.ui.text.font.FontStyle.Italic else androidx.compose.ui.text.font.FontStyle.Normal,
                    )
                }
            }
        }
        Muted(clock(msg.ts) + if (msg.edited) " · edited" else "", 11.5.sp)
    }
}


// ---- 4 · verify a contact -----------------------------------------------------------------------------

@Composable
fun VerifyScreen(m: AppModel, name: String) {
    val o = m.overview ?: return
    val c = o.contacts.firstOrNull { it.name == name } ?: run { m.open(null); return }
    var typed by remember { mutableStateOf("") }
    Page {
        TopBar("Verify $name", { m.open(Screen.Chat(name)) })
        Scrolling {
            Muted("Compare these codes with $name in person or on a call you trust. Read the first aloud; if they match, nobody is in the middle.", 14.sp)
            CardBox {
                Muted("$name's fingerprint", 12.5.sp)
                Text(c.fingerprint.replace("-", " "), fontFamily = Mono, fontSize = 17.sp, lineHeight = 26.sp)
            }
            CardBox {
                Muted("Your fingerprint (they check this one)", 12.5.sp)
                Text(o.me.fingerprint.replace("-", " "), fontFamily = Mono, fontSize = 17.sp, lineHeight = 26.sp)
            }
            CardBox {
                Text("$name's devices", fontWeight = FontWeight.SemiBold)
                Muted("${c.devices} ${if (c.devices == 1) "device" else "devices"} on $name's signed device list. Verifying $name's identity key covers every device on it.", 13.sp)
            }
            if (c.verified) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Ico(Ic.ShieldCheck, C.Green); Text("You verified $name.", fontWeight = FontWeight.Medium)
                }
            } else {
                Field("Type the fingerprint $name reads to you", typed, { typed = it }, placeholder = "xxxxx-xxxxx-…")
                PrimaryButton("Mark $name as verified", enabled = typed.isNotBlank() && !m.busy) {
                    m.run { Bridge.call("mark_verified", args("name" to name, "fingerprint" to typed.trim())); m.notice = "Verified."; m.refresh() }
                }
            }
        }
    }
}

// ---- 10 · add a contact -------------------------------------------------------------------------------

@Composable
internal fun QrCode(matrix: List<String>, modifier: Modifier = Modifier) {
    val n = matrix.size
    Canvas(modifier.background(Color.White)) {
        val quiet = 2
        val cell = size.width / (n + quiet * 2)
        for (y in 0 until n) for (x in 0 until n) {
            if (matrix[y][x] == '1') {
                drawRect(C.Text, Offset((x + quiet) * cell, (y + quiet) * cell), Size(cell + 0.5f, cell + 0.5f))
            }
        }
    }
}

@Composable
fun AddContactScreen(m: AppModel) {
    val o = m.overview ?: return
    val ctx = LocalContext.current
    var tab by remember { mutableStateOf(0) }
    var name by remember { mutableStateOf("") }
    var card by remember { mutableStateOf("") }
    var qr by remember { mutableStateOf<List<String>?>(null) }
    var scanned by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(o.me.card) {
        try {
            val r = Bridge.obj("qr_matrix", args("text" to o.me.card))
            qr = (0 until r.getJSONArray("rows").length()).map { r.getJSONArray("rows").getString(it) }
        } catch (_: Exception) {}
    }
    Page {
        TopBar("Add a contact", { m.open(null) })
        Scrolling {
            Muted("There is no phone-number or email lookup. You swap cards directly, so no server learns who knows whom.", 14.sp)
            Row(
                Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).background(C.AvatarBg).padding(3.dp),
                horizontalArrangement = Arrangement.spacedBy(3.dp),
            ) {
                for ((i, label) in listOf("My code", "Scan", "Paste a card", "Share").withIndex()) {
                    Box(
                        Modifier.weight(1f).heightIn(min = 40.dp).clip(RoundedCornerShape(10.dp))
                            .background(if (tab == i) C.Surface else Color.Transparent).clickable { tab = i },
                        contentAlignment = Alignment.Center,
                    ) { Text(label, fontWeight = if (tab == i) FontWeight.SemiBold else FontWeight.Normal, fontSize = 14.sp) }
                }
            }
            when (tab) {
                0 -> {
                    CardBox {
                        Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                            val q = qr
                            if (q != null) QrCode(q, Modifier.size(220.dp)) else Muted("Making your code…")
                        }
                        Text("Your identity", fontWeight = FontWeight.SemiBold)
                        Text(o.me.fingerprint.replace("-", " "), fontFamily = Mono, fontSize = 15.sp)
                    }
                    Muted("This code holds your public card. Your friend scans it with Add a contact, Scan. Or they can paste your card (the Share tab).", 13.sp)
                }
                1 -> {
                    Muted("Point the camera at your contact's code (their Add a contact, My code). Then compare fingerprints with them once.", 14.sp)
                    if (scanned == null) {
                        QrScanner(Modifier.fillMaxWidth().height(300.dp)) { text ->
                            if (text.trim().startsWith("spike2:")) scanned = text.trim()
                            else m.notice = "That code is not a Spike card."
                        }
                    } else {
                        CardBox {
                            Text("Got a card", fontWeight = FontWeight.SemiBold)
                            Text(scanned!!.take(48) + "…", fontFamily = Mono, fontSize = 12.sp)
                        }
                        Field("Name", name, { name = it }, helper = "Only you see this name.")
                        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                            OutlineButton("Scan again") { scanned = null }
                            Box(Modifier.weight(1f)) {
                                PrimaryButton("Add contact", enabled = name.isNotBlank() && !m.busy) {
                                    val c = scanned!!
                                    m.run {
                                        Bridge.call("add_contact", args("name" to name.trim(), "card" to c))
                                        m.open(null)
                                    }
                                }
                            }
                        }
                    }
                }
                2 -> {
                    Muted("Paste the card your contact shared (it starts with spike2:). Then compare fingerprints with them once.", 14.sp)
                    Field("Name", name, { name = it }, helper = "Only you see this name.")
                    Field("Card", card, { card = it }, singleLine = false, minHeight = 96.dp, maxLines = 4)
                    Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        OutlineButton("Paste") {
                            val cm = ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                            card = cm.primaryClip?.getItemAt(0)?.text?.toString()?.trim() ?: ""
                        }
                        Box(Modifier.weight(1f)) {
                            PrimaryButton("Add contact", enabled = name.isNotBlank() && card.isNotBlank() && !m.busy) {
                                m.run {
                                    Bridge.call("add_contact", args("name" to name.trim(), "card" to card.trim()))
                                    m.open(null)
                                }
                            }
                        }
                    }
                }
                else -> {
                    Text("Your card", fontWeight = FontWeight.SemiBold)
                    CardBox { Text(o.me.card.take(48) + "…", fontFamily = Mono, fontSize = 12.sp) }
                    Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        OutlineButton("Copy") { copy(ctx, o.me.card); m.notice = "Card copied." }
                        OutlineButton("Share") {
                            val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, o.me.card)
                            ctx.startActivity(Intent.createChooser(send, "Share your card"))
                        }
                    }
                    Muted("Holds only your public card. Send it through any app you trust. They are not verified until you compare fingerprints.", 13.sp)
                }
            }
            if (o.requests > 0) {
                Text("See message requests (${o.requests})", color = C.Accent, fontWeight = FontWeight.SemiBold, modifier = Modifier.clickable { m.open(Screen.Requests) }.padding(vertical = 8.dp))
            }
        }
    }
}

// ---- 11 · message requests ------------------------------------------------------------------------------

@Composable
fun RequestsScreen(m: AppModel) {
    Page {
        TopBar("Message requests", { m.open(null) })
        Scrolling {
            Muted("People who are not in your contacts. They can't see whether you have read anything until you accept.", 14.sp)
            if (m.requests.isEmpty()) Muted("No requests right now.", 14.sp)
            for (r in m.requests) {
                var naming by remember(r.id) { mutableStateOf(false) }
                var name by remember(r.id) { mutableStateOf("") }
                CardBox {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                        Avatar(r.fingerprint.take(2), 40.dp)
                        Column {
                            Text("Unknown sender", fontWeight = FontWeight.SemiBold)
                            Text(r.fingerprint.replace("-", " "), fontFamily = Mono, fontSize = 12.sp, color = C.Muted)
                        }
                    }
                    Muted("Message hidden. Links and attachments stay blocked until you accept.", 13.sp)
                    if (r.held.isNotEmpty()) Muted("${r.held.size} ${if (r.held.size == 1) "message" else "messages"} waiting", 13.sp)
                    if (naming) {
                        Field("A name for them", name, { name = it }, placeholder = "e.g. Evan")
                        PrimaryButton("Accept as $name", enabled = name.isNotBlank()) {
                            m.run { Bridge.call("accept_request", args("id" to r.id, "name" to name.trim())); m.refresh() }
                        }
                    } else {
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            OutlineButton("Block", danger = true, small = true) { m.run { Bridge.call("block_request", args("id" to r.id)); m.refresh() } }
                            OutlineButton("Decline", small = true) { m.run { Bridge.call("decline_request", args("id" to r.id)); m.refresh() } }
                            Box(
                                Modifier.heightIn(min = 36.dp).clip(RoundedCornerShape(10.dp)).background(C.Accent).clickable { naming = true }.padding(horizontal = 16.dp),
                                contentAlignment = Alignment.Center,
                            ) { Text("Accept", color = Color.White, fontWeight = FontWeight.SemiBold, fontSize = 13.sp) }
                        }
                    }
                }
            }
            Muted("Requests are end-to-end encrypted like any message. Accepting adds the person as not verified; compare fingerprints when you can. Blocking drops their messages on your devices as they arrive; the relay cannot tell who sent a message, so it still carries them until then.", 12.5.sp)
        }
    }
}

// ---- 5 · your devices ---------------------------------------------------------------------------------------

@Composable
fun DevicesTab(m: AppModel) {
    val o = m.overview ?: return
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).statusBarsPadding().padding(start = 20.dp, end = 20.dp, top = 20.dp, bottom = 16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        H1("Your devices")
        Muted("Contacts encrypt every message separately for each of these. Signed device list v${o.me.listVersion}.", 14.sp)
        for (d in o.me.deviceList) {
            CardBox {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    Ico(Ic.PhoneDevice, C.AvatarText, 26.dp)
                    Column(Modifier.weight(1f)) {
                        Text(if (d.self) "This phone" else "Device ${d.id.take(6)}", fontWeight = FontWeight.SemiBold)
                        Muted(
                            if (d.self) "Added to your identity · you are here"
                            else if (d.approver) "Main device" else "Linked to your identity",
                            13.sp,
                        )
                    }
                    if (d.self) Tag("You are here")
                    else if (o.me.approver) OutlineButton("Remove", danger = true, small = true) {
                        m.run { Bridge.call("remove_device", args("id" to d.id)); m.refresh() }
                    }
                }
            }
        }
        if (o.me.approver) {
            PrimaryButton("Link a new device", icon = Ic.Plus) { m.open(Screen.LinkOld) }
        } else {
            Muted("New devices are linked from your main device (the first one you set up).", 13.sp)
        }
        CardBox {
            Text("Your identity key", fontWeight = FontWeight.SemiBold, fontSize = 13.sp)
            Text(o.me.fingerprint.replace("-", " "), fontFamily = Mono, fontSize = 13.sp)
            Muted("Contacts compare this when they verify you. It signs your device list; it never signs messages.", 12.5.sp)
        }
    }
}

// ---- 8/9 · agents ---------------------------------------------------------------------------------------------

@Composable
fun AgentsTab(m: AppModel) {
    val a = m.agents
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).statusBarsPadding().padding(start = 20.dp, end = 20.dp, top = 20.dp, bottom = 16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        H1("Agents")
        Muted("Software that acts for you only within limits you set, and asks before anything over them. Every request is signed and shown by what it will do.", 14.sp)
        if (a == null) Muted("Loading…", 14.sp)
        else {
            if (a.requests.isEmpty() && a.grants.isEmpty()) {
                Muted("No agents yet. Agents are allowed from the computer app for now; their requests appear here for you to approve.", 14.sp)
            }
            for (r in a.requests) {
                CardBox {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(r.from, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                        when (r.status) {
                            "waiting" -> Tag("Waiting for you", TagKind.Amber)
                            "approved" -> Tag("Approved")
                            else -> Tag("Denied", TagKind.Red)
                        }
                    }
                    if (r.summary != null) {
                        Text(r.summary, fontFamily = Grotesk, fontWeight = FontWeight.SemiBold, fontSize = 18.sp)
                        for ((k, v) in r.typed) Muted("$k: $v", 13.sp)
                        Muted("Agent says (not verified): ${r.note}", 12.5.sp)
                    } else {
                        Muted("Agent says (not verified): ${r.note}", 14.sp)
                    }
                    Muted("Scope: ${r.scope}", 12.5.sp)
                    if (r.status == "waiting") {
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            OutlineButton("Deny", danger = true, small = true) { m.run { Bridge.call("decide", args("id" to r.id, "approve" to false)); m.refresh() } }
                            Box(
                                Modifier.heightIn(min = 36.dp).clip(RoundedCornerShape(10.dp)).background(C.Accent)
                                    .clickable { m.run { Bridge.call("decide", args("id" to r.id, "approve" to true)); m.refresh() } }.padding(horizontal = 16.dp),
                                contentAlignment = Alignment.Center,
                            ) { Text("Approve", color = Color.White, fontWeight = FontWeight.SemiBold, fontSize = 13.sp) }
                        }
                    }
                }
            }
            if (a.grants.isNotEmpty()) {
                Text("Allowed agents", fontWeight = FontWeight.SemiBold)
                for (g in a.grants) {
                    CardBox {
                        Text(g.agent, fontWeight = FontWeight.SemiBold)
                        Muted("Scopes: ${g.scopes.joinToString(", ")}${if (g.typedOnly) " · typed requests only" else ""}", 13.sp)
                    }
                }
            }
        }
    }
}

// ---- 7 · network and privacy (settings) ---------------------------------------------------------------------------

@Composable
fun SettingsTab(m: AppModel) {
    val o = m.overview ?: return
    val ctx = LocalContext.current
    var relay by remember(o.relay) { mutableStateOf(o.relay) }
    var insecure by remember { mutableStateOf(o.relay.startsWith("ws://")) }
    var editing by remember { mutableStateOf(false) }
    val act = ctx as androidx.fragment.app.FragmentActivity
    val bioAvailable = remember { BioLock.available(act) }
    var bio by remember { mutableStateOf(BioLock.enabled(act)) }
    var askPass by remember { mutableStateOf(false) }
    var passTyped by remember { mutableStateOf("") }
    var alerts by remember { mutableStateOf(Prefs.notifyInBackground(ctx)) }
    val askNotify = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { ok ->
        if (ok) { Prefs.setNotifyInBackground(ctx, true); ListenerService.start(ctx); alerts = true }
        else m.notice = "Allow notifications for Spike Connect in the phone's settings to get alerts."
    }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).statusBarsPadding().padding(start = 20.dp, end = 20.dp, top = 20.dp, bottom = 16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        H1("Network and privacy", 28.sp)
        Text("Home relay", fontWeight = FontWeight.SemiBold)
        CardBox {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Ico(Ic.Server, C.AvatarText, 26.dp)
                Column(Modifier.weight(1f)) {
                    Text(o.relay.removePrefix("wss://").removePrefix("ws://"), fontWeight = FontWeight.SemiBold)
                    Text(o.relay, fontFamily = Mono, fontSize = 12.sp, color = C.Muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
                when (m.relayOk) {
                    true -> Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        Box(Modifier.size(8.dp).clip(CircleShape).background(C.Green)); Text("Connected", fontSize = 13.sp)
                    }
                    false -> Tag("Not reachable", TagKind.Red)
                    null -> {}
                }
            }
            Muted("Contacts deliver here while your devices are offline. ${if (o.relay.startsWith("wss://")) "Certificate verified." else "This address is not encrypted in transit; messages are still end-to-end encrypted."}", 13.sp)
            if (!editing) OutlineButton("Change relay", small = true) { editing = true }
            else {
                Field("Relay address", relay, { relay = it }, placeholder = "wss://relay.example")
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("Allow plain ws:// (private networks only)", fontSize = 14.sp, modifier = Modifier.weight(1f))
                    Switch(insecure, { insecure = it }, colors = SwitchDefaults.colors(checkedTrackColor = C.Accent))
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlineButton("Test", small = true) {
                        m.run {
                            Bridge.call("set_allow_insecure", args("allow" to insecure))
                            Bridge.call("set_relay", args("url" to relay.trim()))
                            m.notice = Bridge.call("test_relay") as String
                            m.relayOk = true
                            m.refresh()
                        }
                    }
                    Box(
                        Modifier.heightIn(min = 36.dp).clip(RoundedCornerShape(10.dp)).background(C.Accent).clickable {
                            m.run {
                                Bridge.call("set_allow_insecure", args("allow" to insecure))
                                Bridge.call("set_relay", args("url" to relay.trim()))
                                editing = false
                                m.notice = "Saved."
                                m.refresh()
                            }
                        }.padding(horizontal = 16.dp),
                        contentAlignment = Alignment.Center,
                    ) { Text("Save", color = Color.White, fontWeight = FontWeight.SemiBold, fontSize = 13.sp) }
                }
            }
        }
        BackupCard(m)
        DirectCard(m)
        Text("What any relay can see", fontWeight = FontWeight.SemiBold)
        CardBox {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text("Can see", fontWeight = FontWeight.SemiBold, fontSize = 13.sp, color = C.AmberText)
                    for (t in listOf("Device IDs", "Message sizes", "When devices connect")) Text(t, fontSize = 14.sp)
                }
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text("Never sees", fontWeight = FontWeight.SemiBold, fontSize = 13.sp, color = C.Green)
                    for (t in listOf("Message text or files", "Your contacts", "Your history")) Text(t, fontSize = 14.sp)
                }
            }
        }
        Text("This phone", fontWeight = FontWeight.SemiBold)
        CardBox {
            Text("Your card", fontWeight = FontWeight.Medium)
            Muted("Fingerprint ${o.me.fingerprint}", 12.5.sp)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlineButton("Copy my card", small = true) { copy(ctx, o.me.card); m.notice = "Card copied." }
                OutlineButton("Lock now", small = true) { m.run { Bridge.call("lock"); m.loadStatus() } }
            }
            Muted("The passphrase is asked again after the app restarts, and after ${AUTO_LOCK_MINUTES} minutes away from the app unless alerts are on.", 12.5.sp)
            var shots by remember { mutableStateOf(Prefs.allowScreenshots(ctx)) }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("Allow screenshots", fontWeight = FontWeight.Medium)
                    Muted("Off hides messages from screenshots, screen recording and the recent-apps picture.", 12.5.sp)
                }
                Switch(shots, { shots = it; Prefs.setAllowScreenshots(ctx, it); (ctx as? MainActivity)?.refreshScreenPrivacy() }, colors = SwitchDefaults.colors(checkedTrackColor = C.Accent))
            }
        }
        Text("Unlocking and alerts", fontWeight = FontWeight.SemiBold)
        CardBox {
            if (bioAvailable) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text("Unlock with fingerprint or face", fontWeight = FontWeight.Medium)
                        Muted("Saves typing the passphrase. It is kept sealed by this phone's hardware and dropped if the enrolled fingerprints change.", 12.5.sp)
                    }
                    Switch(bio, { want ->
                        if (!want) { BioLock.disable(act); bio = false }
                        else askPass = true
                    }, colors = SwitchDefaults.colors(checkedTrackColor = C.Accent))
                }
                if (askPass) {
                    Field("Passphrase", passTyped, { passTyped = it }, password = true, helper = "Type it once so it can be sealed. You then confirm with your fingerprint or face.")
                    PrimaryButton("Turn on", enabled = passTyped.isNotEmpty()) {
                        BioLock.enable(act, passTyped) { err ->
                            if (err == null) { bio = true; m.notice = "Fingerprint unlock is on." } else m.notice = "Not turned on: $err"
                            askPass = false; passTyped = ""
                        }
                    }
                }
            } else {
                Muted("Fingerprint or face unlock needs a fingerprint or face set up in the phone's settings.", 12.5.sp)
            }
            if (!ctx.getSystemService(android.os.PowerManager::class.java).isIgnoringBatteryOptimizations(ctx.packageName)) {
                Muted("To receive calls with the screen off, the phone must not put Spike Connect to sleep. Without this the phone drops the connection after a while and calls do not arrive.", 12.5.sp)
                OutlineButton("Allow in the background (battery)") {
                    ctx.startActivity(
                        android.content.Intent(android.provider.Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, android.net.Uri.parse("package:" + ctx.packageName))
                            .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK),
                    )
                }
            }
            if (android.os.Build.VERSION.SDK_INT >= 34 && !ctx.getSystemService(android.app.NotificationManager::class.java).canUseFullScreenIntent()) {
                Muted("Incoming calls can only appear over the lock screen if you allow full-screen notifications for Spike Connect. Without it you get a normal notification.", 12.5.sp)
                OutlineButton("Allow calls on the lock screen") {
                    ctx.startActivity(
                        android.content.Intent(android.provider.Settings.ACTION_MANAGE_APP_USE_FULL_SCREEN_INTENT, android.net.Uri.parse("package:" + ctx.packageName))
                            .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK),
                    )
                }
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("Alerts while the app is closed", fontWeight = FontWeight.Medium)
                    Muted("Keeps a small notification showing and listens for messages. Alerts say that something arrived, never what. Needs the app unlocked since the phone last started.", 12.5.sp)
                }
                Switch(alerts, { want ->
                    if (!want) {
                        Prefs.setNotifyInBackground(ctx, false); ListenerService.stop(ctx); alerts = false
                    } else if (!Notifier.allowed(ctx)) {
                        askNotify.launch(android.Manifest.permission.POST_NOTIFICATIONS)
                    } else {
                        Prefs.setNotifyInBackground(ctx, true); ListenerService.start(ctx); alerts = true
                    }
                }, colors = SwitchDefaults.colors(checkedTrackColor = C.Accent))
            }
        }
    }
}

// ---- large and small groups, in the same style ------------------------------------------------------------------

@Composable
fun InviteScreen(m: AppModel, id: String) {
    val g = m.group
    Page {
        TopBar("Group invite", { m.open(null) })
        Scrolling {
            if (g == null) {
                Muted("Loading…")
            } else {
                Text("${g.invitedBy ?: "Someone"} invited you to “${g.name}”. Nothing from the group is shown until you accept.")
                if (g.large) {
                    CardBox {
                        Text("A large group", fontWeight = FontWeight.SemiBold)
                        Muted("You will see who is in it once you join. Large groups are experimental and have not been independently reviewed yet.", 13.sp)
                    }
                }
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    OutlineButton("Decline", Modifier.weight(1f)) { m.run { Bridge.call("group_decline", args("id" to id)); m.open(null) } }
                    Box(Modifier.weight(1f)) {
                        PrimaryButton("Join the group", enabled = !m.busy) {
                            m.run { Bridge.call("group_accept", args("id" to id)); m.open(Screen.GroupChat(id)) }
                        }
                    }
                }
                Muted("If you decline, they are not told, and anyone can invite you again.", 12.5.sp)
            }
        }
    }
}

@Composable
fun MembersScreen(m: AppModel, id: String) {
    val g = m.group
    var rename by remember(g?.name) { mutableStateOf(g?.name ?: "") }
    val picked = remember { mutableStateListOf<String>() }
    var adding by remember { mutableStateOf(false) }
    Page {
        TopBar(g?.name ?: "Group", { m.open(Screen.GroupChat(id)) })
        if (g == null) return@Page
        Scrolling(gap = 12) {
            Muted("${g.members.size} of up to ${g.max} people${if (g.unverified > 0) " · ${g.unverified} not verified" else ""}", 14.sp)
            if (g.refused > 0) {
                CardBox { Muted("${g.refused} change(s) were refused because they broke the group's rules, and skipped. Latest: ${g.refusedWhy ?: "unknown"}.", 13.sp) }
            }
            if (g.meAdmin) {
                Field("Group name", rename, { rename = it })
                if (rename.isNotBlank() && rename != g.name) {
                    OutlineButton("Rename", small = true) { m.run { Bridge.call("group_rename", args("id" to id, "name" to rename)); m.refresh() } }
                }
            }
            CardBox {
                for ((i, mem) in g.members.withIndex()) {
                    if (i > 0) Box(Modifier.fillMaxWidth().height(1.dp).background(C.Border))
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        Avatar(if (mem.you) "You" else mem.name, 36.dp)
                        Column(Modifier.weight(1f)) {
                            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                                Text(if (mem.you) "You" else mem.name, fontWeight = FontWeight.SemiBold)
                                if (mem.admin) Tag("admin")
                            }
                            if (!mem.you) Muted(if (mem.verified) "Verified" else if (mem.contact) "Not verified" else "Not in your contacts", 12.5.sp)
                        }
                    }
                    if (g.meAdmin && !mem.you) {
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(start = 46.dp)) {
                            if (!mem.admin) OutlineButton("Make admin", small = true) { m.run { Bridge.call("group_make_admin", args("id" to id, "owner" to mem.owner)); m.refresh() } }
                            OutlineButton("Remove", danger = true, small = true) { m.run { Bridge.call("group_remove", args("id" to id, "owner" to mem.owner)); m.refresh() } }
                        }
                    }
                }
            }
            if (g.canAdd) {
                OutlineButton("Add people", Modifier.fillMaxWidth()) { adding = !adding; picked.clear() }
                if (adding) {
                    val candidates = (m.overview?.contacts ?: emptyList()).filter { c -> g.members.none { it.name == c.name } }
                    if (candidates.isEmpty()) Muted("Everyone in your contacts is already in this group.", 14.sp)
                    for (c in candidates) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Checkbox(c.name in picked, { on -> if (on) picked.add(c.name) else picked.remove(c.name) }, colors = CheckboxDefaults.colors(checkedColor = C.Accent))
                            Text(c.name)
                        }
                    }
                    PrimaryButton("Add", enabled = picked.isNotEmpty()) {
                        val names = picked.toList(); adding = false
                        m.run { Bridge.call("group_add", args("names" to names, "id" to id)); m.refresh() }
                    }
                }
            }
            if (g.meAdmin) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("Any member may add people", Modifier.weight(1f))
                    Switch(g.addBy == "any", { on -> m.run { Bridge.call("group_set_add_by", args("id" to id, "any" to on)); m.refresh() } }, colors = SwitchDefaults.colors(checkedTrackColor = C.Accent))
                }
            }
            if (g.status == "joined") {
                OutlineButton("Leave group", Modifier.fillMaxWidth(), danger = true) { m.run { Bridge.call("group_leave", args("id" to id)); m.open(null) } }
            }
            Muted(
                if (g.large) "A large group uses MLS: each message is encrypted once for everyone and stored once on the relay. Experimental; not independently reviewed."
                else "Each message is encrypted separately for every device of every member.",
                12.5.sp,
            )
        }
    }
}

@Composable
fun NewGroupScreen(m: AppModel) {
    var name by remember { mutableStateOf("") }
    var large by remember { mutableStateOf(true) }
    val picked = remember { mutableStateListOf<String>() }
    val contacts = m.overview?.contacts ?: emptyList()
    Page {
        TopBar("New group", { m.open(null) })
        Scrolling(gap = 12) {
            Row(
                Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).background(C.AvatarBg).padding(3.dp),
                horizontalArrangement = Arrangement.spacedBy(3.dp),
            ) {
                for ((isLarge, label) in listOf(false to "Small (up to 20)", true to "Large (up to 500)")) {
                    Box(
                        Modifier.weight(1f).heightIn(min = 40.dp).clip(RoundedCornerShape(10.dp))
                            .background(if (large == isLarge) C.Surface else Color.Transparent).clickable { large = isLarge },
                        contentAlignment = Alignment.Center,
                    ) { Text(label, fontWeight = if (large == isLarge) FontWeight.SemiBold else FontWeight.Normal, fontSize = 14.sp) }
                }
            }
            Muted(if (large) "One encrypted copy of each message for everyone. People you add get an invitation and join when they accept. Experimental: not independently reviewed yet." else "Every message is encrypted separately for each member's devices.", 13.sp)
            Field("Group name", name, { name = it }, placeholder = "e.g. Pilot team")
            Text("Members", fontWeight = FontWeight.SemiBold)
            if (contacts.isEmpty()) Muted("Add a contact first: a group is made from your contacts.", 14.sp)
            CardBox {
                for (c in contacts) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Checkbox(c.name in picked, { on -> if (on) picked.add(c.name) else picked.remove(c.name) }, colors = CheckboxDefaults.colors(checkedColor = C.Accent))
                        Text(c.name, Modifier.weight(1f))
                        if (!c.verified) Tag("Not verified", TagKind.Red)
                    }
                }
            }
            PrimaryButton("Create group", enabled = name.isNotBlank() && picked.isNotEmpty() && !m.busy) {
                val members = picked.toList()
                m.run {
                    val r = if (large) Bridge.obj("group_create_large", args("name" to name, "members" to members))
                    else Bridge.obj("group_create", args("name" to name, "members" to members))
                    m.notice = "Invited ${r.getInt("reached")} of ${r.getInt("people")} people."
                    m.open(Screen.GroupChat(r.getString("group")))
                }
            }
        }
    }
}

// ---- direct connections (ADR 0027) ----------------------------------------------------------------------------

/** Send and receive directly with a contact's device on the same network, without the relay. */
@Composable
fun DirectCard(m: AppModel) {
    var d by remember { mutableStateOf<org.json.JSONObject?>(null) }
    LaunchedEffect(Unit) { try { d = Bridge.obj("direct_status") } catch (_: Exception) {} }
    val st = d ?: return
    val enabled = st.getBoolean("enabled")
    Text("Direct connections", fontWeight = FontWeight.SemiBold)
    CardBox {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("Send and receive directly on this Wi-Fi", fontWeight = FontWeight.Medium)
                Muted("Messages go straight to your contact's device when it is on the same network, and through the relay otherwise. Turning this on opens a port on this phone; only devices of people you have added can use it. The network can see which devices talk, when and how much, never what.", 12.5.sp)
            }
            Switch(enabled, { want ->
                m.run { d = Bridge.obj(if (want) "direct_enable" else "direct_disable") }
            }, colors = SwitchDefaults.colors(checkedTrackColor = C.Accent))
        }
        if (enabled) {
            val addrs = st.getJSONArray("addresses").let { a -> (0 until a.length()).map { a.getString(it) } }
            Text(
                if (!st.getBoolean("listening")) "Not listening."
                else if (addrs.isEmpty()) "Listening on port ${st.getInt("port")}."
                else "Listening. Give this to your other device: ${addrs.joinToString("  ")}",
                fontFamily = Mono, fontSize = 12.sp,
            )
            Muted("Sent directly ${st.getLong("sent_direct")}, by the relay after a direct try failed ${st.getLong("sent_fallback")}, received directly ${st.getLong("received")}.", 12.5.sp)
            val contacts = st.getJSONArray("contacts")
            for (i in 0 until contacts.length()) {
                val c = contacts.getJSONObject(i)
                val name = c.getString("name")
                var addr by remember(name, c.getString("address")) { mutableStateOf(c.getString("address")) }
                val learned = c.getJSONArray("learned")
                Field("Direct address for $name", addr, { addr = it }, placeholder = if (learned.length() > 0) learned.getString(0) else "192.168.1.20:8788")
                OutlineButton("Save address for $name", small = true) {
                    m.run {
                        Bridge.call("direct_set_address", args("name" to name, "address" to addr.trim()))
                        d = Bridge.obj("direct_status")
                        m.notice = if (addr.isBlank()) "Address cleared." else "Address saved."
                    }
                }
            }
        }
    }
}
