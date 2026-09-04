//! aa-bridge — box-side Android Auto USB bridge (Phase 2, wired). The Android Auto SET's unique
//! logic; all the USB-host and phone-detection overlap lives in `box-common`.
//!
//! The phone is already OTG-hosted on `ci_hdrc.0` (the box role-switches to `a_host` on plug). This
//! tool takes it from there:
//!   1. Find the phone on bus 1 (a non-Apple Android candidate, via box_common::phone).
//!   2. If it is not already an AOAP accessory, run the Android Open Accessory switch with the
//!      Android Auto magic strings (manufacturer `Android`, model `Android Auto`), which makes
//!      gearhead start AA projection instead of a generic-accessory dialog.
//!   3. Wait for it to re-enumerate as an accessory (`0x18d1:0x2d0x`), claim its interface, locate
//!      the bulk IN/OUT endpoints.
//!   4. Accept a TCP client (the macOS host app, which runs the whole AA protocol engine) and
//!      full-duplex-pump the raw AA byte stream: bulk-IN → TCP, TCP → bulk-OUT. The box is a dumb
//!      byte pipe — the CarPlay doctrine inverted.
//!
//! USB primitives (control/bulk/claim + descriptor parsing) come from `box_common::usb`; the AOAP
//! control sequence below is the AA-specific part.
//!
//! SECOND TRANSPORT (2026-09-04). The same process also serves WIRELESS Android Auto, in `wireless`:
//! a TCP listener on the SoftAP address that `carplay-wireless` advertised to the phone over
//! Bluetooth (`docs/androidauto/03_WIRELESS.md` §2f). It is armed only by `--wireless`, which the
//! supervisor passes when it raises the wireless stack. Everything AOAP in this file is untouched by
//! it; what the two arms share is the projection-owner arbitration, the single app-side socket
//! (`appport`) and the copy loop (`pump`).
//!
//! `--wireless` also makes the process RESIDENT: the wired arm's precondition failures become a park
//! rather than an exit, because `session_supervisor`'s `arm_aa` is `pgrep aa-bridge`-guarded and
//! would not relaunch a process that is still alive for the wireless listener. Without `--wireless`
//! the lifecycle is exactly what it was — exit, and let `arm_aa` relaunch.

mod appport;
mod pump;
mod wireless;

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use box_common::cfg;
use box_common::flags::{self, ProjectionOwner};
use box_common::phone::{self, PhoneType};
use box_common::usb;

use appport::{AppPort, Arm};

// ---- AOAP protocol constants (Android-Auto-specific) -------------------------------------------
const AOAP_GET_PROTOCOL: u8 = 51;
const AOAP_SEND_STRING: u8 = 52;
const AOAP_START: u8 = 53;
const AOAP_STRING_MANUFACTURER: u16 = 0;
const AOAP_STRING_MODEL: u16 = 1;

// Accessory PIDs the phone re-enumerates as after the switch (accessory, +adb, +audio variants).
const ACCESSORY_PIDS: [u16; 6] = [0x2d00, 0x2d01, 0x2d02, 0x2d03, 0x2d04, 0x2d05];
/// Google's USB vendor id. AOAP accessory-mode devices enumerate as `0x18d1:0x2d0x` — matching the
/// PID alone accepts any vendor's device that happens to reuse one of these PIDs.
const GOOGLE_VID: u16 = 0x18d1;

fn is_accessory(d: &usb::BusDevice) -> bool {
    d.vid == GOOGLE_VID && ACCESSORY_PIDS.contains(&d.pid)
}

/// Poll period for the precondition watchdogs while waiting for the host app to connect.
pub(crate) const POLL: Duration = Duration::from_millis(250);
/// Consecutive host-absent polls before believing it (5 s — comfortably past ocbmd's 2 s presence
/// dip; see `host_gone_confirmed`).
const HOST_GONE_POLLS: u32 = 20;
/// How long to hold the `wired-aa` claim announcing AA to a host that has not connected yet. Long
/// enough for any app that speaks the mode event (it connects in well under a second in practice),
/// short enough that a host WITHOUT AA support — which never connects — cannot starve CarPlay while
/// an Android phone is plugged in. See the `Wait::Idle` arm in main.
const ANNOUNCE_WINDOW: Duration = Duration::from_secs(30);
/// How long to wait UNCLAIMED after an unanswered announcement before announcing once more. This is
/// the BACKSTOP, not the main path: a host that (re)subscribes is detected directly (see
/// `subscribe_stamp`), so this only covers a host that never subscribes again and never leaves.
const UNCLAIMED_RETRY: Duration = Duration::from_secs(600);
/// How many (re)subscribe-triggered re-announcements a host gets before we stop reacting to its
/// subscribes for `STAMP_COOLDOWN`. Two covers an app swap; more than that is a host that keeps
/// subscribing and never speaks AA.
const MAX_STAMP_ANNOUNCES: u32 = 2;
/// Cooldown after the re-announce budget is spent.
const STAMP_COOLDOWN: Duration = Duration::from_secs(300);
const TCP_PORT: u16 = 5277;
const USB_BUS: &str = "/dev/bus/usb/001";

