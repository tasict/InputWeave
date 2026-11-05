//! What a user sends, listed before anything is sent (docs/architecture.md
//! §9): each path given is an item, a file or a folder with everything
//! in it, and a batch is the items sent together.
//!
//! Nothing is read here but folders: a file's size comes from its folder
//! entry. Links are never followed, so a folder cannot lead outside itself
//! or round in circles; they are left out as failed (`symlink`), as is
//! anything that is neither file nor folder (`not_a_file`) and what cannot
//! be listed (`unreadable`). An empty folder, or one whose contents all
//! failed, is sent as a folder of its own so it arrives too.
//!
//! On Windows the broker lists the folders as the user (`broker`), since
//! the agent, running as SYSTEM, could see what the user may not; the
//! walk itself is the same.
//!
//! The paths in a batch name each file by its item's name and the folders
//! down to it, separated by "/". Both the sender and the receiver check
//! them ([`split_relative`]).

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};

/// Items in one batch, and items going from this device at once in all;
/// more are refused, before anything is listed. The transfer list shows
/// one row per item, all in every message to the apps, so this keeps that
/// message well under the IPC frame limit.
pub const MAX_ITEMS: usize = 1_000;
/// Files and empty folders in one batch.
pub const MAX_FILES: usize = 10_000;
/// Bytes in one batch: 100 GiB.
pub const MAX_BYTES: u64 = 100 * 1024 * 1024 * 1024;
/// Parts in a relative path: the item's name, the folders, the name.
const MAX_DEPTH: usize = 64;
/// Bytes in a relative path.
const MAX_PATH_BYTES: usize = 4096;
/// Failures listed for a folder; more are only counted.
pub(crate) const MAX_LISTED_FAILURES: usize = 100;

/// What a path is, as listed without following links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    File(u64),
    Folder,
    Link,
    /// A device, a pipe, a socket.
    Other,
    Missing,
    Unreadable,
}

impl Kind {
    /// Why an item or a folder entry of this kind cannot be sent.
    fn failure(&self) -> &'static str {
        match self {
            Self::Link => "symlink",
            Self::Unreadable => "unreadable",
            _ => "not_a_file",
        }
    }
}

/// A path, and for a folder what is directly in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Listing {
    pub(crate) kind: Option<Kind>,
    pub(crate) entries: Vec<(String, Kind)>,
    /// The folder holds more than was asked for.
    pub(crate) truncated: bool,
    /// Why it cannot be read, as the system said.
    pub(crate) error: String,
}

impl Listing {
    fn of(kind: Kind) -> Self {
        Self {
            kind: Some(kind),
            ..Self::default()
        }
    }

    /// What `e`, met looking at a path, makes of it.
    fn failed(e: &io::Error) -> Self {
        let kind = match e.kind() {
            io::ErrorKind::NotFound => Kind::Missing,
            _ => Kind::Unreadable,
        };
        Self {
            error: e.to_string(),
            ..Self::of(kind)
        }
    }
}

/// Lists paths: this process, or the Windows broker as the user. Fails
/// with `NotConnected` when there is no broker to ask.
pub(crate) trait Lister {
    fn list(&self, path: &Path, max: usize) -> impl Future<Output = io::Result<Listing>> + Send;
}

/// This process lists, off the async threads.
pub(crate) struct LocalLister;

impl Lister for LocalLister {
    async fn list(&self, path: &Path, max: usize) -> io::Result<Listing> {
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || Ok(list_local(&path, max)))
            .await
            .unwrap_or_else(|e| Err(io::Error::other(e)))
    }
}

