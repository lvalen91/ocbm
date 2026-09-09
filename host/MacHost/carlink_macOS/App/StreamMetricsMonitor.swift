// StreamMetricsMonitor.swift — the live A/V stream-performance monitor's driver + Settings surface.
//
// The pure counting lives in OCBM/StreamMetrics.swift (IOKit/AppKit-free, unit-tested). This file is the
// UI/side-effect layer that must NOT ride into the headless test set:
//
//   • A ~1 Hz main-queue timer, armed while an OCBM session is live (started/stopped by AppDelegate),
//     reads two consecutive `OCBMAVDecrypt.metricsSnapshot(at:)` values and derives a StreamMetricsReport.
//     The read is a single Mutex acquisition on the main queue — it never blocks the A/V hot path.
//   • It publishes `report`/`active`/`uptime` for the CCPA Settings section (`StreamPerfSection`).
//   • Every tick it writes a tiny JSON snapshot atomically to `/tmp/carlink_metrics.json` (schema below)
//     so an external tool — or a future session — can read LIVE metrics without scraping the log.
//   • Every ~6 s it emits ONE compact "AVmon …" line to the app log (throttled so it never floods).
//
// ── /tmp/carlink_metrics.json schema (v2, stable) ────────────────────────────────────────────────────
//   {
//     "schema": 2,
//     "ts": 1723200000.421,       // wall-clock unix seconds this snapshot was written
//     "sessionActive": true,       // false is written once on teardown, then updates stop
//     "uptimeSec": 42.1,           // seconds since the session (monitor) started
//     "dt": 1.003,                 // seconds the rates below are averaged over
//     "decodeLatencyMs": null,     // STILL null — real VideoToolbox decode timing is not observable
//                                  //   through this render path. See wrapLatencyMs/handoffLatencyMs.
//     "streams": {
//       "mainVideo":  { ...perStream... },  "altVideo": {...},
//       "mediaAudio": { ...perStream... },  "voiceAudio": {...}
//     },
//     "aa": {                      // null unless an ANDROID AUTO session is live; AA bypasses the
//       "transport": "wired",      //   decrypt layer, so "streams" above stays all-zero during one
//       "dt": 1.0,
//       "videoRxPerSec": 30.0, "videoDecodedPerSec": 30.0, "videoDropPerSec": 0.0,
//       "audioMediaPerSec": 43.0, "audioGuidancePerSec": 0.0, "audioSystemPerSec": 0.0,
//       "audioTelephonyPerSec": 50.0,   // box HFP/SCO lane (SEAM_PKT_PLAIN) + the AA telephony sink
//       "micPerSec": 50.0, "rxMbps": 12.3, "txMbps": 0.4, "backlog": 0,
//       "telephonyRxPerSec": 50.0,      // box HFP/SCO → CH_ALT_AUDIO SEAM_PKT_PLAIN frames/s
//       "micUplinkPerSec": 50.0         // host → CH_MIC 20 ms frames/s
//     },
//     "recent_events": [ {t, tMonoMs, kind, stream, detail}, … ],  // oldest → newest (most-recent LAST)
//     "eventCounts": { "freeze": 1, "silence": 2, … }              // per-kind cumulative this session
//   }
//   perStream = {
//     "mbps": 12.34,               // received megabits/sec over the interval
//     "fps": 30.0,                 // VIDEO: frames/sec · AUDIO: packets/sec (same key)
//     "state": "active"|"idle",    // idle = iOS conserving (static screen / no audio) — NOT a fault
//     "avgKB": 41.2, "minKB": 3.1, "maxKB": 210.0,   // frame-size stats over the interval
//     "gaps": 2, "gapsPerSec": 2.0,                  // lost-frame (seq-gap) events
//     "decryptFails": 0,
//     "jitterMs": 1.8,             // smoothed inter-arrival jitter (video only; 0 for audio)
//     "codec": "AAC-LC 48000Hz 2ch" | "HEVC" | "H.264" | null,  // audio format / video codec
//     "decodedFps": 30.0,          // VIDEO only (main/alt): sample buffers built/sec (AD). fps≈decodedFps → no in-app loss
//     "dropFps": 0.0,              // VIDEO only (main/alt): decode/enqueue-FIFO losses/sec; fps≫decodedFps → in-app queue drop
//     "wrapLatencyMs": 0.1,        // VIDEO only: EWMA arrival→CMSampleBuffer built. A PARSE/WRAP time, NOT decode
//     "handoffLatencyMs": 1.2      // VIDEO only: EWMA arrival→handed to the renderer (adds the enqueue-FIFO wait)
//   }
//   recent_events[].kind ∈ { freeze, fps_drop, decode_fail, gap    (video)
//                            underrun, silence, click, clip, audio_gap, seq_loss (audio) }
//   FREEZE/fps_drop are BASELINE-RELATIVE (judged against a stream's own recent rate, with an ACTIVE/
//   IDLE hysteresis) — a low fps while "idle" is normal demand-driven throttling, never an anomaly.
// ─────────────────────────────────────────────────────────────────────────────────────────────────────
//
// PIPELINE latency is real as of 2026-09-03; DECODE latency is still not measurable here, and the field
// names now say so. `VideoDecoder` measures two EWMAs against each frame's arrival on the USB read
// queue — `wrapLatencyMs` (arrival → CMSampleBuffer built) and `handoffLatencyMs` (arrival → handed to
// the renderer) — and AVmon prints them as `wraplat=<main>/<alt>ms`, rendered `wrap>handoff`.
//
// The first of those shipped for one session as `decodeLatencyMs`/`declat`, which was a LIE by omission:
// it times a zero-copy CMBlockBuffer wrap (~0.1 ms), not decode. Real decode happens inside the renderer
// after the hand-off, and `AVSampleBufferVideoRenderer.Receiver` exposes no VTDecompressionSession and no
// per-frame completion — only synchronous accept/reject and a failures-only event stream. So the
// top-level `decodeLatencyMs` JSON key is back to `null` and stays reserved for a build that routes
// frames through an explicit VTDecompressionSession. `handoffLatencyMs` is the number that matters for
// the bounded FIFOs: it is exactly what the queue depth buys or costs (read it beside `dropFps`). The
// received-frame `jitterMs` remains the upstream-arrival proxy.

