//! carplay-wireless — wireless CarPlay bring-up (ported from the carplayd PoC, iPhone-verified
//! Phase A1+A2). See `docs/wireless/00_WIRELESS_CARPLAY.md`.
//!
//! A separate process/binary, matching the project's strict-modularity rule -- a wireless-stack bug
//! must never destabilize the proven wired OCBM path. Coordinates with the wired session owner via
//! the session arbiter (`/run/carplay/arbiter.sock`) so only one transport is ever an active
//! connection target: claims `wireless`, brings Bluetooth up while held, and goes quiet (not powers
//! off) the moment a wired session preempts it.
//!
//! PORT NOTE (ccpa_custom): the arbiter SERVER doesn't exist yet (the box's wired supervisor is the
//! shell `session_supervisor.sh`, not a Rust `carplayd`). Until it does, an ABSENT arbiter socket is
//! treated as "granted, standalone" so the Bluetooth layer is testable now; the real arbiter wires in
//! with the dual-transport supervisor (docs/wireless/00_WIRELESS_CARPLAY.md).
//!
//! Phase A1 = Bluetooth discoverable + Just-Works pairing + SDP. Phase A2 = the RFCOMM listener +
//! iAP2 handshake (`rfcomm.rs`/`bt_driver.rs`) through `IdentifyAccept`. Phase A3 (`wifi_handoff.rs`)
//! adds the WiFi-credential message codecs + AP hosting.

mod arbiter_client;
mod av;
mod box_identity;
mod bt_bringup;
mod bt_driver;
mod cloexec;
mod control;
mod hci;
mod mfi_local;
mod reconnect;
mod rfcomm;
mod rfcomm_uspace;
mod sdp_client;
mod sdp_server;
mod ssp_agent;
mod wifi_handoff;

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const HCI_DEV: &str = "hci0";
const CONTROLLER_INDEX: u16 = 0;
const RFCOMM_CHANNEL: u8 = 1; // matches the reference project's default CARPLAY_RFCOMM_CHAN
                              // Brand for the advertised name; the actual name is per-device (e.g. "CarLink-b0df") via
                              // `carplay_iap2_core::message::accessory_name` — the SAME suffix as the Wi-Fi SSID + wired iAP2 identity,
                              // so a box shows one distinct name on every transport (multiple boxes stop collapsing into one iOS car).
const ACCESSORY_BRAND: &str = "CarLink";
const ARBITER_SOCK: &str = "/run/carplay/arbiter.sock";

fn log(m: &str) {
    println!("[wireless] {m}");
}

/// Dispatch to whichever RFCOMM implementation this host can use. Both have the identical
/// signature, including `Ok(None)` on timeout, so the caller is unchanged.
/// Outbound counterpart of [`rfcomm_accept`], used by `reconnect` for an already-bonded phone.
fn rfcomm_connect(
    peer: [u8; 6],
    channel: u8,
    timeout_secs: i64,
) -> std::io::Result<std::fs::File> {
    if rfcomm_uspace::selected() {
        rfcomm_uspace::connect_to(peer, channel, timeout_secs)
    } else {
        rfcomm::connect_to(peer, channel, timeout_secs)
    }
}

fn rfcomm_accept(
    channel: u8,
    shutdown: &AtomicBool,
) -> std::io::Result<Option<std::fs::File>> {
    if rfcomm_uspace::selected() {
        rfcomm_uspace::accept_one(channel, shutdown)
    } else {
        rfcomm::accept_one(channel, shutdown)
    }
}

