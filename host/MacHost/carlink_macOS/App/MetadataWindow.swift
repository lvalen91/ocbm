// MetadataWindow.swift — the Window ▸ Metadata viewer (user directive 2026-07-12).
//
// SwiftUI (NavigationSplitView + cards), hosted in this AppKit app via NSHostingView.
//
// Shows EVERY metadata category CarPlay can deliver to a wired accessory, grounded in Apple's own
// SDK parameter symbols read directly from the Xcode-local CarPlaySimulator plugin
// (iAP2MessageKit.framework exports `_iAP2NowPlayingUpdate_MediaItemAttributes_*`,
// `_iAP2RouteGuidanceUpdate_*`, `_iAP2CallStateUpdate_*`, `_iAP2ListUpdate_RecentsList_*`,
// `_iAP2CommunicationsUpdate_*`; the AirPlay `/command` family from CarPlaySDK). See
// docs/carplay/05_METADATA_AND_CONTROLS.md.
//
// THREE FEEDS, all over OCBM CH_METADATA:
//   • META_JSON    — iAP2 metadata parsed box-side by iap2d (media / nav / call / comms / recents)
//   • META_ARTWORK — the album-cover JPEG (iAP2 File-Transfer session)
//   • META_CMD     — inbound AirPlay /command plists (UI/session state) forwarded by airplayd
//
// LIVE-STATE SEMANTICS (per the directive): Media and Navigation hold a current snapshot merged from
// deltas and CLEARED when the activity ends (playback stopped / route ended). A call clears on
// disconnect and is appended to Call History.

import AppKit
import Combine
import Foundation
import SwiftUI
import os

// MARK: - Model

enum MetaCategory: String, CaseIterable, Identifiable {
    case media = "Media"
    case navigation = "Navigation"
    case phone = "Phone"
    case callHistory = "Call History"
    case telephony = "Telephony"
    case apps = "Apps"
    case favorites = "Favorites"
    case power = "Power"
    case device = "Device"
    case accessibility = "Accessibility"
    case modesFocus = "Modes / Focus"
    case audioControl = "Audio Control"
    case uiAppearance = "UI / Appearance"
    case siriSpeech = "Siri / Speech"
    case sessionLifecycle = "Session"
    case vehicle = "Vehicle / Logs"
    case other = "Other"

    var id: String { rawValue }

    var symbol: String {
        switch self {
        case .media: return "music.note"
        case .navigation: return "location.north.line.fill"
        case .phone: return "phone.fill"
        case .callHistory: return "clock.arrow.circlepath"
        case .telephony: return "antenna.radiowaves.left.and.right"
        case .apps: return "square.grid.2x2.fill"
        case .favorites: return "star.fill"
        case .power: return "battery.100.bolt"
        case .device: return "iphone"
        case .accessibility: return "figure.wave"
        case .modesFocus: return "rectangle.3.group"
        case .audioControl: return "speaker.wave.2.fill"
        case .uiAppearance: return "paintbrush.fill"
        case .siriSpeech: return "waveform"
        case .sessionLifecycle: return "bolt.horizontal.fill"
        case .vehicle: return "car.fill"
        case .other: return "ellipsis.circle"
        }
    }

    /// Why a category may be empty — shown inline so a blank pane is never ambiguous
    /// (docs/carplay/05_METADATA_AND_CONTROLS.md).
    var absenceNote: String {
        switch self {
        case .media:
            return "Nothing playing. Media metadata arrives as iAP2 NowPlaying (0x5001), subscribed at Identify — start playback on the iPhone."
        case .navigation:
            return "No active route. Turn-by-turn arrives as iAP2 RouteGuidance (0x5201/0x5202) only while Apple Maps is actively navigating."
        case .phone:
            return "No active call. Call state arrives as iAP2 CallStateUpdate (0x4155), subscribed at Identify."
        case .callHistory:
            return "No calls yet. iPhone Recents ride iAP2 ListUpdate (0x4171), which is not in the stock accessory's declared set and is likely gated on Bluetooth pairing/consent (docs/carplay/05_METADATA_AND_CONTROLS.md §5.6). We subscribe best-effort; calls observed live are recorded here regardless."
        case .telephony:
            return "No telephony status yet. iAP2 CommunicationsUpdate (0x4158) carries carrier, signal, call count and voicemail."
        case .favorites:
            return "No favorites yet. iAP2 ListUpdate (0x4171) carries the iPhone's Favorites alongside Recents, under the same Bluetooth contact-sharing consent."
        case .apps:
            return "No app list yet. iAP2 AppDiscoveryUpdate (0xAD01) needs the user to allow app-list sharing on the iPhone; icons follow as file transfers."
        case .power:
            return "No power update yet. iAP2 PowerUpdate (0xAE01) carries battery level, charging state and the current budget."
        case .device:
            return "Nothing yet. The 0x4E0x updates carry device name, language, time, UUID and the connected Bluetooth profiles."
        case .accessibility:
            return "Nothing yet. VoiceOver and AssistiveTouch declare accessory capabilities and are off unless the pushed metadata tier (Settings ▸ Configuration ▸ Advanced Capabilities ▸ Metadata declaration) is raised above the default."
        case .siriSpeech:
            return "Not expected. setEnhancedSiriParams only arrives if the accessory declares enhancedSiri, which this project does not (out of scope)."
        case .vehicle:
            return "Nothing yet. OEM log configuration and vehicle-information commands appear here."
        default:
            return "No \(rawValue) metadata received yet this session."
        }
    }
}

struct MetaEvent: Identifiable {
    let id = UUID()
    let date: Date
    let title: String
    let body: String
}

struct CallRecord: Identifiable {
    let id = UUID()
    let started: Date
    var ended: Date?
    var displayName: String
    var remoteId: String
    var direction: String
    var status: String
    var wasAnswered: Bool
}

struct MediaSnapshot {
    var title: String?, artist: String?, album: String?, genre: String?, composer: String?, appName: String?
    var durationMs: Int?, elapsedMs: Int?
    var trackNumber: Int?, trackCount: Int?, queueIndex: Int?, queueCount: Int?
    var shuffleMode: Int?, repeatMode: Int?, playbackStatus: Int?
    var artworkId: Int?
    var artwork: NSImage?
    /// 0x4C04 MediaLibraryUpdate revision — the library's version, not its contents.
    var libraryRevision: String?

    var isStopped: Bool { playbackStatus == 0 }
    /// Empty = nothing at all has arrived. NOTE elapsed/status count: iOS streams elapsed-only
    /// deltas between track changes, and the full attribute set (title/artist/album/artwork) is
    /// pushed on a TRACK CHANGE. Subscribing mid-track therefore yields progress-only frames until
    /// the next track — so we render the card as soon as ANY playback data exists rather than
    /// showing a misleading "nothing playing" while music is audibly playing (2026-07-12).
    var isEmpty: Bool {
        title == nil && artist == nil && album == nil && appName == nil
            && playbackStatus == nil && elapsedMs == nil
    }
    /// True when playback is live but the full track attributes haven't been pushed yet.
    var awaitingTrackInfo: Bool { title == nil && (elapsedMs != nil || playbackStatus != nil) }
    var stateText: String? {
        guard let s = playbackStatus else { return nil }
        // 0 stop 1 play 2 pause 3 seekFwd 4 seekBack — a 3-entry table clamped 3/4 into "Paused".
        return ["Stopped", "Playing", "Paused", "Seeking forward", "Seeking backward"][max(0, min(s, 4))]
    }
    var progress: Double? {
        guard let e = elapsedMs, let d = durationMs, d > 0 else { return nil }
        return min(1, Double(e) / Double(d))
    }
}

