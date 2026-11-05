# InputWeave for macOS

The menu bar app (SwiftUI) and the packaging of the background service.

- `project.yml`: the Xcode project, generated with [XcodeGen](https://github.com/yonaskolb/XcodeGen).
- `InputWeave/`: the app. It only talks to the background service over IPC (`Agent/`), never links Rust, and never calls APIs that need TCC permissions.
- `InputWeave/Generated/app.pb.swift`: Swift for `proto/inputweave/app/v1/app.proto`, made by `cargo xtask swift-proto`.
- `Agent/`: the helper bundle's `Info.plist` and the LaunchAgent plist that `SMAppService.agent` registers.
- `scripts/embed-agent.sh`: an Xcode build phase that builds `inputweave` with Cargo and embeds it as `Contents/Library/LoginItems/InputWeave Agent.app`.
- `scripts/make-brand-assets.swift`: draws the app icon, the logos (see `docs/brand/`), the installer package's light and dark backgrounds, `Installer/Resources/background*.tiff`, and the DMG window's background, `Installer/dmg/background.tiff`.
- `InputWeaveTests/`: unit tests for the app's logic. `InputWeaveSnapshots/`: renders the main views offscreen into `$TMPDIR/InputWeaveSnapshots`, for looking at layouts.
- `scripts/make-screenshots.sh`: runs `InputWeaveSnapshots` and copies the images the READMEs and the user guide show into `docs/screenshots/`.

## Build

```sh
xcodegen generate --spec apps/macos/project.yml
xcodebuild -project apps/macos/InputWeave.xcodeproj -scheme InputWeave build
xcodebuild -project apps/macos/InputWeave.xcodeproj -scheme InputWeave test
xcodebuild -project apps/macos/InputWeave.xcodeproj -scheme InputWeaveSnapshots test
```

The Rust toolchain must be installed (`~/.cargo/bin`). Builds are signed to run locally; release builds use the Developer ID identity, which must stay the same across releases so macOS keeps the permissions granted to the background service.

`scripts/package.sh` makes the installer package, the only way to install and update the app. `Installer/` holds what goes into it: `Distribution.xml`, the install and removal scripts, and `Resources/` with the welcome pages and the choices' translations (`en.lproj`, `zh_TW.lproj`) and the backgrounds. A DMG is made only with `INPUTWEAVE_DMG=1`; its window is laid out through Finder, so that needs a logged-in session, and `INPUTWEAVE_DMG_LAYOUT=skip` makes a plain DMG without the layout for headless machines.

## Regenerating code

After changing `proto/inputweave/app/v1/app.proto`:

```sh
# protoc-gen-swift from swift-protobuf, the same version as in project.yml
git clone --depth 1 --branch 1.39.0 https://github.com/apple/swift-protobuf
swift build -c release --package-path swift-protobuf --product protoc-gen-swift
cargo xtask swift-proto swift-protobuf/.build/release/protoc-gen-swift
```
