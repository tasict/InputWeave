//! The Wayland clipboard through the desktop's Clipboard portal,
//! for GNOME, whose compositor
//! offers no data-control (`data_control`; `clipboard` chooses). The portal
//! shares the clipboard with a RemoteDesktop session only: `remote` asks
//! for it (`RequestClipboard`) with every session it creates, before
//! starting it, and the clipboard works while that session runs, on its
//! task, if the desktop allowed it (`clipboard_enabled`). Without remote
//! control there is no clipboard: on GNOME, clipboard sync needs the user
//! to allow remote control. Formats and file lists are
//! `clipboard_formats`', answering and reading `selection`'s. Only
//! `CLIPBOARD`.
//!
//! Asking for the clipboard adds the switch "Allow Clipboard Access" (on)
//! to GNOME's dialog, and the choice goes into the restore token
//! (xdg-desktop-portal-gnome); KDE's dialog asks nothing more, and a token
//! restores only a session that asks for the clipboard as the one that
//! made it did (xdg-desktop-portal-kde). Every session asks, whichever
//! clipboard is used, so the sessions are alike, and the portal's is there
//! should data-control turn out to be missing.
//!
//! - The desktop says each time the clipboard changes hands, with what it
//!   offers and whether this session is its owner
//!   (`SelectionOwnerChanged`): a change, unless this session owns it.
//!   Followed from before the session starts, so what it says as it
//!   starts is heard. Until it first says who owns the clipboard, what is
//!   there is not known: reading is `Unavailable`, and its first word is a
//!   change. Only the portal's own
//!   signals are heard, by the name that owns it on the bus: any app on
//!   the session bus may send signals, to this connection as well.
//! - Writes go to the desktop one at a time, in order (`SetSelection`): a
//!   write is taken when the desktop answers it, which the count going up
//!   by one says. The desktop says things in order, which the serials of
//!   its messages keep, though its answer and its signals reach the task
//!   by different ways: what it said while a write waited for the answer
//!   is held back until the answer, and what came before the answer
//!   counts before the write (another app's copy, an app pasting what was
//!   there, a word on an earlier write), what came after counts after it.
//!   Until the desktop took it, what was there stays there, and an app
//!   pasting it gets it; refused, nothing changed. A write that stopped
//!   waiting is not counted when the desktop takes it later, and one still
//!   waiting its turn then is not carried out. One the desktop did not
//!   answer in time it may still take: until it says the session owns the
//!   clipboard (with no other write waiting), or another app copies, what
//!   is there is not known, and an app pasting gets nothing rather than
//!   what may not be there. A write that stopped waiting and is taken
//!   after all, by the desktop's late answer or its word that the session
//!   owns the clipboard, is a change (the agent took it for not written,
//!   and reads what is there); a promise given up on is ended as the write
//!   stops waiting, so it is withdrawn rather than offered (`SetSelection`
//!   with nothing) and nothing of it is given.
//! - An app pasting has the desktop ask for a MIME type
//!   (`SelectionTransfer`): the answer, from what was on the clipboard
//!   when it asked, goes into the pipe the desktop hands out
//!   (`SelectionWrite`), a promise's content fetched first, once for each
//!   format, on a thread of its own, and the desktop is told how it went
//!   (`SelectionWriteDone`).
//! - Reading asks the desktop for a pipe for each MIME type
//!   (`SelectionRead`), with `selection`'s time limits; its own content is
//!   answered from what was written, a promise reads as nothing. Whether
//!   the clipboard is marked as not to be shared is asked once each time
//!   it changes hands.
//! - The session ending, what was put on the clipboard from here is let
//!   go of (a promise finished) and the clipboard is unavailable until the
//!   next one runs.
//!
//! ashpd makes the calls but two: `SetSelection`, whose answer's serial
//! orders it among the signals, and the signals, followed by their serials
//! and from the portal's unique name. The session's path, which they name,
//! is what ashpd sends for the session in its calls (its `Serialize`):
//! ashpd has no other way to tell it.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ashpd::desktop::Session;
use ashpd::desktop::clipboard::{Clipboard as Desktop, RequestClipboardOptions};
use ashpd::desktop::remote_desktop::RemoteDesktop;
use futures_util::{FutureExt, StreamExt};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};
use zbus::{OwnedObjectPath, OwnedVariant, Variant};

use crate::linux::clipboard_formats::Wire;
use crate::linux::common::{lock, wait_timeout};
use crate::linux::selection::{self, ANSWER_WAIT, Asked, Owned, Promised, READ_WAIT, Written};
use crate::linux::wayland::clipboard::LetsGo;
use crate::{Clipboard, ClipboardContent, ClipboardPromise, ClipboardRead, PlatformError};

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.Clipboard";

/// How long the desktop may take to answer a write.
const SET_WAIT: Duration = Duration::from_secs(2);
/// How long a write waits for the desktop to take it.
const WRITE_WAIT: Duration = Duration::from_millis(2500);

/// How long, from the start of a session's clipboard, what is there is
/// not known without the desktop saying who owns it. GNOME says it as
/// the clipboard is enabled, if anyone owns it: mutter's `EnableClipboard`
/// tells the current owner (`meta_clipboard_session_enable`, before that
/// `handle_enable_clipboard`), which xdg-desktop-portal-gnome passes on
/// once its `Start` answer let the frontend know the session has the
/// clipboard. Nothing is said of a clipboard nobody owns, and
/// xdg-desktop-portal-kde tells only later changes: after this, nothing
/// said is nothing there, until the next copy (one made before the session
/// is not shared then).
const FIRST_OWNER_WAIT: Duration = Duration::from_secs(1);

