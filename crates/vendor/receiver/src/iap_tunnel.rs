//! iap_tunnel.rs — the iAP2 LINK + AUTH + IDENTIFY session run over the AirPlay `iAPSendMessage`
//! tunnel, per docs/wireless/00_WIRELESS_CARPLAY.md. This is a SEPARATE iAP2 session from the BT-time one `bt_driver.rs` runs for
//! the WiFi handoff — Apple's own CarPlay Communication Plug-in R14G17 Integration Guide is explicit:
//! "To support continued iAP during Wireless CarPlay operation... you must perform the full iAP
//! handshaking over this protocol which includes the detect sequence and link synchronization...
//! Only if the current CarPlay session is wireless, you must start a new iAP2 session over the
//! CarPlay control channel. iAP2 over Bluetooth must not be disconnected until the disableBluetooth
//! command is received."
//!
//! Before this module existed, `send_wireless_metadata_subscriptions()` fired bare `Start*Updates`
//! messages into the tunnel with NO link/session ever established. iOS 200-OK'd the AirPlay-level
//! POST (the tunnel itself works) but never routed a reply, because from its iAP2 stack's
//! perspective no identified session exists on that channel at all — exactly the docs/wireless/00_WIRELESS_CARPLAY.md/38 gap.
//!
//! This drives the SAME `iap2_core::state::State` machine `bt_driver.rs`/`iap2d` use (Init ->
//! CertSent -> SignSent -> Authenticated -> IdentSent -> Identified), fed by inbound `iAPSendMessage`
//! frames via [`handle_inbound`] instead of a blocking socket-read loop, and sending link-framed
//! outbound frames via [`crate::events::send_iap_message`]. `TransportComponent::AirPlayTunnel`
//! (message.rs) is the Identify shape for this specific session — same transport-component structure
//! as the BT-time `Wireless` arm, but params 6/7 declare the metadata message ids instead of the
//! WiFi-handoff-only baseline.
//!
//! Deliberately mirrors `bt_driver.rs`'s `process`/`process_one`/`execute` shape (same link/state
//! machine, same MFi chip, same reference behaviors) rather than reinventing it — a 12-Fable review
//! of the first draft found three places this had drifted from that proven reference, all fixed here:
//! link-layer ACKs were never sent (bt_driver ACKs every SYN-ACK and control message — without it,
//! iOS's own link layer may never consider the "link synchronization" the Integration Guide requires
//! complete), the DETECT+SYN nudge resent in ANY pre-Identified state instead of only `State::Init`
//! (a mid-auth resend could reset the phone's link state, matching bt_driver's own `st == State::Init`
//! guard on its resend), and cert/sign calls had no retry (bt_driver's `mfi_retry`, #210, exists
//! because the chip occasionally NAKs a transaction — without it a transient NAK here would stick the
//! handshake with only the phone's own retransmit as a recovery path). Also added: draining every
//! link packet coalesced into one inbound read (`link::packet_len`, mirroring bt_driver's #139 fix),
//! and clearing the session on `Action::Abort` so a stuck handshake actually rebuilds on the next
//! `start()` instead of being permanently wedged.

use iap2_core::{
    link::{self, Link},
    message,
    spec,
    state::{self, Action, State},
};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
// Only the local chip path retries with a backoff; without `local-mfi` nothing here sleeps.
#[cfg(feature = "local-mfi")]
use std::thread::sleep;
use std::time::{Duration, Instant};

fn log(m: &str) {
    println!("[iap-tunnel] {m}");
}

/// Same physical BT MAC `bt_driver.rs::ACCESSORY_BT_MAC` uses — kept identical for identity
/// continuity across the BT-time and tunnel Identifies (the value itself is cosmetic per docs/carplay/04_CAPABILITIES_AND_CONFIG.md;
/// only its presence in param 17 is load-bearing).
const ACCESSORY_BT_MAC: [u8; 6] = [0xD8, 0x3A, 0xDD, 0x65, 0x6E, 0x03];

/// Pre-Identify handshake budget. `bt_driver.rs` (120 s) and `iap2d` both bound their handshakes; this
/// module originally had no timer at all. Past this budget, a pre-Identify session in ANY state is
/// discarded and rebuilt (`budget_check`) — up to `MAX_HANDSHAKE_REBUILDS` times, then it gives up.
///
/// SCOPE (audit Fix #20 widened the Fix #9 baseline): recovery now covers a wedge that occurs AFTER the
/// SYN-ACK too. `budget_check` fires for ANY pre-Identified state (Init/CertSent/SignSent/…), not just
/// the old `Init && !link_up` case — justified because the tunnel is Zero-Ack (no link-layer retransmit
/// timer, so a lost cert/sign/identify frame wedges UNBOUNDED; link.rs SYN_PARAMS_ZERO_ACK_TUNNEL), the
/// Simulator shows a legit pre-Identify phase is <3 s (so 120 s never false-fires), and CINEMO CT5 bounds
/// the whole post-SYN-ACK open externally (10 s×3 then give up — authority §2; libNmeIAP has no internal
/// auth timer). Past the budget the link is provably dead, so the discard+rebuild re-DETECTs correctly
/// (unlike the modesChanged nudge, which must never re-DETECT a LIVE link, docs/carplay/05_METADATA_AND_CONTROLS.md). Driven both by
/// `start()` (record/modesChanged) and — for the SILENT wedge those never see — by `tick()` off the
/// DataStream idle loop. Matched to bt_driver's 120 s value.
const HANDSHAKE_BUDGET: Duration = Duration::from_secs(120);

/// Max budget-driven rebuilds of a wedged pre-Identify handshake before giving up (CINEMO CT5 caps at 3
/// attempts, authority §2). An uncapped 120 s rebuild loop would DETECT-nudge the phone forever; after the
/// cap we wait for a modesChanged nudge or AirPlay teardown (`reset()` re-arms the counter). audit Fix #20.
const MAX_HANDSHAKE_REBUILDS: u32 = 3;
static HANDSHAKE_REBUILDS: AtomicU32 = AtomicU32::new(0);

