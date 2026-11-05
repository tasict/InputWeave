//! The local clipboard, worked on in order on a thread of its own. Reading
//! a large clipboard (up to the lazy limit, with an image converted to PNG
//! on the way) takes time, and must never hold up the engine, which routes
//! input; the engine asks for work here and hears back as an event
//! (`EngineEvent::Local`). The thread also keeps the change count the
//! clipboard last moved between devices at, so an unchanged clipboard is
//! never read, and the order of reads and writes stays the order they were
//! asked in.
//!
//! An offer that has files is handed to what holds such promises here when
//! the clipboard cannot (the Windows app, `Clipboard::hand_over`) without
//! waiting for it: reads, checks and writes asked meanwhile go on, and what
//! came of it comes back as work of its own, in order with the rest
//! (`LocalOp::Settled`). The thread counts its own writes, so a promise
//! handed over is never put on the clipboard over something written here
//! since, nor promised otherwise once something was.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Weak};

use iw_core::layout::DeviceId;
use iw_platform::{
    Clipboard, ClipboardContent, ClipboardFormat, ClipboardPromise, ClipboardRead, Current,
    HandOver, HandedOver, PlatformError,
};
use iw_proto::peer::v1::ClipboardSkip;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::engine::EngineEvent;
use super::lazy_clipboard::Held;

/// What the clipboard is read for, as the cursor moves. `offers` says
/// whether the device it goes to takes offers, `files` whether it takes
/// files in them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Purpose {
    /// To send to `to`, the device the cursor went to from here.
    Push {
        to: DeviceId,
        offers: bool,
        files: bool,
    },
    /// To answer `controller`'s request `id`.
    Answer {
        controller: DeviceId,
        id: u64,
        offers: bool,
        files: bool,
    },
}

impl Purpose {
    /// Whether the content may be offered rather than sent.
    pub(crate) fn offers(&self) -> bool {
        match self {
            Self::Push { offers, .. } | Self::Answer { offers, .. } => *offers,
        }
    }

    /// Whether files copied here may be offered.
    pub(crate) fn files(&self) -> bool {
        match self {
            Self::Push { files, .. } | Self::Answer { files, .. } => *files,
        }
    }
}

/// A pull of an offer this device made, to answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Serve {
    pub(crate) from: DeviceId,
    pub(crate) pull_id: u64,
    pub(crate) offer_id: u64,
    /// The formats wanted; all when empty.
    pub(crate) mimes: Vec<String>,
}

/// A pull of the files of an offer this device made, to answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FilesServe {
    pub(crate) from: DeviceId,
    pub(crate) pull_id: u64,
    pub(crate) offer_id: u64,
}

/// The clipboard read to move on: its content, its change count, and the
/// files and folders copied there, if any, which are offered whatever
/// their size.
pub(crate) type Taken = (ClipboardContent, u64, Vec<PathBuf>);

/// Why content is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WritePurpose {
    /// It arrived whole from `origin`.
    Arrived { origin: DeviceId },
    /// It was pulled for the offer `held`, this clipboard being unable to
    /// promise.
    Pulled { held: Held },
}

pub(crate) enum LocalOp {
    /// Read it to move on, up to `limit` bytes, unless it is unchanged since
    /// it last moved.
    Take {
        purpose: Purpose,
        limit: usize,
    },
    /// Read it for a pull of an offer made at change count `change`, which
    /// it must still have.
    Serve {
        serve: Serve,
        change: u64,
        limit: usize,
    },
    /// Check the clipboard still holds what was offered at change count
    /// `change`, for a pull of its files.
    CheckFiles {
        pull: FilesServe,
        change: u64,
    },
    /// Write `content`; when `expect` is set, only if the change count is
    /// still that, nothing having been copied here since.
    Write {
        content: ClipboardContent,
        expect: Option<u64>,
        purpose: WritePurpose,
    },
    /// Promise an offer; `anew`, only if the clipboard still has that
    /// change count, the one the promise made of it before left it at:
    /// nothing was written or copied since.
    Promise {
        promise: ClipboardPromise,
        held: Held,
        anew: Option<u64>,
    },
    /// What came of the offer `held`, handed over at change count `before`
    /// after the thread's `writes`th write of its own.
    Settled {
        held: Held,
        outcome: HandOver,
        before: u64,
        writes: u64,
    },
    SetSync(bool),
}

/// Why content was not written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unwritten {
    /// Something was copied here since the offer was taken.
    Superseded,
    /// It is larger than this clipboard takes in one write.
    TooLarge,
    Failed,
}

