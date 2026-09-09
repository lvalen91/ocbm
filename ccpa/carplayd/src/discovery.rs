//! CarPlay receiver discovery + connect-out — advertise `_airplay._tcp`, browse `_carplay-ctrl._tcp`,
//! and dial the iPhone's control port so it turns around and opens RTSP to our `:5000`.
//!
//! WAS A SEPARATE BINARY (`rx-connect`), merged here 2026-09-08. The two were never independent:
//! the supervisor launched them on consecutive lines and `kill_session` killed them together, and
//! `btd`'s wireless path spawned and pkilled them as a pair. What forced the merge is not lifecycle
//! but IDENTITY — the advertised `deviceid`/`pi` MUST equal the ones the pairing server verifies
//! with, or pair-verify fails. Two processes deriving the same values independently from the same
//! env vars is a correctness hazard with no upside; here they are function calls against the same
//! `OnceLock`s, so they cannot disagree.
//!
//! Merging also dropped tokio entirely. Everything below is a connect with a timeout, a write, a
//! read with a timeout, and a sleep — `std::net` does all four, and `mdns-sd` is sync already
//! (flume + polling + socket2, no async runtime). One fewer ~307 KB musl/std floor, one fewer
//! process in the double-spawn race, and one heap arena gone.
//!
//! Bearer selection is unchanged and still env-driven, so ONE build serves both:
//!   RX_IFACE  (default "ncm0")  — interface whose index scopes IPv6 link-local dials.
//!   RX_ADDR   (default unset)   — explicit advertised address; unset means addr-auto (all
//!                                 interface addresses). Wireless AP: RX_IFACE=wlan0
//!                                 RX_ADDR=192.168.43.1.

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::{HashMap, HashSet};
use std::io::{Read as _, Write as _};
use std::net::{IpAddr, SocketAddr, SocketAddrV6, TcpStream};
use std::time::Duration;

const PORT: u16 = 5000;

fn mac_to_dec(mac: &str) -> u64 {
    mac.split(':').fold(0u64, |v, p| {
        (v << 8) | u64::from_str_radix(p, 16).unwrap_or(0)
    })
}

/// Dial the iPhone's CarPlay-control port: `GET /ctrl-int/1/connect` → the iPhone turns around and
/// opens RTSP to our advertised `:5000`. Returns whether the dial was accepted, so a transient
/// failure is retried rather than deduping the peer forever.
fn connect_out(addr: SocketAddr, dev_dec: u64) -> bool {
    // Bound the connect: a stale/unroutable address — a phone addr that lagged the mDNS resolve, or
    // an IPv6 link-local whose scope has since changed — must not block this single browse thread
    // for the OS default connect timeout (minutes), which would stall ALL discovery. 3s is generous
    // on a local AP subnet. `connect_timeout` is the std equivalent of the tokio timeout this
    // replaced; it needs a single SocketAddr, which is exactly what `dialable` produces.
    let mut s = match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
        Ok(s) => s,
        Err(e) => {
            println!("[rx] connect-out {addr} failed: {e}");
            return false;
        }
    };
    let _ = s.set_write_timeout(Some(Duration::from_secs(3)));
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let req = format!(
        "GET /ctrl-int/1/connect HTTP/1.1\r\nHost: {addr}\r\nUser-Agent: AirPlay/320.17\r\nAirPlay-Receiver-Device-ID: {dev_dec}\r\nConnection: keep-alive\r\n\r\n"
    );
    // Check the write: a silently-dropped request is not a successful dial — returning true here
    // would mark the peer dialed forever without the iPhone ever being asked to open RTSP.
    if let Err(e) = s.write_all(req.as_bytes()) {
        println!("[rx] connect-out {addr} write failed: {e}");
        return false;
    }
    println!("[rx] connect-out GET /ctrl-int/1/connect -> {addr} (devid={dev_dec})");
    let mut buf = [0u8; 1024];
    match s.read(&mut buf) {
        Ok(n) if n > 0 => {
            let resp = String::from_utf8_lossy(&buf[..n]);
            // Truncate on the BYTE buffer before lossy-decoding (audit #6): byte-indexing the decoded
            // `String` at 160 can land mid-UTF-8-sequence and panic (daemon-fatal under panic="abort").
            // Slicing `buf` first lets from_utf8_lossy place the char boundary safely.
            println!(
                "[rx] connect-out resp {n}B: {}",
                String::from_utf8_lossy(&buf[..n.min(160)])
            );
            // Honor the HTTP status: the genuine accept is `HTTP/1.1 200 OK`. An explicit non-2xx
            // means the iPhone refused — DON'T mark the peer dialed, let the retry loop try again. A
            // successful write with no/partial response is treated as tentatively-accepted (the iPhone
            // can open RTSP asynchronously without a body), preserving the proven working path.
            match resp.split_whitespace().nth(1) {
                Some(code) if code.starts_with('2') => true,
                Some(code) => {
                    println!("[rx] connect-out {addr} refused (status {code}) — will retry");
                    false
                }
                None => true, // no parseable status line; don't over-tighten the working async case
            }
        }
        // Empty response, or the 2s read timeout expiring: the write succeeded, so treat it as
        // tentatively accepted exactly as the tokio version did (a read timeout arrived there as
        // `Err(_)` from `tokio::time::timeout`; here it is `WouldBlock`/`TimedOut` from the socket).
        Ok(_) => true,
        Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => true,
        Err(e) => {
            println!("[rx] connect-out {addr} read failed: {e}");
            false
        }
    }
}

