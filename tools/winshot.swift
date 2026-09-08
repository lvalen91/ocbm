// winshot — capture a macOS window BY OWNER PID, and measure the PNG it produced.
//
// Dependency-free (Foundation + CoreGraphics + ImageIO). Build via the wrapper `tools/winshot.sh`
// (compiles into /tmp, never into the iCloud-synced source tree) or by hand:
//
//     /usr/bin/swiftc -O -o /tmp/winshot tools/winshot.swift
//
// WHY BY WINDOW ID, NOT BY SCREEN REGION. `screencapture -R x,y,w,h` copies whatever is composited on
// screen inside that rectangle. On 2026-09-07 a Terminal window sat over the CarPlay picture during a
// resize run; a region grab would have scored the Terminal's pixels and silently poisoned every
// verdict. `screencapture -o -l<CGWindowID>` renders the window's OWN backing store, so an
// overlapping window does not appear in it. VERIFIED on this machine (macOS 27.0 / Darwin 27,
// 2026-09-07): a Terminal window partly covered by the CarPlay window was captured with `-l`; over the
// covered region its pixels differed from the on-screen region grab by a mean of 42.7/255 and showed
// Terminal content, not CarPlay. Limits observed the same day: a window that is NOT on screen
// (hidden / minimised / on another Space, `onscreen:false` in `list`) fails with "could not create
// image from window" — the tool reports that as ok:false rather than falling back to a region grab.
// The capture includes the title bar (48 pt at 2x = 96 px on this display) — use `--top` in
// `stats`/`diff` to skip it. Needs Screen Recording permission for the CALLING terminal.
//
// USAGE
//     winshot list  <pid>                                   -> JSON array of the pid's windows
//     winshot shot  <pid> <out.png> [--id N | --title SUB]  -> JSON {ok,path,id,title,width,height}
//                   default window: the largest on-screen layer-0 window owned by <pid>
//     winshot stats <png> [--rect WxH@X,Y --src WxH] [--top PX]
//                   -> JSON {width,height,region{...},meanLuma,stdLuma,darkFrac,brightFrac}
//     winshot diff  <a.png> <b.png> [--rect WxH@X,Y --src WxH] [--top PX]
//                   -> JSON {meanAbsDiff,changedFrac,a{...},b{...}}
//
// `--rect` is given in SOURCE coordinates (the CarPlay panel, e.g. 800x480@1520,840) and `--src` is
// the source size (the coded panel, e.g. 3840x2160); the tool maps that rectangle proportionally onto
// the image below `--top`. For a PNG that IS the decoded frame (the app's `shot` command) pass
// `--src` equal to the coded size and omit `--top`, and the mapping is 1:1.
//
// Luma is BT.601 on 8-bit RGB (0..255). `darkFrac` is the share of pixels with luma < 16;
// `brightFrac` luma > 200; `changedFrac` in diff is the share with |Δluma| > 24. These are the raw
// numbers a classifier thresholds on; the tool itself makes no verdict.

import CoreGraphics
import Foundation
import ImageIO

// MARK: - helpers

func die(_ msg: String, code: Int32 = 2) -> Never {
    FileHandle.standardError.write((msg + "\n").data(using: .utf8)!)
    exit(code)
}

func jsonOut(_ obj: Any) {
    guard let d = try? JSONSerialization.data(withJSONObject: obj, options: [.sortedKeys]),
          let s = String(data: d, encoding: .utf8) else { die("could not serialise") }
    print(s)
}

struct Rect { var x: Int, y: Int, w: Int, h: Int }

/// "WxH@X,Y" -> Rect ; "WxH" -> Rect at 0,0
func parseRect(_ s: String) -> Rect? {
    let parts = s.split(separator: "@", maxSplits: 1).map(String.init)
    let wh = parts[0].split(separator: "x").map(String.init)
    guard wh.count == 2, let w = Int(wh[0]), let h = Int(wh[1]) else { return nil }
    var x = 0, y = 0
    if parts.count == 2 {
        let xy = parts[1].split(separator: ",").map(String.init)
        guard xy.count == 2, let px = Int(xy[0]), let py = Int(xy[1]) else { return nil }
        x = px; y = py
    }
    return Rect(x: x, y: y, w: w, h: h)
}

/// Pull `--flag value` pairs out of argv; returns (positional, flags).
func splitArgs(_ a: [String]) -> ([String], [String: String]) {
    var pos: [String] = [], flags: [String: String] = [:]
    var i = 0
    while i < a.count {
        if a[i].hasPrefix("--") {
            guard i + 1 < a.count else { die("flag \(a[i]) needs a value") }
            flags[String(a[i].dropFirst(2))] = a[i + 1]; i += 2
        } else { pos.append(a[i]); i += 1 }
    }
    return (pos, flags)
}

// MARK: - window enumeration