struct Session {
    link: Link,
    state: State,
    /// The exact SYN bytes sent at `start()`, cached so a retry re-sends them VERBATIM (same seq)
    /// rather than calling `build_syn` again, which would consume a new seq and desync — mirrors
    /// `bt_driver.rs::run`'s own resend-the-cached-`syn`-variable pattern.
    syn: Vec<u8>,
    /// When this session was created, for `HANDSHAKE_BUDGET`.
    started: Instant,
    /// Whether we have already fallen back from Zero-Ack to the standard `SYN_PARAMS` this session.
    /// One-shot, so a peer that declines twice cannot put us in a re-SYN loop.
    syn_fallback: bool,
    /// Has the peer answered our SYN with a SYN-ACK? The link is then UP and must never be
    /// re-DETECTed, even though `state` is still `Init` (the state machine only advances once
    /// authentication starts, so `Init` alone does NOT mean "no reply seen").
    ///
    /// docs/carplay/05_METADATA_AND_CONTROLS.md: without this, the `modesChanged` nudge re-sent DETECT+SYN on an already-established
    /// link. The iPhone's own iAP2 trace showed it answering our SYN in 6 ms and then being torn down
    /// by that second DETECT, after which it retransmitted its SYN-ACK indefinitely and the handshake
    /// never completed.
    link_up: bool,
    /// Album-artwork File Transfer assembler for link session 2 (docs/20 §1.2). Ported from the wired
    /// `iap2d` path, which is the proven implementation.
    art: iap2_core::metadata::Artwork,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

/// Start (or nudge) the tunnel's iAP2 link.
///
/// CORRECTED 2026-07-25: `Init` does NOT mean "no reply seen yet" — `state` only advances once the phone
/// opens authentication, so an established, SYN-ACKed link sits at `Init` too. `Session::link_up`
/// distinguishes them, and the nudge resends the cached SYN ONLY, never DETECT (docs/carplay/05_METADATA_AND_CONTROLS.md §2.1).
///
/// SYN-RESEND BUDGET: Apple's device link counts received SYNs and fires `NotifyConnectionFail` ->
/// `Failed` at the **11th** (`iAP2LinkProcessInOrderPacket`, counter at `link+0x15c`). The nudge is
/// one-shot per session, so at most 2 SYNs leave here. If this is ever converted to `bt_driver`'s 1 s
/// resend loop, it MUST carry a retry cap of 10 or fewer.
/// Called from `session.rs::record()` at the end of the start sequence, and again on the `modesChanged`
/// one-shot retry in `events.rs` (docs/wireless/00_WIRELESS_CARPLAY.md #2.8's
/// original purpose — the FIRST send losing a startup race — still applies here, just one layer
/// earlier: a DETECT/SYN lost to the same race). No-ops once `Identified`, and — critically — no-ops
/// (rather than resending) once handshake progress has been seen (state > Init): resending DETECT+SYN
/// once the phone is mid-cert/sign/identify would reset ITS link state while our own `Session` keeps
/// its further-along state, desyncing the two sides permanently. `bt_driver.rs::run`'s own SYN-resend
/// loop has the identical `st == State::Init` guard for the same reason.
pub fn start() {
    let mut guard = crate::plock(&SESSION);
    // Budget-driven recovery of a wedged pre-Identify handshake (audit Fix #20 — see the HANDSHAKE_BUDGET
    // scope note). `budget_check` discards a provably-dead over-budget session (ANY pre-Identified state)
    // and reports whether a rebuild is still permitted under the retry cap; on GiveUp we stop re-DETECTing.
    if let BudgetCheck::GiveUp = budget_check(&mut guard) {
        return; // retry cap hit — await a modesChanged nudge or AirPlay teardown (reset() re-arms the cap)
    }
    match guard.as_ref() {
        Some(sess) if sess.state >= State::Identified => return, // already up
        Some(sess) if sess.state == State::Init && sess.link_up => {
            // SYN-ACK already received: the link IS up, `state` just hasn't advanced yet. Re-sending
            // DETECT here would tear it down (docs/carplay/05_METADATA_AND_CONTROLS.md) — the exact bug the iPhone's trace exposed.
            log("link already up (SYN-ACK received) — not re-DETECTing");
            return;
        }
        Some(sess) if sess.state == State::Init => {
            // No reply seen yet — safe to nudge, matches bt_driver.rs's 1s SYN-resend-while-Init loop,
            // just triggered by modesChanged here instead of a timer (this module has no poll thread).
            // SYN ONLY — never DETECT. `bt_driver.rs`'s proven resend loop re-sends just the cached
            // SYN; a verbatim SYN retransmit is benign, whereas DETECT re-runs the device's attach path
            // (`iAP2LinkDeviceActionSendDetect`) and resets its link. docs/carplay/05_METADATA_AND_CONTROLS.md §2.1: that is what tore down
            // a handshake the phone had already answered.
            let sent = crate::events::send_iap_message(&sess.syn);
            log(&format!("resent SYN only (state={:?}, sent={sent})", sess.state));
            return;
        }
        Some(sess) => {
            // Handshake already past Init — do NOT resend (see doc comment above); just wait.
            log(&format!(
                "handshake already in flight (state={:?}) — not nudging",
                sess.state
            ));
            return;
        }
        None => {}
    }
    // docs/carplay/03_SDK_GROUND_TRUTH.md §8: Apple's Integration Guide (line 290) recommends the Zero-Ack link parameters for
    // iAP2 tunnelled over the CarPlay control channel — this carrier is already reliable and ordered,
    // so the iAP2 link layer's own retransmit/cumulative-ack timers are redundant and would misfire.
    // We previously reused the Bluetooth `SYN_PARAMS` here (2000 ms retransmit, cumulative-ack 6).
    // docs/carplay/05_METADATA_AND_CONTROLS.md: MaxPacketSize must be 0xFFFF on this transport (Apple's transport-type-2 template; the
    // stream's SETUP declares `controlType=2`). With 0x1000 the device restarted the handshake with a
    // fresh SYN-ACK sequence instead of accepting our ACK — observed on hardware 2026-07-25.
    let mut link = Link::new();
    let syn = link.build_syn(&link::SYN_PARAMS_ZERO_ACK_TUNNEL);
    let sent_detect = crate::events::send_iap_message(&link::DETECT);
    let sent_syn = crate::events::send_iap_message(&syn);
    log(&format!(
        "TX detect+SYN over AirPlay tunnel (detect_sent={sent_detect} syn_sent={sent_syn}) — starting fresh iAP2 session"
    ));
    *guard = Some(Session {
        link,
        state: State::Init,
        syn,
        started: Instant::now(),
        syn_fallback: false,
        link_up: false,
        art: Default::default(),
    });
}

/// Outcome of the pre-Identify handshake budget check.
enum BudgetCheck {
    /// No session, or a session within budget / already Identified — `guard` untouched.
    NotExpired,
    /// The session was over budget and discarded (`guard` set to `None`); a rebuild is permitted.
    Rebuild,
    /// Over budget AND the retry cap is exhausted — discarded, and no rebuild should follow.
    GiveUp,
}

/// If the current session is a pre-Identify handshake past `HANDSHAKE_BUDGET`, discard it (it is provably
/// dead — the Zero-Ack tunnel has no link-layer retransmit timer, so nothing else would) and report
/// whether the caller may rebuild, honouring `MAX_HANDSHAKE_REBUILDS`. Shared by `start()` and `tick()`.
/// audit Fix #20.
fn budget_check(guard: &mut Option<Session>) -> BudgetCheck {
    let overdue = matches!(
        guard.as_ref(),
        Some(s) if s.state < State::Identified && s.started.elapsed() > HANDSHAKE_BUDGET
    );
    if !overdue {
        return BudgetCheck::NotExpired;
    }
    let state = guard.as_ref().map(|s| s.state);
    *guard = None; // the link is dead; a full DETECT+SYN rebuild is correct past the budget (docs/carplay/05_METADATA_AND_CONTROLS.md)
    let n = HANDSHAKE_REBUILDS.load(Ordering::Relaxed);
    if n >= MAX_HANDSHAKE_REBUILDS {
        log(&format!(
            "handshake budget ({}s) expired at {state:?} after {n} rebuilds — giving up (awaiting modesChanged nudge / AirPlay teardown)",
            HANDSHAKE_BUDGET.as_secs()
        ));
        return BudgetCheck::GiveUp;
    }
    HANDSHAKE_REBUILDS.fetch_add(1, Ordering::Relaxed);
    log(&format!(
        "handshake budget ({}s) expired at {state:?} — discarding and rebuilding (attempt {}/{})",
        HANDSHAKE_BUDGET.as_secs(),
        n + 1,
        MAX_HANDSHAKE_REBUILDS
    ));
    BudgetCheck::Rebuild
}

/// Periodic tick for the SILENT pre-Identify wedge (audit Fix #20). `start()` is only called from
/// `record()` and the modesChanged one-shot, so a session that SYN-ACKs then goes quiet (a lost auth
/// frame on the retransmit-free Zero-Ack tunnel) is never re-examined. Called from the DataStream serve
/// loop's idle tick (~500 ms), this re-checks the budget with no inbound needed and rebuilds a dead link.
pub fn tick() {
    let mut guard = crate::plock(&SESSION);
    if let BudgetCheck::Rebuild = budget_check(&mut guard) {
        drop(guard); // release before start() re-locks SESSION
        start(); // guard is now None → start()'s rebuild arm sends a fresh DETECT+SYN
    }
}

/// Reset on session teardown so a future reconnect starts fresh (called from `events::clear()`).
pub fn reset() {
    *crate::plock(&SESSION) = None;
    HANDSHAKE_REBUILDS.store(0, Ordering::Relaxed); // re-arm the budget retry cap for the next session
}

/// Build and send ONE metadata subscribe on the established tunnel link, taking the SESSION lock for
/// exactly the build+send. Returns `(sent, frame_len)`.
///
/// Deliberately just-in-time rather than pre-built: `Link::build_msg` stamps both the outgoing sequence
/// number AND a snapshot of `peer_seq` (the cumulative ack) into the frame, so a frame built now and
/// sent 200 ms later can carry a stale ack, while `build_ack` (which does NOT consume a seq — it echoes
/// `my_seq - 1`) would meanwhile advertise a last-transmitted seq for frames still queued. Building
/// immediately before transmission keeps both fields truthful. Called from the paced sender thread in
/// `events::send_wireless_metadata_subscriptions`, so the lock is held only briefly and never across
/// the inter-subscribe sleep.
pub(crate) fn send_subscribe(id: u16, body: &[u8]) -> (bool, usize) {
    let mut guard = crate::plock(&SESSION);
    let Some(sess) = guard.as_mut() else {
        return (false, 0); // session torn down between IdentifyAccept and this send
    };
    let frame = sess.link.build_msg(1, id, body);
    let n = frame.len();
    (crate::events::send_iap_message(&frame), n)
}

/// Feed one inbound tunnel read through the link/state machine, draining EVERY link packet coalesced
/// into it (mirrors `bt_driver.rs::process`'s `link::packet_len` walk, #139: the AirPlay tunnel can
/// just as readily bundle multiple frames — e.g. AuthenticationSucceeded immediately followed by
/// StartIdentification — into one `iAPSendMessage` delivery as BT/RFCOMM can). Returns true if this
/// read was (at least partly) consumed as link/session bring-up traffic (the caller should NOT also
/// try the normal metadata dispatch on it); false means either `start()` was never called for this
/// session, or the link is already `Identified` and metadata replies (0x5001/0x5201/0x5202/etc, still
/// routed by the existing `dispatch_iap_tunnel_message`) own this frame from here on.
pub fn handle_inbound(data: &[u8]) -> bool {
    let mut guard = crate::plock(&SESSION);
    let Some(sess) = guard.as_mut() else {
        return false; // start() never called — not our traffic (shouldn't happen post-RECORD)
    };

    // FALLBACK (deliberate, load-bearing): if the link is established but this delivery is not
    // link-framed at all, hand it straight back to the caller's bare-payload dispatcher. The whole
    // premise of this module is that iOS speaks a real link session on this channel; if that premise
    // is wrong on hardware, this one line keeps the previously-shipped bare-payload path working
    // instead of silently swallowing every metadata update. Costs nothing when the premise holds.
    // FALLBACK, at ANY state (widened 2026-07-25 after review — it used to be `>= Identified` only,
    // which was a real regression on the control channel): if this delivery is not link-framed at all,
    // hand it straight back to the caller's bare-payload dispatcher.
    //
    // Why the state restriction was wrong: while `state < Identified`, a non-link-framed delivery was
    // consumed with ZERO dispatches (logged, `break`, then `return true`). So in exactly the world this
    // fallback exists for — iOS speaking bare payloads on this channel and therefore never answering
    // our SYN — the session would sit at `Init` forever while every bare payload was swallowed. That is
    // WORSE than the pre-change behaviour, where `session.rs::command()` passed the bytes straight to
    // `dispatch_iap_tunnel_message`. The fallback has to work in the state the premise-failure actually
    // produces, which is the pre-Identify one.
    if link::packet_len(data).is_none() {
        return false;
    }

    let mut off = 0;
    let mut aborted = false;
    while off < data.len() {
        let Some(plen) = link::packet_len(&data[off..]) else {
            // Only reachable for a trailing remainder now (off > 0), since a non-frame at off == 0
            // returned false above. Log it either way — a silently dropped tail is how coalesced-read
            // bugs hide (this project has had two).
            log(&format!(
                "RX {} B remaining at offset {off} didn't parse as a link frame (state={:?}) — dropped",
                data.len() - off,
                sess.state
            ));
            break; // partial tail / non-frame bytes — stop
        };
        let packet = &data[off..off + plen];
        if sess.state >= State::Identified {
            // QC 2026-07-25 (HIGH): this branch is new. Previously `handle_inbound` returned false the
            // moment the session reached `Identified`, which ABANDONED the very link session the
            // module had just spent the whole handshake establishing: nothing ever ACKed an inbound
            // frame or advanced `link.peer_seq` again. With the non-Zero-Ack SYN_PARAMS in use
            // (2000 ms retransmit, 30 retries) iOS would retransmit every metadata update — each one
            // re-dispatched, so duplicated — and eventually declare the link dead. Two independent
            // reviewers reached this same conclusion. Now the established link keeps being serviced.
            handle_established(sess, packet);
        } else if handle_one(sess, packet) {
            aborted = true;
            break;
        }
        off += plen;
        // NB: no `break` on reaching Identified mid-walk any more — the remainder of a coalesced read
        // (e.g. the first metadata update arriving alongside IdentificationAccepted) now falls into
        // the branch above and is dispatched instead of being logged and dropped.
    }
    if aborted {
        log("tunnel session aborted (second IdentificationRejected) — clearing; next start() rebuilds fresh");
        *guard = None;
    }
    true
}

/// Service one link packet on an ESTABLISHED (post-Identify) tunnel session: parse it out of the link
/// layer, ACK it, and hand the inner iAP2 payload to the normal metadata dispatcher. The ACK is the
/// point — see the note in `handle_inbound`.
fn handle_established(sess: &mut Session, data: &[u8]) {
    let Some(rx) = sess.link.parse(data) else {
        log(&format!(
            "RX ({} B) post-Identify didn't parse as a link frame — ignoring",
            data.len()
        ));
        return;
    };
    if rx.payload.is_empty() {
        return; // bare ACK from the phone — nothing to dispatch
    }
    crate::events::send_iap_message(&sess.link.build_ack());

    // The ONE message where bypassing `state::on_message` post-Identify actually costs something.
    // Every other id the bypass swallows (0x1D00, 0xAA00, 0x1D03, 0xAA05, 0x1D02) would have been
    // `Ignore` or an idempotent no-op at `Identified` anyway — but `0xAA04 AuthenticationFailed` is
    // an unguarded `Action::Abort` arm. Without this, the phone declares authentication failed on an
    // established tunnel and we keep ACKing and transmitting subscribes on a link it has abandoned.
    // `bt_driver` has no bypass and does abort, so the two transports disagreed on the same event.
    if let Some(msg_id) = link::parse_msg_id(&rx.payload) {
        if msg_id == spec::MSG_AUTHENTICATION_FAILED {
            log("RX 0xAA04 AuthenticationFailed on the established tunnel — aborting session");
            reset();
            return;
        }
    }

    if rx.sess == 2 {
        // File Transfer session — album artwork (docs/20 §1.2). Ported verbatim in behaviour from the
        // wired `iap2d` path (`ccpa/iap2d/src/main.rs:357-371`), which is the proven implementation.
        //
        // The trailing payload-checksum byte MUST be stripped here. Control messages self-bound via
        // their `[40 40][total]` header so the extra byte is inert for them — a raw file-transfer
        // fragment does not, and appending it would inject one stray byte per fragment into the JPEG.
        let body = &rx.payload[..rx.payload.len().saturating_sub(1)];
        log(&format!("RX session-2 file-transfer fragment ({} B)", body.len()));
        if let Some(reply) = sess.art.on_session2(body) {
            // Accept / Success go back out on session 2. Without the Accept the phone sends no data
            // at all, so a silent drop here looks exactly like the phone never offering.
            let sent = crate::events::send_iap_message(&sess.link.build_raw(2, &reply));
            log(&format!("TX session-2 reply ({} B, sent={sent})", reply.len()));
        }
        return;
    }
    // `rx.payload` is the bare `[40 40][len][msg-id][body][cks]` form; the dispatcher's own
    // declared-length clamp handles the trailing payload-checksum byte.
    crate::events::dispatch_iap_tunnel_message(&rx.payload);
}

/// Process exactly one link packet against the session. Returns true if the state machine aborted
/// (second `IdentificationRejected` — see `state::on_message`'s retry-once ceiling), signaling the
/// caller to clear the whole session so a later `start()` builds a truly fresh `Link` (new seq
/// counters) rather than resuming a link the phone may have already abandoned.
fn handle_one(sess: &mut Session, data: &[u8]) -> bool {
    let Some(rx) = sess.link.parse(data) else {
        log(&format!(
            "RX ({} B) didn't parse as a link frame at state={:?} — ignoring",
            data.len(),
            sess.state
        ));
        return false;
    };
    if rx.is_syn_ack() {
        // docs/carplay/03_SDK_GROUND_TRUTH.md §8: Zero-Ack is a NEGOTIATED mode — both ends must agree. Cinemo rejects a peer SYN
        // whose four ack/retransmit fields aren't all zero ("ZeroACK link configuration is not
        // supported") and reverts to normal parameters; Apple has the same fallback in its FSM
        // (`iAP2LinkAccessoryActionRestartSYNWithRetransmit`). Mirror that: if the phone's echoed
        // parameters are not Zero-Ack, rebuild the link once with the proven `SYN_PARAMS` instead of
        // sitting on a mode the peer has declined. At most one fallback per session (`syn_fallback`),
        // so a peer that disagrees twice can't loop us.
        // CRITICAL (2026-07-25 review): distinguish "peer declined Zero-Ack" from "the SYN-ACK carried
        // no readable parameter block". `is_zero_ack` returns false for BOTH, and treating them alike
        // is self-masking: if the phone's SYN-ACK is header-only (declared_len == 9 → empty payload),
        // the fallback would fire on the FIRST SYN-ACK of EVERY session, silently discard Zero-Ack, and
        // log "peer did NOT accept Zero-Ack" — so the change would read as tried-and-rejected by the
        // phone when in fact we never gave it a chance. That is not hypothetical: docs/carplay/03_SDK_GROUND_TRUTH.md §1 itself
        // describes the SYN-ACK as "9 bytes, FF 5A-headed", and the 2026-07-25 capture recorded ZERO
        // inbound iAP frames, so this payload's shape is entirely unobserved on this transport.
        // Evidence favours a full block arriving (Apple has no separate SYN-ACK builder —
        // `_iAP2LinkAccessoryActionSendSYNACK` routes through `_iAP2PacketCreateSYNPacket`, which always
        // serialises >= 10 parameter bytes) — but "probably" is not worth nullifying the change on a
        // run that costs a deploy cycle. Absence of evidence must not count as a decline.
        let peer_declined = rx.payload.len() >= 10 && !link::is_zero_ack(&rx.payload);
        if rx.payload.len() < 10 {
            log(&format!(
                "RX SYN-ACK carried no readable link params ({} B) — assuming Zero-Ack accepted; \
                 record this shape, it has never been observed on this transport",
                rx.payload.len()
            ));
        }
        if peer_declined && !sess.syn_fallback {
            sess.syn_fallback = true;
            log(&format!(
                "RX SYN-ACK but peer did NOT accept Zero-Ack (params {:02x?}) — re-SYNing with standard link params",
                &rx.payload[..rx.payload.len().min(10)]
            ));
            let mut fresh = Link::new();
            // Keep MaxPacketSize=0xFFFF on this transport even when falling back off Zero-Ack —
            // `SYN_PARAMS` carries 0x1000, which docs/carplay/05_METADATA_AND_CONTROLS.md §2.2 corrected for the tunnel.
            let syn = fresh.build_syn(&link::SYN_PARAMS_TUNNEL_RETRANSMIT);
            // One send, one binding. NOTE the state below is installed even if the send FAILED — that is
            // deliberate and safe, because what gets installed is self-consistent (`state = Init`,
            // `link_up = false`, the cached `syn`), so the SYN-only nudge and the budget rebuild can both
            // recover it. Do not "fix" this into an early return.
            let sent_syn = crate::events::send_iap_message(&syn);
            log(&format!("re-SYN sent={sent_syn}"));
            sess.link = fresh;
            sess.syn = syn;
            sess.state = State::Init;
            // Must clear with the rest of the link state, or the guard in `start()` would suppress the
            // recovery nudge for a link we just tore down and rebuilt.
            sess.link_up = false;
            sess.started = Instant::now(); // fresh handshake budget for the retry
            return false;
        }
        log(&format!(
            "RX SYN-ACK — link up (zero-ack={}), ACKing",
            link::is_zero_ack(&rx.payload)
        ));
        sess.link_up = true; // docs/carplay/05_METADATA_AND_CONTROLS.md: never re-DETECT after this point
        crate::events::send_iap_message(&sess.link.build_ack());
        return false;
    }
    if rx.payload.is_empty() {
        return false; // bare ACK, nothing to act on
    }
    let Some(msg_id) = link::parse_msg_id(&rx.payload) else {
        return false;
    };
    // ACK every control message before acting on it — mirrors bt_driver.rs::process_one, and matches
    // the Integration Guide's "link synchronization" requirement (a first-draft review found this
    // ACK was missing entirely; see the module doc comment).
    crate::events::send_iap_message(&sess.link.build_ack());
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
            iap2_core::message::describe_reject(&rx.payload[6..])
        ));
    }
    let (next, action) = state::on_message(sess.state, msg_id, &rx.payload);
    match execute(action, &mut sess.link) {
        ExecOutcome::Commit => {
            sess.state = next;
            if next >= State::Identified {
                HANDSHAKE_REBUILDS.store(0, Ordering::Relaxed); // handshake succeeded — clear the cap (#20)
            }
            log(&format!("RX 0x{msg_id:04X} -> {next:?}"));
            false
        }
        ExecOutcome::NoCommit => {
            log(&format!("RX 0x{msg_id:04X}: action failed, state held"));
            false
        }
        ExecOutcome::Abort => true,
    }
}

