//! What input on a wlroots compositor decides without the compositor:
//! which way input goes in and out
//! of a Wayland desktop, where the virtual pointer puts the cursor, what a
//! scroll is sent as, where the 1-pixel surfaces that catch the cursor at
//! an exit go, and when a captured key repeats.
//!
//! Compositors without the RemoteDesktop and InputCapture portals (sway,
//! Hyprland, river and other wlroots ones) offer protocols of their own
//! instead: a virtual pointer and keyboard (`zwlr_virtual_pointer_v1`,
//! `zwp_virtual_keyboard_v1`) to inject input, and layer surfaces
//! (`zwlr_layer_shell_v1`) and relative pointer motion to capture it at the
//! edges. Which way is decided once, from the compositor's globals alone,
//! without asking the session bus about portals: a compositor that hands
//! these protocols to every client has no gate of its own, so there the
//! user's consent is enabling InputWeave; GNOME (mutter) and KDE (KWin)
//! hand none of them to ordinary clients (KWin has its own fake input, and
//! no virtual pointer or keyboard), so they always go through the portals
//! and the dialog that asks the user.

use std::time::{Duration, Instant};

use iw_core::HidUsage;
use iw_core::geometry::{Point, Rect};
use iw_core::layout::Edge;

use super::barriers::Barrier;
use super::translate::{PIXELS_PER_NOTCH, STEP, Scroll};
use crate::linux::repeat::{KeyRepeat, RepeatPace, never_repeats};

/// The compositor's globals that input without a portal needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Globals {
    pub virtual_pointer: bool,
    pub virtual_keyboard: bool,
    pub layer_shell: bool,
    pub relative_pointer: bool,
    /// Optional: while captured, the compositor's own shortcuts go to the
    /// device the cursor is on too.
    pub shortcuts_inhibit: bool,
    /// Not for input: without it, and without the portals, there is no
    /// clipboard.
    pub data_control: bool,
}

impl Globals {
    /// Whether input from other devices can be injected without a portal.
    pub(crate) fn injects(&self) -> bool {
        self.virtual_pointer && self.virtual_keyboard
    }

    /// Whether this device's input can be captured without a portal:
    /// surfaces at the exits catch the cursor, its motion is read relative,
    /// and a virtual pointer puts it where it comes back.
    pub(crate) fn captures(&self) -> bool {
        self.layer_shell && self.relative_pointer && self.virtual_pointer
    }
}

/// Which way input goes in or out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Way {
    Portal,
    Wlroots,
}

/// How input from other devices is injected: through the virtual pointer
/// and keyboard where the compositor hands them to every client; else
/// through the RemoteDesktop portal, whose dialog asks the user.
pub(crate) fn injection(globals: &Globals) -> Way {
    if globals.injects() {
        Way::Wlroots
    } else {
        Way::Portal
    }
}

/// How this device's input is captured: at layer surfaces where the
/// compositor hands out what that takes; else through the InputCapture
/// portal, whose dialog asks the user.
pub(crate) fn capture(globals: &Globals) -> Way {
    if globals.captures() {
        Way::Wlroots
    } else {
        Way::Portal
    }
}

/// The box that bounds the outputs, over which the compositor spreads the
/// virtual pointer's absolute motion (and puts a point between them on the
/// nearest): worked out once as the outputs change, for every motion
/// after.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Extent {
    left: f64,
    top: f64,
    width: f64,
    height: f64,
}

impl Extent {
    /// The box that bounds `outputs`, in whole units as the protocol takes
    /// them; None without outputs.
    pub(crate) fn of(outputs: &[Rect]) -> Option<Self> {
        let first = outputs.first()?;
        let (mut left, mut top, mut right, mut bottom) =
            (first.left(), first.top(), first.right(), first.bottom());
        for o in &outputs[1..] {
            left = left.min(o.left());
            top = top.min(o.top());
            right = right.max(o.right());
            bottom = bottom.max(o.bottom());
        }
        let (width, height) = ((right - left).round(), (bottom - top).round());
        (width >= 1.0 && height >= 1.0).then_some(Self {
            left,
            top,
            width,
            height,
        })
    }

