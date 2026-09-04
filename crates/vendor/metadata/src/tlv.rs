//! Parsing half of the iAP2 TLV group format `carplayd`'s own `iap2/message.rs::Tlv` only *builds*
//! (encodes). Group framing: `[len:BE16 (includes this 4-byte header)][param_id:BE16][value...]`,
//! groups can nest (a group's value is itself a sequence of groups) -- confirmed against real
//! device captures in the reference project's research (`WIRED_METADATA_PLANE.md`/
//! `WIRED_NAV_METADATA.md`).

use crate::types::{
    DistanceUnit, Maneuver, ManeuverState, ManeuverType, NowPlaying, PlaybackStatus, RouteGuidance,
    RouteGuidanceState,
};

/// Apple iAP2 parameter ids for the metadata-plane messages, decoded from Apple's first-party
/// `CarPlaySimulator.devicekitplugin` spec. Defined locally (not `use`d from
/// `carplay_iap2_core::spec`) because `carplay-iap2-core` DEPENDS ON this crate -- importing it back
/// would create a dependency cycle.
mod pid {
    /// `NowPlayingUpdate` (0x5001) top-level group ids.
    pub mod now_playing_group {
        /// `MediaItemAttributes` group.
        pub const MEDIA_ITEM_ATTRIBUTES: u16 = 0x0000;
        /// `PlaybackAttributes` group.
        pub const PLAYBACK_ATTRIBUTES: u16 = 0x0001;
    }
    /// `MediaItemAttributes` (group 0) sub-param ids.
    pub mod media_item {
        pub const TITLE: u16 = 1; // MediaItemTitle (utf8)
        pub const PLAYBACK_DURATION_MS: u16 = 4; // MediaItemPlaybackDurationInMilliseconds (msecs32)
        pub const ALBUM_TITLE: u16 = 6; // MediaItemAlbumTitle (utf8)
        pub const ALBUM_TRACK_NUMBER: u16 = 7; // MediaItemAlbumTrackNumber (uint16)
        pub const ALBUM_TRACK_COUNT: u16 = 8; // MediaItemAlbumTrackCount (uint16)
        pub const ARTIST: u16 = 12; // MediaItemArtist (utf8)
        pub const GENRE: u16 = 16; // MediaItemGenre (utf8)
        pub const COMPOSER: u16 = 18; // MediaItemComposer (utf8)
        /// Documented for completeness; deliberately not parsed here (album art rides a separate iAP2
        /// File Transfer sub-session). Kept so the spec id is discoverable at the parse site.
        #[allow(dead_code)]
        pub const ARTWORK_FILE_TRANSFER_ID: u16 = 26; // MediaItemArtworkFileTransferIdentifier (uint8)
    }
    /// `PlaybackAttributes` (group 1) sub-param ids.
    pub mod playback {
        pub const STATUS: u16 = 0; // PlaybackStatus (enum)
        pub const ELAPSED_TIME_MS: u16 = 1; // PlaybackElapsedTimeInMilliseconds (msecs32)
        pub const APP_NAME: u16 = 7; // PlaybackAppName (utf8)
        pub const APP_BUNDLE_ID: u16 = 16; // PlaybackAppBundleID (utf8)
    }
    /// `RouteGuidanceUpdate` (0x5201) flat param ids.
    pub mod route_guidance {
        pub const STATE: u16 = 1; // RouteGuidanceState (enum)
        pub const MANEUVER_STATE: u16 = 2; // ManeuverState (enum)
        pub const CURRENT_ROAD_NAME: u16 = 3; // CurrentRoadName (utf8)
        pub const DESTINATION_NAME: u16 = 4; // DestinationName (utf8)
        pub const ETA: u16 = 5; // EstimatedTimeOfArrival (uint64)
        pub const TIME_REMAINING: u16 = 6; // TimeRemainingToDestination (uint64)
        pub const DISTANCE_REMAINING: u16 = 7; // DistanceRemaining (uint32, graphics only)
        pub const DISTANCE_REMAINING_STR: u16 = 8; // DistanceRemainingDisplayString (utf8)
        pub const DISTANCE_REMAINING_UNITS: u16 = 9; // DistanceRemainingDisplayUnits (enum)
        pub const DISTANCE_TO_NEXT_MANEUVER: u16 = 10; // DistanceRemainingToNextManeuver (uint32)
        pub const DISTANCE_TO_NEXT_MANEUVER_STR: u16 = 11; // ...DisplayString (utf8)
        pub const DISTANCE_TO_NEXT_MANEUVER_UNITS: u16 = 12; // ...DisplayUnits (enum)
        pub const SOURCE_NAME: u16 = 19; // SourceName (utf8) -- the navigating app's name
    }
    /// `RouteGuidanceManeuverInformation` (0x5202) flat param ids.
    pub mod maneuver {
        pub const INDEX: u16 = 1; // Index (uint16)
        pub const DESCRIPTION: u16 = 2; // ManeuverDescription (utf8)
        pub const TYPE: u16 = 3; // ManeuverType (enum)
        pub const AFTER_ROAD_NAME: u16 = 4; // AfterManeuverRoadName (utf8)
        pub const DISTANCE_BETWEEN: u16 = 5; // DistanceBetweenManeuver (uint32, graphics only)
        pub const DISTANCE_BETWEEN_STR: u16 = 6; // DistanceBetweenManeuverDisplayString (utf8)
        pub const DISTANCE_BETWEEN_UNITS: u16 = 7; // DistanceBetweenManeuverDisplayUnits (enum)
        pub const EXIT_INFO: u16 = 13; // ExitInfo (utf8)
    }
}

