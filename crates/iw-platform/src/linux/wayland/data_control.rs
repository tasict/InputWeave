//! The Wayland clipboard through data-control: `ext-data-control-v1` where the compositor offers it
//! (wlroots 0.19 and later, KDE Plasma 6.4 and later), else
//! `zwlr_data_control_manager_v1` (sway, Hyprland and other wlroots
//! compositors, KDE Plasma). Clipboard managers use these: a client may
//! watch and set the selection without a window or the keyboard's focus,
//! and nothing asks the user. GNOME offers neither (`portal_clipboard`).
//! Formats and file lists are `clipboard_formats`', answering and reading
//! `selection`'s. Only the `CLIPBOARD` selection, not the primary one.
//!
//! A connection of its own, followed by a thread:
//! - Every selection the compositor announces is a change, but one of
//!   this clipboard's own: each source offers, besides its content, a
//!   MIME type naming this clipboard and the source
//!   (`application/x-inputweave-source.<token>.<serial>`), and a selection
//!   offering it is this clipboard's while the compositor has not
//!   cancelled the source, or when it names the last source announced.
//!   Clipboard managers (Klipper, wl-clip-persist) take every new
//!   selection over with a copy of all its MIME types, the marker among
//!   them, read up front: what they offer of the last write is still that
//!   write, read back as written, not a copy, and a promise is kept while
//!   that copy is the selection (files pasted from it are in the agent's
//!   folder for them, which goes once the promise does); their copy of an
//!   older one, as when the user picks it from the manager's history, is a
//!   copy. What is read back is what the source the selection names
//!   holds, not a newer write the compositor has not announced.
//! - A write takes the selection with a new source and waits, as long as
//!   `WRITE_WAIT`, for the compositor to announce it: the count goes up by
//!   one then, so what was announced before counts before the write and
//!   another app's copy right after it counts after.
//! - An app pasting has the compositor ask the source for a MIME type
//!   (`send`, with a pipe): written content is answered at once, as far as
//!   the pipe takes it (all of most), the rest on a thread of its own, as
//!   is a promise's content, fetched first, once for each format
//!   (`selection::Promised`); apps wait as long as they are willing to. Klipper and other
//!   clipboard managers read every new selection, which fetches a promise
//!   at once; promised files carry `x-kde-passwordManagerHint` = `secret`,
//!   which they honor, so they do not fetch the files (wl-clip-persist
//!   reads everything). A cancelled source is finished, but the last one
//!   announced: that one once another app's selection is announced.
//! - Reading goes from the caller's thread: it asks the current offer for
//!   each MIME type through a pipe, with `selection`'s time limits. Its
//!   own content is answered from what was written, a promise reads as
//!   nothing. Whether an offer is marked as not to be shared is asked of
//!   it once, for both reading its content and its files. Only
//!   `change_count` waits for the thread to hear what the compositor sent
//!   before: reading what the thread heard last is as good, since a
//!   selection it has yet to hear of changes the count again.
//! - The connection opens as the clipboard is first used, on a thread of
//!   its own, so no call waits for the compositor (each step within
//!   `CONNECT_WAIT`); lost, as when the compositor starts over, the next
//!   call has it open again (from 0.5 s doubling to 10 s apart), which
//!   counts as a change; what was put on the clipboard from here went with
//!   it. Until it is open the calls are `Unavailable`; a compositor that
//!   turns out to have no data-control is `Unsupported` (`clipboard` then
//!   uses the portal). A write the compositor's going cuts off is an OS
//!   error, not something copied over it. Only the broken connection's
//!   thread lets go of what was put there through it, and a new one opens
//!   only once it did.
//! - A read whose offer another selection replaced meanwhile fails: what
//!   is left of a replaced offer reads as nothing.
//!
//! The selection goes with the connection: when the agent ends, what it
//! wrote or promised is no longer on the clipboard.

use std::collections::HashMap;
use std::io::{ErrorKind, Write as _};
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};
use wayland_client::backend::WaylandError;
use wayland_client::protocol::{wl_callback, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1 as ext_device, ext_data_control_manager_v1 as ext_manager,
    ext_data_control_offer_v1 as ext_offer, ext_data_control_source_v1 as ext_source,
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1 as wlr_device, zwlr_data_control_manager_v1 as wlr_manager,
    zwlr_data_control_offer_v1 as wlr_offer, zwlr_data_control_source_v1 as wlr_source,
};

use super::displays::roundtrip_until;
use crate::linux::clipboard_formats::Wire;
use crate::linux::common::{Backoff, lock, os, ready};
use crate::linux::selection::{
    self, ANSWER_WAIT, Asked, Owned, Promised, READ_WAIT, Served, Written,
};
use crate::{Clipboard, ClipboardContent, ClipboardPromise, ClipboardRead, PlatformError};

/// How long the compositor may take to say what the selection is, as the
/// connection opens.
const CONNECT_WAIT: Duration = Duration::from_secs(5);
/// How long the thread may take to hear what the compositor sent before.
const SYNC_WAIT: Duration = Duration::from_millis(500);
/// How long a write waits for the compositor to announce its selection.
const WRITE_WAIT: Duration = Duration::from_secs(2);
/// How long dropping the clipboard waits for its thread to end.
const STOP_WAIT: Duration = Duration::from_secs(1);
/// What the own MIME type of each source begins with.
const MARKER: &str = "application/x-inputweave-source.";
/// What an app asking for it gets.
const MARKER_ANSWER: &[u8] = b"InputWeave";

// ---------------------------------------------------------------------
// The two protocols, alike but for their names.

/// A data-control manager, of either protocol.
#[derive(Clone)]
enum Manager {
    Ext(ext_manager::ExtDataControlManagerV1),
    Wlr(wlr_manager::ZwlrDataControlManagerV1),
}

#[derive(Clone)]
enum Device {
    Ext(ext_device::ExtDataControlDeviceV1),
    Wlr(wlr_device::ZwlrDataControlDeviceV1),
}

#[derive(Clone)]
enum Source {
    Ext(ext_source::ExtDataControlSourceV1),
    Wlr(wlr_source::ZwlrDataControlSourceV1),
}

#[derive(Clone)]
enum Offer {
    Ext(ext_offer::ExtDataControlOfferV1),
    Wlr(wlr_offer::ZwlrDataControlOfferV1),
}

impl Manager {
    fn name(&self) -> &'static str {
        match self {
            Manager::Ext(_) => "ext-data-control-v1",
            Manager::Wlr(_) => "wlr-data-control-unstable-v1",
        }
    }

    fn device(&self, seat: &wl_seat::WlSeat, qh: &QueueHandle<State>) -> Device {
        match self {
            Manager::Ext(m) => Device::Ext(m.get_data_device(seat, qh, ())),
            Manager::Wlr(m) => Device::Wlr(m.get_data_device(seat, qh, ())),
        }
    }

    fn source(&self, qh: &QueueHandle<State>, data: SourceData) -> Source {
        match self {
            Manager::Ext(m) => Source::Ext(m.create_data_source(qh, data)),
            Manager::Wlr(m) => Source::Wlr(m.create_data_source(qh, data)),
        }
    }
}