fn not_known() -> PlatformError {
    PlatformError::Unavailable("the desktop has not said what is on the clipboard")
}

fn unavailable() -> PlatformError {
    PlatformError::Unavailable("the desktop's clipboard: only while remote control runs")
}

/// What the clipboard asks of the session's task.
enum Command {
    Own(Write),
    /// A pipe to read `mime` from, or `None` when the desktop has none.
    Read {
        mime: String,
        reply: std::sync::mpsc::Sender<Option<std::os::fd::OwnedFd>>,
    },
}

/// Putting `owned` on the clipboard, offering `mimes`.
struct Write {
    owned: Owned,
    mimes: Vec<String>,
    reply: Reply,
    /// Taking back what was put there, offering nothing: no write the agent
    /// knows of, so no change, and nothing is this session's.
    withdrawal: bool,
}

/// A write never carried out failed.
impl Drop for Write {
    fn drop(&mut self) {
        self.reply.failed(unavailable());
    }
}

/// What came of a write.
#[derive(Default)]
struct Answer {
    result: Option<Result<u64, PlatformError>>,
    /// Said once, whether or not the write still waited.
    said: bool,
    /// The write stopped waiting.
    gave_up: bool,
}

/// Where the session's task says what came of a write, which may have
/// stopped waiting for it.
#[derive(Clone, Default)]
struct Reply(Arc<(Mutex<Answer>, Condvar)>);

impl Reply {
    fn answer(&self) -> MutexGuard<'_, Answer> {
        lock(&self.0.0)
    }

    /// Whether the write stopped waiting.
    fn gave_up(&self) -> bool {
        self.answer().gave_up
    }

    /// The desktop took the write: the count goes up by one, which the
    /// write returns. Not for a write that stopped waiting: it was not
    /// counted as written, and the desktop taking it is no copy either.
    /// Whether the write heard it: not if it stopped waiting, or heard
    /// already.
    fn taken(&self, count: &AtomicU64) -> bool {
        let mut answer = self.answer();
        if std::mem::replace(&mut answer.said, true) || answer.gave_up {
            return false;
        }
        answer.result = Some(Ok(count.fetch_add(1, Ordering::SeqCst) + 1));
        self.0.1.notify_all();
        true
    }

    fn failed(&self, e: PlatformError) {
        let mut answer = self.answer();
        if std::mem::replace(&mut answer.said, true) {
            return;
        }
        answer.result = Some(Err(e));
        self.0.1.notify_all();
    }

    /// What came of the write, waited for at most `wait`; past that, the
    /// write gives up.
    fn wait(&self, wait: Duration) -> Result<u64, PlatformError> {
        let deadline = Instant::now() + wait;
        let mut answer = self.answer();
        loop {
            if let Some(result) = answer.result.take() {
                return result;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                answer.gave_up = true;
                return Err(PlatformError::Unavailable(
                    "the desktop did not take the clipboard in time",
                ));
            }
            answer = wait_timeout(&self.0.1, answer, left);
        }
    }
}

#[derive(Default)]
struct State {
    /// The running session's task, while there is one.
    commands: Option<mpsc::UnboundedSender<Command>>,
    /// What the clipboard offers, as the desktop last said.
    mimes: Vec<String>,
    /// Whether this session owns it.
    ours: bool,
    /// What this session put on the clipboard, as the desktop has it: what
    /// reading it back and an app pasting get.
    held: Owned,
    /// A write the desktop did not answer in time, which it may still
    /// take: until it says, what is there is not known.
    late: Option<Owned>,
    /// The desktop said who owns the clipboard since the session started.
    known: bool,
    /// When the session's clipboard started: the desktop says who owns
    /// it then, if anyone does (`FIRST_OWNER_WAIT`).
    started: Option<Instant>,
    /// Which session's clipboard it is (`Portal::sessions`), 0 for none.
    session: u64,
    /// How many times the clipboard changed hands in this session.
    changes: u64,
    /// Whether what it offers is marked as not to be shared, once asked,
    /// with `changes` then.
    sensitive: Option<(u64, bool)>,
}

/// What the clipboard and the RemoteDesktop session share.
pub(crate) struct Portal {
    /// Changes every time the clipboard changes hands, but not when this
    /// session took it.
    count: AtomicU64,
    state: Mutex<State>,
    /// Counts the sessions whose clipboard ran.
    sessions: AtomicU64,
}

impl Portal {
    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Follows what the desktop will say of the clipboard of `session`,
    /// created and not started, from now on: once for the session, before
    /// asking for it (`Following::ask`). The desktop having no Clipboard
    /// portal is `Declined`.
    pub(crate) async fn follow(
        self: &Arc<Self>,
        conn: &zbus::Connection,
        session: &Arc<Session<RemoteDesktop>>,
    ) -> Result<Following, Sharing> {
        let path = path_of(session).ok_or_else(|| {
            Sharing::Failed("cannot tell the remote desktop session's path".to_owned())
        })?;
        let followed = async {
            let portal = owner(conn).await?;
            let owners = signals(conn, &portal, "SelectionOwnerChanged").await?;
            let transfers = signals(conn, &portal, "SelectionTransfer").await?;
            zbus::Result::Ok((owners, transfers))
        };
        let (owners, transfers) = followed
            .await
            .map_err(|e| Sharing::Failed(format!("cannot follow the desktop's clipboard: {e}")))?;
        let desktop = match Desktop::with_connection(conn.clone()).await {
            Ok(desktop) => desktop,
            Err(ashpd::Error::PortalNotFound(_)) => {
                info!("the desktop has no clipboard to share with remote control");
                return Err(Sharing::Declined);
            }
            Err(e) => {
                return Err(Sharing::Failed(format!(
                    "cannot reach the Clipboard portal: {e}"
                )));
            }
        };
        Ok(Following {
            portal: self.clone(),
            conn: conn.clone(),
            desktop: Arc::new(desktop),
            session: session.clone(),
            path,
            owners,
            transfers,
        })
    }

