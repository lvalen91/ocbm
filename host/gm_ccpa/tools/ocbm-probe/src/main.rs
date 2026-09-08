//! ocbm-probe — bench instrument for the gm_ccpa host client.
//!
//! `ocbm-host` (in ccpa_custom) covers HELLO / MFi / console / file transfer, but it has no
//! CH_LOG or CH_MGMT support, and CH_LOG is the thing gm_ccpa has to learn. This claims the box,
//! arms the log stream, and prints every control frame and every decoded log entry, so the Kotlin
//! decoder can be written against captured bytes rather than the spec alone.
//!
//! usage: ocbm-probe [--secs N] [--subscribe <cfg.yaml>] [--cap-kb N] [--no-log] [--raw]
//!
//! Without `--subscribe` nothing raises `host_present`: no radios, no hostapd.conf rewrite. That
//! is the safe mode. `--subscribe` drives the full edge and is what the head-unit app does.

use ocbm_proto as p;
use rusb::{Context, Device, DeviceHandle, Direction, TransferType, UsbContext};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VID: u16 = 0x1314;
const PID: u16 = 0x2d00;
const LABEL: &str = "gm_ccpa probe";

struct Link {
    h: DeviceHandle<Context>,
    ep_in: u8,
    ep_out: u8,
    seq: u32,
    reasm: p::Reassembler,
}

