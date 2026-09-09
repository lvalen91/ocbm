// ControlPopups.swift — the Control Box's pop-out control surfaces. Each button on the Main video
// Control Box (Phone / Media / Knob) opens its control cluster in its own independent, translucent
// floating window (the existing SwiftUI control views from ControlsWindow.swift, hosted over a Liquid
// Glass NSVisualEffectView). Siri and Settings are handled directly by the Control Box (an action and
// the existing Settings window) and are not ControlPopups.

import AppKit
import SwiftUI

/// A control surface that pops out of the Control Box into its own translucent window.
enum ControlPopupKind: String {
    case telephony = "Phone"
    case media = "Media"
    case knob = "Knob"

    var symbol: String {
        switch self {
        case .telephony: return "phone"
        case .media: return "play.circle"
        case .knob: return "dial.medium"
        }
    }
    var help: String { "\(rawValue) controls" }
}

@MainActor enum ControlPopups {
    /// One reusable window per kind (re-shown, not duplicated).
    private static var windows: [String: NSPanel] = [:]

    /// Show the popup if hidden, hide it if already visible.
    static func toggle(_ kind: ControlPopupKind) {
        if let w = windows[kind.rawValue], w.isVisible {
            w.orderOut(nil)
        } else {
            show(kind)
        }
    }

    static func show(_ kind: ControlPopupKind) {
        if let w = windows[kind.rawValue] {
            w.makeKeyAndOrderFront(nil)
            return
        }
        let bridge = ControlsBridge.shared
        let root: AnyView
        switch kind {
        case .telephony: root = AnyView(TelephonyControlView(bridge: bridge))
        case .media: root = AnyView(MediaControlView(bridge: bridge))
        case .knob: root = AnyView(KnobControlView(bridge: bridge))
        }

        let hosting = NSHostingView(rootView: root)
        hosting.translatesAutoresizingMaskIntoConstraints = false

        // Translucent Liquid Glass backdrop (matches the Control Box / keyboard-shortcuts popup feel).
        let effect = NSVisualEffectView()
        effect.material = .hudWindow
        effect.blendingMode = .behindWindow
        effect.state = .active
        effect.addSubview(hosting)
        NSLayoutConstraint.activate([
            hosting.leadingAnchor.constraint(equalTo: effect.leadingAnchor),
            hosting.trailingAnchor.constraint(equalTo: effect.trailingAnchor),
            hosting.topAnchor.constraint(equalTo: effect.topAnchor),
            hosting.bottomAnchor.constraint(equalTo: effect.bottomAnchor),
        ])

        let size = hosting.fittingSize
        let win = NSPanel(contentRect: NSRect(origin: .zero, size: size),
                          styleMask: [.titled, .closable, .resizable, .fullSizeContentView, .utilityWindow],
                          backing: .buffered, defer: false)
        win.title = kind.rawValue
        win.titlebarAppearsTransparent = true         // glass runs edge-to-edge under the titlebar
        win.titleVisibility = .hidden
        // NOT movable-by-background: it would swallow content drags (e.g. the rotary Knob's rotate
        // gesture) as window moves. The window still moves by its transparent titlebar strip (top edge).
        win.isMovableByWindowBackground = false
        win.isReleasedWhenClosed = false
        win.isFloatingPanel = true
        win.hidesOnDeactivate = false
        win.level = .floating
        win.contentView = effect
        win.center()
        windows[kind.rawValue] = win
        win.makeKeyAndOrderFront(nil)
    }
}