    /// Where `position`, in the desktop's logical space, is for the
    /// virtual pointer's absolute motion: its offset in the box, and the
    /// box's size.
    pub(crate) fn place(&self, position: Point) -> (u32, u32, u32, u32) {
        let x = (position.x - self.left)
            .round()
            .clamp(0.0, self.width - 1.0);
        let y = (position.y - self.top)
            .round()
            .clamp(0.0, self.height - 1.0);
        (x as u32, y as u32, self.width as u32, self.height as u32)
    }
}

/// An axis of `wl_pointer` and the virtual pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Axis {
    Vertical,
    Horizontal,
}

/// A scroll as the virtual pointer sends it, in one frame: on each axis
/// that moves, its value in logical pixels (positive down or right), and
/// the wheel's notches if it turned.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AxisScroll {
    /// A wheel's (`axis_source` wheel, with `axis_discrete`), or else
    /// smooth (`axis_source` finger, as a trackpad's).
    pub wheel: bool,
    pub moves: Vec<(Axis, f64, Option<i32>)>,
}

/// What a scroll as libei takes it (`translate::scroll`) is for the
/// virtual pointer: a wheel's notches as discrete steps of 15 pixels each,
/// as libinput counts them; smooth scrolling as it is.
pub(crate) fn axis_scroll(scroll: Scroll) -> AxisScroll {
    match scroll {
        Scroll::Steps(x, y) => {
            let steps = |v: i32| (f64::from(v) / STEP).round() as i32;
            let moves = [(Axis::Horizontal, steps(x)), (Axis::Vertical, steps(y))]
                .into_iter()
                .filter(|(_, n)| *n != 0)
                .map(|(axis, n)| (axis, f64::from(n) * PIXELS_PER_NOTCH, Some(n)))
                .collect();
            AxisScroll { wheel: true, moves }
        }
        Scroll::Pixels(x, y) => {
            let moves = [(Axis::Horizontal, x), (Axis::Vertical, y)]
                .into_iter()
                .filter(|(_, v)| *v != 0.0)
                .map(|(axis, v)| (axis, f64::from(v), None))
                .collect();
            AxisScroll {
                wheel: false,
                moves,
            }
        }
    }
}

/// Whether moving by (`dx`, `dy`) pushes out through `edge`.
pub(crate) fn pushes_out(edge: Edge, dx: f64, dy: f64) -> bool {
    let out = edge.outward();
    dx * out.x + dy * out.y > 0.0
}

/// The sides of its output a layer surface is anchored to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Anchors {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

/// Where the surface that catches the cursor at a barrier goes on its
/// output: a pixel thick along the edge, its first to its last pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EdgeSurface {
    pub anchors: Anchors,
    pub width: u32,
    pub height: u32,
    /// From the output's top or left side, for a surface on a left or right
    /// edge or on a top or bottom one.
    pub margin_top: i32,
    pub margin_left: i32,
}

/// The surface for `barrier`, on the output that is its zone.
pub(crate) fn edge_surface(barrier: &Barrier) -> EdgeSurface {
    let zone = barrier.zone;
    let length = (barrier.last - barrier.first + 1).max(1) as u32;
    let along_top = barrier.first - zone.top().round() as i32;
    let along_left = barrier.first - zone.left().round() as i32;
    let (anchors, width, height, margin_top, margin_left) = match barrier.edge {
        Edge::Left => (
            Anchors {
                left: true,
                top: true,
                ..Anchors::default()
            },
            1,
            length,
            along_top,
            0,
        ),
        Edge::Right => (
            Anchors {
                right: true,
                top: true,
                ..Anchors::default()
            },
            1,
            length,
            along_top,
            0,
        ),
        Edge::Top => (
            Anchors {
                top: true,
                left: true,
                ..Anchors::default()
            },
            length,
            1,
            0,
            along_left,
        ),
        Edge::Bottom => (
            Anchors {
                bottom: true,
                left: true,
                ..Anchors::default()
            },
            length,
            1,
            0,
            along_left,
        ),
    };
    EdgeSurface {
        anchors,
        width,
        height,
        margin_top,
        margin_left,
    }
}

