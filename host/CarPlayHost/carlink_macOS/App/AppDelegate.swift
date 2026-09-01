import AppKit
import AVFoundation
import os
import Synchronization

/// Application entry point. Creates the window, discovers USB devices, and wires together the OCBM
/// session, video decoders, and audio player.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {

    private nonisolated static let logger = Logger(subsystem: "com.carlink.app", category: "AppDelegate")

    private var windowController: MainWindowController!
    private var usbManager: USBDeviceManager!
    private var transport: USBTransport?
    /// OCBM committed-model host session (replaces `adapter` for the A/V-verification build).
    private var ocbmClient: OCBMClient?
    private var ocbmBridge: OCBMAVBridge?
    private var ocbmCoordinator: OCBMSessionCoordinator?   // task #29: delegate reactions + A/V watchdog
    // App-driven SETUP (plan P3): the host authors the RTSP/SETUP response the phone sees, relayed from
    // the box over CH_RTSP. Both nil unless the pushed config sets `appDrivenSetup` — box-driven otherwise.
    private var controlRelay: OCBMControlRelay?
    private var setupSession: AirPlaySetupSession?
    private var mainDecoder: VideoDecoder?
    /// Dedicated decoder for the ALT / navigation (instrument-cluster) video stream.
    private var altDecoder: VideoDecoder?
    /// Live AA head-unit session (AA_CONNECT / mode-event driven) — retained for its lifetime.
    private var aaSession: AASession?
    /// The CarPlay decoder + video layer parked while an AA session owns the window. AA installs its
    /// OWN decoder into `carPlayView`, so without this a box mode change back to CarPlay would render
    /// into an orphaned layer. Non-nil exactly while `aaSession` is live over OCBM.
    private var parkedCarPlayDecoder: VideoDecoder?
    private var parkedCarPlayLayer: AVSampleBufferDisplayLayer?
    /// Bumped by every AA start AND every AA stop. `AASession` is built on a background queue, so
    /// `aaSession` is assigned some milliseconds AFTER the start — a stop landing in that window
    /// (box re-arbitrates, phone yanked) would otherwise find `aaSession` nil, skip `shutdown()`,
    /// and then have the late session assign itself over the cleared state: a zombie that never
    /// sends BYEBYE and permanently fails the `aaSession == nil` guard on the next start. The
    /// background block re-checks this generation before adopting the session.
    private var aaGeneration: Int = 0
    /// The CH_IP transport of the current (or still-being-built) AA session. Tracked SEPARATELY from
    /// `aaSession` because the stream is opened synchronously at start while the session that owns it
    /// is built on a background queue: a stop in that window has to close the stream itself, or the
    /// box keeps relaying a conn nobody reads.
    private var aaTransport: AAOCBMTransport?
    private var audioPlayer: AudioPlayer?
    private var controlServer: ControlServer?
    private var micCapture: MicCapture?
    // Wireless SSP Numeric-Comparison pairing: the live 6-digit code (nil = none) + the last watchdog
    // status, so the code can take priority over "Waiting for phone…" while pairing and restore after.
    private var activePairingCode: String?
    private var lastCoordStatus: String = ""

    /// Consecutive USB-claim failures for the currently attached adapter;
    /// bounds the automatic re-scan retries in setupDevice.
    private var claimRetryCount = 0

    /// Monotonic USB hotplug-event generation (audit A1). Bumped synchronously in each IOKit
    /// connect/disconnect callback (delivered in order on the manager's serial notifyQueue), so a
    /// disconnect Task can tell whether a NEWER event (a fast replug's connect) has already superseded
    /// it — preventing a stale `endSession` from tearing down a freshly-built session when the two
    /// unstructured `Task { @MainActor }` hops run out of order. A `Mutex` because it is read/written from
    /// the nonisolated callbacks AND the main actor.
    private let usbEventGen = Mutex<UInt64>(0)

    /// Highest `CT_PROJ_MODE` sequence already applied. OCBMClient stamps each mode event on its
    /// serial read queue (the box's emission order); each event is applied on the main actor via its
    /// own `Task` hop, and two hops can land out of order — the `usbEventGen` hazard. Without this
    /// the loser would win: a stale `pmWiredAa` applied after a newer `pmNone` would park CarPlay and
    /// open a CH_IP stream to a bridge the box has already torn down. ocbmd does NOT throttle its
    /// first emission after a SUBSCRIBE reset, so back-to-back modes are reachable in practice.
    private var lastProjModeSeq: UInt64 = 0
    /// Diagnostic lever — see the touch handler.
    private static let edgeClampBug = ProcessInfo.processInfo.environment["AA_EDGE_CLAMP_BUG"] == "1"

    nonisolated static let logFileURL: URL = {
        let tmp = FileManager.default.temporaryDirectory
        return tmp.appendingPathComponent("carlink_macOS.log")
    }()
    private var logFileHandle: FileHandle?

    // MARK: - App Lifecycle

    func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool { true }

    func applicationDidFinishLaunching(_ notification: Notification) {
        setupFileLogging()
        FileLogger.shared.start()
        Self.logger.info("CarLink macOS starting...")
        // Debug control server (off unless CARLINK_CTRL_PORT is set). At LAUNCH, not on device
        // attach: it must be reachable with no adapter present, and its whole point is to be
        // available before anyone starts pressing things. See ControlServer for why it exists.
        if let ctrl = ControlServer() { ctrl.start(); self.controlServer = ctrl }
        // Route scripted taps through the real delegate entry point, so injected touches take the
        // identical path (and identical coordinate scaling) as a tap from the trackpad.
        ControlsBridge.shared.injectTouch = { [weak self] action, x, y in
            guard let self else { return }
            self.carPlayView(self.windowController.carPlayView, didTouch: action, x: x, y: y)
        }
        if let url = FileLogger.shared.currentLogURL {
            Self.logger.info("Session log: \(url.path, privacy: .public)")
        }

        // Create window
        windowController = MainWindowController()
        windowController.window?.delegate = self
        windowController.showWindow(nil)
        windowController.window?.makeKeyAndOrderFront(nil)
        // The borderless video window's controls live in its attached floating control box, created by
        // the window controller as a child window that moves with the video.

        // AA visual-proof player (docs/host/02_ANDROID_AUTO.md Phase 1): AA_PLAY=/path.h264 plays a captured
        // Android Auto stream through the real decoder+view, no box/phone. Skips USB.
        if let aaPath = ProcessInfo.processInfo.environment["AA_PLAY"] {
            let decoder = VideoDecoder()
            decoder.label = "aa-debug"
            self.mainDecoder = decoder
            windowController.window?.title = "CarLink — AA debug player"
            AADebugPlayer.start(path: aaPath, view: windowController.carPlayView, decoder: decoder)
            Self.logger.info("AA debug player: \(aaPath, privacy: .public)")
            return
        }

        // AA LIVE head-unit session (docs/host/02_ANDROID_AUTO.md Phase 1): AA_CONNECT=host:port runs the
        // full AA engine (TLS+GAL+demux) against the phone's head-unit server (adb
        // forward), rendering into the real decoder/view. Skips USB.
        if let aaConn = ProcessInfo.processInfo.environment["AA_CONNECT"] {
            startAALiveSession(target: aaConn)
            return
        }

        // Reset USB devices from any stale state, then start scanning
        usbManager = USBDeviceManager()
        usbManager.delegate = self
        let mgr = usbManager!
        DispatchQueue.global(qos: .userInitiated).async {
            mgr.resetDevicesOnLaunch()
            mgr.startScanning()
        }

        Self.logger.info("Waiting for CPC200-CCPA adapter...")
    }

    // Note: windowWillClose terminates the app when the main window closes,
    // which also covers the "last window" case — a separate
    // applicationShouldTerminateAfterLastWindowClosed would be unreachable.

    func applicationWillTerminate(_ notification: Notification) {
        aaSession?.shutdown()   // clean AA BYEBYE so the phone doesn't retain a stale session
        teardown()
        // STDERR_FILENO was dup2'd onto logFileHandle's descriptor at launch. Point stderr at
        // /dev/null BEFORE closing the handle, or late teardown writes (framework NSLog etc.) land
        // on whatever recycles that fd number.
        redirectStderrToDevNull()
        try? logFileHandle?.close()
        // FileLogger.stop() drains OSLogStore synchronously — under a large backlog that can take
        // seconds. Run it on a background queue and bound the wait so quit proceeds within ~2 s
        // either way (a truncated final batch beats a beachballed quit).
        let drained = DispatchSemaphore(value: 0)
        DispatchQueue.global(qos: .utility).async {
            FileLogger.shared.stop()
            drained.signal()
        }
        _ = drained.wait(timeout: .now() + 2.0)
    }

    /// Repoint stderr at /dev/null. Called at quit before closing the log file handle that
    /// STDERR_FILENO was dup2'd onto (see setupFileLogging), so stderr never aliases a recycled fd.
    private func redirectStderrToDevNull() {
        let devnull = open("/dev/null", O_WRONLY)
        guard devnull >= 0 else { return }
        dup2(devnull, STDERR_FILENO)
        close(devnull)
    }

    /// Start a live AA head-unit session against `host:port` (from AA_CONNECT), rendering
    /// into the main window's decoder/view. Runs the blocking session on a background thread.
    private func startAALiveSession(target: String) {
        let parts = target.split(separator: ":")
        let host = parts.count >= 2 ? String(parts[0]) : "127.0.0.1"
        let port = UInt16(parts.count >= 2 ? String(parts[1]) : target) ?? 5277
        let p12 = ProcessInfo.processInfo.environment["AA_P12"]
            ?? NSHomeDirectory() + "/Documents/carlink/ccpa_custom/host/aa-headunit/certs/headunit.p12"
        let password = ProcessInfo.processInfo.environment["AA_P12_PASS"] ?? "carlink"

        let decoder = VideoDecoder()
        decoder.label = "aa-live"
        self.mainDecoder = decoder
        let view = windowController.carPlayView
        view.videoLayer.removeFromSuperlayer()
        view.videoLayer = decoder.displayLayer
        decoder.displayLayer.videoGravity = .resizeAspect
        decoder.displayLayer.frame = view.bounds
        view.layer?.addSublayer(decoder.displayLayer)
        view.isAndroidAuto = true
        view.delegate = self   // route mouse->touch to the AA input channel
        applyAAVideoSize(width: 800, height: 480) // initial; refined from the decoder below
        windowController.window?.title = "CarLink — Android Auto (live)"
        // Drive the window/video geometry from the ACTUAL decoded resolution (not a
        // hardcoded tier), so AA's negotiated video size scales correctly — the same
        // onDimensions auto-sizing the CarPlay/cluster path uses.
        decoder.onDimensions = { [weak self] w, h in
            DispatchQueue.main.async { self?.applyAAVideoSize(width: w, height: h) }
        }

        // Snapshot the vehicle profile HERE, on the main actor, and hand the value to the session
        // thread — the AA engine must not reach into the observable model from its own thread.
        let cap = AACapability(config: VehicleConfigModel.shared)
        let player = self.audioPlayer   // AA's three sinks play through the same engine as CarPlay
        DispatchQueue.global(qos: .userInitiated).async {
            guard let transport = AATCPTransport(host: host, port: port) else {
                NSLog("[AA] TCP connect to \(host):\(port) failed"); return
            }
            guard let session = AASession(transport: transport, decoder: decoder,
                                          p12Path: p12, p12Password: password,
                                          capability: cap, audio: player,
                                          log: { NSLog("[AA] \($0)") }) else {
                NSLog("[AA] session init failed (p12?)"); return
            }
            DispatchQueue.main.async {
                self.aaSession = session
                ControlsBridge.shared.aaSession = session
                ControlsBridge.shared.isAndroidAuto = true
            }
            session.run()
            // run() returns when the session ends. Hand the Controls window back rather than leaving
            // `isAndroidAuto` latched: a stale true routes every control at a dead AA session, and
            // disables the decoder's keyframe recovery for whatever runs next. The OCBM path clears
            // this in stopAAOverOCBM; this debug path had no equivalent.
            DispatchQueue.main.async {
                guard self.aaSession === session else { return }   // a newer session already took over
                self.aaSession = nil
                ControlsBridge.shared.aaSession = nil
                ControlsBridge.shared.isAndroidAuto = false
                NSLog("[AA] TCP session ended — Controls handed back")
            }
        }
        Self.logger.info("AA live session -> \(host):\(port, privacy: .public)")
    }

    /// AA over the OCBM link (§4e): the app is already OCBM-connected + SUBSCRIBEd (which drives the box
    /// session_supervisor to arm_aa → launch aa-bridge). This opens a CH_IP stream to the box aa-bridge
    /// and runs the SAME AASession engine over `AAOCBMTransport` instead of a TCP socket — Android Auto
    /// on the same USB channel as CarPlay. Normally driven by the box's `ctProjMode` mode event
    /// (`pmWiredAa`); `AA_OCBM=1` (+ optional `AA_OCBM_TARGET`, default the box aa-bridge loopback)
    /// forces it for a box whose ocbmd predates that event.
    private func startAAOverOCBM(client: OCBMClient, target: String) {
        guard aaSession == nil, parkedCarPlayDecoder == nil else {
            Self.logger.info("AA-over-OCBM already running — ignoring duplicate start")
            return
        }
        let p12 = ProcessInfo.processInfo.environment["AA_P12"]
            ?? NSHomeDirectory() + "/Documents/carlink/ccpa_custom/host/aa-headunit/certs/headunit.p12"
        let password = ProcessInfo.processInfo.environment["AA_P12_PASS"] ?? "carlink"

        let decoder = VideoDecoder()
        decoder.label = "aa-ocbm"
        let view = windowController.carPlayView
        // Park the CarPlay decoder/layer rather than dropping it: the box can hand the session back
        // (Android unplugged → iPhone plugged → pmWiredCp), and the OCBM A/V bridge still feeds the
        // parked decoder. `stopAAOverOCBM` puts it back on screen.
        parkedCarPlayDecoder = mainDecoder
        parkedCarPlayLayer = view.videoLayer
        self.mainDecoder = decoder
        view.videoLayer.removeFromSuperlayer()
        view.videoLayer = decoder.displayLayer
        decoder.displayLayer.videoGravity = .resizeAspect
        decoder.displayLayer.frame = view.bounds
        view.layer?.addSublayer(decoder.displayLayer)
        view.isAndroidAuto = true
        view.delegate = self
        applyAAVideoSize(width: 800, height: 480)
        windowController.window?.title = "CarLink — Android Auto (OCBM)"
        decoder.onDimensions = { [weak self] w, h in
            DispatchQueue.main.async { self?.applyAAVideoSize(width: w, height: h) }
        }

        aaGeneration &+= 1
        let gen = aaGeneration
        // Distinct CH_IP conn id per attempt. With a fixed id, a superseded session's late
        // `ipClose` would tear down the stream a NEWER session had already opened under the same id.
        let transport = AAOCBMTransport(client: client,
                                        connId: 0x00AA &+ UInt16(truncatingIfNeeded: gen & 0x3F),
                                        target: target)
        // Mic source: the phone opens channel 9 on an Assistant tap and waits for audio. Reuse the
        // CarPlay capture engine at AA's advertised 16 kHz mono.
        let aaMic = MicCapture()
        self.micCapture = aaMic
        aaTransport = transport
        let cap = AACapability(config: VehicleConfigModel.shared)   // main-actor snapshot; see above
        let player = self.audioPlayer   // AA's three sinks play through the same engine as CarPlay
        DispatchQueue.global(qos: .userInitiated).async {
            guard let session = AASession(transport: transport, decoder: decoder,
                                          p12Path: p12, p12Password: password,
                                          capability: cap, audio: player,
                                          log: { NSLog("[AA] \($0)") }) else {
                NSLog("[AA] OCBM session init failed (p12? \(p12))")
                transport.close()   // the CH_IP stream is already open — don't leak it
                DispatchQueue.main.async {
                    // Give the window back to CarPlay. Without this the parked state stays set, and
                    // the re-entry guard above silently swallows EVERY later start — one missing p12
                    // would disable Android Auto for the life of the app. Skip if a newer start has
                    // since taken ownership of that state.
                    guard self.aaGeneration == gen else { return }
                    self.stopAAOverOCBM(reason: "AA session init failed")
                }
                return
            }
            DispatchQueue.main.async {
                guard self.aaGeneration == gen else {
                    // A stop ran while this session was being built. Never adopt it — shut it down
                    // (sends BYEBYE + closes the CH_IP stream) and leave the cleared state alone.
                    NSLog("[AA] session start superseded — shutting it down without adopting")
                    DispatchQueue.global(qos: .utility).async { session.shutdown() }
                    return
                }
                session.onMicStart = { [weak session] rate, ch in
                    // A denied/busy mic otherwise fails silently inside MicCapture and the phone's
                    // Assistant waits forever for audio that will never come. Say so plainly.
                    let auth = AVCaptureDevice.authorizationStatus(for: .audio)
                    if auth != .authorized {
                        NSLog("[AA] MIC NOT AUTHORIZED (status=\(auth.rawValue)) — Assistant will hear silence")
                    }
                    aaMic.onPCMData = { [weak session] pcm in session?.sendMicPCM(pcm) }
                    aaMic.startCapture(sampleRate: rate, channels: ch)
                }
                session.onMicStop = { aaMic.stopCapture(); aaMic.onPCMData = nil }
                self.aaSession = session
                // The Controls window routes its intents per protocol (see ControlsBridge): point it
                // at this session so Play/Home/Back/D-Pad become AA key events instead of silently
                // going to a CarPlay client that does not own the box.
                ControlsBridge.shared.aaSession = session
                ControlsBridge.shared.isAndroidAuto = true
                DispatchQueue.global(qos: .userInitiated).async {
                    session.run()
                    // run() returned on its OWN (stream ended, protocol error) — nothing else clears
                    // the session, so without this the next pmWiredAa is swallowed by the re-entry
                    // guard and AA stays dark until the box happens to emit pmNone first.
                    DispatchQueue.main.async {
                        guard self.aaGeneration == gen else { return }
                        self.stopAAOverOCBM(reason: "AA session ended on its own")
                    }
                }
            }
        }
        Self.logger.info("AA-over-OCBM session -> CH_IP \(target, privacy: .public)")
    }

    /// Tear the AA-over-OCBM session down and give the window back to CarPlay — the box's mode event
    /// left `pmWiredAa` (Android unplugged, or an iPhone took the box). `shutdown()` sends the AA
    /// BYEBYE, without which the phone keeps the stale session and the next connect hits
    /// `IllegalStateException: Already connected` (docs/host/02_ANDROID_AUTO.md Phase 1).
    /// `drainTimeout`: how long to wait for the AA BYEBYE to reach the wire. Mid-session (a box mode
    /// change) this runs on the main actor with a live UI, so it stays short; at teardown/quit the UI
    /// is going away and a single USB write can legitimately take up to ~1.5 s to fail, so give it
    /// room — a lost BYEBYE leaves the phone holding a stale session that breaks the NEXT attach.
    private func stopAAOverOCBM(reason: String, drainTimeout: TimeInterval = 0.5) {
        guard aaSession != nil || parkedCarPlayDecoder != nil else { return }
        // Supersede any session still being built on the background queue (see `aaGeneration`).
        aaGeneration &+= 1
        aaSession?.shutdown()
        aaSession = nil
        // Hand the Controls window back to CarPlay. Leaving `isAndroidAuto` set would strand every
        // button on a dead AA session — they would report "cannot express this" instead of reaching
        // the CarPlay client that now owns the box.
        ControlsBridge.shared.aaSession = nil
        ControlsBridge.shared.isAndroidAuto = false
        // Close the stream directly as well: if the session is still being built, `shutdown()` above
        // was a no-op and this is the only thing that closes it. Closing twice is harmless.
        aaTransport?.close()
        aaTransport = nil
        // Stop the mic HERE, not only in endSession: this is the box's normal way of ending AA (phone
        // unplugged, iPhone took the port, re-arbitration). A phone that vanishes mid-utterance never
        // sends MediaStop on channel 9, so without this the capture engine keeps running — a live
        // microphone with nowhere to send audio, until the app quits.
        micCapture?.stopCapture()
        micCapture?.onPCMData = nil
        // shutdown() queues the BYEBYE fire-and-forget on the client's CH_IP write queue; wait (with a
        // bound — see drainIpWrites) for it to reach the wire, because the caller may stop the USB
        // transport immediately after.
        ocbmClient?.drainIpWrites(timeout: drainTimeout)
        mainDecoder?.flush()
        let view = windowController.carPlayView
        view.isAndroidAuto = false
        view.clearAndroidAutoVideoMode()
        if let parked = parkedCarPlayLayer {
            view.videoLayer.removeFromSuperlayer()
            view.videoLayer = parked
            parked.frame = view.bounds
            view.layer?.addSublayer(parked)
        }
        mainDecoder = parkedCarPlayDecoder
        parkedCarPlayDecoder = nil
        parkedCarPlayLayer = nil
        // Restore the CarPlay window geometry the AA 800x480 aspect lock replaced.
        let committed = VehicleConfigModel.persistedMainResolution()
        windowController.window?.contentAspectRatio = NSSize(width: committed.width, height: committed.height)
        windowController.applyResolution(width: CGFloat(committed.width), height: CGFloat(committed.height))
        windowController.window?.title = "CarLink"
        Self.logger.info("AA-over-OCBM stopped — \(reason, privacy: .public)")
    }

    /// Size the AA window/video to the actual decoded resolution: lock the window's
    /// content aspect so the video is displayed 1:1 (no fill-crop, no letterbox), and
    /// tell CarPlayView the video aspect so its AA crop/pillarbox math is correct.
    private func applyAAVideoSize(width: Int, height: Int) {
        guard width > 0, height > 0 else { return }
        windowController.carPlayView.updateVideoAspect(width: CGFloat(width), height: CGFloat(height))
        if let win = windowController.window {
            win.contentAspectRatio = NSSize(width: width, height: height)
            var f = win.frame
            f.size.height = f.size.width * CGFloat(height) / CGFloat(width)
            win.setFrame(f, display: true, animate: false)
        }
    }

    // MARK: - File Logging

    private func setupFileLogging() {
        let url = AppDelegate.logFileURL
        // Create/truncate log file
        FileManager.default.createFile(atPath: url.path, contents: nil)
        logFileHandle = FileHandle(forWritingAtPath: url.path)
        _ = try? logFileHandle?.seekToEnd()

        // Redirect stderr to the log file so NSLog output is captured
        let fd = logFileHandle?.fileDescriptor ?? -1
        if fd >= 0 {
            // Duplicate stderr to the log file — NSLog writes to stderr
            dup2(fd, STDERR_FILENO)
        }

        Self.logger.info("Log file: \(url.path, privacy: .public)")
    }

    @MainActor @objc func showKeyboardShortcuts(_ sender: Any?) {
        KeyboardShortcutsPresenter.present(on: windowController.window)
    }

    private static func timestampString() -> String {
        let df = DateFormatter()
        df.dateFormat = "yyyy-MM-dd_HHmmss"
        return df.string(from: Date())
    }

    @MainActor @objc func exportLog(_ sender: Any?) {
        let panel = NSSavePanel()
        panel.title = "Export CarLink Log"
        panel.nameFieldStringValue = "carlink_macOS_\(Self.timestampString()).log"
        panel.allowedContentTypes = [.plainText]
        panel.canCreateDirectories = true

        guard let hostWindow = windowController.window else { return }
        panel.beginSheetModal(for: hostWindow) { response in
            guard response == .OK, let dest = panel.url else { return }
            guard let source = FileLogger.shared.currentLogURL else {
                let alert = NSAlert()
                alert.messageText = "Export Failed"
                alert.informativeText = "No active session log. Try restarting the app."
                alert.alertStyle = .warning
                alert.runModal()
                return
            }

            // Flush latest entries to disk, then read the file and append the stderr tail
            // (NSLog / print / crash output) for completeness. All of that — flushSync's
            // OSLogStore drain + fsync, two full-file reads and the write — used to run on the
            // main thread and beachballed the app on large session logs; do it on a utility
            // queue and hop back to main only to report the outcome.
            DispatchQueue.global(qos: .utility).async {
                FileLogger.shared.flushSync()
                let outcome: Result<Int, Error>
                do {
                    var body = try String(contentsOf: source, encoding: .utf8)
                    if let stderrData = try? Data(contentsOf: AppDelegate.logFileURL),
                       !stderrData.isEmpty,
                       let stderrText = String(data: stderrData, encoding: .utf8) {
                        body.append("\n\n=== stderr (NSLog / print / crash output) ===\n")
                        body.append(stderrText)
                    }
                    try body.write(to: dest, atomically: true, encoding: .utf8)
                    outcome = .success(body.utf8.count)
                } catch {
                    outcome = .failure(error)
                }
                Task { @MainActor in
                    switch outcome {
                    case .success(let bytes):
                        Self.logger.info("Log exported: \(dest.path, privacy: .public) (\(bytes) bytes)")
                    case .failure(let error):
                        Self.logger.error("Log export failed: \(error.localizedDescription, privacy: .public)")
                        let alert = NSAlert()
                        alert.messageText = "Export Failed"
                        alert.informativeText = error.localizedDescription
                        alert.runModal()
                    }
                }
            }
        }
    }

    @MainActor @objc func revealLogsInFinder(_ sender: Any?) {
        let dir = FileLogger.logsDirectory
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        NSWorkspace.shared.open(dir)
    }

    // MARK: - Help > Adapter Info

    /// CarLink ▸ Settings… — the host-authoritative VehicleConfig editor (the YAML pushed at connect).
    @MainActor @objc func showSettings(_ sender: Any?) {
        SettingsWindowController.shared.show()
    }

    /// Window ▸ Alt Video — the dedicated cluster / navigation video decoder's render surface.
    @MainActor @objc func showAltVideoWindow(_ sender: Any?) {
        AltVideoWindowController.shared.show()
    }

    /// Window ▸ Metadata — live, sidebar-categorized view of every metadata event the box forwards.
    @MainActor @objc func showMetadataWindow(_ sender: Any?) {
        MetadataWindowController.shared.show()
    }

    /// Window ▸ Controls — on-screen buttons for the CarPlay HID/command surfaces.
    @MainActor @objc func showControlsWindow(_ sender: Any?) {
        ControlsWindowController.shared.show()
    }

    /// Window ▸ Control Box — show/hide the main video window's attached control box.
    @MainActor @objc func toggleControlHUD(_ sender: Any?) {
        windowController.toggleControlBox()
    }

    @MainActor @objc func showAdapterInfo(_ sender: Any?) {
        // The legacy adapter that populated this box-info was removed; the OCBM session doesn't surface
        // an equivalent info plane yet, so present the default snapshot (window retained for that future).
        // Minimal honesty fix (U10): the `state` field reflects the REAL session (everything else is
        // still placeholder, and the window's header says so — live data lives in Settings ▸ CCPA).
        let state: String
        if ocbmClient == nil {
            state = "disconnected"
        } else if ControlsBridge.shared.sessionActive {
            state = "streaming"
        } else {
            state = "connected — awaiting stream"
        }
        let snapshot = AdapterInfoPresenter.Snapshot(
            state: state, phoneType: "none", micDecodeType: 0, videoEncoderType: 2,
            firmware: nil, adapterBoxInfo: nil, phoneBoxInfo: nil,
            decryptedSessionToken: nil, rawDeviceInfoBlob: nil
        )
        AdapterInfoPresenter.present(snapshot: snapshot, on: windowController.window)
    }

    // MARK: - Teardown

    /// Stops all per-session helpers and clears session state. Shared by USB
    /// disconnect, Reset Session, resolution-change re-init, and app quit so
    /// no path can forget a piece (stale voice mode, zombie nav window,
    /// still-ticking call timer, un-nil'd adapter/transport…).
    @MainActor private func endSession() {
        // AA first: its BYEBYE must go out while the OCBM link is still up, or the phone keeps a stale
        // session (docs/host/02_ANDROID_AUTO.md Phase 1 — the next connect then crashes gearhead's :projection process).
        stopAAOverOCBM(reason: "session ended", drainTimeout: 2.0)
        micCapture?.stopCapture()
        audioPlayer?.stop()
        ocbmClient?.disconnect()   // OCBM: STOP + stop the pipe (box tears down → holding pattern)
        transport?.stop()
        ocbmClient = nil
        ocbmBridge = nil
        ocbmCoordinator = nil
        controlRelay = nil       // app-driven SETUP relay/session — dropped with the session (plan P3)
        setupSession = nil
        transport = nil
        mainDecoder?.flush()
        mainDecoder = nil   // symmetric with altDecoder — don't keep a stale VideoToolbox session alive
        altDecoder?.flush()
        altDecoder = nil
        AltVideoWindowController.shared.sessionEnded()   // hide the floating Nav/Alt window
        MetadataStore.shared.resetSession()  // clear stale media/nav/call/artwork across the session (audit M-g)
        // Controls window: drop the session gate + the stale surfaces. Leftover clusterContent would
        // re-arm the alt-window gate on the next connect, and a stale limitedUI/lastSent readout
        // would claim phone state that no longer exists.
        ControlsBridge.shared.sessionEnded()
        // CCPA tab: the OCBM link is gone — clear any latched busy, stop claiming "Connected", and
        // mark the last snapshot stale so pre-unplug data doesn't read as live.
        CCPABridge.shared.sessionEnded()
        // Stop the A/V performance sampler + write a final sessionActive:false metrics snapshot.
        StreamMetricsMonitor.shared.stop()
        // Nil the per-session helper too: a stale Task queued before teardown could otherwise touch it
        // on an instance that setupDevice is about to abandon.
        micCapture = nil
        audioPlayer = nil
        // A fresh attach (replug) starts with a clean claim-retry budget.
        claimRetryCount = 0
        // Drop any live SSP Numeric-Comparison code (2026-07-25). The box clears `/tmp/pairing_code`
        // and re-emits CT_PAIRING_CODE on the next SUBSCRIBE, but a teardown that lands MID-pairing
        // (unplug, box reboot) would otherwise leave "Pairing code … — confirm it matches your iPhone"
        // on screen while disconnected, and it takes priority over normal status text on reconnect
        // until the box's clear arrives. Numeric mode only; Just-Works never publishes a code.
        activePairingCode = nil
        windowController.carPlayView.isAndroidAuto = false
        windowController.carPlayView.clearAndroidAutoVideoMode()
    }

    private func teardown() {
        endSession()
        usbManager?.stopScanning()
    }

    /// Format a 6-digit SSP Numeric-Comparison code for the status area, e.g. "418926" →
    /// "Pairing code 418 926 — confirm it matches your iPhone". Grouped 3+3 for legibility.
    private static func pairingStatusText(_ code: String) -> String {
        let grouped: String = code.count == 6
            ? "\(code.prefix(3)) \(code.suffix(3))"
            : code
        return "Pairing code \(grouped) — confirm it matches your iPhone"
    }

    /// Task #29: a hard USB transport loss (USBTransport's 5-consecutive-read-error disconnect) means the
    /// OCBM pipe is dead — revive the safety net that was dead in the OCBM path by re-initializing the
    /// session, which re-enumerates the still-attached adapter so it reconnects (data hiccup) or awaits a
    /// physical re-plug. Guarded so a duplicate callback during teardown is a no-op.
    @MainActor private func handleOCBMTransportLost(reason: String) {
        guard ocbmClient != nil else { return }
        Self.logger.error("OCBM transport lost: \(reason, privacy: .public) — re-initializing session")
        reinitializeAdapterSession(reason: "OCBM transport lost")
    }

    /// Tears down the current session and re-enumerates the still-attached
    /// adapter so it reappears via IOKit notification → setupDevice →
    /// runInitSequence. The USB scanner stays alive throughout. No-op when
    /// no session is active.
    @MainActor private func reinitializeAdapterSession(reason: String) {
        guard ocbmClient != nil else { return }   // #29: re-init the OCBM session
        Self.logger.info("Re-initializing adapter (\(reason, privacy: .public))...")
        endSession()
        let mgr = usbManager!
        DispatchQueue.global(qos: .userInitiated).async {
            mgr.resetDevicesOnLaunch()
        }
    }

    // MARK: - Reset Session (Help menu)

    /// Soft session reset — mirrors carlink_native's "Reset Connection" timing (stop → 2s → start).
    /// The actual disconnect mechanism is endSession() → OCBMClient.disconnect(), whose CT_STOP tells
    /// the box to tear the session down and return to its holding pattern; we then re-enumerate the
    /// still-attached adapter and re-initialize it — recovering from a stuck/flaky CarPlay reconnect
    /// with no physical replug.
    /// True between a Reset Session request and its re-scan; a second ⌃⌘R in
    /// that window would otherwise tear down the session being rebuilt.
    private var resetInProgress = false

    @MainActor @objc func resetSession(_ sender: Any?) {
        guard !resetInProgress else {
            Self.logger.info("[Reset] Reset already in progress — ignoring")
            return
        }
        Self.logger.info("[Reset] Reset Session requested")
        guard transport != nil else {
            // No session — nothing to reset; just refresh the scan.
            Self.logger.info("[Reset] No active session — restarting scan")
            usbManager.stopScanning()
            usbManager.startScanning()
            return
        }
        resetInProgress = true
        // STOP: the teardown's OCBMClient.disconnect() sends CT_STOP — the real drop-the-session
        // signal in the committed model. (The legacy header-only DisconnectPhone(0x0F) +
        // CloseDongle(0x15) frames that used to be queued here were removed: they are 0x55AA55AA
        // legacy-protocol messages the OCBM box resync-skips as junk, so they never did anything.)
        // Never block the main thread on the write pipe here — a wedged pipe is often the very
        // reason the user is resetting. The 0.5s grace gives a healthy pipe room to flush the STOP;
        // stop() aborts a stuck one.
        windowController.window?.title = "CarLink — Resetting…"
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
            guard let self else { return }
            self.teardown()  // endSession + stopScanning (zeroes currentDeviceService)
            // START: after a short settle, re-scan → re-detect the still-attached
            // adapter → usbDeviceDidConnect → setupDevice → runInitSequence.
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) { [weak self] in
                guard let self else { return }
                Self.logger.info("[Reset] Re-scanning for adapter…")
                self.resetInProgress = false
                self.usbManager.startScanning()
            }
        }
    }

    // MARK: - Device Setup

    // (Removed the old hardcoded `vehicleConfigYAML(width:height:)` template — dead code superseded by
    // `VehicleConfigModel` (Settings ▸ Configuration), which is the sole source of the pushed YAML. Audit.)

    private func setupDevice(service: io_service_t) {
        // USBDeviceManager handed us a retained reference for this async hop.
        defer { IOObjectRelease(service) }

        // Re-entry guard (audit H2): if a session is somehow already live (a duplicate
        // usbDeviceDidConnect without an intervening disconnect), tear it down first so we never abandon
        // a running OCBMClient (whose bulk-read loop keeps going) or put two clients on the same USB pipe.
        if transport != nil || ocbmClient != nil {
            Self.logger.info("setupDevice: a session was already live — ending it before re-setup")
            endSession()
        }
        // Clear any stale "Disconnected" title from a prior teardown; it advances to "CarLink — CarPlay"
        // when frames flow (audit M-d — the old phone-type title path is dead over OCBM).
        windowController.window?.title = "CarLink"

        // Claim the USB interface. On failure (lost exclusive-access race,
        // enumeration hiccup) the manager still holds this device, so no new
        // attach will ever fire — re-scan a few times instead of sitting idle
        // until a physical replug.
        guard let claimed = USBInterfaceClaimer.claim(deviceService: service) else {
            Self.logger.error("Failed to claim USB interface")
            claimRetryCount += 1
            if claimRetryCount <= 3 {
                Self.logger.info("Retrying adapter claim (attempt \(self.claimRetryCount, privacy: .public)/3) in 2s")
                DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) { [weak self] in
                    guard let self else { return }
                    self.usbManager.stopScanning()
                    self.usbManager.startScanning()
                }
            }
            return
        }
        claimRetryCount = 0

        // Create transport
        let transport = USBTransport(
            interfaceRef: claimed.interfaceRef,
            deviceRef: claimed.deviceRef,
            bulkIn: claimed.bulkInPipe,
            bulkOut: claimed.bulkOutPipe
        )
        self.transport = transport

        // Create H.264 decoder — give it the view's display layer
        let decoder = VideoDecoder()
        decoder.label = "main"
        self.mainDecoder = decoder

        // Replace the view's video layer with the decoder's layer
        let view = windowController.carPlayView
        view.videoLayer.removeFromSuperlayer()
        view.videoLayer = decoder.displayLayer
        decoder.displayLayer.videoGravity = .resizeAspect
        decoder.displayLayer.frame = view.bounds
        view.layer?.addSublayer(decoder.displayLayer)

        // Create audio player
        let audio = AudioPlayer()
        audio.start()
        self.audioPlayer = audio

        // ── OCBM committed-model chain (A/V verification) ──────────────────────────────────────────
        // Replaces the riddlebox AdapterProtocol. As the host app in the committed model, our SUBSCRIBE
        // commands the box IDLE→projection→ARM (docs/carplay/02_SESSION_LIFECYCLE.md); the heartbeat keeps the session alive. The box
        // forwards ENCRYPTED A/V + hands the per-stream key; OCBMAVDecrypt decrypts (ChaCha20) and the
        // OCBMAVBridge feeds the reused VideoDecoder + AudioPlayer. Input (touch/mic), nav, calls, and
        // now-playing are deferred past this A/V-verification milestone.
        // Dedicated ALT / navigation (cluster) decoder — a full duplicate of the main video decoder,
        // rendered in the Alt Video window. Fed only when the box forwards CH_ALT_VIDEO (a second
        // screen stream); dormant otherwise.
        let altDecoder = VideoDecoder()
        altDecoder.label = "alt"
        self.altDecoder = altDecoder
        AltVideoWindowController.shared.attach(layer: altDecoder.displayLayer)
        // Auto-size the floating Nav/Alt window to the cluster stream's ACTUAL coded resolution and
        // lock its aspect (no letterbox; aspect-preserving user resize) — same behavior as the main
        // window. iOS picks the cluster size, so drive it from the decoder, not the requested config.
        altDecoder.onDimensions = { w, h in AltVideoWindowController.shared.applyVideoSize(width: w, height: h) }

        let bridge = OCBMAVBridge(decoder: decoder, altDecoder: altDecoder, audio: audio)
        // Floating Nav/Alt window: appears on the first alt frame, hides when the stream stops, never
        // opens if no alt video ever arrives (user directive 2026-07-12).
        bridge.onAltFrame = {
            DispatchQueue.main.async {
                // Only surface the cluster window when the user has the Nav Video toggle ON. The box
                // releases cluster focus by default, but a few straggler frames can arrive right after a
                // stopUI (or before it lands) — ignoring them here keeps the toggle-off from bouncing back
                // on and stops the window flashing open when the user turned it off.
                guard ControlsBridge.shared.navVideoOn else { return }
                AltVideoWindowController.shared.noteActivity()
            }
        }
        self.ocbmBridge = bridge
        let client = OCBMClient(transport: transport)   // USBTransport conforms to RawBulkTransport
        client.av.delegate = bridge
        // Surface the per-lane video codec (HEVC vs H.264) in the metrics — the bridge's config parse is
        // the authoritative source. Latches on each VideoConfig (main → mainVideo, alt → altVideo).
        bridge.main.onCodec = { [weak client] c in client?.av.setVideoCodec(c.rawValue, stream: .mainVideo) }
        bridge.alt.onCodec = { [weak client] c in client?.av.setVideoCodec(c.rawValue, stream: .altVideo) }
        // Video-loss recovery (task #33): on a detected frame gap, ask the box to request an iOS keyframe.
        client.av.onVideoGap = { [weak client] in client?.requestKeyframe() }
        // ALT/cluster lane uses requestAltKeyframe so the box re-IDRs VideoStream.Alt1 specifically —
        // a bare keyframe only repaints the main console, leaving the nav feed frozen after a view switch.
        client.av.onAltVideoGap = { [weak client] in client?.requestAltKeyframe() }
        altDecoder.onNeedsKeyFrame = { [weak client] in client?.requestAltKeyframe() }
        // V4 backpressure: a frame dropped at the decode/enqueue slot breaks the P-frame chain — ask
        // the box for a fresh IDR (reuses requestKeyframe's ≤1/500 ms throttle) so the decoder re-syncs.
        decoder.onFrameDropped = { [weak client] in client?.requestKeyframe() }
        altDecoder.onFrameDropped = { [weak client] in client?.requestAltKeyframe() }
        // Wire the previously-dead delegates (task #29): box SESSION_EVENT + USBTransport's 5-read-error
        // disconnect + write-failure + the A/V-progress watchdog now drive UI status + reconnect, instead
        // of being dropped (which left the app silently at "waiting for adapter").
        let coord = OCBMSessionCoordinator()
        coord.onStatus = { [weak self] s in
            guard let self else { return }
            self.lastCoordStatus = s
            // A live pairing code takes priority over the watchdog status while the user is matching it.
            let text = self.activePairingCode.map(Self.pairingStatusText) ?? s
            self.windowController.carPlayView.updateStatus(text)
        }
        coord.onStreaming = { [weak self] on in
            guard let self else { return }
            self.windowController.carPlayView.setStreaming(on)
            // Controls-window honesty gate: the client silently drops sends until the box accepts
            // the SUBSCRIBE and streams, so `client != nil` alone can't prove a control reached the
            // wire. This streaming signal is the arm/disarm; endSession() also clears it.
            ControlsBridge.shared.sessionActive = on
            // Drive the window title from the live OCBM streaming signal — the legacy phone-type title
            // path (adapterDidDetectPhoneType) is dead over OCBM, which left the title stuck (audit M-d).
            self.windowController.window?.title = on ? "CarLink — CarPlay" : "CarLink"
        }
        coord.onTransportLost = { [weak self] reason in self?.handleOCBMTransportLost(reason: reason) }
        // Truthful SUBSCRIBE state → Controls window (C2). Distinct from onStreaming (first A/V): commands
        // reach the wire during the SUBSCRIBE→first-frame window, so the Controls "did it send" logic gates
        // on this, not on streaming. Fires on the client's control `queue`; hop to main for the @Published.
        client.onSubscriptionState = { on in
            Task { @MainActor in ControlsBridge.shared.subscribed = on }
        }
        client.delegate = coord
        transport.delegate = coord
        self.ocbmCoordinator = coord
        self.ocbmClient = client
        // Live A/V stream-performance monitor: arm the ~1 Hz sampler on the decrypt layer's per-stream
        // counters. Publishes to Settings ▸ CCPA, writes /tmp/carlink_metrics.json, and logs a throttled
        // "AVmon" summary. Stopped in endSession(). Read-only over the mutex-guarded counters — never
        // touches the A/V hot path. start() resets the shared anomaly log; hand that SAME log to both
        // feeders (video events from the decrypt layer, audio DSP/underrun events from the player).
        StreamMetricsMonitor.shared.start(av: client.av)
        StreamMetricsMonitor.shared.bindDecoders(main: mainDecoder, alt: altDecoder) // AD/drop accounting
        client.av.anomalyLog = StreamMetricsMonitor.shared.anomalyLog
        audio.anomalyLog = StreamMetricsMonitor.shared.anomalyLog
        // Metadata window feed (2026-07-12): box-forwarded inbound /command plists over CH_METADATA.
        client.onMetadata = { chunk in MetadataStore.shared.ingest(chunk) }
        // Microphone uplink: capture the mic ONLY while the box says the iPhone has an active type-100
        // `input` SETUP (Siri/telephony), at the box-negotiated format, and ship the PCM back over
        // CH_MIC. The gate arrives on the transport read queue; hop to the main actor for the
        // AVAudioEngine lifecycle. onPCMData feeds straight into the client (its own queue serializes).
        let mic = MicCapture()
        mic.onPCMData = { [weak client] pcm in client?.sendMicPCM(pcm) }
        self.micCapture = mic
        mic.requestPermission() // surface the TCC prompt now, not mid-Siri
        client.onUplinkGate = { [weak self] on, rate, channels in
            Task { @MainActor in
                guard let self, let mic = self.micCapture else { return }
                if on {
                    mic.startCapture(sampleRate: Double(rate), channels: UInt32(channels))
                } else {
                    mic.stopCapture()
                }
            }
        }
        // Wireless SSP Numeric-Comparison code: show it in the status area for the user to match against
        // the iPhone; clear it (restore the watchdog status) when pairing completes.
        client.onPairingCode = { [weak self] code in
            Task { @MainActor in
                guard let self else { return }
                self.activePairingCode = code
                let text = code.map(Self.pairingStatusText) ?? self.lastCoordStatus
                self.windowController.carPlayView.updateStatus(text)
            }
        }
        // Controls window (2026-07-12): point the SwiftUI bridge at the live client. Repointed on
        // every (re)connect, so a reconnect never leaves a stale send path.
        ControlsBridge.shared.client = client
        // CCPA management tab: point the bridge at the live client + deliver its CH_MGMT responses on the
        // main actor. Repointed each (re)connect. The onBoxInfo/onBoxAck fire on the transport read queue.
        CCPABridge.shared.client = client
        client.onBoxInfo = { info in Task { @MainActor in CCPABridge.shared.receiveInfo(info) } }
        client.onBoxAck = { verb, status in Task { @MainActor in CCPABridge.shared.receiveAck(verb: verb, status: status) } }
        // Host-authoritative VehicleConfig (task #5 / docs/carplay/04_CAPABILITIES_AND_CONFIG.md): an Apple-schema YAML pushed at SUBSCRIBE.
        // ocbmd lands it at /tmp/carplay_cfg.yaml; airplayd reads the main-panel pixelDimensions per
        // connection to build /info (docs/carplay/06_AV_PIPELINE.md). The box ignores fields it doesn't yet consume, so
        // this grows toward the full Apple template as the box learns to act on each.
        //
        // The **Settings ▸ Configuration** window (`VehicleConfigModel`) is now the single source of
        // truth for the config: it renders the YAML pushed here, and its main-video resolution also
        // drives the app window geometry so the window shape matches what the iPhone encodes.
        let cfg = VehicleConfigModel.shared
        client.sessionConfig = cfg.data()
        // App-driven SETUP (plan P3): gate on the pushed config's `appDrivenSetup` toggle (default ON —
        // wired since 2026-08-09, wireless since the 2026-08-10 flip; it now drives both transports).
        // When ON, the box relays each RTSP/SETUP exchange over CH_RTSP and THIS app authors the response
        // the phone sees (AirPlaySetupSession); the box's own local response is the fallback on any relay
        // error. When OFF, `onControlRelay` stays nil ⇒ the box never receives an RS_RESP ⇒ fully
        // box-driven, matching P1's lever. The box also gates on its own `appDrivenSetup` YAML field, so
        // this and the box agree via the same pushed config.
        if cfg.appDrivenSetup {
            // Author from the COMMITTED config (audit A4) — the same snapshot the box was pushed above via
            // cfg.data(). Building from live `cfg.config` would let unsaved form edits make the phone's
            // SETUP response contradict what the box advertised. Falls back to live only pre-first-load.
            let session = AirPlaySetupSession(config: cfg.committedConfig ?? cfg.config, mode: .author)
            let relay = OCBMControlRelay(send: { [weak client] _, framed in client?.sendControlRelay(framed) })
            session.log = { s in Self.logger.info("[setup] \(s, privacy: .public)") }
            relay.log = { s in Self.logger.info("[relay] \(s, privacy: .public)") }
            // docs/carplay/04_CAPABILITIES_AND_CONFIG.md #6 — the box ships `cfg_crc` in every RS_OPEN and this handler used to discard
            // all four parameters. That CRC is taken over the EXACT YAML bytes the box's `/info` was
            // built from, so comparing it against the bytes we pushed is the only cross-process check
            // that the box is serving the config we think it is. Authoring a SETUP response from a
            // config the box never loaded is precisely the drift this detects.
            relay.onOpen = { [weak coord] _, _, boxCRC, _ in
                session.reset()
                let ours = CRC32.compute(cfg.data())
                let detail: String
                if boxCRC == ours {
                    detail = "config in sync"
                } else if boxCRC == 0 {
                    // 0 is the box's sentinel for "no YAML loaded — running on built-in defaults".
                    // With the #26 logging in place the box now says WHY on its own console; this is
                    // the app-side half of the same story.
                    detail = "⚠️ BOX IS ON BUILT-IN DEFAULTS (cfg_crc=0) — our push never landed or was rejected"
                } else {
                    detail = String(
                        format: "⚠️ CONFIG DRIFT — box loaded 0x%08X, we pushed 0x%08X", boxCRC, ours)
                }
                Self.logger.info("[relay] RS_OPEN \(detail, privacy: .public)")
                coord?.noteSetupRelay("RS_OPEN — authoring a fresh session (\(detail))")
            }
            relay.onClose = { [weak coord] _, _ in
                session.reset()
                coord?.noteSetupRelay("RS_CLOSE — session ended")
            }
            relay.onRequest = { [weak coord] _, _, route, _, local, req in
                switch route {
                case OCBM.routeSetup:
                    let body = session.onSetup(local: local, req: req)
                    let diff = AirPlaySetupSession.oracleDiff(authored: body, local: local)
                    if !diff.isEmpty {
                        coord?.noteSetupRelay("SETUP oracle divergence: \(diff.joined(separator: ", "))")
                    }
                    return (200, body)
                case OCBM.routeRecord:
                    return (200, session.onRecord(local: local))
                case OCBM.routeTeardown:
                    session.onTeardown(req: req)
                    return nil   // NOTIFY — box tore down + answered already; no reply owed
                default:
                    return nil   // reserved route — decline so the box falls back to its local response
                }
            }
            client.onControlRelay = { [weak relay] payload in relay?.feed(payload) }
            self.setupSession = session
            self.controlRelay = relay
            Self.logger.info("app-driven SETUP ARMED — host will author RTSP/SETUP (plan P3)")
        }
        // Size the window from the PERSISTED resolution (the same vc.* keys save() writes — the
        // source committedYAML derives from), NOT the live @Published cfg.mainWidth/mainHeight:
        // those are unsaved form edits, and sizing from them while pushing the committed YAML
        // skewed the window aspect + touch mapping vs. what the iPhone actually encodes.
        let committedRes = VehicleConfigModel.persistedMainResolution()
        windowController.applyResolution(width: CGFloat(committedRes.width), height: CGFloat(committedRes.height))

        // The box requests a ForceKeyFrame on the A/V seam connect, so the host doesn't drive keyframes;
        // if the renderer flushes, it resumes on the next natural IDR.
        // Renderer-driven recovery (task #33): when VideoToolbox reports it needs an IDR to resume
        // (flush-required), actively force one via the box instead of passively waiting for a natural IDR.
        // Reuses the ≤1/500 ms throttle in requestKeyframe(). Complements the seq-gap keyframe path.
        decoder.onNeedsKeyFrame = { [weak client] in
            Self.logger.info("decoder needs keyframe — requesting IDR")
            client?.requestKeyframe()
        }

        // Wire touch/keyboard input (delegate exists; the input uplink over OCBM is the next milestone).
        windowController.carPlayView.delegate = self

        // §4e: AA over OCBM, selected by the BOX. The box owns arbitration — it sees the USB bus,
        // classifies the phone, runs the AOAP switch and claims /tmp/projection_owner — and mirrors the
        // result as ctProjMode. On pmWiredAa we open the CH_IP stream to aa-bridge and run the AA engine;
        // on any other mode we hand the window back to CarPlay. This retires the AA_OCBM env stand-in,
        // which remains as a forced start for a box whose ocbmd predates the mode event.
        let aaTarget = ProcessInfo.processInfo.environment["AA_OCBM_TARGET"] ?? "127.0.0.1:5277"
        // Per-CLIENT sequence: OCBMClient's counter starts at 1, so a replug (new client on the same
        // AppDelegate) would have every event rejected against the previous client's high-water mark.
        lastProjModeSeq = 0
        client.onProjectionMode = { [weak self, weak client] mode, seq in
            Task { @MainActor in
                guard let self, let client else { return }
                // Ignore anything from a superseded client (a replug builds a new one): its sequence
                // is from a different counter, and acting on it would fight the live session.
                guard client === self.ocbmClient else { return }
                guard seq > self.lastProjModeSeq else {
                    Self.logger.info("projection mode superseded by a newer event — ignoring")
                    return
                }
                self.lastProjModeSeq = seq
                if mode == OCBM.pmWiredAa {
                    self.startAAOverOCBM(client: client, target: aaTarget)
                } else {
                    self.stopAAOverOCBM(reason: "box mode = \(OCBM.projModeName(mode))")
                }
            }
        }

        // Wired BEFORE connect(): the box re-emits the mode on our SUBSCRIBE, and a callback assigned
        // after the read loop starts could miss it.
        client.connect()   // start the bulk read loop + HELLO + SUBSCRIBE + ~1 Hz heartbeat
        Self.logger.info("OCBM host connected — SUBSCRIBE sent (commanding box projection)")

        if ProcessInfo.processInfo.environment["AA_OCBM"] != nil {
            // Forced (pre-mode-event box): arm_aa needs a moment to launch aa-bridge after SUBSCRIBE.
            DispatchQueue.main.asyncAfter(deadline: .now() + 3.0) { [weak self] in
                self?.startAAOverOCBM(client: client, target: aaTarget)
            }
        }
    }
}