impl Device {
    /// Clears the selection.
    fn clear(&self) {
        match self {
            Device::Ext(d) => d.set_selection(None),
            Device::Wlr(d) => d.set_selection(None),
        }
    }

    fn set_selection(&self, source: &Source) {
        match (self, source) {
            (Device::Ext(d), Source::Ext(s)) => d.set_selection(Some(s)),
            (Device::Wlr(d), Source::Wlr(s)) => d.set_selection(Some(s)),
            _ => unreachable!("a source of the device's own protocol"),
        }
    }
}

impl Source {
    fn offer(&self, mime: String) {
        match self {
            Source::Ext(s) => s.offer(mime),
            Source::Wlr(s) => s.offer(mime),
        }
    }

    fn destroy(&self) {
        match self {
            Source::Ext(s) => s.destroy(),
            Source::Wlr(s) => s.destroy(),
        }
    }
}

impl Offer {
    /// The MIME types it offers, as the compositor announced them before
    /// the offer became the selection.
    fn mimes(&self) -> Vec<String> {
        self.data()
            .map(|d| lock(&d.mimes).clone())
            .unwrap_or_default()
    }

    fn data(&self) -> Option<&OfferData> {
        match self {
            Offer::Ext(o) => o.data::<OfferData>(),
            Offer::Wlr(o) => o.data::<OfferData>(),
        }
    }

    /// Whether its owner marks it as not to be shared, asked of it once
    /// (`ask`).
    fn sensitive(
        &self,
        ask: impl FnOnce() -> Result<bool, PlatformError>,
    ) -> Result<bool, PlatformError> {
        if let Some(known) = self.data().and_then(|d| *lock(&d.sensitive)) {
            return Ok(known);
        }
        let sensitive = ask()?;
        if let Some(data) = self.data() {
            *lock(&data.sensitive) = Some(sensitive);
        }
        Ok(sensitive)
    }

    fn receive(&self, mime: &str, pipe: std::os::fd::BorrowedFd<'_>) {
        match self {
            Offer::Ext(o) => o.receive(mime.to_owned(), pipe),
            Offer::Wlr(o) => o.receive(mime.to_owned(), pipe),
        }
    }

    fn destroy(&self) {
        match self {
            Offer::Ext(o) => o.destroy(),
            Offer::Wlr(o) => o.destroy(),
        }
    }

    /// Whether it is `other`, the same object.
    fn is(&self, other: &Offer) -> bool {
        match (self, other) {
            (Offer::Ext(a), Offer::Ext(b)) => a.id() == b.id(),
            (Offer::Wlr(a), Offer::Wlr(b)) => a.id() == b.id(),
            _ => false,
        }
    }
}

/// The MIME types an offer announced, and whether it is marked as not to
/// be shared, once asked.
#[derive(Default)]
struct OfferData {
    mimes: Mutex<Vec<String>>,
    sensitive: Mutex<Option<bool>>,
}

/// What a source of this clipboard carries.
struct SourceData {
    serial: u64,
    owned: Owned,
}

