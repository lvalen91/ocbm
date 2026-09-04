// ──────────────────────────────────────────────────────────────────────────────
// tests/main.swift — hardware-free CLI test harness (OCBM)
// ──────────────────────────────────────────────────────────────────────────────
// The CPC200-CCPA dongle is not always available, so everything that can be
// verified without hardware lives here. After the legacy Carlinkit 0x55AA55AA
// Protocol/ layer was deleted, this harness covers the LIVE OCBM stack:
//
//   • OCBMFraming / OCBMReassembler — frame build + streaming reassembly, resync
//     from garbage, hcheck corruption, split-across-pushes, oversize rejection.
//   • OCBMClient — the HELLO→SUBSCRIBE→heartbeat→STOP control state machine,
//     driven through a FakeTransport (RawBulkTransport) that records writes and
//     injects reads (pins the C1–C4 seam).
//   • AVCCFastPath — the pure H.264/HEVC length-walk + rewrite helpers.
//
// Run with:  ./tests/run_tests.sh   (compiles the live sources + this file with
// swiftc and executes — no Xcode project, no dongle, no code signing).
//
// The OCBMClient tests run against real DispatchQueue timers (500 ms HELLO cadence,
// 1 Hz heartbeat), so they wait on wall-clock conditions via `waitUntil` with
// generous timeouts and service the main run loop so main-queue completions flush.
// ──────────────────────────────────────────────────────────────────────────────

import CryptoKit
import Foundation
import Synchronization

var failures = 0
var passes = 0

func check(_ condition: Bool, _ name: String) {
    if condition {
        passes += 1
    } else {
        failures += 1
        print("FAIL  \(name)")
    }
}

func section(_ name: String) {
    print("── \(name)")
}

// MARK: - Async test helpers

/// Service the main run loop for `seconds` (drains DispatchQueue.main completions
/// while background queues make wall-clock progress).
func pump(_ seconds: Double) {
    RunLoop.main.run(until: Date().addingTimeInterval(seconds))
}

/// Spin the main run loop in small slices until `cond` holds or `timeout` elapses.
@discardableResult
func waitUntil(_ timeout: Double, _ cond: () -> Bool) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        if cond() { return true }
        RunLoop.main.run(until: Date().addingTimeInterval(0.02))
    }
    return cond()
}

// MARK: - FakeTransport (RawBulkTransport) — records writes, injects reads

final class FakeTransport: RawBulkTransport, @unchecked Sendable {
    private let writesBox = Mutex<[[UInt8]]>([])
    private let startedBox = Mutex<Bool>(false)
    private let stoppedBox = Mutex<Int>(0)
    private var handler: (([UInt8]) -> Void)?

    /// Decides whether a given (already-framed) write "succeeds". Set before connect().
    var writeDecider: ([UInt8]) -> Bool = { _ in true }

    func writeBulk(_ bytes: [UInt8]) -> Bool {
        writesBox.withLock { $0.append(bytes) }   // record every ATTEMPT (success or fail)
        return writeDecider(bytes)
    }
    func setReadHandler(_ handler: @escaping ([UInt8]) -> Void) { self.handler = handler }
    func start() { startedBox.withLock { $0 = true } }
    func stop() { stoppedBox.withLock { $0 += 1 } }

    func inject(_ bytes: [UInt8]) { handler?(bytes) }
    var writes: [[UInt8]] { writesBox.withLock { $0 } }
    var started: Bool { startedBox.withLock { $0 } }
    var stopCount: Int { stoppedBox.withLock { $0 } }
}

/// Count CH_CTRL frames whose first payload byte is `op`.
func ctrlCount(_ t: FakeTransport, _ op: UInt8) -> Int {
    t.writes.filter { f in f.count > OCBM.hdrLen && f[8] == 0x00 && f[9] == 0x00 && f[16] == op }.count
}

/// Build a CH_CTRL frame for injection.
func ctrlFrame(_ payload: [UInt8]) -> [UInt8] {
    OCBM.frame(channel: OCBM.chCtrl, flags: OCBM.fSom | OCBM.fEom, seq: 0, payload: payload)
}

/// Count of frames written on a given channel (checked directly from the frame's channel field,
/// bytes 8-9 — not inferred from an unrelated counter).
func channelFrameCount(_ t: FakeTransport, channel: UInt16) -> Int {
    t.writes.filter { f in f.count > OCBM.hdrLen && OCBM.readLE16(f, 8) == channel }.count
}

/// Payload bytes (post 16-B header) of the LAST frame written on `channel`, or nil if none yet.
func lastPayload(_ t: FakeTransport, channel: UInt16) -> [UInt8]? {
    t.writes.last(where: { f in f.count > OCBM.hdrLen && OCBM.readLE16(f, 8) == channel })
        .map { Array($0[OCBM.hdrLen...]) }
}

/// Parse a hex string (e.g. from a throwaway `xcrun swift` CryptoKit script) into bytes.
func hb(_ s: String) -> [UInt8] {
    var out = [UInt8](); out.reserveCapacity(s.count / 2)
    var i = s.startIndex
    while i < s.endIndex {
        let j = s.index(i, offsetBy: 2)
        out.append(UInt8(s[i..<j], radix: 16)!)
        i = j
    }
    return out
}

// ════════════════════════════════════════════════════════════════════════════
// OCBM framing
// ════════════════════════════════════════════════════════════════════════════

section("OCBM framing round-trip")
do {
    let payload: [UInt8] = (0..<40).map { UInt8($0) }
    let f = OCBM.frame(channel: OCBM.chVideo, flags: OCBM.fSom | OCBM.fEom, seq: 0x11223344, payload: payload)
    check(f.count == OCBM.hdrLen + payload.count, "frame length = header + payload")
    check(OCBM.readLE32(f, 0) == OCBM.magic, "frame magic field")
    check(OCBM.readLE32(f, 4) == UInt32(payload.count), "frame length field")
    check(OCBM.readLE16(f, 8) == OCBM.chVideo, "frame channel field")
    check(f[10] == (OCBM.fSom | OCBM.fEom), "frame flags field")
    check(f[11] == OCBM.hcheck(f[0..<16]), "frame hcheck field")
    check(OCBM.readLE32(f, 12) == 0x11223344, "frame seq field round-trips")

    let r = OCBMReassembler()
    r.push(f)
    if let out = r.next() {
        check(out.channel == OCBM.chVideo, "reassembled channel matches")
        check(out.flags == (OCBM.fSom | OCBM.fEom), "reassembled flags match")
        check(out.payload == payload, "reassembled payload identical")
    } else {
        check(false, "reassembler returned the pushed frame")
    }
    check(r.next() == nil, "no extra frame after the one pushed")
}

section("OCBM framing resync-from-garbage")
do {
    let payload: [UInt8] = [0xAA, 0xBB, 0xCC, 0xDD]
    let f = OCBM.frame(channel: OCBM.chMediaAudio, flags: OCBM.fSom | OCBM.fEom, seq: 7, payload: payload)
    let r = OCBMReassembler()
    // Leading junk includes a stray partial-magic byte (0x4D) to exercise the byte-resync.
    r.push([0x00, 0x01, 0x02, 0x4D, 0x42, 0x99] + f)
    if let out = r.next() {
        check(out.channel == OCBM.chMediaAudio && out.payload == payload,
              "recovers a valid frame after leading garbage")
    } else {
        check(false, "resync-from-garbage failed to recover the frame")
    }
    check(r.next() == nil, "nothing left after the recovered frame")
}

section("OCBM framing hcheck corruption")
do {
    let p1: [UInt8] = [1, 2, 3, 4, 5]
    let p2: [UInt8] = [9, 8, 7, 6]
    var f1 = OCBM.frame(channel: OCBM.chVideo, flags: OCBM.fSom | OCBM.fEom, seq: 1, payload: p1)
    let f2 = OCBM.frame(channel: OCBM.chMetadata, flags: OCBM.fSom | OCBM.fEom, seq: 2, payload: p2)
    f1[8] ^= 0x40   // flip a channel header byte WITHOUT recomputing hcheck → header now invalid
    let r = OCBMReassembler()
    r.push(f1 + f2)
    if let out = r.next() {
        check(out.channel == OCBM.chMetadata && out.payload == p2,
              "corrupt-header frame skipped, next frame recovered")
    } else {
        check(false, "hcheck corruption: no frame recovered")
    }
    check(r.next() == nil, "only the intact frame surfaces")
}

section("OCBM framing split across push boundaries")
do {
    // (a) one byte at a time — the reassembler must yield nothing until complete.
    let payload: [UInt8] = Array(repeating: 0x5A, count: 300)
    let f = OCBM.frame(channel: OCBM.chAltVideo, flags: OCBM.fSom | OCBM.fEom, seq: 42, payload: payload)
    let r = OCBMReassembler()
    var got: OCBMFrame?
    var earlyNonNil = false
    for (i, b) in f.enumerated() {
        r.push([b])
        if let out = r.next() {
            if i < f.count - 1 { earlyNonNil = true }
            got = out
        }
    }
    check(!earlyNonNil, "reassembler yields nothing until the frame is complete")
    if let out = got {
        check(out.channel == OCBM.chAltVideo && out.payload == payload,
              "byte-split frame reassembles identically")
    } else {
        check(false, "byte-split frame never reassembled")
    }

    // (b) two frames pushed in 7-byte chunks (header/payload boundaries land arbitrarily).
    let pa: [UInt8] = (0..<100).map { UInt8($0) }
    let pb: [UInt8] = Array(pa.reversed())
    let fa = OCBM.frame(channel: OCBM.chVideo, flags: OCBM.fSom | OCBM.fEom, seq: 1, payload: pa)
    let fb = OCBM.frame(channel: OCBM.chMediaAudio, flags: OCBM.fSom | OCBM.fEom, seq: 2, payload: pb)
    let stream = fa + fb
    let r2 = OCBMReassembler()
    var frames: [OCBMFrame] = []
    var i = 0
    while i < stream.count {
        let end = min(i + 7, stream.count)
        r2.push(Array(stream[i..<end]))
        while let out = r2.next() { frames.append(out) }
        i = end
    }
    check(frames.count == 2, "two frames recovered from a 7-byte-chunked stream")
    if frames.count == 2 {
        check(frames[0].channel == OCBM.chVideo && frames[0].payload == pa, "chunked frame 0 intact")
        check(frames[1].channel == OCBM.chMediaAudio && frames[1].payload == pb, "chunked frame 1 intact")
    }
}

section("OCBM framing oversize payload rejected")
do {
    // Hand-craft a header whose length field claims > maxPayload but with a VALID hcheck, so the
    // reassembler passes magic+hcheck and must reject on the implausible length and byte-resync.
    var hdr = [UInt8](repeating: 0, count: OCBM.hdrLen)
    OCBM.writeLE32(&hdr, 0, OCBM.magic)
    OCBM.writeLE32(&hdr, 4, UInt32(OCBM.maxPayload + 1))
    OCBM.writeLE16(&hdr, 8, OCBM.chVideo)
    hdr[10] = OCBM.fSom | OCBM.fEom
    hdr[11] = OCBM.hcheck(hdr[0..<16])
    OCBM.writeLE32(&hdr, 12, 1)
    let good = OCBM.frame(channel: OCBM.chCtrl, flags: OCBM.fSom | OCBM.fEom, seq: 2, payload: [OCBM.ctHelloAck])
    let r = OCBMReassembler()
    r.push(hdr + good)
    if let out = r.next() {
        check(out.channel == OCBM.chCtrl && out.payload == [OCBM.ctHelloAck],
              "oversize-length header rejected + resynced to the next valid frame")
    } else {
        check(false, "oversize rejection: no recovery frame")
    }
}

// ════════════════════════════════════════════════════════════════════════════
// CH_LOG — parseLogEntries / logCtl (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG)
// ════════════════════════════════════════════════════════════════════════════

/// Build one wire-format CH_LOG entry: [source][flags][seq u16 LE][unix_ms u64 LE][len u16 LE][text].
func logEntryBytes(source: UInt8, flags: UInt8, seq: UInt16, unixMs: UInt64, text: [UInt8]) -> [UInt8] {
    var b: [UInt8] = [source, flags]
    var seqBytes = [UInt8](repeating: 0, count: 2)
    OCBM.writeLE16(&seqBytes, 0, seq)
    b.append(contentsOf: seqBytes)
    for i in 0..<8 { b.append(UInt8((unixMs >> (8 * i)) & 0xff)) }
    var lenBytes = [UInt8](repeating: 0, count: 2)
    OCBM.writeLE16(&lenBytes, 0, UInt16(text.count))
    b.append(contentsOf: lenBytes)
    b.append(contentsOf: text)
    return b
}

section("CH_LOG decode — two-entry frame")
do {
    let e1 = logEntryBytes(source: OCBM.logSourceBox, flags: 0, seq: 10, unixMs: 1_700_000_000_000,
                            text: Array("[ocbmd] hello".utf8))
    let e2 = logEntryBytes(source: OCBM.logSourceBox, flags: 0, seq: 11, unixMs: 1_700_000_000_500,
                            text: Array("[airplayd] world".utf8))
    let entries = parseLogEntries(e1 + e2)
    check(entries.count == 2, "two entries decoded")
    if entries.count == 2 {
        check(entries[0].seq == 10 && entries[0].text == "[ocbmd] hello", "entry 0 fields")
        check(entries[1].seq == 11 && entries[1].text == "[airplayd] world", "entry 1 fields")
        check(!entries[0].isDropped && !entries[0].isTruncated, "entry 0 has no flags set")
    }
}

section("CH_LOG decode — DROPPED marker")
do {
    var countBytes = [UInt8](repeating: 0, count: 4)
    OCBM.writeLE32(&countBytes, 0, 42)
    let bytes = logEntryBytes(source: OCBM.logSourceBox, flags: OCBM.logFlagDropped, seq: 3,
                               unixMs: 1_700_000_001_000, text: countBytes)
    let entries = parseLogEntries(bytes)
    check(entries.count == 1, "one DROPPED entry decoded")
    if let e = entries.first {
        check(e.isDropped, "DROPPED flag parsed")
        check(e.droppedCount == 42, "dropped count decoded from the u32 LE payload")
        check(e.rawLen == 4, "rawLen == 4 for a DROPPED marker")
    }
}

section("CH_LOG decode — TRUNCATED flag")
do {
    let bytes = logEntryBytes(source: OCBM.logSourceBox, flags: OCBM.logFlagTruncated, seq: 4,
                               unixMs: 1_700_000_002_000, text: Array("[iap2d] a very long line".utf8))
    let entries = parseLogEntries(bytes)
    check(entries.count == 1, "one TRUNCATED entry decoded")
    if let e = entries.first {
        check(e.isTruncated, "TRUNCATED flag parsed")
        check(!e.isDropped, "TRUNCATED does not imply DROPPED")
        check(e.text == "[iap2d] a very long line", "TRUNCATED entry still carries its (clipped) text")
    }
}

section("CH_LOG decode — BACKFILL flag")
do {
    let bytes = logEntryBytes(source: OCBM.logSourceBox, flags: OCBM.logFlagBackfill, seq: 5,
                               unixMs: 1_700_000_003_000, text: Array("[iap2d] replayed line".utf8))
    let entries = parseLogEntries(bytes)
    check(entries.count == 1, "one BACKFILL entry decoded")
    if let e = entries.first {
        check(e.isBackfill, "BACKFILL flag parsed")
        check(!e.isDropped && !e.isTruncated, "BACKFILL does not imply DROPPED or TRUNCATED")
    }

    let live = logEntryBytes(source: OCBM.logSourceBox, flags: 0, seq: 6,
                              unixMs: 1_700_000_004_000, text: Array("[iap2d] live line".utf8))
    let liveEntries = parseLogEntries(live)
    check(liveEntries.count == 1 && !liveEntries[0].isBackfill, "an entry with no flags is not backfill")
}

section("CH_LOG decode — malformed tail dropped without trap")
do {
    let good = logEntryBytes(source: OCBM.logSourceBox, flags: 0, seq: 1, unixMs: 1, text: Array("ok".utf8))
    // (a) a truncated header (fewer than 14 bytes) after a good entry.
    let truncatedHeader = good + [0x00, 0x01, 0x02]
    let a = parseLogEntries(truncatedHeader)
    check(a.count == 1 && a[0].text == "ok", "truncated header tail dropped; valid prefix kept")

    // (b) a complete header whose `len` overruns the buffer.
    var overrun = logEntryBytes(source: OCBM.logSourceBox, flags: 0, seq: 2, unixMs: 2, text: Array("xx".utf8))
    OCBM.writeLE16(&overrun, 12, 9999) // claim a huge len, but don't supply the bytes
    let b = parseLogEntries(good + overrun)
    check(b.count == 1 && b[0].text == "ok", "length-overrun tail dropped; earlier valid entry kept")

    // (c) empty payload / payload shorter than one header — never traps.
    check(parseLogEntries([]).isEmpty, "empty payload decodes to no entries")
    check(parseLogEntries([0x00, 0x01]).isEmpty, "sub-header payload decodes to no entries")
}

section("CH_LOG — logSourceName table")
do {
    check(OCBM.logSourceName(OCBM.logSourceBox) == "box", "source 0 -> box")
    check(OCBM.logSourceName(OCBM.logSourceAirplayd) == "airplayd", "source 1 -> airplayd")
    check(OCBM.logSourceName(OCBM.logSourceRadioBtAttach) == "radio_bt_attach", "source 8 -> radio_bt_attach")
    check(OCBM.logSourceName(OCBM.logSourceTailer) == "internal", "source 255 -> internal")
    check(OCBM.logSourceName(200) == "src200", "unknown source id falls back to src<id>")
}

section("CH_LOG — logCtl payload builder")
do {
    check(OCBM.logCtl(enabled: true, capKB: 256) == [0x1B, 0x01, 0x00, 0x01],
          "logCtl(enabled: true, capKB: 256) == [0x1B, 0x01, 0x00, 0x01]")
    check(OCBM.logCtl(enabled: false, capKB: 0) == [0x1B, 0x00, 0x00, 0x00],
          "logCtl(enabled: false, capKB: 0) == [0x1B, 0x00, 0x00, 0x00]")
}

// ════════════════════════════════════════════════════════════════════════════
// OCBMClient — subscribe state machine (C1–C4 seam)
// ════════════════════════════════════════════════════════════════════════════

section("OCBM client — HELLO / SUBSCRIBE gating + STOP")
do {
    let t = FakeTransport()
    let c = OCBMClient(transport: t)
    c.connect()
    check(t.started, "connect() starts the transport")
    check(waitUntil(3.0) { ctrlCount(t, OCBM.ctHello) >= 2 },
          "HELLO retransmitted until ACK (>=2 sends)")
    check(ctrlCount(t, OCBM.ctSubscribe) == 0, "no SUBSCRIBE before HELLO_ACK")

    t.inject(ctrlFrame([OCBM.ctHelloAck]))
    check(waitUntil(3.0) { ctrlCount(t, OCBM.ctSubscribe) >= 1 },
          "SUBSCRIBE sent only after HELLO_ACK")

    let stopsBefore = ctrlCount(t, OCBM.ctStop)
    c.disconnect()
    check(waitUntil(2.0) { ctrlCount(t, OCBM.ctStop) == stopsBefore + 1 },
          "disconnect() sends CT_STOP")
    pump(0.3)
    check(ctrlCount(t, OCBM.ctStop) == stopsBefore + 1, "CT_STOP sent exactly once")
    check(t.stopCount >= 1, "transport.stop() called on disconnect")
}

section("OCBM client — failed SUBSCRIBE retried on heartbeat")
do {
    let t = FakeTransport()
    // Fail ONLY SUBSCRIBE writes; HELLO / heartbeat / STOP succeed.
    t.writeDecider = { f in
        !(f.count > OCBM.hdrLen && f[8] == 0x00 && f[9] == 0x00 && f[16] == OCBM.ctSubscribe)
    }
    let c = OCBMClient(transport: t)
    c.connect()
    check(waitUntil(3.0) { ctrlCount(t, OCBM.ctHello) >= 1 }, "HELLO sent")
    t.inject(ctrlFrame([OCBM.ctHelloAck]))
    check(waitUntil(4.0) { ctrlCount(t, OCBM.ctSubscribe) >= 2 },
          "failed SUBSCRIBE retried on the next heartbeat tick (>=2 attempts)")

    // subscribed stayed false → a UI send is dropped, never reaching the wire.
    let outBox = Mutex<SendOutcome?>(nil)
    let inputFramesBefore = channelFrameCount(t, channel: OCBM.chInput)
    c.sendCommand(OCBM.cmdSiriDown) { o in outBox.withLock { $0 = o } }
    check(waitUntil(2.0) {
        if case .droppedNotSubscribed? = outBox.withLock({ $0 }) { return true }
        return false
    }, "sendCommand while SUBSCRIBE never landed → droppedNotSubscribed")
    // 04-M2 fix: the old assertion (`t.writes.count == writesBefore || ctrlCount(...) >= 2`) was
    // tautological — the right disjunct was already guaranteed true by the earlier check on line
    // ~264, so this could never fail regardless of what sendCommand actually did. Assert the actual
    // property: no CH_INPUT frame was emitted for the dropped send (checked via the frame's own
    // channel field, not an unrelated SUBSCRIBE counter).
    check(channelFrameCount(t, channel: OCBM.chInput) == inputFramesBefore,
          "dropped input emitted no CH_INPUT frame")
    c.disconnect()
}

section("OCBM client — send before subscribe")
do {
    let t = FakeTransport()
    let c = OCBMClient(transport: t)   // never connect: subscribed == false
    let outBox = Mutex<SendOutcome?>(nil)
    c.sendCommand(OCBM.cmdSiriUp) { o in outBox.withLock { $0 = o } }
    check(waitUntil(2.0) {
        if case .droppedNotSubscribed? = outBox.withLock({ $0 }) { return true }
        return false
    }, "sendCommand before any connect/SUBSCRIBE → droppedNotSubscribed")
    check(t.writes.isEmpty, "dropped input never reached the wire")
}

section("OCBM client — SEV_HOST_GONE re-subscribe")
do {
    let t = FakeTransport()
    let c = OCBMClient(transport: t)
    let subStates = Mutex<[Bool]>([])
    c.onSubscriptionState = { s in subStates.withLock { $0.append(s) } }
    c.connect()
    check(waitUntil(3.0) { ctrlCount(t, OCBM.ctHello) >= 1 }, "HELLO sent (host-gone case)")
    t.inject(ctrlFrame([OCBM.ctHelloAck]))
    check(waitUntil(3.0) { ctrlCount(t, OCBM.ctSubscribe) >= 1 }, "initial SUBSCRIBE landed")
    check(waitUntil(1.0) { subStates.withLock { $0.contains(true) } },
          "onSubscriptionState(true) fired after SUBSCRIBE")

    let subsBefore = ctrlCount(t, OCBM.ctSubscribe)
    // SEV_HOST_GONE: [ctSessionEvent, OCBM.sevHostGone] — the real box byte (ocbm-proto SEV_HOST_GONE
    // = 0x02). 0x00 is an undefined SEV_* byte, which OCBMClient now correctly logs-and-ignores
    // (fix for "unknown SEV treated as HOST GONE") rather than treating as GONE.
    t.inject(ctrlFrame([OCBM.ctSessionEvent, OCBM.sevHostGone]))
    check(waitUntil(1.0) { subStates.withLock { $0.contains(false) } },
          "SEV_HOST_GONE drops subscribed (onSubscriptionState false)")
    check(waitUntil(3.0) { ctrlCount(t, OCBM.ctSubscribe) >= subsBefore + 1 },
          "SEV_HOST_GONE triggers a fresh SUBSCRIBE")
    c.disconnect()
}

