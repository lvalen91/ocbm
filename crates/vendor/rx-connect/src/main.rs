//! rx-connect — CarPlay receiver discovery + connect-out.
//!
//! Advertises `_airplay._tcp` with the CarPlay TXT set, browses `_carplay-ctrl._tcp`, and dials the
//! iPhone's control port (`GET /ctrl-int/1/connect`) so it turns around and opens the RTSP session
//! to our advertised `:5000` — which is served by `airplayd` (the protocol core). This binary is
//! ONLY the external mDNS advertise + browse + connect-out.
//!
//! PORTED for wireless CarPlay (Phase A3, docs/wireless/00_WIRELESS_CARPLAY.md). The wired origin was hardcoded to `ncm0`/IPv6
//! link-local; here the interface + advertised address are env-configurable so ONE binary serves both
//! bearers:
//!   RX_IFACE  (default "ncm0")  — interface whose index scopes IPv6 link-local dials.
//!   RX_ADDR   (default unset)   — explicit advertised IPv4/IPv6 address; when unset, addr-auto
//!                                 advertises all interface addresses. For the wireless AP set
//!                                 RX_IFACE=wlan0 RX_ADDR=192.168.43.1 (the iPhone is an IPv4 DHCP
//!                                 client on that subnet, so no scope is needed for its dial).

use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr, SocketAddrV6};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// Fallback identity (wired / no per-box id supplied). Per-box (#636): carplay-wireless derives a
// per-box device-id/pi from hardware and passes it via CARPLAY_DEVICE_ID / CARPLAY_PI — read below so
// rx-connect advertises the SAME identity airplayd pairs with (they MUST match or pair-verify fails).
const DEVICE_ID_FALLBACK: &str = "AA:BB:CC:DD:EE:01";
const PI_FALLBACK: &str = "dd52a0b0-5435-42ea-99dc-0a6b96bb433b";
const PORT: u16 = 5000;

fn device_id() -> String {
    std::env::var("CARPLAY_DEVICE_ID").unwrap_or_else(|_| DEVICE_ID_FALLBACK.into())
}
fn pairing_identity() -> String {
    std::env::var("CARPLAY_PI").unwrap_or_else(|_| PI_FALLBACK.into())
}

fn mac_to_dec(mac: &str) -> u64 {
    mac.split(':').fold(0u64, |v, p| {
        (v << 8) | u64::from_str_radix(p, 16).unwrap_or(0)
    })
}

/// Dial the iPhone's CarPlay-control port: `GET /ctrl-int/1/connect` → iPhone turns around and opens
/// RTSP to our advertised `:5000`. Returns whether the TCP connect succeeded, so a transient failure
/// can be retried rather than deduping the peer forever.
async fn connect_out(addr: SocketAddr, dev_dec: u64) -> bool {
    // Bound the connect (:38): a stale/unroutable address — a phone addr that lagged the mDNS resolve,
    // or an IPv6 link-local whose scope has since changed — must not block the single-threaded browse
    // loop for the OS default connect timeout (minutes), which would stall ALL discovery. 3s is
    // generous on a local AP subnet.
    let mut s = match tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(addr)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            println!("[rx] connect-out {addr} failed: {e}");
            return false;
        }
        Err(_) => {
            println!("[rx] connect-out {addr} connect timed out (3s)");
            return false;
        }
    };
    let req = format!(
        "GET /ctrl-int/1/connect HTTP/1.1\r\nHost: {addr}\r\nUser-Agent: AirPlay/320.17\r\nAirPlay-Receiver-Device-ID: {dev_dec}\r\nConnection: keep-alive\r\n\r\n"
    );
    // Check the write (:43): a silently-dropped request is not a successful dial — returning true here
    // would mark the peer dialed forever without the iPhone ever being asked to open RTSP.
    if let Err(e) = s.write_all(req.as_bytes()).await {
        println!("[rx] connect-out {addr} write failed: {e}");
        return false;
    }
    println!("[rx] connect-out GET /ctrl-int/1/connect -> {addr} (devid={dev_dec})");
    let mut buf = [0u8; 1024];
    match tokio::time::timeout(Duration::from_secs(2), s.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => {
            let resp = String::from_utf8_lossy(&buf[..n]);
            // Truncate on the BYTE buffer before lossy-decoding (audit #6): byte-indexing the decoded
            // `String` at 160 can land mid-UTF-8-sequence and panic (daemon-fatal under panic="abort").
            // Slicing `buf` first lets from_utf8_lossy place the char boundary safely.
            println!(
                "[rx] connect-out resp {n}B: {}",
                String::from_utf8_lossy(&buf[..n.min(160)])
            );
            // Honor the HTTP status (:43): the genuine accept is `HTTP/1.1 200 OK`. An explicit non-2xx
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
        Ok(Ok(_)) | Err(_) => true, // empty response / read-timeout: write succeeded, treat as tentative
        Ok(Err(e)) => {
            println!("[rx] connect-out {addr} read failed: {e}");
            false
        }
    }
}

