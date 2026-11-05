//! Files the user sends to other devices, read by the broker as the user
//! (docs/architecture.md §9).
//!
//! Any process of the user can ask the agent to send a file, and the agent
//! runs as SYSTEM, which may read files the user may not. So the agent
//! never looks at them: the broker lists the paths and folders sent, one
//! folder at a time and without following links (`list`), opens each file
//! with the user's rights, reports its size, hashes it, and reads the
//! parts the agent asks for. The walk through the folders, and the
//! transfer's state, resuming included, stay in the agent.
//!
//! Hashing reads the whole file, which may take a while, so it runs as a
//! task of its own and stops when the agent closes the file; reads of a
//! part are short and answered in order.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iw_proto::service::v1::{
    BrokerMessage, ChunkData, FileHashed, ListPath, OpenFailed, OpenFailure, OpenForSending,
    OpenedForSending, PathEntry, PathKind, PathListed, ReadChunk, broker_message,
};

use super::files::{blocking, failed};

/// The most the broker reads at once, whatever the agent asks for.
pub(crate) const MAX_CHUNK: usize = 1024 * 1024;
/// The most entries of a folder listed at once, whatever the agent asks
/// for: more than a batch may hold.
const MAX_LISTED: usize = 20_000;
/// About the most bytes of names listed at once, to stay well within a
/// message; a folder whose names are longer is listed in part, as
/// truncated.
const MAX_LISTED_BYTES: usize = 3 * 1024 * 1024;
/// Files open for sending at once; the agent sends four at a time.
const MAX_OPEN: usize = 16;

/// The files open for sending on one connection to the agent. Dropping it
/// closes them.
#[derive(Default)]
pub(crate) struct Outgoing {
    open: HashMap<u64, Arc<Source>>,
}

struct Source {
    file: File,
    /// Set once the agent closes the file, to stop a hash under way.
    closed: AtomicBool,
}

fn message(msg: broker_message::Msg) -> BrokerMessage {
    BrokerMessage { msg: Some(msg) }
}

fn refused(handle: u64, reason: OpenFailure, error: impl ToString) -> BrokerMessage {
    message(broker_message::Msg::OpenFailed(OpenFailed {
        handle,
        reason: reason.into(),
        error: error.to_string(),
    }))
}

impl Outgoing {
    /// Opens a file to send, as the user.
    pub(crate) async fn open(&mut self, open: OpenForSending) -> BrokerMessage {
        let handle = open.handle;
        if self.open.contains_key(&handle) {
            return refused(handle, OpenFailure::Unspecified, "the handle is in use");
        }
        if self.open.len() >= MAX_OPEN {
            return refused(
                handle,
                OpenFailure::Unspecified,
                "too many files are open for sending",
            );
        }
        let path = PathBuf::from(&open.path);
        if !path.is_absolute() {
            return refused(
                handle,
                OpenFailure::Unspecified,
                "refused a path that is not absolute",
            );
        }
        // File names stay out of the logs, which go into diagnostics
        // reports; the transfer id says which transfer it was.
        let opened = blocking(move || Ok(open_to_read(&path)))
            .await
            .unwrap_or_else(|e| Err((OpenFailure::Unspecified, e)));
        match opened {
            Ok((file, size)) => {
                tracing::info!(transfer = %open.transfer_id, size, "opened a file to send");
                let source = Source {
                    file,
                    closed: AtomicBool::new(false),
                };
                self.open.insert(handle, Arc::new(source));
                message(broker_message::Msg::OpenedForSending(OpenedForSending {
                    handle,
                    size,
                }))
            }
            Err((reason, e)) => {
                tracing::info!(
                    transfer = %open.transfer_id,
                    error = %e,
                    "cannot open a file to send"
                );
                refused(handle, reason, e)
            }
        }
    }

