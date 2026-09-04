# CarPlay session lifecycle and management

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 58, 08, 11, 44, 31, 54, 55, 09, 10; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

**Contents:** the lifecycle model → hardening → real-world comparison → start ordering → the app-driven SETUP relay → session management as it stands. Historical captures and the peer-wipe postmortem are kept at the end as evidence.

## Session management — current model

<!-- absorbed: ../carplay/02_SESSION_LIFECYCLE.md -->

**Status: RESEARCH, VERIFIED (2026-08-16).** The protocol-level session lifecycle — how a CarPlay
session is offered, started, kept alive, stopped and reclaimed **between the phone and the accessory**.

This is deliberately *not* docs/carplay/02_SESSION_LIFECYCLE.md. `../carplay/02_SESSION_LIFECYCLE.md` documents the **OCBM host-presence**
lifecycle (box ↔ macOS app: IDLE/ARMED/STREAMING/RECOVERING). It is correct and unchanged. What it does
not cover — and what this project has been deriving on the fly — is the **phone-facing** lifecycle.

**Every claim below was put through an adversarial verification pass** (20 independent checks, several
carried to disassembly). Six claims in the first draft were **refuted** and are corrected here; the
refutations are recorded in §12 rather than deleted, because the *way* they were wrong is the reusable
lesson. Where something is inference rather than observation, it now says so.

**Headline finding (CONFIRMED): the entire "Session Management" feature is post-2017 and absent from
R14G17.** `sessionManagementInfo`, `stopSessionReasons`, `teardownCompleted`, `sessionsStopped`,
`isRemoteControlOnly`, `sessionWillBeHijacked`, `sessionCorrelationUUID` and `DeselectAllSessions` return
zero hits across the whole licensed 2017 drop — verified with case-insensitive, binary-safe, separator-
variant sweeps and a ChangeLog check. R14G17's `stopSession` is a *different, unrelated* command (§5.1).

---

### 1. Sources, and why the standalone Simulator changed the answer

| Source | Path | Answers |
|---|---|---|
| `CarPlaySDK.framework` (standalone) | `~/Downloads/Carplay WWDC/Hardware/CarPlay Simulator.app/Contents/Frameworks/CarPlaySDK.framework` | Current receiver-side contract: SETUP keys, `/info` handling, `/command` dispatch, control-plane ports |
| CarPlay Simulator binary | `…/CarPlay Simulator.app/Contents/MacOS/CarPlay Simulator` | Vehicle-side `SessionEventType` enum and the `SessionController` operational log (§3) |
| `iAP2MessageKit` **external** spec archive | `…/Frameworks/iAP2MessageKit.framework/Resources/iap2messages-external.i2mspecarchive` | Byte-authoritative iAP2 parameter tables (§4) |
| iOS 27 extract — CarKit | `reference/ios27_extract/headers/CarKit/CarKit/` | Phone-side counterpart: `CARSession`, `CARSessionStatus`, `CARSessionObserving` (§9) |
| iOS 27 extract — mined strings | `reference/ios27_extract/mined/` | `AirPlaySender`/`AirPlayReceiver` cross-checks that settled two disputes |
| R14G17 licensed source | `~/carlink/local_carplay_sdk/reference/apple_carplay_sdk_R14G17/` | Literal C for what it covers: teardown triggers, timeout constants, hijack |

#### 1.1 This standalone build is NOT the one docs/ops/03_REFERENCE_INDEX.md already indexed

Verified by content diff, not by version numbers. The two `CarPlaySDK` binaries differ (distinct SHA-256,
6,775,712 vs 6,603,728 bytes, `arm64` vs `arm64e`, XBS trains `CarPlaySimulator` vs
`CarPlaySimulator_Devices`). **Build numbers 267 vs 624 are NOT comparable** — different trains, and both
carry the same `DTXcodeBuild=27A228` stamp. Do not date-order them that way.

The decisive evidence is the string diff. **Xcode's already-indexed copy is missing 5 of the 9
session-management strings this document depends on** — `isRemoteControlOnly`, `sessionWillBeHijacked`,
`sessionCorrelationUUID`, `teardownCompleted`, `sessionsStopped` are simply not in it. The standalone is a
near-strict superset (665 unique real strings vs Xcode's 384, and Xcode's 384 are *entirely*
`/AppleInternal/…` build-path artifacts — zero session content). So docs/ops/03_REFERENCE_INDEX.md's §C entry **could not** have
produced this material. That is why it sat undiscovered, and it is the reason to treat the standalone as
the primary receiver-side source.

Byte-identical between the two copies: all ten `VehicleConfig` templates (SHA-256), and the parameter
trees for 0x4300 / 0x4301 / 0x4E0D across external vs internal spec archives.

#### 1.2 Dead ends, recorded so nobody re-walks them

- **`Contents/Resources/Javascript/*.bundle.js` (~3.8 MB) is the AirPlay Video player web app.** Verified:
  zero hits across all 26 bundles for `stopSession`, `sessionManagement`, `disconnectReason`, `carplay`,
  `sessionUUID`, `RemoteControlSession`, `/command`, `/info`, `RTSP`, `SETUP`. The only `teardown` hits are
  AVQueuePlayer item-lifecycle and hls.js subscription cleanup. `manifest.json` names it outright
  (`hlsJsVersion`, `atvekitJsVersion`, `playerCapabilities.supportsOfflineHLS`).
- **Two unrelated `disconnectReason` keys exist, in two different binaries.** The telephony one is in the
  *Simulator* binary (neighbours `isConferenced`, `conferenceGroup`, then `noReasonCallEnded` /
  `callDeclined` / `callFailed`), corroborated byte-authoritatively by the spec archive: `0x16725
  CallStateUpdate` carries enum `CallStateUpdateDisconnectReason` = 0 No Reason / 1 Declined / 2 Failed.
  The session-stop one is in *CarPlaySDK*. Clean cross-binary separation — do not conflate.
- **No shipped `VehicleConfig` template sets `sessionManagement`.** Verified by full read of all ten, plus
  this machine's live `~/Library/CarPlaySimulator/UserConfigs/`. **But "it defaults off" is NOT
  established** — the default lives in a compiled Swift initializer; absence from templates is equally
  consistent with defaulting on. Note the schema property is `sessionManagement`, *not*
  `enablesSessionManagement` like every sibling (`enablesHEVC`, `enablesMainBufferedAudio`, …), which hints
  it may not be a plain Bool. Also do not confuse it with `Settings.yaml`'s app-level `autoLaunchSessions`.

---

### 2. Two lifecycles, not one

```
  ┌─ TRANSPORT / DISCOVERY  (iAP2 + Bonjour + Wi-Fi/BT)  ── owner: CarPlayControl / iap2d
  │     phone announces CarPlayAvailability          (iAP2 0x4300, device → accessory)
  │     accessory answers CarPlayStartSession        (iAP2 0x4301, accessory → device)
  │        ↓ carries: IP, port, SSID/passphrase/channel, deviceID, PublicKey, SourceVersion
  │
  └─▶ ┌─ AIRPLAY SESSION  (RTSP-over-HTTP on the port named above) ── owner: airplayd / receiver
        │   pair-verify → SETUP(phase 1: control) → SETUP(phase 2: streams) → RECORD
        │   ... /feedback, POST /command, event channel, timing, keepAlive beacon ...
        │   TEARDOWN (partial: streams[] | full: no body)
        └─ AirPlayReceiverSessionCreate → Setup → Start → TearDown → Finalize
```

An accessory can lose the AirPlay session while keeping the transport (our "holding pattern"), but not the
reverse.

---

### 3. The vehicle-side event vocabulary

> **Correction (verification pass).** The first draft presented an ordered ladder and called it "Apple's
> own ordering." **That was wrong.** The backing Swift enum's real declaration order contradicts it:
> `startedConnection` sits at position ~22 of ~39 (not first), and `sessionResetStart`/`sessionResetEnd`
> sit mid-list (not last, as terminal wrap-up). `airPlaySessionFailed` is declared *before* session
> creation. The enum reads as one that grew by appending cases as features shipped — it encodes no
> temporal order at all. **String-table adjacency is ordering-blind; never read sequence out of it.**

What is actually established is the **vocabulary**, from the `SessionEventType` enum backing
`_TtC17CarPlay_Simulator13SessionEvents`. Real declaration order:

```
loadedSessionState, wifiStarting, wifiStarted, wifiFailed,
bluetoothDisconnected, bluetoothConnecting, bluetoothConnected,
iAPAuthenticationFailed, iAPAuthenticationSucceded,
iAPIdentificationRejected, iAPIdentificationAccepted, iAPIdentificationRejectedWillRetry,
carPlayAvailabilityReceived, carPlayStartSessionSent,
airPlaySessionFailed,
stagingVDC, loadingVDC, loadedVDC,
sessionCreated, sessionResetStart, sessionResetEnd,
startedConnection,
bonjourControlSent, bonjourControlFailed,
airPlayServerStarted, airPlayServerStopped,
airPlaySessionCreated, airPlaySessionStarted, airPlaySessionStopped, airPlaySessionStoppedByDevice,
stoppedForThemeAssets,
iAP2Started, iAP2Stopped, carpCreated,
loggingRequested, loggingGenerationStarted, loggingGenerationCompleted, loggingGenerationFailed,
loggingTransferStarted, loggingTransferEnded
```

A plausible *temporal* sequence (WiFi/BT → iAP auth → iAP identify → availability → start-session →
Bonjour → AirPlay server → AirPlay session) is a **reasonable domain-informed reconstruction and nothing
more**. Treat it as a working hypothesis to test against a live capture, not as evidence.

#### 3.1 The better source: the `SessionController` operational log

Separate from the UI-history enum, and more authoritative for bring-up/teardown because it is
parameterised per device:

```
Start Session Triggered · Starting Session for %s - %s · Restarting Session for %s - %s
Stopping Session for %s - %s · Session stopped for %s - %s · Session disconnected
Session Controller Init · Session Controller Fully Stopped · Session Controller Invalidate
Failed Connection Step - %s · Connection start cancelled for %s - %s · Failed to start connection for %s - %s
Loading Session State - %s - %s · Saving Session State - %s · Failed to Save Session State
Session Reset Complete for %s - %s · Session Event: %s
```

`Session Event: %s` is the call site that prints each `SessionEventType`, i.e. this layer *wraps* the one
above.

#### 3.2 Four observations that survive verification

1. **`iAPIdentificationRejectedWillRetry` is its own enum case**, distinct from `iAPIdentificationRejected`.
   Apple's own vehicle treats one reject as recoverable at the transport layer. docs/carplay/05_METADATA_AND_CONTROLS.md's rule that a
   `0x1D03` is unrecoverable *within a session* still stands — these are different scopes.
2. **Per-device session state is persisted** — `Loading Session State`, `Saving Session State`,
   `Failed to Save Session State`. That is the ladder position of our known-devices work.
3. **There are THREE terminal states, not two**: `airPlaySessionStopped`, `airPlaySessionStoppedByDevice`,
   and `stoppedForThemeAssets` — the last is its own case, not a qualifier on the second. It pairs with
   `0x4300`'s `ThemeAssetsAttributes.Available`.
4. **`Failed to start a connection after 30 seconds`** is the Simulator's UI connection watchdog. **It is
   NOT the same timer as `kAirPlayDataTimeoutSecs` in §6** — unrelated mechanisms that happen to share the
   value 30. Do not conflate them.

Two quotes the first draft got slightly wrong: *"Failed to report session startup."* and *"If connecting
wirelessly this will result in more errors and the session will likely fail"* are **two adjacent strings**
(an alert title/message pair), not one; and that wording says startup reporting is *risky to skip*, not
*mandatory* — the first draft overstated it.

---

### 4. iAP2 layer

From the **external** (shipped MFi) archive: 129 messages, a strict subset of the internal archive's 144.
For every message cited here the two archives are **byte-identical**, so nothing below carries
external-vs-internal divergence risk.

#### 4.1 `0x4300 CarPlayAvailability` (device → accessory) — CONFIRMED verbatim

```
0  WiredAttributes      <group> [optional]
   0  Available                    <bool> [required]
   1  USBTransportIdentifier       <utf8> [optional]
1  WirelessAttributes   <group> [optional]
   0  Available                    <bool> [required]
   1  BluetoothTransportIdentifier <utf8> [optional]
2  ThemeAssetsAttributes<group> [optional]
   0  Available                    <bool> [required]
```

Agrees with our own generated table (`iap2-core/src/spec.rs:609`, `session.rs:82-109`).

#### 4.2 `0x4301 CarPlayStartSession` (accessory → device) — CONFIRMED verbatim

```
0  WiredAttributes    <group> [optional]
   0  IPAddress   <utf8> [1+ required]   "Accessory's link-local IPv6 address for wired interface.
                                          IPv6 address must not include a zone index."
1  WirelessAttributes <group> [optional]
   0  WiFiSSID    <utf8>  [required]
   1  Passphrase  <utf8>  [required]
   2  Channel     <uint8> [required]     primary channel for 80/40 MHz; operating channel for 20 MHz
   3  IPAddress   <utf8>  [1+ required]  link-local IPv6, no zone index
   4  SecurityType<enum>  [required]     0,1,2 = Reserved
                                         3 = WPA3 Personal Transition Mode
                                         4 = WPA3 Personal Only
2  Port                     <uint32> [required]
3  DeviceIdentifier         <utf8>   [required]
4  PublicKey                <utf8>   [required]
5  SourceVersion            <utf8>   [required]
6  CarPlaySDKVersion        <utf8>   [optional]
7  AssetInformation         <group>  [optional]  { AssetIdentifier <utf8>, AssetVersion <int32> }
8  SupportsMutualAuthentication <bool> [optional]
```

**`SecurityType` — do NOT conclude "only 3 and 4 are live."** The first draft said that; it overreaches.
Apple's archives (external *and* internal) publish no WPA2 label, but GM's shipping Cinemo stack sends
**2** for a WPA2-PSK softAP on this exact message:

```java
// reference/gm_cinemo/jadx_GMCarPlay_CT5_AAOS14/…/WifiNetworkManager.java
if (securityType == 1) return (byte) 2;   // Android SECURITY_TYPE_WPA2_PSK → 2 ("Reserved" per Apple)
```
fed straight into `CarPlayStartSession.setSecurityType()`. Whether iOS accepts 2 is unproven either way.
**Do not hardcode a 3-or-4-only check** when the wireless start-session path is wired up. (No live conflict
today: our `0x4301` wireless builder in `iap2-core/src/session.rs` is exercised only by its own unit tests.)

#### 4.3 `0x4E0D WirelessCarPlayUpdate` (device → accessory) — CONFIRMED verbatim

```
0  Status <enum> [required]   0 = Unavailable, 1 = Available
```

The phone revoking or restoring wireless availability mid-life. We do not act on it.

#### 4.4 Messages the first draft missed

> **Correction.** The first draft implied 0x4300/0x4301/0x4E0D were the whole lifecycle set. They are not.

- **`0x5700`–`0x5703` — a whole Wi-Fi credential family**: `RequestWiFiInformation`, `WiFiInformation`
  (`RequestStatus`, SSID, passphrase), `RequestAccessoryWiFiConfigurationInformation`,
  `AccessoryWiFiConfigurationInformation` (SSID / Passphrase / SecurityType / Channel). `0x5703`'s fields
  nearly duplicate `0x4301`'s `WirelessAttributes`. **Note these carry differently-valued `SecurityType`
  enums than 0x4301 — `wifi_handoff.rs` already warns about this; do not conflate the three.**
- **`0x1D00`–`0x1D06` Identification** — `0x1D01 IdentificationInformation` carries the
  `WirelessCarPlayTransportComponent` group with `TransportSupportsCarPlay` / `TransportSupportsThemeAssets`.
  CarPlay wireless capability is declared *here*, one step before `0x4300`.
- **`0x4E03`–`0x4E05` Bluetooth connection updates** and **`0x4E0E DeviceTransportIdentifierNotification`**
  (`BluetoothTransportIdentifier` / `USBTransportIdentifier`) — the correlation data behind `0x4300`'s
  transport-identifier sub-params.

Confirmed unrelated: `0xEA00`/`0xEA01`/`0xEA03` `*ExternalAccessoryProtocolSession` key on
`ExternalAccessoryProtocolIdentifier` — generic MFi app-protocol channels, not the CarPlay session.

---

### 5. The Session Management feature (post-2017)

#### 5.1 R14G17's `stopSession` is NOT this feature

`AirPlayCommon.h:746` defines `kAirPlayCommand_StopSession "stopSession"` with the comment (`:741`):

> *"StopSession: Tells the platform that a session has stopped so it should stop any active
> session-specific operations."*

and `:743-744` "No request keys. / No response keys." Paired with `kAirPlayCommand_StartSession` (`:738`)
and `kAirPlayCommand_SessionDied` (`:730`).

**Caveat found in verification:** `kAirPlayCommand_StopSession` has **zero call sites** in the entire
R14G17 tree — it is defined but dead. The "AirPlay stack → platform" direction is established from its live
sibling `StartSession` (`AirPlayReceiverSession.c:1144` → `AirPlayReceiverPOSIX.c:439`, invoked with
qualifier/params/outParams all `NULL`), not observed for `StopSession` itself. The reading is sound by
analogy; it is not directly witnessed.

(Unrelated same-name trap nearby: the plain C function `AirPlayReceiverSessionScreen_StopSession`.)

