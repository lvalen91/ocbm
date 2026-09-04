//! carplay-metadata — shared Now Playing / Route Guidance metadata: iAP2 TLV parsing (`tlv`) and the
//! wire types both ends share (`types`). No GTK/GStreamer/USB dependency, so it can sit on both
//! sides of the producer/consumer boundary. The consumer in this workspace is
//! `crates/vendor/iap2-core`, which owns the iAP2 link this data rides. (Corrected 2026-09-01: this
//! used to name `carplayd` and `crates/ui`; neither exists here.)

pub mod destination;
pub mod location;
pub mod tlv;
pub mod types;
pub mod vehicle;

pub use types::{
    DistanceUnit, Maneuver, ManeuverState, ManeuverType, MetadataMessage, NowPlaying,
    PlaybackStatus, RouteGuidance, RouteGuidanceState,
};

// Vehicle-data / location / destination plane (device→accessory + accessory→device updates and their
// selectable-field subscribes). Kept as standalone types (NOT wired into `MetadataMessage`) so the
// change is strictly additive and cannot break exhaustive matches in dependent crates.
pub use destination::{build_start_destination_information, Coordinate, Destination};
pub use location::{LocationInfo, LocationSelection};
pub use vehicle::{
    ChargingConnectorType, VehicleAlert, VehicleStatus, VehicleStatusSelection, WiperStatus,
};
