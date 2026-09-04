//! Encrypted **event channel** (receiver → iPhone command channel).
//!
//! At RECORD the iPhone connects the receiver's event port; the receiver is the HTTP *client* on it,
//! POSTing `/command` requests (e.g. `hidSendReport` for touch) encrypted with the **Events** keys —
//! the same ChaCha20-Poly1305 frame transport as the control channel (`[len:u16 LE][ct‖tag]`, per-
//! direction monotonic nonce). Mirrors the C `_ControlStart` + `AirPlayReceiverSessionSendCommand` /
//! `AirPlayReceiverSessionSendHIDReport`.
//!
//! Fire-and-forget: HID reports pass a NULL completion in the C, so we send and don't await the
//! response (a background reader drains inbound frames so the socket can't back up).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use plist::{Dictionary, Value};
use rtsp::control::{derive_event_keys, ControlChannel};

/// The active event channel (one per session). `None` until RECORD wires it / after teardown.
static EVENT: Mutex<Option<EventSender>> = Mutex::new(None);

/// Does the receiver currently hold MainScreen focus (#47)? Set true when we take the screen at RECORD;
/// updated from inbound `modesChanged` on either channel. Lets a consumer (host status / watchdog)
/// distinguish a deliberate iOS focus-hand-off (a phone call/Siri took the screen → the screen going
/// quiet is EXPECTED) from a transport stall (link actually broke) — the two look identical from
/// frame-flow alone. Default true (we own the screen post-RECORD until told otherwise).
static SCREEN_FOCUSED: AtomicBool = AtomicBool::new(true);

/// Whether the receiver currently holds MainScreen focus per the last `modesChanged` (#47).
pub fn screen_focused() -> bool {
    SCREEN_FOCUSED.load(Ordering::Acquire)
}

/// docs/wireless/00_WIRELESS_CARPLAY.md #2.8: has the one-shot wireless-metadata re-subscribe already fired this session? The
/// original RECORD-time subscribe (`send_wireless_metadata_subscriptions`) sends once, ~0-150ms after
/// the event channel comes up, with no retry — and per docs/carplay/05_METADATA_AND_CONTROLS.md, iOS's registration reply is
/// unconditionally "success" even before the ACC endpoint is actually ready, so a send that loses that
/// race is silently unrecoverable today. `modesChanged` is a cheap, already-parsed "something changed"
/// signal to retry ONCE per session (not a general retry loop, to avoid spamming the tunnel).
static METADATA_RESUBSCRIBED: AtomicBool = AtomicBool::new(false);

/// docs/carplay/02_SESSION_LIFECYCLE.md: has this session's start sequence COMPLETED? The structural equivalent of Apple's
/// `inSession->sessionStarted` (`AirPlayReceiverSession.c:1147`), which is the hard gate on every
/// accessory-initiated command — `require_action_quiet( inSession->sessionStarted, exit, err = kStateErr )`
/// (`:825`). The Integration Guide states the rule in plain text at :299-302: *"it is not valid to send
/// any commands or messages to the iPhone until after this method has been called… sending commands too
/// early would cause undefined behavior."*
///
/// Set from the END of `session.rs::record()`, and only when the event channel actually came up.
///
/// Deliberately NOT enforced inside `send_command` itself: `send_request_ui`/`send_take_screen` are sent
/// from inside the RECORD accept arm (earlier than this flag is set) and are part of the PROVEN wired
/// baseline, which must not regress. This gates the wireless iAP2 tunnel only — the one path where the
/// Guide's timing rule is known to matter and where nothing is yet proven.
static SESSION_STARTED: AtomicBool = AtomicBool::new(false);

/// Record whether this session's start sequence completed (see [`SESSION_STARTED`]).
pub(crate) fn mark_session_started(started: bool) {
    SESSION_STARTED.store(started, Ordering::Release);
}

/// Is it valid to open the iAP2 tunnel yet?
pub(crate) fn session_started() -> bool {
    SESSION_STARTED.load(Ordering::Acquire)
}

// Read-once caches for SPAWN-scoped env vars consulted per-event/per-frame. These are set by the
// parent before exec and never written at runtime (unlike the per-connection levers in
// `crate::levers`), so a process-lifetime read is exact — and it removes repeated `getenv` calls
// from the inbound hot path.

/// `CARPLAY_EVENTS_LOG` — verbose inbound-event logging. Also gates the per-frame RCS/datastream
/// hex dumps in `datastream.rs::send` and `session.rs::log_datastream_frame` (R6).
pub(crate) fn events_log() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var_os("CARPLAY_EVENTS_LOG").is_some())
}

/// `CARPLAY_WIRELESS_METADATA` — the wireless iAP2-tunnel master gate (set only by the wireless
/// spawn site, `crates/vendor/wireless/src/av.rs`).
fn wireless_metadata() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var_os("CARPLAY_WIRELESS_METADATA").is_some())
}

/// `CARPLAY_EVENT_DUMP` — plaintext inbound-frame dump path, if armed.
fn event_dump_path() -> Option<&'static str> {
    static V: OnceLock<Option<String>> = OnceLock::new();
    V.get_or_init(|| std::env::var("CARPLAY_EVENT_DUMP").ok()).as_deref()
}

