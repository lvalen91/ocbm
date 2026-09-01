// ──────────────────────────────────────────────────────────────────────────────
// USBTransport.swift — CPC200-CCPA bulk pipe I/O (OCBM raw-bulk transport)
// ──────────────────────────────────────────────────────────────────────────────
//
// Role
// ────
// USBTransport owns the claimed OCBM interface's two bulk endpoints and moves
// raw bytes: the read loop hands every inbound bulk chunk to `rawReadHandler`
// (the OCBM reassembler, `OCBMReassembler`, does all framing), and `writeBulkRaw`
// sends an already-framed OCBM frame verbatim. It adapts to `RawBulkTransport`
// so `OCBMClient` can drive it without knowing about IOKit. There is NO message
// framing in this layer — OCBM's 16-byte header + resync lives in OCBMFraming.swift.
// (The dormant legacy Carlinkit 0x55AA55AA framing that used to live here was
// removed with the Protocol/ directory; the OCBM box never speaks it.)
//
// Threading model
// ───────────────
//   • readQueue  — Dedicated serial DispatchQueue running a blocking ReadPipe
//                  loop. Blocking is intentional: the adapter streams H.264
//                  video at up to 60 fps and raw PCM audio continuously, so a
//                  tight synchronous loop minimizes latency and copies.
//   • writeQueue — Dedicated serial DispatchQueue serializing WritePipe calls.
//                  Heartbeats (every 2s), commands, and microphone audio all
//                  share the OUT pipe; the queue ensures they don't interleave.
//   • isRunning  — Mutex<Bool> for clean shutdown. Checked once per ReadPipe
//                  iteration; set to false by stop() to break the loop.
//
// Error recovery
// ──────────────
// USB bulk endpoints can stall when the device NAKs or the host controller
// encounters an error. The standard recovery is ClearPipeStallBothEnds (sends
// CLEAR_FEATURE(HALT) to the device endpoint and resets the host-side data
// toggle) followed by a retry. Both the read loop and write path implement
// this pattern.
//
// Lifecycle
// ─────────
// USBTransport owns both IOKit COM refs handed over by USBInterfaceClaimer:
// the opened interface and the opened device. stop() aborts any in-flight
// pipe transfer (unblocking the read loop), drains both queues, then closes
// and releases both refs. Without this, every session leaked an exclusive-
// access user client and the next USBInterfaceOpen failed.
// ──────────────────────────────────────────────────────────────────────────────

import Foundation
import IOKit.usb
import os
import Synchronization

// MARK: - Transport Delegate

protocol USBTransportDelegate: AnyObject {
    func transportDidEncounterError(_ transport: USBTransport, error: Error)
    func transportDidDisconnect(_ transport: USBTransport)
}

// MARK: - Transport Errors

enum USBTransportError: Error, Sendable {
    case writeFailure(IOReturn)
}

// MARK: - USB Transport

/// Owns the bulk pipe I/O lifecycle for a claimed CPC200-CCPA USB interface.
///
/// Reads are continuous: the adapter pushes framed OCBM video, audio, and control
/// without being polled; each raw chunk goes straight to `rawReadHandler`. Writes
/// are on-demand: SUBSCRIBE, heartbeats, HID input, and microphone PCM are sent as
/// already-framed OCBM frames via `writeBulkRaw`.
// @unchecked Sendable invariant: iface/device/pipes are immutable lets (raw IOKit COM pointers that
// cannot conform to Sendable); isRunning/isClosed are Mutex; consecutiveWriteFailures is
// writeQueue-confined; rawReadHandler is set once before start; delegate is set once during setup.
final class USBTransport: @unchecked Sendable {

    private static let logger = Logger(subsystem: "com.carlink.usb", category: "Transport")

    weak var delegate: USBTransportDelegate?

