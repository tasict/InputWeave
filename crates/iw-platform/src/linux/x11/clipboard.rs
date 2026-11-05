//! The X11 clipboard: the `CLIPBOARD` selection. Formats and file lists are `clipboard_formats`'.
//!
//! Two connections of its own, so neither waits behind capture or
//! injection:
//! - An owner thread holds a window that owns the selection when content
//!   is written or promised here, and answers other apps' requests for it,
//!   large content in parts (INCR). It also hears, through XFixes, every
//!   time the selection changes hands, which is the change count; written
//!   here, the selection does not count as copied. Promised content is
//!   fetched from its source on a helper thread as an app asks for it, one
//!   fetch for each format (or the files) that every request for it waits
//!   on, so the owner thread goes on answering others meanwhile. The app
//!   waits for the answer as long as it is willing to (X has no limit of
//!   its own: GTK and Qt give up after some seconds, so a large promise
//!   that comes slowly may paste nothing the first time, and the next
//!   paste has it). Its connection lost, as when the display server
//!   starts over at a new login, the thread is started again, from 0.5 s
//!   doubling to 10 s apart.
//! - Reading goes through the other connection, on the caller's thread,
//!   with a limit on how long an app may take to answer, so an app that
//!   hangs holding the selection does not hold up the agent. Content
//!   written or promised here is answered from what was written, and a
//!   promise reads as nothing, as the agent never reads its own again.
//!
//! The selection goes with the window: when the agent ends, what it
//! wrote or promised is no longer on the clipboard.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::io::{Read as _, Write as _};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use x11rb::connection::{Connection, RequestConnection, SequenceNumber};
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEvent, SelectionEventMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask,
    PropMode, Property, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent,
    Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME, NONE};

use super::platform::connect;
use crate::linux::clipboard_formats::{
    self as formats, GNOME_COPIED_FILES, Incoming, Outgoing, PASSWORD_HINT, Received, URI_LIST,
    Wire,
};
use crate::linux::common::{Backoff, lock, os};
use crate::linux::selection::{
    self, ANSWER_WAIT, Answer as Answering, Bytes, Data, Got, MAX_FILE_LIST, Owned, Promised,
    READ_WAIT, STALE, Want, Written,
};
use crate::{
    Clipboard, ClipboardContent, ClipboardFormat, ClipboardPromise, ClipboardRead, PlatformError,
};

/// How long the owner thread may take to say what the change count is.
const SYNC_WAIT: Duration = Duration::from_millis(500);
/// How long a write may wait for the owner thread to take it up, and the
/// thread for the server to say it took the clipboard.
const WRITE_WAIT: Duration = Duration::from_secs(2);
/// How long the owner thread, stopping, waits for the server to have heard
/// that its window is gone, and its refusals.
const STOP_WAIT: Duration = Duration::from_secs(1);
/// Answers larger than this go in parts of this size, as long as the
/// server takes requests that large.
const PART: usize = 256 * 1024;
/// The owner thread looks this often whether a transfer in parts has gone
/// `STALE` while there are any.
const STALE_CHECK: Duration = Duration::from_secs(5);
/// The most a list of targets may take, in bytes.
const MAX_TARGETS: usize = 64 * 1024;
/// How long what the clipboard's owner offers is trusted while it keeps
/// the clipboard: reading the content and then its files asks once.
const OFFER_KEPT: Duration = Duration::from_secs(1);

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        CLIPBOARD,
        TARGETS,
        TIMESTAMP,
        INCR,
        UTF8_STRING,
        TEXT,
        TEXT_PLAIN_UTF8: b"text/plain;charset=utf-8",
        TEXT_PLAIN_UTF8_UPPER: b"text/plain;charset=UTF-8",
        TEXT_PLAIN: b"text/plain",
        TEXT_HTML: b"text/html",
        TEXT_RTF: b"text/rtf",
        APPLICATION_RTF: b"application/rtf",
        IMAGE_PNG: b"image/png",
        URI_LIST_ATOM: b"text/uri-list",
        GNOME_COPIED_FILES_ATOM: b"x-special/gnome-copied-files",
        PASSWORD_HINT_ATOM: b"x-kde-passwordManagerHint",
        // Changed to learn the server's time, or that it heard everything
        // sent before.
        INPUTWEAVE_STAMP,
        // Where owners put their answers for reading.
        INPUTWEAVE_CLIPBOARD,
    }
}

impl Atoms {
    /// The targets InputWeave knows by name.
    fn names(&self) -> Names {
        let pairs: Vec<(Atom, &'static str)> = vec![
            (self.UTF8_STRING, "UTF8_STRING"),
            (self.TEXT, "TEXT"),
            (AtomEnum::STRING.into(), formats::LATIN1),
            (self.TEXT_PLAIN_UTF8, "text/plain;charset=utf-8"),
            (self.TEXT_PLAIN_UTF8_UPPER, "text/plain;charset=UTF-8"),
            (self.TEXT_PLAIN, "text/plain"),
            (self.TEXT_HTML, "text/html"),
            (self.TEXT_RTF, "text/rtf"),
            (self.APPLICATION_RTF, "application/rtf"),
            (self.IMAGE_PNG, "image/png"),
            (self.URI_LIST_ATOM, URI_LIST),
            (self.GNOME_COPIED_FILES_ATOM, GNOME_COPIED_FILES),
            (self.PASSWORD_HINT_ATOM, PASSWORD_HINT),
        ];
        Names {
            by_atom: pairs.iter().copied().collect(),
            by_name: pairs.into_iter().map(|(a, n)| (n, a)).collect(),
        }
    }
}

/// Targets by atom and by name.
struct Names {
    by_atom: HashMap<Atom, &'static str>,
    by_name: HashMap<&'static str, Atom>,
}

impl Names {
    fn name(&self, atom: Atom) -> Option<&'static str> {
        self.by_atom.get(&atom).copied()
    }

    fn atom(&self, name: &str) -> Option<Atom> {
        self.by_name.get(name).copied()
    }
}

/// A window for one of the connections: never shown, hearing the
/// properties put on it.
fn window(conn: &RustConnection, root: Window) -> Result<Window, PlatformError> {
    let window = conn
        .generate_id()
        .map_err(|e| os("cannot create a window", e))?;
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        window,
        root,
        -1,
        -1,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        COPY_FROM_PARENT,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .map_err(|e| os("cannot create a window", e))?;
    Ok(window)
}

fn atoms(conn: &RustConnection) -> Result<Atoms, PlatformError> {
    Atoms::new(conn)
        .map_err(|e| os("cannot name the clipboard's formats", e))?
        .reply()
        .map_err(|e| os("cannot name the clipboard's formats", e))
}