/// Lists `path` without following links, and for a folder at most `max`
/// of its entries.
pub(crate) fn list_local(path: &Path, max: usize) -> Listing {
    let kind = |e: &io::Error| match e.kind() {
        io::ErrorKind::NotFound => Kind::Missing,
        _ => Kind::Unreadable,
    };
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) => return Listing::failed(&e),
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Listing::of(Kind::Link);
    }
    if file_type.is_file() {
        // Opened to tell a file that cannot be read now, not later.
        return match std::fs::File::open(path) {
            Ok(_) => Listing::of(Kind::File(metadata.len())),
            Err(e) => Listing::failed(&e),
        };
    }
    if !file_type.is_dir() {
        return Listing::of(Kind::Other);
    }
    let read = match std::fs::read_dir(path) {
        Ok(read) => read,
        Err(e) => return Listing::failed(&e),
    };
    let mut listing = Listing::of(Kind::Folder);
    for entry in read {
        if listing.entries.len() >= max {
            listing.truncated = true;
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        // A name that is not Unicode could not be named on the other side.
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            listing
                .entries
                .push((entry.file_name().to_string_lossy().into(), Kind::Unreadable));
            continue;
        };
        let kind = match entry.file_type() {
            Ok(t) if t.is_symlink() => Kind::Link,
            Ok(t) if t.is_dir() => Kind::Folder,
            Ok(t) if t.is_file() => match entry.metadata() {
                Ok(metadata) => Kind::File(metadata.len()),
                Err(e) => kind(&e),
            },
            Ok(_) => Kind::Other,
            Err(e) => kind(&e),
        };
        listing.entries.push((name, kind));
    }
    listing
}

/// A file or an empty folder to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Its path in the batch, from the item's name.
    pub(crate) relative: String,
    /// Where it is on this computer.
    pub(crate) path: PathBuf,
    pub(crate) size: u64,
    /// An empty folder.
    pub(crate) folder: bool,
}

/// Something in a folder that cannot be sent: its path in the batch and
/// why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    pub(crate) relative: String,
    pub(crate) reason: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ItemKind {
    File(u64),
    Folder {
        entries: Vec<Entry>,
        failures: Vec<Failure>,
    },
    /// It cannot be sent at all.
    Failed(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Item {
    /// Its name: the path's last part.
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) kind: ItemKind,
    /// Why it cannot be read, as the system said; empty otherwise.
    pub(crate) detail: String,
}

impl Item {
    /// The files and empty folders that go, and their bytes.
    pub(crate) fn totals(&self) -> (usize, u64) {
        match &self.kind {
            ItemKind::File(size) => (1, *size),
            ItemKind::Folder { entries, .. } => {
                (entries.len(), entries.iter().map(|e| e.size).sum())
            }
            ItemKind::Failed(_) => (0, 0),
        }
    }

    /// Whether anything of it goes.
    pub(crate) fn sendable(&self) -> bool {
        self.totals().0 > 0
    }
}

/// The items of a batch, listed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Manifest {
    pub(crate) items: Vec<Item>,
}

impl Manifest {
    /// The files and empty folders that go, and their bytes.
    pub(crate) fn totals(&self) -> (usize, u64) {
        self.items.iter().fold((0, 0), |(files, bytes), item| {
            let (f, b) = item.totals();
            (files + f, bytes + b)
        })
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ListError {
    #[error("no broker is connected to list the files as the user")]
    NoBroker,
    #[error("{0} items is more than {MAX_ITEMS} at once")]
    TooManyItems(usize),
    #[error("more than {MAX_FILES} files and folders, or {MAX_BYTES} bytes, at once")]
    TooLarge,
}

fn lost(e: &io::Error) -> Option<ListError> {
    (e.kind() == io::ErrorKind::NotConnected).then_some(ListError::NoBroker)
}

/// Lists the items at `paths`, refusing more than [`MAX_ITEMS`] items,
/// [`MAX_FILES`] files and folders or [`MAX_BYTES`] bytes in all.
pub(crate) async fn list_items(
    lister: &impl Lister,
    paths: Vec<PathBuf>,
) -> Result<Manifest, ListError> {
    if paths.len() > MAX_ITEMS {
        return Err(ListError::TooManyItems(paths.len()));
    }
    let mut budget = Budget::default();
    let mut items = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| valid_part(n))
            .map(str::to_owned);
        let Some(name) = name else {
            let shown = path.to_string_lossy().into_owned();
            items.push(Item {
                name: shown,
                path,
                kind: ItemKind::Failed("not_a_file"),
                detail: String::new(),
            });
            continue;
        };
        // An item that is not absolute could be anywhere.
        let listing = if path.is_absolute() {
            match lister.list(&path, MAX_FILES + 1).await {
                Ok(listing) => listing,
                Err(e) => match lost(&e) {
                    Some(lost) => return Err(lost),
                    None => Listing::failed(&e),
                },
            }
        } else {
            Listing::of(Kind::Missing)
        };
        let detail = listing.error.clone();
        let kind = match listing.kind {
            Some(Kind::File(size)) => {
                budget.add(1, size)?;
                ItemKind::File(size)
            }
            Some(Kind::Folder) => walk(lister, &path, &name, listing, &mut budget).await?,
            Some(other) => ItemKind::Failed(other.failure()),
            None => ItemKind::Failed("unreadable"),
        };
        items.push(Item {
            name,
            path,
            kind,
            detail,
        });
    }
    Ok(Manifest { items })
}

