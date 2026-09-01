//! bt_driver.rs — the RFCOMM-bound half of the iAP2 stack, running the same auth/identify handshake
//! as the wired `carplayd`'s `iap2/driver.rs`, over an accepted RFCOMM socket instead of FunctionFS
//! endpoints. A deliberate, small duplicate of `driver.rs`'s `process`/`execute` dispatch rather than
//! a generalization of it -- see docs/carplay/04_CAPABILITIES_AND_CONFIG.md's design rationale: `driver.rs` is the most incident-scarred
//! file in this project (docs 11/18/20) and this way it is never touched again for wireless work.
//!
//! Everything protocol-level (link framing, the auth/identify state machine, the TLV builder, the MFi
//! auth-service client) comes from `carplay_iap2_core`, unchanged from the wired driver's own use of it.
//!
//! SCOPE (Phase A2): runs the handshake through `IdentifyAccept`, subscribes to Now Playing/Route
//! Guidance updates (mirroring the wired driver), and logs anything that arrives after that. Real
//! metadata forwarding and the WiFi credential exchange are explicitly out of scope here -- see
//! docs/carplay/04_CAPABILITIES_AND_CONFIG.md.

use carplay_iap2_core::{
    link::{self, Link},
    message, spec,
    state::{self, Action, State},
};
// MFi comes from the box's LOCAL i2c chip (crate::mfi_local), NOT carplay_iap2_core::mfi (the PoC's
// remote auth-service TCP client) — this box has the genuine MFi 2.0C on /dev/i2c-1 @0x11. See mfi_local.rs.
use crate::mfi_local as mfi;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Phase flag mirrored to the host by ocbmd as `CT_BT_PHASE`.
///
/// A FILE, deliberately, not a channel: it follows `/tmp/pairing_code` exactly. The BT driver has
/// no handle on the OCBM link (different process), the flag is last-write-wins so a missed update
/// is self-correcting, and a write failure can never affect the handshake it is reporting on.
const BT_PHASE_FILE: &str = "/tmp/bt_phase";

/// Best-effort: reporting must never perturb what it reports.
pub(crate) fn publish_bt_phase(phase: u8) {
    let _ = std::fs::write(BT_PHASE_FILE, phase.to_string());
}

/// Map the iAP2 handshake state onto the wire-visible phase. Deliberately coarse: the host uses it
/// to decide when to stand its A/V consumers up, not to mirror the state machine.
fn phase_for(st: State) -> u8 {
    match st {
        State::Init => ocbm_proto::BTP_LINK_UP,
        State::CertSent | State::SignSent => ocbm_proto::BTP_AUTHENTICATING,
        State::Authenticated => ocbm_proto::BTP_AUTHENTICATED,
        State::IdentSent | State::IdentRetried => ocbm_proto::BTP_IDENTIFYING,
        State::Identified => ocbm_proto::BTP_IDENTIFIED,
    }
}

fn log(m: &str) {
    println!("[bt-driver] {m}");
}

/// This Pi's `hci0` BD address. The reference's own comment on the equivalent constant: "lifted
/// verbatim from the capture... cosmetic — any 6 bytes are accepted by iOS" (see docs/carplay/04_CAPABILITIES_AND_CONFIG.md) — the
/// *presence* of param 17 is what's load-bearing, not this specific value, but using the real
/// address costs nothing and avoids an arbitrary placeholder.
const ACCESSORY_BT_MAC: [u8; 6] = [0xD8, 0x3A, 0xDD, 0x65, 0x6E, 0x03];

/// Write `data` fully to `w`, checking `should_abort` between attempts -- same discipline as the
/// wired driver's `write_interruptible`, generic over `Write` for the same reason (unit-testable
/// without real hardware).
fn write_interruptible<W: Write>(w: &mut W, data: &[u8], should_abort: &dyn Fn() -> bool) -> bool {
    let mut written = 0;
    while written < data.len() {
        if should_abort() {
            return false;
        }
        match w.write(&data[written..]) {
            Ok(0) => return false,
            Ok(n) => written += n,
            Err(e) if e.kind() == ErrorKind::WouldBlock => sleep(Duration::from_millis(50)),
            Err(_) => return false,
        }
    }
    true
}

