// AirPlaySetupSession.swift — HOST-side, transport-free AirPlay/RTSP SETUP state machine (app-driven
// SETUP, plan P3). Line-for-line Swift port of host/ocbm-host/src/setup_driver.rs.
//
// The box runs its unmodified `AvSession` FIRST (binds sockets, spawns, side effects — its locally
// authored response IS the port report) and relays the decrypted request PLUS that local response to us
// over CH_RTSP. Instead of echoing the box's response verbatim (P1's Stage-0 transparency), the host
// AUTHORS the response the phone sees: it takes every box-owned port value FROM the relayed local
// response and authors only genuine host policy — `enabledFeatures` (phase 1) and the per-stream
// response shape (phase 2). Every authored response is diffed against the relayed local (the LIVE
// ORACLE), so a divergence is visible immediately and is either a bug or a documented intentional
// difference (e.g. flipping a feature off in the config).
//
// Transport-free by construction: authoring is a set of PURE functions `(local, req, config) -> bytes`.
// PropertyListSerialization (binary) reads the relayed local bplist + the request and serializes the
// authored response — the precedent is MetadataWindow.swift:604 / SettingsWindow, and it adds ZERO new
// dependencies. bplist is a standard on-wire format, so bytes the box (Rust `plist`) writes parse here
// and vice versa; the oracle compares SEMANTICALLY (parse both, normalize ports, walk), so byte-level
// encoder differences between Rust `plist` and Swift PropertyListSerialization never matter.

import Foundation

/// Response-dict keys whose values are BOX-OWNED socket ports/handles (the box binds them, the host
/// copies them through). The live-oracle diff normalizes these to a placeholder before comparing, so a
/// clean session (host authors the same SHAPE the box did) is a MATCH regardless of which ephemeral
/// ports the kernel handed the box this run. Mirrors setup_driver.rs `PORT_KEYS` exactly.
private let portKeys: Set<String> = [
    "timingPort", "eventPort", "keepAlivePort", "dataPort", "controlPort", "streamID",
]

final class AirPlaySetupSession {
    /// SETUP lifecycle state (plan P2): Idle -> Phase1Done -> StreamsActive, reset on full-TEARDOWN /
    /// RS_CLOSE / RS_OPEN. Diagnostic + a light sanity surface; authoring itself is stateless (pure
    /// functions), so a lost transition can never corrupt an authored response. Mirrors `State`.
    enum State { case idle, phase1Done, streamsActive }

    /// Author (P2 — the host writes the response) vs echo (P1 Stage-0 — return the box's local verbatim,
    /// proving the relay path with zero authoring). Mirrors the two modes of the Rust harness's setup-relay.
    enum Mode { case author, echo }

    private(set) var state: State = .idle
    let mode: Mode
    /// Resolved on every access rather than captured once at init (10-M3): a `let config` snapshot
    /// meant the SETUP author kept using the connect-time config even after a mid-session Settings
    /// Save committed a new one. Same actor-crossing shape as `relay.onOpen`'s existing `cfg.data()`
    /// call (this session is driven from the OCBM background read queue via `feed()`/`dispatch()`,
    /// same as `onOpen`/`onRequest`).
    private let configProvider: () -> VehicleConfig
    private var config: VehicleConfig { configProvider() }
    /// Injected diagnostic sink (nil = muted — the "mute-once" test hook: tests leave it nil to silence
    /// authoring/oracle chatter, prod wires it to os_log). Keeping it a closure keeps this file IOKit-free.
    var log: ((String) -> Void)?

    init(configProvider: @escaping () -> VehicleConfig, mode: Mode = .author) {
        self.configProvider = configProvider
        self.mode = mode
    }

    /// Fixed-config convenience init (tests/main.swift's headless harness, which has no live
    /// `VehicleConfigModel` to poll) — wraps the value in a constant provider.
    convenience init(config: VehicleConfig, mode: Mode = .author) {
        self.init(configProvider: { config }, mode: mode)
    }

