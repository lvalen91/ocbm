/* iap_role_switch.c — wired-CarPlay USB host-role switch helper (implemented from documentation).
 *
 * Sends the Apple vendor control transfer that role-switches an iPhone (05ac:12a8)
 * into USB *host* mode, the documented wired-CarPlay trigger:
 *     bmRequestType=0x40 (host->device, vendor, device)
 *     bRequest=0x51, wValue=1, wIndex=0, wLength=0
 * (0x52 selects extended-function/usbmux/NCM device configs — NOT CarPlay.)
 *
 * Uses raw usbfs (USBDEVFS_CONTROL) so it needs no libusb — runs on the stripped
 * CCPA appliance. Build: zig cc -target arm-linux-musleabihf -static -Os -s.
 * Usage: iap_role_switch [/dev/bus/usb/BBB/DDD]   (default /dev/bus/usb/001/003)
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <linux/usbdevice_fs.h>

int main(int argc, char **argv)
{
    const char *dev = (argc > 1) ? argv[1] : "/dev/bus/usb/001/003";
    int fd = open(dev, O_RDWR);
    if (fd < 0) { fprintf(stderr, "open(%s): %s\n", dev, strerror(errno)); return 1; }

    struct usbdevfs_ctrltransfer ct;
    memset(&ct, 0, sizeof ct);
    ct.bRequestType = 0x40;   /* host->device | vendor | device */
    ct.bRequest     = 0x51;   /* VENDER_REQ_DEV_TO_HOST (role switch -> CarPlay) */
    ct.wValue       = 1;
    ct.wIndex       = 0;
    ct.wLength      = 0;
    ct.timeout      = 1000;   /* ms */
    ct.data         = NULL;

    int r = ioctl(fd, USBDEVFS_CONTROL, &ct);
    printf("iap_role_switch: %s  0x51 wValue=1 -> ret=%d", dev, r);
    if (r < 0) printf("  errno=%d (%s)", errno, strerror(errno));
    printf("\n%s\n", r >= 0 ? "SENT OK — watch for iPhone re-enumerate as host"
                            : "FAILED");
    close(fd);
    return r < 0 ? 2 : 0;
}
