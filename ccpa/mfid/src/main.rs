//! mfid — an ephemeral MFi chip service for NCM bring-up testing.
//!
//! # Why this exists
//!
//! `docs/ops/01_RECOVERY.md` (gm_ccpa) puts four MFi chip operations per session on OCBM's `CH_MFI`. The Pi port
//! keeps the chip on the CCPA but reaches it over the USB-NCM link instead, and `CH_MFI` is not
//! available there: `/script/ncm_only` (or `ncm_wifi`) selects an NCM boot that does not launch
//! `ocbmd`, and `crates/vendor/mfi/src/auth_client.rs` records that the older TCP client to
//! Carlinkit's `ncm_carplayd` auth service was removed by audit Fix #19. This is the minimum
//! replacement.
//!
//! # It is a bring-up instrument, not a product daemon
//!
//! It is meant to be staged into `/tmp` (tmpfs), run in the foreground for the length of a test,
//! and forgotten. A reboot erases it. Nothing installs it, nothing respawns it, and it never edits
//! `/script`, inittab, or any other persistent file.
//!
//! # How it stays out of OCBM's way
//!
//! 1. **The chip lock is shared and structural.** Every operation goes through
//!    `mfi_i2c_local::try_cert` / `try_sign`, which take the same `flock` on
//!    `/tmp/carplay_mfi.lock` that `airplayd`, `iap2d`, `wireless/src/mfi_local.rs` and — the one
//!    that actually matters here — **`ocbmd`'s own `CH_MFI` server** take. All five use
//!    `flock(LOCK_EX|LOCK_NB)` in a 20 ms poll loop under a 10 s deadline on the identical path,
//!    so this process is serialized against every existing chip user by the same mechanism they
//!    use against each other, not by convention.
//! 2. **It never waits unboundedly.** A contended lock polls for at most 10 s and then returns
//!    [`Status::LockBusy`] to the client, so it cannot wedge a caller that holds the iAP2 SESSION
//!    guard.
//! 3. **It never opens `/dev/usb_accessory`** and speaks no OCBM.
//! 4. **It refuses to start in OCBM mode.** With neither NCM flag present the box boots as a pure
//!    OCBM accessory, where `CH_MFI` already serves the chip and there is no NCM link to reach us
//!    on. `--force` overrides, deliberately noisily.
//! 5. **It exits on its own,** and never mid-transaction — see [`Activity`].

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{Ordering};
use portable_atomic::{AtomicU64};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use mfi_i2c_local::MfiError;
use mfi_wire::{read_frame, write_frame, Op, Status, DIGEST_LEN};

/// Flags that select an NCM boot. `ccpa/rootfs/script/start_main_service.sh:18` runs `ocbm_boot.sh`
/// only when NEITHER exists, and `ncm_base_install.sh --wifi-backstop` deliberately rests on
/// `ncm_wifi` with `ncm_only` REMOVED — so testing `ncm_only` alone would refuse to start on a
/// perfectly valid NCM box, and train the operator to reach for `--force`.
const NCM_FLAGS: [&str; 2] = ["/script/ncm_only", "/script/ncm_wifi"];

/// How long a client may leave a connection open without starting a request. Connections are
/// served sequentially, so an idle one blocks everyone behind it.
const IDLE_FRAME_WAIT: Duration = Duration::from_secs(300);

/// Once a frame has begun arriving, how long the remainder of it may take.
///
/// This is the trickle bound, and it is the reason [`DeadlineReader`] exists rather than a plain
/// `set_read_timeout`: `SO_RCVTIMEO` bounds each `read()` syscall, not a frame, and `read_exact`
/// loops. A peer dripping one byte per timeout-minus-epsilon would otherwise hold this
/// single-threaded daemon's only service slot for hours without ever tripping a timeout.
const FRAME_BODY_TIMEOUT: Duration = Duration::from_secs(30);

/// Write-side bound. A stalled reader cannot pin the daemon past this.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

struct Config {
    bind: String,
    idle_timeout: Duration,
    max_lifetime: Option<Duration>,
    force: bool,
}

const USAGE: &str = "\
mfid — ephemeral MFi chip service for NCM bring-up (NOT a product daemon)

USAGE:
    mfid [--bind ADDR:PORT] [--idle-timeout SECS] [--max-lifetime SECS] [--force]

