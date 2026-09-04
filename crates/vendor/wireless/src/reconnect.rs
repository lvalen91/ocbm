//! Model-B accessory-initiated reconnect (docs/wireless/01_BT_AND_RADIO.md): on boot, drive a bonded iPhone back into wireless
//! CarPlay with no user interaction — the behavior native head units and the stock Carlinkit firmware
//! both have. Purely additive to the accept path (`rfcomm::accept_one` in `main.rs`), which stays the
//! fallback for first-time pairing and any phone-initiated connect.
//!
//! Per the stock firmware (docs/wireless/01_BT_AND_RADIO.md §"What the working implementations do"): page the bonded phone,
//! SDP-*query* it for its iAP2 RFCOMM channel, then become the RFCOMM CLIENT and open the iAP2 channel
//! TO the phone. Steps here:
//!   1. `sdp_client::query`  — L2CAP-connect to the phone's SDP PSM (implicitly pages it) and read the
//!      iAP2 RFCOMM channel. This is also the on-hardware probe for docs/wireless/01_BT_AND_RADIO.md's open unknown (does iOS
//!      expose the service on reconnect?).
//!   2. `rfcomm::connect_to` — RFCOMM-connect OUT to that channel.
//!   3. `bt_driver::run`     — the existing, unchanged iAP2 Identify → auth → WiFi-handoff driver.
//!
//! ANDROID AUTO RIDES THE FIRST TWO OF THOSE STEPS, TO A DIFFERENT END (2026-09-04, corrected).
//! The same SDP conversation also asks the peer for an HFP (`0x111F`) and HSP (`0x1112`) audio
//! gateway, and when it has one we RFCOMM-connect OUT to it and complete a headset service-level
//! connection (`hfp_hf`). That is not the Android Auto bootstrap — it is the PRECONDITION for it.
//! gearhead 17.5 will not start wireless setup until the phone's own `BluetoothProfile.HEADSET`
//! reports the head unit connected (`pcl.java:80`, `kzt.java:56-64`, `pco.java:24-29`,
//! `ozb.java:139`; failure event `WIRELESS_SETUP_FAILED_TO_START_NO_HFP_FROM_HU_PRESENCE`), and
//! once it does, the PHONE opens the Android Auto record we advertise on channel 4
//! (`createRfcommSocketToServiceRecord(4de17a00-…)`, `ojk.java:31-35`) — where `main.rs`'s accept
//! thread runs the bootstrap. Stock does precisely this: `hfpd` completes the SLC and the phone
//! opens the AAP channel 26 ms later (`aa_full_session_adapter_20260315.txt:442-607`).
//!
//! An earlier pass had this backwards — it dialled the phone for `4de17a00-…` on the theory that
//! the phone hosted that record. It does not; the search comes back empty
//! (`AA-wireless-UUID search -> 2 bytes: 3500`) and is kept only as a diagnostic.
//! See docs/androidauto/03_WIRELESS.md §2f and §6d.
//!
//! Only ever attempts while no session is live (`session_active`), so it never fights the accept path
//! or a session already in progress. Bounded backoff (10→60 s) between rounds; reset after any session
//! so a post-drive reconnect is prompt.

use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use box_common::flags::{self, ProjectionOwner};

use crate::control::Control;
use crate::{bt_driver, control, hfp_hf, sco_audio, sdp_client};

/// Bounds on a single SDP/RFCOMM connect to a quiet or absent phone (seconds). Long enough for a real
/// page+connect over BR/EDR, short enough that a missing phone doesn't park the thread for a whole
/// backoff interval.
const CONNECT_TIMEOUT_SECS: i64 = 8;
const BACKOFF_START_SECS: u64 = 10;
const BACKOFF_MAX_SECS: u64 = 60;
/// Let bring-up/SSP/SDP settle and give the phone a moment to connect IN on its own before we start
/// paging it ourselves.
const INITIAL_SETTLE_SECS: u64 = 5;

/// `@<unix_ms> ` write-time stamp (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG): the box.log tailer
/// parses this prefix and uses it instead of the millisecond it happened to READ the line at.
fn log(m: &str) {
    println!("@{} [reconnect] {m}", now_ms());
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// How long to hold the ACL open after a bonded peer exposes NEITHER iAP2 NOR Android Auto wireless
/// projection. Default 30 s.
///
/// NARROWED 2026-09-04. It used to cover every non-iAP2 peer, on the theory that holding the link
/// past gearhead's 5 s `waitForHeadUnitConnected` window would make the phone open OUR channel 4.
/// The bench run disproved that: time is not what the phone is waiting for, a headset profile is
/// (§6b), and holding an idle ACL just makes the same timeout arrive later. A peer that exposes an
/// audio gateway is now CONNECTED TO instead (`attempt_headset`), and the RFCOMM link that creates
/// holds the ACL by itself — so this lever is left only for a peer that offers nothing to connect
/// to.
/// Override: `CARPLAY_ACL_HOLD_SECS` or `/tmp/acl_hold_secs` (`0` disables the hold).
///
/// Read from `CARPLAY_ACL_HOLD_SECS` or, because this daemon is `exec`d from inside the
/// supervisor's `setsid sh -c` where setting an env var means editing a shipped script, from
/// `/tmp/acl_hold_secs`. The file form matches the project's existing bench-lever convention
/// (`/tmp/carplay_metadata`), and `/tmp` is tmpfs so a reboot clears it.
const ACL_HOLD_DEFAULT_SECS: u64 = 30;

fn acl_hold_secs() -> Option<u64> {
    let parse = |v: &str| v.trim().parse::<u64>().ok();
    let configured = std::env::var("CARPLAY_ACL_HOLD_SECS")
        .ok()
        .and_then(|v| parse(&v))
        .or_else(|| std::fs::read_to_string("/tmp/acl_hold_secs").ok().and_then(|v| parse(&v)));
    match configured {
        Some(0) => None,
        Some(n) => Some(n),
        None => Some(ACL_HOLD_DEFAULT_SECS),
    }
}

/// Sleep `secs`, but wake every second to observe `shutdown`. Returns early `false` if shutdown fired.
fn interruptible_sleep(secs: u64, shutdown: &AtomicBool) -> bool {
    for _ in 0..secs {
        if shutdown.load(Ordering::Relaxed) {
            return false;
        }
        thread::sleep(Duration::from_secs(1));
    }
    !shutdown.load(Ordering::Relaxed)
}

/// As [`interruptible_sleep`], but also cuts short when the device screen has asked for a connect.
///
/// A tap on *Connect* used to do nothing for up to `BACKOFF_MAX_SECS` — the request is read only at
/// the top of the loop — while the app had already answered `{"ok":true}`. Peeks, so the request is
/// still there for the loop to consume.
///
/// Deliberately NOT used by the no-bonds sleep: a request cannot be served with nothing bonded, so
/// waking on it there would spin the loop instead of waiting for a bond.
fn sleep_or_request(secs: u64, shutdown: &AtomicBool, ctrl: &Control) -> bool {
    for _ in 0..secs {
        if shutdown.load(Ordering::Relaxed) {
            return false;
        }
        if ctrl.has_request() {
            return true;
        }
        thread::sleep(Duration::from_secs(1));
    }
    !shutdown.load(Ordering::Relaxed)
}

/// One reconnect attempt against `peer`: SDP-query → RFCOMM-connect → hand to the iAP2 driver.
/// Returns `true` if a session actually ran (so the caller resets its backoff); `false` also covers
/// losing the single-session claim to the accept path. The slot is CLAIMED via `compare_exchange` right
/// before the driver runs (not held across the connect) and cleared only by the claim owner.
/// What one [`attempt`] did, so the loop can pick the right backoff.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Attempt {
    /// The iAP2 driver ran (a real session, however short).
    Ran,
    /// SDP/RFCOMM failed for an ordinary reason (absent phone, page timeout, lost claim).
    Failed,
    /// The phone rejected the SSP re-pair (`rfcomm_uspace::connect_to` → `PermissionDenied`):
    /// paging it again just produces another prompt it cannot accept. Hold off for the maximum
    /// backoff and let the operator re-pair from the iPhone.
    PairRejected,
}

