package com.carlink.callsim

import android.content.Context
import android.media.AudioManager
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.telecom.CallAudioState
import android.telecom.CallEndpoint
import android.telecom.Connection
import android.telecom.ConnectionRequest
import android.telecom.DisconnectCause
import android.telecom.TelecomManager

class CallSimConnection(
    private val ctx: Context,
    request: ConnectionRequest,
    val incoming: Boolean,
) : Connection() {

    companion object {
        const val EXTRA_NAME = "name"
        const val EXTRA_WAV = "wav"
        const val AUTO_ANSWER_MS = 3000L

        fun routeName(route: Int): String = CallAudioState.audioRouteToString(route)

        fun endpointName(type: Int): String = when (type) {
            CallEndpoint.TYPE_EARPIECE -> "EARPIECE"
            CallEndpoint.TYPE_BLUETOOTH -> "BLUETOOTH"
            CallEndpoint.TYPE_WIRED_HEADSET -> "WIRED_HEADSET"
            CallEndpoint.TYPE_SPEAKER -> "SPEAKER"
            CallEndpoint.TYPE_STREAMING -> "STREAMING"
            else -> "UNKNOWN($type)"
        }
    }

    private val main = Handler(Looper.getMainLooper())
    val number: String = request.address?.schemeSpecificPart ?: "unknown"
    val name: String = request.extras?.getString(EXTRA_NAME) ?: number
    private val wavPath: String? = request.extras?.getString(EXTRA_WAV)

    private val ringer = Ringer(ctx)
    private var player: FarEndPlayer? = null
    private var recorder: UplinkRecorder? = null
    private var engineRunning = false

    init {
        connectionProperties = PROPERTY_SELF_MANAGED
        connectionCapabilities = CAPABILITY_HOLD or CAPABILITY_SUPPORT_HOLD or CAPABILITY_MUTE
        setAddress(request.address ?: Uri.fromParts("tel", number, null), TelecomManager.PRESENTATION_ALLOWED)
        setCallerDisplayName(name, TelecomManager.PRESENTATION_ALLOWED)
        // VoIP => Telecom puts AudioManager into MODE_IN_COMMUNICATION (not MODE_IN_CALL).
        audioModeIsVoip = true
        CallRegistry.current = this
        L.i("Connection created incoming=$incoming name=\"$name\" number=$number farEndWav=${wavPath ?: "<assets/farend_16k.wav>"}")
    }

    // ---- state -------------------------------------------------------------------------

    override fun onStateChanged(state: Int) {
        L.i("state -> ${stateToString(state)}")
        when (state) {
            STATE_ACTIVE -> { ringer.stop(); startEngine() }
            STATE_HOLDING -> stopEngine("hold")
            STATE_DISCONNECTED -> {
                ringer.stop()
                stopEngine("disconnected")
                if (CallRegistry.current === this) CallRegistry.current = null
                CallFgService.stop(ctx)
            }
        }
        if (state == STATE_RINGING || state == STATE_DIALING || state == STATE_ACTIVE || state == STATE_HOLDING) {
            CallFgService.update(ctx, this)
        }
    }

    override fun onShowIncomingCallUi() {
        val am = ctx.getSystemService(AudioManager::class.java)
        L.i("onShowIncomingCallUi (Telecom asks us to ring: self-managed calls are not rung by Telecom) ringerMode=${am.ringerMode}")
        ringer.start()
        CallFgService.update(ctx, this)
    }

    override fun onSilence() {
        L.i("onSilence (volume key / InCallService silenced ringer)")
        ringer.stop()
    }

    fun scheduleAutoAnswer() {
        main.postDelayed({
            if (state == STATE_DIALING) {
                L.i("outgoing: simulated far end answered after ${AUTO_ANSWER_MS} ms")
                setActive()
            }
        }, AUTO_ANSWER_MS)
    }

    /** Local answer: from the phone UI, the AA screen, our notification, or adb ANSWER. */
    fun answer(source: String) {
        L.i("answer from $source (state=${stateToString(state)})")
        if (state == STATE_RINGING) setActive() else L.w("answer ignored: not ringing")
    }

    fun rejectLocal(source: String) {
        L.i("reject from $source")
        ringer.stop()
        setDisconnected(DisconnectCause(DisconnectCause.REJECTED, "rejected via $source"))
        destroy()
    }

    fun hangupLocal(source: String) {
        L.i("local hangup from $source")
        setDisconnected(DisconnectCause(DisconnectCause.LOCAL, "hangup via $source"))
        destroy()
    }

    fun hangupRemote(source: String) {
        L.i("simulated far-end hangup from $source")
        setDisconnected(DisconnectCause(DisconnectCause.REMOTE, "far end hung up ($source)"))
        destroy()
    }

    // ---- Telecom -> us ---------------------------------------------------------------------

    override fun onAnswer(videoState: Int) { answer("Telecom/InCallService videoState=$videoState") }
    override fun onAnswer() { answer("Telecom/InCallService") }
    override fun onReject() { rejectLocal("Telecom/InCallService") }
    override fun onReject(rejectReason: Int) { rejectLocal("Telecom/InCallService reason=$rejectReason") }
    override fun onReject(replyMessage: String?) { rejectLocal("Telecom/InCallService message=$replyMessage") }
    override fun onDisconnect() { hangupLocal("Telecom/InCallService") }
    override fun onAbort() {
        L.i("onAbort")
        setDisconnected(DisconnectCause(DisconnectCause.CANCELED))
        destroy()
    }
    override fun onHold() { L.i("onHold"); setOnHold() }
    override fun onUnhold() { L.i("onUnhold"); setActive() }
    override fun onPlayDtmfTone(c: Char) { L.i("onPlayDtmfTone '$c'") }
    override fun onStopDtmfTone() { L.i("onStopDtmfTone") }

    @Suppress("DEPRECATION")
    override fun onCallAudioStateChanged(state: CallAudioState) {
        val bt = state.activeBluetoothDevice
        val btName = try { bt?.name } catch (e: SecurityException) { "<BLUETOOTH_CONNECT not granted>" }
        L.i("onCallAudioStateChanged route=${routeName(state.route)} supported=${routeName(state.supportedRouteMask)} " +
            "muted=${state.isMuted} activeBtDevice=${btName ?: "none"} audioMode=${ctx.getSystemService(AudioManager::class.java).mode}")
    }

    override fun onCallEndpointChanged(endpoint: CallEndpoint) {
        L.i("onCallEndpointChanged ${endpointName(endpoint.endpointType)} \"${endpoint.endpointName}\"")
    }

    override fun onAvailableCallEndpointsChanged(endpoints: List<CallEndpoint>) {
        L.i("onAvailableCallEndpointsChanged " + endpoints.joinToString { "${endpointName(it.endpointType)}:${it.endpointName}" })
    }

    override fun onMuteStateChanged(isMuted: Boolean) { L.i("onMuteStateChanged muted=$isMuted") }

    // ---- audio engine -----------------------------------------------------------------------

    private fun startEngine() {
        if (engineRunning) return
        engineRunning = true
        val am = ctx.getSystemService(AudioManager::class.java)
        L.i("engine start: audioMode=${am.mode} (3=MODE_IN_COMMUNICATION) sdk=${Build.VERSION.SDK_INT}")
        player = FarEndPlayer(ctx, wavPath).also { it.start() }
        recorder = UplinkRecorder(ctx).also { it.start() }
    }

    private fun stopEngine(why: String) {
        if (!engineRunning) return
        engineRunning = false
        L.i("engine stop ($why)")
        player?.stop(); player = null
        recorder?.stop(); recorder = null
    }
}
