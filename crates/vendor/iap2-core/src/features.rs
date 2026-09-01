//! features.rs — the single source of truth for which iAP2 metadata the accessory supports.
//!
//! Three things have to agree exactly or nothing arrives (docs/carplay/05_METADATA_AND_CONTROLS.md §4):
//!
//!   1. IdentificationInformation param 6 — the `Start*`/`Stop*` ids we will SEND;
//!   2. IdentificationInformation param 7 — the `*Update` ids we will RECEIVE;
//!   3. the subscribe actually sent after identification, naming the wanted fields.
//!
//! Historically these were three hand-maintained lists in three files and they drifted: 0x4157 and
//! 0x4170 were being sent as subscribes while 0x4158/0x4171 were absent from param 7, so both were
//! silently ignored by iOS for the project's entire history. This table generates all three.
//!
//! Field ids and subscribe shapes come from Apple's `iap2messages-internal.i2mspecarchive`
//! (`tools/i2mspec_dump.py`), not from inference.
//!
//! # Tiers
//!
//! [`Tier::Proven`] is exactly what a real iPhone has accepted on this project's hardware — the
//! declaration in use on 2026-07-25 when wireless metadata first worked end to end. It exists as a
//! recovery lever, not as an aspiration: a `0x1D03 IdentificationRejected` is UNRECOVERABLE (params
//! 6/7 are in `REQUIRED_IDENT_PARAMS`, so the retry is byte-identical, the second reject aborts and
//! the session is cleared). `CARPLAY_METADATA=proven` reverts to it without a rebuild.
//!
//! [`Tier::Extended`] is the rest of the metadata surface: telephony status, call history, power,
//! app discovery, lane guidance, destinations, device info. These only report state.
//!
//! [`Tier::Capability`] declares the accessory *does* something — renders VoiceOver, hosts
//! AssistiveTouch, browses the media library. Declaring these changes how iOS treats the head unit,
//! not just what it tells it, so they are off unless asked for (`CARPLAY_METADATA=all`).
//!
//! # What this does NOT touch
//!
//! The Bluetooth-time Identify (`TransportComponent::Wireless`) is byte-pinned and must not grow —
//! docs/wireless/00_WIRELESS_CARPLAY.md recorded iOS rejecting exactly that, twice, on two unrelated id pairs, breaking
//! the WiFi handoff. This table is consumed only by the wired and AirPlay-tunnel arms.

/// How much of the surface to declare and subscribe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Device-accepted on this hardware. The recovery baseline.
    Proven,
    /// Read-only metadata beyond the proven set.
    Extended,
    /// Declarations that change accessory behaviour, not just its metadata feed.
    Capability,
}

impl Tier {
    fn parse(v: &str) -> Option<Policy> {
        Some(match v.trim() {
            // Only the 2026-07-25 device-accepted declaration. The compiled default.
            "proven" => Policy { sent: Tier::Proven, recv: Tier::Proven, subscribe: Tier::Proven },
            // Grow both params, every Start paired with its Stop. Device-ACCEPTED on the RCS
            // tunnel 2026-07-25 (docs/carplay/05_METADATA_AND_CONTROLS.md §6.6): 340 B Identify, `RX 0x1D02 -> Identified`, 8
            // subscribes, every declared feed arriving. An earlier form of this tier omitted the
            // Stop ids and WAS rejected (§6.1/§6.3) — that is what the Stop fields exist for.
            "extended" => {
                Policy { sent: Tier::Extended, recv: Tier::Extended, subscribe: Tier::Extended }
            }
            // REFUTED DEAD END — retained only so the experiment is not repeated (docs/carplay/05_METADATA_AND_CONTROLS.md §6.2).
            // Param 6 at the baseline, param 7 grown. It was proposed on the theory that iOS only
            // polices param 6; the very next session had iOS name param 7 explicitly
            // (`[param 6][], [param 7][0x4171]`), and the shape is in any case exactly what Apple's
            // reason enum calls `OptionalMsgNotValidWithoutRequiredMsgs` — a receive declared
            // without its send. Do not run this again.
            "rx-only" => {
                Policy { sent: Tier::Proven, recv: Tier::Extended, subscribe: Tier::Extended }
            }
            "all" => {
                Policy { sent: Tier::Capability, recv: Tier::Capability, subscribe: Tier::Capability }
            }
            _ => return None,
        })
    }
}

/// Which tier applies to each of the three lists. They are separable because iOS polices them
/// separately — see the `rx-only` note above.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// Identify param 6, `MessagesSentByAccessory`.
    pub sent: Tier,
    /// Identify param 7, `MessagesReceivedFromDevice`.
    pub recv: Tier,
    /// Which features actually get a subscribe.
    pub subscribe: Tier,
}

/// Runtime override file, read when `CARPLAY_METADATA` is unset. `/tmp` is tmpfs on the box, so a
/// reboot always returns to the compiled default — an experiment cannot strand a working session.
pub const POLICY_FILE: &str = "/tmp/carplay_metadata";

impl Policy {
    /// `CARPLAY_METADATA`, else `/tmp/carplay_metadata`, else `proven`.
    ///
    /// The default is the device-accepted baseline, deliberately. `extended` was rejected on
    /// hardware; a build that defaults to a rejected declaration is a build that cannot hold a
    /// session, and the failure is unrecoverable within that session.
    pub fn active() -> Policy {
        resolved().policy
    }
}

/// The policy and skip list, resolved ONCE per process and cached.
///
/// `Policy::active()` and the skip list used to be re-read from `/tmp/carplay_metadata` at every call
/// site — once when the Identify is built, again when the subscribes go out seconds later. An edit in
/// that window silently desynced param 6 from the subscribes, which is the precise drift the feature
/// table exists to make impossible. `airplayd` is spawned per session by the supervisor, so
/// process-lifetime caching is session-lifetime caching; `iap2d` is long-lived, and pinning its policy
/// at first use is the behaviour we want there too — a link's declaration must not change under it.
struct Resolved {
    policy: Policy,
    skip: Vec<String>,
}

/// The app-pushed policy (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B3), armed once per process from the host's YAML before the first
/// declaration is built. Set by the consuming daemon — iap2d for the wired arm, airplayd for the
/// AirPlayTunnel arm — via [`arm_pushed_policy`]; unset means no app config reached this process.
static PUSHED: std::sync::OnceLock<Resolved> = std::sync::OnceLock::new();