fn attempt(
    peer: [u8; 6],
    name: &str,
    shutdown: &AtomicBool,
    session_active: &AtomicBool,
    ctrl: &Control,
) -> Attempt {
    // Do not PAGE at all while a wireless Android Auto session owns the box.
    //
    // The channel-4 accept path releases the single-session slot as soon as its bootstrap returns,
    // which for the normal handover is the moment the phone has the credentials — so
    // `session_active` is free for the whole TCP session and this loop would otherwise resume
    // paging the projecting phone every 10–60 s. A BR/EDR page at a phone that is streaming Android
    // Auto over 2.4 GHz Wi-Fi is at best wasted and at worst coexistence interference, and
    // `attempt_headset` would stand down on the owner flag anyway.
    //
    // Deliberately ONLY `WirelessAa`, not "any owner". `flags::owner()` falls back to the legacy
    // `/tmp/carplay_transport`, so a stale `wireless` there would silently disable the CarPlay
    // reconnect loop this function exists for. No such fallback can produce `WirelessAa` — that
    // token is written by this crate and `aa-bridge` only, and every path that writes it releases it.
    if flags::owner() == ProjectionOwner::WirelessAa {
        log("a wireless Android Auto session owns the box — not paging");
        return Attempt::Failed;
    }
    let services = match sdp_client::query(peer, CONNECT_TIMEOUT_SECS) {
        Ok(s) => s,
        Err(e) => {
            log(&format!("SDP query failed: {e}"));
            return Attempt::Failed;
        }
    };
    // WIRED Android Auto owns the box (2026-09-04): the phone on the cable still needs the car as
    // its hands-free unit — gearhead showed "Not connected to Bluetooth" and kept the call on the
    // handset until the owner connected the box manually — so the headset link IS raised in that
    // state, but an iPhone is never paged for CarPlay (first-come-wins, the wired session cannot be
    // interrupted; a page would only make the iPhone try and fail).
    if flags::owner() == ProjectionOwner::WiredAa && services.iap2.is_some() {
        log("a wired Android Auto session owns the box — not paging the iPhone");
        return Attempt::Failed;
    }
    let channel = match services.iap2 {
        Some(ch) => ch,
        None => {
            // Not an iPhone. If it exposes an HFP or HSP audio gateway, raise a headset link to it:
            // that is the only thing that flips `BluetoothProfile.HEADSET` for our address, which is
            // what gearhead's wireless-setup gate reads before it will dial our Android Auto record.
            if services.has_audio_gateway() {
                return attempt_headset(peer, services, shutdown);
            }
            // No iAP2 and no gateway. Nothing to connect to; the ACL-hold lever is all that is
            // left, and it is off unless configured. See `acl_hold_secs`. Note this arm is reached
            // regardless of `services.aawg` — that search is a diagnostic and nothing dials it.
            if let Some(secs) = acl_hold_secs() {
                sdp_client::hold_acl(peer, secs, CONNECT_TIMEOUT_SECS);
            }
            return Attempt::Failed;
        }
    };
    let sock = match crate::rfcomm_connect(peer, channel, CONNECT_TIMEOUT_SECS) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            log(&format!(
                "RFCOMM connect to channel {channel} aborted: {e} — holding off {BACKOFF_MAX_SECS}s; \
                 re-pair on the iPhone (Settings ▸ Bluetooth) or switch pairing to Just-Works"
            ));
            return Attempt::PairRejected;
        }
        Err(e) => {
            log(&format!("RFCOMM connect to channel {channel} failed: {e}"));
            return Attempt::Failed;
        }
    };
    log(&format!("RFCOMM connected OUT to the phone (ch {channel}) — starting iAP2 handshake"));
    // The real success signal is iAP2 progress inside bt_driver::run, NOT the mgmt DEVICE_CONNECTED
    // for our own outbound connect (docs/wireless/01_BT_AND_RADIO.md §Design notes — the Model-A latching gotcha).
    // Claim the single-session slot ATOMICALLY, immediately before running the driver. compare_exchange
    // (not a plain store) closes the TOCTOU with the accept path: the ~16 s SDP-query + RFCOMM-connect
    // above can overlap an inbound accept that took the slot in the meantime. If we lose the claim, stand
    // down — drop the freshly-connected socket (the phone retries) rather than run a second concurrent
    // bt_driver::run against the same phone. Only the claim owner clears the flag (below), so a finishing
    // session can never clobber another owner's still-live claim.
    // The guard claims the slot, publishes the peer for the device screen, and releases BOTH on
    // every exit from here — so the claim and its release cannot drift apart as this block grows,
    // which is what had already happened between this call site and the accept path's.
    let Some(_claim) = control::SessionClaim::try_claim(session_active, ctrl, Some(peer)) else {
        log("session already active — standing down (dropping outbound connect)");
        return Attempt::Failed;
    };
    bt_driver::run(sock, name, shutdown);
    log("reconnect session ended");
    Attempt::Ran
}

