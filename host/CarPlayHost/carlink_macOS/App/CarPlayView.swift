import AppKit
import AVFoundation

// MARK: - Touch Delegate

@MainActor protocol CarPlayViewDelegate: AnyObject {
    /// CarPlay: single-finger touch (tap, drag, swipe) — float 0..1 coordinates
    func carPlayView(_ view: CarPlayView, didMultiTouch action: MultiTouchAction, x: Float32, y: Float32)
    /// CarPlay: two-finger gesture (pinch, two-finger scroll) — sends two simultaneous touch points
    func carPlayView(_ view: CarPlayView, didMultiTouchTwo points: [(action: MultiTouchAction, x: Float32, y: Float32, id: UInt32)])
    /// Android Auto: single-touch — int 0..10000 coordinates
    func carPlayView(_ view: CarPlayView, didTouch action: TouchAction, x: UInt32, y: UInt32)
    /// Keyboard/media key → adapter command
    func carPlayView(_ view: CarPlayView, didPressCommand command: CommandID)
}

// MARK: - CarPlay View

/// NSView that hosts the AVSampleBufferDisplayLayer for video display
/// and converts mouse events to CarPlay touch input.
final class CarPlayView: NSView {
    weak var delegate: CarPlayViewDelegate?

    var videoLayer: AVSampleBufferDisplayLayer!

    // Video content aspect ratio (updated on resolution change) — seeded from the
    // VehicleConfigModel's persisted main resolution, the single source of truth for
    // what the adapter encodes (and therefore for touch mapping).
    private(set) var videoAspect: CGFloat = VehicleConfigModel.persistedMainAspect()

    /// When true, touch input uses AA protocol (single-touch, 0..10000, type 0x05)
    var isAndroidAuto = false
    /// AA with declared codec margins (T4): the window is locked to the VISIBLE aspect, the codec
    /// frame is centre-cropped to it on both axes (gearhead splits margins evenly), and touch is
    /// mapped bounds-relative — i.e. straight into the visible rect the phone laid its UI out in.
    var aaMarginCrop = false

    /// AA MOVE throttle — 17ms interval matching AutoKit (~60fps)
    private static let aaTouchThrottleNs: UInt64 = 17_000_000
    private var lastAATouchSendTime: UInt64 = 0

    // Status overlay (shown when not streaming)
    private let statusOverlay = CALayer()
    private let statusIconLayer = CALayer()
    private let statusTextLayer = CATextLayer()
    private var isStreaming = false

    // Pairing-code panel (2026-09-03): the wireless SSP Numeric-Comparison code, one digit per shaded
    // cell, grouped n/2 + n/2 with a dash for even lengths ≥ 4. Visible only while a code is pending
    // (CT_PAIRING_CODE non-empty); the one-line status below keeps its normal watchdog text.
    private let pairingPanel = CALayer()
    private var pairingCells: [(box: CALayer, digit: CATextLayer)] = []
    private let pairingDash = CALayer()
    private var pairingCode: String?

    // The user's half of SSP Numeric Comparison. Real NSButtons rather than more CALayers: this is the
    // one place in the overlay that takes a decision from the user, and it must look, key-navigate and
    // speak like a macOS dialog — default button on the right, Return pairs, Escape cancels.
    private var pairingButtons: NSStackView?
    private var pairButton: NSButton?
    private var pairCancelButton: NSButton?
    /// Called with the user's answer (`true` = Pair). The buttons disable themselves on the way out,
    /// so this fires at most once per displayed code.
    var onPairingAnswer: ((Bool) -> Void)?

    // Re-apply the corner mask when iOS's streamed mask arrives/changes after first layout.
    // nonisolated so the nonisolated deinit can remove it (NotificationCenter removal is thread-safe).
    private nonisolated(unsafe) var cornerMaskObs: NSObjectProtocol?

    override init(frame: NSRect) {
        super.init(frame: frame)
        setup()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        setup()
    }

    deinit {
        if let cornerMaskObs { NotificationCenter.default.removeObserver(cornerMaskObs) }
    }

    // Cached icon images
    private var iconLight: NSImage?
    private var iconDark: NSImage?

