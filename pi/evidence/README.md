# Captured device state — 2026-08-16, immediately before shutdown

Everything here lived only on the running Pi. `/tmp` is tmpfs, so the box log and the generated
`VehicleConfig` do not survive a power cycle; they are captured rather than regenerable.

| File | What it is | Why it matters |
|---|---|---|
| `device_state_2026-08-16.txt` | Kernel version and config, SELinux labels, video4linux nodes, registered HEVC decoders, codec2 properties, audio cards, car-audio mode, occupant zones, process and socket state | The baseline for any future image comparison. `CONFIG_BT_RFCOMM is not set` and `Run in legacy mode? true` are both here. |
| `carplay_cfg.generated.yaml` | The `VehicleConfig` the projection app generated from the live display | The exact document the box parsed. Base64 icon payloads elided (~40 KB). |
| `stack_launch_env.txt` | The accessory stack's launch environment, read from `/proc/<pid>/environ` | The authoritative copy. `start_stack.sh` should match it. |
| `airplayd_key_lines.log` | The load-bearing lines from the box log | Contains `ASC f8f03000` (the fixed ELD config) and the SETUP dicts iOS sent. |
| `app_session_log.txt` | The app's own log tail | Session establishment through to steady state. |
| `hostapd_5g.conf` | The 5 GHz AP configuration in use | Framework SoftAP is blocked by the Wi-Fi HAL, so this is how the AP is actually run. |

## Two things worth reading directly

**The ELD fix, visible on the wire.** `airplayd_key_lines.log` shows `ASC f8f03000` — the 4-byte
plain-ELD configuration iOS negotiates. Before the fix it was the 7-byte `f8f0312c00bc00` (LD-SBR),
which iOS discarded silently, and Siri heard nothing while every counter read healthy. Access units
also grew from 87-97 B to 179 B, against Apple's 180 B budget for this stream.

**Car audio is in legacy mode.** `device_state_2026-08-16.txt` records
`CarAudioService: Run in legacy mode? true` and `Configured using audio control? false`, so
`car_audio_configuration.xml` is not parsed and there are no volume groups. That is the most likely
cause of the observed volume behaviour, and it is an image-level fix.
