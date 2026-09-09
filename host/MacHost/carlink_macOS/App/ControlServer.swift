import AppKit
import Foundation

/// A localhost line-protocol server that drives the Controls window's INTENTS programmatically.
///
/// WHY THIS EXISTS. Diagnosing a control that "does not work" needs three facts: did the app send
/// anything, did it reach the phone, did the phone act. Only the third is visible to a human at the
/// bench, so the first two get inferred — and on 2026-08-27 that cost hours: Android Auto HOME/BACK
/// were "broken at the protocol level" through several rounds of field-number theorising, when in
/// fact the knob panel's buttons were being swallowed by our own router and no HOME had ever been
/// transmitted. The fix was found in one minute by TALLYING WHAT WE SENT against gearhead's own
/// `CAR.CAM: Received keycode` log. This server makes that loop cheap and repeatable: drive a
/// control, then read both logs, with no human in the actuation path.
///
/// Google's own Desktop Head Unit does the same thing (a stdin command reader — `dpad back`,
/// `keycode home`, `mic begin`), which is why its behaviour can be A/B-tested against ours at all.
///
/// **It deliberately goes through `ControlsBridge`, exactly like the buttons do.** A side path
/// straight to the transport would prove the transport works while missing every routing bug in the
/// layer under test — precisely the bug this exists to catch. If a control is broken in the UI it
/// must be broken here too.
///
/// OFF unless `CARLINK_CTRL_PORT` is set, and bound to 127.0.0.1 only: this is an unauthenticated
/// remote control for whatever phone is plugged in, and it has no business listening on a LAN.
///
///     CARLINK_CTRL_PORT=8765 open -a carlink_macOS
///     printf 'key home\n' | nc 127.0.0.1 8765
@MainActor
final class ControlServer {

    private var listenFD: Int32 = -1
    private let port: UInt16

    /// Returns nil unless `CARLINK_CTRL_PORT` names a usable port.
    init?() {
        guard let raw = ProcessInfo.processInfo.environment["CARLINK_CTRL_PORT"],
              let p = UInt16(raw), p > 0 else { return nil }
        port = p
    }