/// Apple's DistanceUnit enum (0x5201 params 9/12, 0x5202 param 7). The accompanying *DisplayString
/// must be shown verbatim in these units — no conversion.
private func unitSuffix(_ v: Int?) -> String {
    switch v {
    case 0: return " km"
    case 1: return " mi"
    case 2: return " m"
    case 3: return " yd"
    case 4: return " ft"
    default: return ""
    }
}

/// One maneuver from the route's list (0x5202). Keyed by the `maneuvers` dictionary index; 0x5201 says which is current.
/// One lane out of a 0x5204 LaneGuidanceInformation message.
struct Lane {
    /// `laneStatus` 0/1/2 mapped to "avoid" / "ok" / "preferred".
    var status: String
    /// Apple's `laneAngle` / `laneAngleHighlight`, decoded by the box as a signed 16-bit value.
    /// **Units are UNVERIFIED** — degrees and decidegrees both fit what we have seen, so these are
    /// carried and displayed raw rather than converted. Do not add a "°" until a source says so.
    var angle: Int?
    var highlightAngle: Int?
}

struct Maneuver {
    var description: String?
    var afterRoadName: String?
    var distanceText: String?
    var distanceUnits: Int?
    var exitInfo: String?

    var distanceDisplay: String? {
        guard let d = distanceText, !d.isEmpty else { return nil }
        return d + unitSuffix(distanceUnits)
    }
}

struct NavSnapshot {
    var destinationName: String?, currentRoadName: String?
    var timeRemainingSec: Int?, distanceRemainingM: Int?
    var distanceRemainingText: String?, distanceRemainingUnits: Int?
    var distanceToManeuverText: String?, distanceToManeuverUnits: Int?
    var routeGuidanceState: Int?
    /// 0x10DB DestinationInformation — what a CarPlay navigation app set as the destination.
    var destinationCoordinate: String?
    var destinationAddress: String?
    var destinationSource: String?
    /// 0x5204 LaneGuidanceInformation, keyed by lane index.
    ///
    /// Lanes belong to a MANEUVER, not to the route: each 0x5204 carries one `guidanceIndex` and a
    /// repeated lane group. `lanesGuidanceIndex` records whose lanes these are so a new junction
    /// REPLACES the set instead of merging into it — merging would show the union of every junction
    /// driven so far.
    var lanes: [Int: Lane] = [:]
    var lanesGuidanceIndex: Int?
    /// The whole maneuver list, keyed by Apple's maneuver index, plus which one is current.
    var maneuvers: [Int: Maneuver] = [:]
    var currentManeuverIndex: Int?
    /// Android Auto only: the phone renders the upcoming-maneuver glyph itself and ships it as an
    /// image (`NavigationNextTurnEvent.image`, sized by the options we declared). CarPlay sends a
    /// maneuver TYPE, not a picture, so this stays nil on iAP2.
    var maneuverImage: NSImage?
    /// Android Auto (NavigationState scheme): the phone sends a maneuver TYPE, so the head unit
    /// draws the glyph — an SF Symbol name chosen by `AAMetadata.maneuverSymbol`.
    var maneuverSymbol: String?
    /// Android Auto: the phone's own arrival-time string ("8:24 AM").
    var etaText: String?

    /// The live turn: the maneuver 0x5201 names as current (falls back to the lowest index seen).
    var currentManeuver: Maneuver? {
        if let i = currentManeuverIndex, let m = maneuvers[i] { return m }
        return maneuvers.keys.min().flatMap { maneuvers[$0] }
    }
    /// Distance to the next turn — 0x5201's live value wins (it updates ~1/s as you drive).
    var distanceToTurn: String? {
        if let t = distanceToManeuverText, !t.isEmpty { return t + unitSuffix(distanceToManeuverUnits) }
        return currentManeuver?.distanceDisplay
    }
    var distanceLeft: String? {
        if let t = distanceRemainingText, !t.isEmpty { return t + unitSuffix(distanceRemainingUnits) }
        return distanceRemainingM.map { "\($0) m" }
    }
    /// ETA computed from the LIVE time-remaining rather than the raw epoch param: Apple's
    /// EstimatedTimeOfArrival is expressed against the DESTINATION's timezone (param 24 carries the
    /// offset), so rendering it in the Mac's locale showed a 4-hour-shifted clock (2026-07-12).
    /// now + timeRemaining is unambiguous and matches what CarPlay itself displays.
    var eta: Date? {
        guard let t = timeRemainingSec, t > 0 else { return nil }
        return Date().addingTimeInterval(TimeInterval(t))
    }

    var isEnded: Bool { routeGuidanceState == 0 }
    /// `lanes` counts: a 0x5204 can arrive before any 0x5201/destination has populated the rest, and
    /// without this such an update renders as EmptyPane — data received and then hidden.
    var isEmpty: Bool {
        destinationName == nil && currentRoadName == nil && maneuvers.isEmpty && lanes.isEmpty
    }
}

// MARK: - Store (ObservableObject — SwiftUI drives off @Published)

@MainActor
final class MetadataStore: ObservableObject {
    static let shared = MetadataStore()
    /// Shared trim bound for `callHistory` (was 100 for live-call ends, 200 for Recents — two caps
    /// trimming opposite ends of the same array). `HistoryPane` sorts by `started` descending, so a
    /// single cap is enough; it no longer matters which end insert/append land on.
    static let callHistoryCap = 200

    @Published private(set) var media = MediaSnapshot()
    @Published private(set) var nav = NavSnapshot()
    @Published private(set) var activeCall: CallRecord?
    @Published private(set) var callHistory: [CallRecord] = []
    @Published private(set) var telephony: [String: String] = [:]
    /// 0xAE01 PowerUpdate, 0x4E0x device updates — flat key/value like `telephony`.
    @Published private(set) var power: [String: String] = [:]
    @Published private(set) var device: [String: String] = [:]
    /// 0xAD01 AppDiscoveryUpdate, keyed by bundle id so repeats replace rather than append.
    @Published private(set) var apps: [String: String] = [:]
    /// 0x4171 FavoritesList entries, keyed by display name.
    @Published private(set) var favorites: [String: String] = [:]
    /// 0x4171's list availability/count scalars — the phone's own view of both lists. Deliberately
    /// NOT merged into `favorites` or `callHistory`: those are counted for the sidebar badge, and
    /// status rows are not contacts. Rendered as a second card in both panes instead.
    @Published private(set) var callLists: [String: String] = [:]
    @Published private(set) var events: [MetaCategory: [MetaEvent]] = [:]

    private let log = Logger(subsystem: "com.carlink.ocbm", category: "metadata")
    private var artworkCache: [Int: NSImage] = [:]
    private var jsonCount = 0
    /// Kinds already logged, so each is reported once regardless of where it falls in the sampler.
    private var seenKinds = Set<String>()

    /// Seam reassembly runs off the OCBM read queue; only complete messages hop to the main actor.
    nonisolated private let seamLock = NSLock()
    nonisolated(unsafe) private var seam: [UInt8] = []

    // MARK: Android Auto source
    //
    // The AA engine decodes gal MediaPlaybackStatus / NavigationStatus / PhoneStatus messages on
    // the session thread and hands plain values here; the panes are shared with the iAP2 source.
    // Delta semantics match `nowPlaying`: a nil leaves the field alone, so a status-only frame
    // never blanks the title. Everything is main-actor because the panes observe these.

