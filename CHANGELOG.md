# Changelog

All notable changes to InputWeave are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/). Each release's full notes are in [`docs/release-notes/`](docs/release-notes/README.md) and on its [GitHub release](https://github.com/tasict/InputWeave/releases).

## [1.0.1] - 2026-11-06

### Fixed

- A crash, or rarely memory corruption, of the background service: Tokio 1.54.0 could free an `Interval`'s timer while the runtime was firing it (RUSTSEC-2026-0384). Now Tokio 1.54.1.
- Non-constant-time mask generation in `ctutils` (RUSTSEC-2026-0385). Now 0.4.8.

### Changed

- Linux: zbus 6 and ashpd 0.14 for D-Bus and the desktop portals; zbus 5's `zvariant` and `zbus_names` are no longer maintained.
- The website has stable addresses for the user guide and for reporting a problem, which the release notes link to.

## [1.0.0] - 2026-11-05

The first public release.

### Added

- Sharing one keyboard and mouse between macOS, Windows and Linux computers on the local network: an arrangement of the computers as they sit on the desk, seams where screens meet, a screen-edge glow when the pointer crosses, take back control (⌃⌥⇧⎋ / Ctrl + Alt + Shift + Esc), lock to the current computer, jump to a computer by shortcut or from the panel's map, waiting or holding a modifier before switching, changeable shortcuts, and relative mouse for games.
- Clipboard sync for plain and formatted text and images, large content sent when pasted, files copied in Finder or File Explorer pasted on another computer, and secret content from password managers left out.
- Sending files and folders: drag and drop on the arrangement, Send Files, Windows' Send to, dragging across the edge between a Mac and a PC, a queue, resuming after a dropped connection, and a transfer history.
- Pairing with a 6-digit code; mutual TLS over QUIC with pinned certificates; discovery over mDNS.
- The Windows service, which lets the sign-in screen and UAC prompts be used from another computer.
- Native apps: a SwiftUI menu bar app (macOS), a WinUI 3 notification area app (Windows) and a GTK 4 / libadwaita app (Linux, X11 and Wayland), in English and Traditional Chinese.
- Installers: a notarized `.pkg` for macOS, an MSI for Windows x64, and `.deb` packages for amd64 and arm64.
- Updates from GitHub: Sparkle on macOS, the app's updater on Windows (checked against InputWeave's signature), and an APT package source on Linux.
- An optional license (US$10, up to 5 computers) that shows "Licensed" instead of "Trial" in Settings and About; every feature works without it.
- `inputweave ctl`, the command line for the background service.

[1.0.1]: https://github.com/tasict/InputWeave/releases/tag/v1.0.1
[1.0.0]: https://github.com/tasict/InputWeave/releases/tag/v1.0.0
