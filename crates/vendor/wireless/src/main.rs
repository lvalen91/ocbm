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
mod control;
mod hfp_hf;
mod mfi_local;
mod reconnect;
mod sco_audio;
mod sdp_client;
mod wifi_handoff;

// The Bluetooth primitives moved to `bt-common` (2026-09-01) so wireless Android Auto can use the
// same radio machinery instead of a second copy. Re-exported at the crate root, so every existing
// `crate::hci::…` / `crate::ssp_agent::…` path in the modules above resolves exactly as before and
// this extraction stays a move rather than a rewrite.
pub use bt_common::{cloexec, hci, rfcomm, rfcomm_uspace, sdp_server, ssp_agent};

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const HCI_DEV: &str = "hci0";
const CONTROLLER_INDEX: u16 = 0;
const RFCOMM_CHANNEL: u8 = 1; // matches the reference project's default CARPLAY_RFCOMM_CHAN
/// Android Auto's RFCOMM server channel. 4 is what the stock CCPA's own `bluetoothDaemon` allocates
/// (iAP2 = 1, NearBy = 2, HiChain = 3, AAP = 4), recovered from its per-service record builders.
/// The number is not load-bearing — the phone reads it out of our SDP record, and openauto works on
/// a kernel-allocated one — but matching stock costs nothing and removes a variable.
const AA_RFCOMM_CHANNEL: u8 = bt_common::sdp_record::AAP_RFCOMM_CHANNEL;
/// The two headset-side channels we serve so a phone's `PhonePolicy` can auto-connect to us. Stock
/// serves neither — its `hfpd` only ever dials the phone — so these numbers are ours to pick, and
/// they are the first two free slots after stock's allocation. See `hfp_hf` for why we advertise
/// both profiles.
const HFP_HF_RFCOMM_CHANNEL: u8 = bt_common::sdp_record::HFP_HF_RFCOMM_CHANNEL;
const HSP_HS_RFCOMM_CHANNEL: u8 = bt_common::sdp_record::HSP_HS_RFCOMM_CHANNEL;
// Brand for the advertised name; the actual name is per-device (e.g. "CarLink-b0df") via
// `carplay_iap2_core::message::accessory_name` — the SAME suffix as the Wi-Fi SSID + wired iAP2 identity,
// so a box shows one distinct name on every transport (multiple boxes stop collapsing into one iOS car).
const ACCESSORY_BRAND: &str = "CarLink";
const ARBITER_SOCK: &str = "/run/carplay/arbiter.sock";