section("OCBM client — CT_BOX_HEALTH / CT_BT_PHASE parsed + delivered")
do {
    let t = FakeTransport()
    let c = OCBMClient(transport: t)
    let healthBox = Mutex<UInt8?>(nil)
    let phaseBox = Mutex<UInt8?>(nil)
    c.onBoxHealth = { bits in healthBox.withLock { $0 = bits } }
    c.onBtPhase = { phase in phaseBox.withLock { $0 = phase } }

    // Synthetic box→host frames on CH_CTRL, injected directly (no live box needed).
    let healthBits: UInt8 = OCBM.bhHciPresent | OCBM.bhIap2d | OCBM.bhRootfsOk
    t.inject(ctrlFrame([OCBM.ctBoxHealth, healthBits]))
    check(waitUntil(1.0) { healthBox.withLock { $0 } == healthBits },
          "CT_BOX_HEALTH parsed + delivered via onBoxHealth")

    t.inject(ctrlFrame([OCBM.ctBtPhase, OCBM.btpIdentifying]))
    check(waitUntil(1.0) { phaseBox.withLock { $0 } == OCBM.btpIdentifying },
          "CT_BT_PHASE parsed + delivered via onBtPhase")

    // Malformed (truncated) frames must never trap — no callback fires, no crash.
    healthBox.withLock { $0 = nil }
    phaseBox.withLock { $0 = nil }
    t.inject(ctrlFrame([OCBM.ctBoxHealth]))   // missing bitmask byte
    t.inject(ctrlFrame([OCBM.ctBtPhase]))     // missing phase byte
    pump(0.2)
    check(healthBox.withLock { $0 } == nil, "truncated CT_BOX_HEALTH does not fire onBoxHealth")
    check(phaseBox.withLock { $0 } == nil, "truncated CT_BT_PHASE does not fire onBtPhase")
}

section("OCBM client — CT_PHONE_IDENT parsed + cleared")
do {
    let t = FakeTransport()
    let c = OCBMClient(transport: t)
    let identBox = Mutex<PhoneIdent??>(nil)
    c.onPhoneIdent = { ident in identBox.withLock { $0 = ident } }

    let json = #"{"name":"Test Phone","deviceID":"AA:BB:CC:DD:EE:FF","model":"iPhone99,9","osName":"iPhoneOS","osVersion":"99.0"}"#
    t.inject(ctrlFrame([OCBM.ctPhoneIdent] + Array(json.utf8)))
    check(waitUntil(1.0) { identBox.withLock { $0 } != nil }, "CT_PHONE_IDENT JSON decoded")
    check((identBox.withLock { $0 } ?? nil)?.model == "iPhone99,9", "CT_PHONE_IDENT model field correct")

    identBox.withLock { $0 = nil }
    t.inject(ctrlFrame([OCBM.ctPhoneIdent]))   // empty payload = cleared
    check(waitUntil(1.0) { identBox.withLock { $0 } != nil },
          "CT_PHONE_IDENT empty payload delivers (nil ident)")
    check(identBox.withLock { $0 } == .some(nil), "CT_PHONE_IDENT empty payload clears the identity")
}

section("OCBM client — CH_LOG: auto CT_LOG_CTL after SUBSCRIBE, entries delivered, seq gap marker")
do {
    let t = FakeTransport()
    let c = OCBMClient(transport: t)
    let received = Mutex<[[LogEntry]]>([])
    c.onBoxLog = { entries in received.withLock { $0.append(entries) } }
    c.connect()
    check(waitUntil(3.0) { ctrlCount(t, OCBM.ctHello) >= 1 }, "HELLO sent (CH_LOG case)")
    t.inject(ctrlFrame([OCBM.ctHelloAck]))
    check(waitUntil(3.0) { ctrlCount(t, OCBM.ctSubscribe) >= 1 }, "SUBSCRIBE landed (CH_LOG case)")
    check(waitUntil(2.0) { ctrlCount(t, OCBM.ctLogCtl) >= 1 },
          "CT_LOG_CTL auto-sent right after SUBSCRIBE (logStreamEnabled defaults true)")
    if let f = t.writes.last(where: { f in
        f.count > OCBM.hdrLen && OCBM.readLE16(f, 8) == OCBM.chCtrl && f[OCBM.hdrLen] == OCBM.ctLogCtl
    }) {
        check(Array(f[OCBM.hdrLen...]) == OCBM.logCtl(enabled: true, capKB: 256),
              "auto CT_LOG_CTL payload == enabled/default 256 KB")
    } else {
        check(false, "no CT_LOG_CTL frame found on the wire")
    }

    // seq 0 (baseline), then a jump straight to seq 5 — a loss the client must surface as a marker.
    let e0 = logEntryBytes(source: OCBM.logSourceAirplayd, flags: 0, seq: 0, unixMs: 1_000,
                            text: Array("line0".utf8))
    t.inject(OCBM.frame(channel: OCBM.chLog, flags: OCBM.fSom | OCBM.fEom, seq: 0, payload: e0))
    check(waitUntil(1.0) { received.withLock { $0.count } >= 1 }, "first CH_LOG batch delivered")

    let e5 = logEntryBytes(source: OCBM.logSourceAirplayd, flags: 0, seq: 5, unixMs: 2_000,
                            text: Array("line5".utf8))
    t.inject(OCBM.frame(channel: OCBM.chLog, flags: OCBM.fSom | OCBM.fEom, seq: 0, payload: e5))
    check(waitUntil(1.0) { received.withLock { $0.count } >= 2 }, "second CH_LOG batch delivered")

    let batch2 = received.withLock { $0.count >= 2 ? $0[1] : [] }
    check(batch2.count == 2, "seq gap synthesizes one extra marker entry ahead of the real one")
    if batch2.count == 2 {
        check(batch2[0].isGapMarker, "synthesized entry flagged isGapMarker")
        check(batch2[0].source == OCBM.logSourceAirplayd,
              "gap marker stamped with the SAME source as the entry that revealed the gap")
        check(batch2[1].seq == 5 && batch2[1].text == "line5" && !batch2[1].isGapMarker,
              "the real entry follows the marker, untouched")
    }

    let logCtlBefore = ctrlCount(t, OCBM.ctLogCtl)
    c.disconnect()
    check(waitUntil(2.0) { ctrlCount(t, OCBM.ctLogCtl) >= logCtlBefore + 1 },
          "app-initiated disconnect() sends CT_LOG_CTL disabled")
}

// ════════════════════════════════════════════════════════════════════════════
// AVCCFastPath (pure H.264/HEVC length-walk + rewrite + V4 drop table)
// ════════════════════════════════════════════════════════════════════════════

section("AVCCFastPath length-walk")

func avcc(_ nals: [[UInt8]]) -> [UInt8] {
    var out: [UInt8] = []
    for nal in nals {
        let n = UInt32(nal.count)
        out.append(UInt8((n >> 24) & 0xFF)); out.append(UInt8((n >> 16) & 0xFF))
        out.append(UInt8((n >> 8) & 0xFF));  out.append(UInt8(n & 0xFF))
        out.append(contentsOf: nal)
    }
    return out
}
func walk(_ bytes: [UInt8], isHEVC: Bool) -> AVCCFastPath.Walk {
    bytes.withUnsafeBytes { AVCCFastPath.walkAVCC($0, isHEVC: isHEVC) }
}

let h264KF = walk(avcc([[0x67, 0x42], [0x68, 0xCE], [0x65, 0x88, 0x84]]), isHEVC: false)
check(h264KF.valid, "H.264 keyframe AU is valid")
check(h264KF.containsIDR, "H.264 IDR (type 5) detected")
check(h264KF.hasParamSets, "H.264 in-band SPS/PPS (7/8) detected")

let h264P = walk(avcc([[0x41, 0x9A, 0x00]]), isHEVC: false)
check(h264P.valid && !h264P.containsIDR && !h264P.hasParamSets, "H.264 P-frame: valid, no IDR, no param sets")

func hevcHdr(_ type: UInt8) -> UInt8 { (type << 1) & 0x7E }
let hevcKF = walk(avcc([[hevcHdr(32), 0x01], [hevcHdr(33), 0x01],
                        [hevcHdr(34), 0x01], [hevcHdr(19), 0xAF, 0x00]]), isHEVC: true)
check(hevcKF.valid, "HEVC keyframe AU is valid")
check(hevcKF.containsIDR, "HEVC IRAP (type 19) detected")
check(hevcKF.hasParamSets, "HEVC in-band VPS/SPS/PPS (32/33/34) detected")

let hevcP = walk(avcc([[hevcHdr(1), 0x02, 0x00]]), isHEVC: true)
check(hevcP.valid && !hevcP.containsIDR && !hevcP.hasParamSets, "HEVC trailing slice: valid, no IRAP, no param sets")

let truncated: [UInt8] = [0x00, 0x00, 0x00, 0x0A, 0x65, 0x88, 0x84]
check(!walk(truncated, isHEVC: false).valid, "truncated final NAL → invalid")

let zeroLen: [UInt8] = [0x00, 0x00, 0x00, 0x00]
check(!walk(zeroLen, isHEVC: false).valid, "zero-length NAL → invalid")

var trailing = avcc([[0x65, 0x88, 0x84]]); trailing.append(contentsOf: [0xAB, 0xCD])
check(!walk(trailing, isHEVC: false).valid, "trailing garbage → invalid")

check(!walk([], isHEVC: false).valid, "empty AU → invalid")

section("AVCCFastPath rewriteToFourByteLengths")

let refFour = avcc([[0xAA, 0xBB, 0xCC], [0xDD]])
let one: [UInt8] = [0x03, 0xAA, 0xBB, 0xCC, 0x01, 0xDD]
check(AVCCFastPath.rewriteToFourByteLengths(Data(one), lenSize: 1).map(Array.init) == refFour,
      "rewrite lenSize=1 → 4-byte prefixes byte-equal reference")
let two: [UInt8] = [0x00, 0x03, 0xAA, 0xBB, 0xCC, 0x00, 0x01, 0xDD]
check(AVCCFastPath.rewriteToFourByteLengths(Data(two), lenSize: 2).map(Array.init) == refFour,
      "rewrite lenSize=2 → 4-byte prefixes byte-equal reference")
let three: [UInt8] = [0x00, 0x00, 0x03, 0xAA, 0xBB, 0xCC, 0x00, 0x00, 0x01, 0xDD]
check(AVCCFastPath.rewriteToFourByteLengths(Data(three), lenSize: 3).map(Array.init) == refFour,
      "rewrite lenSize=3 → 4-byte prefixes byte-equal reference")
check(AVCCFastPath.rewriteToFourByteLengths(Data([0x00, 0x05, 0xAA]), lenSize: 2) == nil,
      "rewrite rejects truncated input")

section("AVCCFastPath resolveSlot (V4 drop table)")

func rs(_ old: Bool?, _ new: Bool, _ store: Bool, _ reqKF: Bool, _ name: String) {
    let r = AVCCFastPath.resolveSlot(oldIsKF: old, newIsKF: new)
    check(r.store == store && r.requestKeyframe == reqKF, name)
}
rs(nil, false,   true,  false, "empty → store")
rs(false, false, true,  true,  "P over P → replace + requestKeyframe")
rs(false, true,  true,  false, "IDR over P → replace (IDR repaints)")
rs(true, false,  false, true,  "P over IDR → DROP new P + requestKeyframe")
rs(true, true,   true,  false, "IDR over IDR → replace")

// ════════════════════════════════════════════════════════════════════════════
// App-driven SETUP — AirPlaySetupSession (port of host/ocbm-host/src/setup_driver.rs)
// ════════════════════════════════════════════════════════════════════════════
//
// THREE-WAY EQUIVALENCE (box == Rust harness == Swift). The `bp*` fixtures below are binary plists
// encoded by Python's plistlib (FMT_BINARY) — an implementation independent of BOTH the box's Rust
// `plist` crate and Swift's PropertyListSerialization. They encode the EXACT dicts setup_driver.rs's
// Rust unit tests build (same ports 50001/50002/50003, dataPort 6001, media-audio 7001/7002 + scid
// 0xABCD, screen scid 0x1122). Proving Swift's author + oracle produce an empty diff against these
// bytes — the same logical inputs the Rust tests assert on — establishes that all three implementations
// agree on the SETUP response shape. (bplist is a standard on-wire format, so bytes the box's Rust
// `plist` writes parse here identically; the oracle compares semantically, so encoder byte differences
// never matter.)

func bp(_ b64: String) -> [UInt8] { [UInt8](Data(base64Encoded: b64)!) }

// Python-plistlib fixtures (see scratchpad generator). Names match the Rust test dicts.
let fxP1        = "YnBsaXN0MDDUAQIDBAUKCwxfEA9lbmFibGVkRmVhdHVyZXNZZXZlbnRQb3J0XWtlZXBBbGl2ZVBvcnRadGltaW5nUG9ydKQGBwgJVGhldmNZdmlld0FyZWFzW2Nvcm5lck1hc2tzW2xvZ1RyYW5zZmVyEcNSEcNTEcNRCBEjLTtGS1BaZnJ1eAAAAAAAAAEBAAAAAAAAAA0AAAAAAAAAAAAAAAAAAAB7"
let fxP1Hevc    = "YnBsaXN0MDDTAQIDBAYHXxAPZW5hYmxlZEZlYXR1cmVzWWV2ZW50UG9ydFp0aW1pbmdQb3J0oQVUaGV2YxHDUhHDUQgPISs2OD1AAAAAAAAAAQEAAAAAAAAACAAAAAAAAAAAAAAAAAAAAEM="
let fxScrLocal  = "YnBsaXN0MDDRAQJXc3RyZWFtc6ED0gQFBgdYZGF0YVBvcnRUdHlwZREXcRBuCAsTFRojKCsAAAAAAAABAQAAAAAAAAAIAAAAAAAAAAAAAAAAAAAALQ=="
let fxScrReq    = "YnBsaXN0MDDRAQJXc3RyZWFtc6ED0gQFBgdfEBJzdHJlYW1Db25uZWN0aW9uSURUdHlwZRERIhBuCAsTFRovNDcAAAAAAAABAQAAAAAAAAAIAAAAAAAAAAAAAAAAAAAAOQ=="
let fxMaLocal   = "YnBsaXN0MDDRAQJXc3RyZWFtc6ED1AQFBgcICQoLW2NvbnRyb2xQb3J0WGRhdGFQb3J0XxASc3RyZWFtQ29ubmVjdGlvbklEVHR5cGURG1oRG1kRq80QZggLExUeKjNITVBTVgAAAAAAAAEBAAAAAAAAAAwAAAAAAAAAAAAAAAAAAABY"
let fxMaReq     = "YnBsaXN0MDDRAQJXc3RyZWFtc6ED0gQFBgdfEBJzdHJlYW1Db25uZWN0aW9uSURUdHlwZRGrzRBmCAsTFRovNDcAAAAAAAABAQAAAAAAAAAIAAAAAAAAAAAAAAAAAAAAOQ=="
let fxOmitLocal = "YnBsaXN0MDDRAQJXc3RyZWFtc6ED0gQFBgdYZGF0YVBvcnRUdHlwZREXcRBuCAsTFRojKCsAAAAAAAABAQAAAAAAAAAIAAAAAAAAAAAAAAAAAAAALQ=="
let fxOmitReq   = "YnBsaXN0MDDRAQJXc3RyZWFtc6IDCNIEBQYHXxASc3RyZWFtQ29ubmVjdGlvbklEVHR5cGUQCRBu0gQFBgkQeAgLExYbMDU3OT4AAAAAAAABAQAAAAAAAAAKAAAAAAAAAAAAAAAAAAAAQA=="
// The wireless RCS DataStream (type 130). The REQUEST carries no `streamConnectionID` at all — the
// key set is taken from docs/ops/captures/2026-07-25_SUCCESS_airplayd_wl_handshake.txt:25 — and the box's
// local answer echoes scid 0 plus the `streamID` transport token and the bound dataPort.
let fxDsLocal   = "YnBsaXN0MDDRAQJXc3RyZWFtc6ED1AQFBgcICQoLWGRhdGFQb3J0XxASc3RyZWFtQ29ubmVjdGlvbklEWHN0cmVhbUlEVHR5cGURyGkQABABEIIICxMVHic8RUpNT1EAAAAAAAABAQAAAAAAAAAMAAAAAAAAAAAAAAAAAAAAUw=="
let fxDsReq     = "YnBsaXN0MDDRAQJXc3RyZWFtc6ED0wQFBgcICVljaGFubmVsSURUdHlwZV8QFHdhbnRzRGVkaWNhdGVkU29ja2V0XxAXNUU6Rjc6Rjc6QTk6Q0I6Q0QtUkNTLTEQggkICxMVHCYrQlxeAAAAAAAAAQEAAAAAAAAACgAAAAAAAAAAAAAAAAAAAF8="

section("SETUP — cross-language bplist decode (Python plistlib → Swift)")
do {
    // Proves Swift PropertyListSerialization reads the independently-encoded on-wire bplist the box emits.
    let d = AirPlaySetupSession.parseDict(bp(fxP1))
    check(d != nil, "external bplist parses in Swift")
    check(AirPlaySetupSession.intKey(d ?? [:], "timingPort") == 50001, "timingPort read from external bplist")
    check((d?["enabledFeatures"] as? [Any])?.count == 4, "enabledFeatures array read from external bplist")
}

section("YAML emission — free-text scalar escaping (YamlEmit.quotedBody)")
do {
    // Escape ORDER is load-bearing: backslash first, then quote. Reversing it double-escapes the
    // backslashes the quote-pass introduces and re-opens the bug where one character in a cosmetic
    // field malforms the WHOLE pushed document (the box then falls back to its built-in defaults
    // for resolution/HEVC/appDrivenSetup/audio). Verified against serde_yaml 0.9 during the gate.
    check(YamlEmit.quotedBody("CarLink") == "CarLink", "plain text passes through byte-identical")
    check(YamlEmit.quotedBody("Say \"Hi\"") == "Say \\\"Hi\\\"", "double quotes are escaped")
    check(YamlEmit.quotedBody("back\\slash") == "back\\\\slash", "backslashes are escaped")
    // The regression the wrong order produces: `\"` must become `\\\"`, not `\\\\"`.
    check(YamlEmit.quotedBody("mix\\\"both") == "mix\\\\\\\"both", "backslash-then-quote order preserved")
    // C0 control chars are rejected at the YAML STREAM level — no escaping fixes them, so they go.
    check(YamlEmit.quotedBody("bell\u{07}esc\u{1B}") == "bellesc", "control characters are stripped")
    check(YamlEmit.quotedBody("Citro\u{eb}n \u{2014} \u{fc}n\u{ef}code") == "Citro\u{eb}n \u{2014} \u{fc}n\u{ef}code",
          "non-ASCII is left intact")
    // Cf (format) characters are NOT fatal to the parser, so stripping them would be silent data
    // loss in a field whose whole purpose is display text. Pinned so the strip set can't widen back
    // to CharacterSet.controlCharacters (Cc u Cf), which mangles emoji ZWJ + RTL labels.
    check(YamlEmit.quotedBody("Family \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F466}")
            == "Family \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F466}",
          "emoji ZWJ sequence survives (Cf preserved)")
    check(YamlEmit.quotedBody("AMG\u{200F} x") == "AMG\u{200F} x", "bidi mark survives (Cf preserved)")
    // Classic escaper off-by-one: a trailing backslash must not leave the scalar open.
    check(YamlEmit.quotedBody("ends\\") == "ends\\\\", "trailing backslash is escaped")
    check(YamlEmit.quotedBody("") == "", "empty string round-trips")
    // The persistence path (save()) shares this exact set, so a saved name keeps its emoji/bidi
    // marks — it used to use CharacterSet.controlCharacters and mangled them in the text field.
    check(YamlEmit.stripFatalControls("Family \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F466}")
            == "Family \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F466}",
          "persisted-name strip preserves Cf too")
    check(YamlEmit.stripFatalControls("bell\u{07}x") == "bellx", "persisted-name strip removes Cc")
    // 10-L3: unsafe-libyaml's reader rejects U+FFFE/U+FFFF at the stream level even though they are
    // valid Swift Unicode.Scalars that survive quotedBody's escaping otherwise.
    check(YamlEmit.quotedBody("no\u{FFFE}pe") == "nope", "U+FFFE noncharacter is stripped")
    check(YamlEmit.quotedBody("no\u{FFFF}pe") == "nope", "U+FFFF noncharacter is stripped")
}

section("SETUP — VehicleConfig.enabledFeatures gating + order")
do {
    // Mirrors setup_driver.rs phase1_reproduces_box_local_response's cfg assertion + emission ORDER.
    let cfg = VehicleConfig(enablesHEVC: true, enablesViewAreas: true,
                            enablesCornerMasks: true, enablesLogTransfer: true,
                            enablesMainBufferedAudio: true)
    check(cfg.enabledFeatures() == ["hevc", "viewAreas", "cornerMasks", "logTransfer", "mainBuffered"],
          "order + membership match session.rs setup_phase1")
    // WIRELESS shape (2026-08-10 flip): the box emits iAPChannel + sessionManagement, which the app
    // has no config key for. They MUST survive authoring — /info advertises iAPChannelInfo and iOS
    // 400s every iAPSendMessage without the echo, killing the iAP2 tunnel on every wireless session.
    let wirelessLocal = AirPlaySetupSession.toBinary(
        ["enabledFeatures": ["hevc", "iAPChannel", "sessionManagement"], "timingPort": 50001])
    let authoredW = AirPlaySetupSession.authorPhase1(local: wirelessLocal,
                                                     config: VehicleConfig(enablesHEVC: true))
    let wf = (AirPlaySetupSession.parseDict(authoredW)?["enabledFeatures"] as? [String]) ?? []
    check(wf == ["hevc", "iAPChannel", "sessionManagement"],
          "wireless: box-only feature tokens are preserved through host authoring")

    // alt_screen_feature_from_alt_video_streams: altScreen comes from altVideoStreamsPresent, slotted
    // after hevc.
    let cfgAlt = VehicleConfig(enablesHEVC: true, altVideoStreamsPresent: true)
    check(cfgAlt.enabledFeatures() == ["hevc", "altScreen"], "altScreen from altVideoStreamsPresent")
    // viewAreas turns on for cornerMasks even without enablesViewAreas.
    let cfgCM = VehicleConfig(enablesCornerMasks: true)
    check(cfgCM.enabledFeatures() == ["viewAreas", "cornerMasks"], "cornerMasks implies viewAreas")
    // malformed_yaml_is_empty_features analogue: the all-false default authors nothing.
    check(VehicleConfig().enabledFeatures().isEmpty, "default config → empty feature set")

    // 03-M2 fix: a real safe-area inset arms viewAreas even with enablesViewAreas/enablesCornerMasks
    // both off, mirroring the box's view_areas_enabled() auto-arm (vehicle_config.rs:874).
    var cfgInset = VehicleConfig()
    cfgInset.safeAreaInsetPresent = true
    check(cfgInset.enabledFeatures() == ["viewAreas"], "safeAreaInsetPresent alone arms viewAreas")
    // No-inset default (0,0,0,0 edges, full panel) must NOT arm it — byte-equivalent to pre-fix output.
    check(VehicleConfig.hasRealSafeAreaInset(left: 0, top: 0, right: 0, bottom: 0, width: 1920, height: 1080) == false,
          "zero edges cover the full panel — not a real inset")
    check(VehicleConfig.hasRealSafeAreaInset(left: 40, top: 0, right: 0, bottom: 0, width: 1920, height: 1080) == true,
          "a nonzero edge that doesn't cover the panel IS a real inset")
    // Full-coverage inset expressed as edges landing exactly on the panel bounds is still a no-op.
    check(VehicleConfig.hasRealSafeAreaInset(left: 0, top: 0, right: 0, bottom: 0, width: 800, height: 480) == false,
          "full-bleed alt panel is not a real inset")
}

