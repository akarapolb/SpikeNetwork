package network.spike.connect

import org.json.JSONArray
import org.json.JSONObject

private fun JSONObject.str(key: String): String? =
    if (has(key) && !isNull(key)) getString(key) else null

private inline fun <T> JSONArray.mapObjects(f: (JSONObject) -> T): List<T> =
    (0 until length()).map { f(getJSONObject(it)) }

data class Contact(
    val name: String,
    val verified: Boolean,
    val fingerprint: String,
    val devices: Int,
    val unread: Int,
    val last: String?,
    val lastTs: Long,
)

data class Group(
    val id: String,
    val name: String,
    val members: Int,
    val unread: Int,
    val status: String,
    val invitedBy: String?,
    val unverified: Int,
    val large: Boolean,
    val last: String?,
    val lastTs: Long,
)

data class DeviceInfo(val id: String, val self: Boolean, val approver: Boolean)

data class Me(
    val fingerprint: String,
    val card: String,
    val devices: Int,
    val deviceList: List<DeviceInfo>,
    val listVersion: Long,
    val approver: Boolean,
)

data class Overview(
    val me: Me,
    val contacts: List<Contact>,
    val groups: List<Group>,
    val requests: Int,
    val relay: String,
)

fun parseOverview(o: JSONObject): Overview {
    val me = o.getJSONObject("me")
    return Overview(
        me = Me(
            me.getString("fingerprint"), me.getString("card"), me.getInt("devices"),
            me.getJSONArray("device_list").mapObjects {
                DeviceInfo(it.getString("id"), it.getBoolean("this"), it.getBoolean("approver"))
            },
            me.getLong("list_version"), me.getBoolean("approver"),
        ),
        contacts = o.getJSONArray("contacts").mapObjects {
            Contact(
                it.getString("name"), it.getBoolean("verified"), it.getString("fingerprint"),
                it.getInt("devices"), it.getInt("unread"), it.str("last"),
                if (it.isNull("last_ts")) 0 else it.getLong("last_ts"),
            )
        },
        groups = o.getJSONArray("groups").mapObjects {
            Group(
                it.getString("id"), it.getString("name"), it.getInt("members"), it.getInt("unread"),
                it.getString("status"), it.str("invited_by"), it.getInt("unverified"),
                it.getBoolean("large"), it.str("last"),
                if (it.isNull("last_ts")) 0 else it.getLong("last_ts"),
            )
        },
        requests = o.getInt("requests"),
        relay = o.getString("relay"),
    )
}

data class Message(
    val id: String,
    val out: Boolean,
    val ts: Long,
    val body: String,
    val edited: Boolean,
    val deleted: Boolean,
    val from: String?,
    val fromVerified: Boolean,
    val fileName: String? = null,
    val fileSize: Long = 0,
    /** The message this one replies to, as a short quote. */
    val reply: Quote? = null,
    /** Reactions, most used first. */
    val reactions: List<Reaction> = emptyList(),
)

/** A short quote of the message a reply answers: who wrote it ([out]: you) and the start of its text. */
data class Quote(val id: String, val out: Boolean, val from: String?, val text: String, val deleted: Boolean)

data class Reaction(val emoji: String, val count: Int, val mine: Boolean)

fun parseMessages(a: JSONArray): List<Message> = a.mapObjects {
    Message(
        it.getString("id"), it.getBoolean("out"), it.getLong("ts"), it.getString("body"),
        it.getBoolean("edited"), it.getBoolean("deleted"), it.str("from"),
        it.getBoolean("from_verified"),
        if (it.isNull("file")) null else it.getJSONObject("file").getString("name"),
        if (it.isNull("file")) 0 else it.getJSONObject("file").getLong("size"),
        it.optJSONObject("reply")?.let { r ->
            Quote(r.getString("id"), r.getBoolean("out"), r.str("from"), r.getString("text"), r.getBoolean("deleted"))
        },
        it.optJSONArray("reactions")?.mapObjects { r -> Reaction(r.getString("emoji"), r.getInt("count"), r.getBoolean("mine")) } ?: emptyList(),
    )
}

data class Member(
    val owner: String,
    val name: String,
    val you: Boolean,
    val admin: Boolean,
    val contact: Boolean,
    val verified: Boolean,
)

