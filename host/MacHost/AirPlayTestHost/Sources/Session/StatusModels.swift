// StatusModels.swift — what the window shows, as small @Observable models: one per panel section, so a
// counter that ticks twice a second invalidates only the rows that read it. Every property type is
// Equatable, which lets the generated setters skip a write that changes nothing (the poll rewrites
// every value on every tick). No AppKit / SwiftUI here: the tests drive these through the controller.

import CoreGraphics
import Foundation
import Observation

/// The fields that may be remembered between launches (SSID, name, channel, display, HEVC). The
/// passphrase is not one of them: it lives in the view's own state and in the running request, and
/// nowhere else.
@MainActor
@Observable
final class FormModel {
    private static let ssidKey = "airplayTest.ssid"
    private static let nameKey = "airplayTest.receiverName"
    private static let channelKey = "airplayTest.channel"
    private static let displayKey = "airplayTest.display"
    private static let hevcKey = "airplayTest.hevc"

    var ssid: String
    var name: String
    /// Text, so an empty field can mean "box default".
    var channel: String
    /// `AIRPLAY_REQ_DISPLAY`: the canvas the phone renders for.
    var display: AirPlayModeRequest.Display
    /// `AIRPLAY_REQ_FLAGS` bit 0. Off by default; the phone sent HEVC only with this AND a 4K canvas.
    var hevc: Bool

    @ObservationIgnored private let defaults: UserDefaults?

    /// `defaults: nil` remembers nothing (tests, the synthetic run).
    init(defaults: UserDefaults?) {
        self.defaults = defaults
        ssid = defaults?.string(forKey: Self.ssidKey) ?? ""
        name = defaults?.string(forKey: Self.nameKey) ?? ""
        channel = defaults?.string(forKey: Self.channelKey) ?? ""
        display = defaults?.string(forKey: Self.displayKey).flatMap(AirPlayModeRequest.Display.init(text:)) ?? .measuredDefault
        hevc = defaults?.bool(forKey: Self.hevcKey) ?? false
    }

    func remember() {
        defaults?.set(ssid, forKey: Self.ssidKey)
        defaults?.set(name, forKey: Self.nameKey)
        defaults?.set(channel, forKey: Self.channelKey)
        defaults?.set(display.text, forKey: Self.displayKey)
        defaults?.set(hevc, forKey: Self.hevcKey)
    }

    /// The request these fields and `passphrase` make, or the reason there is none.
    func request(passphrase: String) -> Result<AirPlayModeRequest, AirPlayModeRequest.Invalid> {
        let ch = channel.trimmingCharacters(in: .whitespaces)
        var number: UInt8 = 0
        if !ch.isEmpty {
            guard let n = UInt8(ch) else { return .failure(.channel) }
            number = n
        }
        let req = AirPlayModeRequest(ssid: ssid, psk: passphrase, name: name, channel: number, display: display, hevc: hevc)
        if let bad = req.validate() { return .failure(bad) }
        return .success(req)
    }
}

@MainActor
@Observable
final class LinkStatus {
    /// The user's intent: Start was pressed and Stop was not. Drives the button.
    var running = false
    /// A leave is on its way to the box; the button waits for it.
    var stopping = false
    var boxPresent = false
    var link = "no box on USB"
    var phase = "idle"
    var lastError = ""
    var caps = "-"
    var bundle = "-"
    var bundleState = "-"
    var activeMode = "-"
    var ap = "-"
    var session = "-"
    var lastEnd = "-"
    var sender = "-"
    /// What the running request asked for: "3840x2160@60, HEVC allowed".
    var requested = "-"
    var bytesReceived: UInt64 = 0
    var frames: UInt64 = 0
    var resyncs: UInt64 = 0
}

@MainActor
@Observable
final class StreamsStatus {
    var rows: [MediaPipeline.StreamInfo] = []
}

@MainActor
@Observable
final class VideoStatus {
    var decoded: UInt64 = 0
    /// Decoded pictures per second over the last 2 s (`VideoTimingMeter`).
    var fps = 0.0
    var averageFPS = 0.0
    /// Frames per second by the phone's header timestamps.
    var streamFPS = 0.0
    var jitterMs = 0.0
    var jitterPeakMs = 0.0
    /// "0.4 s ago", or "-" before the first picture.
    var lastFrame = "-"
    var decodeErrors: UInt64 = 0
    var lastError: Int32 = 0
    /// "HEVC Main L5.1, hardware" / "-".
    var codec = "-"
    /// Why the config frame could not be used; empty when it could.
    var codecProblem = ""
    var codedSize = "-"
    /// "Smooth playback" (timestamp pacing) is on.
    var smooth = false
    /// "on decode" / "100 ms buffer: 3 late, 0 dropped".
    var playback = "on decode"
    var headerSize = "-"
    var decrypted: UInt64 = 0
    var undecryptable: UInt64 = 0
    var gaps: UInt64 = 0
    var framesLost: UInt64 = 0
    var keyframes: UInt64 = 0
    var unconfigured: UInt64 = 0
    var lastSeq = "-"
    /// A picture is on screen.
    var showing = false
}

@MainActor
@Observable
final class AudioStatus {
    var stream = "-"
    var packets: UInt64 = 0
    var undecryptable: UInt64 = 0
    var decoded: UInt64 = 0
    var decodeErrors: UInt64 = 0
    var underruns: UInt64 = 0
    var trims: UInt64 = 0
    var stale: UInt64 = 0
    var queuedMs = 0
    var backlog = 0
    var playedSeconds = 0.0
    var output = "-"
}

@MainActor
@Observable
final class MetadataStatus {
    var title = "-"
    var artist = "-"
    var album = "-"
    var volume = "-"
    var artwork: CGImage?
}

struct LogLine: Identifiable, Equatable, Sendable {
    enum Source: Equatable, Sendable { case host, box }
    let id: Int
    let time: String
    let source: Source
    let text: String
}

@MainActor
@Observable
final class LogModel {
    static let capacity = 800
    private(set) var lines: [LogLine] = []
    @ObservationIgnored private var nextID = 0
    @ObservationIgnored private let clock: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss.SSS"
        return f
    }()

    func add(_ text: String, source: LogLine.Source = .host) {
        nextID += 1
        lines.append(LogLine(id: nextID, time: clock.string(from: Date()), source: source, text: text))
        if lines.count > Self.capacity { lines.removeFirst(lines.count - Self.capacity) }
    }

    func clear() { lines = [] }
}
