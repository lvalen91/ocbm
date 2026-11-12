// tests/main.swift — hardware-free tests for the AirPlay Test Host. Run with tests/run_tests.sh.
//
//   wire        MODE_AIRPLAY request bytes (the vectors of crates/ocbm-proto/src/seam.rs), the box's
//               validation rules, HELLO_ACK / MODE_ACK / AIRPLAY_STATE
//   seam        SeamReader / MetaReader reassembly and resync (the same cases as the Rust readers)
//   tunnel      video and audio lanes: keys, the wire seq as nonce, gaps, re-keys, the formats the
//               measurements found (docs/ocbm/02_AIRPLAY_ONLY_MODE.md §4.6)
//   events      META_JSON events, exact 64-bit scids, nothing identifying kept
//   session     the CH_CTRL state machine against a scripted clock
//   playout     AudioPlayoutQueue policies
//   media       real H.264 / AAC (ffmpeg) through tunnel -> decrypt -> VideoToolbox / AudioToolbox,
//               checking pixels and samples
//   end to end  the app's controller against the synthetic box: two sessions, a lost frame, an unplug

import CoreVideo
import Foundation
import ImageIO
import Synchronization

var failures = 0
var passes = 0

@MainActor func check(_ condition: Bool, _ name: String) {
    if condition {
        passes += 1
    } else {
        failures += 1
        print("FAIL  \(name)")
    }
}

@MainActor func section(_ name: String) { print("── \(name)") }

/// Spin the main run loop until `cond` holds or `timeout` elapses.
@MainActor @discardableResult
func waitUntil(_ timeout: Double, _ cond: () -> Bool) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        if cond() { return true }
        RunLoop.main.run(until: Date().addingTimeInterval(0.02))
    }
    return cond()
}

final class Collector<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var items: [T] = []
    func add(_ item: T) { lock.withLock { items.append(item) } }
    var all: [T] { lock.withLock { items } }
    var count: Int { lock.withLock { items.count } }
}

typealias F = TunnelFixture

// MARK: - wire

@MainActor func testWire() {
    section("wire: MODE_AIRPLAY request")
    let r = AirPlayModeRequest(ssid: "My AP", psk: "password1", name: "Car", channel: 36,
                               display: .init(width: 1280, height: 720, maxFPS: 60))
    var want: [UInt8] = [0x03, 0x02, 0x01, 5]
    want += Array("My AP".utf8)
    want += [0x02, 9] + Array("password1".utf8)
    want += [0x03, 3] + Array("Car".utf8)
    want += [0x04, 1, 36]
    want += [0x05, 5, 0x00, 0x05, 0xD0, 0x02, 60]
    check(r.encode() == want, "request bytes are the protocol document's example")
    check(AirPlayModeRequest.decode(want) == r, "request round-trips")
    check(r.validate() == nil, "a valid request validates")

    var minimal: [UInt8] = [0x03, 0x02, 0x7E, 2, 1, 2]
    minimal += [0x02, 8] + Array("12345678".utf8)
    minimal += [0x01, 1, UInt8(ascii: "x")]
    let d = AirPlayModeRequest.decode(minimal)
    check(d?.ssid == "x" && d?.name == "x" && d?.channel == 0 && d?.display == .measuredDefault,
          "name absent -> the SSID, channel absent -> 0, display absent -> 1920x1080@30, unknown tag skipped")

    let plain = AirPlayModeRequest(ssid: "Bench", psk: "12345678")
    let enc = plain.encode()
    var tags: [UInt8] = []
    var i = 2
    while i + 1 < enc.count {
        tags.append(enc[i])
        i += 2 + Int(enc[i + 1])
    }
    check(tags == [0x01, 0x02, 0x05] && AirPlayModeRequest.decode(enc)?.name == "Bench",
          "an empty name and a zero channel send no NAME and no CHANNEL TLV (tags \(tags))")
    check(Array(enc.suffix(7)) == [0x05, 5, 0x80, 0x07, 0x38, 0x04, 30], "the default display TLV is 1920x1080@30")

    func bad(_ f: (inout AirPlayModeRequest) -> Void) -> (AirPlayModeRequest.Invalid?, Bool) {
        var x = r
        f(&x)
        return (x.validate(), AirPlayModeRequest.decode(x.encode()) == nil)
    }
    var v = bad { $0.psk = "short" }
    check(v.0 == .passphrase && v.1, "passphrase < 8 refused by both validate and decode")
    v = bad { $0.psk = String(repeating: "p", count: 64) }
    check(v.0 == .passphrase && v.1, "passphrase > 63 refused")
    v = bad { $0.psk = "pass\nword" }
    check(v.0 == .passphrase && v.1, "passphrase with a control character refused")
    v = bad { $0.psk = "pässword1" }
    check(v.0 == .passphrase && v.1, "non-ASCII passphrase refused")
    v = bad { $0.ssid = "" }
    check(v.0 == .ssid && v.1, "empty SSID refused")
    v = bad { $0.ssid = String(repeating: "s", count: 33) }
    check(v.0 == .ssid && v.1, "33-byte SSID refused")
    v = bad { $0.ssid = "a\nssid=evil" }
    check(v.0 == .ssid && v.1, "SSID with a control character refused")
    v = bad { $0.name = String(repeating: "n", count: 64) }
    check(v.0 == .name && v.1, "64-byte name refused")
    v = bad { $0.channel = 15 }
    check(v.0 == .channel && v.1, "channel 15 refused")
    v = bad { $0.display = .init(width: 0, height: 720, maxFPS: 60) }
    check(v.0 == .display && v.1, "zero-width display refused")
    v = bad { $0.ssid = "caf\u{e9} / & \\ $x `y`" }
    check(v.0 == nil && !v.1, "shell and sed metacharacters in an SSID are data")
    check(AirPlayModeRequest.decode(Array(want.dropLast())) == nil, "truncated TLV refused")
    check(AirPlayModeRequest.decode([0x03, 0x02]) == nil, "no SSID/PSK refused")
    check(!r.summary.contains("password1") && r.summary.contains("9 chars"), "the log summary carries the passphrase's length, never its text")
    for bad in AirPlayModeRequest.Invalid.allCases {
        check(!bad.message.isEmpty, "\(bad) has a message")
    }

    section("wire: display and HEVC flag (TLV 0x05, 0x06)")
    var hv = AirPlayModeRequest(ssid: "Bench", psk: "12345678", display: .init(width: 3840, height: 2160, maxFPS: 60), hevc: true)
    var hvWant: [UInt8] = [0x03, 0x02, 0x01, 5] + Array("Bench".utf8) + [0x02, 8] + Array("12345678".utf8)
    hvWant += [0x05, 5, 0x00, 0x0F, 0x70, 0x08, 60]
    hvWant += [0x06, 1, 0x01]
    check(hv.encode() == hvWant, "3840x2160@60 with HEVC: TLV 05 05 00 0F 70 08 3C then 06 01 01")
    check(AirPlayModeRequest.decode(hvWant) == AirPlayModeRequest(ssid: "Bench", psk: "12345678", name: "Bench",
                                                                   display: .init(width: 3840, height: 2160, maxFPS: 60), hevc: true),
          "the HEVC request round-trips")
    check(!tags.contains(0x06) && enc.suffix(7).first == 0x05, "HEVC off sends no flags TLV (an older box sees the same bytes as before)")
    check(AirPlayModeRequest.decode(enc + [0x06, 1, 0x00])?.hevc == false, "flags 0x00: HEVC off")
    check(AirPlayModeRequest.decode(enc + [0x06, 1, 0x02]) == nil, "a reserved flag bit refuses the request")
    check(AirPlayModeRequest.decode(enc + [0x06, 2, 0x01, 0x00]) == nil, "a flags TLV of the wrong length refuses the request")
    check(AirPlayModeRequest.decode(enc + [0x06, 0]) == nil, "an empty flags TLV refuses the request")
    let d120 = AirPlayModeRequest.Display(width: 3840, height: 2160, maxFPS: 120)
    hv.display = d120
    check(hv.validate() == nil && AirPlayModeRequest.decode(hv.encode())?.display == d120, "3840x2160@120 is valid")
    for (w, h, f, ok) in [(320, 320, 1, true), (7680, 7680, 120, true), (319, 1080, 30, false), (1920, 319, 30, false),
                          (7681, 2160, 60, false), (1920, 7681, 60, false), (1920, 1080, 0, false), (1920, 1080, 121, false)] {
        var x = hv
        x.display = .init(width: UInt16(w), height: UInt16(h), maxFPS: UInt8(f))
        let both = (x.validate() == nil, AirPlayModeRequest.decode(x.encode()) != nil)
        check(both == (ok, ok), "display \(w)x\(h)@\(f) is \(ok ? "valid" : "refused") by validate and decode")
    }
    check(hv.summary.contains("3840x2160@120") && hv.summary.contains("HEVC allowed") && !hv.summary.contains("12345678"),
          "the log summary shows display and HEVC, never the passphrase")
    check(AirPlayModeRequest.Display(text: "3840x2160@60") == .init(width: 3840, height: 2160, maxFPS: 60)
          && AirPlayModeRequest.Display(text: "4k") == nil && AirPlayModeRequest.Display.presets.map(\.text)
          == ["1920x1080@30", "3840x2160@60", "3840x2160@120"], "display text form and the menu presets")
    check(!AirPlayModeRequest.Display.measuredDefault.isAbove1080p && AirPlayModeRequest.Display.presets[1].isAbove1080p,
          "1080p is not above 1080p; 4K is")

    section("wire: the form remembers display and HEVC, never the passphrase")
    let suite = "airplay-test-host-tests-\(getpid())"
    if let defaults = UserDefaults(suiteName: suite) {
        let form = FormModel(defaults: defaults)
        check(form.display == .measuredDefault && form.hevc == false, "defaults: 1920x1080@30, HEVC off")
        form.ssid = "Bench"
        form.display = .init(width: 3840, height: 2160, maxFPS: 60)
        form.hevc = true
        if case .success(let req) = form.request(passphrase: "secret-pass-1") {
            check(req.display.text == "3840x2160@60" && req.hevc && req.encode().suffix(3) == [0x06, 1, 1], "the form's request carries both")
        } else {
            check(false, "the form makes a request")
        }
        form.remember()
        let again = FormModel(defaults: defaults)
        check(again.display.text == "3840x2160@60" && again.hevc, "remembered across launches")
        let stored = defaults.dictionaryRepresentation().values.map { "\($0)" }.joined(separator: " ")
        check(!stored.contains("secret-pass-1"), "the passphrase is not in UserDefaults")
        UserDefaults.standard.removePersistentDomain(forName: suite)
    } else {
        check(false, "a scratch UserDefaults suite")
    }

    section("wire: box answers")
    var ack = [UInt8](repeating: 0, count: 42)
    ack[0] = OCBM.ctHelloAck
    ack[1] = 1
    ack[2] = 0x7F
    ack[6] = OCBM.modeAirplay
    ack[7] = 0xAB
    ack[39] = OCBM.bundleOk
    ack[40] = OCBM.airplayApUp
    ack[41] = OCBM.airplaySessionWaiting
    let full = AirPlayWire.parseHelloAck(ack)
    check(full?.caps == 0x7F && full?.supportsAirPlay == true && full?.activeMode == OCBM.modeAirplay, "HELLO_ACK caps and mode")
    check(full?.bundle?.state == OCBM.bundleOk && full?.bundle?.contentIDHex.hasPrefix("ab00") == true, "HELLO_ACK bundle tail")
    check(full?.airplayAP == OCBM.airplayApUp && full?.airplaySession == OCBM.airplaySessionWaiting, "HELLO_ACK mode tail")
    let forty = AirPlayWire.parseHelloAck(Array(ack[..<40]))
    check(forty?.bundle != nil && forty?.airplayAP == nil, "a 40-byte ACK has a bundle and no mode tail")
    let seven = AirPlayWire.parseHelloAck([OCBM.ctHelloAck, 1, 0x3F, 0, 0, 0, 0])
    check(seven?.bundle == nil && seven?.supportsAirPlay == false, "a 7-byte ACK from an old box still parses; no AirPlay bit")
    check(AirPlayWire.parseHelloAck([OCBM.ctHelloAck, 1, 0]) == nil, "a short ACK is not an ACK")
    let ma = AirPlayWire.parseModeAck([OCBM.ctModeAck, OCBM.modeAirplay, OCBM.modeAckBadParams])
    check(ma?.mode == OCBM.modeAirplay && ma?.status == OCBM.modeAckBadParams, "MODE_ACK")
    let st = AirPlayWire.parseState([OCBM.ctAirplayState, 2, 3, 1])
    check(st == AirPlayBoxState(ap: 2, session: 3, lastEnd: 1) && st?.apName == "up" && st?.sessionName == "streaming"
          && st?.lastEndName == "teardown" && st?.isModeLeft == false, "AIRPLAY_STATE and its names")
    check(AirPlayWire.parseState([OCBM.ctAirplayState, 0, 0, 0])?.isModeLeft == true, "00 00 00 = the mode was left")
    check((OCBM.ctModeAck, OCBM.ctAirplayState, OCBM.modeAirplay, OCBM.capAirplay) == (0x1D, 0x1E, 0x02, 0x40), "constants")
}

