package network.spike.connect

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.provider.Settings as AndroidSettings
import android.media.AudioManager
import android.media.Ringtone
import android.media.RingtoneManager
import android.util.Log
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import org.json.JSONObject
import org.webrtc.AudioTrack
import org.webrtc.Camera2Enumerator
import org.webrtc.CameraVideoCapturer
import org.webrtc.DefaultVideoDecoderFactory
import org.webrtc.DefaultVideoEncoderFactory
import org.webrtc.EglBase
import org.webrtc.SurfaceTextureHelper
import org.webrtc.SurfaceViewRenderer
import org.webrtc.VideoSource
import org.webrtc.VideoTrack
import org.webrtc.IceCandidate
import org.webrtc.MediaConstraints
import org.webrtc.PeerConnection
import org.webrtc.PeerConnectionFactory
import org.webrtc.SdpObserver
import org.webrtc.SessionDescription
import java.security.MessageDigest
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

/** What the call screen shows. */
data class CallUi(
    val name: String,
    val incoming: Boolean,
    /** calling, ringing, connecting, connected */
    val status: String,
    val connectedAt: Long = 0,
    val muted: Boolean = false,
    /** Another call or app has the audio (a phone call came in): we are paused until it ends. */
    val held: Boolean = false,
    /** The connection dropped and is being restored. */
    val reconnecting: Boolean = false,
    /** How the connection is doing, from packet loss and round-trip time: good, fair or poor. Empty until measured. */
    val quality: String = "",
    val code: String = "",
    val verified: Boolean = false,
    val video: Boolean = false,
    val camOff: Boolean = false,
)

/**
 * Voice calls (ADR 0028). The engine carries the signalling and decides who rings and who wins; this
 * object owns the media: it makes the WebRTC offer and answer, hands the text to the engine, and does
 * what the engine says (ring, connect, add the other side's candidates, end). Same network only for
 * now: no STUN or TURN server is configured, so only local addresses are used.
 */
object CallManager {
    private const val TAG = "SpikeCall"
    var ui by mutableStateOf<CallUi?>(null)
        private set

    /** The picture of the other person and of us, when the call has video. */
    var remoteVideo by mutableStateOf<VideoTrack?>(null)
        private set
    var localVideo by mutableStateOf<VideoTrack?>(null)
        private set
    val egl: EglBase by lazy { EglBase.create() }
    private var capturer: CameraVideoCapturer? = null
    private var videoSource: VideoSource? = null
    private var helper: SurfaceTextureHelper? = null

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private var loop: Job? = null
    private var stats: Job? = null
    private var factory: PeerConnectionFactory? = null
    private var pc: PeerConnection? = null
    private var track: AudioTrack? = null
    private var offerSdp: String? = null
    private var callId: String? = null
    private var ready: CompletableDeferred<Unit>? = null
    private var remoteSet = false
    private val pendingRemote = mutableListOf<String>()
    private val localIce = mutableListOf<String>()
    private var iceJob: Job? = null
    private var cfg: PeerConnection.RTCConfiguration? = null
    private var discJob: Job? = null
    private var giveUpJob: Job? = null
    private var restarted = false
    private var netCallback: android.net.ConnectivityManager.NetworkCallback? = null
    private var ringtone: Ringtone? = null
    private var appCtx: Context? = null

    /** Start waiting for the engine's call events. Safe to call again. */
    @Synchronized
    fun start(ctx: Context) {
        appCtx = ctx.applicationContext
        if (loop?.isActive == true) return
        loop = scope.launch {
            while (isActive) {
                try {
                    val events = Bridge.arr("call_wait", args("ms" to 15000))
                    for (i in 0 until events.length()) handle(events.getJSONObject(i))
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Exception) {
                    delay(1500) // locked, or the engine is busy
                }
            }
        }
    }

    @Synchronized
    fun stop() {
        loop?.cancel(); loop = null
        endLocal(null)
    }