/// Parse one decrypted inbound event-channel message. Two shapes arrive here:
///  - an RTSP RESPONSE to a command WE sent (`RTSP/1.0 <code> …`) — surface its status (#121) so a
///    rejected `changeModes`/`requestUI` is visible instead of silently swallowed;
///  - an iOS-initiated `POST /command` with a binary-plist body — parse its `type`, and for
///    `modesChanged` update the screen-focus state (#47).
fn handle_inbound_event(pt: &[u8]) {
    let text = String::from_utf8_lossy(pt);
    let first = text.lines().next().unwrap_or("").trim();
    if let Some(rest) = first.strip_prefix("RTSP/1.0 ") {
        // Response to one of our commands. Log non-2xx always (a real problem); 2xx only when asked.
        let code = rest.split_whitespace().next().unwrap_or("");
        let ok = code.starts_with('2');
        if !ok {
            eprintln!("[events] command response NOT OK: '{first}'");
        } else if events_log() {
            eprintln!("[events] command response: '{first}'");
        }
        // A 2xx response CAN carry a body (Content-Length + plist): Apple's own
        // `_AirPlayReceiverSessionSendCommandCompletion` parses one when present (`{status, params?}` —
        // no `type` key, so this WON'T match the `iap`-type dispatch below and won't itself decode as
        // metadata). Previously this `return` discarded any such body unconditionally; fall through to
        // the same capture-to-disk logic below (for offline inspection) instead of returning — a real
        // inbound NowPlayingUpdate etc. is still expected to arrive as its own separate iOS-initiated
        // request (unaffected by this branch), per `AirPlayReceiverSession.c:574`'s inbound dispatch.
        if !ok {
            return;
        }
    }
    // iOS-initiated request: the binary plist starts after the blank line. Search the RAW bytes for the
    // header terminator (audit R2): `String::from_utf8_lossy` replaces each invalid UTF-8 subsequence
    // with a 3-byte U+FFFD, so a byte offset from `text.find()` can exceed `pt.len()` and `&pt[i+4..]`
    // would panic (daemon-fatal) — the plist body is binary, so it routinely contains non-UTF-8 bytes.
    let body = match pt.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => &pt[i + 4..],
        None => return,
    };
    // A bodyless 2xx ack (the common case for hidSendReport/changeModes/etc., and now also for
    // iAPSendMessage per this session's device-observed silent-accept) falls through to here with an
    // EMPTY body. Stop before capturing it: a `[len=0]` record would abort `decode_cmd_capture.py`'s
    // whole-file walk (it treats `ln == 0` as corruption and stops), which would silently poison every
    // record captured AFTER the first ordinary ack — including any real inbound metadata frame this
    // capture exists to catch. Nothing of value to parse/capture in an empty body anyway.
    if body.is_empty() {
        return;
    }
    // Wireless-metadata investigation (2026-07-22, docs/wireless/00_WIRELESS_CARPLAY.md): unconditionally capture every inbound
    // event-channel plist body, so a live session's iAP2-over-AirPlay tunnel frames (if any arrive —
    // type "iAPSendMessage", the wireless-only metadata carrier) can be pulled over OCBM/UART and
    // decoded offline with `scratchpad/decode_cmd_capture.py` (same `[u32 LE len][plist]` framing the
    // control-channel capture in session.rs uses). Read-only observation, size-capped like that one.
    {
        use std::io::Write as _;
        const CAP_MAX: u64 = 4 * 1024 * 1024;
        let path = "/tmp/carplay_event_capture.bin";
        // Truncate once per session rather than appending across TEARDOWN/SETUP cycles. Kept
        // ALWAYS-ON deliberately: docs/wireless/00_WIRELESS_CARPLAY.md and docs/wireless/00_WIRELESS_CARPLAY.md both tell the operator this capture needs no
        // env var, and docs/wireless/00_WIRELESS_CARPLAY.md records them clearing the file by hand "so the next session's capture
        // starts from zero" — this automates that, so a pulled file is unambiguously the session
        // under test. Gating it behind CARPLAY_EVENT_DUMP would break both procedures and conflate
        // two different wire formats (that dump writes full plaintext with RTSP headers; this writes
        // the plist alone, which is what scratchpad/decode_cmd_capture.py parses).
        static TRUNCATED: std::sync::Once = std::sync::Once::new();
        TRUNCATED.call_once(|| {
            let _ = std::fs::File::create(path);
        });
        let grown = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if grown < CAP_MAX {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = f.write_all(&(body.len() as u32).to_le_bytes());
                let _ = f.write_all(body);
            }
        }
    }
    let Ok(Value::Dictionary(d)) = plist::Value::from_reader(std::io::Cursor::new(body)) else {
        return;
    };
    let ty = d.get("type").and_then(|v| v.as_string()).unwrap_or("");
    if ty.to_lowercase().contains("iap") {
        // The wireless-only iAP2-over-AirPlay tunnel carrier (Apple's `AirPlayReceiverSessionSendiAPMessage`
        // / `kAirPlayCommand_iAPSendMessage`): `{type:"iAPSendMessage", params:{data: <raw iAP2 msg>}}`.
        // `data` (lowercase) confirmed via the R14G17 SDK header's `#define kAirPlayKey_Data "data"` —
        // try it first, but keep the other spellings as a hedge in case iOS's inbound-to-us shape differs
        // from the outbound-from-us shape (unconfirmed; scratchpad/decode_cmd_capture.py hedges the same way).
        eprintln!("[events] *** iAP2-OVER-AIRPLAY TUNNEL FRAME (inbound, {} B) *** type='{ty}'", pt.len());
        let params = d.get("params").and_then(|v| v.as_dictionary());
        let data = ["data", "Data", "_data", "_Data"]
            .iter()
            .find_map(|k| params.and_then(|p| p.get(k)).and_then(|v| v.as_data()));
        match data {
            // docs/wireless/00_WIRELESS_CARPLAY.md: try the tunnel's OWN link/Identify handshake FIRST — while it's in flight
            // (not yet Identified), every inbound frame is handshake traffic (SYN-ACK, cert/sign
            // requests, IdentifyAccept/Rejected), not a metadata reply.
            //
            // CORRECTED 2026-07-25: `handle_inbound` no longer returns false once Identified. It keeps
            // servicing the established link — parsing, ACKing and dispatching each frame internally —
            // because abandoning it left every metadata update un-ACKed and retransmitted. It returns
            // false in exactly two cases, both of which correctly fall through to the bare dispatcher
            // below: no tunnel session exists at all, or this delivery is not link-framed — at ANY
            // state, which is the deliberate fallback for the possibility that iOS speaks bare payloads
            // on this channel after all. (An earlier revision restricted that second case to
            // `>= Identified`, which meant a bare payload arriving pre-Identify was swallowed instead
            // of dispatched — worse than the behaviour it replaced, and precisely in the scenario the
            // fallback exists for. Fixed.) Exactly one dispatch happens per message on every path.
            Some(data) if crate::iap_tunnel::handle_inbound(data) => {}
            Some(data) => dispatch_iap_tunnel_message(data),
            None => eprintln!("[events] '{ty}' had no recognizable Data param — can't route it"),
        }
        // docs/wireless/00_WIRELESS_CARPLAY.md #2.5: reply ONLY to this (iAP-tunnel) request type, and ONLY under the wireless gate —
        // deliberately NOT a blanket reply for every inbound request. Apple's own reference never
        // replies on this channel at all (confirmed against the real SDK: inbound `EVENT/1.0` messages
        // are consumed with no reply path), and this project's own wired capture evidence shows the
        // PROVEN wired session works with zero replies, including repeated `modesChanged` — so a
        // blanket reply would be genuinely unproven behavior. This is narrowly scoped in case the
        // iAP2-tunnel exchange specifically is what's stalling waiting for an ack iOS never gets.
        if wireless_metadata() {
            if let Some(cseq) = inbound_cseq(&text) {
                send_event_reply(cseq);
            }
        }
    } else if ty == "modesChanged" {
        modes_changed(&d);
    } else if ty == "disableBluetooth" {
        // docs/wireless/00_WIRELESS_CARPLAY.md: Apple's own Integration Guide says "iAP2 over Bluetooth must not be disconnected
        // until the disableBluetooth command is received" — i.e. this is the phone's signal that it's
        // now safe to let the BT link go. Recognized/logged here for observability; NOT wired to an
        // actual cross-process BT teardown action yet (that would mean signalling
        // `carplay-wireless`, a separate process, from here) because a 12-Fable review of `bt_driver.
        // rs::run()` found it does NOT disconnect proactively once past this point: its only two
        // accessory-initiated closes are the 120s pre-Identify handshake budget (can't fire on an
        // already-operating session) and `Action::Abort` on a phone-sent 0xAA04
        // AuthenticationFailed (phone-signaled, not a spontaneous accessory decision) — neither is
        // "disconnect early because we felt like it", so the "must not disconnect early" requirement
        // is satisfied in practice, just not by an absolute "never closes" guarantee.
        // This is confirmed, real device evidence (the wired `/command` capture decoded a
        // `disableBluetooth` entry in its type histogram) that this command DOES arrive — not a
        // guess.
        eprintln!("[events] disableBluetooth received (BT link may now be released; no action taken)");
    } else if events_log() {
        eprintln!("[events] ← iPhone event: type='{ty}' ({} B)", pt.len());
    }
}

/// Extract the `CSeq` header value from an inbound request's decoded text (headers precede the binary
/// plist body, so this is safe even though the body portion may contain non-UTF-8 replacement chars
/// from the lossy conversion in [`handle_inbound_event`]). Used by the docs/wireless/00_WIRELESS_CARPLAY.md #2.5 conditional reply.
fn inbound_cseq(text: &str) -> Option<&str> {
    text.lines().find_map(|l| l.strip_prefix("CSeq:").map(|v| v.trim()))
}

/// Handle one inbound `modesChanged`, from EITHER channel.
///
/// `modesChanged` carries the current resource-ownership snapshot. iOS grants us MainScreen while
/// CarPlay is foregrounded and revokes it when something else (a call/Siri/native UI) takes over.
/// Best-effort ownership read, kept conservative so an unrecognized shape defaults to "still focused"
/// and never spuriously reports focus-loss.
///
/// FIXED 2026-09-01: this lived inline in [`handle_inbound_event`], i.e. only on the EVENT channel —
/// the same mistake the nudge below was already fixed for. Every observed `modesChanged` arrives on
/// the CONTROL channel (`session.rs::command`), so the focus flag never updated. Both callers now
/// come here.
pub(crate) fn modes_changed(d: &Dictionary) {
    let owned = modes_screen_owned(d);
    let prev = SCREEN_FOCUSED.swap(owned, Ordering::AcqRel);
    if owned != prev {
        eprintln!("[events] modesChanged: MainScreen focus {}", if owned { "REGAINED" } else { "LOST" });
    }
    // docs/wireless/00_WIRELESS_CARPLAY.md #2.8: one-shot wireless-metadata re-subscribe, piggybacking on this already-parsed
    // inbound event as a cheap "something changed, retry" signal (see METADATA_RESUBSCRIBED's doc).
    modes_changed_tunnel_nudge();
}

