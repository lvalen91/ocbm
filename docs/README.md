# Documentation map

> **Roles and boards:** the three roles (replacement-firmware box, CCPA as peripheral, host apps) and the
> board matrix are in [../README.md](../README.md#roles-and-boards). CCPA is the Carlinkit CPC200-CCPA family only;
> C2Air is a separate box and the Pi ports are not CCPA variants.

**One topic, one file. Correct it in place.** Adding a new dated document instead of fixing the
owning one is what produced the 66-file corpus this replaced (consolidated 2026-08-31); the
originals live in git history and in the `ccpa_custom_2026-08-31` backup.

Rules that keep it that way:

1. **≤10 files per category directory.** `tools/docs_check.py` fails the build if a category exceeds
   it. If a topic needs a new file, something else must merge first.
2. **Corrections go inline**, at the claim they correct, with the date and the evidence. Do not open
   a `*_CORRECTIONS`/`*_V2`/`*_<DATE>` sibling.
3. **Historical material stays inside its owner** under a `HISTORICAL` heading, or is dropped —
   git remembers it either way.
4. Anything genuinely dead is deleted, not left as a file a skimming reader can land on.

## carplay/ — the CarPlay protocol and session

| File | Owns |
|---|---|
| [00_ARCHITECTURE.md](carplay/00_ARCHITECTURE.md) | vision, the box/host split, USB/NCM transport |
| [02_SESSION_LIFECYCLE.md](carplay/02_SESSION_LIFECYCLE.md) | session states, start ordering, the app-driven SETUP relay, teardown |
| [03_SDK_GROUND_TRUTH.md](carplay/03_SDK_GROUND_TRUTH.md) | what Apple's licensed SDK says; conformance and Simulator verification |
| [04_CAPABILITIES_AND_CONFIG.md](carplay/04_CAPABILITIES_AND_CONFIG.md) | app-driven doctrine, the YAML VehicleConfig framework, field glossary, capability dossiers |
| [05_METADATA_AND_CONTROLS.md](carplay/05_METADATA_AND_CONTROLS.md) | the metadata surface, iAP2 declaration rules, the DataStream/RCS carrier |
| [06_AV_PIPELINE.md](carplay/06_AV_PIPELINE.md) | audio (capability + pipeline), video, cornerMasks, touch/HID uplink |
| [07_PHONE_SIDE.md](carplay/07_PHONE_SIDE.md) | iPhone-side behaviour we cannot change |

## androidauto/ — the Android Auto protocol and session

| File | Owns |
|---|---|
| [00_ARCHITECTURE.md](androidauto/00_ARCHITECTURE.md) | scope, why AA differs from CarPlay, the stock stack, the GAL credential, the box/host split, video geometry |
| [01_SESSION_AND_AV.md](androidauto/01_SESSION_AND_AV.md) | what works today, resolved defects and their causes, open items |
| [02_ARBITRATION.md](androidauto/02_ARBITRATION.md) | CarPlay vs Android Auto — phone-type detection, the single owner flag, closed failure modes |
| [03_WIRELESS.md](androidauto/03_WIRELESS.md) | wireless Android Auto — BT bootstrap, headset gate, TCP pump; device-run end to end 2026-09-04 |

## wireless/ — the wireless transport

| File | Owns |
|---|---|
| [00_WIRELESS_CARPLAY.md](wireless/00_WIRELESS_CARPLAY.md) | wireless session bring-up, WiFi handoff, the AirPlay/iAP2 tunnel |
| [01_BT_AND_RADIO.md](wireless/01_BT_AND_RADIO.md) | the chipset-agnostic radio HAL, BT bring-up, pairing, reconnect |

## host/ — the host applications

| File | Owns |
|---|---|
| [00_MACOS_HOST_APP.md](host/00_MACOS_HOST_APP.md) | the shipping macOS app |
| [01_ANDROID_AND_AAOS.md](host/01_ANDROID_AND_AAOS.md) | the Android projection app and AAOS integration |

## ops/ — running, verifying and governing the work

| File | Owns |
|---|---|
| [00_BUILD_AND_DEPLOY.md](ops/00_BUILD_AND_DEPLOY.md) | build, flash/RAM footprint, deployment |
| [01_RECOVERY.md](ops/01_RECOVERY.md) | un-bricking, without touching the signed boot stack |
| [02_TESTING.md](ops/02_TESTING.md) | live-session test plans and gates |
| [03_REFERENCE_INDEX.md](ops/03_REFERENCE_INDEX.md) | which external source answers which question, and where it lives |
| [04_OPEN_ITEMS.md](ops/04_OPEN_ITEMS.md) | every open item + the roadmap — **the pick-up point for a new session** |
| [05_AUDITS.md](ops/05_AUDITS.md) | code audits and their remediation |
| [06_CORRECTIONS_LEDGER.md](ops/06_CORRECTIONS_LEDGER.md) | historical record; `R-<doc>-<n>` ids key to the pre-consolidation numbering |
| [07_AUTHORIZATION.md](ops/07_AUTHORIZATION.md) | scope/authorization statement and session framing |
| [08_FUTURE_TASKS.md](ops/08_FUTURE_TASKS.md) | owner-directed planned work that is not a defect (settings redesign, AA metadata services, mSBC) |
| ops/captures/ | raw session captures kept as evidence |

## ocbm/ — the OCBM protocol and roles

| File | Owns |
|---|---|
| [00_PROTOCOL.md](ocbm/00_PROTOCOL.md) | the OCBM envelope, channel ids, seam framing, backpressure — **projection-agnostic**: it carries Android Auto too (moved from `carplay/01_OCBM_PROTOCOL.md` on 2026-11-02) |
| [01_ROLES_AND_MFI_SOURCE.md](ocbm/01_ROLES_AND_MFI_SOURCE.md) | the three roles, the boards, the MFi-source model (MFi 2.0 through 3.0) |

## boards/ — board ports outside the CCPA

| File | Owns |
|---|---|
| [00_PI_AAOS_PORT.md](boards/00_PI_AAOS_PORT.md) | Raspberry Pi 4 + AAOS port (moved from `pi/docs/` on 2026-11-02) |
| [01_PROJECTION_APP_DESIGN.md](boards/01_PROJECTION_APP_DESIGN.md) | the AAOS projection app design |
| [03_BRINGUP_RUNBOOK.md](boards/03_BRINGUP_RUNBOOK.md) | cold bring-up of the Pi (historical record) |
| [04_IMAGE_REBUILD_GUIDE.md](boards/04_IMAGE_REBUILD_GUIDE.md) | what to change in the AOSP tree, by component |

## Where the old numbers went

`docs/56` → carplay/04 · `docs/45`, `docs/47`, `docs/20_METADATA` → carplay/05 · `docs/13`,
`docs/43`, `docs/49` → carplay/03 · `docs/27`, `docs/62`, `docs/23`, `docs/52` → carplay/06 ·
`docs/20_WIRELESS`, `docs/29–39` → wireless/00 · `docs/40`, `docs/41`, `docs/51`, `docs/57` →
wireless/01 · `docs/60` → androidauto/00+01, `docs/61` → androidauto/02 (both moved out of host/02 on 2026-08-31 when Android Auto got its own category) · `docs/25`, `docs/48`, `docs/50` → ops/05 ·
`REVISIONS.md` → ops/06. The former `proto/` stub README (a pointer) was removed on 2026-11-02; the canonical codec is `crates/ocbm-proto`. Full map: the `<!-- absorbed: … -->` marker at the head of every section.