    func start() {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { NSLog("[ctrl] socket() failed"); return }
        var yes: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, socklen_t(MemoryLayout<Int32>.size))

        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = port.bigEndian
        addr.sin_addr.s_addr = UInt32(0x7F00_0001).bigEndian   // 127.0.0.1 ONLY
        let bound = withUnsafePointer(to: &addr) { p in
            p.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                bind(fd, sa, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0, listen(fd, 4) == 0 else {
            NSLog("[ctrl] bind/listen on \(port) failed (errno \(errno))"); close(fd); return
        }
        listenFD = fd
        NSLog("[ctrl] control server on 127.0.0.1:\(port) — 'help' for commands")

        DispatchQueue.global(qos: .utility).async { [weak self] in
            while true {
                let c = accept(fd, nil, nil)
                if c < 0 { return }
                // A client that drops mid-write (nc killed, curl Ctrl-C) makes the next write(2) raise
                // SIGPIPE, whose default disposition terminates the whole app — not just this bench
                // diagnostics connection. SO_NOSIGPIPE makes that write fail with EPIPE instead.
                var one: Int32 = 1
                setsockopt(c, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
                self?.serve(c)
            }
        }
    }

    /// One client, one command per line, until it closes. Serial by design — these are human-paced
    /// diagnostics, not a throughput path.
    private nonisolated func serve(_ fd: Int32) {
        var buf = [UInt8](repeating: 0, count: 1024)
        var pending = ""
        while true {
            let n = read(fd, &buf, buf.count)
            if n <= 0 { close(fd); return }
            pending += String(decoding: buf[0..<n], as: UTF8.self)
            while let nl = pending.firstIndex(of: "\n") {
                let line = String(pending[pending.startIndex..<nl]).trimmingCharacters(in: .whitespacesAndNewlines)
                pending = String(pending[pending.index(after: nl)...])
                if line.isEmpty { continue }
                // `shot` is the one verb that does real work (a PNG encode of up to a 4K frame) and
                // touches no main-actor state — FrameTap is its own lock — so it runs HERE, on the
                // socket thread, rather than freezing the UI for the encode. Everything else is a
                // main-actor intent and stays on the main queue.
                let reply: String
                let lower = line.lowercased()
                if lower == "shot" || lower.hasPrefix("shot ") {
                    reply = Self.shot(line)
                } else {
                    reply = DispatchQueue.main.sync { MainActor.assumeIsolated { Self.dispatch(line) } }
                }
                _ = (reply + "\n").withCString { write(fd, $0, strlen($0)) }
            }
        }
    }

    // MARK: - Snapshot helpers for the read surfaces
    //
    // Each returns a plain `[String: Any]` that `jsonLine` serialises. They read the SAME observable
    // singletons the UI renders from — never a private path to the transport — for exactly the
    // reason the class docstring gives about actuation: a reader that bypasses the layer under test
    // reports health the user cannot see.

    /// Serialise one snapshot to a single line. `sortedKeys` so repeated polls diff cleanly.
    /// nonisolated: pure, and `shot` calls it from the socket thread.
    private nonisolated static func jsonLine(_ obj: [String: Any]) -> String {
        guard let d = try? JSONSerialization.data(withJSONObject: obj, options: [.sortedKeys]),
              let s = String(data: d, encoding: .utf8) else { return "ERR could not serialise" }
        return s
    }

    private static func sessionSnapshot() -> [String: Any] {
        let b = ControlsBridge.shared
        let m = VehicleConfigModel.shared
        var out: [String: Any] = [
            "sessionActive": b.sessionActive,
            "subscribed": b.subscribed,
            "projection": b.isAndroidAuto ? "androidAuto" : "carPlay",
            "limitedUIOn": b.limitedUIOn,
            "lastSent": b.lastSent,
            "settingsDirty": m.dirty,
        ]
        // Session-failure state (2026-09-05): a bench loop polls this instead of grepping the box
        // log for `full TEARDOWN reason=`. `phase` is idle/connecting/recorded/streaming;
        // `failure.consecutive` is the number of teardowns since the last attempt that streamed
        // (0 on a healthy box); `failure.last` is the most recent teardown of any kind, with the
        // box's own `reason=` token (NSNull when the box printed none — never a guess), the
        // precursor line it printed before it, and the same one-line summary the overlay shows.
        // `failure.repeating` is true when every failure in the current run has the same signature —
        // the retry-loop tell.
        let fs = SessionFailureTracker.shared.snapshot
        out["phase"] = fs.phase.rawValue
        var failure: [String: Any] = [
            "consecutive": fs.consecutiveFailures,
            "repeating": fs.repeating,
            "boxLogStream": fs.boxLogStreamEnabled,
        ]
        if let f = fs.last {
            failure["last"] = [
                "at": f.date.formatted(Date.ISO8601FormatStyle(includingFractionalSeconds: true)),
                "kind": f.kind.rawValue,
                "reason": f.boxReason.map { $0 as Any } ?? NSNull(),
                "precursor": f.precursor.map { $0 as Any } ?? NSNull(),
                "lastCommandSent": f.lastCommandSent.map { $0 as Any } ?? NSNull(),
                "reachedRecord": f.reachedRecord,
                "streamed": f.streamed,
                "iphoneClosedEventChannel": f.iphoneClosedEventChannel,
                "summary": f.summary,
            ]
        } else {
            failure["last"] = NSNull()
        }
        out["failure"] = failure
        return out
    }

    private static func boxSnapshot() -> [String: Any] {
        let s = CCPABridge.shared
        var out: [String: Any] = ["status": s.statusText, "stale": s.stale, "busy": s.busy]
        if let i = s.info {
            // Identity (BT/Wi-Fi MAC, serial) is deliberately INCLUDED: a caller diagnosing a
            // pairing fault needs it, and the app already shows it on the Adapter tab. It must not
            // be copied into anything durable — see the credentials rule in the repo's CLAUDE.md.
            out["identity"] = ["name": i.name, "btMac": i.bt_mac, "wifiMac": i.wifi_mac, "serial": i.serial]
            out["health"] = ["uptimeSec": i.uptime_s, "rootfsPct": i.rootfs_pct,
                             "hciUp": i.hci_up, "ssp": i.ssp, "wlanAp": i.wlan_ap,
                             "transport": i.transport, "phonePresent": i.phone_present]
            out["daemons"] = ["ocbmd": i.daemons.ocbmd, "iap2d": i.daemons.iap2d,
                              "airplayd": i.daemons.airplayd, "carplayWireless": i.daemons.carplay_wireless]
            out["pairedDeviceCount"] = i.devices.count
        }
        if let h = s.boxHealth {
            out["boxHealth"] = Dictionary(uniqueKeysWithValues: h.checklist.map { ($0.label, $0.ok) })
        }
        if let p = s.btPhase { out["btPhase"] = p.displayName }
        return out
    }

    /// The CarPlay view-area rect as iOS reports it in the screen header (Stage 0 — observation only).
    ///
    /// `changes` is the number the experiment turns on: with a single full-panel view area it must be
    /// 1 for an entire session. A 2 means something moved the rect; a -1 means the header parse is
    /// wrong (non-finite floats), which is a bug in us, not a message from the phone.
    private static func viewAreaSnapshot() -> [String: Any] {
        var out: [String: Any]
        if let (g, changes) = ScreenGeometry.latest {
            out = [
                "observed": true,
                "changes": changes,
                "coded": ["width": Int(g.codedWidth), "height": Int(g.codedHeight)],
                "viewArea": ["originX": Int(g.originX), "originY": Int(g.originY),
                             "width": Int(g.width), "height": Int(g.height)],
                "isFullFrame": g.isFullFrame,
                "summary": g.summary,
            ]
        } else {
            out = ["observed": false,
                   "note": "no VideoConfig record yet this session — connect a phone and start projection"]
        }
        // Config-in-force verdict (2026-09-05): is the box coding what we pushed? `matchesProfile` is
        // true/false once a VideoConfig has been seen, NSNull while pending. `pushed` is the main
        // panel + codec + SETUP mode the last landed SUBSCRIBE carried, plus what has been observed
        // against it. A false here with `observed: true` is the 2026-09-05 failure — the box on its
        // compiled 1920x720/H.264 while the app believed 2400x960/HEVC — which nothing else surfaced.
        let ci = ConfigIntegrity.shared.snapshot
        out["matchesProfile"] = ci.verdict.matches.map { $0 as Any } ?? NSNull()
        var pushed: [String: Any] = [:]
        if let p = ci.pushed {
            pushed["width"] = p.mainWidth
            pushed["height"] = p.mainHeight
            pushed["codec"] = p.enablesHEVC ? "HEVC" : "H.264"
            pushed["appDrivenSetup"] = p.appDrivenSetup
        } else {
            pushed["note"] = "no SUBSCRIBE has landed yet"
        }
        pushed["observedCodec"] = ci.codec.map { $0 as Any } ?? NSNull()
        pushed["rsOpenSeen"] = ci.setupOpenSeen
        if let crc = ci.setupOpenCRC { pushed["rsOpenCfgCRC"] = String(format: "0x%08X", crc) }
        pushed["verdict"] = ci.verdict.detail.map { $0 as Any } ?? "pending"
        out["pushed"] = pushed
        // The ARMED second area (2026-09-07): what the model holds and whether it is legal — the third
        // leg beside `observed` (what iOS reports) and `pushed` (what the last SUBSCRIBE carried).
        // `armed.active` true with `pushed` not yet carrying it means "save and reconnect".
        out["armed"] = armedViewArea()
        return out
    }

    private static func avSnapshot() -> [String: Any] {
        let m = StreamMetricsMonitor.shared
        var out: [String: Any] = ["active": m.active, "uptimeSec": Int(m.uptime)]
        if !m.eventCounts.isEmpty { out["anomalies"] = m.eventCounts }
        // Per-stream rates, spelled out. This was `String(describing: report)` in the first cut,
        // which emitted Swift's synthesised struct dump (`carlink_macOS.StreamKind.altVideo: ...`) —
        // the one surface here a caller could not parse, on a door whose whole reason to exist is
        // being parseable. `report` is the DECRYPT-LAYER truth (what actually arrived and decoded),
        // worth more than any UI-side frame counter, so it is the number to get right.
        if let r = m.report {
            out["intervalSec"] = r.dt
            var streams: [String: Any] = [:]
            for kind in StreamKind.allCases {
                let s = r[kind]
                var row: [String: Any] = [
                    "mbps": (s.mbps * 1000).rounded() / 1000,
                    "framesPerSec": (s.framesPerSec * 10).rounded() / 10,
                    "avgFrameBytes": Int(s.avgFrameBytes.rounded()),
                    "minFrameBytes": s.minFrameBytes,
                    "maxFrameBytes": s.maxFrameBytes,
                    "lossPerSec": (s.lossPerSec * 100).rounded() / 100,
                    "gapsDelta": s.gapsDelta,
                    // The one field a caller should alert on: anything non-zero here is corruption,
                    // not slowness.
                    "decryptFailDelta": s.decryptFailDelta,
                    "jitterMs": (s.jitterMs * 10).rounded() / 10,
                ]
                if let f = s.format { row["format"] = f }
                if let c = s.videoCodec { row["videoCodec"] = c }
                streams[kind.label] = row
            }
            out["streams"] = streams
        }
        // Same treatment for the AA side — it is a flat rate struct, so spell it out rather than
        // leaving a second unparseable field behind.
        if let aa = m.aa {
            func r2(_ v: Double) -> Double { (v * 100).rounded() / 100 }
            out["androidAuto"] = [
                "transport": aa.transport,
                "backlog": aa.backlog,
                "rxMbps": r2(aa.rxMbps), "txMbps": r2(aa.txMbps),
                "videoRxPerSec": r2(aa.videoRxPerSec),
                "videoDecodedPerSec": r2(aa.videoDecodedPerSec),
                // Non-zero means frames arrived and did not reach the screen.
                "videoDropPerSec": r2(aa.videoDropPerSec),
                "audioMediaPerSec": r2(aa.audioMediaPerSec),
                "audioGuidancePerSec": r2(aa.audioGuidancePerSec),
                "audioSystemPerSec": r2(aa.audioSystemPerSec),
                "audioTelephonyPerSec": r2(aa.audioTelephonyPerSec),
                "micPerSec": r2(aa.micPerSec),
            ]
        }
        return out
    }

    /// The NEUTRAL profile document — the same bytes `Export profile…` writes.
    private static func profileJSON() -> String {
        do { return String(decoding: try VehicleProfileDocument.encode(VehicleConfigModel.shared.document),
                           as: UTF8.self).replacingOccurrences(of: "\n", with: "") }
        catch { return "ERR could not encode profile: \(error)" }
    }

    /// What Android Auto WOULD declare for the current profile, including everything the renderer
    /// had to approximate. Buildable with no phone attached, which is the point: a caller can check
    /// a geometry change without a session.
    private static func aaSnapshot() -> [String: Any] {
        let m = VehicleConfigModel.shared
        let dark = NSApp?.effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        let cap = AACapability(profile: m.profile, adapter: m.adapterSettings,
                               autoThemeIsDark: dark, warn: { _ in })
        return [
            "tier": "\(cap.resolution.size.w)x\(cap.resolution.size.h)",
            "tierEnum": cap.resolution.rawValue,
            "fps": cap.frameRate == .fps60 ? 60 : 30,
            "codec": cap.videoCodecHEVC ? "H.265" : "H.264",
            "density": cap.density,
            "visible": cap.visibleWidth > 0 ? "\(cap.visibleWidth)x\(cap.visibleHeight)" : "whole tier",
            "margins": "\(cap.margins.w)x\(cap.margins.h)",
            "driverPositionWire": cap.driverPosition,
            "drivingMask": cap.drivingMask.rawValue,
            "touchscreen": cap.declaresTouchscreen,
            // The honesty channel — empty means the profile went out as authored.
            "negotiationNotes": cap.negotiationNotes,
        ]
    }

    /// `shot [path]` — dump the CURRENT decoded main-lane picture to a PNG and describe it.
    ///
    /// The pixels are the verdict (2026-09-07): over WIRED CarPlay there is no phone log on this Mac,
    /// and iOS renders its view-area lockout banner INTO the stream, so a frame dump is the only
    /// detector for a LOCKOUT this side owns. The frame comes from `FrameTap` — a shadow decode of the
    /// exact sample buffer the renderer accepted, not a screen capture (no permission, no window
    /// scaling, no chrome). `frameAgeMs` is how old the newest decoded picture is: a number that keeps
    /// growing between shots means the stream has stopped, and a shot of a stale frame is a shot of
    /// the last thing iOS sent, not of a black screen. `sequence` lets two shots prove they saw
    /// different frames. `ok:false` with `reason` is the honest answer before the first IDR.
    ///
    /// nonisolated: runs on the socket thread (see `serve`); touches only FrameTap and the filesystem.
    private nonisolated static func shot(_ line: String) -> String {
        let t = line.split(separator: " ", maxSplits: 1).map(String.init)
        let path = t.count > 1 && !t[1].isEmpty
            ? t[1]
            : "/tmp/vashots/shot-\(Int(Date().timeIntervalSince1970)).png"
        guard let snap = FrameTap.shared.snapshot() else {
            return jsonLine(["ok": false, "path": path,
                             "reason": "no decoded frame yet — no session, no IDR since launch, or the "
                                     + "tap is off (CARLINK_CTRL_PORT unset at launch)",
                             "decodeFailures": Int(FrameTap.shared.decodeFailures.load(ordering: .relaxed))])
        }
        do {
            try FrameTap.writePNG(snap.pixelBuffer, to: URL(fileURLWithPath: path))
        } catch {
            return jsonLine(["ok": false, "path": path, "reason": "PNG write failed: \(error)"])
        }
        return jsonLine(["ok": true, "path": path,
                         "width": snap.width, "height": snap.height,
                         "frameAgeMs": snap.ageMs, "sequence": Int(snap.sequence),
                         "decodeFailures": Int(FrameTap.shared.decodeFailures.load(ordering: .relaxed))])
    }

    /// The ARMED second view area as the model holds it — the same fields the Settings form edits —
    /// with the verdict the form would show. Shared by `get viewarea` (read) and `viewarea arm` (the
    /// reply after a write), so a caller sees one shape in both places.
    private static func armedViewArea() -> [String: Any] {
        let m = VehicleConfigModel.shared
        let verdict = m.viewArea2Verdict
        let floor = m.viewArea2MinimumSize
        return [
            "enabled": m.viewArea2Enabled,
            "rect": ["x": m.viewArea2X, "y": m.viewArea2Y, "width": m.viewArea2W, "height": m.viewArea2H],
            "spec": ViewAreaSpec(x: m.viewArea2X, y: m.viewArea2Y, w: m.viewArea2W, h: m.viewArea2H).description,
            "panel": ["width": m.mainWidth, "height": m.mainHeight],
            // Non-empty when the last Save stored a panel (or fps/inset/area field) differently
            // from what was set — a sweep that armed a portrait panel and got a landscape one back
            // reads it here, beside the panel it is about to verdict against (2026-09-07).
            "clampNotes": m.clampNotes,
            // nil verdict = legal for the current panel. A string is the SAME message the form shows,
            // in ViewArea2Rule's severity order (containment, parity, positivity are teardowns; the
            // product floor is a lockout).
            "verdict": verdict.map { $0 as Any } ?? NSNull(),
            "legal": verdict == nil,
            "floor": ["width": floor.width, "height": floor.height],
            // What will actually be EMITTED as viewAreas[1] at the next push: enabled AND legal.
            "active": m.viewArea2Active,
        ]
    }

    /// `viewarea arm <WxH@X,Y>` / `viewarea arm off` / `viewarea request <index>`.
    ///
    /// `arm` writes the SAME `VehicleConfigModel` fields the Settings form writes (viewArea2Enabled/X/Y/
    /// W/H) and answers with the form's own verdict — it does not push. `save` commits, and the rect
    /// lands at the next SUBSCRIBE, exactly as a human's edit would. This replaces `defaults write`
    /// against a stopped app and the app-less `/tmp/carplay_viewarea2` bench file as the way to arm a
    /// rect. An illegal rect IS written (so `get viewarea` shows what was asked for, as the form does)
    /// but `active` stays false and the emitter leaves the YAML byte-identical — the same gate the
    /// form has. Parity, containment and the product floor are the app's rules, and this is the app.
    ///
    /// `request` commands the transition through ControlsBridge (the intent table, the AA refusal, the
    /// `lastSent` readout) → OCBMClient `[inputCommand][cmdViewArea][index]` → airplayd, which answers
    /// `updateViewArea` itself and refuses an index `/info` never declared. `ok` here means the command
    /// was ACCEPTED FOR SEND — it is not proof that iOS moved: read `get viewarea` (`changes` and the
    /// observed rect) and `shot` for that.
    private static func viewAreaCommand(_ t: [String]) -> String {
        let usage = "ERR viewarea arm <WxH@X,Y> | viewarea arm off | viewarea off | viewarea request <index 0-255>"
        let m = VehicleConfigModel.shared
        switch (t.dropFirst().first ?? "").lowercased() {
        // `viewarea off` is the spelling `tools/va_wired_sweep.sh` disarms with; `arm off` reads
        // better in a transcript. Same write either way: the enable flag, rect left as typed.
        case "off":
            m.viewArea2Enabled = false
            return jsonLine(["ok": true, "armed": armedViewArea(), "dirty": m.dirty,
                             "note": "not pushed until 'save'"])
        case "arm":
            guard let arg = t.dropFirst(2).first, !arg.isEmpty else { return usage }
            if arg.lowercased() == "off" {
                m.viewArea2Enabled = false
                return jsonLine(["ok": true, "armed": armedViewArea(), "dirty": m.dirty,
                                 "note": "not pushed until 'save'"])
            }
            guard let spec = ViewAreaSpec.parse(arg) else {
                return "ERR viewarea arm: '\(arg)' is not WxH@X,Y (four non-negative integers; "
                     + "':initial' is not authored by the app model)"
            }
            m.viewArea2Enabled = true
            m.viewArea2X = spec.x; m.viewArea2Y = spec.y
            m.viewArea2W = spec.w; m.viewArea2H = spec.h
            return jsonLine(["ok": true, "armed": armedViewArea(), "dirty": m.dirty,
                             "note": "not pushed until 'save'"])
        case "request":
            guard let raw = t.dropFirst(2).first, let idx = UInt8(raw) else { return usage }
            let b = ControlsBridge.shared
            guard !b.isAndroidAuto else {
                return jsonLine(["ok": false, "index": Int(idx),
                                 "reason": b.unavailableReason(.viewArea) ?? "unavailable"])
            }
            guard b.client != nil else {
                return jsonLine(["ok": false, "index": Int(idx), "reason": "no session (no OCBM client)"])
            }
            b.requestViewArea(idx)
            return jsonLine(["ok": true, "index": Int(idx),
                             "wire": "/command updateViewArea {viewAreaIndex: \(idx)}",
                             "note": "accepted for send; the box refuses an undeclared index (box log "
                                   + "'[events] host viewArea index=N REFUSED'). Verify the transition "
                                   + "with 'get viewarea' (changes/observed rect) and 'shot'."])
        default:
            return usage
        }
    }

    /// The `set` allowlist. Every entry writes a field the UI also writes, so the two cannot diverge.
    private static func applySet(key: String, value: String) -> String {
        let m = VehicleConfigModel.shared
        func int(_ s: String) -> Int? { Int(s) }
        func bool(_ s: String) -> Bool { !["off", "false", "0", "no"].contains(s.lowercased()) }
        switch key.lowercased() {
        case "width":          guard let v = int(value) else { return "ERR width <int>" }; m.mainWidth = v
        case "height":         guard let v = int(value) else { return "ERR height <int>" }; m.mainHeight = v
        case "fps":            guard let v = int(value) else { return "ERR fps <30|60>" }; m.maxFPS = v
        case "dpi":            guard let v = int(value) else { return "ERR dpi <int>" }; m.dpi = v
        case "hevc":           m.enablesHEVC = bool(value)
        case "androidauto":    m.androidAutoEnabled = bool(value)
        case "wireless":       m.wirelessEnabled = bool(value)
        case "wifiap":         m.wifiAccessPoint = bool(value)
        case "theme":
            guard let v = AppearanceTheme(rawValue: value.lowercased()) else { return "ERR theme <auto|light|dark>" }
            m.theme = v.rawValue; m.nightMode = (v == .dark)
        case "driverposition":
            guard let v = DriverPosition(rawValue: value.lowercased()) else { return "ERR driverposition <left|right|center>" }
            m.driverPosition = v.rawValue; m.rightHandDrive = (v == .right)
        default:
            return "ERR unknown key '\(key)' — allowlist: width height fps dpi hevc androidauto "
                 + "wireless wifiap theme driverposition"
        }
        // The value IS stored as given (a caller composing a portrait panel needs `set width 480`
        // to survive until its `set height 800` arrives — clamping per write would be order-
        // dependent), but Save clamps, so say NOW what Save will store. `pendingClamp` is non-empty
        // exactly when a read-back after `save` would differ from what was just written; a caller
        // that ignores it has been told. `stored` is the model's read-back of this key.
        let pending = m.clampInPlace(dryRun: true)
        var out: [String: Any] = ["ok": true, "set": key, "value": value, "dirty": m.dirty,
                                  "pendingClamp": pending,
                                  "note": pending.isEmpty ? "not pushed until 'save'"
                                                          : "not pushed until 'save'; save WILL CLAMP — see pendingClamp"]
        switch key.lowercased() {
        case "width":  out["stored"] = m.mainWidth
        case "height": out["stored"] = m.mainHeight
        case "fps":    out["stored"] = m.maxFPS
        case "dpi":    out["stored"] = m.dpi
        default: break
        }
        return jsonLine(out)
    }

    /// Map a command line onto a ControlsBridge intent. Names mirror the Controls window's labels so
    /// a bug report and a repro command use the same vocabulary.
    private static func dispatch(_ line: String) -> String {
        let t = line.split(separator: " ").map(String.init)
        let b = ControlsBridge.shared
        NSLog("[ctrl] <- \(line)")

        switch (t.first ?? "").lowercased() {
        case "help":
            return "commands: key <home|back|select|up|down|left|right|play|pause|playpause|next|prev|"
                 + "answer|end|assistant> | knob <select|home|back|cw|ccw|up|down|left|right> | "
                 + "tap <x 0-10000> <y 0-10000> | night <on|off> | limitedui <on|off> | siri | "
                 + "dark <on|off> | night <on|off> | limitedui <on|off> | siri | status\n"
                 + "READ (one-line JSON): get session | get box | get av | get profile | get aa | get viewarea | get presets\n"
                 + "WRITE: preset <id> | set <key> <value> | save | help\n"
                 + "VIEW AREA (one-line JSON): viewarea arm <WxH@X,Y> | viewarea off | viewarea request <index>\n"
                 + "FRAME (one-line JSON): shot [path] — PNG of the current decoded main-lane frame "
                 + "(default /tmp/vashots/shot-<epoch>.png)"

        case "status":
            return "androidAuto=\(b.isAndroidAuto) aaSession=\(b.aaSession != nil) "
                 + "sessionActive=\(b.sessionActive) siriAvailable=\(b.siriAvailable)"

        // The D-Pad / nav panel — the same calls its buttons make.
        case "key":
            guard let a = t.dropFirst().first?.lowercased() else { return "ERR key <name>" }
            switch a {
            case "home":      b.nav(OCBM.navHome, "ctrl key home")
            case "back":      b.nav(OCBM.navBack, "ctrl key back")
            case "select":    b.nav(OCBM.navSelect, "ctrl key select")
            case "up":        b.nav(OCBM.navUp, "ctrl key up")
            case "down":      b.nav(OCBM.navDown, "ctrl key down")
            case "left":      b.nav(OCBM.navLeft, "ctrl key left")
            case "right":     b.nav(OCBM.navRight, "ctrl key right")
            case "play":      b.media(OCBM.mbtnPlay, "ctrl key play")
            case "pause":     b.media(OCBM.mbtnPause, "ctrl key pause")
            case "playpause": b.media(OCBM.mbtnPlayPause, "ctrl key playpause")
            case "next":      b.media(OCBM.mbtnNext, "ctrl key next")
            case "prev":      b.media(OCBM.mbtnPrev, "ctrl key prev")
            case "answer":    b.telephony(OCBM.telAnswer, "ctrl key answer")
            case "end":       b.telephony(OCBM.telEnd, "ctrl key end")
            case "assistant": b.assistant()
            default:          return "ERR unknown key '\(a)'"
            }
            return "OK key \(a)"

        // The KNOB panel — a separate set of call sites from the D-Pad panel's, which is exactly the
        // distinction that hid the 2026-08-27 HOME/BACK bug. Both are reachable here on purpose.
        case "knob":
            guard let a = t.dropFirst().first?.lowercased() else { return "ERR knob <name>" }
            switch a {
            case "select": b.knob(flags: 0x01, "ctrl knob select")
            case "home":   b.knob(flags: 0x02, "ctrl knob home")
            case "back":   b.knob(flags: 0x04, "ctrl knob back")
            case "cw":     b.knob(rotation: 1, "ctrl knob cw")
            case "ccw":    b.knob(rotation: -1, "ctrl knob ccw")
            case "up":     b.knob(nudgeY: -127, "ctrl knob up")
            case "down":   b.knob(nudgeY: 127, "ctrl knob down")
            case "left":   b.knob(nudgeX: -127, "ctrl knob left")
            case "right":  b.knob(nudgeX: 127, "ctrl knob right")
            default:       return "ERR unknown knob '\(a)'"
            }
            return "OK knob \(a)"

        // Coordinates are the view's normalized 0..10000 space, the same values a real tap produces,
        // so scaling and clamping are exercised rather than bypassed. A tap is down-then-up.
        case "tap":
            let xs = t.dropFirst().first, ys = t.dropFirst(2).first
            guard let xs, let ys, let x = UInt32(xs), let y = UInt32(ys),
                  x <= 10000, y <= 10000 else { return "ERR tap <x 0-10000> <y 0-10000>" }
            guard let inject = b.injectTouch else { return "ERR no view attached" }
            inject(.down, x, y)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.08) { inject(.up, x, y) }
            return "OK tap \(x) \(y)"

        // The video window's control-box sun/moon path (setDisplayDark), which is a DIFFERENT call
        // site from `night` (setNightMode) — and under AA it used to send CarPlay appearance commands
        // and do nothing. Reachable separately so the two cannot be confused again.
        case "dark":
            let on = (t.dropFirst().first?.lowercased() ?? "on") != "off"
            b.setDisplayDark(alt: false, dark: on)
            return "OK dark \(on ? "on" : "off")"

        case "night":
            let on = (t.dropFirst().first?.lowercased() ?? "on") != "off"
            b.setNightMode(on)
            return "OK night \(on ? "on" : "off")"

        case "limitedui":
            let on = (t.dropFirst().first?.lowercased() ?? "on") != "off"
            b.setLimitedUI(on)
            return "OK limitedui \(on ? "on" : "off")"

        case "siri":
            b.siriPress()
            return "OK siri"

        // ---- READ SURFACES (one-line JSON) --------------------------------------------------
        //
        // WHY JSON, when every verb above answers in prose: these exist to be READ BY A PROGRAM —
        // an agent driving the bench over this socket instead of through macOS UI automation. Prose
        // replies are for a human reading a repro transcript; a caller that has to regex "OK dark on"
        // is one wording change away from silently misparsing. One command still yields exactly one
        // line, so the framing above is unchanged and `nc | jq` works.
        //
        // These are READ-ONLY and side-effect-free. Anything that changes state stays a named verb.
        case "get":
            switch (t.dropFirst().first ?? "").lowercased() {
            case "session":  return jsonLine(sessionSnapshot())
            case "box":      return jsonLine(boxSnapshot())
            case "av":       return jsonLine(avSnapshot())
            case "profile":  return profileJSON()
            case "aa":       return jsonLine(aaSnapshot())
            case "viewarea": return jsonLine(viewAreaSnapshot())
            case "presets":  return jsonLine(["presets": VehicleProfilePreset.builtIn.map {
                                 ["id": $0.id, "title": $0.title] }])
            default: return "ERR get <session|box|av|profile|aa|viewarea|presets>"
            }

        // ---- WRITE SURFACES -------------------------------------------------------------------
        //
        // Deliberately NOT a generic key/value writer over the whole model: `set` takes an explicit
        // allowlist. A door that can write any field is a door that can push a config the emitter
        // has never seen, and the pushed YAML is the one artefact in this app that must stay
        // byte-predictable. Everything here mutates the SAME `VehicleConfigModel` the UI edits, so
        // the dirty/Save semantics a human sees are the ones a caller gets.
        case "preset":
            guard let id = t.dropFirst().first else { return "ERR preset <id> — 'get presets' lists them" }
            guard let preset = VehicleProfilePreset.named(id) else { return "ERR no preset '\(id)'" }
            VehicleConfigModel.shared.apply(preset: preset)
            return jsonLine(["ok": true, "applied": id, "dirty": true,
                             "note": "not pushed until 'save'"])

        case "set":
            let key = t.dropFirst().first ?? ""
            let val = t.dropFirst(2).first ?? ""
            guard !key.isEmpty, !val.isEmpty else { return "ERR set <key> <value>" }
            return applySet(key: key, value: val)

        case "save":
            let m = VehicleConfigModel.shared
            let wasDirty = m.dirty
            let ok = m.save()
            // `clamped` names every value Save stored differently from what was set — the same
            // lines the Vehicle tab shows under the field. Empty = the model holds what was asked.
            return jsonLine(["ok": ok, "wasDirty": wasDirty, "clamped": m.clampNotes,
                             "panel": ["width": m.mainWidth, "height": m.mainHeight],
                             "note": ok ? (m.clampNotes.isEmpty ? "committed; pushed at the next SUBSCRIBE"
                                                                : "committed WITH CLAMPS (see clamped); pushed at the next SUBSCRIBE")
                                        : "save refused (see the app log)"])

        // ---- VIEW AREA (2026-09-07) — arm a rect through the model, or command a transition ------
        case "viewarea":
            return viewAreaCommand(t)

        // `shot` is intercepted in `serve` before the main-queue hop; this arm only exists so a
        // caller that reaches dispatch with it (there is none) gets a real answer, not "unknown".
        case "shot":
            return shot(line)

        default:
            return "ERR unknown command — try 'help'"
        }
    }
}