    /// OCBM raw-bulk mode: when set (before `startReadLoop`), the read loop delivers each raw bulk-IN
    /// chunk to this handler and SKIPS the legacy 0x55AA55AA reframing — the OCBM reassembler
    /// (`OCBMReassembler`) does the framing instead. Set once during setup, read on the read queue.
    var rawReadHandler: (([UInt8]) -> Void)?

    private let iface: UnsafeMutablePointer<UnsafeMutablePointer<IOUSBInterfaceInterface300>>
    private let device: UnsafeMutablePointer<UnsafeMutablePointer<IOUSBDeviceInterface300>>
    private let bulkInPipe: UInt8
    private let bulkOutPipe: UInt8

    private let readQueue = DispatchQueue(label: "usb.read", qos: .userInitiated)
    private let writeQueue = DispatchQueue(label: "usb.write", qos: .userInitiated)

    private let isRunning = Mutex(false)
    private let isClosed = Mutex(false)
    private let maxConsecutiveErrors = 5

    // Consecutive writeBulkRaw failures (confined to writeQueue). Mirrors the read loop's
    // `consecutiveErrors`: a persistently dead OUT pipe is a lost session even while video
    // still flows IN (every touch/command dies), so at `maxConsecutiveErrors` the same
    // disconnect escalation fires. Reset on any successful write; fires once per streak.
    private var consecutiveWriteFailures = 0

    // kIOUSBTransactionTimeout — a function-like macro chain in IOUSBLib.h
    // that doesn't import into Swift (iokit_usb_err(0x51)).
    private static let kUSBTransactionTimeout = IOReturn(bitPattern: 0xe0004051)

    // Pipe transfer timeouts (ms). Bounded transfers are what make stop()
    // reliable: AbortPipe only cancels an IN-FLIGHT transfer, so a transfer
    // submitted just after the abort would otherwise park forever on a quiet
    // or wedged device, and the teardown queued behind it would never free
    // the exclusive-access refs.
    private static let readNoDataTimeoutMs: UInt32 = 1000
    private static let writeNoDataTimeoutMs: UInt32 = 2000
    private static let writeCompletionTimeoutMs: UInt32 = 5000
    // OCBM raw writes (writeBulkRaw) are <=64 KiB frames — ~2 ms on a healthy high-speed pipe, so a
    // 500 ms NAK window is already enormous. Short timeouts keep a wedged write's hold on the control
    // queue far under the box's 10 s heartbeat grace (ocbmd HEARTBEAT_GRACE): worst single-write hold
    // 1.5 s, and with maxConsecutiveErrors=5 the worst cumulative heartbeat gap before the transport
    // self-disconnects is 5 x 1.5 = 7.5 s < 10 s — so a wedged OUT pipe can no longer starve heartbeats
    // long enough for the box to declare the host GONE (the root of the lost-command chain, audit C1).
    private static let rawWriteNoDataTimeoutMs: UInt32 = 500
    private static let rawWriteCompletionTimeoutMs: UInt32 = 1500

    init(interfaceRef: UnsafeMutablePointer<UnsafeMutablePointer<IOUSBInterfaceInterface300>>,
         deviceRef: UnsafeMutablePointer<UnsafeMutablePointer<IOUSBDeviceInterface300>>,
         bulkIn: UInt8, bulkOut: UInt8) {
        self.iface = interfaceRef
        self.device = deviceRef
        self.bulkInPipe = bulkIn
        self.bulkOutPipe = bulkOut
    }

    deinit {
        // If stop() was never called, no queue work can be in flight here
        // (readLoop and writeBulkRaw keep self alive), so close inline.
        let alreadyClosed = isClosed.withLock { closed in
            let was = closed
            closed = true
            return was
        }
        if !alreadyClosed {
            releaseIOKitRefs()
        }
    }

    // MARK: - Read Loop

    func startReadLoop() {
        guard !isClosed.withLock({ $0 }) else { return }
        let alreadyRunning = isRunning.withLock { running in
            let was = running
            running = true
            return was
        }
        guard !alreadyRunning else { return }
        readQueue.async { [weak self] in
            self?.readLoop()
        }
    }

