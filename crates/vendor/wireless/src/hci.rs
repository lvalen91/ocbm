//! hci.rs — native replacements for the `hciconfig` invocations in `bt_bringup`.
//!
//! # Why this exists
//!
//! `bt_bringup` shells out to `hciconfig` for seven operations. That binary is part of BlueZ's
//! userspace and **does not exist on Android**, so on the Raspberry Pi port every bring-up failed
//! with `No such file or directory (os error 2)` before the controller was ever touched.
//!
//! Everything `hciconfig` does here is either an ioctl on an HCI socket or a single HCI command
//! packet, so doing it natively removes the last external dependency and makes the daemon
//! self-sufficient on any host with a Linux Bluetooth stack.
//!
//! # Why not the mgmt API
//!
//! `ssp_agent` already speaks mgmt (`HCI_CHANNEL_CONTROL`), and mgmt has tidy commands for powered
//! / connectable / discoverable / name / class. It is deliberately **not** used here for one
//! reason: **mgmt owns EIR**. It synthesises the Extended Inquiry Response itself from the name,
//! class and the UUIDs of registered services, and offers no way to set arbitrary AD structures.
//! The CarPlay accessory identity needs a specific two-UUID EIR — the iAP2 UUID at AD type 0x06
//! (incomplete) and the CarPlay marker UUID at 0x07 (complete) — which only raw
//! `Write_Extended_Inquiry_Response` can express. `hciconfig inqdata` is raw HCI for exactly this
//! reason, and so is this module.
//!
//! # Backend selection
//!
//! Chosen by `CARPLAY_HCI_BACKEND`:
//!   * `hciconfig` (**default**) — the original shell-out. Keeps the proven CCPA path byte-for-byte.
//!   * `native` — this module. What the Raspberry Pi port uses.
//!
//! Defaulting to the old behaviour is deliberate: the CCPA path is device-proven and this module has
//! not been exercised there, so opting in per-host is the honest default.

use std::io;

const AF_BLUETOOTH: libc::sa_family_t = 31;
const BTPROTO_HCI: libc::c_int = 1;
/// Raw channel — lets us write HCI command packets directly. (mgmt is channel 3; see module docs.)
const HCI_CHANNEL_RAW: u16 = 0;

/// `_IOW('H', 201, int)` / `_IOW('H', 202, int)` from `include/net/bluetooth/hci_sock.h`.
///
/// Held as `u32` and cast at the call site with `as _`: bionic declares `ioctl`'s request parameter
/// as `c_int` while glibc/musl use `c_ulong`, so a concrete type here fails to compile on one libc
/// or the other. This crate builds for both (Android/aarch64 and musl/armv7).
const HCIDEVUP: u32 = 0x400448C9;
const HCIDEVDOWN: u32 = 0x400448CA;

// HCI opcodes, OGF 0x03 (Host Controller & Baseband). opcode = (ogf << 10) | ocf.
const OP_WRITE_LOCAL_NAME: u16 = 0x0C13;
const OP_WRITE_SCAN_ENABLE: u16 = 0x0C1A;
const OP_WRITE_CLASS_OF_DEV: u16 = 0x0C24;
const OP_WRITE_EIR: u16 = 0x0C52;
const OP_WRITE_SSP_MODE: u16 = 0x0C56;

/// `HCISETSCAN`, `_IOW('H', 221, int)`. Unlike the other operations this takes a POINTER to a
/// `hci_dev_req`, matching BlueZ `tools/hciconfig.c`'s `cmd_scan`.
const HCISETSCAN: u32 = 0x400448DD;

/// Scan modes for [`set_scan`]: inquiry scan = discoverable, page scan = connectable.
pub const SCAN_PAGE_AND_INQUIRY: u32 = 0x03;
pub const SCAN_NONE: u32 = 0x00;

/// `struct hci_dev_req` — `{ __u16 dev_id; __u32 dev_opt; }`.
#[repr(C)]
struct HciDevReq {
    dev_id: u16,
    dev_opt: u32,
}

const HCI_COMMAND_PKT: u8 = 0x01;

#[repr(C)]
struct SockaddrHci {
    hci_family: libc::sa_family_t,
    hci_dev: u16,
    hci_channel: u16,
}

/// Is the native backend selected? Resolved once per process, matching the crate's env-lever
/// convention.
pub fn native_selected() -> bool {
    static SEL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SEL.get_or_init(|| {
        let native = std::env::var("CARPLAY_HCI_BACKEND")
            .map(|v| v.trim().eq_ignore_ascii_case("native"))
            .unwrap_or(false);
        if native {
            eprintln!("[hci] native backend selected — not using hciconfig");
        }
        native
    })
}