/// Run one active wireless session: bring Bluetooth up discoverable, serve SSP + SDP + RFCOMM/iAP2,
/// and hold until either the arbiter preempts us (wired claimed) or SIGTERM. `arbiter` is the claim
/// connection when a real arbiter granted us; `None` in standalone mode (no arbiter server present).
fn run_active_session(
    shutdown: &Arc<AtomicBool>,
    arbiter: Option<BufReader<UnixStream>>,
    ctrl: &Arc<control::Control>,
    session_active: &Arc<AtomicBool>,
) {
    // Per-device name derived once per session (reads the Wi-Fi MAC / SoC serial). Used for the EIR
    // Complete Local Name (what the iPhone shows) and the wireless iAP2 identify.
    let accessory_name = carplay_iap2_core::message::accessory_name(ACCESSORY_BRAND);
    if let Err(e) = bt_bringup::bring_up(HCI_DEV, &accessory_name) {
        log(&format!("Bluetooth bring-up failed: {e} -- retrying"));
        // Chunked so SIGTERM isn't stuck behind the full 5s retry delay.
        for _ in 0..5 {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
        return;
    }
    log(&format!(
        "discoverable as \"{accessory_name}\" -- waiting for pairing or preempt"
    ));

    let ssp_shutdown = Arc::new(AtomicBool::new(false));
    let t = ssp_shutdown.clone();
    let ssp_handle = thread::spawn(move || {
        if let Err(e) = ssp_agent::run(CONTROLLER_INDEX, &t) {
            log(&format!("SSP agent exited: {e}"));
        }
    });

    // Required, not optional: without an SDP responder the iPhone completes Bluetooth pairing, then
    // immediately fails to find the iAP2 service over SDP and disconnects -- device-confirmed by the
    // PoC.
    let sdp_shutdown = Arc::new(AtomicBool::new(false));
    let t = sdp_shutdown.clone();
    let sdp_handle = thread::spawn(move || {
        if let Err(e) = sdp_server::run(RFCOMM_CHANNEL, &t) {
            log(&format!("SDP server exited: {e}"));
        }
    });

    // One transport is ever an active connection target at a time. `session_active` lets the
    // additive Model-B reconnect path (below) stand down whenever the accept path — or a prior
    // reconnect — already owns a live iAP2 session, so the two never fight over the phone.
    // `session_active` and `ctrl` are owned by main() and passed in — both are PROCESS-lifetime, not
    // per-session. See control::serve for why a per-session Control orphaned itself.
    // Belt and braces after an abnormal previous session: nothing should be claimed at entry.
    ctrl.set_session_peer(None);

    let rfcomm_shutdown = Arc::new(AtomicBool::new(false));
    let t = rfcomm_shutdown.clone();
    let rfcomm_name = accessory_name.clone(); // moved into the accept thread for the iAP2 identify
    let accept_active = session_active.clone();
    let accept_ctrl = ctrl.clone();
    let rfcomm_handle = thread::spawn(move || loop {
        if t.load(Ordering::Relaxed) {
            break;
        }
        match rfcomm_accept(RFCOMM_CHANNEL, &t) {
            Ok(Some(sock)) => {
                log("RFCOMM client connected -- starting iAP2 handshake");
                // Claim the single-session slot ATOMICALLY (compare_exchange, not a plain store) so the
                // accept path and the Model-B reconnect path can never run two concurrent bt_driver::run
                // sessions against the same phone, and neither clobbers the other's flag. If a reconnect
                // attempt already owns the slot, drop this inbound connect and let the phone retry once the
                // live session ends.
                // `_claim` also publishes the session to the device screen and releases BOTH the
                // flag and the peer on every exit from this block.
                //
                // NB `_claim`, not `_`: `let _ = ...` drops the guard IMMEDIATELY and would release
                // the claim before the driver even starts.
                let Some(_claim) =
                    control::SessionClaim::try_claim(&accept_active, &accept_ctrl, None)
                else {
                    log("session already active (reconnect in progress) -- dropping inbound connect");
                    continue;
                };
                // The peer bdaddr is not published here: the accept path is handed an already-open
                // socket. `status.active` is true regardless, which is what the app gates on — and
                // getting THAT wrong is what made the device screen offer Forget for the phone that
                // was actively projecting.
                bt_driver::run(sock, &rfcomm_name, &t);
                log("RFCOMM session ended");
            }
            Ok(None) => break, // shutdown
            Err(e) => {
                log(&format!("RFCOMM accept error: {e} -- retrying"));
                thread::sleep(Duration::from_secs(1));
            }
        }
    });

    // Model-B accessory-initiated reconnect (docs/wireless/01_BT_AND_RADIO.md): drives a bonded phone back to CarPlay on boot
    // with no user interaction. When nothing is bonded it idles (re-checking the bond list on a slow
    // interval), so a phone paired after boot becomes reconnect-eligible without a restart (audit Fix
    // #22). Its own shutdown flag (set in the go-quiet block) stops it on preempt too, not only SIGTERM.
    let reconnect_shutdown = Arc::new(AtomicBool::new(false));
    let rc_shutdown = reconnect_shutdown.clone();
    let rc_active = session_active.clone();
    let rc_name = accessory_name.clone();
    let rc_ctrl = ctrl.clone();
    let reconnect_handle = thread::spawn(move || {
        reconnect::run(&rc_name, &rc_shutdown, &rc_active, &rc_ctrl);
    });

    match arbiter {
        Some(mut reader) => arbiter_client::wait_for_preempt(&mut reader, shutdown),
        None => {
            // Standalone: no arbiter to preempt us -- hold until SIGTERM.
            while !shutdown.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs(1));
            }
        }
    }

    log("going quiet");
    ssp_shutdown.store(true, Ordering::Relaxed);
    sdp_shutdown.store(true, Ordering::Relaxed);
    rfcomm_shutdown.store(true, Ordering::Relaxed);
    reconnect_shutdown.store(true, Ordering::Relaxed);
    if let Err(e) = bt_bringup::go_quiet(HCI_DEV) {
        log(&format!("go_quiet failed (non-fatal): {e}"));
    }
    // Ordering is load-bearing (audit #3, a re-occurrence of the #106 leak): JOIN the RFCOMM producer
    // thread BEFORE tearing down the A/V layer. bt_driver::run can be mid-handshake handling a 0x5702
    // and call av::ensure_av_layer() (which spawns airplayd + rx-connect) with no abort re-check between
    // the reply write and the spawn. If we teardown_av_layer() first and THEN join, that in-flight
    // handshake re-spawns the children right after the pkill -> orphaned rx-connect advertises a dead
    // :5000 (phone keeps dialing a connection-refused receiver) and /tmp/carplay_transport sticks at
    // "wireless" (wired stays suppressed). rfcomm_shutdown was set above; accept_one + bt_driver::run
    // both observe it and return, so this join completes.
    let _ = rfcomm_handle.join();
    // Same #106 discipline as the RFCOMM accept producer: reconnect::attempt can be mid
    // bt_driver::run → av::ensure_av_layer (spawning airplayd + rx-connect), so it must be joined
    // BEFORE teardown_av_layer too, or an in-flight reconnect re-spawns the children right after the
    // pkill. reconnect_shutdown was set above; the loop and bt_driver::run both observe it.
    let _ = reconnect_handle.join();
    let _ = ssp_handle.join();
    let _ = sdp_handle.join();
    // Now that no producer thread can spawn them, reap the airplayd + rx-connect this crate started
    // (#106): a clean preempt/shutdown must not leave a dead-:5000 advertiser or a stuck transport flag.
    av::teardown_av_layer();
    // And un-latch the phase mirror (audit 3.3), same reason as bt_driver's exit funnel: on a preempt
    // or go-quiet `bt_driver::run` may never have been entered, so its own idle publish never fires.
    bt_driver::publish_bt_phase(ocbm_proto::BTP_IDLE);
}