extension AirPlayModeRequest.Invalid: CaseIterable {
    static var allCases: [AirPlayModeRequest.Invalid] { [.ssid, .passphrase, .name, .channel, .display] }
}

// MARK: - seam

@MainActor func testSeam() {
    section("seam: message bytes")
    let key = [UInt8]((0..<32).map { UInt8($0) })
    let km = F.keyMessage(key: key, scid: 0x1122_3344_5566_7788)
    check(km.count == 49 && Array(km[0..<4]) == [0, 0, 0, 45] && Array(km[4..<8]) == Array("SEAV".utf8) && km[8] == 0
          && Array(km[9..<41]) == key && Array(km[41..<49]) == [0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11],
          "key message: len 45, magic, marker 0, key, scid LE trailing the key")
    let fm = F.audioFormat(scid: 0x0102_0304_0506_0708, rate: 44100, channels: 2)
    check(fm.count == 25 && Array(fm[0..<4]) == [0, 0, 0, 21] && fm[8] == OCBM.seamFormat
          && Array(fm[9..<17]) == [8, 7, 6, 5, 4, 3, 2, 1] && fm[17] == 1 && Array(fm[18..<22]) == [0x44, 0xAC, 0, 0]
          && Array(fm[22..<25]) == [2, 0, 0], "format message bytes")
    check(F.meta(OCBM.metaJson, Array("{}".utf8)) == [0x4D, 0x45, 0x54, 0x41, 0, 0, 0, 3, 0x02, 0x7B, 0x7D], "meta message bytes")

    section("seam: reassembly and resync")
    let hdr = [UInt8](repeating: 0, count: 128)
    func frame(_ seq: UInt64, _ body: [UInt8]) -> [UInt8] { F.seam(0x01, F.le64(seq) + hdr + body) }
    var stream = F.keyMessage(key: [UInt8](repeating: 9, count: 32), scid: 5)
    stream += frame(0, Array("abcd".utf8))
    // A torn message: the head of a frame whose body never arrives. Its length swallows what follows.
    stream += frame(1, [UInt8](repeating: 0, count: 4000)).prefix(60)
    for seq in 2..<60 { stream += frame(UInt64(seq), Array("xy".utf8)) }
    for chunk in [1, 7, 64, 4096] {
        var r = SeamReader(max: 1 << 20)
        var got: [SeamMessage] = []
        var at = 0
        while at < stream.count {
            let n = min(chunk, stream.count - at)
            r.push(Array(stream[at..<(at + n)]), newSource: false)
            while let m = r.next() { got.append(m) }
            at += n
        }
        let ok = got.count > 23 && got[0].marker == 0 && got[0].body.count == 40
            && Array(got[1].body[136...]) == Array("abcd".utf8) && r.resyncs >= 1
            && OCBM.readLE64(got[2].body, 0) == 1 && got[2].body.count == 8 + 128 + 4000
        check(ok, "chunk \(chunk): key, frame, then the torn message once (its AEAD would reject it)")
        let tail = got.dropFirst(3).map { OCBM.readLE64($0.body, 0) }
        check(tail.count >= 20 && tail.last == 59 && zip(tail, tail.dropFirst()).allSatisfy { $1 == $0 + 1 }
              && got.dropFirst(3).allSatisfy { $0.marker == 1 && Array($0.body[136...]) == Array("xy".utf8) },
              "chunk \(chunk): every message after the gap is whole and in order (\(tail.count) of them)")
    }
    var r = SeamReader(max: 1 << 16)
    r.push(Array(F.seam(OCBM.seamPkt, F.le64(1) + [UInt8](repeating: 0, count: 100)).prefix(17)), newSource: false)
    check(r.next() == nil, "a head alone is not a message")
    r.push(F.seam(OCBM.seamPkt, F.le64(2) + [1, 2, 3]), newSource: true)
    let m = r.next()
    check(m?.marker == OCBM.seamPkt && m.map { OCBM.readLE64($0.body, 0) } == 2 && m.map { Array($0.body[8...]) } == [1, 2, 3]
          && r.resyncs == 0, "F_NEW_SOURCE drops the old producer's partial message without a resync")
    var big = SeamReader(max: 64)
    big.push(F.seam(0x01, [UInt8](repeating: 7, count: 100)) + F.seam(0x02, [1]), newSource: false)
    check(big.next() == SeamMessage(marker: 2, body: [1]) && big.resyncs >= 1, "a message over the lane's limit is skipped")

    var meta = MetaReader()
    var s = Array("junk".utf8) + F.meta(OCBM.metaJson, Array("{\"kind\":\"airplay\"}".utf8))
    s += F.meta(OCBM.metaArtwork, [0, 0xFF, 0xD8])
    var got: [SeamMessage] = []
    var at = 0
    while at < s.count {
        let n = min(5, s.count - at)
        meta.push(Array(s[at..<(at + n)]), newSource: false)
        while let m = meta.next() { got.append(m) }
        at += n
    }
    check(got == [SeamMessage(marker: OCBM.metaJson, body: Array("{\"kind\":\"airplay\"}".utf8)),
                  SeamMessage(marker: OCBM.metaArtwork, body: [0, 0xFF, 0xD8])] && meta.resyncs >= 1,
          "MetaReader: junk skipped, both messages whole across 5-byte chunks")
}

// MARK: - tunnel

func nal(_ byte: UInt8, _ n: Int) -> [UInt8] { F.be32(UInt32(n)) + [UInt8](repeating: byte, count: n) }

@MainActor func testVideoLane() {
    section("tunnel: video lane")
    let key = [UInt8](repeating: 0x11, count: 32)
    let avcc: [UInt8] = [1, 100, 0, 31, 0xFF, 0xE1, 0, 4, 0x67, 0x64, 0, 31, 1, 0, 2, 0x68, 0xEE]
    let f0 = nal(0x65, 900) + nal(0x06, 10) // an IDR slice and an SEI in one frame
    let f1 = nal(0x41, 300)
    let f2 = nal(0x41, 200)
    let scid: UInt64 = 0xF000_0000_0000_002A
    var lane = F.videoFrame(key: key, seq: 0, opcode: 0, plain: f1) // before any key
    lane += F.keyMessage(key: key, scid: scid)
    lane += F.videoFrame(key: key, seq: 0, opcode: 1, plain: avcc, timestamp: 1000 << 32, geometry: F.Geometry())
    lane += F.videoFrame(key: key, seq: 0, opcode: 0, flags: 0x10, plain: f0)
    lane += F.videoFrame(key: key, seq: 1, opcode: 5, plain: Array("bplist00 statistics, plaintext".utf8))
    lane += F.videoFrame(key: key, seq: 1, opcode: 0, plain: f1)
    // frames 2..9 were lost on the way: the next one still decrypts, because the nonce is its own seq
    lane += F.videoFrame(key: key, seq: 10, opcode: 0, plain: f2)
    // a frame under another key fails its tag
    lane += F.videoFrame(key: [UInt8](repeating: 0x22, count: 32), seq: 11, opcode: 0, plain: f2)
    lane += F.keyMessage(key: key, scid: scid) // the box's seam reconnected: same key again
    lane += F.videoFrame(key: key, seq: 12, opcode: 0, plain: f2)

    for chunk in [lane.count, 61, 1] {
        var v = AirPlayVideoLane()
        var frames: [(UInt64, [UInt8], Bool)] = []
        var keys: [(UInt64, Bool)] = []
        var gaps: [(UInt64, UInt64)] = []
        var bad: [(UInt64, Bool)] = []
        var config: (record: [UInt8], header: ScreenHeader)?
        var at = 0
        while at < lane.count {
            let n = min(chunk, lane.count - at)
            v.feed(Array(lane[at..<(at + n)]), newSource: false) { ev in
                switch ev {
                case .key(let s, let fresh): keys.append((s, fresh))
                case .config(let a, let h): config = (a, h)
                case .frame(let seq, let data, let h): frames.append((seq, Array(data), h.isKeyframeFlagged))
                case .gap(let e, let g): gaps.append((e, g))
                case .undecryptable(let seq, let haveKey): bad.append((seq, haveKey))
                }
            }
            at += n
        }
        let tag = "chunk \(chunk)"
        check(keys.count == 2 && keys[0] == (scid, true) && keys[1] == (scid, false),
              "\(tag): first key is a new stream (scid above 2^63 intact), the same key again is not")
        check(config?.record == avcc, "\(tag): opcode 1 delivers the avcC verbatim")
        check(config?.header.sourceSizeText == "498x1080" && config?.header.outputSizeText == "498x1080"
              && config?.header.offsetX == 711 && config?.header.timestampSeconds == 1000, "\(tag): codec frame header floats and timestamp")
        check(frames.count == 4 && frames[0].0 == 0 && frames[0].1 == f0 && frames[0].2 && frames[1].1 == f1
              && !frames[1].2 && frames[2].0 == 10 && frames[2].1 == f2 && frames[3].0 == 12,
              "\(tag): frames decrypt with the WIRE seq as nonce, also after a gap; the IDR flag is read")
        check(gaps.count == 1 && gaps[0] == (2, 10), "\(tag): the seq jump is reported once, with what was expected (a frame that fails its tag still advances seq)")
        check(bad.count == 2 && bad[0] == (0, false) && bad[1] == (11, true),
              "\(tag): a frame before the key and a frame under another key do not decrypt")
        let s = v.stats
        check((s.decrypted, s.failed, s.noKey, s.flaggedIDR, s.configs) == (4, 1, 1, 1, 1)
              && (s.other, s.gaps, s.framesLost, s.streams, s.keyMessages) == (1, 1, 8, 1, 2), "\(tag): tallies \(s)")
    }

    var v = AirPlayVideoLane()
    let k2 = [UInt8](repeating: 0x33, count: 32)
    var events: [String] = []
    let second = F.keyMessage(key: key, scid: 1) + F.videoFrame(key: key, seq: 0, opcode: 0, plain: f1)
        + F.videoFrame(key: key, seq: 1, opcode: 0, plain: f1)
        + F.keyMessage(key: k2, scid: 2) + F.videoFrame(key: k2, seq: 0, opcode: 0, plain: f2)
        + F.videoFrame(key: key, seq: 1, opcode: 0, plain: f1)
    v.feed(second, newSource: false) { ev in
        switch ev {
        case .key(let s, let fresh): events.append("key \(s) \(fresh)")
        case .frame(let seq, _, _): events.append("frame \(seq)")
        case .gap: events.append("gap")
        case .undecryptable(let seq, _): events.append("bad \(seq)")
        case .config: events.append("config")
        }
    }
    check(events == ["key 1 true", "frame 0", "frame 1", "key 2 true", "frame 0", "bad 1"],
          "a re-key starts seq over with no gap, and the old key no longer opens anything: \(events)")
    v.reset()
    var after = 0
    v.feed(F.videoFrame(key: k2, seq: 0, opcode: 0, plain: f2), newSource: false) { ev in
        if case .undecryptable(_, false) = ev { after += 1 }
    }
    check(after == 1 && v.stats.streams == 0, "reset forgets the key")
}