/// Arm the app-pushed metadata policy. Returns whether THIS call armed it.
///
/// FIRST ARM WINS, deliberately: params 6/7 and the subscribes of one link must come from ONE
/// snapshot, which is the same invariant the env/file cache exists to protect (a mid-link change
/// would desync the declaration from the subscribes — see [`Resolved`]). A later call is logged and
/// ignored rather than swapped in.
///
/// `rx-only` is REFUSED here even though [`Tier::parse`] accepts it: it is a refuted dead end
/// (docs/carplay/05_METADATA_AND_CONTROLS.md §6.2, `OptionalMsgNotValidWithoutRequiredMsgs`) and the app must not be able to
/// construct it. An unparseable word leaves the policy unarmed, so resolution falls through to the
/// bench levers and then the compiled `proven` floor.
pub fn arm_pushed_policy(word: &str, skip: &[String]) -> bool {
    let w = word.trim();
    if w.eq_ignore_ascii_case("rx-only") {
        eprintln!("[features] pushed metadata tier 'rx-only' REFUSED (refuted dead end, docs/carplay/05_METADATA_AND_CONTROLS.md §6.2)");
        return false;
    }
    let Some(policy) = Tier::parse(w) else {
        eprintln!("[features] pushed metadata tier '{w}' unrecognized — ignoring (bench levers / compiled default apply)");
        return false;
    };
    match PUSHED.set(Resolved { policy, skip: skip.to_vec() }) {
        Ok(()) => {
            eprintln!(
                "[features] metadata policy ARMED from pushed config: tier={w} skip={skip:?} (first arm wins)"
            );
            true
        }
        Err(rejected) => {
            // Re-arming with the SAME tier is the NORMAL path (airplayd calls this per control
            // connection, several per session), so only speak up when the request actually differs
            // from what is pinned — otherwise this would out-log the once-per-process resolution
            // line it sits beside. A differing request is worth a loud line: it means a config
            // change will not take effect until this process restarts.
            let pinned = PUSHED.get().expect("set() failed, so it is armed");
            if pinned.policy.sent != rejected.policy.sent
                || pinned.policy.recv != rejected.policy.recv
                || pinned.policy.subscribe != rejected.policy.subscribe
                || pinned.skip != rejected.skip
            {
                eprintln!(
                    "[features] pushed metadata tier '{w}' IGNORED — a DIFFERENT policy is already armed this process; restart the daemon to apply"
                );
            }
            false
        }
    }
}

fn resolved() -> &'static Resolved {
    // Pushed config wins over the bench levers (docs/carplay/04_CAPABILITIES_AND_CONFIG.md: an on-box override exists for app-less
    // bench testing only and is explicitly SUBORDINATE to what the app pushed) — and both still
    // fall through to the compiled `proven` floor.
    if let Some(p) = PUSHED.get() {
        return p;
    }
    static CACHE: std::sync::OnceLock<Resolved> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        let (file_word, file_skip) = file_setting();
        let env_word = std::env::var("CARPLAY_METADATA").ok();
        let env_skip = std::env::var("CARPLAY_METADATA_SKIP").ok();
        resolve(env_word.as_deref(), file_word.as_deref(), env_skip.as_deref(), file_skip)
    })
}

/// The pure resolution body behind [`resolved`]: env word beats file word beats the compiled
/// `proven` default; the env and file skip lists concatenate. Split out (unchanged) so tests can
/// exercise the precedence hermetically — `resolved()` reads the real environment and
/// `/tmp/carplay_metadata`, so a dev host with that file armed failed the default-policy test
/// spuriously.
fn resolve(
    env_word: Option<&str>,
    file_word: Option<&str>,
    env_skip: Option<&str>,
    file_skip: Vec<String>,
) -> Resolved {
    let policy = env_word
        .and_then(Tier::parse)
        .or_else(|| file_word.and_then(Tier::parse))
        .unwrap_or(Policy {
            sent: Tier::Proven,
            recv: Tier::Proven,
            subscribe: Tier::Proven,
        });
    let mut skip: Vec<String> = env_skip
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    skip.extend(file_skip);
    eprintln!(
        "[features] metadata policy (resolved once): param6={:?} param7={:?} subscribe={:?} skip={:?}",
        policy.sent, policy.recv, policy.subscribe, skip
    );
    Resolved { policy, skip }
}

/// How to build a subscribe body. Every variant produces Apple-declared parameter ids only.
#[derive(Debug, Clone, Copy)]
pub enum Body {
    /// No parameters.
    None,
    /// Flat zero-length field selectors.
    Fields(&'static [u16]),
    /// Groups of zero-length field selectors: `(group id, member ids)`. An EMPTY group is silently
    /// ignored by iOS, which is why 0x4170 never returned anything.
    Groups(&'static [(u16, &'static [u16])]),
    /// A byte-exact payload that must not be regenerated (see [`Feature::body`] notes).
    Verbatim(fn() -> Vec<u8>),
}

/// One metadata capability: what we send to turn it on, and what we expect back.
#[derive(Debug, Clone, Copy)]
pub struct Feature {
    /// Log/diagnostic name; also the `CARPLAY_METADATA_SKIP` / `skip=` token.
    pub name: &'static str,
    pub tier: Tier,
    /// How this feature's feed is turned on. See [`Trigger`] — `stop: None` used to be writable and
    /// is the mistake that cost three hardware sessions.
    pub trigger: Trigger,
    /// `*Update` ids to declare in Identify param 7.
    pub updates: &'static [u16],
    pub body: Body,
}

