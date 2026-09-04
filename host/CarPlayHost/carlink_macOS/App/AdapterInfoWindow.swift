import AppKit
import Foundation

/// Presents a snapshot of everything the app currently knows about the connected adapter and
/// phone, sourced live from the box's own `box_info_json` (CH_MGMT GET_INFO), the same data
/// `CCPABridge.shared.info` (`CCPAInfo`) feeds to Settings ▸ CCPA — not the retired riddlebox
/// BoxSettings/SWVERSION/session-token/device-info-blob planes, which the box no longer sends.
///
/// Rendered as an `NSAlert` sheet so it picks up the system "liquid glass" material
/// automatically on macOS 26+, matching the Keyboard Shortcuts dialog.
enum AdapterInfoPresenter {

    struct Snapshot {
        var state: String
        var info: CCPAInfo?
        var lastUpdated: Date?
        var boxHealth: BoxHealth?
        var btPhase: BtPhase?
        var phoneIdent: PhoneIdent?
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

        section("Connection")
        field("State", s.state)
        if let updated = s.lastUpdated {
            field("Snapshot captured", updated.formatted(date: .abbreviated, time: .standard))
        } else {
            field("Snapshot captured", nil)
        }

        // Live state (CT_BOX_HEALTH/CT_BT_PHASE/CT_PHONE_IDENT) — box-pushed on CH_CTRL, independent of
        // the CH_MGMT GET_INFO snapshot below, so this renders even before/without a GET_INFO reply.
        section("Live state")
        if let health = s.boxHealth {
            for (label, ok) in health.checklist {
                lines.append("  \(ok ? "✓" : "✗") \(label)")
            }
        } else {
            field("Box health", nil)
        }
        field("BT phase", s.btPhase?.displayName)
        if let ident = s.phoneIdent {
            field("Phone", "\(ident.model) (\(ident.osName) \(ident.osVersion))")
            field("Phone name", ident.name)
        } else {
            field("Phone", nil)
        }

        guard let info = s.info else {
            lines.append("")
            lines.append("No adapter snapshot yet — connect the adapter or use Settings ▸ CCPA ▸ Refresh.")
            return lines.joined(separator: "\n")
        }

        section("Adapter")
        field("Name", info.name)
        field("Serial", info.serial)
        field("BT MAC", info.bt_mac)
        field("Wi-Fi MAC", info.wifi_mac)
        field("Transport", info.transport)
        field("Uptime", "\(info.uptime_s) s")
        field("Rootfs used", "\(info.rootfs_pct)% (\(info.rootfs_free_kb) KB free)")
        field("Bluetooth SSP", info.ssp ? "yes" : "no")
        field("HCI up", info.hci_up ? "yes" : "no")
        field("Wi-Fi AP up", info.wlan_ap ? "yes" : "no")

        section("Phone")
        field("Present", info.phone_present ? "yes" : "no")
        field("Host present", info.host_present ? "yes" : "no")

        section("Daemons")
        field("ocbmd", info.daemons.ocbmd ? "up" : "down")
        field("iap2d", info.daemons.iap2d ? "up" : "down")
        field("airplayd", info.daemons.airplayd ? "up" : "down")
        field("carplay_wireless", info.daemons.carplay_wireless ? "up" : "down")

        section("Devices")
        if info.devices.isEmpty {
            lines.append("  —")
        } else {
            for d in info.devices { lines.append("  \(d)") }
        }

        return lines.joined(separator: "\n")
    }
}
