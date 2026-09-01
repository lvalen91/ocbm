// OCBMAVDecrypt.swift — decrypt the box-forwarded ENCRYPTED A/V (committed model).
//
// The adapter forwards, over CH_VIDEO / CH_MEDIA_AUDIO, a seam byte-stream of length-prefixed messages:
//     [u32 BE len][marker][payload]
//   marker 0x00 (key)   : [key.output 32B][scid 8B LE]   — the per-stream ChaCha20 key, handed on connect
//   marker 0x01 (frame) : video → [hdr 128B][body];  audio → a raw RTP packet
// This layer reassembles that stream and decrypts host-side (ChaCha20-Poly1305), a faithful port of the
// validated `ocbm-host avdec` + `receiver_core` crypto (both confirmed byte-for-byte against Apple's
// CarPlaySDK). Video: nonce = [0,0,0,0] ++ counter_le64 (per VideoFrame, opcode 0 only), AAD = the 128-B
// header. Audio: nonce = [0,0,0,0] ++ packet trailing 8 B, AAD = pkt[4..12], layout [ct][tag16][nonce8].

import Foundation
import CryptoKit
import os
import Synchronization

/// Per-stream audio format from the box's SEAM_FORMAT message (all-rates/all-streams audio).
/// Wired streams are PCM at various rates; `codec` is the wireless prestage hook (AAC-LC/ELD/OPUS
/// ride the identical encrypted wire — the box never touches the payload).
struct OCBMAudioStreamFormat: Equatable {
    let codec: UInt8      // 0 PCM · 1 AAC-LC · 2 AAC-ELD · 3 OPUS
    let sampleRate: Double
    let channels: UInt32
    let bits: UInt8       // 16 for PCM, 0 for compressed
    let audioType: UInt8  // 0 media · 1 telephony · 2 speechRecognition · 3 alert · 4 default · 5 compatibility

    var isPCM: Bool { codec == 0 }
    // Everything but `media` routes to the voice player. KNOWN GAP (docs/carplay/06_AV_PIPELINE.md §7): atype 5
    // `compatibility` is a MEDIA-carrying PCM fallback, so this sends it to navMixer with the 150 ms
    // voice cap, no pre-roll, and makes it a ducking trigger. Unreachable on wired (media is atype 0
    // there); reachable from the wireless_8 preset.
    var isVoice: Bool { audioType != 0 }
}

/// One consistent snapshot of the six decrypt tallies. Incremented on the per-lane decrypt queues, read from
/// the OCBM control queue (heartbeat) — hence the Mutex in OCBMAVDecrypt, not six bare vars.
struct AVStatsSnapshot: Sendable {
    var videoOK: UInt64 = 0
    var videoFail: UInt64 = 0
    var audioOK: UInt64 = 0
    var audioFail: UInt64 = 0
    // ALT decrypt tally — an alt-key mismatch is otherwise a silently black cluster window.
    var altOK: UInt64 = 0
    var altFail: UInt64 = 0
}

/// Decrypted A/V events for the decode/render layer.
protocol OCBMAVDelegate: AnyObject {
    /// Plaintext avcC/hvcC codec config (video opcode 1) — SPS/PPS/VPS parameter sets.
    func avDidReceiveVideoConfig(_ config: Data)
    /// A decrypted AVCC video access unit (length-prefixed NAL units). `keyframe` from the frame header.
    func avDidReceiveVideoFrame(_ avcc: Data, keyframe: Bool)
    /// A new/changed per-stream audio format (box SEAM_FORMAT) — pre-warm the playback path NOW.
    func avDidReceiveAudioFormat(scid: UInt64, format: OCBMAudioStreamFormat)
    /// A decrypted audio access unit. `format` is nil only if the box never sent SEAM_FORMAT
    /// (legacy box build) — treat as wired media PCM 48k/16/2.
    func avDidReceiveAudio(_ au: Data, scid: UInt64, format: OCBMAudioStreamFormat?)
    /// ALT / navigation (instrument-cluster) screen — SAME payloads as the main video methods but a
    /// SEPARATE stream routed to a DEDICATED decoder. Config = plaintext avcC/hvcC; frame = AVCC AU.
    func avDidReceiveAltVideoConfig(_ config: Data)
    func avDidReceiveAltVideoFrame(_ avcc: Data, keyframe: Bool)
}

// @unchecked Sendable (D1 decouple, perf 2026-08-09): each lane's whole feed runs on its own serial
// queue, so every per-lane stored var stays confined to a single serial context exactly as it was to the
// USB read thread (the invariant the "plain var is safe" comments already rely on) — video vars on
// videoQueue, alt vars on altVideoQueue, audio seams + scid key/format tables on audioQueue. Cross-lane
// shared state (`stats`, `anomalyLog`) is Mutex-guarded; `delegate`/`on*Gap` are set once at setup before
// any frame flows. The compiler can't prove this partition, so the Sendable conformance is unchecked.
final class OCBMAVDecrypt: @unchecked Sendable {
    weak var delegate: OCBMAVDelegate?
    private let log = Logger(subsystem: "com.carlink.ocbm", category: "avdec")

