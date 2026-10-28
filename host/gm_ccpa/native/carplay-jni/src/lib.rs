//! JNI bridge from the Android app to the proven CarPlay receiver core in `ccpa_custom`.
//!
//! **Why this exists.** API 32 does not expose X25519 or Ed25519, so `pair-setup` M5/M6 and
//! `pair-verify` cannot be done in Kotlin. Everything else the control plane needs — the RTSP state
//! machine, the ChaCha20-Poly1305 channel, `/auth-setup`, SETUP/RECORD — is ~19k lines of protocol
//! that has already been paid for once. This wraps it rather than reimplementing it.
//!
//! **The seam is `ControlServer::feed()`**, which is genuinely sans-IO: Kotlin owns the single
//! control TCP connection and hands bytes in and out. Note that the *data plane* is different —
//! `receiver::session` binds its own eight sockets and spawns its own threads, so once a session
//! starts, Rust owns those regardless. A later step should move the listener itself into Rust to
//! recover `carplayd`'s connection-hijack behaviour and its TCP_KEEPIDLE tuning, neither of which
//! Java sockets can express.
//!
//! **Lifetime note.** `ControlServer<'a, P, S>` borrows one `Identity`. That is the whole `'a`, and
//! a `OnceLock` makes it `&'static` — no crate fork, no wrapper gymnastics.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, TryLockError};
use std::time::{Duration, Instant};

use jni::objects::{GlobalRef, JByteArray, JClass, JObject, JString};
use jni::sys::{jbyteArray, jlong};
use jni::{JNIEnv, JavaVM};

use mfi::auth_client::MfiSigner;
use pairing::setup::PeerSaver;
use pairing::verify::{Identity, PeerStore};
use receiver::server::ControlServer;

static JVM: OnceLock<JavaVM> = OnceLock::new();
/// The `:9112` mic-ingest listener (F136), set in `JNI_OnLoad` ONLY once its bind succeeded: the
/// auth token lives inside it, and `nativeMicAuth` reads it out only while the listener is serving.
/// Nothing logs the token.
static MIC_SEAM: OnceLock<receiver::uplink::ControlListener> = OnceLock::new();
static IDENTITY: OnceLock<Identity> = OnceLock::new();

// The one live native session, keyed by a generation token handed back to Kotlin as the `jlong`
// handle. All access is serialized through this mutex, and that is what makes the handle safe: a
// stale generation's feed/destroy becomes a no-op instead of a use-after-free on a freed Box. Only
// one Native ever exists (the receiver is single-session), so a slot — not a map — is the right
// shape. Crucially the incumbent is dropped *under the lock* (nativeInit/nativeDestroy): AvSession's
// Drop clears process-global event/sink state, so a deferred drop that ran after a newer session had
// started would clear the NEW session's channels — the exact reconnect bug this milestone fixes.
static CORE: Mutex<Option<(u64, Native)>> = Mutex::new(None);
static NEXT_GEN: AtomicU64 = AtomicU64::new(1);

/// Lock CORE, recovering from poisoning (a panic caught by `catch_unwind` inside `feed` while the
/// guard was held would otherwise wedge every later call). The data is still consistent because the
/// panic unwinds out of `feed` without leaving a half-written slot.
fn core() -> MutexGuard<'static, Option<(u64, Native)>> {
    CORE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Returned by `nativeInit` when CORE could not be taken inside [`INIT_LOCK_BUDGET`].
///
/// Distinct from 0 ("no core; the connection is closed (no fallback)", v5.9 UC1) because the cause
/// differs: a busy core is transient, so the caller closes this connection and the phone redials into
/// a core that is by then free. Both answers close the connection; there is no fallback path.
const INIT_BUSY: jlong = -1;

/// How long a NEW control connection will wait for the incumbent to release CORE.
///
/// `nativeFeed` holds CORE across the whole of `ControlServer::feed`, which on the control path can
/// sit in the synchronous MFi relay for the length of its budget (12 s cert / 15 s sign). Apple's
/// `_HijackHTTPServerConnections` reconnect is the phone's NORMAL path, so on every hijack the
/// newcomer used to block here for that entire window with nothing written back — iOS gives up and
/// dials again, which is the generation churn the hijack exists to END. Three seconds is longer than
/// any healthy install (the work either side of the lock is microseconds) and far short of iOS's
/// patience, so a timeout here means the incumbent is genuinely mid-MFi, not that we were unlucky.
const INIT_LOCK_BUDGET: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------------------------
// stdout/stderr -> logcat
//
// The receiver core logs with ~138 println!/eprintln! calls. On Android those go to /dev/null, and
// combined with the nine documented silent session-killers that turns a debuggable problem into an
// undebuggable one. Pipe fd 1/2 into __android_log_write before anything else runs.
//
// ONE pipe for both streams, on purpose (A-F2/A-X1, 2026-10-08): two reader threads would lose the
// cross-stream ordering the capture sequences in docs/12 rely on, and the receiver crate prints
// SUCCESS lines on stderr ("[receiver] pair-verify OK"), so stderr cannot be mapped to WARN
// wholesale. Severity instead rides on the line: this crate's own failure sites write through
// [`jni_log`] with a `[jni] E: ` / `[jni] W: ` marker, and the reader maps the marker to the logcat
// priority ([`log_prio`]) so `adb logcat -s NETPROBE:W` surfaces a peer-store write failure, an
// init failure or a JNI_OnLoad panic. Everything else stays INFO. Receiver-side severity (its own
// eprintln! of real faults) is an upstream matter; the crate is static.
// ---------------------------------------------------------------------------------------------
/// logcat priority for one pipe line: 6 (ERROR) for `[jni] E: `, 5 (WARN) for `[jni] W: `, else 4.
fn log_prio(line: &[u8]) -> i32 {
    if line.starts_with(b"[jni] E: ") {
        6
    } else if line.starts_with(b"[jni] W: ") {
        5
    } else {
        4
    }
}

/// This crate's own failure lines: `level` is 'E' or 'W'. Formatted into ONE buffer and written with
/// a single `write_all`, because `eprintln!` on the unbuffered stderr issues one write per format
/// piece and interleaves with the receiver's stdout lines on the shared pipe (A-X1).
fn jni_log(level: char, msg: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let line = format!("[jni] {level}: {msg}\n");
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}

fn redirect_stdio_to_logcat() {
    unsafe {
        let mut fds = [0i32; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return;
        }
        libc::dup2(fds[1], 1);
        libc::dup2(fds[1], 2);
        libc::close(fds[1]);
        let read_fd = fds[0];
        std::thread::spawn(move || {
            let tag = std::ffi::CString::new("NETPROBE").unwrap();
            let mut buf = [0u8; 2048];
            let mut line = Vec::<u8>::new();
            loop {
                let n = libc::read(read_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len());
                if n < 0 {
                    // EINTR or a transient error — keep draining, never exit. If this thread died the
                    // pipe would fill (64 KB) and every println!/eprintln! on the native side would
                    // block forever, freezing the receiver — the opposite of what this exists to do.
                    continue;
                }
                if n == 0 {
                    break; // true EOF: only if fd 1/2 were closed, which never happens in-process.
                }
                for &b in &buf[..n as usize] {
                    if b == b'\n' {
                        // Prefix so it sorts with the Kotlin side's subsystem fields.
                        let prio = log_prio(&line);
                        let mut out = b"[rust ] ".to_vec();
                        out.extend_from_slice(&line);
                        if let Ok(c) = std::ffi::CString::new(out) {
                            android_log(prio, &tag, &c);
                        }
                        line.clear();
                    } else {
                        line.push(b);
                    }
                }
            }
        });
    }
}

unsafe fn android_log(prio: i32, tag: &std::ffi::CStr, msg: &std::ffi::CStr) {
    extern "C" {
        fn __android_log_write(prio: i32, tag: *const libc::c_char, text: *const libc::c_char) -> i32;
    }
    __android_log_write(prio, tag.as_ptr(), msg.as_ptr());
}