section("SETUP — phase-1 authoring reproduces the box local (oracle clean)")
do {
    let cfg = VehicleConfig(enablesHEVC: true, enablesViewAreas: true,
                            enablesCornerMasks: true, enablesLogTransfer: true)
    let local = bp(fxP1)
    let authored = AirPlaySetupSession.authorPhase1(local: local, config: cfg)
    let diff = AirPlaySetupSession.oracleDiff(authored: authored, local: local)
    check(diff.isEmpty, "authored phase-1 == box local after port-normalization: \(diff)")
    // Ports copied through verbatim (not re-invented).
    let ad = AirPlaySetupSession.parseDict(authored) ?? [:]
    check(AirPlaySetupSession.intKey(ad, "timingPort") == 50001, "timingPort copied through")
    check(AirPlaySetupSession.intKey(ad, "eventPort") == 50002, "eventPort copied through")
    check(AirPlaySetupSession.intKey(ad, "keepAlivePort") == 50003, "keepAlivePort copied through")
}

section("SETUP — phase-2 single screen (type 110): {type, dataPort}, no scid")
do {
    let local = bp(fxScrLocal), req = bp(fxScrReq)
    let authored = AirPlaySetupSession.authorPhase2(local: local, req: req)
    check(AirPlaySetupSession.oracleDiff(authored: authored, local: local).isEmpty,
          "screen phase-2 matches the box local")
    let streams = AirPlaySetupSession.dictStreams(authored)
    check(streams.count == 1, "one screen stream authored")
    let sd = streams[0] as? [String: Any] ?? [:]
    check(AirPlaySetupSession.intKey(sd, "type") == 110, "type 110")
    check(AirPlaySetupSession.intKey(sd, "dataPort") == 6001, "box dataPort copied through")
    check(sd["streamConnectionID"] == nil, "type 110 echoes no scid")
}

section("SETUP — phase-2 media-audio (type 102) keeps controlPort + scid")
do {
    let local = bp(fxMaLocal), req = bp(fxMaReq)
    let authored = AirPlaySetupSession.authorPhase2(local: local, req: req)
    check(AirPlaySetupSession.oracleDiff(authored: authored, local: local).isEmpty,
          "media-audio phase-2 matches")
    let sd = AirPlaySetupSession.dictStreams(authored)[0] as? [String: Any] ?? [:]
    check(AirPlaySetupSession.intKey(sd, "controlPort") == 7002, "102 keeps the RTCP controlPort")
    check(AirPlaySetupSession.intKey(sd, "streamConnectionID") == 0xABCD, "scid echoed for audio")
    check(AirPlaySetupSession.intKey(sd, "dataPort") == 7001, "dataPort copied through")
}

section("SETUP — phase-2 omits streams the box omitted")
do {
    // Box answered only the screen; the phone also asked for an unimplemented type 120.
    let local = bp(fxOmitLocal), req = bp(fxOmitReq)
    let authored = AirPlaySetupSession.authorPhase2(local: local, req: req)
    check(AirPlaySetupSession.dictStreams(authored).count == 1, "type 120 omitted like the box")
    check(AirPlaySetupSession.oracleDiff(authored: authored, local: local).isEmpty, "oracle clean after omit")
}

section("SETUP — phase-2 type-130 RCS DataStream keeps its transport token (wireless)")
do {
    // The type-130 request has NO streamConnectionID, so the scid defaults to 0 on both sides and the
    // match must still find the box's answer. `streamID` is the transport token iOS reads out of the
    // SETUP response; dropping or reshaping this entry is byte-for-byte the same outage as the box's
    // own skip guard was (iAP2 tunnel stuck at Init, no NowPlaying, A/V unaffected). Load-bearing
    // since the 2026-08-10 wireless flip put this authoring path on the wireless SETUP.
    let local = bp(fxDsLocal), req = bp(fxDsReq)
    check(AirPlaySetupSession.parseDict(req)?["streams"] != nil, "130 request parses")
    let authored = AirPlaySetupSession.authorPhase2(local: local, req: req)
    let streams = AirPlaySetupSession.dictStreams(authored)
    check(streams.count == 1, "the RCS DataStream is not dropped by host authoring")
    let sd = streams.first as? [String: Any] ?? [:]
    check(AirPlaySetupSession.intKey(sd, "type") == 130, "type 130")
    check(AirPlaySetupSession.intKey(sd, "streamID") == 1, "transport token echoed verbatim")
    check(AirPlaySetupSession.intKey(sd, "dataPort") == 51305, "box dataPort copied through")
    check(AirPlaySetupSession.oracleDiff(authored: authored, local: local).isEmpty,
          "the 130 echo reads as a MATCH, not a divergence")
}

section("SETUP — phase split + state transitions")
do {
    let cfg = VehicleConfig(appDrivenSetup: true)
    let drv = AirPlaySetupSession(config: cfg, mode: .author)
    check(drv.state == .idle, "starts Idle")

    let p1req = AirPlaySetupSession.toBinary(["keepAliveLowPower": true])
    check(!AirPlaySetupSession.requestHasStreams(p1req), "phase-1 request has no streams")
    _ = drv.onSetup(local: bp(fxP1Hevc), req: p1req)
    check(drv.state == .phase1Done, "phase-1 → Phase1Done")

    let p2req: [String: Any] = ["streams": [["type": 110, "streamConnectionID": 3]]]
    let p2 = AirPlaySetupSession.toBinary(p2req)
    check(AirPlaySetupSession.requestHasStreams(p2), "phase-2 request has streams")
    _ = drv.onSetup(local: bp(fxScrLocal), req: p2)
    check(drv.state == .streamsActive, "phase-2 → StreamsActive")

    drv.onTeardown(req: p2)                       // partial (streams present) keeps the session
    check(drv.state == .streamsActive, "partial teardown keeps the session")
    drv.onTeardown(req: AirPlaySetupSession.toBinary([:]))  // full teardown resets
    check(drv.state == .idle, "full teardown resets to Idle")
}

section("SETUP — deliberate divergence: config flips HEVC off")
do {
    // The box (older/other config) still echoed hevc; the host authors the empty set → oracle diverges.
    let cfgOff = VehicleConfig() // hevc off
    check(cfgOff.enabledFeatures().isEmpty, "hevc-off config authors no features")
    let boxLocal = bp(fxP1Hevc)  // box local still carries enabledFeatures:["hevc"]
    let authored = AirPlaySetupSession.authorPhase1(local: boxLocal, config: cfgOff)
    let diff = AirPlaySetupSession.oracleDiff(authored: authored, local: boxLocal)
    check(diff.contains { $0.contains("enabledFeatures") },
          "flipping hevc off diverges on enabledFeatures, got \(diff)")
}

section("SETUP — RECORD authors an empty body")
do {
    let drv = AirPlaySetupSession(config: VehicleConfig(), mode: .author)
    check(drv.onRecord(local: []).isEmpty, "RECORD → bodyless 200")
}

section("SETUP — echo mode returns the box local verbatim (P1 Stage-0)")
do {
    let drv = AirPlaySetupSession(config: VehicleConfig(), mode: .echo)
    let local = bp(fxScrLocal)
    check(drv.onSetup(local: local, req: bp(fxScrReq)) == local, "echo mode returns local unchanged")
}

// ════════════════════════════════════════════════════════════════════════════
// App-driven SETUP — OCBMControlRelay seam codec + dispatch (port of relay.rs codec/tests)
// ════════════════════════════════════════════════════════════════════════════

func rhdr(_ op: UInt8, _ conn: UInt32, _ cseq: UInt32) -> [UInt8] {
    var m: [UInt8] = [op]
    for v in [conn, cseq] {
        m.append(UInt8(v & 0xFF)); m.append(UInt8((v >> 8) & 0xFF))
        m.append(UInt8((v >> 16) & 0xFF)); m.append(UInt8((v >> 24) & 0xFF))
    }
    return m
}
func rMsgOpen(_ conn: UInt32, _ wireless: Bool, _ crc: UInt32, _ ctx: [UInt8]) -> [UInt8] {
    var m = rhdr(OCBM.rsOpen, conn, 0)
    m.append(OCBM.rsVer); m.append(wireless ? OCBM.openFlagWireless : 0)
    for i in 0..<4 { m.append(UInt8((crc >> (8 * UInt32(i))) & 0xFF)) }
    let cl = UInt32(ctx.count)
    for i in 0..<4 { m.append(UInt8((cl >> (8 * UInt32(i))) & 0xFF)) }
    m.append(contentsOf: ctx)
    return m
}
func rMsgReq(_ conn: UInt32, _ cseq: UInt32, _ route: UInt8, _ notify: Bool,
             _ local: [UInt8], _ req: [UInt8]) -> [UInt8] {
    var m = rhdr(OCBM.rsReq, conn, cseq)
    m.append(route); m.append(notify ? OCBM.reqFlagNotify : 0)
    let ll = UInt32(local.count)
    for i in 0..<4 { m.append(UInt8((ll >> (8 * UInt32(i))) & 0xFF)) }
    m.append(contentsOf: local); m.append(contentsOf: req)
    return m
}
func rMsgClose(_ conn: UInt32, _ reason: UInt8) -> [UInt8] {
    var m = rhdr(OCBM.rsClose, conn, 0); m.append(reason); return m
}

section("control relay — frame codec round-trip + header layout")
do {
    var fb = RTSPFrameBuf()
    let m1 = rMsgOpen(7, false, 0xDEADBEEF, [UInt8]("ctx".utf8))
    let m2 = rMsgReq(7, 1, OCBM.routeSetup, false, [UInt8]("local".utf8), [UInt8]("req".utf8))
    fb.push(OCBMControlRelay.frameMsg(m1))
    fb.push(OCBMControlRelay.frameMsg(m2))
    check(fb.next() == m1, "frame 1 round-trips")
    check(fb.next() == m2, "frame 2 round-trips")
    check(fb.next() == nil, "no extra frame")

    let (op, conn, cseq, rest) = OCBMControlRelay.parseHeader(m2)!
    let r = Array(rest)
    check(op == OCBM.rsReq && conn == 7 && cseq == 1, "header op/conn/cseq")
    check(r[0] == OCBM.routeSetup && r[1] == 0, "route + flags")
    let localLen = Int(r[2]) | (Int(r[3]) << 8) | (Int(r[4]) << 16) | (Int(r[5]) << 24)
    check(localLen == 5, "local_len field")
    check(Array(r[6..<11]) == [UInt8]("local".utf8), "local body")
    check(Array(r[11...]) == [UInt8]("req".utf8), "req body")
}

section("control relay — resync on garbage + split-across-pushes")
do {
    var fb = RTSPFrameBuf()
    let m = rMsgClose(3, OCBM.closeReset)
    let framed = OCBMControlRelay.frameMsg(m)
    // Junk (including a stray 0x52 'R' + the first two magic bytes) delivered split mid-magic.
    fb.push([0x00, 0x52, 0xFF, 0x52, 0x54])
    check(fb.next() == nil, "nothing decodable mid-magic")
    fb.push(Array(framed[2...]))
    check(fb.next() == m, "recovers the frame after resync")
    check(fb.next() == nil, "nothing left")
}

section("control relay — oversize length skipped + recovers")
do {
    var fb = RTSPFrameBuf()
    var junk = OCBM.rtspSeamMagic
    let bad = UInt32(OCBM.rtspSeamMax + 1)
    junk.append(contentsOf: [UInt8((bad >> 24) & 0xFF), UInt8((bad >> 16) & 0xFF),
                             UInt8((bad >> 8) & 0xFF), UInt8(bad & 0xFF)])
    junk.append(contentsOf: [UInt8]("leftover".utf8))
    let m = rMsgReq(1, 2, OCBM.routeRecord, false, [], [])
    fb.push(junk)
    fb.push(OCBMControlRelay.frameMsg(m))
    check(fb.next() == m, "good frame recovered after an implausible length")
}

section("control relay — dispatch RS_OPEN → RS_REQ → RS_RESP; stale conn dropped")
do {
    let sentBox = Mutex<[[UInt8]]>([])
    var openConn: UInt32 = 0
    var requests: [(UInt8, [UInt8], [UInt8])] = []  // (route, local, req)
    let relay = OCBMControlRelay(send: { _, framed in sentBox.withLock { $0.append(framed) } })
    relay.onOpen = { conn, _, _, _ in openConn = conn }
    relay.onRequest = { _, _, route, _, local, req in
        requests.append((route, local, req))
        return route == OCBM.routeSetup ? (200, [0xAB, 0xCD]) : nil
    }

    relay.feed(OCBMControlRelay.frameMsg(rMsgOpen(7, false, 0, [])))
    check(openConn == 7 && relay.currentConn == 7, "RS_OPEN sets the current conn")

    // A SETUP request for the WRONG conn is dropped (no handler call, no reply).
    relay.feed(OCBMControlRelay.frameMsg(rMsgReq(99, 1, OCBM.routeSetup, false, [], [])))
    check(requests.isEmpty && sentBox.withLock { $0.isEmpty }, "stale-conn RS_REQ dropped")

    // The real SETUP request is authored and answered with RS_RESP(200, body).
    relay.feed(OCBMControlRelay.frameMsg(rMsgReq(7, 1, OCBM.routeSetup, false,
                                                 [UInt8]("L".utf8), [UInt8]("R".utf8))))
    check(requests.count == 1 && requests[0].0 == OCBM.routeSetup, "SETUP routed to onRequest")
    let sent = sentBox.withLock { $0 }
    check(sent.count == 1, "one RS_RESP emitted")
    var rfb = RTSPFrameBuf(); rfb.push(sent[0])
    let respMsg = rfb.next()!
    let (op, rconn, rcseq, rest) = OCBMControlRelay.parseHeader(respMsg)!
    let rr = Array(rest)
    check(op == OCBM.rsResp && rconn == 7 && rcseq == 1, "RS_RESP header echoes conn/cseq")
    check(UInt16(rr[0]) | (UInt16(rr[1]) << 8) == 200, "RS_RESP status 200")
    check(Array(rr[2...]) == [0xAB, 0xCD], "RS_RESP carries the authored body")

    // A TEARDOWN NOTIFY is delivered but owes no reply; RS_CLOSE clears the current conn.
    relay.feed(OCBMControlRelay.frameMsg(rMsgReq(7, 2, OCBM.routeTeardown, true, [], [])))
    check(requests.count == 2 && sentBox.withLock { $0.count } == 1, "TEARDOWN NOTIFY sends no reply")
    relay.feed(OCBMControlRelay.frameMsg(rMsgClose(7, OCBM.closeEOF)))
    check(relay.currentConn == 0, "RS_CLOSE clears the current conn")
}

section("control relay — declined request emits RS_ERR")
do {
    let sentBox = Mutex<[[UInt8]]>([])
    let relay = OCBMControlRelay(send: { _, framed in sentBox.withLock { $0.append(framed) } })
    relay.onRequest = { _, _, _, _, _, _ in nil }  // decline everything
    relay.feed(OCBMControlRelay.frameMsg(rMsgOpen(1, false, 0, [])))
    relay.feed(OCBMControlRelay.frameMsg(rMsgReq(1, 5, OCBM.routeSetup, false, [], [])))
    var rfb = RTSPFrameBuf(); rfb.push(sentBox.withLock { $0 }[0])
    let (op, _, cseq, _) = OCBMControlRelay.parseHeader(rfb.next()!)!
    check(op == OCBM.rsErr && cseq == 5, "declined SETUP → RS_ERR with the request cseq")
}

// ════════════════════════════════════════════════════════════════════════════
// StreamMetrics — pure rate math (OCBM/StreamMetrics.swift)
// ════════════════════════════════════════════════════════════════════════════
//
// Deterministic: no sockets, no real clock. The accumulator is fed integer samples and snapshotted at
// INJECTED timestamps; the derivation is a pure function of two snapshots + dt.

section("StreamMetrics — video Mbps / fps / frame min-avg-max / loss")
do {
    var acc = StreamMetricsAccumulator()
    let s0 = acc.snapshot(at: 0.0)   // baseline (all zero), rotates the interval bucket

    // 3 main-video frames in the interval: sizes 1000 / 2000 / 3000 B (bytes == size here); one gap.
    acc.record(.mainVideo, bytes: 1000, sizeSample: 1000, gap: false, fail: false, nowNs: 0)
    acc.record(.mainVideo, bytes: 2000, sizeSample: 2000, gap: true,  fail: false, nowNs: 0)
    acc.record(.mainVideo, bytes: 3000, sizeSample: 3000, gap: false, fail: false, nowNs: 0)
    let s1 = acc.snapshot(at: 1.0)   // exactly 1 s later

    let rep = StreamMetricsReport.between(s0, s1)
    let v = rep[.mainVideo]
    check(abs(rep.dt - 1.0) < 1e-9, "dt = 1.0 s from injected timestamps")
    check(abs(v.mbps - 0.048) < 1e-6, "Mbps = 6000 B · 8 / 1e6 over 1 s (got \(v.mbps))")
    check(abs(v.framesPerSec - 3.0) < 1e-9, "fps = 3 frames / 1 s")
    check(abs(v.avgFrameBytes - 2000.0) < 1e-9, "avg frame size = 2000 B")
    check(v.minFrameBytes == 1000 && v.maxFrameBytes == 3000, "frame min/max = 1000/3000 B")
    check(v.gapsDelta == 1 && abs(v.lossPerSec - 1.0) < 1e-9, "1 gap → loss 1/s")
    check(v.decryptFailDelta == 0, "no decrypt fails")
    // Streams with no traffic report zero.
    check(rep[.altVideo].mbps == 0 && rep[.mediaAudio].framesPerSec == 0, "idle streams read zero")
}

section("StreamMetrics — interval bucket rotates (min/max are per-interval, not cumulative)")
do {
    var acc = StreamMetricsAccumulator()
    let a = acc.snapshot(at: 0.0)
    acc.record(.mainVideo, bytes: 5000, sizeSample: 5000, gap: false, fail: false, nowNs: 0)
    let b = acc.snapshot(at: 1.0)                 // interval #1: only the 5000 B frame
    acc.record(.mainVideo, bytes: 500, sizeSample: 500, gap: false, fail: false, nowNs: 0)
    let c = acc.snapshot(at: 2.0)                 // interval #2: only the 500 B frame
    check(StreamMetricsReport.between(a, b)[.mainVideo].maxFrameBytes == 5000, "interval #1 max = 5000")
    let r2 = StreamMetricsReport.between(b, c)[.mainVideo]
    check(r2.minFrameBytes == 500 && r2.maxFrameBytes == 500, "interval #2 min/max = 500 (bucket rotated, not 5000)")
    check(abs(r2.avgFrameBytes - 500.0) < 1e-9, "interval #2 avg = 500 from cumulative diff")
}

section("StreamMetrics — audio packets/sec + codec label passthrough")
do {
    var acc = StreamMetricsAccumulator()
    let s0 = acc.snapshot(at: 10.0)
    let fmt = OCBMAudioStreamFormat(codec: 1, sampleRate: 48000, channels: 2, bits: 0, audioType: 0) // AAC-LC media
    acc.setFormat(.mediaAudio, fmt)
    for _ in 0..<94 { acc.record(.mediaAudio, bytes: 400, sizeSample: 400, gap: false, fail: false, nowNs: 0) }
    let s1 = acc.snapshot(at: 11.0)              // 1 s later → 94 packets
    let a = StreamMetricsReport.between(s0, s1)[.mediaAudio]
    check(abs(a.framesPerSec - 94.0) < 1e-9, "audio packets/sec = 94")
    check(abs(a.mbps - (94.0 * 400 * 8 / 1_000_000)) < 1e-6, "audio Mbps from packet bytes")
    check(a.format == fmt, "SEAM_FORMAT latched onto the stream snapshot")
    check(a.jitterMs == 0, "audio jitter stays 0 (video-only)")
}

section("StreamMetrics — video jitter from injected inter-arrival timestamps")
do {
    var acc = StreamMetricsAccumulator()
    _ = acc.snapshot(at: 0.0)
    // Constant 1 ms cadence → jitter converges to 0.
    for i in 1...4 { acc.record(.mainVideo, bytes: 100, sizeSample: 100, gap: false, fail: false,
                                nowNs: UInt64(i) * 1_000_000) }
    let steady = acc.snapshot(at: 1.0)
    check(steady.main.jitterMs == 0, "constant cadence → 0 jitter")
    // A late frame (5 ms gap after the 4 ms mark) injects inter-arrival variation → jitter > 0.
    acc.record(.mainVideo, bytes: 100, sizeSample: 100, gap: false, fail: false, nowNs: 9_000_000)
    let jittery = acc.snapshot(at: 2.0)
    check(jittery.main.jitterMs > 0, "irregular arrival → jitter > 0 (got \(jittery.main.jitterMs) ms)")
}

section("StreamMetrics — non-positive dt yields zero rates, not a divide-by-zero")
do {
    var acc = StreamMetricsAccumulator()
    let s0 = acc.snapshot(at: 5.0)
    acc.record(.mainVideo, bytes: 9999, sizeSample: 9999, gap: true, fail: true, nowNs: 0)
    let s1 = acc.snapshot(at: 5.0)               // same instant → dt == 0
    let v = StreamMetricsReport.between(s0, s1)[.mainVideo]
    check(v.mbps == 0 && v.framesPerSec == 0 && v.lossPerSec == 0, "dt=0 → all rates zero")
}

