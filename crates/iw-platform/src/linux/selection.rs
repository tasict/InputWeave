//! What the X11 and Wayland clipboards (`x11::clipboard`,
//! `wayland::data_control`, `wayland::portal_clipboard`) share: content
//! written or promised here and how it answers a request for a target or
//! MIME type, reading another app's content a MIME type at a time, and
//! moving bytes through the pipes the Wayland protocols hand out. Formats
//! and file lists are `clipboard_formats`'.
//!
//! - A request is answered without waiting where it can (`serve_now`):
//!   what is known, as far as the pipe takes it. Promised content is
//!   fetched from its source as an app asks for it, which may take as long
//!   as it takes to come, so never on the thread that hears the compositor
//!   or the portal. A promise is fetched once for each format (or its
//!   files) however many requests wait for it meanwhile (`Promised::get`;
//!   the X11 owner thread keeps the requests waiting for it, and has one
//!   helper get it for them); what cannot be had and the files that
//!   arrived are kept for later requests, a format's bytes are not (the
//!   source keeps them, and gives them again at once). Nothing is fetched
//!   of a promise whose source was told it left the clipboard.
//! - Reading asks for each format from the best MIME type offered, the
//!   next when one holds nothing, up to a limit in all; an app that says
//!   nothing for a while, or takes too long in all, fails the read.
//!
//! Free of FFI but for the pipes, so it is tested on every Unix.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::linux::clipboard_formats::{
    self as formats, GNOME_COPIED_FILES, PASSWORD_HINT, SECRET, URI_LIST, Wire,
};
use crate::linux::common::{lock, ready, wait};
use crate::{ClipboardContent, ClipboardFormat, ClipboardPromise, ClipboardRead, FilesFetch};

/// How long an app may say nothing while it answers a request (on X11:
/// and between two parts of a large answer).
pub(crate) const ANSWER_WAIT: Duration = Duration::from_secs(3);
/// The longest a read of the clipboard (its content, or its files) may
/// take, however many answers it waits for.
pub(crate) const READ_WAIT: Duration = Duration::from_secs(8);
/// An app that takes nothing of an answer for this long is given up on.
pub(crate) const STALE: Duration = Duration::from_secs(60);
/// The most a list of copied files may take, in bytes.
pub(crate) const MAX_FILE_LIST: usize = 1024 * 1024;
/// The most the password manager's hint may take, in bytes.
const MAX_HINT: usize = 1024;

/// Bytes shared by every answer that carries them.
pub(crate) type Data = Arc<Vec<u8>>;

/// Content written here, its bytes shared by every answer.
pub(crate) struct Written {
    items: Vec<(ClipboardFormat, Data)>,
}

impl Written {
    pub(crate) fn new(content: &ClipboardContent) -> Self {
        Self {
            items: content
                .items
                .iter()
                .map(|(f, d)| (*f, Arc::new(d.clone())))
                .collect(),
        }
    }

    pub(crate) fn get(&self, format: ClipboardFormat) -> Option<&Data> {
        self.items
            .iter()
            .find(|(f, _)| *f == format)
            .map(|(_, d)| d)
    }

    pub(crate) fn len(&self) -> usize {
        self.items.iter().map(|(_, d)| d.len()).sum()
    }

    pub(crate) fn content(&self) -> ClipboardContent {
        ClipboardContent {
            items: self
                .items
                .iter()
                .map(|(f, d)| (*f, d.as_ref().clone()))
                .collect(),
        }
    }
}

/// What is on the clipboard because it was put there here.
#[derive(Clone, Default)]
pub(crate) enum Owned {
    #[default]
    Nothing,
    Content(Arc<Written>),
    Promise(Arc<Promised>),
}

impl Owned {
    /// What reading it back gives: the content written, unless larger
    /// than `max_len`; a promise reads as nothing, as the agent never
    /// reads its own again.
    pub(crate) fn read_back(&self, max_len: usize) -> ClipboardRead {
        match self {
            Owned::Content(written) if written.len() > max_len => {
                ClipboardRead::TooLarge(written.len())
            }
            Owned::Content(written) => ClipboardRead::Content(written.content()),
            Owned::Promise(_) | Owned::Nothing => ClipboardRead::Empty,
        }
    }

    /// A promise whose source was told it left the clipboard: nothing of
    /// it is offered or given any more.
    pub(crate) fn ended(&self) -> bool {
        matches!(self, Owned::Promise(promised) if promised.finished.load(Ordering::SeqCst))
    }

    /// Tells a promise's source it left the clipboard.
    pub(crate) fn finish(&self) {
        if let Owned::Promise(promised) = self {
            promised.finish();
        }
    }

