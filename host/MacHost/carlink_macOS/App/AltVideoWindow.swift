// AltVideoWindow.swift — a dedicated window that renders the ALT / navigation (instrument-cluster)
// video stream via its own VideoDecoder's AVSampleBufferDisplayLayer (Window ▸ Alt Video).
//
// The main CarPlay screen renders in the main window; this second window hosts the DEDICATED alt
// decoder (a full duplicate of the video decode pipeline). It stays black until the box forwards a
// CH_ALT_VIDEO stream (the cluster/nav screen — see docs/carplay/06_AV_PIPELINE.md), then shows it live.

import AppKit
import AVFoundation

/// The NSView that hosts the alt decoder's display layer, keeping it sized to the window.
private final class AltVideoView: NSView {
    private var videoLayer: CALayer?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layer?.backgroundColor = NSColor.black.cgColor
    }
    required init?(coder: NSCoder) { fatalError() }

    func setVideoLayer(_ new: CALayer) {
        videoLayer?.removeFromSuperlayer()
        new.frame = bounds
        if let display = new as? AVSampleBufferDisplayLayer { display.videoGravity = .resizeAspect }
        layer?.addSublayer(new)
        videoLayer = new
    }

    // NOTE (05-M2/12-L1 fix): iOS never delegates cornerMasks to the alt/cluster display — box-side
    // info.rs gates `cornerMasks` to `is_main` only, and the mask the main lane streams is scaled for
    // the MAIN display's width. Applying it here (as this view used to) carved a corner sized for a
    // different stream. No mask is applied to the alt window; it stays a plain black-backed layer.
    override func layout() {
        super.layout()
        videoLayer?.frame = bounds
    }
}

/// A floating window that opens on the FIRST arriving alt/nav (cluster) video frame and then STAYS
/// OPEN for the rest of the session. It never opens if no alt video is ever received.
///
/// It used to auto-hide after a ~3 s idle gap (user directive 2026-07-12), on the assumption that a
/// gap meant the stream had stopped. **That was wrong about how iOS drives the cluster** (owner
/// report 2026-08-11): iOS sends cluster frames occasionally rather than continuously, so the idle
/// rule made the window flap — hide, reappear on the next stray frame, hide again — for the entire
/// drive. A window that comes and goes on its own is worse than one that lingers, so the idle timer
/// is gone.
///
/// Closing is now driven only by INTENT, never by inactivity:
///   * `sessionEnded()` — adapter teardown (`AppDelegate`), the chrome's red button, and cluster
///     content being set to `.none` (`ControlsWindow`, which is also what turns the `navVideoOn`
///     gate off).
/// Per the red button's long-standing contract it reappears when cluster frames resume.
@MainActor
final class AltVideoWindowController: NSWindowController {
    static let shared = AltVideoWindowController()
    private let contentViewImpl = AltVideoView(frame: NSRect(x: 0, y: 0, width: 800, height: 480))

    // The alt window's own attached control box — its own Day/Night, zoom and cluster controls. It is a
    // child window of the alt window, so it shows/hides and moves right along with it.
    private var controlBox: ControlBox?

    private convenience init() {
        let win = BorderlessKeyWindow(
            contentRect: NSRect(x: 0, y: 0, width: 800, height: 480),
            styleMask: [.borderless, .closable, .resizable, .miniaturizable],
            backing: .buffered, defer: false)
        win.title = "Nav / Alt Video"
        win.isReleasedWhenClosed = false
        win.level = .normal     // ordinary window level — not always-on-top over other apps
        win.backgroundColor = .clear    // non-opaque → rounded corners show the desktop (Phase 3 card)
        win.isOpaque = false
        win.hasShadow = true
        win.collectionBehavior = [.fullScreenNone]
        // No touch surface here, so a plain background drag moves the window (no ⌘ needed).
        win.isMovableByWindowBackground = true
        self.init(window: win)
        win.contentView = contentViewImpl        // pure edge-to-edge cluster video

        // Red = hide the Nav window (it reappears when cluster frames resume), yellow = minimize.
        controlBox = ControlBox(kind: .alt, parent: win,
                                close: { [weak self] in self?.sessionEnded() },
                                minimize: { [weak win] in win?.miniaturize(nil) })
    }

    /// Attach the alt decoder's display layer (called per session when the alt decoder is created).
    func attach(layer: CALayer) {
        contentViewImpl.setVideoLayer(layer)
        // Re-push the persisted cluster-appearance flags now that a cluster surface exists.
        ControlsBridge.shared.syncNavAppearance()
    }

    /// Lock the window to the alt stream's ACTUAL coded aspect ratio and size the content to match —
    /// mirroring the main window (`MainWindowController.applyResolution`). With the content aspect
    /// equal to the video's, `.resizeAspect` fills exactly: no letterbox / black voids, and macOS
    /// keeps the ratio during user resizes (the window just grows/shrinks in the video's shape).
    /// iOS chooses the cluster resolution itself, so this is driven by the decoder's real dimensions,
    /// not the requested config. Called on the main queue from the alt decoder's `onDimensions`.
    func applyVideoSize(width: Int, height: Int) {
        guard let window, width > 0, height > 0 else { return }
        let w = CGFloat(width), h = CGFloat(height)
        let aspect = w / h

        window.contentAspectRatio = NSSize(width: width, height: height)
        // Minimum ~300px on the shorter edge, aspect-correct (content coords, excludes the title bar).
        if w >= h {
            window.contentMinSize = NSSize(width: 300 * aspect, height: 300)
        } else {
            window.contentMinSize = NSSize(width: 300, height: 300 / aspect)
        }

        // Reshape the content to the video aspect only if it's currently off — avoids churn when the
        // same dimensions are re-reported. Preserve the current long edge (or a sane ~800 default) so
        // we honor any size the user already chose, just corrected to the right shape.
        let content = window.contentRect(forFrameRect: window.frame)
        if abs(content.width / max(content.height, 1) - aspect) > 0.01 {
            let longEdge = max(content.width, content.height, 800)
            let newW: CGFloat, newH: CGFloat
            if w >= h { newW = longEdge; newH = (longEdge / aspect).rounded() }
            else { newH = longEdge; newW = (longEdge * aspect).rounded() }

            var frame = window.frameRect(forContentRect: NSRect(x: 0, y: 0, width: newW, height: newH))
            let old = window.frame
            frame.origin.x = old.midX - frame.width / 2
            frame.origin.y = old.midY - frame.height / 2
            if let screen = window.screen { frame = window.constrainFrameRect(frame, to: screen) }
            window.setFrame(frame, display: true, animate: window.isVisible)
        }
    }

    /// Called on the main actor for every ARRIVING alt-video frame (note: arrival, not successful
    /// decode — see `OCBMAVBridge.avDidReceiveAltVideoFrame`). The first frame opens the window; every
    /// frame after that is a no-op, because the window now stays open until something explicitly
    /// closes it.
    func noteActivity() {
        guard let win = window, !win.isVisible else { return }
        win.orderFront(nil)   // orderFront (not key) so the main window keeps touch focus
    }

    /// Explicit close: adapter teardown, the chrome's red button, or cluster content set to `.none`.
    /// This is the ONLY thing that hides the window — inactivity no longer does.
    func sessionEnded() {
        window?.orderOut(nil)
    }

    /// Manual open (Window ▸ Alt Video) — for testing without a live stream.
    func show() {
        window?.center()
        window?.makeKeyAndOrderFront(nil)
    }
}
