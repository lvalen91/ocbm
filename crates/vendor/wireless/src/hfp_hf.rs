//! The headset-side link to the phone: an HFP **hands-free** client, and its no-AT HSP fallback.
//!
//! WHY THIS EXISTS. gearhead 17.5 will not begin wireless Android Auto setup unless the phone's own
//! `BluetoothProfile.HEADSET` reports the head unit connected. The gate is read straight out of the
//! decompiled app: `pcl.java:80` calls `getConnectedDevices().contains(headUnit)`, `ozb.java:139`
//! widens that to `getDevicesMatchingConnectionStates({CONNECTED, CONNECTING})`, and `kzt.java:56-64`
//! / `pco.java:24-29` are the state mapping (`pcq.d` = CONNECTED_WITH_PROFILE). Fail it and the app
//! logs `WIRELESS_SETUP_FAILED_TO_START_NO_HFP_FROM_HU_PRESENCE`. Pass it and the PHONE opens our
//! Android Auto RFCOMM record (`createRfcommSocketToServiceRecord(4de17a00-…)`, `ojk.java:31-35`) —
//! it is the client of that UUID, and never its server.
//!
//! In the phone's HFP/HSP roles the phone is the **audio gateway** and we are the headset side. Two
//! ways to reach the gate, and this module implements both:
//!
//!   * **HFP (primary).** Connect to the phone's Handsfree AG channel and run the AT service-level
//!     connection. This is what the STOCK CCPA does with the same Pixel — `hfpd` (nohands) sends
//!     `AT+BRSF=63`, `AT+CIND=?`, `AT+CMER=3,0,0,1`, `AT+CLIP=1`, `AT+CCWA=1`, `AT+CHLD=?`,
//!     `AT+CIND?` and gets `AG …: Connected`, and 26 ms after the last `OK` the phone opens the
//!     box's AAP channel (`aa_full_session_adapter_20260315.txt:442-607`). [`establish_hfp`] sends
//!     exactly that dialogue, in that order.
//!   * **HSP (fallback).** Connect to the phone's Headset AG channel and say NOTHING. AOSP arms the
//!     SLC timer only for an inbound HFP connection: `bta_ag_act.cc:533-540` reads
//!     `if conn_service == BTA_AG_HFP { start SLC timer } else { bta_ag_svc_conn_open }`, and that
//!     second branch raises `BTA_AG_CONN_EVT` → `BTHF_CONNECTION_STATE_SLC_CONNECTED` →
//!     HeadsetStateMachine `mConnected` immediately. Both public dongles (aa-proxy-rs,
//!     WirelessAndroidAutoDongle) use this route and exchange no AT traffic at all.
//!
//! CORRECTED 2026-09-03 — TELEPHONY IS NO LONGER OUT OF SCOPE. This paragraph used to read "what
//! this module will never do: open a SCO channel, negotiate a codec, or carry audio … telephony is
//! explicitly out of scope". The first clause still holds and the last one does not. Android Auto
//! carries call AND Assistant audio over Bluetooth HFP — gearhead's own routing code does
//! `startVoiceRecognition` → `setCommunicationDevice` → `startBluetoothSco` (`kxr.java:118-150`) —
//! so the audio arrives on an (e)SCO channel on this very link and nowhere else. `sco_audio` serves
//! it; this module stays the AT half and still never touches the audio channel itself. What it does
//! now is CLASSIFY the AG's unsolicited traffic ([`CallTracker`]) so the layer above knows when
//! audio is coming.
//!
//! CODEC NEGOTIATION IS OFF BY DEFAULT AND LEVERED ON, and the default is load-bearing rather than
//! incidental. `AT+BRSF=63` does not set HF bit 7 (codec negotiation), so the AG never sends `+BCS`,
//! never expects `AT+BAC`, and always opens a plain CVSD narrowband channel. 63 is the stock CCPA's
//! own value against this same phone and is the ONLY dialogue proven on this hardware, so it stays
//! the default. [`wbs_enabled`] (`CARPLAY_HFP_WBS=1`, `/tmp/hfp_wbs`, `/script/hfp_wbs`) swaps it for
//! [`HF_SUPPORTED_FEATURES_WBS`] = 191 and adds ONE step to the dialogue — `AT+BAC=1,2` between
//! `AT+BRSF` and `AT+CIND=?`, where HFP 1.6 §4.2 requires it — after which the AG drives everything
//! else with unsolicited `+BCS: <id>` ([`choose_codec`]). Nothing else about the dialogue moves, and
//! with the lever off not one byte differs from what shipped.
//!
//! Runs in BOTH directions over an already-open socket, which is why every entry point takes one
//! rather than dialling: `reconnect` dials the phone, and the accept threads in `main.rs` serve the
//! records we advertise. The HF sends `AT+BRSF` first regardless of who opened the RFCOMM channel,
//! so an inbound HFP connection runs the identical dialogue.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

/// The HF feature bitmap we claim, matching the stock box's `AT+BRSF=63` exactly.
pub const HF_SUPPORTED_FEATURES: u32 = 63;

/// HF feature bit 7 (HFP 1.7 §4.34.1): "Codec negotiation". Setting it is what makes the AG run the
/// `AT+BAC` / `+BCS` exchange at all; with it clear the AG is required to open CVSD and never asks.
pub const HF_FEATURE_CODEC_NEGOTIATION: u32 = 1 << 7;

/// What we claim under the wideband lever: the stock 63 plus codec negotiation. Deliberately
/// 63-plus-one-bit rather than a fresh bitmap — every other claim in the proven dialogue is
/// unchanged, so a difference in the phone's behaviour has exactly one candidate cause.
pub const HF_SUPPORTED_FEATURES_WBS: u32 = HF_SUPPORTED_FEATURES | HF_FEATURE_CODEC_NEGOTIATION;

/// AG feature bit 9 in `+BRSF`: "Codec negotiation". `AT+BAC` is only sent when the AG claims it —
/// HFP 1.7 §4.2.1 makes the whole codec exchange conditional on BOTH sides setting their bit, and a
/// gateway without it answers `ERROR` to `AT+BAC`, which would abort an otherwise-complete SLC.
/// This Pixel answers `+BRSF: 879` = `0b11_0110_1111`, which has it set.
pub const AG_FEATURE_CODEC_NEGOTIATION: u32 = 1 << 9;

/// HFP 1.6 §4.11.3 Codec IDs. Only these two exist for HFP; 3 is reserved for LC3-SWB (HFP 1.9) and
/// we do not offer it, so an id outside this pair is one we never put in `AT+BAC`.
pub const CODEC_CVSD: u8 = 1;
pub const CODEC_MSBC: u8 = 2;

/// Operator lever for wideband speech: `CARPLAY_HFP_WBS=1`, or the presence of `/tmp/hfp_wbs` or
/// `/script/hfp_wbs`. Default OFF.
///
/// THREE sources and not one, for the same reason [`forced_path`] has two: this daemon is `exec`d
/// from inside the supervisor's `setsid sh -c`, so setting an environment variable on the box means
/// editing a shipped script — `/tmp/hfp_wbs` is the bench flip and `/script/hfp_wbs` the one that
/// survives a reboot. PRESENCE is the signal for the files; their contents are never read, so
/// `touch /tmp/hfp_wbs` is the whole gesture and an empty file is not a puzzle.
///
/// Resolved on every call rather than cached, so it can be flipped between reconnect cycles. The
/// value that was actually ACTED ON is recorded in [`Slc::wbs`] — a lever read twice can disagree
/// with itself, and the log must report what went on the wire, not what the file says later.
pub fn wbs_enabled() -> bool {
    if std::path::Path::new("/tmp/hfp_wbs").exists()
        || std::path::Path::new("/script/hfp_wbs").exists()
    {
        return true;
    }
    let raw = std::env::var("CARPLAY_HFP_WBS").ok();
    matches!(raw.as_deref().map(str::trim), Some("1") | Some("on") | Some("yes"))
}

