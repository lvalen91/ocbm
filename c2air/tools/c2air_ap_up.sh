#!/bin/sh
# c2air_ap_up.sh — bring up the C2Air's hosted Wi-Fi AP for wireless CarPlay. Runs ON THE BOX.
#
# This is the C2Air counterpart to the CCPA's /script/radio_ap_up.sh and it deliberately mirrors that
# script's proven sequence (ip -> hostapd -> udhcpd -> converge-or-report). It exists separately
# because three things differ on this board, each of which breaks the CCPA version outright:
#
#   1. `/etc` IS A READ-ONLY SQUASHFS. radio_ap_up.sh `sed -i`s /etc/hostapd.conf and /etc/udhcpd.conf
#      in place; here every write would fail. Both configs are GENERATED into /tmp from the vendor
#      templates instead, and the generated hostapd.conf is the single source of truth for the
#      credentials — carplay-wireless must be pointed at the SAME file via CARPLAY_HOSTAPD_CONF, or
#      the 0x5703 reply describes an AP that does not exist.
#   2. The vendor templates live at `/etc/wifi/{hostapd,udhcpd-cp}.conf`, not `/etc/{hostapd,udhcpd}.conf`.
#      `/etc/hostapd.conf` does not exist at all — which is exactly the failure the box reported:
#          [wifi_handoff] WARNING: /etc/hostapd.conf is empty or unreadable — 0x5703 will carry no credentials
#   3. THE SUBNET MUST BE 192.168.43.0/24, not the vendor's 192.168.50.0/24. This is not cosmetic and
#      not negotiable from the box side: `av.rs:66` hardcodes `AP_IP = "192.168.43.1"` and passes it to
#      rx-connect as RX_ADDR, and `airplayd` decides a control peer is WIRELESS by testing membership
#      in 192.168.43.0/24 (main.rs:1243). On the vendor's 192.168.50.x a phone would associate, get an
#      address, and then be classified as a WIRED peer. The shared Rust is proven on two CCPAs and is
#      not being changed to suit this board, so the board conforms to it.
#      (192.168.50.x is also the CCPA's NCM subnet, which radio_ap_up.sh explicitly refuses for /etc/wifi_ip.)
#
# The vendor lease file at /mnt/UDISK/dnsmasq.leases is ALSO redirected: UDISK has ~316 KB free and
# holds the per-unit carplay.key. Leases go to /tmp.
#
# Usage (on the box):  /tmp/c2air_ap_up.sh [start|status|stop]
# Then start carplay-wireless with:  CARPLAY_HOSTAPD_CONF=/tmp/hostapd.conf
set -u
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:$PATH
L="[c2air-ap]"

# THE OCBM BASELINE STRIP BROKE hostapd AND wpa_supplicant. Both are dynamically linked against
# `libnl-tiny.so`, which the strip removed as "orphaned" — the baseline README's claim that the
# DT_NEEDED closure was verified with no missing libraries is WRONG for these two binaries. Symptom:
#     Error relocating /usr/sbin/hostapd: genl_ctrl_resolve: symbol not found   (and ~8 more)
# `/lib` is read-only squashfs so the library cannot be put back at runtime; stage it on tmpfs and
# point the dynamic loader at it. Recover the file from the STOCK dump, which still has it:
#     unzip -o -j rootfs_c2air_2026.05.14.zip '*libnl*' -d /tmp/c2stock
#     adb push /tmp/c2stock/libnl-tiny.so /tmp/lib/libnl-tiny.so
# The proper fix is to restore it to /usr/lib in the next rootfs image.
export LD_LIBRARY_PATH=/tmp/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}

IF=${RADIO_WLAN_IF:-wlan0}
WLANIP=${CARPLAY_AP_IP:-192.168.43.1}      # MUST match av.rs AP_IP — see header note 3
HOSTAPD_CONF=/tmp/hostapd.conf
UDHCPD_CONF=/tmp/udhcpd.conf
TMPL_HOSTAPD=/etc/wifi/hostapd.conf
TMPL_UDHCPD=/etc/wifi/udhcpd-cp.conf

running() { ps | grep -v grep | grep -q "$1"; }

