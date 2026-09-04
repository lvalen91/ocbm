//! usbdevfs USB-host primitives — control/bulk transfers, interface claim, descriptor parsing, and
//! bus enumeration over `/dev/bus/usb/BBB/DDD`. Raw ioctls via libc; no libusb on the box.
//!
//! This is the overlap the Android Auto set uses today (aa-bridge's AOAP switch + bulk pump) and
//! that any future USB-host work (e.g. a Rust arbiter that probes a freshly-plugged device) reuses.
//! Errno is surfaced via `std::io::Error::last_os_error()` so the code is portable within the crate
//! and needs no `__errno_location`.

use std::ffi::c_void;
use std::fs::File;
use std::io::Read;
use std::os::unix::io::RawFd;

pub const APPLE_VID: u16 = 0x05ac;

// usbdevfs ioctls (asm-generic encoding, matches 32-bit ARM). musl's ioctl() takes the request as
// `int`, so these are c_int (the 0xC010_5500-class values are just bit patterns).
const USBDEVFS_CONTROL: libc::c_int = 0xC010_5500u32 as libc::c_int; // _IOWR('U',0,16)
const USBDEVFS_BULK: libc::c_int = 0xC010_5502u32 as libc::c_int; // _IOWR('U',2,16)
const USBDEVFS_CLAIMINTERFACE: libc::c_int = 0x8004_550Fu32 as libc::c_int; // _IOR('U',15,4)
const USBDEVFS_RELEASEINTERFACE: libc::c_int = 0x8004_5510u32 as libc::c_int; // _IOR('U',16,4)
const USBDEVFS_RESET: libc::c_int = 0x0000_5514u32 as libc::c_int; // _IO('U',20)

#[repr(C)]
struct CtrlTransfer {
    b_request_type: u8,
    b_request: u8,
    w_value: u16,
    w_index: u16,
    w_length: u16,
    timeout: u32, // ms
    data: *mut c_void,
}

#[repr(C)]
struct BulkTransfer {
    ep: u32,
    len: u32,
    timeout: u32, // ms; 0 == no timeout
    data: *mut c_void,
}

/// One control transfer on ep0. Returns bytes transferred.
pub fn control(
    fd: RawFd,
    b_request_type: u8,
    b_request: u8,
    w_value: u16,
    w_index: u16,
    data: &mut [u8],
    timeout: u32,
) -> Result<usize, String> {
    let mut ct = CtrlTransfer {
        b_request_type,
        b_request,
        w_value,
        w_index,
        w_length: data.len() as u16,
        timeout,
        data: if data.is_empty() {
            std::ptr::null_mut()
        } else {
            data.as_mut_ptr() as *mut c_void
        },
    };
    let r = unsafe { libc::ioctl(fd, USBDEVFS_CONTROL, &mut ct as *mut CtrlTransfer) };
    if r < 0 {
        return Err(format!("control(req=0x{b_request:02x}) errno={}", errno()));
    }
    Ok(r as usize)
}

/// One bulk transfer. Returns bytes transferred, or the raw errno (so callers can tell transient
/// EAGAIN/EINTR from a real teardown).
pub fn bulk(fd: RawFd, ep: u32, data: &mut [u8], timeout: u32) -> Result<usize, i32> {
    let mut bt = BulkTransfer {
        ep,
        len: data.len() as u32,
        timeout,
        data: data.as_mut_ptr() as *mut c_void,
    };
    let r = unsafe { libc::ioctl(fd, USBDEVFS_BULK, &mut bt as *mut BulkTransfer) };
    if r < 0 {
        return Err(errno());
    }
    Ok(r as usize)
}

/// Write every byte to a bulk OUT endpoint, chunk by chunk.
pub fn bulk_write_all(fd: RawFd, ep: u32, mut data: &[u8]) -> Result<(), i32> {
    while !data.is_empty() {
        let mut tmp = data.to_vec();
        let n = bulk(fd, ep, &mut tmp, 2000)?;
        if n == 0 {
            return Err(libc::EIO);
        }
        data = &data[n..];
    }
    Ok(())
}

pub fn claim_interface(fd: RawFd, iface: u32) -> Result<(), String> {
    let r = unsafe { libc::ioctl(fd, USBDEVFS_CLAIMINTERFACE, &iface as *const u32) };
    if r < 0 {
        return Err(format!("claim interface {iface}: errno={}", errno()));
    }
    Ok(())
}

pub fn release_interface(fd: RawFd, iface: u32) {
    unsafe {
        libc::ioctl(fd, USBDEVFS_RELEASEINTERFACE, &iface as *const u32);
    }
}

