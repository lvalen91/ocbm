package com.carlink.audio

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference

/**
 * Pure-JVM unit tests for [AudioRingBuffer] (no Android APIs touched, no Robolectric needed).
 *
 * Most tests use a tiny buffer for hand-verifiable math:
 *   sampleRate=1000, channels=2 -> bytesPerMs = 1000*2*2/1000 = 4, bytesPerFrame = 4.
 *   capacityMs=4 -> capacity = 16 bytes = 4 stereo frames.
 */
class AudioRingBufferTest {
    /** capacity = 16 bytes, bytesPerFrame = 4 (stereo 16-bit). */
    private fun tinyBuffer() = AudioRingBuffer(capacityMs = 4, sampleRate = 1000, channels = 2)

    private fun bytes(range: IntRange): ByteArray = range.map { it.toByte() }.toByteArray()

    // ---------------------------------------------------------------
    // Basic roundtrip
    // ---------------------------------------------------------------

    @Test
    fun basicWriteReadRoundtripIsByteExact() {
        val buf = tinyBuffer()
        val input = bytes(1..8)

        assertEquals(8, buf.write(input))
        assertEquals(8, buf.availableForRead())
        assertEquals(8, buf.availableForWrite())

        val out = ByteArray(8)
        assertEquals(8, buf.read(out))
        assertArrayEquals(input, out)

        assertEquals(0, buf.availableForRead())
        assertEquals(16, buf.availableForWrite())
        assertEquals(8L, buf.totalBytesWritten)
        assertEquals(8L, buf.totalBytesRead)
        assertEquals(0, buf.overflowCount)
        assertEquals(0, buf.underflowCount)
        assertEquals(0L, buf.discardedBytes)
    }

    @Test
    fun partialReadReturnsOnlyAvailableBytes() {
        val buf = tinyBuffer()
        buf.write(bytes(1..6))

        val out = ByteArray(16)
        assertEquals(6, buf.read(out))
        assertArrayEquals(bytes(1..6), out.copyOfRange(0, 6))
        assertEquals(0, buf.availableForRead())
    }

    // ---------------------------------------------------------------
    // Wraparound
    // ---------------------------------------------------------------

    @Test
    fun writeAndReadWrapAroundCapacityBoundary() {
        val buf = tinyBuffer() // capacity 16
        // Advance cursors to index 12 so the next write spans the end.
        buf.write(bytes(1..12))
        val sink = ByteArray(12)
        assertEquals(12, buf.read(sink))

        // Write 10 bytes at index 12: 4 land at [12..15], 6 wrap to [0..5].
        val wrapped = bytes(21..30)
        assertEquals(10, buf.write(wrapped))
        assertEquals(10, buf.availableForRead())

        // Read spans the wrap point too.
        val out = ByteArray(10)
        assertEquals(10, buf.read(out))
        assertArrayEquals(wrapped, out)
    }

    @Test
    fun manySequentialWritesReadsStayByteExactAcrossManyWraps() {
        val buf = tinyBuffer() // capacity 16
        var seq = 0
        val out = ByteArray(6)
        repeat(100) {
            val chunk = ByteArray(6) { i -> (seq + i).toByte() }
            assertEquals(6, buf.write(chunk))
            assertEquals(6, buf.read(out))
            assertArrayEquals(chunk, out)
            seq += 6
        }
        assertEquals(600L, buf.totalBytesWritten)
        assertEquals(600L, buf.totalBytesRead)
        assertEquals(0, buf.overflowCount)
    }

    // ---------------------------------------------------------------
    // Exact-capacity fill (no -1 sentinel slot)
    // ---------------------------------------------------------------

    @Test
    fun fillToExactCapacityHoldsAllBytesWithNoSentinelSlot() {
        val buf = tinyBuffer() // capacity 16
        val input = bytes(1..16)

        assertEquals(16, buf.write(input))
        assertEquals(16, buf.availableForRead())
        assertEquals(0, buf.availableForWrite())
        assertEquals(1.0f, buf.fillLevel(), 0.0f)
        assertEquals(0, buf.overflowCount)
        assertEquals(0L, buf.discardedBytes)

        val out = ByteArray(16)
        assertEquals(16, buf.read(out))
        assertArrayEquals(input, out)
    }

    // ---------------------------------------------------------------
    // Overflow: overwrite-oldest, frame-aligned discard
    // ---------------------------------------------------------------