    /// The desktop's signals ended (the bus went), while the session may
    /// still be thought running: nothing from here is on the clipboard,
    /// and there is none until the next session.
    fn lost(&self) {
        warn!(
            "the desktop stopped saying what is on the clipboard; there is none until remote control starts again"
        );
        self.end();
    }

    /// The session ended: nothing from here is on the clipboard.
    fn end(&self) {
        let state = std::mem::take(&mut *self.state());
        state.held.finish();
        if let Some(late) = state.late {
            late.finish();
        }
    }

    /// `end`, if the clipboard is still that of the session `session`;
    /// whether it was.
    fn end_of(&self, session: u64) -> bool {
        let state = {
            let mut state = self.state();
            if state.session != session || state.commands.is_none() {
                return false;
            }
            std::mem::take(&mut *state)
        };
        state.held.finish();
        if let Some(late) = state.late {
            late.finish();
        }
        true
    }
}

/// The path of `session`: what ashpd sends for it in its calls, the only
/// way it tells it.
fn path_of(session: &Session<RemoteDesktop>) -> Option<OwnedObjectPath> {
    use zbus::wire::{LE, serialized::Context, to_bytes};

    let data = to_bytes(Context::new(LE, 0), session).ok()?;
    data.deserialize::<OwnedObjectPath>()
        .ok()
        .map(|(path, _)| path)
}

/// The unique name of the portal on the bus, which its signals come from.
async fn owner(conn: &zbus::Connection) -> zbus::Result<zbus::names::OwnedUniqueName> {
    let bus = zbus::fdo::DBusProxy::new(conn).await?;
    let desktop = zbus::names::BusName::try_from(DESKTOP)?;
    Ok(bus.get_name_owner(desktop).await?)
}

/// The Clipboard portal's signal `member`, as the bus delivers it from
/// `portal`. A rule's unique sender is checked here too, for signals sent
/// to this connection alone, which reach it whatever its rules.
async fn signals(
    conn: &zbus::Connection,
    portal: &zbus::names::OwnedUniqueName,
    member: &str,
) -> zbus::Result<zbus::MessageStream> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(portal.as_str())
        .interface(INTERFACE)
        .member(member)
        .path(PATH)
        .build()?;
    zbus::MessageStream::for_match_rule(rule, conn, std::num::NonZeroUsize::new(64)).await
}

/// What asking for the clipboard came to.
#[derive(Debug, PartialEq)]
pub(crate) enum Sharing {
    /// Asked for: the desktop has the request.
    Requested,
    /// The desktop has no clipboard to share, or would not.
    Declined,
    /// Asking failed on this side, before the desktop said anything.
    Failed(String),
}

/// Whether the desktop says the session asked for the clipboard already.
fn asked_already(e: &zbus::Error) -> bool {
    let said = match e {
        zbus::Error::MethodError(_, Some(why), _) => why.clone(),
        zbus::Error::FDO(e) => e.to_string(),
        _ => return false,
    };
    said.to_ascii_lowercase().contains("already")
}

/// What the desktop says of the clipboard of a session, followed from
/// before it was asked for: started with the session.
pub(crate) struct Following {
    portal: Arc<Portal>,
    conn: zbus::Connection,
    desktop: Arc<Desktop>,
    session: Arc<Session<RemoteDesktop>>,
    path: OwnedObjectPath,
    owners: zbus::MessageStream,
    transfers: zbus::MessageStream,
}

impl Following {
    /// Asks for the clipboard. A request asked again, as when the first
    /// was not answered in time but reached the desktop, is answered that
    /// it was asked already: that is requested too.
    pub(crate) async fn ask(&self) -> Sharing {
        match self
            .desktop
            .request(&self.session, RequestClipboardOptions::default())
            .await
        {
            Ok(()) => Sharing::Requested,
            Err(ashpd::Error::Zbus(e)) if asked_already(&e) => Sharing::Requested,
            // Said by the desktop.
            Err(ashpd::Error::Zbus(zbus::Error::MethodError(..) | zbus::Error::FDO(_))) => {
                info!("the desktop shares no clipboard with remote control");
                Sharing::Declined
            }
            Err(e) => Sharing::Failed(format!("cannot ask for the clipboard: {e}")),
        }
    }

    /// The session started, sharing the clipboard if `enabled`: followed
    /// by a task on the session's runtime, until the returned `Running`
    /// is dropped.
    pub(crate) fn start(self, enabled: bool) -> Running {
        let portal = self.portal.clone();
        if !enabled {
            info!("the desktop does not share the clipboard with remote control");
            return Running { portal, task: None };
        }
        let (commands, rx) = mpsc::unbounded_channel();
        let session = portal.sessions.fetch_add(1, Ordering::SeqCst) + 1;
        *portal.state() = State {
            commands: Some(commands),
            started: Some(Instant::now()),
            session,
            ..State::default()
        };
        info!("the clipboard goes through the desktop's portal");
        let spawned = Spawned::default();
        let end = TaskEnd {
            portal: portal.clone(),
            session,
            spawned: spawned.clone(),
        };
        let task = tokio::spawn(run(self, rx, spawned.clone(), end));
        Running {
            portal,
            task: Some((task, spawned)),
        }
    }
}