// ---------------------------------------------------------------------------------------------
// Peer store — controller id -> Ed25519 long-term public key, persisted.
//
// Without persistence every session re-runs pair-setup; with a wrong one, pair-verify fails and you
// debug crypto that is fine. `carplayd` uses DiskPeers for exactly this reason.
// ---------------------------------------------------------------------------------------------
/// F141: hard cap on stored pairings. Pair-setup runs with the fixed "3939" setup code, so any
/// hotspot client can complete it and append a peer; uncapped, that grows the in-memory Vec and the
/// on-disk store without bound. Eviction is oldest-SAVED first (save_peer's retain+push keeps the Vec
/// in save order). Known cost: 16 hostile pairings evict the real phone, which must then re-pair.
const MAX_PEERS: usize = 16;

struct AppPeers {
    path: std::path::PathBuf,
    peers: Vec<(Vec<u8>, [u8; 32])>,
}

impl AppPeers {
    fn load(path: std::path::PathBuf) -> Self {
        let mut peers = Vec::new();
        if let Ok(data) = std::fs::read(&path) {
            // [u8 idLen][id][32 ltpk] repeated
            let mut i = 0usize;
            while i < data.len() {
                let id_len = data[i] as usize;
                if i + 1 + id_len + 32 > data.len() {
                    break;
                }
                let id = data[i + 1..i + 1 + id_len].to_vec();
                let mut ltpk = [0u8; 32];
                ltpk.copy_from_slice(&data[i + 1 + id_len..i + 1 + id_len + 32]);
                peers.push((id, ltpk));
                i += 1 + id_len + 32;
            }
        }
        println!("[jni] peer store: {} pairing(s) loaded from {:?}", peers.len(), path);
        AppPeers { path, peers }
    }

    fn persist(&self) {
        let mut out = Vec::new();
        for (id, ltpk) in &self.peers {
            if id.len() > 255 {
                continue;
            }
            out.push(id.len() as u8);
            out.extend_from_slice(id);
            out.extend_from_slice(ltpk);
        }
        // Atomic write: a crash mid-`fs::write` truncates the store and drops pairings (forced
        // re-pair, and the "debug crypto that is fine" symptom). Write to a temp then rename, which
        // is atomic on the same filesystem.
        let tmp = self.path.with_extension("bin.tmp");
        if let Err(e) = std::fs::write(&tmp, &out) {
            jni_log('E', format_args!("peer store write failed: {e}"));
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            jni_log('E', format_args!("peer store rename failed: {e}"));
        }
    }
}

impl PeerStore for AppPeers {
    fn find_peer(&self, id: &[u8]) -> Option<[u8; 32]> {
        self.peers.iter().find(|(k, _)| k == id).map(|(_, v)| *v)
    }
}

impl PeerSaver for AppPeers {
    fn save_peer(&mut self, id: &[u8], ltpk: [u8; 32]) {
        self.peers.retain(|(k, _)| k != id);
        // `while`, not `if`: a store loaded from a pre-cap file may hold more than MAX_PEERS.
        while self.peers.len() >= MAX_PEERS {
            let (old, _) = self.peers.remove(0);
            println!("[jni] peer store full ({MAX_PEERS}) — evicted oldest pairing ({}-byte controller id)", old.len());
        }
        self.peers.push((id.to_vec(), ltpk));
        println!("[jni] pairing saved ({}-byte controller id) — pair-verify can now fast-path", id.len());
        self.persist();
    }
}

// ---------------------------------------------------------------------------------------------
// MFi signer — calls back into the Kotlin OCBM CH_MFI relay, which is already device-proven
// (945-byte certificate, 128-byte RSA-1024 signature).
// ---------------------------------------------------------------------------------------------
struct RemoteMfiSigner {
    relay: GlobalRef,
    /// Use the relay's short-budget methods. Set for the iAP2 tunnel's signer, clear for the control
    /// server's. The tunnel runs its chip ops inside `ControlServer::feed` with CORE and SESSION
    /// held, so the phone's `POST /command` gets no HTTP reply until they return — against a
    /// phone-side request timeout we have never measured (no capture, no spec; the nearest sourced
    /// figure is CarKit's disassembly-confirmed 30 s `timeoutInterval` — see audit 4.8 verdict).
    /// The asymmetry is the real point: the tunnel can afford to fail fast (Zero-Ack, its own 120 s
    /// retry budget); the control channel cannot afford to be late.
    fast: bool,
}

// No `unsafe impl Send` needed: RemoteMfiSigner holds only a GlobalRef, and jni 0.21 already
// declares JObject<'static> and JavaVM as Send, so it is auto-Send. `Native` below is auto-Send
// too — ControlServer's `Box<dyn SessionDelegate>` IS Send, via the `SessionDelegate: Send`
// supertrait (receiver `session.rs`).

impl RemoteMfiSigner {
    fn call(&mut self, method: &str, sig: &str, digest: Option<&[u8]>) -> io::Result<Vec<u8>> {
        let vm = JVM.get().ok_or_else(|| io::Error::other("JNI_OnLoad never ran"))?;
        // Attach per call: these can arrive on Rust-created threads the JVM knows nothing about.
        // AttachGuard detaches on drop, so per-session reader threads do not leak registrations.
        let mut env = vm
            .attach_current_thread()
            .map_err(|e| io::Error::other(format!("attach: {e}")))?;

        let res = match digest {
            None => env.call_method(&self.relay, method, sig, &[]),
            Some(d) => match env.byte_array_from_slice(d) {
                Ok(arr) => env.call_method(&self.relay, method, sig, &[(&arr).into()]),
                Err(e) => {
                    // This can fail with a Java exception pending (e.g. OOM). Clear it here too, or
                    // the next JNI call on this thread aborts the VM at an unrelated site.
                    if env.exception_check().unwrap_or(false) {
                        let _ = env.exception_clear();
                    }
                    return Err(io::Error::other(format!("byte array: {e}")));
                }
            },
        };

        // A pending Java exception left set aborts the VM on the next JNI call, at an unrelated site.
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
            return Err(io::Error::other(format!("{method} threw")));
        }
        let obj = res.map_err(|e| io::Error::other(format!("{method}: {e}")))?;
        let jobj = obj.l().map_err(|e| io::Error::other(format!("{method} return: {e}")))?;
        if jobj.is_null() {
            return Err(io::Error::other(format!("{method} returned null")));
        }
        let arr = JByteArray::from(jobj);
        env.convert_byte_array(&arr)
            .map_err(|e| io::Error::other(format!("convert: {e}")))
    }
}

impl MfiSigner for RemoteMfiSigner {
    fn copy_certificate(&mut self) -> io::Result<Vec<u8>> {
        let m = if self.fast { "copyCertificateFast" } else { "copyCertificate" };
        let cert = self.call(m, "()[B", None)?;
        println!("[jni] MFi certificate via OCBM relay: {} bytes", cert.len());
        Ok(cert)
    }

    fn create_signature(&mut self, digest: &[u8]) -> io::Result<Vec<u8>> {
        if digest.len() != mfi::DIGEST_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("digest must be {} bytes, got {}", mfi::DIGEST_LEN, digest.len()),
            ));
        }
        let m = if self.fast { "createSignatureFast" } else { "createSignature" };
        let sig = self.call(m, "([B)[B", Some(digest))?;
        if sig.len() != mfi::SIG_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("signature must be {} bytes, got {}", mfi::SIG_LEN, sig.len()),
            ));
        }
        println!("[jni] MFi signature via OCBM relay: {} bytes", sig.len());
        Ok(sig)
    }
}

// ---------------------------------------------------------------------------------------------
// The native handle
// ---------------------------------------------------------------------------------------------
struct Native {
    server: ControlServer<'static, AppPeers, RemoteMfiSigner>,
}

// Native is auto-Send, so no `unsafe impl` is needed. CORE is a `static Mutex`, which is `Sync`
// only if its payload is `Send` — and it is: every field of ControlServer is Send, including the
// `Box<dyn SessionDelegate>`, because `SessionDelegate: Send` is declared as a supertrait. Do not
// add an `unsafe impl Send` back. The mutex argument that used to sit here justifies Sync, not
// Send, and a blanket impl would silence the checker for any future non-Send field.

static IDENTITY_PARAMS: OnceLock<(String, [u8; 32])> = OnceLock::new();

