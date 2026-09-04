import AppKit

/// Renders the CarLink keyboard-shortcut reference as an NSAlert whose accessory view
/// is a two-column NSGridView: action on the left, HIG-compliant key glyphs on the right.
///
/// Conventions per Apple Human Interface Guidelines (Keyboards):
///   • Modifier glyphs are ⌃ ⌥ ⇧ ⌘.
///   • Modifier order is always Control, Option, Shift, Command.
///   • Upper-character keys stand on their own (⌘? rather than ⇧⌘/).
enum KeyboardShortcutsPresenter {

    // MARK: - Data model

    struct Modifier: OptionSet, Sendable {
        let rawValue: Int
        static let control = Modifier(rawValue: 1 << 0)
        static let option  = Modifier(rawValue: 1 << 1)
        static let shift   = Modifier(rawValue: 1 << 2)
        static let command = Modifier(rawValue: 1 << 3)

        /// Glyphs in the HIG-mandated display order: ⌃⌥⇧⌘.
        var glyphs: String {
            var s = ""
            if contains(.control) { s += "⌃" }
            if contains(.option)  { s += "⌥" }
            if contains(.shift)   { s += "⇧" }
            if contains(.command) { s += "⌘" }
            return s
        }
    }

    /// A shortcut as a single rendered string. Multiple equivalents are comma-separated,
    /// e.g. `⎋, B` for two keys that both invoke Back.
    struct Shortcut {
        let keys: String
        let action: String

        /// Convenience for modifier+key combos so callers don't hand-assemble glyphs.
        static func combo(_ modifiers: Modifier, _ key: String, _ action: String) -> Shortcut {
            Shortcut(keys: modifiers.glyphs + key, action: action)
        }
    }

    struct Section {
        let title: String
        let items: [Shortcut]
    }

    // MARK: - Sections
    //
    // Glyph choices:
    //   ↩  U+21A9  Return  — matches NSMenuItem rendering
    //   ⎋  U+238B  Escape
    //   ←↑→↓        Arrows
    //
    // Modifier glyphs and ordering (⌃⌥⇧⌘) enforced by Modifier.glyphs.
    //
    // Honesty sweep (2026-07-31): only shortcuts that actually work are listed. Removed the dead
    // ⇧⌘R "Start session capture" (no such menu item), the ⏯⏭⏮ hardware media-key halves (no
    // MPRemoteCommandCenter — only the letter keys work), and the two-finger-scroll / pinch trackpad
    // rows (their didMultiTouchTwo sink is an empty no-op over OCBM).
    private static let sections: [Section] = [
        Section(title: "Navigation", items: [
            Shortcut(keys: "←  →  ↑  ↓", action: "D-Pad"),
            Shortcut(keys: "↩",          action: "Select"),
            Shortcut(keys: "⎋,  B",      action: "Back"),
        ]),
        Section(title: "Media", items: [
            Shortcut(keys: "Space", action: "Play / Pause"),
            Shortcut(keys: "N",     action: "Next track"),
            Shortcut(keys: "P",     action: "Previous track"),
            Shortcut(keys: "H",     action: "Home screen"),
        ]),
        Section(title: "Siri", items: [
            Shortcut(keys: "S", action: "Activate Siri"),
        ]),
        Section(title: "Menu", items: [
            .combo([.command, .control], "R", "Reset session"),
            .combo([.command, .shift],   "E", "Export log"),
            .combo([.command, .shift],   "M", "Metadata window"),
            .combo([.command, .shift],   "L", "Box Log window"),
            .combo([.command, .shift],   "K", "Controls window"),
            .combo([.command],           "B", "Control Box"),
            .combo([.command],           "I", "Adapter / Phone Info"),
            .combo([.command],           ",", "Settings…"),
            .combo([.command],           "?", "This help"),
            .combo([.command],           "Q", "Quit"),
            .combo([.command],           "W", "Close window (quits app)"),
            .combo([.command],           "M", "Minimize"),
            .combo([.command],           "H", "Hide"),
        ]),
        Section(title: "Trackpad", items: [
            Shortcut(keys: "Click + Drag", action: "Swipe / scroll"),
        ]),
    ]

    // MARK: - Presentation

    @MainActor
    static func present(on parent: NSWindow?) {
        let alert = NSAlert()
        alert.messageText = "CarLink Keyboard Shortcuts"
        alert.informativeText = ""
        alert.alertStyle = .informational
        alert.addButton(withTitle: "OK")

        alert.accessoryView = buildGrid()

        if let parent {
            alert.beginSheetModal(for: parent, completionHandler: nil)
        } else {
            alert.runModal()
        }
    }

    // MARK: - Grid

    @MainActor
    private static func buildGrid() -> NSView {
        let grid = NSGridView()
        grid.rowSpacing = 4
        grid.columnSpacing = 20

        // Single size and family throughout. 13pt system matches NSAlert body text
        // and keeps action/shortcut columns visually balanced.
        let bodyFont   = NSFont.systemFont(ofSize: 13, weight: .regular)
        let headerFont = NSFont.systemFont(ofSize: 13, weight: .semibold)

        for (index, section) in sections.enumerated() {
            // Spacer row between sections
            if index > 0 {
                let spacer = NSView(frame: NSRect(x: 0, y: 0, width: 1, height: 10))
                let spacerRow = grid.addRow(with: [spacer, NSGridCell.emptyContentView])
                spacerRow.mergeCells(in: NSRange(location: 0, length: 2))
            }

            // Section header
            let header = NSTextField(labelWithString: section.title.uppercased())
            header.font = headerFont
            header.textColor = .secondaryLabelColor
            let headerRow = grid.addRow(with: [header, NSGridCell.emptyContentView])
            headerRow.mergeCells(in: NSRange(location: 0, length: 2))

            // Shortcut rows
            for item in section.items {
                let actionLabel = NSTextField(labelWithString: item.action)
                actionLabel.font = bodyFont
                actionLabel.textColor = .labelColor

                let shortcut = NSTextField(labelWithString: item.keys)
                shortcut.font = bodyFont
                shortcut.textColor = .secondaryLabelColor
                shortcut.alignment = .right

                grid.addRow(with: [actionLabel, shortcut])
            }
        }

        // Alignments per HIG: actions left, shortcuts right
        grid.column(at: 0).xPlacement = .leading
        grid.column(at: 1).xPlacement = .trailing

        // NSAlert uses the accessory view's frame — let the grid compute its natural
        // size, then wrap it in a frame-based container with a small inset.
        let gridFitting = grid.fittingSize
        let hPadding: CGFloat = 12
        let vPadding: CGFloat = 6
        let containerWidth = max(gridFitting.width, 420) + hPadding * 2
        let containerHeight = gridFitting.height + vPadding * 2

        let container = NSView(frame: NSRect(x: 0, y: 0, width: containerWidth, height: containerHeight))
        grid.setFrameOrigin(NSPoint(x: hPadding, y: vPadding))
        grid.setFrameSize(NSSize(width: containerWidth - hPadding * 2, height: gridFitting.height))
        container.addSubview(grid)
        return container
    }
}
