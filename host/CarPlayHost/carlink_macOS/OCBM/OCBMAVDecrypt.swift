// OCBMAVDecrypt.swift — decrypt the box-forwarded ENCRYPTED A/V (committed model).
//
// The adapter forwards, over CH_VIDEO / CH_MEDIA_AUDIO, a seam byte-stream of length-prefixed messages:
//     [u32 BE len][marker][payload]
//   marker 0x00 (key)   : [key.output 32B][scid 8B LE]   — the per-stream ChaCha20 key, handed on connect
//   marker 0x01 (frame) : video → [hdr 128B][body];  audio → a raw RTP packet
//   marker 0x03 (plain) : audio → [scid 8B LE][raw PCM] — the Android Auto telephony lane (HFP/SCO
//                         S16LE). Unencrypted by construction: there is no AirPlay stream behind it,
//                         so there is no key. Delivered verbatim after its SEAM_FORMAT.
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
    let codec: UInt8      // 0 PCM · 1 AAC-LC · 2 AAC-ELD · 3 OPUS · 4 mSBC (OCBM.seamCodecMsbc)
    let sampleRate: Double
    let channels: UInt32
    let bits: UInt8       // 16 for PCM, 0 for compressed
    let audioType: UInt8  // 0 media · 1 telephony · 2 speechRecognition · 3 alert · 4 default · 5 compatibility
    /// NOT on the wire — stamped on the COPY handed to the delegate when the access unit arrived as
    /// `SEAM_PKT_PLAIN` (0x03). Those payloads are the box's Android Auto telephony lane: raw
    /// HFP/SCO S16LE, host-endian, whereas every CarPlay PCM stream arrives BIG-endian inside the
    /// encrypted RTP. Same rate/channels/atype on both, so nothing else can tell them apart, and
    /// getting it wrong is full-scale white noise. The stored table keeps the wire value (false).
    var plainLE: Bool = false

    var isPCM: Bool { codec == 0 }
    /// HFP wideband: the payload is an mSBC bitstream (raw transparent-eSCO reads), NOT PCM, and
    /// `sampleRate`/`bits` describe what it decodes TO. Only the telephony lane ever sets it, and
    /// only OCBMAVBridge acts on it — the decrypt layer forwards these AUs untouched like any other
    /// SEAM_PKT_PLAIN payload. Rendering the bytes as PCM would be full-scale noise.
    var isMSBC: Bool { codec == OCBM.seamCodecMsbc }
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
    // RAW ARRIVAL per audio channel, counted at the OCBM frame — BEFORE any seam parse or decrypt.
    // Separating arrival from decrypt success is what tells "the box sent nothing" apart from "the box
    // sent plenty and the seam is desynced": the 2026-09-02 re-SETUP desync showed audioOK == 0 with
    // megabytes arriving, and the AVmon line could not distinguish that from silence.
    var mediaAudioRxFrames: UInt64 = 0
    var mediaAudioRxBytes: UInt64 = 0
    var voiceAudioRxFrames: UInt64 = 0
    var voiceAudioRxBytes: UInt64 = 0
    /// SEAM_PKTs dropped because no SEAM_KEY has been seen for that scid yet. Was a silent `break`.
    var audioNoKeyDrops: UInt64 = 0
    /// SEAM_PKT_PLAIN (0x03) access units delivered — the AA telephony lane. Separate from `audioOK`
    /// because nothing was decrypted: a nonzero `audioPlainOK` with `audioOK == 0` is the normal
    /// steady state of a call, not a decrypt failure.
    var audioPlainOK: UInt64 = 0
    /// SEAM_PKT_PLAIN dropped for want of a preceding SEAM_FORMAT on that scid — we would not know
    /// the rate/channels to play them at, and guessing 48k/2 is white noise.
    var audioPlainNoFormatDrops: UInt64 = 0
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
    private var audioSeam = AudioSeamBuf()
    private var voiceSeam = AudioSeamBuf()
    private var videoKey: SymmetricKey?
    private var audioKeys: [UInt64: SymmetricKey] = [:]
    private var audioFormats: [UInt64: OCBMAudioStreamFormat] = [:]
    /// First-packets trace per audio scid (RTP payload type, plaintext size/head) — enough to tell a
    /// plain access unit from an RFC 2198 redundancy bundle without a capture.
    private var audioTraceCount: [UInt64: Int] = [:]
    /// scids whose "plain access unit before any SEAM_FORMAT" complaint has already been logged
    /// (audioQueue-confined, like the tables above). One line per scid, not one per 20 ms frame.
    private var plainNoFormatLogged: Set<UInt64> = []

    // ALT / navigation (cluster) video — a fully parallel, independent video pipeline (its own seam,
    // key, counter, seq-resync, and HEVC flag) feeding a DEDICATED decoder. Duplicated from the main
    // video path so a fault in one screen can never corrupt the other.
    private var altVideoSeam: [UInt8] = []
    private var altVideoSeamStart = 0
    private var altVideoKey: SymmetricKey?
    private var altLastVideoSeq: UInt64 = 0
    private var altHaveVideoSeq = false
    private var altVideoNoKeyDrops = 0
    private var altVideoSawMagic = false
    private var altVideoNoMagicBytes = 0
    var onAltVideoGap: (() -> Void)?

    // Video loss recovery (task #33 / docs/carplay/06_AV_PIPELINE.md). The box stamps a per-frame `seq` (its RTP-seq analogue)
    // and prefixes each video message with SEAM_MAGIC. `videoCounter` is set from `seq` per frame, so a
    // dropped frame just makes `seq` jump — the host resyncs instead of desyncing forever. On a gap we ask
    // the box to `forceKeyFrame` so the decoder repaints promptly.
    private static let seamMagic: [UInt8] = [0x53, 0x45, 0x41, 0x56] // "SEAV" — matches receiver SEAM_MAGIC
    /// Max reassembled AUDIO message. An RTP datagram is ~1.5 KB; this is only an anti-wedge bound.
    private static let maxAudioMessage = 2 * OCBM.maxPayload
    /// Which framing an audio seam speaks. The box carried NO magic on the audio seams before
    /// 2026-09-03, so a host must still parse `[u32 BE len][marker]` from an older box. Latched from the
    /// first message parsed on that seam, and DELIBERATELY kept across a `fNewSource` reset: a new seam
    /// producer is the same box build, and re-detecting on a buffer that starts mid-nothing is the one
    /// place a wrong latch could happen.
    private enum AudioFraming { case unknown, magic, legacy }
    /// One audio seam's reassembly buffer plus its latched framing.
    private struct AudioSeamBuf {
        var bytes: [UInt8] = []
        var framing: AudioFraming = .unknown
    }
    /// One log line total, not one per seam: the framing is a property of the box build.
    private var loggedLegacyAudioFraming = false
    /// Max reassembled video MESSAGE size. This is a DIFFERENT limit from `OCBM.maxPayload` (the per-OCBM-
    /// frame transport cap): a single video frame is reassembled across many OCBM frames, so a 4K/HEVC IDR
    /// (multiple MB) is legitimate and must NOT be capped at the OCBM frame size. 16 MB comfortably holds a
    /// 4K@60 keyframe. (Bug fix: the old `2 × maxPayload` = 128 KB cap silently rejected every 4K IDR →
    /// the decoder never got a clean keyframe → permanent P-frame poisoning.)
    private static let maxVideoMessage = 16 * 1024 * 1024
    /// L5: after this many bytes discarded WITHOUT a single SEAM_MAGIC ever appearing, say so. That is
    /// the signature of a box running `OCBM_FWD_ENC=0` (the mode `gm_ccpa` ships,
    /// docs/carplay/01_OCBM_PROTOCOL.md): its video seam is `[u32 BE len][Annex-B]` — no magic, marker,
    /// key or seq — which is wire-incompatible with this host and has NO discriminator on the wire.
    private static let noMagicWarnBytes = 4 * 1024 * 1024
    private var lastVideoSeq: UInt64 = 0
    private var haveVideoSeq = false
    /// Keyframe-bit trust (self-calibrating): once we observe the sender set `hdr[5] & 0x10` (Apple's
    /// codec-agnostic sync-sample flag, plaintext in the frame header) on a real IDR, adopt it as the SOLE
    /// keyframe signal — a single byte test that retires the per-frame NAL walk and works across any
    /// resolution/codec/fps change (it's a transport-layer field). Until that proof — and forever on a
    /// legacy sender that never sets the bit — we OR the bit with the NAL walk, so keyframe detection can
    /// never regress. Read/written only on `videoQueue` (like lastVideoSeq) since the D1 decouple, so a plain var is safe.
    ///
    /// The bit is an ON-WIRE CORRELATION, not something Apple's sources document as a sync-sample flag
    /// (`smallParam[0]` bit 4 is `Unused4` in R14G17), so the latch demands CORROBORATION: the bit and
    /// the NAL walk must agree on the SAME frame before the walk is retired. A sender that sets the bit
    /// for some other meaning therefore never captures the latch — it just keeps the safe OR-both path.
    /// Cleared on every new key (`case 0x00`): a new key is a new stream/sender, so the proof lapses.
    /// Throttled diagnostics for the two silent-drop lanes (frames before SEAM_KEY, and a seam that
    /// never carries SEAM_MAGIC at all). Both otherwise present as a black screen with 0 ok / 0 fail.
    private var videoNoKeyDrops = 0
    private var videoSawMagic = false
    private var videoNoMagicBytes = 0
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

    /// Feed raw CH_VIDEO payload bytes. `newSource` is the frame's `OCBM.fNewSource` bit: the box
    /// accepted a NEW producer on this seam and dropped the old one without draining it, so whatever
    /// partial message is still buffered belongs to a producer that will never finish it. Reset BEFORE
    /// appending — the reset runs on the lane queue so it stays ordered against earlier payloads.
    func feedVideo(_ payload: [UInt8], newSource: Bool = false) {
        let after = pendingVideoBytes.wrappingAdd(payload.count, ordering: .relaxed).newValue
        dispatchDecrypt(after: after, on: videoQueue) { [self] in
            if newSource {
                noteNewSource(.mainVideo, held: videoSeam.count - videoSeamStart)
                videoSeam.removeAll(keepingCapacity: true)
                videoSeamStart = 0
            }
            compactVideoSeam()
            videoSeam.append(contentsOf: payload)
            drainVideo()
            pendingVideoBytes.wrappingSubtract(payload.count, ordering: .relaxed)
        }
    }

    /// Feed raw CH_ALT_VIDEO payload bytes (the cluster / navigation screen — dedicated decoder).
    func feedAltVideo(_ payload: [UInt8], newSource: Bool = false) {
        let after = pendingAltVideoBytes.wrappingAdd(payload.count, ordering: .relaxed).newValue
        dispatchDecrypt(after: after, on: altVideoQueue) { [self] in
            if newSource {
                noteNewSource(.altVideo, held: altVideoSeam.count - altVideoSeamStart)
                altVideoSeam.removeAll(keepingCapacity: true)
                altVideoSeamStart = 0
            }
            compactAltVideoSeam()
            altVideoSeam.append(contentsOf: payload)
            drainAltVideo()
            pendingAltVideoBytes.wrappingSubtract(payload.count, ordering: .relaxed)
        }
    }

    /// One line per new-source reset that actually discarded something. Silent when the buffer was
    /// already empty (the common, healthy case) so a clean re-SETUP does not log.
    private func noteNewSource(_ kind: StreamKind, held: Int) {
        guard held > 0 else { return }
        log.notice("\(kind.label, privacy: .public): new seam producer — dropped \(held, privacy: .public) B of the previous producer's partial message")
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
    func feedAudio(_ payload: [UInt8], newSource: Bool = false) {
        let after = pendingAudioBytes.wrappingAdd(payload.count, ordering: .relaxed).newValue
        dispatchDecrypt(after: after, on: audioQueue) { [self] in
            stats.withLock { h in
                h.av.mediaAudioRxFrames &+= 1
                h.av.mediaAudioRxBytes &+= UInt64(payload.count)
            }
            if newSource {
                noteNewSource(.mediaAudio, held: audioSeam.bytes.count)
                audioSeam.bytes.removeAll(keepingCapacity: true)
            }
            audioSeam.bytes.append(contentsOf: payload)
            drainAudio(&audioSeam, kind: .mediaAudio)
            pendingAudioBytes.wrappingSubtract(payload.count, ordering: .relaxed)
        }
    }

    /// Feed raw CH_ALT_AUDIO (voice sink: telephony/speechRecognition/alert/default) payload bytes.
    /// Identical framing to the media seam; only the buffer differs (separate TCP seams on the box).
    func feedVoice(_ payload: [UInt8], newSource: Bool = false) {
        let after = pendingAudioBytes.wrappingAdd(payload.count, ordering: .relaxed).newValue
        dispatchDecrypt(after: after, on: audioQueue) { [self] in
            stats.withLock { h in
                h.av.voiceAudioRxFrames &+= 1
                h.av.voiceAudioRxBytes &+= UInt64(payload.count)
            }
            if newSource {
                noteNewSource(.voiceAudio, held: voiceSeam.bytes.count)
                voiceSeam.bytes.removeAll(keepingCapacity: true)
            }
            voiceSeam.bytes.append(contentsOf: payload)
            drainAudio(&voiceSeam, kind: .voiceAudio)
            pendingAudioBytes.wrappingSubtract(payload.count, ordering: .relaxed)
        }
    }

    // MARK: - Seam parsing

    /// Pull the next audio message out of `s`, returning `[marker][payload]` — length prefix and (when
    /// present) SEAM_MAGIC stripped, so `drainAudio`'s marker switch is identical for both framings.
    ///
    /// The magic is what makes a re-SETUP survivable. ocbmd replaces a seam producer WITHOUT draining
    /// the old one, so this buffer can be holding half a message when the new producer's bytes arrive;
    /// before 2026-09-03 the audio seam had no magic, the new SEAM_KEY landed mid-message, and the lane
    /// desynced permanently (device-proven: bogus keys, a "1469658167Hz 232ch" format, silence).
    /// `OCBM.fNewSource` normally clears the remainder before we get here — this is the recovery for
    /// when it does not (an older box, or a lost frame).
    private func nextAudioMessage(_ s: inout AudioSeamBuf, kind: StreamKind) -> [UInt8]? {
        let M = Self.seamMagic
        while s.bytes.count >= 5 { // [len 4][marker] is the smallest thing either framing can start with
            let mlen = (Int(s.bytes[0]) << 24) | (Int(s.bytes[1]) << 16) | (Int(s.bytes[2]) << 8) | Int(s.bytes[3])
            if s.framing == .unknown {
                if s.bytes.count >= 8, s.bytes[4] == M[0], s.bytes[5] == M[1], s.bytes[6] == M[2], s.bytes[7] == M[3] {
                    s.framing = .magic
                } else if Self.legacyAudioLengthPlausible(s.bytes, mlen: mlen) {
                    // Only latch LEGACY on bytes that actually parse as a legacy message — a magic-framed
                    // message fails this (its byte 4 is 'S', not a marker), so the two can never be
                    // confused, and a mid-message resync cannot latch the wrong framing off junk.
                    s.framing = .legacy
                    if !loggedLegacyAudioFraming {
                        loggedLegacyAudioFraming = true
                        log.notice("legacy audio framing detected (no SEAM_MAGIC) — this box predates the self-syncing audio seam; parsing [u32 BE len][marker]. A re-SETUP on this build can still desync the lane.")
                    }
                } else if s.bytes.count < 8 {
                    return nil // not enough bytes to tell the two framings apart yet
                } else {
                    s.bytes.removeFirst(1) // neither framing fits — byte-resync
                    continue
                }
            }
            if s.framing == .legacy {
                if mlen < 1 || mlen > Self.maxAudioMessage {
                    s.bytes.removeFirst(1)
                    continue
                }
                if s.bytes.count < 4 + mlen {
                    if s.bytes.count > 8 * OCBM.maxPayload { s.bytes.removeFirst(1); continue } // never completes
                    return nil
                }
                let msg = Array(s.bytes[4..<4 + mlen])
                s.bytes.removeFirst(4 + mlen)
                return msg
            }
            // .magic
            if s.bytes.count < 8 { return nil } // need [len][magic] before anything can be judged
            let magicOK = s.bytes[4] == M[0] && s.bytes[5] == M[1] && s.bytes[6] == M[2] && s.bytes[7] == M[3]
            if !magicOK || !Self.audioLengthPlausible(s.bytes, mlen: mlen) {
                if !resyncAudioToMagic(&s) { return nil }
                continue
            }
            if s.bytes.count < 4 + mlen {
                if s.bytes.count > 8 * OCBM.maxPayload { // a false magic declared a length that never arrives
                    if !resyncAudioToMagic(&s) { return nil }
                    continue
                }
                return nil
            }
            let msg = Array(s.bytes[8..<4 + mlen]) // strip [len][magic] → [marker][payload]
            s.bytes.removeFirst(4 + mlen)
            return msg
        }
        return nil
    }

    /// Magic-framed audio message: `[len 4][magic 4][marker][…]`. The box emits exactly three shapes and
    /// two of them are FIXED length, so — as on the video lane (`videoLengthPlausible`) — "magic verified
    /// ⇒ mlen trustworthy" is structural rather than probabilistic. After a resync the 4 bytes before the
    /// magic are whatever preceded it, and a false magic inside RTP ciphertext would otherwise hand us an
    /// arbitrary length that swallows every following message. An unknown marker is rejected for the same
    /// reason: a random false magic carries a random marker.
    private static func audioLengthPlausible(_ b: [UInt8], mlen: Int) -> Bool {
        guard mlen >= 5, mlen <= maxAudioMessage else { return false }
        guard b.count >= 9 else { return true } // marker not buffered yet — can't judge; wait for it
        switch b[8] {
        case 0x00: return mlen == 45                 // SEAM_KEY:    [magic 4][0x00][key 32][scid 8]
        case 0x02: return mlen == 21                 // SEAM_FORMAT: [magic 4][0x02][scid 8][7 fields]
        case 0x01: return mlen >= 4 + 1 + 8 + 36     // SEAM_PKT: RTP hdr 12 + tag 16 + nonce 8 is the floor
        // SEAM_PKT_PLAIN: [magic 4][0x03][scid 8][raw PCM]. No crypto trailer, so the floor is just one
        // 16-bit sample of payload — enough to reject a false magic whose next byte happens to be 0x03
        // and whose length lands inside the header.
        case OCBM.seamPktPlain: return mlen >= 4 + 1 + 8 + 2
        default: return false
        }
    }

    /// Legacy (pre-magic) audio message: `[len 4][marker][…]`, same three shapes without the magic.
    /// Used ONLY to decide, once, that a seam is speaking the old framing.
    private static func legacyAudioLengthPlausible(_ b: [UInt8], mlen: Int) -> Bool {
        guard b.count >= 5, mlen >= 1, mlen <= maxAudioMessage else { return false }
        switch b[4] {
        case 0x00: return mlen == 41
        case 0x02: return mlen == 17
        case 0x01: return mlen >= 1 + 8 + 36
        default: return false
        }
    }

    /// Scan for the next SEAM_MAGIC and drop everything before its length prefix (magic − 4), so parsing
    /// re-aligns on a clean message boundary. The video lane's equivalent is cursor-based over a
    /// multi-MB buffer; the audio buffers are small and consumed with `removeFirst`, so the two bodies
    /// differ. Returns false if no magic is buffered yet (need more bytes).
    private func resyncAudioToMagic(_ s: inout AudioSeamBuf) -> Bool {
        let M = Self.seamMagic
        var i = 5 // past the current (bad) position; i − 4 ≥ 1 guarantees forward progress
        while i + 4 <= s.bytes.count {
            if s.bytes[i] == M[0] && s.bytes[i + 1] == M[1] && s.bytes[i + 2] == M[2] && s.bytes[i + 3] == M[3] {
                s.bytes.removeFirst(i - 4) // leave [len][magic]… aligned (magic sits at offset 4)
                return true
            }
            i += 1
        }
        // No magic buffered — keep a short tail (one could straddle the next feed) and wait.
        if s.bytes.count > 7 { s.bytes.removeFirst(s.bytes.count - 7) }
        return false
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
            // L1: cross-check `mlen` against the message's own plaintext header BEFORE committing to
            // wait for `4 + mlen` bytes — see `videoLengthPlausible`. Needs [len 4][magic 4][marker 1]
            // [seq 8][bodySize 4] = 21 bytes; below that we can't judge yet and fall through to the wait.
            if videoSeam.count - s >= 21, !Self.videoLengthPlausible(videoSeam, s, mlen: mlen) {
                if !resyncVideoToMagic() { return nil }
                continue
            }
            if videoSeam.count - s < 4 + mlen {
                // Magic verified AND (once 21 B are in) the header's own bodySize agrees with `mlen`, so
                // `mlen` is trustworthy — just wait for the rest of this frame. (A large 4K IDR
                // legitimately streams in across many OCBM frames; do NOT resync early.)
                return nil
            }
            let msg = Array(videoSeam[(s + 8)..<(s + 4 + mlen)]) // strip [len][magic] → [marker][payload]
            videoSeamStart = s + 4 + mlen
            return msg
        }
        return nil
    }

    /// A video message is `[len 4][magic 4][marker][…]`; the box emits exactly two shapes and both have
    /// a length the message's own PLAINTEXT header determines: a frame is `[magic][0x01][seq 8][hdr 128]
    /// [body]` with `mlen == 141 + hdr.bodySize` (`hdr[0..4]` LE32, `session.rs` `forward_screen2`), a key
    /// is `[magic][0x00][key 32][scid 8]` = 45. Checking that makes "magic verified ⇒ mlen trustworthy"
    /// STRUCTURAL rather than probabilistic: after a resync the 4 bytes before the magic are whatever
    /// preceded it, and a false magic inside ciphertext would otherwise hand us an arbitrary mlen (up to
    /// 16 MB) that swallows every following frame into one garbage message. Caller must have ≥ 21 bytes
    /// from `s`.
    ///
    /// An UNKNOWN marker is rejected too, and that is the point: a random false magic carries a random
    /// marker, so ~255/256 of the ones that survive the `mlen` range test would otherwise land in the
    /// "wait for up to 16 MB" branch. Rejecting costs nothing against a future box marker either — the
    /// drain's `default:` drops unknown markers anyway, and the resync re-aligns on the very next magic,
    /// which is exactly where that message ends.
    private static func videoLengthPlausible(_ seam: [UInt8], _ s: Int, mlen: Int) -> Bool {
        switch seam[s + 8] {
        case 0x00: return mlen == 45
        case 0x01:
            let bodySize = Int(seam[s + 17]) | (Int(seam[s + 18]) << 8)
                         | (Int(seam[s + 19]) << 16) | (Int(seam[s + 20]) << 24)
            return mlen == 141 + bodySize
        default: return false
        }
    }

    /// Scan for the next SEAM_MAGIC and drop everything before its length prefix (magic − 4), so parsing
    /// re-aligns on a clean frame boundary. Returns false if no magic is buffered yet (need more bytes).
    private func resyncVideoToMagic() -> Bool {
        let M = Self.seamMagic
        let from = videoSeamStart
        var i = videoSeamStart + 5 // start past the current (bad) position
        while i + 4 <= videoSeam.count {
            if videoSeam[i] == M[0] && videoSeam[i+1] == M[1] && videoSeam[i+2] == M[2] && videoSeam[i+3] == M[3] {
                videoSeamStart = i - 4 // leave [len][magic]… aligned (magic is at offset 4)
                videoSawMagic = true
                return true
            }
            i += 1
        }
        // No magic found — keep a small tail (a magic could straddle the next feed) and wait for more.
        if videoSeam.count - videoSeamStart > 7 { videoSeamStart = videoSeam.count - 7 }
        // L5: a seam that has NEVER carried a magic is not a torn frame, it is the wrong wire.
        if !videoSawMagic {
            videoNoMagicBytes += videoSeamStart - from
            if videoNoMagicBytes >= Self.noMagicWarnBytes {
                let mb = videoNoMagicBytes / (1024 * 1024)
                log.error("main video: \(mb, privacy: .public) MB discarded with NO SEAM_MAGIC — the box is not forward-encrypting (OCBM_FWD_ENC=0 sends [u32 len][Annex-B], no magic/marker/key); this host cannot decode that wire")
                videoNoMagicBytes = 0 // throttle: re-warn every 4 MB
            }
        }
        return false
    }

    private func drainVideo() {
        while let msg = nextVideoMessage() {
            guard let marker = msg.first else { continue }
            switch marker {
            case 0x00 where msg.count >= 33: // [0x00][key 32][scid 8]
                videoKey = SymmetricKey(data: Data(msg[1..<33]))
                haveVideoSeq = false
                // A new key is a new stream/sender: the keyframe-bit proof from the previous one no
                // longer applies, so re-earn it (worst case is the always-safe OR-both path for longer).
                videoNoKeyDrops = 0
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
                    guard let key = videoKey else {
                        // Silent otherwise: no key ⇒ every frame is discarded and both tallies stay 0.
                        videoNoKeyDrops += 1
                        if videoNoKeyDrops == 1 || videoNoKeyDrops % 300 == 0 {
                            log.error("video frame dropped — no SEAM_KEY received yet (\(self.videoNoKeyDrops, privacy: .public) frames)")
                        }
                        break
                    }
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
                        // (msg[9+5]=msg[14]); on-wire-confirmed vs the NAL walk. Self-calibrating: once an
                        // IDR proves the sender sets it, trust the byte test alone (retiring the NAL walk);
                        // until then — and forever on a legacy sender that never sets it — OR with the NAL
                        // walk so detection never regresses (supersedes the A2 OR-both). The latch needs
                        // CORROBORATION (bit AND walk on the SAME frame): the flag is an empirical
                        // correlation, not documented Apple semantics, so a sender that sets bit 4 for
                        // anything else must never retire the walk.
                        // Never retire the walk: the bit is not set on every IDR (measured 2026-09-03 —
                        // trusting it alone after one corroboration dropped every later IDR on the alt
                        // lane and failed every frame after it), so an IDR is whatever either test says.
                        let kfBit = (msg[14] & 0x10) != 0
                        let kf = kfBit || Self.hevcHasIRAP(plain) || Self.avccHasIDR(plain)
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
            if altVideoSeam.count - s >= 21, !Self.videoLengthPlausible(altVideoSeam, s, mlen: mlen) {
                if !resyncAltVideoToMagic() { return nil } // L1 — see the main lane
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
        let from = altVideoSeamStart
        var i = altVideoSeamStart + 5
        while i + 4 <= altVideoSeam.count {
            if altVideoSeam[i] == M[0] && altVideoSeam[i+1] == M[1] && altVideoSeam[i+2] == M[2] && altVideoSeam[i+3] == M[3] {
                altVideoSeamStart = i - 4
                altVideoSawMagic = true
                return true
            }
            i += 1
        }
        if altVideoSeam.count - altVideoSeamStart > 7 { altVideoSeamStart = altVideoSeam.count - 7 }
        if !altVideoSawMagic { // L5 — see the main lane
            altVideoNoMagicBytes += altVideoSeamStart - from
            if altVideoNoMagicBytes >= Self.noMagicWarnBytes {
                let mb = altVideoNoMagicBytes / (1024 * 1024)
                log.error("ALT video: \(mb, privacy: .public) MB discarded with NO SEAM_MAGIC — box not forward-encrypting (OCBM_FWD_ENC=0); this host cannot decode that wire")
                altVideoNoMagicBytes = 0
            }
        }
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
                altVideoNoKeyDrops = 0
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
                    guard let key = altVideoKey else {
                        altVideoNoKeyDrops += 1
                        if altVideoNoKeyDrops == 1 || altVideoNoKeyDrops % 300 == 0 {
                            log.error("ALT video frame dropped — no SEAM_KEY received yet (\(self.altVideoNoKeyDrops, privacy: .public) frames)")
                        }
                        break
                    }
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
                        // Self-calibrating keyframe bit (see main lane): hdr[5]&0x10 = msg[14] & 0x10,
                        // latched only when the NAL walk corroborates it on the same frame.
                        let kfBit = (msg[14] & 0x10) != 0
                        let kf = kfBit || Self.hevcHasIRAP(plain) || Self.avccHasIDR(plain)
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
    private func drainAudio(_ seam: inout AudioSeamBuf, kind: StreamKind) {
        while let msg = nextAudioMessage(&seam, kind: kind) {
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
                guard let key = audioKeys[scid] else {
                    // Was a SILENT break — the exact signature of a desynced seam (packets flowing, no
                    // key for their scid) presented as nothing at all in the log. Throttled: first, then
                    // every 500. Also counted in the stats snapshot so AVmon can show it.
                    let n = stats.withLock { h -> UInt64 in
                        h.av.audioNoKeyDrops &+= 1
                        return h.av.audioNoKeyDrops
                    }
                    if n == 1 || n % 500 == 0 {
                        log.error("audio pkt dropped — no key for scid=\(scid, privacy: .public) (\(n, privacy: .public) dropped, \(kind.label, privacy: .public))")
                    }
                    break
                }
                if let plain = Self.decryptAudio(pkt: pkt, key: key) {
                    stats.withLock { h in
                        h.av.audioOK &+= 1
                        h.metrics.record(kind, bytes: mlen, sizeSample: UInt32(clamping: pkt.count),
                                         gap: false, fail: false, nowNs: 0)
                    }
                    let pt = pkt[1] & 0x7f
                    let seen = audioTraceCount[scid, default: 0]
                    if seen < 8 {
                        audioTraceCount[scid] = seen + 1
                        let head = plain.prefix(12).map { String(format: "%02x", $0) }.joined()
                        log.info("audio pkt trace scid=\(scid) pt=\(pt) len=\(plain.count) head=\(head, privacy: .public)")
                    }
                    // Streams SETUP with `supportsRTPPacketRedundancy` carry RFC 2198 payloads: redundant
                    // older access units in front of the primary one. Hand the codec the primary only.
                    let au = Self.rfc2198Primary(plain) ?? plain
                    delegate?.avDidReceiveAudio(au, scid: scid, format: audioFormats[scid])
                } else {
                    stats.withLock { h in
                        h.av.audioFail &+= 1
                        h.metrics.record(kind, bytes: mlen, sizeSample: nil, gap: false, fail: true, nowNs: 0)
                    }
                    anomalyLog?.record(.decodeFail, stream: kind, detail: "audio decrypt failed",
                                       tMonoMs: AVAnomalyLog.monoMs(), tWall: Date().timeIntervalSince1970)
                }
            case OCBM.seamPktPlain where msg.count >= 9: // SEAM_PKT_PLAIN: [0x03][scid 8][raw PCM]
                // The Android Auto telephony lane. Narrowband: the box has HFP/SCO PCM (CVSD, 8 kHz mono
                // S16LE); WIDEBAND (SEAM_FORMAT codec 4 = mSBC): the payload is one raw transparent-eSCO
                // read — H2 header + 57-byte mSBC frame + pad — which OCBMAVBridge decodes. Either way the
                // box has nothing to encrypt it WITH — there is no AirPlay stream, hence no SEAM_KEY — so the
                // payload rides verbatim and is delivered AS-IS: no ChaCha20 open, no RFC 2198 demux
                // (HFP carries no redundancy bundles, and the demux would happily "find" one inside PCM).
                let scid = Self.readU64LE(msg, at: 1)
                let mlen = msg.count
                guard var fmt = audioFormats[scid] else {
                    // Without SEAM_FORMAT we do not know the rate/channels; playing 8 kHz mono as the
                    // 48k/2ch default is chipmunk noise, so drop. Logged once per scid — a box that
                    // starts the lane without its format would otherwise log 50×/s.
                    let n = stats.withLock { h -> UInt64 in
                        h.av.audioPlainNoFormatDrops &+= 1
                        return h.av.audioPlainNoFormatDrops
                    }
                    if plainNoFormatLogged.insert(scid).inserted {
                        log.error("plain audio pkt dropped — no SEAM_FORMAT for scid=\(scid, privacy: .public) (\(n, privacy: .public) dropped, \(kind.label, privacy: .public))")
                    }
                    break
                }
                let au = Data(msg[9...])
                stats.withLock { h in
                    h.av.audioPlainOK &+= 1
                    h.metrics.record(kind, bytes: mlen, sizeSample: UInt32(clamping: au.count),
                                     gap: false, fail: false, nowNs: 0)
                }
                let seen = audioTraceCount[scid, default: 0]
                if seen < 8 {
                    audioTraceCount[scid] = seen + 1
                    let head = au.prefix(12).map { String(format: "%02x", $0) }.joined()
                    log.info("audio pkt trace scid=\(scid) plain len=\(au.count) head=\(head, privacy: .public)")
                }
                fmt.plainLE = true // host-endian HFP PCM, not the big-endian CarPlay wire
                delegate?.avDidReceiveAudio(au, scid: scid, format: fmt)
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
    // take any DataProtocol, so no copy is added HERE — but the frame is still copied out of the seam by
    // the caller (`Array(videoSeam[…])`) and again by `SealedBox`, so this is not an end-to-end zero-copy
    // path (L3, docs/swift_review_20260902/02_ocbm_av_decrypt.md); it is 3 copies, not 5.
    static func decryptVideoFrame(hdr: ArraySlice<UInt8>, body: ArraySlice<UInt8>, key: SymmetricKey, counter: UInt64) -> Data? {
        guard body.count >= 16 else { return nil }
        var nonceBytes = [UInt8](repeating: 0, count: 12) // [0,0,0,0] ++ counter_le64
        let le = counter.littleEndian
        withUnsafeBytes(of: le) { raw in for i in 0..<8 { nonceBytes[4 + i] = raw[i] } }
        let ct = body.dropLast(16)
        let tag = body.suffix(16)
        return Self.open(nonce: nonceBytes, ciphertext: ct, tag: tag, aad: hdr, key: key)
    }

    /// RFC 2198 demux (CarPlaySDK enables "RFC2198 redundancy" per stream when the SETUP dict carries
    /// `supportsRTPPacketRedundancy`): `[F=1|PT][TS-offset 14][len 10]` 4-byte headers for each redundant
    /// block, then a 1-byte `[F=0|PT]` terminator for the primary, then the data blocks in the same
    /// order — the primary is the LAST block and its length is the remainder. Returns nil when the
    /// payload does not parse as a consistent redundancy bundle (a plain access unit), so the caller
    /// falls back to the payload as-is; the walk is bounded and every length must fit.
    static func rfc2198Primary(_ plain: Data) -> Data? {
        let b = [UInt8](plain)
        var off = 0
        var redundantBytes = 0
        var headers = 0
        var pt: UInt8? = nil
        while off < b.count {
            let byte = b[off]
            let thisPT = byte & 0x7f
            if pt == nil { pt = thisPT } else if pt != thisPT { return nil }
            if byte & 0x80 != 0 {
                guard off + 4 <= b.count, headers < 3 else { return nil }
                let len = (Int(b[off + 2] & 0x03) << 8) | Int(b[off + 3])
                guard len > 0 else { return nil }
                redundantBytes += len
                headers += 1
                off += 4
            } else {
                off += 1
                guard headers > 0 else { return nil } // a 1-byte header alone is indistinguishable from data
                let primaryStart = off + redundantBytes
                guard primaryStart < b.count else { return nil }
                return plain.subdata(in: primaryStart..<b.count)
            }
        }
        return nil
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
