#!/bin/sh
########################
# bt_on.sh - on-demand Bluetooth bring-up (NXP IW416). Independent of WiFi.
# BT name = ccpa-<4 hex from WiFi MAC> (matches WiFi SSID). Forks bluetoothDaemon/hcid/hfpd.
########################
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:/tmp/bin:$PATH
KO=/lib/firmware/nxp/iw416_ko.tar.gz

ID=$(set_wifi_mac 2>/dev/null | grep -oE '([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}' | head -1)
SUF=$(echo "$ID" | sed 's/.*\(..\):\(..\)$/\1\2/' | tr 'A-F' 'a-f')
[ -n "$SUF" ] || SUF=$(cat /etc/serial_number 2>/dev/null | tr -cd '0-9a-fA-F' | tr 'A-F' 'a-f' | sed 's/.*\(....\)$/\1/')
NAME="ccpa-$SUF"

# set BT name BEFORE bluetoothDaemon starts so it advertises the right name.
# /etc/.custom_bluetooth_name is the flag bluetoothDaemon honors to apply a custom name
# (without it the daemon blanks the adapter name during init).
echo "$NAME" > /etc/.custom_bluetooth_name
echo "$NAME" > /etc/bluetooth_name
[ -e /etc/bluetooth/hcid.conf ] && sed -i "s/^\(\s*\)name \".*\";/\1name \"$NAME\";/" /etc/bluetooth/hcid.conf

if [ ! -e /tmp/hci_uart.ko ] && ! grep -q "^hci_uart " /proc/modules; then
	echo "[bt_on] staging hci_uart.ko"; tar -xf "$KO" -C /tmp 2>/dev/null
fi
if ps | grep -v grep | grep -q "[ /]bluetoothDaemon"; then
	echo "[bt_on] bluetoothDaemon already running"
else
	rm -f /tmp/.hciattach_done
	/script/attach_bluetooth.sh &
fi
# docs/wireless/01_BT_AND_RADIO.md: wait for attach_bluetooth.sh to actually CONVERGE (its own bt_responsive() retry loop
# reaching success or giving up after 20 attempts), not just for hci0 to exist. hci0 can appear
# within ~100ms of hciattach even when the chip's firmware never loaded and every subsequent
# command times out -- waiting on mere existence let wireless_up() launch carplay-wireless (which
# does its OWN redundant killall+hciconfig Bluetooth bring-up) WHILE attach_bluetooth.sh's retry
# loop was still mid-flight; the two uncoordinated bring-up attempts fought each other indefinitely
# (observed live: 7+ minutes, never converging -- carplay-wireless's periodic killall bluetoothDaemon
# kept interrupting attach_bluetooth.sh's in-progress reset/reattach). attach_bluetooth.sh touches
# /tmp/.hciattach_done as its LAST statement, after the retry loop concludes either way; bt_off.sh
# clears it, so a stale flag from a prior session is never mistaken for this attempt's completion.
# 500s covers attach_bluetooth.sh's worst case (20 retries, each up to ~20-25s when the chip is
# genuinely unresponsive: reset_bt's ~2s + fw_loader/hciattach + several hcitool/hciconfig calls
# that can each time out for a few seconds + bt_responsive's own 5s bound).
i=0; while [ ! -e /tmp/.hciattach_done ] && [ "$i" -lt 500 ]; do i=$((i+1)); sleep 1; done
[ -e /tmp/.hciattach_done ] || echo "[bt_on] attach_bluetooth.sh did not finish within 500s -- proceeding anyway"
hciconfig hci0 2>&1 | grep -E "BD Address|UP RUNNING" || echo "[bt_on] hci0 not up yet"
# bluetoothDaemon blanks the adapter name during its init; assert AFTER it settles, retry
# until it sticks (handles the race where our set lands before the daemon's reset).
n=0
while [ "$n" -lt 10 ]; do
	hciconfig hci0 name "$NAME" 2>/dev/null
	sleep 0.5
	cur=$(hciconfig hci0 name 2>/dev/null | sed -n "s/.*Name: '\(.*\)'.*/\1/p")
	[ "$cur" = "$NAME" ] && break
	n=$((n+1))
done
echo "[bt_on] BT name=$NAME  hci0 now: '$cur'  (asserted after ${n} retries)"