    /// The work of hashing a file open for sending to its end, to run as a
    /// task of its own: it reads the whole file.
    pub(crate) fn hash(
        &self,
        handle: u64,
    ) -> impl Future<Output = BrokerMessage> + Send + 'static + use<> {
        let source = self.open.get(&handle).cloned();
        async move {
            let Some(source) = source else {
                return failed(handle, "no such file is open");
            };
            match blocking(move || source.hash()).await {
                Ok((size, hash)) => message(broker_message::Msg::FileHashed(FileHashed {
                    handle,
                    size,
                    blake3: hash.as_bytes().to_vec(),
                })),
                Err(e) => failed(handle, format!("cannot read the file: {e}")),
            }
        }
    }

    /// Reads part of a file open for sending.
    pub(crate) async fn read(&self, read: ReadChunk) -> BrokerMessage {
        let handle = read.handle;
        let Some(source) = self.open.get(&handle).cloned() else {
            return failed(handle, "no such file is open");
        };
        let (offset, len) = (read.offset, (read.len as usize).min(MAX_CHUNK));
        match blocking(move || source.read(offset, len)).await {
            Ok(data) => message(broker_message::Msg::ChunkData(ChunkData {
                handle,
                offset,
                data,
            })),
            Err(e) => failed(handle, format!("cannot read the file: {e}")),
        }
    }

    /// Closes a file open for sending, stopping a hash under way.
    pub(crate) fn close(&mut self, handle: u64) {
        if let Some(source) = self.open.remove(&handle) {
            source.closed.store(true, Ordering::SeqCst);
        }
    }
}

impl Drop for Outgoing {
    fn drop(&mut self) {
        for source in self.open.values() {
            source.closed.store(true, Ordering::SeqCst);
        }
    }
}

impl Source {
    fn hash(&self) -> io::Result<(u64, blake3::Hash)> {
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; MAX_CHUNK];
        let mut offset = 0u64;
        loop {
            if self.closed.load(Ordering::SeqCst) {
                return Err(io::Error::other("the file was closed while it was hashed"));
            }
            let n = read_at(&self.file, &mut buf, offset)?;
            if n == 0 {
                return Ok((offset, hasher.finalize()));
            }
            hasher.update(&buf[..n]);
            offset += n as u64;
        }
    }

    /// Up to `len` bytes from `offset`; fewer only at the end of the file.
    fn read(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut data = vec![0u8; len];
        let mut filled = 0;
        while filled < len {
            let n = read_at(&self.file, &mut data[filled..], offset + filled as u64)?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        data.truncate(filled);
        Ok(data)
    }
}

/// Looks at a path the user sends, as the user, without following links:
/// what it is, and for a folder what is directly in it. A file is opened
/// as well, to tell one the user may not read.
pub(crate) async fn list(list: ListPath) -> BrokerMessage {
    let handle = list.handle;
    let path = PathBuf::from(&list.path);
    if !path.is_absolute() {
        return failed(handle, "refused a path that is not absolute");
    }
    let max = (list.max_entries as usize).min(MAX_LISTED);
    // Names stay out of the logs, as above.
    match blocking(move || Ok(list_path(&path, max))).await {
        Ok(mut listed) => {
            listed.handle = handle;
            message(broker_message::Msg::PathListed(listed))
        }
        Err(e) => failed(handle, format!("cannot list the path: {e}")),
    }
}