OPTIONS:
    --bind ADDR:PORT     Listen address           [default: 0.0.0.0:7789]
    --idle-timeout SECS  Exit after SECS with no request; 0 disables  [default: 900]
    --max-lifetime SECS  Exit SECS after start regardless; 0 disables [default: 0]
    --force              Start even in OCBM mode (contends for the chip — avoid)
    -h, --help           This text

NOTE: --idle-timeout measures time since the last REQUEST, so a client polling `ping` on a timer
keeps the daemon alive indefinitely by design. For an unattended run, set --max-lifetime too.

The daemon writes no files, edits no config, and speaks no OCBM. Stage it in /tmp and it is gone
on the next reboot.";

fn parse_args() -> Result<Config, String> {
    let mut cfg = Config {
        bind: format!("0.0.0.0:{}", mfi_wire::DEFAULT_PORT),
        idle_timeout: Duration::from_secs(900),
        max_lifetime: None,
        force: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| -> Result<String, String> {
            args.next().ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--bind" => cfg.bind = value("--bind")?,
            "--idle-timeout" => {
                let secs: u64 = value("--idle-timeout")?
                    .parse()
                    .map_err(|_| "--idle-timeout needs an integer".to_string())?;
                cfg.idle_timeout = Duration::from_secs(secs);
            }
            "--max-lifetime" => {
                let secs: u64 = value("--max-lifetime")?
                    .parse()
                    .map_err(|_| "--max-lifetime needs an integer".to_string())?;
                cfg.max_lifetime = (secs > 0).then(|| Duration::from_secs(secs));
            }
            "--force" => cfg.force = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(cfg)
}

/// Liveness shared with the watchdog thread.
///
/// `chip` is the load-bearing part. An earlier version used an `AtomicBool` "busy" flag, which was
/// a check-then-act race: the watchdog could load `busy == false`, and the main thread could then
/// start a chip transaction before the watchdog's `exit()` actually tore the process down. The
/// window was not nanoseconds — the watchdog logs first, and `eprintln!` to a 115200-baud serial
/// console is slow. Upgrading the atomic's `Ordering` would NOT have fixed it, because the defect
/// was the non-atomic gap, not the memory model.
///
/// Holding a real mutex across the whole request→chip→response cycle, and requiring the watchdog to
/// hold it before exiting, is what actually makes "never exit mid-transaction" true: once the
/// watchdog owns the guard, no transaction can begin, and if one is already running it cannot
/// acquire the guard until that transaction has finished and answered.
struct Activity {
    start: Instant,
    last_ms: AtomicU64,
    chip: Mutex<()>,
}

impl Activity {
    fn new() -> Self {
        Activity {
            start: Instant::now(),
            last_ms: AtomicU64::new(0),
            chip: Mutex::new(()),
        }
    }

    fn touch(&self) {
        self.last_ms
            .store(self.start.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    fn idle_for(&self) -> Duration {
        let now = self.start.elapsed().as_millis() as u64;
        Duration::from_millis(now.saturating_sub(self.last_ms.load(Ordering::Relaxed)))
    }

    /// `panic = "abort"` (root `Cargo.toml`) means poisoning is unreachable; recover from it anyway
    /// rather than let a poisoned mutex hang the request path forever.
    fn lock_chip(&self) -> MutexGuard<'_, ()> {
        self.chip.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn spawn_watchdog(activity: Arc<Activity>, idle_timeout: Duration, max_lifetime: Option<Duration>) {
    if idle_timeout.is_zero() && max_lifetime.is_none() {
        return;
    }
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));

        // Acquire the chip guard BEFORE deciding to exit. Failing to take it means a transaction is
        // in flight, which both defers the deadlines and guarantees we cannot kill it.
        let _guard = match activity.chip.try_lock() {
            Ok(g) => g,
            Err(TryLockError::WouldBlock) => {
                activity.touch();
                continue;
            }
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
        };

        // From here the guard is held, so no chip transaction can start underneath the exit.
        if !idle_timeout.is_zero() && activity.idle_for() >= idle_timeout {
            eprintln!(
                "[mfid] idle {}s — exiting (bring-up daemon, not meant to linger)",
                idle_timeout.as_secs()
            );
            std::process::exit(0);
        }
        if let Some(max) = max_lifetime {
            if activity.start.elapsed() >= max {
                eprintln!("[mfid] max lifetime {}s reached — exiting", max.as_secs());
                std::process::exit(0);
            }
        }
    });
}

/// Bounds a whole frame in wall-clock time by shrinking `SO_RCVTIMEO` to the remaining budget
/// before every read. See [`FRAME_BODY_TIMEOUT`] for why a plain socket timeout is not enough.
struct DeadlineReader<'a> {
    sock: &'a TcpStream,
    deadline: Instant,
    started: bool,
}

