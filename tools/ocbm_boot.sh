#!/bin/sh
########################
# ocbm_boot.sh - boot the host-facing gadget as a PURE OCBM accessory, launch ocbmd, and start the
# session supervisor (IDLE). Runs from start_main_service.sh when neither ncm_only nor ncm_wifi is set
# (the OCBM appliance default). Backgrounds all work + exit 0 so it can never block boot; UART
# early-console remains the recovery path. Uses bDeviceClass=0 (NOT 239) so the host sees a clean single
# vendor interface, not an IAD composite (which macOS seizes). Binaries are FHS-installed (/usr/sbin).
########################
(
  L=/tmp/ocbm_boot.log
  export PATH=/usr/sbin:/usr/bin:/sbin:/bin:/tmp/bin:$PATH
  echo "[ocbm-boot] start uptime=$(cut -d. -f1 /proc/uptime)s" > "$L"
  # stage + load the gadget modules (copy_to_tmp may not have run yet)
  [ -e /tmp/g_android_accessory.ko ] || { [ -e /script/ko.tar.gz ] && tar -xzf /script/ko.tar.gz -C /tmp 2>/dev/null; }
  touch /tmp/UDiskPassThroughMode
  grep -q storage_common /proc/modules || insmod /tmp/storage_common.ko 2>/dev/null
  grep -q g_android_accessory /proc/modules || insmod /tmp/g_android_accessory.ko 2>/dev/null
  A=/sys/class/android_usb_accessory/android0
  i=0; while [ ! -e "$A/enable" ] && [ "$i" -lt 50 ]; do i=$((i+1)); sleep 0.1; done
  [ -e "$A/enable" ] || { echo "[ocbm-boot] gadget sysfs never appeared" >> "$L"; exit 0; }
  echo 0 > "$A/enable"
  # PURE accessory device: class defined at the interface (0xFF), not a composite IAD.
  echo 0 > "$A/bDeviceClass"; echo 0 > "$A/bDeviceSubClass"; echo 0 > "$A/bDeviceProtocol"
  echo 2d00 > "$A/idProduct"                # stable OCBM accessory PID
  echo accessory > "$A/functions"; echo 1 > "$A/enable"
  i=0; while [ ! -e /dev/usb_accessory ] && [ "$i" -lt 50 ]; do i=$((i+1)); sleep 0.1; done
  /usr/sbin/ocbmd >/tmp/ocbmd.log 2>&1 &
  OCBMD=$!
  # Session supervisor: idle-waits on host presence; a host-app SUBSCRIBE drives projection + ARM
  # (docs/carplay/02_SESSION_LIFECYCLE.md). Backgrounded, so it can never block boot. Box stays IDLE (phone unswitched) until a host.
  [ -x /script/session_supervisor.sh ] && setsid /script/session_supervisor.sh >/tmp/supervisor.log 2>&1 &
  echo "[ocbm-boot] armed functions=$(cat $A/functions) class=$(cat $A/bDeviceClass) pid=$(cat $A/idProduct) state=$(cat $A/state) acc=$([ -e /dev/usb_accessory ] && echo yes) ocbmd=$OCBMD sup=$(pgrep -f session_supervisor)" >> "$L"
) &
exit 0
