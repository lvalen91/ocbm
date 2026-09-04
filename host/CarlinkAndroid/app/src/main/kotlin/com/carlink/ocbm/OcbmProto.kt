package com.carlink.ocbm

/**
 * OCBM v1 wire constants — a byte-faithful port of `ccpa_custom/crates/ocbm-proto/src/lib.rs`.
 *
 * Everything in the OCBM envelope is LITTLE-endian. The one exception in the whole protocol is the
 * 16-bit length inside a CH_MFI payload, which is BIG-endian (see [Mfi]).
 */
object Ocbm {
    // ---- envelope ---------------------------------------------------------------------------

    /** `0x4F43424D` — on the wire, little-endian, this is the byte sequence 4D 42 43 4F. */
    const val MAGIC: Int = 0x4F43424D
    const val HDR_LEN: Int = 16
    const val VERSION: Byte = 1
    const val MAX_PAYLOAD: Int = 65536

    const val F_SOM: Byte = 0x01
    const val F_EOM: Byte = 0x02

    /** Replay of a frame the box already sent (dedupe hint; receivers may ignore it). */
    const val F_REPLAY: Byte = 0x04
    const val F_NEW_SOURCE: Byte = 0x08

    /** Every frame in the tree is sent SOM|EOM; the reassembler pops one frame per header. */
    const val F_BOTH: Byte = 0x03

    // ---- channels ---------------------------------------------------------------------------
    const val CH_CTRL: Int = 0x0000
    const val CH_MFI: Int = 0x0001
    const val CH_CONSOLE: Int = 0x0002
    const val CH_IP: Int = 0x0010
    const val CH_FILE: Int = 0x0011
    const val CH_ETH: Int = 0x0012
    const val CH_VIDEO: Int = 0x0020
    const val CH_MEDIA_AUDIO: Int = 0x0021
    const val CH_ALT_AUDIO: Int = 0x0022
    const val CH_METADATA: Int = 0x0023
    const val CH_ALT_VIDEO: Int = 0x0024
    const val CH_INPUT: Int = 0x0030
    const val CH_MIC: Int = 0x0031
    const val CH_MGMT: Int = 0x0040

    /** App-driven SETUP relay. Inert with `appDrivenSetup: false` — the box only relays when asked. */
    const val CH_RTSP: Int = 0x0041 // lib.rs:65
    /** Box→host universal-log stream (`/tmp/box.log` + per-daemon logs); armed by CT_LOG_CTL. */
    const val CH_LOG: Int = 0x0042
    const val CH_ECHO: Int = 0x00FF
    const val CH_DISCARD: Int = 0x0FFF

    fun channelName(ch: Int): String =
        when (ch) {
            CH_CTRL -> "CTRL"
            CH_MFI -> "MFI"
            CH_CONSOLE -> "CONSOLE"
            CH_IP -> "IP"
            CH_FILE -> "FILE"
            CH_ETH -> "ETH"
            CH_VIDEO -> "VIDEO"
            CH_MEDIA_AUDIO -> "MEDIA_AUDIO"
            CH_ALT_AUDIO -> "ALT_AUDIO"
            CH_METADATA -> "METADATA"
            CH_ALT_VIDEO -> "ALT_VIDEO"
            CH_INPUT -> "INPUT"
            CH_MIC -> "MIC"
            CH_MGMT -> "MGMT"
            CH_RTSP -> "RTSP"
            CH_LOG -> "LOG"
            CH_ECHO -> "ECHO"
            CH_DISCARD -> "DISCARD"
            else -> "0x%04x".format(ch)
        }

