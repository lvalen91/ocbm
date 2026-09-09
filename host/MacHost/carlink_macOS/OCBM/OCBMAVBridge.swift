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
        // Wired iOS sends H.264 as a bare avcC record; the HEVC config (and, per the box's own parser,
        // wireless H.264) arrives as a full ISO VisualSampleEntry that has to be unwrapped first.
        let b = AVCCFastPath.extractConfigRecord(fromSampleEntry: raw) ?? raw
        if let (s, p, lenSize) = AVCCFastPath.parseAvcC(b) {
            nalLengthSize = lenSize; configValid = true
            log.info("[\(self.label, privacy: .public)] avcC parsed: SPS \(s.count)B, PPS \(p.count)B, nalLen \(lenSize)")
            decoder.configure(codec: .h264, parameterSets: [Data(s), Data(p)])
            onCodec?(.h264)
        } else if let (v, s, p, lenSize) = AVCCFastPath.parseHvcC(b) {
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
    //
    // MOVED to `AVCCFastPath` (Video/AVCCFastPath.swift, "Codec-config records") 2026-09-03 — verbatim,
    // no behaviour change — so the hardware-free harness (tests/main.swift) can exercise them. The
    // ALT/cluster lane's 63-byte-SPS hvcC had no test; `receiveConfig` above calls straight into them.
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

    /// Telephony (atype 1) lane bookkeeping — the Android Auto call-audio path (box HFP/SCO →
    /// SEAM_PKT_PLAIN). All three are touched only from the audio decrypt queue, the same serial
    /// context `compressedDecoders` above already relies on.
    private var telephonyArmed: Set<UInt64> = []
    private var telRxFrames = 0
    private var telRxBytes = 0
    private var telLastLog = 0.0
    /// Per-scid mSBC decode lanes — the HFP WIDEBAND telephony path (SEAM_FORMAT codec 4). Built on
    /// the format, not the first packet, so the filterbank is warm before audio arrives. Separate
    /// from `compressedDecoders` because CompressedAudioDecoder is an AudioToolbox wrapper and macOS
    /// has no SBC codec at all — this one is our own (Audio/MSBCCodec.swift).
    private var msbcDecoders: [UInt64: MSBCTelephonyDecoder] = [:]
    /// PLC frames reported in the last telephony log window.
    private var telPlcAtLastLog = 0

    func avDidReceiveAudioFormat(scid: UInt64, format: OCBMAudioStreamFormat) {
        // Telephony: the voice/nav mixer node for this rate is already attached and connected from the
        // engine pre-warm (AudioPlayer.wiredPCMRates covers 8 kHz, voice=true covers mono), so "armed"
        // is a statement of fact, not a request — there is nothing to build at the first frame.
        if format.audioType == 1 && (format.isPCM || format.isMSBC) && telephonyArmed.insert(scid).inserted {
            let codec = format.isMSBC ? "mSBC" : "PCM"
            log.info("telephony \(codec, privacy: .public) \(Int(format.sampleRate)) Hz/\(format.channels)ch scid=\(scid) — player armed")
        }
        if format.isMSBC {
            // Wideband: the AUs are eSCO air frames, not PCM. One decoder per scid, reset on every
            // (re)declared format so a second call on the same scid never inherits the first call's
            // filterbank state or its H2 sequence expectation.
            let dec = msbcDecoders[scid] ?? MSBCTelephonyDecoder()
            dec.reset()
            msbcDecoders[scid] = dec
            compressedDecoders[scid] = nil
            log.info("audio scid=\(scid) mSBC \(Int(format.sampleRate))Hz \(format.channels)ch atype=\(format.audioType) — decoder armed")
        } else if format.isPCM {
            compressedDecoders[scid] = nil
            msbcDecoders[scid] = nil   // a lane that was wideband and re-declared narrowband
            log.info("audio scid=\(scid) PCM \(Int(format.sampleRate))Hz \(format.channels)ch atype=\(format.audioType) — pre-warmed node ready")
        } else if let dec = CompressedAudioDecoder(
            codec: format.codec, sampleRate: format.sampleRate, channels: format.channels) {
            compressedDecoders[scid] = dec
            msbcDecoders[scid] = nil
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
        if fmt.isMSBC {
            // Wideband telephony: decode the eSCO read(s) to 16 kHz mono PCM. A payload can be a
            // fragment (no output yet), one frame, or several; `decode` returns exactly what it
            // recovered, including concealment for what the H2 sequence says was lost.
            guard let dec = msbcDecoders[scid] else { return } // format not seen — nothing armed
            let pcm = dec.decode(au)
            if !pcm.isEmpty {
                audio.feedPCM(pcm, rate: MSBC.sampleRate, channels: 1, voice: true, bigEndian: false,
                              preroll: AudioPlayer.telephonyPrerollSeconds)
            }
            noteTelephonyFrame(au.count, format: fmt, plc: dec.plcFrames)
            return
        }
        if fmt.isPCM {
            // `plainLE` is set only on SEAM_PKT_PLAIN (AA telephony): that PCM is host-endian HFP audio.
            // Everything else came out of the CarPlay RTP and is BIG-endian on the wire.
            audio.feedPCM(au, rate: Int(fmt.sampleRate), channels: Int(fmt.channels),
                          voice: fmt.isVoice, bigEndian: !fmt.plainLE,
                          preroll: fmt.plainLE ? AudioPlayer.telephonyPrerollSeconds : 0)
            if fmt.plainLE { noteTelephonyFrame(au.count, format: fmt) }
        } else if let dec = compressedDecoders[scid], let pcm = dec.decode(au) {
            // Decoded output is host-endian at the stream's rate/channels.
            audio.feedPCM(pcm, rate: Int(dec.sampleRate), channels: Int(dec.channels),
                          voice: fmt.isVoice, bigEndian: false)
        }
    }

    /// One-per-second telephony throughput line. A call that is up but silent and a call whose audio
    /// never left the box look identical in the AVmon totals; this prints the delivered frame count and
    /// how much wall-clock audio that is, so a lane running at half rate is visible without a capture.
    /// Audio decrypt queue (serial) — same confinement as the rest of this section.
    private func noteTelephonyFrame(_ bytes: Int, format fmt: OCBMAudioStreamFormat, plc: Int = 0) {
        telRxFrames += 1
        telRxBytes += bytes
        let now = ProcessInfo.processInfo.systemUptime
        if telLastLog == 0 { telLastLog = now; telPlcAtLastLog = plc; return }
        guard now - telLastLog >= 1.0 else { return }
        telLastLog = now
        // mSBC: one AU is one 60-byte air packet carrying 7.5 ms, so the PCM-frame arithmetic below
        // (which divides payload bytes by the sample size) would report a third of the truth.
        let ms: Int
        if fmt.isMSBC {
            ms = telRxFrames * MSBC.samplesPerFrame * 1000 / MSBC.sampleRate
        } else {
            let bytesPerSample = max(1, Int(fmt.bits) / 8)
            let frameBytes = max(1, Int(fmt.channels) * bytesPerSample)
            ms = Int(Double(telRxBytes / frameBytes) * 1000.0 / max(1.0, fmt.sampleRate))
        }
        let plcDelta = max(0, plc - telPlcAtLastLog)
        telPlcAtLastLog = plc
        // A concealed frame is a dropped SCO packet or a corrupt one; silent PLC would make a lossy
        // link look identical to a clean one in the log.
        if plcDelta > 0 {
            log.info("telephony rx=\(self.telRxFrames) frames (\(ms) ms audio) plc=\(plcDelta)")
        } else {
            log.info("telephony rx=\(self.telRxFrames) frames (\(ms) ms audio)")
        }
        telRxFrames = 0
        telRxBytes = 0
    }
}