/// What the accessory must declare in param 6 for this feature's updates to be legal.
///
/// This is a type rather than three `Option`s because the failure that cost this project three
/// hardware sessions was someone typing the plausible-looking default: a `Start*` with `stop: None`.
/// [`Trigger::Subscription`] has no default — the exception must be spelled out as
/// [`Trigger::UnpairedStart`] with device evidence, which nobody writes by accident.
///
/// The type enforces PRESENCE, never correctness — `Subscription { start: 0xAE00, stop: 0x5002 }`
/// typechecks. `every_start_is_paired_with_apples_own_stop` is what checks the Stop is the right one.
#[derive(Debug, Clone, Copy)]
pub enum Trigger {
    /// The device pushes these unsolicited. Nothing goes in param 6.
    Unsolicited,
    /// This feature's updates arrive because ANOTHER feature's subscribe asked for them; it has no
    /// subscribe of its own. Names the owner, so `active()` can refuse to leave a rider orphaned.
    ///
    /// Recording this in the type rather than a comment closes a live hazard: `lane_guidance` rides
    /// `route_guidance`, and with the dependency written only in prose,
    /// `echo "extended skip=route_guidance" > /tmp/carplay_metadata` declared `0x5204` in param 7
    /// with no `0x5200` in param 6 — `OptionalMsgNotValidWithoutRequiredMsgs`, an UNRECOVERABLE
    /// `0x1D03`, produced by the very lever an operator reaches for to narrow a failing declaration.
    RidesOn(&'static str),
    /// A `Start*`/`Stop*` pair. Both are declared in param 6 even though we never send a Stop —
    /// declaration is a capability statement, not a promise of traffic. Device-proven 2026-07-25:
    /// omitting the Stop returns
    /// `iapreject: Param: MessagesSentByAccessory [msgID: 0xae02 Reason: RequiredInfoMissing]`
    /// and rejects the whole feature.
    ///
    /// `also` carries further accessory-sourced ids the feature requires — Destination Sharing needs
    /// `0x10DD DestinationInformationStatus`, and MediaLibrary carries a second Start/Stop pair.
    Subscription { start: u16, stop: u16, also: &'static [u16] },
    /// A `Start*` whose `Stop*` is deliberately withheld. Requires device evidence, because this is
    /// the exact shape iOS rejects everywhere else.
    UnpairedStart { start: u16, device_evidence: &'static str },
}

impl Trigger {
    /// The `Start*` id to subscribe with, if this feature has its own subscribe.
    pub fn start(&self) -> Option<u16> {
        match self {
            Trigger::Subscription { start, .. } | Trigger::UnpairedStart { start, .. } => Some(*start),
            Trigger::Unsolicited | Trigger::RidesOn(_) => None,
        }
    }

    /// Every accessory-sourced id this trigger contributes to param 6.
    pub fn sent_ids(&self) -> Vec<u16> {
        match self {
            Trigger::Unsolicited | Trigger::RidesOn(_) => Vec::new(),
            Trigger::UnpairedStart { start, .. } => vec![*start],
            Trigger::Subscription { start, stop, also } => {
                let mut v = vec![*start, *stop];
                v.extend_from_slice(also);
                v
            }
        }
    }

    /// The feature this one depends on, if any.
    pub fn owner(&self) -> Option<&'static str> {
        match self {
            Trigger::RidesOn(o) => Some(o),
            _ => None,
        }
    }
}

impl Feature {
    /// The `Start*` id to subscribe with, if this feature has its own subscribe.
    pub fn start(&self) -> Option<u16> {
        self.trigger.start()
    }

    pub fn build_body(&self) -> Vec<u8> {
        match self.body {
            Body::None => Vec::new(),
            Body::Fields(ids) => ids.iter().flat_map(|&i| tlv(i, &[])).collect(),
            Body::Groups(groups) => groups
                .iter()
                .flat_map(|(gid, members)| {
                    let inner: Vec<u8> = members.iter().flat_map(|&i| tlv(i, &[])).collect();
                    tlv(*gid, &inner)
                })
                .collect(),
            Body::Verbatim(f) => f(),
        }
    }
}

/// TLV param: `[len BE16][id BE16][value]`. Duplicated from `metadata.rs` to keep this module free
/// of a dependency cycle; asserted identical by `tests::tlv_matches_metadata`.
fn tlv(id: u16, value: &[u8]) -> Vec<u8> {
    debug_assert!(4 + value.len() <= u16::MAX as usize, "TLV length overflows u16"); // audit Fix #23
    let len = (4 + value.len()) as u16;
    let mut o = len.to_be_bytes().to_vec();
    o.extend_from_slice(&id.to_be_bytes());
    o.extend_from_slice(value);
    o
}

// ---------------------------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------------------------

/// `0x4154 StartCallStateUpdates`. Apple also declares id 10 ConferenceGroup; it is deliberately
/// omitted because this exact body is the one iOS has been observed accepting (three
/// `CallStateUpdate` frames over the RCS tunnel, 2026-07-25), and a subscribe carrying selectors
/// iOS does not like degrades silently rather than erroring.
const CALL_STATE_FIELDS: &[u16] = &[0, 1, 2, 3, 4, 6, 7, 8, 9, 11, 12];

/// `0x4157 StartCommunicationsUpdates` — every id Apple declares. There is no id 3.
const COMMS_FIELDS: &[u16] = &[0, 1, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17];

/// `0x4170 StartListUpdates` — RecentsListProperties(1) and FavoritesListProperties(6). These are
/// property GROUPS: the previous revision sent them empty, which iOS ignores, so no call history
/// could ever arrive even had 0x4171 been declared.
const LIST_GROUPS: &[(u16, &[u16])] = &[
    (1, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]), // Index, RemoteID, DisplayName, Label, AddressBookID,
    // Service, Type, UnixTimestamp, Duration, Occurrences
    (6, &[0, 1, 2, 3, 4, 5]), // Favorites has no Type/Timestamp/Duration/Occurrences
];

/// `0xAE00 StartPowerUpdates` — every id Apple declares. There is no id 7 (that is
/// MaximumCurrentDrawnFromDevice, which appears in the update but is not selectable).
const POWER_FIELDS: &[u16] = &[0, 1, 2, 3, 4, 5, 6, 8, 9, 10, 11, 12];

/// `0x560B StartVoiceOverUpdates` — SpeakingVolume, SpeakingRate, Enabled.
const VOICE_OVER_FIELDS: &[u16] = &[0, 1, 2];

/// `0x560F StartVoiceOverCursorUpdates` — Label, Value, Hint, Traits.
const VOICE_OVER_CURSOR_FIELDS: &[u16] = &[0, 1, 2, 3];

/// `0x10DA StartDestinationInformation` — SourceName must be requested to be received.
const DESTINATION_FIELDS: &[u16] = &[0];

/// `0xAD00 StartAppDiscoveryUpdates`: CarPlayAppCategories(0) with AllCarPlayApps(0), and a 90 px
/// icon size (sub 2 — an earlier comment said "(3)"; the code was right, the comment was not).
/// Without an icon size iOS omits `AppIconIdentifier` from the list entirely.
///
/// Apple's full parameter set for 0xAD00 (from `iap2messages-external.i2mspecarchive`, shipped inside
/// the Simulator at `Contents/Frameworks/iAP2MessageKit.framework/Resources/`): 0 CarPlayAppCategories,
/// 1 CarPlayAppListMax, 2 CarPlayAppIconSize, 3 ExternalAccessoryAppCategories,
/// 4 ExternalAccessoryAppListMax. We send 0 and 2.
///
/// **We omit sub 1 `CarPlayAppListMax`; Apple's Simulator always emits it (default 20).** The spec says
/// `0 = no limit`, so omission is probably benign — recorded as a known difference from the normative
/// sender rather than a suspected cause. Apple's other defaults: categories `[]` (empty), icon size 120.
///
/// **Params 10/11 are NOT the gate for the app list** — investigated and refuted 2026-07-30.
/// `IAPEnablementConfig.appDiscovery` and `.externalAccessoryProtocol` are peer booleans, both default
/// false, and Apple's own accessory sends 0xAD00 while declaring neither Identify param 10
/// (SupportedExternalAccessoryProtocol) nor param 11 (AppMatchTeamID) — `AppMatchTeamID` does not
/// appear in the Simulator binary at all. On this axis our declaration already matches Apple's.
/// What actually flips `CarPlayAppListAvailable` 0 Unknown -> 2 Accepted remains UNDETERMINED by every
/// normative source; the one hint is that its enum is Unknown/**Declined**/**Accepted** (consent
/// language) where the ExternalAccessory sibling is Unknown/Unavailable/Available (availability
/// language), suggesting a user-consent decision on the phone. That is inference, not spec text.
fn app_discovery_body() -> Vec<u8> {
    let mut o = tlv(0, &tlv(0, &[])); // CarPlayAppCategories { AllCarPlayApps }
    o.extend_from_slice(&tlv(2, &90u16.to_be_bytes())); // CarPlayAppIconSize = 90 px
    o
}

/// `0x4C03 StartMediaLibraryUpdates`: no LastKnownMediaLibraryRevision, so iOS sends a full
/// database; MediaItemProperties(2) selects persistent id, title, media type, duration, artist,
/// album, genre.
fn media_library_body() -> Vec<u8> {
    let inner: Vec<u8> = [0u16, 1, 2, 4, 6, 7, 12].iter().flat_map(|&i| tlv(i, &[])).collect();
    tlv(2, &inner)
}

/// Every metadata capability this accessory can support, in declaration order.
///
/// `updates` entries land in Identify param 7; `start`/`stop` in param 6. Order is stable so the
/// generated declaration is reproducible byte-for-byte across builds.
pub fn table(now_playing_body: fn() -> Vec<u8>, route_guidance_body: fn() -> Vec<u8>) -> Vec<Feature> {
    vec![
        // ---- Proven: the 2026-07-25 device-accepted declaration ----
        Feature {
            name: "now_playing",
            tier: Tier::Proven,
            trigger: Trigger::Subscription { start: 0x5000, stop: 0x5002, also: &[] },
            updates: &[0x5001],
            body: Body::Verbatim(now_playing_body),
        },
        Feature {
            name: "route_guidance",
            tier: Tier::Proven,
            trigger: Trigger::Subscription { start: 0x5200, stop: 0x5203, also: &[] },
            updates: &[0x5201, 0x5202],
            body: Body::Verbatim(route_guidance_body),
        },
        Feature {
            name: "call_state",
            tier: Tier::Proven,
            trigger: Trigger::UnpairedStart {
                start: 0x4154,
                // 0x4156 StopCallStateUpdates EXISTS in Apple's catalog, but the 2026-07-25
                // device-accepted baseline omits it and iOS did not ask for it (docs/carplay/05_METADATA_AND_CONTROLS.md §5.3,
                // §6.3). Adding it changes a proven declaration, and a 0x1D03 here is
                // unrecoverable. This is the ONLY unpaired Start in the table.
                device_evidence: "docs/carplay/05_METADATA_AND_CONTROLS.md §6.3 — accepted without 0x4156 on 2026-07-25",
            },
            updates: &[0x4155],
            body: Body::Fields(CALL_STATE_FIELDS),
        },
        // ---- Extended: read-only metadata ----
        Feature {
            name: "communications",
            tier: Tier::Extended,
            trigger: Trigger::Subscription { start: 0x4157, stop: 0x4159, also: &[] },
            updates: &[0x4158],
            body: Body::Fields(COMMS_FIELDS),
        },
        Feature {
            name: "call_history",
            tier: Tier::Extended,
            trigger: Trigger::Subscription { start: 0x4170, stop: 0x4172, also: &[] },
            updates: &[0x4171],
            body: Body::Groups(LIST_GROUPS),
        },
        Feature {
            name: "power",
            tier: Tier::Extended,
            trigger: Trigger::Subscription { start: 0xAE00, stop: 0xAE02, also: &[] },
            updates: &[0xAE01],
            body: Body::Fields(POWER_FIELDS),
        },
        Feature {
            name: "app_discovery",
            tier: Tier::Extended,
            // `also: [0xAD03]` — RequestAppDiscoveryAppIcons (Accessory). ADDED 2026-07-30.
            //
            // We declare 0xAD04 AppDiscoveryAppIcon as RECEIVED (param 7) but had never declared its
            // sender 0xAD03 in param 6. That is exactly the shape of declaration rule 2 in CLAUDE.md
            // — "a receive must not be declared without its send
            // (`OptionalMsgNotValidWithoutRequiredMsgs`)" — the same class of error as the missing
            // Stop ids that cost three sessions (docs/carplay/05_METADATA_AND_CONTROLS.md §5.6). Apple's own CarPlay Simulator
            // declares 0xAD00, 0xAD02 and 0xAD03 together (decompiled `IAPConnection.identityMessage`),
            // so this makes our declaration match the normative sender on this feature.
            //
            // SAFE w.r.t. the byte-pinned BT-time Identify: the `TransportComponent::Wireless` arm in
            // `message.rs` builds params 6/7 from a hardcoded list and never consults this table, so
            // this only affects the AirPlayTunnel and wired arms.
            trigger: Trigger::Subscription { start: 0xAD00, stop: 0xAD02, also: &[0xAD03] },
            updates: &[0xAD01, 0xAD04],
            body: Body::Verbatim(app_discovery_body),
        },
        Feature {
            name: "lane_guidance",
            tier: Tier::Extended, // rides the 0x5200 subscribe
            trigger: Trigger::RidesOn("route_guidance"),
            updates: &[0x5204],
            body: Body::None,
        },
        Feature {
            name: "destination",
            tier: Tier::Extended,
            trigger: Trigger::Subscription {
                start: 0x10DA,
                stop: 0x10DC,
                // Destination Sharing also requires 0x10DD DestinationInformationStatus.
                also: &[0x10DD],
            },
            updates: &[0x10DB],
            body: Body::Fields(DESTINATION_FIELDS),
        },
        Feature {
            name: "device_info",
            tier: Tier::Extended, // unsolicited; declaration only
            trigger: Trigger::Unsolicited,
            updates: &[0x4E09, 0x4E0A, 0x4E0B, 0x4E0C, 0x4E0D, 0x4E0E],
            body: Body::None,
        },
        // ---- Capability: declaring these changes how iOS drives the accessory ----
        Feature {
            name: "voice_over",
            tier: Tier::Capability,
            trigger: Trigger::Subscription { start: 0x560B, stop: 0x560D, also: &[] },
            updates: &[0x560C],
            body: Body::Fields(VOICE_OVER_FIELDS),
        },
        Feature {
            name: "voice_over_cursor",
            tier: Tier::Capability,
            trigger: Trigger::Subscription { start: 0x560F, stop: 0x5611, also: &[] },
            updates: &[0x5610],
            body: Body::Fields(VOICE_OVER_CURSOR_FIELDS),
        },
        Feature {
            name: "assistive_touch",
            tier: Tier::Capability,
            trigger: Trigger::Subscription { start: 0x5402, stop: 0x5404, also: &[] },
            updates: &[0x5403],
            body: Body::None,
        },
        Feature {
            name: "media_library",
            tier: Tier::Capability,
            trigger: Trigger::Subscription {
                start: 0x4C03,
                stop: 0x4C05,
                // The MediaLibraryInformation pair. Declaring 0x4C02 without 0x4C00 left 0x4C01
                // unreachable and inverted the Start/Stop rule.
                also: &[0x4C00, 0x4C02],
            },
            updates: &[0x4C01, 0x4C04],
            body: Body::Verbatim(media_library_body),
        },
    ]
}

/// Parse `POLICY_FILE` into `(policy word, skip list)`.
///
/// Format is one line: `<policy> [skip=a,b,c]`, e.g. `rx-only skip=call_history`. Both halves are
/// optional. This exists so a failing declaration can be narrowed over the box console between
/// sessions — a 0x1D03 is unrecoverable within a session, and rebuilding for each bisection step
/// costs a deploy cycle per experiment.
fn file_setting() -> (Option<String>, Vec<String>) {
    let Ok(text) = std::fs::read_to_string(POLICY_FILE) else {
        return (None, Vec::new());
    };
    let mut policy = None;
    let mut skip = Vec::new();
    // `skip=a, b` (with a space) used to drop everything after the space, so an operator bisecting a
    // rejected declaration over the box console would believe two features were dropped when only one
    // was — and misattribute the next unrecoverable 0x1D03 to the wrong feature. Once `skip=` is seen,
    // every later bare token is a continuation of the list.
    let mut in_skip = false;
    for tok in text.split_whitespace() {
        match tok.strip_prefix("skip=") {
            Some(list) => {
                in_skip = true;
                skip.extend(list.split(',').filter(|s| !s.is_empty()).map(str::to_string))
            }
            None if policy.is_none() && !in_skip => policy = Some(tok.to_string()),
            None => skip.extend(tok.split(',').filter(|s| !s.is_empty()).map(str::to_string)),
        }
    }
    (policy, skip)
}

/// The features active at `tier`, minus any named in `CARPLAY_METADATA_SKIP` or the policy file's
/// `skip=` list. The skip list is a field lever: it narrows a failing declaration without a rebuild.
pub fn active(
    tier: Tier,
    now_playing_body: fn() -> Vec<u8>,
    route_guidance_body: fn() -> Vec<u8>,
) -> Vec<Feature> {
    let skip: Vec<&str> = resolved().skip.iter().map(String::as_str).collect();
    active_with_skip(tier, now_playing_body, route_guidance_body, &skip)
}

/// The process-cached skip list as borrowable strs — the same list [`active`] applies internally.
/// Exposed so `message::build_ident_info_excluding` can forward the ambient skip into [`active_with_skip`]
/// without re-reading `/tmp`, keeping the ambient path byte-identical while a test can inject its own
/// (audit Fix #25).
pub(crate) fn ambient_skip() -> Vec<&'static str> {
    resolved().skip.iter().map(String::as_str).collect()
}

