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

/// The app builds its own main menu (`main.swift`) and has no Edit ▸ Find, so ⌘F / ⌘G / ⇧⌘G / ⌘E
/// never reach `performFindPanelAction(_:)` through the menu bar. Route them here, window-wide, so
/// the find bar opens whichever control is focused (filter field, find bar, or the text itself).
private final class BoxLogWindow: NSWindow {
    var findAction: ((NSFindPanelAction) -> Void)?

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if super.performKeyEquivalent(with: event) { return true }
        let mods = event.modifierFlags.intersection([.command, .shift, .control, .option])
        guard let key = event.charactersIgnoringModifiers?.lowercased(), let findAction else { return false }
        let action: NSFindPanelAction
        switch (key, mods) {
        case ("f", [.command]):         action = .showFindPanel
        case ("g", [.command]):         action = .next
        case ("g", [.command, .shift]): action = .previous
        case ("e", [.command]):         action = .setFindString
        default: return false
        }
        findAction(action)
        return true
    }
}

/// Keyboard navigation inside the text (arrows, Page Up/Down, Home/End) moves the viewport
/// without a live-scroll notification; re-evaluate the autoscroll hold after each key.
private final class LogTextView: NSTextView {
    weak var owner: BoxLogWindowController?
    override func keyDown(with event: NSEvent) {
        super.keyDown(with: event)
        owner?.noteUserNavigation()
    }
}

final class BoxLogWindowController: NSWindowController, NSWindowDelegate {
    static let shared = BoxLogWindowController()

    private let textView = LogTextView()
    private let scrollView = NSScrollView()
    private let filterField = NSSearchField()
    private let sourcePopup = NSPopUpButton(frame: .zero, pullsDown: false)
    private let pauseButton = NSButton()
    private let autoscrollButton = NSButton()

    private var paused = false
    // Autoscroll has two axes of state: `autoscroll` is what the next append DOES (follow the tail
    // or leave the viewport alone) and is always what the checkbox shows; `autoscrollHeldByScroll`
    // records that the current OFF was set by the user moving the viewport (scroll, page, find
    // match), not by the checkbox. Only a held OFF resumes when the viewport returns to the bottom
    // — an explicit uncheck stays off until re-checked.
    private var autoscroll = true
    private var autoscrollHeldByScroll = false
    private var internalMutationDepth = 0
    private var filterText = ""
    private var sourceFilter: String?      // nil = All sources
    private var knownSources: Set<String> = []
    private var hideBackfill = false       // "History" toggle — hide LOG_F_BACKFILL-flagged lines
    private let historyButton = NSButton()
    private let log = Logger(subsystem: "com.carlink.app", category: "BoxLogWindow")

