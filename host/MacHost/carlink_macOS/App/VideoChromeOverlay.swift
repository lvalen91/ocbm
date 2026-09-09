// VideoChromeOverlay.swift — the attached, floating "control box" for the borderless video windows.
//
// The two video windows (Main CarPlay + Nav/Alt) are PURE borderless, edge-to-edge video surfaces with
// NOTHING drawn over them — the whole video is an unobstructed CarPlay touch surface. The traffic lights
// and the inline controls live in a separate translucent HUD box that is a CHILD WINDOW of its video
// window: it snaps just above the video, moves with it, and never overlaps it. Each video window gets
// its own box (the alt window's box carries its own Day/Night + zoom + cluster controls).
//
// Moving a video window: drag the box (it moves the video window with it), or ⌘-drag the video directly
// (a plain drag stays a CarPlay touch, so the surface is never sacrificed for window movement).

import AppKit
import SwiftUI

// MARK: - Borderless key window

/// A borderless window that can still become key/main — required so the video window keeps keyboard
/// focus (CarPlay key input) and so ⌘W/⌘M + the Window menu keep a target even with no title bar.
final class BorderlessKeyWindow: NSWindow {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
}

// MARK: - HUD panel + drag-moves-parent backdrop

/// A borderless, non-activating floating panel. Non-activating so clicking its buttons never steals key
/// focus from the video window (CarPlay keyboard input keeps working).
private final class HUDPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

/// The translucent backdrop of the box. Dragging empty areas of it moves the box's *video window* (the
/// box is a child window, so it follows the parent automatically) — using absolute screen-space deltas so
/// the moving child doesn't feed back into the drag. Buttons are subviews and consume their own clicks,
/// so a drag only starts on empty glass.
private final class BoxBackdrop: NSVisualEffectView {
    weak var dragTarget: NSWindow?
    private var originAtDown: NSPoint = .zero
    private var mouseAtDown: NSPoint = .zero
    private var dragging = false

    private func anchor(_ t: NSWindow) {
        originAtDown = t.frame.origin
        mouseAtDown = NSEvent.mouseLocation
        dragging = true
    }

    override func mouseDown(with event: NSEvent) {
        guard let t = dragTarget else { return }
        anchor(t)
    }
    override func mouseDragged(with event: NSEvent) {
        guard let t = dragTarget else { return }
        // A drag that STARTED over a button sends mouseDown to the SwiftUI hosting view, not us — when
        // SwiftUI then abandons it and the drag falls through here, `dragging` is false and the anchor is
        // stale from the previous drag, which makes the window jump. Re-anchor at the first such event so
        // the delta starts from zero (smooth), matching an empty-glass drag.
        if !dragging { anchor(t) }
        let now = NSEvent.mouseLocation
        t.setFrameOrigin(NSPoint(x: originAtDown.x + (now.x - mouseAtDown.x),
                                 y: originAtDown.y + (now.y - mouseAtDown.y)))
    }
    override func mouseUp(with event: NSEvent) {
        dragging = false
        super.mouseUp(with: event)
    }
}

/// Which video window a control box belongs to — selects which controls it carries and what its traffic
/// lights act on.
enum ControlBoxKind { case main, alt }

/// A floating control box attached to one video window as a child window.
@MainActor final class ControlBox {
    private let panel: HUDPanel
    private weak var parent: NSWindow?
    private let gap: CGFloat = 8
    // nonisolated so the (nonisolated) deinit can remove them; NotificationCenter removal is thread-safe.
    private nonisolated(unsafe) var resizeObs: NSObjectProtocol?
    private nonisolated(unsafe) var moveObs: NSObjectProtocol?

