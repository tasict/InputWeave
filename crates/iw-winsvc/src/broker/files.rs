//! Received files, written by the broker as the user: the agent, which
//! runs as SYSTEM, streams what it receives here and keeps the transfer's
//! state (docs/architecture.md §9).
//!
//! Each transfer is written to a hidden part file in the folder,
//! `.inputweave-<transfer id>.part`, which a resumed transfer appends to.
//! Once it is complete the broker hashes it for the agent to compare with
//! what the sender announced, and on the agent's word gives it its name
//! (with " (1)", " (2)"… when taken, never replacing a file) or deletes it.
//!
//! A folder another device sends is received whole under a name of its
//! own: the broker creates it under the name asked for, or "name (1)",
//! "name (2)"… when taken, never using one already there (`claim_folder`);
//! its files are then kept in it, in the folders on the way to them, and
//! its empty folders made (`make_folder`). Where the user drops files
//! another device drags here is checked first (`check_folder`).
//!
//! The agent is trusted, but nothing it asks for may reach outside the
//! folder: transfer ids are plain, names, and each folder on the way to
//! one, are single components Windows allows, and folders are absolute.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iw_proto::service::v1::{
    BrokerMessage, CheckFolder, ClaimFolder, CloseFile, DiscardFile, FileData, FileFailed,
    FileFinished, FileKept, FileOpened, FinishFile, FolderReady, KeepFile, MakeFolder, OpenFile,
    broker_message,
};

const PART_PREFIX: &str = ".inputweave-";
const PART_SUFFIX: &str = ".part";
/// Folders in a path inside a folder another device sends, at most.
const MAX_DEPTH: usize = 64;
/// Part files left from transfers that never resumed are removed after
/// this long.
const STALE_PART: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How many numbered names are tried before giving up.
const MAX_NUMBERED: u32 = 10_000;

/// Folders whose stale part files were removed already; kept for the
/// broker's life.
pub(crate) type Cleaned = Arc<Mutex<HashSet<PathBuf>>>;

/// The part files open for one connection to the agent. Dropping it closes
/// them and keeps them, for their transfers to resume.
pub(crate) struct PartFiles {
    default_folder: Option<PathBuf>,
    cleaned: Cleaned,
    open: HashMap<u64, Part>,
}

struct Part {
    folder: PathBuf,
    path: PathBuf,
    /// `None` once finished, or after a write failed.
    file: Option<File>,
    size: u64,
    written: u64,
    failed: bool,
}

pub(super) fn failed(handle: u64, error: impl ToString) -> BrokerMessage {
    BrokerMessage {
        msg: Some(broker_message::Msg::FileFailed(FileFailed {
            handle,
            error: error.to_string(),
        })),
    }
}

/// Runs blocking file work off the async runtime.
pub(super) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|e| Err(io::Error::other(e)))
}

impl PartFiles {
    pub(crate) fn new(default_folder: Option<PathBuf>, cleaned: Cleaned) -> Self {
        Self {
            default_folder,
            cleaned,
            open: HashMap::new(),
        }
    }