/// Each protocol's events, heard the same way.
macro_rules! dispatch {
    ($kind:ident, $manager:ident::$Manager:ident, $device:ident::$Device:ident,
     $source:ident::$Source:ident, $offer:ident::$Offer:ident) => {
        impl Dispatch<$manager::$Manager, ()> for State {
            fn event(
                _: &mut Self,
                _: &$manager::$Manager,
                _: $manager::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }

        impl Dispatch<$device::$Device, ()> for State {
            fn event(
                state: &mut Self,
                _: &$device::$Device,
                event: $device::Event,
                _: &(),
                conn: &Connection,
                _: &QueueHandle<Self>,
            ) {
                match event {
                    $device::Event::Selection { id } => {
                        state.selection(id.map(Offer::$kind), conn)
                    }
                    // Not the clipboard: let go of at once.
                    $device::Event::PrimarySelection { id: Some(id) } => id.destroy(),
                    $device::Event::Finished => state.broken = Some("the data device ended"),
                    // Its MIME types come next, then the selection.
                    _ => {}
                }
            }

            wayland_client::event_created_child!(State, $device::$Device, [
                $device::EVT_DATA_OFFER_OPCODE => ($offer::$Offer, OfferData::default()),
            ]);
        }

        impl Dispatch<$offer::$Offer, OfferData> for State {
            fn event(
                _: &mut Self,
                _: &$offer::$Offer,
                event: $offer::Event,
                data: &OfferData,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let $offer::Event::Offer { mime_type } = event {
                    lock(&data.mimes).push(mime_type);
                }
            }
        }

        impl Dispatch<$source::$Source, SourceData> for State {
            fn event(
                state: &mut Self,
                source: &$source::$Source,
                event: $source::Event,
                data: &SourceData,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                match event {
                    $source::Event::Send { mime_type, fd } => send(data, mime_type, fd),
                    $source::Event::Cancelled => {
                        state.cancelled(data);
                        Source::$kind(source.clone()).destroy();
                    }
                    _ => {}
                }
            }
        }
    };
}

dispatch!(
    Ext,
    ext_manager::ExtDataControlManagerV1,
    ext_device::ExtDataControlDeviceV1,
    ext_source::ExtDataControlSourceV1,
    ext_offer::ExtDataControlOfferV1
);
dispatch!(
    Wlr,
    wlr_manager::ZwlrDataControlManagerV1,
    wlr_device::ZwlrDataControlDeviceV1,
    wlr_source::ZwlrDataControlSourceV1,
    wlr_offer::ZwlrDataControlOfferV1
);

// ---------------------------------------------------------------------
// What the thread and the clipboard share.

/// The selection, as last announced.
#[derive(Clone, Default)]
struct Seen {
    offer: Option<Offer>,
    mimes: Vec<String>,
    /// When it is this clipboard's: the source it names, and what that
    /// holds.
    ours: Option<(u64, Owned)>,
}

/// A source of this clipboard the compositor has not cancelled.
struct Live {
    owned: Owned,
    /// The write waiting for the compositor to announce it, until it did.
    write: Option<mpsc::Sender<u64>>,
    /// The write stopped waiting before it was announced.
    given_up: bool,
}

/// This clipboard's sources.
#[derive(Default)]
struct Sources {
    /// The serial of the newest source.
    serials: u64,
    live: HashMap<u64, Live>,
    /// The last one announced as the selection, with what it holds, until
    /// another app's selection is: a clipboard manager's copy of it is
    /// still it, and pastes from that copy may still need what it holds.
    current: Option<(u64, Owned)>,
    /// The selection was cleared to withdraw a promise: the next empty
    /// selection announced is that clear, no change. (The compositor
    /// handles the clear after whatever it had already, so what comes
    /// before it is not it.) A write since makes it count again: what it
    /// cleared then was in the way.
    clearing: bool,
}

/// What a selection announced is, for this clipboard.
struct Announced {
    /// This clipboard's: the source it names, and what that holds.
    ours: Option<(u64, Owned)>,
    /// The write waiting for it, which learns its count.
    write: Option<mpsc::Sender<u64>>,
    /// Another app's: a change.
    changed: bool,
    /// What this clipboard put there before, nothing offers any more.
    gone: Option<Owned>,
    /// A promise given up on was announced, and no newer source of this
    /// clipboard was set since: the selection is cleared, so nothing of it
    /// is offered. (A newer one replaces it by itself; a clear sent after
    /// it would wipe it out.)
    withdraw: bool,
}

/// The count a write that stopped waiting (`Sources::stop_waiting`) got
/// after all: on its way once announced, or come meanwhile, though a
/// clipboard manager took the selection over since (cancelled). Announced
/// is written, even if its count never comes (the thread went first):
/// then the next of `count`, a change.
fn counted(waited: &Waited, counts: &mpsc::Receiver<u64>, count: &AtomicU64) -> Option<u64> {
    match waited {
        Waited::Announced => Some(
            counts
                .recv()
                .unwrap_or_else(|_| count.fetch_add(1, Ordering::SeqCst) + 1),
        ),
        Waited::Cancelled | Waited::GaveUp => counts.try_recv().ok(),
    }
}

/// What a write found as it stopped waiting for the compositor.
#[derive(Debug, PartialEq)]
enum Waited {
    /// Announced as the wait ended: the thread took the write, and its
    /// count is on its way.
    Announced,
    /// The compositor cancelled it (another app took the selection), or the
    /// connection went, which let go of it.
    Cancelled,
    /// Neither: given up on. Announced later, it is a change.
    GaveUp,
}

impl Sources {
    /// A new source for `owned`, whose write waits on `write`, through the
    /// connection `alive` is of: none once that connection is gone, as its
    /// thread let go of what was here, and nothing would let go of it.
    /// (The thread says it is gone before it takes this lock to let go.)
    fn add(&mut self, alive: &AtomicBool, owned: Owned, write: mpsc::Sender<u64>) -> Option<u64> {
        if !alive.load(Ordering::SeqCst) {
            return None;
        }
        self.clearing = false;
        self.serials += 1;
        self.live.insert(
            self.serials,
            Live {
                owned,
                write: Some(write),
                given_up: false,
            },
        );
        Some(self.serials)
    }

    /// Everything this clipboard put there, let go of as its connection
    /// went.
    fn drain(&mut self) -> Vec<Owned> {
        let current = self.current.take().map(|(_, owned)| owned);
        self.live
            .drain()
            .map(|(_, live)| live.owned)
            .chain(current)
            .collect()
    }

    /// A selection was announced (`announced`), `empty` when it is none:
    /// the one a withdrawal's clear makes is no change.
    fn selection(&mut self, serial: Option<u64>, empty: bool) -> Announced {
        let cleared = empty && serial.is_none() && std::mem::take(&mut self.clearing);
        let mut announced = self.announced(serial);
        if cleared {
            announced.changed = false;
        }
        announced
    }

    /// The write of the source `serial` stops waiting for the compositor
    /// to announce it.
    /// Given up on, a promise ends here and now, so it is withdrawn
    /// however soon it is announced.
    fn stop_waiting(&mut self, serial: u64) -> Waited {
        match self.live.get_mut(&serial) {
            None => Waited::Cancelled,
            Some(live) => match live.write.take() {
                Some(_) => {
                    live.given_up = true;
                    live.owned.finish();
                    Waited::GaveUp
                }
                // Not given up on: it was written.
                None => Waited::Announced,
            },
        }
    }

    /// A selection naming the source `serial` of this clipboard (`None`
    /// for another app's) was announced. This clipboard's are its sources
    /// not cancelled, and the last one announced: a clipboard manager
    /// offering its copy of that is still it.
    fn announced(&mut self, serial: Option<u64>) -> Announced {
        let current = self.current.as_ref().map(|(serial, _)| *serial);
        let ours = serial.and_then(|serial| match self.live.get(&serial) {
            Some(live) => Some((serial, live.owned.clone())),
            None => self.current.clone().filter(|(s, _)| *s == serial),
        });
        let Some((serial, owned)) = &ours else {
            // Announced before the last write took the selection, it
            // leaves that write there.
            let gone = current
                .filter(|s| !self.live.contains_key(s))
                .and_then(|_| self.current.take())
                .map(|(_, held)| held);
            return Announced {
                ours: None,
                write: None,
                changed: true,
                gone,
                withdraw: false,
            };
        };
        // Announced after its write stopped waiting: a change, as the
        // agent took it for not written.
        let (write, late) = match self.live.get_mut(serial) {
            Some(live) => (live.write.take(), std::mem::take(&mut live.given_up)),
            None => (None, false),
        };
        let mut gone = None;
        if current != Some(*serial)
            && let Some((before, held)) = self.current.replace((*serial, owned.clone()))
            && !self.live.contains_key(&before)
        {
            gone = Some(held);
        }
        let withdraw = late && owned.ended() && *serial == self.serials;
        self.clearing |= withdraw;
        Announced {
            ours,
            write,
            changed: late,
            gone,
            withdraw,
        }
    }

    /// The compositor cancelled the source `serial`, which is the
    /// selection announced last if `showing`. Whether what it holds stays:
    /// the last one announced does, while still the selection, until what
    /// replaced it is announced (a manager's copy is still it, and files
    /// pasted from that copy are where the promise put them while it
    /// lasts). Any other is gone.
    fn cancelled(&mut self, serial: u64, showing: bool) -> bool {
        self.live.remove(&serial);
        let current = self.current.as_ref().is_some_and(|(s, _)| *s == serial);
        if current && !showing {
            self.current = None;
        }
        current && showing
    }
}

struct Shared {
    /// Changes every time the selection does, but not when this clipboard
    /// took it.
    count: AtomicU64,
    /// Names this clipboard's sources among others'.
    token: u64,
    seen: Mutex<Seen>,
    sources: Mutex<Sources>,
}

impl Shared {
    /// The own MIME type of the source `serial`.
    fn marker(&self, serial: u64) -> String {
        format!("{MARKER}{:016x}.{serial}", self.token)
    }

    /// The serial of the source of this clipboard `mime` names.
    fn serial_of(&self, mime: &str) -> Option<u64> {
        let (token, serial) = mime.strip_prefix(MARKER)?.split_once('.')?;
        (u64::from_str_radix(token, 16).ok()? == self.token)
            .then(|| serial.parse().ok())
            .flatten()
    }

    /// Lets go of what was put on the clipboard here, finishing promises.
    fn let_go(&self) {
        let gone = lock(&self.sources).drain();
        for owned in gone {
            owned.finish();
        }
        *lock(&self.seen) = Seen::default();
    }
}

/// A token unlikely to be another clipboard's: the time, the process and
/// how many came before in it.
fn token() -> u64 {
    static MADE: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let made = MADE.fetch_add(1, Ordering::Relaxed);
    nanos ^ (u64::from(std::process::id()) << 32) ^ made.rotate_right(7)
}

/// The thread's side: what it hears from the compositor.
struct State {
    shared: Arc<Shared>,
    /// Until the compositor described the selection as the connection
    /// opened: what it says then is not a change.
    opening: bool,
    /// Why the connection cannot go on, once it cannot.
    broken: Option<&'static str>,
    stopping: Arc<AtomicBool>,
    /// The compositor's globals (name, interface, version), as it listed
    /// them when the connection opened.
    globals: Vec<(u32, String, u32)>,
    /// The device, once there is one: to clear the selection with.
    device: Option<Device>,
}

impl State {
    /// A new selection: counted, unless it is this clipboard's (one of its
    /// sources not cancelled, or a clipboard manager's copy of the last
    /// announced); a write waiting for it learns its count. Another app's
    /// lets go of what this clipboard put there, once the compositor
    /// cancelled it.
    fn selection(&mut self, offer: Option<Offer>, conn: &Connection) {
        let mimes = offer.as_ref().map(Offer::mimes).unwrap_or_default();
        let serial = mimes.iter().find_map(|m| self.shared.serial_of(m));
        let announced = {
            let mut sources = lock(&self.shared.sources);
            let announced = sources.selection(serial, offer.is_none());
            // Asked for under the lock a write takes to make a newer
            // source, so its selection comes after the clear, not before.
            if announced.withdraw
                && let Some(device) = &self.device
            {
                device.clear();
            }
            announced
        };
        // What is there first, so a read right after a write returns sees
        // the write; then the counts.
        let seen = Seen {
            offer,
            mimes,
            ours: announced.ours,
        };
        let old = std::mem::replace(&mut *lock(&self.shared.seen), seen);
        // What it replaced is let go of before a write hears its count.
        if let Some(held) = announced.gone {
            held.finish();
        }
        let count = &self.shared.count;
        if let Some(write) = announced.write {
            let _ = write.send(count.fetch_add(1, Ordering::SeqCst) + 1);
        }
        if announced.changed && !self.opening {
            count.fetch_add(1, Ordering::SeqCst);
        }
        if announced.withdraw {
            let _ = conn.flush();
        }
        if let Some(old) = old.offer {
            old.destroy();
        }
    }

    /// The compositor will not ask the source for anything again: another
    /// app, a clipboard manager, or this clipboard again took the
    /// selection (`Sources::cancelled` says what stays).
    fn cancelled(&mut self, data: &SourceData) {
        let keep = {
            let mut sources = lock(&self.shared.sources);
            let showing = lock(&self.shared.seen)
                .ours
                .as_ref()
                .is_some_and(|(serial, _)| *serial == data.serial);
            sources.cancelled(data.serial, showing)
        };
        if !keep {
            data.owned.finish();
        }
    }
}

/// Answers an app's request for `mime`: at once as far as the pipe takes
/// it, which is all for most content; the rest, and a promise's content to
/// fetch first, on a thread of its own.
fn send(data: &SourceData, mime: String, pipe: OwnedFd) {
    // A few bytes, which fit any pipe: a clipboard manager may leave out
    // what holds nothing, and its copy would lose the marker.
    if mime.starts_with(MARKER) {
        let _ = std::fs::File::from(pipe).write_all(MARKER_ANSWER);
        return;
    }
    let Served::Waits(waits) = selection::serve_now(&data.owned, &mime, pipe) else {
        return;
    };
    let started = std::thread::Builder::new()
        .name("inputweave-wayland-clipboard-send".to_owned())
        .spawn(move || {
            waits.finish();
        });
    if let Err(e) = started {
        // The pipe went with the closure: the app reads nothing.
        warn!(error = %e, "cannot answer a request for the clipboard");
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            state.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
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

/// For `roundtrip_until`.
impl Dispatch<wl_callback::WlCallback, Arc<AtomicBool>> for State {
    fn event(
        _: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        done: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            done.store(true, Ordering::SeqCst);
        }
    }
}

/// Says the thread heard everything the compositor sent before.
struct Synced(Mutex<Option<mpsc::Sender<()>>>);

impl Dispatch<wl_callback::WlCallback, Synced> for State {
    fn event(
        _: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        synced: &Synced,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event
            && let Some(tx) = lock(&synced.0).take()
        {
            let _ = tx.send(());
        }
    }
}

// ---------------------------------------------------------------------
// The connection.

/// A connection to the compositor, followed by its thread.
struct Link {
    conn: Connection,
    qh: QueueHandle<State>,
    manager: Manager,
    device: Device,
    alive: Arc<AtomicBool>,
    /// Its thread let go of what was put on the clipboard through it.
    done: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    ended: Mutex<mpsc::Receiver<()>>,
}

impl Link {
    /// Connects, binds the data-control manager and a device for the
    /// seat, waits for the compositor to say what the selection is, then
    /// follows it on a thread; all within `CONNECT_WAIT`. `Unsupported`
    /// when the compositor offers no data-control.
    fn open(shared: Arc<Shared>) -> Result<Self, PlatformError> {
        let deadline = Instant::now() + CONNECT_WAIT;
        let conn = Connection::connect_to_env()
            .map_err(|e| os("cannot connect to the Wayland compositor", e))?;
        let mut queue = conn.new_event_queue::<State>();
        let qh = queue.handle();
        let registry = conn.display().get_registry(&qh, ());
        let stopping = Arc::new(AtomicBool::new(false));
        let mut state = State {
            shared,
            opening: true,
            broken: None,
            stopping: stopping.clone(),
            globals: Vec::new(),
            device: None,
        };
        roundtrip_until(&conn, &mut queue, &mut state, deadline)
            .map_err(|e| os("the compositor did not list its globals", e))?;
        let global = |interface: &wayland_client::backend::protocol::Interface| {
            state
                .globals
                .iter()
                .find(|(_, name, _)| name == interface.name)
                .map(|(name, _, _)| *name)
        };
        // Version 1: the primary selection is not followed.
        let manager = if let Some(name) = global(ext_manager::ExtDataControlManagerV1::interface())
        {
            Manager::Ext(registry.bind(name, 1, &qh, ()))
        } else if let Some(name) = global(wlr_manager::ZwlrDataControlManagerV1::interface()) {
            Manager::Wlr(registry.bind(name, 1, &qh, ()))
        } else {
            return Err(PlatformError::Unsupported(
                "data-control: the compositor offers no such protocol",
            ));
        };
        let seat = global(wl_seat::WlSeat::interface())
            .ok_or_else(|| PlatformError::Os("the Wayland compositor has no seat".to_owned()))?;
        let seat: wl_seat::WlSeat = registry.bind(seat, 1, &qh, ());
        let device = manager.device(&seat, &qh);
        state.device = Some(device.clone());
        roundtrip_until(&conn, &mut queue, &mut state, deadline)
            .map_err(|e| os("the compositor did not describe the clipboard", e))?;
        state.opening = false;
        let alive = Arc::new(AtomicBool::new(true));
        let done = Arc::new(AtomicBool::new(false));
        let (ended_tx, ended) = mpsc::channel();
        let ending = Ending {
            shared: state.shared.clone(),
            alive: alive.clone(),
            done: done.clone(),
            ended: ended_tx,
        };
        std::thread::Builder::new()
            .name("inputweave-wayland-clipboard".to_owned())
            .spawn(move || {
                let _ending = ending;
                follow(queue, state);
            })
            .map_err(|e| os("cannot start the clipboard thread", e))?;
        Ok(Self {
            conn,
            qh,
            manager,
            device,
            alive,
            done,
            stopping,
            ended: Mutex::new(ended),
        })
    }

    fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Sends what was asked of the compositor. A full socket is waited
    /// on, until `deadline`: what is left then goes out with the next
    /// flush, which is no failure. The connection lost is.
    fn flush(&self, deadline: Instant) -> Result<(), PlatformError> {
        loop {
            match self.conn.flush() {
                Ok(()) => return Ok(()),
                Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    let backend = self.conn.backend();
                    if left.is_zero() || !ready(&backend.poll_fd(), libc::POLLOUT, left) {
                        return Ok(());
                    }
                }
                Err(WaylandError::Io(e)) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(os("lost the Wayland compositor", e)),
            }
        }
    }

    /// Waits, at most `wait`, for the thread to hear everything the
    /// compositor sent before; whether it did.
    fn catch_up(&self, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        let (tx, rx) = mpsc::channel();
        self.conn
            .display()
            .sync(&self.qh, Synced(Mutex::new(Some(tx))));
        self.flush(deadline).is_ok()
            && rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_ok()
    }

    /// Asks `offer` for `mime`, up to `limit` bytes, by `deadline`.
    fn receive(
        &self,
        offer: &Offer,
        mime: &str,
        limit: usize,
        deadline: Instant,
    ) -> Result<Asked, PlatformError> {
        let (reader, writer) = std::io::pipe().map_err(|e| os("cannot make a pipe", e))?;
        offer.receive(mime, writer.as_fd());
        self.flush(deadline)?;
        // Only the app's copy is left, so the pipe ends as it is done.
        drop(writer);
        selection::read_pipe(reader, limit, ANSWER_WAIT, deadline)
            .map_err(|e| os("cannot read the clipboard", e))
    }

    /// Has the thread end, and waits for it a moment.
    fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        // Wakes it.
        let (tx, _rx) = mpsc::channel();
        self.conn
            .display()
            .sync(&self.qh, Synced(Mutex::new(Some(tx))));
        let _ = self.conn.flush();
        let _ = lock(&self.ended).recv_timeout(STOP_WAIT);
    }
}