    // ---- CH_CTRL message types (first payload byte) -------------------------------------------
    const val CT_HELLO: Byte = 0x01
    const val CT_HELLO_ACK: Byte = 0x02
    const val CT_MODE_SELECT: Byte = 0x03
    const val CT_SRC: Byte = 0x04
    const val CT_SETTIME: Byte = 0x05
    const val CT_ETH_START: Byte = 0x06
    const val CT_ETH_STOP: Byte = 0x07
    const val CT_SUBSCRIBE: Byte = 0x10
    const val CT_STOP: Byte = 0x11
    const val CT_HEARTBEAT: Byte = 0x12
    const val CT_SESSION_EVENT: Byte = 0x13
    const val CT_UPLINK: Byte = 0x14
    const val CT_PAIRING_CODE: Byte = 0x15

    /**
     * host->box `[CT_PAIR_CONFIRM][accept u8: 1 = pair, 0 = cancel]` — the user's answer to the
     * CT_PAIRING_CODE prompt. SSP Numeric Comparison needs a real yes/no on BOTH devices, so the box
     * waits for this instead of auto-accepting, for up to 55 s. Any byte != 0 is a yes.
     */
    const val CT_PAIR_CONFIRM: Byte = 0x1C

    /** host->box `[CT_RADIO][0 = radios off now | 1 = radios on if the pushed cfg allows]`. lib.rs:117 */
    const val CT_RADIO: Byte = 0x16

    /**
     * box->host `[CT_BT_PHASE][BTP_*]` — Bluetooth/iAP2 handshake progress. lib.rs:102
     *
     * The host is not in the BT loop (the box owns the radio) and `SEV_PHONE_*` refers to the box's own
     * USB bus, so without this there is NO signal for the entire Bluetooth phase — the UI can only say
     * "waiting" from subscribe until video appears.
     *
     * Advisory and only roughly monotonic: treat an unknown value as progress and never gate on
     * ordering (lib.rs:107-109).
     */
    const val CT_BT_PHASE: Byte = 0x17

    /**
     * box->host `[CT_PHONE_IDENT][utf8 JSON]` — who the connected phone IS. lib.rs:113
     *
     * `{"name","deviceID","model","osName","osVersion"}`, lifted verbatim from the phone's own
     * AirPlay phase-1 SETUP plist. `name` is what the user typed in Settings > General > About >
     * Name; `deviceID` is the BR/EDR MAC, so it joins against MGMT_INFO's bonded list and is the
     * only thing on the wire that says WHICH bonded phone is live. Empty payload = cleared.
     */
    const val CT_PHONE_IDENT: Byte = 0x18

    // BT phase values — lib.rs:110-116.
    const val BTP_IDLE: Byte = 0x00
    const val BTP_LINK_UP: Byte = 0x01
    const val BTP_AUTHENTICATING: Byte = 0x02
    const val BTP_AUTHENTICATED: Byte = 0x03
    const val BTP_IDENTIFYING: Byte = 0x04
    const val BTP_IDENTIFIED: Byte = 0x05
    const val BTP_WIFI_HANDOFF: Byte = 0x06
    const val BTP_PAIR_REJECTED: Byte = 0x07

    fun btpName(b: Byte): String =
        when (b) {
            BTP_IDLE -> "IDLE"
            BTP_LINK_UP -> "LINK_UP"
            BTP_AUTHENTICATING -> "AUTHENTICATING"
            BTP_AUTHENTICATED -> "AUTHENTICATED"
            BTP_IDENTIFYING -> "IDENTIFYING"
            BTP_IDENTIFIED -> "IDENTIFIED"
            BTP_WIFI_HANDOFF -> "WIFI_HANDOFF"
            BTP_PAIR_REJECTED -> "PAIR_REJECTED"
            else -> "BTP_0x%02x".format(b)
        }

    const val MODE_PROJECTION: Byte = 0x00
    const val MODE_CONSOLE: Byte = 0x01

    // ---- session events (payload of CT_SESSION_EVENT) -----------------------------------------
    const val SEV_HOST_PRESENT: Byte = 0x01
    const val SEV_HOST_GONE: Byte = 0x02
    const val SEV_PHONE_PRESENT: Byte = 0x03
    const val SEV_PHONE_ABSENT: Byte = 0x04