/// Walk a flat sequence of `[len][pid][value]` groups with Apple's `I2MRichParser` delivered-size
/// guards. Bounds-checked, never panics on malformed/truncated input.
///
/// Mirrors the guard set in Apple's own `iAP2MessageKitCore` streaming parser
/// (`I2MRichParser` / `I2MRichParserState`, see BEHAVIORAL_REFERENCE §5.2). Each group here is one
/// iAP2 parameter; `len` is the group's *advertised length* (Apple's `advertisedMessageLength`,
/// header-inclusive) which is reconciled against the bytes actually **delivered** in `body`
/// (Apple's `dataLength`) before any slice is taken. The Apple guards enforced, in order:
///
/// 1. **Parameter header minimum** — a group needs ≥4 delivered bytes to read `[len:BE16][pid:BE16]`.
///    Apple: `"A minimum of %lu bytes are required for a parameter, but only %lu byte(s) available"`.
/// 2. **Advertised length is valid** — `len` must be ≥4 (it is header-inclusive, so a value below
///    the header size is nonsense). Apple: `"The parameter with ID %hu indicates a length of %hu
///    which is not valid"`.
/// 3. **Advertised length fits delivered bytes** — `i + len` must not run past `body.len()`; the
///    bounds check happens **before** slicing so we can never over-read. Apple:
///    `"offset is out of bounds"` / `"offset: %lu, length: %lu"` (`I2MRichParserState.bytesAtOffset`).
///
/// Recoverable vs. fatal (Apple `+[I2MRichParserState isErrorRecoverable:]`, §5.3): an **unknown or
/// oversized-for-its-type** optional parameter whose advertised `len` still *fits* the buffer is
/// recoverable — it is returned as a `(pid, value)` pair and the caller's `match` skips it by simply
/// not having an arm (i.e. skipped *by its advertised length*, the cursor already advanced `i += len`
/// past it). A **framing** error (header underflow, `len < 4`, or `len` exceeding delivered bytes) is
/// fatal for the remainder: we cannot know a safe resync offset, so we stop cleanly and return
/// everything parsed so far rather than misparsing garbage — matching this project's defensive style
/// (`iap2::link::Link::parse`) and never regressing already-parsed parameters.
pub fn parse_groups(body: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    // Guard 1: parameter-header minimum (need 4 delivered bytes for [len][pid]).
    while i + 4 <= body.len() {
        let len = u16::from_be_bytes([body[i], body[i + 1]]) as usize;
        // Guard 2 (len < 4: advertised length not valid) + Guard 3 (advertised > delivered): both
        // are fatal framing errors -- stop, keep what parsed. Checked BEFORE the slice below.
        if len < 4 || i + len > body.len() {
            break;
        }
        let pid = u16::from_be_bytes([body[i + 2], body[i + 3]]);
        out.push((pid, &body[i + 4..i + len]));
        i += len;
    }
    out
}

