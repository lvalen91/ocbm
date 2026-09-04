//! The WIRELESS transport half of the bridge: a TCP listener on the box's own SoftAP address that
//! answers the endpoint `carplay-wireless` advertised to the phone over Bluetooth.
//!
//! What happens before this module runs (`docs/androidauto/03_WIRELESS.md` §2f): the phone opens
//! RFCOMM channel 4, `carplay-wireless` speaks the seven-message bootstrap and hands it
//! `WifiStartRequest{ ip_address = box_common::net::AP_IP, port = aa_wireless::DEFAULT_PORT }` plus
//! the credentials of the AP that is actually running. The phone associates and dials that
//! endpoint. From its FIRST BYTE the socket carries the ordinary Android Auto stream — the same
//! bytes the wired AOAP endpoints carry — so the box's job is identical to the wired one: be a dumb
//! full-duplex pipe between the phone and the macOS app's AA engine.
//!
//! It therefore lives in `aa-bridge` and not in `carplay-wireless`, which is the answer to
//! §5's "one bridge or two": the AOAP-specific setup (control transfers, re-enumeration, interface
//! claim, bulk endpoints) is exactly the part that does NOT generalise, and it is untouched here.
//! Everything downstream of "I have two byte streams" is shared — the arbitration, the app-side
//! socket, the logging discipline, the copy loop in `pump.rs`.

use std::net::{TcpListener, TcpStream};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use box_common::cfg;
use box_common::flags::{self, ProjectionOwner};

use crate::appport::{self, AppPort, Arm};
use crate::pump::{self, Claim, Dir, End};
use crate::{host_present, ts, POLL};

/// Log tag. Deliberately distinct from the wired `[aa-bridge …]` so a merged `/tmp/aa-bridge.log`
/// can be split by transport with a single grep, which matters because both arms now run in one
/// process and append to one file.
const TAG: &str = "aa-bridge wl";

/// How long to hold a connected phone waiting for the macOS app to open its CH_IP relay.
///
/// Same 30 s as the wired `ANNOUNCE_WINDOW`, and for the same reason: the app connects in well
/// under a second once it sees `PM_WIRELESS_AA`, so anything longer is a host that does not speak
/// Android Auto at all — and holding the owner claim for it starves CarPlay.
const APP_WAIT: Duration = Duration::from_secs(30);

/// Retry period for the initial bind. The AP address only exists once `radio_hal.sh wifi_ap_on` has
/// run, and the supervisor starts this process alongside that bring-up, so `EADDRNOTAVAIL` on the
/// first attempt is EXPECTED, not an error.
const BIND_RETRY: Duration = Duration::from_secs(2);
/// Re-log a still-failing bind at most this often, so a box with `wifi_ap: false` (or a WLAN that
/// never came up) costs one line a minute rather than one every two seconds forever.
const BIND_LOG_EVERY: Duration = Duration::from_secs(60);

/// Bind the AP endpoint and serve phone connections forever. Runs on its own thread for the whole
/// life of the process; the wired arm is untouched by it.
pub fn run(app: Arc<AppPort>) {
    let addr = format!("{}:{}", box_common::net::AP_IP, aa_wireless::DEFAULT_PORT);
    let listener = match bind_with_retry(&addr) {
        Some(l) => l,
        None => return,
    };
    eprintln!("[{TAG} {}] listening on {addr} — waiting for a bootstrapped phone", ts());

    for conn in listener.incoming() {
        match conn {
            Ok(phone) => serve_one(&app, phone),
            Err(e) => {
                eprintln!("[{TAG} {}] accept failed: {e}", ts());
                thread::sleep(POLL);
            }
        }
    }
    eprintln!("[{TAG} {}] listener closed — wireless Android Auto is no longer served", ts());
}