    /// Reset to Idle — called on full TEARDOWN, RS_CLOSE, and RS_OPEN (a new connection voids any
    /// in-flight session). Idempotent. Mirrors `SetupDriver::reset`.
    func reset() { state = .idle }

    /// Author a SETUP response. Phase 1 vs phase 2 splits on the presence of the `streams` key in the
    /// REQUEST — identical to the box (session.rs:1559). Advances the lifecycle state. Mirrors `on_setup`.
    func onSetup(local: [UInt8], req: [UInt8]) -> [UInt8] {
        if Self.requestHasStreams(req) {
            state = .streamsActive
            return mode == .echo ? local : Self.authorPhase2(local: local, req: req)
        } else {
            state = .phase1Done
            return mode == .echo ? local : Self.authorPhase1(local: local, config: config)
        }
    }

    /// Author a RECORD response. The box's reference answer is a bodyless 200 (session.rs `record`
    /// returns an empty Vec), so the host authors the same empty body. State unchanged. Mirrors `on_record`.
    func onRecord(local: [UInt8]) -> [UInt8] {
        mode == .echo ? local : []
    }

    /// Process a TEARDOWN NOTIFY (no response owed). A FULL teardown (no `streams` key) resets the
    /// session; a partial teardown (a `streams` array) keeps it. Mirrors `on_teardown` / session.rs.
    func onTeardown(req: [UInt8]) {
        if !Self.requestHasStreams(req) { reset() }
    }

    // MARK: - Pure authoring functions (mirror setup_driver.rs)

    /// Does this SETUP/TEARDOWN request carry a `streams` array? (phase-2 / partial-teardown marker).
    static func requestHasStreams(_ req: [UInt8]) -> Bool {
        parseDict(req)?["streams"] != nil
    }

    /// Every feature token the host can author from config. Any token the BOX put in the local
    /// response that is not in this set is box-owned and must be PRESERVED, not overwritten — see
    /// `authorPhase1`.
    static let hostAuthorableFeatures: Set<String> =
        ["hevc", "altScreen", "viewAreas", "cornerMasks", "logTransfer", "mainBuffered"]

    /// Phase-1 authoring: copy every non-`enabledFeatures` key through from the relayed `local` response
    /// (the box-owned control ports timingPort/eventPort/keepAlivePort and anything else the box put
    /// there), and author `enabledFeatures` from the config — UNIONED with any box-only tokens the
    /// local response carried. "The box owns the binds, the host owns the feature policy."
    ///
    /// The union is load-bearing on WIRELESS and was the bug that blocked the 2026-08-10 flip. The box
    /// emits two further tokens there — `iAPChannel` and `sessionManagement`, gated on env vars set at
    /// the wireless spawn site with no config key — and `iAPChannel` is not cosmetic: `/info` advertises
    /// `iAPChannelInfo` AND this echo must carry the token, or iOS never binds a handler and 400s every
    /// `iAPSendMessage` (device-observed 3/3). The iAP2 tunnel's DETECT+SYN rides exactly that command,
    /// so overwriting the list would silently kill the tunnel on every wireless session.
    static func authorPhase1(local: [UInt8], config: VehicleConfig) -> [UInt8] {
        var d = parseDict(local) ?? [:]
        // Appending AFTER the authored six reproduces the box's own order (session.rs emits the two
        // box-only tokens last), which matters because both oracles compare ORDERED sequences. A
        // future box-only token emitted mid-list would need slotting, not appending, or it WARNs.
        let boxOnly = ((d["enabledFeatures"] as? [String]) ?? [])
            .filter { !hostAuthorableFeatures.contains($0) }
        d["enabledFeatures"] = config.enabledFeatures() + boxOnly
        return toBinary(d)
    }

