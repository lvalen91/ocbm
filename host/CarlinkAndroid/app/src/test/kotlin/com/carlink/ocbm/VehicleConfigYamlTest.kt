package com.carlink.ocbm

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Golden test for the pushed vehicle config.
 *
 * This exists because of a specific, recorded failure: a hand-typed fixture once certified a broken
 * emitter whose missing trailing newline glued two keys together, so serde rejected the whole document
 * and the box silently fell back to 1920x720 with HEVC off. The byte-for-byte comparison below is the
 * mechanism that would have caught it, and the per-line newline assertion pins that exact class of bug.
 *
 * **If you change the emitter, change this literal in the same commit.**
 */
class VehicleConfigYamlTest {
    private val expected =
        """
name: "CarLink GM 2400x960"
version: 1
wireless: true
hot_handover: false
pairing: just_works
rightHandDrive: false
nightMode: false
displayPanelsConfig:
  mainDisplayPanel:
    displayPanelID: DisplayPanel.Main
    pixelDimensions:
      width: 2400
      height: 960
  altDisplayPanels: []
videoStreamsConfig:
  mainVideoStream:
    videoStreamID: VideoStream.Main
    pixelDimensions:
      width: 2400
      height: 960
    maxFPS: 60
    viewAreas:
    - viewArea:
        originX: 0
        originY: 0
        width: 2400
        height: 960
      safeArea:
        originX: 0
        originY: 0
        width: 2400
        height: 960
      drawUIOutsideSafeArea: false
    hidConfig:
      dPadSupport: false
      knobSupport: false
      knobSupportsHomeAndBackButton: false
      knobSupportsNudge: false
      mediaButtonsSupport: true
      telephonyButtonsSupport: false
      touchpadSupport: false
      touchpadButtonsSupport: false
      touchScreenMode: High Fidelty
      touchScreenSupportsCancel: true
      touchScreenSupportsMultiTouch: true
      steeringWheelSupport: false
    primaryInput: Touchpad
  altVideoStreams: []
accessoryConfig:
  enablesMainBufferedAudio: false
  enablesHEVC: true
  enablesUIAppearance: true
  enablesMapAppearance: true
  enablesCornerMasks: false
  enablesVideoPlayback: true
  enablesViewAreas: false
  enablesEnhancedSiri: false
  enablesFocusTransfer: false
  enablesUIContext: false
  enablesUISync: false
  enablesFileTransfer: false
  enablesLogTransfer: false
  enablesVehicleDataProtocol: false
  enablesDCX: false
  appDrivenSetup: false
limitedUIConfig:
  softKeyboard: true
  softPhoneKeypad: true
  nonMusicLists: true
  musicLists: true
  japanMaps: false
  longAlerts: true
audio:
  wired:
    preset: wired_pcm
  wireless:
    preset: wireless_8
metadata:
  tier: proven
""".trimStart('\n')

    @Test
    fun `the default document renders byte for byte`() {
        assertEquals(expected, VehicleConfigYaml.render(VehicleConfigSpec()))
    }

    /** The exact 2026-08-11 failure mode: one line without its newline glues two keys together. */
    @Test
    fun `every line ends with a newline, including the last`() {
        val doc = VehicleConfigYaml.render(VehicleConfigSpec())
        assertTrue("document must end with a newline", doc.endsWith("\n"))
        assertFalse("no blank lines — serde tolerates them but the fixture does not", doc.contains("\n\n"))
        assertFalse("no trailing whitespace", doc.lines().any { it != it.trimEnd() })
    }

    @Test
    fun `the target geometry appears in all four places`() {
        val doc = VehicleConfigYaml.render(VehicleConfigSpec())
        // panel, stream, viewArea, safeArea — a mismatch between any two is a silent letterbox
        assertEquals(4, Regex("width: 2400").findAll(doc).count())
        assertEquals(4, Regex("height: 960").findAll(doc).count())
    }

    @Test
    fun `Apple's own misspelling is preserved`() {
        // "High Fidelty" is Apple's typo. Correcting it costs the touch bits.
        assertTrue(VehicleConfigYaml.render(VehicleConfigSpec()).contains("touchScreenMode: High Fidelty"))
    }

    @Test
    fun `every unused HID device stays unadvertised`() {
        val doc = VehicleConfigYaml.render(VehicleConfigSpec())
        for (k in listOf(
            "dPadSupport",
            "knobSupport",
            "knobSupportsHomeAndBackButton",
            "knobSupportsNudge",
            "telephonyButtonsSupport",
            "touchpadSupport",
            "touchpadButtonsSupport",
            "steeringWheelSupport",
        )) {
            assertTrue(
                "$k must be false — advertising an unserved HID has broken reconnect before",
                doc.contains("$k: false"),
            )
        }
        // The one input device we DO serve.
        assertTrue(doc.contains("mediaButtonsSupport: true"))
    }

