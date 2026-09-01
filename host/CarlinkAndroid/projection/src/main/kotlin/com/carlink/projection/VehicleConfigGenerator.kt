package com.carlink.projection

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.hardware.display.DisplayManager
import android.util.Base64
import android.view.Display
import com.carlink.logging.ProbeLog
import java.io.ByteArrayOutputStream
import java.io.File

/**
 * Generates the CarPlay `VehicleConfig` from what AAOS actually reports about this head unit.
 *
 * ## Why this exists
 *
 * `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` makes CarPlay configuration **app-driven**: the accessory stack presents whatever config
 * the app pushes, and the compiled defaults are an interim floor rather than the design. Until now
 * nothing on the Pi produced one, so the stack ran on those compiled defaults — which means a
 * hardcoded resolution that happens to match the bench and would silently misdescribe any other
 * display.
 *
 * This closes that: read the active display at boot, emit the config, and let the stack advertise
 * what the vehicle really has.
 *
 * ## Ordering is load-bearing
 *
 * The config must be written **before anything arms**. Arming is first-arm-wins per process:
 * `airplayd` is spawned per session so it picks up a regenerated config on the next session, but a
 * long-lived process will not. Hence the boot sequence in `pi/docs/01_PROJECTION_APP_DESIGN.md` §5.1
 * — detect display, generate config, *then* bring up BT + Wi-Fi.
 *
 * ## What is deliberately not emitted
 *
 * **No `altVideoStreams`.** This AAOS build already reserves HDMI port 1 for its own instrument
 * cluster (`CarOccupantZoneService` maps `port=1` to `displayType=2`, `cluster_service` is enabled
 * by build config, and a renderer is already bound). A CarPlay cluster stream would contend for a
 * surface the platform owns. See §5.2.
 *
 * The corollary matters for the box side: `airplayd` refuses cluster commands when the alt screen is
 * not advertised, and logs why. That refusal is correct here, not a fault to work around.
 */
