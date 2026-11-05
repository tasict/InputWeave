//! Clipboard promises that have files, held by the app (docs/architecture.md
//! §9). On Windows the broker owns the user's clipboard for content,
//! but File Explorer pastes files that do not exist yet only from a data
//! object that lives as long as the paste: CFSTR_FILEDESCRIPTORW and
//! CFSTR_FILECONTENTS, read on a thread of its own with progress. The app,
//! which runs as the user and already promises dragged files that way, puts
//! such a promise on the clipboard (`ClipboardFilesPromised`); when File
//! Explorer pastes, it chooses a folder of its own, the agent checks the
//! user can write in it and fetches the files there, written by the broker
//! as the user (`PasteClipboardFiles`, `ClipboardFilesArrived`).
//!
//! Only apps that say they take clipboard files (`ClientHello`) hold such
//! promises, the one connected last. With none connected the app is
//! started, through the rules of `app::AppOpener`, and waited for a few
//! seconds, not at all once the broker says it cannot start it or the user
//! quit it; an app that does not take them, or none at all, leaves the
//! promise to the broker, which promises the content without the files.
//!
//! The app asks before it touches the clipboard (`ClipboardFilesPlace`),
//! and may only while the promise is waited for and nothing was copied on
//! this computer since it was decided: so a promise given up on never
//! replaces what the broker promised instead, nor a later copy. Until the
//! promise is placed, the source is its caller's, which may still promise
//! the content otherwise; from then on it is the hub's, and it ends when
//! the app says the clipboard no longer holds it, or goes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use iw_platform::windows::{WindowsFormat, clipboard_representations};
use iw_platform::{ClipboardFormat, ClipboardPromise, ClipboardSource, FilesFetch, PlatformError};
use iw_proto::app::v1::{
    ClipboardFilesArrived, ClipboardFilesPromised, ClipboardInline, DraggedItem,
};
use tokio::sync::mpsc;
use tracing::{debug, info};

/// How long a promise waits for an app to start, then for it to ask to put
/// the promise on the clipboard, then, once it may, for it to say it did.
pub(crate) const APP_WAIT: Duration = Duration::from_secs(5);

/// The most of the clipboard's other content handed to the app along with
/// the files, in bytes; larger content is left out.
pub(crate) const INLINE_MAX: u64 = 1024 * 1024;

/// The most of that content as the Windows clipboard holds it, in bytes:
/// text in UTF-16 takes up to twice the content. What would go past it is
/// left out; it keeps the event well inside an IPC frame.
pub(crate) const INLINE_WINDOWS_MAX: usize = 3 * 1024 * 1024;

/// How long that content is waited for.
const INLINE_WAIT: Duration = Duration::from_secs(5);

/// What an app that takes clipboard files hears.
#[derive(Debug)]
pub(crate) enum AppEvent {
    Promised(ClipboardFilesPromised),
    Arrived(ClipboardFilesArrived),
}

/// Has the app started: when it was asked to, now or moments ago; `None`
/// when it is not, as when it cannot be.
pub(crate) type Start = Box<dyn Fn() -> Option<Instant> + Send + Sync>;

/// Whether the clipboard is still the one a promise was decided on:
/// nothing was copied on this computer since.
pub(crate) type Unchanged = Arc<dyn Fn() -> bool + Send + Sync>;

pub(crate) struct AppClipboard {
    state: Mutex<State>,
    changed: Condvar,
    /// Apps connected over IPC, whether they take clipboard files or not.
    ui_clients: Arc<AtomicUsize>,
    start: Start,
    /// How long a promise waits for an app (`APP_WAIT`; shorter in tests).
    wait: Duration,
}

#[derive(Default)]
struct State {
    /// Apps that take clipboard files, in the order they connected.
    apps: Vec<(u64, mpsc::UnboundedSender<AppEvent>)>,
    next_app: u64,
    next_promise: u64,
    promises: HashMap<u64, Held>,
    /// The user quit the app: it is not started for files on the
    /// clipboard until an app connects again.
    quit: bool,
    /// When the broker last said it could not start the app.
    start_failed: Option<Instant>,
}

/// A promise handed to an app.
struct Held {
    app: u64,
    source: Arc<dyn ClipboardSource>,
    items: usize,
    unchanged: Unchanged,
    placement: Placement,
    /// The hub holds the source: `promise` returned, and the source ends
    /// with the app's promise. Until then the caller does.
    owned: bool,
    paste: Paste,
}

