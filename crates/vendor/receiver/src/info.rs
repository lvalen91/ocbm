//! The `/info` response — a binary plist describing the receiver, returned by `GET/POST /info`.
//! Faithful port of the proven C `AirPlayCopyServerInfo` (AirPlayReceiverServer.c) + the harness
//! `_Build{Display,HID,AudioFormats,AudioLatencies}InfoArray` (airplay_receiver_main.c). Every key
//! string + value + structure matches the C exactly — the iPhone validates this capability set at
//! RECORD/activation and tears down if it's incomplete (missing displays / audioFormats / hidDevices
//! / audioLatencies, or a display whose `uuid` doesn't match the HID `displayUUID`).

use plist::{Dictionary, Value};

/// The display UUID shared by the display and its HID devices (C `kHIDDisplayUUIDStr`). The iPhone
/// binds touch input to the display by matching these, so they MUST be identical.
pub const DISPLAY_UUID: &str = "E0CB6FB0-0000-0000-0000-0000C0FFEE01";
/// Distinct UUID for the ALT / cluster display (docs/carplay/06_AV_PIPELINE.md) — must differ from the main display's.
pub const ALT_DISPLAY_UUID: &str = "E0CB6FB0-0000-0000-0000-0000C0FFEE02";

// HID constants (airplay_receiver_main.c).
const HID_VENDOR_ID: i64 = 0x05AC;
const HID_UID_TOUCHSCREEN: u32 = 0x01;
const HID_UID_MEDIA_BUTTONS: u32 = 0x02;
const HID_UID_DPAD: u32 = 0x03; // CarPlay D-Pad (uid 3) — gated behind CARPLAY_DPAD (incident guard)
const HID_UID_KNOB: u32 = 0x04; // CarPlay rotary Knob (uid 4) — gated behind CARPLAY_KNOB; this is the
                                // device the CarPlay Simulator drives ALL of its "hardware" navigation
                                // through (wheel rotation AND the four arrows AND select), not the D-Pad.
const HID_UID_TELEPHONY: u32 = 0x05; // CarPlay Telephony (uid 5) — gated behind CARPLAY_TELEPHONY;
                                     // Hook Switch / Flash / Drop / Mute / DTMF keypad (Apple HIDTelephony).
const HID_PRODUCT_TOUCHSCREEN: i64 = 0x0001;
const HID_PRODUCT_MEDIA_BUTTONS: i64 = 0x0002;
const HID_PRODUCT_DPAD: i64 = 0x0003;
const HID_PRODUCT_KNOB: i64 = 0x0004;
const HID_PRODUCT_TELEPHONY: i64 = 0x0005;
const HID_COUNTRY_CODE: i64 = 0;

/// Static description of this receiver.
#[derive(Debug, Clone)]
pub struct DeviceConfig {
    pub device_id: String,
    pub name: String,
    pub model: String,
    pub source_version: String,
    pub firmware_revision: String,
    pub manufacturer: String,
    /// HomeKit pairing identity (`pi`) — the accessory identifier used in pairing.
    pub pairing_identity: String,
    /// 64-bit AirPlay feature bitmap.
    pub features: u64,
    /// AirPlay status flags.
    pub status_flags: u64,
    pub display_width: i64,
    pub display_height: i64,
    /// Negotiated max frame rate advertised in `displays[].maxFPS` (host YAML `maxFPS`; iOS caps its
    /// encode at this). Default 60.
    pub max_fps: i64,
    /// Main-stream **safe area** — the inset rectangle inside the coded resolution where CarPlay keeps
    /// its interactive UI (for curved/occluded panels). Absolute px `(originX, originY, width, height)`.
    /// `None` = full-bleed (safeArea == the whole panel, the default). Set from the host YAML
    /// `mainVideoStream.viewAreas[0].safeArea`; iOS honors it only if `viewAreas` is echoed in the
    /// SETUP `enabledFeatures` (the `CARPLAY_VIEWAREAS` lever). See [`view_areas`].
    pub main_safe_area: Option<(i64, i64, i64, i64)>,
    /// `drawUIOutsideSafeArea` for the main stream (true = UI may draw in the viewArea↔safeArea gap).
    pub main_draw_outside_safe: bool,
    /// Alt/cluster-stream safe area + draw-outside flag (same semantics, applied to the type-111 display).
    pub alt_safe_area: Option<(i64, i64, i64, i64)>,
    pub alt_draw_outside_safe: bool,
    /// Alt/cluster-stream `maxFPS`. Separate from [`Self::max_fps`] because the host YAML has a
    /// per-stream `altVideoStreams[0].maxFPS` — which was PARSED AND THEN IGNORED until 2026-07-30:
    /// the alt display's `maxFPS` was written from the MAIN stream's value, so the alt-FPS picker in
    /// the Settings window did nothing. 0 = fall back to `max_fps` (prior behaviour).
    pub alt_max_fps: i64,
    /// The advertised CarPlay audio capability set — exactly what `/info` `audioFormats` emits. Defaults
    /// transport-gated (see [`default_audio_formats`]); a host YAML `audio.preset`/`audio.formats`
    /// section overrides it (see [`crate::vehicle_config`]). This is the YAML-driven HU audio config.
    pub audio_formats: Vec<AudioFormatSpec>,
    /// `/info` `limitedUIElements` — which UI elements iOS restricts when limited-UI mode is on
    /// (R14G17 `AirPlayCommon.h:1007`, "[Array] List of UI elements that are affected in limited UI
    /// mode"). Set from the host YAML `limitedUIConfig`; see
    /// [`crate::vehicle_config::LimitedUiConfig`] for the six valid names and their exact order.
    ///
    /// EMPTY = omit the key entirely, which is the pre-2026-07-30 behaviour: iOS then applies its own
    /// default restriction set. This list does NOT enable limited UI — that is the runtime
    /// `/command setLimitedUI {limitedUI:bool}` ([`crate::events::send_set_limited_ui`]), which works
    /// with or without this key.
    pub limited_ui_elements: Vec<String>,
    /// OEM icon (the vehicle-maker logo on the CarPlay home screen), as a MULTI-RESOLUTION set —
    /// `(decoded PNG bytes, widthPixels, heightPixels)` per entry. Apple's `AppStub` emits 120/180/256
    /// ("for each required size"); a single size renders the label but not the image on-device
    /// (2026-08-02). Empty ⇒ emit no icon keys (byte-identical `/info`).
    /// See [`crate::vehicle_config::OemIconConfig`]. Emitted as `oemIcons`/`oemIconLabel`/`oemIconVisible`.
    pub oem_icons: Vec<(Vec<u8>, i64, i64)>,
    pub oem_icon_label: String,
    pub oem_icon_visible: bool,
}

impl Default for DeviceConfig {
    /// The values the proven C stack advertises (conf + AirPlayGetFeatures/Name).
    fn default() -> Self {
        Self {
            device_id: "00:11:22:33:44:55".into(),
            name: "CarPlay".into(), // AirPlayGetDeviceName is hardcoded to "CarPlay"
            model: "carlink_linux-1.00".into(),
            source_version: "320.17".into(),
            firmware_revision: "1.0".into(),
            manufacturer: "carlink_linux".into(),
            pairing_identity: "00:11:22:33:44:55".into(),
            // `/info` features = 0x44440B80,0x61 — the EXACT genuine CCPA head-unit value, and identical
            // to the proven-working C carplayd (`AirPlayGetFeatures`). Low word already carries the audio
            // capability bits: Audio(9) | RedundantAudio(11) | AudioPCM(18) | AudioUnencrypted(22) |
            // AudioAES_128_MFi_SAPv1(26); high word 0x61 = Car(32) | CarPlayControl(37) | HKPairing(38).
            // REVERTED 2026-07-02 (7-agent reconciliation): a prior edit added AAC-LC bit 20 (→0x44540B80)
            // on an ipsw-trace hypothesis, but the ACTUAL working CCPA capture ran WITHOUT bit 20 and still
            // routed AAC-LC media — AAC-LC is advertised via the audioFormats `media` entry, not this bit.
            // Match the proven value exactly; do not re-add bit 20 without a grounded, tested reason.
            features: 0x0000_0061_4444_0B80,
            status_flags: 0x4,
            display_width: 1920,
            display_height: 720,
            max_fps: 60,
            main_safe_area: None,
            main_draw_outside_safe: false,
            alt_safe_area: None,
            alt_draw_outside_safe: false,
            alt_max_fps: 0, // 0 = inherit max_fps
            // Transport-gated default (PCM wired / 8-entry AAC wireless). A YAML `audio:` section
            // overrides this in `VehicleConfig::apply`. Kept a live env check (not a const) so the wired
            // vs wireless launcher picks the right default with no YAML — the proven behavior.
            audio_formats: default_audio_formats(),
            // Empty by default: omit `limitedUIElements` and let iOS use its own default set.
            limited_ui_elements: Vec::new(),
            // Empty by default: no OEM icon → the oemIcon* keys are omitted from `/info`.
            oem_icons: Vec::new(),
            oem_icon_label: String::new(),
            oem_icon_visible: false,
        }
    }
}

