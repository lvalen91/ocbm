import Foundation

/// Android Auto metadata services — the phone's now-playing, turn-by-turn and call state, decoded
/// from the three head-unit-declared `gal.*` services (MediaPlaybackStatusService,
/// NavigationStatusService, PhoneStatusService). App-only: the box pumps opaque AA bytes.
///
/// Schema sources (2026-09-04): message ids and the media field layout are confirmed from the
/// decompiled gearhead 17.5 sender (`jav`: metadata id 32771 / status id 32769 / input 32770;
/// `xkl` = {1 song, 2 artist, 3 album, 4 art bytes, 5 playlist, 6 duration u32, 7 rating i32};
/// `xkm` = {1 state enum, 2 source, 3 seconds u32, 4/5/6 bools}). The navigation and phone
/// layouts are aasdk's (`NavigationNextTurnEvent` / `NavigationNextTurnDistanceEvent` /
/// `PhoneStatus`), which openauto drives real phones with; gearhead's navigation sender lives in
/// the Play Services car module and was not in the decompile. Anything unrecognised is surfaced
/// as `.raw` so the wire itself corrects the table.
enum AAMetadata {

    struct MediaMetadata {
        var song: String?, artist: String?, album: String?, playlist: String?
        var albumArt: Data?
        var durationSeconds: Int?
        var rating: Int?
    }

    struct MediaStatus {
        /// 1 STOPPED, 2 PLAYING, 3 PAUSED (gal `MediaPlaybackStatus.State`).
        var state: Int?
        var source: String?
        var playbackSeconds: Int?
        var shuffle: Bool?, repeatAll: Bool?, repeatOne: Bool?
    }

    struct NavTurn {
        var road: String?
        /// 1 LEFT, 2 RIGHT, 3 UNSPECIFIED.
        var side: Int?
        /// gal `NextTurnEnum`; see `turnName`.
        var event: Int?
        var image: Data?
        var turnNumber: Int?
        var turnAngle: Int?

        var description: String {
            let base = AAMetadata.turnName(event ?? 0)
            switch side {
            case 1: return base + " left"
            case 2: return base + " right"
            default: return base
            }
        }
    }

    struct NavDistance {
        var meters: Int?
        var secondsToTurn: Int?
        var displayE3: Int?
        /// 1 m, 2 km, 3 km (1 decimal), 4 mi, 5 mi (1 decimal), 6 ft, 7 yd.
        var unit: Int?

        var displayText: String? {
            guard let e3 = displayE3 else { return nil }
            let v = Double(e3) / 1000
            switch unit {
            case 1: return "\(Int(v.rounded())) m"
            case 2: return "\(Int(v.rounded())) km"
            case 3: return String(format: "%.1f km", v)
            case 4: return "\(Int(v.rounded())) mi"
            case 5: return String(format: "%.1f mi", v)
            case 6: return "\(Int(v.rounded())) ft"
            case 7: return "\(Int(v.rounded())) yd"
            default: return nil
            }
        }
    }

    struct NavLane {
        /// gal `NavigationLane.LaneDirection.Shape`: 0 UNKNOWN, 1 STRAIGHT, 2 SLIGHT_LEFT, 3 SLIGHT_RIGHT,
        /// 4 NORMAL_LEFT, 5 NORMAL_RIGHT, 6 SHARP_LEFT, 7 SHARP_RIGHT, 8 U_TURN_LEFT, 9 U_TURN_RIGHT.
        var shapes: [Int] = []
        var highlighted = false
    }

    /// `NavigationState` (id 32774) — the scheme this phone actually uses (device-observed
    /// 2026-09-04, gearhead 17.5 at protocol 1.7): the maneuver is a TYPE enum plus road and cue
    /// text; no image is sent even when the head unit declares IMAGE options. Only the first step
    /// is decoded — it is the upcoming one.
    struct NavState {
        /// gal `NavigationManeuver.NavigationType`; see `maneuverName` / `maneuverSymbol`.
        var maneuverType: Int?
        var roundaboutExitNumber: Int?
        var roundaboutExitAngle: Int?
        var road: String?
        var cue: [String] = []
        var lanes: [NavLane] = []
        var destinations: [String] = []
        var stepCount = 0
    }

