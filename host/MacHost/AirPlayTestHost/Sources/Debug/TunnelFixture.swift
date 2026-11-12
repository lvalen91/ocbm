// TunnelFixture.swift — DEBUG ONLY: the SENDING side of the AirPlay-only tunnel, for the unit tests and
// the synthetic box. It does what the phone and the box do between them (encrypt per frame / per
// packet, wrap as seam messages, split into OCBM frames), mirroring box/airplayd/examples/fake_sender.rs
// and the tests in host/ocbm-host/src/airplay.rs, so the app's receive path can be exercised byte for
// byte with no hardware. Compiled out of release builds.

#if DEBUG

import CoreGraphics
import CryptoKit
import Foundation
import ImageIO

enum TunnelFixture {
    // MARK: - Crypto and seam framing

    /// ChaCha20-Poly1305, nonce = 00 00 00 00 | `nonce8`: returns ciphertext | tag.
    static func seal(key: [UInt8], nonce8: [UInt8], aad: [UInt8], plain: [UInt8]) -> [UInt8] {
        let nonce = try! ChaChaPoly.Nonce(data: [0, 0, 0, 0] + nonce8)
        let box = try! ChaChaPoly.seal(plain, using: SymmetricKey(data: key), nonce: nonce, authenticating: aad)
        return Array(box.ciphertext) + Array(box.tag)
    }

    static func le64(_ v: UInt64) -> [UInt8] { (0..<8).map { UInt8((v >> (8 * UInt64($0))) & 0xff) } }
    static func le32(_ v: UInt32) -> [UInt8] { (0..<4).map { UInt8((v >> (8 * UInt32($0))) & 0xff) } }
    static func be32(_ v: UInt32) -> [UInt8] { le32(v).reversed() }

    /// `[u32 BE len]["SEAV"][marker][body]`.
    static func seam(_ marker: UInt8, _ body: [UInt8]) -> [UInt8] {
        be32(UInt32(5 + body.count)) + SeamReader.magic + [marker] + body
    }

    /// The key message of both lane families: `[key 32][scid u64 LE]`, the scid TRAILING the key.
    static func keyMessage(key: [UInt8], scid: UInt64) -> [UInt8] { seam(OCBM.seamKey, key + le64(scid)) }

    struct Geometry {
        var sourceWidth: Float = 498
        var sourceHeight: Float = 1080
        var offsetX: Float = 711
        var outputWidth: Float = 498
        var outputHeight: Float = 1080
    }

    /// The 128-byte screen header: bodySize @0, opcode @4, flags @5, 32.32 timestamp @8, and on a codec
    /// frame the floats the measurements found (source @16/@20 and @40/@44, x offset @48, output @56/@60).
    static func screenHeader(opcode: UInt8, flags: UInt8, bodySize: Int, timestamp: UInt64, geometry: Geometry? = nil) -> [UInt8] {
        var h = [UInt8](repeating: 0, count: ScreenHeader.length)
        h.replaceSubrange(0..<4, with: le32(UInt32(bodySize)))
        h[4] = opcode
        h[5] = flags
        h.replaceSubrange(8..<16, with: le64(timestamp))
        if let g = geometry {
            h[6] = 0x16
            h[7] = 0x01
            for (off, v) in [(16, g.sourceWidth), (20, g.sourceHeight), (40, g.sourceWidth), (44, g.sourceHeight),
                             (48, g.offsetX), (56, g.outputWidth), (60, g.outputHeight)] {
                h.replaceSubrange(off..<(off + 4), with: le32(v.bitPattern))
            }
        }
        return h
    }

    /// One CH_VIDEO frame message as the box writes it. Opcode 0 is sealed under `key` with `seq` as the
    /// nonce and the header as AAD; every other opcode is plaintext.
    static func videoFrame(key: [UInt8], seq: UInt64, opcode: UInt8, flags: UInt8 = 0, plain: [UInt8],
                           timestamp: UInt64 = 0, geometry: Geometry? = nil) -> [UInt8] {
        let bodySize = opcode == 0 ? plain.count + 16 : plain.count
        let hdr = screenHeader(opcode: opcode, flags: flags, bodySize: bodySize, timestamp: timestamp, geometry: geometry)
        let body = opcode == 0 ? seal(key: key, nonce8: le64(seq), aad: hdr, plain: plain) : plain
        return seam(0x01, le64(seq) + hdr + body)
    }

