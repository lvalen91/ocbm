package com.carlink.protocol

import org.json.JSONObject
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import java.nio.charset.StandardCharsets

/**
 * Golden-byte unit tests for [MessageSerializer].
 *
 * Wire format under test (CPC200-CCPA): 16-byte little-endian header
 * (magic 0x55aa55aa, payload length, type id, ~type id) followed by a
 * type-specific payload. Assertions are made against exact byte values
 * so any accidental change to endianness, field order, or offsets fails loudly.
 */
@RunWith(RobolectricTestRunner::class)
class MessageSerializerTest {
    // ==================== Little-endian helpers ====================

    private fun readIntLE(
        bytes: ByteArray,
        offset: Int,
    ): Int =
        (bytes[offset].toInt() and 0xFF) or
            ((bytes[offset + 1].toInt() and 0xFF) shl 8) or
            ((bytes[offset + 2].toInt() and 0xFF) shl 16) or
            ((bytes[offset + 3].toInt() and 0xFF) shl 24)

    private fun readFloatLE(
        bytes: ByteArray,
        offset: Int,
    ): Float = Float.fromBits(readIntLE(bytes, offset))

    private fun intLE(value: Int): ByteArray =
        byteArrayOf(
            (value and 0xFF).toByte(),
            ((value ushr 8) and 0xFF).toByte(),
            ((value ushr 16) and 0xFF).toByte(),
            ((value ushr 24) and 0xFF).toByte(),
        )

    private fun floatLE(value: Float): ByteArray = intLE(value.toRawBits())

    /** Expected 16-byte header for [typeId] with payload [length]. */
    private fun expectedHeader(
        typeId: Int,
        length: Int,
    ): ByteArray = intLE(PROTOCOL_MAGIC) + intLE(length) + intLE(typeId) + intLE(typeId.inv())

    /** Asserts the first 16 bytes of [frame] form a valid header and returns the payload. */
    private fun assertHeaderAndExtractPayload(
        frame: ByteArray,
        expectedTypeId: Int,
        expectedLength: Int,
    ): ByteArray {
        assertTrue("frame shorter than header: ${frame.size}", frame.size >= HEADER_SIZE)
        assertArrayEquals("magic", intLE(0x55aa55aa.toInt()), frame.copyOfRange(0, 4))
        assertEquals("payload length field", expectedLength, readIntLE(frame, 4))
        assertEquals("type id", expectedTypeId, readIntLE(frame, 8))
        assertEquals("type check (~type)", expectedTypeId.inv(), readIntLE(frame, 12))
        assertEquals("frame size", HEADER_SIZE + expectedLength, frame.size)
        return frame.copyOfRange(HEADER_SIZE, frame.size)
    }

    // ==================== Header / heartbeat ====================

    @Test
    fun `heartbeat is exactly the 16 golden header bytes`() {
        val frame = MessageSerializer.serializeHeartbeat()

        // HEARTBEAT id = 0xAA; ~0xAA = 0xFFFFFF55
        val golden =
            byteArrayOf(
                0xAA.toByte(),
                0x55,
                0xAA.toByte(),
                0x55, // magic 0x55aa55aa LE
                0x00,
                0x00,
                0x00,
                0x00, // length = 0
                0xAA.toByte(),
                0x00,
                0x00,
                0x00, // type = 0xAA LE
                0x55,
                0xFF.toByte(),
                0xFF.toByte(),
                0xFF.toByte(), // ~type LE
            )
        assertArrayEquals(golden, frame)
        assertEquals(0xAA, MessageType.HEARTBEAT.id)
    }

    @Test
    fun `heartbeat repeated calls return equal content and the precomputed cached array`() {
        val first = MessageSerializer.serializeHeartbeat()
        val second = MessageSerializer.serializeHeartbeat()
        assertArrayEquals(first, second)
        // Documented behavior: precomputed once, same instance every call.
        assertSame(first, second)
    }

    @Test
    fun `createHeader encodes magic length type and complement little-endian`() {
        val header = MessageSerializer.createHeader(MessageType.VIDEO_DATA, 0x01020304)
        assertArrayEquals(
            intLE(0x55aa55aa.toInt()) + intLE(0x01020304) + intLE(0x06) + intLE(0x06.inv()),
            header,
        )
        assertEquals(HEADER_SIZE, header.size)
    }

    // ==================== Command ====================

    @Test
    fun `serializeCommand emits 16-byte header with length 4 plus LE command id`() {
        val frame = MessageSerializer.serializeCommand(CommandMapping.SIRI)

        // COMMAND type = 0x08, SIRI id = 5
        assertArrayEquals(expectedHeader(0x08, 4) + intLE(5), frame)
        assertEquals(20, frame.size)
    }

