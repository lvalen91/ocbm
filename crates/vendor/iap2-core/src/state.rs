//! iap2/state.rs — the iAP2 authentication + identification STATE MACHINE (pure transition logic).
//!
//! PURPOSE. Given the accessory's current state and a received message id, decide the next state and
//! what the driver must DO (fetch an MFi cert, sign a challenge, send IdentifyInfo, note a milestone,
//! abort, or ignore). Returning an Action instead of doing I/O keeps the whole flow unit-testable
//! without hardware. Ported from `iap2_pi.c`'s main dispatch.
//!
//! MESSAGE IDS (Apple's authoritative catalog, see `spec`). `0xAA0x` = the on-wire iAP2
//! authentication plane — 0xAA00 RequestAuthenticationCertificate, 0xAA02
//! RequestAuthenticationChallengeResponse, 0xAA04 AuthenticationFailed, 0xAA05
//! AuthenticationSucceeded; `0x1D0x` = identification (0x1D00 StartIdentification, 0x1D01
//! IdentificationInformation, 0x1D02 IdentificationAccepted, 0x1D03 IdentificationRejected);
//! `0x4300` = CarPlayAvailability. The ordering of `State` mirrors the C's integer state so
//! `state >= Authenticated` reproduces the C's `state >= 3` guard. (This on-wire 0xAA0x plane is
//! distinct from the MFi coprocessor-auth plane in `mfi.rs`, which fetches the cert / signs the
//! challenge that we then emit as 0xAA01 / 0xAA03.)
//!
//! NOTE ON 0x4300/0x4301. `0x4300` (CarPlayAvailability, device-source) is only ever received when
//! we declared wired in IdentifyInfo (which we do NOT — see message.rs). We deliberately reply to it
//! with nothing but a log (`Action::Note`): sending the `0x4301 CarPlayStartSession` (accessory-
//! source) the iPhone would then wait for is a proven dead end (the reference resolves the endpoint
//! but never connects to :5000). The working session-start is the mDNS path (rx_connect), not a
//! wired 0x4301 reply.

/// Authentication/identification progress. Ordering mirrors the C's integer `state` (0..=5) so
/// `state >= Authenticated` reproduces the `state>=3` guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    Init,          // 0
    CertSent,      // 1 — 0xAA01 sent
    SignSent,      // 2 — 0xAA03 sent
    Authenticated, // 3 — 0xAA05 AuthSuccess
    IdentSent,     // 4 — 0x1D01 sent
    // 5 — 0x1D01 RE-sent after a first 0x1D03 IdentificationRejected, with the rejected params
    // stripped (Apple's parameter-strip retry, §1.4 of the behavioral reference). Ordered between
    // IdentSent and Identified so it still satisfies the `s >= IdentSent` guard on 0x1D02
    // IdentificationAccepted (a retry can be accepted) while a SECOND 0x1D03 in THIS state is what
    // distinguishes "retry also rejected → hard-fail" from the first, retryable reject.
    IdentRetried,
    Identified, // 6 — 0x1D02 IdentifyAccept
}

/// What the driver must do for a received control message.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Fetch the MFi cert (auth op 0x01), wrap via `group_one(0,·)`, send as `0xAA01`.
    SendCert,
    /// Sign this challenge (auth op 0x02), wrap, send as `0xAA03`.
    SignChallenge(Vec<u8>),
    /// Build IdentifyInformation, send as `0x1D01`.
    SendIdentify,
    /// `0x1D03 IdentificationRejected` parameter-strip retry (Apple reference behavior, §1.4). The
    /// device rejected the identity declaration and named the offending TOP-LEVEL param ids (this
    /// `Vec<u16>`, decoded via `message::parse_rejected_param_ids`).
    ///
    /// DRIVER INTEGRATION (driver.rs / bt_driver.rs — done by the coordinator, NOT here): rebuild the
    /// identity body with those params stripped and re-send it ONCE as a fresh `0x1D01`, e.g.
    /// ```ignore
    /// Action::RetryIdentify(excluded) => {
    ///     let ib = message::build_ident_info_excluding(
    ///         "CarLink",
    ///         message::TransportComponent::Usb { cp_iface: 1 },
    ///         declare_wired,
    ///         &excluded,
    ///     );
    ///     if !write_ep2(ep2, &link.build_msg(1, spec::MSG_IDENTIFICATION_INFORMATION, &ib)) {
    ///         return ExecResult::Abort;
    ///     }
    ///     ExecResult::Commit // state → State::IdentRetried
    /// }
    /// ```
    /// The state machine has already transitioned to `State::IdentRetried`, so a SECOND `0x1D03`
    /// yields `Action::Abort` (teardown) — the retry is attempted at most once.
    RetryIdentify(Vec<u16>),
    /// A milestone with no protocol reply (AuthSuccess / IdentifyAccept / CarPlayAvailability).
    Note(&'static str),
    /// Authentication failed — tear down.
    Abort,
    /// Nothing to do.
    Ignore,
    /// `0x5001 NowPlayingUpdate` received and parsed — see `docs/carplay/05_METADATA_AND_CONTROLS.md`.
    NowPlaying(carplay_metadata::NowPlaying),
    /// `0x5201 RouteGuidanceUpdate` (trip-level) received and parsed.
    RouteGuidance(carplay_metadata::RouteGuidance),
    /// `0x5202 RouteGuidanceManeuverUpdate` (turn-level) received and parsed.
    Maneuver(carplay_metadata::Maneuver),
}