    // Phase 3b "floating card" — clip the video to iOS's EXACT corner curve via `CarPlayCornerMask`
    // (Apple's own topLeftCornerMask bitmap, scaled per resolution). The window is non-opaque, so the
    // cut corners show the desktop through. No cornerRadius guess — see CornerMask.swift / docs/carplay/06_AV_PIPELINE.md.
    private func setup() {
        wantsLayer = true
        layer = CALayer()

        videoLayer = AVSampleBufferDisplayLayer()
        videoLayer.videoGravity = .resizeAspect

        guard let rootLayer = layer else { return }
        rootLayer.addSublayer(videoLayer)

        // Re-apply the corner mask when iOS's streamed topLeftCornerMask arrives (it may land after the
        // first layout()). CarPlayCornerMask posts this on install/clear; needsLayout re-runs layout().
        cornerMaskObs = NotificationCenter.default.addObserver(
            forName: .carPlayCornerMaskUpdated, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.needsLayout = true }
        }

        // Load both icon variants
        iconLight = NSImage(named: "ic_phone_projection_black")
        iconDark = NSImage(named: "ic_phone_projection")

        // Status overlay — centered icon + text below it
        statusOverlay.frame = bounds
        rootLayer.addSublayer(statusOverlay)

        statusIconLayer.contentsGravity = .resizeAspect
        statusIconLayer.frame = CGRect(x: 0, y: 0, width: 120, height: 120)
        statusOverlay.addSublayer(statusIconLayer)

        // Status text
        statusTextLayer.string = "Waiting for adapter..."
        statusTextLayer.fontSize = 16
        statusTextLayer.font = NSFont.systemFont(ofSize: 16, weight: .medium)
        statusTextLayer.alignmentMode = .center
        statusTextLayer.contentsScale = NSScreen.main?.backingScaleFactor ?? 2.0
        statusTextLayer.frame = CGRect(x: 0, y: 0, width: 400, height: 24)
        statusOverlay.addSublayer(statusTextLayer)

        pairingPanel.isHidden = true
        pairingDash.cornerRadius = 18
        pairingDash.cornerCurve = .continuous
        statusOverlay.addSublayer(pairingPanel)
        setupPairingButtons()

