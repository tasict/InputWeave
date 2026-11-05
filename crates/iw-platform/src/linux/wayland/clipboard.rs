//! Which Wayland clipboard the agent uses: data-control (`data_control`) where the compositor offers it, as
//! it asks nobody and works whether or not remote control was allowed;
//! else (GNOME) the Clipboard portal (`portal_clipboard`), which the
//! desktop shares with a remote desktop session only.
//!
//! Which one it is is known once the compositor answered data-control,
//! which connects on a thread of its own: until then, as at a busy login,
//! each use asks again (as often as data-control's backoff allows), and is
//! `Unavailable`. A compositor that turns out to have no data-control,
//! even later, as on connecting again, has the portal used; data-control
//! is asked again at most once a minute (`REPROBE`), and used again once
//! it answers, as after a restart into a compositor that has it: within
//! two minutes, as its first answer comes from a connection started then.
//! Turning back so, the portal lets go of what it put on the clipboard
//! (`LetsGo`): promises finished, and withdrawn while the session runs.
//! Every remote desktop session asks for the portal's clipboard whatever
//! this says, so it is there for such a turn, in the session that runs;
//! the portal goes unused while data-control works.
//!
//! The change count is this clipboard's own, made of the one in use's: it
//! only goes up, across a turn as well, so a count from one is never taken
//! for the other's; a turn moves it on, so the agent reads the clipboard
//! again.
//!
//! A promise's source is told it is gone exactly once, whatever way it
//! goes: by the clipboard that took it (each does, failing too), except
//! that data-control without the protocol leaves it to be handed to the
//! portal; and here, when no clipboard was handed it at all.
//!
//! Free of FFI, so it is tested on every Unix.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::linux::common::lock;
use crate::{Clipboard, ClipboardContent, ClipboardPromise, ClipboardRead, PlatformError};

/// A clipboard `WaylandClipboard` may turn away from.
pub(crate) trait LetsGo: Clipboard {
    /// The other is used from now on: lets go of what was put on the
    /// clipboard through this one, finishing promises, and withdraws it
    /// where it can.
    fn let_go(&mut self);
}

/// Which clipboard is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Chosen {
    /// The compositor has not said whether it offers data-control.
    Unknown,
    DataControl,
    Portal,
}

/// The change count given out, made of the one in use's.
struct Counts {
    /// The clipboard whose counts `offset` is for.
    of: Chosen,
    /// What is added to its counts.
    offset: u64,
    /// The highest count given out.
    highest: u64,
}

/// How often data-control is asked again while the portal is used.
const REPROBE: Duration = Duration::from_secs(60);

fn unknown() -> PlatformError {
    PlatformError::Unavailable("the Wayland compositor has not said how to reach its clipboard")
}

/// The Wayland clipboard: data-control's, or the portal's.
pub(crate) struct WaylandClipboard(Mutex<Inner>, Arc<Which>);

/// Which clipboard is used, as last decided, read without waiting for a
/// call in progress (`WaylandClipboard::which`).
#[derive(Debug, Default)]
pub(crate) struct Which(AtomicU8);

impl Which {
    pub(crate) fn get(&self) -> Chosen {
        match self.0.load(Ordering::Relaxed) {
            1 => Chosen::DataControl,
            2 => Chosen::Portal,
            _ => Chosen::Unknown,
        }
    }

    fn set(&self, chosen: Chosen) {
        let value = match chosen {
            Chosen::Unknown => 0,
            Chosen::DataControl => 1,
            Chosen::Portal => 2,
        };
        self.0.store(value, Ordering::Relaxed);
    }
}

struct Inner {
    chosen: Chosen,
    counts: Counts,
    data_control: Box<dyn Clipboard>,
    portal: Box<dyn LetsGo>,
    /// When data-control is asked again, while the portal is used.
    probe_at: Instant,
    reprobe: Duration,
}

impl WaylandClipboard {
    /// Asks nothing yet: data-control connects as it is first used.
    pub(crate) fn new(data_control: Box<dyn Clipboard>, portal: Box<dyn LetsGo>) -> Self {
        Self(
            Mutex::new(Inner {
                chosen: Chosen::Unknown,
                counts: Counts {
                    of: Chosen::Unknown,
                    offset: 0,
                    highest: 0,
                },
                data_control,
                portal,
                probe_at: Instant::now(),
                reprobe: REPROBE,
            }),
            Arc::default(),
        )
    }

    /// Which clipboard is used, kept up to date as it is decided.
    pub(crate) fn which(&self) -> Arc<Which> {
        self.1.clone()
    }

    #[cfg(test)]
    fn reprobing_every(self, reprobe: Duration) -> Self {
        lock(&self.0).reprobe = reprobe;
        self
    }

    #[cfg(test)]
    fn chosen(&self) -> Chosen {
        lock(&self.0).chosen
    }

