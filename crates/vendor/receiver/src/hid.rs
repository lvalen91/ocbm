//! HID uplink reports — the receiver injects touch + media-button events into the CarPlay session as
//! HID reports (clean-room from `HIDTouchScreen.c` / `HIDMediaButtons.c`). Report layouts:
//!
//! - **Touchscreen** (5 bytes): `[buttons][x:u16 LE][y:u16 LE]`.
//! - **Media buttons** (1 byte): `[index]`.
//!
//! Touch coordinates are absolute in the advertised display geometry; the control input is normalized
//! (0.0–1.0) and scaled here, matching the C harness.

/// Media-button index values (the HID descriptor's usage order from `HIDMediaButtons.h`).
pub mod media_button {
    pub const NONE: u8 = 0;
    pub const PLAY: u8 = 1;
    pub const PAUSE: u8 = 2;
    pub const PLAY_PAUSE: u8 = 3;
    pub const NEXT_TRACK: u8 = 4;
    pub const PREV_TRACK: u8 = 5;
}

/// Build a 5-byte touchscreen report from absolute coordinates.
pub fn touch_report(buttons: u8, x: u16, y: u16) -> [u8; 5] {
    [buttons, x as u8, (x >> 8) as u8, y as u8, (y >> 8) as u8]
}

/// Build a touchscreen report from normalized (0.0–1.0) coordinates scaled to the display geometry.
pub fn touch_report_normalized(buttons: u8, nx: f64, ny: f64, width: u16, height: u16) -> [u8; 5] {
    let x = (nx.clamp(0.0, 1.0) * width as f64) as u16;
    let y = (ny.clamp(0.0, 1.0) * height as f64) as u16;
    touch_report(buttons, x, y)
}

/// Build a 12-byte two-finger touchscreen report.
///
/// Layout is `HIDTouchScreenMultiFillReport` verbatim (R14G17 `Platform/HIDTouchScreen.c:252`):
/// `[transducer1][buttons1][x1:u16 LE][y1:u16 LE][transducer2][buttons2][x2:u16 LE][y2:u16 LE]`.
///
/// The transducer index is the per-contact identity iOS tracks a finger by across reports; it is the
/// `Usage (Transducer Index)` field the multi descriptor adds ahead of each finger's touch bit and
/// which the single-finger descriptor has no room for. `buttons` is the touch bit — 1 while the
/// contact is down, 0 once it lifts.
///
/// Both contacts ride ONE report because the descriptor declares two `Finger` collections in a
/// single input report. A caller therefore cannot emit fingers independently: it must hold the state
/// of both and re-send the pair whenever either changes.
pub fn touch_report_multi(
    transducer1: u8,
    buttons1: u8,
    x1: u16,
    y1: u16,
    transducer2: u8,
    buttons2: u8,
    x2: u16,
    y2: u16,
) -> [u8; 12] {
    [
        transducer1,
        buttons1,
        x1 as u8,
        (x1 >> 8) as u8,
        y1 as u8,
        (y1 >> 8) as u8,
        transducer2,
        buttons2,
        x2 as u8,
        (x2 >> 8) as u8,
        y2 as u8,
        (y2 >> 8) as u8,
    ]
}

/// [`touch_report_multi`] from normalized (0.0-1.0) coordinates, scaled to the display geometry.
#[allow(clippy::too_many_arguments)]
pub fn touch_report_multi_normalized(
    transducer1: u8,
    buttons1: u8,
    nx1: f64,
    ny1: f64,
    transducer2: u8,
    buttons2: u8,
    nx2: f64,
    ny2: f64,
    width: u16,
    height: u16,
) -> [u8; 12] {
    let sx = |n: f64| (n.clamp(0.0, 1.0) * width as f64) as u16;
    let sy = |n: f64| (n.clamp(0.0, 1.0) * height as f64) as u16;
    touch_report_multi(
        transducer1,
        buttons1,
        sx(nx1),
        sy(ny1),
        transducer2,
        buttons2,
        sx(nx2),
        sy(ny2),
    )
}

/// Build a 1-byte media-button report (use [`media_button::NONE`] to signal release).
pub fn media_button_report(index: u8) -> [u8; 1] {
    [index]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_report_matches_apple_fill_order() {
        // Offsets hand-checked against HIDTouchScreenMultiFillReport, not derived from the builder.
        let r = touch_report_multi(0, 1, 0x0320, 0x01E0, 1, 1, 0x0640, 0x03C0);
        assert_eq!(r.len(), 12);
        assert_eq!(&r[0..6], &[0, 1, 0x20, 0x03, 0xE0, 0x01]);
        assert_eq!(&r[6..12], &[1, 1, 0x40, 0x06, 0xC0, 0x03]);
    }

    #[test]
    fn multi_normalized_scales_and_clamps_each_contact() {
        let r = touch_report_multi_normalized(0, 1, 0.5, 0.5, 1, 1, 2.0, -1.0, 1920, 720);
        assert_eq!(u16::from_le_bytes([r[2], r[3]]), 960);
        assert_eq!(u16::from_le_bytes([r[4], r[5]]), 360);
        // Second contact clamps to the geometry rather than wrapping.
        assert_eq!(u16::from_le_bytes([r[8], r[9]]), 1920);
        assert_eq!(u16::from_le_bytes([r[10], r[11]]), 0);
    }

    #[test]
    fn touch_is_little_endian_xy() {
        // x = 0x0320 (800), y = 0x01E0 (480)
        assert_eq!(touch_report(1, 0x0320, 0x01E0), [1, 0x20, 0x03, 0xE0, 0x01]);
    }

    #[test]
    fn normalized_scales_to_geometry() {
        // centre of a 1920x720 display
        let r = touch_report_normalized(1, 0.5, 0.5, 1920, 720);
        let x = u16::from_le_bytes([r[1], r[2]]);
        let y = u16::from_le_bytes([r[3], r[4]]);
        assert_eq!(x, 960);
        assert_eq!(y, 360);
        assert_eq!(r[0], 1);
        // out-of-range is clamped
        let r2 = touch_report_normalized(0, 2.0, -1.0, 1920, 720);
        assert_eq!(u16::from_le_bytes([r2[1], r2[2]]), 1920);
        assert_eq!(u16::from_le_bytes([r2[3], r2[4]]), 0);
    }

    #[test]
    fn media_buttons() {
        assert_eq!(media_button_report(media_button::PLAY_PAUSE), [3]);
        assert_eq!(media_button_report(media_button::NEXT_TRACK), [4]);
        assert_eq!(media_button_report(media_button::NONE), [0]);
    }
}
