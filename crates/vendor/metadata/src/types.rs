//! Shared wire types for Now Playing / Route Guidance metadata. The single source of truth for
//! field names/shape on both ends of the `carplayd` <-> `crates/ui` wire, so producer and consumer
//! can never drift apart (mirrors why `carplay-control`'s `event.rs` types are shared, not
//! independently redefined on each side).
//!
//! All fields are `Option<T>`: iAP2's Now Playing feed is a DELTA stream (title/artist on track
//! change, elapsed-only frames ~2/s in between) -- `None` means "not present in this particular
//! update", not "empty". Merging deltas into a running snapshot is the producer's job (`carplayd`),
//! not encoded in these types themselves -- see the module doc on the merge site for why (the
//! reference implementation's hard-won finding: naive replacement blanks out title/artist on every
//! elapsed-time tick).

use serde::{Deserialize, Serialize};

/// `PlaybackStatus` — Apple `NowPlayingUpdate` (0x5001) `PlaybackAttributes` (group id 1) sub-param
/// `0` **PlaybackStatus** enum. Values are Apple's authoritative enum table.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackStatus {
    Stopped,
    Playing,
    Paused,
    /// Fast-forward scrub. Was previously unhandled (parsed as `None`).
    SeekForward,
    /// Rewind scrub. Was previously unhandled (parsed as `None`).
    SeekBackward,
}

impl PlaybackStatus {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(PlaybackStatus::Stopped),
            1 => Some(PlaybackStatus::Playing),
            2 => Some(PlaybackStatus::Paused),
            3 => Some(PlaybackStatus::SeekForward),
            4 => Some(PlaybackStatus::SeekBackward),
            _ => None,
        }
    }
}

/// `RouteGuidanceState` — Apple `RouteGuidanceUpdate` (0x5201) param `1` **RouteGuidanceState** enum.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteGuidanceState {
    /// A route is not set.
    NoRouteSet,
    /// A route is set.
    RouteSet,
    /// Arrived at the destination.
    Arrived,
    /// Retrieving routing information.
    Loading,
    /// Location/position not available.
    Locating,
    /// Recalculating the route.
    Rerouting,
    /// Vehicle needs to join the route.
    ProceedToRoute,
}

impl RouteGuidanceState {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(RouteGuidanceState::NoRouteSet),
            1 => Some(RouteGuidanceState::RouteSet),
            2 => Some(RouteGuidanceState::Arrived),
            3 => Some(RouteGuidanceState::Loading),
            4 => Some(RouteGuidanceState::Locating),
            5 => Some(RouteGuidanceState::Rerouting),
            6 => Some(RouteGuidanceState::ProceedToRoute),
            _ => None,
        }
    }
}

/// `ManeuverState` — Apple `RouteGuidanceUpdate` (0x5201) param `2` **ManeuverState** enum. Describes
/// how imminent the current upcoming maneuver is.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManeuverState {
    /// Continue along this route until the next maneuver.
    Continue,
    /// Notification that a maneuver is in the near future.
    Initial,
    /// Notification that a maneuver is in the immediate future.
    Prepare,
    /// Vehicle is in the maneuver.
    Execute,
}

impl ManeuverState {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(ManeuverState::Continue),
            1 => Some(ManeuverState::Initial),
            2 => Some(ManeuverState::Prepare),
            3 => Some(ManeuverState::Execute),
            _ => None,
        }
    }
}

/// `DistanceUnit` — Apple's distance-display unit enum, shared by `RouteGuidanceUpdate` (0x5201)
/// params `9` **DistanceRemainingDisplayUnits** / `12`
/// **DistanceRemainingToNextManeuverDisplayUnits** and `RouteGuidanceManeuverInformation` (0x5202)
/// param `7` **DistanceBetweenManeuverDisplayUnits**. The accompanying `*DisplayString` must be shown
/// verbatim in these units, without unit conversion.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistanceUnit {
    Kilometers,
    Miles,
    Meters,
    Yards,
    Feet,
}

