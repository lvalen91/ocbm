package zeno.gmccpa.logging

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Plan v5.8 V7 / Q014 (SSID) and Q023 (IPv6) on the export redactor, plus the pre-existing rules
 * it was moved out of LogExport.kt with. Every address, MAC and SSID here is synthetic
 * (RFC 3849 `2001:db8::/32`, locally-administered MACs, made-up names).
 */
class RedactorTest {

    private val r = Redactor()

    private fun placeholderOf(cat: RedactionCategory, s: String): Boolean =
        Regex("<REDACTED:${cat.name}:[0-9a-f]{4}>").containsMatchIn(s)

    // ---- Q014: SSID ------------------------------------------------------------------------------

    @Test fun ocbmConfigBlockSsidLineRedactedKeyKept() {
        val out = r.redactLine("     | wifi_ssid: Bench Net 5G  ")
        assertTrue(out, out.startsWith("     | wifi_ssid: <REDACTED:SSID:"))
        assertFalse(out, out.contains("Bench"))
        assertFalse(out, out.contains("5G"))
        assertEquals(1L, r.counts()[RedactionCategory.SSID])
    }

    @Test fun ssidKeyValueQuotedAndBare() {
        val quoted = r.redactLine("WifiService: connect ssid=\"Quoted Name\" bssid=any")
        assertTrue(quoted, quoted.contains("ssid=\"<REDACTED:SSID:"))
        assertFalse(quoted, quoted.contains("Quoted"))
        assertTrue(quoted, quoted.endsWith("\" bssid=any"))
        val bare = r.redactLine("wpa_supplicant: SSID: BareName freq=5180")
        assertTrue(bare, bare.contains("SSID: <REDACTED:SSID:"))
        assertFalse(bare, bare.contains("BareName"))
        assertTrue(bare, bare.endsWith("> freq=5180"))
    }

    @Test fun placeholdersAreNeverReWrappedByALaterRule() {
        // The SSID key-value rule and the IPv6 rule both run after rules that already produced a
        // `<REDACTED:CAT:ab12>`; neither may read `SSID:ab12>` or `:ab12` as a new match.
        val a = r.redactLine("wifi_ssid: OnlyOnce")
        assertEquals(a, 1, Regex("<REDACTED:").findAll(a).count())
        val b = r.redactLine("psk=abc ssid=\"n\" peer fe80::2 mac 02:00:00:00:00:02")
        assertEquals(b, 4, Regex("<REDACTED:").findAll(b).count())
        assertFalse(b, b.contains("REDACTED:SSID:<"))
        assertFalse(b, b.contains("REDACTED:IPV6:<"))
    }

    @Test fun ssidPlaceholderIsStableWithinOneRedactorAndCategoryCounted() {
        val a = r.redactLine("wifi_ssid: SameName")
        val b = r.redactLine("ssid=SameName")
        val pa = Regex("<REDACTED:SSID:([0-9a-f]{4})>").find(a)!!.groupValues[1]
        val pb = Regex("<REDACTED:SSID:([0-9a-f]{4})>").find(b)!!.groupValues[1]
        assertEquals(pa, pb)
        assertEquals(1L, r.counts()[RedactionCategory.SSID])
        assertTrue(r.summaryLine().contains("SSID=1"))
    }

    /** fix1 (review L6 F0): the pre-fix1 supervisor STALLED wording, as it appears in older captures. */
    @Test fun quotedSsidInProseIsRedacted() {
        val line = "ARMED -> STALLED  (Bluetooth completed but no CarPlay connection followed — check the hotspot SSID 'HomeNet5' and passphrase on this screen match the vehicle hotspot, then reconnect Bluetooth from the iPhone)"
        val out = r.redactLine(line)
        assertFalse(out, out.contains("HomeNet5"))
        assertTrue(out, out.contains("hotspot SSID '<REDACTED:SSID:"))
        // Only the value is replaced; the quotes around it stay, like the key in `psk="…"`.
        assertTrue(out, out.endsWith(">' and passphrase on this screen match the vehicle hotspot, then reconnect Bluetooth from the iPhone)"))
        val dq = r.redactLine("SSID: \"Two Words\" joined")
        assertFalse(dq, dq.contains("Two Words"))
        assertTrue(dq, dq.endsWith("\" joined"))
    }

