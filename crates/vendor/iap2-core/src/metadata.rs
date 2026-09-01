//! iAP2 metadata plane — the feed behind the host app's **Metadata** window.
//!
//! GROUND TRUTH: the catalogs below were extracted **directly from the Xcode-local CarPlay SDK**
//! (`/Applications/Xcode.app/.../CarPlaySimulator.devicekitplugin/Contents/Frameworks/
//! iAP2MessageKit.framework/Versions/A/iAP2MessageKit`) by reading Apple's own exported parameter
//! symbols — e.g. `_iAP2NowPlayingUpdate_MediaItemAttributes_MediaItemAlbumTitleParameterName`,
//! `_iAP2RouteGuidanceUpdate_DestinationNameParameterName`, `_iAP2CallStateUpdate_DisplayName…`,
//! `_iAP2ListUpdate_RecentsList_…`, `_iAP2CommunicationsUpdate_…`. Every field this module parses
//! is one of Apple's declared parameters; ids are the wire-verified ones (docs/20 §1.2–1.5).
//!
//! WHY iAP2 AND NOT AIRPLAY: wired CarPlay splits its control planes. Media text/artwork, route
//! guidance and call state ride **iAP2** (this file); the AirPlay `/command` channel carries only
//! UI/session state (airplayd forwards those separately). See docs/20 §1.0.
//!
//! FOUR LOAD-BEARING PREREQS for any of this to arrive (learned the hard way, docs/20 §1.2):
//!   1. iAP2 auth + identify complete;
//!   2. the update ids are declared receivable in `0x1D01` `MessagesReceivedFromDevice` (iap2-core's
//!      `RCV_MSG_IDS` already declares 0x5001/0x5201/0x5202/0x4155 — iOS sends NOTHING otherwise);
//!   3. an explicit **subscribe** naming the wanted fields (an empty field list is silently ignored);
//!   4. the updates are DELTAS — merge non-empty fields (an elapsed-only frame must not blank the title).
//!
//! Everything is forwarded to ocbmd's metadata seam (`127.0.0.1:9004` → `CH_METADATA`) as
//! `[u32 BE MAGIC "META"][u32 BE len][marker][payload]` (v2, audit Fix #17 — the magic lets the host
//! resync after a truncated frame): `META_JSON` (a one-line JSON object, `{"kind":…}`) or
//! `META_ARTWORK` (`[id u8][JPEG bytes]`). A missing consumer is harmless (best-effort connect).

use std::collections::HashMap;
use std::io::Write;
use std::net::TcpStream;
use std::sync::Mutex;

// ---- seam framing (mirrors ocbm-proto's META_* markers) ----
const META_CMD: u8 = 0x01;
const META_JSON: u8 = 0x02;
const META_ARTWORK: u8 = 0x03;
const META_CORNERMASK: u8 = 0x04;
/// Seam framing v2 magic (audit Fix #17): a fixed 4-byte prefix ("META", BE) ahead of the length so the
/// host can deterministically resync to the next frame after a truncation (a partial write here, or an
/// OCBM cap-clear dropping a metadata chunk), instead of the old length-plausibility heuristic that could
/// swallow real frames while re-aligning. ocbmd byte-forwards this seam, so containment is end-to-end
/// (box emit → host parse); ocbmd is unchanged. Mirrors the audio seam's own "SEAV" magic.
const SEAM_MAGIC: u32 = 0x4D45_5441; // "META"
const SEAM_ADDR: &str = "127.0.0.1:9004";

static SINK: Mutex<Option<TcpStream>> = Mutex::new(None);

/// Send one framed metadata message to the seam, connecting lazily. Best-effort: with no host
/// consumer (or ocbmd restarting) this fails quietly and retries on the next message.
fn emit(marker: u8, payload: &[u8]) {
    // Recover from a poisoned lock rather than panicking: a panic in any other holder would
    // otherwise wedge the entire metadata plane at this .unwrap() for the process lifetime.
    let mut guard = SINK.lock().unwrap_or_else(|e| e.into_inner());
    emit_locked(&mut guard, marker, payload);
}

/// The body of [`emit`], with the lock already held.
///
/// Split out so [`emit_command_plist`] can do its whole send under the guard `try_lock` returned.
/// It used to `try_lock`, drop the guard, then call `emit` — which takes a BLOCKING lock, so between
/// the drop and the re-acquire the RCS reader thread could take it and the control thread would wait
/// the full connect+write anyway. On a single-core 528 MHz box that preemption window is wide, and
/// `std::sync::Mutex` is unfair, which is precisely the premise that made the reacquire unsafe.
fn emit_locked(guard: &mut Option<TcpStream>, marker: u8, payload: &[u8]) {
    let mut body = Vec::with_capacity(1 + payload.len());
    body.push(marker);
    body.extend_from_slice(payload);
    // [u32 BE MAGIC][u32 BE len][marker][payload] — the magic lets the host resync after a truncation
    // (audit Fix #17). `body` already = [marker][payload]; `len` = body.len() as before.
    let mut framed = SEAM_MAGIC.to_be_bytes().to_vec();
    framed.extend_from_slice(&(body.len() as u32).to_be_bytes());
    framed.extend_from_slice(&body);

    if guard.is_none() {
        let connected = match SEAM_ADDR.parse::<std::net::SocketAddr>() {
            Ok(sa) => TcpStream::connect_timeout(&sa, std::time::Duration::from_secs(2)),
            Err(_) => TcpStream::connect(SEAM_ADDR),
        };
        match connected {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                let _ = s.set_write_timeout(Some(std::time::Duration::from_secs(2)));
                let _ = writeln!(std::io::stderr(), "[meta] connected to ocbmd metadata seam {SEAM_ADDR}");
                *guard = Some(s);
            }
            Err(e) => {
                static WARNED: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    let _ = writeln!(std::io::stderr(), "[meta] seam {SEAM_ADDR} unavailable ({e}) — metadata dropped until ocbmd accepts");
                }
                return;
            }
        }
    }
    let ok = matches!(guard.as_mut().map(|s| s.write_all(&framed)), Some(Ok(())));
    if !ok {
        let _ = writeln!(std::io::stderr(), "[meta] seam write failed — reconnecting on next message");
        *guard = None; // dead seam → reconnect on the next message
    } else {
        static COUNT: portable_atomic::AtomicU64 = portable_atomic::AtomicU64::new(0);
        let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if n == 1 || n.is_multiple_of(100) {
            let _ = writeln!(std::io::stderr(), "[meta] forwarded {n} metadata messages to the host");
        }
    }
}

/// Emit a JSON metadata record (one line, `{"kind":"...",...}`).
pub fn emit_json(json: &str) {
    emit(META_JSON, json.as_bytes());
}

/// Forward an inbound AirPlay `/command` plist to the host's Metadata window.
///
/// Routed through the SAME `SINK` as the JSON and artwork records, deliberately. `receiver`'s
/// `session.rs` used to open its own second TCP connection to `127.0.0.1:9004` for these, and ocbmd
/// keeps exactly one producer per channel (`av_conns.retain(|(_, c)| *c != ch)`), so the two
/// connections inside the same `airplayd` process mutually EVICTED each other: every command plist
/// killed the JSON sink and vice versa. Observed on 2026-07-29 as three
/// `[meta] seam write failed — reconnecting` cycles, each exactly three `modesChanged` forwards after
/// the previous reconnect. The JSON side at least logged its losses; the command side discarded
/// `forward_to_sink`'s return value, so its drops were silent.
/// Best-effort, and deliberately so: this is the ONE emitter on the RTSP control thread.
///
/// It uses `try_lock` and DROPS the record if the seam is busy. The alternative — waiting — couples
/// the control plane to the metadata plane through a mutex that `emit` can hold across a 2 s connect
/// and a 2 s write, with an 8 MiB artwork write as the worst case. `std::sync::Mutex` is unfair and
/// NowPlaying alone emits ~2/s, so a contended seam could starve the control thread indefinitely, and
/// a stalled control connection tears the CarPlay session down. A dropped command plist costs a line
/// in the host's Metadata window; a stalled control thread costs the session.
pub fn emit_command_plist(plist: &[u8]) {
    let mut guard = match SINK.try_lock() {
        Ok(g) => g,
        // A poisoned lock (a prior holder panicked) must be RECOVERED, not treated like "busy" — else
        // every command plist is silently dropped for the process lifetime, mislogged as seam-busy
        // (audit Fix #5a). Matches emit()'s existing poison recovery.
        Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            static DROPPED: portable_atomic::AtomicU64 = portable_atomic::AtomicU64::new(0);
            let n = DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if n == 1 || n.is_multiple_of(50) {
                eprintln!("[meta] seam busy — dropped {n} command plist(s) rather than stall the control thread");
            }
            return;
        }
    };
    // Under the guard `try_lock` returned — NOT re-acquired.
    emit_locked(&mut guard, META_CMD, plist);
}