@MainActor func testAudioLane() {
    section("tunnel: audio lane")
    let k1 = [UInt8](repeating: 1, count: 32), k2 = [UInt8](repeating: 2, count: 32), shk = [UInt8](repeating: 3, count: 32)
    var lane = F.audioPacket(key: k1, scid: 7, kind: .realtime, sequence: 0, rtpTime: 0, nonce: 1, au: Array("dropped: no key yet".utf8))
    lane += F.keyMessage(key: k1, scid: 7)
    lane += F.audioFormat(scid: 7, rate: 44100, channels: 2)
    lane += F.audioPacket(key: k1, scid: 7, kind: .realtime, sequence: 65535, rtpTime: 1024, nonce: 2, au: [UInt8](repeating: 0xA1, count: 300))
    lane += F.audioPacket(key: k1, scid: 7, kind: .realtime, sequence: 0, rtpTime: 2048, nonce: 3, au: []) // the 0-byte unit seen in the measurements
    lane += F.keyMessage(key: k1, scid: 7) // seam reconnect: the same key again
    // mid-session: the phone sets a new realtime stream up (new scid, new key), then buffered audio
    lane += F.keyMessage(key: k2, scid: 8)
    lane += F.audioFormat(scid: 8, rate: 48000, channels: 2)
    lane += F.audioPacket(key: k2, scid: 8, kind: .realtime, sequence: 1, rtpTime: 0, nonce: 9, au: [UInt8](repeating: 0xB2, count: 6))
    lane += F.audioPacket(key: k1, scid: 8, kind: .realtime, sequence: 2, rtpTime: 1024, nonce: 10, au: [UInt8](repeating: 0xEE, count: 6)) // the OLD key on the new stream
    lane += F.keyMessage(key: shk, scid: 9)
    lane += F.audioPacket(key: shk, scid: 9, kind: .buffered, sequence: 0x60_0001, rtpTime: 4096, nonce: 1, au: [UInt8](repeating: 0xC3, count: 2000))

    var a = AirPlayAudioLane()
    var keys: [(UInt64, Bool, UInt64)] = []
    var formats: [(UInt64, AirPlayAudioFormat)] = []
    var packets: [(scid: UInt64, gen: UInt64, au: [UInt8], head: UInt32, ts: UInt32, kind: AirPlayAudioKind)] = []
    var at = 0
    while at < lane.count {
        let n = min(97, lane.count - at)
        a.feed(Array(lane[at..<(at + n)]), newSource: false) { ev in
            switch ev {
            case .key(let s, let fresh, let g): keys.append((s, fresh, g))
            case .format(let s, let f): formats.append((s, f))
            case .packet(let s, let g, let au, let head, let ts, let kind): packets.append((s, g, Array(au), head, ts, kind))
            }
        }
        at += n
    }
    let g = keys.map(\.2)
    check(keys.map { "\($0.0) \($0.1)" } == ["7 true", "7 false", "8 true", "9 true"] && g[1] == g[0] && g[2] > g[1] && g[3] > g[2],
          "each new key raises the generation; the same key again does not")
    check(formats.count == 2 && formats[0].1 == AirPlayAudioFormat(codec: 1, sampleRate: 44100, channels: 2)
          && formats[1].1.sampleRate == 48000 && a.format(of: 9) == nil, "formats per scid; none yet for the shk stream")
    check(packets.count == 3 && packets[0].au == [UInt8](repeating: 0xA1, count: 300) && packets[1].au.count == 6
          && packets[2].au.count == 2000, "three access units out: per-stream keys, the empty unit and the wrong-key one dropped")
    check(packets[0].kind == .realtime && packets[0].kind.sequence(rtpHead: packets[0].head) == 65535 && packets[0].ts == 1024,
          "type 96: PT 96, SSRC 0, 16-bit sequence")
    check(packets[2].kind == .buffered && packets[2].kind.sequence(rtpHead: packets[2].head) == 0x60_0001 && packets[2].gen == g[3],
          "type 103: SSRC 0x16000000, 24-bit sequence (whose top byte may be 0x60 = PT 96)")
    let st = Dictionary(uniqueKeysWithValues: a.stats.map { ($0.scid, $0) })
    check(st[7].map { ($0.decrypted, $0.noKey, $0.empty, $0.failed) } ?? (0, 0, 0, 0) == (1, 1, 1, 0), "scid 7 tallies")
    check(st[8].map { ($0.decrypted, $0.failed) } ?? (0, 0) == (1, 1) && st[9]?.decrypted == 1, "scid 8 and 9 tallies")
    check(AirPlayAudioKind.classify(rtp: [0x80, 96, 0, 1, 0, 0, 0, 0, 0x16, 0, 0, 0][...], declaredType: 96) == .realtime
          && AirPlayAudioKind.classify(rtp: [0x80, 96, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0][...], declaredType: 103) == .buffered,
          "the SETUP type from the stream event overrides what the packet looks like")

    var many = AirPlayAudioLane()
    for i in 0..<12 { many.feed(F.keyMessage(key: [UInt8](repeating: UInt8(i + 1), count: 32), scid: UInt64(100 + i)), newSource: false) { _ in } }
    check(many.stats.count == AirPlayAudioLane.maxStreams && many.stats.first?.scid == 104, "old streams are forgotten, oldest first")
}

// MARK: - events

@MainActor func testEvents() {
    section("events")
    func parse(_ ev: String, _ fields: String) -> AirPlayEvent? {
        AirPlayEvent.parse(Array(F.event(ev, fields).dropFirst(9)))
    }
    let big: UInt64 = 0xF123_4567_89AB_CDEF
    let start = parse("stream", "\"state\":\"start\",\"lane\":\"video\",\"scid\":\(big),\"setup\":{\"type\":110,\"latencyMs\":100,\"streamConnectionID\":\(Int64(bitPattern: big))}")
    if case .streamStart(let lane, let scid, let type, let detail)? = start {
        check(lane == "video" && scid == big && type == 110 && detail == "latencyMs 100", "stream start: scid above 2^63 is exact; type from the SETUP")
    } else {
        check(false, "stream start parses")
    }
    let stop = parse("stream", "\"state\":\"stop\",\"type\":96,\"scid\":9601,\"reason\":\"teardown\",\"messages\":20504,\"bytes\":123456,\"dropped\":2")
    check(stop == .streamStop(type: 96, scid: 9601, reason: "teardown", messages: 20504, bytes: 123456, dropped: 2), "stream stop")
    check(parse("stream", "\"state\":\"unsupported\",\"setup\":{\"type\":120}") == .streamUnsupported(type: 120), "stream unsupported")
    check(parse("session", "\"state\":\"streaming\",\"reason\":\"none\"") == .session(state: "streaming", reason: "none"), "session")
    let sender = parse("sender", "\"name\":\"Phone\",\"model\":\"iPhone19,2\",\"osName\":\"iPhone OS\",\"osVersion\":\"27.3\","
        + "\"deviceID\":\"AA:BB:CC:DD:EE:FF\",\"timing\":\"NTP\",\"mirroring\":true,\"address\":\"192.0.2.7\"")
    if case .sender(let s)? = sender {
        check(s.name == "Phone" && s.model == "iPhone19,2" && s.timing == "NTP" && s.mirroring, "sender")
        let line = sender?.logLine ?? ""
        check(!line.contains("AA:BB") && !line.contains("192.0.2.7") && !"\(s)".contains("AA:BB"), "the sender's deviceID and address are not kept")
    } else {
        check(false, "sender parses")
    }
    check(parse("volume", "\"db\":-17.5") == .volume(db: -17.5), "volume")
    check(parse("progress", "\"start\":1,\"current\":4294967295,\"end\":3") == .progress(start: 1, current: 4_294_967_295, end: 3), "progress (RTP times)")
    check(parse("track", "\"title\":\"A \\\"quoted\\\" title\",\"artist\":\"B\",\"durationMs\":187000")
          == .track(.init(title: "A \"quoted\" title", artist: "B", album: nil, genre: nil, durationMs: 187000)), "track, with only the members present")
    check(parse("artwork", "\"bytes\":0") == .artwork(bytes: 0), "artwork cleared")
    check(parse("anchor", "\"rate\":0,\"rtpTime\":88200,\"networkTimeSecs\":5") == .anchor(rate: 0, rtpTime: 88200), "anchor")
    check(parse("flush", "\"flushUntilSeq\":70123,\"flushUntilTS\":1000")
          == .flush(.init(untilSeq: 70123, untilTS: 1000, fromSeq: nil, fromTS: nil)), "flush")
    check(parse("sync", "\"type\":96,\"scid\":\(big),\"first\":true") == .sync(scid: big) && parse("sync", "\"scid\":1")?.logLine == nil, "sync, never logged")
    check(parse("somethingNew", "\"x\":1") == .other(name: "somethingNew"), "an unknown event is kept by name")
    check(AirPlayEvent.parse(Array("{\"kind\":\"carplay\",\"ev\":\"session\"}".utf8)) == nil
          && AirPlayEvent.parse(Array("not json".utf8)) == nil, "other kinds and non-JSON are not events")
    check(AirPlayVolume.sliderPosition(db: 0) == 1 && AirPlayVolume.sliderPosition(db: -30) == 0
          && AirPlayVolume.sliderPosition(db: -144) == 0 && abs(AirPlayVolume.sliderPosition(db: -15) - 0.5) < 1e-9, "volume scale: 0 full, -30 zero, -144 mute")
}

// MARK: - session state machine

typealias Out = AirPlayModeSession.Output

func sends(_ out: [Out]) -> [[UInt8]] {
    out.compactMap { if case .send(let p) = $0 { return p } else { return nil } }
}

func phases(_ out: [Out]) -> [AirPlayModeSession.Phase] {
    out.compactMap { if case .phase(let p) = $0 { return p } else { return nil } }
}

func helloAck(mode: UInt8 = OCBM.modeProjection, caps: UInt32 = 0x7F) -> [UInt8] {
    var a = [UInt8](repeating: 0, count: 42)
    a[0] = OCBM.ctHelloAck
    a[1] = 1
    a.replaceSubrange(2..<6, with: F.le32(caps))
    a[6] = mode
    return a
}

