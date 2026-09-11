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
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use jni::objects::{GlobalRef, JByteArray, JClass, JObject, JString};
use jni::sys::{jbyteArray, jlong};
use jni::{JNIEnv, JavaVM};

use mfi::auth_client::MfiSigner;
use pairing::setup::PeerSaver;
use pairing::verify::{Identity, PeerStore};
use receiver::server::ControlServer;

static JVM: OnceLock<JavaVM> = OnceLock::new();
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
/// Distinct from 0 ("no core available, fall back to the Kotlin stub") because the correct caller
/// response is the opposite one: close the connection and let the phone redial into a core that is
/// by then free. The stub path cannot pair, so answering a busy core with it guarantees a failed
/// session where a redial would have succeeded.
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
// ---------------------------------------------------------------------------------------------
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
                        let mut out = b"[rust ] ".to_vec();
                        out.extend_from_slice(&line);
                        if let Ok(c) = std::ffi::CString::new(out) {
                            android_log(&tag, &c);
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

unsafe fn android_log(tag: &std::ffi::CStr, msg: &std::ffi::CStr) {
    extern "C" {
        fn __android_log_write(prio: i32, tag: *const libc::c_char, text: *const libc::c_char) -> i32;
    }
    __android_log_write(4 /* INFO */, tag.as_ptr(), msg.as_ptr());
}

// ---------------------------------------------------------------------------------------------
// Peer store — controller id -> Ed25519 long-term public key, persisted.
//
// Without persistence every session re-runs pair-setup; with a wrong one, pair-verify fails and you
// debug crypto that is fine. `carplayd` uses DiskPeers for exactly this reason.
// ---------------------------------------------------------------------------------------------
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
            eprintln!("[jni] peer store write failed: {e}");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            eprintln!("[jni] peer store rename failed: {e}");
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
        eprintln!(
            "[jni] WARNING: nativeInit identity (pi/seed) differs from the first init; the ORIGINAL \
             is reused for the process lifetime, so pair-verify will fail against the new pi. Restart \
             the app to change identity."
        );
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
    receiver::uplink::start_control_listener("127.0.0.1:9112".to_string());
    println!("[jni] levers set (hevc=true, everything else off); mic uplink ingest on 127.0.0.1:9112");
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
        eprintln!("[jni] PANIC in JNI_OnLoad — library load continues, core may be unusable");
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
        // generation token. That is safe ONLY because every generation's signer wraps the SAME
        // Kotlin relay object: `MainActivity` holds one process-lifetime `OcbmProbe`, and
        // `mfiRelay()` returns a singleton bound to its mutable `client` field, so a superseded
        // generation's GlobalRef and the current one point at the same live relay.
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
            eprintln!("[jni] init failed: {e}");
            0
        }
        Err(_) => {
            eprintln!("[jni] init panicked");
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
                    eprintln!("[jni] feed error: {e:?}");
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
            eprintln!("[jni] feed panicked — connection will be torn down");
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
