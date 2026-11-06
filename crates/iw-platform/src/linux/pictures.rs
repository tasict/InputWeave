//! Previews of the desktop pictures on a Linux desktop, for the
//! arrangement canvas.
//!
//! - GNOME keeps one picture for every display in GSettings:
//!   `org.gnome.desktop.background picture-uri`, or `picture-uri-dark`
//!   while `org.gnome.desktop.interface color-scheme` prefers dark;
//!   `picture-options` `none` shows a solid colour.
//! - KDE Plasma keeps one for each desktop containment in
//!   `plasma-org.kde.plasma.desktop-appletsrc`: the containment's
//!   `[Wallpaper][org.kde.image][General] Image=`, a file or a wallpaper
//!   package (a folder of sizes), on the screen it was last on
//!   (`lastScreen`). Each activity has containments of its own
//!   (`activityId`): those of the current one count (kactivitymanagerd's
//!   `[main] currentActivity` in `kactivitymanagerdrc`). Plasma's screen
//!   numbers go to displays by `plasmashellrc`'s `[ScreenConnectors]`
//!   (`0=eDP-1`), the connector names that name the displays (the
//!   outputs' names on Wayland, RandR's monitors on X11); without it, as
//!   Plasma 5.27 and later number them: the main display first, then the
//!   others from left to right and top to bottom. A screen whose containment names no image (Plasma's default), uses
//!   another wallpaper plugin, or that has no containment shows none,
//!   never another screen's. Not tried on a real desktop.
//!
//! - Other X11 desktops show the picture a program put on the root window
//!   and named in its `_XROOTPMAP_ID` (feh, nitrogen and the like;
//!   `x11::root_picture`): each display the part of it under that display.
//!   Without one, or of a solid colour, a display shows none.
//!
//! Other Wayland desktops have none (`available` is false). JPEG, PNG and
//! WebP files are read (the `image` crate), anything else counts as none,
//! as an XML slideshow does. A preview is made again only when its file's
//! path, modification time or size changed, or the root window shows
//! another pixmap; the agent asks again as displays change and every 60
//! seconds.

use std::path::{Path, PathBuf};

use iw_core::layout::Display;

/// GNOME's picture for the colour scheme: `None` for a solid colour, or
/// when nothing names a picture.
pub(crate) fn gnome_uri(
    color_scheme: Option<&str>,
    uri: Option<&str>,
    uri_dark: Option<&str>,
    options: Option<&str>,
) -> Option<String> {
    if options == Some("none") {
        return None;
    }
    let dark = color_scheme == Some("prefer-dark");
    let chosen = if dark {
        uri_dark.filter(|u| !u.is_empty()).or(uri)
    } else {
        uri
    };
    chosen.filter(|u| !u.is_empty()).map(str::to_owned)
}

/// A desktop containment in Plasma's appletsrc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KdeDesktop {
    /// The screen the containment was last on.
    pub screen: Option<u32>,
    /// The activity it is of.
    pub activity: Option<String>,
    /// As written: a `file:` URI or a path, to a file or a package; `None`
    /// where it names none (Plasma's default) or another wallpaper plugin
    /// paints it.
    pub image: Option<String>,
}

/// Plasma's desktop containments, in the order of their numbers; not its
/// panels.
pub(crate) fn kde_desktops(appletsrc: &str) -> Vec<KdeDesktop> {
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Containment {
        screen: Option<u32>,
        activity: Option<String>,
        plugin: Option<String>,
        form_factor: Option<String>,
        wallpaper: Option<String>,
        image: Option<String>,
    }
    let mut containments: BTreeMap<u64, Containment> = BTreeMap::new();
    // The containment a section is of, and whether it is the image
    // wallpaper's settings.
    let mut section: Option<(u64, bool, bool)> = None;
    for line in appletsrc.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = section_of(line);
            continue;
        }
        let Some((id, top, image)) = section else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim().to_owned());
        let entry = containments.entry(id).or_default();
        if top {
            match key {
                "lastScreen" => entry.screen = value.parse().ok(),
                "activityId" => entry.activity = Some(value).filter(|v| !v.is_empty()),
                "plugin" => entry.plugin = Some(value),
                "formfactor" => entry.form_factor = Some(value),
                "wallpaperplugin" => entry.wallpaper = Some(value),
                _ => {}
            }
        } else if image && key == "Image" {
            entry.image = Some(value).filter(|v| !v.is_empty());
        }
    }
    containments
        .into_values()
        // A desktop is planar (form factor 0); panels and their system
        // trays are not.
        .filter(|c| {
            c.plugin.is_some()
                && c.plugin.as_deref() != Some("org.kde.panel")
                && c.form_factor.as_deref().is_none_or(|f| f == "0")
        })
        .map(|c| KdeDesktop {
            screen: c.screen,
            activity: c.activity,
            image: c
                .image
                .filter(|_| c.wallpaper.as_deref().is_none_or(|w| w == "org.kde.image")),
        })
        .collect()
}

