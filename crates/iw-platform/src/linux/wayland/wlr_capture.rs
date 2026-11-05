//! Capturing this device's keyboard and mouse on a wlroots compositor,
//! which has no InputCapture portal.
//!
//! Where the cursor leaves this device (`InputCapture::set_exits`), a layer
//! surface a pixel thick (`zwlr_layer_shell_v1`, the overlay layer) lies
//! along the edge: the strip. The cursor is on it as it reaches the edge,
//! where the compositor keeps it however far it is pushed on, and relative
//! motion (`zwp_relative_pointer_v1`) comes all the same. Once the cursor
//! moves out through the edge, input is handed over as through the portal,
//! and what `barriers` decides for an activation is done the same; only
//! the transport differs:
//!
//! - Held at the edge, the strip takes the keyboard (exclusive keyboard
//!   interactivity), which tells the keys down as it does (`wl_keyboard`'s
//!   enter), so a modifier held to cross is known as the push is told. The
//!   cursor is not held: the compositor keeps it at the edge as it pushes
//!   on, and moving away moves it off the strip, which lets go there.
//! - Grabbed, a transparent layer surface covers every output (the
//!   covers, put on an output that comes meanwhile at once), so the cursor,
//!   hidden, is over a surface here wherever it goes, and its motion,
//!   buttons and wheel come here; the keyboard stays the strip's, and the
//!   compositor's own shortcuts come here too where it lets them
//!   (`zwp_keyboard_shortcuts_inhibit_v1`). sway does not lock or confine
//!   the pointer for a layer surface (pointer constraints are its
//!   windows'), hence the covers.
//! - Should the strip lose the keyboard meanwhile (a screen lock taking
//!   it; sway also takes it a moment as an output comes), the keys down are
//!   reported up, as their ups go elsewhere; should the pointer leave the
//!   surfaces here for another client's (a screen lock taking it), or a
//!   cover close, the buttons down are; moving from one surface here to
//!   another, as from a cover to the next output's, lets go of nothing.
//!   Should the strip close, input is let go of.
//! - Let go of, the covers go, the strip gives the keyboard back, and a
//!   virtual pointer of this connection puts the cursor where the agent
//!   says, beside the strip rather than on it; a push the router does not
//!   cross leaves it where the user moved it. The same pointer moves the
//!   cursor whenever the agent warps it, so jumps here work.
//!
//! The focused app loses the keyboard while input is held at an edge, and
//! the pointer while it is grabbed, as the portal's desktop takes them. Keys
//! repeat at the seat's pace while grabbed, as Wayland leaves repeating to
//! clients. Nobody is asked: no portal stands between, so there is no
//! permission. A thread of its own keeps the connection; the agent's calls
//! queue commands. Once the compositor goes, the thread connects again, and
//! what was down is reported up first.

use std::collections::VecDeque;
use std::os::unix::net::UnixDatagram;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iw_core::HidUsage;
use iw_core::geometry::{Point, Rect};
use iw_core::input::InputEvent;
use iw_core::layout::Exit;
use iw_core::router::Hotkey;
use tracing::{debug, info, warn};
use wayland_client::Proxy;
use wayland_client::backend::ObjectId;
use wayland_client::protocol::{wl_buffer, wl_output, wl_surface};
use wayland_protocols::wp::viewporter::client::wp_viewport;

use super::barriers::{self, Activation, Barrier, GrabTaken, Handover, Pressed, Verdict};
use super::translate::button_of_ei;
use super::wlr::{
    self, Anchor, Ending, Event, Inhibitor, KeyboardInteractivity, Layer, LayerSurface, Link,
    Served, VirtualPointer, Wants, WlAxis,
};
use super::wlroots::{self, Repeat};
use crate::PlatformError;
use crate::linux::common::lock;
use crate::linux::natural_scroll::Natural;

/// How long dropping waits for input to be let go of.
const STOP_WAIT: Duration = Duration::from_secs(1);
/// How long a push waits for the keyboard, and with it the modifiers held,
/// before it is told without them.
const KEYBOARD_WAIT: Duration = Duration::from_millis(50);
/// How long the exits or outputs must hold still before the strips are
/// put again.
const SETTLE: Duration = Duration::from_millis(50);
/// The most input kept for a push that has not begun.
const EARLY_MAX: usize = 256;
/// How long the pointer may be off every surface here before the buttons
/// down are reported up: a leave and the enter after it may come in
/// different reads of the socket.
const AWAY_WAIT: Duration = Duration::from_millis(100);
/// How long an output whose cover the compositor closed goes uncovered,
/// unless the outputs change first: its removal may be on its way, and a
/// cover made at once would be closed again.
const COVER_AGAIN: Duration = Duration::from_millis(200);
/// The layer surfaces' namespace, as the compositor lists them.
const NAMESPACE: &str = "inputweave-edge";

enum Command {
    Exits(Vec<Exit>),
    Grab { meant: u64, grabbed: bool },
    Warp(Point),
    EdgeWait(u64, bool),
}

#[derive(Default)]
struct Shared {
    commands: Mutex<VecDeque<Command>>,
    /// Whether a connection runs.
    running: AtomicBool,
    /// Where input goes, which activation holds it, and the hotkeys.
    handover: Handover,
    ending: Ending,
}

