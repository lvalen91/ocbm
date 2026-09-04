//! ocbm-host — host-side OCBM client (rusb/libusb). Claims the accessory interface and
//! speaks OCBM v1 over the discovered bulk endpoints.
//!   ocbm-host hello   [vid pid]                  handshake (HELLO -> HELLO_ACK)
//!   ocbm-host echo    [vid pid] [secs] [xfer]    ECHO round-trip throughput
//!   ocbm-host rtt     [vid pid] [secs]           ECHO round-trip LATENCY percentiles (per payload size)
//!   ocbm-host mfi     [vid pid]                  genuine MFi 2.0C cert + signature
//!   ocbm-host console [vid pid]                  MODE_SELECT CONSOLE -> root-shell bridge
//!   ocbm-host settime [vid pid]                  push host wall clock -> box (no RTC battery)
//!   ocbm-host push  [vid pid] <local> <remote> [mode-octal]   verified file deploy (default mode 755)
//!   ocbm-host pull  [vid pid] <remote> <local> [mode-octal]   verified file retrieval (box -> host)
//!   ocbm-host bridge [vid pid] [iface]         bridge the box netdev (default ncm0) over CH_ETH; decode frames
//!   ocbm-host setup-relay [vid pid] [secs] <cfg.yaml> [--author] [--mute-once]
//!                                               app-driven SETUP relay harness: subscribe with a YAML
//!                                               containing `accessoryConfig.appDrivenSetup: true` (see
//!                                               tools/setup_relay_cfg.yaml). Default (Stage 0): echo every
//!                                               relayed exchange's local response verbatim.
//!                                               --author (Stage 1, plan P2): AUTHOR each response host-side
//!                                               (setup_driver.rs), live-oracle diff it against the box's
//!                                               local response, and send the host-authored bytes.
//!                                               --mute-once: drop the FIRST RS_REQ (author/send nothing) to
//!                                               exercise the box's timeout → sticky-local fallback.
//! Every connection also auto-pushes the host clock right after HELLO.

mod setup_driver;

use ocbm_proto as p;
use rusb::{Context, Device, DeviceHandle, Direction, TransferType, UsbContext};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

struct Link {
    h: DeviceHandle<Context>,
    ep_in: u8,
    ep_out: u8,
    seq: u32,
    reasm: p::Reassembler,
}

impl Link {
    fn send(&mut self, ch: u16, flags: u8, pl: &[u8]) -> bool {
        let mut buf = vec![0u8; p::HDR_LEN + pl.len()];
        let n = match p::try_frame(&mut buf, ch, flags, self.seq, pl) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("[ocbm] refusing oversized frame on ch {ch}: {} B > {} B max", e.len, e.max);
                return false; // seq NOT advanced: no frame went on the wire
            }
        };
        self.seq = self.seq.wrapping_add(1);
        self.h
            .write_bulk(self.ep_out, &buf[..n], Duration::from_secs(2))
            .map(|w| w == n)
            .unwrap_or(false)
    }
    fn recv(&mut self, out: &mut [u8], deadline: Instant) -> Option<(u16, u8, usize)> {
        loop {
            if let Some(x) = self.reasm.next(out) {
                return Some(x);
            }
            if Instant::now() >= deadline {
                return None;
            }
            let mut tmp = [0u8; 16384];
            if let Ok(n) = self
                .h
                .read_bulk(self.ep_in, &mut tmp, Duration::from_millis(100))
            {
                if n > 0 {
                    self.reasm.push(&tmp[..n]);
                }
            }
        }
    }
}

fn hex8(b: &[u8]) -> String {
    b.iter().take(8).map(|x| format!("{:02x}", x)).collect()
}

fn open(vid: u16, pid: u16) -> Link {
    let ctx = Context::new().expect("libusb init");
    let devs = ctx.devices().expect("device list");
    let dev: Device<Context> = devs
        .iter()
        .find(|d| {
            d.device_descriptor()
                .map(|dd| dd.vendor_id() == vid && (pid == 0 || dd.product_id() == pid))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| {
            eprintln!("device {:04x} not found", vid);
            std::process::exit(1);
        });

    // Pick the interface that actually carries the bulk IN+OUT pair (not always IF0).
    let (mut ifnum, mut ep_in, mut ep_out) = (0u8, 0x81u8, 0x01u8);
    if let Ok(cfg) = dev.active_config_descriptor() {
        for iface in cfg.interfaces() {
            for desc in iface.descriptors() {
                let (mut bin, mut bout) = (None, None);
                for ep in desc.endpoint_descriptors() {
                    if ep.transfer_type() == TransferType::Bulk {
                        match ep.direction() {
                            Direction::In => bin = Some(ep.address()),
                            Direction::Out => bout = Some(ep.address()),
                        }
                    }
                }
                if let (Some(i), Some(o)) = (bin, bout) {
                    ifnum = desc.interface_number();
                    ep_in = i;
                    ep_out = o;
                }
            }
        }
    }

    let h = dev.open().unwrap_or_else(|e| {
        eprintln!("open: {}", e);
        std::process::exit(1);
    });
    let _ = h.set_auto_detach_kernel_driver(true);
    h.claim_interface(ifnum).unwrap_or_else(|e| {
        eprintln!("claim IF{}: {}", ifnum, e);
        std::process::exit(1);
    });
    eprintln!(
        "[ocbm] IF{} bulk IN=0x{:02x} OUT=0x{:02x}",
        ifnum, ep_in, ep_out
    );
    Link {
        h,
        ep_in,
        ep_out,
        seq: 0,
        reasm: p::Reassembler::new(),
    }
}

/// This process's HOST INSTANCE NONCE — `CT_HELLO` bytes 2..6 (`ocbm_proto::CT_HELLO`,
/// docs/carplay/01_OCBM_PROTOCOL.md). The box compares it across reattaches to tell a relaunched
/// host from the same host reconnecting, so it must be stable for the life of the process and
/// differ between runs; 0 means "not supplied", hence the `max(1)`. It is NOT a capability word —
/// there is no host->box capability field, and sending one here (this used to send `0x13`) made
/// every run look like the same host, so the box could never latch `host_replaced`.
fn host_instance_nonce() -> u32 {
    static NONCE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *NONCE.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        ((std::process::id() as u64 ^ nanos) as u32).max(1)
    })
}

fn do_hello(link: &mut Link) {
    let mut hi = [0u8; 6];
    hi[0] = p::CT_HELLO;
    hi[1] = p::VERSION;
    hi[2..6].copy_from_slice(&host_instance_nonce().to_le_bytes());
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &hi);
    // REATTACH RESYNC: a prior session may have left kernel-buffered A/V/bulk frames ahead of our
    // HELLO_ACK that the box cannot retract. Drain frames until the ACK (or timeout) rather than
    // giving up on the first — the box also clears its software queues on HELLO. `out` is full-size
    // so a stale A/V frame doesn't truncate. Re-send HELLO once mid-wait in case the first was lost.
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let deadline = Instant::now() + Duration::from_secs(3);
    let resend_at = Instant::now() + Duration::from_millis(1500);
    let mut stale = 0u32;
    let mut resent = false;
    while Instant::now() < deadline {
        // Bound each recv by the NEXT actionable moment (the resend, then the final deadline) rather
        // than the full deadline: `link.recv` blocks until ITS deadline when the link is silent, so a
        // single recv(&out, deadline) on a truly silent link (the lost-first-HELLO case) would consume
        // the whole 3 s and the mid-wait HELLO resend below would never fire — the exact case it exists
        // for (audit Fix #13). Slice to resend_at first, then to deadline; the resend runs after the
        // (short) recv returns.
        let slice_deadline = if resent { deadline } else { resend_at.min(deadline) };
        if let Some((ch, _, l)) = link.recv(&mut out, slice_deadline) {
            if ch == p::CH_CTRL && l >= 6 && out[0] == p::CT_HELLO_ACK {
                let caps = u32::from_le_bytes(out[2..6].try_into().unwrap());
                if stale > 0 {
                    eprintln!("[ocbm] drained {stale} stale frame(s) before HELLO_ACK (reattach)");
                }
                println!(
                    "HELLO_ACK: box v{} caps=0x{:08x} mode={}",
                    out[1],
                    caps,
                    if l >= 7 { out[6] } else { 0 }
                );
                return;
            }
            stale += 1; // leftover frame from a prior session — keep draining toward the ACK
        }
        if !resent && Instant::now() >= resend_at {
            link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &hi);
            resent = true;
        }
    }
    eprintln!("no HELLO_ACK");
}

/// Push the host wall clock to the box (CT_SETTIME) and consume the box's ack.
/// The box has no RTC battery, so its clock is bogus at boot; a valid clock is
/// required for TLS/AirPlay pairing. Best-effort: a box without the handler just
/// won't ack (times out fast). Returns true on a confirmed apply.
fn push_time(link: &mut Link) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut req = vec![p::CT_SETTIME];
    req.extend_from_slice(&now.to_le_bytes());
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &req);
    // Full-size buffer (the reassembler contract) + loop until the ack, filtering CH_CTRL/CT_SETTIME:
    // a leftover stale frame from reattach must not be mistaken for "no ack".
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let deadline = Instant::now() + Duration::from_secs(1);
    while let Some((ch, _, l)) = link.recv(&mut out, deadline) {
        if ch == p::CH_CTRL && l >= 10 && out[0] == p::CT_SETTIME {
            let t = u64::from_le_bytes(out[1..9].try_into().unwrap());
            if out[9] == 0 {
                eprintln!("[ocbm] box clock set to {} (unix)", t);
                return true;
            }
            eprintln!("[ocbm] box rejected clock set (settimeofday failed)");
            return false;
        }
        // else: a stale/other frame — keep draining until the deadline
    }
    eprintln!("[ocbm] no CT_SETTIME ack (box handler absent?)");
    false
}

