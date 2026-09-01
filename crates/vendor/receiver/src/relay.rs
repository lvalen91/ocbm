//! relay — the box side of the **app-driven SETUP** relay (plan: app-driven-SETUP, phase P1).
//!
//! ARCHITECTURE (owner-approved plan §"Settled decisions"): the relay shape is **pre-bind + oracle**.
//! The box runs today's unmodified [`AvSession`] FIRST (it binds, spawns, and performs every side
//! effect — its locally-authored response IS the port report), then relays the decrypted request
//! **plus that local response** to the host app in one message over OCBM `CH_RTSP` (0x0041). The host
//! authors the response the phone actually sees; ANY relay failure falls back to the in-hand local
//! response, so the phone never sees a relay-caused error. One USB RTT per exchange; `session.rs` is
//! functionally untouched.
//!
//! TRANSPORT: airplayd LISTENS here on `127.0.0.1:9106` (9110 = HID, 9112 = mic — both taken) and
//! ocbmd connects OUT to us, exactly the mic-seam mechanics (`ccpa/ocbmd` `ensure_rtsp_seam`). ocbmd
//! is a dumb byte pipe: it chunks seam bytes into ≤64 KiB OCBM frames both ways, so ALL message
//! framing here is endpoint-to-endpoint (this file ↔ the host app), which sidesteps OCBM's
//! MAX_PAYLOAD (64 KiB) < the RTSP MAX_BODY (256 KiB).
//!
//! SEAM FRAMING — `[u32 BE 0x52545350 "RTSP"][u32 BE len][msg]`, len ≤ [`RELAY_SEAM_MAX`]. The magic
//! is load-bearing: CH_RTSP rides ocbmd's out_hi queue, whose overflow policy is *clear the queue*
//! (no per-message resync), so a truncated frame can land mid-stream. The receiver resyncs by
//! scanning for the magic — the CH_METADATA-v2 "META" pattern (metadata.rs audit Fix #17). A lost
//! message is recovered by the rpc timeout → local fallback, never by wedging.
//!
//! CONSTANT MIRRORING: the `RS_*`/magic constants below are authoritative here and MIRRORED (not
//! imported) in `ocbm-proto`'s doc comments and the Swift/harness consumers — the metadata.rs
//! `META_*` pattern (this crate does not link ocbm-proto; each endpoint declares its own local
//! consts and the doc block is the cross-checked contract).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::session::{AvSession, SessionDelegate};

/// Seam framing magic, "RTSP" big-endian — see the module doc for why a magic (out_hi cap-clear has
/// no resync policy of its own).
pub const SEAM_MAGIC: u32 = 0x5254_5350;
/// Per-message ceiling. Twice the control plane's 256 KiB MAX_BODY (an RS_REQ carries the request
/// AND the local response); anything larger is a garbled length prefix, skipped by magic-resync.
pub const RELAY_SEAM_MAX: usize = 512 * 1024;
/// Where ocbmd connects to reach us (mic-seam mechanics, ocbmd side: `ensure_rtsp_seam`).
/// 9106 verified free in-tree (9110 = HID ingest, 9112 = mic ingest).
pub const RELAY_ADDR: &str = "127.0.0.1:9106";

// ---- message ops (common header [op u8][conn u32 LE][cseq u32 LE]) ----
pub const RS_OPEN: u8 = 0x01; // box→host [ver=1][flags b0=wireless][cfg_crc u32 LE][ctx_len u32 LE][ctx bplist]
pub const RS_REQ: u8 = 0x02; // box→host [route u8][flags b0=NOTIFY][local_len u32 LE][local][req]
pub const RS_RESP: u8 = 0x03; // host→box [status u16 LE][response body]
pub const RS_CLOSE: u8 = 0x04; // box→host [reason u8]
pub const RS_ERR: u8 = 0x05; // host→box [code u8] → box falls back to local

/// RS_OPEN protocol version.
pub const RS_VER: u8 = 1;

// RS_REQ routes (4-7 reserved).
pub const ROUTE_SETUP: u8 = 1;
pub const ROUTE_RECORD: u8 = 2;
pub const ROUTE_TEARDOWN: u8 = 3; // always NOTIFY in v1 (box tears down + answers immediately)

/// RS_REQ flags bit0: NOTIFY — no RS_RESP is expected (fire-and-forget; TEARDOWN in v1).
pub const REQ_FLAG_NOTIFY: u8 = 0x01;
/// RS_OPEN flags bit0: the connection is wireless. Set for real since the 2026-08-10 wireless flip
/// (app-driven SETUP now runs on both transports); the host uses it to tell the arms apart — the
/// wireless one differs in TWO ways: it carries the type-130 RCS DataStream (which the host does not
/// author and echoes the box's local answer for), and its phase-1 `enabledFeatures` includes two
/// box-only tokens (`iAPChannel`, `sessionManagement`) the host must PRESERVE rather than overwrite —
/// stripping `iAPChannel` kills the iAP2 tunnel.
pub const OPEN_FLAG_WIRELESS: u8 = 0x01;

// RS_CLOSE reasons.
pub const CLOSE_EOF: u8 = 0;
pub const CLOSE_HIJACK: u8 = 1;
pub const CLOSE_ERROR: u8 = 2;
pub const CLOSE_RESET: u8 = 3;

/// One answer routed from the seam reader thread to a blocked [`rpc`] caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayAnswer {
    /// RS_RESP: HTTP-style status + the host-authored response body.
    Resp { status: u16, body: Vec<u8> },
    /// RS_ERR: the host explicitly declined (code is diagnostic only — every code means "fall back").
    Err(u8),
    /// The seam died (EOF / write failure) while this call was pending — fail fast, don't wait out
    /// the full timeout (the plan's "host crash mid-SETUP" failure mode).
    HostGone,
}

// ---- statics (house pattern: uplink.rs / metadata.rs — process-lifetime seam state) ----

/// The write half of the CURRENT accepted ocbmd connection (2 s SO_SNDTIMEO — the events.rs/uplink.rs
/// bounded-write discipline: a wedged peer costs a bounded stall, never a forever-block on the
/// control thread). One producer: a new accept REPLACES the old (ocbmd reconnects across restarts).
static SEAM_TX: Mutex<Option<TcpStream>> = Mutex::new(None);