/// Satisfy gearhead's headset gate: connect OUT to the phone's own audio-gateway RFCOMM channel and
/// hold the link while the PHONE opens our Android Auto channel.
///
/// **This replaced `attempt_aa`, which dialled the phone's supposed AA wireless-projection record
/// (2026-09-04, first pass).** That could never work on this phone: gearhead is the CLIENT of
/// `4de17a00-…` (`createRfcommSocketToServiceRecord`, `ojk.java:31-35`) and the Pixel's SDP has no
/// such record at all (`AA-wireless-UUID search -> 2 bytes: 3500`). What actually gates the phone
/// is `BluetoothProfile.HEADSET` reporting US connected (`pcl.java:80`, `kzt.java:56-64`,
/// `pco.java:24-29`, `ozb.java:139`); once it does, the phone dials the record we already advertise
/// on channel 4 and `main.rs`'s accept thread runs the bootstrap. Stock does exactly this — its
/// `hfpd` completes the SLC and the phone opens the AAP channel 26 ms later
/// (`aa_full_session_adapter_20260315.txt:442-607`).
///
/// So this function never claims the single-session slot and never runs a bootstrap. It opens a
/// door and holds it: `run_aa_bootstrap` in the channel-4 accept path is still where the
/// `SessionClaim` and the owner flag are taken, unchanged.
fn attempt_headset(
    peer: [u8; 6],
    services: sdp_client::Services,
    shutdown: &AtomicBool,
) -> Attempt {
    // Stand down if ANY projection already owns the box, before paging anything further — the box
    // is first-come-first-served (docs/androidauto/02_ARBITRATION.md §0), and a headset link raised
    // on top of a live CarPlay session buys nothing and costs a page.
    // (A WIRED Android Auto owner is the exception: that phone needs this link for calls and the
    // Assistant's Bluetooth path, and gearhead reports "Not connected to Bluetooth" without it.)
    let owner = flags::owner();
    if owner != ProjectionOwner::None && owner != ProjectionOwner::WiredAa {
        log(&format!(
            "AA: another projection ({}) owns the box — not raising the headset link",
            owner.as_str()
        ));
        return Attempt::Failed;
    }

    // Which routes to try, in order. AUTO is HFP then HSP: HFP is the one proven against this phone
    // (stock), HSP is the one both public dongles use and needs no AT traffic at all. The lever
    // forces one, for a bench run that wants to isolate a failure to a single route.
    let forced = hfp_hf::forced_path();
    if let Some(p) = forced {
        log(&format!("AA: headset path forced to {} by CARPLAY_AA_HEADSET_PATH", p.as_str()));
    }
    let candidates = headset_candidates(&services, forced);
    if candidates.is_empty() {
        log("AA: the phone exposes no audio gateway we are allowed to dial — cannot satisfy gearhead's headset gate");
        return Attempt::Failed;
    }

    for (path, channel) in candidates {
        log(&format!(
            "AA: dialling the phone's {} audio gateway on RFCOMM channel {channel}",
            path.as_str()
        ));
        let mut sock = match crate::rfcomm_connect(peer, channel, CONNECT_TIMEOUT_SECS) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                log(&format!(
                    "AA: RFCOMM connect to the {} gateway (ch {channel}) aborted: {e} — holding off {BACKOFF_MAX_SECS}s; re-pair on the phone",
                    path.as_str()
                ));
                return Attempt::PairRejected;
            }
            Err(e) => {
                log(&format!(
                    "AA: RFCOMM connect to the {} gateway (ch {channel}) failed: {e}",
                    path.as_str()
                ));
                continue;
            }
        };
        // Before ANY read on this socket: `rfcomm::connect_to` arms only `SO_SNDTIMEO`, so without
        // this a gateway that accepts the channel and then goes quiet parks the SLC — and with it
        // this whole reconnect loop — in an unbounded `read()`. See `hfp_hf::arm_socket_timeouts`.
        if let Err(e) = hfp_hf::arm_socket_timeouts(&sock) {
            log(&format!("AA: could not arm socket timeouts on the {} link: {e} — dropping it rather than risk an unbounded read", path.as_str()));
            continue;
        }
        let up = match path {
            hfp_hf::Path::Hsp => hfp_hf::establish_hsp(),
            hfp_hf::Path::Hfp => match hfp_hf::establish_hfp(&mut sock) {
                Ok(up) => up,
                Err(e) => {
                    // Named step, always — "the SLC failed" is not actionable on a bench.
                    log(&format!("AA: HFP service-level connection failed at {e}"));
                    continue; // fall through to HSP, which needs no AT dialogue at all
                }
            },
        };
        match path {
            hfp_hf::Path::Hfp => log(&format!(
                "AA: HFP hands-free link up with the phone (SLC in {} ms) — waiting for it to open our Android Auto channel",
                up.slc.elapsed.as_millis()
            )),
            hfp_hf::Path::Hsp => log(
                "AA: HSP headset link up with the phone (no AT dialogue) — waiting for it to open our Android Auto channel",
            ),
        }
        let end = hold_headset_link(&sock, up, shutdown);
        log(&format!("AA: {} link released ({})", path.as_str(), end.as_str()));
        match end {
            // A real session ran on top of this link, so the caller resets its backoff.
            HeadsetLinkEnd::ProjectionEnded => return Attempt::Ran,
            HeadsetLinkEnd::ShuttingDown => return Attempt::Failed,
            // The gate opened and the phone still did not dial, or something else took the box.
            // Trying the OTHER route would not change either answer.
            HeadsetLinkEnd::SetupTimeout | HeadsetLinkEnd::Preempted => return Attempt::Failed,
            // The gateway hung up on us. That IS route-specific — a gateway that refuses HFP may
            // still accept HSP — so fall through to the next candidate.
            HeadsetLinkEnd::PeerClosed => continue,
        }
    }
    Attempt::Failed
}