@MainActor func testSession() {
    section("session: start")
    let req = AirPlayModeRequest(ssid: "Bench", psk: "12345678", name: "Car")
    var all: [[UInt8]] = []
    var s = AirPlayModeSession(nonce: 0xDEAD_BEEF, wantBoxLog: true)
    var out = s.start(req, now: 0)
    all += sends(out)
    check(sends(out) == [[OCBM.ctHello, 1, 0xEF, 0xBE, 0xAD, 0xDE]] && phases(out) == [.hello], "start: HELLO at once, with the instance nonce")
    check(s.tick(now: 0.3).isEmpty, "no second HELLO before 500 ms")
    out = s.tick(now: 0.5)
    all += sends(out)
    check(sends(out).count == 1 && sends(out)[0][0] == OCBM.ctHello, "HELLO is retransmitted (a booting box loses the first)")
    var t = 0.5
    var notReady = 0
    var hellos = 2
    while t < 17 {
        t += 0.1
        let o = s.tick(now: t)
        hellos += sends(o).count
        notReady += o.filter { $0 == .boxNotReady }.count
    }
    check(notReady == 1 && hellos > 30 && hellos < 36, "after ~15 s: 'box not ready' once, then HELLO every 2 s (\(hellos) sent)")
    check(s.control([OCBM.ctAirplayState, 0, 0, 0], now: t).contains(.state(AirPlayBoxState())) && s.phase == .hello,
          "a stale 00 00 00 before the request ends nothing")

    out = s.control(helloAck(), now: 20)
    all += sends(out)
    check(sends(out) == [req.encode()] && s.phase == .awaitingAck, "HELLO_ACK: the request goes out, exactly as encoded")
    check(out.contains { if case .helloAck = $0 { return true } else { return false } }, "HELLO_ACK is reported")
    check(sends(s.tick(now: 20.9)).isEmpty, "no heartbeat before one interval has passed")
    out = s.tick(now: 21.0)
    all += sends(out)
    check(sends(out) == [[OCBM.ctHeartbeat]], "heartbeat one second after the request")
    check(s.control([OCBM.ctSessionEvent, OCBM.sevHostGone], now: 21.1).isEmpty, "SEV_HOST_GONE from the projection session our request ended is ignored")
    check(s.control([OCBM.ctAirplayState, 0, 0, 0], now: 21.2).count == 1 && s.phase == .awaitingAck, "00 00 00 before the ack is stale")
    out = s.control([OCBM.ctModeAck, OCBM.modeAirplay, OCBM.modeAckOk], now: 21.3)
    all += sends(out)
    check(s.phase == .active && sends(out) == [OCBM.logCtl(enabled: true, capKB: 0)], "MODE_ACK ok: active, and the box log is armed AFTER the ack")
    out = s.control([OCBM.ctAirplayState, 1, 0, 0], now: 21.4)
    check(out == [.state(AirPlayBoxState(ap: 1, session: 0, lastEnd: 0))] && s.phase == .active, "ap=starting is a state, not an exit")

    var beats = 0
    t = 21.4
    while t < 31.4 {
        t += 0.1
        let o = sends(s.tick(now: t))
        all += o
        beats += o.filter { $0 == [OCBM.ctHeartbeat] }.count
    }
    check(beats == 10, "ten heartbeats in ten seconds (\(beats))")
    let late = sends(s.tick(now: 40))
    check(late.count == 1 && sends(s.tick(now: 40.2)).isEmpty, "a tick that comes late sends one beat, not a burst")

    section("session: the box forgets, the box leaves")
    check(s.control([OCBM.ctSessionEvent, OCBM.sevHostGone], now: 22.0).isEmpty, "SEV_HOST_GONE within 2 s of the ack: ignored")
    out = s.control([OCBM.ctSessionEvent, OCBM.sevHostGone], now: 45)
    all += sends(out)
    check(sends(out) == [req.encode()] && out.contains(.reRequested) && s.phase == .awaitingAck,
          "SEV_HOST_GONE while the mode is held (ocbmd restarted): the mode is requested again and keys are void")
    _ = s.control([OCBM.ctModeAck, OCBM.modeAirplay, OCBM.modeAckOk], now: 45.2)
    out = s.control([OCBM.ctAirplayState, 0, 0, 0], now: 50)
    check(s.phase == .ended(.boxLeft) && phases(out) == [.ended(.boxLeft)], "00 00 00 while active: the box left the mode")
    check(s.tick(now: 60).isEmpty && s.stop(now: 60).isEmpty, "an ended session sends nothing more")
    check(!all.contains { $0.first == OCBM.ctSubscribe }, "no CT_SUBSCRIBE is ever sent (it would end the mode)")

    section("session: refusals")
    for status in [OCBM.modeAckBadParams, OCBM.modeAckUnsupported] {
        var r = AirPlayModeSession(nonce: 1, wantBoxLog: false)
        _ = r.start(req, now: 0)
        _ = r.control(helloAck(caps: 0x3F), now: 0.1)
        let o = r.control([OCBM.ctModeAck, OCBM.modeAirplay, status], now: 0.2)
        check(r.phase == .ended(.refused(status)) && sends(o).isEmpty && sends(r.tick(now: 5)).isEmpty,
              "MODE_ACK status \(status): ended, no heartbeat follows")
    }
    var n = AirPlayModeSession(nonce: 1, wantBoxLog: false)
    _ = n.start(req, now: 0)
    let noCap = n.control(helloAck(caps: 0x3F), now: 0.1)
    check(sends(noCap) == [req.encode()] && noCap.contains { if case .note = $0 { return true } else { return false } },
          "a box without CAP_AIRPLAY is still asked (and the missing bit is noted)")
    _ = n.tick(now: 3.9)
    check(n.phase == .awaitingAck, "still waiting just before the timeout")
    _ = n.tick(now: 4.2)
    check(n.phase == .ended(.noAck), "no MODE_ACK within 4 s: an ocbmd without MODE_AIRPLAY")

    section("session: stop")
    var a = AirPlayModeSession(nonce: 1, wantBoxLog: false)
    _ = a.start(req, now: 0)
    _ = a.control(helloAck(), now: 0.1)
    _ = a.control([OCBM.ctModeAck, OCBM.modeAirplay, OCBM.modeAckOk], now: 0.2)
    out = a.stop(now: 5)
    check(sends(out) == [[OCBM.ctModeSelect, OCBM.modeProjection]] && a.phase == .leaving, "stop: CT_MODE_SELECT [MODE_PROJECTION]")
    check(sends(a.tick(now: 6.0)).isEmpty, "no heartbeat once the host asked to leave")
    check(a.control([OCBM.ctAirplayState, 0, 0, 0], now: 6.1).count == 1 && a.phase == .leaving, "the 00 00 00 that precedes the ack is expected")
    out = a.control([OCBM.ctModeAck, OCBM.modeProjection, OCBM.modeAckOk], now: 6.2)
    check(a.phase == .ended(.stopped) && sends(out).isEmpty, "the projection ack ends it; CT_STOP is not needed")

    var b = AirPlayModeSession(nonce: 1, wantBoxLog: false)
    _ = b.start(req, now: 0)
    _ = b.control(helloAck(), now: 0.1)
    _ = b.stop(now: 1)
    check(sends(b.tick(now: 2.0)).isEmpty && b.phase == .leaving, "waiting for the ack")
    out = b.tick(now: 2.6)
    check(sends(out) == [[OCBM.ctStop]] && b.phase == .ended(.stopped), "no ack within 1.5 s: CT_STOP, then ended")

    var c = AirPlayModeSession(nonce: 1, wantBoxLog: false)
    _ = c.start(req, now: 0)
    out = c.stop(now: 0.2)
    check(sends(out).isEmpty && c.phase == .ended(.stopped), "stop before the box answered HELLO: nothing was requested, nothing to undo")
}

// MARK: - playout queue

@MainActor func testPlayout() {
    section("playout: sequence ranges")
    let m16: UInt32 = 1 << 16, m24: UInt32 = 1 << 24
    check(SequenceRange.contains(5, from: nil, until: 10, modulus: m24) && !SequenceRange.contains(10, from: nil, until: 10, modulus: m24)
          && !SequenceRange.contains(11, from: nil, until: 10, modulus: m24), "before 'until', exclusive")
    check(SequenceRange.contains(65530, from: nil, until: 4, modulus: m16) && !SequenceRange.contains(4, from: nil, until: 65530, modulus: m16),
          "across the wrap")
    check(SequenceRange.contains(7, from: 5, until: 10, modulus: m24) && !SequenceRange.contains(4, from: 5, until: 10, modulus: m24)
          && SequenceRange.contains(5, from: 5, until: 10, modulus: m24) && !SequenceRange.contains(10, from: 5, until: 10, modulus: m24),
          "with 'from': [from, until)")

    section("playout: realtime policy")
    func tone(_ frames: Int, _ value: Int16 = 1000) -> [Int16] { [Int16](repeating: value, count: frames * 2) }
    let ms: UInt64 = 1_000_000
    var l = [Float](repeating: 9, count: 4096), r = [Float](repeating: 9, count: 4096)
    func render(_ q: AudioPlayoutQueue, _ frames: Int, _ now: UInt64) -> Int {
        l.withUnsafeMutableBufferPointer { lp in
            r.withUnsafeMutableBufferPointer { rp in q.render(frames: frames, left: lp.baseAddress!, right: rp.baseAddress!, nowNs: now) }
        }
    }
    let q = AudioPlayoutQueue(sampleRate: 44100, channels: 2, policy: .realtime)
    q.push(sequence: 1, samples: tone(1024), nowNs: 0)
    check(render(q, 512, 1 * ms) == 0 && l[0] == 0 && !q.stats.playing, "below the start cushion: silence")
    for i in 2...6 { q.push(sequence: UInt32(i), samples: tone(1024), nowNs: UInt64(i) * ms) }
    check(q.stats.playing && q.stats.queuedFrames == 6144, "cushion reached (120 ms): playing")
    check(render(q, 512, 10 * ms) == 512 && abs(l[0] - 1000.0 / 32768) < 1e-6 && r[511] == l[0], "real samples, both channels")
    check(render(q, 4096, 20 * ms) == 4096 && render(q, 4096, 30 * ms) == 1536 && l[1535] != 0 && l[1536] == 0 && !q.stats.playing,
          "ran dry: the rest is silence and the cushion must be rebuilt")
    q.push(sequence: 7, samples: tone(1024), nowNs: 60 * ms)
    check(q.stats.underruns == 1, "audio arriving 30 ms after the queue ran dry is an underrun")
    check(render(q, 256, 61 * ms) == 0, "not playing again until the cushion is back")
    check(render(q, 256, 400 * ms) == 256, "nothing new for 300 ms: the tail is played anyway")
    _ = render(q, 4096, 401 * ms)
    q.push(sequence: 8, samples: tone(1024), nowNs: 5_000 * ms)
    check(q.stats.underruns == 1, "a 4.6 s silence is a stream that stopped, not an underrun")
    for i in 0..<60 { q.push(sequence: UInt32(100 + i), samples: tone(1024), nowNs: 5_001 * ms) }
    let st = q.stats
    check(st.trims >= 1 && Double(st.queuedFrames) <= 1.0 * 44100 && st.trimmedFrames > 0,
          "a burst past 1 s is cut back so latency cannot build up (queued \(st.queuedFrames), trimmed \(st.trimmedFrames))")

    section("playout: buffered policy")
    let b = AudioPlayoutQueue(sampleRate: 44100, channels: 2, policy: .buffered)
    for i in 0..<200 { b.push(sequence: 70_000 + UInt32(i), samples: tone(1024, Int16(i)), nowNs: 0) }
    check(b.stats.trims == 0 && b.stats.queuedFrames == 204_800, "4.6 s queued and nothing trimmed")
    check(render(b, 1024, 1 * ms) == 1024 && l[0] == 0 && render(b, 1024, 2 * ms) == 1024 && abs(l[0] - 1.0 / 32768) < 1e-9, "plays in order")
    b.setPaused(true)
    check(render(b, 1024, 3 * ms) == 0 && l[100] == 0 && b.stats.queuedFrames == 202_752, "paused (anchor rate 0): silence, nothing consumed")
    b.setPaused(false)
    let removed = b.flush(from: nil, until: 70_100, modulus: m24)
    check(removed == 98 * 1024 && render(b, 1024, 4 * ms) == 1024 && abs(l[0] - 100.0 / 32768) < 1e-9,
          "flush until seq: playout jumps to that unit (\(removed / 1024) units dropped)")
    check(b.flush(from: 70_150, until: 70_160, modulus: m24) == 10 * 1024, "flush from..until drops only that range")
    b.clear()
    check(b.stats.queuedFrames == 0 && render(b, 64, 5 * ms) == 0 && b.stats.underruns == 0, "clear: empty, and not an underrun")
    let mono = AudioPlayoutQueue(sampleRate: 44100, channels: 1, policy: .buffered)
    mono.push(sequence: 0, samples: [Int16](repeating: 500, count: 22_050), nowNs: 0)
    check(render(mono, 16, 1 * ms) == 16 && l[3] == r[3] && l[3] != 0, "a mono stream plays on both channels")
}

// MARK: - media (ffmpeg)

struct TestMedia {
    var video: F.Video
    var audio: F.Audio
    var h264Path: String
    var aacPath: String
    /// nil when ffmpeg has no libx265 (the HEVC tests are then skipped, loudly).
    var hevc: F.Video?
    var hevcPath: String
}

func ffmpeg(_ args: [String]) -> Bool {
    for path in ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"] where FileManager.default.isExecutableFile(atPath: path) {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: path)
        p.arguments = ["-hide_banner", "-loglevel", "error", "-y"] + args
        p.standardOutput = FileHandle.nullDevice
        do { try p.run() } catch { return false }
        p.waitUntilExit()
        return p.terminationStatus == 0
    }
    return false
}

/// 3 s of 498x1080 H.264 High at 30 fps with ONE IDR and no B-frames (what AirPlay mirroring sends): a
/// red band on blue that jumps from the top half to the bottom half at 1.5 s. 6 s of 440 Hz AAC-LC.
func makeMedia() -> TestMedia? {
    let dir = ProcessInfo.processInfo.environment["AIRPLAY_TEST_MEDIA_DIR"] ?? NSTemporaryDirectory() + "airplay-test-media"
    try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
    let h264 = dir + "/mirror.h264", aac = dir + "/tone.aac"
    guard ffmpeg(["-f", "lavfi", "-i", "color=c=blue:s=498x1080:r=30:d=3", "-f", "lavfi", "-i", "color=c=red:s=498x540:r=30:d=3",
                  "-filter_complex", "[0][1]overlay=x=0:y='if(lt(t,1.5),0,540)',format=yuv420p", "-frames:v", "90",
                  "-c:v", "libx264", "-profile:v", "high", "-level", "3.1", "-bf", "0",
                  "-x264-params", "keyint=600:min-keyint=600:scenecut=0:slices=1:aud=0", "-f", "h264", h264]),
          ffmpeg(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=6", "-ac", "2", "-c:a", "aac",
                  "-b:a", "128k", "-f", "adts", aac]),
          let v = FileManager.default.contents(atPath: h264).flatMap({ F.video(annexB: [UInt8]($0)) }),
          let a = FileManager.default.contents(atPath: aac).flatMap({ F.audio(adts: [UInt8]($0)) }) else { return nil }
    // What the phone sent at a 3840x2160 canvas with bit 42: HEVC Main, 994x2160, ~60 fps, one IDR. One
    // second: a red band on blue that jumps from the top half to the bottom half at 0.5 s.
    let hevc = dir + "/mirror.hevc"
    let h = ffmpeg(["-f", "lavfi", "-i", "color=c=blue:s=994x2160:r=60:d=1", "-f", "lavfi", "-i", "color=c=red:s=994x1080:r=60:d=1",
                    "-filter_complex", "[0][1]overlay=x=0:y='if(lt(t,0.5),0,1080)',format=yuv420p", "-frames:v", "60",
                    "-c:v", "libx265", "-preset", "ultrafast", "-profile:v", "main",
                    "-x265-params", "keyint=600:min-keyint=600:scenecut=0:bframes=0:info=0:log-level=error", "-f", "hevc", hevc])
        ? FileManager.default.contents(atPath: hevc).flatMap({ F.hevcVideo(annexB: [UInt8]($0), width: 994, height: 2160) })
        : nil
    return TestMedia(video: v, audio: a, h264Path: h264, aacPath: aac, hevc: h, hevcPath: hevc)
}

