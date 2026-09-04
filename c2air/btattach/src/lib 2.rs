//! c2air-btattach — Bluetooth HCI line-discipline attach for the C2Air (Allwinner V821, riscv32).
//!
//! ## Why this exists at all, and why ONLY for the C2Air
//!
//! This is the one piece of genuinely board-specific Rust in the C2Air port. Everything else — ocbmd,
//! iap2d, airplayd — builds from the shared CCPA sources with no C2Air-specific code (see
//! `c2air/README.md`).
//!
//! The C2Air ships **no Bluetooth userspace whatsoever**. The vendor's `btapp` lived in the
//! `customer` partition that the OCBM baseline absorbed into rootfs, and it needs
//! `liblylinkmw.so.1`, so it cannot run standalone. The KERNEL side is complete
//! (`Bluetooth: HCI UART driver ver 2.3`, H4, L2CAP/SCO, `krfcommd`) and the AIC8800 firmware patch
//! is pulled **in-kernel** by `aicbt_patch_table_load`, so `/lib/firmware` is empty and unnecessary.
//!
//! The only missing step is the line-discipline attach. On the CCPA that is done by the unit's own
//! vendor helper (`rtk_hciattach`, `hci_attach`, …) dispatched by `init_bluetooth_wifi.sh`, which per
//! the project's radio doctrine (docs/wireless/01_BT_AND_RADIO.md) must never be replaced with a chipset-specific
//! reimplementation. **The C2Air has no such helper to dispatch**, which is what makes writing one
//! here legitimate rather than a doctrine violation: this installs a bring-up path where the vendor
//! provides none, and it is confined to a board-specific crate that no CCPA build links.
//!
//! ## Why a library and not just a binary
//!
//! **The attach dies with the fd.** `HCIUARTSETPROTO` binds the tty to the HCI stack for exactly as
//! long as the file descriptor lives; close it and `hci0` disappears. So whoever owns Bluetooth has
//! to hold it, and on this box that is OCBM. Daemon ownership is therefore the natural design, not a
//! workaround — hence [`Attach`], which keeps the fd and detaches on `Drop`. The binary is a thin
//! wrapper that parks forever, for bring-up and for proving the attach standalone.
//!
//! ## Platform facts come from the environment, never from constants here
//!
//! `/etc/profile` on the OCBM baseline exports the board's own values — `LY_BT_UART=/dev/ttyS1`,
//! `LY_BT_BDRATE` (500000), `LY_MFI_I2C_BUS=1`. Reading them keeps this file from hardcoding a
//! chipset/board pairing, which is the same reason `radio_detect.sh` resolves the CCPA's bring-up at
//! runtime instead of branching on a whitelist. The defaults below are only a fallback for a shell
//! that did not source the profile.

use std::io;
use std::os::unix::io::{AsRawFd, RawFd};

/// Line discipline number for the HCI UART driver (`N_HCI` in `<linux/tty.h>`); not in the libc crate.
const N_HCI: libc::c_int = 15;
/// `HCI_UART_H4` — the C2Air's AIC8800 speaks H4. Not H5/3-wire like the CCPA's Realtek units,
/// which is why the CCPA's `rtk_hciattach -s 115200 ttymxc2 rtk_h5` is not a template for this.
const HCI_UART_H4: libc::c_ulong = 0;

const AF_BLUETOOTH: libc::c_int = 31;
const BTPROTO_HCI: libc::c_int = 1;

/// `_IOW` from asm-generic/ioctl.h. Spelled out rather than hardcoded so the derivation is checkable:
/// dir(2b)<<30 | size(14b)<<16 | type(8b)<<8 | nr(8b). riscv32 uses the generic layout, same as ARM.
const fn iow(ty: u32, nr: u32, size: u32) -> libc::c_ulong {
    ((1u32 << 30) | (size << 16) | (ty << 8) | nr) as libc::c_ulong
}
/// `_IOW('U', 200, int)`. The kernel's `hci_uart_tty_ioctl` uses `arg` as the protocol VALUE,
/// not as a pointer to one — passing `&proto` here silently fails.
const HCIUARTSETPROTO: libc::c_ulong = iow(b'U' as u32, 200, 4);
/// `_IOW('H', 201, int)`. Same convention: `arg` is the device index, by value.
const HCIDEVUP: libc::c_ulong = iow(b'H' as u32, 201, 4);
/// `_IOR('U', 202, int)` = `0x800455CA` under the asm-generic layout used above. Spelled out as a
/// literal (rather than via `ior`, which this file doesn't otherwise need) because the kernel's
/// `hci_uart_tty_ioctl` returns the device index as the ioctl's RETURN VALUE (`err = hu->hdev->id`),
/// never writes it through the `_IOR` arg pointer — so this is read with `ioctl(fd, ..., 0)`, not a
/// pointer, despite what `_IOR` suggests.
const HCIUARTGETDEVICE: libc::c_ulong = 0x800455CA;

/// Where the board's BT UART and its baud come from. See the module docs on why these are read from
/// the environment rather than compiled in.
pub fn board_uart() -> String {
    std::env::var("LY_BT_UART").unwrap_or_else(|_| "/dev/ttyS1".into())
}
pub fn board_baud() -> u32 {
    std::env::var("LY_BT_BDRATE")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(500_000)
}

