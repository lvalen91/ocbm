//! The single acceptor for the host app's AA stream on `0.0.0.0:5277`.
//!
//! WHY A BROKER AND NOT TWO `accept()` CALLERS. Both transports need the SAME app-side socket: the
//! macOS app opens exactly one relay, `ocbmd` CH_IP `IP_OPEN 127.0.0.1:5277`, and it does so after
//! seeing `CT_PROJ_MODE`, which is a mirror of the projection-owner flag. If the wired loop and the
//! wireless listener both called `accept()` on that listener, the arm that happened to poll first
//! would win — and there is a real interleaving where that is the WRONG one:
//!
//!   1. No phone on USB. The wired loop is in its UNCLAIMED wait (`accept_while_wanted` with
//!      `UNCLAIMED_RETRY`), polling `accept()` every 250 ms and holding no claim.
//!   2. A phone finishes the Bluetooth bootstrap; `carplay-wireless` writes `wireless-aa`.
//!   3. `ocbmd::proj_mode_tick` (≤500 ms throttle) sends `PM_WIRELESS_AA`; the app opens its relay.
//!   4. The wired loop's next `accept()` — up to 250 ms before its own `someone_else_owns()` poll —
//!      takes that client, then runs `prepare_accessory()`, finds no phone on USB, gives up ~6 s
//!      later and DROPS the socket. The app is left holding a relay to nothing and the wireless
//!      phone never gets a head unit.
//!
//! So there is one acceptor thread and one queue, and the take is gated. The gate is in-process
//! (`wireless_intent`), not the flag file: the wireless arm registers intent BEFORE it writes the
//! flag, which makes the handoff ordered rather than merely likely. The flag is still consulted for
//! the wired arm, so an owner claimed by a DIFFERENT process (`carplay-wireless`, the CarPlay
//! supervisor) also parks it.

use std::collections::VecDeque;
use std::io::Write;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::someone_else_owns;

/// Which arm is asking for the app's connection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arm {
    Wired,
    Wireless,
}

/// How many accepted-but-unclaimed app connections to hold.
///
/// Two, not one: an app that quits and relaunches inside `ocbmd`'s heartbeat grace can have its
/// corpse's socket queued when the replacement's arrives (the F4 failure in
/// `docs/androidauto/02_ARBITRATION.md`). Holding both and taking the OLDEST first matches the
/// kernel backlog this replaced — the arm reads 0 from the corpse immediately and moves on. Beyond
/// two, the oldest is closed rather than queued, so a host in a connect loop cannot grow this
/// without bound.
const MAX_PENDING: usize = 2;

/// How long an accepted-but-untaken app connection stays queued.
///
/// There IS a state where neither arm may take one: `carplay-wireless` claims `wireless-aa` at the
/// end of the Bluetooth bootstrap and holds it while the phone associates, so for those few seconds
/// the wired arm is parked by the flag and the wireless arm has not registered intent yet. An app
/// that connects and then quits inside that window would otherwise leave a DEAD socket at the head
/// of the queue for the wireless arm to adopt as its host — a session that ends the instant it
/// starts, for no reason visible in either log. 30 s matches the wired `ANNOUNCE_WINDOW`: past that,
/// a host that has not been served has given up.
const PENDING_TTL: Duration = Duration::from_secs(30);

struct State {
    pending: VecDeque<(Instant, TcpStream)>,
    /// Set by the wireless arm from the instant it decides to serve a phone until its session ends.
    wireless_intent: bool,
}

impl State {
    /// Close anything nobody claimed in time. Called under the lock on every offer and every take,
    /// so no timer thread is needed.
    fn prune(&mut self, ttl: Duration) {
        while let Some((t, _)) = self.pending.front() {
            if t.elapsed() < ttl {
                return;
            }
            if let Some((_, c)) = self.pending.pop_front() {
                eprintln!("[aa-bridge] dropping a host connection nobody claimed in {}s", ttl.as_secs());
                let _ = c.shutdown(Shutdown::Both);
            }
        }
    }
}