/// What a batch holds so far, against its limits.
#[derive(Default)]
struct Budget {
    files: usize,
    bytes: u64,
}

impl Budget {
    fn add(&mut self, files: usize, bytes: u64) -> Result<(), ListError> {
        self.files += files;
        self.bytes = self.bytes.saturating_add(bytes);
        if self.files > MAX_FILES || self.bytes > MAX_BYTES {
            return Err(ListError::TooLarge);
        }
        Ok(())
    }
}

/// Everything in the folder at `root`, listed as `listing`: its files in
/// name order, then each folder's contents the same way.
async fn walk(
    lister: &impl Lister,
    root: &Path,
    name: &str,
    listing: Listing,
    budget: &mut Budget,
) -> Result<ItemKind, ListError> {
    let mut entries = Vec::new();
    let mut failures = Vec::new();
    // Folders still to list, last first.
    let mut stack = vec![(root.to_owned(), name.to_owned(), Some(listing))];
    while let Some((dir, relative, listing)) = stack.pop() {
        let listing = match listing {
            Some(listing) => listing,
            None => match lister.list(&dir, MAX_FILES + 1).await {
                Ok(listing) => listing,
                Err(e) => match lost(&e) {
                    Some(lost) => return Err(lost),
                    None => Listing::failed(&e),
                },
            },
        };
        match listing.kind {
            Some(Kind::Folder) => {}
            Some(other) => {
                failures.push(Failure {
                    relative,
                    reason: other.failure(),
                });
                continue;
            }
            None => {
                failures.push(Failure {
                    relative,
                    reason: "unreadable",
                });
                continue;
            }
        }
        if listing.truncated {
            return Err(ListError::TooLarge);
        }
        let mut children = listing.entries;
        children.sort_by(|a, b| a.0.cmp(&b.0));
        let mut holds = false;
        let mut folders = Vec::new();
        for (child, kind) in children {
            let path = dir.join(&child);
            let child_relative = format!("{relative}/{child}");
            if !valid_part(&child) || child_relative.len() > MAX_PATH_BYTES {
                failures.push(Failure {
                    relative: child_relative,
                    reason: "unreadable",
                });
                continue;
            }
            match kind {
                Kind::File(size) => {
                    budget.add(1, size)?;
                    holds = true;
                    entries.push(Entry {
                        relative: child_relative,
                        path,
                        size,
                        folder: false,
                    });
                }
                Kind::Folder if relative.split('/').count() + 1 < MAX_DEPTH => {
                    holds = true;
                    folders.push((path, child_relative, None));
                }
                // Too deep to name on the other side.
                Kind::Folder => failures.push(Failure {
                    relative: child_relative,
                    reason: "unreadable",
                }),
                other => failures.push(Failure {
                    relative: child_relative,
                    reason: other.failure(),
                }),
            }
        }
        if !holds {
            // Empty, or with nothing that goes: sent as a folder of its own.
            budget.add(1, 0)?;
            entries.push(Entry {
                relative,
                path: dir,
                size: 0,
                folder: true,
            });
        }
        stack.extend(folders.into_iter().rev());
    }
    Ok(ItemKind::Folder { entries, failures })
}

/// Whether `part` can be one part of a relative path: not empty, `.` or
/// `..`, and without "/" or NUL.
pub(crate) fn valid_part(part: &str) -> bool {
    !part.is_empty() && part != "." && part != ".." && !part.contains(['/', '\0'])
}