/// `@<unix_ms> ` write-time stamp (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG): the box.log tailer
/// parses this prefix and uses it instead of the millisecond it happened to READ the line at.
fn log(m: &str) {
    println!("@{} [wireless] {m}", now_ms());
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Log `/tmp/radio_caps` once at start — chipset/driver identity is exactly the "which radio is
/// this box" fact a troubleshooting session needs, and `radio_detect.sh` emits it as a flat
/// `KEY=value` file (see `ccpa/rootfs/script/radio_detect.sh`). `RADIO_WLAN_MAC` / `RADIO_BT_MAC`
/// are real MACs, so those two values are redacted — no PII in the log stream — everything else
/// (chip, fw tree, insmod lines, interface names) is not.
fn log_radio_caps() {
    match std::fs::read_to_string("/tmp/radio_caps") {
        Ok(s) => {
            let redacted: Vec<String> = s
                .lines()
                .map(|l| match l.split_once('=') {
                    Some((k, _)) if k.ends_with("_MAC") => format!("{k}=<redacted>"),
                    _ => l.to_string(),
                })
                .collect();
            log(&format!("radio_caps: {}", redacted.join("; ")));
        }
        Err(_) => log("radio_caps: absent"),
    }
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

/// The AP details Android Auto must hand the phone, sourced from the AP that is actually running.
///
/// Not from environment variables. `WifiInfoResponse` describes the network the phone is about to
/// join, and `wifi_handoff`'s own note says it plainly: it MUST describe the AP that is actually
/// running, or the phone leaves Bluetooth and never arrives. So the SSID/passphrase come from the
/// same `hostapd.conf` the AP was raised from — exactly as CarPlay's own 0x5703 handoff does — and
/// the BSSID from the live interface.
///
/// The interface is read from `/tmp/radio_caps` (`RADIO_WLAN_MAC`), never hardcoded: the interface
/// name is an insmod parameter on this hardware, not a constant, and the chipset varies per unit.
fn aa_ap_params() -> aa_wireless::wpp::ApParams {
    // An unreadable AP config leaves the SSID empty, which `credentials_are_sendable` refuses --
    // the AA bootstrap must not invent an AP either.
    let ap = wifi_handoff::read_hostapd_ap_config();
    let bssid = std::fs::read_to_string("/tmp/radio_caps")
        .ok()
        .and_then(|caps| {
            caps.lines()
                .find_map(|l| l.strip_prefix("RADIO_WLAN_MAC=").map(|v| v.trim().to_string()))
        })
        .unwrap_or_default()
        .to_uppercase();

    aa_wireless::wpp::ApParams {
        ssid: ap.as_ref().map(|a| a.ssid.clone()).unwrap_or_default(),
        passphrase: ap.and_then(|a| a.passphrase).unwrap_or_default(),
        bssid,
        // 8 == WPA2_ENTERPRISE in `WifiSecurityMode`, the enum this field is typed with. Field-proven:
        // the stock box's own working AA session reports `securityMode: 8`. Not 24 — that is a value
        // from a different, superseded enum, and would make the phone drop the whole message.
        // FIELD-PROVEN 8 = WPA2_PERSONAL (stock box capture + the 24/WPA2_ENTERPRISE attempt that
        // never associated) — see ccpa/aa-wireless/src/proto.rs and docs/androidauto/03_WIRELESS.md.
        security_mode: aa_wireless::proto::SecurityMode::WPA2_PERSONAL,
        access_point_type: aa_wireless::proto::AccessPointType::STATIC,
        ip_address: crate::av::AP_IP.to_string(),
        port: aa_wireless::DEFAULT_PORT,
    }
}

/// Drive one Android Auto bootstrap over an already-connected RFCOMM socket.
///
/// Shared by BOTH directions, which is the whole reason it takes a borrowed socket and returns the
/// `Outcome` instead of owning and discarding them:
///   * the channel-4 ACCEPT thread below — the phone dialled us. THE direction: gearhead is the
///     client of `4de17a00-…` and opens this channel once its headset gate is satisfied
///     (docs/androidauto/03_WIRELESS.md §2f).
///   * any future caller that has an AA RFCOMM socket from somewhere else. The borrowed socket and
///     returned `Outcome` are what keep this reusable — a caller that needs the link alive past
///     `Established` can hold it, which it could not if this function consumed it.
///
/// `None` means the exchange never started (unusable AP credentials, or another projection owns the
/// box) and the reason has already been logged.
/// Keep the phone's Android Auto RFCOMM channel open after a successful bootstrap. Polls the fd at
/// 1 Hz so shutdown and the owner flag are observed; frames the phone sends post-bootstrap
/// (WifiConnectionStatus, pings) are drained and logged, never answered here. Releases the owner
/// claim only if the pump has no live TCP session when the link ends (otherwise the pump owns it).
fn hold_aa_channel(sock: &std::fs::File, shutdown: &AtomicBool) {
    use std::os::unix::io::AsRawFd;
    let fd = sock.as_raw_fd();
    let started = std::time::Instant::now();
    let mut buf = [0u8; 512];
    let reason = loop {
        if shutdown.load(Ordering::Relaxed) {
            break "daemon shutting down";
        }
        if box_common::flags::owner() != box_common::flags::ProjectionOwner::WirelessAa {
            break "owner flag changed";
        }
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let n = unsafe { libc::poll(&mut pfd, 1, 1000) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break "poll error";
        }
        if n == 0 {
            continue;
        }
        let r = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if r == 0 {
            break "phone closed the channel";
        }
        if r < 0 {
            let e = std::io::Error::last_os_error();
            if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) {
                continue;
            }
            break "read error";
        }
        let n = r as usize;
        log(&format!(
            "AA: {n} post-bootstrap byte(s) on the Android Auto channel (id={}) — drained",
            buf.get(2).map_or("?".to_string(), |b| b.to_string())
        ));
    };
    let held = started.elapsed().as_secs();
    log(&format!("AA: Android Auto channel held {held}s — {reason}"));
    if !reconnect::aa_pump_session_live() {
        aa_wireless::release_owner_if_ours();
        log("AA: no live pump session — released the wireless-aa claim");
    }
}

fn run_aa_bootstrap(sock: &mut std::fs::File) -> Option<aa_wireless::Outcome> {
    let params = aa_ap_params();
    aa_wireless::log_params(&params);
    if let Err(why) = aa_wireless::credentials_are_sendable(&params) {
        log(&format!("AA: refusing to serve -- {why}"));
        return None;
    }
    // Publish which projection owns the box, for ocbmd and the supervisor. Claimed HERE, after the
    // phone has actually opened the AA channel, rather than up front: the SDP records and both
    // accept loops are always live, so nothing waits on this flag to make progress, and claiming it
    // early would make an idle box look busy to the wired arms.
    //
    // (This does not contradict aa-bridge's "claim before serving" lesson. That deadlock existed
    // because the host app waited to see PM_WIRED_AA before it would connect, so a late claim meant
    // neither side moved. Nothing here waits on the flag.)
    //
    // `claim_owner` also stands down if another transport already owns the box — first-come-wins,
    // and the release is ours-only so we can never delete someone else's claim.
    if !aa_wireless::claim_owner() {
        return None;
    }
    // Release the claim on every outcome EXCEPT `Established`. After a successful bootstrap the
    // phone has the credentials and has not associated yet, so the box is still committed to this
    // projection and releasing here would let another transport claim it out from under the phone
    // mid-handoff. The hold is bounded: `run_active_session` releases on its way out.
    //
    // The `Established` arm deliberately does NOT release and the caller decides what to do next —
    // the accept path drops the socket once the phone has the credentials.
    match aa_wireless::run_bootstrap(sock, params) {
        Ok(o) if o.is_established() => {
            log("AA: bootstrap established -- holding the owner claim");
            Some(o)
        }
        Ok(o) => {
            log(&format!("AA: bootstrap ended without association ({o:?})"));
            aa_wireless::release_owner_if_ours();
            Some(o)
        }
        Err(e) => {
            log(&format!("AA: bootstrap error: {e}"));
            aa_wireless::release_owner_if_ours();
            None
        }
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
    // The head unit's Numeric-Comparison yes/no. Owned by the PROCESS-lifetime `Control` (the
    // control port is bound before the claim loop), cloned into each session's agent thread.
    let pair_answer = ctrl.pair_answer();
    let ssp_handle = thread::spawn(move || {
        // Restart on error, like both accept loops below. `run` returns `Ok(())` on the shutdown
        // flag, so an `Err` is always a transient failure (the mgmt socket right after bring_up's
        // DOWN->UP cycle, say) -- and standalone mode holds this session until SIGTERM, so a thread
        // that exits leaves the box discoverable but unable to pair for the rest of the session.
        while !t.load(Ordering::Relaxed) {
            let on_pair_rejected = || bt_driver::publish_bt_phase(ocbm_proto::BTP_PAIR_REJECTED);
            match ssp_agent::run(CONTROLLER_INDEX, &t, Some(&on_pair_rejected), Some(&pair_answer)) {
                Ok(()) => break,
                Err(e) => {
                    log(&format!("SSP agent exited: {e} -- restarting"));
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
    });

    // Required, not optional: without an SDP responder the iPhone completes Bluetooth pairing, then
    // immediately fails to find the iAP2 service over SDP and disconnects -- device-confirmed by the
    // PoC.
    let sdp_shutdown = Arc::new(AtomicBool::new(false));
    let t = sdp_shutdown.clone();
    let sdp_handle = thread::spawn(move || {
        // ONE SDP server advertising every service from this single Bluetooth identity,
        // which is what the stock CCPA does and what the phone requires: L2CAP PSM 0x0001 has one
        // holder, so a second daemon advertising Android Auto separately would just get EADDRINUSE
        // and never be discoverable. The phone tells them apart by the RFCOMM channel it opens —
        // iAP2 on 1, Android Auto on 4, HFP hands-free on 5, HSP headset on 6
        // (docs/androidauto/03_WIRELESS.md §2b).
        //
        // Restarted on error for the same reason as the SSP agent above: `run_services` returns
        // `Ok(())` on the shutdown flag, and without an SDP responder a paired phone disconnects.
        while !t.load(Ordering::Relaxed) {
            let services = vec![
                sdp_server::iap2_service(RFCOMM_CHANNEL),
                sdp_server::android_auto_service(AA_RFCOMM_CHANNEL),
                // The two headset-class records. gearhead will not start wireless setup unless the
                // phone's `BluetoothProfile.HEADSET` reports us connected, and a phone whose
                // `PhonePolicy` auto-connects to a bonded headset needs a record to find first.
                // Both profiles, because AOSP reaches that state by different routes for each and
                // only the HFP one needs an AT dialogue (`hfp_hf`, docs/androidauto/03_WIRELESS.md
                // §6b).
                sdp_server::hfp_hf_service(HFP_HF_RFCOMM_CHANNEL),
                sdp_server::hsp_hs_service(HSP_HS_RFCOMM_CHANNEL),
            ];
            match sdp_server::run_services(services, &t) {
                Ok(()) => break,
                Err(e) => {
                    log(&format!("SDP server exited: {e} -- restarting"));
                    thread::sleep(Duration::from_secs(1));
                }
            }
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

    // ---- Android Auto: its own accept loop on its own channel -----------------------------------
    //
    // A SEPARATE thread with its own `accept_one`, deliberately NOT a shared poll over both
    // channels: the kernel's RFCOMM multiplexes DLCs over one L2CAP session itself, so two
    // independent blocking accepts cannot starve each other, and this design touches not one line
    // of the channel-1 path that wireless CarPlay is proven on.
    //
    // Both loops contend for the SAME `session_active` slot through `SessionClaim`. That is the
    // box's first-come-first-served rule (docs/androidauto/02_ARBITRATION.md §0) expressed in one
    // compare_exchange: whichever phone connects first owns the box, the other is dropped and
    // retries. No preemption in either direction.
    let aa_shutdown = Arc::new(AtomicBool::new(false));
    let t = aa_shutdown.clone();
    let aa_active = session_active.clone();
    let aa_ctrl = ctrl.clone();
    let aa_handle = thread::spawn(move || {
        // KERNEL BACKEND ONLY. `rfcomm_uspace`'s `open_dlc` returns on the first SABM matching its
        // one channel and sends DM to any other — so a second accept loop there would actively
        // reject CarPlay's DLC on a shared session, roughly half the time. That backend is opt-in
        // (the Pi/AAOS port); refuse rather than regress it.
        if rfcomm_uspace::selected() {
            log("Android Auto: userspace RFCOMM backend cannot serve a second channel -- AA disabled");
            return;
        }
        loop {
            if t.load(Ordering::Relaxed) {
                break;
            }
            match rfcomm_accept(AA_RFCOMM_CHANNEL, &t) {
                Ok(Some(mut sock)) => {
                    let Some(_claim) =
                        control::SessionClaim::try_claim(&aa_active, &aa_ctrl, None)
                    else {
                        log("AA: session already active -- dropping inbound connect");
                        continue;
                    };
                    // THE PROVEN DIRECTION, corrected 2026-09-04 (second pass). gearhead is the
                    // CLIENT of the Android Auto wireless UUID — `ojk.java:31-35` calls
                    // `createRfcommSocketToServiceRecord(4de17a00-…)` — and it opens THIS channel
                    // once, and only once, the phone's own `BluetoothProfile.HEADSET` reports the
                    // head unit connected. Stock shows the same order: the SLC completes and the
                    // phone opens the AAP channel 26 ms later
                    // (`aa_full_session_adapter_20260315.txt:442-607`). What raises that headset
                    // link on our side is `reconnect::attempt_headset`; an earlier pass had this
                    // loop marked as the exception and dialled the phone instead, which could never
                    // work — the phone hosts no such record.
                    log("AA: RFCOMM client connected on channel 4 -- starting wireless bootstrap (the phone dialled us, as it does once the headset gate opens)");
                    match run_aa_bootstrap(&mut sock) {
                        Some(o) if o.is_established() => {
                            // HOLD the channel for the whole session (device-proven 2026-09-04):
                            // dropping it right after `Established` made gearhead treat the head
                            // unit as gone — it closed the TCP session 1.5 s after the first IDR,
                            // re-dialled this channel and re-ran the bootstrap in a ~1 s loop (117×),
                            // which is the "Connecting to Android Auto" overlay flapping on the
                            // phone. Stock keeps its AAP service alive for the drive. Drain and log
                            // anything the phone sends; leave when the phone closes it, the owner
                            // flag stops being ours, or we shut down.
                            hold_aa_channel(&sock, &t);
                        }
                        _ => {}
                    }
                    log("AA: bootstrap ended");
                }
                Ok(None) => break, // shutdown
                Err(e) => {
                    log(&format!("AA: RFCOMM accept error: {e} -- retrying"));
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
    });

    // ---- Headset gate: inbound. -----------------------------------------------------------------
    //
    // A phone whose `PhonePolicy` auto-connects HFP or HSP to a bonded headset-class device will
    // dial the records we now advertise instead of waiting to be dialled. Serve both, so the gate
    // opens whichever way round the phone chooses.
    //
    // The HFP arm still runs the FULL hands-free dialogue over the accepted socket: in HFP the
    // hands-free unit sends `AT+BRSF` first regardless of who opened the RFCOMM channel, and the
    // gateway sits waiting for it. The HSP arm says nothing at all — AOSP opens the service level on
    // the connection itself (`bta_ag_act.cc:533-540`).
    //
    // Neither takes the single-session claim. A headset link is not a projection session; the claim
    // is taken by `run_aa_bootstrap` when the phone subsequently opens channel 4, and taking it here
    // would lock CarPlay out of a box that is merely holding a Bluetooth link open.
    let headset_shutdown = Arc::new(AtomicBool::new(false));
    let mut headset_handles = Vec::with_capacity(2);
    for (channel, path) in [
        (HFP_HF_RFCOMM_CHANNEL, hfp_hf::Path::Hfp),
        (HSP_HS_RFCOMM_CHANNEL, hfp_hf::Path::Hsp),
    ] {
        let t = headset_shutdown.clone();
        headset_handles.push(thread::spawn(move || {
            // KERNEL BACKEND ONLY, for the same reason the Android Auto accept loop refuses:
            // `rfcomm_uspace::open_dlc` returns on the first SABM matching its one channel and
            // sends DM to any other, so a second accept loop there would actively reject CarPlay's
            // DLC on a shared session. That backend is the opt-in Pi/AAOS port; refuse rather than
            // regress it.
            if rfcomm_uspace::selected() {
                log(&format!(
                    "{}: userspace RFCOMM backend cannot serve a second channel -- inbound headset disabled",
                    path.as_str()
                ));
                return;
            }
            loop {
                if t.load(Ordering::Relaxed) {
                    break;
                }
                match rfcomm_accept(channel, &t) {
                    Ok(Some(mut sock)) => {
                        log(&format!(
                            "{}: the phone connected to our headset channel {channel}",
                            path.as_str()
                        ));
                        // An ACCEPTED BR/EDR socket does not inherit the listener's SO_RCVTIMEO
                        // (the kernel's `rfcomm_sock_init` copies neither), so without this a phone
                        // that opens the channel and says nothing would park this thread in an
                        // unbounded read — and `run_active_session` joins it on the way to going
                        // quiet. See `hfp_hf::arm_socket_timeouts`.
                        if let Err(e) = hfp_hf::arm_socket_timeouts(&sock) {
                            log(&format!(
                                "{}: could not arm socket timeouts on the accepted link: {e} -- dropping it",
                                path.as_str()
                            ));
                            continue;
                        }
                        let up = match path {
                            hfp_hf::Path::Hsp => {
                                log("AA: HSP headset link up with the phone (no AT dialogue, inbound) — waiting for it to open our Android Auto channel");
                                hfp_hf::establish_hsp()
                            }
                            hfp_hf::Path::Hfp => match hfp_hf::establish_hfp(&mut sock) {
                                Ok(up) => {
                                    log(&format!(
                                        "AA: HFP hands-free link up with the phone (SLC in {} ms, inbound) — waiting for it to open our Android Auto channel",
                                        up.slc.elapsed.as_millis()
                                    ));
                                    up
                                }
                                Err(e) => {
                                    log(&format!(
                                        "AA: inbound HFP service-level connection failed at {e} -- dropping the link"
                                    ));
                                    continue;
                                }
                            },
                        };
                        // Drain until the phone hangs up or we go quiet. Unlike the outbound path
                        // this does NOT bound itself on a setup grace: the phone chose to connect,
                        // nothing here blocks the reconnect loop, and dropping the link would flip
                        // its HEADSET state back to disconnected.
                        let why = reconnect::drain_headset_link(&sock, up, &t);
                        log(&format!("{}: headset link released ({why})", path.as_str()))
                    }
                    Ok(None) => break, // shutdown
                    Err(e) => {
                        log(&format!(
                            "{}: headset RFCOMM accept error: {e} -- retrying",
                            path.as_str()
                        ));
                        thread::sleep(Duration::from_secs(1));
                    }
                }
            }
        }));
    }

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
    aa_shutdown.store(true, Ordering::Relaxed);
    headset_shutdown.store(true, Ordering::Relaxed);
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
    let _ = aa_handle.join();
    // The headset threads spawn nothing and touch no AV state, so their join order is not
    // load-bearing — but they DO hold an RFCOMM channel, and leaving one open past go_quiet means
    // the next session's bind fails EADDRINUSE.
    for h in headset_handles {
        let _ = h.join();
    }
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
    // Bound the owner claim `run_aa_bootstrap` deliberately holds past an established bootstrap:
    // this session is over, so whatever it was holding the box for cannot still be arriving.
    aa_wireless::release_owner_if_ours();
    // And un-latch the phase mirror (audit 3.3), same reason as bt_driver's exit funnel: on a preempt
    // or go-quiet `bt_driver::run` may never have been entered, so its own idle publish never fires.
    bt_driver::publish_bt_phase(ocbm_proto::BTP_IDLE);
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
    install_panic_hook("carplay-wireless");
    let shutdown = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, shutdown.clone())
        .expect("register SIGTERM handler");

    // (#63 child-reaping is handled by double-forking the detached daemons in av::spawn_detached so they
    // reparent to init — NOT by SIGCHLD=SIG_IGN, which would make every Command::status() in this process
    // return ECHILD and break bt_bringup + av::running(). See spawn_detached.)

    log("starting -- wireless CarPlay (Phase A1: Bluetooth discoverable + pairing + SDP)");
    log_radio_caps();

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
