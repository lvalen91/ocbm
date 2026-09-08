// ──────────────────────────────────────────────────────────────────────────────
// ConfigIntegrity.swift — "is the config we pushed actually IN FORCE?" (2026-09-05)
// ──────────────────────────────────────────────────────────────────────────────
//
// WHY THIS EXISTS. On 2026-09-05 a wireless CarPlay session ran for hours at the box's COMPILED
// default — 1920×720, H.264 (`crates/vendor/receiver/src/info.rs`) — while this app believed it had
// pushed 2400×960 with HEVC. Touch was scaled against the wrong panel. Nothing said a word:
//
//   * the supervisor's L2 watchdog restarted ocbmd ("ocbmd wedged (alive mtime stale >=1min, gadget
//     CONFIGURED, pid=71)"); the fresh ocbmd unlinked /tmp/carplay_cfg.yaml at startup;
//   * airplayd_wl was left running, the phone reconnected to it, and its `load_device_config` took
//     the ENOENT branch — the ONE branch of that match that logged nothing — and served defaults;
//   * the existing app-side guard ("BOX IS ON BUILT-IN DEFAULTS", keyed on `cfg_crc == 0` in RS_OPEN,
//     AppDelegate's `relay.onOpen`) never ran: the no-config path also clears `appsetup`, so SETUP
//     went box-driven and no RS_OPEN was relayed. The detector was DISARMED by the failure it targets.
//
// Every one of those signals depended on the box saying something. This one does not. The box sends
// the coded frame size in-band in every opcode-1 VideoConfig header (`ScreenGeometry`) and the codec
// is whatever the decoder actually parsed (avcC vs hvcC). Both are what the phone is ACTUALLY
// encoding; both are compared here against what we pushed at SUBSCRIBE. A third check covers the
// case the first two cannot — a real config that happens to equal the default — by expecting an
// RS_OPEN whenever the pushed config asked for app-driven SETUP and A/V nevertheless starts flowing.
//
// LAYERING. `ScreenGeometry` lives in the decrypt path (OCBMAVDecrypt.swift), which must stay free
// of the settings model (`VehicleConfigModel` is @MainActor SwiftUI state the test harness does not
// compile). So the decrypt path only PUBLISHES observations into this Foundation-only singleton, and
// the pushed side is the plain `VehicleConfig` twin (App/VehicleConfig.swift — harness-compiled, the
// same struct `AirPlaySetupSession` authors from), handed over by OCBMClient at the moment a
// SUBSCRIBE actually lands. The comparison is a pure function (`evaluate`) so the harness can pin it.
//
// Transport-independent: wired and wireless both deliver the same VideoConfig header and the same
// decoder config parse, and RS_OPEN rides CH_RTSP on both.
// ──────────────────────────────────────────────────────────────────────────────

import Foundation
import os

/// The outcome of comparing what the box is sending against what we pushed.
enum ConfigVerdict: Equatable, Sendable {
    /// Not enough information yet (nothing pushed, or no VideoConfig this stream).
    case pending
    /// The stream's coded geometry (and codec, when known) match the pushed config, and the SETUP
    /// path is the one the config asked for.
    case inForce(String)
    /// At least one side disagrees. The string is the one line to log — it names both sides.
    case notInForce(String)

    /// `matchesProfile` for the control socket: nil while pending.
    var matches: Bool? {
        switch self {
        case .pending: return nil
        case .inForce: return true
        case .notInForce: return false
        }
    }

    var detail: String? {
        switch self {
        case .pending: return nil
        case .inForce(let s), .notInForce(let s): return s
        }
    }
}

/// Everything `ControlServer`'s `get viewarea` reports under `pushed`/`matchesProfile`.
struct ConfigIntegritySnapshot: Sendable {
    var pushed: VehicleConfig?
    var codedWidth: Int?
    var codedHeight: Int?
    var codec: String?
    /// An RS_OPEN was relayed for the CURRENT video stream (see `noteVideoStreamStarted`).
    var setupOpenSeen: Bool
    /// The `cfg_crc` that RS_OPEN carried, when one was seen.
    var setupOpenCRC: UInt32?
    var verdict: ConfigVerdict
}