/// Why an offer was not promised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unpromised {
    /// This clipboard cannot promise; its change count is `change`.
    Unsupported {
        change: u64,
    },
    /// It is larger than this clipboard takes in one write (the Windows
    /// broker's limit), so it is not taken at all.
    TooLarge,
    /// Something else was copied since: before it was made anew, or while
    /// it was promised.
    Superseded,
    Failed,
}

/// Work done, for the engine.
#[derive(Debug)]
pub(crate) enum LocalDone {
    /// The clipboard and its change count, or why it does not move.
    Taken {
        purpose: Purpose,
        result: Result<Taken, ClipboardSkip>,
    },
    /// Whether the clipboard still holds the offer whose files are pulled.
    FilesChecked {
        pull: FilesServe,
        result: Result<(), ClipboardSkip>,
    },
    Served {
        serve: Serve,
        result: Result<ClipboardContent, ClipboardSkip>,
    },
    Written {
        purpose: WritePurpose,
        formats: Vec<ClipboardFormat>,
        bytes: u64,
        result: Result<(), Unwritten>,
    },
    /// With the change count the promise left the clipboard at.
    Promised {
        held: Held,
        /// Made anew once its files arrived late.
        anew: bool,
        result: Result<u64, Unpromised>,
    },
}

/// Logs a clipboard that cannot be read or written: a warning, unless
/// nothing can do it for now (Windows without a user logged on).
pub(crate) fn clipboard_failed(e: &PlatformError, doing: &str) -> ClipboardSkip {
    if matches!(e, PlatformError::Unavailable(_)) {
        debug!(error = %e, "cannot {doing} the clipboard");
    } else {
        warn!(error = %e, "cannot {doing} the clipboard");
    }
    ClipboardSkip::Unavailable
}

/// The local clipboard's thread, as the engine sees it.
pub(crate) struct LocalClipboard {
    /// The thread ends once this goes; what settles a promise handed over
    /// holds it only as weakly.
    ops: Arc<std_mpsc::Sender<LocalOp>>,
    /// Whether the clipboard can promise files copied on another device.
    promises_files: bool,
    /// Whether it chooses the folder they are fetched into itself.
    stages_files: bool,
}

impl LocalClipboard {
    /// Starts the thread for `clipboard`, which tells `engine` what it did.
    pub(crate) fn start(
        clipboard: Box<dyn Clipboard>,
        engine: mpsc::WeakUnboundedSender<EngineEvent>,
    ) -> Self {
        let (ops, work) = std_mpsc::channel();
        let ops = Arc::new(ops);
        let promises_files = clipboard.promises_files();
        let stages_files = clipboard.stages_files();
        let queue = Arc::downgrade(&ops);
        let spawned = std::thread::Builder::new()
            .name("clipboard".to_owned())
            .spawn(move || {
                let mut local = Worker {
                    clipboard,
                    seen: None,
                    writes: Arc::default(),
                    queue,
                };
                for op in work {
                    let Some(done) = local.run(op) else {
                        continue;
                    };
                    let told = engine
                        .upgrade()
                        .is_some_and(|e| e.send(EngineEvent::Local(done)).is_ok());
                    if !told {
                        break;
                    }
                }
            });
        if let Err(e) = spawned {
            warn!(error = %e, "cannot start the clipboard's thread; the clipboard is not shared");
        }
        Self {
            ops,
            promises_files,
            stages_files,
        }
    }

    pub(crate) fn ask(&self, op: LocalOp) {
        let _ = self.ops.send(op);
    }

    pub(crate) fn promises_files(&self) -> bool {
        self.promises_files
    }

    pub(crate) fn stages_files(&self) -> bool {
        self.stages_files
    }
}

/// The clipboard and the change count it last moved between devices at:
/// read for sending, or written on arrival.
struct Worker {
    clipboard: Box<dyn Clipboard>,
    seen: Option<u64>,
    /// Writes and promises of its own so far, to tell a promise handed over
    /// whether something was written since.
    writes: Arc<AtomicU64>,
    /// Where what came of a promise handed over goes, in order with the
    /// rest.
    queue: Weak<std_mpsc::Sender<LocalOp>>,
}