    /// Phase-2 authoring: for each REQUESTED stream, author the per-type response dict — taking the
    /// box-bound `dataPort`/`controlPort` from the matching stream in the relayed `local` response and
    /// authoring the rest (`type`, `streamConnectionID`, and WHICH keys a given type carries). A requested
    /// stream the box did not answer (bind failure, invalid scid, unimplemented type) has no local match
    /// and is likewise omitted — mirroring session.rs, which OMITS unsupported streams rather than
    /// erroring the whole SETUP. Mirrors `author_phase2`.
    static func authorPhase2(local: [UInt8], req: [UInt8]) -> [UInt8] {
        let localStreams = dictStreams(local)
        let reqStreams = dictStreams(req)
        var consumed = [Bool](repeating: false, count: localStreams.count)
        var out: [Any] = []

        for rs in reqStreams {
            guard let rd = rs as? [String: Any] else { continue }
            let ty = intKey(rd, "type") ?? 0
            let scid = intKey(rd, "streamConnectionID") ?? 0
            // Find the box's answer for THIS requested stream: same type, and — for stream types whose
            // local answer echoes the scid (audio 100-102, DataStream 130) — the same scid, so duplicate
            // audio types can't be crossed. Screen 110/111 answers carry no scid, matched on type alone.
            // First unconsumed match wins (request order == box order).
            var matched: Int? = nil
            for (i, ls) in localStreams.enumerated() {
                if consumed[i] { continue }
                guard let ld = ls as? [String: Any] else { continue }
                if intKey(ld, "type") != ty { continue }
                if let localScid = intKey(ld, "streamConnectionID") {
                    if localScid != scid { continue }
                }
                matched = i
                break
            }
            guard let i = matched, let ld = localStreams[i] as? [String: Any] else {
                continue // box omitted this stream — omit it too
            }
            consumed[i] = true
            out.append(authorStream(ty: ty, scid: scid, ld: ld))
        }

        return toBinary(["streams": out])
    }

    /// Author one stream's response dict, per the box's per-type shape (session.rs setup_phase2), pulling
    /// the box-owned port values from the matched local stream dict `ld`:
    ///   * 110 / 111 (screen): `{type, dataPort}` — no scid echoed.
    ///   * 100 / 101 (audio):  `{type, streamConnectionID, dataPort}`.
    ///   * 102 (media audio):  `{type, streamConnectionID, dataPort, controlPort}` — 102 alone has RTCP.
    ///   * anything else (e.g. the 130 RCS DataStream, which only WIRELESS carries): pass the box's local answer
    ///     through verbatim so the oracle stays clean and the box keeps ownership of those binds
    ///     on both transports.
    /// Mirrors `author_stream`.
    static func authorStream(ty: Int, scid: Int, ld: [String: Any]) -> [String: Any] {
        func copyPort(_ r: inout [String: Any], _ k: String) {
            if let v = ld[k] { r[k] = v }
        }
        switch ty {
        case 110, 111:
            var r: [String: Any] = ["type": ty]
            copyPort(&r, "dataPort")
            return r
        case 100...102:
            var r: [String: Any] = ["type": ty, "streamConnectionID": scid]
            copyPort(&r, "dataPort")
            if ty == 102 { copyPort(&r, "controlPort") }
            return r
        default:
            // Out of host-authoring scope (e.g. the 130 RCS DataStream): echo the box's own answer.
            // Permanent design, not a milestone gate — the box owns those binds on both transports.
            return ld
        }
    }

    // MARK: - Live oracle (mirror setup_driver.rs oracle_diff / normalize / collect_diffs)

    /// Compare an authored response against the box's local response, plist-semantically, after
    /// normalizing the box-owned port values (which the host copied through). An empty result = MATCH; a
    /// non-empty result names each diverging path (`enabledFeatures`, `streams[1].controlPort`, …).
    /// Mirrors `oracle_diff`.
    static func oracleDiff(authored: [UInt8], local: [UInt8]) -> [String] {
        let a = normalize(parseValue(authored))
        let l = normalize(parseValue(local))
        var diffs: [String] = []
        collectDiffs("", a, l, &diffs)
        return diffs
    }

    /// Parse response bytes to a plist value, or an empty dict if the body is empty/unparseable (a RECORD
    /// body is legitimately empty). Mirrors `parse_value`.
    private static func parseValue(_ bytes: [UInt8]) -> Any {
        guard !bytes.isEmpty else { return [String: Any]() }
        return (try? PropertyListSerialization.propertyList(from: Data(bytes), options: [], format: nil))
            ?? [String: Any]()
    }

