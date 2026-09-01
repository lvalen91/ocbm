#!/bin/sh
# install_fhs.sh — install the validated OCBM stack into FHS locations on the box's jffs2 (rw) rootfs.
#   daemons  -> /usr/sbin  (ocbmd, iap2d, airplayd, rx-connect)
#   tools    -> /usr/bin   (iap_role_switch)
#   scripts  -> /script    (session_supervisor.sh, cold_start_now.sh; ocbm_boot.sh already there)
# and repoints the boot hook ocbm_boot.sh at /usr/sbin/ocbmd (it currently launches /script/ocbmd —
# a binary in the scripts dir, the thing being fixed). Run on the box after pushing the binaries +
# updated scripts to /tmp. Idempotent; sync's to flash. UART early-console stays the recovery path.
set -u
echo "[install] daemons -> /usr/sbin"
for b in ocbmd iap2d airplayd rx-connect; do
  if [ -f "/tmp/$b" ]; then
    # rm-then-cp: overwriting a RUNNING binary in place fails with ETXTBSY; unlink+create dodges it
    # (the running process keeps the old unlinked inode until it exits).
    rm -f "/usr/sbin/$b"
    if cp "/tmp/$b" "/usr/sbin/$b" && chmod 755 "/usr/sbin/$b"; then echo "  /usr/sbin/$b"; else echo "  FAILED /usr/sbin/$b"; fi
  else echo "  MISSING /tmp/$b"; fi
done
echo "[install] tools -> /usr/bin"
if [ -f /tmp/iap_role_switch ]; then cp /tmp/iap_role_switch /usr/bin/iap_role_switch && chmod 755 /usr/bin/iap_role_switch && echo "  /usr/bin/iap_role_switch"; else echo "  MISSING /tmp/iap_role_switch"; fi
echo "[install] scripts -> /script"
# ocbm_boot.sh ships already pointing at /usr/sbin/ocbmd + launching the supervisor, so we install it
# fresh (chmod +x) rather than sed-patching a live boot hook (a sed that drops +x half-bricks the boot).
for s in ocbm_boot.sh session_supervisor.sh projection_up.sh phone_reset.sh peer_store.sh carplay-status.sh run_ocbmd.sh run_supervisor.sh; do
  if [ -f "/tmp/$s" ]; then cp "/tmp/$s" "/script/$s" && chmod 755 "/script/$s" && echo "  /script/$s"; else echo "  MISSING /tmp/$s"; fi
done
echo "[install] remove any misplaced/retired files"
rm -f /script/ocbmd /script/cold_start_now.sh   # binary belongs in /usr/sbin; cold_start_now retired
echo "[install] sync to flash"
sync
echo "[install] result:"
ls -l /usr/sbin/ocbmd /usr/sbin/iap2d /usr/sbin/airplayd /usr/sbin/rx-connect /usr/bin/iap_role_switch 2>&1 | sed 's/^/  /'