/// Whether a live ocbmd connection is attached — the selection gate airplayd reads (`seam_up()`).
static SEAM_UP: AtomicBool = AtomicBool::new(false);

/// Generation counter for accepted seam connections: a superseded reader thread's EOF must not mark
/// the REPLACEMENT connection down (see `read_seam`).
static SEAM_GEN: AtomicU32 = AtomicU32::new(0);

/// Monotonic per-process relay-connection id mint. Hijack ⇒ a new `RemoteSession` ⇒ a new conn; the
/// single serve FIFO in airplayd guarantees RS_CLOSE(old) precedes RS_OPEN(new) on the wire.
static CONN_SEQ: AtomicU32 = AtomicU32::new(0);

/// The in-flight rpc registry's shape: (conn, cseq) → the waiter's answer channel.
type PendingMap = HashMap<(u32, u32), SyncSender<RelayAnswer>>;

/// In-flight rpc() calls awaiting an answer, keyed by (conn, cseq). `SyncSender` with capacity 1: the
/// reader thread's send never blocks (one answer per key), and a late answer after timeout is dropped
/// harmlessly (the receiver is gone; `send` just errors).
fn pending() -> &'static Mutex<PendingMap> {
    // HashMap::new() isn't const-constructible (RandomState), hence OnceLock rather than a static
    // Mutex initializer like SEAM_TX.
    static P: OnceLock<Mutex<PendingMap>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Is a host relay consumer attached? The airplayd selection gate
/// (`levers::appsetup() && relay::seam_up()`, both transports) reads this per connection, so a session
/// that starts with no host present simply runs plain `AvSession` — failure mode #1 of the plan.
pub fn seam_up() -> bool {
    SEAM_UP.load(Ordering::Acquire)
}

/// The rpc answer deadline. Default 3 s, `CARPLAY_RELAY_TIMEOUT_MS` override — resolved ONCE per
/// process and cached (the levers-file discipline: editing the env mid-run does nothing until the
/// daemon restarts; this runs on the timing-critical control thread, so no per-call getenv either).
fn relay_timeout() -> Duration {
    static T: OnceLock<Duration> = OnceLock::new();
    *T.get_or_init(|| {
        let ms = std::env::var("CARPLAY_RELAY_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(3000);
        Duration::from_millis(ms)
    })
}

/// `CARPLAY_RELAY_STRICT=1` upgrades a host-vs-local response MISMATCH from warn-only to a local
/// fallback (plan §Settled-decision 2: equivalence = fresh host logic + live oracle; strict mode is
/// the "reject divergence" lever for validation runs). Resolved once, like the timeout.
fn strict() -> bool {
    static S: OnceLock<bool> = OnceLock::new();
    *S.get_or_init(|| std::env::var("CARPLAY_RELAY_STRICT").is_ok_and(|v| v == "1"))
}

// ---- seam frame codec ----

/// Frame one message for the seam: `[u32 BE "RTSP"][u32 BE len][msg]`.
pub fn frame_msg(msg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + msg.len());
    out.extend_from_slice(&SEAM_MAGIC.to_be_bytes());
    out.extend_from_slice(&(msg.len() as u32).to_be_bytes());
    out.extend_from_slice(msg);
    out
}

/// Streaming seam-frame decoder: `push()` raw bytes, `next()` pops complete messages. Resyncs by
/// scanning for the magic (a truncated frame — an out_hi cap-clear on the OCBM leg — costs exactly
/// the truncated message; the next complete frame parses). An implausible declared length
/// (> [`RELAY_SEAM_MAX`]) is treated as garbage: skip one byte and rescan.
#[derive(Default)]
pub struct FrameBuf {
    buf: Vec<u8>,
}

impl FrameBuf {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Pop the next complete message, or `None` if none is buffered yet.
    ///
    /// Named `next` to match the house decoder vocabulary (ocbm-proto's `Reassembler::next`); it is
    /// deliberately NOT an `Iterator` — `None` means "need more bytes", not exhaustion.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Vec<u8>> {
        let magic = SEAM_MAGIC.to_be_bytes();
        loop {
            // Resync: drop everything ahead of the first magic. If no magic is present, keep only the
            // last 3 bytes (a magic may be split across pushes) so garbage can't grow the buffer.
            match self.buf.windows(4).position(|w| w == magic) {
                Some(0) => {}
                Some(n) => {
                    self.buf.drain(..n);
                }
                None => {
                    let keep = self.buf.len().min(3);
                    self.buf.drain(..self.buf.len() - keep);
                    return None;
                }
            }
            if self.buf.len() < 8 {
                return None; // waiting for the length word
            }
            let len = u32::from_be_bytes([self.buf[4], self.buf[5], self.buf[6], self.buf[7]]) as usize;
            if len > RELAY_SEAM_MAX {
                // A magic followed by a garbled length (or payload bytes that happen to spell the
                // magic). Skip ONE byte and rescan — never trust the bogus length for the skip.
                self.buf.drain(..1);
                continue;
            }
            if self.buf.len() < 8 + len {
                return None; // valid header, waiting for the body
            }
            let msg = self.buf[8..8 + len].to_vec();
            self.buf.drain(..8 + len);
            return Some(msg);
        }
    }
}

// ---- message builders / parser ----

fn hdr(op: u8, conn: u32, cseq: u32) -> Vec<u8> {
    let mut m = Vec::with_capacity(9);
    m.push(op);
    m.extend_from_slice(&conn.to_le_bytes());
    m.extend_from_slice(&cseq.to_le_bytes());
    m
}

/// Build an RS_OPEN: `[hdr][ver][flags][cfg_crc u32 LE][ctx_len u32 LE][ctx]`. `cseq` is 0 — OPEN is
/// not a request/response exchange.
pub fn msg_open(conn: u32, wireless: bool, cfg_crc: u32, ctx: &[u8]) -> Vec<u8> {
    let mut m = hdr(RS_OPEN, conn, 0);
    m.push(RS_VER);
    m.push(if wireless { OPEN_FLAG_WIRELESS } else { 0 });
    m.extend_from_slice(&cfg_crc.to_le_bytes());
    m.extend_from_slice(&(ctx.len() as u32).to_le_bytes());
    m.extend_from_slice(ctx);
    m
}

