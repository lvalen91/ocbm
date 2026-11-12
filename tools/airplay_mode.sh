#!/bin/sh
# airplay_mode.sh — the box half of AirPlay-only mode (OCBM MODE_AIRPLAY, docs/ocbm/02_AIRPLAY_ONLY_MODE.md).
# Installed as /script/airplay_mode.sh; its presence is what ocbmd reports as CAP_AIRPLAY. Run ONLY by
# session_supervisor.sh (airplay_tick), which has stopped the CarPlay stack (carplayd, iap2d, btd, AP)
# before `up` and waits for `down` before any CarPlay bring-up: the two modes never overlap.
#
#   airplay_mode.sh up        apply /tmp/airplay_mode.cfg (written by ocbmd from the host's request): the
#                             AP through the radio seam with EXACTLY its ssid/psk, then airplayd
#   airplay_mode.sh receiver  (re)start airplayd only (the supervisor's liveness check)
#   airplay_mode.sh down      airplayd stopped (mDNS goodbye), AP down, /etc/hostapd.conf restored
#
# The AP is always raised by /script/radio_hal.sh wifi_ap_on (the owned AP layer radio_ap_up.sh, never a
# chipset branch here); RADIO_AP_SSID tells the seam to use the host's SSID for this bring-up instead of
# the box identity. The credentials reuse CarPlay's own mechanism (session_supervisor.sh
# apply_host_wifi_creds): /etc/hostapd.conf is REBUILT from a pristine copy, never sed-substituted, so
# an SSID or passphrase holding / & \ $ cannot be read as syntax. That copy
# (/etc/hostapd.conf.pre_airplay) is put back by `down`, and by the next `up` or `down` after a power
# cut, so CarPlay's 0x5703 credentials never inherit this mode's passphrase.
#
# State for ocbmd (-> CT_AIRPLAY_STATE): /tmp/airplay_ap = "gen=<request>", "ap=<0 down|1 starting|2 up|
# 3 failed>". The passphrase is never echoed, logged or put in a process argument.
set -u
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:/opt/ocbm/bin:$PATH
CFG=/tmp/airplay_mode.cfg
APF=/tmp/airplay_ap
BAK=/etc/hostapd.conf.pre_airplay
GEN=""
log() { echo "[airplay-mode] $*" >> /tmp/box.log; }
val() { sed -n "s/^$1=//p" "$CFG" 2>/dev/null | head -1; }
ap_state() { printf 'gen=%s\nap=%s\ndetail=%s\n' "$GEN" "$1" "$2" > "$APF.tmp" && mv "$APF.tmp" "$APF"; }

stop_receiver() {
  _p=$(pidof airplayd 2>/dev/null) || return 0
  kill -TERM $_p 2>/dev/null
  # its mDNS goodbye takes about half a second: wait for it rather than leave a dead receiver in the
  # phone's list for the record TTL
  _n=0; while [ "$_n" -lt 25 ] && pidof airplayd >/dev/null 2>&1; do _n=$((_n + 1)); sleep 0.2; done
  if pidof airplayd >/dev/null 2>&1; then
    kill -KILL $(pidof airplayd) 2>/dev/null
    log "airplayd killed (no mDNS goodbye)"
  fi
  rm -f /tmp/airplay_state
}

start_receiver() {
  _bin=$(command -v airplayd 2>/dev/null) || _bin=""
  [ -n "$_bin" ] && [ -x "$_bin" ] || { log "airplayd is not installed (PATH=$PATH)"; return 1; }
  rm -f /tmp/airplay_state
  # --cfg carries name=, display= and hevc= only as far as airplayd is concerned; it never reads psk=
  setsid "$_bin" --cfg "$CFG" </dev/null >>/tmp/box.log 2>&1 &
  return 0
}

ap_down() {
  if pidof hostapd >/dev/null 2>&1; then
    sh /script/radio_hal.sh wifi_ap_off >/tmp/wlan_off.log 2>&1 || log "wifi_ap_off failed: $(tail -1 /tmp/wlan_off.log 2>/dev/null)"
  fi
}