/// Ceiling on the whole service-level connection. Stock's took 392 ms end to end (`AT+BRSF` at
/// 17:07:56.982 → the last `OK` at 17:07:57.374); 5 s is an order of magnitude of headroom and
/// still short enough that a wedged AG does not park the reconnect loop.
pub const SLC_BUDGET: Duration = Duration::from_secs(5);

/// Ceiling on any ONE request/response step. Belt and braces with [`SLC_BUDGET`] — a socket read
/// timeout is the mechanism, this is the accounting that survives a socket configured without one.
pub const STEP_BUDGET: Duration = Duration::from_secs(2);

/// AG feature bit 0 in `+BRSF`: three-way calling / call waiting. `AT+CHLD=?` is only meaningful,
/// and on some gateways only ACCEPTED, when it is set (HFP 1.7 §4.2.1). Stock's Pixel answered
/// `+BRSF: 879`, which has it set — so on this phone the step runs.
const AG_FEATURE_THREE_WAY: u32 = 1 << 0;

/// Which of the two routes to the gate was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Path {
    /// HFP hands-free, with the full AT service-level connection.
    Hfp,
    /// HSP headset, no AT traffic at all.
    Hsp,
}

impl Path {
    pub fn as_str(self) -> &'static str {
        match self {
            Path::Hfp => "HFP",
            Path::Hsp => "HSP",
        }
    }
}

/// Operator override for which route to take: `CARPLAY_AA_HEADSET_PATH=hfp|hsp`, or the on-box file
/// `/tmp/aa_headset_path` (this daemon is `exec`d from inside the supervisor's `setsid sh -c`, where
/// setting an environment variable means editing a shipped script — the same reason
/// `reconnect::acl_hold_secs` reads a file). Anything else, including absent, means AUTO: HFP first,
/// HSP if the phone has no HFP AG record or the SLC fails.
///
/// Resolved on every call rather than cached in a `OnceLock`, because it is a bench lever whose
/// whole point is being flipped between reconnect cycles without restarting the daemon.
pub fn forced_path() -> Option<Path> {
    let raw = std::env::var("CARPLAY_AA_HEADSET_PATH")
        .ok()
        .or_else(|| std::fs::read_to_string("/tmp/aa_headset_path").ok())?;
    parse_forced_path(&raw)
}

/// The pure half of [`forced_path`].
fn parse_forced_path(raw: &str) -> Option<Path> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "hfp" => Some(Path::Hfp),
        "hsp" => Some(Path::Hsp),
        _ => None,
    }
}

/// Arm a 1 s receive/send timeout on an RFCOMM socket before running the dialogue over it.
///
/// **Required, not hygiene.** Neither end of this arrives with one. `rfcomm::connect_to` sets only
/// `SO_SNDTIMEO`, and an ACCEPTED BR/EDR socket does not inherit the listener's `SO_RCVTIMEO`: the
/// kernel builds the child in `rfcomm_sock_init`, which copies `sk_type` and the security context
/// and nothing else, so `sock_init_data`'s `MAX_SCHEDULE_TIMEOUT` stands. Without this, a gateway
/// that opens the channel and then says nothing parks [`establish_hfp`] in `read()` forever — the
/// `total_deadline` accounting only runs BETWEEN reads — which for the outbound path wedges the
/// whole reconnect loop and for the inbound one wedges a thread `run_active_session` joins on its
/// way to going quiet. Same discipline, and the same 1 s, as `bt_driver`'s accepted socket.
///
/// A failure here is fatal to the attempt rather than a warning, for exactly the reason it is fatal
/// in `bt_driver`: quietly continuing restores the unbounded blocking syscall this exists to
/// prevent.
pub fn arm_socket_timeouts(sock: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let raw = sock.as_raw_fd();
    // zeroed()+assign, not a struct literal: under `musl32_time64` (riscv32) these types carry
    // private padding and a literal does not compile.
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    tv.tv_sec = 1;
    for opt in [libc::SO_RCVTIMEO, libc::SO_SNDTIMEO] {
        let ret = unsafe {
            libc::setsockopt(
                raw,
                libc::SOL_SOCKET,
                opt,
                &tv as *const libc::timeval as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        if ret < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// A completed service-level connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slc {
    pub path: Path,
    /// The AG's `+BRSF` feature bitmap. `None` for the HSP path, which has no such concept.
    pub ag_features: Option<u32>,
    /// Indicator names from `+CIND: ?`, in the order the AG listed them — which is the order
    /// `+CIEV: <index>,<value>` refers to, and the only way to render one legibly.
    pub indicators: Vec<String>,
    /// The AG's `+CHLD` hold modes, when asked for.
    pub chld: Option<String>,
    /// Whether this dialogue actually OFFERED wideband — `AT+BRSF=191` followed by `AT+BAC=1,2`.
    /// False on the HSP path, with the lever off, and also when the lever was on but the AG did not
    /// claim [`AG_FEATURE_CODEC_NEGOTIATION`]. Recorded rather than re-read from the lever because
    /// only this value says what the AG was told, and every later `+BCS` decision hangs off it.
    pub wbs: bool,
    /// How long the SLC took, for the log line.
    pub elapsed: Duration,
}

/// A failed step, named. The step name is the whole value of this type: "the SLC failed" is not
/// actionable on a bench, "AT+CIND=? -> ERROR" is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlcError {
    pub step: &'static str,
    pub why: String,
}

impl std::fmt::Display for SlcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} -> {}", self.step, self.why)
    }
}

/// Everything an established SLC hands back: the SLC itself, plus whatever the reader had already
/// pulled off the socket past the final `OK`.
///
/// The leftovers matter. The AG interleaves unsolicited results with the dialogue — stock saw
/// `+BSIR: 0` and `+BSIR: 1` land between `AT+CHLD=?`'s `OK` and `AT+CIND?`'s response — and a
/// buffered reader can easily have consumed a whole extra line before the caller takes the socket
/// back. Dropping the buffer would silently lose them; the caller drains `pending` first, then
/// resumes reading with `carry` prepended.
#[derive(Debug)]
pub struct SlcUp {
    pub slc: Slc,
    /// Complete unsolicited lines already read and not yet logged.
    pub pending: Vec<String>,
    /// A partial line left in the reader's buffer, to prepend to the next read.
    pub carry: Vec<u8>,
}

/// Line-framed reader over an AT socket.
///
/// AT responses are `\r\n`-delimited on both sides, but real gateways are sloppy about the exact
/// framing (leading `\r\n`, a trailing `\r` with no `\n`, several results in one packet), so this
/// splits on EITHER terminator and discards empties rather than requiring the pair.
struct AtReader<'a, S: Read> {
    io: &'a mut S,
    buf: Vec<u8>,
    /// Complete lines split out of `buf` but not yet returned.
    queued: std::collections::VecDeque<String>,
}

impl<'a, S: Read> AtReader<'a, S> {
    fn new(io: &'a mut S) -> Self {
        Self { io, buf: Vec::with_capacity(256), queued: std::collections::VecDeque::new() }
    }

    /// Move every complete line out of `buf` into `queued`, keeping any trailing partial.
    fn split_buffered(&mut self) {
        let mut start = 0usize;
        for i in 0..self.buf.len() {
            if self.buf[i] == b'\r' || self.buf[i] == b'\n' {
                if i > start {
                    let line = String::from_utf8_lossy(&self.buf[start..i]).trim().to_string();
                    if !line.is_empty() {
                        self.queued.push_back(line);
                    }
                }
                start = i + 1;
            }
        }
        self.buf.drain(..start);
    }

    /// The next complete line, or `Err` on timeout/EOF/IO error.
    ///
    /// `deadline` bounds the whole wait even if the socket carries no receive timeout, and a
    /// zero-byte read is EOF — an AG that hung up mid-dialogue must fail the step rather than spin.
    fn next_line(&mut self, deadline: Instant) -> Result<String, String> {
        loop {
            if let Some(l) = self.queued.pop_front() {
                return Ok(l);
            }
            if Instant::now() >= deadline {
                return Err("timed out waiting for a response".to_string());
            }
            let mut chunk = [0u8; 256];
            match self.io.read(&mut chunk) {
                Ok(0) => return Err("the gateway closed the link".to_string()),
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    // Guard against an AG that streams bytes with no terminator at all: without a
                    // ceiling this buffer grows until the deadline, and on a box with `panic =
                    // "abort"` an allocation failure would take CarPlay down with it.
                    if self.buf.len() > 8192 {
                        return Err("unterminated response exceeded 8 KiB".to_string());
                    }
                    self.split_buffered();
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    // The socket's own SO_RCVTIMEO fired. Not fatal by itself — `deadline` above is
                    // what decides, so a 1 s socket timeout inside a 2 s step just loops once.
                    continue;
                }
                Err(e) => return Err(format!("read failed: {e}")),
            }
        }
    }
}

