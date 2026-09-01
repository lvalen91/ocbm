//! Destination plane: `DestinationInformation` (0x10DB) typed parse + `StartDestinationInformation`
//! (0x10DA) subscribe builder.
//!
//! DIRECTION (from Apple's first-party spec `source` of truth):
//! - `StartDestinationInformation` (0x10DA) `source=0` (ACCESSORY→device): the accessory SUBSCRIBES.
//!   Its only param is a presence-only `SourceName` (id 0) selector, so [`build_start_destination_information`]
//!   emits the fixed subscribe body (matching the `subscribe.rs` builder idiom).
//! - `DestinationInformation` (0x10DB) `source=1` (DEVICE→accessory): the iPhone pushes the active
//!   navigation destination. The accessory PARSES it → [`parse_destination_information`] yields a
//!   typed [`Destination`].
//!
//! Param ids/types verbatim from Apple's `CarPlaySimulator.devicekitplugin` spec.

use crate::tlv::{i32v, parse_groups, u32v, utf8};
use serde::{Deserialize, Serialize};

/// Apple iAP2 parameter ids for the destination plane. Defined locally (see dependency-cycle note in
/// `tlv.rs`). Each const carries Apple's authoritative name.
mod pid {
    /// `DestinationInformation` (0x10DB) flat param ids.
    pub const DESTINATION_IDENTIFIER: u16 = 0; // DestinationIdentifier (utf8)
    pub const DISPLAY_NAME: u16 = 1; // DisplayName (utf8)
    pub const CENTER_COORDINATE: u16 = 2; // CenterCoordinate (group)
    pub const ADDRESS: u16 = 3; // Address (utf8, newline-separated lines)
    pub const ENTRY_POINT: u16 = 4; // EntryPoint (group)
    pub const COORDINATE_THRESHOLD: u16 = 5; // CoordinateThreshold (uint32, meters)
    pub const LOCALE: u16 = 6; // Locale (utf8)
    pub const SOURCE_NAME: u16 = 7; // SourceName (utf8)

    /// `CenterCoordinate`(2) / `EntryPoint`(4) group sub-param ids.
    pub mod coordinate {
        pub const LATITUDE: u16 = 0; // Latitude (int32, Q10.22 fixed-point degrees)
        pub const LONGITUDE: u16 = 1; // Longitude (int32, Q10.22 fixed-point degrees)
    }

    /// `StartDestinationInformation` (0x10DA) param id.
    pub const START_SOURCE_NAME: u16 = 0; // SourceName selector (presence-only)
}

/// Number of fractional bits in Apple's Q10.22 fixed-point coordinate encoding (10 integer bits +
/// 22 fractional bits, signed).
const Q10_22_FRACTIONAL_BITS: u32 = 22;

/// A geographic coordinate from a `DestinationInformation` group (`CenterCoordinate` id 2 or
/// `EntryPoint` id 4). Apple encodes each of `Latitude`/`Longitude` as a signed `int32` in Q10.22
/// fixed-point decimal degrees. Raw ticks are retained so no precision is lost on the wire; use
/// [`Coordinate::latitude_degrees`] / [`Coordinate::longitude_degrees`] for decimal degrees.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Coordinate {
    /// Raw `Latitude` (sub-param 0) ticks: decimal degrees × 2^22, signed (negative = South).
    pub latitude_raw: Option<i32>,
    /// Raw `Longitude` (sub-param 1) ticks: decimal degrees × 2^22, signed (negative = West).
    pub longitude_raw: Option<i32>,
}

impl Coordinate {
    /// Latitude in decimal degrees (`latitude_raw / 2^22`).
    pub fn latitude_degrees(&self) -> Option<f64> {
        self.latitude_raw.map(|r| r as f64 / (1u32 << Q10_22_FRACTIONAL_BITS) as f64)
    }

    /// Longitude in decimal degrees (`longitude_raw / 2^22`).
    pub fn longitude_degrees(&self) -> Option<f64> {
        self.longitude_raw.map(|r| r as f64 / (1u32 << Q10_22_FRACTIONAL_BITS) as f64)
    }