/// Emit iOS's exact per-session corner-mask PNG to the host: `[u32 BE display_width][png bytes]`.
///
/// Fires at AirPlay SETUP / SET_PARAMETER on the RTSP CONTROL thread (`server.rs`), so it mirrors
/// [`emit_command_plist`]'s `try_lock` discipline — dropping the record on seam contention rather than
/// the blocking [`emit`] — because a stalled control thread tears the CarPlay session down (see the
/// long note on `emit_command_plist`). `display_width` is the advertised main-display pixel width so the
/// host can size the corner exactly: `cp = view_width_pt × (png_n / display_width_px)` — reproducing
/// iOS's own discrete corner (68/102 px) at ANY resolution, replacing the old hardcoded width fraction.
pub fn emit_cornermask(display_width: u32, png: &[u8]) {
    let mut guard = match SINK.try_lock() {
        Ok(g) => g,
        // Recover a poisoned lock (see emit_command_plist / audit Fix #5a) instead of dropping forever.
        Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            eprintln!("[meta] seam busy — dropped a corner-mask PNG rather than stall the control thread");
            return;
        }
    };
    let mut p = Vec::with_capacity(4 + png.len());
    p.extend_from_slice(&display_width.to_be_bytes());
    p.extend_from_slice(png);
    // Under the guard `try_lock` returned — NOT re-acquired (same reason as emit_command_plist).
    emit_locked(&mut guard, META_CORNERMASK, &p);
}

/// Emit a complete artwork image: `[transfer id][JPEG bytes]`.
fn emit_artwork(id: u8, jpeg: &[u8]) {
    let mut p = Vec::with_capacity(1 + jpeg.len());
    p.push(id);
    p.extend_from_slice(jpeg);
    emit(META_ARTWORK, &p);
}

// ---------------------------------------------------------------------------------------------
// Subscribe builders — accessory → device. Each names EXACTLY the fields we want pushed (prereq 3).
// ---------------------------------------------------------------------------------------------

/// TLV param: `[len BE16][id BE16][value]`. A zero-length value is a *field selector* (presence-only).
fn tlv(id: u16, value: &[u8]) -> Vec<u8> {
    debug_assert!(4 + value.len() <= u16::MAX as usize, "TLV length overflows u16"); // audit Fix #23
    let len = (4 + value.len()) as u16;
    let mut o = len.to_be_bytes().to_vec();
    o.extend_from_slice(&id.to_be_bytes());
    o.extend_from_slice(value);
    o
}

/// A group param whose value is a concatenation of sub-TLVs. (Test-only helper for building subscribe
/// payloads in the unit tests; the production subscribe payloads are hand-built byte-exact.)
#[cfg(test)]
fn group(id: u16, subs: &[Vec<u8>]) -> Vec<u8> {
    let mut inner = Vec::new();
    for s in subs {
        inner.extend_from_slice(s);
    }
    tlv(id, &inner)
}

/// `0x5000 StartNowPlayingUpdates` — MediaItem (group 0) + Playback (group 1) field selectors.
///
/// **EXACTLY the wire-captured stock-CCPA subscribe** (40 B; `WIRED_METADATA_PLANE.md` ttylog:4272),
/// byte-for-byte. This is deliberate and was learned twice: iOS only streams the fields of a request
/// it fully accepts, and a subscribe carrying selectors it doesn't like degrades **silently** — you
/// keep getting elapsed-time deltas with no title/artist forever (observed 2026-07-12 when this
/// function requested a superset of SDK-declared ids: 287 NowPlaying updates, zero titles).
/// Extra ids the SDK declares (genre/composer/track+disc counts/queue/shuffle/repeat) are parsed by
/// `now_playing()` if they ever appear, but must NOT be requested here.
///
/// MediaItem (group 0): Title(1), Duration(4), AlbumTitle(6), Artist(12), ArtworkFileTransferId(26).
/// Playback  (group 1): PlaybackStatus(0), ElapsedTimeMs(1), AppName(7).
pub fn start_now_playing() -> Vec<u8> {
    vec![
        // group 0 MediaItemAttributes (len 0x18 = 24)
        0x00, 0x18, 0x00, 0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x04, 0x00, 0x04, 0x00, 0x04, 0x00,
        0x06, 0x00, 0x04, 0x00, 0x0c, 0x00, 0x04, 0x00, 0x1a,
        // group 1 PlaybackAttributes (len 0x10 = 16)
        0x00, 0x10, 0x00, 0x01, 0x00, 0x04, 0x00, 0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x04, 0x00,
        0x07,
    ]
}

/// `0x5200 StartRouteGuidanceUpdates` — echoes the RouteGuidanceDisplayComponent id (42) the
/// identify declared (param 30); iOS withholds guidance unless both match (docs/20 §1.3).
pub fn start_route_guidance() -> Vec<u8> {
    let mut o = tlv(0x0000, &42u16.to_be_bytes()); // RouteGuidanceDisplayComponentID
                                                   // Spec 0x5200 selectors: id 1 = SourceName, id 2 = SourceSupportsRouteGuidance (NOT
                                                   // "maneuver"/"lane" as previously commented). ExitInfo/timezone/EV are gated by ids 3/4/5.
    o.extend_from_slice(&tlv(0x0001, &[])); // SourceName
    o.extend_from_slice(&tlv(0x0002, &[])); // SourceSupportsRouteGuidance
    o
}

// `0x4154 StartCallStateUpdates`, `0x4157 StartCommunicationsUpdates` and `0x4170 StartListUpdates`
// are built from the field-selector lists in `features.rs`, which also generates the matching
// Identify declaration. They used to live here as three hand-written builders that no declaration
// referenced, which is how 0x4157/0x4170 came to be sent for a year with 0x4158/0x4171 undeclared.

// ---------------------------------------------------------------------------------------------
// Generic TLV walk + typed accessors
// ---------------------------------------------------------------------------------------------

/// Walk `[len BE16][id BE16][value]` params, calling `f(id, value)`.
fn walk(mut b: &[u8], mut f: impl FnMut(u16, &[u8])) {
    while b.len() >= 4 {
        let len = ((b[0] as usize) << 8) | b[1] as usize;
        let id = ((b[2] as u16) << 8) | b[3] as u16;
        if len < 4 || len > b.len() {
            return;
        }
        f(id, &b[4..len]);
        b = &b[len..];
    }
}

fn s(v: &[u8]) -> String {
    String::from_utf8_lossy(v)
        .trim_end_matches('\0')
        .to_string()
}
fn u8v(v: &[u8]) -> Option<u64> {
    v.first().map(|b| *b as u64)
}
fn uint(v: &[u8]) -> Option<u64> {
    match v.len() {
        1 => Some(v[0] as u64),
        2 => Some(u16::from_be_bytes([v[0], v[1]]) as u64),
        4 => Some(u32::from_be_bytes([v[0], v[1], v[2], v[3]]) as u64),
        8 => Some(u64::from_be_bytes([
            v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7],
        ])),
        _ => None,
    }
}

/// Minimal JSON string escape (the metadata is user content — titles can contain quotes).
fn esc(v: &str) -> String {
    let mut o = String::with_capacity(v.len() + 8);
    for c in v.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push(' '),
            c => o.push(c),
        }
    }
    o
}

/// Accumulates `key:value` JSON fields.
#[derive(Default)]
struct J(Vec<String>);
impl J {
    fn s(&mut self, k: &str, v: String) {
        self.0.push(format!("\"{k}\":\"{}\"", esc(&v)));
    }
    fn n(&mut self, k: &str, v: u64) {
        self.0.push(format!("\"{k}\":{v}"));
    }
    /// Signed JSON number (for signed iAP2 params like the timezone offset).
    fn i(&mut self, k: &str, v: i64) {
        self.0.push(format!("\"{k}\":{v}"));
    }
    fn finish(self, kind: &str) -> String {
        let mut o = format!("{{\"kind\":\"{kind}\"");
        for f in self.0 {
            o.push(',');
            o.push_str(&f);
        }
        o.push('}');
        o
    }
}

// ---------------------------------------------------------------------------------------------
// Parsers — one per Device→Accessory update. Field names mirror Apple's SDK parameter symbols.
// ---------------------------------------------------------------------------------------------

/// `0x5001 NowPlayingUpdate` — MediaItemAttributes (group 0) + PlaybackAttributes (group 1).
/// DELTA stream: emit only what this frame carried; the HOST merges (prereq 4).
pub fn now_playing(body: &[u8]) -> String {
    let mut j = J::default();
    walk(body, |gid, gv| match gid {
        0x0000 => walk(gv, |id, v| match id {
            1 => j.s("title", s(v)),
            4 => {
                if let Some(n) = uint(v) {
                    j.n("durationMs", n)
                }
            }
            6 => j.s("album", s(v)),
            7 => {
                if let Some(n) = uint(v) {
                    j.n("trackNumber", n)
                }
            }
            8 => {
                if let Some(n) = uint(v) {
                    j.n("trackCount", n)
                }
            }
            9 => {
                if let Some(n) = uint(v) {
                    j.n("discNumber", n)
                }
            }
            10 => {
                if let Some(n) = uint(v) {
                    j.n("discCount", n)
                }
            }
            12 => j.s("artist", s(v)),
            16 => j.s("genre", s(v)), // MediaItemGenre (spec id 16, was mis-numbered 13)
            18 => j.s("composer", s(v)), // MediaItemComposer (spec id 18, was mis-numbered 14)
            26 => {
                if let Some(n) = u8v(v) {
                    j.n("artworkId", n)
                }
            }
            _ => {}
        }),
        0x0001 => walk(gv, |id, v| match id {
            0 => {
                if let Some(n) = u8v(v) {
                    j.n("playbackStatus", n)
                }
            } // 0 stop 1 play 2 pause 3 seekFwd 4 seekBack
            1 => {
                if let Some(n) = uint(v) {
                    j.n("elapsedMs", n)
                }
            }
            2 => {
                if let Some(n) = uint(v) {
                    j.n("queueIndex", n)
                }
            } // PlaybackQueueIndex (spec id 2, was 3)
            3 => {
                if let Some(n) = uint(v) {
                    j.n("queueCount", n)
                }
            } // PlaybackQueueCount (spec id 3, was 4)
            5 => {
                if let Some(n) = u8v(v) {
                    j.n("shuffleMode", n)
                }
            }
            6 => {
                if let Some(n) = u8v(v) {
                    j.n("repeatMode", n)
                }
            }
            7 => j.s("appName", s(v)),
            12 => {
                if let Some(n) = uint(v) {
                    j.n("playbackSpeed", n)
                }
            } // PlaybackSpeed uint16 (spec id 12, was 16)
            _ => {}
        }),
        _ => {}
    });
    j.finish("nowPlaying")
}

