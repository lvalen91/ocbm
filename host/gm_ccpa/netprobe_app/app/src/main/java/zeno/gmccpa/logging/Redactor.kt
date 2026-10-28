package zeno.gmccpa.logging

import java.security.MessageDigest
import java.security.SecureRandom

/** One entry per redaction category actually wired into [Redactor]. */
enum class RedactionCategory { WPA_PSK, MAC_ADDRESS, IPV4, VIN, LONG_DIGITS, LAT_LONG, SSID, IPV6, SERIAL, BOX_NAME }

/**
 * Line-at-a-time redaction for [LogExport]. One instance per export, so its per-category
 * value->placeholder maps (and therefore stable correlation across a whole session's worth of files)
 * live exactly as long as the export does and never leak into a later export's salt.
 *
 * Its own file, with no `android.*` import, since plan v5.8 V7 (Q014/Q023): the rules are
 * exercised by `RedactorTest` on the plain JVM (tools/jvm_tests.txt), which the Android-bound
 * `LogExport` could not be.
 *
 * **What this catches**, in application order (order matters where character classes could
 * overlap; a placeholder already substituted never re-matches a later rule — see the length/shape
 * guards on each regex):
 * 1. **WPA_PSK** — `psk=`, `passphrase=`, `wpa[-_]passphrase=`, `wpa[-_]psk=`, `pre-shared-key=`,
 *    `wifi[-_]pass=`, `hotspot[-_]pass=`, `password=` (case-insensitive), quoted or bare, key-value
 *    form as `wpa_supplicant.conf`, `hostapd.conf` and the OCBM config echo (`OcbmClient.subscribe`)
 *    write it. The key may be preceded by `_`/`-` (`wpa_passphrase`, `hotspot-pass`), so the rule
 *    is anchored on "not a letter or digit" rather than a word boundary. Only the value is replaced;
 *    the key stays so the line is still legible as "a passphrase was here."
 * 2. **SSID** (v5.8 V7 / Q014) — `wifi_ssid: <rest of line>` (the OCBM CT_SUBSCRIBE config block,
 *    `SubscribeConfig.build`, echoed by `OcbmClient.subscribe`) and `ssid=`/`ssid:` key-value forms
 *    (quoted, or bare up to the next whitespace/comma/brace) as Android's own Wi-Fi logcat writes
 *    them, and a quoted SSID after the word (`hotspot SSID 'x'`, the pre-fix1 supervisor STALLED
 *    line), and — since the 2026-10-08 bench export (12,260-line whole-OS capture: network names
 *    survived here and nowhere else) — the framework's `configKey "<name>"` form
 *    (`WifiConfigManager: Cannot find network with networkId -1 or configKey "<name>"WPA_PSK …`,
 *    `WifiConfiguration` dumps). Only the quoted name is replaced; the security label glued after
 *    the closing quote (`WPA_PSK`, `WPA_PSK_SHA256`, `NONE`, …) is a label, not a secret, and stays. Owner rule: no Wi-Fi identifier in durable output. Only the value is replaced. The
 *    length-only echo `wifi_ssid: set (N chars)` (OcbmClient.subscribe) is exempt: it carries no
 *    identifier and is the blank/truncated-entry diagnostic the echo exists for.
 * 3. **MAC_ADDRESS** — six colon- or dash-separated hex octet pairs. Deliberately not
 *    BT-MAC-specific: `CT_PHONE_IDENT.deviceID` (`ocbm/OcbmProto.kt:114-116`) is syntactically identical
 *    to a Wi-Fi BSSID or any other MAC-shaped string, and there is no way to tell them apart from
 *    text alone — so this rule catches both on purpose. Over-redacting a non-BT MAC is the safe
 *    direction. Runs BEFORE the IPv6 rule, so a MAC can never be read as six IPv6 groups.
 * 4. **LAT_LONG** — `lat[itude]=`/`lon[gitude]=` key-value decimal forms, and a bare
 *    `-?DD.DDDD, -?DDD.DDDD` comma-separated pair (a common raw GPS log shape). Redacts the numeric
 *    value(s) only.
 * 5. **IPV4** — every dotted-quad with valid octets, EXCEPT the four ranges below, which are
 *    fixed/well-known rather than vehicle-identifying and are left readable because a redacted
 *    `127.0.0.1` helps nobody debugging this project's own seams:
 *    - `127.0.0.0/8` — loopback; the receiver's A/V seams (:9001-9004) live here.
 *    - `169.254.0.0/16` — IPv4 link-local (RFC 3927), identical on every device.
 *    - `192.168.43.0/24` — Android's own tethering default AP subnet (`MdnsInspect.kt:15`).
 *    - `192.168.5.0/24` — this project's fixed CarPlay/AirPlay accessory subnet
 *      (`CarPlayRx.kt:353`, `:731`), not the vehicle's actual telematics or hotspot LAN.
 *    Every other IPv4 — including a `192.168.50.x`/`192.168.49.x`-style vehicle hotspot LAN or any
 *    public address — is redacted, because those DO identify this specific unit's network.
 * 6. **IPV6** (v5.8 V7 / Q023) — the full form (eight 1-4 hex-digit groups), the `::` compressed
 *    form (one or more groups on either side, or none), either with an optional `%zone` suffix
 *    (`fe80::1234:5678:9abc:def0%wlan0` — the phone's link-local address, which carries its
 *    interface identifier, is exactly the case that used to survive export). EXCEPT the two
 *    fixed addresses `::` (any) and `::1` (loopback), in any spelling, left readable for the same
 *    reason as `127.0.0.1`. The IPv4-mapped form `::ffff:a.b.c.d` is not matched as IPv6; its
 *    dotted-quad goes through the IPv4 rule. A scope operator whose either side holds a non-hex
 *    letter (`Class::member`, `std::string`) never matches: the address must not abut a word
 *    character on either side, and a bare `::` has no hex group. `Add::fee` or a bare `::face` IS
 *    taken (both sides are hex) — accepted over-redaction.
 * 7. **VIN** — 17-char alnum runs excluding `I`, `O`, `Q` (the ISO 3779 exclusion set), so a
 *    plausible-looking non-VIN string of the same shape is also caught; that is accepted
 *    over-redaction, not a bug.
 * 8. **LONG_DIGITS** — bare runs of 13+ digits: IMEI (15), MEID, ICCID, and similar identifiers.
 *    Deliberately wide; it will also catch large non-identifying counters or timestamps expressed
 *    as a single long integer. Over-redaction, accepted for the same reason as VIN. One exemption
 *    (E-F9, 2026-10-08): the `SESSION` line's `start_epoch_ms=<13 digits>` (`SessionSummary.emit`)
 *    is a wall-clock stamp, identifies nothing, and is the only key that lets a session line be
 *    matched against a logcat timestamp — a lookbehind on that key leaves it readable. The field's
 *    format is unchanged (the SESSION line is a versioned contract, see `SessionSummary`).
 * 9. **SERIAL** (E-F2, 2026-10-08) — the adapter's serial as `MGMT_INFO` carries it,
 *    `"serial":"<value>"` (ocbmd `mgmt_info`, read from the box's `/etc/serial_number`), and the
 *    key-value forms `serial=`/`serial_number:` as `UsbAttachActivity.logIdentity` writes the USB
 *    descriptor serial. The `SESSION` line's own `serial=ok|null|sec_exception|unknown` carries a
 *    verdict, not a value (`SessionSummary.SerialOutcome`), and `serial=<null>` / `serial=<Security…`
 *    are explanations: those tokens are exempt, everything else after the key is replaced.
 * 10. **BOX_NAME** — `CarLink-xxxx`: the adapter's Bluetooth name is derived from the last four hex
 *    digits of its Wi-Fi MAC (ocbmd `bt_name_from`, `ccpa/ocbmd/src/main.rs`), i.e. two octets of an
 *    address the MAC rule would otherwise redact whole. Caught as its own category so a reader can
 *    still correlate "the same adapter" across one export.
 *
 * **What this does NOT catch — state these plainly, a false sense of safety is worse than a known
 * gap:**
 * - **NMEA GPS sentences** (`$GPGGA,...`, `$GPRMC,...`) encode lat/long in `DDMM.MMMM,N/S` form,
 *   which [RE_LATLON_PAIR]/[RE_LATLON_KV] do not parse. A raw NMEA dump is NOT redacted by this
 *   class.
 * - **Bluetooth device names**, and a Wi-Fi SSID that appears OUTSIDE a `ssid`/`wifi_ssid`/`configKey`
 *   key-value form (free text such as `joined MyNetwork`), are not pattern-matched; a phone named "John
 *   Smith's iPhone" passes through untouched — EXCEPT where the framework prints the phone's hotspot
 *   as a saved network, which the `configKey` rule now takes. (Corrected 2026-10-08, v5.8 V7: the
 *   key-value SSID forms ARE matched since Q014.)
 * - **VIN embedded inside a longer alnum token** (no word boundary on one side, e.g. concatenated
 *   with a prefix/suffix) is missed — the regex requires a clean 17-char boundary.
 * - **Non-US phone numbers, email addresses, precise street addresses** in free-text log lines —
 *   none of these are pattern-matched.
 * - **IPv6** in a spelling other than the two above: an address that abuts a word character or a
 *   colon on either side (`addr=fe80::1` is fine, `xfe80::1` is not), or an address written with an embedded dotted-quad (`::ffff:1.2.3.4`,
 *   whose IPv4 part the IPv4 rule still takes). (Corrected 2026-10-08, v5.8 V7: until Q023 the
 *   statement here was "IPv6 addresses are not matched at all", and the phone's `fe80::` link-local
 *   address did survive export — rc2 logcat_cp8 18:20:00.292 INBOUND, 18:27:46.309 [uplink] mic.)
 * - A passphrase or key logged in a format other than `key=value` / `key: value` (e.g. split across
 *   two lines, or embedded in a JSON blob with unusual spacing this regex does not anticipate) may
 *   be missed. [RE_WPA] is deliberately permissive about the value's character class to catch the
 *   common cases, not exhaustive about the syntax. Two residual gaps in it: a value containing a
 *   space is redacted only up to that space, and a bare `pass=`/`pass:` key is deliberately NOT in
 *   its key set (see the comment on [RE_WPA]) — a secret logged under exactly that key passes
 *   through.
 * - This operates on captured *text* lines only. A binary-encoded OCBM frame dump (if one is ever
 *   logged as raw bytes rather than the JSON `CT_PHONE_IDENT` form) is not scanned for embedded
 *   MACs/identifiers at the byte level.
 *
 * Redaction is one-way by construction: the placeholder is `<REDACTED:CATEGORY:xxxx>` where `xxxx`
 * is 4 hex chars of `SHA-256(salt || category || rawValue)`, and the map from placeholder back to
 * `rawValue` is never built — only value-to-placeholder, and only for this instance's lifetime. The
 * salt is generated per export via [SecureRandom] and is not persisted or included in the output,
 * so placeholders are useless for correlation *across* two different exports, on purpose.
 */
