//! bt-common — the Bluetooth primitives both wireless projection paths need.
//!
//! CarPlay and Android Auto reach the phone over the same radio and, at this layer, do the same
//! things: bring the controller up, advertise a service record, accept an RFCOMM connection, and
//! pair. Only the *content* differs — the record's UUID, and what is said once the channel opens.
//! Everything in this crate is that shared mechanism; nothing here knows which protocol it serves.
//!
//! Extracted from `btd` (see Cargo.toml for what stayed behind and why). These modules
//! are device-proven in that daemon; treat behaviour changes here as touching a shipping path.
//!
//! NOT cfg-gated to Linux, deliberately. These modules compiled on macOS as part of
//! `btd` and its test suite runs on the build host (`tools/run_tests.sh` calls
//! `cargo test -p btd`), so gating them here would silently take 27 tests out of the
//! host run. Where a syscall genuinely differs, the gating is inside the module that needs it —
//! `cloexec.rs` already carries a macOS branch.

pub mod cloexec;
pub mod hci;
pub mod rfcomm;
pub mod rfcomm_uspace;
pub mod sdp_record;
pub mod sdp_server;
pub mod ssp_agent;

/// Read an environment lever by its current name, falling back to the pre-2026-09-08 `CARPLAY_*`
/// name it was renamed from.
///
/// The renamed levers are the PROTOCOL-NEUTRAL ones: Bluetooth SSP/HFP/HCI/RFCOMM settings that
/// apply to any peer (a Pixel bonds through the same SSP agent an iPhone does), and box-level
/// settings like the SoftAP role. Keeping a `CARPLAY_` prefix on them is what let 3,700 lines of
/// Android Auto code accumulate inside a binary called `carplay-wireless`, and it is why
/// `CARPLAY_AA_HEADSET_PATH` — a CarPlay-prefixed variable whose entire purpose is Android Auto —
/// existed. Genuinely Apple levers (`CARPLAY_DEVICE_ID`, `CARPLAY_PI`, `CARPLAY_HEVC`, …) keep
/// their names: there the prefix is information, not noise.
///
/// The fallback exists because a lever that silently reverts to its default is the worst failure
/// shape here — `CARPLAY_WIFI_AP` was exported by the supervisor and read by NOTHING for as long as
/// the bridge role existed, and nothing reported it. So an old name still works, and says so.
///
/// Duplicated from `box_common::lever` on purpose: `bt-common` deliberately has no dependency
/// beyond `libc`, and one 12-line function is a smaller price than that edge.
pub fn lever(name: &str, legacy: &str) -> Option<String> {
    if let Ok(v) = std::env::var(name) {
        return Some(v);
    }
    match std::env::var(legacy) {
        Ok(v) => {
            eprintln!("[lever] {legacy} is the OLD name for {name} — honouring it, but update the launcher");
            Some(v)
        }
        Err(_) => None,
    }
}