// ════════════════════════════════════════════════════════════════════════════
// Anomaly events — audio DSP detection + video activity/freeze (pure layer)
// ════════════════════════════════════════════════════════════════════════════
//
// Fed synthetic per-buffer DSP numbers / frame arrivals at INJECTED timestamps; asserts the right
// events fire, that debounce collapses a sustained condition to one, and — crucially — that an IDLE
// stream never freezes even across long inter-frame gaps while an ACTIVE stall does.

func adsp(rms: Float, clip: Float = 0, maxDelta: Float = 0, first: Float = 0, last: Float = 0,
          voice: Bool = false, frames: Int = 480, rate: Double = 48000) -> AudioBufferDSP {
    AudioBufferDSP(frames: frames, channels: 1, sampleRate: rate, rms: rms, peak: max(rms, clip),
                   clipFraction: clip, firstSample: first, lastSample: last, maxAbsDelta: maxDelta, voice: voice)
}

section("Anomaly — audio SILENCE after >120 ms of active near-zero RMS")
do {
    let log = AVAnomalyLog()
    // 10 ms buffers (480 frames @ 48k) at near-zero RMS while the stream is ACTIVE.
    var t = 0.0
    while t <= 140 { log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.0005), tMonoMs: t, tWall: 0); t += 10 }
    check(log.counts()["silence"] == 1, "one silence event once the run crosses 120 ms (got \(log.counts()["silence"] ?? 0))")
    check(log.events().contains { $0.kind == .silence && $0.stream == StreamKind.mediaAudio.label },
          "silence event is tagged to the media stream")
}

section("Anomaly — audio SILENCE run RESETS across an idle gap (04-M1 fix, real idleGapMs path)")
do {
    // The `active:` flag is gone (04-M1: it was always `true` in production, so the gate was dead).
    // The real idle protection is the cross-idle `idleGapMs` reset in recordAudioBuffer itself: if
    // the stream sat with no buffers for > idleGapMs (200 ms), the NEXT buffer starts a fresh
    // silence run rather than continuing to accumulate duration from before the gap.
    let log = AVAnomalyLog()
    var t = 0.0
    // Silent buffers accumulating toward — but not reaching — the 120 ms silenceMinMs floor.
    while t <= 50 { log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.0005), tMonoMs: t, tWall: 0); t += 10 }
    check((log.counts()["silence"] ?? 0) == 0, "not yet at the 120 ms floor before the gap")
    // Jump the clock past idleGapMs (200 ms) — simulates the stream going idle (no buffers at all)
    // then resuming, e.g. a pause. Without the reset, this buffer's `dur` would be measured from the
    // PRE-GAP silenceStartMs (t=0) and immediately exceed 120 ms, firing on the very first post-gap
    // buffer — the bug this test pins.
    t = 300
    while t <= 340 { log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.0005), tMonoMs: t, tWall: 0); t += 10 }
    check((log.counts()["silence"] ?? 0) == 0,
          "silence run restarted after the gap — only ~50 ms of post-gap silence, below the 120 ms floor")
    // Detection still works post-reset: keep going until the NEW run itself crosses 120 ms.
    while t <= 420 { log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.0005), tMonoMs: t, tWall: 0); t += 10 }
    check(log.counts()["silence"] == 1, "silence still fires once the POST-GAP run itself crosses 120 ms")
}

section("Anomaly — audio CLICK on a discontinuity above the noise floor")
do {
    let log = AVAnomalyLog()
    log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.2, last: 0.1), tMonoMs: 0, tWall: 0)
    // Big intra-buffer jump (Δ0.8 fs) with real signal present → click.
    log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.2, maxDelta: 0.8, first: 0.1), tMonoMs: 10, tWall: 0)
    check(log.counts()["click"] == 1, "one click event from the discontinuity")
    // A same-size jump during near-silence is an onset transient, not a click.
    let log2 = AVAnomalyLog()
    log2.recordAudioBuffer(.mediaAudio, adsp(rms: 0.001, maxDelta: 0.9), tMonoMs: 0, tWall: 0)
    check((log2.counts()["click"] ?? 0) == 0, "no click when the buffer is near-silent (transient, not a click)")
}

section("Anomaly — audio CLIP on sustained full-scale content")
do {
    let log = AVAnomalyLog()
    log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.7, clip: 0.10), tMonoMs: 0, tWall: 0)
    check(log.counts()["clip"] == 1, "one clip event when ≥2% of samples are at full scale")
}

section("Anomaly — DEBOUNCE collapses a sustained condition to one event")
do {
    let log = AVAnomalyLog()
    // 5 clipping buffers within the clip cooldown (600 ms) → exactly ONE event.
    for i in 0..<5 { log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.7, clip: 0.2),
                                           tMonoMs: Double(i) * 50, tWall: 0) }
    check(log.counts()["clip"] == 1, "sustained clip debounced to a single event, not 5")
}

section("Anomaly — VIDEO freeze fires for an ACTIVE stall but NOT for an idle stream")
do {
    // IDLE: small frames at 2 fps (500 ms gaps). Long gaps are NORMAL — no freeze, state stays idle.
    let idle = AVAnomalyLog()
    var t = 0.0
    for _ in 0..<10 { idle.recordVideoFrame(.mainVideo, bytes: 500, tMonoMs: t, tWall: 0); t += 500 }
    idle.recordVideoFrame(.mainVideo, bytes: 500, tMonoMs: t + 3000, tWall: 0) // a 3 s gap while idle
    idle.pollVideo(tMonoMs: t + 6000, tWall: 0)
    check(idle.videoState(.mainVideo) == .idle, "low-steady-rate small-frame stream classifies as idle")
    check((idle.counts()["freeze"] ?? 0) == 0, "idle stream never freezes, even across long gaps")

    // ACTIVE: 60 fps large frames → active; then a 2 s gap before the next frame → freeze on resume.
    let active = AVAnomalyLog()
    var u = 0.0
    for _ in 0..<10 { active.recordVideoFrame(.mainVideo, bytes: 20000, tMonoMs: u, tWall: 0); u += 16.7 }
    check(active.videoState(.mainVideo) == .active, "60 fps large-frame stream classifies as active")
    active.recordVideoFrame(.mainVideo, bytes: 20000, tMonoMs: u + 2000, tWall: 0)
    check((active.counts()["freeze"] ?? 0) >= 1, "an active stream that stalls mid-motion freezes")

    // ACTIVE stall detected by the poll (no resuming frame arrives).
    let stall = AVAnomalyLog()
    var w = 0.0
    for _ in 0..<10 { stall.recordVideoFrame(.mainVideo, bytes: 20000, tMonoMs: w, tWall: 0); w += 16.7 }
    stall.pollVideo(tMonoMs: w + 1000, tWall: 0)
    check((stall.counts()["freeze"] ?? 0) >= 1, "poll catches an active stream that stopped delivering frames")
}

section("Anomaly — event ring is bounded + reset clears it")
do {
    let log = AVAnomalyLog()
    // Force more than `capacity` DISTINCT events past the debounce (space them out, alternate kinds).
    for i in 0..<60 {
        log.record(i % 2 == 0 ? .gap : .underrun, stream: .mainVideo, detail: "#\(i)",
                   tMonoMs: Double(i) * 1000, tWall: 0)
    }
    check(log.events().count <= AVAnomalyLog.capacity, "ring never exceeds capacity (\(AVAnomalyLog.capacity))")
    check(log.events().last?.detail == "#59", "newest event retained at the tail")
    log.reset()
    check(log.events().isEmpty && log.counts().isEmpty, "reset clears the ring + counters")
}

section("CRC32 — must match ocbm-proto byte for byte (docs/carplay/04_CAPABILITIES_AND_CONFIG.md #6)")
do {
    // The canonical CRC-32 check value. If this drifts, the app would report config drift on EVERY
    // connection — a false alarm is worse than the silence it replaces, because it trains the owner
    // to ignore the warning.
    check(CRC32.compute(Array("123456789".utf8)) == 0xCBF4_3926, "\"123456789\" -> 0xCBF43926")
    check(CRC32.compute([UInt8]()) == 0, "empty input -> 0 (the box's \"no config\" sentinel)")
    check(CRC32.compute(Array("a".utf8)) == 0xE8B7_BE43, "single byte \"a\" -> 0xE8B7BE43")

    // Order-sensitivity: a CRC that ignored byte order would still pass the vectors above by luck of
    // a symmetric input, so pin an asymmetric pair.
    check(CRC32.compute(Array("ab".utf8)) != CRC32.compute(Array("ba".utf8)), "byte order matters")

    // A representative pushed document must produce a stable, non-zero CRC — zero is the box's
    // sentinel for "no YAML loaded", so a document CRCing to 0 would be misread as an absent config.
    // NOTE the literal: the live emitter lives in SettingsWindow.swift, which run_tests.sh does not
    // compile (that is exactly what task #27 is for), so this cannot use VehicleConfigModel.
    let doc = Array("name: \"CarLink Widescreen\"\nversion: 1\nwireless: true\n".utf8)
    check(CRC32.compute(doc) != 0, "a real config does not collide with the no-config sentinel")
    check(CRC32.compute(doc) == CRC32.compute(doc), "deterministic")
}

// MARK: - Annex-B -> AVCC (Android Auto video shim, docs/androidauto/01_SESSION_AND_AV.md)

section("Annex-B -> AVCC (Android Auto video shim)")
do {
    let sps: [UInt8] = [0x67, 0x42, 0x00, 0x1e, 0xAA] // nal_unit_type 7
    let pps: [UInt8] = [0x68, 0xCE, 0x3C, 0x80]       // type 8
    let idr: [UInt8] = [0x65, 0x88, 0x84, 0x00, 0x11] // type 5 (ends in a non-zero, plus an interior 0x00)
    func sc4(_ nal: [UInt8]) -> [UInt8] { [0, 0, 0, 1] + nal }
    func sc3(_ nal: [UInt8]) -> [UInt8] { [0, 0, 1] + nal }

    // Parameter-set extraction from an Annex-B config blob (mixed 4- and 3-byte start codes).
    let config = Data(sc4(sps) + sc3(pps))
    if let (s, p) = AVCCFastPath.h264ParameterSetsFromAnnexB(config) {
        check(Array(s) == sps, "SPS extracted from Annex-B config (start code stripped)")
        check(Array(p) == pps, "PPS extracted from Annex-B config")
    } else {
        check(false, "h264ParameterSetsFromAnnexB found SPS+PPS")
    }

    // Single-NAL frame conversion: Annex-B IDR -> AVCC with a 4-byte BE length prefix.
    if let out = AVCCFastPath.annexBToAVCC(Data(sc4(idr))) {
        check(out.count == 4 + idr.count, "AVCC frame = 4-byte prefix + NAL")
        check(Array(out.prefix(4)) == [0, 0, 0, UInt8(idr.count)], "4-byte BE length prefix correct")
        check(Array(out.suffix(idr.count)) == idr, "NAL payload preserved")
        let w = walk(Array(out), isHEVC: false)
        check(w.valid && w.containsIDR, "converted AVCC walks valid + is a keyframe")
    } else {
        check(false, "annexBToAVCC converted the IDR frame")
    }

    // Multi-NAL access unit (SPS+PPS+IDR) -> valid AVCC that walkAVCC accepts.
    if let out = AVCCFastPath.annexBToAVCC(Data(sc4(sps) + sc4(pps) + sc4(idr))) {
        let w = walk(Array(out), isHEVC: false)
        check(w.valid && w.hasParamSets && w.containsIDR, "multi-NAL Annex-B AU -> valid AVCC (param sets + IDR)")
    } else {
        check(false, "annexBToAVCC handled a multi-NAL access unit")
    }

    // Real captured Android Auto stream from the aa-headunit client, if present.
    if let real = try? Data(contentsOf: URL(fileURLWithPath: "/tmp/aa_tap.h264")) {
        check(AVCCFastPath.h264ParameterSetsFromAnnexB(real) != nil, "real AA capture: SPS+PPS found")
        if let out = AVCCFastPath.annexBToAVCC(real) {
            let w = walk(Array(out), isHEVC: false)
            check(w.valid, "real AA capture -> structurally-valid AVCC")
            check(w.hasParamSets && w.containsIDR, "real AA capture AVCC has SPS/PPS + IDR")
        } else {
            check(false, "real AA capture converted to AVCC")
        }
    } else {
        print("  (skip real-capture check: /tmp/aa_tap.h264 not present)")
    }
}

// MARK: - AAWire (Android Auto engine wire layer, docs/androidauto/01_SESSION_AND_AV.md)

section("AAWire — protobuf builders match the Rust reference byte-for-byte")
do {
    func hex(_ d: Data) -> String { d.map { String(format: "%02x", $0) }.joined() }

    // Known-good byte strings from the validated Rust proto.rs.
    check(hex(AAWire.authResponseSuccess()) == "0800", "AuthResponse{status=0} = 08 00")
    check(hex(AAWire.channelOpenResponseOK()) == "0800", "ChannelOpenResponse{status=0} = 08 00")
    check(hex(AAWire.mediaConfigReady()) == "080210401800",
          "Config{READY,max_unacked=64,idx=[0]} = 08 02 10 40 18 00")
    check(hex(AAWire.videoFocusProjected()) == "08011000", "VideoFocusNotification{PROJECTED,unsolicited=0} = 08 01 10 00")
    check(hex(AAWire.navFocusProjected()) == "0802", "NavFocusNotification{PROJECTED=2} = 08 02")
    check(hex(AAWire.sensorBatchDriving(.none)) == "6a020800", "SensorBatch driving-unrestricted = 6a 02 08 00")
    check(hex(AAWire.sensorBatchNight(false)) == "52020800", "SensorBatch night(false) = 52 02 08 00")
    check(hex(AAWire.byebyeRequest()) == "0801", "ByeByeRequest{USER_SELECTION=1} = 08 01")
    check(hex(AAWire.pingResponse(0x1234)) == "08b424", "PingResponse{ts=0x1234} varint = 08 b4 24")
    check(hex(AAWire.mediaAckBody(7)) == "08071001", "Ack{session_id=7,ack=1} = 08 07 10 01")
    check(hex(AAWire.audioFocusNotification(AAWire.audioFocusStateGain)) == "08011000", "AudioFocusNotification{GAIN} = 08 01 10 00")

    // Varint round-trips (0, >127, >=0x8000, big).
    for v: UInt64 in [0, 1, 127, 128, 300, 0x8000, 0xFFFF, 1 << 40] {
        var d = Data(); AAWire.putVarint(&d, v)
        if let (rv, off) = AAWire.getVarint(d, 0) { check(rv == v && off == d.count, "varint round-trip \(v)") }
        else { check(false, "varint decode \(v)") }
    }

    // getFieldVarint pulls field 1 from a Start-like message.
    var start = Data(); AAWire.putVarintField(&start, 1, 42); AAWire.putVarintField(&start, 2, 99)
    check(AAWire.getFieldVarint(start, 1) == 42, "getFieldVarint field 1 = 42")
    check(AAWire.getFieldVarint(start, 2) == 99, "getFieldVarint field 2 = 99")
    check(AAWire.getFieldVarint(start, 3) == nil, "getFieldVarint missing field = nil")

    // Frame encode: version request = ch0, flags 0x03, len 6, [msgid=1][major=1][minor=7].
    var vr = Data()
    vr.append(0); vr.append(1) // msgid 1 BE
    vr.append(0); vr.append(1) // major 1
    vr.append(0); vr.append(7) // minor 7
    let frame = AAWire.encodeFrame(channel: AAWire.chControl, encrypted: false, control: false, payload: vr)
    check(hex(frame) == "0003" + "0006" + "000100010007", "version-request frame = 00 03 00 06 00 01 00 01 00 07")

    // Encrypted + control flag byte = 0x0f; encrypted specific = 0x0b.
    let encCtrl = AAWire.encodeFrame(channel: 3, encrypted: true, control: true, payload: Data([0x00]))
    check(encCtrl[encCtrl.startIndex + 1] == 0x0f, "encrypted+control flag byte = 0x0f")
    let encSpec = AAWire.encodeFrame(channel: 3, encrypted: true, control: false, payload: Data([0x00]))
    check(encSpec[encSpec.startIndex + 1] == 0x0b, "encrypted+specific flag byte = 0x0b")

    // splitMessageId
    let (mid, body) = AAWire.splitMessageId(Data([0x00, 0x05, 0xAA, 0xBB]))
    check(mid == 5 && Array(body) == [0xAA, 0xBB], "splitMessageId -> (5, [AA,BB])")
    check(AAWire.splitMessageId(Data([0x00])).0 == 0, "splitMessageId short -> 0")

    // Full SD response: parseable, has the 7 channels (field 1) + headunit_info (17).
    let sd = AAWire.serviceDiscoveryResponseFull(resolution: 1, fps: 2, density: 160, tsW: 800, tsH: 480)
    var off = 0, channels = 0, hasHUI = false
    while off < sd.count {
        guard let (tag, o1) = AAWire.getVarint(sd, off) else { break }
        off = o1; let f = UInt32(tag >> 3); let wt = tag & 7
        if wt == 2 {
            guard let (len, o2) = AAWire.getVarint(sd, o1) else { break }
            off = o2 + Int(len)
            if f == 1 { channels += 1 }; if f == 17 { hasHUI = true }
        } else if wt == 0 {
            guard let (_, o2) = AAWire.getVarint(sd, o1) else { break }; off = o2
        } else { break }
    }
    check(channels == 7, "SD response advertises 7 service channels")
    check(hasHUI, "SD response carries headunit_info (field 17)")
}

// ════════════════════════════════════════════════════════════════════════════
// OCBMAVDecrypt — ChaCha20-Poly1305 known-answer + seam-parser tests
// (verify_06_config_tests.md top-5 test #1 — the only compiled-but-untested OCBM file)
// ════════════════════════════════════════════════════════════════════════════
//
// Known-answer vectors below were derived OUTSIDE this harness with a throwaway `xcrun swift`
// script (CryptoKit's `ChaChaPoly.seal`, NOT `OCBMAVDecrypt` — a self-seal/self-open round-trip
// would only prove internal consistency, not correctness against an independent sealer). Script:
//
//   import CryptoKit; import Foundation
//   let key = SymmetricKey(data: Data(<32 bytes>)); let counter: UInt64 = <n>
//   var nonce = [UInt8](repeating: 0, count: 12)
//   withUnsafeBytes(of: counter.littleEndian) { for i in 0..<8 { nonce[4+i] = $0[i] } }
//   let sealed = try! ChaChaPoly.seal(plaintext, using: key, nonce: try! .init(data: nonce),
//                                     authenticating: aad)
//   // print key/aad/plaintext/ciphertext+tag as hex, hardcode below.
//
// This exercises OCBMAVDecrypt.decryptVideoFrame/decryptAudio (the "static internal" crypto
// helpers) directly against ciphertext this harness never produced, plus AAD-tamper and short-body
// guards; a second test drives the SEAM_MAGIC/length-prefix parser end-to-end via feedVideo.

section("OCBMAVDecrypt — ChaCha20-Poly1305 known-answer (external CryptoKit vectors)")
do {
    let vKey = SymmetricKey(data: Data(hb("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")))
    let vCounter: UInt64 = 0x1122_3344
    let vAAD: [UInt8] = hb("030a11181f262d343b424950575e656c737a81888f969da4abb2b9c0c7ced5dce3eaf1f8ff060d141b222930373e454c535a61686f767d848b9299a0a7aeb5bcc3cad1d8dfe6edf4fb020910171e252c333a41484f565d646b727980878e959ca3aab1b8bfc6cdd4dbe2e9f0f7fe050c131a21282f363d444b525960676e757c")
    let vBody: [UInt8] = hb("e9f3e4e1f3363efaf8e91a01716d77b67f1a3c03d425369d422af2abcaba627dc3563ee6fc1ccd641a9dc80b8da6b080cde6ecce33b1ca05b275c196829c7caa5fd6c35d92")
    let vPlain = Data("OCBM known-answer video frame plaintext body ABCDEFGH".utf8)

    if let out = OCBMAVDecrypt.decryptVideoFrame(hdr: vAAD[...], body: vBody[...], key: vKey, counter: vCounter) {
        check(out == vPlain, "decryptVideoFrame recovers the externally-sealed plaintext exactly")
    } else {
        check(false, "decryptVideoFrame failed to open the known-answer vector")
    }
    var badAAD = vAAD; badAAD[0] ^= 0xFF
    check(OCBMAVDecrypt.decryptVideoFrame(hdr: badAAD[...], body: vBody[...], key: vKey, counter: vCounter) == nil,
          "tampered AAD (128-B header) fails authentication")
    check(OCBMAVDecrypt.decryptVideoFrame(hdr: vAAD[...], body: vBody[...], key: vKey, counter: vCounter + 1) == nil,
          "wrong counter (nonce) fails authentication")
    check(OCBMAVDecrypt.decryptVideoFrame(hdr: vAAD[...], body: [0, 1, 2][...], key: vKey, counter: vCounter) == nil,
          "sub-16B body rejected without crashing (cannot hold a Poly1305 tag)")

    // Audio vector: full RTP-shaped pkt [12B hdr, AAD = pkt[4..12)][ct][tag16][nonce8].
    let aKey = SymmetricKey(data: Data(hb("fffefdfcfbfaf9f8f7f6f5f4f3f2f1f0efeeedecebeae9e8e7e6e5e4e3e2e1e0")))
    let aPkt: [UInt8] = hb("000000000001020310203040fb121d26aa42b325485778d25fcec66d4bacfca9644c395d0342c464d74cc77ad9c2f9056fdccee7b41baabbccddeeff0102")
    let aPlain = Data("audio AU payload 16 bytes+".utf8)

    if let out = OCBMAVDecrypt.decryptAudio(pkt: aPkt, key: aKey) {
        check(out == aPlain, "decryptAudio recovers the externally-sealed plaintext exactly")
    } else {
        check(false, "decryptAudio failed to open the known-answer vector")
    }
    var badPkt = aPkt; badPkt[4] ^= 0xFF // corrupt the AAD (ts||ssrc, pkt[4..12)) region
    check(OCBMAVDecrypt.decryptAudio(pkt: badPkt, key: aKey) == nil, "tampered audio AAD fails authentication")
    check(OCBMAVDecrypt.decryptAudio(pkt: Array(aPkt.prefix(20)), key: aKey) == nil,
          "short audio packet (below 12+16+8 floor) rejected without crashing")
}

