#!/bin/sh
# probe_ctl.sh - run the AirPlay Phase-0 probe on the box. Lives in /tmp on the box (RAM); bench
# instrument, never installed, never in the overlay. Everything it touches is tmpfs: it REFUSES to
# run unless /tmp and /etc are tmpfs, so nothing here can reach flash.
#
#   probe_ctl.sh ap-up <ssid> <passphrase>   bring up the box AP through the owned seam
#   probe_ctl.sh ap-down                     stop the AP, restore /etc/hostapd.conf + udhcpd.conf
#   probe_ctl.sh start SPEC [SPEC...]       SPEC = TAG[/MASK[/opt...]], e.g. A  B  B7/B+7  Ar/A/raop
#   probe_ctl.sh stop | status | log [N]
set -u
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:/tmp/bin:$PATH
BIN=/tmp/airplay-probe
LOG=/tmp/airplay-probe.log
BAK=/tmp/airplay-probe.bak
L="[probe_ctl]"

tmpfs_or_die() {
  for m in /tmp /etc; do
    awk -v m="$m" '$2==m{t=$3} END{exit !(t=="tmpfs")}' /proc/mounts \
      || { echo "$L $m is not tmpfs - refusing (this probe must not write to flash)"; exit 1; }
  done
}

# The interface name is an insmod parameter, not a constant: enumerate, never assume wlan0.
wlan_if() {
  for d in /sys/class/net/*; do
    [ -e "$d/phy80211" ] || [ -e "$d/wireless" ] || continue
    echo "${d##*/}"; return 0
  done
  return 1
}

probe_pids() { pidof airplay-probe 2>/dev/null; }

# The AP's own DHCP server only: the exact argv radio_ap_up.sh starts. Never a killall - the NCM
# link runs another udhcpd, and that is the management channel.
ap_dhcp_pids() {
  for p in /proc/[0-9]*; do
    # A pid can exit between the glob and the read; the group swallows the shell's own open error.
    [ "$({ tr '\0' ' ' < "$p/cmdline"; } 2>/dev/null)" = "udhcpd /etc/udhcpd.conf " ] && echo "${p##*/}"
  done
}

do_stop() {
  pids=$(probe_pids)
  [ -n "$pids" ] || { echo "$L probe not running"; return 0; }
  kill -TERM $pids 2>/dev/null
  n=0; while [ "$n" -lt 30 ] && [ -n "$(probe_pids)" ]; do n=$((n+1)); sleep 0.2; done
  if [ -n "$(probe_pids)" ]; then kill -KILL $(probe_pids) 2>/dev/null; echo "$L probe killed (no mDNS goodbye)"
  else echo "$L probe stopped (mDNS goodbye sent)"; fi
}

