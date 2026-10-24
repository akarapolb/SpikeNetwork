package network.spike.connect

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import kotlin.concurrent.thread

/** Debug builds only: run one bridge command and log its JSON answer under the tag SpikeDebug. */
class DebugReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val cmd = intent.getStringExtra("cmd") ?: return
        val args = intent.getStringExtra("args") ?: "{}"
        val tag = intent.getStringExtra("tag") ?: cmd
        if (cmd == "_clip") {
            // Put text on the clipboard, so a test can use the app's own Paste buttons.
            val cm = context.getSystemService(Context.CLIPBOARD_SERVICE) as android.content.ClipboardManager
            cm.setPrimaryClip(android.content.ClipData.newPlainText("test", args))
            Log.i("SpikeDebug", "$tag#0 {\"ok\":\"clipboard set\"}")
            Log.i("SpikeDebug", "$tag#end")
            return
        }
        if (cmd == "_bgunlock") {
            // Turn on "Keep receiving after a restart" with this passphrase (tests only).
            BackgroundUnlock.enable(context, args)
            Log.i("SpikeDebug", "$tag#0 {\"ok\":\"bg unlock on\"}")
            Log.i("SpikeDebug", "$tag#end")
            return
        }
        val pending = goAsync()
        thread {
            try {
                Bridge.ensureOpen(context.applicationContext)
                val answer = Native.call(handleOf(), cmd, args)
                // Logcat truncates long lines: write in chunks that a script can put back together.
                answer.chunked(3000).forEachIndexed { i, part -> Log.i("SpikeDebug", "$tag#$i $part") }
                Log.i("SpikeDebug", "$tag#end")
            } finally {
                pending.finish()
            }
        }
    }

    private fun handleOf(): Long {
        val f = Bridge::class.java.getDeclaredField("handle")
        f.isAccessible = true
        return f.getLong(Bridge)
    }
}