    /// - close/minimize: what the box's traffic lights do (main: quit / minimize main; alt: hide / minimize alt).
    init(kind: ControlBoxKind, parent: NSWindow,
         close: @escaping () -> Void, minimize: @escaping () -> Void) {
        self.parent = parent

        let backdrop = BoxBackdrop()
        backdrop.material = .hudWindow
        backdrop.blendingMode = .behindWindow
        backdrop.state = .active
        backdrop.wantsLayer = true
        backdrop.layer?.cornerRadius = 12
        backdrop.layer?.cornerCurve = .continuous
        backdrop.layer?.masksToBounds = true
        backdrop.dragTarget = parent

        let hosting = NSHostingView(rootView: ControlBoxView(kind: kind, close: close, minimize: minimize))
        hosting.translatesAutoresizingMaskIntoConstraints = false
        backdrop.addSubview(hosting)
        NSLayoutConstraint.activate([
            hosting.leadingAnchor.constraint(equalTo: backdrop.leadingAnchor),
            hosting.trailingAnchor.constraint(equalTo: backdrop.trailingAnchor),
            hosting.topAnchor.constraint(equalTo: backdrop.topAnchor),
            hosting.bottomAnchor.constraint(equalTo: backdrop.bottomAnchor),
        ])

        let size = hosting.fittingSize
        let p = HUDPanel(contentRect: NSRect(origin: .zero, size: size),
                         styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        p.isFloatingPanel = true
        p.becomesKeyOnlyIfNeeded = true
        p.hidesOnDeactivate = false
        p.isMovableByWindowBackground = false     // drag is handled manually to move the PARENT window
        p.isOpaque = false
        p.backgroundColor = .clear
        p.hasShadow = true
        p.contentView = backdrop
        panel = p

        parent.addChildWindow(panel, ordered: .above)
        reposition()
        // Re-snap the box above the video on BOTH resize and move. macOS's child-window auto-follow +
        // our own setFrameOrigin can desync (the child drifts over the video after a move or a
        // window-reorder from a button click); re-snapping on every parent frame change keeps it pinned.
        let resnap: (Notification) -> Void = { [weak self] _ in
            MainActor.assumeIsolated { self?.reposition() }
        }
        resizeObs = NotificationCenter.default.addObserver(
            forName: NSWindow.didResizeNotification, object: parent, queue: .main, using: resnap)
        moveObs = NotificationCenter.default.addObserver(
            forName: NSWindow.didMoveNotification, object: parent, queue: .main, using: resnap)
    }

    deinit {
        if let resizeObs { NotificationCenter.default.removeObserver(resizeObs) }
        if let moveObs { NotificationCenter.default.removeObserver(moveObs) }
    }

    /// Snap the box just above the video window's top-left corner.
    func reposition() {
        guard let parent else { return }
        let f = parent.frame
        panel.setFrameOrigin(NSPoint(x: f.minX, y: f.maxY + gap))
    }

    var isVisible: Bool { panel.isVisible }

    func toggle() {
        if panel.isVisible {
            parent?.removeChildWindow(panel)
            panel.orderOut(nil)
        } else if let parent {
            parent.addChildWindow(panel, ordered: .above)
            reposition()
        }
    }
}

// MARK: - The SwiftUI control box

/// The box content: mac traffic lights (close / minimize) plus the inline controls for this window. The
/// main box carries the main display's sun/moon; the alt box carries the alt display's sun/moon plus the
/// cluster zoom and content/appearance menu. All bound to `ControlsBridge.shared` so they stay in sync
/// with the Controls/Settings windows and a reconnect re-sync, with no hand-wired observers.
struct ControlBoxView: View {
    let kind: ControlBoxKind
    let close: () -> Void
    let minimize: () -> Void
    @ObservedObject private var bridge = ControlsBridge.shared
    @State private var lightsHover = false

    init(kind: ControlBoxKind, close: @escaping () -> Void, minimize: @escaping () -> Void) {
        self.kind = kind
        self.close = close
        self.minimize = minimize
    }

