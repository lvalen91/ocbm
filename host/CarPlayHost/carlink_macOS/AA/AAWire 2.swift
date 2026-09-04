// AAWire.swift — Android Auto (GAL) wire layer: transport framing, protobuf codec,
// channel/message-id constants, and the message builders the head unit sends.
//
// Direct port of the proven Rust reference (host/aa-headunit: proto.rs + the framing
// in main.rs), validated live against a Pixel (docs/androidauto/00_ARCHITECTURE.md). Foundation-only so the pure
// parts compile in the hardware-free swiftc test harness.
//
// Transport frame = [channel u8][flags u8][size][payload]
//   flags = frameType(FIRST=1,LAST=2,BULK=3) | encType(PLAIN=0,ENC=8) | msgType(SPECIFIC=0,CONTROL=4)
//   size  = u16 BE (SHORT); a FIRST-only frame is u16 frameLen + u32 total (EXTENDED=6B)
// Message payload = [messageId u16 BE][body]; body is protobuf. Encrypted message ids
// live INSIDE the TLS record.

import Foundation

enum AAWire {

    // MARK: - Frame flag bits

    static let ftFirst: UInt8 = 1 << 0
    static let ftLast: UInt8 = 1 << 1
    static let ftBulk: UInt8 = ftFirst | ftLast // 0x03
    static let encEncrypted: UInt8 = 1 << 3     // 0x08
    static let mtControl: UInt8 = 1 << 2        // 0x04 (SPECIFIC = 0)

    // MARK: - Channel ids (ChannelId enum ordinals)

    static let chControl: UInt8 = 0
    static let chSensor: UInt8 = 1
    /// AA_TELEPHONY_SINK experiment only — see AACapability.telephonySinkChannel. Never declared by
    /// default, so the phone never opens it and nothing routes here.
    static let chTelephonyAudio: UInt8 = 2
    static let chVideo: UInt8 = 3
    static let chMediaAudio: UInt8 = 4
    static let chGuidanceAudio: UInt8 = 5
    static let chSystemAudio: UInt8 = 6
    static let chInput: UInt8 = 8
    static let chMicrophone: UInt8 = 9
    /// Metadata services (2026-09-04). Ids are the head unit's to choose; these are the first free
    /// ones above the A/V, input and mic set. Declared only when `AACapability.metadataServices`.
    static let chMediaPlayback: UInt8 = 10
    static let chNavigationStatus: UInt8 = 11
    static let chPhoneStatus: UInt8 = 12

    /// Human-readable channel kind for logging (AASession service-discovery / channel-open lines).
    /// Purely diagnostic — never consulted for protocol behaviour.
    static func channelName(_ ch: UInt8) -> String {
        switch ch {
        case chControl: return "control"
        case chSensor: return "sensor"
        case chTelephonyAudio: return "telephony_audio"
        case chVideo: return "video"
        case chMediaAudio: return "media_audio"
        case chGuidanceAudio: return "guidance_audio"
        case chSystemAudio: return "system_audio"
        case chInput: return "input"
        case chMicrophone: return "mic"
        case chMediaPlayback: return "media_playback"
        case chNavigationStatus: return "navigation_status"
        case chPhoneStatus: return "phone_status"
        default: return "ch\(ch)"
        }
    }

    // MARK: - Control message ids (ControlMessageType.proto)

    static let msgVersionRequest: UInt16 = 1
    static let msgVersionResponse: UInt16 = 2
    static let msgEncapsulatedSSL: UInt16 = 3
    static let msgAuthComplete: UInt16 = 4
    static let msgServiceDiscoveryRequest: UInt16 = 5
    static let msgServiceDiscoveryResponse: UInt16 = 6
    static let msgChannelOpenRequest: UInt16 = 7
    static let msgChannelOpenResponse: UInt16 = 8
    static let msgPingRequest: UInt16 = 11
    static let msgPingResponse: UInt16 = 12
    static let msgNavFocusRequest: UInt16 = 13
    static let msgNavFocusNotification: UInt16 = 14
    static let msgByebyeRequest: UInt16 = 15
    static let msgByebyeResponse: UInt16 = 16
    static let msgAudioFocusRequest: UInt16 = 18
    static let msgAudioFocusNotification: UInt16 = 19