/// Seconds since the epoch, for log correlation with the host app's timestamps. ocbmd sets the box
/// clock via CT_SETTIME (there is no RTC), so this is meaningful once a host has connected — and the
/// lack of it is why an earlier teardown could not be lined up against the Mac's log at all.
pub(crate) fn ts() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `panic = "abort"` (workspace-wide) means the default hook's stderr line is the only trace of a
/// crash the supervisor sees — prefix it so it's greppable in the merged host log stream.
fn install_panic_hook(name: &'static str) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("[{name}] PANIC: {info}");
        default_hook(info);
    }));
}

fn main() {
    install_panic_hook("aa-bridge");
    // `--wireless` arms the second transport AND makes this process resident; see the module docs.
    // An argv flag rather than an env var because `session_supervisor` launches this from inside a
    // detached `setsid` wrapper where argv is what an operator can actually see in `ps`.
    let serve_wireless = std::env::args().skip(1).any(|a| a == "--wireless");
    eprintln!(
        "[aa-bridge {}] start (AOAP -> Android Auto USB bridge, tcp :{TCP_PORT}){}",
        ts(),
        if serve_wireless { " + wireless AP listener" } else { "" }
    );

    // One acceptor for the host app's stream, shared by both transports — two `accept()` callers on
    // one listener would race for the app's single CH_IP relay (see `appport`).
    let app = match AppPort::bind(TCP_PORT) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[aa-bridge] bind :{TCP_PORT} failed: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("[aa-bridge] listening on 0.0.0.0:{TCP_PORT}");

    if serve_wireless {
        let app_wl = app.clone();
        thread::spawn(move || wireless::run(app_wl));
    }

    loop {
        // Resident mode waits for the wired preconditions FIRST, so a box that is simply serving the
        // wireless listener with nothing plugged in stays silent instead of logging a spurious
        // "exiting" every second. Without `--wireless` this is skipped entirely and the arm runs
        // immediately, exactly as it always has — `arm_aa` only launches us when they already hold.
        if serve_wireless {
            wait_for_wired_work();
        }
        let code = run_wired_arm(&app);
        if !serve_wireless {
            std::process::exit(code);
        }
        // The wired arm's "exiting" wording is kept verbatim so existing log greps still match; this
        // line is what actually happened.
        eprintln!("[aa-bridge {}] wired arm idle — the wireless listener stays up", ts());
    }
}

/// Resident mode only: park until the wired arm has something to serve again.
///
/// This is `arm_aa`'s shell gate, in-process. It has to be: while this process lives for the
/// wireless listener, `arm_aa`'s `pgrep aa-bridge` guard suppresses the relaunch that would
/// otherwise re-run the wired arm when a phone is plugged in. The conditions are deliberately the
/// same ones — an app subscribed, an Android phone (and no Apple device) on the bus, Android Auto
/// enabled, and nobody else owning the box — so a resident bridge selects AA exactly when the
/// supervisor would have launched one.
fn wait_for_wired_work() {
    loop {
        thread::sleep(POLL * 4);
        if host_present()
            && phone_on_bus()
            && !apple_on_bus()
            && !someone_else_owns()
            && cfg::aa_enabled()
        {
            return;
        }
    }
}