    /** fix2: one SSID, five log shapes, ONE placeholder tag and ONE count — no rule re-wraps another's output. */
    @Test fun sameTagAcrossEveryShapeAndCountedOnce() {
        val shapes = listOf(
            "     | wifi_ssid: Home Net",
            "wpa: ssid=Home",                 // bare form stops at whitespace: a different value on purpose, see below
            "WifiService: connect ssid=\"Home Net\" bssid=any",
            "wpa_supplicant: SSID: \"Home Net\" freq=5180",
            "ARMED -> STALLED  (check the hotspot SSID 'Home Net' and passphrase)",
        )
        val tags = shapes.map { line ->
            val out = r.redactLine(line)
            assertFalse(out, out.contains("Home Net"))
            assertEquals(out, 1, Regex("<REDACTED:").findAll(out).count())
            Regex("<REDACTED:SSID:([0-9a-f]{4})>").find(out)!!.groupValues[1]
        }
        assertEquals(tags.toString(), 1, tags.filterIndexed { i, _ -> i != 1 }.toSet().size)
        // "Home Net" once (four shapes) + "Home" once (the bare form) = 2 distinct values.
        assertEquals(2L, r.counts()[RedactionCategory.SSID])
        val fresh = Redactor()
        fresh.redactLine("ssid=\"Home Net\" freq=2412")
        assertEquals("one occurrence counts once", 1L, fresh.counts()[RedactionCategory.SSID])
    }

    /** fix2: quotes that do not match the value's contents — no fragment of the SSID survives. */
    @Test fun mismatchedQuotesLeaveNoFragment() {
        val a = r.redactLine("hotspot SSID \"Joe's Net\" x")
        assertTrue(a, Regex("^hotspot SSID \"<REDACTED:SSID:[0-9a-f]{4}>\" x$").matches(a))
        val b = r.redactLine("SSID 'it''s' x")
        assertTrue(b, Regex("^SSID '<REDACTED:SSID:[0-9a-f]{4}>' x$").matches(b))
        val c = r.redactLine("SSID: 'O\"Brien' joined")
        assertTrue(c, Regex("^SSID: '<REDACTED:SSID:[0-9a-f]{4}>' joined$").matches(c))
    }

    /** fix1 (review L6 F5): the length-only echo carries no identifier and stays readable in an export. */
    @Test fun lengthOnlySsidEchoIsNotRedacted() {
        val line = "     | wifi_ssid: set (12 chars)"
        assertEquals(line, r.redactLine(line))
        assertEquals(null, r.counts()[RedactionCategory.SSID])
        // The value form next to it is still taken.
        assertTrue(r.redactLine("     | wifi_ssid: set (12 chars) extra").contains("<REDACTED:SSID:"))
    }

    @Test fun ssidRuleDoesNotTouchPassphraseOrUnrelatedWords() {
        val out = r.redactLine("wifi_pass: hunter2hunter2 and the word bssid=02:00:00:00:00:01 stays")
        assertTrue(out, out.contains("wifi_pass: <REDACTED:WPA_PSK:"))
        // `bssid=` is a MAC, not an SSID: the SSID rule must not claim it (the MAC rule does).
        assertFalse(out, placeholderOf(RedactionCategory.SSID, out))
        assertTrue(out, placeholderOf(RedactionCategory.MAC_ADDRESS, out))
    }

    // ---- Q023: IPv6 ------------------------------------------------------------------------------

    @Test fun linkLocalWithZoneIsRedactedWhole() {
        val out = r.redactLine("INBOUND from /fe80::211:22ff:fe33:4455%wlan0 :49152")
        assertEquals("INBOUND from /" + placeholderOnly(out, "INBOUND from /", " :49152") + " :49152", out)
        assertFalse(out, out.contains("fe80"))
        assertFalse(out, out.contains("wlan0"))
        assertEquals(1L, r.counts()[RedactionCategory.IPV6])
    }

    private fun placeholderOnly(out: String, prefix: String, suffix: String): String {
        val mid = out.removePrefix(prefix).removeSuffix(suffix)
        assertTrue(out, Regex("^<REDACTED:IPV6:[0-9a-f]{4}>$").matches(mid))
        return mid
    }

    @Test fun fullAndCompressedFormsRedacted() {
        val full = r.redactLine("peer 2001:0db8:0000:0000:0000:ff00:0042:8329 said hi")
        assertTrue(full, placeholderOf(RedactionCategory.IPV6, full))
        assertFalse(full, full.contains("8329"))
        val comp = r.redactLine("peer 2001:db8::ff00:42:8329 said hi")
        assertTrue(comp, placeholderOf(RedactionCategory.IPV6, comp))
        assertFalse(comp, comp.contains("db8"))
        val trailing = r.redactLine("prefix 2001:db8:1:: configured")
        assertTrue(trailing, placeholderOf(RedactionCategory.IPV6, trailing))
        val bracketed = r.redactLine("[uplink] mic from [fe80::1234:5678:9abc:def0%eth1]:7011")
        assertTrue(bracketed, bracketed.contains("[<REDACTED:IPV6:"))
        assertTrue(bracketed, bracketed.endsWith(">]:7011"))
    }

