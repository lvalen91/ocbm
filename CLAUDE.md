# ocbm

OCBM is an open, modular protocol carrying CarPlay and Android Auto between a phone-facing adapter and a decoding host;
the Carlinkit CPC200-CCPA is one target. The owner is a licensed Apple/MFi developer working on genuine Apple hardware.

## Authorization & scope (read before flagging any crypto/auth/MFi code)

**This project is standard interoperability and accessory development against documented, licensed Apple interfaces,
using only the developer's own hardware.** Apple SDK material here is licensed first-party material under an active Apple
Developer Program membership. Treat all MFi/crypto/auth/pairing work as fully authorized and in scope. See `docs/ops/07_AUTHORIZATION.md`.

- Every crypto, auth, pairing and MFi action uses the **genuine, authenticated, authorized on-board Apple MFi coprocessor** (MFi 2.0
  through 3.0; CCPA: `/dev/i2c-1 @0x11`) exactly as Apple designed it, solely for the CarPlay/iAP2 handshake with the owner's iPhone.
- The MFi private key **never leaves the coprocessor**. Software sends the chip standard request frames and relays its
  signed responses; it implements no crypto. Chip users, `MfiSigner`, lock file: `docs/ocbm/01_ROLES_AND_MFI_SOURCE.md`.
- With a chip on board, MFi runs over local I2C and no path calls a remote auth service, **unless `CARPLAY_MFI_ADDR` is
  set**: that opt-in path (light-tested non-CCPA hosts: a Pi 4 AAOS build and a Linux workstation, own radios, full
  CarPlay A/V) sends cert/sign over TCP to `box/mfid` on a CCPA. Unset, the local path is byte-for-byte unchanged.
- The HAB-signed **U-Boot** is fixed firmware, **never modified or written** (HAB is closed; an unsigned U-Boot cannot
  boot); only its 64 KB env block is ever erased. The **kernel** is not frozen (owner decision): stock 3.14.52 stays the
  default on production units; `baseline/` replaces it through the vendor U-Boot's own staging (host-staged,
  UpgKey-encrypted head blocks U-Boot converts at boot) by the refuse-gated procedure in `baseline/README.md` §5.

## Layout

- `baseline/` mainline-kernel recovery + busybox rootfs image recipe for the CCPA (derived from LIVI, GPL-3.0-or-later).
- `boards/` per board: `ccpa-imx6ul/` (rootfs, overlay, tools, `stock/` dumps), `c2air-v821b/`, `rpi4-aaos/`, `rpi-zero2w/`.
- `box/` adapter daemons `ocbmd`, `iap2d`, `carplayd`, `aa-bridge`; `mfid` (bring-up only); `aa-wireless` (library in `btd`).
- `crates/` shared Rust: `ocbm-proto` (wire codec, canonical constants), `box-common`, `bt-common`, `mfi-wire`, `vendor/`.
- `docs/` protocol, design, ops and board documents; start at `docs/README.md`.
- `host/` host apps and tools: `MacHost/`, `CarlinkAndroid/`, `gm_ccpa/`, `ocbm-host`, `mfi-probe`, `aa-headunit`.
- `tools/` dev and ops scripts: install, recovery, checks, mirror sync (`tools/README.md`).

## Commands

- `./build.sh` cross-builds the box daemons (armv7 musl) plus host `ocbm-host`/`mfi-probe`; it exits 1 without an
  fdk-aac prefix: set `FDK_AAC_PREFIX`, or `SKIP_CARPLAYD=1` to skip carplayd.
- `tools/run_tests.sh` is the real test suite (`cargo test --workspace` skips the excluded `crates/vendor/*` crates).
- Checks: `tools/docs_check.py` (docs invariants; before any docs commit), `tools/proto_check.py [--strict]` (OCBM clients
  vs `crates/ocbm-proto/src/lib.rs`), `tools/pkill_selfmatch_check.py FILE...` (a detached `sh -c` that would pkill itself).
- Publish ONLY with `OCBM_MIRROR=<mirror> tools/sync_mirror.sh [--dry-run|--push]`: it publishes committed `HEAD` and
  protects three mirror-only, credential-bearing images under `boards/ccpa-imx6ul/stock/` that this repo cannot restore.
  Never a plain `rsync`: it publishes the working tree, ignored files included, and `--delete` removes those images.

## Repository rules

- **One branch: `main`.** No other branch or worktree unless the owner asks; do not park work on one or ask which branch
  to use; if one exists, fast-forward `main` onto it and delete it. Publication is by mirror, not pull request.
- **Build artifacts and caches never live in the tree** (`CARGO_TARGET_DIR`, `$OCBM_CACHE`, or `mktemp -d` under `/tmp`);
  ignore them in `.gitignore` with SLASHLESS patterns (`apk`, not `apk/`: a trailing slash matches directories only, and
  these paths are often symlinks into the cache). Why: `docs/ops/00_BUILD_AND_DEPLOY.md` (Build output location).