    /// Recursively replace every [`portKeys`] value with a placeholder so the diff is about SHAPE, not
    /// which ephemeral port the kernel handed the box. Dives into `streams[]` dicts too. Mirrors `normalize`.
    private static func normalize(_ v: Any) -> Any {
        if let d = v as? [String: Any] {
            var out = [String: Any]()
            for (k, val) in d {
                out[k] = portKeys.contains(k) ? 0 : normalize(val)
            }
            return out
        }
        if let a = v as? [Any] {
            return a.map(normalize)
        }
        return v
    }

    /// Walk two normalized values in lockstep, pushing a path string for every point they diverge: a
    /// value mismatch, a key present on one side only, or a differing array length. Mirrors `collect_diffs`.
    private static func collectDiffs(_ path: String, _ a: Any, _ b: Any, _ out: inout [String]) {
        if let da = a as? [String: Any], let db = b as? [String: Any] {
            let keys = Set(da.keys).union(db.keys).sorted()
            for k in keys {
                let child = path.isEmpty ? k : "\(path).\(k)"
                switch (da[k], db[k]) {
                case let (va?, vb?): collectDiffs(child, va, vb, &out)
                case (.some, nil): out.append("\(child) (authored-only)")
                case (nil, .some): out.append("\(child) (local-only)")
                case (nil, nil): break
                }
            }
        } else if let aa = a as? [Any], let ab = b as? [Any] {
            if aa.count != ab.count {
                out.append("\(path) (array len \(aa.count) vs \(ab.count))")
            }
            for (i, pair) in zip(aa, ab).enumerated() {
                collectDiffs("\(path)[\(i)]", pair.0, pair.1, &out)
            }
        } else {
            if !anyEqual(a, b) {
                out.append(path.isEmpty ? "<root>" : path)
            }
        }
    }

    /// Scalar equality for plist leaf values (NSNumber / NSString / NSData / Bool bridged to NSObject).
    private static func anyEqual(_ a: Any, _ b: Any) -> Bool {
        if let na = a as? NSObject, let nb = b as? NSObject { return na.isEqual(nb) }
        return false
    }

    // MARK: - plist helpers (mirror setup_driver.rs parse_dict / dict_streams / int_key / to_binary)

    /// Parse response/request bytes into a top-level dictionary, or nil if empty/unparseable/not a dict.
    static func parseDict(_ bytes: [UInt8]) -> [String: Any]? {
        guard !bytes.isEmpty else { return nil }
        return (try? PropertyListSerialization.propertyList(from: Data(bytes), options: [], format: nil))
            as? [String: Any]
    }

    /// The `streams` array of a request/response, or empty.
    static func dictStreams(_ bytes: [UInt8]) -> [Any] {
        (parseDict(bytes)?["streams"] as? [Any]) ?? []
    }

    /// Read an integer plist key (stored as NSNumber whether the box wrote it signed or unsigned).
    static func intKey(_ d: [String: Any], _ k: String) -> Int? {
        guard let v = d[k] else { return nil }
        if let n = v as? NSNumber {
            // Exclude the CFBoolean NSNumber (a `type`/`streamConnectionID` is never a Bool on the wire,
            // but a defensive guard keeps a stray Bool from reading back as 0/1).
            if CFGetTypeID(n) == CFBooleanGetTypeID() { return nil }
            return n.intValue
        }
        if let i = v as? Int { return i }
        return nil
    }

    /// Serialize a dictionary to a binary plist. On the (impossible for these literal dicts) error path,
    /// an empty byte array is still a valid-if-empty response the oracle will flag. Mirrors `to_binary`.
    static func toBinary(_ dict: [String: Any]) -> [UInt8] {
        guard let data = try? PropertyListSerialization.data(fromPropertyList: dict, format: .binary, options: 0)
        else { return [] }
        return [UInt8](data)
    }
}
