// AirPlayTunnel.swift — the two media lanes of AirPlay-only mode: reassembly, key handling, decrypt.
// The Swift twin of `Capture::{video_msg, audio_msg}` in host/ocbm-host/src/airplay.rs, with the same
// rules (docs/ocbm/00_PROTOCOL.md "AirPlay-only mode", docs/ocbm/02_AIRPLAY_ONLY_MODE.md §4.6):
//
//   CH_VIDEO        0x00 key   [key 32][scid u64 LE]
//                   0x01 frame [seq u64 LE][hdr 128][body]
//                     opcode 0  ChaCha20-Poly1305, nonce = 00 00 00 00 | seq (FROM THE WIRE), AAD = hdr
//                     opcode 1  plaintext codec config (avcC, or an hvc1 box with an hvcC), once at
//                               stream start, not counted in seq
//                     opcode 5  plaintext statistics, not counted in seq
//   CH_MEDIA_AUDIO  SEAM_KEY    [key 32][scid u64 LE]
//                   SEAM_FORMAT [scid u64 LE][codec][rate u32 LE][ch][bits][audio_type]
//                   SEAM_PKT    [scid u64 LE][RTP 12][ciphertext][tag 16][nonce 8], AAD = RTP 4..12
//
// The AEAD itself is the shipping app's (`OCBMAVDecrypt.decryptVideoFrame` / `.decryptAudio`), so this
// file adds no cryptography of its own. Synchronous and queue-confined: feed a lane from one queue and
// take its events from the sink on that same queue. Key bytes never leave this file except as a
// `SymmetricKey`, and are never logged.

import CryptoKit
import Foundation
import Synchronization

/// The 128-byte plaintext header in front of every screen frame body (little-endian).
struct ScreenHeader: Equatable {
    static let length = 128
    var bodySize: UInt32
    var opcode: UInt8
    var flags: UInt8
    /// 32.32 fixed-point seconds on the phone's network clock.
    var timestamp: UInt64
    /// Opcode-1 floats: source size @16/@20, x offset @48, output size @56/@60. Zero when not sent.
    var sourceWidth: Float
    var sourceHeight: Float
    var offsetX: Float
    var outputWidth: Float
    var outputHeight: Float

    var isKeyframeFlagged: Bool { flags & 0x10 != 0 }
    var timestampSeconds: Double { Double(timestamp) / 4_294_967_296.0 }

    /// "498x1080", or nil when the floats are absent or not a plausible size (a sender that leaves the
    /// header zeroed, or a layout this build does not know).
    var sourceSizeText: String? { Self.sizeText(sourceWidth, sourceHeight) }
    var outputSizeText: String? { Self.sizeText(outputWidth, outputHeight) }

    init?(_ h: ArraySlice<UInt8>) {
        guard h.count >= Self.length else { return nil }
        let b = h.startIndex
        func u32(_ o: Int) -> UInt32 {
            UInt32(h[b + o]) | UInt32(h[b + o + 1]) << 8 | UInt32(h[b + o + 2]) << 16 | UInt32(h[b + o + 3]) << 24
        }
        func f32(_ o: Int) -> Float { Float(bitPattern: u32(o)) }
        bodySize = u32(0)
        opcode = h[b + 4]
        flags = h[b + 5]
        timestamp = UInt64(u32(8)) | UInt64(u32(12)) << 32
        sourceWidth = f32(16)
        sourceHeight = f32(20)
        offsetX = f32(48)
        outputWidth = f32(56)
        outputHeight = f32(60)
    }

    private static func sizeText(_ w: Float, _ h: Float) -> String? {
        guard w.isFinite, h.isFinite, w >= 16, h >= 16, w <= 16384, h <= 16384 else { return nil }
        return "\(Int(w))x\(Int(h))"
    }
}

