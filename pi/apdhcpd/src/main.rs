//! apdhcpd — a minimal DHCP server for the Raspberry Pi port's CarPlay SoftAP.
//!
//! # Why this exists
//!
//! The Pi runs `hostapd` standalone, outside Android's Wi-Fi framework, so nothing hands the iPhone
//! an address. Android ships `dnsmasq` **2.51 — a 2009 build** kept only as a vestige (modern
//! tethering uses NetworkStack's own Java `DhcpServer`), and it does not behave:
//!
//! * It answered `DHCPDISCOVER` with `DHCPOFFER` and the iPhone never sent `DHCPREQUEST` —
//!   i.e. the offer was not reaching the client — repeating until the phone abandoned the AP.
//! * `--dhcp-broadcast`, the obvious lever for exactly that failure, is not recognised by 2.51 and
//!   **silently disables DHCP altogether**: no error, no `DHCP, IP range` line, no bind on :67.
//!
//! Rather than keep guessing at an unmaintained binary, this does the one job needed, with the
//! behaviour the failure points at:
//!
//! **Every reply is broadcast to 255.255.255.255:68.** Unicasting a reply to a client that has no
//! address yet requires injecting an ARP entry for it, and that is the step most likely to be
//! failing here. Broadcast sidesteps it entirely and is always legal — RFC 2131 §4.1 permits the
//! server to broadcast when it cannot deliver a unicast, and clients must accept it.
//!
//! Scope is deliberately tiny: one interface, one pool, DISCOVER/REQUEST/RELEASE. No relays
//! (`giaddr` is refused rather than mishandled), no BOOTP, no DNS.

use std::collections::HashMap;
use std::net::{Ipv4Addr, UdpSocket};
use std::time::{Duration, Instant};

const SERVER_PORT: u16 = 67;
const CLIENT_PORT: u16 = 68;

/// `op` values.
const BOOTREQUEST: u8 = 1;
const BOOTREPLY: u8 = 2;
const HTYPE_ETHERNET: u8 = 1;
const HLEN_ETHERNET: u8 = 6;

/// Fixed BOOTP header length before the magic cookie.
const BOOTP_FIXED_LEN: usize = 236;
const MAGIC_COOKIE: [u8; 4] = [0x63, 0x82, 0x53, 0x63];

// DHCP option codes.
const OPT_PAD: u8 = 0;
const OPT_SUBNET_MASK: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_REQUESTED_IP: u8 = 50;
const OPT_LEASE_TIME: u8 = 51;
const OPT_MSG_TYPE: u8 = 53;
const OPT_SERVER_ID: u8 = 54;
const OPT_PARAM_REQUEST: u8 = 55;
const OPT_END: u8 = 255;

// DHCP message types (option 53).
const DHCPDISCOVER: u8 = 1;
const DHCPOFFER: u8 = 2;
const DHCPREQUEST: u8 = 3;
const DHCPDECLINE: u8 = 4;
const DHCPACK: u8 = 5;
const DHCPNAK: u8 = 6;
const DHCPRELEASE: u8 = 7;
const DHCPINFORM: u8 = 8;

fn msg_type_name(t: u8) -> &'static str {
    match t {
        DHCPDISCOVER => "DISCOVER",
        DHCPOFFER => "OFFER",
        DHCPREQUEST => "REQUEST",
        DHCPDECLINE => "DECLINE",
        DHCPACK => "ACK",
        DHCPNAK => "NAK",
        DHCPRELEASE => "RELEASE",
        DHCPINFORM => "INFORM",
        _ => "?",
    }
}

struct Config {
    iface: String,
    server_ip: Ipv4Addr,
    netmask: Ipv4Addr,
    router: Ipv4Addr,
    pool_start: Ipv4Addr,
    pool_len: u32,
    lease_secs: u32,
}

impl Default for Config {
    fn default() -> Self {
        // Matches `wireless/src/av.rs`'s `AP_IP` — the address the AirPlay receiver binds and the
        // one rx-connect advertises. If these ever disagree the phone joins and finds nothing.
        Config {
            iface: "wlan0".into(),
            server_ip: Ipv4Addr::new(192, 168, 43, 1),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
            router: Ipv4Addr::new(192, 168, 43, 1),
            pool_start: Ipv4Addr::new(192, 168, 43, 100),
            pool_len: 100,
            lease_secs: 43200,
        }
    }
}