    pub(crate) async fn open(&mut self, open: OpenFile) -> BrokerMessage {
        let handle = open.handle;
        if !valid_transfer_id(&open.transfer_id) {
            return failed(handle, "refused a transfer id that is not plain");
        }
        let folder = match open.folder.as_str() {
            "" => match &self.default_folder {
                Some(folder) => folder.clone(),
                None => return failed(handle, "there is no folder to receive into"),
            },
            folder => PathBuf::from(folder),
        };
        if !folder.is_absolute() {
            return failed(handle, "refused a folder that is not an absolute path");
        }
        let clean = self.cleaned.lock().unwrap().insert(folder.clone());
        let path = folder.join(format!("{PART_PREFIX}{}{PART_SUFFIX}", open.transfer_id));
        let size = open.size;
        let opened = blocking({
            let (folder, path) = (folder.clone(), path.clone());
            move || {
                std::fs::create_dir_all(&folder)?;
                if clean {
                    remove_stale_parts(&folder);
                }
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)?;
                let mut have = file.metadata()?.len();
                if have > size {
                    file.set_len(0)?;
                    have = 0;
                }
                Ok((file, have, free_space(&folder)))
            }
        })
        .await;
        let (file, have, free) = match opened {
            Ok(opened) => opened,
            Err(e) => return failed(handle, format!("cannot open the part file: {e}")),
        };
        self.open.insert(
            handle,
            Part {
                folder,
                path,
                file: Some(file),
                size,
                written: have,
                failed: false,
            },
        );
        BrokerMessage {
            msg: Some(broker_message::Msg::FileOpened(FileOpened {
                handle,
                offset: have,
                free_bytes: free.unwrap_or(u64::MAX),
            })),
        }
    }

    /// Appends to a part file. Returns a failure to report, the first time
    /// one happens; data for a failed or unknown handle is dropped.
    pub(crate) async fn write(&mut self, data: FileData) -> Option<BrokerMessage> {
        let part = self.open.get_mut(&data.handle)?;
        if part.failed {
            return None;
        }
        let len = data.data.len() as u64;
        let fits = part
            .written
            .checked_add(len)
            .is_some_and(|end| end <= part.size);
        let written = match part.file.take() {
            Some(mut file) if fits => {
                blocking(move || {
                    file.write_all(&data.data)?;
                    Ok(file)
                })
                .await
            }
            Some(_) => Err(io::Error::other("more data than the file's size")),
            None => Err(io::Error::other("the part file is not open")),
        };
        match written {
            Ok(file) => {
                part.file = Some(file);
                part.written += len;
                None
            }
            Err(e) => {
                part.failed = true;
                Some(failed(
                    data.handle,
                    format!("cannot write the part file: {e}"),
                ))
            }
        }
    }

    /// Writes a part file out, closes it and hashes it.
    pub(crate) async fn finish(&mut self, finish: FinishFile) -> BrokerMessage {
        let handle = finish.handle;
        let Some(part) = self.open.get_mut(&handle) else {
            return failed(handle, "no such file is open");
        };
        let file = match (part.failed, part.file.take()) {
            (false, Some(file)) => file,
            _ => return failed(handle, "the part file could not be written"),
        };
        let path = part.path.clone();
        let hashed = blocking(move || {
            file.sync_all()?;
            drop(file);
            hash_file(&path)
        })
        .await;
        match hashed {
            Ok((size, hash)) => BrokerMessage {
                msg: Some(broker_message::Msg::FileFinished(FileFinished {
                    handle,
                    size,
                    blake3: hash.as_bytes().to_vec(),
                })),
            },
            Err(e) => {
                part.failed = true;
                failed(handle, format!("cannot read the part file back: {e}"))
            }
        }
    }

    /// Gives a finished file its name, in the folders on the way to it.
    pub(crate) async fn keep(&mut self, keep: KeepFile) -> BrokerMessage {
        let handle = keep.handle;
        if !valid_name(&keep.name) {
            return failed(
                handle,
                "refused a name Windows does not allow, or with a path",
            );
        }
        let Some(subfolder) = subfolder(&keep.subfolder) else {
            return failed(handle, "refused folders Windows does not allow");
        };
        let Some(part) = self.open.remove(&handle) else {
            return failed(handle, "no such file is open");
        };
        if part.file.is_some() || part.failed {
            self.open.insert(handle, part);
            return failed(handle, "the file is not finished");
        }
        let (folder, name, from) = (part.folder.join(subfolder), keep.name, part.path);
        let kept = blocking(move || {
            std::fs::create_dir_all(&folder)?;
            for target in candidates(&folder, &name) {
                match rename_new(&from, &target) {
                    Ok(()) => return Ok(target),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(e),
                }
            }
            Err(io::Error::other("every numbered name is taken"))
        })
        .await;
        match kept {
            Ok(path) => BrokerMessage {
                msg: Some(broker_message::Msg::FileKept(FileKept {
                    handle,
                    path: path.to_string_lossy().into_owned(),
                })),
            },
            Err(e) => failed(handle, format!("cannot name the file: {e}")),
        }
    }

    /// Deletes a part file.
    pub(crate) async fn discard(&mut self, discard: DiscardFile) {
        if let Some(part) = self.open.remove(&discard.handle) {
            drop(part.file);
            let path = part.path;
            if let Err(e) = blocking(move || std::fs::remove_file(&path)).await
                && e.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(error = %e, "cannot delete a part file");
            }
        }
    }

    /// Closes a part file and keeps it, for its transfer to resume.
    pub(crate) fn close(&mut self, close: CloseFile) {
        self.open.remove(&close.handle);
    }
}

