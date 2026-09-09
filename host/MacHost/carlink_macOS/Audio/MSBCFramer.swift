// MSBCFramer.swift — the HFP/eSCO transport around MSBCCodec: H2 headers, resync, packet-loss
// concealment, and the two adapters the app actually calls.
//
// THE AIR FORMAT (HFP 1.6 §5.7.4, Erratum 2409). A wideband SCO link is opened in TRANSPARENT mode:
// the controller does no decoding, so every read off the socket is raw air data. One packet is
//
//     [0x01][sn][ 57-byte mSBC frame ][0x00]      = 60 bytes = 7.5 ms
//      \______/
//       H2 header: 0x01, then a sequence byte cycling 0x08 → 0x38 → 0xC8 → 0xF8
//
// The sequence nibble is duplicated inside the byte (0b00/0b01/0b10/0b11 doubled), which is why the
// four values are so far apart in Hamming distance — that is the whole point, it survives a bit
// error. The trailing byte is padding to reach the 60-byte eSCO payload; it is not part of SBC.
//
// WHY A RESYNCHRONISING FRAMER AND NOT A LENGTH SPLIT. The box hands us each SCO read verbatim
// (SEAM_PKT_PLAIN, `ocbm-proto::SEAM_CODEC_MSBC`), and a read is not promised to be one packet: a
// short read, a coalesced pair, or a lost packet all reach us as "some bytes". Splitting the byte
// stream by a fixed 60 would silently decode garbage forever after the first odd-sized read. So:
// scan for the H2 header (confirmed by the mSBC syncword 0xAD immediately behind it), carry a
// partial across chunk boundaries, and use the sequence number — the only loss signal the air
// format has — to count what never arrived.
//
// THREADING: both adapters are single-threaded (a codec delay line each). The downlink one runs on
// the audio decrypt queue, the uplink one on the mic IO thread.

import Foundation

// MARK: - H2 header

enum MSBCH2 {
    static let syncByte: UInt8 = 0x01
    /// Sequence bytes in transmission order.
    static let sequenceBytes: [UInt8] = [0x08, 0x38, 0xC8, 0xF8]
    /// Pad byte that completes the 60-byte eSCO payload.
    static let padByte: UInt8 = 0x00

    static func sequenceIndex(of b: UInt8) -> Int? { sequenceBytes.firstIndex(of: b) }
}

// MARK: - Byte-stream framer

/// Turns an arbitrarily chunked transparent-eSCO byte stream into whole mSBC frames plus explicit
/// loss events. A value type: the caller owns it, no locking.
struct MSBCFramer {
    enum Event: Equatable {
        /// One complete 57-byte mSBC frame, H2 header stripped.
        case frame([UInt8])
        /// `n` packets the sequence number says never arrived (1…3; a longer dropout aliases).
        case lost(Int)
    }

    /// Ceiling on the reassembly buffer. 4 KiB is ~68 packets — far past any plausible coalesced
    /// read — so hitting it means the lane is carrying something that is not mSBC and the oldest
    /// bytes are worthless.
    static let maxBuffered = 4096

    private var buf: [UInt8] = []
    private var lastSeq: Int?

    /// Frames handed out.
    private(set) var framesOut = 0
    /// Packets the sequence numbers say were dropped in flight.
    private(set) var lostPackets = 0
    /// Times more than a pad byte of junk was skipped to relock on an H2 header.
    private(set) var resyncs = 0

    mutating func reset() {
        buf.removeAll(keepingCapacity: true)
        lastSeq = nil
    }

    mutating func push(_ bytes: [UInt8]) -> [Event] {
        buf.append(contentsOf: bytes)
        if buf.count > Self.maxBuffered {
            buf.removeFirst(buf.count - Self.maxBuffered)
            resyncs += 1
            lastSeq = nil
        }

        var events: [Event] = []
        var idx = 0
        while true {
            guard let h = headerIndex(from: idx) else {
                // Nothing lockable. Only the last two bytes can still be the head of a header
                // (0x01, or 0x01 + a sequence byte) — everything before them is junk.
                let keep = max(idx, buf.count - 2)
                noteSkip(keep - idx)
                idx = keep
                break
            }
            guard buf.count - h >= 2 + MSBC.frameBytes else {
                noteSkip(h - idx)
                idx = h // header seen, frame still arriving — keep it and wait
                break
            }
            noteSkip(h - idx)
            let sn = MSBCH2.sequenceIndex(of: buf[h + 1])! // headerIndex only matches valid ones
            if let last = lastSeq {
                let gap = (sn - last - 1 + 4) % 4
                if gap > 0 {
                    lostPackets += gap
                    events.append(.lost(gap))
                }
            }
            lastSeq = sn
            events.append(.frame(Array(buf[(h + 2)..<(h + 2 + MSBC.frameBytes)])))
            framesOut += 1
            idx = h + 2 + MSBC.frameBytes
        }
        if idx > 0 { buf.removeFirst(idx) }
        return events
    }

