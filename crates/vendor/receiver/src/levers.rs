//! Process-local cells for the per-connection capability levers (`hevc`, `dpad`, `knob`,
//! `telephony`, `altscreen` + alt dims, `viewareas`, `cornermasks`, `logtransfer`, `mainbuffered`,
//! `appsetup`, `view_area_anim_ms`).
//!
//! Each cell is SEEDED from the like-named `CARPLAY_*` env var once per process, then WRITTEN per
//! control connection by `load_device_config` on all three of its paths (success / parse failure /
//! no config). So the env names below describe the SEED, not the authority (docs/carplay/04_CAPABILITIES_AND_CONFIG.md: the app owns
//! configurable values).
//!
//! Three distinct env-vs-config shapes exist, deliberately — do NOT "unify" them:
//!  1. **Vestigial** (`CARPLAY_HEVC`): cleared per connection, the YAML is the only live control
//!     (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
//!  2. **Sanctioned force-arm** (`CARPLAY_CORNERMASKS` docs/carplay/06_AV_PIPELINE.md §4, `CARPLAY_LOGTRANSFER` docs/carplay/04_CAPABILITIES_AND_CONFIG.md
//!     §2): ORed over the pushed value on the config path, so the env DOES override a pushed
//!     `false`, and honoured on the no-config paths.
//!  3. **No-config fallback only** (`CARPLAY_MAINBUFFERED`): the config path takes the YAML value
//!     ALONE, because a stale bench flag overriding a pushed `false` there could silence media
//!     (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B4). `CARPLAY_APP_SETUP` is the tri-state variant — a value, not a presence, so
//!     `0` can force OFF.
//!
//! carplayd's `load_device_config` used to publish these via `std::env::set_var`/`remove_var` on the
//! serve thread on EVERY control connection, while other live threads concurrently `getenv` the same
//! names (the HID-ingest thread's `CARPLAY_ALTSCREEN` guard; `info.rs`/`session.rs` reads during a
//! hijacking connection's setup). POSIX `setenv`/`getenv` are not mutually thread-safe — musl's
//! `setenv` frees the old value string and `unsetenv` memmoves `__environ` — so a concurrent read is
//! genuine UB, and under panic="abort" a hit is a SIGSEGV that takes the whole daemon down. This is
//! the same class the `events::set_dpad_advertised` atomic mirror fixed for one var (round-2 audit),
//! generalized to every runtime-WRITTEN lever.
//!
//! SPAWN-scoped vars (`CARPLAY_WIRELESS_METADATA`, `CARPLAY_SESSION_MGMT`, `OCBM_FWD_ENC`, …) are set
//! once by the parent before exec and never written at runtime, so they stay plain env reads.

use std::sync::atomic::{AtomicBool, Ordering};
use portable_atomic::{AtomicI64};
use std::sync::Once;

/// Whether the box forwards ENCRYPTED A/V (the host decrypts) — the committed `OCBM_FWD_ENC` model.
///
/// SAFE DEFAULT: forward-encrypted is ON unless EXPLICITLY disabled. `OCBM_FWD_ENC` is a spawn-scoped
/// var the parent sets before exec (`=1`); defaulting to ON means a launcher that drops that prefix
/// can never silently re-arm the on-box-decode path — a latent footgun, since the invariant used to
/// live ONLY in the launcher scripts (`session_supervisor.sh`, wireless `av.rs`) with no in-binary
/// floor. Only an explicit opt-out — `OCBM_FWD_ENC` = `0` / `false` / `off` / empty — selects the
/// on-box decode dev/legacy fallback. Plain env read (spawn-scoped, never written at runtime), so no
/// atomic mirror is needed. Both `session.rs` A/V spawns and carplayd's startup mode-log read via here.
pub fn fwd_enc() -> bool {
    match std::env::var("OCBM_FWD_ENC") {
        Ok(v) => {
            let v = v.trim();
            !(v.is_empty()
                || v == "0"
                || v.eq_ignore_ascii_case("false")
                || v.eq_ignore_ascii_case("off"))
        }
        Err(_) => true, // absent → safe default: forward-encrypted
    }
}

/// Sentinel for "no value" in the alt-dimension cells (the old `remove_var` state). Real dimensions
/// are small positive pixel counts, nowhere near this.
const UNSET: i64 = i64::MIN;

