# Third-party material and reference points

The code in this repository is original work with one exception: `baseline/`, which contains
files taken from LIVI (https://github.com/f-io/LIVI, GPL-3.0-or-later): four verbatim kernel
patches and several modified or ported files, listed below. Other external sources were used as
*reference* only — to learn a wire format, a constant, or a behaviour — and are recorded here so
the distinction is on the record.

## Licence scope

The repository is released under the Unlicense (`LICENSE`), except `baseline/`, which is
GPL-3.0-or-later as a whole (`baseline/LICENSE`). Within it, `baseline/arm-imx6ul/dts/*` keep
GPL-2.0 as Linux kernel derivatives (SPDX tags kept). The four files in
`baseline/arm-imx6ul/patches/*` carry a GPL-3.0-or-later SPDX header (the added header above the
diff); their diff bodies are GPL-2.0 kernel derivatives. The GPL-2.0 text is
`baseline/COPYING-GPL-2.0`.

The programs `baseline/build.sh` fetches (Linux, musl, BusyBox, dropbear, the OpenSSH
sftp-server, mtd-utils, exfatprogs, libnl, iw, wpa_supplicant, BlueZ, ncurses, readline,
wireless-regdb) keep their own licences and are not stored in this repository; the pinned
tarballs are listed in `baseline/common/versions.env`. Radio firmware the build or a unit uses
(linux-firmware blobs, AIC8800 and other vendor firmware) is not covered by this notice; its
redistribution terms are its own and have not been assessed here.

The rest of the repository does not link against `baseline/` and is distributed alongside it as
an aggregate. A built image (kernel, initramfs, rootfs) is GPL-covered: whoever distributes one
must provide the corresponding source (this tree at the build commit, the pinned tarballs in
`baseline/common/versions.env`, and the licence texts) or a written offer for it valid for three
years (GPL-3.0 section 6, GPL-2.0 section 3).

## Based on LIVI

Based on LIVI by Lasse Heitgres (https://github.com/f-io/LIVI)

LIVI is Copyright (C) 2025-2026 Lasse Heitgres, GPL-3.0-or-later; its kernel patches and DTS are
Linux derivatives under GPL-2.0. It was read at version 9.5.0; later upstream versions exist. The
files below were taken into `baseline/` and modified on 2026-10-19 and 2026-10-20. Each carries a
header with the attribution line, its LIVI source path, its modification date and what changed
(except `etc/passwd` and `etc/group`, which have no comment syntax).

Verbatim:
- `baseline/arm-imx6ul/patches/0001` to `0004` (an added header above the diff; the diff bodies
  are LIVI's, byte for byte)
- `baseline/common/rootfs/etc/passwd`, `baseline/common/rootfs/etc/group`

Modified:
- `baseline/arm-imx6ul/initramfs/init`, `flash-kernel`, `flash-rootfs`, `flash-lib.sh`
- `baseline/common/rootfs/init`, `etc/inittab`, `etc/profile`, `etc/init.d/rcS`
- `baseline/common/shared/net-up`, `net-down`, `mtd.sh`
- `baseline/arm-imx6ul/dts/imx6ull-ccpa-baseline.dts`, `radio-iw416.dtsi`, `radio-rtl8822bs.dtsi`,
  `radio-rtl8822cs.dtsi` (GPL-2.0 kept)
- `baseline/build.sh`
- `baseline/arm-imx6ul/kernel/base.fragment`
- `baseline/common/busybox/recovery.applets`, `full.fragment`
- `baseline/arm-imx6ul/rootfs/usr/sbin/radio` (derived from LIVI's `livi-radio`)

Ported (Rust to Python):
- `baseline/arm-imx6ul/tools/stage_kernel.py`

Following LIVI's structure or values only (no LIVI text taken; these carry no attribution line):
- `baseline/common/recipes/userspace.sh` (musl and BusyBox-rescue structure)
- `baseline/arm-imx6ul/rootfs/etc/baseline/board-rcS.sh` (LED pins and polarity from LIVI's DTS)
- `baseline/arm-imx6ul/kernel/radio-modules.fragment` (config symbols from LIVI's `build.sh`)
- `baseline/common/versions.env` (some version pins are LIVI's)

The rest of `baseline/` is original work, licensed with the directory under GPL-3.0-or-later.

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

**Gradle wrapper** (`host/CarlinkAndroid/gradlew`, `gradle-wrapper.properties`,
`gradle-wrapper.jar`, and `host/CallSim/gradlew`) — Apache-2.0, unmodified, retaining its own
license header.

## Linked at build time, not vendored

**libfdk-aac** (Fraunhofer FDK AAC Codec Library) — the AAC-ELD encoder for the CarPlay mic
uplink links against it (`crates/vendor/eld-codec`, behind `receiver`'s `mic-uplink-eld` feature,
which `ccpa/carplayd` enables by default). Its
source is not included here and is built separately. Binaries you distribute that link it
carry Fraunhofer's own license and notice requirements, which this dedication does not and
cannot waive.

## Patents

This dedication waives copyright. It grants no patent rights, and none are implied. Media
codecs (AAC, H.264, HEVC) and accessory authentication in this domain are patent-encumbered
independently of copyright; that is the user's responsibility to assess.