/// Best-effort read of MainScreen ownership from a `modesChanged` dict. Scans any `resources`/`modes`
/// array for a `resourceID == 1` (MainScreen) entry and reads its `entity` — the CURRENT owner.
/// Conservative: returns `true` (we still own it) unless a taken-away MainScreen is clearly present,
/// so an unverified message shape can never fabricate a focus-loss.
///
/// FIXED 2026-09-01: this read `transferType`, which an INBOUND `modesChanged` never carries — it is a
/// `changeModes` REQUEST key (`AirPlayCommon.h:1160`, which is why `send_take_screen` writes it), so
/// the lookup never matched and the function always returned true. Apple's inbound parser
/// (`AirPlayReceiverSessionMakeModeStateFromDictionary`) reads `resources[]{resourceID, entity}`, and
/// all 35 `modesChanged` frames in `docs/ops/captures/2026-07-24_carplay_cmd_capture.bin` carry
/// exactly `{resourceID, entity, permanentEntity}` — no `transferType` anywhere.
fn modes_screen_owned(d: &Dictionary) -> bool {
    for key in ["resources", "modes"] {
        if let Some(arr) = d.get(key).and_then(|v| v.as_array()) {
            for e in arr {
                let Some(ed) = e.as_dictionary() else { continue };
                let is_main_screen = ed
                    .get("resourceID")
                    .and_then(|v| v.as_signed_integer())
                    .map(|id| id == 1)
                    .unwrap_or(false);
                if !is_main_screen {
                    continue;
                }
                // `entity` names who holds the resource now: 2 = kAirPlayEntity_Accessory (us),
                // anything else (controller/host UI) means the screen was taken away.
                if let Some(ent) = ed.get("entity").and_then(|v| v.as_signed_integer()) {
                    return ent == 2;
                }
            }
        }
    }
    true
}

struct EventSender {
    stream: TcpStream,
    chan: ControlChannel,
    cseq: u32,
}

/// Wire the encrypted event channel after the iPhone connects it (RECORD). Spawns a background reader
/// that drains inbound bytes (responses / iPhone event notifications) so the socket never backs up.
pub fn setup(stream: TcpStream, shared: [u8; 32], alive: Arc<AtomicBool>) {
    // QC 2026-07-25 (defensive): a fresh event channel means any prior tunnel iAP2 session is dead by
    // definition, so clear it unconditionally here. Tracing showed the stale-session path is not
    // reachable today (a second RECORD without a phase-1 SETUP is a no-op for this function, a fresh
    // phase-1 SETUP only follows a full TEARDOWN -> reset(), and the accept loop is strictly serial
    // with the displaced session's Drop -> reset() ordered before the next serve) — but the ONE
    // residual sequence, iOS spontaneously re-SETUPing mid-session with no TEARDOWN, would otherwise
    // leave `start()` below looking at a stale `Identified` state and silently skipping the handshake
    // for a connection that never had one. This line costs nothing and closes that.
    crate::iap_tunnel::reset();
    // Symmetric to the reset above, and for the same reason: a re-SETUP without a TEARDOWN must not
    // inherit the previous session's sink. Without this, an RCS reader thread that had not yet observed
    // `alive == false` could `register()` its OLD socket after teardown's clear, and the next session's
    // entire handshake would be written into a dead connection — silently, because a write to a socket
    // the phone left open still succeeds and the POST fallback never engages.
    crate::datastream::clear();
    let chan = ControlChannel::new(derive_event_keys(&shared));
    // Bound the send (#108): `send_command` holds the global EVENT mutex across `write_all`. Without a
    // write timeout, an abrupt wireless drop (no FIN, kernel send buffer full) blocks that write_all
    // FOREVER while holding EVENT — wedging session teardown (`clear()` also locks EVENT) and every
    // other command for minutes. A 2s write timeout turns that into a bounded failure that releases the
    // lock and returns false, letting teardown proceed. Normal tiny commands complete in microseconds.
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
    // Clone for the reader BEFORE `stream` moves into the sender, but install the sender BEFORE the
    // reader thread starts: an inbound frame decrypted in the gap between the two would otherwise
    // reach `iap_tunnel::handle_inbound`/`send_event_reply`, find `EVENT == None`, and be dropped
    // silently. The window is microseconds and the phone rarely speaks first here, so this is
    // ordering hygiene rather than an observed loss.
    let reader = stream.try_clone().ok();
    SCREEN_FOCUSED.store(true, Ordering::Release); // we take the screen at RECORD; reset per session
    METADATA_RESUBSCRIBED.store(false, Ordering::Release); // fresh one-shot retry budget per session (#2.8)
    // docs/carplay/02_SESSION_LIFECYCLE.md: not started until `record()` says so. Cleared HERE as well as in `clear()` so a session
    // that reconnects without a clean teardown can't inherit the previous session's started state.
    SESSION_STARTED.store(false, Ordering::Release);
    *crate::plock(&EVENT) = Some(EventSender { stream, chan, cseq: 0 });
    if let Some(mut reader) = reader {
        // Read timeout + session-liveness check so the drain thread exits when the session ends —
        // otherwise an ABRUPT drop (no FIN) leaves `read()` blocked forever and the thread leaks per
        // session (the `EVENT` sender is cleared on teardown, but this `try_clone` keeps the fd open).
        reader.set_read_timeout(Some(Duration::from_millis(500))).ok();
        let mut rx_chan = ControlChannel::new(derive_event_keys(&shared));
        std::thread::spawn(move || {
            let mut acc: Vec<u8> = Vec::new();
            let mut buf = [0u8; 4096];
            'reader: loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        // EOF — iPhone closed the event channel (graceful).
                        eprintln!("[events] reader: EOF — iPhone closed the event channel");
                        break;
                    }
                    Ok(n) => {
                        // Decrypt + log iOS→receiver event-channel messages (modesChanged / setUpStreams /
                        // changeModes — the audio-routing negotiation). Previously drained unparsed.
                        acc.extend_from_slice(&buf[..n]);
                        loop {
                            match rx_chan.decrypt_frame(&acc) {
                                Ok(Some((pt, used))) => {
                                    acc.drain(..used);
                                    // Parse the message: surface command-response status (#121) and track
                                    // MainScreen focus from modesChanged (#47), instead of the old
                                    // drain-and-only-log-behind-an-env-flag.
                                    handle_inbound_event(&pt);
                                    if let Some(p) = event_dump_path() {
                                        use std::io::Write as _;
                                        if let Ok(mut f) = std::fs::OpenOptions::new()
                                            .create(true).append(true).open(p)
                                        {
                                            let _ = f.write_all(&(pt.len() as u32).to_le_bytes());
                                            let _ = f.write_all(&pt);
                                        }
                                    }
                                }
                                Ok(None) => break, // need more bytes for a full frame
                                Err(e) => {
                                    // Authenticated stream: the read counter does not advance on a
                                    // failed decrypt, so once we desync EVERY later inbound frame
                                    // fails too — while our outbound half keeps "succeeding" into a
                                    // channel the phone can no longer talk back on. Stop reading
                                    // entirely (mirrors the RCS reader's counter-desync policy in
                                    // session.rs) instead of clearing the buffer and grinding on a
                                    // permanently-dead channel. The variant matters: `Oversized` is
                                    // a protocol violation, not a counter desync.
                                    eprintln!(
                                        "[events] reader: decrypt failed ({e:?}, {} B buffered) — \
                                         stopping the inbound reader",
                                        acc.len()
                                    );
                                    break 'reader;
                                }
                            }
                        }
                    }
                    Err(ref e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        if !alive.load(Ordering::Acquire) {
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("[events] reader: read error: {e} — stopping the inbound reader");
                        break;
                    }
                }
            }
        });
    }
    eprintln!("[events] encrypted command channel ready");
    // NOTE 2026-07-25 (docs/carplay/02_SESSION_LIFECYCLE.md): the iAP2-over-AirPlay tunnel used to be opened HERE, immediately after
    // the event channel came up. That was too early and has been moved to the END of `session.rs::
    // record()`. This function is our `_ControlStart` equivalent — Apple sets `sessionStarted` (the hard
    // gate on every command send, `AirPlayReceiverSession.c:825`) only after `_ControlStart` AND the
    // audio threads AND `_ScreenStart` AND a successful platform `kAirPlayCommand_StartSession` have all
    // returned `kNoErr` (`:1108-1152`). Opening the tunnel from here sent our first `iAPSendMessage`
    // several steps before Apple's own code permits any send at all. Do not move it back.
}

