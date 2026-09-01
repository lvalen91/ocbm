package com.carlink.audio

import java.util.concurrent.atomic.AtomicLong

/**
 * Lock-free ring buffer for audio jitter compensation.
 *
 * PURPOSE:
 * Decouples producer from consumer to absorb irregular packet arrival.
 * USB capture analysis shows P99 jitter of ~7ms with max 30ms, so buffer
 * provides significant safety margin for consistent playback.
 *
 * THREAD SAFETY (monotonic-cursor design):
 * Single-producer / single-consumer (SPSC), used in two role configurations:
 * - Playback (DualStreamAudioManager): producer = USB ingest, consumer = AudioTrack writer.
 * - Microphone (MicrophoneCaptureManager): producer = AudioRecord reader, consumer = USB streamer.
 *
 * Cursors are MONOTONIC longs ([writeCount]/[readCount], indices derived via
 * `count % capacity`), with the read cursor advanced by compare-and-set from BOTH
 * sides: the reader on consume, the writer on overflow discard. This replaces the
 * previous volatile-index design, which had a documented SPSC violation: the writer's
 * overflow discard raced the reader's cursor store, and a stale reader update could
 * UNDO the discard — briefly replaying overwritten data. With CAS, a reader that
 * lost the race simply retries against the new cursor; discards can never be undone.
 * `clear()` is likewise a single atomic store (readCount = writeCount) instead of two
 * racy index resets.
 *
 * SAMPLE ALIGNMENT:
 * When discarding oldest data during overflow, bytes are aligned to PCM frame
 * boundaries (channels * 2 bytes for 16-bit audio) to prevent audio corruption.
 * See: https://developer.android.com/reference/android/media/AudioFormat
 *
 * BUFFER SIZING:
 * - Media stream: 200-300ms recommended (absorbs adapter jitter)
 * - Navigation stream: 100-150ms recommended (lower latency for prompts)
 *
 * @param capacityMs Buffer capacity in milliseconds
 * @param sampleRate Audio sample rate (e.g., 48000). [bytesPerMs] is computed via
 *   integer division — exact for 48000/16000/8000 Hz, but loses sub-byte precision
 *   for 44100 Hz (~0.2% under-allocation). The app currently pins the adapter to
 *   48kHz at init, so this is not exercised today.
 * @param channels Number of audio channels (1=mono, 2=stereo)
 */
