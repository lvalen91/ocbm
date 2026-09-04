//! iap2/link.rs — the iAP2 LINK layer: SOP framing, two's-complement checksum, seq/ack windowing.
//!
//! PURPOSE. Every iAP2 control message rides inside a link-layer packet; this module builds and parses
//! those packets (SYN, bare ACK, and data/control frames) and tracks the sequence/ack numbers. Pure
//! byte logic, no I/O — the driver owns the endpoints. Faithful port of `iap2_pi.c`'s
//! `send_msg`/`send_bare_ack`/RX-header parse, unit-tested against the exact 26-byte SYN captured on
//! the device.
//!
//! PACKET LAYOUT:
//!   [SOP1=FF][SOP2=5A][len BE16][ctl][seq][ack][sess][hdr_cks]   (9-byte header)
//!   [payload...][payload_cks]                                     (only when len > 9)
//! `len` is the whole packet length; a bare ACK is header-only (len=9) and does NOT consume a seq.
//!
//! NO ACCESSORY-SIDE RETRANSMISSION: this module tracks `peer_seq` and builds `ack` fields but never
//! queues sent packets, inspects `rx.ack`, or retransmits on our own direction — fire-and-forget,
//! device-proven on all three current carriers (USB char device, AirPlay tunnel, RFCOMM). If a sent
//! packet is lost, the caller's state machine stalls until its own timeout/retry budget fires; see
//! the callers' `Action::RetryIdentify`/handshake-budget handling for that layer.

pub const SOP1: u8 = 0xFF;
pub const SOP2: u8 = 0x5A;
pub const CTL_SYN: u8 = 0x80;
pub const CTL_ACK: u8 = 0x40;

/// The fixed 6-byte detect prelude (`FF 55 02 00 EE 10`) — not a link packet, sent before SYN.
pub const DETECT: [u8; 6] = [0xFF, 0x55, 0x02, 0x00, 0xEE, 0x10];

/// The 16-byte SYN link parameters (from `iap2_pi.c`: max pkt/retransmit/cumulative-ack timings).
/// Used by the BT/RFCOMM and wired transports — both hardware-proven, do not change.
///
/// Field layout (confirmed 2026-07-25 against three independent implementations: `iAP2PacketParseSYNData`
/// in SpeedPlay's `libcustomiap.so`, `OnReceiveSYN` in Cinemo's `libNmeIAP.so`, and
/// `iAP2LinkIsValidSynParam` in Apple's CarPlaySimulator):
///   [0]      LinkVersion              = 1
///   [1]      MaxOutstandingPackets    = 32
///   [2..4]   MaxRcvPacketLength  BE16 = 4096
///   [4..6]   RetransmitTimeout   BE16 = 2000 ms
///   [6..8]   CumulativeAckTimeout BE16= 2000 ms
///   [8]      MaxRetransmissions       = 30
///   [9]      MaxCumulativeAcks        = 6
///   [10..13] session {id=1, type=0 Control,      version=1}
///   [13..16] session {id=2, type=1 FileTransfer, version=1}
///
/// Constraints confirmed from Apple's own validator (CarPlaySimulator), worth knowing before editing:
///   * `MaxOutstandingPackets` must be **1..=127** — the check is signed-byte (`sxtb`), so 128..255 is
///     rejected. Zero is permitted ONLY when the block is Zero-Ack, and never required.
///   * `MaxRcvPacketLength` must be > 23.
///   * **LinkVersion must stay 1.** At version >= 2 the parser reinterprets `wire[10]` as a command
///     count instead of the first session descriptor's id — bumping it would silently mis-parse the
///     session list.
///   * Session count is derived as `(payload_len - 10) / 3`, so 16 payload bytes = exactly 2 sessions.
pub const SYN_PARAMS: [u8; 16] = [1, 32, 0x10, 0x00, 0x07, 0xD0, 0x07, 0xD0, 30, 6, 1, 0, 1, 2, 1, 1];