    /// The MIME types it is offered as over `wire`.
    pub(crate) fn mimes(&self, wire: Wire) -> Vec<&'static str> {
        let mut mimes = Vec::new();
        let formats: Vec<ClipboardFormat> = match self {
            Owned::Nothing => Vec::new(),
            Owned::Content(written) => written.items.iter().map(|(f, _)| *f).collect(),
            Owned::Promise(promised) => {
                let promise = &promised.promise;
                let mut formats = promise.formats.clone();
                if !promise.files.is_empty() {
                    mimes.extend([URI_LIST, GNOME_COPIED_FILES, PASSWORD_HINT]);
                    if !formats.contains(&ClipboardFormat::Text) {
                        formats.push(ClipboardFormat::Text);
                    }
                }
                formats
            }
        };
        for format in formats {
            mimes.extend(formats::offered_targets(wire, format));
        }
        mimes
    }
}

/// What a promise's source is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Want {
    Format(ClipboardFormat),
    Files,
}

/// What the source gave.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Got {
    /// A format's bytes, or nothing when they cannot be had.
    Format(Option<Data>),
    /// The local paths of the files that arrived, none when they cannot
    /// be had.
    Files(Vec<PathBuf>),
    /// The files are still coming: kept for no one, asked again next time.
    Busy,
}

impl Got {
    /// Whether later requests may have it without asking the source.
    fn worth_keeping(&self) -> bool {
        match self {
            Got::Format(data) => data.is_none(),
            Got::Files(_) => true,
            Got::Busy => false,
        }
    }
}

/// The fetches of a promise: those under way, and what was kept.
#[derive(Default)]
struct Fetches {
    kept: HashMap<Want, Got>,
    /// The run fetching each `Want` now.
    running: HashMap<Want, u64>,
    /// How many requests wait for each run.
    waiting: HashMap<u64, usize>,
    /// What a run gave, until the last request waiting for it took it.
    given: HashMap<u64, (Got, usize)>,
    runs: u64,
}

/// A promise on the clipboard, until it leaves.
pub(crate) struct Promised {
    promise: ClipboardPromise,
    /// Set under `fetches`' lock, so a request deciding there whether to
    /// fetch sees it; read without it elsewhere.
    finished: AtomicBool,
    fetches: Mutex<Fetches>,
    fetched: Condvar,
}

impl Promised {
    pub(crate) fn new(promise: ClipboardPromise) -> Self {
        Self {
            promise,
            finished: AtomicBool::new(false),
            fetches: Mutex::new(Fetches::default()),
            fetched: Condvar::new(),
        }
    }

    /// Tells the source the promise left the clipboard, once. Requests
    /// waiting for a fetch get nothing now, and none starts one after.
    pub(crate) fn finish(&self) {
        let first = {
            let _fetches = self.fetches();
            !self.finished.swap(true, Ordering::SeqCst)
        };
        if first {
            self.fetched.notify_all();
            self.promise.source.finished();
        }
    }

    fn fetches(&self) -> std::sync::MutexGuard<'_, Fetches> {
        lock(&self.fetches)
    }

    /// Identifies the promise among others.
    pub(crate) fn key(self: &Arc<Self>) -> usize {
        Arc::as_ptr(self) as usize
    }

    /// What the source gives for `want`: nothing of an ended promise,
    /// what was kept at once, else the answer of the fetch under way for
    /// it, else a fetch of its own, which every request for the same
    /// meanwhile waits for. Decided under one lock, which `finish` takes
    /// too: a promise ended before is never fetched.
    pub(crate) fn get(&self, want: Want) -> Got {
        let mut fetches = self.fetches();
        if self.finished.load(Ordering::SeqCst) {
            return nothing(want);
        }
        if let Some(got) = fetches.kept.get(&want) {
            return got.clone();
        }
        if let Some(&run) = fetches.running.get(&want) {
            *fetches.waiting.entry(run).or_default() += 1;
            loop {
                if let Some((got, left)) = fetches.given.get_mut(&run) {
                    let got = got.clone();
                    *left -= 1;
                    if *left == 0 {
                        fetches.given.remove(&run);
                    }
                    return got;
                }
                if self.finished.load(Ordering::SeqCst) {
                    // Not waiting for the fetch any more.
                    if let Some(left) = fetches.waiting.get_mut(&run) {
                        *left -= 1;
                        if *left == 0 {
                            fetches.waiting.remove(&run);
                        }
                    }
                    return nothing(want);
                }
                fetches = wait(&self.fetched, fetches);
            }
        }
        fetches.runs += 1;
        let run = fetches.runs;
        fetches.running.insert(want, run);
        drop(fetches);
        // Even if the source panics, the requests waiting get an answer.
        let mut fetch = Fetch {
            promised: self,
            want,
            run,
            got: Got::Busy,
        };
        fetch.got = self.fetch(want);
        fetch.got.clone()
    }

    /// What `get` would give at once, without asking the source: what
    /// was kept, or nothing of an ended promise. `None` when only a fetch
    /// can tell.
    pub(crate) fn peek(&self, want: Want) -> Option<Got> {
        if self.finished.load(Ordering::SeqCst) {
            return Some(nothing(want));
        }
        self.fetches().kept.get(&want).cloned()
    }

    /// Asks the source, which may take as long as the content takes to
    /// come.
    fn fetch(&self, want: Want) -> Got {
        let source = &self.promise.source;
        match want {
            Want::Format(format) => Got::Format(source.fetch(format).map(Arc::new)),
            Want::Files => match source.fetch_files() {
                FilesFetch::Ready(paths) => Got::Files(paths.into_iter().flatten().collect()),
                FilesFetch::Busy => Got::Busy,
                FilesFetch::Unavailable => Got::Files(Vec::new()),
            },
        }
    }
}

