//! receiver — the AirPlay/CarPlay receiver: control-plane state machine, and (in later phases) the
//! A/V session (stream setup, ChaCha20 stream crypto, RTP, the decrypt→IPC-forward seam, HID, modes,
//! ForceKeyFrame). Ten subsystems behind a stable API.
//!
//! - Phase 5: [`server::ControlServer`] — sans-IO RTSP control plane (pairing/mfi wired, encrypted flip).
//! - Phase 6: [`info`] (`/info` binary plist) + [`net`] (drive a ControlServer over a byte stream).
//! - Phase 7: [`stream`] (per-stream keys, RTP, audio packet ChaCha20 decrypt) + [`forward`] (the
//!   AVCC→Annex-B / AAC→ADTS / ELD-tag framing the IPC seam carries to carlink_linux).

#![forbid(unsafe_code)]

pub mod datastream;
pub mod events;
pub mod forward;
pub mod hid;
pub mod iap_tunnel;
pub mod info;
pub mod levers;
pub mod net;
pub mod relay;
pub mod server;
pub mod session;
pub mod stream;
pub mod vehicle_config;
#[cfg(feature = "mic-uplink")]
pub mod uplink;

/// Poison-tolerant lock. House policy: never bare `.lock().unwrap()` on cross-thread state.
/// Production runs panic="abort" so poisoning is unreachable there; in test builds (unwind) a
/// panicking holder must not cascade into every later locker. Centralizes the POISON-TOLERANT
/// notes of 2026-07-30.
pub(crate) fn plock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