    static func audioFormat(scid: UInt64, codec: UInt8 = OCBM.seamCodecAacLc, rate: UInt32 = 44100, channels: UInt8 = 2) -> [UInt8] {
        seam(OCBM.seamFormat, le64(scid) + [codec] + le32(rate) + [channels, 0, 0])
    }

    /// One CH_MEDIA_AUDIO packet message: `[RTP 12][ciphertext][tag 16][nonce 8]`, AAD = RTP 4..12.
    /// Type 96: PT 96, 16-bit sequence, SSRC 0. Type 103: `0x80`, 24-bit sequence, SSRC `0x16000000`.
    static func audioPacket(key: [UInt8], scid: UInt64, kind: AirPlayAudioKind, sequence: UInt32, rtpTime: UInt32,
                            nonce: UInt64, au: [UInt8]) -> [UInt8] {
        var rtp: [UInt8]
        switch kind {
        case .realtime:
            rtp = [0x80, 96, UInt8((sequence >> 8) & 0xff), UInt8(sequence & 0xff)] + be32(rtpTime) + [0, 0, 0, 0]
        case .buffered:
            rtp = [0x80, UInt8((sequence >> 16) & 0xff), UInt8((sequence >> 8) & 0xff), UInt8(sequence & 0xff)]
                + be32(rtpTime) + [0x16, 0, 0, 0]
        }
        let sealed = seal(key: key, nonce8: le64(nonce), aad: Array(rtp[4..<12]), plain: au)
        return seam(OCBM.seamPkt, le64(scid) + rtp + sealed + le64(nonce))
    }

    /// CH_METADATA: `["META"][u32 BE len][marker][payload]`, len = 1 + payload.
    static func meta(_ marker: UInt8, _ payload: [UInt8]) -> [UInt8] {
        MetaReader.magic + be32(UInt32(1 + payload.count)) + [marker] + payload
    }

    /// `{"kind":"airplay","ev":...}` with `fields` as the rest of the object, already JSON.
    static func event(_ ev: String, _ fields: String = "") -> [UInt8] {
        let sep = fields.isEmpty ? "" : ","
        return meta(OCBM.metaJson, Array("{\"kind\":\"airplay\",\"ev\":\"\(ev)\"\(sep)\(fields)}".utf8))
    }

    /// Lane bytes as OCBM frames (ocbmd cuts a lane wherever its buffer ends, never on a message).
    static func ocbmFrames(channel: UInt16, payload: [UInt8], seq: inout UInt32, firstFlags: UInt8 = 0, chunk: Int = OCBM.maxPayload) -> [UInt8] {
        var out: [UInt8] = []
        var at = 0
        var first = true
        repeat {
            let n = min(chunk, payload.count - at)
            let flags = OCBM.fSom | OCBM.fEom | (first ? firstFlags : 0)
            out += OCBM.frame(channel: channel, flags: flags, seq: seq, payload: Array(payload[at..<(at + n)]))
            seq &+= 1
            at += n
            first = false
        } while at < payload.count
        return out
    }

    // MARK: - Test media -> what the phone would send

    struct Video {
        var codec: MirrorVideoCodec = .h264
        /// Coded size when the fixture knows it (HEVC); 0 = take the H.264 default geometry.
        var width = 0
        var height = 0
        /// The opcode-1 body: a bare avcC (H.264), or an 'hvc1' sample-entry box wrapping an hvcC (HEVC),
        /// as the phone sends them (docs/ocbm/02_AIRPLAY_ONLY_MODE.md §4.10).
        var config: [UInt8]
        /// One access unit per frame: length-prefixed NAL units (4-byte lengths).
        var frames: [(idr: Bool, au: [UInt8])]
    }

