#!/bin/sh
# ccpa_custom: owned -- minimal NCM-appliance boot orchestrator (rewritten from stock;
# all projection/vendor launches removed). Radios are on-demand (wlan_on/bt_on); NCM is
# the only always-on link. Primary NCM arm is /script/custom_init.sh (early); the block
# at the end is a fallback that runs only if custom_init did not pre-arm.

# --- early system setup ---
date -s "2020-01-02 `date +%T`" &
rm -f /dev/random && ln -s /dev/urandom /dev/random &                 # non-blocking openssl RNG
echo y > /sys/module/printk/parameters/time && echo 7 > /proc/sys/kernel/printk &

# --- custom early init (arms CDC-NCM before radios; sets /tmp/ncm_early_armed) ---
test -e /script/custom_init.sh && /script/custom_init.sh

# --- OCBM accessory appliance: default when no ncm_only/ncm_wifi flag. Boots the
#     host-facing gadget as a pure accessory + launches ocbmd (clean single-interface
#     enumeration; no live ncm->accessory flip needed). ---
if [ -e /script/ncm_only ] || [ -e /script/ncm_wifi ]; then :; else
	test -e /script/ocbm_boot.sh && /script/ocbm_boot.sh
fi

# --- hardware GPIO (BT reset + quick-charge lines) ---
test -e /script/init_gpio.sh && /script/init_gpio.sh &

# --- stage gadget kernel modules (g_android_accessory.ko etc.) to /tmp ---
test -e /script/copy_to_tmp.sh && /script/copy_to_tmp.sh

# --- radios: OFF at boot (on-demand: wlan_on/bt_on to raise, wlan_off/bt_off to release) ---
echo "[start_main_service] radios on-demand (wlan_on/bt_on); NCM only always-on"

# --- NCM network (ncm0 addr/mtu) ---
test -e /script/start_ncm.sh && /script/start_ncm.sh

# --- USB storage: auto-mount a FAT32 stick at /mnt/UPAN if present at boot (mdev handles
#     hotplug-after-boot). Backgrounded; no-op when no stick. ---
test -e /script/mount_usb.sh && /script/mount_usb.sh 12 &

# ===== NCM fallback arm (runs only if custom_init did NOT pre-arm) =====
(
  { [ -e /script/ncm_only ] || [ -e /script/ncm_wifi ]; } || exit 0
  [ -e /tmp/ncm_early_armed ] && exit 0
  L=/tmp/ncm_boot.log
  echo "[ncm] late fallback arm (custom_init did not pre-arm) $(date)" > "$L"
  A=/sys/class/android_usb_accessory/android0
  i=0
  while [ $i -lt 60 ]; do
    [ "$(cat $A/state 2>/dev/null)" = "CONFIGURED" ] && break
    i=$((i+1)); sleep 1
  done
  sleep 10
  touch /tmp/UDiskPassThroughMode
  echo 0 > /sys/class/android_usb/android0/enable 2>/dev/null          # release phone-side ncm0 owner
  echo 0 > "$A/enable"
  echo 239 > "$A/bDeviceClass"; echo 2 > "$A/bDeviceSubClass"; echo 1 > "$A/bDeviceProtocol"
  echo ncm > "$A/functions"; echo 1 > "$A/enable"
  sleep 2
  echo 0 > "$A/enable"; sleep 2; echo ncm > "$A/functions"; echo 1 > "$A/enable"
  sleep 2
  ifconfig ncm0 192.168.50.2 netmask 255.255.255.0 mtu 1500 up
  cat > /tmp/udhcpd_ncm.conf <<CFG
start 192.168.50.100
end 192.168.50.150
interface ncm0
opt subnet 255.255.255.0
opt lease 86400
lease_file /tmp/udhcpd_ncm.leases
pidfile /tmp/udhcpd_ncm.pid
max_leases 20
CFG
  touch /tmp/udhcpd_ncm.leases
  pkill -f udhcpd_ncm.conf 2>/dev/null; sleep 1
  busybox udhcpd -f /tmp/udhcpd_ncm.conf >/tmp/udhcpd_ncm.log 2>&1 &
  sleep 2
  if [ "$(cat $A/state)" != "CONFIGURED" ] || [ "$(cat $A/functions)" != "ncm" ]; then
    echo 0 > "$A/enable"; sleep 2; echo ncm > "$A/functions"; echo 1 > "$A/enable"; sleep 2
    ifconfig ncm0 192.168.50.2 netmask 255.255.255.0 mtu 1500 up
  fi
  echo "[ncm] armed functions=$(cat $A/functions) state=$(cat $A/state)" >> "$L"
) &