import Combine
import Foundation
import SwiftUI
import Synchronization
import os

@MainActor
final class StreamMetricsMonitor: ObservableObject {
    static let shared = StreamMetricsMonitor()

    /// The latest derived report (nil before the second tick). Drives the Settings section.
    @Published private(set) var report: StreamMetricsReport?
    @Published private(set) var active = false
    @Published private(set) var uptime: TimeInterval = 0
    /// Newest-first slice of the anomaly ring for the Settings list (republished each tick).
    @Published private(set) var recentEvents: [AnomalyEvent] = []
    @Published private(set) var eventCounts: [String: Int] = [:]
    /// Per-second Android Auto rates, or nil when no AA session is live. AA traffic rides CH_IP →
    /// AASession and never reaches OCBMAVDecrypt, so without this the Settings panel showed four
    /// all-zero CarPlay rows for an entire AA drive.
    @Published private(set) var aa: AARates?
    /// The BOX-side telephony lane, which is not AA's own: SEAM_PKT_PLAIN access units arriving on
    /// CH_ALT_AUDIO (the phone's call audio, bridged from Bluetooth HFP/SCO by the box) and the 20 ms
    /// CH_MIC frames going back. Published separately from `aa` because they are OCBM channels — an
    /// AA projection can be up with neither of them flowing, and a call can flow with AA's own
    /// telephony sink (the AA_TELEPHONY_SINK experiment) at zero.
    @Published private(set) var telephonyRxPerSec = 0.0
    @Published private(set) var micUplinkPerSec = 0.0

    /// The shared anomaly log — fed by OCBMAVDecrypt (video) + AudioPlayer (audio), polled here, read by
    /// the JSON writer / log / Settings. AppDelegate hands this same instance to both feeders at setup.
    let anomalyLog = AVAnomalyLog()

    private weak var av: OCBMAVDecrypt?
    private var timer: Timer?
    private var previous: StreamMetricsSnapshot?
    private var startedAt: Double = 0          // ProcessInfo.systemUptime seconds
    private var tick = 0
    private var lastEventTotal: UInt64 = 0     // anomaly total at the last log summary