    fun hasMic(ctx: Context) =
        ContextCompat.checkSelfPermission(ctx, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED

    private fun factory(): PeerConnectionFactory {
        factory?.let { return it }
        PeerConnectionFactory.initialize(PeerConnectionFactory.InitializationOptions.builder(appCtx).createInitializationOptions())
        return PeerConnectionFactory.builder()
            .setVideoEncoderFactory(DefaultVideoEncoderFactory(egl.eglBaseContext, true, true))
            .setVideoDecoderFactory(DefaultVideoDecoderFactory(egl.eglBaseContext))
            .createPeerConnectionFactory().also { factory = it }
    }

    private fun hasCamera(): Boolean {
        val c = appCtx ?: return false
        return ContextCompat.checkSelfPermission(c, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED
    }

    /** Open the front camera (or any camera) at 720p and add its track to the connection. */
    private fun addCamera(f: PeerConnectionFactory, p: PeerConnection) {
        val c = appCtx ?: return
        val en = Camera2Enumerator(c)
        val name = en.deviceNames.firstOrNull { en.isFrontFacing(it) } ?: en.deviceNames.firstOrNull() ?: return
        val cap = en.createCapturer(name, null) ?: return
        val th = SurfaceTextureHelper.create("spike-capture", egl.eglBaseContext)
        val src = f.createVideoSource(false)
        cap.initialize(th, c, src.capturerObserver)
        cap.startCapture(1280, 720, 30)
        val t = f.createVideoTrack("video0", src)
        p.addTrack(t, listOf("spike"))
        capturer = cap; videoSource = src; helper = th
        localVideo = t
    }

    /** The relay operator's TURN server with a short-lived login, if the relay has one (ADR 0028). */
    private suspend fun iceServers(): List<PeerConnection.IceServer> = try {
        val a = Bridge.arr("call_ice_servers")
        (0 until a.length()).map { i ->
            val o = a.getJSONObject(i)
            val urls = o.getJSONArray("urls").let { u -> (0 until u.length()).map { u.getString(it) } }
            PeerConnection.IceServer.builder(urls).setUsername(o.getString("username")).setPassword(o.getString("credential")).createIceServer()
        }
    } catch (_: Exception) { emptyList() }

    private fun newPeer(video: Boolean = false, servers: List<PeerConnection.IceServer> = emptyList()): PeerConnection {
        val cfg = PeerConnection.RTCConfiguration(servers).apply {
            sdpSemantics = PeerConnection.SdpSemantics.UNIFIED_PLAN
            bundlePolicy = PeerConnection.BundlePolicy.MAXBUNDLE
            // "Always use the relay": nothing but TURN candidates, so no direct address is shared.
            if (servers.isNotEmpty() && appCtx?.let { Prefs.callsRelayOnly(it) } == true) iceTransportsType = PeerConnection.IceTransportsType.RELAY
        }
        remoteSet = false
        pendingRemote.clear(); localIce.clear()
        val f = factory()
        this.cfg = cfg
        val p = f.createPeerConnection(cfg, object : PeerConnection.Observer {
            override fun onIceCandidate(c: IceCandidate) {
                synchronized(localIce) { localIce.add(c.sdp) }
                iceJob?.cancel()
                iceJob = scope.launch { delay(50); flushIce() }
            }
            override fun onConnectionChange(s: PeerConnection.PeerConnectionState) {
                Log.i(TAG, "connection $s")
                when (s) {
                    PeerConnection.PeerConnectionState.CONNECTED -> {
                        discJob?.cancel(); giveUpJob?.cancel(); restarted = false
                        ui = ui?.copy(reconnecting = false)
                        telecom?.setActive()
                        // What was negotiated, without any content: media lines and directions only.
                        for ((n, d) in listOf("local" to pc?.localDescription, "remote" to pc?.remoteDescription)) {
                            val lines = (d?.description ?: "").lines().filter { it.startsWith("m=") || it.startsWith("a=sendrecv") || it.startsWith("a=recvonly") || it.startsWith("a=sendonly") || it.startsWith("a=inactive") }
                            Log.i(TAG, "sdp $n: " + lines.joinToString(" ") { it.take(24).trim() })
                        }
                        val u = ui ?: return
                        if (u.connectedAt == 0L) { ui = u.copy(status = "connected", connectedAt = System.currentTimeMillis()); startStats() }
                        CallAudio.connected(true)
                    }
                    // A dropped connection (a network change, a Wi-Fi hiccup) is restarted before the call is given up on.
                    PeerConnection.PeerConnectionState.DISCONNECTED -> {
                        if (ui?.connectedAt != 0L) {
                            ui = ui?.copy(reconnecting = true); reconnectAt = System.currentTimeMillis()
                            discJob?.cancel()
                            val mine = pc
                            discJob = scope.launch { delay(3000); if (pc === mine) restartIce() }
                        }
                    }
                    PeerConnection.PeerConnectionState.FAILED -> {
                        if (ui?.connectedAt != 0L && !restarted) restartIce() else scope.launch {
                            try { Bridge.call("call_fail") } catch (_: Exception) {}
                            endLocal("Call failed: the devices could not connect.")
                        }
                    }
                    else -> {}
                }
            }
            override fun onSignalingChange(s: PeerConnection.SignalingState) {}
            override fun onIceConnectionChange(s: PeerConnection.IceConnectionState) {}
            override fun onIceConnectionReceivingChange(b: Boolean) {}
            override fun onIceGatheringChange(s: PeerConnection.IceGatheringState) {}
            override fun onIceCandidatesRemoved(c: Array<out IceCandidate>) {}
            override fun onAddStream(s: org.webrtc.MediaStream) {}
            override fun onRemoveStream(s: org.webrtc.MediaStream) {}
            override fun onDataChannel(d: org.webrtc.DataChannel) {}
            override fun onRenegotiationNeeded() {}
            override fun onAddTrack(r: org.webrtc.RtpReceiver, s: Array<out org.webrtc.MediaStream>) {
                (r.track() as? VideoTrack)?.let { remoteVideo = it }
            }
        }) ?: throw BridgeError("could not start the call")
        val src = f.createAudioSource(MediaConstraints())
        track = f.createAudioTrack("audio0", src)
        p.addTrack(track, listOf("spike"))
        if (video && hasCamera()) addCamera(f, p)
        pc = p
        appCtx?.let { c -> CallAudio.start(c, video, { systemCall }) { h -> hold(h) } }
        return p
    }

    private suspend fun flushIce() {
        val batch = synchronized(localIce) { localIce.take(16).also { repeat(it.size) { localIce.removeAt(0) } } }
        if (batch.isEmpty()) return
        try {
            ready?.await()
            Bridge.call("call_ice", args("candidates" to batch))
        } catch (_: Exception) {}
        if (synchronized(localIce) { localIce.isNotEmpty() }) flushIce()
    }

    private suspend fun SdpOffer(p: PeerConnection): SessionDescription = suspendCancellableCoroutine { k ->
        p.createOffer(object : SdpObserver {
            override fun onCreateSuccess(d: SessionDescription) = k.resume(d)
            override fun onCreateFailure(e: String?) = k.resumeWithException(BridgeError(e ?: "no offer"))
            override fun onSetSuccess() {}
            override fun onSetFailure(e: String?) {}
        }, MediaConstraints().apply {
            mandatory.add(MediaConstraints.KeyValuePair("OfferToReceiveAudio", "true"))
            if (ui?.video == true) mandatory.add(MediaConstraints.KeyValuePair("OfferToReceiveVideo", "true"))
        })
    }

    private suspend fun SdpAnswer(p: PeerConnection): SessionDescription = suspendCancellableCoroutine { k ->
        p.createAnswer(object : SdpObserver {
            override fun onCreateSuccess(d: SessionDescription) = k.resume(d)
            override fun onCreateFailure(e: String?) = k.resumeWithException(BridgeError(e ?: "no answer"))
            override fun onSetSuccess() {}
            override fun onSetFailure(e: String?) {}
        }, MediaConstraints())
    }

    private suspend fun setLocal(p: PeerConnection, d: SessionDescription) = suspendCancellableCoroutine<Unit> { k ->
        p.setLocalDescription(object : SdpObserver {
            override fun onSetSuccess() = k.resume(Unit)
            override fun onSetFailure(e: String?) = k.resumeWithException(BridgeError(e ?: "local description"))
            override fun onCreateSuccess(d: SessionDescription?) {}
            override fun onCreateFailure(e: String?) {}
        }, d)
    }

    private suspend fun setRemote(p: PeerConnection, d: SessionDescription) = suspendCancellableCoroutine<Unit> { k ->
        p.setRemoteDescription(object : SdpObserver {
            override fun onSetSuccess() = k.resume(Unit)
            override fun onSetFailure(e: String?) = k.resumeWithException(BridgeError(e ?: "remote description"))
            override fun onCreateSuccess(d: SessionDescription?) {}
            override fun onCreateFailure(e: String?) {}
        }, d)
    }

    /** Place a voice call. The microphone permission must already be granted. */
    fun call(name: String, verified: Boolean, video: Boolean = false) {
        if (ui != null) return
        ui = CallUi(name, false, "calling", verified = verified, video = video)
        appCtx?.let { CallService.start(it, video); systemCall = SpikeTelecom.outgoing(it, name) }
        watchNetwork()
        scope.launch {
            try {
                val p = newPeer(video, iceServers())
                val offer = SdpOffer(p)
                setLocal(p, offer)
                val done = CompletableDeferred<Unit>().also { ready = it }
                try {
                    callId = Bridge.call("call_start", args("name" to name, "video" to video, "sdp" to offer.description)) as String
                    done.complete(Unit)
                } catch (e: Exception) { done.completeExceptionally(e); throw e }
            } catch (e: Exception) {
                endLocal(e.message ?: "could not start the call")
            }
        }
    }

    fun answer() {
        val u = ui ?: return
        val sdp = offerSdp ?: return
        if (!u.incoming || u.status != "ringing") return
        stopRing()
        ui = u.copy(status = "connecting")
        appCtx?.let { CallService.start(it, u.video) }
        telecom?.setActive()
        watchNetwork()
        scope.launch {
            try {
                val p = newPeer(u.video, iceServers())
                setRemote(p, SessionDescription(SessionDescription.Type.OFFER, sdp))
                remoteSet = true
                val a = SdpAnswer(p)
                setLocal(p, a)
                val done = CompletableDeferred<Unit>().also { ready = it }
                try { Bridge.call("call_accept", args("sdp" to a.description)); done.complete(Unit) }
                catch (e: Exception) { done.completeExceptionally(e); throw e }
                ui = ui?.copy(code = code())
            } catch (e: Exception) {
                try { Bridge.call("call_decline", args("all" to false)) } catch (_: Exception) {}
                endLocal(e.message ?: "could not answer")
            }
        }
    }

    fun decline() {
        if (ui == null) return
        scope.launch { try { Bridge.call("call_decline", args("all" to false)) } catch (_: Exception) {} }
        endLocal(null)
    }

    fun hangUp() {
        val u = ui ?: return
        scope.launch { try { Bridge.call("call_hangup") } catch (_: Exception) {} }
        endLocal(if (u.connectedAt != 0L) "Call ended" else null)
    }

    fun toggleMute() {
        val u = ui ?: return
        ui = u.copy(muted = !u.muted)
        micOn()
    }

    /** The microphone is open only when the person has not muted it and nothing else has the audio. */
    private fun micOn() {
        val u = ui ?: return
        track?.setEnabled(!u.muted && !u.held)
    }

    /** For tests: whether the microphone track is open. */
    fun micState(): String = track?.enabled()?.toString() ?: "none"

    /** A phone call (or another app that needs the audio) took over, or gave it back. */
    fun hold(on: Boolean) {
        val u = ui ?: return
        if (u.held == on) return
        Trace.add("call: " + if (on) "on hold, something else has the audio" else "off hold")
        ui = u.copy(held = on)
        micOn()
        if (on) telecom?.setOnHold() else telecom?.setActive()
    }

    /**
     * After a network change the connection is restarted on the same call (ADR 0028). Only the person who placed the
     * call makes the new offer; the other side asks for one. If it does not come back within 20 seconds the call ends.
     */
    fun restartIce() {
        val u = ui ?: return
        val p = pc ?: return
        if (u.connectedAt == 0L) return
        restarted = true
        ui = ui?.copy(reconnecting = true); reconnectAt = System.currentTimeMillis()
        giveUpJob?.cancel()
        giveUpJob = scope.launch {
            delay(20_000)
            if (pc === p && p.connectionState() != PeerConnection.PeerConnectionState.CONNECTED) {
                try { Bridge.call("call_fail") } catch (_: Exception) {}
                endLocal("Call lost: the connection could not be restored.")
            }
        }
        scope.launch {
            try {
                val servers = iceServers() // a fresh TURN login, in case the call has outlived the old one
                if (servers.isNotEmpty()) cfg?.let { it.iceServers = servers; p.setConfiguration(it) }
                if (u.incoming) { Bridge.call("call_restart_ask"); return@launch }
                p.restartIce()
                val offer = SdpOffer(p)
                setLocal(p, offer)
                Bridge.call("call_restart", args("sdp" to offer.description))
            } catch (e: Exception) {
                try { Bridge.call("call_fail") } catch (_: Exception) {}
                endLocal("Call lost: " + (e.message ?: "unknown"))
            }
        }
    }

    /** Restart when the phone's network changes (Wi-Fi to mobile data and back) during a call. */
    private fun watchNetwork() {
        val c = appCtx ?: return
        val cm = c.getSystemService(android.net.ConnectivityManager::class.java)
        var last: android.net.Network? = null
        val cb = object : android.net.ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: android.net.Network) {
                val changed = last != null && last != network
                last = network
                if (changed && (ui?.connectedAt ?: 0L) != 0L) { Trace.add("network changed during a call: restarting the connection"); restartIce() }
            }
        }
        try { cm.registerDefaultNetworkCallback(cb); netCallback = cb } catch (_: Exception) {}
    }