/// Which headset routes to try against this peer, in order.
///
/// AUTO (`forced == None`) is HFP first, then HSP: HFP is the route proven against this phone by the
/// stock firmware, HSP is the one both public dongles use and the one AOSP opens the service level
/// on with no AT traffic (`bta_ag_act.cc:533-540`). A forced path yields at most that one, and an
/// empty result means the peer offers nothing we can dial — never a silent fallback to the other
/// route, because the point of the lever is isolating a failure to one of them.
fn headset_candidates(
    services: &sdp_client::Services,
    forced: Option<hfp_hf::Path>,
) -> Vec<(hfp_hf::Path, u8)> {
    let mut out = Vec::with_capacity(2);
    if forced != Some(hfp_hf::Path::Hsp) {
        if let Some(ch) = services.hfp_ag {
            out.push((hfp_hf::Path::Hfp, ch));
        }
    }
    if forced != Some(hfp_hf::Path::Hfp) {
        if let Some(ch) = services.hsp_ag {
            out.push((hfp_hf::Path::Hsp, ch));
        }
    }
    out
}

/// How long to hold a headset link waiting for the phone to open our Android Auto channel.
///
/// BOUNDED, and the bound is chosen against CarPlay rather than against Android Auto. This wait
/// blocks the reconnect loop — `run()` walks the bonded phones in order and calls `attempt()` for
/// each — so every second spent here is a second a bonded iPhone later in that list is not being
/// driven back into CarPlay. An unbounded hold would starve it outright.
///
/// 20 s is ~800x the 26 ms stock's phone took between its last `OK` and opening the AAP channel
/// (`aa_full_session_adapter_20260315.txt:598-607`), and comfortably past gearhead's own 5 s
/// `waitForHeadUnitConnected` window. A phone that has not dialled by then is not going to on this
/// cycle, and the loop's backoff brings us straight back.
const HEADSET_SETUP_GRACE: Duration = Duration::from_secs(20);

/// Why [`hold_headset_link`] returned. Typed rather than a string because the caller BRANCHES on
/// every arm and a `&str` comparison would be a silent trap the day the text is reworded.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum HeadsetLinkEnd {
    /// A wireless Android Auto session started on top of this link and has now ended.
    ProjectionEnded,
    /// The phone never opened our Android Auto channel within [`HEADSET_SETUP_GRACE`].
    SetupTimeout,
    /// Some OTHER projection claimed the box while we were holding the door open — a CarPlay
    /// session on the accept path, or wired anything. First-come-wins
    /// (docs/androidauto/02_ARBITRATION.md §0) means we stand down rather than keep an HFP link up
    /// against a phone the box is no longer available to.
    Preempted,
    /// The gateway hung up, or the socket errored.
    PeerClosed,
    /// `shutdown` fired; the daemon is going away.
    ShuttingDown,
}

impl HeadsetLinkEnd {
    fn as_str(self) -> &'static str {
        match self {
            HeadsetLinkEnd::ProjectionEnded => "the Android Auto session ended",
            HeadsetLinkEnd::SetupTimeout => "the phone never opened our Android Auto channel",
            HeadsetLinkEnd::Preempted => "another projection claimed the box",
            HeadsetLinkEnd::PeerClosed => "the gateway closed the link",
            HeadsetLinkEnd::ShuttingDown => "daemon shutting down",
        }
    }
}

/// Hold the headset link open and drain it, in two phases.
///
/// **Phase 1 — setup.** Wait for the projection owner to become `wireless-aa`, which is
/// `run_aa_bootstrap`'s claim in the channel-4 accept path, i.e. the phone actually dialled us.
/// Bounded by [`HEADSET_SETUP_GRACE`].
///
/// **Phase 2 — session.** Keep holding for as long as that owner flag stands. Dropping the link
/// here would flip the phone's `BluetoothProfile.HEADSET` back to DISCONNECTED mid-projection, and
/// stock keeps its own SLC up for the whole drive. `aa_pump_session_live()` is polled once and
/// logged the first time it reads true — the TCP arrival trails the bootstrap by an association +
/// DHCP + connect, so it is confirmation for the log, not an exit condition.
///
/// POLLED, never parked in a blocking read: `shutdown` and the owner flag must both be observed
/// once a second, and a blocking read on a gateway that simply went quiet would hold this thread —
/// and with it the whole reconnect loop — indefinitely.
///
/// Reads are drained, CLASSIFIED and logged. The "never answered" half of this comment used to read
/// "we are a hands-free unit that carries no audio: there is nothing an unsolicited `+CIEV`/`RING`
/// asks of us, and replying would invite a call setup we cannot service." The premise is no longer
/// true — `sco_audio` serves the call setup — but the CONCLUSION stands and is now a deliberate
/// policy rather than a limitation: answering and hanging up belong to the driver, on the phone or
/// the Android Auto screen. The one exception is the bench lever `hfp_hf::autoanswer`.
fn hold_headset_link(
    sock: &std::fs::File,
    up: hfp_hf::SlcUp,
    shutdown: &AtomicBool,
) -> HeadsetLinkEnd {
    hold_headset_link_inner(sock, up, shutdown, Some(HEADSET_SETUP_GRACE))
}

/// The INBOUND half: the phone dialled our headset record, so there is nothing to time out and no
/// reconnect loop to unblock. Hold and drain until the phone hangs up or the daemon goes quiet.
///
/// Deliberately no setup grace and no exit on the projection ending: the phone opened this link and
/// it is the phone's to close. Dropping it after one session would flip its
/// `BluetoothProfile.HEADSET` state back to disconnected and shut the gate on the next one.
pub(crate) fn drain_headset_link(
    sock: &std::fs::File,
    up: hfp_hf::SlcUp,
    shutdown: &AtomicBool,
) -> &'static str {
    hold_headset_link_inner(sock, up, shutdown, None).as_str()
}