/// What an ended promise gives for `want`.
fn nothing(want: Want) -> Got {
    match want {
        Want::Format(_) => Got::Format(None),
        Want::Files => Got::Files(Vec::new()),
    }
}

/// A fetch under way, which gives what it got to the requests waiting for
/// it as it ends.
struct Fetch<'a> {
    promised: &'a Promised,
    want: Want,
    run: u64,
    got: Got,
}

impl Drop for Fetch<'_> {
    fn drop(&mut self) {
        let mut fetches = self.promised.fetches();
        fetches.running.remove(&self.want);
        if self.got.worth_keeping() {
            fetches.kept.insert(self.want, self.got.clone());
        }
        if let Some(waiting) = fetches.waiting.remove(&self.run) {
            fetches.given.insert(self.run, (self.got.clone(), waiting));
            self.promised.fetched.notify_all();
        }
    }
}

/// A promise let go of anywhere, even in a write never carried out, has
/// left the clipboard.
impl Drop for Promised {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Bytes an answer carries.
pub(crate) enum Bytes {
    Shared(Data),
    Made(Vec<u8>),
    Fixed(&'static [u8]),
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        match self {
            Bytes::Shared(data) => data,
            Bytes::Made(data) => data,
            Bytes::Fixed(data) => data,
        }
    }
}

impl Bytes {
    /// The bytes, shared as they are where they can be.
    pub(crate) fn into_data(self) -> Data {
        match self {
            Bytes::Shared(data) => data,
            Bytes::Made(data) => Arc::new(data),
            Bytes::Fixed(data) => Arc::new(data.to_vec()),
        }
    }

    /// `data` as `mime` has it: shared unless it had to be converted.
    fn of(format: ClipboardFormat, mime: &str, data: &Data) -> Self {
        match formats::encode(format, mime, data) {
            Cow::Borrowed(_) => Bytes::Shared(data.clone()),
            Cow::Owned(made) => Bytes::Made(made),
        }
    }
}

/// How a request for a MIME type of what is owned here is answered.
pub(crate) enum Answer {
    Now(Bytes),
    /// Once the source gave `Want`.
    Fetch(Arc<Promised>, Want),
    /// It holds nothing of it.
    Nothing,
}

/// How `owned` answers a request for `mime`: at once what is known, the
/// rest once the source gave it.
pub(crate) fn answer(owned: &Owned, mime: &str) -> Answer {
    match owned {
        Owned::Nothing => Answer::Nothing,
        Owned::Content(written) => formats::format_of(mime)
            .and_then(|format| Some(Bytes::of(format, mime, written.get(format)?)))
            .map_or(Answer::Nothing, Answer::Now),
        Owned::Promise(promised) => {
            let promise = &promised.promise;
            let files = !promise.files.is_empty();
            let format = formats::format_of(mime);
            if mime == PASSWORD_HINT && files {
                Answer::Now(Bytes::Fixed(SECRET))
            } else if (mime == URI_LIST || mime == GNOME_COPIED_FILES) && files {
                Answer::Fetch(promised.clone(), Want::Files)
            } else if let Some(format) = format.filter(|f| promise.formats.contains(f)) {
                Answer::Fetch(promised.clone(), Want::Format(format))
            } else if format == Some(ClipboardFormat::Text) && files {
                // Files with no text of their own: their names.
                let names = formats::names(&promise.files);
                let names = formats::encode(ClipboardFormat::Text, mime, &names).into_owned();
                Answer::Now(Bytes::Made(names))
            } else {
                Answer::Nothing
            }
        }
    }
}

/// The bytes `got` gives a request for `mime`: the format as `mime` has
/// it, or the files as its list.
pub(crate) fn bytes_of(mime: &str, got: &Got) -> Option<Bytes> {
    match got {
        Got::Format(data) => {
            let data = data.as_ref()?;
            Some(Bytes::of(formats::format_of(mime)?, mime, data))
        }
        Got::Files(paths) if !paths.is_empty() => Some(Bytes::Made(if mime == URI_LIST {
            formats::uri_list(paths)
        } else {
            formats::gnome_copied_files(paths)
        })),
        Got::Files(_) | Got::Busy => None,
    }
}

/// Answers a request for `mime` of `owned` into `pipe`, fetching a
/// promise's content first; blocks as long as that and the writing take.
/// Whether it gave anything.
pub(crate) fn serve(owned: &Owned, mime: &str, pipe: OwnedFd) -> bool {
    match serve_now(owned, mime, pipe) {
        Served::Done(gave) => gave,
        Served::Waits(waits) => waits.finish(),
    }
}

/// How far `serve_now` got.
pub(crate) enum Served {
    /// Answered (whether it gave anything), the pipe closed.
    Done(bool),
    /// More to do, which may wait: the rest of what the pipe did not take
    /// at once, or a promise's content to fetch first.
    Waits(Waits),
}

/// What is left of answering a request (`Waits::finish`).
pub(crate) struct Waits {
    pipe: std::fs::File,
    mime: String,
    left: Left,
}

enum Left {
    Rest(Bytes, usize),
    Fetch(Arc<Promised>, Want),
}

impl Waits {
    /// Carries the answer on to its end; blocks as long as that takes.
    /// Whether it gave anything.
    pub(crate) fn finish(self) -> bool {
        let (bytes, sent) = match self.left {
            Left::Rest(bytes, sent) => (bytes, sent),
            Left::Fetch(promised, want) => match bytes_of(&self.mime, &promised.get(want)) {
                Some(bytes) => (bytes, 0),
                None => return false,
            },
        };
        match write_pipe(self.pipe.into(), &bytes.as_ref()[sent..], STALE) {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!(error = %e, mime = self.mime, "a clipboard answer did not go through");
                false
            }
        }
    }
}