/// **Zero-Ack** link parameters, for iAP2 tunnelled over a already-reliable ordered carrier — i.e. the
/// AirPlay control channel. Apple's CarPlay Communication Plug-in R14G17 Integration Guide (line 290)
/// says of iAP2-over-CarPlay: *"The Zero-Ack implementation is recommended for the link parameters."*
///
/// The AISpec is not available to this project, so the definition was established from two independent
/// shipping implementations, which encode the **identical predicate** (see docs/carplay/03_SDK_GROUND_TRUTH.md §8, docs/ops/03_REFERENCE_INDEX.md §B):
///   * Apple's own `iAP2LinkIsNoRetransmit` (CarPlaySimulator @0x3a3074) — returns true iff
///     retransmitTimeout, cumAckTimeout, maxRetransmissions and maxCumulativeAcks are ALL zero, and
///     `iAP2LinkIsValidSynParam` then skips every normal range check (which would otherwise demand
///     retransmitTimeout >= 20, cumAckTimeout >= 10, maxRetransmissions in 1..=30).
///   * Cinemo's `InitLinkParams`/`OnReceiveSYN` (`libNmeIAP.so` @0x200258/@0x202e2c) — zeroes the same
///     four fields together, and logs "ZeroACK link configuration is not supported" if the peer's SYN
///     has any of them non-zero.
///
/// This is the MINIMAL-DELTA variant: identical to `SYN_PARAMS` except the four ack/retransmit fields
/// are zeroed. Apple's own transport-type-2 template additionally uses MaxOutstandingPackets=20,
/// MaxRcvPacketLength=0xFFFF and session version 2 — deliberately NOT adopted here, because a 65535-byte
/// declared length exceeds what `parse` can reassemble from a single delivery (it rejects
/// `declared_len > b.len()` rather than buffering), and changing session versions at the same time
/// would confound the test. Revisit both if Zero-Ack alone doesn't complete the handshake.
///
/// NOTE `MaxOutstandingPackets` is NOT dead under Zero-Ack — Cinemo's own log calls it
/// "max_outstanding_packets (ACK after)", i.e. with no timers it becomes "send a bare ACK every N
/// packets". Apple's validator uniquely permits 0 here in this mode; do not set it to 0.
pub const SYN_PARAMS_ZERO_ACK: [u8; 16] =
    [1, 32, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0, 0, 1, 0, 1, 2, 1, 1];

/// Zero-Ack SYN parameters for the **AirPlay RCS DataStream** link (docs/carplay/05_METADATA_AND_CONTROLS.md). Identical to
/// [`SYN_PARAMS_ZERO_ACK`] except `MaxPacketSize` is `0xFFFF` instead of `0x1000`.
///
/// PRIMARY JUSTIFICATION is byte-identity with the iPhone's own SYN-ACK on this link, not Apple's
/// transport-type-2 template — that template additionally uses `MaxOutstandingPackets=20` and session
/// version 2, neither of which this constant adopts, so citing it alone would overstate the case.
///
/// FORMER OBJECTION (now resolved, audit Fix #14): [`SYN_PARAMS_ZERO_ACK`]'s comment argued against
/// `0xFFFF` because `Link::parse` rejects `declared_len > b.len()` and — at the time — nothing
/// reassembled across deliveries. That hole is now CLOSED on the tunnel path: `receiver::session`
/// accumulates the raw ciphertext frames (`acc`), decrypts each into an RCS byte buffer (`rcs`), and
/// reassembles one RCS message across multiple ChaCha20 frames (`drain_rcs`, 256 KiB cap) before it
/// reaches `datastream::unwrap`, so the full 65535-byte inbound link packet this advertises arrives
/// whole. The value is also free at handshake time — the device echoes these params back verbatim and
/// the negotiation does not compare `MaxPacketSize` (it is per-direction). (Note: the separate RFCOMM
/// `bt_driver` split-frame gap is unrelated and still open.)
///
/// AUTHORITATIVE FIELD ORDER, from Apple's own `iAP2Link.c` (statically linked into Xcode's
/// `CarPlaySimulator.devicekitplugin`, build path
/// `…/CarPlaySimulator_Devices/Libraries/iAP2/iAP2/Public/iAP2Link/iAP2Link.c`), whose validator prints:
///
/// ```text
/// version=%d
/// maxOutstanding=%d maxPacketSize=%d
/// retransmitTimeout=%d cumAckTimeout=%d
/// maxRetransmissions=%d maxCumAck=%d
/// numSessionInfo=%u
/// ```
///
/// So bytes 2..4 are **MaxPacketSize**, not `MaxRetransmissions` as an earlier comment here assumed
/// (that reading came from SpeedPlay's re-derived stack, and it was wrong).
///
/// WHY THIS VALUE: the iPhone's own SYN-ACK on this link offers
/// `01 20 FF FF 00 00 00 00 00 00 01 00 01 02 01 01` — byte-identical to this constant. With `0x1000`
/// we were the only party proposing 4096, and the device restarted the handshake (new SYN-ACK sequence)
/// instead of accepting our ACK. The stream's own SETUP declares `controlType=2`, matching the
/// transport-type-2 template `link.rs`'s original note described but deferred.
///
/// SCOPE: used ONLY by the AirPlay tunnel. The BT and wired links keep [`SYN_PARAMS_ZERO_ACK`] /
/// [`SYN_PARAMS`] — both are proven and must not be disturbed to fix a third transport.
pub const SYN_PARAMS_ZERO_ACK_TUNNEL: [u8; 16] =
    [1, 32, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0, 0, 1, 0, 1, 2, 1, 1];