/// Resolve an interface name to its scope id. Called per-dial (not once at startup): on wireless the
/// AP interface (wlan0) can come up AFTER this thread starts, so a startup lookup would cache a
/// permanent 0 and break every IPv6 link-local dial.
fn iface_scope(iface: &str) -> u32 {
    let c = match std::ffi::CString::new(iface) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    unsafe { libc::if_nametoindex(c.as_ptr()) }
}

/// Turn a resolved mDNS IP into a dialable SocketAddr. The box has no `.local` resolver, so we dial
/// the ADDRESS, not the hostname; an IPv6 link-local (fe80::) needs the receiving interface's scope
/// id. IPv4 (the wireless AP case) needs no scope.
fn dialable(ip: IpAddr, port: u16, scope: u32) -> SocketAddr {
    match ip {
        IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80 => {
            SocketAddr::V6(SocketAddrV6::new(v6, port, 0, scope))
        }
        _ => SocketAddr::new(ip, port),
    }
}

/// Spawn the advertise + browse loop. Called from `run_pairing_server` AFTER `:5000` is bound —
/// deliberately, and a behaviour improvement over the two-process arrangement: `rx-connect` could
/// (and did) advertise a receiver whose RTSP port was not yet listening, so an iPhone that dialled
/// in that window got a connection refused on the turn-around.
///
/// A discovery failure is FATAL to the process, deliberately. Both spawners assume "discovery is a
/// thread inside carplayd, so its death is carplayd's death" and watch ONLY the process:
/// `btd`'s AV latch (`av.rs`, `pid_alive` on the recorded carplayd pid — the rx-connect pid it
/// used to check alongside was removed with the merge) and the wired supervisor's
/// "session daemon died while PRESENT -> re-ARM" arm (`session_supervisor.sh`). If this thread
/// merely ended, carplayd would keep serving `:5000` that no phone can find: every 0x5702 retry
/// would read `ap_ok == true` and log "AV layer already existing — nothing to do", the supervisor
/// would see carplayd alive, and nothing would ever respawn it — the phone joins the AP and
/// projection never starts, with no error anywhere but this log. Exiting makes the assumption
/// true by construction: alive == advertising. Same policy as `start_input_listener`'s bind
/// failure in main.rs ("exiting IS the repair, not a crash loop" — both spawners back off).
///
/// The thread (rather than inline) is for the blocking browse/dial loop, not for isolation.
pub fn start(device_id: String, pairing_identity: String) {
    std::thread::spawn(move || {
        if let Err(e) = run(&device_id, &pairing_identity) {
            eprintln!("[rx] discovery failed: {e}");
            eprintln!(
                "[carplayd] FATAL: mDNS discovery is down — a receiver no phone can find is not a \
                 working receiver; exiting so btd / the supervisor respawn this daemon"
            );
            std::process::exit(1);
        }
    });
}