/// Reset the device — also unblocks a thread parked in a no-timeout bulk-IN.
pub fn reset(fd: RawFd) {
    unsafe {
        libc::ioctl(fd, USBDEVFS_RESET);
    }
}

/// A device found on a usbfs bus directory.
#[derive(Clone, Debug)]
pub struct BusDevice {
    pub path: String,
    pub vid: u16,
    pub pid: u16,
    /// bDeviceClass from the descriptor. 0x09 is a HUB (mandatory per USB 2.0 §11.23.1) and never a
    /// phone — Apple, Android normal mode and AOAP accessories are all per-interface (0x00). The
    /// descriptor is already read here, so this costs nothing and is what lets the arbiter stop
    /// treating a bare hub as an Android Auto candidate.
    pub class: u8,
}

/// Enumerate `/dev/bus/usb/BBB`, reading each node's 18-byte device descriptor. Skips the root hub
/// node ("001") and anything that isn't a device descriptor.
pub fn enumerate_bus(bus_dir: &str) -> Vec<BusDevice> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(bus_dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for ent in entries.flatten() {
        let name = ent.file_name();
        if name.to_string_lossy() == "001" {
            continue; // root hub
        }
        let mut f = match File::open(ent.path()) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let mut dd = [0u8; 18];
        if f.read_exact(&mut dd).is_err() || dd[1] != 0x01 {
            continue;
        }
        out.push(BusDevice {
            path: ent.path().to_string_lossy().into_owned(),
            vid: u16::from_le_bytes([dd[8], dd[9]]),
            pid: u16::from_le_bytes([dd[10], dd[11]]),
            class: dd[4],
        });
    }
    out
}

/// Walk a device+config descriptor blob and return (interface number, bulk IN addr, bulk OUT addr)
/// for the first interface that has both. Bulk = bmAttributes & 0x03 == 2; IN = addr & 0x80.
pub fn parse_bulk_endpoints(d: &[u8]) -> Option<(u8, u8, u8)> {
    let mut i = 0usize;
    let mut cur_iface: Option<u8> = None;
    let mut ep_in: Option<u8> = None;
    let mut ep_out: Option<u8> = None;
    while i + 2 <= d.len() {
        let len = d[i] as usize;
        let dtype = d[i + 1];
        if len == 0 || i + len > d.len() {
            break;
        }
        // The `bLength` guards are load-bearing: the blob comes from the attached device, and a
        // descriptor declaring a length shorter than the field we read would index past `d`.
        match dtype {
            0x04 if len >= 3 => {
                if let (Some(ifn), Some(a), Some(b)) = (cur_iface, ep_in, ep_out) {
                    return Some((ifn, a, b));
                }
                cur_iface = Some(d[i + 2]);
                ep_in = None;
                ep_out = None;
            }
            0x05 if len >= 4 => {
                let addr = d[i + 2];
                let attrs = d[i + 3];
                if attrs & 0x03 == 0x02 {
                    if addr & 0x80 != 0 {
                        ep_in.get_or_insert(addr);
                    } else {
                        ep_out.get_or_insert(addr);
                    }
                }
            }
            _ => {}
        }
        i += len;
    }
    if let (Some(ifn), Some(a), Some(b)) = (cur_iface, ep_in, ep_out) {
        return Some((ifn, a, b));
    }
    None
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::parse_bulk_endpoints;

    #[test]
    fn finds_the_first_interface_with_both_bulk_endpoints() {
        let d = [
            9, 0x04, 0x01, 0, 2, 0xff, 0, 0, 0, // interface 1
            7, 0x05, 0x81, 0x02, 0, 2, 0, // bulk IN
            7, 0x05, 0x02, 0x02, 0, 2, 0, // bulk OUT
        ];
        assert_eq!(parse_bulk_endpoints(&d), Some((1, 0x81, 0x02)));
    }

    #[test]
    fn a_short_descriptor_never_indexes_past_the_blob() {
        // A device may declare any bLength; these two blobs used to panic on `d[i + 2]` / `d[i + 3]`.
        for d in [
            &[2u8, 0x04][..],                                     // interface, bLength 2
            &[3u8, 0x05, 0x81][..],                               // endpoint, bLength 3
            &[9, 0x04, 0x00, 0, 2, 0xff, 0, 0, 0, 3, 0x05, 0x81][..], // short endpoint at the tail
        ] {
            assert_eq!(parse_bulk_endpoints(d), None, "blob {d:?}");
        }
    }
}