    /// Aborts in-flight transfers, drains both queues, then closes and
    /// releases the interface and device. Idempotent; safe from any thread
    /// except the transport's own queues.
    func stop() {
        let alreadyClosed = isClosed.withLock { closed in
            let was = closed
            closed = true
            return was
        }
        guard !alreadyClosed else { return }
        isRunning.withLock { $0 = false }

        // Unblock a ReadPipe/WritePipe parked on either endpoint so the read
        // loop exits and any wedged write returns kIOReturnAborted.
        _ = iface.pointee.pointee.AbortPipe(iface, bulkInPipe)
        _ = iface.pointee.pointee.AbortPipe(iface, bulkOutPipe)

        // Teardown runs behind the exiting read loop; the block retains self
        // so the COM refs stay valid until they are released.
        readQueue.async { [self] in
            writeQueue.sync {}
            releaseIOKitRefs()
        }
    }

    private func releaseIOKitRefs() {
        _ = iface.pointee.pointee.USBInterfaceClose(iface)
        _ = iface.pointee.pointee.Release(iface)
        _ = device.pointee.pointee.USBDeviceClose(device)
        _ = device.pointee.pointee.Release(device)
    }

    private func readLoop() {
        var readBuffer = [UInt8](repeating: 0, count: 65536)
        var consecutiveErrors = 0

        while isRunning.withLock({ $0 }) {
            var bytesRead: UInt32 = UInt32(readBuffer.count)
            let kr = readBuffer.withUnsafeMutableBufferPointer { ptr in
                iface.pointee.pointee.ReadPipeTO(
                    iface, bulkInPipe, ptr.baseAddress, &bytesRead,
                    Self.readNoDataTimeoutMs, 0
                )
            }

            if kr == Self.kUSBTransactionTimeout {
                // No data within the window — not an error, just lets the loop observe isRunning/stop()
                // promptly instead of parking in the kernel forever. (ReadPipeTO does NOT update
                // bytesRead on a no-data timeout — it keeps its initial buffer-size value — so there are
                // no partial bytes to recover here; confirmed empirically 2026-08-24.)
                if !isRunning.withLock({ $0 }) { break }
                consecutiveErrors = 0
                // Deliberately NO ClearPipeStallBothEnds on a clean no-data timeout: it is not a device
                // STALL (the device sent no STALL handshake — it simply had no data), so clearing would
                // needlessly reset the bulk-IN DATA0/1 toggle on a healthy pipe. Verified over 5-min AA
                // sessions with zero read errors. Real stalls are still cleared in the error branch below.
                continue
            }

            if kr != kIOReturnSuccess {
                if !isRunning.withLock({ $0 }) { break }
                consecutiveErrors += 1
                Self.logger.error("Read error: \(UInt32(bitPattern: kr), format: .hex, privacy: .public) (count=\(consecutiveErrors, privacy: .public))")
                if consecutiveErrors >= maxConsecutiveErrors {
                    // 5 consecutive failures likely means the adapter was unplugged
                    // or the firmware crashed. Signal disconnect to tear down the session.
                    delegate?.transportDidDisconnect(self)
                    break
                }
                // Clear a stall on both the device endpoint and the host-side
                // data toggle. ClearPipeStall alone only resets the host side,
                // so a device-side halt would re-fail every retry.
                _ = iface.pointee.pointee.ClearPipeStallBothEnds(iface, bulkInPipe)
                continue
            }

            consecutiveErrors = 0
            guard bytesRead > 0 else { continue }

            // Hand each raw bulk-IN chunk to the OCBM reassembler (via rawReadHandler). All OCBM
            // framing/resync lives in OCBMReassembler, so this transport does no message parsing of
            // its own. In the OCBM app the handler is always installed before startReadLoop().
            if let raw = rawReadHandler {
                raw(Array(readBuffer[0..<Int(bytesRead)]))
                continue
            }
        }
    }