/// HID touchscreen report descriptor (C `HIDTouchScreenSingleCreateDescriptor`): a fixed template
/// with the X/Y logical maxima patched to `width`/`height` (little-endian) at offsets 39/40 + 52/53.
fn touchscreen_descriptor(width: u16, height: u16) -> Vec<u8> {
    let mut d: Vec<u8> = vec![
        0x05, 0x0D, 0x09, 0x04, 0xA1, 0x01, // Digitizer / Touch Screen / Collection(App)
        0x05, 0x0D, 0x09, 0x22, 0xA1, 0x02, // Digitizer / Finger / Collection(Logical)
        0x05, 0x0D, 0x09, 0x33, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81,
        0x02, // Touch bit
        0x75, 0x07, 0x95, 0x01, 0x81, 0x01, // 7-bit constant pad
        0x05, 0x01, 0x09, 0x30, 0x15, 0x00, 0x26, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x01, 0x81,
        0x02, // X
        0x09, 0x31, 0x15, 0x00, 0x26, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x01, 0x81, 0x02, // Y
        0xC0, 0xC0, // End, End
    ];
    d[39] = width as u8;
    d[40] = (width >> 8) as u8;
    d[52] = height as u8;
    d[53] = (height >> 8) as u8;
    d
}

/// HID two-finger touchscreen descriptor (C `HIDTouchScreenMultiCreateDescriptor`,
/// `Platform/HIDTouchScreen.c:140`), transcribed byte for byte from the licensed R14G17 source.
///
/// It is the single-finger template with a second `Finger` logical collection appended, and a
/// `Usage (Transducer Index)` byte (0x09 0x38, Report Size 8) added ahead of each finger's touch
/// bit. That index is how iOS tracks a contact's identity across reports.
///
/// Apple declares exactly TWO fingers. That is the whole capability — enough for pinch, zoom and
/// rotate, which is what Maps needs — and it is deliberately not extended: there is no licensed
/// reference for a wider shape, and a guessed HID descriptor is what broke this box on 2026-07-06.
///
/// The four geometry patch sites are Apple's own offsets (`HIDTouchScreen.c:232-238`): width at
/// 0x2F/0x30 and 0x6E/0x6F, height at 0x3C/0x3D and 0x7B/0x7C — each finger carries its own
/// logical maxima, so all four must be patched or the second contact reports in a different
/// coordinate space than the first.
fn touchscreen_multi_descriptor(width: u16, height: u16) -> Vec<u8> {
    let finger: [u8; 63] = [
        0x05, 0x0D, 0x09, 0x22, 0xA1, 0x02, // Digitizer / Finger / Collection(Logical)
        0x05, 0x0D, 0x09, 0x38, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, // Transducer Index (8 bit)
        0x09, 0x33, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02, // Touch bit
        0x75, 0x07, 0x95, 0x01, 0x81, 0x01, // 7-bit constant pad
        0x05, 0x01, 0x09, 0x30, 0x15, 0x00, 0x26, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x01, 0x81,
        0x02, // X
        0x09, 0x31, 0x15, 0x00, 0x26, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x01, 0x81, 0x02, // Y
        0xC0, // End Collection (Finger)
    ];
    let mut d: Vec<u8> = vec![0x05, 0x0D, 0x09, 0x04, 0xA1, 0x01]; // Digitizer / Touch Screen / Collection(App)
    d.extend_from_slice(&finger);
    d.extend_from_slice(&finger);
    d.push(0xC0); // End Collection (Application)

    // Apple's literal offsets, asserted rather than computed so a transcription slip cannot silently
    // patch the wrong byte. Each site must be the 0xFF of a `Logical Maximum (0x7FFF)` placeholder.
    for (lo, hi, v) in [
        (0x2F, 0x30, width),
        (0x3C, 0x3D, height),
        (0x6E, 0x6F, width),
        (0x7B, 0x7C, height),
    ] {
        debug_assert_eq!(
            (d[lo], d[hi]),
            (0xFF, 0x7F),
            "multi descriptor patch site {lo:#x} is not a 0x7FFF placeholder"
        );
        d[lo] = v as u8;
        d[hi] = (v >> 8) as u8;
    }
    d
}

/// HID media-buttons report descriptor (C `HIDMediaButtonsCreateDescriptor`), fixed.
fn media_buttons_descriptor() -> Vec<u8> {
    // Consumer-Control ARRAY device (uid 2) — MEDIA TRANSPORT ONLY. Report = a single byte index into
    // this usage list (0 = released). This is the proven, device-verified media descriptor; Home /
    // Back / D-pad navigation live on the separate uid-3 D-Pad device (HIDDPadCreateDescriptor) instead,
    // because CarPlay's list Select needs a Generic-Desktop Button which a Consumer array can't express
    // (2026-07-12: a Consumer-array Home+Menu extension made the focus
    // overlay appear but Up/Down didn't scroll and Menu-Pick didn't select).
    vec![
        0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, // Consumer / Consumer Control / Collection(App)
        0x15, 0x00, 0x25, 0x05, 0x05, 0x0C, // Logical Min 0 / Max 5 / Consumer
        0x0A, 0x00, 0x00, // Usage 0 (unassigned)
        0x0A, 0xB0, 0x00, // Play
        0x0A, 0xB1, 0x00, // Pause
        0x0A, 0xCD, 0x00, // Play/Pause
        0x0A, 0xB5, 0x00, // Next
        0x0A, 0xB6, 0x00, // Previous
        0x75, 0x08, 0x95, 0x01, 0x81, 0x00, // Report Size 8 / Count 1 / Input(Array)
        0xC0, // End
    ]
}

/// The CarPlay **D-Pad** HID device (uid 3) — Apple's EXACT `HIDDPadCreateDescriptor` bytes
/// (0x27 = 39 B), read verbatim from the Xcode-local CarPlaySDK (`_HIDDPadCreateDescriptor.
/// kDescriptorTemplate` @ file offset 0x2dd6ec). This is the DISCRETE directional pad — distinct
/// from the rotary Knob (`HIDKnobCreateDescriptor`, a relative wheel + nudge), which is a SEPARATE
/// device for a physical rotating encoder and is NOT what a Up/Down/Left/Right/Select pad is.
///
/// A Consumer **variable bitfield** (NOT an array), report = **2 bytes**. Bit map verified against
/// `HIDDPadFillReport` disassembly:
///   byte0: bit0 = AC Home (0x0223) · bit1 = AC Back (0x0224) · bits 2-7 pad
///   byte1: bit0 = Menu (0x40) · bit1 = Menu Pick/**Select** (0x41) · bit2 = Menu Up (0x42)
///          bit3 = Menu Down (0x43) · bit4 = Menu Left (0x44) · bit5 = Menu Right (0x45) · bits 6-7 pad
///
/// The earlier attempt put these usages in the media Consumer ARRAY (report = one index); CarPlay's
/// HID parser only routes them as D-pad navigation from THIS exact variable-bitfield device — which
/// is why Menu Up/Down and Menu-Pick did nothing from the array (2026-07-12).
///
/// ⚠️ This is a THIRD hidDevices entry. The 2026-07-06 INCIDENT (a GUESSED knob descriptor broke
/// reconnect) is why this uses Apple's exact bytes; hardware-validate that the session still
/// establishes, and if it ever regresses, drop this device (return to 2) as the first step.
fn dpad_descriptor() -> Vec<u8> {
    vec![
        0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, // Consumer / Consumer Control / Collection(App)
        0x15, 0x00, 0x25, 0x01, 0x75, 0x01, // Logical Min 0 / Max 1 / Report Size 1
        0x0A, 0x23, 0x02, 0x0A, 0x24, 0x02, // Usage AC Home (0x0223) / AC Back (0x0224)
        0x95, 0x02, 0x81, 0x02, // Count 2 / Input(Var) — byte0 bits 0-1
        0x95, 0x06, 0x81, 0x01, // Count 6 / Input(Const) — byte0 pad
        0x19, 0x40, 0x29,
        0x45, // Usage Min 0x40 / Max 0x45 (Menu / Pick / Up / Down / Left / Right)
        0x95, 0x06, 0x81, 0x02, // Count 6 / Input(Var) — byte1 bits 0-5
        0x95, 0x02, 0x81, 0x01, // Count 2 / Input(Const) — byte1 pad
        0xC0, // End
    ]
}