static SEED: Once = Once::new();
static HEVC: AtomicBool = AtomicBool::new(false);
static DPAD: AtomicBool = AtomicBool::new(false);
static KNOB: AtomicBool = AtomicBool::new(false);
static MULTITOUCH: AtomicBool = AtomicBool::new(false);
static TELEPHONY: AtomicBool = AtomicBool::new(false);
static ALTSCREEN: AtomicBool = AtomicBool::new(false);
static ALT_W: AtomicI64 = AtomicI64::new(UNSET);
static ALT_H: AtomicI64 = AtomicI64::new(UNSET);
static VIEWAREAS: AtomicBool = AtomicBool::new(false);
static CORNERMASKS: AtomicBool = AtomicBool::new(false);
static LOGTRANSFER: AtomicBool = AtomicBool::new(false);
static MAINBUFFERED: AtomicBool = AtomicBool::new(false);
static APPSETUP: AtomicBool = AtomicBool::new(false);

/// Seed every lever from the environment exactly once, before the first read or write. Presence =
/// on (matching the old `env::var(..).is_ok()` gates), and a present-but-unparseable dimension is
/// treated as unset exactly as the old `parse().ok()` chain did.
fn seed() {
    SEED.call_once(|| {
        HEVC.store(std::env::var_os("CARPLAY_HEVC").is_some(), Ordering::Relaxed);
        DPAD.store(std::env::var_os("CARPLAY_DPAD").is_some(), Ordering::Relaxed);
        KNOB.store(std::env::var_os("CARPLAY_KNOB").is_some(), Ordering::Relaxed);
        MULTITOUCH.store(std::env::var_os("CARPLAY_MULTITOUCH").is_some(), Ordering::Relaxed);
        TELEPHONY.store(std::env::var_os("CARPLAY_TELEPHONY").is_some(), Ordering::Relaxed);
        ALTSCREEN.store(std::env::var_os("CARPLAY_ALTSCREEN").is_some(), Ordering::Relaxed);
        VIEWAREAS.store(std::env::var_os("CARPLAY_VIEWAREAS").is_some(), Ordering::Relaxed);
        CORNERMASKS.store(std::env::var_os("CARPLAY_CORNERMASKS").is_some(), Ordering::Relaxed);
        LOGTRANSFER.store(std::env::var_os("CARPLAY_LOGTRANSFER").is_some(), Ordering::Relaxed);
        MAINBUFFERED.store(std::env::var_os("CARPLAY_MAINBUFFERED").is_some(), Ordering::Relaxed);
        let dim = |name: &str| -> i64 {
            std::env::var(name)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(UNSET)
        };
        ALT_W.store(dim("CARPLAY_ALT_W"), Ordering::Relaxed);
        ALT_H.store(dim("CARPLAY_ALT_H"), Ordering::Relaxed);
        // appsetup seeds from the VALUE (=="1"), not presence: CARPLAY_APP_SETUP is a 1/0 override
        // (harness use — "0" must force OFF over a YAML "true", so its presence alone can't mean on).
        APPSETUP.store(
            std::env::var("CARPLAY_APP_SETUP").is_ok_and(|v| v == "1"),
            Ordering::Relaxed,
        );
    });
}

/// HEVC lever (was `CARPLAY_HEVC` presence): `hevcInfo` in `/info` + the SETUP `enabledFeatures`
/// `"hevc"` echo.
pub fn hevc() -> bool {
    seed();
    HEVC.load(Ordering::Relaxed)
}

pub fn set_hevc(on: bool) {
    seed();
    HEVC.store(on, Ordering::Relaxed);
}

/// D-Pad lever (was `CARPLAY_DPAD` presence): the uid-3 `hidDevices[]` entry + the display features
/// word. `events::dpad_advertised` reads the same cell, so the HID-ingest gate stays in lockstep.
pub fn dpad() -> bool {
    seed();
    DPAD.load(Ordering::Relaxed)
}

pub fn set_dpad(on: bool) {
    seed();
    DPAD.store(on, Ordering::Relaxed);
}

/// Multi-touch lever (`CARPLAY_MULTITOUCH` presence, or the YAML
/// `hidConfig.touchScreenSupportsMultiTouch`): selects Apple's two-finger touchscreen descriptor for
/// the uid-1 `hidDevices[]` entry instead of the single-finger one.
///
/// The HID-ingest side reads the same cell, which is not optional here: the descriptor determines
/// the REPORT LAYOUT (12 bytes vs 5), so a report built for the wrong one is not a degraded touch,
/// it is garbage bytes against a descriptor that cannot parse them.
pub fn multi_touch() -> bool {
    seed();
    MULTITOUCH.load(Ordering::Relaxed)
}

pub fn set_multi_touch(on: bool) {
    seed();
    MULTITOUCH.store(on, Ordering::Relaxed);
}