enum ExecOutcome {
    Commit,
    NoCommit,
    Abort,
}

/// Retry a fallible local-MFi I2C op a few times before giving up — same rationale and shape as
/// `bt_driver.rs::mfi_retry` (#210): the chip on `/dev/i2c-1` occasionally NAKs a transaction, and an
/// immediate retry succeeds. Contention is higher for this module specifically (per
/// `mfi-i2c-local`'s own doc comment, up to three processes can now want the chip around the same
/// time), so this retry matters at least as much here as it did for the BT-time handshake.
/// A remote MFi signer, installed by the embedder when this process cannot reach a chip locally.
///
/// # Why this exists
///
/// The AirPlay-tunnel iAP2 handshake needs two chip operations (`copy_certificate`,
/// `create_signature`). It used to call [`mfi_i2c_local`] unconditionally — direct I2C on
/// `/dev/i2c-1` behind a `flock` on `/tmp/carplay_mfi.lock`. That is correct ON THE BOX, where this
/// crate runs inside `airplayd` alongside four other chip users.
///
/// It is wrong wherever the chip is NOT on the machine running this crate — and which machine that
/// is varies by deployment, which is the whole reason this is a runtime choice rather than an
/// assumption:
///
/// | Deployment | Runs this crate | Where the MFi chip is | Correct path |
/// |---|---|---|---|
/// | CCPA box, wired/wireless CarPlay (`airplayd`) | the box | same board, `/dev/i2c-1` | local — `local-mfi` |
/// | gm_ccpa: GM `gminfo37` head unit owns the SoftAP and the CarPlay session; the CCPA is **only** a BT radio + MFi coprocessor | the head-unit Android app | on the **box**, across USB | remote — OCBM `CH_MFI` |
/// | Pi / NCM bring-up | the host | on the box, across USB-NCM | remote — `mfi-wire` to `mfid` |
///
/// Other OCBM hosts exist with their own arrangements; the rule is the invariant, not the table.
///
/// In the gm_ccpa case two independent facts make the local path impossible, either alone fatal:
///
///  1. `/tmp` does not exist on Android, so the lock file cannot even be opened — which presented
///     for months as "another chip user holds the lock" (see `MfiError::LockUnavailable`);
///  2. a head unit's own coprocessor, where fitted, is reachable only by system apps, and that app is
///     deliberately an ordinary Play-attributed app UID — and it is not the chip the session
///     authenticates with anyway.
///
/// `/auth-setup` in the very same session already proves the correct route: it goes over `CH_MFI` to
/// the box and returns the 945-byte certificate and 128-byte RSA-1024 signature. The tunnel simply
/// never used it.
///
/// A process-wide registry rather than a threaded parameter because [`execute`] is a free function
/// reached through `on_message`, and `SESSION` above establishes that module-level state is how this
/// module already carries per-process context.
static REMOTE_SIGNER: Mutex<Option<Arc<Mutex<dyn mfi::auth_client::MfiSigner + Send>>>> =
    Mutex::new(None);

