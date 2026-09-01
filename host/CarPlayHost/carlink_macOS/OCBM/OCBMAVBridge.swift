// OCBMAVBridge.swift — glue between the decrypted OCBM A/V and the reused decode/render layers.
//
// OCBMAVDecrypt emits plaintext AVCC video frames + a plaintext avcC codec-config, plus decrypted media
// PCM. The `VideoDecoder` consumes AVCC directly (V1 fast path, 2026-08): iOS always sends 4-byte NAL
// length prefixes, so the plaintext is byte-identical to the CMBlockBuffer payload VideoToolbox needs —
// the bridge forwards it zero-copy. Only when the box refines `nalLengthSize` to 1/2/3 (from the avcC)
// does this lane rewrite the prefixes to 4 bytes (still no Annex-B intermediate). The parameter sets are
// parsed once at config time and handed to the decoder, which re-seeds its format from them after a
// flush — there is no per-keyframe in-band SPS/PPS prepend. Audio PCM goes straight to `AudioPlayer`.
//
// The bridge owns the VideoDecoder; its `displayLayer` is the render surface the window hosts.

import Foundation
import os

/// One video decode lane: a dedicated VideoDecoder + its parameter-set state + the avcC/hvcC parse.
/// The main CarPlay screen and the ALT / navigation (cluster) screen each get their own lane, so
/// they decode fully independently (a fault in one screen can't corrupt the other). Dual-codec
/// (H.264 + HEVC) with the FourCC guard and pre-warm, per lane.
final class VideoLane {
    let decoder: VideoDecoder
    private let label: String
    private let log = Logger(subsystem: "com.carlink.ocbm", category: "av-lane")

    private var configValid = false
    private var nalLengthSize = 4

    /// Fired when the codec is (re)determined from the config parse — the authoritative HEVC-vs-H.264
    /// source. Wired to the metrics so the monitor can surface the per-lane codec.
    var onCodec: ((VideoCodec) -> Void)?

    init(decoder: VideoDecoder, label: String) {
        self.decoder = decoder
        self.label = label
    }

    func receiveConfig(_ config: Data) {
        let raw = [UInt8](config)
        // iOS sends the HEVC config as a full ISO hvc1/hev1 VisualSampleEntry; H.264 as a bare avcC.
        let b = Self.extractHvcC(fromSampleEntry: raw) ?? raw
        if let (s, p, lenSize) = Self.parseAvcC(b) {
            nalLengthSize = lenSize; configValid = true
            log.info("[\(self.label, privacy: .public)] avcC parsed: SPS \(s.count)B, PPS \(p.count)B, nalLen \(lenSize)")
            decoder.configure(codec: .h264, parameterSets: [Data(s), Data(p)])
            onCodec?(.h264)
        } else if let (v, s, p, lenSize) = Self.parseHvcC(b) {
            nalLengthSize = lenSize; configValid = true
            log.info("[\(self.label, privacy: .public)] hvcC parsed: VPS \(v.count)B, SPS \(s.count)B, PPS \(p.count)B, nalLen \(lenSize)")
            decoder.configure(codec: .hevc, parameterSets: [Data(v), Data(s), Data(p)])
            onCodec?(.hevc)
        } else {
            configValid = false
            let head = b.prefix(96).map { String(format: "%02x", $0) }.joined()
            log.error("[\(self.label, privacy: .public)] video config (\(b.count)B) neither avcC nor hvcC — dropping frames; head=\(head, privacy: .public)")
        }
    }

    func receiveFrame(_ avcc: Data, keyframe: Bool) {
        guard configValid else { return } // FourCC guard: never feed frames under an unknown config
        // V1 fast path: iOS sends 4-byte NAL length prefixes, so the plaintext AVCC IS the CMBlockBuffer
        // payload — forward it zero-copy. Only when the box refined nalLengthSize to 1/2/3 do we rewrite
        // the prefixes to 4 bytes (no Annex-B intermediate). The decoder handles parameter sets.
        if nalLengthSize == 4 {
            decoder.decodeAndDisplay(avcc: avcc, keyframe: keyframe)
        } else if let fixed = AVCCFastPath.rewriteToFourByteLengths(avcc, lenSize: nalLengthSize) {
            decoder.decodeAndDisplay(avcc: fixed, keyframe: keyframe)
        }
        // else: malformed AU under the current length size → drop (a keyframe will re-sync).
    }

