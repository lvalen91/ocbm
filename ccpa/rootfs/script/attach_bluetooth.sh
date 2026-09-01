#!/bin/sh
# Slim IW416 (SDIO 0x9159) Bluetooth attach — owned rewrite.
# Loads hci_uart, the NXP BT firmware over ttymxc2, attaches HCI, sets MAC/HFP/BLE,
# and starts bluetoothDaemon. All non-IW416 chip branches removed.
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:/tmp/bin:$PATH

# Make sure the N_HCI line discipline exists before anything tries to use it.
#
# hci_uart is a LOADABLE MODULE on this box (RFCOMM/SCO/L2CAP/HCI core are built in; only this one
# is not -- docs/wireless/00_WIRELESS_CARPLAY.md). It ships inside /lib/firmware/nxp/iw416_ko.tar.gz, and historically only
# wlan_on.sh extracted that tarball, into /tmp. So BT attach silently depended on WLAN bring-up
# having run first, on a tmpfs that every reboot wipes.
#
# When /tmp/hci_uart.ko was missing the old line below was `test -e ... && insmod`, which SKIPS
# WITHOUT A WORD. The failure then surfaced three steps later as hciattach's
# "Can't set line discipline: Invalid argument" -- with a flawless firmware download right above it
# (ChipID 7201, "Download Complete"), so every symptom pointed at the chip or the UART while the
# real fault was a missing precondition that reported nothing. Cost: no hci0, no BT, no pairing,
# with no log line naming the cause. Device-diagnosed 2026-08-28.
#
# So: extract it ourselves if it is absent, and be LOUD if it still cannot be loaded.
ensure_hci_ldisc() {
	grep -q "^hci_uart " /proc/modules && return 0
	if [ ! -e /tmp/hci_uart.ko ]; then
		echo "[attach_bluetooth] /tmp/hci_uart.ko absent (tmpfs wiped by a reboot?) -- extracting it"
		( cd /tmp && tar xzf /lib/firmware/nxp/iw416_ko.tar.gz hci_uart.ko 2>/dev/null \
		  || tar xzf /lib/firmware/nxp/iw416_ko.tar.gz 2>/dev/null )
	fi
	if [ ! -e /tmp/hci_uart.ko ]; then
		echo "[attach_bluetooth] FATAL: no hci_uart.ko and none in /lib/firmware/nxp/iw416_ko.tar.gz."
		echo "[attach_bluetooth]        Without it N_HCI is never registered and hciattach CANNOT work."
		return 1
	fi
	insmod /tmp/hci_uart.ko 2>&1
	if grep -q "^n_hci" /proc/tty/ldiscs 2>/dev/null; then
		echo "[attach_bluetooth] hci_uart loaded; n_hci line discipline registered"
		return 0
	fi
	echo "[attach_bluetooth] FATAL: hci_uart insmod did not register n_hci -- hciattach will fail EINVAL"
	return 1
}