    // MARK: - Media/AV channel message ids (MediaMessageId.proto)

    static let mediaData: UInt16 = 0            // [timestamp u64 BE][Annex-B]
    static let mediaDataNoTs: UInt16 = 1        // media WITHOUT the timestamp prefix (audio sinks use both)
    static let mediaCodecConfig: UInt16 = 1     // [Annex-B] SPS/PPS, no timestamp
    static let mediaSetup: UInt16 = 32768
    static let mediaStart: UInt16 = 32769
    static let mediaStop: UInt16 = 32770
    static let mediaConfig: UInt16 = 32771
    static let mediaAck: UInt16 = 32772
    /// Mic channel open/close. The mic is NOT driven by MediaStart/Stop like a sink is: gearhead sends
    /// MicrophoneRequest{open} (iww.java:249 open / :138 close) and reads MicrophoneResponse{status,
    /// session_id} back (jaz.java:17 — a non-zero status is how a head unit DECLINES).
    static let micRequest: UInt16 = 32773

    // MARK: - Metadata service message ids (phone -> head unit unless noted)

    /// MediaPlaybackStatusService — gearhead 17.5 sender `jav`: status c=32769, input e=32770
    /// (head unit -> phone), metadata d=32771. Same numbers as aasdk's MediaPlaybackStatusMessageId.
    static let mediaPlaybackStatus: UInt16 = 32769
    static let mediaPlaybackInput: UInt16 = 32770
    static let mediaPlaybackMetadata: UInt16 = 32771
    /// NavigationStatusService (aasdk NavigationStatusMessageId; gearhead's sender is in the Play
    /// Services car module, not in the decompile — unrecognised ids are logged, see AAMetadata).
    static let navClusterStart: UInt16 = 32769
    static let navClusterStop: UInt16 = 32770
    static let navStatus: UInt16 = 32771
    static let navTurnEvent: UInt16 = 32772
    static let navDistanceEvent: UInt16 = 32773
    static let navState: UInt16 = 32774
    static let navCurrentPosition: UInt16 = 32775
    /// PhoneStatusService (aasdk PhoneStatusMessageId).
    static let phoneStatus: UInt16 = 32769
    static let phoneStatusInput: UInt16 = 32770
    static let micResponse: UInt16 = 32774
    static let mediaVideoFocusRequest: UInt16 = 32775
    static let mediaVideoFocusNotification: UInt16 = 32776

    // Sensor channel ids (SensorMessageId.proto)
    static let sensorRequest: UInt16 = 32769
    static let sensorResponse: UInt16 = 32770
    static let sensorBatch: UInt16 = 32771
    static let sensorTypeDrivingStatus: UInt64 = 13
    static let sensorTypeNightMode: UInt64 = 10

    // Input channel ids (InputMessageId.proto)
    static let inputReport: UInt16 = 32769
    static let inputKeyBindingRequest: UInt16 = 32770
    static let inputKeyBindingResponse: UInt16 = 32771
    static let inputActionDown: UInt64 = 0
    static let inputActionUp: UInt64 = 1
    static let inputActionMoved: UInt64 = 2

    // Enum values used in bodies
    static let audioFocusTypeRelease: UInt64 = 4
    static let audioFocusStateGain: UInt64 = 1
    static let audioFocusStateLoss: UInt64 = 3

