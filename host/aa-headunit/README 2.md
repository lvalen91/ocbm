# aa-headunit

Minimal Android Auto **head-unit client** for bench-testing the AA projection protocol
against a phone's own developer head-unit server — no Carlinkit box in the loop. This is
the Phase 0 / Phase 1 harness for the AA-over-OCBM workstream (see
[`../../docs/androidauto/00_ARCHITECTURE.md`](../../docs/androidauto/00_ARCHITECTURE.md)).

It connects over TCP (default `127.0.0.1:5277` via `adb forward`), performs the AA
handshake as the head unit (TLS 1.2, encapsulated in AA control frames), advertises the
service set gearhead requires, and captures the H.264 video stream to an Annex-B file.

Interop/accessory development: the head-unit certificate is presented only to the device
owner's own phone during the owner's own session.

## Prerequisites (not committed — obtain locally)

Two things are required to build and are deliberately **not** in the repo:

1. **`certs/headunit.crt` and `certs/headunit.key`** — the Google Automotive Link (GAL)
   head-unit certificate + key. This is the public, well-known credential shared across
   the open-source AA head-unit ecosystem (aasdk `cert/`, openauto, etc.); it is
   `include_str!`'d at build time. `certs/` is gitignored (see the cert-handling note in
   docs/androidauto/00_ARCHITECTURE.md). Copy `headunit.crt`/`headunit.key` from an `aasdk` checkout's `cert/`
   directory into `certs/`. Do not commit them.

2. **OpenSSL headers/libs** — on macOS the build needs Homebrew's OpenSSL:

   ```sh
   export OPENSSL_DIR=$(brew --prefix openssl@3)
   ```

## Build

```sh
export OPENSSL_DIR=$(brew --prefix openssl@3)
cargo build --release
```

The crate carries an empty `[workspace]` table so it builds standalone, outside the parent
`ccpa_custom` workspace.

## Run

On the phone: Android Auto → tap the version ~10× to enable developer mode →
⋮ → Developer settings → **Start head unit server** (it listens on TCP 5277 and stops
when idle).

```sh
adb forward tcp:5277 tcp:5277
./target/release/aa-headunit 127.0.0.1:5277 /tmp/aa_capture.h264 120
```

Or use the runner, which waits for the server then captures:

```sh
./run_capture.sh            # -> /tmp/aa_capture.h264, log /tmp/aa_phase1.log
```

Arguments: `aa-headunit [addr] [out.h264] [max_frames]`. The captured file is Annex-B
H.264 — play with `ffplay out.h264` or transcode with `ffmpeg -i out.h264 out.mp4`.