fn identity(pi: &str, ed_seed: [u8; 32]) -> &'static Identity {
    // IDENTITY is set-once for the process. If a later nativeInit passes a DIFFERENT pi/seed
    // (re-provisioning, cleared app data, a test harness), the original silently wins and pair-verify
    // then fails with valid-looking crypto against a stale pi — the "debug crypto that is fine" trap.
    // Surface it loudly instead of failing mysteriously.
    let first = IDENTITY_PARAMS.get_or_init(|| (pi.to_string(), ed_seed));
    if first.0 != pi || first.1 != ed_seed {
        jni_log('W', format_args!(
            "nativeInit identity (pi/seed) differs from the first init; the ORIGINAL \
             is reused for the process lifetime, so pair-verify will fail against the new pi. Restart \
             the app to change identity."
        ));
    }
    IDENTITY.get_or_init(|| Identity::new(pi.as_bytes().to_vec(), ed_seed))
}

#[no_mangle]
pub extern "system" fn JNI_OnLoad(vm: JavaVM, _reserved: *mut libc::c_void) -> jni::sys::jint {
    // Contained like every other boundary fn: since Rust 1.81 a panic unwinding out of an
    // `extern "system"` fn ABORTS the process, which is precisely what panic="unwind" was chosen
    // to avoid. Realistic source here is thread::spawn failing inside redirect_stdio_to_logcat.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    redirect_stdio_to_logcat();

    // Match av.rs's proven wireless spawn environment. These are not cosmetic:
    //
    //   CARPLAY_WIRELESS_METADATA gates BOTH `iAPChannelInfo = {}` in /info AND the iAP2 tunnel
    //   DETECT+SYN at RECORD. Without it iOS "never binds a handler for the command and 400s every
    //   iAPSendMessage regardless of its contents" (info.rs, device-observed 2026-07-22, 3/3 uniform
    //   400) — which is exactly the 400 Bad Request that was tearing our session down right after
    //   RECORD, and why our log had no [iap-tunnel] line where the working capture does.
    //
    //   CARPLAY_WIRELESS_AUDIO selects the 8-entry AAC set over the wired PCM default.
    //   CARPLAY_SESSION_MGMT declares sessionManagementInfo/stopSessionReasons, as the proven
    //   wireless session does.
    //
    // OCBM_FWD_ENC=0 — on-box decode. This app decodes A/V locally off the localhost seam
    // (:9001 Annex-B, :9002 ADTS), so session.rs must DECRYPT and forward decoded frames, NOT forward
    // the encrypted frames + key ("forward-encrypted" is the box's role, where a separate host
    // decodes).
    //
    // MUST be set explicitly. ccpa_custom @273aed1 flipped the default: session.rs went from
    // `std::env::var("OCBM_FWD_ENC").is_ok()` (absent → false → on-box decode) to
    // `levers::fwd_enc()`, which defaults forward-encrypted ON unless OCBM_FWD_ENC is explicitly
    // 0/false/off/empty. So leaving it unset now selects the OPPOSITE path — the consumers would
    // start-code-scan ChaCha20 ciphertext and render garbage (silent video+audio break). Set it to 0.
    std::env::set_var("OCBM_FWD_ENC", "0");
    std::env::set_var("CARPLAY_WIRELESS_METADATA", "1");
    std::env::set_var("CARPLAY_WIRELESS_AUDIO", "1");
    // sessionManagement — the gap against every proven wireless session.
    //
    // `ensure_av_layer` in `crates/vendor/wireless/src/av.rs` puts `("CARPLAY_SESSION_MGMT", "1")` as a literal in the `envs` vec it hands `spawn_detached` — set on every carplayd it spawns, no flag or branch gates it — so the 2026-07-25
    // capture that reaches A/V declares `sessionManagementInfo` in /info AND echoes "sessionManagement"
    // in the SETUP response. We declared neither, and `sessionManagementInfo` is one of the six keys in
    // Apple's own per-feature /info validation cluster (carEndpoint_validateInfoResponseKeyPresentForFeature,
    // docs/35:157-162) — the sibling of carEndpoint_validateEnabledFeaturesWithAccessory, which is
    // exactly what rejected us with -16720 InvalidParameter. The phone proposes `sessionManagement`
    // in its SETUP features[]; we were the only side not carrying it.
    //
    // This gates BOTH sides (info.rs:614 and session.rs:565), so /info must be regenerated with it set
    // or the declaration and the echo desynchronise — the failure mode this whole bug turned out to be.
    std::env::set_var("CARPLAY_SESSION_MGMT", "1");
    // Screen-path diagnostics: the on-box decrypt branch is the one the proven wireless capture never
    // takes (it runs OCBM_FWD_ENC). This dumps the raw opcode-1 body so the codec (avcC vs hvcC) is
    // visible in every capture before any parsing touches it.
    //
    // Left ON deliberately, and the volume is bounded at the consumer, not here: session.rs guards
    // the dump with `screen_dump && frames < 6`, so a session logs at most SIX `[screen] DUMP
    // frame#N` lines (~200 B each) and then nothing — the 2026-09-09 truck capture carries exactly
    // six. Reviewed 2026-09-10 against a proposal to gate it default-off as "per-frame log volume on
    // the A/V path"; it is not per-frame, and the six lines are the only in-band codec identity a
    // capture has. (This comment used to add "it is currently yielding a 0 B VideoConfig" — that
    // was the 2026-08 bug the dump was added for and has been fixed; the claim was stale.)
    std::env::set_var("CARPLAY_SCREEN_DUMP", "1");
    // (OCBM_FWD_ENC is set to "0" above — see that block. It used to be left unset; the ccpa_custom
    // default flip at @273aed1 means unset now selects forward-encrypted, so it must be set here.)

    // THE LEVERS — and this was the bug.
    //
    // /info is generated ahead of time by `infogen` with these levers set, but the RUNNING server
    // reads them again at SETUP to build `enabledFeatures` (session.rs:531-567). carplayd never has
    // this problem because it calls build_info(&load_device_config()) per connection, which applies
    // the YAML and sets the levers in one place. We ship a static /info, so nothing was setting them
    // here: /info advertised `hevcInfo` while enabledFeatures omitted "hevc".
    //
    // That is exactly 05_SESSION_FLOW §8 rule 1 — a negotiated feature not backed by real state —
    // and the phone said so: `Activate callback with err: -16720 kFigEndpointError_InvalidParameter`,
    // the same code the cornerMasks failure produced on 2026-08-02.
    //
    // These MUST stay identical to infogen's set.
    receiver::levers::set_hevc(true);          // backed by hevcInfo {} in /info
    receiver::levers::set_dpad(false);         // touch only -> displays[].features 0x0A
    receiver::levers::set_knob(false);
    receiver::levers::set_telephony(false);
    receiver::levers::set_altscreen(false);    // no type-111 cluster display in /info
    // ON for the resize experiment. iOS honours displays[].viewAreas ONLY when "viewAreas" is echoed
    // in the SETUP enabledFeatures (session.rs:664-666); with it off, the /info structure is present
    // and ignored, which is exactly the state this app shipped in. The backing structure is always
    // emitted, so the teardown risk runs the OTHER way (echoing a feature whose /info shape is
    // missing) — see tools/info_plist_viewareas.py for the declared geometry and why it is that one.
    receiver::levers::set_viewareas(true);

    // Prime the receiver's DECLARED view-area count.
    //
    // `events::switch_view_area` refuses any index >= `info::declared_view_area_count()`, and that
    // static is written ONLY inside `info::build_info` — which this app never calls, because it
    // serves a STATIC `/info` plist from assets instead of generating one. So iOS was doing
    // everything right (negotiating `viewAreas`, drawing the Dock button, sending
    // `requestViewArea index=1`) and OUR OWN core refused the answer with "only 1 area(s) declared".
    // Device-observed 2026-09-08: the button appeared and pressing it did nothing.
    //
    // Calling build_info once for its side effect is deliberate: it runs the same containment and
    // positivity validation on the rect that the generated path would, so a plist and a lever that
    // disagree are caught here rather than on the wire. The bytes are discarded — assets/info.bplist
    // remains what we actually serve. Keep the rect in step with tools/info_plist_viewareas.py.
    {
        let cfg = receiver::info::DeviceConfig {
            display_width: 2400,
            display_height: 960,
            main_view_area_2: Some(receiver::info::ViewArea2 {
                x: 188, y: 118, w: 1416, h: 842, initial: false,
            }),
            ..Default::default()
        };
        let _ = receiver::info::build_info(&cfg);
        eprintln!(
            "[jni] declared view areas primed: {}",
            receiver::info::declared_view_area_count()
        );
    }
    receiver::levers::set_cornermasks(false);

    // Mic uplink ingest. /info advertises audioInputFormats on type 100 (default / telephony /
    // speechRecognition), so the capability is declared — this is what backs it.
    //
    // F136 (2026-10-07): the port is loopback, and on this head unit every app holding INTERNET
    // shares that loopback. A per-process 128-bit token from getrandom gates it: the receiver closes
    // any connection whose first line is not `auth <hex>` in time, and only our own MicUplink learns
    // the token (`nativeMicAuth`). No token, no listener — fail closed, never open. And no listener,
    // no token (fix round 1): if another process already holds :9112, the bind fails here and the
    // token is never published, so MicUplink never sends it — or the driver's mic — to a squatter.
    match start_mic_ingest(&MIC_SEAM, "127.0.0.1:9112") {
        Ok(addr) => println!(
            "[jni] levers set (hevc=true, everything else off); mic uplink ingest on {addr} (auth token required)"
        ),
        Err(why) => eprintln!("[jni] mic uplink ingest NOT started: {why} — no Siri/call mic this process"),
    }
    // (The block that used to sit here argued CARPLAY_SESSION_MGMT must NOT be set. That was a
    // superseded single-variable experiment and it was WRONG — see the set_var above. Setting it is
    // what fixed the -16720 teardown, and with `sessionManagementInfo` baked into the shipped
    // info.bplist, NOT setting it re-creates the declaration-vs-echo desync. The two move together:
    // testing without it means regenerating the asset in the same change.)

    let _ = JVM.set(vm);
    println!("[jni] loaded — receiver core ready (stdout/stderr now go to logcat)");
    jni::sys::JNI_VERSION_1_6
    }))
    .unwrap_or_else(|_| {
        jni_log('E', format_args!("PANIC in JNI_OnLoad — library load continues, core may be unusable"));
        jni::sys::JNI_VERSION_1_6
    })
}

