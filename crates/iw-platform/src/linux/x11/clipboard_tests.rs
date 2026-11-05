//! The X11 clipboard against a real X server, which the Linux container
//! starts as Xvfb (`scripts/linux-x11-tests.sh`): ignored otherwise.
//! Another app is played by `Peer`, which owns the clipboard with what it
//! is given, and by `xclip`, which reads what is written here. The tests
//! share the server's clipboard; run them one at a time.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask,
    PropMode, Property, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME, NONE};

use super::X11Clipboard;
use super::platform::connect;
use crate::{
    Clipboard, ClipboardContent, ClipboardFormat, ClipboardPromise, ClipboardRead, ClipboardSource,
    FilesFetch, PromisedFile,
};

const WAIT: Duration = Duration::from_secs(3);
/// The parts `Peer` sends large content in.
const PEER_PART: usize = 100 * 1024;
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR not really";

/// What a `Peer` has: targets and their bytes.
type Items = Arc<Mutex<Vec<(Atom, Vec<u8>)>>>;

/// Another app owning the clipboard with `items` (target name, bytes),
/// large ones in parts, until dropped.
struct Peer {
    conn: Arc<x11rb::rust_connection::RustConnection>,
    window: u32,
    clipboard: Atom,
    items: Items,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// How a `Peer` answers, beyond giving what it has.
#[derive(Default)]
struct Ways {
    /// The first request is answered this late, after the requests that
    /// came meanwhile.
    late: Duration,
    /// And refused, rather than answered.
    refuse_late: bool,
    /// Targets it lists but refuses.
    refused: Vec<&'static str>,
    /// Every request is answered this late.
    slow: Duration,
}

impl Peer {
    fn own(items: &[(&str, &[u8])]) -> Self {
        Self::with(items, Ways::default())
    }

    fn own_late(items: &[(&str, &[u8])], late: Duration) -> Self {
        Self::with(
            items,
            Ways {
                late,
                ..Ways::default()
            },
        )
    }

