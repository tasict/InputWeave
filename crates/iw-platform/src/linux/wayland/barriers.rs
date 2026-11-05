//! What capture through the InputCapture portal decides without the portal:
//! the pointer barriers that go where
//! the cursor leaves this device (`Layout::exits`), what an activation at
//! one reports and when its input is let go of, and the keys and buttons
//! reported down.
//!
//! The desktop hands input over only as the cursor pushes through a
//! barrier, and then keeps it from the local apps until it is released.
//! Whether the cursor crosses is the router's to decide, which may wait
//! (`EdgePolicy.dwell_ms`) or say no (a switch modifier not held, a button
//! held). So an activation is reported to the agent as a push through the
//! edge, and input is held while the agent says the cursor waits there to
//! cross (`InputCapture::edge_wait`) and the push goes on; it is let go of
//! as soon as the agent says the cursor does not cross, the cursor moves
//! back from the edge or off the barrier, rests, a button or a key other
//! than a modifier goes down, or it was held for long enough. A click or
//! key that let it go has gone to the agent, not to the local apps; the
//! router hears that input was let go of (`CaptureEnded`), so the wait to
//! cross starts over at the next push, as it cannot see the cursor move
//! away meanwhile.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use iw_core::HidUsage;
use iw_core::geometry::{Point, Rect};
use iw_core::input::{Button, InputEvent};
use iw_core::layout::{Edge, Exit};
use iw_core::router::Hotkey;
use iw_keymap::usage;

use crate::linux::lock;
use crate::linux::natural_scroll::Natural;

/// A zone the portal describes (`GetZones`): a stretch of the compositor's
/// logical space, which is this device's desktop space, the pointer moves
/// in.
pub(crate) type Zone = Rect;

/// A pointer barrier on one edge of a zone, as the portal takes it: a line
/// on the left or top side of its pixels, both ends included.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Barrier {
    /// Its id for the portal, never zero.
    pub id: u32,
    pub edge: Edge,
    /// The zone whose edge it is on.
    pub zone: Zone,
    /// The display it is the edge of, as the agent has it, which may lie
    /// a unit or two off the zone (`ZONE_SLACK`): where the cursor is told
    /// to be is on it, so the agent sees it at its own edge.
    pub display: Rect,
    /// Where it lies: x for a left or right edge, y for a top or bottom one.
    pub at: i32,
    /// Its first and last pixel along the edge.
    pub first: i32,
    pub last: i32,
}

impl Barrier {
    /// Whether `other` lies where this does, whatever its id.
    pub(crate) fn same_place(&self, other: &Barrier) -> bool {
        *self
            == Barrier {
                id: self.id,
                ..other.clone()
            }
    }

    /// The line the portal takes: (x1, y1, x2, y2).
    pub(crate) fn line(&self) -> (i32, i32, i32, i32) {
        match self.edge {
            Edge::Left | Edge::Right => (self.at, self.first, self.at, self.last),
            Edge::Top | Edge::Bottom => (self.first, self.at, self.last, self.at),
        }
    }

    /// One unit straight out through it.
    pub(crate) fn outward(&self) -> Point {
        self.edge.outward()
    }

    /// Where `p` lies along the edge.
    fn along(&self, p: Point) -> f64 {
        self.edge.along(self.zone, p).0
    }

    /// Whether `along` is on one of the barrier's pixels.
    fn covers(&self, along: f64) -> bool {
        along >= f64::from(self.first) && along < f64::from(self.last) + 1.0
    }

    /// The display's pixel just inside the edge, at `along`: the edge of a
    /// right or bottom side is past the display's last pixel.
    fn inside(&self, along: f64) -> Point {
        let on = self.edge.point(self.display, along);
        match self.edge {
            Edge::Left | Edge::Top => on,
            Edge::Right | Edge::Bottom => on - self.outward(),
        }
    }

    /// How far `p` is inside the display from its pixel just inside the
    /// edge: negative past it.
    fn depth(&self, p: Point) -> f64 {
        let inside = self.inside(self.along(p));
        let out = self.outward();
        (inside.x - p.x) * out.x + (inside.y - p.y) * out.y
    }

    /// The point of the display at the barrier nearest `p`.
    fn held(&self, p: Point) -> Point {
        let along = self.along(p);
        self.inside(along.clamp(f64::from(self.first), f64::from(self.last)))
    }

    /// How far `p` is from the barrier, squared.
    fn distance_squared(&self, p: Point) -> f64 {
        let along = self.along(p);
        let off_along = (f64::from(self.first) - along)
            .max(along - (f64::from(self.last) + 1.0))
            .max(0.0);
        let on = self.edge.point(self.zone, along);
        let off_across = (p - on).x * self.outward().x + (p - on).y * self.outward().y;
        off_along * off_along + off_across * off_across
    }
}

/// How far a zone's edge may lie from the edge of the display it is: with
/// fractional scaling the portal's zones and `xdg_output`'s logical sizes
/// round differently, by a unit or so.
pub(crate) const ZONE_SLACK: f64 = 2.0;

/// The zone that is `exit`'s display: of those whose edge on that side lies
/// within `ZONE_SLACK` of the exit, the one that covers most of the display.
fn zone_of(exit: &Exit, zones: &[Zone]) -> Option<Zone> {
    let display = exit.bounds;
    zones
        .iter()
        .filter(|zone| (exit.edge.extent(**zone).0 - exit.at).abs() <= ZONE_SLACK)
        .map(|zone| {
            let width = zone.right().min(display.right()) - zone.left().max(display.left());
            let height = zone.bottom().min(display.bottom()) - zone.top().max(display.top());
            (*zone, width.max(0.0) * height.max(0.0))
        })
        .filter(|(_, overlap)| *overlap > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(zone, _)| zone)
}

/// The exits whose display is no zone of `zones`, which make no barrier.
/// Not those too short for a pixel, which make none either.
pub(crate) fn on_no_zone<'a>(exits: &'a [Exit], zones: &[Zone]) -> Vec<&'a Exit> {
    exits
        .iter()
        .filter(|exit| zone_of(exit, zones).is_none())
        .collect()
}

/// The barriers that make `exits` on `zones`: each exit's stretch on the
/// edge of its display's zone, put right on that zone's edge, clamped to it
/// and rounded to whole pixels; ids from `first_id` up. An exit whose
/// display is no zone, as while the portal's zones and the agent's displays
/// disagree, makes none.
pub(crate) fn barriers(exits: &[Exit], zones: &[Zone], first_id: u32) -> Vec<Barrier> {
    let mut barriers = Vec::new();
    for exit in exits {
        let Some(zone) = zone_of(exit, zones) else {
            continue;
        };
        let (at, low, high) = exit.edge.extent(zone);
        let (start, end) = (exit.start.max(low), exit.end.min(high));
        let first = (start.round() as i32).max(low as i32);
        let last = (end.round() as i32 - 1).min(high as i32 - 1);
        if last < first {
            continue;
        }
        barriers.push(Barrier {
            id: first_id.wrapping_add(barriers.len() as u32).max(1),
            edge: exit.edge,
            zone,
            display: exit.bounds,
            at: at.round() as i32,
            first,
            last,
        });
    }
    barriers
}