/// Where (`sx`, `sy`) on the surface of `barrier` is in the desktop.
pub(crate) fn on_surface(barrier: &Barrier, sx: f64, sy: f64) -> Point {
    let zone = barrier.zone;
    let first = f64::from(barrier.first);
    match barrier.edge {
        Edge::Left => Point::new(zone.left() + sx, first + sy),
        Edge::Right => Point::new(zone.right() - 1.0 + sx, first + sy),
        Edge::Top => Point::new(first + sx, zone.top() + sy),
        Edge::Bottom => Point::new(first + sx, zone.bottom() - 1.0 + sy),
    }
}

/// `p`, or, if the virtual pointer would put it on the pixel a barrier's
/// surface covers (`Extent::place` rounds to whole units), the pixel just
/// inside it: put there, the cursor would be on the surface, which takes it
/// as the cursor pushing out once it moves out. A point past the edge, on
/// another output, stays.
pub(crate) fn off_barriers<'a>(p: Point, barriers: impl IntoIterator<Item = &'a Barrier>) -> Point {
    let (x, y) = (p.x.round(), p.y.round());
    for b in barriers {
        let along = b.edge.coordinate(Point::new(x, y));
        if along < f64::from(b.first)
            || along > f64::from(b.last)
            || !b.zone.contains(Point::new(x, y))
        {
            continue;
        }
        let zone = b.zone;
        match b.edge {
            Edge::Left if x < zone.left() + 1.0 => return Point::new(zone.left() + 1.0, y),
            Edge::Right if x >= zone.right() - 1.0 => return Point::new(zone.right() - 2.0, y),
            Edge::Top if y < zone.top() + 1.0 => return Point::new(x, zone.top() + 1.0),
            Edge::Bottom if y >= zone.bottom() - 1.0 => return Point::new(x, zone.bottom() - 2.0),
            _ => {}
        }
    }
    p
}

/// A keymap for the virtual keyboard when the seat has none to copy (no
/// keyboard): US, compiled by the compositor from its xkb data.
pub(crate) const US_KEYMAP: &str = "xkb_keymap {\n\
    \txkb_keycodes { include \"evdev+aliases(qwerty)\" };\n\
    \txkb_types { include \"complete\" };\n\
    \txkb_compat { include \"complete\" };\n\
    \txkb_symbols { include \"pc+us+inet(evdev)\" };\n\
    \txkb_geometry { include \"pc(pc105)\" };\n\
    };\n";

/// Repeats the key held while captured, as Wayland clients repeat keys
/// themselves (`wl_keyboard.repeat_info`): the compositor sends a key down
/// once, and other devices need the repeats (`linux::repeat`).
#[derive(Debug, Default)]
pub(crate) struct Repeat {
    pace: Pace,
    keys: KeyRepeat<HidUsage>,
}

/// The fastest repeat taken from the compositor's `repeat_info`, which
/// bounds none: faster, the loop would repeat without pause.
const MAX_RATE: i32 = 100;

/// The compositor's `repeat_info`.
#[derive(Clone, Copy, Debug)]
struct Pace {
    /// Keys a second; 0 repeats none.
    rate: i32,
    delay: Duration,
}

impl Default for Pace {
    /// libinput's and most desktops' defaults until the compositor says.
    fn default() -> Self {
        Self {
            rate: 25,
            delay: Duration::from_millis(600),
        }
    }
}

impl RepeatPace<HidUsage> for Pace {
    fn repeats(&self, _: HidUsage, key: HidUsage) -> bool {
        self.rate > 0 && !never_repeats(key)
    }