    // AA protocol version. 1.7 is the ceiling gearhead/the DHU will REPORT
    // (docs/androidauto/00_ARCHITECTURE.md); what we REQUEST is 6.1, deliberately higher — see below.
    /// Protocol version we REQUEST. This is not cosmetic — it selects the phone's encoder profile.
    ///
    /// gearhead stores the pair we ask for as `CarInfo.headUnitProtocolMajor/MinorVersionNumber`
    /// (rth.java:189-217; note it stores the REQUESTED pair, not the negotiated one, and accepts
    /// anything — replying 1.7, or 6.1 if we ask for more). `ivc.java:50` then gates on
    /// `>= (6,0)`, which selects the I-frame interval in `acyp.java:57-65`:
    ///   < 6.0  -> VideoEncoderParams__key_frame_interval_wireless = **60 seconds**
    ///  >= 6.0  -> VideoEncoderParams__key_frame_interval_ackless  = **2 seconds**
    /// At 1.7 a single lost/shed P-frame therefore corrupts the picture for up to a MINUTE, because
    /// the protocol has no keyframe request (verified: no PARAMETER_KEY_REQUEST_SYNC_FRAME anywhere
    /// in the app). The >= 6.0 path also stops the phone parsing our ACKs and floors max_unacked to
    /// 24, and nothing in either source proves the framing is identical at 6.x — so this is an
    /// EXPERIMENT, off by default: `AA_PROTO=6.1` to try it, and A/B before adopting.
    /// DEFAULT 6.1 as of 2026-08-25: device-proven to move the phone's IDR cadence from 60 s to 2 s
    /// (IDRs at frames #61/#121/#181/... = every 60 frames at 30 fps), which is the fix for the
    /// long-lived pixelation. Soaked ~5 minutes / 2700 frames / 45 IDRs / 0 teardowns — solid, but not
    /// hours. Revert with `AA_PROTO=1.7` if anything at 6.x misbehaves.
    static let versionMajor: UInt16 = protoOverride?.0 ?? 6
    static let versionMinor: UInt16 = protoOverride?.1 ?? 1
    private static let protoOverride: (UInt16, UInt16)? = {
        guard let v = ProcessInfo.processInfo.environment["AA_PROTO"] else { return nil }
        let parts = v.split(separator: ".")
        guard parts.count == 2, let a = UInt16(parts[0]), let b = UInt16(parts[1]) else { return nil }
        return (a, b)
    }()

    // MARK: - Protobuf primitives (hand-rolled; matches proto.rs byte-for-byte)

    static func putVarint(_ out: inout Data, _ v0: UInt64) {
        var v = v0
        repeat {
            var b = UInt8(v & 0x7f)
            v >>= 7
            if v != 0 { b |= 0x80 }
            out.append(b)
        } while v != 0
    }

    /// Read a varint; returns (value, newOffset) or nil on truncation.
    static func getVarint(_ buf: Data, _ off0: Int) -> (UInt64, Int)? {
        var v: UInt64 = 0
        var shift: UInt64 = 0
        var off = off0
        while true {
            guard off < buf.count else { return nil }
            let b = buf[buf.startIndex + off]
            off += 1
            v |= UInt64(b & 0x7f) << shift
            if b & 0x80 == 0 { return (v, off) }
            shift += 7
            if shift >= 64 { return nil }
        }
    }

    static func putVarintField(_ out: inout Data, _ field: UInt32, _ v: UInt64) {
        putVarint(&out, UInt64(field) << 3)
        putVarint(&out, v)
    }

    static func putLenField(_ out: inout Data, _ field: UInt32, _ data: Data) {
        putVarint(&out, (UInt64(field) << 3) | 2)
        putVarint(&out, UInt64(data.count))
        out.append(data)
    }

    static func putLenField(_ out: inout Data, _ field: UInt32, _ bytes: [UInt8]) {
        putLenField(&out, field, Data(bytes))
    }