/// (r, g, b) of one pixel of a BGRA buffer.
func pixel(_ pb: CVPixelBuffer, x: Int, y: Int) -> (Int, Int, Int) {
    CVPixelBufferLockBaseAddress(pb, .readOnly)
    defer { CVPixelBufferUnlockBaseAddress(pb, .readOnly) }
    guard let base = CVPixelBufferGetBaseAddress(pb) else { return (0, 0, 0) }
    let p = base.advanced(by: y * CVPixelBufferGetBytesPerRow(pb) + x * 4).assumingMemoryBound(to: UInt8.self)
    return (Int(p[2]), Int(p[1]), Int(p[0]))
}

func isRed(_ c: (Int, Int, Int)) -> Bool { c.0 > 170 && c.1 < 90 && c.2 < 90 }
func isBlue(_ c: (Int, Int, Int)) -> Bool { c.2 > 170 && c.0 < 90 && c.1 < 90 }

final class FrameKeeper: @unchecked Sendable {
    private let lock = NSLock()
    private var firstFrame: CVPixelBuffer?
    private var lastFrame: CVPixelBuffer?
    private var n = 0
    func add(_ pb: CVPixelBuffer) {
        lock.withLock {
            if firstFrame == nil { firstFrame = pb }
            lastFrame = pb
            n += 1
        }
    }
    var first: CVPixelBuffer? { lock.withLock { firstFrame } }
    var last: CVPixelBuffer? { lock.withLock { lastFrame } }
    var count: Int { lock.withLock { n } }
}

/// The video lane's bytes for `media`, as the box would write them.
func videoLaneBytes(_ v: F.Video, key: [UInt8], scid: UInt64, drop: Int? = nil) -> [UInt8] {
    var lane = F.keyMessage(key: key, scid: scid)
    var geometry = F.Geometry()
    if v.width > 0 {
        geometry = .init(sourceWidth: Float(v.width), sourceHeight: Float(v.height), offsetX: 0,
                         outputWidth: Float(v.width), outputHeight: Float(v.height))
    }
    lane += F.videoFrame(key: key, seq: 0, opcode: 1, plain: v.config, geometry: geometry)
    for (n, f) in v.frames.enumerated() where n != drop {
        lane += F.videoFrame(key: key, seq: UInt64(n), opcode: 0, flags: f.idr ? 0x10 : 0, plain: f.au,
                             timestamp: (1000 << 32) + UInt64(Double(n) / 30 * 4_294_967_296.0))
    }
    return lane
}

@MainActor func testVideoDecode(_ media: TestMedia) {
    section("media: H.264 through the tunnel into VideoToolbox")
    let v = media.video
    check(v.frames.count == 90 && v.frames.filter(\.idr).count == 1 && v.frames[0].idr && v.config[1] == 100,
          "test stream: 90 frames, High profile, exactly one IDR (the first)")

    func run(drop: Int?) -> (keeper: FrameKeeper, stats: MirrorVideoDecoder.Stats, lane: AirPlayVideoStats) {
        let keeper = FrameKeeper()
        let dec = MirrorVideoDecoder { pb, _ in keeper.add(pb) }
        var lane = AirPlayVideoLane()
        let bytes = videoLaneBytes(v, key: [UInt8](repeating: 0x5A, count: 32), scid: 1, drop: drop)
        var at = 0
        while at < bytes.count {
            let n = min(4096, bytes.count - at)
            lane.feed(Array(bytes[at..<(at + n)]), newSource: false) { ev in
                switch ev {
                case .key(_, let fresh): if fresh { dec.reset() }
                case .config(let record, _): _ = dec.configure(record)
                case .frame(_, let data, _): dec.decode(data)
                case .gap, .undecryptable: break
                }
            }
            at += n
        }
        dec.finish()
        return (keeper, dec.snapshot, lane.stats)
    }

    let clean = run(drop: nil)
    check(clean.lane.decrypted == 90 && clean.lane.failed == 0 && clean.lane.gaps == 0 && clean.lane.flaggedIDR == 1, "90/90 frames decrypt")
    check(clean.stats.decoded == 90 && clean.stats.errors == 0 && clean.keeper.count == 90,
          "90/90 frames decode, no error (decoded \(clean.stats.decoded), errors \(clean.stats.errors), last OSStatus \(clean.stats.lastError))")
    check(clean.stats.width == 498 && clean.stats.height == 1080 && clean.stats.profile == 100 && clean.stats.level == 31,
          "coded size 498x1080, High, level 3.1 (\(clean.stats.width)x\(clean.stats.height))")
    if let first = clean.keeper.first, let last = clean.keeper.last {
        check(CVPixelBufferGetWidth(first) == 498 && CVPixelBufferGetHeight(first) == 1080
              && CVPixelBufferGetPixelFormatType(first) == kCVPixelFormatType_32BGRA && CVPixelBufferGetIOSurface(first) != nil,
              "pictures are 498x1080 BGRA on an IOSurface")
        let a = (pixel(first, x: 249, y: 270), pixel(first, x: 249, y: 810))
        let b = (pixel(last, x: 249, y: 270), pixel(last, x: 249, y: 810))
        check(isRed(a.0) && isBlue(a.1), "first picture (the IDR): red band on top, blue below \(a)")
        check(isBlue(b.0) && isRed(b.1), "last picture (89 P-frames later): the band has moved to the bottom \(b)")
    } else {
        check(false, "pictures were produced")
    }

    // One frame lost on the way. There is no keyframe request: the decoder must be fed everything that
    // follows, count what VideoToolbox rejects, and stay alive.
    let lossy = run(drop: 10)
    check(lossy.lane.gaps == 1 && lossy.lane.framesLost == 1 && lossy.lane.decrypted == 89, "one frame dropped: one seq jump, 89 decrypt")
    check(lossy.stats.submitted == 89 && lossy.stats.decoded + lossy.stats.errors == 89,
          "after the loss every later frame is still submitted and answered (decoded \(lossy.stats.decoded), errors \(lossy.stats.errors))")
    print("   measured: with frame 10 missing, VideoToolbox decoded \(lossy.stats.decoded) of 89 and rejected \(lossy.stats.errors)")

    // Decoder behaviour the pipeline relies on.
    let keeper = FrameKeeper()
    let dec = MirrorVideoDecoder { pb, _ in keeper.add(pb) }
    dec.decode(Data(v.frames[0].au))
    check(dec.snapshot.unconfigured == 1 && dec.snapshot.submitted == 0, "a frame before any avcC is counted, not submitted")
    check(dec.configure([1, 2, 3]) == false && dec.snapshot.configProblem != nil, "a config frame that is not an avcC is refused, with a reason")
    check(dec.configure(v.config) && dec.snapshot.sessions == 1 && dec.snapshot.codec == .h264 && dec.snapshot.configProblem == nil,
          "avcC accepted: one H.264 session")
    for f in v.frames.prefix(10) { dec.decode(Data(f.au)) }
    check(dec.configure(v.config) && dec.snapshot.sessions == 1, "the same avcC again (seam reconnect) keeps the session and its reference pictures")
    for f in v.frames[10..<20] { dec.decode(Data(f.au)) }
    dec.finish()
    check(dec.snapshot.decoded == 20 && dec.snapshot.errors == 0, "decoding continued across the repeated avcC (\(dec.snapshot.decoded)/20)")
    dec.reset()
    check(dec.snapshot.decoded == 0 && dec.snapshot.width == 0, "reset (a new stream's key) clears the stream's tallies")
}

@MainActor func testAudioDecode(_ media: TestMedia) {
    section("media: AAC-LC through the tunnel into AudioToolbox")
    let a = media.audio
    let units = a.units
    check(a.sampleRate == 44100 && a.channels == 2 && units.count > 250, "test stream: 44.1 kHz stereo, \(units.count) access units")
    let k1 = [UInt8](repeating: 0xA5, count: 32), k2 = [UInt8](repeating: 0x5A, count: 32)

    /// Feed lane bytes through the lane into the player, as the pipeline does.
    func feed(_ player: AirPlayAudioPlayer, _ lane: inout AirPlayAudioLane, _ bytes: [UInt8], declared: Int? = nil) {
        var formats: [UInt64: AirPlayAudioFormat] = [:]
        lane.feed(bytes, newSource: false) { ev in
            switch ev {
            case .format(let scid, let f): formats[scid] = f
            case .packet(let scid, let gen, let au, let head, _, let kind):
                player.accept(scid: scid, generation: gen, au: au, rtpHead: head, wireKind: kind, declaredType: declared,
                              format: formats[scid] ?? .measuredDefault)
            case .key: break
            }
        }
    }
    /// Pull, as the render callback would, until nothing more comes for 400 ms (long enough for the
    /// queue's tail rule to release a remainder smaller than the start cushion).
    func drain(_ player: AirPlayAudioPlayer, limit: Int = 10_000_000) -> [Float] {
        var out: [Float] = []
        var idle = 0
        while idle < 80 && out.count < limit {
            player.settle()
            let (l, _, n) = player.renderOffline(frames: 441)
            out += l[..<n]
            if n == 0 {
                idle += 1
                Thread.sleep(forTimeInterval: 0.005)
            } else {
                idle = 0
            }
        }
        return out
    }
    func crossingsPerSecond(_ x: ArraySlice<Float>) -> Double {
        var c = 0
        var prev = x.first ?? 0
        for v in x.dropFirst() {
            if (prev < 0) != (v < 0) { c += 1 }
            prev = v
        }
        return Double(c) / (Double(x.count) / 44100)
    }
    func rms(_ x: ArraySlice<Float>) -> Double { (x.reduce(0.0) { $0 + Double($1) * Double($1) } / Double(max(1, x.count))).squareRoot() }

    // ---- type 96, two streams in a row with different keys
    let player = AirPlayAudioPlayer(enableOutput: false)
    var lane = AirPlayAudioLane()
    let half = 100
    var bytes = F.keyMessage(key: k1, scid: 9600) + F.audioFormat(scid: 9600)
    for n in 0..<half {
        bytes += F.audioPacket(key: k1, scid: 9600, kind: .realtime, sequence: UInt32((65_500 + n) & 0xFFFF),
                               rtpTime: UInt32(n) * 1024, nonce: UInt64(n) + 1, au: units[n])
    }
    // Realtime packets arrive at playback rate; here they arrive at once, so take them in slices the
    // policy would not trim (under 1 s) and play each slice out.
    var pcm: [Float] = []
    let slice = bytes.count / 4
    for part in 0..<4 {
        feed(player, &lane, Array(bytes[(part * slice)..<(part == 3 ? bytes.count : (part + 1) * slice)]))
        pcm += drain(player)
    }
    var s = player.snapshot
    check(s.scid == 9600 && s.kind == .realtime && s.accepted == UInt64(half), "type 96 stream adopted from its packets alone (no stream event)")
    check(s.decoded >= UInt64(half) - 2 && s.trims == 0, "access units decode (\(s.decoded)/\(half), \(s.decodeErrors) rejected)")
    let expect = half * 1024
    check(abs(pcm.count - expect) <= 3 * 1024, "about \(expect) frames come out (\(pcm.count))")
    let mid = pcm[8192..<(pcm.count - 8192)]
    check(abs(crossingsPerSecond(mid) - 880) < 20 && rms(mid) > 0.05 && rms(mid) < 0.2,
          "it is the 440 Hz tone (\(Int(crossingsPerSecond(mid))) zero crossings/s, RMS \(String(format: "%.3f", rms(mid))))")

    var second = F.keyMessage(key: k2, scid: 9601) + F.audioFormat(scid: 9601)
    for n in 0..<20 {
        second += F.audioPacket(key: k2, scid: 9601, kind: .realtime, sequence: UInt32(n), rtpTime: UInt32(n) * 1024, nonce: UInt64(n) + 1, au: units[half + n])
    }
    second += F.audioPacket(key: k1, scid: 9600, kind: .realtime, sequence: 9, rtpTime: 0, nonce: 500, au: units[0]) // a straggler of the old stream
    feed(player, &lane, second)
    let next = drain(player)
    s = player.snapshot
    check(s.scid == 9601 && s.streamSwitches == 2 && s.stale == 1 && abs(next.count - 20 * 1024) <= 3 * 1024,
          "a new scid with a new key takes over; the old stream's straggler is dropped (\(next.count) frames, stale \(s.stale))")
    let stoppedGeneration = lane.stats.first { $0.scid == 9601 }?.generation ?? 0
    player.streamStopped(scid: 9601, generation: stoppedGeneration)
    feed(player, &lane, F.audioPacket(key: k2, scid: 9601, kind: .realtime, sequence: 21, rtpTime: 0, nonce: 600, au: units[1]))
    player.settle()
    check(player.snapshot.scid == nil && player.snapshot.stale == 2, "after the stream's stop event its late packets are dropped")
    // A sender that reuses its stream ids (the bench's synthetic sender does): the same scid under a
    // NEW key is a new stream, also on a new lane (a new client after a reconnect).
    var lane2 = AirPlayAudioLane()
    let k3 = [UInt8](repeating: 0x77, count: 32)
    var again = F.keyMessage(key: k3, scid: 9601) + F.audioFormat(scid: 9601)
    for n in 0..<10 {
        again += F.audioPacket(key: k3, scid: 9601, kind: .realtime, sequence: UInt32(n), rtpTime: UInt32(n) * 1024, nonce: UInt64(n) + 1, au: units[n])
    }
    feed(player, &lane2, again)
    let reused = drain(player)
    check(player.snapshot.scid == 9601 && player.snapshot.stale == 2 && reused.count > 6 * 1024,
          "a stopped scid that returns under a new key plays (\(reused.count) frames)")
    player.shutdown()

    // ---- type 103: everything at once, far ahead of real time
    let buffered = AirPlayAudioPlayer(enableOutput: false)
    var blane = AirPlayAudioLane()
    var all = F.keyMessage(key: k1, scid: 10300) + F.audioFormat(scid: 10300)
    for (n, au) in units.enumerated() {
        all += F.audioPacket(key: k1, scid: 10300, kind: .buffered, sequence: 70_000 + UInt32(n), rtpTime: UInt32(n) * 1024, nonce: UInt64(n), au: au)
    }
    feed(buffered, &blane, all, declared: 103)
    buffered.settle()
    var b = buffered.snapshot
    check(b.kind == .buffered && b.accepted == UInt64(units.count) && b.backlogUnits > units.count / 2 && b.queue.trims == 0,
          "\(units.count) units delivered at once: nothing is dropped, most wait compressed (\(b.backlogUnits) in the backlog)")
    check(Double(b.queue.queuedFrames) / 44100 < 2.0, "only about 1.5 s is decoded ahead (\(b.queue.queuedFrames) frames)")
    var played = drain(buffered, limit: 44100)
    buffered.setRate(0)
    buffered.settle()
    let paused = buffered.renderOffline(frames: 441)
    check(paused.produced == 0 && buffered.snapshot.queue.paused, "anchor rate 0 pauses playout")
    buffered.setRate(1)
    let skip = units.count * 2 / 3
    buffered.flush(from: nil, until: 70_000 + UInt32(skip))
    buffered.settle()
    b = buffered.snapshot
    check(b.flushedUnits > 0, "flush until seq \(70_000 + skip): \(b.flushedUnits) waiting units dropped")
    played += drain(buffered)
    let want = 44100 + (units.count - skip) * 1024
    check(abs(played.count - want) <= 4 * 1024, "playout: 1 s, then straight to the flush point and on to the end (\(played.count) frames, expected about \(want))")
    b = buffered.snapshot
    check(b.decodeErrors <= 1 && b.backlogUnits == 0, "the whole backlog was decoded as playout made room (\(b.decodeErrors) rejected)")
    let tail = played[(played.count - 30_000)..<(played.count - 8_000)]
    check(abs(crossingsPerSecond(tail) - 880) < 20, "still the tone after the flush (\(Int(crossingsPerSecond(tail))) crossings/s)")
    buffered.shutdown()
}