    struct AAMediaUpdate {
        var title: String?, artist: String?, album: String?, appName: String?
        var durationMs: Int?, elapsedMs: Int?
        /// Store convention (iAP2 PlaybackStatus): 0 stopped, 1 playing, 2 paused.
        var playbackStatus: Int?
        var shuffleMode: Int?, repeatMode: Int?
        var artwork: NSImage?
    }

    struct AANavUpdate {
        /// nil = leave; `.some(nil)` = the phone said navigation stopped.
        var active: Bool?
        var maneuverDescription: String?
        var nextRoad: String?
        var distanceText: String?
        var distanceMeters: Int?
        var timeToTurnSec: Int?
        var image: NSImage?
        var symbol: String?
        var exitInfo: String?
        var lanes: [(shapes: [Int], highlighted: Bool)]?
        var destinationName: String?
        var currentRoad: String?
        var destDistanceText: String?
        var destMeters: Int?
        var secondsToArrival: Int?
        var etaText: String?
    }

    @MainActor
    func applyAAMedia(_ u: AAMediaUpdate) {
        if let v = u.title { media.title = v }
        if let v = u.artist { media.artist = v }
        if let v = u.album { media.album = v }
        if let v = u.appName { media.appName = v }
        if let v = u.durationMs { media.durationMs = v }
        if let v = u.elapsedMs { media.elapsedMs = v }
        if let v = u.playbackStatus { media.playbackStatus = v }
        if let v = u.shuffleMode { media.shuffleMode = v }
        if let v = u.repeatMode { media.repeatMode = v }
        if let v = u.artwork { media.artwork = v; media.artworkId = nil }
        if media.isStopped { media = MediaSnapshot() }   // CLEAR ON MEDIA END, as iAP2 does
    }

    @MainActor
    func applyAANav(_ u: AANavUpdate) {
        if u.active == false { nav = NavSnapshot(); return }
        if u.active == true, nav.routeGuidanceState == nil { nav.routeGuidanceState = 1 }
        if let v = u.destinationName { nav.destinationName = v }
        var m = nav.maneuvers[0] ?? Maneuver()
        if let v = u.maneuverDescription { m.description = v }
        if let v = u.nextRoad { m.afterRoadName = v }
        if let v = u.distanceText { m.distanceText = v; nav.distanceToManeuverText = v; nav.distanceToManeuverUnits = nil }
        else if let mtr = u.distanceMeters { let t = mtr >= 1000 ? String(format: "%.1f km", Double(mtr) / 1000) : "\(mtr) m"
            m.distanceText = t; nav.distanceToManeuverText = t; nav.distanceToManeuverUnits = nil }
        if let v = u.timeToTurnSec, u.secondsToArrival == nil, nav.timeRemainingSec == nil { nav.timeRemainingSec = v }
        if let v = u.image { nav.maneuverImage = v }
        if let v = u.symbol { nav.maneuverSymbol = v }
        if let v = u.exitInfo { m.exitInfo = v }
        if let ls = u.lanes {
            nav.lanes = Dictionary(uniqueKeysWithValues: ls.enumerated().map { i, l in
                (i + 1, Lane(status: l.highlighted ? "preferred" : "ok", angle: nil, highlightAngle: nil))
            })
            nav.lanesGuidanceIndex = 0
        }
        if let v = u.currentRoad { nav.currentRoadName = v }
        if let v = u.destDistanceText { nav.distanceRemainingText = v; nav.distanceRemainingUnits = nil }
        if let v = u.destMeters { nav.distanceRemainingM = v }
        if let v = u.secondsToArrival { nav.timeRemainingSec = v }
        if let v = u.etaText { nav.etaText = v }
        nav.maneuvers[0] = m
        nav.currentManeuverIndex = 0
    }

    /// Adapter from the AA engine's decoded events to the store's own update shapes.
    @MainActor
    func applyAndroidAuto(_ ev: AAMetadata.Event) {
        switch ev {
        case .mediaMetadata(let m):
            applyAAMedia(AAMediaUpdate(title: m.song, artist: m.artist, album: m.album,
                                       durationMs: m.durationSeconds.map { $0 * 1000 },
                                       artwork: m.albumArt.flatMap { NSImage(data: $0) }))
        case .mediaStatus(let st):
            // gal State 1 STOPPED / 2 PLAYING / 3 PAUSED -> store (iAP2) 0 / 1 / 2.
            let status: Int? = st.state.map { $0 == 2 ? 1 : ($0 == 3 ? 2 : 0) }
            let rep: Int? = (st.repeatOne == true) ? 1 : (st.repeatAll == true ? 2 : (st.repeatAll == false ? 0 : nil))
            applyAAMedia(AAMediaUpdate(appName: st.source,
                                       elapsedMs: st.playbackSeconds.map { $0 * 1000 },
                                       playbackStatus: status,
                                       shuffleMode: st.shuffle.map { $0 ? 1 : 0 }, repeatMode: rep))
        case .navStatus(let v):
            applyAANav(AANavUpdate(active: v == 1 || v == 3))
        case .navTurn(let t):
            applyAANav(AANavUpdate(maneuverDescription: t.description, nextRoad: t.road,
                                   image: t.image.flatMap { NSImage(data: $0) }))
        case .navDistance(let d):
            applyAANav(AANavUpdate(distanceText: d.displayText, distanceMeters: d.meters,
                                   timeToTurnSec: d.secondsToTurn))
        case .navState(let st):
            let t = st.maneuverType ?? 0
            var desc = AAMetadata.maneuverName(t)
            if let n = st.roundaboutExitNumber, n > 0 { desc += ", exit \(n)" }
            applyAANav(AANavUpdate(active: true, maneuverDescription: desc, nextRoad: st.road,
                                   symbol: AAMetadata.maneuverSymbol(t), exitInfo: st.cue.first,
                                   lanes: st.lanes.map { (shapes: $0.shapes, highlighted: $0.highlighted) },
                                   destinationName: st.destinations.first))
        case .navPosition(let p):
            applyAANav(AANavUpdate(distanceText: p.stepText, distanceMeters: p.stepMeters,
                                   timeToTurnSec: p.secondsToStep, currentRoad: p.currentRoad,
                                   destDistanceText: p.destText, destMeters: p.destMeters,
                                   secondsToArrival: p.secondsToArrival, etaText: p.eta))
        case .phone(let calls, _):
            applyAAPhone(calls)
        case .raw:
            break
        }
    }

    /// PhoneStatus: the first call that is not INACTIVE becomes the active call; none clears it.
    @MainActor
    func applyAAPhone(_ calls: [AAMetadata.PhoneCall]) {
        let live = calls.first { ($0.state ?? 3) != 3 && ($0.state ?? 0) != 0 }
        guard let c = live else {
            if let a = activeCall {
                var ended = a; ended.ended = Date(); ended.status = "ended"
                callHistory.insert(ended, at: 0)
                if callHistory.count > Self.callHistoryCap { callHistory.removeLast(callHistory.count - Self.callHistoryCap) }
                activeCall = nil
            }
            return
        }
        let status: String
        switch c.state {
        case 4: status = "incoming"
        case 2: status = "on hold"
        case 5: status = "conference"
        case 6: status = "muted"
        default: status = "active"
        }
        if var a = activeCall {
            a.status = status; a.wasAnswered = a.wasAnswered || c.state == 1
            if let n = c.number, !n.isEmpty { a.remoteId = n }
            if let id = c.callerId, !id.isEmpty { a.displayName = id }
            activeCall = a
        } else {
            activeCall = CallRecord(started: Date().addingTimeInterval(-Double(c.durationSeconds ?? 0)),
                                    ended: nil, displayName: c.callerId ?? c.number ?? "Unknown",
                                    remoteId: c.number ?? "", direction: c.state == 4 ? "incoming" : "outgoing",
                                    status: status, wasAnswered: c.state == 1)
        }
    }

