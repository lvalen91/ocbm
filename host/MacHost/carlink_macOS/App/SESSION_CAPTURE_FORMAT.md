# Session Capture Binary Format

> **⚠️ SUPERSEDED 2026-07-12 — HISTORICAL RECORD ONLY. The Session Capture feature described below no
> longer exists in this app, and no file in this repo produces or consumes this format.** The Capture
> menu and its ⌘⇧R shortcut went out with the rest of the menu-driven config (`main.swift`: "Capture /
> Resolution / Navigation menus removed 2026-07-12"), and every type this document names is gone from
> the tree: `SessionRecorder`, `ProtocolLogger`, `AdapterProtocol`, `MessageSerializer`, `MessageParser`,
> `MessageTypes`, `SessionTokenDecryptor`, `IAP2CallStateDecoder`, and the entire `Protocol/` directory.
> That takes with it the riddlebox `0x55AA55AA` message-type numbering the record layout is built on and
> the AES-128-CBC adapter-info decode under "Device Information Blob" below. The committed model is OCBM
> + forward-encrypted ChaCha20-Poly1305 A/V (see `../../HOSTAPP.md`); live diagnostics are `FileLogger`
> (OSLogStore → `~/Library/Logs/Carlink/`) plus `StreamMetrics` / `StreamMetricsMonitor`. Retained as the
> specification of already-captured `.clnk` files and as the starting point for anyone re-adding a
> recorder against the OCBM path — do NOT read any of it as current app behaviour.

## Overview

The Session Capture feature logs the protocol messages that the CarLink app itself sends to and receives from the CPC200-CCPA adapter, for its own diagnostics. Audio and video payloads are sampled (headers + partial data) to reduce file size while preserving format identification information. All other messages are captured in full.

**Device metadata** including the adapter's reported firmware version and device-info record are stored in the session-log header to aid debugging and interoperability support.

## File Format Specification

### Session Header (256 bytes)

| Offset | Size | Type   | Description                                    |
|--------|------|--------|------------------------------------------------|
| 0x00   | 4    | ASCII  | Magic: "CLNK" (0x43 0x4C 0x4E 0x4B)           |
| 0x04   | 4    | uint32 | Version: 2 (little-endian)                     |
| 0x08   | 8    | uint64 | Start timestamp (Unix epoch milliseconds, LE) — written at start of recording |
| 0x10   | 8    | uint64 | End timestamp (Unix epoch milliseconds, LE) — 0 until clean stop |
| 0x18   | 4    | uint32 | Total message count (LE) — 0 until clean stop  |
| 0x1C   | 1    | uint8  | Firmware version string length (0-63)          |
| 0x1D   | 63   | ASCII  | Firmware version (UTF-8, null-padded)          |
| 0x5C   | 128  | bytes  | **Reserved (zeros).** Previously a truncated copy of the 0xA3 blob; decoders should ignore this region. The full blob is preserved as a body record of type `0xA3`. |
| 0xDC   | 36   | bytes  | Reserved (zeros)                               |

**Crash captures:** the start timestamp is written when recording starts; the
end timestamp, message count, and firmware/device metadata are finalized on a
clean stop. If `msg_count` is 0 (app crashed or was force-quit mid-session),
parsers should read message records until EOF instead of trusting the count,
truncating any final partial record.

### Message Records (Variable length) — format v2 (17-byte fixed part)

Each message record follows this structure:

| Offset | Size     | Type   | Description                                  |
|--------|----------|--------|----------------------------------------------|
| 0x00   | 1        | uint8  | Direction: 0 = RX (received), 1 = TX (sent) |
| 0x01   | 4        | uint32 | Timestamp offset (milliseconds from session start, LE) |
| 0x05   | 4        | uint32 | Message type (see MessageTypes.swift, LE)    |
| 0x09   | 4        | uint32 | Original payload length in bytes, before sampling (LE) |
| 0x0D   | 4        | uint32 | Sampled payload length in bytes (LE) — the number of payload bytes that follow |
| 0x11   | variable | bytes  | Payload data (sampled or full)               |

`originalLen` is the true on-wire payload size before audio/video sampling, so
captures stay analyzable for frame size / bitrate even though large payloads
are stored sampled. For unsampled messages `originalLen == sampledLen`.

## Payload Sampling Strategy

### Audio Messages (Type 0x07)

- **Full capture:**
  - Audio commands (header + 1-byte command = 13 bytes total)
  
- **Sampled capture:**
  - PCM audio data: Header (12 bytes) + first 256 bytes of samples
  
**Audio Header Structure (12 bytes):**
```
Offset 0: decodeType (uint32, LE) — sample rate/channel config
Offset 4: volume (float32, LE)
Offset 8: audioType (uint32, LE) — stream type (main/navigation/mic)
```

### Video Messages (Type 0x06, 0x2C)

- **Sampled capture:**
  - Header (20 bytes) + first 512 bytes of H.264 NAL data
  
**Video Header Structure (20 bytes):**
```
Offset 0:  width (uint32, LE)
Offset 4:  height (uint32, LE)
Offset 8:  encoderState (uint32, LE)
Offset 12: pts (uint32, LE) — presentation timestamp in milliseconds
Offset 16: flags (uint32, LE)
```

### All Other Messages

- Captured in full with complete payloads

## Device Metadata

### Firmware Version

The adapter sends its firmware version via the **softwareVersion** message (`0xCC`). This is captured as a UTF-8 string in the session header (offset `0x1D`, up to 63 bytes).

`AdapterProtocol` retains the last-seen firmware version across the lifetime of the connection. When a recording starts, the currently cached value is backfilled into the header, so a session started *after* the initial handshake still has firmware metadata. If a new `softwareVersion` message arrives mid-recording, the header value is refreshed (last-wins) on stop.

**Example firmware strings:**
- `"V1.2.3_20230615"`
- `"CPC200-FW-2.4.1"`

### Device Information Blob

The adapter sends encrypted device information via one of these messages:
- **vendorSessionInfo** (`0xA3`) — Vendor-specific session data
- **manufacturerInfo** (`0x14`) — Manufacturer identification data

This data is captured as a body record in the message stream (the 128-byte header slot at `0x5C` is reserved/zero-filled — see the header table). The blob typically contains:
- Device serial number (encrypted)
- Hardware version
- Manufacturing date
- Vendor-specific device-identity fields
- License/activation status

**Note:** The device info blob is encrypted by the adapter firmware. Decryption requires vendor-specific keys and is outside the scope of this tool. The record is stored verbatim for later vendor-support diagnostics.

The full `vendorSessionInfo` (0xA3) payload is captured as a body record in the stream, not in the header. `AdapterProtocol` also caches the last-seen payload and decodes the adapter vendor's diagnostic info record using the documented AES-128-CBC diagnostic parameters (key `W2EC1X1NbZ58TXtn`, IV = first 16 bytes of the base64-decoded payload), purely to display firmware/model info in the Help > Adapter Info panel.

The 128-byte slot at header offset `0x5C` is reserved for future use and left zero-filled; decoders should rely on the body record and ignore the header slot.

## Sample Sizes

The sampling sizes were chosen based on the existing logging implementation in `ProtocolLogger.swift`:

- **Audio sample:** 256 bytes (sufficient for format identification and initial waveform analysis)
- **Video sample:** 512 bytes (sufficient to capture SPS/PPS/I-frame headers for codec detection)

These sizes balance file size reduction with debugging utility.

## Usage

### Starting Capture

1. Select **Capture > Start Session Capture...** from the menu bar (⌘⇧R)
2. Choose a save location and filename
3. Capture begins immediately and runs in the background
4. Capture can be started before adapter connection or during an active session
5. Session operation is not interrupted

### Stopping Capture

1. Select **Capture > Stop Session Capture** from the menu bar
2. The file is finalized with the session header updated (end timestamp, message count, firmware, device-info blob)
3. The confirmation dialog only appears **after** finalization completes — when the user dismisses it, the file on disk is guaranteed to be complete
4. The `.bin` file is ready for analysis

## File Size Estimates

Based on typical CarPlay/Android Auto sessions:

- **Control messages:** ~100-500 bytes each (full capture)
- **Audio frames:** ~280 bytes each (sampled: 12 + 256 + overhead)
- **Video frames:** ~545 bytes each (sampled: 20 + 512 + overhead)

**Example:** A 5-minute session at 60fps video + 48kHz audio:
- Video: 18,000 frames × 545 bytes ≈ 9.3 MB
- Audio: ~15,000 frames × 280 bytes ≈ 4.0 MB
- Control messages: ~500 messages × 200 bytes ≈ 100 KB
- **Total:** ~13.4 MB (vs. several GB for full capture)

## Analysis Tools

The binary format is designed for easy parsing with standard tools:

### Python Example

```python
import struct

def parse_session_capture(filepath):
    with open(filepath, 'rb') as f:
        # Read header
        magic = f.read(4)
        assert magic == b'CLNK', "Invalid magic"
        
        version, = struct.unpack('<I', f.read(4))
        start_time, = struct.unpack('<Q', f.read(8))
        end_time, = struct.unpack('<Q', f.read(8))
        msg_count, = struct.unpack('<I', f.read(4))
        
        # Firmware version
        fw_len, = struct.unpack('B', f.read(1))
        fw_bytes = f.read(63)
        firmware = fw_bytes[:fw_len].decode('utf-8') if fw_len > 0 else "Unknown"
        
        # Device info blob (128 bytes)
        device_blob = f.read(128)
        
        # Reserved
        f.read(36)
        
        print(f"Session: {msg_count} messages")
        print(f"Duration: {(end_time - start_time) / 1000:.1f} seconds")
        print(f"Firmware: {firmware}")
        print(f"Device blob: {len(device_blob)} bytes")
        
        # Read v2 records. msg_count == 0 means the capture was not cleanly
        # stopped (crash) — read to EOF instead.
        i = 0
        while msg_count == 0 or i < msg_count:
            fixed = f.read(17)
            if len(fixed) < 17:
                break  # EOF (or truncated final record)
            direction, timestamp, msg_type, original_len, sampled_len = \
                struct.unpack('<BIIII', fixed)
            payload = f.read(sampled_len)
            if len(payload) < sampled_len:
                break  # truncated final record
            i += 1
            
            dir_str = "RX" if direction == 0 else "TX"
            print(f"{dir_str} @ {timestamp}ms: Type 0x{msg_type:02X} "
                  f"({original_len} bytes on wire, {sampled_len} stored)")

parse_session_capture("carlink_session.bin")
```

### Swift Example

```swift
// Note: illustrative — assumes readLE overloads for UInt32/UInt64; the app's
// Data.readLE helper returns UInt32 only.
struct SessionHeader {
    let magic: [UInt8]  // "CLNK"
    let version: UInt32
    let startTime: UInt64
    let endTime: UInt64
    let messageCount: UInt32
    let firmwareVersion: String?
    let deviceInfoBlob: Data?
}

struct MessageRecord {
    let direction: UInt8  // 0=RX, 1=TX
    let timestamp: UInt32
    let type: UInt32
    let originalLength: UInt32  // on-wire size before sampling
    let payload: Data           // sampled payload
}

func parseSessionCapture(url: URL) throws -> (SessionHeader, [MessageRecord]) {
    let data = try Data(contentsOf: url)
    
    // Parse header
    let version: UInt32 = data.readLE(at: 4)
    let startTime: UInt64 = data.readLE(at: 8)
    let endTime: UInt64 = data.readLE(at: 16)
    let msgCount: UInt32 = data.readLE(at: 24)
    
    // Firmware version
    let fwLen = Int(data[28])
    let firmwareVersion: String? = fwLen > 0 ? String(data: data[29..<(29 + fwLen)], encoding: .utf8) : nil
    
    // Device info blob
    let deviceInfoBlob = data[92..<220]
    
    let header = SessionHeader(
        magic: [0x43, 0x4C, 0x4E, 0x4B],
        version: version,
        startTime: startTime,
        endTime: endTime,
        messageCount: msgCount,
        firmwareVersion: firmwareVersion,
        deviceInfoBlob: Data(deviceInfoBlob)
    )
    
    // Parse v2 records. msgCount == 0 → crash capture: read to EOF.
    var records: [MessageRecord] = []
    var offset = 256  // Header size
    
    while (msgCount == 0 || records.count < Int(msgCount)),
          offset + 17 <= data.count {
        let direction = data[offset]
        let timestamp: UInt32 = data.readLE(at: offset + 1)
        let type: UInt32 = data.readLE(at: offset + 5)
        let originalLen: UInt32 = data.readLE(at: offset + 9)
        let sampledLen: UInt32 = data.readLE(at: offset + 13)
        let payloadStart = offset + 17
        guard payloadStart + Int(sampledLen) <= data.count else { break }  // truncated final record
        let payload = data[payloadStart..<(payloadStart + Int(sampledLen))]
        
        records.append(MessageRecord(
            direction: direction,
            timestamp: timestamp,
            type: type,
            originalLength: originalLen,
            payload: Data(payload)
        ))
        
        offset += 17 + Int(sampledLen)
    }
    
    return (header, records)
}
```

## Integration Notes

### Non-Interrupting Operation

The session recorder:
- Operates on a dedicated serial queue (`com.carlink.recorder.write`)
- Does not block the USB transport queues
- Buffers writes asynchronously
- Can be started/stopped at any time without affecting the adapter session

### Thread Safety

- `SessionRecorder` is a plain `Sendable` singleton — not actor-bound — so hooks in `ProtocolLogger` call it directly from whichever queue the USB transport delivered the message on. No main-actor bouncing on the hot path.
- All internal state is protected by `Synchronization.Mutex`; writes are serialized on a dedicated `DispatchQueue`.
- Arrival timestamps are captured at the call site (before the write is enqueued), so recorded offsets reflect when a message arrived over USB, not when the writer queue got to it.

### Error Handling

- File creation failures are reported via alerts
- Write errors are logged but do not terminate the session
- Incomplete captures (app crash) still have valid partial data after the header

## Future Enhancements

Potential improvements for future versions:

1. **Compression:** LZ4/ZSTD compression for sampled payloads
2. **Resolution/video-mode tags in header** (firmware + device-info are already captured)
3. **Checksum:** CRC32 or SHA-256 for integrity verification
4. **Replay tool:** GUI app to visualize and replay captured sessions
5. **Export formats:** JSON export (or a PCAP-style container) so the app's own session log can be inspected in standard tooling
