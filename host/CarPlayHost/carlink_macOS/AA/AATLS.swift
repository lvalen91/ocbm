// AATLS.swift — encapsulated TLS 1.2 for the Android Auto handshake, in Swift.
//
// Mirrors the Rust reference (host/aa-headunit/tls.rs): the head unit is the TLS
// CLIENT presenting the GAL head-unit certificate, peer verification disabled, and
// the TLS records ride INSIDE Android Auto `ENCAPSULATED_SSL` frames — never straight
// on the socket. In Swift this is Secure Transport (`SSLContext`) with custom I/O
// callbacks (`SSLSetIOFuncs`) that move ciphertext to/from in-memory buffers the AA
// session layer fills (from inbound frames) and drains (to outbound frames) — the
// exact analogue of the Rust memory BIOs.
//
// The GAL identity is loaded from a PKCS#12 (kept out of source, like the PEM certs).

import Foundation
import Security

enum AATLSStatus { case done, wantRead }

final class AATLS {
    private var ctx: SSLContext!
    private var inbound = Data()   // ciphertext fed from ENCAPSULATED_SSL / encrypted frames
    private var outbound = Data()  // ciphertext to emit as frames

    /// Set once SSLRead returns a status that is neither success nor WouldBlock. A TLS context that
    /// has reported a record-level failure never recovers — every subsequent SSLRead on it returns
    /// errSSLClosedAbort — so the session must END, not keep reading. Device-observed 2026-08-27:
    /// a single errSSLDecryptionFail (-9845) at 61 s was followed by 8890 consecutive -9806 while
    /// aa-bridge went on pumping 69 MB to a host that could no longer decrypt a byte of it.
    private var poisoned = false
    /// Total ciphertext bytes fed in. Logged when the context dies so the app-side total can be
    /// compared against aa-bridge's own `IN phone->host total=` — a divergence means bytes were lost
    /// in the CH_IP relay (a TLS stream cannot survive a gap), an equality means they were not.
    private var fedBytes = 0

    /// Load the GAL head-unit identity from a PKCS#12 and build a TLS-1.2 client context
    /// with custom (encapsulated-frame) I/O and peer verification disabled.
    init?(p12Path: String, password: String) {
        guard let p12 = try? Data(contentsOf: URL(fileURLWithPath: p12Path)) else {
            NSLog("[AATLS] cannot read p12 at \(p12Path)"); return nil
        }
        let opts = [kSecImportExportPassphrase as String: password] as CFDictionary
        var itemsCF: CFArray?
        let importStatus = SecPKCS12Import(p12 as CFData, opts, &itemsCF)
        guard importStatus == errSecSuccess,
              let items = itemsCF as? [[String: Any]],
              let first = items.first,
              let identityAny = first[kSecImportItemIdentity as String] else {
            NSLog("[AATLS] SecPKCS12Import failed (\(importStatus))"); return nil
        }
        let identity = identityAny as! SecIdentity

        guard let context = SSLCreateContext(kCFAllocatorDefault, .clientSide, .streamType) else {
            NSLog("[AATLS] SSLCreateContext failed"); return nil
        }
        ctx = context
        SSLSetIOFuncs(ctx, aaSSLRead, aaSSLWrite)
        SSLSetConnection(ctx, Unmanaged.passUnretained(self).toOpaque())
        SSLSetProtocolVersionMin(ctx, .tlsProtocol12)
        SSLSetProtocolVersionMax(ctx, .tlsProtocol12)
        // Present the GAL head-unit cert; skip verifying the phone (VERIFY_NONE equivalent).
        SSLSetCertificate(ctx, [identity] as CFArray)
        SSLSetSessionOption(ctx, .breakOnServerAuth, true)
    }

    // Called by the SSL I/O callbacks (below).
    fileprivate func ioRead(_ data: UnsafeMutableRawPointer, _ dataLength: UnsafeMutablePointer<Int>) -> OSStatus {
        let want = dataLength.pointee
        let avail = min(want, inbound.count)
        if avail > 0 {
            inbound.withUnsafeBytes { src in
                data.copyMemory(from: src.baseAddress!, byteCount: avail)
            }
            inbound.removeFirst(avail)
        }
        dataLength.pointee = avail
        return avail < want ? errSSLWouldBlock : noErr
    }

    fileprivate func ioWrite(_ data: UnsafeRawPointer, _ dataLength: UnsafeMutablePointer<Int>) -> OSStatus {
        let n = dataLength.pointee
        outbound.append(Data(bytes: data, count: n))
        dataLength.pointee = n
        return noErr
    }