// MARK: - codec config (opcode-1 body)

/// A minimal Annex-B HEVC stream with real NAL headers (VPS/SPS/PPS/IDR) and a Main, level 5.1
/// profile_tier_level, including an emulation-prevention byte in the constraint flags.
func fakeHevcAnnexB() -> [UInt8] {
    let sc: [UInt8] = [0, 0, 0, 1]
    let vps: [UInt8] = [0x40, 0x01, 0x0C, 0x01, 0xFF, 0xFF, 0x01, 0x60, 0, 0, 3, 0, 0x90]
    // SPS: header 42 01, then [vps_id|max_sub_layers|nesting] 01, PTL: 01 (Main) 60 00 00 00 (compat)
    // 90 00 00 03 00 00 00 (constraint flags with one 03 escape) 99 (level 5.1 = 153), then filler.
    let sps: [UInt8] = [0x42, 0x01, 0x01, 0x01, 0x60, 0, 0, 0, 0x90, 0, 0, 3, 0, 0, 0, 0x99, 0xA0, 0x01, 0xF0, 0x80]
    let pps: [UInt8] = [0x44, 0x01, 0xC1, 0x72, 0xB4, 0x62, 0x40]
    let idr: [UInt8] = [0x26, 0x01, 0xAF, 0x11, 0x22]
    let trail: [UInt8] = [0x02, 0x01, 0xD0, 0x33]
    return sc + vps + sc + sps + sc + pps + sc + idr + sc + trail
}

@MainActor func testCodecConfig() {
    section("codec config: what the opcode-1 body carries")
    guard let hv = F.hevcVideo(annexB: fakeHevcAnnexB(), width: 994, height: 2160) else {
        check(false, "the HEVC fixture builds")
        return
    }
    let body = hv.config
    check(Array(body[4..<8]) == Array("hvc1".utf8) && Int(body[0]) << 24 | Int(body[1]) << 16 | Int(body[2]) << 8 | Int(body[3]) == body.count
          && Array(body[90..<94]) == Array("hvcC".utf8), "fixture: an hvc1 box (size = body), 78-byte sample entry, then the hvcC box")
    check(hv.frames.count == 2 && hv.frames[0].idr && !hv.frames[1].idr, "fixture: two access units, the first an IDR")
    let c = MirrorVideoConfig.parse(body)
    check(c?.codec == .hevc && c?.container == "hvc1/hvcC" && c?.nalLengthSize == 4 && c?.parameterSets.count == 3,
          "hvc1 box: HEVC, the hvcC found inside, NAL length 4, VPS/SPS/PPS")
    check(c?.profile == 1 && c?.level == 153 && c?.summary == "HEVC Main L5.1", "profile and level read through the escape byte (\(c?.summary ?? "nil"))")
    check(c?.parameterSets.map { ($0[0] >> 1) & 0x3F } == [32, 33, 34], "parameter sets in VPS, SPS, PPS order")
    // The hvcC alone, and the hvcC box behind bytes this build does not know.
    let hvccBox = Array(body[86...]) // [size]['hvcC'][record] ['colr' box]
    let hvccSize = Int(hvccBox[0]) << 24 | Int(hvccBox[1]) << 16 | Int(hvccBox[2]) << 8 | Int(hvccBox[3])
    let record = Array(hvccBox[8..<hvccSize])
    check(MirrorVideoConfig.parse(record)?.codec == .hevc, "a bare hvcC record is HEVC")
    check(MirrorVideoConfig.parse([0xDE, 0xAD, 0xBE, 0xEF, 0, 1] + Array(hvccBox))?.container == "box hvcC", "an hvcC box anywhere in the body is found")
    let avcc: [UInt8] = [1, 100, 0, 31, 0xFF, 0xE1, 0, 4, 0x67, 0x64, 0, 31, 1, 0, 2, 0x68, 0xEE]
    let a = MirrorVideoConfig.parse(avcc)
    check(a?.codec == .h264 && a?.container == "avcC" && a?.summary == "H.264 High L3.1", "a bare avcC is H.264 (\(a?.summary ?? "nil"))")
    var wrapped = F.be32(UInt32(8 + 78 + 8 + avcc.count)) + Array("avc1".utf8) + [UInt8](repeating: 0, count: 78)
    wrapped += F.be32(UInt32(8 + avcc.count)) + Array("avcC".utf8) + avcc
    check(MirrorVideoConfig.parse(wrapped)?.container == "avc1/avcC", "an avc1 box wrapping an avcC is H.264")
    check(MirrorVideoConfig.parse([1, 2, 3]) == nil && MirrorVideoConfig.parse([]) == nil
          && MirrorVideoConfig.parse(Array(body.prefix(100))) == nil, "garbage and a truncated hvc1 box are neither")
    var badSize = body
    badSize[90 - 1] = 0xFF // hvcC box size runs past the body
    check(MirrorVideoConfig.parse(badSize) == nil, "an hvcC box whose size overruns the body is refused")
}

// MARK: - timing (fps, jitter, playout)

@MainActor func testTiming() {
    section("timing: frame rate and arrival jitter")
    var m = VideoTimingMeter()
    check(m.readout(now: 5) == VideoTimingMeter.Readout(), "nothing yet: zeros, no last-frame age")
    // 30 fps for 3 s, arriving exactly on the timestamps, decoded 5 ms later.
    for n in 0..<90 {
        let t = 10 + Double(n) / 30
        m.arrival(host: t, stream: 1000 + Double(n) / 30)
        m.decoded(host: t + 0.005)
    }
    var r = m.readout(now: 10 + 89.0 / 30 + 0.01)
    check(abs(r.fps - 30) < 0.6 && abs(r.averageFPS - 30) < 0.01 && abs(r.streamFPS - 30) < 0.01,
          "steady 30 fps: moving \(r.fps), average \(r.averageFPS), by timestamps \(r.streamFPS)")
    check(r.jitterMs < 0.001 && r.jitterPeakMs < 0.001 && r.decoded == 90 && r.arrivals == 90, "arrival on the timestamps: no jitter")
    // The old readout's failure: a still screen for 0.6 s (iOS encodes on change) read 0 between two
    // polls. The window still shows the rate; after more than the window of stillness it is 0, and the
    // last-frame age says why.
    r = m.readout(now: 10 + 89.0 / 30 + 0.6)
    check(r.fps > 20 && abs((r.sinceLastFrame ?? 0) - 0.595) < 0.01, "0.6 s of a still screen: still \(r.fps) fps over the window, last picture 0.6 s ago")
    r = m.readout(now: 10 + 89.0 / 30 + 2.5)
    check(r.fps == 0 && r.streamFPS == 0 && abs(r.averageFPS - 30) < 0.01, "2.5 s still: moving 0, the average stays")

    var j = VideoTimingMeter()
    // Timestamps every 1/60 s; arrivals alternate 8 ms early and 8 ms late: |D| = 16 ms every frame.
    for n in 0..<240 {
        let ts = Double(n) / 60
        j.arrival(host: 50 + ts + (n % 2 == 0 ? -0.008 : 0.008), stream: ts)
        j.decoded(host: 50 + ts + 0.01)
    }
    r = j.readout(now: 50 + 4.01)
    check(abs(r.jitterMs - 16) < 0.5 && abs(r.jitterPeakMs - 16) < 0.01 && abs(r.streamFPS - 60) < 0.01 && abs(r.fps - 60) < 1,
          "±8 ms arrival jitter at 60 fps: RFC 3550 jitter \(r.jitterMs) ms, peak \(r.jitterPeakMs) ms, \(r.streamFPS) fps by timestamps")
    var young = VideoTimingMeter()
    for n in 0..<10 { young.decoded(host: 1 + Double(n) / 30) }
    check(abs(young.readout(now: 1.31).fps - 30) < 0.01, "a stream younger than the window: rate from its own span")
    young.reset()
    check(young.readout(now: 2) == VideoTimingMeter.Readout(), "reset (a new stream) starts over")

    section("timing: smooth playback schedule")
    var sched = PlayoutSchedule(buffer: 0.1)
    // 30 fps stamps; arrivals with 0..60 ms of extra delay; the fastest path is 20 ms.
    let extra = [0.0, 0.06, 0.01, 0.03, 0.0, 0.05, 0.02, 0.0, 0.04, 0.01]
    var dues: [Double] = []
    for n in 0..<10 {
        let ts = 500 + Double(n) / 30
        let arrival = 7 + Double(n) / 30 + 0.02 + extra[n]
        dues.append(sched.due(stream: ts, arrival: arrival, now: arrival + 0.004))
    }
    let steps = zip(dues, dues.dropFirst()).map { $1 - $0 }
    check(steps.allSatisfy { abs($0 - 1.0 / 30) < 1e-9 } && abs(dues[0] - (7 + 0.02 + 0.1)) < 1e-9 && sched.stats.late == 0,
          "jittered arrivals are shown exactly 1/30 s apart, 100 ms behind the fastest arrival, none late")
    let lateDue = sched.due(stream: 500 + 10.0 / 30, arrival: 7 + 10.0 / 30 + 0.02 + 0.15, now: 7 + 10.0 / 30 + 0.02 + 0.155)
    check(sched.stats.late == 1 && lateDue < 7 + 10.0 / 30 + 0.175, "a frame 150 ms late misses its slot: shown at once, counted")
    _ = sched.due(stream: 400, arrival: 8, now: 8)
    check(sched.stats.rebases == 1, "a timestamp going backwards starts a new base")
    _ = sched.due(stream: 410, arrival: 8.1, now: 8.1)
    check(sched.stats.rebases == 2, "a timestamp jumping 10 s ahead starts a new base")
}