/// Non-Zero-Ack fallback for the **AirPlay RCS DataStream** link (docs/carplay/05_METADATA_AND_CONTROLS.md). Identical to [`SYN_PARAMS`]
/// (the proven BT/wired retransmit profile) except `MaxPacketSize` stays `0xFFFF`.
///
/// Exists because the one-shot fallback in `iap_tunnel` previously re-SYNed with `SYN_PARAMS`, whose
/// `MaxPacketSize` is `0x1000` — silently undoing the transport-type-2 correction on the retry path.
pub const SYN_PARAMS_TUNNEL_RETRANSMIT: [u8; 16] =
    [1, 32, 0xFF, 0xFF, 0x07, 0xD0, 0x07, 0xD0, 30, 6, 1, 0, 1, 2, 1, 1];

/// Does a 16-byte SYN parameter block declare Zero-Ack? True iff `RetransmitTimeout` (bytes 4..6),
/// `CumulativeAckTimeout` (6..8), `MaxRetransmissions` (8) and `MaxCumulativeAcks` (9) are ALL zero —
/// byte-for-byte the predicate Apple's `iAP2LinkIsNoRetransmit` and Cinemo's `OnReceiveSYN` both apply.
///
/// **The caller owns frame-type validation.** This only inspects four fields; it does not check the
/// block's length beyond the guard, nor the LinkVersion, so an all-zero 10-byte buffer reads as
/// Zero-Ack. Both call sites are behind `Rx::is_syn_ack()` with a verified checksum — do not reuse this
/// as a general classifier without adding those checks.
///
/// **Returning `false` means "not Zero-Ack OR not enough bytes to tell".** Callers that act on a
/// decline must check the length separately; conflating the two makes a header-only SYN-ACK look like a
/// peer refusal (see `iap_tunnel.rs`'s `peer_declined`).
///
/// Both sides must agree: Cinemo explicitly rejects a peer SYN whose four fields aren't all zero
/// ("ZeroACK link configuration is not supported") and reverts to normal parameters, and Apple has a
/// matching FSM fallback (`iAP2LinkAccessoryActionRestartSYNWithRetransmit`). Callers should therefore
/// check the peer's echoed parameters and be prepared to re-SYN with `SYN_PARAMS`.
///
/// Accepts any slice ≥ 10 bytes so it can be applied to a received SYN-ACK payload, which carries the
/// peer's parameter block possibly followed by its checksum byte.
pub fn is_zero_ack(params: &[u8]) -> bool {
    params.len() >= 10 && params[4..10].iter().all(|&b| b == 0)
}