    // MARK: - Codec-config parsers

    /// Unwrap an ISO `hvc1`/`hev1` VisualSampleEntry (what iOS sends as the HEVC session config —
    /// observed live 2026-07-12) and return the payload of its `hvcC` child box.
    private static func extractHvcC(fromSampleEntry b: [UInt8]) -> [UInt8]? {
        // [0..4] box size (== total length), [4..8] fourcc 'hvc1' or 'hev1'
        guard b.count > 94 else { return nil }
        let boxSize = (Int(b[0]) << 24) | (Int(b[1]) << 16) | (Int(b[2]) << 8) | Int(b[3])
        let fourcc = String(bytes: b[4..<8], encoding: .ascii)
        guard boxSize >= 94, boxSize <= b.count, fourcc == "hvc1" || fourcc == "hev1" else { return nil }
        var i = 86
        while i + 8 <= boxSize {
            let size = (Int(b[i]) << 24) | (Int(b[i + 1]) << 16) | (Int(b[i + 2]) << 8) | Int(b[i + 3])
            guard size >= 8, i + size <= boxSize else { return nil }
            if b[i + 4] == 0x68, b[i + 5] == 0x76, b[i + 6] == 0x63, b[i + 7] == 0x43 { // 'hvcC'
                return Array(b[(i + 8)..<(i + size)])
            }
            i += size
        }
        return nil
    }

    /// avcC box: [0]=version(1) [1..3]=profile/compat/level [4]=0xFC|(lenSizeMinusOne)
    /// [5]=0xE0|numSPS, per SPS [u16 len][bytes], then numPPS, per PPS [u16 len][bytes].
    /// Returns nil unless the first SPS really is an H.264 SPS (forbidden_zero_bit 0, NAL type 7).
    private static func parseAvcC(_ b: [UInt8]) -> (sps: [UInt8], pps: [UInt8], lenSize: Int)? {
        guard b.count >= 7, b[0] == 1 else { return nil }
        let lenSize = Int(b[4] & 0x03) + 1
        var i = 5
        var sps: [UInt8]?
        var pps: [UInt8]?
        let numSPS = Int(b[i] & 0x1F); i += 1
        for _ in 0..<numSPS {
            guard i + 2 <= b.count else { return nil }
            let len = (Int(b[i]) << 8) | Int(b[i + 1]); i += 2
            guard len > 0, i + len <= b.count else { return nil }
            sps = Array(b[i..<i + len]); i += len
        }
        guard i < b.count else { return nil }
        let numPPS = Int(b[i]); i += 1
        for _ in 0..<numPPS {
            guard i + 2 <= b.count else { return nil }
            let len = (Int(b[i]) << 8) | Int(b[i + 1]); i += 2
            guard len > 0, i + len <= b.count else { return nil }
            pps = Array(b[i..<i + len]); i += len
        }
        guard let s = sps, let p = pps,
              s.first.map({ $0 & 0x80 == 0 && $0 & 0x1F == 7 }) == true, // H.264 SPS
              p.first.map({ $0 & 0x80 == 0 && $0 & 0x1F == 8 }) == true  // H.264 PPS
        else { return nil }
        return (s, p, lenSize)
    }

    /// hvcC box: 22 fixed header bytes ([21] low 2 bits = lenSizeMinusOne), [22]=numOfArrays, then per
    /// array [0]=completeness|NAL type, [1..2]=numNalus BE, per NALU [u16 len][bytes].
    /// Returns nil unless VPS(32)+SPS(33)+PPS(34) are all present with matching NAL headers.
    private static func parseHvcC(_ b: [UInt8]) -> (vps: [UInt8], sps: [UInt8], pps: [UInt8], lenSize: Int)? {
        guard b.count >= 23, b[0] == 1 else { return nil }
        let lenSize = Int(b[21] & 0x03) + 1
        var i = 22
        let numArrays = Int(b[i]); i += 1
        var vps: [UInt8]?
        var sps: [UInt8]?
        var pps: [UInt8]?
        for _ in 0..<numArrays {
            guard i + 3 <= b.count else { return nil }
            let nalType = b[i] & 0x3F; i += 1
            let numNalus = (Int(b[i]) << 8) | Int(b[i + 1]); i += 2
            for _ in 0..<numNalus {
                guard i + 2 <= b.count else { return nil }
                let len = (Int(b[i]) << 8) | Int(b[i + 1]); i += 2
                guard len > 0, i + len <= b.count else { return nil }
                let nal = Array(b[i..<i + len]); i += len
                // The NALU's own header type must match the array's declared type.
                guard let h = nal.first, (h >> 1) & 0x3F == nalType else { return nil }
                switch nalType {
                case 32: vps = nal
                case 33: sps = nal
                case 34: pps = nal
                default: break // SEI arrays etc — fine, ignored
                }
            }
        }
        guard let v = vps, let s = sps, let p = pps else { return nil }
        return (v, s, p, lenSize)
    }
}