/// Parse `hci0` -> 0. Returns `InvalidInput` for anything that is not `hci<N>`.
pub fn dev_id(hci_dev: &str) -> io::Result<u16> {
    hci_dev
        .strip_prefix("hci")
        .and_then(|n| n.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("bad hci device {hci_dev}")))
}

/// An HCI socket. `Drop` closes it — every call site is short-lived, so nothing outlives a command.
struct HciSocket(libc::c_int);

impl Drop for HciSocket {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

impl HciSocket {
    /// Unbound socket, used for the HCIDEVUP/HCIDEVDOWN ioctls (which take the device id as the
    /// ioctl argument rather than via bind).
    fn open() -> io::Result<Self> {
        // SOCK_CLOEXEC: this must not leak into av.rs's fork+exec of the detached daemons.
        let fd = unsafe {
            libc::socket(
                AF_BLUETOOTH as libc::c_int,
                libc::SOCK_RAW | crate::cloexec::SOCK_CLOEXEC,
                BTPROTO_HCI,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(HciSocket(fd))
    }

    /// Socket bound to one controller on the RAW channel, ready to accept command packets.
    fn open_bound(dev: u16) -> io::Result<Self> {
        let s = Self::open()?;
        let addr = SockaddrHci {
            hci_family: AF_BLUETOOTH,
            hci_dev: dev,
            hci_channel: HCI_CHANNEL_RAW,
        };
        let rc = unsafe {
            libc::bind(
                s.0,
                &addr as *const SockaddrHci as *const libc::sockaddr,
                std::mem::size_of::<SockaddrHci>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(s)
    }

    /// Write one HCI command packet: `[0x01][opcode LE][plen][params]`.
    ///
    /// Fire-and-forget by design. The command-complete event comes back on the same socket, but
    /// reading it would mean filtering the shared RAW stream against everything else the kernel is
    /// doing on this controller. `hciconfig` is equally fire-and-forget for these writes, and the
    /// bring-up is verified by its observable effect (the phone sees the accessory) rather than by
    /// per-command status. A failed *write* is still reported.
    fn cmd(&self, opcode: u16, params: &[u8]) -> io::Result<()> {
        if params.len() > u8::MAX as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "HCI parameter block exceeds 255 bytes",
            ));
        }
        let mut pkt = Vec::with_capacity(4 + params.len());
        pkt.push(HCI_COMMAND_PKT);
        pkt.extend_from_slice(&opcode.to_le_bytes());
        pkt.push(params.len() as u8);
        pkt.extend_from_slice(params);

        let n = unsafe { libc::write(self.0, pkt.as_ptr() as *const libc::c_void, pkt.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if (n as usize) < pkt.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                format!("short HCI write: {n} of {} bytes", pkt.len()),
            ));
        }
        Ok(())
    }
}

fn ioctl_dev(request: u32, dev: u16) -> io::Result<()> {
    let s = HciSocket::open()?;
    let rc = unsafe { libc::ioctl(s.0, request as _, dev as libc::c_int) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `hciconfig hciN up`. See `bt_bringup::bring_up` for why the DOWN→UP cycle is load-bearing: the
/// kernel only sends its Set_Event_Mask (which enables the SSP events) from `hci_dev_do_open`.
pub fn dev_up(dev: u16) -> io::Result<()> {
    ioctl_dev(HCIDEVUP, dev)
}

/// `hciconfig hciN down`. Best-effort at every call site. An already-down controller returns 0
/// (`hci_dev_close_sync` propagates 0 when the device is not up) — it is HCIDEVUP that returns
/// `EALREADY` on an already-up device.
pub fn dev_down(dev: u16) -> io::Result<()> {
    ioctl_dev(HCIDEVDOWN, dev)
}

/// `hciconfig hciN sspmode 1` — HCI Write_Simple_Pairing_Mode.
pub fn write_ssp_mode(dev: u16, enable: bool) -> io::Result<()> {
    HciSocket::open_bound(dev)?.cmd(OP_WRITE_SSP_MODE, &[u8::from(enable)])
}

/// `hciconfig hciN class 0xNNNNNN` — HCI Write_Class_of_Device (3 bytes, little-endian).
pub fn write_class_of_device(dev: u16, class: u32) -> io::Result<()> {
    let b = class.to_le_bytes();
    HciSocket::open_bound(dev)?.cmd(OP_WRITE_CLASS_OF_DEV, &b[..3])
}

/// `hciconfig hciN name <name>` — HCI Write_Local_Name. The parameter is a fixed 248-byte field,
/// NUL-padded; over-long names are truncated to fit with room for the terminator.
pub fn write_local_name(dev: u16, name: &str) -> io::Result<()> {
    let mut buf = [0u8; 248];
    let src = name.as_bytes();
    let n = src.len().min(buf.len() - 1);
    buf[..n].copy_from_slice(&src[..n]);
    HciSocket::open_bound(dev)?.cmd(OP_WRITE_LOCAL_NAME, &buf)
}

/// `hciconfig hciN inqdata <hex>` — HCI Write_Extended_Inquiry_Response.
///
/// Parameter is `[fec_required u8][data 240]`. This is the one operation with no mgmt equivalent,
/// and the reason this module talks raw HCI at all: it is what places the iAP2 UUID (AD type 0x06)
/// and the CarPlay marker UUID (AD type 0x07) in front of the iPhone.
pub fn write_eir(dev: u16, eir: &[u8]) -> io::Result<()> {
    if eir.len() > 240 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("EIR is {} bytes, max 240", eir.len()),
        ));
    }
    let mut params = [0u8; 241];
    params[0] = 0; // FEC not required
    params[1..1 + eir.len()].copy_from_slice(eir);
    HciSocket::open_bound(dev)?.cmd(OP_WRITE_EIR, &params)
}