fn mode_echo(link: &mut Link, secs: u64, xfer: usize) {
    let xfer = xfer.min(p::MAX_PAYLOAD);
    let pl = vec![0xA5u8; xfer];
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let (mut bytes, mut mism) = (0u64, 0u64);
    let t0 = Instant::now();
    let end = t0 + Duration::from_secs(secs);
    while Instant::now() < end {
        if !link.send(p::CH_ECHO, p::F_SOM | p::F_EOM, &pl) {
            eprintln!("send fail");
            break;
        }
        match link.recv(&mut out, Instant::now() + Duration::from_secs(2)) {
            Some((ch, _, l)) if ch == p::CH_ECHO && l == xfer && out[..l] == pl[..] => {
                bytes += xfer as u64
            }
            _ => mism += 1,
        }
    }
    let dt = t0.elapsed().as_secs_f64();
    println!(
        "ECHO round-trip: {} bytes in {:.2}s => {:.1} Mbps (payload each way), mismatches={}",
        bytes,
        dt,
        bytes as f64 * 8.0 * 2.0 / 1e6 / dt,
        mism
    );
}

/// CH_ECHO round-trip LATENCY (the docs/ops/04_OPEN_ITEMS.md SETUP-relay gate item — no RTT number existed anywhere;
/// `mode_echo` times only aggregate throughput, never an individual iteration). Serialized ping-pong
/// with per-iteration timing; reports p50/p90/p99/max + timeouts per payload size.
///
/// Interpretation bound: CH_ECHO rides ocbmd's lowest-priority out_lo queue, while the SETUP relay
/// (CH_RTSP) will ride out_hi, which drains first — so these numbers are an UPPER bound on the relay's
/// transport latency. A pass here is a strong pass; do not treat a marginal number as a relay fail
/// without an out_hi measurement.
fn mode_rtt(link: &mut Link, secs: u64) {
    // 64 B ≈ the marker-only floor; 4 KiB ≈ a phase-1 SETUP plist; 16 KiB ≈ a large phase-2/streams
    // exchange; MAX_PAYLOAD = the single-frame ceiling (a bigger relay message spans OCBM frames).
    let sizes: [usize; 4] = [64, 4096, 16384, p::MAX_PAYLOAD];
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    println!("RTT over CH_ECHO — {secs} s per payload size (serialized; echo rides out_lo → upper bound)");
    for &sz in &sizes {
        let mut pl = vec![0xA5u8; sz];
        let mut samples: Vec<u64> = Vec::new();
        let mut timeouts = 0u64;
        let end = Instant::now() + Duration::from_secs(secs);
        let mut ping: u32 = 0;
        while Instant::now() < end {
            // Tag each ping so a stale echo (reattach leftovers, a late reply after a timeout) can
            // never be credited to the wrong iteration — the same drain-until-ours discipline as
            // do_hello/push_time.
            ping = ping.wrapping_add(1);
            pl[..4].copy_from_slice(&ping.to_le_bytes());
            let t = Instant::now();
            if !link.send(p::CH_ECHO, p::F_SOM | p::F_EOM, &pl) {
                eprintln!("[rtt] send fail at {sz} B — aborting this size");
                break;
            }
            let deadline = t + Duration::from_secs(2);
            let mut got = false;
            while let Some((ch, _, l)) = link.recv(&mut out, deadline) {
                if ch == p::CH_ECHO && l == sz && out[..4] == pl[..4] {
                    samples.push(t.elapsed().as_micros() as u64);
                    got = true;
                    break;
                }
                // else: stale/foreign frame — keep draining toward our echo
            }
            if !got {
                timeouts += 1;
            }
        }
        samples.sort_unstable();
        if samples.is_empty() {
            println!("  {sz:>6} B: NO samples (timeouts={timeouts})");
            continue;
        }
        // Nearest-rank on the sorted samples; n is small enough that exact interpolation adds nothing.
        let pct = |p: f64| samples[((samples.len() - 1) as f64 * p).round() as usize] as f64 / 1000.0;
        println!(
            "  {sz:>6} B: n={:<6} p50={:.2} ms  p90={:.2} ms  p99={:.2} ms  max={:.2} ms  timeouts={timeouts}",
            samples.len(),
            pct(0.50),
            pct(0.90),
            pct(0.99),
            *samples.last().unwrap() as f64 / 1000.0,
        );
    }
    println!("gate (plan): PASS p99 ≤ 50 ms under A/V load · MARGINAL 50–250 ms · FAIL > 250 ms or 1 s stalls");
}

fn mode_mfi(link: &mut Link) {
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    link.send(p::CH_MFI, p::F_SOM | p::F_EOM, &[0x01, 0, 0]); // CopyCertificate
    match link.recv(&mut out, Instant::now() + Duration::from_secs(3)) {
        Some((ch, _, l)) if ch == p::CH_MFI && l >= 3 => {
            let (st, n) = (out[0], ((out[1] as usize) << 8) | out[2] as usize);
            if st == 0 {
                println!("MFI cert: {} bytes  first8={}", n, hex8(&out[3..]));
                // Dump the DER so the certificate's SERIAL NUMBER can be read off it. That serial
                // is not a curiosity: iOS 27 keys the CarPlay vehicle record on it
                // (CRVehicle.certificateSerialNumber, CRVehicleAccessoryManager.vehiclesBySerialNumber,
                // CARSession._MFiCertificateSerialNumber), so two adapters presenting the same MFi
                // cert serial collapse into ONE entry in Settings > General > CarPlay, with the name
                // following whichever connected last. Reading it is the only way to tell a genuine
                // per-chip provisioning from a shared/cloned one.
                //   openssl pkcs7 -inform DER -in mfi_cert.der -print_certs | openssl x509 -noout -serial
                if let Some(path) = std::env::var_os("OCBM_MFI_CERT_OUT") {
                    // Bound by the RECEIVED payload `l`, not the reusable 64 KiB buffer: a reply
                    // whose declared `n` overran `l` would append stale bytes to the DER.
                    match std::fs::write(&path, &out[3..3 + n.min(l - 3)]) {
                        Ok(()) => println!("MFI cert written to {}", path.to_string_lossy()),
                        Err(e) => eprintln!("MFI cert write failed: {e}"),
                    }
                }
            } else {
                println!("MFI cert error status={}", st);
            }
        }
        _ => println!("MFI cert: no response"),
    }
    let mut sreq = vec![0x02u8, 0, 20];
    sreq.extend_from_slice(&[0x5Au8; 20]); // 20-byte digest
    link.send(p::CH_MFI, p::F_SOM | p::F_EOM, &sreq);
    match link.recv(&mut out, Instant::now() + Duration::from_secs(5)) {
        Some((ch, _, l)) if ch == p::CH_MFI && l >= 3 => {
            let (st, n) = (out[0], ((out[1] as usize) << 8) | out[2] as usize);
            if st == 0 {
                println!("MFI sign: {}-byte signature  first8={}", n, hex8(&out[3..]));
            } else {
                println!("MFI sign error status={}", st);
            }
        }
        _ => println!("MFI sign: no response"),
    }
}

fn mode_console(link: &mut Link) {
    link.send(
        p::CH_CTRL,
        p::F_SOM | p::F_EOM,
        &[p::CT_MODE_SELECT, p::MODE_CONSOLE],
    );
    let orig = unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        libc::tcgetattr(0, &mut t);
        t
    };
    let mut raw = orig;
    unsafe {
        libc::cfmakeraw(&mut raw);
        libc::tcsetattr(0, libc::TCSANOW, &raw);
    }
    eprintln!("[ocbm-rescue] CONSOLE bridged (Ctrl-] to quit)");
    // Reassembler::next requires out.len() >= MAX_PAYLOAD (it debug_asserts, and in release drops
    // any frame whose plen exceeds out.len()). Every other caller uses p::MAX_PAYLOAD; this was the
    // lone 16 KiB outlier, which would panic in debug or silently drop CONSOLE frames > 16 KiB.
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    'outer: loop {
        let mut fds = [libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        }];
        if unsafe { libc::poll(fds.as_mut_ptr(), 1, 5) } > 0 && (fds[0].revents & libc::POLLIN) != 0
        {
            let mut inb = [0u8; 1024];
            let k = unsafe { libc::read(0, inb.as_mut_ptr() as *mut libc::c_void, inb.len()) };
            if k <= 0 {
                break;
            }
            let k = k as usize;
            if inb[..k].contains(&0x1d) {
                break 'outer; // Ctrl-]
            }
            link.send(p::CH_CONSOLE, p::F_SOM | p::F_EOM, &inb[..k]);
        }
        if let Some((ch, _, l)) = link.recv(&mut out, Instant::now() + Duration::from_millis(15)) {
            if ch == p::CH_CONSOLE {
                use std::io::Write;
                let _ = std::io::stdout().write_all(&out[..l]);
                let _ = std::io::stdout().flush();
            }
        }
    }
    unsafe { libc::tcsetattr(0, libc::TCSANOW, &orig) };
}