/// Max bytes to hold while reassembling one link frame across RFCOMM reads. A legit partial tail is at
/// most one incomplete frame (< advertised `MaxRcvPacketLength` 4096); anything larger is a desynced
/// byte stream that will never form a frame -- drop it and let retransmit / re-SYN recover (audit Fix #21).
const MAX_REASSEMBLY: usize = 8192;

/// Run the iAP2 accessory handshake to `IdentifyAccept` over an already-accepted RFCOMM socket.
/// Returns once the session ends (peer gone, shutdown, or budget expiry) -- exit reason logged.
///
/// A thin wrapper over [`session`] purely so the phase mirror is un-latched on EVERY exit, including
/// the three early returns that never enter the loop. See the `publish_bt_phase` call below.
pub fn run(sock: File, name: &str, shutdown: &AtomicBool) {
    session(sock, name, shutdown);
    // Un-latch the phase mirror (audit 3.3). `/tmp/bt_phase` is last-write-wins with no unlink, so
    // without this the deepest phase reached since boot stays readable forever and `ocbmd` re-emits
    // it to every fresh subscriber -- the host sees a phantom "phone detected" long after the phone
    // left.
    //
    // It sits HERE, not at the bottom of `session`, because `session` has three early returns
    // (detect-write, initial-SYN-write, setsockopt) that leave before the loop is entered. Those
    // paths are exactly the ones that fire when a phone drops the RFCOMM link during bring-up, so
    // they are the last place the latch should survive.
    publish_bt_phase(ocbm_proto::BTP_IDLE);
}