fn list_path(path: &Path, max: usize) -> PathListed {
    let kind_of = |e: &io::Error| match e.kind() {
        io::ErrorKind::NotFound => PathKind::Missing,
        _ => PathKind::Unreadable,
    };
    let only = |kind: PathKind, size: u64| PathListed {
        kind: kind.into(),
        size,
        ..PathListed::default()
    };
    let failed = |e: io::Error| PathListed {
        error: e.to_string(),
        ..only(kind_of(&e), 0)
    };
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) => return failed(e),
    };
    let file_type = metadata.file_type();
    // Junctions count as links: Rust takes every name surrogate for one.
    if file_type.is_symlink() {
        return only(PathKind::Link, 0);
    }
    if file_type.is_file() {
        return match File::open(path) {
            Ok(_) => only(PathKind::File, metadata.len()),
            Err(e) => failed(e),
        };
    }
    if !file_type.is_dir() {
        return only(PathKind::Other, 0);
    }
    let read = match std::fs::read_dir(path) {
        Ok(read) => read,
        Err(e) => return failed(e),
    };
    let mut listed = only(PathKind::Folder, 0);
    let mut bytes = 0;
    for entry in read {
        if listed.entries.len() >= max || bytes > MAX_LISTED_BYTES {
            listed.truncated = true;
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name();
        // A name that is not Unicode could not be named on the other side.
        let Some(name) = name.to_str().map(str::to_owned) else {
            listed.entries.push(PathEntry {
                name: name.to_string_lossy().into_owned(),
                kind: PathKind::Unreadable.into(),
                size: 0,
            });
            continue;
        };
        let (kind, size) = match entry.file_type() {
            Ok(t) if t.is_symlink() => (PathKind::Link, 0),
            Ok(t) if t.is_dir() => (PathKind::Folder, 0),
            Ok(t) if t.is_file() => match entry.metadata() {
                Ok(metadata) => (PathKind::File, metadata.len()),
                Err(e) => (kind_of(&e), 0),
            },
            Ok(_) => (PathKind::Other, 0),
            Err(e) => (kind_of(&e), 0),
        };
        bytes += name.len() + 16;
        listed.entries.push(PathEntry {
            name,
            kind: kind.into(),
            size,
        });
    }
    listed
}

/// One read at `offset`, without moving a cursor others share; 0 at the
/// end of the file.
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    loop {
        #[cfg(windows)]
        let read = std::os::windows::fs::FileExt::seek_read(file, buf, offset);
        #[cfg(unix)]
        let read = std::os::unix::fs::FileExt::read_at(file, buf, offset);
        match read {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            read => return read,
        }
    }
}

