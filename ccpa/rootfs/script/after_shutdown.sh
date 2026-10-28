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

# 2026-10-19: dropped the stock saveLastLog() and its only caller, `test -e /tmp/update_status &&
# saveLastLog /var/log/box_last_reboot.log` (a reboot-time log dump to flash during an OTA). Its
# writers (update_box.sh and the stock update scripts) are removed by ncm_base_install.sh strip,
# so the test could never be true.

sync
echo "[after_shutdown] unmounting filesystems"
df -h |grep /data && umount /data
/bin/umount -a -r
