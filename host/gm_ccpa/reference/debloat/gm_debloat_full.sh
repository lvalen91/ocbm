#!/bin/bash

# GM AAOS Debloat Script - Full Version
# Removes bloatware, telemetry, and reduces logging
# Updated: 2026-07-01
# Reconciled 2026-09-10 with reference/kept_packages.txt: hotspot/tethering section
# and Play Store handling removed, keep-list guard added (see the guard block below).
# 2026-10-15: com.android.vending, com.google.android.gms and com.google.android.gsf
# moved OFF the keep list and INTO BLOAT_PACKAGES (pm uninstall --user, users 0
# and 10) at owner request. The in-motion installer-attribution dependency
# (installerPackageName=com.android.vending) and the Automotive Template Host
# dependency (com.google.android.embedded.projection, not present on this unit)
# are UNTESTED after removal; apps installed with the installer flag will no
# longer have a vending installer on-device.
#
# Usage:
#   ./gm_debloat_full.sh            Apply debloat settings/removals
#   ./gm_debloat_full.sh --status   Report-only: check current state, make no changes
#
# NOTE: This head unit is not rooted. Per-user package removals and some
# settings do not survive every boot (the "Driver" user 10 profile in
# particular has been observed respawning its default package set on
# reboot). Re-run this script after each boot. (A companion gm_debloat_watch.sh
# was referenced here but never existed in this tree — 2026-09-10.)

MODE="apply"
if [ "${1:-}" = "--status" ] || [ "${1:-}" = "--check" ]; then
  MODE="status"
fi

echo "=== GM AAOS Debloat Script ($MODE) ==="
echo ""

# Check ADB connection
if ! adb devices | grep -q "device$"; then
  echo "ERROR: No device connected. Please connect via ADB first."
  exit 1
fi

FAIL_COUNT=0

# =============================================================================
# Helper functions
# =============================================================================

check_setting() {
  # $1=namespace $2=key $3=expected value
  local ns="$1" key="$2" expected="$3" val
  val=$(adb shell settings get "$ns" "$key" 2>/dev/null | tr -d '\r')
  if [ "$val" = "$expected" ]; then
    echo "  [OK]   $ns $key = $val"
  else
    echo "  [FAIL] $ns $key = '$val' (expected $expected)"
    FAIL_COUNT=$((FAIL_COUNT+1))
  fi
}

check_prop() {
  # $1=prop name $2=expected value
  local prop="$1" expected="$2" val
  val=$(adb shell getprop "$prop" 2>/dev/null | tr -d '\r')
  if [ "$val" = "$expected" ]; then
    echo "  [OK]   $prop = $val"
  else
    echo "  [FAIL] $prop = '$val' (expected $expected)"
    FAIL_COUNT=$((FAIL_COUNT+1))
  fi
}

check_tag_silent_quiet() {
  # $1=tag name (without persist.log.tag. prefix). Only prints on failure.
  local tag="$1" val
  val=$(adb shell getprop "persist.log.tag.${tag}" 2>/dev/null | tr -d '\r')
  if [ "$val" != "S" ]; then
    echo "  [FAIL] persist.log.tag.${tag} = '$val' (expected S)"
    FAIL_COUNT=$((FAIL_COUNT+1))
    return 1
  fi
  return 0
}

check_pkg_absent_quiet() {
  # $1=package. Only prints on failure (still installed for some user).
  local pkg="$1" bad=0 user
  for user in 0 10; do
    if adb shell pm list packages --user "$user" 2>/dev/null | grep -q "^package:${pkg}$"; then
      bad=1
    fi
  done
  if [ "$bad" = 1 ]; then
    echo "  [FAIL] $pkg still installed"
    FAIL_COUNT=$((FAIL_COUNT+1))
    return 1
  fi
  return 0
}

check_pkg_disabled() {
  # $1=package. Prints OK/FAIL per user that has it installed.
  local pkg="$1" bad=0 user
  for user in 0 10; do
    if adb shell pm list packages --user "$user" 2>/dev/null | grep -q "^package:${pkg}$"; then
      if adb shell pm list packages -d --user "$user" 2>/dev/null | grep -q "^package:${pkg}$"; then
        echo "  [OK]   $pkg disabled (user $user)"
      else
        echo "  [FAIL] $pkg enabled (user $user)"
        bad=1
      fi
    fi
  done
  if [ "$bad" = 1 ]; then
    FAIL_COUNT=$((FAIL_COUNT+1))
  fi
}

