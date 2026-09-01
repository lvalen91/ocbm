#!/bin/bash
#
# diag_softap_5g.sh — one-shot root-cause capture for "5 GHz SoftAP fails on rpi4 AAOS 16"
#
# Runs from the Mac, drives adb. Read-mostly; every mutation is listed, backed up,
# and undone in the RESTORE phase (also on ^C / error via trap).
#
# Mutations performed (all reversible, all restored at exit):
#   1. logcat buffer sizes main/system/kernel: 256K -> 4M          (restored to 256K)
#   2. cmd wifi set-verbose-logging enabled                        (restored to disabled)
#   3. setprop log.tag.<Tag> V for wifi framework tags             (restored to "")
#   4. Three softap start/stop attempts on wlan0                   (stopped, wlan0 back to managed)
#   5. OPTIONAL (HOSTAPD_DD=1): bind-mount a '-dd' wrapper over
#      the hostapd binary                                          (umounted + wrapper deleted)
# No reboot. No partition writes. No /vendor or /system edits.
#
# Usage:
#   ./diag_softap_5g.sh                 # normal run
#   HOSTAPD_DD=1 ./diag_softap_5g.sh    # also enable the experimental hostapd -dd wrapper
#
set -u

# ---------------------------------------------------------------- configuration
SERIAL="${SERIAL:-10000000bf546fb8}"       # USB serial; use SERIAL=1.1.1.2:5555 for TCP
SSID="CarlinkAP"
PASS="Passw0rd123"
HOSTAPD_DD="${HOSTAPD_DD:-0}"              # 1 = install hostapd -dd wrapper (optional step)
SETTLE_SECS=8                              # wait after each start-softap before sampling
OUT="${OUT:-$HOME/Desktop/softap_diag_$(date +%Y%m%d_%H%M%S)}"

HOSTAPD_BIN="/apex/com.android.hardware.wifi.hostapd.rpi/bin/hw/hostapd"
HOSTAPD_CONF="/data/vendor/wifi/hostapd/hostapd_wlan0.conf"
WRAPPER="/data/local/tmp/hostapd_dd_wrapper.sh"
HOSTAPD_COPY="/data/local/tmp/hostapd.real"

LOG_TAGS="hostapd HostapdHalAidlImp SoftApManager WifiActiveModeWarden ApConfigUtil WifiNative wificond WifiCountryCode WifiNl80211Manager WifiVendorHal HalDevMgr"

# ---------------------------------------------------------------- helpers
ADB="adb -s $SERIAL"
say()  { printf '\n\033[1m== %s\033[0m\n' "$*"; }
fail() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
ash()  { $ADB shell "$@"; }                # adb shell
grab() {                                   # grab <outfile> <shell command...>
  local f="$OUT/$1"; shift
  ash "$@" > "$f" 2>&1
  printf '  saved %s\n' "$f"
}

WRAPPER_MOUNTED=0
VERBOSITY_SET=0
IW_EVENT_PID=""

restore() {
  trap - EXIT INT TERM
  say "RESTORE: returning device to known-good state"
  [ -n "$IW_EVENT_PID" ] && kill "$IW_EVENT_PID" 2>/dev/null
  ash "cmd wifi stop-softap" >/dev/null 2>&1
  if [ "$WRAPPER_MOUNTED" = 1 ]; then
    ash "umount $HOSTAPD_BIN" && echo "  - hostapd -dd bind-mount removed"
    ash "rm -f $WRAPPER $HOSTAPD_COPY" && echo "  - wrapper + hostapd copy deleted from /data/local/tmp"
  fi
  if [ "$VERBOSITY_SET" = 1 ]; then
    ash "cmd wifi set-verbose-logging disabled" >/dev/null 2>&1 \
      && echo "  - wifi verbose logging disabled"
    for t in $LOG_TAGS; do ash "setprop log.tag.$t ''" 2>/dev/null; done
    echo "  - log.tag.* props cleared"
    ash "logcat -b main -G 256K; logcat -b system -G 256K; logcat -b kernel -G 256K" \
      && echo "  - logcat buffers back to 256K"
  fi
  # wait for wlan0 to return to managed
  local i=0
  while [ $i -lt 15 ]; do
    ash "iw dev wlan0 info 2>/dev/null | grep -q 'type managed'" && break
    sleep 1; i=$((i+1))
  done
  ash "iw dev" > "$OUT/99_final_iw_dev.txt" 2>&1
  echo "  - final interface state saved to $OUT/99_final_iw_dev.txt"
  say "Everything changed by this script has been undone. Artifacts: $OUT"
}
trap restore EXIT
trap 'restore; exit 130' INT TERM