/// The barrier an activation came through: the one the portal names, or,
/// if it could not tell which, the one nearest where the cursor is. None if
/// it names one that is none of these, as one put before them: input handed
/// over there is not this agent's to report.
pub(crate) fn barrier_hit(
    barriers: &[Barrier],
    id: Option<u32>,
    cursor: Option<Point>,
) -> Option<&Barrier> {
    if let Some(id) = id {
        return barriers.iter().find(|b| b.id == id);
    }
    let cursor = cursor?;
    barriers.iter().min_by(|a, b| {
        a.distance_squared(cursor)
            .total_cmp(&b.distance_squared(cursor))
    })
}

/// Keeps `event` of input that came before its activation began, in
/// `early`, which holds at most `max`: past it, the oldest movement goes
/// (`is_motion`), so a press, a release or a frame never does. Dropping a
/// release would leave its key down, carried across and repeating there;
/// a movement lost is only a little less of a push. With no movement to
/// drop, it is kept all the same: only the time an activation waits for
/// the modifiers bounds them.
pub(crate) fn keep_early<T>(
    early: &mut Vec<T>,
    event: T,
    max: usize,
    is_motion: impl Fn(&T) -> bool,
) {
    if early.len() >= max {
        if let Some(oldest) = early.iter().position(&is_motion) {
            early.remove(oldest);
        } else if is_motion(&event) {
            return;
        }
    }
    early.push(event);
}

/// How long the agent may take to say what it makes of input held at an
/// edge before it is let go of.
pub(crate) const ANSWER_WAIT: Duration = Duration::from_millis(250);

/// How long the cursor may rest at the edge, while it waits to cross,
/// before input is let go of. The router is told (`CaptureEnded`), so the
/// wait starts over at the next push.
pub(crate) const REST: Duration = Duration::from_millis(300);

/// The longest input is held at an edge without the cursor crossing. Past
/// the longest wait to cross (`EdgePolicy.dwell_ms`, up to a second).
pub(crate) const HOLD_MAX: Duration = Duration::from_millis(1500);

/// How long to wait, once the cursor came back, for where it is put
/// (`warp`), before letting go without saying.
pub(crate) const WARP_WAIT: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// At the edge: reported to the agent, which has not grabbed it.
    Held {
        since: Instant,
        /// When the cursor last moved.
        moved: Instant,
        /// When the agent was last told something it has not answered.
        asked: Option<Instant>,
    },
    /// Grabbed: the cursor is on another device.
    Grabbed,
    /// The cursor came back here: waiting for where to put it.
    Returning { since: Instant },
}

/// What to do with the portal's capture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Verdict {
    Keep,
    /// Let go of input, with the cursor here if anywhere.
    Release(Option<Point>),
}

/// What a grab, or its end, does once the capture thread takes it: the
/// agent made it for the activation current as it did (`Grab::meant`), and
/// another may be current by now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum GrabTaken {
    /// It is about the activation current now.
    Apply,
    /// It is about one that ended, which said so: nothing to do.
    Ignore,
    /// A grab that nothing holds input for, and nothing said so: none was
    /// current as the agent grabbed, or the one it grabbed ended as the
    /// cursor came back, without telling the router. Control comes back
    /// (`InputEvent::CaptureEnded`).
    Ended,
}

/// A grab or its end the agent made while activation `meant` was current,
/// taken while `current` is, the last to end without telling the router
/// being `quiet`; 0 is none for each.
pub(crate) fn grab_taken(meant: u64, grabbed: bool, current: u64, quiet: u64) -> GrabTaken {
    if meant != 0 && meant == current {
        GrabTaken::Apply
    } else if grabbed && (meant == 0 || meant == quiet) {
        GrabTaken::Ended
    } else {
        GrabTaken::Ignore
    }
}

/// An activation: input the desktop handed over at a barrier.
///
/// The agent's answers (`edge_wait`) name the event they answer by its
/// count among those reported (`reported`): only those to the push through
/// the edge and after count, as the keys reported before it are answered
/// before the router saw the push, and an answer about an earlier
/// activation must not let this one go.
#[derive(Clone, Debug)]
pub(crate) struct Activation {
    pub id: u32,
    pub barrier: Barrier,
    /// Where the cursor is here, held at the edge.
    position: Point,
    phase: Phase,
    /// The count of the push through the edge among the events reported,
    /// once reported; and of the last one reported.
    push: Option<u64>,
    last: u64,
    /// The events reported from the push on that the agent has yet to
    /// answer, held at the edge: their counts, and when each went. An
    /// answer about some of them leaves the next to be answered in time.
    unanswered: VecDeque<(u64, Instant)>,
    /// How long to wait for where the cursor goes as it comes back.
    warp_wait: Duration,
}

impl Activation {
    /// Input handed over at `barrier`, with the cursor at `cursor` if the
    /// portal said; and the push through the edge to report, from just
    /// inside it.
    pub(crate) fn new(
        id: u32,
        barrier: Barrier,
        cursor: Option<Point>,
        now: Instant,
    ) -> (Self, InputEvent) {
        let middle = (f64::from(barrier.first) + f64::from(barrier.last)) / 2.0;
        let position = match cursor {
            Some(cursor) => barrier.held(cursor),
            None => barrier.inside(middle),
        };
        let push = InputEvent::PointerMotion {
            position,
            delta: barrier.outward(),
        };
        let activation = Self {
            id,
            barrier,
            position,
            phase: Phase::Held {
                since: now,
                moved: now,
                asked: Some(now),
            },
            push: None,
            last: 0,
            unanswered: VecDeque::new(),
            warp_wait: WARP_WAIT,
        };
        (activation, push)
    }

    /// Waits `wait` rather than `WARP_WAIT` for where the cursor goes as it
    /// comes back.
    pub(crate) fn with_warp_wait(self, wait: Duration) -> Self {
        Self {
            warp_wait: wait,
            ..self
        }
    }

    /// The push went as the `count`th event reported.
    pub(crate) fn pushed(&mut self, count: u64) {
        self.push = Some(count);
        self.last = count;
        if let Phase::Held { since, .. } = self.phase {
            self.unanswered.push_back((count, since));
        }
    }

    /// Another event of this activation went as the `count`th, at `now`:
    /// held at the edge, the agent is to answer it in time.
    pub(crate) fn reported(&mut self, count: u64, now: Instant) {
        self.last = self.last.max(count);
        if let Phase::Held {
            since,
            moved,
            asked,
        } = self.phase
        {
            self.unanswered.push_back((count, now));
            self.phase = Phase::Held {
                since,
                moved,
                asked: asked.or(Some(now)),
            };
        }
    }

    #[cfg(test)]
    fn grabbed(&self) -> bool {
        self.phase == Phase::Grabbed
    }

    /// Whether input is held at the edge, the agent not having grabbed it.
    pub(crate) fn held(&self) -> bool {
        matches!(self.phase, Phase::Held { .. })
    }