stop_ap() {
  busybox pkill -x hostapd 2>/dev/null
  busybox pkill -f "udhcpd $UDHCPD_CONF" 2>/dev/null
  ifconfig "$IF" 0.0.0.0 down 2>/dev/null
  echo "$L stopped"
}

status_ap() {
  echo "$L if=$IF"
  running "[h]ostapd"                && echo "$L   hostapd: running" || echo "$L   hostapd: NOT running"
  running "[u]dhcpd $UDHCPD_CONF"    && echo "$L   udhcpd:  running" || echo "$L   udhcpd:  NOT running"
  ifconfig "$IF" 2>/dev/null | grep -q "inet addr:$WLANIP" \
    && echo "$L   $IF holds $WLANIP" || echo "$L   $IF does NOT hold $WLANIP"
  [ -f "$HOSTAPD_CONF" ] && echo "$L   ssid=$(sed -n 's/^ssid=//p' "$HOSTAPD_CONF" | head -1) ch=$(sed -n 's/^channel=//p' "$HOSTAPD_CONF" | head -1)"
}

start_ap() {
  [ -e "/sys/class/net/$IF" ] || { echo "$L $IF does not exist (is the AIC8800 driver up?)"; exit 1; }
  # Same guard as radio_ap_up.sh: never reconfigure a non-wireless interface.
  [ -e "/sys/class/net/$IF/wireless" ] || [ -e "/sys/class/net/$IF/phy80211" ] \
    || { echo "$L $IF is not a wireless interface - refusing"; exit 1; }
  [ -f "$TMPL_HOSTAPD" ] || { echo "$L missing template $TMPL_HOSTAPD"; exit 1; }
  # Fail here with the real reason rather than letting hostapd die on relocation errors later.
  [ -f /tmp/lib/libnl-tiny.so ] || {
    echo "$L /tmp/lib/libnl-tiny.so is missing — hostapd/wpa_supplicant cannot link without it."
    echo "$L see the header: recover it from the stock rootfs zip and push it to /tmp/lib/."
    exit 1; }

  # ---- hostapd.conf: copy the vendor template, then override only what must change --------------
  # Device-derived SSID so two boxes on one bench do not collide, matching the BT accessory-name
  # convention (CarLink-<last 4 of the wlan0 MAC>) that the phone already saw during pairing.
  SUF=$(tr -d ':' < "/sys/class/net/$IF/address" 2>/dev/null | tr 'A-F' 'a-f' | sed 's/.*\(....\)$/\1/')
  case "$SUF" in ''|*[!0-9a-f]*) SUF="0000" ;; esac
  NAME=${CARPLAY_AP_SSID:-CarLink-$SUF}

  # grep -v then append: the template is read-only, and this avoids sed -i entirely.
  # Channel override + the hw_mode that goes with it. radio_ap_up.sh does the same 1-14 => hw_mode=g
  # mapping; a 2.4 GHz channel left on hw_mode=a is rejected outright.
  CH=${CARPLAY_AP_CHANNEL:-$(sed -n 's/^channel=//p' "$TMPL_HOSTAPD" | head -1)}
  case "$CH" in ''|*[!0-9]*) CH=36 ;; esac
  if [ "$CH" -ge 1 ] && [ "$CH" -le 14 ]; then MODE=g; else MODE=a; fi

  grep -vE '^(interface|ssid|ctrl_interface|noscan|channel|hw_mode|ieee80211ac|ieee80211ax|ht_capab)=' \
      "$TMPL_HOSTAPD" > "$HOSTAPD_CONF"
  {
    echo "channel=$CH"
    echo "hw_mode=$MODE"
    # 11ac/11ax and HT40 are re-added only on 5 GHz. On 2.4 GHz the vendor template's
    # `ieee80211ac=1`/`ieee80211ax=1` + `[HT40+]` are invalid or unsupported and stop the AP starting.
    if [ "$MODE" = a ]; then
      echo "ieee80211ac=1"
      echo "ieee80211ax=1"
      echo "ht_capab=[SHORT-GI-20][SHORT-GI-40][HT40+]"
    else
      echo "ht_capab=[SHORT-GI-20]"
    fi
    echo "interface=$IF"
    echo "ssid=$NAME"
    # /var/run may not be writable here; keep the control socket on tmpfs.
    echo "ctrl_interface=/tmp/hostapd_ctrl"
    # NOTE: do NOT add `noscan=1` here. This board's hostapd v2.10 build does not implement it and
    # rejects the whole file — "Line N: unknown configuration item 'noscan'" — so the AP silently
    # never starts while a stale hostapd from a previous run keeps the converge check happy.
  } >> "$HOSTAPD_CONF"

  PSK=$(sed -n 's/^wpa_passphrase=//p' "$HOSTAPD_CONF" | head -1)
  [ "${#PSK}" -ge 8 ] || { echo "$L wpa_passphrase in $TMPL_HOSTAPD is shorter than 8 chars - hostapd will refuse"; exit 1; }

  # ---- udhcpd.conf: vendor template, but OUR subnet and a tmpfs lease file ---------------------
  BASE=${WLANIP%.*}
  sed -e "s/^start.*/start\t${BASE}.100/" \
      -e "s/^end.*/end\t${BASE}.200/" \
      -e "s|^lease_file.*|lease_file=/tmp/udhcpd.leases|" \
      -e "s/^interface.*/interface\t$IF/" \
      "$TMPL_UDHCPD" > "$UDHCPD_CONF"
  grep -q "^option[[:space:]]*router" "$UDHCPD_CONF" || echo "option	router	$WLANIP" >> "$UDHCPD_CONF"
  : > /tmp/udhcpd.leases

  # ---- bring it up ----------------------------------------------------------------------------
  echo 1 > "/proc/sys/net/ipv6/conf/$IF/disable_ipv6" 2>/dev/null
  ifconfig "$IF" "$WLANIP" netmask 255.255.255.0 mtu 1500 up \
    || { echo "$L could not configure $IF"; exit 1; }

  # `-B` BEFORE the file. radio_ap_up.sh writes `hostapd /etc/hostapd.conf -B`, which this board's
  # hostapd v2.10 parses as a SECOND config file: "Could not open configuration file '-B'".
  running "[h]ostapd" || hostapd -B "$HOSTAPD_CONF" > /tmp/ap_hostapd.log 2>&1
  running "[u]dhcpd $UDHCPD_CONF" \
    || setsid udhcpd "$UDHCPD_CONF" </dev/null > /tmp/ap_dhcp.log 2>&1 &

  # ---- converge or say exactly what is missing (radio_ap_up.sh's discipline) --------------------
  n=0
  while [ "$n" -lt 50 ]; do
    running "[h]ostapd" \
      && ifconfig "$IF" 2>/dev/null | grep -q "inet addr:$WLANIP" \
      && running "[u]dhcpd $UDHCPD_CONF" && break
    n=$((n+1)); usleep 200000
  done

  if running "[h]ostapd" && ifconfig "$IF" 2>/dev/null | grep -q "inet addr:$WLANIP" && running "[u]dhcpd $UDHCPD_CONF"; then
    echo "$L AP up: ssid=$NAME psk=$PSK ip=$WLANIP ch=$CH if=$IF dhcp=${BASE}.100-200"
    echo "$L point carplay-wireless at it:  CARPLAY_HOSTAPD_CONF=$HOSTAPD_CONF"
    exit 0
  fi
  echo "$L AP did NOT converge (ssid=$NAME ch=$CH if=$IF):"
  running "[h]ostapd" || { echo "$L   hostapd not running:"; tail -8 /tmp/ap_hostapd.log 2>/dev/null | sed "s|^|$L     |"; }
  ifconfig "$IF" 2>/dev/null | grep -q "inet addr:$WLANIP" || echo "$L   $IF does not hold $WLANIP"
  running "[u]dhcpd $UDHCPD_CONF" || { echo "$L   udhcpd not running - a phone would associate and get NO address:";
                                       tail -5 /tmp/ap_dhcp.log 2>/dev/null | sed "s|^|$L     |"; }
  exit 1
}

case "${1:-start}" in
  start)  start_ap ;;
  stop)   stop_ap ;;
  status) status_ap ;;
  *) echo "usage: $0 [start|status|stop]"; exit 2 ;;
esac
