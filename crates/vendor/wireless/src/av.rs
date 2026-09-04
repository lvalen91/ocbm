//! av.rs — wireless CarPlay A/V receiver-layer orchestration.
//!
//! After the BT→WiFi credential handoff (we answer the iPhone's 0x5702 with 0x5703), the phone joins
//! our AP and then expects to DISCOVER an AirPlay receiver on `wlan0`. This module brings that layer
//! up on demand:
//!   - `airplayd`  — the AirPlay/RTSP receiver (binds `[::]:5000`, so it accepts the wlan0 connection;
//!     runs pair-verify/MFi/SETUP and forwards A/V over OCBM). Its pairing identity is fixed to match
//!     rx-connect's advertised `deviceid`/`pi`/`features`.
//!   - `rx-connect` — mDNS: advertise `_airplay._tcp` on `wlan0` (`RX_ADDR` = the AP gateway) + browse
//!     the phone's `_carplay-ctrl._tcp` + connect-out `GET /ctrl-int/1/connect`.
//!
//! Idempotent by "start if not already running", so calling it on every 0x5703 is safe: repeated
//! 0x5702s are no-ops, and a reconnect re-spawns a daemon that died. Children are `setsid`-detached so
//! they outlive this RFCOMM session (and this process).

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Serializes `ensure_av_layer` so two 0x5703 handshakes in quick succession can't BOTH spawn airplayd
/// / rx-connect (the observed "Address in use" double-spawn): the double-fork means a just-spawned
/// daemon isn't visible to `pgrep` for a few ms, so without this lock + the post-spawn visibility wait
/// the second caller's `running()` check misses the first spawn and starts a duplicate.
static AV_LOCK: Mutex<()> = Mutex::new(());

/// In-memory "have we already brought the AV layer up this session" latch (2026-07-24 field bug fix).
/// Live-hardware test: a real iPhone retried the BT 0x5702 RequestAccessoryWiFiConfig handshake 3x
/// during one handoff (normal iOS behavior, never exercised by the wired regression suite). Each retry
/// re-entered `ensure_av_layer()`, and every `running("airplayd")` pgrep check — including the one right
/// after the FIRST, ultimately-successful spawn — came back false under the box's BT+WiFi bring-up load,
/// so the "failed to start" rollback fired every time and 2 fully redundant duplicate airplayd processes
/// were spawned (each immediately dying with "Address in use" against the first, real instance).
/// GM's shipped Cinemo NME accessory stack (reference/gm_cinemo — libNmeCarPlay.so) guards this exact
/// scenario with `NmeWifiTransportServer::Create()` gated on an in-memory object-existence check (its
/// "Wifi transport already existing" log line fires against a held object reference, not a fresh OS
/// process-table query) — repeat wifi-config requests are a no-op the instant a transport object exists,
/// with zero reliance on external process discovery. Mirror that: once THIS process has confirmed the
/// layer up once, later 0x5702 retries skip `running()`/spawn/rollback entirely instead of re-deriving
/// liveness from a `pgrep` snapshot that can race under load. Reset by `teardown_av_layer()`.
static AV_LAYER_UP: AtomicBool = AtomicBool::new(false);

/// PID of the airplayd instance the `AV_LAYER_UP` latch vouches for (0 = none recorded). Resolved
/// ONCE per bring-up via `pid_of` right after `wait_visible` confirms the layer up — never
/// re-derived per 0x5702 retry from a fresh `pgrep` snapshot (the exact race the latch exists to
/// avoid). Exists because the latch alone over-corrected (review fix 2026-07-31): with
/// `panic = "abort"` a crashed airplayd was never respawned for this process's whole lifetime,
/// contradicting the module header's "a reconnect re-spawns a daemon that died". The latch-hit path
/// checks THIS pid via a race-free `/proc/<pid>/cmdline` read (a file read, not a process-table
/// scan; the argv[0] comparison also guards PID reuse) and falls through to respawn if it's dead.
static AV_AIRPLAYD_PID: AtomicU32 = AtomicU32::new(0);