final class ConfigIntegrity: @unchecked Sendable {
    static let shared = ConfigIntegrity()

    private let lock = NSLock()
    private var pushed: VehicleConfig?
    private var codedWidth: Int?
    private var codedHeight: Int?
    private var codec: String?
    /// RS_OPEN seen since the previous stream started — consumed by the next `noteVideoStreamStarted`.
    private var setupOpenPending = false
    private var setupOpenPendingCRC: UInt32?
    /// RS_OPEN seen for the stream now flowing.
    private var setupOpenSeen = false
    private var setupOpenCRC: UInt32?
    /// Verdict strings already logged this stream — one line per distinct verdict, never per frame.
    private var logged: Set<String> = []

    private let log = Logger(subsystem: "com.carlink.ocbm", category: "cfgintegrity")
    /// Test seam: receives every verdict string the instance would log (errors AND the one
    /// confirmation line). nil in production ⇒ os_log only.
    var onVerdict: (@Sendable (ConfigVerdict) -> Void)?

    init() {}

    // MARK: - Inputs

    /// The structured twin of the YAML a SUBSCRIBE just carried. Called by `OCBMClient.subscribe()`
    /// ONLY when the write landed — a SUBSCRIBE that failed pushed nothing. A DIFFERENT config than
    /// the last one clears the observations: the box rebuilds the session on a changed push (R3),
    /// so the geometry of the old stream says nothing about the new config.
    func notePushed(_ cfg: VehicleConfig?) {
        lock.lock()
        let changed = cfg != pushed
        pushed = cfg
        if changed { clearStreamLocked() }
        lock.unlock()
    }

    /// RS_OPEN relayed by the box (app-driven SETUP). Arrives at pair-verify, BEFORE the stream it
    /// opens sends its SEAM_KEY, so it is held as "pending" and attributed to the next stream.
    func noteSetupOpen(boxCRC: UInt32) {
        lock.lock()
        setupOpenPending = true
        setupOpenPendingCRC = boxCRC
        lock.unlock()
    }

    /// A new video SEAM_KEY — a new stream/sender (OCBMAVDecrypt's own reading of opcode 0x00). This
    /// is the per-STREAM boundary, deliberately not the per-app-session one: on 2026-09-05 the phone
    /// re-projected through a restarted ocbmd WITHOUT the app tearing down, so an RS_OPEN latched
    /// from the earlier stream would have vouched for a later one that never had its own. The RS_OPEN
    /// that counts is the one seen since the previous stream started.
    func noteVideoStreamStarted() {
        lock.lock()
        clearStreamLocked()
        setupOpenSeen = setupOpenPending
        setupOpenCRC = setupOpenPendingCRC
        setupOpenPending = false
        setupOpenPendingCRC = nil
        lock.unlock()
    }

    /// The coded frame size iOS reports in the VideoConfig header (`ScreenGeometry.observe`, on
    /// change). This is the moment A/V is flowing for this stream, so the RS_OPEN expectation is
    /// judged here too.
    func noteCoded(width: Int, height: Int) {
        lock.lock()
        codedWidth = width
        codedHeight = height
        let v = evaluateLocked()
        lock.unlock()
        emit(v)
    }

    /// The codec the decoder actually parsed for the MAIN lane ("HEVC" / "H.264", `VideoCodec`'s
    /// raw values). Arrives from the bridge's avcC/hvcC parse, just after the VideoConfig record.
    func noteCodec(_ label: String) {
        lock.lock()
        codec = label
        let v = evaluateLocked()
        lock.unlock()
        emit(v)
    }

    /// App-session teardown (AppDelegate, next to `ScreenGeometry.reset()`). Keeps `pushed`: it is
    /// still what the box was last told, and the control socket reports it as such.
    func sessionEnded() {
        lock.lock()
        clearStreamLocked()
        setupOpenPending = false
        setupOpenPendingCRC = nil
        lock.unlock()
    }

    // MARK: - Output

