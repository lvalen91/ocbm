package com.carlink.projection

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * Observable session state, shared by the service, the projection surface, the device screen and
 * the launcher tile.
 *
 * ## The three-state model the UI is built on
 *
 * `pi/docs/01_PROJECTION_APP_DESIGN.md` §1 settled that the tile brings projection forward rather
 * than restarting it, and that means the session and the foreground are **independent**:
 *
 *  - no session          → the device screen offers connect / add device
 *  - session, background → the tile does `moveTaskToFront`; audio keeps playing
 *  - session, foreground → the tile is a no-op
 *
 * Collapsing those into one boolean is what produces the classic head-unit bug where returning to
 * projection tears down and re-establishes the session, costing 10-20 s and a re-pair.
 */
class SessionStatus {
    /**
     * How far along the accessory stack is. Deliberately finer-grained than "connected", because
     * every one of these has been an observed stopping point on real hardware and they need
     * different fixes.
     */
    enum class Phase {
        /** No accessory stack is producing anything. */
        IDLE,

        /** A seam producer connected, so `airplayd` exists and a session is being set up. */
        LINKING,

        /** The per-stream key arrived — a paired, keyed CarPlay session exists. */
        KEYED,

        /** Frames are being decoded to a surface. */
        STREAMING,
    }

    data class State(
        val phase: Phase = Phase.IDLE,
        val videoSeamUp: Boolean = false,
        val audioSeamUp: Boolean = false,
        val keyed: Boolean = false,
        val rendering: Boolean = false,
        /** True while the projection activity is resumed and visible. */
        val foreground: Boolean = false,
        /** True while Siri or a call owns the voice sink. */
        val assistantActive: Boolean = false,
        /** True while `CarUxRestrictions` says the vehicle is in a restricted (driving) state. */
        val driveRestricted: Boolean = false,
        /**
         * True when the negotiated video codec is HEVC, false for H.264, null before the first
         * codec config. Surfaced because an H.264 stream against the HEVC-only decoder renders
         * nothing while every other indicator stays green.
         */
        val hevcNegotiated: Boolean? = null,
        /** Address of the phone the accessory stack reports as connected, if any. */
        val deviceAddress: String? = null,
        val deviceName: String? = null,
    ) {
        /**
         * A session exists in any useful sense. Note this is true while backgrounded — that is the
         * whole point of separating it from [foreground].
         */
        val sessionActive: Boolean get() = phase == Phase.KEYED || phase == Phase.STREAMING
    }

    private val _state = MutableStateFlow(State())
    val state: StateFlow<State> = _state.asStateFlow()

    fun setVideoSeamUp(up: Boolean) = _state.update { recompute(it.copy(videoSeamUp = up)) }

    fun setAudioSeamUp(up: Boolean) = _state.update { recompute(it.copy(audioSeamUp = up)) }

    fun setKeyed(k: Boolean) = _state.update { recompute(it.copy(keyed = k)) }

    fun setRendering(r: Boolean) = _state.update { recompute(it.copy(rendering = r)) }

    fun setHevcNegotiated(h: Boolean) = _state.update { it.copy(hevcNegotiated = h) }

    fun setForeground(f: Boolean) = _state.update { it.copy(foreground = f) }

    fun setAssistantActive(a: Boolean) = _state.update { it.copy(assistantActive = a) }

    fun setDriveRestricted(d: Boolean) = _state.update { it.copy(driveRestricted = d) }

    fun setDevice(
        address: String?,
        name: String?,
    ) = _state.update { it.copy(deviceAddress = address, deviceName = name) }

    /**
     * Reset everything the accessory stack owns, keeping UI-owned fields.
     *
     * [setForeground] is not touched: the activity's visibility is a fact about our own process and
     * is unaffected by a session ending. Clearing it here would make the tile think projection is
     * backgrounded while it is on screen showing the disconnected state.
     */
    fun sessionEnded() =
        _state.update {
            it.copy(
                phase = Phase.IDLE,
                videoSeamUp = false,
                audioSeamUp = false,
                keyed = false,
                rendering = false,
                assistantActive = false,
                hevcNegotiated = null,
                deviceAddress = null,
                deviceName = null,
            )
        }

    private fun recompute(s: State): State {
        val phase =
            when {
                s.keyed && s.rendering -> Phase.STREAMING
                s.keyed -> Phase.KEYED
                s.videoSeamUp || s.audioSeamUp -> Phase.LINKING
                else -> Phase.IDLE
            }
        return s.copy(phase = phase)
    }
}