impl DistanceUnit {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(DistanceUnit::Kilometers),
            1 => Some(DistanceUnit::Miles),
            2 => Some(DistanceUnit::Meters),
            3 => Some(DistanceUnit::Yards),
            4 => Some(DistanceUnit::Feet),
            _ => None,
        }
    }
}

/// `ManeuverType` — Apple `RouteGuidanceManeuverInformation` (0x5202) param `3` **ManeuverType**
/// enum, verbatim from Apple's authoritative value table (0..=53). Previously the maneuver type was
/// not parsed at all, so the accessory had no way to pick a turn icon. Unknown/future values parse as
/// `None` (forward-compatible), mirroring the `PlaybackStatus` pattern.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManeuverType {
    NoTurn,
    LeftTurn,
    RightTurn,
    StraightAhead,
    UTurn,
    FollowRoad,
    EnterRoundabout,
    ExitRoundabout,
    OffRamp,
    OnRamp,
    ArriveEndOfNavigation,
    ProceedToRouteStart,
    ArriveAtDestination,
    KeepLeft,
    KeepRight,
    EnterFerry,
    ExitFerry,
    ChangeFerry,
    UTurnToRoute,
    RoundaboutUTurn,
    EndOfRoadLeft,
    EndOfRoadRight,
    HighwayOffRampLeft,
    HighwayOffRampRight,
    ArriveAtDestinationLeft,
    ArriveAtDestinationRight,
    UTurnWhenPossible,
    ArriveEndOfDirections,
    RoundaboutExit1,
    RoundaboutExit2,
    RoundaboutExit3,
    RoundaboutExit4,
    RoundaboutExit5,
    RoundaboutExit6,
    RoundaboutExit7,
    RoundaboutExit8,
    RoundaboutExit9,
    RoundaboutExit10,
    RoundaboutExit11,
    RoundaboutExit12,
    RoundaboutExit13,
    RoundaboutExit14,
    RoundaboutExit15,
    RoundaboutExit16,
    RoundaboutExit17,
    RoundaboutExit18,
    RoundaboutExit19,
    SharpLeftTurn,
    SharpRightTurn,
    SlightLeftTurn,
    SlightRightTurn,
    ChangeHighway,
    ChangeHighwayLeft,
    ChangeHighwayRight,
}

impl ManeuverType {
    pub fn from_u8(v: u8) -> Option<Self> {
        use ManeuverType::*;
        Some(match v {
            0 => NoTurn,
            1 => LeftTurn,
            2 => RightTurn,
            3 => StraightAhead,
            4 => UTurn,
            5 => FollowRoad,
            6 => EnterRoundabout,
            7 => ExitRoundabout,
            8 => OffRamp,
            9 => OnRamp,
            10 => ArriveEndOfNavigation,
            11 => ProceedToRouteStart,
            12 => ArriveAtDestination,
            13 => KeepLeft,
            14 => KeepRight,
            15 => EnterFerry,
            16 => ExitFerry,
            17 => ChangeFerry,
            18 => UTurnToRoute,
            19 => RoundaboutUTurn,
            20 => EndOfRoadLeft,
            21 => EndOfRoadRight,
            22 => HighwayOffRampLeft,
            23 => HighwayOffRampRight,
            24 => ArriveAtDestinationLeft,
            25 => ArriveAtDestinationRight,
            26 => UTurnWhenPossible,
            27 => ArriveEndOfDirections,
            28 => RoundaboutExit1,
            29 => RoundaboutExit2,
            30 => RoundaboutExit3,
            31 => RoundaboutExit4,
            32 => RoundaboutExit5,
            33 => RoundaboutExit6,
            34 => RoundaboutExit7,
            35 => RoundaboutExit8,
            36 => RoundaboutExit9,
            37 => RoundaboutExit10,
            38 => RoundaboutExit11,
            39 => RoundaboutExit12,
            40 => RoundaboutExit13,
            41 => RoundaboutExit14,
            42 => RoundaboutExit15,
            43 => RoundaboutExit16,
            44 => RoundaboutExit17,
            45 => RoundaboutExit18,
            46 => RoundaboutExit19,
            47 => SharpLeftTurn,
            48 => SharpRightTurn,
            49 => SlightLeftTurn,
            50 => SlightRightTurn,
            51 => ChangeHighway,
            52 => ChangeHighwayLeft,
            53 => ChangeHighwayRight,
            _ => return None,
        })
    }
}