    private fun unwatchNetwork() {
        val c = appCtx ?: return
        try { netCallback?.let { c.getSystemService(android.net.ConnectivityManager::class.java).unregisterNetworkCallback(it) } } catch (_: Exception) {}
        netCallback = null
    }

    fun toggleCamera() {
        val u = ui ?: return
        localVideo?.setEnabled(u.camOff) // was off -> on
        ui = u.copy(camOff = !u.camOff)
    }

    fun switchCamera() { capturer?.switchCamera(null) }

    /** Step the sound through earpiece, speaker and any headset that is connected. */
    fun cycleAudio() { CallAudio.next() }

    private suspend fun handle(ev: JSONObject) {
        Trace.add("call event: ${ev.getString("type")}" + (if (ev.has("result")) " (${ev.getString("result")})" else "") + " screen=" + (appCtx?.getSystemService(android.os.PowerManager::class.java)?.isInteractive.let { if (it == true) "on" else "off" }))
        when (ev.getString("type")) {
            "ring" -> {
                if (ui != null) return
                val name = ev.getString("name")
                val verified = Bridge.obj("overview").getJSONArray("contacts").let { a ->
                    (0 until a.length()).map { a.getJSONObject(it) }.firstOrNull { it.getString("name") == name }?.optBoolean("verified") == true
                }
                appCtx?.let { c ->
                    ringedWhileLocked = !c.getSystemService(android.os.PowerManager::class.java).isInteractive ||
                        c.getSystemService(android.app.KeyguardManager::class.java).isKeyguardLocked
                }
                appCtx?.let { c ->
                    val pm = c.getSystemService(android.os.PowerManager::class.java)
                    if (!pm.isInteractive) {
                        // Turn the screen on, as a phone call does; the alert's full-screen intent then shows the call.
                        @Suppress("DEPRECATION")
                        pm.newWakeLock(android.os.PowerManager.SCREEN_BRIGHT_WAKE_LOCK or android.os.PowerManager.ACQUIRE_CAUSES_WAKEUP, "spike:ring").acquire(15_000)
                    }
                }
                // With "display over other apps" allowed, open the call window ourselves: it does not depend on how
                // this phone's maker treats full-screen alerts.
                appCtx?.let { c ->
                    if (android.provider.Settings.canDrawOverlays(c)) {
                        try { c.startActivity(Notifier.callWindow(c)); Trace.add("opened the call window directly") } catch (e: Exception) { Trace.add("could not open the call window: ${e.message?.take(50)}") }
                    }
                }
                callId = ev.getString("call"); offerSdp = ev.getString("sdp")
                ui = CallUi(name, true, "ringing", verified = verified, video = ev.optBoolean("video"))
                startRing()
                alerted = false
                val c = appCtx ?: return
                // Let the system know a call is coming in; it asks for our UI (showIncoming). If it will not take the
                // call, or says nothing, we show the alert ourselves.
                systemCall = SpikeTelecom.incoming(c, name)
                if (systemCall) {
                    scope.launch { delay(1500); if (!alerted && ui?.status == "ringing") showIncoming(name) }
                } else showIncoming(name)
                if (ev.optBoolean("glare")) answer()
            }
            "stop_ringing" -> if (ev.getString("call") == callId) stopRing()
            "connect" -> {
                if (ev.getString("call") != callId) return
                val p = pc ?: return
                try {
                    setRemote(p, SessionDescription(SessionDescription.Type.ANSWER, ev.getString("sdp")))
                    remoteSet = true
                    ui = ui?.copy(status = "connecting", code = code())
                    for (c in pendingRemote.toList()) addRemote(c)
                    pendingRemote.clear()
                } catch (e: Exception) {
                    try { Bridge.call("call_fail") } catch (_: Exception) {}
                    endLocal("The other device's answer could not be used.")
                }
            }
            "restart_ask" -> if (ev.getString("call") == callId && ui?.incoming == false) restartIce()
            "restart" -> {
                if (ev.getString("call") != callId) return
                val p = pc ?: return
                try {
                    setRemote(p, SessionDescription(SessionDescription.Type.OFFER, ev.getString("sdp")))
                    val a = SdpAnswer(p)
                    setLocal(p, a)
                    Bridge.call("call_restart_answer", args("sdp" to a.description))
                } catch (e: Exception) {
                    try { Bridge.call("call_fail") } catch (_: Exception) {}
                    endLocal("Call lost: " + (e.message ?: "unknown"))
                }
            }
            "restart_answer" -> {
                if (ev.getString("call") != callId) return
                val p = pc ?: return
                try { setRemote(p, SessionDescription(SessionDescription.Type.ANSWER, ev.getString("sdp"))) }
                catch (e: Exception) {
                    try { Bridge.call("call_fail") } catch (_: Exception) {}
                    endLocal("Call lost: " + (e.message ?: "unknown"))
                }
            }
            "remote_ice" -> {
                if (ev.getString("call") != callId) return
                val cs = ev.getJSONArray("candidates")
                for (i in 0 until cs.length()) if (remoteSet) addRemote(cs.getString(i)) else pendingRemote.add(cs.getString(i))
            }
            "ended" -> {
                val u = ui
                if (u == null) {
                    if (ev.getBoolean("incoming") && ev.getString("result") == "missed") scopeToast("Missed call from ${ev.optString("name", "someone")}.")
                    return
                }
                if (callId != null && callId != ev.getString("call")) return
                val who = ev.optString("name").ifEmpty { u.name }
                val secs = ev.optLong("seconds")
                val msg = when (ev.getString("result")) {
                    "completed" -> "Call ended (${secs / 60}:${(secs % 60).toString().padStart(2, '0')})."
                    "missed" -> "Missed call from $who."
                    "declined" -> "$who declined the call."
                    "no_answer" -> "$who did not answer."
                    "busy" -> "$who is busy."
                    "answered_elsewhere" -> "Answered on another of your devices."
                    else -> "The call failed."
                }
                endLocal(msg)
            }
        }
    }