/// Build an RS_REQ: `[hdr][route][flags][local_len u32 LE][local][req]`.
pub fn msg_req(conn: u32, cseq: u32, route: u8, notify: bool, local: &[u8], req: &[u8]) -> Vec<u8> {
    let mut m = hdr(RS_REQ, conn, cseq);
    m.push(route);
    m.push(if notify { REQ_FLAG_NOTIFY } else { 0 });
    m.extend_from_slice(&(local.len() as u32).to_le_bytes());
    m.extend_from_slice(local);
    m.extend_from_slice(req);
    m
}

/// Build an RS_CLOSE: `[hdr][reason]`.
pub fn msg_close(conn: u32, reason: u8) -> Vec<u8> {
    let mut m = hdr(RS_CLOSE, conn, 0);
    m.push(reason);
    m
}

/// Parse an inbound message's common header: `(op, conn, cseq, rest)`.
pub fn parse_header(msg: &[u8]) -> Option<(u8, u32, u32, &[u8])> {
    if msg.len() < 9 {
        return None;
    }
    let op = msg[0];
    let conn = u32::from_le_bytes([msg[1], msg[2], msg[3], msg[4]]);
    let cseq = u32::from_le_bytes([msg[5], msg[6], msg[7], msg[8]]);
    Some((op, conn, cseq, &msg[9..]))
}

fn route_name(route: u8) -> &'static str {
    match route {
        ROUTE_SETUP => "SETUP",
        ROUTE_RECORD => "RECORD",
        ROUTE_TEARDOWN => "TEARDOWN",
        _ => "?",
    }
}

// ---- seam I/O ----

/// Write one framed message to the current seam, best-effort, under the SEAM_TX lock (ONE writer at a
/// time keeps messages contiguous on the byte stream; the 2 s SO_SNDTIMEO bounds a wedged peer).
/// On a write failure the socket is dropped, the seam marked down, and every pending rpc is failed
/// fast with `HostGone` — a broken pipe mid-SETUP must cost ≤ the write timeout, not the full rpc
/// timeout on top.
fn seam_send(msg: &[u8]) -> bool {
    let framed = frame_msg(msg);
    let mut g = crate::plock(&SEAM_TX);
    let ok = match g.as_mut() {
        Some(s) => s.write_all(&framed).is_ok(),
        None => false,
    };
    if !ok && g.is_some() {
        eprintln!("[relay] seam write failed — dropping the ocbmd connection (host gone?)");
        *g = None;
        SEAM_UP.store(false, Ordering::Release);
        drop(g); // don't hold SEAM_TX across the PENDING lock (lock-order hygiene)
        drain_pending(RelayAnswer::HostGone);
    }
    ok
}

/// Fail every in-flight rpc with `answer` (seam death). A send error just means that caller already
/// timed out and dropped its receiver — harmless.
fn drain_pending(answer: RelayAnswer) {
    let drained: Vec<_> = crate::plock(pending()).drain().collect();
    for (_, tx) in drained {
        let _ = tx.send(answer.clone());
    }
}

/// Route one inbound seam message (RS_RESP / RS_ERR) to the pending rpc it answers. Anything else
/// from the host is a protocol surprise — logged, never fatal.
fn dispatch_inbound(msg: &[u8]) {
    let Some((op, conn, cseq, rest)) = parse_header(msg) else {
        eprintln!("[relay] inbound seam message too short ({} B) — dropped", msg.len());
        return;
    };
    let answer = match op {
        RS_RESP if rest.len() >= 2 => RelayAnswer::Resp {
            status: u16::from_le_bytes([rest[0], rest[1]]),
            body: rest[2..].to_vec(),
        },
        RS_ERR => RelayAnswer::Err(rest.first().copied().unwrap_or(0)),
        _ => {
            eprintln!("[relay] unexpected inbound op {op:#04x} (conn {conn} cseq {cseq}) — dropped");
            return;
        }
    };
    match crate::plock(pending()).remove(&(conn, cseq)) {
        Some(tx) => {
            let _ = tx.send(answer);
        }
        // A late answer after its rpc timed out (already fallen back to local), or an answer for a
        // hijacked-away conn. Expected under the failure model; named so it's visible.
        None => eprintln!("[relay] answer for (conn {conn}, cseq {cseq}) has no waiter — late/stale, dropped"),
    }
}

/// Start the relay seam listener ONCE at process startup (airplayd main, next to the uplink
/// listener). House pattern = `uplink::start_control_listener`: bind once, accept loop on its own
/// thread; a bind failure logs and disables the feature (seam_up() stays false → plain AvSession).
/// A NEW accept replaces the old connection — ocbmd reconnects across its restarts, and one producer
/// per seam is the ocbmd-wide contract (`av_conns.retain(|(_, c)| *c != ch)` on its side).
pub fn start_listener() {
    std::thread::spawn(|| {
        let listener = match TcpListener::bind(RELAY_ADDR) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[relay] bind {RELAY_ADDR} failed: {e} — app-driven SETUP disabled (local fallback)");
                return;
            }
        };
        eprintln!("[relay] RTSP relay seam listening on {RELAY_ADDR} (app-driven SETUP, plan P1)");
        for conn in listener.incoming() {
            let s = match conn {
                Ok(s) => s,
                Err(_) => continue,
            };
            let _ = s.set_nodelay(true); // rpc frames are small and latency-critical
            let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
            let reader = match s.try_clone() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[relay] try_clone on accepted seam failed: {e} — dropping connection");
                    continue;
                }
            };
            // Mint + install ATOMICALLY under the SEAM_TX lock. If the mint happened outside the
            // lock, a dying reader could pass its `SEAM_GEN == gen` check just before we bump the
            // generation here, then clear the freshly installed socket right after — leaving the
            // relay silently disabled until the next SUBSCRIBE (gate review 2026-08-08, reviewer 1:
            // the consequence is only the designed local fallback, but a soak run would be polluted).
            // The reader's check+clear takes the same lock, so the interleave is now impossible.
            let gen = {
                let mut g = crate::plock(&SEAM_TX);
                let gen = SEAM_GEN.fetch_add(1, Ordering::Relaxed) + 1;
                *g = Some(s); // replace the old producer
                SEAM_UP.store(true, Ordering::Release);
                gen
            };
            eprintln!("[relay] ocbmd connected to the RTSP seam (gen {gen})");
            std::thread::spawn(move || read_seam(reader, gen));
        }
    });
}