    fn with(items: &[(&str, &[u8])], ways: Ways) -> Self {
        let (conn, root) = connect().unwrap();
        let atom = |name: &str| {
            conn.intern_atom(false, name.as_bytes())
                .unwrap()
                .reply()
                .unwrap()
                .atom
        };
        let (clipboard, targets, incr) = (atom("CLIPBOARD"), atom("TARGETS"), atom("INCR"));
        let items: Vec<(Atom, Vec<u8>)> =
            items.iter().map(|(n, d)| (atom(n), d.to_vec())).collect();
        let items = Arc::new(Mutex::new(items));
        let refused: Vec<Atom> = ways.refused.iter().map(|n| atom(n)).collect();
        let (late, refuse_late, slow) = (ways.late, ways.refuse_late, ways.slow);
        let window = conn.generate_id().unwrap();
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            COPY_FROM_PARENT,
            &CreateWindowAux::new(),
        )
        .unwrap();
        conn.set_selection_owner(window, clipboard, CURRENT_TIME)
            .unwrap();
        let owner = conn
            .get_selection_owner(clipboard)
            .unwrap()
            .reply()
            .unwrap()
            .owner;
        assert_eq!(owner, window);
        let conn = Arc::new(conn);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (stop, conn, items) = (stop.clone(), conn.clone(), items.clone());
            std::thread::spawn(move || {
                // Answers going in parts: requestor, property, type, data, sent.
                let mut parts: Vec<(u32, Atom, Atom, Vec<u8>, usize)> = Vec::new();
                // Events read while answering late, to handle in order.
                let mut pending: std::collections::VecDeque<Event> = Default::default();
                let mut delayed = false;
                while !stop.load(Ordering::SeqCst) {
                    let event = match pending.pop_front() {
                        Some(event) => event,
                        None => match conn.poll_for_event().unwrap() {
                            Some(event) => event,
                            None => {
                                std::thread::sleep(Duration::from_millis(2));
                                continue;
                            }
                        },
                    };
                    match event {
                        Event::SelectionRequest(e) if !late.is_zero() && !delayed => {
                            delayed = true;
                            std::thread::sleep(late);
                            let mut later = Vec::new();
                            while let Some(event) = conn.poll_for_event().unwrap() {
                                later.push(event);
                            }
                            if refuse_late {
                                let notify = SelectionNotifyEvent {
                                    response_type: SELECTION_NOTIFY_EVENT,
                                    sequence: 0,
                                    time: e.time,
                                    requestor: e.requestor,
                                    selection: e.selection,
                                    target: e.target,
                                    property: NONE,
                                };
                                conn.send_event(false, e.requestor, EventMask::NO_EVENT, notify)
                                    .unwrap();
                                conn.flush().unwrap();
                            } else {
                                pending.push_back(Event::SelectionRequest(e));
                            }
                            pending.extend(later);
                        }
                        Event::SelectionRequest(e) => {
                            std::thread::sleep(slow);
                            let property = if e.property == NONE {
                                e.target
                            } else {
                                e.property
                            };
                            let items = items.lock().unwrap().clone();
                            let answered = if e.target == targets {
                                let mut list: Vec<u32> = vec![targets];
                                list.extend(items.iter().map(|(a, _)| *a));
                                conn.change_property32(
                                    PropMode::REPLACE,
                                    e.requestor,
                                    property,
                                    AtomEnum::ATOM,
                                    &list,
                                )
                                .unwrap();
                                property
                            } else if refused.contains(&e.target) {
                                NONE
                            } else if let Some((_, data)) =
                                items.iter().find(|(a, _)| *a == e.target)
                            {
                                if data.len() > PEER_PART {
                                    conn.change_window_attributes(
                                        e.requestor,
                                        &ChangeWindowAttributesAux::new()
                                            .event_mask(EventMask::PROPERTY_CHANGE),
                                    )
                                    .unwrap();
                                    conn.change_property32(
                                        PropMode::REPLACE,
                                        e.requestor,
                                        property,
                                        incr,
                                        &[data.len() as u32],
                                    )
                                    .unwrap();
                                    parts.push((e.requestor, property, e.target, data.clone(), 0));
                                } else {
                                    conn.change_property8(
                                        PropMode::REPLACE,
                                        e.requestor,
                                        property,
                                        e.target,
                                        data,
                                    )
                                    .unwrap();
                                }
                                property
                            } else {
                                NONE
                            };
                            let notify = SelectionNotifyEvent {
                                response_type: SELECTION_NOTIFY_EVENT,
                                sequence: 0,
                                time: e.time,
                                requestor: e.requestor,
                                selection: e.selection,
                                target: e.target,
                                property: answered,
                            };
                            conn.send_event(false, e.requestor, EventMask::NO_EVENT, notify)
                                .unwrap();
                            conn.flush().unwrap();
                        }
                        Event::PropertyNotify(e) if e.state == Property::DELETE => {
                            if let Some(at) =
                                parts.iter().position(|p| p.0 == e.window && p.1 == e.atom)
                            {
                                let (requestor, property, kind, data, sent) = &mut parts[at];
                                let end = (*sent + PEER_PART).min(data.len());
                                conn.change_property8(
                                    PropMode::REPLACE,
                                    *requestor,
                                    *property,
                                    *kind,
                                    &data[*sent..end],
                                )
                                .unwrap();
                                conn.flush().unwrap();
                                if *sent == end {
                                    parts.remove(at);
                                } else {
                                    *sent = end;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            })
        };
        Self {
            conn,
            window,
            clipboard,
            items,
            stop,
            thread: Some(thread),
        }
    }

    /// Copies `items` again, from the same window, as a password manager
    /// copies a user name and then the password.
    fn recopy(&self, items: &[(&str, &[u8])]) {
        let atom = |name: &str| {
            self.conn
                .intern_atom(false, name.as_bytes())
                .unwrap()
                .reply()
                .unwrap()
                .atom
        };
        *self.items.lock().unwrap() = items.iter().map(|(n, d)| (atom(n), d.to_vec())).collect();
        self.conn
            .set_selection_owner(self.window, self.clipboard, CURRENT_TIME)
            .unwrap();
        let owner = self
            .conn
            .get_selection_owner(self.clipboard)
            .unwrap()
            .reply()
            .unwrap()
            .owner;
        assert_eq!(owner, self.window);
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// How long `xclip` may wait for an answer: an app that never gets one
/// fails the test rather than hold the script up.
const XCLIP_WAIT: Duration = Duration::from_secs(15);

/// What `xclip` reads from the clipboard as `target`, or `None` when it
/// could not.
fn xclip(target: &str) -> Option<Vec<u8>> {
    let mut child = Command::new("xclip")
        .args(["-selection", "clipboard", "-o", "-t", target])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("xclip is installed in the Linux container");
    let mut stdout = child.stdout.take().unwrap();
    let reading = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut out);
        out
    });
    let deadline = Instant::now() + XCLIP_WAIT;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("xclip got no answer for {target} in {XCLIP_WAIT:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let out = reading.join().unwrap();
    status.success().then_some(out)
}

/// Waits for `ready`, for at most `WAIT`.
fn eventually(mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if ready() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    ready()
}

fn changed_from(clipboard: &X11Clipboard, count: u64) {
    assert!(
        eventually(|| clipboard.change_count().unwrap() != count),
        "the change count stayed at {count}"
    );
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

fn content(items: &[(ClipboardFormat, &[u8])]) -> ClipboardContent {
    ClipboardContent {
        items: items.iter().map(|(f, d)| (*f, d.to_vec())).collect(),
    }
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn reads_what_another_app_copied() {
    let clipboard = X11Clipboard::connect().unwrap();
    let before = clipboard.change_count().unwrap();
    let _peer = Peer::own(&[
        ("UTF8_STRING", "鍵鼠 share\n".as_bytes()),
        ("text/html", b"<b>x</b>"),
        ("application/rtf", b"{\\rtf1 x}\0"),
        ("image/png", PNG),
    ]);
    changed_from(&clipboard, before);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[
            (ClipboardFormat::Text, "鍵鼠 share\n".as_bytes()),
            (ClipboardFormat::Rtf, b"{\\rtf1 x}"),
            (ClipboardFormat::Html, b"<b>x</b>"),
            (ClipboardFormat::Png, PNG),
        ]))
    );
    assert!(matches!(clipboard.read(10), Ok(ClipboardRead::TooLarge(_))));
    assert!(clipboard.files().unwrap().is_empty());
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn large_content_comes_in_parts() {
    let clipboard = X11Clipboard::connect().unwrap();
    let text: Vec<u8> = (0..600 * 1024).map(|i| b'a' + (i % 26) as u8).collect();
    let before = clipboard.change_count().unwrap();
    let _peer = Peer::own(&[("UTF8_STRING", &text)]);
    changed_from(&clipboard, before);
    let read = clipboard.read(1 << 20).unwrap();
    assert_eq!(
        read,
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, &text)]))
    );
    // Too large, it is left unread, and reading goes on as before.
    assert_eq!(
        clipboard.read(300 * 1024).unwrap(),
        ClipboardRead::TooLarge(text.len())
    );
    assert_eq!(clipboard.read(1 << 20).unwrap(), read);
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn what_password_managers_mark_is_not_read() {
    let clipboard = X11Clipboard::connect().unwrap();
    let before = clipboard.change_count().unwrap();
    let peer = Peer::own(&[
        ("UTF8_STRING", b"hunter2"),
        ("x-kde-passwordManagerHint", b"secret"),
        ("text/uri-list", b"file:///tmp/x\r\n"),
    ]);
    changed_from(&clipboard, before);
    assert_eq!(clipboard.read(1 << 20).unwrap(), ClipboardRead::Sensitive);
    assert!(clipboard.files().unwrap().is_empty());
    drop(peer);
    let before = clipboard.change_count().unwrap();
    let _peer = Peer::own(&[
        ("UTF8_STRING", b"hello"),
        ("x-kde-passwordManagerHint", b"public"),
    ]);
    changed_from(&clipboard, before);
    assert!(matches!(
        clipboard.read(1 << 20),
        Ok(ClipboardRead::Content(_))
    ));
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn a_password_copied_after_its_user_name_is_not_read() {
    let clipboard = X11Clipboard::connect().unwrap();
    let before = clipboard.change_count().unwrap();
    let peer = Peer::own(&[
        ("UTF8_STRING", b"me@example.com"),
        ("x-kde-passwordManagerHint", b"public"),
    ]);
    changed_from(&clipboard, before);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"me@example.com")]))
    );
    // The same window copies the password a moment later, and the read
    // that follows at once looks again rather than trusting what it found
    // a moment ago.
    peer.recopy(&[
        ("UTF8_STRING", b"hunter2"),
        ("x-kde-passwordManagerHint", b"secret"),
    ]);
    assert_eq!(clipboard.read(1 << 20).unwrap(), ClipboardRead::Sensitive);
    assert!(clipboard.files().unwrap().is_empty());
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn a_copy_right_after_a_write_is_not_taken_for_it() {
    let mut clipboard = X11Clipboard::connect().unwrap();
    let peer: Arc<Mutex<Option<Peer>>> = Arc::default();
    {
        let peer = peer.clone();
        // Another app copies the moment the server said this clipboard
        // took it, before the write is counted.
        *super::clipboard::AFTER_TAKING.lock().unwrap() = Some(Box::new(move || {
            let mut peer = peer.lock().unwrap();
            if peer.is_none() {
                *peer = Some(Peer::own(&[("UTF8_STRING", b"theirs")]));
            }
        }));
    }
    let written = clipboard.write(&content(&[(ClipboardFormat::Text, b"ours")]));
    *super::clipboard::AFTER_TAKING.lock().unwrap() = None;
    let written = written.unwrap();
    // Their copy comes after the write: the count moves past the write's,
    // so the agent reads it, and it is theirs.
    changed_from(&clipboard, written);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"theirs")]))
    );
    drop(peer);
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn files_copied_in_a_file_manager_are_listed() {
    let clipboard = X11Clipboard::connect().unwrap();
    let before = clipboard.change_count().unwrap();
    let peer = Peer::own(&[
        (
            "x-special/gnome-copied-files",
            b"copy\nfile:///tmp/a%20b\nfile:///tmp/c",
        ),
        ("text/uri-list", b"file:///tmp/other\r\n"),
        ("UTF8_STRING", b"/tmp/a b\n/tmp/c"),
    ]);
    changed_from(&clipboard, before);
    assert_eq!(
        clipboard.files().unwrap(),
        [PathBuf::from("/tmp/a b"), PathBuf::from("/tmp/c")]
    );
    drop(peer);
    let before = clipboard.change_count().unwrap();
    let peer = Peer::own(&[(
        "text/uri-list",
        b"# Dolphin\r\nfile:///tmp/d\r\nhttps://x/y\r\n",
    )]);
    changed_from(&clipboard, before);
    assert_eq!(clipboard.files().unwrap(), [PathBuf::from("/tmp/d")]);
    drop(peer);
    // GNOME's list with no local file in it does not hide the other one's.
    let before = clipboard.change_count().unwrap();
    let _peer = Peer::own(&[
        ("x-special/gnome-copied-files", b"copy\nhttps://x/y"),
        ("text/uri-list", b"file:///tmp/e\r\n"),
    ]);
    changed_from(&clipboard, before);
    assert_eq!(clipboard.files().unwrap(), [PathBuf::from("/tmp/e")]);
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn what_is_written_here_other_apps_paste() {
    let mut clipboard = X11Clipboard::connect().unwrap();
    let written = content(&[
        (ClipboardFormat::Text, "鍵鼠\ncafé".as_bytes()),
        (ClipboardFormat::Rtf, b"{\\rtf1 x}"),
        (ClipboardFormat::Html, b"<b>x</b>"),
        (ClipboardFormat::Png, PNG),
    ]);
    let count = clipboard.write(&written).unwrap();
    // Written here, it is no copy of the user's.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(clipboard.change_count().unwrap(), count);
    let targets = String::from_utf8(xclip("TARGETS").unwrap()).unwrap();
    for target in [
        "TARGETS",
        "UTF8_STRING",
        "STRING",
        "text/plain;charset=utf-8",
        "text/html",
        "text/rtf",
        "application/rtf",
        "image/png",
    ] {
        assert!(
            targets.lines().any(|t| t == target),
            "{target} in {targets}"
        );
    }
    assert_eq!(xclip("UTF8_STRING").unwrap(), "鍵鼠\ncafé".as_bytes());
    assert_eq!(xclip("STRING").unwrap(), b"??\ncaf\xe9");
    assert_eq!(xclip("text/html").unwrap(), b"<b>x</b>");
    assert_eq!(xclip("application/rtf").unwrap(), b"{\\rtf1 x}");
    assert_eq!(xclip("image/png").unwrap(), PNG);
    // Read back here, from what was written.
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(written)
    );

    // Large, it goes in parts.
    let text: Vec<u8> = (0..700 * 1024).map(|i| b'a' + (i % 26) as u8).collect();
    clipboard
        .write(&content(&[(ClipboardFormat::Text, &text)]))
        .unwrap();
    assert_eq!(xclip("UTF8_STRING").unwrap(), text);

    // Copied in another app, it counts.
    let count = clipboard.change_count().unwrap();
    let _peer = Peer::own(&[("UTF8_STRING", b"theirs")]);
    changed_from(&clipboard, count);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"theirs")]))
    );
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn promised_content_is_fetched_as_it_is_pasted() {
    let mut clipboard = X11Clipboard::connect().unwrap();
    let source = Arc::new(Source {
        content: content(&[
            (ClipboardFormat::Text, b"promised"),
            (ClipboardFormat::Png, PNG),
        ]),
        ..Source::default()
    });
    let count = clipboard
        .write_promise(ClipboardPromise {
            formats: vec![ClipboardFormat::Text, ClipboardFormat::Png],
            bytes: 8 + PNG.len() as u64,
            files: Vec::new(),
            source: source.clone(),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(clipboard.change_count().unwrap(), count);
    assert!(source.fetched.lock().unwrap().is_empty());
    // Its own promise is never read back.
    assert_eq!(clipboard.read(1 << 20).unwrap(), ClipboardRead::Empty);
    let targets = String::from_utf8(xclip("TARGETS").unwrap()).unwrap();
    assert!(targets.lines().any(|t| t == "image/png"));
    assert!(!targets.lines().any(|t| t == "text/uri-list"));
    assert!(source.fetched.lock().unwrap().is_empty());
    assert_eq!(xclip("UTF8_STRING").unwrap(), b"promised");
    assert_eq!(*source.fetched.lock().unwrap(), [ClipboardFormat::Text]);
    assert_eq!(xclip("image/png").unwrap(), PNG);
    assert!(
        source
            .fetched
            .lock()
            .unwrap()
            .contains(&ClipboardFormat::Png)
    );
    assert!(xclip("text/html").is_none());
    assert!(!source.finished.load(Ordering::SeqCst));

    // Something else copied, the promise is gone.
    let _peer = Peer::own(&[("UTF8_STRING", b"theirs")]);
    assert!(eventually(|| source.finished.load(Ordering::SeqCst)));
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn a_promise_written_over_here_is_finished() {
    let mut clipboard = X11Clipboard::connect().unwrap();
    let source = Arc::new(Source::default());
    clipboard
        .write_promise(ClipboardPromise {
            formats: vec![ClipboardFormat::Text],
            bytes: 1,
            files: Vec::new(),
            source: source.clone(),
        })
        .unwrap();
    assert!(!source.finished.load(Ordering::SeqCst));
    clipboard
        .write(&content(&[(ClipboardFormat::Text, b"mine")]))
        .unwrap();
    assert!(source.finished.load(Ordering::SeqCst));
    assert_eq!(xclip("UTF8_STRING").unwrap(), b"mine");
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn promised_files_are_pasted_from_where_they_arrived() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a b.txt");
    let folder = dir.path().join("Photos");
    std::fs::write(&file, b"x").unwrap();
    std::fs::create_dir(&folder).unwrap();
    let mut clipboard = X11Clipboard::connect().unwrap();
    let source = Arc::new(Source {
        files: Mutex::new(Some(FilesFetch::Ready(vec![
            Some(file.clone()),
            Some(folder.clone()),
        ]))),
        ..Source::default()
    });
    let files = vec![
        PromisedFile {
            name: "a b.txt".into(),
            folder: false,
        },
        PromisedFile {
            name: "Photos".into(),
            folder: true,
        },
    ];
    clipboard
        .write_promise(ClipboardPromise {
            formats: Vec::new(),
            bytes: 0,
            files: files.clone(),
            source: source.clone(),
        })
        .unwrap();
    let targets = String::from_utf8(xclip("TARGETS").unwrap()).unwrap();
    for target in [
        "text/uri-list",
        "x-special/gnome-copied-files",
        "x-kde-passwordManagerHint",
        "UTF8_STRING",
    ] {
        assert!(
            targets.lines().any(|t| t == target),
            "{target} in {targets}"
        );
    }
    assert_eq!(*source.files_asked.lock().unwrap(), 0);
    // The names are known without the files.
    assert_eq!(xclip("UTF8_STRING").unwrap(), b"a b.txt\nPhotos");
    assert_eq!(xclip("x-kde-passwordManagerHint").unwrap(), b"secret");
    assert_eq!(*source.files_asked.lock().unwrap(), 0);
    let uris = |paths: &[&PathBuf]| -> String {
        paths
            .iter()
            .map(|p| format!("file://{}", p.display()).replace(' ', "%20"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let list = String::from_utf8(xclip("text/uri-list").unwrap()).unwrap();
    assert_eq!(
        list.replace("\r\n", "\n").trim_end(),
        uris(&[&file, &folder])
    );
    let gnome = String::from_utf8(xclip("x-special/gnome-copied-files").unwrap()).unwrap();
    assert_eq!(gnome, format!("copy\n{}", uris(&[&file, &folder])));
    // Asked once, and kept for every later paste.
    assert_eq!(*source.files_asked.lock().unwrap(), 1);
    *source.files.lock().unwrap() = Some(FilesFetch::Busy);
    assert!(xclip("text/uri-list").is_some());
    assert_eq!(*source.files_asked.lock().unwrap(), 1);

    // Still coming, nothing is pasted, and the next paste asks again; what
    // did not come is left out.
    let source = Arc::new(Source {
        files: Mutex::new(Some(FilesFetch::Busy)),
        ..Source::default()
    });
    clipboard
        .write_promise(ClipboardPromise {
            formats: Vec::new(),
            bytes: 0,
            files,
            source: source.clone(),
        })
        .unwrap();
    assert!(xclip("text/uri-list").is_none());
    assert_eq!(*source.files_asked.lock().unwrap(), 1);
    *source.files.lock().unwrap() = Some(FilesFetch::Ready(vec![None, Some(folder.clone())]));
    let list = String::from_utf8(xclip("text/uri-list").unwrap()).unwrap();
    assert_eq!(list.replace("\r\n", "\n").trim_end(), uris(&[&folder]));
    assert_eq!(*source.files_asked.lock().unwrap(), 2);
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn an_app_that_never_answers_holds_nothing_up() {
    let clipboard = X11Clipboard::connect().unwrap();
    // It takes the clipboard and reads none of what it is asked.
    let (conn, root) = connect().unwrap();
    let atom = |name: &str| {
        conn.intern_atom(false, name.as_bytes())
            .unwrap()
            .reply()
            .unwrap()
            .atom
    };
    let selection = atom("CLIPBOARD");
    let window = conn.generate_id().unwrap();
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        COPY_FROM_PARENT,
        &CreateWindowAux::new(),
    )
    .unwrap();
    conn.set_selection_owner(window, selection, CURRENT_TIME)
        .unwrap();
    conn.get_selection_owner(selection)
        .unwrap()
        .reply()
        .unwrap();
    let started = Instant::now();
    assert!(clipboard.read(1 << 20).is_err());
    let took = started.elapsed();
    assert!(took < Duration::from_secs(5), "took {took:?}");
    drop(conn);
    // Another app's answer is read as before.
    let before = clipboard.change_count().unwrap();
    let _peer = Peer::own(&[("UTF8_STRING", b"after")]);
    changed_from(&clipboard, before);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"after")]))
    );
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn an_app_slow_to_answer_each_request_holds_a_read_up_only_so_long() {
    let clipboard = X11Clipboard::connect().unwrap();
    let before = clipboard.change_count().unwrap();
    // Each answer comes in time, but answering all of them would not.
    let _peer = Peer::with(
        &[
            ("UTF8_STRING", b"text"),
            ("text/html", b"<b>x</b>"),
            ("text/rtf", b"{\\rtf1 x}"),
            ("image/png", PNG),
        ],
        Ways {
            slow: Duration::from_millis(2500),
            ..Ways::default()
        },
    );
    changed_from(&clipboard, before);
    let started = Instant::now();
    assert!(clipboard.read(1 << 20).is_err());
    let took = started.elapsed();
    assert!(took < Duration::from_secs(10), "the read took {took:?}");
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn a_late_answer_is_not_taken_for_the_next() {
    let clipboard = X11Clipboard::connect().unwrap();
    let before = clipboard.change_count().unwrap();
    // It answers the first request past the wait, after the next came.
    let _peer = Peer::own_late(&[("UTF8_STRING", b"late")], Duration::from_millis(3500));
    changed_from(&clipboard, before);
    assert!(clipboard.read(1 << 20).is_err());
    // The answer to the first request comes during the second, to the
    // property given up on; the second is read whole all the same.
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"late")]))
    );
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn pastes_of_the_same_format_share_one_fetch() {
    let mut clipboard = X11Clipboard::connect().unwrap();
    let source = Arc::new(Source {
        content: content(&[(ClipboardFormat::Text, b"once")]),
        slow: Duration::from_millis(800),
        ..Source::default()
    });
    clipboard
        .write_promise(ClipboardPromise {
            formats: vec![ClipboardFormat::Text],
            bytes: 4,
            files: Vec::new(),
            source: source.clone(),
        })
        .unwrap();
    let pastes: Vec<_> = (0..6)
        .map(|_| std::thread::spawn(|| xclip("UTF8_STRING")))
        .collect();
    for paste in pastes {
        assert_eq!(paste.join().unwrap().unwrap(), b"once");
    }
    assert_eq!(*source.fetched.lock().unwrap(), [ClipboardFormat::Text]);
    // A later paste asks again, and the source gives what it keeps: no
    // second copy is kept here.
    assert_eq!(xclip("STRING").unwrap(), b"once");
    assert_eq!(
        *source.fetched.lock().unwrap(),
        [ClipboardFormat::Text, ClipboardFormat::Text]
    );
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn dropped_it_lets_go_of_the_clipboard() {
    let mut clipboard = X11Clipboard::connect().unwrap();
    let source = Arc::new(Source {
        content: content(&[(ClipboardFormat::Text, b"slow")]),
        slow: Duration::from_secs(20),
        ..Source::default()
    });
    clipboard
        .write_promise(ClipboardPromise {
            formats: vec![ClipboardFormat::Text],
            bytes: 4,
            files: Vec::new(),
            source: source.clone(),
        })
        .unwrap();
    let paste = std::thread::spawn(|| xclip("UTF8_STRING"));
    assert!(eventually(|| !source.fetched.lock().unwrap().is_empty()));
    let started = Instant::now();
    drop(clipboard);
    // The paste waiting is refused, and nothing owns the clipboard.
    assert_eq!(paste.join().unwrap(), None);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(source.finished.load(Ordering::SeqCst));
    let (conn, _) = connect().unwrap();
    let clipboard_atom = conn
        .intern_atom(false, b"CLIPBOARD")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let owner = conn
        .get_selection_owner(clipboard_atom)
        .unwrap()
        .reply()
        .unwrap()
        .owner;
    assert_eq!(owner, NONE);
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn a_late_refusal_is_not_taken_for_the_next() {
    let clipboard = X11Clipboard::connect().unwrap();
    let before = clipboard.change_count().unwrap();
    // It refuses the first request past the wait, after the next came:
    // its list of targets, which a refusal taken for the next one's would
    // leave the HTML unread.
    let _peer = Peer::with(
        &[("text/html", b"<b>x</b>")],
        Ways {
            late: Duration::from_millis(3500),
            refuse_late: true,
            ..Ways::default()
        },
    );
    changed_from(&clipboard, before);
    assert!(clipboard.read(1 << 20).is_err());
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Html, b"<b>x</b>")]))
    );
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn a_refused_target_falls_back_to_the_next() {
    let clipboard = X11Clipboard::connect().unwrap();
    let before = clipboard.change_count().unwrap();
    let _peer = Peer::with(
        &[("UTF8_STRING", b"unused"), ("STRING", b"caf\xe9")],
        Ways {
            refused: vec!["UTF8_STRING"],
            ..Ways::default()
        },
    );
    changed_from(&clipboard, before);
    assert_eq!(
        clipboard.read(1 << 20).unwrap(),
        ClipboardRead::Content(content(&[(ClipboardFormat::Text, "café".as_bytes())]))
    );
}