pub struct AppPort {
    state: Mutex<State>,
    /// "Somebody other than the wired arm owns the box." A function pointer, not a direct call, for
    /// exactly one reason: the real one reads `/tmp/projection_owner`, and the gating policy is the
    /// part of this module worth unit-testing. Set once at construction; never swapped at runtime.
    other_owner: fn() -> bool,
    /// See `PENDING_TTL`. A field so the prune is testable without a 30 s test.
    ttl: Duration,
}

impl AppPort {
    /// Bind `0.0.0.0:port` and start the acceptor thread. Returns `Err` with the same message shape
    /// the old inline bind used, so the caller's exit path is unchanged.
    pub fn bind(port: u16) -> Result<Arc<AppPort>, std::io::Error> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        let me = Arc::new(AppPort {
            state: Mutex::new(State {
                pending: VecDeque::new(),
                wireless_intent: false,
            }),
            other_owner: someone_else_owns,
            ttl: PENDING_TTL,
        });
        let sink = me.clone();
        thread::spawn(move || {
            for c in listener.incoming() {
                match c {
                    Ok(c) => sink.offer(c),
                    Err(e) => {
                        // A per-connection accept error (ECONNABORTED, EMFILE) is not a reason to
                        // stop serving: the previous inline loop treated it as `Wait::Gone` and gave
                        // the whole session up. Log and keep accepting.
                        eprintln!("[aa-bridge] accept failed: {e}");
                    }
                }
            }
            eprintln!("[aa-bridge] app listener closed — no further host connections");
        });
        Ok(me)
    }

    pub(crate) fn offer(&self, c: TcpStream) {
        // The pump is blocking I/O in both arms. Linux does not propagate O_NONBLOCK across
        // accept(), but say so explicitly — the wired path used to and the guarantee is load-bearing.
        c.set_nonblocking(false).ok();
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.prune(self.ttl);
        while st.pending.len() >= MAX_PENDING {
            if let Some((_, old)) = st.pending.pop_front() {
                let _ = old.shutdown(Shutdown::Both);
            }
        }
        st.pending.push_back((Instant::now(), c));
    }

    /// Take the app's connection if it is THIS arm's to take. Non-blocking; `None` means either
    /// nothing is queued or the other arm owns the box.
    pub fn try_take(&self, arm: Arm) -> Option<TcpStream> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.prune(self.ttl);
        let allowed = match arm {
            // Ordered against the wireless arm in-process, and against the OTHER PROCESSES that can
            // own the box (carplay-wireless, the CarPlay supervisor) through the flag. The wired
            // loop's own `someone_else_owns()` poll runs at most every 250 ms; this closes that
            // window at the only point where it costs something.
            Arm::Wired => !st.wireless_intent && !(self.other_owner)(),
            Arm::Wireless => st.wireless_intent,
        };
        if !allowed {
            return None;
        }
        st.pending.pop_front().map(|(_, c)| c)
    }

    /// Register/clear the wireless arm's intent to serve. Set BEFORE the owner flag is written and
    /// cleared on every exit path, so the wired arm can never take a client for a session the
    /// wireless arm has already committed to.
    ///
    /// Anything already queued when intent is SET stays queued rather than being closed: it is the
    /// same app's relay either way, and the wireless arm is now the one entitled to take it.
    pub fn set_wireless_intent(&self, on: bool) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.wireless_intent = on;
    }

    /// Has the wireless arm committed to a session?
    ///
    /// The wired arm consults this at the two points it would WRITE the owner flag. The flag itself
    /// cannot carry that ordering: both arms read it and then write it, so the microseconds between
    /// the read and the write are a genuine TOCTOU that a file cannot close. This closes the
    /// in-process half of it outright — the cross-process half (this bridge vs `carplay-wireless`
    /// vs the shell supervisor) remains, and is bounded rather than eliminated: a wired arm that
    /// wins the flag race still cannot obtain a host client (`try_take` refuses it), so it releases
    /// after `ANNOUNCE_WINDOW` instead of projecting.
    pub fn wireless_intent(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).wireless_intent
    }

}