/// PID of the rx-connect instance the `AV_LAYER_UP` latch vouches for (0 = none recorded). Same
/// discipline as `AV_AIRPLAYD_PID`: resolved once per bring-up, checked via `pid_alive` on the latch-hit
/// path. Without it a crashed rx-connect (the mDNS advertiser) stayed latched-up for the process
/// lifetime, so a rejoining phone found no `_airplay._tcp` on `wlan0` and could never discover `:5000`
/// (audit Fix #11a — the latch vouched only for airplayd, not the advertiser).
static AV_RX_CONNECT_PID: AtomicU32 = AtomicU32::new(0);

/// Installed daemon paths. Overridable via env for dev (`/tmp` builds) without a rebuild.
const AIRPLAYD_BIN_DEFAULT: &str = "/usr/sbin/airplayd";
const RX_CONNECT_BIN_DEFAULT: &str = "/usr/sbin/rx-connect";
const WLAN_IFACE: &str = "wlan0";
/// The AP gateway hostapd/udhcpd serve on `wlan0` — the address the iPhone reaches airplayd at, and
/// the A record rx-connect advertises. Matches `start_bluetooth_wifi.sh` (WLANIP default).
///
/// ONE definition, hoisted to `box-common` on 2026-09-04: wireless Android Auto ADVERTISES this
/// address to the phone (`WifiStartRequest.ip_address`, `main.rs::aa_ap_params`) and `aa-bridge`
/// BINDS its wireless pump to it, so a second literal anywhere is a silent handoff failure.
pub(crate) const AP_IP: &str = box_common::net::AP_IP;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Is a process whose `argv[0]` is EXACTLY `path` running? MUST be called with the same string used to
/// spawn it (e.g. `/usr/sbin/airplayd`), not the bare basename — hardware-confirmed 2026-07-24 field
/// bug: BusyBox v1.37.0's `pgrep -x PATTERN` (without `-f`) matches PATTERN against the full `argv[0]`
/// from `/proc/<pid>/cmdline`, NOT against `/proc/<pid>/comm` (which is just the basename). Since every
/// daemon here is spawned via its full installed path (`AIRPLAYD_BIN_DEFAULT` etc.), `argv[0]` is that
/// full path — `pgrep -x airplayd` therefore NEVER matches it, deterministically, 100% of the time (not
/// a race): confirmed live via `pgrep -x airplayd` (exit 1, no match) vs `pgrep -x "/usr/sbin/airplayd"`
/// (exit 0, matched) against the exact same live PID. This was the true root cause of the `AV_LAYER_UP`
/// latch never engaging and rx-connect duplicating on retried 0x5702s (docs/wireless/00_WIRELESS_CARPLAY.md) — not load/timing.
/// `-x` (not `-f`) is kept deliberately: `-f` would go back to the original #31 problem (substring
/// false-matches against a log tail / editor / grep with the path in its own argv); an EXACT match on
/// the full invoked path is precise against both failure modes at once.
fn running(path: &str) -> bool {
    pgrep_exact(path) || pgrep_exact(basename(path))
}

