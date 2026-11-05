# Contributing to InputWeave

Thank you for helping. Bug reports, ideas and pull requests are all welcome. Before you write code, please read this page: it covers how the project is built and tested, the rules that keep it healthy, and the contributor license agreement every code contribution needs.

## Before you start

- **Bugs and ideas:** open an [issue](https://github.com/tasict/InputWeave/issues) using one of the templates. For anything larger than a fix, describe what you want to change first, so we can agree on the approach before you spend time on it.
- **Security problems:** don't open a public issue; see [SECURITY.md](SECURITY.md).
- **The architecture** is described in [`docs/architecture.md`](docs/architecture.md). The parts that matter most when changing code:
  - The background service (`inputweave`, Rust) does the work; the apps (SwiftUI, WinUI 3, GTK 4) are clients of its IPC and hold no business logic. A new feature is a new IPC `Command`/`Event`, implemented in the service, then shown by each app. The one exception is the license check with Polar, which each app makes itself because it affects no feature.
  - `crates/iw-core` is pure state machines: no sockets, threads, timers or OS handles, and time is passed in.
  - Platform APIs appear only in `crates/iw-platform`, `crates/iw-winsvc` and the named pipe transport in `crates/iw-ipc`. Logic in a platform backend that doesn't touch FFI lives in modules that build on every platform, so its tests run everywhere.
  - Protobuf schemas (`proto/`) change only in backward-compatible ways within the `v1` packages.
  - Never leave a key stuck or the user without control: heartbeat timeouts, disconnects and shutdown release everything.

## Building and testing

### The Rust workspace (all platforms)

Rust stable.

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Run a single test with `cargo test -p iw-core router::tests::hops_directly_between_remote_devices`. To try the protocol and routing without touching your real keyboard and mouse, run an agent with `backend.kind = "mock"` and drive it with `inputweave ctl` (the configuration format is at the top of `crates/inputweave/src/config.rs`).

After changing the key code tables or their generator, run `cargo xtask keymap`. After changing `proto/`, `iw-proto` regenerates its code by itself; a change to `app.proto` also needs the macOS app's Swift code regenerated with `cargo xtask swift-proto <protoc-gen-swift>` (see `apps/macos/README.md`).

Windows code can be type-checked on another OS with `scripts/windows-check.sh x86_64-pc-windows-msvc` (see `scripts/windows-check/README.md`); passing it only means it compiles.

### macOS app

Xcode 26 and [XcodeGen](https://github.com/yonaskolb/XcodeGen).

```sh
xcodegen generate --spec apps/macos/project.yml
xcodebuild -project apps/macos/InputWeave.xcodeproj -scheme InputWeave test
xcodebuild -project apps/macos/InputWeave.xcodeproj -scheme InputWeaveSnapshots test   # screenshots of every state
apps/macos/scripts/package.sh [output folder]                                           # the .pkg
```

Capturing and injecting input needs the Accessibility and Input Monitoring permissions, granted to the agent ("InputWeave Agent"), and acts on your real session: try such changes deliberately. The app never calls APIs that trigger permission prompts itself; the agent detects the permissions and reports them.

### Windows app

Windows, the .NET 9 SDK, and WiX 5 for the installer (see `apps/windows/README.md`).

```bat
dotnet build apps\windows\InputWeave.sln -c Release
dotnet test apps\windows\InputWeave.sln -c Release
powershell -ExecutionPolicy Bypass -File apps\windows\scripts\package.ps1
```

`dotnet publish` trims .NET and WinUI, so a change that builds may still fail in the published app: run the published app (the MSI's) after changing the app.

### Linux app and packages

The Linux backends, the GTK app (`apps/linux`, a workspace of its own) and the `.deb` packages are built and tested in two Ubuntu 24.04 containers, driven by `scripts/linux-container.sh` (see [`scripts/linux-container/README.md`](scripts/linux-container/README.md); docker can run locally or on another machine over ssh).

```sh
scripts/linux-container.sh create
scripts/linux-container.sh run 'cargo test --workspace'
scripts/linux-container.sh run sh scripts/linux-x11-tests.sh       # X11 tests on Xvfb
scripts/linux-container.sh run sh scripts/linux-wayland-tests.sh   # a fake portal, a libei server and headless sway
scripts/linux-container.sh run 'cd apps/linux && CARGO_TARGET_DIR=/target/app cargo test'
scripts/linux-container.sh deb && scripts/linux-container.sh check-deb
```

### The website

`site/` is plain HTML, CSS and JavaScript. `scripts/site-build.sh <output folder>` copies it with its images, stamps asset URLs with their hashes and fails on broken links; serve the output folder with any static server to preview it.

## What a pull request needs

- `cargo fmt`, `cargo clippy -- -D warnings` and the tests pass for what you changed; platform code is built and tested on its platform (CI runs Linux, macOS and Windows).
- New behavior has tests. Logic that can be tested without the OS is tested without the OS.
- **Code comments are in English**, as are commit messages and public documentation.
- **Commit messages** follow the existing history: a short subject in plain English that says what changes for the user or the code (for example "Keep the activation when the same license key is entered again"), and a body that explains why when it isn't obvious.
- **Strings and translations:** English is the source language; every user-visible string also has a Traditional Chinese (zh-TW, Taiwan usage) translation. The catalogs are `apps/macos/InputWeave/Localizable.xcstrings`, `apps/windows/InputWeave/Strings/{en-US,zh-TW}/Resources.resw` and `apps/linux/core/strings/{en,zh-TW}.txt`; each app's tests check that both languages have every string. If you can't write the zh-TW text, say so in the pull request.
- **Dependencies** must have permissive licenses (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode); no GPL, LGPL, AGPL or MPL dependencies. `deny.toml` holds the allowlist and CI runs `cargo deny check`. The Linux app's workspace isn't covered by it, so check its new crates by hand.
- **The UI** follows the platform: system fonts, controls and semantic colors. The brand is limited to the seam (the stitched line between screens) and five color tokens (`docs/brand/README.md`).

## Clean-room rule

Don't copy, translate or adapt code from lan-mouse, Deskflow, Synergy, Barrier or TwinDesk, and don't read their source while working on the same feature here. Reading their documentation and comparing behavior is fine. These projects are under other copyright holders' licenses; keeping their code out keeps InputWeave free of their terms and keeps its copyright clear, which the contributor license agreement below relies on.

## Contributor license agreement

Code contributions need the [contributor license agreement](CLA.md) (individual or entity). When you open your first pull request, a bot asks you to sign it by posting a comment in the pull request; your signature is recorded once and covers later contributions. The agreement lets the project's maintainer distribute your contribution under GPL-3.0-or-later and also under other terms, for example in builds made for other platforms; you keep the copyright in your work.

## Code of conduct

Be kind and patient, assume good intent, and keep discussions about the work. Harassment of any kind isn't tolerated; the maintainer may remove comments or block participants who don't follow this.