/// The shared hold loop. `setup_grace` distinguishes the two callers: `Some` bounds the wait for the
/// phone to open our Android Auto channel and returns once a session that started on this link has
/// ended (the outbound case, which blocks the reconnect loop); `None` holds until the link itself
/// goes away (the inbound case, which blocks nothing).
fn hold_headset_link_inner(
    sock: &std::fs::File,
    up: hfp_hf::SlcUp,
    shutdown: &AtomicBool,
    setup_grace: Option<Duration>,
) -> HeadsetLinkEnd {
    use std::io::Read;
    let indicators = up.slc.indicators.clone();
    let path = up.slc.path;

    // The SCO listener lives exactly as long as the headset link that carries its AT control
    // channel. Started here rather than at daemon startup for two reasons: a SCO socket bound with
    // no SLC behind it would let a phone open an audio channel we have no call state for, and
    // `:9112` must not be held while this box is not the Android Auto owner (`sco_audio`'s own gate
    // enforces the second, but not starting at all is the cheaper half).
    let sco = sco_audio::ScoAudio::start(sco_audio::local_bdaddr(crate::HCI_DEV));
    let mut calls = hfp_hf::CallTracker::new(&indicators);

    // The wideband lever, logged ONCE per headset link and reported as what actually went on the
    // wire (`slc.wbs`), not as what the lever says now — the file can be created between the SLC and
    // this line, and a log that claimed wideband while `AT+BRSF=63` went out would send a bench
    // looking for a `+BCS` that can never arrive.
    let wbs = up.slc.wbs;
    log(&format!(
        "AA: HFP wideband speech (mSBC) lever is {} — {}",
        if hfp_hf::wbs_enabled() { "ON" } else { "OFF" },
        match (path, wbs, hfp_hf::wbs_enabled()) {
            (_, true, _) => "offered AT+BRSF=191 then AT+BAC=1,2; the AG chooses with +BCS",
            (hfp_hf::Path::Hsp, _, _) => "the HSP path runs no AT dialogue at all — CVSD narrowband",
            (_, false, true) =>
                "but the AG does not claim codec negotiation (+BRSF bit 9) — CVSD narrowband",
            (_, false, false) => "AT+BRSF=63, no AT+BAC — CVSD narrowband, the proven path",
        }
    ));
    // Set once, by the first fallback: an AG that answers a declined mSBC with another `+BCS: 2`
    // must not walk us back into a transparent channel we already failed to configure.
    let mut narrowed = false;

    // Classify, then log. `describe_unsolicited` renders the raw line (resolving `+CIEV: 6,4` to
    // `battchg = 4`); the tracker turns the subset that matters into named transitions. Both are
    // emitted: the raw line is what a bench session correlates against an HCI trace, the transition
    // is what a reader actually wants.
    let on_line = |l: &str,
                   calls: &mut hfp_hf::CallTracker,
                   sco: &sco_audio::ScoAudio,
                   narrowed: &mut bool|
     -> Vec<String> {
        log(&format!("AA: {} says {}", path.as_str(), hfp_hf::describe_unsolicited(l, &indicators)));
        if let Some(num) = hfp_hf::parse_clip(l) {
            log(&format!("AA: HFP caller id {num}"));
        }
        let mut send = Vec::new();
        for ev in calls.observe(l) {
            log(&format!("AA: {}", ev.describe()));
            if ev == hfp_hf::CallEvent::IncomingRinging && hfp_hf::autoanswer() {
                send.push("ATA".to_string());
            }
        }
        // `+BCS: <id>` — the AG has STOPPED and is waiting for our answer before it opens (e)SCO, so
        // this must produce exactly one command on every path through it. The SCO air mode is
        // applied first and the reply reports what was actually applied: telling the AG "mSBC" and
        // then failing to make the socket transparent is the one outcome that puts noise on a live
        // call instead of narrowband audio.
        if let Some(id) = hfp_hf::parse_bcs(l) {
            let choice = hfp_hf::choose_codec(id, wbs, *narrowed);
            let cmd = match choice {
                hfp_hf::CodecChoice::Use(hfp_hf::CODEC_MSBC) => {
                    if sco.set_codec(sco_audio::ScoCodec::Msbc) {
                        log("AA: HFP codec negotiated: mSBC (wideband 16 kHz)");
                        choice.command()
                    } else {
                        *narrowed = true;
                        log("AA: could not put the SCO listener in transparent mode — offering CVSD only (AT+BAC=1)");
                        hfp_hf::CodecChoice::NarrowToCvsd.command()
                    }
                }
                hfp_hf::CodecChoice::Use(_) => {
                    sco.set_codec(sco_audio::ScoCodec::Cvsd);
                    log("AA: HFP codec negotiated: CVSD");
                    choice.command()
                }
                hfp_hf::CodecChoice::OfferBoth => {
                    log(&format!(
                        "AA: the AG chose codec id {id}, which we never offered — re-offering AT+BAC=1,2"
                    ));
                    choice.command()
                }
                hfp_hf::CodecChoice::NarrowToCvsd => {
                    *narrowed = true;
                    sco.set_codec(sco_audio::ScoCodec::Cvsd);
                    log(&format!("AA: declining codec id {id} — offering CVSD only (AT+BAC=1)"));
                    choice.command()
                }
            };
            send.push(cmd);
        }
        // Arm on ANY reason the AG has audio — a ringing call, an active call, or a voice
        // recognition session — so the mic seam and the app's capture are up before the first SCO
        // packet arrives. Waiting for the SCO accept would clip the onset of every Assistant query,
        // which is precisely the turn the user cares most about.
        if calls.audio_wanted() {
            sco.arm("the audio gateway (call or voice recognition)");
        } else {
            sco.disarm("no call and no voice recognition");
        }
        send
    };

    let mut to_send: Vec<String> = Vec::new();
    for l in &up.pending {
        to_send.extend(on_line(l, &mut calls, &sco, &mut narrowed));
    }
    let mut carry = up.carry;

    let fd = sock.as_raw_fd();
    let mut buf = [0u8; 512];
    let setup_deadline = setup_grace.map(|g| Instant::now() + g);
    let mut projecting = false;
    let mut pump_logged = false;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            // `sco` is dropped on every exit path from this function, and its Drop sets the
            // shutdown flag and joins both listener threads — so the SCO socket and `:9112` are
            // always released with the link, not left to the next one's bind to discover.
            return HeadsetLinkEnd::ShuttingDown;
        }
        // Answers to the AG: `AT+BCS=<id>` / `AT+BAC=…` (codec negotiation), and `ATA` under the
        // bench lever (`CARPLAY_HFP_AUTOANSWER=1` / `/tmp/hfp_autoanswer`). Sent here rather than
        // inside the line handler so the write is not nested inside the read that produced it, and
        // `impl Write for &File` keeps the caller's ownership of the socket intact.
        //
        // Latency matters for exactly one of these: the AG holds its (e)SCO request until `AT+BCS`
        // lands. This runs at the top of the loop immediately after the read that produced the
        // `+BCS`, so the reply is on the wire before the next `poll`, not a tick later.
        for cmd in to_send.drain(..) {
            use std::io::Write as _;
            let mut w: &std::fs::File = sock;
            match w.write_all(format!("{cmd}\r").as_bytes()) {
                Ok(()) => log(&format!("AA: -> {cmd}")),
                Err(e) => log(&format!("AA: could not send {cmd}: {e}")),
            }
        }
        let flag = flags::owner();
        // Either Android Auto owner keeps the link: wireless (the phone dialled our AA channel) or
        // wired (the session is on the cable and the link exists for its calls).
        let owns = flag == ProjectionOwner::WirelessAa || flag == ProjectionOwner::WiredAa;
        // Someone else took the box while we were waiting. Only meaningful before a session of ours
        // starts: once `projecting` is set, `owns` going false is our own session ending, and that
        // is handled below.
        if !projecting && !owns && flag != ProjectionOwner::None && setup_grace.is_some() {
            return HeadsetLinkEnd::Preempted;
        }
        if owns && !projecting {
            projecting = true;
            log("AA: the phone opened our Android Auto channel — holding the headset link for the session");
        } else if projecting && !owns {
            projecting = false;
            pump_logged = false;
            if setup_grace.is_some() {
                return HeadsetLinkEnd::ProjectionEnded;
            }
            log("AA: the Android Auto session ended — keeping the inbound headset link open");
        }
        if !projecting {
            if let Some(d) = setup_deadline {
                if Instant::now() >= d {
                    return HeadsetLinkEnd::SetupTimeout;
                }
            }
        }
        if projecting && !pump_logged && aa_pump_session_live() {
            pump_logged = true;
            log("AA: the pump has a live TCP session — the phone is on the AP");
        }

        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: one initialised pollfd, count 1, on an fd this function borrows for its lifetime.
        let r = unsafe { libc::poll(&mut pfd, 1, 1000) };
        if r < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return HeadsetLinkEnd::PeerClosed; // poll itself failed; the link is not usable
        }
        if r == 0 {
            continue; // idle second; the link is still up
        }
        if pfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return HeadsetLinkEnd::PeerClosed;
        }
        if pfd.revents & libc::POLLIN != 0 {
            // `impl Read for &File`, so the reader is the shared reference itself — no `&mut File`
            // is needed and the caller keeps ownership.
            let mut reader: &std::fs::File = sock;
            match reader.read(&mut buf) {
                Ok(0) => return HeadsetLinkEnd::PeerClosed,
                Ok(n) => {
                    carry.extend_from_slice(&buf[..n]);
                    // Same 8 KiB ceiling `hfp_hf`'s reader applies: a gateway streaming bytes with
                    // no terminator must not grow this buffer without bound on a box built with
                    // `panic = "abort"`.
                    if carry.len() > 8192 {
                        log("AA: unterminated gateway output exceeded 8 KiB — discarding");
                        carry.clear();
                        continue;
                    }
                    for l in split_at_lines(&mut carry) {
                        to_send.extend(on_line(&l, &mut calls, &sco, &mut narrowed));
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue
                }
                Err(_) => return HeadsetLinkEnd::PeerClosed,
            }
        }
    }
}