# ---------------------------------------------------------------- phase 0: preflight
mkdir -p "$OUT" || fail "cannot create output dir $OUT"
say "Phase 0: preflight (output -> $OUT)"

$ADB get-state >/dev/null 2>&1 || fail "device $SERIAL not reachable over adb"
[ "$(ash 'id -u' | tr -d '\r')" = "0" ] || { $ADB root >/dev/null 2>&1; sleep 2; }
[ "$(ash 'id -u' | tr -d '\r')" = "0" ] || fail "adb shell is not root (adb root failed)"
ash "test -x $HOSTAPD_BIN" || fail "hostapd binary not at $HOSTAPD_BIN"
echo "  device root shell OK"

# ---------------------------------------------------------------- phase 1: pre-state snapshot
say "Phase 1: pre-state snapshot"
# WHY each artifact matters is listed in the README written at the end.
grab 01_iw_reg_get.txt          "iw reg get"
grab 01_iw_phy_info.txt         "iw phy phy0 info"
grab 01_iw_phy_channels.txt     "iw phy phy0 channels"
grab 01_iw_list.txt             "iw list"
grab 01_iw_dev.txt              "iw dev"
grab 01_props_wifi.txt          "getprop | grep -iE 'wifi|wlan|hostapd|country|region'"
grab 01_cmd_wifi_status.txt     "cmd wifi status; echo ---; cmd wifi get-country-code; echo ---; cmd wifi is-verbose-logging"
grab 01_dumpsys_wifi_full.txt   "dumpsys wifi"
grab 01_dumpsys_softap.txt      "dumpsys wifi | grep -iE -B2 -A8 'SoftApCapability|SoftApState|SoftApInfo|CountryCode|available channel'"
grab 01_rfkill.txt              "for f in /sys/class/rfkill/rfkill*; do echo \$f \$(cat \$f/name) state=\$(cat \$f/state) soft=\$(cat \$f/soft) hard=\$(cat \$f/hard); done"
grab 01_fs_layout.txt           "ls -laR /data/vendor/wifi 2>&1; echo ---; ls -la /vendor/etc/wifi /vendor/etc/hostapd* 2>&1; echo ---; ls /vendor/firmware/ /vendor/etc/firmware 2>&1 | head -40"
grab 01_hal_services.txt        "service list | grep -iE 'wifi|hostapd|supplicant'; echo ---; ps -A | grep -iE 'hostapd|wifi|wpa'; echo ---; ls /apex/ | grep -iE 'wifi|hostapd'"
grab 01_vintf.txt               "cat /apex/com.android.hardware.wifi*/etc/vintf/* 2>/dev/null; echo ---; grep -rlE 'hostapd|wifi' /vendor/etc/vintf/ 2>/dev/null | while read f; do echo == \$f; cat \$f; done"
grab 01_hostapd_rc.txt          "cat /apex/com.android.hardware.wifi.hostapd.rpi/etc/*.rc"
grab 01_dmesg_pre.txt           "dmesg"
grab 01_hostapd_conf_leftover.txt "cat $HOSTAPD_CONF 2>&1; echo ---; stat -c 'mtime=%y size=%s' $HOSTAPD_CONF 2>&1"
grab 01_brcmfmac.txt            "dmesg | grep -iE 'brcm|firmware|cfg80211|regulatory' | tail -60; echo ---; ls /sys/module/brcmfmac/parameters/ 2>/dev/null && grep -r . /sys/module/brcmfmac/parameters/ 2>/dev/null"

# ---------------------------------------------------------------- phase 2: verbosity up
say "Phase 2: raising verbosity (all reversible)"
grab 02_logcat_g_before.txt "logcat -g"
ash "logcat -b main -G 4M; logcat -b system -G 4M; logcat -b kernel -G 4M"
for t in $LOG_TAGS; do ash "setprop log.tag.$t V"; done
ash "cmd wifi set-verbose-logging enabled"
VERBOSITY_SET=1
echo "  logcat buffers 4M, log.tag props V, wifi verbose logging enabled"
echo "  NOTE: 'set-verbose-logging enabled' also makes the framework call"
echo "  IHostapd.setDebugParams(DEBUG) on hostapd registration, so hostapd itself"
echo "  logs at debug level on every attempt below — usually no rc edit needed."

