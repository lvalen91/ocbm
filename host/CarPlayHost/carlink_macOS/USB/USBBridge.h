// ──────────────────────────────────────────────────────────────────────────────
// USBBridge.h — Bridging header: makes IOKit USB UUIDs visible to Swift
// ──────────────────────────────────────────────────────────────────────────────
//
// The problem
// ───────────
// IOKit's USB API identifies interface types with CFUUID constants like
// kIOUSBDeviceInterfaceID300. These are defined as C preprocessor macros:
//
//     #define kIOUSBDeviceInterfaceID300 \
//         CFUUIDGetConstantUUIDWithBytes(NULL, 0x9C, 0x28, ...)
//
// Swift's ClangImporter cannot bridge macros that expand to function calls,
// so these constants simply don't exist in Swift. Even if they did, they
// produce CFUUIDRef (Unmanaged<CFUUID>), but the COM QueryInterface function
// expects CFUUIDBytes (a plain 16-byte struct).
//
// The solution
// ────────────
// This header provides two families of inline wrappers:
//
//   1. UUID byte getters — Call the macro, then CFUUIDGetUUIDBytes() to return
//      a CFUUIDBytes value Swift can pass directly to QueryInterface:
//        • GetIOUSBDeviceUserClientTypeIDBytes()
//        • GetIOCFPlugInInterfaceIDBytes()
//        • GetIOUSBDeviceInterfaceID300Bytes()
//        • GetIOUSBInterfaceUserClientTypeIDBytes()
//        • GetIOUSBInterfaceInterfaceID300Bytes()
//
//   2. PlugIn factory helpers — Wrap IOCreatePlugInInterfaceForService() with
//      the correct UUID pairs baked in, so Swift never touches CFUUID at all:
//        • CreatePlugInForUSBDevice()   — used in USBDeviceManager.swift
//        • CreatePlugInForUSBInterface() — used in USBInterfaceClaimer
//
// Why not IOUSBHost.framework?
// ────────────────────────────
// See USBDeviceManager.swift header for the full explanation. In short: the
// CPC200-CCPA adapter requires USBDeviceOpenSeize (exclusive access recovery),
// USBDeviceReEnumerate (firmware reset), and synchronous bulk pipe I/O — none
// of which are available in IOUSBHost without Apple-signed entitlements.
// ──────────────────────────────────────────────────────────────────────────────

#ifndef USBBridge_h
#define USBBridge_h

#include <IOKit/IOKitLib.h>
#include <IOKit/IOCFPlugIn.h>
#include <IOKit/usb/IOUSBLib.h>

// ── UUID byte getters ────────────────────────────────────────────────────────
// Each function calls the invisible CFUUID macro, then extracts the raw 16-byte
// UUID struct that QueryInterface expects.

// Identifies the IOUSBDeviceUserClient plug-in type (used with CreatePlugIn).
static inline CFUUIDBytes GetIOUSBDeviceUserClientTypeIDBytes(void) {
    return CFUUIDGetUUIDBytes(kIOUSBDeviceUserClientTypeID);
}

// Base plug-in interface UUID (required by all IOCreatePlugInInterface calls).
static inline CFUUIDBytes GetIOCFPlugInInterfaceIDBytes(void) {
    return CFUUIDGetUUIDBytes(kIOCFPlugInInterfaceID);
}

// IOUSBDeviceInterface300 — the USB 2.0 device control interface we QueryInterface for.
static inline CFUUIDBytes GetIOUSBDeviceInterfaceID300Bytes(void) {
    return CFUUIDGetUUIDBytes(kIOUSBDeviceInterfaceID300);
}

// Identifies the IOUSBInterfaceUserClient plug-in type (for interface claiming).
static inline CFUUIDBytes GetIOUSBInterfaceUserClientTypeIDBytes(void) {
    return CFUUIDGetUUIDBytes(kIOUSBInterfaceUserClientTypeID);
}

// IOUSBInterfaceInterface300 — the USB 2.0 interface we use for bulk pipe I/O.
static inline CFUUIDBytes GetIOUSBInterfaceInterfaceID300Bytes(void) {
    return CFUUIDGetUUIDBytes(kIOUSBInterfaceInterfaceID300);
}

// ── PlugIn factory helpers ────────────────────────────────────────────────────
// These wrap IOCreatePlugInInterfaceForService with the correct UUID pair so
// Swift callers never need to reference CFUUID constants.

// Creates a plug-in for a USB device service (used in resetDevicesOnLaunch and
// USBInterfaceClaimer.claim to obtain IOUSBDeviceInterface300).
static inline kern_return_t CreatePlugInForUSBDevice(
    io_service_t service,
    IOCFPlugInInterface ***plugIn,
    SInt32 *score
) {
    return IOCreatePlugInInterfaceForService(
        service,
        kIOUSBDeviceUserClientTypeID,
        kIOCFPlugInInterfaceID,
        plugIn,
        score
    );
}

// Creates a plug-in for a USB interface service (used in USBInterfaceClaimer
// to obtain IOUSBInterfaceInterface300 for bulk pipe read/write).
static inline kern_return_t CreatePlugInForUSBInterface(
    io_service_t service,
    IOCFPlugInInterface ***plugIn,
    SInt32 *score
) {
    return IOCreatePlugInInterfaceForService(
        service,
        kIOUSBInterfaceUserClientTypeID,
        kIOCFPlugInInterfaceID,
        plugIn,
        score
    );
}

#endif