/// `hciconfig hciN piscan` / `noscan`.
///
/// This uses the **HCISETSCAN ioctl**, NOT a raw Write_Scan_Enable command, and the difference is
/// load-bearing on modern kernels.
///
/// `hciconfig` never sent the raw command either: `cmd_scan` issues this ioctl, and the kernel's
/// handler additionally calls `hci_update_passive_scan_state()` to set the mgmt-level
/// `HCI_CONNECTABLE` / `HCI_DISCOVERABLE` dev_flags ("ensure that the connectable and discoverable
/// states get correctly modified as this was a non-mgmt change").
///
/// A raw Write_Scan_Enable only flips `HCI_PSCAN`/`HCI_ISCAN` and leaves those mgmt flags CLEAR.
/// Linux 6.12 calls `hci_update_scan()` on every ACL connect **and** every disconnect; with the
/// mgmt flags clear it recomputes the desired state as `SCAN_DISABLED` and writes
/// Write_Scan_Enable(0x00) behind our back. The accessory would go silently invisible the moment
/// the phone first connected, and every later reconnect would fail until the next `bring_up`.
pub fn set_scan(dev: u16, scan: u32) -> io::Result<()> {
    let s = HciSocket::open()?;
    let req = HciDevReq {
        dev_id: dev,
        dev_opt: scan,
    };
    let rc = unsafe {
        libc::ioctl(
            s.0,
            HCISETSCAN as _,
            &req as *const HciDevReq as *const libc::c_void,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_id_parses_hci_names() {
        assert_eq!(dev_id("hci0").unwrap(), 0);
        assert_eq!(dev_id("hci1").unwrap(), 1);
        assert_eq!(dev_id("hci12").unwrap(), 12);
    }

    #[test]
    fn dev_id_rejects_garbage() {
        for bad in ["", "hci", "wlan0", "hciX", "0"] {
            assert!(dev_id(bad).is_err(), "{bad} should not parse");
        }
    }

    /// The ioctl numbers are hand-computed from `_IOW('H', nr, int)`; if any is wrong the
    /// controller silently never comes up (or never becomes discoverable), so pin them.
    #[test]
    fn ioctl_numbers_match_iow_h() {
        let iow = |nr: u32| 0x4000_0000u32 | (4 << 16) | (u32::from(b'H') << 8) | nr;
        assert_eq!(HCIDEVUP, iow(201));
        assert_eq!(HCIDEVDOWN, iow(202));
        assert_eq!(HCISETSCAN, iow(221));
    }

    /// opcode = (ogf << 10) | ocf, all OGF 0x03.
    #[test]
    fn opcodes_are_ogf3() {
        let op = |ocf: u16| (0x03u16 << 10) | ocf;
        assert_eq!(OP_WRITE_LOCAL_NAME, op(0x0013));
        assert_eq!(OP_WRITE_SCAN_ENABLE, op(0x001A));
        assert_eq!(OP_WRITE_CLASS_OF_DEV, op(0x0024));
        assert_eq!(OP_WRITE_EIR, op(0x0052));
        assert_eq!(OP_WRITE_SSP_MODE, op(0x0056));
    }
}