    @Test
    fun `serializeCommand multi-byte id is little-endian`() {
        val frame = MessageSerializer.serializeCommand(CommandMapping.WIFI_ENABLE)

        // WIFI_ENABLE id = 1000 = 0x3E8 → E8 03 00 00
        val payload = assertHeaderAndExtractPayload(frame, 0x08, 4)
        assertArrayEquals(byteArrayOf(0xE8.toByte(), 0x03, 0x00, 0x00), payload)
    }

    // ==================== Open ====================

    @Test
    fun `serializeOpen writes seven config ints at fixed offsets`() {
        val config =
            AdapterConfig(
                width = 1920,
                height = 720,
                fps = 60,
            )
        val frame = MessageSerializer.serializeOpen(config)

        // OPEN type = 0x01, payload = 7 ints = 28 bytes
        val payload = assertHeaderAndExtractPayload(frame, 0x01, 28)
        assertEquals("width @0", 1920, readIntLE(payload, 0))
        assertEquals("height @4", 720, readIntLE(payload, 4))
        assertEquals("fps @8", 60, readIntLE(payload, 8))
        assertEquals("format @12", 5, readIntLE(payload, 12))
        assertEquals("packetMax @16", 49152, readIntLE(payload, 16))
        assertEquals("iBoxVersion @20", 2, readIntLE(payload, 20))
        assertEquals("phoneWorkMode @24", 2, readIntLE(payload, 24))
    }

    @Test
    fun `serializeOpen golden bytes for default config`() {
        val frame = MessageSerializer.serializeOpen(AdapterConfig.DEFAULT)

        val golden =
            expectedHeader(0x01, 28) +
                intLE(1920) + intLE(720) + intLE(60) + intLE(5) + intLE(49152) + intLE(2) + intLE(2)
        assertArrayEquals(golden, frame)
    }

    // ==================== Audio ====================

    @Test
    fun `serializeAudio prefixes 12 bytes then payload verbatim`() {
        val pcm = byteArrayOf(0x11, 0x22, 0x33, 0x44, 0x55)
        val frame = MessageSerializer.serializeAudio(pcm, decodeType = 5, audioType = 3, volume = 0.5f)

        // AUDIO_DATA type = 0x07; payload = 12-byte prefix + 5 data bytes
        val payload = assertHeaderAndExtractPayload(frame, 0x07, 12 + pcm.size)
        assertEquals("decodeType @0", 5, readIntLE(payload, 0))
        assertEquals("volume @4", 0.5f, readFloatLE(payload, 4))
        // 0.5f = 0x3F000000 → LE 00 00 00 3F
        assertArrayEquals(byteArrayOf(0x00, 0x00, 0x00, 0x3F), payload.copyOfRange(4, 8))
        assertEquals("audioType @8", 3, readIntLE(payload, 8))
        assertArrayEquals("data verbatim @12", pcm, payload.copyOfRange(12, payload.size))
    }

    @Test
    fun `serializeAudio defaults golden frame with empty data`() {
        val frame = MessageSerializer.serializeAudio(ByteArray(0))

        // Defaults: decodeType=5, volume=0.0f, audioType=3
        val golden = expectedHeader(0x07, 12) + intLE(5) + floatLE(0.0f) + intLE(3)
        assertArrayEquals(golden, frame)
    }

    // ==================== Multi-touch ====================

    @Test
    fun `serializeMultiTouch two pointers produce a 32-byte payload in x y action id order`() {
        val touches =
            listOf(
                MessageSerializer.TouchPoint(x = 0.25f, y = 0.75f, action = MultiTouchAction.DOWN, id = 0),
                MessageSerializer.TouchPoint(x = 1.0f, y = 0.0f, action = MultiTouchAction.MOVE, id = 1),
            )
        val frame = MessageSerializer.serializeMultiTouch(touches)

        // MULTI_TOUCH type = 0x17; 2 touches x 16 bytes
        val golden =
            expectedHeader(0x17, 32) +
                floatLE(0.25f) + floatLE(0.75f) + intLE(1) + intLE(0) + // DOWN id=1
                floatLE(1.0f) + floatLE(0.0f) + intLE(2) + intLE(1) // MOVE id=2
        assertArrayEquals(golden, frame)
    }

