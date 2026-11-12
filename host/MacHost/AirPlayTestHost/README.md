# AirPlay Test Host

A small macOS app that puts the box into AirPlay-only mode (OCBM `MODE_AIRPLAY`) over USB and plays what
comes back: it decrypts, decodes and shows the tunnelled screen stream, plays the audio streams, and shows
the mode's state and counters. The wire: `docs/ocbm/00_PROTOCOL.md` (AirPlay-only mode); the measured
AirPlay protocol: `docs/ocbm/02_AIRPLAY_ONLY_MODE.md`; the reference CLI host: `ocbm-host airplay`.

One window: SSID, passphrase, receiver name, channel, display (1920x1080@30, 3840x2160@60, 3840x2160@120;
`AIRPLAY_REQ_DISPLAY`), "Allow HEVC" (`AIRPLAY_REQ_FLAGS` bit 0 = feature bit 42; off by default, and when
off the TLV is not sent); Start / Stop; "Smooth playback"; status; log. The passphrase is held in memory
while a session runs and is never saved or logged (SSID, name, channel, display and HEVC are remembered).

## Build and test

    xcodebuild -project AirPlayTestHost.xcodeproj -scheme AirPlayTestHost -derivedDataPath /tmp/airplay-test-host-dd build
    tests/run_tests.sh        # hardware-free; the media tests need ffmpeg

**Signing:** no Team ID is committed (`DEVELOPMENT_TEAM` is empty in `project.yml` and the projects). Pass it
on the command line (`xcodebuild ... DEVELOPMENT_TEAM=<your team id>`) or set it in Xcode's signing settings.

`AirPlayTestHost.xcodeproj` is generated from `project.yml` (`xcodegen` in this directory). It is a
separate project: `carlink_macOS.xcodeproj` is not touched.

**Opening the box is exclusive.** The app takes the OCBM interface when Start is pressed and seizes it from
any other client (`ocbm-host`, the carlink app); it releases it when the leave completes. Before Start it
only watches for the device.

## Layout

| Path | What |
|---|---|
| `Sources/Core/` | Pure: request and box answers (`AirPlayModeWire`), lane reassembly (`SeamReader`), keys and decrypt (`AirPlayTunnel`), the opcode-1 codec record (`MirrorVideoConfig`: avcC, or hvc1 box with hvcC), rates / jitter / smooth-playback schedule (`VideoTiming`), events (`AirPlayEvents`), the CH_CTRL state machine (`AirPlayModeSession`) and its pipe wrapper (`AirPlayModeClient`), PCM playout policy (`AudioPlayoutQueue`) |
| `Sources/Media/` | `MirrorVideoDecoder` (VTDecompressionSession, H.264 and HEVC), `FramePresenter` (show on decode, or paced), `AirPlayAudioPlayer` (AAC to AVAudioSourceNode), `MediaPipeline` |
| `Sources/Session/` | `SessionController` and the status models; no views |
| `Sources/App/` | SwiftUI window, `USBBoxLink`, `VideoSurface` |
| `Sources/Debug/` | Debug builds only: `TunnelFixture` (the sending side of the tunnel), `SyntheticBox`, `SyntheticLink` |
| `tests/` | `run_tests.sh` + `main.swift` |

Shared, unchanged, from `../carlink_macOS/`: `OCBM/OCBMFraming.swift` (constants, framing, reassembler,
box log entries), `OCBM/OCBMAVDecrypt.swift` (the two ChaCha20-Poly1305 open functions),
`Video/AVCCFastPath.swift` (avcC parse, NAL length rewrite), `Audio/AudioPlayer.swift`
(`CompressedAudioDecoder`), `USB/` (device watch, interface claim, bulk transport), and
`OCBM/OCBMClient.swift` for the `RawBulkTransport` protocol. The remaining shared files in `project.yml`
are only what those need to compile.

Not shared, and why: `OCBMClient` sends `CT_SUBSCRIBE`, which makes the box leave this mode;
`VideoDecoder` sheds frames and waits for a requested keyframe, and AirPlay mirroring sends one IDR per
session with no keyframe request; `AudioPlayer` drops audio beyond a short backlog, and type 103 arrives
minutes ahead of real time.