    /// `NavigationCurrentPosition` (id 32775): distance/time to the step, ETA and remaining
    /// distance to the destination, current road.
    struct NavPosition {
        var stepMeters: Int?, stepDisplay: String?, stepUnit: Int?
        var secondsToStep: Int?
        var destMeters: Int?, destDisplay: String?, destUnit: Int?
        var eta: String?
        var secondsToArrival: Int?
        var currentRoad: String?

        var stepText: String? { AAMetadata.distanceText(stepDisplay, stepUnit) }
        var destText: String? { AAMetadata.distanceText(destDisplay, destUnit) }
    }

    struct PhoneCall {
        /// 0 UNKNOWN, 1 IN_CALL, 2 ON_HOLD, 3 INACTIVE, 4 INCOMING, 5 CONFERENCED, 6 MUTED.
        var state: Int?
        var durationSeconds: Int?
        var number: String?
        var callerId: String?
        var numberType: String?
        var thumbnail: Data?
    }

    enum Event {
        case mediaMetadata(MediaMetadata)
        case mediaStatus(MediaStatus)
        /// gal `NavigationStatusEnum`: 0 UNAVAILABLE, 1 ACTIVE, 2 INACTIVE, 3 REROUTING.
        case navStatus(Int)
        case navTurn(NavTurn)
        case navDistance(NavDistance)
        case navState(NavState)
        case navPosition(NavPosition)
        case phone(calls: [PhoneCall], signalStrength: Int?)
        /// A message on one of the three channels we did not model — logged, never dropped silently.
        case raw(channel: UInt8, id: UInt16, body: Data)
    }

    static func distanceText(_ display: String?, _ unit: Int?) -> String? {
        guard let d = display, !d.isEmpty else { return nil }
        switch unit {
        case 1: return d + " m"
        case 2, 3: return d + " km"
        case 4, 5: return d + " mi"
        case 6: return d + " ft"
        case 7: return d + " yd"
        default: return d
        }
    }

    /// gal `NavigationType` (0-42) as display text.
    static func maneuverName(_ t: Int) -> String {
        switch t {
        case 1: return "Depart"
        case 2: return "Continue"
        case 3: return "Keep left"
        case 4: return "Keep right"
        case 5: return "Slight left"
        case 6: return "Slight right"
        case 7: return "Turn left"
        case 8: return "Turn right"
        case 9: return "Sharp left"
        case 10: return "Sharp right"
        case 11: return "U-turn left"
        case 12: return "U-turn right"
        case 13: return "On-ramp, slight left"
        case 14: return "On-ramp, slight right"
        case 15: return "On-ramp left"
        case 16: return "On-ramp right"
        case 17: return "On-ramp, sharp left"
        case 18: return "On-ramp, sharp right"
        case 19: return "On-ramp, U-turn left"
        case 20: return "On-ramp, U-turn right"
        case 21: return "Exit, slight left"
        case 22: return "Exit, slight right"
        case 23: return "Exit left"
        case 24: return "Exit right"
        case 25: return "Fork left"
        case 26: return "Fork right"
        case 27: return "Merge left"
        case 28: return "Merge right"
        case 29: return "Merge"
        case 30: return "Enter roundabout"
        case 31: return "Exit roundabout"
        case 32, 33: return "Roundabout (clockwise)"
        case 34, 35: return "Roundabout (counter-clockwise)"
        case 36: return "Straight"
        case 37: return "Ferry"
        case 38: return "Train ferry"
        case 39: return "Destination"
        case 40: return "Destination ahead"
        case 41: return "Destination on the left"
        case 42: return "Destination on the right"
        default: return "Maneuver \(t)"
        }
    }