/// Tear down the event channel (TEARDOWN / session end).
pub fn clear() {
    *crate::plock(&EVENT) = None;
    SESSION_STARTED.store(false, Ordering::Release); // docs/carplay/02_SESSION_LIFECYCLE.md: no sends valid again until the next RECORD
    // Reset the cluster-forward gate so a `CMD_NAV_START` that wasn't followed by a `CMD_NAV_STOP`
    // can't leak into the next session and forward the cluster from the start (audit LOW). Default off.
    set_nav_forward(false);
    crate::iap_tunnel::reset(); // docs/wireless/00_WIRELESS_CARPLAY.md: a future reconnect must start its OWN fresh iAP2 handshake
    crate::datastream::clear(); // docs/carplay/05_METADATA_AND_CONTROLS.md: the RCS socket dies with the session; don't write to a stale fd
}

/// Send a binary-plist `{type, …}` command to the iPhone as an encrypted `POST /command` (fire-and-
/// forget). Returns false if no event channel is active or the write fails.
pub fn send_command(body_plist: &[u8]) -> bool {
    // Poison-tolerant: see `crate::plock` (this mutex is held across a bounded socket write).
    let mut guard = crate::plock(&EVENT);
    let Some(ev) = guard.as_mut() else {
        return false;
    };
    ev.cseq += 1;
    let header = format!(
        "POST /command RTSP/1.0\r\nCSeq: {}\r\nContent-Type: application/x-apple-binary-plist\r\nContent-Length: {}\r\n\r\n",
        ev.cseq,
        body_plist.len()
    );
    let mut msg = header.into_bytes();
    msg.extend_from_slice(body_plist);
    let frame = ev.chan.encrypt_frame(&msg);
    if write_frame_or_fail(&mut ev.stream, &frame) {
        return true;
    }
    // Fail closed: the counter is burned and the stream may carry a partial frame, so the channel is
    // unusable. Drop it rather than writing frame N+1 behind a truncated N.
    eprintln!("[events] dropping event channel — outbound frame could not be completed");
    *guard = None;
    false
}