    private fun addRemote(c: String) {
        pc?.addIceCandidate(IceCandidate("0", 0, c))
    }

    /** A message for the person, shown by whoever is listening (the call screen's owner). */
    /** The call came in while the phone was locked or its screen was off: the app may only be shown for the call. */
    @Volatile var ringedWhileLocked = false

    /** The person pressed Answer on the notification: answer as soon as the call screen is up. */
    @Volatile var pendingAnswer = false

    /** The call as the phone's own call app sees it (Telecom, see Telecom.kt). */
    @Volatile private var telecom: SpikeConnection? = null
    @Volatile private var alerted = false
    /**
     * The system's calling framework took this call (from the moment we hand it over, before it attaches). It then
     * holds and resumes the call itself when another call needs the audio, and it takes the audio focus for the call:
     * that focus change is not "something else has the audio" (it put our own new call on hold, ADR 0028).
     */
    @Volatile private var systemCall = false

    fun attach(c: SpikeConnection) { telecom = c; systemCall = true }

    /** Called when the system asks for the incoming-call UI (or when it refused the call and we show it ourselves). */
    fun showIncoming(name: String) {
        val c = appCtx ?: return
        alerted = true
        Notifier.incomingCall(c, name)
        Trace.add("incoming call alert posted; channel importance=" + (c.getSystemService(android.app.NotificationManager::class.java).getNotificationChannel("calls_v2")?.importance ?: -1) + " (4=high), lock-screen allowed=" + (if (android.os.Build.VERSION.SDK_INT >= 34) c.getSystemService(android.app.NotificationManager::class.java).canUseFullScreenIntent().toString() else "n/a") + ", battery-exempt=" + c.getSystemService(android.os.PowerManager::class.java).isIgnoringBatteryOptimizations(c.packageName))
    }

    fun telecomRefused(name: String) { systemCall = false; if (!alerted) showIncoming(name) }

    /** The system would not place our call: it goes on without it, and audio focus is ours to watch. */
    fun telecomOutgoingRefused() { systemCall = false }

    /** Answered from outside the screen (a Bluetooth headset, a car). */
    fun answerFromSystem() {
        if (ui?.status == "ringing") { pendingAnswer = false; appCtx?.let { c -> try { c.startActivity(Notifier.callWindow(c).putExtra("answer_call", true)) } catch (_: Exception) { answer() } } ?: answer() }
    }
    var message by mutableStateOf<String?>(null)
    private fun scopeToast(m: String) { message = m }