const USAGE: &str = "\
apdhcpd — minimal DHCP server for the CarPlay SoftAP

USAGE:
    apdhcpd [--iface IF] [--server-ip A.B.C.D] [--netmask A.B.C.D] [--router A.B.C.D]
            [--pool-start A.B.C.D] [--pool-len N] [--lease-secs N]

Defaults match the Pi SoftAP: wlan0, 192.168.43.1/24, pool .100 +100, 12 h leases.
All replies are broadcast — see the module docs for why.";

fn parse_args() -> Result<Config, String> {
    let mut c = Config::default();
    let mut a = std::env::args().skip(1);
    while let Some(k) = a.next() {
        let mut val = |n: &str| a.next().ok_or_else(|| format!("{n} needs a value"));
        let ip = |s: String, n: &str| -> Result<Ipv4Addr, String> {
            s.parse().map_err(|_| format!("{n}: bad IPv4 address {s:?}"))
        };
        match k.as_str() {
            "--iface" => c.iface = val("--iface")?,
            "--server-ip" => c.server_ip = ip(val("--server-ip")?, "--server-ip")?,
            "--netmask" => c.netmask = ip(val("--netmask")?, "--netmask")?,
            "--router" => c.router = ip(val("--router")?, "--router")?,
            "--pool-start" => c.pool_start = ip(val("--pool-start")?, "--pool-start")?,
            "--pool-len" => {
                c.pool_len = val("--pool-len")?
                    .parse()
                    .map_err(|_| "--pool-len needs an integer".to_string())?
            }
            "--lease-secs" => {
                c.lease_secs = val("--lease-secs")?
                    .parse()
                    .map_err(|_| "--lease-secs needs an integer".to_string())?
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(c)
}

/// A parsed DHCP request. Only the fields a server needs.
#[derive(Debug, PartialEq)]
struct Request {
    xid: [u8; 4],
    flags: u16,
    ciaddr: Ipv4Addr,
    giaddr: Ipv4Addr,
    chaddr: [u8; 6],
    msg_type: u8,
    requested_ip: Option<Ipv4Addr>,
    server_id: Option<Ipv4Addr>,
    param_request: Vec<u8>,
}

fn ipv4(b: &[u8]) -> Ipv4Addr {
    Ipv4Addr::new(b[0], b[1], b[2], b[3])
}

fn parse_request(buf: &[u8]) -> Option<Request> {
    if buf.len() < BOOTP_FIXED_LEN + 4 || buf[0] != BOOTREQUEST {
        return None;
    }
    if buf[1] != HTYPE_ETHERNET || buf[2] != HLEN_ETHERNET {
        return None;
    }
    if buf[BOOTP_FIXED_LEN..BOOTP_FIXED_LEN + 4] != MAGIC_COOKIE {
        return None;
    }

    let mut r = Request {
        xid: [buf[4], buf[5], buf[6], buf[7]],
        flags: u16::from_be_bytes([buf[10], buf[11]]),
        ciaddr: ipv4(&buf[12..16]),
        giaddr: ipv4(&buf[24..28]),
        chaddr: [buf[28], buf[29], buf[30], buf[31], buf[32], buf[33]],
        msg_type: 0,
        requested_ip: None,
        server_id: None,
        param_request: Vec::new(),
    };

    // Walk the option block. Length-prefixed TLVs; PAD is a bare byte, END terminates.
    let mut i = BOOTP_FIXED_LEN + 4;
    while i < buf.len() {
        let code = buf[i];
        if code == OPT_END {
            break;
        }
        if code == OPT_PAD {
            i += 1;
            continue;
        }
        if i + 1 >= buf.len() {
            break;
        }
        let len = buf[i + 1] as usize;
        let val_start = i + 2;
        if val_start + len > buf.len() {
            break;
        }
        let val = &buf[val_start..val_start + len];
        match code {
            OPT_MSG_TYPE if len == 1 => r.msg_type = val[0],
            OPT_REQUESTED_IP if len == 4 => r.requested_ip = Some(ipv4(val)),
            OPT_SERVER_ID if len == 4 => r.server_id = Some(ipv4(val)),
            OPT_PARAM_REQUEST => r.param_request = val.to_vec(),
            _ => {}
        }
        i = val_start + len;
    }
    (r.msg_type != 0).then_some(r)
}

fn build_reply(cfg: &Config, req: &Request, msg_type: u8, yiaddr: Ipv4Addr) -> Vec<u8> {
    let mut p = vec![0u8; BOOTP_FIXED_LEN];
    p[0] = BOOTREPLY;
    p[1] = HTYPE_ETHERNET;
    p[2] = HLEN_ETHERNET;
    p[4..8].copy_from_slice(&req.xid);
    // Echo the client's flags, including its broadcast bit, so a client that asked for broadcast
    // still sees one set. We broadcast at the socket layer regardless (see module docs).
    p[10..12].copy_from_slice(&req.flags.to_be_bytes());
    if msg_type == DHCPACK {
        p[12..16].copy_from_slice(&req.ciaddr.octets());
    }
    p[16..20].copy_from_slice(&yiaddr.octets());
    p[20..24].copy_from_slice(&cfg.server_ip.octets());
    p[28..34].copy_from_slice(&req.chaddr);

    p.extend_from_slice(&MAGIC_COOKIE);
    let mut opt = |code: u8, val: &[u8]| {
        p.push(code);
        p.push(val.len() as u8);
        p.extend_from_slice(val);
    };
    opt(OPT_MSG_TYPE, &[msg_type]);
    opt(OPT_SERVER_ID, &cfg.server_ip.octets());
    if msg_type != DHCPNAK {
        opt(OPT_LEASE_TIME, &cfg.lease_secs.to_be_bytes());
        opt(OPT_SUBNET_MASK, &cfg.netmask.octets());
        opt(OPT_ROUTER, &cfg.router.octets());
        // Point DNS at ourselves. There is no resolver behind it, but omitting option 6 entirely
        // makes some clients treat the lease as unusable, and CarPlay needs no name resolution.
        opt(OPT_DNS, &cfg.server_ip.octets());
    }
    p.push(OPT_END);
    // Pad to the 300-byte BOOTP minimum; some clients drop shorter datagrams.
    while p.len() < 300 {
        p.push(OPT_PAD);
    }
    p
}

/// Lease table, keyed by MAC. Tiny and in-memory: a CarPlay AP serves one phone, and a reboot
/// re-leasing from scratch is correct behaviour here.
struct Leases {
    map: HashMap<[u8; 6], (Ipv4Addr, Instant)>,
    cfg_start: u32,
    cfg_len: u32,
}

impl Leases {
    fn new(cfg: &Config) -> Self {
        Leases {
            map: HashMap::new(),
            cfg_start: u32::from(cfg.pool_start),
            cfg_len: cfg.pool_len,
        }
    }

    /// Stable per-MAC assignment: the same phone gets the same address across reconnects, which
    /// keeps the iPhone from re-running discovery mid-session.
    fn assign(&mut self, mac: [u8; 6], lease: Duration) -> Ipv4Addr {
        if let Some((ip, _)) = self.map.get(&mac) {
            let ip = *ip;
            self.map.insert(mac, (ip, Instant::now() + lease));
            return ip;
        }
        let taken: Vec<Ipv4Addr> = self.map.values().map(|(ip, _)| *ip).collect();
        for n in 0..self.cfg_len {
            let cand = Ipv4Addr::from(self.cfg_start + n);
            if !taken.contains(&cand) {
                self.map.insert(mac, (cand, Instant::now() + lease));
                return cand;
            }
        }
        // Pool exhausted — reuse the oldest entry rather than refuse service.
        Ipv4Addr::from(self.cfg_start)
    }

    fn release(&mut self, mac: &[u8; 6]) {
        self.map.remove(mac);
    }
}

fn mac_str(m: &[u8; 6]) -> String {
    m.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Pin the socket to the AP interface with `SO_BINDTODEVICE`.
///
/// The Pi also has `eth0` (the host link) and `usb0` (the CCPA's NCM link); answering DHCP on
/// either would be actively harmful, so this is a safety property rather than a nicety.
///
/// The cfg names **both** `linux` and `android`: Rust reports `target_os = "android"` for
/// `aarch64-linux-android`, so a bare `cfg(target_os = "linux")` would silently skip this on the
/// very target it exists for. (That exact mistake cost real debugging time in `wireless/cloexec.rs`,
/// where it turned every `SOCK_CLOEXEC` into a no-op.) The fallback arm exists only so the crate
/// still compiles for `cargo test` on the macOS dev host, where this code never runs.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn bind_to_device(sock: &UdpSocket, iface: &str) {
    use std::os::unix::io::AsRawFd;
    let name = match std::ffi::CString::new(iface) {
        Ok(n) => n,
        Err(_) => {
            eprintln!("[apdhcpd] interface name {iface:?} contains a NUL — not binding");
            return;
        }
    };
    let rc = unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            name.as_ptr() as *const libc::c_void,
            (iface.len() + 1) as libc::socklen_t,
        )
    };
    if rc < 0 {
        eprintln!(
            "[apdhcpd] WARNING: SO_BINDTODEVICE({iface}) failed: {} — answering on ALL interfaces",
            std::io::Error::last_os_error()
        );
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn bind_to_device(_sock: &UdpSocket, _iface: &str) {
    // Host builds are for `cargo test` only; the daemon never runs here.
}

fn main() {
    let cfg = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[apdhcpd] {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    let sock = match UdpSocket::bind(("0.0.0.0", SERVER_PORT)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[apdhcpd] cannot bind 0.0.0.0:{SERVER_PORT}: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = sock.set_broadcast(true) {
        eprintln!("[apdhcpd] SO_BROADCAST failed: {e}");
        std::process::exit(1);
    }
    bind_to_device(&sock, &cfg.iface);

    eprintln!(
        "[apdhcpd] serving {} on {} — pool {}+{}, lease {}s, replies BROADCAST",
        cfg.server_ip, cfg.iface, cfg.pool_start, cfg.pool_len, cfg.lease_secs
    );

    let mut leases = Leases::new(&cfg);
    let lease = Duration::from_secs(u64::from(cfg.lease_secs));
    let bcast = (Ipv4Addr::BROADCAST, CLIENT_PORT);
    let mut buf = [0u8; 1500];

    loop {
        let n = match sock.recv_from(&mut buf) {
            Ok((n, _)) => n,
            Err(e) => {
                eprintln!("[apdhcpd] recv failed: {e}");
                continue;
            }
        };
        let req = match parse_request(&buf[..n]) {
            Some(r) => r,
            None => continue, // not a DHCP request we serve; stay quiet
        };
        if req.giaddr != Ipv4Addr::UNSPECIFIED {
            eprintln!("[apdhcpd] ignoring relayed request via {}", req.giaddr);
            continue;
        }

        let mac = mac_str(&req.chaddr);
        match req.msg_type {
            DHCPDISCOVER => {
                let ip = leases.assign(req.chaddr, lease);
                let reply = build_reply(&cfg, &req, DHCPOFFER, ip);
                match sock.send_to(&reply, bcast) {
                    Ok(_) => eprintln!("[apdhcpd] DISCOVER {mac} -> OFFER {ip} (broadcast)"),
                    Err(e) => eprintln!("[apdhcpd] OFFER to {mac} failed: {e}"),
                }
            }
            DHCPREQUEST => {
                // Honour the address the client asks for when it is ours to give; otherwise NAK so
                // it restarts discovery instead of silently using a stale address.
                let want = req.requested_ip.unwrap_or(req.ciaddr);
                let ours = leases.assign(req.chaddr, lease);
                let (ty, ip) = if want == Ipv4Addr::UNSPECIFIED || want == ours {
                    (DHCPACK, ours)
                } else {
                    (DHCPNAK, Ipv4Addr::UNSPECIFIED)
                };
                let reply = build_reply(&cfg, &req, ty, ip);
                match sock.send_to(&reply, bcast) {
                    Ok(_) => eprintln!(
                        "[apdhcpd] REQUEST {mac} want={want} -> {} {ip}",
                        msg_type_name(ty)
                    ),
                    Err(e) => eprintln!("[apdhcpd] reply to {mac} failed: {e}"),
                }
            }
            DHCPRELEASE | DHCPDECLINE => {
                leases.release(&req.chaddr);
                eprintln!("[apdhcpd] {} {mac}", msg_type_name(req.msg_type));
            }
            other => eprintln!("[apdhcpd] {mac} sent {} — ignored", msg_type_name(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discover(mac: [u8; 6]) -> Vec<u8> {
        let mut p = vec![0u8; BOOTP_FIXED_LEN];
        p[0] = BOOTREQUEST;
        p[1] = HTYPE_ETHERNET;
        p[2] = HLEN_ETHERNET;
        p[4..8].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        p[28..34].copy_from_slice(&mac);
        p.extend_from_slice(&MAGIC_COOKIE);
        p.extend_from_slice(&[OPT_MSG_TYPE, 1, DHCPDISCOVER]);
        p.extend_from_slice(&[OPT_PARAM_REQUEST, 3, OPT_SUBNET_MASK, OPT_ROUTER, OPT_DNS]);
        p.push(OPT_END);
        p
    }

    #[test]
    fn parses_a_discover() {
        let mac = [0xC2, 0xDC, 0xDD, 0x18, 0x7B, 0x24];
        let r = parse_request(&discover(mac)).expect("parses");
        assert_eq!(r.msg_type, DHCPDISCOVER);
        assert_eq!(r.chaddr, mac);
        assert_eq!(r.xid, [0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(r.param_request, vec![OPT_SUBNET_MASK, OPT_ROUTER, OPT_DNS]);
    }

    #[test]
    fn rejects_non_dhcp() {
        assert!(parse_request(&[]).is_none());
        assert!(parse_request(&[0u8; 300]).is_none(), "op=0 is not BOOTREQUEST");
        // Right shape, wrong magic cookie.
        let mut p = discover([1, 2, 3, 4, 5, 6]);
        p[BOOTP_FIXED_LEN] = 0x00;
        assert!(parse_request(&p).is_none());
    }

    /// A truncated option block must not panic or read out of bounds.
    #[test]
    fn survives_truncated_options() {
        let mut p = discover([1, 2, 3, 4, 5, 6]);
        p.truncate(BOOTP_FIXED_LEN + 4 + 2); // option code + length, no value
        assert!(parse_request(&p).is_none(), "no msg type -> not served");
    }

    #[test]
    fn offer_is_well_formed() {
        let cfg = Config::default();
        let req = parse_request(&discover([0xAA; 6])).unwrap();
        let reply = build_reply(&cfg, &req, DHCPOFFER, Ipv4Addr::new(192, 168, 43, 100));

        assert_eq!(reply[0], BOOTREPLY);
        assert_eq!(&reply[4..8], &req.xid, "xid must be echoed");
        assert_eq!(&reply[16..20], &[192, 168, 43, 100], "yiaddr");
        assert_eq!(&reply[28..34], &[0xAA; 6], "chaddr must be echoed");
        assert_eq!(&reply[BOOTP_FIXED_LEN..BOOTP_FIXED_LEN + 4], &MAGIC_COOKIE);
        assert!(reply.len() >= 300, "must meet the BOOTP minimum");

        let opts = &reply[BOOTP_FIXED_LEN + 4..];
        assert_eq!(&opts[0..3], &[OPT_MSG_TYPE, 1, DHCPOFFER]);
        // Every option a client needs to actually use the lease must be present.
        for code in [OPT_SERVER_ID, OPT_LEASE_TIME, OPT_SUBNET_MASK, OPT_ROUTER] {
            assert!(opts.contains(&code), "missing option {code}");
        }
    }

    #[test]
    fn nak_carries_no_lease_options() {
        let cfg = Config::default();
        let req = parse_request(&discover([0xBB; 6])).unwrap();
        let reply = build_reply(&cfg, &req, DHCPNAK, Ipv4Addr::UNSPECIFIED);
        let opts = &reply[BOOTP_FIXED_LEN + 4..];
        assert_eq!(&opts[0..3], &[OPT_MSG_TYPE, 1, DHCPNAK]);
        assert!(!opts.contains(&OPT_LEASE_TIME), "a NAK must not offer a lease");
    }

    /// The same phone must keep its address across reconnects, or iOS re-runs discovery mid-session.
    #[test]
    fn assignment_is_stable_per_mac() {
        let cfg = Config::default();
        let mut l = Leases::new(&cfg);
        let a = l.assign([1; 6], Duration::from_secs(60));
        let b = l.assign([2; 6], Duration::from_secs(60));
        assert_ne!(a, b, "different MACs get different addresses");
        assert_eq!(l.assign([1; 6], Duration::from_secs(60)), a, "stable");
        l.release(&[1; 6]);
        assert_eq!(l.assign([3; 6], Duration::from_secs(60)), a, "released ip reused");
    }
}