/// Now Playing snapshot. Field names follow Apple's `NowPlayingUpdate` (0x5001) parameter names:
/// `title`/`artist`/`album`/`genre`/`composer` are `MediaItemAttributes` (group 0) params
/// Title(1)/Artist(12)/AlbumTitle(6)/Genre(16)/Composer(18); `duration_ms` is
/// MediaItemPlaybackDurationInMilliseconds(4); `track_number`/`track_count` are
/// MediaItemAlbumTrackNumber(7)/MediaItemAlbumTrackCount(8). `elapsed_ms`/`playback_status`/`app`/
/// `app_bundle_id` are `PlaybackAttributes` (group 1) params
/// PlaybackElapsedTimeInMilliseconds(1)/PlaybackStatus(0)/PlaybackAppName(7)/PlaybackAppBundleID(16).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct NowPlaying {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub composer: Option<String>,
    pub duration_ms: Option<u32>,
    pub track_number: Option<u16>,
    pub track_count: Option<u16>,
    pub elapsed_ms: Option<u32>,
    pub playback_status: Option<PlaybackStatus>,
    /// Human-facing app name (`PlaybackAppName`, id 7), e.g. "Music".
    pub app: Option<String>,
    /// Reverse-DNS bundle id (`PlaybackAppBundleID`, id 16), e.g. "com.apple.Music".
    pub app_bundle_id: Option<String>,
}

impl NowPlaying {
    /// Merge a newly-parsed delta into `self` in place: only overwrite fields the delta actually
    /// carried (`Some`), leaving everything else as the last known value. This is what keeps
    /// title/artist visible across the ~2/s elapsed-only frames -- see the module doc.
    pub fn merge(&mut self, delta: NowPlaying) {
        if delta.title.is_some() {
            self.title = delta.title;
        }
        if delta.artist.is_some() {
            self.artist = delta.artist;
        }
        if delta.album.is_some() {
            self.album = delta.album;
        }
        if delta.genre.is_some() {
            self.genre = delta.genre;
        }
        if delta.composer.is_some() {
            self.composer = delta.composer;
        }
        if delta.duration_ms.is_some() {
            self.duration_ms = delta.duration_ms;
        }
        if delta.track_number.is_some() {
            self.track_number = delta.track_number;
        }
        if delta.track_count.is_some() {
            self.track_count = delta.track_count;
        }
        if delta.elapsed_ms.is_some() {
            self.elapsed_ms = delta.elapsed_ms;
        }
        if delta.playback_status.is_some() {
            self.playback_status = delta.playback_status;
        }
        if delta.app.is_some() {
            self.app = delta.app;
        }
        if delta.app_bundle_id.is_some() {
            self.app_bundle_id = delta.app_bundle_id;
        }
    }
}

