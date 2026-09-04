# Bluetooth bring-up, pairing and the radio HAL

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 57, 51, 41, 40; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

**Contents:** the chipset-agnostic radio HAL → accessory-initiated reconnect → the HCI event-mask regression → the bring-up race and its health check.

## Radio HAL — chipset-agnostic bring-up

<!-- absorbed: ../wireless/01_BT_AND_RADIO.md -->

Status: **wireless CarPlay demonstrated end-to-end on an RTL8822CS unit (2026-08-15)** — a
chipset with no backend in this repo — over a SoftAP and Bluetooth controller brought up
entirely from command lines extracted from the unit's own vendor dispatcher. Detection, mapping
extraction, driver load, BT attach with convergence, the owned SoftAP, and the supervisor wiring
are all landed and hardware-validated. Read §7 for what is still open; Broadcom remains untested
and is the known-hardest case.

### 1. The problem, stated precisely

CCPA ships with at least six WLAN/BT parts. Per the resources repo's
`01_Firmware_Architecture/device_variants_and_conversion.md`, **only the driver tarball for a
unit's own chip is in its rootfs** — there is no fallback set. Delete a variant's WLAN files and
its radios are gone until it is reflashed.

The project's owned bring-up scripts (`wlan_on.sh`, `bt_on.sh`, `attach_bluetooth.sh`,
`wlan_off.sh`) are hardcoded to NXP IW416: `/lib/firmware/nxp/iw416_ko.tar.gz`,
`insmod mlan.ko/moal.ko`, `fw_loader_linux` + `hciattach`.

**This is the problem as it stood before the seam landed.** `tools/session_supervisor.sh` called
those scripts by literal path at four sites. On a Realtek unit the files do not exist, so
wireless bring-up **failed silently** while wired projection kept working — invisible because
the calls run inside a detached `setsid sh -c` whose output is redirected and whose exit status
is never examined. Those four sites now call `radio_hal.sh` (`session_supervisor.sh` —
`wifi_ap_on` and `bt_on` inside `wireless_up`, `wifi_ap_off` at both `wireless_down` teardown
branches); the old paths survive only in comments recording why they changed.

#### Why "it's Linux, it should be generic" does not apply here

Genericity on a normal distro is manufactured by four things this platform lacks:

1. **In-tree drivers.** `88x2cs.ko` (Realtek SDIO), `bcmdhd.ko` (Broadcom FullMAC),
   `moal.ko`/`mlan.ko` (NXP) are out-of-tree vendor forks. Realtek's SDIO 8822 parts are still
   not mainline; `bcmdhd` is Broadcom's out-of-tree driver, not mainline `brcmfmac`.
2. **udev/modalias auto-binding.** Normally the device advertises an ID and udev loads the
   matching module unattended. There is no such rule for these vendor modules, so *something*
   must read the SDIO ID and choose. That something is shell.
3. **cfg80211/nl80211 as one API.** That layer is what makes `wlan0` mean the same thing
   everywhere. These drivers each invented their own naming knob instead — `if2name=`,
   `iface_name=`, `uap_name=`.
4. **A standard firmware loader.** Each vendor ships a tarball with its own layout, extracted to
   `/tmp` at boot.

Busybox is not the issue — it is a userspace toolbox and provides `ifconfig`/`ip` fine. The gap
is below it, in a 2014 kernel (3.14.52) with no in-tree support for any of these parts.

**The consequence that shapes the design:** once the driver is loaded and the controller
attached, everything above genuinely *is* generic — `hci0`, `hciconfig`, BlueZ mgmt, `hostapd`
all behave uniformly, and an audit of the Rust sources found **no hard chipset dependency
anywhere**. So the chipset-specific region is exactly the span where mainline's abstraction
would have applied and doesn't. The HAL is not inventing an abstraction; it is rebuilding the
missing one, at precisely that boundary.

### 2. The rule

**Never branch on a chipset whitelist for behaviour.** An unrecognised variant falls off the end
of a `case` and gets no path at all. This is already `tools/ncm_base_install.sh`'s governing
constraint and it applies here unchanged. A chip-name table for *logging* is fine; nothing may
act on it.

### 3. Adopt the vendor's mapping, not their mechanism

Every unit ships `/script/init_bluetooth_wifi.sh` — the vendor's own SDIO-ID dispatcher — plus
`attach_bluetooth.sh`. `ncm_base_install.sh` deliberately preserves them (`is_radio()` protects
them by explicit basename), and its comments already anticipate this seam: on non-IW416 units the
vendor scripts "are KEPT and become the on-demand radio path".

Separate the vendor's **observations** from their **choices** (CLAUDE.md doctrine):

* **Observations — adopt verbatim.** The per-chip `insmod` lines with their parameters, the
  attach helper with its baud and protocol, the firmware tarball path, and the *ordering
  constraints* (on 0xc822 and SD8987 the BT attach must wait for the WLAN driver or the chip
  wedges). These are fleet-deployed and working, and are not re-derivable by us for parts we do
  not own.
* **Choices — do not inherit.** Executing the dispatcher wholesale re-imports:
  - `/script/attach_bluetooth.sh &` — **fork-and-return**, which is exactly the uncoordinated
    double-bring-up docs/wireless/01_BT_AND_RADIO.md recorded fighting itself for 7+ minutes;
  - the Broadcom branch backgrounding `brcm_patchram_plus` the same way;
  - the SD8987 branch's `dmesg | grep "woal_request_fw failed" && reboot`, a reboot inside a
    radio bring-up that would bypass `session_supervisor`'s escalation ladder and its persistent
    reboot budget;
  - `tar -xvf … -C /` overlays written into the rootfs per boot;
  - a BT health check that is object existence, which a dead chip passes.

**The AP layer is always ours — never delegate it.** On a stripped box the vendor's
`start_bluetooth_wifi.sh` reads config through `riddleBoxCfg`, which the OCBM base removes. The
calls are *not* `test -e`-guarded, so with it gone that script seds an **empty `wpa_passphrase`
into `/etc/hostapd.conf`** — persistent, on flash — and falls back to `WLANIP=192.168.50.2`,
which is NCM's own address, then repoints the DHCP pool onto the management subnet. Its teardown
`killall`s every `udhcpd`, including NCM's. That is the control channel you are standing on.

### 4. The two scripts

#### `ccpa/rootfs/script/radio_detect.sh` — read-only, emits `/tmp/radio_caps`

Two phases, because the useful facts split cleanly by cost:

* **Cheap, pre-driver (milliseconds):** SDIO `(vendor, device, class)` via a **glob** of
  `/sys/bus/sdio/devices/*/` — never the vendor's hardcoded `mmc0:0001:1`, which encodes a host
  index and an RCA that are enumeration accidents. Firmware tree, `*_ko.tar.gz`, attach-helper
  presence, backend tier.
* **Post-load:** the interface name and MACs, obtained by **enumerating** `/sys/class/net/*` for
  `wireless`/`phy80211` — never by assuming `wlan0`.

The mapping is extracted with **no chipset table**: intersect the modules in *this unit's own
tarball* with *this unit's own dispatcher's* `insmod` lines. A part nobody anticipated still
resolves, because every unit ships a dispatcher that knows its silicon and a tarball that names
its modules.

Two extraction details that only testing revealed:

* Anchor the `insmod` match on `/tmp/` — the dispatcher's success/failure `echo` strings also
  contain `insmod <module>`, and a greedy `sed` picks the echo.
* Anchor the attach-command match at line start, or it matches the `if [ -e … ]` guard above it.

##### The SCO/HFP lines are extracted PER BRANCH, and that is not the same problem (2026-09-03)

`RADIO_BT_SCO_MTU_CMD` and `RADIO_BT_SCO_ROUTE_CMD` join the descriptor, carrying the vendor's own
post-attach SCO setup so `sco_on` can put it back after the daemon's controller reset. The insmod
and attach lines are discriminated *for free* — by which modules the unit's tarball holds and which
attach helper it ships. **The SCO lines are not.** They appear in several chipset branches with
different contents:

| branch | `scomtu` | routing command |
|---|---|---|
| `0xb822` / `0xc822` / `0xb733` (Realtek) | `hciconfig hci0 scomtu 240:32` | **none** — Realtek routes SCO over HCI by default |
| `0x4354` / `0x4335` (BCM4354) | none | none |
| `0x4358` / `0xaa31` (BCM4358) | `hciconfig hci0 scomtu 240:32` | `hcitool -i hci0 cmd 0x3f 0x1c 0x01 0x02 0x00 0x00 0x00` |
| `0x9149` / `0x9141` / `0x9159` (NXP) | `hciconfig hci0 scomtu 240:32` | `hcitool -i hci0 cmd 0x3f 0x1d 0x00` |

A whole-file `head -1` would hand one chip another chip's vendor-opaque HCI command — the
no-chipset-whitelist rule violated from the other side. So `attach_branch()` selects the branch by
**this unit's own SDIO id** against the dispatcher's own `if/elif` chain: still no table, still no
whitelist, and an id the dispatcher never mentions yields nothing, so the seam reports `unsupported`
honestly. Three details the extraction needs:

* **Count `if`/`fi` depth.** The NXP branches close an inner `if [ $configBurned -eq 0 ]` *before*
  their `scomtu` line, so "stop at the first `fi`" drops the very lines we came for.
* **A single-branch attach script is its own branch.** The owned IW416 rewrite has no `sdioCardID`
  dispatch at all; falling back to the whole file is correct there and only there.
* **Strip trailing comments.** The owned rewrite annotates inline
  (`hcitool … 0x3f 0x1d 0x00   # route SCO to HCI`) while the vendor comments the line above. `#`
  only starts a comment in the shell's *input* — the result of a variable expansion is never
  re-scanned — so a captured trailing comment reaches `hcitool` as four extra positional arguments.

The routing command is recognised by the **vendor's own annotation** ("route sco data to hci", same
line or the one above), never by matching raw opcodes, which would be a chipset table in disguise.

Verified against every branch of the stock 2025.10 `attach_bluetooth.sh` and against the owned
rewrite; an unknown SDIO id refuses both lines.

#### `ccpa/rootfs/script/radio_hal.sh` — the seam

Verbs `probe | status | wifi_ap_on | wifi_ap_off | bt_on | bt_off | sco_on`.
Exit `0` converged now · `1` ran and failed · `2` already converged · `3` unsupported on this
unit. Single-flight `mkdir` locks per subsystem; state published atomically to
`/tmp/.radio_wlan_state` and `/tmp/.radio_bt_state`.

Three contract clauses, each bought with a real failure:

1. **Convergent on return, never fork-and-return.** When an `_on` verb returns 0 the radio is up
   *and responsive* and nothing it started is still in flight.
2. **Responsiveness, not existence.** Convergence is a real HCI round-trip (read the local name
   back under a timeout), because `hci0` can exist — and even read `UP RUNNING` — with a chip
   that answers nothing.
3. **Never reboot, never touch session daemons.** A backend that believes only a reboot can
   recover says so with an exit code and lets the layer that owns reboot policy decide. The HAL
   never signals `airplayd`/`rx-connect`/`carplay-wireless`/`ocbmd` — it cannot know whether a
   live session is wired-owned, which is what the supervisor's conditional reap (docs/wireless/00_WIRELESS_CARPLAY.md #1.4)
   exists to decide.

**Naming is load-bearing.** The supervisor's teardown runs in a detached `sh -c` whose own argv
its own `pkill` patterns can see — a bug this project already paid for once (2026-08-01: a plain
`pkill -f` killed the teardown subshell before it reached `wlan_off`, orphaning a still-beaconing
AP). So the script name and every verb must avoid `airplayd`, `rx-connect`, `carplay-wireless`,
`ocbmd`, `hostapd`. Audited clear; re-audit before renaming a verb.

**The owned AP layer is invoked as `/script/radio_ap_up.sh`, never
`/script/start_bluetooth_wifi.sh`.** Every unit already ships a vendor file at that second path,
so testing it for existence proves the file is there, not that it is ours — and running the
vendor copy is the destructive case in §3.

### 5. Hardware-validated results (RTL8822CS, 2026-08-15)

Detection and mapping, on a chipset with **no repo backend**:

    RADIO_CHIP=realtek_rtl8822cs   SDIO vendor=0x024c device=0xc822 class=0x07
    RADIO_FW_TREE=/lib/firmware/rtlbt      KO=rtl8822_ko.tar.gz
    RADIO_WLAN_INSMOD="insmod /tmp/88x2cs.ko if2name=sta0;"
    RADIO_BT_LDISC_KO=rtk_hci_uart.ko
    RADIO_BT_ATTACH_CMD="rtk_hciattach -s 115200 ttymxc2 rtk_h5"
    RADIO_BT_AFTER_WLAN=1
    RADIO_BACKEND=mapped

Bring-up: `wlan0` appeared with a valid per-unit MAC (the address itself is not recorded here — same
rationale as commit 42f5052: this repo is public and a hardware address is trackable); `hci0`
converged in ~2 s and answered a real name read (`RTK_BT_4.2`, `UP RUNNING`, 57 events / 69
commands exchanged); the attach helper
survived the SSH session closing; `ncm0` and the management channel were untouched throughout.
`sta0` did **not** appear — `if2name=sta0` names the *secondary* interface, which requires
STA/P2P mode (no `wpa_supplicant` on this unit), so `wlan0` is genuinely primary on Realtek,
consistent with the vendor's BT wait being on `wlan0`.

#### Four defects found only by running it

1. **Presence ≠ ownership.** The first `wifi_ap_on` would have run the *vendor's*
   `start_bluetooth_wifi.sh`. Fixed by the unambiguous `radio_ap_up.sh` name. Verified after the
   fix: `hostapd.conf` still `wpa_passphrase=12345678`, `ncm0` still `192.168.50.2`.
2. **`UP RUNNING` is a lie.** A wedged controller reported `UP RUNNING` while every name read
   timed out — docs/wireless/01_BT_AND_RADIO.md reproduced live. The responsiveness check is what caught it.
3. **Reclaim the UART before re-attaching.** Attaching while the previous helper still held
   `ttymxc2` produced `OP_H5_SYNC Transmission timeout` → `Retransmission exhausts`. Correct
   recovery needs the helper killed, **the line-discipline module rmmod'd**, and the reset GPIO
   driven `1` → `0` (polarity matters; the other way is a no-op that looks right in the log),
   then re-insmod and re-attach, retried. With that, the HAL **recovers a wedged controller** —
   something the vendor path cannot do, since its existence-only check never notices.
4. **The probe is not a clean oracle.** Measured: this controller intermittently times out a
   name read, roughly one attempt in four, with the next second's attempt succeeding. This
   drives an asymmetry worth stating generally: **strict to declare success, reluctant to
   destroy.** A false negative on "is it already up?" does not merely misreport — it resets a
   working chip, which during a live session drops the phone. Decisions therefore use a spaced
   multi-attempt probe; poll loops keep the single-shot.

### 6. Where it installs, and why not in the OCBM installer

**The HAL belongs to the baseline conversion (`ncm_base_install.sh`), not to
`ocbm_install.sh`.** The baseline is where the vendor stack is stripped and the owned boot path
lands; the radio platform is a property of the *converted unit*, not of OCBM. OCBM merely
consumes it.

But `ocbm_install.sh --full` ships `session_supervisor.sh`, which is the HAL's *caller*. So the
OCBM installer must **assert** the seam exists on the target rather than assume it — which is
exactly the manifest cross-reference check §7 lists: for every shipped `*.sh`, extract its
absolute-path references and require each to be in the manifest, present on the target, or
explicitly declared optional. Run against the manifest **as it stood before the seam landed**, on a
Realtek unit, that check would have failed on `wlan_on.sh`, `bt_on.sh`, `wlan_off.sh` — catching the
real failure three phases before anything switched.

