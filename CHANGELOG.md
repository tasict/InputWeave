# Changelog

All notable changes to InputWeave are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/). Each release's full notes are in [`docs/release-notes/`](docs/release-notes/README.md) and on its [GitHub release](https://github.com/tasict/InputWeave/releases).

## [Unreleased]

### Added

- Cursor speed for each other computer: in the inspector on the Arrangement and Devices pages, a slider from 50% to 300% makes the pointer move faster or slower on that computer while this one controls it (for large or high-resolution screens). It is this computer's own setting, kept until the computer is removed; relative mouse for games isn't affected. Also `inputweave ctl device <id> --speed <percent>`.
- Single-key shortcuts: take back control and lock to the current computer can be one key that typing doesn't use, F13 to F19, Scroll Lock or Pause (on a Mac F13 to F19, F14 being Scroll Lock on Apple's extended keyboard). Also `inputweave ctl hotkeys set --take-back F13`.
- Double-press Control to take back control: an option in Settings › General › Shortcuts, off by default. While controlling another computer, pressing the same Control key twice quickly, with nothing else in between, brings the keyboard and mouse back; the presses still reach that computer. It does nothing in relative mouse, where games often use Control. Also `inputweave ctl hotkeys set --take-back-double double+control`.
- Wake a sleeping computer over the network (Wake-on-LAN): each computer tells the others how to wake it once it connects, and they keep it. An offline computer that can be woken has a moon after its name and a Wake button in the inspector (and in its right-click menu on the Arrangement page); its status says how long it has been offline, then "Waking…" for up to two minutes, with a notice once it is awake, or a note with a link to the user guide if it didn't wake up. An option in Settings › General › Switching, off by default, wakes a sleeping computer when you push the pointer against the edge toward it, at most every 30 seconds. Works only with wake for network access turned on there, usually over a wired network; the user guide says how on each system. Also `inputweave ctl wake <id>` and `inputweave ctl wake --push on`.
- Send files from the file manager: on a Mac, Finder's Services › Send with InputWeave; on Linux, Dolphin's Actions › Send with InputWeave (KDE Frameworks 5 and 6), which runs the new `inputweave-app --send <files>`. With one computer that can receive them connected they go at once, with a note saying where; otherwise InputWeave asks which.
- Offline for now: Take Offline in a computer's inspector or right-click menu on the Arrangement page (Take This Computer Offline in the panel's ⋯ menu and the tray menu) keeps that computer apart from the whole group until someone brings it back: no computer controls it or is controlled by it, and the clipboard and files go neither to it nor from it. Every computer shows it as "Offline for now" and who set it; if it was being controlled at that moment, the keyboard and mouse come back at once with nothing left pressed on it. Any computer can bring it back, the one taken offline included, and it stays so across restarts. Every computer in the group needs this version: one still on 1.0 doesn't know about it, and the inspector says so. Also `inputweave ctl offline <id> on|off`.
- Profiles (Settings › Profiles): named sets of settings this computer picks by the network it is on, or that you pick: whether it shares at all, which computers it connects to, and whether the clipboard and files go. Default and Other networks (used on every network not in another profile) are always there; updating puts the network you're on in Default, so nothing changes until you change something. A paused profile keeps this computer off the network entirely: it isn't announced, connects to no computer, and none can connect to it. Computers not checked in the profile in use see this computer as offline and show here as "Not in this profile". Networks are recognized by their router, with no permission needed; optionally, on macOS and Windows, also by their Wi-Fi name once you allow the app location access (on Linux always). The panel's title shows the profile in use, the ⋯ menu and tray menu switch it, and a notice says when it changes by itself. Also `inputweave ctl profile`.

### Changed

- Where InputWeave asks which computer files go to (Windows' Send to and Send Files…, Linux's Send Files…), computers that can't receive them are listed too, greyed out with why: offline, or file transfer with that computer turned off. The list follows them while it is open, and files sent meanwhile join it. With no computer connected, it says "No connected computer can receive files."

### Fixed

- Changing shortcuts from two apps at once, or from the command line while an app changes them, no longer times out.

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