    var body: some View {
        HStack(spacing: 12) {
            trafficLights
            grabber
            switch kind {
            case .main:
                appearanceButton(alt: false, tip: "Main display Light / Dark")
                Divider().frame(height: 20)
                // Pop-out control surfaces + Siri/Settings. Each opens an independent translucent window
                // (ControlPopups); Siri fires the invoke directly; Settings opens the config window.
                iconButton("phone", "Phone controls") { ControlPopups.toggle(.telephony) }
                iconButton("play.circle", "Media controls") { ControlPopups.toggle(.media) }
                iconButton("dial.medium", "Knob controls") { ControlPopups.toggle(.knob) }
                iconButton("rectangle.on.rectangle", "Nav / Alt Video window") {
                    AltVideoWindowController.shared.show()
                }
                iconButton("mic.fill", "Invoke Siri") { bridge.siriPress() }
                keyframeButton
                limitedUIButton
                iconButton("gearshape", "Settings") { SettingsWindowController.shared.show() }
            case .alt:
                appearanceButton(alt: true, tip: "Nav display Light / Dark")
                zoomControl
                clusterMenu
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
        .buttonStyle(.borderless)
        .tint(.primary)
        .fixedSize()
    }

    // Drag-affordance grabber: a 2×3 dot grid that signals "grab here to move." Non-interactive
    // (allowsHitTesting(false)) so clicks/drags fall through to the draggable glass backdrop — clicking
    // does nothing on its own, dragging moves the window. Also visually separates the window controls
    // from the app controls (replaces the leading divider).
    private var grabber: some View {
        Grid(horizontalSpacing: 2.5, verticalSpacing: 2.5) {
            ForEach(0..<3, id: \.self) { _ in
                GridRow {
                    Circle().frame(width: 2.5, height: 2.5)
                    Circle().frame(width: 2.5, height: 2.5)
                }
            }
        }
        .foregroundStyle(.tertiary)
        .frame(height: 20)
        .padding(.horizontal, 3)
        .allowsHitTesting(false)
        .help("Drag to move")
    }

    // Classic mac traffic lights (close = red, minimize = yellow). Glyphs appear on group hover. Green
    // (zoom/fullscreen) is intentionally omitted — the video windows never go fullscreen.
    private var trafficLights: some View {
        HStack(spacing: 8) {
            light(.red, glyph: "xmark", action: close,
                  help: kind == .main ? "Close (quit CarLink)" : "Hide Nav window")
            light(.yellow, glyph: "minus", action: minimize, help: "Minimize window")
        }
        .onHover { lightsHover = $0 }
    }

    private func light(_ color: Color, glyph: String, action: @escaping () -> Void, help: String) -> some View {
        Button(action: action) {
            ZStack {
                Circle().fill(color)
                if lightsHover {
                    Image(systemName: glyph)
                        .font(.system(size: 7, weight: .bold))
                        .foregroundStyle(.black.opacity(0.55))
                }
            }
            .frame(width: 13, height: 13)
        }
        .buttonStyle(.plain)
        .help(help)
    }

    // A plain icon button for the Control Box's pop-out surfaces (Phone / Media / Knob / Siri / Settings).
    private func iconButton(_ symbol: String, _ tip: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 15, weight: .medium))
                .frame(width: 22, height: 22)
        }
        .help(tip)
    }

    // Sun/moon: flip a whole display (UI + Map) between Light and Dark. Icon tracks the @Published state.
    /// Manual keyframe request — the recovery action for "the picture is frozen or smeared".
    ///
    /// The plumbing already existed and had no manual route: `onVideoGap` / `onFrameDropped` call
    /// `OCBMClient.requestKeyframe()` AUTOMATICALLY on detected loss, which sends
    /// `ocbm_proto::INPUT_KEYFRAME`; airplayd's `handle_input_frame` turns that into
    /// `events::send_force_key_frame()` on the event channel (only the box owns that channel). So this
    /// button is the same path a detected gap takes — nothing new on the wire — for the case the
    /// detectors miss, where the stream is arriving and decoding but the PICTURE is wrong.
    ///
    /// Deliberately not disabled when idle: `send_force_key_frame` is a no-op between sessions, and a
    /// greyed button would just raise the question of why.
    private var keyframeButton: some View {
        Button {
            bridge.requestKeyframe()
        } label: {
            Image(systemName: "arrow.clockwise.circle")
                .font(.system(size: 15, weight: .medium))
                .frame(width: 22, height: 22)
        }
        .help("Request a keyframe — recovers a frozen or corrupted picture (ForceKeyFrame).")
    }