/// Ends a link as its thread ends, whether it returns or panics: the link
/// is gone before what was put there through it is let go of, so a write
/// cut off by it knows the compositor went; only then, done, may another
/// connection follow the clipboard.
struct Ending {
    shared: Arc<Shared>,
    alive: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    ended: mpsc::Sender<()>,
}

impl Drop for Ending {
    fn drop(&mut self) {
        if std::thread::panicking() {
            warn!("the clipboard's thread failed; it connects again");
        }
        self.alive.store(false, Ordering::SeqCst);
        self.shared.let_go();
        self.done.store(true, Ordering::SeqCst);
        let _ = self.ended.send(());
    }
}

/// Follows the selection until the connection breaks or is let go of.
fn follow(mut queue: EventQueue<State>, mut state: State) {
    loop {
        if let Err(e) = queue.blocking_dispatch(&mut state) {
            warn!(error = %e, "the clipboard lost the Wayland compositor");
            break;
        }
        if let Some(why) = state.broken {
            warn!(why, "the clipboard lost the Wayland compositor");
            break;
        }
        if state.stopping.load(Ordering::SeqCst) {
            break;
        }
    }
}

// ---------------------------------------------------------------------
// The clipboard.

#[derive(Default)]
struct Slot {
    link: Option<Arc<Link>>,
    /// A connection is being opened, on a thread of its own.
    opening: bool,
    /// The compositor offers no data-control.
    unsupported: bool,
    /// The clipboard was dropped: a connection opened now is closed again.
    closed: bool,
    backoff: Backoff,
}