    /// First varint value of a given top-level field number (wiretype 0). Used for
    /// small scalars: Start.session_id, ping timestamp, audio-focus type, sensor type.
    static func getFieldVarint(_ buf: Data, _ field: UInt32) -> UInt64? {
        var off = 0
        while off < buf.count {
            guard let (tag, o1) = getVarint(buf, off) else { return nil }
            off = o1
            let f = UInt32(tag >> 3)
            let wt = tag & 7
            switch wt {
            case 0:
                guard let (v, o2) = getVarint(buf, off) else { return nil }
                off = o2
                if f == field { return v }
            case 2:
                // `len` is peer-supplied and 64-bit: `Int(len)` TRAPS on anything > Int.max, which a
                // 10-byte varint produces, and even an in-range but oversized length would skip past
                // the buffer. This body came off the phone (ping / audio-focus / sensor / mic /
                // media-start), so a malformed one must be an error, never a crash.
                guard let (len, o2) = getVarint(buf, off), len <= UInt64(buf.count - o2) else { return nil }
                off = o2 + Int(len)
            case 5: off += 4
            case 1: off += 8
            default: return nil
            }
        }
        return nil
    }

    /// One decoded top-level protobuf field. Length-delimited payloads are returned as sub-slices;
    /// the caller decides whether that is a string, bytes or a nested message.
    enum ProtoField {
        case varint(UInt32, UInt64)
        case fixed32(UInt32, UInt32)
        case fixed64(UInt32, UInt64)
        case bytes(UInt32, Data)
    }

    /// Walk every top-level field of `buf` in wire order. Stops (returns false) on a malformed tag
    /// or a length that runs past the buffer — peer-supplied bytes, so malformed is an error,
    /// never a crash (same rule as `getFieldVarint`). Used by the metadata services (media
    /// playback / navigation / phone status), whose messages carry repeated and nested fields the
    /// single-scalar getter cannot express.
    @discardableResult
    static func forEachField(_ buf: Data, _ body: (ProtoField) -> Void) -> Bool {
        var off = 0
        while off < buf.count {
            guard let (tag, o1) = getVarint(buf, off) else { return false }
            off = o1
            let f = UInt32(tag >> 3)
            switch tag & 7 {
            case 0:
                guard let (v, o2) = getVarint(buf, off) else { return false }
                off = o2; body(.varint(f, v))
            case 1:
                guard off + 8 <= buf.count else { return false }
                var v: UInt64 = 0
                for i in 0..<8 { v |= UInt64(buf[buf.startIndex + off + i]) << (8 * UInt64(i)) }
                off += 8; body(.fixed64(f, v))
            case 2:
                guard let (len, o2) = getVarint(buf, off), len <= UInt64(buf.count - o2) else { return false }
                let start = buf.startIndex + o2
                body(.bytes(f, buf.subdata(in: start..<(start + Int(len)))))
                off = o2 + Int(len)
            case 5:
                guard off + 4 <= buf.count else { return false }
                var v: UInt32 = 0
                for i in 0..<4 { v |= UInt32(buf[buf.startIndex + off + i]) << (8 * UInt32(i)) }
                off += 4; body(.fixed32(f, v))
            default:
                return false
            }
        }
        return true
    }

    /// First length-delimited value of a top-level field, or nil.
    static func getFieldBytes(_ buf: Data, _ field: UInt32) -> Data? {
        var found: Data?
        forEachField(buf) { if found == nil, case .bytes(field, let d) = $0 { found = d } }
        return found
    }

    /// First length-delimited value of a top-level field decoded as UTF-8, or nil.
    static func getFieldString(_ buf: Data, _ field: UInt32) -> String? {
        getFieldBytes(buf, field).flatMap { String(data: $0, encoding: .utf8) }
    }

    // MARK: - Message builders (port of proto.rs)

    static func authResponseSuccess() -> Data { var o = Data(); putVarintField(&o, 1, 0); return o }
    static func channelOpenResponseOK() -> Data { var o = Data(); putVarintField(&o, 1, 0); return o }
    static func sensorStartResponseOK() -> Data { var o = Data(); putVarintField(&o, 1, 0); return o }
    static func keyBindingResponseOK() -> Data { var o = Data(); putVarintField(&o, 1, 0); return o }