/// Apple's exact `HIDKnobCreateDescriptor` (70 bytes, byte-for-byte from the shipping
/// CarPlaySDK.framework — the "full" knob, logged length 70). This is the device the CarPlay Simulator
/// drives ALL of its hardware navigation through: rotation moves the selector, the two signed X/Y axes
/// carry the four arrows (±127 = a nudge), and the Select button picks the highlighted item.
///
/// Report = 4 bytes: [0] bit0 Select · bit1 AC-Home · bit2 AC-Back (+5 pad); [1] X (signed −127..127,
/// left/right nudge); [2] Y (signed, down/up nudge); [3] Wheel (RELATIVE signed, ±1 per detent).
///
/// ⚠️ FOURTH hidDevices entry — same reconnect-incident class as the D-Pad (2026-07-06). These are
/// Apple's EXACT bytes (the incident was a GUESSED descriptor) — **VERIFIED byte-for-byte against
/// R14G17 `Platform/HIDKnob.c::HIDKnobCreateDescriptor` on 2026-08-10: all 70 bytes identical**, so
/// this is no longer a claim. Gated behind CARPLAY_KNOB, default off,
/// instantly revertible (drop back to the proven 2-device set). Hardware-validate reconnect before use.
#[rustfmt::skip]
fn knob_descriptor() -> Vec<u8> {
    vec![
        0x05, 0x01, 0x09, 0x08, 0xA1, 0x01, // Generic Desktop / Multi-axis Controller / Collection(App)
        0x05, 0x09, 0x09, 0x01, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02, // Button 1 (Select) → byte0 bit0
        0x05, 0x0C, 0x0A, 0x23, 0x02, 0x0A, 0x24, 0x02, 0x95, 0x02, 0x81, 0x02, // Consumer AC Home/Back → byte0 bits 1-2
        0x95, 0x05, 0x81, 0x01, // byte0 pad (5 bits)
        0x05, 0x01, 0x09, 0x01, 0xA1, 0x00, // Generic Desktop / Pointer / Collection(Physical)
        0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02, 0xC0, // X,Y signed 8-bit → bytes 1,2
        0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xC0, // Wheel signed RELATIVE → byte3
    ]
}

/// Apple's EXACT telephony HID descriptor (R14G17 `HIDTelephony.c` `HIDTelephonyCreateDescriptor`)
/// — **VERIFIED byte-for-byte 2026-08-10: all 57 bytes identical.** One 8-bit ARRAY report: the value is a 1-based index into the usage list —
/// 0 = release, 1 = Hook Switch (answer/off-hook), 2 = Flash, 3 = Drop (end/hang-up), 4 = Mute,
/// 5..14 = DTMF 0..9, 15 = `*`, 16 = `#`, 17 = Delete. Gated behind CARPLAY_TELEPHONY (a FIFTH
/// hidDevices entry — same reconnect-incident class as the D-Pad/Knob; default off, instantly revertible).
#[rustfmt::skip]
fn telephony_descriptor() -> Vec<u8> {
    vec![
        0x05, 0x0B, 0x09, 0x07, 0xA1, 0x01, // Usage Page (Telephony) / Telephony Keypad / Collection(App)
        0x15, 0x00, 0x25, 0x11,             // Logical min 0, max 17
        0x05, 0x0B,                         // Usage Page (Telephony)
        0x09, 0x00, 0x09, 0x20, 0x09, 0x21, 0x09, 0x26, 0x09, 0x2F, // Unassigned/HookSwitch/Flash/Drop/Mute
        0x09, 0xB0, 0x09, 0xB1, 0x09, 0xB2, 0x09, 0xB3, 0x09, 0xB4, // PhoneKey 0-4
        0x09, 0xB5, 0x09, 0xB6, 0x09, 0xB7, 0x09, 0xB8, 0x09, 0xB9, // PhoneKey 5-9
        0x09, 0xBA, 0x09, 0xBB,             // PhoneKey Star / Pound
        0x05, 0x07, 0x09, 0x2A,             // Usage Page (Keyboard) / Keyboard DELETE
        0x75, 0x08, 0x95, 0x01, 0x81, 0x00, // Report Size 8, Count 1, Input (Data,Array,Absolute)
        0xC0,                               // End Collection
    ]
}

/// One HID-device dict (C `AirPlayInfoArrayAddHIDDevice`): `uuid` is the UID as uppercase hex.
/// ⚠️ EVERY HID device binds to the single `DISPLAY_UUID`, and that is a ONE-WAY DOOR.
///
/// `hidDevices[].displayUUID` is a foreign key into `displays[].uuid`, and the association is
/// established EXACTLY ONCE, at `/info` time. `AirPlayReceiverSessionSendHIDReport`
/// (R14G17 `AirPlayReceiverSession.c:5404-5422`) puts only the HID uid and the report bytes on the
/// wire — there is no display or stream selector. So if every declared HID device points at the main
/// display, no report can EVER reach an alt display, and no later message or config can recover it
/// (docs/carplay/03_SDK_GROUND_TRUTH.md §2, established by disassembling all seven `_AirPlayInfoArrayAddHIDDevice` call sites).
///
/// This is CORRECT as things stand — we advertise one display (type 110) and the alt/cluster stream
/// (111) is forward-gated default-OFF, so there is no second input target. It becomes a REAL limit
/// the moment anyone wants input on the cluster (a knob driving cluster content, say).
///
/// Two further traps from the same source, for whoever does that work:
///   * Apple builds HID PER VIDEO STREAM — each stream gets a complete independent device set, and
///     the binding key is the VIDEO-STREAM uuid (the `type: 111` entry's `uuid`), NOT
///     `displayPanels[].uid`.
///   * IDs are allocated at RUNTIME by a counter carried across streams, and `steeringWheelID`
///     consumes an id while emitting NO device — so the emitted ids are not dense. Replicate the
///     allocator; never hardcode `1..4 / 5..8`, and bind by id rather than array index (Apple's own
///     emission order is not stable).
fn hid_device(uid: u32, name: &str, product_id: i64, descriptor: Vec<u8>) -> Value {
    let mut d = Dictionary::new();
    d.insert("uuid".into(), Value::String(format!("{uid:X}")));
    d.insert("name".into(), Value::String(name.into()));
    d.insert("displayUUID".into(), Value::String(DISPLAY_UUID.into()));
    d.insert("hidProductID".into(), Value::Integer(product_id.into()));
    d.insert("hidVendorID".into(), Value::Integer(HID_VENDOR_ID.into()));
    d.insert(
        "hidCountryCode".into(),
        Value::Integer(HID_COUNTRY_CODE.into()),
    );
    d.insert("hidDescriptor".into(), Value::Data(descriptor));
    Value::Dictionary(d)
}

/// One-entry `viewAreas` array for a display: a full-panel `viewArea` (the whole coded frame is
/// visible content — the video always fills the rectangle) with a `safeArea` that MAY be inset —
/// the rectangle CarPlay keeps its interactive UI inside, for curved/occluded panels.
///
/// Structure + KEY NAMES are locked to the genuine wired session
/// (`research/ios27_sdk_inventory/CAPTURE_VALIDATION_04_info_keys.md` §1.1): the entry is FLAT (rect
/// fields directly on it) with a nested `safeArea`, and the rect keys are
/// `originXPixels/originYPixels/widthPixels/heightPixels` — NOT the SDK-YAML `originX/width` names.
/// The captured cluster display showed exactly this: a full 1920-wide viewArea with a 100px-inset
/// (originX 100, width 1720) safeArea.
///
/// `safe` = the requested safe rectangle (absolute px) or `None` for full-bleed. It is VALIDATED to
/// sit within the panel; an out-of-bounds or degenerate rect falls back to full-bleed rather than
/// emitting a config iOS would reject.
/// `is_main` = this is the type-110 MAIN screen.
///
/// ⚠️ TYPE-GATING ADDED 2026-07-30. `_AirPlayScreenDictSetViewAreas` (CarPlaySDK @0x26d340) reads the
/// screen dict's `type` and emits `viewAreaTransitionControl`, `viewAreaStatusBarEdge`,
/// `viewAreaSupportsFocusTransfer` and the safeArea's `drawUIOutsideSafeArea` **only when
/// `type == 110`** (`cmp x19,#0x6e` @0x26d420 and @0x26d4e0). We were emitting the first two on the
/// type-111 ALT screen as well, where Apple emits none of them — an alt entry gets the four rect keys
/// plus a bare four-key `safeArea` and nothing else.
///
/// Also added: `viewAreaSupportsFocusTransfer`, which Apple writes UNCONDITIONALLY on every type-110
/// entry (as `false` when the feature is off). We omitted it entirely. Our knob HID device is
/// config-gated (uid-4, `levers::knob()` below) and off unless the pushed `hidConfig.knobSupport`
/// asks for it, so
/// nothing of ours needs focus transfer, but the key belongs in the shape.
fn view_areas(
    width: i64,
    height: i64,
    safe: Option<(i64, i64, i64, i64)>,
    draw_outside: bool,
    is_main: bool,
) -> Value {
    // cornerMasks (Phase 1 experiment) is a DISPLAY-level flag (set on the `disp` dict, not here — iOS's
    // validator reads it only from displays[], never from a viewArea entry). Its ONE viewArea consequence:
    // the display that declares cornerMasks must NOT carry a `safeArea` in its viewArea, or the validator
    // hard-fails "cornerMasks flag set but a safeArea defined in viewAreas" (device-confirmed; AirPlaySender
    // disasm @0x2516d0030). So on the main view, when the lever is on, we simply OMIT safeArea. Main-only
    // (matches the type-110 gating); the alt view keeps its safeArea (it has no cornerMasks flag).
    let masks = is_main && crate::levers::cornermasks();

    let (sx, sy, sw, sh) = safe
        .filter(|&(x, y, w, h)| {
            x >= 0 && y >= 0 && w > 0 && h > 0 && x + w <= width && y + h <= height
        })
        .unwrap_or((0, 0, width, height));

    let mut area = Dictionary::new();
    area.insert("originXPixels".into(), Value::Integer(0.into()));
    area.insert("originYPixels".into(), Value::Integer(0.into()));
    area.insert("widthPixels".into(), Value::Integer(width.into()));
    area.insert("heightPixels".into(), Value::Integer(height.into()));
    if is_main {
        // type-110 only — see the type-gating note above. `viewAreaStatusBarEdge: 0` = Auto
        // (`StatusBarEdge` ordinals: Auto=0, Bottom=1, Driver=2).
        area.insert("viewAreaTransitionControl".into(), Value::Boolean(false));
        area.insert("viewAreaStatusBarEdge".into(), Value::Integer(0.into()));
        // docs/carplay/04_CAPABILITIES_AND_CONFIG.md #25: was hardcoded `false` while the app carried an `enablesFocusTransfer` toggle.
        // The lever defaults `false`, so this is byte-identical unless the owner opts in — and opting
        // in is a NEW advertisement, unvalidated on hardware, not a byte-neutral plumbing change.
        area.insert(
            "viewAreaSupportsFocusTransfer".into(),
            Value::Boolean(crate::levers::focus_transfer()),
        );
    }
    if masks {
        // cornerMasks is declared at the DISPLAY level (see the disp `cornerMasks` insert); this view
        // just OMITS safeArea, which is mandatory — iOS's carEndpoint_checkCarPlayFeatureAcceptance
        // hard-fails "cornerMasks flag set but a safeArea defined in viewAreas" if a cornerMasks display
        // has a viewArea safeArea (device-confirmed + AirPlaySender disasm @0x2516d0030). No per-view
        // cornerMasks key exists — the validator never reads one from a viewArea entry.
    } else {
        let mut safe_d = Dictionary::new();
        safe_d.insert("originXPixels".into(), Value::Integer(sx.into()));
        safe_d.insert("originYPixels".into(), Value::Integer(sy.into()));
        safe_d.insert("widthPixels".into(), Value::Integer(sw.into()));
        safe_d.insert("heightPixels".into(), Value::Integer(sh.into()));
        if is_main {
            safe_d.insert("drawUIOutsideSafeArea".into(), Value::Boolean(draw_outside));
        }
        area.insert("safeArea".into(), Value::Dictionary(safe_d));
    }

    Value::Array(vec![Value::Dictionary(area)])
}