class VehicleConfigGenerator(
    private val ctx: Context,
) {
    private val log = ProbeLog.sub("vcfg")

    data class DisplayGeometry(
        val widthPx: Int,
        val heightPx: Int,
        val fps: Int,
        val densityDpi: Int,
    ) {
        /**
         * Physical size in millimetres — **DIAGNOSTIC ONLY. This is not emitted, and must not be.**
         *
         * The `VehicleConfig` schema has no physical-size field: `PixelDimensions` is
         * `{width, height}` and `DisplayPanelConfig` is
         * `{displayPanelID, pixelDimensions, displayProperties}`. Apple's own templates do carry
         * `physicalWidth`/`physicalHeight`, but on `viewAreas[]` entries, which `ViewAreaEntry` does
         * not parse. On the wire the box hardcodes `widthPhysical`/`heightPhysical` to **0** because
         * that is what the genuine wired main display sent — an invented 240x90 was there once and
         * was corrected. Emitting a derived value would need a box change AND would reverse a
         * device-confirmed correction.
         *
         * An earlier version of this comment claimed a wrong physical size makes iOS render controls
         * at the wrong scale. That was wrong: we send none at all.
         *
         * Kept because the number is the cheapest sanity check on `densityDpi` — a config that logs
         * a 12 mm or a 2 m panel has read a density the display is lying about, and the same metrics
         * feed touch normalisation.
         */
        val widthMm: Int get() = (widthPx.toDouble() / densityDpi * MM_PER_INCH).toInt()
        val heightMm: Int get() = (heightPx.toDouble() / densityDpi * MM_PER_INCH).toInt()

        private companion object {
            const val MM_PER_INCH = 25.4
        }
    }

    /**
     * Read the geometry of the display CarPlay will render to.
     *
     * Uses [Display.DEFAULT_DISPLAY] explicitly rather than "the largest" or "the first": on this
     * build display 0 is the main occupant-zone display (`displayType=1`), and display 1 — when
     * present — is the platform's instrument cluster, which we must not target.
     */
    fun readGeometry(): DisplayGeometry? {
        val dm = ctx.getSystemService(Context.DISPLAY_SERVICE) as? DisplayManager ?: return null
        val display = dm.getDisplay(Display.DEFAULT_DISPLAY) ?: return null
        val mode = display.mode
        val metrics = ctx.resources.displayMetrics
        val g =
            DisplayGeometry(
                widthPx = mode.physicalWidth,
                heightPx = mode.physicalHeight,
                // Rounded, not truncated: a panel reporting 59.94 is 60, and truncating to 59 would
                // advertise a frame rate iOS then paces to.
                fps = Math.round(display.refreshRate),
                densityDpi = metrics.densityDpi,
            )
        log.i("display: ${g.widthPx}x${g.heightPx}@${g.fps} ${g.densityDpi}dpi (${g.widthMm}x${g.heightMm} mm)")
        return g
    }

    /**
     * Render the config YAML.
     *
     * **Every key below was taken from `crates/vendor/receiver/src/vehicle_config.rs`'s serde
     * renames, not from Apple's documentation or from memory.** The nesting is not what it looks
     * like from the outside and is easy to get wrong in ways that fail quietly:
     *
     *  - pixel dimensions live under `displayPanelsConfig`, **not** under the video stream;
     *  - `hidConfig` is nested **inside** `mainVideoStream` — that is Apple's schema, and a
     *    top-level `hidConfig` parses as an unknown key and is silently dropped;
     *  - HEVC is `accessoryConfig.enablesHEVC`, not a codec list;
     *  - multi-touch is `touchScreenSupportsMultiTouch`, not `multiTouch`.
     *
     * serde ignores unknown keys, so a misspelling costs the feature with no error anywhere. Any
     * field we do not emit falls back to the box's compiled default, which is the intended layering
     * — so the rule here is emit what we mean to control and nothing else.
     */
    fun render(g: DisplayGeometry): String =
        buildString {
            appendLine("# GENERATED by com.carlink.projection at boot — do not edit by hand.")
            appendLine("# Source of truth is the live display; regenerate rather than patching.")
            appendLine("# Schema: crates/vendor/receiver/src/vehicle_config.rs (serde renames).")
            appendLine("name: \"AAOS Raspberry Pi (generated)\"")
            appendLine()
            appendLine("displayPanelsConfig:")
            appendLine("  mainDisplayPanel:")
            appendLine("    pixelDimensions:")
            appendLine("      width: ${g.widthPx}")
            appendLine("      height: ${g.heightPx}")
            // NO altDisplayPanels — this AAOS build already owns HDMI port 1 as its own instrument
            // cluster. See the class doc.
            appendLine()
            appendLine("videoStreamsConfig:")
            appendLine("  mainVideoStream:")
            appendLine("    maxFPS: ${negotiableFps(g.fps)}")
            appendLine("    hidConfig:")
            // Declared, not proven. AAOS reports touchscreen.multitouch.jazzhand, but this bench has
            // no touch hardware at all — /proc/bus/input/devices lists two HDMI audio jacks, a USB
            // mouse and EarPods. airplayd's two-contact path is real and will be exercised by these
            // declarations; the mouse can only drive it as a single pointer. §5.3.
            appendLine("      touchScreenMode: \"High Fidelty\"") // Apple's own spelling — do not "fix"
            appendLine("      touchScreenSupportsMultiTouch: true")
            appendLine("      telephonyButtonsSupport: true")
            appendLine("      mediaButtonsSupport: true")
            appendLine("      steeringWheelSupport: true")
            // Off deliberately. Nothing on this unit generates knob or D-Pad events, and airplayd
            // drops reports for an unadvertised device anyway — but advertising a HID device that
            // never reports has previously broken session reconnect in the sibling project, so the
            // safe direction is not to claim them.
            appendLine("      knobSupport: false")
            appendLine("      dPadSupport: false")
            appendLine()
            // The oemIcon is what lets the driver leave CarPlay for the AAOS home screen WITHOUT
            // ending projection — the other half of the launcher tile, and the mechanism §5.5
            // settled on. Rendered from the app's own launcher icon rather than shipped as an
            // asset, so there is one source of truth for the mark.
            val icons = renderOemIcons()
            if (icons.isNotEmpty()) {
                appendLine("oemIconConfig:")
                appendLine("  visible: true")
                appendLine("  label: \"CarLink\"")
                appendLine("  images:")
                for (i in icons) {
                    appendLine("    - width: ${i.size}")
                    appendLine("      height: ${i.size}")
                    // Block scalar: a base64 blob on one line is thousands of characters and some
                    // YAML readers fold long plain scalars. `!!str` is not needed; the quoted form
                    // keeps it unambiguous and is what the box's parser expects for a string field.
                    appendLine("      imageBase64: \"${i.base64}\"")
                }
                appendLine()
            }
            appendLine("accessoryConfig:")
            // HEVC only, per §1. This is the flag that actually selects it.
            appendLine("  enablesHEVC: true")
            // Focus transfer is CarPlay's own borrow-vs-take model, which is what makes a transient
            // nav prompt or call alert return the screen afterwards instead of keeping it. §5.6.
            appendLine("  enablesFocusTransfer: true")
            appendLine()
            // Which UI elements iOS restricts while limitedUI is active. Absent, iOS applies its own
            // default set — the runtime toggle works either way, and this only makes the SELECTION
            // explicit. The five below are the ones R14G17 defines and CT5 CINEMO corroborates.
            // `limitedUIConfig`, with a capital UI. `limitedUiConfig` parses as an unknown key and
            // is dropped in silence — caught only by the cross-language test in vehicle_config.rs.
            appendLine("limitedUIConfig:")
            appendLine("  softKeyboard: true")
            appendLine("  softPhoneKeypad: true")
            appendLine("  nonMusicLists: true")
            appendLine("  musicLists: true")
            appendLine("  longAlerts: true")
            // No `audio:` block. AudioFormatEntry is keyed by iAP2 stream type, not by codec name,
            // and the box's compiled default already negotiates AAC-LC media + AAC-ELD voice — which
            // is exactly what §1 asks for. Emitting a half-understood table here would replace a
            // working default with a worse one.
        }

    /** One rendered icon size and its base64 PNG. */
    data class OemIcon(
        val size: Int,
        val base64: String,
    )

    /**
     * Render the launcher icon at each size Apple's own AppStub emits.
     *
     * **All three sizes are required.** `vehicle_config.rs` records the device-confirmed behaviour:
     * iOS renders only the LABEL — no image at all — for a single-size set. Emitting one size looks
     * like a working config and produces a text-only tile on the phone.
     *
     * Rendering from the app's own drawable rather than shipping PNG assets keeps one source of
     * truth for the mark, and costs one bitmap per boot.
     */
    fun renderOemIcons(sizes: IntArray = OEM_ICON_SIZES): List<OemIcon> {
        val drawable =
            runCatching { ctx.packageManager.getApplicationIcon(ctx.packageName) }
                .onFailure { log.w("no launcher icon available — oemIcon omitted: $it") }
                .getOrNull() ?: return emptyList()
        val out = mutableListOf<OemIcon>()
        for (size in sizes) {
            val bmp = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
            try {
                val canvas = Canvas(bmp)
                drawable.setBounds(0, 0, size, size)
                drawable.draw(canvas)
                val bytes = ByteArrayOutputStream(size * size / 4)
                // PNG, not JPEG: the schema field is named imageBase64 and Apple's AppStub emits
                // PNG. Lossless also matters for a flat-colour mark, where JPEG ringing is visible.
                if (!bmp.compress(Bitmap.CompressFormat.PNG, 100, bytes)) {
                    // ALL THREE OR NONE. A partial set is not a partial feature: iOS renders a
                    // LABEL-ONLY tile for a single-size set, so `continue` here shipped a config
                    // that looks correct, parses correctly, and produces a text tile with nothing
                    // anywhere to say why.
                    log.e("oemIcon ${size}px failed to compress — omitting the WHOLE icon set")
                    return emptyList()
                }
                out += OemIcon(size, Base64.encodeToString(bytes.toByteArray(), Base64.NO_WRAP))
            } catch (e: Exception) {
                log.e("oemIcon ${size}px render failed: $e — omitting the WHOLE icon set")
                return emptyList()
            } finally {
                bmp.recycle()
            }
        }
        // Belt and braces: a caller passing a short `sizes` array, or a future edit that adds a
        // skip path, must not reach the emitter with a partial set either.
        if (out.size != sizes.size) {
            log.e("oemIcon produced ${out.size} of ${sizes.size} sizes — omitting the set entirely")
            return emptyList()
        }
        val bytes = out.sumOf { it.base64.length }
        // The box refuses a config over 256 KB outright rather than truncating, and the icons are
        // the only part that can grow without bound. Dropping them keeps the rest of the config
        // usable, which is strictly better than losing resolution and codec selection too.
        if (bytes > MAX_ICON_BASE64_BYTES) {
            log.w("oemIcon set is $bytes B base64 — over the ${MAX_ICON_BASE64_BYTES} B budget, omitting")
            return emptyList()
        }
        log.i("oemIcon: ${out.size} size(s), $bytes B base64")
        return out
    }

    /**
     * Clamp a measured refresh rate onto what CarPlay can actually negotiate.
     *
     * `vehicle_config.rs` accepts **only 30 or 60** — `CarPlayConfigs.FramesPerSecond` has exactly
     * those two cases, and anything else falls through and silently keeps the box default. So a
     * 50 Hz panel would advertise nothing at all unless it is mapped here.
     *
     * Rounds **down**, not to nearest: advertising 60 to a panel that cannot present 60 makes iOS
     * encode frames we then throw away — worse than advertising 30 and getting every frame. Only a
     * genuine 60 gets 60. (`readGeometry` already rounds 59.94 up to 60 before this sees it, so a
     * real 60 Hz panel is not caught by this.)
     */
    fun negotiableFps(measured: Int): Int = if (measured >= FPS_60_THRESHOLD) 60 else 30

    /**
     * Write the config where the accessory stack reads it, atomically.
     *
     * Atomic because `airplayd` may read this at any moment: it is spawned per session, and a
     * session starting mid-write would parse a truncated document. The box refuses an oversized or
     * empty config loudly rather than silently falling back, so a torn read is visible — but a
     * rename is cheaper than relying on that.
     *
     * Returns true if the file changed, so the caller can log a regeneration rather than a no-op.
     */
    fun writeTo(
        path: File,
        yaml: String,
    ): Boolean {
        val existing = runCatching { path.readText() }.getOrNull()
        if (existing == yaml) {
            log.i("config unchanged (${yaml.length} B) — not rewritten")
            return false
        }
        return try {
            path.parentFile?.mkdirs()
            val tmp = File(path.parentFile, "${path.name}.tmp")
            tmp.writeText(yaml)
            if (!tmp.renameTo(path)) {
                // renameTo across the same directory should not fail; if it does, a direct write is
                // better than no config at all.
                path.writeText(yaml)
                tmp.delete()
            }
            log.i("wrote ${path.absolutePath} (${yaml.length} B)")
            true
        } catch (e: Exception) {
            log.e("cannot write ${path.absolutePath}: $e — the box will use compiled defaults")
            false
        }
    }

    /**
     * Where this app can actually write the config.
     *
     * NOT `/tmp`. That is where `airplayd` looks by default, but an Android app process cannot write
     * it — the attempt fails with `EACCES` because `/tmp` is outside the app sandbox's writable set.
     * (Observed on device: `open failed: EACCES` on the first boot of this service.)
     *
     * So the app writes into its own data directory and `airplayd` is pointed at it with
     * `CARPLAY_CFG_FILE`. That works because `airplayd` runs as root on the Pi, and root reads
     * through the app-private mode bits.
     */
    fun configPath(): File = File(ctx.filesDir, "carplay_cfg.yaml")

    /** Generate and publish in one step. Returns the geometry used, or null if it could not be read. */
    fun generate(path: File = configPath()): DisplayGeometry? {
        val g = readGeometry()
        if (g == null) {
            log.e("no display available — config NOT generated, box will use compiled defaults")
            return null
        }
        writeTo(path, render(g))
        return g
    }

    companion object {
        /**
         * `airplayd`'s COMPILED-IN default location. Recorded for documentation only — this app
         * cannot write it (see [configPath]) and must launch `airplayd` with `CARPLAY_CFG_FILE`
         * pointing at the path it can.
         */
        const val BOX_DEFAULT_PATH = "/tmp/carplay_cfg.yaml"

        /** Only a genuine 60 Hz panel is advertised as 60; everything below rounds down to 30. */
        private const val FPS_60_THRESHOLD = 60

        /**
         * Apple's AppStub emits exactly these ("for each required size"). iOS falls back to a
         * label-only tile if the set has only one entry, so all three are emitted or none.
         */
        val OEM_ICON_SIZES = intArrayOf(120, 180, 256)

        /**
         * Budget for the base64 icon payload. airplayd refuses a config over 256 KB outright, and
         * the icons are the only unbounded part; 128 KB leaves ample room for everything else.
         */
        const val MAX_ICON_BASE64_BYTES = 128 * 1024
    }
}
