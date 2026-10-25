package network.spike.connect

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * Where a call's sound goes (ADR 0028): the earpiece, the loudspeaker, a Bluetooth headset or a wired one. The person
 * picks with one button that steps through what is available right now; plugging in or connecting a headset moves the
 * sound there by itself (unless the person already chose), and unplugging falls back. Also: the screen switches off
 * against the face during a voice call on the earpiece (proximity sensor), and another call or app taking the audio puts
 * this one on hold until it ends.
 */
object CallAudio {
    enum class Route(val label: String) { Earpiece("Earpiece"), Speaker("Speaker"), Bluetooth("Bluetooth headset"), Headset("Wired headset") }

    /** The route in use, and the ones the button steps through. Compose state: the call screen reads these. */
    var route by mutableStateOf(Route.Earpiece)
        private set
    var available by mutableStateOf(listOf(Route.Earpiece, Route.Speaker))
        private set

    private var am: AudioManager? = null
    private var pm: PowerManager? = null
    private var focus: AudioFocusRequest? = null
    private var prox: PowerManager.WakeLock? = null
    private var onHold: ((Boolean) -> Unit)? = null
    private var chose = false
    private var connected = false
    private var video = false
    private val main = Handler(Looper.getMainLooper())

    private val devices = object : AudioDeviceCallback() {
        override fun onAudioDevicesAdded(added: Array<out AudioDeviceInfo>) = changed(true)
        override fun onAudioDevicesRemoved(removed: Array<out AudioDeviceInfo>) = changed(false)
    }

    private val focusListener = AudioManager.OnAudioFocusChangeListener { change ->
        when (change) {
            AudioManager.AUDIOFOCUS_LOSS, AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> onHold?.invoke(true)
            AudioManager.AUDIOFOCUS_GAIN -> onHold?.invoke(false)
            else -> {} // ducking: another app's short sound; the call carries on
        }
    }