/// `0x5201 RouteGuidanceUpdate` — trip summary (~1/s while a route is active).
///
/// Param 13 `RouteGuidanceManeuverCurrentList` is a **uint16 array of the upcoming maneuver
/// INDEXES** — iOS streams the whole maneuver list as separate `0x5202` messages (each carrying its
/// own Index), and this array says which are current. Its FIRST element is the next turn. Without
/// this the host has no way to tell which `0x5202` is live and shows the route's first maneuver
/// forever (the stale "Proceed to the route" bug, 2026-07-12).
pub fn route_guidance(body: &[u8]) -> String {
    let mut j = J::default();
    walk(body, |id, v| match id {
        1 => {
            if let Some(n) = u8v(v) {
                j.n("routeGuidanceState", n)
            }
        } // 0 none/ended, 1 active…
        2 => {
            if let Some(n) = u8v(v) {
                j.n("maneuverState", n)
            }
        }
        3 => j.s("currentRoadName", s(v)),
        4 => j.s("destinationName", s(v)),
        5 => {
            if let Some(n) = uint(v) {
                j.n("etaEpoch", n)
            }
        }
        6 => {
            if let Some(n) = uint(v) {
                j.n("timeRemainingSec", n)
            }
        }
        7 => {
            if let Some(n) = uint(v) {
                j.n("distanceRemainingM", n)
            }
        }
        8 => j.s("distanceRemainingText", s(v)),
        9 => {
            if let Some(n) = u8v(v) {
                j.n("distanceRemainingUnits", n)
            }
        }
        10 => {
            if let Some(n) = uint(v) {
                j.n("distanceToManeuverM", n)
            }
        }
        11 => j.s("distanceToManeuverText", s(v)),
        12 => {
            if let Some(n) = u8v(v) {
                j.n("distanceToManeuverUnits", n)
            }
        }
        13 => {
            // uint16[] — the first element is the CURRENT maneuver's index.
            if v.len() >= 2 {
                j.n(
                    "currentManeuverIndex",
                    u16::from_be_bytes([v[0], v[1]]) as u64,
                );
            }
        }
        14 => {
            if let Some(n) = uint(v) {
                j.n("maneuverTotalCount", n)
            }
        }
        // id 21 = DestinationTimeZoneOffsetMinutes (int16, SIGNED — e.g. UTC-8 = -480). Apple's catalog.
        21 => {
            if v.len() >= 2 {
                j.i(
                    "destTimeZoneOffsetMin",
                    i16::from_be_bytes([v[0], v[1]]) as i64,
                )
            }
        }
        // id 24 = ArrivalBatteryLevel (uint32) — was mislabeled as the tz offset (audit MED, metadata.rs).
        24 => {
            if let Some(n) = uint(v) {
                j.n("arrivalBatteryLevel", n)
            }
        }
        _ => {}
    });
    j.finish("routeGuidance")
}

/// `0x5202 RouteGuidanceManeuverInformation` — ONE maneuver out of the route's list. Always carries
/// its `index`; the host keys maneuvers by it and renders whichever index `0x5201` says is current.
pub fn maneuver(body: &[u8]) -> String {
    let mut j = J::default();
    walk(body, |id, v| match id {
        1 => {
            if let Some(n) = uint(v) {
                j.n("index", n)
            }
        }
        2 => j.s("description", s(v)),
        3 => {
            if let Some(n) = u8v(v) {
                j.n("maneuverType", n)
            }
        }
        4 => j.s("afterManeuverRoadName", s(v)),
        5 => {
            if let Some(n) = uint(v) {
                j.n("distanceM", n)
            }
        }
        6 => j.s("distanceText", s(v)),
        7 => {
            if let Some(n) = u8v(v) {
                j.n("distanceUnits", n)
            }
        }
        8 => {
            if let Some(n) = u8v(v) {
                j.n("drivingSide", n)
            }
        }
        9 => {
            if let Some(n) = u8v(v) {
                j.n("junctionType", n)
            }
        }
        13 => j.s("exitInfo", s(v)),
        _ => {}
    });
    j.finish("maneuver")
}

/// `0x4155 CallStateUpdate` — one record per active/ringing call (idle = a bare Status=0).
pub fn call_state(body: &[u8]) -> String {
    let mut j = J::default();
    walk(body, |id, v| match id {
        0 => j.s("remoteId", s(v)),
        1 => j.s("displayName", s(v)),
        2 => {
            if let Some(n) = u8v(v) {
                j.n("status", n)
            }
        } // spec CallStatus: 0 disconnected 1 sending 2 ringing 3 connecting 4 active 5 held 6 disconnecting
        3 => {
            if let Some(n) = u8v(v) {
                j.n("direction", n)
            }
        } // 1 incoming 2 outgoing
        4 => j.s("callUuid", s(v)),
        6 => j.s("addressBookId", s(v)),
        7 => j.s("label", s(v)),
        8 => {
            if let Some(n) = u8v(v) {
                j.n("service", n)
            }
        }
        9 => {
            if let Some(n) = u8v(v) {
                j.n("isConferenced", n)
            }
        }
        11 => {
            if let Some(n) = u8v(v) {
                j.n("disconnectReason", n)
            }
        }
        12 => {
            if let Some(n) = uint(v) {
                j.n("startTimestamp", n)
            }
        }
        _ => {}
    });
    j.finish("callState")
}

/// `0x4158 CommunicationsUpdate` — telephony capabilities/state (carrier, signal, call count…).
pub fn communications(body: &[u8]) -> String {
    let mut j = J::default();
    // IDs are 1:1 with the CarPlay Simulator's iAP2 CommunicationsUpdate (0x4158) spec: there is
    // NO id 3, and CarrierName=4/CellularSupported=5/TelephonyEnabled=6/… (the previous 3..7 map was
    // off-by-many). The 12..17 availability flags drive dialer button enablement.
    walk(body, |id, v| match id {
        0 => {
            if let Some(n) = u8v(v) {
                j.n("signalStrength", n)
            }
        }
        1 => {
            if let Some(n) = u8v(v) {
                j.n("registrationStatus", n)
            }
        }
        2 => {
            if let Some(n) = u8v(v) {
                j.n("airplaneMode", n)
            }
        }
        4 => j.s("carrierName", s(v)),
        5 => {
            if let Some(n) = u8v(v) {
                j.n("cellularSupported", n)
            }
        }
        6 => {
            if let Some(n) = u8v(v) {
                j.n("telephonyEnabled", n)
            }
        }
        7 => {
            if let Some(n) = u8v(v) {
                j.n("faceTimeAudioEnabled", n)
            }
        }
        8 => {
            if let Some(n) = u8v(v) {
                j.n("faceTimeVideoEnabled", n)
            }
        }
        9 => {
            if let Some(n) = u8v(v) {
                j.n("muteStatus", n)
            }
        }
        10 => {
            if let Some(n) = uint(v) {
                j.n("currentCallCount", n)
            }
        }
        11 => {
            if let Some(n) = uint(v) {
                j.n("newVoicemailCount", n)
            }
        }
        12 => {
            if let Some(n) = u8v(v) {
                j.n("initiateCallAvailable", n)
            }
        }
        13 => {
            if let Some(n) = u8v(v) {
                j.n("endAndAcceptAvailable", n)
            }
        }
        14 => {
            if let Some(n) = u8v(v) {
                j.n("holdAndAcceptAvailable", n)
            }
        }
        15 => {
            if let Some(n) = u8v(v) {
                j.n("swapAvailable", n)
            }
        }
        16 => {
            if let Some(n) = u8v(v) {
                j.n("mergeAvailable", n)
            }
        }
        17 => {
            if let Some(n) = u8v(v) {
                j.n("holdAvailable", n)
            }
        }
        _ => {}
    });
    j.finish("communications")
}