/// What a session's task started: pastes answered, reads, writes asked
/// for. They end with the session: aborted before what they serve is
/// finished, so none goes on to tell the desktop anything after; what
/// they handed to a blocking thread returns as that finishes it.
#[derive(Clone, Default)]
struct Spawned(Arc<Mutex<JoinSet<()>>>);

impl Spawned {
    /// Starts `work`, which ends with the session at the latest.
    fn spawn(&self, work: impl Future<Output = ()> + Send + 'static) {
        let mut spawned = lock(&self.0);
        while spawned.try_join_next().is_some() {}
        spawned.spawn(work);
    }

    fn abort(&self) {
        lock(&self.0).abort_all();
    }
}

/// The clipboard followed with a running session.
pub(crate) struct Running {
    portal: Arc<Portal>,
    task: Option<(tokio::task::JoinHandle<()>, Spawned)>,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some((task, spawned)) = self.task.take() {
            task.abort();
            spawned.abort();
        }
        self.portal.end();
    }
}

/// What the desktop said in a signal.
#[derive(Debug, PartialEq)]
enum Heard {
    /// The clipboard changed hands.
    Owner { mimes: Vec<String>, ours: bool },
    /// An app pastes `mime`, to be answered with `serial`.
    Transfer { mime: String, serial: u32 },
}

/// What the desktop said, with the serial of its message.
#[derive(Debug, PartialEq)]
struct Said {
    serial: u32,
    heard: Heard,
}

/// What `said` holds, in the order the desktop said it, before and after
/// its message `answer`.
fn around(mut said: Vec<Said>, answer: u32) -> (Vec<Heard>, Vec<Heard>) {
    said.sort_by_key(|s| s.serial);
    let after = said.partition_point(|s| s.serial < answer);
    let later = said.split_off(after);
    (
        said.into_iter().map(|s| s.heard).collect(),
        later.into_iter().map(|s| s.heard).collect(),
    )
}

/// The write the desktop was asked to take, until it answered.
struct Writing {
    id: u64,
    write: Write,
}

/// Why the desktop did not take a write.
enum NotTaken {
    Refused(String),
    /// It did not answer in time, and may still take it.
    Unanswered,
}

/// The serial of the desktop's answer to a write, and whether it took it.
type Answered = (u64, u32, Result<(), NotTaken>);

/// The session's task: what the desktop says, and the writes and reads
/// asked of it.
struct Task {
    portal: Arc<Portal>,
    conn: zbus::Connection,
    desktop: Arc<Desktop>,
    session: Arc<Session<RemoteDesktop>>,
    path: OwnedObjectPath,
    /// Writes waiting their turn.
    queue: VecDeque<Write>,
    writing: Option<Writing>,
    writes: u64,
    /// What the desktop said while a write waited for its answer.
    held_back: Vec<Said>,
    answered: mpsc::UnboundedSender<Answered>,
    spawned: Spawned,
}

/// The end of a session's task, however it ends: what it started ends,
/// and its clipboard if no one ended it already, as when the task panics
/// (tokio drops it then), so nothing stays on the clipboard unfinished;
/// never a later session's.
struct TaskEnd {
    portal: Arc<Portal>,
    session: u64,
    spawned: Spawned,
}

impl Drop for TaskEnd {
    fn drop(&mut self) {
        self.spawned.abort();
        if self.portal.end_of(self.session) {
            warn!(
                "the clipboard stopped while remote control ran; there is none until it starts again"
            );
        }
    }
}

async fn run(
    requested: Following,
    mut commands: mpsc::UnboundedReceiver<Command>,
    spawned: Spawned,
    _end: TaskEnd,
) {
    let Following {
        portal,
        conn,
        desktop,
        session,
        path,
        mut owners,
        mut transfers,
    } = requested;
    let (answered, mut answers) = mpsc::unbounded_channel();
    let mut task = Task {
        portal,
        conn,
        desktop,
        session,
        path,
        queue: VecDeque::new(),
        writing: None,
        writes: 0,
        held_back: Vec::new(),
        answered,
        spawned,
    };
    loop {
        task.next_write();
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Own(write)) => task.queue.push_back(write),
                Some(Command::Read { mime, reply }) => task.read(mime, reply),
                None => return,
            },
            message = owners.next() => match message {
                Some(message) => task.said(message),
                None => return task.lost(),
            },
            message = transfers.next() => match message {
                Some(message) => task.said(message),
                None => return task.lost(),
            },
            Some((id, serial, set)) = answers.recv() => {
                // What the desktop said before its answer came in before
                // it, and waits in the streams if not taken from them yet.
                for stream in [&mut owners, &mut transfers] {
                    while let Some(Some(message)) = stream.next().now_or_never() {
                        if let Some(said) = task.said_of(message) {
                            task.held_back.push(said);
                        }
                    }
                }
                task.answered(id, serial, set);
            }
        }
    }
}

impl Task {
    /// The desktop's signals ended: what the task started ends first, as
    /// with the session's end.
    fn lost(&self) {
        self.spawned.abort();
        self.portal.lost();
    }

    fn said_of(&self, message: zbus::Result<zbus::Message>) -> Option<Said> {
        let message = message
            .inspect_err(|e| debug!(error = %e, "an unreadable clipboard signal"))
            .ok()?;
        let heard = owner_of(&self.path, &message).or_else(|| transfer_of(&self.path, &message))?;
        Some(Said {
            serial: message.primary_header().serial_num().get(),
            heard,
        })
    }

    /// What the desktop said: held back while a write waits for its
    /// answer, as only that tells whether it came before the write.
    fn said(&mut self, message: zbus::Result<zbus::Message>) {
        let Some(said) = self.said_of(message) else {
            return;
        };
        if self.writing.is_some() {
            self.held_back.push(said);
        } else {
            self.hear(said.heard);
        }
    }