/// Waits until one of `fds` has something to read, or `wait` passed
/// (forever when `None`).
fn poll(fds: &[i32], wait: Option<Duration>) {
    let ms = wait.map_or(-1, |w| {
        i32::try_from(w.as_millis().max(1)).unwrap_or(i32::MAX)
    });
    let mut fds: Vec<libc::pollfd> = fds
        .iter()
        .map(|&fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    // SAFETY: valid pollfds, as many as given.
    unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
}

/// What the owner thread and the clipboard share.
#[derive(Default)]
struct Shared {
    /// Changes every time the selection changes hands, but not when this
    /// clipboard took it.
    count: AtomicU64,
    owned: Mutex<Owned>,
}

/// A write the owner thread carries out, unless its writer gave up
/// waiting first.
#[derive(Default)]
struct Ticket(Mutex<TicketState>);

#[derive(Default, PartialEq, Eq)]
enum TicketState {
    #[default]
    Waiting,
    Taken,
    Abandoned,
}

impl Ticket {
    /// Whether it is still to be carried out, which it now is.
    fn take(&self) -> bool {
        let mut state = lock(&self.0);
        let waiting = *state == TicketState::Waiting;
        if waiting {
            *state = TicketState::Taken;
        }
        waiting
    }

    /// Whether the write is called off, which it now is; not once taken.
    fn abandon(&self) -> bool {
        let mut state = lock(&self.0);
        let waiting = *state == TicketState::Waiting;
        if waiting {
            *state = TicketState::Abandoned;
        }
        waiting
    }
}

/// The owner thread, or when to start it again.
struct OwnerSlot {
    link: Option<OwnerLink>,
    backoff: Backoff,
}

/// The reading connection, or when to connect again.
struct ReaderSlot {
    reader: Option<Reader>,
    backoff: Backoff,
}

/// What reads ask of the clipboard's owner, kept for `OFFER_KEPT` while
/// the same window owns the clipboard and it did not change hands since.
struct Offer {
    owner: Window,
    /// The reading connection's count of the clipboard changing hands.
    changes: u64,
    at: Instant,
    /// The targets offered, or `None` when it gave no list.
    targets: Option<Vec<&'static str>>,
    sensitive: bool,
}

/// The X11 clipboard.
pub struct X11Clipboard {
    shared: Arc<Shared>,
    owner: Mutex<OwnerSlot>,
    reader: Mutex<ReaderSlot>,
}

impl X11Clipboard {
    pub fn connect() -> Result<Self, PlatformError> {
        let shared = Arc::new(Shared::default());
        shared.count.store(1, Ordering::SeqCst);
        let link = OwnerLink::start(shared.clone())?;
        let reader = Reader::connect()?;
        Ok(Self {
            shared,
            owner: Mutex::new(OwnerSlot {
                link: Some(link),
                backoff: Backoff::new(),
            }),
            reader: Mutex::new(ReaderSlot {
                reader: Some(reader),
                backoff: Backoff::new(),
            }),
        })
    }

    /// A handle on the owner thread, started again if its connection
    /// broke (what was on the clipboard from here went with it, which
    /// counts as a change), no sooner than its backoff allows.
    fn owner(&self) -> Result<Handle, PlatformError> {
        let mut slot = lock(&self.owner);
        if let Some(link) = slot.link.as_ref().filter(|l| l.alive()) {
            return Ok(link.handle.clone());
        }
        if let Some(dead) = slot.link.take() {
            drop(dead);
            self.shared.count.fetch_add(1, Ordering::SeqCst);
            let_go(&self.shared);
            slot.backoff.succeeded();
        }
        if !slot.backoff.ready() {
            return Err(PlatformError::Unavailable(
                "the X server went away; the clipboard connects again",
            ));
        }
        match OwnerLink::start(self.shared.clone()) {
            Ok(link) => {
                let handle = link.handle.clone();
                slot.link = Some(link);
                slot.backoff.succeeded();
                tracing::info!("the clipboard is connected to the X server again");
                Ok(handle)
            }
            Err(e) => {
                slot.backoff.failed();
                Err(e)
            }
        }
    }

    /// Runs `f` with the reading connection, within `READ_WAIT` in all,
    /// connecting again first if it broke (no sooner than its backoff
    /// allows, while the server is away), and dropping it if `f` finds it
    /// broke.
    fn reading<T>(
        &self,
        f: impl FnOnce(&Reader, Option<Window>) -> Result<T, ReadError>,
    ) -> Result<T, PlatformError> {
        let ours = self.owner().ok().map(|h| h.window);
        let mut slot = lock(&self.reader);
        if slot.reader.is_none() {
            if !slot.backoff.ready() {
                return Err(PlatformError::Unavailable(
                    "the X server went away; the clipboard connects again",
                ));
            }
            match Reader::connect() {
                Ok(reader) => {
                    slot.reader = Some(reader);
                    slot.backoff.succeeded();
                }
                Err(e) => {
                    slot.backoff.failed();
                    return Err(e);
                }
            }
        }
        let reader = slot.reader.as_ref().expect("connected above");
        reader.until.set(Instant::now() + READ_WAIT);
        match f(reader, ours) {
            Ok(value) => Ok(value),
            Err(ReadError::Connection(e)) => {
                slot.reader = None;
                Err(e)
            }
            Err(ReadError::Other(e)) => Err(e),
        }
    }

    /// Has the owner thread take the clipboard for `owned`. Given up on
    /// before the thread got to it, the write is called off; once taken,
    /// its answer is waited for, as long as the thread takes to hear from
    /// the server at most. Another app keeping the clipboard, or no answer
    /// in time, is `Unavailable`, as a clipboard something else was copied
    /// to meanwhile.
    fn own(&mut self, owned: Owned) -> Result<u64, PlatformError> {
        let handle = self.owner()?;
        let ticket = Arc::new(Ticket::default());
        let (tx, rx) = mpsc::channel();
        handle.command(Command::Own(owned, ticket.clone(), tx));
        let gone = || PlatformError::Unavailable("the clipboard did not take it");
        match rx.recv_timeout(WRITE_WAIT) {
            Ok(result) => result,
            Err(_) if ticket.abandon() => Err(gone()),
            // Taken meanwhile: it is written, and its count comes, once the
            // server answered the thread (or it gave up on the server).
            Err(_) => rx
                .recv_timeout(WRITE_WAIT * 2)
                .unwrap_or_else(|_| Err(gone())),
        }
    }
}

impl X11Clipboard {
    /// Has the reading connection count the clipboard changing hands up to
    /// what it was sent, without waiting, unless a read has it: so that
    /// between reads what the server sends it does not pile up.
    fn hear(&self) {
        if let Ok(slot) = self.reader.try_lock()
            && let Some(reader) = slot.reader.as_ref()
        {
            reader.hear();
        }
    }
}

impl Drop for X11Clipboard {
    fn drop(&mut self) {
        let link = lock(&self.owner).link.take();
        drop(link);
    }
}

/// Lets go of what was owned here, finishing a promise, outside the lock.
fn let_go(shared: &Shared) {
    let owned = std::mem::take(&mut *lock(&shared.owned));
    if let Owned::Promise(promised) = owned {
        promised.finish();
    }
}

impl Clipboard for X11Clipboard {
    /// The changes the owner thread heard of, up to now: it is asked to
    /// catch up with everything the server said before.
    fn change_count(&self) -> Result<u64, PlatformError> {
        if let Ok(handle) = self.owner() {
            let (tx, rx) = mpsc::channel();
            handle.command(Command::Sync(tx));
            if let Ok(count) = rx.recv_timeout(SYNC_WAIT) {
                self.hear();
                return Ok(count);
            }
        }
        Ok(self.shared.count.load(Ordering::SeqCst))
    }

    fn read(&self, max_len: usize) -> Result<ClipboardRead, PlatformError> {
        let owned = lock(&self.shared.owned).clone();
        self.reading(|reader, ours| {
            let owner = reader.owner()?;
            if owner == NONE {
                return Ok(ClipboardRead::Empty);
            }
            if Some(owner) == ours {
                return Ok(match owned {
                    Owned::Content(written) if written.len() > max_len => {
                        ClipboardRead::TooLarge(written.len())
                    }
                    Owned::Content(written) => ClipboardRead::Content(written.content()),
                    // A promise is not read back: the agent never reads
                    // its own again.
                    Owned::Promise(_) | Owned::Nothing => ClipboardRead::Empty,
                });
            }
            reader.stamp()?;
            let (targets, sensitive) = reader.offer(owner)?;
            if sensitive {
                return Ok(ClipboardRead::Sensitive);
            }
            // An owner that gives no list may still have text.
            let offered = targets.unwrap_or_else(|| vec!["UTF8_STRING", formats::LATIN1]);
            reader.read(&offered, max_len)
        })
    }

    fn write(&mut self, content: &ClipboardContent) -> Result<u64, PlatformError> {
        self.own(Owned::Content(Arc::new(Written::new(content))))
    }

    fn write_promise(&mut self, promise: ClipboardPromise) -> Result<u64, PlatformError> {
        self.own(Owned::Promise(Arc::new(Promised::new(promise))))
    }

    /// The files GNOME's or KDE's file manager copied, by their lists of
    /// URIs. None when the content is sensitive or was put there here.
    fn files(&self) -> Result<Vec<PathBuf>, PlatformError> {
        self.reading(|reader, ours| {
            let owner = reader.owner()?;
            if owner == NONE || Some(owner) == ours {
                return Ok(Vec::new());
            }
            reader.stamp()?;
            let (targets, sensitive) = reader.offer(owner)?;
            match targets {
                Some(offered) if !sensitive => reader.files(&offered),
                _ => Ok(Vec::new()),
            }
        })
    }

    fn promises_files(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------
// Reading

/// Why reading failed: the connection broke, or something else did.
enum ReadError {
    Connection(PlatformError),
    Other(PlatformError),
}

impl From<x11rb::errors::ConnectionError> for ReadError {
    fn from(e: x11rb::errors::ConnectionError) -> Self {
        Self::Connection(os("lost the X server", e))
    }
}

impl From<x11rb::errors::ReplyError> for ReadError {
    fn from(e: x11rb::errors::ReplyError) -> Self {
        match e {
            x11rb::errors::ReplyError::ConnectionError(e) => e.into(),
            x11rb::errors::ReplyError::X11Error(e) => {
                Self::Other(os("the X server refused", format!("{e:?}")))
            }
        }
    }
}

/// What asking the selection's owner for a target found.
enum Answer {
    Data(Vec<u8>),
    /// The owner has no such target, or would not give it.
    Refused,
    /// Larger than the limit, to this many bytes at least. `abandoned`
    /// when it was coming in parts, which its owner may still send.
    TooLarge {
        size: usize,
        abandoned: bool,
    },
}

/// The connection reads go through, with a window of its own the owner
/// puts its answers on.
struct Reader {
    conn: RustConnection,
    root: Window,
    /// Where owners put their answers: replaced by a new one once an
    /// answer was given up on, as its owner may still write there, even
    /// the next part of an answer in parts. Writing to a window that is
    /// gone fails, so nothing late lands on the next answer.
    window: Cell<Window>,
    atoms: Atoms,
    names: Names,
    /// The server's time for this read's requests, so an answer to a
    /// request given up on earlier, a refusal too, is not taken for one.
    time: Cell<u32>,
    /// When the read under way must be done by, however many answers it
    /// waits for.
    until: Cell<Instant>,
    /// How often this connection heard the clipboard change hands
    /// (XFixes, selected on the root), up to the last event it read: what
    /// the owner offers is not trusted past a change. After `stamp`, every
    /// change the server told of before is in it.
    changes: Cell<u64>,
    offer: RefCell<Option<Offer>>,
}

impl Reader {
    fn connect() -> Result<Self, PlatformError> {
        let (conn, root) = connect()?;
        let failed = |e: &dyn std::fmt::Display| os("cannot watch the clipboard", e);
        conn.xfixes_query_version(5, 0)
            .map_err(|e| failed(&e))?
            .reply()
            .map_err(|e| failed(&e))?;
        let atoms = atoms(&conn)?;
        let window = window(&conn, root)?;
        // On the root, so a window replaced keeps hearing.
        conn.xfixes_select_selection_input(
            root,
            atoms.CLIPBOARD,
            SelectionEventMask::SET_SELECTION_OWNER
                | SelectionEventMask::SELECTION_WINDOW_DESTROY
                | SelectionEventMask::SELECTION_CLIENT_CLOSE,
        )
        .map_err(|e| failed(&e))?;
        conn.flush().map_err(|e| os("cannot create a window", e))?;
        let names = atoms.names();
        Ok(Self {
            conn,
            root,
            window: Cell::new(window),
            atoms,
            names,
            time: Cell::new(CURRENT_TIME),
            until: Cell::new(Instant::now() + READ_WAIT),
            changes: Cell::new(0),
            offer: RefCell::new(None),
        })
    }

    /// Counts an event of the clipboard changing hands; says whether it
    /// was one.
    fn heard(&self, event: &Event) -> bool {
        let change =
            matches!(event, Event::XfixesSelectionNotify(e) if e.selection == self.atoms.CLIPBOARD);
        if change {
            self.changes.set(self.changes.get() + 1);
        }
        change
    }

    /// Counts what was sent so far, without waiting; other events, none of
    /// them asked for, are dropped.
    fn hear(&self) {
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            self.heard(&event);
        }
    }

    /// Answers go to a new window from now on; the old one goes, and
    /// whatever its owner still writes to it with it. If no window can be
    /// made, the old one stays.
    fn retire(&self) {
        match window(&self.conn, self.root) {
            Ok(window) => {
                let old = self.window.replace(window);
                let _ = self.conn.destroy_window(old);
                let _ = self.conn.flush();
            }
            Err(e) => tracing::debug!(error = %e, "keeping the clipboard's reading window"),
        }
    }

    /// What the owner of the clipboard offers, asked anew unless asked of
    /// the same owner a moment ago and the clipboard did not change hands
    /// since, as `changes` says after `stamp`: a copy from the same window
    /// since, as a password manager copies the password after the user
    /// name, is asked about again. (A copy made after the stamp is no
    /// different from one made while the content is read: the agent finds
    /// the count moved and drops what it read.)
    fn offer(&self, owner: Window) -> Result<(Option<Vec<&'static str>>, bool), ReadError> {
        let changes = self.changes.get();
        if let Some(offer) = self.offer.borrow().as_ref()
            && offer.owner == owner
            && offer.changes == changes
            && offer.at.elapsed() < OFFER_KEPT
        {
            return Ok((offer.targets.clone(), offer.sensitive));
        }
        let targets = self.targets()?;
        let sensitive = match &targets {
            Some(targets) => self.sensitive(targets)?,
            None => false,
        };
        *self.offer.borrow_mut() = Some(Offer {
            owner,
            changes,
            at: Instant::now(),
            targets: targets.clone(),
            sensitive,
        });
        Ok((targets, sensitive))
    }

    /// Learns the server's time now, for the requests that follow: a
    /// property changed by nothing says when. Only this change says it: a
    /// late one from a stamp given up on came before.
    fn stamp(&self) -> Result<(), ReadError> {
        let (window, stamp) = (self.window.get(), self.atoms.INPUTWEAVE_STAMP);
        let asked = self
            .conn
            .change_property8(PropMode::APPEND, window, stamp, AtomEnum::INTEGER, &[])?
            .sequence_number();
        self.conn.flush()?;
        let time = self.wait(|e, sequence| match e {
            Event::PropertyNotify(p)
                if p.window == window && p.atom == stamp && sequence >= asked =>
            {
                Some(p.time)
            }
            _ => None,
        })?;
        self.time.set(time);
        Ok(())
    }

    /// The window that owns the clipboard, or `NONE`.
    fn owner(&self) -> Result<Window, ReadError> {
        Ok(self
            .conn
            .get_selection_owner(self.atoms.CLIPBOARD)?
            .reply()?
            .owner)
    }

    /// The targets the owner offers that InputWeave knows, or `None` when
    /// it gave no list.
    fn targets(&self) -> Result<Option<Vec<&'static str>>, ReadError> {
        let Answer::Data(data) = self.ask(self.atoms.TARGETS, MAX_TARGETS)? else {
            return Ok(None);
        };
        let atoms = data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&c| u32::from_ne_bytes(c));
        Ok(Some(atoms.filter_map(|a| self.names.name(a)).collect()))
    }

    /// Whether the owner marks the content as not to be shared.
    fn sensitive(&self, offered: &[&str]) -> Result<bool, ReadError> {
        if !offered.contains(&PASSWORD_HINT) {
            return Ok(false);
        }
        Ok(match self.ask(self.atoms.PASSWORD_HINT_ATOM, 1024)? {
            Answer::Data(hint) => formats::is_secret(&hint),
            _ => false,
        })
    }

    /// Each format from the best target offered that gives it, the next
    /// when one is refused or holds nothing.
    fn read(&self, offered: &[&'static str], max_len: usize) -> Result<ClipboardRead, ReadError> {
        let mut content = ClipboardContent::default();
        for format in ClipboardFormat::ALL {
            for target in formats::choices(format, offered) {
                let Some(atom) = self.names.atom(target) else {
                    continue;
                };
                let left = max_len.saturating_sub(content.len());
                match self.ask(atom, left)? {
                    Answer::Data(data) => {
                        if let Some(data) = formats::decode(Wire::X11, format, target, &data) {
                            content.items.push((format, data));
                            break;
                        }
                    }
                    Answer::Refused => {}
                    Answer::TooLarge { size, .. } => {
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

    fn files(&self, offered: &[&str]) -> Result<Vec<PathBuf>, ReadError> {
        for (target, atom) in [
            (GNOME_COPIED_FILES, self.atoms.GNOME_COPIED_FILES_ATOM),
            (URI_LIST, self.atoms.URI_LIST_ATOM),
        ] {
            if !offered.contains(&target) {
                continue;
            }
            // A list with no local file in it may stand beside one that
            // has them.
            if let Answer::Data(data) = self.ask(atom, MAX_FILE_LIST)? {
                let files = formats::files_of(target, &data);
                if !files.is_empty() {
                    return Ok(files);
                }
            }
        }
        Ok(Vec::new())
    }

    /// Asks the owner for `target`, up to `limit` bytes; past
    /// `ANSWER_WAIT` without an answer, or between two parts, it is an
    /// error. An answer given up on is left to the owner, which may still
    /// write it, and the next answer goes to another window.
    fn ask(&self, target: Atom, limit: usize) -> Result<Answer, ReadError> {
        let answer = self.ask_once(target, limit);
        if matches!(
            answer,
            Err(_)
                | Ok(Answer::TooLarge {
                    abandoned: true,
                    ..
                })
        ) {
            self.retire();
        }
        answer
    }

    fn ask_once(&self, target: Atom, limit: usize) -> Result<Answer, ReadError> {
        let (conn, window) = (&self.conn, self.window.get());
        let property = self.atoms.INPUTWEAVE_CLIPBOARD;
        let (selection, time) = (self.atoms.CLIPBOARD, self.time.get());
        conn.delete_property(window, property)?;
        conn.convert_selection(window, selection, target, property, time)?;
        conn.flush()?;
        // Only the answer to this request: one given up on earlier may
        // still come, to the window it asked from then, with the time it
        // asked at. Owners that answer without the time are taken at
        // their word.
        let notify = self.wait(|e, _| match e {
            Event::SelectionNotify(n)
                if n.requestor == window
                    && n.selection == selection
                    && n.target == target
                    && (n.time == time || n.time == CURRENT_TIME)
                    && (n.property == property || n.property == NONE) =>
            {
                Some(n.property)
            }
            _ => None,
        })?;
        if notify == NONE {
            return Ok(Answer::Refused);
        }
        let header = conn
            .get_property(false, window, property, AtomEnum::ANY, 0, 1)?
            .reply()?;
        if header.type_ == self.atoms.INCR {
            let size = header.value32().and_then(|mut v| v.next()).unwrap_or(0) as usize;
            if size > limit {
                // Not deleted: that would ask for the first part.
                return Ok(Answer::TooLarge {
                    size,
                    abandoned: true,
                });
            }
            // Deleting the property asks for the first part.
            conn.delete_property(window, property)?;
            conn.flush()?;
            let mut incoming = Incoming::new(limit);
            loop {
                self.wait(|e, _| match e {
                    Event::PropertyNotify(p)
                        if p.window == window
                            && p.atom == property
                            && p.state == Property::NEW_VALUE =>
                    {
                        Some(())
                    }
                    _ => None,
                })?;
                let part = self.take(property)?;
                match incoming.part(&part) {
                    Received::More => {}
                    Received::Done(data) => return Ok(Answer::Data(data)),
                    Received::TooLarge(size) => {
                        return Ok(Answer::TooLarge {
                            size,
                            abandoned: true,
                        });
                    }
                }
            }
        }
        let size = header.value_len as usize * usize::from(header.format / 8).max(1)
            + header.bytes_after as usize;
        if size > limit {
            conn.delete_property(window, property)?;
            conn.flush()?;
            return Ok(Answer::TooLarge {
                size,
                abandoned: false,
            });
        }
        Ok(Answer::Data(self.take(property)?))
    }

    /// The whole of `property` on the window, deleted as it is read.
    fn take(&self, property: Atom) -> Result<Vec<u8>, ReadError> {
        let reply = self
            .conn
            .get_property(
                true,
                self.window.get(),
                property,
                AtomEnum::ANY,
                0,
                u32::MAX / 4,
            )?
            .reply()?;
        Ok(reply.value)
    }

    /// The first event `pick` takes (given each with the request the
    /// server took last as it sent it), within `ANSWER_WAIT` and what is
    /// left of the read's; others are dropped, as nothing else is asked on
    /// this connection.
    fn wait<T>(
        &self,
        mut pick: impl FnMut(&Event, SequenceNumber) -> Option<T>,
    ) -> Result<T, ReadError> {
        let deadline = (Instant::now() + ANSWER_WAIT).min(self.until.get());
        loop {
            while let Some((event, sequence)) = self.conn.poll_for_event_with_sequence()? {
                if self.heard(&event) {
                    continue;
                }
                if let Some(found) = pick(&event, sequence) {
                    return Ok(found);
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(ReadError::Other(PlatformError::Os(
                    "the app that holds the clipboard did not answer in time".to_owned(),
                )));
            }
            poll(&[self.conn.stream().as_raw_fd()], Some(left));
        }
    }
}

// ---------------------------------------------------------------------
// Owning

enum Command {
    /// Say the change count, once everything the server said before was
    /// heard.
    Sync(Sender<u64>),
    /// Take the clipboard for this, unless its writer gave up, and say the
    /// change count after.
    Own(Owned, Arc<Ticket>, Sender<Result<u64, PlatformError>>),
    /// What a helper got from a promise's source.
    Got(Arc<Promised>, Want, Got),
    Stop,
}

/// A request for the clipboard from another app.
#[derive(Clone, Copy, Debug)]
struct Request {
    requestor: Window,
    selection: Atom,
    target: Atom,
    property: Atom,
    time: u32,
}

impl Request {
    fn of(e: &SelectionRequestEvent) -> Self {
        Self {
            requestor: e.requestor,
            selection: e.selection,
            target: e.target,
            // Clients older than ICCCM 2 name no property.
            property: if e.property == NONE {
                e.target
            } else {
                e.property
            },
            time: e.time,
        }
    }
}

/// What a request is answered with: a property of this type.
struct Reply {
    kind: Atom,
    data: ReplyData,
}

/// A reply's data, as the property's format has it.
enum ReplyData {
    /// Format 8.
    Bytes(Data),
    /// Format 32: atoms, a time.
    Words(Vec<u32>),
}

impl Reply {
    /// `bytes`, shared as they are where they can be.
    fn of(kind: Atom, bytes: Bytes) -> Self {
        Self {
            kind,
            data: ReplyData::Bytes(bytes.into_data()),
        }
    }

    fn words(kind: impl Into<Atom>, words: Vec<u32>) -> Self {
        Self {
            kind: kind.into(),
            data: ReplyData::Words(words),
        }
    }
}

/// A request waiting for what a promise's source is asked for.
struct Waiting {
    request: Request,
    target: &'static str,
    kind: Atom,
}

/// What the clipboard and the helpers reach the owner thread through.
/// They wake it through a pipe of its own, never writing on its X
/// connection: a write there may read the server's events into the
/// connection's queue, where the thread, waiting on the connection's
/// socket, would not see them.
#[derive(Clone)]
struct Handle {
    window: Window,
    commands: Sender<Command>,
    wake: Arc<UnixStream>,
}

impl Handle {
    fn command(&self, command: Command) {
        // A command the thread never sees is dropped, its promise
        // finished by being let go of.
        if self.commands.send(command).is_ok() {
            // Full, the pipe wakes the thread anyway.
            let _ = (&*self.wake).write(&[0]);
        }
    }
}

/// The owner thread, as the clipboard holds it.
struct OwnerLink {
    handle: Handle,
    alive: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl OwnerLink {
    fn start(shared: Arc<Shared>) -> Result<Self, PlatformError> {
        let (conn, root) = connect()?;
        let failed = |e: &dyn std::fmt::Display| os("cannot watch the clipboard", e);
        conn.xfixes_query_version(5, 0)
            .map_err(|e| failed(&e))?
            .reply()
            .map_err(|e| failed(&e))?;
        let atoms = atoms(&conn)?;
        let fence = self::window(&conn, root)?;
        let window = self::window(&conn, root)?;
        conn.xfixes_select_selection_input(
            window,
            atoms.CLIPBOARD,
            SelectionEventMask::SET_SELECTION_OWNER
                | SelectionEventMask::SELECTION_WINDOW_DESTROY
                | SelectionEventMask::SELECTION_CLIENT_CLOSE,
        )
        .map_err(|e| failed(&e))?;
        conn.flush().map_err(|e| failed(&e))?;
        let part = conn
            .maximum_request_bytes()
            .saturating_sub(1024)
            .clamp(4096, PART);
        let conn = Arc::new(conn);
        let (tx, rx) = mpsc::channel();
        let (wake, woken) = UnixStream::pair()
            .and_then(|(a, b)| {
                a.set_nonblocking(true)?;
                b.set_nonblocking(true)?;
                Ok((a, b))
            })
            .map_err(|e| os("cannot start the clipboard thread", e))?;
        let alive = Arc::new(AtomicBool::new(true));
        let handle = Handle {
            window,
            commands: tx,
            wake: Arc::new(wake),
        };
        let names = atoms.names();
        let owner = Owner {
            conn,
            window,
            fence,
            atoms,
            names,
            shared,
            commands: rx,
            woken,
            handle: handle.clone(),
            outgoing: Vec::new(),
            waiting: HashMap::new(),
            pending: VecDeque::new(),
            installing: None,
            replaced: None,
            acquired: CURRENT_TIME,
            part,
        };
        let thread = {
            let alive = Alive(alive.clone());
            std::thread::Builder::new()
                .name("inputweave-x11-clipboard".to_owned())
                .spawn(move || {
                    // Ended however it ends, a panic too, it is started
                    // again.
                    let _alive = alive;
                    owner.run();
                })
                .map_err(|e| os("cannot start the clipboard thread", e))?
        };
        Ok(Self {
            handle,
            alive,
            thread: Some(thread),
        })
    }

    fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

impl Drop for OwnerLink {
    fn drop(&mut self) {
        self.handle.command(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Says the owner thread ended once it is dropped, whether the thread
/// returned or panicked.
struct Alive(Arc<AtomicBool>);

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Run on the owner thread after it took the clipboard and heard the
/// server say so, before it says the count: for a test to have another
/// app copy at that moment.
#[cfg(test)]
pub(super) static AFTER_TAKING: Mutex<Option<Box<dyn FnMut() + Send>>> = Mutex::new(None);

/// What the owner thread does with events read while waiting for the
/// server to have heard everything sent before.
#[derive(Clone, Copy)]
enum Hear {
    /// Handles them.
    All,
    /// Handles those the server sent before it took request `before`;
    /// later ones wait their turn.
    Before(SequenceNumber),
    /// Refuses requests for the clipboard; nothing else matters any more.
    Refusing,
}

/// An answer going out in parts.
struct Out {
    requestor: Window,
    property: Atom,
    kind: Atom,
    transfer: Outgoing,
    last: Instant,
}

/// The owner thread's state.
struct Owner {
    conn: Arc<RustConnection>,
    window: Window,
    /// A window whose property the thread changes to learn that the server
    /// heard everything sent before: apart from the one owning the
    /// clipboard, which may be gone by then.
    fence: Window,
    atoms: Atoms,
    names: Names,
    shared: Arc<Shared>,
    commands: Receiver<Command>,
    /// What wakes the thread when a command comes; see `Handle`.
    woken: UnixStream,
    /// For helpers to reach the thread.
    handle: Handle,
    outgoing: Vec<Out>,
    /// Requests waiting for a helper, by promise and what it was asked.
    waiting: HashMap<(usize, Want), Vec<Waiting>>,
    /// Events read but left for later, in order, with the request the
    /// server took last as it sent each: what it sent after it said it
    /// gave this window the clipboard comes after the write.
    pending: VecDeque<(Event, SequenceNumber)>,
    /// What is being written, to answer requests with from the moment the
    /// server took request `.0`, which gives this window the clipboard;
    /// requests sent before are about what was there before.
    installing: Option<(SequenceNumber, Owned)>,
    /// What the content written replaced, to let go of once written.
    replaced: Option<Owned>,
    /// When this window last took the clipboard, as the server said.
    acquired: u32,
    /// The largest part an answer goes out in.
    part: usize,
}

impl Owner {
    /// Handles the server's events and the clipboard's commands, in the
    /// order they came, until stopped or the connection breaks.
    fn run(mut self) {
        let fds = [self.conn.stream().as_raw_fd(), self.woken.as_raw_fd()];
        loop {
            // Everything already here comes before what was asked.
            let heard = match self.drain() {
                Ok(heard) => heard,
                Err(e) => {
                    tracing::warn!(error = %e, "lost the X server; the clipboard connects again");
                    break;
                }
            };
            let mut asked = false;
            while let Ok(command) = self.commands.try_recv() {
                asked = true;
                if !self.command(command) {
                    self.stop();
                    return;
                }
            }
            self.expire();
            let _ = self.conn.flush();
            if heard || asked {
                continue;
            }
            // Writing may have read events in, which the socket no longer
            // shows: only once nothing more is there does the thread wait,
            // for the server or a command. Transfers in parts are looked at
            // now and then, in case their requestor stopped taking them.
            match self.conn.poll_for_event() {
                Ok(Some(event)) => {
                    self.event(event);
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "lost the X server; the clipboard connects again");
                    break;
                }
            }
            poll(&fds, (!self.outgoing.is_empty()).then_some(STALE_CHECK));
            while (&self.woken).read(&mut [0; 64]).is_ok_and(|n| n > 0) {}
        }
        self.end();
    }

    /// Handles every event left for later, read already, or there to
    /// read, without waiting for more; says whether there was any.
    fn drain(&mut self) -> Result<bool, x11rb::errors::ConnectionError> {
        let mut heard = false;
        while let Some((event, _)) = self.pending.pop_front() {
            heard = true;
            self.event(event);
        }
        while let Some(event) = self.conn.poll_for_event()? {
            heard = true;
            self.event(event);
        }
        Ok(heard)
    }

    /// Hears everything the server said up to now, within `SYNC_WAIT`.
    fn catch_up(&mut self) {
        self.fence(SYNC_WAIT, Hear::All);
    }

    /// Has the server answer a request of this thread's own and reads
    /// until it did, as `hear` says, within `wait`: everything it sent
    /// before is read by then. False when it did not answer in time, or
    /// the connection broke.
    fn fence(&mut self, wait: Duration, hear: Hear) -> bool {
        self.fence_until(Instant::now() + wait, hear)
    }

    /// `fence`, until `deadline`.
    fn fence_until(&mut self, deadline: Instant, hear: Hear) -> bool {
        let (window, stamp) = (self.fence, self.atoms.INPUTWEAVE_STAMP);
        // Only this change ends the wait: one from a fence given up on
        // earlier came before.
        let Ok(asked) = self
            .conn
            .change_property8(PropMode::APPEND, window, stamp, AtomEnum::INTEGER, &[])
            .map(|c| c.sequence_number())
        else {
            return false;
        };
        // What was left for later comes before what is read now, as far as
        // `hear` takes it.
        while let Some((_, sequence)) = self.pending.front() {
            if matches!(hear, Hear::Before(before) if *sequence >= before) {
                break;
            }
            let (event, _) = self.pending.pop_front().expect("looked at above");
            self.heard(event, hear);
        }
        let fd = self.conn.stream().as_raw_fd();
        let mut flushed = false;
        loop {
            match self.conn.poll_for_event_with_sequence() {
                Ok(Some((event, sequence))) => {
                    flushed = false;
                    if let Some((set, _)) = &self.installing
                        && sequence >= *set
                    {
                        self.install();
                    }
                    if let Event::PropertyNotify(p) = &event
                        && p.window == window
                        && p.atom == stamp
                        && sequence >= asked
                    {
                        return true;
                    }
                    match hear {
                        Hear::Before(before) if sequence >= before || !self.pending.is_empty() => {
                            self.pending.push_back((event, sequence));
                        }
                        _ => self.heard(event, hear),
                    }
                    continue;
                }
                // What handling them wrote goes out; writing may read more
                // in, which the next look takes.
                Ok(None) if !flushed => {
                    if self.conn.flush().is_err() {
                        return false;
                    }
                    flushed = true;
                    continue;
                }
                Ok(None) => {}
                Err(_) => return false,
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            poll(&[fd], Some(left));
            flushed = false;
        }
    }

    /// Puts what is being written in place, keeping what it replaced to
    /// let go of.
    fn install(&mut self) {
        if let Some((_, owned)) = self.installing.take() {
            let old = std::mem::replace(&mut *lock(&self.shared.owned), owned);
            self.replaced = Some(old);
        }
    }

    /// An event read while waiting on the server, handled as `hear` says.
    fn heard(&mut self, event: Event, hear: Hear) {
        match (hear, event) {
            (Hear::Refusing, Event::SelectionRequest(e)) => self.answer(Request::of(&e), None),
            (Hear::Refusing, _) => {}
            (_, event) => self.event(event),
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::XfixesSelectionNotify(e) if e.selection == self.atoms.CLIPBOARD => {
                if e.subtype == SelectionEvent::SET_SELECTION_OWNER && e.owner == self.window {
                    self.acquired = e.selection_timestamp;
                } else {
                    self.shared.count.fetch_add(1, Ordering::SeqCst);
                }
            }
            Event::SelectionClear(e)
                if e.owner == self.window && e.selection == self.atoms.CLIPBOARD =>
            {
                // Only if another app has it now: this window may have
                // taken it again since.
                let owner = self
                    .conn
                    .get_selection_owner(self.atoms.CLIPBOARD)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .map(|r| r.owner);
                if owner != Some(self.window) {
                    let_go(&self.shared);
                }
            }
            Event::SelectionRequest(e) => self.request(e),
            Event::PropertyNotify(e) if e.state == Property::DELETE => {
                self.next_part(e.window, e.atom);
            }
            _ => {}
        }
    }

    /// Carries out `command`; false to stop.
    fn command(&mut self, command: Command) -> bool {
        match command {
            Command::Sync(reply) => {
                self.catch_up();
                let _ = reply.send(self.shared.count.load(Ordering::SeqCst));
            }
            Command::Own(owned, ticket, reply) => {
                // Given up on, it is dropped, a promise finished with it.
                if ticket.take() {
                    let _ = reply.send(self.own(owned));
                }
            }
            Command::Got(promised, want, got) => self.got(promised, want, got),
            Command::Stop => return false,
        }
        true
    }

    /// Takes the clipboard for `owned`, which answers requests from the
    /// moment the server gives this window the clipboard; requests it sent
    /// before are answered with what was there before. What the server
    /// sent before it said this window has the clipboard is counted before
    /// this write, so a copy made just before it does not seem made after;
    /// what it sent later, another app's copy right after among it, is
    /// heard after the write, so the write's count never covers it. The
    /// server has `WRITE_WAIT` to say; if it does not, the content stays,
    /// as the server may still give this window the clipboard.
    fn own(&mut self, owned: Owned) -> Result<u64, PlatformError> {
        let conn = self.conn.clone();
        let asked = conn
            .set_selection_owner(self.window, self.atoms.CLIPBOARD, CURRENT_TIME)
            .ok()
            .map(|set| set.sequence_number())
            .and_then(|set| {
                let get = conn.get_selection_owner(self.atoms.CLIPBOARD).ok()?;
                Some((set, get))
            });
        let Some((set, cookie)) = asked else {
            return Err(os(
                "cannot take the clipboard",
                "the request did not go out",
            ));
        };
        self.installing = Some((set, owned));
        let before = cookie.sequence_number();
        let heard = self.fence(WRITE_WAIT, Hear::Before(before));
        self.install();
        // Its reply came before the fence: there now, it does not wait.
        let taken = if heard {
            cookie.reply().ok().map(|r| r.owner)
        } else {
            None
        };
        #[cfg(test)]
        if let Some(hook) = lock(&AFTER_TAKING).as_mut() {
            hook();
        }
        if let Some(Owned::Promise(promised)) = self.replaced.take() {
            promised.finish();
        }
        match taken {
            Some(owner) if owner == self.window => {
                Ok(self.shared.count.fetch_add(1, Ordering::SeqCst) + 1)
            }
            Some(_) => {
                // Neither the new nor the old is on the clipboard.
                let_go(&self.shared);
                Err(PlatformError::Unavailable("another app took the clipboard"))
            }
            None => Err(PlatformError::Unavailable(
                "the X server did not answer in time",
            )),
        }
    }

    /// Lets go of the clipboard, so no app waits on a window that no
    /// longer answers: the window goes, and the clipboard with it if it
    /// still has it (never another app's); every request that came before
    /// is refused, and every request still waiting for a helper. The server
    /// is had to answer once more after those refusals: seeing the
    /// connection close, it may drop what it had not read yet (Xvfb does,
    /// when busy), and the app would wait for ever. Both answers within
    /// `STOP_WAIT` together: a server that takes longer is beyond it.
    fn stop(&mut self) {
        let deadline = Instant::now() + STOP_WAIT;
        let _ = self.conn.destroy_window(self.window);
        self.fence_until(deadline, Hear::Refusing);
        self.end();
        self.fence_until(deadline, Hear::Refusing);
    }

    /// Ends what is left: requests waiting, transfers in parts, writes
    /// still queued (their promises finished), and what was owned.
    fn end(&mut self) {
        let waiting: Vec<Waiting> = self.waiting.drain().flat_map(|(_, w)| w).collect();
        for w in waiting {
            self.answer(w.request, None);
        }
        while !self.outgoing.is_empty() {
            self.done(0);
        }
        let _ = self.conn.flush();
        while let Ok(command) = self.commands.try_recv() {
            match command {
                Command::Sync(reply) => {
                    let _ = reply.send(self.shared.count.load(Ordering::SeqCst));
                }
                Command::Own(owned, _, reply) => {
                    drop(owned);
                    let _ = reply.send(Err(PlatformError::Unavailable(
                        "the clipboard lost the X server",
                    )));
                }
                Command::Got(..) | Command::Stop => {}
            }
        }
        let_go(&self.shared);
    }

    fn request(&mut self, e: SelectionRequestEvent) {
        let request = Request::of(&e);
        let owned = if e.owner == self.window && e.selection == self.atoms.CLIPBOARD {
            lock(&self.shared.owned).clone()
        } else {
            Owned::Nothing
        };
        let target = self.names.name(e.target);
        let kind = self.kind(e.target);
        let reply = match owned {
            Owned::Nothing => None,
            _ if e.target == self.atoms.TARGETS => {
                Some(Reply::words(AtomEnum::ATOM, self.targets(&owned)))
            }
            _ if e.target == self.atoms.TIMESTAMP => {
                Some(Reply::words(AtomEnum::INTEGER, vec![self.acquired]))
            }
            _ => match target.map(|target| (target, selection::answer(&owned, target))) {
                Some((_, Answering::Now(bytes))) => Some(Reply::of(kind, bytes)),
                Some((target, Answering::Fetch(promised, want))) => {
                    return self.promised(request, target, kind, promised, want);
                }
                Some((_, Answering::Nothing)) | None => None,
            },
        };
        self.answer(request, reply);
    }

    /// Answers a request for something promised: what was kept at once,
    /// else once a helper got `want` of it, one helper for every request
    /// waiting for the same (so `Promised::get`'s own sharing of a fetch
    /// is never needed here).
    fn promised(
        &mut self,
        request: Request,
        target: &'static str,
        kind: Atom,
        promised: Arc<Promised>,
        want: Want,
    ) {
        // What was kept is answered at once.
        if let Some(got) = promised.peek(want) {
            let reply = selection::bytes_of(target, &got).map(|bytes| Reply::of(kind, bytes));
            return self.answer(request, reply);
        }
        let waiting = Waiting {
            request,
            target,
            kind,
        };
        let key = (promised.key(), want);
        if let Some(others) = self.waiting.get_mut(&key) {
            // Fetching already: it waits for the same.
            others.push(waiting);
            return;
        }
        self.waiting.insert(key, vec![waiting]);
        self.fetch(promised, want);
    }

    /// Has a helper thread get `want` of the promise, which may take as
    /// long as the content takes to come (`Promised::get`: at once what
    /// was kept).
    fn fetch(&mut self, promised: Arc<Promised>, want: Want) {
        let handle = self.handle.clone();
        let helper = promised.clone();
        let started = std::thread::Builder::new()
            .name("inputweave-x11-clipboard-fetch".to_owned())
            .spawn(move || {
                let got = helper.get(want);
                // Gone meanwhile: nobody waits.
                handle.command(Command::Got(helper, want, got));
            });
        if started.is_err() {
            // Refused this time, asked again next time.
            self.got(promised, want, Got::Busy);
        }
    }

    /// A helper got `got` for `want`: kept if worth keeping, and given to
    /// every request waiting for it.
    fn got(&mut self, promised: Arc<Promised>, want: Want, got: Got) {
        let waiting = self
            .waiting
            .remove(&(promised.key(), want))
            .unwrap_or_default();
        for w in waiting {
            let reply = selection::bytes_of(w.target, &got).map(|bytes| Reply::of(w.kind, bytes));
            self.answer(w.request, reply);
        }
    }

    /// The targets what is owned is offered as.
    fn targets(&self, owned: &Owned) -> Vec<Atom> {
        let mut targets = vec![self.atoms.TARGETS, self.atoms.TIMESTAMP];
        targets.extend(
            owned
                .mimes(Wire::X11)
                .into_iter()
                .filter_map(|name| self.names.atom(name)),
        );
        targets
    }

    /// The type an answer for `target` has: text asked for as `TEXT` is
    /// UTF-8, `STRING` is itself, and MIME types are themselves.
    fn kind(&self, target: Atom) -> Atom {
        if target == self.atoms.TEXT {
            self.atoms.UTF8_STRING
        } else {
            target
        }
    }

    /// Puts `reply` where `request` asked, or refuses it, and says so; a
    /// large reply goes in parts as the requestor takes them.
    fn answer(&mut self, request: Request, reply: Option<Reply>) {
        let property = match reply.map(|r| (r.kind, r.data)) {
            None => NONE,
            Some((kind, ReplyData::Bytes(data))) if data.len() > self.part => {
                let size = u32::try_from(data.len()).unwrap_or(u32::MAX);
                let _ = self.conn.change_window_attributes(
                    request.requestor,
                    &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
                );
                let _ = self.conn.change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    request.property,
                    self.atoms.INCR,
                    &[size],
                );
                self.outgoing
                    .retain(|o| o.requestor != request.requestor || o.property != request.property);
                self.outgoing.push(Out {
                    requestor: request.requestor,
                    property: request.property,
                    kind,
                    transfer: Outgoing::new(data, self.part),
                    last: Instant::now(),
                });
                request.property
            }
            Some((kind, data)) => {
                let (requestor, property) = (request.requestor, request.property);
                let ok = match data {
                    ReplyData::Words(words) => self.conn.change_property32(
                        PropMode::REPLACE,
                        requestor,
                        property,
                        kind,
                        &words,
                    ),
                    ReplyData::Bytes(bytes) => self.conn.change_property8(
                        PropMode::REPLACE,
                        requestor,
                        property,
                        kind,
                        &bytes,
                    ),
                };
                if ok.is_ok() { property } else { NONE }
            }
        };
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property,
        };
        let _ = self
            .conn
            .send_event(false, request.requestor, EventMask::NO_EVENT, notify);
    }

    /// The requestor took the last part of an answer going out in parts:
    /// the next goes, an empty one last.
    fn next_part(&mut self, window: Window, property: Atom) {
        let Some(at) = self
            .outgoing
            .iter()
            .position(|o| o.requestor == window && o.property == property)
        else {
            return;
        };
        let out = &mut self.outgoing[at];
        out.last = Instant::now();
        match out.transfer.next_part() {
            Some(part) => {
                let ended = part.is_empty();
                let _ = self.conn.change_property8(
                    PropMode::REPLACE,
                    out.requestor,
                    out.property,
                    out.kind,
                    part,
                );
                if ended {
                    self.done(at);
                }
            }
            None => self.done(at),
        }
    }

    /// An answer in parts went, or will never go.
    fn done(&mut self, at: usize) {
        let out = self.outgoing.remove(at);
        if !self.outgoing.iter().any(|o| o.requestor == out.requestor) {
            let _ = self.conn.change_window_attributes(
                out.requestor,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
            );
        }
    }

    /// Drops answers in parts whose requestor stopped taking them.
    fn expire(&mut self) {
        while let Some(at) = self.outgoing.iter().position(|o| o.last.elapsed() > STALE) {
            tracing::debug!(
                bytes = self.outgoing[at].transfer.len(),
                "an app stopped taking the clipboard in parts"
            );
            self.done(at);
        }
    }
}