    /// Stream-performance monitor's shared anomaly log (set at session setup, before frames flow). Fed
    /// per decoded video frame (activity/freeze baseline) and on the exceptional gap / decode-fail
    /// branches. Its own Mutex is separate from `stats`; touched only on rare branches or once per frame.
    var anomalyLog: AVAnomalyLog?

    // Per-channel seam reassembly + keys. Audio seams (media CH_MEDIA_AUDIO + voice CH_ALT_AUDIO)
    // carry scid-tagged messages, so keys and formats live in per-scid tables — concurrent streams
    // (telephony + alert share the voice sink) can never clobber each other.
    // Video seams are cursor-based (consume via `…SeamStart`, compact lazily) like OCBMReassembler —
    // no O(remaining) `removeFirst` memmove per message on the hot USB read thread.
    private var videoSeam: [UInt8] = []
    private var videoSeamStart = 0
    private var audioSeam: [UInt8] = []
    private var voiceSeam: [UInt8] = []
    private var videoKey: SymmetricKey?
    private var audioKeys: [UInt64: SymmetricKey] = [:]
    private var audioFormats: [UInt64: OCBMAudioStreamFormat] = [:]

    // ALT / navigation (cluster) video — a fully parallel, independent video pipeline (its own seam,
    // key, counter, seq-resync, and HEVC flag) feeding a DEDICATED decoder. Duplicated from the main
    // video path so a fault in one screen can never corrupt the other.
    private var altVideoSeam: [UInt8] = []
    private var altVideoSeamStart = 0
    private var altVideoKey: SymmetricKey?
    private var altLastVideoSeq: UInt64 = 0
    private var altHaveVideoSeq = false
    private var altVideoBitTrusted = false // per-lane keyframe-bit trust — see videoBitTrusted
    var onAltVideoGap: (() -> Void)?

    // Video loss recovery (task #33 / docs/carplay/06_AV_PIPELINE.md). The box stamps a per-frame `seq` (its RTP-seq analogue)
    // and prefixes each video message with SEAM_MAGIC. `videoCounter` is set from `seq` per frame, so a
    // dropped frame just makes `seq` jump — the host resyncs instead of desyncing forever. On a gap we ask
    // the box to `forceKeyFrame` so the decoder repaints promptly.
    private static let seamMagic: [UInt8] = [0x53, 0x45, 0x41, 0x56] // "SEAV" — matches receiver SEAM_MAGIC
    /// Max reassembled video MESSAGE size. This is a DIFFERENT limit from `OCBM.maxPayload` (the per-OCBM-
    /// frame transport cap): a single video frame is reassembled across many OCBM frames, so a 4K/HEVC IDR
    /// (multiple MB) is legitimate and must NOT be capped at the OCBM frame size. 16 MB comfortably holds a
    /// 4K@60 keyframe. (Bug fix: the old `2 × maxPayload` = 128 KB cap silently rejected every 4K IDR →
    /// the decoder never got a clean keyframe → permanent P-frame poisoning.)
    private static let maxVideoMessage = 16 * 1024 * 1024
    private var lastVideoSeq: UInt64 = 0
    private var haveVideoSeq = false
    /// Keyframe-bit trust (self-calibrating): once we observe the sender set `hdr[5] & 0x10` (Apple's
    /// codec-agnostic sync-sample flag, plaintext in the frame header) on a real IDR, adopt it as the SOLE
    /// keyframe signal — a single byte test that retires the per-frame NAL walk and works across any
    /// resolution/codec/fps change (it's a transport-layer field). Until that proof — and forever on a
    /// legacy sender that never sets the bit — we OR the bit with the NAL walk, so keyframe detection can
    /// never regress. Read/written only on `videoQueue` (like lastVideoSeq) since the D1 decouple, so a plain var is safe.
    private var videoBitTrusted = false
    /// Invoked when a video-frame gap is detected — the host asks the box to request a keyframe from iOS.
    var onVideoGap: (() -> Void)?

    // Stats (surfaced for the A/V verification UI). Mutex-guarded: incremented on the USB read
    // queue, snapshotted from the control queue's heartbeat (C7 — the old six bare vars were an
    // unsynchronized cross-queue read).
    //
    // The stream-performance monitor's per-stream accumulators (bytes/frames/gaps/frame-size/jitter)
    // ride the SAME lock: every per-frame `stats.withLock` already taken for videoOK/videoFail/… now
    // also folds in the metric record, so the A/V hot path gains no extra lock and no allocation — just
    // a few integer adds inside the closure it was already entering (measure-don't-disturb).
    private struct StatsHub: Sendable {
        var av = AVStatsSnapshot()
        var metrics = StreamMetricsAccumulator()
    }
    private let stats = Mutex(StatsHub())

