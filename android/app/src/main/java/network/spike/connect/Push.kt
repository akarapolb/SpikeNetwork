package network.spike.connect

import android.content.Context
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.UnifiedPush
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage

/**
 * Waking the app without Google's push service (ADR 0028), through the open UnifiedPush standard. The person picks a push
 * app on their phone (ntfy, for example); it gives this app a private web address; the app leaves that address with the
 * relay. When a message waits for a device that is not connected, the relay sends one empty message to that address, the
 * push app wakes this app, and the app connects and reads. The push says only "something is waiting": no sender, no
 * text. The push server (yours, or ntfy.sh) learns the time of each wake-up, not what it was for.
 *
 * What a wake-up can do depends on the app being able to open its engine: that needs "Keep receiving after a restart"
 * (the passphrase sealed under the phone's hardware key). Without it a wake-up can only say that something is waiting.
 */
object PushWake {
    private const val INSTANCE = "spike"
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    private fun p(c: Context) = c.getSharedPreferences("spike_push", Context.MODE_PRIVATE)

    /** The person turned this on and a push app gave us an address. */
    fun enabled(c: Context) = p(c).getBoolean("on", false)
    fun endpoint(c: Context): String? = p(c).getString("endpoint", null)
    fun distributor(c: Context): String? = try { UnifiedPush.getSavedDistributor(c) } catch (_: Exception) { null }

    /** Push apps installed on this phone. */
    fun available(c: Context): List<String> = try { UnifiedPush.getDistributors(c) } catch (_: Exception) { emptyList() }

    /** Ask for a push app (the library picks the default one, or lets the person choose) and register with it. */
    fun enable(activity: android.app.Activity, done: (ok: Boolean) -> Unit) {
        UnifiedPush.tryUseCurrentOrDefaultDistributor(activity as Context) { ok ->
            if (ok) {
                p(activity).edit().putBoolean("on", true).apply()
                UnifiedPush.register(activity, INSTANCE)
                Trace.add("push: registering with the push app")
            }
            done(ok)
        }
    }

    fun disable(c: Context) {
        p(c).edit().clear().apply()
        try { UnifiedPush.unregister(c, INSTANCE) } catch (_: Exception) {}
        try { UnifiedPush.removeDistributor(c) } catch (_: Exception) {}
        scope.launch { try { Bridge.call("push_unregister") } catch (_: Exception) {} }
    }

    /** A push app gave us an address: keep it and leave it with the relay. */
    fun gotEndpoint(c: Context, url: String) {
        p(c).edit().putString("endpoint", url).putBoolean("on", true).remove("registered").apply()
        Trace.add("push: got an address from the push app")
        scope.launch { syncRelay(c) }
    }

    /** Leave the address with the relay if the relay does not have it yet. Needs the engine open. */
    suspend fun syncRelay(c: Context) {
        val url = endpoint(c) ?: return
        if (!enabled(c) || p(c).getString("registered", null) == url) return
        try {
            Bridge.call("push_register", args("url" to url))
            p(c).edit().putString("registered", url).apply()
            Trace.add("push: the relay has the address")
        } catch (e: Exception) {
            Trace.add("push: the relay refused the address (${e.message?.take(60)})")
        }
    }

    /**
     * A wake-up arrived. Open the engine if we can, read what is waiting, and let the call code ring. Returns when done.
     * Runs inside the push app's short window, so it does only that.
     */
    suspend fun wake(c: Context): Boolean {
        Trace.add("push: wake-up received")
        try {
            Bridge.ensureOpen(c)
            val st = Bridge.obj("app_status")
            if (!st.getBoolean("unlocked")) {
                val pass = BackgroundUnlock.passphrase(c)
                if (pass == null || !st.getBoolean("exists")) {
                    Trace.add("push: the app is locked; it cannot read until it is opened")
                    Notifier.news(c, 1)
                    return false
                }
                Bridge.call("unlock", args("passphrase" to pass))
            }
            Listener.ready = true
            CallManager.start(c)
            if (Prefs.notifyInBackground(c)) try { ListenerService.start(c) } catch (_: Exception) {}
            val r = Bridge.obj("sync")
            val news = r.getInt("received") + r.getInt("groups") + r.getInt("message_requests")
            if (news > 0 && !Listener.visible) Notifier.news(c, news)
            // Let the call code take a call event, if the wake-up was one.
            var waited = 0
            while (CallManager.ui == null && waited < 6000 && news == 0) { delay(250); waited += 250 }
            return true
        } catch (e: Exception) {
            Trace.add("push: could not read after the wake-up (${e.message?.take(60)})")
            return false
        }
    }

    fun onWake(c: Context) { scope.launch { wake(c.applicationContext) } }
}

class SpikePushReceiver : org.unifiedpush.android.connector.MessagingReceiver() {
    override fun onNewEndpoint(context: Context, endpoint: PushEndpoint, instance: String) {
        PushWake.gotEndpoint(context.applicationContext, endpoint.url)
    }

    override fun onMessage(context: Context, message: PushMessage, instance: String) {
        // The push app gives a receiver about ten seconds: hold the broadcast open while the app reads.
        val pending = goAsync()
        kotlin.concurrent.thread {
            try { kotlinx.coroutines.runBlocking { PushWake.wake(context.applicationContext) } } finally { pending.finish() }
        }
    }

    override fun onRegistrationFailed(context: Context, reason: FailedReason, instance: String) {
        Trace.add("push: the push app could not register us ($reason)")
    }

    override fun onUnregistered(context: Context, instance: String) {
        Trace.add("push: the push app unregistered us")
        context.applicationContext.getSharedPreferences("spike_push", Context.MODE_PRIVATE).edit().remove("endpoint").remove("registered").apply()
    }
}

/** Settings: wake the app without Google's push service. */
@Composable
fun PushSetup(onNotice: (String) -> Unit) {
    val ctx = LocalContext.current
    var on by remember { mutableStateOf(PushWake.enabled(ctx)) }
    val apps = remember { PushWake.available(ctx) }
    Row(verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            androidx.compose.material3.Text("Wake-ups without Google (UnifiedPush)", fontWeight = FontWeight.Medium)
            Muted(
                "Lets your relay wake this phone for a call or message when the app was stopped, through a push app you choose (ntfy, for example) instead of Google. The push says only that something is waiting. " +
                    if (apps.isEmpty()) "No push app is installed on this phone yet: install one, then come back." else "Push app found: " + apps.joinToString(", ") + ".",
                12.5.sp,
            )
            if (on) {
                val url = PushWake.endpoint(ctx)
                Muted(if (url == null) "Waiting for the push app to give an address." else "Registered with the push app and your relay (" + (url.substringAfter("://").substringBefore("/")) + ").", 12.5.sp)
            }
        }
        Switch(on, { want ->
            val act = ctx as? android.app.Activity
            if (!want) { PushWake.disable(ctx); on = false }
            else if (apps.isEmpty() || act == null) onNotice("No push app found. Install ntfy or another UnifiedPush app first.")
            else PushWake.enable(act) { ok -> if (ok) on = true else onNotice("Could not set up a push app.") }
        }, colors = SwitchDefaults.colors(checkedTrackColor = C.Accent))
    }
}