fn run(dev_id: &str, pi: &str) -> Result<(), Box<dyn std::error::Error>> {
    let dev_dec = mac_to_dec(dev_id);
    let iface = std::env::var("RX_IFACE").unwrap_or_else(|_| "ncm0".to_string());
    let adv_addr = std::env::var("RX_ADDR").unwrap_or_default(); // "" => addr-auto
    println!(
        "[rx] device id {dev_id} = {dev_dec} (dec); pi {pi}; iface={iface} adv_addr={}",
        if adv_addr.is_empty() {
            "addr-auto"
        } else {
            &adv_addr
        }
    );

    // mDNS advertise _airplay._tcp (CarPlay TXT) + browse _carplay-ctrl.
    let mdns = ServiceDaemon::new()?;
    let mut props: HashMap<String, String> = HashMap::new();
    for (k, v) in [
        ("deviceid", dev_id),
        // features = the genuine CCPA head unit's live Bonjour value. iOS reads this TXT at DISCOVERY
        // to classify the service; the HIGH word 0x61 = Car(bit32) | CarPlayControl(bit37) |
        // HKPairingAndEncrypt(bit38). The Car bit marks us a *CarPlay* receiver so the iPhone opens
        // RTSP back to :5000 (dropping it → iPhone answers the connect-out 200 OK but never opens RTSP,
        // so no session/video). Must equal /info features (info.rs).
        ("features", "0x44440B80,0x61"),
        ("flags", "0x4"),
        ("model", "CarLink-mac-1.0"),
        ("protovers", "1.0"),
        ("pi", pi),
        ("srcvers", "320.17"),
    ] {
        props.insert(k.to_string(), v.to_string());
    }
    // Advertise with an explicit address when RX_ADDR is set (wireless AP: the wlan0 IPv4 the iPhone
    // can reach); otherwise addr-auto publishes every interface address (wired default).
    let svc = ServiceInfo::new(
        "_airplay._tcp.local.",
        "CarPlay",
        "ncmcarplay.local.",
        adv_addr.as_str(),
        PORT,
        props,
    )?;
    let svc = if adv_addr.is_empty() {
        svc.enable_addr_auto()
    } else {
        svc
    };
    mdns.register(svc)?;
    println!("[rx] advertised _airplay._tcp 'CarPlay' :{PORT}");

    let browse = mdns.browse("_carplay-ctrl._tcp.local.")?;
    let mut dialed: HashSet<String> = HashSet::new();
    while let Ok(ev) = browse.recv() {
        match ev {
            ServiceEvent::ServiceResolved(info) => {
                // Scope for IPv6 link-local dials, resolved NOW rather than once at startup.
                let scope = iface_scope(&iface);
                let port = info.get_port();
                let host = info.get_hostname().trim_end_matches('.').to_string();
                let addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
                println!(
                    "[rx] resolved _carplay-ctrl {} -> {host}:{port} addrs={addrs:?} scope={iface}({scope})",
                    info.get_fullname()
                );
                // Dial the resolved ADDRESS(es), not the hostname (box has no .local resolver). Mark a
                // peer dialed only on SUCCESS, retrying transient failures with a short backoff (the
                // source address can lag the mDNS resolve by a few seconds right after association).
                // Interleave addresses per attempt round (audit Fix #12): try EACH resolved address once
                // per pass, THEN sleep — so a stale/unreachable address listed first (e.g. an IPv6
                // link-local that lags association) can't burn its whole ~40s retry budget before the
                // good address is tried, and can't block this browse thread from processing queued mDNS
                // events (incl. ServiceRemoved) for that long. One accepted dial is enough — the iPhone
                // opens RTSP off it (we don't dial the other family too).
                'rounds: for attempt in 1..=10u32 {
                    let mut tried = false;
                    for ip in &addrs {
                        let sa = dialable(*ip, port, scope);
                        if dialed.contains(&sa.to_string()) {
                            continue;
                        }
                        tried = true;
                        if connect_out(sa, dev_dec) {
                            dialed.insert(sa.to_string());
                            break 'rounds;
                        }
                    }
                    // Every resolved address is already dialed (mDNS re-announce / TTL refresh of a
                    // connected peer) or none resolved — nothing to attempt. Fall through instantly like
                    // the pre-fix address-major loop did, instead of sleeping 10 rounds for nothing (audit
                    // Fix #12 v2, regression caught by the gate: without this a repeat ServiceResolved of
                    // an already-connected phone would stall this browse thread ~10s).
                    if !tried {
                        break 'rounds;
                    }
                    println!("[rx] connect-out round {attempt}/10 — no address accepted, retrying in 1s");
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            // The iPhone's control service went away (Wi-Fi drop / sleep). Clear the dial memory so it
            // gets re-dialed when it reappears — otherwise `dialed` pins the old address forever and the
            // phone can never reconnect without restarting the daemon. Single-phone box, so clearing the
            // whole set on any removal is correct and simplest.
            ServiceEvent::ServiceRemoved(_ty, fullname) if !dialed.is_empty() => {
                println!("[rx] _carplay-ctrl removed ({fullname}) — clearing dial memory");
                dialed.clear();
            }
            _ => {}
        }
    }
    // The browse channel only closes when mdns-sd's daemon thread has exited (0.11.5 delivers
    // ServiceFound/Resolved/Removed with a BLOCKING `send`, never dropping the querier on a full
    // channel, and nothing here ever calls `shutdown()`), so this is "mDNS died mid-session" — an
    // Err, so `start` treats it exactly like a construction failure rather than ending quietly.
    Err("browse channel closed — the mDNS daemon thread exited".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_to_dec_matches_the_advertised_device_id() {
        // The value the box has always advertised, kept as a regression anchor across the merge:
        // this decimal is what goes out in `AirPlay-Receiver-Device-ID` and what the iPhone echoes.
        assert_eq!(mac_to_dec("AA:BB:CC:DD:EE:01"), 187_723_572_702_721);
        assert_eq!(mac_to_dec("00:00:00:00:00:00"), 0);
        // A malformed octet contributes 0 rather than panicking (daemon-fatal under panic="abort").
        assert_eq!(mac_to_dec("ZZ:00:00:00:00:01"), 1);
    }

    #[test]
    fn ipv6_link_local_dials_carry_the_interface_scope() {
        let v6: IpAddr = "fe80::1".parse().unwrap();
        match dialable(v6, 7000, 42) {
            SocketAddr::V6(a) => {
                assert_eq!(a.scope_id(), 42, "link-local dial without a scope is unroutable");
                assert_eq!(a.port(), 7000);
            }
            other => panic!("expected V6, got {other:?}"),
        }
        // A global v6 address takes no scope...
        let g: IpAddr = "2001:db8::1".parse().unwrap();
        assert!(matches!(dialable(g, 7000, 42), SocketAddr::V6(a) if a.scope_id() == 0));
        // ...and IPv4 (the wireless AP bearer) never does.
        let v4: IpAddr = "192.168.43.10".parse().unwrap();
        assert!(matches!(dialable(v4, 7000, 42), SocketAddr::V4(_)));
    }
}