/// `nativeInit(pi, edSeed, infoPlist, peerFile, peerAddr, mfiRelay) -> handle`
#[no_mangle]
pub extern "system" fn Java_zeno_gmccpa_pair_NativeCore_nativeInit<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    pi: JString<'l>,
    ed_seed: JByteArray<'l>,
    info_plist: JByteArray<'l>,
    peer_file: JString<'l>,
    peer_addr: JString<'l>,
    mfi_relay: JObject<'l>,
) -> jlong {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let pi: String = env.get_string(&pi).map_err(|e| e.to_string())?.into();
        let peer_path: String = env.get_string(&peer_file).map_err(|e| e.to_string())?.into();
        let seed_v = env.convert_byte_array(&ed_seed).map_err(|e| e.to_string())?;
        if seed_v.len() != 32 {
            return Err(format!("ed seed must be 32 bytes, got {}", seed_v.len()));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&seed_v);
        let info = env.convert_byte_array(&info_plist).map_err(|e| e.to_string())?;
        let relay = env.new_global_ref(mfi_relay).map_err(|e| e.to_string())?;
        let peer: String = env.get_string(&peer_addr).map_err(|e| e.to_string())?.into();

        // Take the slot FIRST, bounded (see INIT_LOCK_BUDGET), before anything with a Drop side
        // effect exists. `AvSession::drop` -> `reset()` -> `events::clear()` + `clear_sinks()` act on
        // PROCESS-GLOBAL state (receiver session.rs:498-515, events.rs:428), so a session built here
        // and then dropped on the BUSY return would clear the INCUMBENT's live event channel and A/V
        // sinks — the mirror image of the deferred-drop bug described at CORE. Same reason
        // `set_remote_signer` below now waits: a generation that never installs must not replace the
        // signer the live incumbent's tunnel is still using.
        let deadline = Instant::now() + INIT_LOCK_BUDGET;
        let mut slot = loop {
            match CORE.try_lock() {
                Ok(g) => break g,
                Err(std::sync::TryLockError::Poisoned(e)) => break e.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        eprintln!(
                            "[jni] init BUSY: the incumbent generation held CORE for {:?} (it is \
                             inside ControlServer::feed, most likely MFi) — telling the caller to \
                             close this connection and let the phone redial",
                            INIT_LOCK_BUDGET
                        );
                        return Ok(INIT_BUSY);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        };

        // CARPLAY_SETUP_DUMP is deliberately NOT set. It writes one file per SETUP with a monotonic
        // counter and no cleanup, and SETUPs arrive throughout a session (05 §6 records 15 audio
        // SETUPs in one 30-minute run), so it grows without bound in the app's private dir — which is
        // unreadable from adb on this user build, so nobody would ever notice. It existed to read the
        // feature array the phone PROPOSES while diagnosing the -16720 activation failure; that is
        // settled, and the same information is now in the SETUP phase2 log lines.
        let peers = AppPeers::load(std::path::PathBuf::from(peer_path));
        // Give the iAP2 metadata tunnel the SAME relay the control server uses.
        //
        // In this deployment the gminfo37 head unit owns the CarPlay session and the CCPA box is only
        // the Bluetooth radio and the MFi coprocessor — so every chip operation, for /auth-setup and
        // for the tunnel alike, has to cross OCBM `CH_MFI` to the box. The tunnel used to call
        // /dev/i2c-1 directly and fail on every attempt, which cost the metadata/controls channel
        // entirely (gm_ccpa docs/12 Failure Point 6, docs/11 T5.3).
        // Installed once the CORE slot above is HELD, and deliberately not generation-gated —
        // `REMOTE_SIGNER` is a bare process-global in the receiver crate with no notion of our
        // generation token. That is safe ONLY because every generation's signer reaches the SAME
        // live relay: the GlobalRef is a per-connection `CarPlayRx.TracedRelay` (and
        // `OcbmProbe.mfiRelay()` allocates a fresh object per call), but both are thin wrappers that
        // delegate every chip op to the one process-scoped `ProcessMfiRelay`
        // (SessionCoreParts.kt), which resolves the CURRENT probe's client per call — so a superseded
        // generation's wrapper and the current one still sign through the same object. (Corrected
        // 2026-10-08, A-F3: this used to claim `mfiRelay()` itself returned a singleton.)
        //
        // IF THAT EVER CHANGES — a per-Activity or per-connection relay — this becomes a real
        // cross-wiring bug with nothing here to catch it: a hijacking reconnect would install a new
        // signer while the incumbent's in-flight `ControlServer::feed` still holds an Arc to the old
        // one, and the tunnel would sign against a relay belonging to a dead session. The fix then
        // is to key the signer by generation and have the receiver reject a stale one, not to add a
        // lock here. Flagged rather than pre-built because the receiver-side API does not exist yet
        // and inventing one for a hazard that is currently unreachable is the wrong trade.
        receiver::iap_tunnel::set_remote_signer(std::sync::Arc::new(std::sync::Mutex::new(
            RemoteMfiSigner { relay: relay.clone(), fast: true },
        )));
        let server = ControlServer::new(
            identity(&pi, seed),
            b"3939".to_vec(),   // the accessory setup code, as carplayd bakes it in
            peers,
            RemoteMfiSigner { relay, fast: false },
            info,
        )
        // verbose: control-plane only — one line per HTTP request plus the pair-setup/pair-verify/
        // auth-setup verdicts (server.rs `if self.verbose`). Tens of lines per session, nothing on
        // the A/V path, and `auth-setup (MFi-SAP) OK` / `FAILED` is the receiver-side verdict on the
        // CH_MFI relay. Reviewed 2026-09-10 alongside CARPLAY_SCREEN_DUMP and kept for that reason.
        .verbose(true);

        // Attach the A/V session delegate. Without it the server holds `NoSession`, SETUP has
        // nothing to answer with, and the phone hangs up right after auth-setup. AvSession binds its
        // OWN timing/event/keepalive/stream sockets and spawns its own threads — Kotlin cannot supply
        // them — and forwards decoded elementary streams to localhost:
        //   :9001 video  Annex-B      :9002 media audio  ADTS      :9003 voice audio  tagged AUs
        // which is exactly what MediaCodec consumes, so the decode path needs no JNI at all.
        let mut av = receiver::session::AvSession::new();
        // Log-only (4.5, 2026-10-08): the "SETUP negotiated" line prints `self.resolution`, which
        // carplayd sets from its DeviceConfig and nothing set here, so it read 0x0@0. These are the
        // panel and the maxFPS the static assets/info.bplist advertises (the same DeviceConfig
        // JNI_OnLoad primes above); nothing in the receiver reads them back except that line.
        {
            use receiver::session::SessionDelegate;
            av.set_resolution(2400, 960, 60);
        }
        match peer.parse::<std::net::SocketAddr>() {
            Ok(a) => { av.set_peer_addr(a); println!("[jni] session peer {a}"); }
            Err(e) => println!("[jni] peer addr {peer:?} unparsed ({e}) — session will use defaults"),
        }
        let server = server.session(Box::new(av));

        // Install into the already-held slot, dropping any incumbent (see CORE above), and return
        // the generation token as the handle. A stale token can no longer be dereferenced — it is
        // just an integer that no longer matches the slot.
        let gen = NEXT_GEN.fetch_add(1, Ordering::SeqCst);
        *slot = Some((gen, Native { server }));
        drop(slot);
        println!("[jni] ControlServer ready — pi={pi} (AvSession attached, gen {gen})");
        Ok(gen as jlong)
    }));
    match result {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => {
            jni_log('E', format_args!("init failed: {e}"));
            0
        }
        Err(_) => {
            jni_log('E', format_args!("init panicked"));
            0
        }
    }
}