    fn merge(&mut self, delta: Coordinate) {
        if delta.latitude_raw.is_some() {
            self.latitude_raw = delta.latitude_raw;
        }
        if delta.longitude_raw.is_some() {
            self.longitude_raw = delta.longitude_raw;
        }
    }
}

/// Navigation destination snapshot. Field names follow Apple's `DestinationInformation` (0x10DB)
/// params. `Option` fields follow the crate's delta convention (`None` = "not in this update").
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Destination {
    /// `DestinationIdentifier` (id 0): opaque id correlating updates for the same destination.
    pub identifier: Option<String>,
    /// `DisplayName` (id 1): human-facing destination name.
    pub display_name: Option<String>,
    /// `CenterCoordinate` (id 2): the destination's center point.
    pub center_coordinate: Option<Coordinate>,
    /// `Address` (id 3): postal address, lines separated by `\n`.
    pub address: Option<String>,
    /// `EntryPoint` (id 4): navigable entry point (may differ from the center).
    pub entry_point: Option<Coordinate>,
    /// `CoordinateThreshold` (id 5), meters: arrival radius around the coordinate.
    pub coordinate_threshold_m: Option<u32>,
    /// `Locale` (id 6): locale of the destination text (e.g. "en_US").
    pub locale: Option<String>,
    /// `SourceName` (id 7): CarPlay app that provided the destination (display only).
    pub source_name: Option<String>,
}

impl Destination {
    /// Merge a delta into `self`, overwriting only present fields (mirrors `RouteGuidance::merge`).
    /// Coordinate groups are merged field-wise so a partial coordinate update does not blank an axis.
    pub fn merge(&mut self, delta: Destination) {
        if delta.identifier.is_some() {
            self.identifier = delta.identifier;
        }
        if delta.display_name.is_some() {
            self.display_name = delta.display_name;
        }
        if let Some(c) = delta.center_coordinate {
            self.center_coordinate.get_or_insert_with(Coordinate::default).merge(c);
        }
        if delta.address.is_some() {
            self.address = delta.address;
        }
        if let Some(e) = delta.entry_point {
            self.entry_point.get_or_insert_with(Coordinate::default).merge(e);
        }
        if delta.coordinate_threshold_m.is_some() {
            self.coordinate_threshold_m = delta.coordinate_threshold_m;
        }
        if delta.locale.is_some() {
            self.locale = delta.locale;
        }
        if delta.source_name.is_some() {
            self.source_name = delta.source_name;
        }
    }
}

/// `StartDestinationInformation` (0x10DA, accessory-source) subscribe body: a single presence-only
/// `SourceName` (id 0) selector, so the device includes `SourceName` (id 7) in the pushed
/// `DestinationInformation`. Emitted as `[len=4][pid=0]` — a zero-length-value group — matching the
/// presence-only selector encoding used by `subscribe.rs`.
pub fn build_start_destination_information() -> Vec<u8> {
    vec![0x00, 0x04, (pid::START_SOURCE_NAME >> 8) as u8, pid::START_SOURCE_NAME as u8]
}

/// Parse a `DestinationInformation` (0x10DB, device-source) body: flat params, with `CenterCoordinate`
/// (id 2) and `EntryPoint` (id 4) each nesting a Latitude/Longitude sub-group.
pub fn parse_destination_information(body: &[u8]) -> Destination {
    let mut d = Destination::default();
    for (id, val) in parse_groups(body) {
        match id {
            pid::DESTINATION_IDENTIFIER => d.identifier = utf8(val),
            pid::DISPLAY_NAME => d.display_name = utf8(val),
            pid::CENTER_COORDINATE => d.center_coordinate = Some(parse_coordinate(val)),
            pid::ADDRESS => d.address = utf8(val),
            pid::ENTRY_POINT => d.entry_point = Some(parse_coordinate(val)),
            pid::COORDINATE_THRESHOLD => d.coordinate_threshold_m = u32v(val),
            pid::LOCALE => d.locale = utf8(val),
            pid::SOURCE_NAME => d.source_name = utf8(val),
            _ => {}
        }
    }
    d
}

