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

/// Read an environment lever by its current name, falling back to the pre-2026-09-08 `CARPLAY_*`
/// name it was renamed from.
///
/// The renamed levers are the PROTOCOL-NEUTRAL ones: Bluetooth SSP/HFP/HCI/RFCOMM settings that
/// apply to any peer (a Pixel bonds through the same SSP agent an iPhone does), and box-level
/// settings like the SoftAP role. Keeping a `CARPLAY_` prefix on them is what let 3,700 lines of
/// Android Auto code accumulate inside a binary called `carplay-wireless`, and it is why
/// `CARPLAY_AA_HEADSET_PATH` — a CarPlay-prefixed variable whose entire purpose is Android Auto —
/// existed. Genuinely Apple levers (`CARPLAY_DEVICE_ID`, `CARPLAY_PI`, `CARPLAY_HEVC`, …) keep
/// their names: there the prefix is information, not noise.
///
/// The fallback exists because a lever that silently reverts to its default is the worst failure
/// shape here — `CARPLAY_WIFI_AP` was exported by the supervisor and read by NOTHING for as long as
/// the bridge role existed, and nothing reported it. So an old name still works, and says so.
pub fn lever(name: &str, legacy: &str) -> Option<String> {
    if let Ok(v) = std::env::var(name) {
        return Some(v);
    }
    match std::env::var(legacy) {
        Ok(v) => {
            eprintln!("[lever] {legacy} is the OLD name for {name} — honouring it, but update the launcher");
            Some(v)
        }
        Err(_) => None,
    }
}