/// Route Guidance snapshot. Field names follow Apple's `RouteGuidanceUpdate` (0x5201) params:
/// `current_road`=CurrentRoadName(3), `destination`=DestinationName(4),
/// `arrival_time_epoch_s`=EstimatedTimeOfArrival(5), `time_remaining_s`=TimeRemainingToDestination(6),
/// `distance_remaining_m`=DistanceRemaining(7), `distance_remaining_text`=DistanceRemainingDisplayString(8),
/// `distance_remaining_units`=DistanceRemainingDisplayUnits(9),
/// `distance_to_next_maneuver_m`=DistanceRemainingToNextManeuver(10),
/// `distance_to_next_maneuver_text`=DistanceRemainingToNextManeuverDisplayString(11),
/// `distance_to_next_maneuver_units`=DistanceRemainingToNextManeuverDisplayUnits(12),
/// `route_state`=RouteGuidanceState(1), `maneuver_state`=ManeuverState(2), `nav_app`=SourceName(19).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct RouteGuidance {
    /// `RouteGuidanceState` (id 1): overall route status (no route / set / arrived / rerouting…).
    pub route_state: Option<RouteGuidanceState>,
    /// `ManeuverState` (id 2): imminence of the current upcoming maneuver.
    pub maneuver_state: Option<ManeuverState>,
    pub current_road: Option<String>,
    pub destination: Option<String>,
    /// Unix epoch seconds. NOTE: parsed as a raw 8-byte big-endian u64 off the wire, matching every
    /// other numeric TLV field in this protocol -- the reference project's own note that this
    /// arrives as an "8-byte hex STRING" describes their CCPA-side relay's own JSON re-encoding
    /// choice, not necessarily the raw iAP2 wire format. Flagged for live-device confirmation (see
    /// docs/host/00_MACOS_HOST_APP.md) since we parse directly off the real wire here, not through that relay.
    /// Apple: `EstimatedTimeOfArrival` (id 5), `uint64` seconds since 1970 GMT.
    pub arrival_time_epoch_s: Option<u64>,
    pub time_remaining_s: Option<u64>,
    /// `DistanceRemaining` (id 7), meters. Per Apple this MUST NOT be shown as text -- it is only for
    /// graphics (e.g. countdown bars). Show `distance_remaining_text`+`distance_remaining_units` instead.
    pub distance_remaining_m: Option<u32>,
    /// `DistanceRemainingDisplayString` (id 8): the display string, WITHOUT units. Show verbatim.
    pub distance_remaining_text: Option<String>,
    /// `DistanceRemainingDisplayUnits` (id 9): units to render alongside `distance_remaining_text`.
    pub distance_remaining_units: Option<DistanceUnit>,
    /// `DistanceRemainingToNextManeuver` (id 10), meters. Graphics only, like `distance_remaining_m`.
    pub distance_to_next_maneuver_m: Option<u32>,
    /// `DistanceRemainingToNextManeuverDisplayString` (id 11): display string, WITHOUT units.
    pub distance_to_next_maneuver_text: Option<String>,
    /// `DistanceRemainingToNextManeuverDisplayUnits` (id 12).
    pub distance_to_next_maneuver_units: Option<DistanceUnit>,
    pub nav_app: Option<String>,
}

impl RouteGuidance {
    pub fn merge(&mut self, delta: RouteGuidance) {
        if delta.route_state.is_some() {
            self.route_state = delta.route_state;
        }
        if delta.maneuver_state.is_some() {
            self.maneuver_state = delta.maneuver_state;
        }
        if delta.current_road.is_some() {
            self.current_road = delta.current_road;
        }
        if delta.destination.is_some() {
            self.destination = delta.destination;
        }
        if delta.arrival_time_epoch_s.is_some() {
            self.arrival_time_epoch_s = delta.arrival_time_epoch_s;
        }
        if delta.time_remaining_s.is_some() {
            self.time_remaining_s = delta.time_remaining_s;
        }
        if delta.distance_remaining_m.is_some() {
            self.distance_remaining_m = delta.distance_remaining_m;
        }
        if delta.distance_remaining_text.is_some() {
            self.distance_remaining_text = delta.distance_remaining_text;
        }
        if delta.distance_remaining_units.is_some() {
            self.distance_remaining_units = delta.distance_remaining_units;
        }
        if delta.distance_to_next_maneuver_m.is_some() {
            self.distance_to_next_maneuver_m = delta.distance_to_next_maneuver_m;
        }
        if delta.distance_to_next_maneuver_text.is_some() {
            self.distance_to_next_maneuver_text = delta.distance_to_next_maneuver_text;
        }
        if delta.distance_to_next_maneuver_units.is_some() {
            self.distance_to_next_maneuver_units = delta.distance_to_next_maneuver_units;
        }
        if delta.nav_app.is_some() {
            self.nav_app = delta.nav_app;
        }
    }
}