struct WinInfo {
    let id: Int, title: String, layer: Int, onscreen: Bool, alpha: Double, bounds: CGRect
    var json: [String: Any] {
        ["id": id, "title": title, "layer": layer, "onscreen": onscreen, "alpha": alpha,
         "bounds": ["x": Int(bounds.origin.x), "y": Int(bounds.origin.y),
                    "w": Int(bounds.width), "h": Int(bounds.height)]]
    }
}

func windows(ownedBy pid: Int32) -> [WinInfo] {
    guard let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] else {
        return []
    }
    var out: [WinInfo] = []
    for w in list where (w[kCGWindowOwnerPID as String] as? Int32) == pid {
        guard let id = w[kCGWindowNumber as String] as? Int,
              let bd = w[kCGWindowBounds as String] as? NSDictionary,
              let bounds = CGRect(dictionaryRepresentation: bd) else { continue }
        out.append(WinInfo(id: id,
                           title: w[kCGWindowName as String] as? String ?? "",
                           layer: w[kCGWindowLayer as String] as? Int ?? 0,
                           onscreen: (w[kCGWindowIsOnscreen as String] as? Bool) ?? false,
                           alpha: w[kCGWindowAlpha as String] as? Double ?? 1,
                           bounds: bounds))
    }
    return out
}

// MARK: - capture