    /// What `call` gives of the clipboard used, as `route` has it.
    fn route<T>(
        &self,
        call: impl FnMut(&mut dyn Clipboard) -> Result<T, PlatformError>,
    ) -> Result<T, PlatformError> {
        let mut inner = lock(&self.0);
        let result = inner.route(call).map(|(_, t)| t);
        self.1.set(inner.chosen);
        result
    }

    /// The count `call` gives of the clipboard used, as this clipboard
    /// gives it out (`Inner::counted`).
    fn counting(
        &self,
        call: impl FnMut(&mut dyn Clipboard) -> Result<u64, PlatformError>,
    ) -> Result<u64, PlatformError> {
        let mut inner = lock(&self.0);
        let routed = inner.route(call);
        self.1.set(inner.chosen);
        let (of, count) = routed?;
        Ok(inner.counted(of, count))
    }
}

impl Inner {
    /// Data-control is not asked again for a while.
    fn probe_later(&mut self) {
        self.probe_at = Instant::now() + self.reprobe;
    }

    /// What `call` gives of the clipboard used, and which that is:
    /// data-control's, once it answered, unless it turns out to have none
    /// (`Unsupported`, which it is only before it takes what it is given),
    /// then the portal's, until data-control, asked again now and then,
    /// answers. While data-control has not answered, it is asked again,
    /// and the call is `Unavailable`.
    fn route<T>(
        &mut self,
        mut call: impl FnMut(&mut dyn Clipboard) -> Result<T, PlatformError>,
    ) -> Result<(Chosen, T), PlatformError> {
        if self.chosen == Chosen::Portal && Instant::now() >= self.probe_at {
            match self.data_control.change_count() {
                Ok(_) => {
                    tracing::info!("data-control answers; the clipboard goes through it again");
                    self.chosen = Chosen::DataControl;
                    self.portal.let_go();
                }
                // What it says is from a connection started now, if any:
                // heard next time.
                Err(_) => self.probe_later(),
            }
        }
        if self.chosen != Chosen::Portal {
            let answered = match self.chosen {
                Chosen::Unknown => self.data_control.change_count().map(|_| {
                    tracing::info!("the clipboard goes through data-control");
                    self.chosen = Chosen::DataControl;
                }),
                _ => Ok(()),
            };
            match answered.and_then(|()| call(self.data_control.as_mut())) {
                Err(PlatformError::Unsupported(_)) => {
                    tracing::info!("no data-control; the clipboard is shared with remote control");
                    self.chosen = Chosen::Portal;
                    self.probe_later();
                }
                Err(_) if self.chosen == Chosen::Unknown => return Err(unknown()),
                result => return result.map(|t| (Chosen::DataControl, t)),
            }
        }
        call(self.portal.as_mut()).map(|t| (Chosen::Portal, t))
    }

    /// `count` of `of` as this clipboard gives it out: above every count
    /// given out before `of` was used.
    fn counted(&mut self, of: Chosen, count: u64) -> u64 {
        let counts = &mut self.counts;
        if counts.of != of {
            counts.of = of;
            counts.offset = (counts.highest + 1).saturating_sub(count);
        }
        let given = count + counts.offset;
        counts.highest = counts.highest.max(given);
        given
    }
}

impl Clipboard for WaylandClipboard {
    fn change_count(&self) -> Result<u64, PlatformError> {
        self.counting(|c| c.change_count())
    }

    fn read(&self, max_len: usize) -> Result<ClipboardRead, PlatformError> {
        self.route(|c| c.read(max_len))
    }

    fn write(&mut self, content: &ClipboardContent) -> Result<u64, PlatformError> {
        self.counting(|c| c.write(content))
    }

    fn write_promise(&mut self, promise: ClipboardPromise) -> Result<u64, PlatformError> {
        let mut handed = false;
        let written = self.counting(|c| {
            handed = true;
            c.write_promise(promise.clone())
        });
        if written.is_err() && !handed {
            promise.source.finished();
        }
        written
    }

    fn files(&self) -> Result<Vec<PathBuf>, PlatformError> {
        self.route(|c| c.files())
    }

