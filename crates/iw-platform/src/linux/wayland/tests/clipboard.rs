//! The Wayland clipboards against stand-ins. Data-control on headless
//! sway, another app played by `Peer` (a data-control client of the test's
//! own, offering what it is given) and by wl-copy and wl-paste; and the
//! Clipboard portal, played by `FakeClipboard` beside the fake
//! RemoteDesktop portal. Ignored, like the rest of `tests`; they share the
//! process' `WAYLAND_DISPLAY`, so they run one at a time.

use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_callback, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1 as device, zwlr_data_control_manager_v1 as manager,
    zwlr_data_control_offer_v1 as offer, zwlr_data_control_source_v1 as source,
};

use super::super::clipboard::{LetsGo, WaylandClipboard};
use super::super::{DataControlClipboard, PortalClipboard};
use super::*;
use crate::{
    Clipboard, ClipboardContent, ClipboardFormat, ClipboardPromise, ClipboardRead, ClipboardSource,
    FilesFetch, PlatformError, PromisedFile,
};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR not really";
/// How long asking for the clipboard waits for the fake desktop.
const CLIPBOARD_WAIT: Duration = Duration::from_secs(2);
const UTF8: &str = "text/plain;charset=utf-8";

fn content(items: &[(ClipboardFormat, &[u8])]) -> ClipboardContent {
    ClipboardContent {
        items: items.iter().map(|(f, d)| (*f, d.to_vec())).collect(),
    }
}

fn changed_from(clipboard: &dyn Clipboard, count: u64) {
    wait_until(&format!("the change count to move from {count}"), || {
        clipboard.change_count().is_ok_and(|now| now != count)
    });
}

/// A source whose content and files are given, which notes what it was
/// asked for.
#[derive(Default)]
struct Source {
    content: ClipboardContent,
    files: Mutex<Option<FilesFetch>>,
    fetched: Mutex<Vec<ClipboardFormat>>,
    files_asked: Mutex<usize>,
    finished: AtomicBool,
    /// How long fetching takes.
    slow: Duration,
}

impl ClipboardSource for Source {
    fn fetch(&self, format: ClipboardFormat) -> Option<Vec<u8>> {
        self.fetched.lock().unwrap().push(format);
        std::thread::sleep(self.slow);
        self.content.get(format).map(<[u8]>::to_vec)
    }

    fn fetch_files(&self) -> FilesFetch {
        *self.files_asked.lock().unwrap() += 1;
        self.files
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(FilesFetch::Unavailable)
    }

    fn finished(&self) {
        self.finished.store(true, Ordering::SeqCst);
    }
}

fn promise(
    source: &Arc<Source>,
    formats: &[ClipboardFormat],
    files: &[PromisedFile],
) -> ClipboardPromise {
    ClipboardPromise {
        formats: formats.to_vec(),
        bytes: 1,
        files: files.to_vec(),
        source: source.clone(),
    }
}

fn two_files() -> Vec<PromisedFile> {
    vec![
        PromisedFile {
            name: "a b.txt".into(),
            folder: false,
        },
        PromisedFile {
            name: "Photos".into(),
            folder: true,
        },
    ]
}