/// `nativeFeed(handle, inBytes) -> outBytes` — the sans-IO seam. Kotlin owns the socket.
#[no_mangle]
pub extern "system" fn Java_zeno_gmccpa_pair_NativeCore_nativeFeed<'l>(
    env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    input: JByteArray<'l>,
) -> jbyteArray {
    // Contract: return an array (possibly empty) on success; return JNI null on any error — stale
    // handle, feed error, or panic — which Kotlin treats as "close this connection". An empty array
    // is a normal "no bytes to write back this round"; null is "this connection is dead".
    let null: jbyteArray = std::ptr::null_mut();
    if handle == 0 {
        return null;
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let inb = env.convert_byte_array(&input).ok()?;
        // Resolve the handle under the lock. A generation mismatch means this connection's core was
        // already superseded/destroyed — return no output rather than touching freed memory.
        let mut slot = core();
        match slot.as_mut() {
            Some((gen, native)) if *gen == handle as u64 => match native.server.feed(&inb) {
                Ok(out) => Some(out),
                Err(e) => {
                    // W, not E: the core rejecting a connection's bytes ends that connection, and
                    // Kotlin reports the end; a hostile or confused peer is not an app fault.
                    jni_log('W', format_args!("feed error: {e:?}"));
                    None
                }
            },
            _ => {
                eprintln!("[jni] feed on stale/absent handle {handle} — ignoring");
                None
            }
        }
    }));
    let out = match result {
        Ok(Some(o)) => o,
        Ok(None) => return null, // stale handle or feed error — signal close
        Err(_) => {
            jni_log('E', format_args!("feed panicked — connection will be torn down"));
            return null;
        }
    };
    env.byte_array_from_slice(&out).map(|a| a.into_raw()).unwrap_or(null)
}

/// True once pair-verify has completed and the channel is encrypted.
#[no_mangle]
pub extern "system" fn Java_zeno_gmccpa_pair_NativeCore_nativeIsEncrypted(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jni::sys::jboolean {
    if handle == 0 {
        return 0;
    }
    std::panic::catch_unwind(|| {
        let slot = core();
        match slot.as_ref() {
            Some((gen, native)) if *gen == handle as u64 => u8::from(native.server.is_encrypted()),
            _ => 0,
        }
    })
    .unwrap_or(0)
}

/// Milliseconds since this generation's session last RECEIVED screen/audio data or a keepAlive
/// beacon (`ControlServer::av_idle_ms`): -1 = none ever (`None`), -2 = unknown (CORE contended,
/// generation mismatch, or a panic).
///
/// Gates the Kotlin pump's 30 s control-silence backstop exactly as the reference `serve_connection`
/// does (receiver net.rs, AV_IDLE_TEARDOWN_MS). `try_lock`, NOT [`core`]: CORE is the global mutex
/// `feed` holds — including the multi-second MFi step (see `nativeInit`'s BUSY rationale) — and a
/// blocking getter would stall this pump behind another generation. Only `WouldBlock` is -2; a
/// poisoned lock is recovered with `into_inner()` as [`core`] does, or one feed panic would make this
/// -2 forever and the backstop would silently fall back to the unconditional 30 s break. 2026-10-06.
#[no_mangle]
pub extern "system" fn Java_zeno_gmccpa_pair_NativeCore_nativeAvIdleMs(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    if handle <= 0 {
        return -2;
    }
    std::panic::catch_unwind(|| {
        let slot = match CORE.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(e)) => e.into_inner(),
            Err(TryLockError::WouldBlock) => return -2,
        };
        match slot.as_ref() {
            Some((gen, native)) if *gen == handle as u64 => match native.server.av_idle_ms() {
                Some(ms) => ms.min(i64::MAX as u64) as jlong,
                None => -1,
            },
            _ => -2,
        }
    })
    .unwrap_or(-2)
}

/// Send a single-touch HID report to the iPhone.
///
/// `carplayd` takes touch over a `127.0.0.1:9110` socket because its producer (ocbmd) is a separate
/// process. We are both ends, so this skips the socket and the length-prefixed framing entirely and
/// calls the same `events::send_hid_report(1, …)` the socket path ends at — one less hop on the
/// latency-sensitive path, and no framing to desync.
///
/// `nx`/`ny` are normalized 0.0–1.0 in the advertised display geometry; `phase` follows the OCBM
/// vocabulary (0 = DOWN, 1 = MOVE, 2 = UP). For that vocabulary this is identical to
/// `handle_input_frame` (`fn handle_input_frame`, `ccpa/carplayd/src/main.rs`). It deliberately DIVERGES out-of-vocabulary: the
/// reference maps any unknown phase to tip-DOWN, this maps it to tip-UP. Android has ACTION_CANCEL
/// upstream, and a stuck tip turns the next tap into a drag, so unknown fails toward release.
/// Returns false between sessions (no event channel), which is not an error.
#[no_mangle]
pub extern "system" fn Java_zeno_gmccpa_pair_NativeCore_nativeTouch(
    _env: JNIEnv,
    _class: JClass,
    phase: jni::sys::jint,
    nx: jni::sys::jfloat,
    ny: jni::sys::jfloat,
    width: jni::sys::jint,
    height: jni::sys::jint,
) -> jni::sys::jboolean {
    std::panic::catch_unwind(|| {
        // Fail SAFE toward release: an unrecognized phase must not leave the tip stuck down.
        let buttons: u8 = if phase == 0 || phase == 1 { 1 } else { 0 };
        let report = receiver::hid::touch_report_normalized(
            buttons,
            nx as f64,
            ny as f64,
            width.max(1) as u16,
            height.max(1) as u16,
        );
        u8::from(receiver::events::send_hid_report(1, &report))
    })
    .unwrap_or(0)
}

/// Tap one media-transport key on the advertised media-buttons HID device.
///
/// uid **2** is `HID_UID_MEDIA_BUTTONS` (`receiver/src/info.rs:19`), the second HID device this
/// receiver advertises in `/info` alongside the uid-1 touchscreen. The report is a single byte
/// carrying a Consumer usage index (`receiver::hid::media_button`), so a tap is press `[index]` then
/// release `[0]` — a held index repeats the key on iOS.
///
/// This is the ONLY working transport path in this app's bridge role. The OCBM `INPUT_MEDIA_BTN`
/// opcode terminates in the BOX's `carplayd`, which never spawns its A/V layer here and therefore
/// holds no event channel; this process owns the channel instead, exactly as it does for touch.
///
/// Blocks on the same global event mutex as `nativeTouch` — call it off the binder/UI thread. Returns
/// false between sessions (no event channel), which is normal rather than an error.
#[no_mangle]
pub extern "system" fn Java_zeno_gmccpa_pair_NativeCore_nativeMediaButton(
    _env: JNIEnv,
    _class: JClass,
    index: jni::sys::jint,
) -> jni::sys::jboolean {
    std::panic::catch_unwind(|| {
        // Range-checked against the descriptor's logical maximum so an out-of-range array index can
        // never reach iOS; 0 (release) is sent by us below, never by a caller.
        if !(1..=5).contains(&index) {
            eprintln!("[jni] media-btn index={index} out of range 1..=5 — dropped");
            return 0;
        }
        let pressed =
            receiver::events::send_hid_report(2, &receiver::hid::media_button_report(index as u8));
        // Release even if the press was refused: a stuck index is worse than a dropped tap.
        let released = receiver::events::send_hid_report(
            2,
            &receiver::hid::media_button_report(receiver::hid::media_button::NONE),
        );
        u8::from(pressed && released)
    })
    .unwrap_or(0)
}