    fn delay(&self) -> Duration {
        self.delay
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(1) / u32::try_from(self.rate.max(1)).unwrap_or(1)
    }
}

impl Repeat {
    /// The compositor's `repeat_info`.
    pub(crate) fn set(&mut self, rate: i32, delay_ms: i32) {
        self.pace = Pace {
            rate: rate.clamp(0, MAX_RATE),
            delay: Duration::from_millis(u64::try_from(delay_ms.max(0)).unwrap_or(0)),
        };
        if self.pace.rate == 0 {
            self.clear();
        }
    }

    /// `key` went down at `now`: it repeats after the delay, unless it is a
    /// modifier or a lock key, as with every keyboard.
    pub(crate) fn press(&mut self, key: HidUsage, now: Instant) {
        self.keys.press(key, key, &self.pace, now);
    }

    /// `key` went up: it repeats no more.
    pub(crate) fn release(&mut self, key: HidUsage) {
        self.keys.release(key);
    }

    pub(crate) fn clear(&mut self) {
        self.keys.clear();
    }

    /// When the next repeat is due.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.keys.deadline()
    }

    /// The key to repeat now, if its time came.
    pub(crate) fn due(&mut self, now: Instant) -> Option<HidUsage> {
        self.keys.due(now, &self.pace)
    }
}

#[cfg(test)]
mod tests {
    use iw_keymap::usage;

    use super::*;

    const ALL: Globals = Globals {
        virtual_pointer: true,
        virtual_keyboard: true,
        layer_shell: true,
        relative_pointer: true,
        shortcuts_inhibit: true,
        data_control: true,
    };

    #[test]
    fn the_protocols_where_the_compositor_has_them_else_the_portals() {
        // sway, Hyprland, river: the protocols.
        assert_eq!(injection(&ALL), Way::Wlroots);
        assert_eq!(capture(&ALL), Way::Wlroots);
        // GNOME: none of them, the portals.
        let none = Globals::default();
        assert_eq!(injection(&none), Way::Portal);
        assert_eq!(capture(&none), Way::Portal);
        // KDE: layer shell and relative pointer, but no virtual pointer or
        // keyboard: the portals both ways.
        let kde = Globals {
            layer_shell: true,
            relative_pointer: true,
            shortcuts_inhibit: true,
            ..Globals::default()
        };
        assert_eq!(injection(&kde), Way::Portal);
        assert_eq!(capture(&kde), Way::Portal);
    }

    #[test]
    fn each_way_needs_all_its_protocols() {
        let only_pointer = Globals {
            virtual_pointer: true,
            ..Globals::default()
        };
        assert!(!only_pointer.injects());
        assert_eq!(injection(&only_pointer), Way::Portal);
        let no_relative = Globals {
            relative_pointer: false,
            ..ALL
        };
        assert!(no_relative.injects());
        assert!(!no_relative.captures());
        assert_eq!(capture(&no_relative), Way::Portal);
        // Capture puts the cursor back with a virtual pointer of its own.
        let no_pointer = Globals {
            virtual_pointer: false,
            ..ALL
        };
        assert!(!no_pointer.captures());
        // The shortcuts inhibitor is optional.
        let no_inhibit = Globals {
            shortcuts_inhibit: false,
            ..ALL
        };
        assert!(no_inhibit.captures());
    }

    #[test]
    fn absolute_motion_spans_the_box_of_the_outputs() {
        let outputs = [
            Rect::new(0.0, 0.0, 1920.0, 1080.0),
            Rect::new(1920.0, -200.0, 1280.0, 720.0),
        ];
        let extent = Extent::of(&outputs).unwrap();
        assert_eq!(extent.place(Point::new(0.0, 0.0)), (0, 200, 3200, 1280));
        assert_eq!(
            extent.place(Point::new(2500.4, 100.6)),
            (2500, 301, 3200, 1280)
        );
        // Off the box: on its edge.
        assert_eq!(
            extent.place(Point::new(-50.0, 5000.0)),
            (0, 1279, 3200, 1280)
        );
        assert_eq!(
            extent.place(Point::new(3200.0, 0.0)),
            (3199, 200, 3200, 1280)
        );
        assert_eq!(Extent::of(&[]), None);
        assert_eq!(Extent::of(&[Rect::new(5.0, 5.0, 0.2, 100.0)]), None);
    }