    /// Both promise files.
    fn promises_files(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// What a `Fake` says when asked.
    #[derive(Clone, Copy)]
    enum Says {
        Count(u64),
        Unsupported,
        OutOfReach,
    }

    /// A clipboard that says as told, and notes what it was asked. A
    /// promise it is given it keeps, written or not, as data-control and
    /// the portal do, unless it says `Unsupported`.
    #[derive(Clone, Default)]
    struct Fake {
        /// What it says next, in order; the last stays.
        says: Arc<Mutex<Vec<Says>>>,
        asked: Arc<Mutex<Vec<&'static str>>>,
        promises: Arc<Mutex<Vec<ClipboardPromise>>>,
    }

    impl Fake {
        fn saying(says: &[Says]) -> Self {
            let fake = Self::default();
            fake.say(says);
            fake
        }

        fn say(&self, says: &[Says]) {
            *self.says.lock().unwrap() = says.to_vec();
        }

        fn asked(&self, what: &'static str) -> Result<u64, PlatformError> {
            self.asked.lock().unwrap().push(what);
            let mut says = self.says.lock().unwrap();
            let next = match says.len() {
                0 => Says::Count(1),
                1 => says[0],
                _ => says.remove(0),
            };
            match next {
                Says::Count(count) => Ok(count),
                Says::Unsupported => Err(PlatformError::Unsupported("data-control")),
                Says::OutOfReach => Err(PlatformError::Unavailable("out of reach")),
            }
        }

        fn heard(&self) -> Vec<&'static str> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl LetsGo for Fake {
        fn let_go(&mut self) {
            self.asked.lock().unwrap().push("let go");
        }
    }

    impl Clipboard for Fake {
        fn change_count(&self) -> Result<u64, PlatformError> {
            self.asked("count")
        }

        fn read(&self, _: usize) -> Result<ClipboardRead, PlatformError> {
            self.asked("read").map(|_| ClipboardRead::Empty)
        }

        fn write(&mut self, _: &ClipboardContent) -> Result<u64, PlatformError> {
            self.asked("write")
        }

        fn write_promise(&mut self, promise: ClipboardPromise) -> Result<u64, PlatformError> {
            let written = self.asked("promise");
            if !matches!(written, Err(PlatformError::Unsupported(_))) {
                self.promises.lock().unwrap().push(promise);
            }
            written
        }
    }

    /// A promise's source, counting how often it was told it is gone.
    #[derive(Default)]
    struct Source(std::sync::atomic::AtomicUsize);

    impl crate::ClipboardSource for Source {
        fn fetch(&self, _: crate::ClipboardFormat) -> Option<Vec<u8>> {
            None
        }

        fn finished(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn promise(source: &Arc<Source>) -> ClipboardPromise {
        ClipboardPromise {
            formats: vec![crate::ClipboardFormat::Text],
            bytes: 1,
            files: Vec::new(),
            source: source.clone(),
        }
    }

    fn finished(source: &Source) -> usize {
        source.0.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn wayland(data_control: &Fake, portal: &Fake) -> WaylandClipboard {
        WaylandClipboard::new(Box::new(data_control.clone()), Box::new(portal.clone()))
    }

    #[test]
    fn nothing_is_asked_until_it_is_used() {
        let data_control = Fake::default();
        let portal = Fake::default();
        let clipboard = wayland(&data_control, &portal);
        assert_eq!(clipboard.chosen(), Chosen::Unknown);
        assert_eq!(clipboard.which().get(), Chosen::Unknown);
        assert!(data_control.heard().is_empty());
        clipboard.read(10).unwrap();
        assert_eq!(clipboard.chosen(), Chosen::DataControl);
        assert_eq!(clipboard.which().get(), Chosen::DataControl);
        assert_eq!(data_control.heard(), ["count", "read"]);
    }

    #[test]
    fn data_control_until_it_turns_out_to_have_none() {
        let data_control = Fake::saying(&[Says::Count(5)]);
        let portal = Fake::default();
        let mut clipboard = wayland(&data_control, &portal);
        assert_eq!(clipboard.change_count().unwrap(), 5);
        clipboard.write(&ClipboardContent::default()).unwrap();
        assert!(portal.heard().is_empty());

        // Later without data-control, as on another compositor: the
        // portal from then on.
        data_control.say(&[Says::Unsupported]);
        portal.say(&[Says::Count(1)]);
        let written = clipboard.write(&ClipboardContent::default()).unwrap();
        assert_eq!(clipboard.chosen(), Chosen::Portal);
        assert_eq!(clipboard.which().get(), Chosen::Portal);
        // Its count goes on from data-control's, and stays with the write.
        assert_eq!(written, 6);
        assert_eq!(clipboard.change_count().unwrap(), 6);
        portal.say(&[Says::Count(2)]);
        assert_eq!(clipboard.change_count().unwrap(), 7);
        clipboard.read(10).unwrap();
        assert_eq!(portal.heard(), ["write", "count", "count", "read"]);
    }

    #[test]
    fn counts_never_repeat_across_a_turn() {
        // Data-control far ahead of where the portal starts, and behind.
        for (data_control_count, portal_count) in [(40, 1), (1, 40)] {
            let data_control = Fake::saying(&[Says::Count(data_control_count)]);
            let portal = Fake::saying(&[Says::Count(portal_count)]);
            let clipboard = wayland(&data_control, &portal);
            let before = clipboard.change_count().unwrap();
            data_control.say(&[Says::Unsupported]);
            let after = clipboard.change_count().unwrap();
            assert!(after > before, "{after} after {before}");
            portal.say(&[Says::Count(portal_count + 1)]);
            assert_eq!(clipboard.change_count().unwrap(), after + 1);
        }
    }

    #[test]
    fn the_portal_where_there_is_no_data_control() {
        let data_control = Fake::saying(&[Says::Unsupported]);
        let portal = Fake::default();
        let clipboard = wayland(&data_control, &portal);
        clipboard.change_count().unwrap();
        assert_eq!(clipboard.chosen(), Chosen::Portal);
        clipboard.read(10).unwrap();
        assert_eq!(data_control.heard(), ["count"]);
        assert_eq!(portal.heard(), ["count", "read"]);
    }

    #[test]
    fn data_control_is_asked_again_now_and_then_and_used_once_it_answers() {
        let data_control = Fake::saying(&[Says::Unsupported]);
        let portal = Fake::saying(&[Says::Count(4)]);
        let clipboard = wayland(&data_control, &portal).reprobing_every(Duration::from_millis(100));
        let first = clipboard.change_count().unwrap();
        assert_eq!(clipboard.chosen(), Chosen::Portal);
        // Not asked again before its time.
        clipboard.change_count().unwrap();
        assert_eq!(data_control.heard(), ["count"]);

        // Asked again, still without: the portal, and not asked again
        // before its time.
        std::thread::sleep(Duration::from_millis(150));
        clipboard.read(10).unwrap();
        clipboard.read(10).unwrap();
        assert_eq!(clipboard.chosen(), Chosen::Portal);
        assert_eq!(data_control.heard(), ["count", "count"]);

        // With it now: data-control again, its count above the portal's.
        data_control.say(&[Says::Count(1)]);
        std::thread::sleep(Duration::from_millis(150));
        let after = clipboard.change_count().unwrap();
        assert_eq!(clipboard.chosen(), Chosen::DataControl);
        assert!(after > first, "{after} after {first}");
        assert_eq!(data_control.heard(), ["count", "count", "count", "count"]);
        // The portal let go of what it put there.
        assert_eq!(portal.heard(), ["count", "count", "read", "read", "let go"]);
    }

    #[test]
    fn a_promise_ends_once_whichever_way_it_goes() {
        // Data-control without the protocol leaves it to the portal, which
        // keeps it: nobody says it is gone.
        let data_control = Fake::saying(&[Says::Unsupported]);
        let portal = Fake::default();
        let mut clipboard = wayland(&data_control, &portal);
        let source = Arc::new(Source::default());
        clipboard.write_promise(promise(&source)).unwrap();
        assert_eq!(portal.promises.lock().unwrap().len(), 1);
        assert!(data_control.promises.lock().unwrap().is_empty());
        assert_eq!(finished(&source), 0);

        // Handed to a clipboard that fails it: that one's to end.
        portal.say(&[Says::OutOfReach]);
        let source = Arc::new(Source::default());
        assert!(clipboard.write_promise(promise(&source)).is_err());
        assert_eq!(portal.promises.lock().unwrap().len(), 2);
        assert_eq!(finished(&source), 0);

        // Handed to none, as while the compositor has not answered: ended
        // here.
        let data_control = Fake::saying(&[Says::OutOfReach]);
        let portal = Fake::default();
        let mut clipboard = wayland(&data_control, &portal);
        let source = Arc::new(Source::default());
        assert!(clipboard.write_promise(promise(&source)).is_err());
        assert_eq!(data_control.heard(), ["count"]);
        assert_eq!(finished(&source), 1);
    }

    #[test]
    fn a_compositor_out_of_reach_is_asked_again_until_it_says() {
        // Out of reach twice, then without data-control: the portal.
        let data_control = Fake::saying(&[Says::OutOfReach, Says::OutOfReach, Says::Unsupported]);
        let portal = Fake::default();
        let clipboard = wayland(&data_control, &portal);
        for _ in 0..2 {
            assert!(matches!(
                clipboard.read(10),
                Err(PlatformError::Unavailable(_))
            ));
            assert_eq!(clipboard.chosen(), Chosen::Unknown);
        }
        assert!(portal.heard().is_empty());
        clipboard.read(10).unwrap();
        assert_eq!(clipboard.chosen(), Chosen::Portal);
        assert_eq!(portal.heard(), ["read"]);

        // Out of reach, then answering: data-control.
        let data_control = Fake::saying(&[Says::OutOfReach, Says::Count(3)]);
        let portal = Fake::default();
        let clipboard = wayland(&data_control, &portal);
        assert!(clipboard.change_count().is_err());
        assert_eq!(clipboard.change_count().unwrap(), 3);
        assert_eq!(clipboard.chosen(), Chosen::DataControl);
        assert!(portal.heard().is_empty());
    }
}