// MARK: - USBDeviceManagerDelegate

extension AppDelegate: USBDeviceManagerDelegate {
    nonisolated func usbDeviceDidConnect(device: io_service_t) {
        Self.logger.info("CPC200-CCPA adapter connected")
        // Bump the generation so any in-flight disconnect Task sees itself superseded. `setupDevice` always
        // runs (its re-entry guard tears down any prior session, and its `defer` releases `device`), so a
        // replug collapses to just this connect — no stale teardown of the session it builds.
        usbEventGen.withLock { $0 &+= 1 }
        Task { @MainActor in
            setupDevice(service: device)
        }
    }

    nonisolated func usbDeviceDidDisconnect() {
        Self.logger.info("Adapter disconnected")
        let gen = usbEventGen.withLock { $0 &+= 1; return $0 }
        Task { @MainActor [weak self] in
            guard let self else { return }
            // Skip teardown if a newer USB event (a fast replug's connect) has already superseded this
            // disconnect — otherwise the stale endSession would kill the just-built session (audit A1).
            guard self.usbEventGen.withLock({ $0 }) == gen else {
                Self.logger.info("stale disconnect superseded by a newer USB event — skipping teardown")
                return
            }
            self.endSession()
            self.windowController.window?.title = "CarLink — Disconnected"
        }
    }
}