/// Answers a request for `mime` of `owned` into `pipe` as far as it goes
/// without waiting: what is known, as much as the pipe takes at once (most
/// answers fit), the pipe closed then. The rest, or a promise's content to
/// fetch first, is left to `Waits::finish`.
pub(crate) fn serve_now(owned: &Owned, mime: &str, pipe: OwnedFd) -> Served {
    let bytes = match answer(owned, mime) {
        Answer::Now(bytes) => bytes,
        Answer::Fetch(promised, want) => {
            return Served::Waits(Waits {
                pipe: pipe.into(),
                mime: mime.to_owned(),
                left: Left::Fetch(promised, want),
            });
        }
        // Closed with nothing in it: the app reads nothing.
        Answer::Nothing => return Served::Done(false),
    };
    let mut pipe = std::fs::File::from(pipe);
    if let Err(e) = nonblocking(&pipe) {
        tracing::debug!(error = %e, mime, "a clipboard answer did not go through");
        return Served::Done(false);
    }
    let data = bytes.as_ref();
    let mut sent = 0;
    while sent < data.len() {
        match pipe.write(&data[sent..]) {
            Ok(n) => sent += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                return Served::Waits(Waits {
                    pipe,
                    mime: mime.to_owned(),
                    left: Left::Rest(bytes, sent),
                });
            }
            // EPIPE among them: the app went away.
            Err(e) => {
                tracing::debug!(error = %e, mime, "a clipboard answer did not go through");
                return Served::Done(false);
            }
        }
    }
    Served::Done(true)
}

/// What asking the clipboard's owner for a MIME type found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Asked {
    /// Empty when the owner gave nothing.
    Data(Vec<u8>),
    /// Larger than the limit, to this many bytes at least.
    TooLarge(usize),
}

/// Each format from the best MIME type in `offered` that gives it, the
/// next when one holds nothing, up to `max_len` in all, as `wire` has
/// text; `ask` asks the owner for a MIME type, up to a number of bytes.
pub(crate) fn read_offered<E>(
    wire: Wire,
    offered: &[&str],
    max_len: usize,
    mut ask: impl FnMut(&str, usize) -> Result<Asked, E>,
) -> Result<ClipboardRead, E> {
    let mut content = ClipboardContent::default();
    for format in ClipboardFormat::ALL {
        for mime in formats::choices(format, offered) {
            let left = max_len.saturating_sub(content.len());
            match ask(mime, left)? {
                Asked::Data(data) => {
                    if let Some(data) = formats::decode(wire, format, mime, &data) {
                        content.items.push((format, data));
                        break;
                    }
                }
                Asked::TooLarge(size) => {
                    return Ok(ClipboardRead::TooLarge(content.len() + size));
                }
            }
        }
        if content.len() > max_len {
            return Ok(ClipboardRead::TooLarge(content.len()));
        }
    }
    Ok(if content.is_empty() {
        ClipboardRead::Empty
    } else {
        ClipboardRead::Content(content)
    })
}