# ---- OPTIONAL, CLEARLY MARKED: hostapd -dd wrapper -------------------------
# The classic advice is to add '-dd' to hostapd's init .rc. On this device the
# rc lives INSIDE the read-only APEX (com.android.hardware.wifi.hostapd.rpi,
# ext4 loop mount, ro) and init only parses rc files at boot, so editing it is
# impossible without a reboot. Equivalent no-reboot trick: bind-mount a tiny
# wrapper over the hostapd binary that execs the real binary with -dd.
# Reversal = umount (no file was modified). SELinux is permissive so exec from
# /data/local/tmp works. Enabled only with HOSTAPD_DD=1.
if [ "$HOSTAPD_DD" = 1 ]; then
  say "Phase 2b: OPTIONAL hostapd -dd wrapper (HOSTAPD_DD=1)"
  ash "cp $HOSTAPD_BIN $HOSTAPD_COPY && chmod 755 $HOSTAPD_COPY"
  ash "echo '#!/system/bin/sh' > $WRAPPER; echo 'exec $HOSTAPD_COPY -dd \"\$@\"' >> $WRAPPER; chmod 755 $WRAPPER"
  ash "mount --bind $WRAPPER $HOSTAPD_BIN" || fail "bind mount failed"
  WRAPPER_MOUNTED=1
  echo "  wrapper mounted over $HOSTAPD_BIN (undo: umount, done automatically at exit)"
else
  echo "  (skipping hostapd -dd wrapper; rerun with HOSTAPD_DD=1 if AIDL debug level is not enough)"
fi

# ---------------------------------------------------------------- phase 3: A/B/C attempts
run_case() {  # run_case <name> <extra start-softap args...>
  local name="$1"; shift
  say "Phase 3: case '$name'  (start-softap $SSID wpa2 **** $*)"
  local pre_stat post_stat

  ash "logcat -b main -b system -b crash -b kernel -c"          # clean slate per case
  ash "dmesg" > "$OUT/30_${name}_dmesg_pre.txt" 2>&1
  pre_stat=$(ash "stat -c '%Y' $HOSTAPD_CONF 2>/dev/null" | tr -d '\r')

  # nl80211 event stream during the attempt (shows START_AP attempts/refusals)
  $ADB shell "timeout $((SETTLE_SECS+6)) iw event -t" > "$OUT/30_${name}_iw_event.txt" 2>&1 &
  IW_EVENT_PID=$!
  sleep 1

  ash "cmd wifi start-softap $SSID wpa2 $PASS $*" > "$OUT/30_${name}_cmd_output.txt" 2>&1
  sleep "$SETTLE_SECS"

  grab "30_${name}_iw_dev.txt"      "iw dev; echo ---; iw dev wlan0 info 2>&1; echo ---; iw dev wlan0 link 2>&1"
  grab "30_${name}_ps.txt"          "ps -A | grep -iE 'hostapd|wpa' || echo 'no hostapd process'"
  grab "30_${name}_hostapd_conf.txt" "cat $HOSTAPD_CONF 2>&1; echo ---; stat -c 'mtime=%y size=%s' $HOSTAPD_CONF 2>&1; echo ---; md5sum $HOSTAPD_CONF 2>&1"
  post_stat=$(ash "stat -c '%Y' $HOSTAPD_CONF 2>/dev/null" | tr -d '\r')
  if [ "${pre_stat:-x}" = "${post_stat:-y}" ]; then
    echo "CONFIG NOT REWRITTEN during this attempt (mtime unchanged: ${post_stat:-none})" \
      >> "$OUT/30_${name}_hostapd_conf.txt"
  fi
  grab "30_${name}_dumpsys_softap.txt" "dumpsys wifi | grep -iE -B2 -A10 'SoftApState|SoftApInfo|SoftApCapability|SoftApManager|mCurrentSoftApCapability|CountryCode'"
  grab "30_${name}_logcat.txt"      "logcat -b main -b system -b crash -b kernel -d -v threadtime"
  ash "dmesg" > "$OUT/30_${name}_dmesg_post.txt" 2>&1
  # dmesg delta (lines new since the pre snapshot)
  comm -13 <(sort "$OUT/30_${name}_dmesg_pre.txt") <(sort "$OUT/30_${name}_dmesg_post.txt") \
    > "$OUT/30_${name}_dmesg_delta.txt" 2>/dev/null || true

  wait "$IW_EVENT_PID" 2>/dev/null; IW_EVENT_PID=""

  ash "cmd wifi stop-softap" >/dev/null 2>&1
  local i=0
  while [ $i -lt 15 ]; do
    ash "iw dev wlan0 info 2>/dev/null | grep -q 'type managed'" && break
    sleep 1; i=$((i+1))
  done
  sleep 2
  echo "  case '$name' done; wlan0 back to managed"
}