    /// A header is `0x01`, a valid sequence byte, and the mSBC syncword behind it. Requiring the
    /// syncword is what makes a relock trustworthy: 0x01 followed by 0x08 occurs inside compressed
    /// audio often enough to matter, 0x01 0x08 0xAD does not.
    private func headerIndex(from start: Int) -> Int? {
        var i = start
        while i + 2 < buf.count {
            if buf[i] == MSBCH2.syncByte,
               MSBCH2.sequenceIndex(of: buf[i + 1]) != nil,
               buf[i + 2] == MSBC.syncword {
                return i
            }
            i += 1
        }
        return nil
    }

    /// One skipped byte is the packet's own pad byte and is expected; more than that is a relock.
    private mutating func noteSkip(_ n: Int) {
        if n > 1 { resyncs += 1 }
    }
}

// MARK: - Downlink adapter (box → speaker)

/// Framer + decoder + PLC: raw SEAM_PKT_PLAIN payloads in, 16 kHz mono S16LE out.
final class MSBCTelephonyDecoder {
    private var framer = MSBCFramer()
    private let decoder = MSBCDecoder()
    private var lastPCM: [Int16]?
    private var consecutiveConcealed = 0

    private(set) var framesDecoded = 0
    /// Frames synthesised by concealment (sequence gaps + decode failures).
    private(set) var plcFrames = 0
    private(set) var decodeFailures = 0
    var resyncs: Int { framer.resyncs }
    var lostPackets: Int { framer.lostPackets }

    func reset() {
        framer.reset()
        decoder.reset()
        lastPCM = nil
        consecutiveConcealed = 0
    }

    /// Decode one seam payload. Returns S16LE PCM at 16 kHz mono — 240 bytes per recovered frame,
    /// possibly zero bytes (a fragment that did not complete a frame) or several frames' worth.
    func decode(_ payload: Data) -> Data {
        var out = Data()
        for event in framer.push([UInt8](payload)) {
            switch event {
            case .lost(let n):
                for _ in 0..<n { append(conceal(), to: &out) }
            case .frame(let f):
                switch decoder.decode(f[...]) {
                case .success(let pcm):
                    framesDecoded += 1
                    consecutiveConcealed = 0
                    lastPCM = pcm
                    append(pcm, to: &out)
                case .failure:
                    decodeFailures += 1
                    append(conceal(), to: &out)
                }
            }
        }
        return out
    }

    /// Simple PLC: repeat the last good frame faded linearly to zero, then silence. Repeating it at
    /// full level would buzz on a long dropout; substituting silence immediately clicks.
    private func conceal() -> [Int16] {
        plcFrames += 1
        defer { consecutiveConcealed += 1 }
        guard let last = lastPCM, consecutiveConcealed == 0 else {
            return [Int16](repeating: 0, count: MSBC.samplesPerFrame)
        }
        let n = MSBC.samplesPerFrame
        return (0..<n).map { i in
            let gain = 1.0 - Double(i) / Double(n - 1)
            return Int16(clamping: Int((Double(last[i]) * gain).rounded()))
        }
    }

    private func append(_ pcm: [Int16], to data: inout Data) {
        for s in pcm {
            let u = UInt16(bitPattern: s.littleEndian)
            data.append(UInt8(u & 0xFF))
            data.append(UInt8(u >> 8))
        }
    }
}

// MARK: - Uplink adapter (mic → box)

/// Encoder + H2 packetiser: 7.5 ms of 16 kHz mono S16LE in, one 60-byte eSCO packet out. The box
/// writes what this returns to the SCO socket verbatim, so the pad byte and the cycling sequence
/// number are this side's responsibility.
final class MSBCUplinkEncoder {
    private let encoder = MSBCEncoder()
    private var seq = 0
    private(set) var packetsOut = 0

    /// Bytes of PCM one packet consumes: 120 samples × 2.
    static let pcmBytesPerPacket = MSBC.samplesPerFrame * 2

    func reset() {
        encoder.reset()
        seq = 0
    }

    func packet(from pcm: Data) -> Data? {
        guard pcm.count == Self.pcmBytesPerPacket else { return nil }
        var samples = [Int16](repeating: 0, count: MSBC.samplesPerFrame)
        pcm.withUnsafeBytes { raw in
            for i in 0..<MSBC.samplesPerFrame { // loadUnaligned: Data promises no 2-byte alignment
                samples[i] = Int16(littleEndian: raw.loadUnaligned(fromByteOffset: i * 2, as: Int16.self))
            }
        }
        guard let frame = encoder.encode(samples) else { return nil }
        var out = Data(capacity: MSBC.packetBytes)
        out.append(MSBCH2.syncByte)
        out.append(MSBCH2.sequenceBytes[seq])
        out.append(contentsOf: frame)
        out.append(MSBCH2.padByte)
        seq = (seq + 1) % MSBCH2.sequenceBytes.count
        packetsOut += 1
        return out
    }
}
