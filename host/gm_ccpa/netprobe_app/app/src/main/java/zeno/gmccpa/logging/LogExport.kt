package zeno.gmccpa.logging

import android.app.Activity
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.Handler
import android.os.Looper
import android.provider.MediaStore
import zeno.gmccpa.ProbeLog
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.OutputStream
import java.text.SimpleDateFormat
import java.util.Locale
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors

/**
 * Gets [LogCapture]'s rotated files off the head unit and onto a USB stick, because `run-as` is
 * blocked here (see the `MainActivity` class KDoc) and this app cannot be pulled with `adb pull` either — export
 * is the ONLY retrieval path for an untethered drive capture.
 *
 * ## The ladder
 * Four rungs, tried in order, first viable one wins. "Viable" is judged at launch time (can this
 * rung even be attempted), not at outcome time — see [Options.cascadeOnSafCancel] for the one place
 * that distinction is a real decision. A rung that starts writing and then fails partway (disk full,
 * SecurityException mid-stream) cascades to the next rung rather than surfacing a failure the
 * operator cannot act on in a vehicle with no laptop.
 *
 * 1. [Rung.SAF] — `ACTION_CREATE_DOCUMENT`. Probed with `PackageManager.resolveActivity` first, so
 *    an AAOS build shipped without DocumentsUI (common — driver-distraction policy strips the
 *    picker on some units) degrades cleanly instead of throwing `ActivityNotFoundException`. Needs
 *    an [Activity] because there is no Compose `rememberLauncherForActivityResult` in this Views
 *    app; the caller's `onActivityResult` must forward into [onActivityResult].
 *
 *    **Manifest dependency this file does NOT own**: `targetSdk 32` means package-visibility
 *    filtering applies to `resolveActivity` too, so `AndroidManifest.xml` carries a `<queries>`
 *    block for `ACTION_CREATE_DOCUMENT` (mimeType `*&#47;*`). Without it the probe would report
 *    "not available" even with DocumentsUI present and this rung would be skipped one step early —
 *    never a crash. (Corrected 2026-10-08, E-DOC 9: this used to say the block was still to be added.)
 *
 * 2. [Rung.USB_VOLUME] — `Context.getExternalFilesDirs(null)`. The workhorse: no picker, no runtime
 *    permission, and the multi-element form's entries past index 0 are OEM-mounted removable
 *    volumes. Index 0 is *not* skipped by position — it is skipped because `StorageManager
 *    .getStorageVolume(dir)?.isRemovable` is false for it on every real device, and position is not
 *    a contract Android makes. A volume only qualifies if it is both `isRemovable` AND currently
 *    `MEDIA_MOUNTED`; an unmounted or ejected slot's directory entry is skipped, not treated as a
 *    write target. The path landed on is still an app-sandboxed `Android/data/.../files` directory
 *    on the stick — that sandbox is a Scoped-Storage rule enforced by *this device*, not by the
 *    computer the stick is later read on, where it is an ordinary folder on an ordinary FAT/exFAT
 *    filesystem.
 *
 * 3. [Rung.MEDIA_STORE] — `MediaStore.Downloads`, requires API 29+. Since minSdk 32 (OD-4,
 *    2026-10-06; it was 26) every install has it; the SDK_INT check below is kept as a harmless
 *    guard, and the rung would report itself unavailable rather than crash on a missing collection.
 *
 * 4. [Rung.FILES_DIR] — a no-op. Reports [LogCapture.logsDir] as the destination and the sum of the
 *    existing file sizes as `bytesWritten`, but **copies nothing and redacts nothing** — the files
 *    it is pointing at are exactly what [LogCapture] wrote, unredacted, sitting in `filesDir` for a
 *    later tethered pull by whatever finally gets `run-as` or root working. Do not mistake a
 *    [Rung.FILES_DIR] result for an exported artifact; it is a shrug with a path attached.
 *
 * ## One artifact, not a picker-per-file
 * Rungs 1-3 concatenate every file from [LogCapture.logFiles] (sorted by name: every `netprobe-os-*`
 * file, oldest first, then every `netprobe-own-*` file, oldest first — the scope prefix sorts before
 * the timestamp, so the two scopes are NOT interleaved chronologically; corrected 2026-10-08,
 * E-DOC 8) into a single stream, each source file's start marked with a plain-text separator line so
 * provenance survives concatenation. Plain text rather than a zip: these files are read by grepping
 * them in a parking lot with whatever is at hand, and log text does not compress enough over
 * already-line-oriented content to be worth losing that.
 *
 * ## Redaction — mandatory by default, and its exact boundaries
 * See [Redactor] for the categories, the regexes, and — just as important — what is deliberately
 * NOT caught. [Options.redact] `= false` is the explicit opt-out for an operator who has decided
 * they want the raw stream; the default is redact-on, because a USB stick is a durable artifact
 * that leaves the vehicle the moment it is pulled.
 *
 * ## API shape
 * Callback-based, not suspend: this module ships with zero dependencies (`build.gradle` — "so the
 * APK stays tiny... no transitive AndroidX/version friction"), and kotlinx-coroutines is not one of
 * them. [export] never blocks the calling thread; all I/O runs on a private single-thread executor
 * and the result is delivered on the main looper so a menu-item caller can update its own UI
 * directly. It needs no Activity subclass and makes no lifecycle assumption beyond "the Activity
 * that receives the SAF picker's `onActivityResult` is still around to forward it" — exactly the
 * same assumption `startActivityForResult` itself makes.
 */