/// IP stream channel: OPEN a connection to a box-side target and print what it returns.
/// (First-cut validation; a full local-listener `forward` mode builds on this.)
fn mode_ip(link: &mut Link, target: &str, payload: Option<&str>) {
    // "udp:host:port" opens a datagram conn; otherwise TCP.
    let (op, tgt) = match target.strip_prefix("udp:") {
        Some(t) => (p::IP_OPEN_UDP, t),
        None => (p::IP_OPEN, target),
    };
    let mut pl = vec![op];
    pl.extend_from_slice(&1u16.to_le_bytes());
    pl.extend_from_slice(tgt.as_bytes());
    link.send(p::CH_IP, p::F_SOM | p::F_EOM, &pl);
    eprintln!(
        "[ip] OPEN({}) conn 1 -> {}",
        if op == p::IP_OPEN_UDP { "udp" } else { "tcp" },
        tgt
    );
    if let Some(msg) = payload {
        let mut d = vec![p::IP_DATA];
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(msg.as_bytes());
        link.send(p::CH_IP, p::F_SOM | p::F_EOM, &d);
        eprintln!("[ip] sent {} bytes", msg.len());
    }
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut got = 0usize;
    while Instant::now() < deadline {
        if let Some((ch, _, l)) = link.recv(&mut out, Instant::now() + Duration::from_millis(500)) {
            if ch == p::CH_IP && l >= 3 {
                match out[0] {
                    p::IP_DATA => {
                        got += l - 3;
                        print!("{}", String::from_utf8_lossy(&out[3..l]));
                    }
                    p::IP_CLOSE => {
                        eprintln!("[ip] conn closed by box");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    eprintln!("\n[ip] {} bytes received from {}", got, target);
}

/// Read one FILE_ACK (status, crc, size) within `secs`, ignoring other channel traffic.
fn recv_file_ack(link: &mut Link, secs: u64) -> Option<(u8, u32, u32)> {
    let mut out = vec![0u8; p::MAX_PAYLOAD]; // full-size per the reassembler contract
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if let Some((ch, _, l)) = link.recv(&mut out, deadline) {
            if ch == p::CH_FILE && l >= 10 && out[0] == p::FILE_ACK {
                return Some((
                    out[1],
                    u32::from_le_bytes(out[2..6].try_into().unwrap()),
                    u32::from_le_bytes(out[6..10].try_into().unwrap()),
                ));
            }
        }
    }
    None
}

/// Pull a file FROM the box over CH_FILE (the mirror of push): send FILE_PULL(path), then read the
/// FILE_DATA sub-frames the box streams back until the terminal FILE_ACK, reassemble, verify the
/// end-to-end CRC-32 + size, and write `local`. Returns true on success. `mode` is applied to the
/// written file. A stalled/absent box CH_FILE handler → timeout → false (nothing written).
fn mode_pull(link: &mut Link, remote: &str, local: &str, mode: u32) -> bool {
    let mut req = vec![p::FILE_PULL];
    req.extend_from_slice(remote.as_bytes());
    if !link.send(p::CH_FILE, p::F_SOM | p::F_EOM, &req) {
        eprintln!("[pull] request send failed");
        return false;
    }
    let t0 = Instant::now();
    let mut data: Vec<u8> = Vec::new();
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    // Overall deadline is generous (a large binary over the console-shared link); each recv waits until
    // it, and a terminal FILE_ACK ends the loop early on success or error.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match link.recv(&mut out, deadline) {
            Some((ch, _, l)) if ch == p::CH_FILE && l >= 1 => match out[0] {
                p::FILE_DATA => data.extend_from_slice(&out[1..l]),
                p::FILE_ACK if l >= 10 => {
                    let status = out[1];
                    let rcrc = u32::from_le_bytes(out[2..6].try_into().unwrap());
                    let rsize = u32::from_le_bytes(out[6..10].try_into().unwrap());
                    if status != p::FILE_OK {
                        eprintln!("[pull] box refused (status {status}) — bad path / not found?");
                        return false;
                    }
                    let crc = p::crc32(&data);
                    if crc != rcrc || data.len() as u32 != rsize {
                        eprintln!(
                            "[pull] VERIFY FAILED: got crc=0x{:08x} size={} vs box crc=0x{:08x} size={}",
                            crc, data.len(), rcrc, rsize
                        );
                        return false;
                    }
                    if let Err(e) = std::fs::write(local, &data) {
                        eprintln!("[pull] write {local}: {e}");
                        return false;
                    }
                    let _ = std::fs::set_permissions(local, std::fs::Permissions::from_mode(mode));
                    let dt = t0.elapsed().as_secs_f64();
                    println!(
                        "[pull] {} -> {} : {} bytes, crc32=0x{:08x}, mode={:o}, {:.2}s ({:.1} Mbps)",
                        remote, local, rsize, rcrc, mode, dt,
                        rsize as f64 * 8.0 / 1e6 / dt.max(1e-6)
                    );
                    return true;
                }
                _ => {}
            },
            Some(_) => {} // other channel traffic — ignore
            None => {
                eprintln!("[pull] timeout ({} bytes received)", data.len());
                return false;
            }
        }
    }
}

/// Push a local file to the box over CH_FILE: OPEN -> stream DATA -> CLOSE(crc,size).
/// The box verifies the end-to-end CRC-32, chmods to `mode`, and atomically renames into
/// place — a reliable replacement for base64-over-UART deploys. Returns true on success.
fn mode_push(link: &mut Link, local: &str, remote: &str, mode: u32) -> bool {
    let data = match std::fs::read(local) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[push] read {}: {}", local, e);
            return false;
        }
    };
    let crc = p::crc32(&data);
    let size = data.len() as u32;

    // OPEN
    let mut open = vec![p::FILE_OPEN];
    open.extend_from_slice(&mode.to_le_bytes());
    open.extend_from_slice(remote.as_bytes());
    link.send(p::CH_FILE, p::F_SOM | p::F_EOM, &open);
    match recv_file_ack(link, 3) {
        Some((p::FILE_OK, _, _)) => {}
        Some((st, _, _)) => {
            eprintln!("[push] OPEN rejected (status {})", st);
            return false;
        }
        None => {
            eprintln!("[push] no OPEN ack (box CH_FILE handler absent?)");
            return false;
        }
    }

    // DATA — one sub-frame per chunk; type byte + chunk must fit one OCBM payload.
    let t0 = Instant::now();
    let chunk = p::MAX_PAYLOAD - 1;
    for part in data.chunks(chunk) {
        let mut d = Vec::with_capacity(1 + part.len());
        d.push(p::FILE_DATA);
        d.extend_from_slice(part);
        if !link.send(p::CH_FILE, p::F_SOM | p::F_EOM, &d) {
            eprintln!("[push] DATA send failed");
            return false;
        }
    }

    // CLOSE — box verifies crc+size, chmods, renames.
    let mut close = vec![p::FILE_CLOSE];
    close.extend_from_slice(&crc.to_le_bytes());
    close.extend_from_slice(&size.to_le_bytes());
    link.send(p::CH_FILE, p::F_SOM | p::F_EOM, &close);
    match recv_file_ack(link, 5) {
        Some((p::FILE_OK, rcrc, rsize)) => {
            let dt = t0.elapsed().as_secs_f64();
            println!(
                "[push] {} -> {} : {} bytes, crc32=0x{:08x}, mode={:o}, {:.2}s ({:.1} Mbps)",
                local,
                remote,
                rsize,
                rcrc,
                mode,
                dt,
                size as f64 * 8.0 / 1e6 / dt.max(1e-6)
            );
            true
        }
        Some((p::FILE_ERR_VERIFY, rcrc, rsize)) => {
            eprintln!(
                "[push] VERIFY FAILED: box got crc=0x{:08x} size={} vs host crc=0x{:08x} size={}",
                rcrc, rsize, crc, size
            );
            false
        }
        Some((st, _, _)) => {
            eprintln!("[push] CLOSE failed (status {})", st);
            false
        }
        None => {
            eprintln!("[push] no CLOSE ack");
            false
        }
    }
}