impl Worker {
    fn run(&mut self, op: LocalOp) -> Option<LocalDone> {
        Some(match op {
            LocalOp::Take { purpose, limit } => LocalDone::Taken {
                result: self.take(limit, purpose.offers(), purpose.files()),
                purpose,
            },
            LocalOp::Serve {
                serve,
                change,
                limit,
            } => {
                let result = self.serve(&serve, change, limit);
                LocalDone::Served { serve, result }
            }
            LocalOp::CheckFiles { pull, change } => LocalDone::FilesChecked {
                result: match self.count() {
                    Ok(count) if count == change => Ok(()),
                    Ok(_) => Err(ClipboardSkip::Changed),
                    Err(skip) => Err(skip),
                },
                pull,
            },
            LocalOp::Write {
                content,
                expect,
                purpose,
            } => {
                self.wrote();
                LocalDone::Written {
                    formats: content.items.iter().map(|(f, _)| *f).collect(),
                    bytes: content.len() as u64,
                    result: self.write(&content, expect),
                    purpose,
                }
            }
            LocalOp::Promise {
                promise,
                held,
                anew: None,
            } if !promise.files.is_empty() && !self.too_large(&promise) => {
                match self.hand_over(promise, held) {
                    None => return None,
                    Some((promise, held)) => {
                        self.wrote();
                        LocalDone::Promised {
                            result: self.promise(promise),
                            held,
                            anew: false,
                        }
                    }
                }
            }
            LocalOp::Promise {
                promise,
                held,
                anew,
            } => {
                self.wrote();
                LocalDone::Promised {
                    result: match anew.map(|expect| self.count().map(|now| now == expect)) {
                        Some(Ok(false)) => Err(Unpromised::Superseded),
                        Some(Err(_)) => Err(Unpromised::Failed),
                        _ => self.promise(promise),
                    },
                    held,
                    anew: anew.is_some(),
                }
            }
            LocalOp::Settled {
                held,
                outcome,
                before,
                writes,
            } => LocalDone::Promised {
                result: self.settled(outcome, before, writes),
                held,
                anew: false,
            },
            LocalOp::SetSync(on) => {
                self.clipboard.set_sync(on);
                return None;
            }
        })
    }