> **DO NOT READ THE NEXT SENTENCE AS "THIS CLASS OF FAILURE IS CLOSED." IT IS NOT, AND IT COST A
> FULL DAY ON 2026-08-28.** What was re-checked on 2026-08-16 is narrow: the supervisor no longer
> names `wlan_on.sh`/`bt_on.sh` (the old paths survive there only as a comment), and `--full` ships
> the seam. The *mechanism* that made those failures silent is untouched and is still live today:
>
> - `tools/session_supervisor.sh:792,794` (and again at `:865,894`) call
>   `sh /script/radio_hal.sh {wifi_ap_on,bt_on} >/tmp/{wlan,bt}.log 2>&1` inside a detached `setsid`
>   wrapper. **There is no `$?` check at any of those four sites.**
> - So if `/script/radio_hal.sh` or `/script/radio_detect.sh` is simply ABSENT from the target, the
>   shell writes `sh: /script/radio_hal.sh: not found` into `/tmp/bt.log` and the supervisor carries
>   on exactly as if the radios had converged.
> - **Nothing else notices.** The OCBM claim, `CT_HELLO_ACK`, the MFi relay (a real 945-byte
>   certificate and 128-byte signature), `CT_SUBSCRIBE` and `HOST_PRESENT` all still succeed,
>   because none of them touch the radio path. The box looks healthy from every angle the host can
>   see, and Bluetooth simply never exists.
>
> **If you are debugging "BT does nothing", do these two things first:**
> 1. `ls /script/radio_hal.sh /script/radio_detect.sh` on the target. A targeted `ocbm_push.sh` does
>    NOT install them — its default set is `ocbmd` + `carplay-wireless` — so a box can hold a
>    supervisor that depends on files it never received.
> 2. `cat /tmp/bt.log /tmp/wlan.log`. That is the ONLY place the failure is written.
>
> Then read `CT_BOX_HEALTH` bit 0 (`BH_HCI_PRESENT`), which since 2026-08-29 means `hci0` is UP via
> `HCIGETDEVINFO`. A health of `0x50` (`carplay-wireless|rootfs-ok`) with bit 0 clear is this fault's
> exact signature. Full account: `../ops/06_CORRECTIONS_LEDGER.md` `R-20W-5`.
>
> Two facts this document predates and does not otherwise mention: `hci_uart` is a **loadable
> module** on the CCPA, shipped inside `/lib/firmware/nxp/iw416_ko.tar.gz` — if nothing `insmod`s it
> the `n_hci` line discipline is never registered and `hciattach` fails `EINVAL`. And the box's
> `/tmp` is a **tmpfs on a USB-powered device**, so unplugging it to move it between bench and head
> unit IS a power loss: `/tmp/radio_caps` and any extracted `.ko` vanish on every move. `radio_hal.sh`
> now re-derives both, but any future step that caches into `/tmp` must assume it starts empty.

The narrow re-check: the supervisor names only `radio_hal.sh`, and `--full` now ships the seam. The
generic assertion is still unwritten — see §7, and note that `tools/ocbm_push.sh` now warns when the
supervisor is pushed to a box missing the seam, which is a guard at the point the mistake is made
rather than a check in an installer nobody ran.

That is the correct framing of what went wrong: **not a rule violation, a rule gap.** The WLAN
variant rule policed radio *files* in both directions but had no assertion over radio
*references inside non-radio files the installer ships*.

### 6b. Wireless CarPlay session, RTL8822CS, 2026-08-15

With the radios up through the seam and the macOS host app connected over OCBM:

    SUBSCRIBE sent (36848 B config) -> box session event: PRESENT
    phone ABSENT -> phone PRESENT (~15 s: association + handshake)
    [relay] RS_OPEN conn=1 wireless=true
    HEVC 1920x720, A/V decrypt video ok=690 fail=0, audio codec=1 48000Hz 2ch
    voice=33pps (Siri uplink live)
    100+ iAP2 metadata messages; three album artworks up to 141 KB
    steady state: 33-36 fps, jitter 24-30 ms, AVmon gaps=0 fails=0

**One transient worth chasing.** During establishment the video decoder produced bursts of
`Frame enqueued with decode failures — requesting keyframe to re-sync`, peaking at 136 in a
single second, with `hvcC` re-parsed repeatedly alongside them. It self-clears within ~90 s and
the session then runs cleanly, so it is a startup-latency annoyance rather than a session
killer. The shape — an IDR request following each failure, and a fresh `hvcC` parse following
each IDR — suggests the decoder session is being rebuilt against every re-sync request in a
brief feedback loop. Note the *decrypt* counters are clean (`fail=0`) throughout, so this is
strictly downstream of decryption.

**Why this session worked is also why it would not have on Broadcom.** `av.rs:63,66` pin
`WLAN_IFACE="wlan0"` and `AP_IP="192.168.43.1"`. On this Realtek unit both constants happen to
be correct — `wlan0` is primary and the owned AP layer serves 192.168.43.1 — so the Rust side
never noticed it was hardcoded. On a Broadcom unit the interface is `sta0` and `wlan0` does not
exist at all, so the identical session would fail with the radios up and perfectly healthy.
That makes §7's Rust item load-bearing risk rather than tidiness.

### 6c. What the box does with app-pushed config

The app pushes exactly three Wi-Fi keys — `wifi_ssid`, `wifi_pass`, `wifi_channel` — and
`apply_host_wifi_creds()` rebuilds `/etc/hostapd.conf` from a pristine `.stock` snapshot with
them (rebuild, not sed-substitute, because an SSID or passphrase can contain regex/delimiter
characters). What survives after that depends on the role:

| | bridge role (`wifi_ap:false`) | box-AP role (`wifi_ap:true`) |
|---|---|---|
| SSID | honored | **overwritten** with `ccpa-<4hex>` |
| passphrase | honored | honored (written only when absent or <8 chars) |
| channel | honored | honored (unless `/etc/wifi_use_24G` forces ch 6) |

The box-AP SSID overwrite is long-standing — the old `wlan_on.sh` did the identical `sed` — but
it sits awkwardly against docs/carplay/04_CAPABILITIES_AND_CONFIG.md's doctrine that configurable CarPlay state is app-driven and
the box presents app-pushed config. In box-AP mode the box currently names its own AP and the
app cannot override it. Left as-is for now because changing it would alter behaviour on the
IW416 baseline too, but it is a doctrine gap, not a design decision.

**Bluetooth: the app pushes no name at all.** There is no `bt_name` key; the box self-derives
`ccpa-<4hex>`. Naming is a *box* concern in both roles, so an app that wants to brand the BT
identity currently cannot.

#### One box, one name — the intent, and why it is NOT yet achieved

Wi-Fi and Bluetooth must advertise the same `ccpa-<4hex>`, and deriving it per-radio does not
achieve that. The sources disagree: on the RTL8822CS unit the Wi-Fi MAC and the BT controller
address share no octets at all (Realtek programs no `bd_addr`, so the controller uses its own
efuse), so the two derivations yield different suffixes. Which name a radio picked would depend on
which radio happened to be up when it was asked — and in the BT-only bridge role the WLAN driver is
never loaded at all, so Bluetooth would silently take the *other* name. It could also change between
boots, and the phone stores the name in its bonded record.

So the **shell** side resolves the identity once and persists it to `/etc/carplay_ident`:
`radio_hal.sh`'s `box_name()` writes it and passes it to `radio_ap_up.sh` as `RADIO_BOX_NAME`, and
`radio_ap_up.sh` re-reads the same file when invoked directly, so the SSID and the seam's BT name
come from one decision rather than two derivations. A placeholder is deliberately never persisted —
if every source fails the next call retries rather than freezing `ccpa-0000` into flash.

