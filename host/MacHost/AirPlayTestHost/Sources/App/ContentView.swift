// ContentView.swift — the one window: the picture with the log under it on the left; on the right the
// fields, Start / Stop and the status panel.
//
// Structure follows the invalidation: each section is its own View reading its own @Observable model
// (StatusModels.swift), so the counters that change twice a second redraw their own rows and nothing
// else, and typing in a field does not touch the status panel.

import SwiftUI

struct ContentView: View {
    let controller: SessionController
    let surface: VideoSurface
    /// Debug bench runs: the passphrase the app delegate started with, so the field shows it is set.
    let autostart: String?

    var body: some View {
        HStack(spacing: 0) {
            VStack(spacing: 0) {
                VideoPane(surface: surface, video: controller.video, link: controller.link)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                Divider()
                LogPane(log: controller.log)
                    .frame(height: 190)
            }
            .frame(minWidth: 420, maxWidth: .infinity)
            Divider()
            VStack(spacing: 0) {
                ConnectionForm(form: controller.form, link: controller.link, video: controller.video,
                               initialPassphrase: autostart ?? "",
                               onStart: { controller.start(passphrase: $0) }, onStop: { controller.stop() },
                               onSmooth: { controller.setSmoothPlayback($0) })
                    .padding(12)
                Divider()
                StatusPanel(controller: controller)
            }
            .frame(width: 430)
        }
        .frame(minWidth: 900, minHeight: 700)
    }
}

// MARK: - Picture

private struct VideoPane: View {
    let surface: VideoSurface
    let video: VideoStatus
    let link: LinkStatus

    var body: some View {
        ZStack {
            Color.black
            VideoSurfaceView(surface: surface)
            if !video.showing {
                VideoPlaceholder(running: link.running, phase: link.phase, session: link.session)
            }
        }
    }
}

private struct VideoPlaceholder: View {
    let running: Bool
    let phase: String
    let session: String

    var body: some View {
        VStack(spacing: 6) {
            Image(systemName: "rectangle.on.rectangle.slash")
                .font(.system(size: 34))
            Text(running ? "No screen stream" : "Not started")
                .font(.headline)
            Text(verbatim: running ? "\(phase) · session \(session)" : "Enter the AP's SSID and passphrase, then Start")
                .font(.callout)
        }
        .foregroundStyle(.secondary)
        .multilineTextAlignment(.center)
        .padding()
    }
}

// MARK: - Fields

private struct ConnectionForm: View {
    @Bindable var form: FormModel
    let link: LinkStatus
    let video: VideoStatus
    let onStart: (String) -> Void
    let onStop: () -> Void
    let onSmooth: (Bool) -> Void
    /// Memory only: this view's state and, while a session runs, the request. Never stored, never logged.
    @State private var passphrase: String

    init(form: FormModel, link: LinkStatus, video: VideoStatus, initialPassphrase: String,
         onStart: @escaping (String) -> Void, onStop: @escaping () -> Void, onSmooth: @escaping (Bool) -> Void) {
        self.form = form
        self.link = link
        self.video = video
        self.onStart = onStart
        self.onStop = onStop
        self.onSmooth = onSmooth
        _passphrase = State(initialValue: initialPassphrase)
    }