// Apple `I2MRichParser` rich-type accessors over a parsed group list (`firstParameterForID:` /
// `parametersForID:`, and their subparameter twins `firstSubparameterForID:` /
// `subparametersForID:`, BEHAVIORAL_REFERENCE §5.4). `parse_groups` is already the recursive GROUP
// primitive -- a group whose value is itself a sub-TLV list is parsed by re-invoking `parse_groups`
// on its value slice (as `parse_now_playing` does for `MediaItemAttributes`/`PlaybackAttributes`),
// which independently applies the same delivered-size guards at each nesting depth. These accessors
// add the *count-expression* half of Apple's grammar: selecting one, or **all N**, occurrences of a
// parameter id.
//
// Not wired into the current parse paths: the three messages this crate parses (`NowPlayingUpdate`
// 0x5001, `RouteGuidanceUpdate` 0x5201, `RouteGuidanceManeuverInformation` 0x5202) carry only
// unique, non-repeated parameter ids, so their `match`-loop parsers need neither accessor. They are
// the Apple-parity substrate for the repeated/array rich types that neighbouring messages use --
// `LaneGuidanceInformation` (0x5204) `LaneInformation` is a `card=2` **repeated group**, and the
// communications list messages repeat entries -- kept here so those parsers reuse one guarded
// substrate instead of re-deriving it. `#[allow(dead_code)]` documents the deliberate not-yet-wired
// status (same convention as `pid::media_item::ARTWORK_FILE_TRANSFER_ID`).

/// Apple `firstParameterForID:` / `firstSubparameterForID:` — the value of the first parameter with
/// `id`, or `None`. Last-write-wins scalar params (all of this crate's current messages) are read via
/// the `match` loops directly; this is for callers that want a single lookup.
#[allow(dead_code)]
pub(crate) fn first_param_for_id<'a>(groups: &[(u16, &'a [u8])], id: u16) -> Option<&'a [u8]> {
    groups.iter().find(|(pid, _)| *pid == id).map(|(_, val)| *val)
}

/// Apple `parametersForID:` / `subparametersForID:` — the values of **every** parameter with `id`, in
/// wire order, collected into a `Vec`. This is Apple's count-expression semantics
/// (`countExpression`, §5.4): the same parameter id appearing N times is a repeated element list, not
/// an overwrite. Needed for repeated groups (e.g. `LaneGuidanceInformation` `LaneInformation`
/// `card=2`) and list messages.
#[allow(dead_code)]
pub(crate) fn params_for_id<'a>(groups: &[(u16, &'a [u8])], id: u16) -> Vec<&'a [u8]> {
    groups.iter().filter(|(pid, _)| *pid == id).map(|(_, val)| *val).collect()
}

// Scalar TLV value decoders. `pub(crate)` so the vehicle/location/destination plane modules
// (`vehicle.rs`, `location.rs`, `destination.rs`) can share this one bounds-checked substrate rather
// than each re-deriving big-endian decoding. Widening visibility is additive: no external crate sees
// these (the crate root re-exports only the `types`/`wire` surface).
pub(crate) fn utf8(val: &[u8]) -> Option<String> {
    if val.is_empty() {
        return None;
    }
    let trimmed = if val.last() == Some(&0) { &val[..val.len() - 1] } else { val };
    Some(String::from_utf8_lossy(trimmed).into_owned())
}

pub(crate) fn u8v(val: &[u8]) -> Option<u8> {
    val.first().copied()
}

pub(crate) fn u16v(val: &[u8]) -> Option<u16> {
    (val.len() >= 2).then(|| u16::from_be_bytes([val[0], val[1]]))
}

pub(crate) fn u32v(val: &[u8]) -> Option<u32> {
    (val.len() >= 4).then(|| u32::from_be_bytes([val[0], val[1], val[2], val[3]]))
}

pub(crate) fn u64v(val: &[u8]) -> Option<u64> {
    (val.len() >= 8).then(|| {
        u64::from_be_bytes([val[0], val[1], val[2], val[3], val[4], val[5], val[6], val[7]])
    })
}