> **⚠️ CORRECTED: `/etc/carplay_ident` is NOT yet the single source of truth.** `radio_hal.sh` and
> `radio_ap_up.sh` are its only readers; `carplay-wireless`, `bt_on.sh` and `ocbmd` each derive a
> name independently, and `carplay-wireless` overwrites the advertised BT name every session — so
> the divergence §6c claims to have eliminated is still live. Closing it is tracked in §7, not here.
> Full detail, including the platform split (`hciconfig` on the CCPA, raw HCI only on the Raspberry
> Pi under `CARPLAY_HCI_BACKEND=native`) and the correction history behind it:
> [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-57-1`.

### 6d. What single-line extraction cannot represent — and why that was dangerous

The claim in §4 that "a part nobody anticipated still resolves" is **true only where the vendor
branch is closed-form**, and only the Realtek branches are. The NXP and Broadcom branches carry
shell variables resolved elsewhere in the dispatcher:

    insmod /tmp/moal.ko "mod_para=$nxpWiFiConfig"
    brcm_patchram_plus --patchram /lib/firmware/bcm/$bcmBTFirmware ... --bd_addr "$bcmBTMac" &

Captured verbatim those are not commands, only text shaped like one. And writing them into
`/tmp/radio_caps` made `. "$CAPS"` abort under `set -u` with **status 2** — which this seam
defines as *already converged*. Every verb died inside `caps()` before the backend switch,
reporting success, on **every NXP and Broadcom unit including the IW416 baseline**. Measured, not
theorised: sourcing a poisoned descriptor returns 2 on the box's own shell.

Fixed in two independent places, because either alone leaves a sharp edge: `radio_detect.sh`
refuses to emit a mapping it cannot faithfully execute (a trailing `&` is stripped; anything
still carrying `$`, a backtick or quotes becomes an empty mapping, so the seam reports
*unsupported* honestly), and `caps()` can no longer be killed by any descriptor. The usage-error
exit moved off 2, which it had been colliding with.

**So the honest scope of the design claim:** RTL8822BS/CS and RTL8733BS resolve on paper;
Broadcom and NXP currently resolve to an honest "unsupported" rather than to a wrong command.
Making those two families work needs the extraction to resolve the dispatcher's own literal
variable assignments within the unit's branch — tracked in §7, not claimed here.

### 7. Not yet done / not yet proven

* Broadcom and NXP mappings are **refused, not resolved** (§6d). Making them work requires
  slicing the dispatcher to the unit's own SDIO branch and resolving that branch's literal
  `var=value` assignments. Until then those units report `unsupported` — correct behaviour, but
  not working wireless.
* `RADIO_BT_PRELOAD_CMD` is consumed by `radio_hal.sh` but **never emitted** by
  `radio_detect.sh`, so a mapped-path unit needing `fw_loader_linux` before attach would never
  run it. Moot while IW416 resolves to the `owned` backend, latent otherwise.
* `ocbm_install.sh`: the generic manifest cross-reference assertion (§6). The specific Realtek
  failure it would have caught is now closed — `--full` ships the seam alongside the supervisor
  (`ocbm_install.sh:158-160`) — but a future shipped script referencing a path the target lacks
  would still slip through.
* A reversible radio verification gate in the baseline, run while still on NCM.
* Rust parameterisation: `WLAN_IFACE`/`AP_IP` from the seam (`av.rs:63,66`), the identity chain
  (`box_identity.rs`'s `hardware_id()` :15, `iap2-core/src/message.rs`'s `device_suffix()` :215 — a
  live bug on IW416 too, since `/sys/class/net/wlan0/address` is absent whenever the driver is
  unloaded, which under on-demand radios is most of the time), and the hardcoded Raspberry Pi BT MAC
  at `bt_driver.rs:61`. **Added 2026-08-16:** that same chain is why §6c's "one box, one name" is
  **not yet achieved**. `/etc/carplay_ident` is read only by `radio_hal.sh` and `radio_ap_up.sh`;
  `carplay-wireless` (`main.rs` → `bt_bringup::bring_up`), `bt_on.sh` and `ocbmd`'s `bt_name_from()`
  each derive independently, and because the supervisor execs `carplay-wireless` *after*
  `radio_hal.sh bt_on` the controller ends up advertising `CarLink-<suffix>` rather than the seam's
  `ccpa-<4hex>`. In the BT-only bridge role the suffixes can differ too, not just the prefixes.
  Closing it means the Rust side reading the ident file first — a behaviour change, so it is listed
  here rather than done.
* The startup decode transient described in §6b.
* **Broadcom is entirely untested** and is the known-hardest case: `wlan0` there does not exist
  until it is explicitly created on top of `sta0` (`iw dev sta0 interface add wlan0 type
  managed`). No Broadcom hardware is available to this project. RTL8733BS and SD8987 are
  likewise unexercised — the seam resolves them on paper from their own dispatchers, which is
  the whole design claim, but that claim is only measured on 0xc822 so far.

---

## Accessory-initiated BT reconnect

<!-- absorbed: ../wireless/01_BT_AND_RADIO.md -->

Goal: the box should reconnect a **known/bonded** iPhone to wireless CarPlay on boot with **no user
interaction** — the behavior native head units and the stock Carlinkit firmware both have. This doc
records what was tried on hardware, what was eliminated with evidence, and the one path left to
implement (**Model B**). It is the authoritative pickup point; do not re-derive the dead-ends.

---

### ✅ RESOLVED 2026-08-01 — Model B works, device-proven end to end

> **⚠️ The "on boot, with zero user interaction" TRIGGER is superseded; the mechanism is not.**
> Radios power on only on app command and off on app command or app loss; once on, auto-connect to
> a bonded device is correct. The re-gating is CLOSED — the guard is on `wireless_up` itself, and
> the `wired_iphone_on_usb` and per-call-site strategies both failed. Full reasoning:
> [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-51-1`–`R-51-3`.

The box now boots, autonomously pages the bonded phone, connects OUT, and brings up wireless CarPlay
with **zero user interaction**. Implemented in `crates/vendor/wireless/`: `rfcomm::connect_to` +
`sdp_client.rs` (new) + `reconnect.rs` (new) + `ssp_agent::bonded_addrs` + `main.rs` wiring. The
byte-pinned Identify and the accept path are untouched — purely additive.

**The one fact that unlocked it — the phone's iAP2 service UUID is NOT the accessory's.** Our
`sdp_server.rs` advertises the accessory-side iAP2 UUID `00000000-deca-fade-deca-deafdeca**caff**`.
Searching the *phone's* SDP server for that UUID returns an empty `35 00` — iOS does not expose it.
Browsing the phone's **full** SDP catalog (L2CAP-UUID `19 01 00` search, 1889 bytes) shows iOS
actually exposes its iAP2 endpoint as service **"Wireless iAP v2"** under UUID
**`02030302-1d19-415f-86f2-22a2106a0a77`** on **RFCOMM channel 1**. (Two red herrings sit next to it:
the accessory UUID ends `…caff`; iOS's *"Wireless iAP5"* service is `…deca**cafe**`, one byte off; the
real iAP2-v2 service is the unrelated `02030302…` UUID.) `sdp_client.rs` searches for that UUID,
reads channel 1, and `rfcomm::connect_to` opens it.

**iOS's exposed BR/EDR SDP catalog (device capture 2026-08-01), for reference:**

| Service | ServiceClass UUID128 | RFCOMM ch |
|---|---|---|
| **Wireless iAP v2** (this is the one) | `02030302-1d19-415f-86f2-22a2106a0a77` | **1** |
| Wireless iAP5 | `00000000-deca-fade-deca-deafdecacafe` | 1 |
| MAP MAS-iOS | (0x1132) | 2 |
| Phonebook (PBAP) | (0x112f) | 13 |
| Handsfree Gateway | (0x111f) | 8 |
| GATT / AAP Client / AVRCP / A2DP Source | — | — |

**Working flow captured on the box (`/tmp/wl.log`):**
```
[sdp-client] iAP2 RFCOMM channel on the phone = 1
[reconnect] RFCOMM connected OUT to the phone (ch 1) — starting iAP2 handshake
[bt-driver] SYN-ACK -- link up
[bt-driver] RX 0xAA00 -> CertSent ... AuthSuccess ... RX 0xAA05 -> Authenticated
[bt-driver] TX 0x1D01 IdentificationInformation (301 B, wireless transport)
[bt-driver] IdentifyAccept ... RX 0x1D02 -> Identified
[bt-driver] RX 0x5702 RequestAccessoryWiFiConfig -> replying 0x5703 (ssid="ccpa-b0df" ch=36 …)
[bt-driver] RX 0x4E0E DeviceTransportIdentifier: param0=<phone bdaddr>, param1=<phone UDID>
[av] started /usr/sbin/airplayd (detached) → transport=wireless → CarPlay active
```
Also learned, correcting two of this doc's eliminated-models notes: **an active SDP transaction TO the
phone holds the ACL** (Model A's idle L2CAP got reaped; a live ServiceSearchAttributeRequest does not),
and **the accessory drives everything** — iOS never self-initiates (accessoryd stays silent until the
outbound RFCOMM + iAP2 SYN arrives), which is why Models A and the bare page could never have worked.

Everything below is the pre-solution investigation, kept for the record.

---

### Ground truth
- The owner confirms native CarPlay AND the original Carlinkit CPC200-CCPA firmware autoconnect wireless
  on boot. So the feature is real and the mechanism is discoverable — per CLAUDE.md, "Carlinkit does X"
  means X can be made to work.
- Test rig this session: box BT bdaddr redacted (the box advertised `CarLink-b0df`); phone bdaddr and
  UDID redacted (iPhone18,4, iOS 27.0). Bond persists at
  `/etc/carplay/bt_link_keys` (25-byte record, JFFS2). iPhone connected to the Mac over USB with Apple's
  CarPlay/iAP2/BT/WiFi diagnostic profiles installed → `idevicesyslog -u <udid>` gives iOS's own view
  (this is the method that made the difference; keep using it).

  (Addresses redacted 2026-08-16, same rationale as commit 42f5052: this repo is public and a BT
  address is broadcast over the air and trackable.)

### What the working implementations do (stock Carlinkit + SpeedPlay, cited in the session research)
On boot, page the bonded phone by stored MAC, then **the accessory becomes the RFCOMM CLIENT and opens
the iAP2 channel TO the phone**, then drives a fresh iAP2 Identify (never a resume — CINEMO R25 §2.15).
Stock: `FastConnect`/`Bluetooth_ConnectStart` + "Page success | Outgoing page completed" +
`SDPToolSearchEnd` (SDP *client* search of the peer) + literal "BT paging and **RFCOMM connection to the
phone**". SpeedPlay `libCarplayJni.so`: `open_iap_rfcomm`, "start connect iap rfcomm", to `g_remote_addr`
(REMOTE_BTMAC). R14G17 SDK is Bonjour-only at this layer (2017 drop, silent on BT transport).

### Eliminated on hardware (do NOT retry these)
1. **Bare HCI page** (`hcitool cc` / raw Create_Connection). Page works — `DEVICE_CONNECTED`, iOS
   recognizes `SupS < WirelessiAP CarPlay >` and even starts to SDP-query the box — but the ACL is
   **channel-less and BlueZ idle-reaps it** (`DEVICE_DISCONNECTED reason=0x02`, local host) in ~seconds,
   before iOS finishes. iOS does **not** spontaneously start CarPlay from a bare inbound page.
2. **Model A: page + hold via an L2CAP connect to the phone's SDP PSM (0x0001).** Implemented
   `rfcomm::l2cap_connect` (raw `AF_BLUETOOTH`/`SOCK_SEQPACKET`/`BTPROTO_L2CAP`, `sockaddr_l2`,
   `SO_SNDTIMEO`); the connect pages + holds the ACL. iOS **accepts** the channel
   (`RecvConnectReq psm=1`), but then **sits completely idle for the full 12 s hold** — it never
   SDP-queries us, never opens RFCOMM to our server, never starts CarPlay — then the link drops when we
   release. **iOS will NOT self-initiate; the accessory must drive the connection. Model A is refuted.**
   (First cut also had a bug: `mark_connected` latched on our OWN outbound `DEVICE_CONNECTED` — same
   bdaddr — collapsing the hold to ~0 s; fixed to hold the full window, same negative result.)

### THE PATH LEFT — Model B (accessory drives RFCOMM to the phone)
The box must, per the stock firmware: (1) page the bonded phone (or let the RFCOMM connect page it
implicitly); (2) **SDP-query the phone** to discover its iAP2/EA RFCOMM service + channel — the box has
no `sdptool` (only `hcitool`/`hciconfig`), so implement a minimal **SDP client** over L2CAP PSM 0x0001
(the crate already hand-rolls an SDP *server* in `sdp_server.rs` — the encoding is symmetric); (3)
**RFCOMM-connect OUT to the phone's iAP2 channel** (add `rfcomm::connect_to(peer, channel)` mirroring
`open_listener`; the `l2cap_connect` scaffolding from this session — `sockaddr`/`connect`/`SO_SNDTIMEO`
pattern — is the reusable base); (4) hand the connected socket to the existing
`bt_driver::run(File, …)`, which already runs the full iAP2 Identify → 0xAA auth → 0x5702/0x5703 WiFi
handoff over any connected RFCOMM socket. Keep the server/accept-path 100% intact (needed for first-time
pairing and phone-initiated connects); Model B is purely additive. Do NOT touch the byte-pinned Identify
(`bt_driver.rs`/`message.rs`).

Open unknowns for the Model-B session (resolve with the iPhone-log rig):
- Does iOS expose an iAP2/EA **RFCOMM service** for the accessory to connect to on reconnect? The
  session's capture search was inconclusive. The SDP-client query in step (2) answers this directly —
  and if iOS exposes no such service, the stock "RFCOMM connection to the phone" may target a different
  service/UUID, or the whole flow may hinge on the SDP *search* itself keeping the ACL alive long enough
  for a different iOS action. Watch `idevicesyslog -p accessoryd -p carkitd` during each attempt.
- Whether an active SDP *transaction* (a real ServiceSearchAttributeRequest, vs. the idle channel that
  got reaped) is what holds the ACL and prompts iOS forward.

Design notes carried over: retry with bounded backoff (10→60 s), spawn only when bonds exist, keep the
accept-path as the fallback, and remember the "our own outbound connect fires `DEVICE_CONNECTED` for the
peer bdaddr" gotcha — the real connect-success signal is the **RFCOMM accept / iAP2 progress**, not the
mgmt `DEVICE_CONNECTED`.

### Status of the code
The Model-A `l2cap_connect` + reconnect scaffolding was **not committed** and is being reverted from the
working tree; the box is rolled back to the item-1 (committed) `carplay-wireless`. Re-create the socket
scaffolding from this doc for the Model-B session.

---

## Remaining task list (as of 2026-08-01) — ordered

Landed & hardware-validated this session: audit remediation (Phase 0/1), the ocbmd OutQueue refactor,
the lost-command chain (C1/C4), wired↔wireless switching (item 1) + the Hot-Handover/Standard opt-in.

1. **Item 3 — V1 AVCC fast path + V4 host backpressure** (host Swift; biggest host-CPU win; no BT
   unknowns; testable on the live box).
2. **Item 4 — structural**: delete the dormant `Protocol/` layer (extract `TouchAction`/`CommandID`/LE
   helpers to `InputTypes.swift` first) + build the OCBM test suite (unblocked by the coordinator split)
   + add a TSan scheme.
3. **Deferred smaller items** (from docs/ops/05_AUDITS.md): eld-codec `AACENC_SBR_MODE=0` (ELD ASC, needs the phone);
   `CT_INPUT_NACK` protocol addition; `connect_seam` numeric-only (E); ocbmd `opt-level=2` (measure-first).
4. ~~**Item 2 (LAST) — accessory-initiated BT reconnect via Model B** (this doc).~~ **✅ DONE
   2026-08-01, device-proven** — see the RESOLVED banner at the top. SDP client + RFCOMM client +
   reconnect orchestrator landed; the phone's real iAP2-v2 UUID (`02030302-…`) on RFCOMM ch 1 was the
   unlock.

---

## HCI event-mask pairing regression

<!-- absorbed: ../wireless/01_BT_AND_RADIO.md -->

STATUS: FIXED in source and at HEAD, verified by review against the real kernel sources. Shipped in
`6425a7a` as part of the 2026-07-25 QC remediation batch (see §5 for the rest of that batch).
**Live-hardware confirmation: strong indirect evidence only — see the note below.**

> **⚠️ The "pending live-hardware confirmation" caveat is DOWNGRADED, not retired (2026-08-16).**
> docs/wireless/01_BT_AND_RADIO.md §6b's 2026-08-15 RTL8822CS session almost certainly included a fresh SSP pairing, which is
> inference from a working session rather than a logged pairing — and it exercised only the
> **daemon-side** half of the fix (`bt_bringup.rs`); the `attach_bluetooth.sh` half remains
> reviewed-not-retested on IW416. Full reasoning: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-41-1`.

### The symptom

From `SESSION_HANDOFF_2026-07-24.md`: the box becomes discoverable and stays so (`hciconfig hci0 name`
reads `'CarLink-b0df'` steadily, `carplay-wireless` running, SDP/SSP bring-up clean in `/tmp/wl.log`).
The iPhone **sees** the device in Settings > Bluetooth and taps it. Every attempt reports **"Pairing
Unsuccessful."**

Evidence gathered at the time:
- `/tmp/wl.log` shows the phone connecting to the SDP PSM and browsing the "Wireless iAPv2" record —
  `ServiceSearchRequest` → `ServiceAttributeRequest`, three full cycles — then closing.
- **No pairing-related mgmt event is ever logged.** `ssp_agent.rs`'s event loop never fires; only its
  startup `SET_POWERED`/`BONDABLE`/`CONNECTABLE`/`SSP`/`IO_CAPABILITY` lines appear.
- `dmesg` shows zero BT ACL/pairing/auth events.

### Two pieces of that evidence were red herrings — correct the record

1. **"SDP browsed 3× then closed" is NORMAL.** The known-good 2026-07-14 hardware log
   (`scratchpad/boxlogs/wl.log`) shows the identical three-cycle browse-then-close before *every*
   successful session. The regression is entirely in what happens after the close: good = RFCOMM
   connect; broken = nothing.
2. **"No `EV_USER_CONFIRM_REQUEST`" proves nothing.** On this kernel a Just-Works bond with
   NoInputNoOutput is auto-accepted **in-kernel** — `hci_user_confirm_request_evt`
   (`hci_event.c:3144`) sends `HCI_OP_USER_CONFIRM_REPLY` itself and never calls
   `mgmt_user_confirm_request`. That event would not arrive even on a fully successful pairing. Only
   `NEW_LINK_KEY` would.

### Root cause

`ccpa/rootfs/script/attach_bluetooth.sh`'s `attach_bt()` ended with:

```sh
hcitool -i hci0 cmd 0x03 0x0003
```

OGF 0x03 / OCF 0x0003 = **HCI_Reset (0x0C03)**, issued on a raw socket behind the kernel's back, as the
*last* controller operation of the function.

HCI_Reset restores the controller's **spec-default event mask** (`0x00001FFFFFFFFFFF` — events
0x01–0x2D only). The SSP events live above that range:

| Event | Code |
|---|---|
| IO_Capability_Request | 0x31 |
| User_Confirmation_Request | 0x33 |
| User_Passkey_Request | 0x34 |
| Simple_Pairing_Complete | 0x36 |

These are enabled **only** by the kernel's init-time `Set_Event_Mask`, and — verified directly against
the v3.14.52 sources — that call is reachable by exactly one path:

- `hci_setup_event_mask` (`hci_core.c:1081`) sets `events[6] |= 0x01/0x04/0x08/0x20` iff
  `lmp_ssp_capable`.
- It is called only from `hci_init2_req` (`hci_core.c:1174`), reachable only via `__hci_init`
  (`:1364-1391`), reachable only from **`hci_dev_do_open`** (`:1852`) — i.e. an HCIDEVUP on a **DOWN**
  device.
- `hci_dev_reset` (`:2108-2151`) — what `hciconfig hci0 reset` invokes — purges queues and issues a
  bare reset. It does **not** re-init and never touches the event mask.
- `hci_dev_do_open` returns `-EALREADY` when `HCI_UP` is already set (`:1890-1891`), and BlueZ's
  `hciconfig up` swallows `EALREADY` and exits 0 — which is why `bt_bringup`'s existing
  `run(&[hci_dev, "up"])?` "succeeded" while re-initializing nothing.

**Net effect:** SDP and ACL keep working (their events are inside the default mask), the box looks
completely healthy, `sspmode` reads back Enabled, name/class/EIR/piscan are all applied — but when the
iPhone initiates fresh SSP pairing, the controller never delivers IO_Capability_Request to the host,
the kernel never replies, and LMP pairing times out phone-side. "Pairing Unsuccessful", with zero
evidence at the mgmt layer and zero in `dmesg`.

### Why it looked like a regression from a specific date

Bonded **reconnects** use Link_Key_Request (0x17), which *is* inside the default mask — so a
previously-bonded phone reconnects fine with a wiped mask. That is exactly what the 2026-07-14 log
shows. The 30-minute soak on 2026-07-17 (docs/wireless/00_WIRELESS_CARPLAY.md) evidently rode a bring-up ordering in which a kernel
re-init landed last.

docs/wireless/01_BT_AND_RADIO.md's `bt_on.sh` fix then made the bring-up sequence deterministic — and the raw reset became
reliably the final word. The pairing failure went from intermittent to every-time. It also retroactively
explains docs/wireless/00_WIRELESS_CARPLAY.md's "accumulated IW416 controller wedge... cleared by a power-cycle": a power cycle
forces a genuine cold `hci_dev_do_open`.

Supporting detail: line 21 also undid the two vendor commands immediately preceding it (SCO routing,
BLE power) — strong evidence it was a cargo-culted leftover rather than intentional.

### The fix

1. **`ccpa/rootfs/script/attach_bluetooth.sh`** — removed the trailing raw HCI_Reset entirely. Added
   `hciconfig hci0 down` + `up` after the `hciconfig hci0 reset` in the MAC-programming branch, since
   that reset has the same defect. (Bonus: the `up` makes the kernel re-read BD_ADDR, so
   `/sys/class/bluetooth/hci0/address` now reflects a freshly programmed MAC instead of a stale cache.)
2. **`crates/vendor/wireless/src/bt_bringup.rs`** — `bring_up()` now does a best-effort `down` before
   `up`, forcing `hci_dev_do_open` regardless of what the boot scripts did to the controller. This makes
   the daemon self-sufficient rather than dependent on script history.

#### Side effect — REPAIRED 2026-09-03 (this section used to say "accepted")

The controller is `hciattach`'d over UART, so it carries `HCI_QUIRK_RESET_ON_CLOSE` — the `down` makes
the kernel issue a real HCI_Reset on close, and the `up` re-reads `Read_Buffer_Size`. That discards
`attach_bluetooth.sh`'s HFP setup (`scomtu 240:32` plus the SCO-routing and BLE-power vendor commands).

**This section used to read "deliberately not re-issued: CarPlay carries all audio — including
telephony — over the AirPlay/WiFi session, never over SCO/HFP, so nothing in the proven path depends
on them."** Every clause of that is still true *of CarPlay* and the conclusion no longer holds,
because wireless **Android Auto** does the opposite: gearhead routes calls AND the Assistant through
the connected Bluetooth headset (`kxr.java:118-150`), so the audio arrives on an (e)SCO channel on our
own HFP link. The discarded setup is load-bearing again.

**The repair** (`bt_bringup::restore_sco_setup`): after the `up`, the daemon runs
`timeout 20 sh /script/radio_hal.sh sco_on`, which re-applies the *unit's own* extracted SCO lines.
(That `scomtu 240:32` is `mtu:pkts` — 240 bytes over 32 packets — so it comfortably carries the 60 B
transparent-eSCO writes HFP **wideband** needs; `sco_send_frame` only rejects a write longer than the
MTU. `sco_audio` still logs an `EINVAL`/`EMSGSIZE` uplink write once, with the downlink packet size,
because a unit whose scomtu was reset to something smaller would otherwise be a silent mute.)
Three properties, each deliberate:

* **It never composes raw HCI itself.** Which command a unit needs is a per-chipset fact only that
  unit's dispatcher knows; `0x3f 0x1c …` is BCM4358's and `0x3f 0x1d 0x00` is NXP's, and firing
  either at the wrong controller is precisely what the seam exists to prevent.
* **It is bounded.** `sco_on` takes radio_hal's `bt` lock, and `bt_on` can legitimately hold that
  lock for minutes; `bring_up` is on the session's critical path, so an unbounded call would stall a
  session start behind a bring-up already doing the work. Exit 124 is logged as a timeout.
* **It is never fatal.** No mapping → exit 3, logged, Bluetooth still comes up. A box with no SCO
  loses call audio; a box that refuses to bring Bluetooth up loses everything.

The BLE-power command is deliberately NOT restored: nothing in this stack uses BLE, and re-issuing a
vendor-opaque power write for no consumer is an untested write to the controller for no gain.

Note also that `bring_up` runs **once per active session** (re-entered from the arbiter loop on every
wired→wireless handback), not once per process. The down/up is safe because within
`run_active_session` the mgmt socket, SDP listener and RFCOMM listener are all spawned *after* it
returns, the previous session's threads are joined *before* re-entry, and `noscan` blocks inbound ACLs
during the quiet period.

### Diagnostics added alongside (`ssp_agent.rs`)

The failure was undiagnosable from the box because the mgmt event loop's `_ => {}` silently dropped
every unrecognized event. Now:

- The catch-all logs event code, name, controller index, param length and hex params.
- Explicit arms log `DEVICE_CONNECTED` / `DEVICE_DISCONNECTED` / `CONNECT_FAILED` / `AUTH_FAILED` with
  bdaddr and reason/status. All byte offsets verified against v3.14.52 `mgmt.h`.
- `LOAD_LINK_KEYS` now reads and logs its completion status. This matters twice: the kernel rejects the
  whole load on the first record whose `addr_type != 0x00` (BDADDR_BREDR), **and** a rejected load also
  skips the kernel's own `hci_link_keys_clear`, leaving stale kernel keys that can then fail BR/EDR auth
  with no local explanation.
- Removed a `param_len < 6` pre-filter that discarded every short event *before* the match — part of why
  the loop appeared never to fire.
- Corrected the module doc, which claimed the agent "auto-accepts every pairing confirmation". On this
  kernel it never sees one; it is really the settings setup, the NEW_LINK_KEY persistence path, the
  Numeric-Comparison code publisher, and (now) the connection/auth event logger.

### SSP pairing design (current)

`ssp_agent.rs` (`crates/bt-common`) does more than the connection/auth logging above:

- **Bdaddr redaction.** Any bdaddr in a log line carries only its last two octets, never the full
  address.
- **Link-key load.** One line per persisted record on load: `..<tail> addr_type=<n>
  key_type=<name> (0x<hex>)`, named per the BlueZ mgmt "Link Key Type" values (`0x00` combination,
  `0x03` debug, `0x04` unauth_p192, `0x05` auth_p192, `0x06` changed, `0x07` unauth_p256, `0x08`
  auth_p256). `EV_NEW_LINK_KEY` is logged unconditionally, including `store_hint=0` (controller
  declines storage). `EV_AUTH_FAILED` (`[bdaddr 6][addr_type 1][status 1]`, status at param offset
  7) is a named arm, not just the hexdump fallback.
- **Rejection cap.** `ssp_agent` counts `EV_USER_CONFIRM_REQUEST`s per (bdaddr, connection); it
  still replies to every one, but after 3 rejections with no intervening `NEW_LINK_KEY` it logs
  once and publishes `BTP_PAIR_REJECTED` (`ocbm-proto`) via a callback hook `ssp_agent::run` takes
  (`bt-common` has no dependency on `ocbm-proto`/`bt_driver`; the wireless daemon wires the hook to
  `bt_driver::publish_bt_phase`). The counter resets on `DEVICE_DISCONNECTED` or `NEW_LINK_KEY`.
- **Connect hold.** Both RFCOMM backends' `connect_to` (`bt-common::rfcomm.rs`, which the CCPA
  supervisor runs, and `rfcomm_uspace.rs`) go through `rfcomm_uspace::pairing_aware_connect`:
  connect non-blocking and poll. While `/tmp/pairing_code` is fresh for this attempt (mtime ≥ the
  attempt's start; `ssp_agent` writes it on `USER_CONFIRM_REQUEST`) the deadline stretches to
  `rfcomm_uspace::PAIRING_HOLD_SECS` (60 s) so the phone-side tap can land, latched for the rest of
  the attempt. When the rejection streak fires, `ssp_agent` also clears the code and writes
  `/tmp/pair_rejected`; `connect_to` sees it and returns `PermissionDenied` immediately.
  `reconnect.rs` maps that to `Attempt::PairRejected`: stop paging the remaining bonds, sleep
  `BACKOFF_MAX_SECS` (60 s), then clear the flag so the next attempt is judged on its own.
  `NEW_LINK_KEY` also clears the flag; `DEVICE_DISCONNECTED` clears the code.
- **Bonding intent.** Both backends request `BT_SECURITY_MEDIUM` (general bonding) on the outgoing
  socket (`rfcomm_uspace::request_bonding`) so a box-initiated connect's own side asks for a
  persistent key (`store_hint=1`); without it the kernel reported `store_hint=0` and a freshly
  authenticated key was never written, so the next boot would have prompted again. `ssp_agent`
  still honours whatever hint the kernel returns, as BlueZ does.
- **`SET_DISCOVERABLE`.** `bt_bringup`'s `hciconfig piscan` can be undone by mgmt's
  `SET_CONNECTABLE` rewriting Scan_Enable from its own flags. `ssp_agent` sends
  `SET_DISCOVERABLE=general` (no timeout) after `SET_CONNECTABLE` so a phone-initiated re-pair from
  Settings ▸ Bluetooth can still find the box.
- **`confirm_hint`.** `ssp_agent` honours the mgmt `confirm_hint` byte: `1` means the kernel
  resolved the pairing to Just-Works (auto-accept, no code), `0` means a human comparison is
  required — regardless of our own IO capability, as the SSP rules require. Just-Works is always
  auto-accepted, unaffected by anything below.
- **Numeric comparison: default is to confirm our side immediately.** In numeric mode
  (`confirm_hint == 0`) `ssp_agent` publishes the code to `/tmp/pairing_code` and replies
  `OP_USER_CONFIRM_REPLY` at once — this is the default and needs no app attached. Behind
  `CARPLAY_SSP_INTERACTIVE=1` (host YAML `pairing: numeric_comparison_interactive`, app Settings
  "Answer the pairing code in this app") the agent instead waits for a real answer from the head
  unit: it records a pending confirm `{bdaddr, addr_type, deadline = now + 55 s}`
  (`PAIR_CONFIRM_WAIT_SECS`, inside `PAIRING_HOLD_SECS` so the connect hold is still open when the
  answer lands) and replies to nothing until the macOS app sends `CT_PAIR_CONFIRM` (`0x1C`,
  `[accept u8]`, docs/carplay/01_OCBM_PROTOCOL.md) → ocbmd → carplay-wireless's control port
  (`127.0.0.1:9115`, `{"cmd":"pair_answer","accept":true|false}`, `control.rs`). Pair →
  `OP_USER_CONFIRM_REPLY`; Cancel or no answer in 55 s → `OP_USER_CONFIRM_NEG_REPLY`, raising
  `/tmp/pair_rejected` and clearing the code so the connect hold aborts and the reconnect driver
  backs off instead of retrying into the same prompt. `DEVICE_DISCONNECTED` or `NEW_LINK_KEY` for
  that bdaddr drops the pending confirm; a repeat request from the same phone keeps the original
  deadline; a second phone's request supersedes the first (answered NO); an answer with nothing
  pending is dropped, never banked. The rejection cap above still fires at 3 confirms with no bond,
  but in this mode replies NO — the code is already off the screen. This lever is bench-only
  (`tools/session_supervisor.sh` must never set it outside of `pairing_interactive()`'s YAML read).

**Measured against iOS 27 (device captures, 2026-09-03/04):**

| Who initiates the connect | Who confirms | iOS result |
|---|---|---|
| Box (reconnect/re-pair) | Box confirms immediately (default) | iOS shows its code sheet, user confirms on the phone, key = `auth_p192` |
| Box (reconnect/re-pair) | Box waits for the head unit's own yes/no (`CARPLAY_SSP_INTERACTIVE=1`) | iOS returns `pairingComplete result:162` ~0.3 ms after its own confirm request — no sheet ever shown, even with Settings ▸ Bluetooth open |
| Phone, from Settings ▸ Bluetooth | — (Just-Works, iOS offers NoInputNoOutput) | Pairs immediately, key = `unauth_p192`, no code anywhere |

iOS will not hold a box-initiated numeric-comparison request open for a human on the accessory side
to answer, so the spec-literal both-humans flow is unreachable with iOS as the peer and lives only
behind the bench lever above. The one path where the human answers on the phone with the box
merely displaying a code is box-initiated + immediate self-confirm (row 1); the only path where the
user pairs from the phone's own Settings is Just-Works (row 3).

### If this does NOT fix it — the discriminating evidence

The new logging makes the next test conclusive rather than another guess:

- **Only `DEVICE_CONNECTED`/`DEVICE_DISCONNECTED`, nothing else** → still an event-mask problem.
- **`AUTH_FAILED` (0x0011)** → stale/mismatched link key. Check `/etc/carplay/bt_link_keys` and the
  iPhone's **Settings ▸ General ▸ CarPlay** list (CarPlay pairings persist there independently of the
  Bluetooth entry). Moving the key store aside and restarting `carplay-wireless` clears both sides.
- **Nothing at all, not even 0x000B on tap** → event delivery itself is broken; the mask hypothesis
  gains further weight.

A zero-redeploy on-box probe also exists: `hciconfig hci0 down && hciconfig hci0 up`, then restart
`carplay-wireless` and retry pairing.

**Do not build an HFP SDP record on the current evidence.** The standing hypothesis that the Class of
Device (`0x200408`, Audio/Video Hands-free) needs a matching HFP/A2DP SDP record — the box serves only
the iAP2 SPP record — cannot be the cause of a *regression*: that exact combination paired and ran a
30-minute session on 2026-07-14/17. It would require the phone's behavior to have changed.

### §5 — the rest of the 2026-07-25 batch

This document covers the blocker. The same batch also fixed, from a 12-agent code audit:

- **`server.rs`** — the pre-pair-verify plaintext accumulator was unbounded; any peer with network
  adjacency to `[::]:5000` could stream header-less bytes until allocation failed and `panic = "abort"`
  killed `airplayd`. Now capped like the encrypted path.
- **`airplayd/main.rs`** — its `LocalMfiSigner` was the only MFi chip user taking no
  `/tmp/carplay_mfi.lock`, while `iap2d`, `carplay-wireless` and `mfi-i2c-local` all do. Both stateful
  sequences now hold a bounded (10 s) flock.
- **`ocbmd`** — frame splicing: a queue resting mid-frame under USB backpressure (the normal state for
  4K video) let the next `drain()` write a complete CTRL/audio frame into the middle of it. Fixed with a
  `Drain{Done,Partial,Blocked}` result and wire-ownership tracking.
- **`iap_tunnel.rs`/`events.rs`** — the tunnel established a full link session and then abandoned it:
  subscribes went out bare and inbound frames were never ACKed again. Now link-framed (built
  just-in-time so seq/ack are current at transmission) and serviced past Identify.
- Plus: per-stream RTCP flag, stereo RTP timestamp, `CT_SRC` clamp, `Kind::Acc` EOF handling,
  `conns.clear()` on teardown, the removal of two undeclared wired subscribes, and the wired params-6/7
  byte-pin test that had never existed.

---

## HISTORICAL — bring-up race and health check

<!-- absorbed: ../wireless/01_BT_AND_RADIO.md -->

STATUS: SHIPPED 2026-07-24. Written 2026-07-25 — the code comments in
`ccpa/rootfs/script/attach_bluetooth.sh` and `bt_on.sh` referenced "docs/wireless/01_BT_AND_RADIO.md" from the day they were
written, but the document was never actually created (flagged as a loose end in
`SESSION_HANDOFF_2026-07-24.md`). This closes that.

Both fixes are in the box's **shell bring-up scripts**, not in any Rust daemon. They are pre-existing
vendor-script defects that happened to surface while chasing an unrelated problem.

### Fix 1 — `attach_bluetooth.sh`: the retry loop had no real health check

`attach_bt()` loads `hci_uart`, pushes the NXP IW416 firmware over `/dev/ttymxc2` via
`fw_loader_linux`, attaches HCI with `hciattach`, and brings the interface up. The retry loop then
decided whether that worked by checking `hciconfig hci0`'s **exit code**.

That only proves the net-device *object* exists. `fw_loader_linux` can report `Download Error` on the
firmware push while `hciattach` and `hciconfig hci0 up` still succeed at the interface level — leaving
a chip that never answers a single HCI command. Every subsequent read/write then times out at the
kernel HCI layer (~2-3 s each), and the box runs for the rest of boot with a dead radio that looks
alive. Confirmed live via `dmesg`: repeated `hci0 command 0x0c56 / 0x0c24 / 0x0c13 / 0x0c14 tx
timeout`.

**Fix:** added `bt_responsive()`, which reads the local name back —
`timeout 5 hciconfig hci0 name | grep -q "Name:"` — using the exact readback pattern already proven in
`bt_on.sh`, bounded by `timeout` as a belt-and-suspenders on top of the kernel's own per-command HCI
timeout. The retry loop now uses that as its health check instead of the exit code.

### Fix 2 — `bt_on.sh`: two uncoordinated bring-ups fighting each other

`bt_on.sh` backgrounded `attach_bluetooth.sh` with `&` and then waited only for
`/sys/class/bluetooth/hci0` to *exist* (~100 ms) before returning. So `wireless_up()` immediately
exec'd `carplay-wireless`, which runs its **own independent** Bluetooth bring-up — `killall
bluetoothDaemon hcid sdpd` plus its own `hciconfig` calls (`bt_bringup::bring_up`) — concurrently with
`attach_bluetooth.sh`'s still-in-flight retry loop.

The two uncoordinated bring-ups fought each other indefinitely. **Observed live: killed after 7+
minutes without ever converging.**

**Fix:** `bt_on.sh` now waits (up to 500 s) for `/tmp/.hciattach_done` — a flag `attach_bluetooth.sh`
already touched as its final statement and which nothing had ever read — before returning. So
`carplay-wireless` cannot start until BT has genuinely converged.

### Result

After both fixes, BT bring-up was clean and fast on every subsequent test: the chip is responsive
within seconds and the two bring-ups no longer collide.

### Important consequence (see docs/wireless/01_BT_AND_RADIO.md)

Fix 2 made the bring-up **deterministic**. That is a good thing on its own, but it had a side effect
nobody predicted: it also made deterministic the *order* in which the controller's last configuration
write happened — and the last write was a raw `HCI_Reset` at the end of `attach_bt()` that wiped the
kernel's SSP event mask. Before this fix, the racy interleaving sometimes left a kernel re-init as the
final word, so fresh pairing sometimes worked; afterwards the raw reset always won, and fresh SSP
pairing broke every time.

In other words: **this fix did not cause the pairing regression, but it is what turned an intermittent
latent bug into a reproducible one.** That is a good trade — a reproducible bug is a fixable bug — and
the actual defect is documented and fixed in
`../wireless/01_BT_AND_RADIO.md`.

### Deployed

- `/script/attach_bluetooth.sh` — md5 `642b9a27786232fc8de6afc7e410a837`
- `/script/bt_on.sh` — md5 `2184dbfb23b3a64a52fc77da2bdd9013`

Both are plain shell pushed to `/script/`; no rebuild or UPX step.

> **⚠️ THE `attach_bluetooth.sh` MD5 ABOVE IS SUPERSEDED** (`bt_on.sh`'s is still current at HEAD).
> It records what this fix deployed on 2026-07-24; docs/wireless/01_BT_AND_RADIO.md changed the file the next day, so a box
> still matching `642b9a27…` is running the **pre-docs/wireless/01_BT_AND_RADIO.md** script. Such a box is not necessarily
> unable to pair — but push the current file rather than reason about which.
> [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-40-1`.