        applyAppearance()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyAppearance()
    }

    private func applyAppearance() {
        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua

        // Resolve dynamic NSColors within the correct appearance context
        var bgCG: CGColor = CGColor.black
        var textCG: CGColor = CGColor.white
        effectiveAppearance.performAsCurrentDrawingAppearance {
            bgCG = NSColor.windowBackgroundColor.cgColor
            textCG = isDark
                ? NSColor.white.withAlphaComponent(0.7).cgColor
                : NSColor.black.withAlphaComponent(0.6).cgColor
        }

        CATransaction.begin()
        CATransaction.setDisableActions(true)

        if !isStreaming && !isAndroidAuto {
            layer?.backgroundColor = bgCG
            videoLayer.backgroundColor = nil
            videoLayer.isHidden = true   // never hide the AA layer on a light/dark switch (video bug 2026-09-04)
        }

        statusIconLayer.contents = isDark ? iconDark : iconLight
        statusTextLayer.foregroundColor = textCG
        applyPairingAppearance(isDark: isDark)

        CATransaction.commit()
    }

    /// Cell fill + digit colour from the SEMANTIC system palette, resolved in the view's effective
    /// appearance (HIG: never hardcode; system fills and label colours track light/dark, Increase
    /// Contrast and Reduce Transparency on their own). Called inside a transaction.
    private func applyPairingAppearance(isDark: Bool) {
        _ = isDark
        var cellCG = CGColor.clear
        var dashCG = CGColor.clear
        var digitCG = CGColor.black
        effectiveAppearance.performAsCurrentDrawingAppearance {
            cellCG = NSColor.tertiarySystemFill.cgColor
            dashCG = NSColor.secondarySystemFill.cgColor
            digitCG = NSColor.labelColor.cgColor
        }
        pairingDash.backgroundColor = dashCG
        for cell in pairingCells {
            cell.box.backgroundColor = cellCG
            cell.digit.foregroundColor = digitCG
        }
    }

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        videoLayer.frame = bounds
        statusOverlay.frame = bounds
        if let l = layer { CarPlayCornerMask.apply(to: l, bounds: bounds) }
        updateAAVideoGravity()

        // Center the icon and text — with the pairing panel stacked above them while a code is pending.
        let iconSize: CGFloat = 120
        let gap: CGFloat = 16
        let textH: CGFloat = 24
        let panelH = layoutPairingPanelSize().height
        let panelGap: CGFloat = panelH > 0 ? 40 : 0
        // The answer buttons sit directly under the code they answer. Zero-height with no code, so
        // the no-pairing layout is byte-for-byte what it was.
        let buttonsSize = (panelH > 0 ? pairingButtons?.fittingSize : nil) ?? .zero
        let buttonsGap: CGFloat = buttonsSize.height > 0 ? 20 : 0
        let totalH = panelH + buttonsGap + buttonsSize.height + panelGap + iconSize + gap + textH
        let blockTop = bounds.midY + totalH / 2
        if panelH > 0 {
            layoutPairingPanel(top: blockTop, in: bounds)
        }
        if buttonsSize.height > 0, let stack = pairingButtons {
            stack.frame = CGRect(
                x: (bounds.midX - buttonsSize.width / 2).rounded(),
                y: (blockTop - panelH - buttonsGap - buttonsSize.height).rounded(),
                width: buttonsSize.width,
                height: buttonsSize.height
            )
        }
        let centerY = blockTop - panelH - buttonsGap - buttonsSize.height - panelGap

        statusIconLayer.frame = CGRect(
            x: bounds.midX - iconSize / 2,
            y: centerY - iconSize,
            width: iconSize,
            height: iconSize
        )
        statusTextLayer.frame = CGRect(
            x: bounds.midX - 200,
            y: centerY - iconSize - gap - textH,
            width: 400,
            height: textH
        )
        CATransaction.commit()
    }

    // MARK: - Pairing answer buttons

    /// Build the Pair/Cancel pair once, hidden. HIG: standard push buttons, action verb as the title,
    /// the default (Pair) rightmost and Return-keyed, Cancel Escape-keyed. Sized by `fittingSize` and
    /// positioned in `layout()` — the overlay is frame-driven, so no constraints here.
    private func setupPairingButtons() {
        let cancel = NSButton(title: "Cancel", target: self, action: #selector(pairingCancelClicked))
        cancel.bezelStyle = .rounded
        cancel.keyEquivalent = "\u{1b}" // Escape
        cancel.setAccessibilityLabel("Cancel pairing")

        let pair = NSButton(title: "Pair", target: self, action: #selector(pairingPairClicked))
        pair.bezelStyle = .rounded
        pair.keyEquivalent = "\r" // Return — makes this the window's default button while visible
        pair.setAccessibilityLabel("Pair — the codes match")

        let stack = NSStackView(views: [cancel, pair])
        stack.orientation = .horizontal
        stack.spacing = 12
        stack.isHidden = true
        stack.translatesAutoresizingMaskIntoConstraints = true
        addSubview(stack)

        pairingButtons = stack
        pairButton = pair
        pairCancelButton = cancel
    }

    @objc private func pairingPairClicked() { answerPairing(true) }
    @objc private func pairingCancelClicked() { answerPairing(false) }

    /// Deliver the answer and immediately disable both buttons. The box clears the code once it has
    /// acted, which hides them again — until then a second click must not be able to send a second,
    /// contradictory answer for the same code.
    private func answerPairing(_ accept: Bool) {
        guard pairButton?.isEnabled == true || pairCancelButton?.isEnabled == true else { return }
        pairButton?.isEnabled = false
        pairCancelButton?.isEnabled = false
        onPairingAnswer?(accept)
    }

    /// Show/hide the answer buttons, re-enabling them for each newly displayed code.
    /// Pair/Cancel are only meaningful when the box waits for this app's answer (host YAML
    /// `pairing: numeric_comparison_interactive`). Default off: the box confirms its own side and the
    /// user compares on the phone, so the panel is display-only.
    var pairingAnswerEnabled = false

    private func setPairingAnswerVisible(_ visible: Bool) {
        let visible = visible && pairingAnswerEnabled
        pairButton?.isEnabled = visible
        pairCancelButton?.isEnabled = visible
        pairingButtons?.isHidden = !visible
        // A hidden NSButton takes no key equivalent, so Return/Escape go back to being CarPlay
        // D-pad input the moment the prompt is gone (see `keyDown`).
    }

    // MARK: - Pairing-code panel

    /// Show `code` (any length; nil hides the panel). Digits render one per cell; even lengths ≥ 4 split
    /// into two groups around a dash, like the iPhone's own pairing sheet.
    func showPairingCode(_ code: String?) {
        let trimmed = code?.trimmingCharacters(in: .whitespacesAndNewlines)
        let code: String? = (trimmed?.isEmpty ?? true) ? nil : trimmed
        // BEFORE the unchanged-code early return: the box re-emits the live code to a host that
        // reattaches mid-pairing, and that host must get working buttons, not ones still disabled
        // from an answer the previous connection sent.
        setPairingAnswerVisible(code != nil && !isStreaming)
        guard code != pairingCode else { return }
        pairingCode = code
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        for cell in pairingCells {
            cell.box.removeFromSuperlayer()
        }
        pairingCells.removeAll()
        pairingDash.removeFromSuperlayer()
        if let code {
            let scale = NSScreen.main?.backingScaleFactor ?? 2.0
            for ch in code {
                let box = CALayer()
                box.cornerRadius = 8 // standard macOS control radius family
                box.cornerCurve = .continuous
                let digit = CATextLayer()
                digit.string = String(ch)
                digit.alignmentMode = .center
                digit.contentsScale = scale
                digit.truncationMode = .none
                box.addSublayer(digit)
                pairingPanel.addSublayer(box)
                pairingCells.append((box, digit))
            }
            if code.count >= 4 && code.count % 2 == 0 {
                pairingPanel.addSublayer(pairingDash)
            }
            let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            applyPairingAppearance(isDark: isDark)
            pairingPanel.isHidden = false
            // VoiceOver: the digits live in layers, so expose the code on the view itself.
            setAccessibilityLabel("Pairing code " + code.map(String.init).joined(separator: " ") + ". Confirm it matches your phone.")
        } else {
            pairingPanel.isHidden = true
            setAccessibilityLabel(nil)
        }
        CATransaction.commit()
        needsLayout = true
    }

    /// Cell geometry for the current code and bounds. Cells scale down on narrow windows.
    private func layoutPairingPanelSize() -> CGSize {
        guard let code = pairingCode, !code.isEmpty else { return .zero }
        let n = CGFloat(code.count)
        let hasDash = code.count >= 4 && code.count % 2 == 0
        let cellW = min(100, max(44, (bounds.width * 0.6) / (n + (hasDash ? 1.2 : 0))))
        let cellH = cellW * 1.6
        let cellGap = cellW * 0.2
        let dashW: CGFloat = hasDash ? cellW * 0.72 : 0
        // With a dash the middle seam is [pad][dash][pad] INSTEAD of a cell gap, so the dash sits
        // exactly between the groups: (n-2) cell gaps + 2 pads, not (n-1) gaps + 2 pads.
        let width = hasDash
            ? n * cellW + (n - 2) * cellGap + dashW + 2 * cellGap
            : n * cellW + (n - 1) * cellGap
        return CGSize(width: width, height: cellH)
    }

    /// Position the cells (and dash) with their top edge at `top`, centred horizontally.
    private func layoutPairingPanel(top: CGFloat, in bounds: CGRect) {
        guard let code = pairingCode, !code.isEmpty else { return }
        let size = layoutPairingPanelSize()
        let n = code.count
        let hasDash = n >= 4 && n % 2 == 0
        let cellH = size.height
        let cellW = cellH / 1.6
        let cellGap = cellW * 0.2
        let dashW: CGFloat = hasDash ? cellW * 0.72 : 0
        let dashH: CGFloat = cellW * 0.36
        let dashPad: CGFloat = hasDash ? cellGap : 0
        pairingPanel.frame = CGRect(x: bounds.midX - size.width / 2, y: top - cellH, width: size.width, height: cellH)
        var x: CGFloat = 0
        let fontSize = cellH * 0.5
        for (i, cell) in pairingCells.enumerated() {
            if hasDash && i == n / 2 {
                // Middle seam: symmetric [pad][dash][pad] replacing the cell gap.
                x += dashPad
                pairingDash.frame = CGRect(x: x, y: (cellH - dashH) / 2, width: dashW, height: dashH)
                x += dashW + dashPad
            }
            cell.box.frame = CGRect(x: x, y: 0, width: cellW, height: cellH)
            cell.digit.font = NSFont.monospacedDigitSystemFont(ofSize: fontSize, weight: .semibold)
            cell.digit.fontSize = fontSize
            cell.digit.frame = CGRect(x: 0, y: (cellH - fontSize * 1.2) / 2, width: cellW, height: fontSize * 1.2)
            x += cellW
            let nextIsDash = hasDash && i + 1 == n / 2
            if i < pairingCells.count - 1 && !nextIsDash {
                x += cellGap
            }
        }
    }

    // MARK: - Status Overlay

    func updateStatus(_ text: String) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        statusTextLayer.string = text
        CATransaction.commit()
    }

    func setStreaming(_ streaming: Bool) {
        isStreaming = streaming
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        statusOverlay.isHidden = streaming
        // The buttons are SUBVIEWS, not layers in `statusOverlay`, so hiding the overlay does not
        // hide them — video would start with a live Pair button floating over it, still owning
        // Return and Escape.
        setPairingAnswerVisible(!streaming && pairingCode != nil)
        if streaming {
            layer?.backgroundColor = NSColor.black.cgColor
            videoLayer.backgroundColor = NSColor.black.cgColor
            videoLayer.isHidden = false
        } else {
            videoLayer.isHidden = true
            applyAppearance()
        }
        CATransaction.commit()
    }

    func updateVideoAspect(width: CGFloat, height: CGFloat) {
        videoAspect = width / height
    }

    func clearAndroidAutoVideoMode() {
        videoLayer.videoGravity = .resizeAspect
        videoAspect = VehicleConfigModel.persistedMainAspect()
    }

    /// Dynamically choose gravity based on view vs video aspect ratio.
    /// Called on layout() to handle window resize.
    private func updateAAVideoGravity() {
        guard isAndroidAuto, bounds.height > 0 else { return }
        if aaMarginCrop { videoLayer.videoGravity = .resizeAspectFill; return }
        let viewAspect = bounds.width / bounds.height
        if viewAspect > videoAspect {
            // Window wider than video — crop top/bottom black bars
            videoLayer.videoGravity = .resizeAspectFill
        } else {
            // Window narrower/portrait — show full video, pillarbox
            videoLayer.videoGravity = .resizeAspect
        }
    }

    // Note: no NSTrackingArea — this view handles no mouseMoved/entered/exited
    // events (mouseDown/drag/up and scrollWheel don't need one), and an area
    // without a tracking-type option raises NSInternalInconsistencyException
    // in NSTrackingArea.init, killing window construction at launch.

    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        // Keep the status text crisp when the window moves between displays
        // with different backing scales (Retina ↔ non-Retina).
        statusTextLayer.contentsScale = window?.backingScaleFactor ?? 2.0
    }

    // MARK: - Mouse → Touch

    override var acceptsFirstResponder: Bool { true }

    // The video lives in a borderless, floating window that is clicked to focus constantly. Without
    // this, the click that re-activates the window is swallowed and the first CarPlay tap after any
    // focus loss is eaten.
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    // MARK: - Single-Finger (Mouse Click/Drag)

    /// True while a touch that started inside the video rect is in progress.
    /// A drag that leaves the video (letterbox bar, off-window) is clamped to
    /// the video edge rather than dropped — dropping the .up event would
    /// leave the phone with a stuck touch that keeps scrolling/pressing.
    /// Conversely, a drag that started outside the video never sends events.
    private var touchActive = false

    override func mouseDown(with event: NSEvent) {
        // ⌘-drag moves the borderless window (there is no title bar). A plain drag stays a CarPlay
        // touch, so the video surface is never sacrificed for window movement.
        if event.modifierFlags.contains(.command) {
            window?.performDrag(with: event)
            return
        }
        if isAndroidAuto {
            sendAATouch(event: event, action: .down)
        } else {
            sendTouch(event: event, action: .down)
        }
    }

    override func mouseDragged(with event: NSEvent) {
        if isAndroidAuto {
            sendAATouch(event: event, action: .move)
        } else {
            sendTouch(event: event, action: .move)
        }
    }

    override func mouseUp(with event: NSEvent) {
        if isAndroidAuto {
            sendAATouch(event: event, action: .up)
        } else {
            sendTouch(event: event, action: .up)
        }
    }

    /// Normalizes for an in-progress touch: clamps the location into the
    /// video rect first, so drag/up outside the video map to the nearest
    /// edge instead of being lost.
    private func normalizedContinuationPoint(_ locationInView: CGPoint) -> (Float32, Float32)? {
        let vRect = videoRect
        guard vRect.width > 0, vRect.height > 0 else { return nil }
        let clamped = CGPoint(
            x: min(max(locationInView.x, vRect.minX), vRect.maxX),
            y: min(max(locationInView.y, vRect.minY), vRect.maxY)
        )
        // The point is already clamped into vRect, so skip the containment check:
        // CGRect.contains is half-open and EXCLUDES points exactly on maxX/maxY, so a
        // drag/up released past the right or top edge (which clamps to maxX/maxY) would
        // otherwise be dropped — leaving the phone with a stuck touch.
        return normalizePoint(clamped, skipContainmentCheck: true)
    }

    /// CarPlay touch — normalized float 0..1
    private func sendTouch(event: NSEvent, action: MultiTouchAction) {
        let locationInView = convert(event.locationInWindow, from: nil)
        let point: (Float32, Float32)?
        switch action {
        case .down:
            point = normalizePoint(locationInView)
            touchActive = point != nil
        default:
            guard touchActive else { return }
            point = normalizedContinuationPoint(locationInView)
            if action == .up { touchActive = false }
        }
        guard let (x, y) = point else { return }
        delegate?.carPlayView(self, didMultiTouch: action, x: x, y: y)
    }

    /// Android Auto touch — integer 0..10000, single pointer, 17ms MOVE throttle
    private func sendAATouch(event: NSEvent, action: TouchAction) {
        // Throttle MOVE to 17ms intervals (AutoKit compatibility)
        if action == .move {
            let now = DispatchTime.now().uptimeNanoseconds
            if now - lastAATouchSendTime < Self.aaTouchThrottleNs { return }
            lastAATouchSendTime = now
        }
        let locationInView = convert(event.locationInWindow, from: nil)
        let point: (Float32, Float32)?
        switch action {
        case .down:
            point = normalizePoint(locationInView)
            touchActive = point != nil
        default:
            guard touchActive else { return }
            point = normalizedContinuationPoint(locationInView)
            if action == .up { touchActive = false }
        }
        guard let (nx, ny) = point else { return }
        let x = UInt32((nx * 10000).rounded())
        let y = UInt32((ny * 10000).rounded())
        delegate?.carPlayView(self, didTouch: action, x: x, y: y)
    }

    // MARK: - Two-Finger Scroll (Trackpad swipe → two-finger drag)

    private var scrollGestureActive = false
    private var scrollCenter: (Float32, Float32) = (0.5, 0.5)

    /// Shifts a synthesized two-finger pair inward so both points stay in
    /// 0...1 while preserving their separation — out-of-range normalized
    /// coordinates can desynchronize the gesture on the phone.
    private func clampedPair(center: Float32, separation: Float32) -> (Float32, Float32) {
        var lo = center - separation
        var hi = center + separation
        if lo < 0 { hi -= lo; lo = 0 }
        if hi > 1 { lo -= hi - 1; hi = 1 }
        return (max(0, lo), min(1, hi))
    }

    override func scrollWheel(with event: NSEvent) {
        // AA only supports single-touch — ignore two-finger gestures
        if isAndroidAuto { return }

        // End/cancel first: these replay the latched center, so they must
        // fire even when the pointer has left the video rect — dropping the
        // .up here would leave the phone with a stuck two-finger touch.
        if event.phase == .ended || event.phase == .cancelled {
            if scrollGestureActive {
                let sep: Float32 = 0.05
                let (lo, hi) = clampedPair(center: scrollCenter.0, separation: sep)
                let points: [(action: MultiTouchAction, x: Float32, y: Float32, id: UInt32)] = [
                    (.up, lo, scrollCenter.1, 0),
                    (.up, hi, scrollCenter.1, 1),
                ]
                delegate?.carPlayView(self, didMultiTouchTwo: points)
                scrollGestureActive = false
            }
            return
        }

        // Never interleave synthesized two-finger points with an active
        // single-finger drag — the phone tracks the same pointer ids, and a
        // synthesized .up would lift the drag's finger mid-gesture.
        if touchActive { return }

        let locationInView = convert(event.locationInWindow, from: nil)
        guard let (cx, cy) = normalizePoint(locationInView) else { return }

        // Finger separation for two-finger scroll (fixed 5% apart)
        let sep: Float32 = 0.05

        // A physical mouse wheel reports no gesture phases (phases are
        // trackpad-only) — synthesize a complete short two-finger drag per
        // wheel tick so mouse scrolling works at all.
        if event.phase == [] && event.momentumPhase == [] {
            let vRect = videoRect
            guard vRect.width > 0, vRect.height > 0 else { return }
            let dx = Float32(event.scrollingDeltaX / Double(vRect.width)) * (event.hasPreciseScrollingDeltas ? 1.0 : 10.0)
            let dy = Float32(-event.scrollingDeltaY / Double(vRect.height)) * (event.hasPreciseScrollingDeltas ? 1.0 : 10.0)
            guard dx != 0 || dy != 0 else { return }
            let tx = max(0, min(1, cx + dx))
            let ty = max(0, min(1, cy + dy))
            let (dLo, dHi) = clampedPair(center: cx, separation: sep)
            let (mLo, mHi) = clampedPair(center: tx, separation: sep)
            delegate?.carPlayView(self, didMultiTouchTwo: [(.down, dLo, cy, 0), (.down, dHi, cy, 1)])
            delegate?.carPlayView(self, didMultiTouchTwo: [(.move, mLo, ty, 0), (.move, mHi, ty, 1)])
            delegate?.carPlayView(self, didMultiTouchTwo: [(.up, mLo, ty, 0), (.up, mHi, ty, 1)])
            return
        }

        if event.phase == .began {
            scrollGestureActive = true
            scrollCenter = (cx, cy)
            // DOWN both fingers
            let (lo, hi) = clampedPair(center: cx, separation: sep)
            let points: [(action: MultiTouchAction, x: Float32, y: Float32, id: UInt32)] = [
                (.down, lo, cy, 0),
                (.down, hi, cy, 1),
            ]
            delegate?.carPlayView(self, didMultiTouchTwo: points)
        } else if event.phase == .changed, scrollGestureActive {
            // Accumulate scroll deltas into the center position
            let dx = Float32(event.scrollingDeltaX / Double(videoRect.width)) * (event.hasPreciseScrollingDeltas ? 1.0 : 10.0)
            let dy = Float32(-event.scrollingDeltaY / Double(videoRect.height)) * (event.hasPreciseScrollingDeltas ? 1.0 : 10.0)
            scrollCenter.0 = max(0, min(1, scrollCenter.0 + dx))
            scrollCenter.1 = max(0, min(1, scrollCenter.1 + dy))
            let (lo, hi) = clampedPair(center: scrollCenter.0, separation: sep)
            let points: [(action: MultiTouchAction, x: Float32, y: Float32, id: UInt32)] = [
                (.move, lo, scrollCenter.1, 0),
                (.move, hi, scrollCenter.1, 1),
            ]
            delegate?.carPlayView(self, didMultiTouchTwo: points)
        }
        // (.ended/.cancelled handled before the location guard above)
    }

    // MARK: - Pinch to Zoom (Trackpad magnify → two fingers apart/together)

    private var pinchActive = false
    private var pinchCenter: (Float32, Float32) = (0.5, 0.5)
    private var pinchSpread: Float32 = 0.1

    override func magnify(with event: NSEvent) {
        // AA only supports single-touch — ignore pinch gestures
        if isAndroidAuto { return }

        // End/cancel first — same rationale as scrollWheel: the .up replays
        // the latched center and must never be dropped by the location guard.
        if event.phase == .ended || event.phase == .cancelled {
            if pinchActive {
                let (lo, hi) = clampedPair(center: pinchCenter.0, separation: pinchSpread)
                let points: [(action: MultiTouchAction, x: Float32, y: Float32, id: UInt32)] = [
                    (.up, lo, pinchCenter.1, 0),
                    (.up, hi, pinchCenter.1, 1),
                ]
                delegate?.carPlayView(self, didMultiTouchTwo: points)
                pinchActive = false
            }
            return
        }

        // Don't interleave with an active single-finger drag (pointer-id
        // collision — see scrollWheel).
        if touchActive { return }

        let locationInView = convert(event.locationInWindow, from: nil)
        guard let (cx, cy) = normalizePoint(locationInView) else { return }

        if event.phase == .began {
            pinchActive = true
            pinchCenter = (cx, cy)
            pinchSpread = 0.1
            let (lo, hi) = clampedPair(center: cx, separation: pinchSpread)
            let points: [(action: MultiTouchAction, x: Float32, y: Float32, id: UInt32)] = [
                (.down, lo, cy, 0),
                (.down, hi, cy, 1),
            ]
            delegate?.carPlayView(self, didMultiTouchTwo: points)
        } else if event.phase == .changed, pinchActive {
            // magnification is a delta: positive = zoom in (spread fingers), negative = zoom out
            pinchSpread = max(0.02, min(0.45, pinchSpread + Float32(event.magnification) * 0.15))
            let (lo, hi) = clampedPair(center: pinchCenter.0, separation: pinchSpread)
            let points: [(action: MultiTouchAction, x: Float32, y: Float32, id: UInt32)] = [
                (.move, lo, pinchCenter.1, 0),
                (.move, hi, pinchCenter.1, 1),
            ]
            delegate?.carPlayView(self, didMultiTouchTwo: points)
        }
        // (.ended/.cancelled handled before the location guard above)
    }

    // MARK: - Keyboard → Adapter Commands

    override func keyDown(with event: NSEvent) {
        // Ignore key repeats — D-pad and commands should be single-press
        guard !event.isARepeat else { return }
        if let cmd = commandForKey(event) {
            delegate?.carPlayView(self, didPressCommand: cmd)
        }
        // Don't call super — prevents system beep for unhandled keys
    }

    private func commandForKey(_ event: NSEvent) -> CommandID? {
        // Modified chords are not CarPlay input. Without this guard, any
        // ⌘/⌃/⌥ combination AppKit didn't claim fell through to keyDown and
        // fired a command (⌘S activated Siri, ⌘P sent previous-track…).
        guard event.modifierFlags.intersection([.command, .control, .option]).isEmpty else {
            return nil
        }

        // Special keys (arrows, escape, etc.)
        // Up/Down use knob commands for item-by-item list scrolling.
        // D-pad Up/Down (102/103) are page-level jumps in CarPlay.
        switch event.keyCode {
        case 123: return .dpadLeft       // ←  page/tab left
        case 124: return .dpadRight      // →  page/tab right
        case 126: return .knobUp         // ↑  scroll up one item
        case 125: return .knobDown       // ↓  scroll down one item
        case 36:  return .dpadEnter      // Return/Enter  select
        case 53:  return .dpadBack       // Escape  back
        case 49:  return .mediaPlayPause // Space bar
        default: break
        }

        // Character keys
        switch event.charactersIgnoringModifiers?.lowercased() {
        case "h": return .mediaHome        // H = Home
        case "s": return .siriDown         // S = Siri
        case "n": return .mediaNext        // N = Next track
        case "p": return .mediaPrev        // P = Previous track
        case "b": return .dpadBack         // B = Back
        default: break
        }

        return nil
    }

    // Note: hardware media keys (play/pause, next, prev) are handled via
    // MPRemoteCommandCenter in NowPlayingManager — .systemDefined events
    // never reach performKeyEquivalent, so intercepting them here was dead
    // code and has been removed.

    // MARK: - Coordinate Helpers

    /// Whether AA crop mode is active (resizeAspectFill, landscape window wider than 16:9 video).
    private var isAACropMode: Bool {
        isAndroidAuto && bounds.height > 0 && (aaMarginCrop || (bounds.width / bounds.height) > videoAspect)
    }

    private func normalizePoint(_ locationInView: CGPoint, skipContainmentCheck: Bool = false) -> (Float32, Float32)? {
        let vRect = videoRect
        guard vRect.width > 0, vRect.height > 0 else { return nil }

        // Standard (resizeAspect) mode — CarPlay or AA portrait/narrow: the click must land within the
        // visible (letterboxed/pillarboxed) video rect. AA crop mode (resizeAspectFill) fills the view
        // and the surface extends BEYOND it on the cropped axis, so every in-view tap is on the surface —
        // skip the containment check. Both then use the SAME rect-relative mapping, which correctly
        // accounts for the (possibly negative) crop offset `vRect.origin`. The previous crop branch
        // omitted `vRect.origin.y`, mapping taps too high with error growing toward the bottom (audit M-i).
        // Continuation points (drag/up) are pre-clamped into vRect by the caller and pass
        // skipContainmentCheck, since contains() would reject the clamped max-edge coordinate.
        if aaMarginCrop {
            // Visible rect == the view (window aspect is locked to it): bounds-relative is the phone's
            // coordinate space, no overflow arithmetic needed.
            guard bounds.width > 0, bounds.height > 0 else { return nil }
            let x = Float32(locationInView.x / bounds.width)
            let y = Float32(1.0 - locationInView.y / bounds.height)
            return (max(0, min(1, x)), max(0, min(1, y)))
        }
        if !isAACropMode && !skipContainmentCheck {
            guard vRect.contains(locationInView) else { return nil }
        }
        let x = Float32((locationInView.x - vRect.origin.x) / vRect.width)
        let y = Float32(1.0 - (locationInView.y - vRect.origin.y) / vRect.height)
        return (max(0, min(1, x)), max(0, min(1, y)))
    }

    /// Computes the video content rect relative to the view.
    /// - AA crop mode (landscape wider than 16:9): rect extends beyond bounds (resizeAspectFill).
    /// - All other cases (CarPlay, AA portrait): rect fits within bounds (resizeAspect).
    private var videoRect: CGRect {
        let viewAspect = bounds.width / bounds.height

        if isAACropMode {
            if viewAspect > videoAspect {
                // resizeAspectFill: video scaled to fill view, height overflows.
                let videoHeight = bounds.width / videoAspect
                let y = (bounds.height - videoHeight) / 2
                return CGRect(x: 0, y: y, width: bounds.width, height: videoHeight)
            }
            // margin crop with a taller-than-video window: width overflows.
            let videoWidth = bounds.height * videoAspect
            let x = (bounds.width - videoWidth) / 2
            return CGRect(x: x, y: 0, width: videoWidth, height: bounds.height)
        }

        // resizeAspect: video fits within view bounds
        if viewAspect > videoAspect {
            // Pillarboxed (black bars on sides)
            let videoWidth = bounds.height * videoAspect
            let x = (bounds.width - videoWidth) / 2
            return CGRect(x: x, y: 0, width: videoWidth, height: bounds.height)
        } else {
            // Letterboxed (black bars top/bottom)
            let videoHeight = bounds.width / videoAspect
            let y = (bounds.height - videoHeight) / 2
            return CGRect(x: 0, y: y, width: bounds.width, height: videoHeight)
        }
    }
}