# remove_package: uninstall for each user that has it. Some system packages
# (e.g. com.gm.hmianalytics) refuse to actually go away via `pm uninstall`
# on this build even though the command exits cleanly — verify afterward
# and fall back to pm disable-user so it at least can't run.
remove_package() {
  local pkg="$1" found=false user
  if is_kept "$pkg"; then echo "    REFUSED (kept_packages.txt): $pkg"; FAIL_COUNT=$((FAIL_COUNT+1)); return 1; fi
  for user in 0 10; do
    if adb shell pm list packages --user "$user" 2>/dev/null | grep -q "^package:${pkg}$"; then
      found=true
      adb shell pm uninstall --user "$user" "$pkg" 2>/dev/null
      if adb shell pm list packages --user "$user" 2>/dev/null | grep -q "^package:${pkg}$"; then
        adb shell pm disable-user --user "$user" "$pkg" 2>/dev/null
        adb shell am force-stop --user "$user" "$pkg" 2>/dev/null
        echo "    Uninstall didn't stick, disabled instead (user $user): $pkg"
      else
        echo "    Removed (user $user): $pkg"
      fi
    fi
  done
  [ "$found" = false ] && echo "    Skipped (not found): $pkg"
}

disable_package() {
  # disable-user alone doesn't kill an already-running process (observed
  # with com.gm.gmbugreport.service, which stayed alive after disabling) -
  # force-stop it too so disabling actually takes effect immediately.
  local pkg="$1" found=false user
  if is_kept "$pkg"; then echo "    REFUSED (kept_packages.txt): $pkg"; FAIL_COUNT=$((FAIL_COUNT+1)); return 1; fi
  for user in 0 10; do
    if adb shell pm list packages --user "$user" 2>/dev/null | grep -q "^package:${pkg}$"; then
      echo "    Disabling (user $user): $pkg"
      adb shell pm disable-user --user "$user" "$pkg" 2>/dev/null
      adb shell am force-stop --user "$user" "$pkg" 2>/dev/null
      found=true
    fi
  done
  [ "$found" = false ] && echo "    Skipped (not found): $pkg"
}

# =============================================================================
# Data: log tags, packages
# =============================================================================

