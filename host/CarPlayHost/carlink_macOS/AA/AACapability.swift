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
        /// 1440x2560 = 8, 2160x3840 = 9). Added 2026-09-04 (T4) and device-verified the same day,
        /// one at a time via `AA_FORCE_RES` / `AA_FORCE_FPS`; the per-(tier, fps) evidence is the
        /// table in docs/androidauto/01_SESSION_AND_AV.md and is what `deviceVerified(atFPS:)` encodes.
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

        /// Whether a real phone has accepted THIS tier AT THIS FRAME RATE from this head unit. The
        /// evidence is per (tier, fps) PAIRING, not per tier: gearhead decides on the pair ("Checking
        /// video config … isAllowed" in `CAR.VIDEO`), and its decompile carries a "60 fps only up to
        /// 1920×1080 pixels" branch that did not fire on the Pixel 10 / gearhead 17.5 used for the
        /// sweep but may on another phone — exactly the mechanism that would refuse a high-tier
        /// 60 fps pairing nobody has tried. So a verified tier at an untried rate is NOT verified.
        ///
        /// This is a transcription of the committed table in docs/androidauto/01_SESSION_AND_AV.md
        /// (Pixel 10 / gearhead 17.5, wireless, one pairing at a time via `AA_FORCE_RES` /
        /// `AA_FORCE_FPS`, 2026-08-27 and 2026-09-04):
        ///
        ///   tiers 1 800×480, 2 1280×720, 3 1920×1080 (H.264)   60 → 60
        ///   tier  4 2560×1440, 5 3840×2160 (H.265)              30 → 30 AND 60 → 60
        ///   tier  6 720×1280 (H.264)                            60 → 60
        ///   tier  7 1080×1920 (H.264)                           30 → 30
        ///   tiers 8 1440×2560, 9 2160×3840 (H.265)              30 → 30
        ///
        /// When a pairing is verified, add it to the table FIRST, then here. Never widen this from
        /// memory: a "not verified" note on a pairing the owner has watched stream cries wolf, and a
        /// missing note on a pairing that has never streamed is the exact failure the note exists
        /// to prevent — both directions are defects.
        func deviceVerified(atFPS fps: FrameRate) -> Bool {
            switch self {
            // 800x480 is proven at BOTH rates, and the 30 fps half predates the evidence table.
            // Before commit 57501aa (2026-08-27, "capability comes from the vehicle profile, not
            // five constants") this head unit declared exactly `videoRes = 1` / `videoFps = 2`
            // (VIDEO_800x480 / FPS_30) as hardcoded constants in AASession, and ran working
            // sessions on them — that is the baseline the whole AA workstream was built on. The
            // 2026-09-04 table sweeps AA_FORCE_RES and so only records what the SWEEP declared;
            // absence from it is not absence of evidence. Marking this pairing unverified made the
            // shipped `dhu-default` preset (800x480@30, straight from Google's own default.ini)
            // emit a "never been run" note on the project's original working configuration.
            case .r800x480:                                       return true
            case .r1280x720, .r1920x1080, .p720x1280:             return fps == .fps60
            case .r2560x1440, .r3840x2160:                        return true
            case .p1080x1920, .p1440x2560, .p2160x3840:           return fps == .fps30
            }
        }
        /// Convenience for a rate in Hz (30 or 60, snapped the way the renderer snaps it).
        func deviceVerified(atFPS hz: Int) -> Bool { deviceVerified(atFPS: FrameRate.nearest(hz)) }
        /// The frame rates this tier has streamed at, for captions ("verified at 60 fps").
        var deviceVerifiedRates: [FrameRate] {
            [FrameRate.fps30, .fps60].filter { deviceVerified(atFPS: $0) }
        }

        /// Whether the tier has been accepted at ANY rate — all nine have (sweep of 2026-09-04).
        /// This is the weaker, per-tier reading; the renderer's negotiation note uses the
        /// per-pairing `deviceVerified(atFPS:)` above, because a tier proven at 30 says nothing
        /// about the same tier at 60.
        ///
        /// CORRECTED 2026-09-04: this read `rawValue <= 3`, carried forward from the older comment
        /// that only credited the 2026-08-27 session. Calling a proven tier unverified is not the
        /// safe direction it looks like — see `deviceVerified(atFPS:)`.
        ///
        /// Note what IS constrained: tiers 4/5/8/9 must be declared H.265 (`needsHEVC`). That is a
        /// codec-pairing rule enforced by gearhead, not a limit on the tier.
        var deviceVerified: Bool { !deviceVerifiedRates.isEmpty }

        static let landscape: [Resolution] = [.r3840x2160, .r2560x1440, .r1920x1080, .r1280x720, .r800x480]
        static let portrait: [Resolution] = [.p2160x3840, .p1440x2560, .p1080x1920, .p720x1280]

        /// Candidate tiers for a panel orientation, largest first. `allowHEVC == false` drops the
        /// H.265-only tiers (defect 5: `VideoCodecPolicy.hevcAllowed == false` must clamp the AA
        /// declaration to ≤1080p, because gearhead 17.5 refuses tiers 4/5/8/9 as H.264 — device-
        /// measured 2026-09-04, "No working configuration" and a closed transport).
        static func candidates(portrait isPortrait: Bool, allowHEVC: Bool) -> [Resolution] {
            let all = isPortrait ? portrait : landscape
            return allowHEVC ? all : all.filter { !$0.needsHEVC }
        }

        /// Nearest AA-expressible mode for an arbitrary configured size, in the panel's own
        /// orientation. Exact match wins; otherwise the largest mode that fits, and the smallest
        /// mode of that orientation as the floor. Never silently upscales past what was asked for:
        /// claiming a resolution the app will not render is how you get a stretched or cropped
        /// projection. (T4 will add margins on top of this so non-tier panels get an exact UI.)
        static func nearest(width: Int, height: Int, allowHEVC: Bool = true) -> Resolution {
            let all = candidates(portrait: height > width, allowHEVC: allowHEVC)
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
        ///
        /// Two consequences the caller must surface (the init does, as negotiation notes):
        /// - A panel with the SAME aspect as a tier but smaller than it (750x450, 1600x900) gets
        ///   `w == tier.w, h == tier.h` — the whole tier, no margins — and the app scales the
        ///   frame DOWN to the panel. Same contract as every other non-tier panel (2400x960 is
        ///   scaled by the identical ×0.9375), just with the margins at zero.
        /// - The FALLBACK (no tier contains the panel) is the one place the visible rect can be
        ///   SMALLER than the panel, i.e. an upscale: `hevcAllowed == false` + 2400x960 → tier
        ///   1920x1080, visible 1920x768, ×1.25.
        ///
        /// An ODD panel dimension is admitted with a 1-px shortfall on that axis: gearhead splits
        /// each margin evenly, so an odd visible size is unreachable, and escalating a whole tier
        /// (1920x1079 → 2560x1440 as H.265) over one pixel that cannot be declared anyway is the
        /// wrong trade. The shortfall is sub-pixel after scaling (×1.0009) and the init says so.
        static func tierAndVisible(width: Int, height: Int, allowHEVC: Bool = true) -> (tier: Resolution, w: Int, h: Int) {
            let ordered = candidates(portrait: height > width, allowHEVC: allowHEVC).reversed()   // smallest first
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
            // `width & 1` / `height & 1`: the one-pixel allowance for an odd panel axis (see above).
            for t in ordered {
                let (w, h) = fit(t)
                if w + (width & 1) >= width && h + (height & 1) >= height { return (t, w, h) }
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

    /// HARNESS FIXTURE, not the wire. The three sinks shaped by the `AA_*` environment alone, with
    /// no profile in the picture. It has NO production reader: `AASession` always passes the
    /// INSTANCE `audioSinks` below (profile-shaped since 2026-09-04, W2: `voiceRateHz` /
    /// `telephonyOverProjection` moved into the vehicle profile) to `AAWire`, whose default argument
    /// this is only so `tests/main.swift` can encode an SD response without building a profile. Kept
    /// for that reason alone (audit R6, 2026-09-04); a change to what the app DECLARES belongs in
    /// `audioSinkTable` and the init, never here.
    static let audioSinks: [AudioSink] = audioSinkTable(telephony: telephonySinkExperiment)

    /// The sinks THIS head unit declares — profile-shaped (voice rate, telephony sink), environment
    /// overriding for the bench. Declared in the SD response and played from the same table, so a
    /// declared format and a played format cannot drift apart (see `AudioSink`).
    let audioSinks: [AudioSink]
    /// Whether the telephony sink experiment is on for this session (profile
    /// `telephonyOverProjection`, or `AA_TELEPHONY_SINK=1` on the bench).
    let telephonySink: Bool
    func audioSink(forChannel ch: UInt8) -> AudioSink? {
        audioSinks.first { $0.channel == ch }
    }

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

    /// HARNESS-ONLY lookup into the fixture table above. Zero production callers — `AASession`
    /// resolves a channel through the instance method, against the profile-shaped table it declared.
    /// Its only reader is `tests/main.swift` (the audioSink(forChannel:) section), which therefore
    /// pins the FIXTURE's shape, not the wire's; the wire is pinned by the `AACapability(profile:)`
    /// checks in tests/SettingsTests.swift. Retained because removing it breaks that harness file,
    /// which this renderer does not own (audit R6, 2026-09-04).
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
    ///
    /// The five members are gal's complete `DrivingStatus` vocabulary and are kept complete even
    /// where nothing in this file names one (`noVoiceInput` arrives from the profile through
    /// `FeatureMatrix.androidAutoDrivingStatus` as a raw bit): the set is what `fullyRestricted`
    /// (31) is the sum of, and a reader of the mask needs every bit named, not just the ones we
    /// happen to build here.
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
    /// separate UI/map appearance to set). From the neutral `appearance.theme` since 2026-09-04:
    /// dark = true, light = false; `auto` is "follow this Mac's appearance", which the CALLER
    /// resolves (AppDelegate reads `NSApp.effectiveAppearance`) and passes in — this file stays
    /// AppKit-free so the headless harness can compile it.
    let nightMode: Bool
    /// Whether the car is moving enough that the phone should restrict its UI, AT SESSION START.
    /// Drives `SensorBatch{ driving_status }`. Always false here — see the init for why the profile's
    /// restriction set is a CAPABILITY DECLARATION and not a claim that the car is moving.
    let drivingRestricted: Bool
    /// The `driving_status` mask this head unit sends when driving mode is asserted
    /// (`AASession.setDrivingRestricted(true)` from the Controls window). Before 2026-09-04 the
    /// session sent the hardcoded `drivingDefault` and the profile's restriction set never reached
    /// it (DESIGN.md §10). Now: `restrictions.declared` ⇒ `FeatureMatrix.androidAutoDrivingStatus(set)`
    /// (mapped by the bridge in AACapability+Profile.swift — this file cannot see FeatureMatrix),
    /// else `drivingDefault`, which is bit-identical to `.typicalDriving` (mask 26).
    let drivingMask: DrivingRestrictions

    /// Which metadata service descriptors (media playback status, navigation status, phone status)
    /// to declare in service discovery. All three accepted by the Pixel 10 / gearhead 17.5 on
    /// 2026-09-04; a disliked service set makes the phone drop the transport right after discovery,
    /// so each one is individually withholdable from the profile (`MetadataFeeds`).
    struct MetadataServices: Sendable, Equatable {
        var mediaPlayback: Bool
        var navigationStatus: Bool
        var phoneStatus: Bool
        static let all = MetadataServices(mediaPlayback: true, navigationStatus: true, phoneStatus: true)
        static let none = MetadataServices(mediaPlayback: false, navigationStatus: false, phoneStatus: false)
        var any: Bool { mediaPlayback || navigationStatus || phoneStatus }
    }
    /// The bench lever: `AA_METADATA=0` withholds ALL THREE descriptors regardless of the profile
    /// (the first-run shape, kept so a phone that rejects the set can still be brought up).
    static let metadataServices = ProcessInfo.processInfo.environment["AA_METADATA"] != "0"
    /// What THIS session declares: the profile's three feeds, unless the lever above withholds them.
    let metadata: MetadataServices

    /// Whether the input service declares a touchscreen. From `InputDevices.touchscreen != nil`;
    /// `AA_NO_TOUCH=1` forces it off for the bench (DHU `rotary.ini` shape, `touch=false,
    /// controller=true`). InputChannel has no explicit controller flag, so "controller head unit"
    /// is expressed by declaring keycodes and NOT declaring a touchscreen (see AAWire).
    let declaresTouchscreen: Bool

    /// Visible (margin-cropped) size inside the codec tier, or 0×0 when the tier is declared whole
    /// (exact tier, or margins disabled). This is the size gearhead lays the UI out in and the space
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

    /// The profile's `VideoCodecPolicy.hevcAllowed` (Apple's `enablesHEVC`), as consumed by THIS
    /// renderer. Defect 5 (2026-09-04): it was a CarPlay-only toggle that AA ignored. Now false
    /// clamps the tier to ≤1080p (the H.264-only set) and records a note.
    let hevcAllowed: Bool
    /// Video codec to declare: H.265 for the tiers gearhead only encodes with H.265 (when the
    /// profile allows HEVC), or at ≤1080p when the profile PREFERS it (`androidAuto.preferHEVC`;
    /// H.264 is the device-proven path there). `AA_HEVC=1` forces HEVC on any tier for the bench
    /// (exercises the HEVC decode path at 1080p) and wins over the profile, hevcAllowed included.
    let videoCodecHEVC: Bool

    /// Driver seat side — the neutral `DriverPosition` ternary, re-spelled here so this file needs
    /// no contract type. Goes out as `ServiceDiscoveryResponse.driver_position` (field 6; gal
    /// `DriverPosition`, DHU config key `driverposition = left|right|center`) and decides which side
    /// gearhead puts its app rail on. Before 2026-09-04 the response hardcoded 1 with a comment
    /// reading "RIGHT" — the rail sat on the right regardless of the profile; until W2 (2026-09-04)
    /// the value was a Bool and CENTER was unreachable.
    enum DriverSeat: Sendable, Equatable {
        case left, right, center
        /// gal `DriverPosition` wire values. 1 = RIGHT and 2 = LEFT are DEVICE-VERIFIED 2026-09-04
        /// (Pixel 10 / gearhead 17.5: declaring 1 put the app rail on the RIGHT edge, declaring 2
        /// put it on the LEFT). 3 = CENTER is the DHU binary's `DRIVER_POSITION_CENTER` (its enum
        /// string table lists CENTER, LEFT, RIGHT, UNKNOWN — checked 2026-09-04) and is UNVERIFIED on
        /// device; the renderer records a note when it is declared. Note this is NOT aasdk's older
        /// `left_hand_drive_vehicle` bool reading of field 6 (where 1 would mean LEFT) — that reading
        /// is refuted by the observation above. 0 (UNKNOWN) is never declared.
        var wire: UInt64 {
            switch self {
            case .left:   return AACapability.driverPositionLeft
            case .right:  return AACapability.driverPositionRight
            case .center: return AACapability.driverPositionCenter
            }
        }
    }
    let driverSeat: DriverSeat
    /// Legacy Bool reading, kept for the session log; `.center` is neither.
    var rightHandDrive: Bool { driverSeat == .right }
    static let driverPositionLeft: UInt64 = 2
    static let driverPositionRight: UInt64 = 1
    static let driverPositionCenter: UInt64 = 3
    /// The wire value. `AA_DRIVER_POSITION=<n>` overrides for bench tests (it is how 1 and 2 were
    /// told apart in the first place) and still wins over the profile.
    var driverPosition: UInt64 {
        if let s = ProcessInfo.processInfo.environment["AA_DRIVER_POSITION"], let v = UInt64(s) { return v }
        return driverSeat.wire
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

    /// Everything this renderer had to approximate, withhold, or override on the way from the
    /// neutral profile to the AA declaration — one human-readable line each, in the order they were
    /// decided. Defect 4 (2026-09-04): resolution and frame rate silently degraded under AA with
    /// only an NSLog nobody saw. Every line here was ALSO passed to `warn`, so the log is unchanged;
    /// the Vehicle tab (W3) renders this list next to the panel geometry. Empty means the profile
    /// went to the wire exactly as authored. Bench `AA_*` overrides are recorded too, tagged by
    /// their variable name, so a bench run can never be mistaken for the profile's own output.
    let negotiationNotes: [String]

    /// Build the AA projection from the vehicle profile's plain values.
    ///
    /// `warn` reports facts the config asked for that AA cannot express, rather than quietly
    /// substituting. The resolution case is the one that actually bites: CarPlay will happily run
    /// 1024x600 and AA has no such mode.
    /// Takes plain values, not `VehicleProfile`, so this file compiles in the hardware-free test
    /// harness without the contract types; the `init(profile:adapter:warn:)` that maps the neutral
    /// profile (and `FeatureMatrix`) onto these lives in AACapability+Profile.swift. The struct is
    /// Sendable precisely so that crossing to the session thread is safe — the AA engine must never
    /// reach back into the observable model.
    ///
    /// `notes` are approximations the CALLER already made (the profile bridge's "recorded, not sent"
    /// facts); they lead the list so the UI reads profile-level caveats before wire-level ones.
    init(mainWidth: Int, mainHeight: Int, maxFPS: Int, dpi: Int = 160, name: String,
         nightMode: Bool, driverSeat: DriverSeat = .left,
         hevcAllowed: Bool = true, preferHEVC: Bool = false, fitPanelWithMargins: Bool = true,
         drivingMask: DrivingRestrictions = .drivingDefault,
         voiceRateHz: Int = 48000, telephonySink: Bool = false,
         metadata: MetadataServices = .all, touchscreen: Bool = true,
         notes: [String] = [],
         warn: (String) -> Void = { NSLog("[AA] \($0)") }) {
        let env = ProcessInfo.processInfo.environment
        var notes = notes
        // Every approximation is BOTH logged (as before) and recorded (defect 4).
        func note(_ s: String) { notes.append(s); warn(s) }

        // ── Resolution ──────────────────────────────────────────────────────────────────────────
        // AA_FORCE_RES=800|720|1080|1440|2160|p720|p1080|p1440|p2160 overrides the negotiated mode
        // for testing, WITHOUT touching the owner's real vehicle profile (which is a genuine head-unit
        // geometry, not a test fixture). Exists to exercise resolution enums the config cannot reach —
        // an ultrawide 1920x720 maps to 1280x720, so 1920x1080 is otherwise unreachable and stayed
        // unverified.
        let forced = Resolution.forced(env["AA_FORCE_RES"])
        // AA_PANEL=WxH stands in for the profile's geometry on the bench (the owner's profile is a real
        // head-unit geometry, not a fixture) — the margin path can be exercised without editing it.
        var mainWidth = mainWidth, mainHeight = mainHeight
        if let s = env["AA_PANEL"] {
            let parts = s.lowercased().split(separator: "x").compactMap { Int($0) }
            if parts.count == 2, parts[0] > 0, parts[1] > 0 {
                mainWidth = parts[0]; mainHeight = parts[1]
                note("AA_PANEL override -> panel \(mainWidth)x\(mainHeight)")
            }
        }
        // AA_HEVC=1 is the bench's "declare H.265 whatever the profile says" — it wins over
        // hevcAllowed too, because its purpose is to exercise the HEVC path on a profile that
        // otherwise would not (and the note makes the override visible).
        let forceHEVC = env["AA_HEVC"] == "1"
        if forceHEVC, !hevcAllowed {
            note("AA_HEVC override -> declaring H.265 although the profile disallows HEVC")
        }
        let allowHEVCTiers = hevcAllowed || forceHEVC
        // Margins: the profile's `androidAuto.fitPanelWithMargins`; AA_MARGINS=0 was the bench
        // spelling of false and still wins (it is how the pre-T4 nearest-tier path is compared).
        var marginsEnabled = fitPanelWithMargins
        if env["AA_MARGINS"] == "0", marginsEnabled {
            marginsEnabled = false
            note("AA_MARGINS=0 override -> nearest tier without margins")
        }
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
            note("AA_FORCE_RES override -> declaring \(res.size.w)x\(res.size.h)"
                 + (visible.w > 0 ? " with visible \(visible.w)x\(visible.h)" : ""))
            if f.needsHEVC, !hevcAllowed {
                // The forced tier cannot be declared as H.264 at all (gearhead closes the transport),
                // so the bench lever implies HEVC; say so rather than let the phone refuse silently.
                note("AA_FORCE_RES tier \(res.size.w)x\(res.size.h) is H.265-only — declaring H.265 "
                     + "although the profile disallows HEVC")
            }
        } else if marginsEnabled {
            let (t, w, h) = Resolution.tierAndVisible(width: mainWidth, height: mainHeight,
                                                      allowHEVC: allowHEVCTiers)
            res = t
            if !allowHEVCTiers {
                // Defect 5: would the unclamped choice have needed HEVC? Then the profile's
                // hevcAllowed=false is what shrank the declaration, and the owner must be told.
                let unclamped = Resolution.tierAndVisible(width: mainWidth, height: mainHeight, allowHEVC: true).tier
                if unclamped != t {
                    note("panel \(mainWidth)x\(mainHeight) would declare \(unclamped.size.w)x\(unclamped.size.h) "
                         + "(H.265-only on gearhead 17.5) but HEVC is disallowed by the profile — "
                         + "clamped to \(t.size.w)x\(t.size.h)")
                }
            }
            let tw = Int(t.size.w), th = Int(t.size.h)
            // Shortfall per axis: how many panel pixels the visible rect does NOT cover. Positive
            // means the frame is UPSCALED on that axis when the app fits it to the panel. An odd
            // panel axis is allowed exactly one (gearhead's even margin split makes an odd visible
            // size unreachable — see `tierAndVisible`); anything more is the fallback branch.
            let shortW = mainWidth - w, shortH = mainHeight - h
            let oddW = mainWidth & 1, oddH = mainHeight & 1
            let upscaled = shortW > oddW || shortH > oddH
            if tw != w || th != h {
                visible = (w, h)
                let oddNote = (oddW == 1 && shortW == 1) || (oddH == 1 && shortH == 1)
                    ? "; the odd panel axis is rounded to even, a sub-pixel shortfall after scaling"
                    : ""
                note("configured \(mainWidth)x\(mainHeight) is not an Android Auto tier — declaring "
                     + "\(tw)x\(th) with margins \(tw - w)x\(th - h) "
                     + "(visible \(w)x\(h), cropped and scaled to the panel)" + oddNote)
            } else if mainWidth < tw || mainHeight < th {
                // Same aspect as a tier but smaller (750x450, 960x540, 1600x900): the aspect-fit rect
                // IS the tier, so it is declared whole and the app scales the frame DOWN to the
                // panel. The same contract every other non-tier panel gets (2400x960 is scaled by
                // the identical ×0.9375), but with no margins on the wire it would otherwise look
                // like an exact tier — and it is not pixel-exact. Google's own DHU config for the
                // 750x450 six-inch panel (`default_6in.ini`: resolution 800x480, marginwidth 50,
                // marginheight 30) takes the other road, margins to the panel's exact size; the
                // note names those numbers so the owner can see both choices.
                let scale = Double(mainWidth) / Double(tw)
                note("configured \(mainWidth)x\(mainHeight) is not an Android Auto tier — same aspect as "
                     + "\(tw)x\(th) but smaller, so the whole tier is declared (no margins) and the frame "
                     + "is scaled ×\(String(format: "%.2f", scale)) down to the panel; not pixel-exact "
                     + "(the DHU's own config for such a panel declares margins \(tw - mainWidth)x\(th - mainHeight) instead)")
            }
            if upscaled {
                // The fallback: no tier's aspect-fit rect contains the panel, so the largest
                // available tier is used and its fit is SMALLER than the panel — the one case that
                // breaks "never upscale". Distinct note: "cropped and scaled" above does not say
                // which direction, and here the direction is the whole point.
                let scale = max(Double(mainWidth) / Double(w), Double(mainHeight) / Double(h))
                note("no Android Auto tier contains panel \(mainWidth)x\(mainHeight)"
                     + (allowHEVCTiers ? "" : " with HEVC disallowed (tiers above 1080p are H.265-only)")
                     + " — visible \(w)x\(h) is SMALLER than the panel and the projection is upscaled "
                     + "×\(String(format: "%.2f", scale)) to fit it (largest available tier \(tw)x\(th))")
            }
        } else {
            res = Resolution.nearest(width: mainWidth, height: mainHeight, allowHEVC: allowHEVCTiers)
            if !allowHEVCTiers {
                let unclamped = Resolution.nearest(width: mainWidth, height: mainHeight, allowHEVC: true)
                if unclamped != res {
                    note("panel \(mainWidth)x\(mainHeight) would negotiate \(unclamped.size.w)x\(unclamped.size.h) "
                         + "(H.265-only on gearhead 17.5) but HEVC is disallowed by the profile — "
                         + "clamped to \(res.size.w)x\(res.size.h)")
                }
            }
            if Int(res.size.w) != mainWidth || Int(res.size.h) != mainHeight {
                // `nearest` picks the largest mode INSIDE the panel, so the frame is upscaled to the
                // panel — except at its floor (the smallest tier of the orientation), which is
                // larger than the panel and scaled down. Neither is pixel-exact; say which.
                let floored = Int(res.size.w) > mainWidth || Int(res.size.h) > mainHeight
                note("configured \(mainWidth)x\(mainHeight) is not an Android Auto mode — "
                     + "negotiating \(res.size.w)x\(res.size.h) (no margins; "
                     + (floored ? "the smallest tier, larger than the panel and scaled down to it)"
                                : "smaller than the panel and upscaled to it)"))
            }
        }
        self.visibleWidth = UInt32(visible.w)
        self.visibleHeight = UInt32(visible.h)

        // ── Codec (defect 5) ────────────────────────────────────────────────────────────────────
        // First what the PROFILE declares at this tier, then the bench lever on top — so the lever
        // is noted exactly when it changes the declared codec, never when the profile already
        // chose H.265 (audit F2-2, 2026-09-04: with hevcAllowed=true the lever produced
        // codec_type=7 at tier 3 with EMPTY notes, and the Vehicle tab read it as the profile's own
        // output; it was the only one of the AA_* levers with a silent branch).
        let profileHEVC: Bool
        if res.needsHEVC {
            // needsHEVC with hevcAllowed=false is only reachable via AA_FORCE_RES or AA_HEVC (the
            // latter widens the candidate tiers), both noted above.
            profileHEVC = true
        } else if preferHEVC {
            profileHEVC = hevcAllowed
            if hevcAllowed {
                note("declaring H.265 at \(res.size.w)x\(res.size.h) by profile preference "
                     + "(H.264 is the device-proven codec at this tier)")
            } else {
                note("androidAuto.preferHEVC ignored: the profile disallows HEVC")
            }
        } else {
            profileHEVC = false
        }
        let hevc = profileHEVC || forceHEVC
        if forceHEVC, hevcAllowed, !profileHEVC {
            // The hevcAllowed=false case was noted before tier selection (the lever also widens the
            // tier set there); this is the other half — the profile ALLOWS HEVC but would not have
            // declared it at this tier.
            note("AA_HEVC override -> declaring H.265 at \(res.size.w)x\(res.size.h) "
                 + "(the profile would declare H.264 at this tier)")
        }
        self.hevcAllowed = hevcAllowed
        self.videoCodecHEVC = hevc

        // ── Frame rate ──────────────────────────────────────────────────────────────────────────
        // AA_FORCE_FPS=30|60 overrides the profile's rate (T4 tier verification: a refused tier and a
        // refused rate look the same from here — the phone closes the transport after VIDEO CONFIG).
        let fps: FrameRate
        switch env["AA_FORCE_FPS"] {
        case "30": fps = .fps30; note("AA_FORCE_FPS override -> 30 fps")
        case "60": fps = .fps60; note("AA_FORCE_FPS override -> 60 fps")
        default:
            fps = FrameRate.nearest(maxFPS)
            if maxFPS != 30 && maxFPS != 60 {
                note("configured \(maxFPS) fps is not an Android Auto rate — negotiating "
                     + (fps == .fps60 ? "60" : "30"))
            }
        }
        // Verification is per (tier, fps) PAIRING, which is why this sits after both are known.
        // Every tier has streamed at some rate; the note fires only on a pairing that has not
        // (1080x1920@60, 800x480@30 …) and says which rate has, so the owner can pick the proven one
        // or run the sweep and extend the table.
        if !res.deviceVerified(atFPS: fps) {
            let asked = fps == .fps60 ? "60" : "30"
            let proven = res.deviceVerifiedRates.map { $0 == .fps60 ? "60" : "30" }.joined(separator: "/")
            note("tier \(res.size.w)x\(res.size.h) (\(res.rawValue)) at \(asked) fps has not been verified on a "
                 + "device — this tier has only streamed at \(proven) fps; gearhead decides per (tier, fps) "
                 + "pairing and may refuse this one (docs/androidauto/01_SESSION_AND_AV.md)")
        }
        self.resolution = res
        self.frameRate = fps

        // ── Density ─────────────────────────────────────────────────────────────────────────────
        // Density is the DPI gearhead hands to its virtual display (`iuo`/`nso`: createVirtualDisplay
        // with the declared value, unclamped); UI elements scale as density/160 in pixels while the
        // tier, margins and visible rect stay the same, and layouts are chosen from the resulting
        // point width. From the profile's `display.panel.dpi` since 2026-09-04 (it was a constant
        // 160); AA_DENSITY=<n> still overrides for the bench. 80…640 is our own sanity range — the
        // phone would accept anything, which is exactly why a typo must not reach it.
        if let d = env["AA_DENSITY"].flatMap({ UInt32($0) }), d >= 80, d <= 640 {
            self.density = d
            note("AA_DENSITY override -> declaring density \(d)")
        } else {
            let clamped = min(max(dpi, 80), 640)
            if clamped != dpi {
                note("panel dpi \(dpi) is outside 80…640 — declaring density \(clamped)")
            }
            self.density = UInt32(clamped)
        }
        self.name = name
        self.nightMode = nightMode

        // ── Driver seat ─────────────────────────────────────────────────────────────────────────
        self.driverSeat = driverSeat
        if driverSeat == .center {
            note("driver position center -> driver_position \(Self.driverPositionCenter) (gal CENTER) — "
                 + "unverified on device; only left (2) and right (1) are")
        }
        if let s = env["AA_DRIVER_POSITION"], let v = UInt64(s), v != driverSeat.wire {
            note("AA_DRIVER_POSITION override -> driver_position \(v)")
        }

        // ── Driving restrictions ────────────────────────────────────────────────────────────────
        // UNRESTRICTED at session start until something can actually assert otherwise.
        //
        // Do NOT map the profile's restriction set onto `drivingRestricted`, which the first cut did
        // and the bench caught: a restriction set is a CAPABILITY DECLARATION (what this head unit
        // withholds while the car is moving), not a claim that the car IS moving. Feeding it here
        // told the phone to restrict its UI — no keyboard, truncated lists — on a stationary bench,
        // for a setting that means something else entirely. AA's driving status needs a real signal
        // (vehicle speed, parking brake) and the box sources neither yet, so the session starts
        // unrestricted and the Controls window's driving toggle sends `drivingMask` — which IS the
        // profile's set — when asserted.
        self.drivingRestricted = false
        self.drivingMask = drivingMask
        if drivingMask.isEmpty {
            note("no declared restriction has an Android Auto driving_status bit — driving mode will "
                 + "send 0 (unrestricted)")
        }
        if drivingMask.contains(.noVideo) {
            note("restricting video sends NO_VIDEO (bit 1), which blanks the projection while driving")
        }

        // ── Audio sinks ─────────────────────────────────────────────────────────────────────────
        // Voice rate: the profile's `audio.voiceRateHz`; AA_VOICE_RATE still wins (it is the
        // documented way to restore the 16 kHz reference value for a phone that rejects 48 kHz).
        let voiceRate: Int
        if let s = env["AA_VOICE_RATE"], let r = Int(s), [16000, 24000, 48000].contains(r) {
            voiceRate = r
            if r != voiceRateHz { note("AA_VOICE_RATE override -> guidance/system sinks at \(r) Hz") }
        } else if [16000, 24000, 48000].contains(voiceRateHz) {
            voiceRate = voiceRateHz
        } else {
            voiceRate = 48000
            note("voice rate \(voiceRateHz) Hz is not one gearhead negotiates (16000/24000/48000) — declaring 48000")
        }
        // Telephony sink: the profile's `audio.telephonyOverProjection`; AA_TELEPHONY_SINK=1 turns
        // it on regardless (an experiment lever, docs/androidauto/03_WIRELESS.md §6).
        let telephony = telephonySink || Self.telephonySinkExperiment
        if telephony, !telephonySink { note("AA_TELEPHONY_SINK override -> declaring the telephony sink") }
        if telephony {
            note("telephony sink declared (experiment): an unrecognised sink costs the whole session")
        }
        self.telephonySink = telephony
        self.audioSinks = Self.audioSinkTable(telephony: telephony, voiceRate: voiceRate)

        // ── Metadata services ───────────────────────────────────────────────────────────────────
        if Self.metadataServices {
            self.metadata = metadata
        } else {
            self.metadata = .none
            if metadata.any { note("AA_METADATA=0 override -> withholding all metadata services") }
        }

        // ── Input ───────────────────────────────────────────────────────────────────────────────
        // `== "1"`, like every other AA_* lever: the old AAWire check was `!= nil`, so
        // `AA_NO_TOUCH=0` disabled the touchscreen too (audit F2-6, 2026-09-04).
        var touch = touchscreen
        if env["AA_NO_TOUCH"] == "1", touch {
            touch = false
            note("AA_NO_TOUCH override -> no touchscreen declared (controller-only head unit)")
        }
        self.declaresTouchscreen = touch

        self.negotiationNotes = notes
    }

    // The pre-profile six-field init was DELETED 2026-09-04 with `init(config:)` in
    // SettingsWindow.swift, which was its only caller (DESIGN.md §6 Phase 2). Do not reinstate it as
    // a convenience: taking `rightHandDrive: Bool` and `nightMode: Bool` makes the two neutral
    // values that motivated `VehicleProfile` inexpressible — `driverPosition == .center` and
    // `theme == .auto` both collapse on the way in. Construct from a profile, or from the full
    // plain-value init above if you genuinely have loose values.
}