case "${1:-}" in
  ap-up)
    tmpfs_or_die
    SSID=${2:-}; PSK=${3:-}
    [ -n "$SSID" ] && [ "${#PSK}" -ge 8 ] || { echo "usage: $0 ap-up <ssid> <passphrase (8..63)>"; exit 64; }
    case "$SSID$PSK" in *[!A-Za-z0-9_-]*) echo "$L ssid/passphrase: letters, digits, _ and - only"; exit 64 ;; esac
    IF=$(wlan_if) || { echo "$L no wireless interface - radio up"; radio up; sleep 2; IF=$(wlan_if) || { echo "$L still none"; exit 1; }; }
    [ -f /etc/hostapd.conf ] || cp /opt/ocbm/etc/hostapd.conf /etc/hostapd.conf
    [ -f /etc/udhcpd.conf ] || cp /opt/ocbm/etc/udhcpd.conf /etc/udhcpd.conf
    if [ ! -d "$BAK" ]; then
      mkdir -p "$BAK"
      for f in hostapd.conf udhcpd.conf wifi_name; do [ -f "/etc/$f" ] && cp "/etc/$f" "$BAK/$f"; done
    fi
    if grep -q '^wpa_passphrase=' /etc/hostapd.conf; then
      sed -i "s/^wpa_passphrase=.*/wpa_passphrase=$PSK/" /etc/hostapd.conf
    else
      echo "wpa_passphrase=$PSK" >> /etc/hostapd.conf
    fi
    [ -n "${PROBE_CHANNEL:-}" ] && sed -i "s/^channel=.*/channel=$PROBE_CHANNEL/" /etc/hostapd.conf
    RADIO_WLAN_IF="$IF" RADIO_BOX_NAME="$SSID" sh /script/radio_ap_up.sh AP
    exit $?
    ;;
  ap-down)
    tmpfs_or_die
    hp=$(pidof hostapd 2>/dev/null); [ -n "$hp" ] && kill $hp 2>/dev/null
    dp=$(ap_dhcp_pids); [ -n "$dp" ] && kill $dp 2>/dev/null
    IF=$(wlan_if) && { ifconfig "$IF" 0.0.0.0 2>/dev/null; ifconfig "$IF" down 2>/dev/null; }
    if [ -d "$BAK" ]; then
      for f in hostapd.conf udhcpd.conf wifi_name; do [ -f "$BAK/$f" ] && cp "$BAK/$f" "/etc/$f"; done
      rm -rf "$BAK"; echo "$L restored /etc/hostapd.conf, udhcpd.conf, wifi_name"
    fi
    echo "$L AP down"
    ;;
  start)
    tmpfs_or_die
    shift
    [ "$#" -ge 1 ] || { echo "usage: $0 start SPEC [SPEC...]   (SPEC = TAG[/MASK[/opt...]], see $BIN)"; exit 64; }
    [ -x "$BIN" ] || { echo "$L $BIN missing"; exit 1; }
    do_stop
    # One process, every SPEC at once: receiver names ${PROBE_NAME:-OCBM}-<TAG>, ports 7000+n.
    setsid "$BIN" --name "${PROBE_NAME:-OCBM}" --log "$LOG" "$@" </dev/null >/dev/null 2>>/tmp/airplay-probe.err &
    sleep 1
    if [ -n "$(probe_pids)" ]; then echo "$L started (pid $(probe_pids)), log $LOG"; grep '^.*\] variant .* mask ' "$LOG" | tail -n "$#" | cut -c24-140
    else echo "$L probe did not stay up:"; tail -5 "$LOG" /tmp/airplay-probe.err 2>/dev/null; exit 1; fi
    ;;
  stop) do_stop ;;
  status)
    echo "$L probe pid: $(probe_pids || true)"
    grep '==== airplay-probe st' "$LOG" 2>/dev/null | tail -1
    IF=$(wlan_if) && {
      echo "$L wlan $IF: $(ifconfig "$IF" 2>/dev/null | sed -n 's/.*\(inet addr:[0-9.]*\).*/\1/p') hostapd pid: $(pidof hostapd 2>/dev/null) ap-dhcp pid: $(ap_dhcp_pids)"
      echo "$L ssid: $(sed -n 's/^ssid=//p' /etc/hostapd.conf 2>/dev/null) channel: $(sed -n 's/^channel=//p' /etc/hostapd.conf 2>/dev/null)"
      iw dev "$IF" info 2>/dev/null | grep -E 'type|channel|ssid'
      echo "$L stations:"; iw dev "$IF" station dump 2>/dev/null | grep -E '^Station|signal:|connected time'
    }
    echo "$L leases:"; dumpleases -f /var/lib/udhcpd.leases 2>/dev/null
    [ -f "$LOG" ] && echo "$L log: $(wc -l < "$LOG") lines, $(grep -c ' CONNECT from ' "$LOG") connection(s)"
    ;;
  log) tail -n "${2:-60}" "$LOG" ;;
  *) sed -n '2,10p' "$0"; exit 64 ;;
esac
