# OCBM

OCBM is an open, modular protocol that carries CarPlay (iPhone) and Android Auto (Android)
between a phone-facing adapter and a host that decodes and renders. It is not tied to one vendor,
chip or OS: the same protocol runs on different adapters, boards and host platforms.

This repo holds the protocol, an open Rust-first userspace for the Carlinkit CPC200-CCPA (and
other boards), and host apps for a Mac or an Android Automotive head unit.

> **License:** Unlicense (public domain), except baseline/ which is derived from LIVI and is
> GPL-3.0-or-later; see [NOTICE.md](NOTICE.md). Based on LIVI by Lasse Heitgres (https://github.com/f-io/LIVI)

> **Scope & authorization:** licensed Apple/MFi developer, genuine hardware, ordinary accessory development — not security work. See [docs/ops/07_AUTHORIZATION.md](docs/ops/07_AUTHORIZATION.md).

## Scope

OCBM began as an open replacement for the closed protocol in Carlinkit's adapter firmware, and
has grown into a universal one. Carlinkit adapters are one supported target among several; the
protocol can be adapted to other hardware and operating systems.

## What is here

| Directory | What | Details |
|---|---|---|
| `box/` | Daemons that run on the adapter (`ocbmd`, `iap2d`, `carplayd`, `aa-bridge`), the architecture overview, build | [box/README.md](box/README.md) |
| `boards/` | Per-board material: CCPA (i.MX6UL), C2Air, Raspberry Pi 4, Pi Zero 2 W | [boards/ccpa-imx6ul/README.md](boards/ccpa-imx6ul/README.md) |
| `host/` | Host apps and tools: macOS app, Android/AAOS apps, CLI tools | [host/README.md](host/README.md) |
| `baseline/` | Mainline-kernel image recipe for the CCPA (derived from LIVI, GPL) | [baseline/README.md](baseline/README.md) |
| `docs/` | Protocol, design, ops and board documents | [docs/README.md](docs/README.md) |
| `crates/` | Shared Rust crates, including the OCBM wire codec `ocbm-proto` | [layout](docs/README.md#repository-layout) |
| `tools/` | Dev and ops scripts: install, recovery, checks | [tools/README.md](tools/README.md) |

## Start here

- Build, run or understand the box side: [box/README.md](box/README.md)
- Install on a CCPA, recover it, requirements: [boards/ccpa-imx6ul/README.md](boards/ccpa-imx6ul/README.md)
- Host apps (macOS, Android/AAOS): [host/README.md](host/README.md)
- Protocol and design documents, reference material, full layout: [docs/README.md](docs/README.md)
- Roles and the board list: [docs/ocbm/01_ROLES_AND_MFI_SOURCE.md](docs/ocbm/01_ROLES_AND_MFI_SOURCE.md)

## Status

CarPlay and Android Auto both work end to end, wired and wireless, with the macOS and AAOS host
apps. The C2Air and the Pi ports are earlier-stage. Per-feature detail:
[docs/carplay/](docs/README.md#carplay--the-carplay-protocol-and-session) and
[docs/androidauto/01_SESSION_AND_AV.md](docs/androidauto/01_SESSION_AND_AV.md). Open work:
[docs/ops/04_OPEN_ITEMS.md](docs/ops/04_OPEN_ITEMS.md).
