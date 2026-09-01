// ──────────────────────────────────────────────────────────────────────────────
// USBDeviceManager.swift — CPC200-CCPA device discovery & interface claiming
// ──────────────────────────────────────────────────────────────────────────────
//
// Why legacy IOKit instead of IOUSBHost.framework
// ────────────────────────────────────────────────
// The CPC200-CCPA (Carlinkit) adapter exposes a proprietary bulk-transfer USB
// protocol — not a standard USB device class. Talking to it requires:
//
//   1. Seizing exclusive device access (USBDeviceOpenSeize) — if a previous
//      app session crashed, the OS still holds the device open. The legacy
//      IOKit API lets us forcibly reclaim it. IOUSBHost.framework requires the
//      com.apple.vm.device-access entitlement for .deviceCapture, which needs
//      a provisioning profile signed by Apple.
//
//   2. Forcing re-enumeration (USBDeviceReEnumerate) — on launch we reset any
//      attached adapter so the firmware returns to its initial state (idle,
//      phase 0). This is critical because the firmware has a 10-second watchdog
//      from enumeration to first heartbeat. There is no IOUSBHost equivalent
//      for triggering re-enumeration from user space.
//
//   3. Synchronous bulk pipe I/O (ReadPipe / WritePipe) — the adapter streams
//      H.264 video and raw PCM audio over bulk endpoints. A blocking read loop
//      on a dedicated thread is the simplest way to consume this high-throughput
//      data. IOUSBHostPipe uses an async completion-handler model that adds
//      complexity without benefit for continuous streaming.
//
// The IOKit USB API is a COM-style C interface. Swift interacts with it through
// double pointers: iface.pointee.pointee.Method(iface, ...). The CFUUID
// constants needed to acquire these interfaces (kIOUSBDeviceUserClientTypeID,
// kIOUSBInterfaceInterfaceID300, etc.) are C preprocessor macros that expand to
// CFUUIDGetConstantUUIDWithBytes() — invisible to Swift. The USBBridge.h
// bridging header wraps each one as a plain C function returning CFUUIDBytes.
//
// Build warnings (expected, harmless)
// ────────────────────────────────────
//   • "Unused IOReturn/ULONG result" — IOKit COM Release() and Close() return
//     ref counts and status codes. In cleanup paths there is nothing to recover
//     if these fail, so the results are intentionally discarded.
//   • "Capture of non-Sendable type" — IONotificationPortRef and io_iterator_t
//     are opaque C types that cannot conform to Sendable. Thread safety is
//     guaranteed by dispatching all notifications onto the serial notifyQueue.
// ──────────────────────────────────────────────────────────────────────────────

import Foundation
import IOKit
import IOKit.usb
import os

// MARK: - Supported USB Device IDs
//
// Relocated from the deleted Protocol/MessageTypes.swift: USBDeviceManager is the only live user
// (device discovery matches vendor/product against this table).
struct USBDeviceID: Sendable {
    let vendorID: Int
    let productID: Int
}

let kSupportedDevices: [USBDeviceID] = [
    USBDeviceID(vendorID: 0x1314, productID: 0x2d00), // OCBM accessory ("Auto Box") — the committed model
    USBDeviceID(vendorID: 0x1314, productID: 0x1520), // original CCPA (riddlebox) — legacy
    USBDeviceID(vendorID: 0x1314, productID: 0x1521),
    USBDeviceID(vendorID: 0x08E4, productID: 0x01C0),
    // C2Air (Allwinner V821, riscv32) running OCBM. It keeps the ids its own rc.preboot assigns
    // rather than borrowing the CCPA's 1314:2d00 — nothing in OCBM requires a particular VID/PID
    // (ocbm-host takes them as arguments), and reusing the CCPA's would make the two boxes
    // indistinguishable to this table. Device-verified 2026-08-17: HELLO_ACK caps=0x3f, MFi cert +
    // signature, and a root console, all over this id pair. See c2air/README.md.
    USBDeviceID(vendorID: 0x1f3a, productID: 0xace2), // C2Air V821 — OCBM accessory
]

// MARK: - USB Device Manager Delegate

protocol USBDeviceManagerDelegate: AnyObject {
    func usbDeviceDidConnect(device: io_service_t)
    func usbDeviceDidDisconnect()
}