    /// The presets, plus the current value when a launch argument set another one.
    private var displays: [AirPlayModeRequest.Display] {
        let presets = AirPlayModeRequest.Display.presets
        return presets.contains(form.display) ? presets : presets + [form.display]
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 8, verticalSpacing: 6) {
                GridRow {
                    Text("SSID")
                    TextField("the AP the box raises", text: $form.ssid)
                }
                GridRow {
                    Text("Passphrase")
                    SecureField("8 to 63 characters, not saved", text: $passphrase)
                }
                GridRow {
                    Text("Receiver name")
                    TextField("the SSID", text: $form.name)
                }
                GridRow {
                    Text("Channel")
                    TextField("box default (36)", text: $form.channel)
                        .frame(width: 130)
                }
                GridRow {
                    Text("Display")
                    Picker("Display", selection: $form.display) {
                        ForEach(displays, id: \.self) { d in
                            Text(verbatim: "\(d.width) × \(d.height) at \(d.maxFPS) fps").tag(d)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                    .fixedSize()
                }
                GridRow {
                    Text("Codec")
                    Toggle("Allow HEVC", isOn: $form.hevc)
                }
            }
            .textFieldStyle(.roundedBorder)
            .disabled(link.running)

            if form.hevc && !form.display.isAbove1080p {
                Text("Measured: the phone sends HEVC only with a canvas above 1080p.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            // Not part of the request: it can be switched while a stream plays, to compare.
            Toggle("Smooth playback (pace by timestamps, 100 ms buffer)", isOn: Binding(
                get: { video.smooth },
                set: { onSmooth($0) }))

            HStack(spacing: 10) {
                Button(link.running ? "Stop" : "Start") {
                    if link.running { onStop() } else { onStart(passphrase) }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(link.stopping)
                Text(verbatim: link.phase)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            if !link.lastError.isEmpty {
                Text(verbatim: link.lastError)
                    .foregroundStyle(.red)
                    .font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

// MARK: - Status

private struct StatusPanel: View {
    let controller: SessionController

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                BoxSection(link: controller.link)
                ModeSection(link: controller.link)
                StreamsSection(streams: controller.streams)
                VideoSection(video: controller.video)
                AudioSection(audio: controller.audio)
                MetadataSection(meta: controller.meta)
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A titled block of label/value rows.
private struct StatusSection<Rows: View>: View {
    let title: LocalizedStringKey
    @ViewBuilder let rows: Rows

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title)
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .textCase(.uppercase)
            Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 10, verticalSpacing: 2) {
                rows
            }
        }
    }
}

private struct StatusRow: View {
    let label: LocalizedStringKey
    let value: String
    var alert = false

    var body: some View {
        GridRow {
            Text(label)
                .foregroundStyle(.secondary)
                .gridColumnAlignment(.leading)
            Text(verbatim: value)
                .foregroundStyle(alert ? AnyShapeStyle(.red) : AnyShapeStyle(.primary))
                .monospacedDigit()
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .font(.callout)
    }
}

private struct BoxSection: View {
    let link: LinkStatus

    var body: some View {
        StatusSection(title: "Box") {
            StatusRow(label: "Link", value: link.link)
            StatusRow(label: "Capabilities", value: link.caps)
            StatusRow(label: "Bundle", value: link.bundle)
            StatusRow(label: "Bundle state", value: link.bundleState, alert: link.bundleState == "mixed" || link.bundleState == "corrupt")
            StatusRow(label: "Mode at HELLO", value: link.activeMode)
            StatusRow(label: "Received", value: "\(link.bytesReceived.formatted(.byteCount(style: .binary))) in \(link.frames) frames")
            StatusRow(label: "Lane resyncs", value: String(link.resyncs), alert: link.resyncs > 0)
        }
    }
}

private struct ModeSection: View {
    let link: LinkStatus

    var body: some View {
        StatusSection(title: "AirPlay-only mode") {
            StatusRow(label: "AP", value: link.ap, alert: link.ap == "failed")
            StatusRow(label: "Session", value: link.session)
            StatusRow(label: "Last end", value: link.lastEnd, alert: link.lastEnd == "error")
            StatusRow(label: "Requested", value: link.requested)
            StatusRow(label: "Sender", value: link.sender)
        }
    }
}

private struct StreamsSection: View {
    let streams: StreamsStatus

    var body: some View {
        StatusSection(title: "Streams") {
            if streams.rows.isEmpty {
                StatusRow(label: "Current", value: "none")
            } else {
                ForEach(streams.rows) { row in
                    StreamRow(lane: row.lane, type: row.type, scid: row.scid)
                }
            }
        }
    }
}

private struct StreamRow: View {
    let lane: String
    let type: Int?
    let scid: UInt64

    var body: some View {
        GridRow {
            Text(verbatim: type.map { "type \($0)" } ?? "type ?")
                .foregroundStyle(.secondary)
            Text(verbatim: "\(lane), scid \(scid)")
                .monospacedDigit()
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .font(.callout)
    }
}

private struct VideoSection: View {
    let video: VideoStatus

    var body: some View {
        StatusSection(title: "Video") {
            StatusRow(label: "Codec", value: video.codec)
            if !video.codecProblem.isEmpty {
                StatusRow(label: "Codec problem", value: video.codecProblem, alert: true)
            }
            StatusRow(label: "Decoded", value: "\(video.decoded) frames, \(Self.rate(video.fps)) fps (average \(Self.rate(video.averageFPS)))")
            StatusRow(label: "Stream rate", value: "\(Self.rate(video.streamFPS)) fps by timestamps, last picture \(video.lastFrame)")
            StatusRow(label: "Arrival jitter", value: "\(Self.rate(video.jitterMs)) ms (peak \(Self.rate(video.jitterPeakMs)) ms in 2 s)")
            StatusRow(label: "Playback", value: video.playback)
            StatusRow(label: "Decode errors",
                      value: video.decodeErrors == 0 ? "0" : "\(video.decodeErrors) (last OSStatus \(video.lastError))",
                      alert: video.decodeErrors > 0)
            StatusRow(label: "Coded size", value: video.codedSize)
            StatusRow(label: "Header size", value: video.headerSize)
            StatusRow(label: "Decrypted", value: "\(video.decrypted), \(video.keyframes) flagged IDR, last seq \(video.lastSeq)")
            StatusRow(label: "Not decrypted", value: String(video.undecryptable), alert: video.undecryptable > 0)
            StatusRow(label: "Seq jumps", value: "\(video.gaps) (\(video.framesLost) frames lost)", alert: video.gaps > 0)
            StatusRow(label: "Before a config", value: String(video.unconfigured), alert: video.unconfigured > 0)
        }
    }

    private static func rate(_ x: Double) -> String { x.formatted(.number.precision(.fractionLength(1))) }
}

private struct AudioSection: View {
    let audio: AudioStatus

    var body: some View {
        StatusSection(title: "Audio") {
            StatusRow(label: "Playing", value: audio.stream)
            StatusRow(label: "Packets", value: "\(audio.packets) decrypted, \(audio.undecryptable) not", alert: audio.undecryptable > 0)
            StatusRow(label: "Decoded", value: "\(audio.decoded) units, \(audio.decodeErrors) rejected")
            StatusRow(label: "Underruns", value: String(audio.underruns), alert: audio.underruns > 0)
            StatusRow(label: "Latency trims", value: "\(audio.trims), \(audio.stale) stale packets")
            StatusRow(label: "Queued", value: "\(audio.queuedMs) ms decoded, \(audio.backlog) units waiting")
            StatusRow(label: "Played", value: "\(audio.playedSeconds.formatted(.number.precision(.fractionLength(1)))) s of this stream")
            StatusRow(label: "Output", value: audio.output)
        }
    }
}

private struct MetadataSection: View {
    let meta: MetadataStatus

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            StatusSection(title: "Metadata") {
                StatusRow(label: "Title", value: meta.title)
                StatusRow(label: "Artist", value: meta.artist)
                StatusRow(label: "Album", value: meta.album)
                StatusRow(label: "Volume", value: meta.volume)
            }
            ArtworkThumbnail(image: meta.artwork)
        }
    }
}

private struct ArtworkThumbnail: View {
    let image: CGImage?

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 6)
                .fill(.quaternary)
            if let image {
                Image(decorative: image, scale: 1)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: 6))
            } else {
                Image(systemName: "music.note")
                    .foregroundStyle(.secondary)
            }
        }
        .frame(width: 72, height: 72)
        .accessibilityLabel(image == nil ? "No artwork" : "Artwork")
    }
}

// MARK: - Log

private struct LogPane: View {
    let log: LogModel

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Log")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .textCase(.uppercase)
                Spacer()
                Button("Clear") { log.clear() }
                    .controlSize(.small)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 4)
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 1) {
                        ForEach(log.lines) { line in
                            LogRow(time: line.time, text: line.text, fromBox: line.source == .box)
                                .id(line.id)
                        }
                    }
                    .padding(.horizontal, 12)
                    .padding(.bottom, 6)
                }
                .onChange(of: log.lines.last?.id) { _, last in
                    if let last { proxy.scrollTo(last, anchor: .bottom) }
                }
            }
        }
    }
}

private struct LogRow: View {
    let time: String
    let text: String
    let fromBox: Bool

    var body: some View {
        Text(verbatim: "\(time)  \(text)")
            .font(.system(.caption, design: .monospaced))
            .foregroundStyle(fromBox ? AnyShapeStyle(.secondary) : AnyShapeStyle(.primary))
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}
