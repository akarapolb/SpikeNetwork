package network.spike.connect

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** Small settings the app keeps for itself (never keys or messages). */
object Prefs {
    private fun p(c: Context) = c.getSharedPreferences("spike_app", Context.MODE_PRIVATE)
    fun notifyInBackground(c: Context) = p(c).getBoolean("notify_background", false)
    fun setNotifyInBackground(c: Context, on: Boolean) = p(c).edit().putBoolean("notify_background", on).apply()
}

/**
 * Keeps up with the relay: waits for it to say something is waiting, then reads it. Runs while the app
 * is on screen, and also while the foreground service is on (so notifications can arrive with the app
 * closed). One loop for both, so nothing is read twice.
 */
object Listener {
    @Volatile var visible = false
    @Volatile var background = false
    @Volatile var ready = false
    @Volatile var paused = false

    /** Called after each read, from a background thread: whether the relay answered, and how many new things came. */
    @Volatile var onRead: ((ok: Boolean, news: Int) -> Unit)? = null

    /** Called when something arrived directly from another device on the network (ADR 0027): already stored, show it. */
    @Volatile var onDirect: ((news: Int) -> Unit)? = null

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private var job: Job? = null

    @Synchronized
    fun update(ctx: Context) {
        val want = ready && !paused && (visible || background)
        if (want && job?.isActive != true) {
            val app = ctx.applicationContext
            job = scope.launch { launch { directLoop(app) }; loop(app) }
        } else if (!want) {
            job?.cancel()
            job = null
        }
    }

    /** Direct deliveries are stored by the engine's own listener; this only tells the screen. */
    private suspend fun directLoop(ctx: Context) {
        while (currentCoroutineContext().isActive) {
            try {
                val r = Bridge.call("direct_wait", args("ms" to 20000))
                if (r is org.json.JSONObject) {
                    val news = r.getInt("received") + r.getInt("groups") + r.getInt("message_requests")
                    if (news > 0 && !visible) Notifier.news(ctx, news)
                    onDirect?.invoke(news)
                }
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Exception) {
                delay(4000)
            }
        }
    }

    private suspend fun loop(ctx: Context) {
        while (currentCoroutineContext().isActive) {
            try {
                Bridge.call("wait_for_push", args("ms" to 20000))
                if (!currentCoroutineContext().isActive || paused) break
                val r = Bridge.obj("sync")
                val news = r.getInt("received") + r.getInt("groups") + r.getInt("message_requests")
                if (news > 0 && !visible) Notifier.news(ctx, news)
                onRead?.invoke(true, news)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Exception) {
                // No relay, or locked: try again shortly.
                android.util.Log.w("SpikeListen", "read failed: ${e.message}")
                onRead?.invoke(false, 0)
                delay(4000)
            }
        }
    }
}

object Notifier {
    private const val NEWS = "news"
    private const val SERVICE = "service"
    const val SERVICE_ID = 1
    private const val NEWS_ID = 2

    private fun ensureChannels(ctx: Context) {
        val nm = ctx.getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(NEWS, "New messages", NotificationManager.IMPORTANCE_DEFAULT))
        nm.createNotificationChannel(NotificationChannel(SERVICE, "Listening for messages", NotificationManager.IMPORTANCE_LOW))
    }

    private fun open(ctx: Context): PendingIntent =
        PendingIntent.getActivity(
            ctx, 0, Intent(ctx, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )

    fun allowed(ctx: Context): Boolean =
        Build.VERSION.SDK_INT < 33 ||
            ContextCompat.checkSelfPermission(ctx, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

    /** The ongoing notification that keeps the listener alive. */
    fun serviceNotification(ctx: Context): Notification {
        ensureChannels(ctx)
        return NotificationCompat.Builder(ctx, SERVICE)
            .setSmallIcon(android.R.drawable.stat_notify_chat)
            .setContentTitle("Spike Connect")
            .setContentText("Listening for new messages")
            .setOngoing(true)
            .setContentIntent(open(ctx))
            .setVisibility(NotificationCompat.VISIBILITY_SECRET)
            .build()
    }

    /** Says that something arrived, never what: the text of a message stays out of notifications. */
    fun news(ctx: Context, count: Int) {
        if (!allowed(ctx)) return
        ensureChannels(ctx)
        val n = NotificationCompat.Builder(ctx, NEWS)
            .setSmallIcon(android.R.drawable.stat_notify_chat)
            .setContentTitle("Spike Connect")
            .setContentText(if (count == 1) "New message" else "$count new messages")
            .setAutoCancel(true)
            .setContentIntent(open(ctx))
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setPublicVersion(
                NotificationCompat.Builder(ctx, NEWS).setSmallIcon(android.R.drawable.stat_notify_chat).setContentTitle("Spike Connect").build(),
            )
            .build()
        ctx.getSystemService(NotificationManager::class.java).notify(NEWS_ID, n)
    }

    fun clearNews(ctx: Context) {
        ctx.getSystemService(NotificationManager::class.java).cancel(NEWS_ID)
    }
}

/** The foreground service: it only keeps the process, and so [Listener], alive with the app closed. */
class ListenerService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val n = Notifier.serviceNotification(this)
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(Notifier.SERVICE_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_REMOTE_MESSAGING)
        } else {
            startForeground(Notifier.SERVICE_ID, n)
        }
        Listener.background = true
        Listener.update(this)
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        Listener.background = false
        Listener.update(this)
        super.onDestroy()
    }

    companion object {
        fun start(ctx: Context) {
            ContextCompat.startForegroundService(ctx, Intent(ctx, ListenerService::class.java))
        }

        fun stop(ctx: Context) {
            ctx.stopService(Intent(ctx, ListenerService::class.java))
        }
    }
}