/// Declare per-display **appearance support** in a `/info` `displays[]` entry. CarPlaySDK's
/// `_AirPlayScreenDictAddUIAppearance` (@0x26d5bc) / `_AirPlayScreenDictAddMapAppearance` (@0x26d604)
/// add exactly these four int keys — `uiAppearanceMode` / `uiAppearanceSetting` / `mapAppearanceMode` /
/// `mapAppearanceSetting` — to the screen dict. WITHOUT them iOS never learns the display supports
/// appearance, so it **silently drops** the runtime `uiAppearanceUpdate` / `mapAppearanceUpdate` commands
/// for that display.
///
/// HISTORY, because the failure and the fix are easy to conflate: on 2026-08-02 the per-display
/// sun/moon did nothing while the *global* `setNightMode` (which has no per-display gate) worked —
/// that is the symptom of these four keys being absent. **CORRECTED 2026-08-11, owner-confirmed on
/// hardware: the per-display sun/moon WORKS.** So read the 08-02 line as the historical symptom of a
/// missing declaration, NOT as a standing statement that per-display appearance is broken. Anyone
/// removing these keys re-creates the 08-02 behaviour; note docs/carplay/04_CAPABILITIES_AND_CONFIG.md #25 now gates their emission on
/// `enablesUIAppearance` / `enablesMapAppearance`, so turning those toggles OFF is the supported way
/// to reproduce it deliberately.
///
/// Initial state = Light(0) / Automatic(0); the
/// runtime command then drives the live appearance. (Enum values match `events.rs`: mode Light=0/Dark=1,
/// setting Automatic=0.)
///
/// GATED SINCE docs/carplay/04_CAPABILITIES_AND_CONFIG.md #25. These were emitted unconditionally while the app shipped owner toggles
/// for both — the box deciding a value the app owns. Both levers default `true`, so an unconfigured
/// box emits exactly what it always did; only an owner deliberately turning a toggle off changes the
/// wire. The runtime senders in `events.rs` are unaffected either way.
fn add_appearance_keys(d: &mut Dictionary) {
    if crate::levers::ui_appearance() {
        d.insert("uiAppearanceMode".into(), Value::Integer(0.into()));
        d.insert("uiAppearanceSetting".into(), Value::Integer(0.into()));
    }
    if crate::levers::map_appearance() {
        d.insert("mapAppearanceMode".into(), Value::Integer(0.into()));
        d.insert("mapAppearanceSetting".into(), Value::Integer(0.into()));
    }
}