/// The `key=value` entries of section `[name]` of an ini file.
fn entries<'a>(text: &'a str, name: &str) -> Vec<(&'a str, &'a str)> {
    let header = format!("[{name}]");
    let mut inside = false;
    let mut found = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == header;
            continue;
        }
        if inside && let Some((key, value)) = line.split_once('=') {
            found.push((key.trim(), value.trim()));
        }
    }
    found
}

/// The current activity in `kactivitymanagerdrc`.
pub(crate) fn kde_activity(rc: &str) -> Option<String> {
    entries(rc, "main")
        .into_iter()
        .find(|(key, value)| *key == "currentActivity" && !value.is_empty())
        .map(|(_, value)| value.to_owned())
}

/// Plasma's screen numbers and the connectors they are on, from
/// `plasmashellrc`'s `[ScreenConnectors]`.
pub(crate) fn kde_connectors(plasmashellrc: &str) -> Vec<(u32, String)> {
    entries(plasmashellrc, "ScreenConnectors")
        .into_iter()
        .filter_map(|(number, name)| Some((number.parse().ok()?, name.to_owned())))
        .filter(|(_, name)| !name.is_empty())
        .collect()
}

/// For a section header: the containment it is of, whether it is the
/// containment's own section, and whether it holds the image wallpaper's
/// settings: `[Containments][1]` and
/// `[Containments][1][Wallpaper][org.kde.image][General]`.
fn section_of(header: &str) -> Option<(u64, bool, bool)> {
    let parts: Vec<&str> = header
        .split('[')
        .filter_map(|p| p.strip_suffix(']'))
        .collect();
    if parts.first() != Some(&"Containments") {
        return None;
    }
    let id = parts.get(1)?.parse().ok()?;
    let rest = &parts[2..];
    Some((
        id,
        rest.is_empty(),
        rest == ["Wallpaper", "org.kde.image", "General"],
    ))
}

/// The local path a wallpaper names: a `file:` URI or an absolute path.
pub(crate) fn path_of(image: &str) -> Option<PathBuf> {
    if image.starts_with('/') {
        return Some(PathBuf::from(image));
    }
    crate::linux::clipboard_formats::path_of_uri(image)
}

/// The image of a wallpaper package, a folder holding
/// `contents/images/<width>x<height>.<ext>`: the largest. A file is its own
/// image.
pub(crate) fn image_of(path: &Path) -> Option<PathBuf> {
    if !path.is_dir() {
        return Some(path.to_owned());
    }
    let images = std::fs::read_dir(path.join("contents/images")).ok()?;
    images
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter_map(|p| Some((pixels_named(&p)?, p)))
        .max_by_key(|(pixels, _)| *pixels)
        .map(|(_, p)| p)
}

/// The pixel count a package image's file name gives, as `3840x2160.png`.
fn pixels_named(path: &Path) -> Option<u64> {
    let stem = path.file_stem()?.to_str()?;
    let (w, h) = stem.split_once('x')?;
    Some(w.parse::<u64>().ok()? * h.parse::<u64>().ok()?)
}

/// The one picture every display shows (GNOME).
pub(crate) fn everywhere(displays: &[Display], image: Option<String>) -> Vec<(String, String)> {
    let Some(image) = image else {
        return Vec::new();
    };
    displays
        .iter()
        .map(|d| (d.id.clone(), image.clone()))
        .collect()
}

