package com.carlink.voice

import android.os.Bundle
import android.service.voice.VoiceInteractionService
import android.service.voice.VoiceInteractionSession
import android.service.voice.VoiceInteractionSessionService
import com.carlink.CarlinkManager
import com.carlink.logging.logInfo
import com.carlink.logging.logWarn

private const val TAG = "VOICE"

/**
 * Tier 3 of the Siri trigger: the ONLY third-party route to the hardware push-to-talk button.
 *
 * On AAOS the steering-wheel PTT arrives as `KEYCODE_VOICE_ASSIST`, which `CarInputService`
 * intercepts before any activity or MediaSession can see it and hands to the platform's *current
 * assistant* via `AssistUtils.showSessionForActiveService(SHOW_SOURCE_PUSH_TO_TALK)`. A third-party
 * app may be that assistant: it declares a [VoiceInteractionService] guarded by
 * `android.permission.BIND_VOICE_INTERACTION` (a permission the SYSTEM holds — the app does not need
 * it) and the user selects it under Settings > Apps > Default apps > Digital assistant app. This is
 * opt-in by construction: until selected, none of these components are ever bound.
 *
 * When the session is shown we do not put up any UI; we forward the press to the phone as the
 * `CMD_SIRI_DOWN`/`CMD_SIRI_UP` hold pair and dismiss immediately — Siri's own UI is the projected
 * CarPlay screen.
 *
 * Costs the user accepts by opting in (documented in OCBMANDROID.md): while this app is the
 * assistant, the platform also routes `SpeechRecognizer` default-recognizer requests from OTHER apps
 * to [CarlinkRecognitionService], which refuses them. The manifest entry is required for the
 * assistant to be selectable at all (`VoiceInteractionServiceInfo` rejects a service without one).
 */
class CarlinkVoiceInteractionService : VoiceInteractionService() {
    override fun onReady() {
        super.onReady()
        logInfo("[VOICE] selected as the platform assistant — PTT will trigger Siri", tag = TAG)
    }

    override fun onShutdown() {
        logInfo("[VOICE] assistant role released", tag = TAG)
        super.onShutdown()
    }
}

/** Hosts [CarlinkVoiceSession]; one per PTT press. */
class CarlinkVoiceSessionService : VoiceInteractionSessionService() {
    override fun onNewSession(args: Bundle?): VoiceInteractionSession = CarlinkVoiceSession(this)
}

/**
 * The per-press session. No window (UI disabled in [onCreate]); `onShow` is the PTT edge.
 *
 * `showFlags` carries the source (`SHOW_SOURCE_PUSH_TO_TALK`, `SHOW_SOURCE_ASSIST_GESTURE`, …); every
 * source is treated the same — the user asked for the assistant, the assistant is Siri.
 */
class CarlinkVoiceSession(
    context: android.content.Context,
) : VoiceInteractionSession(context) {
    override fun onCreate() {
        super.onCreate()
        // No content view, no overlay window: the CarPlay surface IS Siri's UI. Without this the
        // platform would still create a TYPE_VOICE_INTERACTION window on every show.
        setUiEnabled(false)
    }

    override fun onShow(
        args: Bundle?,
        showFlags: Int,
    ) {
        super.onShow(args, showFlags)
        val m = CarlinkManager.live
        val sent = m?.requestSiri() ?: false
        if (sent) {
            logInfo("[VOICE] PTT (flags=0x%x) -> Siri hold pair sent".format(showFlags), tag = TAG)
        } else {
            logWarn("[VOICE] PTT (flags=0x%x) but no live CarPlay session — ignored".format(showFlags), tag = TAG)
        }
        // Nothing to show; release the session so the platform is not left thinking we are up.
        hide()
        finish()
    }
}