    /// One consistent copy of all six counters under a single lock.
    func statsSnapshot() -> AVStatsSnapshot { stats.withLock { $0.av } }

    /// Freeze the per-stream performance counters at `timestamp` (caller-supplied monotonic seconds) and
    /// rotate the interval frame-size buckets. Read from the monitor's ~1 Hz timer on the main queue.
    func metricsSnapshot(at timestamp: Double) -> StreamMetricsSnapshot {
        stats.withLock { $0.metrics.snapshot(at: timestamp) }
    }

    /// Latch the video codec label ("HEVC"/"H.264") for a video stream into the metrics — called by the
    /// bridge from its authoritative avcC/hvcC config parse (the decoder is the single source of truth;
    /// the decrypt layer no longer sniffs the codec). Surfaced in the metrics `codec` field per lane.
    func setVideoCodec(_ label: String, stream: StreamKind) {
        stats.withLock { $0.metrics.setVideoCodec(stream, label) }
    }

    // D1 decouple (perf 2026-08-09): the USB read thread must ONLY demux. Each lane's whole feed (seam
    // append + reassembly + ChaCha20-Poly1305 decrypt + delegate) runs on ITS OWN serial queue, so a
    // multi-MB Main IDR decrypt can no longer head-of-line-block the read pipe and bunch the cluster
    // lane's frames into a burst that self-overwrites the downstream depth-1 decode slot. Audio (media +
    // voice) shares one queue because both drains touch the scid-keyed audioKeys/audioFormats tables.
    private let videoQueue = DispatchQueue(label: "ocbm.decrypt.video", qos: .userInitiated)
    private let altVideoQueue = DispatchQueue(label: "ocbm.decrypt.altvideo", qos: .userInitiated)
    private let audioQueue = DispatchQueue(label: "ocbm.decrypt.audio", qos: .userInitiated)
    /// Per-lane in-flight backlog (bytes handed to the lane queue but not yet processed). When a lane
    /// exceeds `laneBacklogCap` the read thread hands off SYNCHRONOUSLY, re-applying the project's
    /// closed-loop backpressure (USB fills → box read-gate holds → iPhone throttles THAT encoder,
    /// task #33) instead of growing host memory. Under normal load decrypt keeps up on Apple Silicon and
    /// this stays ~0, so the sync path essentially never fires — it is a bound, not a throttle.
    private let pendingVideoBytes = Atomic<Int>(0)
    private let pendingAltVideoBytes = Atomic<Int>(0)
    private let pendingAudioBytes = Atomic<Int>(0)
    /// 24 MB: above one whole `maxVideoMessage` (16 MB) so a single large 4K IDR never trips the sync
    /// path in normal operation, yet small enough to bound host memory (≤24 MB/lane) + latency. Only
    /// SUSTAINED decrypt overload past this reaches the read-thread block that re-applies backpressure.
    private static let laneBacklogCap = 24 * 1024 * 1024

    /// Dispatch `work` on a lane queue; hands off SYNC once the lane's in-flight backlog (`after`) exceeds
    /// the cap so the read thread is throttled (closed-loop backpressure) rather than the backlog growing
    /// unbounded, else async. `after` is a plain Int (the noncopyable Atomic stays stored on `self`). Only
    /// ever called from the single USB read thread (never from a lane queue), so `.sync` cannot deadlock.
    private func dispatchDecrypt(after: Int, on queue: DispatchQueue, _ work: @escaping () -> Void) {
        if after > Self.laneBacklogCap {
            queue.sync(execute: work)
        } else {
            queue.async(execute: work)
        }
    }

    /// Feed raw CH_VIDEO payload bytes.
    func feedVideo(_ payload: [UInt8]) {
        let after = pendingVideoBytes.wrappingAdd(payload.count, ordering: .relaxed).newValue
        dispatchDecrypt(after: after, on: videoQueue) { [self] in
            compactVideoSeam()
            videoSeam.append(contentsOf: payload)
            drainVideo()
            pendingVideoBytes.wrappingSubtract(payload.count, ordering: .relaxed)
        }
    }

    /// Feed raw CH_ALT_VIDEO payload bytes (the cluster / navigation screen — dedicated decoder).
    func feedAltVideo(_ payload: [UInt8]) {
        let after = pendingAltVideoBytes.wrappingAdd(payload.count, ordering: .relaxed).newValue
        dispatchDecrypt(after: after, on: altVideoQueue) { [self] in
            compactAltVideoSeam()
            altVideoSeam.append(contentsOf: payload)
            drainAltVideo()
            pendingAltVideoBytes.wrappingSubtract(payload.count, ordering: .relaxed)
        }
    }