    /// Whether the cursor came back here and input is about to be let go
    /// of.
    pub(crate) fn returning(&self) -> bool {
        matches!(self.phase, Phase::Returning { .. })
    }

    /// The pointer moved by (`dx`, `dy`): what to report, if anything, and
    /// whether to let go.
    ///
    /// Held at the edge, the cursor slides along it and back from it but
    /// goes no further out; moving back from it, or off the barrier, lets go
    /// with the cursor there. Grabbed, the movement goes from the middle of
    /// the zone, as the router reads only the movement then, and so that a
    /// movement it reads after the cursor came back does not start at an
    /// exit. Coming back, nothing is reported: where the cursor is now is
    /// the agent's to say.
    pub(crate) fn moved(
        &mut self,
        dx: f64,
        dy: f64,
        now: Instant,
    ) -> (Option<InputEvent>, Verdict) {
        let delta = Point::new(dx, dy);
        let Phase::Held { since, asked, .. } = self.phase else {
            let report = (self.phase == Phase::Grabbed).then(|| InputEvent::PointerMotion {
                position: self.barrier.zone.center(),
                delta,
            });
            return (report, Verdict::Keep);
        };
        let report = InputEvent::PointerMotion {
            position: self.position,
            delta,
        };
        let b = &self.barrier;
        let to = self.position + delta;
        let depth = b.depth(to);
        // No further out than the edge.
        self.position = if depth < 0.0 {
            b.inside(b.along(to))
        } else {
            to
        };
        if depth >= 1.0 || !b.covers(b.along(to)) {
            return (Some(report), Verdict::Release(Some(self.position)));
        }
        self.phase = Phase::Held {
            since,
            moved: now,
            asked: asked.or(Some(now)),
        };
        (Some(report), Verdict::Keep)
    }

    /// A button, the wheel, or a key went down: held at the edge, input is
    /// let go of, unless the press is `kept`: a modifier, which may be the
    /// one that must be held to cross, or a key that makes a hotkey the
    /// router acts on with the modifiers held, as one to jump. Coming back,
    /// it is let go of at once, without waiting for where the cursor goes:
    /// the press is lost to the local apps as it is the agent's, and what
    /// follows reaches them only once the desktop has input back.
    pub(crate) fn pressed(&mut self, kept: bool) -> Verdict {
        match self.phase {
            Phase::Held { .. } if !kept => Verdict::Release(Some(self.position)),
            Phase::Returning { .. } => Verdict::Release(None),
            _ => Verdict::Keep,
        }
    }

    /// The agent says whether the cursor waits at the edge to cross, after
    /// the `handled`th event reported. Answers to events before the push
    /// are not about it.
    pub(crate) fn edge_wait(&mut self, handled: u64, waiting: bool) -> Verdict {
        if self.push.is_none_or(|push| handled < push) {
            return Verdict::Keep;
        }
        match self.phase {
            Phase::Held {
                since,
                moved,
                asked,
            } if waiting => {
                // Answered once the last event reported is; until then, the
                // first not yet answered is to be, in time.
                while self.unanswered.front().is_some_and(|(n, _)| *n <= handled) {
                    self.unanswered.pop_front();
                }
                let asked = if handled >= self.last {
                    None
                } else {
                    self.unanswered.front().map(|(_, at)| *at).or(asked)
                };
                self.phase = Phase::Held {
                    since,
                    moved,
                    asked,
                };
                Verdict::Keep
            }
            Phase::Held { .. } => Verdict::Release(Some(self.position)),
            _ => Verdict::Keep,
        }
    }

    /// The agent grabbed input, as the cursor crossed; or let go of it, as
    /// it came back, and says next where it is (`warped`).
    pub(crate) fn set_grabbed(&mut self, grabbed: bool, now: Instant) {
        self.unanswered.clear();
        self.phase = if grabbed {
            Phase::Grabbed
        } else {
            Phase::Returning { since: now }
        };
    }

    /// The agent puts the cursor at `to`: as it came back, or, held at the
    /// edge, as it jumps somewhere on this device. Input is let go of with
    /// the cursor there, the one way to move it on Wayland. Grabbed, the
    /// cursor is on another device.
    pub(crate) fn warped(&mut self, to: Point) -> Verdict {
        match self.phase {
            Phase::Returning { .. } | Phase::Held { .. } => Verdict::Release(Some(to)),
            Phase::Grabbed => Verdict::Keep,
        }
    }

    /// When `tick` may let go next.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        match self.phase {
            Phase::Held {
                since,
                moved,
                asked,
            } => {
                let mut at = (since + HOLD_MAX).min(moved + REST);
                if let Some(asked) = asked {
                    at = at.min(asked + ANSWER_WAIT);
                }
                Some(at)
            }
            Phase::Grabbed => None,
            Phase::Returning { since } => Some(since + self.warp_wait),
        }
    }

    /// Lets go once a wait ran out: the agent did not answer, the cursor
    /// rested, it was held long enough, or the agent did not say where the
    /// cursor came back.
    pub(crate) fn tick(&mut self, now: Instant) -> Verdict {
        match self.deadline() {
            Some(at) if now >= at => match self.phase {
                Phase::Returning { .. } => Verdict::Release(None),
                _ => Verdict::Release(Some(self.position)),
            },
            _ => Verdict::Keep,
        }
    }
}

/// The modifiers libei's masks tell (`ei_keyboard.modifiers`), at their
/// bits in a keymap's real modifiers (Shift, Lock, Control, Mod1…Mod5), as
/// the usual keymaps have Alt on Mod1 and Super on Mod4.
const MASKS: [(u32, HidUsage, HidUsage); 4] = [
    (1 << 0, usage::LEFT_SHIFT, usage::RIGHT_SHIFT),
    (1 << 2, usage::LEFT_CONTROL, usage::RIGHT_CONTROL),
    (1 << 3, usage::LEFT_ALT, usage::RIGHT_ALT),
    (1 << 6, usage::LEFT_GUI, usage::RIGHT_GUI),
];

/// What a capture that has input handed over at the edges shares between
/// the agent's calls and its own thread: where input goes and how many
/// events went, which activation holds input, the one the agent grabbed,
/// and the hotkeys that keep input held. The portal's capture (`capture`)
/// and the layer surfaces' (`wlr_capture`) both keep one.
#[derive(Default)]
pub(crate) struct Handover {
    /// Where input goes, once the agent started capture while it runs; let
    /// go of as it stops, so input reaches the agent only while it knows
    /// capture runs.
    events: Mutex<Option<Sender<InputEvent>>>,
    /// Counts the events reported, as the agent counts those it takes
    /// (`InputCapture::edge_wait`). The two counts agree because every event
    /// sent on the channel reaches the agent exactly once, in order: only
    /// events that went count here.
    reported: AtomicU64,
    /// The activation that holds input now, numbered from 1 as each begins
    /// (`last`); 0 while none does.
    current: AtomicU64,
    last: AtomicU64,
    /// The last activation to end without the router hearing so, as the
    /// cursor came back to this device (0: none).
    quiet: AtomicU64,
    /// The activation the agent grabbed input of, as the cursor crossed (0:
    /// none): set at once, ahead of the command saying so, so input held at
    /// the edge is not let go of meanwhile.
    grabbing: AtomicU64,
    /// The hotkeys the router acts on (`InputCapture::watch_hotkeys`): a key
    /// that makes one does not let go of input held at an edge.
    hotkeys: Mutex<Vec<Hotkey>>,
}