    /// The AA session ended: drop what it had shown so the panes do not keep a dead route/track.
    @MainActor
    func clearAndroidAuto() {
        media = MediaSnapshot()
        nav = NavSnapshot()
        activeCall = nil
    }

    func count(for cat: MetaCategory) -> Int {
        switch cat {
        case .media: return media.isEmpty ? 0 : 1
        case .navigation: return nav.isEmpty ? 0 : 1
        case .phone: return activeCall == nil ? 0 : 1
        case .callHistory: return callHistory.count
        case .telephony: return telephony.count
        case .power: return power.count
        case .device: return device.count
        case .apps: return apps.count
        case .favorites: return favorites.count
        default: return events[cat]?.count ?? 0
        }
    }

    func resetSession() {
        media = MediaSnapshot(); nav = NavSnapshot(); activeCall = nil
        telephony.removeAll(); events.removeAll(); artworkCache.removeAll()
        power.removeAll(); device.removeAll(); apps.removeAll(); favorites.removeAll()
        callLists.removeAll(); seenKinds.removeAll()
        jsonCount = 0 // per-session numbering, so `#N` and the `<= 3` branch mean what they say
        seamLock.lock(); seam.removeAll(); seamLock.unlock()
        CarPlayCornerMask.clearSessionCorner() // next session starts from the bundled fallback until its mask arrives
        // Call history deliberately survives — it IS the history.
    }

