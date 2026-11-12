// BoxLink.swift — where the raw OCBM pipe comes from. The app has two: the box on USB (`USBBoxLink`) and,
// in debug builds, a synthetic box in memory (`SyntheticLink`). The controller sees only this.

import Foundation

@MainActor
protocol BoxLink: AnyObject {
    /// A box appeared or went away. Watching is passive: nothing is opened until `open()`.
    var onPresence: ((Bool) -> Void)? { get set }
    /// The pipe handed out by `open()` died under the client (unplug, bulk errors).
    var onLost: ((String) -> Void)? { get set }
    var isPresent: Bool { get }
    /// "USB 1314:2d00", "synthetic box".
    var label: String { get }
    func startWatching()
    /// Claim the pipe. Exclusive: another OCBM client on the same box loses it.
    func open() -> RawBulkTransport?
    /// The client is done with the pipe it was given (it stops the transport itself).
    func closed()
}