section("OCBMAVDecrypt — seam parser round-trip via feedVideo (SEAM_MAGIC length-prefix framing)")
do {
    final class RecordingAVDelegate: OCBMAVDelegate {
        let framesBox = Mutex<[(Data, Bool)]>([])
        func avDidReceiveVideoConfig(_ config: Data) {}
        func avDidReceiveVideoFrame(_ avcc: Data, keyframe: Bool) { framesBox.withLock { $0.append((avcc, keyframe)) } }
        func avDidReceiveAudioFormat(scid: UInt64, format: OCBMAudioStreamFormat) {}
        func avDidReceiveAudio(_ au: Data, scid: UInt64, format: OCBMAudioStreamFormat?) {}
        func avDidReceiveAltVideoConfig(_ config: Data) {}
        func avDidReceiveAltVideoFrame(_ avcc: Data, keyframe: Bool) {}
    }
    let dec = OCBMAVDecrypt()
    let delegate = RecordingAVDelegate()
    dec.delegate = delegate

    // Independently CryptoKit-sealed vector with hdr[4]=0 (opcode: encrypted frame) and hdr[5]=0x10
    // (Apple's sync-sample/keyframe bit) so the seam parser's opcode + keyframe-bit reads are exercised.
    let key: [UInt8] = hb("404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f")
    // hdr[0..4] LE32 must equal the ciphertext+tag body length (session.rs forward_screen2's
    // `mlen == 141 + hdr.bodySize` contract enforced by OCBMAVDecrypt.videoLengthPlausible) —
    // an arbitrary AAD vector fails that plausibility check before decrypt is ever reached.
    let hdr: [UInt8] = hb("410000000010191c1f2225282b2e3134373a3d404346494c4f5255585b5e6164676a6d707376797c7f8285888b8e9194979a9da0a3a6a9acafb2b5b8bbbec1c4c7cacdd0d3d6d9dcdfe2e5e8ebeef1f4f7fafd000306090c0f1215181b1e2124272a2d303336393c3f4245484b4e5154575a5d606366696c6f7275787b7e8184")
    let body: [UInt8] = hb("7acc6e59c8a83b7acd3152d7794b86af9a19d0c48911f9726a06253c3458a64a1834517247bef8efe7207a22b394bf5c223c6664402e827d41b514adfe410fb034")
    let plain = Data("SEAM round-trip AVCC access unit bytes 0123456789".utf8)
    let seq: UInt64 = 7

    let seamMagic: [UInt8] = [0x53, 0x45, 0x41, 0x56] // "SEAV"
    func lenPrefixed(_ magicPlusMsg: [UInt8]) -> [UInt8] {
        let mlen = UInt32(magicPlusMsg.count)
        return [UInt8(mlen >> 24), UInt8((mlen >> 16) & 0xFF), UInt8((mlen >> 8) & 0xFF), UInt8(mlen & 0xFF)] + magicPlusMsg
    }

    var keyMsg: [UInt8] = [0x00]; keyMsg += key; keyMsg += [UInt8](repeating: 0, count: 8) // scid=0
    let keyWire = lenPrefixed(seamMagic + keyMsg)

    var frameMsg: [UInt8] = [0x01]
    let leSeq = seq.littleEndian
    withUnsafeBytes(of: leSeq) { raw in frameMsg += Array(raw) }
    frameMsg += hdr
    frameMsg += body
    let frameWire = lenPrefixed(seamMagic + frameMsg)

    dec.feedVideo(keyWire)
    dec.feedVideo(frameWire)

    check(waitUntil(2.0) { delegate.framesBox.withLock { !$0.isEmpty } }, "seam parser delivered a decrypted video frame")
    if let (avcc, kf) = delegate.framesBox.withLock({ $0 }).first {
        check(avcc == plain, "seam-parsed + decrypted plaintext matches the externally-sealed vector")
        check(kf, "keyframe bit (hdr[5]&0x10) surfaced through the seam parser")
    } else {
        check(false, "no frame delivered by the seam parser")
    }
    check(dec.statsSnapshot().videoOK == 1 && dec.statsSnapshot().videoFail == 0, "video decrypt tally: 1 ok, 0 fail")
}

// ════════════════════════════════════════════════════════════════════════════
// OCBMAVDecrypt — AUDIO seam: SEAM_MAGIC resync, fNewSource reset, legacy framing
// ════════════════════════════════════════════════════════════════════════════
//
// THE BUG THESE PIN (device-proven 2026-09-02). ocbmd forwards each seam read as one OCBM frame and,
// on a re-SETUP, replaces the seam producer WITHOUT draining the old one. The host reassembles the
// channel as ONE continuous byte stream, so it was left holding a partial message when the new
// producer's first bytes arrived: the new SEAM_KEY landed mid-message and the lane never recovered
// (18 bogus "received audio key" lines, an "audio format … 1469658167Hz 232ch", no media audio on 3
// of 4 streams). Two independent recoveries, each tested here: the box now stamps F_NEW_SOURCE on the
// first frame of a new producer (reset before append), and the audio seam now carries SEAM_MAGIC so a
// host can re-align without it.

section("OCBMAVDecrypt — audio seam: magic resync, new-source reset, legacy fallback")
do {
    final class AudioRecorder: OCBMAVDelegate {
        let aus = Mutex<[(Data, UInt64)]>([])
        let formats = Mutex<[(UInt64, OCBMAudioStreamFormat)]>([])
        func avDidReceiveVideoConfig(_ config: Data) {}
        func avDidReceiveVideoFrame(_ avcc: Data, keyframe: Bool) {}
        func avDidReceiveAudioFormat(scid: UInt64, format: OCBMAudioStreamFormat) {
            formats.withLock { $0.append((scid, format)) }
        }
        func avDidReceiveAudio(_ au: Data, scid: UInt64, format: OCBMAudioStreamFormat?) {
            aus.withLock { $0.append((au, scid)) }
        }
        func avDidReceiveAltVideoConfig(_ config: Data) {}
        func avDidReceiveAltVideoFrame(_ avcc: Data, keyframe: Bool) {}
    }

    let seamMagic: [UInt8] = [0x53, 0x45, 0x41, 0x56] // "SEAV"
    // Same externally-sealed audio vector as the known-answer section above.
    let aKeyBytes = hb("fffefdfcfbfaf9f8f7f6f5f4f3f2f1f0efeeedecebeae9e8e7e6e5e4e3e2e1e0")
    let aPkt: [UInt8] = hb("000000000001020310203040fb121d26aa42b325485778d25fcec66d4bacfca9644c395d0342c464d74cc77ad9c2f9056fdccee7b41baabbccddeeff0102")
    let aPlain = Data("audio AU payload 16 bytes+".utf8)
    let scid: UInt64 = 0x2A

    func le64(_ v: UInt64) -> [UInt8] { withUnsafeBytes(of: v.littleEndian) { Array($0) } }
    func le32(_ v: UInt32) -> [UInt8] { withUnsafeBytes(of: v.littleEndian) { Array($0) } }
    /// `[u32 BE len]([SEAV])[marker][…]` — `withMagic: false` is the pre-2026-09-03 box wire.
    func framed(_ body: [UInt8], withMagic: Bool) -> [UInt8] {
        let b = withMagic ? seamMagic + body : body
        let n = UInt32(b.count)
        return [UInt8(n >> 24), UInt8((n >> 16) & 0xFF), UInt8((n >> 8) & 0xFF), UInt8(n & 0xFF)] + b
    }
    let keyMsg: [UInt8] = [0x00] + aKeyBytes + le64(scid)                       // 41 B body
    let fmtMsg: [UInt8] = [0x02] + le64(scid) + [0] + le32(48_000) + [2, 16, 0] // 17 B body (PCM 48k/2/media)
    let pktMsg: [UInt8] = [0x01] + le64(scid) + aPkt

    check(framed(keyMsg, withMagic: true).count == 4 + 45, "SEAM_KEY frames to the pinned len 45")
    check(framed(fmtMsg, withMagic: true).count == 4 + 21, "SEAM_FORMAT frames to the pinned len 21")

    // ── 1. Happy path + magic resync ────────────────────────────────────────────────────────────
    do {
        let dec = OCBMAVDecrypt()
        let rec = AudioRecorder()
        dec.delegate = rec
        dec.feedAudio(framed(keyMsg, withMagic: true)) // latches the magic framing
        // A message with a VALID magic but an impossible length (SEAM_KEY must be 45). Structurally
        // rejected, exactly like a false magic landing inside RTP ciphertext, so the parser must scan
        // forward to the NEXT magic instead of trusting the length and swallowing what follows.
        var torn: [UInt8] = [0x00, 0x00, 0x00, 100]
        torn += seamMagic
        torn += [0x00]
        torn += [UInt8](repeating: 0, count: 20) // filler, deliberately contains no "SEAV"
        dec.feedAudio(torn + framed(fmtMsg, withMagic: true) + framed(pktMsg, withMagic: true))

        check(waitUntil(2.0) { rec.aus.withLock { !$0.isEmpty } },
              "audio seam resynced on SEAM_MAGIC after a torn message and delivered the next AU")
        if let (au, gotScid) = rec.aus.withLock({ $0 }).first {
            check(au == aPlain, "resynced AU decrypts to the externally-sealed plaintext")
            check(gotScid == scid, "AU carries the scid from its own SEAM_PKT")
        }
        if let (fmtScid, fmt) = rec.formats.withLock({ $0 }).first {
            check(fmtScid == scid && fmt.sampleRate == 48_000 && fmt.channels == 2 && fmt.isPCM,
                  "SEAM_FORMAT after the resync parses as PCM 48000 Hz 2ch — not the 1469658167Hz/232ch garbage a desync produced")
        } else {
            check(false, "no SEAM_FORMAT delivered after the resync")
        }
        let s = dec.statsSnapshot()
        check(s.audioOK == 1, "one audio packet decrypted")
        check(s.mediaAudioRxFrames == 2 && s.mediaAudioRxBytes > 0,
              "RAW CH_MEDIA_AUDIO arrival is counted separately from decrypt success (arx=)")
    }

    // ── 2. fNewSource resets the reassembly buffer BEFORE appending ─────────────────────────────
    do {
        let dec = OCBMAVDecrypt()
        let rec = AudioRecorder()
        dec.delegate = rec
        // A previous producer's message that will never be completed: the length prefix promises 75
        // bytes and only 20 arrive. This is the state the re-SETUP desync left the buffer in.
        dec.feedAudio(Array(framed(pktMsg, withMagic: true).prefix(20)))
        // The new producer's first frame. Without the reset its SEAM_KEY is swallowed as the tail of
        // that dead message and every following packet is dropped keyless.
        dec.feedAudio(framed(keyMsg, withMagic: true) + framed(fmtMsg, withMagic: true) + framed(pktMsg, withMagic: true),
                      newSource: true)

        check(waitUntil(2.0) { rec.aus.withLock { !$0.isEmpty } },
              "fNewSource dropped the dead producer's partial message and the new key took effect")
        check(dec.statsSnapshot().audioNoKeyDrops == 0,
              "no packet was dropped keyless after a new-source reset")
    }

    // ── 3. A keyless packet is counted and logged, not silently dropped ─────────────────────────
    do {
        let dec = OCBMAVDecrypt()
        let rec = AudioRecorder()
        dec.delegate = rec
        dec.feedAudio(framed(pktMsg, withMagic: true)) // SEAM_PKT before any SEAM_KEY
        check(waitUntil(2.0) { dec.statsSnapshot().audioNoKeyDrops == 1 },
              "SEAM_PKT with no key for its scid is counted (was a silent `break`)")
        check(rec.aus.withLock { $0.isEmpty }, "and nothing was handed to the player")
    }

    // ── 4. Legacy (pre-magic) box build still parses ────────────────────────────────────────────
    do {
        let dec = OCBMAVDecrypt()
        let rec = AudioRecorder()
        dec.delegate = rec
        dec.feedAudio(framed(keyMsg, withMagic: false) + framed(fmtMsg, withMagic: false) + framed(pktMsg, withMagic: false))
        check(waitUntil(2.0) { rec.aus.withLock { !$0.isEmpty } },
              "a box that sends no SEAM_MAGIC is detected once and parsed with the legacy framing")
        if let (au, _) = rec.aus.withLock({ $0 }).first {
            check(au == aPlain, "legacy-framed AU decrypts to the same plaintext")
        }
        check(rec.formats.withLock { $0.count } == 1, "legacy SEAM_FORMAT parsed exactly once")
    }

    // ── 5. The voice seam (CH_ALT_AUDIO) is independent and separately counted ──────────────────
    do {
        let dec = OCBMAVDecrypt()
        let rec = AudioRecorder()
        dec.delegate = rec
        dec.feedVoice(framed(keyMsg, withMagic: true) + framed(fmtMsg, withMagic: true) + framed(pktMsg, withMagic: true))
        check(waitUntil(2.0) { rec.aus.withLock { !$0.isEmpty } }, "voice seam parses the same framing")
        let s = dec.statsSnapshot()
        check(s.voiceAudioRxFrames == 1 && s.mediaAudioRxFrames == 0,
              "voice arrival is counted on its own channel, never folded into media")
    }
}

// ════════════════════════════════════════════════════════════════════════════
// OCBMClient — HID/uplink payload byte encodings via FakeTransport (top-5 test #2)
// ════════════════════════════════════════════════════════════════════════════

section("OCBM client — uplink payload byte encodings (touch/knob/nav/appearance/nightMode/telephony/mic)")
do {
    let t = FakeTransport()
    let c = OCBMClient(transport: t)
    c.connect()
    check(waitUntil(3.0) { ctrlCount(t, OCBM.ctHello) >= 1 }, "HELLO sent")
    t.inject(ctrlFrame([OCBM.ctHelloAck]))
    check(waitUntil(3.0) { ctrlCount(t, OCBM.ctSubscribe) >= 1 }, "SUBSCRIBE sent")

    c.sendTouch(phase: 1, nx: 0.5, ny: 0.25, finger: 2)
    // subscribe() proactively fires a CH_INPUT keyframe request too (R2 quick-relaunch grace,
    // OCBMClient.swift ~302-309), racing this send on the SAME channel — wait for the touch marker
    // specifically rather than "any CH_INPUT frame" (which the keyframe request already satisfies).
    check(waitUntil(2.0) { t.writes.contains { f in f.count > OCBM.hdrLen && OCBM.readLE16(f, 8) == OCBM.chInput && f[OCBM.hdrLen] == OCBM.inputTouch } },
          "touch frame emitted")
    let xi = UInt16((0.5 * 65535).rounded()), yi = UInt16((0.25 * 65535).rounded())
    let touchPayload = t.writes.last(where: { f in f.count > OCBM.hdrLen && OCBM.readLE16(f, 8) == OCBM.chInput && f[OCBM.hdrLen] == OCBM.inputTouch })
        .map { Array($0[OCBM.hdrLen...]) }
    check(touchPayload == [OCBM.inputTouch, 1, UInt8(xi & 0xFF), UInt8(xi >> 8), UInt8(yi & 0xFF), UInt8(yi >> 8), 2],
          "touch payload = [inputTouch][phase][nx u16 LE][ny u16 LE][finger]")

    let knobOut = Mutex<SendOutcome?>(nil)
    c.sendKnob(flags: 0x05, nudgeX: -3, nudgeY: 4, rotation: -1) { o in knobOut.withLock { $0 = o } }
    check(waitUntil(2.0) { knobOut.withLock { $0 } != nil }, "sendKnob completes")
    check(lastPayload(t, channel: OCBM.chInput) == [OCBM.inputKnob, 0x05, UInt8(bitPattern: Int8(-3)), UInt8(bitPattern: Int8(4)), UInt8(bitPattern: Int8(-1))],
          "knob payload = [inputKnob][flags][nudgeX i8][nudgeY i8][rotation i8]")

    let navOut = Mutex<SendOutcome?>(nil)
    c.sendNav(0x02) { o in navOut.withLock { $0 = o } }
    check(waitUntil(2.0) { navOut.withLock { $0 } != nil }, "sendNav completes")
    check(lastPayload(t, channel: OCBM.chInput) == [OCBM.inputNav, 0x02], "nav payload = [inputNav][nav]")

    let apOut = Mutex<SendOutcome?>(nil)
    c.sendAppearance(stream: OCBM.appearanceStreamMain, dark: true, isMap: false) { o in apOut.withLock { $0 = o } }
    check(waitUntil(2.0) { apOut.withLock { $0 } != nil }, "sendAppearance completes")
    check(lastPayload(t, channel: OCBM.chInput) == [OCBM.inputCommand, OCBM.cmdUIAppearance, OCBM.appearanceStreamMain, OCBM.appearanceModeDark],
          "appearance payload = [inputCommand][cmdUIAppearance][stream][modeDark]")

    let nmOut = Mutex<SendOutcome?>(nil)
    c.sendNightMode(true) { o in nmOut.withLock { $0 = o } }
    check(waitUntil(2.0) { nmOut.withLock { $0 } != nil }, "sendNightMode completes")
    check(lastPayload(t, channel: OCBM.chInput) == [OCBM.inputCommand, OCBM.cmdNightMode, 1],
          "nightMode payload = [inputCommand][cmdNightMode][1]")

    let telOut = Mutex<SendOutcome?>(nil)
    c.sendTelephony(9) { o in telOut.withLock { $0 = o } }
    check(waitUntil(2.0) { telOut.withLock { $0 } != nil }, "sendTelephony completes")
    check(lastPayload(t, channel: OCBM.chInput) == [OCBM.inputTelephony, 9], "telephony payload = [inputTelephony][index]")

    // Mic uplink rides CH_MIC (not CH_INPUT) as raw PCM bytes, no wrapper.
    c.sendMicPCM(Data([0xDE, 0xAD, 0xBE, 0xEF]))
    check(waitUntil(2.0) { lastPayload(t, channel: OCBM.chMic) != nil }, "mic PCM frame emitted on CH_MIC")
    check(lastPayload(t, channel: OCBM.chMic) == [0xDE, 0xAD, 0xBE, 0xEF], "mic PCM payload is raw bytes, no wrapper")

    c.disconnect()
}

// ════════════════════════════════════════════════════════════════════════════
// StreamMetrics gaps (top-5 test #3): idleGapMs cross-idle reset, fps_drop re-fire,
// AVAnomalyLog.delta(sinceTotal:) boundary reads
// ════════════════════════════════════════════════════════════════════════════

section("Anomaly — audio SILENCE resets across an idle gap (idleGapMs cross-idle reset)")
do {
    let log = AVAnomalyLog()
    var t = 0.0
    while t <= 140 { log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.0005), tMonoMs: t, tWall: 0); t += 10 }
    check(log.counts()["silence"] == 1, "first active run: one silence event")
    // Idle gap > AudioDSP.idleGapMs (200 ms) so the detector resets, AND > AVAnomalyLog's 1000 ms
    // per-kind cooldown so the second event is not debounced away — the reset is what's under test.
    t += 1100
    log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.0005), tMonoMs: t, tWall: 0)
    check(log.counts()["silence"] == 1, "resume after the gap does not immediately re-fire (timer reset, not carried over)")
    var t2 = t + 10
    while t2 <= t + 140 { log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.0005), tMonoMs: t2, tWall: 0); t2 += 10 }
    check(log.counts()["silence"] == 2, "second active run crosses 120 ms again -> a NEW event (cross-idle reset worked)")
}

section("Anomaly — video FPS_DROP fires once then re-fires after the cooldown")
do {
    let log = AVAnomalyLog()
    var t = 0.0
    for _ in 0..<5 { log.recordVideoFrame(.mainVideo, bytes: 9000, tMonoMs: t, tWall: 0); t += 10 } // ramp to ACTIVE (fast, large frames)
    check(log.videoState(.mainVideo) == .active, "stream reached ACTIVE after the hysteresis streak")
    for _ in 0..<4 { t += 100; log.recordVideoFrame(.mainVideo, bytes: 9000, tMonoMs: t, tWall: 0) } // slow to ~10 fps, still motion-sized
    check(log.counts()["fps_drop"] == 1, "fps_drop fires once as the smoothed rate crosses below threshold")
    for _ in 0..<3 { t += 100; log.recordVideoFrame(.mainVideo, bytes: 9000, tMonoMs: t, tWall: 0) }
    check(log.counts()["fps_drop"] == 1, "debounce collapses the sustained condition to one event")
    for _ in 0..<25 { t += 100; log.recordVideoFrame(.mainVideo, bytes: 9000, tMonoMs: t, tWall: 0) } // 2.5 s more, 100 ms steps (never trips freeze)
    check(log.counts()["fps_drop"] == 2, "fps_drop re-fires once the 2000 ms cooldown window has elapsed")
}

section("StreamMetrics — AVAnomalyLog.delta(sinceTotal:) monotonic + boundary reads")
do {
    let log = AVAnomalyLog()
    let (n0, l0, t0) = log.delta(sinceTotal: 0)
    check(n0 == 0 && l0 == nil && t0 == 0, "delta on an empty log: 0 new, no latest, total 0")
    log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.7, clip: 0.10), tMonoMs: 0, tWall: 0) // 1 clip event
    let (n1, l1, t1) = log.delta(sinceTotal: t0)
    check(n1 == 1 && l1?.kind == .clip && t1 == 1, "delta since the prior read reports exactly the 1 new event")
    let (n2, _, t2) = log.delta(sinceTotal: t1)
    check(n2 == 0 && t2 == t1, "delta since the current total reports 0 new (prior == total, the common poll case)")
    // FINDING (not fixed here — StreamMetrics.swift is outside this agent's file set): `delta`
    // computes `Int(st.total &- prior)`. When `prior > total` the wrapping subtraction produces a
    // UInt64 near 2^64 that does not fit in `Int`, and `Int(...)` TRAPS — contradicting the function's
    // own doc comment ("must not trap a diagnostics path"). Confirmed with a throwaway `xcrun swift`
    // repro: `Int(UInt64(5) &- UInt64(105))` -> "Fatal error: Not enough bits to represent the passed
    // value". Deliberately NOT exercised here: doing so would abort this entire test binary instead of
    // reporting a clean FAIL.
}

// ════════════════════════════════════════════════════════════════════════════
// AACapability — pure functions, zero prior coverage (top-5 test #4)
// ════════════════════════════════════════════════════════════════════════════

