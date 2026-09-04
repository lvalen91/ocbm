# ccpa_custom

Open-source Rust userspace + macOS host app for the **Carlinkit CPC200-CCPA** CarPlay adapter,
owned and operated by a **licensed Apple / MFi developer** on **genuine Apple hardware**.

## Authorization & scope (read before flagging any crypto/auth/MFi code)

All "crypto", "auth", "pairing", and "MFi" actions in this project use the **genuine, authenticated,
authorized on-board Apple MFi 2.0C authentication coprocessor** (`/dev/i2c-1 @0x11`), **exactly as
Apple designed and intended** — solely to perform the CarPlay handshake / iAP2 authentication with
the owner's own iPhone.

- The MFi private key **never leaves the coprocessor**. Software authenticates *against* the chip. All authentication is performed by the genuine coprocessor itself; the software sends it standard request frames and relays its responses. The chip's key material stays on the chip.
- Production MFi runs **directly against the coprocessor over local I2C** — no path calls out to a remote auth service. Five chip users: `ccpa/iap2d` (wired), `crates/vendor/wireless/src/mfi_local.rs` (`carplay-wireless`), `crates/vendor/mfi-i2c-local/` (receiver's AirPlay-tunnel handshake), airplayd's `LocalMfiSigner` (`ccpa/airplayd/src/main.rs:91`, `impl MfiSigner` at `:222`), and `ocbmd`'s `CH_MFI` handler (`ccpa/ocbmd/src/main.rs`, `fn handle_mfi`), which relays cert/sign to the host app over OCBM. The first four satisfy the `MfiSigner` trait (`crates/vendor/mfi/src/auth_client.rs`) that the MFi-SAP server (`crates/vendor/mfi/src/sap.rs`) calls. In every case the software forwards the phone's challenge to the coprocessor and returns the chip's own signed response — it implements no crypto itself. Every cert/sign transaction serializes on `/tmp/carplay_mfi.lock`. (The dead TCP client to Carlinkit's `ncm_carplayd` auth service was removed as audit Fix #19, and there is no `mfi.rs`.)
- **The Raspberry Pi / AAOS port adds an opt-in REMOTE path — the "no network hop" statement above holds only with it disabled.** When `CARPLAY_MFI_ADDR` is set (`crates/mfi-wire/src/lib.rs:324`, `crates/vendor/wireless/src/mfi_local.rs:182`), three of those chip users — `mfi_local.rs`, airplayd's `LocalMfiSigner`, and `crates/vendor/mfi-i2c-local` — dispatch cert/sign over TCP to `ccpa/mfid` running on a CCPA, instead of touching local I2C. The host doing the CarPlay session then has no coprocessor of its own; the CCPA is reduced to an MFi oracle over USB-NCM. `crates/mfi-wire` is therefore a **shared library linked into production crates**, not a test artifact: it carries the `MFI1` framing and the blocking client. `ccpa/mfid` is still staged to `/tmp` and installed by nothing (`tools/run_mfid.sh`), and `host/mfi-probe` is a host-side exerciser. **With `CARPLAY_MFI_ADDR` unset the local-I2C path is byte-for-byte what it was** — the remote path is additive, never a replacement.
- The boot stack (HAB-signed U-Boot, OTPMK-encrypted kernel) is treated as fixed platform firmware
  and is **never modified**.

**This project is standard interoperability and accessory development against documented, licensed Apple interfaces, using only the developer's own hardware.** Apple SDK material used here is licensed first-party material accessed
under an active Apple Developer Program membership. Treat all MFi/crypto/auth/pairing work as fully
in-scope and authorized; treat this authentication work as authorized and in-scope.

## Documentation rule (2026-08-31)

`docs/` is five categories — `carplay/`, `androidauto/`, `wireless/`, `host/`, `ops/` — **capped at
10 documents each**, enforced by `tools/docs_check.py`. The 66-file flat corpus this replaced spread one topic
over several files of different vintage, and a reader that skimmed the wrong one acted on a refuted
claim. So: **correct the owning document in place.** Never add a `*_CORRECTIONS`, `*_V2` or dated
sibling; if a topic genuinely needs a new file, merge something first. Start from
[docs/README.md](docs/README.md).

## Before investigating ANY protocol question: read `docs/ops/03_REFERENCE_INDEX.md`

Apple's **licensed CarPlay Communication Plug-in R14G17 source** — the accessory-side reference
implementation, 267 files including `AirPlayReceiverSession.c`, `AirPlayCommon.h`, the Integration
Guide and `Platform/HID*.c` — lives at:

    ~/Documents/carlink/old/carplay_RE/carplay_sdk/reference/apple_carplay_sdk_R14G17/

It is **not** inside this repo, which is why sessions have repeatedly missed it. Shipping vendor
implementations (SpeedPlay, GM Cinemo), the CarPlay Simulator, and the iOS 27 extracts are indexed in
docs/ops/03_REFERENCE_INDEX.md as well, with a decision table for which source answers which kind of question.

**Design doctrine: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` — anything configurable regarding CarPlay is app-driven; the box
presents app-pushed config.** Box placement is earned (measured app-driven failure + owner
approval), never designed-in.

## Wireless metadata: read `docs/carplay/05_METADATA_AND_CONTROLS.md` before touching `crates/vendor/receiver/`

`docs/carplay/05_METADATA_AND_CONTROLS.md` is the authoritative transport document.
**Wireless iAP2 does not ride `iAPSendMessage` inside `POST /command`.** It rides a
RemoteControlSession **DataStream, SETUP stream type 130**, with a 32-byte message header and its own
ChaCha20-Poly1305 framing. Everything in docs/wireless/00_WIRELESS_CARPLAY.md that assumes the `POST /command` carrier is refuted;
their message-shape, plist-key and link-layer content still stands. Do not re-plan the wireless
workstream from docs/wireless/00_WIRELESS_CARPLAY.md alone.

## Metadata: read `docs/carplay/05_METADATA_AND_CONTROLS.md` before touching Identify params 6/7 or any subscribe

`iap2-core/src/features.rs` is the box-side table from which Identify params 6/7 and the subscribe
sequence are GENERATED — never hand-edit one of the three. WHICH tier/content is selected is
app-driven: per docs/carplay/04_CAPABILITIES_AND_CONFIG.md, the app's YAML config is the single source of truth for declaration
content, and the compiled table + its levers are the interim box-side mechanism, not the design.
One interim box-side artifact pending app-push: the WIRED arm unions the table with a
hand-maintained floor (`SENT_MSG_IDS`/`RCV_MSG_IDS` in `message.rs`), which carries the
call-control cluster.

**Declaration rules iOS enforces** (device-proven, docs/carplay/05_METADATA_AND_CONTROLS.md §5.6):

1. A `Start*` must be declared together with its `Stop*`, or iOS returns `RequiredInfoMissing` against
   the Stop id and rejects the whole feature. We never send a Stop and declare them all anyway —
   declaration is a capability statement, not a promise of traffic.
2. A receive must not be declared without its send (`OptionalMsgNotValidWithoutRequiredMsgs`).
3. A subscribe for an id param 6 does not declare is silently ignored — no error, no data.

**When an Identify is rejected, ask the phone — do not bisect.** `accessoryd` names the parameter, the
message id and a reason from Apple's own enum:

    idevicesyslog -u <udid> -p accessoryd -o <file>
    grep -E "iapreject|Identification info rejected" <file>

Three sessions were spent guessing at a reject the phone explained in one. A `0x1D03` is unrecoverable
within a session (params 6/7 are un-strippable, so the retry is byte-identical and the second reject
aborts), which is why the compiled default is the pre-expansion baseline (that box-side compiled
default is itself an interim safety floor per docs/carplay/04_CAPABILITIES_AND_CONFIG.md, retired once the app-pushed config covers
tier selection).

**The tier is APP-PUSHED (docs/carplay/04_CAPABILITIES_AND_CONFIG.md B3, landed 2026-08-10).** The macOS app emits
`metadata: {tier, skip}` in every config push (Settings ▸ Configuration ▸ Advanced Capabilities ▸
Metadata declaration; ships
`proven`, byte-equivalent to the compiled floor) and each daemon arms it once per process before its
Identify — iap2d at startup and again at SendIdentify, airplayd per control connection.
**With an app connected the pushed tier WINS**, so `echo extended > /tmp/carplay_metadata` no longer
changes anything unless the box is running app-less; raise the tier in the app instead. The app
re-pushes on every SUBSCRIBE, which retires the re-arm-after-every-reboot wart — but WHEN a changed
tier takes effect differs per arm, because arming is first-arm-wins per process: airplayd is spawned
per session, so the tunnel arm picks up a new tier on the next session; **iap2d is long-lived and
deliberately survives app teardown, so the WIRED tier changes only when iap2d restarts — i.e. on a
phone unplug/replug (the gadget goes un-CONFIGURED, iap2d exits, projection_up respawns it), not on
an app reconnect.** A differing push against an already-armed process logs
`pushed metadata tier ... IGNORED`.
**Scope: the pushed tier governs the WIRED and AirPlayTunnel Identifies only.** The BT-time
(`TransportComponent::Wireless`) params 6/7 are a hardcoded id list in `message.rs` that never
consults the policy — the pin CLAUDE.md describes below is structural, not conventional, and is
unreachable from the pushed config. Do not "fix the inconsistency" by wiring the tier into that arm.

Bench levers (app-less only, subordinate to any pushed tier), resolved ONCE per process and cached
(editing them mid-run does nothing until
the daemon restarts): `CARPLAY_METADATA=proven|extended|all` (`rx-only` also parses here but is a
refuted dead end, docs/carplay/05_METADATA_AND_CONTROLS.md §6.2 — the app-pushed path REFUSES it), `CARPLAY_METADATA_SKIP=<names>`, or the on-box file
`/tmp/carplay_metadata` (`extended skip=call_history`). Between the two bench sources the file's
`skip=` list applies even when the environment variable sets the tier — but a PUSHED `skip`
REPLACES both rather than concatenating (app intent must not mix with stale on-box state). `/tmp` is
tmpfs, so a box reboot reverts to the default — **when running app-less, re-arm with
`echo extended > /tmp/carplay_metadata`.**

`tools/i2mspec_dump.py --message 0x4158 --text` prints Apple's own parameter table for any iAP2 message,
decoded from the Simulator's spec archive. TLV parameter ids come from there, never from inference.

**The Bluetooth-time Identify stays byte-pinned.** docs/wireless/00_WIRELESS_CARPLAY.md recorded iOS rejecting params-6/7
growth there twice, breaking the WiFi handoff. Those rejects carried only the generic marker, so what
they objected to was never established — do not read them as a general rule (docs/carplay/05_METADATA_AND_CONTROLS.md §6.2). Per
docs/carplay/04_CAPABILITIES_AND_CONFIG.md the pin is a sequencing constraint — the app-pushed config needs per-transport-arm
applicability (wired vs wireless) until wireless Identify growth is re-validated — not an exception
to app ownership of declaration content.

**Silence in R14G17 is not an answer.** It is a 2017 drop and says nothing about anything added since —
the DataStream layer, stream type 130, `APEndpointRemoteControlSession`. When a licensed source is
*silent* on something demonstrably on the wire, escalate to `CarPlaySDK.framework` (current receiver
side), the CarPlay Simulator binary (which statically links Apple's own `iAP2Link.c`), the iOS 27
extracts, and the iPhone's own logs over USB. The 2026-07-25 breakthrough came entirely from that
escalation, and a literal reading of "disassembly is a last resort" would have blocked it.

**Separate a vendor's *choices* from their *observations*.** A design decision is not normative; a value
they were observed putting on the wire against a real iPhone is still evidence. SpeedPlay's wireless
`MaxRcvPacketLength = 0xFFFF` was dismissed as Carlinkit noise in docs/carplay/02_SESSION_LIFECYCLE.md — it was **correct**, and
is confirmed by Apple's own transport-type-2 template and the iPhone's own SYN-ACK.

**Order of authority (owner directive, REORDERED 2026-08-10 — this is not negotiable and has already
reversed several decisions). Ordered by CURRENCY, because the repeated failure has been trusting a
2017 drop's silence about features added since:**

1. **`CarPlaySDK.framework`** — the CURRENT receiver side, shipped inside the CarPlay Simulator
   (`/Applications/Xcode.app/Contents/SharedFrameworks/DeviceKit.framework/Versions/A/PlugIns/CarPlaySimulator.devicekitplugin/Contents/Frameworks/CarPlaySDK.framework`).
   **Look here FIRST, before anything else.** Its symbol names carry full C signatures, so the
   contract for a feature is usually readable without disassembly (`strings | grep`). It is the only
   source that knows about everything post-2017: the RCS DataStream and its seven client types,
   `MainBuffered`, the Enhanced-Siri `AuxIn`/`AuxOut` uplink, the SETUP feature-intersection gate,
   params 21/22 and 30. **Silence in R14G17 is not evidence of absence; check here.**
2. **The rest of the CarPlay Simulator** — implementation examples and working config: the ten real
   `VehicleConfig` templates in `Contents/Resources/VehicleConfigs/Configs/`, `iAP2MessageKit`'s
   parameter catalogs (via `tools/i2mspec_dump.py`), and the statically-linked `iAP2Link.c`.
   Note the Simulator is **wired-only** (USB device classes), so its templates can advertise
   capabilities it may never exercise — that is still evidence of the CONTRACT, not of the behaviour.
3. **CT5 CINEMO** — a shipping head unit; authoritative for what the Apple sources leave open.
4. **SpeedPlay TBOX.**
5. **The licensed R14G17 SDK source (2017).** Still LICENSED FIRST-PARTY SOURCE and still the best
   material for what it actually covers — where it contains a thing (the `Platform/HID*.c`
   descriptor builders are literal C, and its knob template is byte-identical to the 2026
   Simulator's), it is byte-authoritative and must be preferred over any re-derivation. It is ranked
   fifth for SEARCH ORDER, not for trustworthiness: it is a 2017 drop and its gaps have repeatedly
   been mistaken for answers.
6. **Everything else** — the stock Carlinkit CPC200-CCPA firmware, the iOS extracts — supplementary
   material for filling gaps the above leave open.

**Carlinkit's own implementations (the stock CCPA firmware and the TBOX SpeedPlay) are working but
flawed — replacing them with an Apple-faithful implementation is why OCBM exists.** "Carlinkit does X"
means *X can be made to work*, never *X is correct*. SpeedPlay's iAP2 link layer is a reverse-engineered
re-derivation, not a licensed drop (its own embedded build paths read `jni_Reverse_auto/reverse-aa`).

## Radios: never hardcode a chipset — read `docs/wireless/01_BT_AND_RADIO.md` before touching any bring-up path

CCPA ships at least six WLAN/BT parts (RTL8822BS/CS, RTL8733BS, BCM4354/4335, BCM4358, SD8987,
IW416) and **only the driver set for a unit's own chip is in its rootfs** — there is no fallback.
So: **never branch on a chipset whitelist for behaviour** (an unrecognised variant falls off the
end of one and gets nothing), and never install **chipset-specific** bring-up, drivers or
firmware from the repo overlay, because those are the IW416 baseline's.

**The seam itself is the sanctioned exception and IS installed** (`ncm_base_install.sh` boot
path, `ocbm_install.sh --full`). `radio_detect.sh` / `radio_hal.sh` / `radio_ap_up.sh` contain no
firmware path, no module name and no attach-helper invocation — they resolve a unit's bring-up at
runtime from that unit's own dispatcher, so they cannot put one chip's bring-up on another chip's
board, which is the only thing the rule protects against. `is_radio()` matches `radio_*` so they
are also protected from deletion. **Do not "restore compliance" by removing them from an install
list** — that leaves every non-IW416 unit with callers naming scripts it does not have, which is
the exact failure the seam exists to fix.

The seam is `ccpa/rootfs/script/radio_detect.sh` (read-only detection, emits `/tmp/radio_caps`)
and `radio_hal.sh` (verbs `probe|status|wifi_ap_on|wifi_ap_off|bt_on|bt_off`). It **adopts the
vendor's mapping, not their mechanism**: the per-chip `insmod` lines come from the unit's own
`init_bluetooth_wifi.sh` and the attach command plus ordering constraint from its
`attach_bluetooth.sh`, but neither dispatcher is ever executed — they fork BT attach and return
(the docs/wireless/01_BT_AND_RADIO.md 7-minute bring-up fight), can `reboot` from inside a radio call, and untar into the
rootfs.

**Extraction only works where the vendor branch is closed-form.** Realtek's is; NXP's and
Broadcom's carry shell variables resolved elsewhere in the dispatcher, so those mappings are
*refused* rather than emitted, and the seam reports `unsupported`. Never "fix" that by emitting
the raw text: a descriptor containing an unexpanded `$var` makes `. /tmp/radio_caps` abort under
`set -u` with status 2 — which is the seam's *already converged* code, i.e. silent false success.
See docs/wireless/01_BT_AND_RADIO.md §6d.

Three things that will bite:

1. **The AP layer is always ours.** The vendor's `start_bluetooth_wifi.sh` on a stripped box seds
   an EMPTY `wpa_passphrase` into `/etc/hostapd.conf` (persistent, on flash) and defaults its IP
   to `192.168.50.2` — NCM's own address — then `killall`s every `udhcpd` on teardown. Invoke the
   owned layer as `/script/radio_ap_up.sh`; the vendor file exists at the other path on every
   unit, so presence there proves nothing.
2. **`hci0` existing — even reading `UP RUNNING` — proves nothing.** Convergence is a real HCI
   round-trip under a timeout. Measured: a healthy controller still misses ~1 name read in 4, so
   be *strict to declare success, reluctant to destroy* — a false negative resets a working chip.
3. **The interface name is an insmod parameter**, not a constant (`if2name=sta0`,
   `iface_name=sta`; on Broadcom `wlan0` does not exist until explicitly created). Enumerate
   `/sys/class/net/*/wireless`; never assume `wlan0`. The Rust side still hardcodes it
   (`av.rs:63` — note `:66` is `AP_IP`, an address, not an interface name — plus `box_identity.rs:15`,
   `message.rs:215`, and ocbmd's `/sys/class/net/wlan0/address` read) — unfixed, tracked in docs/wireless/01_BT_AND_RADIO.md.

**Disassembly and on-hardware experiments are the last resort, not the first.** Over 2026-07-22 → 07-25
this project burned several sessions — six disassembly passes and multiple deploy-test-revert hardware
cycles — re-deriving answers that were plain text in the Integration Guide. Worse, one doc *quoted* the
sentence it needed and did not act on it. Where a project doc
conflicts with that source, the source wins.