    /// Limited UI (Drive) — `/command setLimitedUI`, which restricts CarPlay as if the vehicle were in
    /// Drive: no on-screen keyboard, no phone keypad, no long scrollable lists.
    ///
    /// LIVES HERE BECAUSE IT HAD NOWHERE ELSE. The only control for it was the Controls window's "UI"
    /// tab, deleted in fcffdcd (2026-08-02, "Controls redesign") on the grounds that Limited UI was
    /// "a niche runtime toggle". That took the sole route to `setLimitedUI` with it, leaving the whole
    /// backend — `limitedUIOn`, the sender, and the box's `limitedUIElements` in /info — intact and
    /// unreachable, which is why it "worked previously" and then appeared to vanish.
    ///
    /// `.main`, not `.alt`: this restricts the WHOLE CarPlay UI, not the cluster surface.
    ///
    /// The glyph is a literal D for Drive, filled and tinted while active, so the restricted state is
    /// readable at a glance rather than only from a tooltip — an invisible restriction is exactly how
    /// this one got lost.
    private var limitedUIButton: some View {
        Button {
            bridge.setLimitedUI(!bridge.limitedUIOn)
        } label: {
            Image(systemName: bridge.limitedUIOn ? "d.circle.fill" : "d.circle")
                .font(.system(size: 15, weight: .medium))
                .frame(width: 22, height: 22)
                .foregroundStyle(bridge.limitedUIOn ? Color.orange : Color.primary)
        }
        .help(bridge.limitedUIOn
              ? "Limited UI ON (Drive) — keyboard, phone keypad and long lists are restricted. Click to release."
              : "Limited UI (Drive) — restrict CarPlay as if the vehicle is in Drive.")
    }

    private func appearanceButton(alt: Bool, tip: String) -> some View {
        Button {
            bridge.toggleDisplayDark(alt: alt)
        } label: {
            Image(systemName: bridge.displayIsDark(alt: alt) ? "moon.fill" : "sun.max")
                .font(.system(size: 15, weight: .medium))
                .frame(width: 22, height: 22)
        }
        .help(tip)
    }

    // Cluster map zoom (Simulator Alt1 −/+): cluster-only, no-ops phone-side without a map surface.
    private var zoomControl: some View {
        HStack(spacing: 2) {
            Button { bridge.client?.sendCommand(OCBM.cmdNavZoomOut) } label: {
                Image(systemName: "minus").frame(width: 20, height: 22)
            }
            Button { bridge.client?.sendCommand(OCBM.cmdNavZoomIn) } label: {
                Image(systemName: "plus").frame(width: 20, height: 22)
            }
        }
        .help("Cluster map zoom (− / +)")
    }

    // Cluster content (radio) + Speed Limit / Compass / ETA toggles — the Simulator's Alt1 Content +
    // Appearance popover; content drives the SAME ControlsBridge state as the Controls window.
    private var clusterMenu: some View {
        Menu {
            Picker("Cluster Content", selection: Binding(
                get: { bridge.clusterContent },
                set: { bridge.setClusterContent($0) })) {
                ForEach(ClusterContent.allCases) { c in
                    Label(c.rawValue, systemImage: c.systemImage).tag(c)
                }
            }
            Divider()
            let queryable = bridge.clusterContent == .map || bridge.clusterContent == .navigationApp
            Toggle("Speed Limit", isOn: navFlag(OCBM.navApSpeedLimit)).disabled(!queryable)
            Toggle("Compass", isOn: navFlag(OCBM.navApCompass)).disabled(!queryable)
            Toggle("ETA", isOn: navFlag(OCBM.navApETA)).disabled(!queryable)
        } label: {
            Image(systemName: "slider.horizontal.3")
                .font(.system(size: 15, weight: .medium))
                .frame(width: 22, height: 22)
        }
        .menuIndicator(.hidden)
        .frame(width: 28)
        .help("Nav cluster: content + Speed Limit / Compass / ETA")
    }

    private func navFlag(_ bit: UInt8) -> Binding<Bool> {
        Binding(get: { bridge.navAppearanceFlags & bit != 0 },
                set: { _ in bridge.toggleNavAppearance(bit) })
    }
}
