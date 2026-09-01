# AAOS image corrections found during the CarPlay port

**These are `/vendor` build-config and HAL changes.** None can be fixed from userspace with root;
each needs an AAOS image rebuild, which currently needs an x86_64 Linux environment this project
does not have. Recorded here so a future rebuild can bake them in.

**If you have a build environment in front of you, read `pi/docs/04_IMAGE_REBUILD_GUIDE.md`
instead.** This document is organised by what was *observed on the device*; `04` is the same
material organised by *where the change goes in the AOSP tree*, plus the two largest items that are
not defects at all — the kernel and Wi-Fi HAL changes that would delete code this port had to write.

Companion to `/Volumes/stuff/rpi/aaos/docs/os-corrections-2026-08-16.md`, which holds the earlier
round (UART overlay, Wi-Fi country code, SoftAP HAL, `CONFIG_BT_RFCOMM`). **That volume was
unmounted when this was written**; fold these in when it is next available.

Platform: `RaspberryVanillaAOSP16-20260413-rpi4_car.img`, kernel `6.12.73-g8af794a959ec-v8`,
Raspberry Pi 4 Model B Rev 1.5.

---

## 1. ~~Microphone input is synthesised~~ — RETRACTED, the microphone works

**This finding was WRONG and is kept, struck through, because it was acted on.**

The original claim: `ro.boot.audio.tinyalsa.simulate_input=true` in `/vendor/build.prop`, only
`AUDIO_DEVICE_IN_BUILTIN_MIC` in the audio policy, and no USB audio HAL — therefore `AudioRecord`
returns generated silence and the CarPlay mic uplink can never work.

**Measured on device, it does not:**

```
[mic] peak=1786  (-25.3 dBFS) rms=414   — baseline room noise
[mic] peak=16997 (-5.7 dBFS)  rms=801   — speech
[mic] peak=20549 (-4.1 dBFS)  rms=1433  — speech
[mic] peak=11988 (-8.7 dBFS)  rms=1295  — speech
```

That is a real microphone capturing a real voice at healthy level. Whatever `simulate_input` does on
this build, it is not suppressing capture.

**How the wrong conclusion was reached, since the reasoning pattern is the reusable part.** The
properties and the policy XML were read correctly; the inference from them to behaviour was never
tested. It then survived a first test because the detector used at the time only asked "are all
samples exactly zero" — which answered *no* and was reported as inconclusive rather than as
contradicting the hypothesis. Only replacing that with a level measurement settled it. **Config
that looks like it should cause a symptom is not evidence that it did**; the same mistake as reading
HAL symbols as the active audio route (`os-corrections-2026-08-16.md`).

The mic uplink chain is now verified end to end: capture → `airplayd:9112` → AAC-ELD → RTP to the
phone, with the box logging `mic PCM rx` and `sent N packets`.

**The input device is the USB EarPods**, established independently: `/proc/asound/card3/pcm0c/sub0/status`
reads `RUNNING`, `dumpsys media.audio_flinger` reports `Input device: 0x82000000
(AUDIO_DEVICE_IN_USB_HEADSET)`, and card 3 is the **only** capture-capable PCM on the box — the Pi 4
has no built-in mic. So `simulate_input=true` is set and simply ignored.

**Siri's failure was found elsewhere and is fixed** (commit `536dfb8`): the ELD encoder was emitting
AAC-ELD *v2 / LD-SBR*, ASC `f8f0312c00bc00`, which iOS never negotiated and silently discarded.
Nothing to do with capture or with this image.

### 1a. A real defect found alongside it (box-side, not image)

`uplink::clear()` is called only from full session teardown (`session.rs:473`), never on per-stream
teardown. iOS creates and destroys a type-100 `speechRecognition` stream **per Siri turn** — two
turns were observed with different `scid`s and dataPorts — but the box's `UPLINK` state stays armed
across them, and `uplink off` was never sent once in a whole session (`grep -c "uplink disarmed"` =
0).

Consequences: the microphone is held open continuously rather than only during a turn, which defeats
the point of the gate, and RTP keeps being sent to a dataPort iOS has already closed. The
destination does update on the next `arm()`, so this was not why Siri failed — but it is wrong on
its own terms. Confirmed by counting: 424 `armed=true` lines, **0** `armed=false`, across a session
containing a completed Siri round trip.

The app-side half of the same problem (capture not released on any pump exit path, and `stop()`
being undoable by an in-flight arm) is fixed in `f3ee653`.

### 1b. NEW — the ALSA capture stream is overrunning and restarting every ~2 s

Found while verifying the above, and **not** previously known.