    @Test
    fun overflowDiscardsOldestFrameAlignedAndKeepsSurvivingBytes() {
        val buf = tinyBuffer() // capacity 16, frame 4
        buf.write(bytes(1..16)) // full

        // Overflow by 6 bytes (1.5 frames). Discard must round UP to 8 bytes (2 frames).
        val overflowData = bytes(101..106)
        assertEquals(6, buf.write(overflowData))

        assertEquals(1, buf.overflowCount)
        assertEquals(8L, buf.discardedBytes)
        assertEquals(8L % 4L, buf.discardedBytes % 4L) // frame-aligned
        // 16 - 8 discarded + 6 new = 14 readable.
        assertEquals(14, buf.availableForRead())

        val out = ByteArray(14)
        assertEquals(14, buf.read(out))
        // Survivors: original bytes 9..16, then the 6 overflow bytes.
        assertArrayEquals(bytes(9..16) + overflowData, out)
    }

    @Test
    fun overflowWithOddSizesAlwaysDiscardsWholeFrames() {
        val buf = tinyBuffer() // capacity 16, frame 4
        buf.write(bytes(1..16)) // full

        // Overflow by 5 bytes: minDiscard 5 -> aligned 8.
        assertEquals(5, buf.write(bytes(51..55)))
        assertEquals(8L, buf.discardedBytes)
        assertEquals(1, buf.overflowCount)
        assertEquals(13, buf.availableForRead())

        // Overflow again by 7 bytes: space = 3, minDiscard 4 -> aligned 4.
        assertEquals(7, buf.write(bytes(61..67)))
        assertEquals(12L, buf.discardedBytes)
        assertEquals(2, buf.overflowCount)
        assertEquals(16, buf.availableForRead())

        val out = ByteArray(16)
        assertEquals(16, buf.read(out))
        // Timeline: 1..16 -> discard 1..8 -> +51..55 -> discard 9..12 -> +61..67.
        assertArrayEquals(bytes(13..16) + bytes(51..55) + bytes(61..67), out)
    }

    // ---------------------------------------------------------------
    // Oversized single write (> capacity)
    // ---------------------------------------------------------------

    @Test
    fun oversizedWriteKeepsOnlyFirstCapacityBytesNoPhantomData() {
        val buf = tinyBuffer() // capacity 16
        val input = bytes(1..20)

        // Only the first frame-aligned capacity bytes (16) are written; tail dropped.
        assertEquals(16, buf.write(input))
        assertEquals(16, buf.availableForRead())
        assertEquals(0, buf.availableForWrite())
        assertEquals(16L, buf.totalBytesWritten)

        val out = ByteArray(20)
        assertEquals(16, buf.read(out))
        assertArrayEquals(bytes(1..16), out.copyOfRange(0, 16))
        // Nothing phantom left behind.
        assertEquals(0, buf.availableForRead())
    }

    @Test
    fun oversizedWriteIntoNonEmptyBufferDiscardsAllOldData() {
        val buf = tinyBuffer() // capacity 16
        buf.write(bytes(1..8))

        // 20 > capacity: capped to 16, must evict the existing 8 bytes entirely.
        assertEquals(16, buf.write(bytes(101..120)))
        assertEquals(16, buf.availableForRead())
        assertEquals(8L, buf.discardedBytes)

        val out = ByteArray(16)
        assertEquals(16, buf.read(out))
        assertArrayEquals(bytes(101..116), out)
    }

    // ---------------------------------------------------------------
    // clear()
    // ---------------------------------------------------------------

    @Test
    fun clearEmptiesBufferAndSubsequentWriteReadWorks() {
        val buf = tinyBuffer()
        buf.write(bytes(1..10))
        assertEquals(10, buf.availableForRead())

        buf.clear()
        assertEquals(0, buf.availableForRead())
        assertEquals(16, buf.availableForWrite())
        assertEquals(0.0f, buf.fillLevel(), 0.0f)

        // Buffer is fully usable after clear (cursors stay monotonic, no reset glitch).
        val fresh = bytes(31..38)
        assertEquals(8, buf.write(fresh))
        val out = ByteArray(8)
        assertEquals(8, buf.read(out))
        assertArrayEquals(fresh, out)
    }

    // ---------------------------------------------------------------
    // Underflow + fill-level math
    // ---------------------------------------------------------------

    @Test
    fun underflowCountIncrementsOnEmptyRead() {
        val buf = tinyBuffer()
        val out = ByteArray(4)

        assertEquals(0, buf.read(out))
        assertEquals(1, buf.underflowCount)
        assertEquals(0, buf.read(out))
        assertEquals(2, buf.underflowCount)
        assertEquals(0L, buf.totalBytesRead)

        // A non-empty read does not bump the counter.
        buf.write(bytes(1..4))
        assertEquals(4, buf.read(out))
        assertEquals(2, buf.underflowCount)
    }

