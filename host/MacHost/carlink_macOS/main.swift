import AppKit
import os

private let logger = Logger(subsystem: "com.carlink.app", category: "Main")

// Single instance enforcement — activate the existing instance and exit.
// Bundle ID is read from Info.plist so a rename can't silently break this.
if let bundleID = Bundle.main.bundleIdentifier {
    let runningApps = NSRunningApplication.runningApplications(withBundleIdentifier: bundleID)
    if runningApps.count > 1 {
        logger.warning("Another instance of CarLink is already running. Activating it and exiting.")
        runningApps.first { $0 != NSRunningApplication.current }?.activate()
        exit(1)
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate

// Build a minimal main menu (Cmd+Q, Cmd+W, Cmd+H)
let mainMenu = NSMenu()

let appMenuItem = NSMenuItem()
mainMenu.addItem(appMenuItem)
let appMenu = NSMenu()
appMenu.addItem(withTitle: "About CarLink", action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)), keyEquivalent: "")
appMenu.addItem(NSMenuItem.separator())
appMenu.addItem(withTitle: "Settings…", action: #selector(AppDelegate.showSettings(_:)), keyEquivalent: ",")
appMenu.addItem(NSMenuItem.separator())
appMenu.addItem(withTitle: "Hide CarLink", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h")
appMenu.addItem(withTitle: "Hide Others", action: #selector(NSApplication.hideOtherApplications(_:)), keyEquivalent: "h")
appMenu.items.last?.keyEquivalentModifierMask = [.command, .option]
appMenu.addItem(withTitle: "Show All", action: #selector(NSApplication.unhideAllApplications(_:)), keyEquivalent: "")
appMenu.addItem(NSMenuItem.separator())
appMenu.addItem(withTitle: "Quit CarLink", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
appMenuItem.submenu = appMenu

let fileMenuItem = NSMenuItem()
mainMenu.addItem(fileMenuItem)
let fileMenu = NSMenu(title: "File")
let exportItem = NSMenuItem(title: "Export Log...", action: #selector(AppDelegate.exportLog(_:)), keyEquivalent: "e")
exportItem.keyEquivalentModifierMask = [.command, .shift]
fileMenu.addItem(exportItem)
fileMenuItem.submenu = fileMenu
_ = delegate  // retained by app.delegate; referenced here to keep the binding explicit

// (Capture / Resolution / Navigation menus removed 2026-07-12 — config is host-authoritative YAML,
// pushed at SUBSCRIBE, not driven by menu items. See the config redesign in docs.)

let windowMenuItem = NSMenuItem()
mainMenu.addItem(windowMenuItem)
let windowMenu = NSMenu(title: "Window")
windowMenu.addItem(withTitle: "Minimize", action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m")
windowMenu.addItem(withTitle: "Close", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")
windowMenu.addItem(NSMenuItem.separator())
// Auxiliary windows (user directives 2026-07-12): live metadata viewer + on-screen CarPlay controls.
let metadataItem = NSMenuItem(title: "Metadata", action: #selector(AppDelegate.showMetadataWindow(_:)), keyEquivalent: "M")
windowMenu.addItem(metadataItem)
let controlsItem = NSMenuItem(title: "Controls", action: #selector(AppDelegate.showControlsWindow(_:)), keyEquivalent: "K")
windowMenu.addItem(controlsItem)
let boxLogItem = NSMenuItem(title: "Box Log", action: #selector(AppDelegate.showBoxLogWindow(_:)), keyEquivalent: "L")
windowMenu.addItem(boxLogItem)
let hudItem = NSMenuItem(title: "Control Box", action: #selector(AppDelegate.toggleControlHUD(_:)), keyEquivalent: "b")
windowMenu.addItem(hudItem)
let altVideoItem = NSMenuItem(title: "Alt Video", action: #selector(AppDelegate.showAltVideoWindow(_:)), keyEquivalent: "")
windowMenu.addItem(altVideoItem)
windowMenuItem.submenu = windowMenu
app.windowsMenu = windowMenu

let helpMenuItem = NSMenuItem()
mainMenu.addItem(helpMenuItem)
let helpMenu = NSMenu(title: "Help")
let kbItem = NSMenuItem(title: "Keyboard Shortcuts", action: #selector(AppDelegate.showKeyboardShortcuts(_:)), keyEquivalent: "?")
helpMenu.addItem(kbItem)
let adapterInfoItem = NSMenuItem(title: "Adapter / Phone Info…", action: #selector(AppDelegate.showAdapterInfo(_:)), keyEquivalent: "i")
adapterInfoItem.keyEquivalentModifierMask = [.command]
helpMenu.addItem(adapterInfoItem)
helpMenu.addItem(NSMenuItem.separator())
let resetSessionItem = NSMenuItem(title: "Reset Session", action: #selector(AppDelegate.resetSession(_:)), keyEquivalent: "r")
resetSessionItem.keyEquivalentModifierMask = [.command, .control]   // ⌃⌘R (⇧⌘R is Start Capture)
helpMenu.addItem(resetSessionItem)
helpMenu.addItem(NSMenuItem.separator())
let revealLogsItem = NSMenuItem(title: "Reveal Logs in Finder", action: #selector(AppDelegate.revealLogsInFinder(_:)), keyEquivalent: "")
helpMenu.addItem(revealLogsItem)
helpMenuItem.submenu = helpMenu

app.mainMenu = mainMenu
app.setActivationPolicy(.regular)
app.activate()
app.run()