internal class Redactor {

    private val salt = ByteArray(16).also { SecureRandom().nextBytes(it) }
    private val digest = MessageDigest.getInstance("SHA-256")
    private val cache = HashMap<String, String>() // "CATEGORY\u0000rawValue" -> placeholder
    private val counts = LinkedHashMap<RedactionCategory, Long>()

    fun redactLine(line: String): String {
        var s = line
        s = sub(s, RE_WPA, 2, RedactionCategory.WPA_PSK)
        s = sub(s, RE_SSID_LINE, 1, RedactionCategory.SSID)
        s = subSsidKv(s)
        s = sub(s, RE_SSID_QUOTED, 2, RedactionCategory.SSID)
        s = sub(s, RE_CONFIGKEY, 1, RedactionCategory.SSID)
        s = subWhole(s, RE_MAC, RedactionCategory.MAC_ADDRESS)
        s = sub(s, RE_SERIAL_JSON, 1, RedactionCategory.SERIAL)
        s = subSerialKv(s)
        s = subWhole(s, RE_BOX_NAME, RedactionCategory.BOX_NAME)
        s = sub(s, RE_LATLON_KV, 2, RedactionCategory.LAT_LONG)
        s = subPair(s, RE_LATLON_PAIR, RedactionCategory.LAT_LONG)
        s = subIpv4(s)
        s = subIpv6(s)
        s = subWhole(s, RE_VIN, RedactionCategory.VIN)
        s = subWhole(s, RE_LONG_DIGITS, RedactionCategory.LONG_DIGITS)
        return s
    }