/// Install the signer the tunnel should use instead of local chip access. Call once, before a
/// session starts. Installing a signer takes precedence over [`mfi_i2c_local`] unconditionally.
///
/// # The signer you install MUST be bounded, and bounded tightly
///
/// [`handle_inbound`] holds the module-wide `SESSION` mutex across its whole dispatch, which
/// includes [`execute`] and therefore these two chip calls. Whatever budget your signer has is a
/// budget the RCS reader, the event handler, `POST /command` and teardown all wait on — none of
/// which have a deadline of their own. This is not new (the local path has the same shape, and the
/// note on [`mfi_retry`] below says so), but a REMOTE signer makes it a network property rather
/// than a bus property, so the ceiling is whatever the embedder chose rather than a chip timeout.
///
/// Embedders today, for calibration:
///  - gm_ccpa's OCBM `CH_MFI` relay uses its short-budget methods, 4 s TOTAL per op (lock
///    acquisition and reply wait come out of one clock). Cert measures ~1.1 s, sign ~1.7 s.
///  - `mfi-wire` (Pi / NCM bring-up) uses a 5 s connect + 30 s I/O timeout, i.e. up to ~35 s. That
///    is far too long to hold `SESSION` and is only survivable because that host is a bench tool,
///    not a live projection session. Do not copy those numbers into a vehicle path.
///
/// If you need a budget larger than a few seconds, restructure so the call happens outside the
/// `SESSION` guard rather than raising it here.
pub fn set_remote_signer(signer: Arc<Mutex<dyn mfi::auth_client::MfiSigner + Send>>) {
    if let Ok(mut slot) = REMOTE_SIGNER.lock() {
        *slot = Some(signer);
        log("remote MFi signer installed — tunnel chip ops go through the embedder, not /dev/i2c-1");
    }
}

