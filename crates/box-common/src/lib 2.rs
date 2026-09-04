//! box-common — protocol-agnostic box-side foundation shared by the CarPlay/AirPlay set and the
//! Android Auto set. See Cargo.toml for the layering. Each projection "set" depends on this crate
//! for the overlap (OCBM, USB, phone detection, session arbitration) and keeps only its unique
//! protocol logic (iAP2/AirPlay receiver vs AOAP byte pump).

pub mod cfg;
pub mod flags;
pub mod net;
pub mod phone;

/// The agnostic OCBM framing/channel layer. For now a re-export of `ocbm-proto` so every box crate
/// references ONE definition of the channel ids and frame codec; ocbmd's mux-core (OutQueue/poll
/// dispatch) migrates behind this module in a later, separately-verified step.
pub mod ocbm {
    pub use ocbm_proto::*;
}

/// usbdevfs USB-host primitives (control/bulk/claim/reset) + descriptor parsing. Linux-only; a
/// stub keeps off-Linux workspace builds green (mirrors ocbmd's eth module).
#[cfg(target_os = "linux")]
pub mod usb;
#[cfg(not(target_os = "linux"))]
pub mod usb {
    //! Off-Linux stub: usbdevfs does not exist, so the host build gets a no-op surface.
    //!
    //! WIDENED 2026-09-04 from `APPLE_VID` alone to the whole `usb` surface, so `aa-bridge` — which
    //! is nothing but usbdevfs plus a byte pump — COMPILES on the build host and its pure logic
    //! (`pump.rs`) is reachable from `cargo test -p aa-bridge` there. Before this the crate did not
    //! type-check off Linux at all, so every aa-bridge test had to be a cross-compile-and-deploy.
    //!
    //! Every function here is an honest failure, never a plausible success: `enumerate_bus` returns
    //! an empty bus and each transfer returns `ENOSYS`. A host run therefore finds no phone and does
    //! nothing, which is the only safe reading of "there is no USB host controller here".
    use std::os::unix::io::RawFd;

    pub const APPLE_VID: u16 = 0x05ac;

    /// `ENOSYS`, as the errno-shaped `Err` the real functions return.
    const NO_USBFS: i32 = 38;

    #[derive(Clone, Debug)]
    pub struct BusDevice {
        pub path: String,
        pub vid: u16,
        pub pid: u16,
        pub class: u8,
    }

    pub fn control(
        _fd: RawFd,
        _rtype: u8,
        _req: u8,
        _value: u16,
        _index: u16,
        _data: &mut [u8],
        _timeout: u32,
    ) -> Result<usize, String> {
        Err(format!("usbdevfs is Linux-only (errno {NO_USBFS})"))
    }

    pub fn bulk(_fd: RawFd, _ep: u32, _data: &mut [u8], _timeout: u32) -> Result<usize, i32> {
        Err(NO_USBFS)
    }

    pub fn bulk_write_all(_fd: RawFd, _ep: u32, _data: &[u8]) -> Result<(), i32> {
        Err(NO_USBFS)
    }

    pub fn claim_interface(_fd: RawFd, _iface: u32) -> Result<(), String> {
        Err("usbdevfs is Linux-only".to_string())
    }

    pub fn release_interface(_fd: RawFd, _iface: u32) {}

    pub fn reset(_fd: RawFd) {}

    pub fn enumerate_bus(_bus_dir: &str) -> Vec<BusDevice> {
        Vec::new()
    }

    pub fn parse_bulk_endpoints(_d: &[u8]) -> Option<(u8, u8, u8)> {
        None
    }
}