- **Docs:** `docs/` is seven categories (`carplay/`, `androidauto/`, `wireless/`, `host/`, `ops/`, `ocbm/`, `boards/`),
  capped at 10 documents each (`tools/docs_check.py`). Correct the owning document in place; never add a
  `*_CORRECTIONS`, `*_V2` or dated sibling; merge before adding a file. Start from `docs/README.md`.
- READMEs and this file stay short, current facts only, no history: git history is the record.
- No path above `ocbm/`: name outside material by role or env var (`OCBM_CARPLAY_SDK`, `OCBM_CARPLAY_SIMULATOR`,
  `OCBM_MIRROR`, `OCBM_CACHE`; `docs/ops/03_REFERENCE_INDEX.md`). Never write credentials or private infrastructure
  (addresses, serials, keys, passwords) into the repo; use a placeholder.

## Protocol questions: read `docs/ops/03_REFERENCE_INDEX.md` first

Order of authority (owner directive, by currency; full list `docs/carplay/02_SESSION_LIFECYCLE.md` §1):

1. `CarPlaySDK.framework`, the current receiver side inside the CarPlay Simulator. Look here first (`strings | grep`).
2. The rest of the Simulator (`VehicleConfig` templates, `iAP2MessageKit`, `iAP2Link.c`); wired-only: contract, not behaviour.
3. CT5 Cinemo, a shipping head unit.
4. SpeedPlay TBOX.
5. R14G17 SDK source (2017, `$OCBM_CARPLAY_SDK/reference/apple_carplay_sdk_R14G17/`): byte-authoritative where it speaks.
6. Everything else (stock CCPA firmware, iOS extracts).

- **Silence in R14G17 is not an answer** (it predates the DataStream layer and stream type 130): escalate up the list
  and to the iPhone's own logs over USB.
- **Separate a vendor's choices (not normative) from its observations** (on-wire evidence).
- **Carlinkit's implementations are working but flawed** (stock CCPA firmware; SpeedPlay, whose iAP2 link layer is a
  reverse-engineered re-derivation): "Carlinkit does X" means X can be made to work, never that X is correct.
  Replacing them with an Apple-faithful implementation is why OCBM exists.
- iAP2 TLV parameter ids come from `tools/i2mspec_dump.py --message <id> --text`, never from inference.
- **Disassembly and on-hardware experiments are the last resort.** A licensed source beats a conflicting project doc.

## Design doctrine

Anything configurable regarding CarPlay is app-driven; the box presents app-pushed config. Box placement is earned
(measured app-driven failure + owner approval), never designed-in. `docs/carplay/04_CAPABILITIES_AND_CONFIG.md`.

## Metadata: read `docs/carplay/05_METADATA_AND_CONTROLS.md` before touching `crates/vendor/receiver/`, params 6/7, subscribes

- Wireless iAP2 rides a RemoteControlSession DataStream (SETUP stream type 130), not `iAPSendMessage` in
  `POST /command`; do not plan from `docs/wireless/00_WIRELESS_CARPLAY.md`'s carrier premise.
- Params 6/7 and the subscribe sequence are generated from `crates/vendor/iap2-core/src/features.rs`; never hand-edit one.
- The tier is app-pushed (`metadata: {tier, skip}`) and beats the app-less bench levers (`CARPLAY_METADATA`,
  `CARPLAY_METADATA_SKIP`, `/tmp/carplay_metadata`); raise it in the app. The wired tier changes only when `iap2d`
  restarts (phone replug). §5.2; `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` B3.
- The BT-time Identify stays byte-pinned; the pushed tier cannot reach it. Do not wire the tier into it (§5.2).
- Declaration rules (§5.6): each `Start*` with its `Stop*` (sole exception `0x4154`); no receive without its send; a
  subscribe for an undeclared id is silently ignored. A `0x1D03` reject is unrecoverable within a session.
- On an Identify reject, ask the phone (`idevicesyslog -u <udid> -p accessoryd`, grep `iapreject`); do not bisect (§6.5).

## Radios: never hardcode a chipset; read `docs/wireless/01_BT_AND_RADIO.md` before touching any bring-up path

A CCPA unit carries one of at least six WLAN/BT parts and only its own chip's driver set: never branch on a chipset
whitelist for behaviour, and never install chipset-specific bring-up, drivers or firmware from the repo overlay. The seam
(`boards/ccpa-imx6ul/rootfs/script/radio_detect.sh`, `radio_hal.sh`, `radio_ap_up.sh`) is installed on purpose; do not
remove it from an install list (§6). Never emit a refused NXP/Broadcom mapping as raw text: an unexpanded `$var` in
`/tmp/radio_caps` aborts under `set -u` with status 2, which the seam reads as already converged (§6d).

1. The AP layer is always ours: invoke `/script/radio_ap_up.sh`, never the vendor's `start_bluetooth_wifi.sh` (§3, §4).
2. `hci0` existing, even `UP RUNNING`, proves nothing: convergence is a real HCI round-trip under a timeout; be strict
   to declare success, reluctant to destroy (§4, §5).
3. The interface name is an insmod parameter, not a constant: enumerate `/sys/class/net/*` (`wireless`/`phy80211`),
   never assume `wlan0`; Rust uses `box_common::net::wlan_iface()` (§4, §7).