run_case b2_known_good  -b 2
run_case b5_known_bad   -b 5
run_case b5_f5180       -b 5 -f 5180

# post-run regdomain: did any attempt change what the driver reports?
grab 40_iw_reg_get_after.txt "iw reg get"
grab 40_dumpsys_countrycode_after.txt "dumpsys wifi | grep -iE 'countrycode|country_code' | head -30"

# ---------------------------------------------------------------- phase 4: README / hypothesis map
cat > "$OUT/00_README.txt" <<'EOF'
HOW TO READ THIS CAPTURE  (highest-value files first)
=====================================================

30_b5_known_bad_hostapd_conf.txt   vs   30_b2_known_good_hostapd_conf.txt
  THE key artifact. Diff them.
  - If the b5 conf says hw_mode=a + a concrete channel (e.g. channel=36) and hostapd
    still exits -> the driver/kernel refused; look at iw_event + dmesg_delta.
  - If the b5 conf says channel=0 / acs_num_scans or freqlist=... -> framework asked
    for ACS on 5 GHz; ACS failure hypothesis confirmed.
  - If the b5 conf was NOT rewritten (marker line at bottom) -> hostapd/HAL rejected
    the AIDL request before writing config: parameter-validation failure in the
    hostapd AIDL frontend (e.g. empty allowed-frequency list), NOT a driver problem.

30_b5_known_bad_logcat.txt
  Full framework+HAL+hostapd+kernel slice for the failing attempt only.
  grep for: "HostapdHalAidlImp" (exact AIDL error code), "SoftApManager",
  "ApConfigUtil" (channel selection result), "hostapd" (with verbose logging the
  hostapd daemon prints why it bails: e.g. "Could not select hw_mode and channel",
  "Configured channel (36) not found from the channel list of current mode",
  "Hardware does not support configured channel").
  - "Deiniting aidl control" with no earlier nl80211 error -> hostapd's own
    channel/hw_mode validation failed (config-vs-capability mismatch).
  - nl80211: "kernel reports: ..." -> kernel/regulatory refusal.

30_b5_known_bad_iw_event.txt  +  30_b5_known_bad_dmesg_delta.txt
  Kernel's view. A START_AP that never appears means hostapd died before touching
  nl80211 (config-stage failure). "regulatory" / brcmfmac lines here confirm a
  driver/regdomain refusal instead.

01_iw_reg_get.txt / 01_iw_phy_channels.txt
  Pre-known finding on this device: phy0 reports its own "country 99: DFS-UNSET"
  (driver self-managed regdomain) while global is US. If 5 GHz channels carry
  no-IR/passive flags, AP on them is regulatory-blocked -> confirms regdomain
  hypothesis. If chan 36-48 are enabled with no no-IR flag, regdomain at the
  kernel level is NOT the blocker.

01_dumpsys_softap.txt / 30_*_dumpsys_softap.txt
  Framework's channel table (SoftApCapability supported channels per band) and
  country-code state. Pre-known: mDriverCountryCode is null on this device.
  - Empty 5 GHz supported-channel list -> framework-level capability hypothesis
    (ApConfigUtil found no usable channel; hostapd was doomed before it started).
  - Non-empty list -> framework thinks 5 GHz is fine; blame moves down the stack.

30_b5_f5180 vs 30_b5_known_bad
  A/B on explicit frequency. If -f 5180 works while plain -b 5 fails, the bug is
  channel *selection* (ACS/framework picks nothing), not 5 GHz AP capability.
  If both fail identically, capability/regulatory is the problem.

30_b2_known_good_*  (control)
  Baseline for every diff above; also proves the instrumentation itself works.

01_hal_services.txt / 01_vintf.txt / 01_hostapd_rc.txt
  Confirms which hostapd build answers the AIDL calls (APEX
  com.android.hardware.wifi.hostapd.rpi) and its declared HAL version - rules
  out "wrong service registered" and gives the exact code the HAL-internals
  agent should be reading.

01_brcmfmac.txt / 01_dmesg_pre.txt
  Firmware file actually loaded, CLM blob presence, driver params. A missing or
  generic CLM blob is the classic reason brcmfmac stays in "country 99" and
  won't do 5 GHz AP.

40_iw_reg_get_after.txt
  Whether the failed attempts changed the driver regdomain (some stacks push a
  country code only when softap starts).
EOF
echo
echo "README with hypothesis map written to $OUT/00_README.txt"

# restore() runs via trap EXIT and prints the change/undo summary
exit 0