func blankPixelBuffer() -> CVPixelBuffer? {
    var pb: CVPixelBuffer?
    CVPixelBufferCreate(kCFAllocatorDefault, 16, 16, kCVPixelFormatType_32BGRA, nil, &pb)
    return pb
}

@MainActor func testPresenter() {
    section("timing: the presenter (live clock)")
    let shown = Collector<Double>()
    let p = FramePresenter(buffer: 0.1) { _ in shown.add(FramePresenter.now) }
    guard let pb = blankPixelBuffer() else { check(false, "a pixel buffer"); return }
    // Off (the default): shown at once.
    let t0 = FramePresenter.now
    for n in 0..<3 { p.submit(pb, time: .init(arrival: t0, stream: Double(n) / 30)) }
    check(shown.count == 3 && p.snapshot.presented == 3 && !p.snapshot.smooth, "smooth off: every picture shown on decode")
    // On: 12 pictures at 30 fps that arrive in clumps of three every 100 ms (Wi-Fi aggregation) are
    // spread back out over their own timestamps, 100 ms behind the earliest arrival. The first clump
    // sets the clock base (its newest picture arrived earliest relative to its stamp), so it is due as
    // one and only its newest picture is shown; from the second clump on, every picture has its slot.
    p.smooth = true
    let startCount = shown.count
    let t1 = FramePresenter.now
    for c in 0..<4 {
        while FramePresenter.now < t1 + Double(c) * 0.1 { Thread.sleep(forTimeInterval: 0.001) }
        let now = FramePresenter.now
        for k in 0..<3 { p.submit(pb, time: .init(arrival: now, stream: 100 + Double(3 * c + k) / 30)) }
    }
    _ = waitUntil(2) { shown.count >= startCount + 10 }
    RunLoop.main.run(until: Date().addingTimeInterval(0.1))
    let times = Array(shown.all.dropFirst(startCount))
    let gaps = zip(times, times.dropFirst()).map { $1 - $0 }
    check(times.count == 10 && abs(times[0] - (t1 + 0.1)) < 0.02 && gaps.allSatisfy { $0 > 0.018 && $0 < 0.05 }
          && p.snapshot.late == 0,
          "smooth on: clumps of 3 every 100 ms come out ~33 ms apart (\(times.count) shown, first at \(Int(((times.first ?? 0) - t1) * 1000)) ms, gaps \(gaps.map { Int($0 * 1000) }) ms, late \(p.snapshot.late))")
    // A stream that ends with pictures still held: nothing of it appears after the clear.
    let before = shown.count
    let t2 = FramePresenter.now
    for n in 0..<5 { p.submit(pb, time: .init(arrival: t2, stream: 200 + Double(n) / 30)) }
    p.cancelPending()
    RunLoop.main.run(until: Date().addingTimeInterval(0.4))
    check(shown.count == before && p.snapshot.dropped == 5, "cancel: the 5 held pictures are dropped, none shown later")
    p.smooth = false
    check(!p.snapshot.smooth, "smooth can be switched off again")
}

// MARK: - media: HEVC

@MainActor func testHevcDecode(_ media: TestMedia) {
    section("media: HEVC (hvc1/hvcC) through the tunnel into VideoToolbox")
    guard let hv = media.hevc else {
        print("SKIPPED  HEVC media tests: ffmpeg has no libx265")
        return
    }
    print("   this Mac: hardware HEVC decode \(MirrorVideoDecoder.hevcHardwareAvailable ? "available" : "NOT available")")
    check(hv.frames.count == 60 && hv.frames.filter(\.idr).count == 1 && hv.frames[0].idr, "test stream: 60 frames, one IDR (the first)")
    check(MirrorVideoConfig.parse(hv.config)?.summary.hasPrefix("HEVC Main") == true, "its config body is an hvc1 box with an HEVC Main hvcC")

    let keeper = FrameKeeper()
    let dec = MirrorVideoDecoder { pb, _ in keeper.add(pb) }
    var lane = AirPlayVideoLane()
    let bytes = videoLaneBytes(hv, key: [UInt8](repeating: 0x6B, count: 32), scid: 7)
    var at = 0
    while at < bytes.count {
        let n = min(65_536, bytes.count - at)
        lane.feed(Array(bytes[at..<(at + n)]), newSource: false) { ev in
            switch ev {
            case .key(_, let fresh): if fresh { dec.reset() }
            case .config(let record, _): _ = dec.configure(record)
            case .frame(_, let data, _): dec.decode(data)
            case .gap, .undecryptable: break
            }
        }
        at += n
    }
    dec.finish()
    let s = dec.snapshot
    check(lane.stats.decrypted == 60 && lane.stats.failed == 0, "60/60 HEVC frames decrypt")
    check(s.decoded == 60 && s.errors == 0 && s.codec == .hevc && s.width == 994 && s.height == 2160,
          "60/60 decode as HEVC 994x2160, no error (decoded \(s.decoded), errors \(s.errors), OSStatus \(s.lastError), \(s.codecSummary))")
    check(s.hardware != nil, "the decoder reports hardware or software (\(s.hardware == true ? "hardware" : "software"))")
    if MirrorVideoDecoder.hevcHardwareAvailable { check(s.hardware == true, "a Mac with an HEVC decoder block uses it") }
    if let first = keeper.first, let last = keeper.last {
        check(CVPixelBufferGetWidth(first) == 994 && CVPixelBufferGetHeight(first) == 2160, "pictures are 994x2160")
        let a = (pixel(first, x: 497, y: 540), pixel(first, x: 497, y: 1620))
        let b = (pixel(last, x: 497, y: 540), pixel(last, x: 497, y: 1620))
        check(isRed(a.0) && isBlue(a.1), "first HEVC picture: red band on top, blue below \(a)")
        check(isBlue(b.0) && isRed(b.1), "last HEVC picture: the band has moved to the bottom \(b)")
    } else {
        check(false, "HEVC pictures were produced")
    }

    section("media: codec switch H.264 -> HEVC -> H.264")
    // One decoder, no reset in between (the worst case: a config of the other codec on a live session).
    let k2 = FrameKeeper()
    let sw = MirrorVideoDecoder { pb, _ in k2.add(pb) }
    check(sw.configure(media.video.config), "H.264 configured")
    for f in media.video.frames.prefix(10) { sw.decode(Data(f.au)) }
    sw.finish()
    check(sw.configure(hv.config) && sw.snapshot.codec == .hevc && sw.snapshot.codecSwitches == 1 && sw.snapshot.sessions == 2,
          "an HEVC config replaces the H.264 session (switches \(sw.snapshot.codecSwitches), sessions \(sw.snapshot.sessions))")
    for f in hv.frames { sw.decode(Data(f.au)) }
    sw.finish()
    check(sw.snapshot.decoded == 70 && sw.snapshot.errors == 0, "10 H.264 then 60 HEVC pictures, no error (\(sw.snapshot.decoded))")
    if let last = k2.last { check(CVPixelBufferGetWidth(last) == 994 && isRed(pixel(last, x: 497, y: 1620)), "the last picture is the HEVC one") }
    check(sw.configure([0, 0, 0, 9, 0x68, 0x76, 0x63, 0x31, 0]) == false && sw.snapshot.codec == nil && sw.snapshot.configProblem != nil,
          "an unusable config leaves no decoder: \(sw.snapshot.configProblem ?? "")")
    for f in hv.frames.prefix(3) { sw.decode(Data(f.au)) }
    check(sw.snapshot.unconfigured == 3 && sw.snapshot.submitted == 70, "frames after it are counted unconfigured, never fed to the old codec's decoder")
    check(sw.configure(media.video.config) && sw.snapshot.codec == .h264 && sw.snapshot.width == 498, "back to H.264")
    for f in media.video.frames { sw.decode(Data(f.au)) }
    sw.finish()
    check(sw.snapshot.decoded == 70 + 90 && sw.snapshot.errors == 0, "H.264 decodes again (\(sw.snapshot.decoded), errors \(sw.snapshot.errors))")

    section("media: the pipeline across two streams with different codecs")
    let lines = Collector<String>()
    let k3 = FrameKeeper()
    let pipe = MediaPipeline(audioOutput: false, onFrame: { k3.add($0) }, onVideoCleared: {}, onLog: { lines.add($0) })
    var lane2 = AirPlayVideoLane()
    let both = videoLaneBytes(media.video, key: [UInt8](repeating: 1, count: 32), scid: 1)
        + videoLaneBytes(hv, key: [UInt8](repeating: 2, count: 32), scid: 2)
    lane2.feed(both, newSource: false) { pipe.handle(.video($0)) }
    pipe.decoder.finish()
    let log = lines.all.joined(separator: "\n")
    check(k3.count == 150 && pipe.decoder.snapshot.decoded == 60 && pipe.decoder.snapshot.codec == .hevc,
          "90 H.264 then 60 HEVC pictures through the pipeline (\(k3.count))")
    check(log.contains("video: H.264 High L3.1") && log.contains("video: HEVC Main") && log.contains("codec changed H.264 -> HEVC"),
          "both configs and the codec change are logged")
    let t = pipe.timingReadout()
    check(t.decoded == 60 && t.arrivals == 60, "the timing meter restarted with the new stream (\(t.decoded) decoded)")
}

// MARK: - end to end: controller + synthetic box