    fun sevName(s: Byte): String =
        when (s) {
            SEV_HOST_PRESENT -> "HOST_PRESENT"
            SEV_HOST_GONE -> "HOST_GONE"
            // NB: PHONE_* refer to an iPhone on the BOX's own USB bus (wired path). They say nothing
            // about Bluetooth — that is CT_BT_PHASE's job.
            SEV_PHONE_PRESENT -> "PHONE_PRESENT (box USB bus)"
            SEV_PHONE_ABSENT -> "PHONE_ABSENT (box USB bus)"
            else -> "SEV_0x%02x".format(s)
        }


    // ---- CT_PROJ_MODE (0x19) — which projection mode the box has selected -----------------------
    // Box->host, additive per the OCBM extensibility rules. Payload `[CT_PROJ_MODE][PM_*]`, emitted
    // on CHANGE only and re-armed on every fresh CT_SUBSCRIBE, so a re-attaching host learns the
    // current mode without waiting. Mirrors `box_common::flags::owner().wire_code()`.
    const val CT_PROJ_MODE: Byte = 0x19

    const val PM_NONE: Byte = 0x00 // idle — no projection session; either phone kind may claim the box
    const val PM_WIRED_CP: Byte = 0x01 // wired CarPlay (projection_up -> iap2d + airplayd)
    const val PM_WIRELESS_CP: Byte = 0x02 // wireless CarPlay (carplay-wireless: BT + WiFi AP)
    const val PM_WIRED_AA: Byte = 0x03 // wired Android Auto (aa-bridge AOAP pump; app drives AA over CH_IP)
    const val PM_WIRELESS_AA: Byte = 0x04 // reserved — wireless Android Auto (unbuilt)

    fun pmName(m: Byte): String =
        when (m) {
            PM_NONE -> "NONE"
            PM_WIRED_CP -> "WIRED_CP"
            PM_WIRELESS_CP -> "WIRELESS_CP"
            PM_WIRED_AA -> "WIRED_AA"
            PM_WIRELESS_AA -> "WIRELESS_AA"
            else -> "PM_0x%02x".format(m)
        }

    // ---- CT_BOX_HEALTH (0x1A) — one bitmask of box-side subsystem liveness ----------------------
    // Box->host telemetry. Read bit 0 (BH_HCI_PRESENT) FIRST when Bluetooth looks dead: a value of
    // 0x50 with bit 0 clear is the signature of a missing radio HAL script — no hci0 was ever
    // created, while OCBM, MFi and CT_SUBSCRIBE all still report success.
    const val CT_BOX_HEALTH: Byte = 0x1A

    const val BH_HCI_PRESENT: Int = 0x01
    const val BH_SSP: Int = 0x02
    const val BH_IAP2D: Int = 0x04
    const val BH_AIRPLAYD: Int = 0x08
    const val BH_CARPLAY_WIRELESS: Int = 0x10
    const val BH_WLAN_AP: Int = 0x20
    const val BH_ROOTFS_OK: Int = 0x40

    // ---- CT_LOG_CTL (0x1B) — host→box `[CT_LOG_CTL][enabled u8][cap_kb u16 LE]` ----------------
    // Arms the CH_LOG stream: the box tails its universal log (source 0, truncated at cap_kb once
    // streamed) and the supervisor's per-daemon logs (tail-only). Entry layout on CH_LOG:
    // [source u8][flags u8][seq u16 LE][unix_ms u64 LE][len u16 LE][text]. Not consumed by this app
    // yet; defined so tools/proto_check.py keeps the three implementations in step.
    const val CT_LOG_CTL: Byte = 0x1B
    const val LOG_CAP_DEFAULT_KB: Int = 0x100
    const val LOG_ENTRY_HDR: Int = 0x0E
    const val LOG_MAX_LINE: Int = 0x400
    const val LOG_MAX_FRAME: Int = 0x1000
    const val LOG_F_DROPPED: Int = 0x01
    const val LOG_F_TRUNCATED: Int = 0x02
    const val LOG_F_BACKFILL: Int = 0x04
    const val LOG_SRC_BOX: Int = 0x00
    const val LOG_SRC_AIRPLAYD: Int = 0x01
    const val LOG_SRC_AIRPLAYD_WL: Int = 0x02
    const val LOG_SRC_IAP2D: Int = 0x03
    const val LOG_SRC_AA_BRIDGE: Int = 0x04
    const val LOG_SRC_RX_CONNECT: Int = 0x05
    const val LOG_SRC_BT: Int = 0x06
    const val LOG_SRC_RADIO_AP_DHCP: Int = 0x07
    const val LOG_SRC_RADIO_BT_ATTACH: Int = 0x08
    const val LOG_SRC_RX_CONNECT_WL: Int = 0x09
    const val LOG_SRC_CARPLAY_WIRELESS: Int = 0x0A
    const val LOG_SRC_INTERNAL: Int = 0xFF

