# CarPlay A/V + input pipeline

> **STATUS:** CURRENT · single owner for audio, video, cornerMasks and touch/HID. Consolidated
> 2026-08-31 from docs 12, 15, 16, 23, 24, 27, 52, 62 (pre-consolidation numbering; the originals
> are in git history and in the 2026-08-31 backup). Correct this file in place — do not add a
> sibling.

**Contents:** audio capability → audio pipeline → video (resolution, cluster, multi-stream, loss
recovery) → cornerMasks → touch/HID uplink.

## Audio — advertised capability (formats, YAML, presets)

<!-- absorbed: ../carplay/06_AV_PIPELINE.md -->

The set of CarPlay audio capabilities the box advertises to iOS is **authored in the VehicleConfig
YAML** (the same document the macOS app pushes at OCBM SUBSCRIBE). This turns the box into a
configurable head-unit (HU) test rig: one YAML selects exactly which codecs / rates / stream types the
box offers, iOS negotiates against that set, and a working session both *tests* the config and
*documents* that OCBM + App support it end-to-end.

Before this, the advertised audio set was hard-gated behind the `CARPLAY_WIRELESS_AUDIO` env (set by the
wireless launcher). That env is now the **transport indicator** (it selects which per-transport arm
of a pushed `audio:` section applies) and the default selector only when the YAML says nothing. (The env selector is interim, box-side — it retires as the app always
pushes `audio:`, per docs/carplay/04_CAPABILITIES_AND_CONFIG.md.)

### The `audio:` YAML section

Added to `VehicleConfig` (`crates/vendor/receiver/src/vehicle_config.rs`). Absent = keep the
transport-gated default (PCM over wired, the 8-entry AAC set over wireless — the proven behavior;
an interim box-side floor per docs/carplay/04_CAPABILITIES_AND_CONFIG.md — the app always pushes the set once config coverage lands,
and the transport-gated default retires).

```yaml
audio:
  preset: wireless_8            # optional named baseline: wired_pcm | wireless_8 (alias wireless_full)
  formats:                      # optional explicit list — when present, REPLACES preset/default entirely
    - {type: 102, audioType: media, out: aac_lc_48k_stereo}
    - {type: 100, audioType: speechRecognition, in: aac_eld_16k_mono, out: aac_eld_16k_mono}
    - {type: 100, audioType: compatibility, in: pcm_16k_mono, out: "pcm_48k_stereo|pcm_16k_mono"}
  wired:                        # per-transport arms (2026-08-10, docs/carplay/04_CAPABILITIES_AND_CONFIG.md B5): one pushed YAML serves
    preset: wired_pcm           # whichever transport connects next; the box presents the matching arm.
  wireless:                     # The app's "Auto — match transport" mode pushes exactly this pair,
    preset: wireless_8          # byte-equivalent to the old box default, so auto sessions are unchanged.
```

Resolution order (in `AudioConfig::resolve`): the matched per-transport arm (`wired:`/`wireless:`,
selected by the session transport — the `CARPLAY_WIRELESS_AUDIO` env's role shrinks to transport
indicator) wins; else the flat keys (an explicit non-empty `formats` list, else a named
`preset`); else the transport-gated default. An all-invalid `formats` list or unknown `preset` falls to the
next level (inside an arm: the flat keys, then the default; in the flat form: the default) (the box never advertises an empty `audioFormats`, which would fail iOS activation). One
unknown codec name skips only that entry, logged — a typo can't invalidate the whole advertised set.
The transport-gated default at the bottom of this resolution order is the interim safety floor
(docs/carplay/04_CAPABILITIES_AND_CONFIG.md): the design state is app-always-pushes, with the box default retired.

#### Entry fields

| Field | Meaning |
|-------|---------|
| `type` | Stream type: **100** MainAudio (bidirectional — carries the mic uplink), **101** AltAudio, **102** MainHighAudio (high-latency media, AAC-LC; corrected 2026-08-01: was "realtime" — R14G17 `AirPlayCommon.h`/`AudioUtils.h` `kAirPlayStreamType_MainHighAudio`/`kAudioStreamType_MainHighAudio` is documented "RTP payload type for **high-latency** audio output"). |
| `audioType` | The route iOS selects against: `media`, `default`, `telephony`, `speechRecognition`, `alert`, `compatibility`. Omitted = the wired PCM **catch-all** (no `audioType` key — this is what lets iOS map `audioType:"media"` onto a type-100 PCM entry over USB). |
| `in` | Input (mic capture) codec name, or omitted/`none` = output-only stream. |
| `out` | Output (playback) codec name. Multiple names may be OR'd with `\|`. |

### The complete `kAirPlayAudioFormat_*` enum (authority: R14G17 `AirPlayCommon.h`)

*Added 2026-08-02. Earlier revisions of this doc presented the token table below as "the full matrix,"
and `receiver::info`'s comment still says the same. **It is not** — it is the subset this project
implements. Apple's enum is larger, and the bits it omits are not gaps in the protocol.*

Source: `AppleCarPlay/Sources/AirPlayCommon.h` lines 335–368 in the licensed R14G17 drop —
**34 defines, top bit 32.** (Path updated 2026-08-16 to the copy CLAUDE.md designates,
`~/carlink/local_carplay_sdk/reference/apple_carplay_sdk_R14G17/AppleCarPlay/Sources/AirPlayCommon.h`.
The originally-cited `old/carplay_RE/carplay_sdk/reference/…` copy is NOT gone — it still exists under the
workspace root, `…/carlink/old/carplay_RE/…`, and is byte-identical over this range; it is simply no longer
the designated drop. The line range and every row of the table below were re-verified define-for-define
against the designated copy on the same date.)

| Bits | Formats | In this project? |
|---|---|---|
| 0, 1 | `Reserved1`, `Reserved2` | n/a — Apple placeholders, never assigned |
| 2–17 | **PCM, 16 variants**: 8 / 16 / 24 / 32 / 44.1 / 48 kHz × mono/stereo at 16-bit (2–11, 14, 15), **plus 24-bit at 44.1 and 48 kHz** × mono/stereo (12, 13, 16, 17) | only **2 of 16** (bits 4, 15) |
| 18–21 | `Reserved3` … `Reserved6` | n/a |
| 22, 23 | AAC-LC 44.1 / 48 kHz stereo | ✅ both |
| 24, 25 | AAC-ELD 44.1 / 48 kHz stereo | ✅ both |
| 26, 27 | AAC-ELD 16 / 24 kHz mono | ✅ both |
| 28, 29, 30 | Opus 16 / 24 / 48 kHz mono | ✅ all three |
| 31, 32 | AAC-ELD 44.1 / 48 kHz mono | ✅ both |
| 33+ | **do not exist in R14G17** | — see bit 43 below |

**27 real formats** (34 defines − `Invalid` − 6 `Reserved`). This project implements **13 of them**, plus
one that postdates R14G17. **All 14 unimplemented formats are PCM variants** — every AAC/Opus format in
Apple's enum is already in the token table.

#### Two findings from diffing against the current SDK

The CarPlay Simulator bundle ships Apple's **current** format-name table
(`~/Downloads/Carplay WWDC/Hardware/CarPlay Simulator.app/Contents/Frameworks/CarPlaySDK.framework/Versions/A/CarPlaySDK`,
extractable with `strings`). Diffed against R14G17:

*(Path rooted 2026-08-16 — it was written bundle-relative, and the `/Volumes/Additional Tools` image it was
originally read from is no longer mounted; the bundle itself has been copied to `~/Downloads/Carplay WWDC/`
and is indexed in [`../ops/03_REFERENCE_INDEX.md`](../ops/03_REFERENCE_INDEX.md) §D. Xcode's
`CarPlaySimulator.devicekitplugin` ships its OWN, separate copy of `CarPlaySDK.framework` — per CLAUDE.md's
order of authority §1 — so do not treat the two as interchangeable when quoting a `strings` extraction.)*

- **`AAC-ELD/32000/1` was added.** This is the provenance of the otherwise-unexplained **bit 43**
  (`aac_eld_32k_mono`) in the table below — it is **absent from R14G17 entirely**. Note the index: Apple
  left a deliberate hole at bits 33–42.
- **Every 24-bit PCM entry is absent.** R14G17 defines `PCM/44100/24/{1,2}` and `PCM/48000/24/{1,2}`
  (bits 12, 13, 16, 17) and names them in `AirPlayAudioFormatToString`; the Simulator binary contains no
  `PCM/*/24/*` string at all. Since 24-bit/48 kHz stereo was the **highest-fidelity format the protocol
  ever had**, this is worth confirming properly before relying on it — a `strings` diff is strong
  evidence but not a symbol table. Test by advertising bit 17 and seeing whether iOS negotiates it.

#### The ceiling, stated plainly

The bitmask is flat — codec × rate × channels — with **no channel count above 2 and no entry for ALAC,
AC-3/E-AC-3, or any object-based format**. There is no way to express Atmos or multichannel audio on this
wire, in any SDK revision on disk (R11B 27 defines → intermediate 32 → R14G17 34 → current 35). **The
best media path CarPlay offers is AAC-LC 48 kHz stereo**, and the direction of travel across revisions is
more mono voice codecs, not higher fidelity.

**Corroborated by Apple's own sessions** (`~/Downloads/Carplay WWDC/`, indexed at
[`../ops/03_REFERENCE_INDEX.md`](../ops/03_REFERENCE_INDEX.md) §E). The entire CarPlay audio-format
story Apple has ever told publicly is three sentences, `wwdc2016-722.txt:80-82`:

> "LPCM is used for wired CarPlay. Wireless CarPlay requires compressed audio. AAC-LC is used for media,
> and you have a choice between OPUS and AAC-ELD for other audio."

That matches the enum exactly. Note what surrounds it: the immediately preceding paragraph
(`wwdc2016-722.txt:71-73`) makes hard, numeric demands about *video* — 24-bit colour, 60 Hz, a specific
H.264 profile, "this is a hard requirement". Apple was precise about video fidelity and silent about
audio fidelity in the same breath, and has stayed silent since. **Across all five CarPlay sessions
(2016 ×2, 2017, 2019, 2023) there are zero mentions of Atmos, spatial, surround, multichannel, lossless,
hi-res, bit depth, or sample rate.**

> Do not read the `{codec: PCM|AAC-LC|AAC-ELD|Opus|DDP|QAAC|Atmos, …}` list in
> [`../carplay/01_OCBM_PROTOCOL.md`](../carplay/01_OCBM_PROTOCOL.md) §"Self-describing streams" as a CarPlay capability claim.
> It is an *OCBM extensibility* illustration — what our own stream descriptor could carry if a future
> format existed. CarPlay does not negotiate DDP, QAAC, or Atmos.

### `mainBuffered` — where Apple's audio effort actually went

Apple's one substantive CarPlay audio improvement is **not** a codec. It is a buffered delivery path,
introduced in `wwdc2023-10150.txt:136-142`:

> "Audio apps are now adopting AirPlay enhanced audio buffering… The audio is provided as an additional
> stream to the vehicle system, called **main buffered audio**. **The CarPlay communication plugin
> contains an up to 2 minute audio buffer**, where audio from iPhone is streamed in **faster than
> real-time speeds**. This makes for improved responsiveness, and audio content can **continue playback
> through an intermittent disconnection**." … "Enhanced buffering is **the preferred platform for
> streaming audio like music** to your car's speakers."

**The buffer is in the head unit, not the phone.** "The CarPlay communication plugin" is the
*accessory-side* component (`wwdc2016-723.txt:9` defines it as such). The phone pushes audio downstream
ahead of playout; the vehicle accumulates up to two minutes and drains it against its own clock.