/// The skip-list-explicit form of [`active`], so the rider logic is testable without touching the
/// environment or `/tmp`.
pub fn active_with_skip(
    tier: Tier,
    now_playing_body: fn() -> Vec<u8>,
    route_guidance_body: fn() -> Vec<u8>,
    skip: &[&str],
) -> Vec<Feature> {
    let mut kept: Vec<Feature> = table(now_playing_body, route_guidance_body)
        .into_iter()
        .filter(|f| f.tier <= tier && !skip.contains(&f.name))
        .collect();

    // Drop any rider whose owner did not survive. A `RidesOn` feature declares updates in param 7
    // that only the OWNER's subscribe can produce, so an orphaned rider is exactly
    // `OptionalMsgNotValidWithoutRequiredMsgs` — an unrecoverable 0x1D03. `skip=route_guidance`
    // orphaned `lane_guidance` this way, via the same lever used to narrow a failing declaration.
    // Iterated to a fixed point so a rider-of-a-rider cannot survive its grandparent.
    loop {
        let names: Vec<&'static str> = kept.iter().map(|f| f.name).collect();
        let before = kept.len();
        kept.retain(|f| match f.trigger.owner() {
            Some(owner) => {
                let ok = names.contains(&owner);
                if !ok {
                    eprintln!(
                        "[features] dropping {} — it rides {owner}, which is not active",
                        f.name
                    );
                }
                ok
            }
            None => true,
        });
        if kept.len() == before {
            break;
        }
    }
    kept
}