    /// Feed one inbound ciphertext blob (an ENCAPSULATED_SSL frame body, or an
    /// encrypted message frame payload).
    func feedInbound(_ data: Data) { inbound.append(data); fedBytes += data.count }

    /// Drain queued outbound ciphertext (to wrap in frames). nil when empty.
    func takeOutbound() -> Data? {
        guard !outbound.isEmpty else { return nil }
        let d = outbound; outbound.removeAll(keepingCapacity: true); return d
    }

    /// One SSLHandshake step. Returns .done when complete, .wantRead when it needs
    /// more inbound ciphertext. Caller drains outbound after this and, on .wantRead,
    /// feeds one inbound frame then calls again.
    func handshakeStep() -> AATLSStatus? {
        while true {
            let st = SSLHandshake(ctx)
            switch st {
            case noErr:
                return .done
            case errSSLWouldBlock:
                return .wantRead
            case errSSLPeerAuthCompleted:
                continue // accept the phone's cert (no verification) and proceed
            default:
                NSLog("[AATLS] SSLHandshake error \(st)")
                return nil
            }
        }
    }

    /// Encrypt a full plaintext message ([messageId||protobuf]) → TLS record bytes for
    /// an ENCRYPTED frame payload.
    func encrypt(_ plaintext: Data) -> Data? {
        var processed = 0
        let st = plaintext.withUnsafeBytes { raw in
            SSLWrite(ctx, raw.baseAddress, plaintext.count, &processed)
        }
        guard st == noErr, processed == plaintext.count else { NSLog("[AATLS] SSLWrite \(st)"); return nil }
        var out = Data()
        while let c = takeOutbound() { out.append(c) }
        return out
    }

    /// Decrypt an ENCRYPTED frame payload → [messageId||protobuf]. Drains all records
    /// the fed ciphertext yields (a WouldBlock after ≥1 record just means "no more").
    func decrypt(_ ciphertext: Data) -> Data? {
        if poisoned { return nil }
        feedInbound(ciphertext)
        var out = Data()
        var buf = [UInt8](repeating: 0, count: 16 * 1024)
        while true {
            var processed = 0
            let st = SSLRead(ctx, &buf, buf.count, &processed)
            if processed > 0 { out.append(contentsOf: buf[0..<processed]) }
            if st == noErr { continue }
            if st == errSSLWouldBlock { break }
            // FATAL. Not "no plaintext this time": the context is dead and cannot be read again.
            // Returning `out` here (the old behaviour) let recvMsg treat it as a normal empty read,
            // so the session ran on with a poisoned context, spinning on -9806 for as long as the
            // box kept sending — a frozen window, a busy loop, and a bridge held by a host that had
            // already failed. `nil` ends the session, which closes the transport, which lets the box
            // observe EOF and re-announce.
            NSLog("[AATLS] SSLRead \(st) — FATAL, ending the session " +
                  "(fed \(fedBytes) B ciphertext, \(inbound.count) B unconsumed, " +
                  "\(out.count) B plaintext this call)")
            poisoned = true
            return nil
        }
        // Empty is NOT an error: a record that yields no application bytes (an alert, or the first
        // fragment of a record split across AA frames) is normal. Returning nil here made the caller
        // treat it as EOF and tear the session down. `nil` is reserved for a real failure.
        return out
    }

    /// "TLSv1.2 / <cipher>" for logging.
    func describe() -> String {
        var cipher: SSLCipherSuite = 0
        SSLGetNegotiatedCipher(ctx, &cipher)
        return String(format: "TLSv1.2 / cipher 0x%04x", cipher)
    }
}

// SSLSetIOFuncs callbacks — trampolines to the AATLS instance via the connection ref.
private func aaSSLRead(_ connection: SSLConnectionRef,
                       _ data: UnsafeMutableRawPointer,
                       _ dataLength: UnsafeMutablePointer<Int>) -> OSStatus {
    let tls = Unmanaged<AATLS>.fromOpaque(connection).takeUnretainedValue()
    return tls.ioRead(data, dataLength)
}

private func aaSSLWrite(_ connection: SSLConnectionRef,
                        _ data: UnsafeRawPointer,
                        _ dataLength: UnsafeMutablePointer<Int>) -> OSStatus {
    let tls = Unmanaged<AATLS>.fromOpaque(connection).takeUnretainedValue()
    return tls.ioWrite(data, dataLength)
}