/// Per-connection reader: reassemble seam frames and dispatch RS_RESP/RS_ERR to PENDING. On EOF or
/// error, fail-fast every pending rpc with HostGone — and mark the seam down ONLY if this reader is
/// still the current generation (a superseded connection's late EOF must not clobber the fresh one).
fn read_seam(mut s: TcpStream, gen: u32) {
    let mut fb = FrameBuf::new();
    let mut tmp = [0u8; 16384];
    loop {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                fb.push(&tmp[..n]);
                while let Some(msg) = fb.next() {
                    dispatch_inbound(&msg);
                }
            }
        }
    }
    // Check + clear under the SEAM_TX lock, pairing with the accept loop's locked mint+install:
    // without the lock this was check-then-act, and a reconnect racing the check could get its
    // fresh socket cleared by the dying reader (see the accept-side comment).
    {
        let mut g = crate::plock(&SEAM_TX);
        if SEAM_GEN.load(Ordering::Relaxed) == gen {
            SEAM_UP.store(false, Ordering::Release);
            *g = None;
            eprintln!("[relay] seam reader EOF (gen {gen}) — host relay down, local fallback armed");
        } else {
            eprintln!("[relay] superseded seam reader (gen {gen}) exited");
        }
    }
    // Pending answers were registered against THIS connection's writes either way — the replacement
    // connection will never answer them. Fail them fast regardless of generation.
    drain_pending(RelayAnswer::HostGone);
}

// ---- the rpc itself ----

/// What one relay exchange resolved to.
enum RpcResult {
    /// 200 from the host: use its body as the response the phone sees.
    Host(Vec<u8>),
    /// Fall back to the box's local response. `sticky` = also stop relaying for the rest of this
    /// connection (timeout / RS_ERR / non-200 / HostGone — the transport or the host is unwell).
    /// A STRICT-mode content mismatch is NOT sticky: the transport is fine and every later exchange
    /// gets its own oracle diff.
    FallLocal { sticky: bool },
}

/// One blocking request/response over the seam. Runs on the per-connection CONTROL thread, which is
/// designed to block (net.rs wraps the whole exchange in a 30 s SO_RCVTIMEO envelope; RECORD already
/// legally stalls ≤5 s box-side): register PENDING → frame+write under the SEAM_TX lock →
/// `recv_timeout` (3 s default / `CARPLAY_RELAY_TIMEOUT_MS`).
///
/// On 200 the elapsed µs is logged — **THE latency measurement** the P1 gate reads (superseding P0's
/// CH_ECHO proxy, which rides out_lo while this rides out_hi) — and, for SETUP, the host body is
/// warn-diffed against the local oracle (enabledFeatures + stream types).
fn rpc(conn: u32, cseq: u32, route: u8, req: &[u8], local: &[u8]) -> RpcResult {
    if !seam_up() {
        // Raced a seam death between the caller's gate and here; treat like HostGone.
        return RpcResult::FallLocal { sticky: true };
    }
    let (tx, rx) = sync_channel::<RelayAnswer>(1);
    crate::plock(pending()).insert((conn, cseq), tx);
    let t0 = Instant::now();
    if !seam_send(&msg_req(conn, cseq, route, false, local, req)) {
        crate::plock(pending()).remove(&(conn, cseq));
        eprintln!("[relay] {} (conn {conn} cseq {cseq}): seam write failed — LOCAL fallback (sticky)", route_name(route));
        return RpcResult::FallLocal { sticky: true };
    }
    match rx.recv_timeout(relay_timeout()) {
        Ok(RelayAnswer::Resp { status: 200, body }) => {
            eprintln!(
                "[relay] {} host-authored in {} µs (conn {conn} cseq {cseq}, {} B)",
                route_name(route),
                t0.elapsed().as_micros(),
                body.len()
            );
            if route == ROUTE_SETUP {
                if let Some(diff) = diff_setup(local, &body) {
                    if strict() {
                        eprintln!("[relay] STRICT: host SETUP response diverges from local oracle ({diff}) — LOCAL fallback for this exchange");
                        return RpcResult::FallLocal { sticky: false };
                    }
                    eprintln!("[relay] WARN: host SETUP response diverges from local oracle ({diff}) — using host body (warn-only; CARPLAY_RELAY_STRICT=1 to reject)");
                }
            }
            RpcResult::Host(body)
        }
        Ok(RelayAnswer::Resp { status, .. }) => {
            eprintln!("[relay] {} (conn {conn} cseq {cseq}): host answered status {status} (non-200 = host-reject) — LOCAL fallback, relay STICKY-OFF for this connection", route_name(route));
            RpcResult::FallLocal { sticky: true }
        }
        Ok(RelayAnswer::Err(code)) => {
            eprintln!("[relay] {} (conn {conn} cseq {cseq}): host RS_ERR code {code} — LOCAL fallback, relay STICKY-OFF for this connection", route_name(route));
            RpcResult::FallLocal { sticky: true }
        }
        Ok(RelayAnswer::HostGone) => {
            eprintln!("[relay] {} (conn {conn} cseq {cseq}): seam died mid-exchange — LOCAL fallback, relay STICKY-OFF for this connection", route_name(route));
            RpcResult::FallLocal { sticky: true }
        }
        Err(_) => {
            // Timeout. Deregister so a late answer is dropped as stale rather than leaking the slot.
            crate::plock(pending()).remove(&(conn, cseq));
            eprintln!(
                "[relay] {} (conn {conn} cseq {cseq}): NO ANSWER within {:?} — LOCAL fallback, relay STICKY-OFF for this connection",
                route_name(route),
                relay_timeout()
            );
            RpcResult::FallLocal { sticky: true }
        }
    }
}

