// VideoSurface.swift — the picture. Decoded pictures (BGRA on an IOSurface) arrive on VideoToolbox's
// thread; the newest one becomes the contents of a CALayer on the main queue. `contentsGravity` is
// `.resizeAspect`, so the picture keeps the decoded buffer's own aspect (a 498x1080 phone screen stays
// a tall strip whatever the window's shape). Shown as soon as decoded: there is no playout clock here.

import AppKit
import CoreVideo
import QuartzCore
import SwiftUI
import Synchronization

final class VideoSurface: Sendable {
    private struct Pending: @unchecked Sendable {
        var buffer: CVPixelBuffer?
        var clear = false
        var scheduled = false
    }

    private let pending = Mutex(Pending())
    @MainActor let layer: CALayer
    /// The buffer on screen. Held so VideoToolbox's pool cannot recycle its surface while it is shown.
    @MainActor private var shown: CVPixelBuffer?

    @MainActor init() {
        let l = CALayer()
        l.contentsGravity = .resizeAspect
        l.backgroundColor = CGColor(gray: 0, alpha: 1)
        l.actions = ["contents": NSNull(), "bounds": NSNull(), "position": NSNull()]
        layer = l
    }

    /// Any thread. A picture that arrives before the previous one was put on screen replaces it.
    func present(_ buffer: CVPixelBuffer) {
        nonisolated(unsafe) let b = buffer
        let schedule = pending.withLock { p -> Bool in
            p.buffer = b
            p.clear = false
            defer { p.scheduled = true }
            return !p.scheduled
        }
        if schedule { hop() }
    }

    /// Any thread. Take the picture off the screen (the stream ended).
    func clear() {
        let schedule = pending.withLock { p -> Bool in
            p.buffer = nil
            p.clear = true
            defer { p.scheduled = true }
            return !p.scheduled
        }
        if schedule { hop() }
    }

    private func hop() {
        DispatchQueue.main.async {
            MainActor.assumeIsolated { self.apply() }
        }
    }

    @MainActor private func apply() {
        let p = pending.withLock { p -> Pending in
            let copy = p
            p = Pending()
            return copy
        }
        if let buffer = p.buffer {
            shown = buffer
            layer.contents = CVPixelBufferGetIOSurface(buffer)?.takeUnretainedValue()
        } else if p.clear {
            shown = nil
            layer.contents = nil
        }
    }
}

struct VideoSurfaceView: NSViewRepresentable {
    let surface: VideoSurface

    func makeNSView(context: Context) -> NSView {
        let view = HostView()
        view.wantsLayer = true
        view.layer?.backgroundColor = CGColor(gray: 0, alpha: 1)
        view.videoLayer = surface.layer
        view.layer?.addSublayer(surface.layer)
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {}

    private final class HostView: NSView {
        var videoLayer: CALayer?

        override func layout() {
            super.layout()
            videoLayer?.frame = bounds
            videoLayer?.contentsScale = window?.backingScaleFactor ?? 2
        }
    }
}
