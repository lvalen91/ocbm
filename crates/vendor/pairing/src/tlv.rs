//! TLV8 — the Apple/HomeKit type-length-value codec used by pair-setup, pair-verify and
//! MFi-SAP. Clean-room from the public TLV8 format: a stream of `[type:u8][len:u8][value]`
//! items; a value longer than 255 bytes is split into consecutive fragments **of the same
//! type**, every fragment but the last being exactly 255 bytes, and decoders coalesce them
//! (cf. AccessorySDK `TLV8CopyCoalesced`). Zero-dependency, `#![forbid(unsafe_code)]`.
//!
//! Note on the coalescing rule: [`decode`] merges **every** run of consecutive same-type items
//! into one value, exactly like the C `TLV8CopyCoalesced` (a zero-length continuation contributes
//! nothing but does not break the run; a different type ends it). Distinct values of the same type
//! are disambiguated by a zero-length separator (e.g. `0xFF`) between them — the different type
//! byte breaks the run — which is how the encoder always emits them.

/// Maximum value bytes in a single TLV8 fragment (the length field is one byte).
pub const MAX_FRAGMENT: usize = 255;

/// TLV8 decode errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlvError {
    /// The buffer ended in the middle of a header or value (offset of the short read).
    Truncated { at: usize },
}

impl core::fmt::Display for TlvError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TlvError::Truncated { at } => write!(f, "TLV8 truncated at offset {at}"),
        }
    }
}

impl std::error::Error for TlvError {}

/// Encode `(type, value)` items into a TLV8 buffer. Values longer than [`MAX_FRAGMENT`] are
/// split into consecutive same-type fragments that [`decode`] coalesces back together.
pub fn encode(items: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(ty, val) in items {
        if val.is_empty() {
            out.push(ty);
            out.push(0);
            continue;
        }
        let mut off = 0;
        while off < val.len() {
            let n = core::cmp::min(MAX_FRAGMENT, val.len() - off);
            out.push(ty);
            out.push(n as u8);
            out.extend_from_slice(&val[off..off + n]);
            off += n;
        }
    }
    out
}

/// Decode a TLV8 buffer into `(type, value)` items, coalescing fragmented values.
pub fn decode(buf: &[u8]) -> Result<Vec<(u8, Vec<u8>)>, TlvError> {
    let mut items: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        if i + 2 > buf.len() {
            return Err(TlvError::Truncated { at: i });
        }
        let ty = buf[i];
        let len = buf[i + 1] as usize;
        i += 2;
        if i + len > buf.len() {
            return Err(TlvError::Truncated { at: i });
        }
        let val = &buf[i..i + len];
        i += len;
        // Coalesce every run of consecutive same-type items into one value (matches the C
        // `TLV8CopyCoalesced`): a zero-length continuation adds nothing but keeps the run going,
        // a different type starts a new item, and the same type reappearing after a different
        // type is a new, distinct value. Distinct same-type values are separated by a `0xFF`
        // separator whose different type byte breaks the run.
        match items.last_mut() {
            Some((prev_ty, prev_val)) if *prev_ty == ty => prev_val.extend_from_slice(val),
            _ => items.push((ty, val.to_vec())),
        }
    }
    Ok(items)
}

/// Return the (coalesced) value of the first item with the given type, if present.
pub fn get(buf: &[u8], ty: u8) -> Option<Vec<u8>> {
    decode(buf).ok()?.into_iter().find(|(t, _)| *t == ty).map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_small() {
        let buf = encode(&[(0x06, &[0x01]), (0x01, b"hello")]);
        assert_eq!(buf, vec![0x06, 0x01, 0x01, 0x01, 0x05, b'h', b'e', b'l', b'l', b'o']);
        let items = decode(&buf).unwrap();
        assert_eq!(items, vec![(0x06, vec![0x01]), (0x01, b"hello".to_vec())]);
        assert_eq!(get(&buf, 0x01).unwrap(), b"hello");
        assert_eq!(get(&buf, 0x99), None);
    }

    #[test]
    fn empty_value() {
        let buf = encode(&[(0xFF, &[])]);
        assert_eq!(buf, vec![0xFF, 0x00]);
        assert_eq!(decode(&buf).unwrap(), vec![(0xFF, vec![])]);
    }

    #[test]
    fn exactly_255_is_one_fragment() {
        let v = vec![0xAB; 255];
        let buf = encode(&[(0x05, &v)]);
        assert_eq!(buf.len(), 2 + 255); // single fragment
        assert_eq!(buf[1], 255);
        assert_eq!(decode(&buf).unwrap(), vec![(0x05, v)]);
    }

    #[test]
    fn fragments_over_255_coalesce() {
        let v: Vec<u8> = (0..600u32).map(|n| n as u8).collect(); // 600 bytes → 255+255+90
        let buf = encode(&[(0x05, &v)]);
        // headers: 255,255,90
        assert_eq!(buf[1], 255);
        assert_eq!(buf[2 + 255 + 1], 255);
        assert_eq!(buf[2 + 255 + 2 + 255 + 1], 90);
        let items = decode(&buf).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].0, 0x05);
        assert_eq!(items[0].1, v);
    }

    #[test]
    fn distinct_same_type_items_stay_split() {
        // two short same-type items separated by a different type — must NOT coalesce
        let buf = encode(&[(0x01, b"a"), (0xFF, &[]), (0x01, b"b")]);
        let items = decode(&buf).unwrap();
        assert_eq!(
            items,
            vec![(0x01, b"a".to_vec()), (0xFF, vec![]), (0x01, b"b".to_vec())]
        );
    }

    #[test]
    fn bare_adjacent_same_type_coalesce() {
        // Two same-type items with NO separator between them are one coalesced value, matching the
        // C TLV8CopyCoalesced (the encoder only produces this shape for a fragmented >255B value).
        let buf = vec![0x03, 0x02, 0xAA, 0xBB, 0x03, 0x01, 0xCC];
        assert_eq!(decode(&buf).unwrap(), vec![(0x03, vec![0xAA, 0xBB, 0xCC])]);
    }

    #[test]
    fn multiple_types_and_order_preserved() {
        let items_in: Vec<(u8, &[u8])> =
            vec![(0x06, &[0x03][..]), (0x03, b"pubkey"), (0x05, b"enc")];
        let buf = encode(&items_in);
        let out = decode(&buf).unwrap();
        assert_eq!(out[0], (0x06, vec![0x03]));
        assert_eq!(out[1], (0x03, b"pubkey".to_vec()));
        assert_eq!(out[2], (0x05, b"enc".to_vec()));
    }

    #[test]
    fn truncated_header_and_value_error() {
        assert_eq!(decode(&[0x01]), Err(TlvError::Truncated { at: 0 }));
        assert_eq!(decode(&[0x01, 0x05, b'h', b'i']), Err(TlvError::Truncated { at: 2 }));
    }
}
