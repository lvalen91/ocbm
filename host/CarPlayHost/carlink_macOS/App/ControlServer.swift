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
                let line = String(pending[pending.startIndex..<nl]).trimmingCharacters(in: .whitespaces)
                pending = String(pending[pending.index(after: nl)...])
                if line.isEmpty { continue }
                let reply = DispatchQueue.main.sync { MainActor.assumeIsolated { Self.dispatch(line) } }
                _ = (reply + "\n").withCString { write(fd, $0, strlen($0)) }
            }
        }
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
                 + "dark <on|off> | status | help"

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

        default:
            return "ERR unknown command — try 'help'"
        }
    }
}