    // Telephony (SEAM_PKT_PLAIN) + mic-uplink rates for the AVmon line. Both are cumulative counters
    // owned by other threads (the audio decrypt queue and the mic tap), diffed here at 1 Hz.
    private var lastTelFrames: UInt64 = 0
    private var lastMicTxFrames: UInt64 = 0
    private var lastRateAt: Double = 0
    private var telPerSec = 0.0
    private var micTxPerSec = 0.0

    /// The newest AA counter snapshot, published from AASession's own loop thread. A Mutex box rather
    /// than a hop to the main actor: AASession runs a blocking event loop and must never await the UI.
    private nonisolated static let aaBox = Mutex<AAStatsSnapshot?>(nil)
    private var lastAA: AAStatsSnapshot?

    /// AASession → monitor, once per its 1 Hz stats tick. `nil` on teardown hides the AA rows.
    nonisolated static func publishAA(_ snapshot: AAStatsSnapshot?) {
        aaBox.withLock { $0 = snapshot }
    }

    // AD (decoded/sec) + slot-drop/sec accounting (perf 2026-08-09): the two decoders' cross-thread
    // counters, diffed each sample. AR (fps, decrypt layer) − decodedFps = decode-slot loss — surfaced in
    // the JSON so a live session localizes cluster loss (upstream starvation vs in-app slot drop).
    private weak var mainDecoder: VideoDecoder?
    private weak var altDecoder: VideoDecoder?
    private var lastMainDecoded: UInt64 = 0
    private var lastAltDecoded: UInt64 = 0
    private var lastMainDrops: UInt64 = 0
    private var lastAltDrops: UInt64 = 0

    private let log = Logger(subsystem: "com.carlink.ocbm", category: "avmon")
    private nonisolated static let jsonURL = URL(fileURLWithPath: "/tmp/carlink_metrics.json")
    /// JSON writes + the atomic rename run off-main so no file I/O touches the UI/timer beat.
    private static let ioQueue = DispatchQueue(label: "carplay-host.avmon.io", qos: .utility)
    /// L6: logger + one-shot flag for the write-failure path below. `nonisolated(unsafe)` because both
    /// are touched only from `ioQueue`, which is serial — no concurrent access is possible even though
    /// the compiler can't see that queue confinement.
    private nonisolated static let staticLog = Logger(subsystem: "com.carlink.ocbm", category: "avmon")
    nonisolated(unsafe) private static var loggedWriteFailure = false

