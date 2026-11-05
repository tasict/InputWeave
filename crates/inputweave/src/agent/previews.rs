//! Previews of desktop pictures, for the arrangement canvas: this device's
//! come from the platform (on Windows, from the broker), other devices'
//! from their `Displays` messages. Pictures from another device, or from
//! the broker, are checked against limits before they are kept. Another
//! device's are kept as files next to the devices file, so a device that
//! is offline still shows them:
//!
//! ```text
//! previews/<device id>/<display id in hex>.jpg
//! ```
//!
//! The display id is written in hex because it comes from the other
//! device and could be anything, `\\.\DISPLAY1` on Windows. A device's
//! folder holds one file per display it last reported a picture for, and
//! goes when the device is no longer trusted.

use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iw_proto::peer::v1 as peer;
use tracing::warn;

/// Another device's picture larger than this is refused.
pub const MAX_PICTURE: usize = 128 * 1024;
/// Another device's pictures together larger than this are refused.
pub const MAX_PICTURES_TOTAL: usize = 1024 * 1024;
/// Pictures for more displays than this are refused.
pub const MAX_PICTURES: usize = 16;
/// A picture is refused for a display id longer than this, in bytes.
pub const MAX_DISPLAY_ID: usize = 64;
/// The pictures in one arrangement sent to an IPC client stay under this
/// together, well inside an IPC frame; devices whose pictures would pass
/// it go without.
pub const MAX_ARRANGEMENT_PICTURES: usize = 2 * 1024 * 1024;

/// What every JPEG starts with.
const JPEG_START: [u8; 3] = [0xff, 0xd8, 0xff];

/// A JPEG preview of a desktop picture. Clones share the bytes. Its
/// `Debug` shows only the size, so logs and diagnostics reports never
/// hold a picture.
#[derive(Clone, Eq)]
pub struct Preview(Arc<[u8]>);

impl PartialEq for Preview {
    /// Clones compare without reading the bytes, as the platform poller's
    /// state and the published arrangement do every time.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

impl Preview {
    pub fn new(jpeg: Vec<u8>) -> Self {
        Self(jpeg.into())
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Preview {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Preview({} bytes)", self.0.len())
    }
}

/// Pictures by display id.
pub type Pictures = BTreeMap<String, Preview>;

/// Why another device's pictures were dropped.
#[derive(Debug, PartialEq, thiserror::Error)]
pub enum Refused {
    #[error("the picture for display {display:?} is {len} bytes, over {MAX_PICTURE}")]
    TooLarge { display: String, len: usize },
    #[error("the pictures are {0} bytes together, over {MAX_PICTURES_TOTAL}")]
    TooMuch(usize),
    #[error("{0} displays have a picture, over {MAX_PICTURES}")]
    TooMany(usize),
    #[error("a display id of {0} bytes has a picture; ids are 1 to {MAX_DISPLAY_ID} bytes")]
    BadId(usize),
    #[error("the picture for display {0:?} is not a JPEG")]
    NotJpeg(String),
}

/// Whether one picture keeps to the limits on its own.
fn check(display: &str, jpeg: &[u8]) -> Result<(), Refused> {
    if display.is_empty() || display.len() > MAX_DISPLAY_ID {
        return Err(Refused::BadId(display.len()));
    }
    if jpeg.len() > MAX_PICTURE {
        return Err(Refused::TooLarge {
            display: display.to_owned(),
            len: jpeg.len(),
        });
    }
    if !jpeg.starts_with(&JPEG_START) {
        return Err(Refused::NotJpeg(display.to_owned()));
    }
    Ok(())
}

/// The pictures in another device's `Displays`, if all of them keep to
/// the limits. One that does not drops them all; the displays themselves
/// are kept either way. A display id that comes more than once keeps the
/// last picture given for it, and the count and total are of the pictures
/// kept.
pub(crate) fn accept(displays: &[peer::Display]) -> Result<Pictures, Refused> {
    accept_pictures(
        displays
            .iter()
            .map(|d| (d.id.as_str(), d.preview_jpeg.as_slice())),
    )
}

/// Pictures by display id from elsewhere, another device or the Windows
/// broker, held to the limits as [`accept`] describes. An empty picture
/// stands for none.
pub(crate) fn accept_pictures<'a>(
    pictures: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Result<Pictures, Refused> {
    let mut kept = Pictures::new();
    for (id, jpeg) in pictures.into_iter().filter(|(_, jpeg)| !jpeg.is_empty()) {
        check(id, jpeg)?;
        kept.insert(id.to_owned(), Preview::new(jpeg.to_vec()));
    }
    if kept.len() > MAX_PICTURES {
        return Err(Refused::TooMany(kept.len()));
    }
    let total = kept.values().map(Preview::len).sum();
    if total > MAX_PICTURES_TOTAL {
        return Err(Refused::TooMuch(total));
    }
    Ok(kept)
}

/// The pictures this device sends for the displays in `display_ids`,
/// within the limits [`accept`] holds them to, so another device never
/// drops them all: a picture over a limit on its own is left out, and
/// while there are more than [`MAX_PICTURES`] or they weigh more than
/// [`MAX_PICTURES_TOTAL`] together, the largest of the rest is too.
pub(crate) fn sendable<'a>(
    pictures: &Pictures,
    display_ids: impl IntoIterator<Item = &'a str>,
) -> Pictures {
    let mut sent: Pictures = display_ids
        .into_iter()
        .filter_map(|id| pictures.get_key_value(id))
        .filter(|(id, jpeg)| check(id, jpeg.bytes()).is_ok())
        .map(|(id, jpeg)| (id.clone(), jpeg.clone()))
        .collect();
    // Smallest first, so the largest come off the end; ties by id.
    let mut by_size: Vec<(usize, String)> = sent
        .iter()
        .map(|(id, jpeg)| (jpeg.len(), id.clone()))
        .collect();
    by_size.sort();
    let mut total: usize = by_size.iter().map(|(len, _)| len).sum();
    while sent.len() > MAX_PICTURES || total > MAX_PICTURES_TOTAL {
        let Some((len, id)) = by_size.pop() else {
            break;
        };
        sent.remove(&id);
        total -= len;
    }
    sent
}

/// Other devices' pictures as files under one folder, or nowhere when the
/// agent keeps no files.
pub(crate) struct PreviewFiles {
    dir: Option<PathBuf>,
}

impl PreviewFiles {
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        Self { dir }
    }

