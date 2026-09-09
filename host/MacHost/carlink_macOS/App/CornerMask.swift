// CornerMask.swift — apply Apple's EXACT CarPlay corner curve to the video layer (Phase 3b, docs/carplay/06_AV_PIPELINE.md).
//
// Rather than guess a `cornerRadius`, we use iOS's own `topLeftCornerMask` bitmap. That mask is a single
// canonical continuous-curvature ("squircle") corner that iOS scales by display width (verified: 102px
// @1920w and 68px @1280w are the same shape scaled). The captured mask is bundled as an asset
// ("carplay_corner_mask", 8-bit grayscale where luma = coverage), and here we build a full-frame alpha
// mask — opaque center, all four corners cut to Apple's exact curve, each corner scaled to
// `width × cornerFraction`. Because it's the real bitmap scaled, it's pixel-faithful at any custom
// resolution with no magic radius.

import AppKit

extension Notification.Name {
    /// Posted (on main) when the streamed corner mask changes, so CarPlayView/AltVideoView re-apply it
    /// even if the mask arrives after the view's first `layout()`.
    static let carPlayCornerMaskUpdated = Notification.Name("carPlayCornerMaskUpdated")
}

// @MainActor: `sessionCorner` is mutable static state written from the OCBM metadata handler (which
// runs inside `Task { @MainActor }`) and read in NSView `layout()` (already main-isolated). Isolating
// the whole enum keeps that state race-free under Swift 6 (precedent: `@MainActor enum ControlPopups`).
@MainActor
enum CarPlayCornerMask {
    /// FALLBACK corner region size ÷ display width, used ONLY when iOS's streamed mask is unavailable.
    /// NOTE: iOS actually sizes the mask as a DISCRETE value (34pt × 2×/3× point-scale = 68/102 px, keyed
    /// to physical display size — iOS-27 `CRCornerMask`/`CRDisplayScaleInfo`), NOT a width fraction; this
    /// 0.0531 two-point fit is only a rough fallback and is wrong at untested resolutions (e.g. 2400×960).
    /// The primary path uses `sessionCorner` (iOS's exact streamed bitmap) — see `setSessionCorner`.
    static let cornerFraction: CGFloat = 68.0 / 1280.0   // ≈ 0.0531

    /// Decode a corner-mask image to normalized coverage: row-major `n×n`, 0 = cut/transparent,
    /// 255 = keep/opaque, with the transparent nub forced to array (0,0). Shared by the bundled asset
    /// and the streamed PNG. Returns nil (→ caller falls back) on a non-square / degenerate image.
    private static func decodeCoverage(_ cg: CGImage) -> (coverage: [UInt8], n: Int)? {
        let n = cg.width
        guard n > 1, cg.height == n else { return nil }
        var raw = [UInt8](repeating: 0, count: n * n)
        let gray = CGColorSpaceCreateDeviceGray()
        // 05-L1: `&raw` only converts to a valid pointer for the duration of the CGContext(data:)
        // call itself — the returned context retaining it past that is documented UB. Use the same
        // withUnsafeMutableBytes pattern maskImage(size:) already uses below to keep the buffer's
        // pointer alive for the context's whole lifetime, draw included.
        let drew: Bool = raw.withUnsafeMutableBytes { ptr in
            guard let ctx = CGContext(data: ptr.baseAddress, width: n, height: n, bitsPerComponent: 8,
                                      bytesPerRow: n, space: gray,
                                      bitmapInfo: CGImageAlphaInfo.none.rawValue) else { return false }
            ctx.draw(cg, in: CGRect(x: 0, y: 0, width: n, height: n))
            return true
        }
        guard drew else { return nil }
        // Normalize orientation independent of any draw flip: put the nub (min-coverage corner) at (0,0).
        let tl = raw[0], tr = raw[n - 1], bl = raw[(n - 1) * n], br = raw[(n - 1) * n + (n - 1)]
        let flipX = min(tr, br) < min(tl, bl)
        let flipY = min(bl, br) < min(tl, tr)
        var cov = [UInt8](repeating: 0, count: n * n)
        for y in 0..<n {
            let sy = flipY ? n - 1 - y : y
            for x in 0..<n {
                let sx = flipX ? n - 1 - x : x
                cov[y * n + x] = raw[sy * n + sx]
            }
        }
        return (cov, n)
    }