/// Knob lever (`CARPLAY_KNOB` presence): the uid-4 `hidDevices[]` entry. `events::knob_advertised`
/// reads the same cell so the HID-ingest gate stays in lockstep. The `0x02 Knobs` display-features
/// bit is claimed unconditionally, so no features-word change is needed.
pub fn knob() -> bool {
    seed();
    KNOB.load(Ordering::Relaxed)
}

pub fn set_knob(on: bool) {
    seed();
    KNOB.store(on, Ordering::Relaxed);
}

/// Telephony lever (`CARPLAY_TELEPHONY` presence): the uid-5 `hidDevices[]` entry (Hook Switch / Flash /
/// Drop / Mute / DTMF). Opt-in + revertible like the Knob; `events::telephony_advertised` gates ingest.
pub fn telephony() -> bool {
    seed();
    TELEPHONY.load(Ordering::Relaxed)
}

pub fn set_telephony(on: bool) {
    seed();
    TELEPHONY.store(on, Ordering::Relaxed);
}

/// Alt/cluster-screen lever (was `CARPLAY_ALTSCREEN` presence): the 2nd `displays[]` entry, the
/// `altScreen` feature echo, and the HID-ingest cluster-command guard.
pub fn altscreen() -> bool {
    seed();
    ALTSCREEN.load(Ordering::Relaxed)
}

pub fn set_altscreen(on: bool) {
    seed();
    ALTSCREEN.store(on, Ordering::Relaxed);
}

/// Alt display width in px (was `CARPLAY_ALT_W`); `None` = unset → info.rs falls back to 800.
pub fn alt_w() -> Option<i64> {
    seed();
    match ALT_W.load(Ordering::Relaxed) {
        UNSET => None,
        v => Some(v),
    }
}

/// Alt display height in px (was `CARPLAY_ALT_H`); `None` = unset → info.rs falls back to 480.
pub fn alt_h() -> Option<i64> {
    seed();
    match ALT_H.load(Ordering::Relaxed) {
        UNSET => None,
        v => Some(v),
    }
}

/// Set (or clear, with `None`) both alt dimensions — mirrors the old set-both/remove-both pattern.
pub fn set_alt_dims(dims: Option<(i64, i64)>) {
    seed();
    let (w, h) = dims.unwrap_or((UNSET, UNSET));
    ALT_W.store(w, Ordering::Relaxed);
    ALT_H.store(h, Ordering::Relaxed);
}

/// viewAreas lever (was `CARPLAY_VIEWAREAS` presence): the SETUP `enabledFeatures` `"viewAreas"` echo.
pub fn viewareas() -> bool {
    seed();
    VIEWAREAS.load(Ordering::Relaxed)
}

pub fn set_viewareas(on: bool) {
    seed();
    VIEWAREAS.store(on, Ordering::Relaxed);
}

/// cornerMasks lever (`CARPLAY_CORNERMASKS`): the SETUP `enabledFeatures` `"cornerMasks"` echo plus the
/// per-view `cornerMasks: true` flag in `/info` displays[].viewAreas (which then DROPS `safeArea` on
/// that view — the two are mutually exclusive per iOS's validator). Phase 1 of the corner-mask
/// experiment: advertise the feature so the phone starts streaming its `topLeftCornerMask` buffer, which
/// `CARPLAY_CORNERMASK_CAPTURE` (server.rs) dumps so we can learn the (undocumented) wire format.
///
/// Unlike [`logtransfer`], this lever has no one-sided-emission hazard under the app-driven SETUP
/// relay: the `viewAreas` structure it rides on is emitted unconditionally, so an env force-arm the
/// host's echo cannot see produces structure-without-echo, which iOS ignores.
pub fn cornermasks() -> bool {
    seed();
    CORNERMASKS.load(Ordering::Relaxed)
}

pub fn set_cornermasks(on: bool) {
    seed();
    CORNERMASKS.store(on, Ordering::Relaxed);
}

/// docs/carplay/04_CAPABILITIES_AND_CONFIG.md #25 — the appearance and focus-transfer advertisements the box used to decide by itself.
///
/// The two appearance statics start `true` and the focus-transfer one `false`, reproducing exactly
/// what `/info` emitted before they existed: the appearance keys unconditionally, and
/// `viewAreaSupportsFocusTransfer` hardcoded `false`. Getting these initial values wrong would change
/// the wire for the no-config / parse-failure path, which is the one path that has no owner to ask.
static UI_APPEARANCE: AtomicBool = AtomicBool::new(true);
static MAP_APPEARANCE: AtomicBool = AtomicBool::new(true);
static FOCUS_TRANSFER: AtomicBool = AtomicBool::new(false);

