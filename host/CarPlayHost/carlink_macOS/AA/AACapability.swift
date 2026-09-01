import Foundation

/// What this head unit tells Android Auto about itself — the AA PROJECTION of the shared vehicle
/// profile.
///
/// The vehicle facts (how big the screen is, what frame rate it can take, whether it is night,
/// whether the car is moving) belong to the CAR, not to a protocol. CarPlay renders them as an
/// Apple-schema YAML pushed to the box; Android Auto renders them as protobuf enums in its
/// ServiceDiscoveryResponse. This struct is the second renderer, and it is the ONLY place that
/// knows AA's vocabulary for them.
///
/// Nothing here goes on the wire to the box. The AA head-unit engine runs in this app, so these
/// values reach the phone directly in the SD response; `aa-bridge` is a byte pump and never sees
/// them. The one AA lever the BOX needs is the `android_auto` enable flag, which already rides the
/// pushed YAML. Keeping the rest app-side is deliberate: `carplay_cfg.yaml` mirrors the CarPlay
/// Simulator's schema, and that fidelity is what makes a future Apple SDK change diffable — an
/// `android_auto:` block inside it would cost exactly that.
///
/// Before this existed, every value below was a hardcoded constant in `AASession` (800x480, 30 fps,
/// density 160, night ALWAYS false, driving ALWAYS unrestricted) while the CarPlay side of the same
/// app had all of it app-driven. docs/carplay/04_CAPABILITIES_AND_CONFIG.md: anything configurable about projection is app-driven.
struct AACapability: Sendable {

    // MARK: - The AA video mode

    /// `VideoCodecResolutionType`. AA accepts an ENUM here, not free pixel dimensions — the
    /// fundamental difference from CarPlay's `pixelDimensions`, which takes any width/height. A
    /// vehicle profile that CarPlay can honour exactly may therefore only be APPROXIMATED for AA.
    enum Resolution: UInt32 {
        case r800x480 = 1
        /// DEVICE-VERIFIED 2026-08-27: declared to a Pixel 10, which accepted it and sent
        /// 1280x720 video ("H.264 format ready — 1280×720").
        case r1280x720 = 2
        /// DEVICE-VERIFIED 2026-08-27 via `AA_FORCE_RES=1080`: declared to a Pixel 10, which accepted
        /// it and encoded `H.264 format ready — 1920x1080`, streaming clean at slotDrops=0.
        case r1920x1080 = 3

        var size: (w: UInt32, h: UInt32) {
            switch self {
            case .r800x480:   return (800, 480)
            case .r1280x720:  return (1280, 720)
            case .r1920x1080: return (1920, 1080)
            }
        }

        /// Nearest AA-expressible mode for an arbitrary configured size. Exact match wins; otherwise
        /// the largest mode that fits, and 800x480 as the floor. Never silently upscales past what
        /// was asked for: claiming a resolution the app will not render is how you get a stretched
        /// or cropped projection.
        static func nearest(width: Int, height: Int) -> Resolution {
            let all: [Resolution] = [.r1920x1080, .r1280x720, .r800x480]
            if let exact = all.first(where: { Int($0.size.w) == width && Int($0.size.h) == height }) {
                return exact
            }
            return all.first(where: { Int($0.size.w) <= width && Int($0.size.h) <= height }) ?? .r800x480
        }
    }

    /// `VideoFrameRateType`. Only these two exist.
    enum FrameRate: UInt32 {
        case fps60 = 1
        case fps30 = 2
        static func nearest(_ fps: Int) -> FrameRate { fps >= 60 ? .fps60 : .fps30 }
    }

    let resolution: Resolution
    let frameRate: FrameRate
    /// Screen density in dpi. AA uses it for UI scaling; 160 = mdpi, the safe default.
    let density: UInt32
    /// Head-unit identity shown by the phone. Sourced from the same `name` the CarPlay config uses.
    let name: String

    // MARK: - Audio sinks

    /// One AA audio sink: what we DECLARE in service discovery and, necessarily, what we must then
    /// PLAY. Both readings come from the single table below — declaring 48 kHz stereo and then
    /// playing the bytes as 16 kHz mono is not a mismatch the phone can detect, it is just wrong
    /// audio, so the two must not be able to drift apart.
    struct AudioSink: Sendable {
        /// AA channel id.
        let channel: UInt8
        /// AA `AudioStreamType` in the SD response.
        let streamType: UInt32
        let rate: Int
        let channels: Int
        /// Route through the ducking (nav/voice) mixer rather than the media mixer. Guidance is
        /// turn-by-turn speech and system is alerts — both must duck music, which is what this flag
        /// means to AudioPlayer. Media itself is the thing being ducked.
        let voice: Bool
        let label: String
    }