/// Decode a bridged ethernet frame into a one-line summary — enough to confirm the iPhone's
/// wired-CarPlay discovery (link-local IPv6: NDP/RS/RA + mDNS) is traversing OCBM to the Mac.
fn describe_frame(f: &[u8], n: u64) -> String {
    if f.len() < 14 {
        return format!("#{n} runt {}B", f.len());
    }
    let ethertype = ((f[12] as u16) << 8) | f[13] as u16;
    let mac = |b: &[u8]| {
        b.iter()
            .map(|x| format!("{:02x}", x))
            .collect::<Vec<_>>()
            .join(":")
    };
    let l2 = format!("{}->{}", mac(&f[6..12]), mac(&f[0..6]));
    match ethertype {
        0x86DD if f.len() >= 54 => {
            let ip = &f[14..];
            let nh = ip[6];
            let v6 = |b: &[u8]| {
                let mut s = String::new();
                for i in (0..16).step_by(2) {
                    if i > 0 {
                        s.push(':');
                    }
                    s.push_str(&format!("{:x}", ((b[i] as u16) << 8) | b[i + 1] as u16));
                }
                s
            };
            let (src, dst) = (v6(&ip[8..24]), v6(&ip[24..40]));
            let kind = match nh {
                58 => "ICMPv6/NDP".to_string(),
                // Require the 4-byte L4 header (ports at ip[40..44]) before reading it — the outer
                // guard only ensures ip.len() >= 40, so an IPv6 frame of length 54..57 with nh 17/6
                // would otherwise index out of bounds and panic on wire-controlled bridged input.
                17 if ip.len() >= 44 => {
                    let udp = &ip[40..];
                    let dport = ((udp[2] as u16) << 8) | udp[3] as u16;
                    if dport == 5353 {
                        "mDNS".to_string()
                    } else {
                        format!("UDP:{dport}")
                    }
                }
                6 if ip.len() >= 44 => {
                    let tcp = &ip[40..];
                    let dport = ((tcp[2] as u16) << 8) | tcp[3] as u16;
                    format!("TCP:{dport}")
                }
                x => format!("nh{x}"),
            };
            format!("#{n} IPv6 {kind}  {src} -> {dst}  [{l2}] {}B", f.len())
        }
        0x0800 => format!("#{n} IPv4  [{l2}] {}B", f.len()),
        t => format!("#{n} ethertype 0x{t:04x}  [{l2}] {}B", f.len()),
    }
}

/// `bridge` (observe): tell the box to bridge ncm0 onto CH_ETH, then decode inbound frames.
/// This is the step-1 verification that the iPhone's link-local traffic reaches the Mac over OCBM.
/// (The feth+BPF injection that lets receiver_core bind fe80:: on it builds on this.)
fn mode_bridge(link: &mut Link, iface: &str, secs: u64) {
    let mut req = vec![p::CT_ETH_START];
    req.extend_from_slice(iface.as_bytes());
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &req);
    let deadline = if secs > 0 {
        Some(Instant::now() + Duration::from_secs(secs))
    } else {
        None
    };
    eprintln!(
        "[bridge] CT_ETH_START({iface}) sent — observing bridged frames{}",
        if secs > 0 {
            format!(" for {secs}s")
        } else {
            " (forever)".into()
        }
    );
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let (mut nframes, mut nbytes) = (0u64, 0u64);
    let mut last = Instant::now();
    loop {
        if let Some(d) = deadline {
            if Instant::now() >= d {
                break;
            }
        }
        if let Some((ch, _, l)) = link.recv(&mut out, Instant::now() + Duration::from_millis(300)) {
            if ch == p::CH_ETH {
                nframes += 1;
                nbytes += l as u64;
                if nframes <= 60 || nframes % 50 == 0 {
                    println!("{}", describe_frame(&out[..l], nframes));
                }
            }
        }
        if last.elapsed() >= Duration::from_secs(2) {
            eprintln!("[bridge] {nframes} frames / {nbytes} bytes so far");
            last = Instant::now();
        }
    }
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_ETH_STOP]);
    println!("[bridge] CT_ETH_STOP sent — total {nframes} frames / {nbytes} bytes");
}

/// `session` — act as the live host receiver for the lifecycle (docs/carplay/02_SESSION_LIFECYCLE.md): SUBSCRIBE (+ push an optional
/// YAML config file), HEARTBEAT ~1/s to prove liveness, print the box's SESSION_EVENTs, and STOP cleanly
/// on exit. Killing this process instead (no STOP) simulates a host crash → the box's heartbeat watchdog
/// declares the host gone after the grace. Drives the box's presence state machine end to end.
fn mode_session(link: &mut Link, secs: u64, cfg_path: Option<&str>) {
    let mut sub = vec![p::CT_SUBSCRIBE];
    match cfg_path.and_then(|p| std::fs::read(p).ok()) {
        Some(bytes) => {
            eprintln!(
                "[session] SUBSCRIBE with {} B config from {}",
                bytes.len(),
                cfg_path.unwrap()
            );
            sub.extend_from_slice(&bytes);
        }
        None => eprintln!("[session] SUBSCRIBE (no config)"),
    }
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &sub);
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let deadline = if secs > 0 {
        Some(Instant::now() + Duration::from_secs(secs))
    } else {
        None
    };
    let mut next_hb = Instant::now() + Duration::from_secs(1);
    loop {
        if let Some(d) = deadline {
            if Instant::now() >= d {
                break;
            }
        }
        // drain any SESSION_EVENTs (short wait so heartbeats stay ~1/s)
        if let Some((ch, _, l)) = link.recv(&mut out, Instant::now() + Duration::from_millis(250)) {
            if ch == p::CH_CTRL && l >= 2 && out[0] == p::CT_SESSION_EVENT {
                let ev = match out[1] {
                    x if x == p::SEV_HOST_PRESENT => "HOST_PRESENT",
                    x if x == p::SEV_HOST_GONE => "HOST_GONE",
                    _ => "?",
                };
                println!("[session] box event: {ev}");
            }
        }
        if Instant::now() >= next_hb {
            link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_HEARTBEAT]);
            next_hb = Instant::now() + Duration::from_secs(1);
        }
    }
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_STOP]);
    println!("[session] STOP sent — clean exit");
}

/// `av` — receive the box's A/V over OCBM (CH_VIDEO / CH_MEDIA_AUDIO), count it, and save the video
/// stream to a file. Validates the box-session -> ocbmd -> OCBM -> host A/V path with a live iPhone.
fn mode_av(link: &mut Link, secs: u64) {
    eprintln!(
        "[av] receiving CH_VIDEO/CH_MEDIA_AUDIO{} — saving video to /tmp/carplay_video.bin",
        if secs > 0 {
            format!(" for {secs}s")
        } else {
            " (forever)".into()
        }
    );
    use std::io::Write;
    let mut vfile = std::fs::File::create("/tmp/carplay_video.bin").ok();
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let (mut vb, mut ab, mut vf, mut af) = (0u64, 0u64, 0u64, 0u64);
    let deadline = if secs > 0 {
        Some(Instant::now() + Duration::from_secs(secs))
    } else {
        None
    };
    let mut last = Instant::now();
    loop {
        if let Some(d) = deadline {
            if Instant::now() >= d {
                break;
            }
        }
        if let Some((ch, _, l)) = link.recv(&mut out, Instant::now() + Duration::from_millis(300)) {
            match ch {
                p::CH_VIDEO => {
                    vf += 1;
                    vb += l as u64;
                    if let Some(f) = vfile.as_mut() {
                        let _ = f.write_all(&out[..l]);
                    }
                }
                p::CH_MEDIA_AUDIO => {
                    af += 1;
                    ab += l as u64;
                }
                _ => {}
            }
        }
        if last.elapsed() >= Duration::from_secs(2) {
            eprintln!("[av] video {vf} frm / {vb} B · audio {af} frm / {ab} B");
            last = Instant::now();
        }
    }
    println!("[av] total — video: {vf} frames / {vb} bytes (→ /tmp/carplay_video.bin) · audio: {af} frames / {ab} bytes");
}

/// `avdec` — COMMITTED-MODEL validation. The box runs with `OCBM_FWD_ENC=1`, so it forwards the
/// *encrypted* video + hands the per-stream key (NO on-box decrypt). This mode reassembles the seam
/// byte stream from CH_VIDEO, takes the handed key, and decrypts on the HOST — proving the committed
/// "forward encrypted + hand the session key" path end to end.
///
/// Wire (each message = `[u32 BE len][payload]`, reassembled across CH_VIDEO frames):
///   key    payload: `[SEAM_MAGIC "SEAV"][0x00][key.output 32B][scid 8B LE]`
///   frame  payload: `[SEAM_MAGIC "SEAV"][0x01][seq u64 LE][hdr 128B][body]`  — opcode = hdr[4]: 0 = encrypted VideoFrame, 1 = plaintext avcC.
/// Per VideoFrame the host reconstructs `nonce = [0,0,0,0] ++ seq_le64` from the WIRE seq (#678 — a
/// local counter desyncs permanently at the first gated/dropped frame), `aad = hdr`, `msg = body`.
/// The box's screen-seam resync marker ("SEAV"), byte-for-byte the receiver's `SEAM_MAGIC`. Every
/// forwarded screen message on CH_VIDEO/CH_ALT_VIDEO begins with it.
const SEAM_MAGIC: [u8; 4] = *b"SEAV";
/// Per-frame length ceiling for the screen seam (#651). Large enough for any single 4K IDR the box
/// forwards as one length-prefixed message, small enough to reject a garbled length prefix.
const SEAM_VID_MAX: usize = 4 * 1024 * 1024;