    /// The NAL units of an Annex-B stream, start codes removed.
    static func annexBNALs(_ es: [UInt8]) -> [ArraySlice<UInt8>] {
        var starts: [(code: Int, payload: Int)] = []
        var i = 0
        while i + 3 <= es.count {
            if es[i] == 0 && es[i + 1] == 0 && es[i + 2] == 1 {
                starts.append((i > 0 && es[i - 1] == 0 ? i - 1 : i, i + 3))
                i += 3
            } else {
                i += 1
            }
        }
        var nals: [ArraySlice<UInt8>] = []
        for (n, s) in starts.enumerated() {
            let end = n + 1 < starts.count ? starts[n + 1].code : es.count
            if end > s.payload { nals.append(es[s.payload..<end]) }
        }
        return nals
    }

    /// Annex-B H.264 (one slice per frame) -> an avcC and one access unit per frame.
    static func video(annexB es: [UInt8]) -> Video? {
        let nals = annexBNALs(es)
        guard let sps = nals.first(where: { $0.first.map { $0 & 0x1f } == 7 }),
              let pps = nals.first(where: { $0.first.map { $0 & 0x1f } == 8 }), sps.count >= 4 else { return nil }
        let s = Array(sps), p = Array(pps)
        var avcc: [UInt8] = [1, s[1], s[2], s[3], 0xFF, 0xE1, UInt8(s.count >> 8), UInt8(s.count & 0xff)]
        avcc += s
        avcc += [1, UInt8(p.count >> 8), UInt8(p.count & 0xff)]
        avcc += p
        var frames: [(idr: Bool, au: [UInt8])] = []
        var au: [UInt8] = []
        for n in nals {
            let t = n[n.startIndex] & 0x1f
            if t == 7 || t == 8 { continue }
            au += be32(UInt32(n.count))
            au += n
            if t == 1 || t == 5 {
                frames.append((t == 5, au))
                au = []
            }
        }
        return frames.isEmpty ? nil : Video(codec: .h264, config: avcc, frames: frames)
    }

    /// Annex-B HEVC (one slice per frame, VPS/SPS/PPS in band) -> the phone's opcode-1 body and one access
    /// unit per frame. The body is what the D4k60 capture carried: `[size]['hvc1']` + the 78-byte visual
    /// sample entry (`width` x `height`) + an `hvcC` box (VPS, SPS, PPS arrays; NAL length size 4) + a
    /// trailing `colr` box, so a reader has to FIND the hvcC rather than assume where it is.
    static func hevcVideo(annexB es: [UInt8], width: Int, height: Int) -> Video? {
        let nals = annexBNALs(es)
        func type(_ n: ArraySlice<UInt8>) -> UInt8 { (n[n.startIndex] >> 1) & 0x3F }
        guard let vps = nals.first(where: { type($0) == 32 }), let sps = nals.first(where: { type($0) == 33 }),
              let pps = nals.first(where: { type($0) == 34 }), sps.count >= 16 else { return nil }
        // profile_tier_level (general part, 12 bytes) follows the SPS's NAL header and its first byte;
        // remove emulation prevention before reading it.
        var rbsp: [UInt8] = []
        var zeros = 0
        for b in sps.dropFirst(2) {
            if zeros >= 2 && b == 3 { zeros = 0; continue }
            zeros = b == 0 ? zeros + 1 : 0
            rbsp.append(b)
            if rbsp.count >= 13 { break }
        }
        guard rbsp.count >= 13 else { return nil }
        var hvcc: [UInt8] = [1] + Array(rbsp[1...12]) // version, profile/tier/idc, compat 4, constraint 6, level
        hvcc += [0xF0, 0x00, 0xFC, 0xFD, 0xF8, 0xF8, 0x00, 0x00]
        hvcc += [0x0F] // constantFrameRate 0, numTemporalLayers 1, temporalIdNested 1, lengthSizeMinusOne 3
        hvcc += [3]
        for nal in [vps, sps, pps] {
            hvcc += [0x80 | type(nal), 0, 1, UInt8(nal.count >> 8), UInt8(nal.count & 0xff)] + Array(nal)
        }
        func box(_ t: String, _ payload: [UInt8]) -> [UInt8] { be32(UInt32(8 + payload.count)) + Array(t.utf8) + payload }
        func be16(_ v: Int) -> [UInt8] { [UInt8((v >> 8) & 0xff), UInt8(v & 0xff)] }
        var entry = [UInt8](repeating: 0, count: 6) + be16(1)          // reserved, data_reference_index
        entry += [UInt8](repeating: 0, count: 16)                        // pre_defined, reserved, pre_defined[3]
        entry += be16(width) + be16(height)
        entry += be32(0x0048_0000) + be32(0x0048_0000) + be32(0) + be16(1) // 72 dpi x2, reserved, frame_count
        entry += [UInt8](repeating: 0, count: 32)                        // compressorname
        entry += be16(0x18) + [0xFF, 0xFF]                               // depth, pre_defined -1
        let colr = box("colr", Array("nclx".utf8) + be16(1) + be16(1) + be16(1) + [0])
        let config = box("hvc1", entry + box("hvcC", hvcc) + colr)
        var frames: [(idr: Bool, au: [UInt8])] = []
        var au: [UInt8] = []
        for n in nals {
            let t = type(n)
            if (32...35).contains(t) { continue } // VPS, SPS, PPS, AUD
            au += be32(UInt32(n.count))
            au += n
            if t <= 31 {
                frames.append(((16...21).contains(t), au))
                au = []
            }
        }
        return frames.isEmpty ? nil : Video(codec: .hevc, width: width, height: height, config: config, frames: frames)
    }