    /// Config { status=READY(2), max_unacked=64, configuration_indices=[0] }.
    /// Validated against gearhead: audio sinks read ONLY max_unacked (>0 or INVALID_ACK_CONFIG,
    /// jdk.java:446); video ALSO requires a non-empty configuration_indices (NO_VIDEO_CONFIGS,
    /// jem.java:267). One shape satisfies both.
    /// max_unacked was 1 (stop-and-wait): the phone would not send video frame N+1 until it received
    /// our ACK for N, so a single delayed/lost video ACK over the higher-latency box relay wedged the
    /// video channel (~2 min in) while audio/control kept flowing. A pipelined window of 8 hides the
    /// ack RTT and tolerates relay jitter (12-agent root-cause analysis, 2026-08-24).
    static func mediaConfigReady() -> Data {
        var o = Data(); putVarintField(&o, 1, 2); putVarintField(&o, 2, 64); putVarintField(&o, 3, 0); return o
    }
    /// VideoFocusNotification { focus, unsolicited }.
    ///
    /// The reference head unit sets `unsolicited=true` when it grants focus spontaneously (e.g. the
    /// grant that follows SETUP) and `false` only when answering a VideoFocusRequest. gearhead just
    /// logs the flag, so this is conformance rather than behaviour.
    /// Modes: PROJECTED=1, NATIVE=2, NATIVE_TRANSIENT=3, PROJECTED_NO_INPUT_FOCUS=4.
    static func videoFocus(_ mode: UInt64, unsolicited: Bool) -> Data {
        var o = Data(); putVarintField(&o, 1, mode); putVarintField(&o, 2, unsolicited ? 1 : 0); return o
    }
    static let videoFocusModeProjected: UInt64 = 1
    static let videoFocusModeNativeTransient: UInt64 = 3
    /// Answer to a VideoFocusRequest.
    static func videoFocusProjected() -> Data { videoFocus(videoFocusModeProjected, unsolicited: false) }
    /// NavFocusNotification { focus_type=NAV_FOCUS_PROJECTED(2) }.
    static func navFocusProjected() -> Data { var o = Data(); putVarintField(&o, 1, 2); return o }
    /// AudioFocusNotification { focus_state, unsolicited=false }.
    static func audioFocusNotification(_ state: UInt64) -> Data {
        var o = Data(); putVarintField(&o, 1, state); putVarintField(&o, 2, 0); return o
    }
    /// PingResponse { timestamp }.
    static func pingResponse(_ ts: UInt64) -> Data { var o = Data(); putVarintField(&o, 1, ts); return o }
    /// ByeByeRequest { reason=USER_SELECTION(1) }.
    static func byebyeRequest() -> Data { var o = Data(); putVarintField(&o, 1, 1); return o }
    /// Ack { session_id, ack=1 }.
    static func mediaAckBody(_ sessionId: Int64) -> Data {
        var o = Data(); putVarintField(&o, 1, UInt64(bitPattern: sessionId)); putVarintField(&o, 2, 1); return o
    }
    /// MicrophoneResponse { status, session_id }. status 0 = mic open and streaming; non-zero = we
    /// decline (denied/busy), which the phone surfaces instead of waiting on a mic that never speaks.
    static func micResponseBody(status: UInt64, sessionId: UInt64) -> Data {
        var o = Data(); putVarintField(&o, 1, status); putVarintField(&o, 2, sessionId); return o
    }
    /// SensorBatch { driving_status_data(13):[{status}] }. `status` is a BITMASK
    /// (`AACapability.DrivingRestrictions`), not a boolean — see that type for why sending 1 was
    /// wrong. The nearest AA analogue of CarPlay's `limitedUI` catalogue.
    static func sensorBatchDriving(_ r: AACapability.DrivingRestrictions) -> Data {
        var dsd = Data(); putVarintField(&dsd, 1, r.rawValue)
        var o = Data(); putLenField(&o, 13, dsd); return o
    }
    /// SensorBatch { night_mode_data(10):[{night_mode}] }.
    static func sensorBatchNight(_ night: Bool) -> Data {
        var nmd = Data(); putVarintField(&nmd, 1, night ? 1 : 0)
        var o = Data(); putLenField(&o, 10, nmd); return o
    }