#[test]
#[ignore = "needs an X server: scripts/linux-x11-tests.sh"]
fn the_clipboard_comes_back_after_the_server_restarts() {
    let mut server = super::tests::OwnXvfb::start();
    let clipboard = X11Clipboard::connect().unwrap();
    let before = clipboard.change_count().unwrap();
    {
        let _peer = Peer::own(&[("UTF8_STRING", b"before")]);
        changed_from(&clipboard, before);
        assert_eq!(
            clipboard.read(1 << 20).unwrap(),
            ClipboardRead::Content(content(&[(ClipboardFormat::Text, b"before")]))
        );
    }
    let count = clipboard.change_count().unwrap();

    server.stop();
    assert!(clipboard.read(1 << 20).is_err());
    // The display server starts over, as at a new login; nothing is
    // written here meanwhile.
    server.start_again();
    let _peer = Peer::own(&[("UTF8_STRING", b"after")]);
    // The connections try again on the reconnect backoff (0.5 s doubling
    // to 10 s); under load a try may come before the server takes them.
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        assert!(Instant::now() < deadline, "the clipboard did not come back");
        let read = clipboard.read(1 << 20);
        if read.as_ref().ok()
            == Some(&ClipboardRead::Content(content(&[(
                ClipboardFormat::Text,
                b"after",
            )])))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // The server going away, and the copy on the new one, both count.
    assert!(clipboard.change_count().unwrap() > count);

    drop((_peer, clipboard, server));
}