/// A single maneuver. Field names follow Apple's `RouteGuidanceManeuverInformation` (0x5202) params:
/// `index`=Index(1), `instruction`=ManeuverDescription(2), `maneuver_type`=ManeuverType(3),
/// `road`=AfterManeuverRoadName(4), `distance_m`=DistanceBetweenManeuver(5),
/// `distance_text`=DistanceBetweenManeuverDisplayString(6),
/// `distance_units`=DistanceBetweenManeuverDisplayUnits(7), `exit_info`=ExitInfo(13).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Maneuver {
    /// `Index` (id 1): which item in the maneuver list this update is for. Correlates with
    /// `RouteGuidanceUpdate`'s RouteGuidanceManeuverCurrentList / RouteGuidanceManeuverTotalCount.
    pub index: Option<u16>,
    pub instruction: Option<String>,
    /// `ManeuverType` (id 3): the turn/maneuver kind, for icon selection.
    pub maneuver_type: Option<ManeuverType>,
    /// `AfterManeuverRoadName` (id 4): the road the vehicle ends up on after the maneuver.
    pub road: Option<String>,
    /// `DistanceBetweenManeuver` (id 5), meters. Graphics only -- not to be shown as text.
    pub distance_m: Option<u32>,
    /// `DistanceBetweenManeuverDisplayString` (id 6): display string, WITHOUT units.
    pub distance_text: Option<String>,
    /// `DistanceBetweenManeuverDisplayUnits` (id 7).
    pub distance_units: Option<DistanceUnit>,
    /// `ExitInfo` (id 13): highway exit info (e.g. exit number). Only sent if SupportsExitInfo was
    /// requested in StartRouteGuidanceUpdates -- which the current subscribe does NOT request, so this
    /// stays `None` in practice until that subscribe opts in (see subscribe.rs).
    pub exit_info: Option<String>,
}

impl Maneuver {
    pub fn merge(&mut self, delta: Maneuver) {
        if delta.index.is_some() {
            self.index = delta.index;
        }
        if delta.instruction.is_some() {
            self.instruction = delta.instruction;
        }
        if delta.maneuver_type.is_some() {
            self.maneuver_type = delta.maneuver_type;
        }
        if delta.road.is_some() {
            self.road = delta.road;
        }
        if delta.distance_m.is_some() {
            self.distance_m = delta.distance_m;
        }
        if delta.distance_text.is_some() {
            self.distance_text = delta.distance_text;
        }
        if delta.distance_units.is_some() {
            self.distance_units = delta.distance_units;
        }
        if delta.exit_info.is_some() {
            self.exit_info = delta.exit_info;
        }
    }
}

/// The one envelope type that actually crosses the `carplayd` <-> `crates/ui` wire -- an
/// internally-tagged enum so a client only needs to deserialize one shape and match on it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type")]
pub enum MetadataMessage {
    NowPlaying(NowPlaying),
    RouteGuidance(RouteGuidance),
    Maneuver(Maneuver),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_only_overwrites_present_fields() {
        let mut snapshot = NowPlaying {
            title: Some("Track A".into()),
            artist: Some("Artist A".into()),
            elapsed_ms: Some(1000),
            ..Default::default()
        };
        // An elapsed-only delta, matching the ~2/s frames the reference project documented.
        let delta = NowPlaying { elapsed_ms: Some(2000), ..Default::default() };
        snapshot.merge(delta);
        assert_eq!(snapshot.title.as_deref(), Some("Track A"));
        assert_eq!(snapshot.artist.as_deref(), Some("Artist A"));
        assert_eq!(snapshot.elapsed_ms, Some(2000));
    }

    #[test]
    fn merge_overwrites_when_present() {
        let mut snapshot = NowPlaying { title: Some("Old".into()), ..Default::default() };
        snapshot.merge(NowPlaying { title: Some("New".into()), ..Default::default() });
        assert_eq!(snapshot.title.as_deref(), Some("New"));
    }

    #[test]
    fn message_envelope_round_trips_through_json() {
        let msg = MetadataMessage::NowPlaying(NowPlaying {
            title: Some("Foo".into()),
            ..Default::default()
        });
        let json = serde_json::to_string(&msg).unwrap();
        let back: MetadataMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }
}