    /// InputReport { timestamp(1), button_event(4){ button:[{ keycode, is_pressed, meta, long }] } }.
    ///
    /// **Field 4, derived from the one field we have PROVEN.** Touch works at field 3 (device-proven
    /// 2026-08-24), which pins the layout to the variant where `disp_channel = 2`, `touch_event = 3`,
    /// `button_event = 4`. The first cut put this at field 2 and the phone silently ignored every key:
    /// field 2 is an int32 there, so a length-delimited blob was simply discarded — no error, no
    /// effect, which is exactly what the bench saw (30 key events sent, nothing happened).
    static func inputReportKey(timestamp: UInt64, keycode: UInt32, down: Bool,
                               longPress: Bool = false) -> Data {
        var key = Data()
        putVarintField(&key, 1, UInt64(keycode))
        putVarintField(&key, 2, down ? 1 : 0)
        putVarintField(&key, 3, 0)                     // metastate: no modifiers on a head unit
        putVarintField(&key, 4, longPress ? 1 : 0)
        var ke = Data(); putLenField(&ke, 1, key)
        var o = Data()
        putVarintField(&o, 1, timestamp)
        // disp_channel(2) is deliberately NOT set. BOTH plausible values were tried on hardware and
        // neither changed anything: 0 ("the main display") and 3 (our video sink's channel, on the
        // theory that a display is identified by its channel). Left off rather than shipping an
        // unproven field — and recorded here so it is not tried a third time.
        putLenField(&o, 4, ke)
        return o
    }

    /// InputReport { timestamp(1), relative_input_event(6){ relative_input_events:[{scan_code, delta}] } }.
    ///
    /// The rotary detent. It is NOT a button code, despite SCROLL_WHEEL living in the ButtonCode enum:
    /// a working head unit sends it as a RELATIVE event carrying a signed delta, with SCROLL_WHEEL as
    /// the scan_code (openauto `InputService.cpp:146`). One detent = ±1.
    static func inputReportScroll(timestamp: UInt64, delta: Int32) -> Data {
        var ev = Data()
        putVarintField(&ev, 1, UInt64(AACapability.Key.scrollWheel.rawValue))
        // int32 on the wire is a plain varint; negatives are sign-extended to 10 bytes.
        putVarintField(&ev, 2, UInt64(bitPattern: Int64(delta)))
        var evs = Data(); putLenField(&evs, 1, ev)
        var o = Data()
        putVarintField(&o, 1, timestamp); putLenField(&o, 6, evs)
        return o
    }

    /// InputReport { timestamp, touch_event{ pointer{x,y,id}, action_index, action } }.
    static func inputReportTouch(timestamp: UInt64, x: UInt32, y: UInt32, action: UInt64) -> Data {
        var ptr = Data()
        putVarintField(&ptr, 1, UInt64(x)); putVarintField(&ptr, 2, UInt64(y)); putVarintField(&ptr, 3, 0)
        var te = Data()
        putLenField(&te, 1, ptr); putVarintField(&te, 2, 0); putVarintField(&te, 3, action)
        var o = Data()
        putVarintField(&o, 1, timestamp); putLenField(&o, 3, te)
        return o
    }