    @Test fun loopbackAndAnyStayReadable() {
        for (s in listOf("bound [::]:7011", "bound [::1]:7011", "peer 0:0:0:0:0:0:0:1 ok", "peer 0::1 ok", "peer ::0001 ok", "peer 0:0:0:0:0:0:0:0 ok")) {
            assertEquals(s, r.redactLine(s))
        }
        assertEquals(null, r.counts()[RedactionCategory.IPV6])
    }

    @Test fun timesPortsMacsAndScopeOperatorsAreNotIpv6() {
        for (s in listOf(
            "10-08 18:27:46.309 I/x: tick",
            "connect :9001 ok",
            "ref CarPlayRx::stop called",
            "Foo::bar and std::string and ns::face",   // scope operators abut word characters: never an address
        )) assertEquals(s, r.redactLine(s))
        // A MAC is six groups: the MAC rule takes it, the IPv6 rule must not re-wrap the placeholder.
        val mac = r.redactLine("bssid 02:00:00:00:00:01 seen")
        assertTrue(mac, Regex("^bssid <REDACTED:MAC_ADDRESS:[0-9a-f]{4}> seen$").matches(mac))
        assertEquals(null, r.counts()[RedactionCategory.IPV6])
    }

    @Test fun ipv4MappedLeavesIpv6RuleToIpv4Rule() {
        val out = r.redactLine("peer ::ffff:10.1.2.3 ok")
        assertEquals(null, r.counts()[RedactionCategory.IPV6])
        assertTrue(out, out.contains("::ffff:<REDACTED:IPV4:"))
    }

    @Test fun ipv6ShapeValidation() {
        assertTrue(Redactor.isValidIpv6("fe80::1"))
        assertTrue(Redactor.isValidIpv6("2001:db8:0:0:0:0:0:1"))
        assertTrue(Redactor.isValidIpv6("1:2:3:4:5:6:7::"))
        assertFalse("bare :: is not an address here", Redactor.isValidIpv6("::"))
        assertFalse("two :: runs", Redactor.isValidIpv6("1::2::3"))
        assertFalse("8 groups plus ::", Redactor.isValidIpv6("1:2:3:4:5:6:7:8::"))
        assertTrue(Redactor.isWhitelistedIpv6("::1"))
        assertTrue(Redactor.isWhitelistedIpv6("::"))
        assertTrue(Redactor.isWhitelistedIpv6("0:0:0:0:0:0:0:1"))
        assertFalse(Redactor.isWhitelistedIpv6("fe80::1"))
        assertFalse(Redactor.isWhitelistedIpv6("::2"))
        assertFalse("seven zero groups and a 1 is not loopback without ::", Redactor.isWhitelistedIpv6("0:0:0:0:0:0:1"))
    }

    // ---- the moved rules still hold ------------------------------------------------------------

    @Test fun existingRulesUnchangedByTheMove() {
        val out = r.redactLine("psk=\"s3cret\" lat=12.3456 lon=-98.7654 addr 10.0.0.7 lo 127.0.0.1 ap 192.168.43.1 vin 1HGBH41JXMN109186 id 123456789012345")
        assertTrue(out, out.contains("psk=\"<REDACTED:WPA_PSK:"))
        assertTrue(out, out.contains("lat=<REDACTED:LAT_LONG:"))
        assertTrue(out, out.contains("addr <REDACTED:IPV4:"))
        assertTrue(out, out.contains("lo 127.0.0.1 ap 192.168.43.1"))
        assertTrue(out, out.contains("vin <REDACTED:VIN:"))
        assertTrue(out, out.contains("id <REDACTED:LONG_DIGITS:"))
        val summary = r.summaryLine()
        for (c in RedactionCategory.values()) assertTrue(summary, summary.contains("${c.name}="))
    }

    // ---- E-F2 / E-F3 / E-F9 (2026-10-08): serial, box name, passphrase spellings, start_epoch_ms ----