    /// Feed raw CH_METADATA payload bytes. Called on the OCBM read queue (nonisolated).
    nonisolated func ingest(_ chunk: [UInt8]) {
        var msgs: [(UInt8, [UInt8])] = []
        seamLock.lock()
        seam.append(contentsOf: chunk)
        // Seam framing v2 (audit Fix #17): [u32 BE MAGIC "META"][u32 BE len][marker][payload]. The magic
        // lets us deterministically resync to the next frame after a truncated one (a partial write on the
        // box, or an OCBM cap-clear dropping a metadata chunk) — dropping one byte at a time until the
        // magic re-appears, instead of the old length-only heuristic that could swallow real frames.
        while seam.count >= 8 {
            if seam[0] != 0x4D || seam[1] != 0x45 || seam[2] != 0x54 || seam[3] != 0x41 {
                seam.removeFirst(1); continue // not at a frame boundary — scan forward for the magic
            }
            let mlen = (Int(seam[4]) << 24) | (Int(seam[5]) << 16) | (Int(seam[6]) << 8) | Int(seam[7])
            if mlen == 0 || mlen > 16 << 20 { seam.removeFirst(1); continue } // coincidental magic → resync
            if seam.count < 8 + mlen { break } // full frame not here yet
            let msg = Array(seam[8..<8 + mlen])
            seam.removeFirst(8 + mlen)
            if let m = msg.first, msg.count > 1 { msgs.append((m, Array(msg[1...]))) }
        }
        seamLock.unlock()
        guard !msgs.isEmpty else { return }
        // FIFO-preserving main-actor hop (mirrors OCBMClient.deliver): an unstructured `Task {
        // @MainActor }` here has no cross-task ordering guarantee, so two frames from consecutive
        // `ingest` calls (e.g. callState status 4 then 0) could apply reversed. The main queue IS the
        // MainActor executor, so `assumeIsolated` is sound and keeps consecutive deltas ordered.
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated {
                guard let self else { return }
                for (marker, payload) in msgs {
                    switch marker {
                    case 0x01: self.handleCommandPlist(Data(payload))
                    case 0x02: self.handleJSON(Data(payload))
                    case 0x03: self.handleArtwork(payload)
                    case 0x04: self.handleCornerMask(payload)
                    default: break
                    }
                }
            }
        }
    }

    // MARK: feed handlers (main actor)

    /// META_CORNERMASK (0x04): `[u32 BE display_width][PNG]` — iOS's exact per-resolution
    /// `topLeftCornerMask`. Install it; `CarPlayCornerMask` posts `.carPlayCornerMaskUpdated` so the
    /// video views re-render the corner from iOS's real curve (docs/carplay/06_AV_PIPELINE.md Phase-3b).
    private func handleCornerMask(_ payload: [UInt8]) {
        guard payload.count > 4 else {
            NSLog("[cornermask] META_CORNERMASK too short (\(payload.count) B)")
            return
        }
        let width = (Int(payload[0]) << 24) | (Int(payload[1]) << 16) | (Int(payload[2]) << 8) | Int(payload[3])
        CarPlayCornerMask.setSessionCorner(Data(payload[4...]), displayWidth: width)
    }


    private func handleArtwork(_ bytes: [UInt8]) {
        guard let id = bytes.first, bytes.count > 1, let img = NSImage(data: Data(bytes[1...])) else { return }
        artworkCache[Int(id)] = img
        // The wire id is ONE byte; media.artworkId is the full iAP2 integer. Compare its truncated
        // low byte (the cache-key convention used everywhere else) — a full-int compare never matched
        // for ids ≥ 256, so late-arriving art for the current track was silently dropped.
        if media.artworkId.map({ $0 & 0xFF }) == Int(id) || media.artwork == nil { media.artwork = img }
        log.info("album artwork received id=\(id, privacy: .public) (\(bytes.count - 1) B)")
    }

    private func handleJSON(_ data: Data) {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let kind = obj["kind"] as? String else {
            log.error("undecodable metadata JSON (\(data.count) B)")
            return
        }
        jsonCount += 1
        // Sample the firehose, but ALWAYS log a kind the first time it appears. `nowPlaying` is ~93%
        // of the volume and the only reason sampling exists; sampling everything else is what made
        // absence uninformative when Call History and Apps were being investigated — the records fell
        // in the blind spot between #3 and #50 and neither their presence nor absence could be shown.
        let firstOfKind = seenKinds.insert(kind).inserted
        if firstOfKind || jsonCount <= 3 || jsonCount % 50 == 0 {
            // `privacy: .public` on EVERY interpolation. os_log's `.auto` is public for scalars but
            // PRIVATE for strings, so an unannotated `String` renders as literal "<private>" — which
            // silently redacted this marker, and stamped "<private>" onto every sampled line too.
            let suffix = firstOfKind ? " (first)" : ""
            log.info(
                "iAP2 metadata #\(self.jsonCount, privacy: .public): \(kind, privacy: .public)\(suffix, privacy: .public)"
            )
        }
        // Every structured record is ALSO kept raw in its category's event list. A pane that maps
        // no fields then shows the JSON instead of an empty box — the 2026-07-25 session had
        // CommunicationsUpdate/PowerUpdate/DeviceInformationUpdate all arriving at the box and three
        // panes reading empty, with nothing anywhere to say which side had dropped them.
        func str(_ k: String) -> String? { obj[k] as? String }
        func int(_ k: String) -> Int? { (obj[k] as? NSNumber)?.intValue }

        switch kind {
        case "nowPlaying":
            // DELTA merge — an elapsed-only frame must not blank the title
            // (docs/carplay/05_METADATA_AND_CONTROLS.md).
            if let v = str("title") { media.title = v }
            if let v = str("artist") { media.artist = v }
            if let v = str("album") { media.album = v }
            if let v = str("genre") { media.genre = v }
            if let v = str("composer") { media.composer = v }
            if let v = str("appName") { media.appName = v }
            if let v = int("durationMs") { media.durationMs = v }
            if let v = int("elapsedMs") { media.elapsedMs = v }
            if let v = int("trackNumber") { media.trackNumber = v }
            if let v = int("trackCount") { media.trackCount = v }
            if let v = int("queueIndex") { media.queueIndex = v }
            if let v = int("queueCount") { media.queueCount = v }
            if let v = int("shuffleMode") { media.shuffleMode = v }
            if let v = int("repeatMode") { media.repeatMode = v }
            if let v = int("playbackStatus") { media.playbackStatus = v }
            if let v = int("artworkId") {
                media.artworkId = v
                if let cached = artworkCache[v & 0xFF] { media.artwork = cached }
            }
            if media.isStopped { media = MediaSnapshot() }   // CLEAR ON MEDIA END

        case "artworkReady":
            if let v = int("artworkId"), let img = artworkCache[v & 0xFF] { media.artworkId = v; media.artwork = img }

        case "routeGuidance":
            if let v = str("destinationName") { nav.destinationName = v }
            if let v = str("currentRoadName") { nav.currentRoadName = v }
            if let v = int("timeRemainingSec") { nav.timeRemainingSec = v }
            if let v = int("distanceRemainingM") { nav.distanceRemainingM = v }
            if let v = str("distanceRemainingText") { nav.distanceRemainingText = v }
            if let v = int("distanceRemainingUnits") { nav.distanceRemainingUnits = v }
            if let v = str("distanceToManeuverText") { nav.distanceToManeuverText = v }
            if let v = int("distanceToManeuverUnits") { nav.distanceToManeuverUnits = v }
            if let v = int("currentManeuverIndex") { nav.currentManeuverIndex = v }
            if let v = int("routeGuidanceState") { nav.routeGuidanceState = v }
            if nav.isEnded { nav = NavSnapshot() }           // CLEAR ON NAV END

        case "maneuver":
            // One entry of the route's maneuver list — REPLACE that index (never merge across
            // maneuvers, or a stale "then onto X" from an earlier turn survives into the next).
            let idx = int("index") ?? 0
            nav.maneuvers[idx] = Maneuver(
                description: str("description"),
                afterRoadName: str("afterManeuverRoadName"),
                distanceText: str("distanceText"),
                distanceUnits: int("distanceUnits"),
                exitInfo: str("exitInfo"))
            // Bound the list (a long route streams many maneuvers).
            if nav.maneuvers.count > 64, let oldest = nav.maneuvers.keys.min() {
                nav.maneuvers.removeValue(forKey: oldest)
            }

        case "callState":
            let status = int("status") ?? 0
            let name = str("displayName") ?? str("remoteId") ?? "Unknown"
            if status == 0 {
                if var c = activeCall {                       // CLEAR ON CALL END → history
                    c.ended = Date(); c.status = "Ended"
                    callHistory.insert(c, at: 0)
                    // Same cap as `recentCall` below (was 100 vs 200, trimming opposite ends —
                    // `HistoryPane` sorts by `started` descending, so keep one shared bound here).
                    if callHistory.count > Self.callHistoryCap { callHistory.removeLast() }
                    activeCall = nil
                }
            } else {
                // iAP2 CallStatus enum (audit #8): 1 Sending, 2 Ringing, 3 Connecting, 4 Active (answered
                // + talking), 5 Held, 6 Disconnecting. The old 5-entry table clamped at 4 mislabelled an
                // ACTIVE call as "On hold" and marked wasAnswered on Connecting (3) instead of Active (4).
                let label: String
                switch status {
                case 1: label = "Dialing"     // Sending
                case 2: label = "Ringing"
                case 3: label = "Connecting"
                case 4: label = "Connected"   // Active — answered, in conversation
                case 5: label = "On hold"     // Held
                case 6: label = "Ending"      // Disconnecting
                default: label = "Unknown"
                }
                let answered = status == 4    // Active is the answered/talking state
                if var c = activeCall {
                    c.displayName = name; c.status = label
                    if answered { c.wasAnswered = true }
                    activeCall = c
                } else {
                    activeCall = CallRecord(
                        started: Date(), ended: nil, displayName: name,
                        remoteId: str("remoteId") ?? "",
                        direction: (int("direction") == 2) ? "Outgoing" : "Incoming",
                        status: label, wasAnswered: answered)
                }
            }

        case "communications":
            if let v = str("carrierName") { telephony["Carrier"] = v }
            if let v = int("signalStrength") { telephony["Signal"] = "\(v)" }
            if let v = int("registrationStatus") { telephony["Registration"] = "\(v)" }
            if let v = int("currentCallCount") { telephony["Active calls"] = "\(v)" }
            if let v = int("newVoicemailCount") { telephony["New voicemail"] = "\(v)" }
            if let v = int("airplaneMode") { telephony["Airplane mode"] = v != 0 ? "on" : "off" }
            if let v = int("muteStatus") { telephony["Muted"] = v != 0 ? "yes" : "no" }
            if let v = int("telephonyEnabled") { telephony["Telephony"] = v != 0 ? "enabled" : "disabled" }
            append(.telephony, MetaEvent(date: Date(), title: "communications", body: pretty(obj)))

        case "callListStatus":
            // The phone's own view of the two lists. Without this the pane cannot distinguish an
            // empty Recents list from one the phone declined to share.
            if let v = int("recentsAvailable") { callLists["Recents"] = v != 0 ? "available" : "unavailable" }
            if let v = int("recentsCount") { callLists["Recents entries"] = "\(v)" }
            if let v = int("favoritesAvailable") { callLists["Favorites"] = v != 0 ? "available" : "unavailable" }
            if let v = int("favoritesCount") { callLists["Favorites entries"] = "\(v)" }
            append(.favorites, MetaEvent(date: Date(), title: "callListStatus", body: pretty(obj)))

        case "favorite":
            // 0x4171 FavoritesList. Same per-entry shape as `recentCall` minus type/timestamp, so it
            // is a contact rather than a call — kept in the Phone-adjacent Call History pane's
            // sidebar counts would be misleading, so it lands in its own list.
            if let name = str("displayName") ?? str("remoteId") {
                favorites[name] = str("remoteId") ?? ""
            }
            append(.favorites, MetaEvent(date: Date(), title: "favorite", body: pretty(obj)))

        case "recentCall":
            // 0x4171 is a full-list SNAPSHOT, not a delta — every ListUpdate re-sends the whole
            // Recents list. Appending blindly duplicates every row on the second update and grows
            // without bound across sessions (`resetSession` deliberately preserves callHistory).
            // This was latent only because the parser emitted zero records until 2026-07-29.
            // Dedup on (remoteId, start) — the phone's own identity for an entry — and cap.
            let incomingId = str("remoteId") ?? ""
            let incomingStart = Date(timeIntervalSince1970: TimeInterval(int("unixTimestamp") ?? 0))
            if callHistory.contains(where: { $0.remoteId == incomingId && $0.started == incomingStart }) {
                break
            }
            callHistory.append(CallRecord(
                started: incomingStart,
                // durationSec (metadata.rs list_update id 8) was ignored, so Recents always showed
                // "—" in the Duration column even though the box sends it.
                ended: int("durationSec").map { incomingStart.addingTimeInterval(TimeInterval($0)) },
                displayName: str("displayName") ?? str("remoteId") ?? "Unknown",
                remoteId: str("remoteId") ?? "",
                // Apple's 0x4171 `Type` enum is 0 Unknown, 1 Incoming, 2 Outgoing, 3 Missed, and the
                // box forwards it raw. The previous table was indexed from 0 and clamped at 2, so
                // every entry was mislabelled by one and Missed collapsed onto Outgoing.
                direction: ["Unknown", "Incoming", "Outgoing", "Missed"][max(0, min(int("callType") ?? 0, 3))],
                status: "From iPhone Recents",
                wasAnswered: (int("callType") ?? 0) != 3))
            if callHistory.count > Self.callHistoryCap { callHistory.removeFirst(callHistory.count - Self.callHistoryCap) }

        // ---- Extended metadata (docs/carplay/05_METADATA_AND_CONTROLS.md). Flat key/value panes; the raw record also lands in the
        // category's event list so nothing is lost if a field is not mapped here.

        case "power":
            let pct = int("batteryChargeLevel")
            if let v = pct { power["Battery"] = "\(v)%" }
            if let v = int("batteryChargingState") {
                power["Charging"] = ["disabled", "charging", "charged"][max(0, min(v, 2))]
            }
            if let v = int("externalChargerConnected") { power["External charger"] = v != 0 ? "yes" : "no" }
            if let v = int("accessoryPowerMode") { power["Accessory power mode"] = "\(v)" }
            if let v = int("maxCurrentFromAccessoryMa") { power["Draw from accessory"] = "\(v) mA" }
            if let v = int("maxCurrentFromDeviceMa") { power["Draw from device"] = "\(v) mA" }
            append(.power, MetaEvent(date: Date(), title: "PowerUpdate", body: pretty(obj)))

        case "appList":
            if let v = int("carPlayListCount") { apps["CarPlay apps"] = "\(v)" }
            if let v = int("carPlayListAvailable") {
                apps["App list"] = ["unknown", "declined", "accepted"][max(0, min(v, 2))]
            }
            if let v = int("eaListCount") { apps["External-accessory apps"] = "\(v)" }
            append(.apps, MetaEvent(date: Date(), title: "appList", body: pretty(obj)))

        case "app":
            if let bundle = str("bundleId") {
                apps[str("displayName") ?? bundle] = bundle
            }
            append(.apps, MetaEvent(date: Date(), title: "app", body: pretty(obj)))

        case "appIcon":
            // The PNG itself arrives on an iAP2 File Transfer session; the box does not yet
            // demultiplex those from album artwork, so record the id and show the record.
            if let bundle = str("bundleId"), let xfer = int("iconTransferId") {
                apps["icon: \(bundle)"] = "transfer \(xfer)"
            }
            append(.apps, MetaEvent(date: Date(), title: "appIcon", body: pretty(obj)))

        case "device":
            if let v = str("deviceName") { device["Name"] = v }
            if let v = str("language") { device["Language"] = v }
            if let v = str("uuid") { device["UUID"] = v }
            if let v = int("unixTimestamp") {
                device["Device time"] = ISO8601DateFormatter().string(
                    from: Date(timeIntervalSince1970: TimeInterval(v)))
            }
            if let v = int("timeZoneOffsetMin") { device["UTC offset"] = "\(v) min" }
            if let v = int("wirelessCarPlayAvailable") {
                device["Wireless CarPlay"] = v != 0 ? "available" : "unavailable"
            }
            append(.device, MetaEvent(date: Date(), title: "device", body: pretty(obj)))

        case "bluetoothConnection":
            // 0x4E04 is not currently declared receivable in any tier, so this cannot fire yet — the
            // parser and pane are ready for when `features.rs` declares it.
            if let v = str("profiles") { device["Bluetooth profiles"] = v.isEmpty ? "none" : v }
            append(.device, MetaEvent(date: Date(), title: "bluetoothConnection", body: pretty(obj)))

        case "destination":
            if let v = str("displayName") { nav.destinationName = v }
            if let lat = str("latitude"), let lon = str("longitude") {
                nav.destinationCoordinate = "\(lat), \(lon)"
            }
            if let v = str("address") { nav.destinationAddress = v }
            if let v = str("sourceName") { nav.destinationSource = v }
            append(.navigation, MetaEvent(date: Date(), title: "destination", body: pretty(obj)))

        case "laneGuidance":
            // One record per lane, every record of a message sharing that message's guidanceIndex.
            // A CHANGED guidanceIndex means we have moved to the next junction, so drop the previous
            // lane set first — without this the pane accumulates every lane of the whole route.
            // (If the box omitted guidanceIndex we cannot scope the set, so we merge and accept it.)
            if let g = int("guidanceIndex"), g != nav.lanesGuidanceIndex {
                nav.lanesGuidanceIndex = g
                nav.lanes.removeAll()
            }
            // laneStatus: 0 do not take, 1 usable, 2 preferred.
            if let idx = int("index"), let status = int("laneStatus") {
                nav.lanes[idx] = Lane(
                    status: ["avoid", "ok", "preferred"][max(0, min(status, 2))],
                    angle: int("laneAngle"),
                    highlightAngle: int("laneAngleHighlight"))
            }
            append(.navigation, MetaEvent(date: Date(), title: "laneGuidance", body: pretty(obj)))

        case "voiceOver", "voiceOverCursor", "assistiveTouch":
            append(.accessibility, MetaEvent(date: Date(), title: kind, body: pretty(obj)))

        case "mediaLibrary":
            if let v = str("revision") { media.libraryRevision = v }
            append(.media, MetaEvent(date: Date(), title: "mediaLibrary", body: pretty(obj)))

        default:
            append(.other, MetaEvent(date: Date(), title: kind, body: pretty(obj)))
        }
    }

    private func handleCommandPlist(_ plist: Data) {
        guard let obj = try? PropertyListSerialization.propertyList(from: plist, format: nil),
              let dict = obj as? [String: Any] else { return }
        let type = dict["type"] as? String ?? "?"
        append(Self.category(forCommand: type), MetaEvent(date: Date(), title: type, body: pretty(dict)))
    }

    private func append(_ cat: MetaCategory, _ ev: MetaEvent) {
        events[cat, default: []].append(ev)
        if let n = events[cat]?.count, n > 200 { events[cat]?.removeFirst(n - 200) }
    }

    static func category(forCommand t: String) -> MetaCategory {
        switch t {
        case "modesChanged", "deviceOfferFocus", "requestViewArea": return .modesFocus
        case "duckAudio", "unduckAudio", "flushAudio", "setVolume": return .audioControl
        case "showUI", "stopUI", "changeUIContext", "setNightMode", "uiAppearanceUpdate",
             "mapAppearanceUpdate", "setLimitedUI", "performHapticFeedback": return .uiAppearance
        case "setEnhancedSiriParams": return .siriSpeech
        case "startSession", "stopSession", "tearDownStreams", "sessionDied": return .sessionLifecycle
        case "updateVehicleInformation", "disableBluetooth", "setOEMLogConfiguration",
             "handleLogArchiveRequest": return .vehicle
        default: return .other
        }
    }

    private func pretty(_ v: Any, indent: String = "") -> String {
        switch v {
        case let d as [String: Any]:
            return d.keys.sorted().map { "\(indent)\($0): \(pretty(d[$0]!, indent: indent + "  "))" }
                .joined(separator: "\n")
        case let a as [Any]: return "[" + a.map { pretty($0, indent: indent) }.joined(separator: ", ") + "]"
        case let data as Data: return "<\(data.count) B>"
        default: return "\(v)"
        }
    }
}

