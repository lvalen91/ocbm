# Third-party material and reference points

The code in this repository is original work. Nothing here is copied from another
project. Several external sources were used as *reference* — to learn a wire format, a
constant, or a behaviour — and are recorded here so the distinction is on the record.

## Referenced, not incorporated

**aasdk** (`github.com/f1xpl/aasdk`, GPL-3.0-or-later) — an open-source Android Auto
head-unit library, used as a starting reference for the AA transport. In practice its approach
did not work as-is against a current phone, so the implementation here was arrived at by
iterating variants until the wire behaviour matched, and several of its behaviours end up
deliberately opposed to aasdk's:

- **TLS framing.** aasdk reassembles a fragmented message's *ciphertext* per channel before
  decrypting. Following that shape produced a session that died every 57-94 s with
  `errSSLDecryptionFail`, because one TLS stream is shared by every channel and the phone
  interleaves them mid-message. This implementation decrypts each frame as it arrives and
  accumulates *plaintext* per channel instead.
- **Channel ids** follow the decompiled gearhead app's ordinals, not aasdk's — `SENSOR = 1`
  and `INPUT = 8` here versus `2` and `1` there.
- **Flow control.** The video window is advertised as `max_unacked = 64`; the stop-and-wait
  value wedged video after about two minutes over a higher-latency relay.
- **Protocol version.** 6.1 is requested rather than 1.7, because the phone selects its
  keyframe interval from the version the head unit asks for.

What is genuinely shared is the wire format itself — frame flags FIRST/LAST/BULK, the
encryption and message-type bits — which are protocol facts every correct implementation
carries identically. No aasdk code is present here, and the AA client is Rust and Swift with
no structure in common with aasdk's C++. aasdk's head-unit certificate and key are **not**
distributed with this repository; `host/aa-headunit/README.md` explains where to obtain them.

**gearhead** (the Android Auto phone app) — decompiled to establish protocol facts:
channel ordinals, keyframe-interval behaviour by requested protocol version, the
`InputReport` field layout, and the keycode set the phone echoes back.

**Apple CarPlay Communication Plug-in SDK** — licensed first-party material accessed under
an active Apple Developer Program membership, used to check protocol conformance. It is not
redistributed here: the reference tree is outside this repository and excluded from it. See
`docs/ops/07_AUTHORIZATION.md` for the scope and authorization statement.

## Included third-party files

**Gradle wrapper** (`host/CarlinkAndroid/gradlew`, `gradle-wrapper.properties`) — Apache-2.0,
unmodified, retaining its own license header.

## Linked at build time, not vendored

**libfdk-aac** (Fraunhofer FDK AAC Codec Library) — the AAC-ELD encoder for the CarPlay mic
uplink links against it (`crates/vendor/eld-codec`, behind the `mic-uplink-eld` feature). Its
source is not included here and is built separately. Binaries you distribute that link it
carry Fraunhofer's own license and notice requirements, which this dedication does not and
cannot waive.

## Patents

This dedication waives copyright. It grants no patent rights, and none are implied. Media
codecs (AAC, H.264, HEVC) and accessory authentication in this domain are patent-encumbered
independently of copyright; that is the user's responsibility to assess.