    /** Human-readable bit list for a CT_BOX_HEALTH payload — the log line and the UI detail. */
    fun bhString(f: Int): String {
        val on = buildList {
            if (f and BH_HCI_PRESENT != 0) add("HCI")
            if (f and BH_SSP != 0) add("SSP")
            if (f and BH_IAP2D != 0) add("iap2d")
            if (f and BH_AIRPLAYD != 0) add("airplayd")
            if (f and BH_CARPLAY_WIRELESS != 0) add("carplay-wireless")
            if (f and BH_WLAN_AP != 0) add("hostapd")
            if (f and BH_ROOTFS_OK != 0) add("rootfs-ok")
        }
        return if (on.isEmpty()) "none" else on.joinToString("|")
    }

    fun fileStatusName(s: Byte): String =
        when (s) {
            FILE_OK -> "OK"
            FILE_ERR_OPEN -> "OPEN_FAILED"
            FILE_ERR_VERIFY -> "VERIFY_FAILED"
            FILE_ERR_NOFILE -> "NO_SUCH_FILE"
            FILE_ERR_WRITE -> "IO_FAILED"
            else -> "0x%02x".format(s)
        }

    // NOTE: which health bits a given deployment REQUIRES is app policy, not protocol, and stays in
    // the app. gm_ccpa's bridge role, for one, deliberately does not require BH_WLAN_AP because the
    // head unit owns Wi-Fi there — a rule that would be wrong for this app.

    // ---- CH_IP sub-opcodes (stream-mux relay) --------------------------------------------------
    const val IP_OPEN: Byte = 0x01 // data = target "host:port"; the box connect()s and relays
    const val IP_DATA: Byte = 0x02 // data = stream bytes
    const val IP_CLOSE: Byte = 0x03 // no data
    const val IP_OPEN_UDP: Byte = 0x04 // same as IP_OPEN, UDP instead of TCP

    // ---- CH_FILE sub-opcodes -------------------------------------------------------------------
    const val FILE_OPEN: Byte = 0x01
    const val FILE_DATA: Byte = 0x02
    const val FILE_CLOSE: Byte = 0x03
    const val FILE_ACK: Byte = 0x04
    const val FILE_PULL: Byte = 0x05

    const val FILE_OK: Byte = 0
    const val FILE_ERR_OPEN: Byte = 1
    const val FILE_ERR_VERIFY: Byte = 2
    const val FILE_ERR_NOFILE: Byte = 3
    const val FILE_ERR_WRITE: Byte = 4

    // ---- capability bits, as reported in CT_HELLO_ACK ------------------------------------------
    const val CAP_CONSOLE: Int = 0x0000_0001
    const val CAP_ECHO: Int = 0x0000_0002
    const val CAP_MFI: Int = 0x0000_0004
    const val CAP_IP: Int = 0x0000_0008
    const val CAP_FILE: Int = 0x0000_0010
    const val CAP_ETH: Int = 0x0000_0020