impl Shared {
    fn capturing(&self) -> bool {
        self.running.load(Ordering::SeqCst) && self.handover.delivering()
    }
}

/// Capture of this device's keyboard and mouse at layer surfaces.
pub struct WlrCapture {
    shared: Arc<Shared>,
    wake: UnixDatagram,
}

impl WlrCapture {
    /// Starts the thread, which connects as it can.
    pub(crate) fn start() -> Result<Self, PlatformError> {
        let (wake, woken) = wlr::wake_pair()?;
        let shared = Arc::new(Shared::default());
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("inputweave-wayland-edge-capture".to_owned())
            .spawn(move || run(&thread_shared, &woken))
            .map_err(|e| PlatformError::Os(format!("cannot start the edge capture thread: {e}")))?;
        Ok(Self { shared, wake })
    }

    fn send(&self, command: Command) {
        lock(&self.shared.commands).push_back(command);
        let _ = self.wake.send(&[1]);
    }

    /// Whether capture at the edges runs, for the session's status.
    pub(crate) fn edges(&self) -> Edges {
        Edges(self.shared.clone())
    }
}

/// What the session's status reads of capture at the edges: whether a
/// connection runs, with its layer surfaces.
#[derive(Clone)]
pub(crate) struct Edges(Arc<Shared>);

impl Edges {
    pub(crate) fn connected(&self) -> bool {
        self.0.running.load(Ordering::SeqCst)
    }
}

impl Drop for WlrCapture {
    /// Lets go of input and ends, waiting a moment for it.
    fn drop(&mut self) {
        self.shared.ending.ask(&self.wake, STOP_WAIT);
    }
}

impl crate::InputCapture for WlrCapture {
    /// Input goes to the agent once a connection runs; until then it is
    /// starting, and the agent tries again.
    fn start(&mut self, events: Sender<InputEvent>) -> Result<(), PlatformError> {
        let running = || self.shared.running.load(Ordering::SeqCst);
        if self.shared.handover.deliver_if(events, running) {
            Ok(())
        } else {
            Err(PlatformError::Starting("input capture at the screen edges"))
        }
    }

    fn set_grabbed(&mut self, grabbed: bool) -> Result<(), PlatformError> {
        let meant = self.shared.handover.grab(grabbed);
        self.send(Command::Grab { meant, grabbed });
        Ok(())
    }

    /// The strip took the keyboard, and the covers the pointer, from the
    /// focused app, and with them every key and button down.
    fn releases_carried_input_locally(&self) -> bool {
        false
    }

    fn capturing(&self) -> bool {
        self.shared.capturing()
    }

    /// Its own virtual pointer moves the cursor whenever a connection runs.
    fn can_warp(&self) -> bool {
        self.shared.running.load(Ordering::SeqCst)
    }

    fn watch_hotkeys(&mut self, hotkeys: &[Hotkey]) {
        self.shared.handover.watch_hotkeys(hotkeys);
    }

    fn warp(&mut self, position: Point) -> Result<(), PlatformError> {
        self.send(Command::Warp(position));
        Ok(())
    }

    fn set_exits(&mut self, exits: &[Exit]) {
        self.send(Command::Exits(exits.to_vec()));
    }

    /// Only while input is handed over: the cursor leaves this device
    /// across an edge.
    fn grab_ready(&self) -> bool {
        self.shared.handover.holds()
    }

    fn wants_exits(&self) -> bool {
        true
    }

    /// The cursor leaves this device only across an exit: input is handed
    /// over only as it pushes out there.
    fn edges_only(&self) -> bool {
        true
    }

    fn edge_wait(&mut self, handled: u64, waiting: bool) {
        self.send(Command::EdgeWait(handled, waiting));
    }
}

/// Connects, serves, and connects again, until dropped. The exits are kept
/// from one connection to the next, and taken as they come meanwhile;
/// nothing else has anything to act on then.
fn run(shared: &Shared, woken: &UnixDatagram) {
    wlr::keep_connected(
        "capture input at the screen edges",
        &shared.ending,
        woken,
        &mut Vec::new(),
        |exits| {
            let mut running = match Running::open(shared, std::mem::take(exits)) {
                Ok(running) => running,
                Err((e, kept)) => {
                    *exits = kept;
                    return Served::Unopened(e);
                }
            };
            info!("capturing input at the screen edges through layer surfaces");
            shared.running.store(true, Ordering::SeqCst);
            let why = running.serve(woken);
            shared.running.store(false, Ordering::SeqCst);
            running.lost();
            *exits = std::mem::take(&mut running.exits);
            match why {
                Ok(()) => Served::Ended,
                Err(e) => Served::Lost(e),
            }
        },
        |exits| {
            for command in lock(&shared.commands).drain(..) {
                if let Command::Exits(e) = command {
                    *exits = e;
                }
            }
        },
    );
}

/// A layer surface of this connection: a strip at a barrier, or a cover.
struct Surface {
    surface: wl_surface::WlSurface,
    layer: LayerSurface,
    /// Where the compositor scales surfaces.
    viewport: Option<wp_viewport::WpViewport>,
    buffer: Option<wl_buffer::WlBuffer>,
    /// The size shown at.
    size: Option<(u32, u32)>,
}

