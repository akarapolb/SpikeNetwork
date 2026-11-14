package network.spike.connect

import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.app.NotificationCompat
import androidx.core.app.Person
import androidx.core.app.RemoteInput
import org.json.JSONArray
import org.json.JSONObject

/**
 * Message previews in notifications, only when the person turns them on (Settings, off by default). Then each chat
 * with something new gets its own notification: who wrote and what, with quick reactions and a reply box. The lock
 * screen still shows only "Spike Connect" (the public version). With previews off, [Notifier.news] says only that
 * something arrived, as before.
 */
object Previews {
    /** The reactions offered on a notification (the first of the chat's quick reactions). */
    val QUICK = listOf("👍", "❤️", "😂")
    const val REPLY_KEY = "reply_text"
    private const val MAX_LINES = 5

    /** Unread counts already shown per chat, so a sync that brought nothing new for a chat does not alert again. */
    private val shown = HashMap<String, Int>()

    /** One chat with something new: the contact's name or the group's id and name. */
    private data class Chat(val key: String, val title: String, val contact: String?, val group: String?, val unread: Int)

    /**
     * Post a notification per chat with new messages. Runs on the listener's thread (it reads the engine). Returns
     * false when there was nothing it could show, so the caller falls back to the plain "New message".
     */
    @Synchronized
    fun show(ctx: Context): Boolean {
        val o = try { Bridge.callBlocking("overview") as JSONObject } catch (_: Exception) { return false }
        val chats = mutableListOf<Chat>()
        val cs = o.getJSONArray("contacts")
        for (i in 0 until cs.length()) cs.getJSONObject(i).let { c ->
            if (c.getInt("unread") > 0) chats += Chat("c:" + c.getString("name"), c.getString("name"), c.getString("name"), null, c.getInt("unread"))
        }
        val gs = o.getJSONArray("groups")
        for (i in 0 until gs.length()) gs.getJSONObject(i).let { g ->
            if (g.getInt("unread") > 0 && g.getString("status") == "joined") chats += Chat("g:" + g.getString("id"), g.getString("name"), null, g.getString("id"), g.getInt("unread"))
        }
        var posted = false
        for (c in chats) {
            if ((shown[c.key] ?: 0) >= c.unread) { posted = true; continue }
            val msgs = try {
                if (c.contact != null) Bridge.callBlocking("conversation", args("name" to c.contact)) as JSONArray
                else Bridge.callBlocking("group_conversation", args("id" to c.group)) as JSONArray
            } catch (_: Exception) { continue }
            val incoming = (0 until msgs.length()).map { msgs.getJSONObject(it) }
                .filter { !it.getBoolean("out") && !it.getBoolean("deleted") }
                .takeLast(minOf(c.unread, MAX_LINES))
            if (incoming.isEmpty()) continue
            post(ctx, c, incoming)
            shown[c.key] = c.unread
            posted = true
        }
        return posted
    }

    /** Forget what was shown (the app was opened: everything is read or on screen). */
    @Synchronized
    fun clear(ctx: Context) {
        val nm = ctx.getSystemService(NotificationManager::class.java)
        for (k in shown.keys) nm.cancel(idOf(k))
        shown.clear()
    }

    fun idOf(key: String) = 1000 + (key.hashCode() and 0x3fffffff) % 100_000

    private fun text(m: JSONObject): String {
        val body = m.getString("body")
        val file = if (m.has("file") && !m.isNull("file")) m.getJSONObject("file").optString("name") else ""
        return when {
            body.isNotBlank() -> body
            file.isNotEmpty() -> "Sent a file: $file"
            else -> "New message"
        }
    }