    private fun endLocal(msg: String?) {
        unwatchNetwork()
        discJob?.cancel(); giveUpJob?.cancel(); restarted = false
        telecom?.let { try { it.finish(if (msg == null) android.telecom.DisconnectCause.LOCAL else android.telecom.DisconnectCause.REMOTE) } catch (_: Exception) {} }
        telecom = null
        systemCall = false
        stopRing()
        stats?.cancel(); stats = null
        iceJob?.cancel()
        try { track?.setEnabled(false) } catch (_: Exception) {}
        // Stopping the camera waits for its own thread: never do that on the main thread (it froze the app).
        val (cap, src, th) = Triple(capturer, videoSource, helper)
        capturer = null; videoSource = null; helper = null; remoteVideo = null; localVideo = null
        if (cap != null || src != null || th != null) scope.launch {
            try { cap?.stopCapture() } catch (_: Exception) {}
            try { cap?.dispose() } catch (_: Exception) {}
            try { src?.dispose() } catch (_: Exception) {}
            try { th?.dispose() } catch (_: Exception) {}
        }
        // Closing waits for WebRTC's own threads, which on a busy phone took over 5 s: on the main thread (a Hang up
        // tap) that froze the app ("isn't responding"). Close it in the background, like the camera.
        val old = pc
        if (old != null) scope.launch { try { old.close() } catch (_: Exception) {} }
        pc = null; track = null; callId = null; offerSdp = null; ready = null
        CallAudio.stop()
        appCtx?.let {
            Notifier.clearIncomingCall(it)
            CallService.stop(it)
        }
        ui = null
        if (msg != null) message = msg
    }

    private var vibrator: android.os.Vibrator? = null

    private fun startRing() {
        val c = appCtx ?: return
        try {
            vibrator = (c.getSystemService(Context.VIBRATOR_MANAGER_SERVICE) as android.os.VibratorManager).defaultVibrator.also {
                it.vibrate(android.os.VibrationEffect.createWaveform(longArrayOf(0, 900, 700), 0))
            }
        } catch (_: Exception) {}
        try {
            ringtone = RingtoneManager.getRingtone(c, RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE))?.also { it.play() }
        } catch (_: Exception) {}
    }

    private fun stopRing() {
        appCtx?.let { Notifier.clearIncomingCall(it) } // the alert goes as soon as it is answered, declined or over
        try { vibrator?.cancel() } catch (_: Exception) {}
        vibrator = null
        try { ringtone?.stop() } catch (_: Exception) {}
        ringtone = null
    }

    private fun fingerprint(sdp: String?): String =
        Regex("a=fingerprint:sha-256 ([0-9A-Fa-f:]+)").find(sdp ?: "")?.groupValues?.get(1)?.uppercase() ?: ""

    /** Four digits both people can read to each other, from the two DTLS certificate fingerprints. */
    private fun code(): String {
        val a = fingerprint(pc?.localDescription?.description)
        val b = fingerprint(pc?.remoteDescription?.description)
        if (a.isEmpty() || b.isEmpty()) return ""
        val d = MessageDigest.getInstance("SHA-256").digest(listOf(a, b).sorted().joinToString("|").toByteArray())
        val v = ((d[0].toLong() and 0xff) shl 24) or ((d[1].toLong() and 0xff) shl 16) or ((d[2].toLong() and 0xff) shl 8) or (d[3].toLong() and 0xff)
        return (v % 10000).toString().padStart(4, '0')
    }

    /** Debug aid and test evidence: packet counts, never content. */
    private var reconnectAt = 0L

    private fun startStats() {
        stats?.cancel()
        var prevRx = 0L; var prevLost = 0L; var stall = 0
        stats = scope.launch {
            while (isActive) {
                pc?.getStats { report ->
                    var rx = 0L; var energy = 0.0; var tx = 0L; var local = ""; var vrx = 0L; var lost = 0L; var rtt = -1.0
                    for (s in report.statsMap.values) {
                        if (s.type == "candidate-pair" && s.members["state"] == "succeeded" && (s.members["nominated"] == true || s.members["selected"] == true)) {
                            (s.members["currentRoundTripTime"] as? Number)?.let { rtt = it.toDouble() }
                        }
                        if (s.type == "inbound-rtp" && s.members["kind"] == "audio") {
                            rx = (s.members["packetsReceived"] as? Number)?.toLong() ?: 0
                            lost = (s.members["packetsLost"] as? Number)?.toLong() ?: 0
                            energy = (s.members["totalAudioEnergy"] as? Number)?.toDouble() ?: 0.0
                        }
                        if (s.type == "inbound-rtp" && s.members["kind"] == "video") vrx = (s.members["packetsReceived"] as? Number)?.toLong() ?: 0
                        if (s.type == "outbound-rtp" && s.members["kind"] == "audio") tx = (s.members["packetsSent"] as? Number)?.toLong() ?: 0
                        if (s.type == "local-candidate" && local.isEmpty()) local = s.members["candidateType"]?.toString() ?: ""
                    }
                    Log.i(TAG, "stats rx=$rx energy=$energy tx=$tx code=${ui?.code} vrx=$vrx")
                    // Loss over the last interval and the round trip, as a word the person can read.
                    val dRx = (rx - prevRx).coerceAtLeast(0); val dLost = (lost - prevLost).coerceAtLeast(0)
                    prevRx = rx; prevLost = lost
                    val loss = if (dRx + dLost > 0) dLost.toDouble() / (dRx + dLost) else 0.0
                    val q = if (loss > 0.10 || rtt > 0.6) "poor" else if (loss > 0.03 || rtt > 0.3) "fair" else "good"
                    // Sound arriving again ends "Reconnecting", whether or not the connection ever reported a drop.
                    // And no sound for four seconds is the start of it, even before the connection reports a drop.
                    stall = if (dRx > 0) 0 else stall + 1
                    if (stall >= 2 && ui?.reconnecting == false && ui?.connectedAt != 0L) { ui = ui?.copy(reconnecting = true); reconnectAt = System.currentTimeMillis() }
                    else if (ui?.reconnecting == true && dRx > 0 && System.currentTimeMillis() - reconnectAt > 1500) ui = ui?.copy(reconnecting = false)
                    ui?.let { if (it.quality != q && it.connectedAt != 0L) ui = it.copy(quality = q) }
                }
                delay(2000)
            }
        }
    }
}

private fun line(name: String, vararg paths: String): ImageVector {
    val b = ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
    for (p in paths) b.addPath(
        pathData = addPathNodes(p), fill = null, stroke = SolidColor(Color.Black), strokeLineWidth = 2f,
        strokeLineCap = StrokeCap.Round, strokeLineJoin = StrokeJoin.Round,
    )
    return b.build()
}