    @Test
    fun `serializeMultiTouch coordinates roundtrip bit-exact`() {
        val x = 0.123456789f
        val y = 0.987654321f
        val frame =
            MessageSerializer.serializeMultiTouch(
                listOf(MessageSerializer.TouchPoint(x, y, MultiTouchAction.UP, id = 7)),
            )

        val payload = assertHeaderAndExtractPayload(frame, 0x17, 16)
        assertEquals("x bits", x.toRawBits(), readIntLE(payload, 0))
        assertEquals("y bits", y.toRawBits(), readIntLE(payload, 4))
        assertEquals("action UP id=0", 0, readIntLE(payload, 8))
        assertEquals("pointer id", 7, readIntLE(payload, 12))
    }

    @Test
    fun `serializeMultiTouch empty list yields header-only frame`() {
        val frame = MessageSerializer.serializeMultiTouch(emptyList())
        assertArrayEquals(expectedHeader(0x17, 0), frame)
    }

    // ==================== Box settings ====================

    @Test
    fun `serializeBoxSettings payload is valid JSON with syncTime and mediaSound 1`() {
        val frame = MessageSerializer.serializeBoxSettings(AdapterConfig.DEFAULT, syncTime = 1700000000L)

        // BOX_SETTINGS type = 0x19
        val payloadLength = readIntLE(frame, 4)
        val payload = assertHeaderAndExtractPayload(frame, 0x19, payloadLength)

        val json = JSONObject(String(payload, StandardCharsets.US_ASCII))
        assertEquals(1700000000L, json.getLong("syncTime"))
        assertEquals(1, json.getInt("mediaSound"))
        assertEquals(36, json.getInt("WiFiChannel"))
        assertEquals(500, json.getInt("mediaDelay"))
        assertEquals(1, json.getInt("DashboardInfo"))
        assertEquals("carlink", json.getString("wifiName"))
        assertEquals("carlink", json.getString("btName"))
        assertEquals("carlink", json.getString("boxName"))
        assertTrue(json.getBoolean("autoConn"))
    }

    // ==================== Header-only device messages ====================

    @Test
    fun `serializeDisconnectPhone is header-only with type 0x0F`() {
        assertArrayEquals(expectedHeader(0x0F, 0), MessageSerializer.serializeDisconnectPhone())
    }

    @Test
    fun `serializeCloseDongle is header-only with type 0x15`() {
        assertArrayEquals(expectedHeader(0x15, 0), MessageSerializer.serializeCloseDongle())
    }

    @Test
    fun `serializeRebootAdapter is header-only with dual-direction type 0xCD`() {
        // Outbound 0xCD = HUDComand_A_Reboot; enum name HEARTBEAT_ECHO reflects the inbound meaning.
        assertArrayEquals(expectedHeader(0xCD, 0), MessageSerializer.serializeRebootAdapter())
        assertEquals(0xCD, MessageType.HEARTBEAT_ECHO.id)
    }

    // ==================== BT MAC messages ====================

    @Test
    fun `serializeForgetBluetoothAddr emits 17-byte ASCII MAC payload with type 0x22`() {
        val mac = "AA:BB:CC:DD:EE:FF"
        val frame = MessageSerializer.serializeForgetBluetoothAddr(mac)

        val payload = assertHeaderAndExtractPayload(frame, 0x22, 17)
        assertArrayEquals(mac.toByteArray(StandardCharsets.US_ASCII), payload)
        assertEquals("AA:BB:CC:DD:EE:FF", String(payload, StandardCharsets.US_ASCII))
    }

    @Test
    fun `serializeAutoConnectByBtAddress emits 17-byte ASCII MAC payload with dual-direction type 0x11`() {
        val mac = "01:23:45:67:89:ab"
        val frame = MessageSerializer.serializeAutoConnectByBtAddress(mac)

        // Outbound 0x11 = AutoConnect_By_BluetoothAddress (inbound name WIFI_STATUS_DATA)
        val payload = assertHeaderAndExtractPayload(frame, 0x11, 17)
        assertArrayEquals(mac.toByteArray(StandardCharsets.US_ASCII), payload)
    }

    @Test
    fun `malformed BT MAC is rejected with IllegalArgumentException`() {
        assertThrows(IllegalArgumentException::class.java) {
            MessageSerializer.serializeForgetBluetoothAddr("AA-BB-CC-DD-EE-FF")
        }
        assertThrows(IllegalArgumentException::class.java) {
            MessageSerializer.serializeForgetBluetoothAddr("AA:BB:CC:DD:EE")
        }
        assertThrows(IllegalArgumentException::class.java) {
            MessageSerializer.serializeAutoConnectByBtAddress("GG:BB:CC:DD:EE:FF")
        }
    }

    // ==================== File messages ====================