    /// The folder for `device`'s pictures. Device ids are letters, digits,
    /// `.`, `_` and `-`; one of dots alone would name a folder elsewhere.
    fn device_dir(&self, device: &str) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        let safe = iw_net::is_valid_device_id(device) && !device.bytes().all(|b| b == b'.');
        safe.then(|| dir.join(device))
    }

    /// The pictures kept for `device`, for the displays in `display_ids`.
    /// Files for other displays, or that are not a picture this agent
    /// would have kept, are removed.
    pub(crate) fn load<'a>(
        &self,
        device: &str,
        display_ids: impl IntoIterator<Item = &'a str>,
    ) -> Pictures {
        let Some(dir) = self.device_dir(device) else {
            return Pictures::new();
        };
        let wanted: BTreeMap<String, String> = display_ids
            .into_iter()
            .filter(|id| !id.is_empty() && id.len() <= MAX_DISPLAY_ID)
            .map(|id| (file_name(id), id.to_owned()))
            .collect();
        let mut pictures = Pictures::new();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return pictures;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let picture = wanted.get(&name).and_then(|id| {
                let jpeg = read_picture(&entry.path())?;
                Some((id.clone(), jpeg))
            });
            match picture {
                Some((id, jpeg)) if pictures.len() < MAX_PICTURES => {
                    pictures.insert(id, jpeg);
                }
                _ => remove_entry(&entry.path()),
            }
        }
        if pictures.is_empty() {
            let _ = std::fs::remove_dir(&dir);
        }
        pictures
    }

    /// Keeps `pictures` as `device`'s, in place of what was kept before.
    pub(crate) fn save(&self, device: &str, pictures: &Pictures) {
        let Some(dir) = self.device_dir(device) else {
            return;
        };
        if pictures.is_empty() {
            self.remove(device);
            return;
        }
        if let Err(e) = write_all(&dir, pictures) {
            warn!(%device, error = %e, "cannot keep the desktop pictures of a device");
        }
    }

    /// Forgets `device`'s pictures.
    pub(crate) fn remove(&self, device: &str) {
        let Some(dir) = self.device_dir(device) else {
            return;
        };
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!(%device, error = %e, "cannot remove the desktop pictures of a device"),
        }
    }

    /// Removes the folders of devices other than `keep`.
    pub(crate) fn prune<'a>(&self, keep: impl IntoIterator<Item = &'a str>) {
        let Some(dir) = &self.dir else { return };
        let keep: Vec<&str> = keep.into_iter().collect();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !keep.iter().any(|k| name == **k) {
                remove_entry(&entry.path());
            }
        }
    }
}