impl Surface {
    fn id(&self) -> ObjectId {
        self.surface.id()
    }

    /// Shows the surface at `size`, transparent: one pixel stretched over
    /// it where the compositor scales surfaces, else a buffer of that
    /// size, made again as the size changes. A cover's would be some
    /// 8 MB on a 1080p output, 33 MB on a 4K one.
    fn show(&mut self, link: &Link, size: (u32, u32)) {
        if let Some(viewport) = &self.viewport {
            let (Ok(width), Ok(height)) = (i32::try_from(size.0), i32::try_from(size.1)) else {
                return;
            };
            viewport.set_destination(width, height);
            if self.buffer.is_some() {
                self.size = Some(size);
                return;
            }
        }
        let (width, height) = if self.viewport.is_some() {
            (1, 1)
        } else {
            size
        };
        let Some(buffer) = link.clear_buffer(width, height) else {
            return;
        };
        self.surface.attach(Some(&buffer), 0, 0);
        // `damage_buffer` came with wl_surface 4.
        if self.surface.version() >= 4 {
            self.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        } else {
            self.surface.damage(0, 0, i32::MAX, i32::MAX);
        }
        if let Some(old) = self.buffer.replace(buffer) {
            old.destroy();
        }
        self.size = Some(size);
    }

    fn destroy(self) {
        if let Some(viewport) = self.viewport {
            viewport.destroy();
        }
        self.layer.destroy();
        self.surface.destroy();
        if let Some(buffer) = self.buffer {
            buffer.destroy();
        }
    }
}

/// A strip: the surface at a barrier.
struct Strip {
    barrier: Barrier,
    surface: Surface,
}

/// A push out through an edge, waiting for the keyboard to begin.
struct Pending {
    position: Point,
    begin_at: Instant,
}

/// A connection, its surfaces, and the input it holds.
struct Running<'a> {
    shared: &'a Shared,
    link: Link,
    pointer: VirtualPointer,
    exits: Vec<Exit>,
    strips: Vec<Strip>,
    /// While grabbed: a surface over each output, with the output's id.
    covers: Vec<(ObjectId, Surface)>,
    /// The outputs whose cover the compositor closed, uncovered until the
    /// outputs change or until `cover_again_at` (`COVER_AGAIN`).
    uncovered: Vec<ObjectId>,
    cover_again_at: Option<Instant>,
    /// The id of the next barrier: never given again.
    next_id: u32,
    place_at: Option<Instant>,
    /// The outputs' changes seen so far.
    outputs_seen: u64,
    /// Counts this connection's own warps; and the last, until the
    /// compositor handled it (`Event::Fenced`).
    warps: u32,
    warping: Option<u32>,
    /// When the buttons down go up, the pointer having left the surfaces
    /// here and entered none since (`AWAY_WAIT`).
    away_at: Option<Instant>,
    /// The surface here the pointer last entered, until it left it.
    pointer_on: Option<ObjectId>,
    warned_unplaced: bool,
    /// The strip the pointer is on, and where on the desktop.
    on: Option<(ObjectId, Point)>,
    /// The strip input is held at as a push began there, and where.
    held_at: Option<(ObjectId, Point)>,
    pending: Option<Pending>,
    early: Vec<Event>,
    /// Modifiers down as the strip took the keyboard.
    entered: Vec<HidUsage>,
    activation: Option<Activation>,
    inhibitor: Option<Inhibitor>,
    pressed: Pressed,
    repeat: Repeat,
    discrete: Option<(i32, i32)>,
    smooth: (f32, f32),
}

impl<'a> Running<'a> {
    fn open(shared: &'a Shared, exits: Vec<Exit>) -> Result<Self, (String, Vec<Exit>)> {
        let link = match Link::open(Wants {
            keyboard: true,
            keymap: false,
            pointer: true,
        }) {
            Ok(link) => link,
            Err(e) => return Err((e, exits)),
        };
        let state = &link.state;
        let (Some(seat), Some(pointers)) = (state.seat.clone(), state.pointer_manager.clone())
        else {
            return Err((
                "the compositor has no seat or virtual pointer".to_owned(),
                exits,
            ));
        };
        if state.layer_shell.is_none() || state.compositor.is_none() || state.shm.is_none() {
            return Err(("the compositor has no layer shell".to_owned(), exits));
        }
        let pointer = pointers.create_virtual_pointer(Some(&seat), &state.qh, ());
        let mut repeat = Repeat::default();
        if let Some((rate, delay)) = state.repeat {
            repeat.set(rate, delay);
        }
        Ok(Self {
            shared,
            link,
            pointer,
            exits,
            strips: Vec::new(),
            covers: Vec::new(),
            uncovered: Vec::new(),
            cover_again_at: None,
            next_id: 1,
            place_at: Some(Instant::now()),
            outputs_seen: 0,
            warps: 0,
            warping: None,
            away_at: None,
            pointer_on: None,
            warned_unplaced: false,
            on: None,
            held_at: None,
            pending: None,
            early: Vec::new(),
            entered: Vec::new(),
            activation: None,
            inhibitor: None,
            pressed: Pressed::default(),
            repeat,
            discrete: None,
            smooth: (0.0, 0.0),
        })
    }