/// `pgrep -x` matches DIFFERENT things on the two userlands this daemon runs on, and the two are
/// exact opposites — so the portable answer is to try both forms.
///
/// * **BusyBox 1.37** (the CCPA): matches against `argv[0]`, i.e. the FULL invoked path. Verified
///   in the field, see the comment above — `pgrep -x airplayd` never matches there.
/// * **toybox** (Android / the Raspberry Pi): matches against `/proc/<pid>/comm`, i.e. the
///   BASENAME. Verified live: `pgrep -x /data/local/tmp/airplayd` → rc=1 while
///   `pgrep -x airplayd` → the pid, for the very same process.
///
/// Both forms stay `-x` (exact), never `-f`: `-f` would substring-match the path inside an
/// unrelated process's command line — including the shell that exported `AIRPLAYD_BIN=<path>` —
/// which is the false-positive class `-x` exists to avoid.
fn pgrep_exact(pattern: &str) -> bool {
    Command::new("pgrep")
        .arg("-x")
        .arg(pattern)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The one PID of a process whose `argv[0]` is EXACTLY `path`, if running (`pgrep -x -o` = oldest
/// match). See `running`'s doc comment — `path` must be the full invoked path, not the bare basename.
fn pid_of(path: &str) -> Option<u32> {
    // Same dual-form lookup as `running` — see `pgrep_exact`.
    pgrep_oldest(path).or_else(|| pgrep_oldest(basename(path)))
}

fn pgrep_oldest(pattern: &str) -> Option<u32> {
    let out = Command::new("pgrep")
        .arg("-x")
        .arg("-o")
        .arg(pattern)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Race-free liveness check for the latched airplayd: does `/proc/<pid>/cmdline` still exist AND
/// name `path` as its `argv[0]`? A single /proc file read — NOT a `pgrep` process-table scan, so it
/// cannot produce the load-induced false negatives that motivated `AV_LAYER_UP`; a dead process
/// reads as ENOENT deterministically, never as a race. The argv[0] comparison guards PID reuse (a
/// recycled PID's cmdline won't be the full airplayd path — same full-path matching rule as
/// `running`, see its doc comment).
fn pid_alive(pid: u32, path: &str) -> bool {
    match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(cmdline) => cmdline.split(|&b| b == 0).next() == Some(path.as_bytes()),
        Err(_) => false, // /proc/<pid> gone — the process is dead
    }
}

/// Does the resident `airplayd` (docs/wireless/00_WIRELESS_CARPLAY.md #1.1) carry `CARPLAY_WIRELESS_METADATA=1` in its exec-time
/// environment? `ensure_av_layer`'s old "already running" check (`running("airplayd")`) trusted ANY
/// process named airplayd, including one the WIRED supervisor spawned with no wireless env at all
/// (`tools/session_supervisor.sh` deliberately omits it) — that silently starved every wireless session
/// of `iAPChannel`/metadata support with zero visible error. Reading `/proc/<pid>/environ` (the kernel's
/// own record of what was actually exec'd, not a guess) is the authoritative check; a failed read (PID
/// exited between the `pgrep` and this read) is treated as "not a match" so the caller respawns fresh
/// rather than risk trusting a process that's already gone.
fn airplayd_is_wireless_configured(airplayd_path: &str) -> bool {
    let Some(pid) = pid_of(airplayd_path) else {
        return false; // not running at all — caller's normal spawn path handles this
    };
    match std::fs::read(format!("/proc/{pid}/environ")) {
        Ok(env) => env
            .split(|&b| b == 0)
            .any(|kv| kv == b"CARPLAY_WIRELESS_METADATA=1"),
        Err(_) => false, // couldn't read (process gone, or /proc unavailable) — respawn to be safe
    }
}

/// Tear down the wireless A/V layer (#106): stop airplayd + rx-connect and clear the transport flag, so
/// a wireless session ending doesn't leave rx-connect advertising a dead `:5000` (the iPhone would keep
/// dialing a connection-refused receiver) or `carplay_transport` stuck at "wireless" (which would keep
/// the wired supervisor suppressed forever). Called on preempt / shutdown.
///
/// NOTE: this kills BY NAME (`pkill -x`), not by the PIDs we spawned — fine in today's standalone
/// single-transport mode (only ONE airplayd runs). If a true concurrent dual-transport arbiter ever
/// lets a wired airplayd run alongside, this would need PID-scoping (rx-connect is wireless-only, so
/// only airplayd is at risk); the transport-flag suppression already keeps the wired supervisor out of
/// airplayd's way, so the two don't run simultaneously in the current design.
pub fn teardown_av_layer() {
    // `pkill -x` needs the full invoked path too (see `running`'s doc comment) — a bare-name `pkill -x
    // airplayd` silently kills nothing on this BusyBox, leaving the real daemon running untouched.
    for path in [
        env_or("AIRPLAYD_BIN", AIRPLAYD_BIN_DEFAULT),
        env_or("RX_CONNECT_BIN", RX_CONNECT_BIN_DEFAULT),
    ] {
        let _ = Command::new("pkill").arg("-x").arg(path).status();
    }
    // Root cause #3 (dual-transport wired<->wireless switch): the WIRED supervisor's `arm()` spawns
    // rx-connect by BARE name (`setsid rx-connect`, so its argv[0] == "rx-connect"), while the `pkill -x`
    // above targets only our full-path `/usr/sbin/rx-connect`. A leaked wired rx-connect would otherwise
    // survive this teardown as a SECOND `_airplay._tcp` advertiser (wrong iface/identity). Exact-match
    // (`-x`) on the bare name reaps THAT form specifically and — being exact, not a `-f` substring — can
    // never hit our own full-path daemon or an unrelated `tail`/`grep` of an rx-connect log.
    let _ = Command::new("pkill").arg("-x").arg("rx-connect").status();
    let _ = std::fs::remove_file("/tmp/carplay_transport");
    AV_AIRPLAYD_PID.store(0, Ordering::Release);
    AV_RX_CONNECT_PID.store(0, Ordering::Release);
    AV_LAYER_UP.store(false, Ordering::Release);
    println!("[av] wireless A/V layer torn down (airplayd + rx-connect stopped)");
}

/// Spawn `bin` detached (own session via setsid, so a SIGTERM to us won't take it down), stdout+stderr
/// appended to `log`, with `envs` set. Fire-and-forget; we never reap it.
fn spawn_detached(bin: &str, log: &str, envs: &[(&str, &str)]) {
    let mut cmd = Command::new(bin);
    cmd.stdin(Stdio::null());
    if let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
    {
        if let Ok(f2) = f.try_clone() {
            cmd.stdout(Stdio::from(f)).stderr(Stdio::from(f2));
        }
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    // DOUBLE-FORK so the daemon reparents to init and never zombies in THIS process (#63) — WITHOUT
    // SIGCHLD=SIG_IGN, which would make every Command::status() here return ECHILD and break bt_bringup
    // + av::running(). In pre_exec (Command's direct child): setsid(), fork a grandchild; the
    // intermediate _exit(0)s and the grandchild execs the daemon with init as its parent. The parent
    // reaps the (immediately-exiting) intermediate via child.wait(), so nothing is left defunct here.
    // SAFETY: setsid/fork/_exit are async-signal-safe; no allocation between fork and exec.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            match libc::fork() {
                -1 => Err(std::io::Error::last_os_error()),
                0 => Ok(()),         // grandchild → exec the daemon
                _ => libc::_exit(0), // intermediate → exit; the parent reaps it
            }
        });
    }
    match cmd.spawn() {
        Ok(mut child) => {
            let _ = child.wait(); // reap the intermediate (it exits at once after forking the grandchild)
            println!("[av] started {bin} (detached, reparented to init)");
        }
        Err(e) => println!("[av] failed to start {bin}: {e}"),
    }
}

