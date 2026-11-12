// AirPlayModeSession.swift — the host's CH_CTRL state machine for AirPlay-only mode, with no clock, no
// queue and no transport of its own: every input carries `now`, every effect comes back as an `Output`.
// `AirPlayModeClient` drives it against a real pipe; the tests drive it directly.
//
//   idle --start--> hello --HELLO_ACK--> awaitingAck --MODE_ACK ok--> active --stop--> leaving --> ended
//
// Rules it encodes (docs/ocbm/00_PROTOCOL.md "AirPlay-only mode", Lifecycle):
//   - HELLO is retransmitted until the box answers (a box that is still booting loses the first one).
//   - No CT_SUBSCRIBE, ever: a SUBSCRIBE makes the box leave the mode ("projection wins").
//   - CT_HEARTBEAT about once a second from the moment the mode is requested; the box leaves the mode
//     after 10 s without one. No beat is sent before the request (an unsubscribed host's beat makes
//     ocbmd answer SEV_HOST_GONE) and none after the host asks to leave.
//   - CT_AIRPLAY_STATE 00 00 00 after the ack = the box left the mode (heartbeat loss, another host's
//     SUBSCRIBE, CT_STOP). Before the ack it is stale and ignored.
//   - SEV_HOST_GONE while the mode is held = a restarted ocbmd that knows nothing of the request (it
//     is its answer to a beat from a host it has no mode or subscription for): ask again.
//   - Leaving = CT_MODE_SELECT [MODE_PROJECTION], which is acknowledged; CT_STOP if the ack never comes.

import Foundation

struct AirPlayModeSession {
    enum End: Equatable, Sendable {
        case stopped            // the host asked to leave
        case refused(UInt8)     // MODE_ACK with a status other than OK
        case noAck              // the box never answered MODE_SELECT: an ocbmd without MODE_AIRPLAY
        case boxLeft            // CT_AIRPLAY_STATE 00 00 00 while the mode was held

        var message: String {
            switch self {
            case .stopped: return "stopped"
            case .refused(let st): return "box refused the request: \(AirPlayWire.modeAckName(st))"
            case .noAck: return "no CT_MODE_ACK from the box: this ocbmd does not know MODE_AIRPLAY"
            case .boxLeft: return "the box left AirPlay-only mode"
            }
        }
    }

    enum Phase: Equatable, Sendable {
        case idle, hello, awaitingAck, active, leaving
        case ended(End)

        var isEnded: Bool { if case .ended = self { return true } else { return false } }
    }

    enum Output: Equatable {
        case send([UInt8])              // a CH_CTRL payload
        case phase(Phase)
        case helloAck(HelloAckInfo)
        case boxNotReady                // ~15 s of unanswered HELLOs; retries continue
        case state(AirPlayBoxState)
        case reRequested                // the mode was asked for again: every stream key is void
        case note(String)
    }

    static let helloFastInterval = 0.5
    static let helloFastAttempts = 30
    static let helloSlowInterval = 2.0
    static let heartbeatInterval = 1.0
    static let ackTimeout = 4.0
    static let leaveTimeout = 1.5
    /// A SEV_HOST_GONE this soon after the ack is the echo of the projection session our own request
    /// ended, not a sign that the box forgot the mode.
    static let hostGoneQuiet = 2.0

    private(set) var phase: Phase = .idle
    private(set) var boxState = AirPlayBoxState()
    private var request: AirPlayModeRequest?
    private let nonce: UInt32
    private let wantBoxLog: Bool
    private var helloAttempts = 0
    private var nextHello = 0.0
    private var nextBeat = 0.0
    private var ackDeadline = 0.0
    private var ackedAt = 0.0
    private var leaveDeadline = 0.0

    /// `nonce` = this process's HELLO instance nonce (non-zero, fixed for the process lifetime).
    init(nonce: UInt32, wantBoxLog: Bool) {
        self.nonce = nonce == 0 ? 1 : nonce
        self.wantBoxLog = wantBoxLog
    }

    private mutating func enter(_ p: Phase, _ out: inout [Output]) {
        guard p != phase else { return }
        phase = p
        if p.isEnded { request = nil } // the passphrase goes with the session
        out.append(.phase(p))
    }

    private var helloPayload: [UInt8] {
        [OCBM.ctHello, OCBM.version, UInt8(nonce & 0xff), UInt8((nonce >> 8) & 0xff),
         UInt8((nonce >> 16) & 0xff), UInt8(nonce >> 24)]
    }