    #[test]
    fn wheels_scroll_in_notches_and_trackpads_smoothly() {
        assert_eq!(
            axis_scroll(Scroll::Steps(0, 240)),
            AxisScroll {
                wheel: true,
                moves: vec![(Axis::Vertical, 30.0, Some(2))],
            }
        );
        assert_eq!(
            axis_scroll(Scroll::Steps(-120, -120)),
            AxisScroll {
                wheel: true,
                moves: vec![
                    (Axis::Horizontal, -15.0, Some(-1)),
                    (Axis::Vertical, -15.0, Some(-1)),
                ],
            }
        );
        assert_eq!(
            axis_scroll(Scroll::Pixels(0.0, -7.5)),
            AxisScroll {
                wheel: false,
                moves: vec![(Axis::Vertical, -7.5, None)],
            }
        );
    }

    fn barrier(edge: Edge, first: i32, last: i32) -> Barrier {
        let zone = Rect::new(100.0, 50.0, 1920.0, 1080.0);
        let at = match edge {
            Edge::Left => 100,
            Edge::Right => 2020,
            Edge::Top => 50,
            Edge::Bottom => 1130,
        };
        Barrier {
            id: 1,
            edge,
            zone,
            display: zone,
            at,
            first,
            last,
        }
    }

    #[test]
    fn edge_surfaces_lie_a_pixel_thick_along_their_stretch() {
        let right = edge_surface(&barrier(Edge::Right, 70, 969));
        assert_eq!(
            right,
            EdgeSurface {
                anchors: Anchors {
                    right: true,
                    top: true,
                    ..Anchors::default()
                },
                width: 1,
                height: 900,
                margin_top: 20,
                margin_left: 0,
            }
        );
        let bottom = edge_surface(&barrier(Edge::Bottom, 100, 2019));
        assert!(bottom.anchors.bottom);
        assert_eq!((bottom.width, bottom.height), (1920, 1));
        assert_eq!(bottom.margin_left, 0);
        let left = edge_surface(&barrier(Edge::Left, 50, 50));
        assert_eq!((left.width, left.height, left.margin_top), (1, 1, 0));
        let top = edge_surface(&barrier(Edge::Top, 300, 399));
        assert_eq!((top.width, top.height, top.margin_left), (100, 1, 200));
    }

    #[test]
    fn a_point_on_a_surface_is_on_the_edge_it_covers() {
        let right = barrier(Edge::Right, 70, 969);
        assert_eq!(on_surface(&right, 0.5, 10.0), Point::new(2019.5, 80.0));
        let left = barrier(Edge::Left, 70, 969);
        assert_eq!(on_surface(&left, 0.0, 0.0), Point::new(100.0, 70.0));
        let top = barrier(Edge::Top, 300, 399);
        assert_eq!(on_surface(&top, 5.0, 0.5), Point::new(305.0, 50.5));
        let bottom = barrier(Edge::Bottom, 300, 399);
        assert_eq!(on_surface(&bottom, 5.0, 0.0), Point::new(305.0, 1129.0));
    }