data class GroupDetail(
    val id: String,
    val name: String,
    val status: String,
    val invitedBy: String?,
    val members: List<Member>,
    val meAdmin: Boolean,
    val canAdd: Boolean,
    val addBy: String,
    val max: Int,
    val unverified: Int,
    val large: Boolean,
    val refused: Long,
    val refusedWhy: String?,
)

fun parseGroup(o: JSONObject) = GroupDetail(
    o.getString("id"), o.getString("name"), o.getString("status"), o.str("invited_by"),
    o.getJSONArray("members").mapObjects {
        Member(
            it.getString("owner"), it.getString("name"), it.getBoolean("you"),
            it.getBoolean("admin"), it.getBoolean("contact"), it.getBoolean("verified"),
        )
    },
    o.getBoolean("me_admin"), o.getBoolean("can_add"), o.getString("add_by"), o.getInt("max"),
    o.getInt("unverified"), o.getBoolean("large"),
    if (o.has("refused")) o.getLong("refused") else 0, o.str("refused_why"),
)

data class Held(val ts: Long, val body: String, val file: String?)

data class MsgRequest(val id: String, val fingerprint: String, val received: Long, val held: List<Held>)

fun parseRequests(a: JSONArray): List<MsgRequest> = a.mapObjects { r ->
    MsgRequest(
        r.getString("id"), r.getString("fingerprint"), r.getLong("received"),
        r.getJSONArray("held").mapObjects { Held(it.getLong("ts"), it.getString("body"), it.str("file")) },
    )
}

data class AgentAsk(
    val id: String,
    val from: String,
    val scope: String,
    /** The agent's own words: never the headline of a typed request. */
    val note: String,
    /** The line rendered from the signed typed fields; null for free text. */
    val summary: String?,
    /** waiting, approved or denied. */
    val status: String,
    val by: String?,
    val typed: Map<String, String>,
    val expires: Long,
    val amountCents: Long?,
    val received: Long,
    /** Waiting on your main device to record the decision. */
    val forwarded: Boolean,
)

data class Grant(
    val agent: String,
    val scopes: List<String>,
    val typedOnly: Boolean,
    val expires: Long,
    /** The most one request may move; above it the engine refuses without asking. Null: no money. */
    val maxCents: Long?,
    /** Approved without asking up to this; above it, up to the limit, it asks you. */
    val autoBelowCents: Long?,
    val perHour: Int,
    val dailyCents: Long?,
)

/** One line of the signed, hash-chained activity log: what happened, never message content. */
data class LogEntry(val seq: Long, val ts: Long, val event: String, val agent: String, val detail: String, val hash: String)

/** [logOk] is null on a device that does not keep the log, false when its check failed. */
data class Agents(val approver: Boolean, val grants: List<Grant>, val requests: List<AgentAsk>, val log: List<LogEntry>, val logOk: Boolean?)

private fun JSONObject.longOrNull(key: String): Long? = if (has(key) && !isNull(key)) getLong(key) else null

fun parseAgents(o: JSONObject) = Agents(
    o.getBoolean("approver"),
    o.getJSONArray("grants").mapObjects { g ->
        Grant(
            g.getString("agent"),
            (0 until g.getJSONArray("scopes").length()).map { g.getJSONArray("scopes").getString(it) },
            g.getBoolean("typed_only"), g.getLong("expires"),
            g.longOrNull("max_cents"), g.longOrNull("auto_below_cents"), g.optInt("per_hour", 0), g.longOrNull("daily_cents"),
        )
    },
    o.getJSONArray("requests").mapObjects { r ->
        val typed = mutableMapOf<String, String>()
        if (!r.isNull("typed")) {
            val t = r.getJSONObject("typed")
            t.keys().forEach { k -> typed[k] = t.getString(k) }
        }
        AgentAsk(
            r.getString("id"), r.getString("from"), r.getString("scope"), r.getString("note"),
            r.str("summary"), r.getString("status"), r.str("by"), typed, r.getLong("expires"),
            r.longOrNull("amount_cents"), r.optLong("received", 0), r.optBoolean("forwarded", false),
        )
    },
    if (o.has("log")) o.getJSONArray("log").mapObjects { e ->
        LogEntry(e.getLong("seq"), e.getLong("ts"), e.getString("event"), e.getString("agent"), e.getString("detail"), e.getString("hash"))
    } else emptyList(),
    if (o.has("log_ok") && !o.isNull("log_ok")) o.getBoolean("log_ok") else null,
)
