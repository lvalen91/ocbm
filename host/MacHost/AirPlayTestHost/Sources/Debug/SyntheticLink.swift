// SyntheticLink.swift — DEBUG ONLY: a `BoxLink` whose box is a `SyntheticBox`. Used by the tests and by
// the app when it is launched with `-synthetic-h264 FILE -synthetic-aac FILE`, in which case no USB
// API is called at all. `unplugAfter` pulls the box out once, mid-session, and puts a new one back.

#if DEBUG

import Foundation

@MainActor
final class SyntheticLink: BoxLink {
    var onPresence: ((Bool) -> Void)?
    var onLost: ((String) -> Void)?
    private(set) var isPresent = true
    let label = "synthetic box"

    private let media: SyntheticBox.Media
    private let options: SyntheticBox.Options
    /// Seconds after the first open at which the box is unplugged for `unplugFor` seconds (0 = never).
    private let unplugAfter: Double
    private let unplugFor: Double
    private var unplugged = false
    /// Every box handed out, newest last (the tests read what each one saw).
    private(set) var boxes: [SyntheticBox] = []

    init(media: SyntheticBox.Media, options: SyntheticBox.Options = .init(), unplugAfter: Double = 0, unplugFor: Double = 2) {
        self.media = media
        self.options = options
        self.unplugAfter = unplugAfter
        self.unplugFor = unplugFor
    }

    func startWatching() {}

    func open() -> RawBulkTransport? {
        guard isPresent else { return nil }
        let box = SyntheticBox(media: media, options: options)
        boxes.append(box)
        if unplugAfter > 0 && !unplugged {
            unplugged = true
            DispatchQueue.main.asyncAfter(deadline: .now() + unplugAfter) { [weak self] in
                MainActor.assumeIsolated { self?.unplug(box) }
            }
        }
        return box
    }

    func closed() {}

    private func unplug(_ box: SyntheticBox) {
        box.kill()
        isPresent = false
        onPresence?(false)
        onLost?("synthetic unplug")
        DispatchQueue.main.asyncAfter(deadline: .now() + unplugFor) { [weak self] in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.isPresent = true
                self.onPresence?(true)
            }
        }
    }
}

/// A link with no box on it, for a synthetic run that could not load its media.
@MainActor
final class AbsentLink: BoxLink {
    var onPresence: ((Bool) -> Void)?
    var onLost: ((String) -> Void)?
    let isPresent = false
    let label = "no box (synthetic run)"
    func startWatching() {}
    func open() -> RawBulkTransport? { nil }
    func closed() {}
}

#endif
