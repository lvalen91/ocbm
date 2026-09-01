package com.carlink.device

import com.carlink.CarlinkManager.DeviceInfo
import org.json.JSONArray
import org.json.JSONObject

/**
 * One phone the app has seen, remembered across restarts.
 *
 * Keyed by the BR/EDR MAC because that is the only identifier both sides share: `MGMT_INFO.devices`
 * is a bare MAC array read out of the box's 25-byte link-key records, and `CT_PHONE_IDENT.deviceID`
 * is the same MAC. Everything else here comes from the phone's own AirPlay SETUP plist and exists
 * nowhere on the box — the link-key record has no name, no timestamp and no device class, so if the
 * app does not remember it, nobody does.
 */
data class KnownDevice(
    /** Lower-cased BR/EDR MAC. The primary key everywhere in this package. */
    val mac: String,
    /** As set in Settings > General > About > Name. Null until the phone has been in session once. */
    val name: String? = null,
    val model: String? = null,
    val osName: String? = null,
    val osVersion: String? = null,
    /** Wall clock of first sighting. Wall clock, not elapsedRealtime — it must survive reboots. */
    val firstSeenMs: Long,
    /** Wall clock of the last `CT_PHONE_IDENT` for this MAC. Null if never connected. */
    val lastConnectedMs: Long? = null,
)

/** Everything the store persists: the history plus which device the user picked. */
data class KnownDeviceSnapshot(
    val devices: Map<String, KnownDevice> = emptyMap(),
    /**
     * User's explicit choice, or null for "follow most-recently-connected".
     *
     * A single top-level field rather than a per-device flag: that structurally guarantees "at most
     * one preferred" instead of leaving an invariant to police on every mutation.
     */
    val preferredMac: String? = null,
)

/**
 * JSON form of [KnownDeviceSnapshot]. Separated from the store so it is testable without a Context.
 *
 * This is a compatibility surface — a document written by one app version is read by the next — so
 * it carries a schema version and [decode] never throws. A document it cannot understand yields an
 * empty snapshot: the box's bond list reseeds the MACs on the next poll, which is strictly better
 * than crashing on a file the user cannot reach to delete.
 */
object KnownDeviceCodec {
    const val SCHEMA_VERSION = 1

    /**
     * Emit the document with an explicit, stable key order.
     *
     * NOT `JSONObject.toString()`: its key order is implementation-defined — AOSP backs it with a
     * LinkedHashMap, the reference org.json with a HashMap — so the same snapshot serialises to
     * different bytes on device than under test. Escaping is still delegated to [JSONObject.quote]
     * rather than hand-rolled, since that is where the real bugs live (device names are typed by the
     * user and routinely contain quotes, backslashes and emoji).
     *
     * Devices are sorted by MAC so an unordered map cannot produce a different document each write.
     */
    fun encode(s: KnownDeviceSnapshot): String =
        buildString {
            append("""{"v":""").append(SCHEMA_VERSION)
            s.preferredMac?.let { append(""","preferredMac":""").append(JSONObject.quote(it)) }
            append(""","devices":[""")
            s.devices.values.sortedBy { it.mac }.forEachIndexed { i, d ->
                if (i > 0) append(',')
                append("""{"mac":""").append(JSONObject.quote(d.mac))
                d.name?.let { append(""","name":""").append(JSONObject.quote(it)) }
                d.model?.let { append(""","model":""").append(JSONObject.quote(it)) }
                d.osName?.let { append(""","osName":""").append(JSONObject.quote(it)) }
                d.osVersion?.let { append(""","osVersion":""").append(JSONObject.quote(it)) }
                append(""","firstSeenMs":""").append(d.firstSeenMs)
                d.lastConnectedMs?.let { append(""","lastConnectedMs":""").append(it) }
                append('}')
            }
            append("]}")
        }

    fun decode(json: String?): KnownDeviceSnapshot {
        if (json.isNullOrBlank()) return KnownDeviceSnapshot()
        val root = runCatching { JSONObject(json) }.getOrNull() ?: return KnownDeviceSnapshot()
        if (root.optInt("v", -1) != SCHEMA_VERSION) return KnownDeviceSnapshot()

        val arr = root.optJSONArray("devices") ?: JSONArray()
        val out =
            (0 until arr.length())
                .mapNotNull { i -> arr.optJSONObject(i) }
                .mapNotNull { o ->
                    // A record with no MAC cannot be joined against anything, so it is not a record.
                    val mac = o.optString("mac").lowercase().ifEmpty { return@mapNotNull null }
                    mac to
                        KnownDevice(
                            mac = mac,
                            name = o.optString("name").ifEmpty { null },
                            model = o.optString("model").ifEmpty { null },
                            osName = o.optString("osName").ifEmpty { null },
                            osVersion = o.optString("osVersion").ifEmpty { null },
                            firstSeenMs = o.optLong("firstSeenMs", 0L),
                            lastConnectedMs = o.optLong("lastConnectedMs", 0L).takeIf { it > 0L },
                        )
                }.toMap(LinkedHashMap())
        return KnownDeviceSnapshot(
            devices = out,
            preferredMac = root.optString("preferredMac").lowercase().ifEmpty { null },
        )
    }
}

/**
 * Build the display list from remembered history and the box's current bond list.
 *
 * Pure, and takes every input explicitly, so the merge rules are testable without a Context, a box,
 * or a clock. The caller supplies [suppressed] (recently-forgotten MACs) because that state belongs
 * to the manager's lock, and [formatLastSeen] because formatting needs a locale.
 *
 * The two directions are NOT symmetric, deliberately:
 *
 *  - **In the bond list, not in history** — a real device the app has never named (fresh install
 *    over existing bonds). Shown under its MAC, and the caller is expected to learn it.
 *  - **In history, not in the bond list** — the user's requirement is history "until cleared", so
 *    the entry stays, but marked `bonded = false`. It must never render as connectable: the box
 *    cannot page a phone it has no link key for, and offering Connect there is a lie the user
 *    discovers by tapping it.
 *
 * Ordering collapses all three phrasings of the request into one precedence — explicit selection,
 * else most-recently-connected, else first-seen — so with one phone it is "the first device", with
 * a choice made it is "the device selected", and otherwise it follows recency with no configuration.
 */
fun mergeDeviceList(
    known: Map<String, KnownDevice>,
    bondedMacs: Set<String>,
    suppressed: Set<String>,
    preferredMac: String?,
    formatLastSeen: (Long) -> String,
): List<DeviceInfo> {
    val bonded = bondedMacs.map { it.lowercase() }.toSet()
    val hidden = suppressed.map { it.lowercase() }.toSet()
    val macs = (known.keys + bonded).filterNot { it in hidden }

    return macs
        .map { mac ->
            val k = known[mac]
            DeviceInfo(
                btMac = mac,
                name = k?.name ?: mac,
                type = "CarPlay",
                lastConnected = k?.lastConnectedMs?.let(formatLastSeen),
                bonded = mac in bonded,
            )
        }.sortedWith(
            compareBy(
                { if (it.btMac == preferredMac?.lowercase()) 0 else 1 },
                { -(known[it.btMac]?.lastConnectedMs ?: 0L) },
                { known[it.btMac]?.firstSeenMs ?: Long.MAX_VALUE },
                { it.btMac },
            ),
        )
}