/** Call icons, in the same 2px round line style as the prototype's (Icons.kt). The prototype's Phone icon is a mobile device, so the handset is here. */
val IcHandset = line("Handset", "M22 16.9v3a2 2 0 0 1-2.2 2 19.8 19.8 0 0 1-8.6-3.1 19.5 19.5 0 0 1-6-6A19.8 19.8 0 0 1 2.1 4.2 2 2 0 0 1 4.1 2h3a2 2 0 0 1 2 1.7c.1 1 .4 1.9.7 2.8a2 2 0 0 1-.5 2.1L8.1 9.9a16 16 0 0 0 6 6l1.3-1.3a2 2 0 0 1 2.1-.4c.9.3 1.8.6 2.8.7a2 2 0 0 1 1.7 2z")
val IcMic = line("Mic", "M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3z", "M5 11a7 7 0 0 0 14 0", "M12 18v3")
val IcMicOff = line("MicOff", "M4 4l16 16", "M9 9v3a3 3 0 0 0 5 2.2", "M15 11V6a3 3 0 0 0-5.7-1.3", "M5 11a7 7 0 0 0 11.2 5.6", "M19 11a7 7 0 0 1-.7 3", "M12 18v3")
val IcSpeaker = line("Speaker", "M4 9v6h4l5 4V5L8 9H4z", "M16.5 9a4.5 4.5 0 0 1 0 6", "M19 6.5a8 8 0 0 1 0 11")
val IcBluetooth = line("Bluetooth", "M6.5 6.5l11 11L12 23V1l5.5 5.5-11 11")
val IcHeadset = line("Headset", "M4 14v-2a8 8 0 0 1 16 0v2", "M4 14h3v5H4z", "M17 14h3v5h-3z")
val IcEarpiece = line("Earpiece", "M4 9v6h4l5 4V5L8 9H4z")
val IcVideo = line("VideoCam", "M3 6h10a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2z", "M15 10l7-4v12l-7-4")
val IcVideoOff = line("VideoOff", "M4 4l16 16", "M13 6H3a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-1", "M15 10l7-4v12l-3-1.7")
val IcFlip = line("Flip", "M4 8a8 8 0 0 1 14-2l2 2", "M20 4v4h-4", "M20 16a8 8 0 0 1-14 2l-2-2", "M4 20v-4h4")