@MainActor func testEndToEnd(_ media: TestMedia) {
    section("end to end: controller against the synthetic box (two sessions, 6x speed)")
    let boxMedia = SyntheticBox.Media(video: media.video, audio: media.audio)
    func makeController(_ link: SyntheticLink, keeper: FrameKeeper, cleared: Collector<Int>) -> SessionController {
        let form = FormModel(defaults: nil)
        form.ssid = "Bench AP"
        form.name = "Car"
        form.channel = "36"
        return SessionController(boxLink: link, form: form, audioOutput: false,
                                 onFrame: { keeper.add($0) }, onVideoCleared: { cleared.add(1) })
    }

    var options = SyntheticBox.Options()
    options.speed = 6
    options.pairs = 1
    let link = SyntheticLink(media: boxMedia, options: options)
    let keeper = FrameKeeper(), cleared = Collector<Int>()
    let c = makeController(link, keeper: keeper, cleared: cleared)
    check(link.boxes.isEmpty, "nothing is opened before Start")
    check(!c.start(passphrase: "short") && c.link.lastError.contains("8 to 63") && !c.link.running && link.boxes.isEmpty,
          "an invalid passphrase is refused before the link is touched")
    check(c.start(passphrase: "correct horse") && c.link.running, "Start")
    check(waitUntil(5) { c.link.phase == "AirPlay-only mode active" }, "mode active (phase: \(c.link.phase))")
    check(waitUntil(5) { c.link.ap == "up" && c.link.session == "waiting" }, "AP up, waiting for a sender")
    check(c.link.caps.contains("AirPlay") && c.link.bundleState == "ok" && c.link.bundle.hasPrefix("a0a1a2"), "HELLO_ACK caps and bundle shown (\(c.link.caps), \(c.link.bundle))")
    check(waitUntil(8) { c.video.decoded >= 90 }, "session A: all 90 frames decoded (\(c.video.decoded))")
    check(c.video.codedSize.hasPrefix("498x1080") && c.video.headerSize == "source 498x1080, output 498x1080", "resolution from the decoder and from the header floats")
    check(waitUntil(8) { c.meta.title == "Test Track" && c.meta.artwork != nil }, "session B: track and artwork arrive")
    check(c.meta.artist == "Nobody!" && c.meta.volume.hasPrefix("-17.5 dB") && (c.meta.artwork?.width ?? 0) == 96, "metadata shown; the JPEG decodes")
    check(waitUntil(8) { c.link.session == "waiting" && c.link.lastEnd == "teardown" && c.streams.rows.isEmpty }, "both sessions ended by teardown; no stream rows left")
    RunLoop.main.run(until: Date().addingTimeInterval(0.6))

    let box = link.boxes[0]
    let counters = c.pipeline.snapshot
    let seen = box.seen
    let d = c.pipeline.decoder.snapshot
    check(seen.modeRequests.count == 1 && seen.modeRequests[0] == AirPlayModeRequest(ssid: "Bench AP", psk: "correct horse", name: "Car", channel: 36),
          "the box received exactly the request typed")
    check(seen.subscribes == 0 && seen.heartbeatsOutsideMode == 0 && seen.heartbeats >= 2 && !seen.leftByWatchdog && seen.logArmed,
          "no SUBSCRIBE, heartbeats only inside the mode (\(seen.heartbeats)), the watchdog never fired, box log armed")
    check(c.video.decrypted == 90 && c.video.undecryptable == 0 && c.video.gaps == 0 && c.video.keyframes == 1 && c.video.decodeErrors == 0
          && d.decoded == 90, "video: 90 decrypted, 90 decoded, one IDR, no error")
    check(keeper.count == 90 && cleared.count >= 1 && !c.video.showing, "90 pictures reached the surface, which was cleared when the stream stopped")
    if let first = keeper.first, let last = keeper.last {
        check(isRed(pixel(first, x: 249, y: 270)) && isRed(pixel(last, x: 249, y: 810)), "the pictures are the test pattern, first to last")
    }
    let a = c.pipeline.player.snapshot
    check(c.audio.packets == UInt64(seen.audioPacketsSent) && c.audio.undecryptable == 0,
          "audio: every packet of all three streams decrypts (\(c.audio.packets)/\(seen.audioPacketsSent))")
    check(a.streamSwitches == 3 && a.flushedUnits > 0 && a.decoded > 100, "three audio streams played in turn (two type 96 keys, then type 103); the flush was applied (\(a.flushedUnits) units)")
    check(counters.sender?.name == "Synthetic Phone" && !c.link.sender.contains("02:00") && !c.link.sender.contains("192.0.2"),
          "sender shown by name and model; no device id, no address")
    let logText = c.log.lines.map(\.text).joined(separator: "\n")
    check(!logText.contains("correct horse") && logText.contains("passphrase 13 chars"), "the passphrase is in no log line")
    check(!logText.contains("02:00:00") && !logText.contains("192.0.2.1"), "the sender's device id and address are in no log line")
    check(c.log.lines.contains { $0.source == .box && $0.text.contains("synthetic box log armed") }, "box log lines reach the log pane")
    check(logText.contains("video: key for a new stream") && logText.contains("stream start: type 103"), "stream starts are logged")

    let beatsBefore = seen.heartbeats
    c.stop()
    check(waitUntil(3) { !c.link.running && c.link.phase == "idle" }, "Stop: back to idle")
    RunLoop.main.run(until: Date().addingTimeInterval(1.3))
    let end = box.seen
    check(end.projectionSelects == 1 && end.heartbeats <= beatsBefore + 1 && end.heartbeatsOutsideMode == 0,
          "Stop sent CT_MODE_SELECT [PROJECTION] once and no heartbeat after it")
    check(c.start(passphrase: "") == false && c.link.lastError.contains("8 to 63"), "the passphrase is not remembered: a second Start needs it again")
    c.shutdown()

    section("end to end: a lost frame")
    var lossy = SyntheticBox.Options()
    lossy.speed = 6
    lossy.pairs = 1
    lossy.dropVideoFrame = 10
    let link2 = SyntheticLink(media: boxMedia, options: lossy)
    let c2 = makeController(link2, keeper: FrameKeeper(), cleared: Collector<Int>())
    c2.start(passphrase: "correct horse")
    check(waitUntil(10) { c2.video.decrypted == 89 }, "89 frames decrypt")
    RunLoop.main.run(until: Date().addingTimeInterval(1.0))
    check(c2.video.gaps == 1 && c2.video.framesLost == 1 && c2.video.decoded + c2.video.decodeErrors == 89,
          "one seq jump shown; decoding went on (decoded \(c2.video.decoded), decode errors \(c2.video.decodeErrors))")
    check(c2.log.lines.contains { $0.text.contains("seq jump, expected 10 got 11") }, "the jump is in the log")
    c2.stop()
    waitUntil(3) { !c2.link.running }
    c2.shutdown()

    section("end to end: the box is unplugged mid-session and comes back")
    var plain = SyntheticBox.Options()
    plain.speed = 6
    let link3 = SyntheticLink(media: boxMedia, options: plain, unplugAfter: 1.2, unplugFor: 0.8)
    let keeper3 = FrameKeeper()
    let c3 = makeController(link3, keeper: keeper3, cleared: Collector<Int>())
    c3.start(passphrase: "correct horse")
    check(waitUntil(4) { c3.link.phase.hasPrefix("box disconnected") }, "unplug: the app says so and keeps the intent (\(c3.link.phase))")
    check(c3.link.running && c3.link.ap == "-", "still 'running'; the box state is unknown")
    check(waitUntil(6) { link3.boxes.count == 2 && c3.link.phase == "AirPlay-only mode active" }, "re-attach: opened again and the mode requested again")
    check(waitUntil(6) { link3.boxes[1].seen.modeRequests.first?.psk == "correct horse" }, "with the same request, passphrase included (held in memory for this)")
    let before = keeper3.count
    check(waitUntil(8) { keeper3.count >= before + 90 }, "and the new session's video decodes from its own IDR (\(keeper3.count - before) new pictures)")
    RunLoop.main.run(until: Date().addingTimeInterval(0.6))
    check(c3.video.decodeErrors == 0, "no decode error on the session after the reconnect")
    c3.stop()
    check(waitUntil(3) { !c3.link.running }, "Stop after a reconnect")
    c3.shutdown()
}

// MARK: - end to end: HEVC, codec switch, frame rate, smooth playback

@MainActor func testEndToEndHevc(_ media: TestMedia) {
    guard let hv = media.hevc else {
        print("SKIPPED  HEVC end-to-end tests: ffmpeg has no libx265")
        return
    }
    let boxMedia = SyntheticBox.Media(video: media.video, audio: media.audio, hevc: hv)
    func controller(_ link: SyntheticLink, _ keeper: FrameKeeper, display: AirPlayModeRequest.Display, hevc: Bool) -> SessionController {
        let form = FormModel(defaults: nil)
        form.ssid = "Bench AP"
        form.display = display
        form.hevc = hevc
        return SessionController(boxLink: link, form: form, audioOutput: false, onFrame: { keeper.add($0) }, onVideoCleared: {})
    }
    let uhd = AirPlayModeRequest.Display(width: 3840, height: 2160, maxFPS: 60)

    section("end to end: HEVC allowed at 3840x2160@60 -> the synthetic phone sends HEVC")
    var o = SyntheticBox.Options()
    o.speed = 4
    o.pairs = 1
    let link = SyntheticLink(media: boxMedia, options: o)
    let keeper = FrameKeeper()
    let c = controller(link, keeper, display: uhd, hevc: true)
    let json = NSTemporaryDirectory() + "airplay-test-status-\(getpid()).json"
    c.statusDumpPath = json
    check(c.start(passphrase: "correct horse"), "Start")
    check(c.link.requested == "3840x2160@60, HEVC allowed", "the request is shown (\(c.link.requested))")
    var maxFps = 0.0
    var sawJSON = false
    let decodedAll = waitUntil(10) {
        maxFps = max(maxFps, c.video.fps)
        if !sawJSON, c.video.decoded > 20, let d = FileManager.default.contents(atPath: json),
           let o = try? JSONSerialization.jsonObject(with: d) as? [String: Any], let v = o["video"] as? [String: Any],
           (v["fps"] as? Double ?? 0) > 0, v["codec"] as? String == "HEVC" {
            sawJSON = true
        }
        return c.video.decoded >= 60
    }
    check(decodedAll, "all 60 HEVC frames decoded (\(c.video.decoded))")
    check(c.video.codec.hasPrefix("HEVC Main") && c.video.codedSize == "994x2160" && c.video.decodeErrors == 0,
          "status: \(c.video.codec), coded \(c.video.codedSize), no decode error")
    check(c.video.headerSize == "source 994x2160, output 994x2160", "header geometry of the 4K canvas (\(c.video.headerSize))")
    check(maxFps > 0, "the frame-rate readout is not 0 while frames decode (max \(maxFps) fps at 4x speed)")
    check(abs(c.video.streamFPS - 60) < 1 || c.video.streamFPS == 0, "stream rate by timestamps is the phone's 60 fps (\(c.video.streamFPS))")
    check(sawJSON, "status JSON: video.codec HEVC with a non-zero fps while decoding")
    let seen = link.boxes[0].seen
    check(seen.modeRequests.first?.hevc == true && seen.modeRequests.first?.display == uhd && seen.videoCodecs.first == .hevc,
          "the box received display 3840x2160@60 and the HEVC flag, and chose HEVC")
    if let d = FileManager.default.contents(atPath: json), let text = String(data: d, encoding: .utf8) {
        check(text.contains("\"display\" : \"3840x2160@60\"") && !text.contains("correct horse"),
              "status JSON carries the request's display, never the passphrase")
    }
    if let first = keeper.first { check(CVPixelBufferGetHeight(first) == 2160 && isRed(pixel(first, x: 497, y: 540)), "the HEVC picture reached the surface") }
    c.stop()
    _ = waitUntil(3) { !c.link.running }
    c.shutdown()
    try? FileManager.default.removeItem(atPath: json)

    section("end to end: HEVC allowed at 1920x1080 -> H.264 (the phone's rule)")
    let link2 = SyntheticLink(media: boxMedia, options: o)
    let c2 = controller(link2, FrameKeeper(), display: .measuredDefault, hevc: true)
    c2.start(passphrase: "correct horse")
    check(waitUntil(10) { c2.video.decoded >= 90 }, "90 H.264 frames decoded")
    check(c2.video.codec.hasPrefix("H.264 High") && link2.boxes[0].seen.videoCodecs.first == .h264, "H.264 (\(c2.video.codec))")
    c2.stop()
    _ = waitUntil(3) { !c2.link.running }
    c2.shutdown()

    section("end to end: H.264 session, then an HEVC session, in one run")
    var alt = SyntheticBox.Options()
    alt.speed = 6
    alt.pairs = 2
    alt.alternateCodecs = true
    let link3 = SyntheticLink(media: boxMedia, options: alt)
    let keeper3 = FrameKeeper()
    let c3 = controller(link3, keeper3, display: uhd, hevc: true)
    c3.start(passphrase: "correct horse")
    check(waitUntil(25) { keeper3.count >= 150 && c3.video.codec.hasPrefix("HEVC") }, "90 H.264 then 60 HEVC pictures (\(keeper3.count), \(c3.video.codec))")
    RunLoop.main.run(until: Date().addingTimeInterval(0.5))
    let log3 = c3.log.lines.map(\.text).joined(separator: "\n")
    check(link3.boxes[0].seen.videoCodecs.prefix(2) == [.h264, .hevc] && log3.contains("codec changed H.264 -> HEVC")
          && c3.video.decodeErrors == 0, "codec switched between sessions, logged, no decode error")
    c3.stop()
    _ = waitUntil(3) { !c3.link.running }
    c3.shutdown()

    section("end to end: smooth playback at real speed")
    var real = SyntheticBox.Options()
    real.pairs = 1
    let link4 = SyntheticLink(media: boxMedia, options: real)
    let times = Collector<Double>()
    let form = FormModel(defaults: nil)
    form.ssid = "Bench AP"
    let c4 = SessionController(boxLink: link4, form: form, audioOutput: false,
                               onFrame: { _ in times.add(FramePresenter.now) }, onVideoCleared: {})
    c4.setSmoothPlayback(true)
    c4.start(passphrase: "correct horse")
    check(waitUntil(12) { c4.video.decoded >= 90 }, "90 frames decoded with smooth playback on")
    RunLoop.main.run(until: Date().addingTimeInterval(0.6))
    let p = c4.pipeline.presenter.snapshot
    let t = times.all
    let gaps = zip(t, t.dropFirst()).map { $1 - $0 }.sorted()
    let median = gaps.isEmpty ? 0 : gaps[gaps.count / 2]
    check(p.smooth && p.presented >= 85 && t.count >= 85 && abs(median - 1.0 / 30) < 0.006,
          "pictures paced at the stream's 30 fps (\(t.count) shown, median gap \(Int(median * 1000)) ms, \(p.late) late, \(p.dropped) dropped)")
    check(c4.video.playback.hasPrefix("100 ms buffer") && (abs(c4.video.streamFPS - 30) < 1 || c4.video.streamFPS == 0),
          "status shows the pacing (\(c4.video.playback))")
    print("   measured (synthetic, smooth): arrival jitter \(c4.video.jitterMs) ms (peak \(c4.video.jitterPeakMs)), late \(p.late)")
    c4.stop()
    _ = waitUntil(3) { !c4.link.running }
    c4.shutdown()
}

// MARK: - run

testWire()
testSeam()
testVideoLane()
testAudioLane()
testEvents()
testSession()
testPlayout()
testCodecConfig()
testTiming()
testPresenter()
if let media = makeMedia() {
    testVideoDecode(media)
    testHevcDecode(media)
    testAudioDecode(media)
    testEndToEnd(media)
    testEndToEndHevc(media)
} else {
    print("SKIPPED  media and end-to-end tests: ffmpeg (with libx264 and aac) is not installed")
}

print("──────────────────────────────")
print("\(passes) passed, \(failures) failed")
exit(failures == 0 ? 0 : 1)
