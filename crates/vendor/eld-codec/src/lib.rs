//! `eld-codec` — a minimal libfdk-aac **AAC-ELD encoder** for the CarPlay MainAudio mic uplink.
//!
//! All `unsafe` FFI is isolated here (over the `csrc/eld_shim.c` C ABI) so the `receiver` crate stays
//! `#![forbid(unsafe_code)]`. 16 kHz mono produces the AudioSpecificConfig `f8f03000` — byte-identical
//! to what the iPhone uses for the `speechRecognition` stream (verified by the encoder spike + tests).

use std::os::raw::{c_int, c_void};

extern "C" {
    fn eld_enc_open(sr: c_int, ch: c_int) -> *mut c_void;
    fn eld_enc_asc(h: *mut c_void, asc: *mut u8) -> c_int;
    fn eld_enc_frame_len(h: *mut c_void) -> c_int;
    fn eld_enc_encode(h: *mut c_void, pcm: *const i16, nsamp: c_int, out: *mut u8, outcap: c_int) -> c_int;
    fn eld_enc_close(h: *mut c_void);
}

/// One AAC-ELD encoder instance (one stream).
pub struct EldEncoder {
    h: *mut c_void,
    frame_len: usize,
}

// The raw handle is only ever touched through `&mut self` (the encoder is not used concurrently),
// so it is safe to move the wrapper between threads.
unsafe impl Send for EldEncoder {}

impl EldEncoder {
    /// Open an ELD encoder for `sample_rate`/`channels`. `None` if libfdk-aac rejects the config.
    pub fn new(sample_rate: u32, channels: u16) -> Option<EldEncoder> {
        let h = unsafe { eld_enc_open(sample_rate as c_int, channels as c_int) };
        if h.is_null() {
            return None;
        }
        let frame_len = unsafe { eld_enc_frame_len(h) };
        if frame_len <= 0 {
            // A 0 frame_len would busy-loop callers chunking PCM by frame_len; treat it as a
            // rejected config. Close the handle rather than leaking it (mirrors the null check).
            unsafe { eld_enc_close(h) };
            return None;
        }
        Some(EldEncoder { h, frame_len: frame_len as usize })
    }

    /// PCM samples (per channel) consumed per encoded frame — 480 for 16 kHz ELD.
    pub fn frame_len(&self) -> usize {
        self.frame_len
    }

    /// The AudioSpecificConfig the encoder emits (e.g. `f8f03000` for 16 kHz mono).
    pub fn asc(&self) -> Vec<u8> {
        let mut buf = [0u8; 64];
        let n = unsafe { eld_enc_asc(self.h, buf.as_mut_ptr()) }.max(0) as usize;
        buf[..n].to_vec()
    }

    /// Encode one frame (`frame_len` interleaved S16 samples) → one ELD access unit. Returns an empty
    /// vec while the encoder is priming (its initial ~1-frame delay) or on a transient encoder error.
    pub fn encode(&mut self, pcm: &[i16]) -> Vec<u8> {
        let mut out = vec![0u8; 1536];
        let n = unsafe {
            eld_enc_encode(self.h, pcm.as_ptr(), pcm.len() as c_int, out.as_mut_ptr(), out.len() as c_int)
        };
        if n <= 0 {
            return Vec::new();
        }
        out.truncate(n as usize);
        out
    }
}

impl Drop for EldEncoder {
    fn drop(&mut self) {
        unsafe { eld_enc_close(self.h) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eld_16k_mono_asc_matches_iphone() {
        let enc = EldEncoder::new(16000, 1).expect("open ELD encoder");
        assert_eq!(enc.frame_len(), 480);
        assert_eq!(enc.asc(), vec![0xf8, 0xf0, 0x30, 0x00]); // == the iPhone's speechRecognition ASC
    }

    #[test]
    fn produces_access_units_after_priming() {
        let mut enc = EldEncoder::new(16000, 1).unwrap();
        let frame = vec![0i16; enc.frame_len()];
        let produced = (0..8).filter(|_| !enc.encode(&frame).is_empty()).count();
        assert!(produced > 0, "encoder produced no access units");
    }
}
