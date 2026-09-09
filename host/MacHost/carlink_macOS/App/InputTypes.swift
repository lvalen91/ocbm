// InputTypes.swift — live host-side input wire types.
//
// Extracted from the now-deleted Protocol/MessageTypes.swift (the dormant legacy Carlinkit
// 0x55AA55AA protocol) so the OCBM app keeps only what it actually uses:
//   • TouchAction / MultiTouchAction — pointer/finger phase enums consumed by CarPlayView + AppDelegate.
//   • CommandID — the keyboard/host command ids CarPlayView maps and AppDelegate dispatches.
//
// Everything else in the old MessageTypes.swift (USBHeader, MessageType, the framing constants,
// resolution presets, audio/video headers) was dead legacy framing and was removed with Protocol/.
// `Data.appendLE/readLE/readFloatLE` were removed too (11-L6, verify_05 fix plan) — no callers;
// `OCBMFraming` has its own `readLE32/16`.

import Foundation

// MARK: - Touch Actions (single-touch, pointer/click style)

enum TouchAction: UInt32, Sendable {
    case down = 14
    case move = 15
    case up   = 16
}

// MARK: - MultiTouch Actions (finger/swipe style, used by CarPlay)

enum MultiTouchAction: UInt32, Sendable {
    case up   = 0
    case down = 1
    case move = 2
}

// MARK: - Command IDs (host/keyboard commands)

enum CommandID: UInt32, Sendable {
    // Mic control
    case startMic          = 1
    case stopMic           = 2

    // UI / Host
    case requestHostUI     = 3    // "My Car" button
    case disableBluetooth  = 4

    // Siri
    case siriDown          = 5
    case siriUp            = 6

    // Mic routing
    case micTypeCar        = 7
    case micTypeBox        = 8

    // Video
    case keyFrame          = 12
    case hideUI            = 14
    case micTypePhone      = 15   // boxMici2s

    // Night mode
    case nightModeStart    = 16
    case nightModeStop     = 17

    // GNSS
    case gnssStart         = 18
    case gnssStop          = 19

    // Audio/mic
    case micTypePhoneAlt   = 21   // phoneMic
    case audioTransferBT   = 22   // Route audio via Bluetooth
    case audioTransferAdpt = 23   // Route audio via adapter/USB to host

    // WiFi
    case wifiBand24GHz     = 24
    case wifiBand5GHz      = 25
    case refreshFrame      = 26

    // Standby / BLE
    case enableStandby     = 28
    case disableStandby    = 29
    case startBleAdvert    = 30
    case stopBleAdvert     = 31

    // D-Pad navigation
    case dpadLeft          = 100
    case dpadRight         = 101
    case dpadUp            = 102
    case dpadDown          = 103
    case dpadEnter         = 104  // select down
    case dpadEnterUp       = 105  // select up
    case dpadBack          = 106

    // Rotary knob
    case knobLeft          = 111
    case knobRight         = 112
    case knobUp            = 113
    case knobDown          = 114

    // Media control
    case mediaHome         = 200
    case mediaPlay         = 201
    case mediaPause        = 202
    case mediaPlayPause    = 203
    case mediaNext         = 204
    case mediaPrev         = 205

    // Phone DTMF
    case phoneAccept       = 300
    case phoneReject       = 301
    case phoneKey0         = 302
    case phoneKey1         = 303
    case phoneKey2         = 304
    case phoneKey3         = 305
    case phoneKey4         = 306
    case phoneKey5         = 307
    case phoneKey6         = 308
    case phoneKey7         = 309
    case phoneKey8         = 310
    case phoneKey9         = 311
    case phoneKeyStar      = 312
    case phoneKeyHash      = 313
    case phoneKeyHook      = 314

    // Android Auto focus
    case aaRequestVideo    = 500
    case aaReleaseVideo    = 501
    case aaRequestAudioDuck = 504
    case aaReleaseAudio    = 505
    case aaRequestNavi     = 506
    case aaReleaseNavi     = 507
    case aaRequestNaviScreen = 508
    case aaReleaseNaviScreen = 509

    // Connection status
    case wifiEnable        = 1000
    case autoConnectEnable = 1001
    case wifiConnect       = 1002
    case getBtOnlineList   = 1013
}