/** Ask for the microphone if needed, then run [then]. */
@Composable
fun rememberMicGate(camera: Boolean = false, then: () -> Unit): () -> Unit {
    val ctx = LocalContext.current
    val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { r ->
        if (r[Manifest.permission.RECORD_AUDIO] == true || CallManager.hasMic(ctx)) then()
        else CallManager.message = "Spike needs the microphone to make calls. Allow it in Settings and try again."
    }
    return {
        val need = buildList {
            if (!CallManager.hasMic(ctx)) add(Manifest.permission.RECORD_AUDIO)
            if (camera && ContextCompat.checkSelfPermission(ctx, Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) add(Manifest.permission.CAMERA)
        }
        if (need.isEmpty()) then() else ask.launch(need.toTypedArray())
    }
}

@Composable
fun CallScreen(u: CallUi) {
    var now by androidx.compose.runtime.remember { mutableStateOf(System.currentTimeMillis()) }
    LaunchedEffect(u.connectedAt) { while (u.connectedAt != 0L) { now = System.currentTimeMillis(); delay(1000) } }
    val answerGate = rememberMicGate(camera = u.video) { CallManager.answer() }
    LaunchedEffect(u.status) {
        if (u.status == "ringing" && CallManager.pendingAnswer) { CallManager.pendingAnswer = false; answerGate() }
    }
    val dark = Color(0xFF0E1418)
    val remote = CallManager.remoteVideo
    val showRemote = u.video && remote != null
    // A soft teal glow behind the call, as on the desktop.
    val glow = androidx.compose.ui.graphics.Brush.radialGradient(listOf(Color(0xFF17393E), dark), radius = 1400f, center = androidx.compose.ui.geometry.Offset(540f, 500f))
    Box(Modifier.fillMaxSize().background(glow)) {
        if (showRemote) VideoView(remote!!, mirror = false, Modifier.fillMaxSize())
        Column(Modifier.fillMaxSize().statusBarsPadding().navigationBarsPadding().padding(24.dp), horizontalAlignment = Alignment.CenterHorizontally) {
            if (showRemote) Spacer(Modifier.height(8.dp)) else {
                Spacer(Modifier.weight(1f))
                val (tbg, tfg) = C.tint(u.name, dark = true)
                Box(Modifier.size(176.dp).clip(CircleShape).background(Color(0x0D5CC2C9)), contentAlignment = Alignment.Center) {
                    Box(Modifier.size(152.dp).clip(CircleShape).background(Color(0x1A5CC2C9)), contentAlignment = Alignment.Center) {
                        Box(Modifier.size(128.dp).clip(CircleShape).background(tbg), contentAlignment = Alignment.Center) {
                            Text(u.name.take(2).uppercase(), color = tfg, fontSize = 44.sp, fontWeight = FontWeight.SemiBold)
                        }
                    }
                }
                Spacer(Modifier.height(12.dp))
            }
            Text(
                when (u.status) { "ringing" -> "${u.name} is calling"; "calling" -> "Calling ${u.name}"; else -> u.name },
                color = Color(0xFFE6EDF2), fontSize = 26.sp, fontFamily = Grotesk, fontWeight = FontWeight.SemiBold, textAlign = TextAlign.Center,
            )
            val sub = when (u.status) {
                "ringing" -> if (u.video) "Incoming video call" else "Incoming voice call"
                "calling" -> "Ringing on all of their devices"
                "connected" -> if (u.reconnecting) "Reconnecting…" else ((now - u.connectedAt) / 1000).let { "${it / 60}:${(it % 60).toString().padStart(2, '0')}" } + (if (u.held) "  ·  On hold" else "") + (when (u.quality) { "good" -> "  ·  Good connection"; "fair" -> "  ·  Fair connection"; "poor" -> "  ·  Poor connection"; else -> "" })
                else -> "Connecting"
            }
            Text(sub, color = if (u.reconnecting || u.quality == "poor") Color(0xFFF5DDB0) else Color(0xFFC3CED7), fontSize = 15.sp, modifier = Modifier.padding(top = 6.dp))
            Spacer(Modifier.height(16.dp))
            Text(
                if (u.verified) "${u.name} is a verified contact" else "${u.name} is not verified yet",
                color = if (u.verified) Color(0xFF6FCF97) else Color(0xFFF5DDB0), fontSize = 13.5.sp, textAlign = TextAlign.Center,
            )
            if (u.code.isNotEmpty()) Text(
                "Call code ${u.code}" + if (u.verified) "" else ": read it to each other; it must match",
                color = if (u.verified) Color(0xFFC3CED7) else Color(0xFFF5DDB0), fontSize = 13.5.sp, textAlign = TextAlign.Center,
                modifier = Modifier.padding(top = 4.dp),
            )
            Text(
                "Encrypted between your devices; the keys are checked against the identity you verified",
                color = Color(0xFF9AA8B4), fontSize = 12.sp, textAlign = TextAlign.Center, modifier = Modifier.padding(top = 4.dp),
            )
            Spacer(Modifier.weight(1f))
            Row(
                Modifier.clip(RoundedCornerShape(40.dp)).background(Color(0x0FFFFFFF)).padding(horizontal = 12.dp, vertical = 10.dp),
                horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically,
            ) {
                if (u.status == "ringing") {
                    RoundButton("Decline", Color(0xFFD9443A), IcHandset, rotate = true) { CallManager.decline() }
                    RoundButton("Answer", Color(0xFF1F9D57), IcHandset) { answerGate() }
                } else {
                    RoundButton(if (u.muted) "Unmute microphone" else "Mute microphone", if (u.muted) Color(0xFFE6EDF2) else Color(0x1FFFFFFF), if (u.muted) IcMicOff else IcMic, tint = if (u.muted) dark else Color.White) { CallManager.toggleMute() }
                    val outs = CallAudio.available
                    val cur = CallAudio.route
                    val nextOut = outs[(outs.indexOf(cur) + 1) % outs.size]
                    val loud = cur != CallAudio.Route.Earpiece
                    RoundButton(
                        if (outs.size < 2) "Sound: ${cur.label}" else "Sound: ${cur.label}. Tap for ${nextOut.label}",
                        if (loud) Color(0xFFE6EDF2) else Color(0x1FFFFFFF),
                        when (cur) { CallAudio.Route.Speaker -> IcSpeaker; CallAudio.Route.Bluetooth -> IcBluetooth; CallAudio.Route.Headset -> IcHeadset; else -> IcEarpiece },
                        tint = if (loud) dark else Color.White,
                    ) { CallManager.cycleAudio() }
                    if (u.video) {
                        RoundButton(if (u.camOff) "Turn camera on" else "Turn camera off", if (u.camOff) Color(0xFFE6EDF2) else Color(0x1FFFFFFF), if (u.camOff) IcVideoOff else IcVideo, tint = if (u.camOff) dark else Color.White) { CallManager.toggleCamera() }
                        RoundButton("Switch camera", Color(0x1FFFFFFF), IcFlip) { CallManager.switchCamera() }
                    }
                    RoundButton("Hang up", Color(0xFFD9443A), IcHandset, rotate = true) { CallManager.hangUp() }
                }
            }
            Spacer(Modifier.height(24.dp))
        }
        val mine = CallManager.localVideo
        if (u.video && !u.camOff && mine != null) {
            VideoView(mine, mirror = true, Modifier.align(Alignment.BottomEnd).navigationBarsPadding().padding(end = 12.dp, bottom = 120.dp).size(width = 100.dp, height = 140.dp).clip(androidx.compose.foundation.shape.RoundedCornerShape(12.dp)))
        }
    }
}

/** A WebRTC video surface that shows [track] while it is on screen. */
@Composable
private fun VideoView(track: VideoTrack, mirror: Boolean, modifier: Modifier) {
    androidx.compose.ui.viewinterop.AndroidView(
        modifier = modifier,
        factory = { c ->
            SurfaceViewRenderer(c).apply {
                init(CallManager.egl.eglBaseContext, null)
                setEnableHardwareScaler(true)
                setMirror(mirror)
                // Your own preview fills its small window; the other person's picture is shown whole, so a shared screen is not cropped.
                setScalingType(if (mirror) org.webrtc.RendererCommon.ScalingType.SCALE_ASPECT_FILL else org.webrtc.RendererCommon.ScalingType.SCALE_ASPECT_FIT)
                if (mirror) setZOrderMediaOverlay(true)
                track.addSink(this)
            }
        },
        onRelease = { v -> try { track.removeSink(v); v.release() } catch (_: Exception) {} },
    )
}

@Composable
private fun RoundButton(label: String, bg: Color, icon: ImageVector, tint: Color = Color.White, rotate: Boolean = false, onClick: () -> Unit) {
    Box(
        Modifier.size(56.dp).clip(CircleShape).background(bg).clickable(onClick = onClick).semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) { Ico(icon, tint, 28.dp, if (rotate) Modifier.rotate(135f) else Modifier) }
}

/** Runs while a call is on, so the microphone (and camera) keep working if the person leaves the app. */
class CallService : android.app.Service() {
    override fun onBind(intent: android.content.Intent?): android.os.IBinder? = null

    override fun onStartCommand(intent: android.content.Intent?, flags: Int, startId: Int): Int {
        val video = intent?.getBooleanExtra("video", false) == true
        val n = Notifier.callNotification(this, "Call in progress")
        if (android.os.Build.VERSION.SDK_INT >= 30) {
            var type = android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
            if (video) type = type or android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA
            startForeground(Notifier.CALL_SERVICE_ID, n, type)
        } else {
            startForeground(Notifier.CALL_SERVICE_ID, n)
        }
        return START_NOT_STICKY
    }

    companion object {
        fun start(ctx: Context, video: Boolean) {
            try { ContextCompat.startForegroundService(ctx, android.content.Intent(ctx, CallService::class.java).putExtra("video", video)) } catch (_: Exception) {}
        }
        fun stop(ctx: Context) { ctx.stopService(android.content.Intent(ctx, CallService::class.java)) }
    }
}

/** Decline pressed on the incoming-call notification. */
class CallActionReceiver : android.content.BroadcastReceiver() {
    override fun onReceive(context: Context, intent: android.content.Intent) {
        if (intent.action == "decline") CallManager.decline()
    }
}

/**
 * Shows an incoming or running call, and nothing else of the app, so it may light the screen and appear over the lock
 * screen (declared in the manifest). It closes itself when the call ends, so the app is never left open on a locked phone.
 */
class CallActivity : androidx.activity.ComponentActivity() {
    override fun onCreate(savedInstanceState: android.os.Bundle?) {
        super.onCreate(savedInstanceState)
        setShowWhenLocked(true)
        setTurnScreenOn(true)
        window.addFlags(android.view.WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        enableEdgeToEdge()
        Bridge.ensureOpen(applicationContext)
        if (intent.getBooleanExtra("answer_call", false)) CallManager.pendingAnswer = true
        setContent {
            SpikeTheme {
                val u = CallManager.ui
                LaunchedEffect(u == null) { if (u == null) finish() }
                if (u != null) CallScreen(u)
            }
        }
    }

    override fun onNewIntent(intent: android.content.Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        if (intent.getBooleanExtra("answer_call", false)) CallManager.pendingAnswer = true
    }
}

/** Where a phone maker hides its "autostart" or "protected apps" switch. Tried in order; the first that exists is opened. */
private val AUTOSTART_SCREENS = listOf(
    "com.miui.securitycenter" to "com.miui.permcenter.autostart.AutoStartManagementActivity",
    "com.coloros.safecenter" to "com.coloros.safecenter.permission.startup.StartupAppListActivity",
    "com.oppo.safe" to "com.oppo.safe.permission.startup.StartupAppListActivity",
    "com.vivo.permissionmanager" to "com.vivo.permissionmanager.activity.BgStartUpManagerActivity",
    "com.iqoo.secure" to "com.iqoo.secure.ui.phoneoptimize.AddWhiteListActivity",
    "com.huawei.systemmanager" to "com.huawei.systemmanager.startupmgr.ui.StartupNormalAppListActivity",
    "com.huawei.systemmanager" to "com.huawei.systemmanager.optimize.process.ProtectActivity",
    "com.oneplus.security" to "com.oneplus.security.chainlaunch.view.ChainLaunchAppListActivity",
    "com.samsung.android.lool" to "com.samsung.android.sm.battery.ui.BatteryActivity",
    "com.asus.mobilemanager" to "com.asus.mobilemanager.autostart.AutoStartActivity",
)

private fun openAutostart(ctx: Context) {
    val pm = ctx.packageManager
    for ((pkg, cls) in AUTOSTART_SCREENS) {
        val i = android.content.Intent().setClassName(pkg, cls).addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK)
        if (i.resolveActivity(pm) != null) {
            try { ctx.startActivity(i); return } catch (_: Exception) {}
        }
    }
    ctx.startActivity(
        android.content.Intent(android.provider.Settings.ACTION_APPLICATION_DETAILS_SETTINGS, android.net.Uri.parse("package:" + ctx.packageName))
            .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK),
    )
}