fn mode_avdec(link: &mut Link, secs: u64, cfg_path: Option<&str>, rtt: bool) {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
    use std::io::Write;
    eprintln!("[avdec] full host receiver — SUBSCRIBE + heartbeat + decrypt box A/V, then STOP");
    // Act as the live host receiver: SUBSCRIBE (arms the box session via the supervisor), HEARTBEAT to
    // stay present, drain + decrypt CH_VIDEO/CH_MEDIA_AUDIO, then STOP. One USB claim does the whole loop.
    let mut sub = vec![p::CT_SUBSCRIBE];
    if let Some(bytes) = cfg_path.and_then(|pp| std::fs::read(pp).ok()) {
        eprintln!("[avdec] SUBSCRIBE with {} B config", bytes.len());
        sub.extend_from_slice(&bytes);
    }
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &sub);
    let mut next_hb = Instant::now() + Duration::from_secs(1);
    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let mut buf: Vec<u8> = Vec::new(); // video seam reassembly
    let mut cipher: Option<ChaCha20Poly1305> = None;
    let (mut ok, mut fail, mut cfg) = (0u64, 0u64, 0u64);
    let mut plain_bytes = 0u64;
    let mut dec = std::fs::File::create("/tmp/carplay_dec.h264").ok();
    // Audio (CH_MEDIA_AUDIO): same seam framing; each 0x01 payload is a raw RTP packet. Per-packet the
    // nonce is IN the packet (no counter): nonce = [0,0,0,0] ++ payload trailing 8B, aad = pkt[4..12],
    // ct‖tag = payload[..len-8]  (payload = pkt[12..]). Key handed on the 0x00 message like video.
    let mut abuf: Vec<u8> = Vec::new();
    // Seam v2: every audio message is scid-tagged (media :9002 AND voice :9003 → CH_ALT_AUDIO share
    // the framing), so keys live in a per-scid map — concurrent streams can't clobber each other.
    let mut aciphers: std::collections::HashMap<u64, ChaCha20Poly1305> =
        std::collections::HashMap::new();
    let (mut aok, mut afail) = (0u64, 0u64);
    let deadline = if secs > 0 {
        Some(Instant::now() + Duration::from_secs(secs))
    } else {
        None
    };
    let mut last = Instant::now();
    // --rtt: interleave a CH_ECHO ping every ~100 ms DURING the live session. This is the
    // under-A/V-load RTT the SETUP-relay gate actually cares about (docs/ops/04_OPEN_ITEMS.md); it must run in-process
    // because the bulk pipe cannot be shared with a second measuring process. Pings are id-tagged and
    // matched via a pending map — under load replies can arrive late/out of order, and a serialized
    // wait would stall the A/V drain this mode exists to keep honest.
    let mut ping_at = Instant::now() + Duration::from_millis(100);
    let mut pings: std::collections::HashMap<u32, Instant> = std::collections::HashMap::new();
    let mut ping_seq: u32 = 0;
    let mut rtt_samples: Vec<u64> = Vec::new();
    let mut rtt_lost = 0u64;
    loop {
        if let Some(d) = deadline {
            if Instant::now() >= d {
                break;
            }
        }
        if let Some((ch, _, l)) = link.recv(&mut out, Instant::now() + Duration::from_millis(300)) {
            if ch == p::CH_VIDEO {
                buf.extend_from_slice(&out[..l]);
            } else if ch == p::CH_MEDIA_AUDIO || ch == p::CH_ALT_AUDIO {
                abuf.extend_from_slice(&out[..l]); // v2 framing is scid-tagged — one drain serves both
            } else if rtt && ch == p::CH_ECHO && l >= 4 {
                let id = u32::from_le_bytes(out[..4].try_into().unwrap());
                if let Some(t) = pings.remove(&id) {
                    rtt_samples.push(t.elapsed().as_micros() as u64);
                }
                // an unknown id is a pruned-as-lost ping arriving very late — already counted in rtt_lost
            } else if ch == p::CH_CTRL && l >= 2 && out[0] == p::CT_SESSION_EVENT {
                // Label all four event kinds (#637): the old two-way branch mislabeled the phone-presence
                // events (0x03/0x04) as "HOST_GONE".
                match out[1] {
                    p::SEV_HOST_PRESENT => eprintln!("[avdec] box session event: HOST_PRESENT"),
                    p::SEV_HOST_GONE => eprintln!("[avdec] box session event: HOST_GONE"),
                    p::SEV_PHONE_PRESENT => eprintln!("[avdec] box session event: PHONE_PRESENT"),
                    p::SEV_PHONE_ABSENT => eprintln!("[avdec] box session event: PHONE_ABSENT"),
                    other => eprintln!("[avdec] box session event: unknown 0x{other:02x}"),
                }
            }
        }
        if Instant::now() >= next_hb {
            link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_HEARTBEAT]);
            next_hb = Instant::now() + Duration::from_secs(1);
        }
        if rtt && Instant::now() >= ping_at {
            ping_seq = ping_seq.wrapping_add(1);
            let mut pp = [0xA5u8; 64];
            pp[..4].copy_from_slice(&ping_seq.to_le_bytes());
            let t = Instant::now();
            if link.send(p::CH_ECHO, p::F_SOM | p::F_EOM, &pp) {
                pings.insert(ping_seq, t);
            }
            ping_at = Instant::now() + Duration::from_millis(100);
            // A ping outstanding > 5 s is lost, not late — prune so the map can't grow unbounded on a
            // wedged link (5 s is far past the FAIL threshold either way).
            let before = pings.len();
            pings.retain(|_, sent| sent.elapsed() < Duration::from_secs(5));
            rtt_lost += (before - pings.len()) as u64;
        }
        // Drain every complete length-prefixed message the reassembly buffer now holds.
        loop {
            if buf.len() < 4 {
                break;
            }
            let mlen = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
            // Plausibility bound raised (#651): a single 4K IDR access unit routinely exceeds the old
            // 2×MAX_PAYLOAD (128 KiB) ceiling — the box sends the whole frame as ONE length-prefixed
            // message — so that bound was rejecting legitimate keyframes as "desync". SEAM_VID_MAX is a
            // generous per-frame cap that still catches a truly garbled length prefix.
            if mlen == 0 || mlen > SEAM_VID_MAX {
                buf.drain(0..1); // implausible length prefix: resync one byte (never wedge on desync)
                continue;
            }
            if buf.len() < 4 + mlen {
                if buf.len() > 2 * SEAM_VID_MAX {
                    buf.drain(0..1); // a plausible length that never completes — treat as desync, resync
                    continue;
                }
                break; // wait for the rest of the message
            }
            let msg = buf[4..4 + mlen].to_vec();
            buf.drain(0..4 + mlen);
            // Current box wire (#664): every screen message is `[SEAM_MAGIC "SEAV"][marker][…]` — the
            // magic prefix (+ an 8-byte seq on frames) that this replica used to ignore, parsing the
            // marker at msg[0]. Realign to the real layout; on a missing magic, resync one byte.
            if msg.len() < 5 || msg[0..4] != SEAM_MAGIC {
                continue; // no SEAM magic where a message boundary was expected — skip this message
            }
            match msg[4] {
                // KEY: [magic 4][0x00][key 32][scid 8 LE]
                0x00 if msg.len() >= 4 + 1 + 32 => {
                    let mut k = [0u8; 32];
                    k.copy_from_slice(&msg[5..37]);
                    cipher = Some(ChaCha20Poly1305::new(Key::from_slice(&k)));
                    eprintln!("[avdec] received video key from box — decrypting on host");
                }
                // FRAME: [magic 4][0x01][seq u64 LE 8][hdr 128][body]
                0x01 if msg.len() >= 4 + 1 + 8 + 128 => {
                    let seq = u64::from_le_bytes(msg[5..13].try_into().unwrap());
                    let hdr = &msg[13..141];
                    let body = &msg[141..];
                    match hdr[4] {
                        0 => {
                            if let (Some(c), true) = (cipher.as_ref(), body.len() >= 16) {
                                // Nonce from the WIRE seq (#678), not a local per-frame counter: the
                                // ChaCha20 nonce must equal the iPhone's VideoFrame counter, and any
                                // dropped/gated frame on the box advances that counter — a local counter
                                // would desync permanently after the first gap, failing every Poly1305
                                // tag thereafter.
                                let mut nonce = [0u8; 12];
                                nonce[4..].copy_from_slice(&seq.to_le_bytes());
                                match c.decrypt(
                                    Nonce::from_slice(&nonce),
                                    Payload {
                                        msg: body,
                                        aad: hdr,
                                    },
                                ) {
                                    Ok(pt) => {
                                        ok += 1;
                                        plain_bytes += pt.len() as u64;
                                        if let Some(f) = dec.as_mut() {
                                            let _ = f.write_all(&pt);
                                        }
                                    }
                                    Err(_) => fail += 1,
                                }
                            }
                        }
                        1 => cfg += 1, // plaintext avcC/hvcC config (no decrypt)
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        // Audio: same [u32 len][marker][payload] framing on CH_MEDIA_AUDIO.
        loop {
            if abuf.len() < 4 {
                break;
            }
            let mlen = u32::from_be_bytes([abuf[0], abuf[1], abuf[2], abuf[3]]) as usize;
            if mlen == 0 || mlen > 2 * p::MAX_PAYLOAD {
                abuf.drain(0..1); // implausible length prefix: resync one byte
                continue;
            }
            if abuf.len() < 4 + mlen {
                if abuf.len() > 8 * p::MAX_PAYLOAD {
                    abuf.drain(0..1); // never-completing length: resync
                    continue;
                }
                break;
            }
            let mut msg = abuf[4..4 + mlen].to_vec();
            abuf.drain(0..4 + mlen);
            // Since 2026-09-03 the audio seam is magic-tagged like the video seam:
            // `[u32 BE len][SEAM_MAGIC "SEAV"][marker]…`. Strip it when present so this tool reads
            // both wires; a pre-magic box build still sends `[len][marker]`. (This debug receiver does
            // not implement the full scan-forward resync the macOS host does — it only needs to parse
            // a healthy stream.)
            if msg.len() >= 5 && msg[..4] == [0x53, 0x45, 0x41, 0x56] {
                msg.drain(0..4);
            }
            match msg[0] {
                // SEAM_KEY: [0x00][key 32][scid 8]
                0x00 if msg.len() >= 41 => {
                    let mut k = [0u8; 32];
                    k.copy_from_slice(&msg[1..33]);
                    let scid = u64::from_le_bytes(msg[33..41].try_into().unwrap());
                    aciphers.insert(scid, ChaCha20Poly1305::new(Key::from_slice(&k)));
                    eprintln!("[avdec] received audio key (scid={scid}) — decrypting on host");
                }
                // SEAM_FORMAT: [0x02][scid 8][codec][rate u32 LE][ch][bits][audio_type]
                0x02 if msg.len() >= 17 => {
                    let scid = u64::from_le_bytes(msg[1..9].try_into().unwrap());
                    let codec = msg[9];
                    let rate = u32::from_le_bytes(msg[10..14].try_into().unwrap());
                    let (chn, bits, at) = (msg[14], msg[15], msg[16]);
                    eprintln!("[avdec] audio format scid={scid}: codec={codec} {rate}Hz {chn}ch {bits}bit atype={at}");
                }
                // SEAM_PKT: [0x01][scid 8][raw RTP datagram]
                0x01 if msg.len() >= 9 => {
                    let scid = u64::from_le_bytes(msg[1..9].try_into().unwrap());
                    let pkt = &msg[9..];
                    // MIN_AUDIO_PACKET = 12 (hdr) + 16 (tag) + 8 (nonce)
                    if let (Some(c), true) = (aciphers.get(&scid), pkt.len() >= 12 + 16 + 8) {
                        let aad = &pkt[4..12];
                        let payload = &pkt[12..];
                        let n = payload.len();
                        let mut nonce = [0u8; 12];
                        nonce[4..].copy_from_slice(&payload[n - 8..]);
                        let ct_and_tag = &payload[..n - 8];
                        match c.decrypt(
                            Nonce::from_slice(&nonce),
                            Payload {
                                msg: ct_and_tag,
                                aad,
                            },
                        ) {
                            Ok(_) => aok += 1,
                            Err(_) => afail += 1,
                        }
                    }
                }
                _ => {}
            }
        }
        if last.elapsed() >= Duration::from_secs(2) {
            eprintln!(
                "[avdec] video {ok} OK / {fail} fail · {cfg} cfg · {plain_bytes} B  |  audio {aok} OK / {afail} fail"
            );
            last = Instant::now();
        }
    }
    println!(
        "[avdec] COMMITTED forward-encrypted — video {ok} decrypted / {fail} failed / {cfg} config ({plain_bytes} B AVCC → /tmp/carplay_dec.h264) · audio {aok} decrypted / {afail} failed"
    );
    // Report per stream; only claim a stream "validated" if it actually decrypted frames with 0 failures
    // (avoids overstating validation when e.g. no audio was playing so no audio stream arrived).
    if ok > 0 && fail == 0 {
        println!("[avdec] ✓ video decrypted host-side, 0 failures");
    }
    if aok > 0 && afail == 0 {
        println!("[avdec] ✓ audio decrypted host-side, 0 failures");
    }
    if fail > 0 || afail > 0 {
        println!("[avdec] ✗ decrypt failures — video {fail}, audio {afail}");
    }
    if rtt {
        rtt_lost += pings.len() as u64; // still-outstanding at exit = lost
        rtt_samples.sort_unstable();
        if rtt_samples.is_empty() {
            println!("[avdec] RTT under load: NO samples (lost={rtt_lost})");
        } else {
            let pct =
                |p: f64| rtt_samples[((rtt_samples.len() - 1) as f64 * p).round() as usize] as f64 / 1000.0;
            println!(
                "[avdec] RTT under A/V load (64 B echo, out_lo → upper bound): n={} p50={:.2} ms p90={:.2} ms p99={:.2} ms max={:.2} ms lost={}",
                rtt_samples.len(),
                pct(0.50),
                pct(0.90),
                pct(0.99),
                *rtt_samples.last().unwrap() as f64 / 1000.0,
                rtt_lost
            );
            println!("[avdec] gate (plan): PASS p99 ≤ 50 ms · MARGINAL 50–250 ms · FAIL > 250 ms or 1 s stalls");
        }
    }
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_STOP]);
    println!("[avdec] STOP sent — box tears the session down (holding pattern)");
}

