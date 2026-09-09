// OCBMControlRelay.swift — HOST side of the app-driven SETUP control relay (plan P3).
//
// Faithful Swift port of the seam codec + message layer in the box's
// `crates/vendor/receiver/src/relay.rs`. The box terminates + decrypts RTSP/SETUP, runs its own
// unmodified `AvSession` FIRST (pre-bind + oracle), then relays the decrypted request PLUS its local
// response to us over OCBM `CH_RTSP` (0x0041). We author the response the phone actually sees and send
// it back as RS_RESP; any failure on our side (RS_ERR / timeout) makes the box fall back to its local
// response, so the phone never sees a relay-caused error.
//
// SEAM FRAMING — `[u32 BE "RTSP"][u32 BE len][msg]`, len <= rtspSeamMax. CH_RTSP rides ocbmd's out_hi
// queue whose overflow policy is *clear the queue* (no per-message resync), so a truncated frame can
// land mid-stream; the magic lets the reassembler resync by scanning forward. This is the same
// discipline as OCBMAVDecrypt.swift's SEAM_MAGIC video reassembly and MetadataWindow.swift's "META"
// v2 seam — a lost message is recovered by the box's rpc timeout -> local fallback, never by wedging.
//
// Pure + IOKit-free by construction: bytes come in through `feed(_:)`, replies go out through an
// INJECTED send closure `(UInt16, [UInt8]) -> Void` (the OCBM channel + the seam-framed message), and
// request authoring is delegated to `onRequest`. That keeps this unit-testable headless (tests/run_tests.sh)
// and lets AppDelegate glue it to the live OCBMClient. Constants are mirrored from relay.rs in
// OCBMFraming.swift (OCBM.rsOpen/rsReq/... and OCBM.rtspSeamMagic/rtspSeamMax).

import Foundation

/// Streaming seam-frame decoder for `[u32 BE "RTSP"][u32 BE len][msg]`. Push raw CH_RTSP bytes, pop
/// complete messages. Resyncs by scanning for the magic; an implausible declared length (> rtspSeamMax)
/// is treated as garbage — skip one byte and rescan. Port of relay.rs `FrameBuf`.
struct RTSPFrameBuf {
    private var buf: [UInt8] = []

    mutating func push(_ data: [UInt8]) { buf.append(contentsOf: data) }

    /// First index at which the 4-byte magic appears, or nil.
    private func magicIndex() -> Int? {
        let m = OCBM.rtspSeamMagic
        guard buf.count >= 4 else { return nil }
        var i = 0
        let last = buf.count - 4
        while i <= last {
            if buf[i] == m[0], buf[i + 1] == m[1], buf[i + 2] == m[2], buf[i + 3] == m[3] { return i }
            i += 1
        }
        return nil
    }

    /// Pop the next complete message, or nil if none is buffered yet. `nil` means "need more bytes",
    /// not exhaustion (matches relay.rs `FrameBuf::next`, deliberately not an Iterator).
    mutating func next() -> [UInt8]? {
        while true {
            // Resync: drop everything ahead of the first magic. If no magic is present, keep only the
            // last 3 bytes (a magic may straddle two pushes) so garbage can't grow the buffer.
            if let n = magicIndex() {
                if n > 0 { buf.removeFirst(n) }
            } else {
                let keep = min(buf.count, 3)
                if buf.count > keep { buf.removeFirst(buf.count - keep) }
                return nil
            }
            if buf.count < 8 { return nil } // waiting for the length word
            let len = (Int(buf[4]) << 24) | (Int(buf[5]) << 16) | (Int(buf[6]) << 8) | Int(buf[7])
            if len > OCBM.rtspSeamMax {
                // A magic followed by a garbled length (or payload bytes that spell the magic). Skip
                // ONE byte and rescan — never trust the bogus length for the skip.
                buf.removeFirst(1)
                continue
            }
            if buf.count < 8 + len { return nil } // valid header, waiting for the body
            let msg = Array(buf[8..<8 + len])
            buf.removeFirst(8 + len)
            return msg
        }
    }
}

