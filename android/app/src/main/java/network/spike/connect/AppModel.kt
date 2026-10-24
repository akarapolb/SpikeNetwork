package network.spike.connect

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

enum class Tab { Chats, Agents, Devices, Settings }

/** A screen that sits on top of the tabs. */
sealed interface Screen {
    data class Chat(val name: String) : Screen
    data class GroupChat(val id: String) : Screen
    data class Members(val id: String) : Screen
    data class Invite(val id: String) : Screen
    data class Verify(val name: String) : Screen
    data object NewGroup : Screen
    data object AddContact : Screen
    data object Requests : Screen

    /** This phone is the NEW device: show a code for the existing device to enter. */
    data object LinkNew : Screen

    /** This phone is the EXISTING device: enter the new device's code. */
    data object LinkOld : Screen
}

/** What the screens show, and what they ask for. One per app process. */
class AppModel(private val scope: CoroutineScope) {
    /** loading, new (no identity yet), locked, or ready. */
    var status by mutableStateOf("loading")
    var overview by mutableStateOf<Overview?>(null)
    var tab by mutableStateOf(Tab.Chats)
    var screen by mutableStateOf<Screen?>(null)
    var notice by mutableStateOf<String?>(null)
    var messages by mutableStateOf<List<Message>>(emptyList())
    var group by mutableStateOf<GroupDetail?>(null)
    var requests by mutableStateOf<List<MsgRequest>>(emptyList())
    var agents by mutableStateOf<Agents?>(null)
    var busy by mutableStateOf(false)

    /** Whether the last talk to the relay worked: the chip on the chat list. */
    var relayOk by mutableStateOf<Boolean?>(null)

    /** Run something for the person; an error becomes a notice, not a crash. */
    fun run(block: suspend () -> Unit) {
        scope.launch {
            busy = true
            try {
                block()
            } catch (e: BridgeError) {
                notice = e.message
            } catch (e: Exception) {
                notice = e.message ?: e.toString()
            } finally {
                busy = false
            }
        }
    }

    suspend fun loadStatus() {
        val s = Bridge.obj("app_status")
        status = when {
            s.getBoolean("unlocked") -> "ready"
            s.getBoolean("exists") -> "locked"
            else -> "new"
        }
        if (status == "ready") refresh()
    }

    /** Re-read what the screens show, from the engine. */
    suspend fun refresh() {
        overview = parseOverview(Bridge.obj("overview"))
        when (val s = screen) {
            is Screen.Chat -> messages = parseMessages(Bridge.arr("conversation", args("name" to s.name)))
            is Screen.GroupChat -> loadGroup(s.id, true)
            is Screen.Members -> loadGroup(s.id, false)
            is Screen.Invite -> loadGroup(s.id, false)
            Screen.Requests -> requests = parseRequests(Bridge.arr("message_requests"))
            else -> {}
        }
        if (screen == null && tab == Tab.Agents) agents = parseAgents(Bridge.obj("agents"))
    }

    private suspend fun loadGroup(id: String, withMessages: Boolean) {
        try {
            group = parseGroup(Bridge.obj("group", args("id" to id)))
            if (withMessages) messages = parseMessages(Bridge.arr("group_conversation", args("id" to id)))
        } catch (_: BridgeError) {
            screen = null
            group = null
        }
    }

    fun open(s: Screen?) {
        screen = s
        messages = emptyList()
        group = null
        run {
            refresh()
            when (s) {
                is Screen.Chat -> Bridge.call("mark_read", args("name" to s.name))
                is Screen.GroupChat -> Bridge.call("group_mark_read", args("id" to s.id))
                else -> {}
            }
            refresh()
        }
    }

    fun select(t: Tab) {
        tab = t
        screen = null
        run { refresh() }
    }

    /** The listener read the relay: say whether it answered, and show what came. */
    fun listened(ok: Boolean, news: Int) {
        scope.launch {
            relayOk = ok
            if (ok && news >= 0) {
                try { refresh() } catch (_: Exception) {}
            }
        }
    }

    /** Something arrived directly from another device: show it (the engine already stored it). */
    fun directArrived() {
        scope.launch {
            try { refresh() } catch (_: Exception) {}
        }
    }
}