fn remote_signer() -> Option<Arc<Mutex<dyn mfi::auth_client::MfiSigner + Send>>> {
    REMOTE_SIGNER.lock().ok().and_then(|s| s.clone())
}

/// `copy_certificate`, from whichever chip path this build/process actually has.
fn mfi_cert() -> Option<Vec<u8>> {
    if let Some(sig) = remote_signer() {
        return match sig.lock() {
            Ok(mut s) => match s.copy_certificate() {
                Ok(c) => Some(c),
                Err(e) => {
                    log(&format!("MFi cert via remote signer FAILED: {e}"));
                    None
                }
            },
            Err(_) => {
                log("MFi cert: remote signer mutex poisoned");
                None
            }
        };
    }
    #[cfg(feature = "local-mfi")]
    {
        mfi_retry("cert", mfi_i2c_local::try_cert)
    }
    #[cfg(not(feature = "local-mfi"))]
    {
        log("MFi cert: no remote signer installed and local chip access is not compiled in");
        None
    }
}

/// `create_signature` over a 20-byte SHA-1 digest, same dispatch as [`mfi_cert`].
fn mfi_sign(chal: &[u8]) -> Option<Vec<u8>> {
    if let Some(sig) = remote_signer() {
        return match sig.lock() {
            Ok(mut s) => match s.create_signature(chal) {
                Ok(v) => Some(v),
                Err(e) => {
                    log(&format!("MFi sign via remote signer FAILED: {e}"));
                    None
                }
            },
            Err(_) => {
                log("MFi sign: remote signer mutex poisoned");
                None
            }
        };
    }
    #[cfg(feature = "local-mfi")]
    {
        mfi_retry("sign", || mfi_i2c_local::try_sign(chal))
    }
    #[cfg(not(feature = "local-mfi"))]
    {
        log("MFi sign: no remote signer installed and local chip access is not compiled in");
        None
    }
}