## Behaviour worth knowing

- **Codec:** taken from the opcode-1 body of each stream: a bare avcC is H.264; an `hvc1` box is searched
  for its `hvcC` (VPS/SPS/PPS) and decoded as HEVC. Measured: the phone sends HEVC only when HEVC is
  allowed AND the display is above 1080p. A stream whose codec differs from the last one gets a new
  decoder (logged as "codec changed"); a config that cannot be used leaves no decoder, and its frames are
  counted under "Before a config". The status shows the codec, profile/level, and whether VideoToolbox
  gave a hardware or a software decoder; a Mac where no HEVC decoder can be created says so in red.
- **Frame rate:** decoded pictures over a 2 s moving window, the stream's average, the rate by the phone's
  header timestamps, and the age of the last picture. iOS encodes on screen change, so a still screen
  is a real 0 after 2 s (the old per-poll rate read 0 after half a second).
- **Arrival jitter:** RFC 3550 interarrival jitter of frame arrival (after decrypt) against the 32.32
  header timestamps, plus the peak in the last 2 s.
- **Smooth playback** (off by default, switchable while playing, not remembered): pictures are held to
  their timestamp, re-based on the fastest arrival of the last 4 s, plus 100 ms (the phone's
  `latencyMs`). A picture past its slot is shown at once and counted late; a stream end or a new key
  drops what is held.
- **Lost video frame:** reported (seq jump, frames lost) and decoding continues; the picture can stay
  damaged until the stream ends. Measured with the synthetic box: VideoToolbox rejected no later frame.
- **Audio:** one stream plays at a time, the one with the newest key that has sent a packet. Type 96 starts
  after a 120 ms cushion and is trimmed if more than 1 s queues up. Type 103 waits compressed and is decoded
  about 1.5 s ahead; `anchor` rate 0/1 pauses and resumes it, `flush` drops by sequence number. The phone's
  `volume` sets the output gain.
- **No A/V sync and no playout clock** tied to the phone's (open in `02_AIRPLAY_ONLY_MODE.md` §3): pictures
  are shown when decoded (or paced on their own timestamps, above), audio runs on the Mac's output clock.
- **Box unplugged while running:** the app waits, opens the box again when it returns and requests the
  mode again with the same values. `SEV_HOST_GONE` while the mode is held (a restarted ocbmd) also
  re-requests. `CT_AIRPLAY_STATE 00 00 00` (the box left the mode) ends the run.

## Synthetic run (debug builds, no USB)

Every switch is a `-key value` pair (AppKit takes a lone argument for a file to open, and then shows no
window):

    open -n -a "/tmp/airplay-test-host-dd/Build/Products/Debug/AirPlay Test Host.app" \
      --env OCBM_AIRPLAY_PSK=<throwaway> --args \
      -synthetic-h264 FILE.h264 -synthetic-aac FILE.aac -ssid "Bench AP" -autostart YES \
      -status-json /tmp/status.json -output-gain 0.02

`FILE.h264` = Annex-B H.264, one slice per frame, no B-frames, one IDR; `FILE.aac` = ADTS AAC-LC (the
commands are in `tests/main.swift`, `makeMedia`). The synthetic box answers the control plane as ocbmd
does and loops a mirroring session (type 110, a type 96 stream that is torn down and comes back with a new
key) and a music session (type 103 ahead of real time, track, artwork, pause, flush). Further switches:
`-synthetic-drop-frame N`, `-synthetic-unplug-after S`, `-name`, `-channel`, `-display WxH@F`,
`-hevc YES`, `-smooth YES`. `-synthetic-hevc FILE.hevc` (Annex-B HEVC, one IDR, no B-frames; coded size
`-synthetic-hevc-size WxH`, default 994x2160) is sent instead of the H.264 when the request allows HEVC at a
canvas above 1080p, as the phone does; `-synthetic-alternate YES` sends H.264 and HEVC sessions in turn.
With `-synthetic-*` given, the USB code is never reached, even when the files cannot be read.
