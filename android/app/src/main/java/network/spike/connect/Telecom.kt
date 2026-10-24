package network.spike.connect

import android.content.ComponentName
import android.content.Context
import android.net.Uri
import android.os.Bundle
import android.telecom.Connection
import android.telecom.ConnectionRequest
import android.telecom.ConnectionService
import android.telecom.DisconnectCause
import android.telecom.PhoneAccount
import android.telecom.PhoneAccountHandle
import android.telecom.TelecomManager

/**
 * Spike calls as the phone's own calls (ADR 0028), through Android's "self-managed" calling framework. The system then
 * knows a call is happening: it lets the call window start from the background, wakes the screen the way it does for
 * a phone call, routes a Bluetooth headset or a car, and stops two calls fighting over the microphone. It is the
 * same on every phone maker, which the alert-only approach is not. Our own call screen and notification are still
 * what the person sees (self-managed means the app draws its own UI). If the system refuses a call, the old
 * notification path is used instead.
 */
object SpikeTelecom {
    private const val ACCOUNT = "spike-calls"

    fun handle(ctx: Context) = PhoneAccountHandle(ComponentName(ctx, SpikeConnectionService::class.java), ACCOUNT)

    private fun ensureAccount(ctx: Context) {
        val tm = ctx.getSystemService(TelecomManager::class.java)
        tm.registerPhoneAccount(
            PhoneAccount.builder(handle(ctx), "Spike Connect").setCapabilities(PhoneAccount.CAPABILITY_SELF_MANAGED).build(),
        )
    }

    fun address(name: String): Uri = Uri.fromParts("spike", name, null)

    /** Tell the system a call is coming in. False if it could not be done (then the caller shows its own alert). */
    fun incoming(ctx: Context, name: String): Boolean = try {
        ensureAccount(ctx)
        val extras = Bundle().apply {
            putParcelable(TelecomManager.EXTRA_INCOMING_CALL_ADDRESS, address(name))
            putString("name", name)
        }
        ctx.getSystemService(TelecomManager::class.java).addNewIncomingCall(handle(ctx), extras)
        true
    } catch (e: Exception) {
        Trace.add("telecom: incoming call refused (${e.javaClass.simpleName})")
        false
    }

    /** Tell the system we are placing a call. Best effort: the call works without it. */
    fun outgoing(ctx: Context, name: String) {
        try {
            ensureAccount(ctx)
            val extras = Bundle().apply {
                putParcelable(TelecomManager.EXTRA_PHONE_ACCOUNT_HANDLE, handle(ctx))
                putString("name", name)
            }
            ctx.getSystemService(TelecomManager::class.java).placeCall(address(name), extras)
        } catch (e: Exception) {
            Trace.add("telecom: outgoing call not registered (${e.javaClass.simpleName})")
        }
    }
}

class SpikeConnectionService : ConnectionService() {
    override fun onCreateIncomingConnection(account: PhoneAccountHandle?, request: ConnectionRequest?): Connection {
        val name = request?.extras?.getString("name") ?: "Spike Connect"
        Trace.add("telecom: system accepted the incoming call")
        return SpikeConnection(name, true).also { CallManager.attach(it); it.setRinging() }
    }

    override fun onCreateIncomingConnectionFailed(account: PhoneAccountHandle?, request: ConnectionRequest?) {
        val name = request?.extras?.getString("name") ?: "Spike Connect"
        Trace.add("telecom: system refused the incoming call; using our own alert")
        CallManager.telecomRefused(name)
    }

    override fun onCreateOutgoingConnection(account: PhoneAccountHandle?, request: ConnectionRequest?): Connection {
        val name = request?.extras?.getString("name") ?: "Spike Connect"
        return SpikeConnection(name, false).also { CallManager.attach(it); it.setDialing() }
    }
}

class SpikeConnection(val name: String, val incoming: Boolean) : Connection() {
    init {
        connectionProperties = PROPERTY_SELF_MANAGED
        connectionCapabilities = CAPABILITY_MUTE
        audioModeIsVoip = true
        setAddress(SpikeTelecom.address(name), TelecomManager.PRESENTATION_ALLOWED)
        setCallerDisplayName(name, TelecomManager.PRESENTATION_ALLOWED)
    }

    /** The system asks the app to show its incoming-call UI: our call-style notification with the call window. */
    override fun onShowIncomingCallUi() {
        CallManager.showIncoming(name)
    }

    // Answered or rejected from outside the screen (a Bluetooth headset, a car, a watch).
    override fun onAnswer() { CallManager.answerFromSystem() }
    override fun onAnswer(videoState: Int) { CallManager.answerFromSystem() }
    override fun onReject() { CallManager.decline() }
    override fun onDisconnect() { CallManager.hangUp() }
    override fun onAbort() { CallManager.hangUp() }

    fun finish(cause: Int) {
        setDisconnected(DisconnectCause(cause))
        destroy()
    }
}