    private static let monoFont = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)

    private convenience init() {
        let win = BoxLogWindow(
            contentRect: NSRect(x: 0, y: 0, width: 920, height: 560),
            styleMask: [.titled, .closable, .resizable, .miniaturizable, .fullSizeContentView],
            backing: .buffered, defer: false)
        win.title = "Box Log"
        win.isReleasedWhenClosed = false
        self.init(window: win)
        win.delegate = self
        win.findAction = { [weak self] action in self?.performFind(action) }
        textView.owner = self
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

        // "feed" on the label: this pauses INGESTION into the view, a different axis from autoscroll.
        pauseButton.title = "Pause feed"
        pauseButton.bezelStyle = .rounded
        pauseButton.target = self
        pauseButton.action = #selector(togglePause)

        autoscrollButton.setButtonType(.switch)
        autoscrollButton.title = "Autoscroll"
        autoscrollButton.target = self
        autoscrollButton.action = #selector(toggleAutoscroll)
        autoscrollButton.state = .on

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

        let bar = NSStackView(views: [filterField, sourcePopup, pauseButton, autoscrollButton, historyButton, exportButton, revealButton])
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
        // Find WITHIN the log (⌘F, ⌘G/⇧⌘G, match highlighting + count) is the stock NSTextView find
        // bar. The toolbar's "Filter…" field is a different question — it HIDES non-matching lines —
        // and the find bar searches whatever the filter left visible.
        textView.usesFindBar = true
        textView.isIncrementalSearchingEnabled = true

        scrollView.documentView = textView
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.findBarPosition = .aboveContent
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        let nc = NotificationCenter.default
        nc.addObserver(self, selector: #selector(liveScrolled), name: NSScrollView.didLiveScrollNotification, object: scrollView)
        nc.addObserver(self, selector: #selector(liveScrolled), name: NSScrollView.didEndLiveScrollNotification, object: scrollView)
        nc.addObserver(self, selector: #selector(selectionChanged), name: NSTextView.didChangeSelectionNotification, object: textView)

        content.addSubview(bar)
        content.addSubview(scrollView)
        NSLayoutConstraint.activate([
            // `.fullSizeContentView` runs the content view under the title bar; pin the toolbar row
            // to the content layout guide so it starts below it instead of underneath it.
            bar.topAnchor.constraint(equalTo: (win.contentLayoutGuide as? NSLayoutGuide)?.topAnchor ?? content.topAnchor),
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
        let clip = scrollView.contentView
        let savedOrigin = clip.bounds.origin
        withInternalMutation {
            textView.string = ""
            append(BoxLogStore.shared.snapshot())
            // Autoscroll off: the rebuild emptied the document (which clamps the viewport to the
            // top), so put the viewport back where it was rather than jumping to either end.
            if !autoscroll {
                let target = clip.constrainBoundsRect(NSRect(origin: savedOrigin, size: clip.bounds.size)).origin
                clip.scroll(to: target)
                scrollView.reflectScrolledClipView(clip)
            }
        }
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
        withInternalMutation {
            textView.textStorage?.append(addedText)
            if autoscroll { scrollToEnd() }
        }
    }

    // MARK: - Autoscroll

    private func scrollToEnd() {
        withInternalMutation { textView.scrollToEndOfDocument(nil) }
    }

    /// Selection changes the window causes itself (a rebuild, an append shifting the caret) must
    /// not read as the user navigating; `selectionChanged` ignores notifications posted inside.
    private func withInternalMutation(_ body: () -> Void) {
        internalMutationDepth += 1
        body()
        internalMutationDepth -= 1
    }

    /// Within about two lines of the end of the document.
    private var isAtBottom: Bool {
        scrollView.contentView.bounds.maxY >= textView.frame.maxY - 24
    }

    /// The user moved the viewport (wheel/trackpad/scroller — `NSScrollView` live-scroll
    /// notifications are user-initiated by definition, so a layout-driven origin adjustment never
    /// lands here — or a keyboard/find navigation, see `LogTextView`). Away from the bottom, a
    /// follow becomes a HELD off; back at the bottom, only a held off resumes.
    fileprivate func noteUserNavigation() {
        if autoscroll {
            if !isAtBottom { setAutoscroll(false, heldByScroll: true) }
        } else if autoscrollHeldByScroll, isAtBottom {
            setAutoscroll(true, heldByScroll: false)
        }
    }

    @objc private func liveScrolled(_ note: Notification) { noteUserNavigation() }

    /// A non-empty selection appearing away from the bottom is a find-bar match (or a drag-select)
    /// the user wants to read; caret-only changes carry no viewport intent and are ignored.
    @objc private func selectionChanged(_ note: Notification) {
        guard internalMutationDepth == 0, textView.selectedRange().length > 0 else { return }
        noteUserNavigation()
    }

    private func setAutoscroll(_ on: Bool, heldByScroll: Bool) {
        autoscroll = on
        autoscrollHeldByScroll = !on && heldByScroll
        autoscrollButton.state = on ? .on : .off
    }

    @objc private func toggleAutoscroll() {
        let on = autoscrollButton.state == .on
        setAutoscroll(on, heldByScroll: false)
        if on { scrollToEnd() }
    }

    // MARK: - Find

    private func performFind(_ action: NSFindPanelAction) {
        // `performFindPanelAction(_:)` reads the action from the sender's tag, as the Edit ▸ Find
        // menu items would carry it.
        let sender = NSMenuItem()
        sender.tag = Int(action.rawValue)
        textView.performFindPanelAction(sender)
        // Opening the bar is intent to read: hold the tail-follow before the incremental search
        // starts scrolling to matches (that scroll is the finder's own and posts nothing we can
        // observe). Held, not off — scrolling back to the bottom resumes as usual.
        if action == .showFindPanel, autoscroll { setAutoscroll(false, heldByScroll: true) }
        else { noteUserNavigation() }
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
        pauseButton.title = paused ? "Resume feed" : "Pause feed"
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