/// `0x4171 ListUpdate` — RecentsList (call history) / FavoritesList. Each list is a group whose
/// value repeats per-entry sub-TLVs (Apple's `_iAP2ListUpdate_RecentsList_*` symbol family).
/// Entries are emitted one JSON record each so the host can append them to a history table.
pub fn list_update(body: &[u8], out: &mut Vec<String>) {
    // `1 RecentsList` and `6 FavoritesList` are REPEATED groups (`countExpressionEnum` 0): the
    // parameter TLV is emitted once per entry and its value holds that entry's fields directly.
    // There is no per-entry wrapper.
    //
    // An earlier revision walked one level deeper, treating each field TLV as an entry group and its
    // value as more TLVs. That emitted ZERO records for every well-formed ListUpdate, which is why
    // call history never appeared even after 0x4171 started arriving (docs/ops/05_AUDITS.md §2). The proof it was
    // wrong is in this same file: `now_playing` parses `0 MediaItemAttributes` — a group of the same
    // declared shape — at ONE level, and that path is hardware-proven with 229 records carrying
    // titles. Apple's notes, our own device-accepted Identify params 16/17, and CINEMO's
    // `CinemoIAP2RecentsListItem[]` flat array all agree.
    // The list-availability and count scalars. Two of the three 0x4171 messages the phone sent on
    // 2026-07-29 carried ONLY these — `RecentsListAvailable=1, RecentsListCount=0` and
    // `FavoritesListAvailable=1, FavoritesListCount=1` — and produced no output at all, so the host
    // could not tell "no favorites" from "favorites unavailable", and had no expected count to check
    // its list against. `app_discovery` already reports its equivalents; this did not.
    let mut head = J::default();
    walk(body, |id, v| match id {
        0 => {
            if let Some(n) = u8v(v) {
                head.n("recentsAvailable", n)
            }
        }
        2 => {
            if let Some(n) = uint(v) {
                head.n("recentsCount", n)
            }
        }
        5 => {
            if let Some(n) = u8v(v) {
                head.n("favoritesAvailable", n)
            }
        }
        7 => {
            if let Some(n) = uint(v) {
                head.n("favoritesCount", n)
            }
        }
        _ => {}
    });
    if !head.0.is_empty() {
        out.push(head.finish("callListStatus"));
    }

    walk(body, |gid, gv| {
        let kind = match gid {
            0x0001 => "recentCall",
            0x0006 => "favorite",
            _ => return,
        };
        // Entry sub-ids per Apple's 0x4171: 0 Index, 1 RemoteID, 2 DisplayName, 3 Label,
        // 4 AddressBookID, 5 Service, 6 Type, 7 UnixTimestamp, 8 Duration, 9 Occurrences.
        let mut j = J::default();
        walk(gv, |id, v| match id {
            0 => {
                if let Some(n) = uint(v) {
                    j.n("index", n)
                }
            }
            1 => j.s("remoteId", s(v)),
            2 => j.s("displayName", s(v)),
            3 => j.s("label", s(v)),
            4 => j.s("addressBookId", s(v)),
            5 => {
                if let Some(n) = u8v(v) {
                    j.n("service", n)
                }
            }
            // Type: 0 Unknown, 1 Incoming, 2 Outgoing, 3 Missed — forwarded raw.
            6 => {
                if let Some(n) = u8v(v) {
                    j.n("callType", n)
                }
            }
            7 => {
                if let Some(n) = uint(v) {
                    j.n("unixTimestamp", n)
                }
            }
            8 => {
                if let Some(n) = uint(v) {
                    j.n("durationSec", n)
                }
            }
            9 => {
                if let Some(n) = u8v(v) {
                    j.n("occurrences", n)
                }
            }
            _ => {}
        });
        if !j.0.is_empty() {
            out.push(j.finish(kind));
        }
    });
}

/// Route one inbound device→accessory message to its parser and emit the result to the host seam.
///
/// Returns `false` for ids this module does not parse, so the caller can log them — an unhandled id
/// that we declared receivable is a real gap, and it used to be discovered only by noticing an empty
/// pane in the host app. Both transports call this: the wired driver (`iap2d`) and the wireless RCS
/// tunnel (`receiver::events`). They diverged once, with `0x4171 ListUpdate` handled on one side
/// only; there is one dispatcher now so they cannot.
pub fn dispatch(msg_id: u16, body: &[u8]) -> bool {
    use crate::spec::*;
    let many = |recs: Vec<String>| {
        for r in recs {
            emit_json(&r);
        }
    };
    match msg_id {
        MSG_NOW_PLAYING_UPDATE => emit_json(&now_playing(body)),
        MSG_ROUTE_GUIDANCE_UPDATE => emit_json(&route_guidance(body)),
        MSG_ROUTE_GUIDANCE_MANEUVER_INFORMATION => emit_json(&maneuver(body)),
        MSG_LANE_GUIDANCE_INFORMATION => {
            let mut r = Vec::new();
            lane_guidance(body, &mut r);
            many(r);
        }
        MSG_CALL_STATE_UPDATE => emit_json(&call_state(body)),
        MSG_COMMUNICATIONS_UPDATE => emit_json(&communications(body)),
        MSG_LIST_UPDATE => {
            let mut r = Vec::new();
            list_update(body, &mut r);
            many(r);
        }
        MSG_POWER_UPDATE => emit_json(&power(body)),
        MSG_APP_DISCOVERY_UPDATE => {
            let mut r = Vec::new();
            app_discovery(body, &mut r);
            many(r);
        }
        MSG_APP_DISCOVERY_APP_ICON => emit_json(&app_icon(body)),
        MSG_DESTINATION_INFORMATION => emit_json(&destination(body)),
        MSG_BLUETOOTH_CONNECTION_UPDATE => emit_json(&bluetooth_connection(body)),
        MSG_DEVICE_INFORMATION_UPDATE
        | MSG_DEVICE_LANGUAGE_UPDATE
        | MSG_DEVICE_TIME_UPDATE
        | MSG_DEVICE_UUID_UPDATE
        | MSG_WIRELESS_CAR_PLAY_UPDATE
        | MSG_DEVICE_TRANSPORT_IDENTIFIER_NOTIFICATION => emit_json(&device_update(msg_id, body)),
        MSG_VOICE_OVER_UPDATE => emit_json(&voice_over(body)),
        MSG_VOICE_OVER_CURSOR_UPDATE => emit_json(&voice_over_cursor(body)),
        MSG_ASSISTIVE_TOUCH_INFORMATION => emit_json(&assistive_touch(body)),
        MSG_MEDIA_LIBRARY_INFORMATION | MSG_MEDIA_LIBRARY_UPDATE => {
            emit_json(&media_library(msg_id, body))
        }
        _ => return false,
    }
    true
}

// ---------------------------------------------------------------------------------------------
// Extended metadata parsers (docs/carplay/05_METADATA_AND_CONTROLS.md). Parameter ids and enum meanings come from Apple's
// `iap2messages-internal.i2mspecarchive` via `tools/i2mspec_dump.py` — not from inference.
// ---------------------------------------------------------------------------------------------

/// `0xAE01 PowerUpdate` — the device's battery and charging state, plus the current-budget fields
/// the power negotiation uses. `batteryChargeLevel` is 0-100.
pub fn power(body: &[u8]) -> String {
    let mut j = J::default();
    walk(body, |id, v| {
        let num = |j: &mut J, k: &str| {
            if let Some(n) = uint(v) {
                j.n(k, n)
            }
        };
        match id {
            0 => num(&mut j, "maxCurrentFromAccessoryMa"),
            1 => num(&mut j, "willChargeIfPowerPresent"),
            2 => num(&mut j, "accessoryPowerMode"), // 1 low, 2 intermittent high, 3 ultra high
            3 => num(&mut j, "availableSiphoningCurrentMa"),
            4 => num(&mut j, "externalChargerConnected"),
            5 => num(&mut j, "batteryChargingState"), // 0 disabled, 1 charging, 2 charged
            6 => num(&mut j, "batteryChargeLevel"),
            7 => num(&mut j, "maxCurrentFromDeviceMa"),
            8 => num(&mut j, "chargerCurrentMa"),
            9 => num(&mut j, "chargerVoltageMv"),
            10 => num(&mut j, "reserveCurrentMa"),
            11 => num(&mut j, "minSiphoningCurrentMa"),
            12 => num(&mut j, "maxNonSiphoningCurrentMa"),
            _ => {}
        }
    });
    j.finish("power")
}

/// `0xAD01 AppDiscoveryUpdate` — the CarPlay app list (group 1) and the External Accessory app list
/// (group 4). One record per app so the host can build a table; a leading `appList` record carries
/// the availability and count scalars.
pub fn app_discovery(body: &[u8], out: &mut Vec<String>) {
    let mut head = J::default();
    walk(body, |id, v| match id {
        0 => {
            if let Some(n) = u8v(v) {
                head.n("carPlayListAvailable", n) // 0 unknown, 1 declined, 2 accepted
            }
        }
        2 => {
            if let Some(n) = uint(v) {
                head.n("carPlayListCount", n)
            }
        }
        3 => {
            if let Some(n) = u8v(v) {
                head.n("eaListAvailable", n) // 0 unknown, 1 unavailable, 2 available
            }
        }
        5 => {
            if let Some(n) = uint(v) {
                head.n("eaListCount", n)
            }
        }
        _ => {}
    });
    out.push(head.finish("appList"));

    // `1 CarPlayAppList` / `4 ExternalAccessoryAppList` are REPEATED groups: one TLV per app, fields
    // directly inside. See the note in `list_update` — the extra nesting emitted zero app records.
    walk(body, |gid, gv| {
        let list = match gid {
            1 => "carplay",
            4 => "externalAccessory",
            _ => return,
        };
        {
            let mut j = J::default();
            j.s("list", list.to_string());
            walk(gv, |id, v| match id {
                0 => {
                    if let Some(n) = uint(v) {
                        j.n("index", n)
                    }
                }
                1 => j.s("bundleId", s(v)),
                2 => j.s("displayName", s(v)),
                3 => {
                    if let Some(n) = uint(v) {
                        j.n("categoryId", n)
                    }
                }
                4 => j.n("iconIdLen", v.len() as u64),
                _ => {}
            });
            if j.0.len() > 1 {
                out.push(j.finish("app"));
            }
        }
    });
}

/// `0xAD04 AppDiscoveryAppIcon` — the icon's file-transfer id. The PNG itself arrives on an iAP2
/// File Transfer session, the same mechanism as album artwork.
pub fn app_icon(body: &[u8]) -> String {
    let mut j = J::default();
    walk(body, |id, v| match id {
        0 => j.s("bundleId", s(v)),
        1 => {
            if let Some(n) = u8v(v) {
                j.n("iconAvailable", n)
            }
        }
        2 => {
            if let Some(n) = u8v(v) {
                j.n("iconTransferId", n)
            }
        }
        _ => {}
    });
    j.finish("appIcon")
}