    /// The three sinks this head unit offers. Channel ids and rates are AA's, not ours — they are
    /// what gearhead expects to open.
    static let audioSinks: [AudioSink] = [
        AudioSink(channel: 4, streamType: 3, rate: 48000, channels: 2, voice: false, label: "media"),
        AudioSink(channel: 5, streamType: 1, rate: 16000, channels: 1, voice: true,  label: "guidance"),
        AudioSink(channel: 6, streamType: 2, rate: 16000, channels: 1, voice: true,  label: "system"),
    ]

    /// The mic SOURCE we offer on channel 9. Declared in the SD response AND used to configure the
    /// capture engine when the phone opens it — one definition, for the same reason as the sinks.
    struct MicSource: Sendable {
        let channel: UInt8 = 9
        let rate: Int = 16000
        let bits: Int = 16
        let channels: Int = 1
    }
    static let micSource = MicSource()

    static func audioSink(forChannel ch: UInt8) -> AudioSink? {
        audioSinks.first { $0.channel == ch }
    }

    /// AA carries PCM LITTLE-ENDIAN (it is Android-native audio), unlike wired CarPlay, which puts
    /// PCM on the wire BIG-ENDIAN (network order). Getting this backwards is not subtle — byte-swapped
    /// 16-bit PCM plays as full-scale white noise.
    ///
    /// DEVICE-VERIFIED 2026-08-27: media playback from a Pixel 10 was clean through the Mac's
    /// speakers, which is the only way to tell these apart (the phone cannot detect the mistake, and
    /// both orders produce a valid-looking byte stream).
    static let pcmIsBigEndian = false

    /// AA's `DrivingStatus` — a BITMASK of what the phone must withhold while the car is moving, not
    /// a boolean. Sending 1 for "restricted" was wrong: 1 is NO_VIDEO, which suppresses the picture
    /// rather than the keyboard (device-observed 2026-08-27 — the on-screen keyboard still appeared).
    ///
    /// This is the nearest AA analogue of CarPlay's `limitedUI` catalogue, and like it, the useful
    /// thing is a SET rather than an on/off.
    struct DrivingRestrictions: OptionSet, Sendable {
        let rawValue: UInt64
        static let none             = DrivingRestrictions([])
        static let noVideo          = DrivingRestrictions(rawValue: 1)
        static let noKeyboardInput  = DrivingRestrictions(rawValue: 2)
        static let noVoiceInput     = DrivingRestrictions(rawValue: 4)
        static let noConfig         = DrivingRestrictions(rawValue: 8)
        static let limitMessageLen  = DrivingRestrictions(rawValue: 16)
        static let fullyRestricted  = DrivingRestrictions(rawValue: 31)

        /// What the app's "Limited UI" toggle means for Android Auto. Deliberately NOT
        /// `fullyRestricted`: that includes NO_VIDEO, which would blank the projection — the opposite
        /// of a usable driving mode. Withhold the input surfaces a driver should not be using
        /// (keyboard, free-form config, long messages) and keep the picture and voice.
        static let drivingDefault: DrivingRestrictions = [.noKeyboardInput, .noConfig, .limitMessageLen]
    }

    // MARK: - Keys

    /// Android `KeyEvent` codes this head unit can send on the input channel.
    ///
    /// These are plain Android keycodes — AA does not invent its own set. They are the AA vocabulary
    /// for intents the Controls window already expresses in CarPlay's (`OCBM.mbtnPlay`,
    /// `OCBM.navHome` …); the mapping between the two lives in the intent router, not here.
    /// Verbatim from aasdk's `ButtonCode` enum — the AA vocabulary, NOT plain Android keycodes,
    /// though most values coincide. Confirmed 2026-08-27 against the published proto.
    enum Key: UInt32, CaseIterable, Sendable {
        case microphone2 = 0x01
        case menu = 0x02
        case home = 0x03
        case back = 0x04
        case phone = 0x05
        case callEnd = 0x06
        case dpadUp = 0x13
        case dpadDown = 0x14
        case dpadLeft = 0x15
        case dpadRight = 0x16
        case dpadCenter = 0x17     // ENTER
        /// MICROPHONE_1 — the mic button, and what actually triggers the Assistant. It is NOT
        /// "search"; that was our name for it before the enum was checked.
        case microphone1 = 0x54
        case mediaPlayPause = 0x55  // TOGGLE_PLAY
        case mediaNext = 0x57
        case mediaPrevious = 0x58
        case mediaPlay = 0x7E
        case mediaPause = 0x7F
        /// The rotary detent. A BUTTON CODE, not a RelativeEvent as previously assumed.
        case scrollWheel = 65536
    }