#### 5.2 The modern `stopSession`: phone → accessory — CONFIRMED by disassembly

Direction was verified three ways, and all three alternative hypotheses fail:

- `__requestProcessCommand` sits in the same `HTTPStatus _requestProcess*(AirPlayReceiverConnectionRef,
  HTTPMessageRef)` family as the SETUP/TEARDOWN handlers. Disassembly (function at `0x87f0`) shows it
  decoding the **inbound** HTTP body via `CFBinaryPlistV0CreateWithData`, extracting `type` and a params
  sub-dict, and forwarding into `_AirPlayReceiverSessionControl`. We are the server; the phone is the
  client. **Accessory → phone is ruled out.**
- `CARSession` owns `struct OpaqueFigEndpoint *_endpoint` and exposes a generic
  `sendCommand:withParameters:`; `sendStopSessionWithReason:` is a typed wrapper over it.
  `CRCarKitService stopSessionWithSessionIdentifier:reason:reply:` is the XPC hop *before* the wire, not a
  rival destination. **Phone-internal-only is ruled out.**
- `CARSessionObserving` has **no** "received stopSession from vehicle" callback — only a generic
  `sessionDidDisconnect:`. Consistent with the vehicle never sending it.

**Mechanism detail worth having.** The `stopSession` branch lives in what `AirPlayReceiverSessionTearDown`
forwards to `AirPlayReceiverSessionPlatformControl`, and if the params dict lacks the reason key
(`CFDictionaryGetInt64` fails) it returns silently — **no log, no delegate call**. So
`Received stopSession command with reason %u` and the platform notification fire *only* on a reason-bearing
teardown, never on a generic one (network loss, idle timeout, plain RTSP TEARDOWN). That is the mechanism
behind the vehicle distinguishing "Stopped" from "Stopped By Device."

**Residual gap (honest):** the direct `bl` from `_AirPlayReceiverSessionControl` to `TearDown` was not
observed — command-name dispatch appears to go through an indirect table. The middle link is strong
circumstantial fit, not a traced call. See §11.5 for the settling experiment.

Phone side (all three quotes verified exact, line numbers included):

```objc
// CarKit/CARSession.h:108
- (void)sendStopSessionWithReason:(unsigned long long)reason;
// CarKit/CRCarKitService-Protocol.h:57
- (void)stopSessionWithSessionIdentifier:(id)identifier reason:(unsigned long long)reason reply:(id)reply;
// CarKit/CARSessionConfiguration.h:78-80
@property (readonly, nonatomic) unsigned long long supportsStopSession;            // bitmask, NOT a Bool
@property (readonly, nonatomic) _Bool supportsStopSessionDisconnectForThisDrive;
@property (readonly, nonatomic) NSSet *supportedStopSessionDisconnectReasons;
```

`CARPLAY_FEATURE_REFERENCE.md:585` tags the cluster `new-in-27` (citation range exact).

#### 5.3 Where the feature is declared — CORRECTED

> **Correction.** The first draft claimed `/info` returns `sessionManagementInfo` containing a nested
> `stopSessionReasons` array, and that this becomes the phone's
> `supportedStopSessionDisconnectReasons`. **Disassembly contradicts both halves.**

**What `_requestProcessInfo` actually does with `stopSessionReasons`.** The three
`### Supported … %@` log lines fire while parsing the **incoming request body**, doing top-level
`CFDictionaryGetValue` lookups for `altScreenURLs` / `uiContextURLs` / `stopSessionReasons` and caching
copies into the connection struct at `+0x140` / `+0x150` / `+0x168`:

```
82f8: CFDictionaryGetValue(req, "altScreenURLs")      → CFArrayCreateCopy → [x21,#0x140] → log
8420: CFDictionaryGetValue(req, "uiContextURLs")      → CFArrayCreateCopy → [x21,#0x150] → log
8498: CFDictionaryGetValue(req, "stopSessionReasons") → CFArrayCreateCopy → [x21,#0x168] → log
```

Those are exactly the offsets the exported getters `AirPlayReceiverSessionCopyAltScreenURLs` /
`CopyUIContextURLs` / `CopyStopSessionReasons` read back — **accessors for what the phone declared**, for
the accessory app to query. None is called anywhere else, including inside `AirPlayCopyServerInfo`.

**What `sessionManagementInfo` actually is.** In the `/info` **response** it is a bare pass-through:

```
62d0: x2 = "sessionManagementInfo"
62e4: bl _AirPlayReceiverServerPlatformCopyProperty     ; opaque OEM delegate
6300: bl _CFDictionarySetValue                          ; response["sessionManagementInfo"] = whatever it returned
```

CarPlaySDK contains **no code writing `stopSessionReasons` into that dict.** The two are unrelated code
paths that share a name family and sit near each other in the string table.

