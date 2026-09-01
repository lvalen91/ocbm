import AppKit
import Foundation

/// Presents a snapshot of everything the app currently knows about the connected
/// adapter and phone: firmware, identifiers, link status, decrypted session-token
/// telemetry, and raw device-info blob (in hex + base64 + printable strings).
///
/// Rendered as an `NSAlert` sheet so it picks up the system "liquid glass" material
/// automatically on macOS 26+, matching the Keyboard Shortcuts dialog.
enum AdapterInfoPresenter {

    struct Snapshot {
        var state: String
        var phoneType: String
        var micDecodeType: UInt32
        var videoEncoderType: UInt32
        var firmware: String?
        var adapterBoxInfo: [String: Any]?
        var phoneBoxInfo: [String: Any]?
        var decryptedSessionToken: [String: Any]?
        var rawDeviceInfoBlob: Data?
    }

    /// Present as a sheet on the given window (nil = modal).
    @MainActor
    static func present(snapshot: Snapshot, on parent: NSWindow?) {
        let body = render(snapshot: snapshot)

        let alert = NSAlert()
        alert.messageText = "Adapter / Phone Info"
        alert.informativeText = ""
        alert.alertStyle = .informational
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Copy to Clipboard")

        // Scrollable monospaced accessory view — NSAlert inherits the system
        // material background, so the text view stays transparent.
        let contentSize = NSSize(width: 640, height: 460)
        let scroll = NSScrollView(frame: NSRect(origin: .zero, size: contentSize))
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = false
        scroll.borderType = .bezelBorder
        scroll.autohidesScrollers = false
        scroll.drawsBackground = false

        let textView = NSTextView(frame: NSRect(origin: .zero, size: contentSize))
        textView.isEditable = false
        textView.isSelectable = true
        textView.isRichText = false
        textView.drawsBackground = false
        textView.font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        textView.textContainerInset = NSSize(width: 8, height: 8)
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.textContainer?.widthTracksTextView = true
        textView.string = body

        scroll.documentView = textView
        alert.accessoryView = scroll

        let handler: (NSApplication.ModalResponse) -> Void = { response in
            if response == .alertSecondButtonReturn {
                let pb = NSPasteboard.general
                pb.clearContents()
                pb.setString(body, forType: .string)
            }
        }

        if let parent {
            alert.beginSheetModal(for: parent, completionHandler: handler)
        } else {
            handler(alert.runModal())
        }
    }

    // MARK: - Rendering

    private static func render(snapshot s: Snapshot) -> String {
        var lines: [String] = []

        func section(_ title: String) {
            lines.append("")
            lines.append("━━━ \(title) ━━━")
        }

        func field(_ key: String, _ value: Any?) {
            let v: String
            switch value {
            case nil:
                v = "—"
            case let s as String where s.isEmpty:
                v = "—"
            case let any?:
                v = String(describing: any)
            }
            let paddedKey = key.padding(toLength: 22, withPad: " ", startingAt: 0)
            lines.append("  \(paddedKey) \(v)")
        }

        func dumpDict(_ dict: [String: Any]?, indent: String = "  ") {
            guard let dict, !dict.isEmpty else {
                lines.append("\(indent)—")
                return
            }
            for key in dict.keys.sorted() {
                let value = dict[key]!
                if let nested = value as? [String: Any] {
                    lines.append("\(indent)\(key):")
                    dumpDict(nested, indent: indent + "  ")
                    continue
                }
                let valueStr: String
                if let array = value as? [Any] {
                    valueStr = "[\(array.map { String(describing: $0) }.joined(separator: ", "))]"
                } else {
                    valueStr = String(describing: value)
                }
                lines.append("\(indent)\(key): \(valueStr)")
            }
        }

        // No "Captured: <now>" stamp: the legacy info plane that fed this snapshot is gone, so most
        // fields are static placeholders and a fresh timestamp would falsely present them as live.
        lines.append("Static placeholder — see Settings ▸ CCPA for live adapter data")

        section("Connection")
        field("State", s.state)
        field("Phone type", s.phoneType)
        field("Firmware (SWVERSION)", s.firmware)
        field("Mic decode type", s.micDecodeType)
        field("Video encoder type", videoEncoderName(s.videoEncoderType))

        section("Adapter (from BoxSettings + SWVERSION)")
        if let ab = s.adapterBoxInfo {
            dumpDict(ab)
        } else {
            lines.append("  — not yet received")
        }

        section("Phone (from BoxSettings)")
        if let pb = s.phoneBoxInfo {
            dumpDict(pb)
        } else {
            lines.append("  — not yet received")
        }

        section("Session Token (type 0xA3) — Decrypted")
        if let tok = s.decryptedSessionToken {
            dumpDict(tok)
        } else if s.rawDeviceInfoBlob != nil {
            lines.append("  Decryption failed. Raw blob is shown below.")
        } else {
            lines.append("  — not yet received")
        }

        if let blob = s.rawDeviceInfoBlob {
            section("Device-Info Blob (raw as received)")
            lines.append("  Length: \(blob.count) bytes")
            let printable = blob.prefix(while: { $0 != 0 })
            if let text = String(data: printable, encoding: .ascii) {
                lines.append("  Printable ASCII prefix: \(text)")
                // Only attempt a base64 decode of a string that actually IS base64. Passing
                // .ignoreUnknownCharacters would "decode" arbitrary ASCII into garbage bytes and
                // report a bogus length, so decode strictly (no option) — non-base64 yields nil.
                if let decoded = Data(base64Encoded: text) {
                    lines.append("  (prefix is valid base64) decoded length: \(decoded.count) bytes")
                    lines.append("  Base64-decoded hex (first 128B):")
                    lines.append("    " + hexDump(decoded.prefix(128)))
                }
            }
            lines.append("  Raw hex (first 256B):")
            lines.append("    " + hexDump(blob.prefix(256)))
        }

        return lines.joined(separator: "\n")
    }

    private static func hexDump(_ data: Data) -> String {
        data.map { String(format: "%02x", $0) }.joined(separator: " ")
    }

    private static func videoEncoderName(_ type: UInt32) -> String {
        switch type {
        case 1: return "H.264"
        case 2: return "H.265 (default)"
        case 4: return "MJPEG"
        default: return "unknown(\(type))"
        }
    }
}