/// What a URI list of `paths` holds, with `\n` between them.
fn uris(paths: &[&PathBuf]) -> String {
    paths
        .iter()
        .map(|p| format!("file://{}", p.display()).replace(' ', "%20"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------
// Data-control, on headless sway.

/// The data-control clipboard, connected (on a thread of its own: the
/// first calls are `Unavailable`).
fn connected() -> DataControlClipboard {
    let clipboard = DataControlClipboard::new();
    wait_until("the clipboard to connect", || {
        clipboard.change_count().is_ok()
    });
    clipboard
}

/// Headless sway, which the process' Wayland clients connect to.
fn sway() -> Sway {
    let sway = Sway::start();
    use_sway(&sway);
    sway
}

fn use_sway(sway: &Sway) {
    // SAFETY: the ignored tests run one at a time.
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", sway.runtime.path());
        std::env::set_var("WAYLAND_DISPLAY", &sway.display);
    }
}

/// What wl-paste pastes as `mime`, or `None` when it could not.
fn wl_paste(mime: &str) -> Option<Vec<u8>> {
    let out = Command::new("wl-paste")
        .args(["--no-newline", "--type", mime])
        .stderr(Stdio::null())
        .output()
        .expect("wl-paste is installed in the Linux container");
    out.status.success().then_some(out.stdout)
}

/// The MIME types wl-paste sees offered.
fn wl_types() -> Vec<String> {
    let out = Command::new("wl-paste")
        .arg("--list-types")
        .output()
        .expect("wl-paste");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// wl-copy holding the clipboard with `data` as `mime`, until dropped.
struct WlCopy(Child);

fn wl_copy(mime: &str, data: &[u8]) -> WlCopy {
    let mut child = Command::new("wl-copy")
        .args(["--foreground", "--type", mime])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("wl-copy is installed in the Linux container");
    child.stdin.take().unwrap().write_all(data).unwrap();
    WlCopy(child)
}

impl Drop for WlCopy {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Another app holding the clipboard through data-control, offering
/// `items` (MIME type, bytes), until dropped.
struct Peer {
    conn: Connection,
    qh: QueueHandle<PeerState>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// The MIME types it was asked for.
    asked: Arc<Mutex<Vec<String>>>,
}

struct PeerState {
    items: HashMap<String, Vec<u8>>,
    /// Never answers, holding every pipe open.
    silent: bool,
    /// How long each answer takes.
    slow: Duration,
    held: Vec<OwnedFd>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Peer {
    fn own(items: &[(&str, &[u8])]) -> Self {
        Self::start(items, false, Duration::ZERO)
    }

    /// One that never answers.
    fn silent(mimes: &[&str]) -> Self {
        let items: Vec<(&str, &[u8])> = mimes.iter().map(|m| (*m, &b""[..])).collect();
        Self::start(&items, true, Duration::ZERO)
    }

    /// One that takes `slow` over each answer.
    fn slowly(items: &[(&str, &[u8])], slow: Duration) -> Self {
        Self::start(items, false, slow)
    }

    fn start(items: &[(&str, &[u8])], silent: bool, slow: Duration) -> Self {
        let conn = Connection::connect_to_env().unwrap();
        let (globals, mut queue) = registry_queue_init::<PeerState>(&conn).unwrap();
        let qh = queue.handle();
        let manager: manager::ZwlrDataControlManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).unwrap();
        let device = manager.get_data_device(&seat, &qh, ());
        let source = manager.create_data_source(&qh, ());
        for (mime, _) in items {
            source.offer((*mime).to_owned());
        }
        device.set_selection(Some(&source));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let mut state = PeerState {
            items: items
                .iter()
                .map(|(m, d)| ((*m).to_owned(), d.to_vec()))
                .collect(),
            silent,
            slow,
            held: Vec::new(),
            asked: asked.clone(),
        };
        queue.roundtrip(&mut state).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = std::thread::spawn(move || {
            // Kept while it runs.
            let _kept = (manager, device, source);
            while !stopping.load(Ordering::SeqCst) {
                if queue.blocking_dispatch(&mut state).is_err() {
                    break;
                }
            }
        });
        Self {
            conn,
            qh,
            stop,
            thread: Some(thread),
            asked,
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.conn.display().sync(&self.qh, ());
        let _ = self.conn.flush();
        let _ = self.thread.take().unwrap().join();
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for PeerState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for PeerState {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for PeerState {
    fn event(
        _: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<manager::ZwlrDataControlManagerV1, ()> for PeerState {
    fn event(
        _: &mut Self,
        _: &manager::ZwlrDataControlManagerV1,
        _: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<device::ZwlrDataControlDeviceV1, ()> for PeerState {
    fn event(
        _: &mut Self,
        _: &device::ZwlrDataControlDeviceV1,
        _: device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    wayland_client::event_created_child!(PeerState, device::ZwlrDataControlDeviceV1, [
        device::EVT_DATA_OFFER_OPCODE => (offer::ZwlrDataControlOfferV1, ()),
    ]);
}

impl Dispatch<offer::ZwlrDataControlOfferV1, ()> for PeerState {
    fn event(
        _: &mut Self,
        _: &offer::ZwlrDataControlOfferV1,
        _: offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<source::ZwlrDataControlSourceV1, ()> for PeerState {
    fn event(
        state: &mut Self,
        _: &source::ZwlrDataControlSourceV1,
        event: source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let source::Event::Send { mime_type, fd } = event {
            state.asked.lock().unwrap().push(mime_type.clone());
            if state.silent {
                state.held.push(fd);
                return;
            }
            let data = state.items.get(&mime_type).cloned().unwrap_or_default();
            let slow = state.slow;
            std::thread::spawn(move || {
                std::thread::sleep(slow);
                let _ = std::fs::File::from(fd).write_all(&data);
            });
        }
    }
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_reads_what_another_app_copied() {
    let _sway = sway();
    let clipboard = connected();
    assert_eq!(clipboard.read(1 << 20).unwrap(), ClipboardRead::Empty);
    let count = clipboard.change_count().unwrap();

    let copy = wl_copy("text/plain", "théirs\r\nline".as_bytes());
    changed_from(&clipboard, count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(
            ClipboardFormat::Text,
            "théirs\nline".as_bytes()
        )]))
    );
    drop(copy);

    let count = clipboard.change_count().unwrap();
    let peer = Peer::own(&[
        (UTF8, b"text"),
        ("text/html", b"<b>text</b>"),
        ("text/rtf", b"{\\rtf1 text}"),
        ("image/png", PNG),
    ]);
    changed_from(&clipboard, count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[
            (ClipboardFormat::Text, b"text"),
            (ClipboardFormat::Rtf, b"{\\rtf1 text}"),
            (ClipboardFormat::Html, b"<b>text</b>"),
            (ClipboardFormat::Png, PNG),
        ]))
    );
    // Reading changes nothing.
    let count = clipboard.change_count().unwrap();
    clipboard.read(1 << 20).unwrap();
    assert_eq!(clipboard.change_count().unwrap(), count);
    drop(peer);

    // Large content, and past the limit.
    let large = vec![b'x'; 600 * 1024];
    let count = clipboard.change_count().unwrap();
    let _peer = Peer::own(&[(UTF8, &large)]);
    changed_from(&clipboard, count);
    assert!(matches!(
        clipboard.read(100 * 1024).unwrap(),
        ClipboardRead::TooLarge(n) if n > 100 * 1024
    ));
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, &large)]))
    );
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_leaves_what_password_managers_mark_alone() {
    let _sway = sway();
    let clipboard = connected();
    let count = clipboard.change_count().unwrap();
    let peer = Peer::own(&[(UTF8, b"hunter2"), ("x-kde-passwordManagerHint", b"secret")]);
    changed_from(&clipboard, count);
    assert_eq!(clipboard.read(1 << 20).unwrap(), ClipboardRead::Sensitive);
    assert_eq!(clipboard.files().unwrap(), Vec::<PathBuf>::new());
    assert!(!peer.asked.lock().unwrap().contains(&UTF8.to_owned()));
    drop(peer);

    // Right after, the same kind of app with a name that is not secret.
    let count = clipboard.change_count().unwrap();
    let _peer = Peer::own(&[(UTF8, b"alice"), ("x-kde-passwordManagerHint", b"public")]);
    changed_from(&clipboard, count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"alice")]))
    );
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_lists_files_a_file_manager_copied() {
    let _sway = sway();
    let clipboard = connected();
    let count = clipboard.change_count().unwrap();
    // GNOME's list, with nothing local, beside a URI list that has them.
    let peer = Peer::own(&[
        ("x-special/gnome-copied-files", b"copy\nsftp://nas/x"),
        (
            "text/uri-list",
            b"file:///tmp/a%20b\r\nfile:///home/u/Photos\r\n",
        ),
    ]);
    changed_from(&clipboard, count);
    assert_eq!(
        clipboard.files().unwrap(),
        [PathBuf::from("/tmp/a b"), PathBuf::from("/home/u/Photos")]
    );
    drop(peer);
    let count = clipboard.change_count().unwrap();
    let _peer = Peer::own(&[("x-special/gnome-copied-files", b"cut\nfile:///tmp/x")]);
    changed_from(&clipboard, count);
    assert_eq!(clipboard.files().unwrap(), [PathBuf::from("/tmp/x")]);
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_writes_what_other_apps_paste() {
    let _sway = sway();
    let mut clipboard = connected();
    let count = clipboard
        .write(&content(&[
            (ClipboardFormat::Text, "café\nline".as_bytes()),
            (ClipboardFormat::Html, b"<b>caf\xc3\xa9</b>"),
            (ClipboardFormat::Png, PNG),
        ]))
        .unwrap();
    // Its own write is not a copy, and reads back as written.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(clipboard.change_count().unwrap(), count);
    assert!(matches!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(c) if c.get(ClipboardFormat::Text) == Some("café\nline".as_bytes())
    ));
    let types = wl_types();
    for mime in [UTF8, "text/plain", "UTF8_STRING", "text/html", "image/png"] {
        assert!(types.iter().any(|t| t == mime), "{mime} in {types:?}");
    }
    // No Latin-1 text on Wayland.
    for mime in ["STRING", "TEXT"] {
        assert!(!types.iter().any(|t| t == mime), "{mime} in {types:?}");
    }
    assert_eq!(wl_paste(UTF8).unwrap(), "café\nline".as_bytes());
    assert_eq!(wl_paste("text/plain").unwrap(), "café\nline".as_bytes());
    assert_eq!(wl_paste("text/html").unwrap(), "<b>café</b>".as_bytes());
    assert_eq!(wl_paste("image/png").unwrap(), PNG);
    assert!(wl_paste("text/rtf").is_none());
    assert_eq!(clipboard.change_count().unwrap(), count);

    // Large content.
    let large = vec![b'y'; 700 * 1024];
    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, &large)]))
        .unwrap();
    assert_eq!(wl_paste(UTF8).unwrap(), large);

    // Another app's copy counts, and is what is read.
    let _copy = wl_copy("text/plain", b"theirs");
    changed_from(&clipboard, count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"theirs")]))
    );
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_text_is_utf8_both_ways() {
    let _sway = sway();
    let mut clipboard = connected();
    let text = "鍵盤滑鼠 café";
    // Written here, wl-paste gets it whole.
    clipboard
        .write(&content(&[(ClipboardFormat::Text, text.as_bytes())]))
        .unwrap();
    assert_eq!(wl_paste(UTF8).unwrap(), text.as_bytes());
    assert_eq!(wl_paste("UTF8_STRING").unwrap(), text.as_bytes());
    assert_eq!(wl_paste("STRING"), None);

    // wl-copy's (offered as STRING and TEXT too, all UTF-8).
    let count = clipboard.change_count().unwrap();
    let copy = wl_copy("text/plain", text.as_bytes());
    changed_from(&clipboard, count);
    let read = ClipboardRead::Content(content(&[(ClipboardFormat::Text, text.as_bytes())]));
    assert_eq!(clipboard.read(1 << 20).unwrap(), read);
    drop(copy);

    // Offered as STRING alone: UTF-8 where it is, Latin-1 where not.
    let count = clipboard.change_count().unwrap();
    let peer = Peer::own(&[("STRING", text.as_bytes())]);
    changed_from(&clipboard, count);
    assert_eq!(clipboard.read(1 << 20).unwrap(), read);
    drop(peer);
    let count = clipboard.change_count().unwrap();
    let _peer = Peer::own(&[("STRING", b"caf\xe9")]);
    changed_from(&clipboard, count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, "café".as_bytes())]))
    );
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_fetches_a_promise_as_it_is_pasted() {
    let _sway = sway();
    let mut clipboard = connected();
    let source = Arc::new(Source {
        content: content(&[
            (ClipboardFormat::Text, b"promised"),
            (ClipboardFormat::Png, PNG),
        ]),
        // Long enough for every paste to come while the first is fetched.
        slow: Duration::from_secs(2),
        ..Source::default()
    });
    let count = clipboard
        .write_promise(promise(
            &source,
            &[ClipboardFormat::Text, ClipboardFormat::Png],
            &[],
        ))
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(clipboard.change_count().unwrap(), count);
    // Its own promise is never read back.
    assert_eq!(clipboard.read(1 << 20).unwrap(), ClipboardRead::Empty);
    let types = wl_types();
    assert!(types.iter().any(|t| t == "image/png"), "{types:?}");
    assert!(!types.iter().any(|t| t == "text/uri-list"), "{types:?}");
    assert!(source.fetched.lock().unwrap().is_empty());

    // Six apps pasting at once: one fetch.
    let pastes: Vec<_> = (0..6)
        .map(|_| std::thread::spawn(|| wl_paste(UTF8)))
        .collect();
    for paste in pastes {
        assert_eq!(paste.join().unwrap().unwrap(), b"promised");
    }
    assert_eq!(*source.fetched.lock().unwrap(), [ClipboardFormat::Text]);
    // Each format as it is pasted; the source keeps the bytes, and gives
    // them again.
    assert_eq!(wl_paste("image/png").unwrap(), PNG);
    assert_eq!(wl_paste("text/plain").unwrap(), b"promised");
    assert_eq!(
        *source.fetched.lock().unwrap(),
        [
            ClipboardFormat::Text,
            ClipboardFormat::Png,
            ClipboardFormat::Text
        ]
    );
    assert!(!source.finished.load(Ordering::SeqCst));

    // Written over here, the promise is finished.
    clipboard
        .write(&content(&[(ClipboardFormat::Text, b"mine")]))
        .unwrap();
    wait_until("the promise to be finished", || {
        source.finished.load(Ordering::SeqCst)
    });

    // And by another app's copy.
    let source = Arc::new(Source::default());
    let count = clipboard
        .write_promise(promise(&source, &[ClipboardFormat::Text], &[]))
        .unwrap();
    let _copy = wl_copy("text/plain", b"theirs");
    changed_from(&clipboard, count);
    wait_until("the promise to be finished", || {
        source.finished.load(Ordering::SeqCst)
    });
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_pastes_promised_files_from_where_they_arrived() {
    let _sway = sway();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a b.txt");
    let folder = dir.path().join("Photos");
    std::fs::write(&file, b"x").unwrap();
    std::fs::create_dir(&folder).unwrap();
    let mut clipboard = connected();
    let source = Arc::new(Source {
        files: Mutex::new(Some(FilesFetch::Ready(vec![
            Some(file.clone()),
            Some(folder.clone()),
        ]))),
        ..Source::default()
    });
    clipboard
        .write_promise(promise(&source, &[], &two_files()))
        .unwrap();
    let types = wl_types();
    for mime in [
        "text/uri-list",
        "x-special/gnome-copied-files",
        "x-kde-passwordManagerHint",
        UTF8,
    ] {
        assert!(types.iter().any(|t| t == mime), "{mime} in {types:?}");
    }
    // The names and the hint without the files.
    assert_eq!(wl_paste(UTF8).unwrap(), b"a b.txt\nPhotos");
    assert_eq!(wl_paste("x-kde-passwordManagerHint").unwrap(), b"secret");
    assert_eq!(*source.files_asked.lock().unwrap(), 0);
    let list = String::from_utf8(wl_paste("text/uri-list").unwrap()).unwrap();
    assert_eq!(
        list.replace("\r\n", "\n").trim_end(),
        uris(&[&file, &folder])
    );
    let gnome = String::from_utf8(wl_paste("x-special/gnome-copied-files").unwrap()).unwrap();
    assert_eq!(gnome, format!("copy\n{}", uris(&[&file, &folder])));
    // Asked once, and kept for every later paste.
    assert_eq!(*source.files_asked.lock().unwrap(), 1);

    // Still coming: nothing pasted, and the next paste asks again; what
    // did not come is left out.
    let source = Arc::new(Source {
        files: Mutex::new(Some(FilesFetch::Busy)),
        ..Source::default()
    });
    clipboard
        .write_promise(promise(&source, &[], &two_files()))
        .unwrap();
    assert_eq!(wl_paste("text/uri-list").unwrap_or_default(), b"");
    assert_eq!(*source.files_asked.lock().unwrap(), 1);
    *source.files.lock().unwrap() = Some(FilesFetch::Ready(vec![None, Some(folder.clone())]));
    let list = String::from_utf8(wl_paste("text/uri-list").unwrap()).unwrap();
    assert_eq!(list.replace("\r\n", "\n").trim_end(), uris(&[&folder]));
    assert_eq!(*source.files_asked.lock().unwrap(), 2);
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_an_app_that_never_answers_holds_a_read_up_only_so_long() {
    let _sway = sway();
    let clipboard = connected();
    let count = clipboard.change_count().unwrap();
    let peer = Peer::silent(&[UTF8, "text/html"]);
    changed_from(&clipboard, count);
    let started = Instant::now();
    assert!(clipboard.read(1 << 20).is_err());
    let took = started.elapsed();
    assert!(
        took >= Duration::from_secs(3) && took < Duration::from_secs(9),
        "{took:?}"
    );
    drop(peer);
    let count = clipboard.change_count().unwrap();
    let _peer = Peer::own(&[(UTF8, b"later")]);
    changed_from(&clipboard, count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"later")]))
    );
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_a_managers_copy_of_the_last_write_is_still_it() {
    let _sway = sway();
    let mut clipboard = connected();
    // What a clipboard manager (Klipper, wl-clip-persist) takes over: every
    // MIME type offered, with what each holds.
    // What holds nothing is left out, as some managers do.
    let taken_over = || -> Vec<(String, Vec<u8>)> {
        wl_types()
            .into_iter()
            .map(|mime| {
                let data = wl_paste(&mime).unwrap_or_default();
                (mime, data)
            })
            .filter(|(_, data)| !data.is_empty())
            .collect()
    };
    let offer = |items: &[(String, Vec<u8>)]| {
        let items: Vec<(&str, &[u8])> = items
            .iter()
            .map(|(m, d)| (m.as_str(), d.as_slice()))
            .collect();
        Peer::own(&items)
    };

    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, b"first")]))
        .unwrap();
    let first = taken_over();
    assert!(
        first
            .iter()
            .any(|(m, _)| m.starts_with("application/x-inputweave-source."))
    );
    let manager = offer(&first);
    // Not a copy: the write, read back as written.
    assert_eq!(clipboard.change_count().unwrap(), count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"first")]))
    );
    // Pasted from the manager now.
    assert_eq!(wl_paste(UTF8).unwrap(), b"first");
    drop(manager);

    // Its copy of an older write, as picked from its history, is a copy.
    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, b"second")]))
        .unwrap();
    let _picked = offer(&first);
    changed_from(&clipboard, count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"first")]))
    );

    // And another app's copy after a manager's of the last write.
    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, b"third")]))
        .unwrap();
    let _manager = offer(&taken_over());
    assert_eq!(clipboard.change_count().unwrap(), count);
    let _copy = wl_copy("text/plain", b"theirs");
    changed_from(&clipboard, count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"theirs")]))
    );
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_is_chosen_once_a_compositor_out_of_reach_answers() {
    // A compositor that is not there yet, under the name it will have.
    let sway = Sway::start();
    use_sway(&sway);
    let runtime = sway.runtime.clone();
    drop(sway);
    let started = Instant::now();
    let clipboard = WaylandClipboard::new(
        Box::new(DataControlClipboard::new()),
        Box::new(PortalClipboard::new()),
    );
    // Nothing waits for the compositor.
    assert!(matches!(
        clipboard.read(1 << 20),
        Err(PlatformError::Unavailable(_))
    ));
    assert!(matches!(
        clipboard.change_count(),
        Err(PlatformError::Unavailable(_))
    ));
    assert!(started.elapsed() < Duration::from_millis(500));

    let sway = Sway::start_in(runtime);
    use_sway(&sway);
    wait_until("the new compositor", compositor_up);
    wait_until("data-control to be used", || {
        clipboard.change_count().is_ok()
    });
    let count = clipboard.change_count().unwrap();
    let _copy = wl_copy("text/plain", b"there");
    changed_from(&clipboard, count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"there")]))
    );
}