impl Handover {
    /// Input goes to `events` from now on.
    pub(crate) fn deliver(&self, events: Sender<InputEvent>) {
        *lock(&self.events) = Some(events);
    }

    /// Input goes to `events` from now on if `runs` says capture runs,
    /// asked under the same lock, so capture stopping meanwhile (which lets
    /// go of where input goes after saying it no longer runs) drops them.
    pub(crate) fn deliver_if(
        &self,
        events: Sender<InputEvent>,
        runs: impl FnOnce() -> bool,
    ) -> bool {
        let mut kept = lock(&self.events);
        if runs() {
            *kept = Some(events);
            true
        } else {
            false
        }
    }

    /// Input goes nowhere from now on.
    pub(crate) fn stop_delivering(&self) {
        *lock(&self.events) = None;
    }

    pub(crate) fn delivering(&self) -> bool {
        lock(&self.events).is_some()
    }

    /// Reports `event`; which one it is among those reported, if it went.
    pub(crate) fn report(&self, event: InputEvent) -> Option<u64> {
        let events = lock(&self.events);
        events.as_ref()?.send(event).ok()?;
        Some(self.reported.fetch_add(1, Ordering::SeqCst) + 1)
    }

    pub(crate) fn report_all(&self, events: impl IntoIterator<Item = InputEvent>) {
        for event in events {
            let _ = self.report(event);
        }
    }

    /// How many events went.
    pub(crate) fn reported(&self) -> u64 {
        self.reported.load(Ordering::SeqCst)
    }

    /// The agent grabs input or lets go of it: which activation that is of
    /// (`grab_taken`'s `meant`). A grab is of the one current now, noted
    /// ahead of the command saying so; letting go is of the one grabbed.
    pub(crate) fn grab(&self, grabbed: bool) -> u64 {
        if grabbed {
            let current = self.current();
            self.grabbing.store(current, Ordering::SeqCst);
            current
        } else {
            self.grabbing.swap(0, Ordering::SeqCst)
        }
    }

    /// Whether the agent grabbed input of the activation current now, ahead
    /// of its command.
    pub(crate) fn grabbed_ahead(&self) -> bool {
        let grabbing = self.grabbing.load(Ordering::SeqCst);
        grabbing != 0 && grabbing == self.current()
    }

    /// The activation that holds input now; 0 while none does.
    pub(crate) fn current(&self) -> u64 {
        self.current.load(Ordering::SeqCst)
    }

    /// Whether an activation holds input now.
    pub(crate) fn holds(&self) -> bool {
        self.current() != 0
    }

    /// The last activation to end without the router hearing so.
    pub(crate) fn quiet(&self) -> u64 {
        self.quiet.load(Ordering::SeqCst)
    }

    /// An activation begins: its number.
    pub(crate) fn begin(&self) -> u64 {
        let current = self.last.fetch_add(1, Ordering::SeqCst) + 1;
        self.current.store(current, Ordering::SeqCst);
        current
    }

    /// The activation holding input ends: which it was (0: none). If
    /// `quietly`, the router does not hear so, as the cursor came back.
    pub(crate) fn end(&self, quietly: bool) -> u64 {
        let ended = self.current.swap(0, Ordering::SeqCst);
        if quietly {
            self.quiet.store(ended, Ordering::SeqCst);
        }
        ended
    }

    pub(crate) fn watch_hotkeys(&self, hotkeys: &[Hotkey]) {
        *lock(&self.hotkeys) = hotkeys.to_vec();
    }

    /// Whether `key`, going down with what `pressed` holds, makes a hotkey
    /// the router acts on.
    pub(crate) fn makes_hotkey(&self, key: HidUsage, pressed: &Pressed) -> bool {
        lock(&self.hotkeys)
            .iter()
            .any(|h| h.matches(key, |k| pressed.is_down(k)))
    }
}

/// The keys and buttons reported down while input is handed over.
///
/// Buttons held as the desktop hands input over are not told at all: neither
/// libei nor the portal says which are down, so a drag that pushes through
/// a barrier crosses as if no button were held (whether GNOME or KDE hand
/// input over during a drag is not known).
///
/// Keys that went down before the desktop handed input over are not told
/// as keys, only as the modifiers they hold: those held as an activation
/// begins are reported down (as their left key), so the router knows a
/// modifier held to cross is, and carries it to the device the cursor goes
/// to. Everything reported down is reported up as input is let go of,
/// since what happens next goes to the local apps.
#[derive(Debug, Default)]
pub(crate) struct Pressed {
    keys: Vec<HidUsage>,
    /// Those of `keys` reported for a modifier mask rather than a key.
    from_mask: Vec<HidUsage>,
    buttons: Vec<Button>,
    mask: u32,
}

impl Pressed {
    /// A key went down or up: what to report. A key down already (as from
    /// the mask) is not reported down again, nor one never reported down
    /// up.
    pub(crate) fn key(&mut self, usage: HidUsage, pressed: bool) -> Option<InputEvent> {
        let down = self.keys.contains(&usage);
        self.from_mask.retain(|k| *k != usage);
        if pressed == down {
            return None;
        }
        if pressed {
            self.keys.push(usage);
        } else {
            self.keys.retain(|k| *k != usage);
        }
        Some(InputEvent::Key { usage, pressed })
    }

    /// Whether `usage` is down, as reported.
    pub(crate) fn is_down(&self, usage: HidUsage) -> bool {
        self.keys.contains(&usage)
    }

    pub(crate) fn button(&mut self, button: Button, pressed: bool) -> Option<InputEvent> {
        if pressed == self.buttons.contains(&button) {
            return None;
        }
        if pressed {
            self.buttons.push(button);
        } else {
            self.buttons.retain(|b| *b != button);
        }
        Some(InputEvent::Button { button, pressed })
    }

    /// The modifiers held now (the `depressed` mask), noted for when input
    /// is handed over next.
    pub(crate) fn set_mask(&mut self, mask: u32) {
        self.mask = mask;
    }

    /// While input is handed over: the modifiers the mask holds that no key
    /// reported down stands for, reported down, and those reported for the
    /// mask that it no longer holds, up.
    pub(crate) fn follow_mask(&mut self) -> Vec<InputEvent> {
        let mut reports = Vec::new();
        for (bit, left, right) in MASKS {
            let held = self.mask & bit != 0;
            if held && !self.keys.contains(&left) && !self.keys.contains(&right) {
                self.keys.push(left);
                self.from_mask.push(left);
                reports.push(InputEvent::Key {
                    usage: left,
                    pressed: true,
                });
            } else if !held && self.from_mask.contains(&left) {
                self.from_mask.retain(|k| *k != left);
                self.keys.retain(|k| *k != left);
                reports.push(InputEvent::Key {
                    usage: left,
                    pressed: false,
                });
            }
        }
        reports
    }