impl Held {
    /// Whether the app put it on the clipboard, or is taken to have.
    fn placed(&self) -> bool {
        self.placement == Placement::Placed
            || (self.owned && matches!(self.placement, Placement::Granted(_)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    /// Handed to the app, which has not asked to place it.
    Offered,
    /// The app may put it on the clipboard, since then.
    Granted(Instant),
    Placed,
    /// The app could not put it there.
    Failed,
    /// Something was copied on this computer before the app asked.
    Superseded,
}

enum Paste {
    Not,
    Underway,
    Done(ClipboardFilesArrived),
}

/// What a paste of a promise does.
pub(crate) enum Pasting {
    /// The first fetches the files, from this source.
    Start(Arc<dyn ClipboardSource>),
    /// One before fetches them, or did: the app hears once, or again, when
    /// they are there.
    Joined,
    /// Not a promise the app put on the clipboard.
    Unknown,
}

/// An app that takes clipboard files, for as long as it is connected.
pub(crate) struct AppSession {
    hub: Arc<AppClipboard>,
    id: u64,
}

impl AppSession {
    pub(crate) fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for AppSession {
    fn drop(&mut self) {
        self.hub.disconnected(self.id);
    }
}

impl AppClipboard {
    pub(crate) fn new(ui_clients: Arc<AtomicUsize>, start: Start, wait: Duration) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::default(),
            changed: Condvar::new(),
            ui_clients,
            start,
            wait,
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wait_for_change<'a>(
        &self,
        state: MutexGuard<'a, State>,
        left: Duration,
    ) -> MutexGuard<'a, State> {
        self.changed
            .wait_timeout(state, left)
            .unwrap_or_else(|e| e.into_inner())
            .0
    }

    /// An app that takes clipboard files connected; it hears what to
    /// promise on the receiver.
    pub(crate) fn connect(self: &Arc<Self>) -> (AppSession, mpsc::UnboundedReceiver<AppEvent>) {
        let (events, receiver) = mpsc::unbounded_channel();
        let id = {
            let mut state = self.state();
            state.next_app += 1;
            let id = state.next_app;
            state.apps.push((id, events));
            state.quit = false;
            id
        };
        self.changed.notify_all();
        debug!(app = id, "an app that takes clipboard files connected");
        (
            AppSession {
                hub: self.clone(),
                id,
            },
            receiver,
        )
    }

    /// The app went: the promises it put on the clipboard are gone from
    /// it, or will be once it ends. Those it did not put there are left to
    /// whoever waits for them.
    fn disconnected(&self, app: u64) {
        let gone: Vec<Arc<dyn ClipboardSource>> = {
            let mut state = self.state();
            state.apps.retain(|(id, _)| *id != app);
            let ids: Vec<u64> = state
                .promises
                .iter()
                .filter(|(_, held)| held.app == app)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| state.promises.remove(&id))
                .filter(|held| held.owned)
                .map(|held| held.source)
                .collect()
        };
        self.changed.notify_all();
        for source in gone {
            source.finished();
        }
    }

    /// The user quit the app: it is not started for files on the
    /// clipboard until an app connects again.
    pub(crate) fn user_quit(&self) {
        self.state().quit = true;
        info!("the user quit the app; it is not started for files on the clipboard");
    }

    /// An app that does not take files on the clipboard connected: a
    /// promise waiting for one to start stops waiting.
    pub(crate) fn client_connected(&self) {
        self.changed.notify_all();
    }

    /// The broker could not start the app: a promise waiting for it stops.
    pub(crate) fn start_failed(&self) {
        self.state().start_failed = Some(Instant::now());
        self.changed.notify_all();
    }

    /// Hands `promise`, which has files, to an app to put on the
    /// clipboard, and waits for it to: on the clipboard's thread. With no
    /// app connected, one is started and waited for while the rest of the
    /// content is fetched. The app may put it there only while the
    /// clipboard is `unchanged`. Fails, with nothing on the clipboard and
    /// the source still the caller's, when no app takes it; once the app
    /// may put it there, never: from then on it is the hub's.
    pub(crate) fn promise(
        &self,
        promise: &ClipboardPromise,
        unchanged: Unchanged,
    ) -> Result<(), PlatformError> {
        let started = self.start_if_needed()?;
        let inline = inline(promise);
        let mut state = self.state();
        if let Some(at) = started {
            let deadline = at + self.wait;
            while state.apps.is_empty() {
                if state.start_failed.is_some_and(|failed| failed >= at) {
                    return Err(PlatformError::Unavailable(
                        "the app could not be started to take files on the clipboard",
                    ));
                }
                // An app older than files on the clipboard: none will come.
                if self.ui_clients.load(Ordering::SeqCst) > 0 {
                    return Err(PlatformError::Unsupported(
                        "the app connected does not take files on the clipboard",
                    ));
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(PlatformError::Unavailable(
                        "the app did not start to take files on the clipboard",
                    ));
                }
                state = self.wait_for_change(state, left);
            }
        }
        let Some((app, events)) = state.apps.last().cloned() else {
            return Err(PlatformError::Unavailable(
                "no app to take files on the clipboard",
            ));
        };
        state.next_promise += 1;
        let id = state.next_promise;
        let event = ClipboardFilesPromised {
            promise_id: id,
            from_device_id: promise.source.origin().unwrap_or_default(),
            items: promise
                .files
                .iter()
                .map(|file| DraggedItem {
                    name: file.name.clone(),
                    folder: file.folder,
                    ..DraggedItem::default()
                })
                .collect(),
            inline,
        };
        if events.send(AppEvent::Promised(event)).is_err() {
            return Err(PlatformError::Unavailable("the app is gone"));
        }
        state.promises.insert(
            id,
            Held {
                app,
                source: promise.source.clone(),
                items: promise.files.len(),
                unchanged,
                placement: Placement::Offered,
                owned: false,
                paste: Paste::Not,
            },
        );
        let offered = Instant::now() + self.wait;
        loop {
            let placement = state.promises.get(&id).map(|held| held.placement);
            let deadline = match placement {
                None => return Err(PlatformError::Unavailable("the app went")),
                Some(Placement::Placed) => {
                    if let Some(held) = state.promises.get_mut(&id) {
                        held.owned = true;
                    }
                    return Ok(());
                }
                Some(Placement::Failed) => {
                    state.promises.remove(&id);
                    return Err(PlatformError::Unsupported(
                        "the app could not put the files on the clipboard",
                    ));
                }
                Some(Placement::Superseded) => {
                    state.promises.remove(&id);
                    return Err(PlatformError::Unavailable(
                        "something was copied before the app put the files on the clipboard",
                    ));
                }
                Some(Placement::Offered) => offered,
                Some(Placement::Granted(at)) => at + self.wait,
            };
            let left = deadline.saturating_duration_since(Instant::now());
            if !left.is_zero() {
                state = self.wait_for_change(state, left);
                continue;
            }
            if matches!(placement, Some(Placement::Granted(_))) {
                // It may be on the clipboard by now, so nothing else is
                // promised: the hub holds it until the app says it is not
                // there, or goes.
                info!(
                    promise = id,
                    "the app did not say whether it put the files on the clipboard"
                );
                if let Some(held) = state.promises.get_mut(&id) {
                    held.owned = true;
                }
                return Ok(());
            }
            // Asking late, it is told so, and leaves the clipboard alone.
            state.promises.remove(&id);
            return Err(PlatformError::Unavailable(
                "the app did not take the files on the clipboard in time",
            ));
        }
    }

    /// Starts the app when none that takes files is connected; returns
    /// when it was asked to start, if it was.
    fn start_if_needed(&self) -> Result<Option<Instant>, PlatformError> {
        {
            let state = self.state();
            if !state.apps.is_empty() {
                return Ok(None);
            }
            if self.ui_clients.load(Ordering::SeqCst) > 0 {
                return Err(PlatformError::Unsupported(
                    "the app connected does not take files on the clipboard",
                ));
            }
            if state.quit {
                debug!("the user quit the app; promising the clipboard without its files");
                return Err(PlatformError::Unavailable("the user quit the app"));
            }
        }
        match (self.start)() {
            Some(at) => Ok(Some(at)),
            None => Err(PlatformError::Unavailable(
                "no app to take files on the clipboard",
            )),
        }
    }

    /// The app `app` asks whether it may put promise `id` on the
    /// clipboard: only while it is waited for and the clipboard did not
    /// change since. Refused with the code the app hears.
    pub(crate) fn place(&self, app: u64, id: u64) -> Result<(), &'static str> {
        let unchanged = {
            let state = self.state();
            match state.promises.get(&id) {
                Some(held) if held.app == app => held.unchanged.clone(),
                Some(_) => return Err("not_found"),
                None if id != 0 && id <= state.next_promise => return Err("too_late"),
                None => return Err("not_found"),
            }
        };
        // Checked outside the hub's lock: it takes the broker's.
        let same = unchanged();
        let answer = {
            let mut state = self.state();
            match state.promises.get_mut(&id).filter(|held| held.app == app) {
                None => Err("too_late"),
                Some(held) => match held.placement {
                    Placement::Offered if same => {
                        held.placement = Placement::Granted(Instant::now());
                        Ok(())
                    }
                    Placement::Offered => {
                        held.placement = Placement::Superseded;
                        Err("superseded")
                    }
                    Placement::Granted(_) | Placement::Placed => Ok(()),
                    Placement::Failed => Err("too_late"),
                    Placement::Superseded => Err("superseded"),
                },
            }
        };
        self.changed.notify_all();
        answer
    }