    /// AppDelegate.setupDevice: bind to the live decrypt layer and arm the 1 Hz sampler.
    func start(av: OCBMAVDecrypt) {
        self.av = av
        previous = nil
        report = nil
        tick = 0
        lastEventTotal = 0
        anomalyLog.reset()
        recentEvents = []
        eventCounts = [:]
        startedAt = ProcessInfo.processInfo.systemUptime
        uptime = 0
        active = true
        timer?.invalidate()
        let t = Timer(timeInterval: 1.0, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.sample() }
        }
        t.tolerance = 0.1   // L8: ~10% coalescing window, harmless since dt is measured, not assumed
        RunLoop.main.add(t, forMode: .common)   // .common so it keeps firing during menu/resize tracking
        timer = t
    }

    /// Bind the two video decoders so the sampler can surface AD (decoded/sec) + slot-drop/sec per lane.
    /// Called from AppDelegate.setupDevice after both decoders exist; weak so it never keeps them alive.
    func bindDecoders(main: VideoDecoder?, alt: VideoDecoder?) {
        mainDecoder = main
        altDecoder = alt
        lastMainDecoded = 0; lastAltDecoded = 0; lastMainDrops = 0; lastAltDrops = 0
    }

    /// AppDelegate.endSession: disarm and publish a final `sessionActive:false` JSON so external readers
    /// see the session ended rather than a stale-but-active snapshot.
    func stop() {
        timer?.invalidate()
        timer = nil
        av = nil
        active = false
        Self.publishAA(nil)
        lastAA = nil
        aa = nil
        telPerSec = 0; micTxPerSec = 0; lastRateAt = 0
        telephonyRxPerSec = 0; micUplinkPerSec = 0
        // L5: write the final snapshot BEFORE clearing `report` — otherwise the teardown JSON shows
        // all-zero streams while `sessionActive:false`, contradicting the "post-mortem snapshot" the
        // JSON's own schema comment promises.
        writeJSON(active: false)
        report = nil
        previous = nil
    }

    private func sample() {
        guard let av else { return }
        uptime = ProcessInfo.processInfo.systemUptime - startedAt
        let now = ProcessInfo.processInfo.systemUptime
        let cur = av.metricsSnapshot(at: now)
        if let prev = previous {
            report = StreamMetricsReport.between(prev, cur)
        }
        previous = cur
        // Catch an ACTIVE video stream that stopped delivering frames (no frame arrives to trigger the
        // per-frame detector). Baseline-relative — never fires while a stream is legitimately idle.
        // Telephony + mic-uplink per-second rates (AVmon `tel=`/`mictx=`, and the AA panel's own rows).
        // Diffed against wall time, not the tick count: the timer has a 0.1 s tolerance.
        let dtRate = lastRateAt > 0 ? now - lastRateAt : 0
        lastRateAt = now
        let st = av.statsSnapshot()
        let micTx = OCBMClient.micTxFrames.load(ordering: .relaxed)
        if dtRate > 0 {
            telPerSec = Double(st.audioPlainOK &- lastTelFrames) / dtRate
            micTxPerSec = Double(micTx &- lastMicTxFrames) / dtRate
        }
        lastTelFrames = st.audioPlainOK
        lastMicTxFrames = micTx
        telephonyRxPerSec = telPerSec
        micUplinkPerSec = micTxPerSec
        // Android Auto: convert AASession's cumulative counters against the previous published tick.
        // A snapshot older than 5 s means the AA loop stopped (session torn down without a shutdown,
        // or wedged) — stale rates on screen are worse than none.
        if let cur = Self.aaBox.withLock({ $0 }), now - cur.t < 5.0 {
            if let prev = lastAA, cur.t > prev.t {
                aa = AARates.between(prev, cur)
            } else if lastAA == nil {
                aa = AARates()   // first tick: show the section with zeros rather than nothing
            }
            lastAA = cur
        } else {
            lastAA = nil
            aa = nil
        }
        anomalyLog.pollVideo(tMonoMs: AVAnomalyLog.monoMs(), tWall: Date().timeIntervalSince1970)
        recentEvents = anomalyLog.events().suffix(8).reversed()   // newest-first for the Settings list
        eventCounts = anomalyLog.counts()
        writeJSON(active: true)
        tick += 1
        if tick % 6 == 0 { logSummary() }   // ~every 6 s
    }

    /// Per-stream ACTIVE/IDLE label for display: video from the activity detector; audio from whether
    /// packets are flowing this interval. IDLE is normal (iOS conserving / nothing playing), not a fault.
    func stateLabel(_ kind: StreamKind) -> String {
        if kind.isVideo { return anomalyLog.videoState(kind).rawValue }
        return (report?[kind].framesPerSec ?? 0) > 0 ? "active" : "idle"
    }

    // MARK: - Compact log line (throttled)

    private func logSummary() {
        guard let r = report else { return }
        // RAW OCBM arrival on the two audio channels, and the no-key drop tally. `a=`/`voice=` below are
        // DECRYPT SUCCESS; when the seam desyncs those go to zero while bytes keep pouring in, and
        // without `arx=` the log reads identically to "the box sent nothing" (2026-09-02).
        let s = av?.statsSnapshot()
        let v = r[.mainVideo], alt = r[.altVideo], a = r[.mediaAudio], voice = r[.voiceAudio]
        let gaps = v.gapsDelta + alt.gapsDelta
        let fails = v.decryptFailDelta + alt.decryptFailDelta + a.decryptFailDelta + voice.decryptFailDelta
        let upSec = Int(uptime)
        // New anomaly events since the last summary + the most recent one (so the log reads what the
        // user saw/heard without needing the JSON).
        let (newEvents, latest, total) = anomalyLog.delta(sinceTotal: lastEventTotal)
        lastEventTotal = total
        let evStr = latest.map { "\($0.kind.rawValue)/\($0.stream ?? "-")(\($0.detail))" } ?? "-"
        let mainState = stateLabel(.mainVideo)
        // Pipeline latency per lane (arrival→wrapped > arrival→renderer). Bound outside the log
        // interpolation: an os.Logger literal is an autoclosure, so a bare `mainDecoder` there would be
        // an implicit self capture.
        let mainLat = Self.latPair(mainDecoder), altLat = Self.latPair(altDecoder)
        // Same reason as `mainLat`: bind before the log literal so the autoclosure captures a local.
        let telStr = String(format: "%.0f", telPerSec), micStr = String(format: "%.0f", micTxPerSec)
        // The main lane's `v=` line is CarPlay by default; while an AA decoder is bound (label ==
        // "aa-ocbm"), tag it `aa:` so a combined-log reader can tell which session the numbers are for
        // without cross-referencing the AA-over-OCBM start/stop lines. Field name itself (`v=`) is
        // unchanged so existing tooling/greps keep working.
        let mainLabel = mainDecoder?.label ?? "video"
        // Tag only the AA decoder ("aa-ocbm"); the CarPlay decoder's label is not "video", which made
        // every CarPlay session read "aa:v=" (noticed 2026-09-04 during the CarPlay regression run).
        let vTag = mainLabel.hasPrefix("aa") ? "aa:" : ""   // "aa-ocbm" / "aa-live" / "aa-debug"; CarPlay is "main"
        log.info("""
        AVmon \(vTag, privacy: .public)v=\(String(format: "%.1f", v.mbps), privacy: .public)Mbps/\(String(format: "%.0f", v.framesPerSec), privacy: .public)fps(\(mainState, privacy: .public)) \
        alt=\(String(format: "%.1f", alt.mbps), privacy: .public)Mbps/\(String(format: "%.0f", alt.framesPerSec), privacy: .public)fps \
        a=\(String(format: "%.1f", a.mbps), privacy: .public)Mbps/\(String(format: "%.0f", a.framesPerSec), privacy: .public)pps \
        voice=\(String(format: "%.1f", voice.mbps), privacy: .public)Mbps/\(String(format: "%.0f", voice.framesPerSec), privacy: .public)pps \
        arx=\(s?.mediaAudioRxFrames ?? 0, privacy: .public)f/\((s?.mediaAudioRxBytes ?? 0) / 1024, privacy: .public)KB \
        varx=\(s?.voiceAudioRxFrames ?? 0, privacy: .public)f/\((s?.voiceAudioRxBytes ?? 0) / 1024, privacy: .public)KB \
        nokey=\(s?.audioNoKeyDrops ?? 0, privacy: .public) \
        tel=\(telStr, privacy: .public) mictx=\(micStr, privacy: .public) \
        gaps=\(gaps, privacy: .public) fails=\(fails, privacy: .public) jit=\(String(format: "%.1f", v.jitterMs), privacy: .public)ms \
        wraplat=\(mainLat, privacy: .public)/\(altLat, privacy: .public)ms \
        events+\(newEvents, privacy: .public) last=\(evStr, privacy: .public) uptime=\(upSec, privacy: .public)s
        """)
    }

    /// `wrap>handoff` EWMA pair for one lane, rendered for the AVmon line ("0.1>1.2", "-" before the
    /// first frame). Two lanes are printed as `wraplat=<main>/<alt>ms`. NOT a decode time — see the
    /// header note; `wrap` is the CMSampleBuffer build, `handoff` is when the renderer took the frame.
    private static func latPair(_ d: VideoDecoder?) -> String {
        guard let d, let wrap = d.wrapLatencyMs else { return "-" }
        let handoff = d.handoffLatencyMs ?? wrap
        return String(format: "%.1f>%.1f", wrap, handoff)
    }

    // MARK: - JSON snapshot

    private func writeJSON(active: Bool) {
        let r = report
        let dt = r?.dt ?? 0
        let up = uptime
        // AD (decoded/sec) + slot-drop/sec per video lane, diffed from the decoders' cross-thread counters.
        var mainAD = 0.0, mainDrop = 0.0, altAD = 0.0, altDrop = 0.0
        if dt > 0 {
            if let d = mainDecoder {
                let cd = d.decodedCount.load(ordering: .relaxed), cx = d.slotDrops.load(ordering: .relaxed)
                mainAD = Double(cd &- lastMainDecoded) / dt; mainDrop = Double(cx &- lastMainDrops) / dt
                lastMainDecoded = cd; lastMainDrops = cx
            }
            if let d = altDecoder {
                let cd = d.decodedCount.load(ordering: .relaxed), cx = d.slotDrops.load(ordering: .relaxed)
                altAD = Double(cd &- lastAltDecoded) / dt; altDrop = Double(cx &- lastAltDrops) / dt
                lastAltDecoded = cd; lastAltDrops = cx
            }
        }
        func stream(_ k: StreamKind) -> [String: Any] {
            let s = r?[k] ?? StreamRates()
            var d: [String: Any] = [
                "mbps": round2(s.mbps),
                "fps": round2(s.framesPerSec),
                // ACTIVE vs IDLE — a low fps in "idle" is iOS conserving (static screen / no audio), NOT
                // a problem. Read fps together with state.
                "state": stateLabel(k),
                "avgKB": round2(Double(s.avgFrameBytes) / 1024.0),
                "minKB": round2(Double(s.minFrameBytes) / 1024.0),
                "maxKB": round2(Double(s.maxFrameBytes) / 1024.0),
                "gaps": s.gapsDelta,
                "gapsPerSec": round2(s.lossPerSec),
                "decryptFails": s.decryptFailDelta,
                "jitterMs": round2(s.jitterMs),
            ]
            // Audio streams show the negotiated format ("AAC-LC 48000Hz 2ch"); video streams show the
            // decoder's codec ("HEVC" / "H.264"); null until known.
            d["codec"] = s.format.map { Self.codecLabel($0) } ?? (s.videoCodec.map { $0 as NSString } ?? NSNull())
            // Video only: decodedFps (AD, sample buffers built) + dropFps (depth-1 slot losses). Compare
            // against "fps" (AR, decrypted): fps≈decodedFps → no in-app loss; fps≫decodedFps → slot drop.
            if k == .mainVideo { d["decodedFps"] = round2(mainAD); d["dropFps"] = round2(mainDrop) }
            if k == .altVideo { d["decodedFps"] = round2(altAD); d["dropFps"] = round2(altDrop) }
            // Pipeline latency: arrival→wrapped and arrival→renderer EWMAs, ms. null until the lane has
            // wrapped/handed off its first frame. Neither is a decode time (see the header note).
            if k == .mainVideo || k == .altVideo {
                let dec = (k == .mainVideo ? mainDecoder : altDecoder)
                d["wrapLatencyMs"] = dec?.wrapLatencyMs.map { round2($0) as Any } ?? NSNull()
                d["handoffLatencyMs"] = dec?.handoffLatencyMs.map { round2($0) as Any } ?? NSNull()
            }
            return d
        }
        // Anomaly ring, oldest → newest (most-recent-LAST, per the schema contract).
        let recent: [[String: Any]] = anomalyLog.events().map { ev in
            [
                "t": round2(ev.tWall),
                "tMonoMs": round2(ev.tMonoMs),
                "kind": ev.kind.rawValue,
                "stream": ev.stream ?? NSNull(),
                "detail": ev.detail,
            ]
        }
        let root: [String: Any] = [
            "schema": 2,
            "ts": Date().timeIntervalSince1970,
            "sessionActive": active,
            "uptimeSec": round2(up),
            "dt": round2(dt),
            // Reserved for REAL VideoToolbox decode timing, which this render path cannot observe. V5
            // briefly published the wrap time here; that was misleading, so it is null again.
            "decodeLatencyMs": NSNull(),
            "streams": [
                "mainVideo": stream(.mainVideo),
                "altVideo": stream(.altVideo),
                "mediaAudio": stream(.mediaAudio),
                "voiceAudio": stream(.voiceAudio),
            ],
            "recent_events": recent,
            "eventCounts": anomalyLog.counts(),
            // Android Auto rates, or null when no AA session is live. AA never reaches the decrypt
            // layer above, so "streams" stays all-zero during a projection — read this instead.
            "aa": aa.map { r -> [String: Any] in
                [
                    "transport": r.transport,
                    "dt": round2(r.dt),
                    "videoRxPerSec": round2(r.videoRxPerSec),
                    "videoDecodedPerSec": round2(r.videoDecodedPerSec),
                    "videoDropPerSec": round2(r.videoDropPerSec),
                    "audioMediaPerSec": round2(r.audioMediaPerSec),
                    "audioGuidancePerSec": round2(r.audioGuidancePerSec),
                    "audioSystemPerSec": round2(r.audioSystemPerSec),
                    "audioTelephonyPerSec": round2(r.audioTelephonyPerSec),
                    "micPerSec": round2(r.micPerSec),
                    // Box-side telephony lane (OCBM, not AA channels): HFP/SCO PCM in, 20 ms mic out.
                    "telephonyRxPerSec": round2(telephonyRxPerSec),
                    "micUplinkPerSec": round2(micUplinkPerSec),
                    "rxMbps": round2(r.rxMbps),
                    "txMbps": round2(r.txMbps),
                    "backlog": r.backlog,
                ]
            } ?? NSNull(),
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: root,
                                                     options: [.sortedKeys, .prettyPrinted]) else { return }
        Self.ioQueue.async {
            // `.atomic` writes to an auxiliary file and renames into place — the required temp+rename.
            do {
                try data.write(to: Self.jsonURL, options: .atomic)
            } catch {
                // L6: was `try?` — a write failure (e.g. /tmp unwritable) left the previous run's
                // sessionActive:true file on disk with nothing to say so. Log once, not per tick.
                if !Self.loggedWriteFailure {
                    Self.loggedWriteFailure = true
                    Self.staticLog.error("failed to write \(Self.jsonURL.path, privacy: .public): \(error.localizedDescription, privacy: .public)")
                }
            }
        }
    }

    private func round2(_ v: Double) -> Double { (v * 100).rounded() / 100 }

    static func codecLabel(_ f: OCBMAudioStreamFormat) -> String {
        let name: String
        switch f.codec {
        case 0: name = "PCM"
        case 1: name = "AAC-LC"
        case 2: name = "AAC-ELD"
        case 3: name = "OPUS"
        default: name = "codec\(f.codec)"
        }
        return "\(name) \(Int(f.sampleRate))Hz \(f.channels)ch"
    }
}