/// Holds `sway` still (SIGSTOP) or lets it go on (SIGCONT), as a
/// compositor that hangs for a while.
fn hold(sway: &Sway, held: bool) {
    let signal = if held { libc::SIGSTOP } else { libc::SIGCONT };
    // SAFETY: a signal to the test's own child.
    unsafe { libc::kill(sway.child.id() as libc::pid_t, signal) };
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_waits_for_no_compositor() {
    let sway = sway();
    hold(&sway, true);
    let clipboard = DataControlClipboard::new();
    let started = Instant::now();
    for _ in 0..5 {
        assert!(matches!(
            clipboard.change_count(),
            Err(PlatformError::Unavailable(_))
        ));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(started.elapsed() < Duration::from_secs(1));
    hold(&sway, false);
    wait_until("the clipboard to connect", || {
        clipboard.change_count().is_ok()
    });
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_lets_go_of_a_promise_it_cannot_take_yet() {
    // No compositor yet: the promise is let go of, its source told so
    // once. (Only one with no data-control is left to be put elsewhere.)
    let sway = sway();
    drop(sway);
    let mut clipboard = DataControlClipboard::new();
    let source = Arc::new(Source::default());
    assert!(matches!(
        clipboard.write_promise(promise(&source, &[ClipboardFormat::Text], &[])),
        Err(PlatformError::Unavailable(_))
    ));
    assert!(source.finished.load(Ordering::SeqCst));
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_reads_back_what_the_selection_names() {
    let sway = sway();
    let mut clipboard = connected();
    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, b"first")]))
        .unwrap();
    // The compositor holds a newer write up past its wait: the selection
    // is still the first, and what is read back is that.
    hold(&sway, true);
    assert!(matches!(
        clipboard.write(&content(&[(ClipboardFormat::Text, b"second")])),
        Err(PlatformError::Unavailable(_))
    ));
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"first")]))
    );
    // Announced later, it is what is read back, and a change: the agent
    // took it for not written.
    hold(&sway, false);
    wait_until("the late write to be the selection", || {
        clipboard.read(1 << 20).ok()
            == Some(ClipboardRead::Content(content(&[(
                ClipboardFormat::Text,
                b"second",
            )])))
    });
    assert_ne!(clipboard.change_count().unwrap(), count);
    assert_eq!(wl_paste(UTF8).unwrap(), b"second");

    // A promise given up on is ended at once, and withdrawn once the
    // compositor announces it: nothing of it is offered or fetched.
    let count = clipboard.change_count().unwrap();
    let source = Arc::new(Source {
        content: content(&[(ClipboardFormat::Text, b"promised")]),
        ..Source::default()
    });
    hold(&sway, true);
    assert!(matches!(
        clipboard.write_promise(promise(&source, &[ClipboardFormat::Text], &[])),
        Err(PlatformError::Unavailable(_))
    ));
    assert!(source.finished.load(Ordering::SeqCst));
    hold(&sway, false);
    wait_until("the late promise to be withdrawn", || {
        clipboard.change_count().is_ok_and(|now| now != count) && wl_types().is_empty()
    });
    assert_eq!(wl_paste(UTF8), None);
    assert!(source.fetched.lock().unwrap().is_empty());
    assert_eq!(clipboard.read(1 << 20).unwrap(), ClipboardRead::Empty);
    // One change: the withdrawal's own clear is none.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(clipboard.change_count().unwrap(), count + 1);
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_a_withdrawal_leaves_a_newer_write_alone() {
    let sway = sway();
    let mut clipboard = connected();
    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, b"first")]))
        .unwrap();
    // A promise and then a write, both past their wait while the
    // compositor is held up.
    hold(&sway, true);
    let source = Arc::new(Source {
        content: content(&[(ClipboardFormat::Text, b"promised")]),
        ..Source::default()
    });
    assert!(
        clipboard
            .write_promise(promise(&source, &[ClipboardFormat::Text], &[]))
            .is_err()
    );
    assert!(
        clipboard
            .write(&content(&[(ClipboardFormat::Text, b"newer")]))
            .is_err()
    );
    // Announced in turn: the promise is not withdrawn by a clear that
    // would come after the newer write, which stays.
    hold(&sway, false);
    wait_until("the newer write to be the selection", || {
        clipboard.read(1 << 20).ok()
            == Some(ClipboardRead::Content(content(&[(
                ClipboardFormat::Text,
                b"newer",
            )])))
    });
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(wl_paste(UTF8).unwrap(), b"newer");
    assert!(source.fetched.lock().unwrap().is_empty());
    // Each was a change, the agent having taken it for not written.
    assert_eq!(clipboard.change_count().unwrap(), count + 2);
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_a_write_the_compositor_cuts_off_fails() {
    let sway = sway();
    let mut clipboard = connected();
    hold(&sway, true);
    let killing = {
        let pid = sway.child.id() as libc::pid_t;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            // SAFETY: a signal to the test's own child.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        })
    };
    // Lost while it waited: a failure, not something copied over it.
    let written = clipboard.write(&content(&[(ClipboardFormat::Text, b"cut off")]));
    killing.join().unwrap();
    assert!(matches!(written, Err(PlatformError::Os(_))), "{written:?}");
    drop(sway);
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_a_managers_copy_keeps_a_promise_of_files() {
    let _sway = sway();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a b.txt");
    std::fs::write(&file, b"x").unwrap();
    let mut clipboard = connected();
    let source = Arc::new(Source {
        files: Mutex::new(Some(FilesFetch::Ready(vec![Some(file.clone())]))),
        ..Source::default()
    });
    let count = clipboard
        .write_promise(promise(
            &source,
            &[],
            &[PromisedFile {
                name: "a b.txt".into(),
                folder: false,
            }],
        ))
        .unwrap();
    // As wl-clip-persist does: every MIME type read at once, then offered
    // again from its own copy, leaving out what holds nothing.
    let items: Vec<(String, Vec<u8>)> = wl_types()
        .into_iter()
        .map(|mime| {
            let data = wl_paste(&mime).unwrap_or_default();
            (mime, data)
        })
        .filter(|(_, data)| !data.is_empty())
        .collect();
    assert!(
        items
            .iter()
            .any(|(m, _)| m.starts_with("application/x-inputweave-source."))
    );
    assert_eq!(*source.files_asked.lock().unwrap(), 1);
    let offered: Vec<(&str, &[u8])> = items
        .iter()
        .map(|(m, d)| (m.as_str(), d.as_slice()))
        .collect();
    let _manager = Peer::own(&offered);
    assert_eq!(clipboard.change_count().unwrap(), count);
    // The promise holds while that copy is the selection, so the files
    // pasted from it stay where they are.
    std::thread::sleep(Duration::from_millis(200));
    assert!(!source.finished.load(Ordering::SeqCst));
    let list = String::from_utf8(wl_paste("text/uri-list").unwrap()).unwrap();
    assert_eq!(list.replace("\r\n", "\n").trim_end(), uris(&[&file]));
    // Another app's copy ends it.
    let _copy = wl_copy("text/plain", b"theirs");
    changed_from(&clipboard, count);
    wait_until("the promise to be finished", || {
        source.finished.load(Ordering::SeqCst)
    });
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_a_read_another_copy_cuts_into_fails() {
    let _sway = sway();
    let clipboard = connected();
    let count = clipboard.change_count().unwrap();
    // Its text takes a while; another app copies meanwhile, before the
    // HTML is asked for.
    let _peer = Peer::slowly(
        &[(UTF8, b"text"), ("text/html", b"<b>text</b>")],
        Duration::from_millis(800),
    );
    changed_from(&clipboard, count);
    let reading = std::thread::spawn(move || {
        let read = clipboard.read(1 << 20);
        (clipboard, read)
    });
    std::thread::sleep(Duration::from_millis(300));
    let _copy = wl_copy("text/plain", b"theirs");
    let (clipboard, read) = reading.join().unwrap();
    assert!(
        matches!(read, Err(PlatformError::Unavailable(_))),
        "a part read as the whole: {read:?}"
    );
    // Read again, it is the new copy.
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"theirs")]))
    );
}