    /// The app `app` put promise `id`, which it was allowed to, on the
    /// clipboard, or could not. False for a promise it may not have put
    /// there.
    pub(crate) fn placed(&self, app: u64, id: u64, ok: bool) -> bool {
        let (known, gone) = {
            let mut state = self.state();
            match state.promises.get_mut(&id).filter(|held| held.app == app) {
                None => (false, None),
                Some(held) => match (held.placement, ok) {
                    (Placement::Granted(_), true) => {
                        held.placement = Placement::Placed;
                        (true, None)
                    }
                    (Placement::Placed, true) => (true, None),
                    // Taken to be there, but it is not after all.
                    (Placement::Offered | Placement::Granted(_), false) if held.owned => {
                        (true, state.promises.remove(&id).map(|held| held.source))
                    }
                    (Placement::Offered | Placement::Granted(_), false) => {
                        held.placement = Placement::Failed;
                        (true, None)
                    }
                    _ => (false, None),
                },
            }
        };
        self.changed.notify_all();
        if let Some(source) = gone {
            source.finished();
        }
        known
    }

    /// Whether app `app` put promise `id` on the clipboard.
    pub(crate) fn holds(&self, app: u64, id: u64) -> bool {
        self.state()
            .promises
            .get(&id)
            .is_some_and(|held| held.app == app && held.placed())
    }

