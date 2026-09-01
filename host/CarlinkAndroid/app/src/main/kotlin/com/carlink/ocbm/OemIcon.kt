package com.carlink.ocbm

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.util.Base64
import com.carlink.logging.Logger
import com.carlink.logging.logInfo
import com.carlink.logging.logWarn

/**
 * Renders this app's launcher icon into the PNG set CarPlay wants for `oemIconConfig`.
 *
 * The OEM icon is the tile on the CarPlay home screen that returns the driver to the vehicle's own
 * UI. Advertising it is only half the feature — the tap comes back as an inbound `requestUI`
 * command on `CH_METADATA`, which [com.carlink.CarlinkManager] decodes with [BinaryPlist].
 *
 * **All three sizes are mandatory.** Apple's own AppStub emits 120, 180 and 256
 * (`Examples/AppleCarPlay_AppStub.c:611-637`), and the box's notes record a device-confirmed
 * finding from 2026-08-02: iOS renders *only the label* for a single-size set. A partial set is
 * therefore not a smaller icon, it is no icon at all.
 */
object OemIcon {
    /** The exact sizes Apple's reference accessory publishes. Not a preference. */
    val SIZES = intArrayOf(120, 180, 256)

    /**
     * Bytes reserved for everything in the pushed config that is NOT the icon.
     *
     * The document is ~1.9 KB today, so this is a ~4x margin for future sections.
     */
    private const val NON_ICON_RESERVE = 8 * 1024

    /**
     * Ceiling on the total base64 the icon may contribute to the pushed config.
     *
     * DERIVED, not chosen: `CT_SUBSCRIBE` carries `[verb][yaml]` in ONE OCBM frame, so the whole
     * document must fit under [Ocbm.MAX_PAYLOAD]. A hand-picked ceiling gets this wrong in both
     * directions — the first value here was 48 KB, which rejected a perfectly deliverable 52 KB icon
     * set on real hardware. Anything that still exceeds this is a pathological icon (a photographic
     * PNG that will not compress), and dropping it beats failing the frame the config rides in.
     */
    const val MAX_TOTAL_BASE64 = Ocbm.MAX_PAYLOAD - NON_ICON_RESERVE

    /**
     * The dominant colour of [d], obtained by letting the rasteriser downsample it to a single
     * pixel. Cheaper and steadier than averaging the full bitmap, and it is exactly the "what colour
     * is this artwork" question being asked.
     */
    private fun dominantColor(d: android.graphics.drawable.Drawable): Int {
        val one = Bitmap.createBitmap(1, 1, Bitmap.Config.ARGB_8888)
        return try {
            d.setBounds(0, 0, 1, 1)
            d.draw(Canvas(one))
            one.getPixel(0, 0) or (0xFF shl 24) // force opaque: the tile is not composited over anything
        } finally {
            one.recycle()
        }
    }

    data class Image(
        val width: Int,
        val height: Int,
        val base64: String,
    )

    /**
     * Render the launcher icon at every size in [SIZES].
     *
     * Returns an empty list if the icon cannot be loaded or the set would exceed
     * [MAX_TOTAL_BASE64] — empty means "advertise no icon", which the emitter turns into an omitted
     * `oemIconConfig`, exactly what the box expects for "no icon".
     */
    fun render(context: Context): List<Image> {
        val icon =
            runCatching { context.packageManager.getApplicationIcon(context.packageName) }
                .getOrElse {
                    logWarn("[OEMICON] launcher icon unavailable: ${it.message}", tag = Logger.Tags.ADAPTR)
                    return emptyList()
                }

        // Flatten an adaptive icon to [solid colour + foreground] rather than compositing its real
        // background layer.
        //
        // This is a SIZE decision, measured on this app's own icon: the background layer is a
        // detailed image that does not compress, costing 87 KB of base64 at 256px alone — the three
        // required sizes came to 115,852 B against a 57,344 B budget, so the full-fidelity icon
        // simply cannot ride in one CT_SUBSCRIBE frame. The foreground layer is the logo and the
        // background is a backdrop, so filling with the backdrop's own dominant colour keeps the
        // mark and the palette while bringing the set to ~35 KB.
        val adaptive = icon as? android.graphics.drawable.AdaptiveIconDrawable
        val fill = adaptive?.background?.let { dominantColor(it) }
        val layer = adaptive?.foreground ?: icon

        val out = ArrayList<Image>(SIZES.size)
        var total = 0
        for (px in SIZES) {
            val bmp = Bitmap.createBitmap(px, px, Bitmap.Config.ARGB_8888)
            try {
                val canvas = Canvas(bmp)
                fill?.let(canvas::drawColor)
                layer.setBounds(0, 0, px, px)
                layer.draw(canvas)

                val png = java.io.ByteArrayOutputStream(px * px / 4)
                // PNG is lossless, so the quality argument is ignored — stated because passing 100
                // here reads like a JPEG setting and has been "optimised" away before.
                if (!bmp.compress(Bitmap.CompressFormat.PNG, 100, png)) {
                    logWarn("[OEMICON] PNG encode failed at ${px}px", tag = Logger.Tags.ADAPTR)
                    return emptyList()
                }
                val b64 = Base64.encodeToString(png.toByteArray(), Base64.NO_WRAP)
                total += b64.length
                if (total > MAX_TOTAL_BASE64) {
                    logWarn(
                        "[OEMICON] icon set is ${total}B of base64, over the ${MAX_TOTAL_BASE64}B budget — " +
                            "advertising no icon rather than risking an oversized CT_SUBSCRIBE",
                        tag = Logger.Tags.ADAPTR,
                    )
                    return emptyList()
                }
                out += Image(px, px, b64)
                logInfo("[OEMICON] ${px}px -> ${png.size()}B PNG, ${b64.length}B base64", tag = Logger.Tags.ADAPTR)
            } finally {
                bmp.recycle()
            }
        }
        logInfo("[OEMICON] rendered ${out.size} sizes, ${total}B base64 total", tag = Logger.Tags.ADAPTR)
        return out
    }
}