/// Build the `/info` binary plist bytes from `cfg`.
pub fn build_info(cfg: &DeviceConfig) -> Vec<u8> {
    let mut d = Dictionary::new();

    // audioFormats — the 8 wireless-CarPlay entries (C `_BuildAudioFormatsArray`).
    d.insert("audioFormats".into(), Value::Array(audio_formats(cfg)));

    // modes — the InitialMode. iOS reads this at endpoint activation; without it the screen resource
    // is type=N/A → StartupFailed -17483 → teardown right after RECORD/modesChanged.
    //
    // Declare the accessory TAKES MainScreen(1) + MainAudio(2). (C: AirPlayCreateModesDictionary;
    // transferType Take=1, transferPriority NiceToHave=100, constraint Anytime=100.) Matches carplayd.
    //
    // AUDIO INVESTIGATION 2026-07-02 (DISPROVEN, kept as Take): a live decoded modesChanged showed
    // MainScreen→entity 1 (iOS owns → video) but MainAudio STAYS entity=2/permanentEntity=2 (accessory)
    // → no media routing. Hypothesis: declaring Take makes the car the permanent audio owner. Tested
    // MainAudio=Untake(2) → NO change (MainAudio still entity=2), and the working CCPA capture ALSO had
    // MainAudio permanentEntity=2 yet routed audio — so permanentEntity=2 is NORMAL and the seed
    // transferType is NOT the lever. Reverted to Take(1) to match carplayd. The real blocker is iOS's
    // routing decision (it sends disableBluetooth but never borrows our MainAudio) — see docs/ writeup.
    let mode_resource = |rid: i64| -> Value {
        let mut r = Dictionary::new();
        r.insert("resourceID".into(), Value::Integer(rid.into()));
        r.insert("transferType".into(), Value::Integer(1.into())); // Take
        r.insert("transferPriority".into(), Value::Integer(100.into())); // NiceToHave
        r.insert("takeConstraint".into(), Value::Integer(100.into())); // Anytime
        r.insert("borrowConstraint".into(), Value::Integer(100.into())); // Anytime
        Value::Dictionary(r)
    };
    let mut modes = Dictionary::new();
    modes.insert(
        "resources".into(),
        Value::Array(vec![mode_resource(1), mode_resource(2)]),
    );
    d.insert("modes".into(), Value::Dictionary(modes));

    // audioLatencies — one catch-all entry (type/audioType/sr/ss/ch omitted ⇒ applies to all).
    let mut lat = Dictionary::new();
    lat.insert("inputLatencyMicros".into(), Value::Integer(0.into()));
    lat.insert("outputLatencyMicros".into(), Value::Integer(0.into()));
    d.insert(
        "audioLatencies".into(),
        Value::Array(vec![Value::Dictionary(lat)]),
    );

    d.insert("deviceID".into(), Value::String(cfg.device_id.clone()));

    // displays — one main display (type 110) carrying a full-panel `viewAreas`+`safeArea` (matches the
    // genuine wired receiver, CAPTURE_VALIDATION_04 §1.1/Q3 — Apple always ships them). This is advertised
    // as a *capability*; it is only *activated* once the SETUP response echoes "viewAreas" in
    // `enabledFeatures` (Phase 3, with on-device validation). Advertising the structure without echoing is
    // harmless — the teardown risk runs the other way (echoing a feature whose /info structure is missing).
    // uuid == the HID displayUUID.
    let mut disp = Dictionary::new();
    disp.insert("uuid".into(), Value::String(DISPLAY_UUID.into()));
    // Display feature bits gate which HID inputs iOS ROUTES to this display.
    //
    // ⚠️ THE BIT NAMES HERE WERE WRONG UNTIL 2026-07-30, and docs/carplay/05_METADATA_AND_CONTROLS.md §2.7.4 still needs the same
    // correction. The authoritative mapping, confirmed independently by BOTH normative sources —
    // R14G17 `AppleCarPlay/Sources/AirPlayCommon.h:210-213`, and the Simulator's
    // `DisplayFeatures.init(airPlayValue:)` / `.rawValue` in CarPlaySDK — is:
    //
    //     0x02 Knobs   0x04 LowFidelityTouch   0x08 HighFidelityTouch
    //     0x10 Touchpad                        0x20 DirectionButtons
    //
    // So the old comment ("HighFidelityTouch 0x02 + Knobs 0x08 + Direction Buttons 0x10") had all
    // three labels permuted, and 0x10 is **Touchpad**, not Direction Buttons. The claim that D-pad
    // routing REQUIRES 0x10 is unfounded: in `HIDConfig.displayFeatures` the 0x10 bit is ORed from
    // `touchpadSupport` and 0x20 from `steeringWheelSupport`, while `dPadSupport` contributes
    // NOTHING to this word — it only gates the D-Pad `hidDevices[]` entry. docs/carplay/05_METADATA_AND_CONTROLS.md §2.7.4 misread
    // `[HIDConfig+0x24]` as the D-pad bool; +0x24 is `touchpadSupport`.
    //
    // ⚠️ CONFIRMED 2026-07-30: BOTH bits we set are unbacked by a device.
    //   `HIDConfig.displayFeatures` (@0x1002ca40c) drives the word from four properties, and Apple
    //   pairs two of them with an actual `hidDevices[]` entry:
    //     0x02 Knobs    <- knobSupport (+0x18)        -> ALSO registers a "Knob" device
    //                                                    (`HIDController.reset` @0x10007f514 allocates
    //                                                     knobID; `airPlayHID` @0x10007f8c0 calls
    //                                                     addKnobDevice @0x100080184 FIRST)
    //     0x04 / 0x08   <- touchScreenMode (+0x21)    -> "Touch Screen"
    //     0x10 Touchpad <- touchpadSupport (+0x24)    -> "Touchpad" (addTouchpadDevice @0x1000833f8)
    //     0x20 DirBtns  <- steeringWheelSupport(+0x39)-> no device of its own
    //   `telephonyButtonsSupport` and `mediaButtonsSupport` contribute NO bit at all — so our media
    //   device correctly needs none.
    //   We therefore claim **Knobs (0x02) with no knob device** on the DEFAULT build, and additionally
    //   **Touchpad (0x10) with no touchpad device** under CARPLAY_DPAD. The honest value for what we
    //   actually ship (touchscreen + media buttons, optional D-Pad) is 0x08 alone.
    //
    // WHAT WE EMIT IS DELIBERATELY UNCHANGED. 0x0A/0x1A is hardware-validated (clean session with
    // the D-Pad advertised), and the wired path is the proven baseline. Note the
    // value is defensible by accident: Apple's own `Standard.yaml` (knobSupport + High Fidelty +
    // touchpadSupport + dPadSupport) also yields 0x1A. But be clear about what it now means —
    // 0x0A = Knobs|HighFidelityTouch, and 0x1A additionally claims **Touchpad**, a capability we
    // back with no `hidDevices[]` entry. Correcting that (drop 0x10; add 0x20 only if a steering
    // wheel is ever declared) is a wire change and needs a hardware session, not a desk edit.
    let disp_features: i64 = if crate::levers::dpad() { 0x1A } else { 0x0A };
    disp.insert("features".into(), Value::Integer(disp_features.into()));
    // primaryInputDevice: value 0 = **Undeclared** (NOT "touchscreen"). The genuine wired session runs
    // with `primaryInputDevice: 0` and touch still binds via the hidDevices[] Digitizer (displayUUID
    // match), so KEEP 0 — the capture overrides the SDK-based facet-11 suggestion to set it to 1.
    disp.insert("primaryInputDevice".into(), Value::Integer(0.into()));
    disp.insert("maxFPS".into(), Value::Integer(cfg.max_fps.into()));
    disp.insert(
        "widthPixels".into(),
        Value::Integer(cfg.display_width.into()),
    );
    disp.insert(
        "heightPixels".into(),
        Value::Integer(cfg.display_height.into()),
    );
    // Physical dims: the genuine wired MAIN display sent 0/0 (omitted/unknown); the prior 240×90 was
    // invented (facet-08 CORRECTION-1). Match the genuine main = 0/0.
    disp.insert("widthPhysical".into(), Value::Integer(0.into()));
    disp.insert("heightPhysical".into(), Value::Integer(0.into()));
    disp.insert("initialViewArea".into(), Value::Integer(0.into()));
    disp.insert("adjacentViewAreas".into(), Value::Array(Vec::new()));
    disp.insert(
        "viewAreas".into(),
        view_areas(
            cfg.display_width,
            cfg.display_height,
            cfg.main_safe_area,
            cfg.main_draw_outside_safe,
            true, // type-110 main screen
        ),
    );
    // cornerMasks at the DISPLAY (screen) level — `CARScreenInfo.wantsCornerMasks` is a screen-level
    // BOOL, and iOS's `carEndpoint_checkCarPlayFeatureAcceptance` iterates screens for the flag ("not set
    // for any view" fired when it was only on the viewArea entry). Set on the main screen under the lever.
    if crate::levers::cornermasks() {
        disp.insert("cornerMasks".into(), Value::Boolean(true));
    }
    add_appearance_keys(&mut disp);
    disp.insert("type".into(), Value::Integer(110.into()));

    // ALT / cluster display (docs/carplay/06_AV_PIPELINE.md), gated behind CARPLAY_ALTSCREEN (host YAML altVideoStreams).
    // A 2nd displays[] entry of the instrument-cluster type (111 / Cluster_Display) so iOS offers the
    // type-111 screen stream. Mirrors the main dict shape with a DISTINCT uuid + alt pixel size.
    // ⚠️ Advertising a capability whose /info is incomplete risks RECORD→TEARDOWN (docs/carplay/06_AV_PIPELINE.md) — this is
    // the flag-gated, hardware-validated lever; if a session tears down with it on, unset the flag.
    let mut displays = vec![Value::Dictionary(disp)];
    if crate::levers::altscreen() {
        let aw: i64 = crate::levers::alt_w().unwrap_or(800);
        let ah: i64 = crate::levers::alt_h().unwrap_or(480);
        let mut alt = Dictionary::new();
        alt.insert("uuid".into(), Value::String(ALT_DISPLAY_UUID.into()));
        alt.insert("features".into(), Value::Integer(0.into()));
        alt.insert("primaryInputDevice".into(), Value::Integer(0.into()));
        // Use the alt stream's OWN maxFPS when the YAML supplied one; 0 = inherit the main stream's.
        let afps = if cfg.alt_max_fps > 0 { cfg.alt_max_fps } else { cfg.max_fps };
        alt.insert("maxFPS".into(), Value::Integer(afps.into()));
        alt.insert("widthPixels".into(), Value::Integer(aw.into()));
        alt.insert("heightPixels".into(), Value::Integer(ah.into()));
        alt.insert("widthPhysical".into(), Value::Integer(0.into()));
        alt.insert("heightPhysical".into(), Value::Integer(0.into()));
        alt.insert("initialViewArea".into(), Value::Integer(0.into()));
        alt.insert("adjacentViewAreas".into(), Value::Array(Vec::new()));
        alt.insert(
            "viewAreas".into(),
            view_areas(aw, ah, cfg.alt_safe_area, cfg.alt_draw_outside_safe, false),
        );
        add_appearance_keys(&mut alt);
        alt.insert("type".into(), Value::Integer(111.into())); // Cluster / AltScreen
        alt.insert("showsInstruments".into(), Value::Boolean(true));
        alt.insert(
            "initialURL".into(),
            Value::String("maps:/car/instrumentcluster/map".into()),
        );
        displays.push(Value::Dictionary(alt));
        // Supported cluster content-type URLs. iOS only encodes content types the cluster DECLARES it
        // supports (`setAllowedContentTypes:`), so advertising only `/map` told iOS the cluster is
        // map-only and it never rendered the maneuver/ETA INSTRUCTION CARD. Match the genuine CCPA box
        // on this same hardware, which advertised all three (WIRED capture ttylog:4507-4511): map +
        // instructioncard + the base cluster URL. Now iOS can composite the maneuver banner / card.
        let cluster_urls = || {
            vec![
                Value::String("maps:/car/instrumentcluster/map".into()),
                Value::String("maps:/car/instrumentcluster/instructioncard".into()),
                Value::String("maps:/car/instrumentcluster".into()),
            ]
        };
        d.insert("altScreenURLs".into(), Value::Array(cluster_urls()));
        // The genuine box also advertised a suggest-UI list (ttylog:4677-4680).
        d.insert(
            "altScreenSuggestUIURLs".into(),
            Value::Array(cluster_urls()),
        );
    }
    d.insert("displays".into(), Value::Array(displays));

    // HEVC (env-gated, CORRECTED 2026-07-04 vs a reference /info from `f-io` that reports HEVC working
    // — likely WIRELESS; credit f-io; see docs/carplay/06_AV_PIPELINE.md). Corrections from that comparison:
    //   • `hevcInfo = {}` (empty dict, presence-only) — already correct; the reference sets ONLY this.
    //   • `extendedFeatures` MUST be `["vocoderInfo","enhancedRequestCarUI"]` (the reference + GM r17
    //     always send this pair). Our old `["hevc"]` was a CATEGORY ERROR — `"hevc"` is a SETUP feature
    //     token, not an extended feature — and it overwrote the real values.
    //   • `enabledFeatures` is NOT a valid `/info` key at all (it is the SETUP-RESPONSE field, carrying
    //     e.g. ["altScreen","viewAreas"]); removed.
    //   • The base `features` u64 has NO HEVC bit — do not touch it for HEVC.
    // Reconciliation with our wired block (docs/carplay/06_AV_PIPELINE.md): `hevcInfo` alone is sufficient ONLY IF iOS's
    // NEGOTIATED feature list also carries `hevc`. The reference gets that coupling on its (wireless)
    // transport; on WIRED it must be seeded via the SETUP-response `enabledFeatures` array (session.rs,
    // still the wired-specific unproven lever). So on wired, expect this may still RECORD→TEARDOWN;
    // test WIRELESS first (the reference's proven transport). Requires a FRESH pair. Default = H.264.
    if crate::levers::hevc() {
        d.insert("hevcInfo".into(), Value::Dictionary(Dictionary::new()));
    }
    // `extendedFeatures` — HOISTED OUT OF THE HEVC BLOCK 2026-07-30. It was previously inserted only
    // when `CARPLAY_HEVC` was set, so on a default (H.264) build we sent NO `extendedFeatures` key at
    // all. Apple sets it UNCONDITIONALLY: `AirPlayCopyServerInfo` queries the platform property with
    // no gate (CarPlaySDK 509.11 @0x580c-0x5848), and the Simulator's InfoRequest asks for it on every
    // session (cp.log:1251, :5657) — in a capture where HEVC was DISABLED. It has nothing to do with
    // HEVC; the two were only ever adjacent in our code.
    //
    // The pair itself is confirmed correct: `VehicleConfig.extendedFeaturesArray` (Simulator app
    // @0x10010d3a0) builds exactly `["enhancedRequestCarUI", "vocoderInfo"]` from two independent
    // Bools. We emit the same pair (order differs; it is an unordered capability array).
    d.insert(
        "extendedFeatures".into(),
        Value::Array(vec![
            Value::String("vocoderInfo".into()),
            Value::String("enhancedRequestCarUI".into()),
        ]),
    );
    // iAPChannel (docs/wireless/00_WIRELESS_CARPLAY.md, docs/carplay/03_SDK_GROUND_TRUTH.md §"SETUP feature-intersection gate"): the iAP2-over-AirPlay tunnel
    // (`iAPSendMessage`, events.rs) is itself a SETUP-feature-intersection-gated capability, exactly
    // like `hevc` above — `/info` must advertise `iAPChannelInfo = {}` (presence-only, same shape as
    // `hevcInfo`) AND the SETUP-response `enabledFeatures` (session.rs) must echo `"iAPChannel"` back,
    // or iOS never negotiates the channel. The token is a real CarPlay SETUP feature key: CarPlaySDK
    // 509.11 has `enabledFeatures` -> `AirPlayCopyAccessoryEnabledFeatures` -> "Enabling iAP Channel
    // support" -> `iAPChannel`, and iOS 27's `carEndpoint_createSetupRequestFeatureList` proposes the
    // pair `iAPChannel`/`enableiAPChannel` one entry below `logTransfer` (docs/carplay/03_SDK_GROUND_TRUTH.md §3 correction).
    // The ECHO is the load-bearing half: `carEndpoint_createiAPChannelIfNeeded` is what opens the
    // stream-130 RCS channel. `iAPChannelInfo` itself is NOT in the phone's validated-info-key cluster
    // (`carEndpoint_validateInfoResponseKeyPresentForFeature`) — kept for both-sides-present symmetry,
    // not because anything checks it. NOTE: the "3/3 uniform 400 (2026-07-22)" previously cited here is
    // WITHDRAWN — that run also carried the capital-`Data` bug (docs/wireless/00_WIRELESS_CARPLAY.md:33, events.rs), so it could not
    // discriminate. Same env gate as the tunnel send itself so this stays a single on/off lever.
    if std::env::var("CARPLAY_WIRELESS_METADATA").is_ok() {
        d.insert("iAPChannelInfo".into(), Value::Dictionary(Dictionary::new()));
    }
    // logTransfer (docs/carplay/04_CAPABILITIES_AND_CONFIG.md Half A): presence-only dict, same shape as `hevcInfo`/`iAPChannelInfo`.
    // iOS polls this key every session ("AirPlay Requesting Server Value For - logTransferInfo") and
    // validates it via `carEndpoint_validateInfoResponseKeyPresentForFeature`; the other half of the
    // gate is the SETUP `enabledFeatures` `"logTransfer"` echo (session.rs), both on the same lever.
    if crate::levers::logtransfer() {
        d.insert("logTransferInfo".into(), Value::Dictionary(Dictionary::new()));
    }
    // mainBufferedAudio (docs/carplay/04_CAPABILITIES_AND_CONFIG.md Phase A; docs/carplay/04_CAPABILITIES_AND_CONFIG.md B4) — config-primary: the lever is armed per
    // connection from the pushed `enablesMainBufferedAudio` (app default OFF — the old
    // every-session-firing concern is solved app-side), with `CARPLAY_MAINBUFFERED` presence only
    // as the app-less bench fallback. Presence-only {} (leading hypothesis, mirrors hevcInfo; the
    // dict shape is unpinned). Paired with the SETUP enabledFeatures "mainBuffered" echo
    // (session.rs) — both gate on the SAME lever so the coupling validator passes.
    if crate::levers::mainbuffered() {
        d.insert("mainBufferedInfo".into(), Value::Dictionary(Dictionary::new()));
    }
    // sessionManagementInfo (docs/carplay/02_SESSION_LIFECYCLE.md, docs/carplay/05_METADATA_AND_CONTROLS.md §7, docs/carplay/05_METADATA_AND_CONTROLS.md): a SEPARATE experiment from iAPChannel
    // above, deliberately gated on its OWN env var rather than reused — docs/carplay/05_METADATA_AND_CONTROLS.md cross-referenced Apple's
    // own validated-`/info`-key cluster (`carEndpoint_validateInfoResponseKeyPresentForFeature`) and
    // found `sessionManagementInfo` IS in it while `iAPChannelInfo` is NOT, i.e. this may be the actual
    // capability gate the phone-side ACC-transport connection/endpoint registration checks before
    // `iAPSendMessage` delivery works at all. A shared flag would make it impossible to ever test
    // "sessionManagement without iAPChannel," the one experiment that discriminates between the two.
    // `stopSessionReasons` vocabulary: GM CT5's real, shipped 5-value shape (docs/carplay/05_METADATA_AND_CONTROLS.md) reused as our own
    // reason codes — 0=unspecified, 1=received-teardown, 2=out-of-range-active, 3=out-of-range-idle,
    // 4=network-change — not an assertion that GM's exact semantics apply here.
    if std::env::var("CARPLAY_SESSION_MGMT").is_ok() {
        let mut smi = Dictionary::new();
        smi.insert(
            "stopSessionReasons".into(),
            Value::Array(
                [0i64, 1, 2, 3, 4]
                    .iter()
                    .map(|&r| Value::Integer(r.into()))
                    .collect(),
            ),
        );
        d.insert("sessionManagementInfo".into(), Value::Dictionary(smi));
    }

    if cfg.features != 0 {
        d.insert(
            "features".into(),
            Value::Integer((cfg.features as i64).into()),
        );
    }
    d.insert(
        "firmwareRevision".into(),
        Value::String(cfg.firmware_revision.clone()),
    );

    // hidDevices — touchscreen (index 0) + media buttons, sharing the display UUID. The D-Pad (uid 3)
    // is appended only when `CARPLAY_DPAD` is set (airplayd arms it from the host YAML
    // `accessoryConfig.enablesDPad`), so the safe two-device set is the default and a third device is
    // opt-in + instantly revertible — the guard against the 2026-07-06 reconnect incident.
    let mut hids = vec![
        hid_device(
            HID_UID_TOUCHSCREEN,
            "CarLink Touchscreen",
            HID_PRODUCT_TOUCHSCREEN,
            if crate::levers::multi_touch() {
                touchscreen_multi_descriptor(cfg.display_width as u16, cfg.display_height as u16)
            } else {
                touchscreen_descriptor(cfg.display_width as u16, cfg.display_height as u16)
            },
        ),
        hid_device(
            HID_UID_MEDIA_BUTTONS,
            "CarLink Media Buttons",
            HID_PRODUCT_MEDIA_BUTTONS,
            media_buttons_descriptor(),
        ),
    ];
    if crate::levers::dpad() {
        hids.push(hid_device(
            HID_UID_DPAD,
            "CarLink D-Pad",
            HID_PRODUCT_DPAD,
            dpad_descriptor(),
        ));
    }
    // The knob (uid 4) is the Simulator's real navigation device. Opt-in + revertible like the D-Pad;
    // the display-features `0x02 Knobs` bit is ALREADY claimed above, so this makes it truthful.
    if crate::levers::knob() {
        hids.push(hid_device(
            HID_UID_KNOB,
            "CarLink Knob",
            HID_PRODUCT_KNOB,
            knob_descriptor(),
        ));
    }
    // Telephony (uid 5) — Answer/End/Flash/Mute + DTMF keypad (Apple HIDTelephony). Opt-in + revertible.
    if crate::levers::telephony() {
        hids.push(hid_device(
            HID_UID_TELEPHONY,
            "CarLink Telephony",
            HID_PRODUCT_TELEPHONY,
            telephony_descriptor(),
        ));
    }
    d.insert("hidDevices".into(), Value::Array(hids));

    // `limitedUIElements` — which UI elements iOS restricts when limited-UI mode is on. Emitted ONLY
    // when the host YAML's `limitedUIConfig` selects at least one, matching Apple: in
    // `AirPlayCopyServerInfo` every one of these optional keys is guarded by `if( obj )`, and the
    // Simulator's own `LimitedUIConfig.airPlayElements` returns an empty array when nothing is
    // selected. Omitting it leaves iOS on its default restriction set — the behaviour every build
    // before 2026-07-30 had, so a YAML that doesn't ask for this produces a byte-identical `/info`.
    //
    // This does NOT turn limited UI on. That is the runtime `/command setLimitedUI {limitedUI:bool}`
    // (`events::send_set_limited_ui`, driven from the host Controls window over OCBM 0x08/0x09),
    // which needs no reconnect and works with or without this key.
    if !cfg.limited_ui_elements.is_empty() {
        d.insert(
            "limitedUIElements".into(),
            Value::Array(
                cfg.limited_ui_elements.iter().map(|s| Value::String(s.clone())).collect(),
            ),
        );
    }

    // `limitedUI` — the INITIAL limited-UI state, and (hypothesis) the declaration that makes iOS
    // honour the runtime `setLimitedUI` command at all.
    //
    // R14G17 `AirPlayReceiverServer.c:473-480` emits this from `AirPlayCopyServerInfo` right after
    // `limitedUIElements`, and the current SDK still requests it during the InfoRequest phase. We
    // emitted NEITHER key, which is the only material difference found between us and Apple's
    // Simulator on this feature: the `/command setLimitedUI {limitedUI:bool}` we send is byte-identical
    // to `AirPlayReceiverSessionSetLimitedUI` (`AirPlayReceiverSession.c:5311-5316`), it reaches the
    // encrypted event channel (`sent=true` verified on hardware 2026-07-30), and iOS does nothing.
    //
    // Ruled OUT as the cause: a missing `limitedUIElements`. No stock Apple template and no user
    // Simulator config sets `limitedUIConfig`, so the Simulator's own `airPlayElements` is empty too —
    // yet its toggle takes effect immediately. So the element list is not the gate.
    //
    // Emitted unconditionally as `false` (CarPlay starts unrestricted; the runtime command moves it),
    // matching Apple's own emission whenever the platform supplies a value. This is a WIRE CHANGE on a
    // proven path and is the candidate fix, NOT a confirmed one — validate on a session, and revert
    // this single insert if anything regresses.
    d.insert("limitedUI".into(), Value::Boolean(false));

    // `buttonInfo` — an EMPTY BUT PRESENT array, which is literally what Apple's reference accessory
    // sends. ADDED 2026-07-30.
    //
    // Recovered from the Simulator's `AirPlayProperty.serverValue` @0x10002f8dc: the `buttonInfo` arm
    // loads `__swiftEmptyArrayStorage` and returns `([] as [Any]).asCFArray` — **non-nil**. So
    // `AirPlayCopyServerInfo`'s `if(obj)` guard passes and the key IS emitted, carrying zero button
    // declarations. Corroborated by cp.log: `oemIcon` (:1340) and `sessionManagementInfo` (:1351) log
    // `Unknown Property` at error level, while `buttonInfo` (:1353) does not — it is a handled arm.
    //
    // Why this matters and why it is NOT over-claiming: the captured session proves an *empty*
    // `buttonInfo` does not block classic `requestSiri`. It does NOT prove that OMITTING the key is
    // equally safe — the Simulator never exercises that path, and any real gate lives on the iOS
    // sender, which is not observable from these binaries. Matching Apple's reference emission
    // key-for-key is the evidence-backed choice; treating the key as optional is a guess.
    //
    // The array is deliberately empty: it declares physical hardware buttons, and we ship none.
    d.insert("buttonInfo".into(), Value::Array(Vec::new()));
    d.insert("keepAliveLowPower".into(), Value::Boolean(true)); // always set by the C
    d.insert("keepAliveSendStatsAsBody".into(), Value::Boolean(true)); // always set by the C
    d.insert(
        "manufacturer".into(),
        Value::String(cfg.manufacturer.clone()),
    );
    d.insert("model".into(), Value::String(cfg.model.clone()));
    d.insert("name".into(), Value::String(cfg.name.clone()));
    // OEM icon (vehicle-maker logo on the CarPlay home screen). Emitted ONLY when the host YAML sets it
    // — mirrors Apple's guarded block (AirPlayReceiverServer.c:551-607); absent = these keys omitted so
    // `/info` stays byte-identical. `oemIcons` is an ARRAY of one dict per resolution — Apple's AppStub
    // emits 120/180/256 ("for each required size", AppStub.c:597-649); a single size renders the label but
    // NOT the image on-device (2026-08-02), so the host scales the source to all three sizes.
    if !cfg.oem_icons.is_empty() {
        let mut arr = Vec::with_capacity(cfg.oem_icons.len());
        for (png, w, h) in &cfg.oem_icons {
            let mut icon = Dictionary::new();
            icon.insert("imageData".into(), Value::Data(png.clone()));
            icon.insert("widthPixels".into(), Value::Integer((*w).into()));
            icon.insert("heightPixels".into(), Value::Integer((*h).into()));
            icon.insert("prerendered".into(), Value::Boolean(true));
            arr.push(Value::Dictionary(icon));
        }
        d.insert("oemIcons".into(), Value::Array(arr));
        d.insert("oemIconVisible".into(), Value::Boolean(cfg.oem_icon_visible));
        if !cfg.oem_icon_label.is_empty() {
            d.insert("oemIconLabel".into(), Value::String(cfg.oem_icon_label.clone()));
        }
    }
    d.insert("pi".into(), Value::String(cfg.pairing_identity.clone()));
    d.insert(
        "sourceVersion".into(),
        Value::String(cfg.source_version.clone()),
    );
    if cfg.status_flags != 0 {
        d.insert(
            "statusFlags".into(),
            Value::Integer((cfg.status_flags as i64).into()),
        );
    }
    // protocolVersion is omitted (the C omits it when == "1.0").

    let mut buf = Vec::new();
    Value::Dictionary(d)
        .to_writer_binary(&mut buf)
        .expect("binary plist serialize");
    buf
}