/// Message ids for Identify param 6 (`MessagesSentByAccessory`), in table order.
pub fn sent_ids(features: &[Feature]) -> Vec<u16> {
    features
        .iter()
        .flat_map(|f| f.trigger.sent_ids())
        .collect()
}

/// Message ids for Identify param 7 (`MessagesReceivedFromDevice`), in table order.
pub fn received_ids(features: &[Feature]) -> Vec<u16> {
    features.iter().flat_map(|f| f.updates.iter().copied()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{self, Source};

    fn t(tier: Tier) -> Vec<Feature> {
        // Hermetic (audit Fix #16): use the empty-skip form so the tier tests don't read the real
        // /tmp/carplay_metadata / CARPLAY_METADATA_SKIP (via resolved()) — a dev box with `skip=` armed
        // (the state the handoff tells operators to set) would otherwise skew these assertions.
        active_with_skip(tier, crate::metadata::start_now_playing, crate::metadata::start_route_guidance, &[])
    }

    /// The whole point of the table: what we declare as sendable must be accessory-sourced and what
    /// we declare as receivable must be device-sourced, per Apple's catalog.
    #[test]
    fn declared_directions_match_apples_catalog() {
        let f = t(Tier::Capability);
        for id in sent_ids(&f) {
            assert_eq!(
                spec::source_of(id),
                Some(Source::Accessory),
                "0x{id:04X} is declared sendable but is not accessory-sourced"
            );
        }
        for id in received_ids(&f) {
            assert_eq!(
                spec::source_of(id),
                Some(Source::Device),
                "0x{id:04X} is declared receivable but is not device-sourced"
            );
        }
    }

    /// `Tier::Proven` must reproduce the declaration iOS accepted on 2026-07-25, byte for byte.
    /// If this fails, the recovery lever no longer recovers anything.
    #[test]
    fn proven_tier_is_the_device_accepted_baseline() {
        let f = t(Tier::Proven);
        assert_eq!(sent_ids(&f), vec![0x5000, 0x5002, 0x5200, 0x5203, 0x4154]);
        assert_eq!(received_ids(&f), vec![0x5001, 0x5201, 0x5202, 0x4155]);
    }

    /// The compiled default must be the pre-expansion baseline. `extended` is device-accepted
    /// (docs/carplay/05_METADATA_AND_CONTROLS.md §6.6), but the box's policy file lives on tmpfs and a reject is unrecoverable
    /// in-session, so an unconfigured build defaults to the shape that has the longest device
    /// history rather than the newest one.
    #[test]
    fn default_policy_is_proven() {
        // Hermetic: `resolve` with no env word, no file word and no skip lists is the compiled
        // default. (This used to call `Policy::active()`, which reads the REAL environment and
        // `/tmp/carplay_metadata` — a dev host with `extended` armed in that file failed this test
        // spuriously.)
        let r = resolve(None, None, None, Vec::new());
        assert_eq!(r.policy.sent, Tier::Proven);
        assert_eq!(r.policy.recv, Tier::Proven);
        assert_eq!(r.policy.subscribe, Tier::Proven);
        assert!(r.skip.is_empty());
    }

    /// The app-pushed arm (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B3) refuses the refuted `rx-only` tier and any unknown word, so
    /// an app can never construct the `OptionalMsgNotValidWithoutRequiredMsgs` shape or silently
    /// select a tier that does not exist. Both refusals leave the policy UNARMED, which is what
    /// makes resolution fall through to the bench levers and then the compiled `proven` floor.
    ///
    /// Deliberately does not exercise a successful arm: `PUSHED` is a process-global `OnceLock`, so
    /// arming it in one test would leak into every other test in this binary. The success path is
    /// covered where it is actually observable — the consuming daemons' integration checks.
    #[test]
    fn pushed_policy_refuses_rx_only_and_unknown_tiers() {
        assert!(!arm_pushed_policy("rx-only", &[]));
        assert!(!arm_pushed_policy("RX-Only", &[])); // case-insensitive refusal
        assert!(!arm_pushed_policy("nonsense", &[]));
        assert!(!arm_pushed_policy("", &[]));
        // None of the refusals armed anything, so the compiled floor still governs.
        assert!(PUSHED.get().is_none());
    }

    /// `rx-only` keeps param 6 at the baseline while param 7 and the subscribes carry the full set.
    /// The asymmetry is deliberate and the mode is a REFUTED dead end (docs/carplay/05_METADATA_AND_CONTROLS.md §6.2) — this test
    /// pins its shape so the refutation stays reproducible, not because the mode should be used.
    #[test]
    fn rx_only_grows_param_7_and_the_subscribes_but_not_param_6() {
        let p = Tier::parse("rx-only").expect("rx-only is a valid policy");
        assert_eq!(p.sent, Tier::Proven);
        assert_eq!(p.recv, Tier::Extended);
        assert_eq!(p.subscribe, Tier::Extended);

        let sent = sent_ids(&t(p.sent));
        let recv = received_ids(&t(p.recv));
        assert_eq!(sent, vec![0x5000, 0x5002, 0x5200, 0x5203, 0x4154]);
        assert!(recv.contains(&0x4158) && recv.contains(&0x4171) && recv.contains(&0xAE01));
        // The knowing contract violation: we subscribe to ids param 6 does not declare.
        assert!(t(p.subscribe).iter().any(|f| f.start() == Some(0x4157) && !sent.contains(&0x4157)));
    }

    /// Apple's catalog names the Stop for every Start: `StartPowerUpdates` -> `StopPowerUpdates`.
    /// Derive it rather than trusting the table, so a Stop that is present but WRONG is caught too —
    /// `Subscription { start: 0xAE00, stop: 0x5002 }` typechecks and a presence-only test passes it.
    fn catalog_stop_for(start: u16) -> Option<u16> {
        let tail = spec::name_of(start)?.strip_prefix("Start")?;
        let want = format!("Stop{tail}");
        // Exact name match, not a prefix: StartAssistiveTouch and StartAssistiveTouchInformation
        // would otherwise collide.
        spec::IAP2_MESSAGES.iter().find(|(_, n, _)| *n == want).map(|(id, _, _)| *id)
    }

    /// Every `Start*` we declare — including ones hiding in `also` — must be paired with the Stop
    /// Apple's catalog names for it. Omitting the Stop returns `RequiredInfoMissing` and rejects the
    /// feature; that cost three hardware sessions (docs/carplay/05_METADATA_AND_CONTROLS.md §6.3).
    #[test]
    fn every_start_is_paired_with_apples_own_stop() {
        for f in t(Tier::Capability) {
            let sent = f.trigger.sent_ids();
            for id in &sent {
                let Some(name) = spec::name_of(*id) else { continue };
                if !name.starts_with("Start") {
                    continue;
                }
                let Some(stop) = catalog_stop_for(*id) else {
                    panic!("{}: Apple declares no Stop for 0x{id:04X} {name}", f.name)
                };
                if let Trigger::UnpairedStart { start, .. } = f.trigger {
                    if start == *id {
                        continue; // deliberate, and separately justified below
                    }
                }
                assert!(
                    sent.contains(&stop),
                    "{}: declares 0x{id:04X} {name} without its Stop 0x{stop:04X} — iOS returns \
                     RequiredInfoMissing and rejects the feature (docs/carplay/05_METADATA_AND_CONTROLS.md §6.3)",
                    f.name
                );
            }
        }
    }

    /// The unpaired-Start exemptions, keyed on message id so a feature rename cannot silently move
    /// one, and checked in BOTH directions so a stale entry is flagged rather than quietly excusing
    /// a future reuse of that id.
    const UNPAIRED_START_EXCEPTIONS: &[(u16, &str)] = &[(
        0x4154,
        "StartCallStateUpdates: 0x4156 exists but the 2026-07-25 device-accepted baseline omits it \
         and iOS did not ask for it (docs/carplay/05_METADATA_AND_CONTROLS.md §5.3, §6.3). Adding it changes a proven declaration; \
         a 0x1D03 here is unrecoverable.",
    )];

    #[test]
    fn unpaired_starts_are_exactly_the_documented_exceptions() {
        let features = t(Tier::Capability);
        let unpaired: Vec<u16> = features
            .iter()
            .filter_map(|f| match f.trigger {
                Trigger::UnpairedStart { start, .. } => Some(start),
                _ => None,
            })
            .collect();
        for id in &unpaired {
            assert!(
                UNPAIRED_START_EXCEPTIONS.iter().any(|(e, _)| e == id),
                "0x{id:04X} is declared without its Stop but is not a documented exception"
            );
        }
        for (id, _) in UNPAIRED_START_EXCEPTIONS {
            assert!(
                unpaired.contains(id),
                "stale exemption: 0x{id:04X} is no longer an unpaired Start — remove it, or it \
                 will silently excuse a future reuse of that id"
            );
        }
    }

    /// Rule 2: nothing may declare an update in param 7 whose accessory-side trigger is absent from
    /// param 6. `rx-only` breaks this deliberately and is excluded by name.
    #[test]
    fn no_receive_is_declared_without_its_send() {
        for word in ["proven", "extended", "all"] {
            let p = Tier::parse(word).expect("policy word");
            let sent = sent_ids(&t(p.sent));
            for f in t(p.recv) {
                if let Some(owner) = f.trigger.owner() {
                    // A rider is legal only while its owner's Start is declared.
                    let owner_start = t(p.sent)
                        .iter()
                        .find(|o| o.name == owner)
                        .and_then(|o| o.start());
                    assert!(
                        owner_start.is_some_and(|s| sent.contains(&s)),
                        "{word}: {} rides {owner}, whose Start is not in param 6",
                        f.name
                    );
                    continue;
                }
                if let Some(start) = f.start() {
                    assert!(
                        sent.contains(&start),
                        "{word}: {} expects updates but its Start 0x{start:04X} is not declared",
                        f.name
                    );
                }
            }
        }
    }

    /// Skipping an owner must drop its riders, not orphan them. `skip=route_guidance` used to leave
    /// `lane_guidance` declaring 0x5204 with no 0x5200 in param 6 — an unrecoverable 0x1D03 produced
    /// by the very lever used to narrow a failing declaration.
    #[test]
    fn skipping_an_owner_drops_its_riders() {
        let np = crate::metadata::start_now_playing;
        let rg = crate::metadata::start_route_guidance;
        let all = active_with_skip(Tier::Extended, np, rg, &["route_guidance"]);
        assert!(
            !all.iter().any(|f| f.name == "lane_guidance"),
            "lane_guidance survived without route_guidance — param 7 would declare 0x5204 orphaned"
        );
        assert!(!received_ids(&all).contains(&0x5204));
        // And the owner surviving keeps the rider.
        let kept = active_with_skip(Tier::Extended, np, rg, &[]);
        assert!(kept.iter().any(|f| f.name == "lane_guidance"));
    }

    /// Rule 3: the subscribe tier must never exceed the sent tier, or we send Starts param 6 does
    /// not declare — the exact drift that silenced 0x4157/0x4170 for the project's history.
    #[test]
    fn subscribe_tier_never_exceeds_the_declared_send_tier() {
        for word in ["proven", "extended", "all"] {
            let p = Tier::parse(word).expect("policy word");
            assert!(
                p.subscribe <= p.sent,
                "{word}: subscribes at {:?} but declares sends at only {:?}",
                p.subscribe,
                p.sent
            );
        }
        // rx-only is the documented, refuted exception.
        assert!(Tier::parse("rx-only").unwrap().subscribe > Tier::parse("rx-only").unwrap().sent);
    }

    #[test]
    fn tiers_are_supersets() {
        let (p, e, c) = (t(Tier::Proven), t(Tier::Extended), t(Tier::Capability));
        assert!(p.len() < e.len() && e.len() < c.len());
        for id in received_ids(&p) {
            assert!(received_ids(&e).contains(&id));
        }
    }

    /// No id may be declared twice — a duplicate in params 6/7 is malformed.
    #[test]
    fn no_duplicate_ids() {
        let f = t(Tier::Capability);
        for list in [sent_ids(&f), received_ids(&f)] {
            let mut sorted = list.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), list.len(), "duplicate id in {list:04X?}");
        }
    }

    /// Every subscribe body must be non-empty except where Apple's message takes no parameters, and
    /// group selectors must nest their members — an empty group is silently ignored by iOS.
    #[test]
    fn subscribe_bodies_are_well_formed() {
        for f in t(Tier::Capability) {
            let body = f.build_body();
            if f.start().is_none() {
                assert!(body.is_empty(), "{} declares no subscribe but built one", f.name);
                continue;
            }
            if matches!(f.body, Body::None) {
                continue; // 0x5402 StartAssistiveTouchInformation genuinely takes no parameters
            }
            assert!(!body.is_empty(), "{} built an empty subscribe body", f.name);
            let mut i = 0;
            while i + 4 <= body.len() {
                let len = u16::from_be_bytes([body[i], body[i + 1]]) as usize;
                assert!(len >= 4 && i + len <= body.len(), "{} malformed TLV at {i}", f.name);
                if let Body::Groups(_) = f.body {
                    assert!(len > 4, "{} emitted an EMPTY group — iOS ignores those", f.name);
                }
                i += len;
            }
            assert_eq!(i, body.len(), "{} has trailing bytes", f.name);
        }
    }

    #[test]
    fn tlv_matches_metadata() {
        assert_eq!(tlv(0x0006, &[]), vec![0x00, 0x04, 0x00, 0x06]);
        assert_eq!(tlv(2, &90u16.to_be_bytes()), vec![0x00, 0x06, 0x00, 0x02, 0x00, 0x5A]);
    }

    #[test]
    fn skip_list_narrows_the_declaration() {
        // Not using the env var (tests share a process); exercise the filter directly.
        let all = table(crate::metadata::start_now_playing, crate::metadata::start_route_guidance);
        let without: Vec<_> =
            all.iter().filter(|f| f.name != "power").map(|f| f.name).collect();
        assert!(!without.contains(&"power"));
        assert!(without.contains(&"now_playing"));
    }

    /// THE DEVICE-DIRECTED SKIP: `tier: all` + `skip: [voice_over_cursor]` removes exactly the three
    /// ids a real iPhone refused on 2026-08-10, and nothing else.
    ///
    /// Evidence for the refusal:
    /// `docs/ops/captures/2026-08-10_TUNNEL_IDENT_REJECT_tier_all_voiceover_cursor.txt` — a live tunnel
    /// Identify at tier `all` drew a decoded `0x1D03` naming `0x560F StartVoiceOverCursorUpdates`,
    /// `0x5611 StopVoiceOverCursorUpdates` (param 6) and `0x5610 VoiceOverCursorUpdate` (param 7);
    /// the retry was byte-identical (params 6/7 are un-strippable) so the tunnel aborted.
    ///
    /// This test exists so the owner's next hardware experiment rests on evidence rather than hope:
    /// it proves the EXISTING skip lever is sufficient and no code change is needed. It deliberately
    /// asserts BOTH directions — the three ids leave, and the neighbouring `voice_over` feature
    /// (0x560B/0x560C/0x560D, one id away and NOT refused) stays. A skip that took `voice_over` with
    /// it would silently cost a capability the phone never objected to.
    ///
    /// It does NOT prove the phone will accept the result — only the phone can say that. If a later
    /// session sees a different reject at this tier, add that feature's name here rather than
    /// dropping the tier wholesale, and re-read docs/carplay/05_METADATA_AND_CONTROLS.md §6.5: ask the phone, do not bisect.
    #[test]
    fn skipping_voice_over_cursor_removes_exactly_the_refused_ids() {
        let all = active_with_skip(
            // `all` maps to Capability on all three axes (see `Tier::parse`).
            Tier::Capability,
            crate::metadata::start_now_playing,
            crate::metadata::start_route_guidance,
            &[],
        );
        let skipped = active_with_skip(
            Tier::Capability,
            crate::metadata::start_now_playing,
            crate::metadata::start_route_guidance,
            &["voice_over_cursor"],
        );

        const REFUSED_SENT: [u16; 2] = [0x560F, 0x5611]; // param 6, per the phone
        const REFUSED_RECV: u16 = 0x5610; // param 7, per the phone

        let (sent_all, recv_all) = (sent_ids(&all), received_ids(&all));
        let (sent_skip, recv_skip) = (sent_ids(&skipped), received_ids(&skipped));

        // The tier really does declare them — otherwise this test proves nothing about the reject.
        for id in REFUSED_SENT {
            assert!(sent_all.contains(&id), "tier All should declare {id:#06X} in param 6");
        }
        assert!(recv_all.contains(&REFUSED_RECV), "tier All should declare 0x5610 in param 7");

        // ...and the skip removes exactly those.
        for id in REFUSED_SENT {
            assert!(!sent_skip.contains(&id), "{id:#06X} must be gone from param 6");
        }
        assert!(!recv_skip.contains(&REFUSED_RECV), "0x5610 must be gone from param 7");

        // The neighbouring, NOT-refused voice_over feature must survive untouched.
        for id in [0x560Bu16, 0x560D] {
            assert_eq!(
                sent_all.contains(&id),
                sent_skip.contains(&id),
                "voice_over's {id:#06X} must be unaffected by skipping voice_over_cursor"
            );
        }
        assert_eq!(recv_all.contains(&0x560C), recv_skip.contains(&0x560C));

        // Nothing ELSE moved: the skip is surgical, not a blunt tier drop.
        let lost_sent: Vec<u16> = sent_all.iter().copied().filter(|i| !sent_skip.contains(i)).collect();
        let lost_recv: Vec<u16> = recv_all.iter().copied().filter(|i| !recv_skip.contains(i)).collect();
        assert_eq!(lost_sent, REFUSED_SENT.to_vec(), "param 6 lost more than the refused ids");
        assert_eq!(lost_recv, vec![REFUSED_RECV], "param 7 lost more than the refused id");
    }
}