impl Link {
    fn send(&mut self, ch: u16, pl: &[u8]) -> bool {
        let mut buf = vec![0u8; p::HDR_LEN + pl.len()];
        let n = match p::try_frame(&mut buf, ch, p::F_SOM | p::F_EOM, self.seq, pl) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("[probe] oversize on ch {ch}: {} > {}", e.len, e.max);
                return false;
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

fn open() -> Link {
    let ctx = Context::new().expect("libusb init");
    let devs = ctx.devices().expect("device list");
    let dev: Device<Context> = devs
        .iter()
        .find(|d| {
            d.device_descriptor()
                .map(|dd| dd.vendor_id() == VID && dd.product_id() == PID)
                .unwrap_or(false)
        })
        .unwrap_or_else(|| {
            eprintln!("no {VID:04x}:{PID:04x} on the bus");
            std::process::exit(1);
        });

    // Walk the interfaces for the bulk IN+OUT pair rather than assuming IF0/0x81/0x01.
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
    let h = dev.open().expect("open (permissions?)");
    let _ = h.claim_interface(ifnum);
    eprintln!("[probe] IF{ifnum} bulk IN=0x{ep_in:02x} OUT=0x{ep_out:02x}");
    Link { h, ep_in, ep_out, seq: 0, reasm: p::Reassembler::new() }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn ct_name(op: u8) -> &'static str {
    match op {
        p::CT_HELLO_ACK => "HELLO_ACK",
        p::CT_SETTIME => "SETTIME_ACK",
        p::CT_SESSION_EVENT => "SESSION_EVENT",
        p::CT_UPLINK => "UPLINK",
        p::CT_PAIRING_CODE => "PAIRING_CODE",
        p::CT_BT_PHASE => "BT_PHASE",
        p::CT_PHONE_IDENT => "PHONE_IDENT",
        p::CT_PROJ_MODE => "PROJ_MODE",
        p::CT_BOX_HEALTH => "BOX_HEALTH",
        _ => "?",
    }
}

/// Mirrors the box's own framing so the Kotlin port has a reference rendering.
fn print_log_frame(pl: &[u8], last_seq: &mut Option<u16>, raw: bool) {
    let mut off = 0usize;
    while off < pl.len() {
        let Some((e, used)) = p::decode_log_entry(&pl[off..]) else {
            eprintln!("[probe] !! malformed log entry at +{off} ({} B left)", pl.len() - off);
            return;
        };
        off += used;

        // Seq counts entries on CH_LOG as a whole, NOT per source — device-observed 2026-09-08,
        // where one ascending run spans bt -> radio_bt_attach -> wl -> box. A per-source counter
        // would report a gap on every source switch. It wraps; a real jump means the box's 64 KiB
        // queue overflowed and dropped entries we will never see.
        if let Some(prev) = *last_seq {
            let expect = prev.wrapping_add(1);
            if e.seq != expect {
                println!("            [gap] CH_LOG seq {} -> {}", prev, e.seq);
            }
        }
        *last_seq = Some(e.seq);

        let tags = {
            let mut t = String::new();
            if e.flags & p::LOG_F_BACKFILL != 0 { t.push_str(" [backfill]"); }
            if e.flags & p::LOG_F_TRUNCATED != 0 { t.push_str(" [truncated]"); }
            t
        };
        if let Some(n) = e.dropped_count() {
            println!("  !! {} lines dropped by {}", n, p::log_source_name(e.source));
            continue;
        }
        let text = String::from_utf8_lossy(e.text);
        println!("  {:>13} s={:<5}{} {}", p::log_source_name(e.source), e.seq, tags, text);
        if raw {
            println!("      raw hdr: src={:02x} flg={:02x} seq={} ms={} len={}",
                     e.source, e.flags, e.seq, e.unix_ms, e.text.len());
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let val = |name: &str| {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
    };
    let secs: u64 = val("--secs").and_then(|s| s.parse().ok()).unwrap_or(20);
    let cap_kb: u16 = val("--cap-kb").and_then(|s| s.parse().ok()).unwrap_or(256);
    let cfg_path = val("--subscribe");
    let want_log = !flag("--no-log");
    let raw = flag("--raw");

    let mut link = open();
    let mut out = vec![0u8; p::MAX_PAYLOAD];

    // ---- HELLO. Non-zero instance nonce + label, as the spec requires of a real host.
    let nonce: u32 = (std::process::id() as u32).wrapping_mul(2654435761).max(1);
    let mut hello = vec![p::CT_HELLO, p::VERSION];
    hello.extend_from_slice(&nonce.to_le_bytes());
    hello.extend_from_slice(LABEL.as_bytes());
    link.send(p::CH_CTRL, &hello);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match link.recv(&mut out, deadline) {
            Some((ch, _, l)) if ch == p::CH_CTRL && l >= 7 && out[0] == p::CT_HELLO_ACK => {
                let caps = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
                println!("HELLO_ACK v{} caps=0x{caps:08x} mode={}", out[1], out[6]);
                break;
            }
            Some(_) => continue,
            None => {
                eprintln!("[probe] no HELLO_ACK in 20 s");
                return;
            }
        }
    }

    // ---- CT_SETTIME. The box has no RTC battery; without this every log stamp is bogus.
    let mut st = vec![p::CT_SETTIME];
    st.extend_from_slice(&(now_ms() / 1000).to_le_bytes());
    link.send(p::CH_CTRL, &st);

    // ---- CH_MGMT GET_INFO. Works with or without a subscription.
    link.send(p::CH_MGMT, &[p::MGMT_GET_INFO]);

    // ---- Optional SUBSCRIBE. This is the host_present 0->1 edge: radios up, hostapd.conf rewrite.
    if let Some(path) = &cfg_path {
        match std::fs::read(path) {
            Ok(bytes) => {
                let mut sub = vec![p::CT_SUBSCRIBE];
                sub.extend_from_slice(&bytes);
                link.send(p::CH_CTRL, &sub);
                println!("[probe] SUBSCRIBE {} B from {path}", bytes.len());
                // SUBSCRIBE resets the clock-independent state AND clears the radio inhibit, so a
                // real host re-pushes time here too.
                link.send(p::CH_CTRL, &st);
            }
            Err(e) => eprintln!("[probe] cannot read {path}: {e}"),
        }
    }

    // ---- Arm the log stream. Must follow every SUBSCRIBE: the box resets it to OFF on teardown.
    if want_log {
        let mut lc = vec![p::CT_LOG_CTL, 1];
        lc.extend_from_slice(&cap_kb.to_le_bytes());
        link.send(p::CH_CTRL, &lc);
        println!("[probe] CT_LOG_CTL on, cap {cap_kb} KiB");
    }

    let mut last_seq: Option<u16> = None;
    let stop_at = Instant::now() + Duration::from_secs(secs);
    let mut next_hb = Instant::now();
    while Instant::now() < stop_at {
        if Instant::now() >= next_hb {
            link.send(p::CH_CTRL, &[p::CT_HEARTBEAT]);
            next_hb = Instant::now() + Duration::from_secs(1);
        }
        let Some((ch, flags, l)) = link.recv(&mut out, Instant::now() + Duration::from_millis(250))
        else { continue };
        let pl = &out[..l];
        match ch {
            p::CH_LOG => print_log_frame(pl, &mut last_seq, raw),
            p::CH_CTRL if l >= 1 => {
                let rep = if flags & p::F_REPLAY != 0 { " (replay)" } else { "" };
                match pl[0] {
                    p::CT_BOX_HEALTH if l >= 2 => {
                        println!("CTRL BOX_HEALTH 0x{:02x}{rep}", pl[1])
                    }
                    p::CT_BT_PHASE if l >= 2 => println!("CTRL BT_PHASE {}{rep}", pl[1]),
                    p::CT_PROJ_MODE if l >= 2 => println!("CTRL PROJ_MODE {}{rep}", pl[1]),
                    p::CT_SESSION_EVENT if l >= 2 => {
                        println!("CTRL SESSION_EVENT {}{rep}", pl[1])
                    }
                    p::CT_PHONE_IDENT => println!(
                        "CTRL PHONE_IDENT {}{rep}",
                        String::from_utf8_lossy(&pl[1..])
                    ),
                    op => println!("CTRL {} (0x{op:02x}) {} B{rep}", ct_name(op), l),
                }
            }
            p::CH_MGMT if l >= 1 && pl[0] == p::MGMT_INFO => {
                println!("MGMT_INFO {}", String::from_utf8_lossy(&pl[1..]));
            }
            other => println!("ch 0x{other:04x} {l} B"),
        }
    }

    // ---- Leave the box as we found it: log stream off, then STOP (immediate teardown since
    // 2026-09-03 — there is no warm grace to rely on any more).
    if want_log {
        link.send(p::CH_CTRL, &[p::CT_LOG_CTL, 0, 0, 0]);
    }
    if cfg_path.is_some() {
        link.send(p::CH_CTRL, &[p::CT_STOP]);
        println!("[probe] CT_STOP sent");
    }
}