    fun capsString(caps: Int): String {
        val bits = ArrayList<String>()
        if (caps and CAP_CONSOLE != 0) bits.add("CONSOLE")
        if (caps and CAP_ECHO != 0) bits.add("ECHO")
        if (caps and CAP_MFI != 0) bits.add("MFI")
        if (caps and CAP_IP != 0) bits.add("IP")
        if (caps and CAP_FILE != 0) bits.add("FILE")
        if (caps and CAP_ETH != 0) bits.add("ETH")
        return if (bits.isEmpty()) "none" else bits.joinToString("|")
    }

    // ---- CH_MGMT verbs -------------------------------------------------------------------------
    const val MGMT_GET_INFO: Byte = 0x01
    const val MGMT_REBOOT: Byte = 0x02
    const val MGMT_FORGET_ALL: Byte = 0x03
    const val MGMT_FORGET_DEVICE: Byte = 0x04
    const val MGMT_RESTART_WIRELESS: Byte = 0x05
    const val MGMT_ENTER_NCM: Byte = 0x06
    const val MGMT_INFO: Byte = 0x81.toByte()
    const val MGMT_ACK: Byte = 0x82.toByte()

    // ---- CH_INPUT sub-frames (host->box) -------------------------------------------------------
    // Payload is [type u8][...]. lib.rs:183-206.

    /**
     * `[INPUT_TOUCH][phase u8][nx u16 LE][ny u16 LE][finger u8]`. lib.rs:183
     *
     * Coordinates are NORMALIZED 0..=65535 across the display. airplayd scales them to absolute HID
     * coordinates using the SAME resolution it advertised in `/info`, so the box stays the single
     * resolution authority and the host never needs to know the panel size.
     */
    const val INPUT_TOUCH: Byte = 0x01

    /** Ask the box to request an iOS keyframe (video-loss recovery, NOT user input). lib.rs:184 */
    const val INPUT_KEYFRAME: Byte = 0x02

    /** `[INPUT_MEDIA_BTN][index u8]` -> box taps the uid-2 media HID (press+release). lib.rs:197 */
    const val INPUT_MEDIA_BTN: Byte = 0x03

    /** `[INPUT_COMMAND][cmd u8][...]` -> box dispatches the mapped `/command`. lib.rs:198 */
    const val INPUT_COMMAND: Byte = 0x04

    /** `[INPUT_NAV][nav u8]` -> box taps the uid-3 D-Pad. lib.rs:199 */
    const val INPUT_NAV: Byte = 0x05

    /** Force a keyframe on the ALT/cluster stream specifically; a bare INPUT_KEYFRAME only re-IDRs
     *  the main console. lib.rs:185 */
    const val INPUT_KEYFRAME_ALT: Byte = 0x06

    /** `[INPUT_KNOB][flags u8][nudgeX i8][nudgeY i8][rotation i8]` -> uid-4 knob. lib.rs:203 */
    const val INPUT_KNOB: Byte = 0x07

    /** `[INPUT_TELEPHONY][buttonIndex u8]` -> uid-5 telephony HID (report then release). lib.rs:200 */
    const val INPUT_TELEPHONY: Byte = 0x08

    // Touch phases — lib.rs:295-297. NOTE these differ from the legacy riddleBox MultiTouchAction
    // values (UP=0/DOWN=1/MOVE=2); the two enumerations collide numerically but mean different things.
    const val TOUCH_DOWN: Byte = 0x00
    const val TOUCH_MOVE: Byte = 0x01
    const val TOUCH_UP: Byte = 0x02

    // INPUT_MEDIA_BTN indices — the Consumer-array index into the uid-2 descriptor. lib.rs:211.
    // MEDIA-TRANSPORT ONLY; Home/Back/nav are the uid-3 D-Pad. 0 = release (the box completes the pair).
    const val MEDIA_BTN_RELEASE: Byte = 0
    const val MEDIA_BTN_PLAY: Byte = 1
    const val MEDIA_BTN_PAUSE: Byte = 2
    const val MEDIA_BTN_PLAY_PAUSE: Byte = 3
    const val MEDIA_BTN_NEXT: Byte = 4
    const val MEDIA_BTN_PREV: Byte = 5

