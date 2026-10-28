package zeno.gmccpa.ocbm

/**
 * [SessionTrace.Board] entry names for the OCBM side, in one place so `OcbmProbe`,
 * `UsbBulkTransport` and this client cannot spell the same component two ways.
 */
internal object OcbmBoard {
    /** The claimed USB interface — written by `UsbBulkTransport.open/stop` and, for the waiting states, by `OcbmProbe.awaitClaimable`. */
    const val USB_CLAIM = "usb-claim"
    /** The `ocbm-read` thread. */
    const val USB_READ = "usb-read-loop"
    /** The OCBM link: HELLO_ACK, subscribed, HOST_PRESENT confirmed — or FAILED with why. */
    const val LINK = "ocbm-link"
    /** The 1 Hz heartbeat thread. */
    const val HEARTBEAT = "ocbm-heartbeat"
    /** The box's `CH_LOG` push stream: armed, flowing, disarmed. */
    const val CH_LOG = "box-log-stream"
    /** Host-side radio inhibit (`CT_RADIO`), as last commanded. */
    const val RADIOS = "box-radios"
    /** The box's last `CT_BOX_HEALTH`, decoded via [Ocbm.bhString]; FAILED when hci0 is missing. */
    const val HEALTH = "box-health"
    /** The BT/iAP2 handshake phase (`CT_BT_PHASE`). */
    const val BT_PHASE = "bt-phase"
    /** The identified phone (`CT_PHONE_IDENT`). */
    const val PHONE = "phone"
    /** The SSP numeric-comparison code on display (`CT_PAIRING_CODE`). */
    const val PAIRING_CODE = "pairing-code"
    /** Which projection transport owns the box (`CT_PROJ_MODE`). */
    const val PROJ_MODE = "proj-mode"
}

/**
 * [SessionTrace] expectation names for the bring-up ladder. Each is armed exactly where the wait
 * begins and met exactly where the answer is decoded; see the class KDoc of [OcbmClient] for the
 * budget behind each.
 */
internal object OcbmExpect {
    /** Armed by `UsbAttachActivity` when it forwards an attach; met at the top of `OcbmProbe.runAllLocked`. */
    const val LINK_ATTEMPT = "link-attempt-after-attach"
    /** Armed by `OcbmProbe.awaitClaimable` when it raises the permission dialog; met when the grant appears. */
    const val USB_PERMISSION = "usb-permission"
    const val HELLO_ACK = "ocbm-hello-ack"
    const val SETTIME_ACK = "ocbm-settime-ack"
    /** Bring-up MFi steps, armed by `OcbmProbe.runAllLocked` with the client's own budgets. */
    const val MFI_CERT = "mfi-cert"
    const val MFI_SIGN = "mfi-sign"
    const val MFI_SIGN_RETRY = "mfi-sign-retry"
    const val HOST_PRESENT = "sev-host-present"
    const val HCI_PRESENT = "box-health-hci"
    const val LOG_FIRST_LINE = "box-log-first-entry"

    /** The names [OcbmClient] arms itself, cancelled together on [OcbmClient.stop]. */
    val CLIENT_OWNED = listOf(HELLO_ACK, SETTIME_ACK, HOST_PRESENT, HCI_PRESENT, LOG_FIRST_LINE)
    /** The names `OcbmProbe` arms, cancelled together when it stops the link. */
    val PROBE_OWNED = listOf(USB_PERMISSION, MFI_CERT, MFI_SIGN, MFI_SIGN_RETRY)
}

/**
 * Name a `CH_LOG` source id. Mirrors `ocbm-proto`'s `log_source_name` exactly.
 *
 * Lives here rather than in `OcbmProto.kt` because that file restates the wire protocol and nothing
 * else (an app-owned fork of ccpa_custom's since 2026-09-11, kept in step by `tools/proto_check.py`;
 * until then it was consumed through a symlink) — this repo restates the protocol, it does not
 * extend it. An unknown id renders
 * rather than drops: the source table grows box-side, and a host that discards lines it cannot label
 * loses exactly the evidence about the thing that is new.
 */
internal fun logSourceName(id: Int): String = when (id) {
    Ocbm.LOG_SRC_BOX -> "box"
    Ocbm.LOG_SRC_AIRPLAYD -> "airplayd"
    Ocbm.LOG_SRC_AIRPLAYD_WL -> "airplayd-wl"
    Ocbm.LOG_SRC_IAP2D -> "iap2d"
    Ocbm.LOG_SRC_AA_BRIDGE -> "aa-bridge"
    Ocbm.LOG_SRC_RX_CONNECT -> "rx-connect"
    Ocbm.LOG_SRC_BT -> "bt"
    Ocbm.LOG_SRC_RADIO_AP_DHCP -> "radio_ap_dhcp"
    Ocbm.LOG_SRC_RADIO_BT_ATTACH -> "radio_bt_attach"
    Ocbm.LOG_SRC_RX_CONNECT_WL -> "rx-connect_wl"
    Ocbm.LOG_SRC_CARPLAY_WIRELESS -> "wl"
    Ocbm.LOG_SRC_INTERNAL -> "internal"
    else -> "src$id"
}

/**
 * The phone's user-given device name (`CT_PHONE_IDENT`'s `"name"`, what the user typed under
 * Settings > General > About > Name, usually their own name) is redacted before any log line, because
 * the export-time redactor does not recognise it (audit12 F119). `deviceID`, `model` and the OS stay:
 * they are what joins a session to a bond, and the MAC is the export redactor's own job.
 */
object PhoneIdent {
    /** `"name": "<value>"` with the JSON escapes honoured. Possessive, so a long value cannot recurse. */
    private val NAME_VALUE = Regex("(\"name\"\\s*:\\s*)\"((?:[^\"\\\\]++|\\\\.)*+)\"")
    /** A `"name"` string value that never closes (a truncated payload): everything after it goes. */
    private val NAME_UNCLOSED = Regex("(\"name\"\\s*:\\s*)\"(?!<name \\d+ chars>\")[\\s\\S]*$")

    /** [json] with every `"name"` string value replaced by `<name N chars>` (N = decoded code points). */
    fun redactName(json: String): String {
        val closed = NAME_VALUE.replace(json) { m ->
            m.groupValues[1] + "\"" + placeholder(decode(m.groupValues[2])) + "\""
        }
        return NAME_UNCLOSED.replace(closed) { m -> m.groupValues[1] + "\"<name truncated>" }
    }

    /** The placeholder for an already-decoded name; used by the Board line, which decodes itself. */
    fun placeholder(name: String): String = "<name ${name.codePointCount(0, name.length)} chars>"

    /** JSON string-body unescape, enough to count characters: `\uXXXX` and the one-letter escapes. */
    private fun decode(raw: String): String {
        val sb = StringBuilder(raw.length)
        var i = 0
        while (i < raw.length) {
            val c = raw[i]
            if (c == '\\' && i + 1 < raw.length) {
                val e = raw[i + 1]
                if (e == 'u' && i + 6 <= raw.length) {
                    val cp = raw.substring(i + 2, i + 6).toIntOrNull(16)
                    if (cp != null) { sb.append(cp.toChar()); i += 6; continue }
                }
                sb.append(e); i += 2; continue
            }
            sb.append(c); i++
        }
        return sb.toString()
    }
}