pub fn ui_appearance() -> bool {
    seed();
    UI_APPEARANCE.load(Ordering::Relaxed)
}

pub fn set_ui_appearance(on: bool) {
    seed();
    UI_APPEARANCE.store(on, Ordering::Relaxed);
}

pub fn map_appearance() -> bool {
    seed();
    MAP_APPEARANCE.load(Ordering::Relaxed)
}

pub fn set_map_appearance(on: bool) {
    seed();
    MAP_APPEARANCE.store(on, Ordering::Relaxed);
}

/// Unlike the appearance pair, arming this ADVERTISES A CAPABILITY we have never advertised. It is
/// owner-opt-in and unvalidated on hardware.
pub fn focus_transfer() -> bool {
    seed();
    FOCUS_TRANSFER.load(Ordering::Relaxed)
}

pub fn set_focus_transfer(on: bool) {
    seed();
    FOCUS_TRANSFER.store(on, Ordering::Relaxed);
}

/// logTransfer lever (`CARPLAY_LOGTRANSFER`): `logTransferInfo = {}` in `/info` + the SETUP
/// `enabledFeatures` `"logTransfer"` echo — Tier-1 advertise/negotiate only (docs/carplay/04_CAPABILITIES_AND_CONFIG.md Half A).
/// The archive transfer itself (RCS LogTransfer channel routing + the chunked file-message codec)
/// is NOT implemented; an inbound LogTransfer RCS SETUP is accepted as a stream and its frames are
/// logged-and-dropped, which is the correct passive-responder posture until the wire shapes are known.
///
/// APP-LESS BENCH CORNER, wider here than for cornerMasks — arm the YAML, not the env, whenever the
/// app-driven SETUP relay is up. The host authors the `enabledFeatures` echo from the pushed YAML and
/// cannot see this lever's env force-arm, so an env-armed `logTransferInfo` in `/info` can pair with
/// an echo that omits the token — the iOS-27 `carEndpoint_validateInfoResponseKeyPresentForFeature`
/// "found but not negotiated" abort. Reachable both on the config path (`/tmp/logtransfer_test`
/// armed while the app pushes `enablesLogTransfer: false`) and, since the app-less paths stopped
/// wiping the seed, with `CARPLAY_APP_SETUP=1` and no/unparseable config. `cornerMasks` is exempt:
/// its backing `viewAreas` structure is emitted unconditionally, so only the harmless direction
/// (structure without echo) can occur there.
pub fn logtransfer() -> bool {
    seed();
    LOGTRANSFER.load(Ordering::Relaxed)
}

pub fn set_logtransfer(on: bool) {
    seed();
    LOGTRANSFER.store(on, Ordering::Relaxed);
}

/// mainBufferedAudio arm (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B4): config-primary — armed per connection from the pushed
/// `accessoryConfig.enablesMainBufferedAudio` (app default OFF); on the no-config/parse-failure
/// paths it falls back to `CARPLAY_MAINBUFFERED` presence (app-less bench, subordinate to pushed
/// config). Deliberately NOT the cornerMasks OR-force-arm shape: OR-ing a stale bench flag over an
/// app that said `false` could silence media if iOS moves to a buffered stream we don't serve.
/// Both the `/info` `mainBufferedInfo` emission and the SETUP `"mainBuffered"` echo read THIS
/// lever, preserving the both-sides-or-neither coupling iOS validates.
///
/// App-less bench corner: with no/unparseable config AND `CARPLAY_APP_SETUP=1`, the env-armed
/// `/info` key pairs with a host-authored echo that omits the token, which iOS-27 rejects as
/// "found but not negotiated" — arm the YAML, not the env, when running the relay harness.
pub fn mainbuffered() -> bool {
    seed();
    MAINBUFFERED.load(Ordering::Relaxed)
}

pub fn set_mainbuffered(on: bool) {
    seed();
    MAINBUFFERED.store(on, Ordering::Relaxed);
}

/// App-driven-SETUP lever (`accessoryConfig.appDrivenSetup`, env override `CARPLAY_APP_SETUP=1/0`):
/// selects `relay::RemoteSession` over the plain `AvSession` on BOTH transports when the host relay
/// seam is up (carplayd's per-connection delegate selection — plan P1; wireless joined at the
/// 2026-08-10 flip). The YAML arrives from the host at SUBSCRIBE, so this lever doubles as the
/// host-capability flag: a host that can't answer RS_REQs simply never pushes it. The app defaults
/// it ON; the box's local response stays the sticky fallback on any relay failure.
pub fn appsetup() -> bool {
    seed();
    APPSETUP.load(Ordering::Relaxed)
}