section("AACapability — Resolution.nearest / FrameRate.nearest / audioSink(forChannel:)")
do {
    check(AACapability.Resolution.nearest(width: 1920, height: 1080) == .r1920x1080, "exact 1920x1080 match")
    check(AACapability.Resolution.nearest(width: 1280, height: 720) == .r1280x720, "exact 1280x720 match")
    check(AACapability.Resolution.nearest(width: 800, height: 480) == .r800x480, "exact 800x480 match")
    check(AACapability.Resolution.nearest(width: 3840, height: 2160) == .r3840x2160, "exact 3840x2160 (tier 5) matches")
    check(AACapability.Resolution.nearest(width: 5120, height: 2880) == .r3840x2160, "oversize input snaps to the largest landscape tier")
    check(AACapability.Resolution.nearest(width: 2400, height: 960) == .r1280x720, "2400x960: largest tier that fits both axes is 1280x720 (margins are T4's job)")
    check(AACapability.Resolution.nearest(width: 1080, height: 1920) == .p1080x1920, "portrait exact tier 7")
    check(AACapability.Resolution.nearest(width: 900, height: 1600) == .p720x1280, "portrait non-exact snaps within the portrait set")
    check(AACapability.Resolution.forced("p1440") == .p1440x2560 && AACapability.Resolution.forced("2160") == .r3840x2160 && AACapability.Resolution.forced("x") == nil, "AA_FORCE_RES spellings")
    do {
        let a = AACapability.Resolution.tierAndVisible(width: 2400, height: 960)
        check(a.tier == .r2560x1440 && a.w == 2560 && a.h == 1024, "2400x960 -> tier 2560x1440, visible 2560x1024 (height margin 416)")
        let b = AACapability.Resolution.tierAndVisible(width: 1920, height: 720)
        check(b.tier == .r1920x1080 && b.w == 1920 && b.h == 720, "1920x720 -> tier 1920x1080, visible 1920x720 (height margin 360)")
        let c = AACapability.Resolution.tierAndVisible(width: 3840, height: 1600)
        check(c.tier == .r3840x2160 && c.w == 3840 && c.h == 1600, "3840x1600 -> tier 3840x2160, visible 3840x1600")
        let d = AACapability.Resolution.tierAndVisible(width: 1080, height: 1920)
        check(d.tier == .p1080x1920 && d.w == 1080 && d.h == 1920, "exact portrait tier has no margins")
        let e = AACapability.Resolution.tierAndVisible(width: 5000, height: 3000)
        check(e.tier == .r3840x2160 && e.w == 3600 && e.h == 2160, "oversize panel falls back to the largest tier, aspect-fit inside it")
        let f = AACapability.Resolution.tierAndVisible(width: 1280, height: 720)
        check(f.tier == .r1280x720 && f.w == 1280 && f.h == 720, "exact landscape tier")
        let g = AACapability.Resolution.tierAndVisible(width: 1024, height: 600)
        check(g.tier == .r1280x720 && g.w == 1228 && g.h == 720, "1024x600 -> 1280x720 with a width margin (visible 1228x720, even)")
    }
    do {
        let sd = AAWire.serviceDiscoveryResponseFull(resolution: 4, fps: 2, density: 160, widthMargin: 0, heightMargin: 416, tsW: 2560, tsH: 1024)
        // VideoConfiguration field 4 (height_margin) = 416 -> varint a0 03 after tag 0x20
        let bytes = [UInt8](sd)
        var found = false
        for i in 0..<(bytes.count - 2) where bytes[i] == 0x20 && bytes[i+1] == 0xa0 && bytes[i+2] == 0x03 { found = true }
        check(found, "height_margin 416 is encoded on the wire (field 4)")
    }
    check(AACapability.Resolution.nearest(width: 1000, height: 600) == .r800x480, "non-exact, non-fitting input floors to 800x480")
    check(AACapability.Resolution.nearest(width: 1280, height: 480) == .r800x480,
          "asymmetric input (wide fits 1280, tall only fits 480) floors to a mode BOTH dims fit")

    check(AACapability.FrameRate.nearest(60) == .fps60, "60 fps -> fps60")
    check(AACapability.FrameRate.nearest(75) == .fps60, "above 60 fps -> fps60 (never claims a rate beyond the enum)")
    check(AACapability.FrameRate.nearest(59) == .fps30, "just under 60 -> fps30 (only two rates exist)")
    check(AACapability.FrameRate.nearest(1) == .fps30, "low fps -> fps30")

    check(AACapability.audioSink(forChannel: 4)?.label == "media", "channel 4 = media sink")
    check(AACapability.audioSink(forChannel: 5)?.label == "guidance", "channel 5 = guidance sink")
    check(AACapability.audioSink(forChannel: 6)?.label == "system", "channel 6 = system sink")
    check(AACapability.audioSink(forChannel: 9) == nil, "channel 9 (mic source, not a sink) is not in audioSinks")
    let media = AACapability.audioSink(forChannel: 4)!
    check(media.rate == 48000 && media.channels == 2 && !media.voice, "media sink: 48k/stereo, not ducking-routed")
    let guidance = AACapability.audioSink(forChannel: 5)!
    check(guidance.rate == AACapability.voiceSinkRate && guidance.channels == 1 && guidance.voice,
          "guidance sink: voiceSinkRate (48k default since 2026-09-04, AA_VOICE_RATE lever) / mono, ducking-routed")
    check(AACapability.audioSinkTable(telephony: false, voiceRate: 16000)[1].rate == 16000, "reference 16k guidance shape still buildable")
}

// ════════════════════════════════════════════════════════════════════════════
// AVCCFastPath.rewriteToFourByteLengths — HEVC NAL shapes, lenSize 1/2/3 (top-5 test #5)
// ════════════════════════════════════════════════════════════════════════════

section("AVCCFastPath.rewriteToFourByteLengths — HEVC NAL headers, lenSize 1/2/3")
do {
    // HEVC NAL header: byte0 = (nal_unit_type << 1) | layer_id_high_bit; byte1 = (layer_id<<3)|tid+1.
    let vps: [UInt8] = [0x40, 0x01, 0xAA, 0xBB] // type 32 (VPS) — param set
    let sps: [UInt8] = [0x42, 0x01, 0xCC]       // type 33 (SPS) — param set
    let idr: [UInt8] = [0x26, 0x01, 0x11, 0x22, 0x33] // type 19 (IDR_W_RADL) — IRAP/keyframe

    /// Build an access unit with `lenSize`-byte BE length prefixes (1/2/3, not the standard 4).
    func withShortLengths(_ nals: [[UInt8]], lenSize: Int) -> Data {
        var out = [UInt8]()
        for nal in nals {
            let n = nal.count
            for k in stride(from: lenSize - 1, through: 0, by: -1) { out.append(UInt8((n >> (8 * k)) & 0xFF)) }
            out.append(contentsOf: nal)
        }
        return Data(out)
    }

    for lenSize in 1...3 {
        let input = withShortLengths([vps, sps, idr], lenSize: lenSize)
        guard let out = AVCCFastPath.rewriteToFourByteLengths(input, lenSize: lenSize) else {
            check(false, "rewriteToFourByteLengths(lenSize: \(lenSize)) returned nil on a well-formed AU")
            continue
        }
        let w = walk(Array(out), isHEVC: true)
        check(w.valid, "lenSize \(lenSize): rewritten AU walks as structurally valid 4-byte-prefixed AVCC")
        check(w.hasParamSets, "lenSize \(lenSize): VPS/SPS (types 32/33) recognized after rewrite")
        check(w.containsIDR, "lenSize \(lenSize): IRAP (type 19) recognized after rewrite")
        check(out.count == input.count + 3 * (4 - lenSize),
              "lenSize \(lenSize): output size = input + 3×(4-lenSize) widened-prefix bytes")
        check(Array(out.prefix(4)) == [0, 0, 0, UInt8(vps.count)], "lenSize \(lenSize): first prefix rewritten to 4-byte BE")
        check(Array(out.dropFirst(4).prefix(vps.count)) == vps, "lenSize \(lenSize): first NAL payload preserved")
    }

    check(AVCCFastPath.rewriteToFourByteLengths(Data([0x00, 0x00, 0x00, 0x05, 0x11]), lenSize: 4) == nil,
          "truncated final NAL (claims 5B, has 1B) -> nil, not a crash")
    check(AVCCFastPath.rewriteToFourByteLengths(Data([0x01, 0x02]), lenSize: 0) == nil, "lenSize 0 rejected")
    check(AVCCFastPath.rewriteToFourByteLengths(Data([0x01, 0x02]), lenSize: 5) == nil, "lenSize 5 rejected")
}

// ════════════════════════════════════════════════════════════════════════════
// OCBMAVDecrypt.rfc2198Primary — RFC 2198 redundant-audio demux (telephony/Siri streams SETUP with
// supportsRTPPacketRedundancy). The ELD decoder must see only the primary access unit.
// ════════════════════════════════════════════════════════════════════════════

section("RFC 2198 redundant-audio demux")
do {
    // One redundant block (PT 96, TS offset 480, len 3) + primary terminator (PT 96), data: [aa bb cc][dd ee ff 11]
    // hdr0 = F=1|PT=96 → 0xE0; TSoffset(14)=480 → bits: 0x0780 → next 14 bits: 0x01E0 → byte1=0x07, byte2 high 6 bits...
    // Encode directly: byte0 = 0x80|0x60 = 0xE0; byte1 = TSoff>>6 = 480>>6 = 7 → 0x07;
    // byte2 = ((480 & 0x3F) << 2) | (len>>8) = (32<<2)|0 = 0x80; byte3 = len & 0xFF = 0x03.
    let bundle = Data([0xE0, 0x07, 0x80, 0x03, 0x60, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x11])
    let primary = OCBMAVDecrypt.rfc2198Primary(bundle)
    check(primary == Data([0xdd, 0xee, 0xff, 0x11]), "primary = last block after 1 redundant block")

    // Two redundant blocks (len 2 and len 1) then primary.
    let two = Data([0xE0, 0x07, 0x80, 0x02, 0xE0, 0x03, 0xC0, 0x01, 0x60, 0x01, 0x02, 0x03, 0x99, 0x98])
    check(OCBMAVDecrypt.rfc2198Primary(two) == Data([0x99, 0x98]), "primary after 2 redundant blocks")

    // A plain access unit whose first byte has the F bit clear is not a bundle (1-byte header alone).
    check(OCBMAVDecrypt.rfc2198Primary(Data([0x60, 0x01, 0x02])) == nil, "F=0 first byte → not a bundle")
    // F bit set but the length runs past the payload → rejected.
    check(OCBMAVDecrypt.rfc2198Primary(Data([0xE0, 0x07, 0x83, 0xFF, 0x60, 0x01])) == nil, "length past end → nil")
    // Mismatched PT between headers → rejected.
    check(OCBMAVDecrypt.rfc2198Primary(Data([0xE0, 0x07, 0x80, 0x01, 0x61, 0x01, 0x02])) == nil, "PT mismatch → nil")
    // Terminator never found within the bound → rejected.
    check(OCBMAVDecrypt.rfc2198Primary(Data([0xE0, 0x07, 0x80, 0x01, 0xE0, 0x07, 0x80, 0x01, 0xE0, 0x07, 0x80, 0x01, 0xE0, 0x07, 0x80, 0x01])) == nil,
          "more than 3 redundant headers → nil")
}

// ════════════════════════════════════════════════════════════════════════════
// AVCCFastPath codec-config records — the avcC/hvcC extractor+parsers moved out of `VideoLane`
// (OCBM/OCBMAVBridge.swift) so they are reachable from this harness. Anchored on the ALT/cluster
// lane's real shape, measured 2026-09-03 on hardware: hvcC with VPS 24 B / **SPS 63 B** / PPS 7 B,
// nalLen 4 (the main lane's SPS is 48 B, so a length/offset bug that only bites the longer SPS
// would have been invisible in every main-lane session).
// ════════════════════════════════════════════════════════════════════════════

section("AVCCFastPath config records — hvcC with a 63-byte SPS (ALT/cluster lane shape)")
do {
    // HEVC NAL header: byte0 = (type << 1) | layerIdHigh, byte1 = (layerId << 3) | (tid + 1).
    func nal(type: UInt8, count: Int) -> [UInt8] {
        var n: [UInt8] = [type << 1, 0x01]
        // Filler that can never look like a `hvcC`/`avcC` FourCC to the fallback scan.
        while n.count < count { n.append(UInt8(0x10 &+ UInt8(n.count % 0x40))) }
        return n
    }
    let vps = nal(type: 32, count: 24)
    let sps = nal(type: 33, count: 63)   // ← the 63-byte SPS
    let pps = nal(type: 34, count: 7)
    check(vps.count == 24 && sps.count == 63 && pps.count == 7, "fixture NAL sizes are 24/63/7 B")

    /// A well-formed hvcC configuration record carrying one VPS/SPS/PPS array each, lenSizeMinusOne = 3.
    func hvcCRecord(_ sets: [(UInt8, [UInt8])]) -> [UInt8] {
        var r = [UInt8](repeating: 0, count: 22)
        r[0] = 1                    // configurationVersion
        r[21] = 0xF3                // …|lengthSizeMinusOne = 3 → nalLen 4
        r.append(UInt8(sets.count)) // numOfArrays
        for (type, bytes) in sets {
            r.append(0x80 | type)   // array_completeness | NAL_unit_type
            r.append(contentsOf: [0x00, 0x01])                                   // numNalus = 1
            r.append(contentsOf: [UInt8(bytes.count >> 8), UInt8(bytes.count & 0xFF)])
            r.append(contentsOf: bytes)
        }
        return r
    }
    let record = hvcCRecord([(32, vps), (33, sps), (34, pps)])

    /// Wrap a configuration record in an ISO VisualSampleEntry: [size][fourcc][78 fixed][child atoms].
    func sampleEntry(fourcc: String, child: String, _ rec: [UInt8]) -> [UInt8] {
        let childSize = 8 + rec.count
        let total = 86 + childSize
        var b: [UInt8] = [UInt8(total >> 24), UInt8((total >> 16) & 0xFF), UInt8((total >> 8) & 0xFF), UInt8(total & 0xFF)]
        b += Array(fourcc.utf8)
        b += [UInt8](repeating: 0, count: 78)   // VisualSampleEntry fixed fields
        b += [UInt8(childSize >> 24), UInt8((childSize >> 16) & 0xFF), UInt8((childSize >> 8) & 0xFF), UInt8(childSize & 0xFF)]
        b += Array(child.utf8)
        b += rec
        return b
    }

    // 1. BARE record — extractConfigRecord must decline (configurationVersion == 1) so the caller's
    //    `?? raw` hands the record straight to the parsers, exactly as the box's early-return does.
    check(AVCCFastPath.extractConfigRecord(fromSampleEntry: record) == nil,
          "bare hvcC record (b[0] == 1) → extractConfigRecord returns nil for the `?? raw` fallthrough")
    if let p = AVCCFastPath.parseHvcC(record) {
        check(p.vps == vps && p.sps == sps && p.pps == pps, "bare record: VPS/SPS/PPS round-trip byte-for-byte")
        check(p.sps.count == 63, "bare record: the 63-byte SPS is returned at full length (no truncation)")
        check(p.lenSize == 4, "bare record: lenSizeMinusOne 3 → nalLen 4")
    } else {
        check(false, "bare 63-byte-SPS hvcC record parsed")
    }

    // 2. STRUCTURED walk — hvc1/hev1 VisualSampleEntry, child atoms at offset 86.
    for outer in ["hvc1", "hev1"] {
        let entry = sampleEntry(fourcc: outer, child: "hvcC", record)
        guard let inner = AVCCFastPath.extractConfigRecord(fromSampleEntry: entry) else {
            check(false, "\(outer) sample entry unwrapped"); continue
        }
        check(inner == record, "\(outer): unwrapped payload is the hvcC record exactly (offset i+8, size-bounded)")
        guard let p = AVCCFastPath.parseHvcC(inner) else {
            check(false, "\(outer): unwrapped record parses"); continue
        }
        check(p.sps.count == 63 && p.sps == sps, "\(outer): 63-byte SPS survives the unwrap")
        check(p.vps.count == 24 && p.pps.count == 7 && p.lenSize == 4, "\(outer): VPS 24 B / PPS 7 B / nalLen 4")
    }

    // 3. FALLBACK scan — an outer FourCC the structured walk does not model must still resolve, and
    //    must land on the SAME record (i+4 after the FourCC == i+8 after size+FourCC).
    let odd = sampleEntry(fourcc: "zzzz", child: "hvcC", record)
    if let inner = AVCCFastPath.extractConfigRecord(fromSampleEntry: odd) {
        check(Array(inner.prefix(record.count)) == record,
              "unmodelled outer FourCC: fallback FourCC scan returns the record at the same offset")
        check(AVCCFastPath.parseHvcC(inner)?.sps == sps, "fallback path: 63-byte SPS parses identically")
    } else {
        check(false, "unmodelled outer FourCC falls back to the FourCC scan")
    }

    // 4. Trailing sibling atoms after hvcC are ignored (the parsers are length-driven).
    var withTail = sampleEntry(fourcc: "hvc1", child: "hvcC", record)
    let tail: [UInt8] = [0x00, 0x00, 0x00, 0x0C] + Array("pasp".utf8) + [0, 0, 0, 1]
    withTail += tail
    let newTotal = withTail.count
    withTail[0] = UInt8(newTotal >> 24); withTail[1] = UInt8((newTotal >> 16) & 0xFF)
    withTail[2] = UInt8((newTotal >> 8) & 0xFF); withTail[3] = UInt8(newTotal & 0xFF)
    check(AVCCFastPath.parseHvcC(AVCCFastPath.extractConfigRecord(fromSampleEntry: withTail) ?? [])?.sps == sps,
          "sample entry with a trailing `pasp` atom still yields the 63-byte SPS")

    // 5. Malformed records are refused, never truncated-but-accepted.
    check(AVCCFastPath.parseHvcC(Array(record.dropLast(10))) == nil,
          "record truncated inside the 63-byte SPS → nil (no short/garbage SPS handed to the decoder)")
    check(AVCCFastPath.parseHvcC(hvcCRecord([(32, vps), (33, sps)])) == nil, "hvcC missing its PPS array → nil")
    // A VPS/SPS/PPS whose NAL header disagrees with its array's declared type is rejected.
    check(AVCCFastPath.parseHvcC(hvcCRecord([(32, vps), (33, pps), (34, pps)])) == nil,
          "array declares SPS(33) but the NALU header says PPS(34) → nil")
    // The H.264 parser must not claim an hvcC.
    check(AVCCFastPath.parseAvcC(record) == nil, "parseAvcC declines an hvcC record (no false H.264 match)")
}

section("AVCCFastPath.walkAVCC — nalTypeMask names the NAL types of a frame")
do {
    // AU: prefix SEI (39) + IDR_W_RADL (19); then a trailing-picture P frame (TRAIL_R = 1).
    func au(_ nals: [[UInt8]]) -> [UInt8] {
        var out = [UInt8]()
        for n in nals {
            let c = n.count
            out += [UInt8((c >> 24) & 0xFF), UInt8((c >> 16) & 0xFF), UInt8((c >> 8) & 0xFF), UInt8(c & 0xFF)]
            out += n
        }
        return out
    }
    let sei: [UInt8] = [39 << 1, 0x01, 0x00]
    let idr: [UInt8] = [19 << 1, 0x01, 0x11, 0x22]
    let trail: [UInt8] = [1 << 1, 0x01, 0x33]

    let wKey = walk(au([sei, idr]), isHEVC: true)
    check(wKey.valid && wKey.containsIDR, "HEVC SEI+IDR AU walks valid and is keyframe-class")
    check(wKey.nalTypeMask == (1 << 39) | (1 << 19), "nalTypeMask records exactly types 39 and 19")
    check(AVCCFastPath.nalTypeSummary(wKey.nalTypeMask) == "19,39", "nalTypeSummary renders ascending type list")

    let wP = walk(au([trail]), isHEVC: true)
    check(wP.valid && !wP.containsIDR, "HEVC TRAIL_R AU walks valid and is NOT keyframe-class")
    check(AVCCFastPath.nalTypeSummary(wP.nalTypeMask) == "1", "P-frame summary names type 1")

    // H.264 uses the 5-bit type field: SPS 7 + PPS 8 + IDR 5.
    let h264 = au([[0x67, 0x42], [0x68, 0xCE], [0x65, 0x88]])
    let w264 = walk(h264, isHEVC: false)
    check(w264.valid && w264.hasParamSets && w264.containsIDR, "H.264 SPS+PPS+IDR AU classified")
    check(AVCCFastPath.nalTypeSummary(w264.nalTypeMask) == "5,7,8", "H.264 mask uses the 5-bit type field")

    // A malformed AU still reports what it managed to read before refusing.
    let bad = au([sei]) + [0x00, 0x00, 0x00, 0x40, 0x01]  // second NAL claims 64 B, has 1
    let wBad = walk(bad, isHEVC: true)
    check(!wBad.valid, "truncated final NAL → invalid")
    check(wBad.nalTypeMask == (1 << 39), "invalid walk still carries the types it read (39) for the log")
    check(AVCCFastPath.nalTypeSummary(0) == "-", "empty mask renders as '-'")
}

// ════════════════════════════════════════════════════════════════════════════
// V5 bounded frame FIFO (AVCCFastPath.FrameFIFO) — the decoder's drop policy
// ════════════════════════════════════════════════════════════════════════════
//
// WHY THIS SUITE EXISTS: the depth-1 latest-wins slots shed a frame on every producer/consumer
// collision, and a shed frame punches a hole in the P-frame reference chain — everything after it
// decodes with errors until the phone's next IDR (~2 s). The 2026-09-03 device session logged 13 such
// drops in 12 s. `FrameFIFO` is the replacement, factored as a pure generic type precisely so the whole
// policy is testable here without VideoToolbox. Frames are Ints (their stream index) so a test can
// assert exactly WHICH frame survived, not merely how many.

section("AVCCFastPath.FrameFIFO — overflow drops the oldest unprotected P")
do {
    var q = AVCCFastPath.FrameFIFO<Int>(depth: 3)
    // Four consecutive P frames; the queue is never drained.
    check(q.push(0, keyframe: false).outcome == .queued, "P0 into an empty depth-3 FIFO → queued")
    check(q.push(1, keyframe: false).wasEmpty == false, "wasEmpty is false once the FIFO holds a frame")
    _ = q.push(2, keyframe: false)
    check(q.count == 3, "depth-3 FIFO fills to 3")
    let a = q.push(3, keyframe: false)
    check(a.outcome == .evictedOldest(index: 0), "full + all-P → the OLDEST frame is evicted")
    check(a.droppedWasKeyframe == false, "the evicted frame is reported as a P (for the log)")
    check(a.requestKeyframe, "evicting a P with no IDR behind it punches a hole → request a keyframe")
    check(q.count == 3, "the FIFO stays at its depth after an eviction")
    // Drain and confirm the SURVIVORS are the newest three, in order.
    check(q.pop() == 1 && q.pop() == 2 && q.pop() == 3, "survivors drain oldest→newest, minus frame 0")
    check(q.pop() == nil, "a drained FIFO pops nil")
}

section("AVCCFastPath.FrameFIFO — an IDR and the frame after it survive overflow")
do {
    var q = AVCCFastPath.FrameFIFO<Int>(depth: 3)
    _ = q.push(0, keyframe: false)   // P  — unprotected
    _ = q.push(1, keyframe: true)    // IDR — protected (keyframe)
    _ = q.push(2, keyframe: false)   // P  — protected (immediately follows the IDR)
    check(q.protectionFlags == [false, true, true], "protection = IDR + the frame immediately after it")
    let a = q.push(3, keyframe: false)
    check(a.outcome == .evictedOldest(index: 0), "the only unprotected frame (the leading P) is evicted")
    check(!a.requestKeyframe, "an IDR sits behind the hole → the chain self-repairs, no keyframe request")
    check(q.keyframeFlags == [true, false, false], "the IDR and IDR+1 both survived the overflow")
    check(q.pop() == 1 && q.pop() == 2 && q.pop() == 3, "IDR, IDR+1 and the newcomer drain in order")

    // The protection is STREAM-ORDER and frozen at push time: after an eviction the survivors keep the
    // answer they were admitted with, so a frame that merely INHERITS an IDR neighbour is not protected.
    var r = AVCCFastPath.FrameFIFO<Int>(depth: 3)
    _ = r.push(0, keyframe: true)    // IDR — protected
    _ = r.push(1, keyframe: false)   // P   — protected (IDR+1)
    _ = r.push(2, keyframe: false)   // P   — unprotected
    _ = r.push(3, keyframe: false)   // evicts 2 (the only unprotected one)
    check(r.keyframeFlags.count == 3 && r.protectionFlags == [true, true, false],
          "frame 3 stays unprotected even though it now sits next to IDR+1 (stream order, not adjacency)")
}