/// Clears `Slot::opening` should the connecting thread panic (it clears
/// it itself as it stores what it opened), so the next use tries again,
/// once the backoff allows.
struct Opening(Arc<Mutex<Slot>>);

impl Drop for Opening {
    fn drop(&mut self) {
        if std::thread::panicking() {
            warn!("connecting the clipboard failed; it is tried again");
            let mut slot = lock(&self.0);
            slot.opening = false;
            slot.backoff.failed();
        }
    }
}

fn lost() -> PlatformError {
    PlatformError::Os("lost the Wayland compositor while writing the clipboard".to_owned())
}

fn connecting() -> PlatformError {
    PlatformError::Unavailable("connecting the clipboard to the Wayland compositor")
}

/// The clipboard of a compositor with data-control.
pub struct DataControlClipboard {
    shared: Arc<Shared>,
    slot: Arc<Mutex<Slot>>,
}

impl Default for DataControlClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl DataControlClipboard {
    /// The clipboard of the compositor `WAYLAND_DISPLAY` names, connected
    /// to as it is first used, on a thread of its own: `Unavailable` until
    /// then, `Unsupported` once the compositor turned out to offer no
    /// data-control, as GNOME does not.
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                count: AtomicU64::new(1),
                token: token(),
                seen: Mutex::new(Seen::default()),
                sources: Mutex::new(Sources::default()),
            }),
            slot: Arc::new(Mutex::new(Slot::default())),
        }
    }

    /// The connection. Without one (none yet, or it broke: what was on the
    /// clipboard from here went with it, which counts as a change), one is
    /// opened on another thread, no sooner than its backoff allows, and the
    /// call is `Unavailable`; no call waits for the compositor. A broken
    /// one's thread lets go of what was put there through it, and a new one
    /// opens only once it did, so nothing of the new one is let go of.
    fn link(&self) -> Result<Arc<Link>, PlatformError> {
        let mut slot = lock(&self.slot);
        if let Some(link) = &slot.link {
            if link.alive() {
                return Ok(link.clone());
            }
            if !link.done.load(Ordering::SeqCst) {
                return Err(connecting());
            }
            slot.link = None;
            self.shared.count.fetch_add(1, Ordering::SeqCst);
            slot.backoff.succeeded();
        }
        // A compositor that had no data-control is asked again, as often
        // as the backoff allows: what it said holds meanwhile.
        let said = |slot: &Slot| match slot.unsupported {
            true => {
                PlatformError::Unsupported("data-control: the compositor offers no such protocol")
            }
            false => connecting(),
        };
        if slot.opening || !slot.backoff.ready() {
            return Err(said(&slot));
        }
        slot.opening = true;
        let (shared, opened) = (self.shared.clone(), self.slot.clone());
        let opening = Opening(self.slot.clone());
        let started = std::thread::Builder::new()
            .name("inputweave-wayland-clipboard-connect".to_owned())
            .spawn(move || {
                let _opening = opening;
                let link = Link::open(shared);
                let mut slot = lock(&opened);
                slot.opening = false;
                match link {
                    Ok(link) if slot.closed => {
                        drop(slot);
                        link.stop();
                    }
                    Ok(link) => {
                        info!(
                            protocol = link.manager.name(),
                            "the clipboard follows the Wayland compositor's through data-control"
                        );
                        slot.link = Some(Arc::new(link));
                        slot.unsupported = false;
                        slot.backoff.succeeded();
                    }
                    Err(PlatformError::Unsupported(_)) => {
                        slot.unsupported = true;
                        slot.backoff.failed();
                    }
                    Err(e) => {
                        slot.backoff.failed();
                        debug!(error = %e, "cannot connect the clipboard to the Wayland compositor");
                    }
                }
            });
        if let Err(e) = started {
            slot.opening = false;
            slot.backoff.failed();
            warn!(error = %e, "cannot start connecting the clipboard");
        }
        Err(said(&slot))
    }

    /// Fails a read of `offer` that another selection replaced meanwhile:
    /// what is left of a replaced offer reads as nothing, so what was read
    /// may be part of it only. (The change count moved too, which the
    /// agent checks around a read; X11 leaves it to that.)
    fn unchanged(&self, offer: &Offer) -> Result<(), PlatformError> {
        let seen = lock(&self.shared.seen);
        if seen.offer.as_ref().is_some_and(|o| o.is(offer)) {
            Ok(())
        } else {
            Err(PlatformError::Unavailable(
                "the clipboard changed while it was read",
            ))
        }
    }

    /// The selection the thread heard of last, with the connection to ask
    /// it through.
    fn seen(&self) -> Result<(Arc<Link>, Seen), PlatformError> {
        let link = self.link()?;
        let seen = lock(&self.shared.seen).clone();
        Ok((link, seen))
    }

    /// Takes the selection for `owned` through `link` with a new source,
    /// and waits for the compositor to announce it. Not announced in time,
    /// the content stays, as the compositor may still announce it; another
    /// app taking the selection first, neither is there; the compositor
    /// lost meanwhile, it is a failure, not something copied over it.
    fn own(&mut self, link: Arc<Link>, owned: Owned) -> Result<u64, PlatformError> {
        let (tx, rx) = mpsc::channel();
        let added = lock(&self.shared.sources).add(&link.alive, owned.clone(), tx);
        let Some(serial) = added else {
            // The connection went since it was looked up.
            owned.finish();
            return Err(lost());
        };
        let mimes = owned.mimes(Wire::Wayland);
        let source = link.manager.source(
            &link.qh,
            SourceData {
                serial,
                owned: owned.clone(),
            },
        );
        for mime in mimes {
            source.offer(mime.to_owned());
        }
        source.offer(self.shared.marker(serial));
        link.device.set_selection(&source);
        let deadline = Instant::now() + WRITE_WAIT;
        if let Err(e) = link.flush(deadline) {
            // Nothing of it reaches the clipboard.
            lock(&self.shared.sources).live.remove(&serial);
            owned.finish();
            return Err(e);
        }
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(count) => Ok(count),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let waited = lock(&self.shared.sources).stop_waiting(serial);
                if let Some(count) = counted(&waited, &rx, &self.shared.count) {
                    return Ok(count);
                }
                if waited == Waited::Cancelled && link.alive() {
                    return Err(PlatformError::Unavailable("another app took the clipboard"));
                }
                // Given up on, a promise ends now: the agent lets its offer
                // go, and nothing of it is given.
                owned.finish();
                if !link.alive() {
                    return Err(lost());
                }
                Err(PlatformError::Unavailable(
                    "the Wayland compositor did not answer in time",
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) if !link.alive() => Err(lost()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(PlatformError::Unavailable("another app took the clipboard"))
            }
        }
    }
}