restore_conf() {
  [ -f "$BAK" ] || return 0
  mv "$BAK" /etc/hostapd.conf && log "restored /etc/hostapd.conf (CarPlay's AP settings)"
}

do_up() {
  GEN=$(val gen); SSID=$(val ssid); PSK=$(val psk); CH=$(val channel)
  if [ -z "$GEN" ] || [ -z "$SSID" ] || [ "${#PSK}" -lt 8 ]; then
    log "up: no usable request in $CFG"; ap_state 3 "bad request"; return 1
  fi
  ap_state 1 "starting"
  stop_receiver
  # A running AP carries other credentials, and the seam reads "hostapd running" as converged.
  ap_down
  [ -f /etc/hostapd.conf ] || cp /opt/ocbm/etc/hostapd.conf /etc/hostapd.conf 2>/dev/null
  [ -f "$BAK" ] || cp -p /etc/hostapd.conf "$BAK" 2>/dev/null
  if [ ! -f "$BAK" ]; then
    log "up: no /etc/hostapd.conf to start from"; ap_state 3 "no hostapd.conf"; return 1
  fi
  # Only the keys being replaced are stripped: an unsupplied channel keeps the template's (this
  # hostapd has no ACS and will not start without one).
  _strip='^ssid=|^wpa_passphrase='
  case "${CH:-0}" in ''|0) CH=0 ;; *[!0-9]*) CH=0 ;; *) _strip="$_strip|^channel=" ;; esac
  (
    umask 077
    grep -vE "$_strip" "$BAK" > /etc/hostapd.conf.new || exit 1
    printf 'ssid=%s\nwpa_passphrase=%s\n' "$SSID" "$PSK" >> /etc/hostapd.conf.new
    grep -qE '^(wpa|wpa_key_mgmt)=' /etc/hostapd.conf.new \
      || printf 'wpa=2\nwpa_key_mgmt=WPA-PSK\nrsn_pairwise=CCMP\n' >> /etc/hostapd.conf.new
    [ "$CH" = 0 ] || printf 'channel=%s\n' "$CH" >> /etc/hostapd.conf.new
    mv /etc/hostapd.conf.new /etc/hostapd.conf
  ) || { log "up: could not write /etc/hostapd.conf"; ap_state 3 "hostapd.conf not written"; return 1; }
  RADIO_AP_SSID="$SSID" sh /script/radio_hal.sh wifi_ap_on >/tmp/wlan.log 2>&1
  _rc=$?
  # "Up" is a claim about THESE credentials: hostapd running, started from a file that holds exactly
  # the requested SSID and passphrase (the AP layer rewrites ssid= itself, from RADIO_AP_SSID).
  if [ "$_rc" = 0 ] && pidof hostapd >/dev/null 2>&1 \
     && grep -Fxq "ssid=$SSID" /etc/hostapd.conf && grep -Fxq "wpa_passphrase=$PSK" /etc/hostapd.conf; then
    _ch=$(sed -n 's/^channel=//p' /etc/hostapd.conf | head -1)
    log "AP up with the requested credentials: channel ${_ch:-?}, passphrase ${#PSK} chars, $(tail -1 /tmp/wlan.log 2>/dev/null | cut -c1-120)"
    ap_state 2 "up channel=${_ch:-?}"
    start_receiver || { ap_state 3 "airplayd missing"; return 1; }
    return 0
  fi
  log "AP did NOT come up (radio_hal rc=$_rc): $(tail -2 /tmp/wlan.log 2>/dev/null | tr '\n' ' ' | cut -c1-200)"
  ap_state 3 "radio seam rc=$_rc"
  return 1
}

case "${1:-}" in
  up) do_up ;;
  receiver) stop_receiver; start_receiver ;;
  down)
    stop_receiver
    ap_down
    restore_conf
    rm -f "$APF" /tmp/airplay_state
    log "down: receiver stopped, AP down"
    ;;
  *) sed -n '2,10p' "$0"; exit 64 ;;
esac