    /// Counts a write or promise of its own; returns how many so far.
    fn wrote(&self) -> u64 {
        self.writes.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn too_large(&self, promise: &ClipboardPromise) -> bool {
        self.clipboard
            .max_write()
            .is_some_and(|max| promise.bytes > max as u64)
    }

    /// Hands an offer that has files over (`Clipboard::hand_over`), not
    /// waiting for it: what comes of it is `LocalOp::Settled`. Hands it
    /// back when nothing takes it here, or the clipboard cannot be read.
    fn hand_over(
        &mut self,
        promise: ClipboardPromise,
        held: Held,
    ) -> Option<(ClipboardPromise, Held)> {
        let Ok(before) = self.count() else {
            return Some((promise, held));
        };
        let writes = self.wrote();
        let current: Current = {
            let counted = self.writes.clone();
            Arc::new(move || counted.load(Ordering::SeqCst) == writes)
        };
        let (queue, settling) = (self.queue.clone(), held.clone());
        let done: HandedOver = Box::new(move |outcome| {
            if let Some(queue) = queue.upgrade() {
                let _ = queue.send(LocalOp::Settled {
                    held: settling,
                    outcome,
                    before,
                    writes,
                });
            }
        });
        match self.clipboard.hand_over(promise, current, done) {
            Ok(()) => {
                // What it holds is not a copy by the user: neither the
                // promise, nor the content promised instead.
                self.seen = Some(before);
                None
            }
            Err(promise) => Some((promise, held)),
        }
    }

    /// What came of an offer handed over at change count `before`, after
    /// `writes` of its own: held, or, not taken, promised without its files
    /// while the clipboard is still as it was then. Something written here
    /// since, or copied, is what the clipboard holds; once the clipboard
    /// cannot be read, it fails.
    fn settled(&mut self, outcome: HandOver, before: u64, writes: u64) -> Result<u64, Unpromised> {
        let mut promise = match outcome {
            // Whatever was written here since replaced it, and what holds
            // it lets its source know.
            HandOver::Held => return Ok(before),
            HandOver::Returned(promise) => promise,
        };
        let unpromised = match self.count() {
            Err(_) => Some(Unpromised::Failed),
            Ok(now) if now != before || self.writes.load(Ordering::SeqCst) != writes => {
                Some(Unpromised::Superseded)
            }
            Ok(_) if promise.formats.is_empty() => Some(Unpromised::Failed),
            Ok(_) => None,
        };
        if let Some(why) = unpromised {
            // Nothing goes on the clipboard: what the offer had is let go.
            promise.source.finished();
            return Err(why);
        }
        promise.files.clear();
        self.wrote();
        self.promise(promise)
    }

    fn count(&self) -> Result<u64, ClipboardSkip> {
        self.clipboard
            .change_count()
            .map_err(|e| clipboard_failed(&e, "read"))
    }

    /// The clipboard up to `limit` bytes, unless unchanged since it last
    /// moved, or not to be shared. If it changes while being read, what was
    /// read is not used: the next move reads it again. Too large for a
    /// device that takes no offers (`offers` unset, `limit` the eager one),
    /// it is not counted as moved either: it may still be offered to the
    /// next. Files copied there go to a device that takes files in offers
    /// (`files`) whatever their size, with what else of the clipboard fits.
    fn take(&mut self, limit: usize, offers: bool, files: bool) -> Result<Taken, ClipboardSkip> {
        let count = self.count()?;
        if self.seen == Some(count) {
            return Err(ClipboardSkip::Unchanged);
        }
        let read = self
            .clipboard
            .read(limit)
            .map_err(|e| clipboard_failed(&e, "read"))?;
        let files = if offers && files && read != ClipboardRead::Sensitive {
            self.clipboard.files().unwrap_or_else(|e| {
                debug!(error = %e, "cannot read the files on the clipboard");
                Vec::new()
            })
        } else {
            Vec::new()
        };
        if self.count()? != count {
            return Err(ClipboardSkip::Unavailable);
        }
        if files.is_empty() && matches!(read, ClipboardRead::TooLarge(_)) && !offers {
            return Err(ClipboardSkip::TooLarge);
        }
        // Whatever it held has been looked at; it moves again only after
        // the next copy.
        self.seen = Some(count);
        if !files.is_empty() {
            let content = match read {
                ClipboardRead::Content(content) => content,
                _ => ClipboardContent::default(),
            };
            return Ok((content, count, files));
        }
        match read {
            ClipboardRead::Content(content) => Ok((content, count, Vec::new())),
            ClipboardRead::Empty => Err(ClipboardSkip::Empty),
            ClipboardRead::Sensitive => Err(ClipboardSkip::Sensitive),
            ClipboardRead::TooLarge(_) => Err(ClipboardSkip::TooLarge),
        }
    }

    /// The clipboard in the formats `serve` asks for, while it is still
    /// what was offered at change count `change`.
    fn serve(
        &self,
        serve: &Serve,
        change: u64,
        limit: usize,
    ) -> Result<ClipboardContent, ClipboardSkip> {
        if self.count()? != change {
            return Err(ClipboardSkip::Changed);
        }
        let read = self
            .clipboard
            .read(limit)
            .map_err(|e| clipboard_failed(&e, "read"))?;
        if self.count()? != change {
            return Err(ClipboardSkip::Changed);
        }
        match read {
            ClipboardRead::Content(mut content) => {
                if !serve.mimes.is_empty() {
                    content
                        .items
                        .retain(|(f, _)| serve.mimes.iter().any(|m| m == f.mime()));
                }
                if content.is_empty() {
                    Err(ClipboardSkip::Empty)
                } else {
                    Ok(content)
                }
            }
            ClipboardRead::Empty => Err(ClipboardSkip::Changed),
            ClipboardRead::Sensitive => Err(ClipboardSkip::Sensitive),
            ClipboardRead::TooLarge(_) => Err(ClipboardSkip::TooLarge),
        }
    }

    fn write(&mut self, content: &ClipboardContent, expect: Option<u64>) -> Result<(), Unwritten> {
        if let Some(expect) = expect
            && self.count().map_err(|_| Unwritten::Failed)? != expect
        {
            return Err(Unwritten::Superseded);
        }
        // What arrived may be larger than its offer said.
        if self
            .clipboard
            .max_write()
            .is_some_and(|max| content.len() > max)
        {
            return Err(Unwritten::TooLarge);
        }
        match self.clipboard.write(content) {
            Ok(count) => {
                // What was just written is not a copy by the user.
                self.seen = Some(count);
                Ok(())
            }
            Err(e) => {
                clipboard_failed(&e, "write");
                Err(Unwritten::Failed)
            }
        }
    }

    /// Promises the content, unless that is more than this clipboard takes
    /// in one write: written now or when pulled, it would fail there.
    /// Returns the change count it left the clipboard at. Not promised
    /// because something was copied here meanwhile, as while the Windows
    /// app was waited for, it is superseded.
    fn promise(&mut self, promise: ClipboardPromise) -> Result<u64, Unpromised> {
        if self
            .clipboard
            .max_write()
            .is_some_and(|max| promise.bytes > max as u64)
        {
            return Err(Unpromised::TooLarge);
        }
        let before = self.count().ok();
        match self.clipboard.write_promise(promise) {
            Ok(count) => {
                // What was just promised is not a copy by the user either.
                self.seen = Some(count);
                Ok(count)
            }
            // Not placed, and something was copied here meanwhile. An OS
            // error stays a failure: it may follow the clipboard clearing
            // itself (macOS), which moves the count too; and so does one
            // that cannot be read now.
            Err(PlatformError::Unavailable(_) | PlatformError::Unsupported(_))
                if before
                    .is_some_and(|before| matches!(self.count(), Ok(now) if now != before)) =>
            {
                Err(Unpromised::Superseded)
            }
            Err(PlatformError::Unsupported(_)) => match self.count() {
                Ok(change) => Err(Unpromised::Unsupported { change }),
                Err(_) => Err(Unpromised::Failed),
            },
            Err(e) => {
                clipboard_failed(&e, "write");
                Err(Unpromised::Failed)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iw_platform::mock::MockClipboard;

    fn text(s: &str) -> ClipboardContent {
        ClipboardContent {
            items: vec![(ClipboardFormat::Text, s.as_bytes().to_vec())],
        }
    }

    fn worker(clipboard: &MockClipboard) -> Worker {
        Worker {
            clipboard: Box::new(clipboard.clone()),
            seen: None,
            writes: Arc::default(),
            queue: Weak::new(),
        }
    }

    #[test]
    fn what_was_written_or_read_does_not_move_again() {
        let clipboard = MockClipboard::default();
        let mut local = worker(&clipboard);
        clipboard.copy(text("one"), false);
        let (content, _, _) = local.take(100, true, true).unwrap();
        assert_eq!(content, text("one"));
        assert_eq!(local.take(100, true, true), Err(ClipboardSkip::Unchanged));
        local.write(&text("two"), None).unwrap();
        assert_eq!(local.take(100, true, true), Err(ClipboardSkip::Unchanged));
        clipboard.copy(text("three"), false);
        assert_eq!(local.take(100, true, true).unwrap().0, text("three"));
        clipboard.copy(text("hunter2"), true);
        assert_eq!(local.take(100, true, true), Err(ClipboardSkip::Sensitive));
        assert_eq!(local.take(100, true, true), Err(ClipboardSkip::Unchanged));
        // Too large for a device that takes no offers: still there for the
        // next one.
        clipboard.copy(text("large"), false);
        assert_eq!(local.take(2, false, true), Err(ClipboardSkip::TooLarge));
        assert_eq!(local.take(100, true, true).unwrap().0, text("large"));
        // Too large for any: not read again.
        clipboard.copy(text("huge"), false);
        assert_eq!(local.take(2, true, true), Err(ClipboardSkip::TooLarge));
        assert_eq!(local.take(100, true, true), Err(ClipboardSkip::Unchanged));
    }

    #[test]
    fn files_copied_go_with_the_rest_to_a_device_that_takes_offers() {
        let clipboard = MockClipboard::default();
        let mut local = worker(&clipboard);
        let paths = vec![PathBuf::from("/Users/me/report.txt")];
        clipboard.copy_files(paths.clone(), text("report.txt"));
        // A device that takes no offers gets the names only.
        assert_eq!(
            local.take(100, false, true).unwrap(),
            (text("report.txt"), 1, Vec::new())
        );
        clipboard.copy_files(paths.clone(), text("report.txt"));
        assert_eq!(
            local.take(100, true, true).unwrap(),
            (text("report.txt"), 2, paths.clone())
        );
        // Files go whatever else is too large.
        clipboard.copy_files(paths.clone(), text("very long names"));
        assert_eq!(
            local.take(4, true, true).unwrap(),
            (ClipboardContent::default(), 3, paths.clone())
        );
        // Copied over with what is marked not to be shared: nothing moves.
        clipboard.copy_files(paths, text("secret"));
        clipboard.copy(text("hunter2"), true);
        assert_eq!(local.take(100, true, true), Err(ClipboardSkip::Sensitive));
    }

    #[test]
    fn a_promise_is_made_anew_only_over_its_own() {
        let clipboard = MockClipboard::default();
        let mut local = worker(&clipboard);
        let held = Held {
            origin: DeviceId::new("pc"),
            offer_id: 1,
            via: DeviceId::new("pc"),
            formats: vec![ClipboardFormat::Text],
            bytes: 1,
            files: Vec::new(),
            serial: 1,
        };
        let mut promise = |anew| {
            let promise = ClipboardPromise {
                formats: vec![ClipboardFormat::Text],
                bytes: 1,
                files: Vec::new(),
                source: std::sync::Arc::new(NoSource),
            };
            let held = held.clone();
            match local.run(LocalOp::Promise {
                promise,
                held,
                anew,
            }) {
                Some(LocalDone::Promised { result, .. }) => result,
                _ => panic!("not promised"),
            }
        };
        let first = promise(None).unwrap();
        // Nothing since: made anew over it.
        let second = promise(Some(first)).unwrap();
        assert_ne!(first, second);
        // Anything copied or written since, the agent's own writes of
        // other devices' content as much as the user's copies: never over
        // it.
        clipboard.copy(text("written"), false);
        assert_eq!(promise(Some(second)), Err(Unpromised::Superseded));
        let third = promise(None).unwrap();
        clipboard.copy(text("mine"), false);
        assert_eq!(promise(Some(third)), Err(Unpromised::Superseded));
        assert_eq!(clipboard.content(), text("mine"));
    }

    #[test]
    fn a_promise_of_files_is_not_read_again_as_not_to_be_shared() {
        let clipboard = MockClipboard::default();
        let mut local = worker(&clipboard);
        let promise = ClipboardPromise {
            formats: vec![ClipboardFormat::Text],
            bytes: 1,
            files: vec![iw_platform::PromisedFile {
                name: "report.txt".to_owned(),
                folder: false,
            }],
            source: std::sync::Arc::new(NoSource),
        };
        local.promise(promise).unwrap();
        // Marked for clipboard managers to leave it, as the macOS backend
        // marks it; the agent's own promise is not looked at again.
        assert!(matches!(
            local.clipboard.read(100),
            Ok(ClipboardRead::Sensitive)
        ));
        assert_eq!(local.take(100, true, true), Err(ClipboardSkip::Unchanged));
        clipboard.copy(text("mine"), false);
        assert_eq!(local.take(100, true, true).unwrap().0, text("mine"));
    }

    #[test]
    fn content_pulled_for_an_offer_does_not_replace_a_later_copy() {
        let clipboard = MockClipboard::default();
        clipboard.refuse_promises();
        let mut local = worker(&clipboard);
        let promise = |bytes| ClipboardPromise {
            formats: vec![ClipboardFormat::Text],
            bytes,
            files: Vec::new(),
            source: std::sync::Arc::new(NoSource),
        };
        // More than the clipboard takes in one write is not taken at all.
        clipboard.limit_writes(6);
        assert_eq!(local.promise(promise(7)), Err(Unpromised::TooLarge));
        let Err(Unpromised::Unsupported { change }) = local.promise(promise(6)) else {
            panic!("the mock was told to refuse promises");
        };
        // Copied here while the pull was on its way.
        clipboard.copy(text("mine"), false);
        assert_eq!(
            local.write(&text("pulled"), Some(change)),
            Err(Unwritten::Superseded)
        );
        assert_eq!(clipboard.content(), text("mine"));
        // Nothing copied meanwhile: it is written, unless larger than the
        // clipboard takes.
        let change = clipboard.change_count().unwrap();
        assert_eq!(
            local.write(&text("pulled and more"), Some(change)),
            Err(Unwritten::TooLarge)
        );
        local.write(&text("pulled"), Some(change)).unwrap();
        assert_eq!(clipboard.content(), text("pulled"));
    }

    #[test]
    fn a_pull_is_served_only_while_the_clipboard_is_what_was_offered() {
        let clipboard = MockClipboard::default();
        let local = worker(&clipboard);
        clipboard.copy(
            ClipboardContent {
                items: vec![
                    (ClipboardFormat::Text, b"t".to_vec()),
                    (ClipboardFormat::Html, b"<b>t</b>".to_vec()),
                ],
            },
            false,
        );
        let change = clipboard.change_count().unwrap();
        let serve = Serve {
            from: DeviceId::new("mini"),
            pull_id: 1,
            offer_id: 2,
            mimes: vec!["text/html".to_owned()],
        };
        assert_eq!(
            local.serve(&serve, change, 100).unwrap().items,
            vec![(ClipboardFormat::Html, b"<b>t</b>".to_vec())]
        );
        clipboard.copy(text("other"), false);
        assert_eq!(
            local.serve(&serve, change, 100),
            Err(ClipboardSkip::Changed)
        );
    }

    #[test]
    fn work_is_done_in_order_and_reported() {
        let clipboard = MockClipboard::default();
        let (engine, mut events) = mpsc::unbounded_channel();
        let local = LocalClipboard::start(Box::new(clipboard.clone()), engine.downgrade());
        clipboard.copy(text("copied"), false);
        local.ask(LocalOp::Take {
            purpose: Purpose::Push {
                to: DeviceId::new("pc"),
                offers: true,
                files: true,
            },
            limit: 100,
        });
        local.ask(LocalOp::Write {
            content: text("arrived"),
            expect: None,
            purpose: WritePurpose::Arrived {
                origin: DeviceId::new("pc"),
            },
        });
        local.ask(LocalOp::Take {
            purpose: Purpose::Push {
                to: DeviceId::new("pc"),
                offers: true,
                files: true,
            },
            limit: 100,
        });
        let mut done = Vec::new();
        while done.len() < 3 {
            match events.blocking_recv() {
                Some(EngineEvent::Local(d)) => done.push(d),
                Some(_) => {}
                None => break,
            }
        }
        assert!(
            matches!(&done[0], LocalDone::Taken { result: Ok((c, _, _)), .. } if *c == text("copied"))
        );
        assert!(matches!(
            &done[1],
            LocalDone::Written { result: Ok(()), .. }
        ));
        // What arrived is not sent back.
        assert!(matches!(
            &done[2],
            LocalDone::Taken {
                result: Err(ClipboardSkip::Unchanged),
                ..
            }
        ));
        drop(engine);
    }

    struct NoSource;

    impl iw_platform::ClipboardSource for NoSource {
        fn fetch(&self, _format: ClipboardFormat) -> Option<Vec<u8>> {
            None
        }
    }

    /// A source that counts the times it was let go.
    #[derive(Default)]
    struct Counted(std::sync::atomic::AtomicUsize);

    impl iw_platform::ClipboardSource for Counted {
        fn fetch(&self, _format: ClipboardFormat) -> Option<Vec<u8>> {
            None
        }

        fn finished(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A clipboard whose promises with files are handed over, held until
    /// the test says what came of them, as the Windows app takes its time.
    #[derive(Clone, Default)]
    struct Handing {
        inner: MockClipboard,
        handed: Arc<std::sync::Mutex<Vec<(ClipboardPromise, Current, HandedOver)>>>,
    }

    impl Clipboard for Handing {
        fn change_count(&self) -> Result<u64, PlatformError> {
            self.inner.change_count()
        }

        fn read(&self, max_len: usize) -> Result<ClipboardRead, PlatformError> {
            self.inner.read(max_len)
        }

        fn write(&mut self, content: &ClipboardContent) -> Result<u64, PlatformError> {
            self.inner.write(content)
        }

        fn write_promise(&mut self, promise: ClipboardPromise) -> Result<u64, PlatformError> {
            self.inner.write_promise(promise)
        }

        fn promises_files(&self) -> bool {
            true
        }

        fn hand_over(
            &mut self,
            promise: ClipboardPromise,
            current: Current,
            done: HandedOver,
        ) -> Result<(), ClipboardPromise> {
            self.handed.lock().unwrap().push((promise, current, done));
            Ok(())
        }
    }

    fn files_held(offer_id: u64) -> Held {
        Held {
            origin: DeviceId::new("mac"),
            offer_id,
            via: DeviceId::new("mac"),
            formats: vec![ClipboardFormat::Text],
            bytes: 5,
            files: Vec::new(),
            serial: offer_id,
        }
    }

    fn files_promise(source: Arc<Counted>) -> ClipboardPromise {
        ClipboardPromise {
            formats: vec![ClipboardFormat::Text],
            bytes: 5,
            files: vec![iw_platform::PromisedFile {
                name: "report.txt".to_owned(),
                folder: false,
            }],
            source,
        }
    }

    fn next_done(events: &mut mpsc::UnboundedReceiver<EngineEvent>) -> LocalDone {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match events.try_recv() {
                Ok(EngineEvent::Local(done)) => return done,
                Ok(_) => {}
                Err(_) => {
                    assert!(std::time::Instant::now() < deadline, "nothing done");
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
    }

    /// While an offer with files waits for the app, the clipboard's thread
    /// goes on: a read asked after it is done at once. What came of it then
    /// comes in order; not taken, it is promised without its files.
    #[test]
    fn reads_go_on_while_an_offer_with_files_waits_for_the_app() {
        let clipboard = Handing::default();
        let (engine, mut events) = mpsc::unbounded_channel();
        let local = LocalClipboard::start(Box::new(clipboard.clone()), engine.downgrade());
        clipboard.inner.copy(text("before"), false);
        let source = Arc::new(Counted::default());
        local.ask(LocalOp::Promise {
            promise: files_promise(source.clone()),
            held: files_held(1),
            anew: None,
        });
        let push = Purpose::Push {
            to: DeviceId::new("mac"),
            offers: true,
            files: true,
        };
        local.ask(LocalOp::Take {
            purpose: push.clone(),
            limit: 100,
        });
        // The read, though the app has not answered: and the clipboard as
        // it was, as the offer is not a copy here.
        assert!(matches!(
            next_done(&mut events),
            LocalDone::Taken {
                result: Err(ClipboardSkip::Unchanged),
                ..
            }
        ));
        let (promise, current, done) = clipboard.handed.lock().unwrap().pop().unwrap();
        assert!(current());
        done(HandOver::Returned(promise));
        let LocalDone::Promised { result, held, .. } = next_done(&mut events) else {
            panic!("not promised");
        };
        assert_eq!(held.offer_id, 1);
        assert!(result.is_ok());
        // Promised without its files.
        assert_eq!(clipboard.inner.promised_files(), Some(Vec::new()));
        assert_eq!(source.0.load(Ordering::SeqCst), 0);
    }

    /// Something written here, or copied, while the app was waited for is
    /// what the clipboard holds: the offer is not put there, nor promised
    /// otherwise, and is let go; held, it stays the app's.
    #[test]
    fn an_offer_waiting_for_the_app_gives_way_to_what_came_since() {
        let clipboard = Handing::default();
        let (engine, mut events) = mpsc::unbounded_channel();
        let local = LocalClipboard::start(Box::new(clipboard.clone()), engine.downgrade());
        let written = Arc::new(Counted::default());
        local.ask(LocalOp::Promise {
            promise: files_promise(written.clone()),
            held: files_held(1),
            anew: None,
        });
        local.ask(LocalOp::Write {
            content: text("newer"),
            expect: None,
            purpose: WritePurpose::Arrived {
                origin: DeviceId::new("pc"),
            },
        });
        assert!(matches!(
            next_done(&mut events),
            LocalDone::Written { result: Ok(()), .. }
        ));
        let (promise, current, done) = clipboard.handed.lock().unwrap().pop().unwrap();
        // The app may no longer put it there.
        assert!(!current());
        done(HandOver::Returned(promise));
        assert!(matches!(
            next_done(&mut events),
            LocalDone::Promised {
                result: Err(Unpromised::Superseded),
                ..
            }
        ));
        assert_eq!(written.0.load(Ordering::SeqCst), 1);
        assert_eq!(clipboard.inner.content(), text("newer"));

        // Copied here by the user meanwhile: the same.
        let copied = Arc::new(Counted::default());
        local.ask(LocalOp::Promise {
            promise: files_promise(copied.clone()),
            held: files_held(2),
            anew: None,
        });
        let handed = || clipboard.handed.lock().unwrap().pop();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let (promise, _, done) = loop {
            if let Some(handed) = handed() {
                break handed;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        clipboard.inner.copy(text("mine"), false);
        done(HandOver::Returned(promise));
        assert!(matches!(
            next_done(&mut events),
            LocalDone::Promised {
                result: Err(Unpromised::Superseded),
                ..
            }
        ));
        assert_eq!(copied.0.load(Ordering::SeqCst), 1);

        // Held by the app: promised, at the count it was handed over at.
        let held = Arc::new(Counted::default());
        local.ask(LocalOp::Promise {
            promise: files_promise(held.clone()),
            held: files_held(3),
            anew: None,
        });
        let (_, _, done) = loop {
            if let Some(handed) = handed() {
                break handed;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        done(HandOver::Held);
        let LocalDone::Promised { result, .. } = next_done(&mut events) else {
            panic!("not promised");
        };
        assert_eq!(
            result,
            clipboard
                .inner
                .change_count()
                .map_err(|_| Unpromised::Failed)
        );
        assert_eq!(held.0.load(Ordering::SeqCst), 0);
    }
}