    /// Lazy compaction (OCBMReassembler pattern): drop consumed bytes only when fully drained or the
    /// cursor has passed a threshold — never per message.
    private func compactVideoSeam() {
        if videoSeamStart >= videoSeam.count {
            videoSeam.removeAll(keepingCapacity: true)
            videoSeamStart = 0
        } else if videoSeamStart > 2 * OCBM.maxPayload {
            videoSeam.removeFirst(videoSeamStart)
            videoSeamStart = 0
        }
    }

    private func compactAltVideoSeam() {
        if altVideoSeamStart >= altVideoSeam.count {
            altVideoSeam.removeAll(keepingCapacity: true)
            altVideoSeamStart = 0
        } else if altVideoSeamStart > 2 * OCBM.maxPayload {
            altVideoSeam.removeFirst(altVideoSeamStart)
            altVideoSeamStart = 0
        }
    }

    /// Feed raw CH_MEDIA_AUDIO payload bytes. Shares audioQueue with voice (both touch the scid key/
    /// format tables), so they stay mutually serialized as before — just off the USB read thread.
    func feedAudio(_ payload: [UInt8]) {
        let after = pendingAudioBytes.wrappingAdd(payload.count, ordering: .relaxed).newValue
        dispatchDecrypt(after: after, on: audioQueue) { [self] in
            audioSeam.append(contentsOf: payload)
            drainAudio(&audioSeam, kind: .mediaAudio)
            pendingAudioBytes.wrappingSubtract(payload.count, ordering: .relaxed)
        }
    }

    /// Feed raw CH_ALT_AUDIO (voice sink: telephony/speechRecognition/alert/default) payload bytes.
    /// Identical framing to the media seam; only the buffer differs (separate TCP seams on the box).
    func feedVoice(_ payload: [UInt8]) {
        let after = pendingAudioBytes.wrappingAdd(payload.count, ordering: .relaxed).newValue
        dispatchDecrypt(after: after, on: audioQueue) { [self] in
            voiceSeam.append(contentsOf: payload)
            drainAudio(&voiceSeam, kind: .voiceAudio)
            pendingAudioBytes.wrappingSubtract(payload.count, ordering: .relaxed)
        }
    }

    // MARK: - Seam parsing

    /// Pull complete [u32 BE len][payload] messages out of a seam buffer.
    private func nextMessage(_ seam: inout [UInt8]) -> [UInt8]? {
        while seam.count >= 4 {
            let mlen = (Int(seam[0]) << 24) | (Int(seam[1]) << 16) | (Int(seam[2]) << 8) | Int(seam[3])
            if mlen == 0 || mlen > 2 * OCBM.maxPayload {
                seam.removeFirst(1) // implausible length — resync
                continue
            }
            if seam.count < 4 + mlen {
                if seam.count > 8 * OCBM.maxPayload { seam.removeFirst(1); continue } // never-completing → resync
                return nil
            }
            let msg = Array(seam[4..<4 + mlen])
            seam.removeFirst(4 + mlen)
            return msg
        }
        return nil
    }

    /// Pull the next video message: `[u32 BE len][SEAM_MAGIC][marker][payload]`. Returns `[marker][payload]`
    /// (magic stripped). If the length-prefix framing is torn by a lost packet, scan forward for SEAM_MAGIC
    /// and re-align (the magic marks each frame boundary — the RTP-resync analogue).
    private func nextVideoMessage() -> [UInt8]? {
        let M = Self.seamMagic
        while videoSeam.count - videoSeamStart >= 8 { // need at least [len 4][magic 4]
            let s = videoSeamStart
            let mlen = (Int(videoSeam[s]) << 24) | (Int(videoSeam[s + 1]) << 16) | (Int(videoSeam[s + 2]) << 8) | Int(videoSeam[s + 3])
            let magicOK = videoSeam[s + 4] == M[0] && videoSeam[s + 5] == M[1] && videoSeam[s + 6] == M[2] && videoSeam[s + 7] == M[3]
            if mlen < 5 || mlen > Self.maxVideoMessage || !magicOK {
                if !resyncVideoToMagic() { return nil } // desync (bad len or missing magic) → re-align
                continue
            }
            if videoSeam.count - s < 4 + mlen {
                // Magic is verified, so `mlen` is trustworthy — just wait for the rest of this frame.
                // (A large 4K IDR legitimately streams in across many OCBM frames; do NOT resync early.)
                return nil
            }
            let msg = Array(videoSeam[(s + 8)..<(s + 4 + mlen)]) // strip [len][magic] → [marker][payload]
            videoSeamStart = s + 4 + mlen
            return msg
        }
        return nil
    }

