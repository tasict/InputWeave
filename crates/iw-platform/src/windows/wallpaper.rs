//! Which picture file each display shows, and the previews made of them,
//! for the reader in `desktop_pictures`. Free of FFI so it is tested on
//! every OS.
//!
//! The shell's `IDesktopWallpaper` names monitors by device path and gives
//! each one's rectangle; the agent names displays by device name
//! (`\\.\DISPLAY1`), from `EnumDisplayMonitors`. The two are matched by
//! rectangle, both in physical pixels since the reader's process is per
//! monitor DPI aware, as the agent is.

#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::monitors::{Monitor, PixelRect};

/// A monitor as the shell's desktop wallpaper object reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShellMonitor {
    /// Where it is on the virtual screen, in physical pixels.
    pub rect: PixelRect,
    /// The picture file it shows; empty if the shell names none for it.
    pub wallpaper: String,
}

/// The picture file each display shows, by display id.
///
/// Each display gets the shell monitor at exactly its rectangle. Should
/// that leave one display and one attached shell monitor without a match,
/// they are the same monitor, whatever their rectangles say. A shell
/// monitor without a picture of its own shows `shared`, the picture every
/// monitor shows when they show the same one (as when it spans them all).
/// Displays showing neither, which is a solid color, are left out, as are
/// shell monitors with an empty rectangle: the shell remembers monitors
/// that are no longer attached.
pub(crate) fn files_by_display(
    monitors: &[Monitor],
    shell: &[ShellMonitor],
    shared: &str,
) -> BTreeMap<String, PathBuf> {
    let mut left: Vec<&ShellMonitor> = shell
        .iter()
        .filter(|s| s.rect.width() > 0 && s.rect.height() > 0)
        .collect();
    let mut pairs = Vec::new();
    let mut unmatched = Vec::new();
    for monitor in monitors {
        match left.iter().position(|s| s.rect == monitor.pixels) {
            Some(i) => pairs.push((monitor, left.remove(i))),
            None => unmatched.push(monitor),
        }
    }
    if let ([monitor], [shell]) = (unmatched.as_slice(), left.as_slice()) {
        pairs.push((monitor, shell));
    }
    pairs
        .into_iter()
        .filter_map(|(monitor, shell)| {
            let file = if shell.wallpaper.is_empty() {
                shared
            } else {
                &shell.wallpaper
            };
            (!file.is_empty()).then(|| (monitor.id.clone(), PathBuf::from(file)))
        })
        .collect()
}

/// Which file a picture comes from, as far as telling a change goes.
#[derive(Clone, Debug, PartialEq)]
struct Source {
    modified: Option<SystemTime>,
    len: u64,
}

impl Source {
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            modified: meta.modified().ok(),
            len: meta.len(),
        })
    }
}

/// The previews made so far, by picture file, so a picture is decoded and
/// encoded again only when its file's modification time or length
/// changed, and once however many displays show it.
#[derive(Debug, Default)]
pub(crate) struct Previews {
    made: HashMap<PathBuf, (Source, Option<Vec<u8>>)>,
}

