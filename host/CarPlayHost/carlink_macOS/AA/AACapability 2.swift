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
        /// Tiers 4–9 are gal `VideoCodecResolutionType` as shipped in aasdk's copy of the enum
        /// (2560x1440 = 4, 3840x2160 = 5, then the portrait set 720x1280 = 6, 1080x1920 = 7,
        /// 1440x2560 = 8, 2160x3840 = 9). Added 2026-09-04 (T4); each is verified against the
        /// Pixel one at a time via `AA_FORCE_RES` — see the per-case notes as they land.
        case r2560x1440 = 4
        case r3840x2160 = 5
        case p720x1280 = 6
        case p1080x1920 = 7
        case p1440x2560 = 8
        case p2160x3840 = 9

        var size: (w: UInt32, h: UInt32) {
            switch self {
            case .r800x480:   return (800, 480)
            case .r1280x720:  return (1280, 720)
            case .r1920x1080: return (1920, 1080)
            case .r2560x1440: return (2560, 1440)
            case .r3840x2160: return (3840, 2160)
            case .p720x1280:  return (720, 1280)
            case .p1080x1920: return (1080, 1920)
            case .p1440x2560: return (1440, 2560)
            case .p2160x3840: return (2160, 3840)
            }
        }

        var isPortrait: Bool { size.h > size.w }
        /// gearhead 17.5 caps H.264 at 1080p in either orientation (`ivf.B`: "VideoCodecResolutionType
        /// %s is not allowed for the codec type %s"), so 2560x1440 / 3840x2160 and their portrait
        /// twins must be declared with MEDIA_CODEC_VIDEO_H265 (7). Device-measured 2026-09-04: tier 4
        /// declared as H.264 → the phone found "No working configuration" and closed the transport.
        var needsHEVC: Bool { rawValue == 4 || rawValue == 5 || rawValue == 8 || rawValue == 9 }

        static let landscape: [Resolution] = [.r3840x2160, .r2560x1440, .r1920x1080, .r1280x720, .r800x480]
        static let portrait: [Resolution] = [.p2160x3840, .p1440x2560, .p1080x1920, .p720x1280]

        /// Nearest AA-expressible mode for an arbitrary configured size, in the panel's own
        /// orientation. Exact match wins; otherwise the largest mode that fits, and the smallest
        /// mode of that orientation as the floor. Never silently upscales past what was asked for:
        /// claiming a resolution the app will not render is how you get a stretched or cropped
        /// projection. (T4 will add margins on top of this so non-tier panels get an exact UI.)
        static func nearest(width: Int, height: Int) -> Resolution {
            let all = height > width ? portrait : landscape
            if let exact = all.first(where: { Int($0.size.w) == width && Int($0.size.h) == height }) {
                return exact
            }
            return all.first(where: { Int($0.size.w) <= width && Int($0.size.h) <= height }) ?? all.last!
        }

        /// T4 (2026-09-04): tier + VISIBLE sub-rect for a panel that is not a tier. gearhead lays its
        /// UI out inside `codec size − margins` (`iux.c`: left = w/2, right = (w+1)/2, top = (h+1)/2,
        /// bottom = h/2 — an even split) and the head unit crops those margins away. Candidates are
        /// the panel's own orientation, smallest first; a tier is admitted when the largest
        /// panel-aspect rect that fits inside it is at least the panel (no upscale), else the largest
        /// tier is used. The visible size is returned even; margins = tier − visible.
        static func tierAndVisible(width: Int, height: Int) -> (tier: Resolution, w: Int, h: Int) {
            let ordered = (height > width ? portrait : landscape).reversed()   // smallest first
            let aspect = Double(width) / Double(height)
            func fit(_ t: Resolution) -> (Int, Int) {
                let tw = Double(t.size.w), th = Double(t.size.h)
                var vw = min(tw, th * aspect)
                var vh = vw / aspect
                if vh > th { vh = th; vw = vh * aspect }
                let w = min(Int(t.size.w), Int(vw.rounded(.down)) & ~1)
                let h = min(Int(t.size.h), Int(vh.rounded(.down)) & ~1)
                return (w, h)
            }
            for t in ordered {
                let (w, h) = fit(t)
                if w >= width && h >= height { return (t, w, h) }
            }
            let big = ordered.last!
            let (w, h) = fit(big)
            return (big, w, h)
        }

        /// `AA_FORCE_RES` spellings: 800 | 720 | 1080 | 1440 | 2160 for landscape, p720 | p1080 |
        /// p1440 | p2160 for portrait.
        static func forced(_ s: String?) -> Resolution? {
            switch s {
            case "800":  return .r800x480
            case "720":  return .r1280x720
            case "1080": return .r1920x1080
            case "1440": return .r2560x1440
            case "2160": return .r3840x2160
            case "p720": return .p720x1280
            case "p1080": return .p1080x1920
            case "p1440": return .p1440x2560
            case "p2160": return .p2160x3840
            default: return nil
            }
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
    static let audioSinks: [AudioSink] = audioSinkTable(telephony: telephonySinkExperiment)

    /// EXPERIMENT (docs/androidauto/03_WIRELESS.md §6, OFF by default): declare a FOURTH sink with
    /// `AudioStreamType.TELEPHONY` (4) and see whether the phone ever opens it. The question it exists
    /// to answer is whether call audio can ride the projection link at all instead of Bluetooth
    /// HFP/SCO — nobody has observed gearhead routing it there, and the answer is one channel-open
    /// away. Default OFF because the service set is accepted or rejected WHOLE: an unrecognised sink
    /// costs the entire session (`CAR.SERVICE Critical error 2/24`), not just the sink.
    static let telephonySinkExperiment = ProcessInfo.processInfo.environment["AA_TELEPHONY_SINK"] == "1"

    /// Channel 2 — the first id not already spoken for (0 control, 1 sensor, 3 video, 4/5/6 audio,
    /// 8 input, 9 mic). Ids here are the head unit's to choose: this table already places sensor at 1
    /// and mic at 9, which no reference implementation does.
    static let telephonySinkChannel: UInt8 = AAWire.chTelephonyAudio

    /// The sink table, parameterised so a test can build both variants without touching the
    /// environment (the lever is resolved once, at first use of `audioSinks`).
    /// Telephony mirrors GUIDANCE's shape — 16 kHz mono, voice-routed so it ducks media — because
    /// that is the narrowband speech shape AA already negotiates and the one the playback path is
    /// pre-warmed for. If the phone ever opens the channel, its `MediaSinkService` config in the
    /// setup request is the authority and this declaration is what we must then honour.
    /// GUIDANCE and SYSTEM sink rate. 48 kHz by default since 2026-09-04: the reference head unit's
    /// 16 kHz (Google's 2016 integration guide floor) is why navigation prompts and Assistant replies
    /// sounded call-like; declared at 48 kHz the Pixel 10 / gearhead 17.5 initialised both channels
    /// at 48 kHz mono ("init, samplingRate: 48000 ... numberOfChannels: 1") and the owner confirmed
    /// the prompts noticeably better. `AA_VOICE_RATE=16000` restores the reference value for a phone
    /// that rejects the higher one (the phone's setup request is the authority on what it sends).
    static let voiceSinkRate: Int = {
        if let s = ProcessInfo.processInfo.environment["AA_VOICE_RATE"], let r = Int(s),
           [16000, 24000, 48000].contains(r) { return r }
        return 48000
    }()

    static func audioSinkTable(telephony: Bool, voiceRate: Int = voiceSinkRate) -> [AudioSink] {
        var t: [AudioSink] = [
            AudioSink(channel: 4, streamType: 3, rate: 48000, channels: 2, voice: false, label: "media"),
            AudioSink(channel: 5, streamType: 1, rate: voiceRate, channels: 1, voice: true,  label: "guidance"),
            AudioSink(channel: 6, streamType: 2, rate: voiceRate, channels: 1, voice: true,  label: "system"),
        ]
        if telephony {
            t.append(AudioSink(channel: telephonySinkChannel, streamType: 4, rate: 16000, channels: 1,
                               voice: true, label: "telephony"))
        }
        return t
    }

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
    /// Metadata services (media playback status, navigation status, phone status) declared in
    /// service discovery. On by default since the Pixel 10 / gearhead 17.5 accepted all three
    /// descriptors and opened the channels on 2026-09-04 (a disliked service set makes the phone
    /// drop the transport right after discovery, so this was levered off for the first run).
    /// `AA_METADATA=0` withholds them.
    static let metadataServices = ProcessInfo.processInfo.environment["AA_METADATA"] != "0"

    /// Visible (margin-cropped) size inside the codec tier, or 0×0 when the tier is declared whole
    /// (exact tier, or `AA_MARGINS=0`). This is the size gearhead lays the UI out in and the space
    /// touch is sent in (`jjd` adds dispLeft/dispTop to incoming pointers; the InputSourceService
    /// touchscreen width/height are ignored by the phone — `ikb` reads only the type).
    let visibleWidth: UInt32
    let visibleHeight: UInt32
    /// Codec-frame pixels outside the visible rect, total per axis (gearhead splits them evenly).
    var margins: (w: UInt32, h: UInt32) {
        guard visibleWidth > 0, visibleHeight > 0 else { return (0, 0) }
        return (resolution.size.w - visibleWidth, resolution.size.h - visibleHeight)
    }
    var hasMargins: Bool { margins.w > 0 || margins.h > 0 }

    /// Video codec to declare: HEVC for the tiers gearhead only encodes with H.265, H.264 otherwise.
    /// `AA_HEVC=1` forces HEVC on any tier (bench: exercises the HEVC decode path at 1080p).
    var videoCodecHEVC: Bool {
        ProcessInfo.processInfo.environment["AA_HEVC"] == "1" || resolution.needsHEVC
    }

    /// Driver seat side, from the same vehicle-profile toggle CarPlay's `rightHandDrive` uses. Goes
    /// out as `ServiceDiscoveryResponse.driver_position` (field 6; gal `DriverPosition`, DHU config key
    /// `driverposition`) and decides which side gearhead puts its app rail on. Before 2026-09-04 the
    /// response hardcoded 1 with a comment reading "RIGHT" — the rail sat on the right regardless of
    /// the profile. `AA_DRIVER_POSITION=<n>` overrides the wire value for bench tests.
    let rightHandDrive: Bool
    /// gal `DriverPosition` wire values, verified on device 2026-09-04 (Pixel 10 / gearhead 17.5):
    /// declaring 1 put gearhead's app rail on the RIGHT edge, declaring 2 put it on the LEFT (the
    /// driver's side of a left-hand-drive car). So 1 = RIGHT, 2 = LEFT. Note this is NOT aasdk's
    /// older `left_hand_drive_vehicle` bool reading of field 6 (where 1 would mean LEFT) — that
    /// reading is refuted by the observation above. 0 (UNKNOWN) and 3 (CENTER) are untested.
    static let driverPositionLeft: UInt64 = 2
    static let driverPositionRight: UInt64 = 1
    var driverPosition: UInt64 {
        if let s = ProcessInfo.processInfo.environment["AA_DRIVER_POSITION"], let v = UInt64(s) { return v }
        return rightHandDrive ? Self.driverPositionRight : Self.driverPositionLeft
    }

    /// The size the touch surface is reported at, and the space touch coordinates are sent in. Tied
    /// to the negotiated resolution, NOT to the configured one: reporting a surface we did not
    /// negotiate puts every tap in the wrong place.
    /// The touch surface we map into: the VISIBLE size when margins are declared (gearhead expects
    /// pointer coordinates relative to the visible rect), else the codec size.
    var touchSize: (w: UInt32, h: UInt32) {
        (visibleWidth > 0 ? visibleWidth : resolution.size.w,
         visibleHeight > 0 ? visibleHeight : resolution.size.h)
    }

    /// Build the AA projection from the vehicle profile's plain values.
    ///
    /// `warn` reports facts the config asked for that AA cannot express, rather than quietly
    /// substituting. The resolution case is the one that actually bites: CarPlay will happily run
    /// 1024x600 and AA has no such mode.
    /// Takes values, not `VehicleConfigModel`, so this file compiles in the hardware-free test
    /// harness; the `@MainActor init(config:)` that snapshots the observable model lives beside the
    /// model in SettingsWindow.swift. The struct is Sendable precisely so that crossing to the
    /// session thread is safe — the AA engine must never reach back into the observable model.
    init(mainWidth: Int, mainHeight: Int, maxFPS: Int, name: String, nightMode: Bool,
         rightHandDrive: Bool = false,
         warn: (String) -> Void = { NSLog("[AA] \($0)") }) {
        // AA_FORCE_RES=800|720|1080|1440|2160|p720|p1080|p1440|p2160 overrides the negotiated mode for testing, WITHOUT touching the
        // owner's real vehicle profile (which is a genuine head-unit geometry, not a test fixture).
        // Exists to exercise resolution enums the config cannot reach — an ultrawide 1920x720 maps to
        // 1280x720, so 1920x1080 is otherwise unreachable and stayed unverified.
        let forced = Resolution.forced(ProcessInfo.processInfo.environment["AA_FORCE_RES"])
        // AA_PANEL=WxH stands in for the profile's geometry on the bench (the owner's profile is a real
        // head-unit geometry, not a fixture) — the margin path can be exercised without editing it.
        var mainWidth = mainWidth, mainHeight = mainHeight
        if let s = ProcessInfo.processInfo.environment["AA_PANEL"] {
            let parts = s.lowercased().split(separator: "x").compactMap { Int($0) }
            if parts.count == 2, parts[0] > 0, parts[1] > 0 {
                mainWidth = parts[0]; mainHeight = parts[1]
                warn("AA_PANEL override -> panel \(mainWidth)x\(mainHeight)")
            }
        }
        let marginsEnabled = ProcessInfo.processInfo.environment["AA_MARGINS"] != "0"
        let res: Resolution
        var visible: (w: Int, h: Int) = (0, 0)
        if let f = forced {
            res = f
            if marginsEnabled, Int(f.size.w) != mainWidth || Int(f.size.h) != mainHeight {
                // Compose the forced tier with the panel: the largest panel-aspect rect inside it.
                let aspect = Double(mainWidth) / Double(mainHeight)
                var vw = min(Double(f.size.w), Double(f.size.h) * aspect); var vh = vw / aspect
                if vh > Double(f.size.h) { vh = Double(f.size.h); vw = vh * aspect }
                visible = (Int(vw.rounded(.down)) & ~1, Int(vh.rounded(.down)) & ~1)
            }
            warn("AA_FORCE_RES override -> declaring \(res.size.w)x\(res.size.h)"
                 + (visible.w > 0 ? " with visible \(visible.w)x\(visible.h)" : ""))
        } else if marginsEnabled {
            let (t, w, h) = Resolution.tierAndVisible(width: mainWidth, height: mainHeight)
            res = t
            if Int(t.size.w) != w || Int(t.size.h) != h {
                visible = (w, h)
                warn("configured \(mainWidth)x\(mainHeight) is not an Android Auto tier — declaring "
                     + "\(t.size.w)x\(t.size.h) with margins \(Int(t.size.w) - w)x\(Int(t.size.h) - h) "
                     + "(visible \(w)x\(h), cropped and scaled to the panel)")
            }
        } else {
            res = Resolution.nearest(width: mainWidth, height: mainHeight)
            if Int(res.size.w) != mainWidth || Int(res.size.h) != mainHeight {
                warn("configured \(mainWidth)x\(mainHeight) is not an Android Auto mode — "
                     + "negotiating \(res.size.w)x\(res.size.h) (AA_MARGINS=0: no margins)")
            }
        }
        self.visibleWidth = UInt32(visible.w)
        self.visibleHeight = UInt32(visible.h)
        // AA_FORCE_FPS=30|60 overrides the profile's rate (T4 tier verification: a refused tier and a
        // refused rate look the same from here — the phone closes the transport after VIDEO CONFIG).
        let fps: FrameRate = {
            switch ProcessInfo.processInfo.environment["AA_FORCE_FPS"] {
            case "30": return .fps30
            case "60": return .fps60
            default: return FrameRate.nearest(maxFPS)
            }
        }()
        if (fps == .fps60) != (maxFPS >= 60) || (maxFPS != 30 && maxFPS != 60) {
            warn("configured \(maxFPS) fps is not an Android Auto rate — negotiating "
                 + (fps == .fps60 ? "60" : "30"))
        }
        self.resolution = res
        self.frameRate = fps
        // Density is the DPI gearhead hands to its virtual display (`iuo`/`nso`: createVirtualDisplay
        // with the declared value, unclamped); UI elements scale as density/160 in pixels while the
        // tier, margins and visible rect stay the same, and layouts are chosen from the resulting
        // point width. AA_DENSITY=<n> overrides for the bench until the profile carries a panel size.
        let dEnv = ProcessInfo.processInfo.environment["AA_DENSITY"].flatMap { UInt32($0) }
        if let d = dEnv, d >= 80, d <= 640 {
            self.density = d
            warn("AA_DENSITY override -> declaring density \(d)")
        } else {
            self.density = 160
        }
        self.name = name
        self.nightMode = nightMode
        self.rightHandDrive = rightHandDrive
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
