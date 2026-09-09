import AppKit
import AVFoundation

/// Manages the main application window with aspect ratio locked to the selected resolution.
///
/// The window is borderless (no title bar) and PURE edge-to-edge video — nothing is ever drawn over it,
/// so the entire surface is an unobstructed CarPlay touch area. Window controls (traffic lights) and the
/// inline sun/moon / cluster controls live in a separate floating HUD (`ControlHUDController`) that never
/// overlaps the video. Moving the window: ⌘-drag (a plain drag stays a CarPlay touch); see CarPlayView.
@MainActor final class MainWindowController: NSWindowController {
    let carPlayView = CarPlayView(frame: .zero)
    private var controlBox: ControlBox?

    convenience init() {
        // Seed from the VehicleConfigModel's persisted main resolution — the single source of truth
        // for what the adapter (and therefore the video geometry / touch mapping) will use.
        let res = VehicleConfigModel.persistedMainResolution()
        let aspect = CGFloat(res.width) / CGFloat(res.height)

        // Start at a reasonable size that fits most screens
        // Use 1200px on the longer edge
        let initialWidth: CGFloat
        let initialHeight: CGFloat
        if res.width >= res.height {
            initialWidth = 1200
            initialHeight = 1200 / aspect
        } else {
            initialHeight = 900
            initialWidth = 900 * aspect
        }

        // Borderless, but still key/main-eligible (BorderlessKeyWindow) so the traffic lights color
        // correctly and ⌘W/⌘M + the Window menu keep a target. Keep .closable/.miniaturizable — their
        // perform actions no-op (beep) without the mask bits even though there's no visible title bar.
        let window = BorderlessKeyWindow(
            contentRect: NSRect(x: 0, y: 0, width: initialWidth, height: initialHeight),
            styleMask: [.borderless, .closable, .resizable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        self.init(window: window)

        window.title = "CarLink"
        window.contentView = carPlayView          // pure edge-to-edge video, no overlay
        window.center()

        window.contentAspectRatio = NSSize(width: res.width, height: res.height)
        // Non-opaque so CarPlayView's rounded-corner clip shows the desktop through the corners (Phase 3
        // floating card). hasShadow gives the card its drop shadow.
        window.backgroundColor = .clear
        window.isOpaque = false
        window.hasShadow = true
        window.collectionBehavior = [.fullScreenNone]   // no fullscreen — video windows never zoom full
        applyMinSize(width: CGFloat(res.width), height: CGFloat(res.height))

        // The floating control box — a child window snapped above the video that moves with it. Red =
        // quit (matches the old main-window-close behavior), yellow = minimize the video window.
        controlBox = ControlBox(kind: .main, parent: window,
                                close: { NSApp.terminate(nil) },
                                minimize: { [weak window] in window?.miniaturize(nil) })
    }

    /// Window ▸ Control Box — show/hide the main video window's control box.
    func toggleControlBox() { controlBox?.toggle() }

    /// Updates window aspect ratio and resizes to match a new resolution.
    func applyResolution(width: CGFloat, height: CGFloat) {
        guard let window else { return }
        let aspect = width / height

        window.contentAspectRatio = NSSize(width: Int(width), height: Int(height))
        applyMinSize(width: width, height: height)
        carPlayView.updateVideoAspect(width: width, height: height)

        // Resize the CONTENT area (borderless ⇒ content rect == frame, so this is the whole window).
        let contentRect = window.contentRect(forFrameRect: window.frame)
        let newContentWidth: CGFloat
        let newContentHeight: CGFloat
        if width >= height {
            // Landscape — keep current width, adjust height
            newContentWidth = contentRect.width
            newContentHeight = contentRect.width / aspect
        } else {
            // Portrait — keep current height, adjust width
            newContentHeight = contentRect.height
            newContentWidth = contentRect.height * aspect
        }

        // Respect the minimum content size — setFrame bypasses contentMinSize
        // (which only constrains user resizes), so an orientation flip could
        // otherwise produce a window smaller than its own minimum.
        let minSize = window.contentMinSize
        let scale = max(minSize.width / newContentWidth, minSize.height / newContentHeight, 1)
        let finalWidth = newContentWidth * scale
        let finalHeight = newContentHeight * scale

        // Center the new frame around the old center, then keep it on screen.
        let oldFrame = window.frame
        var newFrame = window.frameRect(forContentRect: NSRect(
            x: 0, y: 0, width: finalWidth, height: finalHeight
        ))
        newFrame.origin.x = oldFrame.midX - newFrame.width / 2
        newFrame.origin.y = oldFrame.midY - newFrame.height / 2
        if let screen = window.screen {
            newFrame = window.constrainFrameRect(newFrame, to: screen)
        }
        window.setFrame(newFrame, display: true, animate: true)
    }

    private func applyMinSize(width: CGFloat, height: CGFloat) {
        // Minimum ~400px on the shorter edge, in content coordinates
        // (minSize would include the title bar and skew the aspect).
        let aspect = width / height
        if width >= height {
            window?.contentMinSize = NSSize(width: 400 * aspect, height: 400)
        } else {
            window?.contentMinSize = NSSize(width: 400, height: 400 / aspect)
        }
    }
}