    /// Full ServiceDiscoveryResponse (the accepted 1.7 service set, docs/androidauto/00_ARCHITECTURE.md):
    /// video(3) + input(8) + sensor(1) + media/guidance/system audio(4/5/6) + mic(9) + headunit_info.
    /// `sinks` is injectable ONLY so the harness can encode both variants of the AA_TELEPHONY_SINK
    /// experiment in one process; production always passes the resolved table.
    static func serviceDiscoveryResponseFull(resolution: UInt32, fps: UInt32, density: UInt32,
                                             widthMargin: UInt32 = 0, heightMargin: UInt32 = 0,
                                             tsW: UInt32, tsH: UInt32,
                                             name: String = "Carlink",
                                             sinks sinkTable: [AACapability.AudioSink] = AACapability.audioSinks,
                                             driverPosition: UInt64 = AACapability.driverPositionLeft,
                                             metadataServices: Bool = false,
                                             hevc: Bool = false) -> Data {
        // video sink id=3
        var vc = Data()
        putVarintField(&vc, 1, UInt64(resolution)); putVarintField(&vc, 2, UInt64(fps))
        // 3/4: codec-frame pixels outside the visible rect (gearhead `iux.c` splits each evenly).
        putVarintField(&vc, 3, UInt64(widthMargin)); putVarintField(&vc, 4, UInt64(heightMargin))
        putVarintField(&vc, 5, UInt64(density))
        // MediaCodecType: 3 = MEDIA_CODEC_VIDEO_H264_BP, 7 = MEDIA_CODEC_VIDEO_H265 (gearhead `xkj`).
        // Declared twice as gal does: MediaSinkService.codec_type (1) and VideoConfiguration.
        // video_codec_type (10); gearhead's 1080p H.264 cap keys off the latter (`ivf.B`).
        let codec: UInt64 = hevc ? 7 : 3
        if hevc { putVarintField(&vc, 10, codec) }
        var mssV = Data(); putVarintField(&mssV, 1, codec); putLenField(&mssV, 4, vc)
        var scVideo = Data(); putVarintField(&scVideo, 1, 3); putLenField(&scVideo, 3, mssV)

        // input id=8: keycodes_supported(1) + touchscreen(2). The keycodes MUST be declared here or
        // the phone has no reason to accept a key event we later send — see AACapability.Key.
        var iss = Data()
        for kc in AACapability.supportedKeycodes { putVarintField(&iss, 1, UInt64(kc)) }
        // AA_NO_TOUCH=1 declares NO touchscreen, making this a controller-only head unit — DHU's
        // `rotary.ini` shape (`touch=false, controller=true`). An experiment, not a shipping mode:
        // with a touchscreen declared, gearhead treats D-Pad focus as secondary and the focus ring
        // behaves inconsistently (device-observed: focus moves, then vanishes). InputChannel has no
        // explicit controller flag, so "controller head unit" is expressed by declaring keycodes and
        // NOT declaring a touchscreen.
        if ProcessInfo.processInfo.environment["AA_NO_TOUCH"] == nil {
            var ts = Data(); putVarintField(&ts, 1, UInt64(tsW)); putVarintField(&ts, 2, UInt64(tsH))
            putLenField(&iss, 2, ts)
        }
        var scInput = Data(); putVarintField(&scInput, 1, 8); putLenField(&scInput, 4, iss)

        // sensor id=1: driving-status + night
        var sss = Data()
        var sDrive = Data(); putVarintField(&sDrive, 1, 13); putLenField(&sss, 1, sDrive)
        var sNight = Data(); putVarintField(&sNight, 1, 10); putLenField(&sss, 1, sNight)
        var scSensor = Data(); putVarintField(&scSensor, 1, 1); putLenField(&scSensor, 2, sss)

        // audio sinks (media/guidance/system) — from the SAME table AASession plays them back
        // with, so a declared format and a played format cannot drift apart.
        let sinks = sinkTable.map {
            audioSink(UInt32($0.channel), $0.streamType, UInt32($0.rate), UInt32($0.channels))
        }
        // + mic source
        var micAc = Data()
        let mic = AACapability.micSource
        putVarintField(&micAc, 1, UInt64(mic.rate))
        putVarintField(&micAc, 2, UInt64(mic.bits))
        putVarintField(&micAc, 3, UInt64(mic.channels))
        var mssMic = Data(); putVarintField(&mssMic, 1, 1); putLenField(&mssMic, 2, micAc)
        var scMic = Data(); putVarintField(&scMic, 1, UInt64(mic.channel)); putLenField(&scMic, 5, mssMic)

        var hui = Data()
        // The head unit's own identity, as the phone displays it. Sourced from the same `name` the
        // CarPlay config carries, so one vehicle profile names the box once for both protocols.
        putLenField(&hui, 1, Array(name.utf8)); putLenField(&hui, 2, Array("OCBM".utf8))
        putLenField(&hui, 5, Array(name.utf8)); putLenField(&hui, 6, Array("carlink-macos".utf8))

        var out = Data()
        putLenField(&out, 1, scVideo)
        putLenField(&out, 1, scInput)
        putLenField(&out, 1, scSensor)
        for sc in sinks { putLenField(&out, 1, sc) }
        putLenField(&out, 1, scMic)
        if metadataServices {
            // ChannelDescriptor field numbers per aasdk Service.proto: navigation_status_service = 8,
            // media_playback_service = 9, phone_status_service = 10. Media and phone configs are
            // empty messages; navigation carries {1 minimum_interval_ms, 2 type (1 = IMAGE),
            // 3 ImageOptions {1 height, 2 width, 3 colour_depth_bits}} — the phone renders the
            // maneuver glyph at this size and ships it in NavigationNextTurnEvent.image.
            var scMedia = Data(); putVarintField(&scMedia, 1, UInt64(chMediaPlayback)); putLenField(&scMedia, 9, Data())
            var img = Data(); putVarintField(&img, 1, 128); putVarintField(&img, 2, 128); putVarintField(&img, 3, 32)
            var navCfg = Data(); putVarintField(&navCfg, 1, 1000); putVarintField(&navCfg, 2, 1); putLenField(&navCfg, 3, img)
            var scNav = Data(); putVarintField(&scNav, 1, UInt64(chNavigationStatus)); putLenField(&scNav, 8, navCfg)
            var scPhone = Data(); putVarintField(&scPhone, 1, UInt64(chPhoneStatus)); putLenField(&scPhone, 10, Data())
            putLenField(&out, 1, scMedia)
            putLenField(&out, 1, scNav)
            putLenField(&out, 1, scPhone)
        }
        putVarintField(&out, 6, driverPosition) // driver_position (gal DriverPosition; see AACapability.driverPosition)
        putLenField(&out, 14, Array("\(name) OCBM".utf8)) // display_name
        putVarintField(&out, 15, 0) // probe_for_support = false
        putLenField(&out, 17, hui) // headunit_info
        return out
    }