/// Whether the owner marks what it offers as not to be shared.
pub(crate) fn sensitive<E>(
    offered: &[&str],
    mut ask: impl FnMut(&str, usize) -> Result<Asked, E>,
) -> Result<bool, E> {
    if !offered.contains(&PASSWORD_HINT) {
        return Ok(false);
    }
    Ok(match ask(PASSWORD_HINT, MAX_HINT)? {
        Asked::Data(hint) => formats::is_secret(&hint),
        Asked::TooLarge(_) => false,
    })
}

/// The files the owner offers, from GNOME's list or else the URI list.
pub(crate) fn files_offered<E>(
    offered: &[&str],
    mut ask: impl FnMut(&str, usize) -> Result<Asked, E>,
) -> Result<Vec<PathBuf>, E> {
    for mime in [GNOME_COPIED_FILES, URI_LIST] {
        if !offered.contains(&mime) {
            continue;
        }
        // A list with no local file in it may stand beside one that has
        // them.
        if let Asked::Data(data) = ask(mime, MAX_FILE_LIST)? {
            let files = formats::files_of(mime, &data);
            if !files.is_empty() {
                return Ok(files);
            }
        }
    }
    Ok(Vec::new())
}

fn nonblocking(fd: &impl AsRawFd) -> std::io::Result<()> {
    // SAFETY: fcntl on a descriptor this owns, with plain flags.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        // SAFETY: as above.
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Reads what the owner writes into `pipe` until it closes it, up to
/// `limit` bytes: past `quiet` without a byte, or at `deadline`, it is
/// an error.
pub(crate) fn read_pipe(
    pipe: impl Into<OwnedFd>,
    limit: usize,
    quiet: Duration,
    deadline: Instant,
) -> std::io::Result<Asked> {
    let mut pipe = std::fs::File::from(pipe.into());
    nonblocking(&pipe)?;
    let mut data = Vec::new();
    let mut part = vec![0; 64 * 1024];
    loop {
        match pipe.read(&mut part) {
            Ok(0) => return Ok(Asked::Data(data)),
            Ok(n) => {
                if data.len() + n > limit {
                    return Ok(Asked::TooLarge(data.len() + n));
                }
                data.extend_from_slice(&part[..n]);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() || !ready(&pipe, libc::POLLIN, quiet.min(left)) {
                    return Err(std::io::Error::new(
                        ErrorKind::TimedOut,
                        "the app holding the clipboard did not answer in time",
                    ));
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// Writes `data` into `pipe` and closes it; an app that takes nothing for
/// `stale` is given up on.
pub(crate) fn write_pipe(pipe: OwnedFd, data: &[u8], stale: Duration) -> std::io::Result<()> {
    let mut pipe = std::fs::File::from(pipe);
    nonblocking(&pipe)?;
    let mut sent = 0;
    while sent < data.len() {
        match pipe.write(&data[sent..]) {
            Ok(n) => sent += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if !ready(&pipe, libc::POLLOUT, stale) {
                    return Err(std::io::Error::new(
                        ErrorKind::TimedOut,
                        "the app pasting took nothing",
                    ));
                }
            }
            // EPIPE among them: the app went away.
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::{ClipboardSource, PromisedFile};

    #[derive(Default)]
    struct Source {
        fetches: AtomicUsize,
        file_fetches: AtomicUsize,
        finished: AtomicUsize,
        delay: Duration,
        busy: AtomicBool,
    }

    impl ClipboardSource for Source {
        fn fetch(&self, format: ClipboardFormat) -> Option<Vec<u8>> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(self.delay);
            (format == ClipboardFormat::Text).then(|| "promised".as_bytes().to_vec())
        }

        fn fetch_files(&self) -> FilesFetch {
            self.file_fetches.fetch_add(1, Ordering::SeqCst);
            if self.busy.load(Ordering::SeqCst) {
                FilesFetch::Busy
            } else {
                FilesFetch::Ready(vec![Some(PathBuf::from("/tmp/a b")), None])
            }
        }

        fn finished(&self) {
            self.finished.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn promised(source: &Arc<Source>, formats: &[ClipboardFormat], files: bool) -> Arc<Promised> {
        Arc::new(Promised::new(ClipboardPromise {
            formats: formats.to_vec(),
            bytes: 8,
            files: if files {
                vec![PromisedFile {
                    name: "a b".into(),
                    folder: false,
                }]
            } else {
                Vec::new()
            },
            source: source.clone(),
        }))
    }

    fn bytes(answer: Answer) -> Option<Vec<u8>> {
        match answer {
            Answer::Now(bytes) => Some(bytes.as_ref().to_vec()),
            _ => None,
        }
    }

    #[test]
    fn written_content_answers_its_mime_types() {
        let written = Owned::Content(Arc::new(Written::new(&ClipboardContent {
            items: vec![
                (ClipboardFormat::Text, "café".as_bytes().to_vec()),
                (ClipboardFormat::Png, b"\x89PNG".to_vec()),
            ],
        })));
        let mimes = written.mimes(Wire::X11);
        assert!(mimes.contains(&"text/plain;charset=utf-8"));
        assert!(mimes.contains(&"STRING"));
        assert!(mimes.contains(&"image/png"));
        assert!(!mimes.contains(&"text/html"));
        // Wayland has no Latin-1 text.
        let mimes = written.mimes(Wire::Wayland);
        assert!(mimes.contains(&"text/plain;charset=utf-8") && mimes.contains(&"image/png"));
        assert!(!mimes.contains(&"STRING") && !mimes.contains(&"TEXT"));
        assert_eq!(
            bytes(answer(&written, "UTF8_STRING")).unwrap(),
            "café".as_bytes()
        );
        assert_eq!(bytes(answer(&written, "STRING")).unwrap(), b"caf\xe9");
        assert!(matches!(answer(&written, "text/html"), Answer::Nothing));
        assert!(matches!(
            answer(&Owned::Nothing, "text/plain"),
            Answer::Nothing
        ));
        assert_eq!(written.read_back(4), ClipboardRead::TooLarge(9));
        assert!(matches!(written.read_back(100), ClipboardRead::Content(_)));
    }

    #[test]
    fn a_promise_of_files_offers_lists_names_and_the_hint() {
        let source = Arc::new(Source::default());
        let owned = Owned::Promise(promised(&source, &[], true));
        let mimes = owned.mimes(Wire::Wayland);
        for mime in [URI_LIST, GNOME_COPIED_FILES, PASSWORD_HINT, "UTF8_STRING"] {
            assert!(mimes.contains(&mime), "{mime}");
        }
        assert_eq!(bytes(answer(&owned, PASSWORD_HINT)).unwrap(), SECRET);
        assert_eq!(bytes(answer(&owned, "text/plain")).unwrap(), b"a b");
        let Answer::Fetch(promised, want) = answer(&owned, URI_LIST) else {
            panic!("files are fetched");
        };
        assert_eq!(want, Want::Files);
        let got = promised.get(want);
        assert_eq!(
            bytes_of(URI_LIST, &got).unwrap().as_ref(),
            b"file:///tmp/a%20b\r\n"
        );
        assert!(
            bytes_of(GNOME_COPIED_FILES, &got)
                .unwrap()
                .as_ref()
                .starts_with(b"copy\n")
        );
        // The files that came are kept.
        promised.get(Want::Files);
        assert_eq!(source.file_fetches.load(Ordering::SeqCst), 1);
        assert_eq!(source.fetches.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn what_was_kept_is_peeked_at_without_a_fetch() {
        let source = Arc::new(Source::default());
        let promised = promised(&source, &[ClipboardFormat::Text], true);
        assert_eq!(promised.peek(Want::Files), None);
        let got = promised.get(Want::Files);
        assert_eq!(promised.peek(Want::Files), Some(got));
        // A format's bytes are not kept: only a fetch tells.
        promised.get(Want::Format(ClipboardFormat::Text));
        assert_eq!(promised.peek(Want::Format(ClipboardFormat::Text)), None);
        assert_eq!(source.file_fetches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn nothing_is_fetched_of_an_ended_promise() {
        let source = Arc::new(Source::default());
        let promised = promised(&source, &[ClipboardFormat::Text], true);
        let owned = Owned::Promise(promised.clone());
        assert!(!owned.ended());
        owned.finish();
        assert!(owned.ended());
        assert_eq!(
            promised.get(Want::Format(ClipboardFormat::Text)),
            Got::Format(None)
        );
        assert_eq!(promised.get(Want::Files), Got::Files(Vec::new()));
        assert_eq!(source.fetches.load(Ordering::SeqCst), 0);
        assert_eq!(source.file_fetches.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn files_still_coming_are_asked_for_again() {
        let source = Arc::new(Source::default());
        source.busy.store(true, Ordering::SeqCst);
        let promised = promised(&source, &[], true);
        assert_eq!(promised.get(Want::Files), Got::Busy);
        assert!(bytes_of(URI_LIST, &Got::Busy).is_none());
        source.busy.store(false, Ordering::SeqCst);
        assert!(matches!(promised.get(Want::Files), Got::Files(f) if f.len() == 1));
        assert_eq!(source.file_fetches.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_promise_ending_as_a_request_comes_is_not_fetched() {
        let source = Arc::new(Source::default());
        let promised = promised(&source, &[ClipboardFormat::Text], false);
        // The request waits for the lock while the promise ends under it,
        // as `finish` ends it.
        let fetches = lock(&promised.fetches);
        let asking = {
            let promised = promised.clone();
            std::thread::spawn(move || promised.get(Want::Format(ClipboardFormat::Text)))
        };
        std::thread::sleep(Duration::from_millis(100));
        promised.finished.store(true, Ordering::SeqCst);
        drop(fetches);
        assert_eq!(asking.join().unwrap(), Got::Format(None));
        assert_eq!(source.fetches.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn requests_waiting_for_a_fetch_get_nothing_as_the_promise_ends() {
        let source = Arc::new(Source {
            delay: Duration::from_secs(2),
            ..Source::default()
        });
        let promised = promised(&source, &[ClipboardFormat::Text], false);
        let want = Want::Format(ClipboardFormat::Text);
        let asking = |promised: &Arc<Promised>| {
            let promised = promised.clone();
            std::thread::spawn(move || promised.get(want))
        };
        let fetching = asking(&promised);
        while source.fetches.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
        let waiting = asking(&promised);
        std::thread::sleep(Duration::from_millis(100));
        let ended = Instant::now();
        promised.finish();
        assert_eq!(waiting.join().unwrap(), Got::Format(None));
        assert!(ended.elapsed() < Duration::from_secs(1));
        // The fetch itself ends as the source returns; nobody waits for
        // what it got.
        fetching.join().unwrap();
        assert!(lock(&promised.fetches).given.is_empty());
        assert!(lock(&promised.fetches).waiting.is_empty());
    }

    #[test]
    fn requests_meanwhile_share_one_fetch_and_later_ones_ask_again() {
        let source = Arc::new(Source {
            delay: Duration::from_millis(300),
            ..Source::default()
        });
        let promised = promised(
            &source,
            &[ClipboardFormat::Text, ClipboardFormat::Html],
            false,
        );
        let threads: Vec<_> = (0..6)
            .map(|_| {
                let promised = promised.clone();
                std::thread::spawn(move || promised.get(Want::Format(ClipboardFormat::Text)))
            })
            .collect();
        for thread in threads {
            assert_eq!(
                thread.join().unwrap(),
                Got::Format(Some(Arc::new(b"promised".to_vec())))
            );
        }
        assert_eq!(source.fetches.load(Ordering::SeqCst), 1);
        // The source keeps the bytes; asked again, it gives them again.
        promised.get(Want::Format(ClipboardFormat::Text));
        assert_eq!(source.fetches.load(Ordering::SeqCst), 2);
        // What cannot be had is kept.
        assert_eq!(
            promised.get(Want::Format(ClipboardFormat::Html)),
            Got::Format(None)
        );
        assert_eq!(
            promised.get(Want::Format(ClipboardFormat::Html)),
            Got::Format(None)
        );
        assert_eq!(source.fetches.load(Ordering::SeqCst), 3);
        // Only what was promised is fetched.
        let owned = Owned::Promise(promised.clone());
        assert!(matches!(answer(&owned, "image/png"), Answer::Nothing));
        drop(owned);
        drop(promised);
        assert_eq!(source.finished.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn formats_are_read_from_the_best_mime_type_up_to_the_limit() {
        let offered = [
            "text/plain",
            "text/plain;charset=utf-8",
            "text/html",
            "image/png",
            PASSWORD_HINT,
        ];
        let mut asked = Vec::new();
        let read = read_offered::<()>(Wire::Wayland, &offered, 100, |mime, _| {
            asked.push(mime.to_owned());
            Ok(Asked::Data(match mime {
                // The best holds nothing: the next is asked.
                "text/plain;charset=utf-8" => Vec::new(),
                "text/plain" => b"a\r\nb".to_vec(),
                "text/html" => b"<b>x</b>".to_vec(),
                _ => b"not png".to_vec(),
            }))
        })
        .unwrap();
        assert_eq!(
            asked,
            [
                "text/plain;charset=utf-8",
                "text/plain",
                "text/html",
                "image/png"
            ]
        );
        let ClipboardRead::Content(content) = read else {
            panic!("{read:?}");
        };
        assert_eq!(content.get(ClipboardFormat::Text).unwrap(), b"a\nb");
        assert_eq!(content.get(ClipboardFormat::Html).unwrap(), b"<b>x</b>");
        assert_eq!(content.get(ClipboardFormat::Png), None);

        let large = read_offered::<()>(Wire::Wayland, &offered, 6, |mime, limit| {
            Ok(match mime {
                "text/html" => Asked::TooLarge(limit + 1),
                _ => Asked::Data(b"abc".to_vec()),
            })
        });
        assert_eq!(large, Ok(ClipboardRead::TooLarge(7)));
        assert_eq!(
            read_offered::<()>(Wire::Wayland, &["text/uri-list"], 6, |_, _| unreachable!()),
            Ok(ClipboardRead::Empty)
        );
    }

    #[test]
    fn the_hint_and_the_file_lists_are_read_when_offered() {
        let secret = |hint: &'static [u8]| {
            sensitive::<()>(&["text/plain", PASSWORD_HINT], |_, _| {
                Ok(Asked::Data(hint.to_vec()))
            })
            .unwrap()
        };
        assert!(secret(b"secret\n"));
        assert!(!secret(b"public"));
        assert!(!sensitive::<()>(&["text/plain"], |_, _| unreachable!()).unwrap());
        let files = files_offered::<()>(&[URI_LIST, GNOME_COPIED_FILES], |mime, _| {
            Ok(Asked::Data(match mime {
                GNOME_COPIED_FILES => b"copy\nsmb://nas/x".to_vec(),
                _ => b"file:///tmp/x\r\n".to_vec(),
            }))
        })
        .unwrap();
        assert_eq!(files, [PathBuf::from("/tmp/x")]);
    }

    #[test]
    fn pipes_carry_answers_within_their_limits() {
        let deadline = || Instant::now() + Duration::from_secs(5);
        let (reader, writer) = std::io::pipe().unwrap();
        let data = vec![7u8; 300 * 1024];
        let sent = data.clone();
        let writing = std::thread::spawn(move || write_pipe(writer.into(), &sent, STALE));
        assert_eq!(
            read_pipe(reader, 1 << 20, ANSWER_WAIT, deadline()).unwrap(),
            Asked::Data(data)
        );
        writing.join().unwrap().unwrap();

        let (reader, writer) = std::io::pipe().unwrap();
        let writing = std::thread::spawn(move || write_pipe(writer.into(), &[1; 200_000], STALE));
        assert!(matches!(
            read_pipe(reader, 1000, ANSWER_WAIT, deadline()).unwrap(),
            Asked::TooLarge(n) if n > 1000
        ));
        // The reader went away: the writer hears so, rather than wait.
        assert!(writing.join().unwrap().is_err());

        // An owner that says nothing.
        let (reader, writer) = std::io::pipe().unwrap();
        let started = Instant::now();
        let silent = read_pipe(reader, 10, Duration::from_millis(200), deadline());
        assert_eq!(silent.unwrap_err().kind(), ErrorKind::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(200));
        drop(writer);

        // A reader that takes nothing.
        let (reader, writer) = std::io::pipe().unwrap();
        let stuck = write_pipe(writer.into(), &[0; 1 << 20], Duration::from_millis(200));
        assert_eq!(stuck.unwrap_err().kind(), ErrorKind::TimedOut);
        drop(reader);
    }

    #[test]
    fn written_content_is_answered_at_once_as_far_as_the_pipe_takes_it() {
        let small = Owned::Content(Arc::new(Written::new(&ClipboardContent {
            items: vec![(ClipboardFormat::Text, b"small".to_vec())],
        })));
        let (reader, writer) = std::io::pipe().unwrap();
        assert!(matches!(
            serve_now(&small, "text/plain", writer.into()),
            Served::Done(true)
        ));
        let mut data = Vec::new();
        std::fs::File::from(OwnedFd::from(reader))
            .read_to_end(&mut data)
            .unwrap();
        assert_eq!(data, b"small");

        // More than the pipe takes: the rest is left to finish.
        let large = vec![9u8; 1 << 20];
        let owned = Owned::Content(Arc::new(Written::new(&ClipboardContent {
            items: vec![(ClipboardFormat::Png, large.clone())],
        })));
        let (reader, writer) = std::io::pipe().unwrap();
        let Served::Waits(waits) = serve_now(&owned, "image/png", writer.into()) else {
            panic!("a megabyte fits no pipe");
        };
        let finishing = std::thread::spawn(move || waits.finish());
        let mut data = Vec::new();
        std::fs::File::from(OwnedFd::from(reader))
            .read_to_end(&mut data)
            .unwrap();
        assert!(finishing.join().unwrap());
        assert_eq!(data, large);

        // A promise is fetched by finish, not at once.
        let source = Arc::new(Source::default());
        let owned = Owned::Promise(promised(&source, &[ClipboardFormat::Text], false));
        let (_reader, writer) = std::io::pipe().unwrap();
        assert!(matches!(
            serve_now(&owned, "text/plain", writer.into()),
            Served::Waits(_)
        ));
        assert_eq!(source.fetches.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn serving_writes_what_is_owned_and_closes_the_pipe() {
        let source = Arc::new(Source::default());
        let owned = Owned::Promise(promised(&source, &[ClipboardFormat::Text], false));
        let read = |mime: &str| {
            let (reader, writer) = std::io::pipe().unwrap();
            let gave = serve(&owned, mime, writer.into());
            let mut data = Vec::new();
            std::fs::File::from(OwnedFd::from(reader))
                .read_to_end(&mut data)
                .unwrap();
            (gave, data)
        };
        assert_eq!(read("text/plain"), (true, b"promised".to_vec()));
        assert_eq!(read("text/html"), (false, Vec::new()));
        assert_eq!(source.fetches.load(Ordering::SeqCst), 1);
    }
}