/// Best-effort "tell the app this is not happening" on a refusal path: shut the socket down so the
/// relay closes promptly instead of waiting on a read that will never be answered.
pub fn close(mut c: TcpStream) {
    let _ = c.flush();
    let _ = c.shutdown(Shutdown::Both);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::SocketAddr;

    fn idle() -> bool {
        false
    }
    fn busy() -> bool {
        true
    }

    fn port(other_owner: fn() -> bool, ttl: Duration) -> AppPort {
        AppPort {
            state: Mutex::new(State {
                pending: VecDeque::new(),
                wireless_intent: false,
            }),
            other_owner,
            ttl,
        }
    }

    /// A real connected loopback pair, because the queue holds `TcpStream`s and the prune/evict
    /// paths call `shutdown` on them. Returns (our end, the peer's end kept alive by the caller).
    fn pair() -> (TcpStream, TcpStream, SocketAddr) {
        let l = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let addr = l.local_addr().unwrap();
        let client = TcpStream::connect(addr).expect("connect");
        let (server, _) = l.accept().expect("accept");
        (server, client, addr)
    }

    const LONG: Duration = Duration::from_secs(30);

    #[test]
    fn the_wired_arm_takes_the_app_socket_on_an_idle_box() {
        let p = port(idle, LONG);
        let (s, _peer, _) = pair();
        p.offer(s);
        assert!(p.try_take(Arm::Wireless).is_none(), "no intent registered");
        assert!(p.try_take(Arm::Wired).is_some());
        assert!(p.try_take(Arm::Wired).is_none(), "queue is empty again");
    }

    /// THE interleaving this module exists for: the wireless arm registers intent BEFORE it writes
    /// the owner flag, so the wired arm cannot take the relay in the window where the flag still
    /// reads idle.
    #[test]
    fn wireless_intent_parks_the_wired_arm_before_the_flag_is_even_written() {
        let p = port(idle, LONG); // flag still says nobody owns the box
        let (s, _peer, _) = pair();
        p.set_wireless_intent(true);
        p.offer(s);
        assert!(p.try_take(Arm::Wired).is_none(), "wired must not steal a committed wireless session's host");
        assert!(p.try_take(Arm::Wireless).is_some());
    }

    #[test]
    fn a_socket_queued_before_the_intent_still_goes_to_the_wireless_arm() {
        let p = port(idle, LONG);
        let (s, _peer, _) = pair();
        p.offer(s); // arrived first...
        p.set_wireless_intent(true); // ...then the phone dialled the AP
        assert!(p.try_take(Arm::Wired).is_none());
        assert!(p.try_take(Arm::Wireless).is_some());
    }

    #[test]
    fn releasing_the_intent_gives_the_wired_arm_the_socket_back() {
        let p = port(idle, LONG);
        let (s, _peer, _) = pair();
        p.set_wireless_intent(true);
        p.offer(s);
        assert!(p.try_take(Arm::Wired).is_none());
        p.set_wireless_intent(false);
        assert!(p.try_take(Arm::Wired).is_some());
    }

    #[test]
    fn another_owner_parks_the_wired_arm_even_with_no_wireless_intent() {
        // e.g. carplay-wireless holding `wireless-aa` across the association, or a wired CarPlay
        // session holding `wired-cp`. Neither is this process, so the flag is the only signal.
        let p = port(busy, LONG);
        let (s, _peer, _) = pair();
        p.offer(s);
        assert!(p.try_take(Arm::Wired).is_none());
    }

    #[test]
    fn an_unclaimed_socket_is_closed_once_it_goes_stale() {
        let p = port(busy, Duration::from_millis(1)); // nobody may take it
        let (s, mut peer, _) = pair();
        p.offer(s);
        std::thread::sleep(Duration::from_millis(10));
        // Any take (or offer) prunes. Ask as the arm that is refused, so the ONLY thing that can
        // empty the queue is the prune.
        assert!(p.try_take(Arm::Wired).is_none());
        assert!(p.state.lock().unwrap().pending.is_empty(), "the stale socket must be dropped");
        // ...and actually shut down, not merely forgotten: the peer reads EOF.
        let mut buf = [0u8; 1];
        assert_eq!(peer.read(&mut buf).ok(), Some(0), "the app's end must see the close");
    }

    #[test]
    fn the_queue_never_grows_past_two() {
        let p = port(busy, LONG);
        let mut peers = Vec::new();
        for _ in 0..4 {
            let (s, peer, _) = pair();
            peers.push(peer);
            p.offer(s);
        }
        assert_eq!(p.state.lock().unwrap().pending.len(), MAX_PENDING);
    }
}
