package com.carlink.callsim

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Environment
import android.os.Handler
import android.os.Looper
import java.io.BufferedOutputStream
import java.io.File
import java.io.FileOutputStream
import java.io.RandomAccessFile
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlin.math.log10
import kotlin.math.sqrt

/**
 * Records the call uplink (VOICE_COMMUNICATION source = whatever mic Telecom routed: HFP SCO
 * mic when the head unit owns the call) to a 16 kHz mono 16-bit WAV and logs RMS per second.
 */
class UplinkRecorder(private val ctx: Context) {
    companion object {
        const val RATE = 16000
        const val SILENCE_RMS = 20.0 // ~ -64 dBFS; below this for 3 s => flagged as silent
    }

    @Volatile private var running = false
    private var thread: Thread? = null
    var outputPath: String? = null
        private set

    fun start() {
        running = true
        thread = Thread({ run() }, "callsim-uplink").also { it.start() }
    }

    fun stop() {
        running = false
        thread?.join(3000)
        thread = null
    }

    private fun pickOutput(): File {
        val ts = SimpleDateFormat("yyyyMMdd_HHmmss", Locale.US).format(Date())
        val name = "callsim_uplink_$ts.wav"
        @Suppress("DEPRECATION")
        val dl = Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS)
        val primary = File(dl, name)
        return try {
            dl.mkdirs()
            if (primary.createNewFile()) primary else throw IllegalStateException("createNewFile false")
        } catch (e: Exception) {
            val fb = File(ctx.getExternalFilesDir(null), name)
            L.w("cannot create $primary ($e); falling back to $fb")
            fb
        }
    }

    private fun run() {
        if (ctx.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            L.e("uplink: RECORD_AUDIO not granted — adb shell pm grant com.carlink.callsim android.permission.RECORD_AUDIO")
            return
        }
        val minBuf = AudioRecord.getMinBufferSize(RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        val rec = try {
            AudioRecord.Builder()
                .setAudioSource(MediaRecorder.AudioSource.VOICE_COMMUNICATION)
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(RATE)
                        .setChannelMask(AudioFormat.CHANNEL_IN_MONO)
                        .build(),
                )
                .setBufferSizeInBytes(maxOf(minBuf * 2, RATE)) // >= 500 ms
                .build()
        } catch (e: Exception) {
            L.e("uplink: AudioRecord create failed", e)
            return
        }
        if (rec.state != AudioRecord.STATE_INITIALIZED) {
            L.e("uplink: AudioRecord not initialized state=${rec.state}")
            rec.release()
            return
        }
        rec.addOnRoutingChangedListener(
            { r -> L.i("uplink routed device -> ${AudioUtil.describe(r.routedDevice)}") },
            Handler(Looper.getMainLooper()),
        )
        val file = pickOutput()
        outputPath = file.absolutePath
        val out = BufferedOutputStream(FileOutputStream(file), 1 shl 16)
        var total = 0L
        try {
            out.write(wavHeader(0)) // patched on stop
            total = record(rec, out, file)
        } catch (e: Exception) {
            L.e("uplink recording error", e)
        } finally {
            try { rec.stop() } catch (_: IllegalStateException) {}
            rec.release()
            out.flush()
            out.close()
        }
        val dataBytes = total * 2
        RandomAccessFile(file, "rw").use { raf ->
            raf.seek(0)
            raf.write(wavHeader(dataBytes.toInt()))
        }
        L.i("uplink recording stopped: ${file.absolutePath} ${dataBytes + 44} bytes ${total / RATE} s")
    }

    /** Pump loop; returns total frames written. */
    private fun record(rec: AudioRecord, out: BufferedOutputStream, file: File): Long {
        rec.startRecording()
        if (rec.recordingState != AudioRecord.RECORDSTATE_RECORDING) {
            L.e("uplink: startRecording failed recordingState=${rec.recordingState} (mic blocked? app in background without FGS mic capability?)")
        }
        L.i("uplink recording start -> ${file.absolutePath} ($RATE Hz mono 16-bit) routed=${AudioUtil.describe(rec.routedDevice)}")

        val frames = RATE / 10 // 100 ms
        val buf = ShortArray(frames)
        val bytes = ByteBuffer.allocate(frames * 2).order(ByteOrder.LITTLE_ENDIAN)
        var total = 0L
        var sumSq = 0.0
        var count = 0L
        var peak = 0
        var silentSeconds = 0
        var second = 0
        var lastTick = System.currentTimeMillis()
        while (running) {
            val n = rec.read(buf, 0, frames)
            if (n < 0) { L.e("uplink: AudioRecord.read error $n"); break }
            if (n == 0) continue
            bytes.clear()
            for (i in 0 until n) {
                val s = buf[i].toInt()
                sumSq += (s * s).toDouble()
                val a = if (s < 0) -s else s
                if (a > peak) peak = a
                bytes.putShort(buf[i])
            }
            out.write(bytes.array(), 0, n * 2)
            total += n
            count += n
            val now = System.currentTimeMillis()
            if (now - lastTick >= 1000) {
                lastTick = now
                second++
                val rms = if (count > 0) sqrt(sumSq / count) else 0.0
                val dbfs = if (rms > 0) 20 * log10(rms / 32768.0) else -120.0
                val silent = rms < SILENCE_RMS
                silentSeconds = if (silent) silentSeconds + 1 else 0
                L.i(String.format(Locale.US, "uplink t=%ds rms=%.0f (%.1f dBFS) peak=%d%s", second, rms, dbfs, peak,
                    if (silentSeconds >= 3) "  ** SILENT UPLINK (${silentSeconds}s) **" else ""))
                sumSq = 0.0; count = 0; peak = 0
            }
        }
        return total
    }

    private fun wavHeader(dataBytes: Int): ByteArray {
        val bb = ByteBuffer.allocate(44).order(ByteOrder.LITTLE_ENDIAN)
        bb.put("RIFF".toByteArray()).putInt(36 + dataBytes).put("WAVE".toByteArray())
        bb.put("fmt ".toByteArray()).putInt(16).putShort(1).putShort(1).putInt(RATE).putInt(RATE * 2).putShort(2).putShort(16)
        bb.put("data".toByteArray()).putInt(dataBytes)
        return bb.array()
    }
}
