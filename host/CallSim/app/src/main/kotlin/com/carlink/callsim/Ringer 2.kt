package com.carlink.callsim

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioManager
import android.media.Ringtone
import android.media.RingtoneManager
import android.os.VibrationEffect
import android.os.Vibrator
import android.provider.Settings

/** Plays the system ringtone: Telecom's Ringer skips self-managed calls (`endEarly` on isSelfManaged). */
class Ringer(private val ctx: Context) {
    private var ringtone: Ringtone? = null
    private var vibrating = false

    fun start() {
        if (ringtone != null) return
        val am = ctx.getSystemService(AudioManager::class.java)
        val uri = RingtoneManager.getActualDefaultRingtoneUri(ctx, RingtoneManager.TYPE_RINGTONE)
            ?: Settings.System.DEFAULT_RINGTONE_URI
        if (am.ringerMode == AudioManager.RINGER_MODE_NORMAL) {
            ringtone = RingtoneManager.getRingtone(ctx, uri)?.apply {
                audioAttributes = AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                    .build()
                isLooping = true
                try { play() } catch (e: Exception) { L.e("ringtone play failed", e) }
            }
            L.i("ringtone start uri=$uri playing=${ringtone?.isPlaying}")
        } else {
            L.i("ringtone skipped: ringerMode=${am.ringerMode} (0=silent 1=vibrate)")
        }
        if (am.ringerMode != AudioManager.RINGER_MODE_SILENT) {
            val v = ctx.getSystemService(Vibrator::class.java)
            if (v.hasVibrator()) {
                v.vibrate(VibrationEffect.createWaveform(longArrayOf(0, 700, 1300), 0))
                vibrating = true
            }
        }
    }

    fun stop() {
        ringtone?.let {
            it.stop()
            L.i("ringtone stop")
        }
        ringtone = null
        if (vibrating) {
            ctx.getSystemService(Vibrator::class.java).cancel()
            vibrating = false
        }
    }
}