**Independent cross-framework corroboration.** `AirPlaySender.strings.txt:10888` groups
`altScreenURLs`/`uiContextURLs`/`stopSessionReasons` under `carEndpoint_createInfoRequestFeatureList` (the
phone's *request*-construction side), while `sessionManagementInfo` appears alone in the separate
`carEndpoint_validateInfoResponseKeyPresentForFeature` response cluster. And the phone's
`supportedStopSessionDisconnectReasons` looks to be filled by a dedicated
`APCarPlay_CRFetchStopSessionReasonsList(CFArrayRef *)` RPC — one of a family with
`APCarPlay_CRFetchInstrumentClusterURLs` / `CRFetchDisplayCornerMasks` — not an `/info` dictionary walk.

**What survives.** Two of the three declaration points are unaffected and verified:

1. **SETUP request feature-intersection: `sessionManagement`** — sits between `mainBuffered` and
   `logTransfer` in the key list (§5.4).
2. **SETUP response echo in `enabledFeatures`** — the docs/carplay/04_CAPABILITIES_AND_CONFIG.md rule: the phone reads the accessory's echo,
   not the request. `AirPlayReceiverSessionHasFeatureSessionManagement` is the runtime predicate; the
   Simulator logs `AirPlay supportsSessionManagement - %{bool}d`.

**Unresolved, and it matters (see §11.2):** *which message carries `stopSessionReasons`, in which
direction.* docs/carplay/02_SESSION_LIFECYCLE.md says it lives in the **SETUP** dict; this doc's first draft said the `/info`
**response**; the disassembly says the `/info` **request**, phone → accessory. Our `info.rs:730-741` emits
it in the `/info` response. Since the response value is an opaque OEM pass-through, GM CT5 putting
`stopSessionReasons` inside it (docs/carplay/05_METADATA_AND_CONTROLS.md) is perfectly legal even though the SDK never constructs it — so
our bytes may well be right for the wrong stated reason. This needs a capture, not more prose.

#### 5.4 The SETUP key list and the three new keys — CONFIRMED and strengthened

Verified far past string adjacency: the verifier decoded `LC_DYLD_CHAINED_FIXUPS`, resolved every
CFConstantString, and found **direct code references to all 16 keys, in string-pool order**, inside one
function.

```
_requestProcessSetupPlist(AirPlayReceiverConnectionRef, HTTPMessageRef)   ← __PRETTY_FUNCTION__ string
Setup · macAddress · sessionUUID · SessionSetupRequest Begin
enhancedSiri · altScreen · uiContext · cornerMasks · focusTransfer · h.264Level5.1 · hevc
· mainBuffered · sessionManagement · logTransfer · vehicleStateProtocol
isRemoteControlOnly · sessionWillBeHijacked · sessionCorrelationUUID
SessionSetupRequest End delta ms=%llu · ### Setup session failed: %#m
```

**Practical note for the next person:** `_requestProcessSetupPlist` has **no standalone symbol** in this
binary — `nm` will not find it. The compiler inlined it into the top-level dispatcher
`__HandleHTTPConnectionMessage` (`0x263f68`–`0x264420`). Its `__PRETTY_FUNCTION__` string, the
`bl __HijackConnections` call site and the 16-key sequence all travelled together into that inlined block.
Each of the 16 keys is referenced exactly once in the whole binary, and `/info`'s keys live in a separate
literal pool — so this is a genuine handler key list, not a linker coincidence.

- **`sessionWillBeHijacked` — upgraded from inference to confirmed.** The sequence is literally warn-then-
  hijack: if `AirPlaySessionManagerIsMasterSessionActive`, call `AirPlaySessionManagerCopyMasterSession`,
  invoke a platform-control callback on that *existing* session with `kCFBooleanTrue` keyed by
  `sessionWillBeHijacked`, release, **then** `__HijackConnections`.
- **`isRemoteControlOnly` — the original reading holds, and the suspected RCS conflation does not.**
  Read via `CFDictionaryGetInt64`; if true, execution jumps straight to HTTP 200 and returns, **skipping**
  the master-session check, the hijack notify, `__HijackConnections`, the `sessionCorrelationUUID` read,
  `_AirPlayReceiverSessionCreate`, `_AirPlayReceiverUISessionDelegatePrivSetup` and
  `_AirPlaySessionManagerAddSession`. It is a phase-1, session-establishment-time flag.
  **RCS is a different mechanism**: `AirPlayReceiverSessionCreateRemoteControlSessionOnSender` /
  `_CreateAndRegisterRemoteControlSession` are called only from `_AirPlayReceiverSessionSetup` (phase 2,
  requiring an existing session), and CarKit's mirror is per-channel on `CARSessionChannel`. iOS 27's
  shared `airplayReqProcessor_requestProcessSetupPlist` settles the semantics: `isRemoteControlOnly` sits
  with `isSharedConnection` / `isHomeTheaterSession` / `isPersistentConnection` / `isNonMediaSession`, under
  guards *"isPC and isRemoteControlOnly can't both be true at the same time"* and *"RC-only server doesn't
  allow non-RC sessions"*, with `"Hijacking active connection and becoming main session, as persistent
  session became media session"` and `"New stream setup request on existing RC session [%{ptr}]
  (sessionType=%d)"`. **RC-only is a named session type that a later SETUP can promote to a media
  session** — genuinely a holding pattern, and closer to our RECOVERING than anything we have.
- **`sessionCorrelationUUID` — corroborated, not airtight.** Read via `CFDictionaryGetValue` immediately
  after the hijack call; where the value is *compared* was not traced. "Correlates a new session to a
  prior one" is the best-supported reading. **It is live on the wire**: a Dec-2025 capture (iOS 26.3) at
  `local_carplay_sdk/conformance/captures/phone_incoming/adapter_tty_025839_29DEC25.log:874-876` shows the
  phone sending `sessionCorrelationUUID`, `sessionUUID` and `timingPort` in one SETUP dict.
- **New: `hijackID`.** The sender-side `apsession_appendControlSetupRequest` key list groups
  `sessionCorrelationUUID` / `macAddress` / `sessionUUID` / `isRemoteControlOnly` and, further down, a
  distinct `hijackID`. Unexamined — see §11.4.

#### 5.5 Teardown completion — CORRECTED

> **Correction.** The first draft listed `sessionsStopped` / `teardownSession` / `teardownCompleted` as
> `AirPlayReceiverSessionPlatformControl` verbs and concluded there was an asynchronous *accessory↔SDK*
> teardown handshake we were missing. **Disassembly refutes the attribution.**

`AirPlayReceiverSessionPlatformControl` (0x4a78–0x4f84) compares `inCommand` against exactly **six** verbs,
in code order:

```
duckAudio · unduckAudio · startSession · stopSession · performHapticFeedback · deviceOfferFocus
```

That is the whole table. The first draft's "in order" list straddled a function boundary —
`_HandleReceiverUIEvent`'s own name-string sits in the dump between the two groups.

`sessionsStopped` / `teardownSession` / `teardownCompleted` belong to **`_HandleReceiverUIEvent(const char
*, CFDictionaryRef, void *)`**, a separate `strcmp`-over-`const char*` dispatcher for the receiver's
embedded UI/WebApp bridge (siblings: `userPlayPause`, `secureStop`, `subscribe`, `sessionTorndown`,
`putAppInBackground`, `displayInfo`, `notifyBackButtonIgnored`), reached from
`_AirPlayReceiverUI_AirPlaySessionEstablished_Internal` / `_AirPlaySessionTornDown_Internal` /
`_HandleSessionTornDownFromApp`.

The async completion is real — that dispatcher's `teardownCompleted` branch does
`bl _AirPlayReceiverServerSessionTeardownCompleted` — but it is **receiver-UI → native server**, not
platform → SDK. The clincher is a negative: the Simulator, whose whole job is to *be* the platform,
references `startSession` and the duck/unduck callbacks but contains **zero** occurrences of
`teardownCompleted`, `sessionsStopped`, `teardownSession` or `DeselectAllSessions`. If the accessory were
expected to call back with them, it would have to know their names.

**Consequence:** "2026 added an async teardown-completion mechanism absent from 2017" survives narrowly
(both symbols exported; zero R14G17 hits; R14G17's `AirPlayReceiverSessionTearDown` is genuinely
synchronous, all work inline with completion via an `outDone` out-param). **But there is no accessory-side
handshake for us to implement** — that gap row is withdrawn.

---

### 6. Teardown triggers

From `AppleCarPlay_CommunicationPlugIn_IntegrationGuide.txt:211-230`, verified verbatim.

**Network-stack triggers.** The plug-in watches the routing socket for `RTM_NEWLINK`, `RTM_DELLINK`,
`RTM_NEWADDR`, `RTM_DELADDR`, `RTM_IFINFO`, `RTM_CHANGE` — **"(if defined on a platform)"**, a qualifier
the first draft dropped — and tears the session down when:

- the NCM interface is torn down (can't get `SIOCGIFEFLAGS`)
- the interface is not running (no `IFF_RUNNING`)
- the interface status becomes inactive

**Inactivity triggers** (guide wording: *"No data received for N seconds"*):

| Condition | Guide's figure |
|---|---|
| audio or video stream active, platform supports `TCP_KEEPCNT` | 9 s |
| audio or video stream active, no `TCP_KEEPCNT` | 30 s |
| no audio or video stream active | 30 s |

#### 6.1 The 9 s figure is Apple's own undercount — it is really ~12 s

> **Correction.** The first draft wrote "the 9 s figure is literally 3 s × 3 probes." That repeats Apple's
> arithmetic uncritically, and the arithmetic is wrong.

`AirPlayReceiverSession.c:1726` calls `SocketSetKeepAlive(sock, kAirPlayDataTimeoutSecs / 10, 3)` with the
comment `//9 sec`. But `SocketSetKeepAlive` (`NetUtils.c:2942-2991`) applies `inIdleSecs` to **both**
`TCP_KEEPIDLE` (wait before the *first* probe) **and** `TCP_KEEPINTVL` (spacing between probes), with
`TCP_KEEPCNT = inMaxUnansweredProbes`. So:

```
detect ≈ KEEPIDLE + KEEPCNT × KEEPINTVL = 3 + 3×3 = 12 s,  not 3 × 3 = 9 s
```

Apple's comment omits the initial idle wait. **Our own implementer independently computed the same ~12 s**
— `ccpa/airplayd/src/main.rs:409-411` documents "detected in ~12 s" while citing Apple's
`SocketSetKeepAlive(sock, kAirPlayDataTimeoutSecs/10, 3)`. Our code is right; the guide's 9 is the outlier.

**`kAirPlayDataTimeoutSecs = 30`** (`AirPlayCommon.h:101`) → `server->timeoutDataSecs`
(`AirPlayReceiverServer.c:932`, the only assignment; structurally overridable, never overridden in this
tree) → `ats->maxIdleTicks` (`AirPlayReceiverSession.c:457`) → the idle check in `_PerformPeriodTasks`
(`:1498-1528`) → `kAirPlayCommand_SessionDied`.

#### 6.2 The idle-keepalive exemption — CONFIRMED

`_UsingIdleStateKeepAlive(ME)` = `IsValidSocket((ME)->keepAliveSock)` (`:247`). The kill condition
(`:1520-1522`) is `idleExpired && ( !keepalive || ( keepalive && sessionStarted && usingScreenOrAudio ) )`.
Truth table: no beacon → always kill on expiry; beacon present → kill **only** if started *and* using
screen or audio, otherwise suppressed. A session parked with the beacon running is legitimately allowed to
sit idle. (The verifier went in expecting an inverted paraphrase; the logic holds.)

The beacon is **received**, not sent, by the accessory: `ServerSocketOpen(… SOCK_DGRAM …)` at `:3617`,
`SocketRecvFrom` at `:3786`, thread named `AirPlayKeepAliveReceiver`. The port is negotiated in
`_ControlSetup` (guarded by `if( !me->controlSetup )`) **iff** the request set `keepAliveLowPower`
(`:1773-1780`).

#### 6.3 What the 2026 binary does and does not prove

CarPlaySDK retains `### Interface %s is not running; killing session.` and `### Interface %s is inactive;
killing session.` verbatim, so those code paths persist. **But it does not prove the 9/30 s constants are
unchanged** — they appear only as `%d` format arguments, and `TCP_KEEPCNT` is a socket option that is never
printed. The first draft's "so this is unchanged in 2026" overstated what string evidence can carry.

One divergence worth noting: R14G17 logs `Idle timeout after %d seconds with no audio`; 2026 logs
`Idle timeout after %d seconds (audio=%d video=%d)`.

---

### 7. Session ownership and hijacking

A receiver serves **one** session, and a new SETUP preempts rather than queueing or rejecting.
`_HijackConnections( inCnx->server, inCnx->httpCnx )` runs at `AirPlayReceiverServer.c:3200` — the first
*action on server state*, though **not literally the first statement** (an `aprs_ulog(…, "Setup\n")` and
the local declarations precede it; the first draft said "first statement").

`_HijackHTTPServerConnections` (`:1983-2005`) walks every connection that is not the hijacker and passes
`_IsConnectionActive`, logs `*** Hijacking connection %##a for %##a` (`:1995`), unlinks it and calls
`_DestroyConnection` → `HTTPConnectionStop` + `CFRelease`. A hard close, not a soft mark.

Two details: `_IsConnectionActive` (`:1967-1974`) actually tests `aprsCnx->didAnnounce`, not "started
audio" as Apple's own comment above it claims. And **no reject path exists** — every `kHTTPStatus_` return
in the SETUP handler is `BadRequest` / `InternalServerError` / `KeyManagementError`, i.e. parse or crypto
failures; the `NotEnoughBandwidth` occurrences in the file belong to RECORD (`:3135`) and HomeKit pair-setup
backoff (`:2920`), neither a second-SETUP rejection.

2026 retains both the log string and the `_HijackHTTPServerConnections` signature, and adds the
advance-notice callback described in §5.4.

#### 7.1 `sessionUUID`

16-byte SETUP key (`kAirPlayKey_SessionUUID`, `AirPlayCommon.h:1124`). `AirPlayReceiverServer.c:3215-3219`
reads it, and **only the first 8 bytes**, big-endian, into `clientSessionID` via `ReadBig64`
(`CommonServices.h:1905-1914`); the other 8 are discarded. A wrong *length* is a 400; an **absent** key is
tolerated silently, because the length check sits inside `if( !err )`.

---

### 8. Control-plane objects created per session

> **Correction.** The first draft ordered this table Event → Timing → keepAlive from string layout. The
> real creation order is **Timing → keepAlive → Event**, in both eras.

`_ControlSetup` (`AirPlayReceiverSession.c:1754-1800`) does, in order:

| # | Object | Port key | Notes |
|---|---|---|---|
| 1 | Timing | `timingPort` | `AirTunesClock_Create` + `_TimingInitialize`; failure budget `Too many time negotiate failures: G=%d B=%d R=%d T=%d` |
| 2 | Idle keepAlive | `keepAlivePort` | conditional on `keepAliveLowPower`; UDP beacon, `AirPlayKeepAliveReceiver` |
| 3 | Event channel | `eventPort` | `Events-Salt`, `Events-Read-Encryption-Key`, `Events-Write-Encryption-Key` |
| — | DataStream | `dataPort` | `DataStream-Salt`, `DataStream-Output-Encryption-Key`, `DataStream-Input-Encryption-Key` |
| — | Screen streams | per-stream | `ScreenStreamSetup Type: %s UUID: %@` |

Confirmed still Timing-first in 2026: disassembling `_AirPlayReceiverSessionSetup` shows
`_AirTunesClock_Create` + `__TimingInitialize` at ~+0x2a8, with the keepAlive/event `_ServerSocketOpenEx3`
calls ~0x470 and ~0x6d0 later.

**DataStream is not itself post-2017** — R14G17 has `DataStream-Salt` / `dataPort` / `_GetStreamSecurityKeys`
(`AirPlayCommon.h:296-301`, used at `AirPlayReceiverSession.c:2135, 3383, 4368`). What is post-2017 is
specifically **stream type 130**, the RCS use, per docs/carplay/05_METADATA_AND_CONTROLS.md.

**Phase instrumentation IS new in 2026** — zero R14G17 hits for the Begin/End timing blocks. Note the
wording is not uniform: `InitialConnection` and `Authorize` use **Start/End**; `AuthSetup`,
`SessionSetupRequest` and `InfoRequest` use **Begin/End**. All carry `delta ms=%llu`.

The session-start summary, quoted in full this time (the first draft dropped `%s%?u%s` — a *conditional*
`Scr=<n> ms` field present only when a screen stream is active — and the trailing status):

```
AirPlay session started: From=%s D=0x%012llx A=%##a T=%s C=%s
  L=%u ms Bonjour=%u ms Conn=%u ms Auth=%u ms Ann=%u ms Setup=%u ms %s%?u%sRec=%u ms: %#m
AirPlay session ended: Dur=%u seconds Reason=%#m
```

Both byte-identical to R14G17's `_LogStarted` / `_LogEnded` (`:4767-4775`, `:4803`) — unchanged since 2017.
Transport tags: `Enet` / `WiFi` / `AWDL` / `Direct` / `BTLE`.

**Verb set.** CarPlaySDK's `Public:` header is `ANNOUNCE, SETUP, RECORD, PAUSE, FLUSH, TEARDOWN, OPTIONS,
POST, GET, PUT, GET_PARAMETER`. The first draft framed `SET_PARAMETER`'s absence as a 2026 change; that is
backwards. R14G17's equivalent (`AirPlayReceiverServer.c:2879`) also lacks `SET_PARAMETER` **and lacks
`GET_PARAMETER` too** — so the real delta is that **2026 added `GET_PARAMETER`**, while `SET_PARAMETER` was
never advertised in either era (it does exist in 2026's method-dispatch table, just unadvertised). Our
`server.rs:309` advertises both, a superset of Apple's.

**Endpoints**: `/info`, `/command`, `/feedback`, `/logs`, `/diag-info`, `/metrics` — **plus**
`/pair-setup`, `/pair-verify`, `/auth-setup`, which the first draft omitted.

---

### 9. The phone-side lifecycle — the counterpart this doc originally missed

`CARSession` is not where the phone's lifecycle lives. Two sibling headers are:

**`CarKit/CARSessionObserving-Protocol.h`** — the connect/disconnect callback surface:

```objc
- (void)sessionDidConnect:(id)connect;
- (void)sessionDidDisconnect:(id)disconnect;
- (void)session:(id)session didUpdateConfiguration:(id)configuration;
- (void)startedConnectionAttemptOnTransport:(unsigned long long)transport;
- (void)cancelledConnectionAttemptOnTransport:(unsigned long long)transport;
```

**`CarKit/CARSessionStatus.h`** — the state machine, including the phone's **own connection timeout**:

```objc
@property (retain, nonatomic) NSObject<OS_dispatch_source> *connectingTimer;
@property (nonatomic) unsigned long long timeoutInterval;
- (void)_sessionUpdatesQueue_startConnectingTimer / _stopConnectingTimer;
- (void)_sessionUpdatesQueue_handleConnectingTimeout;
- (void)_handleAuthenticationSucceeded:(id)succeeded;
- (void)_handleEndpointActivated:(id)activated;
- (void)_sessionUpdatesQueue_notifyDidConnectSession: / notifyDidDisconnectSession:
- (void)waitForSessionInitialization;
```

The `authenticated` → `activated` → `_sessionReady` progression on `CARSession` (`:23`, `:26`, `:82`) is the
phone-side mirror of §3's vehicle vocabulary, and `connectingTimer`/`timeoutInterval` is the phone's
counterpart to the Simulator's 30 s connection watchdog. Also on `CARSession`: `isPaired`,
`sessionStatusOptions`, `configuration`, and the designated initializer
`initWithFigEndpoint:sessionStatusOptions:nightModeProvider:`.

Do not confuse `_sessionUpdatesQueue_handleStopUIWithParameters:` (`:92`) with session stop — it is UI stop.

---

### 10. Gap analysis against our implementation

| Item | Status |
|---|---|
| `sessionManagementInfo` + `stopSessionReasons` in `/info` response | Present, gated on `CARPLAY_SESSION_MGMT` (`info.rs:730-741`), reasons `[0,1,2,3,4]`. **But see §5.3 — the message and direction are now in doubt.** |
| `sessionManagement` echo in SETUP `enabledFeatures` | Present (`session.rs:665-669`), verified to be the **same literal lever** `CARPLAY_SESSION_MGMT`, a spawn-scoped var (`levers.rs:30`) set by `wireless/av.rs:354` |
| Inbound `POST /command type="stopSession"` handler | **MISSING — the real gap.** Verified exhaustively across `crates/`, `ccpa/`, ocbmd, airplayd and the macOS app. `session.rs:1541`'s `fn command` branches only on `modesChanged` plus the iAP tunnel; everything else falls through to logging and `empty_plist_dict()`. `relay.rs:730` is a pure pass-through. The host's `MetadataWindow.swift:655` categorises `stopSession` as `.sessionLifecycle` **for display only**. |
| `disconnectReason` plumbing to the supervisor | Missing (follows from the above) |
| ~~`teardownSession`/`teardownCompleted` async handshake~~ | **Row withdrawn** — §5.5: no accessory-side handshake exists |
| `isRemoteControlOnly` SETUP key | Not handled. Now the most interesting item: it is a real, promotable session type (§5.4), i.e. a *protocol-native* holding pattern where ours is improvised |
| `sessionUUID` | **Not handled.** The first draft claimed `session.rs:248` handles it — that line is a **comment** citing Apple's source while documenting `name`/`deviceID`; `publish_phone_identity` never reads it, and repo-wide that comment is the only occurrence of the string |
| `sessionWillBeHijacked` / `sessionCorrelationUUID` / `hijackID` | Not read. `sessionCorrelationUUID` is confirmed live on the wire (§5.4) |
| Connection hijack on SETUP | Not implemented as such; `events.rs:295-303` handles re-SETUP-without-TEARDOWN defensively (`iap_tunnel::reset()`), covering the same hazard from the other side |
| `WirelessCarPlayUpdate` (0x4E0D), `0x5700`–`0x5703` | Not acted on |
| Idle timeouts | **Better than the first draft said.** `AV_IDLE_TEARDOWN_MS = 30_000` (`net.rs:15`) is a *stated* decision — `net.rs:14` reads "Mirrors the C's `kAirPlayDataTimeoutSecs` (30 s)" — not a coincidence. And we **do** implement the fast path: `arm_keepalive` (`airplayd/src/main.rs:409-440`) is armed unconditionally on every control connection at 3/3/3 ≈ 12 s. What we do not model is Apple's *gating* of that fast path on stream-active state. |

**The single highest-value fix remains the `stopSession` handler.** We declare a five-value reason
vocabulary and then ignore the command. Both sides are behind `CARPLAY_SESSION_MGMT`, so nothing is live
today — but shipping the declaration without the handler would be worse than not declaring it.

---

### 11. Open questions

Each needs one hardware experiment, not more static analysis. Tracked in `../ops/04_OPEN_ITEMS.md`.

1. Which `stopSession` reason integer means "disconnect for this drive". The enum is structurally
   unrecoverable from the iOS extract (runtime-metadata headers erase C enums), so it must come from
   a capture. Our `[0,1,2,3,4]` is GM's shape, not a semantic claim.
2. Which message carries `stopSessionReasons`, and in which direction. One `/info` + SETUP capture
   settles it and either validates or retires the current emission.
3. Whether `supportsStopSession` is a bitmask over reason codes or a count — declared
   `unsigned long long` while the reasons arrive as an `NSSet`.
4. What sender-side `hijackID` is, relative to `sessionWillBeHijacked`/`sessionCorrelationUUID`.
5. Whether `isRemoteControlOnly` is a cheaper holding pattern than tearing down A/V.
6. Whether servicing the keepalive beacon defers the phone's own idle kill. We negotiate
   `keepAlivePort` and have never measured it.

Method for 1, 2 and 4: ask the phone. `idevicesyslog -u <udid> -p carkitd` during a phone-initiated
disconnect, correlated against the accessory's `Received stopSession command with reason %u`, proves
direction, reason value and correlation end-to-end.

### 12. How this document went wrong the first time

The first draft asserted several handlers existed on the strength of a nearby comment or a symbol
name rather than a call site — `sessionUUID` being the clearest case, where the cited line was a
comment. Verify a claim against the code path that executes, not against text that mentions it. The
per-claim record is in `../ops/06_CORRECTIONS_LEDGER.md`.

## Lifecycle — states and transitions

<!-- absorbed: ../carplay/02_SESSION_LIFECYCLE.md -->

**Status: DESIGN (2026-07-09).** Captures the committed model for how a CarPlay session starts, survives
transport hiccups, and tears down — driven by the presence of a live host app (the receiver). Grounded in
on-hardware probes from this session (see [Empirical basis](#empirical-basis)). Phase 2 video
forward-encrypted is **validated**; the lifecycle below is the next build (task #9).

---

### Governing principle: CarPlay is a live-state UI stream (think VNC)

CarPlay is not a media file being played; it is the head unit's screen **right now**. Navigation is where
you *are*, not where you *were*. Any software queue, replay buffer, or accumulation on the A/V path is a
correctness failure — it means a stale frame or a stale map position is waiting to be shown. Therefore:

- **The A/V path carries no accumulating software buffer — it applies backpressure instead of dropping.**
  ocbmd carries video and cluster-video on their own queues (`out_video`, `out_alt_video`,
  `main.rs`, the `out_video`/`out_alt_video` fields) and pulls a stream's next seam chunk only once *that* stream's queue has drained —
  the poll loop simply withholds the seam fd (`main.rs`, the `let gated = match *ch` read gate). A slow USB/host therefore stops
  ocbmd reading `:9001`/`:9005`, which blocks airplayd's screen thread, which stops it reading the
  iPhone's screen socket, so **TCP flow control reaches the phone**. This is Apple's own model
  (docs/carplay/06_AV_PIPELINE.md: "Apple flow-controls") and it avoids dropping P-frames, which poisons the decoder until the
  next IDR. *The further step — that iOS responds by lowering its encode rate — is the design
  expectation and is **not measured anywhere in this repo**; the per-lane `[screen] acct lane=… recv=/s
  fwd=/s` counters (`session.rs`) are the instrument that would confirm it.*
- **The bound is still one frame in flight, now by gating rather than dropping.** Because a seam fd is
  polled only when its stream's queue is empty, each video queue holds at most one ≤64 KiB OCBM frame
  (`MAX_PAYLOAD`). `OUT_QUEUE_CAP` (1 MiB, **per queue**, `main.rs`, `const OUT_QUEUE_CAP`) is an OOM backstop, not a drop
  policy: on the video lanes it is unreachable by construction.
- **Audio is the exception — no gate, and no backpressure is possible.** `CH_MEDIA_AUDIO` and
  `CH_ALT_AUDIO` share one ungated queue (`out_audio`, `main.rs`, the `out_audio` field; the gate's `_ => false` arm at
  `main.rs`, the `let gated = match *ch` arm), because their source is RTP over **UDP** — there is no transport flow control to
  propagate to the phone. A long host stall grows that queue to `OUT_QUEUE_CAP` and then drops whole
  frames, counting `av_dropped` and logging `[ocbmd] live-A/V queue cap hit on ch 0x… — host wedged?`
  (`main.rs`, the `live-A/V queue cap hit` arm). Audio is low-rate so this is rare, but it is a real drop path, not a
  pathological one — in practice `av_dropped` is an audio counter.
- **Backpressure propagation is bounded at ~2 s.** airplayd sets a 2 s write timeout on each seam socket
  ("audit R3: never block the screen thread on a stalled sink"). Past that the seam write fails, airplayd
  tears the seam down and drops frames, and the reconnect requests a fresh keyframe. So under a
  *sustained* stall the drop happens in airplayd, not ocbmd.
- **Buffers outside ocbmd exist and matter.** The loopback seams and the USB gadget FIFO all buffer — see
  prerequisite #1 below ("drained 28 stale frame(s) before HELLO_ACK"). ocbmd clears its A/V queues on
  `CT_HELLO` only; `go_idle()` does not.
- **Scope: this is the BOX's egress policy.** Control traffic (pairing, the session-key handoff, the YAML
  config push) MUST be reliable — you cannot drop a byte of a key — and rides `out_hi`
  (`CH_CTRL`/`CH_MFI`/`CH_RTSP`); `CH_CONSOLE` has its own `out_console` drained *below* A/V so a console
  flood cannot freeze video; reliable bulk is `out_lo`, whose cap-clear resyncs to the next `F_SOM` so the
  peer never sees a truncated message (#567). The live-UI principle above is unchanged and still governs
  the **host** renderer, where a stalled consumer still DROPS rather than buffers
  (`VideoDecoder.swift` — "drop on backpressure, never buffer"); buffering *there* really would show a
  stale frame. Refined 2026-09-03: the mechanism is no longer a single latest-wins slot but a depth-3
  bounded FIFO that never blocks its producer and picks the CHEAPEST frame to lose (see
  [../host/00_MACOS_HOST_APP.md](../host/00_MACOS_HOST_APP.md) "Host decode pipeline"). The principle is
  unchanged — a two-frame cushion is not "buffering", it is the difference between dropping a stale P
  and dropping the IDR the next two seconds of video depend on.

> **⚠️ REVERSAL — the principle above supersedes both the earlier "bounded queue with drop-oldest"
> idea AND this document's own original "drop the whole frame on `EAGAIN`" mechanism (2026-07-09,
> prerequisite item 3 below).** Backpressure-not-drop is what shipped; bounded per-stream queues are
> deliberate. Full reasoning: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-08-1`.

---

### Failure taxonomy — set by the power topology

The host USB cable is also the adapter's **main 5V rail**. That single fact splits every host-side failure
into two cases, and the box can only ever observe one of them:

| Case | What happens to the box | Handling |
|---|---|---|
| **VBUS lost** — cable pull, Mac powered off, port power cut | Adapter loses power → **off**. Not running. | Nothing to detect or recover. On power return the box cold-boots and re-inits from scratch. Self-cleaning. |
| **VBUS present, data disrupted** — jitter, latency spike, USB bus reset, brief host re-enumeration, host-app crash/relaunch | 5V holds → **box stays powered and running**; only the OCBM data path hiccups. | The case this document engineers for: hold, attempt recovery, time out. |

**Elegant consequence:** if the box is alive to observe a host dropout *at all*, then by construction VBUS
is still present — it is the powered, data-only case every time. The box physically cannot witness the
catastrophic case (it is unpowered during it). So the box's correct default on **any** observable host
dropout is always *"hold and attempt recovery,"* never *"assume permanently gone."* The only reason it ever
gives up is to distinguish a transient hiccup from a Mac that is powered-but-idle (app quit for good, or a
sleep state that keeps VBUS alive) — and that is exactly what the grace timeout resolves.

---

### Host-presence detection — drain health, not a heartbeat

An app-level keepalive is a *proxy* for liveness. The box has a more **direct** signal, and on-hardware measurement proved
which signals are real:

1. **Primary — backpressure / no-drain (box-visible, immediate).** The box is the USB *device*; the host
   app issues the IN tokens that drain the link. When the app is consuming, writes complete; when it
   crashes, quits, stalls, or simply cannot keep up, the IN tokens stop and writes back up. This one
   measurement covers app-death, app-stall, and slow-app at once — and it measures the exact thing you care
   about ("is my A/V being consumed?"), which a heartbeat only proxies.
2. **Coarse but instant — USB enumeration loss.** A cable pull / Mac sleep / shutdown drops VBUS and
   de-configures the gadget (or powers the box off). Hard event, seen immediately. (In practice this
   overlaps the "VBUS lost" row above.)
3. **Disambiguator — the app heartbeat, NOT the primary trigger.** Its only unique job is telling
   "app alive but momentarily not draining (paused/backgrounded)" apart from "app dead," and catching a
   wedged app that still holds the interface. Because backpressure already handles the fast,
   resource-critical cases, the keepalive interval can be **relaxed** (a few seconds), not tight.

**Detection as built has exactly two detectors — and neither is the pair originally designed here:** the
heartbeat watchdog (`HEARTBEAT_GRACE`) and an explicit `CT_STOP` (immediate, no grace). Sustained
backpressure does **not** feed a grace clock — `out_video`/`out_alt_video` backpressure their producers and
`out_lo` clears at `OUT_QUEUE_CAP` with no effect on presence. An accessory-fd error or hangup is not a
grace path either: ocbmd `exit(1)`s immediately (`main.rs`, the POLLHUP/POLLERR and read-EOF `exit(1)` arms) —
but **before exiting it now clears the same three files `main()` clears at startup** (`clear_session_state_for_exit`:
`/tmp/host_present` → 0, the ephemeral `/tmp/carplay_cfg.yaml`, the `/tmp/radio_off` inhibit; corrected
2026-09-03). It used to leave `/tmp/host_present` at its last value, so a supervisor on a box WITHOUT the
inittab respawn believed a host was present forever. No `SEV_HOST_GONE` accompanies it: the accessory fd
that would carry it is exactly what just died.

> **Ruled out:** `/sys/class/udc/*/state` is **not** an app-presence signal. It reflects cable enumeration,
> not host-app liveness — see [Empirical basis](#empirical-basis).

---

### State machine

```
                    host SUBSCRIBE (+ YAML config)
   ┌─────────┐      ─────────────────────────────►   ┌────────┐   iPhone connects   ┌───────────┐
   │  IDLE   │                                        │ ARMED  │ ───────────────────►│ STREAMING │
   │(holding │ ◄─────────────────────────────────    │(adver- │                     │  A/V →    │
   │pattern) │      grace expires → TEARDOWN          │ tise)  │                     │  host     │
   └─────────┘                                        └────────┘                     └─────┬─────┘
        ▲                                                                                  │ host link drops
        │ grace expires (10s beat / 5s clean-STOP): TEARDOWN                               │ (heartbeat loss)
        │                                                                                  ▼
        │                                                                          ┌──────────────┐
        └──────────────────────────────────────────────────────────────────────  │  RECOVERING  │
                        host returns within grace → resume STREAMING               │ (10s grace)  │
                                                                                   └──────────────┘
```

- **IDLE / holding pattern.** iAP2 link **held**, MFi + pairing state resident, but AirPlay is **not
  advertised** and SETUP-phase2/RECORD is refused → **no A/V produced**. The iPhone sees a connected head
  unit (not a cold disconnect); CarPlay simply isn't running. This is the correct no-host resting state —
  cheap, and it avoids the expensive re-enumeration/re-pair on the next host arrival.
- **ARMED.** A host app has SUBSCRIBEd and pushed its config; `rx_connect` advertises `_airplay._tcp`;
  airplayd permits the full session.
- **STREAMING.** iPhone connected, A/V forwarded to the host (encrypted; see `docs/carplay/00_ARCHITECTURE.md`/HANDOFF).
- **RECOVERING (conceptual; 10 s grace).** *A model, not a coded state* — ocbmd holds no state enum, only
  `subscribed`/`last_hb`/`present` plus the `stop_grace_deadline`/`rearm_deadline` timers
  (`main.rs`, the `subscribed`/`last_hb`/`present` + deadline fields), and `session_supervisor.sh`'s own `RECOVERING` phase (`:977`) means something
  different: the L1/L2 escalation ladder has fired. Entered here on host link drop:
  - **iAP2 link held** — do NOT touch the iPhone accessory link; that is what avoids re-enum/re-pair.
  - **A/V backpressured, not accumulated** — at most one frame in flight per video lane; a hiccup still
    cannot become an OOM.
  - **airplayd keeps the iPhone session warm** — it keeps answering the phone's RTSP `/feedback`/keepalives
    independent of host drain, so the phone does not time its own session out. The phone sees a brief
    freeze, not a disconnect.
  - the **host** retries the OCBM link (it retransmits `CT_HELLO` until ACKed, `OCBMClient.swift:105`); the
    box only answers with `CT_HELLO_ACK` and flushes its stale output queues (`main.rs`, the `CT_HELLO` arm).
  - host returns within grace → **resume STREAMING** (no relaunch on the phone).
  - grace expires → **TEARDOWN**.
- **TEARDOWN → holding pattern.** *(CORRECTED 2026-08-16 — there is no clean RTSP TEARDOWN today.* `kill_session()` in `tools/session_supervisor.sh` `pkill`s airplayd and rx-connect, then `pkill -9` after 1 s; airplayd installs no SIGTERM handler, so nothing emits an RTSP TEARDOWN on the presence edge. Making it graceful is the "optional future refinement" recorded later in this document.)* The A/V session is killed, then the box drops back to IDLE
  (accessory still up, head unit still "present"). NOT a cold disconnect. Host reappears later →
  re-advertise → CarPlay relaunches from the warm accessory state.

There is now **one grace**, in `ccpa/ocbmd/src/main.rs`: **`HEARTBEAT_GRACE = 10 s`** covers a host that
stops beating — crash, wedge, App Nap, USB stall. A *clean* `CT_STOP` gets none (changed 2026-09-03):
a host that closes is telling the box the session is over, so it tears down at once via the same
`go_idle` routine. The `STOP_GRACE = 5 s` warm-reuse hold this section used to describe is **gone** —
it left a projection running with no host and the phone attached to nobody for 5 s after every quit,
and the relaunch it optimised for still had to re-`SUBSCRIBE` anyway. **`REARM_HOLD = 2 s`** is not a
grace at all: it is how
long `/tmp/host_present` must read 0 for the 1 Hz supervisor to see a re-arm edge. It now does double
duty — `Daemon::raise_presence` holds a `CT_SUBSCRIBE`'s flag raise until the *preceding* GONE edge is
`REARM_HOLD` old, because the actor samples that flag at 1 Hz and acts on edges: a scripted
quit→relaunch would otherwise write 0 then 1 between two samples, the actor would read 1 → 1, and the
teardown `CT_STOP` just performed would be invisible to it (leaving the new host subscribed against
the dead session's airplayd). Only the flag waits; `present`, `SEV_HOST_PRESENT` and `HELLO_ACK` are
all immediate. Detection granularity for both is the 500 ms bounded poll. The `~5 s` this section originally quoted was a notional
design starting value that matched no constant; `HEARTBEAT_GRACE` was itself widened 3 s → 10 s by audit
QC #428, because expiry is maximally destructive and a 1 Hz host can miss several beats without being dead.

---

### Session semantics: resume vs. new session

- **Link-level reconnect within grace** (same app instance, data path blipped) → **resume**. Nothing
  re-negotiates.
- **App exit + relaunch** (new process) → a fresh **SUBSCRIBE** → **new session**. A fresh SUBSCRIBE always
  means "tear down whatever session exists and start clean with the config I am about to push." The app does
  not have to guess whether the box holds stale state — announcing itself *is* the teardown trigger.

---

### Persistence: pairing persists, config never does

Two different kinds of state were being conflated; separated, both rules hold cleanly:

- **Session config (the YAML)** — screen geometry, audio formats, feature flags, VehicleConfig-like
  parameters — is app-authoritative and **ephemeral, always**. The app pushes it per session; the box holds
  it only for that session's life; it **never persists**.
- **Pairing / identity records** — the iPhone's long-term Ed25519 pairing key — **persist on the box**, per
  known device. This is dictated by the AirPlay/CarPlay model itself: pair-setup (the expensive SRP
  exchange) runs **once** per device; every later connection is a pair-verify (Curve25519 ECDH) against the
  stored record. This is the "streamlined known-device" path, and it is what the original RiddleBox firmware
  kept in its `/etc/` data folders.

**The fast known-device reconnect comes entirely from persistent pairing, not persistent config.** A known
device skips the crypto handshake cost, then the app re-pushes its (cheap) config fresh. Config persistence
buys almost nothing and risks serving stale UI parameters; pairing persistence buys the whole streamlined
reconnect. Pairing lives **on the box** (the box owns the stable crypto foundation; the app only ever
receives the derived session key) — exactly like RiddleBox.

---

### The four mechanisms this lifecycle rests on

All four are implemented and hardware-validated. Stated here as current behaviour; the dated
build-and-validate narrative was cut 2026-08-31.

**1. Host-link reattach and resync.** A `CT_HELLO` resets the session box-side — clears
`out_hi`/`out_video`/`out_alt_video`/`out_audio`/`out_lo` and zeroes `seq` before replying HELLO_ACK
(`out_console` is deliberately not cleared) — so a new host is never served a prior session's frames.
Host-side, `do_hello` drains frames until the ACK and re-sends HELLO once mid-wait, because
kernel-FIFO-buffered stale frames cannot be retracted by the box. host→box self-resyncs on magic in
the reassembler. This is also the resync path RECOVERING uses.

**2. Disk-backed PeerStore.** `MemPeers` → `DiskPeers`: an in-memory map persisted as
`[u8 id_len][id][32-byte LTPK]` records, atomic `.tmp`+rename, path from `PEERSTORE_PATH` (default
`/etc/carplay_peers.bin`), degrading to in-memory with a warning if the path is not writable. `/etc`
persists on this rootfs; a known device reconnects with pair-verify only, no pair-setup. Only pairing
persists — the session YAML is always ephemeral.

**3. Backpressure, not drop, on the live A/V path.** The original design dropped whole frames on
backpressure; task #33 replaced it because dropping P-frames poisons the decoder until the next IDR.
Today `send()` tries a zero-copy `writev` fast path, then queues; the cap is `OUT_QUEUE_CAP` (1 MiB)
per stream, and the queues are split per stream (`out_video`/`out_alt_video`/`out_audio`) so the
low-rate cluster lane cannot gate the main 4K seam. A slow host yields backpressure and a lower source
framerate, not drops. Memory stays flat (~488 kB RSS with no host reader, against an 18.7 MB ratchet
before). **Open:** `av_dropped`/`lo_dropped` are stderr-only; `MGMT_GET_INFO` does not report them.

**4. CH_CTRL session control and presence.** Opcodes: `CT_SUBSCRIBE` (+ ephemeral YAML config),
`CT_HEARTBEAT`, `CT_STOP` host→box; `CT_SESSION_EVENT` [`SEV_HOST_PRESENT`|`SEV_HOST_GONE`] box→host;
`CT_RADIO` (0x16) the app's mid-session radio kill switch (flag `/tmp/radio_off`, ocbmd-owned, cleared
on fresh SUBSCRIBE, app loss, or ocbmd start — the box side is complete but `sendRadio` is currently
uncalled app-side). ocbmd tracks `(subscribed, present)` behind a heartbeat watchdog
(`HEARTBEAT_GRACE` 10 s) and mirrors presence to `/tmp/host_present`, the cross-process signal
rx_connect and airplayd read. `tools/session_supervisor.sh` is the actor: on the presence edges it
ARMs (airplayd `OCBM_FWD_ENC=1` + rx_connect) or TEARs DOWN to the holding pattern, where iap2d stays
up so the iPhone remains an enumerated accessory with CarPlay not running. GONE teardown also powers
BT off (`hciconfig hci0 down`, not just noscan) to drop the iPhone's BT connection to an app-less box;
the wired iap2d holding pattern is the sanctioned exception, since no radio is involved.

Because ocbmd applies its grace before moving `/tmp/host_present`, a heartbeat blip shorter than
`HEARTBEAT_GRACE` never reaches the actor — no teardown/re-ARM cycle occurs. A clean `CT_STOP` is the
opposite case and reaches it immediately: the flag goes to 0 in the same instant, and the actor's 1→0
edge runs the complete wireless teardown back to IDLE. The exception is a *replacement* host whose predecessor died without
`CT_STOP`: `rearm_presence_silently()` dips the flag for `REARM_HOLD` (2 s) so the 1 Hz actor loop can
observe a GONE→PRESENT edge, while the host itself is never sent `SEV_HOST_GONE`.

> **Replacement detection requires `present` AND `subscribed`, not `present` alone.** The flag means
> "the previous host died mid-session without `CT_STOP`", and only a SUBSCRIBEd host has a session to
> die in. A predecessor that closed cleanly is never a replacement: `CT_STOP` already dropped both
> flags. `host_replaced` is also cleared in `go_idle()`.

**Possible refinement, not done:** move teardown into airplayd as a graceful RTSP TEARDOWN (a
presence-watchdog thread shutting the control socket) so airplayd stays a persistent daemon rather
than being killed. Cleaner; the kill-based supervisor is correct and was lower-risk to ship.

### Empirical basis

Measurements taken on the box this session that ground the claims above:

- **`/sys/class/udc/*/state` ≠ app liveness.** A host echo pushed **137 MB at 220 Mbps** through the
  accessory link while `ci_hdrc.1` read `suspended` and `ci_hdrc.0` read `configured` — both unchanged
  whether or not a host app was active. "configured" means the Mac is plugged in and the kernel enumerated
  the gadget; it stays "configured" through an app crash. So USB device-state tracks **cable enumeration**,
  not host-app presence. This is why detection leans on drain-health, not USB state.
- **ocbmd `out_lo` memory ratchet.** At idle (no A/V, no airplayd) ocbmd held **18.7 MB RSS** vs iap2d's
  188 kB. Cause: `out_lo` is a `Vec<u8>` grown by `extend_from_slice` during A/V forwarding with a slow/absent
  reader, and `Vec::drain(0..w)` never returns capacity — so RSS holds the high-water mark until restart.
  The A/V ingest path had no backpressure guard (unlike the srcbench flood, which caps at 256 KB). The
  live-UI drop-on-backpressure design removes the accumulation entirely.
- **Idle-link desync.** See prerequisite #1 — the recurring `no HELLO_ACK`/mismatch-until-restart pattern.
- **Fixed buffers are negligible.** `scratch`/`rbuf`/`plbuf` ≈ 64 KB each (`MAX_PAYLOAD` 64 KB); the box has
  **123 MB total RAM, no swap** — so an unbounded A/V queue is a real OOM risk, which is why "no software
  A/V buffer" is a hard requirement, not a nicety.

---

## Lifecycle hardening

<!-- absorbed: ../carplay/02_SESSION_LIFECYCLE.md -->

Synthesis of a 13-agent read-only analysis of the box's lifecycle/session management, plus a review of Apple's publicly shipped CarPlay SDK (`CarPlaySDK.framework` "StarkSDK" inside Xcode's
`CarPlaySimulator.devicekitplugin`). Motivated by the peer-wipe stall (`docs/carplay/02_SESSION_LIFECYCLE.md`), where the box entered
an unrecoverable flapping state that only a manual power cycle fixed.

Apple constants below are marked **[Apple-evidenced]** (observed in the shipped SDK's symbols and resource files) vs
**[inferred]**. Everything else is grounded in the cited box source.

### Root cause (unanimous across the analysis)

1. **No truthful health signal.** "Healthy" = `host_present=1` + `pgrep -f airplayd`
   (`tools/session_supervisor.sh:79` as the file then stood — that line is blank today). airplayd's accept
   loop stays alive across failed pair-setup/connect-out cycles, so a session that never reaches
   pair-verify or streams looks identical to a live one. Presence (subscribed) is conflated with health
   (streaming).
   **CORRECTED 2026-08-16 — FIXED, do not re-do:** the supervisor now latches `pair-verify OK` then
   `RECORD done` from the transport-scoped airplayd log (`scan_milestones`) and publishes the verdict to
   `/tmp/session_healthy` (`write_healthy`); the bare `pgrep -f airplayd` is gone — `airplayd_alive()`
   uses exact-match `pgrep -x` probes and is *deliberately* not `pgrep -f`, which false-matched a
   `tail`/`grep` of an `*airplayd*.log`.
2. **No self-heal.** The re-arm path is an unbounded `arm || sleep 4` with no escalation. The one counter
   (`fails`) was **zeroed by `teardown()` on every GONE edge**, so a presence flap erased its own counter
   each cycle. The only real reset primitive (the OTG/gadget baseline reset) existed **only in the manual
   `tools/cold_start2.sh:36-41`** (still that exact block today), never in any automated path — which is
   literally why the incident needed a physical power cycle.
   **CORRECTED 2026-08-16 — FIXED, do not re-do:** `tools/phone_reset.sh` is that primitive as an
   automated script (installed `/script/phone_reset.sh` by `tools/ocbm_install.sh`), invoked by
   `tools/session_supervisor.sh` at both the L1 and L2 rungs; and the STUCK counters now live in a block
   explicitly commented *"INTENTIONALLY survives teardown()"*, cleared only by a confirmed-established
   session held `CONFIRM_HOLD` seconds.
3. **Incident trigger:** `airplayd` reads `/etc/carplay_peers.bin` only once at startup, so the
   mid-session `rm` silently diverged disk from memory and surfaced as a failure at an arbitrary supervisor-chosen
   restart. State mutation was uncoordinated with the live session.

### Apple's session/lifecycle model (the reference we should match)

- **Two keepalive regimes.** ACTIVE data socket: TCP keepalive **idle 3 s / interval 3 s / 3 probes →
  ~12 s dead-link detection** (`_AirPlayReceiverSessionSetup`). IDLE: TCP keepalive **disabled**, replaced
  by a **30 s** UDP beacon poll (`0x7530`=30000 ms) — the real `kAirPlayDataTimeoutSecs`. Link-loss is
  distinguished from quiet-but-live by *mechanism* (probe exhaustion / beacon absence), not silence.
  **[Apple-evidenced]**
- **"Established" = RECORD**, not SETUP. Milestone chain: **Bonjour → Connect → Auth → Announce → Setup →
  RECORD**; session-started telemetry emits only after RECORD; the event channel is accepted at RECORD.
  **[Apple-evidenced]**
- **One idempotent, reason-carrying finalize:** `AirPlayReceiverSessionTearDown(…, OSStatus reason,
  Boolean *outDone)` — clean end and failure share it; per-stream partial teardown (`outDone=false`) keeps
  the session alive. **[Apple-evidenced]**
- **Tiered re-establishment:** Pair-**resume** (reuse persisted session id) → **pair-verify** (known
  device) → **pair-setup** (cold), with explicit resume→verify fallback. **[Apple-evidenced]**
- **Live re-negotiation via `updateDisplayPanels`** (keys `widthPixels/heightPixels/heightPhysical/maxFPS/
  displayUUID/primaryInputDevice`) — accessory pushes display/resolution changes iOS honors live, **no
  re-pair**. When we build this, the app authors the new panel values and the box relays them
  (docs/carplay/04_CAPABILITIES_AND_CONFIG.md). Same session-control family as `changeModes`/`modesChanged`, `forceKeyFrame`, `duckAudio`,
  `requestUI`, `accessoryAcquireFocus`. **[Apple-evidenced — key set; exact plist schema inferred]**
- **RTSP `/feedback`** keeps the session warm (answering it prevents the phone's own teardown).

### The solution: a bounded self-heal loop (detect → represent → escalate → prevent)

> **docs/carplay/04_CAPABILITIES_AND_CONFIG.md note:** the box rightly executes this self-heal machinery (supervising its own daemons is
> hardware-host work), but every tunable in it — stuck thresholds, escalation triggers, backoffs, the
> reboot budget, the keepalive/grace constants in "Supporting layers" — is a configurable value:
> app-pushed config the box executes. The numbers in this plan are interim box-side values pending
> config coverage. (The 3/3/3 keepalive and ~12 s grace mirror the phone's own Apple-evidenced
> regime, so their *useful* range is narrow — treat them as app-overridable rather than free config;
> whether they belong in the docs/carplay/04_CAPABILITIES_AND_CONFIG.md "permanently-stable mechanics" bucket is an open call.)

#### 1. Truthful health signal
`airplayd` writes `/tmp/session_healthy` atomically (mirror of ocbmd `write_flag_atomic`): `1` once
**pair-verify secret derived AND RECORD accepted / ≥1 video AU forwarded**, `0` at start/on failure. The
supervisor consumes this instead of `pgrep airplayd`. RECORD is Apple's own "established" edge.

#### 2. Stuck detection with a counter that survives teardown
Count presence edges / iap2d exits / arm cycles on the monotonic `/proc/uptime` clock. **Reset the counter
only on a confirmed-established session held ≥15 s — never in `teardown()`** (fixes the exact bug). Stuck:
≥5 presence edges in 20 s; ≥3 projection failures; ARMED but `session_healthy=0` past a milestone-aware
grace; ncm0 no-carrier / ifindex churn; iap2d exit-rate.

#### 3. Reset primitive + escalation ladder (the missing self-heal) — SHIPPED
`phone_reset()` — the automated power-cycle equivalent — ports the OTG baseline from `cold_start2.sh:36-41`
(`a_clr_err`/`a_bus_drop`/`a_bus_req` + gadget disable + `functions` clear + `ncm0 down` + settle),
**strictly scoped to `ci_hdrc.0`**, `pkill iap2d` *before* dropping the gadget.
**SHIPPED (verified 2026-08-16) as the standalone `tools/phone_reset.sh`** (installed
`/script/phone_reset.sh`), called from the L1 and L2 rungs of `tools/session_supervisor.sh`; L3 and its
persistent `/etc/ccpa_reboot_count` budget shipped with it.

| Level | Action | Trigger |
|---|---|---|
| L0 Retry (exists) | re-ARM airplayd, backoff `fails*5` cap 30 s | airplayd death while armed |
| L1 Phone-facing reset | `phone_reset()` → re-run projection | ≥3 projection fails / ncm0 stuck / iap2d flap |
| L2 Full daemon restart | restart ocbmd + iap2d + airplayd + rx_connect; force `host_present=0` | presence-flap loop, L1 ×2, or **ocbmd wedge** (below) |
| L3 Reboot | `reboot -f`, **persistent `/etc` budget ≤2/10 min**, then park in IDLE + surface fault | L2 exhausted, or L2 fails to clear an **ocbmd wedge** within 10 s |

The reboot budget **must live in `/etc` (jffs2), not `/tmp` (tmpfs)**, or it evaporates each reboot.

**ocbmd wedge escalation (added after a bench incident: `ocbmd` stuck in an uninterruptible HCI
ioctl during BT line-discipline teardown never exits, so the inittab respawn, the pid-lock
singleton, and the failover watchdog's first-120s window all did nothing — USB writes to the
host stalled until a manual power cycle).** `ocbmd` touches `/tmp/ocbmd_alive` (mtime only) once
per second from its dispatch loop. `session_supervisor.sh`'s 1 Hz main loop calls this WEDGED
(independent of, and in addition to, the phone-session ladder above) when **all** of: the mtime
of `/tmp/ocbmd_alive` is stale, the Mac/host-facing accessory gadget
(`/sys/class/android_usb_accessory/android0/state`) reads `CONFIGURED`, and `pidof ocbmd` is
non-empty. An absent `/tmp/ocbmd_alive` is a no-op (older `ocbmd` builds never create it).
Staleness is checked with `find /tmp/ocbmd_alive -maxdepth 0 -mmin +0`, the same integer-minute
BusyBox `find` idiom already used by `radio_hal.sh`/`radio_ap_up.sh` for stale-lock detection —
this box's BusyBox has no `-newermt`/fractional support, so `-mmin +0` (>=1 full minute stale) is
the finest granularity available, not the 15 s the underlying contract targets. On WEDGED
(rate-limited to once per 60 s) the supervisor logs to `/tmp/box.log`, runs the same L2
restart primitive (`restart_ocbmd_daemon()` in `tools/session_supervisor.sh` — extracted from the
L2 rung above so both callers share one code path), then polls `pidof ocbmd` for up to 10 s: if
the pid is unchanged (a D-state process ignores `SIGKILL`), it escalates straight to L3
(`sync; reboot`).

#### 4. Prevent the trigger
Idle-gate persistent-state mutation: refuse peer-store writes/deletes while `present==1`; route
config/pairing changes through quiesce → mutate-at-idle → clean restart.

### Supporting layers

- **Observability:** supervisor-written `/tmp/carplay_state` (`phase` + flap counters +
  `health=STUCK reason=…` verdict), uptime-prefixed log lines, a **count-bounded** transition ring (the
  current blind byte-tail truncation discards the flap *onset*), and a `carplay-status` reader.
- **Supervision floor — SHIPPED (task #28):** `ccpa/rootfs/etc/inittab` now carries
  `::respawn:/script/run_ocbmd.sh` and `::respawn:/script/run_supervisor.sh` alongside the UART console,
  and the supervisor health-checks `airplayd` + `rx-connect` + `iap2d`, not just airplayd.
  **CORRECTED 2026-08-16 — the drifted-copy item is CLOSED, and the drift now runs the other way.** BOTH
  `ccpa/rootfs/script/ocbm_boot.sh` and `tools/ocbm_boot.sh` launch the supervisor from the identical
  line 31 (`[ -x /script/session_supervisor.sh ] && setsid /script/session_supervisor.sh …`), and have
  done so as far back as this repo's git history reaches (2026-07-25), so whether the rootfs copy ever
  lacked it is no longer verifiable — treat the item as fixed, not as wrong. Today the rootfs copy is the
  LONGER one (+76 lines): a first-boot dead-man (`/script/ocbm_trial`) and an opt-in NCM failover
  watchdog (`/script/ocbm_failover`) that the `tools/` copy lacks. Reconcile toward the rootfs copy,
  never away from it.
- **Apple keepalive regimes in airplayd:** arm TCP keepalive **3 s/3 s/3** (Linux `TCP_KEEPIDLE`, not
  Darwin `TCP_KEEPALIVE`) on the iPhone-facing socket; pin the RECOVERING grace to the phone's **~12 s**
  active budget (keep answering keepalive/`/feedback` for the full window); keep 30 s only as the
  idle/no-A/V backstop; add a control-channel inactivity watchdog gated on audio/video-active flags.
- **Control-plane feedback + host resilience:** wire the dead delegates (`client.delegate` and
  `transport.delegate` are never assigned — the box's `SEV_HOST_GONE` and USBTransport's read-error/
  write-fail safety nets are dead code). **RESOLVED 2026-08-01: now wired, `AppDelegate.swift`** (both
  are assigned to `OCBMSessionCoordinator`, which implements the handlers). Widen `CT_SESSION_EVENT`
  to `[state][reason]` so the box streams
  real lifecycle state; add host-side establishment timeout + A/V-progress watchdog + bounded resubscribe
  with an atomic `OCBMAVDecrypt.reset()` (ChaCha counter lockstep).
- **Single reason-carrying finalize** (mirror `AirPlayReceiverSessionTearDown(reason, outDone)`): graceful
  RTSP TEARDOWN with a status, callable from clean stop and failure/Drop; partial-stream teardown stub.

### What is shipped, and what is left

The real state machine today is four loosely-coupled FSMs across two processes plus a shell loop. The
intended endpoint is an explicit model with STUCK and RECOVERING states, ARMED split from STREAMING,
and the invariant `ARMED ⇒ iap2d alive` — ultimately a small Rust supervisor daemon (`ccpad`) owning a
dependency DAG with real `fork`/`waitpid`, pinned as the single `::respawn:` entry. `ccpad` would be
box-internal process supervision only: no CarPlay policy, tunables arriving in the app-pushed config.
No such binary exists.

**Shipped:** the health gate at the RECORD milestone; a flap counter that survives teardown;
`phone_reset()` with the L1/L2 ladder; idle-gated mutation; the `/tmp/carplay_state` verdict;
`inittab` respawn plus iap2d/rx_connect health; Apple keepalive 3/3/3 (`airplayd::arm_keepalive`); the
L3 reboot budget (`/etc/ccpa_reboot_count`); uptime-stamped logs and a count-bounded transition ring
(`/tmp/lifecycle.ndjson`); host-app delegate wiring and the A/V-progress watchdog
(`OCBMSessionCoordinator`).

**Open.** The 12 s RECOVERING grace as specified: what shipped is milestone-aware but generous
(`ESTAB_CONNECT_GRACE=90` → `ESTAB_STREAM_GRACE=30`), not the phone's ~12 s active keepalive budget.
Bounded resubscribe with an atomic `OCBMAVDecrypt.reset()`: no `reset()` exists, and re-SUBSCRIBE is
the unbounded heartbeat / `SEV_HOST_GONE` retry in `OCBMClient`. Strategic items, none started: a
box→host `CT_SESSION_EVENT[state][reason]` stream (still the 2-byte `[CT_SESSION_EVENT][SEV_*]`); a
single reason-carrying finalize with partial-stream teardown; the formal state machine and `ccpad`; a
pair-resume tier; `updateDisplayPanels` live display re-negotiation.

### Cross-cutting guardrail (every agent flagged this)

**Do not let self-heal become a new flap.** Cold pair-setup can block on the human "Allow CarPlay" tap for
tens of seconds, so grace must be **milestone-aware** — start the stream-grace clock only *after* a control
connection appears, not at ARM. Every escalation must be bounded (backoff + persistent reboot budget +
park-in-IDLE on exhaustion). A naive fail-counter or tight timeout tears down sessions about to succeed, or
reboot-loops on a genuinely absent phone. The ~5 s host grace must stay inside the phone's ~12 s active
keepalive budget.

---

## Session start ordering and reference authority

<!-- absorbed: ../carplay/02_SESSION_LIFECYCLE.md -->

**Original status: AUTHORITATIVE for (a) the reference authority order and (b) the point in the session
start sequence at which the iAP2-over-AirPlay tunnel is opened.** Supersedes the placement decision implied by
docs/wireless/00_WIRELESS_CARPLAY.md and docs/wireless/00_WIRELESS_CARPLAY.md, which opened the tunnel from `events::setup()`.

This document exists because the previous session's conclusion — "our sequence is now conformant with
everything R14G17 specifies" — was true of *message content* and false of *ordering*.

Reference root (see `../ops/03_REFERENCE_INDEX.md`):
`~/carlink/local_carplay_sdk/reference/apple_carplay_sdk_R14G17/` — below, `SDK/`.

---

### 1. REFERENCE AUTHORITY ORDER (owner directive, 2026-07-25)

Future sessions **must** weight the reference material in this order. This is not a heuristic; it was
stated explicitly by the project owner and it has already changed two decisions (§3).

> **⚠️ THE 2026-07-25 ORDER IS SUPERSEDED — use the REORDERED list below.** A further owner
> directive of 2026-08-10 replaced it; the original 3-tier order is archived, with why it failed,
> in [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-44-3`.

**REORDERED (owner directive, 2026-08-10). Ordered by CURRENCY.** Sessions must consult in this order:

1. **`CarPlaySDK.framework`** — the CURRENT receiver side, inside the Simulator plugin at
   `…/CarPlaySimulator.devicekitplugin/Contents/Frameworks/CarPlaySDK.framework`. **Look here FIRST.**
   Symbol names carry full C signatures, so a feature's contract is usually readable with
   `strings | grep`, no disassembly. It is the ONLY source covering everything post-2017.
2. **The rest of the CarPlay Simulator** — implementation examples and working config: the ten real
   `VehicleConfig` templates (`Contents/Resources/VehicleConfigs/Configs/`), `iAP2MessageKit`'s
   parameter catalogs via `tools/i2mspec_dump.py`, the statically-linked `iAP2Link.c`. **Caveat: the
   Simulator is WIRED-ONLY** (USB device classes), so a capability its templates enable may never be
   exercised there — evidence of the CONTRACT, not of the behaviour.
3. **CT5 CINEMO** (`GMCarPlay.apk` + `libNmeCarPlay.so`/`libNmeIAP.so`/`libNmeTransport.so`) — a
   shipping head unit; authoritative for what the Apple sources leave open.
4. **SpeedPlay TBOX.**
5. **The licensed R14G17 SDK source (2017).** Still LICENSED FIRST-PARTY SOURCE, and for what it
   ACTUALLY CONTAINS it is byte-authoritative and beats any re-derivation — `Platform/HID*.c` are
   literal descriptor builders, and the knob template is byte-identical to the 2026 Simulator's.
   Ranked fifth for SEARCH ORDER, not for trustworthiness. The distinction that matters: **its
   presence is authority; its absence is not evidence.**
6. **Everything else** — the stock CPC200-CCPA riddleBox firmware, the iOS extracts — supplementary,
   consulted to fill gaps the above leave open.

**Why the reorder (2026-08-10).** Four features this project actually needed were invisible in
R14G17 and fully described in `CarPlaySDK.framework`: the RCS DataStream (stream type 130) and its
seven client types, `MainBuffered` audio, the Enhanced-Siri `AuxIn`/`AuxOut` uplink, and the SETUP
feature-intersection gate. The 2026-07-25 DataStream breakthrough came from exactly this escalation.
Putting the current receiver first makes that the default path rather than the recovery path.

#### Why Carlinkit implementations are not normative

Both the stock CPC200-CCPA firmware and the TBOX SpeedPlay are *working* wireless-CarPlay
implementations, and that makes them tempting to copy. They are also **flawed in various respects — the
reason OCBM exists at all.** The purpose of this project is to replace Carlinkit's code with an
implementation that honours Apple's intended design as closely as possible. "Carlinkit does X" is
therefore evidence that *X can be made to work*, never evidence that *X is correct*.

This is not an abstract concern. `libSdCarplay.so` embeds its own build paths:

```
D:/eclipse-workspace/jni_Reverse_auto/reverse-aa//jni/plugin_47580/iap2link/iAP2Link/iAP2Link.c
```

SpeedPlay's iAP2 link layer is a **reverse-engineered re-derivation** of Apple's reference, not a
licensed drop. Its parameter choices are "known to work against the iOS of that era", nothing more.

**AMENDED — this demotion was applied too broadly and cost a deploy cycle (docs/carplay/05_METADATA_AND_CONTROLS.md §8).** "Don't copy
Carlinkit's design" is right; "ignore what Carlinkit observed on the wire" is not. Their implementations
are working references whose *choices* are not normative — their *observations* still carry evidence.
SpeedPlay's wireless `MaxRcvPacketLength = 0xFFFF` (against `0x0800` on BT/USB) was demoted under this
section and was **correct**.

---

### 2. THE CHANGE: the tunnel is opened at the END of `record()`, not from `events::setup()`

This ordering change is SDK-conformant and hardware-verified, and it stays — but it did **not** fix
wireless metadata. The missing piece was the unanswered RCS DataStream SETUP, not the start point.

#### What Apple does

`AirPlayReceiverSessionStart()` — reached from `_requestProcessRecord` — performs, in order,
`_ControlStart` (which *accepts the event-channel TCP*), the audio thread creation, `_ScreenStart`, and
the platform `kAirPlayCommand_StartSession`. Only if **all** of those return `kNoErr` does it set:

```c
inSession->sessionStarted = true;
```
`SDK/AppleCarPlay/Sources/AirPlayReceiverSession.c:1147` (the whole function is `:1096-1168`; the
failure-gated steps are `_ControlStart` `:1108-1112`, the three `pthread_create` audio threads
`:1116-1136`, `_ScreenStart` `:1138-1142`, and the platform `StartSession` `:1144-1146`). A failed start
is answered `500` and `didRecord` is never set — `SDK/AppleCarPlay/Sources/AirPlayReceiverServer.c:3136`.

Every accessory→phone command is hard-gated on that flag:

```c
require_action_quiet( inSession->sessionStarted, exit, err = kStateErr );
```
`SDK/AppleCarPlay/Sources/AirPlayReceiverSession.c:825` (`AirPlayReceiverSessionSendCommand`)

`AirPlayReceiverSessionSendiAPMessage` (`:5331-5364`) routes through that same function, and adds its own
`NetTransportTypeIsWireless( inSession->transportType )` requirement at `:5346`.

#### What we were doing (WRONG)

`crates/vendor/receiver/src/events.rs::setup()` is this project's `_ControlStart` equivalent — it is
called from the event-channel accept loop inside `session.rs::record()`. `iap_tunnel::start()` was
invoked at the end of it. That put our first `iAPSendMessage` **before** the stream/screen bring-up and
the session-focus handshake that follow it in `record()` — i.e. several steps earlier than the point at
which Apple's own code permits a single byte to be sent.

#### What we do now

`iap_tunnel::start()` is called at the **end of `session.rs::record()`**, after the event channel, the
stream threads and the session-focus handshake have all completed. `events::setup()` carries a comment
recording why it must not be moved back.

Both call sites were and remain gated on `CARPLAY_WIRELESS_METADATA`, so with the variable unset (every
wired session) the change is a pure no-op. The wired path is unaffected.

#### The structural gate (added after verification review)

Moving the call site alone left two holes, both flagged by the conformance review and both now closed:

1. **Control-start failure was not gated.** Apple never sets `sessionStarted` if `_ControlStart` fails, so
   nothing can be sent. Our `record()` falls through all three failure paths (accept timeout, accept
   error, no listener). `record()` now carries a `control_started` flag and opens the tunnel only when the
   event channel actually came up. We deliberately do *not* also answer RECORD with 500 as Apple does —
   that would change the proven wired baseline.
2. **`modes_changed_tunnel_nudge()` could still fire early.** It is reachable from the **control**
   connection (`session.rs::command()`), which is served independently of RECORD, so a `modesChanged`
   arriving before RECORD completed could have opened the tunnel anyway — reintroducing the exact bug the
   move was meant to fix. There is now a `SESSION_STARTED` atomic in `events.rs`, set from the end of
   `record()`, checked by the nudge before it consumes its one-shot retry.

`SESSION_STARTED` is **not** enforced inside `send_command` itself, even though Apple enforces its
equivalent on every send at `:825`. `send_request_ui()`/`send_take_screen()` are issued from inside the
RECORD accept arm — earlier than the flag is set — and are part of the proven wired baseline. Gating them
would be more conformant and is a real remaining divergence (§7.5), but it is not worth regressing a
working path to close on a path that is already working.

#### Corroboration from CINEMO

CINEMO defers even further than Apple's minimum. `handleSessionStarted()` — the handler for Apple's
`AirPlayReceiverSessionStarted_f` — does not create the WiFi iAP2 link inline; it **posts**
`MESSAGE_START_IAP` to a handler queue (`CarPlayManager.java:1052`), and `handleStartIAP()` re-checks
`mConnectionState == CONNECTED` before proceeding. The WiFi link is created strictly after the AirPlay
session is started, never before, and never speculatively.

Its native gate agrees: `libNmeCarPlay.so fcn.0x0006da00` (`GetIAPOverWiFiURL`) locks a weak reference to
the live session and bails with `"GetIAPWifiUrl: No session running"` (0x6db2c) if it is gone.

---

### 3. TWO HYPOTHESES CLOSED

#### 3.1 "The `disableBluetooth` command requires us to tear down the BT iAP2 link" — REFUTED

The two vendor implementations disagree, and under §1 the disagreement resolves against SpeedPlay.

- **SpeedPlay** treats it as an imperative. `iap2_start_wifi_session` (`libSdCarplay.so` @ `0x13b38`) runs
  `iap2_wifi_stop` → `iap2_notify_wifi_connect_state(1)` → **`iap2_bt_stop`** → `iap2_wifi_start`, with no
  window in which both links are attached.
- **CINEMO never hands over.** `handleBtDisable()` (`CarPlayManager.java:774-795`) only broadcasts
  `gm.carplay.intent.action.disableBT` at the BT/DCM stack, targeting the phone's HFP/A2DP profiles.
  Nothing in `GMCarPlay.apk` ever closes the type-2 (BT) iAP2 accessory — every `stopAuth` call site is
  type **3** (WiFi). The BT and WiFi iAP2 links run simultaneously for the whole session.

CINEMO's reading is also the strict reading of Apple's own requirement — *"iAP2 over Bluetooth must not be
disconnected until the disableBluetooth command is received"*
(`SDK/AppleCarPlay_CommunicationPlugIn_IntegrationGuide.txt:306`) is a floor, not a mandate to disconnect.

**Conclusion: our existing behaviour — recognise and log `disableBluetooth`, take no teardown action — is
correct. Do not "fix" it.** Had SpeedPlay been weighted equally, we would have implemented a teardown that
Apple does not ask for and the authoritative vendor does not perform.

#### 3.2 "Our `iAPSendMessage` never actually leaves the box" — REFUTED BY EXISTING EVIDENCE

A plausible theory (the accessory-side analogue of `SendCommand` returning `kStateErr`) is that the send
silently fails and the bytes never reach the phone. **This repo already contained the disproof.**

`events.rs::handle_inbound_event` parses the RTSP status line of responses to our own commands and logs
every non-2xx (`crates/vendor/receiver/src/events.rs`, the `strip_prefix("RTSP/1.0 ")` branch at the top
of that function). The device-observed result is recorded a few lines further down, in the same
function's `body.is_empty()` early-return comment: iOS answers `iAPSendMessage` with a **bodyless 2xx**.

So the carrier is accepted and the bytes do leave. The phone simply never binds an iAP2 link to the
tunnel. That is what makes *start ordering* the live suspect rather than message content.

---

### 4. WHAT IS ALREADY CONFORMANT (do not re-investigate without new evidence)

> **⚠️ THE `MaxPacketSize` ROW IS ACTIVELY WRONG AND COST A DEPLOY CYCLE** — it told readers not to
> make the change that was required; the tunnel needs `0xFFFF` (docs/carplay/05_METADATA_AND_CONTROLS.md §2.2). Corrected in place in
> the row itself. Also note the "inbound tunnel frames on the **control** connection" row is scoped
> to the `POST /command` carrier only. The other rows stand. Full reasoning:
> [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-44-2`.

| Aspect | Status | Evidence |
|---|---|---|
| Tunnel carrier shape `{type:"iAPSendMessage", params:{data:…}}` | Conformant | `SDK/…/AirPlayReceiverSession.c:5350-5355` vs `events.rs::send_iap_message` |
| `data` key lowercase | Conformant | `AirPlayCommon.h` `#define kAirPlayKey_Data "data"` |
| Outbound commands on the **event** channel | Conformant | `SendCommand` dispatches on `eventQueue`/`eventReplyTimer`, `AirPlayReceiverSession.c:816-838` |
| Inbound tunnel frames on the **control** connection | Conformant | docs/carplay/03_SDK_GROUND_TRUTH.md §1; proven live by the `modesChanged` nudge firing |
| A full, fresh iAP2 identification over the tunnel | Conformant | IG:289; CINEMO runs a second `ICinemoIAP.Open(…, flags=142)` |
| Tunnel Identify declares the metadata message ids | Conformant | `iap_tunnel.rs`'s `Action::SendIdentify` arm → `message.rs`'s `TransportComponent::AirPlayTunnel` params-6/7 arm (generated from `features.rs` since docs/carplay/05_METADATA_AND_CONTROLS.md); CINEMO publishes the same in `MediaManager.java:163-173` |
| Wireless transport component advertised over BT | Conformant | param 24 with sub 2 `TransportSupportsiAP2Connection` + sub 4 `TransportSupportsCarPlay`, `message.rs`'s param-24 `WirelessCarPlayTransportComponent` block; CINEMO's equivalent is `CinemoWIFIComponent`, `BTAccessory.java:27-34` + `Util.java:57-63` |
| Zero-Ack SYN link parameters | Conformant | IG:290 recommends them |
| `MaxPacketSize` on the tunnel | ~~Conformant~~ **WRONG — corrected, docs/carplay/05_METADATA_AND_CONTROLS.md §2.2** | This row previously read *"SpeedPlay's contrary values … are **not** a reason to change ours"* and so told readers **not** to make the change that was required. `MaxPacketSize` **must be `0xFFFF`** here: it is Apple's transport-type-2 value, the stream's SETUP declares `controlType=2`, and the iPhone's own SYN-ACK carries it. With `0x1000` we were the only party proposing 4096. Now `SYN_PARAMS_ZERO_ACK_TUNNEL`; BT and wired keep their proven constants. **The methodological error: SpeedPlay's *choice* was demoted and its *observation* discarded with it** |

**Important scope note.** docs/wireless/00_WIRELESS_CARPLAY.md's rule *"never grow identify params 6/7"* is a constraint on the
**Bluetooth** Identify, which is byte-pinned by device testing. It does **not** apply to the tunnel
Identify — that one must declare its own metadata message ids, and CINEMO confirms it.

---

### 5. `AdvancedFeatures` / `DashboardInfo` — Carlinkit-private, no protocol counterpart

These were raised as a possible metadata gate. They are not.

From the stock firmware's own web config UI (`/etc/boa/www/js/app.js` in the preserved riddleBox rootfs —
human-readable even though the CarPlay binaries are packed):

- `DashboardInfo` — bitmask, default 1, max 7. **bit0 = Media info, bit1 = Vehicle info, bit2 = Route
  info.** An earlier comment in `host/…/MessageSerializer.swift` guessed bit1 was a "comm engine" slot;
  it is not, and there is no communications bit. Corrected in that file 2026-07-25. The preserved unit
  persists `DashboardInfo: 1` in `/etc/riddle.conf`.
- `AdvancedFeatures` — *"Legacy naviScreenInfo gate — REDUNDANT if host sends naviScreenInfo in
  BoxSettings (firmware bypasses this check at 0x16e64). Also advertises "naviScreen" in boxInfo
  supportFeatures."* It gates **nav cluster video**, not metadata.

`/etc/airplay.conf` contains no metadata gating at all (only `oemIconVisible`, `name`, `model`,
`oemIconPath`, `oemIconLabel`).

**CINEMO has no equivalent knob.** Its gating *is* the identification: declared message ids, plus
transport components flagged `SUPPORTS_IAP2_CONNECTION=1` / `SUPPORTS_CARPLAY=1`, plus which features run
in phase 1 (`RouteGuidance`, immediately on tunnel-identify success) versus phase 2 (media, call state,
after `notifyCarPlaySession(true)`). There is no iAP2 display-component parameter for media or
communications — only `ROUTE_GUIDANCE_DISPLAY_COMPONENT` (param 30).

---

### 6. NEW CAPABILITY: the iPhone's own logs, over USB

Verified working on iOS 27.0 (iPhone Air, `iPhone18,4`) 2026-07-25 with `idevicesyslog`
(libimobiledevice). **This is the phone's side of the conversation and we had never used it.**

The signal-carrying processes, measured against a 94k-line/15s baseline:

- **`airplayd(CoreUtils)`** — the single most valuable source. These lines come from the *same
  CoreUtils/HTTPClient code as our licensed R14G17 reference*, so they report every `/command` exchange
  by type and status:
  ```
  Request start:     CID 0xBCCF04BA, Peer (null), TimeoutSecs 10
  Request written:   CID 0xBCCF04BA, Header 69 bytes, Body 0 bytes, Type 'null'
  Response received: CID 0xBCCF04BA, Header 75 bytes, Body 0 bytes, Status 200
  ```
- **`carkitd`** — CarPlay/iAP2 daemon.
- **`accessoryd`** — iAP accessory daemon.
- **`wifid(WiFiPolicy)`** — `__WiFiDeviceManagerTrackCarPlayLinkQuality` (RSSI, tx/rx rate, roam count).

Exclude `bluetoothd` and `CommCenter` from any live stream — measured at ~9,700 lines per 8 seconds on an
*idle* phone, they bury everything else. Capture them in the logarchive instead and query them.

Harness: `tools/capture_iphone_carplay.v2.sh` — the committed successor to this session's
`scratchpad/capture_iphone_carplay.sh` (path updated 2026-08-16; docs/carplay/05_METADATA_AND_CONTROLS.md §6 and docs/ops/02_TESTING.md gate 4 use the
same tool). It streams the filtered set live, then pulls a full
`.logarchive` via `idevicesyslog archive`, which is queryable offline with
`log show --archive … --predicate 'process == "airplayd"' --info --debug`.

**Known limitation:** ~34% of lines carry `<private>` redaction under the default configuration. Apple
publishes per-subsystem **logging profiles** (developer.apple.com/bug-reporting/profiles) that raise the
log level and unredact the relevant subsystems; installing the CarPlay profile on the test device is the
single highest-value improvement available to this workstream and is very likely the reason earlier
sessions found iOS logs uninformative. Installing it requires physical device interaction and has not
been done.

---

### 7. WHAT IS STILL OPEN

1. ~~Whether the ordering change in §2 is sufficient~~ — **ANSWERED: no.** Tested on hardware;
   conformant and harmless, but ordering was never the blocker (docs/carplay/05_METADATA_AND_CONTROLS.md). Note the ordering *within*
   `record()` was itself racy in the other direction — `mark_session_started()` ran before
   `iap_tunnel::start()`, letting a `modesChanged` open the tunnel first and RECORD send a second
   DETECT+SYN (docs/carplay/05_METADATA_AND_CONTROLS.md §5.1). Fixed by inverting it.
2. Whether `disableBluetooth` arrives before or after session-start on a real device. No code-level
   ordering constraint exists in CINEMO between the two callbacks; both handlers are asynchronous.
   Settle it by timestamping its arrival against our own `RECORD done`.
3. The literal bytes CINEMO puts in its first tunnelled message. `libNmeIAP` builds the detect/SYN
   internally and nothing in Java or the string tables shows the frame. SpeedPlay's detect
   (`FF 55 02 00 EE 10`, `libSdCarplay.so` @ `0x149c6-0x14a08`) matches our `link::DETECT` constant, but
   under §1 that is corroboration, not authority.
4. Whether iOS honours a subscribe for a message id that the *tunnel* Identify declared but the *BT*
   Identify did not. CINEMO's architecture implies yes (its WiFi accessory declares its own ids
   independently), but this has not been observed directly on our hardware.
5. **Remaining conformance divergences accepted, not fixed** (from the R14G17 conformance review):
   - `send_command` has no structural `sessionStarted` guard — the ordering is by call-site convention
     plus the `SESSION_STARTED` gate on the tunnel only. See §2.
   - This project spawns the audio and screen threads at **SETUP phase 2**, where Apple creates them at
     RECORD. Earlier, never later, so they are alive before the tunnel opens; Apple's own code does the
     same when a session is already started (`AirPlayReceiverSession.c:2149-2154`, `:3461-3466`), so this
     is benign — but it is a difference from the reference cold-start sequence.
   - There is no counterpart to Apple's platform `kAirPlayCommand_StartSession` (`:1144-1146`), its
     `started_f` delegate (`:1149-1152`), or its 250 ms `_PerformPeriodTasks` timer (`:1156-1163`). These
     are absent rather than reordered; our idle watchdog lives in the control loop.
   - Apple gates iAP-over-AirPlay structurally on `NetTransportTypeIsWireless( transportType )`
     (`:5346`); we use the process-scoped `CARPLAY_WIRELESS_METADATA` env var as the wireless proxy.
     Same intent, different mechanism.

---

## Real-world comparison (GM/vendor behaviour)

<!-- absorbed: ../carplay/02_SESSION_LIFECYCLE.md -->

**Status: REFERENCE (2026-07-23).** Synthesis of two independent research passes — Apple's own
CarPlay session-management implementation (iOS 27 RE + WWDC + CarPlay Simulator.app) and GM's
production CINEMO-based CarPlay stack (Silverado AAOS12 vs CT5 AAOS14) — diffed against
`ccpa_custom`'s current session code (`crates/vendor/receiver/src/session.rs`,
`tools/session_supervisor.sh`, `../carplay/02_SESSION_LIFECYCLE.md`). Purpose: **planning input** for the
next round of ccpa/box-side changes (and, downstream of those, host-app changes) — nothing in this
doc has been implemented yet.

Full source reports (heavier, with file:line citations) — **GONE, verified 2026-08-16.** Both lived in
per-session scratchpad space under `/private/tmp/claude-501/…`, which is not preserved across sessions;
neither path resolves any more. **This document is now the surviving record of them** — do not chase the
paths; re-derive from the primary sources indexed in `docs/ops/03_REFERENCE_INDEX.md` (§C the CarPlay Simulator / `CarPlaySDK.framework`,
§D the iOS 27 extract, §E the WWDC transcripts) if a claim below needs re-verifying:
- ~~`/private/tmp/claude-501/.../scratchpad/apple_wwdc_session_management.md`~~ (lost)
- ~~`/private/tmp/claude-501/.../scratchpad/gm_cinemo_aaos12_vs_aaos14.md`~~ (lost)

**Scope discipline (per explicit user correction):** GM/CINEMO and Apple findings here describe
*other* systems, used only as reference points. They are not conflated with `ccpa_custom`'s own
independent OCBM firmware, nor with the original stock Carlinkit CCPA firmware this project
replaces. Priority direction: the ccpa/box side is the actual lever for session behavior; host-app
changes are downstream of ccpa changes, not a parallel track.

---

### 1. Session establishment

| | Apple (protocol owner) | GM/CINEMO (OEM implementer) | `ccpa_custom` today |
|---|---|---|---|
| Transport negotiation | `StartSession` dict: `sessionType`/`transportType`/`BT Classic`/`nextGenCarPlaySession`; wireless variant additionally carries `ssid`/`pass`/`securityType`/`pubKey`/`mutualAuth` inline (`accessoryd` `platform_CarPlay_startSession`) | iAP1/iAP2 session/metapool config (`com.gm.server.iap2.*`) parametrizes the Cinemo SDK's transport; both USB and WiFi iAP transports registered together (`NmeAndroidTransport`/`NmeIAPWifiTransport`/`NmeIAPUSBTransport`) | Wired: iAP2 over NCM. Wireless: AirPlay SETUP over WiFi. No unified StartSession-style negotiation dict; transport is implicit in which control server is running |
| Wireless handshake | Bonjour-based (`CARSessionRequestClient/-Host/-BonjourHost`) is the *legacy* path; iOS14+ "simplified connection flow" reuses the **existing iAP2 connection** to exchange IP/port directly, dropping Bonjour entirely (WWDC 2023-10150) | n/a (not observed in extraction) | Uses Bonjour/mDNS (`_airplay._tcp` advertised by `rx_connect`) — closer to Apple's *legacy* pre-iOS14 path than the modern simplified flow |
| Mutual auth | MFi-certificate-serial-backed, optional (`supportsMutualAuthentication` flag on `CARSessionRequestHost`) | `OPTION_APPLEAUTH_URL` = `i2c:///dev/i2c-0:16` — hardware MFi chip addressed directly | Onboard MFi 2.0C coprocessor at `/dev/i2c-1@0x11` — architecturally the same "hardware chip over I2C" shape as GM's, just a different bus address |
| Fast-path for known devices | `attempting fast-reconnection with %@` gate, requires cached WiFi credentials + "known & enabled CarPlay vehicle" | n/a | Disk-backed `PeerStore` (Ed25519 pairing persisted, pair-verify-only on reconnect) — already matches the "known device skips expensive handshake" principle Apple describes |

**Takeaway:** ccpa_custom's wireless bring-up mirrors Apple's *legacy* Bonjour-based flow, not the
newer iOS14+ simplified flow that reuses an existing iAP2 connection. Since wired already holds an
iAP2 link independent of AirPlay, there may be room to align wireless bring-up closer to that model —
but this needs its own investigation, not a conclusion drawn here.

---

### 2. Resource arbitration (screen/audio ownership)

| | Apple | GM/CINEMO | `ccpa_custom` today |
|---|---|---|---|
| Vocabulary | `changeModes`/`modesChanged`, two verbs: **take** (unconditional, no give-back bookkeeping — no `epp_UntakeScreen` exists) and **borrow** (temporary, tied to an opaque token, released via explicit `unborrow`) | **TAKE(1)/UNTAKE(2)/BORROW(3)/UNBORROW(4)** — a *symmetric* 4-verb model; TAKE/UNTAKE must NOT carry a borrow ID, BORROW/UNBORROW MUST, and every BORROW is validated against a live borrow-ID map | Sends a one-shot `send_request_ui()` + `send_take_screen()` at RECORD (`crates/vendor/receiver/src/session.rs`, the post-RECORD session-focus handshake — `:1801-1802` as of 2026-08-16; the `:692-693` anchor recorded in 2026-07-23 now lands in `setup_phase2`) — a single outbound "take," no borrow, no untake, no arbitration state machine at all |
| Authority | iOS (the *controller*) is the sole arbiter; accessory proposes via `changeModes`, iOS's `modesChanged` is authoritative — no unilateral local state change on either side | Native VR **borrows** (not takes) Main Audio via `CinemoConstants.CARPLAY_BORROWID_NATIVE_VR` — modeled as a returnable loan, matching Apple's take/borrow distinction exactly | ccpa is accessory-side only; it never contests or arbitrates — it asks once at connect and never again |
| Preemption | AirPlay overlay has a *dedicated* `takeScreenForAirPlayOverlayClient:` entrypoint — explicit, named preemption path | Not directly observed (native strings only) | N/A — no overlay/preemption concept exists in ccpa's model |
| Screen vs audio split | Same take/borrow verbs apply to both `mainScreen` and `mainAudio`, arbitrated independently; `mainAudio` has sub-types (media/alert/speechRecognition/telephony/default), each arbitrated separately; `alternateAudio` is explicitly *never* arbitrated (always mixed) | Confirmed native-string level for both screen and audio; Java-level whitelist found for audio only | ccpa doesn't route audio by iOS's sub-type taxonomy at the arbitration layer — it does route by `audioType` string for sink selection (media→9002, else→9003), which is adjacent but not the same as modeling `changeModes` sub-types |

**Takeaway:** this is the largest structural gap. Both real implementations (protocol owner and OEM)
model resource ownership as an explicit, stateful, two-party negotiation with a request/response
round-trip and a persistent-until-transferred ownership record. `ccpa_custom` currently fires a single
one-way `takeScreen` at connect and never revisits it — there is no borrow concept, no observation of
`modesChanged`-equivalent state from iOS, and no way to model a preempting overlay or native VR
interrupt. Whether this matters in practice depends on whether iOS ever actually contests the screen
against this specific accessory type — an open question, not yet tested.

**Verified correction (2026-07-23 verification pass):** GM's screen-side arbitration is NOT just
inferred from native strings as originally reported — it's fully implemented in Java at
`resmanagement/DisplayStateMachine.java` + `resmanagement/ResourceMode.java`, using the identical
TAKE(1)/UNTAKE(2)/BORROW(3)/UNBORROW(4) wire codes as audio, plus a typed `BORROWID` enum
(`NATIVE_VR`, `BACKUP_CAMERA`, `PHONE_CALL`, `DISPLAY_OFF`, `VOL_CONTROL_UI`, `PHONE_ZONE_UI`, …) that
names *why* a resource is borrowed. This means **GM's model is actually more symmetric than Apple's
own**: GM has both TAKE and a real UNTAKE for screen and audio alike, whereas Apple's `CARSession`/
FigEndpoint layer has no `untakeScreen`/`epp_UntakeScreen` counterpart at all (confirmed by direct
string-table exhaustive search — take is a one-way, no-give-back operation in Apple's own API,
though a bare "Untake" label does appear once, at `AirPlaySender.strings.txt:11443`, in an unrelated
StarkMode logging cluster). **If `ccpa_custom` ever adopts an arbitration vocabulary, GM's symmetric
TAKE/UNTAKE/BORROW/UNBORROW-with-typed-reason-ID model is arguably the cleaner reference of the two**,
not Apple's own asymmetric one.

---

### 3. Session status / connect-disconnect observation

| | Apple | GM/CINEMO | `ccpa_custom` today |
|---|---|---|---|
| State machine | `CARSessionStatus`: idle → connecting-timer started → attempt-on-transport (possibly cancelled) → connected/disconnected/updated → notification posted. Confirmed live (not dead code) via 1:1 log-string correlation | `OnSessionFinalize(reason)` fires at session end; session objects otherwise long-lived (see §4/§5) | IDLE → ARMED → STREAMING → RECOVERING(~5s) → TEARDOWN (docs/carplay/02_SESSION_LIFECYCLE.md) — a comparable number of states, different triggers (host-presence-driven, not phone-presence-driven) |
| Detection signal | Darwin notifications (`com.apple.carplay.in-car`/`out-of-car`/`starting-wired-connection`) + endpoint/auth/config-updated callbacks | Not directly observed (native-only) | Drain-health / backpressure (box-visible, immediate) + accessory-fd error/close + de-prioritized app heartbeat — a deliberately *different but analogous* three-signal design, already justified in docs/carplay/02_SESSION_LIFECYCLE.md by on-hardware measurement (`/sys/class/udc/*/state` proven NOT to be an app-presence signal) |
| Numeric timeout for "connecting"/grace | **`CARSessionStatus.timeoutInterval` defaults to 30s** — confirmed 2026-07-23 by disassembling the real device `CarKit` binary (`split/CarKit`, iOS27/24A5390f): `-[CARSessionStatus initWithOptions:...]` does `mov w2,#0x1e; setTimeoutInterval:`, and the timer fires `timeoutInterval * NSEC_PER_SEC` after `_sessionUpdatesQueue_startConnectingTimer`. (The Simulator's "30 seconds" string was NOT dev-tool-only after all — it matches the real compiled constant exactly.) | `IAP_AUTH_TIMEOUT=10s`, `IAPSession.CLOSE_TIMEOUT=5s`; **better analogue found on verification: `SESSION_FINALIZE_DELAY=2000ms`** (`CarPlayManager.java`) — the actual "wait N seconds after DISCONNECTING, then finalize the session" grace timer, distinct from CLOSE_TIMEOUT (which bounds the lower-level Cinemo `Disconnect()` call itself) | `~5s` RECOVERING grace (docs/carplay/02_SESSION_LIFECYCLE.md, "starting value... tune against measured reattach latencies"), `ESTAB_CONNECT_GRACE=90s`, `ESTAB_STREAM_GRACE=30s`, `HEARTBEAT_GRACE=3s` (session_supervisor.sh) |

**Takeaway (revised):** `ccpa_custom`'s ~5s RECOVERING grace is actually **more than double** GM's own
measured "wait after disconnect, then finalize" window (GM's `SESSION_FINALIZE_DELAY` = 2s, not the
previously-cited 5s `CLOSE_TIMEOUT`) — so ccpa's grace is generous by that comparison, not
under-tuned. Separately, `ccpa_custom`'s `ESTAB_CONNECT_GRACE=90s` ("must reach pair-verify within
this, covers the human Allow tap") is 3x Apple's own real 30s connecting-timeout default — though the
two aren't measuring quite the same window (Apple's timer covers iOS's own connection-attempt state,
ccpa's covers the full human-interaction pair-verify wait), this is now a real, not hypothetical,
number to weigh ccpa's own timers against.

---

### 4. Session-boost / quick-reconnect

Apple ships **two distinct, non-overlapping** fast-reconnect mechanisms `ccpa_custom` has no
equivalent of:

1. **`CRSessionBoostService`/`CRSessionBoostClient`** (carkitd⇄caraccessoryd XPC pair): a proactive
   "start dialing before you're sure you need to" optimization —
   `connectionRequested()` → `startedConnectionAttempt(on:)` → `[cancelledConnectionAttempt(on:) |
   sessionDidConnect(_:)]` → `session(_:didUpdate:)` → `disconnectIfNeeded()`.
2. **`CarPlay_WiFiQuickReconnect`** (AirPlaySender, `carManager_attemptQuickReconnect`): link-quality-
   triggered — on receiving `connect` from the head unit while an existing wireless Bonjour endpoint's
   RSSI is degraded, re-injects a connect to the *same* endpoint rather than restarting full
   discovery.
3. Apple also tracks explicit **reconnection-time telemetry** (`reconnectionBTTime`/
   `reconnectionWifiTime`/`reconnectionTotalTime`/WiFi 4-way-handshake sub-phases) against an internal
   "CarPlay spec" TTR (time-to-reconnect) SLA that triggers an internal bug-report workflow when
   exceeded — confirming Apple holds OEMs (and by extension, any accessory) to a real, if
   unpublished, reconnect-speed bar.

`ccpa_custom`'s `session_supervisor.sh` is reactive only (ARM on host-presence 0→1, TEARDOWN on
1→0) — there is no proactive/anticipatory reconnect path, and no link-quality-aware fast path
distinct from the general grace-timer logic.

GM/CINEMO gave no additional evidence here beyond the general reconnect philosophy in §5.

---

### 5. Reconnect philosophy — recreate fresh vs. resume/pause-in-place

**Both Apple and GM agree, independently, on the same philosophy: never tear down and rebuild
session/media-session objects across a drop — pause/error-state them in place.**

- GM's first-party `CarPlayMediaSession` (CT5) has exactly one `new MediaSession()` call-site,
  guarded create-only-if-null; `release()` never destroys the object; on error it sets
  `STATE_ERROR` with a resolution intent, not a teardown. Unchanged between AAOS12 (as a third-party
  workaround for the same structural reason) and AAOS14 (now simply how GM's own object is built).
- Apple's own resource/session model is built the same way structurally: `modesChanged` updates
  existing ownership state rather than recreating sessions, and WWDC 2023-10150 explicitly instructs
  OEMs to **not** close CarPlay TCP sockets on a short link disconnection — i.e. Apple's own public
  guidance is "debounce and hold the session," not "tear down and recreate."

`ccpa_custom`'s own design (docs/carplay/02_SESSION_LIFECYCLE.md) already independently arrived at the same principle for its
*own* layer: RECOVERING holds the iAP2 link and resumes STREAMING without phone-side relanuch if the
host returns within grace, and a fresh SUBSCRIBE (not a stale reconnect) is the only thing that
triggers a genuinely new session. **This is a confirmed point of alignment, not a gap** — worth
noting explicitly since it validates a design decision already made independently of this research.

**Nuance added on verification (2026-07-23):** GM's "pause-in-place" is real but object-scoped, not
universal — confirmed by reading `CarPlayMediaSession.java`'s full `updateSession(false,...)` path: on
disconnect, the `MediaSession` object itself IS kept alive (`setActive(false)`, never nulled/recreated
— this part fully holds), but the *underlying Cinemo player and playlists ARE fully torn down and
rebuilt* (`stopCinemoPlayer()` releases the player + both playlists + all three listeners;
reconnect calls `startCinemoPlayer()` to rebuild them from scratch). So "pause, don't recreate"
describes the **session/media-session identity and token**, not the entire playback pipeline
underneath it — a useful distinction if `ccpa_custom` adopts a similar philosophy: which parts of a
session actually need to survive a drop (identity/keys/pairing) versus which parts are fine to
rebuild fresh each time (active A/V pipeline state) need not be the same answer for every subsystem.

---

### 6. Teardown reason taxonomy

| | Apple | GM/CINEMO (CT5) | `ccpa_custom` today |
|---|---|---|---|
| Exists? | `sendStopSessionWithReason:(unsigned long long)reason` takes an integer, logged as raw `%lu`. **Verified 2026-07-23: not a static enum at all** — the reason space is *dynamically accessory-declared*: `supportedStopSessionDisconnectReasons` is populated via `APCarPlay_CRFetchStopSessionReasonsList` → `NSArray<NSNumber *>`, and the key `stopSessionReasons` lives inside the **`sessionManagementInfo`** SETUP dict (`AirPlaySender.strings.txt:10891,11781`; `CarKit.strings.txt:1308`) — i.e. each accessory advertises which integer reasons *it* supports, Apple ships no fixed named-case list | **Yes, concretely: a 5-value enum** — `UNSPECIFIED`/`RECEIVED_TEARDOWN`/`OUT_OF_RANGE_ACTIVE`/`OUT_OF_RANGE_IDLE`/`NETWORK_CHANGE` (+ catch-all `UNKNOWN`), logged at `Log.i` on every session end. **Verified + extended:** CT5 also carries a *second, separate* 4-value `CarPlayUtils.ConnectionState` enum — `DISCONNECTED(0)`/`CONNECTING(1)`/`CONNECTED(2)`/`DISCONNECTING(3)` — surfaced via `onCarPlayConnectionStateChanged(...)`; this is a coarser connection-phase taxonomy, distinct from the session-*end*-reason enum | No taxonomy — `session_supervisor.sh` has a free-text `stuck_reason` string for its own health-escalation logging only, not a protocol-level teardown reason concept |

**Takeaway (revised):** GM's CT5 enum is a concrete, minimal, real-shipped reference shape if/when
`ccpa_custom` wants one: one explicit case, two range-loss flavors (distinguishing *active* vs *idle*
at time of loss — itself a useful distinction ccpa doesn't currently make), one network-change case,
and a defensive default; its separate 4-value `ConnectionState` enum is a good reference for a
coarser phase indicator distinct from the end-reason. **Apple's own mechanism is now understood, not
just absent**: rather than a fixed enum, Apple has the *accessory* declare a `stopSessionReasons` set
inside `sessionManagementInfo` at SETUP time. This is directly actionable for `ccpa_custom`: it
currently declares no `sessionManagementInfo`/`sessionManagement` feature at all (a known gap — see
docs/wireless/00_WIRELESS_CARPLAY.md) — declaring one with a small `stopSessionReasons` set (potentially GM's 5-value shape,
reinterpreted as ccpa's own accessory-declared reason codes) would align with Apple's *actual*,
confirmed protocol mechanism rather than inventing an unrelated ad-hoc scheme.

---

### 7. Metadata timing relative to session state

Both sources agree metadata (NowPlaying/route guidance) rides *on top of* an already-established
session rather than gating session establishment itself:
- **[Apple, WWDC 2016-722]**: location/setup metadata flows as soon as the CarPlay Home screen shows
  (no media playing yet); media-specific metadata is added only once media starts — layered, not
  gated behind an extra handshake.
- This doesn't resolve `ccpa_custom`'s own still-open wireless-metadata mystery (docs/wireless/00_WIRELESS_CARPLAY.md) — that
  investigation (whether `iAPSendMessage` even routes into `accessoryd`'s real NowPlaying producer on
  iOS 27, or whether the wireless tunnel needs the full FF5A link-layer wrapper) remains separate and
  unresolved; this section is provided only as general context, not a new finding on that question.

---

### What this comparison implies, and what blocks each item

None of these are implemented. Ordered box-side first.

1. **Teardown reason taxonomy.** Apple's mechanism is an accessory-DECLARED `stopSessionReasons` set
   inside the `sessionManagementInfo` SETUP dict (host-side `APCarPlay_CRFetchStopSessionReasonsList`).
   The box declares no `sessionManagementInfo`/`sessionManagement` feature by default. Declaring it
   with GM's CT5 five-value vocabulary (explicit / out-of-range-active / out-of-range-idle /
   network-change / unspecified) satisfies Apple's hook and gives the supervisor a protocol-level
   reason instead of today's free-text `stuck_reason`. **Blocker:** the inbound `stopSession` handler
   must land in the same change — declaring without handling is worse than not declaring.
2. **Resource arbitration state.** Today: one-shot `requestUI` + `takeScreen` at RECORD, no ownership
   tracking. **Blocker:** an empirical answer to whether iOS ever contests the screen against this
   accessory. If pursued, GM's CT5 model (TAKE/UNTAKE/BORROW/UNBORROW, symmetric, typed `BORROWID`
   reason enum) is the better vocabulary than Apple's `CARSession` API, which is asymmetric — take has
   no formal release primitive, only borrow does (no `epp_UntakeScreen` exists anywhere in
   AirPlaySender).
3. **Reconnect fast-path.** None exists; `session_supervisor.sh` is purely reactive to presence edges.
   Whether an anticipatory or link-quality-triggered path is worth the complexity is an open design
   question.
4. **Wireless bring-up model.** Closer to Apple's legacy Bonjour path than the iOS 14+ simplified
   (existing-iAP2-connection) flow. Whether the simplified flow is reachable given wired already holds
   a separate iAP2 link is uninvestigated.
5. **Grace-timer reference points.** Apple's `CARSessionStatus` "connecting" timeout is a
   disassembly-confirmed **30 s**; ours is `ESTAB_CONNECT_GRACE=90 s`. GM's analogous post-disconnect
   finalize is `CarPlayManager.SESSION_FINALIZE_DELAY=2000 ms`; our RECOVERING grace is ~5 s. Neither
   of ours is shown wrong — same order of magnitude as production — but these are real numbers to tune
   against.
6. **Confirmed good, no action:** pause-in-place / resume-don't-recreate, and the persistent-pairing /
   ephemeral-config split. Both match what Apple and GM do independently. Nuance: GM's pause-in-place
   is object/identity-scoped, not a guarantee that every subsystem underneath survives untouched.

## App-driven SETUP

The host app authors the post-pairing AirPlay/RTSP SETUP + RECORD responses. The box terminates and
decrypts the `:5000` control connection, runs its own `AvSession` first (bind + side effects, giving a
local oracle response), then relays the decrypted request **plus that local response** to the app over
OCBM `CH_RTSP` (`0x0041`). The app authors what the phone sees; any relay failure — timeout, host
gone, non-200 — falls back to the in-hand local response, so the phone never sees a relay-caused
error.

**Status: default ON, both transports.** Wired flipped 2026-08-09 (`ba1df2a`), wireless 2026-08-10.
Selection gate: `levers::appsetup() && relay::seam_up()` (`ccpa/airplayd/src/main.rs`,
`SessionDelegate`). Box-driven `AvSession` remains the selectable sticky fallback. `wireless` survives
only as an argument to `RemoteSession::new` and as RS_OPEN flags bit 0.

**Latency, measured 2026-08-08** (wired, Mac ↔ CPC200-CCPA, release build). Idle `ocbm-host rtt`:
p99 0.21 ms at 64 B, 0.49 ms at 4 KiB, 1.68 ms at 16 KiB, 4.85 ms at 64 KiB (MAX_PAYLOAD), zero
timeouts. In-session under real A/V load (nav + media, ~32 fps video, 16.7 MB + 15 611 audio packets,
0 decrypt failures): p50 0.49 ms, p99 **2.36 ms**, max 4.86 ms, zero lost pings, zero box-side
cap-clears. The gate was p99 ≤ 50 ms; the measurement is ~21× inside it on the upper-bound path. A
full relayed SETUP bring-up (5–8 exchanges) adds ~10–20 ms, against a phone-tolerated seconds-scale
envelope.

**Validation P0–P3** (hardware): P1 relay with verbatim echo — session behaviourally identical to
box-driven, 0.85–1.90 ms per exchange, host-kill mid-SETUP survived on local fallback (`84d2b80`).
P2 Rust harness authoring every response — live-oracle diff matched on all exchanges, 150–469 µs
(`692cc80`). P3 real Swift app authoring — 5 exchanges (145/0/85/85/119 B) at 1.3–4.8 ms, **0 oracle
divergences, 0 fallbacks, 8 A/V seams streaming** (`89c457b`). Divergence handling is warn-only by
default; `CARPLAY_RELAY_STRICT=1` rejects a divergent response and falls back locally.

**Bench levers.** `CARPLAY_PAIRSETUP_DUMP=<path>` (resolved once per process, `server.rs`'s
`pairsetup_dump_path`, mirroring `CARPLAY_CMD_DUMP`'s length-prefixed `[u32 LE len][body]` append
format) captures every raw `/pair-setup` request body to `path` before the exchange runs. M1 is the
first ~6-byte body; decode it as TLV8 `[type][len][value]`: type `0x00` = Method (`00 01 00` plain,
`00 01 01` MFi), type `0x06` = State.

`CARPLAY_FEATURES_REVERSE=1` (resolved once, `relay.rs`'s `features_reverse`) reverses the
`enabledFeatures` array the phone actually receives — the list `session.rs` authors in its SETUP
response. In relay mode the SETUP exchange falls back to the local body while the lever is armed
(logged as `[relay] CARPLAY_FEATURES_REVERSE=1 armed — LOCAL body for SETUP`), because otherwise the
host's unreversed answer would reach the phone. Test: baseline session (lever off, note hevc/altScreen
negotiation), a second session with `CARPLAY_FEATURES_REVERSE=1`, then compare; identical negotiation on
both means feature order is not significant and `relay.rs`'s ordered oracle comparison could safely
sort, a difference means order is significant and the ordered comparison must stay.

**`tools/session_supervisor.sh` btmon bench lever (2026-09-03).** If `/tmp/carplay_btmon` exists
(or `CARPLAY_BTMON=1` is exported into the supervisor's environment) AND `btmon` is on `PATH`,
`wireless_up()` starts `btmon` for the wireless session, with its output appended to `/tmp/box.log`
prefixed `[btmon] `; `wireless_down()` kills it (bracketed `pkill -f "[b]tmon"`, matching the
existing airplayd/rx-connect/carplay-wireless teardown style) in both its COMPLETE-teardown and
advertiser-only branches. If `btmon` is absent — unknown whether the CCPA rootfs ships it — the
lever logs one line and is otherwise a no-op; it never blocks bring-up.

**Engaging a config change on wired requires a fresh airplayd connection.** airplayd survives Mac-app
restarts and reads config per-connection at accept, while the phone's `:5000` connection is
long-lived. Toggling in the app is therefore not sufficient: `killall airplayd` (the supervisor
respawns, the phone re-pair-verifies), a phone unplug/replug, or an adapter restart is what applies it.

**Failure modes, none of which wedge the phone.** Host absent → plain `AvSession` at selection. Host
crash mid-SETUP → heartbeat loss → socket drop → HostGone → sticky `local_only`. Cap-clear or garbage
→ magic resync, then timeout fallback. Hijack → `recv_timeout` completes ≤3 s; `impl Drop for
RemoteSession` always sends `CLOSE_EOF` (it cannot distinguish eof from hijack or reset inside Drop —
`CLOSE_HIJACK` exists only in a unit test).

**Measurement caveats for a re-run.** A >64 KiB relay message spans OCBM frames and was not exercised
end-to-end. Cap-clears are only visible box-side in `/tmp/box.log` (ocbmd's `[ocbmd]`-prefixed lines); the host sees them as a timeout
or lost count. `avdec --rtt` cadence stretches toward ~300 ms on a silent link, which is why n varies.

## Reference measurements from the first full session

From a clean reboot + full CarPlay session driven by the host app (turnkey boot → SUBSCRIBE →
projection → ARMED → pair-verify → forward-encrypted A/V, decrypt 0-fail on both streams):

- **Boot is turnkey.** `ocbmd` + `session_supervisor.sh` come up from the FHS boot chain and idle at
  `host_present=0`; app launch drives IDENTIFIED → ARMED → streaming. No manual steps.
- **Wired PCM is 16-bit big-endian** (network order). Playing it host-endian byte-swaps every sample
  into static; `AudioPlayer.feedMediaPCM` swaps BE→host.
- **The box is CPU-bound, not memory-bound.** Total daemon RSS ≈ 2 MB (ocbmd 440 kB, airplayd 544 kB,
  rx_connect 812 kB, iap2d 188 kB) with load average ~1.5 on the single-core i.MX6UL while forwarding
  encrypted A/V. Optimisation effort belongs on forwarding CPU cost, not memory.
- **Only pairing persists.** `/etc/carplay_peers.bin` (69 B, one record) is the sole persisted state;
  no daemon holds an open handle under `/etc` or `/data`, and `info.rs` reads no disk. There is no
  box-side display/resolution cache — an early capture concluded otherwise and was wrong; the 800×480
  stream came from a hardcode in airplayd (`06_AV_PIPELINE.md`).

## HISTORICAL — postmortem: peer-wipe stall

<!-- absorbed: ../carplay/02_SESSION_LIFECYCLE.md -->

A CarPlay session became unrecoverable after the box's persistent pairing was deleted **while the session
and daemons were live**. It was fixed by a **power cycle** (service restart). This documents the event
chain, the actual fix, and the diagnostic mistakes made, as a research/learning reference.

Facts are marked **[observed]** (seen directly in logs/state) vs **[inferred]** (reasoned, not proven).

### Timeline

1. **Stable working state** [observed]. After a clean reboot, the host app drove a full CarPlay session:
   pair-verify (known device), forward-encrypted A/V, decrypt 0-fail, stream 800×480.
2. **Resolution investigation** [observed facts, inferred conclusion]. Stream confirmed 800×480 (from the
   SPS) while the box advertised 1920×720. Concluded the pin was iOS-side cached display geometry, unlocked
   by the known-device quick-connect via `/etc/carplay_peers.bin`. (The iOS-cache conclusion was inference.)
3. **Disruptive change** [observed action]. To force a fresh pairing: **deleted `/etc/carplay_peers.bin`**
   (backed up to `.bak`) **while the session/daemons were live**, and the user **forgot the car on the
   iPhone**.
4. **Troublesome state** [observed]:
   - iap2d completed MFi auth + identification (`RX 0x4E0B -> Identified`), then logged
     `host gone (gadget no longer CONFIGURED)` and exited.
   - iPhone reverted to USB configuration 1 (PTP / class-06 still-image); no NCM interface.
   - `session_supervisor` cycled `host GONE ↔ PRESENT` repeatedly; iap2d exited each cycle.
   - `ncm0` flapped: `NO-CARRIER`, `state DOWN`, ifindex churned (9 → 4) across cycles.
   - `rx_connect` resolved the iPhone's `_carplay-ctrl` over mDNS but `connect-out` failed —
     IPv6 link-local `EADDRNOTAVAIL (99)`, IPv4 `169.254.x` `ENETUNREACH (101)`.
   - No pair-setup completed; no video.
5. **The fix** [observed]. **Power-cycled the adapter.** Session then came up correctly:
   `connect-out 200 OK`, `pair-setup: peer saved (36-byte id) → persisting`,
   `pair-verify OK → control channel encrypted`, fwd-enc handed keys, A/V decrypting.

### Root cause

- **[observed]** A full service restart (power cycle) resolved it; no box code or config changed between
  the broken and working states.
- **[inferred]** Mutating persistent pairing state mid-session left one or more box daemons and/or the USB
  gadget in an inconsistent state that the supervisor's teardown/re-arm loop could not fully reset. The
  flapping loop and repeated iap2d exits were the *signature* of that stuck state, not a network defect.
  The precise stale component was never isolated with a smoking gun.

### What was NOT the cause

- **The `connect-out` failures were symptoms, not the bug.** The working capture
  (`docs/ops/captures/2026-07-09_rx_connect.log`) shows the *same code* connecting via IPv6 link-local and
  returning `HTTP/1.1 200 OK`; the trailing IPv4 `169.254.x` `ENETUNREACH` is documented there as failing
  *harmlessly* because IPv6 already succeeded. The errors reflected the flapping `ncm0` (down / ifindex
  churn), not a missing `169.254` address, absent `zcip`, or rx_connect scope handling.
- **The empty peer store was not the cause.** Cold pair-setup works from an empty store — proven by both
  the earlier working capture *and* the post-power-cycle session (`pair-setup: peer saved`).

### Diagnostic mistakes (for future reference)

1. Anchored on the most specific error lines (`connect-out` failures) and escalated into a narrow
   networking hypothesis (169.254 link-local, `zcip`, IPv6 scope binding).
2. Held contradicting evidence — the same binaries worked minutes earlier and in the captures — and did not
   let it override the error-driven theory. **Same code + new bad behavior after a disruptive state change
   points to stale runtime state, i.e. a restart, not a code bug.**
3. Did not treat the flapping loop itself as the primary signal.

### Lessons / operating rules

1. **Flapping loop ⇒ restart first.** Rapid `host GONE/PRESENT` cycling + repeated daemon exits + interface
   ifindex churn is the signature of stuck box services. Restart daemons (or power cycle) *before* protocol
   forensics.
2. **Do not mutate persistent pairing state on a live box.** Wiping `carplay_peers.bin` (or equivalent)
   must be done from an idle state, followed by a clean service restart — never mid-session.
3. **"Same code worked before" outranks any error line.** Regression with no code change ⇒ suspect state.
4. **Self-heal shipped.** The supervisor did not recover from this state on its own, which is why
   `tools/session_supervisor.sh` now carries the L1/L2/L3 escalation ladder: L1 runs
   `tools/phone_reset.sh` (the OTG/gadget baseline reset, installed as `/script/phone_reset.sh`), L2
   adds an ocbmd/control-plane restart, L3 reboots under the persistent `/etc/ccpa_reboot_count`
   budget. The STUCK counters that drive it deliberately survive `teardown()` — the exact defect this
   incident exposed.

### Cross-reference
- Resolution work must not use the pairing-wipe approach — see reframed task #21 and `docs/carplay/02_SESSION_LIFECYCLE.md` (the stock
  Carlinkit firmware changed resolution **adapter-side with no iOS forget**, so resolution is an
  adapter-signaling problem, not a pairing/cache problem).