`/proc/asound/card3/pcm0c/sub0/status` sampled 30 times shows `trigger_time` advancing every
1.5-3.5 s with `hw_ptr` resetting to near zero each time (e.g. 26352 → 912) — the stream is being
re-prepared repeatedly. And `avail` / `avail_max` reach **3000-4800 frames against a `buffer_size`
of 480**, an 8-10× overrun.

So capture is **lossy**: the HAL is dropping data and restarting roughly every two seconds. The
uplink stays armed and audio does flow, but with periodic gaps. Worth resolving before concluding
anything about Siri recognition quality — a two-second-cadence dropout is exactly the kind of thing
that leaves speech intelligible to a level meter and useless to a recogniser.

Whether this is HAL overrun-recovery or a deliberate reopen was not determined; the owner is the
audio HAL's own `reader` thread either way, and `avail` ≫ `buffer_size` points at overrun. Capture
format is mono S24_3LE 48000 Hz, period 240, buffer 480.

## 2. HEVC decode does a redundant full-frame CPU copy per frame

**Severity: performance.** **~33% of one Cortex-A72 core** for a near-static 1080p stream.

> **Corrected.** This first said 46%. That came from a single `top -H` sample, not an average.
> Independently re-measured over five windows and the process lifetime: 30.9% / 36.4% / 37.3% /
> 31.5%, lifetime 259.9 s CPU over 785.0 s = **33.1%**, at a full unthrottled 1800 MHz. 46% was
> never observed again and was not a transient that was missed. The mechanism below is unaffected;
> only the number was wrong.

Hardware HEVC decode itself is working — this was initially misdiagnosed as software decode and is
not. `c2.ffmpeg.hevc.decoder` drives the Pi's `rpi-hevc-dec` block on `/dev/video19` through
ffmpeg's V4L2 stateless (Request API) hwaccel:

```
HWACCEL: ffmpeg_hwaccel_init: [hevc], hw device = drm
FFMPEG : [hevc] Hwaccel V4L2 HEVC stateless V4; devices: /dev/media0,/dev/video19;
         buffers: src DMABuf, dst DMABuf; swfmt=rpi4_8
```

Verified four ways, the last of which is decisive:

* pid 664 (`android.hardware.media.c2-service-ffmpeg`) holds `/dev/media0` and `/dev/video19` open,
  **plus four `anon_inode:request` fds** — the signature of a stateless V4L2 decoder in active use.
* `/sys/class/video4linux/video19/` → `DRIVER=rpi-hevc-dec`, `OF_COMPATIBLE_0=brcm,bcm2711-hevc-dec`.
* `libavcodec.so` exports `ff_hevc_v4l2request_hwaccel`, `ff_v4l2_request_decode_slice`.
* **The block is firing interrupts.** `/proc/interrupts` line 36 (`feb00000.codec`): 1732 IRQs in
  30 s = 57.7/s, against 900 frames in 32.4 s = 27.8 fps — **2.08 interrupts per frame**, the
  rpivid two-phase-per-frame pattern. An idle block emits zero. Software HEVC decode of 1080p28 on
  an A72 would cost 1.5-2.5 full cores; the whole process uses 0.32. Software decode is
  arithmetically impossible here.

The cost is in the **output path**, which makes two full-frame CPU passes where one would do:

| Pass | What | Necessary |
|---|---|---|
| 1 | SAND (`NV12_COL128`) → planar detile | Yes, today |
| 2 | `sws_scale` yuv420p → **yuv420p** | **No — pure memcpy** |

Corroborated by the user/system split: **utime 18.2% / stime 18.7%**. A true zero-copy hwaccel
passthrough would be almost entirely *system* time; half the cost being userspace is the copy. The
SurfaceView consumer buffers are `YV12` (fourcc 842094169, 3038.56 KiB = 1920×1080×1.5), a
CPU-accessible planar format, and this libavcodec ships the scalar de-tilers
(`av_rpi_sand_to_planar_y8` / `_c8`). Not profiled — no perf on a live session — so the symbol
attribution is inference from the format pairing.

Pass 2 has identical source and destination dimensions *and* pixel format, so swscale takes its
unscaled copy path: a row-by-row memcpy of a full 1080p frame (~187 MB/s at 30 fps), whose only
purpose is moving data from ffmpeg's own `AVFrame` into a gralloc block fetched *after* the transfer
already happened.

**Not fixable by configuration** — the only properties the HAL reads are
`persist.vendor.ffmpeg_codec2.rank[.audio|.video]`, `.v4l2.h264`, `.v4l2.h265` and
`debug.ffmpeg.loglevel`.

