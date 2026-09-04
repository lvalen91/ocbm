//! The transport-agnostic half of the bridge: the byte-copy loop, the session-end vocabulary, and
//! the wireless owner-claim decision.
//!
//! Everything here is pure — no usbdevfs, no sockets, no flags — so it compiles and is unit-tested
//! on the BUILD HOST (`cargo test -p aa-bridge`). That is the whole point of the split: the CCPA
//! offers no interactive debugger (it is OCBM or NCM, never both), so anything that can be settled
//! off-box must be.
//!
//! The WIRED pump is deliberately NOT built on this. It is bound to usbdevfs bulk ioctls
//! (`usb::bulk` on a raw fd, with its own EAGAIN/EINTR retry and its device-node watchdog), it is
//! device-proven, and forcing it through a `Read + Write` shim would have been a refactor of the
//! one path that ships. `docs/androidauto/03_WIRELESS.md` §5 ("one bridge or two") is answered the
//! way the doc asked: one bridge, one process, two transports — sharing the arbitration, the
//! logging discipline and this loop, not the AOAP setup.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use box_common::flags::ProjectionOwner;

/// Copy buffer size. Same 16 KiB the wired pump uses, so the two transports have the same read
/// granularity and their `+NB last` log numbers are comparable.
pub const BUF: usize = 16 * 1024;

/// Which way the bytes are going, for the log line only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    /// Phone -> host app.
    In,
    /// Host app -> phone.
    Out,
}

impl Dir {
    fn label(self) -> &'static str {
        match self {
            Dir::In => "IN phone->host",
            Dir::Out => "OUT host->phone",
        }
    }
}

/// Why a copy loop stopped. Distinct variants rather than `()` because the caller LOGS this as the
/// session-end reason and the three cases mean different things to whoever reads the log: a clean
/// peer close is the app quitting, a read error is the link dying, a write error is the far side
/// dying while this side still had data.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum End {
    /// The reader returned 0 — the peer closed cleanly.
    PeerClosed,
    /// The other direction (or a watchdog) cleared the `alive` flag first.
    Cancelled,
    /// `read` failed.
    ReadError(String),
    /// `write` failed — the far side is gone.
    WriteError(String),
}

impl End {
    pub fn as_str(&self) -> String {
        match self {
            End::PeerClosed => "peer closed".to_string(),
            End::Cancelled => "cancelled by the other direction".to_string(),
            End::ReadError(e) => format!("read error: {e}"),
            End::WriteError(e) => format!("write error: {e}"),
        }
    }
}

/// One direction of a full-duplex byte pump, with the wired pump's logging discipline.
///
/// Backpressure is the same as the wired loop's and is the reason this is a blocking `write_all`
/// and not a queue: AA carries an encapsulated TLS stream, so a dropped or reordered byte is
/// unrecoverable (ocbmd says the same about CH_IP). Blocking here propagates the slow side's
/// pressure back to the fast side's TCP window, which is exactly what we want — a buffer between
/// them would only turn a stall into a stall plus latency, then a stall plus loss.
///
/// `alive` is checked once per iteration, so the caller's watchdog bounds this loop to one blocking
/// read. Unblocking that read is the caller's job (a socket `shutdown`); this function cannot do it
/// generically, which is why the reason vocabulary exists instead.
pub fn copy_stream<R: Read, W: Write>(
    r: &mut R,
    w: &mut W,
    tag: &str,
    dir: Dir,
    t0: Instant,
    alive: &AtomicBool,
) -> (u64, End) {
    let mut buf = vec![0u8; BUF];
    let mut total: u64 = 0;
    let mut last = Instant::now();
    while alive.load(Ordering::Relaxed) {
        let n = match r.read(&mut buf) {
            Ok(0) => return (total, End::PeerClosed),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return (total, End::ReadError(e.to_string())),
        };
        total += n as u64;
        if let Err(e) = w.write_all(&buf[..n]) {
            return (total, End::WriteError(e.to_string()));
        }
        // Once a second, timestamped from session start — identical shape to the wired pump's two
        // lines, so a stall can be attributed to a DIRECTION by reading which timestamp stopped
        // advancing, on either transport, with the same eyes.
        if last.elapsed() >= Duration::from_secs(1) {
            eprintln!(
                "[{tag}] t={}s {} total={total} (+{n}B last)",
                t0.elapsed().as_secs(),
                dir.label()
            );
            last = Instant::now();
        }
    }
    (total, End::Cancelled)
}

/// What the wireless arm may do with the projection-owner flag when a phone dials the AP endpoint.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Claim {
    /// Nobody owns the box — take it as `wireless-aa`.
    Take,
    /// `wireless-aa` is ALREADY set. That is not a conflict: `carplay-wireless` claims it the moment
    /// the phone finishes the Bluetooth bootstrap and deliberately HOLDS it across the association
    /// (`crates/vendor/wireless/src/main.rs::run_aa_bootstrap`), precisely so nothing else can take
    /// the box out from under a phone that is mid-handoff. The TCP connect we are answering IS that
    /// phone arriving. Adopt the claim; do not re-write it.
    Adopt,
    /// Somebody else owns the box. First-come-first-served, no preemption in either direction
    /// (`docs/androidauto/02_ARBITRATION.md` §0) — refuse and close.
    Refuse,
}