impl Previews {
    /// The previews of the pictures in `files`, by display id. `make`
    /// previews a file, or returns `None` if it cannot; a file that is
    /// gone has no preview either. Files no display shows any more are
    /// forgotten.
    pub(crate) fn update(
        &mut self,
        files: &BTreeMap<String, PathBuf>,
        mut make: impl FnMut(&Path) -> Option<Vec<u8>>,
    ) -> BTreeMap<String, Vec<u8>> {
        self.made
            .retain(|path, _| files.values().any(|f| f == path));
        let mut out = BTreeMap::new();
        for (display, path) in files {
            let Some(source) = Source::of(path) else {
                self.made.remove(path);
                continue;
            };
            let jpeg = match self.made.get(path) {
                Some((made_from, jpeg)) if *made_from == source => jpeg.clone(),
                _ => {
                    let jpeg = make(path);
                    self.made.insert(path.clone(), (source, jpeg.clone()));
                    jpeg
                }
            };
            if let Some(jpeg) = jpeg {
                out.insert(display.clone(), jpeg);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(id: &str, left: i32, top: i32, width: i32, height: i32) -> Monitor {
        Monitor {
            id: id.to_owned(),
            pixels: PixelRect::new(left, top, left + width, top + height),
            scale: 1.0,
            primary: left == 0 && top == 0,
        }
    }

    fn shell(left: i32, top: i32, width: i32, height: i32, wallpaper: &str) -> ShellMonitor {
        ShellMonitor {
            rect: PixelRect::new(left, top, left + width, top + height),
            wallpaper: wallpaper.to_owned(),
        }
    }

    fn files(pairs: &[(&str, &str)]) -> BTreeMap<String, PathBuf> {
        pairs
            .iter()
            .map(|(id, file)| ((*id).to_owned(), PathBuf::from(file)))
            .collect()
    }

    #[test]
    fn displays_get_the_picture_of_the_shell_monitor_at_their_rectangle() {
        // A 4K monitor at 150% left of the primary, and a portrait one
        // above it; the shell lists them in another order.
        let monitors = [
            monitor(r"\\.\DISPLAY1", 0, 0, 1920, 1080),
            monitor(r"\\.\DISPLAY2", -3840, -500, 3840, 2160),
            monitor(r"\\.\DISPLAY3", 0, -1920, 1080, 1920),
        ];
        let listed = [
            shell(0, -1920, 1080, 1920, r"C:\Pictures\tall.jpg"),
            shell(
                0,
                0,
                1920,
                1080,
                r"C:\Windows\Web\Wallpaper\Windows\img0.jpg",
            ),
            shell(-3840, -500, 3840, 2160, r"C:\Pictures\wide.png"),
        ];
        assert_eq!(
            files_by_display(&monitors, &listed, ""),
            files(&[
                (
                    r"\\.\DISPLAY1",
                    r"C:\Windows\Web\Wallpaper\Windows\img0.jpg"
                ),
                (r"\\.\DISPLAY2", r"C:\Pictures\wide.png"),
                (r"\\.\DISPLAY3", r"C:\Pictures\tall.jpg"),
            ])
        );
    }

    #[test]
    fn a_solid_color_has_no_file() {
        let monitors = [
            monitor(r"\\.\DISPLAY1", 0, 0, 1920, 1080),
            monitor(r"\\.\DISPLAY2", 1920, 0, 1920, 1080),
        ];
        let listed = [shell(0, 0, 1920, 1080, ""), shell(1920, 0, 1920, 1080, "")];
        assert!(files_by_display(&monitors, &listed, "").is_empty());
    }

    #[test]
    fn a_monitor_without_a_picture_of_its_own_shows_the_shared_one() {
        let monitors = [
            monitor(r"\\.\DISPLAY1", 0, 0, 1920, 1080),
            monitor(r"\\.\DISPLAY2", 1920, 0, 1920, 1080),
        ];
        let listed = [
            shell(0, 0, 1920, 1080, ""),
            shell(1920, 0, 1920, 1080, r"C:\b.jpg"),
        ];
        assert_eq!(
            files_by_display(&monitors, &listed, r"C:\span.jpg"),
            files(&[
                (r"\\.\DISPLAY1", r"C:\span.jpg"),
                (r"\\.\DISPLAY2", r"C:\b.jpg")
            ])
        );
    }

    #[test]
    fn detached_and_unknown_monitors_are_left_out() {
        let monitors = [
            monitor(r"\\.\DISPLAY1", 0, 0, 1920, 1080),
            monitor(r"\\.\DISPLAY2", 1920, 0, 1920, 1080),
        ];
        // A monitor the shell remembers but that is not attached has an
        // empty rectangle; nothing the shell lists sits where DISPLAY2 is.
        let listed = [
            shell(0, 0, 0, 0, r"C:\old.jpg"),
            shell(0, 0, 1920, 1080, r"C:\a.jpg"),
            shell(5000, 0, 1280, 1024, r"C:\c.jpg"),
        ];
        // One display and one attached shell monitor are left: the same
        // monitor, its rectangle seen differently.
        assert_eq!(
            files_by_display(&monitors, &listed, ""),
            files(&[
                (r"\\.\DISPLAY1", r"C:\a.jpg"),
                (r"\\.\DISPLAY2", r"C:\c.jpg")
            ])
        );

        // Two of each left: no telling which is which.
        let monitors = [
            monitor(r"\\.\DISPLAY1", 0, 0, 1920, 1080),
            monitor(r"\\.\DISPLAY2", 1920, 0, 1920, 1080),
        ];
        let listed = [
            shell(0, 0, 1280, 720, r"C:\a.jpg"),
            shell(1280, 0, 1280, 720, r"C:\b.jpg"),
        ];
        assert!(files_by_display(&monitors, &listed, "").is_empty());
        // Nothing listed at all.
        assert!(files_by_display(&monitors, &[], r"C:\span.jpg").is_empty());
    }

    #[test]
    fn a_picture_is_made_once_until_its_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        let b = dir.path().join("b.jpg");
        std::fs::write(&a, b"first").unwrap();
        std::fs::write(&b, b"other").unwrap();
        let shown: BTreeMap<String, PathBuf> = [
            ("1".to_owned(), a.clone()),
            ("2".to_owned(), a.clone()),
            ("3".to_owned(), b.clone()),
        ]
        .into();
        let mut previews = Previews::default();
        let made = std::cell::RefCell::new(Vec::new());
        let mut make = |path: &Path| {
            made.borrow_mut().push(path.to_path_buf());
            let bytes = std::fs::read(path).ok()?;
            // The other picture cannot be read.
            (bytes != b"other").then_some(bytes)
        };
        let first = previews.update(&shown, &mut make);
        assert_eq!(
            first,
            [
                ("1".to_owned(), b"first".to_vec()),
                ("2".to_owned(), b"first".to_vec())
            ]
            .into()
        );
        // Unchanged, nothing is made again, not even what failed.
        assert_eq!(previews.update(&shown, &mut make), first);
        // A file of another length is.
        std::fs::write(&a, b"second!").unwrap();
        let second = previews.update(&shown, &mut make);
        assert_eq!(second["1"], b"second!");
        assert_eq!(*made.borrow(), vec![a.clone(), b.clone(), a.clone()]);

        // A file that is gone has no preview, and one shown again is made
        // again.
        std::fs::remove_file(&b).unwrap();
        let only_b: BTreeMap<String, PathBuf> = [("3".to_owned(), b.clone())].into();
        assert!(previews.update(&only_b, &mut make).is_empty());
        let only_a: BTreeMap<String, PathBuf> = [("1".to_owned(), a.clone())].into();
        assert_eq!(previews.update(&only_a, &mut make)["1"], b"second!");
        assert_eq!(made.borrow().len(), 4);
    }
}