/// Map a numeric baud to its `B*` constant. Only the rates this UART plausibly runs are listed; an
/// unknown value is an error rather than a silent fallback, because attaching at the wrong speed
/// produces a controller that enumerates and then fails every command — the hardest kind of bug to
/// read back from `hci0 UP`.
fn baud_const(baud: u32) -> io::Result<libc::speed_t> {
    Ok(match baud {
        115_200 => libc::B115200,
        230_400 => libc::B230400,
        460_800 => libc::B460800,
        500_000 => libc::B500000,
        576_000 => libc::B576000,
        921_600 => libc::B921600,
        1_000_000 => libc::B1000000,
        1_500_000 => libc::B1500000,
        3_000_000 => libc::B3000000,
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported BT UART baud {other} (add its B* constant if the board really uses it)"),
            ))
        }
    })
}

/// Wrap the CURRENT errno with context. Must be called before anything else can clobber errno —
/// in particular before an `Attach` is dropped, since its `close()` would overwrite it.
fn last_err<T>(what: &str) -> io::Result<T> {
    let e = io::Error::last_os_error();
    Err(io::Error::new(e.kind(), format!("{what}: {e}")))
}

/// A live HCI attach. Dropping this detaches the controller, so hold it for as long as you want
/// `hci0` to exist.
pub struct Attach {
    fd: RawFd,
    pub dev: String,
    pub baud: u32,
}

impl Attach {
    /// Attach `dev` at `baud` and bring `hci0` up.
    ///
    /// Sequence, in the only order that works: raw termios + CRTSCTS (the AIC8800 uses hardware
    /// flow control at 500 kbaud) → `TIOCSETD` N_HCI → `HCIUARTSETPROTO` H4 → `HCIDEVUP`. The
    /// ldisc must be set before the protocol, and the device cannot come up before both.
    pub fn open(dev: &str, baud: u32) -> io::Result<Self> {
        let speed = baud_const(baud)?;
        let cdev = std::ffi::CString::new(dev).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        // O_NOCTTY: this must not become our controlling terminal, or a hangup on it would signal us.
        let fd = unsafe { libc::open(cdev.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
        if fd < 0 {
            return last_err(&format!("open {dev}"));
        }
        let me = Attach { fd, dev: dev.to_string(), baud };

        let mut tio: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut tio) } < 0 {
            return last_err("tcgetattr");
        }
        unsafe { libc::cfmakeraw(&mut tio) };
        tio.c_cflag |= libc::CRTSCTS | libc::CLOCAL | libc::CREAD;
        if unsafe { libc::cfsetispeed(&mut tio, speed) } < 0 || unsafe { libc::cfsetospeed(&mut tio, speed) } < 0 {
            return last_err("cfsetspeed");
        }
        // TCSANOW, then flush: any bytes the bootloader or a previous attach left in the FIFO would
        // otherwise be parsed as the first HCI packet.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &tio) } < 0 {
            return last_err("tcsetattr");
        }
        unsafe { libc::tcflush(fd, libc::TCIOFLUSH) };

        if unsafe { libc::ioctl(fd, libc::TIOCSETD as _, &N_HCI as *const libc::c_int) } < 0 {
            return last_err("ioctl TIOCSETD N_HCI (is CONFIG_BT_HCIUART built in?)");
        }
        if unsafe { libc::ioctl(fd, HCIUARTSETPROTO as _, HCI_UART_H4) } < 0 {
            return last_err("ioctl HCIUARTSETPROTO H4");
        }
        // Registration is synchronous inside HCIUARTSETPROTO (HCI_UART_INIT_PENDING is never set on
        // this path), so the kernel-assigned index is readable immediately on the same fd. It is NOT
        // necessarily 0 — that was wrong on any box where another controller registers first.
        let idx = unsafe { libc::ioctl(fd, HCIUARTGETDEVICE as _, 0 as libc::c_ulong) };
        if idx < 0 {
            return last_err("ioctl HCIUARTGETDEVICE");
        }

        // HCIDEVUP needs an AF_BLUETOOTH socket, not the tty fd.
        let sock = unsafe { libc::socket(AF_BLUETOOTH, libc::SOCK_RAW | libc::SOCK_CLOEXEC, BTPROTO_HCI) };
        if sock < 0 {
            return last_err("socket(AF_BLUETOOTH, SOCK_RAW, BTPROTO_HCI)");
        }
        let up = unsafe { libc::ioctl(sock, HCIDEVUP as _, idx as libc::c_ulong) };
        let errno = io::Error::last_os_error();
        unsafe { libc::close(sock) };
        // EALREADY simply means someone already brought hci0 up; that is success for our purposes.
        if up < 0 && errno.raw_os_error() != Some(libc::EALREADY) {
            return Err(io::Error::new(errno.kind(), format!("ioctl HCIDEVUP: {errno}")));
        }
        Ok(me)
    }

    /// Attach using the board's own values from the environment.
    pub fn open_board() -> io::Result<Self> {
        Self::open(&board_uart(), board_baud())
    }
}

impl AsRawFd for Attach {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Drop for Attach {
    fn drop(&mut self) {
        // Closing the fd IS the detach. Nothing else to undo.
        unsafe { libc::close(self.fd) };
    }
}