/// Opens `path` to read, with its length, or why not.
fn open_to_read(path: &Path) -> Result<(File, u64), (OpenFailure, io::Error)> {
    let classified = |e: io::Error| {
        let reason = match e.kind() {
            io::ErrorKind::NotFound => OpenFailure::NotAFile,
            io::ErrorKind::PermissionDenied => OpenFailure::AccessDenied,
            _ => OpenFailure::Unspecified,
        };
        (reason, e)
    };
    let not_a_file = || (OpenFailure::NotAFile, io::Error::other("not a file"));
    // Windows refuses to open a folder as a file with "access denied", so
    // folders are told apart first.
    if !std::fs::metadata(path).map_err(classified)?.is_file() {
        return Err(not_a_file());
    }
    let file = File::open(path).map_err(classified)?;
    let metadata = file.metadata().map_err(classified)?;
    if !metadata.is_file() {
        return Err(not_a_file());
    }
    Ok((file, metadata.len()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn kind(msg: &BrokerMessage) -> &broker_message::Msg {
        msg.msg.as_ref().unwrap()
    }

    fn open(handle: u64, path: &Path) -> OpenForSending {
        OpenForSending {
            handle,
            transfer_id: format!("t-{handle}"),
            path: path.to_string_lossy().into_owned(),
        }
    }

    #[tokio::test]
    async fn opens_hashes_and_reads_files_to_send() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("報告.pdf");
        let data: Vec<u8> = (0..(3 * MAX_CHUNK + 17) as u32)
            .map(|i| (i % 251) as u8)
            .collect();
        std::fs::write(&path, &data).unwrap();
        let mut outgoing = Outgoing::default();

        let opened = outgoing.open(open(1, &path)).await;
        assert!(
            matches!(kind(&opened), broker_message::Msg::OpenedForSending(o)
                if o.handle == 1 && o.size == data.len() as u64),
            "{opened:?}"
        );
        let hashed = outgoing.hash(1).await;
        let broker_message::Msg::FileHashed(hashed) = kind(&hashed) else {
            panic!("{hashed:?}");
        };
        assert_eq!(hashed.size, data.len() as u64);
        assert_eq!(hashed.blake3, blake3::hash(&data).as_bytes());

        // Parts, at most a mebibyte, shorter at the end and empty past it.
        for (offset, len, expected) in [
            (0u64, 10u32, &data[..10]),
            (5, u32::MAX, &data[5..5 + MAX_CHUNK]),
            (data.len() as u64 - 7, 100, &data[data.len() - 7..]),
            (data.len() as u64 + 1, 100, &[][..]),
        ] {
            let read = outgoing
                .read(ReadChunk {
                    handle: 1,
                    offset,
                    len,
                })
                .await;
            let broker_message::Msg::ChunkData(chunk) = kind(&read) else {
                panic!("{read:?}");
            };
            assert_eq!(chunk.offset, offset);
            assert_eq!(chunk.data, expected, "at {offset}");
        }

        // Once closed, nothing more is read.
        outgoing.close(1);
        let read = outgoing
            .read(ReadChunk {
                handle: 1,
                offset: 0,
                len: 1,
            })
            .await;
        assert!(matches!(kind(&read), broker_message::Msg::FileFailed(_)));
        assert!(matches!(
            kind(&outgoing.hash(1).await),
            broker_message::Msg::FileFailed(_)
        ));
    }

    #[tokio::test]
    async fn refuses_what_is_not_a_readable_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, "only the owner").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable_anyway = File::open(&secret).is_ok();
        let mut outgoing = Outgoing::default();

        let reason = |msg: BrokerMessage| match msg.msg {
            Some(broker_message::Msg::OpenFailed(failed)) => failed.reason(),
            other => panic!("expected a refusal, got {other:?}"),
        };
        let missing = outgoing.open(open(1, &dir.path().join("gone.txt"))).await;
        assert_eq!(reason(missing), OpenFailure::NotAFile);
        let folder = outgoing.open(open(2, dir.path())).await;
        assert_eq!(reason(folder), OpenFailure::NotAFile);
        let relative = outgoing.open(open(3, Path::new("relative.txt"))).await;
        assert_eq!(reason(relative), OpenFailure::Unspecified);
        // Root reads anything; the check is the user's.
        if !readable_anyway {
            let denied = outgoing.open(open(4, &secret)).await;
            assert_eq!(reason(denied), OpenFailure::AccessDenied);
        }
        assert!(outgoing.open.is_empty());
    }

    #[tokio::test]
    async fn lists_folders_without_following_links() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Photos");
        std::fs::create_dir_all(folder.join("2024")).unwrap();
        std::fs::write(folder.join("a.jpg"), "abc").unwrap();
        std::os::unix::fs::symlink(dir.path(), folder.join("up")).unwrap();
        let listed = |path: &Path, max: u32| {
            let request = ListPath {
                handle: 7,
                path: path.to_string_lossy().into_owned(),
                max_entries: max,
            };
            async move {
                match list(request).await.msg {
                    Some(broker_message::Msg::PathListed(listed)) => listed,
                    other => panic!("{other:?}"),
                }
            }
        };
        let mut photos = listed(&folder, 100).await;
        assert_eq!((photos.handle, photos.kind()), (7, PathKind::Folder));
        photos.entries.sort_by(|a, b| a.name.cmp(&b.name));
        let entries: Vec<(&str, PathKind, u64)> = photos
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.kind(), e.size))
            .collect();
        assert_eq!(
            entries,
            [
                ("2024", PathKind::Folder, 0),
                ("a.jpg", PathKind::File, 3),
                ("up", PathKind::Link, 0)
            ]
        );
        assert!(!photos.truncated);
        assert!(listed(&folder, 1).await.truncated);
        let file = listed(&folder.join("a.jpg"), 100).await;
        assert_eq!((file.kind(), file.size), (PathKind::File, 3));
        assert_eq!(listed(&folder.join("up"), 100).await.kind(), PathKind::Link);
        assert_eq!(
            listed(&folder.join("gone"), 100).await.kind(),
            PathKind::Missing
        );
        let relative = list(ListPath {
            handle: 8,
            path: "relative".to_owned(),
            max_entries: 1,
        })
        .await;
        assert!(matches!(
            kind(&relative),
            broker_message::Msg::FileFailed(_)
        ));
    }

    #[tokio::test]
    async fn a_hash_stops_once_the_file_is_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        std::fs::write(&path, vec![1u8; 4 * MAX_CHUNK]).unwrap();
        let mut outgoing = Outgoing::default();
        outgoing.open(open(1, &path)).await;
        let source = outgoing.open[&1].clone();
        outgoing.close(1);
        assert!(source.hash().is_err());
    }
}