fn folder_ready(handle: u64, path: &Path) -> BrokerMessage {
    BrokerMessage {
        msg: Some(broker_message::Msg::FolderReady(FolderReady {
            handle,
            path: path.to_string_lossy().into_owned(),
        })),
    }
}

/// Creates a new folder for one another device sends: the name asked
/// for, or the first of "name (1)", "name (2)"… not taken, never one
/// already there.
pub(crate) async fn claim_folder(claim: ClaimFolder) -> BrokerMessage {
    let handle = claim.handle;
    let folder = PathBuf::from(&claim.folder);
    if !folder.is_absolute() || !valid_name(&claim.name) {
        return failed(handle, "refused a folder Windows does not allow");
    }
    let name = claim.name;
    let claimed = blocking(move || {
        std::fs::create_dir_all(&folder)?;
        for target in folder_candidates(&folder, &name) {
            match std::fs::create_dir(&target) {
                Ok(()) => return Ok(target),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::other("every numbered name is taken"))
    })
    .await;
    match claimed {
        Ok(path) => folder_ready(handle, &path),
        Err(e) => failed(handle, format!("cannot make the folder: {e}")),
    }
}

/// Creates an empty folder another device sent, and those on the way.
pub(crate) async fn make_folder(make: MakeFolder) -> BrokerMessage {
    let handle = make.handle;
    let folder = PathBuf::from(&make.folder);
    let Some(subfolder) = subfolder(&make.subfolder).filter(|_| folder.is_absolute()) else {
        return failed(handle, "refused a folder Windows does not allow");
    };
    let path = folder.join(subfolder);
    let made = blocking({
        let path = path.clone();
        move || std::fs::create_dir_all(&path)
    })
    .await;
    match made {
        Ok(()) => folder_ready(handle, &path),
        Err(e) => failed(handle, format!("cannot make the folder: {e}")),
    }
}

/// Checks that the user can create files in a folder, by creating one and
/// deleting it again.
pub(crate) async fn check_folder(check: CheckFolder) -> BrokerMessage {
    let handle = check.handle;
    let folder = PathBuf::from(&check.folder);
    if !folder.is_absolute() {
        return failed(handle, "refused a folder that is not an absolute path");
    }
    let checked = blocking({
        let folder = folder.clone();
        move || {
            if !std::fs::metadata(&folder)?.is_dir() {
                return Err(io::Error::other("not a folder"));
            }
            let probe = folder.join(format!(
                "{PART_PREFIX}check-{:x}-{:x}{PART_SUFFIX}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ));
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&probe)?;
            std::fs::remove_file(&probe)
        }
    })
    .await;
    match checked {
        Ok(()) => folder_ready(handle, &folder),
        Err(e) => failed(handle, format!("cannot write to the folder: {e}")),
    }
}

/// The folders on the way to something another device sent, "/"-separated
/// names Windows allows, as a relative path; empty for none. `None` for
/// anything else.
fn subfolder(path: &str) -> Option<PathBuf> {
    if path.is_empty() {
        return Some(PathBuf::new());
    }
    let parts: Vec<&str> = path.split('/').collect();
    (parts.len() <= MAX_DEPTH && parts.iter().all(|p| valid_name(p)))
        .then(|| parts.iter().collect())
}

/// `folder/name`, then `folder/name (1)`, `(2)`…: a folder is numbered at
/// its end, whatever dots its name has.
fn folder_candidates<'a>(folder: &'a Path, name: &'a str) -> impl Iterator<Item = PathBuf> + 'a {
    std::iter::once(folder.join(name))
        .chain((1..=MAX_NUMBERED).map(move |n| folder.join(format!("{name} ({n})"))))
}

fn hash_file(path: &Path) -> io::Result<(u64, blake3::Hash)> {
    let mut hasher = blake3::Hasher::new();
    let mut file = File::open(path)?;
    let size = io::copy(&mut file, &mut hasher)?;
    Ok((size, hasher.finalize()))
}

/// Transfer ids name part files, so they are kept plain.
pub(crate) fn valid_transfer_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Names Windows keeps for devices, with or without an extension.
const RESERVED: [&str; 32] = [
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3",
    "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// Whether `name` is the name of a file in a folder on Windows and nothing
/// more: not empty, `.` or `..`, no path separators, drive or stream
/// colons, wildcards, control characters or other characters Windows
/// refuses, not ending in a dot or space, not a device's name, and at most
/// 255 UTF-16 units.
pub(crate) fn valid_name(name: &str) -> bool {
    const REFUSED: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    if name.is_empty()
        || name.encode_utf16().count() > 255
        || name.ends_with(['.', ' '])
        || name.chars().any(|c| c.is_control() || REFUSED.contains(&c))
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or_default().trim_end();
    !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem))
}

/// `folder/name`, then `folder/name (1).ext`, `(2)`…
pub(crate) fn candidates<'a>(
    folder: &'a Path,
    name: &'a str,
) -> impl Iterator<Item = PathBuf> + 'a {
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    std::iter::once(folder.join(name))
        .chain((1..=MAX_NUMBERED).map(move |n| folder.join(format!("{stem} ({n}){extension}"))))
}