// MARK: - SwiftUI

private func hms(_ seconds: Int) -> String {
    let h = seconds / 3600, m = (seconds % 3600) / 60, s = seconds % 60
    return h > 0 ? String(format: "%d:%02d:%02d", h, m, s) : String(format: "%d:%02d", m, s)
}

/// A titled card — the one visual primitive the detail panes are built from.
private struct Card<Content: View>: View {
    let title: String
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title.uppercased())
                .font(.caption).fontWeight(.semibold)
                .foregroundStyle(.secondary)
            content
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.35), in: RoundedRectangle(cornerRadius: 12))
    }
}

private struct Row: View {
    let label: String
    let value: String?
    var body: some View {
        if let value, !value.isEmpty {
            LabeledContent(label) { Text(value).fontWeight(.medium) }
        }
    }
}

/// Shown when a category has no data — states plainly why (never a blank pane).
private struct EmptyPane: View {
    let category: MetaCategory
    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: category.symbol)
                .font(.system(size: 40)).foregroundStyle(.tertiary)
            Text("No \(category.rawValue) metadata").font(.headline)
            Text(category.absenceNote)
                .font(.callout).foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 460)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
    }
}

private struct MediaPane: View {
    let m: MediaSnapshot
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Card(title: "Now Playing") {
                    HStack(alignment: .top, spacing: 16) {
                        Group {
                            if let art = m.artwork {
                                Image(nsImage: art).resizable().aspectRatio(contentMode: .fit)
                            } else {
                                RoundedRectangle(cornerRadius: 8).fill(.quaternary)
                                    .overlay { Image(systemName: "music.note").font(.largeTitle).foregroundStyle(.tertiary) }
                            }
                        }
                        .frame(width: 132, height: 132)
                        .clipShape(RoundedRectangle(cornerRadius: 8))

                        VStack(alignment: .leading, spacing: 6) {
                            Text(m.title ?? (m.awaitingTrackInfo ? "Playing…" : "—"))
                                .font(.title2).fontWeight(.semibold).lineLimit(2)
                            Text(m.artist ?? "—").font(.title3).foregroundStyle(.secondary).lineLimit(1)
                            if let album = m.album { Text(album).font(.callout).foregroundStyle(.tertiary).lineLimit(1) }
                            if let state = m.stateText {
                                Label(state, systemImage: state == "Playing" ? "play.fill" : "pause.fill")
                                    .font(.caption).padding(.top, 2)
                            }
                            if let p = m.progress, let e = m.elapsedMs, let d = m.durationMs {
                                VStack(alignment: .leading, spacing: 3) {
                                    ProgressView(value: p)
                                    HStack {
                                        Text(hms(e / 1000)); Spacer(); Text(hms(d / 1000))
                                    }.font(.caption2).foregroundStyle(.secondary)
                                }.padding(.top, 4)
                            }
                        }
                        Spacer()
                    }
                }

