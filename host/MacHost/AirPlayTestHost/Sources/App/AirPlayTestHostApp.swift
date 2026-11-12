// AirPlayTestHostApp.swift — "AirPlay Test Host": one window that puts the box into AirPlay-only mode
// (OCBM MODE_AIRPLAY) and shows what comes back. See ../../README.md.

import AppKit
import SwiftUI

@main
struct AirPlayTestHostApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        Window("AirPlay Test Host", id: "main") {
            ContentView(controller: delegate.controller, surface: delegate.surface, autostart: delegate.launch.autostartPassphrase)
        }
        .defaultSize(width: 1180, height: 900)
        // A test tool: its one window is always there at launch, whatever was open last time.
        .defaultLaunchBehavior(.presented)
        .restorationBehavior(.disabled)
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let surface = VideoSurface()
    let launch = LaunchOptions(arguments: CommandLine.arguments, environment: ProcessInfo.processInfo.environment)
    private(set) lazy var controller: SessionController = launch.makeController(surface: surface)

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Debug bench runs: Start without anyone at the window (and whether or not the window has
        // come on screen yet, which a launch from a background shell can delay).
        if let passphrase = launch.autostartPassphrase { controller.start(passphrase: passphrase) }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func applicationWillTerminate(_ notification: Notification) {
        controller.shutdown()
    }
}

/// What the command line asks for. A release build takes nothing: it talks to the box on USB, and every
/// value is typed into the window. Debug builds add the bench switches below.
///
/// Every switch is a `-key value` PAIR, on purpose: AppKit reads the command line too, takes an argument
/// that is nobody's value for a file to open, and an app launched "to open a file" gets no window.
///
///   -synthetic-h264 FILE -synthetic-aac FILE   no USB at all: a synthetic box plays these files through
///                                              the real tunnel format (README.md)
///   -synthetic-hevc FILE                       ... and this Annex-B HEVC when the request allows HEVC
///                                              with a canvas above 1080p (as the phone does)
///   -synthetic-hevc-size WxH                   its coded size, for the header geometry (994x2160)
///   -synthetic-alternate YES                   ... or H.264 and HEVC sessions in turn, whatever is asked
///   -synthetic-drop-frame N                    ... and loses video frame N of each mirroring session
///   -synthetic-unplug-after S                  ... and is unplugged once, S seconds after Start
///   -ssid S -name N -channel C                 fill the fields
///   -display WxH@F -hevc YES|NO                the request's display and HEVC flag (as the menu/toggle)
///   -smooth YES                                start with "Smooth playback" on
///   -autostart YES                             press Start; the passphrase comes from the
///                                              OCBM_AIRPLAY_PSK environment variable (never argv)
///   -status-json FILE                          write the numeric status there twice a second
///   -output-gain X                             scale the audio output (0...1)
@MainActor
struct LaunchOptions {
    var syntheticH264: String?
    var syntheticAAC: String?
    var syntheticHEVC: String?
    var syntheticHEVCSize = (width: 994, height: 2160)
    var alternateCodecs = false
    var display: AirPlayModeRequest.Display?
    var hevc: Bool?
    var smooth = false
    var dropFrame: Int?
    var unplugAfter = 0.0
    var ssid: String?
    var name: String?
    var channel: String?
    var statusJSON: String?
    var outputGain: Float = 1
    /// Non-nil = start as soon as the window is up.
    var autostartPassphrase: String?
    var problems: [String] = []