    /// SF Symbol for a `NavigationType` — the head unit draws the glyph in this scheme.
    static func maneuverSymbol(_ t: Int) -> String {
        switch t {
        case 1: return "location.fill"
        case 2, 36: return "arrow.up"
        case 3: return "arrow.up.left"
        case 4: return "arrow.up.right"
        case 5, 13, 21: return "arrow.up.left"
        case 6, 14, 22: return "arrow.up.right"
        case 7, 15, 23: return "arrow.turn.up.left"
        case 8, 16, 24: return "arrow.turn.up.right"
        case 9, 17: return "arrow.turn.down.left"
        case 10, 18: return "arrow.turn.down.right"
        case 11, 19: return "arrow.uturn.left"
        case 12, 20: return "arrow.uturn.right"
        case 25: return "arrow.triangle.branch"
        case 26: return "arrow.triangle.branch"
        case 27, 28, 29: return "arrow.triangle.merge"
        case 30, 31, 32, 33, 34, 35: return "arrow.triangle.2.circlepath"
        case 37, 38: return "ferry"
        case 39, 40: return "flag.checkered"
        case 41: return "flag.checkered"
        case 42: return "flag.checkered"
        default: return "questionmark.circle"
        }
    }

    static func turnName(_ e: Int) -> String {
        switch e {
        case 1: return "Depart"
        case 2: return "Continue onto"
        case 3: return "Slight turn"
        case 4: return "Turn"
        case 5: return "Sharp turn"
        case 6: return "U-turn"
        case 7: return "On ramp"
        case 8: return "Off ramp"
        case 9: return "Fork"
        case 10: return "Merge"
        case 11: return "Enter roundabout"
        case 12: return "Exit roundabout"
        case 13: return "Roundabout"
        case 14: return "Straight"
        case 16: return "Ferry"
        case 17: return "Train ferry"
        case 19: return "Destination"
        default: return "Maneuver \(e)"
        }
    }

    // MARK: decode