/// Bind, retrying while the address does not exist yet.
///
/// Binding the AP ADDRESS rather than `0.0.0.0` is deliberate: this endpoint is only ever reachable
/// by a phone associated to our own SoftAP, and the address is the one we told the phone to dial.
/// A wildcard bind would also expose the raw AA stream on `ncm0` — the USB link the macOS app and
/// every bench tool sit on — where nothing authenticates it.
///
/// The cost is this loop, because the supervisor launches us next to `radio_hal.sh wifi_ap_on` and
/// the interface may not have the address for a second or two. Returns `None` only if the failure
/// is not "the address is not there yet", which is the one case retrying cannot fix.
fn bind_with_retry(addr: &str) -> Option<TcpListener> {
    let mut last_log: Option<Instant> = None;
    loop {
        match TcpListener::bind(addr) {
            Ok(l) => return Some(l),
            Err(e) => {
                let transient = matches!(
                    e.kind(),
                    std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::AddrInUse
                );
                if !transient {
                    eprintln!("[{TAG} {}] bind {addr} failed permanently: {e} — wireless Android Auto disabled", ts());
                    return None;
                }
                if last_log.map_or(true, |t| t.elapsed() >= BIND_LOG_EVERY) {
                    eprintln!("[{TAG} {}] {addr} not bindable yet ({e}) — retrying while the AP comes up", ts());
                    last_log = Some(Instant::now());
                }
                thread::sleep(BIND_RETRY);
            }
        }
    }
}

/// One phone connection, start to finish.
fn serve_one(app: &Arc<AppPort>, phone: TcpStream) {
    // The peer address is NOT logged. It is a DHCP lease on the box's own SoftAP and this file is
    // durable (`/tmp/aa-bridge.log` is collected into `/tmp/box.log` and pulled off the box), and
    // nothing downstream needs it: there is exactly one phone on this listener at a time and the
    // session is already identified by its timestamps.
    eprintln!("[{TAG}] phone connected from <redacted>");

    if !cfg::aa_enabled() {
        eprintln!("[{TAG} {}] android_auto:false in the pushed config — refusing this phone", ts());
        appport::close(phone);
        return;
    }

    // Intent BEFORE the claim (see `appport`): from this instant the wired arm may not take the
    // app's relay, so the app connection that `PM_WIRELESS_AA` is about to provoke lands here.
    app.set_wireless_intent(true);
    match pump::decide_wireless_claim(flags::owner()) {
        Claim::Adopt => {
            eprintln!("[{TAG} {}] owner=wireless-aa already (claimed by the Bluetooth bootstrap) — adopting", ts());
        }
        Claim::Take => {
            // Reachable when the bootstrap released early (a `PeerClosed` after it had already sent
            // the credentials) or when the phone reconnects to a still-configured AP without a new
            // bootstrap. The projection is real either way, so claim it.
            match flags::set_owner(ProjectionOwner::WirelessAa) {
                Ok(()) => eprintln!("[{TAG} {}] claimed projection owner = wireless-aa", ts()),
                Err(e) => eprintln!("[{TAG} {}] warning: could not claim projection owner: {e}", ts()),
            }
        }
        Claim::Refuse => {
            eprintln!(
                "[{TAG} {}] another projection ({}) owns the box — refusing this phone (first-come-wins)",
                ts(),
                flags::owner().as_str()
            );
            app.set_wireless_intent(false);
            appport::close(phone);
            return;
        }
    };

    let client = match wait_for_app(app) {
        Some(c) => c,
        None => {
            eprintln!("[{TAG} {}] no host app opened the AA stream in {}s — dropping the phone", ts(), APP_WAIT.as_secs());
            release(app);
            appport::close(phone);
            return;
        }
    };
    eprintln!("[{TAG} {}] host connected — pumping", ts());

    let (in_b, out_b, reason, secs) = serve_session(phone, client);
    eprintln!(
        "[{TAG} {}] session ended after {secs}s: {reason} (phone->host {in_b} B, host->phone {out_b} B)",
        ts()
    );
    release(app);
}

/// Drop the claim (ours only) and let the wired arm have the app socket back.
///
/// The flag is released whenever it still reads `wireless-aa`, including the case where the
/// Bluetooth bootstrap set it and we merely ADOPTED it. That is deliberate: this session ending IS
/// the wireless projection ending, and `carplay-wireless` holds the token past its own bootstrap
/// only so that nothing takes the box while the phone associates. Leaving it set would keep the box
/// "busy" for the whole remaining Bluetooth session with nothing projecting.
///
/// KNOWN LIMIT, worth stating rather than hiding: `release_owner_if_ours` is ours-only by TOKEN, and
/// this process and `carplay-wireless` write the SAME token. Neither can tell its own claim from the
/// other's. The consequence is bounded and one-directional — `carplay-wireless`'s session teardown
/// (`run_active_session`'s exit) can clear the flag under a live TCP session here — but that teardown
/// means the radio itself is going away, so the session is over regardless. Distinguishing them
/// would need a pid or a lock in the flag file, which is a change to a format three daemons and the
/// shell supervisor parse.
fn release(app: &Arc<AppPort>) {
    if flags::owner() == ProjectionOwner::WirelessAa {
        let _ = flags::clear_owner();
    }
    app.set_wireless_intent(false);
}