// ---------------------------------------------------------------------------------------------
// setup-relay — Stage 0 of the app-driven SETUP plan (P1): full transparency. The host answers
// every relayed exchange by echoing the box's own local response verbatim (status 200), so the
// session must be behaviorally IDENTICAL to today's box-driven one — proving the whole relay path
// (airplayd RemoteSession → :9106 seam → ocbmd → CH_RTSP → here → back) end to end with zero
// authoring logic. Host authoring lands in P2 (setup_driver.rs); this mode is its oracle harness.
//
// Constants MIRRORED from `receiver::relay` (the authoritative definition) — the META_* pattern:
// this binary deliberately does not link the box's receiver crates.
// ---------------------------------------------------------------------------------------------
const RS_MAGIC: [u8; 4] = *b"RTSP"; // seam framing [magic][u32 BE len][msg]
const RS_SEAM_MAX: usize = 512 * 1024;
const RS_OPEN: u8 = 0x01;
const RS_REQ: u8 = 0x02;
const RS_RESP: u8 = 0x03;
const RS_CLOSE: u8 = 0x04;
const RS_REQ_FLAG_NOTIFY: u8 = 0x01;

fn rs_route_name(r: u8) -> &'static str {
    match r {
        1 => "SETUP",
        2 => "RECORD",
        3 => "TEARDOWN",
        _ => "?",
    }
}

fn rs_close_reason(r: u8) -> &'static str {
    match r {
        0 => "eof",
        1 => "hijack",
        2 => "error",
        3 => "reset",
        _ => "?",
    }
}

/// Send one seam-framed relay message back over CH_RTSP, chunked to OCBM's MAX_PAYLOAD (ocbmd
/// byte-forwards each chunk to airplayd's :9106 seam; the RTSP magic re-frames on that side, so
/// chunk boundaries are free).
fn send_rs(link: &mut Link, msg: &[u8]) -> bool {
    let mut framed = Vec::with_capacity(8 + msg.len());
    framed.extend_from_slice(&RS_MAGIC);
    framed.extend_from_slice(&(msg.len() as u32).to_be_bytes());
    framed.extend_from_slice(msg);
    let n = framed.chunks(p::MAX_PAYLOAD).count();
    for (i, chunk) in framed.chunks(p::MAX_PAYLOAD).enumerate() {
        let mut flags = 0u8;
        if i == 0 {
            flags |= p::F_SOM;
        }
        if i == n - 1 {
            flags |= p::F_EOM;
        }
        if !link.send(p::CH_RTSP, flags, chunk) {
            return false;
        }
    }
    true
}

/// Minimal YAML the box needs for this mode. Printed by the usage text and mirrored at
/// `tools/setup_relay_cfg.yaml`; the load-bearing line is `appDrivenSetup: true` — it is BOTH the
/// box's lever (levers::appsetup → the RemoteSession delegate selection) and the host's capability
/// declaration, so subscribing WITHOUT it leaves the session box-driven and this mode sees nothing.
const RS_EXAMPLE_YAML: &str = "accessoryConfig:\n  appDrivenSetup: true\n";