impl Read for DeadlineReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "frame deadline exceeded",
            ));
        }
        let mut s = self.sock;
        // A zero SO_RCVTIMEO means "block forever" — never let the budget round down to it.
        s.set_read_timeout(Some(remaining.max(Duration::from_millis(1))))?;
        let n = s.read(buf)?;
        // The first byte of a frame flips the generous inter-request wait into the tight body
        // deadline, so a client may idle between requests but may not trickle one.
        if n > 0 && !self.started {
            self.started = true;
            self.deadline = Instant::now() + FRAME_BODY_TIMEOUT;
        }
        Ok(n)
    }
}

/// Map the chip layer's typed failure onto the wire. The distinction matters to the client:
/// `Chip` is worth retrying, `LockBusy` is not.
fn status_for(err: MfiError) -> Status {
    match err {
        MfiError::LockBusy => Status::LockBusy,
        // Deliberately mapped onto the existing LockBusy wire status rather than given a new one.
        // On the box this cannot normally happen — `/tmp` exists here and mfid owns the chip — so it
        // would mean a broken rootfs, and spending a wire discriminant that every existing client
        // would have to learn buys nothing for a case that should not occur. The distinction is
        // preserved where it is actually diagnostic: in the log below and in `MfiError` itself, which
        // is what the in-process callers (the receiver's iAP2 tunnel) match on.
        MfiError::LockUnavailable => {
            eprintln!(
                "[mfid] MFi lock path unopenable — this is NOT contention; the rootfs is wrong. \
                 Reporting LockBusy on the wire for compatibility."
            );
            Status::LockBusy
        }
        MfiError::Chip => Status::Chip,
    }
}

/// Caller must hold the chip guard.
fn handle_request(op: Op, payload: &[u8]) -> (Status, Vec<u8>) {
    match op {
        // Never touches the chip, so it is safe to poll during a live session.
        Op::Ping => (Status::Ok, Vec::new()),

        Op::Cert => {
            let started = Instant::now();
            match mfi_i2c_local::try_cert() {
                Ok(cert) => {
                    eprintln!(
                        "[mfid] cert ok — {} bytes in {} ms",
                        cert.len(),
                        started.elapsed().as_millis()
                    );
                    (Status::Ok, cert)
                }
                Err(e) => {
                    eprintln!("[mfid] cert failed: {:?}", e);
                    (status_for(e), Vec::new())
                }
            }
        }

        Op::Sign => {
            // Guard the length here rather than in the chip layer: a short digest would be handed
            // straight to the coprocessor, and the failure would surface as an opaque NAK.
            if payload.len() != DIGEST_LEN {
                eprintln!(
                    "[mfid] sign rejected — digest was {} bytes, expected {}",
                    payload.len(),
                    DIGEST_LEN
                );
                return (Status::BadRequest, Vec::new());
            }
            let started = Instant::now();
            match mfi_i2c_local::try_sign(payload) {
                Ok(sig) => {
                    eprintln!(
                        "[mfid] sign ok — {} bytes in {} ms",
                        sig.len(),
                        started.elapsed().as_millis()
                    );
                    (Status::Ok, sig)
                }
                Err(e) => {
                    eprintln!("[mfid] sign failed: {:?}", e);
                    (status_for(e), Vec::new())
                }
            }
        }
    }
}