/// The host end of the SETUP relay. Reassembles CH_RTSP seam bytes, parses RS_OPEN / RS_REQ / RS_CLOSE,
/// and emits RS_RESP / RS_ERR through the injected `send` closure. Authoring is delegated to `onRequest`.
final class OCBMControlRelay {
    /// Injected transport: `(channel, seam-framed bytes)`. Prod wires it to OCBMClient.sendControlRelay;
    /// tests inject a recorder. Keeping the channel in the signature matches the plan's contract even
    /// though the relay always uses `OCBM.chRtsp`.
    private let send: (UInt16, [UInt8]) -> Void

    /// The relay-connection id of the CURRENT connection (from the latest RS_OPEN). 0 = none yet.
    /// Messages for any other conn are dropped (a hijacked-away / stale connection — the box's single
    /// serve FIFO guarantees RS_CLOSE(old) precedes RS_OPEN(new), so we only ever track the newest).
    private(set) var currentConn: UInt32 = 0

    /// RS_OPEN handler: a fresh phone connection began (reset authoring state here).
    var onOpen: ((_ conn: UInt32, _ wireless: Bool, _ cfgCRC: UInt32, _ ctx: [UInt8]) -> Void)?
    /// RS_REQ handler: author the response for a SETUP/RECORD/TEARDOWN exchange. Return `(status, body)`
    /// for a request/response route (SETUP/RECORD) — RS_RESP is sent for you. Return nil to send RS_ERR
    /// (box falls back to local). For a NOTIFY route (TEARDOWN) the return value is ignored (no reply owed).
    var onRequest: ((_ conn: UInt32, _ cseq: UInt32, _ route: UInt8, _ notify: Bool,
                     _ local: [UInt8], _ req: [UInt8]) -> (status: UInt16, body: [UInt8])?)?
    /// RS_CLOSE handler: the connection went away (reset authoring state here).
    var onClose: ((_ conn: UInt32, _ reason: UInt8) -> Void)?
    /// Optional diagnostic sink (nil = muted). Prod wires it to os_log; tests capture or ignore.
    var log: ((String) -> Void)?

    private var frameBuf = RTSPFrameBuf()

    init(send: @escaping (UInt16, [UInt8]) -> Void) {
        self.send = send
    }

    /// Feed raw CH_RTSP payload bytes (called from OCBMClient.onRead). Reassembles + dispatches.
    func feed(_ payload: [UInt8]) {
        frameBuf.push(payload)
        while let msg = frameBuf.next() {
            dispatch(msg)
        }
    }

    // MARK: - Message codec (mirrors relay.rs parse_header / msg_resp / msg_err)

    /// Parse an inbound message's common header: `(op, conn, cseq, rest)`. Port of relay.rs parse_header.
    static func parseHeader(_ msg: [UInt8]) -> (op: UInt8, conn: UInt32, cseq: UInt32, rest: ArraySlice<UInt8>)? {
        guard msg.count >= 9 else { return nil }
        let op = msg[0]
        let conn = UInt32(msg[1]) | (UInt32(msg[2]) << 8) | (UInt32(msg[3]) << 16) | (UInt32(msg[4]) << 24)
        let cseq = UInt32(msg[5]) | (UInt32(msg[6]) << 8) | (UInt32(msg[7]) << 16) | (UInt32(msg[8]) << 24)
        return (op, conn, cseq, msg[9...])
    }

    private static func header(_ op: UInt8, _ conn: UInt32, _ cseq: UInt32) -> [UInt8] {
        var m = [UInt8]()
        m.reserveCapacity(9)
        m.append(op)
        for v in [conn, cseq] {
            m.append(UInt8(v & 0xFF)); m.append(UInt8((v >> 8) & 0xFF))
            m.append(UInt8((v >> 16) & 0xFF)); m.append(UInt8((v >> 24) & 0xFF))
        }
        return m
    }

    /// Build an RS_RESP: `[hdr][status u16 LE][body]`.
    static func msgResp(conn: UInt32, cseq: UInt32, status: UInt16, body: [UInt8]) -> [UInt8] {
        var m = header(OCBM.rsResp, conn, cseq)
        m.append(UInt8(status & 0xFF)); m.append(UInt8((status >> 8) & 0xFF))
        m.append(contentsOf: body)
        return m
    }