/// Apple iAP2 `uint16[]` **simple array** type (`I2MTypeUtils.isSimpleArrayType:` /
/// `elementTypeForSimpleArrayType:`, BEHAVIORAL_REFERENCE §5.4): a value that is a packed sequence of
/// big-endian `uint16` elements with no per-element framing. Used by e.g.
/// `RouteGuidanceUpdate` (0x5201) param 13 `RouteGuidanceManeuverCurrentList` (indexes of upcoming
/// maneuvers) and the IdentificationInformation message-id arrays. Delivered-size safe: a trailing
/// odd byte (value length not a multiple of 2) is ignored rather than over-read. Not currently wired
/// into a parse output (no consuming type field yet) -- provided as the guarded simple-array
/// substrate; see the accessor note above.
///
/// NOTE on **2D arrays** (`I2MTypeUtils.is2DArrayType:` / `sizeFor2DArray` / `I2M2DArraySize`, §5.4):
/// Apple's grammar has a distinct array-of-arrays type with an inner element size, which must not be
/// treated as a flat blob. **None of the three messages this crate parses (0x5001/0x5201/0x5202) use
/// a 2D-array parameter**, so no 2D size path is implemented here -- documented per the mandate. If a
/// future message needs it (Apple computes count via `countExpression` and inner size via
/// `sizeFor2DArray`), add it alongside `u16_array` rather than flattening.
#[allow(dead_code)]
pub(crate) fn u16_array(val: &[u8]) -> Vec<u16> {
    val.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect()
}

/// Signed 16-bit big-endian (Apple iAP2 `int16`, e.g. VehicleStatusUpdate OutsideTemperature in
/// degrees C, which can legitimately be negative).
pub(crate) fn i16v(val: &[u8]) -> Option<i16> {
    (val.len() >= 2).then(|| i16::from_be_bytes([val[0], val[1]]))
}

/// Signed 32-bit big-endian (Apple iAP2 `int32`, e.g. DestinationInformation Latitude/Longitude in
/// Q10.22 fixed-point degrees, which are signed for S latitude / W longitude).
pub(crate) fn i32v(val: &[u8]) -> Option<i32> {
    (val.len() >= 4).then(|| i32::from_be_bytes([val[0], val[1], val[2], val[3]]))
}

/// Apple iAP2 `bool`: a single non-zero byte is `true`. An empty value decodes to `None` (absent),
/// distinct from `Some(false)`.
pub(crate) fn boolv(val: &[u8]) -> Option<bool> {
    val.first().map(|&b| b != 0)
}