// ---- oracle diff (plan §Settled-decision 2: every live exchange is diffed against the box's own
// ---- relayed local response — warn-only by default) ----

/// Extract the comparison-relevant surface of a SETUP response plist: the `enabledFeatures` string
/// array (phase 1) and, per stream, its `type` PLUS its sorted KEY SET (phase 2). Values are still
/// excluded — ports, UUIDs and keys are LEGITIMATELY different between host and box authorship — but
/// the presence or absence of a key is not a value, and it carries signal the type alone does not.
///
/// CONSEQUENCE — adding a key to an A/V response shape in `session.rs` is now a TWIN-SYNC OBLIGATION.
/// If the box starts emitting a new key on 110/111/100-102 and `setup_driver.rs::author_stream` is not
/// updated to carry it, this oracle WARNs. That is a TRUE positive (the box put a key on the wire and
/// the host dropped it), but it will look like a regression to whoever trips it, so expect it rather
/// than discover it. No false-positive path exists today: `author_stream` never invents a key — it
/// inserts `type` (and `streamConnectionID` for 100-102) exactly where the box does unconditionally,
/// and every other key arrives via `copy_port`, which is presence-preserving by construction. A failed
/// RTCP bind omits `controlPort` on both sides; a failed 110/111 bind omits the whole stream on both.
///
/// Comparing types alone was blind to the exact failure this project has already shipped once. The
/// type-130 (RCS DataStream) entry's entire payload is `streamID`, the transport token: a host answer
/// that keeps `{type:130}` and drops `streamID` has an IDENTICAL type list, so the oracle called it a
/// MATCH and — in the default warn-only mode — passed it to the phone, which then has no outbound
/// iAP2 path at all. That is byte-for-byte the 2026-07-31 -> 08-10 wireless-metadata outage
/// (`docs/ops/captures/2026-08-10_REGRESSION_datastream130_scid_rejected.txt`), and since app-driven
/// SETUP became the wireless default it is reachable through host authorship too. The key set closes
/// that door without making legitimate value differences divergent.
fn setup_surface(body: &[u8]) -> (Vec<String>, Vec<(i64, String)>) {
    let dict = plist::Value::from_reader(std::io::Cursor::new(body))
        .ok()
        .and_then(|v| v.into_dictionary());
    let mut features: Vec<String> = Vec::new();
    let mut stream_types: Vec<(i64, String)> = Vec::new();
    if let Some(d) = dict {
        if let Some(arr) = d.get("enabledFeatures").and_then(|v| v.as_array()) {
            for f in arr {
                if let Some(s) = f.as_string() {
                    features.push(s.to_string());
                }
            }
        }
        if let Some(arr) = d.get("streams").and_then(|v| v.as_array()) {
            for s in arr {
                if let Some(sd) = s.as_dictionary() {
                    let t = sd
                        .get("type")
                        .and_then(|v| v.as_signed_integer())
                        .unwrap_or(-1);
                    let mut keys: Vec<&str> = sd.keys().map(|k| k.as_str()).collect();
                    keys.sort_unstable();
                    stream_types.push((t, keys.join(",")));
                }
            }
        }
    }
    stream_types.sort_unstable();
    (features, stream_types)
}

/// Does `body` parse as a binary-plist dictionary? Used by `diff_setup` to tell an unparseable host
/// response apart from a parseable one with no surface keys (audit B2).
fn is_plist_dict(body: &[u8]) -> bool {
    plist::Value::from_reader(std::io::Cursor::new(body))
        .ok()
        .and_then(|v| v.into_dictionary())
        .is_some()
}

/// Compare host vs local SETUP responses on the surfaces above. `None` = equivalent;
/// `Some(description)` names what diverged (for the warn / strict-reject log). An omitted stream in
/// the host answer is the plan's "host omits a bound stream" failure mode — warn, idle threads are
/// reaped at TEARDOWN via the existing av_streams flags.
fn diff_setup(local: &[u8], host: &[u8]) -> Option<String> {
    // A host body that isn't even a valid plist dict — while the local one IS — must count as a divergence
    // (audit B2). Otherwise both `setup_surface` to ([], []) and `diff_setup` returns None, so a garbage
    // host SETUP response would pass to the phone even under CARPLAY_RELAY_STRICT (which is supposed to fall
    // back to the local response on any divergence). An empty/non-plist LOCAL (e.g. a RECORD empty-200) has
    // no structured surface to protect, so it's exempt.
    if is_plist_dict(local) && !is_plist_dict(host) {
        return Some("host response is not a valid binary plist".to_string());
    }
    let (lf, ls) = setup_surface(local);
    let (hf, hs) = setup_surface(host);
    let mut diffs = Vec::new();
    if lf != hf {
        diffs.push(format!("enabledFeatures local={lf:?} host={hf:?}"));
    }
    if ls != hs {
        diffs.push(format!("stream type+keys local={ls:?} host={hs:?}"));
    }
    if diffs.is_empty() {
        None
    } else {
        Some(diffs.join("; "))
    }
}

// ---- the delegate ----

/// [`SessionDelegate`] that wraps the real [`AvSession`] and relays SETUP/RECORD (request/response)
/// and TEARDOWN (notify) to the host app. Inner ALWAYS runs first — it owns every side effect and its
/// response is the oracle + the fallback. Selected per connection in airplayd
/// (`levers::appsetup() && relay::seam_up()`, both transports); plain `AvSession` otherwise.
pub struct RemoteSession {
    inner: AvSession,
    /// Relay-connection id, minted at `on_paired` (0 = pairing never completed; no OPEN, no CLOSE).
    conn: u32,
    /// Per-connection exchange sequence (cseq of the next RS_REQ).
    cseq: u32,
    /// Sticky per-connection local fallback (plan §Settled-decision 3): once a relay exchange times
    /// out / errors, every later exchange on THIS phone connection answers locally without waiting.
    /// The next control connection gets a fresh `RemoteSession` and a fresh chance.
    local_only: bool,
    /// crc32 of the YAML the box built `/info` from (0 = built-in default) — echoed in RS_OPEN so the
    /// host can verify the box is running the config it pushed at SUBSCRIBE.
    cfg_crc: u32,
    wireless: bool,
    /// Advertised main-display geometry for the RS_OPEN ctx (the host authors phase-1 answers from
    /// the same YAML, and this lets it sanity-check which geometry this connection negotiated).
    display_width: u32,
    display_height: u32,
}