impl Drop for DataControlClipboard {
    fn drop(&mut self) {
        let link = {
            let mut slot = lock(&self.slot);
            slot.closed = true;
            slot.link.take()
        };
        if let Some(link) = link {
            link.stop();
        }
    }
}

impl Clipboard for DataControlClipboard {
    /// The changes the thread heard of, once it heard what the compositor
    /// sent before.
    fn change_count(&self) -> Result<u64, PlatformError> {
        self.link()?.catch_up(SYNC_WAIT);
        Ok(self.shared.count.load(Ordering::SeqCst))
    }

    fn read(&self, max_len: usize) -> Result<ClipboardRead, PlatformError> {
        let (link, seen) = self.seen()?;
        if let Some((_, owned)) = &seen.ours {
            return Ok(owned.read_back(max_len));
        }
        let Some(offer) = seen.offer else {
            return Ok(ClipboardRead::Empty);
        };
        let offered: Vec<&str> = seen.mimes.iter().map(String::as_str).collect();
        let deadline = Instant::now() + READ_WAIT;
        let ask = |mime: &str, limit| link.receive(&offer, mime, limit, deadline);
        if offer.sensitive(|| selection::sensitive(&offered, ask))? {
            return Ok(ClipboardRead::Sensitive);
        }
        let read = selection::read_offered(Wire::Wayland, &offered, max_len, ask)?;
        self.unchanged(&offer)?;
        Ok(read)
    }

    fn write(&mut self, content: &ClipboardContent) -> Result<u64, PlatformError> {
        let link = self.link()?;
        self.own(link, Owned::Content(Arc::new(Written::new(content))))
    }

    /// The promise is taken, and its source told when it is gone, unless
    /// the compositor has no data-control (`Unsupported`): then it is the
    /// caller's still, to put on another clipboard (`WaylandClipboard`
    /// hands it to the portal).
    fn write_promise(&mut self, promise: ClipboardPromise) -> Result<u64, PlatformError> {
        let link = match self.link() {
            Ok(link) => link,
            Err(e @ PlatformError::Unsupported(_)) => return Err(e),
            Err(e) => {
                promise.source.finished();
                return Err(e);
            }
        };
        self.own(link, Owned::Promise(Arc::new(Promised::new(promise))))
    }

    /// The files GNOME's or KDE's file manager copied, by their lists of
    /// URIs. None when the content is sensitive or was put there here.
    fn files(&self) -> Result<Vec<PathBuf>, PlatformError> {
        let (link, seen) = self.seen()?;
        let Some(offer) = seen.offer.filter(|_| seen.ours.is_none()) else {
            return Ok(Vec::new());
        };
        let offered: Vec<&str> = seen.mimes.iter().map(String::as_str).collect();
        let deadline = Instant::now() + READ_WAIT;
        let ask = |mime: &str, limit| link.receive(&offer, mime, limit, deadline);
        if offer.sensitive(|| selection::sensitive(&offered, ask))? {
            return Ok(Vec::new());
        }
        let files = selection::files_offered(&offered, ask)?;
        self.unchanged(&offer)?;
        Ok(files)
    }