    /// The bundled corner mask (fallback), normalized so the transparent nub is at array (0,0).
    private static let corner: (coverage: [UInt8], n: Int)? = {
        guard let img = NSImage(named: "carplay_corner_mask"),
              let cg = img.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return nil }
        return decodeCoverage(cg)
    }()

    /// iOS's EXACT streamed `topLeftCornerMask` for the current session, plus the display pixel width it
    /// was computed for. Set from the box's META_CORNERMASK forward; preferred over the bundled asset so
    /// the rendered corner matches CarPlay at ANY resolution. Cleared at session end.
    static private(set) var sessionCorner: (coverage: [UInt8], n: Int, srcWidth: Int)?

    /// Install iOS's streamed corner PNG. `displayWidth` = the pixel width the mask corresponds to, so
    /// the corner scales exactly (`cp = view_width × n / displayWidth`). Leaves `sessionCorner` unchanged
    /// on a bad/degenerate PNG (the bundled fallback then applies).
    static func setSessionCorner(_ png: Data, displayWidth: Int) {
        guard displayWidth > 0,
              let src = CGImageSourceCreateWithData(png as CFData, nil),
              let cg = CGImageSourceCreateImageAtIndex(src, 0, nil),
              let (cov, n) = decodeCoverage(cg) else {
            NSLog("[cornermask] streamed PNG decode failed (\(png.count) B, width=\(displayWidth)) — keeping fallback")
            return
        }
        sessionCorner = (cov, n, displayWidth)
        NSLog("[cornermask] streamed corner installed: n=\(n) px @ display_width=\(displayWidth) (fraction \(Double(n)/Double(displayWidth)))")
        NotificationCenter.default.post(name: .carPlayCornerMaskUpdated, object: nil)
    }

    /// Drop the streamed corner (session end) so the next session starts from the bundled fallback until
    /// its own mask arrives.
    static func clearSessionCorner() {
        guard sessionCorner != nil else { return }
        sessionCorner = nil
        NotificationCenter.default.post(name: .carPlayCornerMaskUpdated, object: nil)
    }

    /// Build a full-frame alpha mask CGImage for a video of `size`. Prefers iOS's streamed corner
    /// (exact) and falls back to the bundled asset. Nil if neither is available (caller then no-rounds).
    static func maskImage(size: CGSize) -> CGImage? {
        // 05-M1: the app's own corner carve is only correct once iOS has DECLARED the accessory owns
        // the corner (enablesCornerMasks). With it off, iOS rounds its own UI at the Apple radius and
        // streams no mask; carving one here (esp. the width-fraction fallback) cuts INTO live CarPlay
        // UI at any width other than 1280/1920 (docs/carplay/06_AV_PIPELINE.md §6c). Read the same
        // persisted VehicleConfigModel field the rest of the app treats as the single source of truth
        // — no new source of truth introduced.
        guard VehicleConfigModel.shared.enablesCornerMasks else { return nil }
        let w = Int(size.width.rounded()), h = Int(size.height.rounded())
        guard w > 2, h > 2 else { return nil }
        // Source selection: streamed (exact, cp = width × n/srcWidth) beats bundled (cp = width × 0.0531).
        let cov: [UInt8]; let n: Int; let cpRaw: CGFloat
        if let sc = sessionCorner {
            cov = sc.coverage; n = sc.n
            cpRaw = size.width * CGFloat(sc.n) / CGFloat(sc.srcWidth)
        } else if let c = corner {
            cov = c.coverage; n = c.n
            cpRaw = size.width * cornerFraction
        } else {
            return nil
        }
        let cp = min(max(1, Int(cpRaw.rounded())), min(w, h) / 2)

        var buf = [UInt8](repeating: 255, count: w * h)   // 255 = opaque everywhere, carve the corners
        // Nearest-neighbour sample the n×n corner down/up to cp×cp and stamp all four corners with flips.
        // (nub at each OUTER corner: TL as-is, TR flip-x, BL flip-y, BR flip-both.)
        for j in 0..<cp {
            let sy = j * n / cp
            for i in 0..<cp {
                let a = cov[sy * n + (i * n / cp)]
                buf[j * w + i] = a                              // top-left
                buf[j * w + (w - 1 - i)] = a                    // top-right
                buf[(h - 1 - j) * w + i] = a                    // bottom-left
                buf[(h - 1 - j) * w + (w - 1 - i)] = a          // bottom-right
            }
        }
        return buf.withUnsafeMutableBytes { ptr -> CGImage? in
            guard let ctx = CGContext(data: ptr.baseAddress, width: w, height: h, bitsPerComponent: 8,
                                      bytesPerRow: w, space: CGColorSpaceCreateDeviceGray(),
                                      bitmapInfo: CGImageAlphaInfo.alphaOnly.rawValue) else { return nil }
            return ctx.makeImage()
        }
    }

    /// Install/update the corner mask on `host`'s layer for the current bounds. Falls back to no mask if
    /// the asset is missing. The mask layer is symmetric, so image orientation doesn't matter.
    static func apply(to host: CALayer, bounds: CGRect) {
        guard bounds.width > 2, bounds.height > 2, let img = maskImage(size: bounds.size) else {
            host.mask = nil
            return
        }
        let m = host.mask ?? CALayer()
        m.frame = bounds
        m.contents = img
        host.mask = m
    }
}