impl RemoteSession {
    pub fn new(
        inner: AvSession,
        cfg_crc: u32,
        wireless: bool,
        display_width: u32,
        display_height: u32,
    ) -> Self {
        Self {
            inner,
            conn: 0,
            cseq: 0,
            local_only: false,
            cfg_crc,
            wireless,
            display_width,
            display_height,
        }
    }

    fn next_cseq(&mut self) -> u32 {
        self.cseq = self.cseq.wrapping_add(1);
        self.cseq
    }

    /// Whether to even attempt a relay for the next exchange.
    fn relaying(&self) -> bool {
        !self.local_only && self.conn != 0 && seam_up()
    }

    /// Run one relayed exchange, applying the sticky-fallback policy. `local` is both the oracle and
    /// the fallback body.
    fn exchange(&mut self, route: u8, req: &[u8], local: Vec<u8>) -> Vec<u8> {
        let cseq = self.next_cseq();
        match rpc(self.conn, cseq, route, req, &local) {
            RpcResult::Host(body) => body,
            RpcResult::FallLocal { sticky } => {
                if sticky && !self.local_only {
                    self.local_only = true;
                    eprintln!("[relay] connection {} now LOCAL-ONLY (box-driven) for its remaining lifetime", self.conn);
                }
                local
            }
        }
    }
}

impl SessionDelegate for RemoteSession {
    fn on_paired(&mut self, shared_secret: [u8; 32]) {
        self.inner.on_paired(shared_secret);
        // Mint the relay connection id and announce it. RS_OPEN must be emittable BEFORE any host
        // bytes exist on this conn — ocbmd keeps the seam eagerly connected while subscribed for
        // exactly this reason (its `ensure_rtsp_seam` comment). Best-effort: if the seam is down the
        // send fails quietly and every exchange falls back locally via `relaying()`.
        self.conn = CONN_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
        let mut d = plist::Dictionary::new();
        d.insert(
            "peer".into(),
            plist::Value::String(
                self.inner
                    .peer_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_default(),
            ),
        );
        d.insert("displayWidth".into(), plist::Value::from(self.display_width as i64));
        d.insert("displayHeight".into(), plist::Value::from(self.display_height as i64));
        let mut ctx = Vec::new();
        if plist::to_writer_binary(&mut ctx, &d).is_err() {
            ctx.clear(); // cannot happen for this literal dict; an empty ctx is still a valid OPEN
        }
        let sent = seam_send(&msg_open(self.conn, self.wireless, self.cfg_crc, &ctx));
        eprintln!(
            "[relay] RS_OPEN conn {} (wireless={} cfg_crc={:#010x} {}×{}) sent={sent}",
            self.conn, self.wireless, self.cfg_crc, self.display_width, self.display_height
        );
    }

    fn setup(&mut self, request_plist: &[u8]) -> Vec<u8> {
        // Pre-bind + oracle: the box session runs FIRST (binds, spawns, side effects) and its answer
        // is what we relay AND what we fall back to. Phase 1 vs phase 2 needs no distinguishing here
        // — the host splits on the `streams` key, same as the box.
        let local = self.inner.setup(request_plist);
        if local.is_empty() {
            // Bind failure (AvSession answers an empty body its caller maps to an error). Nothing to
            // author host-side and nothing worth a USB RTT — return it as-is, don't relay.
            return local;
        }
        if !self.relaying() {
            return local;
        }
        self.exchange(ROUTE_SETUP, request_plist, local)
    }

    fn record(&mut self) -> Vec<u8> {
        // ALL of RECORD's side effects run box-side first (event-channel accept, the
        // requestUI/takeScreen focus handshake, the iap_tunnel open, mark_session_started) — the
        // relay only offers the host authorship of the (empty) response body. NOTE: an empty local is
        // NORMAL here (RECORD's reference answer is bodyless), unlike setup's empty-means-bind-failure.
        let local = self.inner.record();
        if !self.relaying() {
            return local;
        }
        self.exchange(ROUTE_RECORD, &[], local)
    }

    fn command(&mut self, request_plist: &[u8]) -> Vec<u8> {
        // Pure pass-through (plan §Settled-decision 3): COMMAND stays box-side — the host already
        // sees every command plist via the META_CMD seam, so relaying would buy a second copy at the
        // cost of an RTT on the hottest control path.
        self.inner.command(request_plist)
    }

    fn teardown(&mut self, request_plist: &[u8]) {
        // Box first: teardown + answer immediately (the single reset owner, AvSession::reset, stays
        // untouched). Then a fire-and-forget NOTIFY so the host can reset its authoring state — no
        // pending registration, no wait, and by design no answer.
        self.inner.teardown(request_plist);
        if self.relaying() {
            let cseq = self.next_cseq();
            let _ = seam_send(&msg_req(self.conn, cseq, ROUTE_TEARDOWN, true, &[], request_plist));
        }
    }

    fn last_activity(&self) -> Option<std::sync::Arc<portable_atomic::AtomicU64>> {
        // Pass-through preserves the av_idle_ms plumbing (the control-loop idle watchdog must keep
        // seeing the REAL session's activity stamp, or a quiet control channel tears live A/V down).
        self.inner.last_activity()
    }
}