    static func decode(channel: UInt8, id: UInt16, body: Data) -> Event {
        switch (channel, id) {
        case (AAWire.chMediaPlayback, AAWire.mediaPlaybackMetadata):
            var m = MediaMetadata()
            AAWire.forEachField(body) { f in
                switch f {
                case .bytes(1, let d): m.song = str(d)
                case .bytes(2, let d): m.artist = str(d)
                case .bytes(3, let d): m.album = str(d)
                case .bytes(4, let d): m.albumArt = d
                case .bytes(5, let d): m.playlist = str(d)
                case .varint(6, let v): m.durationSeconds = Int(v)
                case .varint(7, let v): m.rating = Int(Int32(truncatingIfNeeded: v))
                default: break
                }
            }
            return .mediaMetadata(m)
        case (AAWire.chMediaPlayback, AAWire.mediaPlaybackStatus):
            var s = MediaStatus()
            AAWire.forEachField(body) { f in
                switch f {
                case .varint(1, let v): s.state = Int(v)
                case .bytes(2, let d): s.source = str(d)
                case .varint(3, let v): s.playbackSeconds = Int(v)
                case .varint(4, let v): s.shuffle = v != 0
                case .varint(5, let v): s.repeatAll = v != 0
                case .varint(6, let v): s.repeatOne = v != 0
                default: break
                }
            }
            return .mediaStatus(s)
        case (AAWire.chNavigationStatus, AAWire.navStatus):
            return .navStatus(Int(AAWire.getFieldVarint(body, 1) ?? 0))
        case (AAWire.chNavigationStatus, AAWire.navTurnEvent):
            var t = NavTurn()
            AAWire.forEachField(body) { f in
                switch f {
                case .bytes(1, let d): t.road = str(d)
                case .varint(2, let v): t.side = Int(v)
                case .varint(3, let v): t.event = Int(v)
                case .bytes(4, let d): t.image = d
                case .varint(5, let v): t.turnNumber = Int(Int32(truncatingIfNeeded: v))
                case .varint(6, let v): t.turnAngle = Int(Int32(truncatingIfNeeded: v))
                default: break
                }
            }
            return .navTurn(t)
        case (AAWire.chNavigationStatus, AAWire.navDistanceEvent):
            var d = NavDistance()
            AAWire.forEachField(body) { f in
                switch f {
                case .varint(1, let v): d.meters = Int(Int32(truncatingIfNeeded: v))
                case .varint(2, let v): d.secondsToTurn = Int(Int32(truncatingIfNeeded: v))
                case .varint(3, let v): d.displayE3 = Int(Int32(truncatingIfNeeded: v))
                case .varint(4, let v): d.unit = Int(v)
                default: break
                }
            }
            return .navDistance(d)
        case (AAWire.chNavigationStatus, AAWire.navState):
            var st = NavState()
            AAWire.forEachField(body) { f in
                switch f {
                case .bytes(1, let step):
                    st.stepCount += 1
                    guard st.stepCount == 1 else { return }
                    AAWire.forEachField(step) { g in
                        switch g {
                        case .bytes(1, let man):
                            AAWire.forEachField(man) { h in
                                switch h {
                                case .varint(1, let v): st.maneuverType = Int(v)
                                case .varint(2, let v): st.roundaboutExitNumber = Int(Int32(truncatingIfNeeded: v))
                                case .varint(3, let v): st.roundaboutExitAngle = Int(Int32(truncatingIfNeeded: v))
                                default: break
                                }
                            }
                        case .bytes(2, let road): st.road = AAWire.getFieldString(road, 1)
                        case .bytes(3, let lane):
                            var l = NavLane()
                            AAWire.forEachField(lane) { h in
                                if case .bytes(1, let dir) = h {
                                    AAWire.forEachField(dir) { k in
                                        switch k {
                                        case .varint(1, let v): l.shapes.append(Int(v))
                                        case .varint(2, let v): if v != 0 { l.highlighted = true }
                                        default: break
                                        }
                                    }
                                }
                            }
                            st.lanes.append(l)
                        case .bytes(4, let cue):
                            AAWire.forEachField(cue) { h in
                                if case .bytes(1, let d) = h, let t = str(d) { st.cue.append(t) }
                            }
                        default: break
                        }
                    }
                case .bytes(2, let dest):
                    if let a = AAWire.getFieldString(dest, 1) { st.destinations.append(a) }
                default: break
                }
            }
            return .navState(st)
        case (AAWire.chNavigationStatus, AAWire.navCurrentPosition):
            var p = NavPosition()
            func dist(_ d: Data) -> (Int?, String?, Int?) {
                var m: Int?, disp: String?, u: Int?
                AAWire.forEachField(d) { h in
                    switch h {
                    case .varint(1, let v): m = Int(Int32(truncatingIfNeeded: v))
                    case .bytes(2, let s): disp = str(s)
                    case .varint(3, let v): u = Int(v)
                    default: break
                    }
                }
                return (m, disp, u)
            }
            var firstDest = true
            AAWire.forEachField(body) { f in
                switch f {
                case .bytes(1, let sd):
                    AAWire.forEachField(sd) { g in
                        switch g {
                        case .bytes(1, let d): (p.stepMeters, p.stepDisplay, p.stepUnit) = dist(d)
                        case .varint(2, let v): p.secondsToStep = Int(Int64(bitPattern: v))
                        default: break
                        }
                    }
                case .bytes(2, let dd):
                    guard firstDest else { return }
                    firstDest = false
                    AAWire.forEachField(dd) { g in
                        switch g {
                        case .bytes(1, let d): (p.destMeters, p.destDisplay, p.destUnit) = dist(d)
                        case .bytes(2, let s): p.eta = str(s)
                        case .varint(3, let v): p.secondsToArrival = Int(Int64(bitPattern: v))
                        default: break
                        }
                    }
                case .bytes(3, let road): p.currentRoad = AAWire.getFieldString(road, 1)
                default: break
                }
            }
            return .navPosition(p)
        case (AAWire.chPhoneStatus, AAWire.phoneStatus):
            var calls: [PhoneCall] = []
            var signal: Int?
            AAWire.forEachField(body) { f in
                switch f {
                case .bytes(1, let d):
                    var c = PhoneCall()
                    AAWire.forEachField(d) { g in
                        switch g {
                        case .varint(1, let v): c.state = Int(v)
                        case .varint(2, let v): c.durationSeconds = Int(v)
                        case .bytes(3, let s): c.number = str(s)
                        case .bytes(4, let s): c.callerId = str(s)
                        case .bytes(5, let s): c.numberType = str(s)
                        case .bytes(6, let s): c.thumbnail = s
                        default: break
                        }
                    }
                    calls.append(c)
                case .varint(2, let v): signal = Int(v)
                default: break
                }
            }
            return .phone(calls: calls, signalStrength: signal)
        default:
            return .raw(channel: channel, id: id, body: body)
        }
    }

    private static func str(_ d: Data) -> String? { String(data: d, encoding: .utf8) }
}