    // MARK: - Raw bulk write (OCBM: the caller supplies an already-framed OCBM frame, no header added)

    /// Write raw bytes (a full OCBM frame) to the OUT pipe, synchronously on the write queue. Returns
    /// success. Used for the OCBM control plane (SUBSCRIBE / heartbeat / input). On failure the frame
    /// is DROPPED, not retried (audit C4) — see below. Bounded by the short raw-write timeouts (C1).
    @discardableResult
    func writeBulkRaw(_ bytes: [UInt8]) -> Bool {
        writeQueue.sync { [self] in
            guard !isClosed.withLock({ $0 }) else { return false }
            let writeStart = Date()
            var buf = bytes
            let n = UInt32(buf.count) // hoist out of withUnsafeMutableBytes (exclusivity)
            let kr = buf.withUnsafeMutableBytes { ptr in
                iface.pointee.pointee.WritePipeTO(
                    iface, bulkOutPipe, ptr.baseAddress, n,
                    Self.rawWriteNoDataTimeoutMs, Self.rawWriteCompletionTimeoutMs)
            }
            if kr == kIOReturnSuccess {
                // A write that SUCCEEDS but takes hundreds of ms is the invisible failure mode: it
                // starves the 1 Hz heartbeat (shared serial queue) and delays AA's per-frame ACKs
                // toward the phone's watchdog. Only outright failures were logged before, so a run of
                // slow-but-successful writes left no trace at all.
                let ms = Date().timeIntervalSince(writeStart) * 1000
                if ms > 200 {
                    Self.logger.error("USB write took \(Int(ms), privacy: .public) ms (\(bytes.count, privacy: .public) B) — pipe stalling")
                }
                consecutiveWriteFailures = 0
                return true
            }
            // A write aborted by stop() is expected during teardown — do not
            // start a new transfer on the closing interface.
            if isClosed.withLock({ $0 }) { return false }
            // NO blind full-frame retry (audit C4): a failed WritePipeTO may have already put a
            // PARTIAL frame on the wire, and re-sending the whole frame would duplicate those bytes
            // and desync the box's OCBM reassembler while falsely returning success — and no
            // synchronous-write IOReturn guarantees zero bytes were transferred. Losing the
            // interrupted frame is the correct, already-designed failure mode: the NEXT frame starts
            // with the OCBM magic, which is the box reassembler's resync marker. Clear the stall so
            // that next frame goes out clean, report this one lost, and let the box/caller recover.
            _ = iface.pointee.pointee.ClearPipeStallBothEnds(iface, bulkOutPipe)
            Self.logger.error("Raw bulk write failed (\(n, privacy: .public) B): \(UInt32(bitPattern: kr), format: .hex, privacy: .public) — frame dropped, stall cleared")
            delegate?.transportDidEncounterError(self, error: USBTransportError.writeFailure(kr))
            consecutiveWriteFailures += 1
            if consecutiveWriteFailures == maxConsecutiveErrors {
                // Same 5-error rule as the read loop: the OUT pipe is dead, the session is a
                // zombie (video flows, every touch dies) — signal disconnect to tear it down.
                // `==` (not `>=`) so one dead pipe fires the escalation exactly once.
                Self.logger.error("Raw bulk write failed \(self.consecutiveWriteFailures, privacy: .public)x consecutively — OUT pipe dead, signaling disconnect")
                delegate?.transportDidDisconnect(self)
            }
            return false
        }
    }
}

// MARK: - RawBulkTransport conformance (OCBM host)

extension USBTransport: RawBulkTransport {
    func writeBulk(_ bytes: [UInt8]) -> Bool { writeBulkRaw(bytes) }
    func setReadHandler(_ handler: @escaping ([UInt8]) -> Void) { rawReadHandler = handler }
    func start() { startReadLoop() }
    // `stop()` already satisfies the protocol.
}
