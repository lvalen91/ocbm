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
//   • Data LE helpers (App/InputTypes.swift).
//
// Run with:  ./tests/run_tests.sh   (compiles the live sources + this file with
// swiftc and executes — no Xcode project, no dongle, no code signing).
//
// The OCBMClient tests run against real DispatchQueue timers (500 ms HELLO cadence,
// 1 Hz heartbeat), so they wait on wall-clock conditions via `waitUntil` with
// generous timeouts and service the main run loop so main-queue completions flush.
// ──────────────────────────────────────────────────────────────────────────────

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
    let writesBefore = t.writes.count
    c.sendCommand(OCBM.cmdSiriDown) { o in outBox.withLock { $0 = o } }
    check(waitUntil(2.0) {
        if case .droppedNotSubscribed? = outBox.withLock({ $0 }) { return true }
        return false
    }, "sendCommand while SUBSCRIBE never landed → droppedNotSubscribed")
    check(t.writes.count == writesBefore || ctrlCount(t, OCBM.ctSubscribe) >= 2,
          "dropped input did not emit an input frame")
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
    // SEV_HOST_GONE: [ctSessionEvent, 0x00] — 0x00 is neither host-present nor a phone code → GONE.
    t.inject(ctrlFrame([OCBM.ctSessionEvent, 0x00]))
    check(waitUntil(1.0) { subStates.withLock { $0.contains(false) } },
          "SEV_HOST_GONE drops subscribed (onSubscriptionState false)")
    check(waitUntil(3.0) { ctrlCount(t, OCBM.ctSubscribe) >= subsBefore + 1 },
          "SEV_HOST_GONE triggers a fresh SUBSCRIBE")
    c.disconnect()
}

// ════════════════════════════════════════════════════════════════════════════
// Data LE helpers (App/InputTypes.swift)
// ════════════════════════════════════════════════════════════════════════════

section("Data LE helpers")
do {
    var d = Data()
    d.appendLE(UInt32(0xDEADBEEF))
    d.appendLE(UInt32(0x00000001))
    check(d.count == 8, "appendLE writes 4 bytes per value")
    check(d.readLE(at: 0) == 0xDEADBEEF, "readLE round-trip")
    check(d.readLE(at: 4) == 1, "readLE offset")
    check(d.readLE(at: 5) == 0, "readLE out-of-bounds returns 0")
    check(d.readLE(at: 8) == 0, "readLE at end returns 0")
    let sliced = d.dropFirst(4)
    check(sliced.readLE(at: 0) == 1, "readLE is startIndex-relative on slices")
    var fd = Data()
    fd.appendLE(Float32(0.75))
    check(abs(fd.readFloatLE(at: 0) - 0.75) < 0.0001, "appendLE/readFloatLE round-trip")
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
    while t <= 140 { log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.0005), active: true, tMonoMs: t, tWall: 0); t += 10 }
    check(log.counts()["silence"] == 1, "one silence event once the run crosses 120 ms (got \(log.counts()["silence"] ?? 0))")
    check(log.events().contains { $0.kind == .silence && $0.stream == StreamKind.mediaAudio.label },
          "silence event is tagged to the media stream")
}

section("Anomaly — audio SILENCE not flagged while stream is idle (no packets)")
do {
    let log = AVAnomalyLog()
    // active:false ⇒ nothing playing ⇒ never a dropout, even at zero RMS.
    var t = 0.0
    while t <= 300 { log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.0), active: false, tMonoMs: t, tWall: 0); t += 10 }
    check((log.counts()["silence"] ?? 0) == 0, "no silence events when the stream is not active")
}

section("Anomaly — audio CLICK on a discontinuity above the noise floor")
do {
    let log = AVAnomalyLog()
    log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.2, last: 0.1), active: true, tMonoMs: 0, tWall: 0)
    // Big intra-buffer jump (Δ0.8 fs) with real signal present → click.
    log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.2, maxDelta: 0.8, first: 0.1), active: true, tMonoMs: 10, tWall: 0)
    check(log.counts()["click"] == 1, "one click event from the discontinuity")
    // A same-size jump during near-silence is an onset transient, not a click.
    let log2 = AVAnomalyLog()
    log2.recordAudioBuffer(.mediaAudio, adsp(rms: 0.001, maxDelta: 0.9), active: true, tMonoMs: 0, tWall: 0)
    check((log2.counts()["click"] ?? 0) == 0, "no click when the buffer is near-silent (transient, not a click)")
}

section("Anomaly — audio CLIP on sustained full-scale content")
do {
    let log = AVAnomalyLog()
    log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.7, clip: 0.10), active: true, tMonoMs: 0, tWall: 0)
    check(log.counts()["clip"] == 1, "one clip event when ≥2% of samples are at full scale")
}

section("Anomaly — DEBOUNCE collapses a sustained condition to one event")
do {
    let log = AVAnomalyLog()
    // 5 clipping buffers within the clip cooldown (600 ms) → exactly ONE event.
    for i in 0..<5 { log.recordAudioBuffer(.mediaAudio, adsp(rms: 0.7, clip: 0.2), active: true,
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

// MARK: - Annex-B -> AVCC (Android Auto video shim, docs/host/02_ANDROID_AUTO.md)

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

// MARK: - AAWire (Android Auto engine wire layer, docs/host/02_ANDROID_AUTO.md)

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
    check(hex(AAWire.sensorBatchDrivingUnrestricted()) == "6a020800", "SensorBatch driving-unrestricted = 6a 02 08 00")
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

// MARK: - Summary

print("──────────────────────────────")
print("\(passes) passed, \(failures) failed")
exit(failures == 0 ? 0 : 1)