                Card(title: "Details") {
                    VStack(spacing: 6) {
                        Row(label: "App", value: m.appName)
                        Row(label: "Genre", value: m.genre)
                        Row(label: "Composer", value: m.composer)
                        if let t = m.trackNumber, let c = m.trackCount { Row(label: "Track", value: "\(t) of \(c)") }
                        if let i = m.queueIndex, let c = m.queueCount { Row(label: "Queue", value: "\(i + 1) of \(c)") }
                        if let s = m.shuffleMode { Row(label: "Shuffle", value: s == 0 ? "Off" : "On") }
                        if let r = m.repeatMode { Row(label: "Repeat", value: r == 0 ? "Off" : "On") }
                        if let a = m.artworkId {
                            Row(label: "Artwork", value: m.artwork != nil ? "id \(a) — received" : "id \(a) — transferring…")
                        }
                    }
                }

                if m.awaitingTrackInfo {
                    Card(title: "Waiting for track info") {
                        Text("Playback is live (elapsed time is streaming), but iOS pushes the full track attributes — title, artist, album, artwork — on a TRACK CHANGE. Skip to the next track to populate them.")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                }

                if m.artworkId == nil && !m.awaitingTrackInfo {
                    Card(title: "Album Art") {
                        Text("iOS is not offering artwork for this track. Album art requires the accessory to declare a supported-artwork-format list in the iAP2 identify — the one unresolved gate (docs/carplay/05_METADATA_AND_CONTROLS.md). The File-Transfer receiver is implemented and will display art the moment iOS offers it.")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                }
            }
            .padding(20)
        }
    }
}

private struct NavPane: View {
    let n: NavSnapshot

    /// Deliberately status-only glyphs. `laneAngle` would let us draw the actual arrow direction,
    /// but its units are unverified, so a rotated arrow would be a guess presented as a fact.
    private static func laneGlyph(_ status: String?) -> String {
        switch status {
        case "preferred": return "\u{25CF}"   // filled — take this lane
        case "ok":        return "\u{25CB}"   // hollow — usable
        default:          return "\u{2715}"   // cross  — do not take
        }
    }

    private static func laneTint(_ status: String?) -> Color {
        switch status {
        case "preferred": return .green
        case "ok":        return .primary
        default:          return .secondary
        }
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Card(title: "Next Maneuver") {
                    VStack(alignment: .leading, spacing: 6) {
                        let m = n.currentManeuver
                        HStack(alignment: .center, spacing: 12) {
                            if let img = n.maneuverImage {
                                Image(nsImage: img).resizable().aspectRatio(contentMode: .fit)
                                    .frame(width: 56, height: 56)
                                    .accessibilityLabel(m?.description ?? "Maneuver")
                            }
                            else if let sym = n.maneuverSymbol {
                                Image(systemName: sym).font(.system(size: 40, weight: .semibold))
                                    .frame(width: 56, height: 56)
                                    .accessibilityLabel(m?.description ?? "Maneuver")
                            }
                            Text(m?.description ?? "—").font(.title2).fontWeight(.semibold)
                        }
                        if let d = n.distanceToTurn {
                            Text("in \(d)").font(.title3).foregroundStyle(.secondary)
                        }
                        if let exit = m?.exitInfo, !exit.isEmpty {
                            Label(exit, systemImage: "arrow.triangle.branch")
                                .font(.callout).foregroundStyle(.secondary)
                        }
                        if let after = m?.afterRoadName, !after.isEmpty {
                            Label("then onto \(after)", systemImage: "arrow.turn.up.right")
                                .font(.callout).foregroundStyle(.tertiary)
                        }
                    }
                }
                // 0x5204 arrives on hardware TODAY (16 messages in one live session, bodies 75-138 B)
                // and was parsed into `nav.lanes` for months while NOTHING read the property, so the
                // data was received and silently dropped one step short of the screen.
                if !n.lanes.isEmpty {
                    Card(title: "Lanes") {
                        VStack(alignment: .leading, spacing: 8) {
                            HStack(spacing: 12) {
                                ForEach(n.lanes.keys.sorted(), id: \.self) { i in
                                    VStack(spacing: 3) {
                                        Text(Self.laneGlyph(n.lanes[i]?.status))
                                            .font(.title2)
                                            .foregroundStyle(Self.laneTint(n.lanes[i]?.status))
                                        Text("\(i)")
                                            .font(.caption2)
                                            .foregroundStyle(.tertiary)
                                    }
                                    .frame(minWidth: 24)
                                }
                            }
                            if let g = n.lanesGuidanceIndex {
                                Text("for maneuver #\(g)")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
                Card(title: "Route") {
                    VStack(spacing: 6) {
                        Row(label: "Destination", value: n.destinationName)
                        Row(label: "Current road", value: n.currentRoadName)
                        Row(label: "Distance left", value: n.distanceLeft)
                        if let t = n.timeRemainingSec { Row(label: "Time left", value: hms(t)) }
                        if let t = n.etaText {
                            Row(label: "Arrival", value: t)
                        } else if let e = n.eta {
                            Row(label: "Arrival", value: e.formatted(date: .omitted, time: .shortened))
                        }
                        if let i = n.currentManeuverIndex {
                            Row(label: "Maneuver", value: "#\(i) of \(n.maneuvers.count) received")
                        }
                    }
                }
            }
            .padding(20)
        }
    }
}

private struct CallPane: View {
    let c: CallRecord
    var body: some View {
        ScrollView {
            Card(title: "Active Call") {
                HStack(spacing: 16) {
                    Image(systemName: "phone.fill.arrow.down.left")
                        .font(.system(size: 34)).foregroundStyle(.green)
                    VStack(alignment: .leading, spacing: 6) {
                        Text(c.displayName).font(.title2).fontWeight(.semibold)
                        if !c.remoteId.isEmpty { Text(c.remoteId).font(.callout).foregroundStyle(.secondary) }
                        HStack(spacing: 8) {
                            Text(c.direction).font(.caption).padding(.horizontal, 8).padding(.vertical, 3)
                                .background(.quaternary, in: Capsule())
                            Text(c.status).font(.caption).padding(.horizontal, 8).padding(.vertical, 3)
                                .background(.quaternary, in: Capsule())
                        }
                        Text("Started \(c.started.formatted(date: .omitted, time: .standard))")
                            .font(.caption).foregroundStyle(.tertiary)
                    }
                    Spacer()
                }
            }
            .padding(20)
        }
    }
}

private struct HistoryPane: View {
    let calls: [CallRecord]
    var body: some View {
        // Live-call ends are inserted newest-first; Recents are appended in phone order — sort here
        // so the table is always in time order regardless of which feed a row came from (L4).
        Table(calls.sorted { $0.started > $1.started }) {
            TableColumn("When") { c in
                Text(c.started.formatted(date: .abbreviated, time: .shortened))
            }
            TableColumn("Direction") { c in
                Label(c.direction, systemImage: c.direction == "Outgoing"
                      ? "phone.arrow.up.right" : (c.wasAnswered ? "phone.arrow.down.left" : "phone.down"))
            }
            TableColumn("Name") { c in Text(c.displayName) }
            TableColumn("Number") { c in Text(c.remoteId).foregroundStyle(.secondary) }
            TableColumn("Duration") { c in
                Text(c.ended.map { hms(Int($0.timeIntervalSince(c.started))) } ?? "—")
            }
        }
    }
}

private struct EventsPane: View {
    let events: [MetaEvent]
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 10) {
                ForEach(events.reversed()) { ev in
                    Card(title: "\(ev.date.formatted(date: .omitted, time: .standard))  ·  \(ev.title)") {
                        Text(ev.body)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                    }
                }
            }
            .padding(20)
        }
    }
}