fn session(mut sock: File, name: &str, shutdown: &AtomicBool) {
    let budget = Duration::from_secs(120); // pre-Identify handshake budget, matching the wired driver
    let start = Instant::now();
    // A stalled pre-Identify WRITE must honor the budget too, not just shutdown (audit B5): a peer that opens
    // RFCOMM then stalls its receive window mid-write (e.g. during the ~2 KB AuthenticationCertificate) would
    // otherwise spin write_interruptible (1 s SO_SNDTIMEO + 50 ms sleep) until preempt. But the budget must
    // apply ONLY pre-Identify — `abort` is ALSO threaded into the post-Identify ACK / WiFi-config writes and
    // (originally) the loop-top check, and once Identify completes the RFCOMM iAP2 link is held open for the
    // whole CarPlay-over-WiFi session. So gate the budget on `!identified_flag` (set below when Identify is
    // reached); shutdown always aborts. The loop-top check is shutdown-only; the read-side pre-Identify
    // budget stays its own explicit check in the loop.
    let identified_flag = AtomicBool::new(false);
    let abort = || {
        shutdown.load(Ordering::Relaxed)
            || (!identified_flag.load(Ordering::Relaxed) && start.elapsed() >= budget)
    };

    let mut link = Link::new();
    if !write_interruptible(&mut sock, &link::DETECT, &abort) {
        log("failed to write detect prelude (shutdown, budget, or I/O error) -- aborting session start");
        return;
    }
    let syn = link.build_syn(&link::SYN_PARAMS);
    if !write_interruptible(&mut sock, &syn, &abort) {
        log("failed to write initial SYN (shutdown, budget, or I/O error) -- aborting session start");
        return;
    }

    let mut st = State::Init;
    let mut last_syn = Instant::now();
    // SYN-RESEND BUDGET (audit 5.7). Apple's device link counts RECEIVED SYNs and fires
    // `NotifyConnectionFail` -> `Failed` at the 11th (`iAP2LinkProcessInOrderPacket`, counter at
    // `link+0x15c`); see `receiver/src/iap_tunnel.rs:115-118`, which documents the same constraint and
    // warns against exactly this 1 s resend loop being written without a cap. Initial SYN + 9 resends
    // = 10 total, one under the limit. KEEP THE TWO SITES IN SYNC.
    const SYN_RESEND_MAX: u32 = 9;
    let mut syn_resends: u32 = 0;
    let mut buf = [0u8; 8192];
    let mut acc: Vec<u8> = Vec::new();

    // A blocking read with a timeout gives the shutdown/budget checks a chance to run periodically,
    // matching the wired driver's non-blocking-read + WouldBlock-retry idiom without needing a
    // second non-blocking fd (RFCOMM is one full-duplex socket, unlike ep1/ep2).
    //
    // SO_SNDTIMEO too, not just SO_RCVTIMEO (review fix 2026-07-31): without a send timeout a write
    // against a non-draining peer blocks on the kernel's own default (~20s) and
    // `write_interruptible`'s WouldBlock/abort machinery above is dead code — the shutdown check
    // between attempts never gets a chance to run. With 1s on both directions the retry arm is live.
    // Checked (like ssp_agent.rs's setsockopt): a silent failure here would quietly restore the
    // very unbounded-blocking-syscall behavior this project's write_interruptible incident
    // discipline exists to prevent, so treat it as fatal to the session.
    let raw = std::os::fd::AsRawFd::as_raw_fd(&sock);
    // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these
    // types carry private padding and a literal does not compile.
    let mut timeout: libc::timeval = unsafe { std::mem::zeroed() };
    timeout.tv_sec = 1;
    for (opt, what) in [
        (libc::SO_RCVTIMEO, "SO_RCVTIMEO"),
        (libc::SO_SNDTIMEO, "SO_SNDTIMEO"),
    ] {
        let ret = unsafe {
            libc::setsockopt(
                raw,
                libc::SOL_SOCKET,
                opt,
                &timeout as *const libc::timeval as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        if ret < 0 {
            log(&format!(
                "setsockopt({what}) failed: {} -- aborting session (timeouts keep shutdown/budget checks live)",
                std::io::Error::last_os_error()
            ));
            return;
        }
    }

    loop {
        // Shutdown-only here (audit B5): the pre-Identify budget is the explicit check just below, and
        // folding it into this top-of-loop check would terminate a healthy post-Identify session at 120 s.
        if shutdown.load(Ordering::Relaxed) {
            log("shutdown requested -- closing RFCOMM session");
            break;
        }
        let identified = st >= State::Identified;
        if identified {
            // Latch it so `abort` stops applying the handshake budget to the post-Identify writes.
            identified_flag.store(true, Ordering::Relaxed);
        }
        if !identified && start.elapsed() >= budget {
            log("handshake budget expired before Identify");
            break;
        }
        match sock.read(&mut buf) {
            Ok(0) => {
                log("peer closed the RFCOMM connection");
                break;
            }
            Ok(n) => {
                // Reassemble across reads (audit Fix #21): RFCOMM is SOCK_STREAM, so one iAP2 link frame
                // can split across two read()s. Append to a persistent accumulator; `process` drains
                // complete frames and RETAINS the partial tail for the next read (the old code passed a
                // fresh `&buf[..n]` each read, dropping any split frame + everything behind it).
                acc.extend_from_slice(&buf[..n]);
                if process(&mut acc, &mut link, &mut st, &mut sock, name, &abort) {
                    break; // Abort
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                if st == State::Init
                    && syn_resends < SYN_RESEND_MAX
                    && last_syn.elapsed() >= Duration::from_secs(1)
                {
                    let _ = write_interruptible(&mut sock, &syn, &abort);
                    last_syn = Instant::now();
                    syn_resends += 1;
                    if syn_resends == SYN_RESEND_MAX {
                        // Keep reading: the phone may still SYN-ACK, and the pre-Identify budget
                        // remains the thing that ends a session that never gets there.
                        log("SYN resend budget exhausted (10 total, Apple NotifyConnectionFail at 11) -- waiting passively");
                    }
                }
            }
            Err(e) => {
                log(&format!("read error: {e}"));
                break;
            }
        }
    }
    log(&format!("exit state={st:?}"));
}

/// Parse the iAP2 params of a control-session message payload (`[40 40][total u16 BE][msg_id u16 BE]
/// [params…][cksum]`) into `paramN=value` strings, treating each param value as a (possibly
/// NUL-terminated) UTF-8 string. DeviceTransportIdentifierNotification's params (the phone's Wi-Fi MAC
/// and device-id) are exactly such strings. `total` (= 6 + body len) bounds the walk so the trailing
/// payload checksum byte is never mistaken for param data.
fn parse_transport_identifiers(payload: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    if payload.len() < 6 {
        return out;
    }
    let total = ((payload[2] as usize) << 8) | payload[3] as usize;
    let end = total.min(payload.len());
    let mut i = 6; // skip [40 40][total u16][msg_id u16]
    while i + 4 <= end {
        let plen = ((payload[i] as usize) << 8) | payload[i + 1] as usize;
        let pid = ((payload[i + 2] as u16) << 8) | payload[i + 3] as u16;
        if plen < 4 || i + plen > end {
            break;
        }
        let s: String = payload[i + 4..i + plen]
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| b as char)
            .collect();
        if !s.is_empty() {
            out.push(format!("param{pid}={s}"));
        }
        i += plen;
    }
    out
}

/// Drain ALL link packets in one RFCOMM read (#139). BR/EDR RFCOMM readily coalesces several iAP2 link
/// frames into a single `read()`, and `Link::parse` decodes only the FIRST (it ignores trailing bytes),
/// so a single-parse-per-read silently dropped every packet after the first — the same truncation the
/// File-Transfer path already guards against with `link::packet_len`. Walk the buffer packet-by-packet.
fn process(
    acc: &mut Vec<u8>,
    link: &mut Link,
    st: &mut State,
    sock: &mut File,
    name: &str,
    abort: &dyn Fn() -> bool,
) -> bool {
    let mut off = 0;
    while off < acc.len() {
        match link::packet_len(&acc[off..]) {
            // A complete valid-SOP frame at `off` -- dispatch it and advance past it.
            Some(plen) => {
                if process_one(&acc[off..off + plen], link, st, sock, name, abort) {
                    acc.drain(..off + plen); // drop everything consumed incl. the aborting frame
                    return true; // Abort propagates immediately
                }
                off += plen;
            }
            // Not a complete frame at `off`. Two sub-cases:
            //  (a) valid-SOP frame still arriving (`FF 5A ...`, declared_len > available, OR only the
            //      first `FF` seen so SOP2 is unknown) -- RETAIN it and wait for the next read.
            //  (b) a non-frame byte at the head -- the phone's 6-byte DETECT prelude `FF 55 02 00 EE 10`
            //      (link::DETECT, sent before SYN, `b[1] != SOP2`), or desync junk. `packet_len` would
            //      return None on it forever, wedging every real frame behind it (audit Fix #21, per the
            //      accessoryd DETECT trace). Skip one byte to resync to the next candidate SOP.
            None => {
                let head_is_partial_frame = acc[off] == link::SOP1
                    && (off + 1 >= acc.len() || acc[off + 1] == link::SOP2);
                if head_is_partial_frame {
                    break; // (a) retain from `off` and wait for more bytes
                }
                off += 1; // (b) resync past a non-frame byte (DETECT prelude / junk)
            }
        }
    }
    acc.drain(..off); // drop consumed frames + skipped non-frame bytes; RETAIN any partial tail (#21)
    // Belt-and-suspenders: after resync, `acc` holds at most one genuine partial frame (< advertised
    // MaxRcvPacketLength 4096). If a corrupt-length `FF 5A` frame declares a size that never arrives and
    // pushes it past the cap, drop it and let retransmit / re-SYN recover.
    if acc.len() > MAX_REASSEMBLY {
        log(&format!(
            "RFCOMM reassembly: dropping {} desynced bytes (no frame boundary)",
            acc.len()
        ));
        acc.clear();
    }
    false
}

/// Process exactly ONE link packet. Returns `true` to abort the session. Mirrors `driver.rs::process`.
fn process_one(
    data: &[u8],
    link: &mut Link,
    st: &mut State,
    sock: &mut File,
    name: &str,
    abort: &dyn Fn() -> bool,
) -> bool {
    let Some(rx) = link.parse(data) else {
        return false;
    };
    if rx.is_syn_ack() {
        log("SYN-ACK -- link up");
        publish_bt_phase(ocbm_proto::BTP_LINK_UP);
        return !write_interruptible(sock, &link.build_ack(), abort);
    }
    if rx.payload.is_empty() {
        return false;
    }
    if rx.sess == 2 {
        return !write_interruptible(sock, &link.build_ack(), abort);
    }
    let Some(msg_id) = link::parse_msg_id(&rx.payload) else {
        return false;
    };
    if !write_interruptible(sock, &link.build_ack(), abort) {
        return true;
    }

    // Phase A3 — WiFi credential handoff (device→accessory path). The iPhone asks the accessory to
    // describe the AP it hosts (`RequestAccessoryWiFiConfigurationInformation` 0x5702, no params); we
    // answer `AccessoryWiFiConfigurationInformation` (0x5703) with our LIVE hostapd AP config so the
    // phone can join wlan0. Intercepted here (not in iap2-core::state) to keep the shared wired state
    // machine untouched. Sent on the control session (1), like identify.
    if msg_id == spec::MSG_REQUEST_ACCESSORY_WI_FI_CONFIGURATION_INFORMATION {
        let cfg = crate::wifi_handoff::read_hostapd_ap_config();
        log(&format!(
            "RX 0x5702 RequestAccessoryWiFiConfig -> replying 0x5703 (ssid={:?} ch={:?} sec={:?})",
            cfg.ssid, cfg.channel, cfg.security_type
        ));
        publish_bt_phase(ocbm_proto::BTP_WIFI_HANDOFF);
        let body = crate::wifi_handoff::build_accessory_wifi_configuration_information(&cfg);
        let sent = write_interruptible(
            sock,
            &link.build_msg(
                1,
                spec::MSG_ACCESSORY_WI_FI_CONFIGURATION_INFORMATION,
                &body,
            ),
            abort,
        );
        if sent && !abort() {
            // The iPhone now has the AP credentials and will associate to wlan0, then look for the
            // AirPlay receiver via mDNS. Bring up that layer (airplayd RTSP :5000 + rx-connect
            // advertise/browse/connect-out on wlan0) so it's discoverable the moment the phone joins.
            // Idempotent — safe on repeated 0x5702 and on reconnect.
            //
            // Re-check `abort` between the reply write and the spawn (audit #3): if shutdown/preempt was
            // signalled while we were writing 0x5703, spawning here would race the teardown and orphan
            // airplayd/rx-connect. The main loop also joins this thread before teardown_av_layer(), so
            // this guard is belt-and-suspenders against that ordering.
            crate::av::ensure_av_layer();
        }
        return !sent;
    }

    // The set of post-Identify messages we expect from the device (all `Source::Device` in Apple's
    // catalog): NowPlayingUpdate, RouteGuidanceUpdate, RouteGuidanceManeuverInformation,
    // AuthenticationSucceeded, IdentificationAccepted, CarPlayAvailability. Anything else is logged.
    // DeviceTransportIdentifierNotification (0x4E0E) carries the phone's transport identifiers — its
    // real Wi-Fi MAC + device-id string — as iAP2 params (#189). It arrives every wireless handshake;
    // parse + log the identifiers instead of dumping it as "unrecognized". Only ARP's private MAC is
    // otherwise visible on wlan0, so this is the one place the phone's true Wi-Fi MAC is exposed.
    if msg_id == spec::MSG_DEVICE_TRANSPORT_IDENTIFIER_NOTIFICATION {
        let ids = parse_transport_identifiers(&rx.payload);
        if !ids.is_empty() {
            log(&format!(
                "RX 0x4E0E DeviceTransportIdentifier: {}",
                ids.join(", ")
            ));
        }
    } else if *st >= State::Identified
        && !matches!(
            msg_id,
            spec::MSG_NOW_PLAYING_UPDATE                    // 0x5001
                | spec::MSG_ROUTE_GUIDANCE_UPDATE           // 0x5201
                | spec::MSG_ROUTE_GUIDANCE_MANEUVER_INFORMATION // 0x5202
                | spec::MSG_AUTHENTICATION_SUCCEEDED        // 0xAA05
                | spec::MSG_IDENTIFICATION_ACCEPTED         // 0x1D02
                | spec::MSG_CAR_PLAY_AVAILABILITY // 0x4300
        )
    {
        log(&format!(
            "RX unrecognized 0x{msg_id:04X} ({} B): {:02x?}",
            rx.payload.len(),
            rx.payload
        ));
    }

    // docs/wireless/00_WIRELESS_CARPLAY.md Phase 5.2 diagnostic addition: `state::parse_rejected_param_ids` (called inside
    // `on_message` below) deliberately returns only TOP-LEVEL rejected param ids, never the message-id
    // array nested inside a params-6/7 reject's value — so a 5.1-style "stripped [6]" log never showed
    // WHICH declared message id iOS actually objected to. Log the raw reject payload here, before it's
    // parsed down, so a 5.2 (or any future) reject on params 6/7 can be manually decoded afterward to
    // tell content-specific rejection (iOS names 0x5200 specifically) apart from a general reaction to
    // params 6/7 growing at all (iOS would then just echo back the whole current list) — the exact
    // open question the Phase 5.1 postmortem left unanswered. Log-only; no behavior change.
    if msg_id == spec::MSG_IDENTIFICATION_REJECTED {
        log(&format!(
            "RX 0x1D03 IdentificationRejected raw payload ({} B): {:02x?}",
            rx.payload.len(),
            rx.payload
        ));
        // Decoded: params 6/7 carry an array of the specific message ids iOS will not accept.
        // Until 2026-07-25 that array was always empty, which is why docs/wireless/00_WIRELESS_CARPLAY.md read the reject as a
        // blanket objection to params-6/7 growth. It is not — iOS names the offender when it has one,
        // and the named id goes straight into `CARPLAY_METADATA_SKIP` / the policy file's `skip=`.
        log(&format!(
            "RX 0x1D03 decoded: {}",
            carplay_iap2_core::message::describe_reject(&rx.payload[6..])
        ));
    }
    let (next, action) = state::on_message(*st, msg_id, &rx.payload);
    match execute(action, link, sock, name, abort) {
        ExecResult::Commit => {
            *st = next;
            publish_bt_phase(phase_for(*st));
            log(&format!("RX 0x{msg_id:04X} -> {st:?}"));
            false
        }
        ExecResult::NoCommit => {
            log(&format!("RX 0x{msg_id:04X}: action failed, state held"));
            false
        }
        ExecResult::Abort => true,
    }
}

enum ExecResult {
    Commit,
    NoCommit,
    Abort,
}

/// Execute a state-machine action. Mirrors `driver.rs::execute`, minus the USB-only call-control
/// integration and metadata-server forwarding (out of scope for Phase A2 -- see docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
/// Retry a fallible local-MFi I2C op a few times before giving up (#210). The auth chip on `/dev/i2c-1`
/// occasionally NAKs a transaction on a rapid reconnect (the observed 0xAA00 "action failed, state
/// held"); an immediate retry succeeds. Retrying in-session keeps the handshake alive instead of
/// returning NoCommit → the iPhone timing out and resetting the entire RFCOMM link to try again.
fn mfi_retry<T>(what: &str, mut op: impl FnMut() -> Option<T>) -> Option<T> {
    for attempt in 1..=3u32 {
        if let Some(v) = op() {
            return Some(v);
        }
        if attempt < 3 {
            log(&format!("MFi {what} attempt {attempt}/3 failed — retrying"));
            sleep(Duration::from_millis(20));
        }
    }
    None
}

fn execute(
    action: Action,
    link: &mut Link,
    sock: &mut File,
    name: &str,
    abort: &dyn Fn() -> bool,
) -> ExecResult {
    match action {
        Action::SendCert => match mfi_retry("cert", mfi::cert) {
            Some(cert) => {
                // AuthenticationCertificate (0xAA01, Source::Accessory): param 0 = AuthenticationCertificate (blob).
                let body = message::group_one(0x0000, &cert);
                if write_interruptible(
                    sock,
                    &link.build_msg(1, spec::MSG_AUTHENTICATION_CERTIFICATE, &body),
                    abort,
                ) {
                    ExecResult::Commit
                } else {
                    ExecResult::Abort
                }
            }
            None => ExecResult::NoCommit,
        },
        Action::SignChallenge(chal) => match mfi_retry("sign", || mfi::sign(&chal)) {
            Some(sig) => {
                // AuthenticationResponse (0xAA03, Source::Accessory): param 0 = AuthenticationResponse (blob).
                let body = message::group_one(0x0000, &sig);
                if write_interruptible(
                    sock,
                    &link.build_msg(1, spec::MSG_AUTHENTICATION_RESPONSE, &body),
                    abort,
                ) {
                    ExecResult::Commit
                } else {
                    ExecResult::Abort
                }
            }
            None => ExecResult::NoCommit,
        },
        Action::SendIdentify => {
            let ib = message::build_ident_info(
                name,
                message::TransportComponent::Wireless {
                    bt_mac: ACCESSORY_BT_MAC,
                },
                false,
            );
            // IdentificationInformation (0x1D01, Source::Accessory). The wireless bearer is declared
            // via param 24 WirelessCarPlayTransportComponent (+ companion param 17
            // BluetoothTransportComponent) inside `build_ident_info`'s Wireless arm.
            if !write_interruptible(
                sock,
                &link.build_msg(1, spec::MSG_IDENTIFICATION_INFORMATION, &ib),
                abort,
            ) {
                return ExecResult::Abort;
            }
            log(&format!(
                "TX 0x1D01 IdentificationInformation ({} B, wireless transport)",
                ib.len()
            ));
            ExecResult::Commit
        }
        Action::RetryIdentify(excluded) => {
            // 0x1D03 IdentificationRejected recovery (Apple's reference behavior, BEHAVIORAL_REFERENCE
            // §1.4): rebuild 0x1D01 without the rejected params and re-send once. State is already
            // State::IdentRetried, so a second reject -> Action::Abort (the retry fires at most once).
            let ib = message::build_ident_info_excluding(
                name,
                message::TransportComponent::Wireless {
                    bt_mac: ACCESSORY_BT_MAC,
                },
                false,
                &excluded,
            );
            if !write_interruptible(
                sock,
                &link.build_msg(1, spec::MSG_IDENTIFICATION_INFORMATION, &ib),
                abort,
            ) {
                return ExecResult::Abort;
            }
            log(&format!(
                "TX 0x1D01 retry, stripped {excluded:?} ({} B, wireless)",
                ib.len()
            ));
            ExecResult::Commit
        }
        Action::Note(m) => {
            log(m);
            // Wireless CarPlay: after IdentifyAccept we do NOT subscribe to NowPlaying (0x5000) or
            // RouteGuidance (0x5200). The working carlink_linux reference doesn't — it waits for MFi
            // auth then the device's 0x5702 RequestAccessoryWiFiConfigurationInformation. Sending those
            // subscribes here would (a) transmit messages we no longer declare in
            // MessagesSentByAccessory (iAP2 contract violation), and (b) divert iOS into plain
            // media-accessory behavior (streaming NowPlaying over BT) instead of initiating the
            // wireless-CarPlay WiFi handoff. NowPlaying/route ride the AirPlay/WiFi link once the
            // session is up, not BT iAP2. (Removed 2026-07-13 per the 12-agent analysis. docs/wireless/00_WIRELESS_CARPLAY.md
            // Phase 5.1, 2026-07-24, briefly declared 0x5000 in MessagesSentByAccessory and was
            // reverted after live-device testing confirmed iOS 0x1D03-rejects it — see message.rs — so
            // rationale (a) is accurate again as written.)
            ExecResult::Commit
        }
        Action::Ignore => ExecResult::Commit,
        Action::Abort => ExecResult::Abort,
        // Real metadata forwarding is out of scope for Phase A2 (no metadata_server in this binary
        // yet) -- log only, matching this phase's "prove the handshake" goal.
        Action::NowPlaying(np) => {
            log(&format!("NowPlaying: {np:?}"));
            ExecResult::Commit
        }
        Action::RouteGuidance(rg) => {
            log(&format!("RouteGuidance: {rg:?}"));
            ExecResult::Commit
        }
        Action::Maneuver(mv) => {
            log(&format!("Maneuver: {mv:?}"));
            ExecResult::Commit
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::FromRawFd;

    /// A connected AF_UNIX SOCK_STREAM pair as two `File`s: the accessory side to hand to `run`,
    /// and the peer side standing in for a phone that opened RFCOMM and then said nothing.
    fn pair() -> (File, File) {
        let mut fds = [0i32; 2];
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) },
            0,
            "socketpair: {}",
            std::io::Error::last_os_error()
        );
        unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
    }

    fn count_syns(stream: &[u8]) -> usize {
        stream.windows(link::SYN_PARAMS.len()).filter(|w| *w == link::SYN_PARAMS).count()
    }

    /// THE CAP THAT KEEPS THE SESSION ALIVE. Apple's device link counts RECEIVED SYNs and fires
    /// `NotifyConnectionFail` -> `Failed` at the 11th, so an uncapped 1 s resend loop against a
    /// silent phone kills the link at ~11 s -- and looks like a phone-side fault, not ours.
    ///
    /// Deliberately an upper bound plus a liveness floor: under a loaded test host you can only ever
    /// see FEWER resends than the schedule, never more, so the assertion that matters cannot flake.
    #[test]
    fn syn_resends_stop_one_under_apples_disconnect_threshold() {
        const APPLE_DISCONNECT_AT: usize = 11;
        let (acc, mut peer) = pair();
        let shutdown = AtomicBool::new(false);

        // Never answer: this is the whole scenario -- a phone that opens RFCOMM and goes quiet.
        let reader = std::thread::spawn(move || {
            let mut seen = Vec::new();
            let mut b = [0u8; 4096];
            let deadline = Instant::now() + Duration::from_secs(13);
            unsafe {
                let tv = libc::timeval { tv_sec: 1, tv_usec: 0 };
                libc::setsockopt(
                    std::os::unix::io::AsRawFd::as_raw_fd(&peer),
                    libc::SOL_SOCKET,
                    libc::SO_RCVTIMEO,
                    &tv as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::timeval>() as libc::socklen_t,
                );
            }
            while Instant::now() < deadline {
                match peer.read(&mut b) {
                    Ok(0) => break,
                    Ok(n) => seen.extend_from_slice(&b[..n]),
                    Err(_) => continue, // timeout: the driver is between resends
                }
            }
            seen
        });

        std::thread::scope(|sc| {
            sc.spawn(|| run(acc, "test-accessory", &shutdown));
            std::thread::sleep(Duration::from_secs(13));
            shutdown.store(true, Ordering::Relaxed);
        });

        let seen = reader.join().unwrap();
        let syns = count_syns(&seen);
        assert!(
            syns < APPLE_DISCONNECT_AT,
            "sent {syns} SYNs in 13 s -- Apple disconnects at the {APPLE_DISCONNECT_AT}th; \
             the resend budget in `run` is missing or too large"
        );
        assert!(syns >= 2, "expected the initial SYN plus at least one resend, got {syns}");
        assert!(
            seen.starts_with(&link::DETECT),
            "the detect prelude must still lead the stream"
        );
    }

    #[test]
    fn session_end_publishes_the_idle_phase_so_the_mirror_cannot_latch() {
        // `/tmp/bt_phase` is last-write-wins with no unlink, and `ocbmd` re-emits whatever it holds
        // to every fresh subscriber. A session that ends without idling it leaves the host reading
        // "phone detected" indefinitely.
        // The peer is gone BEFORE `run` starts, so the detect-prelude write fails and `run` takes
        // one of its three early returns without ever entering the loop. That is the path a phone
        // dropping RFCOMM during bring-up takes, and it must idle the mirror like any other.
        let (acc, peer) = pair();
        drop(peer);
        publish_bt_phase(ocbm_proto::BTP_IDENTIFIED); // pretend a previous session got deep
        let shutdown = AtomicBool::new(false);
        run(acc, "test-accessory", &shutdown);
        assert_eq!(
            std::fs::read_to_string(BT_PHASE_FILE).unwrap().trim(),
            ocbm_proto::BTP_IDLE.to_string(),
            "an early return must un-latch the phase mirror too"
        );

        // ...and so must a normal loop exit (peer closes once the session is under way).
        let (acc2, peer2) = pair();
        publish_bt_phase(ocbm_proto::BTP_IDENTIFIED);
        std::thread::scope(|sc| {
            let h = sc.spawn(|| run(acc2, "test-accessory", &shutdown));
            drop(peer2);
            h.join().unwrap();
        });
        assert_eq!(
            std::fs::read_to_string(BT_PHASE_FILE).unwrap().trim(),
            ocbm_proto::BTP_IDLE.to_string(),
            "the loop exit must un-latch the phase mirror"
        );
    }
}