// MARK: - USB Device Manager

/// Discovers CPC200-CCPA adapters via IOKit notifications and claims USB interfaces.
/// `@unchecked Sendable`: all mutable state is funnelled through `notifyQueue` (verified), so the
/// type is safe to hand to a background `DispatchQueue.global().async` even though the compiler
/// can't prove it — this makes that capture explicit rather than a strict-concurrency warning.
final class USBDeviceManager: @unchecked Sendable {

    private static let logger = Logger(subsystem: "com.carlink.usb", category: "DeviceManager")

    // Wiring order is currently safe (the delegate is set before startScanning), so pendingDelivery
    // is hardening: if an attach ever lands while the delegate is still nil, remember it and deliver
    // the moment the delegate is set (didSet hops to notifyQueue, where all state lives).
    weak var delegate: USBDeviceManagerDelegate? {
        didSet {
            notifyQueue.async { [weak self] in
                guard let self, self.pendingDelivery, self.currentDeviceService != 0,
                      let delegate = self.delegate else { return }
                self.pendingDelivery = false
                Self.logger.info("Delivering deferred attach (device arrived before delegate was set)")
                _ = IOObjectRetain(self.currentDeviceService)
                delegate.usbDeviceDidConnect(device: self.currentDeviceService)
            }
        }
    }

    // All mutable state below is confined to notifyQueue: the IOKit handlers
    // already run there, and startScanning()/stopScanning() hop onto it, so
    // attach/detach callbacks can never race setup or teardown.
    private var notifyPort: IONotificationPortRef?
    private var matchIterators: [io_iterator_t] = []
    private var removeIterators: [io_iterator_t] = []
    private let notifyQueue = DispatchQueue(label: "usb.notify", qos: .userInitiated)

    private var currentDeviceService: io_service_t = 0
    /// notifyQueue-confined: an attach arrived while `delegate` was nil and awaits delivery.
    private var pendingDelivery = false

    deinit {
        stopScanning()
    }

    // MARK: - USB Reset

    /// Resets any attached CPC200-CCPA adapter so its firmware returns to the idle state
    /// (phase 0). This serves two purposes:
    ///   1. Releases stale exclusive access if a previous app session crashed without
    ///      closing the USB device — the OS will still hold it open otherwise.
    ///   2. Forces the firmware to restart its 10-second init watchdog, giving us a
    ///      clean window to send Open (0x01) and begin the heartbeat.
    /// Called before startScanning() on each launch, and again by
    /// reinitializeAdapterSession (resolution changes) to force the attached
    /// adapter back through enumeration mid-run.
    func resetDevicesOnLaunch() {
        for deviceID in kSupportedDevices {
            guard let dict = createMatchingDict(vendorID: deviceID.vendorID,
                                                productID: deviceID.productID) else {
                Self.logger.warning("resetDevicesOnLaunch: matching dict creation failed for VID=\(UInt(bitPattern: deviceID.vendorID), format: .hex, privacy: .public) PID=\(UInt(bitPattern: deviceID.productID), format: .hex, privacy: .public)")
                continue
            }
            let service = IOServiceGetMatchingService(kIOMainPortDefault, dict as CFDictionary)
            guard service != 0 else {
                Self.logger.debug("resetDevicesOnLaunch: no attached device VID=\(UInt(bitPattern: deviceID.vendorID), format: .hex, privacy: .public) PID=\(UInt(bitPattern: deviceID.productID), format: .hex, privacy: .public) — nothing to reset")
                continue
            }
            defer { IOObjectRelease(service) }

            // Acquire a temporary device interface just to call ReEnumerate.
            // This COM dance (PlugIn → QueryInterface → typed pointer) is the
            // only way IOKit exposes USB device control from user space.
            var pluginInterface: UnsafeMutablePointer<UnsafeMutablePointer<IOCFPlugInInterface>?>?
            var score: Int32 = 0
            let kr = CreatePlugInForUSBDevice(service, &pluginInterface, &score)
            guard kr == KERN_SUCCESS, let plugin = pluginInterface else {
                Self.logger.warning("resetDevicesOnLaunch: CreatePlugInForUSBDevice failed (\(kr, privacy: .public)) — device NOT reset")
                continue
            }

            var rawPtr: UnsafeMutableRawPointer?
            let uuid = GetIOUSBDeviceInterfaceID300Bytes()
            _ = plugin.pointee?.pointee.QueryInterface(plugin, uuid, &rawPtr)
            _ = plugin.pointee?.pointee.Release(plugin)

            guard let raw = rawPtr else {
                Self.logger.warning("resetDevicesOnLaunch: QueryInterface for IOUSBDeviceInterface300 failed — device NOT reset")
                continue
            }
            let dev = raw.assumingMemoryBound(to: UnsafeMutablePointer<IOUSBDeviceInterface300>.self)

            // USBDeviceOpenSeize forcibly takes the device even if another process
            // (or a stale handle from our own previous run) holds exclusive access.
            // Once open, ReEnumerate tells the firmware to simulate a USB disconnect/
            // reconnect cycle, which resets its internal state machine to phase 0.
            let openResult = dev.pointee.pointee.USBDeviceOpenSeize(dev)
            if openResult == kIOReturnSuccess {
                let reEnum = dev.pointee.pointee.USBDeviceReEnumerate(dev, 0)
                Self.logger.info("Re-enumerated device VID=\(UInt(bitPattern: deviceID.vendorID), format: .hex, privacy: .public) PID=\(UInt(bitPattern: deviceID.productID), format: .hex, privacy: .public): \(UInt32(bitPattern: reEnum), format: .hex, privacy: .public)")
                dev.pointee.pointee.USBDeviceClose(dev)
            } else {
                Self.logger.warning("resetDevicesOnLaunch: USBDeviceOpenSeize failed (\(UInt32(bitPattern: openResult), format: .hex, privacy: .public)) — device NOT reset")
            }
            _ = dev.pointee.pointee.Release(dev)
        }

        // Wait for the USB bus to settle after re-enumeration. The adapter's
        // NXP i.MX6UL SoC needs time to re-initialize its USB gadget driver.
        // Firmware RE docs recommend ~3 seconds for reliable re-enumeration.
        Thread.sleep(forTimeInterval: 3.0)
    }