    /** E-F2: the MGMT_INFO JSON's `serial` and `devices` MACs, as one line, are both taken. */
    @Test fun mgmtInfoJsonSerialAndMacsRedacted() {
        val json = "{\"bt_mac\":\"02:00:00:00:00:55\",\"wifi_mac\":\"02:00:00:00:44:66\",\"serial\":\"SYNTH0001\"," +
            "\"name\":\"CarLink-4466\",\"devices\":[\"02:00:00:00:00:77\",\"02:00:00:00:00:78\"]}"
        val out = r.redactLine("     $json")
        assertFalse(out, out.contains("SYNTH0001"))
        assertTrue(out, out.contains("\"serial\":\"<REDACTED:SERIAL:"))
        assertFalse(out, out.contains("02:00:00"))
        assertFalse(out, out.contains("CarLink-4466"))
        assertTrue(out, out.contains("\"name\":\"<REDACTED:BOX_NAME:"))
        assertEquals(1L, r.counts()[RedactionCategory.SERIAL])
        assertEquals(4L, r.counts()[RedactionCategory.MAC_ADDRESS])
        assertEquals(1L, r.counts()[RedactionCategory.BOX_NAME])
        // The SESSION_DETAIL line logs the same JSON whole: same coverage, same tags.
        val detail = r.redactLine("SESSION_DETAIL id=3 bond_start_json=$json")
        assertFalse(detail, detail.contains("SYNTH0001"))
        assertFalse(detail, detail.contains("02:00:00"))
        assertEquals(1L, r.counts()[RedactionCategory.SERIAL])
    }

    /** E-F2: a MAC that a 110-char chunk used to split is whole on an element-boundary chunk. */
    @Test fun elementBoundaryChunksKeepEveryMacRedactable() {
        val json = "{\"serial\":\"SYNTH0002\",\"devices\":[" +
            (0 until 20).joinToString(",") { "\"02:00:00:00:%02x:%02x\"".format(it, it + 1) } + "]}"
        val chunks = zeno.gmccpa.ocbm.MgmtInfo.logChunks(json, 110)
        assertTrue(chunks.toString(), chunks.size > 1)
        assertEquals(json, chunks.joinToString(""))
        for (c in chunks) {
            assertTrue(c, c.length <= 110 || !c.contains(","))
            val out = r.redactLine("     $c")
            assertFalse("no MAC fragment may survive: $out", Regex("[0-9a-f]{2}:[0-9a-f]{2}").containsMatchIn(out))
            assertFalse(out, out.contains("SYNTH0002"))
        }
        assertEquals(20L, r.counts()[RedactionCategory.MAC_ADDRESS])
        // The old `chunked(110)` is exactly what let a MAC through: prove the premise, not just the fix.
        val leaked = json.chunked(110).map { r.redactLine(it) }.any { Regex("\\b[0-9a-f]{2}:[0-9a-f]{2}\\b").containsMatchIn(it) }
        assertTrue("fixed-width chunking splits a MAC", leaked)
    }

    @Test fun serialKeyValueFormsRedactedButVerdictTokensKept() {
        val usb = r.redactLine("attach dev: name=/dev/bus/usb/001/004 id=1004 serial=SYNTHSER01 mfr=x product=y")
        assertFalse(usb, usb.contains("SYNTHSER01"))
        assertTrue(usb, usb.contains("serial=<REDACTED:SERIAL:"))
        assertTrue(usb, usb.endsWith("> mfr=x product=y"))
        val num = r.redactLine("Serial_Number: \"SYNTHSER02\" ok")
        assertTrue(num, num.contains("Serial_Number: \"<REDACTED:SERIAL:"))
        for (s in listOf(
            "SESSION v=3 id=1 serial=ok prompted=false",
            "SESSION v=3 id=1 serial=sec_exception prompted=false",
            "SESSION v=3 id=1 serial=unknown",
            "attach dev: serial=<null> mfr=x",
            "attach dev: serial=<SecurityException: user 10 denied> — USB permission NOT held",
            "serialOutcome=OK serial_outcome=OK",
        )) assertEquals(s, r.redactLine(s))
        assertEquals(2L, r.counts()[RedactionCategory.SERIAL])
        // A placeholder's own `SERIAL:ab12>` tail is not re-matched by the key-value rule.
        val once = r.redactLine("serial=SYNTHSER01")
        assertEquals(once, 1, Regex("<REDACTED:").findAll(once).count())
    }

    @Test fun boxNameIsItsOwnCategoryAndNotPartOfLongerTokens() {
        val out = r.redactLine("adapter CarLink-ab12 linked; box name CarLink-AB12")
        assertFalse(out, out.contains("ab12"))
        assertFalse(out, out.contains("AB12"))
        assertEquals(2L, r.counts()[RedactionCategory.BOX_NAME]) // case differs: two values
        for (s in listOf("CarLink", "CarLink-evil", "XCarLink-ab12", "CarLink-ab123", "CarLink-ab1")) assertEquals(s, r.redactLine(s))
    }

