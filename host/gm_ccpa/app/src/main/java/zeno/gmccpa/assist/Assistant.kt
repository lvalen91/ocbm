package zeno.gmccpa.assist

import android.app.role.RoleManager
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.provider.Settings
import android.service.voice.VoiceInteractionService
import android.service.voice.VoiceInteractionSession
import android.service.voice.VoiceInteractionSessionService
import android.speech.RecognitionService
import android.speech.SpeechRecognizer
import zeno.gmccpa.ProbeLog
import zeno.gmccpa.av.CarPlayEventTx
import zeno.gmccpa.pair.NativeCore
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Steering-wheel Voice button → Siri, by being the Android default assistant.
 *
 * The wheel button is not a KeyEvent an app can receive: GM's `com.gm.car.input` GMCarInputService
 * captures it (custom input 1002 → KEYCODE_VOICE_ASSIST) and, on a GAS build, a SHORT press that no
 * projection / BT call / OnStar call consumed ends in `AssistUtils.showSessionForActiveService(
 * {com.android.car.input.EXTRA_CAR_PUSH_TO_TALK=true}, 0x20, …)` — a VoiceInteractionSession in the
 * ACTIVE assistant. A long press never reaches the assistant. So holding android.app.role.ASSISTANT is
 * the only door; the role is not self-grantable (`requestable=false` in this firmware's roles.xml), it
 * is set over adb — docs/06_BRINGUP_RUNBOOK.md §4 "Steering-wheel Voice button".
 *
 * What is sent is one Siri-button tap, [NativeCore.siriTap]. No call-state branching here: iOS decides
 * what the Siri request means in its current state (owner direction). All lines are `[vis  ]`.
 */
object Assistant {
    const val EXTRA_CAR_PUSH_TO_TALK = "com.android.car.input.EXTRA_CAR_PUSH_TO_TALK"
    val log = ProbeLog.sub("vis")
    private val statusLogged = AtomicBoolean(false)

    /**
     * Once per process: does this app hold the ASSISTANT role, and what do the two settings the
     * platform derives from it say. A bench run shows at a glance whether the wheel can reach us.
     */
    fun logStatusOnce(ctx: Context, origin: String) {
        if (!statusLogged.compareAndSet(false, true)) return
        val held = runCatching {
            ctx.getSystemService(RoleManager::class.java)?.isRoleHeld(RoleManager.ROLE_ASSISTANT)
        }.fold({ it?.toString() ?: "no RoleManager" }, { "error ${it.javaClass.simpleName}" })
        // Read-only. If S+ settings-read rules refuse a key, the refusal itself is the answer to log.
        fun secure(key: String) = runCatching { Settings.Secure.getString(ctx.contentResolver, key) }
            .fold({ it ?: "<unset>" }, { "<unreadable: ${it.javaClass.simpleName}>" })
        log.i("ASSISTANT status ($origin): roleHeld=$held voice_interaction_service=${secure("voice_interaction_service")} " +
            "assistant=${secure("assistant")} (expect ${ctx.packageName}/${AssistantService::class.java.name})")
    }

    /** Hand one Siri tap to the event-channel thread (it blocks on the native mutex) and log the result. */
    fun forwardSiriTap(why: String) {
        val loaded = NativeCore.loaded
        log.i("forward Siri ($why): native core ${if (loaded) "loaded" else "NOT loaded"} — queueing on cp-event-tx")
        CarPlayEventTx.execute {
            val code = NativeCore.siriTap()
            val line = "Siri tap ($why) result=$code: ${AssistOutcome.describeTap(code)}"
            if (code == NativeCore.SiriTap.BOTH) log.i(line) else log.w(line)
        }
    }
}

/**
 * The always-bound half. Once this app holds the role the system keeps it bound (so the process
 * stays up); it does nothing but log and switch off the assist-data/screenshot fetch.
 */
class AssistantService : VoiceInteractionService() {
    override fun onCreate() {
        super.onCreate()
        Assistant.log.i("service created")
        Assistant.logStatusOnce(this, "service")
    }

    override fun onReady() {
        super.onReady()
        // showSessionForActiveService ORs in WITH_ASSIST|WITH_SCREENSHOT; without this the platform
        // would capture the foreground app's view structure and a screenshot for a session that
        // never reads them.
        setDisabledShowContext(VoiceInteractionSession.SHOW_WITH_ASSIST or VoiceInteractionSession.SHOW_WITH_SCREENSHOT)
        Assistant.log.i("service ready (connected; assist structure + screenshot disabled)")
    }

    override fun onShutdown() {
        Assistant.log.i("service shutdown (no longer the active assistant, or unbound)")
        super.onShutdown()
    }

    override fun onDestroy() {
        Assistant.log.i("service destroyed")
        super.onDestroy()
    }
}

class AssistantSessionService : VoiceInteractionSessionService() {
    override fun onNewSession(args: Bundle?): VoiceInteractionSession = AssistantSession(this)
}

/**
 * No UI. Forwards one Siri tap per show and dismisses immediately — the same shape as CarlinkAndroid's
 * `CarlinkVoiceSession` (setUiEnabled(false), send on onShow, hide()+finish()), emulator-proven there
 * with `cmd car_service inject-key 231` (docs/ops/06_CORRECTIONS_LEDGER.md R-ANDROID-4).
 */
class AssistantSession(ctx: Context) : VoiceInteractionSession(ctx) {
    override fun onCreate() {
        super.onCreate()
        setUiEnabled(false)
    }

    override fun onShow(args: Bundle?, showFlags: Int) {
        super.onShow(args, showFlags)
        val ptt = args?.takeIf { it.containsKey(Assistant.EXTRA_CAR_PUSH_TO_TALK) }
            ?.getBoolean(Assistant.EXTRA_CAR_PUSH_TO_TALK)?.toString() ?: "absent"
        Assistant.log.i("session onShow showFlags=${AssistOutcome.describeFlags(showFlags)} pushToTalk=$ptt")
        Assistant.forwardSiriTap("wheel/assist show")
        hide()
        finish()
    }

    override fun onHandleAssist(state: VoiceInteractionSession.AssistState) {
        // Expected on EVERY press on this firmware: GMCarInputService passes showFlags 0x20,
        // VoiceInteractionManagerService ORs in WITH_ASSIST|WITH_SCREENSHOT (0x1|0x2), and because
        // AssistantService disabled that context AssistDataRequester dispatches NULL assist data, i.e.
        // handleAssist(-1, null, …) lands here. Nothing to do; the tap was already sent from onShow.
        Assistant.log.i("session onHandleAssist (expected on every press: null assist data; nothing extra sent)")
    }

    override fun onHide() {
        Assistant.log.i("session onHide")
        super.onHide()
    }
}

/**
 * A stub that exists only because the platform requires `recognitionService` in the metadata (role
 * qualification in PermissionController AssistantRoleBehavior, and VoiceInteractionServiceInfo).
 * Deliberately NOT exported and with NO android.speech.RecognitionService intent-filter, so it never
 * enters the system speech-recognizer list (CarlinkAndroid's stub has both; this firmware's
 * VoiceInteractionManagerService never needs it, it only reads the name). Anything that does reach it
 * gets a client error.
 */
class AssistantRecognitionService : RecognitionService() {
    override fun onStartListening(recognizerIntent: Intent?, listener: RecognitionService.Callback?) {
        Assistant.log.w("recognition stub asked to listen — refused (gm_ccpa has no speech recognizer)")
        runCatching { listener?.error(SpeechRecognizer.ERROR_CLIENT) }
    }

    override fun onCancel(listener: RecognitionService.Callback?) {
        runCatching { listener?.error(SpeechRecognizer.ERROR_CLIENT) }
    }

    override fun onStopListening(listener: RecognitionService.Callback?) {
        runCatching { listener?.error(SpeechRecognizer.ERROR_CLIENT) }
    }
}