/// Which picture shows on each display under Plasma: the image of the
/// desktop on the display's screen, in `activity` (when known; the
/// containments of another do not count). A display's screen number is
/// the one `connectors` gives its name; without any, as Plasma 5.27 and
/// later number screens: the main one first, then from left to right and
/// top to bottom. Where the desktops that may be on a screen disagree (the
/// current activity not known, or stale ones), it shows none.
pub(crate) fn kde_assign(
    displays: &[Display],
    desktops: &[KdeDesktop],
    activity: Option<&str>,
    connectors: &[(u32, String)],
) -> Vec<(String, String)> {
    let numbered: Vec<(u32, &Display)> = if connectors.is_empty() {
        let mut ordered: Vec<&Display> = displays.iter().collect();
        ordered.sort_by(|a, b| {
            (!a.primary, a.bounds.x, a.bounds.y)
                .partial_cmp(&(!b.primary, b.bounds.x, b.bounds.y))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        (0..).zip(ordered).collect()
    } else {
        let mut named: Vec<(u32, &Display)> = displays
            .iter()
            .filter_map(|d| {
                let (number, _) = connectors.iter().find(|(_, name)| *name == d.id)?;
                Some((*number, d))
            })
            .collect();
        named.sort_by_key(|(number, _)| *number);
        named
    };
    let current = |d: &&KdeDesktop| match (activity, d.activity.as_deref()) {
        (Some(current), Some(its)) => its == current,
        _ => true,
    };
    numbered
        .into_iter()
        .filter_map(|(number, display)| {
            let mut on = desktops
                .iter()
                .filter(current)
                .filter(|d| d.screen == Some(number))
                .map(|d| d.image.as_deref());
            let first = on.next()?;
            if on.any(|image| image != first) {
                return None;
            }
            Some((display.id.clone(), first?.to_owned()))
        })
        .collect()
}

#[cfg(target_os = "linux")]
mod platform {
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::SystemTime;

    use iw_core::layout::Display;

    use super::{
        everywhere, gnome_uri, image_of, kde_activity, kde_assign, kde_connectors, kde_desktops,
        path_of,
    };
    use crate::linux::common::lock;
    use crate::linux::desktop::{
        Desktop, config_home, gsettings_list, gsettings_string, gvariant_string, listed,
    };
    use crate::linux::x11::root_picture::{RootPicture, Shown, region, solid};
    use crate::{DesktopPictures, PlatformError, preview};

    /// The largest image read, on either side.
    const MAX_SIDE: u32 = 16384;

    /// The desktop's pictures, read for the agent.
    pub(crate) struct LinuxPictures {
        desktop: Desktop,
        /// Previews made, by picture file.
        made: Mutex<HashMap<PathBuf, Made>>,
        /// The root window's picture, on an X11 desktop other than GNOME
        /// and Plasma.
        root: Option<RootPicture>,
        /// Previews made of it, by display.
        root_made: Mutex<HashMap<String, RootMade>>,
    }

    struct Made {
        modified: Option<SystemTime>,
        len: u64,
        jpeg: Option<Vec<u8>>,
    }

    /// A display's preview of the root window's picture, and of what.
    struct RootMade {
        shown: Shown,
        region: (u16, u16, u16, u16),
        jpeg: Option<Vec<u8>>,
    }

    impl LinuxPictures {
        /// The pictures of `desktop`, or of the root window when
        /// `root_picture` (`Info::root_picture`).
        pub(crate) fn new(desktop: Desktop, root_picture: bool) -> Self {
            Self {
                desktop,
                made: Mutex::new(HashMap::new()),
                root: root_picture.then(RootPicture::new),
                root_made: Mutex::new(HashMap::new()),
            }
        }

        /// Each display's part of the root window's picture, made again
        /// only for a display that moved or when the root window shows
        /// another pixmap.
        fn root_pictures(
            &self,
            root: &RootPicture,
            displays: &[Display],
        ) -> HashMap<String, Vec<u8>> {
            let mut out = HashMap::new();
            let mut made = lock(&self.root_made);
            let Some(shown) = root.shown() else {
                made.clear();
                return out;
            };
            made.retain(|id, _| displays.iter().any(|d| &d.id == id));
            for screen in displays {
                let Some(part) = region(screen.bounds, shown.width, shown.height) else {
                    made.remove(&screen.id);
                    continue;
                };
                let jpeg = match made.get(&screen.id) {
                    Some(m) if m.shown == shown && m.region == part => m.jpeg.clone(),
                    _ => {
                        let jpeg = root
                            .read(&shown, part)
                            .filter(|rgb| !solid(rgb.as_raw()))
                            .and_then(|rgb| preview_of_image(&image::DynamicImage::ImageRgb8(rgb)));
                        if jpeg.is_none() {
                            tracing::debug!(
                                display = %screen.id,
                                pixmap = shown.pixmap,
                                "no preview of the root window's picture"
                            );
                        }
                        made.insert(
                            screen.id.clone(),
                            RootMade {
                                shown,
                                region: part,
                                jpeg: jpeg.clone(),
                            },
                        );
                        jpeg
                    }
                };
                if let Some(jpeg) = jpeg {
                    out.insert(screen.id.clone(), jpeg);
                }
            }
            out
        }

        /// The wallpaper image of each display, as written.
        fn images(&self, displays: &[Display]) -> Vec<(String, String)> {
            match self.desktop {
                Desktop::Gnome => {
                    // The background's keys in one process, the colour
                    // scheme in another.
                    const BACKGROUND: &str = "org.gnome.desktop.background";
                    let list = gsettings_list(BACKGROUND).unwrap_or_default();
                    let read = |key: &str| listed(&list, BACKGROUND, key).and_then(gvariant_string);
                    let uri = gnome_uri(
                        gsettings_string("org.gnome.desktop.interface", "color-scheme").as_deref(),
                        read("picture-uri").as_deref(),
                        read("picture-uri-dark").as_deref(),
                        read("picture-options").as_deref(),
                    );
                    everywhere(displays, uri)
                }
                Desktop::Kde => {
                    let read = |name: &str| {
                        std::fs::read_to_string(config_home().join(name)).unwrap_or_default()
                    };
                    let desktops = kde_desktops(&read("plasma-org.kde.plasma.desktop-appletsrc"));
                    let activity = kde_activity(&read("kactivitymanagerdrc"));
                    let connectors = kde_connectors(&read("plasmashellrc"));
                    kde_assign(displays, &desktops, activity.as_deref(), &connectors)
                }
                Desktop::Other => Vec::new(),
            }
        }

        /// The preview of the picture in `file`, made again only if the
        /// file changed.
        fn preview(&self, file: &Path) -> Option<Vec<u8>> {
            let meta = std::fs::metadata(file).ok()?;
            let (modified, len) = (meta.modified().ok(), meta.len());
            let mut made = lock(&self.made);
            if let Some(m) = made.get(file)
                && m.modified == modified
                && m.len == len
            {
                return m.jpeg.clone();
            }
            let jpeg = preview_of(file);
            if jpeg.is_none() {
                tracing::debug!(file = %file.display(), "no preview of the desktop picture");
            }
            made.insert(
                file.to_owned(),
                Made {
                    modified,
                    len,
                    jpeg: jpeg.clone(),
                },
            );
            jpeg
        }
    }

    impl DesktopPictures for LinuxPictures {
        fn pictures(
            &self,
            displays: &[Display],
        ) -> Result<HashMap<String, Vec<u8>>, PlatformError> {
            if let Some(root) = &self.root {
                return Ok(self.root_pictures(root, displays));
            }
            let mut out = HashMap::new();
            let mut used = Vec::new();
            for (display, image) in self.images(displays) {
                let Some(file) = path_of(&image).and_then(|p| image_of(&p)) else {
                    continue;
                };
                if let Some(jpeg) = self.preview(&file) {
                    out.insert(display, jpeg);
                }
                used.push(file);
            }
            lock(&self.made).retain(|file, _| used.contains(file));
            Ok(out)
        }

        fn available(&self) -> bool {
            self.desktop != Desktop::Other || self.root.is_some()
        }
    }

    /// A JPEG preview of the JPEG, PNG or WebP image in `file`, or `None`
    /// for any other file or one no size of which is light enough.
    pub(crate) fn preview_of(file: &Path) -> Option<Vec<u8>> {
        use image::{ImageFormat, ImageReader, Limits};

        let mut reader = ImageReader::open(file).ok()?.with_guessed_format().ok()?;
        if !matches!(
            reader.format(),
            Some(ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP)
        ) {
            return None;
        }
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_SIDE);
        limits.max_image_height = Some(MAX_SIDE);
        reader.limits(limits);
        preview_of_image(&reader.decode().ok()?)
    }

    /// A JPEG preview of `image`, or `None` if no size of it is light
    /// enough.
    fn preview_of_image(image: &image::DynamicImage) -> Option<Vec<u8>> {
        preview::smallest_fitting(
            f64::from(image.width()),
            f64::from(image.height()),
            |width, height| jpeg_of(image, width, height),
        )
    }

    /// `image` drawn at `width` × `height` and encoded as a JPEG.
    fn jpeg_of(image: &image::DynamicImage, width: u32, height: u32) -> Option<Vec<u8>> {
        let small = image
            .resize_exact(width, height, image::imageops::FilterType::Triangle)
            .to_rgb8();
        let mut out = Cursor::new(Vec::new());
        let quality = (preview::QUALITY * 100.0).round() as u8;
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .encode_image(&small)
            .ok()?;
        Some(out.into_inner())
    }
}

#[cfg(target_os = "linux")]
pub(crate) use platform::LinuxPictures;
#[cfg(all(test, target_os = "linux"))]
pub(crate) use platform::preview_of;

#[cfg(test)]
mod tests {
    use iw_core::geometry::Rect;

    use super::*;

    fn display(id: &str, primary: bool) -> Display {
        Display {
            id: id.to_owned(),
            bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            scale: 1.0,
            primary,
        }
    }

    #[test]
    fn gnome_shows_the_picture_of_its_colour_scheme() {
        let light = Some("file:///light.png");
        let dark = Some("file:///dark.jpg");
        assert_eq!(
            gnome_uri(Some("default"), light, dark, Some("zoom")).as_deref(),
            Some("file:///light.png")
        );
        assert_eq!(
            gnome_uri(Some("prefer-dark"), light, dark, Some("zoom")).as_deref(),
            Some("file:///dark.jpg")
        );
        // No dark picture: the one picture.
        assert_eq!(
            gnome_uri(Some("prefer-dark"), light, Some(""), None).as_deref(),
            Some("file:///light.png")
        );
        // A solid colour.
        assert_eq!(gnome_uri(None, light, dark, Some("none")), None);
        assert_eq!(gnome_uri(None, Some(""), None, None), None);
    }

    const APPLETSRC: &str = "\
[ActionPlugins][0]
RightButton;NoModifier=org.kde.contextmenu

[Containments][1]
activityId=8a6c
formfactor=0
immutability=1
lastScreen=0
location=0
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image

[Containments][1][Wallpaper][org.kde.image][General]
Image=file:///home/me/Pictures/left%20side.jpg
PreviewImage=file:///home/me/Pictures/left%20side.jpg

[Containments][2]
activityId=
formfactor=2
lastScreen=0
location=4
plugin=org.kde.panel
wallpaperplugin=org.kde.image

[Containments][3]
activityId=
formfactor=2
lastScreen=0
plugin=org.kde.plasma.private.systemtray
wallpaperplugin=org.kde.image

[Containments][7]
activityId=8a6c
formfactor=0
lastScreen=1
plugin=org.kde.desktopcontainment
wallpaperplugin=org.kde.image

[Containments][7][Wallpaper][org.kde.image][General]
Image=/usr/share/wallpapers/Next/

[Containments][9]
activityId=f00d
formfactor=0
lastScreen=0
plugin=org.kde.desktopcontainment
wallpaperplugin=org.kde.image

[Containments][10]
activityId=f00d
formfactor=0
lastScreen=1
plugin=org.kde.desktopcontainment
wallpaperplugin=org.kde.color

[Containments][10][Wallpaper][org.kde.image][General]
Image=file:///home/me/Pictures/old.png
";

    #[test]
    fn plasma_has_a_desktop_for_each_screen_of_each_activity() {
        let desktop = |screen, activity: &str, image: Option<&str>| KdeDesktop {
            screen: Some(screen),
            activity: Some(activity.to_owned()),
            image: image.map(str::to_owned),
        };
        assert_eq!(
            kde_desktops(APPLETSRC),
            [
                desktop(0, "8a6c", Some("file:///home/me/Pictures/left%20side.jpg")),
                desktop(1, "8a6c", Some("/usr/share/wallpapers/Next/")),
                // Plasma's default picture: named nowhere.
                desktop(0, "f00d", None),
                // A plain colour, though an image was set once.
                desktop(1, "f00d", None),
            ]
        );
        assert_eq!(kde_desktops(""), []);
    }

    #[test]
    fn the_current_activity_is_kactivitymanagerds() {
        let rc = "[activities]\n8a6c=Default\nf00d=Work\n\n[main]\ncurrentActivity=f00d\n";
        assert_eq!(kde_activity(rc).as_deref(), Some("f00d"));
        assert_eq!(kde_activity("[activities]\ncurrentActivity=8a6c\n"), None);
        assert_eq!(kde_activity(""), None);
    }

    #[test]
    fn wallpapers_name_files_by_uri_or_path() {
        assert_eq!(
            path_of("file:///home/me/Pictures/left%20side.jpg"),
            Some(PathBuf::from("/home/me/Pictures/left side.jpg"))
        );
        assert_eq!(
            path_of("/usr/share/wallpapers/Next/"),
            Some(PathBuf::from("/usr/share/wallpapers/Next/"))
        );
        assert_eq!(path_of("https://example.com/a.jpg"), None);
    }

    #[test]
    fn a_wallpaper_package_shows_its_largest_image() {
        let dir = tempfile::tempdir().unwrap();
        let images = dir.path().join("contents/images");
        std::fs::create_dir_all(&images).unwrap();
        for name in [
            "1920x1080.png",
            "3840x2160.png",
            "1280x1024.png",
            "notes.txt",
        ] {
            std::fs::write(images.join(name), b"").unwrap();
        }
        assert_eq!(image_of(dir.path()), Some(images.join("3840x2160.png")));
        let file = images.join("1920x1080.png");
        assert_eq!(image_of(&file), Some(file.clone()));
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(image_of(empty.path()), None);
    }

    #[test]
    fn gnome_puts_its_picture_on_every_display() {
        let displays = [display("DP-1", false), display("eDP-1", true)];
        assert_eq!(
            everywhere(&displays, Some("file:///a.png".to_owned())),
            [
                ("DP-1".to_owned(), "file:///a.png".to_owned()),
                ("eDP-1".to_owned(), "file:///a.png".to_owned()),
            ]
        );
        assert_eq!(everywhere(&displays, None), []);
    }

    fn at(id: &str, x: f64, primary: bool) -> Display {
        Display {
            bounds: Rect::new(x, 0.0, 1920.0, 1080.0),
            ..display(id, primary)
        }
    }

    #[test]
    fn plasma_screens_show_their_desktops_of_the_current_activity() {
        let displays = [at("DP-1", 1920.0, false), at("eDP-1", 0.0, true)];
        let desktops = kde_desktops(APPLETSRC);
        let left = "file:///home/me/Pictures/left%20side.jpg".to_owned();
        let next = "/usr/share/wallpapers/Next/".to_owned();
        assert_eq!(
            kde_assign(&displays, &desktops, Some("8a6c"), &[]),
            [("eDP-1".to_owned(), left), ("DP-1".to_owned(), next)]
        );
        // The other activity names no picture on either screen: none, not
        // the first activity's.
        assert_eq!(kde_assign(&displays, &desktops, Some("f00d"), &[]), []);
        // Not knowing which activity is current, the screens' desktops
        // disagree: none.
        assert_eq!(kde_assign(&displays, &desktops, None, &[]), []);
    }

    #[test]
    fn a_plasma_screen_without_its_own_picture_shows_none() {
        let displays = [at("DP-1", 1920.0, false), at("eDP-1", 0.0, true)];
        let desktops = [
            KdeDesktop {
                screen: Some(0),
                activity: None,
                image: Some("file:///a.png".to_owned()),
            },
            KdeDesktop {
                screen: Some(1),
                activity: None,
                image: None,
            },
        ];
        assert_eq!(
            kde_assign(&displays, &desktops, None, &[]),
            [("eDP-1".to_owned(), "file:///a.png".to_owned())]
        );
        // A screen with no desktop at all shows none either.
        assert_eq!(
            kde_assign(&displays, &desktops[..1], Some("8a6c"), &[]),
            [("eDP-1".to_owned(), "file:///a.png".to_owned())]
        );
    }

    #[test]
    fn plasma_numbers_the_main_screen_first_then_left_to_right() {
        let displays = [
            at("HDMI-A-1", 3840.0, false),
            at("eDP-1", 1920.0, true),
            at("DP-1", 0.0, false),
        ];
        let desktops: Vec<KdeDesktop> = ["main", "left", "right"]
            .iter()
            .enumerate()
            .map(|(screen, name)| KdeDesktop {
                screen: Some(screen as u32),
                activity: None,
                image: Some(format!("/{name}.png")),
            })
            .collect();
        assert_eq!(
            kde_assign(&displays, &desktops, None, &[]),
            [
                ("eDP-1".to_owned(), "/main.png".to_owned()),
                ("DP-1".to_owned(), "/left.png".to_owned()),
                ("HDMI-A-1".to_owned(), "/right.png".to_owned()),
            ]
        );
    }

    #[test]
    fn plasmas_screen_connectors_say_which_display_a_screen_is() {
        let rc = "[PlasmaViews][Panel 2]\nalignment=132\n\n[ScreenConnectors]\n0=HDMI-A-1\n1=eDP-1\n2=\n";
        let connectors = kde_connectors(rc);
        assert_eq!(
            connectors,
            [(0, "HDMI-A-1".to_owned()), (1, "eDP-1".to_owned())]
        );
        assert_eq!(kde_connectors(""), []);
        // On Wayland the output at (0, 0) is taken as the main one, which
        // need not be Plasma's: the connectors decide, not that.
        let displays = [at("eDP-1", 0.0, true), at("HDMI-A-1", 1920.0, false)];
        let desktops: Vec<KdeDesktop> = ["external", "laptop"]
            .iter()
            .enumerate()
            .map(|(screen, name)| KdeDesktop {
                screen: Some(screen as u32),
                activity: None,
                image: Some(format!("/{name}.png")),
            })
            .collect();
        assert_eq!(
            kde_assign(&displays, &desktops, None, &connectors),
            [
                ("HDMI-A-1".to_owned(), "/external.png".to_owned()),
                ("eDP-1".to_owned(), "/laptop.png".to_owned()),
            ]
        );
        // Without them, the main display first.
        assert_eq!(
            kde_assign(&displays, &desktops, None, &[]),
            [
                ("eDP-1".to_owned(), "/external.png".to_owned()),
                ("HDMI-A-1".to_owned(), "/laptop.png".to_owned()),
            ]
        );
        // A display the connectors do not name shows none.
        let more = [at("eDP-1", 0.0, true), at("DP-2", 1920.0, false)];
        assert_eq!(
            kde_assign(&more, &desktops, None, &connectors),
            [("eDP-1".to_owned(), "/laptop.png".to_owned())]
        );
    }

    /// A PNG, a JPEG and a WebP make previews no larger than the limits; a
    /// text file and a missing one make none.
    #[cfg(target_os = "linux")]
    #[test]
    fn previews_are_made_of_jpeg_png_and_webp() {
        let dir = tempfile::tempdir().unwrap();
        let picture = image::RgbImage::from_fn(1600, 900, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        for (name, format) in [
            ("a.png", image::ImageFormat::Png),
            ("a.jpg", image::ImageFormat::Jpeg),
            ("a.webp", image::ImageFormat::WebP),
        ] {
            let file = dir.path().join(name);
            picture.save_with_format(&file, format).unwrap();
            let jpeg = preview_of(&file).unwrap_or_else(|| panic!("no preview of {name}"));
            assert!(jpeg.starts_with(&[0xFF, 0xD8, 0xFF]), "{name}");
            assert!(jpeg.len() <= crate::preview::MAX_BYTES, "{name}");
            let small = image::load_from_memory(&jpeg).unwrap();
            assert_eq!((small.width(), small.height()), (320, 180), "{name}");
        }
        let text = dir.path().join("slideshow.xml");
        std::fs::write(&text, b"<background></background>").unwrap();
        assert_eq!(preview_of(&text), None);
        assert_eq!(preview_of(&dir.path().join("missing.png")), None);
    }
}
