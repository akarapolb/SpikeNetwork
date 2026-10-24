package network.spike.connect

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioManager
import android.media.Ringtone
import android.media.RingtoneManager
import android.util.Log
import androidx.activity.compose.rememberLauncherForActivityResult
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
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
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
    val speaker: Boolean = false,
    val code: String = "",
    val verified: Boolean = false,
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
        return PeerConnectionFactory.builder().createPeerConnectionFactory().also { factory = it }
    }

    private fun newPeer(): PeerConnection {
        val cfg = PeerConnection.RTCConfiguration(emptyList()).apply {
            sdpSemantics = PeerConnection.SdpSemantics.UNIFIED_PLAN
            bundlePolicy = PeerConnection.BundlePolicy.MAXBUNDLE
        }
        remoteSet = false
        pendingRemote.clear(); localIce.clear()
        val f = factory()
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
                        val u = ui ?: return
                        if (u.connectedAt == 0L) { ui = u.copy(status = "connected", connectedAt = System.currentTimeMillis()); startStats() }
                    }
                    PeerConnection.PeerConnectionState.FAILED -> scope.launch {
                        try { Bridge.call("call_fail") } catch (_: Exception) {}
                        endLocal("Call failed: the devices could not connect.")
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
            override fun onAddTrack(r: org.webrtc.RtpReceiver, s: Array<out org.webrtc.MediaStream>) {}
        }) ?: throw BridgeError("could not start the call")
        val src = f.createAudioSource(MediaConstraints())
        track = f.createAudioTrack("audio0", src)
        p.addTrack(track, listOf("spike"))
        pc = p
        appCtx?.let { c ->
            val am = c.getSystemService(Context.AUDIO_SERVICE) as AudioManager
            am.mode = AudioManager.MODE_IN_COMMUNICATION
            am.isSpeakerphoneOn = false
        }
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
        }, MediaConstraints().apply { mandatory.add(MediaConstraints.KeyValuePair("OfferToReceiveAudio", "true")) })
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
    fun call(name: String, verified: Boolean) {
        if (ui != null) return
        ui = CallUi(name, false, "calling", verified = verified)
        scope.launch {
            try {
                val p = newPeer()
                val offer = SdpOffer(p)
                setLocal(p, offer)
                val done = CompletableDeferred<Unit>().also { ready = it }
                try {
                    callId = Bridge.call("call_start", args("name" to name, "video" to false, "sdp" to offer.description)) as String
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
        scope.launch {
            try {
                val p = newPeer()
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
        track?.setEnabled(u.muted) // was muted -> enable
        ui = u.copy(muted = !u.muted)
    }

    fun toggleSpeaker() {
        val u = ui ?: return
        (appCtx?.getSystemService(Context.AUDIO_SERVICE) as? AudioManager)?.isSpeakerphoneOn = !u.speaker
        ui = u.copy(speaker = !u.speaker)
    }

    private suspend fun handle(ev: JSONObject) {
        when (ev.getString("type")) {
            "ring" -> {
                if (ui != null) return
                val name = ev.getString("name")
                val verified = Bridge.obj("overview").getJSONArray("contacts").let { a ->
                    (0 until a.length()).map { a.getJSONObject(it) }.firstOrNull { it.getString("name") == name }?.optBoolean("verified") == true
                }
                callId = ev.getString("call"); offerSdp = ev.getString("sdp")
                ui = CallUi(name, true, "ringing", verified = verified)
                startRing()
                Notifier.incomingCall(appCtx ?: return, name)
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
    var message by mutableStateOf<String?>(null)
    private fun scopeToast(m: String) { message = m }

    private fun endLocal(msg: String?) {
        stopRing()
        stats?.cancel(); stats = null
        iceJob?.cancel()
        try { track?.setEnabled(false) } catch (_: Exception) {}
        try { pc?.close() } catch (_: Exception) {}
        pc = null; track = null; callId = null; offerSdp = null; ready = null
        appCtx?.let {
            val am = it.getSystemService(Context.AUDIO_SERVICE) as AudioManager
            am.mode = AudioManager.MODE_NORMAL
            am.isSpeakerphoneOn = false
            Notifier.clearIncomingCall(it)
        }
        ui = null
        if (msg != null) message = msg
    }

    private fun startRing() {
        val c = appCtx ?: return
        try {
            ringtone = RingtoneManager.getRingtone(c, RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE))?.also { it.play() }
        } catch (_: Exception) {}
    }

    private fun stopRing() {
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
    private fun startStats() {
        stats?.cancel()
        stats = scope.launch {
            while (isActive) {
                pc?.getStats { report ->
                    var rx = 0L; var energy = 0.0; var tx = 0L; var local = ""
                    for (s in report.statsMap.values) {
                        if (s.type == "inbound-rtp" && s.members["kind"] == "audio") {
                            rx = (s.members["packetsReceived"] as? Number)?.toLong() ?: 0
                            energy = (s.members["totalAudioEnergy"] as? Number)?.toDouble() ?: 0.0
                        }
                        if (s.type == "outbound-rtp" && s.members["kind"] == "audio") tx = (s.members["packetsSent"] as? Number)?.toLong() ?: 0
                        if (s.type == "local-candidate" && local.isEmpty()) local = s.members["candidateType"]?.toString() ?: ""
                    }
                    Log.i(TAG, "stats rx=$rx energy=$energy tx=$tx code=${ui?.code}")
                }
                delay(2000)
            }
        }
    }
}

/** A handset icon (the prototype's Phone icon is a mobile device). */
val IcHandset: ImageVector = ImageVector.Builder("Handset", 24.dp, 24.dp, 24f, 24f).addPath(
    pathData = addPathNodes("M22 16.9v3a2 2 0 0 1-2.2 2 19.8 19.8 0 0 1-8.6-3.1 19.5 19.5 0 0 1-6-6A19.8 19.8 0 0 1 2.1 4.2 2 2 0 0 1 4.1 2h3a2 2 0 0 1 2 1.7c.1 1 .4 1.9.7 2.8a2 2 0 0 1-.5 2.1L8.1 9.9a16 16 0 0 0 6 6l1.3-1.3a2 2 0 0 1 2.1-.4c.9.3 1.8.6 2.8.7a2 2 0 0 1 1.7 2z"),
    fill = null, stroke = SolidColor(Color.Black), strokeLineWidth = 2f, strokeLineCap = StrokeCap.Round, strokeLineJoin = StrokeJoin.Round,
).build()

/** Ask for the microphone if needed, then run [then]. */
@Composable
fun rememberMicGate(then: () -> Unit): () -> Unit {
    val ctx = LocalContext.current
    val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) then() else CallManager.message = "Spike needs the microphone to make calls. Allow it in Settings and try again."
    }
    return { if (CallManager.hasMic(ctx)) then() else ask.launch(Manifest.permission.RECORD_AUDIO) }
}

@Composable
fun CallScreen(u: CallUi) {
    val ctx = LocalContext.current
    var now by androidx.compose.runtime.remember { mutableStateOf(System.currentTimeMillis()) }
    LaunchedEffect(u.connectedAt) { while (u.connectedAt != 0L) { now = System.currentTimeMillis(); delay(1000) } }
    val answerGate = rememberMicGate { CallManager.answer() }
    val dark = Color(0xFF0E1418)
    Box(Modifier.fillMaxSize().background(dark).statusBarsPadding().navigationBarsPadding()) {
        Column(Modifier.fillMaxSize().padding(24.dp), horizontalAlignment = Alignment.CenterHorizontally) {
            Spacer(Modifier.weight(1f))
            Box(Modifier.size(110.dp).clip(CircleShape).background(Color(0xFF24313A)), contentAlignment = Alignment.Center) {
                Text(u.name.take(2).uppercase(), color = Color(0xFFC3CED7), fontSize = 36.sp, fontWeight = FontWeight.SemiBold)
            }
            Spacer(Modifier.height(16.dp))
            Text(
                when (u.status) { "ringing" -> "${u.name} is calling"; "calling" -> "Calling ${u.name}"; else -> u.name },
                color = Color(0xFFE6EDF2), fontSize = 24.sp, fontWeight = FontWeight.SemiBold, textAlign = TextAlign.Center,
            )
            val sub = when (u.status) {
                "ringing" -> "Incoming voice call"
                "calling" -> "Ringing on all of their devices"
                "connected" -> ((now - u.connectedAt) / 1000).let { "${it / 60}:${(it % 60).toString().padStart(2, '0')}" }
                else -> "Connecting"
            }
            Text(sub, color = Color(0xFFC3CED7), fontSize = 15.sp, modifier = Modifier.padding(top = 6.dp))
            Spacer(Modifier.height(20.dp))
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
            Row(horizontalArrangement = Arrangement.spacedBy(20.dp), verticalAlignment = Alignment.CenterVertically) {
                if (u.status == "ringing") {
                    RoundButton("Decline", Color(0xFFC0392B), rotate = true) { CallManager.decline() }
                    RoundButton("Answer", Color(0xFF1F7A45)) { answerGate() }
                } else {
                    RoundButton(if (u.muted) "Unmute microphone" else "Mute microphone", if (u.muted) Color(0xFFE6EDF2) else Color(0xFF24313A), tint = if (u.muted) dark else Color.White, glyph = if (u.muted) "Mic off" else "Mic") { CallManager.toggleMute() }
                    RoundButton(if (u.speaker) "Speaker off" else "Speaker on", if (u.speaker) Color(0xFFE6EDF2) else Color(0xFF24313A), tint = if (u.speaker) dark else Color.White, glyph = "Spk") { CallManager.toggleSpeaker() }
                    RoundButton("Hang up", Color(0xFFC0392B), rotate = true) { CallManager.hangUp() }
                }
            }
            Spacer(Modifier.height(24.dp))
        }
    }
}

@Composable
private fun RoundButton(label: String, bg: Color, tint: Color = Color.White, rotate: Boolean = false, glyph: String? = null, onClick: () -> Unit) {
    Box(
        Modifier.size(64.dp).clip(CircleShape).background(bg).clickable(onClick = onClick).semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        if (glyph == null) Ico(IcHandset, tint, 28.dp, if (rotate) Modifier.rotate(135f) else Modifier)
        else Text(glyph, color = tint, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
    }
}