    var snapshot: ConfigIntegritySnapshot {
        lock.lock(); defer { lock.unlock() }
        return ConfigIntegritySnapshot(pushed: pushed, codedWidth: codedWidth, codedHeight: codedHeight,
                                       codec: codec, setupOpenSeen: setupOpenSeen,
                                       setupOpenCRC: setupOpenCRC, verdict: evaluateLocked())
    }

    // MARK: - The comparison (pure — pinned by tests/main.swift)

    /// Compare the observed stream against the pushed config.
    ///
    /// * geometry: coded W×H must equal the pushed main panel (`mainWidth`×`mainHeight`);
    /// * codec: "HEVC" ⇔ `enablesHEVC`. The pushed-HEVC/got-H.264 direction is the tell seen on
    ///   2026-09-05; the reverse is impossible unless the box is running some other config, so both
    ///   directions count;
    /// * RS_OPEN: with `appDrivenSetup` pushed, a stream that reaches VideoConfig without an RS_OPEN
    ///   means SETUP was box-driven — the box never loaded our config (that is the ONLY way the
    ///   `appsetup` lever is off while we asked for it). This is the check the other two miss when a
    ///   real config happens to equal the default.
    ///
    /// `.pending` until the coded size is known; the codec is optional (the bridge latches it a
    /// moment later, and only the main lane feeds it).
    static func evaluate(pushed: VehicleConfig?, codedWidth: Int?, codedHeight: Int?,
                         codec: String?, setupOpenSeen: Bool) -> ConfigVerdict {
        guard let pushed, let w = codedWidth, let h = codedHeight else { return .pending }
        let pushedCodec = pushed.enablesHEVC ? "HEVC" : "H.264"
        let geometryMismatch = w != pushed.mainWidth || h != pushed.mainHeight
        let codecMismatch = codec.map { $0 != pushedCodec } ?? false
        let openMissing = pushed.appDrivenSetup && !setupOpenSeen
        let observed = "box coded \(w)x\(h) (\(codec ?? "codec pending"))"
        let expected = "pushed \(pushed.mainWidth)x\(pushed.mainHeight) (\(pushedCodec))"
        guard geometryMismatch || codecMismatch || openMissing else {
            return .inForce("CONFIG IN FORCE: \(observed) matches \(expected)"
                            + (pushed.appDrivenSetup ? ", RS_OPEN seen" : ""))
        }
        var s = "CONFIG NOT IN FORCE: \(observed), \(expected)"
        if openMissing {
            // Same wording as AppDelegate's RS_OPEN `cfg_crc == 0` arm — this IS that verdict,
            // reached by the path that arm cannot see (no RS_OPEN ⇒ that arm never runs).
            s += " — no RS_OPEN although appDrivenSetup=true: SETUP went box-driven ⇒ "
               + "⚠️ BOX IS ON BUILT-IN DEFAULTS — our push never landed or was rejected"
        }
        s += " (touch is scaled against the box's panel, not ours; re-SUBSCRIBE / check the box log)"
        return .notInForce(s)
    }

    // MARK: - Internals

    private func clearStreamLocked() {
        codedWidth = nil; codedHeight = nil; codec = nil
        setupOpenSeen = false; setupOpenCRC = nil
        logged.removeAll()
    }

    private func evaluateLocked() -> ConfigVerdict {
        Self.evaluate(pushed: pushed, codedWidth: codedWidth, codedHeight: codedHeight,
                      codec: codec, setupOpenSeen: setupOpenSeen)
    }

    /// One line per distinct verdict per stream. `notInForce` is an error; the matching case logs a
    /// single confirmation so a log reader can see the check RAN, not merely that it was silent.
    private func emit(_ v: ConfigVerdict) {
        guard let text = v.detail else { return }
        lock.lock()
        let fresh = logged.insert(text).inserted
        lock.unlock()
        guard fresh else { return }
        switch v {
        case .notInForce: log.error("\(text, privacy: .public)")
        case .inForce: log.info("\(text, privacy: .public)")
        case .pending: break
        }
        onVerdict?(v)
    }
}
