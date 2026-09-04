// BoxLogWindow.swift — Window ▸ Box Log: a live tail of the box's universal log stream (CH_LOG),
// combined with the app's own log via BoxLogStore.
//
// Plain AppKit (NSWindowController + NSTextView), not SwiftUI — mirrors AdapterInfoWindow's
// NSTextView-in-NSScrollView presentation rather than MetadataWindow's SwiftUI host: a 20,000-line
// ring re-rendered through SwiftUI's List diffing on every append would be far slower than appending
// text runs to an NSTextStorage, and this window is a pure live tail with no per-row structure to
// diff against.

import AppKit
import Foundation
import os

final class BoxLogWindowController: NSWindowController, NSWindowDelegate {
    static let shared = BoxLogWindowController()

    private let textView = NSTextView()
    private let scrollView = NSScrollView()
    private let filterField = NSSearchField()
    private let sourcePopup = NSPopUpButton(frame: .zero, pullsDown: false)
    private let pauseButton = NSButton()

    private var paused = false
    private var filterText = ""
    private var sourceFilter: String?      // nil = All sources
    private var knownSources: Set<String> = []
    private var hideBackfill = false       // "History" toggle — hide LOG_F_BACKFILL-flagged lines
    private let historyButton = NSButton()
    private let log = Logger(subsystem: "com.carlink.app", category: "BoxLogWindow")