// MARK: - Settings ▸ CCPA section

/// The live metrics block rendered inside `CCPATab`. Mirrors the CCPA tab's idiom (a `Section` of
/// `LabeledContent` rows bound to a shared `ObservableObject`); refreshes off the monitor's own 1 Hz
/// timer rather than a per-view timer, so it costs nothing when the window is closed.
struct StreamPerfSection: View {
    @ObservedObject var monitor = StreamMetricsMonitor.shared

    var body: some View {
        Section {
            // The AA block stands alone: an Android Auto session over the TCP test transport has no
            // OCBM decrypt layer at all (`active == false`), and one over CH_IP has an active-but-idle
            // one. Either way the CarPlay rows below stay at zero, which is correct, not broken.
            if let aa = monitor.aa { aaRows(aa) }
            if !monitor.active {
                if monitor.aa == nil {
                    Text("No active CarPlay session — stream metrics appear once A/V is flowing.")
                        .foregroundStyle(.secondary)
                }
            } else {
                LabeledContent("Session uptime", value: uptimeText)
                videoRow("Main video", .mainVideo)
                videoRow("Alt video", .altVideo)
                audioRow("Media audio", .mediaAudio)
                audioRow("Voice audio", .voiceAudio)
                LabeledContent("Decode latency", value: "n/a (deferred)")
                if !monitor.recentEvents.isEmpty {
                    Divider()
                    Text("Recent anomalies").font(.caption).foregroundStyle(.secondary)
                    ForEach(monitor.recentEvents, id: \.id) { ev in
                        HStack(spacing: 8) {
                            Text(ev.kind.rawValue)
                                .font(.caption).fontWeight(.medium)
                                .foregroundStyle(color(for: ev.kind))
                            Text(ev.stream ?? "—").font(.caption).foregroundStyle(.secondary)
                            Spacer()
                            Text(ev.detail).font(.caption2).foregroundStyle(.secondary)
                        }
                    }
                }
            }
        } header: {
            Text("Stream Performance")
        } footer: {
            Text("Live receive-side health measured on the Mac (~1 Hz). ‘idle’ is normal (iOS conserves bandwidth on a static screen / no audio). Also written to /tmp/carlink_metrics.json.")
                .font(.caption)
        }
    }