    mutating func start(_ req: AirPlayModeRequest, now: Double) -> [Output] {
        guard phase == .idle else { return [] }
        var out: [Output] = []
        request = req
        helloAttempts = 1
        nextHello = now + Self.helloFastInterval
        enter(.hello, &out)
        out.append(.send(helloPayload))
        return out
    }

    /// Ask to leave. From `hello` nothing was requested, so there is nothing to undo.
    mutating func stop(now: Double) -> [Output] {
        var out: [Output] = []
        switch phase {
        case .idle, .hello:
            enter(.ended(.stopped), &out)
        case .awaitingAck, .active:
            out.append(.send([OCBM.ctModeSelect, OCBM.modeProjection]))
            leaveDeadline = now + Self.leaveTimeout
            enter(.leaving, &out)
        case .leaving, .ended:
            break
        }
        return out
    }

    mutating func tick(now: Double) -> [Output] {
        var out: [Output] = []
        switch phase {
        case .hello:
            if now >= nextHello {
                out.append(.send(helloPayload))
                helloAttempts += 1
                if helloAttempts == Self.helloFastAttempts + 1 { out.append(.boxNotReady) }
                nextHello = now + (helloAttempts <= Self.helloFastAttempts ? Self.helloFastInterval : Self.helloSlowInterval)
            }
        case .awaitingAck, .active:
            if now >= nextBeat {
                out.append(.send([OCBM.ctHeartbeat]))
                // A late tick must not be answered with a burst: one beat, then a full interval.
                nextBeat = max(nextBeat + Self.heartbeatInterval, now + Self.heartbeatInterval / 2)
            }
            if phase == .awaitingAck && now >= ackDeadline {
                enter(.ended(.noAck), &out)
            }
        case .leaving:
            if now >= leaveDeadline {
                out.append(.send([OCBM.ctStop]))
                enter(.ended(.stopped), &out)
            }
        case .idle, .ended:
            break
        }
        return out
    }

    private mutating func sendRequest(now: Double, _ out: inout [Output]) {
        guard let request else { return }
        out.append(.send(request.encode()))
        ackDeadline = now + Self.ackTimeout
        nextBeat = now + Self.heartbeatInterval
        enter(.awaitingAck, &out)
    }

    /// One CH_CTRL payload from the box.
    mutating func control(_ pl: [UInt8], now: Double) -> [Output] {
        var out: [Output] = []
        guard let op = pl.first else { return out }
        switch op {
        case OCBM.ctHelloAck:
            guard let info = AirPlayWire.parseHelloAck(pl) else { break }
            out.append(.helloAck(info))
            if let ap = info.airplayAP, let se = info.airplaySession {
                boxState = AirPlayBoxState(ap: ap, session: se, lastEnd: boxState.lastEnd)
            }
            guard phase == .hello else { break } // a late duplicate of an answered HELLO
            if !info.supportsAirPlay {
                out.append(.note("box caps 0x\(String(info.caps, radix: 16)) do not include CAP_AIRPLAY (0x40); asking anyway"))
            }
            sendRequest(now: now, &out)
        case OCBM.ctModeAck:
            guard let ack = AirPlayWire.parseModeAck(pl) else { break }
            if ack.mode == OCBM.modeAirplay && phase == .awaitingAck {
                if ack.status == OCBM.modeAckOk {
                    ackedAt = now
                    enter(.active, &out)
                    // Per-session box state (CT_LOG_CTL among it) is reset by the mode switch: arm after the ack.
                    if wantBoxLog { out.append(.send(OCBM.logCtl(enabled: true, capKB: 0))) }
                } else {
                    enter(.ended(.refused(ack.status)), &out)
                }
            } else if ack.mode == OCBM.modeProjection && phase == .leaving {
                enter(.ended(.stopped), &out)
            }
        case OCBM.ctAirplayState:
            guard let st = AirPlayWire.parseState(pl) else { break }
            boxState = st
            out.append(.state(st))
            if st.isModeLeft && phase == .active {
                enter(.ended(.boxLeft), &out)
            }
        case OCBM.ctSessionEvent:
            guard pl.count >= 2, pl[1] == OCBM.sevHostGone, phase == .active,
                  now - ackedAt >= Self.hostGoneQuiet else { break }
            out.append(.note("box answered a heartbeat with SEV_HOST_GONE: it holds no mode for this host (ocbmd restarted); requesting the mode again"))
            out.append(.reRequested)
            sendRequest(now: now, &out)
        default:
            break
        }
        return out
    }
}