/// Two's-complement checksum: `(0x100 - (Σ bytes & 0xff)) & 0xff`.
pub fn cks(b: &[u8]) -> u8 {
    let sum: u32 = b.iter().map(|&x| x as u32).sum();
    ((0x100 - (sum & 0xff)) & 0xff) as u8
}

/// A received link packet with its header parsed. `payload` is the post-header region as the C treats
/// it (`b[9..]`, i.e. it still carries the trailing payload-checksum byte).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rx {
    pub ctl: u8,
    pub seq: u8,
    pub ack: u8,
    pub sess: u8,
    pub payload: Vec<u8>,
}

impl Rx {
    /// Both SYN and ACK control bits set — the link-up SYN-ACK.
    pub fn is_syn_ack(&self) -> bool {
        (self.ctl & 0xC0) == 0xC0
    }
}

/// The accessory link endpoint: our outgoing seq and the peer's last seq (echoed as ack).
#[derive(Debug, Default)]
pub struct Link {
    pub my_seq: u8,
    pub peer_seq: u8,
}

impl Link {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a SYN packet carrying the 16-byte link params (consumes a seq).
    pub fn build_syn(&mut self, params: &[u8; 16]) -> Vec<u8> {
        let total = 9 + 16 + 1;
        let mut pkt = vec![
            SOP1, SOP2, (total >> 8) as u8, (total & 0xff) as u8, CTL_SYN, self.my_seq, 0, 0,
        ];
        self.my_seq = self.my_seq.wrapping_add(1);
        pkt.push(cks(&pkt[..8]));
        pkt.extend_from_slice(params);
        pkt.push(cks(params));
        pkt
    }

    /// Build a control-session message packet (consumes a seq).
    pub fn build_msg(&mut self, sess: u8, msg_id: u16, body: &[u8]) -> Vec<u8> {
        let payload = msg_payload(msg_id, body);
        let total = 9 + payload.len() + 1;
        debug_assert!(total <= u16::MAX as usize, "link frame length overflows u16"); // audit Fix #23
        let mut pkt = vec![
            SOP1, SOP2, (total >> 8) as u8, (total & 0xff) as u8,
            CTL_ACK, self.my_seq, self.peer_seq, sess,
        ];
        self.my_seq = self.my_seq.wrapping_add(1);
        pkt.push(cks(&pkt[..8]));
        pkt.extend_from_slice(&payload);
        pkt.push(cks(&payload));
        pkt
    }

    /// Build a packet whose payload is sent RAW on `sess` — no `[40 40][total][msg_id]` control
    /// wrapper. Needed by the **File Transfer session (session 2)**, whose datagrams are bare
    /// `[id][opcode][args…]` (the album-artwork path: Accept `[id][01]` / Success `[id][05]`).
    /// Consumes a seq like `build_msg`. See ccpa_custom `iap2d/src/metadata.rs`.
    pub fn build_raw(&mut self, sess: u8, payload: &[u8]) -> Vec<u8> {
        let total = 9 + payload.len() + 1;
        debug_assert!(total <= u16::MAX as usize, "link raw frame length overflows u16"); // audit Fix #23
        let mut pkt = vec![
            SOP1, SOP2, (total >> 8) as u8, (total & 0xff) as u8,
            CTL_ACK, self.my_seq, self.peer_seq, sess,
        ];
        self.my_seq = self.my_seq.wrapping_add(1);
        pkt.push(cks(&pkt[..8]));
        pkt.extend_from_slice(payload);
        pkt.push(cks(payload));
        pkt
    }

    /// Build a bare ACK — header only, does NOT consume a seq (echoes `my_seq-1`), like `send_bare_ack`.
    pub fn build_ack(&self) -> Vec<u8> {
        let mut a = vec![
            SOP1, SOP2, 0, 9, CTL_ACK, self.my_seq.wrapping_sub(1), self.peer_seq, 0,
        ];
        a.push(cks(&a[..8]));
        a
    }