    @Test
    fun `a resolution change moves geometry everywhere at once`() {
        val doc = VehicleConfigYaml.render(VehicleConfigSpec(width = 1920, height = 720))
        assertEquals(4, Regex("width: 1920").findAll(doc).count())
        assertEquals(4, Regex("height: 720").findAll(doc).count())
    }

    @Test
    fun `a safe area inset from a display cutout is emitted independently of the view area`() {
        val doc =
            VehicleConfigYaml.render(
                VehicleConfigSpec(
                    safeOriginX = 40,
                    safeOriginY = 10,
                    safeWidth = 2320,
                    safeHeight = 940,
                    drawUIOutsideSafeArea = true,
                ),
            )
        assertTrue(doc.contains("        originX: 40"))
        assertTrue(doc.contains("        width: 2320"))
        assertTrue(doc.contains("      drawUIOutsideSafeArea: true"))
        // ...while the view area still covers the whole panel.
        assertTrue(doc.contains("        width: 2400"))
    }

    @Test
    fun `an illegal frame rate is refused at construction rather than on the wire`() {
        // 24 was removed box-side in 2026-07; the box silently ignores anything but 30/60, which would
        // present as "my config didn't apply" rather than as an error.
        for (bad in listOf(24, 25, 0, 120)) {
            var threw = false
            try {
                VehicleConfigSpec(maxFps = bad)
            } catch (_: IllegalArgumentException) {
                threw = true
            }
            assertTrue("maxFPS=$bad must be rejected", threw)
        }
    }

    @Test
    fun `the refuted metadata tiers cannot be selected by accident`() {
        var threw = false
        try {
            VehicleConfigSpec(metadataTier = "rx-only")
        } catch (_: IllegalArgumentException) {
            threw = true
        }
        assertTrue("rx-only is a refuted dead end the box rejects", threw)
        // `all` is legal to express but device-refuted; it stays constructible so the choice is explicit.
        assertEquals("all", VehicleConfigSpec(metadataTier = "all").metadataTier)
    }

    @Test
    fun `a name containing a quote cannot tear the document`() {
        val doc = VehicleConfigYaml.render(VehicleConfigSpec(name = """Owner's "Roadster" \ EV"""))
        assertTrue(doc.startsWith("""name: "Owner's \"Roadster\" \\ EV"""" + "\n"))
        assertEquals(1, doc.lines().count { it.startsWith("name:") })
    }

    // ---- oemIconConfig ---------------------------------------------------------------------------

    @Test
    fun `oemIconConfig is omitted entirely when there are no images`() {
        val doc = VehicleConfigYaml.render(VehicleConfigSpec())
        assertFalse("an empty section is a different statement from none", doc.contains("oemIconConfig"))
    }

    @Test
    fun `oemIconConfig emits Apple's three sizes in the box's key order`() {
        val doc =
            VehicleConfigYaml.render(
                VehicleConfigSpec(
                    oemIconImages =
                        listOf(
                            OemIcon.Image(120, 120, "AAAA"),
                            OemIcon.Image(180, 180, "BBBB"),
                            OemIcon.Image(256, 256, "CCCC"),
                        ),
                    oemIconLabel = "Carlink",
                ),
            )
        assertTrue(
            "emitted document did not contain the expected oemIconConfig block:\n$doc",
            doc.contains(
                """
                oemIconConfig:
                  images:
                    - width: 120
                      height: 120
                      imageBase64: "AAAA"
                    - width: 180
                      height: 180
                      imageBase64: "BBBB"
                    - width: 256
                      height: 256
                      imageBase64: "CCCC"
                  label: "Carlink"
                  visible: true
                """.trimIndent() + "\n",
            ),
        )
    }

    /** It sits between limitedUIConfig and audio, matching the box's struct order. */
    @Test
    fun `oemIconConfig is placed between limitedUIConfig and audio`() {
        val doc =
            VehicleConfigYaml.render(
                VehicleConfigSpec(oemIconImages = listOf(OemIcon.Image(120, 120, "AAAA"))),
            )
        val limited = doc.indexOf("limitedUIConfig:")
        val oem = doc.indexOf("oemIconConfig:")
        val audio = doc.indexOf("audio:")
        assertTrue("all three sections present", limited >= 0 && oem >= 0 && audio >= 0)
        assertTrue("limitedUIConfig must precede oemIconConfig", limited < oem)
        assertTrue("oemIconConfig must precede audio", oem < audio)
    }

    @Test
    fun `a label with quotes or control characters is escaped like every other string`() {
        val doc =
            VehicleConfigYaml.render(
                VehicleConfigSpec(
                    oemIconImages = listOf(OemIcon.Image(120, 120, "AAAA")),
                    oemIconLabel = "Owner\u0007s \"Car\"",
                ),
            )
        // The BEL is stripped (Cc is rejected at the YAML stream level), the quote is escaped.
        assertTrue("label not escaped as expected:\n$doc", doc.contains("""  label: "Owners \"Car\""""))
    }
}
