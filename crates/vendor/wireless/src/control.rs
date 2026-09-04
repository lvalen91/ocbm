//! Device-management control socket — the server half of the AAOS projection app's device screen.
//!
//! ⚠️ **PI-VERIFIED ONLY (2026-08-16). This module is new and runs on the CCPA too.**
//!
//! It was written for and exercised on the Raspberry Pi. `carplay-wireless` is a shared binary, so
//! a CCPA now also: binds `127.0.0.1:9115` for the process lifetime, and — when a policy is pushed
//! — writes `projection_policy.json` into `ssp_agent::state_dir()`, which on a CCPA is
//! `/etc/carplay`, i.e. **flash**. Only on a user toggle, so a handful of writes per box lifetime,
//! but it is a new flash write on a platform that had none from this crate.
//!
//! Nothing here has run on CCPA hardware. A failed bind is non-fatal by design (logged, device
//! management simply unavailable), so the worst case should be a log line — but "should" is doing
//! work in that sentence.
//!
//! ## Why this exists
//!
//! On the CCPA there was no such surface: the macOS app drove everything over OCBM, and the box that
//! owned the radio was the same box the app talked to. On the Raspberry Pi port the radio owner (this
//! process, which drives `hci0` directly) and the UI (`com.carlink.projection`) are separate
//! processes on one host, and the platform's own Bluetooth settings pane cannot substitute — Android's
//! Bluetooth stack is disabled precisely so this process can own the controller, so that pane has no
//! stack behind it and can neither list nor pair a phone.
//!
//! So the app needs somewhere to ask "what phones do I know, and connect to this one". This is it.
//!
//! ## Protocol
//!
//! Newline-delimited JSON over loopback TCP, one request line and one response line per connection.
//! Line-JSON rather than a binary framing on purpose: this is a low-rate control path, and being able
//! to drive it from `nc` during a bring-up session is worth more than efficiency. Every other seam in
//! this system is binary and awkward to inspect by hand.
//!
//! ```text
//!   {"cmd":"list"}     -> {"ok":true,"devices":[{"address":"AA:BB:CC:DD:EE:FF","name":"","bonded":true,"connected":false}]}
//!   {"cmd":"status"}   -> {"ok":true,"active":false,"address":"","autoConnect":true}
//!   {"cmd":"connect","address":"AA:BB:.."} -> {"ok":true}    // address optional: omit = first-to-connect
//!   {"cmd":"policy","autoConnect":false,"order":["AA:BB:.."]} -> {"ok":true}
//!   {"cmd":"forget","address":"AA:BB:.."}  -> {"ok":true}
//!   {"cmd":"pair_answer","accept":true}    -> {"ok":true}    // the head unit's yes/no for the
//!                                                            // Numeric-Comparison code on screen
//! ```
//!
//! ## Address byte order — the thing that will bite
//!
//! `ssp_agent::bonded_addrs` returns each bdaddr in the stored **mgmt little-endian** order, because
//! that is what `rfcomm::connect_to` and `sdp_client::query` want in their sockaddrs. A human-readable
//! `AA:BB:CC:DD:EE:FF` is the **reverse** of that. Every conversion in this file goes through
//! [`fmt_addr`] / [`parse_addr`], which reverse, and nothing else formats an address — getting this
//! wrong yields a device list that looks plausible and connects to nothing.
//!
//! ## What is deliberately NOT implemented
//!
//! `disconnect` returns an error rather than a lie. Tearing down a live session needs a cancellation
//! hook inside `bt_driver::run`, which does not exist; bolting one on blind would risk the proven
//! session path for a convenience command. The app surfaces the failure rather than showing a button
//! that silently does nothing.
//!
//! Device *names* are not available here. The phone's name arrives in the iAP2 Identify, which
//! `airplayd` sees and this process does not, so `name` is always empty and the app falls back to the
//! address. Plumbing it is a follow-up, not a guess.

// `Read` is needed for `Read::take` on the request reader — without it in scope the
// method call resolves to `Iterator::take` and fails to compile against BufReader.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use crate::ssp_agent;

/// Loopback only. Binding every interface would expose device management to the CarPlay SoftAP —
/// the one network where an untrusted device is by definition present.
pub const CONTROL_PORT: u16 = 9115;

/// Bounds a stuck client so one bad connection cannot hold the single-threaded accept loop.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

/// A request line longer than this is not a request we understand. Bounded before allocation.
const MAX_REQUEST_BYTES: usize = 8 * 1024;