/// Poll for the macOS app's CH_IP relay, giving up on the same terms the wired arm does.
fn wait_for_app(app: &Arc<AppPort>) -> Option<TcpStream> {
    let deadline = Instant::now() + APP_WAIT;
    let mut warned_no_host = false;
    loop {
        if let Some(c) = app.try_take(Arm::Wireless) {
            return Some(c);
        }
        // The mid-session toggle is watched separately (see `serve_session`); this covers the user
        // turning Android Auto off in the window between the phone arriving and the app answering.
        if !cfg::aa_enabled() {
            eprintln!("[{TAG} {}] android_auto turned off while waiting for the host — standing down", ts());
            return None;
        }
        // If something ELSE took the box while we waited, our claim is gone and serving would be
        // projecting on top of another transport. Only reachable in the adopted case (we hold no
        // claim of our own to protect us) or if a process ignored first-come-wins.
        if flags::owner() != ProjectionOwner::WirelessAa {
            eprintln!(
                "[{TAG} {}] owner changed to '{}' while waiting for the host — standing down",
                ts(),
                flags::owner().as_str()
            );
            return None;
        }
        if !host_present() && !warned_no_host {
            // Not fatal and NOT debounced into a decision: ocbmd dips this flag for 2 s on every
            // re-subscribe. One line, then the deadline decides.
            eprintln!("[{TAG} {}] no host app is subscribed yet — waiting", ts());
            warned_no_host = true;
        }
        if Instant::now() >= deadline {
            return app.try_take(Arm::Wireless); // one last look, same as the wired arm's final accept
        }
        thread::sleep(POLL);
    }
}

/// Full-duplex TCP<->TCP pump. Returns `(phone->host bytes, host->phone bytes, reason, seconds)`.
///
/// Sibling of the wired `serve_session`, not a generalisation of it: that one is bound to usbdevfs
/// bulk ioctls on a raw fd and to a device-node watchdog, and both are meaningless here. What IS
/// shared is `pump::copy_stream` (the loop, the once-a-second per-direction totals) and the
/// teardown discipline — whichever direction dies first shuts BOTH sockets down, because the other
/// direction is parked in a blocking read that nothing else will ever wake.
fn serve_session(phone: TcpStream, client: TcpStream) -> (u64, u64, String, u64) {
    phone.set_nodelay(true).ok();
    client.set_nodelay(true).ok();
    arm_keepalive(&phone);
    let t0 = Instant::now();
    let alive = Arc::new(AtomicBool::new(true));

    // One handle per direction. `try_clone` dups the fd; `shutdown` is socket-level, so shutting
    // down ANY handle unblocks a read parked on any other.
    let mut phone_r = match phone.try_clone() {
        Ok(h) => h,
        Err(e) => return (0, 0, format!("phone socket clone failed: {e}"), 0),
    };
    let mut phone_w = phone;
    let mut app_r = match client.try_clone() {
        Ok(h) => h,
        Err(e) => return (0, 0, format!("host socket clone failed: {e}"), 0),
    };
    let mut app_w = client;

    // Watchdog: the `android_auto` toggle, mid-session. Same lever and same rationale as the wired
    // pump's (`02_ARBITRATION.md` F3 — the setting used to do nothing until the phone was
    // unplugged). There is no device-node watchdog here: a phone that leaves Wi-Fi is a TCP
    // half-open, which the AA stream's own traffic surfaces as a write error within seconds,
    // whereas a vanished USB device never fails its bulk ioctl at all.
    let wd_alive = alive.clone();
    let wd_phone = phone_w.try_clone().ok();
    let wd_app = app_w.try_clone().ok();
    let disabled = Arc::new(AtomicBool::new(false));
    let wd_disabled = disabled.clone();
    let watchdog = thread::spawn(move || {
        while wd_alive.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(500));
            if !wd_alive.load(Ordering::Relaxed) {
                return;
            }
            if !cfg::aa_enabled() {
                eprintln!("[{TAG} {}] android_auto turned off mid-session — ending session", ts());
                wd_disabled.store(true, Ordering::Relaxed);
                wd_alive.store(false, Ordering::Relaxed);
                shutdown_both(wd_phone.as_ref(), wd_app.as_ref());
                return;
            }
        }
    });

    // Thread A: phone -> host app.  This thread: host app -> phone.
    let a_alive = alive.clone();
    let a_phone = phone_w.try_clone().ok();
    let a_app = app_w.try_clone().ok();
    let reader = thread::spawn(move || {
        let (n, end) = pump::copy_stream(&mut phone_r, &mut app_w, TAG, Dir::In, t0, &a_alive);
        a_alive.store(false, Ordering::Relaxed);
        shutdown_both(a_phone.as_ref(), a_app.as_ref());
        (n, end)
    });

    let (out_b, out_end) = pump::copy_stream(&mut app_r, &mut phone_w, TAG, Dir::Out, t0, &alive);
    alive.store(false, Ordering::Relaxed);
    shutdown_both(Some(&phone_w), Some(&app_r));
    let (in_b, in_end) = reader.join().unwrap_or((0, End::Cancelled));
    let _ = watchdog.join();

    let reason = if disabled.load(Ordering::Relaxed) {
        "android_auto turned off mid-session".to_string()
    } else {
        // Both directions are reported. Which one is CAUSE and which is consequence is exactly the
        // question a stall investigation asks, and the wired pump's per-direction timestamps answer
        // it the same way.
        format!(
            "phone->host {}; host->phone {}",
            in_end.as_str(),
            out_end.as_str()
        )
    };
    (in_b, out_b, reason, t0.elapsed().as_secs())
}