    /// Serves until dropped (`Ok`) or the connection breaks.
    fn serve(&mut self, woken: &UnixDatagram) -> Result<(), String> {
        loop {
            if self.shared.ending.asked() {
                self.release(None, true);
                return self.link.sync(STOP_WAIT / 2);
            }
            let commands: Vec<Command> = lock(&self.shared.commands).drain(..).collect();
            for command in commands {
                self.command(command);
            }
            while let Some(event) = self.link.state.events.pop_front() {
                self.event(event);
            }
            if let Some((rate, delay)) = self.link.state.repeat.take() {
                self.repeat.set(rate, delay);
            }
            if self.link.state.outputs.changes != self.outputs_seen {
                self.outputs_seen = self.link.state.outputs.changes;
                self.place_soon();
                self.uncovered.clear();
                self.cover_again_at = None;
                self.catch_up();
                if self.grabbed() {
                    self.cover_outputs();
                }
            }
            self.timers();
            self.link.pump()?;
            if !self.link.state.events.is_empty() || !lock(&self.shared.commands).is_empty() {
                continue;
            }
            let deadline = [
                self.activation.as_ref().and_then(Activation::deadline),
                self.pending.as_ref().map(|p| p.begin_at),
                self.repeat.deadline(),
                self.place_due(),
                self.away_at,
                self.cover_again_at,
            ]
            .into_iter()
            .flatten()
            .min();
            self.link.wait(woken, deadline)?;
        }
    }

    /// The connection is gone: what was down is reported up and, unless
    /// the cursor came back already, the router hears that capture holds
    /// input no more. The agent starts capture again once a connection
    /// runs.
    fn lost(&mut self) {
        self.pending = None;
        self.early.clear();
        self.end();
        self.shared.handover.stop_delivering();
    }

    fn timers(&mut self) {
        let now = Instant::now();
        if self.pending.as_ref().is_some_and(|p| now >= p.begin_at) {
            self.begin();
        }
        self.catch_up();
        if let Some(a) = self.activation.as_mut() {
            let moves = !a.held();
            let verdict = a.tick(now);
            self.follow(verdict, moves);
        }
        if let Some(key) = self.repeat.due(now)
            && self.grabbed()
        {
            self.report(InputEvent::Key {
                usage: key,
                pressed: true,
            });
        }
        if self.place_due().is_some_and(|at| now >= at) {
            self.place();
        }
        // The pointer went to another client's surface, as a screen lock
        // takes it, or a surface here closed under it: the buttons down go
        // up there, so they are reported up here. Moving from one surface
        // here to another (a cover to the next output's) leaves and enters
        // within `AWAY_WAIT`, and lets go of nothing.
        if self.away_at.is_some_and(|at| now >= at) {
            self.away_at = None;
            self.release_buttons();
        }
        if self.cover_again_at.is_some_and(|at| now >= at) {
            self.cover_again_at = None;
            self.uncovered.clear();
            if self.grabbed() {
                self.cover_outputs();
            }
        }
    }

    /// When the strips are put again: not while input is held or a push
    /// waits to begin, which the strips must stay for. The serve loop waits
    /// for it and `timers` places at it, both from here, so the loop never
    /// wakes for a placing that cannot happen yet.
    fn place_due(&self) -> Option<Instant> {
        self.place_at
            .filter(|_| self.activation.is_none() && self.pending.is_none())
    }

    fn place_soon(&mut self) {
        self.place_at = Some(Instant::now() + SETTLE);
    }

    /// A layer surface on `output`, on the overlay layer, ignoring other
    /// surfaces' exclusive zones, not taking the keyboard: anchored to
    /// `anchor`, of `size` (0 stretches between anchors), with margins.
    fn layer(
        &self,
        output: &wl_output::WlOutput,
        anchor: Anchor,
        size: (u32, u32),
        margin_top: i32,
        margin_left: i32,
    ) -> Option<Surface> {
        let state = &self.link.state;
        let (compositor, shell) = (state.compositor.as_ref()?, state.layer_shell.as_ref()?);
        let surface = compositor.create_surface(&state.qh, ());
        let layer = shell.get_layer_surface(
            &surface,
            Some(output),
            Layer::Overlay,
            NAMESPACE.to_owned(),
            &state.qh,
            surface.id(),
        );
        layer.set_anchor(anchor);
        layer.set_size(size.0, size.1);
        layer.set_margin(margin_top, 0, 0, margin_left);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        let viewport = state
            .viewporter
            .as_ref()
            .map(|v| v.get_viewport(&surface, &state.qh, ()));
        surface.commit();
        Some(Surface {
            surface,
            layer,
            viewport,
            buffer: None,
            size: None,
        })
    }