    // INPUT_NAV actions (uid-3 D-Pad) — lib.rs:216-222.
    const val NAV_UP: Byte = 1
    const val NAV_DOWN: Byte = 2
    const val NAV_LEFT: Byte = 3
    const val NAV_RIGHT: Byte = 4
    const val NAV_SELECT: Byte = 5
    const val NAV_HOME: Byte = 6
    const val NAV_BACK: Byte = 7

    // INPUT_TELEPHONY indices (uid-5) — lib.rs:201-202. DTMF digit d -> TEL_DIGIT0 + d.
    const val TEL_ANSWER: Byte = 1
    const val TEL_FLASH: Byte = 2
    const val TEL_END: Byte = 3
    const val TEL_MUTE: Byte = 4
    const val TEL_DIGIT0: Byte = 5
    const val TEL_STAR: Byte = 15
    const val TEL_POUND: Byte = 16
    const val TEL_DELETE: Byte = 17

    // ---- INPUT_COMMAND values ------------------------------------------------------------------
    const val CMD_REQUEST_UI: Byte = 0x01 // lib.rs:224

    @Deprecated("iOS ignores the bare requestSiri (validated 2026-07-11) — use CMD_SIRI_DOWN/UP")
    const val CMD_REQUEST_SIRI: Byte = 0x02 // lib.rs:227

    const val CMD_SIRI_DOWN: Byte = 0x03 // lib.rs:231
    const val CMD_SIRI_UP: Byte = 0x04 // lib.rs:232
    const val CMD_NAV_START: Byte = 0x05 // lib.rs:238
    const val CMD_NAV_STOP: Byte = 0x06 // lib.rs:239
    const val CMD_NAV_CARD: Byte = 0x07 // lib.rs:243
    const val CMD_LIMITED_UI_ON: Byte = 0x08 // lib.rs:247
    const val CMD_LIMITED_UI_OFF: Byte = 0x09 // lib.rs:248
    const val CMD_NAV_APP: Byte = 0x0A // lib.rs:249
    const val CMD_NAV_APPEARANCE: Byte = 0x0B // lib.rs:255 — `[cmd][flags]`

    const val NAV_APPEARANCE_SPEED_LIMIT: Int = 0x01 // lib.rs:256
    const val NAV_APPEARANCE_COMPASS: Int = 0x02 // lib.rs:257
    const val NAV_APPEARANCE_ETA: Int = 0x04 // lib.rs:258

    const val CMD_NAV_ZOOM_IN: Byte = 0x0C // lib.rs:261
    const val CMD_NAV_ZOOM_OUT: Byte = 0x0D // lib.rs:262

    /**
     * Display appearance. lib.rs:264-278. Wire forms differ per command:
     * ```
     *   [INPUT_COMMAND][CMD_UI_APPEARANCE ][stream][mode]  -> uiAppearanceUpdate
     *   [INPUT_COMMAND][CMD_MAP_APPEARANCE][stream][mode]  -> mapAppearanceUpdate
     *   [INPUT_COMMAND][CMD_NIGHT_MODE    ][on]            -> setNightMode
     * ```
     * `setNightMode` is GLOBAL and advisory — one input alongside iOS's own logic. The per-display
     * commands set a named display's appearance directly, but are silently dropped unless
     * `enablesUIAppearance`/`enablesMapAppearance` were advertised in the pushed config.
     */
    const val CMD_UI_APPEARANCE: Byte = 0x0E // lib.rs:272
    const val CMD_MAP_APPEARANCE: Byte = 0x0F // lib.rs:273
    const val CMD_NIGHT_MODE: Byte = 0x10 // lib.rs:274

    const val APPEARANCE_STREAM_MAIN: Byte = 0x00 // lib.rs:275
    const val APPEARANCE_STREAM_ALT: Byte = 0x01 // lib.rs:276
    const val APPEARANCE_MODE_LIGHT: Byte = 0x00 // lib.rs:277
    const val APPEARANCE_MODE_DARK: Byte = 0x01 // lib.rs:278