/// Is this line an unsolicited result rather than an answer to the command in flight?
///
/// Everything the AG can volunteer mid-dialogue. `+BSIR` is not hypothetical: stock's capture has
/// `+BSIR: 0` and `+BSIR: 1` arriving between two of its commands.
pub fn is_unsolicited(line: &str) -> bool {
    // `+BVRA` ADDED 2026-09-03. gearhead routes the Assistant through the headset before it routes
    // anything else (`kxr.java:118-150`: `startVoiceRecognition` → `setCommunicationDevice` →
    // `startBluetoothSco`), so on this link `+BVRA: 1` is not an edge case — it is the FIRST audio
    // event most drives will see. It was already tolerated as an "unknown intermediate result", but
    // only by accident of `command`'s fallback; naming it makes the classification deliberate.
    const PREFIXES: [&str; 9] =
        ["+CIEV", "+BSIR", "+CLIP", "+CCWA", "+VGS", "+VGM", "+BCS", "+BIND", "+BVRA"];
    line == "RING" || PREFIXES.iter().any(|p| line.starts_with(p))
}

/// Is this line a final result code, and if so was it success?
fn final_result(line: &str) -> Option<bool> {
    if line == "OK" {
        return Some(true);
    }
    if line == "ERROR"
        || line == "NO CARRIER"
        || line.starts_with("+CME ERROR")
        || line.starts_with("+CMS ERROR")
    {
        return Some(false);
    }
    None
}

/// Send `cmd` and read until its final result, collecting any lines that start with `want_prefix`.
///
/// Returns the collected prefix-matched payloads. Unsolicited lines are collected separately into
/// `unsolicited` so the caller can log them without them being mistaken for the answer — the
/// distinction matters because `+CIEV` can arrive in the middle of a `+CIND` exchange and matching
/// on "the next line" would read it as the response.
fn command<S: Read + Write>(
    reader: &mut AtReader<'_, S>,
    step: &'static str,
    cmd: &str,
    want_prefix: Option<&str>,
    unsolicited: &mut Vec<String>,
    total_deadline: Instant,
) -> Result<Vec<String>, SlcError> {
    let fail = |why: String| SlcError { step, why };
    let step_deadline = (Instant::now() + STEP_BUDGET).min(total_deadline);
    // `\r` alone, not `\r\n`: that is what every AT gateway expects as the command terminator and
    // what stock sends. A trailing `\n` is tolerated by most and rejected by some.
    let wire = format!("{cmd}\r");
    reader
        .io
        .write_all(wire.as_bytes())
        .map_err(|e| fail(format!("write failed: {e}")))?;
    reader.io.flush().map_err(|e| fail(format!("flush failed: {e}")))?;

    let mut collected = Vec::new();
    loop {
        let line = reader.next_line(step_deadline).map_err(fail)?;
        if is_unsolicited(&line) {
            unsolicited.push(line);
            continue;
        }
        match final_result(&line) {
            Some(true) => return Ok(collected),
            Some(false) => return Err(fail(line)),
            None => {}
        }
        // Some gateways echo the command back when echo is on. Never treat that as the answer.
        if line.starts_with("AT") {
            continue;
        }
        match want_prefix {
            Some(p) if line.starts_with(p) => collected.push(line),
            // A response we did not ask for and cannot classify. Keep it for the log rather than
            // failing: an unknown intermediate result is not an error, and HFP gateways differ.
            _ => unsolicited.push(line),
        }
    }
}

/// Run the HFP hands-free service-level connection over an already-open RFCOMM socket.
///
/// The command order is STOCK's, not the spec's recommended order, and deliberately so: this exact
/// sequence is the only one proven against this phone
/// (`aa_full_session_adapter_20260315.txt:536-607`).
pub fn establish_hfp<S: Read + Write>(io: &mut S) -> Result<SlcUp, SlcError> {
    establish_hfp_with(io, wbs_enabled())
}

/// [`establish_hfp`] with the wideband lever passed in rather than read from the environment, so a
/// test can run both dialogues in one process without racing a global.
pub fn establish_hfp_with<S: Read + Write>(io: &mut S, wbs: bool) -> Result<SlcUp, SlcError> {
    let started = Instant::now();
    let total_deadline = started + SLC_BUDGET;
    let mut unsolicited = Vec::new();
    let mut reader = AtReader::new(io);

    // 1. Feature exchange. The AG's reply is what gates step 1b and step 6.
    let hf_features =
        if wbs { HF_SUPPORTED_FEATURES_WBS } else { HF_SUPPORTED_FEATURES };
    let brsf = command(
        &mut reader,
        "AT+BRSF",
        &format!("AT+BRSF={hf_features}"),
        Some("+BRSF:"),
        &mut unsolicited,
        total_deadline,
    )?;
    let ag_features = brsf
        .first()
        .and_then(|l| l.trim_start_matches("+BRSF:").trim().parse::<u32>().ok())
        .ok_or(SlcError {
            step: "AT+BRSF",
            why: format!("no parseable +BRSF in {brsf:?}"),
        })?;

    // 1b. WIDEBAND ONLY, and ONLY here. HFP 1.6 §4.2 puts `AT+BAC` immediately after the feature
    //     exchange and BEFORE `AT+CIND=?`: the codec list is part of establishing the SLC, and an
    //     AG that receives it later may answer `ERROR` or simply never offer mSBC. Skipped when the
    //     AG did not claim codec negotiation, because then `AT+BAC` itself is an `ERROR` waiting to
    //     abort a dialogue that was otherwise complete — the same reasoning as `AT+CHLD=?` below.
    //     With the lever off this step does not exist and the dialogue is byte-identical to stock's.
    let offered_wbs = wbs && ag_features & AG_FEATURE_CODEC_NEGOTIATION != 0;
    if offered_wbs {
        command(&mut reader, "AT+BAC", "AT+BAC=1,2", None, &mut unsolicited, total_deadline)?;
    }

    // 2. Indicator names and ranges. Parsed for the NAMES: `+CIEV: 6,4` is unreadable without them.
    let cind_test = command(
        &mut reader,
        "AT+CIND=?",
        "AT+CIND=?",
        Some("+CIND:"),
        &mut unsolicited,
        total_deadline,
    )?;
    let indicators = cind_test.first().map(|l| parse_indicator_names(l)).unwrap_or_default();

    // 3. Enable unsolicited indicator events. We never act on them; we log them.
    command(&mut reader, "AT+CMER", "AT+CMER=3,0,0,1", None, &mut unsolicited, total_deadline)?;
    // 4. Calling line identification.
    command(&mut reader, "AT+CLIP", "AT+CLIP=1", None, &mut unsolicited, total_deadline)?;
    // 5. Call waiting notification.
    command(&mut reader, "AT+CCWA", "AT+CCWA=1", None, &mut unsolicited, total_deadline)?;

    // 6. Hold modes — only when the AG claims three-way calling. Asking a gateway that does not
    //    would earn an `ERROR` and abort an SLC that was otherwise complete.
    let chld = if ag_features & AG_FEATURE_THREE_WAY != 0 {
        command(
            &mut reader,
            "AT+CHLD=?",
            "AT+CHLD=?",
            Some("+CHLD:"),
            &mut unsolicited,
            total_deadline,
        )?
        .first()
        .map(|l| l.trim_start_matches("+CHLD:").trim().to_string())
    } else {
        None
    };

    // 7. Current indicator values. Its OK is the moment the AG logs `Connected` and, on stock,
    //    26 ms before the phone opened the Android Auto channel.
    command(&mut reader, "AT+CIND?", "AT+CIND?", Some("+CIND:"), &mut unsolicited, total_deadline)?;

    let carry = reader.buf.clone();
    let mut pending: Vec<String> = reader.queued.iter().cloned().collect();
    pending.splice(0..0, unsolicited);
    Ok(SlcUp {
        slc: Slc {
            path: Path::Hfp,
            ag_features: Some(ag_features),
            indicators,
            chld,
            wbs: offered_wbs,
            elapsed: started.elapsed(),
        },
        pending,
        carry,
    })
}