    /// Puts a strip at each barrier the exits make on the outputs, keeping
    /// those already where one is to be, and the cursor on them.
    fn place(&mut self) {
        self.place_at = None;
        let outputs = self.link.geometry().outputs.clone();
        let zones: Vec<Rect> = outputs.iter().map(|o| o.1).collect();
        let unplaced = barriers::on_no_zone(&self.exits, &zones);
        if unplaced.is_empty() {
            self.warned_unplaced = false;
        } else if !self.warned_unplaced {
            warn!(exits = ?unplaced, ?zones, "some exits lie on no output");
            self.warned_unplaced = true;
        }
        let made = barriers::barriers(&self.exits, &zones, self.next_id);
        self.next_id = self.next_id.wrapping_add(made.len() as u32).max(1);
        let (kept, gone): (Vec<Strip>, Vec<Strip>) = std::mem::take(&mut self.strips)
            .into_iter()
            .partition(|s| made.iter().any(|b| b.same_place(&s.barrier)));
        self.strips = kept;
        for strip in gone {
            if self
                .on
                .as_ref()
                .is_some_and(|(s, _)| *s == strip.surface.id())
            {
                self.on = None;
            }
            strip.surface.destroy();
        }
        for barrier in made {
            if self.strips.iter().any(|s| s.barrier.same_place(&barrier)) {
                continue;
            }
            let Some((_, _, output)) = outputs.iter().find(|o| o.1 == barrier.zone) else {
                continue;
            };
            let place = wlroots::edge_surface(&barrier);
            let mut anchor = Anchor::empty();
            for (on, side) in [
                (place.anchors.top, Anchor::Top),
                (place.anchors.bottom, Anchor::Bottom),
                (place.anchors.left, Anchor::Left),
                (place.anchors.right, Anchor::Right),
            ] {
                if on {
                    anchor |= side;
                }
            }
            let Some(surface) = self.layer(
                output,
                anchor,
                (place.width, place.height),
                place.margin_top,
                place.margin_left,
            ) else {
                return;
            };
            debug!(id = barrier.id, edge = ?barrier.edge, first = barrier.first, last = barrier.last, "a strip at an exit");
            self.strips.push(Strip { barrier, surface });
        }
    }

    fn strip(&self, surface: &ObjectId) -> Option<&Strip> {
        self.strips.iter().find(|s| s.surface.id() == *surface)
    }

    fn is_cover(&self, surface: &ObjectId) -> bool {
        self.covers.iter().any(|(_, c)| c.id() == *surface)
    }

    fn command(&mut self, command: Command) {
        let now = Instant::now();
        match command {
            Command::Exits(exits) => {
                if exits != self.exits {
                    self.exits = exits;
                    self.place_soon();
                }
            }
            Command::Grab { meant, grabbed } => {
                let current = self.shared.handover.current();
                let quiet = self.shared.handover.quiet();
                match barriers::grab_taken(meant, grabbed, current, quiet) {
                    GrabTaken::Apply => {
                        if let Some(a) = self.activation.as_mut() {
                            a.set_grabbed(grabbed, now);
                        }
                        if grabbed {
                            self.hold_grabbed();
                        }
                    }
                    GrabTaken::Ended => {
                        let _ = self.shared.handover.report(InputEvent::CaptureEnded);
                    }
                    GrabTaken::Ignore => debug!(meant, current, "a grab of input that ended"),
                }
            }
            Command::Warp(to) => match self.activation.as_mut() {
                Some(a) => {
                    let verdict = a.warped(to);
                    self.follow(verdict, true);
                }
                None => self.move_cursor(to),
            },
            Command::EdgeWait(handled, waiting) => {
                debug_assert!(handled <= self.shared.handover.reported());
                self.catch_up();
                let (verdict, moves) = match self.activation.as_mut() {
                    Some(a) => (a.edge_wait(handled, waiting), !a.held()),
                    None => (Verdict::Keep, false),
                };
                self.follow(verdict, moves);
            }
        }
    }