    fun counts(): Map<RedactionCategory, Long> = counts.toMap()

    fun summaryLine(): String {
        val body = RedactionCategory.values().joinToString(" ") { "${it.name}=${counts[it] ?: 0}" }
        return "##### EXPORT REDACTION COUNTS: $body #####\n"
    }

    private fun placeholderFor(category: RedactionCategory, raw: String): String {
        val key = category.name + "\u0000" + raw
        return cache.getOrPut(key) {
            counts[category] = (counts[category] ?: 0L) + 1L
            digest.reset()
            digest.update(salt)
            digest.update(key.toByteArray(Charsets.UTF_8))
            val hex = digest.digest().joinToString("") { "%02x".format(it) }.take(4)
            "<REDACTED:${category.name}:$hex>"
        }
    }

    /** Replaces the whole match. */
    private fun subWhole(s: String, re: Regex, cat: RedactionCategory): String =
        re.replace(s) { m -> placeholderFor(cat, m.value) }

    /** Replaces only capture group [group], keeping the rest of the match (e.g. the `key=`) intact. */
    private fun sub(s: String, re: Regex, group: Int, cat: RedactionCategory): String =
        re.replace(s) { m ->
            val g = m.groups[group]
            // A value that is already a placeholder (an earlier rule's output, e.g. the key-value SSID
            // rule's `ssid="<REDACTED:SSID:ab12>"` seen by the quoted-SSID rule) is left alone: wrapping
            // it again would give one SSID two correlation tags and count it twice (fix2).
            if (g == null || g.value.isEmpty() || g.value.startsWith("<REDACTED:")) {
                m.value
            } else {
                // Splice by the group's own range, never by searching for its text: a value that is a
                // substring of the key (`passphrase=phrase`, `psk="k"`) would otherwise hit the key
                // first and leave the secret in the output.
                val off = m.range.first
                m.value.replaceRange(g.range.first - off, g.range.last + 1 - off, placeholderFor(cat, g.value))
            }
        }