fn remove_stale_parts(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with(PART_PREFIX) && name.ends_with(PART_SUFFIX)) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age > STALE_PART));
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Moves `from` to `to` unless `to` exists, which fails with
/// `AlreadyExists`.
#[cfg(windows)]
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use windows::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    use windows::core::HSTRING;

    // SAFETY: both paths are valid strings for the call. Without
    // MOVEFILE_REPLACE_EXISTING an existing target is never replaced.
    unsafe {
        MoveFileExW(
            &HSTRING::from(from.as_os_str()),
            &HSTRING::from(to.as_os_str()),
            MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xffff))
}

#[cfg(not(windows))]
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::hard_link(from, to)?;
    std::fs::remove_file(from)
}

/// Free space for the user in `folder`, if known.
#[cfg(windows)]
fn free_space(folder: &Path) -> Option<u64> {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows::core::HSTRING;

    let mut free = 0u64;
    // SAFETY: the path is a valid string and the out pointer is valid for
    // the call.
    unsafe {
        GetDiskFreeSpaceExW(
            &HSTRING::from(folder.as_os_str()),
            Some(&mut free),
            None,
            None,
        )
    }
    .ok()
    .map(|()| free)
}

#[cfg(not(windows))]
fn free_space(_folder: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_with_paths_or_that_windows_refuses_are_invalid() {
        for name in [
            "報告.pdf",
            "notes",
            "a b.tar.gz",
            ".hidden",
            "COM10.txt",
            "console",
        ] {
            assert!(valid_name(name), "{name}");
        }
        for name in [
            "",
            ".",
            "..",
            "a/b",
            "..\\up",
            "C:x",
            "file.txt:stream",
            "what?",
            "star*",
            "tab\t",
            "dot.",
            "space ",
            "con",
            "NUL.txt",
            "com1.tar.gz",
            "Lpt9",
            "CONOUT$",
            "COM¹",
            "LPT³.log",
        ] {
            assert!(!valid_name(name), "{name:?}");
        }
        assert!(valid_name(&"長".repeat(255)));
        assert!(!valid_name(&"長".repeat(256)));
    }

    #[test]
    fn taken_names_get_numbers() {
        let folder = Path::new("/d");
        let names: Vec<PathBuf> = candidates(folder, "報告.pdf").take(3).collect();
        assert_eq!(
            names,
            [
                Path::new("/d/報告.pdf"),
                Path::new("/d/報告 (1).pdf"),
                Path::new("/d/報告 (2).pdf")
            ]
        );
        let names: Vec<PathBuf> = candidates(folder, ".profile").take(2).collect();
        assert_eq!(
            names,
            [Path::new("/d/.profile"), Path::new("/d/.profile (1)")]
        );
    }

    #[test]
    fn transfer_ids_are_plain() {
        assert!(valid_transfer_id("18f0a-1c2-3"));
        assert!(!valid_transfer_id("../x"));
        assert!(!valid_transfer_id(""));
        assert!(!valid_transfer_id(&"a".repeat(65)));
    }

    fn data(handle: u64, bytes: &[u8]) -> FileData {
        FileData {
            handle,
            data: bytes.to_vec(),
        }
    }

    fn kind(msg: &BrokerMessage) -> &broker_message::Msg {
        msg.msg.as_ref().unwrap()
    }

    #[tokio::test]
    async fn parts_resume_hash_and_never_replace_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Downloads").join("InputWeave");
        let mut files = PartFiles::new(Some(folder.clone()), Cleaned::default());
        let open = |handle| OpenFile {
            handle,
            transfer_id: "t-1".to_owned(),
            size: 6,
            folder: String::new(),
        };

        let opened = files.open(open(1)).await;
        assert!(
            matches!(kind(&opened), broker_message::Msg::FileOpened(o) if o.offset == 0),
            "{opened:?}"
        );
        assert!(files.write(data(1, b"abc")).await.is_none());
        // Cut off; a new attempt goes on from what is there.
        files.close(CloseFile { handle: 1 });
        let opened = files.open(open(2)).await;
        assert!(matches!(kind(&opened), broker_message::Msg::FileOpened(o) if o.offset == 3));
        assert!(files.write(data(2, b"def")).await.is_none());
        // Past the size: refused, and the handle is done for.
        assert!(matches!(
            files.write(data(2, b"!")).await.as_ref().map(kind),
            Some(broker_message::Msg::FileFailed(_))
        ));
        assert!(files.write(data(2, b"!")).await.is_none(), "reported once");

        // A fresh transfer whose part is complete.
        let mut files = PartFiles::new(Some(folder.clone()), Cleaned::default());
        files.open(open(3)).await;
        let finished = files.finish(FinishFile { handle: 3 }).await;
        let broker_message::Msg::FileFinished(finished) = kind(&finished) else {
            panic!("{finished:?}");
        };
        assert_eq!(finished.size, 6);
        assert_eq!(finished.blake3, blake3::hash(b"abcdef").as_bytes());

        std::fs::write(folder.join("x.txt"), "already here").unwrap();
        let refused = files
            .keep(KeepFile {
                handle: 3,
                name: "..\\x.txt".to_owned(),
                subfolder: String::new(),
            })
            .await;
        assert!(matches!(kind(&refused), broker_message::Msg::FileFailed(_)));
        let kept = files
            .keep(KeepFile {
                handle: 3,
                name: "x.txt".to_owned(),
                subfolder: String::new(),
            })
            .await;
        let broker_message::Msg::FileKept(kept) = kind(&kept) else {
            panic!("{kept:?}");
        };
        assert_eq!(Path::new(&kept.path), folder.join("x (1).txt"));
        assert_eq!(std::fs::read(&kept.path).unwrap(), b"abcdef");
        assert_eq!(
            std::fs::read(folder.join("x.txt")).unwrap(),
            b"already here"
        );
        assert!(!folder.join(".inputweave-t-1.part").exists());

        // Discarded parts are deleted.
        files
            .open(OpenFile {
                handle: 4,
                transfer_id: "t-2".to_owned(),
                size: 1,
                folder: folder.to_string_lossy().into_owned(),
            })
            .await;
        assert!(folder.join(".inputweave-t-2.part").exists());
        files.discard(DiscardFile { handle: 4 }).await;
        assert!(!folder.join(".inputweave-t-2.part").exists());
    }

    #[tokio::test]
    async fn folders_are_received_whole_under_names_of_their_own() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Downloads");
        std::fs::create_dir_all(folder.join("Photos")).unwrap();
        std::fs::write(folder.join("Photos").join("old.jpg"), "old").unwrap();
        let path = |msg: &BrokerMessage| match kind(msg) {
            broker_message::Msg::FolderReady(ready) => PathBuf::from(&ready.path),
            other => panic!("{other:?}"),
        };
        let claim = |name: &str| ClaimFolder {
            handle: 1,
            folder: folder.to_string_lossy().into_owned(),
            name: name.to_owned(),
        };
        // Taken: received as "Photos (1)", never into the one there.
        let claimed = path(&claim_folder(claim("Photos")).await);
        assert_eq!(claimed, folder.join("Photos (1)"));
        assert_eq!(
            path(&claim_folder(claim("Photos")).await),
            folder.join("Photos (2)")
        );
        for bad in ["..", "a\\b", "CON", ""] {
            assert!(matches!(
                kind(&claim_folder(claim(bad)).await),
                broker_message::Msg::FileFailed(_)
            ));
        }

        // A file kept in the folders on the way to it.
        let mut files = PartFiles::new(Some(folder.clone()), Cleaned::default());
        files
            .open(OpenFile {
                handle: 2,
                transfer_id: "t-3".to_owned(),
                size: 2,
                folder: String::new(),
            })
            .await;
        assert!(files.write(data(2, b"hi")).await.is_none());
        files.finish(FinishFile { handle: 2 }).await;
        let refused = files
            .keep(KeepFile {
                handle: 2,
                name: "c.jpg".to_owned(),
                subfolder: "Photos (1)/../..".to_owned(),
            })
            .await;
        assert!(matches!(kind(&refused), broker_message::Msg::FileFailed(_)));
        let kept = files
            .keep(KeepFile {
                handle: 2,
                name: "c.jpg".to_owned(),
                subfolder: "Photos (1)/2024".to_owned(),
            })
            .await;
        let broker_message::Msg::FileKept(kept) = kind(&kept) else {
            panic!("{kept:?}");
        };
        assert_eq!(
            Path::new(&kept.path),
            folder.join("Photos (1)").join("2024").join("c.jpg")
        );

        // Empty folders, with those on the way.
        let make = |subfolder: &str| MakeFolder {
            handle: 3,
            folder: folder.to_string_lossy().into_owned(),
            subfolder: subfolder.to_owned(),
        };
        let made = path(&make_folder(make("Photos (1)/empty/inner")).await);
        assert!(made.is_dir());
        assert!(matches!(
            kind(&make_folder(make("../x")).await),
            broker_message::Msg::FileFailed(_)
        ));

        // Where files dragged here may go.
        let check = |folder: &Path| CheckFolder {
            handle: 4,
            folder: folder.to_string_lossy().into_owned(),
        };
        assert_eq!(path(&check_folder(check(&folder)).await), folder);
        for bad in [
            folder.join("gone"),
            folder.join("Photos").join("old.jpg"),
            PathBuf::from("relative"),
        ] {
            assert!(matches!(
                kind(&check_folder(check(&bad)).await),
                broker_message::Msg::FileFailed(_)
            ));
        }
        let leftovers = std::fs::read_dir(&folder)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(PART_PREFIX))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[tokio::test]
    async fn refuses_what_would_leave_the_folder() {
        let mut files = PartFiles::new(None, Cleaned::default());
        for (transfer_id, folder) in [
            ("../../evil", "/tmp/x"),
            ("t", "relative\\folder"),
            ("t", ""),
        ] {
            let opened = files
                .open(OpenFile {
                    handle: 1,
                    transfer_id: transfer_id.to_owned(),
                    size: 1,
                    folder: folder.to_owned(),
                })
                .await;
            assert!(
                matches!(kind(&opened), broker_message::Msg::FileFailed(_)),
                "{transfer_id} in {folder}"
            );
        }
    }
}
