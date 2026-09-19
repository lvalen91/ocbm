package com.carlink.callui

import org.json.JSONObject

/**
 * One iAP2 `CallStateUpdate` (0x4155) record as the box forwards it on `CH_METADATA`
 * (`{"kind":"callState", …}`, `iap2-core/src/metadata.rs` `call_state`). iOS emits one record per
 * ringing/active call; idle is a bare `status: 0`.
 *
 * Field values are Apple's, verbatim — no normalisation here so a future consumer can rely on the spec.
 */
data class CallState(
    /** `CallStatus`: 0 disconnected 1 sending 2 ringing 3 connecting 4 active 5 held 6 disconnecting. */
    val status: Int,
    /** 1 incoming 2 outgoing; 0 when iOS did not say. */
    val direction: Int,
    val displayName: String?,
    /** Phone number or handle. */
    val remoteId: String?,
    val callUuid: String?,
    /** Contact label ("mobile", "home"). */
    val label: String?,
    /** iOS's own start stamp when present. Units are Apple's; only use it relatively. */
    val startTimestamp: Long?,
) {
    /** A call the user can still answer. */
    val isRingingIncoming: Boolean get() = status == STATUS_RINGING && direction != DIRECTION_OUTGOING

    /** Any call that exists right now (dialling, ringing, connecting, active or held). */
    val isLive: Boolean get() = status in STATUS_SENDING..STATUS_HELD

    /** A call that is gone or going. */
    val isEnded: Boolean get() = status == STATUS_DISCONNECTED || status == STATUS_DISCONNECTING

    /** Stable identity for a call across its records: the UUID when iOS gives one, else the number. */
    val key: String get() = callUuid?.takeIf { it.isNotEmpty() } ?: remoteId?.takeIf { it.isNotEmpty() } ?: SINGLE_KEY

    /** Best display line: name, else number, else a placeholder — `CallStyle` refuses an empty name. */
    val displayLine: String get() = displayName?.takeIf { it.isNotBlank() } ?: remoteId?.takeIf { it.isNotBlank() } ?: "Unknown caller"

    companion object {
        const val STATUS_DISCONNECTED = 0
        const val STATUS_SENDING = 1
        const val STATUS_RINGING = 2
        const val STATUS_CONNECTING = 3
        const val STATUS_ACTIVE = 4
        const val STATUS_HELD = 5
        const val STATUS_DISCONNECTING = 6

        const val DIRECTION_UNKNOWN = 0
        const val DIRECTION_INCOMING = 1
        const val DIRECTION_OUTGOING = 2

        /** Key of a record with no identity at all — the idle "no calls" record. */
        internal const val SINGLE_KEY = "<no-identity>"

        /** Parse a `callState` record; null when it carries no status at all. */
        fun fromJson(o: JSONObject): CallState? {
            if (!o.has("status")) return null
            return CallState(
                status = o.optInt("status"),
                direction = o.optInt("direction", DIRECTION_UNKNOWN),
                displayName = o.optString("displayName").takeIf { it.isNotEmpty() },
                remoteId = o.optString("remoteId").takeIf { it.isNotEmpty() },
                callUuid = o.optString("callUuid").takeIf { it.isNotEmpty() },
                label = o.optString("label").takeIf { it.isNotEmpty() },
                startTimestamp = if (o.has("startTimestamp")) o.optLong("startTimestamp") else null,
            )
        }

        fun statusName(s: Int): String =
            when (s) {
                STATUS_DISCONNECTED -> "disconnected"
                STATUS_SENDING -> "sending"
                STATUS_RINGING -> "ringing"
                STATUS_CONNECTING -> "connecting"
                STATUS_ACTIVE -> "active"
                STATUS_HELD -> "held"
                STATUS_DISCONNECTING -> "disconnecting"
                else -> "status$s"
            }
    }
}

/**
 * The set of calls iOS currently reports, reduced to the one thing worth a notification.
 *
 * iOS sends per-call records with no "list" framing, so the roster keys them by [CallState.key] and
 * prunes ended ones. A bare idle record (`status 0`, no identity) clears everything — that is how
 * iOS says "no calls" after the last one drops.
 */
class CallRoster {
    sealed interface Presentation {
        data object None : Presentation

        data class Incoming(
            val call: CallState,
        ) : Presentation

        data class Ongoing(
            val call: CallState,
            /** `SystemClock.elapsedRealtime()`-style stamp of when this call was first seen live here. */
            val sinceMs: Long,
        ) : Presentation
    }

    private val calls = LinkedHashMap<String, CallState>()
    private val firstLiveAt = HashMap<String, Long>()

    val size: Int get() = calls.size

    fun update(
        s: CallState,
        nowMs: Long,
    ): Presentation {
        if (s.isEnded) {
            if (s.key == CallState.SINGLE_KEY) {
                calls.clear()
                firstLiveAt.clear()
            } else {
                calls.remove(s.key)
                firstLiveAt.remove(s.key)
            }
        } else {
            calls[s.key] = s
            if (s.isLive && !s.isRingingIncoming) firstLiveAt.getOrPut(s.key) { nowMs }
        }
        return present()
    }

    fun clear(): Presentation {
        calls.clear()
        firstLiveAt.clear()
        return Presentation.None
    }

    private fun present(): Presentation {
        // A ringing incoming call always wins: it is the one with a decision attached.
        calls.values.firstOrNull { it.isRingingIncoming }?.let { return Presentation.Incoming(it) }
        // Otherwise the most "active" one: active > connecting > sending > held.
        val live = calls.values.filter { it.isLive }
        val pick =
            live.firstOrNull { it.status == CallState.STATUS_ACTIVE }
                ?: live.firstOrNull { it.status == CallState.STATUS_CONNECTING }
                ?: live.firstOrNull { it.status == CallState.STATUS_SENDING }
                ?: live.firstOrNull()
                ?: return Presentation.None
        return Presentation.Ongoing(pick, firstLiveAt[pick.key] ?: 0L)
    }
}