section("AVCCFastPath.FrameFIFO — all-protected: incoming P is refused, incoming IDR evicts")
do {
    // [IDR, P(IDR+1), IDR] — every slot protected, no eviction candidate.
    var q = AVCCFastPath.FrameFIFO<Int>(depth: 3)
    _ = q.push(0, keyframe: true)
    _ = q.push(1, keyframe: false)
    _ = q.push(2, keyframe: true)
    check(q.protectionFlags == [true, true, true], "IDR / IDR+1 / IDR — nothing is droppable")

    var refused = q
    let p = refused.push(3, keyframe: false)
    check(p.outcome == .rejectedIncoming, "all queued protected + incoming P → the INCOMING frame drops")
    check(p.requestKeyframe, "refusing the newcomer breaks the chain → request a keyframe")
    check(p.droppedWasKeyframe == false, "the refused newcomer is reported as a P")
    check(refused.keyframeFlags == [true, false, true], "the queue is untouched by a refused push")
    check(refused.count == 3, "a refused push does not grow the FIFO")

    // An incoming IDR is never refused: dropping it would orphan every P that references it (~2 s of
    // poison), whereas the frames behind the evicted one are repaired by that IDR within `depth` frames.
    var admitted = q
    let k = admitted.push(3, keyframe: true)
    check(k.outcome == .evictedProtected(index: 0), "all queued protected + incoming IDR → oldest evicted")
    check(!k.requestKeyframe, "the admitted IDR is itself the re-sync — no keyframe request")
    check(k.droppedWasKeyframe == true, "the evicted frame is reported as an IDR")
    check(admitted.pop() == 1 && admitted.pop() == 2 && admitted.pop() == 3, "the incoming IDR is queued")

    // A refusal resets the protection carry: the frame AFTER a refused push follows a hole, not an IDR.
    var s = AVCCFastPath.FrameFIFO<Int>(depth: 1)
    _ = s.push(0, keyframe: true)
    check(s.push(1, keyframe: false).outcome == .rejectedIncoming, "depth-1: P over IDR is refused")
    _ = s.pop()
    _ = s.push(2, keyframe: false)
    check(s.protectionFlags == [false], "the frame after a REFUSED frame inherits no IDR protection")
}

section("AVCCFastPath.FrameFIFO — depth 1 reproduces the V4 resolveSlot table")
do {
    // The oracle is `resolveSlot` itself (the V4 single-slot drop table), not a hand-copied duplicate.
    func replay(_ held: Bool?, _ incoming: Bool) -> (store: Bool, requestKeyframe: Bool) {
        var q = AVCCFastPath.FrameFIFO<Int>(depth: 1)
        if let held { _ = q.push(0, keyframe: held) }
        let a = q.push(1, keyframe: incoming)
        // "store" in V4 terms = the incoming frame ended up in the slot.
        return (a.outcome != .rejectedIncoming, a.requestKeyframe)
    }
    for held in [Bool?.none, .some(false), .some(true)] {
        for incoming in [false, true] {
            let want = AVCCFastPath.resolveSlot(oldIsKF: held, newIsKF: incoming)
            let got = replay(held, incoming)
            let name = "depth-1 held=\(held.map { $0 ? "IDR" : "P" } ?? "-") new=\(incoming ? "IDR" : "P")"
            check(got.store == want.store, "\(name): store matches resolveSlot")
            check(got.requestKeyframe == want.requestKeyframe, "\(name): requestKeyframe matches resolveSlot")
        }
    }
    // Documented (and deliberate) divergence: when the previously ADMITTED frame was an IDR, the single
    // slot now holds IDR+1, which V5 protects. V4 replaced it; replacing it also orphans the incoming
    // frame, so keeping IDR+1 is strictly better — this is the one case the tables differ.
    var q = AVCCFastPath.FrameFIFO<Int>(depth: 1)
    _ = q.push(0, keyframe: true)
    _ = q.pop()
    _ = q.push(1, keyframe: false)          // IDR+1, protected
    let a = q.push(2, keyframe: false)
    check(a.outcome == .rejectedIncoming, "depth-1 protects IDR+1 (the one deliberate V4 divergence)")
    check(AVCCFastPath.resolveSlot(oldIsKF: false, newIsKF: false).store,
          "…where V4 would have replaced it (oracle still says store)")
}

section("AVCCFastPath.FrameFIFO — depth knob, flush, and drain bookkeeping")
do {
    var q = AVCCFastPath.FrameFIFO<Int>(depth: 3)
    check(q.push(0, keyframe: false).wasEmpty, "the first push reports wasEmpty (caller dispatches a drain)")
    _ = q.push(1, keyframe: false)
    q.removeAll()
    check(q.isEmpty && q.count == 0, "removeAll empties the FIFO")
    check(q.push(2, keyframe: false).wasEmpty, "after a flush the next push reports wasEmpty again")
    check(q.protectionFlags == [false], "removeAll also clears the IDR protection carry")

    // The knob is live (AA raises it after construction) and clamps at 1.
    q.depth = 64
    for i in 0..<64 { _ = q.push(i, keyframe: false) }
    check(q.count == 64, "raising depth admits the AA startup burst without shedding")
    q.depth = 0
    check(q.depth == 1, "depth clamps to a minimum of 1")
    // A shrink is not retroactive; the drop policy simply engages until the consumer catches up.
    let a = q.push(999, keyframe: false)
    check(a.outcome == .evictedOldest(index: 0) && q.count == 64, "over-depth FIFO sheds on the next push")
    check(AVCCFastPath.FrameFIFO<Int>(depth: -5).depth == 1, "init clamps a nonsense depth to 1")
}



section("CT_PAIR_CONFIRM — the user's answer to the Numeric-Comparison prompt")
do {
    // The wire byte itself: a disagreement here is two endpoints meaning different things by one
    // opcode, which is exactly what tools/proto_check.py guards across languages.
    check(OCBM.ctPairConfirm == 0x1C, "ctPairConfirm is 0x1C")

    let t = FakeTransport()
    let c = OCBMClient(transport: t)
    c.connect()
    t.inject(ctrlFrame([OCBM.ctHelloAck]))
    check(waitUntil(3.0) { ctrlCount(t, OCBM.ctSubscribe) >= 1 }, "client subscribed")

    // The last CH_CTRL frame carrying `op`, payload only (header is 16 B).
    func lastCtrlPayload(_ op: UInt8) -> [UInt8]? {
        t.writes.last { f in f.count > OCBM.hdrLen && OCBM.readLE16(f, 8) == OCBM.chCtrl && f[16] == op }
            .map { Array($0[OCBM.hdrLen...]) }
    }

    c.sendPairConfirm(accept: true)
    check(waitUntil(2.0) { ctrlCount(t, OCBM.ctPairConfirm) == 1 }, "Pair sends exactly one frame")
    check(lastCtrlPayload(OCBM.ctPairConfirm) ?? [] == [OCBM.ctPairConfirm, 1],
          "Pair encodes as [0x1C, 1] — 2 bytes, no padding")

    c.sendPairConfirm(accept: false)
    check(waitUntil(2.0) { ctrlCount(t, OCBM.ctPairConfirm) == 2 }, "Cancel sends its own frame")
    check(lastCtrlPayload(OCBM.ctPairConfirm) ?? [] == [OCBM.ctPairConfirm, 0],
          "Cancel encodes as [0x1C, 0] — the box reads any non-zero byte as Pair")

    // It rides CH_CTRL like every other control message, and does not disturb the heartbeat.
    let f = t.writes.last { $0.count > OCBM.hdrLen && $0[16] == OCBM.ctPairConfirm }!
    check(OCBM.readLE16(f, 8) == OCBM.chCtrl, "sent on CH_CTRL")
    check(f[10] == (OCBM.fSom | OCBM.fEom), "single SOM+EOM frame")

    c.disconnect()
}

section("AVCCFastPath.FrameFIFO — the drain-dispatch protocol never strands a frame (V6)")
do {
    // WHY: both hand-offs dispatch a drain ONLY on the empty→non-empty transition and rely on the
    // in-flight drain re-dispatching itself while the FIFO is non-empty. At depth 1 the tail dispatch
    // is unreachable, so it was easy to omit — and `drainEnqueue` HAD omitted it, which at depth 3
    // would have stranded every frame pushed onto a non-empty queue until the next empty→non-empty
    // edge. This models the exact protocol (see `VideoDecoder.decodeAndDisplay` / `drainDecode` /
    // `enqueue` / `drainEnqueue`) and asserts the invariant that makes it correct:
    //
    //     the FIFO is non-empty  ⇒  at least one drain is scheduled or running
    //
    // Deterministic pseudo-random schedules, so a failure is reproducible rather than flaky.
    var seed: UInt64 = 0x5DEECE66D
    func next(_ n: Int) -> Int {                       // xorshift64* — no Foundation RNG dependency
        seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17
        return Int(seed % UInt64(n))
    }

    var invariantHeld = true, orderHeld = true, strandedAny = false, sawTailDispatch = false
    for depth in [1, 2, 3, 8] {
        for trial in 0..<200 {
            var fifo = AVCCFastPath.FrameFIFO<Int>(depth: depth)
            var scheduledDrains = 0
            var delivered: [Int] = []
            var pushed = 0

            // A step is either a producer push or (when one is scheduled) a consumer drain, in an
            // arbitrary order — exactly the interleavings the two real queues can produce.
            for _ in 0..<60 {
                if next(2) == 0 || scheduledDrains == 0 {
                    // PRODUCER — VideoDecoder.decodeAndDisplay / enqueue
                    let isIDR = next(10) == 0            // ~1 keyframe in 10, as on the wire
                    let a = fifo.push(pushed, keyframe: isIDR)
                    if a.wasEmpty { scheduledDrains += 1 }
                    pushed += 1
                } else {
                    // CONSUMER — drainDecode / drainEnqueue: pop one, then re-dispatch if work remains
                    scheduledDrains -= 1
                    if let f = fifo.pop() {
                        if let last = delivered.last, f <= last { orderHeld = false }
                        delivered.append(f)
                        if !fifo.isEmpty { scheduledDrains += 1; sawTailDispatch = true }
                    }
                }
                // THE invariant. Violating it is a stranded frame — silent frozen video, not a crash.
                if !fifo.isEmpty && scheduledDrains == 0 { invariantHeld = false }
            }

            // Run the scheduled drains to completion: everything still queued must come out.
            var guardCount = 0
            while scheduledDrains > 0 && guardCount < 1000 {
                guardCount += 1
                scheduledDrains -= 1
                if let f = fifo.pop() {
                    delivered.append(f)
                    if !fifo.isEmpty { scheduledDrains += 1 }
                }
            }
            if !fifo.isEmpty { strandedAny = true }
            _ = trial
        }
    }
    check(invariantHeld, "non-empty FIFO always has a drain scheduled (no lost wakeup, all depths)")
    check(!strandedAny, "quiescing the scheduled drains always empties the FIFO (no stranded frame)")
    check(orderHeld, "frames are delivered in strictly increasing push order (FIFO, never reordered)")
    check(sawTailDispatch, "the schedules actually exercised the tail re-dispatch (test is not vacuous)")
}



// ════════════════════════════════════════════════════════════════════════════
//
// SEAM_PKT_PLAIN (marker 0x03) — the Android Auto TELEPHONY lane. The box bridges the call's
// Bluetooth HFP/SCO audio (CVSD, 8 kHz mono S16LE) onto the existing voice sink. There is no AirPlay
// stream behind it, so there is no key and no RTP: the payload rides verbatim after a PCM
// SEAM_FORMAT and must reach the player BYTE-FOR-BYTE. Two ways this could silently break — the
// length-plausibility gate rejecting the new marker as a false magic (which would resync the lane
// away one message at a time), and the RFC 2198 demux "finding" a redundancy bundle inside PCM.

