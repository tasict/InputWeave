# Screenshots

The PNGs here are made by `apps/macos/scripts/make-screenshots.sh`; don't edit them by hand. The script runs the `InputWeaveSnapshots` scheme, which draws the app's screens offscreen with sample data, and copies the ones below here. To update them, change the screens or the sample data in `apps/macos/InputWeaveSnapshots`, then run from the repository root:

```sh
apps/macos/scripts/make-screenshots.sh
```

The screens are in English, the app's source language.

| File | What it shows |
|---|---|
| `panel.png`, `panel-dark.png` | The menu bar panel, light and dark |
| `panel-controlling.png` | The menu bar panel while controlling another computer |
| `arrangement.png`, `arrangement-dark.png` | The main window's Arrangement page, light and dark |
| `arrangement-pictures.png`, `arrangement-pictures-dark.png` | The Arrangement page with desktop pictures behind the screens (the sample pictures are gradients drawn at run time), in a wider window so the keyboard and mouse badge has room for its text |
| `pairing-code.png` | The pairing code on the computer being paired |
| `add-device.png` | Add Device: the computers nearby that can be paired |
| `status-icon-2x.png` | The menu bar icon's five states, on a dark (top) and a light (bottom) menu bar, at 2x scaled up 4 times |
| `about.png`, `about-dark.png` | The About InputWeave window, light and dark |

- The dark versions (`*-dark.png`) are for GitHub's dark theme and only appear as the dark source of a `<picture>` (with `prefers-color-scheme`).
- The panel, the pairing code and Add Device have transparent backgrounds; `panel-controlling.png`, `pairing-code.png` and `add-device.png` have no dark version.
- The welcome page of first-run setup isn't included: the test runner has no app icon, so that snapshot shows a generic folder icon.
