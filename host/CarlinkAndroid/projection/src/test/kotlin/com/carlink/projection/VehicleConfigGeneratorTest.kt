package com.carlink.projection

import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import java.io.File
import java.nio.file.Files

/**
 * The generated config is the one artefact in this app whose mistakes are completely silent.
 *
 * serde ignores unknown keys, so a misspelled or mis-nested field costs the feature with no error
 * on either side — the box logs a successful parse and simply advertises the compiled default. That
 * makes the exact key names and nesting worth pinning in a test, because nothing else will catch a
 * regression.
 */
@RunWith(RobolectricTestRunner::class)
class VehicleConfigGeneratorTest {
    private fun gen() = VehicleConfigGenerator(ApplicationProvider.getApplicationContext())

    private val geometry =
        VehicleConfigGenerator.DisplayGeometry(
            widthPx = 1920,
            heightPx = 1080,
            fps = 60,
            densityDpi = 240,
        )

    @Test
    fun `pixel dimensions live under displayPanelsConfig, not the video stream`() {
        val yaml = gen().render(geometry)
        // The nesting Apple's schema actually uses, and which vehicle_config.rs's `apply()` reads:
        // display_panels_config.main_display_panel.pixel_dimensions.
        val panel = yaml.substringAfter("displayPanelsConfig:").substringBefore("videoStreamsConfig:")
        assertTrue("width missing under displayPanelsConfig", panel.contains("width: 1920"))
        assertTrue("height missing under displayPanelsConfig", panel.contains("height: 1080"))
    }

    @Test
    fun `hidConfig is nested inside mainVideoStream`() {
        val yaml = gen().render(geometry)
        // A top-level hidConfig parses as an unknown key and is silently dropped, so the indent
        // here is load-bearing. mainVideoStream's children are at 4 spaces; hidConfig's at 6.
        assertTrue(yaml.contains("  mainVideoStream:"))
        assertTrue(yaml.contains("    hidConfig:"))
        assertTrue(yaml.contains("      touchScreenSupportsMultiTouch: true"))
    }

    @Test
    fun `multi-touch uses the schema's own key name`() {
        val yaml = gen().render(geometry)
        // `multiTouch` would be silently ignored; the real key is touchScreenSupportsMultiTouch.
        assertTrue(yaml.contains("touchScreenSupportsMultiTouch: true"))
        assertFalse("bare 'multiTouch' is not a key vehicle_config.rs knows", yaml.contains("multiTouch:"))
    }

    @Test
    fun `HEVC is selected through accessoryConfig, not a codec list`() {
        val yaml = gen().render(geometry)
        assertTrue(yaml.contains("accessoryConfig:"))
        assertTrue(yaml.contains("  enablesHEVC: true"))
        assertFalse("there is no codecs list in the schema", yaml.contains("codecs:"))
    }

    @Test
    fun `no altVideoStreams are emitted`() {
        // AAOS already owns HDMI port 1 as its own instrument cluster (CarOccupantZoneService maps
        // port=1 to displayType=2). A CarPlay cluster stream would contend for it.
        val yaml = gen().render(geometry)
        assertFalse(yaml.contains("altVideoStreams"))
        assertFalse(yaml.contains("altDisplayPanels"))
    }

    @Test
    fun `apple's own misspelling of touchScreenMode is preserved`() {
        // "High Fidelty" is what Apple's templates and CarPlaySDK contain. Correcting it here would
        // produce a value the box does not recognise.
        assertTrue(gen().render(geometry).contains("touchScreenMode: \"High Fidelty\""))
    }

    @Test
    fun `limitedUIConfig is spelled with a capital UI`() {
        // The serde rename is `limitedUIConfig`. `limitedUiConfig` parses as an unknown key and is
        // silently dropped, so iOS falls back to its own default restriction set and nothing
        // anywhere reports a problem. This exact typo shipped and was caught only by the Rust-side
        // contract test.
        val yaml = gen().render(geometry)
        assertTrue(yaml.contains("limitedUIConfig:"))
        assertFalse(yaml.contains("limitedUiConfig:"))
    }

    @Test
    fun `unadvertised HID devices are declared false rather than omitted`() {
        val yaml = gen().render(geometry)
        assertTrue(yaml.contains("knobSupport: false"))
        assertTrue(yaml.contains("dPadSupport: false"))
    }

    @Test
    fun `fps is clamped to the two values CarPlay can negotiate`() {
        val g = gen()
        // vehicle_config.rs accepts ONLY 30 or 60; anything else falls through to the box default,
        // so an unclamped 50 Hz panel would silently advertise nothing.
        assertEquals(60, g.negotiableFps(60))
        assertEquals(60, g.negotiableFps(120))
        assertEquals(30, g.negotiableFps(30))
        assertEquals(30, g.negotiableFps(24))
        // Rounds DOWN, never to nearest: advertising 60 to a 50 Hz panel makes iOS encode frames we
        // then discard, which is worse than advertising 30 and presenting every one.
        assertEquals(30, g.negotiableFps(50))
    }

    @Test
    fun `physical size is derived from density rather than trusted from EDID`() {
        // 1920 px at 240 dpi = 8 inches = 203 mm. The bench display is an HDMI-to-USB capture
        // dongle whose EDID physical size is meaningless.
        assertEquals(203, geometry.widthMm)
        assertEquals(114, geometry.heightMm)
    }

    @Test
    fun `writeTo is atomic and skips an unchanged document`() {
        val dir = Files.createTempDirectory("vcfg").toFile()
        val target = File(dir, "carplay_cfg.yaml")
        val g = gen()
        val yaml = g.render(geometry)

        assertTrue("first write should report a change", g.writeTo(target, yaml))
        assertEquals(yaml, target.readText())

        // A rewrite of identical content must not touch the file: airplayd may read it at any
        // moment, and a needless rewrite is a needless chance to be read mid-write.
        assertFalse("unchanged content should not be rewritten", g.writeTo(target, yaml))

        // No temp file left behind.
        assertFalse(File(dir, "carplay_cfg.yaml.tmp").exists())
    }

    @Test
    fun `writeTo reports failure instead of throwing when the path is unwritable`() {
        // A config that cannot be written must degrade to "box uses compiled defaults", never take
        // down the service that is about to start the session.
        val g = gen()
        val unwritable = File("/proc/definitely/not/writable/carplay_cfg.yaml")
        assertFalse(g.writeTo(unwritable, g.render(geometry)))
    }
}
