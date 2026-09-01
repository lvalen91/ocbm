//! box-common — protocol-agnostic box-side foundation shared by the CarPlay/AirPlay set and the
//! Android Auto set. See Cargo.toml for the layering. Each projection "set" depends on this crate
//! for the overlap (OCBM, USB, phone detection, session arbitration) and keeps only its unique
//! protocol logic (iAP2/AirPlay receiver vs AOAP byte pump).

pub mod cfg;
pub mod flags;
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
    pub const APPLE_VID: u16 = 0x05ac;
}