    /// Parse the FIRST link packet from a received buffer, recording the peer seq. `None` if it is
    /// not a valid SOP frame: too short, wrong magic, a declared length that doesn't fit the buffer,
    /// or a bad header/payload checksum. The checksum verification (audit finding: a prior version
    /// accepted any buffer starting with the magic bytes regardless of checksum, silently desyncing
    /// peer_seq on a bit-flipped or torn USB read) is kept — it's the only place in the stack that
    /// can catch wire corruption.
    ///
    /// COALESCED READS: we bound the packet by its own `declared_len` rather than requiring it to
    /// equal `b.len()`. The `/dev/android_iap2` char device can hand us more than one link packet
    /// in a single read — in particular the iPhone piggybacks `0xAA05 AuthSuccess` with the next
    /// `0x1D00 StartIdentification` frame, so that read carries two packets. Parsing just the first
    /// (and letting the driver's per-packet ACK make the iPhone retransmit the rest) matches the
    /// proven `iap2_auth.c`, which processes one message per read. The prior `declared_len != b.len()`
    /// rejection silently dropped that coalesced AuthSuccess and stalled authentication at SignSent.
    pub fn parse(&mut self, b: &[u8]) -> Option<Rx> {
        if b.len() < 9 || b[0] != SOP1 || b[1] != SOP2 {
            return None;
        }
        let declared_len = ((b[2] as usize) << 8) | b[3] as usize;
        // The first packet must be a complete link frame (>=9 header bytes) that fits in the read.
        if declared_len < 9 || declared_len > b.len() {
            return None;
        }
        let pkt = &b[..declared_len]; // ignore any trailing coalesced packet(s)
        if cks(&pkt[..8]) != pkt[8] {
            return None;
        }
        let payload = if declared_len > 9 {
            let body = &pkt[9..];
            // The trailing byte is the payload checksum (see msg_payload/build_msg); the C's own
            // `payload` slice included it, so Rx.payload keeps that shape, but we still verify it.
            let (data, sum_byte) = body.split_at(body.len() - 1);
            if cks(data) != sum_byte[0] {
                return None;
            }
            body.to_vec()
        } else {
            Vec::new()
        };
        let rx = Rx { ctl: pkt[4], seq: pkt[5], ack: pkt[6], sess: pkt[7], payload };
        self.peer_seq = rx.seq;
        Some(rx)
    }
}

/// The control-session payload: `[40 40][total BE16][msg_id BE16][body]`, `total = 6 + body.len()`.
pub fn msg_payload(msg_id: u16, body: &[u8]) -> Vec<u8> {
    let total = 6 + body.len();
    debug_assert!(total <= u16::MAX as usize, "control payload length overflows u16"); // audit Fix #23
    let mut pl = vec![
        0x40, 0x40, (total >> 8) as u8, (total & 0xff) as u8, (msg_id >> 8) as u8, (msg_id & 0xff) as u8,
    ];
    pl.extend_from_slice(body);
    pl
}

/// Byte length of the FIRST link packet in `b` (its declared length), or `None` if `b` doesn't start
/// with a plausible frame. Lets a caller walk a COALESCED read packet-by-packet instead of dropping
/// everything after the first — required by the File-Transfer session (album artwork), whose ~26 data
/// fragments arrive several-per-read and are never retransmitted once ACKed, so a first-packet-only
/// drain silently truncates the file (observed 2026-07-12: a 92 KB JPEG completed at 47 KB).
pub fn packet_len(b: &[u8]) -> Option<usize> {
    if b.len() < 9 || b[0] != SOP1 || b[1] != SOP2 {
        return None;
    }
    let declared_len = ((b[2] as usize) << 8) | b[3] as usize;
    if declared_len < 9 || declared_len > b.len() {
        return None;
    }
    Some(declared_len)
}