/// Retry a chip operation — but NOT a lock timeout.
///
/// Every attempt used to take `MfiLock` with its own 10 s deadline, so pure lock contention cost
/// 3 x 10 s with the tunnel's `SESSION` mutex held, blocking the RCS reader, the event handler,
/// `POST /command` and teardown, none of which have a deadline of their own. Retrying could never
/// help: a lock timeout means another chip user holds it, and waiting the same 10 s twice more does
/// not change that. `MfiError` now distinguishes the two, so contention costs 10 s once and the retry
/// is reserved for the chip NAK it was written for.
///
/// It also produces a truthful diagnostic: a wedged holder and a dead chip used to log the same
/// "all 3 attempts failed".
#[cfg(feature = "local-mfi")]
fn mfi_retry<T>(
    what: &str,
    mut op: impl FnMut() -> Result<T, mfi_i2c_local::MfiError>,
) -> Option<T> {
    for attempt in 1..=3u32 {
        match op() {
            Ok(v) => return Some(v),
            Err(mfi_i2c_local::MfiError::LockBusy) => {
                log(&format!(
                    "MFi {what}: lock busy (another chip user holds /tmp/carplay_mfi.lock) — \
                     not retrying, that would just stall SESSION again"
                ));
                return None;
            }
            // NOT contention — this host cannot reach the chip locally at all, and no retry, no
            // waiting and no later session will change that. Saying so plainly matters: while this
            // shared a variant with LockBusy, an unconditional structural failure on Android was
            // logged as a transient race and read that way for months (gm_ccpa
            // 12_OBSERVED_FLOW.md Failure Point 6). The fix is to route these two call sites through
            // the MfiSigner the ControlServer already holds — on Android that is the OCBM CH_MFI
            // relay, which /auth-setup uses successfully in the very same session (gm_ccpa
            // 11_HARDENING_PLAN.md T5.3).
            Err(mfi_i2c_local::MfiError::LockUnavailable) => {
                log(&format!(
                    "MFi {what}: NO LOCAL CHIP ACCESS on this host (lock path unopenable) — this is \
                     NOT lock contention and will never succeed here; the tunnel needs a remote \
                     signer (OCBM CH_MFI). See T5.3."
                ));
                return None;
            }
            Err(mfi_i2c_local::MfiError::Chip) if attempt < 3 => {
                log(&format!("MFi {what} attempt {attempt}/3 failed (chip) — retrying"));
                sleep(Duration::from_millis(20));
            }
            Err(mfi_i2c_local::MfiError::Chip) => {}
        }
    }
    log(&format!("MFi {what}: all 3 chip attempts failed"));
    None
}

