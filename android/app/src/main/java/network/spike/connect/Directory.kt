package network.spike.connect

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.json.JSONObject

// ---- the agent directory (ADR 0036): signed listings, "identity verified" only when a lister you trust signed one

data class Lister(val name: String, val key: String)

data class Listed(
    val agent: String, val name: String, val maker: String, val about: String, val scopes: List<String>,
    /** A lister you trust signed it. */
    val verified: Boolean,
    /** The trusted lister's name, "its maker", or "a lister you do not trust". */
    val listedBy: String,
    val contact: String?, val allowed: Boolean,
)

data class Directory(val listers: List<Lister>, val entries: List<Listed>)

fun parseDirectory(o: JSONObject) = Directory(
    o.getJSONArray("listers").let { a -> (0 until a.length()).map { a.getJSONObject(it).let { l -> Lister(l.getString("name"), l.getString("key")) } } },
    o.getJSONArray("entries").let { a ->
        (0 until a.length()).map { i ->
            val e = a.getJSONObject(i)
            val sc = e.getJSONArray("scopes")
            Listed(
                e.getString("agent"), e.getString("name"), e.getString("maker"), e.getString("about"),
                (0 until sc.length()).map { sc.getString(it) }, e.getBoolean("verified"), e.getString("listed_by"),
                if (e.isNull("contact")) null else e.getString("contact"), e.getBoolean("allowed"),
            )
        }
    },
)

/** A small filled button, for the one action on a directory card. */
@Composable
private fun SmallButton(text: String, primary: Boolean, onClick: () -> Unit) {
    Text(
        text, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, color = if (primary) C.OnAccent else C.Text,
        modifier = Modifier.clip(RoundedCornerShape(10.dp)).then(
            if (primary) Modifier.background(C.Accent) else Modifier.background(C.Surface).border(1.dp, C.BorderStrong, RoundedCornerShape(10.dp)),
        ).clickable(onClick = onClick).padding(horizontal = 12.dp, vertical = 7.dp),
    )
}

@Composable
private fun Flag(text: String) = Text(
    text, fontSize = 11.5.sp, fontWeight = FontWeight.Medium, color = C.RedText,
    modifier = Modifier.clip(RoundedCornerShape(999.dp)).background(C.RedBg).padding(horizontal = 8.dp, vertical = 2.dp),
)

/** Add a listed agent as a contact (it grants nothing), then open its empty limits form. */
private fun allow(m: AppModel, e: Listed) = m.run {
    val name = Bridge.call("add_listed", args("agent" to e.agent)) as String
    m.open(Screen.AgentGrant(name))
}