/// Extract the message id from a control-session payload, if well-formed (`40 40` prefix, ≥6 bytes).
pub fn parse_msg_id(payload: &[u8]) -> Option<u16> {
    if payload.len() < 6 || payload[0] != 0x40 || payload[1] != 0x40 {
        return None;
    }
    Some(((payload[4] as u16) << 8) | payload[5] as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_matches_device_vectors() {
        // SYN header (first 8 bytes) checksum from the live capture = 0x0d.
        assert_eq!(cks(&[0xFF, 0x5A, 0x00, 0x1A, 0x80, 0x00, 0x00, 0x00]), 0x0D);
        // SYN param checksum from the live capture = 0xf7.
        assert_eq!(cks(&SYN_PARAMS), 0xF7);
    }

    #[test]
    fn build_syn_matches_captured_bytes() {
        // The exact 26-byte SYN observed on the device:
        // ff 5a 00 1a 80 00 00 00 0d 01 20 10 00 07 d0 07 d0 1e 06 01 00 01 02 01 01 f7
        let expected: [u8; 26] = [
            0xFF, 0x5A, 0x00, 0x1A, 0x80, 0x00, 0x00, 0x00, 0x0D, 0x01, 0x20, 0x10, 0x00, 0x07,
            0xD0, 0x07, 0xD0, 0x1E, 0x06, 0x01, 0x00, 0x01, 0x02, 0x01, 0x01, 0xF7,
        ];
        let mut link = Link::new();
        assert_eq!(link.build_syn(&SYN_PARAMS), expected);
        assert_eq!(link.my_seq, 1, "SYN consumes a seq");
    }

    #[test]
    fn zero_ack_params_differ_from_normal_only_in_the_four_ack_fields() {
        // The Zero-Ack definition, pinned: bytes 4..10 (RetransmitTimeout BE16, CumulativeAckTimeout
        // BE16, MaxRetransmissions, MaxCumulativeAcks) are ALL zero and NOTHING else changes. This is
        // the exact predicate Apple's `iAP2LinkIsNoRetransmit` and Cinemo's `OnReceiveSYN` apply.
        assert!(is_zero_ack(&SYN_PARAMS_ZERO_ACK));
        assert!(!is_zero_ack(&SYN_PARAMS), "BT/wired params are NOT zero-ack");
        for i in 0..16 {
            if (4..10).contains(&i) {
                assert_eq!(SYN_PARAMS_ZERO_ACK[i], 0, "byte {i} must be zeroed for Zero-Ack");
            } else {
                assert_eq!(
                    SYN_PARAMS_ZERO_ACK[i], SYN_PARAMS[i],
                    "byte {i} must be unchanged from the proven params (minimal-delta variant)"
                );
            }
        }
        // MaxOutstandingPackets keeps its meaning under Zero-Ack ("ACK after N packets") — it must not
        // be zeroed even though Apple's validator uniquely permits 0 in this mode.
        assert_ne!(SYN_PARAMS_ZERO_ACK[1], 0);
        // Session descriptors intact: control (id 1, type 0) + file transfer (id 2, type 1) — the
        // artwork path needs session 2, and Apple's own Zero-Ack template declares it too.
        assert_eq!(&SYN_PARAMS_ZERO_ACK[10..16], &[1, 0, 1, 2, 1, 1]);
    }

    #[test]
    fn is_zero_ack_tolerates_a_trailing_checksum_and_short_input() {
        let mut with_cks = SYN_PARAMS_ZERO_ACK.to_vec();
        with_cks.push(cks(&SYN_PARAMS_ZERO_ACK));
        assert!(is_zero_ack(&with_cks), "must work on a received SYN-ACK payload");
        assert!(!is_zero_ack(&[0u8; 9]), "too short to judge → not zero-ack");
    }

    #[test]
    fn msg_payload_and_parse_roundtrip() {
        let pl = msg_payload(0xAA01, &[1, 2, 3]);
        // [40 40][00 09][AA 01][01 02 03], total = 6 + 3 = 9
        assert_eq!(pl, vec![0x40, 0x40, 0x00, 0x09, 0xAA, 0x01, 1, 2, 3]);
        assert_eq!(parse_msg_id(&pl), Some(0xAA01));
        assert_eq!(parse_msg_id(&[0x40, 0x41]), None);
    }

    #[test]
    fn bare_ack_is_header_only() {
        let mut link = Link::new();
        link.my_seq = 3;
        link.peer_seq = 7;
        let a = link.build_ack();
        assert_eq!(a.len(), 9);
        assert_eq!(a[5], 2, "ack echoes my_seq-1");
        assert_eq!(a[6], 7, "ack carries peer_seq");
        assert_eq!(a[8], cks(&a[..8]));
    }

    #[test]
    fn parse_records_peer_seq_and_detects_syn_ack() {
        let mut link = Link::new();
        // ctl 0xC0 = SYN|ACK. Header checksum must be correct now that parse() verifies it:
        // cks(&[FF,5A,00,09,C0,2A,00,00]) = 0xB4.
        let header = [SOP1, SOP2, 0, 9, 0xC0, 42, 0, 0];
        let mut pkt = header.to_vec();
        pkt.push(cks(&header));
        let rx = link.parse(&pkt).unwrap();
        assert!(rx.is_syn_ack());
        assert_eq!(link.peer_seq, 42);
        assert!(link.parse(&[0x00, 0x00]).is_none());
    }

    #[test]
    fn parse_rejects_bad_header_checksum() {
        let mut link = Link::new();
        // Same header as above but with a deliberately wrong checksum byte (0 instead of 0xB4) --
        // must now be rejected rather than silently accepted with a corrupted peer_seq.
        assert!(link.parse(&[SOP1, SOP2, 0, 9, 0xC0, 42, 0, 0, 0]).is_none());
        assert_eq!(link.peer_seq, 0, "a rejected packet must not update peer_seq");
    }

    #[test]
    fn parse_rejects_declared_length_mismatch() {
        let mut link = Link::new();
        let header = [SOP1, SOP2, 0, 26, 0xC0, 42, 0, 0]; // declares 26 bytes, but only 9 supplied
        let mut pkt = header.to_vec();
        pkt.push(cks(&header));
        assert!(link.parse(&pkt).is_none());
    }

    #[test]
    fn parse_rejects_bad_payload_checksum() {
        let mut link = Link::new();
        let mut pkt = link.build_msg(1, 0xAA01, &[1, 2, 3]);
        let last = pkt.len() - 1;
        pkt[last] ^= 0xFF; // corrupt the trailing payload checksum byte
        assert!(link.parse(&pkt).is_none());
    }

    #[test]
    fn parse_first_packet_of_coalesced_read() {
        // Regression: the iPhone piggybacks 0xAA05 AuthSuccess with the next 0x1D00 frame in a
        // single /dev/android_iap2 read. parse() must return the FIRST packet (not drop the whole
        // buffer because declared_len != b.len()), and the trailing packet must remain parseable.
        let mut sender = Link::new();
        sender.peer_seq = 5;
        let p1 = sender.build_msg(1, 0xAA05, &[]); // AuthSuccess
        let p2 = sender.build_msg(1, 0x1D00, &[]); // StartIdentification, coalesced behind it
        let seq1 = p1[5];
        let mut buf = p1.clone();
        buf.extend_from_slice(&p2);

        let mut rx_link = Link::new();
        let rx = rx_link.parse(&buf).expect("first packet parses despite trailing data");
        assert_eq!(parse_msg_id(&rx.payload), Some(0xAA05));
        assert_eq!(rx_link.peer_seq, seq1, "peer_seq is the FIRST packet's seq");
        // the trailing coalesced packet is independently parseable (driver/retransmit handles it)
        let rx2 = rx_link.parse(&buf[p1.len()..]).expect("second packet parses");
        assert_eq!(parse_msg_id(&rx2.payload), Some(0x1D00));
    }
}