/// Whether a Wayland client can talk to the compositor now.
fn compositor_up() -> bool {
    struct Nothing;
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Nothing {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    Connection::connect_to_env()
        .ok()
        .is_some_and(|conn| registry_queue_init::<Nothing>(&conn).is_ok())
}

#[test]
#[ignore = "needs sway; scripts/linux-wayland-tests.sh"]
fn data_control_comes_back_after_the_compositor_restarts() {
    let sway = sway();
    let mut clipboard = connected();
    clipboard
        .write(&content(&[(ClipboardFormat::Text, b"before")]))
        .unwrap();
    let count = clipboard.change_count().unwrap();
    let runtime = sway.runtime.clone();
    drop(sway);
    // Gone, the compositor took what was written with it.
    wait_until("the clipboard to hear the compositor went", || {
        clipboard.change_count().ok() != Some(count)
    });
    assert!(matches!(
        clipboard.write(&content(&[(ClipboardFormat::Text, b"nowhere")])),
        Err(PlatformError::Unavailable(_) | PlatformError::Os(_))
    ));

    let mut sway = Sway::start_in(runtime);
    use_sway(&sway);
    wait_until("the new compositor", compositor_up);
    let copy = wl_copy("text/plain", b"after");
    wait_until("another app's copy on the new compositor", || {
        clipboard.read(1 << 20).ok()
            == Some(ClipboardRead::Content(content(&[(
                ClipboardFormat::Text,
                b"after",
            )])))
    });
    drop(copy);
    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, b"again")]))
        .unwrap();
    assert_eq!(wl_paste(UTF8).unwrap(), b"again");
    assert_eq!(clipboard.change_count().unwrap(), count);

    // Gone and back at once, a write as soon as it is connected again: the
    // old connection let go of what it held before, not of the new one's.
    for round in 0..3 {
        let runtime = sway.runtime.clone();
        drop(sway);
        sway = Sway::start_in(runtime);
        use_sway(&sway);
        let text = format!("round {round}");
        let mut clipboard_write =
            || clipboard.write(&content(&[(ClipboardFormat::Text, text.as_bytes())]));
        wait_until("a write on the new compositor", || {
            clipboard_write().is_ok()
        });
        assert_eq!(wl_paste(UTF8).unwrap(), text.as_bytes());
        assert_eq!(
            clipboard.read(1 << 20).unwrap(),
            ClipboardRead::Content(content(&[(ClipboardFormat::Text, text.as_bytes())]))
        );
    }
}

// ---------------------------------------------------------------------
// The Clipboard portal, with a remote desktop session.

const PATH: &str = "/org/freedesktop/portal/desktop";

/// What the fake Clipboard portal was asked, and holds.
#[derive(Default)]
struct Board {
    /// The MIME types each `SetSelection` offered.
    selections: Vec<Vec<String>>,
    /// Another app's content, by MIME type, for `SelectionRead`.
    theirs: HashMap<String, Vec<u8>>,
    /// The MIME types `SelectionRead` was asked for.
    read: Vec<String>,
    /// What the session wrote for each serial, once it closed the pipe.
    written: HashMap<u32, Vec<u8>>,
    /// `SelectionWriteDone`'s serials and successes.
    done: Vec<(u32, bool)>,
    /// `SetSelection` does not say the session owns the clipboard.
    quiet: bool,
    /// `SetSelection` says, before it answers, that another app copied.
    copied_before: Option<Vec<(String, Vec<u8>)>>,
    /// `SetSelection` says, a moment after it answered, that another app
    /// copied.
    copied_after: Option<Vec<(String, Vec<u8>)>>,
    /// `SetSelection` first says an app pastes this MIME type, to answer
    /// with this serial, as for what was there before.
    pasted_first: Option<(String, u32)>,
    /// How long `SetSelection` takes, once it said what it says first.
    slow: Option<Duration>,
    /// `SetSelection` fails.
    refuse: bool,
    /// `SetSelection` first says the session owns the clipboard, as a late
    /// word on an earlier write.
    owned_first: bool,
    /// The unique name of the connection that asked for the clipboard.
    client: Option<String>,
    /// `RequestClipboard` fails.
    refuse_request: bool,
    /// `RequestClipboard` never answers this many more times.
    unanswered: usize,
    /// The next `RequestClipboard` takes the request at once and answers
    /// this late.
    answer_late: Option<Duration>,
}

struct FakeClipboard {
    asked: Arc<Mutex<Asked>>,
    board: Arc<Mutex<Board>>,
}

fn owner(mimes: &[String], ours: bool) -> HashMap<&'static str, Variant<'static>> {
    let mut options = HashMap::from([("mime_types", Variant::from(mimes.to_vec()))]);
    if ours {
        options.insert("session_is_owner", Variant::from(true));
    }
    options
}

/// Another app's copy of `items`, as the desktop tells `session`.
async fn copied(
    conn: &zbus::Connection,
    board: &Mutex<Board>,
    session: &OwnedObjectPath,
    items: Vec<(String, Vec<u8>)>,
) -> zbus::Result<()> {
    let mimes: Vec<String> = items.iter().map(|(m, _)| m.clone()).collect();
    board.lock().unwrap().theirs = items.into_iter().collect();
    let emitter = SignalEmitter::new(conn, PATH)?;
    FakeClipboard::selection_owner_changed(&emitter, session.as_ref(), owner(&mimes, false)).await
}