/** "Find an agent", as in the design: search, then one card per listed agent with its one action. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun DirectoryScreen(m: AppModel) {
    val d = m.directory
    var q by remember { mutableStateOf("") }
    Page {
        TopBar("Find an agent", { m.open(Screen.NewGrant) }, sub = "An agent can do nothing until you set its limits")
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 20.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            if (d == null) { Muted("Loading…", 14.sp); return@Column }
            Field(null, q, { q = it }, placeholder = "Search agents")
            if (d.entries.isEmpty()) Muted(
                if (d.listers.isEmpty()) "No listings yet, and nobody is trusted to vouch for an agent. Add a lister you trust (your organisation, or one you chose), then add its listings."
                else "No listings yet. Add the listings your lister publishes.",
                13.5.sp,
            )
            val shown = d.entries.filter { q.isBlank() || listOf(it.name, it.maker, it.about).any { s -> s.contains(q.trim(), ignoreCase = true) } }
            if (d.entries.isNotEmpty() && shown.isEmpty()) Muted("No agent matches “${q.trim()}”.", 13.5.sp)
            for (e in shown) {
                Column(
                    Modifier.fillMaxWidth().clip(RoundedCornerShape(16.dp)).border(1.dp, C.BorderStrong, RoundedCornerShape(16.dp)).padding(14.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        Box(Modifier.size(38.dp).clip(RoundedCornerShape(11.dp)).background(C.AvatarBg), contentAlignment = Alignment.Center) { Ico(Ic.Sparkle, C.AvatarText, 17.dp) }
                        Column(Modifier.weight(1f)) {
                            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                                Text(e.name, fontSize = 14.5.sp, fontWeight = FontWeight.SemiBold)
                                if (e.verified) Ico(Ic.ShieldCheck, C.Green, 15.dp)
                            }
                            if (e.verified) Text("Identity verified · by ${e.listedBy}", fontSize = 12.sp, color = C.Muted)
                            else Flag("Identity not verified")
                        }
                    }
                    Text(if (e.verified) e.about else "${e.about.ifBlank { "Listed by ${e.listedBy}." }} Nobody you trust has checked who runs it.".trim(), fontSize = 13.sp, color = C.AvatarText)
                    if (e.scopes.isNotEmpty()) FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        for (s in e.scopes) Text(
                            scopeWords(s), fontSize = 12.sp,
                            modifier = Modifier.clip(RoundedCornerShape(999.dp)).border(1.dp, C.Border, RoundedCornerShape(999.dp)).padding(horizontal = 9.dp, vertical = 2.dp),
                        )
                    }
                    when {
                        e.allowed -> SmallButton("Allowed", false) { m.open(Screen.AgentGrant(e.contact!!)) }
                        e.verified -> SmallButton("Set limits", true) { allow(m, e) }
                        else -> SmallButton("Review first", false) { m.open(Screen.ReviewListed(e.agent)) }
                    }
                }
            }
        }
        Row(Modifier.navigationBarsPadding().padding(start = 20.dp, end = 20.dp, bottom = 14.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            OutlineButton("Trusted listers", Modifier.weight(1f)) { m.open(Screen.Listers) }
            OutlineButton("Add listings", Modifier.weight(1f)) { m.open(Screen.Listers) }
        }
    }
}

/** An agent nobody you trust vouches for: what it says about itself, the warning, and then the choice. */
@Composable
fun ReviewListedScreen(m: AppModel, agent: String) {
    val e = m.directory?.entries?.firstOrNull { it.agent == agent }
    StepPage(
        { m.open(Screen.Directory) }, title = e?.name ?: "Agent",
        actions = { if (e != null) { OutlineButton("Add it and set limits", Modifier.fillMaxWidth()) { allow(m, e) }; TextLink("Not now") { m.open(Screen.Directory) } } },
    ) {
        if (e == null) { Muted("This listing is no longer here.", 14.sp); return@StepPage }
        Flag("Identity not verified")
        Text(
            "Listed by ${e.listedBy}. Nobody you trust has checked who runs it, so its name and description are its own words. " +
                "Its limits still protect you: it can do nothing you do not allow.",
            color = C.AvatarText, fontSize = 14.sp,
        )
        Column {
            KvRow("Says it is made by", e.maker)
            if (e.about.isNotBlank()) KvRow("Says it does", e.about)
            KvRow("Asks for", e.scopes.joinToString(", ") { scopeWords(it) }.ifEmpty { "Nothing listed" })
            KvRow("Key", e.agent.take(8) + "…" + e.agent.takeLast(8))
        }
    }
}

/** Who may vouch for an agent, and adding the listings they publish. Nobody is trusted until added here. */
@Composable
fun ListersScreen(m: AppModel) {
    val d = m.directory
    var name by remember { mutableStateOf("") }
    var key by remember { mutableStateOf("") }
    var listings by remember { mutableStateOf("") }
    Page {
        TopBar("Agent directory", { m.open(Screen.Directory) }, sub = "Nobody vouches for an agent until you add them")
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 20.dp, vertical = 14.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Listers you trust", fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
            Muted("A lister checks who runs an agent and signs its listing. Their listings mark an agent “identity verified”. Spike checks nobody.", 12.5.sp)
            if (d != null && d.listers.isEmpty()) Muted("None yet.", 13.sp)
            for (l in d?.listers ?: emptyList()) Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(l.name, fontSize = 14.sp, fontWeight = FontWeight.Medium)
                    Text(l.key.take(8) + "…" + l.key.takeLast(8), fontFamily = Mono, fontSize = 11.5.sp, color = C.Muted)
                }
                OutlineButton("Remove", small = true, danger = true) { m.run { Bridge.call("remove_lister", args("key" to l.key)); m.refresh() } }
            }
            Field("Lister's name", name, { name = it }, placeholder = "Northwind IT")
            Field("Lister's key or card", key, { key = it }, placeholder = "64 hex characters or spike2:…", singleLine = false, maxLines = 3)
            OutlineButton("Trust this lister", Modifier.fillMaxWidth(), enabled = name.isNotBlank() && key.isNotBlank() && !m.busy) {
                m.run { m.notice = Bridge.call("add_lister", args("name" to name, "key" to key)) as? String; name = ""; key = ""; m.refresh() }
            }
            Box(Modifier.fillMaxWidth().height(1.dp).background(C.Border))
            Text("Add listings", fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
            Muted("Paste the signed listings a lister or an agent's maker gave you. Each one is checked; ones that do not check out are left out.", 12.5.sp)
            Field(null, listings, { listings = it }, placeholder = "Signed listings", singleLine = false, minHeight = 96.dp, maxLines = 6)
            OutlineButton("Add listings", Modifier.fillMaxWidth(), enabled = listings.isNotBlank() && !m.busy) {
                m.run { m.notice = Bridge.call("import_listings", args("text" to listings)) as? String; listings = ""; m.refresh() }
            }
        }
    }
}