/// `0x5204 LaneGuidanceInformation` — one record per lane, with the recommendation status and the
/// angle the lane leads to. `laneStatus`: 0 do not take, 1 usable, 2 preferred.
pub fn lane_guidance(body: &[u8], out: &mut Vec<String>) {
    let mut index = None;
    walk(body, |id, v| {
        if id == 1 {
            index = uint(v);
        }
    });
    walk(body, |gid, gv| {
        if gid != 2 {
            return;
        }
        let mut j = J::default();
        if let Some(i) = index {
            j.n("guidanceIndex", i);
        }
        walk(gv, |id, v| match id {
            0 => {
                if let Some(n) = uint(v) {
                    j.n("index", n)
                }
            }
            1 => {
                if let Some(n) = u8v(v) {
                    j.n("laneStatus", n)
                }
            }
            2 | 3 => {
                if let (Some(a), Some(b)) = (v.first(), v.get(1)) {
                    let key = if id == 2 { "laneAngle" } else { "laneAngleHighlight" };
                    j.i(key, i16::from_be_bytes([*a, *b]) as i64)
                }
            }
            _ => {}
        });
        if j.0.len() > 1 {
            out.push(j.finish("laneGuidance"));
        }
    });
}

/// Q10.22 fixed-point decimal degrees, Apple's coordinate encoding.
fn q10_22(v: &[u8]) -> Option<f64> {
    if v.len() < 4 {
        return None;
    }
    Some(i32::from_be_bytes([v[0], v[1], v[2], v[3]]) as f64 / (1 << 22) as f64)
}

/// `0x10DB DestinationInformation` — the destination a CarPlay navigation app has set, so the head
/// unit can mirror it (cluster display, or handing off to native navigation).
pub fn destination(body: &[u8]) -> String {
    let mut j = J::default();
    walk(body, |id, v| match id {
        0 => j.s("destinationId", s(v)),
        1 => j.s("displayName", s(v)),
        2 => walk(v, |sid, sv| {
            if let Some(d) = q10_22(sv) {
                j.s(if sid == 0 { "latitude" } else { "longitude" }, format!("{d:.6}"));
            }
        }),
        3 => j.s("address", s(v)),
        4 => walk(v, |sid, sv| {
            if let Some(d) = q10_22(sv) {
                j.s(
                    if sid == 0 { "entryLatitude" } else { "entryLongitude" },
                    format!("{d:.6}"),
                );
            }
        }),
        5 => {
            if let Some(n) = uint(v) {
                j.n("coordinateThresholdM", n)
            }
        }
        6 => j.s("locale", s(v)),
        7 => j.s("sourceName", s(v)),
        _ => {}
    });
    j.finish("destination")
}

/// The `0x4E0x` device-state updates. These arrive unsolicited once declared receivable, so one
/// parser dispatches on the message id rather than each having its own entry point.
pub fn device_update(msg_id: u16, body: &[u8]) -> String {
    let mut j = J::default();
    match msg_id {
        0x4E09 => walk(body, |id, v| {
            if id == 0 {
                j.s("deviceName", s(v))
            }
        }),
        0x4E0A => walk(body, |id, v| {
            if id == 0 {
                j.s("language", s(v))
            }
        }),
        0x4E0B => walk(body, |id, v| match id {
            0 => {
                if let Some(n) = uint(v) {
                    j.n("unixTimestamp", n)
                }
            }
            1 => {
                if let (Some(a), Some(b)) = (v.first(), v.get(1)) {
                    j.i("timeZoneOffsetMin", i16::from_be_bytes([*a, *b]) as i64)
                }
            }
            2 => {
                if let Some(a) = v.first() {
                    j.i("daylightSavingOffsetMin", *a as i8 as i64)
                }
            }
            _ => {}
        }),
        0x4E0C => walk(body, |id, v| {
            if id == 0 {
                j.s("uuid", s(v))
            }
        }),
        0x4E0D => walk(body, |id, v| {
            if id == 0 {
                if let Some(n) = u8v(v) {
                    j.n("wirelessCarPlayAvailable", n)
                }
            }
        }),
        // Apple declares BOTH as <utf8>: 0 BluetoothTransportIdentifier, 1 USBTransportIdentifier.
        // Parsing them with `uint` emitted an empty record for a real 17-byte MAC string, and a
        // meaningless decimal of the ASCII bytes for any 1/2/4/8-byte value.
        0x4E0E => walk(body, |id, v| match id {
            0 => j.s("bluetoothTransportId", s(v)),
            1 => j.s("usbTransportId", s(v)),
            _ => {}
        }),
        _ => {}
    }
    j.finish("device")
}

/// `0x4E04 BluetoothConnectionUpdate` — which Bluetooth profiles the device currently has connected
/// to this accessory. Group 1's member ids are the profile enumeration; presence means connected.
pub fn bluetooth_connection(body: &[u8]) -> String {
    const PROFILE: [&str; 17] = [
        "handsFree",
        "phoneBookAccess",
        "",
        "avRemoteControl",
        "a2dp",
        "hid",
        "sensorService",
        "iap2Link",
        "panAccessPoint",
        "messageAccess",
        "passthrough",
        "gaming",
        "panClient",
        "braille",
        "multiStream",
        "leGattClient",
        "lea",
    ];
    let mut j = J::default();
    let mut connected: Vec<String> = Vec::new();
    walk(body, |id, v| match id {
        0 => {
            if let Some(n) = uint(v) {
                j.n("componentId", n)
            }
        }
        1 => walk(v, |pid, _| {
            if let Some(name) = PROFILE.get(pid as usize).filter(|n| !n.is_empty()) {
                connected.push((*name).to_string());
            }
        }),
        _ => {}
    });
    j.s("profiles", connected.join(","));
    j.finish("bluetoothConnection")
}

/// `0x560C VoiceOverUpdate` — whether VoiceOver is on and how it is configured.
pub fn voice_over(body: &[u8]) -> String {
    let mut j = J::default();
    walk(body, |id, v| {
        if let Some(n) = u8v(v) {
            match id {
                0 => j.n("speakingVolume", n),
                1 => j.n("speakingRate", n),
                2 => j.n("enabled", n),
                _ => {}
            }
        }
    });
    j.finish("voiceOver")
}

/// `0x5610 VoiceOverCursorUpdate` — the element VoiceOver is focused on.
pub fn voice_over_cursor(body: &[u8]) -> String {
    let mut j = J::default();
    walk(body, |id, v| match id {
        0 => j.s("label", s(v)),
        1 => j.s("value", s(v)),
        2 => j.s("hint", s(v)),
        _ => {}
    });
    j.finish("voiceOverCursor")
}

/// `0x5403 AssistiveTouchInformation`.
pub fn assistive_touch(body: &[u8]) -> String {
    let mut j = J::default();
    walk(body, |id, v| {
        if id == 0 {
            if let Some(n) = u8v(v) {
                j.n("enabled", n)
            }
        }
    });
    j.finish("assistiveTouch")
}

/// `0x4C01 MediaLibraryInformation` / `0x4C04 MediaLibraryUpdate` — summary only.
///
/// A full update is a paged database sync that can run to tens of thousands of items. CarPlay draws
/// its own browse UI, so mirroring the database buys nothing here; this reports the library identity,
/// its revision and how many items the frame carried, which is what a diagnostic view needs.
pub fn media_library(msg_id: u16, body: &[u8]) -> String {
    let mut j = J::default();
    let mut items = 0u64;
    match msg_id {
        // 0x4C01 MediaLibraryInformation: ONE top-level param, `0 MediaLibraryInformation <group>`,
        // containing 0 Name, 1 UniqueIdentifier, 2 Type. Applying 0x4C04's flat layout here
        // stringified the whole sub-TLV blob into `libraryId`, control bytes and all.
        0x4C01 => walk(body, |gid, gv| {
            if gid == 0 {
                walk(gv, |id, v| match id {
                    0 => j.s("libraryName", s(v)),
                    1 => j.s("libraryId", s(v)),
                    2 => {
                        if let Some(n) = u8v(v) {
                            j.n("libraryType", n)
                        }
                    }
                    _ => {}
                })
            }
        }),
        // 0x4C04 MediaLibraryUpdate: flat identifier + revision, then repeated `2 MediaItem` groups.
        // Summary only — a full sync is a paged database of tens of thousands of items and CarPlay
        // draws its own browse UI.
        _ => walk(body, |id, v| match id {
            0 => j.s("libraryId", s(v)),
            1 => j.s("revision", s(v)),
            2 => items += 1,
            _ => {}
        }),
    }
    j.n("itemsInFrame", items);
    j.finish("mediaLibrary")
}

// ---------------------------------------------------------------------------------------------
// Artwork — iAP2 File Transfer (link session 2). NowPlaying attr 26 carries only a transfer id;
// the JPEG arrives as session-2 datagrams (docs/20 §1.2, WIRED_ALBUM_ART.md — wire-decoded).
//   iPhone→box  [id][04][size u64 BE]   Setup   (size 0 = probe → ignore)
//   box→iPhone  [id][01]                Accept  (REQUIRED or no data flows)
//   iPhone→box  [id][flags][data…]      Data    flags: bit7 First, bit6 Last, low nibble = type
//   box→iPhone  [id][05]                Success (after the Last fragment)
// KNOWN GATE: the iPhone may never OFFER artwork (no attr 26, no session-2 frames) until the
// accessory declares a supported-artwork-format list in `0x1D01` — that param is unresolved in the
// grounded sources (WIRED_ALBUM_ART.md "STATUS"). This receiver is complete and will light up the
// moment the offer arrives; the host window reports the absence and this reason.
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
pub struct Artwork {
    xfers: HashMap<u8, (u64, Vec<u8>)>, // id -> (expected size, buffer)
}

