// SeamReader.swift — host-side reassembly of the media lanes and of CH_METADATA. Swift twins of
// `ocbm_proto::seam::{SeamReader, MetaReader}`: the same framing, the same resync rule, the same tests.
//
//   media lane   [u32 BE len]["SEAV"][marker u8][body]   len counts every byte after itself
//   CH_METADATA  ["META"][u32 BE len][marker u8][body]   len = 1 + body
//
// ocbmd does not preserve message boundaries, so a message may span many OCBM frames. Not thread-safe:
// each reader belongs to the one queue that feeds its lane.

import Foundation

struct SeamMessage: Equatable {
    var marker: UInt8
    var body: [UInt8]
}

struct SeamReader {
    static let magic: [UInt8] = [0x53, 0x45, 0x41, 0x56] // "SEAV"

    private var buf: [UInt8] = []
    private var start = 0
    private let max: Int
    /// Times the reader had to scan forward to the next magic (torn or foreign bytes).
    private(set) var resyncs: UInt64 = 0

    /// `max` = the largest message accepted (a video lane needs several MiB, an audio lane a few KiB).
    init(max: Int) { self.max = max }

    /// `newSource` = the OCBM frame carried `F_NEW_SOURCE`: the producer was replaced, so a partial
    /// message still held belongs to the old one and is dropped.
    mutating func push(_ data: [UInt8], newSource: Bool) {
        if newSource {
            buf.removeAll(keepingCapacity: true)
            start = 0
        } else if start > 0 && (start == buf.count || start > 1 << 20) {
            buf.removeFirst(start)
            start = 0
        }
        buf.append(contentsOf: data)
    }

    /// The next whole message, or nil until more bytes arrive.
    mutating func next() -> SeamMessage? {
        while true {
            let avail = buf.count - start
            if avail < 9 { return nil }
            let len = Int(buf[start]) << 24 | Int(buf[start + 1]) << 16 | Int(buf[start + 2]) << 8 | Int(buf[start + 3])
            let magicOK = buf[start + 4] == Self.magic[0] && buf[start + 5] == Self.magic[1]
                && buf[start + 6] == Self.magic[2] && buf[start + 7] == Self.magic[3]
            if !magicOK || len < 5 || len > max {
                // Re-align on the next magic; a message starts 4 bytes before it. The search starts at
                // +5, so the new start is at least one byte further on.
                resyncs += 1
                if let at = Self.find(Self.magic, in: buf, from: start + 5) {
                    start = at - 4
                    continue
                }
                // A magic may straddle the next push: keep the 3 bytes it could start in and the 4 its
                // length prefix would be.
                start = buf.count - Swift.min(avail, 7)
                return nil
            }
            if avail < 4 + len { return nil }
            let msg = SeamMessage(marker: buf[start + 8], body: Array(buf[(start + 9)..<(start + 4 + len)]))
            start += 4 + len
            return msg
        }
    }

    fileprivate static func find(_ needle: [UInt8], in hay: [UInt8], from: Int) -> Int? {
        guard from >= 0, hay.count >= needle.count else { return nil }
        var i = from
        let last = hay.count - needle.count
        while i <= last {
            if hay[i] == needle[0] && hay[i + 1] == needle[1] && hay[i + 2] == needle[2] && hay[i + 3] == needle[3] {
                return i
            }
            i += 1
        }
        return nil
    }
}

struct MetaReader {
    static let magic: [UInt8] = [0x4D, 0x45, 0x54, 0x41] // "META"
    static let maxMessage = 4 * 1024 * 1024

    private var buf: [UInt8] = []
    private var start = 0
    private(set) var resyncs: UInt64 = 0

    mutating func push(_ data: [UInt8], newSource: Bool) {
        if newSource {
            buf.removeAll(keepingCapacity: true)
            start = 0
        } else if start > 0 && (start == buf.count || start > 1 << 20) {
            buf.removeFirst(start)
            start = 0
        }
        buf.append(contentsOf: data)
    }

    mutating func next() -> SeamMessage? {
        while true {
            let avail = buf.count - start
            if avail < 9 { return nil }
            let len = Int(buf[start + 4]) << 24 | Int(buf[start + 5]) << 16 | Int(buf[start + 6]) << 8 | Int(buf[start + 7])
            let magicOK = buf[start] == Self.magic[0] && buf[start + 1] == Self.magic[1]
                && buf[start + 2] == Self.magic[2] && buf[start + 3] == Self.magic[3]
            if !magicOK || len == 0 || len > Self.maxMessage {
                resyncs += 1
                if let at = SeamReader.find(Self.magic, in: buf, from: start + 1) {
                    start = at
                    continue
                }
                start = buf.count - Swift.min(avail, 3)
                return nil
            }
            if avail < 8 + len { return nil }
            let msg = SeamMessage(marker: buf[start + 8], body: Array(buf[(start + 9)..<(start + 8 + len)]))
            start += 8 + len
            return msg
        }
    }
}