/// Parse a coordinate group value (Latitude id 0, Longitude id 1, both int32 Q10.22).
fn parse_coordinate(val: &[u8]) -> Coordinate {
    let mut c = Coordinate::default();
    for (id, sub) in parse_groups(val) {
        match id {
            pid::coordinate::LATITUDE => c.latitude_raw = i32v(sub),
            pid::coordinate::LONGITUDE => c.longitude_raw = i32v(sub),
            _ => {}
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(pid: u16, value: &[u8]) -> Vec<u8> {
        let mut g = Vec::new();
        g.extend_from_slice(&((4 + value.len()) as u16).to_be_bytes());
        g.extend_from_slice(&pid.to_be_bytes());
        g.extend_from_slice(value);
        g
    }

    fn str_val(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    /// Encode decimal degrees into Apple's signed Q10.22 int32.
    fn q10_22(deg: f64) -> i32 {
        (deg * (1u32 << 22) as f64).round() as i32
    }

    #[test]
    fn build_subscribe_is_presence_only_source_name_selector() {
        assert_eq!(build_start_destination_information(), vec![0x00, 0x04, 0x00, 0x00]);
    }

    #[test]
    fn parse_destination_flat_fields_and_coordinates() {
        let center = [
            group(pid::coordinate::LATITUDE, &q10_22(37.3349).to_be_bytes()),
            group(pid::coordinate::LONGITUDE, &q10_22(-122.0090).to_be_bytes()), // west = negative
        ]
        .concat();
        let body = [
            group(pid::DESTINATION_IDENTIFIER, &str_val("dest-1")),
            group(pid::DISPLAY_NAME, &str_val("Apple Park")),
            group(pid::CENTER_COORDINATE, &center),
            group(pid::ADDRESS, &str_val("One Apple Park Way\nCupertino, CA 95014")),
            group(pid::COORDINATE_THRESHOLD, &50u32.to_be_bytes()),
            group(pid::LOCALE, &str_val("en_US")),
            group(pid::SOURCE_NAME, &str_val("Maps")),
        ]
        .concat();
        let d = parse_destination_information(&body);
        assert_eq!(d.identifier.as_deref(), Some("dest-1"));
        assert_eq!(d.display_name.as_deref(), Some("Apple Park"));
        assert_eq!(d.address.as_deref(), Some("One Apple Park Way\nCupertino, CA 95014"));
        assert_eq!(d.coordinate_threshold_m, Some(50));
        assert_eq!(d.locale.as_deref(), Some("en_US"));
        assert_eq!(d.source_name.as_deref(), Some("Maps"));

        let c = d.center_coordinate.expect("center coordinate present");
        // Round-trip through Q10.22 is lossy at ~2.4e-7 deg; assert within tolerance.
        assert!((c.latitude_degrees().unwrap() - 37.3349).abs() < 1e-5);
        assert!((c.longitude_degrees().unwrap() - (-122.0090)).abs() < 1e-5);
        assert!(c.longitude_raw.unwrap() < 0, "west longitude must stay signed-negative");
    }

    #[test]
    fn parse_entry_point_group() {
        let entry = [
            group(pid::coordinate::LATITUDE, &q10_22(37.3300).to_be_bytes()),
            group(pid::coordinate::LONGITUDE, &q10_22(-122.0100).to_be_bytes()),
        ]
        .concat();
        let body = group(pid::ENTRY_POINT, &entry);
        let d = parse_destination_information(&body);
        let e = d.entry_point.expect("entry point present");
        assert!((e.latitude_degrees().unwrap() - 37.3300).abs() < 1e-5);
        assert!(d.center_coordinate.is_none());
    }

    #[test]
    fn merge_overwrites_present_and_merges_coordinate_axes() {
        let mut snap = Destination {
            display_name: Some("Old".into()),
            center_coordinate: Some(Coordinate {
                latitude_raw: Some(100),
                longitude_raw: Some(200),
            }),
            ..Default::default()
        };
        let delta = Destination {
            display_name: Some("New".into()),
            center_coordinate: Some(Coordinate { latitude_raw: Some(999), ..Default::default() }),
            ..Default::default()
        };
        snap.merge(delta);
        assert_eq!(snap.display_name.as_deref(), Some("New"));
        let c = snap.center_coordinate.unwrap();
        assert_eq!(c.latitude_raw, Some(999)); // overwritten
        assert_eq!(c.longitude_raw, Some(200)); // preserved through nested merge
    }
}