/**
 * Unlock with fingerprint or face. The passphrase is kept encrypted under a key that lives in the
 * phone's hardware keystore and works only after a strong biometric check (and is dropped if the
 * enrolled fingerprints change). The passphrase itself still unlocks everything; this only saves typing it.
 */
object BioLock {
    private const val ALIAS = "spike_unlock_v1"
    private const val AUTH = BiometricManager.Authenticators.BIOMETRIC_STRONG

    private fun p(c: Context) = c.getSharedPreferences("spike_bio", Context.MODE_PRIVATE)

    fun available(c: Context): Boolean = BiometricManager.from(c).canAuthenticate(AUTH) == BiometricManager.BIOMETRIC_SUCCESS

    fun enabled(c: Context): Boolean = p(c).contains("blob")

    fun disable(c: Context) {
        p(c).edit().clear().apply()
        try {
            KeyStore.getInstance("AndroidKeyStore").apply { load(null) }.deleteEntry(ALIAS)
        } catch (_: Exception) {}
    }

    private fun key(): SecretKey {
        val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (ks.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val spec = KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .setUserAuthenticationRequired(true)
            .setInvalidatedByBiometricEnrollment(true)
            .build()
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply { init(spec) }.generateKey()
    }

    private fun prompt(a: FragmentActivity, title: String, cipher: Cipher, done: (Cipher?, String?) -> Unit) {
        val info = BiometricPrompt.PromptInfo.Builder()
            .setTitle(title)
            .setSubtitle("Spike Connect")
            .setNegativeButtonText("Use passphrase")
            .setAllowedAuthenticators(AUTH)
            .build()
        val p = BiometricPrompt(
            a, ContextCompat.getMainExecutor(a),
            object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                    done(result.cryptoObject?.cipher, null)
                }

                override fun onAuthenticationError(code: Int, msg: CharSequence) {
                    done(null, msg.toString())
                }
            },
        )
        p.authenticate(info, BiometricPrompt.CryptoObject(cipher))
    }

    /** Check the person's fingerprint or face once, then keep [passphrase] sealed under the hardware key. */
    fun enable(a: FragmentActivity, passphrase: String, done: (error: String?) -> Unit) {
        try {
            val cipher = Cipher.getInstance("AES/GCM/NoPadding").apply { init(Cipher.ENCRYPT_MODE, key()) }
            prompt(a, "Turn on fingerprint unlock", cipher) { c, err ->
                if (c == null) {
                    disable(a)
                    done(err ?: "cancelled")
                } else {
                    try {
                        val sealed = c.doFinal(passphrase.toByteArray())
                        p(a).edit()
                            .putString("iv", Base64.encodeToString(c.iv, Base64.NO_WRAP))
                            .putString("blob", Base64.encodeToString(sealed, Base64.NO_WRAP))
                            .apply()
                        done(null)
                    } catch (e: Exception) {
                        disable(a)
                        done(e.message ?: "could not save")
                    }
                }
            }
        } catch (e: Exception) {
            disable(a)
            done(e.message ?: "this phone cannot do that")
        }
    }

    /** Ask for the fingerprint or face and give back the passphrase. */
    fun unlock(a: FragmentActivity, done: (passphrase: String?, error: String?) -> Unit) {
        val iv = p(a).getString("iv", null)
        val blob = p(a).getString("blob", null)
        if (iv == null || blob == null) return done(null, "not set up")
        try {
            val cipher = Cipher.getInstance("AES/GCM/NoPadding").apply {
                init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, Base64.decode(iv, Base64.NO_WRAP)))
            }
            prompt(a, "Unlock Spike Connect", cipher) { c, err ->
                if (c == null) return@prompt done(null, err)
                try {
                    done(String(c.doFinal(Base64.decode(blob, Base64.NO_WRAP))), null)
                } catch (e: Exception) {
                    done(null, "could not read the saved passphrase")
                }
            }
        } catch (e: KeyPermanentlyInvalidatedException) {
            // The fingerprints on this phone changed: the saved passphrase is gone, by design.
            disable(a)
            done(null, "Your fingerprints changed, so type your passphrase and turn fingerprint unlock on again.")
        } catch (e: Exception) {
            done(null, e.message ?: "could not start")
        }
    }
}