section("OCBMAVDecrypt — SEAM_PKT_PLAIN (0x03): the AA telephony lane")
do {
    final class PlainRecorder: OCBMAVDelegate {
        let aus = Mutex<[(Data, UInt64, OCBMAudioStreamFormat?)]>([])
        let formats = Mutex<[(UInt64, OCBMAudioStreamFormat)]>([])
        func avDidReceiveVideoConfig(_ config: Data) {}
        func avDidReceiveVideoFrame(_ avcc: Data, keyframe: Bool) {}
        func avDidReceiveAudioFormat(scid: UInt64, format: OCBMAudioStreamFormat) {
            formats.withLock { $0.append((scid, format)) }
        }
        func avDidReceiveAudio(_ au: Data, scid: UInt64, format: OCBMAudioStreamFormat?) {
            aus.withLock { $0.append((au, scid, format)) }
        }
        func avDidReceiveAltVideoConfig(_ config: Data) {}
        func avDidReceiveAltVideoFrame(_ avcc: Data, keyframe: Bool) {}
    }

    let seamMagic: [UInt8] = [0x53, 0x45, 0x41, 0x56] // "SEAV"
    func le64(_ v: UInt64) -> [UInt8] { withUnsafeBytes(of: v.littleEndian) { Array($0) } }
    func le32(_ v: UInt32) -> [UInt8] { withUnsafeBytes(of: v.littleEndian) { Array($0) } }
    func framed(_ body: [UInt8]) -> [UInt8] {
        let b = seamMagic + body
        let n = UInt32(b.count)
        return [UInt8(n >> 24), UInt8((n >> 16) & 0xFF), UInt8((n >> 8) & 0xFF), UInt8(n & 0xFF)] + b
    }
    let scid: UInt64 = 0x7F
    // The contract's telephony format: PCM, 8000 Hz, 1 ch, 16-bit, audio_type 1 (telephony).
    let telFmt: [UInt8] = [0x02] + le64(scid) + [0] + le32(8_000) + [1, 16, 1]
    // 320 B = 160 samples of S16LE = exactly 20 ms at 8 kHz. Content is a ramp so a byte-swap, a
    // truncation or an RFC 2198 "primary" would all show up as a different payload.
    let pcm: [UInt8] = (0..<320).map { UInt8($0 & 0xFF) }
    let plainMsg: [UInt8] = [0x03] + le64(scid) + pcm

    check(framed(telFmt).count == 4 + 21, "the telephony SEAM_FORMAT still frames to the pinned len 21")
    check(framed(plainMsg).count == 4 + 4 + 1 + 8 + 320, "a 20 ms plain frame is [len][SEAV][0x03][scid][320 B]")

    // ── 1. Format then plain frames: delivered unchanged, no key anywhere in sight ───────────────
    do {
        let dec = OCBMAVDecrypt()
        let rec = PlainRecorder()
        dec.delegate = rec
        dec.feedVoice(framed(telFmt) + framed(plainMsg) + framed(plainMsg))
        check(waitUntil(2.0) { rec.aus.withLock { $0.count == 2 } },
              "both SEAM_PKT_PLAIN frames were delivered (the 0x03 marker is not rejected as a false magic)")
        if let (au, gotScid, fmt) = rec.aus.withLock({ $0 }).first {
            check(au == Data(pcm), "the plain payload reaches the player BYTE-FOR-BYTE (no decrypt, no RFC 2198 demux)")
            check(au.count == 320, "one frame is 320 B = 20 ms of 8 kHz mono S16LE")
            check(gotScid == scid, "the AU carries the scid from its own SEAM_PKT_PLAIN")
            check(fmt?.sampleRate == 8_000 && fmt?.channels == 1 && fmt?.bits == 16 && fmt?.isPCM == true,
                  "the delivered format is the 8000 Hz mono 16-bit PCM one the box declared")
            check(fmt?.audioType == 1 && fmt?.isVoice == true,
                  "audio_type 1 (telephony) routes to the voice sink, so it ducks media")
            check(fmt?.plainLE == true,
                  "the delivered format is stamped plainLE — HFP PCM is host-endian, unlike CarPlay's big-endian wire")
        }
        let st = dec.statsSnapshot()
        check(st.audioPlainOK == 2, "plain access units are counted in audioPlainOK")
        check(st.audioOK == 0 && st.audioFail == 0, "and NOT in the decrypt tallies — nothing was decrypted")
        check(st.audioNoKeyDrops == 0, "a plain frame is never a keyless drop (there is no key by design)")
        check(st.voiceAudioRxFrames == 1 && st.voiceAudioRxBytes > 0,
              "raw CH_ALT_AUDIO arrival still counts the frame that carried them")
        // The SEAM_FORMAT the delegate saw is the WIRE one; only the per-AU copy is stamped.
        check(rec.formats.withLock { $0.first?.1.plainLE } == false,
              "the format table keeps the wire value — plainLE is a property of the AU's marker, not the stream")
    }

    // ── 2. A plain frame with no SEAM_FORMAT is dropped and counted ──────────────────────────────
    do {
        let dec = OCBMAVDecrypt()
        let rec = PlainRecorder()
        dec.delegate = rec
        dec.feedVoice(framed(plainMsg) + framed(plainMsg))
        check(waitUntil(2.0) { dec.statsSnapshot().audioPlainNoFormatDrops == 2 },
              "plain frames before any SEAM_FORMAT are counted as drops (rate/channels unknown — playing them would be noise)")
        check(rec.aus.withLock { $0.isEmpty }, "and nothing was handed to the player")
        check(dec.statsSnapshot().audioPlainOK == 0, "a dropped frame is not also counted as delivered")
    }

    // ── 3. The three existing markers are untouched by the new one ───────────────────────────────
    do {
        let dec = OCBMAVDecrypt()
        let rec = PlainRecorder()
        dec.delegate = rec
        // A 48k/2ch media SEAM_FORMAT on the media seam still parses exactly as before, and a plain
        // frame interleaved on the OTHER seam does not disturb it (separate buffers, shared tables).
        let mediaScid: UInt64 = 0x2A
        let mediaFmt: [UInt8] = [0x02] + le64(mediaScid) + [0] + le32(48_000) + [2, 16, 0]
        dec.feedAudio(framed(mediaFmt))
        dec.feedVoice(framed(telFmt) + framed(plainMsg))
        check(waitUntil(2.0) { rec.formats.withLock { $0.count == 2 } }, "both SEAM_FORMATs parsed")
        let fmts = rec.formats.withLock { $0 }
        check(fmts.contains { $0.0 == mediaScid && $0.1.sampleRate == 48_000 && $0.1.channels == 2 && $0.1.audioType == 0 },
              "the media stream's 48000 Hz 2ch PCM format is unchanged")
        check(fmts.contains { $0.0 == scid && $0.1.sampleRate == 8_000 && $0.1.channels == 1 },
              "the telephony stream's 8000 Hz mono format decodes beside it")
        check(waitUntil(2.0) { dec.statsSnapshot().audioPlainOK == 1 }, "the plain frame on the voice seam still landed")
    }

    // ── 4. A plain frame torn out of framing still re-aligns on the next magic ───────────────────
    do {
        let dec = OCBMAVDecrypt()
        let rec = PlainRecorder()
        dec.delegate = rec
        dec.feedVoice(framed(telFmt))
        // Valid magic, impossible length for a 0x03 message (13 is header-only, below the 15 floor):
        // structurally rejected, so the parser must scan forward rather than trust it.
        var torn: [UInt8] = [0x00, 0x00, 0x00, 13]
        torn += seamMagic
        torn += [0x03]
        torn += [UInt8](repeating: 0x11, count: 8)
        dec.feedVoice(torn + framed(plainMsg))
        check(waitUntil(2.0) { rec.aus.withLock { $0.count == 1 } },
              "a bad-length plain message is skipped and the next real one is delivered")
        if let (au, _, _) = rec.aus.withLock({ $0 }).first {
            check(au == Data(pcm), "the recovered AU is still byte-identical")
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════

section("AA — AA_TELEPHONY_SINK experiment (docs/androidauto/03_WIRELESS.md §6)")
do {
    let base = AACapability.audioSinkTable(telephony: false)
    let withTel = AACapability.audioSinkTable(telephony: true)
    check(base.count == 3, "the DEFAULT service set is still exactly three audio sinks")
    check(base.map { $0.channel } == [4, 5, 6], "on the same channel ids as before")
    check(withTel.count == 4 && withTel.last?.streamType == 4,
          "the lever appends one AUDIO_STREAM_TELEPHONY (type 4) sink")
    check(withTel.last?.channel == 2,
          "on channel 2 — the first id not already spoken for (0/1/3/4/5/6/8/9)")
    check(!base.map({ $0.channel }).contains(2), "which no default sink uses")
    check(withTel.last?.rate == 16_000 && withTel.last?.channels == 1 && withTel.last?.voice == true,
          "mirroring the GUIDANCE sink's shape: 16 kHz mono, voice-routed so it ducks media")
    check(Array(withTel.prefix(3)).map { $0.channel } == base.map { $0.channel },
          "and changes nothing about the three sinks that ship")

    // The declaration is accepted or rejected WHOLE (CAR.SERVICE Critical error 2/24), so what
    // matters is that the type-4 sink appears in the encoded SD response ONLY with the lever.
    func sd(_ sinks: [AACapability.AudioSink]) -> [UInt8] {
        [UInt8](AAWire.serviceDiscoveryResponseFull(resolution: 1, fps: 2, density: 160,
                                                    tsW: 800, tsH: 480, name: "Carlink", sinks: sinks))
    }
    // One MediaSinkService ServiceConfiguration for channel 2: sc{1: id=2, 3: mss{1:1, 2:type, 3:ac}}.
    // Encoded by the same builder the response uses, so this is a substring search for that sink.
    let telSC = [UInt8](AAWire.audioSink(2, 4, 16_000, 1))
    func contains(_ hay: [UInt8], _ needle: [UInt8]) -> Bool {
        guard !needle.isEmpty, hay.count >= needle.count else { return false }
        for i in 0...(hay.count - needle.count) where Array(hay[i..<i + needle.count]) == needle { return true }
        return false
    }
    check(!contains(sd(base), telSC), "the DEFAULT SD response carries NO type-4 telephony sink")
    check(contains(sd(withTel), telSC), "with the lever, the SD response carries it")
    check(sd(withTel).count > sd(base).count, "and is otherwise the same document, one sink longer")
    check(AACapability.telephonySinkExperiment == (ProcessInfo.processInfo.environment["AA_TELEPHONY_SINK"] == "1"),
          "the lever is read from AA_TELEPHONY_SINK and is OFF unless it is exactly \"1\"")
    check(AACapability.telephonySinkChannel == AAWire.chTelephonyAudio,
          "one channel-id constant, so the declaration and the router cannot drift apart")
}

// ════════════════════════════════════════════════════════════════════════════

section("AARates — Android Auto cumulative counters → per-second rates")
do {
    var a = AAStatsSnapshot()
    a.t = 100.0
    a.videoRx = 100; a.videoDecoded = 98; a.videoDropped = 2
    a.audioMedia = 1000; a.audioGuidance = 10; a.audioSystem = 5; a.audioTelephony = 0
    a.micFrames = 500; a.bytesRx = 1_000_000; a.bytesTx = 100_000
    var b = a
    b.t = 102.0                       // a 2 s gap: AA's loop is frame-driven, ticks are not exactly 1 s
    b.videoRx = 160; b.videoDecoded = 158; b.videoDropped = 2
    b.audioMedia = 1086; b.audioGuidance = 10; b.audioSystem = 9; b.audioTelephony = 100
    b.micFrames = 600; b.bytesRx = 4_000_000; b.bytesTx = 200_000
    b.backlog = 3; b.transport = "wireless"

    let r = AARates.between(a, b)
    check(r.dt == 2.0, "the interval comes from the snapshots' own clock, not an assumed tick")
    check(r.videoRxPerSec == 30.0 && r.videoDecodedPerSec == 30.0, "60 frames over 2 s = 30/s")
    check(r.videoDropPerSec == 0.0, "an unchanged drop counter is 0/s, not a stale total")
    check(r.audioMediaPerSec == 43.0 && r.audioSystemPerSec == 2.0, "audio packet rates are per-channel")
    check(r.audioTelephonyPerSec == 50.0, "the telephony sink's 20 ms frames read as 50/s")
    check(r.micPerSec == 50.0, "mic uplink likewise")
    check(abs(r.rxMbps - 12.0) < 0.001 && abs(r.txMbps - 0.4) < 0.001, "bytes become Mbps (×8 / 1e6)")
    check(r.backlog == 3 && r.transport == "wireless", "backlog is a LEVEL and the transport label passes through")

    // Degenerate inputs the UI must never render as inf/NaN or a negative rate.
    check(AARates.between(b, b).videoRxPerSec == 0 && AARates.between(b, b).dt == 0,
          "the same snapshot twice is all zeros, never a division by zero")
    check(AARates.between(b, a).videoRxPerSec == 0, "a snapshot going BACKWARDS in time is all zeros")
    var restarted = b
    restarted.t = 104.0
    restarted.videoRx = 0             // AASession restarted: counters back to zero
    check(AARates.between(b, restarted).videoRxPerSec == 0,
          "a counter reset (new AA session) clamps to 0 rather than reporting a negative rate")
    check(AARates.between(b, restarted).backlog == restarted.backlog, "levels still pass through a reset")
}



// ──────────────────────────────────────────────────────────────────────────────
// mSBC — HFP wideband telephony codec (Audio/MSBCCodec.swift, Audio/MSBCFramer.swift)
// ──────────────────────────────────────────────────────────────────────────────
// macOS has no SBC codec, so this one is ours end to end: get a coefficient sign wrong and the
// only symptom on hardware is "the call sounds like noise", with no reference to compare against.
// Hence the checks below start at the filterbank and work outwards.
//
// The reference vector is a 4-frame bitstream produced by this encoder and decoded by an
// INDEPENDENT implementation — a transcription of ffmpeg's fixed-point `sbc_synthesize_eight`
// (libavcodec/sbcdec.c) driven by its own Q32 window tables — so it pins polarity, scaling, the
// scale-factor/bit-allocation parse and the synthesis permutation all at once. Agreement to a
// couple of LSB is the expected fixed-point-vs-float difference; a structural error is not subtle.

section("mSBC — prototype filterbank tables")
do {
    check(MSBCTables.proto.count == 80, "the SBC 8-subband prototype is 80 taps")
    var symmetric = true
    for n in 1..<80 where MSBCTables.proto[n] != MSBCTables.proto[80 - n] { symmetric = false }
    check(symmetric, "the unfolded prototype is exactly symmetric about tap 40 (a transcription typo breaks this)")
    check(abs(MSBCTables.proto[40] - 1.46955073e-01) < 1e-9, "centre tap is the spec's proto_8_80[40]")
    // The fold C[n] = p[n]·(-1)^floor(n/16) is what lets both matrixing steps index by (n mod 16).
    var folded = true
    for n in 0..<80 {
        let want = (n / 16) % 2 == 0 ? MSBCTables.proto[n] : -MSBCTables.proto[n]
        if MSBCTables.analysisWindow[n] != want { folded = false }
    }
    check(folded, "the analysis window is the prototype with the block-index sign fold")
    check(MSBCTables.synthesisWindow[40] == -8.0 * MSBCTables.analysisWindow[40],
          "the synthesis window is -8x the analysis window")
    // Bit allocation is recomputed identically by both ends from the transmitted scale factors.
    check(MSBCTables.calculateBits(scaleFactors: [0, 0, 0, 0, 0, 0, 0, 0]).reduce(0, +) <= MSBC.bitpool,
          "an all-zero scale-factor set allocates no more than the bitpool (and terminates)")
    let bits = MSBCTables.calculateBits(scaleFactors: [11, 12, 9, 10, 0, 0, 0, 0])
    check(bits == [8, 7, 5, 6, 0, 0, 0, 0], "LOUDNESS allocation matches the spec loop for a voiced frame")
}

section("mSBC — encoder/decoder round trip")
do {
    // 1 kHz at 16 kHz mono, 20 frames = 150 ms.
    let frames = 20
    let n = frames * MSBC.samplesPerFrame
    let tone: [Int16] = (0..<n).map { Int16(8000.0 * sin(2.0 * Double.pi * 1000.0 * Double($0) / 16000.0)) }

    let enc = MSBCEncoder()
    let dec = MSBCDecoder()
    var encoded: [[UInt8]] = []
    var decoded: [Int16] = []
    for f in 0..<frames {
        guard let frame = enc.encode(Array(tone[(f * 120)..<((f + 1) * 120)])) else {
            check(false, "encoder accepted a 120-sample frame"); break
        }
        encoded.append(frame)
        switch dec.decode(frame[...]) {
        case .success(let pcm): decoded.append(contentsOf: pcm)
        case .failure(let why): check(false, "decode failed: \(why)")
        }
    }
    check(encoded.count == frames && encoded.allSatisfy { $0.count == MSBC.frameBytes },
          "every frame encodes to exactly 57 bytes")
    check(encoded.allSatisfy { $0[0] == 0xAD && $0[1] == 0 && $0[2] == 0 },
          "every frame carries the mSBC syncword and zeroed parameter bytes")
    check(decoded.count == tone.count, "decoded length == input length (120 samples per frame)")

    // The filterbank's reconstruction delay is 73 samples; measure SNR on the aligned overlap,
    // skipping the first two frames while the delay line fills.
    let delay = 73
    var sig = 0.0, err = 0.0
    for i in 240..<(n - delay) {
        let a = Double(tone[i]), b = Double(decoded[i + delay])
        sig += a * a
        err += (b - a) * (b - a)
    }
    let snr = 10.0 * log10(sig / max(err, 1e-9))
    check(snr > 20.0, "1 kHz round-trip SNR > 20 dB (measured \(String(format: "%.1f", snr)) dB)")

    // Silence must stay silence: with all scale factors 0 the quantiser lands exactly mid-scale,
    // and any DC offset introduced here would be audible as a click at every call setup.
    let encS = MSBCEncoder(), decS = MSBCDecoder()
    var silentOut: [Int16] = []
    for _ in 0..<4 {
        guard let f = encS.encode([Int16](repeating: 0, count: 120)) else { break }
        if case .success(let pcm) = decS.decode(f[...]) { silentOut.append(contentsOf: pcm) }
    }
    check(silentOut.count == 480 && silentOut.allSatisfy { $0 == 0 },
          "an all-zero frame decodes to digital silence")
}

section("mSBC — frame validation")
do {
    let enc = MSBCEncoder()
    let tone: [Int16] = (0..<120).map { Int16(6000.0 * sin(2.0 * Double.pi * 400.0 * Double($0) / 16000.0)) }
    guard var frame = enc.encode(tone) else { check(false, "encode"); exit(1) }
    let dec = MSBCDecoder()
    if case .failure(let why) = dec.decode(frame[...]) { check(false, "a well-formed frame decodes (\(why))") }
    else { check(true, "a well-formed frame decodes") }

    var bad = frame
    bad[3] ^= 0xFF
    check(MSBCDecoder().decode(bad[...]) == .failure(.crcMismatch), "a CRC mismatch is rejected")

    // A corrupt scale factor changes the CRC input, so it is caught as a CRC failure rather than
    // silently decoding with the wrong bit allocation — which is the whole point of the CRC.
    var sfCorrupt = frame
    sfCorrupt[4] ^= 0x10
    check(MSBCDecoder().decode(sfCorrupt[...]) == .failure(.crcMismatch), "a flipped scale-factor bit is caught by the CRC")

    frame[0] = 0x9C // plain SBC syncword
    check(MSBCDecoder().decode(frame[...]) == .failure(.badSync), "a non-mSBC syncword is rejected")
    check(MSBCDecoder().decode(ArraySlice([UInt8](repeating: 0xAD, count: 20))) == .failure(.shortFrame),
          "a short frame is rejected, not read past")
    var reserved = enc.encode(tone)!
    reserved[1] = 0x01
    check(MSBCDecoder().decode(reserved[...]) == .failure(.reservedHeader),
          "a frame with non-zero SBC parameter bytes is rejected (it is not mSBC)")
}

section("mSBC — H2/eSCO framer: resync, fragmentation, sequence loss")
do {
    let up = MSBCUplinkEncoder()
    var pcm = Data()
    for i in 0..<120 {
        let s = Int16(5000.0 * sin(2.0 * Double.pi * 800.0 * Double(i) / 16000.0))
        let u = UInt16(bitPattern: s)
        pcm.append(UInt8(u & 0xFF)); pcm.append(UInt8(u >> 8))
    }
    // Four packets with the cycling sequence, exactly as they go on the SCO socket.
    var packets: [[UInt8]] = []
    for _ in 0..<4 { packets.append([UInt8](up.packet(from: pcm)!)) }
    check(packets.allSatisfy { $0.count == MSBC.packetBytes }, "an eSCO packet is 60 bytes (H2 + 57 + pad)")
    check(packets.map { $0[1] } == MSBCH2.sequenceBytes, "the H2 sequence cycles 0x08 0x38 0xC8 0xF8")
    check(packets.allSatisfy { $0[0] == 0x01 && $0[2] == MSBC.syncword && $0[59] == 0 },
          "each packet is [0x01][sn][0xAD …][pad]")

    // 1. Whole packets, one push each.
    var f = MSBCFramer()
    var got = 0
    for p in packets { for e in f.push(p) { if case .frame = e { got += 1 } } }
    check(got == 4 && f.lostPackets == 0 && f.resyncs == 0,
          "four clean packets yield four frames, no loss, no resync (the pad byte is not a resync)")

    // 2. The same stream cut at arbitrary, packet-misaligned boundaries.
    var f2 = MSBCFramer()
    var flat: [UInt8] = []
    for p in packets { flat += p }
    var frames2 = 0
    for chunk in stride(from: 0, to: flat.count, by: 37) {
        let slice = Array(flat[chunk..<min(chunk + 37, flat.count)])
        for e in f2.push(slice) { if case .frame = e { frames2 += 1 } }
    }
    check(frames2 == 4 && f2.lostPackets == 0,
          "the framer reassembles across split reads (37-byte chunks cut every packet)")

    // 3. A packet dropped in flight: the sequence number is the ONLY signal, and it must produce a
    //    counted loss event rather than a silent gap in the audio.
    var f3 = MSBCFramer()
    var events: [MSBCFramer.Event] = []
    for (i, p) in packets.enumerated() where i != 2 { events += f3.push(p) }
    let lost = events.compactMap { if case .lost(let n) = $0 { return n } else { return nil } }
    check(events.filter { if case .frame = $0 { return true } else { return false } }.count == 3,
          "three surviving packets decode")
    check(lost == [1] && f3.lostPackets == 1, "the dropped packet is detected from the H2 sequence gap and counted")

    // 4. Garbage on the lane: relock on the header, and say so.
    var f4 = MSBCFramer()
    var frames4 = 0
    for e in f4.push([UInt8](repeating: 0x5A, count: 40) + packets[0]) {
        if case .frame = e { frames4 += 1 }
    }
    check(frames4 == 1 && f4.resyncs == 1, "40 bytes of junk are skipped, the packet behind them decodes, one resync counted")

    // 5. End to end through the downlink adapter, including concealment for the dropped packet.
    let tel = MSBCTelephonyDecoder()
    var out = Data()
    for (i, p) in packets.enumerated() where i != 2 { out.append(tel.decode(Data(p))) }
    check(out.count == 4 * MSBC.samplesPerFrame * 2,
          "three good packets + one concealed frame = four frames of 16 kHz PCM")
    check(tel.framesDecoded == 3 && tel.plcFrames == 1, "the adapter counts 3 decoded frames and 1 PLC frame")
    let telFrag = MSBCTelephonyDecoder()
    check(telFrag.decode(Data(packets[0].prefix(30))).isEmpty, "a fragment yields no PCM until it completes")
    check(telFrag.decode(Data(packets[0].dropFirst(30))).count == MSBC.samplesPerFrame * 2,
          "the completing fragment yields exactly one frame")
}

section("mSBC — decode consistency against an independent (ffmpeg fixed-point) decoder")
do {
    // 4 frames of 1 kHz + 3 kHz at 16 kHz, produced by this encoder; the expected PCM for the
    // FOURTH frame comes from the reference decoder run over the same four frames from cold state
    // (so it also pins the synthesis history, not just one frame's arithmetic).
    let refFrames = [
        "ad0000f8cc9a88777deddb5f7b76d860ddb5e7478cc6b1c95233c2ea314db5729c6d9f349b5729c6d9f349b5729c6d9f349b5729c6d9f349b4",
        "ad00009abc9a00003c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cc",
        "ad00009abc9a0000c2d172cf0b1f3c2d172cf0b1f3c2d172cf0b1f3c2d172cf0b1f3c2d172cf0b1f3c2d172cf0b1f3c2d172cf0b1f3c2d172c",
        "ad00009abc9a00003c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cf0b45cb3c2c7cc",
    ]
    let refPCM4th: [Int16] = [
        -4968, -36, 4885, 7072, 6670, 6092, 6734, 7162, 4969, 38, -4884, -7071,
        -6671, -6091, -6733, -7161, -4968, -36, 4885, 7072, 6670, 6092, 6734, 7162,
        4969, 38, -4884, -7071, -6671, -6091, -6733, -7161, -4968, -36, 4885, 7072,
        6670, 6092, 6734, 7162, 4969, 38, -4884, -7071, -6671, -6091, -6733, -7161,
        -4968, -36, 4885, 7072, 6670, 6092, 6734, 7162, 4969, 38, -4884, -7071,
        -6671, -6091, -6733, -7161, -4968, -36, 4885, 7072, 6670, 6092, 6734, 7162,
        4969, 38, -4884, -7071, -6671, -6091, -6733, -7161, -4968, -36, 4885, 7072,
        6670, 6092, 6734, 7162, 4969, 38, -4884, -7071, -6671, -6091, -6733, -7161,
        -4968, -36, 4885, 7072, 6670, 6092, 6734, 7162, 4969, 38, -4884, -7071,
        -6671, -6091, -6733, -7161, -4968, -36, 4885, 7072, 6670, 6092, 6734, 7162,
    ]
    func unhex(_ s: String) -> [UInt8] {
        var out: [UInt8] = []
        var it = s.startIndex
        while it < s.endIndex {
            let next = s.index(it, offsetBy: 2)
            out.append(UInt8(s[it..<next], radix: 16)!)
            it = next
        }
        return out
    }

    // The encoder must reproduce the recorded bitstream bit-for-bit from the same input: a change
    // in scale-factor selection or quantiser rounding is a wire change and must not pass silently.
    let src: [Int16] = (0..<480).map { i in
        let t = Double(i) / 16000.0
        return Int16((8000.0 * sin(2.0 * .pi * 1000.0 * t) + 2000.0 * sin(2.0 * .pi * 3000.0 * t)).rounded())
    }
    let enc = MSBCEncoder()
    var produced: [[UInt8]] = []
    for f in 0..<4 { produced.append(enc.encode(Array(src[(f * 120)..<((f + 1) * 120)]))!) }
    check(produced.map { Data($0).map { String(format: "%02x", $0) }.joined() } == refFrames,
          "the encoder reproduces the recorded reference bitstream byte-for-byte")

    let dec = MSBCDecoder()
    var last: [Int16] = []
    for hex in refFrames {
        switch dec.decode(unhex(hex)[...]) {
        case .success(let pcm): last = pcm
        case .failure(let why): check(false, "reference frame decode failed: \(why)")
        }
    }
    var maxDiff = 0
    for i in 0..<min(last.count, refPCM4th.count) { maxDiff = max(maxDiff, abs(Int(last[i]) - Int(refPCM4th[i]))) }
    check(last.count == 120 && maxDiff <= 8,
          "decoded PCM matches the independent fixed-point decoder within \(maxDiff) LSB")
}

section("OCBM client — CT_UPLINK codec byte (7-byte = PCM, 8-byte = mSBC)")
do {
    let t = FakeTransport()
    let c = OCBMClient(transport: t)
    let gate = Mutex<(Bool, UInt32, UInt8, UInt8)?>(nil)
    c.onUplinkGate = { on, rate, ch, codec in gate.withLock { $0 = (on, rate, ch, codec) } }

    func le32(_ v: UInt32) -> [UInt8] { [UInt8(v & 0xFF), UInt8((v >> 8) & 0xFF), UInt8((v >> 16) & 0xFF), UInt8((v >> 24) & 0xFF)] }

    // The pre-2026-09-04 shape. It must keep meaning PCM forever: the box still sends OFF this way.
    t.inject(ctrlFrame([OCBM.ctUplink, 1] + le32(8000) + [1]))
    check(waitUntil(1.0) { gate.withLock { $0 }?.1 == 8000 }, "7-byte CT_UPLINK still parses")
    check(gate.withLock { $0 }?.3 == 0, "a 7-byte CT_UPLINK means codec 0 (PCM)")

    // HFP wideband: 16 kHz mono, mSBC.
    gate.withLock { $0 = nil }
    t.inject(ctrlFrame([OCBM.ctUplink, 1] + le32(16000) + [1, OCBM.seamCodecMsbc]))
    check(waitUntil(1.0) { gate.withLock { $0 } != nil }, "8-byte CT_UPLINK parses")
    let g = gate.withLock { $0 }
    check(g?.0 == true && g?.1 == 16000 && g?.2 == 1 && g?.3 == 4,
          "8-byte CT_UPLINK yields ON / 16000 Hz / 1 ch / codec 4 (mSBC)")

    // OFF carries no format, and must zero the codec so a stale mSBC gate cannot survive a hangup.
    gate.withLock { $0 = nil }
    t.inject(ctrlFrame([OCBM.ctUplink, 0] + le32(0) + [0]))
    check(waitUntil(1.0) { gate.withLock { $0 } != nil }, "OFF gate delivered")
    check(gate.withLock { $0 }?.0 == false && gate.withLock { $0 }?.3 == 0, "OFF zeroes rate/channels/codec")

    // A truncated CT_UPLINK must not index past the payload.
    gate.withLock { $0 = nil }
    t.inject(ctrlFrame([OCBM.ctUplink, 1, 0x80, 0x3E]))
    pump(0.2)
    check(gate.withLock { $0 } == nil, "a truncated CT_UPLINK is ignored, not read past")
}

section("OCBMAVDecrypt — SEAM_FORMAT codec 4 + 60-byte SEAM_PKT_PLAIN (HFP wideband)")
do {
    final class Rec: OCBMAVDelegate {
        let aus = Mutex<[(Data, OCBMAudioStreamFormat?)]>([])
        let formats = Mutex<[OCBMAudioStreamFormat]>([])
        func avDidReceiveVideoConfig(_ config: Data) {}
        func avDidReceiveVideoFrame(_ avcc: Data, keyframe: Bool) {}
        func avDidReceiveAudioFormat(scid: UInt64, format: OCBMAudioStreamFormat) {
            formats.withLock { $0.append(format) }
        }
        func avDidReceiveAudio(_ au: Data, scid: UInt64, format: OCBMAudioStreamFormat?) {
            aus.withLock { $0.append((au, format)) }
        }
        func avDidReceiveAltVideoConfig(_ config: Data) {}
        func avDidReceiveAltVideoFrame(_ avcc: Data, keyframe: Bool) {}
    }
    let seamMagic: [UInt8] = [0x53, 0x45, 0x41, 0x56]
    func le64(_ v: UInt64) -> [UInt8] { withUnsafeBytes(of: v.littleEndian) { Array($0) } }
    func le32(_ v: UInt32) -> [UInt8] { withUnsafeBytes(of: v.littleEndian) { Array($0) } }
    func framed(_ body: [UInt8]) -> [UInt8] {
        let b = seamMagic + body
        let n = UInt32(b.count)
        return [UInt8(n >> 24), UInt8((n >> 16) & 0xFF), UInt8((n >> 8) & 0xFF), UInt8(n & 0xFF)] + b
    }
    let scid: UInt64 = 0x4846_5053_434F_0001 // "HFPSCO\0\1" — the box's fixed SCO scid
    let rec = Rec()
    let dec = OCBMAVDecrypt()
    dec.delegate = rec
    // codec 4, 16 kHz, 1 ch, bits 16 (the DECODED format), audio_type 1 (telephony).
    dec.feedVoice(framed([0x02] + le64(scid) + [OCBM.seamCodecMsbc] + le32(16000) + [1, 16, 1]))
    check(waitUntil(2.0) { rec.formats.withLock { $0.count } == 1 }, "SEAM_FORMAT with codec 4 parses")
    let fmt = rec.formats.withLock { $0.first }
    check(fmt?.codec == 4 && fmt?.isMSBC == true && fmt?.isPCM == false,
          "codec 4 is mSBC, and must NOT be mistaken for PCM (playing the bitstream is noise)")
    check(fmt?.isVoice == true && fmt?.sampleRate == 16000, "audio_type 1 routes to the voice sink at 16 kHz")

    // One 60-byte eSCO read, the shape the box forwards verbatim.
    let pkt: [UInt8] = [0x01, 0x08, MSBC.syncword] + [UInt8](repeating: 0x5A, count: 57)
    dec.feedVoice(framed([0x03] + le64(scid) + pkt))
    check(waitUntil(2.0) { rec.aus.withLock { $0.count } == 1 },
          "a 60-byte SEAM_PKT_PLAIN body passes the length-plausibility gate and is delivered")
    let au = rec.aus.withLock { $0.first }
    check(au?.0.count == 60 && [UInt8](au!.0) == pkt, "the air packet reaches the bridge byte-for-byte")
    check(au?.1?.isMSBC == true && au?.1?.plainLE == true, "the per-AU format copy says mSBC and plain")
}

// MARK: - Summary

print("──────────────────────────────")
print("\(passes) passed, \(failures) failed")
exit(failures == 0 ? 0 : 1)