object LogExport {

    /** Which rung of the ladder actually produced (or, for [FILES_DIR], pointed at) the artifact. */
    enum class Rung { SAF, USB_VOLUME, MEDIA_STORE, FILES_DIR }

    // The redaction categories ([RedactionCategory]) and the rules ([Redactor]) live in Redactor.kt
    // since plan v5.8 V7: no android.* there, so the rules run under the tier-0 JVM tests.

    /**
     * @param redact on by default; `false` is the operator's explicit opt-out for a raw export.
     * @param fileName base name for the concatenated artifact (rungs 1-3 only); a `.log` extension
     *   is appended if missing. Defaults to a timestamped name so repeat exports never collide.
     * @param cascadeOnSafCancel when the operator backs out of the SAF picker (`RESULT_CANCELED` or
     *   a null Uri), `true` (default) falls through to [Rung.USB_VOLUME] etc. so the menu action
     *   still lands an artifact somewhere without a second tap — appropriate for "no laptop
     *   present." Set `false` if a cancel should be treated as the operator declining to export at
     *   all, e.g. because they backed out to free space first and a silent fallback would surprise
     *   them.
     */
    data class Options(
        val redact: Boolean = true,
        val fileName: String = defaultFileName(),
        val cascadeOnSafCancel: Boolean = true,
    )

    /**
     * @param destination absolute file path ([Rung.USB_VOLUME], [Rung.FILES_DIR]) or content URI
     *   string ([Rung.SAF], [Rung.MEDIA_STORE]).
     * @param bytesWritten bytes actually written to [destination]; for [Rung.FILES_DIR] this is the
     *   sum of the existing (untouched) file sizes, not bytes newly written — see the class KDoc.
     * @param filesIncluded how many rotated [LogCapture] files were concatenated (or, for
     *   [Rung.FILES_DIR], are present).
     * @param redactionCounts per-category counts of DISTINCT values redacted (a value seen twice is
     *   one placeholder and one count — `Redactor.placeholderFor`), so the operator can sanity-check
     *   redaction actually fired; empty when [Options.redact] is false or the rung is [FILES_DIR].
     */
    data class ExportResult(
        val rung: Rung,
        val destination: String,
        val bytesWritten: Long,
        val filesIncluded: Int,
        val redactionCounts: Map<RedactionCategory, Long>,
    )

    /** Default request code for the SAF leg. Callers with their own request-code space should pass one explicitly to both [export] and [onActivityResult]. */
    const val REQUEST_CODE = 0x6C67 // "lg" - arbitrary but memorable, unlikely to collide

    private val log = ProbeLog.sub("lexp")
    private val mainHandler = Handler(Looper.getMainLooper())
    private val ioExecutor = Executors.newSingleThreadExecutor { r -> Thread(r, "logexport-io").apply { isDaemon = true } }

    /** SAF launches keyed by request code; consumed exactly once by [onActivityResult]. */
    private class Pending(val context: Context, val options: Options, val callback: (Result<ExportResult>) -> Unit)
    private val pending = ConcurrentHashMap<Int, Pending>()

    /**
     * Entry point for both a menu item and a scripted `am start --es` verb — both run inside an
     * Activity, so both can supply one. Pass `activity = null` (or omit it, from a non-Activity
     * caller) to skip straight to rung 2; the result is identical to what a SAF probe failure would
     * have produced, just without the wasted probe.
     *
     * Never blocks: returns immediately in every case. When rung 1 is attempted, [callback] fires
     * later from [onActivityResult]; otherwise it fires as soon as the background I/O finishes.
     * Always delivered on the main looper.
     */
    fun export(
        context: Context,
        activity: Activity?,
        options: Options = Options(),
        requestCode: Int = REQUEST_CODE,
        callback: (Result<ExportResult>) -> Unit,
    ) {
        val appCtx = context.applicationContext
        if (activity != null && trySaf(appCtx, activity, options, requestCode, callback)) return
        ioExecutor.execute { deliver(callback, runFallbackLadder(appCtx, options)) }
    }