    /// Scan for the next SEAM_MAGIC and drop everything before its length prefix (magic − 4), so parsing
    /// re-aligns on a clean frame boundary. Returns false if no magic is buffered yet (need more bytes).
    private func resyncVideoToMagic() -> Bool {
        let M = Self.seamMagic
        var i = videoSeamStart + 5 // start past the current (bad) position
        while i + 4 <= videoSeam.count {
            if videoSeam[i] == M[0] && videoSeam[i+1] == M[1] && videoSeam[i+2] == M[2] && videoSeam[i+3] == M[3] {
                videoSeamStart = i - 4 // leave [len][magic]… aligned (magic is at offset 4)
                return true
            }
            i += 1
        }
        // No magic found — keep a small tail (a magic could straddle the next feed) and wait for more.
        if videoSeam.count - videoSeamStart > 7 { videoSeamStart = videoSeam.count - 7 }
        return false
    }

    private func drainVideo() {
        while let msg = nextVideoMessage() {
            guard let marker = msg.first else { continue }
            switch marker {
            case 0x00 where msg.count >= 33: // [0x00][key 32][scid 8]
                videoKey = SymmetricKey(data: Data(msg[1..<33]))
                haveVideoSeq = false
                log.info("received video key")
            case 0x01 where msg.count >= 1 + 8 + 128: // [0x01][seq 8 LE][hdr 128][body]
                var seq: UInt64 = 0
                for k in 0..<8 { seq |= UInt64(msg[1 + k]) << (8 * k) }
                let hdr = msg[9..<137]   // slices, not Array copies (opt): `msg` is already a fresh Array;
                let body = msg[137...]   // decrypt takes DataProtocol slices → no per-frame copy (MB on a 4K IDR).
                let opcode = msg[13]     // header offset 4 = msg[9+4]; hdr is now a NON-0-based slice.
                if opcode == 1 {
                    // VideoConfig: plaintext avcC/hvcC — no decrypt, no counter.
                    delegate?.avDidReceiveVideoConfig(Data(body))
                } else if opcode == 0 {
                    guard let key = videoKey else { break }
                    let mlen = msg.count
                    let nowNs = DispatchTime.now().uptimeNanoseconds
                    // V6: shorter than the 16-B Poly1305 tag — cannot be ciphertext. Classify it
                    // explicitly instead of letting decrypt's own guard land it silently in the
                    // fail bucket; advance the seq (the box consumed the nonce) and recover.
                    guard body.count >= 16 else {
                        let s = stats.withLock { h -> AVStatsSnapshot in
                            h.av.videoFail &+= 1
                            h.metrics.record(.mainVideo, bytes: mlen, sizeSample: nil, gap: false, fail: true, nowNs: 0)
                            return h.av
                        }
                        log.error("sub-16B opcode-0 body (\(body.count, privacy: .public) B) — invalid, dropped (ok=\(s.videoOK, privacy: .public) fail=\(s.videoFail, privacy: .public))")
                        onVideoGap?()
                        anomalyLog?.record(.decodeFail, stream: .mainVideo, detail: "sub-16B body",
                                           tMonoMs: Double(nowNs) / 1e6, tWall: Date().timeIntervalSince1970)
                        lastVideoSeq = seq
                        haveVideoSeq = true
                        break
                    }
                    // A gap (seq jumped) = frames were lost. Resync the counter from the box's absolute
                    // seq (never desync) and ask the box for a keyframe so the decoder repaints.
                    var gap = false
                    if haveVideoSeq && seq != lastVideoSeq &+ 1 {
                        gap = true
                        log.info("video gap: seq \(self.lastVideoSeq)→\(seq) — resync + keyframe")
                        onVideoGap?()
                        anomalyLog?.record(.gap, stream: .mainVideo, detail: "seq \(lastVideoSeq)→\(seq)",
                                           tMonoMs: Double(nowNs) / 1e6, tWall: Date().timeIntervalSince1970)
                    }
                    if let plain = Self.decryptVideoFrame(hdr: hdr, body: body, key: key, counter: seq) {
                        stats.withLock { h in
                            h.av.videoOK &+= 1
                            h.metrics.record(.mainVideo, bytes: mlen, sizeSample: UInt32(clamping: body.count),
                                             gap: gap, fail: false, nowNs: nowNs)
                        }
                        // Activity/freeze baseline: fed the DECODED-OK frame's arrival + size.
                        anomalyLog?.recordVideoFrame(.mainVideo, bytes: body.count,
                                                     tMonoMs: Double(nowNs) / 1e6, tWall: Date().timeIntervalSince1970)
                        // Keyframe = Apple's sync-sample flag in the plaintext header, hdr[5] & 0x10
                        // (msg[9+5]=msg[14]); on-wire-confirmed 100% vs the NAL walk. Self-calibrating: once
                        // an IDR proves the sender sets it, trust the byte test alone (retiring the NAL walk);
                        // until then — and forever on a legacy sender that never sets it — OR with the NAL
                        // walk so detection never regresses (supersedes the A2 OR-both).
                        let kfBit = (msg[14] & 0x10) != 0
                        let kf: Bool
                        if videoBitTrusted {
                            kf = kfBit
                        } else {
                            kf = kfBit || Self.hevcHasIRAP(plain) || Self.avccHasIDR(plain)
                            if kfBit {
                                videoBitTrusted = true
                                log.info("main video: keyframe bit (hdr[5]&0x10) confirmed on-wire — NAL walk retired")
                            }
                        }
                        delegate?.avDidReceiveVideoFrame(plain, keyframe: kf)
                    } else {
                        // V5: a failed decrypt is a lost frame — recover like a seq gap (the gap
                        // detector can't fire for it, since seq still advances below).
                        let s = stats.withLock { h -> AVStatsSnapshot in
                            h.av.videoFail &+= 1
                            h.metrics.record(.mainVideo, bytes: mlen, sizeSample: nil, gap: gap, fail: true, nowNs: 0)
                            return h.av
                        }
                        onVideoGap?()
                        anomalyLog?.record(.decodeFail, stream: .mainVideo, detail: "decrypt failed",
                                           tMonoMs: Double(nowNs) / 1e6, tWall: Date().timeIntervalSince1970)
                        if s.videoFail == 1 || s.videoFail % 300 == 0 { // throttled: first + ~every 5 s at 60 fps
                            log.error("video decrypt FAILED (ok=\(s.videoOK, privacy: .public) fail=\(s.videoFail, privacy: .public)) — requesting keyframe")
                        }
                    }
                    // Advance even on failure: seq is the nonce and the box consumed it. Freezing
                    // here would make the NEXT good frame look like a spurious second gap.
                    lastVideoSeq = seq
                    haveVideoSeq = true
                }
            default:
                break
            }
        }
    }