    /// App `app` pastes promise `id`: the first paste fetches its files,
    /// the others share that.
    pub(crate) fn begin_paste(&self, app: u64, id: u64) -> Pasting {
        let mut state = self.state();
        let State { promises, apps, .. } = &mut *state;
        let Some(held) = promises
            .get_mut(&id)
            .filter(|held| held.app == app && held.placed())
        else {
            return Pasting::Unknown;
        };
        match &held.paste {
            Paste::Not => {
                held.paste = Paste::Underway;
                Pasting::Start(held.source.clone())
            }
            Paste::Underway => Pasting::Joined,
            Paste::Done(arrived) => {
                if let Some((_, events)) = apps.iter().find(|(a, _)| *a == app) {
                    let _ = events.send(AppEvent::Arrived(arrived.clone()));
                }
                Pasting::Joined
            }
        }
    }

    /// The files of promise `id` that app `app` pastes ended, as `fetched`:
    /// it hears where they are.
    pub(crate) fn arrived(&self, app: u64, id: u64, fetched: FilesFetch) {
        let mut state = self.state();
        let State { promises, apps, .. } = &mut *state;
        let Some(held) = promises.get_mut(&id).filter(|held| held.app == app) else {
            return;
        };
        let event = arrived_event(id, held.items, fetched);
        held.paste = Paste::Done(event.clone());
        if let Some((_, events)) = apps.iter().find(|(a, _)| *a == app) {
            let _ = events.send(AppEvent::Arrived(event));
        }
    }

    /// The clipboard no longer holds promise `id` of app `app`, and no
    /// paste of it waits.
    pub(crate) fn gone(&self, app: u64, id: u64) -> bool {
        let held = {
            let mut state = self.state();
            match state.promises.get(&id) {
                Some(held) if held.app == app => state.promises.remove(&id),
                _ => None,
            }
        };
        self.changed.notify_all();
        match held {
            Some(held) => {
                if held.owned {
                    held.source.finished();
                }
                true
            }
            None => false,
        }
    }

    /// Whether an app that takes clipboard files is connected.
    #[cfg(test)]
    pub(crate) fn connected(&self) -> bool {
        !self.state().apps.is_empty()
    }
}