    fn event(&mut self, event: Event) {
        // The virtual pointer's warp comes back as relative motion to the
        // surface under the cursor, which is no movement of the user's: a
        // strip would take it for a push out.
        if matches!(event, Event::Relative { .. }) && self.warping.is_some() {
            return;
        }
        // Input of a push that has not begun waits for it.
        if self.pending.is_some()
            && matches!(
                event,
                Event::Relative { .. }
                    | Event::Button { .. }
                    | Event::Key { .. }
                    | Event::Axis { .. }
                    | Event::AxisSteps { .. }
                    | Event::Frame
            )
        {
            barriers::keep_early(&mut self.early, event, EARLY_MAX, |e| {
                matches!(e, Event::Relative { .. })
            });
            return;
        }
        match event {
            Event::PointerEnter {
                surface,
                serial,
                x,
                y,
            } => {
                self.away_at = None;
                self.pointer_on = Some(surface.clone());
                if let Some(strip) = self.strip(&surface) {
                    let at = wlroots::on_surface(&strip.barrier, x, y);
                    self.on = Some((surface, at));
                } else if self.is_cover(&surface)
                    && let Some(pointer) = &self.link.state.pointer
                {
                    // Grabbed: hidden while over a cover.
                    pointer.set_cursor(serial, None, 0, 0);
                }
            }
            Event::PointerLeave { surface } => {
                // Off a surface here: unless it enters another of ours
                // soon, the buttons down go up (`timers`).
                self.away_at.get_or_insert(Instant::now() + AWAY_WAIT);
                if self.pointer_on.as_ref() == Some(&surface) {
                    self.pointer_on = None;
                }
                if self.on.as_ref().is_some_and(|(s, _)| *s == surface) {
                    self.on = None;
                    // Off the edge while pushing on it, not grabbed: the
                    // push is over, and the cursor is where the user moved
                    // it. A grab the agent made ahead of its command is
                    // a grab.
                    self.catch_up();
                    let held = self.activation.as_ref().is_some_and(Activation::held);
                    if self.pending.is_some() || held {
                        debug!("the cursor left the edge; letting go of input");
                        self.release(None, false);
                    }
                }
            }
            Event::PointerMotion { x, y } => {
                let moved = (self.on.as_ref())
                    .and_then(|(surface, _)| self.strip(surface))
                    .map(|strip| wlroots::on_surface(&strip.barrier, x, y));
                if let (Some((_, at)), Some(moved)) = (self.on.as_mut(), moved) {
                    *at = moved;
                }
            }
            Event::Relative { dx, dy } => self.relative(dx, dy),
            Event::Button { button, pressed } => {
                let Some(button) = button_of_ei(button) else {
                    return;
                };
                if self.input_begins() {
                    if !self.returning()
                        && let Some(report) = self.pressed.button(button, pressed)
                    {
                        self.report(report);
                    }
                    if pressed {
                        self.pressed_while_held(false);
                    }
                }
            }
            Event::Axis { axis, value } => {
                let value = value as f32;
                match axis {
                    WlAxis::HorizontalScroll => self.smooth.0 += value,
                    _ => self.smooth.1 += value,
                }
            }
            Event::AxisSteps { axis, value120 } => {
                let (x, y) = self.discrete.unwrap_or_default();
                self.discrete = Some(match axis {
                    WlAxis::HorizontalScroll => (x + value120, y),
                    _ => (x, y + value120),
                });
            }
            Event::Frame => {
                let (discrete, smooth) = (self.discrete.take(), std::mem::take(&mut self.smooth));
                // sway's and Hyprland's natural scrolling is not read yet,
                // so not turned back.
                if let Some((dx, dy)) = barriers::scroll_of(discrete, smooth, Natural::default())
                    && self.input_begins()
                {
                    if !self.returning() {
                        self.report(InputEvent::Scroll { dx, dy });
                    }
                    self.pressed_while_held(false);
                }
            }
            Event::KeyboardEnter { surface, keys } => {
                if self.strip(&surface).is_none() {
                    return;
                }
                let modifiers = keys
                    .into_iter()
                    .filter_map(|k| u16::try_from(k).ok().and_then(HidUsage::from_evdev))
                    .filter(|k| k.is_modifier());
                if self.pending.is_some() {
                    // What the push waited for.
                    self.entered = modifiers.collect();
                    self.begin();
                } else if self.input_begins() && !self.returning() {
                    // Held or grabbed, the strip has the keyboard (again):
                    // after the push began without it, or as sway gives it
                    // back once an output came (`KeyboardLeave` reported
                    // the keys up). The modifiers still down are down.
                    for usage in modifiers {
                        if let Some(report) = self.pressed.key(usage, true) {
                            self.report(report);
                        }
                    }
                }
            }
            Event::KeyboardLeave { surface } => {
                self.repeat.clear();
                if self.held_at.as_ref().is_some_and(|(s, _)| *s == surface) {
                    debug!("the strip lost the keyboard; the keys down are let go of");
                    for up in self.pressed.release_keys() {
                        self.report(up);
                    }
                }
            }
            Event::Key { key, pressed } => {
                let Some(usage) = u16::try_from(key).ok().and_then(HidUsage::from_evdev) else {
                    return;
                };
                if self.input_begins() {
                    if !self.returning()
                        && let Some(report) = self.pressed.key(usage, pressed)
                    {
                        self.report(report);
                    }
                    if pressed {
                        // Timed from now even while held at the edge, so a
                        // key still down once grabbed repeats as it would
                        // have: only repeats while grabbed are reported
                        // (`timers`), as on X11.
                        self.repeat.press(usage, Instant::now());
                        let kept = usage.is_modifier() || self.makes_hotkey(usage);
                        self.pressed_while_held(kept);
                    } else {
                        self.repeat.release(usage);
                    }
                }
            }
            Event::Configure {
                surface,
                serial,
                width,
                height,
            } => self.configured(&surface, serial, width, height),
            Event::Fenced(n) => {
                if self.warping == Some(n) {
                    self.warping = None;
                }
            }
            Event::Closed { surface } => {
                if let Some(at) = self.strips.iter().position(|s| s.surface.id() == surface) {
                    debug!("the compositor closed a strip");
                    if self.held_at.as_ref().is_some_and(|(s, _)| *s == surface) {
                        self.release(None, false);
                    }
                    // The cursor is on no strip of ours until it enters
                    // one again.
                    if self.on.as_ref().is_some_and(|(s, _)| *s == surface) {
                        self.on = None;
                    }
                    self.strips.remove(at).surface.destroy();
                    self.place_soon();
                } else if let Some(at) = self.covers.iter().position(|(_, c)| c.id() == surface) {
                    debug!("the compositor closed a cover");
                    // The buttons down go up where the pointer goes from
                    // under it: not if it is over another surface here.
                    let elsewhere = self.pointer_on.as_ref().is_some_and(|on| {
                        *on != surface && (self.strip(on).is_some() || self.is_cover(on))
                    });
                    if !elsewhere {
                        self.release_buttons();
                    }
                    let (output, cover) = self.covers.remove(at);
                    cover.destroy();
                    // Still grabbed, a new one goes over that output as
                    // the outputs change, or a moment later if it stays.
                    self.uncovered.push(output);
                    self.cover_again_at = Some(Instant::now() + COVER_AGAIN);
                }
            }
        }
    }