fn main() {
    let shutdown = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, shutdown.clone())
        .expect("register SIGTERM handler");

    // (#63 child-reaping is handled by double-forking the detached daemons in av::spawn_detached so they
    // reparent to init — NOT by SIGCHLD=SIG_IGN, which would make every Command::status() in this process
    // return ECHILD and break bt_bringup + av::running(). See spawn_detached.)

    log("starting -- wireless CarPlay (Phase A1: Bluetooth discoverable + pairing + SDP)");

    // A fresh process asserts idle (audit 3.3): `/tmp/bt_phase` survives a crash/restart of this
    // daemon, so without this the previous process's deepest phase would be read as ours.
    bt_driver::publish_bt_phase(ocbm_proto::BTP_IDLE);

    // Logged once per streak, not every retry -- wired can plausibly stay active a long time and a
    // line every 3s for hours would just be journal noise.
    let mut denied_logged = false;
    let mut standalone_logged = false;

    // ⚠️ PI-VERIFIED ONLY (2026-08-16). This block and the SessionClaim guard below replaced a
    // per-session Control plus a hand-rolled compare_exchange/store pair. It touches the path that
    // decides whether an INBOUND CONNECT IS ACCEPTED, which is the CCPA's primary path — and it has
    // only been run on the Pi. 64 unit tests pass; a paired phone on a CCPA is a different claim.
    //
    // PROCESS-lifetime, deliberately outside the claim loop below.
    //
    // `run_active_session` is re-entered on every arbiter preempt/re-claim. Building these per
    // entry meant the previous control listener kept the port, the new bind failed EADDRINUSE, and
    // every device-management request thereafter mutated an orphaned Control while still answering
    // {"ok":true}. Binding here also means the device screen works during bring-up and while the
    // WIRED transport holds the arbiter — which is exactly when a driver looks at it.
    let session_active = Arc::new(AtomicBool::new(false));
    let ctrl = Arc::new(control::Control::new(session_active.clone()));
    control::serve(ctrl.clone());

    while !shutdown.load(Ordering::Relaxed) {
        match arbiter_client::try_claim(ARBITER_SOCK) {
            Ok(arbiter_client::ClaimResult::Granted(reader)) => {
                denied_logged = false;
                run_active_session(&shutdown, Some(reader), &ctrl, &session_active);
            }
            Ok(arbiter_client::ClaimResult::GrantedStandalone) => {
                if !standalone_logged {
                    log("no arbiter present -- running standalone (single-transport) until it is wired in");
                    standalone_logged = true;
                }
                run_active_session(&shutdown, None, &ctrl, &session_active);
            }
            Ok(arbiter_client::ClaimResult::Denied) => {
                if !denied_logged {
                    log("wired is active -- waiting for it to end before going discoverable");
                    denied_logged = true;
                }
                thread::sleep(Duration::from_secs(3));
            }
            Err(e) => {
                log(&format!("arbiter connect failed: {e} -- retrying"));
                thread::sleep(Duration::from_secs(3));
            }
        }
    }
    log("SIGTERM received -- exiting");
}