#[zbus::interface(name = "org.freedesktop.portal.Clipboard")]
impl FakeClipboard {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }

    async fn request_clipboard(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        _session: ObjectPath<'_>,
        _options: HashMap<String, OwnedVariant>,
    ) -> zbus::fdo::Result<()> {
        let (unanswered, refused, late) = {
            let mut board = self.board.lock().unwrap();
            board.client = header.sender().map(|s| s.to_string());
            let unanswered = board.unanswered > 0;
            if unanswered {
                board.unanswered -= 1;
            }
            (unanswered, board.refuse_request, board.answer_late.take())
        };
        self.asked.lock().unwrap().calls.push("RequestClipboard");
        if unanswered {
            std::future::pending::<()>().await;
        }
        if refused {
            return Err(zbus::fdo::Error::Failed("no clipboard".to_owned()));
        }
        {
            let mut asked = self.asked.lock().unwrap();
            // As xdg-desktop-portal answers a session asking twice.
            if asked.clipboard_requested {
                return Err(zbus::fdo::Error::Failed(
                    "Clipboard access already requested".to_owned(),
                ));
            }
            asked.clipboard_requested = true;
        }
        if let Some(late) = late {
            tokio::time::sleep(late).await;
        }
        Ok(())
    }

    async fn set_selection(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        session: ObjectPath<'_>,
        options: HashMap<String, OwnedVariant>,
    ) -> zbus::fdo::Result<()> {
        let mimes: Vec<String> = options
            .get("mime_types")
            .and_then(|v| v.downcast_ref::<zbus::Array<'_>>().ok())
            .map(|a| {
                a.iter()
                    .filter_map(|m| m.downcast_ref::<&str>().ok().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let session: OwnedObjectPath = session.into();
        let (quiet, before, after, pasted, slow, refuse, owned_first) = {
            let mut board = self.board.lock().unwrap();
            board.selections.push(mimes);
            (
                board.quiet,
                board.copied_before.take(),
                board.copied_after.take(),
                board.pasted_first.take(),
                board.slow.take(),
                std::mem::take(&mut board.refuse),
                std::mem::take(&mut board.owned_first),
            )
        };
        if owned_first {
            let emitter = SignalEmitter::new(conn, PATH)?;
            Self::selection_owner_changed(&emitter, session.as_ref(), owner(&[], true)).await?;
        }
        if let Some((mime, serial)) = pasted {
            let emitter = SignalEmitter::new(conn, PATH)?;
            Self::selection_transfer(&emitter, session.as_ref(), &mime, serial).await?;
        }
        if let Some(items) = before {
            copied(conn, &self.board, &session, items).await?;
        }
        if let Some(slow) = slow {
            tokio::time::sleep(slow).await;
        }
        if refuse {
            return Err(zbus::fdo::Error::Failed("not now".to_owned()));
        }
        if !quiet {
            // As mutter says it, before the call returns.
            let emitter = SignalEmitter::new(conn, PATH)?;
            Self::selection_owner_changed(&emitter, session.as_ref(), owner(&[], true)).await?;
        }
        if let Some(items) = after {
            let (conn, board) = (conn.clone(), self.board.clone());
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                copied(&conn, &board, &session, items).await.unwrap();
            });
        }
        Ok(())
    }

    async fn selection_write(&self, _session: ObjectPath<'_>, serial: u32) -> zbus::OwnedFd {
        let (reader, writer) = std::io::pipe().unwrap();
        let board = self.board.clone();
        std::thread::spawn(move || {
            let mut data = Vec::new();
            let _ = std::fs::File::from(OwnedFd::from(reader)).read_to_end(&mut data);
            board.lock().unwrap().written.insert(serial, data);
        });
        OwnedFd::from(writer).into()
    }

    async fn selection_write_done(&self, _session: ObjectPath<'_>, serial: u32, success: bool) {
        self.board.lock().unwrap().done.push((serial, success));
    }

    async fn selection_read(
        &self,
        _session: ObjectPath<'_>,
        mime_type: &str,
    ) -> zbus::fdo::Result<zbus::OwnedFd> {
        let data = {
            let mut board = self.board.lock().unwrap();
            board.read.push(mime_type.to_owned());
            board.theirs.get(mime_type).cloned()
        };
        let data = data.ok_or_else(|| zbus::fdo::Error::Failed("nothing of it".to_owned()))?;
        let (reader, writer) = std::io::pipe().unwrap();
        std::thread::spawn(move || {
            let _ = std::fs::File::from(OwnedFd::from(writer)).write_all(&data);
        });
        Ok(OwnedFd::from(reader).into())
    }

    #[zbus(signal)]
    async fn selection_owner_changed(
        emitter: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        options: HashMap<&str, Variant<'_>>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn selection_transfer(
        emitter: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        mime_type: &str,
        serial: u32,
    ) -> zbus::Result<()>;
}

impl Desktop {
    /// The Clipboard portal beside the RemoteDesktop one.
    fn with_clipboard(&self) -> Arc<Mutex<Board>> {
        let board = Arc::new(Mutex::new(Board::default()));
        let clipboard = FakeClipboard {
            asked: self.asked.clone(),
            board: board.clone(),
        };
        self.runtime
            .block_on(async { self.conn.object_server().at(PATH, clipboard).await.unwrap() });
        board
    }

    /// Remote control, sharing `clipboard`, which the desktop has
    /// `CLIPBOARD_WAIT` to answer.
    fn remote_sharing(&self, clipboard: &PortalClipboard, tokens: &Path) -> Arc<Remote> {
        self.remote_with(clipboard, tokens, 3)
    }

    /// As `remote_sharing`, with `retries` tries after a failed start.
    fn remote_with(
        &self,
        clipboard: &PortalClipboard,
        tokens: &Path,
        retries: usize,
    ) -> Arc<Remote> {
        Arc::new(
            Remote::start(RemoteOptions {
                clipboard: Some(clipboard.portal()),
                clipboard_wait: CLIPBOARD_WAIT,
                ..RemoteOptions::with(Lifecycle {
                    bus: Some(self.bus.address.clone()),
                    retries: vec![Duration::from_millis(100); retries],
                    ..Lifecycle::new(Tokens::in_dir(Some(tokens)), super::super::APP_ID)
                })
            })
            .unwrap(),
        )
    }

    fn last_session(&self) -> OwnedObjectPath {
        self.asked.lock().unwrap().sessions.last().unwrap().clone()
    }

    /// Another app copies `items` (MIME type, bytes).
    fn copied(&self, board: &Mutex<Board>, items: &[(&str, &[u8])]) {
        let items = items
            .iter()
            .map(|(m, d)| ((*m).to_owned(), d.to_vec()))
            .collect();
        let session = self.last_session();
        self.runtime
            .block_on(copied(&self.conn, board, &session, items))
            .unwrap();
    }

    /// An app pastes `mime`, with `serial` to answer.
    fn paste(&self, mime: &str, serial: u32) {
        let session = self.last_session();
        self.runtime.block_on(async {
            let emitter = SignalEmitter::new(&self.conn, PATH).unwrap();
            FakeClipboard::selection_transfer(&emitter, session.as_ref(), mime, serial)
                .await
                .unwrap();
        });
    }
}

/// What the session answered the paste `serial` with, once it said how
/// that went.
fn pasted(board: &Mutex<Board>, serial: u32) -> Option<Vec<u8>> {
    wait_until(&format!("the paste {serial} to be answered"), || {
        let board = board.lock().unwrap();
        board.done.iter().any(|(s, _)| *s == serial) && board.written.contains_key(&serial)
    });
    let board = board.lock().unwrap();
    let ok = board.done.contains(&(serial, true));
    ok.then(|| board.written[&serial].clone())
}

fn running(desktop: &Desktop, clipboard: &PortalClipboard) -> (Arc<Remote>, tempfile::TempDir) {
    let state = tempfile::tempdir().unwrap();
    let remote = desktop.remote_sharing(clipboard, state.path());
    remote.ask().unwrap();
    wait_until("the session to run", || remote.portal() == Portal::Running);
    (remote, state)
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn the_clipboard_portal_is_shared_with_remote_control() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    let mut clipboard = PortalClipboard::new().not_knowing_for(Duration::from_secs(2));
    let state = tempfile::tempdir().unwrap();
    let remote = desktop.remote_sharing(&clipboard, state.path());
    let status = WaylandStatus::new(Reaches::Portal(remote.clone()));

    // Until remote control runs there is none.
    assert!(matches!(
        clipboard.read(1 << 20),
        Err(PlatformError::Unavailable(_))
    ));
    assert!(matches!(
        clipboard.write(&content(&[(ClipboardFormat::Text, b"x")])),
        Err(PlatformError::Unavailable(_))
    ));
    let before = clipboard.change_count().unwrap();
    status
        .request_permission(Permission::RemoteControl)
        .unwrap();
    wait_until("the session to run", || remote.portal() == Portal::Running);
    assert_eq!(
        desktop.calls(),
        [
            "CreateSession",
            "RequestClipboard",
            "SelectDevices",
            "Start",
            "ConnectToEIS"
        ]
    );
    // Until the desktop says who owns the clipboard, what is there is not
    // known: no change, and nothing read.
    assert_eq!(clipboard.change_count().unwrap(), before);
    assert!(matches!(
        clipboard.read(1 << 20),
        Err(PlatformError::Unavailable(_))
    ));
    // Said nothing a while (nobody owns it, or a desktop that says only
    // what changes), there is nothing: no change either.
    wait_until("nothing to be there", || {
        clipboard.read(1 << 20).ok() == Some(ClipboardRead::Empty)
    });
    assert_eq!(clipboard.change_count().unwrap(), before);
    assert_eq!(clipboard.files().unwrap(), Vec::<PathBuf>::new());

    // Another app copies.
    let count = clipboard.change_count().unwrap();
    desktop.copied(
        &board,
        &[
            ("text/plain", b"plain"),
            (UTF8, b"theirs"),
            ("text/html", b"<b>theirs</b>"),
        ],
    );
    changed_from(&clipboard, count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[
            (ClipboardFormat::Text, b"theirs"),
            (ClipboardFormat::Html, b"<b>theirs</b>"),
        ]))
    );
    assert_eq!(board.lock().unwrap().read, [UTF8, "text/html"]);
    assert_eq!(clipboard.files().unwrap(), Vec::<PathBuf>::new());

    // Written here: offered, not a copy, and read back as written.
    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, "café".as_bytes())]))
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(clipboard.change_count().unwrap(), count);
    let offered = board.lock().unwrap().selections.last().unwrap().clone();
    for mime in [UTF8, "text/plain", "UTF8_STRING"] {
        assert!(offered.iter().any(|m| m == mime), "{mime} in {offered:?}");
    }
    for mime in ["text/html", "STRING", "TEXT"] {
        assert!(!offered.iter().any(|m| m == mime), "{mime} in {offered:?}");
    }
    let read = board.lock().unwrap().read.len();
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, "café".as_bytes())]))
    );
    assert_eq!(
        board.lock().unwrap().read.len(),
        read,
        "not asked of the desktop"
    );
    // Apps paste it through the desktop.
    desktop.paste(UTF8, 7);
    assert_eq!(pasted(&board, 7).unwrap(), "café".as_bytes());
    desktop.paste("text/plain", 8);
    assert_eq!(pasted(&board, 8).unwrap(), "café".as_bytes());
    desktop.paste("image/png", 9);
    assert_eq!(pasted(&board, 9), None);

    // A promise: fetched as it is pasted.
    let source = Arc::new(Source {
        content: content(&[(ClipboardFormat::Text, b"promised")]),
        ..Source::default()
    });
    let count = clipboard
        .write_promise(promise(&source, &[ClipboardFormat::Text], &[]))
        .unwrap();
    assert_eq!(clipboard.change_count().unwrap(), count);
    assert_eq!(clipboard.read(1 << 20).unwrap(), ClipboardRead::Empty);
    assert!(source.fetched.lock().unwrap().is_empty());
    desktop.paste(UTF8, 10);
    assert_eq!(pasted(&board, 10).unwrap(), b"promised");
    assert_eq!(*source.fetched.lock().unwrap(), [ClipboardFormat::Text]);
    // Another app's copy finishes it.
    desktop.copied(&board, &[(UTF8, b"later")]);
    changed_from(&clipboard, count);
    wait_until("the promise to be finished", || {
        source.finished.load(Ordering::SeqCst)
    });

    // Promised files: names and the hint at once, the files as pasted.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a b.txt");
    let source = Arc::new(Source {
        files: Mutex::new(Some(FilesFetch::Ready(vec![Some(file.clone()), None]))),
        ..Source::default()
    });
    clipboard
        .write_promise(promise(&source, &[], &two_files()))
        .unwrap();
    let offered = board.lock().unwrap().selections.last().unwrap().clone();
    for mime in [
        "text/uri-list",
        "x-special/gnome-copied-files",
        "x-kde-passwordManagerHint",
    ] {
        assert!(offered.iter().any(|m| m == mime), "{mime} in {offered:?}");
    }
    desktop.paste("x-kde-passwordManagerHint", 11);
    assert_eq!(pasted(&board, 11).unwrap(), b"secret");
    desktop.paste(UTF8, 12);
    assert_eq!(pasted(&board, 12).unwrap(), b"a b.txt\nPhotos");
    assert_eq!(*source.files_asked.lock().unwrap(), 0);
    desktop.paste("x-special/gnome-copied-files", 13);
    assert_eq!(
        String::from_utf8(pasted(&board, 13).unwrap()).unwrap(),
        format!("copy\n{}", uris(&[&file]))
    );
    assert_eq!(*source.files_asked.lock().unwrap(), 1);

    // The desktop ends the session: nothing from here is on the
    // clipboard, and there is none until the next one.
    desktop.close_last_session();
    wait_until("the session to end", || remote.portal() == Portal::Stopped);
    wait_until("the promise to be finished", || {
        source.finished.load(Ordering::SeqCst)
    });
    assert!(matches!(
        clipboard.read(1 << 20),
        Err(PlatformError::Unavailable(_))
    ));
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn the_clipboard_portal_marks_and_lists_what_apps_copied() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    let clipboard = PortalClipboard::new();
    let (_remote, _state) = running(&desktop, &clipboard);

    let count = clipboard.change_count().unwrap();
    desktop.copied(
        &board,
        &[(UTF8, b"hunter2"), ("x-kde-passwordManagerHint", b"secret")],
    );
    changed_from(&clipboard, count);
    assert_eq!(clipboard.read(1 << 20).unwrap(), ClipboardRead::Sensitive);
    assert_eq!(clipboard.files().unwrap(), Vec::<PathBuf>::new());
    assert!(!board.lock().unwrap().read.iter().any(|m| m == UTF8));

    let count = clipboard.change_count().unwrap();
    desktop.copied(
        &board,
        &[
            ("x-special/gnome-copied-files", b"copy\nfile:///tmp/a%20b"),
            ("text/uri-list", b"file:///tmp/a%20b\r\n"),
            (UTF8, b"a b"),
        ],
    );
    changed_from(&clipboard, count);
    assert_eq!(clipboard.files().unwrap(), [PathBuf::from("/tmp/a b")]);

    // Past the limit.
    let large = vec![b'z'; 300 * 1024];
    desktop.copied(&board, &[(UTF8, &large)]);
    wait_until(
        "the large copy",
        || matches!(clipboard.read(1000), Ok(ClipboardRead::TooLarge(n)) if n > 1000),
    );
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, &large)]))
    );
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn the_clipboard_portal_counts_copies_around_a_write_in_order() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    let mut clipboard = PortalClipboard::new();
    let (_remote, _state) = running(&desktop, &clipboard);

    // The desktop says nothing of the owner: taken as it answers.
    board.lock().unwrap().quiet = true;
    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, b"mine")]))
        .unwrap();
    assert_eq!(clipboard.change_count().unwrap(), count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"mine")]))
    );

    // Another app's copy the desktop heard of before the write: it came
    // first, and the write is what is on the clipboard.
    board.lock().unwrap().copied_before = Some(vec![(UTF8.to_owned(), b"before".to_vec())]);
    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, b"second")]))
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(clipboard.change_count().unwrap(), count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"second")]))
    );

    // One heard after the desktop answered: the write took the clipboard,
    // and the copy came after it.
    let source = Arc::new(Source::default());
    board.lock().unwrap().copied_after = Some(vec![(UTF8.to_owned(), b"after".to_vec())]);
    let count = clipboard
        .write_promise(promise(&source, &[ClipboardFormat::Text], &[]))
        .unwrap();
    changed_from(&clipboard, count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"after")]))
    );
    assert!(source.finished.load(Ordering::SeqCst));
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn the_clipboard_is_asked_for_with_every_session() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    let mut clipboard = PortalClipboard::new().not_knowing_for(Duration::from_secs(5));
    let (remote, _state) = running(&desktop, &clipboard);
    let first = desktop.last_session();
    clipboard
        .write(&content(&[(ClipboardFormat::Text, b"mine")]))
        .unwrap();

    // The compositor goes away; the session starts again with its token,
    // and asks for the clipboard again before it starts.
    desktop.end_libei();
    wait_until("the session to run again", || {
        desktop.asked.lock().unwrap().sessions.len() == 2 && remote.portal() == Portal::Running
    });
    let calls = desktop.calls();
    let requests = calls.iter().filter(|c| **c == "RequestClipboard").count();
    assert_eq!(requests, 2, "{calls:?}");
    let starts: Vec<usize> = calls
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == "Start")
        .map(|(i, _)| i)
        .collect();
    let asked: Vec<usize> = calls
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == "RequestClipboard")
        .map(|(i, _)| i)
        .collect();
    assert!(asked[1] < starts[1], "{calls:?}");
    // Nothing from the old session is on the clipboard now, and what is
    // there is not known until the desktop says.
    assert!(matches!(
        clipboard.read(1 << 20),
        Err(PlatformError::Unavailable(_))
    ));

    // What the desktop says of the old session is not this one's.
    let count = clipboard.change_count().unwrap();
    desktop.runtime.block_on(async {
        let emitter = SignalEmitter::new(&desktop.conn, PATH).unwrap();
        FakeClipboard::selection_owner_changed(
            &emitter,
            first.as_ref(),
            owner(&[UTF8.to_owned()], false),
        )
        .await
        .unwrap();
    });
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(clipboard.change_count().unwrap(), count);
    desktop.copied(&board, &[(UTF8, b"new")]);
    changed_from(&clipboard, count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"new")]))
    );
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn remote_control_runs_without_a_clipboard_the_desktop_does_not_share() {
    // A desktop that does not share it.
    let desktop = Desktop::start();
    desktop.with_clipboard();
    desktop.asked.lock().unwrap().clipboard_refused = true;
    let clipboard = PortalClipboard::new();
    let (_remote, _state) = running(&desktop, &clipboard);
    assert!(desktop.calls().contains(&"RequestClipboard"));
    assert!(matches!(
        clipboard.read(1 << 20),
        Err(PlatformError::Unavailable(_))
    ));
    drop(_remote);

    // A desktop with no Clipboard portal at all.
    let desktop = Desktop::start();
    let clipboard = PortalClipboard::new();
    let (_remote, _state) = running(&desktop, &clipboard);
    assert!(!desktop.calls().contains(&"RequestClipboard"));
    assert!(matches!(
        clipboard.read(1 << 20),
        Err(PlatformError::Unavailable(_))
    ));
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn the_clipboard_portal_answers_a_paste_with_what_the_desktop_had() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    let mut clipboard = PortalClipboard::new();
    let (_remote, _state) = running(&desktop, &clipboard);
    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, b"first")]))
        .unwrap();

    // An app pastes as the desktop takes the next write, before it does:
    // it gets what was there.
    let source = Arc::new(Source {
        content: content(&[(ClipboardFormat::Text, b"second")]),
        ..Source::default()
    });
    {
        let mut board = board.lock().unwrap();
        board.pasted_first = Some((UTF8.to_owned(), 31));
        board.slow = Some(Duration::from_millis(300));
    }
    let written = clipboard
        .write_promise(promise(&source, &[ClipboardFormat::Text], &[]))
        .unwrap();
    assert_eq!(written, count + 1);
    let count = written;
    assert_eq!(pasted(&board, 31).unwrap(), b"first");
    assert!(source.fetched.lock().unwrap().is_empty());
    // After, it gets the write.
    desktop.paste(UTF8, 32);
    assert_eq!(pasted(&board, 32).unwrap(), b"second");

    // Refused, nothing changed: what was there is still this session's.
    let refused = Arc::new(Source::default());
    board.lock().unwrap().refuse = true;
    assert!(matches!(
        clipboard.write_promise(promise(&refused, &[ClipboardFormat::Text], &[])),
        Err(PlatformError::Os(_))
    ));
    assert!(refused.finished.load(Ordering::SeqCst));
    assert!(!source.finished.load(Ordering::SeqCst));
    assert_eq!(clipboard.change_count().unwrap(), count);
    desktop.paste(UTF8, 33);
    assert_eq!(pasted(&board, 33).unwrap(), b"second");
    assert_eq!(clipboard.read(1 << 20).unwrap(), ClipboardRead::Empty);
    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, b"third")]))
        .unwrap();
    assert!(source.finished.load(Ordering::SeqCst));
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"third")]))
    );
    assert_eq!(clipboard.change_count().unwrap(), count);
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_write_the_desktop_takes_too_late_is_a_change() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    let mut clipboard = PortalClipboard::new();
    let (_remote, _state) = running(&desktop, &clipboard);
    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, b"before")]))
        .unwrap();
    // The desktop takes it, and says so, after the write stopped waiting.
    board.lock().unwrap().slow = Some(Duration::from_secs(3));
    let started = Instant::now();
    assert!(
        clipboard
            .write(&content(&[(ClipboardFormat::Text, b"late")]))
            .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    // Until it says, what is there is not known: an app pasting gets
    // nothing, not what may no longer be there.
    desktop.paste(UTF8, 61);
    assert_eq!(pasted(&board, 61), None);
    assert!(matches!(
        clipboard.read(1 << 20),
        Err(PlatformError::Unavailable(_))
    ));
    // Said, the late write is what is there, and a change: the agent took
    // it for not written.
    wait_until("the late write to be what is there", || {
        clipboard.read(1 << 20).ok()
            == Some(ClipboardRead::Content(content(&[(
                ClipboardFormat::Text,
                b"late",
            )])))
    });
    assert_eq!(clipboard.change_count().unwrap(), count + 1);
    desktop.paste(UTF8, 62);
    assert_eq!(pasted(&board, 62).unwrap(), b"late");
    // The next write goes as usual.
    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, b"next")]))
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(clipboard.change_count().unwrap(), count);
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn signals_only_the_portal_sends_are_heard() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    let clipboard = PortalClipboard::new();
    let (_remote, _state) = running(&desktop, &clipboard);
    let session = desktop.last_session();
    let client = board.lock().unwrap().client.clone().unwrap();
    let count = clipboard.change_count().unwrap();
    // Another app on the bus, to everyone and to the agent alone.
    desktop.runtime.block_on(async {
        let spoof = zbus::connection::Builder::address(desktop.bus.address.as_str())
            .build()
            .await
            .unwrap();
        let body = (session.as_ref(), owner(&[UTF8.to_owned()], false));
        for to in [None, Some(client.as_str())] {
            spoof
                .emit_signal(
                    to,
                    PATH,
                    "org.freedesktop.portal.Clipboard",
                    "SelectionOwnerChanged",
                    &body,
                )
                .await
                .unwrap();
            spoof
                .emit_signal(
                    to,
                    PATH,
                    "org.freedesktop.portal.Clipboard",
                    "SelectionTransfer",
                    &(session.as_ref(), UTF8, 41u32),
                )
                .await
                .unwrap();
        }
    });
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(clipboard.change_count().unwrap(), count);
    assert!(board.lock().unwrap().written.is_empty());
    // The portal's own are.
    desktop.copied(&board, &[(UTF8, b"real")]);
    changed_from(&clipboard, count);
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_word_on_an_earlier_write_does_not_take_the_next() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    let mut clipboard = PortalClipboard::new();
    let (_remote, _state) = running(&desktop, &clipboard);
    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, b"first")]))
        .unwrap();
    // The desktop says the session owns the clipboard (of the first), then
    // refuses the second: nothing changed.
    let refused = Arc::new(Source::default());
    {
        let mut board = board.lock().unwrap();
        board.owned_first = true;
        board.refuse = true;
    }
    assert!(matches!(
        clipboard.write_promise(promise(&refused, &[ClipboardFormat::Text], &[])),
        Err(PlatformError::Os(_))
    ));
    assert!(refused.finished.load(Ordering::SeqCst));
    assert_eq!(clipboard.change_count().unwrap(), count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"first")]))
    );
    desktop.paste(UTF8, 51);
    assert_eq!(pasted(&board, 51).unwrap(), b"first");
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_session_restored_asks_for_the_clipboard_as_its_token_was_made() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    // The clipboard request fails: the token is of a session without it.
    board.lock().unwrap().refuse_request = true;
    let clipboard = PortalClipboard::new();
    let (remote, state) = running(&desktop, &clipboard);
    assert_eq!(
        Tokens::in_dir(Some(state.path()))
            .load("remote_desktop_clipboard")
            .as_deref(),
        Some("no")
    );
    // Restored with that token, it does not ask either, though it could now.
    board.lock().unwrap().refuse_request = false;
    desktop.end_libei();
    wait_until("the session to run again", || {
        desktop.asked.lock().unwrap().sessions.len() == 2 && remote.portal() == Portal::Running
    });
    let requests = || {
        desktop
            .calls()
            .iter()
            .filter(|c| **c == "RequestClipboard")
            .count()
    };
    assert_eq!(requests(), 1);
    assert!(matches!(
        clipboard.read(1 << 20),
        Err(PlatformError::Unavailable(_))
    ));

    // A session the user allows anew asks, and its token says so.
    desktop.close_last_session();
    wait_until("the session to end", || remote.portal() == Portal::Stopped);
    remote.ask().unwrap();
    wait_until("the session to run anew", || {
        desktop.asked.lock().unwrap().sessions.len() == 3 && remote.portal() == Portal::Running
    });
    assert_eq!(requests(), 2);
    assert_eq!(
        Tokens::in_dir(Some(state.path()))
            .load("remote_desktop_clipboard")
            .as_deref(),
        Some("yes")
    );
    desktop.end_libei();
    wait_until("the session to run again with the clipboard", || {
        desktop.asked.lock().unwrap().sessions.len() == 4 && remote.portal() == Portal::Running
    });
    assert_eq!(requests(), 3);
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_promise_the_desktop_takes_too_late_is_withdrawn() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    let mut clipboard = PortalClipboard::new();
    let (_remote, _state) = running(&desktop, &clipboard);
    clipboard
        .write(&content(&[(ClipboardFormat::Text, b"before")]))
        .unwrap();
    let count = clipboard.change_count().unwrap();
    let source = Arc::new(Source {
        content: content(&[(ClipboardFormat::Text, b"promised")]),
        ..Source::default()
    });
    board.lock().unwrap().slow = Some(Duration::from_secs(3));
    assert!(matches!(
        clipboard.write_promise(promise(&source, &[ClipboardFormat::Text], &[])),
        Err(PlatformError::Unavailable(_))
    ));
    // Given up on, it is ended: the agent lets its offer go.
    assert!(source.finished.load(Ordering::SeqCst));
    // Taken late: a change, withdrawn (offered with nothing), and nothing
    // of it given.
    wait_until("the late promise to be withdrawn", || {
        board
            .lock()
            .unwrap()
            .selections
            .last()
            .is_some_and(Vec::is_empty)
    });
    // The withdrawal taken as well: one change in all.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(clipboard.change_count().unwrap(), count + 1);
    desktop.paste(UTF8, 71);
    assert_eq!(pasted(&board, 71), None);
    assert!(source.fetched.lock().unwrap().is_empty());
    wait_until("nothing to be read", || {
        clipboard.read(1 << 20).ok() == Some(ClipboardRead::Empty)
    });
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_token_without_the_clipboard_is_restored_without_asking() {
    // A token kept without saying the session had the clipboard.
    let desktop = Desktop::start();
    desktop.with_clipboard();
    let state = tempfile::tempdir().unwrap();
    Tokens::in_dir(Some(state.path()))
        .save("remote_desktop", Some("kept"))
        .unwrap();
    let clipboard = PortalClipboard::new();
    let remote = desktop.remote_sharing(&clipboard, state.path());
    wait_until("the session to run", || remote.portal() == Portal::Running);
    assert!(!desktop.calls().contains(&"RequestClipboard"));
    assert_eq!(
        desktop.asked.lock().unwrap().restore_tokens,
        [Some("kept".to_owned())]
    );
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_clipboard_request_not_answered_is_asked_again() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    // Not answered once: whether it reached the desktop is not known, so
    // the session asks again, and starts with the clipboard.
    board.lock().unwrap().unanswered = 1;
    let clipboard = PortalClipboard::new();
    let state = tempfile::tempdir().unwrap();
    let remote = desktop.remote_sharing(&clipboard, state.path());
    remote.ask().unwrap();
    running_within(&remote, Duration::from_secs(10));
    let calls = desktop.calls();
    assert_eq!(
        calls.iter().filter(|c| **c == "RequestClipboard").count(),
        2,
        "{calls:?}"
    );
    assert_eq!(
        calls.iter().filter(|c| **c == "CreateSession").count(),
        1,
        "{calls:?}"
    );
    assert_eq!(
        Tokens::in_dir(Some(state.path()))
            .load("remote_desktop_clipboard")
            .as_deref(),
        Some("yes")
    );
    desktop.copied(&board, &[(UTF8, b"there")]);
    wait_until("the clipboard", || {
        clipboard.read(1 << 20).ok()
            == Some(ClipboardRead::Content(content(&[(
                ClipboardFormat::Text,
                b"there",
            )])))
    });
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn remote_control_goes_on_when_the_clipboard_cannot_be_asked_for() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    // Not answered twice in a row: remote control starts without it, and
    // the token says so.
    board.lock().unwrap().unanswered = 2;
    let clipboard = PortalClipboard::new();
    let state = tempfile::tempdir().unwrap();
    let remote = desktop.remote_sharing(&clipboard, state.path());
    remote.ask().unwrap();
    running_within(&remote, Duration::from_secs(10));
    let calls = desktop.calls();
    assert_eq!(
        calls.iter().filter(|c| **c == "RequestClipboard").count(),
        2,
        "{calls:?}"
    );
    assert!(calls.contains(&"ConnectToEIS"), "{calls:?}");
    assert_eq!(
        Tokens::in_dir(Some(state.path()))
            .load("remote_desktop_clipboard")
            .as_deref(),
        Some("no")
    );
    assert!(matches!(
        clipboard.read(1 << 20),
        Err(PlatformError::Unavailable(_))
    ));
}