    init(arguments: [String], environment: [String: String]) {
        #if DEBUG
        var autostart = false
        var i = 1
        while i + 1 < arguments.count {
            let value = arguments[i + 1]
            switch arguments[i] {
            case "-synthetic-h264": syntheticH264 = value
            case "-synthetic-aac": syntheticAAC = value
            case "-synthetic-hevc": syntheticHEVC = value
            case "-synthetic-hevc-size":
                let p = value.split(separator: "x").compactMap { Int($0) }
                if p.count == 2 { syntheticHEVCSize = (p[0], p[1]) }
            case "-synthetic-alternate": alternateCodecs = value == "YES"
            case "-display":
                display = AirPlayModeRequest.Display(text: value)
                if display == nil { problems.append("-display wants WxH@F, e.g. 3840x2160@60 (ignored: \(value))") }
            case "-hevc": hevc = value == "YES"
            case "-smooth": smooth = value == "YES"
            case "-synthetic-drop-frame": dropFrame = Int(value)
            case "-synthetic-unplug-after": unplugAfter = Double(value) ?? 0
            case "-ssid": ssid = value
            case "-name": name = value
            case "-channel": channel = value
            case "-status-json": statusJSON = value
            case "-output-gain": outputGain = Float(value) ?? 1
            case "-autostart": autostart = value == "YES"
            default: break // AppKit's own pairs (-NSDocumentRevisionsDebugMode YES and friends)
            }
            i += 2
        }
        if autostart {
            if let psk = environment["OCBM_AIRPLAY_PSK"], !psk.isEmpty {
                autostartPassphrase = psk
            } else {
                problems.append("-autostart needs the passphrase in the OCBM_AIRPLAY_PSK environment variable")
            }
        }
        #endif
    }

    func makeController(surface: VideoSurface) -> SessionController {
        var notes = problems
        #if DEBUG
        let synthetic = syntheticLink(notes: &notes)
        #else
        let synthetic: BoxLink? = nil
        #endif
        // A bench run leaves the remembered fields alone.
        let form = FormModel(defaults: synthetic == nil ? .standard : nil)
        if let ssid { form.ssid = ssid }
        if let name { form.name = name }
        if let channel { form.channel = channel }
        if let display { form.display = display }
        if let hevc { form.hevc = hevc }
        let controller = SessionController(
            boxLink: synthetic ?? USBBoxLink(), form: form, audioOutput: true, outputScale: outputGain,
            onFrame: { surface.present($0) }, onVideoCleared: { surface.clear() })
        controller.statusDumpPath = statusJSON
        for note in notes { controller.log.add(note) }
        if smooth { controller.setSmoothPlayback(true) }
        return controller
    }

    #if DEBUG
    /// Non-nil whenever a synthetic run was asked for. A run whose media cannot be read gets a link with
    /// no box on it, NEVER the USB one: `-synthetic-*` is a promise that USB is not touched.
    private func syntheticLink(notes: inout [String]) -> BoxLink? {
        guard syntheticH264 != nil || syntheticAAC != nil else { return nil }
        guard let h264 = syntheticH264, let aac = syntheticAAC,
              let v = FileManager.default.contents(atPath: h264).flatMap({ TunnelFixture.video(annexB: [UInt8]($0)) }),
              let a = FileManager.default.contents(atPath: aac).flatMap({ TunnelFixture.audio(adts: [UInt8]($0)) }) else {
            notes.append("SYNTHETIC RUN, but the media could not be read as Annex-B H.264 and ADTS AAC: there is no box (USB is not used)")
            return AbsentLink()
        }
        var options = SyntheticBox.Options()
        options.dropVideoFrame = dropFrame
        options.alternateCodecs = alternateCodecs
        var media = SyntheticBox.Media(video: v, audio: a)
        if let path = syntheticHEVC {
            if let h = FileManager.default.contents(atPath: path).flatMap({
                TunnelFixture.hevcVideo(annexB: [UInt8]($0), width: syntheticHEVCSize.width, height: syntheticHEVCSize.height)
            }) {
                media.hevc = h
                notes.append("SYNTHETIC BOX: HEVC stream loaded, \(h.frames.count) frames at \(Int(media.hevcFPS)) fps")
            } else {
                notes.append("SYNTHETIC BOX: -synthetic-hevc could not be read as Annex-B HEVC; H.264 only")
            }
        }
        notes.append("SYNTHETIC BOX: no USB is used. \(v.frames.count) video frames, \(a.units.count) audio units, looping")
        return SyntheticLink(media: media, options: options, unplugAfter: unplugAfter, unplugFor: 3)
    }
    #endif
}
