package network.spike.connect

import android.os.Bundle
import android.view.WindowManager
import androidx.activity.SystemBarStyle
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.isImeVisible
import androidx.activity.enableEdgeToEdge
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.height
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.fragment.app.FragmentActivity
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.compose.runtime.DisposableEffect
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/** Minutes away from the app before the keys are forgotten and the passphrase is asked again (alerts off). */
const val AUTO_LOCK_MINUTES = 2

class MainActivity : FragmentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Draw edge to edge with dark status and navigation icons: the screens are light.
        val clear = android.graphics.Color.TRANSPARENT
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.light(clear, clear),
            navigationBarStyle = SystemBarStyle.light(clear, clear),
        )
        applyScreenPrivacy()
        Bridge.ensureOpen(applicationContext)
        setContent {
            SpikeTheme {
                Surface(Modifier.fillMaxSize(), color = C.Bg) { Root() }
            }
        }
    }

    override fun onResume() {
        super.onResume()
        applyScreenPrivacy() // the setting may have changed on the Settings screen
    }

    /** Keep messages out of screenshots, screen recordings and the recent-apps thumbnail unless the person allows them. */
    fun refreshScreenPrivacy() = applyScreenPrivacy()

    private fun applyScreenPrivacy() {
        if (Prefs.allowScreenshots(this)) window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
        else window.setFlags(WindowManager.LayoutParams.FLAG_SECURE, WindowManager.LayoutParams.FLAG_SECURE)
    }
}

@OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)
@Composable
private fun Root() {
    val scope = rememberCoroutineScope()
    val m = remember { AppModel(scope) }
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val autoLock = remember { arrayOfNulls<kotlinx.coroutines.Job>(1) }

    LaunchedEffect(Unit) { m.run { m.loadStatus() } }
    // Listen to the relay while the app is on screen (and, with alerts on, while the service runs).
    val ctx = androidx.compose.ui.platform.LocalContext.current
    DisposableEffect(lifecycle) {
        val obs = LifecycleEventObserver { _, e ->
            if (e == Lifecycle.Event.ON_START) { Listener.visible = true; Notifier.clearNews(ctx); autoLock[0]?.cancel(); autoLock[0] = null }
            if (e == Lifecycle.Event.ON_STOP) {
                Listener.visible = false
                // Away from the app: forget the keys after a while, unless alerts need the engine to keep reading.
                if (m.status == "ready" && !Prefs.notifyInBackground(ctx)) {
                    autoLock[0] = scope.launch {
                        delay(AUTO_LOCK_MINUTES * 60_000L)
                        try { Bridge.call("lock") } catch (_: Exception) {}
                        m.loadStatus()
                    }
                }
            }
            Listener.update(ctx)
        }
        lifecycle.addObserver(obs)
        onDispose { lifecycle.removeObserver(obs) }
    }
    DisposableEffect(Unit) {
        Listener.onRead = { ok, news -> m.listened(ok, news) }
        onDispose { Listener.onRead = null }
    }
    LaunchedEffect(m.status) {
        Listener.ready = m.status == "ready"
        Listener.update(ctx)
        if (m.status == "ready" && Prefs.notifyInBackground(ctx) && Notifier.allowed(ctx)) ListenerService.start(ctx)
    }
    BackHandler(enabled = m.status == "ready" && m.screen != null) {
        when (val s = m.screen) {
            is Screen.Members -> m.open(Screen.GroupChat(s.id))
            is Screen.Verify -> m.open(Screen.Chat(s.name))
            Screen.LinkNew, Screen.LinkOld -> m.run { try { Bridge.call("link_cancel") } catch (_: Exception) {}; m.open(null) }
            else -> m.open(null)
        }
    }

    // Back closes the keyboard first (some keyboards leave that to the app).
    val imeVisible = androidx.compose.foundation.layout.WindowInsets.Companion.isImeVisible
    val focus = androidx.compose.ui.platform.LocalFocusManager.current
    BackHandler(enabled = imeVisible) { focus.clearFocus() }

    Box(Modifier.fillMaxSize()) {
        when (m.status) {
            "loading" -> Text("Loading…", Modifier.align(Alignment.Center), color = C.Muted)
            "new", "locked" -> GateScreen(m)
            else -> Ready(m)
        }
        m.notice?.let { text ->
            Row(
                Modifier.align(Alignment.BottomCenter).navigationBarsPadding().padding(16.dp).clip(RoundedCornerShape(14.dp))
                    .background(C.Text).padding(start = 16.dp, end = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(text, color = Color.White, fontSize = 14.sp, modifier = Modifier.weight(1f, fill = false).padding(vertical = 12.dp))
                Box(Modifier.clickable { m.notice = null }.padding(horizontal = 14.dp, vertical = 12.dp)) {
                    Text("OK", color = Color.White, fontWeight = FontWeight.SemiBold, fontSize = 14.sp)
                }
            }
        }
    }
}

@Composable
private fun Ready(m: AppModel) {
    val o = m.overview
    when (val s = m.screen) {
        null -> Column(Modifier.fillMaxSize()) {
            Box(Modifier.weight(1f)) {
                when (m.tab) {
                    Tab.Chats -> ChatsTab(m)
                    Tab.Agents -> AgentsTab(m)
                    Tab.Devices -> DevicesTab(m)
                    Tab.Settings -> SettingsTab(m)
                }
            }
            Box(Modifier.fillMaxWidth().height(1.dp).background(C.Border))
            TabBar(m)
        }
        is Screen.Chat -> {
            val c = o?.contacts?.firstOrNull { it.name == s.name }
            val n = c?.devices ?: 1
            ConversationScreen(
                m, s.name, "End-to-end encrypted · $n ${if (n == 1) "device" else "devices"}", c?.verified == true, false,
                if (n == 1) "Encrypted for ${s.name}'s device and your own other devices" else "Encrypted separately for each of ${s.name}'s $n devices and your own other devices",
                { t -> Bridge.call("send", args("name" to s.name, "text" to t)) },
                { m.open(Screen.Verify(s.name)) },
                { fileName, f -> Bridge.call("send_file", args("name" to s.name, "file_name" to fileName, "path" to f.absolutePath, "caption" to "")) },
            )
        }
        is Screen.GroupChat -> {
            val g = m.group
            val people = g?.members?.size ?: 0
            ConversationScreen(
                m, g?.name ?: "Group",
                "$people ${if (people == 1) "person" else "people"} · ${if (g?.large == true) "large group (MLS)" else "end-to-end encrypted"}",
                false, true,
                if (g?.large == true) "One encrypted copy for everyone in the group" else "Encrypted separately for every device of every member",
                { t -> Bridge.call("group_send", args("id" to s.id, "text" to t)) },
                { m.open(Screen.Members(s.id)) },
                // Files in large groups are not built yet (the engine refuses them).
                if (g?.large == true) null else { fileName, f -> Bridge.call("group_send_file", args("id" to s.id, "file_name" to fileName, "path" to f.absolutePath, "caption" to "")) },
            )
        }
        is Screen.Members -> MembersScreen(m, s.id)
        is Screen.Invite -> InviteScreen(m, s.id)
        is Screen.Verify -> VerifyScreen(m, s.name)
        Screen.NewGroup -> NewGroupScreen(m)
        Screen.AddContact -> AddContactScreen(m)
        Screen.Requests -> RequestsScreen(m)
        Screen.LinkNew -> LinkNewScreen(m)
        Screen.LinkOld -> LinkOldScreen(m)
    }
}
