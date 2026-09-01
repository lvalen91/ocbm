//! carplay-metadata — shared Now Playing / Route Guidance metadata: iAP2 TLV parsing (`tlv`) and the
//! wire types both ends share (`types`). No GTK/GStreamer/USB dependency -- used by `carplayd`
//! (the producer, owning the iAP2 link that this data actually rides) and `crates/ui` (the
//! consumer, displaying it), matching how `carplay-control` is a plain library shared conceptually
//! across the touch producer/consumer boundary rather than duplicated on each side.

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