    // MARK: - Scanning

    func startScanning() {
        notifyQueue.sync { startScanningLocked() }
    }

    private func startScanningLocked() {
        guard notifyPort == nil else { return }
        notifyPort = IONotificationPortCreate(kIOMainPortDefault)
        guard let port = notifyPort else {
            Self.logger.error("Failed to create notification port")
            return
        }
        IONotificationPortSetDispatchQueue(port, notifyQueue)

        let selfPtr = Unmanaged.passUnretained(self).toOpaque()

        for deviceID in kSupportedDevices {
            // IOServiceAddMatchingNotification consumes the matching dict,
            // so we must create a fresh one for each call.
            guard let matchDictAttach = createMatchingDict(vendorID: deviceID.vendorID,
                                                           productID: deviceID.productID),
                  let matchDictDetach = createMatchingDict(vendorID: deviceID.vendorID,
                                                           productID: deviceID.productID) else {
                continue
            }

            // Attach notification
            var iter: io_iterator_t = 0
            let kr = IOServiceAddMatchingNotification(
                port,
                kIOFirstMatchNotification,
                matchDictAttach as CFDictionary,
                { refCon, iterator in
                    let mgr = Unmanaged<USBDeviceManager>.fromOpaque(refCon!).takeUnretainedValue()
                    mgr.handleDeviceAttached(iterator: iterator)
                },
                selfPtr,
                &iter
            )

            if kr == KERN_SUCCESS {
                handleDeviceAttached(iterator: iter) // drain to arm
                matchIterators.append(iter)
            }

            // Detach notification
            var removeIter: io_iterator_t = 0
            let kr2 = IOServiceAddMatchingNotification(
                port,
                kIOTerminatedNotification,
                matchDictDetach as CFDictionary,
                { refCon, iterator in
                    let mgr = Unmanaged<USBDeviceManager>.fromOpaque(refCon!).takeUnretainedValue()
                    mgr.handleDeviceRemoved(iterator: iterator)
                },
                selfPtr,
                &removeIter
            )

            if kr2 == KERN_SUCCESS {
                handleDeviceRemoved(iterator: removeIter) // drain to arm
                removeIterators.append(removeIter)
            }
        }

        Self.logger.info("Scanning for CPC200-CCPA adapters...")
    }