    // MARK: - ALT / navigation video (dedicated, parallel to the main video path above)

    /// Pull the next ALT-video message (mirror of `nextVideoMessage`, on `altVideoSeam`).
    private func nextAltVideoMessage() -> [UInt8]? {
        let M = Self.seamMagic
        while altVideoSeam.count - altVideoSeamStart >= 8 {
            let s = altVideoSeamStart
            let mlen = (Int(altVideoSeam[s]) << 24) | (Int(altVideoSeam[s + 1]) << 16) | (Int(altVideoSeam[s + 2]) << 8) | Int(altVideoSeam[s + 3])
            let magicOK = altVideoSeam[s + 4] == M[0] && altVideoSeam[s + 5] == M[1] && altVideoSeam[s + 6] == M[2] && altVideoSeam[s + 7] == M[3]
            if mlen < 5 || mlen > Self.maxVideoMessage || !magicOK {
                if !resyncAltVideoToMagic() { return nil }
                continue
            }
            if altVideoSeam.count - s < 4 + mlen { return nil }
            let msg = Array(altVideoSeam[(s + 8)..<(s + 4 + mlen)])
            altVideoSeamStart = s + 4 + mlen
            return msg
        }
        return nil
    }

    private func resyncAltVideoToMagic() -> Bool {
        let M = Self.seamMagic
        var i = altVideoSeamStart + 5
        while i + 4 <= altVideoSeam.count {
            if altVideoSeam[i] == M[0] && altVideoSeam[i+1] == M[1] && altVideoSeam[i+2] == M[2] && altVideoSeam[i+3] == M[3] {
                altVideoSeamStart = i - 4
                return true
            }
            i += 1
        }
        if altVideoSeam.count - altVideoSeamStart > 7 { altVideoSeamStart = altVideoSeam.count - 7 }
        return false
    }