    fn hear(&mut self, heard: Heard) {
        match heard {
            Heard::Owner { mimes, ours } => self.owner(mimes, ours),
            Heard::Transfer { mime, serial } => {
                // Nothing while what is there is not known.
                let owned = {
                    let state = self.portal.state();
                    match state.late {
                        Some(_) => Owned::Nothing,
                        None => state.held.clone(),
                    }
                };
                let work = transfer(
                    self.desktop.clone(),
                    self.session.clone(),
                    owned,
                    mime,
                    serial,
                );
                self.spawned.spawn(work);
            }
        }
    }

    /// The desktop says who owns the clipboard now. That it is this
    /// session never takes a write waiting for its answer: said before the
    /// answer, it may be of an earlier write. Said with none waiting, it
    /// makes a write it did not answer in time what is there: a change, as
    /// that write was given up on, and a promise ended then is withdrawn.
    fn owner(&mut self, mimes: Vec<String>, ours: bool) {
        let mut state = self.portal.state();
        let first = !std::mem::replace(&mut state.known, true);
        state.mimes = mimes;
        state.ours = ours;
        state.changes += 1;
        state.sensitive = None;
        if ours {
            let late = match self.writing {
                None => state.late.take(),
                Some(_) => None,
            };
            if let Some(late) = late {
                drop(state);
                self.late_taken(late);
            } else if first {
                self.portal.count.fetch_add(1, Ordering::SeqCst);
            }
            return;
        }
        self.portal.count.fetch_add(1, Ordering::SeqCst);
        let gone = std::mem::take(&mut state.held);
        let late = state.late.take();
        drop(state);
        gone.finish();
        if let Some(late) = late {
            late.finish();
        }
    }

    /// The desktop took `writing`: it is what reading back gives, and what
    /// was there before is let go of, before the write hears it; one that
    /// stopped waiting meanwhile is taken late.
    fn taken(&mut self, writing: Writing) {
        let mut state = self.portal.state();
        if writing.write.withdrawal {
            state.ours = false;
            state.mimes.clear();
            state.known = true;
            return;
        }
        let late = state.late.take();
        state.ours = true;
        state.known = true;
        let before = std::mem::replace(&mut state.held, writing.write.owned.clone());
        drop(state);
        before.finish();
        if let Some(late) = late {
            late.finish();
        }
        if !writing.write.reply.taken(&self.portal.count) {
            self.late_taken(writing.write.owned.clone());
        }
    }

    /// A write that stopped waiting, which the agent took for not written,
    /// is on the clipboard after all, however the desktop said so: a
    /// change, so the agent reads what is there. A promise given up on is
    /// ended: withdrawn (offered with nothing) rather than offered, and
    /// nothing of it given.
    fn late_taken(&mut self, owned: Owned) {
        self.portal.count.fetch_add(1, Ordering::SeqCst);
        owned.finish();
        let ended = owned.ended();
        let mut state = self.portal.state();
        state.ours = true;
        state.known = true;
        let before = std::mem::replace(&mut state.held, if ended { Owned::Nothing } else { owned });
        drop(state);
        before.finish();
        if ended {
            self.queue.push_front(Write {
                owned: Owned::Nothing,
                mimes: Vec::new(),
                reply: Reply::default(),
                withdrawal: true,
            });
        }
    }

    /// The desktop answered the write `id`: what it said before the
    /// answer is heard first, then the answer (the write taken, or nothing
    /// changed), then the rest.
    fn answered(&mut self, id: u64, serial: u32, set: Result<(), NotTaken>) {
        if !self.writing.as_ref().is_some_and(|w| w.id == id) {
            return;
        }
        let (before, after) = around(std::mem::take(&mut self.held_back), serial);
        for heard in before {
            self.hear(heard);
        }
        if let Some(writing) = self.writing.take() {
            match set {
                Ok(()) => self.taken(writing),
                // Nothing changed. Finished before the write hears it.
                Err(NotTaken::Refused(e)) => {
                    writing.write.owned.finish();
                    writing.write.reply.failed(PlatformError::Os(format!(
                        "the desktop did not take the clipboard: {e}"
                    )));
                }
                // It may yet: kept, for if it says it did. Given up on, a
                // promise ends now: the agent lets its offer go.
                Err(NotTaken::Unanswered) => {
                    writing.write.owned.finish();
                    let before = self
                        .portal
                        .state()
                        .late
                        .replace(writing.write.owned.clone());
                    if let Some(before) = before {
                        before.finish();
                    }
                    writing.write.reply.failed(PlatformError::Unavailable(
                        "the desktop did not answer the clipboard in time",
                    ));
                }
            }
        }
        for heard in after {
            self.hear(heard);
        }
    }

    /// The next write, once none waits for the desktop; one that stopped
    /// waiting meanwhile is not carried out.
    fn next_write(&mut self) {
        if self.writing.is_some() {
            return;
        }
        let Some(write) =
            std::iter::from_fn(|| self.queue.pop_front()).find(|w| !w.reply.gave_up())
        else {
            return;
        };
        self.writes += 1;
        let id = self.writes;
        let (conn, session, answered) = (
            self.conn.clone(),
            self.session.clone(),
            self.answered.clone(),
        );
        let mimes = write.mimes.clone();
        self.spawned.spawn(async move {
            // By hand, for the serial of the answer.
            let options = HashMap::from([("mime_types", Variant::from(mimes))]);
            let body = (&*session, options);
            let call =
                conn.call_method(Some(DESKTOP), PATH, Some(INTERFACE), "SetSelection", &body);
            // Unanswered, everything the desktop said meanwhile came
            // before.
            let (serial, set) = match tokio::time::timeout(SET_WAIT, call).await {
                Ok(Ok(reply)) => (reply.primary_header().serial_num().get(), Ok(())),
                Ok(Err(e)) => {
                    let serial = match &e {
                        zbus::Error::MethodError(_, _, reply) => {
                            reply.primary_header().serial_num().get()
                        }
                        // An error named by the D-Bus specification comes
                        // without the reply, so without its serial: as if
                        // unanswered.
                        zbus::Error::FDO(_) => u32::MAX,
                        _ => u32::MAX,
                    };
                    (serial, Err(NotTaken::Refused(e.to_string())))
                }
                Err(_) => (u32::MAX, Err(NotTaken::Unanswered)),
            };
            let _ = answered.send((id, serial, set));
        });
        self.writing = Some(Writing { id, write });
    }