attach_bt() {
	ensure_hci_ldisc || echo "[attach_bluetooth] proceeding into a known-doomed attach (see FATAL above)"
	fw_loader_linux /dev/ttymxc2 115200 1 /lib/firmware/nxp/uartiw416_bt_v0.bin 3000000
	hciattach /dev/ttymxc2 any 3000000 flow
	hciconfig hci0 up
	if ! cat /sys/class/bluetooth/hci0/address 2>/dev/null | grep -qi "38:ba:b0"; then
		nxpBTMac=$(set_wifi_mac | sed "s/Setting Wi-Fi MAC address: 00:E0:4C/38:BA:B0/")
		BTMac=$(echo "$nxpBTMac" | awk -F: '{print $6, $5, $4, $3, $2, $1}')
		hcitool -i hci0 cmd 3f 61 00 01 02 1c 37 e0 1c 00 ff ff ff ff 01 8f 08 04 08 00 00 00 c0 c6 2d 00 $BTMac f0 00
		# docs/wireless/01_BT_AND_RADIO.md: a controller reset restores the SPEC-DEFAULT HCI event mask (events 0x01-0x2D only).
		# The SSP events -- IO_Capability_Request (0x31), User_Confirmation_Request (0x33),
		# User_Passkey_Request (0x34), Simple_Pairing_Complete (0x36) -- are enabled ONLY by the kernel's
		# init-time Set_Event_Mask inside hci_dev_do_open(), i.e. an HCIDEVUP on a DOWN device.
		# `hciconfig reset` (hci_dev_reset) does NOT re-init, so a bare reset silently disables all SSP
		# pairing events while SDP/ACL keep working. Always follow a reset with down+up to force re-init.
		hciconfig hci0 reset
		hciconfig hci0 down
		hciconfig hci0 up
	fi
	hciconfig hci0 scomtu 240:32                    # HFP voice quality
	hcitool -i hci0 cmd 0x3f 0x1d 0x00              # route SCO to HCI
	hcitool -i hci0 cmd 0x3F 0x00EE 0x01 0x02       # BLE power
	# docs/wireless/01_BT_AND_RADIO.md: REMOVED a raw `hcitool -i hci0 cmd 0x03 0x0003` (HCI_Reset) that used to sit here as the
	# LAST controller operation of attach_bt(). Issued behind the kernel's back it wiped the event mask
	# (see the note above) AND undid the two vendor commands immediately preceding it, so fresh SSP
	# pairing became impossible -- the controller never delivered IO_Capability_Request to the host, the
	# kernel never replied, and the iPhone reported "Pairing Unsuccessful" with zero mgmt/dmesg evidence.
	# Bonded RECONNECTS still worked (Link_Key_Request 0x17 is inside the default mask), which is why the
	# 2026-07-14 logs looked healthy. Do not reintroduce a raw reset here.
}

reset_bt() {
	killall hciattach 2>/dev/null
	grep -q "^hci_uart " /proc/modules && { sleep 2; rmmod hci_uart 2>/dev/null; }
	echo 1 > /sys/class/gpio/gpio1/value; sleep 0.1; echo 0 > /sys/class/gpio/gpio1/value
}

# docs/wireless/01_BT_AND_RADIO.md: `hciconfig hci0` alone only proves the net-device OBJECT exists -- fw_loader_linux can
# report "Download Error" on the firmware push while hciattach/hciconfig still bring the interface
# up, leaving a chip that never answers any HCI command (every subsequent read/write times out at
# the kernel HCI layer, ~2-3s each). That silent-failure mode let a broken chip pass this loop's old
# health check and run for the rest of boot unresponsive. Verify real responsiveness by reading the
# local name back (bounded by `timeout` as a belt-and-suspenders on top of the kernel's own per-command
# HCI timeout, in case that timeout is ever absent/longer than expected).
bt_responsive() {
	hciconfig hci0 >/dev/null 2>&1 || return 1
	timeout 5 hciconfig hci0 name 2>/dev/null | grep -q "Name:"
}

cp /usr/sbin/bluetoothDaemon /tmp/bin/ 2>/dev/null
bluetoothDaemon -n &

# Brief grace for a CONCURRENT wlan_on.sh extraction, so we do not race it to the same tarball.
# No longer load-bearing: attach_bt -> ensure_hci_ldisc extracts the module itself if this expires.
i=0
while ! ls /tmp/*hci_uart.ko >/dev/null 2>&1 && [ "$i" -lt 120 ]; do i=$((i+1)); sleep 0.1; done
[ -e /tmp/hci_uart.ko ] || echo "[attach_bluetooth] no hci_uart.ko after 12s wait -- extracting it ourselves"

attach_bt
bt_responsive; rc=$?
t=0
while [ "$rc" -ne 0 ] && [ "$t" -lt 20 ]; do
	t=$((t+1)); reset_bt; attach_bt; bt_responsive; rc=$?
done
[ "$rc" -eq 0 ] && echo "[attach_bluetooth] hci0 up: $(cat /sys/class/bluetooth/hci0/address 2>/dev/null)" \
                || echo "[attach_bluetooth] FAILED after $t retries (chip unresponsive)"
touch /tmp/.hciattach_done