/// The wired AOAP arm. Returns the process exit code it would have exited with; in resident mode
/// the caller parks instead. NOTHING inside is transport-shared beyond `app` and the owner flag.
fn run_wired_arm(app: &Arc<AppPort>) -> i32 {
    let mut backoff = Duration::from_secs(2);
    // Budget for re-announcements triggered by a (re)subscribe while unclaimed. An app swap costs one
    // or two; a host that keeps re-subscribing without ever speaking AA would otherwise re-switch the
    // phone every ~35 s forever (announce window + switch), which is visible as the phone dropping in
    // and out of accessory mode. After the budget, stamp changes are ignored until STAMP_COOLDOWN has
    // passed, and UNCLAIMED_RETRY remains the backstop. A served session or a real departure — i.e.
    // evidence about who is actually out there — refills it.
    let mut stamp_announces: u32 = 0;
    let mut last_stamp_announce = Instant::now();
    loop {
        // Preconditions for owning the box at all: an app is subscribed (nothing to serve otherwise —
        // the app IS the head unit) and a phone is on the bus. Exit, don't idle: `arm_aa` relaunches
        // us the moment both hold again, and a not-running bridge is the state the supervisor's
        // liveness self-heal understands.
        if host_gone_confirmed() {
            eprintln!("[aa-bridge] host app gone — exiting (arm_aa relaunches when it returns)");
            release_owner_if_ours();
            return 0;
        }
        if !phone_on_bus() {
            eprintln!("[aa-bridge] no phone on the bus — exiting");
            release_owner_if_ours();
            return 0;
        }
        // Belt-and-braces against being armed on top of CarPlay. session_supervisor is supposed to
        // refuse that (`carplay_session_live`), but nothing else stops us claiming the box, and the
        // consequence is severe: the supervisor would then treat the live CarPlay stack as handed off
        // and the app would be told to switch its window to AA. An Apple device on the phone-facing
        // bus means CarPlay owns it, or is about to — stand down.
        if apple_on_bus() {
            eprintln!("[aa-bridge] an Apple device is on the bus — CarPlay owns it, exiting");
            release_owner_if_ours();
            return 0;
        }
        if someone_else_owns() {
            eprintln!("[aa-bridge {}] another projection ({}) owns the box — standing down, exiting", ts(), flags::owner().as_str());
            return 0; // NOT clear_owner: the flag is someone else's, not ours to clear
        }
        if !cfg::aa_enabled() {
            eprintln!("[aa-bridge {}] android_auto:false in the pushed config — exiting", ts());
            release_owner_if_ours();
            return 0;
        }

        let acc = match prepare_accessory() {
            Ok(a) => {
                backoff = Duration::from_secs(2);
                a
            }
            Err(e) => {
                // The phone is there but won't become an accessory (AOAP refused by an MDM policy or
                // a locked-down ROM, or it is plugged in only to charge). Retry IN-PROCESS on a
                // capped backoff instead of exiting: exiting would have the supervisor relaunch us
                // every pass, and each relaunch re-runs the ~18 s switch attempt and appends to
                // /tmp/aa-bridge.log forever. Staying alive keeps arm_aa's `pgrep` guard suppressing
                // the churn, and the preconditions above still bound how long we stay.
                eprintln!("[aa-bridge] no Android Auto accessory: {e} (retry in {}s)", backoff.as_secs());
                nap_while_enabled(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue;
            }
        };
        // Claim the single projection owner so session_supervisor's escalate()/kill_session()/arm()
        // (incl. phone_reset.sh's USB port reset) stand down while AA owns ci_hdrc.0. Cleared on
        // session end; the supervisor also liveness-self-heals this flag if aa-bridge dies uncleanly.
        // Re-check at the instant of claiming: CarPlay can have armed during prepare_accessory(),
        // which takes seconds (AOAP switch + re-enumeration).
        if someone_else_owns() {
            eprintln!("[aa-bridge {}] another projection ({}) armed while we prepared — standing down, exiting", ts(), flags::owner().as_str());
            release_accessory(acc);
            return 0;
        }
        // The in-process half of the same check, and the only one that is actually ORDERED. The
        // wireless arm registers intent BEFORE it writes the flag, so this catches a phone that
        // dialled the AP during our multi-second AOAP switch even in the window where the flag still
        // reads idle. Unreachable without `--wireless`, so the wired-only lifecycle is untouched.
        if app.wireless_intent() {
            eprintln!("[aa-bridge {}] a wireless Android Auto session claimed the box while we prepared — standing down", ts());
            release_accessory(acc);
            return 0;
        }
        // The AOAP switch takes seconds, which is long enough for the toggle to have flipped since
        // the loop top. Claiming now would announce AA to the app the user just turned it off in.
        if !cfg::aa_enabled() {
            eprintln!("[aa-bridge {}] android_auto turned off while we prepared — exiting", ts());
            release_owner_if_ours();
            release_accessory(acc);
            return 0;
        }
        if let Err(e) = flags::set_owner(ProjectionOwner::WiredAa) {
            eprintln!("[aa-bridge] warning: could not claim projection owner: {e}");
        }
        eprintln!(
            "[aa-bridge {}] accessory ready: {} (in ep 0x{:02x}, out ep 0x{:02x}, iface {}) — owner=wired-aa",
            ts(), acc.path, acc.ep_in, acc.ep_out, acc.iface
        );

        match accept_while_wanted(app, Some(ANNOUNCE_WINDOW), None) {
            Wait::Client(client) => {
                stamp_announces = 0; // somebody does speak AA — refill the budget
                let peer = client.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                eprintln!("[aa-bridge {}] host connected: {peer}", ts());
                if let Err(e) = serve_session(client, acc) {
                    eprintln!("[aa-bridge {}] session ended: {e}", ts());
                }
                // Release the claim so the app sees the session end (mode -> idle) and CarPlay can
                // arm; the next pass re-prepares (serve_session resets the device, bouncing the phone
                // to normal mode) and re-claims, which re-announces AA to a reconnecting app.
                release_owner_if_ours();
                eprintln!("[aa-bridge] waiting for next host connection...");
            }
            Wait::Gone | Wait::HostChanged => {
                // The app went away (or the phone did) before it ever connected. Drop the claim and
                // bounce the phone out of accessory mode so it is not left half-switched, then let
                // the top of the loop decide whether to exit.
                stamp_announces = 0; // whoever is out there now, it is not who we announced to
                eprintln!("[aa-bridge] no host connected while wanted — releasing the claim");
                release_owner_if_ours();
                release_accessory(acc);
            }
            Wait::Idle => {
                // The app is there, the phone is there, and the announcement went unanswered for
                // ANNOUNCE_WINDOW. That is what a host app WITHOUT the AA mode-event support looks
                // like (it subscribes but never opens the CH_IP stream), and holding `wired-aa` for
                // it would starve CarPlay for as long as the Android phone stays plugged in —
                // `arm()` stands down against this flag and the supervisor's only self-heal is
                // process liveness. So: release the claim, un-switch the phone, and fall back to
                // waiting UNCLAIMED. A host that does speak AA can still connect (we prepare on
                // demand below); one that doesn't costs the box nothing.
                eprintln!("[aa-bridge] no host connected in {}s — releasing the claim, waiting unclaimed",
                          ANNOUNCE_WINDOW.as_secs());
                release_owner_if_ours();
                release_accessory(acc);

                // Baseline the pushed-config stamp at release time: any (re)subscribe after this
                // point is a host that never saw our announcement — unless we have already spent the
                // re-announce budget on this host without it ever connecting.
                let watch = if stamp_announces < MAX_STAMP_ANNOUNCES
                    || last_stamp_announce.elapsed() >= STAMP_COOLDOWN
                {
                    Some(subscribe_stamp())
                } else {
                    None
                };
                match accept_while_wanted(app, Some(UNCLAIMED_RETRY), watch) {
                    Wait::Client(client) => {
                        // Somebody DOES speak AA after all — prepare on demand (the pre-mode-event
                        // order, which is safe here because the client is already connected).
                        let peer = client.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                        eprintln!("[aa-bridge] host connected while unclaimed: {peer}");
                        match prepare_accessory() {
                            Ok(acc2) if someone_else_owns() => {
                                eprintln!("[aa-bridge {}] another projection ({}) owns the box — refusing this client", ts(), flags::owner().as_str());
                                release_accessory(acc2);
                            }
                            Ok(acc2) if !cfg::aa_enabled() => {
                                eprintln!("[aa-bridge {}] android_auto:false — refusing this client", ts());
                                release_accessory(acc2);
                            }
                            Ok(acc2) => {
                                if let Err(e) = flags::set_owner(ProjectionOwner::WiredAa) {
                                    eprintln!("[aa-bridge] warning: could not claim projection owner: {e}");
                                }
                                if let Err(e) = serve_session(client, acc2) {
                                    eprintln!("[aa-bridge {}] session ended: {e}", ts());
                                }
                                release_owner_if_ours();
                            }
                            Err(e) => eprintln!("[aa-bridge] no Android Auto accessory: {e}"),
                        }
                    }
                    Wait::HostChanged => {
                        // A host (re)subscribed: charge the budget and go announce for it.
                        stamp_announces += 1;
                        last_stamp_announce = Instant::now();
                    }
                    // Gone: the app or the phone left — back to the top, which exits or re-announces
                    // for whoever is there now, with a fresh budget.
                    Wait::Gone => stamp_announces = 0,
                    // Idle: still nobody, after UNCLAIMED_RETRY. Fall back to the top for one more
                    // announcement anyway — an app that genuinely speaks AA but missed the first
                    // window would otherwise stay dark for the rest of its session.
                    Wait::Idle => {}
                }
            }
        }
    }
}

/// Sleep for `d`, but wake early if Android Auto gets turned off.
///
/// Used only for the prepare-failure backoff, which grows to 30 s. A phone that refuses AOAP (MDM
/// policy, or one plugged in only to charge) is exactly the case a user reaches for the toggle in,
/// and a flat `sleep(30s)` would keep re-probing that phone for up to half a minute after they did.
fn nap_while_enabled(d: Duration) {
    let deadline = Instant::now() + d;
    while Instant::now() < deadline {
        if !cfg::aa_enabled() {
            return;
        }
        thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// Hand the phone back: drop our interface claim and reset the device so it leaves accessory mode
/// instead of sitting half-switched with nobody driving it.
fn release_accessory(acc: Accessory) {
    usb::release_interface(acc.file.as_raw_fd(), acc.iface as u32);
    usb::reset(acc.file.as_raw_fd());
}

/// Outcome of waiting for the host app to open the AA stream.
enum Wait {
    /// The host connected.
    Client(TcpStream),
    /// A precondition went away — the app or the phone left.
    Gone,
    /// A host (re)subscribed while we were waiting unclaimed: it has never seen an announcement.
    HostChanged,
    /// Nobody connected within the caller's window, but everything is still present.
    Idle,
}

/// Wait for the host app's connection, but only for as long as serving one still makes sense.
///
/// A plain blocking `accept()` here is what would strand the `wired-aa` claim (see main), so this
/// polls and gives up on the caller's terms: `Gone` when the app or the phone left, `Idle` when
/// `window` elapsed with both still present (`None` = wait indefinitely, used only when we hold no
/// claim). Both presence checks are debounced — the AOAP re-enumeration is a brief phone absence and
/// ocbmd's re-arm is a brief host absence, and acting on either would tear down a session that is
/// about to be used.
fn accept_while_wanted(
    app: &Arc<AppPort>,
    window: Option<Duration>,
    watch_subscribe: Option<Option<(SystemTime, u64)>>,
) -> Wait {
    let mut phone_misses = 0;
    let mut host_misses = 0;
    let deadline = window.map(|w| Instant::now() + w);
    loop {
        // Was a non-blocking `listener.accept()`; now a non-blocking take from the shared broker,
        // which additionally refuses to hand US the app's socket when the WIRELESS arm has committed
        // to a session (`appport::try_take`). Same poll cadence, same `Wait` semantics; the accept
        // error arm is gone because the acceptor thread now owns — and survives — those errors.
        if let Some(c) = app.try_take(Arm::Wired) {
            return Wait::Client(c);
        }
        // A host (re)subscribed since the caller's baseline: it has never seen an announcement, so
        // go make one. This is what catches an app REPLACEMENT while we sit unclaimed — the presence
        // flag cannot: ocbmd dips it for only 2 s on a re-arm, and a clean relaunch with an unchanged
        // config does not dip it at all.
        if let Some(baseline) = watch_subscribe {
            if subscribe_stamp() != baseline {
                eprintln!("[aa-bridge] a host (re)subscribed — re-announcing Android Auto");
                return Wait::HostChanged;
            }
        }
        thread::sleep(POLL);
        // Stand down the moment CarPlay takes the box. Only reachable while we hold NO claim (if we
        // held one, owner() would read wired-aa and arm() would have refused) — i.e. exactly the
        // unclaimed wait, which can otherwise sit for UNCLAIMED_RETRY and then AOAP-bounce the phone
        // and re-claim on top of a live CarPlay session.
        if someone_else_owns() {
            eprintln!("[aa-bridge {}] another projection ({}) took the box while waiting — standing down", ts(), flags::owner().as_str());
            return Wait::Gone;
        }
        // The user turned Android Auto off (app push -> ocbmd rewrote the config). `Gone` is the
        // right answer for both callers: it releases the claim, un-switches the phone, and drops to
        // the loop top, whose own `aa_enabled` check exits the process.
        if !cfg::aa_enabled() {
            eprintln!("[aa-bridge {}] android_auto turned off while waiting — standing down", ts());
            return Wait::Gone;
        }
        // Departure is the only host transition acted on via THIS flag, and only after the full
        // debounce. Do NOT read a short absence as "a different app arrived": ocbmd dips the flag for
        // 2 s on any re-subscribe (a settings push, a within-grace relaunch) while the SAME app is
        // present, so an undebounced edge re-switched the phone every time an AA-unaware app touched
        // its settings. Nor is the flag a reliable NEW-host signal in the other direction — a clean
        // relaunch with an unchanged config never dips it at all. Detecting a new host is the
        // `watch_subscribe` job above; this one only decides when nobody is there any more.
        host_misses = if host_present() { 0 } else { host_misses + 1 };
        if host_misses >= HOST_GONE_POLLS {
            return Wait::Gone;
        }
        phone_misses = if phone_on_bus() { 0 } else { phone_misses + 1 };
        if phone_misses >= 4 {
            return Wait::Gone; // ~1 s of absence — the phone is really gone, not re-enumerating
        }
        if let Some(d) = deadline {
            if Instant::now() >= d {
                // One last take before giving up: a connect that landed in the queue during the
                // final sleep would otherwise be abandoned, and the caller's release_accessory()
                // would reset the phone out from under a host that thinks it just connected.
                if let Some(c) = app.try_take(Arm::Wired) {
                    return Wait::Client(c);
                }
                return Wait::Idle;
            }
        }
    }
}

/// Is a host app subscribed? ocbmd owns this flag (CT_SUBSCRIBE / heartbeat watchdog).
pub(crate) fn host_present() -> bool {
    flags::is_set(flags::HOST_PRESENT)
}

/// Has the host been gone LONG ENOUGH to act on? Never trust a single clear reading.
///
/// `/tmp/host_present` is not purely "an app is subscribed": ocbmd deliberately DIPS it to 0 for
/// `REARM_HOLD` (2 s) in `rearm_presence_silently()` while the host is still there, so that
/// session_supervisor's shell poll sees a GONE→PRESENT edge and re-spawns airplayd. That fires on
/// any relaunch arriving inside the stop grace / heartbeat grace — i.e. an ordinary "quit and
/// reopen the app". Reading the dip as a departure would drop the claim and RESET the accessory
/// mid-reconnect, bouncing the phone's AA session for no reason. Confirming over a window well
/// past the dip costs nothing: nothing depends on releasing within a second.
fn host_gone_confirmed() -> bool {
    for _ in 0..HOST_GONE_POLLS {
        if host_present() {
            return false;
        }
        thread::sleep(POLL);
    }
    !host_present()
}

/// Identity stamp for the app-pushed config: (mtime, len). Changes on every CT_SUBSCRIBE, so a NEW
/// host — or the same host re-subscribing — is detectable while we sit unclaimed. Returns None when
/// no config has been pushed (no host).
fn subscribe_stamp() -> Option<(SystemTime, u64)> {
    let md = std::fs::metadata(cfg::CARPLAY_CFG_FILE).ok()?;
    Some((md.modified().ok()?, md.len()))
}

/// Release the owner flag ONLY if it is still ours.
///
/// A blanket `clear_owner()` deletes whatever token is there — including CarPlay's `wired-cp` — which
/// would hand the box to Android Auto by way of "cleaning up after ourselves". Every exit path goes
/// through here instead.
fn release_owner_if_ours() {
    if flags::owner() == ProjectionOwner::WiredAa {
        let _ = flags::clear_owner();
    }
}

/// Does a CarPlay transport (wired or wireless) own the box right now?
///
/// This is the RELIABLE stand-down, and it is why session_supervisor now writes `wired-cp`: the
/// `apple_on_bus` check below cannot see an iPhone that has already role-switched into projection
/// (it stops enumerating as 05ac), and the supervisor's launch-time gate does not apply to a bridge
/// that is ALREADY RESIDENT — which can re-claim from its unclaimed wait, from a late client, or
/// after a re-announce. Claiming under a live CarPlay session makes the supervisor treat CarPlay as
/// handed off and the app switch its window to AA mid-drive.
///
/// WIDENED 2026-09-01 from "CarPlay owns it" to "ANYONE ELSE owns it". The box is
/// first-come-first-served (docs/androidauto/02_ARBITRATION.md §0): the first connection of any
/// kind owns the session and every other is refused until it ends. Testing `is_carplay()` alone
/// meant `WirelessAa` read as "nobody owns the box", so plugging an Android phone into USB during a
/// live WIRELESS Android Auto session would arm this bridge and overwrite the owner with `wired-aa`
/// — two Android Auto sessions then contending for the same phone.
///
/// Deliberately "not None and not mine" rather than a list of specific owners: a future transport
/// added to `ProjectionOwner` is then refused by default rather than silently ignored here.
pub(crate) fn someone_else_owns() -> bool {
    match flags::owner() {
        ProjectionOwner::None => false,
        ProjectionOwner::WiredAa => false, // that is us
        _ => true,
    }
}

/// Is an Apple device on the phone-facing bus? (Only the raw enumeration is visible here; a phone
/// mid-CarPlay may have role-switched away from 05ac, which is why the shell gate is the primary
/// defence and this is the backstop.)
fn apple_on_bus() -> bool {
    // DELIBERATELY vid-only, not class-aware: an Apple-vid hub or dock should still make us stand
    // down. This is the one arm where being over-inclusive fails toward CarPlay, which is the side
    // we want to be wrong on.
    usb::enumerate_bus(USB_BUS)
        .iter()
        .any(|d| phone::classify(d.vid) == PhoneType::Apple)
}

/// Is a phone we could serve on the bus — an Android candidate in normal mode, or one already
/// switched into an AOAP accessory?
fn phone_on_bus() -> bool {
    usb::enumerate_bus(USB_BUS)
        .iter()
        .any(|d| phone::classify_dev(d.vid, d.class) == PhoneType::Android || is_accessory(d))
}

fn serve_session(client: TcpStream, acc: Accessory) -> Result<(), String> {
    client.set_nodelay(true).ok();
    let usb_fd = acc.file.as_raw_fd();
    let alive = Arc::new(AtomicBool::new(true));

    // Watchdog: notice the phone LEAVING THE BUS, promptly.
    //
    // The reader below is parked in a timeout-less bulk-IN, and a device that disconnects does not
    // reliably fail that ioctl until something resets the fd — so without this the box only learned
    // the phone was gone when the HOST gave up and closed the relay (measured: ~20 s of frozen video
    // on a real mid-session USB drop). usbfs removes the device node on disconnect, so its absence is
    // the earliest, cheapest signal available. Shutting the socket unblocks the writer, which runs the
    // normal teardown (release + reset + join), which in turn unblocks the reader.
    let dev_node = acc.path.clone();
    let alive_w = alive.clone();
    let tcp_wd = client.try_clone().map_err(|e| format!("tcp clone: {e}"))?;
    thread::spawn(move || {
        while alive_w.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(500));
            // Re-check AFTER the sleep: serve_session's own teardown resets the device (which removes
            // the node) on EVERY session end, so a watchdog waking mid-teardown would otherwise report
            // a phone departure that never happened — poisoning the very log correlation these
            // timestamps exist for.
            if !alive_w.load(Ordering::Relaxed) {
                return;
            }
            if !std::path::Path::new(&dev_node).exists() {
                eprintln!("[aa-bridge {}] phone left the bus mid-session ({dev_node}) — ending session", ts());
                alive_w.store(false, Ordering::Relaxed);
                let _ = tcp_wd.shutdown(Shutdown::Both);
                return;
            }
            // The android_auto lever, mid-session. Every other stand-down in this program guards a
            // point where we might CLAIM the box; this one is the only place that can honour the
            // toggle while we already hold it. Without it, `aa_enabled` gated nothing but the
            // supervisor's launch (`arm_aa`), so turning Android Auto off during a live session did
            // NOTHING until the phone was unplugged — the box kept projecting and kept the claim
            // that stands `arm()` down, i.e. the setting silently lied (docs/androidauto/02_ARBITRATION.md F3).
            // Same teardown as a phone departure: shutting the socket unblocks the writer, which
            // releases and resets the accessory; the loop top then exits the process.
            if !cfg::aa_enabled() {
                eprintln!("[aa-bridge {}] android_auto turned off mid-session — ending session", ts());
                alive_w.store(false, Ordering::Relaxed);
                let _ = tcp_wd.shutdown(Shutdown::Both);
                return;
            }
        }
    });

    // Thread A: phone bulk-IN -> TCP.  Main: TCP -> phone bulk-OUT.
    let mut tcp_w = client.try_clone().map_err(|e| format!("tcp clone: {e}"))?;
    let mut tcp_r = client;
    let ep_in = acc.ep_in as u32;
    let ep_out = acc.ep_out as u32;
    let alive_a = alive.clone();

    let t0 = Instant::now();
    let reader = thread::spawn(move || {
        let mut buf = vec![0u8; 16 * 1024];
        let mut total: u64 = 0;
        let mut last = Instant::now();
        while alive_a.load(Ordering::Relaxed) {
            match usb::bulk(usb_fd, ep_in, &mut buf, 0) {
                Ok(0) => continue,
                Ok(n) => {
                    total += n as u64;
                    if tcp_w.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    // Timestamped, once/sec: [t=Xs] IN(phone->host) total + this read size. So a stall
                    // shows the LAST timestamp the phone actually sent video (vs the OUT/ACK direction).
                    if last.elapsed() >= Duration::from_secs(1) {
                        eprintln!("[aa-bridge] t={}s IN phone->host total={} (+{}B last)", t0.elapsed().as_secs(), total, n);
                        last = Instant::now();
                    }
                }
                Err(e) => {
                    if e == libc::EAGAIN || e == libc::EINTR {
                        continue;
                    }
                    eprintln!("[aa-bridge {}] bulk-IN error errno={e} — phone stopped talking", ts());
                    break;
                }
            }
        }
        alive_a.store(false, Ordering::Relaxed);
        // Unblock the writer. It is parked in a timeout-less `tcp_r.read()`, and AA is a
        // phone-driven protocol — the host only writes in RESPONSE to the phone (acks, ping/sensor
        // replies) — so once the phone is gone NOTHING ever wakes that read. serve_session would
        // never return, the `wired-aa` claim would never clear, and the supervisor's only self-heal
        // is `pgrep aa-bridge` (the process is alive, just wedged): a phone unplugged mid-session
        // would lock CarPlay out of the box until someone killed us by hand.
        let _ = tcp_w.shutdown(Shutdown::Both);
    });

    let mut buf = vec![0u8; 16 * 1024];
    let mut total: u64 = 0;
    let mut last = Instant::now();
    while alive.load(Ordering::Relaxed) {
        match tcp_r.read(&mut buf) {
            Ok(0) => break, // host closed
            Ok(n) => {
                total += n as u64;
                if let Err(e) = usb::bulk_write_all(usb_fd, ep_out, &buf[..n]) {
                    eprintln!("[aa-bridge {}] bulk-OUT error errno={e}", ts());
                    break;
                }
                // Timestamped, once/sec: [t=Xs] OUT(host->phone = ACKs/ping) total. If OUT keeps its
                // timestamp advancing while IN froze -> the phone stopped sending; if OUT freezes first
                // -> our ACKs/keepalive stopped reaching the phone (the CH_IP/relay throughput stall).
                if last.elapsed() >= Duration::from_secs(1) {
                    eprintln!("[aa-bridge] t={}s OUT host->phone total={} (+{}B last)", t0.elapsed().as_secs(), total, n);
                    last = Instant::now();
                }
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                eprintln!("[aa-bridge {}] tcp read error: {e} — host closed the relay", ts());
                break;
            }
        }
    }

    alive.store(false, Ordering::Relaxed);
    usb::release_interface(usb_fd, acc.iface as u32);
    usb::reset(usb_fd); // unblock the reader parked in a no-timeout bulk-IN
    let _ = reader.join();
    Ok(())
}

struct Accessory {
    file: std::fs::File,
    path: String,
    iface: u8,
    ep_in: u8,
    ep_out: u8,
}

/// Find the phone, run the AA/AOAP switch if needed, wait for the accessory, then open it and
/// locate the bulk endpoints.
fn prepare_accessory() -> Result<Accessory, String> {
    // Already an accessory from a prior run? Verify it is actually LIVE first. A hard bridge restart
    // (SIGKILL, not a clean exit) can leave a stale 0x2d0x node whose underlying device is already
    // gone: it opens + claims fine but every bulk transfer fails ESHUTDOWN/ETIMEDOUT. If the node is
    // dead, reset it (bounces the phone back to normal mode) and fall through to a fresh AOAP switch.
    if let Some(dev) = find_accessory() {
        match open_accessory(&dev.path) {
            Ok(acc) if accessory_is_live(acc.file.as_raw_fd()) => {
                eprintln!("[aa-bridge] phone already in accessory mode (0x{:04x}:0x{:04x})", dev.vid, dev.pid);
                return Ok(acc);
            }
            Ok(acc) => {
                eprintln!("[aa-bridge] stale accessory node {} — resetting + fresh AOAP switch", dev.path);
                usb::reset(acc.file.as_raw_fd()); // bounce the phone back to normal mode
                thread::sleep(Duration::from_millis(800));
            }
            Err(e) => eprintln!("[aa-bridge] accessory open failed ({e}) — fresh AOAP switch"),
        }
    }

    // Fresh switch: find an Android candidate in normal mode (wait briefly for re-enumeration if we
    // just reset a stale accessory), switch it, then wait for it to come back as an accessory.
    let dev = wait_for_android_phone(Duration::from_secs(6))
        .ok_or_else(|| "no Android phone found on bus 1 (is it plugged into the box host port?)".to_string())?;
    eprintln!("[aa-bridge] phone at {} (0x{:04x}:0x{:04x}); starting AOAP -> Android Auto switch", dev.path, dev.vid, dev.pid);
    aoap_switch(&dev.path)?;
    wait_for_accessory(Duration::from_secs(12))
}

/// Poll for an Android device in NORMAL mode (not yet an AOAP accessory) until `timeout`.
fn wait_for_android_phone(timeout: Duration) -> Option<usb::BusDevice> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(dev) = usb::enumerate_bus(USB_BUS).into_iter().find(|d| {
            // class-aware: a bare hub is not a phone, and picking it here made AA through a hub
            // impossible (readdir order decides which node we try to switch).
            phone::classify_dev(d.vid, d.class) == PhoneType::Android && !is_accessory(d)
        }) {
            return Some(dev);
        }
        if Instant::now() > deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Poll for the phone to re-enumerate as an AOAP accessory (post-switch 0x18d1:0x2d0x), open it, and
/// confirm it is live.
fn wait_for_accessory(timeout: Duration) -> Result<Accessory, String> {
    let deadline = Instant::now() + timeout;
    loop {
        thread::sleep(Duration::from_millis(300));
        if let Some(dev) = find_accessory() {
            if let Ok(acc) = open_accessory(&dev.path) {
                if accessory_is_live(acc.file.as_raw_fd()) {
                    eprintln!("[aa-bridge] accessory appeared: {} (0x{:04x}:0x{:04x})", dev.path, dev.vid, dev.pid);
                    return Ok(acc);
                }
            }
        }
        if Instant::now() > deadline {
            return Err("phone never re-enumerated as a live AOAP accessory (AA switch rejected?)".into());
        }
    }
}

/// A device already in AOAP accessory mode (post-switch 0x18d1:0x2d0x).
fn find_accessory() -> Option<usb::BusDevice> {
    usb::enumerate_bus(USB_BUS)
        .into_iter()
        .find(is_accessory)
}

/// Liveness probe: a standard GET_CONFIGURATION control transfer (bmRequestType=0x80, bRequest=0x08)
/// goes over the wire, so a stale/dead node (whose cached descriptors still read fine) fails it,
/// while a genuinely-connected accessory returns its 1-byte config value.
fn accessory_is_live(fd: RawFd) -> bool {
    let mut cfg = [0u8; 1];
    usb::control(fd, 0x80, 0x08, 0, 0, &mut cfg, 500).is_ok()
}

/// Run the AOAP handshake with the Android Auto magic strings.
fn aoap_switch(path: &str) -> Result<(), String> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("open {path}: {e}"))?;
    let fd = f.as_raw_fd();

    let mut ver = [0u8; 2];
    let n = usb::control(fd, 0xC0, AOAP_GET_PROTOCOL, 0, 0, &mut ver, 1000)?;
    if n < 2 {
        return Err(format!("getProtocol short read ({n} B)"));
    }
    let proto = u16::from_le_bytes(ver);
    eprintln!("[aa-bridge] AOAP protocol version {proto}");
    if proto == 0 {
        return Err("phone reports AOAP unsupported (version 0)".into());
    }

    // The model "Android Auto" is what triggers AA (vs a generic accessory).
    send_string(fd, AOAP_STRING_MANUFACTURER, "Android")?;
    send_string(fd, AOAP_STRING_MODEL, "Android Auto")?;

    usb::control(fd, 0x40, AOAP_START, 0, 0, &mut [], 1000)?;
    eprintln!("[aa-bridge] AOAP start sent; phone will re-enumerate");
    Ok(())
}

fn send_string(fd: RawFd, index: u16, s: &str) -> Result<(), String> {
    let mut data = s.as_bytes().to_vec();
    data.push(0); // NUL-terminated
    usb::control(fd, 0x40, AOAP_SEND_STRING, 0, index, &mut data, 1000)?;
    Ok(())
}

/// Open the accessory, claim its interface, and find the bulk IN/OUT endpoints.
fn open_accessory(path: &str) -> Result<Accessory, String> {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("open {path}: {e}"))?;

    // Reading the usbfs node returns the cached descriptors (device + config(s)).
    let mut desc = vec![0u8; 4096];
    let got = f.read(&mut desc).map_err(|e| format!("read descriptors: {e}"))?;
    desc.truncate(got);
    let (iface, ep_in, ep_out) = usb::parse_bulk_endpoints(&desc)
        .ok_or_else(|| "no bulk IN/OUT endpoints in accessory config".to_string())?;

    usb::claim_interface(f.as_raw_fd(), iface as u32)?;

    Ok(Accessory {
        file: f,
        path: path.to_string(),
        iface,
        ep_in,
        ep_out,
    })
}