    /// The keys reported down, now reported up; the buttons stay down.
    pub(crate) fn release_keys(&mut self) -> Vec<InputEvent> {
        self.from_mask.clear();
        std::mem::take(&mut self.keys)
            .into_iter()
            .map(|usage| InputEvent::Key {
                usage,
                pressed: false,
            })
            .collect()
    }

    /// The buttons reported down, now reported up; the keys stay down.
    pub(crate) fn release_buttons(&mut self) -> Vec<InputEvent> {
        std::mem::take(&mut self.buttons)
            .into_iter()
            .map(|button| InputEvent::Button {
                button,
                pressed: false,
            })
            .collect()
    }

    /// Everything reported down, now reported up; and the modifiers held
    /// forgotten, to be told again as input is handed over next, rather
    /// than reported held then from what was so before.
    pub(crate) fn release_all(&mut self) -> Vec<InputEvent> {
        self.mask = 0;
        let mut ups = self.release_keys();
        ups.extend(self.release_buttons());
        ups
    }
}

/// A frame's scrolling as `InputEvent::Scroll` takes it: in notches,
/// positive `dy` away from the user, as the wheel turned. A wheel's notches
/// (120 a notch, `y` down) count where there are any, as the same turn may
/// come as smooth scrolling too; else smooth scrolling, at 15 logical
/// pixels a notch. Natural scrolling, which the desktop applied before
/// handing it over, is turned back (`Natural::turned`).
pub(crate) fn scroll_of(
    discrete: Option<(i32, i32)>,
    smooth: (f32, f32),
    natural: Natural,
) -> Option<(f64, f64)> {
    use super::translate::{PIXELS_PER_NOTCH, STEP};
    let (dx, dy) = match discrete {
        Some((x, y)) => (f64::from(x) / STEP, -f64::from(y) / STEP),
        None => (
            f64::from(smooth.0) / PIXELS_PER_NOTCH,
            -f64::from(smooth.1) / PIXELS_PER_NOTCH,
        ),
    };
    let (dx, dy) = if natural.turned(discrete.is_some()) {
        (-dx, -dy)
    } else {
        (dx, dy)
    };
    (dx != 0.0 || dy != 0.0).then_some((dx, dy))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An exit of the display `bounds`.
    fn exit(bounds: Rect, edge: Edge, at: f64, start: f64, end: f64) -> Exit {
        Exit {
            display: "DP-1".to_owned(),
            bounds,
            edge,
            at,
            start,
            end,
        }
    }

    fn lines(barriers: &[Barrier]) -> Vec<(u32, (i32, i32, i32, i32))> {
        barriers.iter().map(|b| (b.id, b.line())).collect()
    }

    #[test]
    fn barriers_lie_on_the_zones_edges_in_whole_pixels() {
        let zones = [
            Rect::new(0.0, 0.0, 1512.0, 982.0),
            Rect::new(1512.0, 0.0, 1920.0, 1080.0),
        ];
        let (laptop, monitor) = (zones[0], zones[1]);
        let exits = [
            // The laptop's top, short of its corners.
            exit(laptop, Edge::Top, 0.0, 20.0, 1492.0),
            // The monitor's right edge, a hair off from the float math.
            exit(monitor, Edge::Right, 3_432.000_000_1, 20.0, 1060.0),
            // Below the laptop, where the monitor is taller.
            exit(laptop, Edge::Bottom, 982.0, 20.0, 1492.0),
        ];
        let made = barriers(&exits, &zones, 1);
        assert_eq!(
            lines(&made),
            [
                (1, (20, 0, 1491, 0)),
                (2, (3432, 20, 3432, 1059)),
                (3, (20, 982, 1491, 982)),
            ]
        );
        assert_eq!(made[1].zone, zones[1]);
        // Ids go on from where the last barriers' ended.
        assert_eq!(
            barriers(&exits, &zones, 4)
                .iter()
                .map(|b| b.id)
                .collect::<Vec<_>>(),
            [4, 5, 6]
        );
    }

    #[test]
    fn barriers_are_clamped_to_their_displays_zone_and_need_one() {
        let zones = [
            Rect::new(0.0, 0.0, 1920.0, 1080.0),
            Rect::new(0.0, 1080.0, 1920.0, 1080.0),
        ];
        let (upper, lower) = (zones[0], zones[1]);
        let exits = [
            // Reaching past its zone: kept to it.
            exit(upper, Edge::Left, 0.0, -10.0, 1000.5),
            exit(lower, Edge::Right, 1920.0, 1000.0, 2000.0),
            // No zone has an edge there.
            exit(
                Rect::new(0.0, 0.0, 1000.0, 100.0),
                Edge::Right,
                1000.0,
                0.0,
                100.0,
            ),
            // A display that is no zone.
            exit(
                Rect::new(5000.0, 0.0, 800.0, 600.0),
                Edge::Right,
                5800.0,
                20.0,
                580.0,
            ),
            // Less than a pixel.
            exit(upper, Edge::Top, 0.0, 10.2, 10.4),
        ];
        assert_eq!(
            lines(&barriers(&exits, &zones, 1)),
            [(1, (0, 0, 0, 1000)), (2, (1920, 1080, 1920, 1999))]
        );
        // Those on no zone are told apart from the one too short.
        assert_eq!(on_no_zone(&exits, &zones), [&exits[2], &exits[3]]);
        assert!(barriers(&exits, &[], 1).is_empty());
        assert_eq!(on_no_zone(&exits, &[]).len(), exits.len());
    }

    #[test]
    fn the_push_is_told_at_the_displays_edge_where_the_zone_is_a_unit_short() {
        use iw_core::layout::{Device, DeviceId, EdgePolicy, Layout};

        // The agent's display ends at 1512; the portal's zone at 1511.
        let display = Rect::new(0.0, 0.0, 1512.0, 982.0);
        let zones = [Rect::new(0.0, 0.0, 1511.0, 982.0)];
        let made = barriers(
            &[exit(display, Edge::Right, 1512.0, 20.0, 962.0)],
            &zones,
            1,
        );
        assert_eq!(lines(&made), [(1, (1511, 20, 1511, 961))]);
        // The desktop says the cursor is past the zone's edge.
        let (_, push) = Activation::new(
            1,
            made[0].clone(),
            Some(Point::new(1511.5, 500.0)),
            Instant::now(),
        );
        let InputEvent::PointerMotion { position, delta } = push else {
            panic!("{push:?}");
        };
        assert_eq!(
            position,
            Point::new(1511.0, 500.0),
            "on the display's last pixel"
        );
        // As the router sees it, the push crosses to the device beside.
        let device = |id: &str, offset: f64, bounds: Rect| Device {
            id: DeviceId::new(id),
            name: id.to_owned(),
            displays: vec![iw_core::layout::Display {
                id: "main".to_owned(),
                bounds,
                scale: 1.0,
                primary: true,
            }],
            offset: Point::new(offset, 0.0),
        };
        let layout = Layout::new(vec![
            device("here", 0.0, display),
            device("there", 1512.0, Rect::new(0.0, 0.0, 1920.0, 1080.0)),
        ])
        .unwrap();
        let step = layout
            .step(
                &DeviceId::new("here"),
                position,
                delta,
                &EdgePolicy::default(),
                |_| true,
            )
            .unwrap();
        assert_eq!(step.device, DeviceId::new("there"));
    }

    #[test]
    fn a_zone_a_unit_off_its_display_takes_the_barrier_on_its_own_edge() {
        // Scaled by 1.5, 2560 pixels: the outputs say 1706 units, the
        // portal's zone 1707.
        let zones = [Rect::new(0.0, 0.0, 1707.0, 960.0)];
        let display = Rect::new(0.0, 0.0, 1706.0, 960.0);
        assert_eq!(
            lines(&barriers(
                &[exit(display, Edge::Right, 1706.0, 20.0, 940.0)],
                &zones,
                1
            )),
            [(1, (1707, 20, 1707, 939))]
        );
        // Further off than that, it is some other edge.
        let narrow = Rect::new(0.0, 0.0, 1704.0, 960.0);
        assert!(barriers(&[exit(narrow, Edge::Right, 1704.0, 20.0, 940.0)], &zones, 1).is_empty());
    }

    fn screen() -> Rect {
        Rect::new(0.0, 0.0, 1920.0, 1080.0)
    }

    fn right() -> Barrier {
        Barrier {
            id: 4,
            edge: Edge::Right,
            zone: screen(),
            display: screen(),
            at: 1920,
            first: 20,
            last: 1059,
        }
    }

    #[test]
    fn the_barrier_hit_is_the_one_named_or_the_nearest() {
        let top = Barrier {
            id: 1,
            edge: Edge::Top,
            zone: screen(),
            display: screen(),
            at: 0,
            first: 20,
            last: 1899,
        };
        let all = [top.clone(), right()];
        assert_eq!(barrier_hit(&all, Some(4), None), Some(&all[1]));
        let near_top = Some(Point::new(500.0, -1.0));
        // Named, but none of these: none, wherever the cursor is.
        assert_eq!(barrier_hit(&all, Some(9), near_top), None);
        // Not named: the nearest.
        assert_eq!(barrier_hit(&all, None, near_top), Some(&all[0]));
        assert_eq!(
            barrier_hit(&all, None, Some(Point::new(1921.0, 600.0))),
            Some(&all[1])
        );
        // Just past the top barrier's last pixel along, it is still the
        // nearest.
        assert_eq!(
            barrier_hit(&all, None, Some(Point::new(1900.5, -1.0))),
            Some(&all[0])
        );
        assert_eq!(barrier_hit(&all, None, None), None);
        assert_eq!(barrier_hit(&[], Some(1), near_top), None);
    }

    #[test]
    fn an_activation_reports_a_push_from_just_inside_the_edge() {
        let now = Instant::now();
        // The portal puts the cursor past the edge, off the barrier's end.
        let (a, push) = Activation::new(7, right(), Some(Point::new(1920.4, 1075.0)), now);
        assert_eq!(
            push,
            InputEvent::PointerMotion {
                position: Point::new(1919.0, 1059.0),
                delta: Point::new(1.0, 0.0),
            }
        );
        assert_eq!(a.id, 7);
        assert!(!a.grabbed());
        // Without a position, from the barrier's middle.
        let (_, push) = Activation::new(8, right(), None, now);
        assert_eq!(
            push,
            InputEvent::PointerMotion {
                position: Point::new(1919.0, 539.5),
                delta: Point::new(1.0, 0.0),
            }
        );
        // On the other sides, the zone's first pixel is just inside.
        let left = Barrier {
            edge: Edge::Left,
            at: 0,
            ..right()
        };
        let (_, push) = Activation::new(9, left, Some(Point::new(-0.5, 300.0)), now);
        assert_eq!(
            push,
            InputEvent::PointerMotion {
                position: Point::new(0.0, 300.0),
                delta: Point::new(-1.0, 0.0),
            }
        );
    }

    /// An activation at the right barrier with the cursor at `y`, its push
    /// reported as the `count`th event.
    fn pushed_at(y: f64, count: u64, now: Instant) -> Activation {
        let (mut a, _) = Activation::new(1, right(), Some(Point::new(1920.0, y)), now);
        a.pushed(count);
        a
    }

    fn motion(report: Option<InputEvent>) -> (Point, Point) {
        match report {
            Some(InputEvent::PointerMotion { position, delta }) => (position, delta),
            other => panic!("not a motion: {other:?}"),
        }
    }

    #[test]
    fn held_at_the_edge_the_cursor_slides_and_goes_back() {
        let now = Instant::now();
        let mut a = pushed_at(500.0, 1, now);
        assert_eq!(a.edge_wait(1, true), Verdict::Keep);
        // Pushing on and sliding along: no further out.
        let (report, verdict) = a.moved(5.0, 3.0, now);
        assert_eq!(
            motion(report),
            (Point::new(1919.0, 500.0), Point::new(5.0, 3.0))
        );
        assert_eq!(verdict, Verdict::Keep);
        let (report, _) = a.moved(2.0, 0.0, now);
        assert_eq!(
            motion(report),
            (Point::new(1919.0, 503.0), Point::new(2.0, 0.0))
        );
        // Back from the edge: let go, there.
        let (_, verdict) = a.moved(-4.0, 1.0, now);
        assert_eq!(verdict, Verdict::Release(Some(Point::new(1915.0, 504.0))));

        // Within the barrier's last pixel along the edge: still on it; past
        // it: off.
        let mut a = pushed_at(1050.0, 1, now);
        assert_eq!(a.moved(1.0, 9.5, now).1, Verdict::Keep);
        let (_, verdict) = a.moved(1.0, 0.5, now);
        assert_eq!(verdict, Verdict::Release(Some(Point::new(1919.0, 1060.0))));
    }

    #[test]
    fn held_input_goes_unless_the_agent_says_the_cursor_waits_to_cross() {
        let now = Instant::now();
        let held = Point::new(1919.0, 500.0);
        // No: a switch modifier not held, say.
        let mut a = pushed_at(500.0, 1, now);
        assert_eq!(a.edge_wait(1, false), Verdict::Release(Some(held)));

        // No answer in time.
        let mut a = pushed_at(500.0, 1, now);
        assert_eq!(a.deadline(), Some(now + ANSWER_WAIT));
        assert_eq!(a.tick(now + ANSWER_WAIT / 2), Verdict::Keep);
        assert_eq!(a.tick(now + ANSWER_WAIT), Verdict::Release(Some(held)));

        // Waiting to cross: held while the cursor pushes, up to a point.
        let mut a = pushed_at(500.0, 1, now);
        assert_eq!(a.edge_wait(1, true), Verdict::Keep);
        assert_eq!(a.deadline(), Some(now + REST));
        let mut t = now;
        let mut count = 1;
        while t < now + HOLD_MAX - Duration::from_millis(100) {
            t += Duration::from_millis(100);
            count += 1;
            assert_eq!(a.moved(1.0, 0.0, t).1, Verdict::Keep);
            a.reported(count, t);
            assert_eq!(a.edge_wait(count, true), Verdict::Keep);
            assert_eq!(a.tick(t), Verdict::Keep);
        }
        assert_eq!(a.tick(now + HOLD_MAX), Verdict::Release(Some(held)));

        // Resting.
        let mut a = pushed_at(500.0, 1, now);
        a.edge_wait(1, true);
        a.moved(1.0, 0.0, now + Duration::from_millis(100));
        a.reported(2, now + Duration::from_millis(100));
        a.edge_wait(2, true);
        let rest = now + Duration::from_millis(100) + REST;
        assert_eq!(a.deadline(), Some(rest));
        assert_eq!(a.tick(rest), Verdict::Release(Some(held)));

        // A movement not yet answered must be, in time; an answer to the
        // push before it does not do.
        let mut a = pushed_at(500.0, 1, now);
        let later = now + Duration::from_millis(50);
        a.moved(1.0, 0.0, later);
        a.reported(2, later);
        a.edge_wait(1, true);
        assert_eq!(a.deadline(), Some(later + ANSWER_WAIT));
        a.edge_wait(2, true);
        assert_eq!(a.deadline(), Some(later + REST));

        // Under steady movement, a mouse reporting every 8 ms, the answers
        // come one event behind: each is about one in time, so input is
        // held while the cursor waits to cross, as for a long dwell.
        let mut a = pushed_at(500.0, 1, now);
        let mut t = now;
        let mut count = 1;
        while t < now + Duration::from_secs(1) {
            t += Duration::from_millis(8);
            count += 1;
            assert_eq!(a.moved(1.0, 0.0, t).1, Verdict::Keep);
            a.reported(count, t);
            assert_eq!(a.edge_wait(count - 1, true), Verdict::Keep);
            assert_eq!(a.tick(t), Verdict::Keep, "let go of at {count}");
        }
        assert_eq!(a.deadline(), Some(t + ANSWER_WAIT));
        // Unanswered, the first not answered is let go of in time.
        assert_eq!(a.tick(t + ANSWER_WAIT), Verdict::Release(Some(held)));
    }

    #[test]
    fn answers_before_the_push_are_not_about_it() {
        let now = Instant::now();
        let (mut a, _) = Activation::new(1, right(), Some(Point::new(1920.0, 500.0)), now);
        // Not yet reported: nothing answers it.
        assert_eq!(a.edge_wait(9, false), Verdict::Keep);
        // A held modifier went as the 6th event, the push as the 7th. The
        // modifier's answer (the router had no push yet) and one about an
        // earlier activation are not about this push.
        a.pushed(7);
        assert_eq!(a.edge_wait(6, false), Verdict::Keep);
        assert_eq!(a.edge_wait(3, false), Verdict::Keep);
        assert_eq!(a.edge_wait(7, true), Verdict::Keep);
        assert_eq!(
            a.edge_wait(7, false),
            Verdict::Release(Some(Point::new(1919.0, 500.0)))
        );
    }

    #[test]
    fn a_jump_here_lets_input_held_at_the_edge_go_where_it_goes() {
        let now = Instant::now();
        let mut a = pushed_at(500.0, 1, now);
        a.edge_wait(1, true);
        // Held, as by the jump hotkey's key: the agent puts the cursor in
        // the middle of the main display, and input goes back there.
        let middle = Point::new(960.0, 540.0);
        assert_eq!(a.warped(middle), Verdict::Release(Some(middle)));
    }

    #[test]
    fn a_press_held_at_the_edge_lets_go_but_a_modifier() {
        let now = Instant::now();
        let mut a = pushed_at(500.0, 1, now);
        a.edge_wait(1, true);
        assert_eq!(a.pressed(true), Verdict::Keep);
        assert_eq!(
            a.pressed(false),
            Verdict::Release(Some(Point::new(1919.0, 500.0)))
        );
        a.set_grabbed(true, now);
        assert_eq!(a.pressed(false), Verdict::Keep, "on the other device");
    }

    #[test]
    fn input_kept_before_an_activation_begins_loses_movement_first() {
        #[derive(Debug, PartialEq)]
        enum E {
            Move(u32),
            Key(u32, bool),
            Frame,
        }
        let is_motion = |e: &E| matches!(e, E::Move(_));
        let mut early = Vec::new();
        for event in [E::Move(1), E::Key(30, true), E::Move(2), E::Frame] {
            keep_early(&mut early, event, 4, is_motion);
        }
        // Full: the oldest movement goes for anything that comes.
        keep_early(&mut early, E::Key(30, false), 4, is_motion);
        assert_eq!(
            early,
            [E::Key(30, true), E::Move(2), E::Frame, E::Key(30, false)]
        );
        keep_early(&mut early, E::Move(3), 4, is_motion);
        assert_eq!(
            early,
            [E::Key(30, true), E::Frame, E::Key(30, false), E::Move(3)]
        );
        // No movement left to drop: a press or a release is kept beyond,
        // a movement is not.
        keep_early(&mut early, E::Frame, 4, is_motion);
        keep_early(&mut early, E::Key(31, true), 4, is_motion);
        assert_eq!(early.len(), 5);
        keep_early(&mut early, E::Move(4), 5, is_motion);
        assert_eq!(early.last(), Some(&E::Key(31, true)));
        keep_early(&mut early, E::Key(31, false), 5, is_motion);
        assert_eq!(early.last(), Some(&E::Key(31, false)));
        assert!(!early.contains(&E::Move(3)), "dropped for the frame");
    }

    #[test]
    fn a_grab_is_about_the_activation_current_as_the_agent_made_it() {
        // Made for the one current now: done.
        assert_eq!(grab_taken(3, true, 3, 0), GrabTaken::Apply);
        assert_eq!(grab_taken(3, false, 3, 2), GrabTaken::Apply);
        // Made for one that ended, as the next began: not about the next.
        // It said it ended, which the agent hears after its grab.
        assert_eq!(grab_taken(2, true, 3, 0), GrabTaken::Ignore);
        assert_eq!(grab_taken(2, true, 0, 0), GrabTaken::Ignore);
        assert_eq!(grab_taken(2, false, 3, 0), GrabTaken::Ignore);
        // None was current: nothing holds input for the grab.
        assert_eq!(grab_taken(0, true, 3, 0), GrabTaken::Ended);
        assert_eq!(grab_taken(0, true, 0, 0), GrabTaken::Ended);
        assert_eq!(grab_taken(0, false, 3, 0), GrabTaken::Ignore);
        // Made again for one that ended as the cursor came back, which told
        // the router nothing: control comes back. Its end changes nothing.
        assert_eq!(grab_taken(2, true, 0, 2), GrabTaken::Ended);
        assert_eq!(grab_taken(2, true, 3, 2), GrabTaken::Ended);
        assert_eq!(grab_taken(2, false, 0, 2), GrabTaken::Ignore);
    }

    #[test]
    fn grabbed_and_back() {
        let now = Instant::now();
        let mut a = pushed_at(500.0, 1, now);
        a.set_grabbed(true, now);
        assert!(a.grabbed());
        assert_eq!(a.deadline(), None);
        assert_eq!(a.tick(now + Duration::from_secs(60)), Verdict::Keep);
        // Movements go as they are, from the middle of the zone, away from
        // any exit.
        let (report, verdict) = a.moved(300.0, 40.0, now);
        assert_eq!(
            motion(report),
            (Point::new(960.0, 540.0), Point::new(300.0, 40.0))
        );
        assert_eq!(verdict, Verdict::Keep);
        assert_eq!(a.edge_wait(5, false), Verdict::Keep);
        assert_eq!(a.warped(Point::new(1.0, 1.0)), Verdict::Keep);
        // Back: nothing more is reported, and input is let go of where the
        // agent puts the cursor; or at once, as a key or button goes down.
        a.set_grabbed(false, now);
        assert!(a.returning());
        assert_eq!(a.moved(5.0, 0.0, now), (None, Verdict::Keep));
        assert_eq!(a.pressed(false), Verdict::Release(None));
        assert_eq!(a.pressed(true), Verdict::Release(None));
        assert_eq!(
            a.warped(Point::new(1919.0, 620.0)),
            Verdict::Release(Some(Point::new(1919.0, 620.0)))
        );
        // Or, if it says nothing, without.
        let (mut a, _) = Activation::new(1, right(), None, now);
        a.set_grabbed(true, now);
        a.set_grabbed(false, now);
        assert_eq!(a.tick(now + WARP_WAIT / 2), Verdict::Keep);
        assert_eq!(a.tick(now + WARP_WAIT), Verdict::Release(None));
    }

    #[test]
    fn keys_down_are_reported_once_and_up_as_input_goes() {
        let mut p = Pressed::default();
        let a = iw_keymap::usage::A;
        assert_eq!(
            p.key(a, true),
            Some(InputEvent::Key {
                usage: a,
                pressed: true
            })
        );
        assert_eq!(p.key(a, true), None);
        assert_eq!(p.key(usage::C, false), None, "never reported down");
        assert_eq!(
            p.button(Button::Left, true),
            Some(InputEvent::Button {
                button: Button::Left,
                pressed: true
            })
        );
        assert_eq!(p.button(Button::Left, true), None);
        assert_eq!(
            p.release_all(),
            [
                InputEvent::Key {
                    usage: a,
                    pressed: false
                },
                InputEvent::Button {
                    button: Button::Left,
                    pressed: false
                },
            ]
        );
        assert_eq!(p.release_all(), []);
    }

    #[test]
    fn keys_and_buttons_go_up_apart() {
        let mut p = Pressed::default();
        let a = iw_keymap::usage::A;
        p.key(a, true);
        p.button(Button::Left, true);
        assert_eq!(
            p.release_buttons(),
            [InputEvent::Button {
                button: Button::Left,
                pressed: false
            }]
        );
        assert_eq!(p.release_buttons(), []);
        assert!(p.is_down(a));
        // An up after it is not reported again.
        assert_eq!(p.button(Button::Left, false), None);
        p.button(Button::Right, true);
        assert_eq!(
            p.release_keys(),
            [InputEvent::Key {
                usage: a,
                pressed: false
            }]
        );
        assert_eq!(p.key(a, false), None);
        assert_eq!(
            p.release_all(),
            [InputEvent::Button {
                button: Button::Right,
                pressed: false
            }]
        );
    }

    #[test]
    fn modifiers_held_as_input_is_handed_over_are_reported() {
        let mut p = Pressed::default();
        // Control and Super held before: reported as their left keys.
        p.set_mask((1 << 2) | (1 << 6) | (1 << 1));
        let down = |usage| InputEvent::Key {
            usage,
            pressed: true,
        };
        let up = |usage| InputEvent::Key {
            usage,
            pressed: false,
        };
        assert_eq!(
            p.follow_mask(),
            [down(usage::LEFT_CONTROL), down(usage::LEFT_GUI)]
        );
        assert_eq!(p.follow_mask(), [], "told once");
        // The real key of one goes up: reported up as itself.
        assert_eq!(
            p.key(usage::LEFT_CONTROL, false),
            Some(up(usage::LEFT_CONTROL))
        );
        p.set_mask(1 << 6);
        assert_eq!(p.follow_mask(), []);
        // Another keyboard's right Shift goes down, then the mask says so.
        assert_eq!(
            p.key(usage::RIGHT_SHIFT, true),
            Some(down(usage::RIGHT_SHIFT))
        );
        p.set_mask((1 << 6) | 1);
        assert_eq!(p.follow_mask(), [], "the key stands for it");
        // Super let go of, as the mask tells.
        p.set_mask(1);
        assert_eq!(p.follow_mask(), [up(usage::LEFT_GUI)]);
        // A key down from the mask, then pressed for real: not again.
        p.set_mask(1 | (1 << 3));
        assert_eq!(p.follow_mask(), [down(usage::LEFT_ALT)]);
        assert_eq!(p.key(usage::LEFT_ALT, true), None);
        // As its own key now, the mask's dropping it does not let it go.
        p.set_mask(1);
        assert_eq!(p.follow_mask(), []);
        assert_eq!(
            p.release_all(),
            [up(usage::RIGHT_SHIFT), up(usage::LEFT_ALT)]
        );
        // Shift was held as input went back: not taken to be held next time
        // without the desktop telling so again.
        assert_eq!(p.follow_mask(), []);
        p.set_mask(1);
        assert_eq!(p.follow_mask(), [down(usage::LEFT_SHIFT)]);
    }

    #[test]
    fn scrolling() {
        // A notch away from the user is up, negative y in libei.
        let plain = Natural::default();
        assert_eq!(
            scroll_of(Some((0, -120)), (0.0, -15.0), plain),
            Some((0.0, 1.0))
        );
        assert_eq!(
            scroll_of(Some((240, 0)), (0.0, 0.0), plain),
            Some((2.0, 0.0))
        );
        assert_eq!(scroll_of(None, (-7.5, 30.0), plain), Some((-0.5, -2.0)));
        assert_eq!(scroll_of(None, (0.0, 0.0), plain), None);
        // Natural scrolling turned back: a wheel by the mouse's setting,
        // smooth scrolling by the touchpad's.
        let touchpad = Natural {
            mouse: false,
            touchpad: true,
        };
        assert_eq!(
            scroll_of(Some((0, -120)), (0.0, -15.0), touchpad),
            Some((0.0, 1.0))
        );
        assert_eq!(scroll_of(None, (-7.5, 30.0), touchpad), Some((0.5, 2.0)));
        let mouse = Natural {
            mouse: true,
            touchpad: false,
        };
        assert_eq!(
            scroll_of(Some((0, -120)), (0.0, -15.0), mouse),
            Some((0.0, -1.0))
        );
        assert_eq!(scroll_of(None, (0.0, 0.0), mouse), None);
    }
}
