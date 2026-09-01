#!/bin/sh
########################
# ocbm_boot.sh - boot the host-facing gadget as a PURE OCBM accessory, launch ocbmd, and start the
# session supervisor (IDLE). Runs from start_main_service.sh when neither ncm_only nor ncm_wifi is set
# (the OCBM appliance default). Backgrounds all work + exit 0 so it can never block boot; UART
# early-console remains the recovery path. Uses bDeviceClass=0 (NOT 239) so the host sees a clean single
# vendor interface, not an IAD composite (which macOS seizes). Binaries are FHS-installed (/usr/sbin).
########################
# SYNCHRONOUS, and deliberately OUTSIDE the subshell below: runMainProcess skips ARMadb only if
# this flag exists by the time start_main_service.sh reaches it (~line 74). We are invoked at its
# line 19 and return immediately, so anything we background is racing those ~55 lines. It used to
# be set inside the subshell *after* a `tar -xzf`, i.e. the race could be lost on a slow unpack --
# and losing it means ARMadb starts, clobbers our accessory gadget, and the host sees a SECOND
# enumeration with a DIFFERENT identity. Hosts that key a USB permission grant on the descriptors
# (Android) treat that as a different device. One touch, one identity, one enumeration.
touch /tmp/UDiskPassThroughMode
(
  L=/tmp/ocbm_boot.log
  export PATH=/usr/sbin:/usr/bin:/sbin:/bin:/tmp/bin:$PATH
  echo "[ocbm-boot] start uptime=$(cut -d. -f1 /proc/uptime)s" > "$L"
  # stage + load the gadget modules (copy_to_tmp may not have run yet)
  [ -e /tmp/g_android_accessory.ko ] || { [ -e /script/ko.tar.gz ] && tar -xzf /script/ko.tar.gz -C /tmp 2>/dev/null; }
  grep -q storage_common /proc/modules || insmod /tmp/storage_common.ko 2>/dev/null
  grep -q g_android_accessory /proc/modules || insmod /tmp/g_android_accessory.ko 2>/dev/null
  A=/sys/class/android_usb_accessory/android0
  i=0; while [ ! -e "$A/enable" ] && [ "$i" -lt 50 ]; do i=$((i+1)); sleep 0.1; done
  [ -e "$A/enable" ] || { echo "[ocbm-boot] gadget sysfs never appeared" >> "$L"; exit 0; }
  echo 0 > "$A/enable"
  # PURE accessory device: class defined at the interface (0xFF), not a composite IAD.
  echo 0 > "$A/bDeviceClass"; echo 0 > "$A/bDeviceSubClass"; echo 0 > "$A/bDeviceProtocol"
  # PID 0x2d00 marks a DIFFERENT APPLICATION PROTOCOL on an otherwise identical pipe: stock/NCM is
  # 0x1520, and the endpoints + interface class are the same either way, so the PID is the only thing
  # in the descriptors telling a host this no longer speaks the Carlinkit protocol. That is what keeps
  # a native-protocol app (carlink_native filters 0x1520/0x1521 only — correct by design) from
  # claiming a converted box. Host apps that DO speak OCBM must list 0x1314:0x2d00 in their
  # usb_device_filter.xml + runtime allowlist, or Android never matches USB_DEVICE_ATTACHED and they
  # lose the implicit permission grant. See docs/carplay/00_ARCHITECTURE.md.
  echo 2d00 > "$A/idProduct"                # stable OCBM accessory PID (see comment above)
  echo accessory > "$A/functions"; echo 1 > "$A/enable"
  i=0; while [ ! -e /dev/usb_accessory ] && [ "$i" -lt 50 ]; do i=$((i+1)); sleep 0.1; done
  /usr/sbin/ocbmd >/tmp/ocbmd.log 2>&1 &
  OCBMD=$!
  # Session supervisor: idle-waits on host presence; a host-app SUBSCRIBE drives projection + ARM
  # (docs/carplay/02_SESSION_LIFECYCLE.md). Backgrounded, so it can never block boot. Box stays IDLE (phone unswitched) until a host.
  [ -x /script/session_supervisor.sh ] && setsid /script/session_supervisor.sh >/tmp/supervisor.log 2>&1 &
  echo "[ocbm-boot] armed functions=$(cat $A/functions) class=$(cat $A/bDeviceClass) pid=$(cat $A/idProduct) state=$(cat $A/state) acc=$([ -e /dev/usb_accessory ] && echo yes) ocbmd=$OCBMD sup=$(pgrep -f session_supervisor)" >> "$L"

  # ---- FIRST-BOOT DEAD-MAN (armed by ocbm_install.sh finalize: /script/ocbm_trial) --------
  # The installer tells the operator: "if this host does not confirm over the OCBM link within
  # 240s, the box restores ncm_only and reboots itself back to NCM." That promise was made by
  # the host and implemented NOWHERE - the flag was written and cleared host-side and no box
  # code ever read it. On a unit with no UART, a first boot where the OCBM stack does not come
  # up therefore had no way back at all: no NCM, no OCBM, and a finalize message telling the
  # operator to wait for a rescue that could never arrive. This is that rescue.
  #
  # It is deliberately SEPARATE from the failover watchdog below. That one asks "is our stack
  # healthy?" and only runs when opted into; this one asks "did a human confirm they can still
  # reach this box?" and runs exactly once, on the boot that flipped the default. A stack can
  # look perfectly healthy from the inside while being unreachable from the outside - wrong
  # gadget descriptors, a host that cannot claim the interface, a cable that never enumerates -
  # and only an outside acknowledgement can distinguish those.
  #
  # One-shot by construction: the confirm removes the flag, and a revert creates ncm_only, so
  # the box boots NCM and stays there until a human decides otherwise. A timed-out trial can
  # never become a reboot loop.
  if [ -e /script/ocbm_trial ]; then
  (
    T=/script/ocbm_trial.log
    echo "$(date) trial armed - waiting up to 240s for a host to confirm" >> "$T"
    i=0
    while [ "$i" -lt 240 ]; do
      [ -e /script/ocbm_trial ] || {
        echo "$(date) CONFIRMED by host at ${i}s - OCBM stays the default" >> "$T"; exit 0; }
      i=$((i+1)); sleep 1
    done
    echo "$(date) NO CONFIRMATION in ${i}s - restoring ncm_only and rebooting to NCM" >> "$T"
    touch /script/ncm_only || echo "$(date) WARNING: could not create ncm_only (rootfs full?)" >> "$T"
    rm -f /script/ocbm_trial
    sync; sleep 1; reboot
  ) &
  fi

  # ---- NCM FAILOVER WATCHDOG (opt-in: /script/ocbm_failover) ------------------------------
  # On a unit with no UART, OCBM-as-default is the only door. If the OCBM stack cannot come up
  # at all, this drops /script/ncm_only and reboots, so the box returns to NCM with ssh/telnet
  # on 192.168.50.2 instead of being unreachable.
  #
  # WHAT COUNTS AS FAILURE — both triggers mean OUR OWN STACK is broken:
  #   1. /dev/usb_accessory never appears: the accessory gadget never bound, so ocbmd has
  #      nothing to open. Nothing downstream can work.
  #   2. ocbmd will not stay running: repeatedly dead across the observation window, i.e. the
  #      inittab respawn is churning rather than holding.
  #
  # WHAT DELIBERATELY DOES NOT COUNT: no host talking to us. The gadget sitting at CONNECTED,
  # or CONFIGURED with silence on the wire, is the NORMAL state of an appliance in a car or on
  # a bench. Treating that as failure would make the box flap between modes every time it is
  # powered without a host, which is worse than either mode on its own. ocbmd exiting ONCE on
  # a host disconnect is also normal — that is what the respawn wrapper is for — hence a count,
  # not a single miss.
  #
  # It is one-shot by construction: once /script/ncm_only exists the box boots NCM and stays
  # there until a human removes it, so a persistent fault cannot become a reboot loop.
  if [ -e /script/ocbm_failover ]; then
  (
    W=/script/ocbm_failover.log
    fail() {
      echo "$(date) FAILOVER: $1 -> arming ncm_only and rebooting" >> "$W"
      touch /script/ncm_only; sync; sleep 1; reboot
    }
    i=0; while [ ! -e /dev/usb_accessory ] && [ "$i" -lt 60 ]; do i=$((i+1)); sleep 1; done
    [ -e /dev/usb_accessory ] || fail "/dev/usb_accessory never appeared in ${i}s"
    misses=0; t=0
    while [ "$t" -lt 120 ]; do
      if pidof ocbmd >/dev/null 2>&1; then misses=0; else
        misses=$((misses+1))
        [ "$misses" -ge 4 ] && fail "ocbmd not running on $misses consecutive checks by ${t}s"
      fi
      sleep 5; t=$((t+5))
    done
    echo "$(date) OCBM healthy at ${t}s (ocbmd pid=$(pidof ocbmd))" >> "$W"
  ) &
  fi
) &
exit 0