// MARK: - CarPlayViewDelegate

extension AppDelegate: CarPlayViewDelegate {
    func carPlayView(_ view: CarPlayView, didMultiTouch action: MultiTouchAction, x: Float32, y: Float32) {
        // OCBM/CarPlay path (task #20): forward single-touch to the box → airplayd → iPhone hidSendReport.
        guard let client = ocbmClient else { return }
        let phase: UInt8
        switch action {
        case .down: phase = OCBM.touchDown
        case .move: phase = OCBM.touchMove
        case .up:   phase = OCBM.touchUp
        }
        client.sendTouch(phase: phase, nx: x, ny: y)
    }

    // Two-finger gestures and the Android-Auto integer-touch path were legacy-adapter only; the OCBM
    // box doesn't advertise a multi-touch HID descriptor yet, so these are no-ops until Phase 2.
    func carPlayView(_ view: CarPlayView, didMultiTouchTwo points: [(action: MultiTouchAction, x: Float32, y: Float32, id: UInt32)]) {}

    func carPlayView(_ view: CarPlayView, didTouch action: TouchAction, x: UInt32, y: UInt32) {
        // AA mode: sendAATouch hands x,y in 0..10000 (crop-aware normalized). Map to the
        // touchscreen we ACTUALLY ADVERTISED and push up the input channel.
        guard let session = aaSession else { return }
        // Scale from the session's own advertised surface, never a literal. This was hardcoded
        // 800x480, which was correct only while the SD response was also hardcoded 800x480; once the
        // declaration became configurable (AACapability) the two silently disagreed and every touch
        // landed compressed into the top-left of the screen — a 1280x720 session mapped its far right
        // edge to x=799 of 1280. Device-observed 2026-08-27, immediately after 720p was negotiated.
        let (sw, sh) = session.touchSurface
        // Clamp to the LAST valid pixel (w-1/h-1), not the width/height: x == width is off the
        // surface, and a tap on the extreme right/bottom edge would hand gearhead an out-of-range
        // pointer.
        // `AA_EDGE_CLAMP_BUG=1` restores the OLD off-by-one (clamping to the width/height rather than
        // the last pixel) so the pre-fix behaviour can be reproduced on demand — it is how the
        // tap-triggered teardown gets attributed by experiment instead of by assumption.
        let hi = Self.edgeClampBug
        let ax = UInt32(min(Int(hi ? sw : sw - 1), Int(x) * Int(sw) / 10000))
        let ay = UInt32(min(Int(hi ? sh : sh - 1), Int(y) * Int(sh) / 10000))
        let aaAction: UInt64
        switch action {
        case .down: aaAction = 0
        case .up: aaAction = 1
        default: aaAction = 2
        }
        session.enqueueTouch(x: ax, y: ay, action: aaAction)
    }