fn serve(sock: TcpStream, activity: &Activity) -> io::Result<()> {
    sock.set_write_timeout(Some(WRITE_TIMEOUT))?;
    // The payloads are tiny and latency matters more than packing.
    let _ = sock.set_nodelay(true);

    loop {
        // Wait for the first byte WITHOUT the chip guard, then take the guard, then read the frame.
        // Reading the frame first and locking afterwards left a gap in which the watchdog could take
        // the guard and `exit()` between a request being parsed and its answer — the client saw EOF
        // on a request the daemon had already read. Blocking on the guard instead of on the socket is
        // not an option either: `IDLE_FRAME_WAIT` is 300 s and the watchdog treats a held guard as a
        // live transaction, so a parked connection would defer the idle exit for five minutes.
        // `peek` does not consume, so `read_frame` below still sees a whole frame.
        sock.set_read_timeout(Some(IDLE_FRAME_WAIT))?;
        match sock.peek(&mut [0u8; 1]) {
            Ok(0) => return Ok(()), // clean close between requests
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                eprintln!("[mfid] frame deadline exceeded — dropping a stalled peer");
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        // Held across the read of the rest of the frame, the chip call AND the response write, so a
        // shutdown can never land between a request the daemon has parsed and its answer, nor
        // between a signature the chip has already computed and its delivery to the client.
        let guard = activity.lock_chip();
        let mut reader = DeadlineReader {
            sock: &sock,
            // The first byte has already arrived, so this is the tight body deadline, not the
            // generous inter-request wait.
            deadline: Instant::now() + FRAME_BODY_TIMEOUT,
            started: true,
        };
        let (code, payload) = match read_frame(&mut reader) {
            Ok(frame) => frame,
            // A peer that closes MID-frame. The clean-close case is caught by the `peek` above; both
            // tear the connection down and no partial request is ever acted on.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            // A deadline expiry arrives either as our own TimedOut or, when SO_RCVTIMEO fires
            // first, as the platform's WouldBlock/EAGAIN. Both mean "this peer stalled" — say so,
            // rather than surfacing a bare errno that reads like a bug.
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                eprintln!("[mfid] frame deadline exceeded — dropping a stalled peer");
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        activity.touch();

        let (status, response) = match Op::from_u8(code) {
            Some(op) => handle_request(op, &payload),
            None => {
                eprintln!("[mfid] unsupported opcode 0x{code:02x}");
                (Status::Unsupported, Vec::new())
            }
        };
        let mut writer = &sock;
        let result = write_frame(&mut writer, status as u8, &response).and_then(|()| writer.flush());
        drop(guard);
        result?;

        activity.touch();
    }
}

fn ncm_mode() -> bool {
    NCM_FLAGS.iter().any(|f| Path::new(f).exists())
}

fn main() {
    let cfg = match parse_args() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("[mfid] {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    // Refuse to contend with a live OCBM projection session for the chip.
    if !ncm_mode() {
        if !cfg.force {
            eprintln!(
                "[mfid] neither {} nor {} exists — this box boots as a pure OCBM accessory, where \
                 CH_MFI already serves the chip and there is no NCM link to reach us on. Refusing \
                 to start; pass --force only if you know why you want to contend for the chip.",
                NCM_FLAGS[0], NCM_FLAGS[1]
            );
            std::process::exit(3);
        }
        eprintln!("[mfid] WARNING: --force in OCBM mode — contending with CH_MFI for the chip.");
    }

    let listener = match TcpListener::bind(&cfg.bind) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[mfid] cannot bind {}: {e}", cfg.bind);
            std::process::exit(1);
        }
    };

    let activity = Arc::new(Activity::new());
    activity.touch();
    spawn_watchdog(Arc::clone(&activity), cfg.idle_timeout, cfg.max_lifetime);

    eprintln!(
        "[mfid] listening on {} — idle-timeout {}s, max-lifetime {}",
        cfg.bind,
        cfg.idle_timeout.as_secs(),
        cfg.max_lifetime
            .map(|d| format!("{}s", d.as_secs()))
            .unwrap_or_else(|| "off".into()),
    );

    // Sequential by design: the chip is exclusive, so concurrency would only queue on the lock.
    for stream in listener.incoming() {
        match stream {
            Ok(sock) => {
                let peer = sock
                    .peer_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|_| "?".into());
                eprintln!("[mfid] connect {peer}");
                activity.touch();
                if let Err(e) = serve(sock, &activity) {
                    eprintln!("[mfid] {peer} ended: {e}");
                }
                eprintln!("[mfid] disconnect {peer}");
                activity.touch();
            }
            Err(e) => eprintln!("[mfid] accept failed: {e}"),
        }
    }
}