/// `@<unix_ms> ` write-time stamp (docs/carplay/01_OCBM_PROTOCOL.md CH_LOG): the box.log tailer
/// parses this prefix and uses it instead of the millisecond it happened to READ the line at.
fn log(m: &str) {
    println!("@{} [control] {m}", now_ms());
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---- address formatting ---------------------------------------------------------------------

/// mgmt little-endian bdaddr -> the conventional big-endian display form.
pub fn fmt_addr(a: &[u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        a[5], a[4], a[3], a[2], a[1], a[0]
    )
}

/// Display form -> mgmt little-endian bdaddr. Accepts `:` or `-` separators, any case.
pub fn parse_addr(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.split([':', '-']).collect();
    if parts.len() != 6 {
        return None;
    }
    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        let b = u8::from_str_radix(p, 16).ok()?;
        out[5 - i] = b; // reverse into mgmt order
    }
    Some(out)
}

// ---- minimal JSON ---------------------------------------------------------------------------
//
// Hand-rolled rather than pulling in serde: this crate is deliberately dependency-light (see its
// Cargo.toml), the request shape is fixed and tiny, and a full parser would be more code than the
// server. These readers are intentionally forgiving about whitespace and key order and strict about
// nothing else — a malformed request simply fails to match and yields an error response.