private fun settingsIntent(action: String, ctx: Context, vararg extras: Pair<String, String>, uri: Boolean = false): android.content.Intent {
    val i = if (uri) android.content.Intent(action, android.net.Uri.parse("package:" + ctx.packageName)) else android.content.Intent(action)
    for ((k, v) in extras) i.putExtra(k, v)
    return i.addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK)
}

/**
 * Everything a phone needs switched on for a call to reach it with the screen off. Phone makers differ (some also
 * keep a private "autostart" list that kills apps in the background), so each item says whether it is on and opens
 * the right settings page. Re-checked every time the person comes back to this screen.
 */
@Composable
fun CallSetup() {
    val ctx = LocalContext.current
    var tick by androidx.compose.runtime.remember { mutableStateOf(0) }
    val owner = androidx.lifecycle.compose.LocalLifecycleOwner.current
    androidx.compose.runtime.DisposableEffect(owner) {
        val o = androidx.lifecycle.LifecycleEventObserver { _, e -> if (e == androidx.lifecycle.Lifecycle.Event.ON_RESUME) tick++ }
        owner.lifecycle.addObserver(o)
        onDispose { owner.lifecycle.removeObserver(o) }
    }
    Notifier.ensureCallChannel(ctx)
    val nm = ctx.getSystemService(android.app.NotificationManager::class.java)
    val items = androidx.compose.runtime.remember(tick) {
        buildList<Triple<String, Boolean?, () -> Unit>> {
            add(Triple("Notifications allowed", nm.areNotificationsEnabled(), {
                ctx.startActivity(settingsIntent(AndroidSettings.ACTION_APP_NOTIFICATION_SETTINGS, ctx, AndroidSettings.EXTRA_APP_PACKAGE to ctx.packageName))
            }))
            add(Triple("Incoming-call alerts set to urgent", (nm.getNotificationChannel(Notifier.CALLS)?.importance ?: 4) >= android.app.NotificationManager.IMPORTANCE_HIGH, {
                ctx.startActivity(settingsIntent(AndroidSettings.ACTION_CHANNEL_NOTIFICATION_SETTINGS, ctx, AndroidSettings.EXTRA_APP_PACKAGE to ctx.packageName, AndroidSettings.EXTRA_CHANNEL_ID to Notifier.CALLS))
            }))
            if (android.os.Build.VERSION.SDK_INT >= 34) add(Triple("Calls over the lock screen", nm.canUseFullScreenIntent(), {
                ctx.startActivity(settingsIntent(AndroidSettings.ACTION_MANAGE_APP_USE_FULL_SCREEN_INTENT, ctx, uri = true))
            }))
            add(Triple("Not put to sleep by the battery saver", ctx.getSystemService(android.os.PowerManager::class.java).isIgnoringBatteryOptimizations(ctx.packageName), {
                ctx.startActivity(settingsIntent(AndroidSettings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, ctx, uri = true))
            }))
            add(Triple("Display over other apps (opens the call at once)", AndroidSettings.canDrawOverlays(ctx), {
                ctx.startActivity(settingsIntent(AndroidSettings.ACTION_MANAGE_OVERLAY_PERMISSION, ctx, uri = true))
            }))
            add(Triple("Allowed to start by itself (" + android.os.Build.MANUFACTURER + ")", null, { openAutostart(ctx) }))
        }
    }
    var relayOnly by androidx.compose.runtime.remember { mutableStateOf(Prefs.callsRelayOnly(ctx)) }
    Text("Calls on this phone", fontWeight = FontWeight.Medium)
    Muted("A call can only reach a locked phone if each of these is on. Phone makers differ; tap an item to open its settings. The last one has no on/off check: look for Spike Connect in your phone's autostart or protected-apps list.", 12.5.sp)
    for ((label, ok, open) in items) {
        Row(Modifier.clickable { open() }.padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(
                when (ok) { true -> "On"; false -> "Off"; null -> "Check" },
                color = when (ok) { true -> Color(0xFF1F7A45); false -> Color(0xFFC0392B); null -> Color(0xFF7A4A00) },
                fontWeight = FontWeight.SemiBold, fontSize = 13.sp, modifier = Modifier.size(width = 48.dp, height = 20.dp),
            )
            Text(label, fontSize = 14.sp, modifier = Modifier.weight(1f))
            Ico(Ic.Chevron, size = 16.dp)
        }
    }
    Row(verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text("Always use the relay for calls", fontWeight = FontWeight.Medium)
            Muted("The other person then never sees this phone's own address; your relay's operator sees that a call happened and how much data it carried. Needs a TURN server on the relay; without one calls connect directly.", 12.5.sp)
        }
        androidx.compose.material3.Switch(relayOnly, { relayOnly = it; Prefs.setCallsRelayOnly(ctx, it) })
    }
}
