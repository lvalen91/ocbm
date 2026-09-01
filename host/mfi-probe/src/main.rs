//! mfi-probe — host-side exerciser for `mfid`.
//!
//! This is how the NCM MFi path gets proven before anything depends on it: point it at the box
//! over the USB-NCM link and confirm the coprocessor answers with a real certificate and a real
//! signature. It is the direct analogue of what `CH_MFI` does over OCBM, minus OCBM.
//!
//! ```text
//! mfi-probe selftest                       # ping + cert + sign against the default address
//! mfi-probe --addr 192.168.50.2:7789 cert  # dump the MFi certificate
//! mfi-probe sign 000102...13               # sign a 20-byte SHA-1 digest given as hex
//! ```

use std::io::Write;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use mfi_wire::{read_frame, write_frame, Op, Status, DEFAULT_PORT, DIGEST_LEN};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Generous: a contended chip lock is bounded at 10 s inside `mfi-i2c-local`, and the sign path
/// itself can poll for ~2.1 s. A client that gives up sooner would report a false failure.
const IO_TIMEOUT: Duration = Duration::from_secs(30);

const USAGE: &str = "\
mfi-probe — exercise the box's ephemeral MFi daemon (mfid) over NCM

USAGE:
    mfi-probe [--addr HOST:PORT] <COMMAND>

COMMANDS:
    ping                 Liveness only — does not touch the chip
    cert                 Fetch the MFi certificate (DER)
    sign [HEX]           Sign a 20-byte digest (default: a fixed test pattern)
    selftest             ping, then cert, then sign — the bring-up check

OPTIONS:
    --addr HOST:PORT     [default: 192.168.50.2:7789]
    -h, --help           This text";

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let bytes = s.trim().as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err("hex string has an odd number of characters".into());
    }
    // Validate every character BEFORE decoding, for two reasons: slicing a `&str` by byte offsets
    // panics when the offset lands inside a multi-byte character (`sign "€€"` is 6 bytes, passes an
    // even-length check, and splits mid-encoding), and `u8::from_str_radix` accepts a leading `+`,
    // so "+a+b…" would decode "successfully" and a typo'd digest would get signed.
    if let Some(&bad) = bytes.iter().find(|c| !c.is_ascii_hexdigit()) {
        return Err(format!("bad hex character: {:?}", bad as char));
    }
    fn nibble(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => c - b'A' + 10, // validated as an ASCII hex digit above
        }
    }
    Ok(bytes
        .chunks(2)
        .map(|pair| (nibble(pair[0]) << 4) | nibble(pair[1]))
        .collect())
}

fn connect(addr: &str) -> Result<TcpStream, String> {
    // Resolve first so a bad host is reported as such rather than as a timeout.
    let sockaddr = std::net::ToSocketAddrs::to_socket_addrs(&addr)
        .map_err(|e| format!("cannot resolve {addr}: {e}"))?
        .next()
        .ok_or_else(|| format!("{addr} resolved to nothing"))?;
    let sock = TcpStream::connect_timeout(&sockaddr, CONNECT_TIMEOUT)
        .map_err(|e| format!("cannot connect to {addr}: {e}"))?;
    sock.set_read_timeout(Some(IO_TIMEOUT)).ok();
    sock.set_write_timeout(Some(IO_TIMEOUT)).ok();
    sock.set_nodelay(true).ok();
    Ok(sock)
}

/// One request/response exchange. Returns the payload, or the daemon's status as an error.
fn exchange(sock: &mut TcpStream, op: Op, payload: &[u8]) -> Result<Vec<u8>, String> {
    write_frame(sock, op as u8, payload).map_err(|e| format!("send failed: {e}"))?;
    sock.flush().map_err(|e| format!("flush failed: {e}"))?;
    let (code, body) = read_frame(sock).map_err(|e| format!("receive failed: {e}"))?;
    match Status::from_u8(code) {
        Some(Status::Ok) => Ok(body),
        Some(other) => Err(format!("daemon returned {}", other.as_str())),
        None => Err(format!("daemon returned unknown status 0x{code:02x}")),
    }
}

fn cmd_ping(addr: &str) -> Result<(), String> {
    let mut sock = connect(addr)?;
    let started = Instant::now();
    exchange(&mut sock, Op::Ping, &[])?;
    println!("ping ok ({} ms)", started.elapsed().as_millis());
    Ok(())
}

fn cmd_cert(addr: &str) -> Result<Vec<u8>, String> {
    let mut sock = connect(addr)?;
    let started = Instant::now();
    let cert = exchange(&mut sock, Op::Cert, &[])?;
    println!(
        "cert ok — {} bytes in {} ms",
        cert.len(),
        started.elapsed().as_millis()
    );
    // A genuine MFi 2.0C certificate is ~945 bytes of DER and starts with SEQUENCE (0x30).
    if cert.first() != Some(&0x30) {
        println!("  WARNING: does not begin with DER SEQUENCE (0x30) — is this a real cert?");
    }
    println!("  head: {}…", hex_encode(&cert[..cert.len().min(32)]));
    Ok(cert)
}

fn cmd_sign(addr: &str, digest: &[u8]) -> Result<Vec<u8>, String> {
    if digest.len() != DIGEST_LEN {
        return Err(format!(
            "digest must be exactly {DIGEST_LEN} bytes, got {}",
            digest.len()
        ));
    }
    let mut sock = connect(addr)?;
    let started = Instant::now();
    let sig = exchange(&mut sock, Op::Sign, digest)?;
    println!(
        "sign ok — {} bytes in {} ms",
        sig.len(),
        started.elapsed().as_millis()
    );
    println!("  head: {}…", hex_encode(&sig[..sig.len().min(32)]));
    Ok(sig)
}

fn main() {
    let mut addr = format!("192.168.50.2:{DEFAULT_PORT}");
    let mut positional: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--addr" => match args.next() {
                Some(v) => addr = v,
                None => {
                    eprintln!("--addr needs a value");
                    std::process::exit(2);
                }
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            other => positional.push(other.to_string()),
        }
    }

    let command = positional.first().map(String::as_str).unwrap_or("selftest");

    // A fixed, obviously-synthetic digest. The chip signs any 20 bytes, so this proves the path
    // without pretending to be a real pair-setup challenge.
    let default_digest: Vec<u8> = (0u8..DIGEST_LEN as u8).collect();

    let result = match command {
        "ping" => cmd_ping(&addr),
        "cert" => cmd_cert(&addr).map(|_| ()),
        "sign" => {
            let digest = match positional.get(1) {
                Some(hex) => match hex_decode(hex) {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("mfi-probe: {e}");
                        std::process::exit(2);
                    }
                },
                None => default_digest,
            };
            cmd_sign(&addr, &digest).map(|_| ())
        }
        "selftest" => {
            println!("mfi-probe selftest against {addr}");
            cmd_ping(&addr)
                .and_then(|_| cmd_cert(&addr).map(|_| ()))
                .and_then(|_| cmd_sign(&addr, &default_digest).map(|_| ()))
                .map(|_| println!("\nselftest PASSED — the chip is reachable over NCM"))
        }
        other => {
            eprintln!("mfi-probe: unknown command {other:?}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    if let Err(e) = result {
        eprintln!("mfi-probe: {e}");
        std::process::exit(1);
    }
}
