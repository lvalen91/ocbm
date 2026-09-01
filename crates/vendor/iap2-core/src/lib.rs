//! carplay-iap2-core — the transport-agnostic half of the iAP2 accessory protocol stack, extracted
//! from `carplayd`'s original `src/iap2/` so both the wired driver (`carplayd`'s own
//! `src/iap2/driver.rs`, FunctionFS-bound) and the wireless driver (`carplay-wireless`'s
//! `bt_driver.rs`, RFCOMM-bound) can share it without duplication.
//!
//! What moved here (pure protocol logic, zero I/O, zero USB references — confirmed by direct
//! inspection before this extraction, see docs/carplay/04_CAPABILITIES_AND_CONFIG.md): `link` (SOP/checksum link-layer framing),
//! `state` (the auth/identify state machine, `on_message`), `message` (the TLV builder +
//! `build_ident_info`). What did NOT move (irreducibly transport-specific, stayed in each driver's own
//! crate):
//! FunctionFS descriptors/`ffs.rs` and the actual endpoint-opening/liveness-polling code for the
//! wired side; the RFCOMM socket/SDP/HCI bring-up code for the wireless side.

/// The metadata capability table: what we declare in Identify params 6/7 and what we subscribe to,
/// generated from one list so the three cannot drift apart again. See docs/carplay/05_METADATA_AND_CONTROLS.md.
pub mod features;
/// Reader for the app-pushed `VehicleConfig` YAML — the iAP2 side's slice of it (metadata tier
/// today; vehicle identity next). See docs/carplay/04_CAPABILITIES_AND_CONFIG.md.
pub mod config;
pub mod link;
pub mod message;
/// iAP2 metadata plane: NowPlaying / route-guidance / maneuver / call-state / communications / list
/// subscribe-builders + inbound parsers, plus the `127.0.0.1:9004 → CH_METADATA` seam forwarder and
/// the session-2 artwork reassembler. Std-only, zero new deps. Shared by the wired driver (`iap2d`)
/// and the wireless driver (`carplay-wireless`'s `bt_driver`) so both flow identical metadata frames.
pub mod metadata;
/// CarPlay session-start handshake: `CarPlayStartSession` (0x4301) builder + `CarPlayAvailability`
/// (0x4300) parser. Additive over `message`; see the module header for the wired-path constraint.
pub mod session;
/// Authoritative iAP2/MFi4 message catalog generated from Apple's internal `.i2mspecarchive`
/// files (Xcode 27 CarPlaySimulator.devicekitplugin). The single source of protocol truth.
pub mod spec;
pub mod state;