/// One session-2 datagram. Returns an optional reply payload to send back on session 2.
impl Artwork {
    /// Total bytes buffered across ALL in-flight transfers.
    ///
    /// The 8 MiB per-transfer cap bounds one image; nothing bounded the SUM. The id is a `u8`, so a
    /// peer could open 256 transfers, declare 8 MiB each and feed them all one byte short of
    /// completion — a 2 GiB ceiling on a 123 MB box, with no timeout and eviction only on Cancel or
    /// completion. Measured: 200 x 512 KiB reached 102 MiB RSS before this bound existed.
    fn buffered(&self) -> usize {
        self.xfers.values().map(|(_, b)| b.len()).sum()
    }

    pub fn on_session2(&mut self, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.len() < 2 {
            return None;
        }
        let id = payload[0];
        let flags = payload[1];
        let ty = flags & 0x0F;
        match ty {
            4 => {
                // Setup: [id][04][size u64 BE]
                if payload.len() < 10 {
                    return None;
                }
                let size = u64::from_be_bytes([
                    payload[2], payload[3], payload[4], payload[5], payload[6], payload[7],
                    payload[8], payload[9],
                ]);
                if size == 0 || size > 8 << 20 {
                    return None; // the size-0 probe transfer — ignore (it gets Cancelled)
                }
                // Do NOT pre-allocate the attacker-declared `size` (up to 8 MiB, and ids are u8 so a
                // peer could open 256 transfers ≈ 2 GiB of eager allocation with zero data sent).
                // Start empty and let the buffer grow with actual bytes; the `buf.len() >= size`
                // completion check still bounds real growth.
                self.xfers.insert(id, (size, Vec::new()));
                eprintln!("[art] setup id=0x{id:02x} size={size} — accepting");
                Some(vec![id, 0x01]) // Accept — without this the iPhone sends nothing
            }
            0 => {
                // Data fragment (First bit 0x80 / Last bit 0x40 in the flags).
                // Aggregate ceiling across ALL in-flight transfers, checked after the append.
                // The 8 MiB per-transfer cap bounds one image; nothing bounded the sum.
                const MAX_TOTAL_BUFFERED: usize = 12 << 20; // 12 MiB, ~10% of box RAM
                #[allow(unused_assignments)]
                let mut over_budget = false;
                let done = {
                    let (size, buf) = self.xfers.get_mut(&id)?;
                    buf.extend_from_slice(&payload[2..]);
                    over_budget = buf.len() > MAX_TOTAL_BUFFERED;
                    (flags & 0x40) != 0 || buf.len() as u64 >= *size
                };
                if over_budget || self.buffered() > MAX_TOTAL_BUFFERED {
                    eprintln!(
                        "[art] aggregate buffer {} B across {} transfers exceeds \
                         {MAX_TOTAL_BUFFERED} B — dropping all in flight",
                        self.buffered(),
                        self.xfers.len()
                    );
                    self.xfers.clear();
                    return None;
                }
                if done {
                    if let Some((size, buf)) = self.xfers.remove(&id) {
                        // A Last fragment does NOT imply we received everything: any fragment dropped
                        // upstream (e.g. an RCS message that failed reassembly) leaves a short buffer.
                        // Emitting it would hand the host a truncated JPEG, and replying Success would
                        // tell the phone the transfer succeeded. Neither is recoverable, so refuse both
                        // and say so — a missing image is diagnosable, a corrupt one is not.
                        if (buf.len() as u64) < size {
                            eprintln!(
                                "[art] INCOMPLETE id=0x{id:02x} {} B of {size} — fragments were lost; \
                                 not emitting and not acknowledging",
                                buf.len()
                            );
                            return None;
                        }
                        // An OVER-length buffer is the signature of a duplicated fragment: the link
                        // layer has no duplicate-sequence suppression (`link::parse` just assigns
                        // `peer_seq`), so under the retransmitting SYN profiles a re-sent frame is
                        // re-dispatched and its bytes appended twice.
                        //
                        // Reviewed 2026-07-29 and deliberately NOT rejected. The proven wireless
                        // path negotiates Zero-Ack and so cannot retransmit; all four transfers on
                        // record are byte-exact; and when this does fire the delta is a garbled
                        // cover rather than a missing one, both self-healing on the next track.
                        // Rejecting on `len != size` is the only change that could drop an image
                        // that renders today. Named explicitly here so that if it ever does appear
                        // in a capture it is unmistakable rather than two numbers to compare.
                        // The real fix, if it is ever needed, is duplicate suppression in the link
                        // layer — artwork is just the one payload where re-dispatch is not
                        // idempotent.
                        if buf.len() as u64 != size {
                            eprintln!(
                                "[art] OVERLONG id=0x{id:02x} {} B > declared {size} — emitting \
                                 anyway (possible duplicated fragment; link layer has no dedupe)",
                                buf.len()
                            );
                        }
                        eprintln!(
                            "[art] complete id=0x{id:02x} {} B (expected {size})",
                            buf.len()
                        );
                        emit_artwork(id, &buf);
                        // Tell the host which id this image belongs to (pairs with NowPlaying artworkId).
                        emit_json(&format!("{{\"kind\":\"artworkReady\",\"artworkId\":{id}}}"));
                        return Some(vec![id, 0x05]); // Success
                    }
                }
                None
            }
            2 => {
                self.xfers.remove(&id); // Cancel
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_playing_parses_delta_fields() {
        // group0 MediaItem { Title(1)="Hi", Artist(12)="A" }
        let media = {
            let mut m = tlv(1, b"Hi");
            m.extend_from_slice(&tlv(12, b"A"));
            m
        };
        let body = group(0x0000, &[media]);
        let j = now_playing(&body);
        assert!(j.contains("\"kind\":\"nowPlaying\""), "{j}");
        assert!(j.contains("\"title\":\"Hi\""), "{j}");
        assert!(j.contains("\"artist\":\"A\""), "{j}");
    }

    #[test]
    fn json_escapes_quotes_in_titles() {
        let body = group(0x0000, &[tlv(1, b"He said \"hi\"")]);
        let j = now_playing(&body);
        assert!(j.contains("\\\"hi\\\""), "{j}");
    }

    #[test]
    fn subscribe_bodies_are_well_formed_tlvs() {
        for body in [start_now_playing(), start_route_guidance()] {
            let mut count = 0;
            walk(&body, |_, _| count += 1);
            assert!(count > 0, "subscribe body must contain params");
        }
        // The route-guidance subscribe must echo component id 42 (the identify's param-30 id).
        let rg = start_route_guidance();
        assert_eq!(&rg[0..6], &[0x00, 0x06, 0x00, 0x00, 0x00, 0x2a]);
    }

    // ---- Extended metadata parsers (docs/carplay/05_METADATA_AND_CONTROLS.md). Bodies are built from Apple's declared parameter
    // ids (`tools/i2mspec_dump.py`), so a wrong id here fails the test rather than the hardware.

    /// The other two `0x4171` bodies from the same live session carried only the availability and
    /// count scalars — `RecentsListAvailable=1, RecentsListCount=0` and `FavoritesListAvailable=1,
    /// FavoritesListCount=1`. They used to produce no output whatsoever, leaving the host unable to
    /// distinguish an empty list from an unavailable one.
    #[test]
    fn real_iphone_list_status_scalars_are_reported() {
        let mut recents = tlv(0, &[1]); // RecentsListAvailable = true
        recents.extend(tlv(2, &0u16.to_be_bytes())); // RecentsListCount = 0
        let mut out = Vec::new();
        list_update(&recents, &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("\"kind\":\"callListStatus\""), "{}", out[0]);
        assert!(out[0].contains("\"recentsAvailable\":1") && out[0].contains("\"recentsCount\":0"));

        let mut favs = tlv(5, &[1]);
        favs.extend(tlv(7, &1u16.to_be_bytes()));
        let mut out2 = Vec::new();
        list_update(&favs, &mut out2);
        assert!(out2[0].contains("\"favoritesCount\":1"), "{}", out2[0]);
    }

    /// Built from a `0x4171 ListUpdate` a live iPhone sent this accessory on 2026-07-29, archived at
    /// `docs/ops/captures/2026-07-29_iphone_0x4171_listupdate.txt`.
    ///
    /// ```text
    /// 40 40 00 74 41 71 | 00 6e 00 06 | 00 06 00 00 00 00 | 00 16 00 01 "+1 (907) 385-5769"
    ///  \____ iAP2 ____/   \_ id 6 _/   \_ id 0 Index _/   \__ id 1 RemoteID __/
    /// ```
    ///
    /// SCOPE, stated precisely: the real frame is 126 bytes and `log_datastream_frame` caps its hex
    /// dump at 48, so the capture shows the header plus the first two entry fields — 28 of the
    /// FavoritesList group's declared 106 bytes. The remaining 78 (further entry fields) were never
    /// dumped, so this fixture reproduces the VISIBLE PREFIX, not the whole frame. That prefix is
    /// what the encoding question turns on: the entry's fields sit directly inside the group, with
    /// no per-entry wrapper.
    ///
    /// The pre-fix parser walked one level deeper — it took `Index`'s 2-byte value as a TLV stream
    /// (too short, discarded) and `RemoteID`'s ASCII as one (`"+1"` reads as len 0x2B31, overruns,
    /// bails) — emitting ZERO records from exactly this shape.
    #[test]
    fn real_iphone_favorites_entry_parses() {
        let mut entry = tlv(0, &[0x00, 0x00]); // Index = 0
        entry.extend(tlv(1, b"+1 (907) 385-5769\0")); // RemoteID, NUL-terminated as sent
        let body = tlv(6, &entry); // FavoritesList

        let mut out = Vec::new();
        list_update(&body, &mut out);
        assert_eq!(out.len(), 1, "the captured frame must yield exactly one favorite: {out:?}");
        assert!(out[0].contains("\"kind\":\"favorite\""), "{}", out[0]);
        assert!(out[0].contains("\"remoteId\":\"+1 (907) 385-5769\""), "{}", out[0]);
        assert!(out[0].contains("\"index\":0"), "{}", out[0]);
    }

    /// A repeated group is one parameter TLV per entry, fields directly inside — no wrapper. This
    /// pins the shape that cost the project its call history: the parser walked one level deeper and
    /// emitted nothing at all (docs/ops/05_AUDITS.md §2). Fixtures here are built to Apple's declared layout, NOT
    /// to whatever the parser happens to accept.
    #[test]
    fn repeated_groups_are_flat_one_tlv_per_entry() {
        let entry = |idx: u16, remote: &str, name: &str, ctype: u8| {
            let mut e = tlv(0, &idx.to_be_bytes());
            e.extend(tlv(1, remote.as_bytes()));
            e.extend(tlv(2, name.as_bytes()));
            e.extend(tlv(6, &[ctype]));
            e
        };
        // Two RecentsList entries + one Favorites entry, each its own id-1 / id-6 TLV.
        let mut body = tlv(1, &entry(0, "+15551234567", "Jane Doe", 1));
        body.extend(tlv(1, &entry(1, "+15559876543", "John Roe", 3)));
        body.extend(tlv(6, &entry(0, "+15550000000", "Home", 0)));

        let mut out = Vec::new();
        list_update(&body, &mut out);
        assert_eq!(out.len(), 3, "expected 2 recents + 1 favorite, got {out:?}");
        assert!(out[0].contains("\"kind\":\"recentCall\"") && out[0].contains("Jane Doe"), "{}", out[0]);
        assert!(out[0].contains("\"callType\":1"), "Type must be forwarded raw: {}", out[0]);
        assert!(out[1].contains("John Roe") && out[1].contains("\"callType\":3"), "{}", out[1]);
        assert!(out[2].contains("\"kind\":\"favorite\"") && out[2].contains("Home"), "{}", out[2]);

        // The doubly-nested shape the old parser demanded must now yield nothing — if this ever
        // starts producing records again, the extra walk has been reintroduced.
        let nested = tlv(1, &tlv(0, &entry(0, "+15551234567", "Jane Doe", 1)));
        let mut out2 = Vec::new();
        list_update(&nested, &mut out2);
        assert!(out2.is_empty() || !out2[0].contains("Jane Doe"), "extra nesting parsed: {out2:?}");
    }

    /// 0x4E0E's two identifiers are `<utf8>`; parsing them as integers emitted an empty record for a
    /// real MAC-address string and a nonsense decimal for a short one.
    #[test]
    fn device_transport_identifiers_are_strings() {
        let mut body = tlv(0, b"AA:BB:CC:DD:EE:FF");
        body.extend(tlv(1, b"0000816000013"));
        let j = device_update(0x4E0E, &body);
        assert!(j.contains("\"bluetoothTransportId\":\"AA:BB:CC:DD:EE:FF\""), "{j}");
        assert!(j.contains("\"usbTransportId\":\"0000816000013\""), "{j}");
    }

    /// 0x4C01's single top-level param is a group; 0x4C04's are flat. Applying one layout to the
    /// other stringified the whole sub-TLV blob into `libraryId`.
    #[test]
    fn media_library_information_and_update_have_different_layouts() {
        let mut inner = tlv(0, b"My Library");
        inner.extend(tlv(1, b"ABC-123"));
        inner.extend(tlv(2, &[0]));
        let info = media_library(0x4C01, &tlv(0, &inner));
        assert!(info.contains("\"libraryName\":\"My Library\""), "{info}");
        assert!(info.contains("\"libraryId\":\"ABC-123\""), "{info}");

        let mut upd = tlv(0, b"ABC-123");
        upd.extend(tlv(1, b"rev-7"));
        upd.extend(tlv(2, &tlv(0, &[0; 8])));
        let update = media_library(0x4C04, &upd);
        assert!(update.contains("\"revision\":\"rev-7\"") && update.contains("\"itemsInFrame\":1"), "{update}");
    }

    #[test]
    fn power_update_reports_battery_and_charging() {
        let mut body = tlv(6, &[75]); // BatteryChargeLevel
        body.extend(tlv(5, &[1])); // BatteryChargingState = charging
        body.extend(tlv(4, &[1])); // IsExternalChargerConnected
        body.extend(tlv(0, &500u16.to_be_bytes())); // MaximumCurrentDrawnFromAccessory
        let j = power(&body);
        assert!(j.contains("\"kind\":\"power\""), "{j}");
        assert!(j.contains("\"batteryChargeLevel\":75"), "{j}");
        assert!(j.contains("\"batteryChargingState\":1"), "{j}");
        assert!(j.contains("\"maxCurrentFromAccessoryMa\":500"), "{j}");
    }

    #[test]
    fn app_discovery_emits_one_record_per_app() {
        let app = |idx: u16, bundle: &str, name: &str| {
            let mut e = tlv(0, &idx.to_be_bytes());
            e.extend(tlv(1, bundle.as_bytes()));
            e.extend(tlv(2, name.as_bytes()));
            e
        };
        // `1 CarPlayAppList` is a REPEATED group: one TLV per app, fields directly inside. The
        // previous fixture wrapped each app in an extra sub-group, which is not the wire shape — so
        // the test passed against a parser that emitted zero records from real data (docs/ops/05_AUDITS.md §2).
        let mut body = tlv(0, &[2]); // CarPlayAppListAvailable = accepted
        body.extend(tlv(2, &2u16.to_be_bytes())); // CarPlayAppListCount
        body.extend(tlv(1, &app(0, "com.apple.Maps", "Maps")));
        body.extend(tlv(1, &app(1, "com.spotify.client", "Spotify")));
        let mut out = Vec::new();
        app_discovery(&body, &mut out);
        assert_eq!(out.len(), 3, "one header + two apps: {out:?}");
        assert!(out[0].contains("\"carPlayListCount\":2"), "{}", out[0]);
        assert!(out[1].contains("com.apple.Maps") && out[1].contains("\"list\":\"carplay\""));
        assert!(out[2].contains("Spotify"));
    }

    #[test]
    fn destination_decodes_q10_22_coordinates() {
        // 37.3318° N — Q10.22 is degrees * 2^22.
        let lat = ((37.3318_f64 * (1 << 22) as f64) as i32).to_be_bytes();
        let lon = ((-122.0312_f64 * (1 << 22) as f64) as i32).to_be_bytes();
        let mut coord = tlv(0, &lat);
        coord.extend(tlv(1, &lon));
        let mut body = tlv(1, b"Apple Park");
        body.extend(tlv(2, &coord));
        body.extend(tlv(7, b"Maps"));
        let j = destination(&body);
        assert!(j.contains("\"displayName\":\"Apple Park\""), "{j}");
        assert!(j.contains("\"latitude\":\"37.3318"), "{j}");
        assert!(j.contains("\"longitude\":\"-122.0312"), "{j}");
        assert!(j.contains("\"sourceName\":\"Maps\""), "{j}");
    }

    #[test]
    fn device_update_dispatches_on_message_id() {
        assert!(device_update(0x4E09, &tlv(0, b"Owner's iPhone")).contains("Owner"));
        assert!(device_update(0x4E0A, &tlv(0, b"en")).contains("\"language\":\"en\""));
        let mut time = tlv(0, &1_753_400_000u64.to_be_bytes());
        time.extend(tlv(1, &(-480i16).to_be_bytes())); // UTC-8
        let j = device_update(0x4E0B, &time);
        assert!(j.contains("\"unixTimestamp\":1753400000"), "{j}");
        assert!(j.contains("\"timeZoneOffsetMin\":-480"), "{j}");
    }

    #[test]
    fn bluetooth_connection_names_connected_profiles() {
        let mut profiles = tlv(0, &[]); // HandsFree
        profiles.extend(tlv(7, &[])); // iAP2Link
        let mut body = tlv(0, &2u16.to_be_bytes());
        body.extend(tlv(1, &profiles));
        let j = bluetooth_connection(&body);
        assert!(j.contains("\"profiles\":\"handsFree,iap2Link\""), "{j}");
        assert!(j.contains("\"componentId\":2"), "{j}");
    }

    #[test]
    fn lane_guidance_emits_one_record_per_lane() {
        let lane = |idx: u16, status: u8, angle: i16| {
            let mut e = tlv(0, &idx.to_be_bytes());
            e.extend(tlv(1, &[status]));
            e.extend(tlv(2, &angle.to_be_bytes()));
            e
        };
        let mut body = tlv(1, &0u16.to_be_bytes()); // LaneGuidanceIndex
        body.extend(tlv(2, &lane(0, 2, -45)));
        body.extend(tlv(2, &lane(1, 0, 90)));
        let mut out = Vec::new();
        lane_guidance(&body, &mut out);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out[0].contains("\"laneStatus\":2") && out[0].contains("\"laneAngle\":-45"));
        assert!(out[1].contains("\"laneAngle\":90"));
    }

    /// The dispatcher must own every id the feature table declares receivable — an update we asked
    /// for but do not parse reaches the host as nothing at all.
    #[test]
    fn dispatcher_handles_every_declared_update_id() {
        let f = crate::features::active_with_skip(
            crate::features::Tier::Capability,
            start_now_playing,
            start_route_guidance,
            &[], // hermetic (audit Fix #25): check the FULL Capability set regardless of /tmp skip
        );
        for id in crate::features::received_ids(&f) {
            assert!(
                dispatch(id, &[]),
                "0x{id:04X} is declared receivable but `dispatch` does not handle it"
            );
        }
    }

    /// Every JSON key the metadata plane emits, with the message that produces it.
    ///
    /// This is a CONTRACT with `host/CarPlayHost/carlink_macOS/App/MetadataWindow.swift`. The box can
    /// parse a message perfectly and the host still shows an empty pane if one side renames a key,
    /// and nothing in either build fails — that cost a full hardware session on 2026-07-25 with
    /// CommunicationsUpdate, PowerUpdate and the device updates all arriving and three panes blank.
    const EMITTED_KEYS: &[(&str, &str)] = &[
        ("power", "batteryChargeLevel"),
        ("power", "batteryChargingState"),
        ("power", "externalChargerConnected"),
        ("power", "accessoryPowerMode"),
        ("power", "maxCurrentFromAccessoryMa"),
        ("power", "maxCurrentFromDeviceMa"),
        ("appList", "carPlayListAvailable"),
        ("appList", "carPlayListCount"),
        ("appList", "eaListCount"),
        ("app", "bundleId"),
        ("app", "displayName"),
        ("appIcon", "iconTransferId"),
        ("device", "deviceName"),
        ("device", "language"),
        ("device", "uuid"),
        ("device", "unixTimestamp"),
        ("device", "timeZoneOffsetMin"),
        ("device", "wirelessCarPlayAvailable"),
        ("bluetoothConnection", "profiles"),
        // The 0x4171 family. `callListStatus` is the phone's own view of both lists; `recentCall`
        // and `favorite` are the entries. All three were unguarded until 2026-07-29 — the contract
        // list exists precisely to fail on a rename, and a new record type that is not in it is
        // working only by coincidence.
        ("callListStatus", "recentsAvailable"),
        ("callListStatus", "recentsCount"),
        ("callListStatus", "favoritesAvailable"),
        ("callListStatus", "favoritesCount"),
        ("recentCall", "remoteId"),
        ("recentCall", "callType"),
        ("recentCall", "unixTimestamp"),
        ("favorite", "displayName"),
        ("communications", "carrierName"),
        ("communications", "signalStrength"),
        ("communications", "registrationStatus"),
        ("communications", "currentCallCount"),
        ("destination", "displayName"),
        ("destination", "latitude"),
        ("laneGuidance", "laneStatus"),
        ("voiceOver", "enabled"),
        ("mediaLibrary", "revision"),
    ];

    /// The host must read every key the box emits. Cross-language, so it reads the Swift source
    /// directly — a rename on either side fails here instead of silently blanking a pane.
    #[test]
    fn host_app_reads_every_emitted_key() {
        let swift = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../host/CarPlayHost/carlink_macOS/App/MetadataWindow.swift"
        );
        let Ok(src) = std::fs::read_to_string(swift) else {
            // The host app is not part of every checkout; skip rather than fail spuriously.
            eprintln!("skipping: {swift} not present");
            return;
        };
        let mut missing = Vec::new();
        for (kind, key) in EMITTED_KEYS {
            if !src.contains(&format!("\"{key}\"")) {
                missing.push(format!("{kind}.{key}"));
            }
            if !src.contains(&format!("case \"{kind}\"")) {
                missing.push(format!("kind {kind} (no case)"));
            }
        }
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "host app does not read these emitted keys — the pane will render empty:\n  {}",
            missing.join("\n  ")
        );
    }