    func carPlayView(_ view: CarPlayView, didPressCommand command: CommandID) {
        // OCBM/CarPlay path (task #35): media keys become HID media-button taps (uid 2) on the box;
        // Siri becomes an AirPlay /command dispatch (requestSiri, integer hold pair). Home and the
        // D-pad ride the uid-3 HID D-Pad device, which IS advertised (dPadSupport defaults true);
        // knob is uid 4 behind hidConfig.knobSupport. The 2026-07-06 third-HID-device incident no
        // longer gates them. Legacy adapter fallback for the old carlink USB protocol.
        guard let client = ocbmClient else { return }
        switch command {
        case .mediaPlay:      client.sendMediaButton(OCBM.mbtnPlay)
        case .mediaPause:     client.sendMediaButton(OCBM.mbtnPause)
        case .mediaPlayPause: client.sendMediaButton(OCBM.mbtnPlayPause)
        case .mediaNext:      client.sendMediaButton(OCBM.mbtnNext)
        case .mediaPrev:      client.sendMediaButton(OCBM.mbtnPrev)
        case .mediaHome, .requestHostUI:
            // Home is the D-Pad HID AC Home usage (uid 3, SDK HIDDPad), NOT requestUI (which only
            // foregrounds the accessory UI and did nothing as a Home button — 2026-07-12).
            client.sendNav(OCBM.navHome)
        case .dpadBack:              client.sendNav(OCBM.navBack)
        case .dpadUp, .knobUp:       client.sendNav(OCBM.navUp)
        case .dpadDown, .knobDown:   client.sendNav(OCBM.navDown)
        case .dpadLeft:              client.sendNav(OCBM.navLeft)
        case .dpadRight:             client.sendNav(OCBM.navRight)
        case .dpadEnter:             client.sendNav(OCBM.navSelect)
        case .siriDown:
            // Siri is a HOLD (SDK AirPlaySiriAction, docs/carplay/05_METADATA_AND_CONTROLS.md §2.4). The keyboard has no reliable
            // key-up path here, so a tap synthesizes the pair with a short hold gap.
            client.sendCommand(OCBM.cmdSiriDown)
            // Capture `client` STRONGLY: with [weak client] an endSession() racing this window
            // deallocated the client and DROPPED the paired UP, leaving iOS holding a phantom
            // Siri button. A bounded 0.3 s lifetime extension is safe, and if the session died
            // meanwhile sendCommand's own subscribed gate drops the UP harmlessly.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
                client.sendCommand(OCBM.cmdSiriUp)
            }
        case .siriUp:
            client.sendCommand(OCBM.cmdSiriUp)
        default:
            Self.logger.debug("command \(command.rawValue) has no OCBM backend — dropped")
        }
    }
}

