//! bt-common — the Bluetooth primitives both wireless projection paths need.
//!
//! CarPlay and Android Auto reach the phone over the same radio and, at this layer, do the same
//! things: bring the controller up, advertise a service record, accept an RFCOMM connection, and
//! pair. Only the *content* differs — the record's UUID, and what is said once the channel opens.
//! Everything in this crate is that shared mechanism; nothing here knows which protocol it serves.
//!
//! Extracted from `carplay-wireless` (see Cargo.toml for what stayed behind and why). These modules
//! are device-proven in that daemon; treat behaviour changes here as touching a shipping path.
//!
//! NOT cfg-gated to Linux, deliberately. These modules compiled on macOS as part of
//! `carplay-wireless` and its test suite runs on the build host (`tools/run_tests.sh` calls
//! `cargo test -p carplay-wireless`), so gating them here would silently take 27 tests out of the
//! host run. Where a syscall genuinely differs, the gating is inside the module that needs it —
//! `cloexec.rs` already carries a macOS branch.

pub mod cloexec;
pub mod hci;
pub mod rfcomm;
pub mod rfcomm_uspace;
pub mod sdp_record;
pub mod sdp_server;
pub mod ssp_agent;
