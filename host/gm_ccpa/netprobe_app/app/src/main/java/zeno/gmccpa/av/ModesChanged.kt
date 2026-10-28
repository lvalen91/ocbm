package zeno.gmccpa.av

/**
 * The `modesChanged` decode, pure (plan v5.9 refactor item, 2026-10-08): the plist is already a
 * `Map` from [BPlist]; this pulls `params.appStates[]{appStateID, entity, speechMode}` out of it.
 * The rules are `CarPlayActivity.onModesChanged`'s, unchanged — see its KDoc for what `speechMode`
 * means (A11) and why 2/3 are only logged. `ModesChangedTest` runs it over the 35 frames of
 * `docs/ops/captures/2026-07-24_carplay_cmd_capture.bin`.
 */
internal object ModesChanged {
    /** `-1` speech mode = NotApplicable (the energy-gate behaviour); entities default to 0. */
    class Modes(val speechMode: Long, val speechEntity: Long, val phoneEntity: Long, val turnsEntity: Long)

    /** Null when [root] is not a modesChanged plist with a `params.appStates` list. */
    @Suppress("UNCHECKED_CAST")
    fun decode(root: Any?): Modes? {
        val params = (root as? Map<String, Any?>)?.get("params") as? Map<String, Any?> ?: return null
        val states = params["appStates"] as? List<Any?> ?: return null
        var speechMode = -1L; var speechEntity = 0L; var phoneEntity = 0L; var turnsEntity = 0L
        for (s in states) {
            val d = s as? Map<String, Any?> ?: continue
            val id = d["appStateID"] as? Long ?: continue
            val ent = d["entity"] as? Long ?: 0L
            when (id) {
                1L -> { speechEntity = ent; speechMode = d["speechMode"] as? Long ?: -1L }
                2L -> phoneEntity = ent
                3L -> turnsEntity = ent
            }
        }
        return Modes(speechMode, speechEntity, phoneEntity, turnsEntity)
    }
}