enum AirPlayVideoEvent {
    /// A key message. `fresh` = a new key or scid: a new stream on a new data connection, so the decoder
    /// state is void and `seq` restarts. Not fresh = the box re-sent the live key after a seam reconnect.
    case key(scid: UInt64, fresh: Bool)
    /// Opcode 1: the codec configuration body, verbatim (an avcC, or an 'hvc1' box wrapping an hvcC:
    /// `MirrorVideoConfig.parse`), with the header that carried it.
    case config(record: [UInt8], header: ScreenHeader)
    /// Opcode 0, decrypted: length-prefixed NAL units.
    case frame(seq: UInt64, data: Data, header: ScreenHeader)
    /// `seq` jumped: frames were lost before this one. No keyframe request exists in this mode.
    case gap(expected: UInt64, got: UInt64)
    /// Opcode 0 that did not authenticate (or arrived before any key).
    case undecryptable(seq: UInt64, haveKey: Bool)
}

struct AirPlayVideoStats: Equatable, Sendable {
    var keyMessages: UInt64 = 0
    var streams: UInt64 = 0
    var decrypted: UInt64 = 0
    var failed: UInt64 = 0
    var noKey: UInt64 = 0
    var flaggedIDR: UInt64 = 0
    var configs: UInt64 = 0
    var other: UInt64 = 0
    var gaps: UInt64 = 0
    var framesLost: UInt64 = 0
    var plainBytes: UInt64 = 0
    var laneBytes: UInt64 = 0
    var resyncs: UInt64 = 0
}

struct AirPlayVideoLane {
    static let maxMessage = 16 * 1024 * 1024

    private var reader = SeamReader(max: maxMessage)
    private var key: SymmetricKey?
    private var keyBytes: [UInt8] = []
    private(set) var scid: UInt64?
    private var nextSeq: UInt64?
    private(set) var stats = AirPlayVideoStats()

    /// Drop everything: the link went away, or the mode was left.
    mutating func reset() {
        reader = SeamReader(max: Self.maxMessage)
        key = nil
        keyBytes = []
        scid = nil
        nextSeq = nil
        stats = AirPlayVideoStats()
    }

    mutating func feed(_ payload: [UInt8], newSource: Bool, sink: (AirPlayVideoEvent) -> Void) {
        stats.laneBytes += UInt64(payload.count)
        reader.push(payload, newSource: newSource)
        while let m = reader.next() {
            handle(m, sink)
        }
        stats.resyncs = reader.resyncs
    }

    private mutating func handle(_ m: SeamMessage, _ sink: (AirPlayVideoEvent) -> Void) {
        switch m.marker {
        case OCBM.seamKey where m.body.count == 40:
            let k = Array(m.body[0..<32])
            let s = OCBM.readLE64(m.body, 32)
            let fresh = k != keyBytes || s != scid
            stats.keyMessages += 1
            if fresh {
                stats.streams += 1
                key = SymmetricKey(data: k)
                keyBytes = k
                scid = s
                nextSeq = nil // a new data connection: the counter starts over
            }
            sink(.key(scid: s, fresh: fresh))
        case 0x01 where m.body.count >= 8 + ScreenHeader.length:
            let seq = OCBM.readLE64(m.body, 0)
            let hdr = m.body[8..<(8 + ScreenHeader.length)]
            let body = m.body[(8 + ScreenHeader.length)...]
            guard let header = ScreenHeader(hdr) else { return }
            switch header.opcode {
            case 0:
                if let want = nextSeq, want != seq {
                    stats.gaps += 1
                    if seq > want { stats.framesLost += seq - want }
                    sink(.gap(expected: want, got: seq))
                }
                nextSeq = seq &+ 1
                // The nonce is the WIRE seq, never a local count: a host that counts desyncs at the first gap.
                guard let key else {
                    stats.noKey += 1
                    sink(.undecryptable(seq: seq, haveKey: false))
                    return
                }
                guard let plain = OCBMAVDecrypt.decryptVideoFrame(hdr: hdr, body: body, key: key, counter: seq) else {
                    stats.failed += 1
                    sink(.undecryptable(seq: seq, haveKey: true))
                    return
                }
                stats.decrypted += 1
                stats.plainBytes += UInt64(plain.count)
                if header.isKeyframeFlagged { stats.flaggedIDR += 1 }
                sink(.frame(seq: seq, data: plain, header: header))
            case 1:
                stats.configs += 1
                sink(.config(record: Array(body), header: header))
            default:
                stats.other += 1 // opcode 5: the phone's plaintext statistics
            }
        default:
            break
        }
    }
}