/// TCP keepalive on the PHONE socket, aggressively tuned.
///
/// This closes the one failure the wired pump's device-node watchdog has no analogue for. A phone
/// that leaves Wi-Fi — drives out of range, radio off, battery dead — sends no FIN and no RST. Both
/// directions of this pump are then parked in a blocking read that nothing will ever wake: the
/// phone is silent because it is gone, and the host app is silent because AA is phone-driven and it
/// has nothing to answer. The session would hang FOREVER holding `wireless-aa`, which stands down
/// `arm()`, `kill_session()` and `escalate()` in the supervisor — i.e. it would lock CarPlay out of
/// the box until someone killed the process by hand. That is exactly the hazard the wired arm's
/// "every wait is bounded" rule exists for.
///
/// Kernel defaults are useless here (2 h idle). 10 s idle + 3 probes 5 s apart bounds it at ~25 s,
/// which is the same order as the ~20 s the wired path measured before its own watchdog was added.
///
/// Only the phone socket needs it: the host side is a loopback relay from `ocbmd`, and a dead
/// `ocbmd` closes its fds, so that direction cannot go half-open.
fn arm_keepalive(s: &TcpStream) {
    let fd = s.as_raw_fd();
    // SAFETY: setsockopt on a live fd this function borrows, with correctly-sized `c_int` values.
    // Every call is checked only insofar as failure is non-fatal — a kernel that refuses one of the
    // per-socket knobs leaves the socket exactly as it was, which is the pre-2026-09-04 behaviour.
    unsafe fn opt(fd: i32, level: i32, name: i32, val: i32) {
        libc::setsockopt(
            fd,
            level,
            name,
            &val as *const i32 as *const libc::c_void,
            std::mem::size_of::<i32>() as libc::socklen_t,
        );
    }
    unsafe {
        opt(fd, libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1);
        // The per-socket timing knobs are Linux names. The box is Linux; the macOS build exists only
        // so `cargo test` can reach `pump.rs`/`appport.rs`, and there SO_KEEPALIVE alone is fine.
        #[cfg(target_os = "linux")]
        {
            opt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, 10);
            opt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, 5);
            opt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPCNT, 3);
        }
    }
}

/// Shut both sockets down so neither blocking read can survive the other direction's exit.
fn shutdown_both(a: Option<&TcpStream>, b: Option<&TcpStream>) {
    if let Some(s) = a {
        let _ = s.shutdown(std::net::Shutdown::Both);
    }
    if let Some(s) = b {
        let _ = s.shutdown(std::net::Shutdown::Both);
    }
}