/// Extract a string value for `"key"`. Handles the `\"` escape only; no other escape appears in a
/// bdaddr or a command name, and a value containing one simply fails to match.
fn json_str(src: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let i = src.find(&pat)? + pat.len();
    let rest = &src[i..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let mut chars = after.char_indices();
    if chars.next()?.1 != '"' {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    for (_, c) in chars {
        if escaped {
            out.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    None
}

/// Extract a boolean value for `"key"`.
fn json_bool(src: &str, key: &str) -> Option<bool> {
    let pat = format!("\"{key}\"");
    let i = src.find(&pat)? + pat.len();
    let rest = &src[i..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    if after.starts_with("true") {
        Some(true)
    } else if after.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// Extract an array of strings for `"key"`.
fn json_str_array(src: &str, key: &str) -> Option<Vec<String>> {
    let pat = format!("\"{key}\"");
    let i = src.find(&pat)? + pat.len();
    let rest = &src[i..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    if !after.starts_with('[') {
        return None;
    }
    let end = after.find(']')?;
    let body = &after[1..end];
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for c in body.chars() {
        match (c, cur.as_mut()) {
            ('"', None) => cur = Some(String::new()),
            ('"', Some(_)) => out.push(cur.take().unwrap()),
            (ch, Some(s)) => s.push(ch),
            _ => {}
        }
    }
    Some(out)
}

/// Is `key` present as an object key at all?
///
/// The typed readers cannot tell "the client omitted this field" from "the field is there and
/// malformed" — both are `None` — and those two deserve OPPOSITE answers: keep the current value,
/// versus refuse the request.
fn json_has_key(
    src: &str,
    key: &str,
) -> bool {
    let pat = format!("\"{key}\"");
    match src.find(&pat) {
        Some(i) => src[i + pat.len()..].trim_start().starts_with(':'),
        None => false,
    }
}

/// Escape a string for embedding in our responses.
///
/// Control characters MUST be escaped, not only quote and backslash. The one string we echo back is
/// an unknown `cmd`, and the client is Java's `BufferedReader.readLine()`, which terminates a line
/// on a BARE CR as well as on LF — so a raw CR inside an echoed command cut our response in half and
/// the app saw a JSONException instead of the error we were trying to report.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// ---- shared state ---------------------------------------------------------------------------

/// Connection policy, owned here and consulted by `reconnect`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// When false the reconnect loop stays idle unless the app asks for a specific phone —
    /// GM's "tap to connect". When true it drives the order below on its own.
    pub auto_connect: bool,
    /// First-to-connect order. Addresses not in the bond list are ignored; bonded addresses not in
    /// this list are tried after it, so a newly paired phone is never invisible to reconnect.
    pub order: Vec<[u8; 6]>,
}

impl Default for Policy {
    fn default() -> Self {
        // Auto-connect on by default: a head unit that needs a tap before it will talk to an
        // already-paired phone is not what a driver expects, and it is what the stock firmware does.
        Self { auto_connect: true, order: Vec::new() }
    }
}

/// Everything the control socket reads or writes. ONE instance for the PROCESS lifetime — it
/// outlives any single session, because `main`'s claim loop re-enters `run_active_session`.
#[derive(Default)]
pub struct Control {
    policy: RwLock<Policy>,
    /// An explicit connect request from the app, consumed by the reconnect loop.
    ///
    /// `Some(None)` means "connect to whatever is next in the order" — i.e. the app pressed connect
    /// without naming a device. `Some(Some(addr))` names one. `None` means nothing pending.
    #[allow(clippy::option_option)]
    pending: Mutex<Option<Option<[u8; 6]>>>,
    /// The peer of the live session, when it is KNOWN. Only the reconnect path knows the bdaddr —
    /// the accept path is handed an already-open socket — so this stays `None` through an
    /// accept-path session. That is exactly why it must never be the source of truth for "is a
    /// session live".
    session_peer: RwLock<Option<[u8; 6]>>,
    /// The single-session claim, SHARED with `main`'s accept loop and `reconnect`. THIS is what
    /// `active`/`connected` report.
    ///
    /// Deriving `active` from `session_peer` alone made every accept-path session — first pairing
    /// and every phone-initiated connect — report `active:false`, and the app then offered
    /// **Forget** for the phone that was actively projecting, which deletes a live phone's link key.
    session_active: Arc<AtomicBool>,
    /// The head unit's yes/no for a Numeric-Comparison pairing prompt, read by the SSP agent's mgmt
    /// loop. PROCESS-lifetime like the rest of this struct, and cloned into each session's agent
    /// thread — pairing happens on the accept path, before any session exists, so a per-session
    /// answer slot would be created after the prompt it is meant to answer.
    pair_answer: Arc<ssp_agent::PairAnswer>,
}

impl Control {
    /// `session_active` is the SAME `Arc` the accept path and `reconnect` claim.
    pub fn new(session_active: Arc<AtomicBool>) -> Self {
        Self {
            policy: RwLock::new(load_policy()),
            session_active,
            ..Default::default()
        }
    }

    /// The slot the SSP agent polls for the head unit's pairing answer. Clone it into the agent
    /// thread; `handle_request`'s `pair_answer` verb writes it.
    pub fn pair_answer(&self) -> Arc<ssp_agent::PairAnswer> {
        self.pair_answer.clone()
    }

    /// True while a session is live on EITHER path, named peer or not.
    pub fn session_active(&self) -> bool {
        self.session_active.load(Ordering::Acquire)
    }

    pub fn policy(&self) -> Policy {
        self.policy.read().map(|p| p.clone()).unwrap_or_default()
    }

    pub fn set_policy(&self, p: Policy) {
        if let Ok(mut w) = self.policy.write() {
            *w = p.clone();
        }
        save_policy(&p);
    }

    /// Take any pending connect request. Called by the reconnect loop each round.
    pub fn take_request(&self) -> Option<Option<[u8; 6]>> {
        self.pending.lock().ok().and_then(|mut p| p.take())
    }

    /// Is a connect request waiting? Peeks rather than takes, so the reconnect loop's backoff sleep
    /// can cut short without consuming the request it is about to act on.
    pub fn has_request(&self) -> bool {
        self.pending.lock().map(|p| p.is_some()).unwrap_or(false)
    }

    pub fn request_connect(&self, addr: Option<[u8; 6]>) {
        if let Ok(mut p) = self.pending.lock() {
            *p = Some(addr);
        }
    }

    pub fn set_session_peer(&self, addr: Option<[u8; 6]>) {
        if let Ok(mut w) = self.session_peer.write() {
            *w = addr;
        }
    }

    pub fn session_peer(&self) -> Option<[u8; 6]> {
        self.session_peer.read().ok().and_then(|r| *r)
    }

    /// Bonded phones in the order reconnect should try them.
    pub fn ordered_bonds(&self) -> Vec<[u8; 6]> {
        order_bonds(&ssp_agent::bonded_addrs(), &self.policy().order)
    }
}

/// RAII holder of the single-session claim. Clears the flag AND the published peer on every exit
/// from the claimed region.
///
/// ⚠️ Scope note so nobody over-trusts this: the shipped binaries build `--release` and the
/// workspace sets `panic = "abort"`, so a panic terminates the process WITHOUT unwinding and this
/// `Drop` never runs for one. Its value is structural — the claim and its release cannot drift
/// apart as the block grows, which is what had already happened between the two call sites — plus
/// correctness under `cargo test`. Real panic resilience is a supervisor's job, not a guard's.
pub struct SessionClaim<'a> {
    flag: &'a AtomicBool,
    ctrl: &'a Control,
}

impl<'a> SessionClaim<'a> {
    /// Claim the slot, or `None` if another path owns it. `compare_exchange`, not a store: the
    /// ~16 s SDP+RFCOMM connect in `reconnect::attempt` can overlap an inbound accept.
    pub fn try_claim(
        flag: &'a AtomicBool,
        ctrl: &'a Control,
        peer: Option<[u8; 6]>,
    ) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        ctrl.set_session_peer(peer);
        Some(Self { flag, ctrl })
    }
}

impl Drop for SessionClaim<'_> {
    fn drop(&mut self) {
        self.ctrl.set_session_peer(None);
        self.flag.store(false, Ordering::Release);
    }
}

/// First-to-connect ordering: policy order first (for entries that are actually bonded), then every
/// remaining bond in its stored order.
///
/// The tail is the load-bearing half. A strict "policy order only" rule would make a phone paired
/// AFTER the policy was last written invisible to reconnect forever — the same class of bug as the
/// snapshotted bond list that audit Fix #22 removed from `reconnect`.
///
/// Free function so it is testable without touching the on-disk link-key store.
pub fn order_bonds(bonds: &[[u8; 6]], order: &[[u8; 6]]) -> Vec<[u8; 6]> {
    let mut out: Vec<[u8; 6]> = order.iter().copied().filter(|a| bonds.contains(a)).collect();
    for b in bonds {
        if !out.contains(b) {
            out.push(*b);
        }
    }
    out
}

// ---- policy persistence ---------------------------------------------------------------------

/// The connection policy sits in the SAME directory as the link-key store, via the same helper —
/// not a second `CARPLAY_STATE_DIR` read with its own default.
///
/// They had diverged: this defaulted to `/tmp/carplay` while `ssp_agent` defaulted to
/// `/etc/carplay`, so on a CCPA (where the variable is unset) the bonds were persistent while the
/// policy deciding whether to auto-connect to them sat on tmpfs and vanished every reboot. The Pi
/// sets the variable, so both agreed there and the split was invisible on the only platform anyone
/// had tested.
fn policy_path() -> String {
    format!("{}/projection_policy.json", ssp_agent::state_dir())
}

fn load_policy() -> Policy {
    let Ok(s) = std::fs::read_to_string(policy_path()) else {
        // NO FILE is not corruption — it is a box nobody has configured. Auto-connect on is right
        // there (a head unit that needs a tap before it talks to a paired phone is not what a driver
        // expects) and matches the stock firmware.
        return Policy::default();
    };
    let Some(auto_connect) = json_bool(&s, "autoConnect") else {
        // The file EXISTS and does not parse: the user's setting is LOST. Guess in the direction
        // that cannot page a phone they asked us to leave alone. `unwrap_or(true)` inverted an
        // explicit "off" on every corrupt read — silently, and in the one direction with a
        // real-world consequence.
        log("policy file is unparseable — assuming autoConnect OFF until the app pushes a new one");
        return Policy { auto_connect: false, order: Vec::new() };
    };
    let order = json_str_array(&s, "order")
        .unwrap_or_default()
        .iter()
        .filter_map(|a| parse_addr(a))
        .collect();
    Policy { auto_connect, order }
}

fn save_policy(p: &Policy) {
    let order: Vec<String> = p.order.iter().map(|a| format!("\"{}\"", fmt_addr(a))).collect();
    let body = format!(
        "{{\"autoConnect\":{},\"order\":[{}]}}",
        p.auto_connect,
        order.join(",")
    );
    let path = policy_path();
    if let Some(dir) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Atomic: the app may read this while we write, and a torn document reads as "defaults".
    let tmp = format!("{path}.tmp");
    if std::fs::write(&tmp, &body).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
        return;
    }
    let _ = std::fs::write(&path, &body);
}

// ---- request handling -----------------------------------------------------------------------

/// Build a response for one request line. Pure, so it is fully testable without a socket.
pub fn handle_request(ctrl: &Control, line: &str) -> String {
    let Some(cmd) = json_str(line, "cmd") else {
        return r#"{"ok":false,"error":"no cmd"}"#.to_string();
    };
    match cmd.as_str() {
        "list" => {
            let peer = ctrl.session_peer();
            let active = ctrl.session_active();
            let items: Vec<String> = ctrl
                .ordered_bonds()
                .iter()
                .map(|a| {
                    format!(
                        r#"{{"address":"{}","name":"{}","bonded":true,"connected":{}}}"#,
                        fmt_addr(a),
                        // Always empty for now — see the module doc. The app falls back to the
                        // address rather than showing a placeholder that looks like a real name.
                        json_escape(""),
                        // Only a NAMED peer marks a row connected. An accept-path session is live
                        // with no bdaddr, so no row claims it and `status.active` is how the app
                        // learns a session exists at all. Do NOT "fix" this by marking every bond
                        // connected — that is a lie the UI would act on by offering Forget.
                        active && peer == Some(*a)
                    )
                })
                .collect();
            format!(r#"{{"ok":true,"devices":[{}]}}"#, items.join(","))
        }
        "status" => {
            let peer = ctrl.session_peer();
            format!(
                r#"{{"ok":true,"active":{},"address":"{}","name":"","autoConnect":{}}}"#,
                ctrl.session_active(),
                peer.map(|a| fmt_addr(&a)).unwrap_or_default(),
                ctrl.policy().auto_connect
            )
        }
        "connect" => {
            let addr = json_str(line, "address");
            match addr {
                Some(s) => match parse_addr(&s) {
                    Some(a) => {
                        ctrl.request_connect(Some(a));
                        log(&format!("connect requested for {}", fmt_addr(&a)));
                        r#"{"ok":true}"#.to_string()
                    }
                    None => r#"{"ok":false,"error":"bad address"}"#.to_string(),
                },
                None => {
                    ctrl.request_connect(None);
                    log("connect requested (first-to-connect)");
                    r#"{"ok":true}"#.to_string()
                }
            }
        }
        "policy" => {
            // FAIL CLOSED, and distinguish absent from malformed.
            //
            // `unwrap_or(true)` turned a malformed request into "auto-connect ON" — the opposite of
            // the only setting a user bothers to send, in the direction that pages a phone. And
            // `order` falling back to empty is worse on THIS path than on load, because this one
            // SAVES: a malformed order would overwrite the stored one with nothing.
            let auto_connect = match json_bool(line, "autoConnect") {
                Some(v) => v,
                None if !json_has_key(line, "autoConnect") => ctrl.policy().auto_connect,
                None => {
                    return r#"{"ok":false,"error":"policy: autoConnect must be true or false"}"#
                        .to_string()
                }
            };
            let order = match json_str_array(line, "order") {
                Some(v) => v.iter().filter_map(|a| parse_addr(a)).collect(),
                None if !json_has_key(line, "order") => ctrl.policy().order,
                None => {
                    return r#"{"ok":false,"error":"policy: order must be an array of addresses"}"#
                        .to_string()
                }
            };
            let p = Policy { auto_connect, order };
            log(&format!(
                "policy: autoConnect={} order={} device(s)",
                p.auto_connect,
                p.order.len()
            ));
            ctrl.set_policy(p);
            r#"{"ok":true}"#.to_string()
        }
        // The user's answer to the Numeric-Comparison prompt (macOS app → CT_PAIR_CONFIRM → ocbmd →
        // here). FAIL CLOSED, like `policy`: a malformed or absent `accept` is refused, never read
        // as "pair" — an unparseable request must not be able to complete a bond nobody confirmed.
        //
        // Answering is always `{"ok":true}` even with no prompt outstanding: this port cannot see
        // whether the agent still has one pending (it may have just timed out), and reporting an
        // error for a race the caller cannot avoid would only teach the app to ignore the field.
        // The agent drops a stray answer rather than banking it.
        "pair_answer" => match json_bool(line, "accept") {
            Some(accept) => {
                ctrl.pair_answer.set(accept);
                log(&format!(
                    "pairing answer from the head unit: {}",
                    if accept { "PAIR" } else { "CANCEL" }
                ));
                r#"{"ok":true}"#.to_string()
            }
            None => r#"{"ok":false,"error":"pair_answer: accept must be true or false"}"#.to_string(),
        },
        "forget" => match json_str(line, "address").as_deref().and_then(parse_addr) {
            Some(a) => {
                if ssp_agent::forget_bond(&a) {
                    log(&format!("forgot {}", fmt_addr(&a)));
                    r#"{"ok":true}"#.to_string()
                } else {
                    r#"{"ok":false,"error":"not bonded"}"#.to_string()
                }
            }
            None => r#"{"ok":false,"error":"bad address"}"#.to_string(),
        },
        // Honest refusal rather than a no-op that reports success. See the module doc.
        "disconnect" => {
            r#"{"ok":false,"error":"unsupported: no session cancellation hook in bt_driver"}"#
                .to_string()
        }
        other => format!(r#"{{"ok":false,"error":"unknown cmd {}"}}"#, json_escape(other)),
    }
}

// ---- server ---------------------------------------------------------------------------------

/// Bind ONCE, for the process lifetime. Spawns its own thread and returns immediately.
///
/// Called from `main` BEFORE the claim loop, never from `run_active_session`, and that is
/// load-bearing. `run_active_session` is re-entered on every arbiter preempt/re-claim; a bind per
/// entry left the PREVIOUS listener owning the port forever — `for stream in listener.incoming()`
/// blocks in `accept()`, and a shutdown check there only runs once a connection arrives. The second
/// bind then failed EADDRINUSE and every request landed on an ORPHANED `Control`: `connect` queued
/// into a `pending` nobody drained, `status`/`list` read state nothing updated. All silently, with
/// every response still `{"ok":true}`.
///
/// Second reason, and this one bites today: the old call site sat after `bt_bringup::bring_up`, so
/// while bring-up was failing and retrying the socket was never bound at all and the app could not
/// even list or forget a phone. Binding here also keeps device management alive while WIRED holds
/// the arbiter — exactly when a driver goes looking at the screen.
///
/// The listener dies with the process, so there is deliberately no shutdown observation. Do not
/// reintroduce a per-session bind to make one meaningful.
pub fn serve(ctrl: Arc<Control>) {
    std::thread::spawn(move || {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, CONTROL_PORT);
        let listener = match TcpListener::bind(addr) {
            Ok(l) => l,
            Err(e) => {
                // Non-fatal by design: the accessory stack works without a UI, and refusing to
                // start the whole daemon because a management port is taken would be a worse
                // failure than losing the device screen.
                log(&format!("bind 127.0.0.1:{CONTROL_PORT} failed: {e} — device management unavailable"));
                return;
            }
        };
        log(&format!("device management on 127.0.0.1:{CONTROL_PORT}"));
        for stream in listener.incoming() {
            match stream {
                Ok(s) => serve_one(&ctrl, s),
                Err(e) => log(&format!("accept failed: {e}")),
            }
        }
    });
}

/// One request, one response, close. Served inline rather than on a thread per connection: requests
/// are trivial and bounded by CLIENT_TIMEOUT, and a thread per connection would let a misbehaving
/// client spawn without limit.
fn serve_one(ctrl: &Control, stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut line = String::new();
    // take() bounds the read BEFORE allocation — an endless line from a hostile or broken client
    // would otherwise grow `line` without limit.
    let mut limited = (&mut reader).take(MAX_REQUEST_BYTES as u64);
    if limited.read_line(&mut line).is_err() || line.trim().is_empty() {
        return;
    }
    let resp = handle_request(ctrl, line.trim());
    let mut out = stream;
    let _ = out.write_all(resp.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addr_round_trips_through_the_display_form() {
        // mgmt little-endian: the display form is the REVERSE. This test is the guard on the one
        // conversion that silently produces a plausible-but-wrong device list.
        let mgmt = [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66];
        assert_eq!(fmt_addr(&mgmt), "66:55:44:33:22:11");
        assert_eq!(parse_addr("66:55:44:33:22:11"), Some(mgmt));
    }

    #[test]
    fn parse_addr_accepts_dashes_and_lowercase_and_rejects_junk() {
        assert_eq!(parse_addr("aa-bb-cc-dd-ee-ff"), parse_addr("AA:BB:CC:DD:EE:FF"));
        assert_eq!(parse_addr("AA:BB:CC:DD:EE"), None);
        assert_eq!(parse_addr("ZZ:BB:CC:DD:EE:FF"), None);
        assert_eq!(parse_addr(""), None);
    }

    #[test]
    fn json_readers_extract_the_fields_we_use() {
        let s = r#"{"cmd":"policy", "autoConnect": false ,"order":["AA:BB:CC:DD:EE:FF","11:22:33:44:55:66"]}"#;
        assert_eq!(json_str(s, "cmd").as_deref(), Some("policy"));
        assert_eq!(json_bool(s, "autoConnect"), Some(false));
        assert_eq!(
            json_str_array(s, "order"),
            Some(vec![
                "AA:BB:CC:DD:EE:FF".to_string(),
                "11:22:33:44:55:66".to_string()
            ])
        );
        assert_eq!(json_str(s, "missing"), None);
        assert_eq!(json_bool(s, "missing"), None);
    }

    #[test]
    fn an_echoed_command_can_never_terminate_the_response_line() {
        // Java's BufferedReader.readLine() splits on a bare CR too, so an unescaped one truncates
        // the JSON the app is mid-parse on and it reports a JSONException instead of our error.
        let c = Control::default();
        let r = handle_request(&c, "{\"cmd\":\"a\rb\"}");
        assert!(!r.contains('\r') && !r.contains('\n'), "raw control char in response: {r:?}");
        assert!(r.contains("\\r"), "CR should be escaped: {r}");
    }

    /// `has_request` exists so the reconnect backoff sleep can cut short on a tap; it must PEEK,
    /// or the request it woke for would be gone by the time the loop reads it.
    #[test]
    fn has_request_peeks_and_take_request_consumes() {
        let c = Control::default();
        assert!(!c.has_request());
        c.request_connect(Some([1, 2, 3, 4, 5, 6]));
        assert!(c.has_request());
        assert!(c.has_request(), "peeking must not consume");
        assert_eq!(c.take_request(), Some(Some([1, 2, 3, 4, 5, 6])));
        assert!(!c.has_request());
    }

    #[test]
    fn json_str_handles_an_escaped_quote() {
        assert_eq!(json_str(r#"{"n":"a\"b"}"#, "n").as_deref(), Some("a\"b"));
    }

    #[test]
    fn unknown_and_malformed_requests_are_refused_not_ignored() {
        let c = Control::default();
        assert!(handle_request(&c, "not json").contains("\"ok\":false"));
        assert!(handle_request(&c, r#"{"cmd":"nope"}"#).contains("unknown cmd nope"));
        // A refusal, never a silent success — the app shows the failure.
        assert!(handle_request(&c, r#"{"cmd":"disconnect"}"#).contains("\"ok\":false"));
    }

    #[test]
    fn a_malformed_policy_request_is_refused_and_changes_nothing() {
        let c = Control::default();
        let before = c.policy();
        let r = handle_request(&c, r#"{"cmd":"policy","autoConnect":"maybe","order":[]}"#);
        assert!(r.contains("\"ok\":false"), "{r}");
        assert_eq!(c.policy(), before, "a refused request must not mutate the policy");
    }

    #[test]
    fn a_policy_request_that_omits_a_field_leaves_it_alone() {
        // The app's auto-connect switch sends ONLY autoConnect, because it does not manage the
        // first-to-connect order. An omitted field must therefore keep its stored value — sending
        // an empty array would wipe an order the UI never showed the user.
        let c = Control::default();
        c.set_policy(Policy { auto_connect: true, order: vec![[1, 0, 0, 0, 0, 0]] });
        assert!(handle_request(&c, r#"{"cmd":"policy","autoConnect":false}"#).contains("\"ok\":true"));
        assert!(!c.policy().auto_connect);
        assert_eq!(c.policy().order.len(), 1, "an omitted order must not wipe the stored one");
    }

    #[test]
    fn json_has_key_distinguishes_absent_from_malformed() {
        assert!(json_has_key(r#"{"autoConnect":false}"#, "autoConnect"));
        assert!(json_has_key(r#"{"autoConnect" : "bad"}"#, "autoConnect"));
        assert!(!json_has_key(r#"{"order":[]}"#, "autoConnect"));
    }

    #[test]
    fn connect_without_an_address_means_first_to_connect() {
        let c = Control::default();
        assert!(handle_request(&c, r#"{"cmd":"connect"}"#).contains("\"ok\":true"));
        // Some(None): a request is pending, naming no particular device.
        assert_eq!(c.take_request(), Some(None));
        // Consumed exactly once.
        assert_eq!(c.take_request(), None);
    }

    #[test]
    fn connect_with_an_address_parses_into_mgmt_order() {
        let c = Control::default();
        assert!(handle_request(&c, r#"{"cmd":"connect","address":"66:55:44:33:22:11"}"#)
            .contains("\"ok\":true"));
        assert_eq!(c.take_request(), Some(Some([0x11, 0x22, 0x33, 0x44, 0x55, 0x66])));
    }

    #[test]
    fn connect_with_a_bad_address_does_not_queue_anything() {
        let c = Control::default();
        assert!(handle_request(&c, r#"{"cmd":"connect","address":"nope"}"#).contains("\"ok\":false"));
        assert_eq!(c.take_request(), None);
    }

    #[test]
    fn a_pair_answer_reaches_the_slot_the_ssp_agent_polls() {
        let c = Control::new(Arc::new(AtomicBool::new(false)));
        let slot = c.pair_answer();
        assert!(handle_request(&c, r#"{"cmd":"pair_answer","accept":true}"#).contains("\"ok\":true"));
        assert_eq!(slot.take(), Some(true));
        assert!(handle_request(&c, r#"{"cmd":"pair_answer","accept":false}"#).contains("\"ok\":true"));
        assert_eq!(slot.take(), Some(false));
        assert_eq!(slot.take(), None);
    }

    #[test]
    fn a_malformed_pair_answer_is_refused_and_confirms_nothing() {
        // The failure that matters: junk being read as "pair" would complete a bond no human
        // approved, which is the whole reason this path exists.
        let c = Control::new(Arc::new(AtomicBool::new(false)));
        let slot = c.pair_answer();
        assert!(handle_request(&c, r#"{"cmd":"pair_answer","accept":"yes"}"#).contains("\"ok\":false"));
        assert!(handle_request(&c, r#"{"cmd":"pair_answer"}"#).contains("\"ok\":false"));
        assert_eq!(slot.take(), None);
    }

    #[test]
    fn status_reports_the_live_peer() {
        // The claim, not the peer, is what makes a session "active" — see the test below for why.
        let flag = Arc::new(AtomicBool::new(false));
        let c = Control::new(flag.clone());
        assert!(handle_request(&c, r#"{"cmd":"status"}"#).contains(r#""active":false"#));
        flag.store(true, Ordering::Release);
        c.set_session_peer(Some([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]));
        let r = handle_request(&c, r#"{"cmd":"status"}"#);
        assert!(r.contains(r#""active":true"#));
        assert!(r.contains("66:55:44:33:22:11"));
    }

    /// REGRESSION: an accept-path session is live with NO peer, and `active` must still be true.
    ///
    /// Deriving `active` from `session_peer` alone reported false for every accept-path session —
    /// first pairing and every phone-initiated connect — and the device screen then rendered that
    /// row as merely "Paired", offering **Forget**, which deletes the link key of the phone that is
    /// currently projecting.
    #[test]
    fn status_is_active_during_a_session_with_no_named_peer() {
        let flag = Arc::new(AtomicBool::new(false));
        let c = Control::new(flag.clone());
        flag.store(true, Ordering::Release);
        let r = handle_request(&c, r#"{"cmd":"status"}"#);
        assert!(r.contains(r#""active":true"#), "{r}");
        assert!(r.contains(r#""address":"""#), "no peer is known, so address stays empty: {r}");
    }

    /// The claim is released on EVERY exit from the guarded region, and a second claimer is refused
    /// while it is held.
    #[test]
    fn session_claim_releases_on_drop_and_refuses_a_second_claimer() {
        let flag = AtomicBool::new(false);
        let c = Control::default();
        {
            let _claim = SessionClaim::try_claim(&flag, &c, Some([1, 0, 0, 0, 0, 0])).unwrap();
            assert!(flag.load(Ordering::Acquire));
            assert_eq!(c.session_peer(), Some([1, 0, 0, 0, 0, 0]));
            assert!(
                SessionClaim::try_claim(&flag, &c, None).is_none(),
                "two concurrent bt_driver sessions against one phone must be impossible",
            );
        }
        assert!(!flag.load(Ordering::Acquire), "claim not released");
        assert_eq!(c.session_peer(), None, "peer not cleared — the screen would name a dead session");
    }

    #[test]
    fn policy_order_is_honoured_but_never_hides_a_new_bond() {
        // ordered_bonds() consults ssp_agent, so this exercises the pure ordering rule with an
        // explicit bond list instead.
        let ordered = order_bonds(
            &[[1, 0, 0, 0, 0, 0], [2, 0, 0, 0, 0, 0], [3, 0, 0, 0, 0, 0]],
            &[[3, 0, 0, 0, 0, 0], [9, 0, 0, 0, 0, 0]],
        );
        // 3 first (policy), then the rest in bond order. 9 is not bonded and is dropped; 1 and 2
        // are bonded but unlisted and MUST still appear, or a phone paired after the policy was
        // written would never be tried.
        assert_eq!(
            ordered,
            vec![[3, 0, 0, 0, 0, 0], [1, 0, 0, 0, 0, 0], [2, 0, 0, 0, 0, 0]]
        );
    }

}