/// Split every COMPLETE `\r`/`\n`-terminated line out of `buf`, leaving any trailing partial in
/// place. Empty lines are dropped — AT framing is `\r\n`, so every result yields one.
fn split_at_lines(buf: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for i in 0..buf.len() {
        if buf[i] == b'\r' || buf[i] == b'\n' {
            if i > start {
                let l = String::from_utf8_lossy(&buf[start..i]).trim().to_string();
                if !l.is_empty() {
                    out.push(l);
                }
            }
            start = i + 1;
        }
    }
    buf.drain(..start);
    out
}

/// Is the wireless Android Auto byte pump actually carrying a session right now?
///
/// Read straight out of `/proc/net/tcp`: an ESTABLISHED connection whose LOCAL port is the endpoint
/// the bootstrap advertised means the phone associated and dialled it, and `aa-bridge` is pumping.
/// No new IPC and no new flag — the two processes already share the endpoint by construction
/// (`aa_wireless::DEFAULT_PORT`), which is the only thing that has to stay true for this to be
/// correct. Absent `/proc` (the macOS test host) reads as "no session", which is the safe answer.
pub(crate) fn aa_pump_session_live() -> bool {
    ["/proc/net/tcp", "/proc/net/tcp6"].iter().any(|p| {
        std::fs::read_to_string(p)
            .map(|t| has_established_on_local_port(&t, aa_wireless::DEFAULT_PORT))
            .unwrap_or(false)
    })
}

/// The pure half of [`aa_pump_session_live`]. `/proc/net/tcp` columns: `sl local_address
/// rem_address st …`, addresses `HEX:PORT` with the port in upper-case hex, `st` `01` =
/// `TCP_ESTABLISHED` (`0A` is LISTEN — the pump's own idle listener, which must NOT count).
fn has_established_on_local_port(table: &str, port: u16) -> bool {
    table.lines().skip(1).any(|line| {
        let mut f = line.split_whitespace();
        let (_sl, local, _rem, st) = match (f.next(), f.next(), f.next(), f.next()) {
            (Some(a), Some(b), Some(c), Some(d)) => (a, b, c, d),
            _ => return false,
        };
        if !st.eq_ignore_ascii_case("01") {
            return false;
        }
        local
            .rsplit_once(':')
            .and_then(|(_, p)| u16::from_str_radix(p, 16).ok())
            .map(|p| p == port)
            .unwrap_or(false)
    })
}