/// Write an already-encrypted frame, retrying transient stalls, and FAIL CLOSED if it cannot finish.
///
/// `encrypt_frame` advances the ChaCha20 write counter before the write is attempted, so these exact
/// bytes are the only ones the peer can decrypt at that counter value. `write_all` under the socket's
/// 2 s `SO_SNDTIMEO` can return `Err` AFTER a partial write — leaving a truncated frame on the wire,
/// the counter burned, and the next frame queued behind it. That is a permanent framing AND counter
/// desync: the phone's transport hits a size error or a verify failure and drops the session.
///
/// Apple's reference does not lose these: `_NetTransportWriteV` buffers the encrypted frame and
/// resumes from `writeBufferedPtr` on the next call, so it never skips a counter. We retry to the
/// same end, and if the bytes genuinely cannot be delivered we return false so the caller tears the
/// channel down rather than continuing on a desynced one. The RCS datastream path already does this
/// (`datastream::write_all_retrying`); the event channel did not, and was strictly worse because it
/// also kept the sender installed.
fn write_frame_or_fail(stream: &mut TcpStream, frame: &[u8]) -> bool {
    use std::io::ErrorKind::{Interrupted, TimedOut, WouldBlock};
    const DEADLINE: Duration = Duration::from_secs(6); // 3x the socket's own write timeout
    let start = Instant::now();
    let mut sent = 0usize;
    while sent < frame.len() {
        match stream.write(&frame[sent..]) {
            Ok(0) => return false,
            Ok(n) => sent += n,
            Err(e) if matches!(e.kind(), WouldBlock | TimedOut | Interrupted) => {
                if start.elapsed() >= DEADLINE {
                    eprintln!(
                        "[events] TX stalled at {sent}/{} B ({e}) — giving up, channel is desynced",
                        frame.len()
                    );
                    return false;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                eprintln!("[events] TX failed at {sent}/{} B: {e}", frame.len());
                return false;
            }
        }
    }
    true
}

/// Reply to an inbound request with a bare `RTSP/1.0 200 OK`, echoing its `CSeq` (docs/wireless/00_WIRELESS_CARPLAY.md #2.5). Uses
/// the SAME send-direction event channel/key as [`send_command`] (never the reader thread's own
/// `rx_chan`) — deliberately does NOT touch `ev.cseq` (that counter is for OUR own outbound requests,
/// not replies to inbound ones). No lock is held by the caller across this call, so there's no
/// re-entrancy/deadlock risk locking `EVENT` here.
fn send_event_reply(cseq: &str) -> bool {
    // Poison-tolerant: see `crate::plock` (this mutex is held across a bounded socket write).
    let mut guard = crate::plock(&EVENT);
    let Some(ev) = guard.as_mut() else {
        return false;
    };
    let msg = format!("RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nContent-Length: 0\r\n\r\n");
    let frame = ev.chan.encrypt_frame(msg.as_bytes());
    if write_frame_or_fail(&mut ev.stream, &frame) {
        return true;
    }
    eprintln!("[events] dropping event channel — outbound reply could not be completed");
    *guard = None;
    false
}

/// Build + send the iAP2-over-AirPlay tunnel command — the wireless-only carrier Apple's SDK uses to
/// relay a raw iAP2 message (the `[0x40,0x40][len BE16][msg-id BE16][body]` shape from
/// `iap2_core::link::msg_payload`) over the encrypted event channel, instead of the BT/USB iAP2 link.
/// Mirrors the C `AirPlayReceiverSessionSendiAPMessage`: `{type:"iAPSendMessage", params:{data: <bytes>}}`
/// (`AirPlayReceiverSession.c:5331-5364`; the `type` key is set at `:5350`). The `data` key is LOWERCASE
/// — confirmed against the R14G17 SDK
/// header (`AirPlayCommon.h`: `#define kAirPlayKey_Data "data"`); an earlier revision of this function
/// used `"Data"` (capital), which a live device test confirmed iOS rejects with a uniform
/// `RTSP/1.0 400 Bad Request` regardless of payload. Returns false if no event channel is active or
/// the write fails (same fire-and-forget contract as [`send_command`]).
pub fn send_iap_message(raw: &[u8]) -> bool {
    // docs/carplay/05_METADATA_AND_CONTROLS.md: prefer the RCS DataStream when one is live. That is where modern iOS actually carries
    // wireless iAP2 — it answers there (its SYN|ACK was observed on it) and never on this path. Every
    // `iap_tunnel` send funnels through this function, so routing here redirects the whole link
    // (DETECT, SYN, ACKs, Identify, the metadata subscribes) in one place.
    //
    // The `POST /command` path below is retained as the fallback for the pre-RCS window and for any
    // iOS build that still accepts it: iOS 2xx-acks it, so it is harmless when it is not the real
    // carrier, and it is the only option until the phone has opened the channel.
    if crate::datastream::send(raw) {
        return true;
    }
    let mut params = Dictionary::new();
    params.insert("data".into(), Value::Data(raw.to_vec()));
    let mut d = Dictionary::new();
    d.insert("type".into(), Value::String("iAPSendMessage".into()));
    d.insert("params".into(), Value::Dictionary(params));
    let mut body = Vec::new();
    if Value::Dictionary(d).to_writer_binary(&mut body).is_err() {
        return false;
    }
    send_command(&body)
}

/// One-shot "something changed, retry the tunnel link" nudge, piggybacked on an inbound
/// `modesChanged` (docs/wireless/00_WIRELESS_CARPLAY.md #2.8, docs/wireless/00_WIRELESS_CARPLAY.md). Reached from [`modes_changed`], which both inbound paths
/// call, sharing one atomic so it still fires at most once per session regardless of which channel
/// delivers the event.
///
/// FIXED 2026-07-25: this used to live only in `handle_inbound_event` (the EVENT channel), and the
/// first successful wireless session showed why that was useless — all 8 `modesChanged` of that
/// session arrived on the CONTROL channel (`session.rs::command`), and the event channel received no
/// inbound traffic at all. The nudge therefore never fired once, so the retry docs/wireless/00_WIRELESS_CARPLAY.md #2.8 added had
/// never actually been exercised.
pub(crate) fn modes_changed_tunnel_nudge() {
    // docs/carplay/02_SESSION_LIFECYCLE.md: `modesChanged` is delivered on the CONTROL connection (`session.rs::command()`), which is
    // served independently of RECORD — so without this gate an early `modesChanged` could open the tunnel
    // before the session start sequence had completed, which is exactly what the move of the primary
    // start site out of `events::setup()` was meant to prevent. Checked BEFORE the one-shot swap so a
    // too-early nudge doesn't silently consume the session's single retry.
    if !session_started() {
        return;
    }
    if wireless_metadata() && !METADATA_RESUBSCRIBED.swap(true, Ordering::AcqRel) {
        eprintln!("[events] modesChanged: one-shot iAP2-tunnel link nudge (docs/wireless/00_WIRELESS_CARPLAY.md #2.8, docs/wireless/00_WIRELESS_CARPLAY.md)");
        crate::iap_tunnel::start();
    }
}

/// Subscribe to the wireless metadata feed (NowPlaying/RouteGuidance/CallState) over the iAP2-over-
/// AirPlay tunnel — the fix attempt for docs/wireless/00_WIRELESS_CARPLAY.md's wireless-metadata gap. Wired sessions already get
/// these from `iap2d`'s own physical iAP2 link (over `/dev/android_iap2`) and never call this.
///
/// WHY THIS IS NEEDED: the wireless BT identify (`crates/vendor/wireless/src/bt_driver.rs`,
/// `build_ident_info_excluding`'s `TransportComponent::Wireless` arm) deliberately declares NEITHER
/// these message ids NOR the `Start*Updates` subscribes over BT — doing so diverts iOS into plain
/// media-accessory behavior and breaks the WiFi handoff (device-observed, see that file's comment).
/// Once the handoff has completed and the AirPlay session is up, that risk no longer applies, so this
/// sends the SAME subscribe bodies `iap2d` sends wired, but over the AirPlay tunnel instead.
///
/// UNVERIFIED ON HARDWARE: whether iOS honors a message id as "receivable" only when the ORIGINAL
/// identify (over BT) declared it, or accepts any subscribe arriving on this tunnel regardless, is an
/// open question this send is designed to answer — watch `/tmp/carplay_event_capture.bin` (captured by
/// [`handle_inbound_event`]) on the next live wireless session for a NowPlayingUpdate/RouteGuidanceUpdate/
/// CallStateUpdate frame coming back. Gated behind `CARPLAY_WIRELESS_METADATA=1` (default off) so
/// deploying this can't regress the proven wireless session.
pub(crate) fn send_wireless_metadata_subscriptions() {
    // The subscribe list and the Identify declaration are generated from the SAME table
    // (`iap2_core::features`, docs/carplay/05_METADATA_AND_CONTROLS.md) — that is the whole point of the table. Sending a subscribe
    // for an id param 6 does not declare is a silent no-op, which is how 0x4157/0x4170 came to be
    // sent for the project's entire history with 0x4158/0x4171 undeclared.
    let features = iap2_core::features::active(
        iap2_core::features::Policy::active().subscribe,
        iap2_core::metadata::start_now_playing,
        iap2_core::metadata::start_route_guidance,
    );
    let subs: Vec<(u16, String, Vec<u8>)> = features
        .iter()
        .filter_map(|f| {
            let start = f.start()?;
            Some((start, format!("0x{start:04X} {}", f.name), f.build_body()))
        })
        .collect();
    eprintln!(
        "[events] iAP2-tunnel metadata: {} subscribes ({})",
        subs.len(),
        features.iter().map(|f| f.name).collect::<Vec<_>>().join(", ")
    );
    std::thread::spawn(move || {
        for (id, name, body) in subs {
            let (ok, n) = crate::iap_tunnel::send_subscribe(id, &body);
            eprintln!(
                "[events] TX iAP2-tunnel {name} ({n} B, link-framed) → {}",
                if ok { "sent" } else { "FAILED (no tunnel session / no event channel?)" }
            );
            std::thread::sleep(Duration::from_millis(50)); // don't burst the tunnel
        }
    });
}

/// Route one tunneled iAP2 message to the SAME metadata parsers `iap2d` uses on the wired physical
/// link, forwarding decoded JSON to the host's existing `:9004` Metadata seam (`emit_json` owns that
/// connection). Accepts either candidate framing (unconfirmed which iOS actually uses, matching
/// `scratchpad/decode_cmd_capture.py`'s `framing_of`): the bare `msg_payload` shape
/// (`[0x40,0x40][len BE16][msg-id BE16][body]`), or that same shape wrapped in a 9-byte iAP2 LINK header
/// (`iap2_core::link`'s `[SOP1 SOP2][len][ctl][seq][ack][sess][hdr_cks]`, i.e. `link.build_msg`'s output)
/// — strip the first 9 bytes and retry once if the bare shape isn't found at offset 0.
pub(crate) fn dispatch_iap_tunnel_message(data: &[u8]) {
    use iap2_core::metadata;
    let is_4040 = |b: &[u8]| b.len() >= 6 && b[0] == 0x40 && b[1] == 0x40;
    let payload = if is_4040(data) {
        data
    } else if data.len() > 9 && data[0] == 0xFF && data[1] == 0x5A && is_4040(&data[9..]) {
        eprintln!("[events] iAP tunnel Data is FF5A-link-wrapped — stripping 9-byte header");
        &data[9..]
    } else {
        eprintln!(
            "[events] iAP tunnel Data ({} B) not in a recognized shape: {:02x?}",
            data.len(),
            &data[..data.len().min(8)]
        );
        return;
    };
    // docs/wireless/00_WIRELESS_CARPLAY.md #2.7: clamp to the declared BE16 length at payload[2..4] before slicing the body. Without
    // this, `&payload[6..]` ran to the end of whatever buffer was selected above — for the FF5A-wrapped
    // case that's the ORIGINAL `data` buffer, so a trailing link-frame checksum byte silently leaked
    // into the parsed TLV body. Confirmed dead in practice today (this project's own outbound sends are
    // always bare `msg_payload`, never FF5A-wrapped, and docs/carplay/05_METADATA_AND_CONTROLS.md found Apple's real chain never sends
    // this framing either) — fixed for hygiene, not because it's currently reachable.
    let declared = payload
        .get(2..4)
        .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
        .unwrap_or(payload.len());
    let payload: &[u8] = if (6..=payload.len()).contains(&declared) {
        &payload[..declared]
    } else {
        payload
    };
    let msg_id = u16::from_be_bytes([payload[4], payload[5]]);
    let body = &payload[6..];
    eprintln!("[events] iAP tunnel msg 0x{msg_id:04x} ({} B body)", body.len());
    // One dispatcher, shared with the wired driver (`iap2_core::metadata::dispatch`). These two
    // paths diverged once — 0x4171 ListUpdate was handled wired and silently dropped here — and the
    // shared function exists so they cannot again.
    if !metadata::dispatch(msg_id, body) {
        eprintln!("[events] iAP tunnel msg 0x{msg_id:04x} not yet handled");
    }
}

/// Request a fresh IDR keyframe from the iPhone — the C's ForceKeyFrame (`Session.c:4916`:
/// `{kAirPlayKey_Type: kAirPlayCommand_ForceKeyFrame}` = `{type:"forceKeyFrame"}`) over the encrypted
/// event channel. CarPlay sends the SPS/PPS + IDR only at stream start, so a video consumer that joins
/// mid-stream sees P-frames only (black); requesting a keyframe on (re)connect gives it a fresh
/// config + IDR to start decoding. Returns false if no event channel is active or the write fails.
pub fn send_force_key_frame() -> bool {
    send_force_key_frame_stream(None)
}

/// Force-keyframe for a SPECIFIC video stream (#129). The main console stream keyframes fine with no
/// `streamID` (its omission = the default main stream), but the type-111 instrument-cluster stream only
/// re-IDRs when the request names it (`streamID: "VideoStream.Alt1"`, see [`CLUSTER_STREAM_ID`]); without
/// the id, a cluster consumer that (re)joins after the nav-forward gate opens gets P-frames only = a
/// black cluster window until iOS happens to emit its own IDR.
pub fn send_force_key_frame_stream(stream_id: Option<&str>) -> bool {
    let mut d = Dictionary::new();
    d.insert("type".into(), Value::String("forceKeyFrame".into()));
    // SHAPE CORRECTED 2026-07-30: was a TOP-LEVEL `streamID`. `_AirPlayReceiverSessionForceKeyFrame`
    // @0x26ba84 builds `{type:"forceKeyFrame", params:{uuid:<streamUUID>}}` — nested, and keyed `uuid`.
    // Wrong key AND wrong nesting meant the stream selector was invisible to iOS, so a cluster consumer
    // that rejoined got P-frames only until iOS emitted its own IDR. The BARE form (no id) is left
    // exactly as-is: it is the proven main-stream path and must not regress.
    if let Some(sid) = stream_id {
        let mut params = Dictionary::new();
        params.insert("uuid".into(), Value::String(sid.into()));
        d.insert("params".into(), Value::Dictionary(params));
    }
    let mut body = Vec::new();
    if Value::Dictionary(d).to_writer_binary(&mut body).is_err() {
        return false;
    }
    send_command(&body)
}

/// Change the cluster **map zoom level** — the Simulator's Alt1 zoom button pair (`+`/`-`). Cluster
/// only (VideoStream.Alt1), matching `SessionClusterMapZoomView` (cluster doc §8). The SDK exit
/// `AirPlayReceiverSessionChangeMapZoomLevel(session, CFStringRef streamUUID, AirPlayZoomDirection, …)`
/// → wire `{type:"changeMapZoomLevel", params:{uuid:<streamUUID>, zoomDirection:<0|1>}}` (param key
/// `zoomDirection` and type from the CarPlaySDK symbol table). `AirPlayZoomDirection`: **0 = in** (the
/// `+` button), **1 = out** (the `-` button). Returns false if no event channel is active or the write fails.
pub fn send_change_map_zoom(direction: u8) -> bool {
    let mut params = Dictionary::new();
    params.insert("uuid".into(), Value::String(CLUSTER_STREAM_ID.into()));
    params.insert("zoomDirection".into(), Value::Integer(i64::from(direction).into()));
    let mut d = Dictionary::new();
    d.insert("type".into(), Value::String("changeMapZoomLevel".into()));
    d.insert("params".into(), Value::Dictionary(params));
    let mut body = Vec::new();
    if Value::Dictionary(d).to_writer_binary(&mut body).is_err() {
        return false;
    }
    send_command(&body)
}

/// Actively **take the screen** (video focus) as a `changeModes` command over the event channel —
/// byte-exact to what the proven-working carplayd sends post-RECORD (`airplay_receiver_main.c:1363-1367`:
/// `AirPlayReceiverSessionTakeScreen(session, UserInitiated/500, Anytime/100, Anytime/100, "video focus")`
/// == `ChangeResourceMode(MainScreen, Take)`). CORRECTION 2026-07-02: an earlier version of this sent an
/// **Untake** of MainScreen+MainAudio @100 (a misread of a raw capture) — carplayd's shipped code proves
/// the Untake is WRONG ("Our old code UNTOOK (released) the screen, so iOS never brought an app foreground
/// or encoded video"). For a Take (`transferType=1`) the C `AirPlayCreateModesDictionary` serializes the
/// take/borrow constraints too, and the reason via `kAirPlayKey_ReasonStr` (`AirPlayUtils.c:549`). Wire:
/// `{type:"changeModes", params:{resources:[{resourceID:1, transferType:1, transferPriority:500,
/// takeConstraint:100, borrowConstraint:100}], reasonStr:"video focus"}}`.
///
/// NOTE (grounded skepticism): carplayd documents this as a *video-focus* fix; our video already renders
/// without it, so it may not affect the open **audio** blocker (iOS not taking MainAudio over wired — see
/// `docs/carplay/03_SDK_GROUND_TRUTH.md`). It is the one accessory-behavior difference vs the working flow, tested here as a clean A/B.
pub fn send_take_screen() -> bool {
    let mut r = Dictionary::new();
    r.insert("resourceID".into(), Value::Integer(1.into())); // MainScreen
    r.insert("transferType".into(), Value::Integer(1.into())); // Take
    r.insert("transferPriority".into(), Value::Integer(500.into())); // UserInitiated
    r.insert("takeConstraint".into(), Value::Integer(100.into())); // Anytime
    r.insert("borrowConstraint".into(), Value::Integer(100.into())); // Anytime
    let mut params = Dictionary::new();
    params.insert("resources".into(), Value::Array(vec![Value::Dictionary(r)]));
    params.insert("reasonStr".into(), Value::String("video focus".into()));
    let mut d = Dictionary::new();
    d.insert("type".into(), Value::String("changeModes".into()));
    d.insert("params".into(), Value::Dictionary(params));
    let mut body = Vec::new();
    if Value::Dictionary(d).to_writer_binary(&mut body).is_err() {
        return false;
    }
    send_command(&body)
}

/// Ask the controller to bring the accessory's UI forward — `requestUI` with no URL, emitted right after
/// the initial `changeModes` (carplayd Add #6). Wire (`AirPlayReceiverSessionRequestUI`, NULL url):
/// `{type:"requestUI"}` (no `params`).
pub fn send_request_ui() -> bool {
    let mut d = Dictionary::new();
    d.insert("type".into(), Value::String("requestUI".into()));
    // EMPTY `params` ADDED 2026-07-30. `_AirPlayReceiverSessionRequestUI` (CarPlaySDK 509.11
    // @0x26bf20) branches at 0x26bf5c around ONLY the `url` set; the `params` attach at 0x26bf6c is
    // UNCONDITIONAL. So Apple sends `{type:"requestUI", params:{}}` when there is no url, and we were
    // sending `{type:"requestUI"}`. Low severity — but this is the authority, and a bare dict is a
    // byte-level divergence on a live path (session.rs RECORD, and OCBM CMD_REQUEST_UI).
    d.insert("params".into(), Value::Dictionary(Dictionary::new()));
    let mut body = Vec::new();
    if Value::Dictionary(d).to_writer_binary(&mut body).is_err() {
        return false;
    }
    send_command(&body)
}

/// Wire value of the `streamID` param for the instrument-cluster (type-111) stream. VERIFIED by
/// disassembling the authoritative standalone CarPlay Simulator: `AirPlayController.sendShowUI`
/// switches the `VideoStreamID` enum into a literal string and bridges it straight to the CFString
/// passed as `streamID` (app offsets 0x100012008–0x100012044). Main console = "VideoStream.Main",
/// cluster #1 (type-111) = "VideoStream.Alt1", cluster #2 (type-112) = "VideoStream.Alt2".
pub const CLUSTER_STREAM_ID: &str = crate::info::ALT_DISPLAY_UUID;

// ⚠️ VALUE CORRECTED 2026-07-30. This was the literal `"VideoStream.Alt1"`, copied from the
// Simulator's own config. That is the SIMULATOR's display uuid — it matches NOTHING in our session.
// The uuid iOS expects is whatever WE put in `/info` `displays[].uuid` for the type-111 entry, which
// it echoes back at SETUP phase 2 (`_ScreenSetup` reads params["uuid"]). For us that is
// `info::ALT_DISPLAY_UUID`. Addressing a stream by a foreign uuid is unaddressable, not merely wrong.

/// `showUI` — select which cluster CONTENT the controller renders into an ALREADY-established
/// stream (map / instructioncard / instrumentcluster). This is the command the authoritative CarPlay
/// Simulator's content picker uses — NOT `requestUI` (verified: the picker's `sendShowUI` calls
/// `_AirPlayReceiverSessionShowUI` @0x26cdf4, which builds `{type:"showUI", params:{streamID, url?}}`;
/// `streamID` is REQUIRED, `url` optional). Content-only: it does not create/destroy the stream (the
/// stream is enabled by advertising `altScreen` in `/info` → auto-SETUP, and the box `nav_forward`
/// gate controls whether its frames reach the host).
pub fn send_show_ui(stream_id: &str, url: &str) -> bool {
    let mut params = Dictionary::new();
    // KEY CORRECTED 2026-07-30: `uuid`, not `streamID`. `_AirPlayReceiverSessionShowUI` @0x26cdf4
    // writes params["uuid"], and the SDK's INBOUND dispatcher (@0x2697b4) reads params["uuid"] too —
    // same name both directions. A `streamID` key is silently ignored, so the stream was never
    // addressed and the content selection never landed.
    params.insert("uuid".into(), Value::String(stream_id.into()));
    params.insert("url".into(), Value::String(url.into()));
    let mut d = Dictionary::new();
    d.insert("type".into(), Value::String("showUI".into()));
    d.insert("params".into(), Value::Dictionary(params));
    let mut body = Vec::new();
    if Value::Dictionary(d).to_writer_binary(&mut body).is_err() {
        return false;
    }
    send_command(&body)
}

/// `stopUI{streamID}` — clear the cluster content on a stream (the "None" picker option), leaving the
/// "No Instrument Cluster Content" placeholder. VERIFIED: the Simulator's `sendStopUI` calls
/// `_AirPlayReceiverSessionStopUI` @0x26ceb0, which builds `{type:"stopUI", params:{streamID}}` — keyed
/// by `streamID` with NO url (a url here would be ignored). Content-only; does not stop the stream (the
/// box `nav_forward` gate does that — iOS keeps encoding while a route is active regardless).
pub fn send_stop_ui(stream_id: &str) -> bool {
    let mut params = Dictionary::new();
    // KEY CORRECTED 2026-07-30 — see `send_show_ui`. `_AirPlayReceiverSessionStopUI` @0x26ceb0 writes
    // ONLY params["uuid"] and never a url.
    params.insert("uuid".into(), Value::String(stream_id.into()));
    let mut d = Dictionary::new();
    d.insert("type".into(), Value::String("stopUI".into()));
    d.insert("params".into(), Value::Dictionary(params));
    let mut body = Vec::new();
    if Value::Dictionary(d).to_writer_binary(&mut body).is_err() {
        return false;
    }
    send_command(&body)
}

/// Whether the host wants the ALT / cluster (type-111) stream FORWARDED over OCBM. Default `false`:
/// the box DROPS cluster frames so the second stream can't steal OCBM/USB bandwidth from the main 4K
/// video (observed: main stalled 8s + cut in/out while the cluster streamed). iOS keeps encoding the
/// cluster while a nav route is active regardless of `stopUI` (focus release is not a reliable gate),
/// so we gate at the box's forward instead. The host toggles this via CMD_NAV_START/CARD/APP (→ true)
/// and CMD_NAV_STOP (→ false); see `spawn_screen`.
static NAV_FORWARD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set whether the cluster (type-111) stream is forwarded to the host (true) or dropped at the box (false).
pub fn set_nav_forward(on: bool) {
    NAV_FORWARD.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the cluster stream should currently be forwarded over OCBM.
pub fn nav_forward() -> bool {
    NAV_FORWARD.load(std::sync::atomic::Ordering::Relaxed)
}

/// Mirror of [`set_dpad_advertised`] for the two-finger touchscreen. The HID-ingest path and
/// `info.rs` must agree on this or the report layout will not match the advertised descriptor.
pub fn set_multi_touch_advertised(on: bool) {
    crate::levers::set_multi_touch(on);
}

/// True when the two-finger descriptor was advertised for this connection.
pub fn multi_touch_advertised() -> bool {
    crate::levers::multi_touch()
}

/// Set from `load_device_config` per connection (true = the D-Pad device is advertised). Now a thin
/// wrapper over [`crate::levers::set_dpad`] — the atomic that used to live here was the prototype
/// for the generalized lever mirror (`crate::levers`), and folding it in keeps ONE cell that both
/// the `/info` builder and the HID-ingest gate read.
pub fn set_dpad_advertised(on: bool) {
    crate::levers::set_dpad(on);
}

/// Whether the D-Pad device is advertised — the HID-input thread reads this instead of the env var.
pub fn dpad_advertised() -> bool {
    crate::levers::dpad()
}

/// Set per connection (true = the Knob device is advertised). Thin wrapper over [`crate::levers::set_knob`]
/// so the `/info` builder and the INPUT_KNOB ingest gate read one cell.
pub fn set_knob_advertised(on: bool) {
    crate::levers::set_knob(on);
}

/// Whether the Knob device is advertised — the INPUT_KNOB ingest reads this before sending a report.
pub fn knob_advertised() -> bool {
    crate::levers::knob()
}

/// Set per connection (true = the Telephony device is advertised). Thin wrapper over
/// [`crate::levers::set_telephony`] so the `/info` builder and the INPUT_TELEPHONY ingest read one cell.
pub fn set_telephony_advertised(on: bool) {
    crate::levers::set_telephony(on);
}

/// Whether the Telephony device is advertised — the INPUT_TELEPHONY ingest reads this before sending.
pub fn telephony_advertised() -> bool {
    crate::levers::telephony()
}

/// `setLimitedUI` — restrict the CarPlay UI as if the vehicle shifted into Drive (`true`), or release
/// it when parked (`false`). Runtime `/command` on the event channel; NO reconnect, NO `/info` change,
/// NO SETUP feature negotiation (Apple `AirPlayReceiverSessionSetLimitedUI`, `kAirPlayCommand_SetLimitedUI`).
/// iOS restricts the on-screen keyboard, phone dial keypad and long scrollable lists. The bool is
/// nested under `params` (as with `requestUI`/`stopUI`). Which elements restrict is an optional `/info`
/// `limitedUIElements` list; absent, iOS applies its default set.
pub fn send_set_limited_ui(limit: bool) -> bool {
    let mut params = Dictionary::new();
    params.insert("limitedUI".into(), Value::Boolean(limit));
    let mut d = Dictionary::new();
    d.insert("type".into(), Value::String("setLimitedUI".into()));
    d.insert("params".into(), Value::Dictionary(params));
    let mut body = Vec::new();
    if Value::Dictionary(d).to_writer_binary(&mut body).is_err() {
        return false;
    }
    send_command(&body)
}

/// `AirPlayAppearanceMode` wire value (`CarPlaySDK` `AppearanceMode.airPlayValue = rawValue`, the
/// Simulator's Swift enum in declaration order): Light = 0, Dark = 1.
pub const APPEARANCE_MODE_LIGHT: i64 = 0;
pub const APPEARANCE_MODE_DARK: i64 = 1;

/// `AirPlayAppearanceSetting` wire value (`CarPlaySDK` `AppearanceSetting.airPlayValue = rawValue`):
/// Automatic = 0, UserChoice = 1, Always = 2. Apple's Simulator `setUIAppearanceMode` sends
/// `appearanceSetting = .automatic` as a HARDCODED CONSTANT (`mov w19,#0` at its @0x10000ff08 body) on
/// EVERY explicit UI/Map appearance command, with the `appearanceMode` carried on a fully independent
/// register — so Automatic cannot make the mode inert, or Apple's own pickers would be inert. This is
/// the only known-working reference value; do NOT "upgrade" it to UserChoice/Always (that would be a
/// deviation, not a fix). If a hardware A/B ever shows the toggle inert, the first suspect is the
/// missing per-display `uiAppearanceModes`/`mapAppearanceModes` advertisement in `/info` (which the
/// Simulator emits and we do not), NOT this setting.
pub const APPEARANCE_SETTING_AUTOMATIC: i64 = 0;

/// `uiAppearanceUpdate` — tell iOS to render the CarPlay **UI** on a given display in light or dark
/// mode, mirroring the vehicle system state (the Simulator's "UI Appearance" picker /
/// `SessionClusterAppearanceView`). Runtime `/command`; NO reconnect, NO `/info` change. Verified from
/// Apple's `CarPlaySDK` `_AirPlayReceiverSessionUIAppearanceUpdate` @0x26c318: it builds
/// `{type:"uiAppearanceUpdate", params:{uuid:<streamUUID>, appearanceMode:<int>, appearanceSetting:<int>}}`
/// — the generic keys `appearanceMode`/`appearanceSetting` (NOT `uiAppearanceMode`), the command *type*
/// distinguishes UI from Map. `uuid` is OUR display uuid (main = [`crate::info::DISPLAY_UUID`], cluster =
/// [`CLUSTER_STREAM_ID`]); the Simulator passes the literal "VideoStream.Main"/".Alt1" because those ARE
/// its config's display uuids — the same lesson as `send_show_ui`, address by the uuid iOS echoed at SETUP.
pub fn send_ui_appearance_update(stream_uuid: &str, dark: bool) -> bool {
    send_appearance_update("uiAppearanceUpdate", stream_uuid, dark)
}

/// `mapAppearanceUpdate` — same as [`send_ui_appearance_update`] but toggles the **map content** light/dark
/// on the named display (the Simulator's "Map Appearance" picker / `SessionClusterMapAppearanceView`).
/// `CarPlaySDK` `_AirPlayReceiverSessionMapAppearanceUpdate` @0x26c41c: byte-identical params to the UI
/// form (`{uuid, appearanceMode, appearanceSetting}`); only the `type` string differs.
pub fn send_map_appearance_update(stream_uuid: &str, dark: bool) -> bool {
    send_appearance_update("mapAppearanceUpdate", stream_uuid, dark)
}

/// Shared body for the UI/Map appearance commands — identical param dict, only the `type` differs.
fn send_appearance_update(type_str: &str, stream_uuid: &str, dark: bool) -> bool {
    let mut params = Dictionary::new();
    params.insert("uuid".into(), Value::String(stream_uuid.into()));
    params.insert(
        "appearanceMode".into(),
        Value::Integer(
            if dark { APPEARANCE_MODE_DARK } else { APPEARANCE_MODE_LIGHT }.into(),
        ),
    );
    params.insert(
        "appearanceSetting".into(),
        Value::Integer(APPEARANCE_SETTING_AUTOMATIC.into()),
    );
    let mut d = Dictionary::new();
    d.insert("type".into(), Value::String(type_str.into()));
    d.insert("params".into(), Value::Dictionary(params));
    let mut body = Vec::new();
    if Value::Dictionary(d).to_writer_binary(&mut body).is_err() {
        return false;
    }
    send_command(&body)
}

/// `setNightMode` — tell iOS whether the vehicle reports it is night (`true`) or day (`false`). GLOBAL,
/// not per-display, and it is one input (alongside iOS's own logic) into whether the CarPlay UI goes
/// dark — distinct from the explicit UI/Map appearance above. Verified TWO ways: `CarPlaySDK`
/// `_AirPlayReceiverSessionSetNightMode` @0x26bfb4, and Apple's licensed R14G17 source
/// (`AirPlayReceiverSession.c:5278-5282`), which both build `{type:"setNightMode", params:{nightMode:<bool>}}`.
pub fn send_set_night_mode(on: bool) -> bool {
    let mut params = Dictionary::new();
    params.insert("nightMode".into(), Value::Boolean(on));
    let mut d = Dictionary::new();
    d.insert("type".into(), Value::String("setNightMode".into()));
    d.insert("params".into(), Value::Dictionary(params));
    let mut body = Vec::new();
    if Value::Dictionary(d).to_writer_binary(&mut body).is_err() {
        return false;
    }
    send_command(&body)
}

/// Activate Siri — `requestSiri` over the encrypted event channel. VALIDATED NEGATIVE 2026-07-11:
/// the bare `{type:"requestSiri"}` shape dispatches cleanly but iOS does NOT react. The SDK
/// (docs/20 §2.4, `_AirPlayReceiverSessionRequestSiriActionInternal` + the adjacent
/// `AirPlaySiriAction` enum) shows Siri-via-button is a **HOLD**: `siriAction: 2` on press, `3` on
/// release. Those are INTEGER enum values — the names ("buttondown"/"buttonup"/"prewarm"/
/// "voiceactivation") are log labels only; iOS reads the key with `CFDictionaryGetInt64`.
/// Kept for A/B reference; the real path is [`send_request_siri_action`].
pub fn send_request_siri() -> bool {
    let mut d = Dictionary::new();
    d.insert("type".into(), Value::String("requestSiri".into()));
    let mut body = Vec::new();
    if Value::Dictionary(d).to_writer_binary(&mut body).is_err() {
        return false;
    }
    send_command(&body)
}

/// Siri hold-pair leg: `{type:"requestSiri", params:{siriAction:<int>}}`. `siriAction` is the
/// **`AirPlaySiriAction` INTEGER enum**, NOT a string — the SDK reads it via `CFDictionaryGetInt64`
/// (`RequestSiriActionInternal`), so a string value never matches and Siri does nothing (the
/// 2026-07-12 bug; docs/20 §2.4b).
///
/// ⚠️ VALUES CORRECTED 2026-07-30 — the old comment said "prewarm=0, buttondown=1, buttonup=2,
/// voiceactivation=4", which is off by one AND self-inconsistent (a 0,1,2,4 enum with a hole).
/// Apple's `AirPlaySiriAction` (`AirPlayCommon.h:1363-1386`) is:
///   0 = n/a · **1 = prewarm** · **2 = buttondown** · **3 = buttonup** · 4 = voiceactivation
/// The button is a HOLD: send **2** on press, **3** on release. `siriTriggerTimestamp` /
/// `siriTriggerZone` are optional (the SDK's plain button path passes NULL) — omitted;
/// `siriTriggerZone` is only ever set when `siriAction == 4`.
///
/// Also corrected: the old claim that "no mic uplink is required; classic Siri uses the phone's own
/// mic" is WRONG. cp.log:1840 shows that after buttonup, iOS SETUPs `MainAudio … for
/// speechRecognition, direction InputOutput` — the ACCESSORY mic is the uplink even for classic Siri.
pub fn send_request_siri_action(action: i64) -> bool {
    let mut params = Dictionary::new();
    params.insert("siriAction".into(), Value::Integer(action.into()));
    let mut d = Dictionary::new();
    d.insert("type".into(), Value::String("requestSiri".into()));
    d.insert("params".into(), Value::Dictionary(params));
    let mut body = Vec::new();
    if Value::Dictionary(d).to_writer_binary(&mut body).is_err() {
        return false;
    }
    send_command(&body)
}

/// Build + send a HID input report (`hidSendReport`) for device `uid` (touchscreen = 1, media = 2).
/// Mirrors the C `AirPlayReceiverSessionSendHIDReport`: `{type:"hidSendReport", uuid:<hex>, hidReport}`.
pub fn send_hid_report(uid: u32, report: &[u8]) -> bool {
    let mut d = Dictionary::new();
    d.insert("type".into(), Value::String("hidSendReport".into()));
    d.insert("uuid".into(), Value::String(format!("{uid:X}")));
    d.insert("hidReport".into(), Value::Data(report.to_vec()));
    let mut body = Vec::new();
    if Value::Dictionary(d).to_writer_binary(&mut body).is_err() {
        return false;
    }
    send_command(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one `resources[]` entry in the shape every `modesChanged` in
    /// `docs/ops/captures/2026-07-24_carplay_cmd_capture.bin` uses: `{resourceID, entity,
    /// permanentEntity}` — no `transferType`.
    fn resource(resource_id: i64, entity: i64) -> Value {
        let mut e = Dictionary::new();
        e.insert("resourceID".into(), Value::Integer(resource_id.into()));
        e.insert("entity".into(), Value::Integer(entity.into()));
        e.insert("permanentEntity".into(), Value::Integer(entity.into()));
        Value::Dictionary(e)
    }

    fn modes(resources: Vec<Value>) -> Dictionary {
        let mut d = Dictionary::new();
        d.insert("type".into(), Value::String("modesChanged".into()));
        d.insert("resources".into(), Value::Array(resources));
        d
    }

    #[test]
    fn modes_screen_owned_reads_entity_not_transfer_type() {
        // MainScreen (resourceID 1) held by the accessory (entity 2) = ours.
        assert!(modes_screen_owned(&modes(vec![resource(2, 2), resource(1, 2)])));
        // Same frame with MainScreen handed to the controller = focus lost. Before the 2026-09-01 fix
        // this returned true for every real frame, because it looked for a `changeModes` request key.
        assert!(!modes_screen_owned(&modes(vec![resource(2, 2), resource(1, 1)])));
    }

    #[test]
    fn modes_screen_owned_defaults_to_focused_on_an_unknown_shape() {
        assert!(modes_screen_owned(&modes(vec![])));
        assert!(modes_screen_owned(&modes(vec![resource(2, 1)]))); // MainAudio only, no MainScreen
        assert!(modes_screen_owned(&Dictionary::new()));
    }
}