    /** E-F3: every passphrase spelling, and the words the rule must leave alone. */
    @Test fun passphraseSpellings() {
        for (line in listOf(
            "wpa_passphrase=secret1", "wpa-passphrase: secret1", "passphrase=\"secret1\"",
            "password=secret1", "Password: secret1", "hotspot_pass=secret1", "hotspot-pass: secret1",
            "wifi_pass: secret1", "WPA-PSK=secret1", "psk=\"secret1\"", "pre-shared-key=secret1",
        )) {
            val out = r.redactLine(line)
            assertFalse("$line -> $out", out.contains("secret1"))
            assertTrue(out, out.contains("<REDACTED:WPA_PSK:"))
            // The key is kept whole: `wpa_passphrase=` must not come back as `wpa_<REDACTED…`.
            assertTrue(out, out.startsWith(line.substringBefore('=').substringBefore(':')))
        }
        assertEquals(1L, r.counts()[RedactionCategory.WPA_PSK])
        for (s in listOf("bypass=1", "pass: 7", "pass=secret1", "passthrough=on", "compass=north", "hotspot_passed=true")) {
            assertEquals(s, r.redactLine(s))
        }
        assertEquals(1L, r.counts()[RedactionCategory.WPA_PSK])
    }

    /** E-F9: the SESSION line's wall-clock stamp stays; any other 13+ digit run is still taken. */
    @Test fun startEpochMsIsExemptFromLongDigits() {
        val line = "SESSION v=3 id=1 start_epoch_ms=1759900000000 attach_at_ms=12345 serial=ok"
        assertEquals(line, r.redactLine(line))
        val other = r.redactLine("imei=123456789012345 and start_epoch_ms=1759900000000")
        assertTrue(other, other.contains("imei=<REDACTED:LONG_DIGITS:"))
        assertTrue(other, other.contains("start_epoch_ms=1759900000000"))
        assertEquals(1L, r.counts()[RedactionCategory.LONG_DIGITS])
    }

    // ---- configKey (2026-10-08 bench export): the framework's saved-network form -----------------

    @Test fun configKeyNetworkNameRedacted_securityLabelKept() {
        val line = "E WifiConfigManager: Cannot find network with networkId -1 or configKey \"Synth Net 5G\"WPA_PSK upgradable security type check"
        val out = r.redactLine(line)
        assertFalse(out, out.contains("Synth Net"))
        assertTrue(out, Regex("configKey \"<REDACTED:SSID:[0-9a-f]{4}>\"WPA_PSK upgradable").containsMatchIn(out))
        assertTrue(out, out.startsWith("E WifiConfigManager: Cannot find network with networkId -1 or configKey "))
        val sha = r.redactLine("WifiConfiguration: ID: 7 SSID: \"Synth Net 5G\" configKey=\"Synth Net 5G\"WPA_PSK_SHA256 BSSID: null")
        assertFalse(sha, sha.contains("Synth Net"))
        assertTrue(sha, sha.contains("\"WPA_PSK_SHA256 BSSID: null"))
        assertEquals("one value, one count across both shapes", 1L, r.counts()[RedactionCategory.SSID])
    }

    /** A line that carries the name twice (`SSID: "x"` and `configKey "x"`) gets ONE tag per site and no double wrap. */
    @Test fun configKeyAndSsidOnOneLine_sameTag_noDoubleWrap() {
        val out = r.redactLine("WifiConfigManager: ssid=\"Phone Hotspot\" configKey \"Phone Hotspot\"WPA_PSK")
        assertFalse(out, out.contains("Phone Hotspot"))
        assertEquals(out, 2, Regex("<REDACTED:SSID:").findAll(out).count())
        assertFalse(out, out.contains("REDACTED:SSID:<"))
        val tags = Regex("<REDACTED:SSID:([0-9a-f]{4})>").findAll(out).map { it.groupValues[1] }.toSet()
        assertEquals(1, tags.size)
        assertEquals(1L, r.counts()[RedactionCategory.SSID])
        // An empty configKey value is not a secret and is left as is (no empty placeholder).
        val empty = "configKey \"\"NONE"
        assertEquals(empty, r.redactLine(empty))
    }

    /** Negative: quoted text without a `configKey` key, and words merely containing it, are untouched. */
    @Test fun configKeyNegatives() {
        for (s in listOf(
            "WifiService: saved networks \"Quoted Thing\" WPA_PSK label only",
            "WifiConfigManager: network with networkId -1 found, security WPA_PSK_SHA256",
            "myconfigKey \"NotIt\" (prefixed word)",
        )) assertEquals(s, r.redactLine(s))
        assertEquals(null, r.counts()[RedactionCategory.SSID])
    }
}