    fn promises_files(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::{ClipboardFormat, ClipboardSource};

    /// A promise's source, counting how often it was told it is gone.
    #[derive(Default)]
    struct Gone(AtomicUsize);

    impl ClipboardSource for Gone {
        fn fetch(&self, _: ClipboardFormat) -> Option<Vec<u8>> {
            None
        }

        fn finished(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn promised(source: &Arc<Gone>) -> Owned {
        Owned::Promise(Arc::new(Promised::new(ClipboardPromise {
            formats: vec![ClipboardFormat::Text],
            bytes: 1,
            files: Vec::new(),
            source: source.clone(),
        })))
    }

    fn written(text: &str) -> Owned {
        Owned::Content(Arc::new(Written::new(&ClipboardContent {
            items: vec![(ClipboardFormat::Text, text.as_bytes().to_vec())],
        })))
    }

    fn text(owned: &Owned) -> String {
        match owned.read_back(100) {
            ClipboardRead::Content(c) => {
                String::from_utf8(c.get(ClipboardFormat::Text).unwrap().to_vec()).unwrap()
            }
            other => format!("{other:?}"),
        }
    }

    fn live(sources: &mut Sources, serial: u64, owned: Owned) -> mpsc::Receiver<u64> {
        let (tx, rx) = mpsc::channel();
        sources.serials = sources.serials.max(serial);
        sources.live.insert(
            serial,
            Live {
                owned,
                write: Some(tx),
                given_up: false,
            },
        );
        rx
    }

    #[test]
    fn what_is_read_back_is_what_the_announced_source_holds() {
        let mut sources = Sources::default();
        let _first = live(&mut sources, 1, written("first"));
        let announced = sources.announced(Some(1));
        assert!(!announced.changed && announced.write.is_some());
        assert_eq!(text(&announced.ours.unwrap().1), "first");
        // A newer write the compositor has not announced (it may never):
        // the selection is still the first.
        let _second = live(&mut sources, 2, written("second"));
        assert_eq!(text(&sources.announced(Some(1)).ours.unwrap().1), "first");
        // Announced, it is the second; the first is still offered until
        // cancelled.
        let announced = sources.announced(Some(2));
        assert_eq!(text(&announced.ours.unwrap().1), "second");
        assert!(announced.gone.is_none());
        assert!(!sources.cancelled(1, false));
        // Another app's, and what is not this clipboard's.
        assert!(sources.announced(None).changed);
        assert!(sources.announced(Some(7)).changed);
    }

    #[test]
    fn a_write_stopping_waiting_learns_what_became_of_it() {
        let mut sources = Sources::default();
        // Nothing yet: given up on.
        let _write = live(&mut sources, 1, written("a"));
        assert_eq!(sources.stop_waiting(1), Waited::GaveUp);
        assert!(sources.announced(Some(1)).changed);
        // Announced as the wait ended.
        let write = live(&mut sources, 2, written("b"));
        let announced = sources.announced(Some(2));
        announced.write.unwrap().send(5).unwrap();
        assert_eq!(sources.stop_waiting(2), Waited::Announced);
        assert_eq!(write.recv(), Ok(5));
        // Cancelled as the wait ended: another app took the selection.
        let _write = live(&mut sources, 3, written("c"));
        sources.cancelled(3, false);
        assert_eq!(sources.stop_waiting(3), Waited::Cancelled);
    }

    #[test]
    fn a_promise_given_up_on_is_withdrawn_however_soon_it_is_announced() {
        let source = Arc::new(Gone::default());
        let mut sources = Sources::default();
        let _write = live(&mut sources, 1, promised(&source));
        // The compositor announces it right after the write stopped
        // waiting.
        assert_eq!(sources.stop_waiting(1), Waited::GaveUp);
        assert_eq!(source.0.load(Ordering::SeqCst), 1);
        let announced = sources.announced(Some(1));
        assert!(announced.changed && announced.withdraw);
    }

    #[test]
    fn a_withdrawal_clears_only_what_nothing_newer_replaced() {
        // Given up on, announced late: the newest, so cleared; the clear,
        // announced next, is no change.
        let source = Arc::new(Gone::default());
        let mut sources = Sources::default();
        let _write = live(&mut sources, 1, promised(&source));
        sources.stop_waiting(1);
        let late = sources.selection(Some(1), false);
        assert!(late.changed && late.withdraw);
        assert!(!sources.selection(None, true).changed);
        // Another app's empty selection after it is a change again.
        assert!(sources.selection(None, true).changed);

        // A newer write set since: it replaces the promise by itself, and
        // a clear would wipe it out.
        let source = Arc::new(Gone::default());
        let mut sources = Sources::default();
        let _write = live(&mut sources, 1, promised(&source));
        sources.stop_waiting(1);
        let _newer = live(&mut sources, 2, written("newer"));
        let late = sources.selection(Some(1), false);
        assert!(late.changed && !late.withdraw);
        assert!(sources.selection(None, true).changed);

        // Another app's copy, or a manager's of the promise, announced
        // before the clear: the clear is still the next empty one.
        for before in [None, Some(1)] {
            let source = Arc::new(Gone::default());
            let mut sources = Sources::default();
            let _write = live(&mut sources, 1, promised(&source));
            sources.stop_waiting(1);
            assert!(sources.selection(Some(1), false).withdraw);
            sources.selection(before, false);
            assert!(!sources.selection(None, true).changed);
        }

        // Written again since: the clear counts.
        let source = Arc::new(Gone::default());
        let mut sources = Sources::default();
        let _write = live(&mut sources, 1, promised(&source));
        sources.stop_waiting(1);
        assert!(sources.selection(Some(1), false).withdraw);
        let (tx, _rx) = mpsc::channel();
        sources.add(&AtomicBool::new(true), written("again"), tx);
        assert!(sources.selection(None, true).changed);
    }

    fn shared() -> Arc<Shared> {
        Arc::new(Shared {
            count: AtomicU64::new(1),
            token: 1,
            seen: Mutex::new(Seen::default()),
            sources: Mutex::new(Sources::default()),
        })
    }

    #[test]
    fn a_thread_that_panics_ends_its_link_all_the_same() {
        // The follow thread: gone, what was put there let go of, done.
        let shared = shared();
        let source = Arc::new(Gone::default());
        let (tx, _rx) = mpsc::channel();
        let alive = Arc::new(AtomicBool::new(true));
        let _ = lock(&shared.sources).add(&alive, promised(&source), tx);
        let done = Arc::new(AtomicBool::new(false));
        let (ended_tx, ended) = mpsc::channel();
        let ending = Ending {
            shared: shared.clone(),
            alive: alive.clone(),
            done: done.clone(),
            ended: ended_tx,
        };
        let thread = std::thread::spawn(move || {
            let _ending = ending;
            panic!("in the clipboard's thread");
        });
        assert!(thread.join().is_err());
        assert!(!alive.load(Ordering::SeqCst) && done.load(Ordering::SeqCst));
        assert_eq!(source.0.load(Ordering::SeqCst), 1);
        assert!(lock(&shared.sources).live.is_empty());
        assert_eq!(ended.try_recv(), Ok(()));

        // The connecting thread: no longer opening, tried again later.
        let slot = Arc::new(Mutex::new(Slot {
            opening: true,
            ..Slot::default()
        }));
        let opening = Opening(slot.clone());
        let thread = std::thread::spawn(move || {
            let _opening = opening;
            panic!("connecting");
        });
        assert!(thread.join().is_err());
        assert!(!lock(&slot).opening);
        assert!(!lock(&slot).backoff.ready());
        // Returning, it leaves `opening` to the thread.
        lock(&slot).opening = true;
        drop(Opening(slot.clone()));
        assert!(lock(&slot).opening);
    }

    #[test]
    fn a_write_announced_whose_count_never_came_is_written() {
        let count = AtomicU64::new(7);
        let (tx, rx) = mpsc::channel::<u64>();
        drop(tx);
        assert_eq!(counted(&Waited::Announced, &rx, &count), Some(8));
        assert_eq!(count.load(Ordering::SeqCst), 8);
        assert_eq!(counted(&Waited::GaveUp, &rx, &count), None);
    }

    #[test]
    fn a_write_announced_as_its_wait_ends_is_written() {
        // Announced, then stopped waiting: written, not given up on, so a
        // manager's copy of it announced later is no change.
        let mut sources = Sources::default();
        let rx = live(&mut sources, 1, written("a"));
        sources
            .selection(Some(1), false)
            .write
            .unwrap()
            .send(3)
            .unwrap();
        let waited = sources.stop_waiting(1);
        assert_eq!(waited, Waited::Announced);
        assert_eq!(counted(&waited, &rx, &AtomicU64::new(0)), Some(3));
        assert!(!sources.selection(Some(1), false).changed);

        // Announced, then taken over by a manager (cancelled), both before
        // it stopped waiting: written all the same.
        let mut sources = Sources::default();
        let rx = live(&mut sources, 1, written("b"));
        sources
            .selection(Some(1), false)
            .write
            .unwrap()
            .send(4)
            .unwrap();
        assert!(sources.cancelled(1, true));
        let waited = sources.stop_waiting(1);
        assert_eq!(waited, Waited::Cancelled);
        assert_eq!(counted(&waited, &rx, &AtomicU64::new(0)), Some(4));

        // Cancelled before it was announced: another app took it.
        let mut sources = Sources::default();
        let rx = live(&mut sources, 1, written("c"));
        sources.cancelled(1, false);
        let waited = sources.stop_waiting(1);
        assert_eq!(counted(&waited, &rx, &AtomicU64::new(0)), None);
    }

    #[test]
    fn a_source_added_as_its_connection_goes_is_let_go_of() {
        let alive = AtomicBool::new(true);
        let mut sources = Sources::default();
        let (tx, _rx) = mpsc::channel();
        assert_eq!(sources.add(&alive, written("a"), tx), Some(1));
        // The thread says it is gone, then lets go: of that one too.
        alive.store(false, Ordering::SeqCst);
        assert_eq!(sources.drain().len(), 1);
        // Added after: refused, so nothing is left that nothing lets go of.
        let source = Arc::new(Gone::default());
        let (tx, _rx) = mpsc::channel();
        assert_eq!(sources.add(&alive, promised(&source), tx), None);
        assert!(sources.live.is_empty() && sources.current.is_none());
    }

    #[test]
    fn a_promise_is_finished_once_unless_it_is_handed_on() {
        // No connection yet (one is being opened): taken, and let go of at
        // once.
        let source = Arc::new(Gone::default());
        let mut clipboard = DataControlClipboard::new();
        lock(&clipboard.slot).opening = true;
        let promise = ClipboardPromise {
            formats: vec![ClipboardFormat::Text],
            bytes: 1,
            files: Vec::new(),
            source: source.clone(),
        };
        assert!(matches!(
            clipboard.write_promise(promise.clone()),
            Err(PlatformError::Unavailable(_))
        ));
        assert_eq!(source.0.load(Ordering::SeqCst), 1);
        // No data-control: the caller's still.
        let source = Arc::new(Gone::default());
        lock(&clipboard.slot).unsupported = true;
        let promise = ClipboardPromise {
            source: source.clone(),
            ..promise
        };
        assert!(matches!(
            clipboard.write_promise(promise),
            Err(PlatformError::Unsupported(_))
        ));
        assert_eq!(source.0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_cancelled_source_wakes_its_write() {
        let mut sources = Sources::default();
        let write = live(&mut sources, 1, written("x"));
        assert!(!sources.cancelled(1, false));
        assert_eq!(write.try_recv(), Err(mpsc::TryRecvError::Disconnected));
    }

    #[test]
    fn a_write_given_up_on_and_announced_later_is_a_change() {
        let mut sources = Sources::default();
        let _write = live(&mut sources, 1, written("late"));
        sources.live.get_mut(&1).unwrap().given_up = true;
        let announced = sources.announced(Some(1));
        assert!(announced.changed && !announced.withdraw);
        assert_eq!(text(&announced.ours.unwrap().1), "late");
        // Once.
        assert!(!sources.announced(Some(1)).changed);

        // A promise given up on is ended: withdrawn when announced.
        let source = Arc::new(Gone::default());
        let owned = promised(&source);
        let _write = live(&mut sources, 2, owned.clone());
        sources.live.get_mut(&2).unwrap().given_up = true;
        owned.finish();
        let announced = sources.announced(Some(2));
        assert!(announced.changed && announced.withdraw);
    }

    #[test]
    fn a_managers_copy_of_the_last_write_keeps_its_promise() {
        let source = Arc::new(Gone::default());
        let mut sources = Sources::default();
        let _write = live(&mut sources, 1, promised(&source));
        sources.announced(Some(1));
        // A manager takes it over: cancelled while still the selection.
        assert!(sources.cancelled(1, true));
        let copy = sources.announced(Some(1));
        assert!(!copy.changed && copy.ours.is_some() && copy.gone.is_none());
        // Another app's copy ends it.
        let theirs = sources.announced(None);
        assert!(theirs.changed);
        theirs.gone.unwrap().finish();
        assert_eq!(source.0.load(Ordering::SeqCst), 1);

        // A newer write announced after a manager's copy ends the older.
        let source = Arc::new(Gone::default());
        let _write = live(&mut sources, 2, promised(&source));
        sources.announced(Some(2));
        assert!(sources.cancelled(2, true));
        sources.announced(Some(2));
        let _newer = live(&mut sources, 3, written("newer"));
        sources.announced(Some(3)).gone.unwrap().finish();
        assert_eq!(source.0.load(Ordering::SeqCst), 1);

        // Cancelled once something else was announced: gone at once.
        let mut sources = Sources::default();
        let _write = live(&mut sources, 1, written("x"));
        sources.announced(Some(1));
        assert!(!sources.cancelled(1, false));
        assert!(sources.current.is_none());
    }

    #[test]
    fn markers_name_this_clipboard_and_the_source() {
        let shared = Shared {
            count: AtomicU64::new(0),
            token: 0xfeed,
            seen: Mutex::new(Seen::default()),
            sources: Mutex::new(Sources::default()),
        };
        let marker = shared.marker(12);
        assert_eq!(
            marker,
            "application/x-inputweave-source.000000000000feed.12"
        );
        assert_eq!(shared.serial_of(&marker), Some(12));
        let other = Shared {
            token: 0xbeef,
            ..shared
        };
        assert_eq!(other.serial_of(&marker), None);
        assert_eq!(other.serial_of("text/plain"), None);
        assert_eq!(
            other.serial_of("application/x-inputweave-source.zz.1"),
            None
        );
        assert_ne!(token(), token());
    }
}