    /// Every keycode we DECLARE in the input service, which is exactly the set we may then send.
    ///
    /// The declaration is not optional decoration. `InputSourceService.keycodes_supported` is how the
    /// phone learns which keys this head unit has; we previously declared NONE (only a touchscreen),
    /// so a key event would have been sent against a capability we never claimed. CarPlay enforces the
    /// same shape of rule and iOS answers it with silence (docs/carplay/05_METADATA_AND_CONTROLS.md: a subscribe for an id param 6 does
    /// not declare is ignored — no error, no data), so declare first and send only what is declared.
    static var supportedKeycodes: [UInt32] { Key.allCases.map(\.rawValue) }

    // MARK: - Vehicle state (the AA sensor channel)

    /// Night mode. Drives `SensorBatch{ night_mode }` — AA's equivalent of the CarPlay `nightMode`
    /// key, and the input from which AA derives its own dark UI (unlike CarPlay, there is no
    /// separate UI/map appearance to set).
    let nightMode: Bool
    /// Whether the car is moving enough that the phone should restrict its UI. Drives
    /// `SensorBatch{ driving_status }`; the CarPlay analogue is the `limitedUI` catalogue, which is
    /// far more granular — AA offers one bit.
    let drivingRestricted: Bool

    /// The size the touch surface is reported at, and the space touch coordinates are sent in. Tied
    /// to the negotiated resolution, NOT to the configured one: reporting a surface we did not
    /// negotiate puts every tap in the wrong place.
    var touchSize: (w: UInt32, h: UInt32) { resolution.size }

    /// Build the AA projection from the shared vehicle profile.
    ///
    /// `warn` reports facts the config asked for that AA cannot express, rather than quietly
    /// substituting. The resolution case is the one that actually bites: CarPlay will happily run
    /// 1024x600 and AA has no such mode.
    /// @MainActor because VehicleConfigModel is: build the snapshot on the main actor, then hand
    /// the resulting value to the session thread. The struct is Sendable precisely so that crossing
    /// is safe — the AA engine must never reach back into the observable model from its own thread.
    @MainActor
    init(config: VehicleConfigModel, warn: (String) -> Void = { NSLog("[AA] \($0)") }) {
        // AA_FORCE_RES=800|720|1080 overrides the negotiated mode for testing, WITHOUT touching the
        // owner's real vehicle profile (which is a genuine head-unit geometry, not a test fixture).
        // Exists to exercise resolution enums the config cannot reach — an ultrawide 1920x720 maps to
        // 1280x720, so 1920x1080 is otherwise unreachable and stayed unverified.
        let forced: Resolution? = {
            switch ProcessInfo.processInfo.environment["AA_FORCE_RES"] {
            case "800":  return .r800x480
            case "720":  return .r1280x720
            case "1080": return .r1920x1080
            default:     return nil
            }
        }()
        let res = forced ?? Resolution.nearest(width: config.mainWidth, height: config.mainHeight)
        if forced != nil {
            warn("AA_FORCE_RES override -> declaring \(res.size.w)x\(res.size.h)")
        } else if Int(res.size.w) != config.mainWidth || Int(res.size.h) != config.mainHeight {
            warn("configured \(config.mainWidth)x\(config.mainHeight) is not an Android Auto mode — "
                 + "negotiating \(res.size.w)x\(res.size.h) (AA takes a fixed resolution enum, "
                 + "CarPlay takes any size)")
        }
        let fps = FrameRate.nearest(config.maxFPS)
        if (fps == .fps60) != (config.maxFPS >= 60) || (config.maxFPS != 30 && config.maxFPS != 60) {
            warn("configured \(config.maxFPS) fps is not an Android Auto rate — negotiating "
                 + (fps == .fps60 ? "60" : "30"))
        }
        self.resolution = res
        self.frameRate = fps
        self.density = 160
        self.name = config.name
        self.nightMode = config.nightMode
        // UNRESTRICTED until something can actually assert otherwise.
        //
        // Do NOT map CarPlay's `limitedUI` onto this, which the first cut did and the bench caught:
        // limitedUI is a CAPABILITY DECLARATION (what this head unit can still offer while the car
        // is moving), not a claim that the car IS moving. Feeding it here told the phone to restrict
        // its UI — no keyboard, truncated lists — on a stationary bench, for a setting that means
        // something else entirely. AA's driving status needs a real signal (vehicle speed, parking
        // brake) and the box sources neither yet, so declaring unrestricted is both honest and the
        // behaviour that shipped before this file existed.
        self.drivingRestricted = false
    }
}
