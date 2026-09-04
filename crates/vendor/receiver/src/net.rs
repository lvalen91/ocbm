//! Transport glue — drive a [`ControlServer`] over any blocking byte stream (a `TcpStream` in the
//! daemon). Read bytes → `feed` → write the response bytes; loop until the peer closes. The protocol
//! state machine lives in `ControlServer`; this is just I/O, kept generic over `Read + Write` so it
//! can be exercised with an in-memory stream in tests. A single CarPlay controller connects at a
//! time, so a blocking one-connection-per-thread model suffices (as in the C receiver).

use std::io::{Read, Write};

use mfi::auth_client::MfiSigner;

use crate::server::{ControlServer, Pairings};

/// Idle window for the activity-based control-loop backstop — a session idle on BOTH control and A/V
/// for this long is torn down. Mirrors the C's `kAirPlayDataTimeoutSecs` (30 s).
const AV_IDLE_TEARDOWN_MS: u64 = 30_000;

/// Serve one connection to completion. Returns when the peer closes the stream or a protocol error
/// occurs (the latter mirrors the C receiver tearing the connection down).
pub fn serve_connection<T, P, S>(
    mut stream: T,
    server: &mut ControlServer<'_, P, S>,
) -> std::io::Result<()>
where
    T: Read + Write,
    P: Pairings,
    S: MfiSigner,
{
    let mut buf = [0u8; 8192];
    loop {
        let n = match stream.read(&mut buf) {
            Ok(n) => n,
            // SO_RCVTIMEO read-timeout expiry (EAGAIN → `WouldBlock`): the control channel went quiet.
            // Apply the activity-based backstop (#3) — if A/V DATA flowed within the window the session is
            // alive despite a quiet control channel, so keep waiting; only a session idle on BOTH control
            // and A/V is torn down (→ `AvSession::drop` resets it). Per-`read()`, so a slow handler can't
            // false-trip it.
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if let Some(idle) = server.av_idle_ms() {
                    if idle < AV_IDLE_TEARDOWN_MS {
                        continue; // A/V still flowing → session alive
                    }
                }
                eprintln!(
                    "[receiver] idle backstop tearing down session (screen_focused={})",
                    crate::events::screen_focused()
                );
                return Ok(());
            }
            // TCP-keepalive exhaustion surfaces as ETIMEDOUT → `TimedOut`, DISTINCT from the SO_RCVTIMEO
            // `WouldBlock` above: the TRANSPORT is dead (peer gone / cable / link lost). Tear down NOW —
            // this is mechanism #2's fast ~12 s detection, and it must NOT be gated behind the A/V-idle
            // backstop (A/V was flowing until the drop, so `av_idle` would be < 30 s and wrongly keep the
            // session alive). (On Unix, SO_RCVTIMEO → WouldBlock while a keepalive-killed socket →
            // TimedOut, so the two are reliably separable.)
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        if n == 0 {
            return Ok(()); // peer closed
        }
        match server.feed(&buf[..n]) {
            Ok(out) if !out.is_empty() => stream.write_all(&out)?,
            Ok(_) => {}
            // A protocol/decrypt error drops the connection. Log the variant — this was silent before,
            // and root-causing a teardown depends on telling a feed error apart from a peer EOF.
            Err(e) => {
                eprintln!("[receiver] control error → closing connection: {e:?}");
                return Ok(());
            }
        }
    }
}
