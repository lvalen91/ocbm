// USBBoxLink.swift — the box on USB, through the shipping app's own IOKit layer, unchanged:
// `USBDeviceManager` (attach / detach notifications), `USBInterfaceClaimer` (open + claim the vendor
// interface), `USBTransport` (the bulk pipe, a `RawBulkTransport`).
//
// Watching is passive (IOKit matching notifications; no device is opened). `open()` is not: it takes
// the interface, seizing it from any other OCBM client (`ocbm-host`, the carlink app). That is why this
// app opens the box only on Start and lets it go on Stop.

import Foundation
import IOKit
import IOKit.usb

@MainActor
final class USBBoxLink: BoxLink {
    /// The identities an OCBM accessory enumerates with (the OCBM rows of `kSupportedDevices`). The
    /// shared table also matches stock Carlinkit firmware, which does not speak OCBM: not opened here.
    private static let ocbmIDs: [(vendor: Int, product: Int)] = [(0x1314, 0x2d00), (0x1f3a, 0xace2)]

    var onPresence: ((Bool) -> Void)?
    var onLost: ((String) -> Void)?
    private(set) var isPresent = false
    private(set) var label = "USB"

    private let manager = USBDeviceManager()
    private let bridge = Bridge()
    private var service: io_service_t = 0
    private var transport: USBTransport?

    init() {
        bridge.owner = self
        manager.delegate = bridge
    }

    func startWatching() { manager.startScanning() }

    func open() -> RawBulkTransport? {
        guard service != 0, let claimed = USBInterfaceClaimer.claim(deviceService: service) else { return nil }
        let t = USBTransport(interfaceRef: claimed.interfaceRef, deviceRef: claimed.deviceRef,
                             bulkIn: claimed.bulkInPipe, bulkOut: claimed.bulkOutPipe)
        t.delegate = bridge
        transport = t
        return t
    }

    func closed() { transport = nil }

    // MARK: - From the bridge (main queue)

    /// `device` arrives retained (USBDeviceManager's contract): released here or on detach.
    fileprivate func attached(_ device: io_service_t) {
        func number(_ key: String) -> Int? {
            IORegistryEntryCreateCFProperty(device, key as CFString, kCFAllocatorDefault, 0)?.takeRetainedValue() as? Int
        }
        let vid = number("idVendor") ?? 0, pid = number("idProduct") ?? 0
        guard Self.ocbmIDs.contains(where: { $0.vendor == vid && $0.product == pid }) else {
            IOObjectRelease(device)
            return
        }
        if service != 0 { IOObjectRelease(service) }
        service = device
        label = String(format: "USB %04x:%04x", vid, pid)
        isPresent = true
        onPresence?(true)
    }

    fileprivate func detached() {
        if service != 0 { IOObjectRelease(service) }
        service = 0
        guard isPresent else { return }
        isPresent = false
        onPresence?(false)
        if transport != nil {
            transport = nil
            onLost?("the box was unplugged")
        }
    }

    fileprivate func pipeFailed(_ id: ObjectIdentifier) {
        guard let t = transport, ObjectIdentifier(t) == id else { return }
        transport = nil
        onLost?("USB bulk pipe errors")
    }
}

/// The IOKit layer calls its delegates on its own queues; this carries them to the main queue.
private final class Bridge: USBDeviceManagerDelegate, USBTransportDelegate, @unchecked Sendable {
    /// Set once, on the main queue, before scanning starts.
    weak var owner: USBBoxLink?

    private func onMain(_ work: @escaping @MainActor (USBBoxLink) -> Void) {
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated {
                if let owner = self?.owner { work(owner) }
            }
        }
    }

    func usbDeviceDidConnect(device: io_service_t) { onMain { $0.attached(device) } }
    func usbDeviceDidDisconnect() { onMain { $0.detached() } }
    func transportDidEncounterError(_ transport: USBTransport, error: Error) {}
    func transportDidDisconnect(_ transport: USBTransport) {
        let id = ObjectIdentifier(transport)
        onMain { $0.pipeFailed(id) }
    }
}