/// `<display id in hex>.jpg`.
fn file_name(display_id: &str) -> String {
    let mut name = String::with_capacity(display_id.len() * 2 + 4);
    for b in display_id.bytes() {
        let _ = write!(name, "{b:02x}");
    }
    name.push_str(".jpg");
    name
}

fn read_picture(path: &Path) -> Option<Preview> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_PICTURE as u64 {
        return None;
    }
    let jpeg = std::fs::read(path).ok()?;
    jpeg.starts_with(&JPEG_START).then(|| Preview::new(jpeg))
}

fn remove_entry(path: &Path) {
    let removed = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(_) => return,
    };
    if let Err(e) = removed {
        warn!(path = %path.display(), error = %e, "cannot remove a desktop picture file");
    }
}

/// Writes one file per picture, each replaced in one step and readable only
/// by its owner, then removes every other file in `dir`.
fn write_all(dir: &Path, pictures: &Pictures) -> std::io::Result<()> {
    super::private::private_dir(dir)?;
    let mut names = Vec::new();
    for (id, jpeg) in pictures {
        let name = file_name(id);
        let path = dir.join(&name);
        let tmp = dir.join(format!("{name}.tmp"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(jpeg.bytes())?;
        drop(file);
        std::fs::rename(&tmp, &path)?;
        names.push(name);
    }
    for entry in std::fs::read_dir(dir)?.flatten() {
        if !names.iter().any(|n| entry.file_name() == n.as_str()) {
            remove_entry(&entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(len: usize) -> Vec<u8> {
        let mut bytes = JPEG_START.to_vec();
        bytes.resize(len.max(3), 0x42);
        bytes
    }

    fn display(id: &str, picture: Vec<u8>) -> peer::Display {
        peer::Display {
            id: id.to_owned(),
            width: 100.0,
            height: 100.0,
            scale: 1.0,
            preview_jpeg: picture,
            ..Default::default()
        }
    }

    #[test]
    fn pictures_within_the_limits_are_kept_by_display() {
        let pictures = accept(&[
            display("1", jpeg(1000)),
            display("2", Vec::new()),
            display("3", jpeg(MAX_PICTURE)),
        ])
        .unwrap();
        assert_eq!(pictures.keys().collect::<Vec<_>>(), ["1", "3"]);
        assert_eq!(pictures["3"].len(), MAX_PICTURE);
        assert!(accept(&[display("1", Vec::new())]).unwrap().is_empty());
    }

    #[test]
    fn one_picture_over_the_limit_drops_them_all() {
        assert_eq!(
            accept(&[display("1", jpeg(10)), display("2", jpeg(MAX_PICTURE + 1))]),
            Err(Refused::TooLarge {
                display: "2".to_owned(),
                len: MAX_PICTURE + 1
            })
        );
        assert_eq!(
            accept(&[display("1", b"GIF89a".to_vec())]),
            Err(Refused::NotJpeg("1".to_owned()))
        );
        let long = "x".repeat(MAX_DISPLAY_ID + 1);
        assert_eq!(
            accept(&[display(&long, jpeg(10))]),
            Err(Refused::BadId(MAX_DISPLAY_ID + 1))
        );
        assert_eq!(accept(&[display("", jpeg(10))]), Err(Refused::BadId(0)));
        // A long id without a picture is no concern of previews.
        assert!(accept(&[display(&long, Vec::new())]).is_ok());
    }

    #[test]
    fn too_much_altogether_drops_them_all() {
        let nine: Vec<peer::Display> = (0..9)
            .map(|i| display(&i.to_string(), jpeg(MAX_PICTURE)))
            .collect();
        assert_eq!(accept(&nine), Err(Refused::TooMuch(9 * MAX_PICTURE)));
        assert!(accept(&nine[..8]).is_ok(), "exactly 1 MiB");

        let many: Vec<peer::Display> = (0..=MAX_PICTURES)
            .map(|i| display(&i.to_string(), jpeg(10)))
            .collect();
        assert_eq!(accept(&many), Err(Refused::TooMany(MAX_PICTURES + 1)));
        assert!(accept(&many[..MAX_PICTURES]).is_ok());
    }

    #[test]
    fn a_display_given_twice_keeps_its_last_picture_and_counts_once() {
        // Nine copies of one id would be 9 × 128 KB counted one by one.
        let mut repeated: Vec<peer::Display> =
            (0..8).map(|_| display("1", jpeg(MAX_PICTURE))).collect();
        repeated.push(display("1", jpeg(500)));
        let pictures = accept(&repeated).unwrap();
        assert_eq!(pictures.len(), 1);
        assert_eq!(pictures["1"].len(), 500, "the last one wins");

        // Sixteen displays, one of them twice, are sixteen pictures.
        let mut many: Vec<peer::Display> = (0..MAX_PICTURES)
            .map(|i| display(&i.to_string(), jpeg(10)))
            .collect();
        many.push(display("0", jpeg(20)));
        assert_eq!(accept(&many).unwrap().len(), MAX_PICTURES);

        // Eight full pictures plus a repeat of one stay at 1 MiB.
        let mut full: Vec<peer::Display> = (0..8)
            .map(|i| display(&i.to_string(), jpeg(MAX_PICTURE)))
            .collect();
        full.push(display("3", jpeg(MAX_PICTURE)));
        assert_eq!(accept(&full).unwrap().len(), 8);
    }

    fn ids(pictures: &Pictures) -> Vec<&str> {
        pictures.keys().map(String::as_str).collect()
    }

    #[test]
    fn this_device_sends_what_others_accept() {
        // Eleven displays at 96 KB come to more than 1 MiB: the largest
        // goes, and the rest are accepted.
        let mut eleven: Vec<(String, usize)> =
            (0..11).map(|i| (format!("d{i:02}"), 96 * 1024)).collect();
        eleven[4].1 += 1;
        let local: Pictures = eleven
            .iter()
            .map(|(id, len)| (id.clone(), Preview::new(jpeg(*len))))
            .collect();
        let all = || eleven.iter().map(|(id, _)| id.as_str());
        let all_displays: Vec<peer::Display> = all()
            .map(|id| display(id, local[id].bytes().to_vec()))
            .collect();
        assert!(matches!(accept(&all_displays), Err(Refused::TooMuch(_))));
        let sent = sendable(&local, all());
        assert_eq!(sent.len(), 10);
        assert!(!sent.contains_key("d04"), "{:?}", ids(&sent));
        let displays: Vec<peer::Display> = all()
            .map(|id| {
                display(
                    id,
                    sent.get(id).map(|p| p.bytes().to_vec()).unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(accept(&displays).unwrap(), sent);

        // Of equal pictures, those with the greatest ids go first.
        let mut equal: Vec<(String, usize)> =
            (0..9).map(|i| (format!("d{i}"), 128 * 1024)).collect();
        equal[0].1 = 10;
        let local: Pictures = equal
            .iter()
            .map(|(id, len)| (id.clone(), Preview::new(jpeg(*len))))
            .collect();
        let sent = sendable(&local, equal.iter().map(|(id, _)| id.as_str()));
        assert_eq!(ids(&sent), ["d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7"]);

        // More than sixteen: the largest go.
        let local: Pictures = (0..20)
            .map(|i| (format!("{i:02}"), Preview::new(jpeg(100 + i))))
            .collect();
        let sent = sendable(&local, local.keys().map(String::as_str));
        assert_eq!(sent.len(), MAX_PICTURES);
        assert!(sent.keys().all(|id| id.as_str() < "16"));
    }

    #[test]
    fn pictures_over_a_limit_on_their_own_or_of_no_display_are_not_sent() {
        let long = "x".repeat(MAX_DISPLAY_ID + 1);
        let local = pictures(&[
            ("big", MAX_PICTURE + 1),
            ("ok", 100),
            ("gone", 100),
            (long.as_str(), 100),
        ]);
        let sent = sendable(&local, ["big", "ok", "unseen", long.as_str()]);
        assert_eq!(ids(&sent), ["ok"]);
        let mut gif = Pictures::new();
        gif.insert("1".to_owned(), Preview::new(b"GIF89a".to_vec()));
        assert!(sendable(&gif, ["1"]).is_empty());
    }

    #[test]
    fn previews_never_show_their_bytes() {
        let preview = Preview::new(jpeg(2048));
        assert_eq!(format!("{preview:?}"), "Preview(2048 bytes)");
    }

    #[test]
    fn files_are_named_by_display_id_in_hex() {
        assert_eq!(file_name("1"), "31.jpg");
        assert_eq!(file_name(r"\\.\DISPLAY1"), "5c5c2e5c444953504c415931.jpg");
        assert_eq!(file_name("../x"), "2e2e2f78.jpg");
    }

    fn pictures(entries: &[(&str, usize)]) -> Pictures {
        entries
            .iter()
            .map(|(id, len)| (id.to_string(), Preview::new(jpeg(*len))))
            .collect()
    }

    #[test]
    fn pictures_are_kept_until_replaced_or_removed() {
        let dir = tempfile::tempdir().unwrap();
        let files = PreviewFiles::new(Some(dir.path().join("previews")));
        let win = dir.path().join("previews/win");

        files.save("win", &pictures(&[("main", 100), (r"\\.\DISPLAY2", 200)]));
        assert_eq!(std::fs::read_dir(&win).unwrap().count(), 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&win), 0o700);
            assert_eq!(mode(&win.join(file_name("main"))), 0o600);
        }
        let loaded = files.load("win", ["main", r"\\.\DISPLAY2", "other"]);
        assert_eq!(loaded, pictures(&[("main", 100), (r"\\.\DISPLAY2", 200)]));

        // A display that went away takes its file with it.
        files.save("win", &pictures(&[("main", 300)]));
        assert_eq!(std::fs::read_dir(&win).unwrap().count(), 1);
        assert_eq!(files.load("win", ["main"]), pictures(&[("main", 300)]));

        // Loading for fewer displays than there are files removes the rest.
        files.save("win", &pictures(&[("main", 300), ("side", 10)]));
        assert_eq!(files.load("win", ["side"]), pictures(&[("side", 10)]));
        assert_eq!(std::fs::read_dir(&win).unwrap().count(), 1);

        files.save("win", &Pictures::new());
        assert!(!win.exists());
        files.save("win", &pictures(&[("main", 300)]));
        files.remove("win");
        assert!(!win.exists());
        assert!(files.load("win", ["main"]).is_empty());
    }

    #[test]
    fn files_that_are_not_kept_pictures_are_removed_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let files = PreviewFiles::new(Some(dir.path().to_owned()));
        let mini = dir.path().join("mini");
        std::fs::create_dir_all(mini.join("junk")).unwrap();
        std::fs::write(mini.join(file_name("big")), jpeg(MAX_PICTURE + 1)).unwrap();
        std::fs::write(mini.join(file_name("png")), b"\x89PNG").unwrap();
        std::fs::write(mini.join(file_name("ok")), jpeg(50)).unwrap();
        std::fs::write(mini.join("31.jpg.tmp"), jpeg(50)).unwrap();
        let loaded = files.load("mini", ["big", "png", "ok"]);
        assert_eq!(loaded, pictures(&[("ok", 50)]));
        assert_eq!(std::fs::read_dir(&mini).unwrap().count(), 1);
    }

    #[test]
    fn devices_no_longer_trusted_lose_their_folders() {
        let dir = tempfile::tempdir().unwrap();
        let files = PreviewFiles::new(Some(dir.path().to_owned()));
        files.save("win", &pictures(&[("main", 10)]));
        files.save("mini", &pictures(&[("ext", 10)]));
        std::fs::write(dir.path().join("stray.jpg"), b"x").unwrap();
        files.prune(["mini"]);
        let left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["mini"]);
    }

    #[test]
    fn ids_of_dots_alone_never_name_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let previews = dir.path().join("previews");
        let files = PreviewFiles::new(Some(previews.clone()));
        std::fs::write(dir.path().join("devices.toml"), "keep me").unwrap();
        for id in [".", "..", "...", "a/b", ""] {
            files.save(id, &pictures(&[("main", 10)]));
            files.remove(id);
            assert!(files.load(id, ["main"]).is_empty());
        }
        assert!(dir.path().join("devices.toml").exists());
        assert!(!previews.exists());

        // Without a folder nothing is kept.
        let nowhere = PreviewFiles::new(None);
        nowhere.save("win", &pictures(&[("main", 10)]));
        assert!(nowhere.load("win", ["main"]).is_empty());
    }
}
