package com.carlink.voice

import android.content.Intent
import android.speech.RecognitionService
import android.speech.SpeechRecognizer
import com.carlink.logging.logWarn

/**
 * Required companion of [CarlinkVoiceInteractionService]: `VoiceInteractionServiceInfo` refuses an
 * assistant without a `recognitionService`, and selecting the assistant makes this the platform's
 * default recognizer. There is no on-head-unit recogniser — Siri runs on the phone — so every
 * request is refused promptly with `ERROR_CLIENT` rather than left hanging. Callers that use the
 * default `SpeechRecognizer` will see that error for as long as this app is the selected assistant;
 * the opt-in text says so.
 */
class CarlinkRecognitionService : RecognitionService() {
    override fun onStartListening(
        recognizerIntent: Intent?,
        listener: Callback,
    ) {
        logWarn("[VOICE] SpeechRecognizer request refused — recognition happens on the phone", tag = "VOICE")
        runCatching { listener.error(SpeechRecognizer.ERROR_CLIENT) }
    }

    override fun onCancel(listener: Callback) {
        runCatching { listener.error(SpeechRecognizer.ERROR_CLIENT) }
    }

    override fun onStopListening(listener: Callback) {
        runCatching { listener.error(SpeechRecognizer.ERROR_CLIENT) }
    }
}