// MARK: - Window lifecycle

extension AppDelegate: NSWindowDelegate {

    /// Closing the main CarLink window quits the whole app.
    func windowWillClose(_ notification: Notification) {
        if (notification.object as? NSWindow) === windowController.window {
            Self.logger.info("Main window closed — quitting app")
            NSApplication.shared.terminate(nil)
        }
    }

    // Fullscreen support was removed 2026-08-02: the video windows are borderless, floating, and
    // aspect-locked (collectionBehavior = .fullScreenNone), so `windowDidEnter/ExitFullScreen` are
    // unreachable. The old "Match Screen Resolution?" prompt lived only in the enter callback; per owner
    // decision it is dropped (resolution stays fully configurable in Settings), and the now-dead
    // `fullscreenResolutionPromptDeclined` UserDefault is simply left unread.
}

// MARK: - AA visual-proof player (docs/host/02_ANDROID_AUTO.md Phase 1)

// Plays a captured Android Auto H.264 elementary stream (Annex-B, from host/aa-headunit)
// through the REAL VideoDecoder + CarPlayView — no box, no live phone. AA video is
// Annex-B; the app's decoder wants AVCC, so each frame goes through AVCCFastPath (the
// same shim the box CH_AA path will use). Enable: AA_PLAY=/tmp/aa_tap.h264.
enum AADebugPlayer {
    static func start(path: String, view: CarPlayView, decoder: VideoDecoder) {
        // Host the decoder's display layer in the CarPlayView (mirrors the live wiring).
        view.videoLayer.removeFromSuperlayer()
        view.videoLayer = decoder.displayLayer
        decoder.displayLayer.videoGravity = .resizeAspect
        decoder.displayLayer.frame = view.bounds
        view.layer?.addSublayer(decoder.displayLayer)
        view.isAndroidAuto = true
        view.updateVideoAspect(width: 800, height: 480)

        DispatchQueue.global(qos: .userInitiated).async {
            guard let data = try? Data(contentsOf: URL(fileURLWithPath: path)) else {
                NSLog("[AA-play] cannot read \(path)"); return
            }
            let nals = Self.splitAnnexB(data)
            guard !nals.isEmpty else { NSLog("[AA-play] no NAL units in \(path)"); return }

            var sps: Data?
            var pps: Data?
            for n in nals where !n.isEmpty {
                switch n[n.startIndex] & 0x1F {
                case 7: if sps == nil { sps = n }
                case 8: if pps == nil { pps = n }
                default: break
                }
            }
            guard let s = sps, let p = pps else { NSLog("[AA-play] no SPS/PPS found"); return }
            decoder.configure(codec: .h264, parameterSets: [s, p])

            let frames = nals.filter { !$0.isEmpty && ($0[$0.startIndex] & 0x1F == 1 || $0[$0.startIndex] & 0x1F == 5) }
            NSLog("[AA-play] \(frames.count) frames from \(path) — looping at 30fps")
            let interval = 1.0 / 30.0
            while true {
                for f in frames {
                    let isKF = (f[f.startIndex] & 0x1F) == 5
                    decoder.decodeAndDisplay(avcc: Self.avccWrap(f), keyframe: isKF)
                    Thread.sleep(forTimeInterval: interval)
                }
            }
        }
    }

    private static func avccWrap(_ nal: Data) -> Data {
        var out = Data(capacity: 4 + nal.count)
        let len = UInt32(nal.count).bigEndian
        withUnsafeBytes(of: len) { out.append(contentsOf: $0) }
        out.append(nal)
        return out
    }

    private static func splitAnnexB(_ data: Data) -> [Data] {
        return data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> [Data] in
            guard let base = raw.baseAddress else { return [] }
            return AVCCFastPath.annexBNALRanges(raw).map { r in
                Data(bytes: base + r.lowerBound, count: r.count)
            }
        }
    }
}