/// What the app hears of the files of promise `id`, `items` of them.
fn arrived_event(id: u64, items: usize, fetched: FilesFetch) -> ClipboardFilesArrived {
    let paths: Vec<String> = match fetched {
        FilesFetch::Ready(paths) => paths
            .into_iter()
            .map(|path| {
                path.map(|p: PathBuf| p.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect(),
        FilesFetch::Busy | FilesFetch::Unavailable => vec![String::new(); items],
    };
    ClipboardFilesArrived {
        promise_id: id,
        ok: paths.iter().any(|p| !p.is_empty()),
        paths,
    }
}

/// The clipboard's other content, for the app to put next to the files,
/// when it is small, as the Windows clipboard holds it
/// (`clipboard_representations`, the broker's own conversions): text, HTML
/// and RTF, never an image, which this SYSTEM process does not decode.
/// Fetched ahead of any paste, off this thread, and left out if it takes
/// longer than [`INLINE_WAIT`] or cannot be had, which leaves the source to
/// fetch it again when pasted.
fn inline(promise: &ClipboardPromise) -> Vec<ClipboardInline> {
    let formats: Vec<ClipboardFormat> = promise
        .formats
        .iter()
        .copied()
        .filter(|format| *format != ClipboardFormat::Png)
        .collect();
    if formats.is_empty() || promise.bytes > INLINE_MAX {
        return Vec::new();
    }
    let source = promise.source.clone();
    let (answer, answered) = std::sync::mpsc::channel();
    let fetched = std::thread::Builder::new()
        .name("clipboard inline".to_owned())
        .spawn(move || {
            let got: Vec<(WindowsFormat, Vec<u8>)> = formats
                .into_iter()
                .filter_map(|format| source.fetch_ahead(format).map(|data| (format, data)))
                .filter_map(|(format, data)| clipboard_representations(format, &data))
                .collect();
            let _ = answer.send(got);
        });
    if fetched.is_err() {
        return Vec::new();
    }
    let got = answered.recv_timeout(INLINE_WAIT).unwrap_or_default();
    let mut total = 0;
    got.into_iter()
        .filter(|(_, data)| {
            let fits = total + data.len() <= INLINE_WINDOWS_MAX;
            if fits {
                total += data.len();
            }
            fits
        })
        .map(|(format, data)| match format {
            WindowsFormat::Standard(standard) => ClipboardInline {
                standard,
                name: String::new(),
                data,
            },
            WindowsFormat::Registered(name) => ClipboardInline {
                standard: 0,
                name: name.to_owned(),
                data,
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iw_platform::{ClipboardContent, PromisedFile};
    use std::sync::atomic::AtomicBool;

    struct Source {
        content: ClipboardContent,
        finished: AtomicUsize,
        /// How long content takes to come.
        delay: Duration,
    }

    impl ClipboardSource for Source {
        fn fetch(&self, format: ClipboardFormat) -> Option<Vec<u8>> {
            std::thread::sleep(self.delay);
            self.content.get(format).map(<[u8]>::to_vec)
        }

        fn finished(&self) {
            self.finished.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn promise(text: &str) -> (ClipboardPromise, Arc<Source>) {
        promise_after(text, Duration::ZERO)
    }

    fn promise_after(text: &str, delay: Duration) -> (ClipboardPromise, Arc<Source>) {
        let source = Arc::new(Source {
            content: ClipboardContent {
                items: vec![(ClipboardFormat::Text, text.as_bytes().to_vec())],
            },
            finished: AtomicUsize::new(0),
            delay,
        });
        let promise = ClipboardPromise {
            formats: vec![ClipboardFormat::Text],
            bytes: text.len() as u64,
            files: vec![
                PromisedFile {
                    name: "report.txt".to_owned(),
                    folder: false,
                },
                PromisedFile {
                    name: "Photos".to_owned(),
                    folder: true,
                },
            ],
            source: source.clone(),
        };
        (promise, source)
    }

    fn finished(source: &Source) -> usize {
        source.finished.load(Ordering::SeqCst)
    }

    /// A hub whose app is never started, waiting a moment.
    fn hub(ui: usize) -> Arc<AppClipboard> {
        hub_starting(
            ui,
            Box::new(|| panic!("started")),
            Duration::from_millis(300),
        )
    }

    fn hub_starting(ui: usize, start: Start, wait: Duration) -> Arc<AppClipboard> {
        AppClipboard::new(Arc::new(AtomicUsize::new(ui)), start, wait)
    }

    fn same() -> Unchanged {
        Arc::new(|| true)
    }

    /// The first promise the app hears of.
    fn promised(events: &mut mpsc::UnboundedReceiver<AppEvent>) -> ClipboardFilesPromised {
        loop {
            match events.try_recv() {
                Ok(AppEvent::Promised(promised)) => return promised,
                Ok(event) => panic!("{event:?}"),
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }

    /// An app that asks to put the promise it hears of on the clipboard
    /// and, when it may, says it did, or could not.
    fn app_placing(
        hub: &Arc<AppClipboard>,
        app: u64,
        mut events: mpsc::UnboundedReceiver<AppEvent>,
        ok: bool,
    ) -> std::thread::JoinHandle<(ClipboardFilesPromised, mpsc::UnboundedReceiver<AppEvent>)> {
        let hub = hub.clone();
        std::thread::spawn(move || {
            let promised = promised(&mut events);
            if hub.place(app, promised.promise_id).is_ok() {
                assert!(hub.placed(app, promised.promise_id, ok));
            }
            (promised, events)
        })
    }

    /// The app hears the promise with the items and the small content,
    /// asks, puts it on the clipboard, and the promise holds; pastes share
    /// one fetch.
    #[test]
    fn an_app_is_handed_the_promise_and_puts_it_on_the_clipboard() {
        let hub = hub(1);
        let (session, events) = hub.connect();
        let app = session.id();
        let (promise, source) = promise("report.txt\nPhotos");
        let placer = app_placing(&hub, app, events, true);
        hub.promise(&promise, same()).unwrap();
        let (promised, mut events) = placer.join().unwrap();
        let names: Vec<_> = promised
            .items
            .iter()
            .map(|i| (i.name.as_str(), i.folder))
            .collect();
        assert_eq!(names, [("report.txt", false), ("Photos", true)]);
        // As the Windows clipboard holds it.
        assert_eq!(promised.inline.len(), 1);
        assert_eq!(promised.inline[0].standard, 13);
        assert_eq!(
            promised.inline[0].data,
            "report.txt\r\nPhotos\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        );
        let id = promised.promise_id;
        assert!(hub.holds(app, id));
        assert!(!hub.holds(app + 1, id));
        assert!(matches!(hub.begin_paste(app + 1, id), Pasting::Unknown));

        // One fetch, one answer; a paste after it hears again.
        assert!(matches!(hub.begin_paste(app, id), Pasting::Start(_)));
        assert!(matches!(hub.begin_paste(app, id), Pasting::Joined));
        hub.arrived(app, id, FilesFetch::Unavailable);
        assert!(matches!(events.try_recv(), Ok(AppEvent::Arrived(a)) if a.promise_id == id));
        assert!(events.try_recv().is_err());
        assert!(matches!(hub.begin_paste(app, id), Pasting::Joined));
        assert!(matches!(events.try_recv(), Ok(AppEvent::Arrived(a)) if a.promise_id == id));

        // Gone from the clipboard: let go of once.
        assert!(hub.gone(app, id));
        assert!(!hub.gone(app, id));
        assert_eq!(finished(&source), 1);
    }

    /// An app that could not, or never asks, leaves the promise to the
    /// caller, which still has a live source; asking late, it is told so.
    #[test]
    fn an_app_that_cannot_or_does_not_answer_is_passed_over() {
        let hub = hub(1);
        let (session, events) = hub.connect();
        let app = session.id();
        let (promise, source) = promise("x");
        let refuser = app_placing(&hub, app, events, false);
        assert!(matches!(
            hub.promise(&promise, same()),
            Err(PlatformError::Unsupported(_))
        ));
        let (_, mut events) = refuser.join().unwrap();
        // Nobody asks: given up after the wait.
        let started = Instant::now();
        assert!(matches!(
            hub.promise(&promise, same()),
            Err(PlatformError::Unavailable(_))
        ));
        assert!(started.elapsed() >= Duration::from_millis(250));
        let late = promised(&mut events);
        assert_eq!(hub.place(app, late.promise_id), Err("too_late"));
        assert!(!hub.placed(app, late.promise_id, true));
        assert!(!hub.holds(app, late.promise_id));
        assert_eq!(hub.place(app, late.promise_id + 1), Err("not_found"));
        assert_eq!(finished(&source), 0);
    }

    /// Something copied on this computer since the promise was decided:
    /// the app may not put it on the clipboard.
    #[test]
    fn a_clipboard_changed_meanwhile_is_left_alone() {
        let hub = hub(1);
        let (session, mut events) = hub.connect();
        let app = session.id();
        let (promise, source) = promise("x");
        let copied = Arc::new(AtomicBool::new(false));
        let unchanged: Unchanged = {
            let copied = copied.clone();
            Arc::new(move || !copied.load(Ordering::SeqCst))
        };
        let asker = {
            let hub = hub.clone();
            std::thread::spawn(move || {
                let promised = promised(&mut events);
                copied.store(true, Ordering::SeqCst);
                hub.place(app, promised.promise_id)
            })
        };
        assert!(matches!(
            hub.promise(&promise, unchanged),
            Err(PlatformError::Unavailable(_))
        ));
        assert_eq!(asker.join().unwrap(), Err("superseded"));
        assert_eq!(finished(&source), 0);
    }

    /// Allowed to put it there, the app is waited for, and then taken to
    /// have: nothing else is promised, and the promise ends with the app.
    #[test]
    fn a_promise_the_app_may_place_is_held_even_without_an_answer() {
        let hub = hub(1);
        let (session, mut events) = hub.connect();
        let app = session.id();
        let (promise, source) = promise("x");
        let asker = {
            let hub = hub.clone();
            std::thread::spawn(move || {
                let promised = promised(&mut events);
                assert_eq!(hub.place(app, promised.promise_id), Ok(()));
                promised.promise_id
            })
        };
        let started = Instant::now();
        hub.promise(&promise, same()).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(250));
        let id = asker.join().unwrap();
        assert!(hub.holds(app, id));
        assert_eq!(finished(&source), 0);
        drop(session);
        assert_eq!(finished(&source), 1);
    }

    /// Taken to be on the clipboard, the app says it could not put it
    /// there after all: it ends.
    #[test]
    fn a_promise_taken_to_be_placed_ends_when_it_was_not() {
        let hub = hub(1);
        let (session, mut events) = hub.connect();
        let app = session.id();
        let (promise, source) = promise("x");
        let asker = {
            let hub = hub.clone();
            std::thread::spawn(move || {
                let promised = promised(&mut events);
                hub.place(app, promised.promise_id).unwrap();
                (promised.promise_id, events)
            })
        };
        hub.promise(&promise, same()).unwrap();
        let (id, _events) = asker.join().unwrap();
        assert!(hub.placed(app, id, false));
        assert_eq!(finished(&source), 1);
        assert!(!hub.holds(app, id));
    }

    /// The content is fetched while the app starts, and the app has the
    /// whole wait to ask once it hears of the promise.
    #[test]
    fn the_content_is_fetched_while_the_app_starts() {
        let hub = Arc::new(Mutex::new(None::<Arc<AppClipboard>>));
        let start: Start = {
            let hub = hub.clone();
            Box::new(move || {
                let hub = hub.lock().unwrap().clone().unwrap();
                std::thread::spawn(move || {
                    let (session, mut events) = hub.connect();
                    let promised = promised(&mut events);
                    assert_eq!(promised.inline.len(), 1);
                    std::thread::sleep(Duration::from_millis(100));
                    hub.place(session.id(), promised.promise_id).unwrap();
                    hub.placed(session.id(), promised.promise_id, true);
                    std::mem::forget(session);
                });
                Some(Instant::now())
            })
        };
        let clipboard = hub_starting(0, start, Duration::from_millis(300));
        *hub.lock().unwrap() = Some(clipboard.clone());
        let (promise, _) = promise_after("x", Duration::from_millis(400));
        clipboard.promise(&promise, same()).unwrap();
    }

    /// With no app at all, it is started and waited for; with an app that
    /// does not take files, nothing is started; once the broker says it
    /// cannot start it, it is not waited for.
    #[test]
    fn the_app_is_started_only_when_none_is_connected() {
        let (promise, _) = promise("x");
        assert!(matches!(
            hub(1).promise(&promise, same()),
            Err(PlatformError::Unsupported(_))
        ));

        let hub = Arc::new(Mutex::new(None::<Arc<AppClipboard>>));
        let started = Arc::new(AtomicUsize::new(0));
        let start: Start = {
            let (hub, started) = (hub.clone(), started.clone());
            Box::new(move || {
                started.fetch_add(1, Ordering::SeqCst);
                let hub = hub.lock().unwrap().clone().unwrap();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(50));
                    let (session, events) = hub.connect();
                    let _ = app_placing(&hub, session.id(), events, true).join();
                    std::mem::forget(session);
                });
                Some(Instant::now())
            })
        };
        let clipboard = hub_starting(0, start, Duration::from_secs(5));
        *hub.lock().unwrap() = Some(clipboard.clone());
        clipboard.promise(&promise, same()).unwrap();
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert!(clipboard.connected());

        let failing: Arc<Mutex<Option<Arc<AppClipboard>>>> = Arc::default();
        let start: Start = {
            let failing = failing.clone();
            Box::new(move || {
                let hub = failing.lock().unwrap().clone().unwrap();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(20));
                    hub.start_failed();
                });
                Some(Instant::now())
            })
        };
        let clipboard = hub_starting(0, start, Duration::from_secs(5));
        *failing.lock().unwrap() = Some(clipboard.clone());
        let at = Instant::now();
        assert!(matches!(
            clipboard.promise(&promise, same()),
            Err(PlatformError::Unavailable(_))
        ));
        assert!(at.elapsed() < Duration::from_secs(2));
    }

    /// An app older than files on the clipboard connects while one is
    /// waited for to start: none will come, and the wait ends.
    #[test]
    fn an_app_that_does_not_take_files_ends_the_wait() {
        let ui = Arc::new(AtomicUsize::new(0));
        let hub = Arc::new(Mutex::new(None::<Arc<AppClipboard>>));
        let start: Start = {
            let (hub, ui) = (hub.clone(), ui.clone());
            Box::new(move || {
                let (hub, ui) = (hub.lock().unwrap().clone().unwrap(), ui.clone());
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(50));
                    ui.fetch_add(1, Ordering::SeqCst);
                    hub.client_connected();
                });
                Some(Instant::now())
            })
        };
        let clipboard = AppClipboard::new(ui, start, Duration::from_secs(5));
        *hub.lock().unwrap() = Some(clipboard.clone());
        let (promise, _) = promise("x");
        let at = Instant::now();
        assert!(matches!(
            clipboard.promise(&promise, same()),
            Err(PlatformError::Unsupported(_))
        ));
        assert!(at.elapsed() < Duration::from_secs(2));
    }

    /// Once the user quit the app, it is not started for files on the
    /// clipboard, until an app connects again.
    #[test]
    fn an_app_the_user_quit_is_not_started_again() {
        let started = Arc::new(AtomicUsize::new(0));
        let start: Start = {
            let started = started.clone();
            Box::new(move || {
                started.fetch_add(1, Ordering::SeqCst);
                None
            })
        };
        let hub = hub_starting(0, start, Duration::from_millis(100));
        let (promise, _) = promise("x");
        hub.user_quit();
        assert!(matches!(
            hub.promise(&promise, same()),
            Err(PlatformError::Unavailable(_))
        ));
        assert_eq!(started.load(Ordering::SeqCst), 0);
        drop(hub.connect());
        assert!(hub.promise(&promise, same()).is_err());
        assert_eq!(started.load(Ordering::SeqCst), 1);
    }

    /// The app going ends the promises it put on the clipboard, and leaves
    /// one it had not to the caller, who stops waiting.
    #[test]
    fn an_app_going_lets_go_of_what_it_put_on_the_clipboard() {
        let hub = hub(1);
        let (session, events) = hub.connect();
        let (placed, source) = promise("x");
        let placer = app_placing(&hub, session.id(), events, true);
        hub.promise(&placed, same()).unwrap();
        let _events = placer.join().unwrap();
        drop(session);
        assert_eq!(finished(&source), 1);
        assert!(!hub.connected());

        let hub = hub_starting(1, Box::new(|| None), Duration::from_secs(5));
        let (session, mut events) = hub.connect();
        let (promise, source) = promise("x");
        let leaving = std::thread::spawn(move || {
            promised(&mut events);
            drop(session);
        });
        let at = Instant::now();
        assert!(matches!(
            hub.promise(&promise, same()),
            Err(PlatformError::Unavailable(_))
        ));
        assert!(at.elapsed() < Duration::from_secs(2));
        leaving.join().unwrap();
        assert_eq!(finished(&source), 0);
    }

    #[test]
    fn what_arrived_names_each_item() {
        let ready = FilesFetch::Ready(vec![Some(PathBuf::from("/s/report.txt")), None]);
        let event = arrived_event(7, 2, ready);
        assert!(event.ok);
        assert_eq!(event.paths, ["/s/report.txt", ""]);
        let event = arrived_event(7, 2, FilesFetch::Unavailable);
        assert!(!event.ok);
        assert_eq!(event.paths, ["", ""]);
    }

    /// An image copied with the files does not come along, nor is it
    /// asked for: the text does.
    #[test]
    fn an_image_copied_with_files_is_left_out() {
        let (mut promise, _) = promise("report.txt");
        let source = Arc::new(Source {
            content: ClipboardContent {
                items: vec![
                    (ClipboardFormat::Text, b"report.txt".to_vec()),
                    (ClipboardFormat::Png, b"\x89PNG\r\n\x1a\n".to_vec()),
                ],
            },
            finished: AtomicUsize::new(0),
            delay: Duration::ZERO,
        });
        promise.formats = vec![ClipboardFormat::Text, ClipboardFormat::Png];
        promise.bytes = 18;
        promise.source = source;
        let inline = inline(&promise);
        assert_eq!(inline.len(), 1);
        assert_eq!(inline[0].standard, 13);
        // Only an image: nothing.
        promise.formats = vec![ClipboardFormat::Png];
        assert!(super::inline(&promise).is_empty());
    }

    #[test]
    fn large_content_is_left_out() {
        let (mut promise, _) = promise("x");
        promise.bytes = INLINE_MAX + 1;
        assert!(inline(&promise).is_empty());
    }
}