/// One advertised `audioFormats` entry — a single CarPlay audio capability the box offers for iOS to
/// select. The full matrix (codec × rate × channels × stream type × audioType) is expressible here, so a
/// YAML `audio.formats` list can describe ANY head-unit audio configuration for testing, and the set the
/// box advertises is exactly what the YAML says. `audio_type: None` = no `audioType` key (the wired PCM
/// catch-all style that lets iOS map `audioType:"media"` onto a type-100 PCM entry). `input_formats == 0`
/// omits the `audioInputFormats` key (output-only, no mic capture on that stream).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFormatSpec {
    pub stream_type: i64,
    pub audio_type: Option<String>,
    pub input_formats: i64,
    pub output_formats: i64,
}

// --- CarPlay `audioFormat` bitmask constants (1<<index). The full codec/rate/channel matrix the box +
// app support. Names here are the YAML `in:`/`out:` tokens (see `audio_format_bit`). ---
const AF_PCM_16K_MONO: i64 = 0x10; // 1<<4  — S16LE 16 kHz mono
const AF_PCM_48K_STEREO: i64 = 0x8000; // 1<<15 — S16LE 48 kHz stereo
const AF_AAC_LC_44K_STEREO: i64 = 1 << 22;
const AF_AAC_LC_48K_STEREO: i64 = 1 << 23;
const AF_AAC_ELD_44K_STEREO: i64 = 1 << 24;
const AF_AAC_ELD_48K_STEREO: i64 = 1 << 25;
const AF_AAC_ELD_16K_MONO: i64 = 1 << 26;
const AF_AAC_ELD_24K_MONO: i64 = 1 << 27;
const AF_OPUS_16K_MONO: i64 = 1 << 28;
const AF_OPUS_24K_MONO: i64 = 1 << 29;
const AF_OPUS_48K_MONO: i64 = 1 << 30;
const AF_AAC_ELD_44K_MONO: i64 = 1 << 31;
const AF_AAC_ELD_48K_MONO: i64 = 1 << 32;
const AF_AAC_ELD_32K_MONO: i64 = 1 << 43;

