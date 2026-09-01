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
//     "decodeLatencyMs": null,     // reserved — VideoToolbox decode timing (deferred, see note)
//     "streams": {
//       "mainVideo":  { ...perStream... },  "altVideo": {...},
//       "mediaAudio": { ...perStream... },  "voiceAudio": {...}
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
//     "dropFps": 0.0               // VIDEO only (main/alt): depth-1 decode/enqueue-slot losses/sec; fps≫decodedFps → in-app slot drop
//   }
//   recent_events[].kind ∈ { freeze, fps_drop, decode_fail, gap    (video)
//                            underrun, silence, click, clip, audio_gap, seq_loss (audio) }
//   FREEZE/fps_drop are BASELINE-RELATIVE (judged against a stream's own recent rate, with an ACTIVE/
//   IDLE hysteresis) — a low fps while "idle" is normal demand-driven throttling, never an anomaly.
// ─────────────────────────────────────────────────────────────────────────────────────────────────────
//
// Decode latency is DEFERRED: `VideoDecoder` renders through the macOS-26 AVSampleBufferRenderSynchronizer
// receiver, which surfaces no VTDecompressionSession start/finish callback, so a true submit→decoded time
// is not cleanly observable without reworking that path (out of scope, and the brief says don't hack the
// decode path riskily). `decodeLatencyMs` stays null and the log prints `declat=n/a`. The received-frame
// `jitterMs` is the cheap, safe proxy for what visually reads as stutter.

import Combine
import Foundation
import SwiftUI
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

    /// The shared anomaly log — fed by OCBMAVDecrypt (video) + AudioPlayer (audio), polled here, read by
    /// the JSON writer / log / Settings. AppDelegate hands this same instance to both feeders at setup.
    let anomalyLog = AVAnomalyLog()

    private weak var av: OCBMAVDecrypt?
    private var timer: Timer?
    private var previous: StreamMetricsSnapshot?
    private var startedAt: Double = 0          // ProcessInfo.systemUptime seconds
    private var tick = 0
    private var lastEventTotal: UInt64 = 0     // anomaly total at the last log summary

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
    private static let jsonURL = URL(fileURLWithPath: "/tmp/carlink_metrics.json")
    /// JSON writes + the atomic rename run off-main so no file I/O touches the UI/timer beat.
    private static let ioQueue = DispatchQueue(label: "carplay-host.avmon.io", qos: .utility)

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
        report = nil
        previous = nil
        writeJSON(active: false)
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
        log.info("""
        AVmon v=\(String(format: "%.1f", v.mbps), privacy: .public)Mbps/\(String(format: "%.0f", v.framesPerSec), privacy: .public)fps(\(mainState, privacy: .public)) \
        alt=\(String(format: "%.1f", alt.mbps), privacy: .public)Mbps/\(String(format: "%.0f", alt.framesPerSec), privacy: .public)fps \
        a=\(String(format: "%.1f", a.mbps), privacy: .public)Mbps/\(String(format: "%.0f", a.framesPerSec), privacy: .public)pps \
        voice=\(String(format: "%.1f", voice.mbps), privacy: .public)Mbps/\(String(format: "%.0f", voice.framesPerSec), privacy: .public)pps \
        gaps=\(gaps, privacy: .public) fails=\(fails, privacy: .public) jit=\(String(format: "%.1f", v.jitterMs), privacy: .public)ms \
        declat=n/a events+\(newEvents, privacy: .public) last=\(evStr, privacy: .public) uptime=\(upSec, privacy: .public)s
        """)
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
            "decodeLatencyMs": NSNull(),
            "streams": [
                "mainVideo": stream(.mainVideo),
                "altVideo": stream(.altVideo),
                "mediaAudio": stream(.mediaAudio),
                "voiceAudio": stream(.voiceAudio),
            ],
            "recent_events": recent,
            "eventCounts": anomalyLog.counts(),
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: root,
                                                     options: [.sortedKeys, .prettyPrinted]) else { return }
        Self.ioQueue.async {
            // `.atomic` writes to an auxiliary file and renames into place — the required temp+rename.
            try? data.write(to: Self.jsonURL, options: .atomic)
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
            if !monitor.active {
                Text("No active CarPlay session — stream metrics appear once A/V is flowing.")
                    .foregroundStyle(.secondary)
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
                    ForEach(Array(monitor.recentEvents.enumerated()), id: \.offset) { _, ev in
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