/// Execute a state-machine action, sending link-framed replies over the tunnel via
/// `events::send_iap_message`. Mirrors `bt_driver.rs::execute`'s `ExecResult` shape (Commit/NoCommit/
/// Abort) exactly, rather than collapsing it to a bool — `Abort` now actually clears the session (see
/// `handle_inbound`), so it needs to be distinguishable from a merely-failed send.
fn execute(action: Action, link: &mut Link) -> ExecOutcome {
    match action {
        Action::SendCert => match mfi_cert() {
            Some(cert) => {
                let body = message::group_one(0x0000, &cert);
                let sent = crate::events::send_iap_message(&link.build_msg(
                    1,
                    spec::MSG_AUTHENTICATION_CERTIFICATE,
                    &body,
                ));
                if sent {
                    log("TX 0xAA01 AuthenticationCertificate (tunnel)");
                    ExecOutcome::Commit
                } else {
                    ExecOutcome::NoCommit
                }
            }
            None => ExecOutcome::NoCommit,
        },
        Action::SignChallenge(chal) => match mfi_sign(&chal) {
            Some(sig) => {
                let body = message::group_one(0x0000, &sig);
                let sent = crate::events::send_iap_message(&link.build_msg(
                    1,
                    spec::MSG_AUTHENTICATION_RESPONSE,
                    &body,
                ));
                if sent {
                    log("TX 0xAA03 AuthenticationResponse (tunnel)");
                    ExecOutcome::Commit
                } else {
                    ExecOutcome::NoCommit
                }
            }
            None => ExecOutcome::NoCommit,
        },
        Action::SendIdentify => {
            let ib = message::build_ident_info(
                "CarLink",
                message::TransportComponent::AirPlayTunnel {
                    bt_mac: ACCESSORY_BT_MAC,
                },
                false,
            );
            let sent = crate::events::send_iap_message(&link.build_msg(
                1,
                spec::MSG_IDENTIFICATION_INFORMATION,
                &ib,
            ));
            if sent {
                log(&format!(
                    "TX 0x1D01 IdentificationInformation ({} B, AirPlay tunnel)",
                    ib.len()
                ));
                ExecOutcome::Commit
            } else {
                ExecOutcome::NoCommit
            }
        }
        Action::RetryIdentify(excluded) => {
            let ib = message::build_ident_info_excluding(
                "CarLink",
                message::TransportComponent::AirPlayTunnel {
                    bt_mac: ACCESSORY_BT_MAC,
                },
                false,
                &excluded,
            );
            let sent = crate::events::send_iap_message(&link.build_msg(
                1,
                spec::MSG_IDENTIFICATION_INFORMATION,
                &ib,
            ));
            if sent {
                log(&format!(
                    "TX 0x1D01 retry, stripped {excluded:?} ({} B, AirPlay tunnel)",
                    ib.len()
                ));
                ExecOutcome::Commit
            } else {
                ExecOutcome::NoCommit
            }
        }
        Action::Note(m) => {
            log(m);
            if m == "IdentifyAccept" {
                // The tunnel's OWN iAP2 session is now Identified — only now is it safe/meaningful to
                // subscribe. This is the actual fix: previously this call happened with no session
                // ever having been established at all. The subscribes now go out LINK-FRAMED, built
                // just-in-time per send via `send_subscribe` (see its doc comment for why not here:
                // `link` is borrowed under the SESSION lock we are currently inside, and pre-building
                // would stamp stale seq/ack fields into frames sent up to 250 ms later).
                crate::events::send_wireless_metadata_subscriptions();
            }
            ExecOutcome::Commit
        }
        Action::Ignore => ExecOutcome::Commit,
        Action::Abort => ExecOutcome::Abort,
        // Not reachable pre-Identified (on_message guards these on `state >= Identified`), and once
        // Identified `handle_inbound` returns `false` before ever calling `execute` — kept exhaustive
        // for the match, not because this path fires.
        Action::NowPlaying(_) | Action::RouteGuidance(_) | Action::Maneuver(_) => ExecOutcome::Commit,
    }
}