    /// Drain the ALT video seam → the dedicated decoder (mirror of `drainVideo`).
    private func drainAltVideo() {
        while let msg = nextAltVideoMessage() {
            guard let marker = msg.first else { continue }
            switch marker {
            case 0x00 where msg.count >= 33:
                altVideoKey = SymmetricKey(data: Data(msg[1..<33]))
                altHaveVideoSeq = false
                log.info("received ALT video key")
            case 0x01 where msg.count >= 1 + 8 + 128:
                var seq: UInt64 = 0
                for k in 0..<8 { seq |= UInt64(msg[1 + k]) << (8 * k) }
                let hdr = msg[9..<137]   // slices, not Array copies (opt) — see main lane
                let body = msg[137...]
                let opcode = msg[13]     // header offset 4 = msg[9+4]
                if opcode == 1 {
                    delegate?.avDidReceiveAltVideoConfig(Data(body))
                } else if opcode == 0 {
                    guard let key = altVideoKey else { break }
                    let mlen = msg.count
                    let nowNs = DispatchTime.now().uptimeNanoseconds
                    var gap = false
                    if altHaveVideoSeq && seq != altLastVideoSeq &+ 1 {
                        gap = true
                        log.info("ALT video gap: seq \(self.altLastVideoSeq)→\(seq) — resync + keyframe")
                        onAltVideoGap?()
                        anomalyLog?.record(.gap, stream: .altVideo, detail: "seq \(altLastVideoSeq)→\(seq)",
                                           tMonoMs: Double(nowNs) / 1e6, tWall: Date().timeIntervalSince1970)
                    }
                    if let plain = Self.decryptVideoFrame(hdr: hdr, body: body, key: key, counter: seq) {
                        stats.withLock { h in
                            h.av.altOK &+= 1
                            h.metrics.record(.altVideo, bytes: mlen, sizeSample: UInt32(clamping: body.count),
                                             gap: gap, fail: false, nowNs: nowNs)
                        }
                        anomalyLog?.recordVideoFrame(.altVideo, bytes: body.count,
                                                     tMonoMs: Double(nowNs) / 1e6, tWall: Date().timeIntervalSince1970)
                        // Self-calibrating keyframe bit (see main lane): hdr[5]&0x10 = msg[14] & 0x10.
                        let kfBit = (msg[14] & 0x10) != 0
                        let kf: Bool
                        if altVideoBitTrusted {
                            kf = kfBit
                        } else {
                            kf = kfBit || Self.hevcHasIRAP(plain) || Self.avccHasIDR(plain)
                            if kfBit {
                                altVideoBitTrusted = true
                                log.info("alt video: keyframe bit (hdr[5]&0x10) confirmed on-wire — NAL walk retired")
                            }
                        }
                        delegate?.avDidReceiveAltVideoFrame(plain, keyframe: kf)
                    } else {
                        // V5 (alt lane): failed decrypt = lost frame — recover like a seq gap.
                        let s = stats.withLock { h -> AVStatsSnapshot in
                            h.av.altFail &+= 1
                            h.metrics.record(.altVideo, bytes: mlen, sizeSample: nil, gap: gap, fail: true, nowNs: 0)
                            return h.av
                        }
                        onAltVideoGap?()
                        anomalyLog?.record(.decodeFail, stream: .altVideo, detail: "decrypt failed",
                                           tMonoMs: Double(nowNs) / 1e6, tWall: Date().timeIntervalSince1970)
                        if s.altFail == 1 || s.altFail % 300 == 0 { // throttled: first + ~every 5 s at 60 fps
                            log.error("ALT video decrypt FAILED (ok=\(s.altOK, privacy: .public) fail=\(s.altFail, privacy: .public)) — key mismatch would leave the cluster black")
                        }
                    }
                    // Advance even on failure — seq is the nonce, the box consumed it (see main lane).
                    altLastVideoSeq = seq
                    altHaveVideoSeq = true
                }
            default:
                break
            }
        }
    }

    /// Drain one audio seam buffer (media or voice — identical scid-tagged v2 framing). `kind` is the
    /// physical channel (media vs voice sink), which is exactly the monitor's two audio streams.
    private func drainAudio(_ seam: inout [UInt8], kind: StreamKind) {
        while let msg = nextMessage(&seam) {
            guard let marker = msg.first else { continue }
            switch marker {
            case 0x00 where msg.count >= 41: // SEAM_KEY: [0x00][key 32][scid 8]
                let scid = Self.readU64LE(msg, at: 33)
                audioKeys[scid] = SymmetricKey(data: Data(msg[1..<33]))
                log.info("received audio key (scid=\(scid, privacy: .public))")
            case 0x02 where msg.count >= 17: // SEAM_FORMAT: [0x02][scid 8][codec][rate u32][ch][bits][atype]
                let scid = Self.readU64LE(msg, at: 1)
                let rate = UInt32(msg[10]) | (UInt32(msg[11]) << 8) | (UInt32(msg[12]) << 16) | (UInt32(msg[13]) << 24)
                let fmt = OCBMAudioStreamFormat(
                    codec: msg[9], sampleRate: Double(rate), channels: UInt32(msg[14]),
                    bits: msg[15], audioType: msg[16])
                stats.withLock { $0.metrics.setFormat(kind, fmt) }
                if audioFormats[scid] != fmt {
                    audioFormats[scid] = fmt
                    log.info("audio format scid=\(scid, privacy: .public): codec=\(fmt.codec) \(rate, privacy: .public)Hz \(fmt.channels)ch atype=\(fmt.audioType, privacy: .public)")
                    delegate?.avDidReceiveAudioFormat(scid: scid, format: fmt)
                }
            case 0x01 where msg.count >= 9: // SEAM_PKT: [0x01][scid 8][raw RTP]
                let scid = Self.readU64LE(msg, at: 1)
                let pkt = Array(msg[9...])
                let mlen = msg.count
                guard let key = audioKeys[scid] else { break }
                if let au = Self.decryptAudio(pkt: pkt, key: key) {
                    stats.withLock { h in
                        h.av.audioOK &+= 1
                        h.metrics.record(kind, bytes: mlen, sizeSample: UInt32(clamping: pkt.count),
                                         gap: false, fail: false, nowNs: 0)
                    }
                    delegate?.avDidReceiveAudio(au, scid: scid, format: audioFormats[scid])
                } else {
                    stats.withLock { h in
                        h.av.audioFail &+= 1
                        h.metrics.record(kind, bytes: mlen, sizeSample: nil, gap: false, fail: true, nowNs: 0)
                    }
                    anomalyLog?.record(.decodeFail, stream: kind, detail: "audio decrypt failed",
                                       tMonoMs: AVAnomalyLog.monoMs(), tWall: Date().timeIntervalSince1970)
                }
            default:
                break
            }
        }
    }

