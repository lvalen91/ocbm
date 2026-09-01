//! Client for `carplayd`'s wired/wireless session arbiter (`docs/wireless/00_WIRELESS_CARPLAY.md` §2.5 "Reconnection & transport arbitration" and §3) --
//! connects to `/run/carplay/arbiter.sock`, attempts to claim the `wireless` transport, and (once
//! granted) watches for an unprompted `"preempt\n"` push if a wired session claims later.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub enum ClaimResult {
    /// Claim granted; holds the connection open (dropping it releases the claim implicitly, since
    /// the arbiter server clears a wireless claim on client disconnect).
    Granted(BufReader<UnixStream>),
    /// No arbiter server present (the socket path does not exist). Run standalone as the sole
    /// transport -- used until the dual-transport arbiter is wired into the box supervisor (docs/wireless/00_WIRELESS_CARPLAY.md).
    GrantedStandalone,
    /// Wired currently holds the arbiter -- caller should wait and retry.
    Denied,
}

fn log(m: &str) {
    println!("[arbiter-client] {m}");
}

/// Connect to the arbiter and attempt to claim `wireless`.
pub fn try_claim(sock_path: &str) -> std::io::Result<ClaimResult> {
    let mut stream = match UnixStream::connect(sock_path) {
        Ok(s) => s,
        // Socket path absent => no arbiter server yet (box wired supervisor is still the shell
        // session_supervisor.sh). Run standalone so the Bluetooth layer is usable now. A present
        // socket that refuses/errors is a real failure and propagates (caller retries).
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ClaimResult::GrantedStandalone);
        }
        Err(e) => return Err(e),
    };
    stream.write_all(b"claim wireless\n")?;
    // Bounded read, mirroring the 2026-07-07 wait_for_preempt fix below: without a timeout, an
    // arbiter that accepts the connection but never answers wedges this call (and the caller's
    // claim loop) forever. On Granted, wait_for_preempt re-sets its own 1s timeout, so this
    // 5s claim-reply bound never carries over.
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    match line.trim() {
        "ok" => {
            log("claimed wireless");
            Ok(ClaimResult::Granted(reader))
        }
        _ => Ok(ClaimResult::Denied),
    }
}

/// Block until the arbiter pushes `"preempt\n"` (wired claimed), the connection drops (arbiter
/// restarted), or `shutdown` is set. Any of these means the caller should tear down its wireless
/// session.
///
/// BUG FIX (live-reproduced 2026-07-07): this used to block on a plain `read_line()` with no
/// timeout, so a process kill/SIGTERM had no way to unblock it -- with wired never claiming during a
/// test, this call just blocks forever, and the process survives a plain `kill` until force-killed
/// by PID. Matches the SO_RCVTIMEO + shutdown-flag-poll discipline already used by every other
/// blocking loop in this crate (ssp_agent.rs, sdp_server.rs, rfcomm.rs) -- this was the one place
/// that didn't follow it.
pub fn wait_for_preempt(reader: &mut BufReader<UnixStream>, shutdown: &AtomicBool) {
    let _ = reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(1)));
    let mut line = String::new();
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                log("arbiter connection closed");
                return;
            }
            Ok(_) if line.trim() == "preempt" => {
                log("preempted by wired");
                return;
            }
            Ok(_) => continue, // unrecognized line, keep waiting
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue; // just a timeout tick -- loop back and re-check shutdown
            }
            Err(_) => return,
        }
    }
}