/// `screencapture -x -o -l<id>`: -x no sound, -o no window shadow, -l the window's own image.
func capture(windowID: Int, to path: String) -> (Bool, String) {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
    p.arguments = ["-x", "-o", "-l\(windowID)", path]
    let err = Pipe(); p.standardError = err; p.standardOutput = err
    do { try p.run() } catch { return (false, "could not run screencapture: \(error)") }
    p.waitUntilExit()
    let msg = String(decoding: err.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        .trimmingCharacters(in: .whitespacesAndNewlines)
    let ok = p.terminationStatus == 0 && FileManager.default.fileExists(atPath: path)
    return (ok, ok ? "" : (msg.isEmpty ? "screencapture exit \(p.terminationStatus)" : msg))
}

// MARK: - pixels

struct Image {
    let w: Int, h: Int
    let rgba: [UInt8]

    static func load(_ path: String) -> Image? {
        guard let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
              let img = CGImageSourceCreateImageAtIndex(src, 0, nil) else { return nil }
        return Image(cg: img, w: img.width, h: img.height)
    }

    /// Draw `cg` into a w x h RGBA8 buffer (scaling if the sizes differ — used by diff to compare
    /// two captures of different pixel size on a common grid).
    init(cg: CGImage, w: Int, h: Int) {
        self.w = w; self.h = h
        var buf = [UInt8](repeating: 0, count: w * h * 4)
        buf.withUnsafeMutableBytes { raw in
            guard let ctx = CGContext(data: raw.baseAddress, width: w, height: h, bitsPerComponent: 8,
                                      bytesPerRow: w * 4, space: CGColorSpaceCreateDeviceRGB(),
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else {
                die("could not create bitmap context")
            }
            ctx.interpolationQuality = .medium
            ctx.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
        }
        rgba = buf
    }

    @inline(__always) func luma(_ x: Int, _ y: Int) -> Double {
        let i = (y * w + x) * 4
        return 0.299 * Double(rgba[i]) + 0.587 * Double(rgba[i + 1]) + 0.114 * Double(rgba[i + 2])
    }
}

/// The pixel region to score: `--rect` in source coords mapped through `--src` onto the image
/// below `--top`. Without `--rect` the whole image below `--top`.
func region(for img: Image, flags: [String: String]) -> Rect {
    let top = Int(flags["top"] ?? "0") ?? 0
    let availH = img.h - top
    guard availH > 0 else { die("--top \(top) leaves no image") }
    guard let rs = flags["rect"] else { return Rect(x: 0, y: top, w: img.w, h: availH) }
    guard let r = parseRect(rs) else { die("bad --rect '\(rs)' (want WxH@X,Y)") }
    guard let ss = flags["src"], let s = parseRect(ss) else { die("--rect needs --src WxH") }
    let sx = Double(img.w) / Double(s.w), sy = Double(availH) / Double(s.h)
    var x = Int((Double(r.x) * sx).rounded()), y = top + Int((Double(r.y) * sy).rounded())
    var w = Int((Double(r.w) * sx).rounded()), h = Int((Double(r.h) * sy).rounded())
    x = max(0, min(img.w - 1, x)); y = max(top, min(img.h - 1, y))
    w = max(1, min(img.w - x, w)); h = max(1, min(img.h - y, h))
    return Rect(x: x, y: y, w: w, h: h)
}

func stats(_ img: Image, _ r: Rect) -> [String: Any] {
    var sum = 0.0, sumSq = 0.0
    var dark = 0, bright = 0
    for y in r.y..<(r.y + r.h) {
        for x in r.x..<(r.x + r.w) {
            let l = img.luma(x, y)
            sum += l; sumSq += l * l
            if l < 16 { dark += 1 } else if l > 200 { bright += 1 }
        }
    }
    let n = Double(r.w * r.h)
    let mean = sum / n
    let variance = max(0, sumSq / n - mean * mean)
    func r3(_ v: Double) -> Double { (v * 1000).rounded() / 1000 }
    return ["meanLuma": r3(mean), "stdLuma": r3(variance.squareRoot()),
            "darkFrac": r3(Double(dark) / n), "brightFrac": r3(Double(bright) / n),
            "pixels": r.w * r.h]
}

func regionJSON(_ r: Rect) -> [String: Any] { ["x": r.x, "y": r.y, "w": r.w, "h": r.h] }

// MARK: - commands

let argv = Array(CommandLine.arguments.dropFirst())
guard let cmd = argv.first else {
    die("usage: winshot list <pid> | shot <pid> <out.png> [--id N|--title SUB] | "
        + "stats <png> [--rect WxH@X,Y --src WxH] [--top PX] | diff <a.png> <b.png> [same]")
}
let (pos, flags) = splitArgs(Array(argv.dropFirst()))

switch cmd {
case "list":
    guard let pid = pos.first.flatMap({ Int32($0) }) else { die("list <pid>") }
    jsonOut(windows(ownedBy: pid).map { $0.json })

case "shot":
    guard pos.count >= 2, let pid = Int32(pos[0]) else { die("shot <pid> <out.png> [--id N|--title SUB]") }
    let out = pos[1]
    let all = windows(ownedBy: pid)
    var pick: WinInfo?
    if let ids = flags["id"], let id = Int(ids) {
        pick = all.first { $0.id == id }
        if pick == nil { jsonOut(["ok": false, "error": "pid \(pid) owns no window id \(id)"]); exit(1) }
    } else if let sub = flags["title"] {
        pick = all.filter { $0.title.localizedCaseInsensitiveContains(sub) }
                  .max { $0.bounds.width * $0.bounds.height < $1.bounds.width * $1.bounds.height }
        if pick == nil { jsonOut(["ok": false, "error": "no window of pid \(pid) titled like '\(sub)'"]); exit(1) }
    } else {
        pick = all.filter { $0.layer == 0 && $0.onscreen && $0.alpha > 0 }
                  .max { $0.bounds.width * $0.bounds.height < $1.bounds.width * $1.bounds.height }
        if pick == nil { jsonOut(["ok": false, "error": "pid \(pid) has no on-screen window"]); exit(1) }
    }
    let w = pick!
    let (ok, err) = capture(windowID: w.id, to: out)
    var res: [String: Any] = ["ok": ok, "path": out, "id": w.id, "title": w.title,
                              "onscreen": w.onscreen, "bounds": w.json["bounds"]!]
    if ok, let img = Image.load(out) { res["width"] = img.w; res["height"] = img.h }
    if !ok { res["error"] = err }
    jsonOut(res)
    exit(ok ? 0 : 1)

case "stats":
    guard let path = pos.first else { die("stats <png> [--rect WxH@X,Y --src WxH] [--top PX]") }
    guard let img = Image.load(path) else { die("could not load \(path)") }
    let r = region(for: img, flags: flags)
    var out = stats(img, r)
    out["width"] = img.w; out["height"] = img.h; out["region"] = regionJSON(r); out["path"] = path
    jsonOut(out)

case "diff":
    guard pos.count >= 2 else { die("diff <a.png> <b.png> [--rect WxH@X,Y --src WxH] [--top PX]") }
    guard let a = Image.load(pos[0]) else { die("could not load \(pos[0])") }
    var b: Image
    var resampled = false
    if let bl = Image.load(pos[1]) {
        if bl.w == a.w && bl.h == a.h { b = bl }
        else {
            resampled = true
            // Resample B onto A's grid so a 2x window capture and a 1x frame can still be compared.
            guard let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: pos[1]) as CFURL, nil),
                  let cg = CGImageSourceCreateImageAtIndex(src, 0, nil) else { die("could not reload \(pos[1])") }
            b = Image(cg: cg, w: a.w, h: a.h)
        }
    } else { die("could not load \(pos[1])") }
    let r = region(for: a, flags: flags)
    var sum = 0.0; var changed = 0
    for y in r.y..<(r.y + r.h) {
        for x in r.x..<(r.x + r.w) {
            let d = abs(a.luma(x, y) - b.luma(x, y))
            sum += d
            if d > 24 { changed += 1 }
        }
    }
    let n = Double(r.w * r.h)
    jsonOut(["meanAbsDiff": (sum / n * 1000).rounded() / 1000,
             "changedFrac": (Double(changed) / n * 1000).rounded() / 1000,
             "region": regionJSON(r), "resampledB": resampled,
             "a": stats(a, r), "b": stats(b, r)])

default:
    die("unknown command '\(cmd)' — list | shot | stats | diff")
}