/// `setLimitedUI` — tell iOS to restrict the CarPlay UI for driving, and to release it when parked.
///
/// Wraps [`receiver::events::send_set_limited_ui`], which emits
/// `{type:"setLimitedUI", params:{limitedUI:<bool>}}` on the event channel (Apple
/// `kAirPlayCommand_SetLimitedUI`). iOS restricts the on-screen keyboard, the phone dial keypad and
/// long scrollable lists; which elements exactly is an optional `/info` `limitedUIElements` list, and
/// absent that list iOS applies its own default set — which is what we want, because Apple's default
/// is the one their HMI guidelines are written against.
///
/// **This is a pure runtime command: no reconnect, no `/info` change, no SETUP feature negotiation.**
/// That is why the drive-state binding can be added to an already-shipping `/info` without touching
/// the pairing or capability surface — the single most valuable property of this particular lever, and
/// the reason it is safe to toggle repeatedly inside a live session.
///
/// The macOS host drives the same receiver function over OCBM (`CMD_LIMITED_UI_ON`/`_OFF`, dispatched
/// in `ccpa/carplayd/src/main.rs`) because there the receiver is a separate process on the box. Here
/// the receiver is in-process behind this JNI boundary, so the box is not involved at all and no OCBM
/// opcode is needed.
///
/// BLOCKING on the global event mutex, like every other command here — call it off the binder/UI
/// thread. Returns false between sessions (no event channel), which is normal rather than an error:
/// limited-UI state is re-asserted on session up, not persisted across one.
#[no_mangle]
pub extern "system" fn Java_zeno_gmccpa_pair_NativeCore_nativeSetLimitedUI(
    _env: JNIEnv,
    _class: JClass,
    limit: jni::sys::jboolean,
) -> jni::sys::jboolean {
    std::panic::catch_unwind(|| u8::from(receiver::events::send_set_limited_ui(limit != 0)))
        .unwrap_or(0)
}

/// `setNightMode` — mirror the head unit's day/night state into CarPlay's own UI.
///
/// Wraps [`receiver::events::send_set_night_mode`], which emits
/// `{type:"setNightMode", params:{nightMode:<bool>}}`. Like `setLimitedUI` this is a pure runtime
/// `/command`: no reconnect, no `/info` change, no SETUP negotiation.
///
/// The source on this head unit is the AAOS `uiMode` night flag, which is exactly what
/// `carlink_native`'s Compose `isSystemInDarkTheme()` reads — that app then sends the OLD Carlinkit
/// adapter opcodes (`ENABLE_NIGHT_MODE` 16 / `DISABLE_NIGHT_MODE` 17) through the box, because there
/// the box owns the CarPlay session. Here we own it, so the same signal drives the native CarPlay
/// command directly and no adapter opcode is involved.
///
/// Deliberately NOT `uiAppearanceUpdate`: that is the newer per-display UI/Map appearance surface,
/// carries an `appearanceSetting` whose only known-working value is a hardcoded constant, and needs
/// the per-display `uiAppearanceModes` advertisement in `/info` that we do not emit. `setNightMode` is
/// the coarse, long-standing lever and the one `carlink_native` proved against real hardware.
///
/// BLOCKING on the global event mutex — call it off the binder/UI thread. Returns false between
/// sessions, which is normal; night state is re-asserted on session-up.
#[no_mangle]
pub extern "system" fn Java_zeno_gmccpa_pair_NativeCore_nativeSetNightMode(
    _env: JNIEnv,
    _class: JClass,
    night: jni::sys::jboolean,
) -> jni::sys::jboolean {
    std::panic::catch_unwind(|| u8::from(receiver::events::send_set_night_mode(night != 0)))
        .unwrap_or(0)
}

/// Plan v5.9 MED M5 / OD-9C (owner: stock-style) — take MainAudio from the phone after the head unit's
/// media focus was lost for good, so the phone pauses instead of playing into a car that switched
/// source. Wraps [`receiver::events::send_take_main_audio`] (stock's media-loss `changeModes`, see its
/// doc). Returns false when a take is already outstanding, or between sessions.
///
/// BLOCKING on the global event mutex — call it off the binder/UI thread (`CarPlayEventTx`).
#[no_mangle]
pub extern "system" fn Java_zeno_gmccpa_pair_NativeCore_nativeTakeMainAudio(
    _env: JNIEnv,
    _class: JClass,
) -> jni::sys::jboolean {
    std::panic::catch_unwind(|| u8::from(receiver::events::send_take_main_audio())).unwrap_or(0)
}

/// Give MainAudio back after an OD-9C take ([`receiver::events::release_main_audio`]). Returns
/// 0 = nothing owed (no take outstanding — the normal case), 1 = Untake sent, -1 = owed but the write
/// failed. BLOCKING on the global event mutex — call it off the binder/UI thread (`CarPlayEventTx`).
#[no_mangle]
pub extern "system" fn Java_zeno_gmccpa_pair_NativeCore_nativeReleaseMainAudio(
    _env: JNIEnv,
    _class: JClass,
) -> jni::sys::jint {
    std::panic::catch_unwind(|| match receiver::events::release_main_audio() {
        None => 0,
        Some(true) => 1,
        Some(false) => -1,
    })
    .unwrap_or(-1)
}

/// Ask iOS for a fresh IDR. The receiver already does this when a seam consumer attaches; this is the
/// decoder-driven path (drop, reset, stall) so a corrupt picture recovers in ~500 ms instead of waiting
/// for the next natural IRAP, which CarPlay may not send for a minute.
#[no_mangle]
pub extern "system" fn Java_zeno_gmccpa_pair_NativeCore_nativeForceKeyFrame(
    _env: JNIEnv,
    _class: JClass,
) -> jni::sys::jboolean {
    u8::from(
        std::panic::catch_unwind(receiver::events::send_force_key_frame).unwrap_or(false),
    )
}

/// Draw the per-process token, bind `addr` SYNCHRONOUSLY, and publish the listener (with its
/// token) into `cell` only on success. `Err` = nothing published: `mic_auth_hex` then yields `None`
/// forever, so the app never hands the token to whatever holds the port.
fn start_mic_ingest(
    cell: &OnceLock<receiver::uplink::ControlListener>,
    addr: &str,
) -> Result<std::net::SocketAddr, String> {
    if cell.get().is_some() {
        return Err("already started (JNI_OnLoad ran twice) — listener not restarted".into());
    }
    let mut token = [0u8; receiver::uplink::MIC_TOKEN_LEN];
    getrandom::getrandom(&mut token).map_err(|e| format!("getrandom failed ({e})"))?;
    let l = receiver::uplink::start_control_listener(addr, Some(token))
        .map_err(|e| format!("bind {addr} failed ({e}) — another process may hold it"))?;
    let local = l.local_addr();
    // A racing second caller lost the bind already (the port is ours), so this set cannot lose.
    cell.set(l).map_err(|_| "already started".to_string())?;
    Ok(local)
}

/// Reserve `local_port` on the published listener and return its token as 32 lowercase hex chars.
/// `None` when no listener was published, it stopped serving, or the port is out of range.
fn mic_auth_hex(cell: &OnceLock<receiver::uplink::ControlListener>, local_port: i32) -> Option<String> {
    let port = u16::try_from(local_port).ok()?;
    let token = cell.get()?.admit(port)?;
    Some(token.iter().map(|b| format!("{b:02x}")).collect())
}

/// Withdraw the reservation for `local_port` (see `mic_auth_hex`); a no-op when the accept already
/// consumed it, no listener was published, or the port is out of range.
fn mic_release(cell: &OnceLock<receiver::uplink::ControlListener>, local_port: i32) {
    if let (Ok(port), Some(l)) = (u16::try_from(local_port), cell.get()) {
        l.release(port);
    }
}