/// The reconnect loop. Spawns nothing. Whenever no session is live, re-reads the bond list and tries
/// each bonded phone in turn with bounded backoff, until `shutdown`. When nothing is bonded it idles
/// (re-checking every `BACKOFF_MAX_SECS`) rather than exiting, so a phone paired via the accept path
/// after boot becomes reconnect-eligible without a daemon restart (audit Fix #22).
///
/// ## Connection policy (Raspberry Pi port)
///
/// The order comes from [`Control::ordered_bonds`] — the projection app's configured
/// first-to-connect list, with any bond it does not mention appended so a newly paired phone is
/// never invisible. With `autoConnect` off the loop does **not** drive on its own; it waits for an
/// explicit request from the device screen, which is GM's "tap to connect".
///
/// An explicit request always wins, including while `autoConnect` is off and including out of
/// order: pressing Connect on a specific phone means that phone, now.
pub fn run(
    name: &str,
    shutdown: &Arc<AtomicBool>,
    session_active: &Arc<AtomicBool>,
    ctrl: &Arc<Control>,
) {
    // Settle before the first probe (bring-up/SSP/SDP, and give the phone a chance to connect IN first).
    if !interruptible_sleep(INITIAL_SETTLE_SECS, shutdown) {
        return;
    }

    let mut backoff = BACKOFF_START_SECS;
    // Log only on a change in the bonded/idle state, so the per-round re-read below doesn't spam the log.
    // `None` = nothing logged yet; `Some(true)` = had bonds; `Some(false)` = idle.
    let mut last_state: Option<bool> = None;
    let mut last_manual_log = false;
    while !shutdown.load(Ordering::Relaxed) {
        // Never fight an in-progress session (accept path or a prior reconnect). Advisory fast-path
        // only — the authoritative claim is the compare_exchange in attempt().
        if session_active.load(Ordering::Relaxed) {
            if !interruptible_sleep(BACKOFF_START_SECS, shutdown) {
                break;
            }
            backoff = BACKOFF_START_SECS; // a live session means the phone is here; retry promptly after it
            continue;
        }
        // Re-read the bond list every round (audit Fix #22): a phone paired via the accept path AFTER
        // boot must become reconnect-eligible without a daemon restart. The old code snapshotted bonds
        // once and returned early when the set was empty, so a box that booted unpaired never drove
        // reconnect for a later-paired phone. This just reads the persisted link-key file — cheap, no
        // paging, no side effects — now ordered by the app's policy.
        let bonds = ctrl.ordered_bonds();
        let has_bonds = !bonds.is_empty();
        if last_state != Some(has_bonds) {
            if has_bonds {
                log(&format!("{} bonded phone(s) — driving reconnect when idle", bonds.len()));
            } else {
                log("no bonded phones — reconnect idle (accept path handles first pairing; re-checking)");
            }
            last_state = Some(has_bonds);
        }
        if !has_bonds {
            if !interruptible_sleep(BACKOFF_MAX_SECS, shutdown) {
                break;
            }
            continue;
        }

        // An explicit request from the device screen. Consumed here, so it fires exactly once.
        let request = ctrl.take_request();
        let auto = ctrl.policy().auto_connect;

        // Tap-to-connect with nothing pending: stay idle rather than paging phones the driver did
        // not ask for. Logged once per transition — this is a configured state, not a fault.
        if !auto && request.is_none() {
            if !last_manual_log {
                log("autoConnect off — idle until the device screen asks for a phone");
                last_manual_log = true;
            }
            if !sleep_or_request(BACKOFF_START_SECS, shutdown, ctrl) {
                break;
            }
            continue;
        }
        last_manual_log = false;

        // A named request is tried alone and first. Anything else walks the policy order.
        let targets: Vec<[u8; 6]> = match request {
            Some(Some(addr)) => {
                if bonds.contains(&addr) {
                    log(&format!("explicit connect request for {}", control::fmt_addr(&addr)));
                    vec![addr]
                } else {
                    // Requesting an unbonded phone cannot work — there is no link key to offer, so
                    // the connect would fail at pairing. Say so instead of silently trying it.
                    log(&format!(
                        "connect requested for {} but it is not bonded — ignoring",
                        control::fmt_addr(&addr)
                    ));
                    Vec::new()
                }
            }
            _ => bonds.clone(),
        };

        let mut ran = false;
        let mut rejected = false;
        for &peer in &targets {
            if shutdown.load(Ordering::Relaxed) || session_active.load(Ordering::Relaxed) {
                break;
            }
            match attempt(peer, name, shutdown, session_active, ctrl) {
                Attempt::Ran => ran = true,
                Attempt::Failed => {}
                Attempt::PairRejected => {
                    rejected = true;
                    break; // do not page the next phone into the same rejection storm
                }
            }
        }
        if rejected {
            backoff = BACKOFF_MAX_SECS; // the phone said no; more paging only rotates codes
        } else if ran {
            backoff = BACKOFF_START_SECS; // reset toward prompt retry after a real attempt…
        }
        // …but ALWAYS sleep at least the current backoff (≥ BACKOFF_START_SECS) before the next attempt
        // (audit B5). `attempt()` returns true when the bt_driver was merely INVOKED, not when it reached a
        // real milestone, so a bonded phone that is RFCOMM-connectable but never completes iAP2 (accepts the
        // DLC then drops, or fails auth fast) makes each attempt return in well under a second. Without this
        // floor the old `continue` spun the loop back-to-back — pinning the single i.MX6UL core and paging
        // the phone continuously. A genuine session that just ended still retries within BACKOFF_START_SECS.
        if !sleep_or_request(backoff, shutdown, ctrl) {
            break;
        }
        if rejected {
            // The hold-off is over: drop the flag so the next attempt is judged on its own.
            let _ = std::fs::remove_file(bt_common::rfcomm_uspace::PAIR_REJECTED_FLAG);
        } else if !ran {
            backoff = (backoff * 2).min(BACKOFF_MAX_SECS);
        }
    }
    log("reconnect loop exiting");
}