/// The HSP route: the connection itself IS the service level.
///
/// There is nothing to send. AOSP's `bta_ag_act.cc:533-540` opens the service level on the inbound
/// connection when `conn_service != BTA_AG_HFP`, so by the time this returns the phone's
/// HeadsetStateMachine has already published `mConnected` for our address. Kept as a function
/// rather than inlined at the call site so both routes produce the same [`SlcUp`] and the caller
/// holds the link identically either way.
pub fn establish_hsp() -> SlcUp {
    SlcUp {
        slc: Slc {
            path: Path::Hsp,
            ag_features: None,
            indicators: Vec::new(),
            chld: None,
            wbs: false,
            elapsed: Duration::ZERO,
        },
        pending: Vec::new(),
        carry: Vec::new(),
    }
}

/// Pull the indicator NAMES out of a `+CIND: ("call",(0,1)),("callsetup",(0-3)),…` response, in
/// order. Index `n` in a later `+CIEV: n,v` is 1-based into this list.
///
/// Deliberately tolerant: anything in double quotes, in order, is a name. A gateway that formats
/// the ranges differently still yields usable names, and a response this cannot read yields an
/// empty list rather than failing an SLC over a log-only nicety.
pub fn parse_indicator_names(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('"') {
        let after = &rest[open + 1..];
        match after.find('"') {
            Some(close) => {
                out.push(after[..close].to_string());
                rest = &after[close + 1..];
            }
            None => break,
        }
    }
    out
}

/// Render an unsolicited line for the log, resolving `+CIEV: <n>,<v>` against the indicator names
/// the SLC learned. `+CIEV: 6,4` on stock's phone means `battchg = 4`, which is only legible with
/// the names to hand.
pub fn describe_unsolicited(line: &str, indicators: &[String]) -> String {
    let Some(args) = line.strip_prefix("+CIEV:") else {
        return line.to_string();
    };
    let mut parts = args.trim().splitn(2, ',');
    let (Some(idx), Some(val)) = (parts.next(), parts.next()) else {
        return line.to_string();
    };
    match idx.trim().parse::<usize>() {
        // 1-based, per HFP 1.7 §4.35.
        Ok(i) if i >= 1 && i <= indicators.len() => {
            format!("{line} ({} = {})", indicators[i - 1], val.trim())
        }
        _ => line.to_string(),
    }
}

/// A call-state change worth logging, and worth arming the SCO path for.
///
/// Deliberately DERIVED FROM THE INDICATORS, not from `RING`. `RING` is a repeating alert with no
/// "stopped" counterpart, so a state machine built on it cannot tell a missed call from an answered
/// one. `+CIEV` on the `call`/`callsetup`/`callheld` indicators is the AG's own authoritative state,
/// and `AT+CMER=3,0,0,1` (already in the SLC) is what turns those into unsolicited events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEvent {
    /// `callsetup` → 1: the phone is ringing.
    IncomingRinging,
    /// `callsetup` → 2: an outgoing call is being placed.
    OutgoingDialing,
    /// `callsetup` → 3: the remote end is ringing.
    OutgoingAlerting,
    /// `call` → 1: audio is live.
    CallActive,
    /// `call` → 0: the last call ended.
    CallEnded,
    /// `callsetup` → 0 with no active call: the call never connected (missed, rejected, cancelled).
    SetupAbandoned,
    /// `callheld` → 1/2.
    CallHeld,
    /// `callheld` → 0 while a call is still active.
    CallResumed,
    /// `+BVRA: <n>`. The AG's voice-recognition (Assistant) session, which carries its audio over
    /// the same SCO link a call would.
    VoiceRecognition(bool),
}

impl CallEvent {
    /// The exact log line for this transition. Kept next to the variant so the wording cannot drift
    /// between the two call sites (inbound and outbound headset links).
    pub fn describe(self) -> &'static str {
        match self {
            CallEvent::IncomingRinging => "incoming call ringing",
            CallEvent::OutgoingDialing => "outgoing call dialing",
            CallEvent::OutgoingAlerting => "outgoing call alerting (the far end is ringing)",
            CallEvent::CallActive => "call active — audio is on the Bluetooth SCO link",
            CallEvent::CallEnded => "call ended",
            CallEvent::SetupAbandoned => "call setup ended without connecting (missed/rejected)",
            CallEvent::CallHeld => "call held",
            CallEvent::CallResumed => "call resumed",
            CallEvent::VoiceRecognition(true) => {
                "phone started Bluetooth voice recognition (Assistant) — SCO audio armed"
            }
            CallEvent::VoiceRecognition(false) => {
                "phone stopped Bluetooth voice recognition (Assistant) — SCO audio disarmed"
            }
        }
    }
}

/// Follows the AG's call state across the life of one service-level connection.
///
/// Purely observational: nothing here answers, rejects or holds a call. Answering stays on the
/// phone and the Android Auto screen (`AT+ANSWER` has no equivalent in HFP anyway — it is `ATA`),
/// with the single exception of the bench lever in [`autoanswer`].
///
/// The indicator NAMES come from the AG's own `AT+CIND=?`, never from a fixed table: the order is
/// gateway-specific and `+CIEV: 2,1` means nothing without it. An AG whose `+CIND=?` this could not
/// parse yields an empty name list, and then every `+CIEV` is simply unclassified — degraded
/// logging, never a wrong state transition.
#[derive(Debug, Clone, Default)]
pub struct CallTracker {
    indicators: Vec<String>,
    call: u8,
    callsetup: u8,
    callheld: u8,
    vr: bool,
    /// `RING` seen with no `callsetup` indicator to explain it. Some gateways ring without moving
    /// an indicator we can see; one synthetic `IncomingRinging` is better than silence, and the
    /// latch keeps the repeats from producing one line per ring.
    ring_latched: bool,
}

impl CallTracker {
    pub fn new(indicators: &[String]) -> Self {
        Self { indicators: indicators.to_vec(), ..Default::default() }
    }

    /// True while the AG has audio to carry: a call in any phase, or a voice-recognition session.
    pub fn audio_wanted(&self) -> bool {
        self.call != 0 || self.callsetup != 0 || self.vr
    }

    /// Feed one unsolicited line; returns the transitions it caused (usually zero or one).
    pub fn observe(&mut self, line: &str) -> Vec<CallEvent> {
        let mut out = Vec::new();
        if let Some(rest) = line.strip_prefix("+BVRA:") {
            let on = rest.trim().split(',').next().and_then(|v| v.trim().parse::<u8>().ok());
            if let Some(v) = on {
                let on = v != 0;
                if on != self.vr {
                    self.vr = on;
                    out.push(CallEvent::VoiceRecognition(on));
                }
            }
            return out;
        }
        if line == "RING" {
            if self.callsetup == 0 && self.call == 0 && !self.ring_latched {
                self.ring_latched = true;
                out.push(CallEvent::IncomingRinging);
            }
            return out;
        }
        let Some(args) = line.strip_prefix("+CIEV:") else { return out };
        let mut parts = args.trim().splitn(2, ',');
        let (Some(idx), Some(val)) = (parts.next(), parts.next()) else { return out };
        let Ok(i) = idx.trim().parse::<usize>() else { return out };
        // 1-based, per HFP 1.7 §4.35 — the same convention `describe_unsolicited` renders with.
        let Some(name) = i.checked_sub(1).and_then(|z| self.indicators.get(z)) else { return out };
        let Ok(v) = val.trim().parse::<u8>() else { return out };
        match name.as_str() {
            "call" => {
                let was = self.call;
                self.call = v;
                if was == 0 && v != 0 {
                    self.ring_latched = false;
                    out.push(CallEvent::CallActive);
                } else if was != 0 && v == 0 {
                    self.callheld = 0;
                    self.ring_latched = false;
                    out.push(CallEvent::CallEnded);
                }
            }
            "callsetup" => {
                let was = self.callsetup;
                self.callsetup = v;
                match v {
                    1 if was != 1 => {
                        self.ring_latched = true;
                        out.push(CallEvent::IncomingRinging)
                    }
                    2 if was != 2 => out.push(CallEvent::OutgoingDialing),
                    3 if was != 3 => out.push(CallEvent::OutgoingAlerting),
                    // Setup cleared. If a call went active the `call` indicator already said so —
                    // reporting an abandonment here too would log every answered call as missed.
                    0 if was != 0 && self.call == 0 => {
                        self.ring_latched = false;
                        out.push(CallEvent::SetupAbandoned)
                    }
                    _ => {}
                }
            }
            "callheld" => {
                let was = self.callheld;
                self.callheld = v;
                if was == 0 && v != 0 {
                    out.push(CallEvent::CallHeld);
                } else if was != 0 && v == 0 && self.call != 0 {
                    out.push(CallEvent::CallResumed);
                }
            }
            _ => {}
        }
        out
    }
}

