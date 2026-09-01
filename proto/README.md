OCBM protocol definitions (spec in `docs/carplay/01_OCBM_PROTOCOL.md`). The shipped wire codec lives in
`crates/ocbm-proto/` — envelope/header, channel constants, the `Reassembler`,
`frame_into`/`write_header`, and CRC-32 — proven on hardware.