    /** `ssid="quoted value"` (group 2) or `ssid=bare` (group 3): whichever matched is the value. */
    private fun subSsidKv(s: String): String =
        RE_SSID_KV.replace(s) { m ->
            val g = m.groups[2] ?: m.groups[3]
            if (g == null || g.value.isEmpty() || g.value.startsWith("<REDACTED:")) {
                m.value
            } else {
                val off = m.range.first
                m.value.replaceRange(g.range.first - off, g.range.last + 1 - off, placeholderFor(RedactionCategory.SSID, g.value))
            }
        }

    /**
     * `serial=<value>` / `serial_number: <value>`: the value is replaced unless it is one of the
     * [SessionSummary.SerialOutcome] verdict tags or an explanation in angle brackets (`<null>`,
     * `<SecurityException: …>`), which carry no identifier.
     */
    private fun subSerialKv(s: String): String =
        RE_SERIAL_KV.replace(s) { m ->
            val g = m.groups[2]
            if (g == null || g.value.isEmpty() || g.value.startsWith("<") || g.value.lowercase() in SERIAL_VERDICTS) {
                m.value
            } else {
                val off = m.range.first
                m.value.replaceRange(g.range.first - off, g.range.last + 1 - off, placeholderFor(RedactionCategory.SERIAL, g.value))
            }
        }

    /** Like [sub] but for a two-value match (a lat,long pair) — both numbers get their own placeholder. */
    private fun subPair(s: String, re: Regex, cat: RedactionCategory): String =
        re.replace(s) { m ->
            val a = placeholderFor(cat, m.groupValues[1])
            val b = placeholderFor(cat, m.groupValues[2])
            "$a,$b"
        }

    /** IPv4 needs octet-range validation and a whitelist check the generic helpers above don't do. */
    private fun subIpv4(s: String): String =
        RE_IPV4.replace(s) { m ->
            val octets = m.groupValues.drop(1).map { it.toInt() }
            if (octets.any { it > 255 } || isWhitelistedIpv4(octets)) m.value
            else placeholderFor(RedactionCategory.IPV4, m.value)
        }

    private fun isWhitelistedIpv4(o: List<Int>): Boolean {
        val (a, b) = o[0] to o[1]
        return a == 127 || // 127.0.0.0/8 loopback
            (a == 169 && b == 254) || // 169.254.0.0/16 link-local
            (a == 192 && b == 168 && (o[2] == 43 || o[2] == 5)) // Android tether default / this project's CarPlay AP subnet
    }