/// `nativeMicRelease(localPort: Int)` (F136 fix round 2) — `MicUplink` calls this on every attempt's
/// teardown, BEFORE closing the socket bound to `localPort`: after a failed connect the reservation
/// would otherwise stay claimable (up to the receiver's 5 s TTL) by whatever socket the OS next hands
/// that port to. While our socket still holds the port nothing else can match it, so release-then-
/// close leaves no window. Panics are caught; nothing is returned.
#[no_mangle]
pub extern "system" fn Java_zeno_gmccpa_pair_NativeCore_nativeMicRelease(
    _env: JNIEnv,
    _class: JClass,
    local_port: jni::sys::jint,
) {
    let _ = std::panic::catch_unwind(|| mic_release(&MIC_SEAM, local_port));
}

/// `nativeMicAuth(localPort: Int) -> String?` (F136) — for the app's own `MicUplink`, which has
/// already BOUND its socket to `localPort` and is about to connect from it to `:9112`. Reserves that
/// source port (its connection then gets a pre-auth slot no flood can take) and returns the token to
/// send as `auth <hex>`. JNI null when the listener never bound (port held by another process,
/// getrandom failed) or stopped serving, for a bad port, on a JNI string error, or on a panic.
#[no_mangle]
pub extern "system" fn Java_zeno_gmccpa_pair_NativeCore_nativeMicAuth<'l>(
    env: JNIEnv<'l>,
    _class: JClass<'l>,
    local_port: jni::sys::jint,
) -> jni::sys::jstring {
    let null: jni::sys::jstring = std::ptr::null_mut();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let hex = mic_auth_hex(&MIC_SEAM, local_port)?;
        env.new_string(hex).ok().map(|s| s.into_raw())
    }))
    .ok()
    .flatten()
    .unwrap_or(null)
}

/// `nativeVoiceClockUs() -> Long` (plan v5.8 V1a) — `receiver::session::voice_clock_us()`: the
/// process-wide monotonic microsecond clock the receiver stamps every voice AU (`rx_us`) and every
/// BEGIN/END (`write_us`) with, and logs SETUP/TEARDOWN handling on. The app stamps its own reads
/// and writes with this, in-process, so every voice age it reports is on rust's clock — never
/// `System.nanoTime` against a relay timestamp. No lock, no allocation; a panic (none is possible
/// past the first-call `OnceLock` init) reads as -1.
#[no_mangle]
pub extern "system" fn Java_zeno_gmccpa_pair_NativeCore_nativeVoiceClockUs(
    _env: JNIEnv,
    _class: JClass,
) -> jlong {
    std::panic::catch_unwind(|| receiver::session::voice_clock_us().min(i64::MAX as u64) as jlong)
        .unwrap_or(-1)
}