    /// One audio-playback MediaSinkService ServiceConfiguration (PCM).
    static func audioSink(_ id: UInt32, _ audioType: UInt32, _ rate: UInt32, _ channels: UInt32) -> Data {
        var ac = Data()
        putVarintField(&ac, 1, UInt64(rate)); putVarintField(&ac, 2, 16); putVarintField(&ac, 3, UInt64(channels))
        var mss = Data(); putVarintField(&mss, 1, 1); putVarintField(&mss, 2, UInt64(audioType)); putLenField(&mss, 3, ac)
        var sc = Data(); putVarintField(&sc, 1, UInt64(id)); putLenField(&sc, 3, mss)
        return sc
    }

    // MARK: - Frame encode/decode

    /// Build one BULK frame: [channel][flags][u16 BE len][payload]. `payload` is the full
    /// message payload ([messageId][body]) — already TLS-encrypted iff `encrypted`.
    static func encodeFrame(channel: UInt8, encrypted: Bool, control: Bool, payload: Data) -> Data {
        var flags = ftBulk
        if encrypted { flags |= encEncrypted }
        if control { flags |= mtControl }
        var f = Data()
        f.append(channel); f.append(flags)
        let len = UInt16(payload.count)
        f.append(UInt8(len >> 8)); f.append(UInt8(len & 0xff))
        f.append(payload)
        return f
    }

    /// Split a message payload into (messageId, body). Returns (0, empty) if <2 bytes.
    static func splitMessageId(_ p: Data) -> (UInt16, Data) {
        guard p.count >= 2 else { return (0, Data()) }
        let a = p[p.startIndex], b = p[p.startIndex + 1]
        let id = (UInt16(a) << 8) | UInt16(b)
        return (id, p.subdata(in: (p.startIndex + 2)..<p.endIndex))
    }
}
