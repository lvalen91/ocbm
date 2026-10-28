package zeno.gmccpa.av

/**
 * The two PendingIntents that open MainActivity, kept distinguishable (D-F1, 2026-10-08).
 *
 * `PendingIntent.getActivity` keys on (requestCode, Intent.filterEquals): extras are NOT part of
 * the key. Both the status notification ([CarPlaySessionService]) and the media card
 * ([CarPlayMediaBrowserService]) used code 0 and a bare `Intent(ctx, MainActivity)`, so whichever
 * was created first was handed back for the other — and the media card's
 * `EXTRA_FROM_MEDIA_CARD` tag, which is what stops a card tap from auto-starting (and undoing an
 * on-screen Stop, OD-9D), rode on the notification's untagged intent or vice versa. Distinct
 * request codes AND a distinct action on the card's intent, so neither key can collide.
 * `tools/pending_intent_lint.py` (run by `tools/test.sh`) fails the gate if two `getActivity` sites
 * targeting MainActivity ever share a request-code argument; `TapTargetTest` holds the constants and
 * the card's action.
 */
internal object TapTarget {
    /** The ongoing status notification's tap: the launcher as from the icon (may auto-start). */
    const val NOTIFICATION_REQUEST = 0
    /** The AAOS media card's tap: bring forward only (`MainActivity.EXTRA_FROM_MEDIA_CARD`). */
    const val MEDIA_CARD_REQUEST = 1
    /** Also distinct in `filterEquals`, independently of the code. */
    const val MEDIA_CARD_ACTION = "zeno.gmccpa.action.MEDIA_CARD_TAP"
}