/// Parse a `NowPlayingUpdate` (0x5001, device-source) body: two nested groups, `MediaItemAttributes`
/// (group 0x0000) and `PlaybackAttributes` (group 0x0001). Param ids/types validated verbatim against
/// Apple's first-party spec. `MediaItemArtworkFileTransferIdentifier` (id 26) is intentionally
/// unhandled -- album art rides a separate iAP2 File Transfer sub-session, out of scope here.
pub fn parse_now_playing(body: &[u8]) -> NowPlaying {
    use pid::{media_item as mi, now_playing_group as grp, playback as pb};
    let mut np = NowPlaying::default();
    for (gid, gval) in parse_groups(body) {
        match gid {
            grp::MEDIA_ITEM_ATTRIBUTES => {
                for (pid, val) in parse_groups(gval) {
                    match pid {
                        mi::TITLE => np.title = utf8(val),
                        mi::PLAYBACK_DURATION_MS => np.duration_ms = u32v(val),
                        mi::ALBUM_TITLE => np.album = utf8(val),
                        mi::ALBUM_TRACK_NUMBER => np.track_number = u16v(val),
                        mi::ALBUM_TRACK_COUNT => np.track_count = u16v(val),
                        mi::ARTIST => np.artist = utf8(val),
                        mi::GENRE => np.genre = utf8(val),
                        mi::COMPOSER => np.composer = utf8(val),
                        _ => {}
                    }
                }
            }
            grp::PLAYBACK_ATTRIBUTES => {
                for (pid, val) in parse_groups(gval) {
                    match pid {
                        pb::STATUS => np.playback_status = u8v(val).and_then(PlaybackStatus::from_u8),
                        pb::ELAPSED_TIME_MS => np.elapsed_ms = u32v(val),
                        pb::APP_NAME => np.app = utf8(val),
                        pb::APP_BUNDLE_ID => np.app_bundle_id = utf8(val),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    np
}

/// Parse a `RouteGuidanceUpdate` (0x5201, device-source) body: FLAT params (not nested, unlike
/// NowPlaying), confirmed against the reference project's live device capture AND Apple's spec (all
/// 0x5201 params are top-level, not grouped). Param ids/types validated verbatim against the spec.
pub fn parse_route_guidance(body: &[u8]) -> RouteGuidance {
    use pid::route_guidance as rgp;
    let mut rg = RouteGuidance::default();
    for (pid, val) in parse_groups(body) {
        match pid {
            rgp::STATE => rg.route_state = u8v(val).and_then(RouteGuidanceState::from_u8),
            rgp::MANEUVER_STATE => rg.maneuver_state = u8v(val).and_then(ManeuverState::from_u8),
            rgp::CURRENT_ROAD_NAME => rg.current_road = utf8(val),
            rgp::DESTINATION_NAME => rg.destination = utf8(val),
            rgp::ETA => rg.arrival_time_epoch_s = u64v(val),
            rgp::TIME_REMAINING => rg.time_remaining_s = u64v(val),
            rgp::DISTANCE_REMAINING => rg.distance_remaining_m = u32v(val),
            rgp::DISTANCE_REMAINING_STR => rg.distance_remaining_text = utf8(val),
            rgp::DISTANCE_REMAINING_UNITS => {
                rg.distance_remaining_units = u8v(val).and_then(DistanceUnit::from_u8)
            }
            rgp::DISTANCE_TO_NEXT_MANEUVER => rg.distance_to_next_maneuver_m = u32v(val),
            rgp::DISTANCE_TO_NEXT_MANEUVER_STR => rg.distance_to_next_maneuver_text = utf8(val),
            rgp::DISTANCE_TO_NEXT_MANEUVER_UNITS => {
                rg.distance_to_next_maneuver_units = u8v(val).and_then(DistanceUnit::from_u8)
            }
            rgp::SOURCE_NAME => rg.nav_app = utf8(val),
            _ => {}
        }
    }
    rg
}

/// Parse a `RouteGuidanceManeuverInformation` (0x5202, device-source) body: flat params. Param
/// ids/types validated verbatim against Apple's spec.
pub fn parse_route_guidance_maneuver(body: &[u8]) -> Maneuver {
    use pid::maneuver as mvp;
    let mut m = Maneuver::default();
    for (pid, val) in parse_groups(body) {
        match pid {
            mvp::INDEX => m.index = u16v(val),
            mvp::DESCRIPTION => m.instruction = utf8(val),
            mvp::TYPE => m.maneuver_type = u8v(val).and_then(ManeuverType::from_u8),
            mvp::AFTER_ROAD_NAME => m.road = utf8(val),
            mvp::DISTANCE_BETWEEN => m.distance_m = u32v(val),
            mvp::DISTANCE_BETWEEN_STR => m.distance_text = utf8(val),
            mvp::DISTANCE_BETWEEN_UNITS => m.distance_units = u8v(val).and_then(DistanceUnit::from_u8),
            mvp::EXIT_INFO => m.exit_info = utf8(val),
            _ => {}
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a group the same way the wire does, for constructing test fixtures without hand-
    /// counting bytes each time.
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

    #[test]
    fn parse_groups_handles_the_documented_wire_capture() {
        // From the research capture: MediaItemAttributes group carrying Title/Duration/Album/Artist.
        // 50 01  00 59 00 00
        //   00 12 00 01 "Big Black Car\0"
        //   00 08 00 04 00 03 51 7a
        //   00 23 00 06 "This Empty Northern Hemisphere\0"
        //   00 18 00 0c "Gregory Alan Isakov\0"
        let inner = [
            group(0x01, &str_val("Big Black Car")),
            group(0x04, &217_466u32.to_be_bytes()),
            group(0x06, &str_val("This Empty Northern Hemisphere")),
            group(0x0c, &str_val("Gregory Alan Isakov")),
        ]
        .concat();
        let outer = group(0x0000, &inner);
        let np = parse_now_playing(&outer);
        assert_eq!(np.title.as_deref(), Some("Big Black Car"));
        assert_eq!(np.duration_ms, Some(217_466));
        assert_eq!(np.album.as_deref(), Some("This Empty Northern Hemisphere"));
        assert_eq!(np.artist.as_deref(), Some("Gregory Alan Isakov"));
    }

    #[test]
    fn parse_now_playing_playback_attributes() {
        let inner = [
            group(0x00, &[1]), // Playing
            group(0x01, &1500u32.to_be_bytes()),
            group(0x07, &str_val("Music")),
        ]
        .concat();
        let outer = group(0x0001, &inner);
        let np = parse_now_playing(&outer);
        assert_eq!(np.playback_status, Some(PlaybackStatus::Playing));
        assert_eq!(np.elapsed_ms, Some(1500));
        assert_eq!(np.app.as_deref(), Some("Music"));
    }

    #[test]
    fn parse_now_playing_ignores_unknown_groups_and_params() {
        let inner = [group(0x99, &str_val("unknown"))].concat();
        let outer = [group(0x0000, &inner), group(0x77, &[1, 2, 3])].concat();
        let np = parse_now_playing(&outer);
        assert_eq!(np, NowPlaying::default());
    }

    #[test]
    fn parse_route_guidance_flat_params() {
        let body = [
            group(3, &str_val("I-79 N")),
            group(4, &str_val("PPG Paints Arena")),
            group(6, &1920u64.to_be_bytes()), // 32 min
            group(7, &44_315u32.to_be_bytes()),
            group(19, &str_val("Apple Maps")),
        ]
        .concat();
        let rg = parse_route_guidance(&body);
        assert_eq!(rg.current_road.as_deref(), Some("I-79 N"));
        assert_eq!(rg.destination.as_deref(), Some("PPG Paints Arena"));
        assert_eq!(rg.time_remaining_s, Some(1920));
        assert_eq!(rg.distance_remaining_m, Some(44_315));
        assert_eq!(rg.nav_app.as_deref(), Some("Apple Maps"));
    }

    #[test]
    fn parse_maneuver_flat_params() {
        let body = [
            group(2, &str_val("79 North to Pittsburgh")),
            group(4, &str_val("US-79 N")),
            group(5, &1448u32.to_be_bytes()), // ~0.9 mi in metres
            group(6, &str_val("0.9")),
        ]
        .concat();
        let m = parse_route_guidance_maneuver(&body);
        assert_eq!(m.instruction.as_deref(), Some("79 North to Pittsburgh"));
        assert_eq!(m.road.as_deref(), Some("US-79 N"));
        assert_eq!(m.distance_m, Some(1448));
        assert_eq!(m.distance_text.as_deref(), Some("0.9"));
    }

    #[test]
    fn parse_now_playing_extended_media_item_fields() {
        // MediaItemAttributes carrying genre(16), composer(18), track number(7)/count(8, uint16).
        let inner = [
            group(16, &str_val("Indie Folk")),
            group(18, &str_val("Gregory Alan Isakov")),
            group(7, &3u16.to_be_bytes()),
            group(8, &12u16.to_be_bytes()),
        ]
        .concat();
        let np = parse_now_playing(&group(0x0000, &inner));
        assert_eq!(np.genre.as_deref(), Some("Indie Folk"));
        assert_eq!(np.composer.as_deref(), Some("Gregory Alan Isakov"));
        assert_eq!(np.track_number, Some(3));
        assert_eq!(np.track_count, Some(12));
    }

    #[test]
    fn parse_now_playing_seek_status_and_bundle_id() {
        // PlaybackStatus=3 (SeekForward) was previously dropped; PlaybackAppBundleID(16) is new.
        let inner =
            [group(0x00, &[3]), group(16, &str_val("com.apple.Music"))].concat();
        let np = parse_now_playing(&group(0x0001, &inner));
        assert_eq!(np.playback_status, Some(PlaybackStatus::SeekForward));
        assert_eq!(np.app_bundle_id.as_deref(), Some("com.apple.Music"));
    }

    #[test]
    fn parse_route_guidance_state_units_and_next_maneuver() {
        let body = [
            group(1, &[5]),  // RouteGuidanceState = Rerouting
            group(2, &[3]),  // ManeuverState = Execute
            group(9, &[1]),  // DistanceRemainingDisplayUnits = Miles
            group(10, &805u32.to_be_bytes()), // DistanceRemainingToNextManeuver (m)
            group(11, &str_val("0.5")),       // ...DisplayString
            group(12, &[1]),                  // ...DisplayUnits = Miles
        ]
        .concat();
        let rg = parse_route_guidance(&body);
        assert_eq!(rg.route_state, Some(RouteGuidanceState::Rerouting));
        assert_eq!(rg.maneuver_state, Some(ManeuverState::Execute));
        assert_eq!(rg.distance_remaining_units, Some(DistanceUnit::Miles));
        assert_eq!(rg.distance_to_next_maneuver_m, Some(805));
        assert_eq!(rg.distance_to_next_maneuver_text.as_deref(), Some("0.5"));
        assert_eq!(rg.distance_to_next_maneuver_units, Some(DistanceUnit::Miles));
    }

    #[test]
    fn parse_maneuver_index_type_units_and_exit_info() {
        let body = [
            group(1, &4u16.to_be_bytes()), // Index
            group(3, &[8]),                // ManeuverType = OffRamp
            group(7, &[1]),                // DistanceBetweenManeuverDisplayUnits = Miles
            group(13, &str_val("Exit 57A")), // ExitInfo
        ]
        .concat();
        let m = parse_route_guidance_maneuver(&body);
        assert_eq!(m.index, Some(4));
        assert_eq!(m.maneuver_type, Some(ManeuverType::OffRamp));
        assert_eq!(m.distance_units, Some(DistanceUnit::Miles));
        assert_eq!(m.exit_info.as_deref(), Some("Exit 57A"));
    }

    #[test]
    fn maneuver_type_covers_full_apple_enum_table() {
        // Apple's authoritative table is 0..=53; 54+ must fall back to None (forward-compatible).
        assert_eq!(ManeuverType::from_u8(0), Some(ManeuverType::NoTurn));
        assert_eq!(ManeuverType::from_u8(53), Some(ManeuverType::ChangeHighwayRight));
        assert_eq!(ManeuverType::from_u8(54), None);
    }

    #[test]
    fn parse_groups_stops_on_truncated_trailing_group_instead_of_panicking() {
        let mut body = group(1, b"ok");
        body.extend_from_slice(&[0x00, 0xFF]); // claims a 255-byte group that isn't there
        let groups = parse_groups(&body);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, 1);
    }

    // ---- Apple I2MRichParser delivered-size guard fixtures (BEHAVIORAL_REFERENCE §5.2/5.3) ----

    #[test]
    fn guard1_truncated_group_header_returns_partial_no_panic() {
        // Guard 1: a group needs >=4 delivered bytes for [len][pid]. A good group followed by a
        // 3-byte stub (header underflow) must yield the good group and stop -- never over-read.
        let mut body = group(2, b"hi");
        body.extend_from_slice(&[0x00, 0x08, 0x00]); // only 3 bytes: no complete parameter header
        let groups = parse_groups(&body);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, 2);
    }

    #[test]
    fn guard3_group_length_exceeding_delivered_buffer_stops_cleanly() {
        // Guard 3: advertised length must fit delivered bytes; the check is BEFORE the slice.
        // Advertise a 64-byte group in a 6-byte buffer -> fatal framing, return what parsed (none).
        let body = [0x00, 0x40, 0x00, 0x05, 0xAA, 0xBB];
        let groups = parse_groups(&body);
        assert!(groups.is_empty(), "must not slice past the delivered buffer");
        // And when a valid group precedes the oversized one, the valid one survives.
        let mut body2 = group(1, b"ok");
        body2.extend_from_slice(&[0x00, 0x40, 0x00, 0x05, 0xAA]);
        let groups2 = parse_groups(&body2);
        assert_eq!(groups2.len(), 1);
        assert_eq!(groups2[0].0, 1);
    }

    #[test]
    fn guard2_invalid_advertised_length_below_header_stops() {
        // Guard 2: a header-inclusive len < 4 is "not valid" (Apple's invalid-length string).
        let body = [0x00, 0x02, 0x00, 0x01, 0x00, 0x00]; // len=2, impossible (header is 4)
        assert!(parse_groups(&body).is_empty());
    }

    #[test]
    fn unknown_optional_param_is_skipped_by_advertised_length_recoverable() {
        // Recoverable (§5.3): an unknown optional param that FITS is skipped by its advertised
        // length and parsing continues -- the following known param is still recovered.
        let body = [
            group(0x00FE, &[1, 2, 3, 4, 5]), // unknown pid, well-formed length
            group(3, &str_val("I-79 N")),    // known: CurrentRoadName
        ]
        .concat();
        // The walker returns both; the RouteGuidance match simply has no arm for 0x00FE (skips it).
        let groups = parse_groups(&body);
        assert_eq!(groups.len(), 2);
        let rg = parse_route_guidance(&body);
        assert_eq!(rg.current_road.as_deref(), Some("I-79 N"));
    }

    #[test]
    fn zero_length_value_parses_as_empty_and_utf8_none() {
        // A group with len==4 (header only) carries a zero-length value: valid, not a framing error.
        let body = group(3, &[]); // CurrentRoadName with empty value
        let groups = parse_groups(&body);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].1, &[] as &[u8]);
        assert_eq!(utf8(groups[0].1), None); // empty -> absent
        // whole-message parse must not panic and yields no road name
        assert_eq!(parse_route_guidance(&body).current_road, None);
    }

    #[test]
    fn oversized_for_type_value_is_tolerated_not_fatal() {
        // A scalar param carrying MORE bytes than its type needs is recoverable: the typed decoder
        // reads the leading bytes and ignores the surplus (never a panic, never a whole-msg abort).
        let body = group(1, &[0x05, 0xDE, 0xAD, 0xBE, 0xEF]); // RouteGuidanceState (u8=5) + junk
        let rg = parse_route_guidance(&body);
        assert_eq!(rg.route_state, Some(RouteGuidanceState::Rerouting)); // first byte = 5
    }

    // ---- Apple rich-type fixtures: recursive group, repeated params, simple array (§5.4) ----

    #[test]
    fn nested_group_parses_recursively_at_each_depth() {
        // parse_groups is the recursive GROUP primitive: a group whose value is a sub-TLV list is
        // parsed by re-invoking it. NowPlaying nests MediaItemAttributes(0) -> Title(1).
        let inner = group(1, &str_val("Nested"));
        let outer = group(0x0000, &inner);
        let np = parse_now_playing(&outer);
        assert_eq!(np.title.as_deref(), Some("Nested"));
        // Direct accessor over the parsed outer level.
        let top = parse_groups(&outer);
        let media = first_param_for_id(&top, 0x0000).expect("MediaItemAttributes group present");
        let sub = parse_groups(media);
        assert_eq!(utf8(first_param_for_id(&sub, 1).unwrap()).as_deref(), Some("Nested"));
    }

    #[test]
    fn repeated_param_list_collects_all_occurrences_count_expression() {
        // Apple parametersForID: / countExpression -- the same id N times is a repeated list, not an
        // overwrite. (RouteGuidanceUpdate's params are unique, so this exercises the substrate that
        // LaneGuidanceInformation's card=2 LaneInformation and the comms-list messages rely on.)
        let body = [
            group(2, &str_val("lane A")),
            group(2, &str_val("lane B")),
            group(2, &str_val("lane C")),
            group(9, &[0]), // a different id interleaved
        ]
        .concat();
        let groups = parse_groups(&body);
        let lanes = params_for_id(&groups, 2);
        assert_eq!(lanes.len(), 3);
        let texts: Vec<_> = lanes.iter().map(|v| utf8(v).unwrap()).collect();
        assert_eq!(texts, ["lane A", "lane B", "lane C"]);
        assert_eq!(params_for_id(&groups, 9).len(), 1);
        assert!(params_for_id(&groups, 0xABCD).is_empty());
    }

    #[test]
    fn u16_simple_array_decodes_and_ignores_trailing_odd_byte() {
        // uint16[] simple-array type (e.g. RouteGuidanceManeuverCurrentList). Packed BE u16 elements.
        assert_eq!(u16_array(&[0x00, 0x01, 0x00, 0x2A, 0xFF, 0xFF]), vec![1, 42, 0xFFFF]);
        // Odd trailing byte (malformed length) is dropped, not over-read / panicked.
        assert_eq!(u16_array(&[0x00, 0x01, 0x99]), vec![1]);
        assert!(u16_array(&[]).is_empty());
    }

    #[test]
    fn deeply_nested_and_oversized_length_never_panics() {
        // Adversarial: a valid outer group whose inner content advertises an oversized sub-group.
        let mut inner = group(1, b"ok");
        inner.extend_from_slice(&[0x7F, 0xFF, 0x00, 0x09, 0x01]); // sub-group claims 32767 bytes
        let outer = group(0x0000, &inner);
        // Must not panic; recovers the well-formed inner Title and stops at the oversized sub-group.
        let np = parse_now_playing(&outer);
        assert_eq!(np.title.as_deref(), Some("ok"));
    }
}