/// Poll for `path` (the full invoked path — see `running`'s doc comment) to become visible to `pgrep`
/// (or ~6s elapses), so a subsequent `ensure_av_layer` call sees it and doesn't spawn a duplicate.
/// Needed because the double-fork means the grandchild takes a few ms to exec and register.
///
/// Widened from 20×50ms (1s total) to 60×100ms (6s total) after the live-hardware BT+WiFi retry bug
/// (see `AV_LAYER_UP`) — this widening alone turned out NOT to be the actual fix (the real cause was
/// `running()` matching against the wrong string entirely, see its doc comment), but is kept as
/// reasonable headroom for airplayd's real startup (opens the MFi i2c chip, binds :5000) under
/// simultaneous BT RFCOMM + WiFi AP bring-up load.
fn wait_visible(path: &str) -> bool {
    for _ in 0..60 {
        if running(path) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Ensure the wireless A/V receiver layer is running. Idempotent; call after answering 0x5703.
pub fn ensure_av_layer() {
    // Serialize (see AV_LOCK): two rapid 0x5703s must not both spawn the daemons.
    let _guard = AV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // docs/wireless/00_WIRELESS_CARPLAY.md #1.6: claim the transport flag HERE, at spawn time, rather than waiting for airplayd to
    // see the phone's first control connection (that write happens seconds later, inside airplayd
    // itself). The gap between those two points was a real race: the wired supervisor's `arm()` guard
    // checks this same flag, and with nothing written yet it could kill/replace a correctly-configured
    // wireless airplayd mid-handoff. Reaching this function at all means a 0x5703 handoff just
    // succeeded, so wireless legitimately owns the session from this point on.
    let _ = std::fs::write("/tmp/carplay_transport", "wireless");

    let airplayd = env_or("AIRPLAYD_BIN", AIRPLAYD_BIN_DEFAULT);
    let rx_connect = env_or("RX_CONNECT_BIN", RX_CONNECT_BIN_DEFAULT);

    // Cinemo-pattern in-memory guard (see AV_LAYER_UP doc comment): once THIS process has confirmed the
    // layer up, later 0x5702 retries (iOS legitimately resends the BT WiFi-config handshake) are a no-op
    // here — never re-touch `pgrep`/spawn/rollback for them. This is what actually closes the live bug:
    // it removes the repeated `running("airplayd")` snapshots (racy under BT+WiFi load) from the retry
    // path entirely, rather than just widening their timeout.
    //
    // Extended (review fix 2026-07-31): the latch is only trusted while the RECORDED airplayd pid is
    // still alive, checked via `pid_alive` — a `/proc/<pid>/cmdline` read, so the Cinemo "no fresh
    // process-table snapshot on the retry path" rationale still holds (a file read on a known pid
    // cannot false-negative under load the way pgrep did). Without this, a crashed airplayd
    // (`panic = "abort"`, no unwinding to reset anything) stayed latched-up forever and no reconnect
    // could ever respawn it. Dead → reset the latch and fall through to the normal spawn path.
    // pid 0 (never resolved — `pid_of` raced the ONE post-confirm lookup) keeps the pre-fix
    // trust-the-latch behavior rather than reintroducing a pgrep on every retry.
    if AV_LAYER_UP.load(Ordering::Acquire) {
        // The latch vouches for the WHOLE layer — airplayd AND the rx-connect mDNS advertiser. Check
        // both (audit Fix #11a): if EITHER died, fall through to respawn (the per-daemon `running()`
        // checks below are idempotent, so only the dead one is actually restarted). `pid == 0` keeps the
        // pre-fix trust-the-latch behavior for a pid the ONE post-confirm `pid_of` lookup failed to
        // resolve, rather than reintroducing a pgrep on every retry.
        let ap_pid = AV_AIRPLAYD_PID.load(Ordering::Acquire);
        let rx_pid = AV_RX_CONNECT_PID.load(Ordering::Acquire);
        let ap_ok = ap_pid == 0 || pid_alive(ap_pid, &airplayd);
        let rx_ok = rx_pid == 0 || pid_alive(rx_pid, &rx_connect);
        if ap_ok && rx_ok {
            println!("[av] AV layer already existing — nothing to do (0x5702 retry)");
            return;
        }
        println!(
            "[av] latched AV layer degraded (airplayd pid {ap_pid} alive={ap_ok}, rx-connect pid \
             {rx_pid} alive={rx_ok}) — resetting the latch, respawning"
        );
        AV_AIRPLAYD_PID.store(0, Ordering::Release);
        AV_RX_CONNECT_PID.store(0, Ordering::Release);
        AV_LAYER_UP.store(false, Ordering::Release);
    }

    // Per-box identity (#636): derive it once and hand the SAME values to both airplayd and rx-connect
    // so each box is a distinct iOS "car" while the two daemons stay identity-locked (rx-connect's
    // advertised deviceid/pi MUST equal airplayd's or pair-verify fails).
    let id = crate::box_identity::derive();

    // NOTE ON `running`/`pkill` TARGETS BELOW: this specific block deliberately checks the BARE name
    // "airplayd", not the full `airplayd` path variable — it exists to catch a WIRED-spawned resident
    // specifically (`tools/session_supervisor.sh`'s `arm()` runs `setsid airplayd` via bare-name PATH
    // lookup, so ITS `argv[0]` really is "airplayd"). Every wireless-spawned airplayd is spawned via the
    // full path below and is always wireless-configured by construction, so it can never be the resident
    // this block is looking for — bare-name matching is correct here, not a leftover bug (see `running`'s
    // doc comment for why bare names otherwise never match this module's OWN full-path spawns).
    if running("airplayd") && !airplayd_is_wireless_configured("airplayd") {
        // docs/wireless/00_WIRELESS_CARPLAY.md #1.1: a resident airplayd exists but wasn't spawned for wireless (e.g. the wired
        // supervisor's env-less instance survived a prior session, or an unplug left it running) — do
        // NOT silently reuse it, or every wireless session would be starved of iAPChannel/metadata
        // support with no visible error. Kill it and fall through to the normal spawn path below.
        println!("[av] resident airplayd is not wireless-configured — replacing it");
        let _ = Command::new("pkill").arg("-x").arg("airplayd").status();
        // Poll for it to actually die (mirrors `wait_visible`'s pattern), escalating to -9 if a SIGTERM
        // handler (e.g. OCBM teardown) runs long — a single fixed short sleep here previously let a
        // slow-dying process still read as `running()` right below, silently falling into the "already
        // running (wireless-configured)" branch with a log message asserting the opposite (review
        // finding, 2026-07-24).
        for i in 0..20 {
            if !running("airplayd") {
                break;
            }
            if i == 9 {
                let _ = Command::new("pkill").arg("-9").arg("-x").arg("airplayd").status();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    // From here on, `running`/`wait_visible` target THIS module's own full-path spawns, so they take the
    // resolved `airplayd`/`rx_connect` path variables (see `running`'s doc comment) — not bare names.
    let airplayd_up = if running(&airplayd) {
        println!("[av] airplayd already running (wireless-configured)");
        true
    } else {
        // OCBM_FWD_ENC=1 = the committed forwarding model (encrypted frames + key over the seam;
        // the HOST decrypts/decodes), exactly as the wired launcher sets it
        // (tools/session_supervisor.sh). Without it airplayd falls back to the legacy on-box
        // decrypt path, whose VideoConfig→Annex-B conversion is avcC-only — an HEVC session
        // (app cfg `hevc=true`) then yields a 0-byte config and the host can never decode.
        // Log to a WIRELESS-specific file (not the wired /tmp/airplayd.log the supervisor scans): so the
        // wired supervisor's milestone grep can't latch a wireless session's "RECORD done" and mark the
        // wired projection healthy (per-transport logs, critical-finding cross-attribution fix).
        let mut envs: Vec<(&str, &str)> = vec![
            ("OCBM_FWD_ENC", "1"),
            // Advertise the wireless AAC audioFormats set so iOS negotiates media audio over
            // type-102 AAC-LC (Apple Music etc.); the wired supervisor-spawned airplayd does NOT get
            // this, so it keeps its PCM-only advert. (#400 — grounded in the carplayd-rs reference.)
            ("CARPLAY_WIRELESS_AUDIO", "1"),
            ("CARPLAY_DEVICE_ID", &id.device_id),
            ("CARPLAY_PI", &id.pi),
            ("CARPLAY_ED_SEED", &id.ed_seed_hex),
            // EXPERIMENTAL (docs/wireless/00_WIRELESS_CARPLAY.md, 2026-07-22, unverified on hardware): subscribe to NowPlaying/
            // RouteGuidance/CallState over the iAP2-over-AirPlay tunnel once the event channel wires
            // up (events.rs::send_wireless_metadata_subscriptions). THIS is the actual wireless
            // spawn path — session_supervisor.sh's launch line is wired-only and irrelevant here.
            ("CARPLAY_WIRELESS_METADATA", "1"),
            // EXPERIMENTAL (docs/wireless/00_WIRELESS_CARPLAY.md #2.1): declare sessionManagementInfo/stopSessionReasons.
            // Deliberately a SEPARATE flag from CARPLAY_WIRELESS_METADATA (not reused) so the two
            // features stay independently toggleable — docs/carplay/05_METADATA_AND_CONTROLS.md found sessionManagementInfo is in
            // Apple's own validated-feature-key cluster while iAPChannelInfo is NOT, so isolating
            // them is what makes it possible to tell which one (if either) is the real gate.
            ("CARPLAY_SESSION_MGMT", "1"),
        ];
        // cornerMask experiment (Phase 1): opt-in via the on-box flag file /tmp/cornermask_test (tmpfs,
        // so a reboot disarms it — same gate the wired supervisor uses). When present, advertise
        // cornerMasks and dump the phone's `topLeftCornerMask` buffer (CARPLAY_CORNERMASK_CAPTURE) so we
        // can learn its undocumented wire format. Absent = byte-identical to the proven wireless spawn.
        if std::path::Path::new("/tmp/cornermask_test").exists() {
            envs.push(("CARPLAY_CORNERMASKS", "1"));
            envs.push(("CARPLAY_CORNERMASK_CAPTURE", "/tmp"));
        }
        // mainBufferedAudio Phase-A (docs/carplay/04_CAPABILITIES_AND_CONFIG.md; docs/carplay/04_CAPABILITIES_AND_CONFIG.md workstream B4): the PRIMARY arm is now the
        // app-pushed `accessoryConfig.enablesMainBufferedAudio` (app default OFF), which airplayd
        // applies per control connection FROM THE YAML ALONE — so with an app connected this flag is
        // INERT (any parsed config overwrites the seeded env, in both directions). It survives only
        // for the no-config / parse-failure paths (manually launched airplayd, no
        // /tmp/carplay_cfg.yaml). Mirrors the wired supervisor block. CARPLAY_SETUP_DUMP captures the
        // phone's SETUP request to /tmp/setup_req.N. WARNING: NO buffered stream handler exists — if
        // iOS opens a MainBuffered stream we omit it (session.rs phase-2 default arm) and MEDIA GOES
        // SILENT; wireless is the arm where iOS is most likely to try (buffered audio is the
        // wireless-drop remedy, unlike wired which declined it) and is NOT yet captured. Disable the
        // config toggle / remove this flag on any audio dropout.
        if std::path::Path::new("/tmp/mainbuffered_test").exists() {
            envs.push(("CARPLAY_MAINBUFFERED", "1"));
            envs.push(("CARPLAY_SETUP_DUMP", "/tmp/setup_req"));
        }
        // Capturing the phone's SETUP request had been reachable ONLY through the flag above, which
        // also arms mainBufferedAudio — a combination the comment there warns can silence media on
        // exactly this (wireless) arm. That coupling makes a read-only diagnostic cost a real audio
        // risk, so it gets its own flag: `touch /tmp/setup_dump` writes /tmp/setup_req.N and changes
        // nothing else about the session.
        else if std::path::Path::new("/tmp/setup_dump").exists() {
            envs.push(("CARPLAY_SETUP_DUMP", "/tmp/setup_req"));
        }
        spawn_detached(&airplayd, "/tmp/airplayd_wl.log", &envs);
        wait_visible(&airplayd) // confirm up before a concurrent call could re-check (no double-spawn)
    };

    // Root cause #3 (dual-transport switch): reap any BARE-name wired rx-connect (argv[0] == "rx-connect",
    // the form `tools/session_supervisor.sh`'s `arm()` spawns) before the check below. `running(&rx_connect)`
    // only sees our full-path `/usr/sbin/rx-connect`, so a leaked wired rx-connect would slip past it and
    // co-advertise `_airplay._tcp` on the wrong interface/identity. `-x` on the bare name hits only that
    // stray form and never our own full-path spawn. (0x5702 retries take the `AV_LAYER_UP` early-return
    // above, so this runs once per real bring-up, not per retry.)
    let _ = Command::new("pkill").arg("-x").arg("rx-connect").status();

    let rx_up = if running(&rx_connect) {
        println!("[av] rx-connect already running");
        true
    } else {
        spawn_detached(
            &rx_connect,
            "/tmp/rx-connect_wl.log",
            &[
                ("RX_IFACE", WLAN_IFACE),
                ("RX_ADDR", AP_IP),
                ("CARPLAY_DEVICE_ID", &id.device_id),
                ("CARPLAY_PI", &id.pi),
            ],
        );
        wait_visible(&rx_connect)
    };

    // docs/wireless/00_WIRELESS_CARPLAY.md #1.6 (extended, review finding 2026-07-24; hardened 2026-07-24 field bug fix): the
    // transport flag is claimed at the TOP of this function, before the spawn is even attempted, to
    // close the pre-flag `arm()` race as early as possible. The tradeoff: if the spawn above ultimately
    // fails, the flag would be orphaned at "wireless" with no live airplayd — `wireless_owns_session` in
    // `tools/session_supervisor.sh` would then suppress `arm()`/`kill_session`/`escalate` indefinitely,
    // stranding the box out of the wired path too. Roll the claim back if the spawn genuinely failed.
    //
    // Deciding this from `airplayd_up` (the `wait_visible` result captured above) rather than a FRESH
    // `running("airplayd")` snapshot is the actual fix for the live bug: a fresh pgrep here re-opens the
    // exact same race `wait_visible` just spent up to 6s resolving, so re-querying it moments later can
    // still catch a false negative under BT+WiFi load. `airplayd_up` is authoritative for what THIS call
    // just did; trust it instead of re-deriving the answer from the OS process table a second time.
    if airplayd_up {
        // Record the pid behind the latch ONCE, here, while `running`/`wait_visible` just confirmed
        // visibility (so `pgrep -x -o` resolving it is as reliable as it ever gets). Every later
        // liveness question on the retry path is answered from this value via `pid_alive`'s /proc
        // read — never from a fresh process-table snapshot (see AV_AIRPLAYD_PID). Stored before the
        // latch so a latch-observing reader also sees the pid.
        AV_AIRPLAYD_PID.store(pid_of(&airplayd).unwrap_or(0), Ordering::Release);
        AV_RX_CONNECT_PID.store(if rx_up { pid_of(&rx_connect).unwrap_or(0) } else { 0 }, Ordering::Release);
        AV_LAYER_UP.store(true, Ordering::Release);
    } else {
        println!("[av] airplayd failed to start — releasing the transport flag");
        let _ = std::fs::remove_file("/tmp/carplay_transport");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards the #63 regression the verifier caught: after spawning detached children, a subsequent
    /// `Command::status()` in this process MUST still work (return Ok), i.e. we did NOT poison SIGCHLD
    /// into ECHILD. Also confirms the double-fork leaves no child of ours behind (wait would ECHILD).
    #[test]
    fn spawn_detached_does_not_break_command_status() {
        spawn_detached("/usr/bin/true", "/dev/null", &[]);
        // The exact failure mode of the reverted SIG_IGN fix: this returns Err(ECHILD) if SIGCHLD is
        // ignored process-wide. With the double-fork it stays Ok.
        let st = std::process::Command::new("/usr/bin/true").status();
        assert!(
            st.is_ok(),
            "Command::status() must still work after spawn_detached (no SIGCHLD=SIG_IGN)"
        );
        assert!(st.unwrap().success());
    }
}