    /// And the reverse: the parsers must actually emit those keys from a representative body, so the
    /// contract list cannot drift away from the code it describes.
    #[test]
    fn parsers_emit_their_contract_keys() {
        let mut got: Vec<(String, String)> = Vec::new();
        let mut record = |json: &str| {
            let kind = json.split("\"kind\":\"").nth(1).unwrap().split('"').next().unwrap();
            for part in json.split(",\"").skip(1) {
                if let Some(k) = part.split('"').next() {
                    got.push((kind.to_string(), k.to_string()));
                }
            }
        };

        let mut power_body = tlv(6, &[75]);
        power_body.extend(tlv(5, &[1]));
        power_body.extend(tlv(4, &[1]));
        power_body.extend(tlv(2, &[2]));
        power_body.extend(tlv(0, &500u16.to_be_bytes()));
        power_body.extend(tlv(7, &900u16.to_be_bytes()));
        record(&power(&power_body));

        let mut comms = tlv(0, &[3]);
        comms.extend(tlv(1, &[4]));
        comms.extend(tlv(4, b"Carrier"));
        comms.extend(tlv(10, &[1]));
        record(&communications(&comms));

        record(&device_update(0x4E09, &tlv(0, b"iPhone")));
        record(&device_update(0x4E0A, &tlv(0, b"en")));
        record(&device_update(0x4E0C, &tlv(0, b"UUID")));
        let mut time = tlv(0, &1u64.to_be_bytes());
        time.extend(tlv(1, &(-480i16).to_be_bytes()));
        record(&device_update(0x4E0B, &time));
        record(&device_update(0x4E0D, &tlv(0, &[1])));

        let mut bt = tlv(0, &1u16.to_be_bytes());
        bt.extend(tlv(1, &tlv(0, &[])));
        record(&bluetooth_connection(&bt));

        let mut dest = tlv(1, b"Home");
        let mut coord = tlv(0, &(1i32 << 22).to_be_bytes());
        coord.extend(tlv(1, &(1i32 << 22).to_be_bytes()));
        dest.extend(tlv(2, &coord));
        record(&destination(&dest));

        record(&voice_over(&tlv(2, &[1])));
        record(&media_library(0x4C04, &tlv(1, b"rev1")));
        record(&app_icon(&tlv(2, &[7])));

        let mut apps = Vec::new();
        let mut entry = tlv(1, b"com.apple.Maps");
        entry.extend(tlv(2, b"Maps"));
        let mut body = tlv(0, &[2]);
        body.extend(tlv(2, &1u16.to_be_bytes()));
        body.extend(tlv(5, &0u16.to_be_bytes()));
        body.extend(tlv(1, &entry)); // repeated group: one TLV per app, fields directly inside
        app_discovery(&body, &mut apps);
        for a in &apps {
            record(a);
        }

        // 0x4171: the status scalars, plus one Recents and one Favorites entry. Shapes match the
        // frames a live iPhone sent on 2026-07-29.
        let mut status = tlv(0, &[1]);
        status.extend(tlv(2, &2u16.to_be_bytes()));
        status.extend(tlv(5, &[1]));
        status.extend(tlv(7, &1u16.to_be_bytes()));
        let mut recent = tlv(1, b"+15551234567");
        recent.extend(tlv(6, &[1]));
        recent.extend(tlv(7, &1_700_000_000u64.to_be_bytes()));
        status.extend(tlv(1, &recent));
        status.extend(tlv(6, &tlv(2, b"Home")));
        let mut lists = Vec::new();
        list_update(&status, &mut lists);
        for l in &lists {
            record(l);
        }

        let mut lanes = Vec::new();
        let mut lane = tlv(0, &0u16.to_be_bytes());
        lane.extend(tlv(1, &[2]));
        lane_guidance(&tlv(2, &lane), &mut lanes);
        for l in &lanes {
            record(l);
        }

        let mut missing = Vec::new();
        for (kind, key) in EMITTED_KEYS {
            if !got.iter().any(|(k, v)| k == kind && v == key) {
                missing.push(format!("{kind}.{key}"));
            }
        }
        assert!(missing.is_empty(), "contract lists keys no parser emits: {missing:?}");
    }

    #[test]
    fn artwork_setup_accepts_and_reassembles() {
        let mut a = Artwork::default();
        // Setup id=0x81 size=4 → Accept
        let mut setup = vec![0x81, 0x04];
        setup.extend_from_slice(&4u64.to_be_bytes());
        assert_eq!(a.on_session2(&setup), Some(vec![0x81, 0x01]));
        // size-0 probe is ignored
        let mut probe = vec![0x80, 0x04];
        probe.extend_from_slice(&0u64.to_be_bytes());
        assert_eq!(a.on_session2(&probe), None);
        // Data first (0x80|type0), then last (0x40|type0) completes → Success
        assert_eq!(a.on_session2(&[0x81, 0x80, 0xff, 0xd8]), None);
        assert_eq!(
            a.on_session2(&[0x81, 0x40, 0xff, 0xd9]),
            Some(vec![0x81, 0x05])
        );
    }

    #[test]
    fn call_state_parses_display_name_and_status() {
        let mut body = tlv(1, b"Jane");
        body.extend_from_slice(&tlv(2, &[3])); // Status = connected
        let j = call_state(&body);
        assert!(j.contains("\"displayName\":\"Jane\""), "{j}");
        assert!(j.contains("\"status\":3"), "{j}");
    }
}