    fun start(c: Context, withVideo: Boolean, hold: (Boolean) -> Unit) {
        val m = c.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        am = m
        pm = c.getSystemService(Context.POWER_SERVICE) as PowerManager
        onHold = hold
        video = withVideo
        connected = false
        chose = false
        m.mode = AudioManager.MODE_IN_COMMUNICATION
        try {
            val req = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
                .setAudioAttributes(AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION).setContentType(AudioAttributes.CONTENT_TYPE_SPEECH).build())
                .setOnAudioFocusChangeListener(focusListener, main).build()
            focus = req
            m.requestAudioFocus(req)
        } catch (_: Exception) {}
        try { m.registerAudioDeviceCallback(devices, main) } catch (_: Exception) {}
        refresh()
        apply(best())
    }

    fun stop() {
        val m = am ?: return
        try { m.unregisterAudioDeviceCallback(devices) } catch (_: Exception) {}
        release()
        try { focus?.let { m.abandonAudioFocusRequest(it) } } catch (_: Exception) {}
        focus = null
        try {
            if (Build.VERSION.SDK_INT >= 31) m.clearCommunicationDevice()
            else { m.stopBluetoothSco(); m.isBluetoothScoOn = false }
        } catch (_: Exception) {}
        m.isSpeakerphoneOn = false
        m.mode = AudioManager.MODE_NORMAL
        am = null; onHold = null
        route = Route.Earpiece
        available = listOf(Route.Earpiece, Route.Speaker)
    }

    /** The call is up: the screen may now switch off against the ear. */
    fun connected(isConnected: Boolean) { connected = isConnected; proximity() }

    /** Step to the next place the sound can go. */
    fun next() {
        val list = available
        if (list.size < 2) return
        chose = true
        apply(list[(list.indexOf(route) + 1) % list.size])
    }

    /** What is the best place for the sound when nothing was chosen: a headset, else the speaker for video, else the ear. */
    private fun best(): Route = when {
        Route.Bluetooth in available -> Route.Bluetooth
        Route.Headset in available -> Route.Headset
        video || Route.Earpiece !in available -> Route.Speaker
        else -> Route.Earpiece
    }

    private fun changed(added: Boolean) {
        if (am == null) return
        val before = available
        refresh()
        if (before == available) return
        Trace.add("audio: devices changed -> " + available.joinToString { it.label })
        // A headset that just arrived takes the sound unless the person picked something; one that left drops us back.
        if (route !in available) apply(best())
        else if (added && !chose) apply(best())
    }

    private fun refresh() {
        val m = am ?: return
        val types: List<Int> = if (Build.VERSION.SDK_INT >= 31) m.availableCommunicationDevices.map { it.type }
        else m.getDevices(AudioManager.GET_DEVICES_OUTPUTS).map { it.type }
        val out = mutableListOf(Route.Earpiece, Route.Speaker)
        // A phone without an earpiece (a tablet) lists no earpiece: then the speaker is the only built-in route.
        if (Build.VERSION.SDK_INT >= 31 && types.none { it == AudioDeviceInfo.TYPE_BUILTIN_EARPIECE }) out.remove(Route.Earpiece)
        if (types.any { it == AudioDeviceInfo.TYPE_BLUETOOTH_SCO || (Build.VERSION.SDK_INT >= 31 && it == AudioDeviceInfo.TYPE_BLE_HEADSET) }) out.add(Route.Bluetooth)
        if (types.any { it == AudioDeviceInfo.TYPE_WIRED_HEADSET || it == AudioDeviceInfo.TYPE_WIRED_HEADPHONES || it == AudioDeviceInfo.TYPE_USB_HEADSET }) out.add(Route.Headset)
        available = out
    }

    private fun apply(r: Route) {
        val m = am ?: return
        try {
            if (Build.VERSION.SDK_INT >= 31) {
                val want: (AudioDeviceInfo) -> Boolean = when (r) {
                    Route.Earpiece -> { d -> d.type == AudioDeviceInfo.TYPE_BUILTIN_EARPIECE }
                    Route.Speaker -> { d -> d.type == AudioDeviceInfo.TYPE_BUILTIN_SPEAKER }
                    Route.Bluetooth -> { d -> d.type == AudioDeviceInfo.TYPE_BLUETOOTH_SCO || d.type == AudioDeviceInfo.TYPE_BLE_HEADSET }
                    Route.Headset -> { d -> d.type == AudioDeviceInfo.TYPE_WIRED_HEADSET || d.type == AudioDeviceInfo.TYPE_WIRED_HEADPHONES || d.type == AudioDeviceInfo.TYPE_USB_HEADSET }
                }
                val dev = m.availableCommunicationDevices.firstOrNull(want)
                if (dev != null) m.setCommunicationDevice(dev) else m.clearCommunicationDevice()
            } else {
                when (r) {
                    Route.Speaker -> { m.stopBluetoothSco(); m.isBluetoothScoOn = false; m.isSpeakerphoneOn = true }
                    Route.Bluetooth -> { m.isSpeakerphoneOn = false; m.startBluetoothSco(); m.isBluetoothScoOn = true }
                    else -> { m.stopBluetoothSco(); m.isBluetoothScoOn = false; m.isSpeakerphoneOn = false } // the wired headset takes over by itself
                }
            }
        } catch (e: Exception) {
            Trace.add("audio: could not use ${r.label} (${e.javaClass.simpleName})")
        }
        route = r
        proximity()
    }

    /** Screen off against the ear: only a connected voice call on the earpiece. Video, speaker and headsets keep it on. */
    private fun proximity() {
        val want = connected && !video && route == Route.Earpiece
        if (!want) { release(); return }
        if (prox?.isHeld == true) return
        val p = pm ?: return
        try {
            if (!p.isWakeLockLevelSupported(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK)) return
            prox = p.newWakeLock(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK, "spike:proximity").also { it.setReferenceCounted(false); it.acquire(4 * 60 * 60 * 1000L) }
        } catch (_: Exception) {}
    }

    private fun release() {
        try { if (prox?.isHeld == true) prox?.release() } catch (_: Exception) {}
        prox = null
    }

    /** For tests: what is going on (read through the debug bridge). */
    fun describe(): String = "route=${route.name} available=${available.joinToString { it.name }} proximity=${prox?.isHeld == true}"
}