/// Advance for a received control message. Returns the (optimistic) next state — the one the C reaches
/// once the action's send succeeds — and the action to perform.
pub fn on_message(state: State, msg_id: u16, payload: &[u8]) -> (State, Action) {
    use crate::spec::{
        MSG_AUTHENTICATION_FAILED, MSG_AUTHENTICATION_SUCCEEDED, MSG_CAR_PLAY_AVAILABILITY,
        MSG_IDENTIFICATION_ACCEPTED, MSG_IDENTIFICATION_REJECTED, MSG_NOW_PLAYING_UPDATE,
        MSG_REQUEST_AUTHENTICATION_CERTIFICATE, MSG_REQUEST_AUTHENTICATION_CHALLENGE_RESPONSE,
        MSG_ROUTE_GUIDANCE_MANEUVER_INFORMATION, MSG_ROUTE_GUIDANCE_UPDATE, MSG_START_IDENTIFICATION,
    };
    match (msg_id, state) {
        (MSG_REQUEST_AUTHENTICATION_CERTIFICATE, State::Init) => (State::CertSent, Action::SendCert), // 0xAA00
        (MSG_REQUEST_AUTHENTICATION_CHALLENGE_RESPONSE, State::CertSent) => {
            // 0xAA02. An empty extraction means the payload was malformed/short — signing an empty
            // challenge would send a doomed 0xAA03, so hold CertSent and let the phone re-request.
            let challenge = extract_challenge(payload);
            if challenge.is_empty() {
                (state, Action::Ignore)
            } else {
                (State::SignSent, Action::SignChallenge(challenge))
            }
        }
        // Guarded both ways (audit finding: this previously matched any state via `_`, so a
        // premature/spurious AuthSuccess before we had even sent the signature could jump straight
        // to Authenticated). `s >= SignSent` requires the cert/sign steps to have actually
        // happened; `.max(Authenticated)` still makes a legitimate duplicate/idempotent AuthSuccess
        // in a later state a no-op rather than a regression (unit-tested below).
        (MSG_AUTHENTICATION_SUCCEEDED, s) if s >= State::SignSent => {
            // 0xAA05
            (s.max(State::Authenticated), Action::Note("AuthSuccess"))
        }
        (MSG_AUTHENTICATION_FAILED, _) => (state, Action::Abort), // 0xAA04
        // Bounded on BOTH sides now (audit finding: the original guard had no upper bound, so a
        // late/duplicate 0x1D00 arriving after we are already Identified would regress the tracked
        // state back to IdentSent, even though IdentifyInformation was already sent and accepted).
        // Upper bound is `IdentSent`, NOT `Identified`. `IdentRetried` sits between the two, so the
        // wider guard let a `0x1D00` arriving after a reject send a FULL identify and reset the
        // retry ceiling — `0x1D00 -> 0x1D03 -> 0x1D00 -> 0x1D03 …` never reaching `Abort`.
        // Demonstrated by execution: six rounds without aborting, where the same sequence WITHOUT
        // the interleaved `0x1D00` correctly aborts on the second reject. Bounded on Bluetooth by
        // the 120 s handshake budget; UNBOUNDED on the tunnel, which has no periodic budget check.
        (MSG_START_IDENTIFICATION, s) if s >= State::Authenticated && s < State::IdentSent => {
            // 0x1D00
            (State::IdentSent, Action::SendIdentify)
        }
        // Guarded (audit finding: this previously matched any state via `_`, so a premature/
        // spurious IdentifyAccept before we had sent IdentifyInformation could jump straight to
        // Identified without SendIdentify ever having fired).
        (MSG_IDENTIFICATION_ACCEPTED, s) if s >= State::IdentSent => {
            // 0x1D02
            (State::Identified, Action::Note("IdentifyAccept"))
        }
        // 0x1D03 IdentificationRejected — Apple's PARAMETER-STRIP RETRY (§1.4 of the behavioral
        // reference, recovered from CarPlaySimulator: `handleIdentificationRejected`,
        // `iAPIdentificationRejectedWillRetry`, `Retrying without problematic parameters`,
        // `shouldRetry`, `Identification retry failed, rejecting`). FIRST reject while awaiting the
        // identify verdict (IdentSent): decode the rejected top-level param ids, ask the driver to
        // re-send 0x1D01 with them stripped, and move to IdentRetried so a re-reject is
        // distinguishable. In production our first declaration is accepted (0x1D02), so this path is
        // the graceful fallback, not the happy path.
        (MSG_IDENTIFICATION_REJECTED, State::IdentSent) => {
            // 0x1D03, first reject
            let ids = crate::message::parse_rejected_param_ids(strip_header(payload));
            (State::IdentRetried, Action::RetryIdentify(ids))
        }
        // SECOND 0x1D03 after we already retried once → give up (Apple's `Identification retry
        // failed, rejecting`). Hard-fail / teardown.
        (MSG_IDENTIFICATION_REJECTED, State::IdentRetried) => (state, Action::Abort),
        // A 0x1D03 in any OTHER state (before we ever sent identify, or after we're already
        // Identified) is not part of a retry sequence — fall through to Ignore below, preserving the
        // pre-existing "reject outside the identify window is a no-op" behavior.
        (MSG_CAR_PLAY_AVAILABILITY, _) => (state, Action::Note("CarPlayAvailability")), // 0x4300
        // Now Playing / Route Guidance metadata only flows once we're actually Identified and have
        // subscribed (see driver.rs's IdentifyAccept handling) -- guarding here is defensive (a
        // pre-Identify 0x5001 shouldn't be possible per the protocol) rather than load-bearing.
        // Pure additions: no existing arm above is touched.
        (MSG_NOW_PLAYING_UPDATE, s) if s >= State::Identified => {
            // 0x5001
            (state, Action::NowPlaying(carplay_metadata::tlv::parse_now_playing(strip_header(payload))))
        }
        (MSG_ROUTE_GUIDANCE_UPDATE, s) if s >= State::Identified => {
            // 0x5201
            (state, Action::RouteGuidance(carplay_metadata::tlv::parse_route_guidance(strip_header(payload))))
        }
        (MSG_ROUTE_GUIDANCE_MANEUVER_INFORMATION, s) if s >= State::Identified => {
            // 0x5202
            (state, Action::Maneuver(carplay_metadata::tlv::parse_route_guidance_maneuver(strip_header(payload))))
        }
        _ => (state, Action::Ignore),
    }
}