impl Drop for RemoteSession {
    fn drop(&mut self) {
        // Best-effort RS_CLOSE so the host resets per-conn state promptly (hijack included — the
        // serve FIFO guarantees this precedes the successor's RS_OPEN). We can't distinguish
        // eof/hijack/reset from inside Drop, so EOF is the honest generic reason; `inner`'s own Drop
        // runs after this and resets the real session exactly as today.
        if self.conn != 0 {
            let _ = seam_send(&msg_close(self.conn, CLOSE_EOF));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Codec ------------------------------------------------------------------------------------

    #[test]
    fn frame_roundtrip() {
        let mut fb = FrameBuf::new();
        let m1 = msg_open(7, false, 0xDEAD_BEEF, b"ctx");
        let m2 = msg_req(7, 1, ROUTE_SETUP, false, b"local", b"req");
        fb.push(&frame_msg(&m1));
        fb.push(&frame_msg(&m2));
        assert_eq!(fb.next().as_deref(), Some(&m1[..]));
        assert_eq!(fb.next().as_deref(), Some(&m2[..]));
        assert!(fb.next().is_none());
    }

    #[test]
    fn resync_on_garbage_and_split_pushes() {
        let mut fb = FrameBuf::new();
        let m = msg_close(3, CLOSE_RESET);
        let framed = frame_msg(&m);
        // Garbage (including a stray 'R') ahead of the frame, delivered split mid-magic.
        fb.push(&[0x00, b'R', 0xFF, 0x52, 0x54]); // junk + the first 2 magic bytes
        assert!(fb.next().is_none());
        fb.push(&framed[2..]); // the rest of the frame
        assert_eq!(fb.next().as_deref(), Some(&m[..]));
        assert!(fb.next().is_none());
    }

    #[test]
    fn oversize_length_is_skipped_and_recovers() {
        let mut fb = FrameBuf::new();
        // A real magic with an implausible length — must be skipped byte-wise, then the good frame
        // after it must still decode.
        let mut junk = SEAM_MAGIC.to_be_bytes().to_vec();
        junk.extend_from_slice(&((RELAY_SEAM_MAX as u32) + 1).to_be_bytes());
        junk.extend_from_slice(b"leftover");
        let m = msg_req(1, 2, ROUTE_RECORD, false, &[], &[]);
        fb.push(&junk);
        fb.push(&frame_msg(&m));
        assert_eq!(fb.next().as_deref(), Some(&m[..]));
    }

    #[test]
    fn truncated_frame_is_recovered_by_next_magic() {
        // A frame whose declared body never fully arrives (the out_hi cap-clear truncation) followed
        // by a complete frame: the decoder eventually resyncs on the next magic once enough bytes
        // arrive to disprove the first length. This mirrors the "lost message is recovered by
        // timeout→fallback, never by wedging" policy: the FIRST message is lost, the second decodes.
        let mut fb = FrameBuf::new();
        let mut truncated = frame_msg(&[0xAA; 100]);
        truncated.truncate(8 + 40); // 60 bytes of the body never arrive
        let m = msg_close(9, CLOSE_EOF);
        fb.push(&truncated);
        // Nothing decodable yet (waiting on the truncated body)…
        assert!(fb.next().is_none());
        // …until LATER traffic supplies enough bytes to "complete" the phantom length: the first
        // pop is a spliced garbage message (truncated body + the head of the good stream), after
        // which the magic scan realigns on a subsequent intact frame. Push enough copies that at
        // least one whole good frame lies beyond the 60 phantom bytes (each frame is 18 B).
        let good = frame_msg(&m);
        for _ in 0..5 {
            fb.push(&good);
        }
        let got = std::iter::from_fn(|| fb.next()).collect::<Vec<_>>();
        assert!(got.iter().any(|g| g == &m), "good frame must be recovered after a truncation");
    }

    // Message builders / parser ----------------------------------------------------------------

    #[test]
    fn header_roundtrip_and_req_layout() {
        let m = msg_req(0x0102_0304, 0x0A0B_0C0D, ROUTE_SETUP, true, b"LL", b"RR");
        let (op, conn, cseq, rest) = parse_header(&m).unwrap();
        assert_eq!((op, conn, cseq), (RS_REQ, 0x0102_0304, 0x0A0B_0C0D));
        assert_eq!(rest[0], ROUTE_SETUP);
        assert_eq!(rest[1], REQ_FLAG_NOTIFY);
        assert_eq!(u32::from_le_bytes([rest[2], rest[3], rest[4], rest[5]]), 2);
        assert_eq!(&rest[6..8], b"LL");
        assert_eq!(&rest[8..], b"RR");
    }

    // rpc-layer helpers (the delegate's inner is a concrete AvSession, so hermetic tests exercise
    // the layers under it: PENDING dispatch and the seam-down fast path) ------------------------

    #[test]
    fn dispatch_routes_resp_to_pending_waiter() {
        let key = (0xF00D_0001u32, 42u32);
        let (tx, rx) = sync_channel::<RelayAnswer>(1);
        crate::plock(pending()).insert(key, tx);
        let mut resp = hdr(RS_RESP, key.0, key.1);
        resp.extend_from_slice(&200u16.to_le_bytes());
        resp.extend_from_slice(b"body");
        dispatch_inbound(&resp);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            RelayAnswer::Resp { status: 200, body: b"body".to_vec() }
        );
        assert!(!crate::plock(pending()).contains_key(&key), "answered slot must be removed");
    }

    #[test]
    fn dispatch_routes_err_and_ignores_stale() {
        let key = (0xF00D_0002u32, 7u32);
        let (tx, rx) = sync_channel::<RelayAnswer>(1);
        crate::plock(pending()).insert(key, tx);
        let mut e = hdr(RS_ERR, key.0, key.1);
        e.push(3);
        dispatch_inbound(&e);
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), RelayAnswer::Err(3));
        // A second (stale) answer for the same key must be a logged no-op, not a panic.
        dispatch_inbound(&e);
    }

    #[test]
    fn rpc_falls_back_immediately_when_seam_down() {
        // Hermetic: no test in this binary raises SEAM_UP (the listener is never started here), so
        // the seam is down and rpc must return the sticky local fallback without waiting out the
        // 3 s answer timeout.
        let t0 = Instant::now();
        match rpc(0xF00D_0003, 1, ROUTE_SETUP, b"req", b"local") {
            RpcResult::FallLocal { sticky: true } => {}
            _ => panic!("seam-down rpc must be a sticky local fallback"),
        }
        assert!(t0.elapsed() < Duration::from_secs(1), "must fail fast, not wait the rpc timeout");
    }

    // Oracle diff --------------------------------------------------------------------------------

    fn plist_bytes(v: &plist::Dictionary) -> Vec<u8> {
        let mut b = Vec::new();
        plist::to_writer_binary(&mut b, v).unwrap();
        b
    }

    #[test]
    fn diff_setup_flags_feature_and_stream_divergence_only() {
        let mut base = plist::Dictionary::new();
        base.insert(
            "enabledFeatures".into(),
            plist::Value::Array(vec!["hevc".into()]),
        );
        base.insert("eventPort".into(), plist::Value::from(7001i64));
        let mut same = base.clone();
        same.insert("eventPort".into(), plist::Value::from(9999i64)); // ports legitimately differ
        assert!(diff_setup(&plist_bytes(&base), &plist_bytes(&same)).is_none());

        let mut diverged = base.clone();
        diverged.insert(
            "enabledFeatures".into(),
            plist::Value::Array(vec!["hevc".into(), "viewAreas".into()]),
        );
        assert!(diff_setup(&plist_bytes(&base), &plist_bytes(&diverged)).is_some());

        // Phase 2: an omitted stream type must be flagged.
        let mut stream = plist::Dictionary::new();
        stream.insert("type".into(), plist::Value::from(110i64));
        let mut with_stream = plist::Dictionary::new();
        with_stream.insert("streams".into(), plist::Value::Array(vec![plist::Value::Dictionary(stream)]));
        let empty = plist::Dictionary::new();
        assert!(diff_setup(&plist_bytes(&with_stream), &plist_bytes(&empty)).is_some());
    }

    /// The KEY-SET half of `setup_surface`. Without these three the widening from `Vec<i64>` to
    /// `Vec<(i64, String)>` is untested — the test above passes identically under the old
    /// types-only surface, so a future refactor could narrow it back silently.
    #[test]
    fn diff_setup_compares_stream_key_sets_but_not_their_values() {
        let streams = |s: Vec<plist::Dictionary>| {
            let mut d = plist::Dictionary::new();
            d.insert(
                "streams".into(),
                plist::Value::Array(s.into_iter().map(plist::Value::Dictionary).collect()),
            );
            plist_bytes(&d)
        };
        let ds = |with_stream_id: bool| {
            let mut d = plist::Dictionary::new();
            d.insert("type".into(), plist::Value::from(130i64));
            d.insert("streamConnectionID".into(), plist::Value::from(0i64));
            d.insert("dataPort".into(), plist::Value::from(51305i64));
            if with_stream_id {
                d.insert("streamID".into(), plist::Value::from(1i64));
            }
            d
        };

        // THE scenario this widening exists for: a host answer that keeps `{type:130}` but drops the
        // `streamID` transport token. Under the types-only surface both sides read as `[130]` and the
        // oracle called it a MATCH — in warn-only mode that body reaches the phone and the iAP2 tunnel
        // has no outbound path at all (the 2026-07-31 -> 08-10 outage, reachable again through host
        // authorship since app-driven SETUP became the wireless default).
        assert!(
            diff_setup(&streams(vec![ds(true)]), &streams(vec![ds(false)])).is_some(),
            "a dropped streamID must be a divergence, not a MATCH"
        );

        // …but VALUES must still be excluded: host and box legitimately carry different port numbers,
        // and making those divergent would flag every real exchange.
        let mut other_port = ds(true);
        other_port.insert("dataPort".into(), plist::Value::from(9999i64));
        assert!(
            diff_setup(&streams(vec![ds(true)]), &streams(vec![other_port])).is_none(),
            "a differing port value must NOT be a divergence"
        );

        // The RTCP-bind-failure path, pinned as a test rather than a claim: on a failed
        // `UdpSocket::bind` the box OMITS `controlPort` from its type-102 answer, and the host's
        // `copy_port` omits it too (it copies only keys present in the box's dict). Key sets agree, so
        // this must not read as a false-positive divergence.
        let audio = |with_ctrl: bool| {
            let mut d = plist::Dictionary::new();
            d.insert("type".into(), plist::Value::from(102i64));
            d.insert("streamConnectionID".into(), plist::Value::from(0xABCDi64));
            d.insert("dataPort".into(), plist::Value::from(7001i64));
            if with_ctrl {
                d.insert("controlPort".into(), plist::Value::from(7002i64));
            }
            d
        };
        assert!(
            diff_setup(&streams(vec![audio(false)]), &streams(vec![audio(false)])).is_none(),
            "a 102 that omits controlPort on BOTH sides is a MATCH"
        );
        assert!(
            diff_setup(&streams(vec![audio(true)]), &streams(vec![audio(false)])).is_some(),
            "…but only one side omitting it is a real divergence"
        );
    }

    // Ephemeral-port socket test for the codec (the ONE real-socket test; no fixed ports) --------

    #[test]
    fn codec_roundtrips_over_a_real_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap(); // ephemeral
        let addr = listener.local_addr().unwrap();
        let m1 = msg_open(1, true, 0x1234_5678, b"\x00bplist-ish");
        let m2 = msg_close(1, CLOSE_HIJACK);
        let (f1, f2) = (frame_msg(&m1), frame_msg(&m2));
        let writer = std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            // Interleave junk between the frames + split a write mid-frame to exercise streaming.
            s.write_all(&f1[..5]).unwrap();
            s.write_all(&f1[5..]).unwrap();
            s.write_all(&[0xDE, 0xAD]).unwrap();
            s.write_all(&f2).unwrap();
        });
        let (mut conn, _) = listener.accept().unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut fb = FrameBuf::new();
        let mut got: Vec<Vec<u8>> = Vec::new();
        let mut tmp = [0u8; 1024];
        while got.len() < 2 {
            match conn.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    fb.push(&tmp[..n]);
                    while let Some(m) = fb.next() {
                        got.push(m);
                    }
                }
                Err(_) => break,
            }
        }
        writer.join().unwrap();
        assert_eq!(got, vec![m1, m2]);
    }
}