/// Map a YAML audio-format NAME (the `in:`/`out:` tokens) to its `audioFormat` bitmask, or `None` if the
/// name is unrecognized. `0` / `"none"` is a valid "omit" for `in:`. This table is the box's DOCUMENTED
/// audio capability surface — every codec/rate/channel it can advertise. Multiple names may be OR'd in
/// YAML with `|` (see [`crate::vehicle_config`]).
pub fn audio_format_bit(name: &str) -> Option<i64> {
    Some(match name.trim() {
        "none" | "0" => 0,
        "pcm_16k_mono" => AF_PCM_16K_MONO,
        "pcm_48k_stereo" => AF_PCM_48K_STEREO,
        "aac_lc_44k_stereo" => AF_AAC_LC_44K_STEREO,
        "aac_lc_48k_stereo" => AF_AAC_LC_48K_STEREO,
        "aac_eld_44k_stereo" => AF_AAC_ELD_44K_STEREO,
        "aac_eld_48k_stereo" => AF_AAC_ELD_48K_STEREO,
        "aac_eld_16k_mono" => AF_AAC_ELD_16K_MONO,
        "aac_eld_24k_mono" => AF_AAC_ELD_24K_MONO,
        "aac_eld_32k_mono" => AF_AAC_ELD_32K_MONO,
        "aac_eld_44k_mono" => AF_AAC_ELD_44K_MONO,
        "aac_eld_48k_mono" => AF_AAC_ELD_48K_MONO,
        "opus_16k_mono" => AF_OPUS_16K_MONO,
        "opus_24k_mono" => AF_OPUS_24K_MONO,
        "opus_48k_mono" => AF_OPUS_48K_MONO,
        _ => return None,
    })
}