/// The owner-claim decision for an inbound wireless-AA phone connection, as a pure function of the
/// current flag. Split out so the policy is testable without a `/tmp` flag file and so it reads as
/// one table rather than a chain of `if`s spread through the accept loop.
///
/// Note the asymmetry with the wired arm's `someone_else_owns()`: that one treats "not None and not
/// mine" as a refusal, where MINE is `WiredAa`. Here mine is `WirelessAa`, and `WiredAa` is a
/// refusal — a wired Android Auto session already owns the phone-facing port, and serving a second
/// AA session to (potentially) the same phone over Wi-Fi is exactly the two-sessions-one-phone bug
/// the widened wired check was added to prevent.
pub fn decide_wireless_claim(owner: ProjectionOwner) -> Claim {
    match owner {
        ProjectionOwner::None => Claim::Take,
        ProjectionOwner::WirelessAa => Claim::Adopt,
        _ => Claim::Refuse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A writer that fails after `ok_writes` successful calls, for the WriteError arm.
    struct FlakyWriter {
        ok_writes: usize,
        written: Vec<u8>,
    }
    impl Write for FlakyWriter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if self.ok_writes == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gone"));
            }
            self.ok_writes -= 1;
            self.written.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A reader that returns EINTR once before its data, so the retry arm is actually exercised —
    /// a `continue` on Interrupted that accidentally became a `return` would still pass a
    /// happy-path test.
    struct InterruptOnceReader {
        interrupted: bool,
        inner: Cursor<Vec<u8>>,
    }
    impl Read for InterruptOnceReader {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "eintr"));
            }
            self.inner.read(b)
        }
    }

    fn alive() -> AtomicBool {
        AtomicBool::new(true)
    }

    #[test]
    fn copies_every_byte_and_reports_a_clean_close() {
        let payload: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        let mut r = Cursor::new(payload.clone());
        let mut w: Vec<u8> = Vec::new();
        let (total, end) = copy_stream(&mut r, &mut w, "t", Dir::In, Instant::now(), &alive());
        assert_eq!(end, End::PeerClosed);
        assert_eq!(total, payload.len() as u64);
        // Byte-for-byte, ACROSS the 16 KiB buffer boundary: 40 000 B is three reads, and an
        // off-by-one in the `&buf[..n]` slice would only show up on the short final one.
        assert_eq!(w, payload);
    }

    #[test]
    fn an_interrupted_read_is_retried_not_reported() {
        let mut r = InterruptOnceReader {
            interrupted: false,
            inner: Cursor::new(b"hello".to_vec()),
        };
        let mut w: Vec<u8> = Vec::new();
        let (total, end) = copy_stream(&mut r, &mut w, "t", Dir::Out, Instant::now(), &alive());
        assert_eq!((total, end), (5, End::PeerClosed));
        assert_eq!(w, b"hello");
    }

    #[test]
    fn a_dead_far_side_ends_the_loop_as_a_write_error() {
        let mut r = Cursor::new(vec![0u8; 3 * BUF]);
        let mut w = FlakyWriter { ok_writes: 1, written: Vec::new() };
        let (total, end) = copy_stream(&mut r, &mut w, "t", Dir::In, Instant::now(), &alive());
        // One full buffer made it out; the second write failed and the loop stopped THERE rather
        // than spinning on a broken pipe for the rest of the stream.
        assert_eq!(total, 2 * BUF as u64);
        assert!(matches!(end, End::WriteError(_)), "{end:?}");
        assert_eq!(w.written.len(), BUF);
    }

    #[test]
    fn a_cleared_alive_flag_stops_the_loop_without_touching_the_stream() {
        let mut r = Cursor::new(b"never read".to_vec());
        let mut w: Vec<u8> = Vec::new();
        let dead = AtomicBool::new(false);
        let (total, end) = copy_stream(&mut r, &mut w, "t", Dir::In, Instant::now(), &dead);
        assert_eq!((total, end), (0, End::Cancelled));
        assert!(w.is_empty());
    }

    #[test]
    fn the_wireless_arm_takes_an_idle_box_and_adopts_its_own_bootstrap_claim() {
        assert_eq!(decide_wireless_claim(ProjectionOwner::None), Claim::Take);
        // carplay-wireless already claimed it for the phone that is dialling us right now.
        assert_eq!(decide_wireless_claim(ProjectionOwner::WirelessAa), Claim::Adopt);
    }

    #[test]
    fn the_wireless_arm_refuses_every_other_owner_including_wired_aa() {
        for owner in [
            ProjectionOwner::WiredCp,
            ProjectionOwner::WirelessCp,
            ProjectionOwner::WiredAa,
        ] {
            assert_eq!(decide_wireless_claim(owner), Claim::Refuse, "owner {owner:?}");
        }
    }
}