fn mode_setup_relay(link: &mut Link, secs: u64, cfg_path: Option<&str>, author: bool, mute_once: bool) {
    // The cfg is REQUIRED: without appDrivenSetup: true in the pushed YAML the box never selects the
    // relay delegate and this harness would silently observe nothing — error out loudly instead.
    let Some(cfg_path) = cfg_path else {
        eprintln!("usage: ocbm-host setup-relay [vid pid] [secs] <cfg.yaml>");
        eprintln!("       the YAML must contain (see tools/setup_relay_cfg.yaml):\n{RS_EXAMPLE_YAML}");
        std::process::exit(2);
    };
    let cfg = match std::fs::read(cfg_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[relay] read {cfg_path}: {e}");
            eprintln!("       a config is required; minimal content:\n{RS_EXAMPLE_YAML}");
            std::process::exit(2);
        }
    };
    if !String::from_utf8_lossy(&cfg).contains("appDrivenSetup") {
        eprintln!("[relay] WARNING: {cfg_path} does not mention appDrivenSetup — the box will stay box-driven");
    }
    // CRC of what we push — the box echoes ITS loaded config's crc in RS_OPEN; MISMATCH means the
    // session's /info was built from a different (stale/default) config than ours, in which case
    // Stage-0 transparency still holds (we echo local verbatim regardless) but the P2 authoring
    // premise ("host and box share one YAML") would not.
    let pushed_crc = p::crc32(&cfg);
    // Stage 1 (--author, plan P2): author each response host-side and diff it against the box's local
    // response. The driver derives its enabledFeatures from the SAME YAML the box built /info from, so
    // the oracle diff is meaningful. Stage 0 (default) still echoes local verbatim.
    let authoring_cfg = setup_driver::AuthoringConfig::from_yaml(&cfg);
    let mut driver = setup_driver::SetupDriver::new(authoring_cfg.clone());
    if author {
        eprintln!(
            "[relay] Stage-1 AUTHORING harness — SUBSCRIBE with {} B config (crc 0x{pushed_crc:08x}); authoring host-side + live-oracle diff. Derived features {:?}{}",
            cfg.len(),
            authoring_cfg.enabled_features(),
            if authoring_cfg.app_driven_setup { "" } else { " [WARN: config does not set appDrivenSetup — the box will stay box-driven and this harness will see nothing]" }
        );
    } else {
        eprintln!(
            "[relay] Stage-0 transparency harness — SUBSCRIBE with {} B config (crc 0x{pushed_crc:08x}), echoing every RS_REQ's local response verbatim",
            cfg.len()
        );
    }
    if mute_once {
        eprintln!("[relay] --mute-once armed: the FIRST RS_REQ will be dropped (author/send nothing) to exercise the box's timeout → sticky-local fallback");
    }
    let mut muted = false;
    let mut sub = vec![p::CT_SUBSCRIBE];
    sub.extend_from_slice(&cfg);
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &sub);

    let mut out = vec![0u8; p::MAX_PAYLOAD];
    let mut next_hb = Instant::now() + Duration::from_secs(1);
    let deadline = if secs > 0 {
        Some(Instant::now() + Duration::from_secs(secs))
    } else {
        None
    };
    // CH_RTSP seam reassembly (magic-resync, mirroring receiver::relay::FrameBuf).
    let mut rsbuf: Vec<u8> = Vec::new();
    // The current relay connection (RS_OPEN sets it; RS_CLOSE clears it). Messages for any other
    // conn are stale — a hijacked-away predecessor — and are dropped, per the protocol contract.
    let mut cur_conn: Option<u32> = None;
    let mut exchanges = 0u64;
    // Minimal A/V drain so the live session's video/audio doesn't back up ocbmd's queues behind an
    // unread host (byte counts only, no decrypt — mode_avdec is the decrypting harness).
    let (mut vbytes, mut abytes) = (0u64, 0u64);
    let mut last = Instant::now();
    loop {
        if let Some(d) = deadline {
            if Instant::now() >= d {
                break;
            }
        }
        if let Some((ch, _, l)) = link.recv(&mut out, Instant::now() + Duration::from_millis(100)) {
            match ch {
                p::CH_RTSP => rsbuf.extend_from_slice(&out[..l]),
                p::CH_VIDEO | p::CH_ALT_VIDEO => vbytes += l as u64,
                p::CH_MEDIA_AUDIO | p::CH_ALT_AUDIO => abytes += l as u64,
                p::CH_CTRL if l >= 2 && out[0] == p::CT_SESSION_EVENT => {
                    let ev = match out[1] {
                        x if x == p::SEV_HOST_PRESENT => "HOST_PRESENT",
                        x if x == p::SEV_HOST_GONE => "HOST_GONE",
                        x if x == p::SEV_PHONE_PRESENT => "PHONE_PRESENT",
                        x if x == p::SEV_PHONE_ABSENT => "PHONE_ABSENT",
                        _ => "?",
                    };
                    println!("[relay] box session event: {ev}");
                }
                _ => {}
            }
        }
        if Instant::now() >= next_hb {
            link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_HEARTBEAT]);
            next_hb = Instant::now() + Duration::from_secs(1);
        }
        // Drain complete relay messages: [magic "RTSP"][u32 BE len][msg], resync by magic scan.
        loop {
            match rsbuf.windows(4).position(|w| w == RS_MAGIC) {
                Some(0) => {}
                Some(n) => {
                    eprintln!("[relay] resynced over {n} garbage bytes (truncated frame upstream?)");
                    rsbuf.drain(..n);
                }
                None => {
                    let keep = rsbuf.len().min(3);
                    rsbuf.drain(..rsbuf.len() - keep);
                    break;
                }
            }
            if rsbuf.len() < 8 {
                break;
            }
            let mlen = u32::from_be_bytes(rsbuf[4..8].try_into().unwrap()) as usize;
            if mlen > RS_SEAM_MAX {
                rsbuf.drain(..1); // garbled length behind a stray magic — skip a byte, rescan
                continue;
            }
            if rsbuf.len() < 8 + mlen {
                break;
            }
            let msg: Vec<u8> = rsbuf[8..8 + mlen].to_vec();
            rsbuf.drain(..8 + mlen);
            // Common header [op u8][conn u32 LE][cseq u32 LE].
            if msg.len() < 9 {
                eprintln!("[relay] short message ({} B) — dropped", msg.len());
                continue;
            }
            let t0 = Instant::now();
            let op = msg[0];
            let conn = u32::from_le_bytes(msg[1..5].try_into().unwrap());
            let cseq = u32::from_le_bytes(msg[5..9].try_into().unwrap());
            let rest = &msg[9..];
            match op {
                RS_OPEN if rest.len() >= 10 => {
                    let (ver, flags) = (rest[0], rest[1]);
                    let box_crc = u32::from_le_bytes(rest[2..6].try_into().unwrap());
                    let ctx_len = u32::from_le_bytes(rest[6..10].try_into().unwrap()) as usize;
                    let crc_verdict = if box_crc == pushed_crc { "MATCH" } else { "MISMATCH" };
                    println!(
                        "[relay] RS_OPEN conn {conn}: ver {ver} flags 0x{flags:02x} (wireless={}) box cfg_crc 0x{box_crc:08x} vs pushed 0x{pushed_crc:08x} → {crc_verdict} · ctx {ctx_len} B",
                        flags & 0x01 != 0
                    );
                    if box_crc != pushed_crc {
                        println!("[relay]   (MISMATCH: the box built /info from a different config — Stage 0 still echoes local verbatim, but do not trust a P2 authoring diff on this session)");
                    }
                    cur_conn = Some(conn);
                    driver.reset(); // fresh connection = fresh SETUP state
                }
                RS_REQ if rest.len() >= 6 => {
                    let route = rest[0];
                    let flags = rest[1];
                    let local_len = u32::from_le_bytes(rest[2..6].try_into().unwrap()) as usize;
                    if rest.len() < 6 + local_len {
                        eprintln!("[relay] RS_REQ with truncated local response — dropped (box falls back local on timeout)");
                        continue;
                    }
                    let local = &rest[6..6 + local_len];
                    let req = &rest[6 + local_len..];
                    if cur_conn != Some(conn) {
                        // Stale conn (pre-hijack leftovers) — the protocol says drop; the box's
                        // 3 s timeout → local fallback covers the (already dead) exchange.
                        eprintln!("[relay] {} for non-current conn {conn} (current {cur_conn:?}) — dropped", rs_route_name(route));
                        continue;
                    }
                    if flags & RS_REQ_FLAG_NOTIFY != 0 {
                        // NOTIFY = no reply owed (TEARDOWN in v1). Still drive the driver's state so a
                        // full TEARDOWN resets it (partial keeps the session), same split as the box.
                        if author && route == 3 {
                            driver.on_teardown(req);
                        }
                        println!(
                            "[relay] {} NOTIFY conn {conn} cseq {cseq}: req {} B (no reply owed){}",
                            rs_route_name(route),
                            req.len(),
                            if author && route == 3 {
                                format!(" → driver state {:?}", driver.state)
                            } else {
                                String::new()
                            }
                        );
                        continue;
                    }
                    // --mute-once: deliberately DROP the first RS_REQ (author nothing, send nothing) to
                    // exercise the box's answer-timeout → sticky-local fallback. Validates that the box
                    // recovers the session on its own local response when the host goes silent.
                    if mute_once && !muted {
                        muted = true;
                        exchanges += 1;
                        println!(
                            "[relay] {} conn {conn} cseq {cseq}: MUTED (dropped, nothing sent) — box should time out and fall back to its local response (sticky-local)",
                            rs_route_name(route)
                        );
                        continue;
                    }
                    // Author the body: Stage 1 (--author) drives setup_driver; Stage 0 echoes local.
                    let body: Vec<u8> = if author {
                        match route {
                            1 => driver.on_setup(local, req), // SETUP (phase 1 vs 2 splits on `streams`)
                            2 => driver.on_record(local),     // RECORD → empty 200 body
                            _ => local.to_vec(),              // unknown route: fall back to local
                        }
                    } else {
                        local.to_vec()
                    };
                    // LIVE ORACLE: diff the bytes we're about to send vs the box's local response,
                    // plist-semantically with box-owned ports normalized (Stage 0 echoes local, so the
                    // diff is trivially clean there and only carries signal under --author).
                    let verdict = if author {
                        let diffs = setup_driver::oracle_diff(&body, local);
                        if diffs.is_empty() {
                            "MATCH".to_string()
                        } else {
                            format!("MISMATCH[{}]", diffs.join(", "))
                        }
                    } else {
                        "echo".to_string()
                    };
                    let mut resp = Vec::with_capacity(11 + body.len());
                    resp.push(RS_RESP);
                    resp.extend_from_slice(&conn.to_le_bytes());
                    resp.extend_from_slice(&cseq.to_le_bytes());
                    resp.extend_from_slice(&200u16.to_le_bytes());
                    resp.extend_from_slice(&body);
                    let sent = send_rs(link, &resp);
                    exchanges += 1;
                    println!(
                        "[relay] {} conn {conn} cseq {cseq}: req {} B · local {} B → {} {} B (host-side {} µs, sent={sent})",
                        rs_route_name(route),
                        req.len(),
                        local.len(),
                        verdict,
                        body.len(),
                        t0.elapsed().as_micros()
                    );
                }
                RS_CLOSE => {
                    let reason = rest.first().copied().unwrap_or(0xFF);
                    println!("[relay] RS_CLOSE conn {conn}: reason {}", rs_close_reason(reason));
                    if cur_conn == Some(conn) {
                        cur_conn = None; // per-conn state reset; the next RS_OPEN starts fresh
                        driver.reset(); // reset the authoring state machine on connection close
                    }
                }
                other => eprintln!("[relay] unexpected op 0x{other:02x} (conn {conn} cseq {cseq}) — dropped"),
            }
        }
        if last.elapsed() >= Duration::from_secs(5) {
            eprintln!(
                "[relay] alive — conn {cur_conn:?} · {exchanges} exchanges {} · drained video {vbytes} B / audio {abytes} B",
                if author { "authored" } else { "echoed" }
            );
            last = Instant::now();
        }
    }
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &[p::CT_STOP]);
    println!(
        "[relay] STOP sent — {exchanges} exchanges {}",
        if author {
            "host-authored + live-oracle-diffed (Stage 1, plan P2)"
        } else {
            "echoed verbatim (Stage 0 transparency)"
        }
    );
}