LOG_TAGS=(
  # --- GMLogger ---
  "GMLogger" "gmlogger" "gmloggerd" "GMLoggerService"
  # --- Vehicle HAL / Car Service ---
  "VHAL-STATS" "GMVHAL" "CAR.HalClient" "CAR.PropertyHalService"
  "automotive.vehicle@2.0-impl" "CarWatchdogService" "CAR.WatchdogStorage"
  "carwatchdogd" "GMWatchdog"
  # --- Safety Camera ---
  "SafetyCamera" "SafetyCamera_VehicleProxy" "SafetyCamera_CameraStates"
  "CameraPropertyHelper"
  # --- Navigation / GPS ---
  "NAVD" "GM_LOCATION"
  # --- Display / LVDS ---
  "lvds_device" "IPCutils" "DisplayDeviceConfig"
  # --- PTP Clock Sync ---
  "_PTP"
  # --- Platform Layer / IPC ---
  "pal_common" "pal_tod" "pal_calibrations" "ethctrlmgr" "PLSS_PMPAL"
  "PLSS_PMSvc" "plmanager"
  # --- FSA Heartbeats ---
  "f.e" "FSAServiceConnectedClient" "com.gm.fsa.service.FSAServiceConnectedClient"
  "FSANetCommsClient" "FSAIStateOfHealthClient" "nfsa.transport.in"
  # --- Audio / Tuner / Media ---
  "GMTS.a" "GMTS.m" "TunerCommon" "PlaybackViewModel"
  "AudioClusterPresentationService" "CarNowplaying" "SvrAudioAppImpl"
  "MediaService" "MediaManager" "MediaActivity" "MediaActivityCtr"
  "MediaButton" "MediaBrowserConnector" "MediaSessionUtils"
  "MediaSettingsService" "ControllerSession" "SourceControllerSRep"
  "SRReceiverManager" "DataRepStateReceiver" "SystemListener"
  "ResponseListener" "FavoritesSession" "SourceHelper" "COREU" "pulseaudio"
  "HarmanBroadcastRadioImpl2.0" "HarmanAudioControl.Plugin" "HarmanHal"
  "Media_RDMService"
  # --- HVAC ---
  "HVAC_FAN_SPEED" "HVAC_" "HVAC_ALL_SEATS" "HVAC_TEMPERATURE_SET"
  "HVAC_AUTO_ON" "HVAC_FAN_DIRECTION" "HVAC_POWER_ON" "HVAC_DUAL_ON"
  "HVAC_AC_ON" "HVAC_RIGHT_SEATS" "HVAC_RECIRC_ON" "HVAC_LEFT_SEATS"
  "ButtonBarController" "TemperatureController" "getFanSpeed"
  "PowerStateController"
  # --- Wi-Fi / Network ---
  "WifiScanningService" "WifiHandler.WifiScanningService"
  "WifiAsyncChannel.WifiScanningService" "WifiScanRequestProxy"
  "WifiNetworkSelector" "WifiHealthMonitor" "WifiNl80211Manager"
  "WifiNative" "WifiScoringParams" "WifiVendorHal" "WifiService"
  "wpa_supplicant_bcm" "wificond" "PasspointManager"
  "ConnectivityBroadcast" "WiFiNetworksRepository"
  # --- CarPlay ---
  "CarPlay_CarPlayManager" "CarPlay_CarPlayUtils" "CarPlay_GMCinemoManager"
  "CARLINK_BROWSER"
  # --- UI / Framework ---
  "WindowManager" "View" "CardView_Window" "FocusAreaHelper"
  "FragmentManager" "EmojiCompatManager" "SurfaceFlinger" "OpenGLRenderer"
  "libvintf" "android_os_HwBinder" "BroadcastQueue" "BroadcastDispatcher"
  "CPECallbackController"
  # --- Notifications / Status Bar ---
  "SmartChipController" "AssistantSuggestions" "CarHUNContainer"
  "PreprocessingManager" "NotificationViewControl" "CarNotificationBodyView"
  "GMHomeScreen_App" "GMStatusBar" "KeyguardUpdateMonitor"
  # --- System Services ---
  "JobScheduler" "JobScheduler.DeviceIdle" "JobScheduler.Background"
  "JobScheduler.Quota" "JobScheduler.Concurrency" "NetworkPolicy"
  "NetworkManagement" "CountryDetector" "ProcessStats" "AdbDebuggingManager"
  "UsbHostManager" "UsbAlsaManager"
  # --- Time / Timezone ---
  "TimeOfDay" "TimeZoneManager" "TimeAndDateRepository"
  "SystemChangesBroadcastReceiver" "EventsService" "tagValue"
  # --- Misc ---
  "SQLiteConnectionPool" "UpdateService"
)