/// Bridges decrypted OCBM A/V into the decode/render layers. Two independent video lanes — the main
/// CarPlay screen and the ALT / navigation (cluster) screen — plus the shared audio player.
final class OCBMAVBridge: OCBMAVDelegate {
    let main: VideoLane
    let alt: VideoLane
    private let audio: AudioPlayer
    private let log = Logger(subsystem: "com.carlink.ocbm", category: "av-bridge")

    /// Fired (on altVideoQueue, the alt-lane decrypt queue) for every alt-video frame — drives the floating window's
    /// appear-on-arrival / hide-on-idle behavior. The handler must hop to the main actor itself.
    var onAltFrame: (() -> Void)?

    init(decoder: VideoDecoder, altDecoder: VideoDecoder, audio: AudioPlayer) {
        self.main = VideoLane(decoder: decoder, label: "main")
        self.alt = VideoLane(decoder: altDecoder, label: "alt")
        self.audio = audio
    }

    // MARK: - OCBMAVDelegate (video)
    func avDidReceiveVideoConfig(_ config: Data) { main.receiveConfig(config) }
    func avDidReceiveVideoFrame(_ avcc: Data, keyframe: Bool) { main.receiveFrame(avcc, keyframe: keyframe) }
    func avDidReceiveAltVideoConfig(_ config: Data) { alt.receiveConfig(config) }
    func avDidReceiveAltVideoFrame(_ avcc: Data, keyframe: Bool) {
        alt.receiveFrame(avcc, keyframe: keyframe)
        onAltFrame?()   // appear-on-arrival for the floating Nav/Alt window
    }

    // MARK: - Audio (all-rates / all-streams)

    // Per-scid decoders for compressed (wireless) codecs, created on SEAM_FORMAT so the converter is
    // warmed before the first AU. Wired streams are PCM → this map stays empty on the wired transport.
    private var compressedDecoders: [UInt64: CompressedAudioDecoder] = [:]

    func avDidReceiveAudioFormat(scid: UInt64, format: OCBMAudioStreamFormat) {
        if format.isPCM {
            compressedDecoders[scid] = nil
            log.info("audio scid=\(scid) PCM \(Int(format.sampleRate))Hz \(format.channels)ch atype=\(format.audioType) — pre-warmed node ready")
        } else if let dec = CompressedAudioDecoder(
            codec: format.codec, sampleRate: format.sampleRate, channels: format.channels) {
            compressedDecoders[scid] = dec
            log.info("audio scid=\(scid) compressed codec=\(format.codec) — decoder prestaged")
        } else {
            log.error("audio scid=\(scid) codec=\(format.codec) unsupported — AUs will be dropped")
        }
    }

    func avDidReceiveAudio(_ au: Data, scid: UInt64, format: OCBMAudioStreamFormat?) {
        // No SEAM_FORMAT (legacy box build): wired media PCM 48k/16/2 big-endian.
        guard let fmt = format else {
            audio.feedMediaPCM(au)
            return
        }
        if fmt.isPCM {
            audio.feedPCM(au, rate: Int(fmt.sampleRate), channels: Int(fmt.channels),
                          voice: fmt.isVoice, bigEndian: true)
        } else if let dec = compressedDecoders[scid], let pcm = dec.decode(au) {
            // Decoded output is host-endian at the stream's rate/channels.
            audio.feedPCM(pcm, rate: Int(dec.sampleRate), channels: Int(dec.channels),
                          voice: fmt.isVoice, bigEndian: false)
        }
    }
}
