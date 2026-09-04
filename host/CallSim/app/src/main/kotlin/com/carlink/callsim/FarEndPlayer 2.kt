package com.carlink.callsim

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.os.Handler
import android.os.Looper
import java.io.File
import java.io.InputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Loops a 16-bit PCM WAV into the call downlink: USAGE_VOICE_COMMUNICATION so it follows
 * Telecom's route (SCO when the HFP device owns the call, else earpiece/speaker).
 */
class FarEndPlayer(private val ctx: Context, private val wavPath: String?) {
    private class Pcm(val rate: Int, val channels: Int, val data: ByteArray)

    @Volatile private var running = false
    private var thread: Thread? = null

    fun start() {
        running = true
        thread = Thread({ run() }, "callsim-farend").also { it.start() }
    }

    fun stop() {
        running = false
        thread?.join(2000)
        thread = null
    }

    private fun run() {
        val src = wavPath ?: "assets/farend_16k.wav"
        val pcm = try {
            val stream: InputStream = if (wavPath != null) File(wavPath).inputStream() else ctx.assets.open("farend_16k.wav")
            stream.use { parseWav(it.readBytes()) }
        } catch (e: Exception) {
            L.e("far-end WAV load failed ($src): $e")
            return
        }
        val bytesPerSec = pcm.rate * pcm.channels * 2
        val mask = if (pcm.channels == 1) AudioFormat.CHANNEL_OUT_MONO else AudioFormat.CHANNEL_OUT_STEREO
        val minBuf = AudioTrack.getMinBufferSize(pcm.rate, mask, AudioFormat.ENCODING_PCM_16BIT)
        val track = try {
            AudioTrack.Builder()
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                        .build(),
                )
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(pcm.rate)
                        .setChannelMask(mask)
                        .build(),
                )
                .setBufferSizeInBytes(maxOf(minBuf, bytesPerSec / 5))
                .setTransferMode(AudioTrack.MODE_STREAM)
                .build()
        } catch (e: Exception) {
            L.e("AudioTrack create failed", e)
            return
        }
        if (track.state != AudioTrack.STATE_INITIALIZED) {
            L.e("AudioTrack not initialized state=${track.state}")
            track.release()
            return
        }
        val am = ctx.getSystemService(AudioManager::class.java)
        try {
        track.addOnRoutingChangedListener(
            { t -> L.i("far-end routed device -> ${AudioUtil.describe(t.routedDevice)}") },
            Handler(Looper.getMainLooper()),
        )
        track.play()
        L.i("far-end playback start src=$src ${pcm.rate} Hz ${pcm.channels}ch ${pcm.data.size / bytesPerSec} s " +
            "audioMode=${am.mode} commDevice=${AudioUtil.describe(am.communicationDevice)} " +
            "sessionId=${track.audioSessionId}")

        val chunk = bytesPerSec / 10 // 100 ms
        var off = 0
        var loops = 0
        var lastRouteLog = 0L
        while (running) {
            val n = minOf(chunk, pcm.data.size - off)
            val w = track.write(pcm.data, off, n) // blocking
            if (w < 0) { L.e("AudioTrack.write error $w"); break }
            off += w
            if (off >= pcm.data.size) {
                off = 0
                loops++
                L.i("far-end WAV looped ($loops)")
            }
            val now = System.currentTimeMillis()
            if (now - lastRouteLog > 5000) {
                lastRouteLog = now
                L.i("far-end playing: routed=${AudioUtil.describe(track.routedDevice)} " +
                    "pos=${off / bytesPerSec}s underruns=${track.underrunCount} audioMode=${am.mode}")
            }
        }
        } catch (e: Exception) {
            L.e("far-end playback error", e)
        } finally {
            try { track.stop() } catch (_: IllegalStateException) {}
            track.release()
        }
        L.i("far-end playback stopped")
    }

    /** Minimal RIFF/WAVE parser: PCM 16-bit only, finds the fmt and data chunks. */
    private fun parseWav(bytes: ByteArray): Pcm {
        val bb = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        require(bytes.size > 12 && String(bytes, 0, 4) == "RIFF" && String(bytes, 8, 4) == "WAVE") { "not a RIFF/WAVE file" }
        var pos = 12
        var rate = 0
        var channels = 0
        var bits = 0
        var format = 0
        while (pos + 8 <= bytes.size) {
            val id = String(bytes, pos, 4)
            val size = bb.getInt(pos + 4)
            val body = pos + 8
            when (id) {
                "fmt " -> {
                    format = bb.getShort(body).toInt()
                    channels = bb.getShort(body + 2).toInt()
                    rate = bb.getInt(body + 4)
                    bits = bb.getShort(body + 14).toInt()
                }
                "data" -> {
                    require(format == 1 && bits == 16) { "need PCM 16-bit, got format=$format bits=$bits" }
                    require(channels in 1..2) { "need mono/stereo, got $channels ch" }
                    val len = minOf(size, bytes.size - body)
                    return Pcm(rate, channels, bytes.copyOfRange(body, body + len))
                }
            }
            pos = body + size + (size and 1)
        }
        throw IllegalArgumentException("no data chunk")
    }
}