    fn read(&self, mime: String, reply: std::sync::mpsc::Sender<Option<std::os::fd::OwnedFd>>) {
        let (desktop, session) = (self.desktop.clone(), self.session.clone());
        self.spawned.spawn(async move {
            let pipe = desktop.selection_read(&session, &mime).await;
            if let Err(e) = &pipe {
                debug!(error = %e, mime, "the desktop gave no pipe to read the clipboard");
            }
            let _ = reply.send(pipe.ok().map(Into::into));
        });
    }
}

/// What `SelectionOwnerChanged` says of `path`'s session: what the
/// clipboard offers, and whether the session owns it.
fn owner_of(path: &OwnedObjectPath, message: &zbus::Message) -> Option<Heard> {
    if message
        .header()
        .member()
        .is_none_or(|m| m.as_str() != "SelectionOwnerChanged")
    {
        return None;
    }
    let (session, options): (OwnedObjectPath, HashMap<String, OwnedVariant>) =
        message.body().deserialize().ok()?;
    if session != *path {
        return None;
    }
    let mimes = options
        .get("mime_types")
        .and_then(|v| v.downcast_ref::<zbus::Array<'_>>().ok())
        .map(|array| {
            array
                .iter()
                .filter_map(|m| m.downcast_ref::<&str>().ok().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let ours = options
        .get("session_is_owner")
        .and_then(|v| v.downcast_ref::<bool>().ok())
        .unwrap_or(false);
    Some(Heard::Owner { mimes, ours })
}

/// What `SelectionTransfer` asks of `path`'s session: a MIME type, and
/// the serial to answer it with.
fn transfer_of(path: &OwnedObjectPath, message: &zbus::Message) -> Option<Heard> {
    if message
        .header()
        .member()
        .is_none_or(|m| m.as_str() != "SelectionTransfer")
    {
        return None;
    }
    let (session, mime, serial): (OwnedObjectPath, String, u32) =
        message.body().deserialize().ok()?;
    (session == *path).then_some(Heard::Transfer { mime, serial })
}

/// Answers an app's request for `mime` of `owned` through the desktop.
async fn transfer(
    desktop: Arc<Desktop>,
    session: Arc<Session<RemoteDesktop>>,
    owned: Owned,
    mime: String,
    serial: u32,
) {
    let wrote = match desktop.selection_write(&session, serial).await {
        Ok(pipe) => {
            let pipe: std::os::fd::OwnedFd = pipe.into();
            tokio::task::spawn_blocking(move || selection::serve(&owned, &mime, pipe))
                .await
                .unwrap_or(false)
        }
        Err(e) => {
            debug!(error = %e, "the desktop gave no pipe to answer the clipboard");
            false
        }
    };
    if let Err(e) = desktop.selection_write_done(&session, serial, wrote).await {
        debug!(error = %e, "cannot tell the desktop how answering the clipboard went");
    }
}

/// What reading the clipboard works from: the running session's task, what
/// the clipboard offers and how many times it changed hands, whether the
/// session owns it, and what was put there here.
struct Seen {
    commands: mpsc::UnboundedSender<Command>,
    mimes: Vec<String>,
    changes: u64,
    ours: bool,
    held: Owned,
    /// What is there is not known (`State::known`, `State::late`).
    unknown: bool,
}

/// The clipboard of a desktop that shares it through its portal, while
/// remote control runs.
pub struct PortalClipboard {
    portal: Arc<Portal>,
    /// How long a write waits for the desktop to take it.
    write_wait: Duration,
    /// `FIRST_OWNER_WAIT`.
    first_owner_wait: Duration,
}

impl PortalClipboard {
    pub(crate) fn new() -> Self {
        Self {
            portal: Arc::new(Portal {
                count: AtomicU64::new(1),
                state: Mutex::new(State::default()),
                sessions: AtomicU64::new(0),
            }),
            write_wait: WRITE_WAIT,
            first_owner_wait: FIRST_OWNER_WAIT,
        }
    }

    /// One that takes what is there for not known, without the desktop
    /// saying, for `wait` from the start of a session's clipboard.
    #[cfg(test)]
    pub(crate) fn not_knowing_for(mut self, wait: Duration) -> Self {
        self.first_owner_wait = wait;
        self
    }

    /// One whose writes wait at most `wait`, shorter than the desktop's
    /// answer may take.
    #[cfg(test)]
    pub(crate) fn waiting_at_most(mut self, wait: Duration) -> Self {
        self.write_wait = wait;
        self
    }

    /// What the RemoteDesktop session shares it through.
    pub(crate) fn portal(&self) -> Arc<Portal> {
        self.portal.clone()
    }

    fn seen(&self) -> Result<Seen, PlatformError> {
        let state = self.portal.state();
        let waiting = !state.known
            && state
                .started
                .is_some_and(|started| started.elapsed() < self.first_owner_wait);
        Ok(Seen {
            commands: state.commands.clone().ok_or_else(unavailable)?,
            mimes: state.mimes.clone(),
            changes: state.changes,
            ours: state.ours,
            held: state.held.clone(),
            unknown: waiting || state.late.is_some(),
        })
    }

    /// Asks the desktop for `mime`, up to `limit` bytes, by `deadline`.
    fn ask(
        commands: &mpsc::UnboundedSender<Command>,
        mime: &str,
        limit: usize,
        deadline: Instant,
    ) -> Result<Asked, PlatformError> {
        let (reply, pipe) = std::sync::mpsc::channel();
        commands
            .send(Command::Read {
                mime: mime.to_owned(),
                reply,
            })
            .map_err(|_| unavailable())?;
        let left = deadline.saturating_duration_since(Instant::now());
        match pipe.recv_timeout(left.min(ANSWER_WAIT)) {
            Ok(Some(pipe)) => selection::read_pipe(pipe, limit, ANSWER_WAIT, deadline)
                .map_err(|e| PlatformError::Os(format!("cannot read the clipboard: {e}"))),
            // As an app that has nothing of it.
            Ok(None) => Ok(Asked::Data(Vec::new())),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(PlatformError::Os(
                "the desktop did not answer for the clipboard in time".to_owned(),
            )),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(unavailable()),
        }
    }

    /// Whether what `seen` offers is marked as not to be shared: asked
    /// once each time the clipboard changes hands.
    fn sensitive(&self, seen: &Seen, deadline: Instant) -> Result<bool, PlatformError> {
        if let Some((changes, sensitive)) = self.portal.state().sensitive
            && changes == seen.changes
        {
            return Ok(sensitive);
        }
        let offered: Vec<&str> = seen.mimes.iter().map(String::as_str).collect();
        let sensitive = selection::sensitive(&offered, |mime: &str, limit| {
            Self::ask(&seen.commands, mime, limit, deadline)
        })?;
        let mut state = self.portal.state();
        if state.changes == seen.changes {
            state.sensitive = Some((seen.changes, sensitive));
        }
        Ok(sensitive)
    }

    /// Offers `owned`, and waits for the desktop to take it. Not taken in
    /// time, it is not counted as written however it goes; refused, nothing
    /// from here changed on the clipboard.
    fn own(&mut self, owned: Owned) -> Result<u64, PlatformError> {
        let commands = self.seen()?.commands;
        let mimes = owned
            .mimes(Wire::Wayland)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let reply = Reply::default();
        commands
            .send(Command::Own(Write {
                owned: owned.clone(),
                mimes,
                reply: reply.clone(),
                withdrawal: false,
            }))
            .map_err(|_| unavailable())?;
        let written = reply.wait(self.write_wait);
        if written.is_err() {
            // Given up on, a promise ends now: the agent lets its offer go.
            owned.finish();
        }
        written
    }
}

/// Data-control is used from now on (`WaylandClipboard`): what was put
/// on the clipboard here is let go of, promises finished, and withdrawn
/// with a `SetSelection` offering nothing while the session runs and it
/// is, or may be, there. Pastes until then get nothing; reads ask the
/// desktop.
impl LetsGo for PortalClipboard {
    fn let_go(&mut self) {
        let mut state = self.portal.state();
        let held = std::mem::take(&mut state.held);
        let late = state.late.take();
        // Read from the desktop from now on, whether or not it takes the
        // withdrawal.
        let ours = std::mem::take(&mut state.ours);
        if (ours || late.is_some())
            && let Some(commands) = &state.commands
        {
            let _ = commands.send(Command::Own(Write {
                owned: Owned::Nothing,
                mimes: Vec::new(),
                reply: Reply::default(),
                withdrawal: true,
            }));
        }
        drop(state);
        held.finish();
        if let Some(late) = late {
            late.finish();
        }
    }
}

impl Clipboard for PortalClipboard {
    fn change_count(&self) -> Result<u64, PlatformError> {
        Ok(self.portal.count.load(Ordering::SeqCst))
    }

    fn read(&self, max_len: usize) -> Result<ClipboardRead, PlatformError> {
        let seen = self.seen()?;
        if seen.unknown {
            return Err(not_known());
        }
        if seen.ours {
            return Ok(seen.held.read_back(max_len));
        }
        let deadline = Instant::now() + READ_WAIT;
        if self.sensitive(&seen, deadline)? {
            return Ok(ClipboardRead::Sensitive);
        }
        let offered: Vec<&str> = seen.mimes.iter().map(String::as_str).collect();
        selection::read_offered(Wire::Wayland, &offered, max_len, |mime: &str, limit| {
            Self::ask(&seen.commands, mime, limit, deadline)
        })
    }

    fn write(&mut self, content: &ClipboardContent) -> Result<u64, PlatformError> {
        self.own(Owned::Content(Arc::new(Written::new(content))))
    }

    fn write_promise(&mut self, promise: ClipboardPromise) -> Result<u64, PlatformError> {
        self.own(Owned::Promise(Arc::new(Promised::new(promise))))
    }

    /// The files Nautilus or another file manager copied, by their lists
    /// of URIs. None when the content is sensitive or was put there here.
    fn files(&self) -> Result<Vec<PathBuf>, PlatformError> {
        let seen = self.seen()?;
        if seen.unknown {
            return Err(not_known());
        }
        if seen.ours {
            return Ok(Vec::new());
        }
        let deadline = Instant::now() + READ_WAIT;
        if self.sensitive(&seen, deadline)? {
            return Ok(Vec::new());
        }
        let offered: Vec<&str> = seen.mimes.iter().map(String::as_str).collect();
        selection::files_offered(&offered, |mime: &str, limit| {
            Self::ask(&seen.commands, mime, limit, deadline)
        })
    }

    fn promises_files(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(serial: u32, ours: bool) -> Said {
        Said {
            serial,
            heard: Heard::Owner {
                mimes: Vec::new(),
                ours,
            },
        }
    }

    fn paste(serial: u32, asked: u32) -> Said {
        Said {
            serial,
            heard: Heard::Transfer {
                mime: "text/plain".to_owned(),
                serial: asked,
            },
        }
    }

    #[test]
    fn what_the_desktop_said_is_put_in_order_around_its_answer() {
        // Taken from the streams in another order than said.
        let said = vec![owner(9, false), paste(4, 1), owner(5, true), paste(12, 2)];
        let (before, after) = around(said, 7);
        assert_eq!(before, [paste(4, 1).heard, owner(5, true).heard,]);
        assert_eq!(after, [owner(9, false).heard, paste(12, 2).heard]);
        // No answer: everything came before.
        let (before, after) = around(vec![owner(3, false)], u32::MAX);
        assert_eq!((before.len(), after.len()), (1, 0));
    }

    #[test]
    fn a_task_ending_ends_its_sessions_clipboard_only() {
        #[derive(Default)]
        struct Gone(std::sync::atomic::AtomicUsize);
        impl crate::ClipboardSource for Gone {
            fn fetch(&self, _: crate::ClipboardFormat) -> Option<Vec<u8>> {
                None
            }
            fn finished(&self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let portal = PortalClipboard::new().portal();
        let source = Arc::new(Gone::default());
        let (commands, _rx) = mpsc::unbounded_channel();
        *portal.state() = State {
            commands: Some(commands),
            session: 2,
            held: Owned::Promise(Arc::new(Promised::new(ClipboardPromise {
                formats: vec![crate::ClipboardFormat::Text],
                bytes: 1,
                files: Vec::new(),
                source: source.clone(),
            }))),
            ..State::default()
        };
        let end = |session| TaskEnd {
            portal: portal.clone(),
            session,
            spawned: Spawned::default(),
        };
        // An earlier session's task: nothing of this one.
        drop(end(1));
        assert!(portal.state().commands.is_some());
        assert_eq!(source.0.load(Ordering::SeqCst), 0);
        // Its own, as when it panicked: ended.
        drop(end(2));
        assert!(portal.state().commands.is_none());
        assert_eq!(source.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn letting_go_leaves_nothing_taken_for_this_sessions() {
        let mut clipboard = PortalClipboard::new();
        let (commands, mut rx) = mpsc::unbounded_channel();
        *clipboard.portal.state() = State {
            commands: Some(commands),
            ours: true,
            known: true,
            mimes: vec!["text/plain".to_owned()],
            ..State::default()
        };
        clipboard.let_go();
        // Read from the desktop, even should it not take the withdrawal.
        let seen = clipboard.seen().unwrap();
        assert!(!seen.ours && !seen.unknown);
        assert!(matches!(
            rx.try_recv(),
            Ok(Command::Own(Write {
                withdrawal: true,
                ..
            }))
        ));
    }

    #[test]
    fn signals_ending_leave_no_clipboard() {
        #[derive(Default)]
        struct Gone(std::sync::atomic::AtomicBool);
        impl crate::ClipboardSource for Gone {
            fn fetch(&self, _: crate::ClipboardFormat) -> Option<Vec<u8>> {
                None
            }
            fn finished(&self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let promised = |source: &Arc<Gone>| {
            Owned::Promise(Arc::new(Promised::new(ClipboardPromise {
                formats: vec![crate::ClipboardFormat::Text],
                bytes: 1,
                files: Vec::new(),
                source: source.clone(),
            })))
        };
        let (held, late) = (Arc::new(Gone::default()), Arc::new(Gone::default()));
        let clipboard = PortalClipboard::new();
        let (commands, _rx) = mpsc::unbounded_channel();
        *clipboard.portal.state() = State {
            commands: Some(commands),
            ours: true,
            known: true,
            held: promised(&held),
            late: Some(promised(&late)),
            ..State::default()
        };
        clipboard.portal.lost();
        assert!(held.0.load(Ordering::SeqCst) && late.0.load(Ordering::SeqCst));
        assert!(clipboard.portal.state().commands.is_none());
        assert!(matches!(
            clipboard.read(1 << 20),
            Err(PlatformError::Unavailable(_))
        ));
    }

    #[test]
    fn a_write_that_stopped_waiting_is_not_counted() {
        let count = AtomicU64::new(5);
        let reply = Reply::default();
        assert!(matches!(
            reply.wait(Duration::from_millis(20)),
            Err(PlatformError::Unavailable(_))
        ));
        assert!(reply.gave_up());
        reply.taken(&count);
        assert_eq!(count.load(Ordering::SeqCst), 5);

        let reply = Reply::default();
        let answering = reply.clone();
        let count = Arc::new(AtomicU64::new(5));
        let counting = count.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            answering.taken(&counting);
            // Said once.
            answering.taken(&counting);
            answering.failed(PlatformError::Unavailable("late"));
        });
        assert_eq!(reply.wait(Duration::from_secs(5)).unwrap(), 6);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(count.load(Ordering::SeqCst), 6);

        // Never carried out, it failed.
        let reply = Reply::default();
        drop(Write {
            owned: Owned::Nothing,
            mimes: Vec::new(),
            reply: reply.clone(),
            withdrawal: false,
        });
        assert!(matches!(
            reply.wait(Duration::from_secs(1)),
            Err(PlatformError::Unavailable(_))
        ));
    }
}