/// Waits for `remote` to run, for at most `wait`: requests not answered
/// take `CLIPBOARD_WAIT` each.
fn running_within(remote: &Remote, wait: Duration) {
    let deadline = Instant::now() + wait;
    while remote.portal() != Portal::Running {
        assert!(Instant::now() < deadline, "the session did not run");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_write_answered_after_it_stopped_waiting_is_taken_late() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    // Writes wait half a second; the desktop answers in one, within its
    // own time, and says nothing of the owner.
    let mut clipboard = PortalClipboard::new().waiting_at_most(Duration::from_millis(500));
    let (_remote, _state) = running(&desktop, &clipboard);
    let count = clipboard
        .write(&content(&[(ClipboardFormat::Text, b"before")]))
        .unwrap();
    {
        let mut board = board.lock().unwrap();
        board.quiet = true;
        board.slow = Some(Duration::from_secs(1));
    }
    assert!(
        clipboard
            .write(&content(&[(ClipboardFormat::Text, b"late")]))
            .is_err()
    );
    // Answered, it is what is there, and a change.
    wait_until("the late write to be what is there", || {
        clipboard.read(1 << 20).ok()
            == Some(ClipboardRead::Content(content(&[(
                ClipboardFormat::Text,
                b"late",
            )])))
    });
    assert_eq!(clipboard.change_count().unwrap(), count + 1);
    desktop.paste(UTF8, 81);
    assert_eq!(pasted(&board, 81).unwrap(), b"late");

    // A promise so: ended as the write stopped waiting, withdrawn once
    // answered, a change, and nothing of it given.
    let count = clipboard.change_count().unwrap();
    let source = Arc::new(Source {
        content: content(&[(ClipboardFormat::Text, b"promised")]),
        ..Source::default()
    });
    board.lock().unwrap().slow = Some(Duration::from_secs(1));
    assert!(
        clipboard
            .write_promise(promise(&source, &[ClipboardFormat::Text], &[]))
            .is_err()
    );
    assert!(source.finished.load(Ordering::SeqCst));
    wait_until("the late promise to be withdrawn", || {
        board
            .lock()
            .unwrap()
            .selections
            .last()
            .is_some_and(Vec::is_empty)
    });
    // The withdrawal taken as well: one change in all.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(clipboard.change_count().unwrap(), count + 1);
    desktop.paste(UTF8, 82);
    assert_eq!(pasted(&board, 82), None);
    assert!(source.fetched.lock().unwrap().is_empty());
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_session_restored_with_the_clipboard_asks_again_at_its_next_try() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    let state = tempfile::tempdir().unwrap();
    Tokens::in_dir(Some(state.path()))
        .save_all(&[
            ("remote_desktop", Some("kept")),
            ("remote_desktop_clipboard", Some("yes")),
        ])
        .unwrap();
    // Not answered once: the desktop restores only a session that asks
    // alike, so with tries left this start fails before it used the
    // token, and the next asks again.
    board.lock().unwrap().unanswered = 1;
    let clipboard = PortalClipboard::new();
    let remote = desktop.remote_sharing(&clipboard, state.path());
    running_within(&remote, Duration::from_secs(10));
    let calls = desktop.calls();
    let count = |call: &str| calls.iter().filter(|c| **c == call).count();
    assert_eq!(count("CreateSession"), 2, "{calls:?}");
    assert_eq!(count("RequestClipboard"), 2, "{calls:?}");
    assert_eq!(count("Start"), 1, "{calls:?}");
    assert_eq!(
        desktop.asked.lock().unwrap().restore_tokens,
        [Some("kept".to_owned())]
    );
    assert_eq!(
        Tokens::in_dir(Some(state.path()))
            .load("remote_desktop_clipboard")
            .as_deref(),
        Some("yes")
    );
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_clipboard_request_answered_late_is_taken_as_asked() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    // The first request reaches the desktop, which answers it past the
    // time it has; asked again, it says it was asked already.
    board.lock().unwrap().answer_late = Some(CLIPBOARD_WAIT + Duration::from_secs(1));
    let clipboard = PortalClipboard::new();
    let state = tempfile::tempdir().unwrap();
    let remote = desktop.remote_sharing(&clipboard, state.path());
    remote.ask().unwrap();
    running_within(&remote, Duration::from_secs(10));
    let calls = desktop.calls();
    assert_eq!(
        calls.iter().filter(|c| **c == "RequestClipboard").count(),
        2,
        "{calls:?}"
    );
    assert_eq!(
        Tokens::in_dir(Some(state.path()))
            .load("remote_desktop_clipboard")
            .as_deref(),
        Some("yes")
    );
    // The desktop shares it: it is followed.
    desktop.copied(&board, &[(UTF8, b"there")]);
    wait_until("the clipboard", || {
        clipboard.read(1 << 20).ok()
            == Some(ClipboardRead::Content(content(&[(
                ClipboardFormat::Text,
                b"there",
            )])))
    });
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn pastes_in_flight_end_with_the_session() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    let mut clipboard = PortalClipboard::new();
    let (remote, _state) = running(&desktop, &clipboard);
    let source = Arc::new(Source {
        content: content(&[(ClipboardFormat::Text, b"promised")]),
        slow: Duration::from_secs(3),
        ..Source::default()
    });
    clipboard
        .write_promise(promise(&source, &[ClipboardFormat::Text], &[]))
        .unwrap();
    // One paste fetches, another waits for that fetch.
    desktop.paste(UTF8, 41);
    wait_until("the fetch", || !source.fetched.lock().unwrap().is_empty());
    desktop.paste(UTF8, 42);
    std::thread::sleep(Duration::from_millis(200));

    // The desktop ends the session meanwhile.
    desktop.close_last_session();
    wait_until("the session to end", || remote.portal() == Portal::Stopped);
    assert!(source.finished.load(Ordering::SeqCst));
    // The paste waiting gets nothing at once, its pipe closed.
    let ended = std::time::Instant::now();
    wait_until("the waiting paste's pipe to close", || {
        board.lock().unwrap().written.contains_key(&42)
    });
    assert!(ended.elapsed() < Duration::from_secs(1));
    assert_eq!(board.lock().unwrap().written[&42], b"");
    // The fetching one closes its pipe as the source returns; neither is
    // answered to the desktop for a session gone.
    wait_until("the fetching paste's pipe to close", || {
        board.lock().unwrap().written.contains_key(&41)
    });
    std::thread::sleep(Duration::from_millis(300));
    let done = board.lock().unwrap().done.clone();
    assert!(
        !done.iter().any(|(serial, _)| [41, 42].contains(serial)),
        "{done:?}"
    );
    assert_eq!(*source.fetched.lock().unwrap(), [ClipboardFormat::Text]);
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn a_session_restored_with_the_clipboard_starts_without_it_on_its_last_try() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    let state = tempfile::tempdir().unwrap();
    Tokens::in_dir(Some(state.path()))
        .save_all(&[
            ("remote_desktop", Some("kept")),
            ("remote_desktop_clipboard", Some("yes")),
        ])
        .unwrap();
    // Never answered: the first start and its two tries fail before they
    // used the token; the last asks twice and goes on without.
    board.lock().unwrap().unanswered = usize::MAX;
    let clipboard = PortalClipboard::new();
    let remote = desktop.remote_with(&clipboard, state.path(), 2);
    running_within(&remote, Duration::from_secs(20));
    let calls = desktop.calls();
    let count = |call: &str| calls.iter().filter(|c| **c == call).count();
    assert_eq!(count("CreateSession"), 3, "{calls:?}");
    assert_eq!(count("RequestClipboard"), 4, "{calls:?}");
    assert_eq!(count("Start"), 1, "{calls:?}");
    assert_eq!(
        desktop.asked.lock().unwrap().restore_tokens,
        [Some("kept".to_owned())]
    );
    // The desktop restored it without the clipboard (GNOME): so it is
    // restored next time.
    assert_eq!(
        Tokens::in_dir(Some(state.path()))
            .load("remote_desktop_clipboard")
            .as_deref(),
        Some("no")
    );
    assert!(matches!(
        clipboard.read(1 << 20),
        Err(PlatformError::Unavailable(_))
    ));
}

#[test]
#[ignore = "needs dbus-daemon; scripts/linux-wayland-tests.sh"]
fn the_portal_lets_go_of_its_promise_as_data_control_takes_over() {
    let desktop = Desktop::start();
    let board = desktop.with_clipboard();
    let mut clipboard = PortalClipboard::new();
    let (_remote, _state) = running(&desktop, &clipboard);
    let source = Arc::new(Source {
        content: content(&[(ClipboardFormat::Text, b"promised")]),
        ..Source::default()
    });
    clipboard
        .write_promise(promise(&source, &[ClipboardFormat::Text], &[]))
        .unwrap();
    assert!(!source.finished.load(Ordering::SeqCst));
    // Data-control is used from now on: the promise ends, and the desktop
    // is asked to take nothing in its place.
    clipboard.let_go();
    assert!(source.finished.load(Ordering::SeqCst));
    wait_until("the promise to be withdrawn", || {
        board
            .lock()
            .unwrap()
            .selections
            .last()
            .is_some_and(Vec::is_empty)
    });
    desktop.paste(UTF8, 71);
    assert_eq!(pasted(&board, 71), None);
    assert!(source.fetched.lock().unwrap().is_empty());
    assert_eq!(clipboard.read(1 << 20).unwrap(), ClipboardRead::Empty);
}