/// One-way DOWNLINK (box->host): ask the box to flood, drain + count raw bulk bytes.
fn mode_srcbench(link: &mut Link, secs: u64) {
    let ms = (secs * 1000) as u32;
    let mut req = vec![p::CT_SRC];
    req.extend_from_slice(&ms.to_le_bytes());
    link.send(p::CH_CTRL, p::F_SOM | p::F_EOM, &req);
    let mut buf = vec![0u8; 262144];
    let mut total = 0u64;
    let t0 = Instant::now();
    let end = t0 + Duration::from_secs(secs);
    while Instant::now() < end {
        if let Ok(n) = link
            .h
            .read_bulk(link.ep_in, &mut buf, Duration::from_millis(500))
        {
            total += n as u64;
        }
    }
    let dt = t0.elapsed().as_secs_f64();
    println!(
        "SOURCE downlink box->host: {} bytes in {:.2}s => {:.1} Mbps",
        total,
        dt,
        total as f64 * 8.0 / 1e6 / dt
    );
}

/// One-way UPLINK (host->box): flood CH_DISCARD frames (box parses + drops); count accepted bytes.
fn mode_sinkbench(link: &mut Link, secs: u64, xfer: usize) {
    let xfer = xfer.min(p::MAX_PAYLOAD);
    let payload = vec![0xA5u8; xfer];
    let mut frame = vec![0u8; p::HDR_LEN + xfer];
    let (mut total, mut seq) = (0u64, 0u32);
    let t0 = Instant::now();
    let end = t0 + Duration::from_secs(secs);
    while Instant::now() < end {
        let n = p::frame(
            &mut frame,
            p::CH_DISCARD,
            p::F_SOM | p::F_EOM,
            seq,
            &payload,
        );
        seq = seq.wrapping_add(1);
        match link
            .h
            .write_bulk(link.ep_out, &frame[..n], Duration::from_secs(2))
        {
            Ok(w) => total += w as u64,
            Err(_) => break,
        }
    }
    let dt = t0.elapsed().as_secs_f64();
    println!(
        "SINK uplink host->box: {} bytes in {:.2}s => {:.1} Mbps",
        total,
        dt,
        total as f64 * 8.0 / 1e6 / dt
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("hello");
    let vid = u16::from_str_radix(args.get(2).map(String::as_str).unwrap_or("1314"), 16)
        .unwrap_or(0x1314);
    let pid = u16::from_str_radix(args.get(3).map(String::as_str).unwrap_or("2d00"), 16)
        .unwrap_or(0x2d00);
    let secs: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(5);
    let xfer: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(16384);

    let mut link = open(vid, pid);
    do_hello(&mut link);
    // Auto-sync the box clock on every connect (the box has no RTC battery).
    push_time(&mut link);
    match mode {
        "settime" => {} // clock already pushed above; nothing more to do
        "echo" => mode_echo(&mut link, secs, xfer),
        "rtt" => mode_rtt(&mut link, secs),
        "mfi" => mode_mfi(&mut link),
        "console" => mode_console(&mut link),
        "ip" => mode_ip(
            &mut link,
            args.get(4).map(String::as_str).unwrap_or("127.0.0.1:22"),
            args.get(5).map(String::as_str),
        ),
        "srcbench" => mode_srcbench(&mut link, secs),
        "sinkbench" => mode_sinkbench(&mut link, secs, xfer),
        "bridge" => mode_bridge(
            &mut link,
            args.get(4).map(String::as_str).unwrap_or("ncm0"),
            args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0),
        ),
        "av" => mode_av(
            &mut link,
            args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0),
        ),
        "avdec" => mode_avdec(
            &mut link,
            args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0),
            // cfg path is positional; --rtt may appear anywhere after it
            args.get(5).map(String::as_str).filter(|a| *a != "--rtt"),
            args.iter().any(|a| a == "--rtt"),
        ),
        "session" => mode_session(
            &mut link,
            args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0),
            args.get(5).map(String::as_str),
        ),
        "setup-relay" => mode_setup_relay(
            &mut link,
            args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0),
            // cfg path is the first non-flag positional at/after index 5 (flags may follow it).
            args.iter().skip(5).find(|a| !a.starts_with("--")).map(String::as_str),
            args.iter().any(|a| a == "--author"),
            args.iter().any(|a| a == "--mute-once"),
        ),
        "push" => {
            let local = args.get(4).map(String::as_str).unwrap_or("");
            let remote = args.get(5).map(String::as_str).unwrap_or("");
            let mode = args
                .get(6)
                .and_then(|s| u32::from_str_radix(s, 8).ok())
                .unwrap_or(0o755);
            if local.is_empty() || remote.is_empty() {
                eprintln!("usage: ocbm-host push [vid pid] <local> <remote> [mode-octal]");
                std::process::exit(2);
            }
            if !mode_push(&mut link, local, remote, mode) {
                std::process::exit(1);
            }
        }
        "pull" => {
            let remote = args.get(4).map(String::as_str).unwrap_or("");
            let local = args.get(5).map(String::as_str).unwrap_or("");
            let mode = args
                .get(6)
                .and_then(|s| u32::from_str_radix(s, 8).ok())
                .unwrap_or(0o644);
            if remote.is_empty() || local.is_empty() {
                eprintln!("usage: ocbm-host pull [vid pid] <remote> <local> [mode-octal]");
                std::process::exit(2);
            }
            if !mode_pull(&mut link, remote, local, mode) {
                std::process::exit(1);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

    /// The nonce must be stable within a process (a reattach is the SAME host) and never 0
    /// ("not supplied"), or the box's replacement detection reads it wrong in either direction.
    #[test]
    fn host_instance_nonce_is_stable_and_nonzero() {
        let n = super::host_instance_nonce();
        assert_ne!(n, 0);
        assert_eq!(n, super::host_instance_nonce());
    }
    // Encrypt an audio AU exactly as receiver_core's encrypt_audio_aad (packet = [12B hdr][ct‖tag][nonce8],
    // nonce = [0,0,0,0]‖nonce8, aad = hdr[4..12]), then decrypt with the exact logic mode_avdec uses for
    // CH_MEDIA_AUDIO. Proves the host audio-decrypt replica matches the box wire.
    #[test]
    fn audio_decrypt_replica_roundtrip() {
        let key = [0x11u8; 32];
        let au = b"hello carplay audio AU payload!".to_vec();
        let header: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let nonce8 = [0x22u8; 8];
        let mut enc_nonce = [0u8; 12];
        enc_nonce[4..].copy_from_slice(&nonce8);
        let ct_tag = ChaCha20Poly1305::new(Key::from_slice(&key))
            .encrypt(
                Nonce::from_slice(&enc_nonce),
                Payload {
                    msg: &au,
                    aad: &header[4..12],
                },
            )
            .unwrap();
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&header);
        pkt.extend_from_slice(&ct_tag);
        pkt.extend_from_slice(&nonce8);
        // host replica (mode_avdec CH_MEDIA_AUDIO branch)
        assert!(pkt.len() >= 12 + 16 + 8);
        let aad = &pkt[4..12];
        let payload = &pkt[12..];
        let n = payload.len();
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&payload[n - 8..]);
        let ct_and_tag = &payload[..n - 8];
        let pt = ChaCha20Poly1305::new(Key::from_slice(&key))
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: ct_and_tag,
                    aad,
                },
            )
            .unwrap();
        assert_eq!(pt, au, "host audio decrypt must recover the original AU");
    }
}