**Needed:** a ~40-line change in
[`android_external_ffmpeg_codec2`](https://github.com/raspberry-vanilla/android_external_ffmpeg_codec2)
`C2FFMPEGVideoDecodeComponent::outputFrame()` — fetch and map the graphic block *first*, point the
hwaccel transfer's `data[]`/`linesize[]` at the `C2GraphicView` planes (`av_hwframe_transfer_data`
only allocates when `dst->buf[0]` is NULL), and delete the swscale path. Expect roughly half the CPU
back. Switching the output surface from `HAL_PIXEL_FORMAT_YV12` to NV12 stacks on top and is cheaper
still — upstream already does that for HEVC Main 10.

### 2a. A red herring to ignore

```
CCodecBufferChannel: Query output surface allocator returned 0 params => BAD_INDEX (6)
```

This is **benign** and was initially misread as a fallback to a non-surface pool. In
`CCodecBufferChannel.cpp` it is the *designed trigger* for the fallback **to** `BUFFERQUEUE`
(allocatorID 18), which is a surface-backed gralloc pool. Every component that does not override the
allocator logs it, including the stock software codecs. Nothing to fix.

---

## 3. `/dev/media0` has the wrong SELinux label — latent, breaks HEVC under enforcing

```
/dev/video19  u:object_r:video_device:s0     # correct
/dev/media0   u:object_r:device:s0           # generic fallback — WRONG
```

The board currently boots `androidboot.selinux=permissive`, so this is inert today. Under enforcing,
the media node becomes unreachable, and because the rpivid decoder is a **stateless** Request-API
device that needs `/dev/media0` for control submission, **hardware HEVC decode stops entirely** —
falling back to a software decoder at a very different CPU cost, or failing outright.

**Needed:** a `media_device`-style type in the device's `file_contexts`, plus the matching allow
rules for the codec2 HAL domain. Worth fixing *before* anyone tries switching to enforcing, because
the failure will look like a codec regression rather than a policy one.

---

## 4. No hardware HEVC entry in the V4L2 codec2 XML — correct as-is, do not "fix"

Recorded because it looks like a bug and is not.

`/vendor/etc/media_codecs_v4l2_c2_video.xml` declares only `c2.v4l2.avc.decoder` and
`c2.v4l2.avc.encoder` — no HEVC. Upstream removed HEVC deliberately
([commit `846ea6f78`](https://github.com/raspberry-vanilla/android_external_v4l2_codec2/commit/846ea6f78),
"V4L2ComponentStore: remove unsupported decoder and encoder").

The reason is structural: `components/V4L2Decoder.cpp` maps `VideoCodec::HEVC` to
`V4L2_PIX_FMT_HEVC` — the **stateful** elementary-stream fourcc — and the ChromeOS-derived codebase
contains no Media Request API code at all. It cannot drive `rpi-hevc-dec`, which is stateless and
takes `V4L2_PIX_FMT_HEVC_SLICE`. So HEVC was routed to the ffmpeg HAL out of necessity:

```
persist.vendor.ffmpeg_codec2.v4l2.h265 = true    # ffmpeg owns HEVC
persist.vendor.ffmpeg_codec2.v4l2.h264 = false   # c2.v4l2.avc.decoder owns AVC via /dev/video10
```

Adding an XML entry would advertise a component that fails at creation. The partition is correct.

---

## 5. Confirmed good — no action

Recorded so a future session does not re-investigate:

* The HEVC block is in the **base DTB**, not an overlay: `scb/codec@7eb10000`,
  `compatible = brcm,bcm2711-hevc-dec`, no `status` property (so `okay`). No `dtoverlay=rpivid-v4l2`
  is needed or present — and the boot partition has no `config.txt` at all.
* `CONFIG_VIDEO_RPI_HEVC_DEC=y` and `CONFIG_VIDEO_CODEC_BCM2835=y`, both built in (`/proc/modules`
  is empty).
* `/dev/video19` advertises `V4L2_PIX_FMT_HEVC_SLICE` in, `NV12_COL128` / 10-bit `NC30` out; Request
  API supported; all stateless HEVC controls present.
* CMA pool 512 MB with ~323 MB free.
* `/dev/video12` (`bcm2835-codec-isp`) accepts `NC12` and emits linear `NV12`/`YU12` — a **hardware
  detile path that is currently unused**. A possible alternative to §2's CPU pass, though the
  in-component fix is simpler.

---

## Also worth knowing (not an image change)

**USB audio silently takes over output routing.** `persist.vendor.audio.device=hdmi0` is set, but
with a USB audio device attached the audio policy prefers it: card 3 (EarPods) playback runs while
card 0 (jack), card 1 (`vc4hdmi0`) and card 2 (`vc4hdmi1`) all sit closed. This is correct Android
behaviour, not a fault — but it means a test that assumes HDMI or the 3.5 mm jack is actually
hearing the USB device.