    // ---- Audio seam v2 markers (CH_MEDIA_AUDIO / CH_ALT_AUDIO) ----------------------------------
    // `[u32 BE len][SEAM_MAGIC "SEAV"][marker]…`; 0x00 SEAM_KEY, 0x01 SEAM_PKT, 0x02 SEAM_FORMAT.
    /** `[0x03][scid 8 LE][payload]` — UNENCRYPTED access unit (no RTP, no key): the Android Auto
     *  telephony lane, HFP/SCO S16LE PCM forwarded verbatim after a PCM SEAM_FORMAT. */
    const val SEAM_PKT_PLAIN: Byte = 0x03

    /** SEAM_FORMAT `codec` 4 — mSBC, the HFP wideband-speech codec (HFP 1.6 §5.7.4). The payload
     *  under it is NOT PCM: each SEAM_PKT_PLAIN carries one raw transparent-eSCO read (2-byte H2
     *  header + 57-byte mSBC frame + pad), and `rate`/`bits` describe the DECODED audio (16 kHz
     *  mono S16LE). A host without an mSBC decoder must drop the stream, not play the bytes.
     *  Decoded on macOS by carlink_macOS/Audio/MSBCCodec.swift; this client has no decoder yet. */
    const val SEAM_CODEC_MSBC: Byte = 4

    // ---- CH_METADATA seam markers --------------------------------------------------------------
    // Framing is [u32 BE "META"][u32 BE len][marker][payload] where len = 1 + payload.size.
    // Magic per iap2-core/src/metadata.rs:42; markers per lib.rs:36-39. Payloads are PLAINTEXT —
    // the box decrypts the control connection and the host is the trusted consumer over USB.
    val META_SEAM_MAGIC: ByteArray = byteArrayOf(0x4D, 0x45, 0x54, 0x41) // "META"

    /** Raw binary plist of an inbound iPhone `POST /command` ({type, params}) — from airplayd. */
    const val META_CMD: Int = 0x01

    /** One JSON object `{"kind": …}` of iAP2 metadata — from iap2d. */
    const val META_JSON: Int = 0x02

    /** `[artwork id u8][JPEG bytes]` — album art via the iAP2 File-Transfer session. */
    const val META_ARTWORK: Int = 0x03

    /** `[u32 BE display_width_px][PNG bytes]` — iOS's per-resolution corner mask. */
    const val META_CORNERMASK: Int = 0x04

    // ---- timing --------------------------------------------------------------------------------

    /** Host sends CT_HEARTBEAT at this cadence once HELLO_ACK has arrived. */
    const val HEARTBEAT_MS: Long = 1000

    /** Box-side watchdog. Miss this and the box emits SEV_HOST_GONE and clears `subscribed`. */
    const val HEARTBEAT_GRACE_MS: Long = 10_000

    /**
     * Minimum spacing between keyframe requests, matching the macOS host's 500 ms throttle.
     *
     * Load-bearing, not decoration: `VideoSeam.onGap` fires per seq gap AND per decrypt failure, so a
     * wrong key produces one call per frame — 60/s at 60 fps — and a single IDR repaints the whole
     * screen anyway.
     */
    const val KEYFRAME_MIN_INTERVAL_MS: Long = 500

    /**
     * CH_MIC chunk size. Deliberately not MAX_PAYLOAD: 16 KiB is 4-byte aligned so a chunk boundary can
     * never split a 16-bit stereo sample frame, and it matches the transport's WRITE_CHUNK so one mic
     * chunk is exactly one bulk transfer.
     */
    const val MIC_CHUNK: Int = 16384
}

/**
 * CH_MFI sub-framing (`crates/vendor/mfi/src/lib.rs`).
 *
 *   request : [op u8][plen u16 BE][payload]
 *   response: [status u8][rlen u16 BE][payload]
 *
 * The u16 lengths here are BIG-endian — the only big-endian field family in OCBM.
 */