    private fun post(ctx: Context, c: Chat, msgs: List<JSONObject>) {
        val id = idOf(c.key)
        val me = Person.Builder().setName("You").build()
        val style = NotificationCompat.MessagingStyle(me).setConversationTitle(if (c.group != null) c.title else null).setGroupConversation(c.group != null)
        for (m in msgs) {
            val from = if (c.group != null) (if (m.isNull("from")) "Someone" else m.getString("from")) else c.title
            style.addMessage(text(m), m.getLong("ts"), Person.Builder().setName(from).build())
        }
        val last = msgs.last()
        val target = last.getString("id")
        fun action(kind: String, code: Int, extra: Intent.() -> Unit, mutable: Boolean = false): PendingIntent =
            PendingIntent.getBroadcast(
                ctx, id * 8 + code,
                Intent(ctx, PreviewActionReceiver::class.java).setAction(kind).putExtra("notif", id).putExtra("key", c.key)
                    .putExtra("target", target).putExtra("contact", c.contact).putExtra("group", c.group).apply(extra),
                PendingIntent.FLAG_UPDATE_CURRENT or (if (mutable) PendingIntent.FLAG_MUTABLE else PendingIntent.FLAG_IMMUTABLE),
            )
        val b = NotificationCompat.Builder(ctx, Notifier.NEWS)
            .setSmallIcon(android.R.drawable.stat_notify_chat)
            .setStyle(style)
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .setAutoCancel(true)
            .setContentIntent(Notifier.openApp(ctx))
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setPublicVersion(NotificationCompat.Builder(ctx, Notifier.NEWS).setSmallIcon(android.R.drawable.stat_notify_chat).setContentTitle("Spike Connect").build())
        val reply = RemoteInput.Builder(REPLY_KEY).setLabel("Reply to ${c.title}").build()
        b.addAction(
            NotificationCompat.Action.Builder(0, "Reply", action("reply", 0, {}, mutable = true))
                .addRemoteInput(reply).setAllowGeneratedReplies(false)
                .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_REPLY).build(),
        )
        QUICK.forEachIndexed { i, e ->
            b.addAction(NotificationCompat.Action.Builder(0, e, action("react", i + 1, { putExtra("emoji", e) })).build())
        }
        ctx.getSystemService(NotificationManager::class.java).notify(id, b.build())
    }
}

/** A reaction or a reply from a preview notification: sent by the engine, then the notification goes away. */
class PreviewActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val pending = goAsync()
        val ctx = context.applicationContext
        Thread {
            val nm = ctx.getSystemService(NotificationManager::class.java)
            val id = intent.getIntExtra("notif", 0)
            val target = intent.getStringExtra("target") ?: ""
            val contact = intent.getStringExtra("contact")
            val group = intent.getStringExtra("group")
            try {
                Bridge.ensureOpen(ctx)
                when (intent.action) {
                    "react" -> Bridge.callBlocking("react", args("id" to target, "emoji" to (intent.getStringExtra("emoji") ?: "")))
                    "reply" -> {
                        val text = RemoteInput.getResultsFromIntent(intent)?.getCharSequence(Previews.REPLY_KEY)?.toString()?.trim().orEmpty()
                        if (text.isNotEmpty()) {
                            if (contact != null) Bridge.callBlocking("send", args("name" to contact, "text" to text, "reply_to" to target))
                            else Bridge.callBlocking("group_send", args("id" to group, "text" to text, "reply_to" to target))
                        }
                    }
                }
                Trace.add("notification: ${intent.action} sent")
                nm.cancel(id)
            } catch (e: Exception) {
                // Locked, or no relay: say so on the notification itself; the app shows the rest.
                Trace.add("notification: ${intent.action} failed (${e.message?.take(60)})")
                nm.notify(
                    id,
                    NotificationCompat.Builder(ctx, Notifier.NEWS).setSmallIcon(android.R.drawable.stat_notify_chat)
                        .setContentTitle("Spike Connect").setContentText("Could not send. Open the app to try again.")
                        .setContentIntent(Notifier.openApp(ctx)).setAutoCancel(true).build(),
                )
            } finally {
                pending.finish()
            }
        }.start()
    }
}