    /// Build an RS_ERR: `[hdr][code u8]`.
    static func msgErr(conn: UInt32, cseq: UInt32, code: UInt8) -> [UInt8] {
        var m = header(OCBM.rsErr, conn, cseq)
        m.append(code)
        return m
    }

    /// Frame one message for the seam: `[u32 BE "RTSP"][u32 BE len][msg]`. Port of relay.rs frame_msg.
    static func frameMsg(_ msg: [UInt8]) -> [UInt8] {
        var out = [UInt8]()
        out.reserveCapacity(8 + msg.count)
        out.append(contentsOf: OCBM.rtspSeamMagic)
        let len = UInt32(msg.count)
        out.append(UInt8((len >> 24) & 0xFF)); out.append(UInt8((len >> 16) & 0xFF))
        out.append(UInt8((len >> 8) & 0xFF)); out.append(UInt8(len & 0xFF))
        out.append(contentsOf: msg)
        return out
    }

    // MARK: - Dispatch

    private func dispatch(_ msg: [UInt8]) {
        guard let (op, conn, cseq, rest) = Self.parseHeader(msg) else {
            log?("inbound control-relay message too short (\(msg.count) B) — dropped")
            return
        }
        switch op {
        case OCBM.rsOpen:
            // [ver][flags][cfg_crc u32 LE][ctx_len u32 LE][ctx]
            let r = Array(rest)
            guard r.count >= 10 else { log?("RS_OPEN too short — dropped"); return }
            let flags = r[1]
            let wireless = (flags & OCBM.openFlagWireless) != 0
            let cfgCRC = UInt32(r[2]) | (UInt32(r[3]) << 8) | (UInt32(r[4]) << 16) | (UInt32(r[5]) << 24)
            let ctxLen = Int(r[6]) | (Int(r[7]) << 8) | (Int(r[8]) << 16) | (Int(r[9]) << 24)
            let ctx = (r.count >= 10 + ctxLen) ? Array(r[10..<10 + ctxLen]) : []
            currentConn = conn
            log?("RS_OPEN conn=\(conn) wireless=\(wireless) cfg_crc=0x\(String(cfgCRC, radix: 16))")
            onOpen?(conn, wireless, cfgCRC, ctx)

        case OCBM.rsReq:
            // [route][flags][local_len u32 LE][local][req]
            let r = Array(rest)
            guard r.count >= 6 else { log?("RS_REQ too short — dropped"); return }
            let route = r[0]
            let notify = (r[1] & OCBM.reqFlagNotify) != 0
            let localLen = Int(r[2]) | (Int(r[3]) << 8) | (Int(r[4]) << 16) | (Int(r[5]) << 24)
            guard r.count >= 6 + localLen else { log?("RS_REQ local length overruns message — dropped"); return }
            let local = Array(r[6..<6 + localLen])
            let req = Array(r[(6 + localLen)...])
            // Drop messages for a stale / hijacked-away connection.
            guard conn == currentConn else {
                log?("RS_REQ for non-current conn \(conn) (current \(currentConn)) — dropped")
                return
            }
            let result = onRequest?(conn, cseq, route, notify, local, req)
            if notify { return } // fire-and-forget (TEARDOWN) — no reply owed
            if let (status, body) = result {
                send(OCBM.chRtsp, Self.frameMsg(Self.msgResp(conn: conn, cseq: cseq, status: status, body: body)))
            } else {
                // No handler / declined — RS_ERR so the box falls back to its in-hand local response.
                send(OCBM.chRtsp, Self.frameMsg(Self.msgErr(conn: conn, cseq: cseq, code: 0)))
            }

        case OCBM.rsClose:
            let reason = rest.first ?? OCBM.closeEOF
            log?("RS_CLOSE conn=\(conn) reason=\(reason)")
            if conn == currentConn { currentConn = 0 }
            onClose?(conn, reason)

        default:
            log?("unexpected control-relay op 0x\(String(op, radix: 16)) (conn \(conn) cseq \(cseq)) — dropped")
        }
    }
}