#[cfg(test)]
mod remote_signer_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Records what it was asked for, so the test can prove the tunnel took the REMOTE path rather
    /// than falling through to `/dev/i2c-1`.
    struct Fake {
        certs: Arc<AtomicUsize>,
        signs: Arc<AtomicUsize>,
        fail: bool,
        last_digest: Arc<Mutex<Vec<u8>>>,
    }

    impl mfi::auth_client::MfiSigner for Fake {
        fn copy_certificate(&mut self) -> std::io::Result<Vec<u8>> {
            self.certs.fetch_add(1, Ordering::Relaxed);
            if self.fail {
                return Err(std::io::Error::other("relay down"));
            }
            Ok(vec![0xC0; 945]) // the 945-byte cert the OCBM CH_MFI relay returns on hardware
        }
        fn create_signature(&mut self, digest: &[u8]) -> std::io::Result<Vec<u8>> {
            self.signs.fetch_add(1, Ordering::Relaxed);
            *self.last_digest.lock().unwrap() = digest.to_vec();
            if self.fail {
                return Err(std::io::Error::other("relay down"));
            }
            Ok(vec![0x51; 128]) // 128-byte RSA-1024 signature
        }
    }

    /// One test, not several: `REMOTE_SIGNER` is process-wide state and cargo runs tests in
    /// parallel threads, so the ordering has to be explicit.
    #[test]
    fn remote_signer_takes_over_the_tunnels_two_chip_call_sites() {
        // 1. Nothing installed. Without `local-mfi` there is no chip to fall back to, so both
        //    dispatchers must report failure rather than reach for /dev/i2c-1.
        #[cfg(not(feature = "local-mfi"))]
        {
            assert!(mfi_cert().is_none(), "no signer + no local chip must not silently succeed");
            assert!(mfi_sign(&[0u8; 20]).is_none());
        }

        // 2. Install one. THIS is the T5.3 fix: on Android the tunnel used to call mfi-i2c-local
        //    unconditionally and fail every attempt (no /tmp -> the lock could not even be opened),
        //    which cost the metadata/controls channel entirely.
        let certs = Arc::new(AtomicUsize::new(0));
        let signs = Arc::new(AtomicUsize::new(0));
        let last = Arc::new(Mutex::new(Vec::new()));
        set_remote_signer(Arc::new(Mutex::new(Fake {
            certs: certs.clone(),
            signs: signs.clone(),
            fail: false,
            last_digest: last.clone(),
        })));

        let cert = mfi_cert().expect("cert must come back from the remote signer");
        assert_eq!(cert.len(), 945);
        assert_eq!(certs.load(Ordering::Relaxed), 1, "the REMOTE path must have been taken");

        let digest = [0xAB; 20];
        let sig = mfi_sign(&digest).expect("signature must come back from the remote signer");
        assert_eq!(sig.len(), 128);
        assert_eq!(signs.load(Ordering::Relaxed), 1);
        assert_eq!(&*last.lock().unwrap(), &digest, "the digest must be passed through verbatim");

        // 3. A failing relay is a failed handshake attempt, not a panic and not a fallback to the
        //    local chip: the tunnel is Zero-Ack and its own 120 s budget retries.
        set_remote_signer(Arc::new(Mutex::new(Fake {
            certs: certs.clone(),
            signs: signs.clone(),
            fail: true,
            last_digest: last.clone(),
        })));
        assert!(mfi_cert().is_none());
        assert!(mfi_sign(&digest).is_none());
        assert_eq!(certs.load(Ordering::Relaxed), 2, "still remote -- no local fallback on error");
        assert_eq!(signs.load(Ordering::Relaxed), 2);
    }
}