BLOAT_PACKAGES=(
  # Google
  "com.google.android.car.setupwizard"
  "com.google.android.carassistant"
  "com.google.android.apps.maps"
  "com.google.android.feedback"
  "com.google.android.tts"
  "com.google.android.partnersetup"
  "com.google.android.onetimeinitializer"

  # GM - SiriusXM
  "brand.chevrolet.app.sxm"
  "com.gm.hmi.sxm"
  "com.siriusxm.svcsxedl"
  "com.siriusxm.svcsxedl8"

  # GM - OnStar / Telemetry
  "com.gm.hmi.onstar"
  "com.gm.hmi.onstarui"
  "com.onstar.vttproxyserver"
  "com.gm.subscriptionmanager"
  # "com.gm.vmsplugin"                    # KEEP - required for Cluster Navigation Metadata (VMSClusterService, U=0 and U=10)
  "com.gm.hmianalytics"
  "com.gm.mybrand"

  # GM - CarPlay / Android Auto / wireless projection connection UI
  # (using a third-party CarPlay adapter + custom zeno.carlink app instead)
  #
  # 2026-09-10: "com.gm.hmi.connection" REMOVED FROM THIS LIST and pinned in
  # reference/kept_packages.txt. It hosts WifiHotspotActivity, the only GUI
  # that can display the myChevrolet SoftAP passphrase, which the app cannot
  # read programmatically (docs/01_FINDINGS.md §4). The 2026-07-01 rationale
  # ("using a third-party adapter instead") no longer holds now that the
  # wireless plan needs the unit's own hotspot. brand.chevrolet.app.connection
  # stays removed: it was actually uninstalled and the hotspot GUI still worked.
  "com.gm.hmi.applecarplay"
  "com.gm.hmi.androidauto"
  "brand.chevrolet.app.connection"

  # Phone / Dialer (built-in hands-free calling - not used)
  "com.android.car.dialer"
  "com.android.car.dialer.brand.chevrolet.app.dialer.rro"
  "com.android.car.dialer.brand.chevrolet.theme.rro"

  # GM - Alexa / Voice
  "com.gm.info3.alexa"
  "com.gm.info3.alexa.brand.chevrolet.theme.rro"
  "com.gm.gmvoiceassistantproxy"

  # GM - Payment / Updater
  "com.gm.ecomm.payment"
  # "com.gm.updater"                      # KEEP - leave OTA/software updates enabled

  # GM - Teen Mode
  "com.gm.teenmode"
  "brand.chevrolet.app.teenmode"
  "com.gm.teenmode.brand.chevrolet.theme.rro"

  # GM - Setup / Auto-install
  "com.gm.setupwizard"
  "com.gm.vehicle.autoinstall.one"
  "com.gm.vehicle.autoinstall.two"
  "com.gm.vehicle.autoinstall.three"
  "com.gm.vehicle.autoinstall.four"
  "com.gm.vehicle.autoinstall.five"
  "android.autoinstalls.config.gm.gminfo37"

  # GM - Trailer (KEPT 2026-10-15, owner request; pinned in reference/kept_packages.txt)
  # "com.gm.hmi.trailer"
  # "brand.chevrolet.app.trailer"

  # GM - Climate/HVAC (COMMENTED OUT - breaks volume control)
  # "com.gm.hmi.hvac"
  # "brand.chevrolet.app.hvac"

  # Google Play / GMS (2026-10-15, owner request - were KEEP, pinned in
  # reference/kept_packages.txt; see the header note). Last in the list and
  # dependents first (Play Store, then GSF, then GMS) so every other removal
  # has already run if dropping GMS disturbs the unit. Nothing later in this
  # script uses them: Sections 4-7 are pm/settings/setprop/logcat only.
  "com.android.vending"
  "com.google.android.gsf"
  "com.google.android.gms"
)

DISABLE_PACKAGES=(
  "com.gm.deviceinformation"
  "com.gm.gmbugreport.service"

  # Google Play / GMS
  # "com.android.vending"                     # 2026-10-15: REMOVED at owner request - now uninstalled
  #                                           # via BLOAT_PACKAGES, no longer kept. In-motion display
  #                                           # (installerPackageName=com.android.vending) is UNTESTED
  #                                           # after removal; installer-flag installs no longer have a
  #                                           # vending installer on-device.
  # "com.google.android.gms"                  # 2026-10-15: REMOVED at owner request - now uninstalled
  #                                           # via BLOAT_PACKAGES (with com.google.android.gsf); the
  #                                           # Template Host dependency below is UNTESTED after removal.
  # "com.google.android.embedded.projection"  # KEEP - Automotive Template Host needed (not present on
  #                                           # this unit; still pinned in reference/kept_packages.txt)
)

# 2026-09-10: the former TETHER_PACKAGES array and SECTION 5 (SoftAP / hotspot
# disable: tether_supported 0, wifi_ap_enabled 0, fid_hotspot_disable_status 1,
# tethering_allow 0, hostapd kill) are gone. The wireless plan depends on the
# myChevrolet hotspot; those settings persist and one run poisoned the unit
# until reverted by hand. The 2026-07-01 original is byte-exact at commit b049810
# (git show b049810:host/gm_ccpa/reference/debloat/gm_debloat_full.sh); the copy
# in old/gm_ccpa is a separate 2026-09-08 variant, not this file's ancestor.
# Revert recipe in docs/01_FINDINGS.md §8.