#[cfg(test)]
mod tests {
    use super::{has_established_on_local_port, headset_candidates, split_at_lines};
    use crate::hfp_hf::Path;
    use crate::sdp_client::Services;

    /// The bench Pixel's own numbers: HFP AG on RFCOMM 4, HSP AG on 3. AUTO must try HFP first —
    /// that is the route the stock firmware proved against this exact phone.
    #[test]
    fn auto_tries_hfp_before_hsp() {
        let s = Services { hfp_ag: Some(4), hsp_ag: Some(3), ..Services::default() };
        assert_eq!(headset_candidates(&s, None), vec![(Path::Hfp, 4), (Path::Hsp, 3)]);
    }

    /// A forced path yields ONLY that path — never a silent fallback to the other, which would
    /// defeat the point of a lever whose job is isolating a failure to one route.
    #[test]
    fn a_forced_path_excludes_the_other_one() {
        let s = Services { hfp_ag: Some(4), hsp_ag: Some(3), ..Services::default() };
        assert_eq!(headset_candidates(&s, Some(Path::Hfp)), vec![(Path::Hfp, 4)]);
        assert_eq!(headset_candidates(&s, Some(Path::Hsp)), vec![(Path::Hsp, 3)]);
    }

    /// Forcing a path the peer does not expose must yield nothing, not the other one.
    #[test]
    fn forcing_a_path_the_peer_lacks_yields_nothing() {
        let hsp_only = Services { hsp_ag: Some(3), ..Services::default() };
        assert!(headset_candidates(&hsp_only, Some(Path::Hfp)).is_empty());
        assert_eq!(headset_candidates(&hsp_only, None), vec![(Path::Hsp, 3)]);
        let hfp_only = Services { hfp_ag: Some(4), ..Services::default() };
        assert!(headset_candidates(&hfp_only, Some(Path::Hsp)).is_empty());
        assert_eq!(headset_candidates(&hfp_only, None), vec![(Path::Hfp, 4)]);
    }

    /// An iAP2 or AA-wireless hit is not an audio gateway; neither may produce a candidate.
    #[test]
    fn only_gateway_records_produce_candidates() {
        let s = Services { iap2: Some(1), aawg: Some(8), ..Services::default() };
        assert!(headset_candidates(&s, None).is_empty());
        assert!(headset_candidates(&Services::default(), None).is_empty());
    }

    /// The hold loop's line splitter must keep a partial line in the buffer rather than logging a
    /// truncated one — an AG can and does split a result across two RFCOMM frames.
    #[test]
    fn the_drain_splitter_keeps_partial_lines_for_the_next_read() {
        let mut buf = b"\r\n+CIEV: 6,4\r\n\r\n+BSI".to_vec();
        assert_eq!(split_at_lines(&mut buf), vec!["+CIEV: 6,4".to_string()]);
        assert_eq!(buf, b"+BSI".to_vec());
        buf.extend_from_slice(b"R: 1\r\n");
        assert_eq!(split_at_lines(&mut buf), vec!["+BSIR: 1".to_string()]);
        assert!(buf.is_empty());
    }

    /// Empty lines (every `\r\n` pair produces one) must never be logged, and a buffer with no
    /// terminator at all must yield nothing and be left intact.
    #[test]
    fn the_drain_splitter_drops_empties_and_holds_unterminated_input() {
        let mut buf = b"\r\n\r\n\r\n".to_vec();
        assert!(split_at_lines(&mut buf).is_empty());
        assert!(buf.is_empty());
        let mut buf = b"OK".to_vec();
        assert!(split_at_lines(&mut buf).is_empty());
        assert_eq!(buf, b"OK".to_vec());
    }

    /// Realistic `/proc/net/tcp`, with the three rows that matter: the pump's own LISTEN socket on
    /// the endpoint (must NOT count — it is there whenever the process is), an ESTABLISHED
    /// connection on it (must count), and unrelated traffic.
    const TABLE: &str = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 012BA8C0:14A8 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1234 1 0000 100 0 0 10 0
   1: 0100007F:149D 0100007F:C350 01 00000000:00000000 00:00000000 00000000     0        0 1235 1 0000 100 0 0 10 0
";

    /// 0x14A8 == 5288 == `aa_wireless::DEFAULT_PORT`; 0x149D == 5277 is the app-side pump port.
    #[test]
    fn a_listening_socket_on_the_endpoint_is_not_a_session() {
        assert!(!has_established_on_local_port(TABLE, 5288));
    }

    #[test]
    fn an_established_connection_on_the_endpoint_is_a_session() {
        let t = format!(
            "{TABLE}   2: 012BA8C0:14A8 022BA8C0:D431 01 00000000:00000000 00:00000000 00000000     0        0 1236 1 0000 100 0 0 10 0\n"
        );
        assert!(has_established_on_local_port(&t, 5288));
    }

    /// The REMOTE port must never be mistaken for the local one: a phone that happens to source
    /// from 5288 to some other service would otherwise read as a live pump session.
    #[test]
    fn a_remote_port_match_does_not_count() {
        let t = "  sl  local_address rem_address   st\n   0: 012BA8C0:1F90 022BA8C0:14A8 01 x\n";
        assert!(!has_established_on_local_port(t, 5288));
    }

    #[test]
    fn other_established_connections_do_not_count() {
        assert!(!has_established_on_local_port(TABLE, 5288));
        // 0x149D == 5277 IS established in the table above, so the parser is not simply saying no.
        assert!(has_established_on_local_port(TABLE, 5277));
    }

    /// A truncated or otherwise unexpected table must read as "no session", never panic: this is
    /// parsed on a live box on every bootstrap teardown, and `panic = "abort"` would take a running
    /// CarPlay session down with it.
    #[test]
    fn a_malformed_table_is_no_session_and_never_panics() {
        for t in ["", "header only\n", "h\n   0:\n", "h\n   0: nocolon 00:00 01\n", "h\n   0: AB:ZZZZ x 01\n"] {
            assert!(!has_established_on_local_port(t, 5288), "table {t:?}");
        }
    }
}