    func stopScanning() {
        notifyQueue.sync {
            for iter in matchIterators { IOObjectRelease(iter) }
            matchIterators.removeAll()
            for iter in removeIterators { IOObjectRelease(iter) }
            removeIterators.removeAll()
            if let port = notifyPort { IONotificationPortDestroy(port); notifyPort = nil }
            if currentDeviceService != 0 { IOObjectRelease(currentDeviceService); currentDeviceService = 0 }
            pendingDelivery = false
        }
    }

    // MARK: - Notification Handlers

    private func handleDeviceAttached(iterator: io_iterator_t) {
        while case let service = IOIteratorNext(iterator), service != 0 {
            Self.logger.info("Device attached: service=\(service, format: .hex, privacy: .public)")
            if currentDeviceService != 0 {
                Self.logger.warning("Second adapter attached (service=\(service, format: .hex, privacy: .public)) — ignored; one active adapter is supported")
                IOObjectRelease(service)
                continue
            }
            currentDeviceService = service
            // The delegate consumes the service asynchronously (main-actor
            // hop), and this manager may release currentDeviceService on a
            // detach in the meantime — hand the delegate its own retained
            // reference, which the delegate must IOObjectRelease when done.
            // Retain only when a delegate exists, or the extra ref leaks.
            if let delegate {
                _ = IOObjectRetain(service)
                delegate.usbDeviceDidConnect(device: service)
            } else {
                // No delegate yet — remember the attach; delegate.didSet delivers it (C10 hardening).
                pendingDelivery = true
                Self.logger.warning("Device attached before delegate was set — deferring delivery")
            }
        }
    }

    private func handleDeviceRemoved(iterator: io_iterator_t) {
        var removedCurrent = false
        while case let service = IOIteratorNext(iterator), service != 0 {
            Self.logger.info("Device removed: service=\(service, format: .hex, privacy: .public)")
            // Only tear down the session when the terminated device is the one
            // we are actually using. Handles are distinct mach port names, so
            // compare identity via IOKit rather than by value.
            if currentDeviceService != 0, IOObjectIsEqualTo(service, currentDeviceService) != 0 {
                removedCurrent = true
            }
            IOObjectRelease(service)
        }
        if removedCurrent {
            IOObjectRelease(currentDeviceService)
            currentDeviceService = 0
            pendingDelivery = false // an undelivered attach for this device is now moot
            delegate?.usbDeviceDidDisconnect()
        }
    }

    // MARK: - Matching Dictionary

    private func createMatchingDict(vendorID: Int, productID: Int) -> NSDictionary? {
        let dict = IOServiceMatching(kIOUSBDeviceClassName) as NSMutableDictionary?
        dict?[kUSBVendorID] = vendorID
        dict?[kUSBProductID] = productID
        return dict
    }
}

// MARK: - USB Interface Claimer

/// Claims the CPC200-CCPA's USB interface and discovers its bulk endpoints.
///
/// The adapter exposes a single USB configuration with one interface containing
/// two bulk endpoints: IN (device→host: video, audio, metadata) and OUT
/// (host→device: heartbeat, commands, microphone). This class walks the IOKit
/// COM chain to claim that interface and return the pipe indices for USBTransport.
final class USBInterfaceClaimer {

    private static let logger = Logger(subsystem: "com.carlink.usb", category: "InterfaceClaimer")

    struct ClaimedInterface {
        let interfaceRef: UnsafeMutablePointer<UnsafeMutablePointer<IOUSBInterfaceInterface300>>
        /// The opened device interface. Ownership transfers to USBTransport,
        /// which closes and releases it (together with interfaceRef) in stop().
        let deviceRef: UnsafeMutablePointer<UnsafeMutablePointer<IOUSBDeviceInterface300>>
        let bulkInPipe: UInt8
        let bulkOutPipe: UInt8
    }