# --- keep-list guard (2026-09-10) -------------------------------------------
# reference/kept_packages.txt is authoritative: a package it names is never
# uninstalled OR disabled here, whichever array it lands in. Hard-fail, not
# warn: this script re-runs every boot and is non-transactional.
# 2026-10-15: vending/gms/gsf deleted from the keep list (now in BLOAT_PACKAGES);
# the guard is unchanged and still covers every remaining entry.
KEEP_FILE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/kept_packages.txt"
[ -r "$KEEP_FILE" ] || { echo "ERROR: keep list not found: $KEEP_FILE"; exit 1; }
KEEP_PACKAGES=()
while IFS= read -r line || [ -n "$line" ]; do
  line="${line%%#*}"
  line="$(printf '%s' "$line" | tr -d '\r' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
  [ -n "$line" ] && KEEP_PACKAGES+=("$line")
done < "$KEEP_FILE"

is_kept() { local k; for k in "${KEEP_PACKAGES[@]}"; do [ "$k" = "$1" ] && return 0; done; return 1; }

conflicts=""
for pkg in "${BLOAT_PACKAGES[@]}" "${DISABLE_PACKAGES[@]}"; do
  is_kept "$pkg" && conflicts="$conflicts $pkg"
done
if [ -n "$conflicts" ]; then
  echo "ERROR: in an action list AND in $KEEP_FILE:$conflicts"
  echo "       Edit one side. Refusing to run."
  exit 1
fi

# =============================================================================
# Status report (used standalone via --status, and as post-apply verification)
# =============================================================================