/// Strip the `[40 40][total][msg_id]` 6-byte link-message header, leaving just the TLV body --
/// the same convention `extract_challenge` below already uses for `0xAA02`.
fn strip_header(payload: &[u8]) -> &[u8] {
    payload.get(6..).unwrap_or(&[])
}

/// From a `0xAA02` payload, extract the challenge. Payload is `[40 40][total][msg_id][body]`; the body
/// (at offset 6) is a group `[len BE16][pid BE16][challenge]`, challenge length = `len - 4`.
fn extract_challenge(payload: &[u8]) -> Vec<u8> {
    if payload.len() < 6 + 4 {
        return Vec::new();
    }
    let body = &payload[6..];
    let group_len = ((body[0] as usize) << 8) | body[1] as usize;
    let chlen = group_len.saturating_sub(4);
    body.get(4..4 + chlen).map(<[u8]>::to_vec).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_handshake_flow() {
        let mut s = State::Init;
        let a;
        (s, a) = on_message(s, 0xAA00, &[]);
        assert_eq!((s, a), (State::CertSent, Action::SendCert));

        // 0xAA02 body: [40 40][total][AA 02][group_len=0x0008][pid 0000][chal 4B]
        let payload = [0x40, 0x40, 0x00, 0x0E, 0xAA, 0x02, 0x00, 0x08, 0x00, 0x00, 1, 2, 3, 4];
        let (s2, a2) = on_message(s, 0xAA02, &payload);
        assert_eq!(s2, State::SignSent);
        assert_eq!(a2, Action::SignChallenge(vec![1, 2, 3, 4]));

        let (s3, a3) = on_message(s2, 0xAA05, &[]);
        assert_eq!((s3, a3), (State::Authenticated, Action::Note("AuthSuccess")));

        let (s4, a4) = on_message(s3, 0x1D00, &[]);
        assert_eq!((s4, a4), (State::IdentSent, Action::SendIdentify));

        let (s5, a5) = on_message(s4, 0x1D02, &[]);
        assert_eq!((s5, a5), (State::Identified, Action::Note("IdentifyAccept")));
    }

    #[test]
    fn malformed_challenge_request_holds_cert_sent() {
        // A 0xAA02 too short to carry a challenge (extract_challenge -> empty) must NOT advance to
        // SignSent — signing an empty challenge sends a doomed 0xAA03. Hold CertSent so the phone's
        // re-request still matches the (0xAA02, CertSent) arm.
        for short in [&[][..], &[0x40, 0x40, 0x00, 0x06, 0xAA, 0x02][..]] {
            assert_eq!(
                on_message(State::CertSent, 0xAA02, short),
                (State::CertSent, Action::Ignore)
            );
        }
    }

    #[test]
    fn identify_requires_authentication_first() {
        // 0x1D00 before AuthSuccess is ignored (the C's `state>=3` guard).
        assert_eq!(on_message(State::Init, 0x1D00, &[]), (State::Init, Action::Ignore));
        assert_eq!(on_message(State::CertSent, 0x1D00, &[]), (State::CertSent, Action::Ignore));
    }

    #[test]
    fn auth_failed_aborts() {
        assert_eq!(on_message(State::CertSent, 0xAA04, &[]).1, Action::Abort);
    }

    #[test]
    fn auth_success_never_regresses_state() {
        // Late/duplicate 0xAA05 must not drop an already-Identified session back to Authenticated.
        assert_eq!(on_message(State::Identified, 0xAA05, &[]).0, State::Identified);
    }

    #[test]
    fn premature_auth_success_is_ignored() {
        // 0xAA05 before the cert/sign steps happened (Init, CertSent) must not jump to
        // Authenticated -- it must not have been signed yet if we never sent the challenge sig.
        assert_eq!(on_message(State::Init, 0xAA05, &[]), (State::Init, Action::Ignore));
        assert_eq!(on_message(State::CertSent, 0xAA05, &[]), (State::CertSent, Action::Ignore));
    }

    #[test]
    fn premature_identify_accept_is_ignored() {
        // 0x1D02 before we ever sent IdentifyInformation (0x1D01, which happens on the transition
        // to IdentSent) must not jump straight to Identified.
        assert_eq!(on_message(State::Init, 0x1D02, &[]), (State::Init, Action::Ignore));
        assert_eq!(on_message(State::Authenticated, 0x1D02, &[]), (State::Authenticated, Action::Ignore));
    }

    #[test]
    fn late_duplicate_identify_info_request_does_not_regress_identified() {
        // A late/duplicate 0x1D00 after we are already Identified must not regress the tracked
        // state back to IdentSent -- mirrors auth_success_never_regresses_state but for 0x1D00.
        assert_eq!(on_message(State::Identified, 0x1D00, &[]), (State::Identified, Action::Ignore));
    }

    #[test]
    fn unknown_message_is_ignored() {
        assert_eq!(on_message(State::Identified, 0x4E0E, &[]), (State::Identified, Action::Ignore));
    }

    /// Wrap a TLV body in the `[40 40][total][msg_id]` 6-byte header `strip_header` expects,
    /// mirroring how `full_handshake_flow`'s own 0xAA02 payload is constructed above.
    fn framed(msg_id: u16, body: &[u8]) -> Vec<u8> {
        let mut p = vec![0x40, 0x40, 0x00, 0x00, (msg_id >> 8) as u8, (msg_id & 0xff) as u8];
        p.extend_from_slice(body);
        p
    }

    #[test]
    fn now_playing_update_parses_once_identified() {
        // group(0x0000, [group(0x01, "Hi\0")]) -- a minimal MediaItemAttributes/Title group.
        let body = [0x00, 0x0B, 0x00, 0x00, 0x00, 0x07, 0x00, 0x01, b'H', b'i', 0x00];
        let (next, action) = on_message(State::Identified, 0x5001, &framed(0x5001, &body));
        assert_eq!(next, State::Identified, "metadata never changes auth/identify state");
        match action {
            Action::NowPlaying(np) => assert_eq!(np.title.as_deref(), Some("Hi")),
            other => panic!("expected Action::NowPlaying, got {other:?}"),
        }
    }

    #[test]
    fn route_guidance_and_maneuver_updates_parse_once_identified() {
        let (_, action) = on_message(State::Identified, 0x5201, &framed(0x5201, &[]));
        assert!(matches!(action, Action::RouteGuidance(_)));
        let (_, action) = on_message(State::Identified, 0x5202, &framed(0x5202, &[]));
        assert!(matches!(action, Action::Maneuver(_)));
    }

    #[test]
    fn metadata_updates_before_identified_are_ignored() {
        // Defensive guard, not load-bearing per the protocol -- but must not panic or misdispatch.
        assert_eq!(
            on_message(State::Authenticated, 0x5001, &framed(0x5001, &[])),
            (State::Authenticated, Action::Ignore)
        );
    }

    #[test]
    fn first_reject_retries_with_stripped_param_ids() {
        // 0x1D03 body rejects param 30 (RouteGuidanceDisplayComponent) then param 24
        // (WirelessCarPlayTransportComponent): two present top-level `none` groups.
        let mut body = Vec::new();
        body.extend_from_slice(&[0x00, 0x04, 0x00, 0x1E]); // none(30)
        body.extend_from_slice(&[0x00, 0x04, 0x00, 0x18]); // none(24)
        let (next, action) = on_message(State::IdentSent, 0x1D03, &framed(0x1D03, &body));
        assert_eq!(next, State::IdentRetried, "first reject moves to IdentRetried");
        assert_eq!(action, Action::RetryIdentify(vec![30, 24]));
    }

    /// A `0x1D00` arriving after a reject must NOT reset the retry ceiling. The guard used to be
    /// `s < Identified`, and `IdentRetried` sits below that — so a phone alternating
    /// `0x1D00`/`0x1D03` kept us re-sending a full identify forever. Unbounded on the tunnel, which
    /// has no periodic budget check.
    #[test]
    fn interleaved_start_identification_cannot_reset_the_retry_ceiling() {
        let body = [0x00, 0x04, 0x00, 0x1E]; // none(30)
        let (st, a) = on_message(State::Authenticated, 0x1D00, &[]);
        assert_eq!(a, Action::SendIdentify);
        assert_eq!(st, State::IdentSent);

        let (st, _) = on_message(st, 0x1D03, &framed(0x1D03, &body));
        assert_eq!(st, State::IdentRetried);

        // The phone re-asks. We must NOT go back to IdentSent.
        let (next, a) = on_message(st, 0x1D00, &[]);
        assert_eq!(
            a,
            Action::Ignore,
            "0x1D00 in IdentRetried reset the ceiling — identify could loop forever"
        );
        assert_eq!(next, State::IdentRetried);

        // And the next reject still aborts, as designed.
        let (_, a) = on_message(st, 0x1D03, &framed(0x1D03, &body));
        assert_eq!(a, Action::Abort, "second reject must abort");
    }

    #[test]
    fn second_reject_after_retry_aborts() {
        // Already retried once (IdentRetried) → a second 0x1D03 hard-fails.
        let body = [0x00, 0x04, 0x00, 0x1E]; // none(30)
        let (next, action) = on_message(State::IdentRetried, 0x1D03, &framed(0x1D03, &body));
        assert_eq!(next, State::IdentRetried, "abort does not advance state");
        assert_eq!(action, Action::Abort);
    }

    #[test]
    fn reject_in_wrong_state_is_ignored() {
        // Before we ever sent identify, or after we're already Identified, a 0x1D03 is not part of a
        // retry sequence and must be ignored (no RetryIdentify, no Abort).
        for s in [State::Init, State::CertSent, State::Authenticated, State::Identified] {
            assert_eq!(
                on_message(s, 0x1D03, &framed(0x1D03, &[0x00, 0x04, 0x00, 0x18])),
                (s, Action::Ignore),
                "0x1D03 in {s:?} must be ignored"
            );
        }
    }

    #[test]
    fn retry_can_still_be_accepted() {
        // A retry that the device accepts: 0x1D02 IdentificationAccepted while in IdentRetried must
        // advance to Identified (the `s >= IdentSent` guard covers IdentRetried).
        let (next, action) = on_message(State::IdentRetried, 0x1D02, &[]);
        assert_eq!((next, action), (State::Identified, Action::Note("IdentifyAccept")));
    }

    #[test]
    fn reject_with_empty_body_retries_with_no_exclusions() {
        // A 0x1D03 whose body has no parsable params still retries once (Apple retries on the reject,
        // even if it cannot name a param) — RetryIdentify with an empty id list.
        let (next, action) = on_message(State::IdentSent, 0x1D03, &framed(0x1D03, &[]));
        assert_eq!(next, State::IdentRetried);
        assert_eq!(action, Action::RetryIdentify(vec![]));
    }
}
