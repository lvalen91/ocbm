//! rtsp — the AirPlay/CarPlay receiver control plane (clean-room from `AirPlayReceiverServer.c`).
//!
//! Phase 4 (this): the reusable, socket-independent substrate —
//! - [`message`]: the RTSP/1.0 + HTTP/1.1 request/response codec (CSeq, Content-Length body).
//! - [`route`]: the method/path → [`route::Route`] dispatch table.
//! - [`control`]: post-pair-verify control-channel keys (`Control-Salt` / `Control-*-Encryption-Key`)
//!   and the ChaCha20-Poly1305 `[len][ct][tag]` frame codec.
//!
//! The endpoint *handlers* (`/info` plist, SETUP/RECORD against the session, Bonjour) are wired in a
//! later phase where the receiver session + config exist; the auth endpoints call the `pairing` /
//! `mfi` crates already built.

#![forbid(unsafe_code)]

pub mod control;
pub mod message;
pub mod route;