/// The parts of a relative path in a batch, if it is one: not absolute,
/// at most [`MAX_DEPTH`] parts and [`MAX_PATH_BYTES`] bytes, without empty,
/// `.` or `..` parts. Each part is still to be made a name the receiving
/// system allows (`files::part_name`).
pub(crate) fn split_relative(path: &str) -> Option<Vec<&str>> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.starts_with('/') {
        return None;
    }
    let parts: Vec<&str> = path.split('/').collect();
    (parts.len() <= MAX_DEPTH && parts.iter().all(|p| valid_part(p))).then_some(parts)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// `path`, written from the root with "/", as an absolute path on this
    /// system: on Windows one needs a drive too.
    fn at(path: &str) -> PathBuf {
        PathBuf::from(if cfg!(windows) {
            format!("C:{path}")
        } else {
            path.to_owned()
        })
    }

    /// Folders in memory: each path's listing.
    #[derive(Default)]
    struct Fake(HashMap<PathBuf, Listing>);

    impl Fake {
        fn folder(mut self, path: &str, entries: &[(&str, Kind)]) -> Self {
            let listing = Listing {
                entries: entries
                    .iter()
                    .map(|(n, k)| ((*n).to_owned(), k.clone()))
                    .collect(),
                ..Listing::of(Kind::Folder)
            };
            self.0.insert(at(path), listing);
            self
        }

        fn file(mut self, path: &str, size: u64) -> Self {
            self.0.insert(at(path), Listing::of(Kind::File(size)));
            self
        }

        fn unreadable(mut self, path: &str) -> Self {
            let denied = io::Error::from(io::ErrorKind::PermissionDenied);
            self.0.insert(at(path), Listing::failed(&denied));
            self
        }
    }

    impl Lister for Fake {
        async fn list(&self, path: &Path, max: usize) -> io::Result<Listing> {
            let mut listing = self
                .0
                .get(path)
                .cloned()
                .unwrap_or(Listing::of(Kind::Missing));
            if listing.entries.len() > max {
                listing.entries.truncate(max);
                listing.truncated = true;
            }
            Ok(listing)
        }
    }

    fn entry(relative: &str, size: u64, folder: bool) -> Entry {
        Entry {
            relative: relative.to_owned(),
            path: at("/u").join(relative),
            size,
            folder,
        }
    }

    fn failure(relative: &str, reason: &'static str) -> Failure {
        Failure {
            relative: relative.to_owned(),
            reason,
        }
    }

    #[tokio::test]
    async fn folders_are_listed_whole_without_following_links() {
        let fake = Fake::default()
            .folder(
                "/u/Photos",
                &[
                    ("b.jpg", Kind::File(20)),
                    ("2024", Kind::Folder),
                    ("a.jpg", Kind::File(10)),
                    ("empty", Kind::Folder),
                    ("link", Kind::Link),
                    ("fifo", Kind::Other),
                    ("locked", Kind::Folder),
                    ("only links", Kind::Folder),
                ],
            )
            .folder("/u/Photos/2024", &[("c.jpg", Kind::File(30))])
            .folder("/u/Photos/empty", &[])
            .folder("/u/Photos/only links", &[("up", Kind::Link)])
            .file("/u/notes.txt", 5)
            .unreadable("/u/Photos/locked")
            .unreadable("/u/secret.txt");
        let manifest = list_items(
            &fake,
            vec![
                at("/u/Photos"),
                at("/u/notes.txt"),
                at("/u/gone"),
                PathBuf::from("relative.txt"),
                at("/u/secret.txt"),
            ],
        )
        .await
        .unwrap();
        let photos = &manifest.items[0];
        assert_eq!(photos.name, "Photos");
        assert_eq!(
            photos.kind,
            ItemKind::Folder {
                entries: vec![
                    entry("Photos/a.jpg", 10, false),
                    entry("Photos/b.jpg", 20, false),
                    entry("Photos/2024/c.jpg", 30, false),
                    entry("Photos/empty", 0, true),
                    entry("Photos/only links", 0, true),
                ],
                failures: vec![
                    failure("Photos/fifo", "not_a_file"),
                    failure("Photos/link", "symlink"),
                    failure("Photos/locked", "unreadable"),
                    failure("Photos/only links/up", "symlink"),
                ],
            }
        );
        assert_eq!(manifest.items[1].kind, ItemKind::File(5));
        assert_eq!(manifest.items[2].kind, ItemKind::Failed("not_a_file"));
        assert_eq!(manifest.items[3].kind, ItemKind::Failed("not_a_file"));
        let secret = &manifest.items[4];
        assert_eq!(secret.kind, ItemKind::Failed("unreadable"));
        assert!(secret.detail.contains("denied"), "{}", secret.detail);
        assert_eq!(manifest.totals(), (6, 65));
        assert!(!manifest.items[2].sendable());
    }

    #[tokio::test]
    async fn batches_over_the_limits_are_refused() {
        let paths = (0..=MAX_ITEMS).map(|i| at(&format!("/u/{i}"))).collect();
        assert_eq!(
            list_items(&Fake::default(), paths).await,
            Err(ListError::TooManyItems(MAX_ITEMS + 1))
        );

        let many: Vec<(String, Kind)> = (0..=MAX_FILES)
            .map(|i| (format!("{i}.txt"), Kind::File(1)))
            .collect();
        let entries: Vec<(&str, Kind)> =
            many.iter().map(|(n, k)| (n.as_str(), k.clone())).collect();
        let fake = Fake::default().folder("/u/many", &entries);
        assert_eq!(
            list_items(&fake, vec![at("/u/many")]).await,
            Err(ListError::TooLarge)
        );

        let fake = Fake::default()
            .file("/u/a.bin", MAX_BYTES / 2)
            .file("/u/b.bin", MAX_BYTES / 2 + 1);
        assert_eq!(
            list_items(&fake, vec![at("/u/a.bin"), at("/u/b.bin")]).await,
            Err(ListError::TooLarge)
        );
    }

    #[tokio::test]
    async fn a_lost_broker_stops_the_listing() {
        struct Gone;
        impl Lister for Gone {
            async fn list(&self, _: &Path, _: usize) -> io::Result<Listing> {
                Err(io::Error::new(io::ErrorKind::NotConnected, "gone"))
            }
        }
        assert_eq!(
            list_items(&Gone, vec![at("/u/a")]).await,
            Err(ListError::NoBroker)
        );
    }

    #[test]
    fn local_listing_tells_files_folders_and_links_apart() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "abc").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
        let mut listing = list_local(dir.path(), 10);
        listing.entries.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(listing.kind, Some(Kind::Folder));
        let expected: Vec<(String, Kind)> = [
            ("a.txt", Kind::File(3)),
            ("loop", Kind::Link),
            ("sub", Kind::Folder),
        ]
        .into_iter()
        .filter(|(name, _)| cfg!(unix) || *name != "loop")
        .map(|(name, kind)| (name.to_owned(), kind))
        .collect();
        assert_eq!(listing.entries, expected);
        assert_eq!(
            list_local(&dir.path().join("a.txt"), 10).kind,
            Some(Kind::File(3))
        );
        assert_eq!(
            list_local(&dir.path().join("gone"), 10).kind,
            Some(Kind::Missing)
        );
        #[cfg(unix)]
        assert_eq!(
            list_local(&dir.path().join("loop"), 10).kind,
            Some(Kind::Link)
        );
        let truncated = list_local(dir.path(), 1);
        assert!(truncated.truncated);
    }

    #[test]
    fn relative_paths_stay_inside() {
        assert_eq!(
            split_relative("Photos/2024/a.jpg"),
            Some(vec!["Photos", "2024", "a.jpg"])
        );
        assert_eq!(split_relative("報告.pdf"), Some(vec!["報告.pdf"]));
        for bad in [
            "",
            "/etc/passwd",
            "a//b",
            "a/./b",
            "a/../../b",
            "..",
            "a/",
            "a/b\0c",
        ] {
            assert_eq!(split_relative(bad), None, "{bad:?}");
        }
        let deep = vec!["d"; MAX_DEPTH + 1].join("/");
        assert_eq!(split_relative(&deep), None);
        let long = "x".repeat(MAX_PATH_BYTES + 1);
        assert_eq!(split_relative(&long), None);
    }
}