    private static let monoFont = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)

    private convenience init() {
        let win = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 920, height: 560),
            styleMask: [.titled, .closable, .resizable, .miniaturizable, .fullSizeContentView],
            backing: .buffered, defer: false)
        win.title = "Box Log"
        win.isReleasedWhenClosed = false
        self.init(window: win)
        win.delegate = self
        buildUI(in: win)
    }

    private func buildUI(in win: NSWindow) {
        guard let content = win.contentView else { return }

        filterField.placeholderString = "Filter…"
        filterField.target = self
        filterField.action = #selector(filterChanged)

        sourcePopup.addItem(withTitle: "All sources")
        sourcePopup.target = self
        sourcePopup.action = #selector(sourceChanged)

        pauseButton.title = "Pause"
        pauseButton.bezelStyle = .rounded
        pauseButton.target = self
        pauseButton.action = #selector(togglePause)

        // Replayed-history lines (LOG_F_BACKFILL — the box's enable-time backfill of an existing
        // box.log) render dimmed by default; this hides them outright.
        historyButton.setButtonType(.switch)
        historyButton.title = "Hide history"
        historyButton.target = self
        historyButton.action = #selector(toggleHideBackfill)
        historyButton.state = .off

        let exportButton = NSButton(title: "Export session log…", target: self, action: #selector(exportSessionLog))
        exportButton.bezelStyle = .rounded
        let revealButton = NSButton(title: "Reveal in Finder", target: self, action: #selector(revealInFinder))
        revealButton.bezelStyle = .rounded

        let bar = NSStackView(views: [filterField, sourcePopup, pauseButton, historyButton, exportButton, revealButton])
        bar.orientation = .horizontal
        bar.spacing = 8
        bar.edgeInsets = NSEdgeInsets(top: 8, left: 10, bottom: 8, right: 10)
        bar.translatesAutoresizingMaskIntoConstraints = false
        filterField.widthAnchor.constraint(greaterThanOrEqualToConstant: 220).isActive = true

        textView.isEditable = false
        textView.isSelectable = true
        textView.isRichText = false
        textView.font = Self.monoFont
        textView.textContainerInset = NSSize(width: 6, height: 6)
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.textContainer?.widthTracksTextView = true
        textView.drawsBackground = true
        // Dynamic system colours so the pane follows Light/Dark appearance instead of defaulting to
        // black text on whatever the window background is.
        textView.backgroundColor = .textBackgroundColor
        textView.textColor = .textColor
        textView.insertionPointColor = .textColor

        scrollView.documentView = textView
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.translatesAutoresizingMaskIntoConstraints = false

        content.addSubview(bar)
        content.addSubview(scrollView)
        NSLayoutConstraint.activate([
            bar.topAnchor.constraint(equalTo: content.topAnchor),
            bar.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            bar.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            scrollView.topAnchor.constraint(equalTo: bar.bottomAnchor),
            scrollView.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            scrollView.bottomAnchor.constraint(equalTo: content.bottomAnchor),
        ])
    }

    func show() {
        window?.center()
        window?.makeKeyAndOrderFront(nil)
        BoxLogStore.shared.onAppend = { [weak self] lines in
            DispatchQueue.main.async { self?.append(lines) }
        }
        reload()
    }

    func windowWillClose(_ notification: Notification) {
        BoxLogStore.shared.onAppend = nil
    }

    // MARK: - Content

    /// Rebuild the visible text from the FULL ring under the current filter — used on open and
    /// whenever a filter/source/pause control changes (the ring, unlike the live-append path, is
    /// small enough — capped at 20,000 lines — that a full re-render is cheap and always correct).
    private func reload() {
        textView.string = ""
        append(BoxLogStore.shared.snapshot())
    }

    private func append(_ lines: [BoxLogStore.Line]) {
        guard !paused else { return }
        let addedText = NSMutableAttributedString()
        for line in lines {
            let (outer, inner) = Self.tags(in: line.text)
            for tag in [outer, inner].compactMap({ $0 }) where !knownSources.contains(tag) {
                knownSources.insert(tag)
                sourcePopup.addItem(withTitle: tag)
            }
            guard matches(line) else { continue }
            // Dynamic system colours (never hardcoded, see textView setup above): history renders
            // in the same dimmed tone the rest of the app uses for secondary/subordinate text.
            let color: NSColor = line.isBackfill ? .secondaryLabelColor : .textColor
            addedText.append(NSAttributedString(
                string: line.text + "\n", attributes: [.font: Self.monoFont, .foregroundColor: color]))
        }
        guard addedText.length > 0 else { return }
        textView.textStorage?.append(addedText)
        textView.scrollToEndOfDocument(nil)
    }

    private func matches(_ line: BoxLogStore.Line) -> Bool {
        if hideBackfill && line.isBackfill { return false }
        if let src = sourceFilter {
            let (outer, inner) = Self.tags(in: line.text)
            guard outer == src || inner == src else { return false }
        }
        if !filterText.isEmpty, !line.text.localizedCaseInsensitiveContains(filterText) { return false }
        return true
    }

    /// Two independent source tags a rendered line can carry:
    ///   • OUTER — the CH_LOG `source` id BoxLogStore stamped every line with, e.g.
    ///     `<ts> [box/airplayd] connected` → `"airplayd"`. Always present.
    ///   • INNER — a `source: box` (the universal `/tmp/box.log`) line's OWN leading prefix, e.g.
    ///     `<ts> [box/box] [ocbmd] listening` → `"ocbmd"`. Present only on universal-log lines that
    ///     carry one; a marker line (seq gap / dropped-count) has neither.
    /// The filter popup offers both, and `matches` accepts EITHER matching the selection.
    private static func tags(in line: String) -> (outer: String?, inner: String?) {
        guard let boxRange = line.range(of: "[box/") else { return (nil, nil) }
        let afterOpen = line[boxRange.upperBound...]
        guard let close = afterOpen.firstIndex(of: "]") else { return (nil, nil) }
        let outer = String(afterOpen[afterOpen.startIndex..<close])
        var rest = afterOpen[afterOpen.index(after: close)...]
        while rest.first == " " { rest = rest.dropFirst() }
        guard rest.first == "[", let innerClose = rest.firstIndex(of: "]") else { return (outer, nil) }
        let inner = String(rest[rest.index(after: rest.startIndex)..<innerClose])
        return (outer, inner)
    }

    // MARK: - Controls

    @objc private func filterChanged() {
        filterText = filterField.stringValue
        reload()
    }

    @objc private func sourceChanged() {
        let title = sourcePopup.titleOfSelectedItem ?? "All sources"
        sourceFilter = title == "All sources" ? nil : title
        reload()
    }

    @objc private func toggleHideBackfill() {
        hideBackfill = historyButton.state == .on
        reload()
    }

    @objc private func togglePause() {
        paused.toggle()
        pauseButton.title = paused ? "Resume" : "Pause"
        if !paused { reload() } // catch up on everything buffered while paused
    }

    /// Exports the COMBINED app+box log — FileLogger's own session file, which already contains every
    /// box line via the `[box/<sourceName>]`-prefixed bridge in `BoxLogStore.ingest` — not a box-only file.
    @objc private func exportSessionLog(_ sender: Any?) {
        let panel = NSSavePanel()
        panel.title = "Export Combined Session Log"
        let df = DateFormatter()
        df.dateFormat = "yyyy-MM-dd_HHmmss"
        panel.nameFieldStringValue = "carlink_combined_\(df.string(from: Date())).log"
        panel.allowedContentTypes = [.plainText]
        panel.canCreateDirectories = true
        guard let win = window else { return }
        panel.beginSheetModal(for: win) { [log] response in
            guard response == .OK, let dest = panel.url else { return }
            guard let source = FileLogger.shared.currentLogURL else {
                let alert = NSAlert()
                alert.messageText = "Export Failed"
                alert.informativeText = "No active session log."
                alert.alertStyle = .warning
                alert.runModal()
                return
            }
            DispatchQueue.global(qos: .utility).async {
                FileLogger.shared.flushSync()
                do {
                    let body = try String(contentsOf: source, encoding: .utf8)
                    try body.write(to: dest, atomically: true, encoding: .utf8)
                    log.info("combined log exported: \(dest.path, privacy: .public)")
                } catch {
                    log.error("combined log export failed: \(error.localizedDescription, privacy: .public)")
                }
            }
        }
    }

    @objc private func revealInFinder(_ sender: Any?) {
        let dir = FileLogger.logsDirectory
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        NSWorkspace.shared.open(dir)
    }
}