    private static func readU64LE(_ b: [UInt8], at i: Int) -> UInt64 {
        var v: UInt64 = 0
        for k in 0..<8 { v |= UInt64(b[i + k]) << (8 * k) }
        return v
    }

    // MARK: - Crypto (ChaCha20-Poly1305, exact port of the validated box logic)

    // Takes ArraySlices (opt: no per-frame `Array(msg[9..<137])`/`Array(msg[137...])` copy — the caller
    // passes seam slices directly; a 4K IDR body is multiple MB). Index-AGNOSTIC (`dropLast`/`suffix`, not
    // `body[0..<…]`) because a slice carries the original array's indices, not 0-based. CryptoKit + `open`
    // take any DataProtocol, so the slices flow through with no intermediate copy.
    static func decryptVideoFrame(hdr: ArraySlice<UInt8>, body: ArraySlice<UInt8>, key: SymmetricKey, counter: UInt64) -> Data? {
        guard body.count >= 16 else { return nil }
        var nonceBytes = [UInt8](repeating: 0, count: 12) // [0,0,0,0] ++ counter_le64
        let le = counter.littleEndian
        withUnsafeBytes(of: le) { raw in for i in 0..<8 { nonceBytes[4 + i] = raw[i] } }
        let ct = body.dropLast(16)
        let tag = body.suffix(16)
        return Self.open(nonce: nonceBytes, ciphertext: ct, tag: tag, aad: hdr, key: key)
    }

    static func decryptAudio(pkt: [UInt8], key: SymmetricKey) -> Data? {
        // layout: [12B RTP hdr][ciphertext][16B tag][8B nonce]; AAD = ts‖ssrc = pkt[4..12]
        guard pkt.count >= 12 + 16 + 8 else { return nil }
        let n = pkt.count
        let aad = pkt[4..<12]
        var nonceBytes = [UInt8](repeating: 0, count: 12)
        for i in 0..<8 { nonceBytes[4 + i] = pkt[n - 8 + i] }
        let ct = pkt[12..<(n - 8 - 16)]
        let tag = pkt[(n - 8 - 16)..<(n - 8)]
        return Self.open(nonce: nonceBytes, ciphertext: ct, tag: tag, aad: aad, key: key)
    }

    private static func open<C: DataProtocol, T: DataProtocol, A: DataProtocol>(
        nonce: [UInt8], ciphertext: C, tag: T, aad: A, key: SymmetricKey
    ) -> Data? {
        do {
            let n = try ChaChaPoly.Nonce(data: nonce)
            let box = try ChaChaPoly.SealedBox(nonce: n, ciphertext: ciphertext, tag: tag)
            return try ChaChaPoly.open(box, using: key, authenticating: aad)
        } catch {
            return nil
        }
    }

    /// IRAP (keyframe-class) detection in a length-prefixed HEVC access unit — NAL types 16–21
    /// (BLA/IDR/CRA), type = (header >> 1) & 0x3F.
    private static func hevcHasIRAP(_ data: Data) -> Bool {
        data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            let b = raw.bindMemory(to: UInt8.self)
            var i = 0
            while i + 4 <= b.count {
                let len = (Int(b[i]) << 24) | (Int(b[i + 1]) << 16) | (Int(b[i + 2]) << 8) | Int(b[i + 3])
                if len <= 0 || i + 4 + len > b.count { break }
                let nalType = (b[i + 4] >> 1) & 0x3F
                if (16...21).contains(nalType) { return true }
                i += 4 + len
            }
            return false
        }
    }

    /// Approximate IDR detection in an AVCC access unit (4-byte length prefixes, H.264 NAL type 5).
    private static func avccHasIDR(_ avcc: Data) -> Bool {
        avcc.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            let b = raw.bindMemory(to: UInt8.self)
            var i = 0
            while i + 4 <= b.count {
                let len = (Int(b[i]) << 24) | (Int(b[i + 1]) << 16) | (Int(b[i + 2]) << 8) | Int(b[i + 3])
                if len <= 0 || i + 4 + len > b.count { break }
                let nalType = b[i + 4] & 0x1f
                if nalType == 5 { return true }
                i += 4 + len
            }
            return false
        }
    }
}