struct MetadataRootView: View {
    @ObservedObject var store = MetadataStore.shared
    @State private var selection: MetaCategory? = .media

    var body: some View {
        NavigationSplitView {
            List(MetaCategory.allCases, selection: $selection) { cat in
                let n = store.count(for: cat)
                NavigationLink(value: cat) {
                    Label(cat.rawValue, systemImage: cat.symbol)
                        .badge(n > 0 ? n : 0)
                        .foregroundStyle(n > 0 ? .primary : .secondary)
                }
            }
            .navigationSplitViewColumnWidth(min: 200, ideal: 220)
        } detail: {
            Group {
                switch selection ?? .media {
                case .media:
                    store.media.isEmpty ? AnyView(EmptyPane(category: .media)) : AnyView(MediaPane(m: store.media))
                case .navigation:
                    store.nav.isEmpty ? AnyView(EmptyPane(category: .navigation)) : AnyView(NavPane(n: store.nav))
                case .phone:
                    store.activeCall.map { AnyView(CallPane(c: $0)) } ?? AnyView(EmptyPane(category: .phone))
                case .callHistory:
                    // The phone's own list status is shown ALONGSIDE the entries, and above the
                    // absence note when there are none — "Recents: available, 0 entries" is the
                    // difference between a shared-but-empty list and a declined one, and that is
                    // exactly what a reader of an empty Call History pane needs to know.
                    ScrollView {
                        VStack(spacing: 16) {
                            if !store.callLists.isEmpty {
                                Card(title: "List status (from iPhone)") {
                                    VStack(spacing: 6) {
                                        ForEach(store.callLists.keys.sorted(), id: \.self) { k in
                                            Row(label: k, value: store.callLists[k])
                                        }
                                    }
                                }
                            }
                            if store.callHistory.isEmpty {
                                EmptyPane(category: .callHistory)
                            } else {
                                HistoryPane(calls: store.callHistory)
                            }
                        }.padding(20)
                    }
                case .telephony:
                    KeyValuePane(category: .telephony, title: "Telephony Status", pairs: store.telephony,
                                 raw: store.events[.telephony] ?? [])
                case .power:
                    KeyValuePane(category: .power, title: "Power", pairs: store.power,
                                 raw: store.events[.power] ?? [])
                case .device:
                    KeyValuePane(category: .device, title: "Device", pairs: store.device,
                                 raw: store.events[.device] ?? [])
                case .apps:
                    KeyValuePane(category: .apps, title: "Apps", pairs: store.apps,
                                 raw: store.events[.apps] ?? [])
                case .favorites:
                    ScrollView {
                        VStack(spacing: 16) {
                            if !store.callLists.isEmpty {
                                Card(title: "List status (from iPhone)") {
                                    VStack(spacing: 6) {
                                        ForEach(store.callLists.keys.sorted(), id: \.self) { k in
                                            Row(label: k, value: store.callLists[k])
                                        }
                                    }
                                }
                            }
                            KeyValuePane(category: .favorites, title: "Favorites",
                                         pairs: store.favorites,
                                         raw: store.events[.favorites] ?? [])
                        }.padding(20)
                    }
                default:
                    let evs = store.events[selection ?? .other] ?? []
                    evs.isEmpty ? AnyView(EmptyPane(category: selection ?? .other)) : AnyView(EventsPane(events: evs))
                }
            }
            .navigationTitle(selection?.rawValue ?? "Metadata")
        }
    }
}

/// A sorted key/value card, or the category's absence note when nothing has arrived. Used by every
/// pane whose message is a flat set of scalars (telephony, power, device, apps).
private struct KeyValuePane: View {
    let category: MetaCategory
    let title: String
    let pairs: [String: String]
    var raw: [MetaEvent] = []

    var body: some View {
        if pairs.isEmpty && !raw.isEmpty {
            // Records arrived but no field mapped — show them rather than an empty pane.
            EventsPane(events: raw)
        } else if pairs.isEmpty {
            EmptyPane(category: category)
        } else {
            ScrollView {
                Card(title: title) {
                    VStack(spacing: 6) {
                        ForEach(pairs.keys.sorted(), id: \.self) { k in
                            Row(label: k, value: pairs[k])
                        }
                    }
                }.padding(20)
            }
        }
    }
}

// MARK: - AppKit host

final class MetadataWindowController: NSWindowController {
    static let shared = MetadataWindowController()

    private convenience init() {
        let win = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 900, height: 580),
            styleMask: [.titled, .closable, .resizable, .miniaturizable, .fullSizeContentView],
            backing: .buffered, defer: false)
        win.title = "Metadata"
        win.isReleasedWhenClosed = false
        win.contentView = NSHostingView(rootView: MetadataRootView())
        self.init(window: win)
    }

    func show() {
        window?.center()
        window?.makeKeyAndOrderFront(nil)
    }
}