class AudioRingBuffer(
    private val capacityMs: Int,
    private val sampleRate: Int,
    private val channels: Int,
) {
    // PCM frame size: 2 bytes per sample (16-bit) * number of channels
    // Stereo 16-bit = 4 bytes per frame, Mono 16-bit = 2 bytes per frame
    private val bytesPerFrame = channels * 2
    private val bytesPerMs = (sampleRate * channels * 2) / 1000
    private val capacity = capacityMs * bytesPerMs
    private val buffer = ByteArray(capacity)

    // Monotonic cursors. Only the writer thread advances writeCount; readCount is
    // CAS-advanced by the reader (consume) and the writer (overflow discard).
    // Invariant: readCount <= writeCount <= readCount + capacity.
    // The full/empty sentinel of the old index design is gone — the buffer holds
    // exactly `capacity` bytes when full.
    private val writeCount = AtomicLong(0)
    private val readCount = AtomicLong(0)

    @Volatile var totalBytesWritten: Long = 0
        private set

    @Volatile var totalBytesRead: Long = 0
        private set

    @Volatile var overflowCount: Int = 0
        private set

    @Volatile var underflowCount: Int = 0
        private set

    @Volatile var discardedBytes: Long = 0
        private set

    /**
     * Write data to ring buffer (non-blocking, overwrite-oldest when full).
     *
     * @return Bytes written. Equals [length] in the normal case (oldest data is
     *   discarded as needed to make room). If [length] exceeds [capacity], only the
     *   FIRST [capacity] bytes (frame-aligned) are written and the tail is dropped.
     *   Realistic chunks (~30ms) are well under buffer capacity (>=100ms) so this
     *   edge does not occur in practice.
     */
    fun write(
        data: ByteArray,
        offset: Int = 0,
        length: Int = data.size - offset,
    ): Int {
        if (length <= 0 || capacity == 0) return 0
        // Pathological oversized chunk: cap at a frame-aligned capacity so the
        // discard math below can always make room. (The old design's discard could
        // push the read cursor PAST the write cursor here — a phantom discard that
        // exposed never-written bytes as readable.)
        val toWrite = minOf(length, (capacity / bytesPerFrame) * bytesPerFrame)

        // Overwrite-oldest: CAS the read cursor forward until there is room.
        // Frame-aligned so a partial-frame discard can't phase-shift the channels.
        // (toWrite <= capacity, so when the buffer is empty space >= toWrite —
        // the loop always terminates.)
        var haveRoom = false
        while (!haveRoom) {
            val r = readCount.get()
            val w = writeCount.get() // stable: only this thread advances it
            val space = capacity - (w - r).toInt()
            if (space >= toWrite) {
                haveRoom = true
            } else {
                val minDiscard = toWrite - space
                val aligned = ((minDiscard + bytesPerFrame - 1) / bytesPerFrame) * bytesPerFrame
                // Never discard more than is actually buffered. CAS failure means the
                // reader consumed concurrently — recompute; we may already have room.
                val toDiscard = minOf(aligned.toLong(), w - r)
                if (readCount.compareAndSet(r, r + toDiscard)) {
                    discardedBytes += toDiscard
                    overflowCount++
                }
            }
        }

        val w = writeCount.get()
        val writeIndex = (w % capacity).toInt()
        val firstChunk = minOf(toWrite, capacity - writeIndex)
        System.arraycopy(data, offset, buffer, writeIndex, firstChunk)
        if (toWrite > firstChunk) {
            System.arraycopy(data, offset + firstChunk, buffer, 0, toWrite - firstChunk)
        }

        writeCount.set(w + toWrite)
        totalBytesWritten += toWrite
        return toWrite
    }

    /**
     * Read data from ring buffer (non-blocking).
     * @return Bytes read (may be less than length if buffer empty)
     */
    fun read(
        out: ByteArray,
        offset: Int = 0,
        length: Int = out.size - offset,
    ): Int {
        while (true) {
            val r = readCount.get()
            val w = writeCount.get()
            val available = (w - r).toInt()
            if (available <= 0) {
                underflowCount++
                return 0
            }

            val toRead = minOf(length, available)
            val readIndex = (r % capacity).toInt()
            val firstChunk = minOf(toRead, capacity - readIndex)
            System.arraycopy(buffer, readIndex, out, offset, firstChunk)
            if (toRead > firstChunk) {
                System.arraycopy(buffer, 0, out, offset + firstChunk, toRead - firstChunk)
            }

            // Commit: if the writer discarded this region concurrently (overflow),
            // the CAS fails and we retry against fresh cursors — the possibly-torn
            // bytes we copied are simply thrown away, never returned.
            if (readCount.compareAndSet(r, r + toRead)) {
                totalBytesRead += toRead
                return toRead
            }
        }
    }

    fun availableForRead(): Int = (writeCount.get() - readCount.get()).toInt().coerceAtLeast(0)

    // No -1 sentinel: the monotonic-cursor design distinguishes full from empty
    // without sacrificing a slot.
    fun availableForWrite(): Int = capacity - availableForRead()

    fun fillLevel(): Float = if (capacity > 0) availableForRead().toFloat() / capacity else 0f

    // bytesPerMs > 0 guard is defensive — unreachable for any realistic audio
    // config (would require sampleRate * channels * 2 < 1000).
    fun fillLevelMs(): Int = if (bytesPerMs > 0) availableForRead() / bytesPerMs else 0

    /**
     * Discard everything currently buffered.
     *
     * CAS loop, not a plain store: an unconditional `readCount.set(writeCount.get())`
     * could move the read cursor BACKWARD if this thread were preempted between the
     * get and the set while traffic advanced past the captured value — breaking the
     * monotonicity that the whole design's safety argument rests on (replay of
     * consumed bytes, availableForRead > capacity, and a narrow ABA path returning
     * torn data). The CAS only ever moves the cursor FORWARD to the write cursor;
     * a lost race (reader consumed concurrently) simply retries. (The old two-index
     * reset was worse still — observable half-applied.)
     */
    fun clear() {
        while (true) {
            val r = readCount.get()
            val w = writeCount.get()
            if (w <= r || readCount.compareAndSet(r, w)) return
        }
    }

    fun getStats(): Map<String, Any> =
        mapOf(
            "capacityMs" to capacityMs,
            "capacityBytes" to capacity,
            "fillLevelMs" to fillLevelMs(),
            "fillPercent" to (fillLevel() * 100).toInt(),
            "totalBytesWritten" to totalBytesWritten,
            "totalBytesRead" to totalBytesRead,
            "overflowCount" to overflowCount,
            "underflowCount" to underflowCount,
            "discardedBytes" to discardedBytes,
            "sampleRate" to sampleRate,
            "channels" to channels,
        )
}
