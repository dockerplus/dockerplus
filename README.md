# DockerPlus

![DockerPlus](assets/brand/rex/brand-sheet.png)

A native macOS app for [Colima](https://github.com/abiosoft/colima) and its Docker engine.
Rust on AppKit with real Liquid Glass. No webview, no background runtime. macOS 26+.

## Features

- **Colima:** start, stop, restart; change CPUs, memory and disk.
- **Containers:** start, stop, restart, pause, remove; live logs; details.
- **Context menu:** shell in Terminal, open a port in the browser, show a bind mount in
  Finder, select a whole Compose project.
- **Images and volumes:** pull, remove, see sizes and which containers use them.
- Filter, sort and multi-select everywhere. Updates live from Docker events.

Shortcuts: `⌘1`–`⌘4` pages, `⌘F` filter, `⌘L` logs, `⌘I` details, `⌘R` refresh.

## Install

Needs Rust, the Xcode Command Line Tools and Colima.

```bash
brew install colima
scripts/bundle.sh
open target/DockerPlus.app
```

The app is ad-hoc signed and not notarized, so it is meant for the Mac that built it.
Open in Terminal also needs the Docker CLI (`brew install docker`).

## Safe next to Docker Desktop and OrbStack

DockerPlus talks only to Colima's socket and starts Colima with `--activate=false`, so
your current Docker context never changes. Run `docker context use colima` to switch.

## Notes

- Closing the window quits the app. Colima keeps running.
- If your macOS user name is also an Ubuntu system account (`man`, `mail`…), DockerPlus
  writes `~/.colima/_lima/_config/override.yaml` so `colima start` does not hang.

## Development

```bash
cargo test
scripts/bundle.sh --features devtools   # adds a socket that drives the UI in tests
```

`src/backend/` holds the Docker API and Colima CLI code, `src/ui/` the AppKit views.
Brand guide: [assets/brand/rex/BRAND.md](assets/brand/rex/BRAND.md).

## Status

Version 0.1, tested on macOS 27.2, Colima 0.10.3 and Docker Engine 29.5.2. Not yet
supported: Colima profiles other than `default`, Compose up and down, networks,
per-container stats, notarization.