    struct Audio {
        var sampleRate: UInt32
        var channels: UInt8
        /// Raw access units, ADTS headers removed.
        var units: [[UInt8]]
    }

    /// ADTS AAC -> raw access units, as AirPlay carries them.
    static func audio(adts: [UInt8]) -> Audio? {
        let rates: [UInt32] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350]
        var units: [[UInt8]] = []
        var at = 0
        var rate: UInt32 = 44100
        var channels: UInt8 = 2
        while at + 7 <= adts.count {
            guard adts[at] == 0xFF, adts[at + 1] & 0xF0 == 0xF0 else { return nil }
            let hdr = adts[at + 1] & 1 == 1 ? 7 : 9
            let len = Int(adts[at + 3] & 3) << 11 | Int(adts[at + 4]) << 3 | Int(adts[at + 5] >> 5)
            guard len >= hdr, at + len <= adts.count else { break }
            let sr = Int((adts[at + 2] >> 2) & 0x0f)
            if sr < rates.count { rate = rates[sr] }
            channels = (adts[at + 2] & 1) << 2 | adts[at + 3] >> 6
            units.append(Array(adts[(at + hdr)..<(at + len)]))
            at += len
        }
        return units.isEmpty ? nil : Audio(sampleRate: rate, channels: channels, units: units)
    }

    /// A small real JPEG (a coloured square with a diagonal), so the artwork path decodes an image.
    static func jpeg(seed: Int, size: Int = 96) -> [UInt8] {
        guard let ctx = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0,
                                  space: CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { return [] }
        let hue = CGFloat((seed * 37) % 100) / 100
        ctx.setFillColor(red: 0.2 + 0.6 * hue, green: 0.5, blue: 0.9 - 0.6 * hue, alpha: 1)
        ctx.fill(CGRect(x: 0, y: 0, width: size, height: size))
        ctx.setStrokeColor(red: 1, green: 1, blue: 1, alpha: 1)
        ctx.setLineWidth(6)
        ctx.move(to: CGPoint(x: 8, y: 8))
        ctx.addLine(to: CGPoint(x: size - 8, y: size - 8))
        ctx.strokePath()
        let data = NSMutableData()
        guard let image = ctx.makeImage(),
              let dest = CGImageDestinationCreateWithData(data, "public.jpeg" as CFString, 1, nil) else { return [] }
        CGImageDestinationAddImage(dest, image, nil)
        guard CGImageDestinationFinalize(dest) else { return [] }
        return [UInt8](data as Data)
    }
}

#endif