    @Test
    fun `serializeFile golden bytes include null-terminated name and length prefixes`() {
        val content = byteArrayOf(0x01, 0x02, 0x03)
        val frame = MessageSerializer.serializeFile("/tmp/x", content)

        val nameBytes = "/tmp/x\u0000".toByteArray(StandardCharsets.US_ASCII)
        // SEND_FILE type = 0x99; payload = nameLen(4) + name(7) + contentLen(4) + content(3)
        val golden = expectedHeader(0x99, 18) + intLE(7) + nameBytes + intLE(3) + content
        assertArrayEquals(golden, frame)
    }

    @Test
    fun `serializeNumber writes 4-byte LE file content`() {
        val frame = MessageSerializer.serializeNumber(160, FileAddress.DPI)

        val nameBytes = (FileAddress.DPI.path + "\u0000").toByteArray(StandardCharsets.US_ASCII)
        val golden = expectedHeader(0x99, 4 + nameBytes.size + 4 + 4) + intLE(nameBytes.size) + nameBytes + intLE(4) + intLE(160)
        assertArrayEquals(golden, frame)
    }

    // ==================== Init sequence ordering ====================

    /** Parses the header type id of a serialized frame. */
    private fun frameTypeId(frame: ByteArray): Int = readIntLE(frame, 8)

    /** For a COMMAND frame, the 4-byte LE command id payload. */
    private fun commandId(frame: ByteArray): Int {
        assertEquals("expected COMMAND frame", 0x08, frameTypeId(frame))
        return readIntLE(frame, HEADER_SIZE)
    }

    @Test
    fun `generateInitRemainder FULL ends with WIFI_ENABLE as the last frame`() {
        val config =
            AdapterConfig(
                viewAreaData = ByteArray(24) { it.toByte() },
                safeAreaData = ByteArray(20) { (it + 100).toByte() },
            )
        val messages = MessageSerializer.generateInitRemainder(config, "FULL")

        assertTrue("FULL mode should emit multiple frames", messages.size > 1)
        val last = messages.last()
        assertEquals(1000, commandId(last)) // WIFI_ENABLE
        // WIFI_ENABLE must appear exactly once, at the end.
        val wifiEnableIndices =
            messages.withIndex().filter { (_, frame) ->
                frameTypeId(frame) == 0x08 && readIntLE(frame, HEADER_SIZE) == 1000
            }
        assertEquals(listOf(messages.lastIndex), wifiEnableIndices.map { it.index })
    }

    @Test
    fun `generateInitRemainder FULL frame type sequence matches contract`() {
        val config =
            AdapterConfig(
                viewAreaData = ByteArray(24),
                safeAreaData = ByteArray(20),
            )
        val messages = MessageSerializer.generateInitRemainder(config, "FULL")

        // DPI file, ViewArea file, SafeArea file, handDriveMode file, boxName file,
        // chargeMode file, WiFi band command, BoxSettings JSON, AirPlay config file,
        // mic command, audio-transfer command, WIFI_ENABLE command.
        val expectedTypes = listOf(0x99, 0x99, 0x99, 0x99, 0x99, 0x99, 0x08, 0x19, 0x99, 0x08, 0x08, 0x08)
        assertEquals(expectedTypes, messages.map { frameTypeId(it) })

        // Default config: 5GHz band, OS mic (car mic), USB audio (transfer off).
        assertEquals(25, commandId(messages[6])) // WIFI_5G
        assertEquals(7, commandId(messages[9])) // MIC (car mic, micType="os")
        assertEquals(23, commandId(messages[10])) // AUDIO_TRANSFER_OFF
        assertEquals(1000, commandId(messages[11])) // WIFI_ENABLE last
    }

    @Test
    fun `generateInitRemainder MINIMAL_ONLY emits exactly one WIFI_ENABLE frame`() {
        val messages = MessageSerializer.generateInitRemainder(AdapterConfig.DEFAULT, "MINIMAL_ONLY")

        assertEquals(1, messages.size)
        assertEquals(1000, commandId(messages[0]))
        assertArrayEquals(expectedHeader(0x08, 4) + intLE(1000), messages[0])
    }

    @Test
    fun `generateInitRemainder FULL box settings frame parses as JSON with mediaSound 1`() {
        val messages = MessageSerializer.generateInitRemainder(AdapterConfig.DEFAULT, "FULL")

        val boxSettingsFrames = messages.filter { frameTypeId(it) == 0x19 }
        assertEquals(1, boxSettingsFrames.size)
        val payload = boxSettingsFrames[0].copyOfRange(HEADER_SIZE, boxSettingsFrames[0].size)
        val json = JSONObject(String(payload, StandardCharsets.US_ASCII))
        assertTrue("syncTime present", json.has("syncTime"))
        assertEquals(1, json.getInt("mediaSound"))
    }
}