    /**
     * IPv6: the regex admits the shape; this validates the group count (a compressed form may hold
     * at most 7 explicit groups, a full form exactly 8) and leaves `::`/`::1` readable. The `%zone`
     * is redacted with the address when the address ends the token (`fe80::1%wlan0`, `[...%wlan0]`);
     * with a bare `:port` after the zone (`fe80::1%wlan0:7000`) the lookahead rejects the zone and
     * only the address is replaced — the interface name stays, which identifies nothing.
     */
    private fun subIpv6(s: String): String =
        RE_IPV6.replace(s) { m ->
            val addr = m.groupValues[1]
            if (isValidIpv6(addr) && !isWhitelistedIpv6(addr)) placeholderFor(RedactionCategory.IPV6, m.value)
            else m.value
        }

    companion object {
        // `wifi[-_]?pass` covers the OCBM config block's own `wifi_pass: ` key
        // (`OcbmProbe.btOnlyConfig`), which the psk/passphrase spellings alone let through verbatim;
        // `hotspot[-_]?pass` and `wpa[-_]?passphrase` (hostapd.conf's key) and `password` were added
        // 2026-10-08 (E-F3). Deliberately NOT a bare `pass`: that over-redacts ~49 unrelated
        // `pass:`/`pass=` lines of third-party logcat, and these exports are the primary debugging
        // artifact for this unit. The lookbehind replaces `\b` so a key reached through `_`/`-`
        // (`wpa_passphrase`) still matches; longer alternatives come first so the whole key is kept.
        private val RE_WPA = Regex(
            "(?i)(?<![A-Za-z0-9])(wpa[-_]?passphrase|passphrase|password|wpa[-_]?psk|psk|pre-?shared-?key|wifi[-_]?pass|hotspot[-_]?pass)\\s*[:=]\\s*\"?([^\",\\s}]+)\"?"
        )
        /** The MGMT_INFO JSON form, `"serial":"<value>"` (ocbmd `mgmt_info`); group 1 is the value. */
        private val RE_SERIAL_JSON = Regex("(?i)\"serial\"\\s*:\\s*\"([^\"]+)\"")
        /** `serial=<value>` / `serial_number: <value>`, bare or quoted; group 2 is the value. See [subSerialKv]. */
        // The lookbehind excludes ':' for the same reason as RE_SSID_KV: this class's own
        // `<REDACTED:SERIAL:ab12>` must not re-match as `SERIAL:ab12>`.
        private val RE_SERIAL_KV = Regex("(?i)(?<![A-Za-z0-9_:-])(serial(?:[-_]?number)?)\\s*[:=]\\s*\"?([^\",\\s}]+)\"?")
        /** `SessionSummary.SerialOutcome` tags: verdicts about the serial read, never the serial itself. */
        private val SERIAL_VERDICTS = setOf("ok", "null", "sec_exception", "unknown", "none")
        /** The adapter's derived Bluetooth name (see class KDoc, rule 10). */
        private val RE_BOX_NAME = Regex("(?<![A-Za-z0-9])CarLink-[0-9A-Fa-f]{4}(?![A-Za-z0-9])")
        /**
         * The OCBM config block line, `wifi_ssid: <value>` — an SSID may contain spaces, and this
         * key is only ever written one value per line (`SubscribeConfig.build`), so the value runs to
         * the end of the line (trailing whitespace excluded).
         */
        private val RE_SSID_LINE = Regex("(?i)\\bwifi[-_]?ssid\\s*:\\s*(?!set \\(\\d+ chars\\)\\s*$)(\\S.*?)\\s*$")
        /**
         * Prose that quotes an SSID after the word, with or without a separator: `hotspot SSID 'x'`
         * (the pre-v5.8-fix1 SessionSupervisor STALLED line, still present in older captures) and
         * `SSID: 'x'`. Single or double quotes.
         */
        // fix2: the closing quote must match the opening one (backreference), the value is greedy so an
        // SSID containing the other quote or a doubled quote (`"Joe's Net"`, `'it''s'`) is taken whole
        // and no fragment survives; a value that is already a placeholder is skipped (see [sub]).
        private val RE_SSID_QUOTED = Regex("(?i)(?<![A-Za-z0-9_:-])ssid\\s*[:=]?\\s*(['\"])((?!<REDACTED:).+)\\1")
        /** `ssid="quoted"` / `ssid=bare` / `SSID: "quoted"` — Android's wpa_supplicant/WifiService logcat shapes. */
        // The lookbehind excludes ':' too: `<REDACTED:SSID:ab12>`, this class's own placeholder from the
        // line rule above, would otherwise re-match as `SSID:ab12>` and be wrapped a second time.
        // fix2: the bare value excludes a single quote too, so `SSID: 'O"Brien'` is left to the quoted
        // rule below as one value instead of being split at the quote.
        private val RE_SSID_KV = Regex("(?i)(?<![A-Za-z0-9_:-])(ssid)\\s*[:=]\\s*(?:\"([^\"]*)\"|([^\"',\\s}]+))")
        /**
         * The framework's `configKey "<name>"` (WifiConfigManager / WifiConfiguration; 2026-10-08
         * bench export). Group 1 is the name; the security label after the closing quote
         * (`"<name>"WPA_PSK`) is deliberately outside the match. A value that is already a
         * placeholder is skipped by [sub], so a line that also carries `ssid="<name>"` gets one tag.
         */
        private val RE_CONFIGKEY = Regex("(?i)(?<![A-Za-z0-9_])configKey\\s*[:=]?\\s*\"([^\"]*)\"")
        private val RE_MAC = Regex("\\b([0-9A-Fa-f]{2})([:-])([0-9A-Fa-f]{2})(\\2[0-9A-Fa-f]{2}){4}\\b")
        private val RE_LATLON_KV = Regex("(?i)\\b(lat(?:itude)?|lon(?:g|gitude)?)\\s*[:=]\\s*(-?\\d{1,3}\\.\\d{3,})")
        private val RE_LATLON_PAIR = Regex("(-?\\d{1,3}\\.\\d{4,6})\\s*,\\s*(-?\\d{1,3}\\.\\d{4,6})")
        private val RE_IPV4 = Regex("\\b(\\d{1,3})\\.(\\d{1,3})\\.(\\d{1,3})\\.(\\d{1,3})\\b")
        /**
         * Group 1 = the address; group 2 = an optional `%zone`. Two alternatives: the full form
         * (exactly 8 groups) or a compressed form (`::` with up to 7 groups around it, at least one).
         * Bounded on both sides by "not a word character or colon", so a time `18:27:46`, a port suffix
         * `]:7011` after a bracketed address, a placeholder's `:ab12>` tail and a scope operator
         * (`std::string`, `ns::face` — the `::` abuts a letter) cannot be swallowed.
         */
        private val RE_IPV6 = Regex(
            "(?<![\\w:])(" +
                "(?:[0-9A-Fa-f]{1,4}:){7}[0-9A-Fa-f]{1,4}" +
                "|(?:[0-9A-Fa-f]{1,4}(?::[0-9A-Fa-f]{1,4}){0,6})?::(?:[0-9A-Fa-f]{1,4}(?::[0-9A-Fa-f]{1,4}){0,6})?" +
                ")(%[A-Za-z0-9_.-]+)?(?![\\w:])"
        )
        private val RE_VIN = Regex("\\b[A-HJ-NPR-Z0-9]{17}\\b")
        // `(?<!start_epoch_ms=)`: the SESSION line's wall-clock stamp stays readable (class KDoc, rule 8).
        private val RE_LONG_DIGITS = Regex("(?<!start_epoch_ms=)\\b\\d{13,}\\b")

        /** Shape-validated: a full form has 8 groups; a compressed one has exactly one `::` and at most 7 groups, at least one. */
        internal fun isValidIpv6(addr: String): Boolean {
            val dc = addr.indexOf("::")
            if (dc < 0) return addr.split(':').size == 8
            if (addr.indexOf("::", dc + 1) >= 0) return false
            val groups = addr.split(':').filter { it.isNotEmpty() }
            return groups.size in 1..7
        }

        /** `::` (any) and `::1` (loopback), in any spelling (`0:0:0:0:0:0:0:1`, `0::1`, `::0001`). */
        internal fun isWhitelistedIpv6(addr: String): Boolean {
            val groups = addr.split(':').filter { it.isNotEmpty() }.map { it.toInt(16) }
            if (groups.any { it != 0 } && !(groups.last() == 1 && groups.dropLast(1).all { it == 0 })) return false
            // Every explicit group is 0 (any) or the only non-zero group is a trailing 1 (loopback);
            // with `::` present the omitted groups are zero, and a full form has 8 explicit groups.
            return addr.contains("::") || groups.size == 8
        }
    }
}