    /// Android Auto rows — same shape as the CarPlay rows (per-second rates, a caption of detail),
    /// shown only while an AA session is publishing. AA's own counters, not the decrypt layer's.
    @ViewBuilder private func aaRows(_ aa: AARates) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            LabeledContent("Android Auto video",
                value: String(format: "%.1f Mbps · %.0f fps · %@", aa.rxMbps, aa.videoDecodedPerSec,
                              aa.videoRxPerSec > 0 ? "active" : "idle"))
            Text(String(format: "%@ · rx %.0f/s · decoded %.0f/s · dropped %.1f/s · backlog %d",
                        aa.transport, aa.videoRxPerSec, aa.videoDecodedPerSec, aa.videoDropPerSec, aa.backlog))
                .font(.caption).foregroundStyle(.secondary)
        }
        VStack(alignment: .leading, spacing: 2) {
            LabeledContent("Android Auto audio",
                value: String(format: "%.0f pkt/s", aa.audioMediaPerSec + aa.audioGuidancePerSec
                              + aa.audioSystemPerSec + aa.audioTelephonyPerSec + monitor.telephonyRxPerSec))
            Text(String(format: "media %.0f/s · guidance %.0f/s · system %.0f/s · AA telephony sink %.0f/s · call audio (box HFP) %.0f/s",
                        aa.audioMediaPerSec, aa.audioGuidancePerSec, aa.audioSystemPerSec,
                        aa.audioTelephonyPerSec, monitor.telephonyRxPerSec))
                .font(.caption).foregroundStyle(.secondary)
        }
        VStack(alignment: .leading, spacing: 2) {
            LabeledContent("Android Auto mic uplink",
                value: String(format: "%.0f frame/s · %.2f Mbps up", aa.micPerSec, aa.txMbps))
            Text(String(format: "AA channel 9 %.0f/s · CH_MIC 20 ms frames %.0f/s (call uplink)",
                        aa.micPerSec, monitor.micUplinkPerSec))
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    private var uptimeText: String {
        let s = Int(monitor.uptime)
        return String(format: "%02d:%02d:%02d", s / 3600, (s % 3600) / 60, s % 60)
    }

    private func color(for kind: AnomalyKind) -> Color {
        switch kind {
        case .freeze, .decodeFail, .clip: return .red
        case .underrun, .silence, .gap:   return .orange
        default:                          return .yellow
        }
    }

    @ViewBuilder private func videoRow(_ title: String, _ kind: StreamKind) -> some View {
        let r = monitor.report?[kind] ?? StreamRates()
        VStack(alignment: .leading, spacing: 2) {
            LabeledContent(title,
                value: String(format: "%@%.1f Mbps · %.0f fps · %@",
                              r.videoCodec.map { "\($0) · " } ?? "", r.mbps, r.framesPerSec, monitor.stateLabel(kind)))
            Text(detailLine(r))
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    @ViewBuilder private func audioRow(_ title: String, _ kind: StreamKind) -> some View {
        let r = monitor.report?[kind] ?? StreamRates()
        VStack(alignment: .leading, spacing: 2) {
            LabeledContent(title,
                value: String(format: "%.2f Mbps · %.0f pkt/s · %@", r.mbps, r.framesPerSec, monitor.stateLabel(kind)))
            Text(r.format.map(StreamMetricsMonitor.codecLabel) ?? "no format yet")
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    /// frame size + loss/fail/jitter line shared by the video rows.
    private func detailLine(_ r: StreamRates) -> String {
        let sz = String(format: "frame %.0f/%.0f/%.0f KB (min/avg/max)",
                        Double(r.minFrameBytes) / 1024, r.avgFrameBytes / 1024, Double(r.maxFrameBytes) / 1024)
        return "\(sz) · loss \(r.gapsDelta) (\(String(format: "%.1f", r.lossPerSec))/s) · fails \(r.decryptFailDelta) · jitter \(String(format: "%.1f", r.jitterMs)) ms"
    }
}