    /// A surface of `width` × `height` (0: the size it asked for) is to
    /// be shown.
    fn configured(&mut self, surface: &ObjectId, serial: u32, width: u32, height: u32) {
        let (s, asked) = if let Some(strip) =
            self.strips.iter_mut().find(|s| s.surface.id() == *surface)
        {
            let place = wlroots::edge_surface(&strip.barrier);
            (&mut strip.surface, (place.width, place.height))
        } else if let Some((_, cover)) = self.covers.iter_mut().find(|(_, c)| c.id() == *surface) {
            (cover, (1, 1))
        } else {
            return;
        };
        let size = (
            if width == 0 { asked.0 } else { width },
            if height == 0 { asked.1 } else { height },
        );
        s.layer.ack_configure(serial);
        if s.size != Some(size) {
            s.show(&self.link, size);
        }
        s.surface.commit();
    }

    /// Relative motion: a push out through the edge the cursor is on
    /// starts handing input over; held or grabbed, it is the activation's.
    fn relative(&mut self, dx: f64, dy: f64) {
        if self.input_begins() {
            let a = self.activation.as_mut().expect("an activation");
            let moves = !a.held();
            let (report, verdict) = a.moved(dx, dy, Instant::now());
            if let Some(report) = report {
                self.report(report);
            }
            self.follow(verdict, moves);
            return;
        }
        let Some((surface, at)) = self.on.clone() else {
            return;
        };
        let Some(strip) = self.strip(&surface) else {
            return;
        };
        if self.exits.is_empty()
            || !self.shared.capturing()
            || !wlroots::pushes_out(strip.barrier.edge, dx, dy)
        {
            return;
        }
        // The push is reported as it begins (`Activation::new`); this
        // movement, which made it, is not reported again after it, as the
        // portal's desktop keeps the movement that crosses a barrier.
        self.push(surface, at);
    }

    /// The cursor pushes out at `at` on the strip `strip`: the strip takes
    /// the keyboard, and once it has it (with the modifiers held), or a
    /// moment after, the push begins.
    fn push(&mut self, strip: ObjectId, at: Point) {
        let Some(s) = self.strip(&strip) else {
            return;
        };
        s.surface
            .layer
            .set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        s.surface.surface.commit();
        self.entered.clear();
        self.held_at = Some((strip, at));
        self.pending = Some(Pending {
            position: at,
            begin_at: Instant::now() + KEYBOARD_WAIT,
        });
    }

    /// The push begins: the modifiers down are told, then the push through
    /// the edge, then the input that came meanwhile.
    fn begin(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let barrier = self
            .held_at
            .as_ref()
            .and_then(|(id, _)| self.strip(id))
            .map(|s| s.barrier.clone());
        let Some(barrier) = barrier else {
            // Its strip went meanwhile, as the outputs changed.
            self.early.clear();
            self.give_back();
            return;
        };
        let now = Instant::now();
        let (mut activation, push) =
            Activation::new(barrier.id, barrier, Some(pending.position), now);
        self.shared.handover.begin();
        for usage in std::mem::take(&mut self.entered) {
            if let Some(report) = self.pressed.key(usage, true) {
                let _ = self.shared.handover.report(report);
            }
        }
        if let Some(count) = self.shared.handover.report(push) {
            activation.pushed(count);
        }
        self.activation = Some(activation);
        for event in std::mem::take(&mut self.early) {
            self.event(event);
        }
    }

    /// Grabbed: the cursor is on another device. A cover over each output
    /// keeps it over a surface here, hidden, wherever it goes; the
    /// compositor's shortcuts come here too where it lets them.
    fn hold_grabbed(&mut self) {
        if self.covers.is_empty() {
            self.cover_outputs();
        }
        let held = (self.held_at.as_ref())
            .and_then(|(id, _)| self.strip(id))
            .map(|s| s.surface.surface.clone());
        let state = &self.link.state;
        if self.inhibitor.is_none()
            && let (Some(manager), Some(seat)) = (&state.inhibit_manager, &state.seat)
            && let Some(surface) = held
        {
            self.inhibitor = Some(manager.inhibit_shortcuts(&surface, seat, &state.qh, ()));
        }
    }