    #[test]
    fn the_cursor_is_put_beside_a_surface_not_on_it() {
        let barriers = [barrier(Edge::Right, 70, 969), barrier(Edge::Top, 300, 399)];
        // On the right edge's pixel, or rounding to it: the one inside.
        assert_eq!(
            off_barriers(Point::new(2019.4, 500.0), &barriers),
            Point::new(2018.0, 500.0)
        );
        assert_eq!(
            off_barriers(Point::new(2018.6, 500.2), &barriers),
            Point::new(2018.0, 500.0)
        );
        // Past its stretch, or inside: as it is.
        assert_eq!(
            off_barriers(Point::new(2019.4, 1000.0), &barriers),
            Point::new(2019.4, 1000.0)
        );
        assert_eq!(
            off_barriers(Point::new(2018.0, 500.0), &barriers),
            Point::new(2018.0, 500.0)
        );
        assert_eq!(
            off_barriers(Point::new(350.0, 50.0), &barriers),
            Point::new(350.0, 51.0)
        );
        // Past the edge, on another output: as it is.
        assert_eq!(
            off_barriers(Point::new(2500.0, 500.0), &barriers),
            Point::new(2500.0, 500.0)
        );
        assert_eq!(
            off_barriers(Point::new(350.0, 20.0), &barriers),
            Point::new(350.0, 20.0)
        );
    }

    #[test]
    fn only_movement_out_through_the_edge_pushes() {
        assert!(pushes_out(Edge::Right, 3.0, -1.0));
        assert!(!pushes_out(Edge::Right, 0.0, 5.0));
        assert!(!pushes_out(Edge::Right, -1.0, 0.0));
        assert!(pushes_out(Edge::Top, 0.0, -0.5));
        assert!(pushes_out(Edge::Left, -2.0, 0.0));
        assert!(pushes_out(Edge::Bottom, 0.0, 1.0));
    }

    #[test]
    fn a_held_key_repeats_at_the_compositors_pace() {
        let start = Instant::now();
        let mut repeat = Repeat::default();
        repeat.set(10, 500);
        repeat.press(usage::A, start);
        assert_eq!(repeat.due(start), None);
        assert_eq!(repeat.deadline(), Some(start + Duration::from_millis(500)));
        assert_eq!(
            repeat.due(start + Duration::from_millis(500)),
            Some(usage::A)
        );
        assert_eq!(repeat.deadline(), Some(start + Duration::from_millis(600)));
        assert_eq!(repeat.due(start + Duration::from_millis(550)), None);
        assert_eq!(
            repeat.due(start + Duration::from_millis(600)),
            Some(usage::A)
        );
        // Late: one repeat now, and the next an interval from now, not at
        // once.
        let late = start + Duration::from_millis(2000);
        assert_eq!(repeat.due(late), Some(usage::A));
        assert_eq!(repeat.due(late), None);
        assert_eq!(repeat.deadline(), Some(late + Duration::from_millis(100)));
        // A little late, it keeps to its pace.
        let little = late + Duration::from_millis(130);
        assert_eq!(repeat.due(little), Some(usage::A));
        assert_eq!(repeat.deadline(), Some(late + Duration::from_millis(200)));
        // Another key up leaves it; its own up stops it.
        repeat.release(usage::C);
        assert!(repeat.deadline().is_some());
        repeat.release(usage::A);
        assert_eq!(repeat.deadline(), None);
        // Modifiers and locks never repeat; nor anything at rate 0.
        repeat.press(usage::LEFT_SHIFT, start);
        repeat.press(usage::CAPS_LOCK, start);
        assert_eq!(repeat.deadline(), None);
        repeat.set(0, 500);
        repeat.press(usage::A, start);
        assert_eq!(repeat.deadline(), None);
    }

    #[test]
    fn a_rate_beyond_a_hundred_a_second_repeats_at_a_hundred() {
        let start = Instant::now();
        let mut repeat = Repeat::default();
        repeat.set(i32::MAX, 0);
        repeat.press(usage::A, start);
        assert_eq!(repeat.due(start), Some(usage::A));
        assert_eq!(repeat.deadline(), Some(start + Duration::from_millis(10)));
        // Held down when it is set to 0: it repeats no more.
        repeat.set(0, 0);
        assert_eq!(repeat.deadline(), None);
        repeat.set(-5, 0);
        repeat.press(usage::A, start);
        assert_eq!(repeat.deadline(), None);
    }
}