pub fn set_appsetup(on: bool) {
    seed();
    APPSETUP.store(on, Ordering::Relaxed);
}

/// View-area resize animation duration (ms) — the `animationDurationMillis` the box puts in its
/// `updateViewArea` ANSWER (`events::send_update_view_area`), the one runtime parameter on that
/// answer. Armed per connection from the pushed top-level `view_area_anim_ms` (OUR extension, the
/// `wifi_ap`/`hot_handover` shape, not Apple schema); absent = 3000, the wire the box always sent.
///
/// No env form and no bench override — the #25 shape, not the cornerMasks one: the app owns the
/// value, and a second source of truth here would just put the recompile-per-experiment loop this
/// lever exists to kill back on in a different costume. So it clears to the CONSTANT on the
/// no-config / parse-failure paths.
///
/// The cell can never hold an out-of-range value: [`set_view_area_anim_ms`] clamps to
/// [`VIEW_AREA_ANIM_MS_RANGE`] and returns what it stored, so the arming site can log the clamp.
/// iOS never acknowledges this field, so the `[events] updateViewArea -> … duration=` line (which
/// prints the value read from THIS cell, i.e. post-clamp) is the only observability there is.
pub const VIEW_AREA_ANIM_MS_DEFAULT: i64 = 3000;
/// FLOOR RAISED TO 1000 ms (owner, 2026-09-09) after the bench sweep. Both bounds are device-proven
/// on a 1920x1080 panel: 10 ms renders the switch as an instant flicker and 10000 ms runs the full
/// 10 s envelope, so iOS honours the field literally with NO floor and NO cap of its own — this
/// range is a PRODUCT decision about what to offer, not a protocol limit. 10 ms is deliberately no
/// longer reachable from the app: it works, it was simply judged too abrupt to ship as an option.
/// The DEFAULT stays 3000 (absent key), so no existing pushed document changes.
pub const VIEW_AREA_ANIM_MS_RANGE: std::ops::RangeInclusive<i64> = 1_000..=10_000;
static VIEW_AREA_ANIM_MS: AtomicI64 = AtomicI64::new(VIEW_AREA_ANIM_MS_DEFAULT);

/// Clamp a requested duration to the nearest bound of [`VIEW_AREA_ANIM_MS_RANGE`]. Never a rejection:
/// a cosmetic knob must not be able to take the whole pushed document off the air.
pub fn clamp_view_area_anim_ms(ms: i64) -> i64 {
    ms.clamp(*VIEW_AREA_ANIM_MS_RANGE.start(), *VIEW_AREA_ANIM_MS_RANGE.end())
}

pub fn view_area_anim_ms() -> i64 {
    seed();
    VIEW_AREA_ANIM_MS.load(Ordering::Relaxed)
}

/// Store the CLAMPED value and return it; a result `!= ms` means the request was out of range and
/// the caller should say so.
pub fn set_view_area_anim_ms(ms: i64) -> i64 {
    seed();
    let v = clamp_view_area_anim_ms(ms);
    VIEW_AREA_ANIM_MS.store(v, Ordering::Relaxed);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_area_anim_ms_clamps_to_nearest_bound_and_passes_in_range() {
        assert_eq!(VIEW_AREA_ANIM_MS_DEFAULT, 3000, "absent-key wire must stay 3000");
        assert_eq!(clamp_view_area_anim_ms(3000), 3000);
        // FLOOR IS 1000, not 0 — raised by the owner 2026-09-09 (see VIEW_AREA_ANIM_MS_RANGE).
        // These four assertions still expected the old 0 floor and were left behind by that
        // change, which is what turned Tier-0 red from f836a2b onward (found 2026-09-10).
        assert_eq!(clamp_view_area_anim_ms(1000), 1000, "the floor itself passes through");
        assert_eq!(clamp_view_area_anim_ms(0), 1000);
        assert_eq!(clamp_view_area_anim_ms(10_000), 10_000);
        assert_eq!(clamp_view_area_anim_ms(-1), 1000);
        assert_eq!(clamp_view_area_anim_ms(i64::MIN), 1000);
        assert_eq!(clamp_view_area_anim_ms(10_001), 10_000);
        assert_eq!(clamp_view_area_anim_ms(i64::MAX), 10_000);
        // The setter reports what it stored, and the getter reads the same (clamped) cell.
        assert_eq!(set_view_area_anim_ms(50_000), 10_000);
        assert_eq!(view_area_anim_ms(), 10_000);
        assert_eq!(set_view_area_anim_ms(VIEW_AREA_ANIM_MS_DEFAULT), 3000);
        assert_eq!(view_area_anim_ms(), 3000);
    }
}