run_status_report() {
  local pkg tag total fails

  echo "--- GMLogger settings ---"
  check_setting global gmlogger_enabled_status 0
  check_setting system gmlogger_enabled_status 0
  check_setting secure gmlogger_enabled 0
  check_setting global gmlogger_log_level -1
  check_setting system gmlogger_log_level 0
  check_setting global gmlogger_upload_enabled 0
  check_setting system gmlogger_upload_enabled 0
  check_setting global gmlogger_write_enabled 0
  check_setting system gmlogger_write_enabled 0
  check_setting global activity_starts_logging_enabled 0
  check_setting global dropbox_max_files 0
  check_setting global netstats_enabled 0
  check_prop persist.traced.enable 0
  echo ""

  echo "--- Log tags (${#LOG_TAGS[@]} total, only failures shown) ---"
  check_prop persist.log.tag S
  fails=0
  for tag in "${LOG_TAGS[@]}"; do
    check_tag_silent_quiet "$tag" || fails=$((fails+1))
  done
  echo "  $((${#LOG_TAGS[@]}-fails))/${#LOG_TAGS[@]} tags silenced"
  echo ""

  echo "--- Bloatware packages (${#BLOAT_PACKAGES[@]} total, only failures shown) ---"
  total=${#BLOAT_PACKAGES[@]}
  fails=0
  for pkg in "${BLOAT_PACKAGES[@]}"; do
    check_pkg_absent_quiet "$pkg" || fails=$((fails+1))
  done
  echo "  $((total-fails))/$total packages removed"
  echo ""

  echo "--- Disabled system packages ---"
  for pkg in "${DISABLE_PACKAGES[@]}"; do
    check_pkg_disabled "$pkg"
  done
  echo ""

  echo "--- Performance tuning ---"
  check_setting global window_animation_scale 0
  check_setting global transition_animation_scale 0
  check_setting global animator_duration_scale 0
  check_setting global app_standby_enabled 1
  echo ""

  echo "--- gmloggerd ---"
  local gmlog_cpu
  gmlog_cpu=$(adb shell top -b -n 1 2>/dev/null | grep gmloggerd | awk '{print $9}' | head -1)
  echo "  gmloggerd CPU: ${gmlog_cpu:-0}% (cannot be fully stopped without root)"
  echo ""
}

if [ "$MODE" = "status" ]; then
  run_status_report
  if [ "$FAIL_COUNT" -eq 0 ]; then
    echo "=== All checks passed ==="
  else
    echo "=== $FAIL_COUNT check(s) not applied — run without --status to apply ==="
  fi
  exit 0
fi

# =============================================================================
# SECTION 1: GMLogger Settings (Persistent - survives reboot)
# =============================================================================
echo ">>> [1/7] Configuring GMLogger settings (persistent)..."

adb shell settings put global gmlogger_enabled_status 0
adb shell settings put system gmlogger_enabled_status 0
adb shell settings put secure gmlogger_enabled 0

adb shell settings put global gmlogger_log_level -1
adb shell settings put system gmlogger_log_level 0

adb shell settings put global gmlogger_upload_enabled 0
adb shell settings put system gmlogger_upload_enabled 0
adb shell settings put global gmlogger_write_enabled 0
adb shell settings put system gmlogger_write_enabled 0

adb shell settings put global activity_starts_logging_enabled 0
adb shell settings put global dropbox_max_files 0
adb shell settings put global netstats_enabled 0

# Perfetto system tracing daemons (traced, traced_probes). Not needed for
# normal driving; takes effect on next boot since the daemons are already
# running and can't be killed without root.
adb shell setprop persist.traced.enable 0

echo "    GMLogger persistent settings applied."
echo ""

# =============================================================================
# SECTION 2: Log Tag Silencing (Persistent - survives reboot)
# =============================================================================
echo ">>> [2/7] Setting log tags to Silent (persist)..."

adb shell setprop persist.log.tag S
for tag in "${LOG_TAGS[@]}"; do
  adb shell setprop "persist.log.tag.${tag}" S
done

# Reduce logcat buffer
adb shell logcat -G 64K 2>/dev/null

# Clear log buffers
adb shell logcat -c 2>/dev/null
adb shell logcat -b all -c 2>/dev/null

# Disable other logging features
adb shell cmd looper_stats disable 2>/dev/null
adb shell cmd stats clear-puller-cache 2>/dev/null
adb shell dumpsys batterystats --disable full-wake-history 2>/dev/null

echo "    Log tags set to Silent (all persistent)."
echo ""

# =============================================================================
# SECTION 3: Uninstall Bloatware Packages
# =============================================================================
echo ">>> [3/7] Removing bloatware packages..."

for pkg in "${BLOAT_PACKAGES[@]}"; do
  remove_package "$pkg"
done

echo "    Bloatware removal complete."
echo ""

# =============================================================================
# SECTION 4: Disable System Telemetry Packages
# =============================================================================
echo ">>> [4/7] Disabling system telemetry packages..."

for pkg in "${DISABLE_PACKAGES[@]}"; do
  disable_package "$pkg"
done

echo "    System packages disabled."
echo ""

# =============================================================================
# SECTION 5: Performance Tuning (Persistent)
# =============================================================================
echo ">>> [5/7] Applying performance tuning..."

# Disable all UI animations (makes UI feel instant)
adb shell settings put global window_animation_scale 0
adb shell settings put global transition_animation_scale 0
adb shell settings put global animator_duration_scale 0

# Enable app standby (idle unused apps faster)
adb shell settings put global app_standby_enabled 1
adb shell settings put global always_finish_activities 0

# Reduce HWUI debug overhead
adb shell setprop debug.hwui.render_dirty_regions false
adb shell setprop debug.hwui.overdraw false

echo "    Performance tuning applied."
echo ""

# =============================================================================
# SECTION 6: Verification
# =============================================================================
echo ">>> [6/7] Verifying applied state..."
echo ""
FAIL_COUNT=0
run_status_report

# =============================================================================
# SECTION 7: COMPLETE
# =============================================================================
echo ">>> [7/7] Done."
echo ""
echo "=== Debloat Complete ($FAIL_COUNT check(s) not fully applied) ==="
echo ""
echo "NOTES:"
echo "  - GMLogger daemon cannot be fully stopped without root (~5-30% CPU)"
echo "  - Settings, log tags and perf tuning persist across reboots,"
echo "    but user 10's package set has been observed reverting on reboot -"
echo "    re-run this script after every boot"
echo "  - Run './gm_debloat_full.sh --status' any time to check state without"
echo "    changing anything"
echo ""
echo "To re-enable a disabled package:"
echo "  adb shell pm enable --user 10 <package>"
echo ""