/// The caller's number out of `+CLIP: "+441234567890",145,…`, for the log only.
///
/// Returns the raw field. No normalisation and no formatting: this is a diagnostic line, and a
/// number we reshaped is a number that no longer matches what the phone shows.
pub fn parse_clip(line: &str) -> Option<String> {
    let rest = line.strip_prefix("+CLIP:")?.trim();
    let first = rest.split(',').next()?.trim();
    let n = first.trim_matches('"').trim();
    (!n.is_empty()).then(|| n.to_string())
}

/// The codec id out of an unsolicited `+BCS: <id>`. `None` for any other line.
pub fn parse_bcs(line: &str) -> Option<u8> {
    line.strip_prefix("+BCS:")?.trim().split(',').next()?.trim().parse::<u8>().ok()
}

/// What to answer a `+BCS: <id>` with. The AG has stopped and is WAITING for this — HFP 1.7 §4.11.3
/// has it send `+BCS`, wait for `AT+BCS`, and only then issue the (e)SCO connection request — so
/// every reachable input must produce exactly one command, and never zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecChoice {
    /// The AG picked a codec we offered. Apply the matching SCO voice setting FIRST, then reply
    /// `AT+BCS=<id>`; the order matters because the SCO request follows the reply within
    /// milliseconds and the accepted socket inherits whatever the listener carried at that moment.
    Use(u8),
    /// An id we never offered (HFP 1.7 §4.11.3: "the HF shall respond with AT+BAC with its
    /// available codecs"). Re-offer both and let the AG choose again.
    OfferBoth,
    /// Narrow the offer to CVSD only. Two callers: the lever is off (we never claimed wideband, so
    /// nothing else is honest), and the transparent voice setting could not be applied — in which
    /// case accepting mSBC would leave the AG sending air frames into a socket still decoding CVSD,
    /// i.e. full-scale noise on the call that made us do it.
    NarrowToCvsd,
}

impl CodecChoice {
    /// The AT command this choice sends. `String` because `Use` carries the AG's own id back.
    pub fn command(self) -> String {
        match self {
            CodecChoice::Use(id) => format!("AT+BCS={id}"),
            CodecChoice::OfferBoth => "AT+BAC=1,2".to_string(),
            CodecChoice::NarrowToCvsd => "AT+BAC=1".to_string(),
        }
    }
}

/// Decide the answer to `+BCS: <id>`.
///
/// `wbs` is [`Slc::wbs`] — what this link actually offered — and `narrowed` records that we have
/// already fallen back to `AT+BAC=1` on this link, so a second `+BCS: 2` cannot walk us back into a
/// transparent channel we already know we cannot serve. CVSD is accepted unconditionally: it is the
/// kernel's default voice setting, so there is no way to fail at it and nothing to fall back to,
/// and refusing it would be the one answer that leaves a call with no codec at all.
pub fn choose_codec(id: u8, wbs: bool, narrowed: bool) -> CodecChoice {
    match id {
        CODEC_CVSD => CodecChoice::Use(CODEC_CVSD),
        CODEC_MSBC if wbs && !narrowed => CodecChoice::Use(CODEC_MSBC),
        // mSBC we cannot or will not serve. NOT `Use(1)`: the AG asked about a specific id and the
        // spec's answer to "not that one" is a fresh `AT+BAC`, which restarts the negotiation and
        // gets us a `+BCS: 1` we can accept.
        CODEC_MSBC => CodecChoice::NarrowToCvsd,
        _ if wbs && !narrowed => CodecChoice::OfferBoth,
        _ => CodecChoice::NarrowToCvsd,
    }
}