/// The known named presets a YAML `audio.preset:` can request. `None` if the name is unknown.
pub fn audio_preset(name: &str) -> Option<Vec<AudioFormatSpec>> {
    Some(match name.trim() {
        "wired_pcm" => preset_wired_pcm(),
        "wireless_8" | "wireless_full" => preset_wireless_8(),
        _ => return None,
    })
}

/// WIRED default: PCM only, types 100/101, NO `audioType` (the catch-all that lets iOS map
/// `audioType:"media"` onto a type-100 PCM entry over the USB link). Byte-exact to the stock CCPA's
/// `WiredAudioFormats` (live-captured working wired session). Advertising AAC over wired makes iOS find
/// no usable PCM media format and borrow no MainAudio → no audio, so this is the wired default.
pub fn preset_wired_pcm() -> Vec<AudioFormatSpec> {
    vec![
        AudioFormatSpec {
            stream_type: 100,
            audio_type: None,
            input_formats: AF_PCM_16K_MONO,
            output_formats: AF_PCM_48K_STEREO | AF_PCM_16K_MONO,
        },
        AudioFormatSpec {
            stream_type: 101,
            audio_type: None,
            input_formats: AF_PCM_16K_MONO,
            output_formats: AF_PCM_48K_STEREO,
        },
    ]
}

/// WIRELESS default: the 8-entry set the working carplayd-rs reference advertises
/// (`_BuildAudioFormatsArray`). Media rides **type-102 `media` AAC-LC 48k stereo** (Apple Music etc.);
/// voice/Siri/mic rides **type-100 AAC-ELD 16k mono**; alerts AAC-ELD 48k stereo. Device-verified:
/// media plays through the box + Siri mic captures.
pub fn preset_wireless_8() -> Vec<AudioFormatSpec> {
    let e = |t: i64, at: &str, i: i64, o: i64| AudioFormatSpec {
        stream_type: t,
        audio_type: Some(at.into()),
        input_formats: i,
        output_formats: o,
    };
    vec![
        e(
            100,
            "compatibility",
            AF_PCM_16K_MONO,
            AF_PCM_48K_STEREO | AF_PCM_16K_MONO,
        ),
        e(101, "compatibility", 0, AF_PCM_48K_STEREO),
        e(100, "alert", 0, AF_AAC_ELD_48K_STEREO),
        e(100, "default", AF_AAC_ELD_16K_MONO, AF_AAC_ELD_16K_MONO),
        e(100, "telephony", AF_AAC_ELD_16K_MONO, AF_AAC_ELD_16K_MONO),
        e(
            100,
            "speechRecognition",
            AF_AAC_ELD_16K_MONO,
            AF_AAC_ELD_16K_MONO,
        ),
        e(101, "default", 0, AF_AAC_ELD_48K_STEREO),
        e(102, "media", 0, AF_AAC_LC_48K_STEREO), // ← Apple Music / media, high-latency AAC-LC
    ]
}

/// The DEFAULT audio set when a YAML doesn't specify `audio:` — resolved by transport so the proven
/// behavior is preserved with no YAML: wireless (the `CARPLAY_WIRELESS_AUDIO` env av.rs sets) → the
/// 8-entry AAC set; wired → PCM only. A YAML `audio.preset`/`audio.formats` OVERRIDES this (see
/// [`crate::vehicle_config::VehicleConfig::apply`]).
pub fn default_audio_formats() -> Vec<AudioFormatSpec> {
    if std::env::var("CARPLAY_WIRELESS_AUDIO").is_ok() {
        preset_wireless_8()
    } else {
        preset_wired_pcm()
    }
}

/// Build the `/info` `audioFormats` array from the resolved capability set on `cfg` (YAML-driven; see
/// [`AudioFormatSpec`]). `audioType` omitted when `None`; `audioInputFormats` omitted when `0`.
fn audio_formats(cfg: &DeviceConfig) -> Vec<Value> {
    cfg.audio_formats
        .iter()
        .map(|s| {
            let mut e = Dictionary::new();
            e.insert("type".into(), Value::Integer(s.stream_type.into()));
            if let Some(at) = &s.audio_type {
                e.insert("audioType".into(), Value::String(at.clone()));
            }
            if s.input_formats != 0 {
                e.insert(
                    "audioInputFormats".into(),
                    Value::Integer(s.input_formats.into()),
                );
            }
            e.insert(
                "audioOutputFormats".into(),
                Value::Integer(s.output_formats.into()),
            );
            Value::Dictionary(e)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn info_plist_has_full_capability_set() {
        let cfg = DeviceConfig::default();
        let v = Value::from_reader(Cursor::new(build_info(&cfg))).unwrap();
        let d = v.as_dictionary().unwrap();
        // the keys the iPhone validates at activation
        for k in [
            "audioFormats",
            "audioLatencies",
            "deviceID",
            "displays",
            "features",
            "hidDevices",
            "keepAliveLowPower",
            "keepAliveSendStatsAsBody",
            "model",
            "name",
            "pi",
            "sourceVersion",
            "statusFlags",
        ] {
            assert!(d.contains_key(k), "missing /info key: {k}");
        }
        assert_eq!(d["name"].as_string().unwrap(), "CarPlay");
        assert_eq!(d["audioFormats"].as_array().unwrap().len(), 2); // wired build emits 2 (PCM type 100/101)
                                                                    // display.uuid must equal the hidDevices' displayUUID (touch binding)
        let disp = &d["displays"].as_array().unwrap()[0];
        let hids = d["hidDevices"].as_array().unwrap();
        assert_eq!(hids.len(), 2);
        assert_eq!(
            disp.as_dictionary().unwrap()["uuid"].as_string().unwrap(),
            DISPLAY_UUID
        );
        assert_eq!(
            hids[0].as_dictionary().unwrap()["displayUUID"]
                .as_string()
                .unwrap(),
            DISPLAY_UUID
        );
        // Full-panel viewAreas structure present (capture-locked shape): one entry, flat rect keys +
        // nested safeArea (CAPTURE_VALIDATION_04 §1.1).
        let disp_d = disp.as_dictionary().unwrap();
        let vareas = disp_d["viewAreas"].as_array().expect("viewAreas array");
        assert_eq!(vareas.len(), 1);
        let va0 = vareas[0].as_dictionary().unwrap();
        assert!(va0.contains_key("widthPixels") && va0.contains_key("safeArea"));
        assert!(va0["safeArea"]
            .as_dictionary()
            .unwrap()
            .contains_key("drawUIOutsideSafeArea"));
        // protocolVersion omitted (1.0)
        assert!(!d.contains_key("protocolVersion"));
    }

    #[test]
    fn multi_touchscreen_descriptor_matches_apple_template() {
        let d = touchscreen_multi_descriptor(2400, 960);
        // 6 B application collection + two 63 B finger collections + 1 B end.
        assert_eq!(d.len(), 133, "HIDTouchScreenMultiCreateDescriptor is 133 B");

        // Apple's four literal patch sites (HIDTouchScreen.c:232-238), asserted at their absolute
        // offsets rather than recomputed, so a layout slip cannot move them silently.
        assert_eq!((d[0x2F], d[0x30]), (2400u16 as u8, (2400u16 >> 8) as u8));
        assert_eq!((d[0x3C], d[0x3D]), (960u16 as u8, (960u16 >> 8) as u8));
        assert_eq!((d[0x6E], d[0x6F]), (2400u16 as u8, (2400u16 >> 8) as u8));
        assert_eq!((d[0x7B], d[0x7C]), (960u16 as u8, (960u16 >> 8) as u8));

        // Two Finger collections, each carrying a Transducer Index (0x09 0x38) — the field the
        // single-finger descriptor lacks and the whole reason a second contact is addressable.
        assert_eq!(d.windows(2).filter(|w| w == b"\x09\x22").count(), 2, "two Finger usages");
        assert_eq!(d.windows(2).filter(|w| w == b"\x09\x38").count(), 2, "two Transducer Index usages");

        // Both fingers are byte-identical apart from the patched geometry, so the second contact
        // reports in the same coordinate space as the first.
        assert_eq!(&d[6..69], &d[69..132]);
        assert_eq!(d[132], 0xC0);
    }

    #[test]
    fn touchscreen_descriptor_patches_geometry() {
        let d = touchscreen_descriptor(1920, 720);
        assert_eq!(d.len(), 62);
        assert_eq!(u16::from_le_bytes([d[39], d[40]]), 1920); // X logical max
        assert_eq!(u16::from_le_bytes([d[52], d[53]]), 720); // Y logical max
    }
}