/// What a packet's own bytes say about its stream type. AirPlay sends type 96 as RTP PT 96 with a 16-bit
/// sequence and SSRC 0, and type 103 as `0x80` + a 24-bit sequence with SSRC `0x16000000` (§4.6).
enum AirPlayAudioKind: Equatable, Sendable {
    case realtime   // type 96 (UDP, with mirroring)
    case buffered   // type 103 (TCP, ahead of real time)

    var streamType: Int { self == .realtime ? 96 : 103 }

    /// The packet's sequence number: RTP bytes 2..4 (16 bits) on type 96, bytes 1..4 (24 bits) on type 103.
    func sequence(rtpHead: UInt32) -> UInt32 { self == .realtime ? rtpHead & 0xFFFF : rtpHead & 0xFF_FFFF }
    /// The modulus of that sequence space.
    var sequenceModulus: UInt32 { self == .realtime ? 1 << 16 : 1 << 24 }

    /// `declaredType` = the SETUP `type` from the stream's `start` event when it has arrived; it wins.
    static func classify(rtp: ArraySlice<UInt8>, declaredType: Int?) -> AirPlayAudioKind {
        if declaredType == 96 { return .realtime }
        if declaredType == 103 { return .buffered }
        let b = rtp.startIndex
        guard rtp.count >= 12 else { return .realtime }
        let ssrcZero = rtp[b + 8] == 0 && rtp[b + 9] == 0 && rtp[b + 10] == 0 && rtp[b + 11] == 0
        return (rtp[b + 1] & 0x7f) == 96 && ssrcZero ? .realtime : .buffered
    }
}

struct AirPlayAudioFormat: Equatable, Sendable {
    var codec: UInt8 = OCBM.seamCodecAacLc
    var sampleRate: UInt32 = 44100
    var channels: UInt8 = 2

    /// What the measurements found on every stream; assumed until a `SEAM_FORMAT` says otherwise.
    static let measuredDefault = AirPlayAudioFormat()

    var codecName: String {
        switch codec {
        case OCBM.seamCodecPcm: return "PCM"
        case OCBM.seamCodecAacLc: return "AAC-LC"
        case OCBM.seamCodecAacEld: return "AAC-ELD"
        case OCBM.seamCodecOpus: return "Opus"
        default: return "codec \(codec)"
        }
    }
}

enum AirPlayAudioEvent {
    /// `generation` rises with every fresh key on the lane: the newest stream has the highest.
    case key(scid: UInt64, fresh: Bool, generation: UInt64)
    case format(scid: UInt64, format: AirPlayAudioFormat)
    /// One decrypted raw access unit (never empty). `rtpHead` = RTP bytes 0..4 big-endian, from which
    /// `AirPlayAudioKind.sequence(rtpHead:)` takes the sequence number once the stream type is settled;
    /// `wireKind` = what the packet's own bytes say.
    case packet(scid: UInt64, generation: UInt64, au: Data, rtpHead: UInt32, rtpTime: UInt32, wireKind: AirPlayAudioKind)
}

struct AirPlayAudioStreamStats: Equatable, Sendable {
    var scid: UInt64
    var generation: UInt64 = 0
    var decrypted: UInt64 = 0
    var failed: UInt64 = 0
    var empty: UInt64 = 0
    var noKey: UInt64 = 0
    var plainBytes: UInt64 = 0
}

struct AirPlayAudioLane {
    static let maxMessage = 1024 * 1024
    /// Streams remembered at once. A session sets up a handful; the oldest is forgotten first.
    static let maxStreams = 8