    /**
     * Forward from the owning Activity's `onActivityResult`. Returns `true` if `requestCode`
     * belonged to a pending export (whether or not it was ultimately handled here) so the caller
     * knows not to also treat it as its own. A cancelled or empty picker result cascades to the
     * fallback ladder iff [Options.cascadeOnSafCancel]; otherwise it is reported as a failure.
     */
    fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?): Boolean {
        val p = pending.remove(requestCode) ?: return false
        val uri = if (resultCode == Activity.RESULT_OK) data?.data else null
        if (uri == null) {
            if (p.options.cascadeOnSafCancel) {
                log.i("SAF cancelled/empty — falling back to rung 2+")
                ioExecutor.execute { deliver(p.callback, runFallbackLadder(p.context, p.options)) }
            } else {
                deliver(p.callback, Result.failure(IOException("SAF export cancelled by operator")))
            }
            return true
        }
        ioExecutor.execute {
            deliver(p.callback, runSafWrite(p.context, uri, p.options))
        }
        return true
    }

    private fun deliver(callback: (Result<ExportResult>) -> Unit, result: Result<ExportResult>) {
        mainHandler.post { callback(result) }
    }

    // --- rung 1: SAF ---------------------------------------------------------------------------

    /** True if the launch was accepted (result now pending on [onActivityResult]); false to fall through. */
    private fun trySaf(
        context: Context,
        activity: Activity,
        options: Options,
        requestCode: Int,
        callback: (Result<ExportResult>) -> Unit,
    ): Boolean {
        val probe = Intent(Intent.ACTION_CREATE_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("text/plain")
        val resolved = try {
            context.packageManager.resolveActivity(probe, PackageManager.MATCH_DEFAULT_ONLY)
        } catch (e: SecurityException) {
            null // package-visibility denial reads as "unavailable", not a crash
        }
        if (resolved == null) {
            log.i("SAF unavailable (no resolver for ACTION_CREATE_DOCUMENT) — trying rung 2")
            return false
        }
        val launch = Intent(Intent.ACTION_CREATE_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE)
            .setType("text/plain").putExtra(Intent.EXTRA_TITLE, artifactName(options))
        // One picker per request code (E-F6). A second export while the first picker is still up
        // used to overwrite the pending entry: the first caller never heard back, and the picker's
        // result was then written with the SECOND call's options (a raw/redacted mix-up). Refuse the
        // newcomer instead; `true` because the request was consumed here, not handed to the ladder.
        if (pending.putIfAbsent(requestCode, Pending(context, options, callback)) != null) {
            log.w("export refused: a SAF export is already pending for requestCode=$requestCode")
            deliver(callback, Result.failure(IOException("export already pending (requestCode=$requestCode)")))
            return true
        }
        return try {
            activity.startActivityForResult(launch, requestCode)
            log.i("SAF picker launched (requestCode=$requestCode)")
            true
        } catch (e: android.content.ActivityNotFoundException) {
            // Race with the resolveActivity probe, or an OEM resolver that lies. Same fallback
            // path as an absent resolver.
            pending.remove(requestCode)
            log.w("SAF resolved but launch threw ActivityNotFoundException — trying rung 2")
            false
        }
    }

    private fun runSafWrite(context: Context, uri: Uri, options: Options): Result<ExportResult> = try {
        val out = context.contentResolver.openOutputStream(uri)
            ?: return Result.failure(IOException("openOutputStream returned null for $uri"))
        val (bytes, files, counts) = writeArtifact(context, out, options)
        Result.success(ExportResult(Rung.SAF, uri.toString(), bytes, files, counts))
    } catch (e: IOException) {
        log.w("SAF write failed: ${e.message} — falling back to ladder")
        runFallbackLadder(context, options)
    } catch (e: SecurityException) {
        log.w("SAF write denied: ${e.message} — falling back to ladder")
        runFallbackLadder(context, options)
    }

    // --- rungs 2-4: the synchronous fallback ladder ---------------------------------------------

    /** Runs on [ioExecutor]. Tries USB volume, then MediaStore, then the filesDir no-op; the last rung cannot fail short of the log directory itself being unreadable. */
    private fun runFallbackLadder(context: Context, options: Options): Result<ExportResult> {
        usbVolumeDir(context)?.let { dir ->
            try {
                val dest = File(dir, artifactName(options))
                FileOutputStream(dest).use { out ->
                    val (bytes, files, counts) = writeArtifact(context, out, options)
                    log.i("exported via USB_VOLUME -> ${dest.absolutePath} ($bytes bytes, $files files)")
                    return Result.success(ExportResult(Rung.USB_VOLUME, dest.absolutePath, bytes, files, counts))
                }
            } catch (e: IOException) {
                log.w("USB_VOLUME write failed: ${e.message} — trying MediaStore")
            } catch (e: SecurityException) {
                log.w("USB_VOLUME write denied: ${e.message} — trying MediaStore")
            }
        }

        if (Build.VERSION.SDK_INT >= 29) {
            try {
                val values = ContentValues().apply {
                    put(MediaStore.Downloads.DISPLAY_NAME, artifactName(options))
                    put(MediaStore.Downloads.MIME_TYPE, "text/plain")
                    put(MediaStore.Downloads.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS)
                }
                val resolver = context.contentResolver
                val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
                if (uri != null) {
                    resolver.openOutputStream(uri)?.use { out ->
                        val (bytes, files, counts) = writeArtifact(context, out, options)
                        log.i("exported via MEDIA_STORE -> $uri ($bytes bytes, $files files)")
                        return Result.success(ExportResult(Rung.MEDIA_STORE, uri.toString(), bytes, files, counts))
                    }
                }
            } catch (e: IOException) {
                log.w("MEDIA_STORE write failed: ${e.message} — falling back to filesDir")
            } catch (e: SecurityException) {
                log.w("MEDIA_STORE write denied: ${e.message} — falling back to filesDir")
            }
        } else {
            log.i("MEDIA_STORE rung needs API 29+ (running ${Build.VERSION.SDK_INT}) — skipped")
        }

        // Rung 4: no-op. Always "succeeds" because it promises nothing more than a path.
        val files = LogCapture.logFiles(context)
        val totalBytes = files.sumOf { it.length() }
        log.i("no exportable sink — reporting filesDir path for a later tethered pull: ${LogCapture.logsDir(context)}")
        return Result.success(
            ExportResult(Rung.FILES_DIR, LogCapture.logsDir(context).absolutePath, totalBytes, files.size, emptyMap())
        )
    }

    /**
     * Finds a removable, mounted external-files-dir volume — i.e. an actually-inserted USB stick,
     * not just `getExternalFilesDirs()[1]` existing as a stale/unmounted entry. `dir` itself may be
     * null (a slot with nothing inserted reports as such in the array) and is skipped along with it.
     */
    private fun usbVolumeDir(context: Context): File? {
        val sm = context.getSystemService(Context.STORAGE_SERVICE) as? android.os.storage.StorageManager
            ?: return null
        for (dir in context.getExternalFilesDirs(null)) {
            if (dir == null) continue
            val removable = try {
                sm.getStorageVolume(dir)?.isRemovable == true
            } catch (e: IllegalArgumentException) {
                false // dir not associated with a known volume
            }
            if (!removable) continue
            val mounted = Environment.getExternalStorageState(dir) == Environment.MEDIA_MOUNTED
            if (!mounted) continue
            if (!dir.exists() && !dir.mkdirs()) continue
            return dir
        }
        return null
    }

    // --- the shared writer: concatenate, redact, count ------------------------------------------

    private data class WriteOutcome(val bytesWritten: Long, val filesIncluded: Int, val counts: Map<RedactionCategory, Long>)

    /** Counts bytes as they reach the sink so the caller gets an exact total without re-encoding every line. */
    private class CountingOutputStream(out: OutputStream) : java.io.FilterOutputStream(out) {
        var count = 0L; private set
        override fun write(b: Int) { out.write(b); count++ }
        // FilterOutputStream's default forwards this one byte at a time; override so the buffered writer's
        // 8 KB flushes go through as one write(2).
        override fun write(b: ByteArray, off: Int, len: Int) { out.write(b, off, len); count += len }
    }

    /**
     * Streams every [LogCapture.logFiles] file into [out], oldest first, one line at a time —
     * mandatory, because these files run to hundreds of MB and reading one whole into memory would
     * be the difference between an export and an OOM on an automotive head unit. Closes [out] when
     * done (or on failure) via `use`.
     */
    private fun writeArtifact(context: Context, out: OutputStream, options: Options): WriteOutcome {
        val files = LogCapture.logFiles(context)
        val redactor = if (options.redact) Redactor() else null
        val counting = CountingOutputStream(out)
        counting.use { sink ->
            val writer = sink.bufferedWriter(Charsets.UTF_8)
            for (f in files) {
                writer.write("##### EXPORT: ${f.name} (${f.length()} bytes) #####\n")
                f.bufferedReader(Charsets.UTF_8).use { r ->
                    while (true) {
                        val line = r.readLine() ?: break
                        writer.write(redactor?.redactLine(line) ?: line)
                        writer.write("\n")
                    }
                }
            }
            if (redactor != null) writer.write(redactor.summaryLine())
            writer.flush()
        }
        return WriteOutcome(counting.count, files.size, redactor?.counts() ?: emptyMap())
    }

    private fun artifactName(options: Options): String =
        if (options.fileName.endsWith(".log")) options.fileName else "${options.fileName}.log"

    private fun defaultFileName(): String =
        "netprobe-export-${SimpleDateFormat("yyyyMMdd-HHmmss", Locale.US).format(java.util.Date())}"
}