    /// A cover over each output there is now, as soon as it is there,
    /// described or not; none over one gone.
    fn cover_outputs(&mut self) {
        let outputs: Vec<wl_output::WlOutput> = self
            .link
            .state
            .outputs
            .list
            .iter()
            .map(|o| o.output.clone())
            .collect();
        let (kept, gone): (Vec<_>, Vec<_>) = std::mem::take(&mut self.covers)
            .into_iter()
            .partition(|(output, _)| outputs.iter().any(|o| o.id() == *output));
        self.covers = kept;
        for (_, cover) in gone {
            cover.destroy();
        }
        let all = Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right;
        for output in outputs {
            if self.covers.iter().any(|(o, _)| *o == output.id())
                || self.uncovered.contains(&output.id())
            {
                continue;
            }
            if let Some(cover) = self.layer(&output, all, (0, 0), 0, 0) {
                self.covers.push((output.id(), cover));
            }
        }
    }

    /// The buttons reported down, reported up.
    fn release_buttons(&mut self) {
        for up in self.pressed.release_buttons() {
            self.report(up);
        }
    }

    /// Input held ends: what is down is reported up and, unless the cursor
    /// came back here as the agent asked, the router hears that capture
    /// holds input no more.
    fn end(&mut self) {
        self.shared.handover.end(self.returning());
        let ups = self.pressed.release_all();
        self.shared.handover.report_all(ups);
        self.repeat.clear();
        self.discrete = None;
        self.smooth = (0.0, 0.0);
        if self.activation.take().is_some_and(|a| !a.returning()) {
            let _ = self.shared.handover.report(InputEvent::CaptureEnded);
        }
    }

    /// Does what a verdict says; `moves`: whether letting go puts the
    /// cursor where the verdict says, as it does once grabbed or coming
    /// back. Held at the edge, the cursor is where the user moved it.
    fn follow(&mut self, verdict: Verdict, moves: bool) {
        if let Verdict::Release(at) = verdict {
            self.release(at, moves);
        }
    }

    /// Lets go of input: the covers and the keyboard go back and, if
    /// `moves`, the cursor is put at `at`, or else where input was held,
    /// beside the strip, so the app under it has it again.
    fn release(&mut self, at: Option<Point>, moves: bool) {
        if self.activation.is_none() && self.pending.is_none() && self.covers.is_empty() {
            return;
        }
        debug!(?at, moves, "letting go of input");
        let held = self.held_at.as_ref().map(|(_, p)| *p);
        self.pending = None;
        self.early.clear();
        self.end();
        self.give_back();
        if moves && let Some(to) = at.or(held) {
            self.move_cursor(to);
        }
    }

    /// The covers go, and the strip gives back the keyboard and the
    /// compositor's shortcuts.
    fn give_back(&mut self) {
        for (_, cover) in self.covers.drain(..) {
            cover.destroy();
        }
        self.uncovered.clear();
        self.cover_again_at = None;
        if let Some(inhibitor) = self.inhibitor.take() {
            inhibitor.destroy();
        }
        if let Some((id, _)) = self.held_at.take()
            && let Some(s) = self.strip(&id)
        {
            s.surface
                .layer
                .set_keyboard_interactivity(KeyboardInteractivity::None);
            s.surface.surface.commit();
        }
    }

    /// Puts the cursor at `to`, beside any strip rather than on it.
    fn move_cursor(&mut self, to: Point) {
        let to = wlroots::off_barriers(to, self.strips.iter().map(|s| &s.barrier));
        if let Some(extent) = self.link.geometry().extent {
            let (x, y, width, height) = extent.place(to);
            let time = self.link.time();
            self.pointer.motion_absolute(time, x, y, width, height);
            self.pointer.frame();
            self.warps = self.warps.wrapping_add(1);
            self.warping = Some(self.warps);
            self.link.fence(self.warps);
        }
    }

    fn report(&mut self, event: InputEvent) {
        let count = self.shared.handover.report(event);
        if let (Some(a), Some(count)) = (self.activation.as_mut(), count) {
            a.reported(count, Instant::now());
        }
    }

    fn input_begins(&mut self) -> bool {
        self.catch_up();
        self.activation.is_some()
    }

    /// The agent grabbed input, ahead of its command.
    fn catch_up(&mut self) {
        if self.shared.handover.grabbed_ahead()
            && let Some(a) = self.activation.as_mut()
            && a.held()
        {
            a.set_grabbed(true, Instant::now());
            self.hold_grabbed();
        }
    }

    fn grabbed(&self) -> bool {
        self.activation
            .as_ref()
            .is_some_and(|a| !a.held() && !a.returning())
    }

    fn returning(&self) -> bool {
        self.activation.as_ref().is_some_and(Activation::returning)
    }

    fn makes_hotkey(&self, key: HidUsage) -> bool {
        self.shared.handover.makes_hotkey(key, &self.pressed)
    }

    fn pressed_while_held(&mut self, kept: bool) {
        let (verdict, moves) = match self.activation.as_mut() {
            Some(a) => {
                let moves = !a.held();
                (a.pressed(kept), moves)
            }
            None => (Verdict::Keep, false),
        };
        self.follow(verdict, moves);
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        for strip in self.strips.drain(..) {
            strip.surface.destroy();
        }
        for (_, cover) in self.covers.drain(..) {
            cover.destroy();
        }
        self.pointer.destroy();
        let _ = self.link.pump();
    }
}