    private struct Stream {
        var key: SymmetricKey?
        var keyBytes: [UInt8] = []
        var format: AirPlayAudioFormat?
        var stats: AirPlayAudioStreamStats
    }

    private var reader = SeamReader(max: maxMessage)
    private var streams: [UInt64: Stream] = [:]
    private var order: [UInt64] = []
    /// Process-wide, so a stream keyed later always outranks one keyed earlier, also across lanes (a
    /// new client after a lost link starts a new lane) and across resets.
    private static let generations = Atomic<UInt64>(0)
    private(set) var laneBytes: UInt64 = 0
    private(set) var resyncs: UInt64 = 0

    mutating func reset() {
        reader = SeamReader(max: Self.maxMessage)
        streams = [:]
        order = []
        laneBytes = 0
        resyncs = 0
    }

    /// Per-stream tallies, oldest stream first.
    var stats: [AirPlayAudioStreamStats] { order.compactMap { streams[$0]?.stats } }

    func format(of scid: UInt64) -> AirPlayAudioFormat? { streams[scid]?.format }

    mutating func feed(_ payload: [UInt8], newSource: Bool, sink: (AirPlayAudioEvent) -> Void) {
        laneBytes += UInt64(payload.count)
        reader.push(payload, newSource: newSource)
        while let m = reader.next() {
            handle(m, sink)
        }
        resyncs = reader.resyncs
    }

    private mutating func stream(_ scid: UInt64) -> Stream {
        if let s = streams[scid] { return s }
        order.append(scid)
        if order.count > Self.maxStreams {
            streams[order.removeFirst()] = nil
        }
        return Stream(stats: AirPlayAudioStreamStats(scid: scid))
    }

    private mutating func handle(_ m: SeamMessage, _ sink: (AirPlayAudioEvent) -> Void) {
        switch m.marker {
        case OCBM.seamKey where m.body.count == 40:
            let k = Array(m.body[0..<32])
            let scid = OCBM.readLE64(m.body, 32)
            var s = stream(scid)
            let fresh = s.keyBytes != k
            if fresh {
                s.key = SymmetricKey(data: k)
                s.keyBytes = k
                s.stats.generation = Self.generations.wrappingAdd(1, ordering: .relaxed).newValue
            }
            streams[scid] = s
            sink(.key(scid: scid, fresh: fresh, generation: s.stats.generation))
        case OCBM.seamFormat where m.body.count == 16:
            let scid = OCBM.readLE64(m.body, 0)
            let f = AirPlayAudioFormat(codec: m.body[8], sampleRate: OCBM.readLE32(m.body, 9), channels: m.body[13])
            var s = stream(scid)
            let changed = s.format != f
            s.format = f
            streams[scid] = s
            if changed { sink(.format(scid: scid, format: f)) }
        case OCBM.seamPkt where m.body.count >= 8 + 12 + 16 + 8:
            let scid = OCBM.readLE64(m.body, 0)
            var s = stream(scid)
            defer { streams[scid] = s }
            guard let key = s.key else {
                s.stats.noKey += 1
                return
            }
            let pkt = Array(m.body[8...])
            guard let au = OCBMAVDecrypt.decryptAudio(pkt: pkt, key: key) else {
                s.stats.failed += 1
                return
            }
            if au.isEmpty {
                s.stats.empty += 1 // measured: about one per session, nothing to decode
                return
            }
            s.stats.decrypted += 1
            s.stats.plainBytes += UInt64(au.count)
            let kind = AirPlayAudioKind.classify(rtp: pkt[0..<12], declaredType: nil)
            let head = UInt32(pkt[0]) << 24 | UInt32(pkt[1]) << 16 | UInt32(pkt[2]) << 8 | UInt32(pkt[3])
            let ts = UInt32(pkt[4]) << 24 | UInt32(pkt[5]) << 16 | UInt32(pkt[6]) << 8 | UInt32(pkt[7])
            sink(.packet(scid: scid, generation: s.stats.generation, au: au, rtpHead: head, rtpTime: ts, wireKind: kind))
        default:
            break
        }
    }
}