Every symbol in the Simulator confirms the direction — all of it receiver-side
(`~/Downloads/Carplay WWDC/Hardware/CarPlay Simulator.app/Contents/Frameworks/CarPlaySDK.framework/Versions/A/CarPlaySDK`,
via `strings`; path rooted 2026-08-16 — see the path note under "Two findings from diffing against the
current SDK" above, and do not substitute Xcode's separate `CarPlaySimulator.devicekitplugin` copy):

| Symbol | What it proves |
|---|---|
| `_BufferedAudioEnquePacketInBuffer(AirPlayReceiverSessionRef, AirTunesBufferNode *, size_t)` | the **receiver** holds a node-list buffer |
| `_receiveBufferedRTP(AirPlayReceiverSessionRef, NetSocketRef)`, `_bufReadPacketLength`, `_bufProcessPacket`, `_BufferedAudioThread` | dedicated receiver read thread + state machine (`unknown buffered machine state = %d`) |
| `_BufferedAudioSkewUsingRamstadASRC(...)` | asynchronous sample-rate conversion — only needed if *we* drain against our own clock |
| `_BufferedAudioTrackNetworkPacketLosses(...)` | receiver-side loss accounting |
| `HTTPStatus _requestProcessFlushBuffered(AirPlayReceiverConnectionRef, HTTPMessageRef)` + RTSP verb **`FLUSHBUFFERED`** + `AirPlayAudioStreamFlushBufferedAudio(..., const AirPlayFlushPoint *, ...)` | **decisive** — the phone must be able to tell the car to discard what it already sent (skip/seek/pause). Only meaningful if the car holds the buffer |
| `Failed to read from buffered audio socket: %d, bytesRead this time: %d; expected %d`, `_bufReadPacketLength` | length-prefixed **stream** reads, not datagrams — consistent with `docs/carplay/03_SDK_GROUND_TRUTH.md` §7 calling MainBuffered "media/music, **TCP**" |
| `_BufferedAudioParseFormatInfoFromSetupMessage(CFDictionaryRef, …, AirPlayCompressionType *, …)` + `Unsupported compression type for mainBuffered` | the buffered path negotiates its own **compression type** in SETUP, separate from the `audioFormat` bits — and **fails loudly**, which is rare in this protocol |

This is AirPlay 2's long-standing realtime-UDP vs buffered-TCP split arriving in CarPlay.

> **⚠️ CORRECTS docs/carplay/04_CAPABILITIES_AND_CONFIG.md.** `../carplay/04_CAPABILITIES_AND_CONFIG.md` (`enablesMainBufferedAudio`) once called
> buffered *"better quality than realtime."* It does **not** change the codec — the same `audioFormat`
> bitmask applies. What improves is delivery integrity: **better delivery of the same stream, not higher
> fidelity.** (docs/carplay/04_CAPABILITIES_AND_CONFIG.md's entry has since been corrected in place.) Full reasoning:
> [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-27-1`.

Scope limit worth respecting: WWDC says "intermittent disconnection" without defining the boundary, and
nothing in the strings states what happens on a full session teardown versus a transient link stall.
Untested.

### Codec vocabulary (`in`/`out` tokens)

The subset this project implements — every AAC/Opus format Apple defines, plus two PCM variants. Tokens
map to the CarPlay `audioFormat` bitmask (`receiver::info::audio_format_bit`):

| Token | Bit | Codec / rate / channels | Status |
|-------|-----|-------------------------|--------|
| `pcm_16k_mono` | 1<<4 | PCM S16 16 kHz mono | ● verified |
| `pcm_48k_stereo` | 1<<15 | PCM S16 48 kHz stereo | ● verified |
| `aac_lc_44k_stereo` | 1<<22 | AAC-LC 44.1 kHz stereo | ○ advertisable |
| `aac_lc_48k_stereo` | 1<<23 | AAC-LC 48 kHz stereo (media) | ● verified |
| `aac_eld_48k_stereo` | 1<<25 | AAC-ELD 48 kHz stereo | ○ advertisable |
| `aac_eld_44k_stereo` | 1<<24 | AAC-ELD 44.1 kHz stereo | ○ advertisable |
| `aac_eld_16k_mono` | 1<<26 | AAC-ELD 16 kHz mono (Siri/mic) | ● verified |
| `aac_eld_24k_mono` | 1<<27 | AAC-ELD 24 kHz mono | ○ advertisable |
| `aac_eld_32k_mono` | 1<<43 | AAC-ELD 32 kHz mono | ○ advertisable — **post-R14G17**, see above |
| `aac_eld_44k_mono` | 1<<31 | AAC-ELD 44.1 kHz mono | ○ advertisable |
| `aac_eld_48k_mono` | 1<<32 | AAC-ELD 48 kHz mono | ○ advertisable |
| `opus_16k_mono` | 1<<28 | Opus 16 kHz mono | ○ advertisable |
| `opus_24k_mono` | 1<<29 | Opus 24 kHz mono | ○ advertisable |
| `opus_48k_mono` | 1<<30 | Opus 48 kHz mono | ○ advertisable |

**● verified** = proven end-to-end on the box today (PCM wired media, AAC-LC wireless media, AAC-ELD
wireless mic). **○ advertisable** = the box advertises it and iOS will negotiate it, but on-box
decode/forward for that exact variant is not yet device-confirmed. Both the box decoder
(`receiver::info` / `session::decode_audio_format` — PCM, AAC-LC, AAC-ELD, OPUS arms) and the app's
`AVAudioConverter` path accept the full set; the ○ marks are honesty about what's been *run*, not a
parser limitation.

**Adding a PCM variant is cheap** if one is ever wanted: the 14 unimplemented bits are all PCM, and
`decode_audio_format` already has a PCM arm — it needs the mask constant, a token in
`audio_format_bit`, and a `(rate, channels)` mapping. An unrecognised mask is rejected and the stream
skipped, so a missing bit fails loudly rather than silently mis-decoding. Two source comments overstate
coverage and should be read against the table above: `info.rs`'s *"The full codec/rate/channel matrix the
box + app support"* and *"This table is the box's DOCUMENTED audio capability surface — every codec/rate/
channel it can advertise."* Both describe this project's surface, not CarPlay's.

### Named presets

- **`wired_pcm`** — PCM only, types 100/101, no `audioType` (catch-all). Byte-exact to the stock CCPA's
  `WiredAudioFormats`. Advertising AAC over the USB link makes iOS find no usable PCM media format and
  borrow no MainAudio → no audio, so this is the wired default *value* (held app-side; the box's
  transport-gated fallback to it is interim per docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
- **`wireless_8`** — the 8-entry set from the working carplayd-rs reference (`_BuildAudioFormatsArray`):
  media on type-102 AAC-LC 48k stereo, Siri/mic on type-100 AAC-ELD 16k mono, alerts AAC-ELD 48k stereo,
  plus PCM compatibility. Device-verified: media plays through the box + Siri mic captures.

### App UI

macOS Settings ▸ Configuration ▸ **Audio** (`host/CarPlayHost/carlink_macOS/App/SettingsWindow.swift`):

- **Audio formats** mode picker: *Auto — match transport* / *Wired — PCM* / *Wireless — AAC (full 8)* /
  *Custom…*. Non-custom modes show a one-line summary of the resolved set.
- **Custom** mode: an add/remove list of format entries, each with stream-type / audioType / input /
  output codec pickers (codecs marked ● verified vs ○ advertisable).
- **Supported audio formats** reference — the full vocabulary above, so the configurable capabilities
  are visible in-app.
- The generated `audio:` YAML appears live in the "Generated YAML" disclosure and is what pushes to the
  box on the next connection.

### Data flow

App authors YAML → OCBM SUBSCRIBE → ocbmd lands `/tmp/carplay_cfg.yaml` → airplayd
`load_device_config()` → `VehicleConfig::apply()` resolves `audio:` onto `DeviceConfig.audio_formats` →
`build_info()` emits the `/info` `audioFormats` array. Re-read per control connection, so a config push =
a fresh session picks it up.

---

## Audio — the end-to-end pipeline

<!-- absorbed: ../carplay/06_AV_PIPELINE.md -->

Scope: CarPlay only. Android Auto negotiates its own audio sinks — see the Android Auto doc.

### 0 · The path in one line

```
iPhone ──RTP/UDP (ChaCha20-Poly1305)──▶ airplayd (box)
        └─ per-stream key + format + raw encrypted RTP ──seam :9002/:9003──▶ ocbmd
              └─ CH_MEDIA_AUDIO 0x21 / CH_ALT_AUDIO 0x22 ──USB accessory──▶ host app
                    └─ OCBMAVDecrypt (host-side decrypt) ──▶ AudioPlayer (AVAudioEngine)

mic: MicCapture ──CH_MIC 0x31──▶ ocbmd ──seam :9112──▶ receiver::uplink ──RTP/UDP──▶ iPhone
```

**The box never decodes audio in the shipping configuration.** It decrypts nothing, transcodes
nothing, and holds no jitter buffer; it hands the host the per-stream key and forwards ciphertext.
All decode, buffering and mixing is host-side.

### 1 · Negotiation — what arrives is decided before any audio flows

1. **Advertise.** `receiver::info::build_info` emits `/info` `audioFormats` from the resolved
   capability set (`AudioFormatSpec`; YAML-driven per docs/carplay/06_AV_PIPELINE.md) plus `audioLatencies` as a single
   catch-all entry (no `type`/`audioType`/rate keys ⇒ applies to every stream).
2. **iOS SETUPs streams** against that set. `session.rs` phase 2 handles stream types **100**
   (MainAudio, bidirectional), **101** (AltAudio) and **102** (MainHighAudio). MainBuffered / AuxIn /
   AuxOut fall through to the "NOT IMPLEMENTED — omitted" arm (see §7).
3. **Format decode.** `decode_audio_format(fmt)` maps the SETUP's `audioFormat` bitmask to
   `(AudioCodec, sample_rate, channels)`. An unrecognised mask **rejects the stream** and logs —
   it never silently mis-decodes.
4. **Routing is by `audioType`, not by stream type.** `is_media = audioType == "media"` selects the
   media sink; everything else goes to the voice sink. This matters because **wired media arrives on
   stream type 100 as PCM**, not on 102.
5. **`atype` tagging.** The SETUP's `audioType` is mapped to one byte carried on the seam:

   | `atype` | `audioType` |
   |---|---|
   | 0 | `media` |
   | 1 | `telephony` |
   | 2 | `speechRecognition` (Siri) |
   | 3 | `alert` |
   | 4 | `default` / absent |
   | 5 | `compatibility` |

   `compatibility` is deliberately **not** folded into 4: it is a media-carrying PCM fallback, and at
   48 kHz stereo it is byte-indistinguishable from alt-audio/nav. See §6 for the host-side gap here.

Each audio stream gets its own UDP socket (`dataPort`) and a thread from `spawn_audio`.

### 2 · Downlink on the box (`crates/vendor/receiver/`)

**Crypto** (`stream.rs`). Keys are HKDF-SHA512 of the pair-verify shared secret, salt
`"DataStream-Salt<scid>"`, info `DataStream-{Output,Input}-Encryption-Key`. The **output** key
decrypts iPhone→box. Packet layout is `[12B RTP header][ciphertext][tag 16][nonce 8]`; the nonce
rides every packet (no counter, no sequence state) and the **AAD is `ts‖ssrc` = header bytes 4..12
for every audio stream** on a modern iOS client — verified live, including type-100 ELD.
`MIN_AUDIO_PACKET` = 12 + 16 + 8; anything shorter is dropped.

**Forward-encrypted is the default and the committed model** (`levers::fwd_enc`, env `OCBM_FWD_ENC`;
absent ⇒ true, and only an explicit `0`/`false`/`off`/empty selects the legacy on-box path). In this
mode `spawn_audio` writes three message kinds to the sink, all length-prefixed `[u32 BE len][SEAM_MAGIC
"SEAV" 4B][marker]` (magic added 2026-09-03, `len` counting it — same envelope shape as the video
seams; see `../carplay/01_OCBM_PROTOCOL.md` for why) and all **scid-tagged** so concurrent streams
sharing one sink cannot clobber each other:

- `0x00 SEAM_KEY` — `[key 32][scid 8 LE]`
- `0x02 SEAM_FORMAT` — `[scid 8 LE][codec][rate u32 LE][ch][bits][atype]`, codec `0 PCM · 1 AAC-LC ·
  2 AAC-ELD · 3 OPUS`, `bits` = 16 for PCM else 0
- `0x01 SEAM_PKT` — `[scid 8 LE][raw encrypted RTP packet]`

Key + format are **re-handed on every seam (re)connect** (`key_sent` is cleared when a write fails),
so a mid-stream reconnect never leaves the host keyless or format-blind.

**Two persistent sinks, shared across every SETUP** — `MEDIA_SINK` :9002 and `VOICE_SINK` :9003. They
are process-lifetime connections on purpose: a fresh `TcpStream::connect` per `spawn_audio` starved
later, content-bearing connections during Siri turns (iOS re-SETUPs MainAudio ~4×/turn) and produced
voice silence.

**Legacy on-box path** (only with `OCBM_FWD_ENC=0`): decrypt, then frame per sink —
- media :9002 — AAC-LC wrapped in a 7-byte ADTS header (`forward::adts_from_aac_lc`); wired PCM
  forwarded **verbatim** (fixed 48k/16/stereo, network-order samples);
- voice :9003 — every AU tagged `[rate u32 BE][ch u16 BE][atype u8][len u32 BE][AU]`
  (`forward::tag_voice`), because one socket multiplexes mixed formats. Opus is forwarded tagged, not
  decoded — the box has no Opus decoder.

### 3 · ocbmd — seam to USB

`ccpa/ocbmd/src/main.rs` listens on the local A/V seams and maps port → channel from a fixed table:
`9002 → CH_MEDIA_AUDIO (0x0021)`, `9003 → CH_ALT_AUDIO (0x0022)` (`9001/9005` are the video lanes).

- **Both audio lanes share one out-queue** (`out_audio`) and are **never read-gated**. The
  per-stream backpressure that keeps the video lanes honest is video-only; `OUT_QUEUE_CAP` on the
  audio queue is an **OOM backstop that drops**, counted in `av_dropped`.
- A complete frame on an idle wire takes the `write_vectored` fast path straight to
  `/dev/usb_accessory`, skipping the queue copy.
- On host teardown the audio queues are cleared with the rest of the session state, and the mic /
  input / RTSP seams are dropped (they belong to the departed session's airplayd).

### 4 · Host playback (macOS, `host/CarPlayHost/carlink_macOS/`)

**Decrypt** — `OCBM/OCBMAVDecrypt.swift`. Media and voice payloads share `audioQueue` (both touch the
scid-keyed `audioKeys`/`audioFormats` tables), reassemble their seam buffer, and decrypt each
`SEAM_PKT` with `nonce = [0,0,0,0] ++ pkt.suffix(8)`, `AAD = pkt[4..12]` — the mirror of the box's
`encrypt_audio_aad`, round-trip-tested against the Rust implementation in
`host/ocbm-host` (`audio_decrypt_replica_roundtrip`).

**Route** — `OCBM/OCBMAVBridge.swift::avDidReceiveAudio`:
- no `SEAM_FORMAT` yet (legacy box build) → treat as wired media PCM 48k/16/2 **big-endian**;
- `isPCM` → `feedPCM(..., bigEndian: true)` (wire samples are network order);
- compressed → the per-scid `CompressedAudioDecoder` (AudioToolbox `AVAudioConverter`, AAC-LC /
  AAC-ELD / Opus), created the moment `SEAM_FORMAT` announces the codec so the converter is warm
  before the first AU, then `feedPCM(..., bigEndian: false)`. **Prestaged, not live-validated** —
  decode errors log and drop the AU.

**Play** — `Audio/AudioPlayer.swift`, one `AVAudioEngine` with a **pre-warmed player node per
rate×channel combo**, attached and connected at engine setup, so a new stream (Siri chime 16k mono,
alert 48k stereo) starts with zero graph mutation. Media-role nodes hang off `mediaMixer`, voice-role
nodes off `navMixer`. Everything that touches the engine runs on the serial `engineQueue`.

Delivery philosophy is **not uniform**, and this is the single most consequential thing in the file:

| Class | Streams | Queue cap | Pre-roll |
|---|---|---|---|
| Voice / telephony | MainAudio 100, AltAudio 101 | 150 ms | none — arrival-paced is correct |
| Media | MainHighAudio 102 | 1000 ms | 400 ms |

Media is delivered in **bursts ahead of realtime** (~750 ms fast, then a ~200 ms pause). Under the old
uniform 150 ms cap the node starved every cycle — 1062 underruns / 10 min on hardware, Apple Music
chopping while Siri and telephony stayed clean. A deep cap alone does not fix it: an
`AVAudioPlayerNode` drains as fast as it fills, so media buffers are **held until 400 ms is staged**
and then released in order. On overflow the packet is **dropped** — there is no rate-matched ring
buffer for clock drift (§7).

**Ducking is host-side and energy-gated.** iOS's `duckAudio`/`unduckAudio`/`flushAudio`/`setVolume`
arrive as `POST /command` and are only logged/classified (`MetadataWindow.swift` → `.audioControl`;
`CARPLAY_CMD_DUMP` writes full plists). The actual duck fires on **voice-stream activity**: a voice
buffer whose int16 peak clears ≈ −32 dBFS drops `mediaMixer.outputVolume` to `0.2`, with a watcher
restoring it. The energy gate exists because iOS keeps voice streams alive with continuous digital
silence — packet flow alone ducked media permanently (2026-07-12).

### 5 · Mic uplink

The uplink is the **input leg of the type-100 MainAudio stream** and dies with it.

1. **Arming.** At SETUP, if the stream dict carries `input=true` and a `dataPort`,
   `uplink::configure` takes the stream's **INPUT** key, the control peer's address with the port
   swapped (IPv6 scope preserved) and the negotiated codec.
2. **Gate.** The box pushes `uplink on <rate> <ch>` / `uplink off` **back** over the mic seam, so the
   app gates capture on the real SETUP edge. Siri wants the mic *before* any downlink audio, so a
   downlink-activity gate would clip the onset. A reconnecting peer is told the armed format
   immediately.
3. **Capture.** `Audio/MicCapture.swift` runs `AVAudioEngine` + `AVAudioConverter` to the
   box-negotiated rate/channels and emits **S16LE**; the engine only runs during a live turn — never
   a hot mic between turns.
4. **Transport.** `CH_MIC 0x0031` → ocbmd `forward_mic` → airplayd's mic seam **`127.0.0.1:9112`**
   as `mic <len>\n<pcm…>` lines (`receiver::uplink::read_control`). The seam is established
   **eagerly**, because the `uplink on/off` gate travels back over it.
5. **Encode + send.** `push_pcm` packetizes and `send_au` builds the RTP header (`ssrc = 0`, seq
   incrementing), encrypts with `encrypt_audio_aad` (same `ts‖ssrc` AAD as the downlink) and sends to
   the iPhone's port. Codec is transport-determined:
   - **wired** — raw **big-endian** PCM in fixed sample-count packets, no encoder. This is the only
     path the box builds (feature `mic-uplink`);
   - **wireless** — AAC-ELD via `crates/vendor/eld-codec` (libfdk-aac shim), feature
     `mic-uplink-eld`. fdk-aac is not available on the box, so **airplayd never enables it**. 16 kHz
     mono produces ASC `f8f03000`, byte-identical to the iPhone's own `speechRecognition` stream
     (`eld_16k_mono_asc_matches_iphone`).
6. **One uplink at a time.** iOS re-SETUPs MainAudio each Siri turn; the newest instance's
   key/port/codec supersede the prior, mirroring the downlink sink policy.

### 6 · Android host parity

`host/CarlinkAndroid/` mirrors the same contract: `audio/AudioRingBuffer.kt` (lock-free SPSC,
monotonic cursors, CAS-advanced read cursor) absorbs USB jitter (P99 ~7 ms, max 30 ms) on playback
and on mic capture; `audio/MicProfile.kt` derives the capture profile from the box-negotiated
`(rate, channels)` with a **20 ms tick matching the box's own RTP packetization**, split out as pure
logic precisely because a mismatch is silent — capture opens at 16 kHz mono while the box believes
otherwise, and the only symptom is Siri hearing a pitch-shifted stream. `ProjectionService` must hold
`RECORD_AUDIO` **before** iOS arms the uplink or capture returns zeroed buffers, and it declares the
`microphone` FGS type conditionally to avoid taking video and input down with a `SecurityException`.

### 7 · Known gaps (open, verified against the tree at this commit)

- **`compatibility` (atype 5) is routed as voice on the host.** `OCBMAudioStreamFormat.isVoice` is
  `audioType != 0`, so a `compatibility` stream — a **media-carrying** PCM fallback by the box's own
  reasoning in `forward::tag_voice` — lands on `navMixer` with the 150 ms voice cap, no pre-roll, and
  counts as a ducking trigger. Wired sessions never hit it (media is atype 0 there); it is reachable
  from the `wireless_8` preset. The box tags it correctly; only the host mapping is wrong.
- **`mainBufferedAudio` Phase B is unbuilt** — advertise+echo landed, but SETUP phase 2 still omits
  MainBuffered, so no buffered audio can arrive. Shape and stream-type number unknown. (docs/carplay/04_CAPABILITIES_AND_CONFIG.md,
  `../ops/04_OPEN_ITEMS.md`.)
- **Compressed decode is prestaged, not device-run.** AAC-LC/ELD/Opus decode exists on the host and
  the box advertises the formats, but the ○-marked entries in docs/carplay/06_AV_PIPELINE.md have never been through a live
  wireless session on this bench.
- **No rate-matched ring buffer for long-session clock drift; the behaviour on drift is to DROP.**
- **`OCBMAudioStreamFormat.bits` is parsed and stored but never read.**
- **Opus is never decoded on the box** by design; in the legacy on-box mode it is tag-forwarded raw.

### 8 · Bench levers and evidence

| Lever | Effect |
|---|---|
| `OCBM_FWD_ENC=0` | legacy on-box decrypt/decode path (ADTS + tagged voice) instead of forward-encrypted |
| `CARPLAY_AUDIO_CAPTURE=<path>` | dumps `shared‖scid‖key‖type‖first packet` per stream — the offline decrypt kit |
| `CARPLAY_AU_DUMP=<path>` | length-prefixed decoded-AU dump per `type`/`scid` (on-box path only); read once at spawn, never per-AU |
| `CARPLAY_CMD_DUMP` | full plists for every `POST /command`, incl. the audio-focus signalling |

Offline/replica coverage: `forward.rs` (`aac_lc_adts_header`, `eld_tagging`), `stream.rs`
encrypt→decrypt round-trips, `eld-codec` ASC test, `host/ocbm-host`
`audio_decrypt_replica_roundtrip`, `setup_driver.rs` `phase2_media_audio_keeps_control_port`, and the
Swift `tests/main.swift` OCBM framing + `StreamMetrics` audio cases. Live validation to date: wired
PCM media, Siri/telephony voice and the mic uplink on hardware; `ocbm-host avdec` decrypted thousands
of audio packets on both lanes with **0 failures**.

### 9 · Corrections applied while writing this document (2026-08-31)

Comment-only fixes, no behaviour change:

- `crates/vendor/receiver/src/uplink.rs` — `start_control_listener`'s doc said "the control-in
  (`:9110`) listener". `:9110` is the **HID input** seam; airplayd passes `127.0.0.1:9112`
  (`MIC_INGEST_ADDR`). Corrected to name the caller-supplied address.
- `crates/ocbm-proto/src/lib.rs` — the `SEAM_FORMAT audio_type` legend stopped at `4 default`,
  omitting `5 compatibility`, which `session.rs` has emitted since the compatibility split.
- `host/CarPlayHost/carlink_macOS/OCBM/OCBMAVDecrypt.swift` — same legend, same omission; the
  `isVoice` comment now names the misrouting recorded in §7 rather than implying it is intended.

---

## Video — resolution negotiation (800×480 → 1920×720)

<!-- absorbed: ../carplay/06_AV_PIPELINE.md -->

Definitive diagnosis of why every CarPlay session negotiated **800×480 H.264** despite `info.rs` defaulting
to 1920×720, the fix, and the on-hardware proof. From a 6-agent parse of the Rust stack (unanimous) plus a
live validation.

### Root cause — a hardcoded override (EVIDENCED, 5 of 6 agents)
`~/Documents/carlink/ccpa_custom/ccpa/airplayd/src/main.rs` (the daemon the box actually runs)
constructs `DeviceConfig::default()` (1920×720) and then **overrides it to 800×480** before building `/info`:
```rust
let mut dev = DeviceConfig::default();   // 1920×720 (`impl Default for DeviceConfig` — info.rs:114-115 in today's `crates/vendor/receiver/src/info.rs`; was cited as info.rs:62-63 against the then-sibling `ncm_carplayd` tree)
dev.display_width  = 800;                // ← the override (was line 342)
dev.display_height = 480;                // ← (was line 343)
let info = build_info(&dev);
```
`build_info` (then `old/ncm_carplayd/receiver_core/crates/receiver/src/info.rs` — that tree is archived at
`~/Documents/carlink/old/ncm_carplayd`; the LIVE copy is vendored in-repo at
`crates/vendor/receiver/src/info.rs`) bakes those into three iPhone-facing `/info` fields simultaneously
(anchors below re-verified against the vendored copy 2026-08-16; the 2026-07-10 numbers were the old tree's):
- `displays[0].widthPixels`/`heightPixels` — **`build_info`, info.rs:572-579** (the field the iPhone reads
  to choose the H.264 coded resolution)
- the `viewAreas`/`safeArea` rect — `info.rs::view_areas()` (defined info.rs:375, inserted info.rs:586-595)
- the HID touchscreen descriptor X/Y logical maxima — `info.rs::touchscreen_descriptor()` /
  `touchscreen_multi_descriptor()` (info.rs:138, :172), fed `cfg.display_width/height` at info.rs:771-774

The iPhone encodes exactly what `/info` advertises — **proven by the 1:1 match**: advertised 800×480 =
observed SPS 800×480.

### Ruled out (EVIDENCED)
- **iAP2** carries no display geometry (identification 0x1D01, CarPlayStartSession — none). `ccpa/iap2d`.
- **AirPlay SETUP/RTSP** carries no dimensions — the box does not generate SPS/PPS; it receives the avcC
  (VideoConfig) from the iPhone's encoder and forwards it. `receiver/src/session.rs setup_phase2` (screen
  110 response = only `type` + `dataPort`).
- **The docs/carplay/02_SESSION_LIFECYCLE.md "iOS cache pin / requires forget" theory is DISPROVEN.** The box was literally advertising
  800×480 from `airplayd`; it was never an iOS cache artifact. (docs/carplay/02_SESSION_LIFECYCLE.md §"Video resolution" reasoned from
  the 1920×720 *struct default*, not the airplayd override it couldn't see.) The "forget" done on
  2026-07-09 was chasing a wrong hypothesis.
- No competing 800×480 literal existed anywhere else at the time of this sweep (2026-07-10, exhaustive):
  the only `800`/`480` / `0x320`/`0x1E0` were `airplayd:342-343` (the override, since removed) and a
  `hid.rs` unit-test constant. **Anchors re-verified 2026-08-16:** that test constant is now
  `crates/vendor/receiver/src/hid.rs:129-130` (`touch_is_little_endian_xy`; the same two values also occur
  at `hid.rs:111` in `multi_report_matches_apple_fill_order`), and the receiver's `DISPLAY_WH=(1920,720)`
  is `crates/vendor/receiver/src/uplink.rs:83` — touch-scaling only. The sweep's "nowhere else" is a
  statement about the 2026-07-10 tree: features added since carry their own 800×480 (the ALT/cluster
  fallback dims at `info.rs:612-613`).
  **Latent, flagged, not fixed:** `uplink.rs::set_display` has **no caller anywhere in the repo**.
  What tracks the resolution is airplayd's own `DISPLAY_WH` static; the receiver's
  `uplink::handle_touch` path still scales against the never-updated 1920×720 default.

### The fix (deployed + validated)
Changed `airplayd/src/main.rs:342-343` to **1920×720**, cross-compiled (armv7-musl), deployed to
`/usr/sbin/airplayd` (old binary backed up `/usr/sbin/airplayd.res800bak`).

**On-hardware proof (2026-07-10):** a fresh session — *no iPhone "forget"* — produced:
```
[com.carlink.video:H264] Format updated from SPS/PPS — 1920×720
```
`session_healthy=1, phase=STREAMING`. The same iPhone that was streaming 800×480 minutes earlier honored
1920×720 on the very next connection, purely from the changed `/info`. This is the original-firmware
behavior: change the advertised resolution → honored on the next connect, no re-pairing.

### Why no forget is needed — the protocol model (see docs/carplay/03_SDK_GROUND_TRUTH.md)
`/info` is re-served on every control connection; a config pushed at SUBSCRIBE = a fresh session = `/info`
re-read. Resolution (`displays[].widthPixels/heightPixels`) is in the reconnect-consumed class, so a new
session adopts it. (A mid-session change would use the `updateDisplayPanels` `/command` — task #21/#5.)

*(The 2026-07 follow-up list is dropped: the YAML/VehicleConfig path landed and 1920×720 now
survives only as the app-less fallback in `base_device_config()`.)*

---

## Video — the alternate / cluster stream

<!-- absorbed: ../carplay/06_AV_PIPELINE.md -->

Ground truth for the CarPlay **instrument-cluster video** (second AirPlay screen stream). Evidence tags: **[E]** = evidenced (symbol / string / template / SDK constant), **[I]** = inferred.

Primary sources:
- Apple templates: `.../CarPlaySimulator.devicekitplugin/Contents/Resources/VehicleConfigs/Configs/{Standard,Widescreen} {Instrument Cluster,Navigation}.yaml`, `.../VehicleDataConfigs/Navigation/Navigation.vdc.json`.
- `CarPlaySDK.framework/Versions/A/CarPlaySDK` (arm64e) — sourced from SDK symbols/strings/templates.
- `CarPlaySimulator` binary (sender side).
- Receiver: then `ncm_carplayd/receiver_core/crates/receiver/src/{session.rs, info.rs, events.rs}` — that
  tree is now archived at `~/Documents/carlink/old/ncm_carplayd` and the LIVE copy is vendored
  in this repo at `crates/vendor/receiver/src/{session.rs, info.rs, events.rs}`. All `session.rs`/`info.rs`/
  `events.rs` anchors below were re-verified against the vendored copy on 2026-08-16 except those marked
  historical in §4.
- `../carplay/03_SDK_GROUND_TRUTH.md` §3/§4/§6, `../carplay/05_METADATA_AND_CONTROLS.md`.

---

### 0. What the alt/cluster video stream IS

**[E]** The cluster video is a **second AirPlay screen stream** — a distinct `(AirPlayStreamType, UUID, port)` triple SETUP alongside the main screen. In the templates it is `VideoStream.Alt1`, painted onto `DisplayPanel.Alt1` whose `displayProperties: [showsInstruments]`, and seeded with `initialURL: maps:/car/instrumentcluster/map`. It carries the Apple Maps **cluster map** (turn-by-turn map + instruction card) rendered by iOS/Maps and H.264/HEVC-encoded to the accessory.

Three things must not be conflated:

| Channel | What it is | Identity | Evidence |
|---|---|---|---|
| **Main screen** | The CarPlay app UI (springboard, apps) | `VideoStream.Main`, `DisplayPanel.Main`, display `type` **110**, screen stream **type 110** | `mainVideoStream`/`DisplayPanel.Main` in every YAML; info.rs:603 `type=110`; SDK `getPlatformLayer` @type 0x6e **[E]** |
| **Alt / cluster video** | Second screen = the Maps cluster map | `VideoStream.Alt1`, `DisplayPanel.Alt1 {showsInstruments}`, screen stream **type 111** | `altVideoStreams`/`altDisplayPanels` in cluster+nav YAMLs; SDK `getClusterLayer:` @type 0x6f **[E]** |
| **VDC nav telemetry** | Turn/route data over the Vehicle Data / CARP channel — *not video* | `carpConfig.carpIdentifier: Navigation`, `Navigation.vdc.json` (RouteStatus, Legs, Destination…) | `Navigation.yaml` `carpConfig`; `Navigation.vdc.json` **[E]** |

Note the template split: **Instrument Cluster** configs give you the alt video **without** the CARP nav channel; **Navigation** configs add `carpConfig` (the VDC nav-telemetry accessory) **on top of** the same alt video panel. So "cluster video" and "nav telemetry" are orthogonal — a car can have either or both. **[E]**

`ClusterDP.*` (speed/range/battery, `CommonStates.lua`) and TBT audio (the `turns` AirPlay mode) are *separate again* from the video — see docs/carplay/03_SDK_GROUND_TRUTH.md §6. **[E]**

---

### 1. Decisive protocol constants (all [E])

**AirPlay screen stream types** (from `ScreenStreamStart` dispatch, SDK `otool -tvV`):
- `0x6e` = **110 = MainScreen** → receiver calls selector **`getPlatformLayer`** (cfstring @0x3ee030).
- `0x6f` = **111 = AltScreen / cluster** → receiver calls selector **`getClusterLayer:`** (cfstring @0x3ee050); stop notification `STScreenStreamAltStopped` @0x3ee070.

**SETUP per-screen-stream keys** (resolved cfstrings inside `_AirPlayReceiverSessionScreen_Setup`):
- **`type`** (int64) — the stream type (110 or 111).
- **`uuid`** — the stream UUID (retained; matches log `screen stream for type: %u, uuid: %@ set up on port: %d`).
- **`latencyMs`** (int64, default **0x46 = 70**).

**changeModes / resource model** (from `_AirPlayReceiverSessionMakeModeStateFromDictionary` + `Modes changed:` log + events.rs shipped wire):
- Modes dict = `appStates[]{appStateID, entity, speechMode}` + `resources[]{resourceID, entity, permanentEntity}`.
- Live resource ownership entities logged: **`screen` (+ `permScreen`), `mainAudio` (+ `permMainAudio`), `speech`, `phone`, `turns`**.
- **resourceID 1 = MainScreen, resourceID 2 = MainAudio** (`events.rs::send_take_screen`, events.rs:732–742, shipped). `transferType` **1 = Take, 2 = Untake**; `transferPriority` (500 = UserInitiated); `takeConstraint`/`borrowConstraint` (100 = Anytime); reason under `reasonStr`.
- **There is NO separate `resourceID` for the alt/cluster screen.** `MainScreen/MainAudio/MainHighAudio/Speech/Phone` are the only resource entities; `altScreen`/`AltScreen` appear **only** as a SETUP *feature* name and a *stream-type* name — never as a modes resource. **[E]**

**Command surface** (SDK exported symbols):
- `_AirPlayReceiverSessionChangeModes`, `_AirPlayReceiverSessionChangeResourceMode` (thin wrapper: validates a 1/2 selector then calls `ChangeModes`), `_AirPlayReceiverSessionRequestUI` (`requestUI`), `_AirPlayReceiverSessionStopUI` (`stopUI` / `changeUIContext`), `_AirPlayReceiverSessionCopyAltScreenURLs`, `_AirPlayReceiverSessionHasFeatureAltScreen`. **[E]**

**/info advertisement keys** (confirmed strings): `altScreenURLs`, `altScreenSuggestUIURLs`, `supportsAltScreen` (SETUP-feature bool, logged `AirPlay supportsAltScreen - %{bool}d`), `approvedClusterURLs`, `mapAppearance`/`enablesMapAppearance`, `initialVideoStreams`, `uiContextLastOnDisplayURLs`/`uiContextNowOnDisplayURLs`, `showsInstruments`. Cluster URL namespace: `maps:/car/instrumentcluster`, `maps:/car/instrumentcluster/map`, `maps:/car/instrumentcluster/instructioncard`. **[E]**

---

### 2. (a) SUBSCRIBE / ENABLEMENT CHAIN — ordered checklist

#### Step A — VehicleConfig (declarative, drives everything)  [E]
From the four cluster/nav YAMLs — the fields that turn the alt video on:
1. `displayPanelsConfig.altDisplayPanels[]` → one panel `displayPanelID: DisplayPanel.Alt1` with **`displayProperties: [showsInstruments]`**. This is the flag that makes it a *cluster* panel.
2. `videoStreamsConfig.altVideoStreams[]` → `videoStreamID: VideoStream.Alt1`, `pixelDimensions`, `viewAreas[]` (may be multiple candidate rects, e.g. widescreen offers 1920×720 / 800×480 / 1024×600), **`initialURL: maps:/car/instrumentcluster/map`**. Note: alt video stream has **no `hidConfig`** — it is display-only (no touch/knob input routed to it).
3. `accessoryConfig.enablesMapAppearance: true` — required capability for the cluster map to render.
4. (Navigation variants only) `carpConfig.carpIdentifier: Navigation` — adds the VDC nav-telemetry accessory; **not** required for the cluster video itself.

#### Step B — `/info` advertisement (accessory → iPhone)  [E]
The accessory must expose the superset:
5. A second **`displays[]`** entry for the cluster (built by `AirPlayAltScreenDictCreate` vs `MainScreenDictCreate`) — a display with a cluster role (`Cluster_Display`/`Secondary_Cluster_Display`) whose `uuid` matches a HID/`displayUUID`, plus its own `viewAreas`/`safeArea` (typically an inset safeArea, unlike the full-bleed main). Mirror in `displayPanels`.
6. **`altScreenURLs`** (and optionally `altScreenSuggestUIURLs`) listing the permitted cluster URLs (must include `maps:/car/instrumentcluster/map`). iOS reads these via `AirPlayReceiverSessionCopyAltScreenURLs`.
7. `enablesMapAppearance`/`mapAppearance` capability advertised.
8. The alt-screen feature bit in the `features`/`extendedFeatures` superset so it can survive the SETUP intersection.

#### Step C — SETUP feature-intersection gate  [E]
9. The SETUP **request** `features:[…]` from iOS is intersected with the accessory's advertised set; `AirPlayCopyAccessoryEnabledFeatures` = the live set. The **`altScreen`** feature must survive this intersection, logged `### Alt Screen supported: %s` / `supportsAltScreen - %{bool}d`. Advertising alone is insufficient — it must survive the intersection (docs/carplay/03_SDK_GROUND_TRUTH.md §3).

#### Step D — SETUP the second screen stream  [E]
10. Once `altScreen` is live, iOS issues a screen-stream SETUP whose `streams[]` entry carries **`type: 111`** (AltScreen) + a fresh **`uuid`** + `latencyMs`. Receiver allocates a data port and answers (same shape as the main type-110 screen SETUP).
11. On stream start, the receiver's `ScreenStreamStart` sees type 111 and binds the frames to the **`getClusterLayer:`** layer (vs `getPlatformLayer` for 110). Log: `screen stream for type: 111, uuid: … set up on port: …`.

#### Step E — Trigger to actually encode cluster video  [I/E]
12. **[I]** iOS begins encoding the cluster once there is cluster content to present on the `showsInstruments` display — i.e. Maps presents a cluster URL. `initialURL`/`initialVideoStreams` seed the first URL (`maps:/car/instrumentcluster/map`); thereafter presentation is driven by `requestUI`(cluster URL) / the Maps app being active. The type-111 stream carries frames only while a cluster URL is presented on that display.

---

### 3. (b) LIVE ENABLE / DISABLE — exact values

**The cluster is NOT toggled by a take/untake of a dedicated modes resource** — there is no alt-screen `resourceID` (see §1). resourceID 1 (MainScreen) take/untake controls **main-app foregrounding**, not the cluster. Two mechanisms actually gate the cluster at runtime, no reconnect:

#### Mechanism 1 — present / dismiss the cluster content (primary)  [E]
**CORRECTED 2026-08-01: was described as `requestUI`/`stopUI`.** Verified against the authoritative
CarPlay Simulator's own content picker (`events.rs` `send_show_ui`/`send_stop_ui`, ~line 784-827,
confirmed 2026-07-30 by disassembling `_AirPlayReceiverSessionShowUI`/`_AirPlayReceiverSessionStopUI`):
the picker uses **`showUI`/`stopUI`, keyed by `params["uuid"]`** — not `requestUI`, and not `streamID`.
- **Show:** `showUI` with the cluster stream's uuid (+ optional url) → `AirPlayReceiverSessionShowUI`. Wire: `{ type: "showUI", params: { uuid: <cluster stream uuid>, url: "maps:/car/instrumentcluster/map" } }`. `uuid` is REQUIRED; `url` optional. The URL must be in the advertised `altScreenURLs`/`approvedClusterURLs` allowlist or it is rejected. **[E]**
- **Hide:** `stopUI` → `AirPlayReceiverSessionStopUI` → `{ type: "stopUI", params: { uuid: <cluster stream uuid> } }` (no url — a url here is ignored). **[E]**
- `suggestUI` is the softer variant (`altScreenSuggestUIURLs`).
- `requestUI` still exists in `events.rs` as **`send_request_ui()`** (events.rs:755) — the no-URL, no-uuid "bring our UI forward" nudge emitted after RECORD, wire `{type:"requestUI", params:{}}` — but it is NOT what the Simulator's content picker sends for cluster content.
  (The URL-addressed `send_request_ui_url`/`send_stop_ui_url` pair was deleted in a dead-code sweep;
  today's only cluster-addressed emitters are `send_show_ui` and `send_stop_ui`.)

#### Mechanism 2 — SETUP / partial-TEARDOWN the type-111 stream  [E]
- Because iOS SETUPs the alt screen as its own `streams[]` entry, the stream can be torn down independently via a **partial TEARDOWN** (a TEARDOWN whose body carries a `streams` array → tears only those streams, session stays alive — `session.rs::teardown`, the `streams`-array branch at session.rs:1681) and re-SETUP later. This is the hard on/off of the video pipe itself. Receiver `STScreenStreamAltStopped` marks the stop.

#### On `changeResourceMode` / `changeModes` and the cluster
- `AirPlayReceiverSessionChangeResourceMode(session, selector∈{1,2}, resourceID, …)` → builds a one-resource modes dict and calls `ChangeModes`. Valid **only** for resourceIDs 1 (MainScreen) / 2 (MainAudio) / speech / phone. **You cannot Take/Untake "the cluster" this way — no such resourceID exists.** Use Mechanism 1/2. **[E]**
- Shipped Take template (for reference, MAIN screen focus): `{type:"changeModes", params:{ resources:[{ resourceID:1, transferType:1, transferPriority:500, takeConstraint:100, borrowConstraint:100 }], reasonStr:"video focus" }}` (`events.rs::send_take_screen`, events.rs:732). The cluster relies on this main-screen focus being held (see §5), but toggling it is done via showUI/stopUI (§3 Mechanism 1).

---

### 4. (c) What the box (`receiver_core`) must ADD to negotiate + forward a 2nd screen

> **⚠️ SHIPPED — this supersedes the "must ADD" framing of this section.** The type-111 cluster path is
> BUILT and validated: the SETUP arm, the second screen socket, the `displays[]` cluster entry with
> `altScreenURLs`, and the host-side per-lane decode all exist; the box forward is gated behind
> `events::nav_forward()` (default-OFF). The `ncm_carplayd` line numbers in §4 below are historical.
> Full account: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-23-2`.

Today the receiver is a **single flat main display** (info.rs:260 one `displays[]` type-110 entry; session.rs:196 `setup_phase2` handles screen type **110** + audio 100/101/102 only; type-111 hits the `_ => … not yet handled` arm at session.rs:305). To carry the cluster:

1. **`/info` (info.rs):**
   - Add a **second `displays[]` entry** with a cluster role and a distinct `uuid` (+ matching HID `displayUUID`), its own `viewAreas`/`safeArea` (inset), and appropriate feature bits (display-only, no touch needed).
   - Add **`altScreenURLs`** (incl. `maps:/car/instrumentcluster/map`) and advertise `enablesMapAppearance`/`mapAppearance`.
   - Add the **`altScreen`** feature bit to the advertised `features`/`extendedFeatures` superset. (info.rs:169–175 currently *deliberately* keeps `altScreen` OUT of the negotiated echo because ncm is single-display — that guard must be lifted and the type-111 path built first.)

2. **SETUP negotiation (session.rs `setup_phase2`):**
   - Handle `streams[]` entry `type == 111`: open a **second screen data socket** (TCP listener, same as the 110 leg at session.rs:213), spawn a **second screen receive/decode loop**, and echo `{type:111, dataPort, …}`.
   - Keep the `uuid` from the SETUP so frames on that stream are tagged as cluster vs main.
   - Support **partial TEARDOWN** of just the 111 stream (the partial-teardown path already exists at session.rs:349 — extend it to close the alt screen loop).

3. **SETUP-phase1 feature echo (session.rs:162–185):** include `altScreen` (and `viewAreas`) in the negotiated features **only** once 1+2 exist — otherwise iOS may SETUP a 111 stream the receiver can't service.

4. **Forwarding (forward.rs / IPC):** route the type-111 decoded frames to a **second sink** (a second IPC video port / a "cluster" surface) distinct from the main-screen sink, so the head unit can paint the cluster display separately.

5. **Live control (events.rs):** add `requestUI{url:"maps:/car/instrumentcluster/map"}` and `stopUI` emitters to present/dismiss the cluster on demand (mirrors existing `send_take_screen`/`requestUI` helpers). The box only *emits* these; whether/when to present or dismiss is the app's decision (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).

---

### 5. (d) Runtime KEEP-ALIVE / focus requirements

- **`enablesMapAppearance` capability** must be present and negotiated — without the map-appearance capability iOS will not render the cluster map into the type-111 stream. **[E]** (`enablesMapAppearance` template flag; `mapAppearance`/`mapAppearanceModes`/`SessionClusterMapAppearanceView` in the simulator.)
- **Main-screen focus held.** The cluster is secondary to the CarPlay session; the accessory must hold the **MainScreen resource** (changeModes Take resourceID 1) so iOS keeps an app/Maps foregrounded and producing cluster content. If the receiver *untakes* the screen, iOS stops encoding (this exact regression is documented in `events.rs::send_take_screen`'s doc comment, events.rs:721–724, for the main stream). **[E]**
- **URL presented.** The type-111 stream only carries frames while a cluster URL from the `altScreenURLs`/`approvedClusterURLs` allowlist is presented (`initialURL`/`requestUI`); `stopUI` blanks it. **[E/I]**
- **AirPlay keep-alive.** Same session keep-alive as the main stream (`keepAlive{SendStatsAsBody,LowPower}` `/info` keys, event-channel liveness). `forceKeyFrame` is available per-stream to recover a decoder. **[E]** (docs/carplay/03_SDK_GROUND_TRUTH.md §3; events.rs ForceKeyFrame.)
- **Data staleness.** Receiver stamps last-A/V activity into ONE session-wide `activity: Arc<AtomicU64>` (then `session.rs:82`; today `crates/vendor/receiver/src/session.rs:349`, doc `:301-303` — the old anchor was correct against the archived `ncm_carplayd` tree and rotted when the receiver was vendored) that is cloned into every stream thread, the alt loop included (`spawn_screen(… 9005 …, self.activity.clone())`, session.rs:841–842), while per-stream liveness rides its own `stream_flag` (session.rs:441). **UPDATED 2026-08-16:** the stamp is session-wide, not per-stream, and the "the alt loop must feed the same liveness" requirement is now MET — the idle watchdog already sees cluster traffic.

---

### Appendix — evidence index
- Stream types 110/111 + selectors: `otool -tvV CarPlaySDK` `ScreenStreamStart` @0x1d04c/0x1d08c → cfstrings `getPlatformLayer`(0x3ee030)/`getClusterLayer:`(0x3ee050)/`STScreenStreamAltStopped`(0x3ee070).
- SETUP keys `type`/`uuid`/`latencyMs`: `_AirPlayReceiverSessionScreen_Setup` @0x12cdc, cfstrings 0x31ae43/0x319525/0x3213e2.
- Modes struct: `_AirPlayReceiverSessionMakeModeStateFromDictionary` cfstrings `appStates/appStateID/entity/speechMode/resources/resourceID/permanentEntity`; `Modes changed:` log string.
- resourceID 1/2 + transferType 1/2: `crates/vendor/receiver/src/events.rs::send_take_screen` (events.rs:732–742, shipped wire).
- ChangeResourceMode→ChangeModes: `_AirPlayReceiverSessionChangeResourceMode` @0xb618 tail-calls `_AirPlayReceiverSessionChangeModes`.
- /info + capability strings: `strings CarPlaySDK`/`CarPlaySimulator` → `altScreenURLs`, `altScreenSuggestUIURLs`, `supportsAltScreen`, `approvedClusterURLs`, `enablesMapAppearance`, `maps:/car/instrumentcluster{,/map,/instructioncard}`.
- Templates: `Standard/Widescreen Instrument Cluster.yaml`, `Standard/Widescreen Navigation.yaml`, `Navigation.vdc.json`.
- Receiver gaps (HISTORICAL — old `ncm_carplayd` line numbers; the gaps themselves are CLOSED, see the §4
  STATUS block). Current anchors, `crates/vendor/receiver/src/`: `session.rs::setup_phase2` (:690), its
  type-111 arm (:841–842, `spawn_screen(… 9005 …)`) and the partial-TEARDOWN branch (:1681);
  `info.rs` main type-110 display (:603), alt type-111 entry (:632) and `altScreenURLs` (:651);
  `events.rs::send_take_screen` (:732), `send_show_ui`/`send_stop_ui` (:791/:814), `nav_forward()` (:843).

---

### ADDENDUM 2026-07-12 — VERIFIED trigger (corrects the [I] inference above)

The earlier body inferred the cluster trigger was `requestUI{url:"maps:/car/instrumentcluster/map"}`
and concluded there is no dedicated altScreen focus. A cross-source trace (old Carlinkit firmware +
live wired [CAP], see `old/ncm_carplayd/docs/14_WIRED_CARPLAY_PROTOCOL.md` §4.3 & §8.1 — that tree is archived at `~/Documents/carlink/old/ncm_carplayd`) corrects this:

- **The old firmware's "two nav commands"** = adapter Command `0x08` opcodes **508
  `RequestNaviScreenFocus`** (START) / **509 `ReleaseNaviScreenFocus`** (STOP). Live-verified
  2026-06-13: 508 → iPhone spins up its 2nd encoder and streams cluster video; 509 → it stops within
  ~seconds; main video keeps flowing. (Main-screen pair for contrast = 500/501 RequestVideoFocus.)
- Those are **USB-adapter** opcodes. `ccpa_custom` IS the AirPlay receiver, so it does not send 508;
  it emits the AirPlay operations 508 maps to. VERIFIED wired handshake order (docs/carplay/04_CAPABILITIES_AND_CONFIG.md [CAP]):
  `SETUP type:111 → Modes: screen accessory → RequestNaviScreenFocus → SETUP type:110 → RequestVideoFocus
  → Modes: screen controller, mainAudio accessory`.
- **What this means for us:**
  1. **Advertising `altScreen`** (2nd `displays[]` type-111 entry + `enabledFeatures:[altScreen]`) is
     what makes iOS AUTO-SETUP the type-111 stream. The offer is the trigger — there is no separate
     "create the stream" command. (Genuine CCPA also echoes `viewAreas`; we hold that until /info
     carries the backing viewArea structures.)
  2. The screen **focus** is a `changeModes` screen **Take** — exactly `events::send_take_screen()`
     (resourceID 1), already issued after RECORD.
  3. `requestUI(maps:/car/instrumentcluster/map)` is a **complementary content step** (foreground
     Maps' cluster view into the 111 stream; URL must be in advertised `altScreenURLs`), NOT the
     encoder gate. Kept as a content nudge.
- **Open item:** the capture shows RequestNaviScreenFocus as a screen-focus DISTINCT from
  RequestVideoFocus, but the 320.17 SDK only defines one screen resourceID (mainScreen=1); a dedicated
  altScreen resourceID is net-new (later SDK, not in source). If type-111 sets up but sends no frames,
  the missing piece is that distinct alt focus — experiment with resourceID candidates then.
- **Evidence:** old firmware `~/Documents/carlink/carlink_macOS/carlink_macOS/Protocol/{MessageTypes.swift:436-443
  (`aaRequestNaviScreen = 508` / `aaReleaseNaviScreen = 509` at :442-443), AdapterProtocol.swift:174-201}`
  (the OLD-firmware companion app — NOT this repo's `host/CarPlayHost/carlink_macOS`);
  `old/carplay_RE/carplay_sdk/MAP/tbox_map/libSdCarplay.md:150` (changeModes altScreen dimension);
  `old/ncm_carplayd/docs/14` §4.3, §8.1.

#### VALIDATED 2026-07-12 — end-to-end, live session
Deployed (airplayd `180e796d`) + fixed host YAML generator + session reset. Live box log:
```
[airplayd] cfg: /tmp/carplay_cfg.yaml (1711 B) → 1280×720@30 hevc=true dpad=true
[session] SETUP phase2 ALT screen(111) scid=… → dataPort 49454 (→ :9005)
[session] SETUP phase2 screen(110) …        [session] SETUP phase2 audio(100) …
[screen] iPhone connected … forwarding video → 127.0.0.1:9005   [screen] carlink :9005 connected
```
Host log: `[alt] hvcC parsed: VPS 24B, SPS 42B, PPS 7B` (cluster stream is **HEVC**); `A/V decrypt —
video fail=0` throughout (main session NOT torn down); floating "Nav / Alt Video" window opened and
stayed up (sustained alt frames). **Conclusion CONFIRMED: advertising `altScreen` alone makes iOS
auto-SETUP the type-111 stream (111 before 110, matching the [CAP] capture) — no explicit start
command needed.** The dedicated alt decoder + isolated floating window behave exactly per directive.

---

## Video — multi-stream, view areas, dynamic resize

<!-- absorbed: ../carplay/06_AV_PIPELINE.md -->

Status: **REFERENCE ONLY.** Grounded in WWDC 2019-252 & 2023-10150 + the local Xcode 27 CarPlay
Simulator (`CarPlaySimulator.devicekitplugin`, BuildVersion 456) + the CarPlaySDK protocol reference notes and the wired
CPC200-CCPA session capture (`old/ncm_carplayd/research/**`, that tree archived at
`~/Documents/carlink/old/ncm_carplayd`). Labels: [CAP] wire-verified · [SIM] in the local
Simulator · [SDK] SDK-symbol reference · [I] inferred.

The macOS host app does NOT implement dynamic resizing (runtime view-area switching). It is documented
here for a future **Android** implementation where it would apply. The other two topics (a 3rd stream,
static multi-view-area) are documented with their implementation cost + teardown risk.

### 1. How many video streams (the "three streams" clarified)
- Stream/display discriminator on the wire = the integer **`type`** on both the `/info` `displays[]`
  entry and the RTSP SETUP: **110 = main console**, **111 = instrument cluster #1** (`showsInstruments`),
  **112 = instrument cluster #2** (`DisplayPanel.Alt2`/`VideoStream.Alt2`). [SDK]/[SIM]
- The Simulator states literally: **"CarPlay supports up to two Instrument Clusters."** So the max is
  **3 video streams = main + 2 clusters.** [SIM]
- **The "instruction card" is NOT a third stream** — it is a **content type** a cluster can display:
  `maps:/car/instrumentcluster/instructioncard` vs `maps:/car/instrumentcluster/map` (Simulator content
  labels "Instrument Cluster Map" / "Instrument Cluster Instruction Card", `setAllowedContentTypes:`,
  constraint "Only one cluster UI stream should display a map"). WWDC's "map + card in two parallel
  streams" = using BOTH clusters (111 map + 112 card) to show them simultaneously. [SIM]
- Type 112 is **[SDK]/[SIM] only — never observed on the CCPA wire**
  (`old/ncm_carplayd/research/ios27_sdk_inventory/10_video.md:207`; the genuine box ran 110+111).
  Advertising a 3rd display is the HIGHEST teardown risk (incomplete display dict →
  RECORD `-17483`); validate against a hand-authored 2-cluster Simulator config first.

### 2. Static multiple view areas per stream
- `displays[].viewAreas[]` is a LIST; multiple entries = selectable layouts (e.g. wide full-screen +
  narrow split-screen), each with its own `safeArea`. Identified by **array index** (no `viewAreaID`
  on the wire). [CAP]/[SIM]
- Sibling keys on the display: **`initialViewArea`** (starting index) and **`adjacentViewAreas`** (array
  of indices you may transition to; `[]` = no runtime switching). The genuine box sent one area,
  `initialViewArea:0`, `adjacentViewAreas:[]`. [CAP]
- Per-view-area keys: `originXPixels/originYPixels/widthPixels/heightPixels`, **`viewAreaTransitionControl`**
  (bool — participates in animated resize), **`viewAreaStatusBarEdge`** (int — which edge the CarPlay
  status bar pins to for this area; 2023 status-bar override), and the nested `safeArea`. [CAP]
- Simulator ships multi-area templates (`Widescreen Instrument Cluster.yaml` main=2 areas, cluster=3;
  `Portrait.yaml` main=2). ncm currently emits a single-element array only. [SIM]
- Implementation cost (LOW risk): `info.rs::view_areas()` return N entries; set `initialViewArea` +
  `adjacentViewAreas`; `vehicle_config.rs` already parses multiple `viewAreas[]` (only `[0]` consumed
  today). Additive; risk only if an index is out of range or a safeArea exceeds the panel.

### 3. Dynamic resizing / runtime view-area switching (DOCUMENT-ONLY here; for the Android port)
- **Command (accessory → iPhone):** `AirPlayReceiverSessionViewAreaUpdate(session, displayUUID CFString,
  uint32 viewAreaIndex, uint32 transitionDuration_ms, uint32 flags, const uint32* rect[x,y,w,h],
  completion, ctx)` (ordinal `0x0001f2b8`, in the Simulator's `CarPlaySDK.framework`), which emits the
  `/command` **`kAirPlayCommand_RequestViewArea`**. This is "please switch to view area N over this
  duration." [SIM]/[SDK] (the 3 uint32 ordering is [I]).
- **Callback (iPhone → accessory):** **`handleViewAreaDimensionsChanged:toRect:`** — during the ~3 s
  animated resize iOS reports the CURRENT encoded-image rectangle so the vehicle re-lays-out its own
  chrome in lock-step. The encoded size is NOT in frame framing and NOT a separate /command — it is this
  session callback. [SIM]
- **Trigger:** the "Always Available" resize button is **accessory-side** (Simulator `__selectedViewAreaPicker`,
  `currentViewAreaIndices`, `SessionClusterViewAreaPopoverView`); it drives `RequestViewArea`. Capability
  declared by the accessory via the same `viewAreas` / `supportsViewAreas` feature already negotiated in
  SETUP. Only areas listed in `adjacentViewAreas` may be requested. [SIM]
- Distinct nearby feature (do NOT couple): **Focus Transfer** (`viewAreaSupportsFocusTransfer` /
  `enablesFocusTransfer`) — knob focus moving between CarPlay and native UI; not required for resize.
- **Local Simulator coverage:** all three (2nd cluster/112, static multi-area, runtime ViewAreaUpdate)
  are present in the Xcode 27 Simulator and testable there — except exercising type-112 needs a
  hand-authored 2-cluster config (no stock template ships two clusters). [SIM]

### 4. Recommended order if/when implemented
1. Static multi-view-area (LOW risk) — additive `/info`, Simulator-testable with a stock template.
2. Runtime `ViewAreaUpdate` switching (MED risk) — needs the command emitter + the dimensions-changed
   inbound callback; only meaningful with non-empty `adjacentViewAreas`. **← the Android-port feature.**
3. 3rd stream / 2nd cluster type-112 (HIGH risk, unverified on CCPA wire) — new `displays[]` entry +
   `session.rs` 112 SETUP arm + new sink; validate on a 2-cluster Simulator config before the CCPA.

---

## Video — loss detection and recovery

<!-- absorbed: ../carplay/06_AV_PIPELINE.md -->

How the box↔host video path detects and recovers from a lost frame, mapped to Apple's own CarPlaySDK
behavior. Governing rule (user): **adhere to the SDK; any deviation must be verified and justified.**

### What Apple's CarPlaySDK does (evidenced, from the `CarPlaySDK` binary)
The screen stream is real-time RTP with active loss recovery — NOT buffered playback, NOT loss-ignorant:
- **RTP with per-packet sequence numbers** — `_receiveBufferedRTP`, `APSRTPPacketHandler`,
  `AirPlayReceiverSessionScreenProcessStreamQ`, `Final packet length … withoutRTPHeader …`.
- **NACK retransmission** of lost packets from a bounded buffer — `### Abort retransmit`,
  `### Aborting retransmits <= %u`, `### No free retransmit nodes, dropping retransmit of seq %u#%u`,
  `### Retransmit seq %u not found`.
- **Loss tracking, recovered vs unrecovered** — `### Burst packet loss %u-%u`,
  `### Unrecovered packets: %u-%u`, `RTP Buffer: %3d ms` (a millisecond-scale reorder/retransmit
  window, not a playback jitter buffer for video).
- **Flow control** — `Flow control exceeded max`.
- **forceKeyFrame** as the last-resort resync when a loss is unrecoverable.

Apple's layered order: **sequence every packet → detect a gap → retransmit to recover → if
unrecoverable, drop that frame and `forceKeyFrame` to repaint.**

### Our mapping (adhere / deviate + justification)
| Apple mechanism | Our implementation | Adheres / Deviates |
|---|---|---|
| Per-packet RTP **sequence number** | Per-**frame** `seq` (u64) in the box's forward wrapper | **Deviates (granularity), justified:** our decode + decrypt unit is the whole frame, and OCBM reassembles each frame reliably. The box no longer drops as policy — task #33 landed and it gates the seam read instead — so `seq` is not a drop-tracker: it is the **decrypt-counter resync** and the recovery marker after a seam teardown. That matters because the live loss mode is the 2 s `SO_SNDTIMEO` on the box→app seam write, which can tear a message mid-write — exactly why each forwarded message carries `SEAM_MAGIC` for the host to re-align on. Frame-level `seq` + `SEAM_MAGIC` detect the loss that remains; packet-level would be finer than our architecture needs. |
| **Flow control** (limit outstanding) | Reliable forward + backpressure to the iPhone | **Adheres — LANDED** (task #33; the per-seam `gated` test in ocbmd's poll-build loop, `ccpa/ocbmd/src/main.rs`, `for (idx, (s, ch)) in d.av_conns.iter().enumerate()` → `let gated = match *ch`). Each video lane is limited to one outstanding frame: ocbmd stops reading that seam until its queue drains, blocking airplayd's screen thread, which stops it reading the iPhone's screen socket and closes the phone's TCP window. Two caveats: **video only** (audio is UDP-sourced and ungated, so no backpressure can reach its sender), and **bounded at ~2 s** by the seam's `SO_SNDTIMEO`, past which airplayd tears the seam down and requests a keyframe. That iOS responds by lowering its encode rate is the design expectation and is **not yet measured**. Apple flow-controls; the stock firmware sustained 4K@60 this way. |
| **NACK retransmit** (recover the packet) | Deferred | **Deviates, justified:** retransmit needs a bidirectional low-latency NACK channel + a sender retransmit buffer — a large transport addition. Apple's own fallback for *unrecovered* loss is `forceKeyFrame`, which we implement. seq + keyframe = Apple's unrecovered-loss path. Retransmit is a documented follow-on. |
| **forceKeyFrame** on unrecovered loss | Box relays a keyframe request when the host detects a gap (reuses `events::send_force_key_frame`) | **Adheres.** |
| Loss tracking / logging | Box logs drops; host logs seq gaps | **Adheres.** |

### Split (box = forwarder, host = recovery) — per the committed architecture
- **Box (airplayd):** stamps a per-frame `seq` in the forward wrapper (the RTP-seq equivalent; the box
  already tracks this counter). Relays a `forceKeyFrame` to the iPhone when signaled (only the box holds
  the encrypted event channel + keys). The box never decodes/buffers/caches — it sequences what it forwards.
- **Box (ocbmd):** ~~forwards/drops at **whole-frame** boundaries… (parse the seam length prefix;
  commit-or-drop a whole frame, never mid-frame fragments)~~ — **NOT IMPLEMENTED, and superseded.** ocbmd
  never parses a seam length prefix: its `Kind::AvConn` poll arm (`ccpa/ocbmd/src/main.rs`) reads raw
  bytes into `avbuf` (a `MAX_PAYLOAD` = 64 KiB buffer) and emits each chunk as its own `F_SOM|F_EOM` OCBM
  frame, so it stays a dumb byte-forwarder. The torn-frame case is handled host-side by `SEAM_MAGIC`
  re-alignment instead (see item 3 below), which also survives a USB hiccup rather than only a
  backpressure drop.
- **Host (OCBMAVDecrypt):** reads `seq`; a gap ⇒ a frame was lost ⇒ (1) resync the decrypt counter to
  `seq` (no more permanent desync), (2) discard any partial frame, (3) signal the box to `forceKeyFrame`.
  The decoder repaints on the next IDR. All detection/resync/refresh logic is host-side.

### Wire format
- Fwd-enc key message: `[len u32 BE][SEAM_MAGIC "SEAV" 4B][0x00][key.output 32B][scid 8B LE]`
  (corrected 2026-08-01: was missing the `SEAM_MAGIC` field between the length prefix and the marker
  byte — session.rs `SEAM_MAGIC`/`km`).
- Fwd-enc **frame** message: `[len u32 BE][SEAM_MAGIC "SEAV" 4B][0x01][seq u64 LE][hdr 128B][body]`
  (corrected 2026-08-01: same missing `SEAM_MAGIC` field) ← `seq` added; hdr+body
  (the iPhone's frame, encrypted) pass through byte-for-byte, untouched.
- Host→box keyframe request: an OCBM control sub-frame the box relays to `events::send_force_key_frame`.

### Phased implementation
1. **Core — IMPLEMENTED + hardware-validated at 4K@60 (2026-07-10; the header STATUS banner is
   authoritative — this line previously read "builds; pending 4K hardware test", corrected 2026-08-16).**
   Box stamps `SEAM_MAGIC` + per-frame `seq`
   in the fwd-enc wrapper (`session.rs`); host re-aligns on the magic after a torn packet and sets its
   decrypt counter from `seq` (`OCBMAVDecrypt` — `nextVideoMessage`/`resyncVideoToMagic`/`drainVideo`).
   Eliminates both failure modes (torn framing + counter desync). Chose a **self-delimiting magic frame**
   over ocbmd whole-frame drops so it also survives a torn packet from a USB hiccup, not just a
   backpressure drop — verified/justified deviation from the docs/carplay/06_AV_PIPELINE.md draft (host re-aligns, ocbmd
   unchanged and stays a dumb byte-forwarder).
2. **Keyframe relay — IMPLEMENTED.** Host gap → `OCBMClient.requestKeyframe` (throttled ≤1/500 ms) →
   `CH_INPUT[INPUT_KEYFRAME]` → ocbmd relay → airplayd `events::send_force_key_frame` → iOS. Prompt repaint.
3. **Efficiency (separate track):** reliable forward + backpressure so genuine drops are rare (stock
   firmware ran 4K@60 with no drops on this hardware; 72% CPU idle / load 0.90 when we were dropping).
4. **Follow-on (fuller SDK parity):** NACK retransmit to recover the lost frame before falling back to keyframe.

---

## cornerMasks — protocol and wire format

<!-- absorbed: ../carplay/06_AV_PIPELINE.md -->

**Status: Phase 1 device-proven.** The accessory advertises `cornerMasks`, the session survives, and
iOS streams the corner-mask bitmap. Off by default; armed primarily by the app config switch
`accessoryConfig.enablesCornerMasks` (§4), with the env `CARPLAY_CORNERMASKS` as a dev override for
testing without the app. Verified on
iOS 27.0 build `24A5390f` (iPhone18,4), wireless transport, at both 1920×1080 and 1280×480.

Commits: `6a4d2db` (advertise + capture), `71ba56b` (Apple-faithful `/info` cleanup).

---

### 1. What cornerMasks is

`cornerMasks` declares that **the accessory** will handle corner masking, so iOS should hand off corner
responsibility. It is a **post-2017 feature** — absent from the licensed R14G17 SDK, so the authority
here is the iOS 27 image + `CarPlaySDK.framework` disassembly + the live wire.

**What it actually changes on screen (device A/B, 2026-08-02):**
- **OFF (default):** iOS rounds its *own* UI corners at the Apple radius and **black-fills** out to the
  sharp video-frame corner — it assumes it owns the corner shape.
- **ON:** iOS **stops** rounding/black-filling and **extends the wallpaper to the full rectangle**,
  delegating the corner shape to the accessory. It also ships a **coverage bitmap** (`topLeftCornerMask`)
  — the per-pixel corner shape the accessory may optionally apply to mask/curve the decoded frame to a
  physically rounded/curved panel (or leave full-bleed).

So `cornerMasks` does NOT add a curve to the received video — it *removes* iOS's own curve+black-fill and
gives the accessory the shape to do its own masking.

The phone always offers it: the SETUP-request `features` array includes `cornerMasks` (sibling of
`viewAreas`, `focusTransfer`). The accessory opts in.

### 2. Declaration — TWO required sites (device + disasm confirmed)

1. **SETUP response `enabledFeatures`** must contain the string `"cornerMasks"` (the master
   enable). iOS logs `displayCornerMasksEnabled = 1` when it reads it back. → `session.rs`.
2. **`/info` `displays[]` — a per-SCREEN boolean `cornerMasks: true` on the DISPLAY dict** (sibling of
   `widthPixels`/`heightPixels`/`viewAreas`), NOT inside a `viewAreas` entry. → `info.rs::build_info`
   (main display), gated by `crate::levers::cornermasks()`.

**The validator (`carEndpoint_checkCarPlayFeatureAcceptance`, inlined in `carEndpoint_Activate…`,
`AirPlaySender` arm64e) reads the flag ONLY from `displays[]`** — offsets from
`~/Downloads/ios27_extract_24A5390f/split/AirPlaySender`:
- enumerates `infoDict["displays"]` (`@0x2516cfd60`); per display dict it tests the `"cornerMasks"` key
  and sets a run-wide SEEN flag (`@0x2516cfff8`). Same object also yields `widthPixels`/`viewAreas`/… →
  it is the display dict, not a viewArea entry.
- FAIL `"cornerMasks flag not set for any view."` fires when the feature is DECLARED but SEEN==0
  (`@0x2516d03c4`). **One display with the flag is enough** (the SEEN flag is set-only, never reset).
- Header corroboration (iOS 27 extract): `CARScreenInfo.h:20 _Bool wantsCornerMasks` (screen-level);
  `CARScreenViewArea.h:36` merely takes `wantsCornerMasks:` as an init param fed down from the screen —
  the viewArea never parses it from its own dict. (`_TtC6CarKit12CRCornerMask.h` is a pure factory.)

#### Mutual exclusion — REAL and per-display
A display that declares `cornerMasks` must **not** carry a `safeArea` in its viewArea, or the validator
hard-fails `"cornerMasks flag set but a safeArea defined in viewAreas."` (`@0x2516d0030`). So on the
cornerMasks display we **omit** `safeArea` (`info.rs::view_areas`, `masks` branch). The exclusion is
scoped to the display carrying the flag — the alt (type-111) display keeps its `safeArea` and no
`cornerMasks` key, which is fine.

An earlier receiver-side note claimed `safeArea` is always emitted and there is no exclusion. The
sender-side validator disassembly and live hardware both say otherwise — dropping `safeArea` is what
makes it pass. Trust the validator.

#### Why they're exclusive — two strategies for one problem
`safeArea` and `cornerMasks` are two **opposite** solutions to the *same* problem: the physical panel is
**not a clean rectangle** (a curve, notch, cutout, or overlapping cluster obscures part of the decoded
frame). Per display you pick one:
- **`safeArea` = avoidance.** "Part of this rectangle will be physically obscured — keep *interactive* UI
  inside this smaller safe rectangle." iOS **insets** its tappable layout (wallpaper may still bleed out
  via `drawUIOutsideSafeArea`, but nothing touchable lands in the bad region).
- **`cornerMasks` = masking/delegation.** "Render the whole rectangle full-bleed; *I* (the accessory)
  will cut it to the panel's real shape using the bitmap you give me." iOS renders full and hands off.

They give contradictory instructions for the same corner region (inset-and-leave vs render-and-trim), so
declaring both on one display is nonsense — hence the hard-fail. Different displays can each choose (our
alt/cluster keeps `safeArea`; the main takes `cornerMasks`).

#### Real-world reference: Ford Sync 4+
Ford Sync 4+ ships `cornerMasks` for **windowed** (non-fullscreen) CarPlay: it declares the feature, iOS
renders full-bleed, Sync applies the corner cutout, and CarPlay appears as a **rounded card floating**
over Ford's card-style OS UI — no black-fill-to-sharp-corner. This is the canonical shipping-OEM example
of exactly the pipeline implemented here, and the model for our host-side Phase 3 (§6).

### 3. The mask — wire format (captured off the live wire)

Delivered in the **type-110 screen-stream SETUP dict**, key **`topLeftCornerMask`** (NOT a runtime
SET_PARAMETER, NOT the video ES). Value = an **8-bit grayscale PNG**, one corner (top-left), **mirror to
all four corners** accessory-side. `hasAlpha: no` — **luma is the coverage** (not an alpha channel).
Absent / NSNull = no mask.

Size scales with resolution (proportional to display **width**, ≈ **5.31%**):

| display    | mask PNG | bytes |
|------------|----------|-------|
| 1920×1080  | 102×102  | 4163  |
| 1280×480   | 68×68    | 3906  |

(102/1920 = 68/1280 = 0.05313.) A resolution change does **not** break the feature — iOS recomputes the
mask for the new radius. The accessory-side `CarPlaySDK` never decodes the buffer (it `memcpy`s the
opaque `CFData` to the app delegate `handleCornerMaskDataReceived:maskBuffer:length:`), which is why the
byte format is defined solely by the sender and had to be captured, not read from any SDK.

### 4. Arming (config-driven toggle — device-verified)

**Primary: the macOS Settings switch "Corner masks (cutout)"** (Settings ▸ Configuration). It writes
`accessoryConfig.enablesCornerMasks: true` into the pushed YAML; `airplayd` reads it via
`VehicleConfig::corner_masks_enabled()` and calls `levers::set_cornermasks(...)` (same pattern as
`set_viewareas`). **Persistent** — the app re-pushes config on every connect, so it survives box reboots
(unlike `/tmp`). Verified end-to-end 2026-08-02: toggle ON → wallpaper extends to full frame + iOS
streams `topLeftCornerMask`; toggle OFF → Apple radius + black corner fill.

**Dev override:** the env `CARPLAY_CORNERMASKS` still force-arms it —
`set_cornermasks(vc.corner_masks_enabled() || env)`. It is injected at both spawn sites
(`tools/session_supervisor.sh`, `crates/vendor/wireless/src/av.rs`) when the on-box file
`/tmp/cornermask_test` exists (tmpfs → cleared on reboot). Use only for testing without the app.

- `CARPLAY_CORNERMASK_CAPTURE=<dir>` — debug only: dumps `topLeftCornerMask` to
  `<dir>/cornermask_<route>.bin` + logs `len`/magic; also dumps the served `/info`
  (`served_info.bplist`). → `server.rs`. Independent of arming.

### 5. How it was cracked (reusable method)

The AirPlay/CarPlay negotiation logs are os_log `<private>` and **not** in the legacy syslog relay, so
`idevicesyslog` shows nothing useful. What worked:
1. Install Apple's **CarPlay/AirPlay logging profile** on the iPhone (developer.apple.com
   profiles-and-logs) to unredact + raise the level.
2. Capture the **unified** log with **`pymobiledevice3 syslog live`** (uses `os_trace_relay`; captures
   os_log — `idevicesyslog` does not). Grep for `cornerMask` / `displayCornerMasksEnabled` /
   `carEndpoint_…`. This surfaced iOS's exact rejection string.
3. Box side, while the macOS app owns OCBM: drive commands over the **UART root console**
   (`/dev/cu.usbserial-0001`, 115200) — helper `scratchpad/uart_cmd.sh`; deploy over UART with
   `tools/uart_push.sh` (or `ocbm-host push` with the app closed).

The confirmed rejection `carEndpoint_checkCarPlayFeatureAcceptance failed: cornerMasks flag not set for
any view` is what pinpointed the display-vs-viewArea placement in one shot — "ask the phone, don't
bisect."

### 6. Phase 3 — floating rounded card on the macOS host (DONE)

With the toggle ON, iOS ships **full-bleed wallpaper** and hands us the corner shape, so the corner
treatment is ours. The host now renders CarPlay as a **rounded card floating on the desktop** (the Ford
Sync 4+ treatment): the video windows are **non-opaque** and the video layer is clipped to Apple's
corner curve, so the cut corners show the desktop through.

We do NOT compute a `cornerRadius` — the mask is a continuous-curvature **squircle**, not a circular arc
(its coverage ramps over ~15px, not a 1–2px tangent), which is *why* Apple ships a bitmap. Instead
`host/.../App/CornerMask.swift` uses Apple's **actual `topLeftCornerMask` bitmap** (bundled as the
`carplay_corner_mask` asset) and builds a full-frame alpha mask — opaque center, all four corners cut to
the exact curve, each corner scaled to `width × 0.0531`. NOTE (superseded by §6c below): this
width-fraction was a two-point fit (1280/1920) and is NOT exact at other resolutions — iOS sizes the mask
by physical `screenScale`, not width (see §6c). The bundled bitmap is now the **fallback only**; the
primary path renders iOS's actual streamed mask, forwarded box→host. `CarPlayView`/`AltVideoView` install
it in `layout()`; the mask is 4-fold symmetric so image orientation is a non-issue.

Note: the radius is Apple's, so on a very **wide/short** aspect (e.g. 1280×480, 2.67:1) the curve is
proportionally large by design; a 16:9-ish resolution reads subtle like Ford. A deliberate deviation
(smaller-than-Apple) would just scale `CornerMask.cornerFraction`.

### 6c. Phase 3c — render iOS's STREAMED mask, forwarded box→host (DONE, device-verified 2400×960)

Phase 3 (§6) rendered the corner from the BUNDLED `carplay_corner_mask` asset scaled by a fixed
`cornerFraction = 68/1280 ≈ 0.0531`. That matched iOS at 1280/1920 but **diverged at higher resolutions**
(e.g. 2400×960 the app's curve cut INTO the CarPlay UI), because the width-fraction model is wrong:

- **iOS sizes the mask by `screenScale`, not width.** iOS-27 `+[CRCornerMask
  cornerMaskDataWithScreenScale:]` = `34pt × pointScale`, a DISCRETE value (102px @3× / 68px @2×) keyed to
  the display's **physical size (mm) + pixels** (`CRDisplayScaleInfo`/`CRScreenScaleHeuristics`), quantized
  to 2×/3×. So the corner is 68 or 102 px — NOT `0.0531·width` (which gives 127 at 2400, hence the cut-in).
- **No accessory-side geometry exists.** CarPlaySDK/CINEMO/SpeedPlay all treat `topLeftCornerMask` as
  opaque bytes (Apple's SDK `memcpy`s it to `handleCornerMaskDataReceived:` — see §3). The curve law lives
  only on the iPhone, so the exact answer is to render iOS's own streamed bitmap — not to reverse a formula.

**The fix (commit eac9b09):** the box now FORWARDS iOS's streamed `topLeftCornerMask` to the host and the
app renders from it:
- Box: `server.rs::forward_corner_mask` extracts the PNG from the type-110 SETUP dict AND the runtime
  SET_PARAMETER (`carEndpoint_updateDisplayCornerMasks`), gated on `levers::cornermasks()`, and sends it
  over `CH_METADATA` marker **`0x04 META_CORNERMASK` = `[u32 BE display_width][PNG]`** via
  `iap2_core::metadata::emit_cornermask` (try_lock, control-thread safe). `ControlServer` carries the
  advertised main-display width (a `.display_width(w)` builder set from `DeviceConfig.display_width`).
- App: `CornerMask.swift` (`@MainActor`) installs the streamed corner (`setSessionCorner`) and sizes it
  EXACTLY as `cp = view_width × (png_n / display_width)` — reproducing iOS's discrete 68/102 at any
  resolution. `MetadataWindow` decodes 0x04; `CarPlayView`/`AltVideoView` re-apply on arrival (it can land
  after first `layout()`); cleared on session reset. The **bundled asset + `cornerFraction` remain the
  FALLBACK** for the (rare) case iOS sends no mask.

This supersedes §6's earlier "the bundled bitmap reproduces iOS's exact curve at any resolution — no
box→host forwarding needed" claim. Device-verified 2026-08-07: corners match CarPlay at 2400×960; 1280/1920
unchanged; non-cornerMask sessions byte-identical (nothing forwarded).

FUTURE (not done): a no-image fallback that COMPUTES the curve from Apple's law (`34pt × pointScale`, shape
captured once) — requires an empirical resolution sweep to pin the 2×↔3× threshold and confirm the squircle
shape; tracked separately. Today's fallback is still the fixed-fraction bundled asset.

---

## Input — touch / HID uplink

<!-- absorbed: ../carplay/06_AV_PIPELINE.md -->

Host → box → iPhone input. Grounded in the CarPlay-SDK ground truth (docs/carplay/03_SDK_GROUND_TRUTH.md §8) and an audit of the
existing box + host code. **Headline: most of the machinery already exists on both ends; the gap is the
transport between them.**

### 1. Ground truth (docs/carplay/03_SDK_GROUND_TRUTH.md §8, Apple CarPlaySimulator)
- **Uplink command:** `AirPlayReceiverSessionSendHIDReport(session, uuid, report, len)` → encrypted
  `POST /command` `{type:"hidSendReport", uuid:<hex>, hidReport:<bytes>}` on the event channel.
- **Coordinate space:** absolute **16-bit little-endian in `[0, LogicalMaximum]`**, NOT normalized.
  `LogicalMaximum` = the advertised display resolution. No report-ID prefix; devices keyed by `uuid`.
- **Report layouts:** single-touch **5 B** `[tip][Xlo Xhi][Ylo Yhi]` (down: tip=1+coords; move: tip=1;
  up: tip=0). Multi-touch **12 B** `[0][tip0][X0lo Xhi][Y0..][1][tip1][X1..][Y1..]`. Media buttons 1 B `[index]`.
- **Binding:** the touchscreen `hidDevices[]` entry's `displayUUID` must equal the display's `uuid`, and
  its descriptor's X/Y Logical Maximum = the display resolution.

### 2. What already exists (audit)

#### Box — airplayd already SENDS touch; it just has no input source
- **`receiver/hid.rs`** (compiled into airplayd, not gated): `touch_report(buttons,x,y)` → the exact 5-B
  `[buttons][x LE16][y LE16]`; `touch_report_normalized(buttons,nx,ny,w,h)` scales 0..1 → absolute; media
  buttons too. Unit-tested (LE order, clamping).
- **`receiver/events.rs`** (compiled in, not gated): `send_hid_report(uid, report)` → the exact
  `{type:"hidSendReport", uuid, hidReport}` `POST /command` over the encrypted event channel. The event
  channel is wired at RECORD (`events::setup`) — **confirmed live** this session (`session-focus handshake
  sent` uses the same channel).
- **`receiver/info.rs`** already advertises the touchscreen HID descriptor (`uuid=1`) with X/Y logical max
  = the config resolution and `displayUUID` = the display — and since task #5 that tracks the pushed
  VehicleConfig resolution. *(CORRECTED 2026-08-16 — this said "2400×960 now". That was a one-session
  bench config, not the shipped value: the macOS app defaults to **1920×1080**
  (`VehicleConfig.swift:38-39`) and the box's app-less fallback is **1920×720** (`info.rs`, the
  `display_width` default). 2400×960 survives only as a receiver unit-test constant and the
  2026-07-10 capture.)* Media-buttons descriptor (`uuid=2`) is advertised too.
- **Missing:** anything that *feeds* `send_hid_report`. The only ingest in receiver_core is
  `uplink.rs::handle_touch`, fed by a local TCP control-in (`:9110`) — and `uplink.rs` is behind the
  `mic-uplink` feature (it also holds the eld mic encoder), which airplayd builds **without**. So airplayd
  has the touch *emitter* but not the *ingest*.
  **CORRECTED 2026-08-16 — true when written, false now, and the gap is closed.** The feature gate moved:
  airplayd builds `receiver` with `default-features = false` plus its own default `mic-uplink-eld`
  (`ccpa/airplayd/Cargo.toml`), which turns `mic-uplink` ON, so `uplink.rs` IS compiled in today. And the
  touch ingest never landed in `uplink.rs` at all: airplayd owns its own dependency-free HID seam that
  binds `127.0.0.1:9110` directly (`ccpa/airplayd/src/main.rs`, log line `HID input ingest on
  127.0.0.1:9110 (task #20)`), exactly as §3 planned.

#### Host — CarPlayView already reads local touch/trackpad input; it just goes nowhere
- **`CarPlayView.swift`** converts mouse down/drag/up, trackpad two-finger scroll, pinch/magnify, and the
  keyboard into **normalized 0..1** coordinates, letterbox/videoRect-aware, Y already flipped to top-left
  origin (HID convention). Emits via `CarPlayViewDelegate`: `didMultiTouch` (single), `didMultiTouchTwo`
  (two points w/ ids), `didTouch` (AA), `didPressCommand`.
- **`AppDelegate`** implements the delegate but routes to the **legacy** `adapter?.sendMultiTouch(...)` —
  the original carlink USB protocol. In OCBM/CarPlay mode `adapter` is nil, so **touch is captured and
  dropped**. `ocbmClient` has no input path.
  **FIXED 2026-07-25 (task #20):** `AppDelegate.carPlayView(_:didMultiTouch:x:y:)` now guards on
  `ocbmClient` and calls `client.sendTouch(phase:nx:ny:)`. Still open: the two-finger delegate
  `carPlayView(_:didMultiTouchTwo:)` is an **empty stub**. Its in-code comment ("the OCBM box doesn't
  advertise a multi-touch HID descriptor yet") is now only half true — the box HAS the descriptor (§5
  Phase 2) but still advertises it only when `levers::multi_touch()` is on, and the app pushes no
  `hidConfig.touchScreenSupportsMultiTouch`, so under every app-pushed config today it is not advertised.

#### The gap (only this) — ALL FOUR CLOSED (2026-07-25, task #20)
1. ~~No host→box **OCBM input channel**.~~ `CH_INPUT = 0x0030` + `INPUT_TOUCH` (`crates/ocbm-proto/src/lib.rs`).
2. ~~No **ocbmd → airplayd** relay for input.~~ `INPUT_INGEST_ADDR = "127.0.0.1:9110"` (`ccpa/ocbmd/src/main.rs`).
3. ~~No **airplayd ingest** feeding `send_hid_report`.~~ airplayd's own `:9110` listener →
   `hid::touch_report_normalized` (scaled by the `DISPLAY_WH` cell that `load_device_config` sets) →
   `events::send_hid_report`.
4. ~~Host delegate routes to the legacy adapter, not OCBM.~~ `AppDelegate.carPlayView(_:didMultiTouch:x:y:)`
   → `OCBMClient.sendTouch`.

### 3. Design decisions
- **Scaling rides the pushed config's resolution (box renders app values).** Host sends **normalized**
  coords (u16 fixed-point 0..65535); airplayd scales with `touch_report_normalized` using the SAME
  resolution it advertised in `/info` — itself the app-pushed VehicleConfig value (from
  `load_device_config`, task #5). One resolution authority, can't drift; per docs/carplay/04_CAPABILITIES_AND_CONFIG.md the box is
  rendering an app-authored value here, not owning a policy. (Tension noted: docs/host/00_MACOS_HOST_APP.md Tier-2 #8 wants
  host-side touch aspect derived from the decoded frame — any move there follows the docs/carplay/04_CAPABILITIES_AND_CONFIG.md
  earned-fallback path.)
- **Reuse the emitter as-is.** `hid.rs` + `events.rs::send_hid_report` are done and correct — no changes.
- **Do NOT un-gate `uplink.rs` for touch.** It drags in eld/mic. Add a tiny, dependency-free `input`
  ingest in airplayd instead (calls the existing `hid`/`events`). (Mic will reuse the transport later —
  see §5 — un-gating only the PCM path then.)
- **MVP = single-touch** (tap/drag/swipe): covers the core interaction with the descriptor already
  advertised. Multi-touch (pinch/two-finger) needs the 12-B multi-touch descriptor added to `/info`
  first — Phase 2. (Which HID descriptors `/info` advertises is a `hidConfig` value from the pushed
  config per docs/carplay/04_CAPABILITIES_AND_CONFIG.md — the app selects the set; the box advertises it.)

### 4. Plan — Phase 1 (single-touch MVP)
1. **`ocbm-proto`**: add `CH_INPUT = 0x0030` (host→box) with sub-frames `[kind u8][…]`:
   `INPUT_TOUCH(0x01) = [phase u8][nx u16 LE][ny u16 LE][finger u8]` (nx/ny normalized 0..65535,
   phase 0=down/1=move/2=up). (Reserve `INPUT_MBUTTON`, `INPUT_CMD` for later phases.)
2. **`ocbmd`**: on `CH_INPUT` frames, relay the payload to airplayd over a local socket (connect to
   `127.0.0.1:9110` on demand, mirror of the A/V seam but reverse). Small, in the existing poll loop.
3. **`airplayd`**: new `input` module — a process-lifetime `TcpListener 127.0.0.1:9110`; parse
   `INPUT_TOUCH`; `buttons = (phase==up ? 0 : 1)`; `report = hid::touch_report_normalized(buttons,
   nx/65535, ny/65535, W, H)`; `events::send_hid_report(1, &report)`. `W,H` from a shared cell updated by
   `load_device_config` (so scaling == advertised `/info`). No eld, no `uplink.rs`.
4. **Host**: `OCBMClient.sendTouch(phase,nx,ny,finger)` frames `CH_INPUT`; `AppDelegate.carPlayView(
   didMultiTouch:)` calls it when `ocbmClient != nil` (keep the legacy `adapter` path for the old protocol).
5. **Validate on hardware**: tap a CarPlay button → registers; drag/scroll a list → tracks; confirm no
   stuck touch on drag-out (the host already clamps to the video edge on up).

### 5. Follow-on phases (after MVP) — status 2026-08-16
- **Phase 2 — multi-touch + gestures: BOX SIDE DONE (hardware-verified 2026-08-15); the macOS HOST is the
  only gap.** The box advertises the 12-byte two-finger descriptor
  (`info.rs::touchscreen_multi_descriptor`, gated by `levers::multi_touch()` ←
  `hidConfig.touchScreenSupportsMultiTouch` / `CARPLAY_MULTITOUCH`), builds the report
  (`hid.rs::touch_report_multi` / `touch_report_multi_normalized`, unit-tested against Apple's fill
  order) and reassembles the one-finger-per-`INPUT_TOUCH` wire framing into a single two-contact report
  (airplayd's `CONTACTS` slots + `contact_slot`; a third finger is dropped, as Apple's descriptor holds
  two). The **Android** host already drives it end-to-end (`CarlinkAndroid`'s `CarlinkManager` sends one
  `INPUT_TOUCH` per pointer with the finger id). **The remaining gap is the macOS host:**
  `AppDelegate.carPlayView(_:didMultiTouchTwo:)` is an empty stub, so pinch / two-finger scroll are still
  captured and dropped there.
- **Phase 3 — buttons/commands: DONE** (task #35). `INPUT_MEDIA_BTN` (media buttons, HID uid 2),
  `INPUT_NAV` (D-Pad, uid 3, flag-gated), `INPUT_KNOB` (uid 4), `INPUT_TELEPHONY` and `INPUT_COMMAND` all
  ride `CH_INPUT`; Siri is dispatched box-side as the `/command` `requestSiri`. See
  `crates/ocbm-proto/src/lib.rs` and `AppDelegate.carPlayView(_:didPressCommand:)`.

### 6. Microphone (next task, same transport) — DONE (2026-08-16 status)
**Shipped as its own channel and its own seam, not as a `CH_INPUT` sibling:** `CH_MIC = 0x0031` → ocbmd
`forward_mic` → airplayd's dedicated `MIC_INGEST_ADDR = "127.0.0.1:9112"`, kept separate from the `:9110`
HID seam so a mic fault cannot disturb working HID. The "un-gate just the PCM path" route below was not
the one taken either — airplayd builds `mic-uplink-eld`, so both the wired PCM leg and the wireless
AAC-ELD encoder are compiled in. The original plan text is kept below as the record.

Mic is the same host→box→iPhone shape, audio instead of HID, and the box code already exists in
`uplink.rs` (`mic <len>\n<pcm>` → RTP → iPhone). **Wired mic = raw PCM 16 kHz mono big-endian, no eld
encoder** (only wireless AAC-ELD needs eld), so airplayd can do it **without** the eld dependency by
un-gating just the PCM path. Reuse the CH_INPUT sibling (or `CH_MIC`) → ocbmd → airplayd → the PCM uplink
framing. Host mic capture already exists (`MicCapture.swift`). This is a clean Phase-4 once the input
transport (§4) is proven.

### 7. Touchpoints (files)
- `crates/ocbm-proto/src/lib.rs` — `CH_INPUT` + sub-frame consts.
- `ccpa/ocbmd/src/main.rs` — relay `CH_INPUT` → `127.0.0.1:9110`.
- `ccpa/airplayd/src/main.rs` (+ small `input` fn) — ingest → `hid`/`events`; share `W,H` from `load_device_config`.
- Host `OCBM/OCBMClient.swift` (send), `App/AppDelegate.swift` (delegate → OCBM), reuse `CarPlayView.swift` as-is.
- Emitter unchanged: `receiver/hid.rs`, `receiver/events.rs`, `receiver/info.rs`.