/// Resolve an interface name to its scope id. Called per-dial (not once at startup): on wireless the
/// AP interface (wlan0) can come up AFTER rx-connect starts, so a startup lookup would cache a
/// permanent 0 and break every IPv6 link-local dial (:108).
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

// Single-threaded runtime: the CCPA is a single-core Cortex-A7, and rx-connect's work (mDNS browse +
// a few connect-out dials) is I/O-bound, not CPU-bound — a multi-thread work-stealing scheduler only
// adds worker threads and code with nothing to steal. `flavor = "current_thread"` drops the
// `rt-multi-thread` feature (trimmed from Cargo.toml) for a smaller, leaner binary.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let dev_id = device_id();
    let pi = pairing_identity();
    let dev_dec = mac_to_dec(&dev_id);
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
        ("deviceid", dev_id.as_str()),
        // features = the genuine CCPA head unit's live Bonjour value. iOS reads this TXT at DISCOVERY
        // to classify the service; the HIGH word 0x61 = Car(bit32) | CarPlayControl(bit37) |
        // HKPairingAndEncrypt(bit38). The Car bit marks us a *CarPlay* receiver so the iPhone opens
        // RTSP back to :5000 (dropping it → iPhone answers the connect-out 200 OK but never opens RTSP,
        // so no session/video). Must equal /info features (info.rs).
        ("features", "0x44440B80,0x61"),
        ("flags", "0x4"),
        ("model", "CarLink-mac-1.0"),
        ("protovers", "1.0"),
        ("pi", pi.as_str()),
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
    while let Ok(ev) = browse.recv_async().await {
        match ev {
            ServiceEvent::ServiceResolved(info) => {
                // Scope for IPv6 link-local dials, resolved NOW rather than once at startup (:108).
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
                // good address is tried, and can't block this single-threaded browse loop from
                // processing queued mDNS events (incl. ServiceRemoved) for that long. One accepted dial
                // is enough — the iPhone opens RTSP off it (we don't dial the other family too, :125).
                // Same 10-round / 1s-backoff window as before, so the "source address lags the resolve
                // by a few seconds" tolerance is preserved.
                'rounds: for attempt in 1..=10u32 {
                    let mut tried = false;
                    for ip in &addrs {
                        let sa = dialable(*ip, port, scope);
                        if dialed.contains(&sa.to_string()) {
                            continue;
                        }
                        tried = true;
                        if connect_out(sa, dev_dec).await {
                            dialed.insert(sa.to_string());
                            break 'rounds;
                        }
                    }
                    // Every resolved address is already dialed (mDNS re-announce / TTL refresh of a
                    // connected peer) or none resolved — nothing to attempt. Fall through instantly like
                    // the pre-fix address-major loop did, instead of sleeping 10 rounds for nothing (audit
                    // Fix #12 v2, regression caught by the gate: without this a repeat ServiceResolved of
                    // an already-connected phone would stall this single-threaded browse loop ~10s).
                    if !tried {
                        break 'rounds;
                    }
                    println!("[rx] connect-out round {attempt}/10 — no address accepted, retrying in 1s");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            // The iPhone's control service went away (Wi-Fi drop / sleep). Clear the dial memory so it
            // gets re-dialed when it reappears (:112) — otherwise `dialed` pins the old address forever
            // and the phone can never reconnect without restarting rx-connect. Single-phone box, so
            // clearing the whole set on any removal is correct and simplest.
            ServiceEvent::ServiceRemoved(_ty, fullname) if !dialed.is_empty() => {
                println!("[rx] _carplay-ctrl removed ({fullname}) — clearing dial memory");
                dialed.clear();
            }
            _ => {}
        }
    }
    Ok(())
}