/// BENCH ONLY: answer an incoming call with `ATA` instead of reaching for the phone.
///
/// Off by default and deliberately so — answering is the driver's decision, and the Android Auto
/// screen is where it belongs. This exists to make the SCO path testable by one person with one
/// phone, and it is resolved on every call (not cached) for the same reason [`forced_path`] is: the
/// whole point of a bench lever is flipping it without restarting the daemon.
pub fn autoanswer() -> bool {
    let raw = std::env::var("CARPLAY_HFP_AUTOANSWER")
        .ok()
        .or_else(|| std::fs::read_to_string("/tmp/hfp_autoanswer").ok());
    matches!(raw.as_deref().map(str::trim), Some("1") | Some("on") | Some("yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory audio gateway that replays a scripted response for each command it receives.
    ///
    /// Scripted per COMMAND rather than as one byte stream, because the property under test is that
    /// the client sends the right commands in the right order — a single stream would pass even if
    /// the client sent them backwards.
    struct FakeAg {
        /// `(expected command, response bytes)`, consumed in order.
        script: Vec<(&'static str, &'static str)>,
        sent: Vec<String>,
        pending_read: Vec<u8>,
        /// Bytes written by the client that have not yet formed a complete command.
        partial: Vec<u8>,
    }

    impl FakeAg {
        fn new(script: Vec<(&'static str, &'static str)>) -> Self {
            Self { script, sent: Vec::new(), pending_read: Vec::new(), partial: Vec::new() }
        }
    }

    impl Write for FakeAg {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.partial.extend_from_slice(buf);
            while let Some(pos) = self.partial.iter().position(|b| *b == b'\r') {
                let cmd = String::from_utf8_lossy(&self.partial[..pos]).to_string();
                self.partial.drain(..pos + 1);
                self.sent.push(cmd.clone());
                if self.script.is_empty() {
                    return Err(std::io::Error::other(format!("unscripted command {cmd:?}")));
                }
                let (expect, resp) = self.script.remove(0);
                assert_eq!(cmd, expect, "commands must be sent in the stock order");
                self.pending_read.extend_from_slice(resp.as_bytes());
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Read for FakeAg {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pending_read.is_empty() {
                // Nothing scripted to say: EOF, so a test that waits for a line it will never get
                // fails fast instead of burning the 2 s step budget.
                return Ok(0);
            }
            let n = buf.len().min(self.pending_read.len());
            buf[..n].copy_from_slice(&self.pending_read[..n]);
            self.pending_read.drain(..n);
            Ok(n)
        }
    }

    /// Exactly what the stock box's `hfpd` exchanged with this Pixel, transcribed from
    /// `aa_full_session_adapter_20260315.txt:536-607`, including the split `+BRSF`/`OK` lines and
    /// the two `+BSIR` results that landed mid-dialogue.
    fn stock_script() -> Vec<(&'static str, &'static str)> {
        vec![
            ("AT+BRSF=63", "\r\n+BRSF: 879\r\n\r\nOK\r\n"),
            (
                "AT+CIND=?",
                "\r\n+CIND: (\"call\",(0,1)),(\"callsetup\",(0-3)),(\"service\",(0-1)),\
                 (\"signal\",(0-5)),(\"roam\",(0,1)),(\"battchg\",(0-5)),(\"callheld\",(0-2))\r\n\r\nOK\r\n",
            ),
            ("AT+CMER=3,0,0,1", "\r\nOK\r\n"),
            ("AT+CLIP=1", "\r\nOK\r\n"),
            ("AT+CCWA=1", "\r\nOK\r\n"),
            ("AT+CHLD=?", "\r\n+CHLD: (0,1,2,3)\r\n\r\nOK\r\n"),
            // The +BSIR pair arrives here on the real phone, between the CHLD OK and the CIND
            // values — interleaved with, not after, the dialogue.
            ("AT+CIND?", "\r\n+BSIR: 0\r\n\r\n+BSIR: 1\r\n\r\n+CIND: 0,0,0,0,0,5,0\r\n\r\nOK\r\n"),
        ]
    }

    /// THE test: the stock dialogue, replayed, must complete and must have sent exactly the stock
    /// commands in the stock order (`FakeAg` asserts the order as it goes).
    #[test]
    fn the_stock_dialogue_establishes_the_slc() {
        let mut ag = FakeAg::new(stock_script());
        let up = establish_hfp_with(&mut ag, false).expect("SLC must establish");
        assert_eq!(up.slc.path, Path::Hfp);
        assert_eq!(up.slc.ag_features, Some(879));
        assert_eq!(up.slc.chld.as_deref(), Some("(0,1,2,3)"));
        assert_eq!(
            up.slc.indicators,
            ["call", "callsetup", "service", "signal", "roam", "battchg", "callheld"]
        );
        assert_eq!(
            ag.sent,
            [
                "AT+BRSF=63",
                "AT+CIND=?",
                "AT+CMER=3,0,0,1",
                "AT+CLIP=1",
                "AT+CCWA=1",
                "AT+CHLD=?",
                "AT+CIND?",
            ]
        );
        // The interleaved +BSIR results must be surfaced, not silently eaten.
        assert!(up.pending.iter().any(|l| l == "+BSIR: 0"), "pending: {:?}", up.pending);
        assert!(up.pending.iter().any(|l| l == "+BSIR: 1"), "pending: {:?}", up.pending);
        assert!(up.carry.is_empty(), "no partial line should be left over");
    }

    /// `AT+CHLD=?` must be SKIPPED when the AG does not claim three-way calling, or a gateway that
    /// answers `ERROR` to it aborts an otherwise-complete SLC.
    #[test]
    fn chld_is_skipped_when_the_ag_does_not_claim_three_way() {
        let mut script = stock_script();
        script[0] = ("AT+BRSF=63", "\r\n+BRSF: 878\r\n\r\nOK\r\n"); // 879 with bit0 cleared
        script.remove(5); // no AT+CHLD=? expected
        let mut ag = FakeAg::new(script);
        let up = establish_hfp_with(&mut ag, false).expect("SLC must still establish");
        assert_eq!(up.slc.ag_features, Some(878));
        assert_eq!(up.slc.chld, None);
        assert!(!ag.sent.iter().any(|c| c.starts_with("AT+CHLD")), "sent: {:?}", ag.sent);
    }

    /// An `ERROR` must fail, and must name the step that got it — the whole point of `SlcError`.
    #[test]
    fn an_error_fails_the_named_step() {
        let mut script = stock_script();
        script[2] = ("AT+CMER=3,0,0,1", "\r\nERROR\r\n");
        let mut ag = FakeAg::new(script);
        let err = establish_hfp_with(&mut ag, false).expect_err("ERROR must fail the SLC");
        assert_eq!(err.step, "AT+CMER");
        assert_eq!(err.why, "ERROR");
        assert_eq!(err.to_string(), "AT+CMER -> ERROR");
    }

    /// `+CME ERROR: n` is the extended form and must fail the same way.
    #[test]
    fn a_cme_error_fails_the_named_step() {
        let mut script = stock_script();
        script[1] = ("AT+CIND=?", "\r\n+CME ERROR: 4\r\n");
        let mut ag = FakeAg::new(script);
        let err = establish_hfp_with(&mut ag, false).expect_err("+CME ERROR must fail the SLC");
        assert_eq!(err.step, "AT+CIND=?");
        assert_eq!(err.why, "+CME ERROR: 4");
    }

    /// A gateway that hangs up mid-dialogue must fail the step, not spin to the total budget.
    #[test]
    fn a_closed_link_fails_the_step_it_closed_on() {
        let mut script = stock_script();
        script[0] = ("AT+BRSF=63", ""); // nothing back at all -> our FakeAg reads EOF
        let mut ag = FakeAg::new(script);
        let started = Instant::now();
        let err = establish_hfp_with(&mut ag, false).expect_err("EOF must fail");
        assert_eq!(err.step, "AT+BRSF");
        assert_eq!(err.why, "the gateway closed the link");
        assert!(started.elapsed() < STEP_BUDGET, "must not wait out the step budget on EOF");
    }

    /// Framing tolerance: a gateway that uses bare `\n`, or crams the whole response into one
    /// packet with no leading CRLF, must still parse. Both shapes are in the wild.
    #[test]
    fn sloppy_line_framing_still_parses() {
        let mut script = stock_script();
        script[0] = ("AT+BRSF=63", "+BRSF: 879\nOK\n");
        script[5] = ("AT+CHLD=?", "+CHLD: (0,1,2)\rOK\r");
        let mut ag = FakeAg::new(script);
        let up = establish_hfp_with(&mut ag, false).expect("SLC must establish");
        assert_eq!(up.slc.ag_features, Some(879));
        assert_eq!(up.slc.chld.as_deref(), Some("(0,1,2)"));
    }

    /// An unparseable `+BRSF` must fail at `AT+BRSF` rather than silently continuing with zero
    /// features — zero would also skip `AT+CHLD=?` and quietly produce a different dialogue.
    #[test]
    fn an_unparseable_brsf_fails_rather_than_defaulting_to_zero() {
        let mut script = stock_script();
        script[0] = ("AT+BRSF=63", "\r\n+BRSF: lots\r\n\r\nOK\r\n");
        let mut ag = FakeAg::new(script);
        let err = establish_hfp_with(&mut ag, false).expect_err("a junk +BRSF must fail");
        assert_eq!(err.step, "AT+BRSF");
        assert!(err.why.contains("+BRSF"), "why: {}", err.why);
    }

    #[test]
    fn the_hsp_path_says_nothing_at_all() {
        let up = establish_hsp();
        assert_eq!(up.slc.path, Path::Hsp);
        assert_eq!(up.slc.ag_features, None);
        assert!(up.pending.is_empty() && up.carry.is_empty());
        assert_eq!(Path::Hsp.as_str(), "HSP");
        assert_eq!(Path::Hfp.as_str(), "HFP");
    }

    #[test]
    fn indicator_names_come_out_in_order() {
        let line = "+CIND: (\"call\",(0,1)),(\"callsetup\",(0-3)),(\"battchg\",(0-5))";
        assert_eq!(parse_indicator_names(line), ["call", "callsetup", "battchg"]);
        // An unreadable response is an empty list, never a panic or a failed SLC.
        assert!(parse_indicator_names("+CIND: garbage").is_empty());
        assert!(parse_indicator_names("+CIND: (\"unterminated").is_empty());
    }

    /// `+CIEV: 6,4` is the line stock's phone actually sent. It means `battchg = 4`, and that is
    /// only readable with the indicator list from `AT+CIND=?`.
    #[test]
    fn ciev_is_rendered_against_the_indicator_names() {
        let names: Vec<String> =
            ["call", "callsetup", "service", "signal", "roam", "battchg", "callheld"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        assert_eq!(describe_unsolicited("+CIEV: 6,4", &names), "+CIEV: 6,4 (battchg = 4)");
        // Out of range, zero (the list is 1-based) and malformed all fall back to the raw line.
        assert_eq!(describe_unsolicited("+CIEV: 99,1", &names), "+CIEV: 99,1");
        assert_eq!(describe_unsolicited("+CIEV: 0,1", &names), "+CIEV: 0,1");
        assert_eq!(describe_unsolicited("+CIEV: x", &names), "+CIEV: x");
        assert_eq!(describe_unsolicited("+BSIR: 1", &names), "+BSIR: 1");
        // With no names learned (the HSP path) it must still not panic or mis-index.
        assert_eq!(describe_unsolicited("+CIEV: 6,4", &[]), "+CIEV: 6,4");
    }

    #[test]
    fn unsolicited_classification_covers_what_the_ag_volunteers() {
        for l in ["+CIEV: 1,0", "+BSIR: 0", "RING", "+CLIP: \"x\",129", "+VGS=7", "+BCS: 2"] {
            assert!(is_unsolicited(l), "{l} must be unsolicited");
        }
        for l in ["OK", "ERROR", "+BRSF: 879", "+CIND: 0,0", "+CHLD: (0,1)"] {
            assert!(!is_unsolicited(l), "{l} must NOT be unsolicited");
        }
    }

    #[test]
    fn the_path_lever_parses_both_values_and_nothing_else() {
        assert_eq!(parse_forced_path("hfp"), Some(Path::Hfp));
        assert_eq!(parse_forced_path(" HSP\n"), Some(Path::Hsp));
        assert_eq!(parse_forced_path("auto"), None);
        assert_eq!(parse_forced_path(""), None);
        assert_eq!(parse_forced_path("1"), None);
    }

    /// The wideband script: `AT+BRSF=191`, then `AT+BAC=1,2` BEFORE `AT+CIND=?`, then stock's
    /// dialogue unchanged. Same Pixel answers as the stock capture.
    fn wbs_script() -> Vec<(&'static str, &'static str)> {
        let mut s = stock_script();
        s[0] = ("AT+BRSF=191", "\r\n+BRSF: 879\r\n\r\nOK\r\n");
        s.insert(1, ("AT+BAC=1,2", "\r\nOK\r\n"));
        s
    }

    /// THE wideband test. `AT+BAC` must land between the feature exchange and `AT+CIND=?` — HFP 1.6
    /// §4.2 puts it there, and `FakeAg` asserts the position by failing on the first command that
    /// arrives out of order.
    #[test]
    fn the_wideband_lever_adds_bac_after_brsf_and_before_cind() {
        let mut ag = FakeAg::new(wbs_script());
        let up = establish_hfp_with(&mut ag, true).expect("the wideband SLC must establish");
        assert!(up.slc.wbs, "the SLC must record that wideband was offered");
        assert_eq!(
            ag.sent,
            [
                "AT+BRSF=191",
                "AT+BAC=1,2",
                "AT+CIND=?",
                "AT+CMER=3,0,0,1",
                "AT+CLIP=1",
                "AT+CCWA=1",
                "AT+CHLD=?",
                "AT+CIND?",
            ]
        );
    }

    /// With the lever OFF not one byte may differ from the dialogue proven against this phone: 63,
    /// and no `AT+BAC` at any position.
    #[test]
    fn the_lever_off_dialogue_is_byte_identical_to_stocks() {
        let mut ag = FakeAg::new(stock_script());
        let up = establish_hfp_with(&mut ag, false).expect("SLC must establish");
        assert!(!up.slc.wbs);
        assert_eq!(ag.sent[0], "AT+BRSF=63");
        assert!(!ag.sent.iter().any(|c| c.starts_with("AT+BAC")), "sent: {:?}", ag.sent);
    }

    /// An AG without `+BRSF` bit 9 must NOT be sent `AT+BAC`: HFP 1.7 §4.2.1 makes the exchange
    /// conditional on both sides, and a gateway that answers `ERROR` would abort an otherwise
    /// complete SLC — the same failure `AT+CHLD=?` is guarded against.
    #[test]
    fn bac_is_skipped_when_the_ag_does_not_claim_codec_negotiation() {
        let mut script = wbs_script();
        // 879 - 512 = 367: bit 9 cleared, everything else (including three-way) intact.
        script[0] = ("AT+BRSF=191", "\r\n+BRSF: 367\r\n\r\nOK\r\n");
        script.remove(1);
        let mut ag = FakeAg::new(script);
        let up = establish_hfp_with(&mut ag, true).expect("SLC must still establish");
        assert_eq!(up.slc.ag_features, Some(367));
        assert!(!up.slc.wbs, "nothing was offered, so nothing may be recorded as offered");
        assert!(!ag.sent.iter().any(|c| c.starts_with("AT+BAC")), "sent: {:?}", ag.sent);
    }

    /// A `+BCS` that arrives DURING the dialogue must be surfaced as unsolicited, not mistaken for
    /// the answer to the command in flight — the hold loop is what replies to it, and it can only do
    /// that if the line reaches it.
    #[test]
    fn a_bcs_during_the_slc_is_carried_out_in_pending() {
        let mut script = wbs_script();
        script[3] = ("AT+CMER=3,0,0,1", "\r\n+BCS: 2\r\n\r\nOK\r\n");
        let mut ag = FakeAg::new(script);
        let up = establish_hfp_with(&mut ag, true).expect("SLC must establish");
        assert!(up.pending.iter().any(|l| l == "+BCS: 2"), "pending: {:?}", up.pending);
    }

    #[test]
    fn the_wideband_bitmap_is_stocks_plus_exactly_the_codec_bit() {
        assert_eq!(HF_SUPPORTED_FEATURES_WBS, 191);
        assert_eq!(HF_SUPPORTED_FEATURES_WBS, HF_SUPPORTED_FEATURES | 128);
        assert_eq!(HF_FEATURE_CODEC_NEGOTIATION, 1 << 7);
        // The Pixel's own `+BRSF: 879` must read as "claims codec negotiation".
        assert_eq!(AG_FEATURE_CODEC_NEGOTIATION, 512);
        assert_ne!(879 & AG_FEATURE_CODEC_NEGOTIATION, 0);
        assert_eq!(367 & AG_FEATURE_CODEC_NEGOTIATION, 0);
    }

    #[test]
    fn bcs_ids_parse_and_nothing_else_does() {
        assert_eq!(parse_bcs("+BCS: 2"), Some(2));
        assert_eq!(parse_bcs("+BCS:1"), Some(1));
        assert_eq!(parse_bcs("+BCS: 3,1"), Some(3), "a trailing field must not defeat the id");
        assert_eq!(parse_bcs("+BCS: x"), None);
        assert_eq!(parse_bcs("+BCS:"), None);
        assert_eq!(parse_bcs("+BVRA: 1"), None);
        assert!(is_unsolicited("+BCS: 2"), "the SLC must not read it as an answer");
    }

    /// Every reachable `+BCS` must produce exactly one command: the AG is stopped, waiting, and will
    /// not open (e)SCO until it gets one.
    #[test]
    fn the_codec_answer_is_never_silence() {
        for id in [0u8, 1, 2, 3, 255] {
            for wbs in [false, true] {
                for narrowed in [false, true] {
                    let c = choose_codec(id, wbs, narrowed).command();
                    assert!(
                        c.starts_with("AT+BCS=") || c.starts_with("AT+BAC="),
                        "id {id} wbs {wbs} narrowed {narrowed} -> {c:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_ag_choosing_msbc_is_accepted_only_while_wideband_is_live() {
        assert_eq!(choose_codec(CODEC_MSBC, true, false), CodecChoice::Use(CODEC_MSBC));
        assert_eq!(choose_codec(CODEC_MSBC, true, false).command(), "AT+BCS=2");
        // Lever off — we never offered it, so accepting would be a lie about a socket we never
        // configured.
        assert_eq!(choose_codec(CODEC_MSBC, false, false), CodecChoice::NarrowToCvsd);
        // Already fallen back once: a second +BCS: 2 must not walk us back in.
        assert_eq!(choose_codec(CODEC_MSBC, true, true), CodecChoice::NarrowToCvsd);
        assert_eq!(CodecChoice::NarrowToCvsd.command(), "AT+BAC=1");
    }

    /// CVSD is accepted unconditionally — it is the kernel's default air mode, there is nothing to
    /// fail at, and refusing it is the one answer that leaves a call with no codec at all.
    #[test]
    fn cvsd_is_always_accepted() {
        for (wbs, narrowed) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(choose_codec(CODEC_CVSD, wbs, narrowed), CodecChoice::Use(CODEC_CVSD));
        }
        assert_eq!(choose_codec(CODEC_CVSD, true, false).command(), "AT+BCS=1");
    }

    /// HFP 1.7 §4.11.3: an id we never offered is answered with a fresh `AT+BAC`, not with silence
    /// and not with a different id.
    #[test]
    fn an_unoffered_codec_id_re_offers_the_list() {
        assert_eq!(choose_codec(3, true, false), CodecChoice::OfferBoth); // LC3-SWB, HFP 1.9
        assert_eq!(choose_codec(3, true, false).command(), "AT+BAC=1,2");
        assert_eq!(choose_codec(0, true, false), CodecChoice::OfferBoth);
        // …but once narrowed, or with the lever off, the honest list is CVSD alone.
        assert_eq!(choose_codec(3, true, true), CodecChoice::NarrowToCvsd);
        assert_eq!(choose_codec(3, false, false), CodecChoice::NarrowToCvsd);
    }

    /// The feature bitmap we advertise over AT must be the same 63 the SDP record claims, or the
    /// phone sees a device that says one thing in its record and another on the wire.
    #[test]
    fn the_advertised_feature_bitmap_matches_the_sdp_record() {
        assert_eq!(HF_SUPPORTED_FEATURES, 63);
        let rec = bt_common::sdp_record::HandsFreeRecord {
            handle: 0,
            rfcomm_channel: bt_common::sdp_record::HFP_HF_RFCOMM_CHANNEL,
            name: "Hands-Free",
            profile_version: 0x0107,
            supported_features: 0x003F,
        }
        .encode();
        assert_eq!(&rec[rec.len() - 6..], &[0x09, 0x03, 0x11, 0x09, 0x00, 0x3f]);
        assert_eq!(0x003Fu16 as u32, HF_SUPPORTED_FEATURES);
    }

    /// The indicator list the stock Pixel actually returned, so index→name resolution is tested
    /// against the real ordering rather than the spec's example ordering.
    fn stock_indicators() -> Vec<String> {
        parse_indicator_names(
            "+CIND: (\"call\",(0,1)),(\"callsetup\",(0-3)),(\"service\",(0-1)),\
             (\"signal\",(0-5)),(\"roam\",(0,1)),(\"battchg\",(0-5)),(\"callheld\",(0-2))",
        )
    }

    /// An inbound call, start to finish, exactly as the AG reports it: setup=1, then call=1 when it
    /// is answered, then both back to 0.
    #[test]
    fn an_answered_inbound_call_walks_ringing_active_ended() {
        let ind = stock_indicators();
        let mut t = CallTracker::new(&ind);
        assert!(!t.audio_wanted());
        assert_eq!(t.observe("+CIEV: 2,1"), [CallEvent::IncomingRinging]);
        assert!(t.audio_wanted(), "a ringing call already needs the mic seam up");
        assert_eq!(t.observe("RING"), [], "RING must not duplicate the indicator transition");
        assert_eq!(t.observe("+CIEV: 1,1"), [CallEvent::CallActive]);
        assert_eq!(t.observe("+CIEV: 2,0"), [], "setup clearing into an active call is not an abandonment");
        assert!(t.audio_wanted());
        assert_eq!(t.observe("+CIEV: 1,0"), [CallEvent::CallEnded]);
        assert!(!t.audio_wanted());
    }

    /// The failure mode the `call == 0` guard exists for: a call that rings and is never answered
    /// must not be reported as ended-after-active.
    #[test]
    fn a_missed_call_is_an_abandonment_not_an_ended_call() {
        let ind = stock_indicators();
        let mut t = CallTracker::new(&ind);
        assert_eq!(t.observe("+CIEV: 2,1"), [CallEvent::IncomingRinging]);
        assert_eq!(t.observe("+CIEV: 2,0"), [CallEvent::SetupAbandoned]);
        assert!(!t.audio_wanted());
    }

    #[test]
    fn an_outgoing_call_walks_dialing_alerting_active() {
        let ind = stock_indicators();
        let mut t = CallTracker::new(&ind);
        assert_eq!(t.observe("+CIEV: 2,2"), [CallEvent::OutgoingDialing]);
        assert_eq!(t.observe("+CIEV: 2,3"), [CallEvent::OutgoingAlerting]);
        assert_eq!(t.observe("+CIEV: 1,1"), [CallEvent::CallActive]);
        assert_eq!(t.observe("+CIEV: 7,1"), [CallEvent::CallHeld]);
        assert_eq!(t.observe("+CIEV: 7,0"), [CallEvent::CallResumed]);
    }

    /// Indicators we do not act on must move state for nobody. `+CIEV: 6,4` is `battchg` on this
    /// phone and index 6 is inside the `callsetup`/`callheld` numeric range of other gateways —
    /// resolving by NAME is what keeps that from becoming a phantom call.
    #[test]
    fn unrelated_indicators_produce_no_events() {
        let ind = stock_indicators();
        let mut t = CallTracker::new(&ind);
        for l in ["+CIEV: 6,4", "+CIEV: 4,3", "+CIEV: 3,1", "+CIEV: 5,0", "+BSIR: 1"] {
            assert_eq!(t.observe(l), [], "{l} must not be a call transition");
        }
        assert!(!t.audio_wanted());
    }

    /// A gateway whose `+CIND=?` we could not parse must degrade to no classification — never to a
    /// wrong one derived from assumed indices.
    #[test]
    fn an_unknown_indicator_map_classifies_nothing() {
        let mut t = CallTracker::new(&[]);
        assert_eq!(t.observe("+CIEV: 1,1"), []);
        assert_eq!(t.observe("+CIEV: 2,1"), []);
        assert!(!t.audio_wanted());
        // …but RING still works, which is the whole reason the latch exists.
        assert_eq!(t.observe("RING"), [CallEvent::IncomingRinging]);
        assert_eq!(t.observe("RING"), [], "the latch must not log one line per ring");
    }

    /// The Assistant path. gearhead calls `startVoiceRecognition` BEFORE `startBluetoothSco`
    /// (`kxr.java:118-150`), so `+BVRA: 1` is the earliest possible signal that audio is coming.
    #[test]
    fn voice_recognition_toggles_and_wants_audio() {
        let ind = stock_indicators();
        let mut t = CallTracker::new(&ind);
        assert_eq!(t.observe("+BVRA: 1"), [CallEvent::VoiceRecognition(true)]);
        assert!(t.audio_wanted());
        assert_eq!(t.observe("+BVRA: 1"), [], "a repeat is not a transition");
        assert_eq!(t.observe("+BVRA: 0"), [CallEvent::VoiceRecognition(false)]);
        assert!(!t.audio_wanted());
        // HFP 1.9 adds `+BVRA: <vrect>,<state>` — the first field is still the enable flag.
        assert_eq!(t.observe("+BVRA: 1,2"), [CallEvent::VoiceRecognition(true)]);
    }

    #[test]
    fn bvra_is_classified_as_unsolicited() {
        assert!(is_unsolicited("+BVRA: 1"));
        assert!(is_unsolicited("RING"));
        assert!(!is_unsolicited("OK"));
        assert!(!is_unsolicited("+BRSF: 879"));
    }

    #[test]
    fn clip_yields_the_bare_number() {
        assert_eq!(parse_clip("+CLIP: \"+441234567890\",145"), Some("+441234567890".into()));
        assert_eq!(parse_clip("+CLIP: \"5551234\",129,,,\"Alice\""), Some("5551234".into()));
        assert_eq!(parse_clip("+CLIP: \"\",128"), None, "a withheld number is not a number");
        assert_eq!(parse_clip("+CIEV: 1,1"), None);
    }

    /// The exact rendering asserted, because the log line is the deliverable on a bench.
    #[test]
    fn the_voice_recognition_log_line_is_the_agreed_wording() {
        assert_eq!(
            CallEvent::VoiceRecognition(true).describe(),
            "phone started Bluetooth voice recognition (Assistant) — SCO audio armed"
        );
    }
}