    @Test
    fun fillLevelMsMapsBytesBackToMillisecondsAt48kStereo() {
        // 48000 Hz * 2 ch * 2 bytes = 192000 B/s -> 192 bytes per ms.
        val buf = AudioRingBuffer(capacityMs = 200, sampleRate = 48000, channels = 2)
        assertEquals(200 * 192, buf.getStats()["capacityBytes"])
        assertEquals(0, buf.fillLevelMs())

        // 50 ms worth of audio.
        buf.write(ByteArray(50 * 192))
        assertEquals(50, buf.fillLevelMs())
        assertEquals(0.25f, buf.fillLevel(), 1e-6f)

        // Fill to the brim: fillLevelMs == capacityMs exactly.
        buf.write(ByteArray(150 * 192))
        assertEquals(200, buf.fillLevelMs())
        assertEquals(1.0f, buf.fillLevel(), 0.0f)

        // Draining 25 ms drops it accordingly.
        buf.read(ByteArray(25 * 192))
        assertEquals(175, buf.fillLevelMs())
    }

    // ---------------------------------------------------------------
    // offset / length variants
    // ---------------------------------------------------------------

    @Test
    fun writeAndReadHonorOffsetAndLength() {
        val buf = tinyBuffer()
        val src = bytes(0..15)

        // Write 6 bytes starting at offset 4: values 4..9.
        assertEquals(6, buf.write(src, offset = 4, length = 6))
        assertEquals(6, buf.availableForRead())

        // Read into the middle of a pre-filled sink; surroundings must be untouched.
        val sink = ByteArray(12) { 99 }
        assertEquals(6, buf.read(sink, offset = 3, length = 6))
        assertArrayEquals(ByteArray(3) { 99 }, sink.copyOfRange(0, 3))
        assertArrayEquals(bytes(4..9), sink.copyOfRange(3, 9))
        assertArrayEquals(ByteArray(3) { 99 }, sink.copyOfRange(9, 12))
    }

    @Test
    fun defaultLengthUsesRemainderAfterOffsetAndZeroLengthWriteIsNoop() {
        val buf = tinyBuffer()
        val src = bytes(1..10)

        // Default length = data.size - offset.
        assertEquals(4, buf.write(src, offset = 6))
        val out = ByteArray(4)
        assertEquals(4, buf.read(out))
        assertArrayEquals(bytes(7..10), out)

        // Zero/negative length writes are no-ops.
        assertEquals(0, buf.write(src, offset = 10))
        assertEquals(0, buf.write(src, offset = 0, length = 0))
        assertEquals(0, buf.availableForRead())
    }

    // ---------------------------------------------------------------
    // Concurrency smoke test (SPSC)
    // ---------------------------------------------------------------

    /**
     * One producer writes stereo frames whose 4 bytes encode a big-endian frame id;
     * one consumer reads concurrently for ~1 second. Under overflow, frames may be
     * DISCARDED (gaps allowed) but must never be reordered, duplicated, or torn:
     * every frame id observed must be strictly greater than the previous one and
     * must be an id that was actually produced.
     */
    @Test
    @Suppress("LongMethod") // integration-style concurrency scenario; splitting hurts readability
    fun concurrentProducerConsumerFramesAreStrictlyIncreasingAndUncorrupted() {
        // 48kHz stereo, 5 ms -> capacity 960 bytes = 240 four-byte frames.
        val buf = AudioRingBuffer(capacityMs = 5, sampleRate = 48000, channels = 2)
        val frameBytes = 4
        val running = AtomicBoolean(true)
        val producerDone = AtomicBoolean(false)
        val producedFrames = AtomicLong(0)
        val consumedFrames = AtomicLong(0)
        val failure = AtomicReference<String?>(null)
        val startLatch = CountDownLatch(1)

        val producer =
            Thread {
                startLatch.await()
                var frameId = 0
                val chunkFrames = 12
                val chunk = ByteArray(chunkFrames * frameBytes)
                while (running.get()) {
                    for (f in 0 until chunkFrames) {
                        val id = frameId + f
                        chunk[f * 4] = (id ushr 24).toByte()
                        chunk[f * 4 + 1] = (id ushr 16).toByte()
                        chunk[f * 4 + 2] = (id ushr 8).toByte()
                        chunk[f * 4 + 3] = id.toByte()
                    }
                    val written = buf.write(chunk)
                    if (written != chunk.size) {
                        failure.compareAndSet(null, "write returned $written, expected ${chunk.size}")
                        return@Thread
                    }
                    frameId += chunkFrames
                    producedFrames.set(frameId.toLong())
                }
            }

        val consumer =
            Thread {
                startLatch.await()
                var lastId = -1
                val out = ByteArray(16 * frameBytes)
                // Keep draining until the producer has fully stopped AND the buffer is
                // empty — producerDone is only set after producer.join(), so no data can
                // arrive once both conditions hold.
                while (!producerDone.get() || buf.availableForRead() > 0) {
                    val n = buf.read(out)
                    if (n == 0) continue
                    if (n % frameBytes != 0) {
                        failure.compareAndSet(null, "read returned $n bytes, not frame-aligned")
                        return@Thread
                    }
                    for (f in 0 until n / frameBytes) {
                        val id =
                            ((out[f * 4].toInt() and 0xFF) shl 24) or
                                ((out[f * 4 + 1].toInt() and 0xFF) shl 16) or
                                ((out[f * 4 + 2].toInt() and 0xFF) shl 8) or
                                (out[f * 4 + 3].toInt() and 0xFF)
                        if (id <= lastId) {
                            failure.compareAndSet(
                                null,
                                "frame id $id not strictly greater than previous $lastId " +
                                    "(reorder/duplicate/torn frame)",
                            )
                            return@Thread
                        }
                        lastId = id
                        consumedFrames.incrementAndGet()
                    }
                }
            }

        producer.start()
        consumer.start()
        startLatch.countDown()
        Thread.sleep(1000)
        running.set(false)
        producer.join(5000)
        producerDone.set(true)
        consumer.join(5000)

        assertNull("Corruption detected: ${failure.get()}", failure.get())
        assertTrue("Producer made no progress", producedFrames.get() > 0)
        assertTrue("Consumer made no progress", consumedFrames.get() > 0)
        assertTrue(
            "Consumed (${consumedFrames.get()}) cannot exceed produced (${producedFrames.get()})",
            consumedFrames.get() <= producedFrames.get(),
        )
        // Accounting invariant: everything written was either read or discarded (buffer drained).
        assertEquals(0, buf.availableForRead())
        assertEquals(buf.totalBytesWritten, buf.totalBytesRead + buf.discardedBytes)
    }