    static func claim(deviceService: io_service_t) -> ClaimedInterface? {
        // Step 1: Create a PlugIn for the USB device service.
        // IOKit's COM model requires this intermediate object to QueryInterface
        // for the actual typed device interface (IOUSBDeviceInterface300).
        var pluginInterface: UnsafeMutablePointer<UnsafeMutablePointer<IOCFPlugInInterface>?>?
        var score: Int32 = 0

        let kr = CreatePlugInForUSBDevice(deviceService, &pluginInterface, &score)
        guard kr == KERN_SUCCESS, let plugin = pluginInterface else {
            logger.error("Failed to create plugin interface: \(kr, privacy: .public)")
            return nil
        }

        // Step 2: QueryInterface for IOUSBDeviceInterface300 using its CFUUID.
        // The "300" variant supports USB 2.0 features we need (bulk transfers,
        // configuration management). The UUID comes from USBBridge.h.
        var deviceInterfacePtr: UnsafeMutableRawPointer?
        let uuidDevice = GetIOUSBDeviceInterfaceID300Bytes()
        let hresult = plugin.pointee?.pointee.QueryInterface(
            plugin,
            uuidDevice,
            &deviceInterfacePtr
        )
        plugin.pointee?.pointee.Release(plugin)

        guard hresult == S_OK, let rawDevice = deviceInterfacePtr else {
            logger.error("Failed to get device interface")
            return nil
        }

        let device = rawDevice.assumingMemoryBound(
            to: UnsafeMutablePointer<IOUSBDeviceInterface300>.self
        )

        // Step 3: Open the device. If another process (or a stale handle from a
        // previous crash) holds exclusive access, escalate to OpenSeize.
        var openResult = device.pointee.pointee.USBDeviceOpen(device)
        if openResult == kIOReturnExclusiveAccess {
            openResult = device.pointee.pointee.USBDeviceOpenSeize(device)
        }
        guard openResult == kIOReturnSuccess else {
            logger.error("Failed to open device: \(UInt32(bitPattern: openResult), format: .hex, privacy: .public)")
            device.pointee.pointee.Release(device)
            return nil
        }

        // Step 4: Activate USB configuration 1 (the only config the CPC200 exposes).
        // May return an error if already set — safe to ignore.
        let configResult = device.pointee.pointee.SetConfiguration(device, 1)
        if configResult != kIOReturnSuccess {
            logger.warning("SetConfiguration failed: \(UInt32(bitPattern: configResult), format: .hex, privacy: .public) (may be ok if already set)")
        }

        // Step 5: Find the first USB interface. The CPC200 uses interface 0 for
        // all bulk data (video, audio, commands). We use "don't care" filters
        // because the adapter doesn't conform to a standard USB class.
        var request = IOUSBFindInterfaceRequest(
            bInterfaceClass: UInt16(kIOUSBFindInterfaceDontCare),
            bInterfaceSubClass: UInt16(kIOUSBFindInterfaceDontCare),
            bInterfaceProtocol: UInt16(kIOUSBFindInterfaceDontCare),
            bAlternateSetting: UInt16(kIOUSBFindInterfaceDontCare)
        )

        var interfaceIterator: io_iterator_t = 0
        let findResult = device.pointee.pointee.CreateInterfaceIterator(device, &request, &interfaceIterator)
        guard findResult == kIOReturnSuccess else {
            logger.error("CreateInterfaceIterator failed: \(UInt32(bitPattern: findResult), format: .hex, privacy: .public)")
            device.pointee.pointee.USBDeviceClose(device)
            device.pointee.pointee.Release(device)
            return nil
        }

        let interfaceService = IOIteratorNext(interfaceIterator)
        IOObjectRelease(interfaceIterator)

        guard interfaceService != 0 else {
            logger.error("No interfaces found")
            device.pointee.pointee.USBDeviceClose(device)
            device.pointee.pointee.Release(device)
            return nil
        }

        // Step 6: Repeat the COM dance for the interface service — PlugIn first,
        // then QueryInterface for IOUSBInterfaceInterface300.
        var ifacePlugin: UnsafeMutablePointer<UnsafeMutablePointer<IOCFPlugInInterface>?>?
        var ifaceScore: Int32 = 0

        let kr2 = CreatePlugInForUSBInterface(interfaceService, &ifacePlugin, &ifaceScore)
        IOObjectRelease(interfaceService)

        guard kr2 == KERN_SUCCESS, let ifPlugin = ifacePlugin else {
            logger.error("Failed to create interface plugin: \(kr2, privacy: .public)")
            device.pointee.pointee.USBDeviceClose(device)
            device.pointee.pointee.Release(device)
            return nil
        }

        var ifaceRawPtr: UnsafeMutableRawPointer?
        let uuidIface = GetIOUSBInterfaceInterfaceID300Bytes()
        let hresult2 = ifPlugin.pointee?.pointee.QueryInterface(
            ifPlugin,
            uuidIface,
            &ifaceRawPtr
        )
        ifPlugin.pointee?.pointee.Release(ifPlugin)

        guard hresult2 == S_OK, let rawIface = ifaceRawPtr else {
            logger.error("Failed to get interface interface")
            device.pointee.pointee.USBDeviceClose(device)
            device.pointee.pointee.Release(device)
            return nil
        }

        let iface = rawIface.assumingMemoryBound(
            to: UnsafeMutablePointer<IOUSBInterfaceInterface300>.self
        )

        // Step 7: Open the interface for I/O. This grants exclusive access to
        // the interface's endpoints. If a stale user client (e.g. from a
        // crashed previous session) still holds it, seize it.
        var openIface = iface.pointee.pointee.USBInterfaceOpen(iface)
        if openIface == kIOReturnExclusiveAccess {
            openIface = iface.pointee.pointee.USBInterfaceOpenSeize(iface)
        }
        guard openIface == kIOReturnSuccess else {
            logger.error("Failed to open interface: \(UInt32(bitPattern: openIface), format: .hex, privacy: .public)")
            iface.pointee.pointee.Release(iface)
            device.pointee.pointee.USBDeviceClose(device)
            device.pointee.pointee.Release(device)
            return nil
        }

        // Step 8: Enumerate pipe properties to find the bulk IN and OUT endpoints.
        // The CPC200 exposes exactly two bulk pipes per interface:
        //   • Bulk IN  (direction=1): adapter → host (video, audio, metadata)
        //   • Bulk OUT (direction=0): host → adapter (heartbeat, open, mic, commands)
        var numEndpoints: UInt8 = 0
        iface.pointee.pointee.GetNumEndpoints(iface, &numEndpoints)
        logger.info("Interface has \(numEndpoints, privacy: .public) endpoints")

        guard numEndpoints > 0 else {
            logger.error("No endpoints found on interface")
            iface.pointee.pointee.USBInterfaceClose(iface)
            _ = iface.pointee.pointee.Release(iface)
            device.pointee.pointee.USBDeviceClose(device)
            _ = device.pointee.pointee.Release(device)
            return nil
        }

        var bulkIn: UInt8 = 0
        var bulkOut: UInt8 = 0

        for pipeIdx: UInt8 in 1...numEndpoints {
            var direction: UInt8 = 0
            var number: UInt8 = 0
            var transferType: UInt8 = 0
            var maxPacketSize: UInt16 = 0
            var interval: UInt8 = 0

            iface.pointee.pointee.GetPipeProperties(
                iface, pipeIdx, &direction, &number, &transferType, &maxPacketSize, &interval
            )

            logger.debug("Pipe \(pipeIdx, privacy: .public): dir=\(direction, privacy: .public) num=\(number, privacy: .public) type=\(transferType, privacy: .public) maxPkt=\(maxPacketSize, privacy: .public)")

            // transferType: 0=control, 1=isochronous, 2=bulk, 3=interrupt
            // We only care about bulk endpoints.
            if transferType == 2 {
                if direction == 1 { // IN
                    bulkIn = pipeIdx
                } else { // OUT
                    bulkOut = pipeIdx
                }
            }
        }

        guard bulkIn != 0 && bulkOut != 0 else {
            logger.error("Could not find bulk IN/OUT endpoints")
            iface.pointee.pointee.USBInterfaceClose(iface)
            iface.pointee.pointee.Release(iface)
            device.pointee.pointee.USBDeviceClose(device)
            device.pointee.pointee.Release(device)
            return nil
        }

        logger.info("Claimed interface: bulkIn=pipe\(bulkIn, privacy: .public), bulkOut=pipe\(bulkOut, privacy: .public)")

        // The device interface must stay open for the duration of the USB
        // session, so it is handed off (with the interface ref) to
        // USBTransport, which closes and releases both in stop().
        return ClaimedInterface(
            interfaceRef: iface,
            deviceRef: device,
            bulkInPipe: bulkIn,
            bulkOutPipe: bulkOut
        )
    }
}