#[no_mangle]
pub extern "system" fn Java_zeno_gmccpa_pair_NativeCore_nativeDestroy(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle == 0 {
        return;
    }
    let _ = std::panic::catch_unwind(|| {
        let mut slot = core();
        match slot.as_ref() {
            // Only the generation that installed the core may destroy it. This is what makes the old
            // connection's `finally { stop() }` harmless during a reconnect: it targets a superseded
            // generation and no-ops instead of tearing down the freshly-installed session.
            Some((gen, _)) if *gen == handle as u64 => {
                *slot = None; // drops the Native (and AvSession) under the lock
                println!("[jni] control server destroyed (gen {handle})");
            }
            Some((gen, _)) => {
                println!("[jni] destroy ignored: handle {handle} != current gen {gen}");
            }
            None => {}
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Host tests (F141). Run: `cargo test --no-default-features` (default features need a cross-built
// libfdk-aac via eld-codec, which the host does not have).
// ---------------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn tmp_store() -> std::path::PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "carplayjni_peers_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("peers.bin")
    }

    fn id(n: usize) -> Vec<u8> {
        format!("controller-{n:04}").into_bytes()
    }

    // A-F2: the marker -> logcat priority map the pipe reader applies. Receiver-side lines, including
    // its stderr SUCCESS lines, stay INFO; only this crate's marked failures are W/E.
    #[test]
    fn log_prio_maps_markers_only() {
        assert_eq!(log_prio(b"[jni] E: peer store write failed: boom"), 6);
        assert_eq!(log_prio(b"[jni] W: feed error: Bad"), 5);
        assert_eq!(log_prio(b"[jni] loaded - receiver core ready"), 4);
        assert_eq!(log_prio(b"[receiver] pair-verify OK"), 4);
        assert_eq!(log_prio(b"[jni] E:missing space"), 4);
        assert_eq!(log_prio(b""), 4);
        assert_eq!(log_prio(b"E: not ours"), 4);
    }

    fn key(n: usize) -> [u8; 32] {
        [n as u8; 32]
    }

    #[test]
    fn cap_evicts_oldest_saved() {
        let path = tmp_store();
        let mut p = AppPeers::load(path.clone());
        for n in 0..MAX_PEERS + 4 {
            p.save_peer(&id(n), key(n));
            assert!(p.peers.len() <= MAX_PEERS);
        }
        assert_eq!(p.peers.len(), MAX_PEERS);
        for n in 0..4 {
            assert_eq!(p.find_peer(&id(n)), None, "peer {n} should have been evicted");
        }
        for n in 4..MAX_PEERS + 4 {
            assert_eq!(p.find_peer(&id(n)), Some(key(n)));
        }
        // On disk too.
        assert_eq!(AppPeers::load(path).peers.len(), MAX_PEERS);
    }

    #[test]
    fn resave_refreshes_position() {
        let mut p = AppPeers::load(tmp_store());
        for n in 0..MAX_PEERS {
            p.save_peer(&id(n), key(n));
        }
        // Re-pair peer 0 with a new key: it moves to newest and is not duplicated.
        p.save_peer(&id(0), key(200));
        assert_eq!(p.peers.len(), MAX_PEERS);
        assert_eq!(p.find_peer(&id(0)), Some(key(200)));
        // The next new peer evicts peer 1 (now oldest), not peer 0.
        p.save_peer(&id(99), key(99));
        assert_eq!(p.find_peer(&id(0)), Some(key(200)));
        assert_eq!(p.find_peer(&id(1)), None);
        assert_eq!(p.peers.len(), MAX_PEERS);
    }

    #[test]
    fn load_persist_round_trip() {
        let path = tmp_store();
        let mut p = AppPeers::load(path.clone());
        assert!(p.peers.is_empty());
        let long_id = vec![0xAB; 255];
        p.save_peer(&id(1), key(1));
        p.save_peer(&long_id, key(2));
        p.save_peer(b"", key(3));
        let q = AppPeers::load(path.clone());
        assert_eq!(q.peers, p.peers);
        assert!(!path.with_extension("bin.tmp").exists());
    }

    #[test]
    fn oversized_legacy_store_trims_on_next_save() {
        // A store written before the cap may hold more than MAX_PEERS: load keeps all of them
        // (no pairing is dropped silently at startup); the next save trims to MAX_PEERS, newest kept.
        let path = tmp_store();
        let mut raw = Vec::new();
        for n in 0..40 {
            let i = id(n);
            raw.push(i.len() as u8);
            raw.extend_from_slice(&i);
            raw.extend_from_slice(&key(n));
        }
        std::fs::write(&path, &raw).unwrap();
        let mut p = AppPeers::load(path.clone());
        assert_eq!(p.peers.len(), 40);
        p.save_peer(&id(100), key(100));
        assert_eq!(p.peers.len(), MAX_PEERS);
        assert_eq!(p.find_peer(&id(100)), Some(key(100)));
        assert_eq!(p.find_peer(&id(39)), Some(key(39)));
        assert_eq!(p.find_peer(&id(24)), None);
        assert_eq!(p.find_peer(&id(25)), Some(key(25)));
        assert_eq!(AppPeers::load(path).peers.len(), MAX_PEERS);
    }

    #[test]
    fn truncated_store_loads_complete_records_only() {
        let path = tmp_store();
        let mut p = AppPeers::load(path.clone());
        p.save_peer(&id(1), key(1));
        p.save_peer(&id(2), key(2));
        let mut raw = std::fs::read(&path).unwrap();
        raw.truncate(raw.len() - 5);
        std::fs::write(&path, &raw).unwrap();
        let q = AppPeers::load(path);
        assert_eq!(q.peers.len(), 1);
        assert_eq!(q.find_peer(&id(1)), Some(key(1)));
    }

    // ---- F136 fix round 1: the :9112 mic seam's JNI glue, and the flood test ----------------------

    /// Fix 1: a held port means `Err` and NOTHING published — no token for anyone.
    #[test]
    fn mic_ingest_on_a_held_port_publishes_no_token() {
        let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = squatter.local_addr().unwrap().to_string();
        let cell = OnceLock::new();
        let err = start_mic_ingest(&cell, &taken).unwrap_err();
        assert!(err.contains("bind"), "unexpected error: {err}");
        assert!(cell.get().is_none(), "a listener was published although the bind failed");
        assert_eq!(mic_auth_hex(&cell, 40200), None, "a token was handed out for a squatted port");
        // The happy path, and the guards on the port argument.
        let ok = OnceLock::new();
        start_mic_ingest(&ok, "127.0.0.1:0").unwrap();
        assert_eq!(mic_auth_hex(&ok, 40201).map(|h| h.len()), Some(32));
        assert_eq!(mic_auth_hex(&ok, 0), None);
        assert_eq!(mic_auth_hex(&ok, -1), None);
        assert_eq!(mic_auth_hex(&ok, 65536), None);
        assert!(start_mic_ingest(&ok, "127.0.0.1:0").is_err(), "started twice");
        // Fix round 2: release is safe on any input, including an empty cell.
        mic_release(&ok, 40202);
        mic_release(&ok, -1);
        mic_release(&cell, 40203);
    }

    /// What MicUplink does: bind 127.0.0.1:0, ask the core for the token (which reserves the bound
    /// port), connect from that port, send `auth`, wait for the ack.
    fn reserved_attempt(
        cell: &OnceLock<receiver::uplink::ControlListener>,
        server: std::net::SocketAddr,
    ) -> Result<Duration, String> {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        let sin = |ip: [u8; 4], port: u16| {
            // SAFETY: sockaddr_in is plain old data; all-zero is a valid value.
            let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            {
                sa.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
            }
            sa.sin_family = libc::AF_INET as _;
            sa.sin_port = port.to_be();
            sa.sin_addr.s_addr = u32::from_ne_bytes(ip);
            sa
        };
        let len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        // SAFETY: a fresh socket fd, owned immediately; the sockaddr pointers are valid for `len`.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(format!("socket: {}", io::Error::last_os_error()));
        }
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut local = sin([127, 0, 0, 1], 0);
        if unsafe { libc::bind(owned.as_raw_fd(), (&local as *const libc::sockaddr_in).cast(), len) } != 0 {
            return Err(format!("bind: {}", io::Error::last_os_error()));
        }
        let mut l = len;
        if unsafe { libc::getsockname(owned.as_raw_fd(), (&mut local as *mut libc::sockaddr_in).cast(), &mut l) } != 0 {
            return Err(format!("getsockname: {}", io::Error::last_os_error()));
        }
        let port = u16::from_be(local.sin_port);
        let hex = mic_auth_hex(cell, i32::from(port)).ok_or("no token")?;
        let t0 = Instant::now();
        let dst = sin([127, 0, 0, 1], server.port());
        if unsafe { libc::connect(owned.as_raw_fd(), (&dst as *const libc::sockaddr_in).cast(), len) } != 0 {
            return Err(format!("connect: {}", io::Error::last_os_error()));
        }
        let mut c = std::net::TcpStream::from(owned);
        auth_and_ack(&mut c, &hex).map(|()| t0.elapsed())
    }

    fn auth_and_ack(c: &mut std::net::TcpStream, hex: &str) -> Result<(), String> {
        use std::io::{BufRead as _, Write as _};
        c.write_all(format!("auth {hex}\n").as_bytes()).map_err(|e| format!("write: {e}"))?;
        c.set_read_timeout(Some(Duration::from_millis(1500))).unwrap();
        let mut line = String::new();
        let _ = io::BufReader::new(c.try_clone().unwrap()).read_line(&mut line);
        if line == receiver::uplink::AUTH_ACK {
            Ok(())
        } else {
            Err(format!("no ack (got {line:?})"))
        }
    }

    /// Fix 2: N attacker threads hammer the seam — silent connects held until the server closes
    /// them, garbage lines, and connect-and-drop churn — while the real client (bind, reserve,
    /// connect, auth) connects and authenticates repeatedly. Every reserved attempt must be acked.
    /// An UNRESERVED client making the same attempts is measured and printed, not asserted: the
    /// anonymous pool is exactly what the flood is allowed to fill.
    #[test]
    fn mic_seam_flood_never_blocks_the_reserved_client() {
        use std::io::{Read as _, Write as _};
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        let cell = OnceLock::new();
        let server = start_mic_ingest(&cell, "127.0.0.1:0").unwrap();
        let env_n = |k: &str, d: usize| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
        let (silent, garbage, churn) =
            (env_n("F136_FLOOD_SILENT", 48), env_n("F136_FLOOD_GARBAGE", 8), env_n("F136_FLOOD_CHURN", 8));
        let stop = Arc::new(AtomicBool::new(false));
        let connects = Arc::new(AtomicU64::new(0));
        let mut hs = vec![];
        for kind in std::iter::repeat_n(0u8, silent).chain(std::iter::repeat_n(1, garbage)).chain(std::iter::repeat_n(2, churn)) {
            let (stop, connects) = (Arc::clone(&stop), Arc::clone(&connects));
            hs.push(std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let Ok(mut c) = std::net::TcpStream::connect(server) else { continue };
                    connects.fetch_add(1, Ordering::Relaxed);
                    match kind {
                        0 => {}
                        1 => {
                            let _ = c.write_all(b"GET / HTTP/1.0\r\nmic 4\nauth 00\n");
                        }
                        _ => {
                            // Connect-and-drop with an RST (SO_LINGER 0): no TIME_WAIT, so the churn
                            // stresses the listener's accept queue, not the host's ephemeral ports.
                            use std::os::fd::AsRawFd;
                            let lg = libc::linger { l_onoff: 1, l_linger: 0 };
                            // SAFETY: valid fd and a correctly sized linger struct.
                            unsafe {
                                libc::setsockopt(
                                    c.as_raw_fd(),
                                    libc::SOL_SOCKET,
                                    libc::SO_LINGER,
                                    (&lg as *const libc::linger).cast(),
                                    std::mem::size_of::<libc::linger>() as libc::socklen_t,
                                );
                            }
                            continue;
                        }
                    }
                    let _ = c.set_read_timeout(Some(Duration::from_secs(3)));
                    let mut b = [0u8; 64];
                    while matches!(c.read(&mut b), Ok(n) if n > 0) {}
                }
            }));
        }
        std::thread::sleep(Duration::from_millis(300));
        let attempts = env_n("F136_FLOOD_ATTEMPTS", 40);
        let (mut ok, mut anon_ok, mut worst) = (0usize, 0usize, Duration::ZERO);
        let mut failures = vec![];
        for _ in 0..attempts {
            match reserved_attempt(&cell, server) {
                Ok(d) => {
                    ok += 1;
                    worst = worst.max(d);
                }
                Err(e) => failures.push(e),
            }
            if let Ok(mut c) = std::net::TcpStream::connect(server) {
                let hex = mic_auth_hex(&cell, 1).unwrap(); // token only; port 1 is never a peer
                anon_ok += usize::from(auth_and_ack(&mut c, &hex).is_ok());
            }
            // MicUplink's real cadence is 2 s; 50 ms lets the server see the previous close.
            std::thread::sleep(Duration::from_millis(50));
        }
        stop.store(true, Ordering::Relaxed);
        for h in hs {
            let _ = h.join();
        }
        eprintln!(
            "F136 FLOOD ({silent} silent + {garbage} garbage + {churn} churn threads, {} attacker connects): \
             reserved client acked {ok}/{attempts} (worst connect->ack {worst:?}); unreserved client \
             acked {anon_ok}/{attempts}; failures: {failures:?}",
            connects.load(Ordering::Relaxed)
        );
        assert_eq!(ok, attempts, "the reserved client was refused under the flood: {failures:?}");
    }
}
