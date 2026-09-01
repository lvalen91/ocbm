#!/bin/sh
# ccpa_custom: owned -- audited & adopted from stock; part of the open CCPA userspace.
########################
# Copyright(c) 2014-2020 DongGuan HeWei Communication Technologies Co. Ltd.
# file    after_shutdown.sh
# brief
# author  Shi Kai
# version 1.0.0
# date    27Feb20
########################

saveLastLog() {
	lastLogPath=$1
	echo "[after_shutdown] saving last log to $lastLogPath"
	#if [ -e $lastLogPath ]; then
	#	return
	#fi
	echo "Save last log when reboot" > "$lastLogPath"
	df -h >> "$lastLogPath"
	cat /proc/meminfo >> "$lastLogPath"
	ps -l >> "$lastLogPath"
	echo y > /sys/module/printk/parameters/time
	dmesg | tail -n 1000 >> "$lastLogPath"
	tail -n 1000 /tmp/userspace.log >> "$lastLogPath"
	sync
}

#change USB to device mode

test -e /tmp/update_status && saveLastLog /var/log/box_last_reboot.log

sync
echo "[after_shutdown] unmounting filesystems"
df -h |grep /data && umount /data
/bin/umount -a -r