object Mfi {
    const val OP_COPY_CERTIFICATE: Byte = 0x01
    const val OP_CREATE_SIGNATURE: Byte = 0x02

    const val STATUS_OK: Byte = 0x00

    /** Chip absent, I2C failure, lock timeout (>10s), poll timeout, or an implausible chip length. */
    const val STATUS_FAILED: Byte = 0x01

    /** Unknown opcode, or sign with ilen==0 / a truncated payload. */
    const val STATUS_UNSUPPORTED: Byte = 0x02

    /**
     * Optional 1-byte correlation tag appended AFTER the declared payload and echoed on the reply
     * (`ocbmd::handle_mfi`, canonical `MFI_TAG_LEN`). Without it two concurrent chip users can only
     * correlate by payload length, which cannot tell two 128-byte signatures apart — a late reply to
     * a timed-out sign then answers the wrong digest and the phone drops the session as a crypto
     * fault. An untagged peer degrades to the old length-correlation behaviour.
     */
    const val TAG_LEN: Int = 1

    const val HDR_LEN: Int = 3
    const val DIGEST_LEN: Int = 20 // SHA-1
    const val SIG_LEN: Int = 128 // RSA-1024

    /** Observed certificate length on real hardware. */
    const val EXPECTED_CERT_LEN: Int = 945

    /** `01 00 00` — a bare opcode with no length is silently dropped by the box. */
    fun certRequest(tag: Byte? = null): ByteArray =
        if (tag == null) {
            byteArrayOf(OP_COPY_CERTIFICATE, 0, 0)
        } else {
            byteArrayOf(OP_COPY_CERTIFICATE, 0, 0, tag)
        }

    /** `02 00 14 <20-byte SHA-1 digest>`. */
    fun signRequest(digest: ByteArray, tag: Byte? = null): ByteArray {
        require(digest.isNotEmpty() && digest.size <= 0xFFFF) { "bad digest length ${digest.size}" }
        val out = ByteArray(HDR_LEN + digest.size + if (tag == null) 0 else TAG_LEN)
        if (tag != null) out[HDR_LEN + digest.size] = tag
        out[0] = OP_CREATE_SIGNATURE
        out[1] = ((digest.size shr 8) and 0xFF).toByte()
        out[2] = (digest.size and 0xFF).toByte()
        System.arraycopy(digest, 0, out, HDR_LEN, digest.size)
        return out
    }

    /** [tag] is null when the peer echoed none — an older box, or an untagged request. */
    class Response(
        val status: Byte,
        val payload: ByteArray,
        val tag: Byte? = null,
    ) {
        val ok: Boolean get() = status == STATUS_OK

        fun statusName(): String =
            when (status) {
                STATUS_OK -> "OK"
                STATUS_FAILED -> "FAILED (no chip / i2c / lock timeout)"
                STATUS_UNSUPPORTED -> "UNSUPPORTED (bad opcode or malformed request)"
                else -> "0x%02x".format(status)
            }
    }

    fun parse(
        payload: ByteArray,
        len: Int,
    ): Response? {
        if (len < HDR_LEN) return null
        val rlen = ((payload[1].toInt() and 0xFF) shl 8) or (payload[2].toInt() and 0xFF)
        // Reject a torn/short frame rather than silently clamping: a short "certificate" would be
        // accepted here and fail much later in MFi-SAP with an opaque pairing error.
        if (rlen > len - HDR_LEN) return null
        val body = if (rlen > 0) payload.copyOfRange(HDR_LEN, HDR_LEN + rlen) else ByteArray(0)
        // Exactly one trailing byte past the declared payload is this response's correlation tag.
        // Anything else (none, or more) means an untagged peer; fall back to length correlation.
        val tag = if (len == HDR_LEN + rlen + TAG_LEN) payload[HDR_LEN + rlen] else null
        return Response(payload[0], body, tag)
    }
}