    @Test
    @Suppress("LongMethod") // integration-style concurrency scenario, same as the SPSC test above
    fun concurrentClearNeverBreaksInvariants() {
        // clear() is called from control paths while the producer/consumer are live.
        // The CAS-loop implementation must only ever move the read cursor FORWARD:
        // a backward move would replay consumed bytes and let availableForRead exceed
        // capacity. Run P + C + a clearing thread and assert the level invariant and
        // frame-id monotonicity (clear may only skip frames, never repeat them).
        val buf = AudioRingBuffer(capacityMs = 5, sampleRate = 48000, channels = 2)
        val capacity = 5 * 192
        val stop =
            java.util.concurrent.atomic
                .AtomicBoolean(false)
        val failure =
            java.util.concurrent.atomic
                .AtomicReference<String?>(null)

        val producer =
            Thread {
                var frameId = 0
                val frame = ByteArray(4)
                while (!stop.get()) {
                    frame[0] = (frameId ushr 24).toByte()
                    frame[1] = (frameId ushr 16).toByte()
                    frame[2] = (frameId ushr 8).toByte()
                    frame[3] = frameId.toByte()
                    buf.write(frame)
                    frameId++
                    val level = buf.availableForRead()
                    if (level < 0 || level > capacity) {
                        failure.compareAndSet(null, "producer saw level=$level (capacity=$capacity)")
                        stop.set(true)
                    }
                }
            }
        val consumer =
            Thread {
                val out = ByteArray(4)
                var lastId = -1
                while (!stop.get()) {
                    val n = buf.read(out)
                    if (n == 4) {
                        val id =
                            ((out[0].toInt() and 0xFF) shl 24) or ((out[1].toInt() and 0xFF) shl 16) or
                                ((out[2].toInt() and 0xFF) shl 8) or (out[3].toInt() and 0xFF)
                        if (id <= lastId) {
                            failure.compareAndSet(null, "frame id went backward/repeated: $id after $lastId")
                            stop.set(true)
                        }
                        lastId = id
                    } else if (n != 0) {
                        failure.compareAndSet(null, "non-frame-aligned read: $n bytes")
                        stop.set(true)
                    }
                }
            }
        val clearer =
            Thread {
                while (!stop.get()) {
                    buf.clear()
                    val level = buf.availableForRead()
                    if (level < 0 || level > capacity) {
                        failure.compareAndSet(null, "clearer saw level=$level (capacity=$capacity)")
                        stop.set(true)
                    }
                    Thread.sleep(1)
                }
            }

        producer.start()
        consumer.start()
        clearer.start()
        Thread.sleep(700)
        stop.set(true)
        producer.join(2000)
        consumer.join(2000)
        clearer.join(2000)

        org.junit.Assert.assertNull("Invariant violation: ${failure.get()}", failure.get())
    }
}
