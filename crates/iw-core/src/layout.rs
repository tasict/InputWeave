//! The shared arrangement of every device's displays on one canvas.
//!
//! Each device reports its displays in its own desktop coordinates. The user
//! places whole devices on the canvas, which gives each device an `offset`;
//! a display's canvas bounds are its desktop bounds translated by that offset.
//!
//! When the cursor leaves a display through one of its edges, it goes to the
//! nearest display lying beyond that edge, at the point of that display's
//! facing edge nearest to where it left; a gap between the two is skipped. If
//! the nearest is another display of the same device, it stays, as that
//! device's system does. Where displays of two different devices share an edge, that
//! point is where the cursor left, so it passes straight through: that shared
//! edge segment is a [`Seam`].

use std::fmt;

use crate::geometry::{EPSILON, Point, Rect, approx_eq};
use crate::hotkey::{JUMP_SLOTS, Modifier};
use crate::speed::CursorSpeed;

/// How far short of a display's exclusive far end the cursor enters it,
/// when it slides along an edge to get there.
const INSET: f64 = 1e-3;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(String);

impl DeviceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    pub id: String,
    /// Bounds in the owning device's desktop coordinates.
    pub bounds: Rect,
    pub scale: f64,
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: DeviceId,
    pub name: String,
    pub displays: Vec<Display>,
    /// Position of the device's desktop origin on the canvas.
    pub offset: Point,
}

impl Device {
    pub fn to_canvas(&self, local: Point) -> Point {
        local + self.offset
    }

    pub fn to_local(&self, canvas: Point) -> Point {
        canvas - self.offset
    }

    pub fn canvas_bounds(&self) -> impl Iterator<Item = Rect> + '_ {
        self.displays
            .iter()
            .map(|d| d.bounds.translate(self.offset))
    }

    /// The primary display, or the first one if none is marked primary.
    pub fn primary_display(&self) -> Option<&Display> {
        self.displays
            .iter()
            .find(|d| d.primary)
            .or_else(|| self.displays.first())
    }

    /// The display nearest canvas point `p`, with its canvas bounds.
    pub(crate) fn nearest_display(&self, p: Point) -> Option<(&Display, Rect)> {
        self.displays
            .iter()
            .map(|d| (d, d.bounds.translate(self.offset)))
            .min_by(|a, b| a.1.distance_squared(p).total_cmp(&b.1.distance_squared(p)))
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum LayoutError {
    #[error("device {0} appears more than once")]
    DuplicateDevice(DeviceId),
    #[error("device {0} has no displays")]
    NoDisplays(DeviceId),
    #[error("displays of {0} and {1} overlap")]
    Overlap(DeviceId, DeviceId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    /// A vertical edge at a fixed x; `first` is on the left.
    Vertical,
    /// A horizontal edge at a fixed y; `first` is on top.
    Horizontal,
}

/// An edge segment shared by displays of two different devices. The cursor
/// passes straight through it; elsewhere on an edge it slides to the nearest
/// display beyond.
#[derive(Clone, Debug, PartialEq)]
pub struct Seam {
    pub first: DeviceId,
    pub second: DeviceId,
    pub orientation: Orientation,
    /// The fixed coordinate of the edge (x for vertical, y for horizontal).
    pub at: f64,
    /// Extent along the edge.
    pub start: f64,
    pub end: f64,
}

/// Which side of a device a newly arranged device goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    #[default]
    Right,
    Left,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgePolicy {
    /// Distance from a corner of the display the cursor is on within which
    /// it does not cross, so hot corners and window snapping keep working.
    pub corner_margin: f64,
    /// How long, in milliseconds, the cursor must push against an edge
    /// before it crosses; 0 crosses at once. The router applies it, since
    /// it takes the time; `Layout::step` ignores it.
    pub dwell_ms: u32,
    /// The cursor crosses an edge only while this modifier is held, either
    /// way; none crosses whenever it reaches one. Jumping to a device is
    /// not crossing. The router applies it, since it knows which keys are
    /// held; `Layout::step` ignores it.
    pub switch_modifier: Option<Modifier>,
}

impl Default for EdgePolicy {
    fn default() -> Self {
        Self {
            corner_margin: 20.0,
            dwell_ms: 0,
            switch_modifier: None,
        }
    }
}

/// Where a pointer movement ends up.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub device: DeviceId,
    /// Canvas coordinates.
    pub position: Point,
    /// Where it left one device for another through an edge, if it did.
    pub crossing: Option<Crossing>,
}

/// A point on an edge of one of a device's displays: where the cursor left
/// or entered it (`Crossing`).
#[derive(Clone, Debug, PartialEq)]
pub struct EdgeSpot {
    pub device: DeviceId,
    /// The display whose edge it is.
    pub display: String,
    pub edge: Edge,
    /// Where along the edge, in the device's own desktop coordinates: y on
    /// a left or right edge, x on a top or bottom one.
    pub along: f64,
}

/// The cursor crossed from one device to another through an edge (a
/// `step` that changed device): where it left the first and where it
/// entered the second, the facing edge of the display it reached, at the
/// point nearest to where it left (across a gap, too).
#[derive(Clone, Debug, PartialEq)]
pub struct Crossing {
    pub left: EdgeSpot,
    pub entered: EdgeSpot,
}

/// An edge of a display.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// A stretch of one of a device's display edges through which the cursor,
/// moving straight out, goes to another device (`Layout::exits`). In that
/// device's own desktop coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Exit {
    /// The display whose edge it is, and its bounds.
    pub display: String,
    pub bounds: Rect,
    pub edge: Edge,
    /// Where the edge lies: x for a left or right edge, y for a top or
    /// bottom one.
    pub at: f64,
    /// Its extent along the edge, from `start` up to `end`.
    pub start: f64,
    pub end: f64,
}

/// How long a stretch of an edge `Layout::exits` decides for at a time.
const EXIT_STEP: f64 = 1.0;

impl Edge {
    const ALL: [Edge; 4] = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom];

    /// Where this edge of `source` lies (x or y), and its extent along it.
    pub fn extent(self, source: Rect) -> (f64, f64, f64) {
        match self {
            Edge::Left => (source.left(), source.top(), source.bottom()),
            Edge::Right => (source.right(), source.top(), source.bottom()),
            Edge::Top => (source.top(), source.left(), source.right()),
            Edge::Bottom => (source.bottom(), source.left(), source.right()),
        }
    }

    /// The edge facing this one across a crossing: a right edge faces a
    /// left one.
    pub fn opposite(self) -> Edge {
        match self {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }

    /// One unit straight out through this edge.
    pub fn outward(self) -> Point {
        match self {
            Edge::Left => Point::new(-1.0, 0.0),
            Edge::Right => Point::new(1.0, 0.0),
            Edge::Top => Point::new(0.0, -1.0),
            Edge::Bottom => Point::new(0.0, 1.0),
        }
    }

    /// The point `along` this edge of `source`.
    pub fn point(self, source: Rect, along: f64) -> Point {
        let (at, _, _) = self.extent(source);
        match self {
            Edge::Left | Edge::Right => Point::new(at, along),
            Edge::Top | Edge::Bottom => Point::new(along, at),
        }
    }

    /// Where point `p` on this edge of `source` lies along it, and the
    /// edge's extent.
    pub fn along(self, source: Rect, p: Point) -> (f64, f64, f64) {
        match self {
            Edge::Left | Edge::Right => (p.y, source.top(), source.bottom()),
            Edge::Top | Edge::Bottom => (p.x, source.left(), source.right()),
        }
    }

    /// Where point `p` lies along this edge: its y on a left or right
    /// edge, its x on a top or bottom one.
    pub fn coordinate(self, p: Point) -> f64 {
        match self {
            Edge::Left | Edge::Right => p.y,
            Edge::Top | Edge::Bottom => p.x,
        }
    }

    /// Whether `r` lies wholly beyond this edge of `source`. It may reach
    /// back across the edge by `EPSILON`, as edges that touch may.
    fn is_beyond(self, source: Rect, r: Rect) -> bool {
        match self {
            Edge::Right => r.left() >= source.right() - EPSILON,
            Edge::Left => r.right() <= source.left() + EPSILON,
            Edge::Bottom => r.top() >= source.bottom() - EPSILON,
            Edge::Top => r.bottom() <= source.top() + EPSILON,
        }
    }

    /// The way from point `p` on this edge of `source` to the nearest point
    /// of the edge of `r` that faces it: across the gap between the two
    /// edges, and along them into `r`'s extent. Zero where they touch at `p`.
    fn offset_to(self, source: Rect, r: Rect, p: Point) -> Point {
        match self {
            Edge::Right => Point::new(
                r.left() - source.right(),
                into_range(p.y, r.top(), r.bottom()),
            ),
            Edge::Left => Point::new(
                r.right() - source.left(),
                into_range(p.y, r.top(), r.bottom()),
            ),
            Edge::Bottom => Point::new(
                into_range(p.x, r.left(), r.right()),
                r.top() - source.bottom(),
            ),
            Edge::Top => Point::new(
                into_range(p.x, r.left(), r.right()),
                r.bottom() - source.top(),
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    devices: Vec<Device>,
}

impl Layout {
    pub fn new(devices: Vec<Device>) -> Result<Self, LayoutError> {
        for (i, d) in devices.iter().enumerate() {
            if d.displays.is_empty() {
                return Err(LayoutError::NoDisplays(d.id.clone()));
            }
            if devices[..i].iter().any(|other| other.id == d.id) {
                return Err(LayoutError::DuplicateDevice(d.id.clone()));
            }
        }
        for (i, a) in devices.iter().enumerate() {
            for b in &devices[i + 1..] {
                let overlapping = a
                    .canvas_bounds()
                    .any(|ra| b.canvas_bounds().any(|rb| overlaps(ra, rb)));
                if overlapping {
                    return Err(LayoutError::Overlap(a.id.clone(), b.id.clone()));
                }
            }
        }
        Ok(Self { devices })
    }

    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    pub fn device(&self, id: &DeviceId) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == *id)
    }

    /// The devices jumped to by number, the first being number 1: from left
    /// to right by the left edge of their displays on the canvas, then from
    /// top to bottom by their top edge, at most [`JUMP_SLOTS`].
    pub fn jump_order(&self) -> Vec<DeviceId> {
        let corner = |d: &Device| {
            d.canvas_bounds()
                .fold((f64::INFINITY, f64::INFINITY), |(x, y), r| {
                    (x.min(r.left()), y.min(r.top()))
                })
        };
        let mut devices: Vec<((f64, f64), &DeviceId)> =
            self.devices.iter().map(|d| (corner(d), &d.id)).collect();
        devices.sort_by(|((ax, ay), a), ((bx, by), b)| {
            ax.total_cmp(bx).then(ay.total_cmp(by)).then(a.cmp(b))
        });
        devices
            .into_iter()
            .take(usize::from(JUMP_SLOTS))
            .map(|(_, id)| id.clone())
            .collect()
    }

    /// The device whose display contains the canvas point `p`.
    pub fn device_at(&self, p: Point) -> Option<&Device> {
        self.devices
            .iter()
            .find(|d| d.canvas_bounds().any(|r| r.contains(p)))
    }

    pub fn seams(&self) -> Vec<Seam> {
        let mut seams = Vec::new();
        for p_dev in &self.devices {
            for q_dev in &self.devices {
                if p_dev.id == q_dev.id {
                    continue;
                }
                for p in p_dev.canvas_bounds() {
                    for q in q_dev.canvas_bounds() {
                        if approx_eq(p.right(), q.left()) {
                            let (start, end) = (p.top().max(q.top()), p.bottom().min(q.bottom()));
                            if end - start > EPSILON {
                                seams.push(Seam {
                                    first: p_dev.id.clone(),
                                    second: q_dev.id.clone(),
                                    orientation: Orientation::Vertical,
                                    at: q.left(),
                                    start,
                                    end,
                                });
                            }
                        }
                        if approx_eq(p.bottom(), q.top()) {
                            let (start, end) = (p.left().max(q.left()), p.right().min(q.right()));
                            if end - start > EPSILON {
                                seams.push(Seam {
                                    first: p_dev.id.clone(),
                                    second: q_dev.id.clone(),
                                    orientation: Orientation::Horizontal,
                                    at: q.top(),
                                    start,
                                    end,
                                });
                            }
                        }
                    }
                }
            }
        }
        seams
    }

    /// Moves a pointer at canvas point `from`, currently on device `current`,
    /// by `delta`.
    ///
    /// The pointer moves freely among `current`'s displays. When it would
    /// leave them, it crosses to the nearest display beyond the edge it
    /// leaves through, if that display belongs to a device `can_enter` allows
    /// rather than to `current`, and the pointer does not leave within the
    /// corner margin. Otherwise it is held inside the display it started on.
    /// Returns `None` if `current` is not part of the layout.
    pub fn step(
        &self,
        current: &DeviceId,
        from: Point,
        delta: Point,
        policy: &EdgePolicy,
        can_enter: impl Fn(&DeviceId) -> bool,
    ) -> Option<Step> {
        self.step_at(current, from, delta, policy, can_enter, |_| {
            CursorSpeed::NORMAL
        })
        .map(|(step, _)| step)
    }

    /// Like [`step`](Self::step), with the pointer moving at `speed(device)`
    /// on each device (`CursorSpeed::apply`): on `current` until it crosses,
    /// and on the device it enters for the rest of `delta`. Also returns
    /// `delta` as the pointer moves on `current`.
    pub fn step_at(
        &self,
        current: &DeviceId,
        from: Point,
        delta: Point,
        policy: &EdgePolicy,
        can_enter: impl Fn(&DeviceId) -> bool,
        speed: impl Fn(&DeviceId) -> CursorSpeed,
    ) -> Option<(Step, Point)> {
        let here = speed(current);
        let delta = here.apply(delta);
        // How much farther, or less far, what is left of the movement goes
        // on the device entered than it would have here; a speed is never
        // 0, so this always has a meaning.
        let beyond = |entered: &DeviceId| speed(entered).factor() / here.factor();
        let device = self.device(current)?;
        let (display, source) = device.nearest_display(from)?;
        let from = source.clamp(from);
        let target = from + delta;

        if device.canvas_bounds().any(|r| r.contains(target)) {
            let step = Step {
                device: current.clone(),
                position: target,
                crossing: None,
            };
            return Some((step, delta));
        }
        let step = self
            .cross(
                device, display, source, from, delta, policy, can_enter, beyond,
            )
            .unwrap_or_else(|| Step {
                device: current.clone(),
                position: source.clamp(target),
                crossing: None,
            });
        Some((step, delta))
    }

    /// Where a pointer moving from `from` on display `source` of `current` by
    /// `delta`, out of `current`'s displays, goes to another device, if it
    /// does.
    ///
    /// It leaves `source` through the edge its path reaches first, and not
    /// within the corner margin of `source`. The candidates are the displays
    /// lying wholly beyond that edge, of `current` itself and of the other
    /// devices `can_enter` allows. The one whose facing edge is nearest to
    /// the crossing point wins. If that is one of `current`'s own, the
    /// pointer stays: its system keeps a wall there, and the pointer never
    /// skips past this device's own display to a farther device. Otherwise
    /// the pointer enters at the nearest point of that edge and carries on
    /// with the part of `delta` past the crossing, `beyond(device)` times as
    /// far, so a gap is skipped. Where the edges touch and `beyond` is 1,
    /// that is right where the path goes, to the bit.
    #[allow(clippy::too_many_arguments)]
    fn cross(
        &self,
        current: &Device,
        source_display: &Display,
        source: Rect,
        from: Point,
        delta: Point,
        policy: &EdgePolicy,
        can_enter: impl Fn(&DeviceId) -> bool,
        beyond: impl Fn(&DeviceId) -> f64,
    ) -> Option<Step> {
        let (t, edge) = exit(source, from, delta)?;
        let crossing = Point::new(from.x + delta.x * t, from.y + delta.y * t);
        let (along, low, high) = edge.along(source, crossing);
        if along - low < policy.corner_margin || high - along < policy.corner_margin {
            return None;
        }
        let candidates = self
            .devices
            .iter()
            .filter(|d| d.id == current.id || can_enter(&d.id))
            .flat_map(|d| {
                d.displays
                    .iter()
                    .map(move |display| ((d, display), display.bounds.translate(d.offset)))
            });
        let ((device, entered_display), display, offset) =
            nearest_beyond(edge, source, crossing, candidates)?;
        if device.id == current.id {
            return None;
        }
        // The facing point plus what is left of `delta`, put this way so that
        // where the edges touch it is exactly where the path goes.
        // What is past the crossing goes `beyond` times as far: `beyond - 1`
        // times it more, which is nothing at all when `beyond` is 1.
        let rest = from + delta - crossing;
        let entry = from + delta + offset + rest.scaled(beyond(&device.id) - 1.0);
        let position = if device.canvas_bounds().any(|r| r.contains(entry)) {
            entry
        } else {
            display.clamp(entry)
        };
        // Where it left, and the facing point it reached, each in its own
        // device's desktop coordinates.
        let facing = crossing + offset;
        let left = EdgeSpot {
            device: current.id.clone(),
            display: source_display.id.clone(),
            edge,
            along: edge.coordinate(current.to_local(crossing)),
        };
        let entered_edge = edge.opposite();
        let entered = EdgeSpot {
            device: device.id.clone(),
            display: entered_display.id.clone(),
            edge: entered_edge,
            along: entered_edge.coordinate(device.to_local(facing)),
        };
        Some(Step {
            device: device.id.clone(),
            position,
            crossing: Some(Crossing { left, entered }),
        })
    }

    /// Where the cursor leaves `current` for another device: the stretches
    /// of its displays' edges from which a movement straight out crosses,
    /// as `step` decides, with `can_enter` saying which devices it may
    /// enter. In `current`'s desktop coordinates; empty if it is not part
    /// of the layout.
    ///
    /// The corner margin is left out, since the cursor does not cross
    /// there. Otherwise this is where the Windows app puts its strips that
    /// catch a drag of files (`DragStrips.cs`), and it is found the same
    /// way: each unit along an edge goes to the nearest display beyond it,
    /// so a stretch's ends are within a unit of where `step` changes. That
    /// walks every edge, which is quick enough not to work out where `step`
    /// changes instead: about 0.2 ms (2 ms unoptimized) for three 4K
    /// displays with devices on every side, on an M-series Mac
    /// (`exits_take_this_long_for_a_large_desk`).
    pub fn exits(
        &self,
        current: &DeviceId,
        policy: &EdgePolicy,
        can_enter: impl Fn(&DeviceId) -> bool,
    ) -> Vec<Exit> {
        let Some(device) = self.device(current) else {
            return Vec::new();
        };
        // In the order `cross` weighs them, so a tie goes the same way.
        let candidates: Vec<(bool, Rect)> = self
            .devices
            .iter()
            .filter(|d| d.id == *current || can_enter(&d.id))
            .flat_map(|d| {
                let own = d.id == *current;
                d.canvas_bounds().map(move |r| (own, r))
            })
            .collect();
        let mut exits = Vec::new();
        for display in &device.displays {
            let source = display.bounds.translate(device.offset);
            for edge in Edge::ALL {
                if !candidates
                    .iter()
                    .any(|(own, r)| !own && edge.is_beyond(source, *r))
                {
                    continue;
                }
                let (at, low, high) = edge.extent(source);
                let (low, high) = (low + policy.corner_margin, high - policy.corner_margin);
                // As `cross` decides, for the point along the edge.
                let crosses = |along: f64| {
                    let point = edge.point(source, along);
                    nearest_beyond(edge, source, point, candidates.iter().copied())
                        .is_some_and(|(own, _, _)| !own)
                };
                let (shift_at, shift_along) = match edge {
                    Edge::Left | Edge::Right => (device.offset.x, device.offset.y),
                    Edge::Top | Edge::Bottom => (device.offset.y, device.offset.x),
                };
                let mut push = |start: f64, end: f64| {
                    exits.push(Exit {
                        display: display.id.clone(),
                        bounds: display.bounds,
                        edge,
                        at: at - shift_at,
                        start: start - shift_along,
                        end: end - shift_along,
                    })
                };
                let mut open = None;
                let mut from = low;
                while from < high {
                    let to = (from + EXIT_STEP).min(high);
                    match (crosses((from + to) / 2.0), open) {
                        (true, None) => open = Some(from),
                        (false, Some(start)) => {
                            push(start, from);
                            open = None;
                        }
                        _ => {}
                    }
                    from = to;
                }
                if let Some(start) = open {
                    push(start, high);
                }
            }
        }
        exits
    }

    /// Where to put a device that has `displays` (in its own desktop
    /// coordinates) so that it sits beside `anchor` on `side`, with the tops
    /// of the two primary displays aligned. If that spot is taken, it goes
    /// beyond every device on that side instead, where it cannot overlap
    /// anything. Returns the device's offset, or `None` if `anchor` is not in
    /// the layout or `displays` is empty.
    pub fn place_beside(
        &self,
        anchor: &DeviceId,
        displays: &[Display],
        side: Side,
    ) -> Option<Point> {
        let anchor = self.device(anchor)?;
        let theirs = bounding(displays.iter().map(|d| d.bounds))?;
        let ours = bounding(anchor.canvas_bounds())?;
        let all = bounding(self.devices.iter().flat_map(|d| d.canvas_bounds()))?;
        let x_beside = |edge: Rect| match side {
            Side::Right => edge.right() - theirs.left(),
            Side::Left => edge.left() - theirs.right(),
        };
        let their_primary = displays.iter().find(|d| d.primary).unwrap_or(&displays[0]);
        let our_primary = anchor.primary_display()?.bounds.translate(anchor.offset);
        let y = our_primary.top() - their_primary.bounds.top();
        let beside = Point::new(x_beside(ours), y);
        let free = displays.iter().all(|d| {
            let r = d.bounds.translate(beside);
            self.devices
                .iter()
                .all(|other| other.canvas_bounds().all(|o| !overlaps(r, o)))
        });
        Some(if free {
            beside
        } else {
            Point::new(x_beside(all), y)
        })
    }
}

/// The edge through which a path from `from`, inside `source`, by `delta`
/// leaves `source` first, and how far along the path (0 to 1) that is.
fn exit(source: Rect, from: Point, delta: Point) -> Option<(f64, Edge)> {
    let target = from + delta;
    let mut exit: Option<(f64, Edge)> = None;
    let mut consider = |t: f64, edge: Edge| {
        if t >= 0.0 && exit.is_none_or(|(best, _)| t < best) {
            exit = Some((t, edge));
        }
    };
    if delta.x > 0.0 && target.x >= source.right() {
        consider((source.right() - from.x) / delta.x, Edge::Right);
    }
    if delta.x < 0.0 && target.x < source.left() {
        consider((source.left() - from.x) / delta.x, Edge::Left);
    }
    if delta.y > 0.0 && target.y >= source.bottom() {
        consider((source.bottom() - from.y) / delta.y, Edge::Bottom);
    }
    if delta.y < 0.0 && target.y < source.top() {
        consider((source.top() - from.y) / delta.y, Edge::Top);
    }
    exit
}

/// How far `v` has to move to lie in the half-open range `low..high`; the
/// exclusive end is approached to within `INSET`.
fn into_range(v: f64, low: f64, high: f64) -> f64 {
    if v < low {
        low - v
    } else if v < high {
        0.0
    } else {
        high - INSET - v
    }
}

/// What a pointer crossing `edge` of `source` at `point` reaches: of
/// `candidates` (with what they stand for), those lying wholly beyond the
/// edge, the one whose facing edge is nearest to `point`, the first of
/// equals; with the way from `point` to it. `cross` and `exits` both decide
/// by it, so where the cursor leaves is where it crosses.
fn nearest_beyond<T>(
    edge: Edge,
    source: Rect,
    point: Point,
    candidates: impl IntoIterator<Item = (T, Rect)>,
) -> Option<(T, Rect, Point)> {
    candidates
        .into_iter()
        .filter(|(_, r)| edge.is_beyond(source, *r))
        .map(|(t, r)| (t, r, edge.offset_to(source, r, point)))
        .min_by(|a, b| length_squared(a.2).total_cmp(&length_squared(b.2)))
}

fn length_squared(p: Point) -> f64 {
    p.x * p.x + p.y * p.y
}

/// The smallest rectangle containing all of `rects`.
fn bounding(rects: impl Iterator<Item = Rect>) -> Option<Rect> {
    rects.reduce(|a, b| {
        let (left, top) = (a.left().min(b.left()), a.top().min(b.top()));
        let (right, bottom) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
        Rect::new(left, top, right - left, bottom - top)
    })
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.left() < b.right() - EPSILON
        && b.left() < a.right() - EPSILON
        && a.top() < b.bottom() - EPSILON
        && b.top() < a.bottom() - EPSILON
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{desk, id, staggered};

    /// Where the step goes; where it crosses is tested on its own
    /// (`crossing`).
    fn step(layout: &Layout, on: &str, from: (f64, f64), delta: (f64, f64)) -> Step {
        let s = crossing_step(layout, on, from, delta);
        Step {
            crossing: None,
            ..s
        }
    }

    fn crossing_step(layout: &Layout, on: &str, from: (f64, f64), delta: (f64, f64)) -> Step {
        layout
            .step(
                &id(on),
                Point::new(from.0, from.1),
                Point::new(delta.0, delta.1),
                &EdgePolicy::default(),
                |_| true,
            )
            .unwrap()
    }

    #[test]
    fn rejects_invalid_layouts() {
        let mut devices = desk().devices().to_vec();
        devices[1].offset = Point::new(1000.0, 0.0);
        assert_eq!(
            Layout::new(devices).unwrap_err(),
            LayoutError::Overlap(id("mac"), id("win"))
        );

        let mut devices = desk().devices().to_vec();
        devices[2].id = id("mac");
        assert_eq!(
            Layout::new(devices).unwrap_err(),
            LayoutError::DuplicateDevice(id("mac"))
        );

        let mut devices = desk().devices().to_vec();
        devices[0].displays.clear();
        assert_eq!(
            Layout::new(devices).unwrap_err(),
            LayoutError::NoDisplays(id("mac"))
        );
    }

    #[test]
    fn devices_are_numbered_left_to_right_then_top_to_bottom() {
        // The Mac mini and the PC both start at x 1512; the mini is higher.
        assert_eq!(desk().jump_order(), vec![id("mac"), id("mini"), id("win")]);
        assert_eq!(
            staggered().jump_order(),
            vec![id("mac"), id("upper"), id("lower")]
        );
        // Ten in a row: the tenth has no number.
        let row = (0..10)
            .map(|i| Device {
                id: id(&format!("d{}", 9 - i)),
                name: String::new(),
                displays: vec![Display {
                    id: "main".to_owned(),
                    bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
                    scale: 1.0,
                    primary: true,
                }],
                offset: Point::new(f64::from(i) * 100.0, 0.0),
            })
            .collect();
        let order = Layout::new(row).unwrap().jump_order();
        assert_eq!(order.len(), 9);
        assert_eq!(order.first(), Some(&id("d9")));
        assert_eq!(order.last(), Some(&id("d1")));
    }

    #[test]
    fn finds_seams_between_devices_only() {
        let seams = desk().seams();
        assert_eq!(
            seams,
            vec![
                Seam {
                    first: id("mac"),
                    second: id("win"),
                    orientation: Orientation::Vertical,
                    at: 1512.0,
                    start: 400.0,
                    end: 1382.0,
                },
                Seam {
                    first: id("mini"),
                    second: id("win"),
                    orientation: Orientation::Horizontal,
                    at: 0.0,
                    start: 1512.0,
                    end: 3432.0,
                },
            ]
        );
    }

    #[test]
    fn moves_within_a_device() {
        let s = step(&desk(), "mac", (700.0, 900.0), (10.0, -5.0));
        assert_eq!(s.device, id("mac"));
        assert_eq!(s.position, Point::new(710.0, 895.0));
    }

    #[test]
    fn moves_between_displays_of_the_same_device() {
        let s = step(&desk(), "win", (4000.0, 500.0), (100.0, 0.0));
        assert_eq!(s.device, id("win"));
        assert_eq!(s.position, Point::new(4100.0, 500.0));
    }

    #[test]
    fn crosses_a_vertical_seam() {
        let s = step(&desk(), "mac", (1511.0, 900.0), (5.0, 0.0));
        assert_eq!(s.device, id("win"));
        assert_eq!(s.position, Point::new(1516.0, 900.0));
    }

    #[test]
    fn crosses_a_horizontal_seam() {
        let s = step(&desk(), "win", (2000.0, 1.0), (0.0, -5.0));
        assert_eq!(s.device, id("mini"));
        assert_eq!(s.position, Point::new(2000.0, -4.0));
    }

    #[test]
    fn stops_at_an_edge_without_a_neighbour() {
        let s = step(&desk(), "mac", (1.0, 900.0), (-10.0, 0.0));
        assert_eq!(s.device, id("mac"));
        assert_eq!(s.position, Point::new(0.0, 900.0));
    }

    #[test]
    fn corner_margin_blocks_crossing() {
        let s = step(&desk(), "mac", (1511.0, 405.0), (5.0, 0.0));
        assert_eq!(s.device, id("mac"));
        assert!(s.position.x < 1512.0);
    }

    #[test]
    fn the_edge_reached_first_decides() {
        // Moving up and right from near the Mac's top right corner, the
        // pointer leaves through the Mac's top edge at x 1408. The Mac mini
        // lies beyond that edge, its nearest point there is (1512, 0), and
        // the rest of the movement carries on from it.
        let s = step(&desk(), "mac", (1400.0, 420.0), (200.0, -500.0));
        assert_eq!(s.device, id("mini"));
        assert_near(s.position, (1704.0, -480.0));
    }

    fn assert_near(p: Point, expected: (f64, f64)) {
        assert!(
            (p.x - expected.0).abs() < 1e-9 && (p.y - expected.1).abs() < 1e-9,
            "{p:?} is not {expected:?}"
        );
    }

    /// A Mac and a PC with one display each, at these canvas bounds.
    fn mac_and_pc(mac: Rect, pc: Rect) -> Layout {
        let device = |name: &str, r: Rect| Device {
            id: id(name),
            name: name.to_owned(),
            displays: vec![Display {
                id: "main".to_owned(),
                bounds: Rect::new(0.0, 0.0, r.width, r.height),
                scale: 1.0,
                primary: true,
            }],
            offset: Point::new(r.x, r.y),
        };
        Layout::new(vec![device("mac", mac), device("pc", pc)]).unwrap()
    }

    #[test]
    fn a_neighbour_of_the_same_height_is_entered_where_the_cursor_leaves() {
        let layout = mac_and_pc(
            Rect::new(0.0, 0.0, 3840.0, 1080.0),
            Rect::new(3840.0, 0.0, 1920.0, 1080.0),
        );
        assert_eq!(
            step(&layout, "mac", (3839.0, 500.0), (5.0, 0.0)),
            Step {
                device: id("pc"),
                position: Point::new(3844.0, 500.0),
                crossing: None,
            }
        );
        assert_eq!(
            step(&layout, "pc", (3841.0, 500.0), (-5.0, 0.0)),
            Step {
                device: id("mac"),
                position: Point::new(3836.0, 500.0),
                crossing: None,
            }
        );
    }

    #[test]
    fn a_shorter_neighbour_is_entered_at_its_nearest_point() {
        // The PC's top is level with the Mac's, but it ends at y 600.
        let layout = mac_and_pc(
            Rect::new(0.0, 0.0, 3840.0, 1080.0),
            Rect::new(3840.0, 0.0, 1920.0, 600.0),
        );
        assert_eq!(
            step(&layout, "mac", (3839.0, 300.0), (5.0, 0.0)),
            Step {
                device: id("pc"),
                position: Point::new(3844.0, 300.0),
                crossing: None,
            }
        );
        // Below it, the Mac's edge leads to the PC's bottom corner.
        let s = step(&layout, "mac", (3839.0, 900.0), (5.0, 0.0));
        assert_eq!(s.device, id("pc"));
        assert_near(s.position, (3844.0, 600.0 - INSET));
        // What is left of the movement after the edge carries on from there.
        let s = step(&layout, "mac", (3839.0, 900.0), (5.0, -3.0));
        assert_eq!(s.device, id("pc"));
        assert_near(s.position, (3844.0, 600.0 - INSET - 2.4));
        // The corner margin still holds at the Mac's own corners.
        for y in [10.0, 1075.0] {
            let s = step(&layout, "mac", (3839.0, y), (5.0, 0.0));
            assert_eq!(s.device, id("mac"));
            assert!(s.position.x < 3840.0);
        }
    }

    #[test]
    fn a_gap_between_neighbours_is_skipped() {
        let layout = mac_and_pc(
            Rect::new(0.0, 0.0, 1512.0, 982.0),
            Rect::new(1600.0, 0.0, 1920.0, 1080.0),
        );
        assert_eq!(
            step(&layout, "mac", (1510.0, 500.0), (5.0, 0.0)),
            Step {
                device: id("pc"),
                position: Point::new(1603.0, 500.0),
                crossing: None,
            }
        );
        assert_eq!(
            step(&layout, "pc", (1601.0, 500.0), (-5.0, 0.0)),
            Step {
                device: id("mac"),
                position: Point::new(1508.0, 500.0),
                crossing: None,
            }
        );

        // Up and to the right, across the gap and along to the PC's corner.
        let layout = mac_and_pc(
            Rect::new(0.0, 0.0, 1512.0, 982.0),
            Rect::new(2000.0, -1200.0, 1920.0, 1080.0),
        );
        assert_eq!(
            step(&layout, "mac", (1000.0, 1.0), (0.0, -5.0)),
            Step {
                device: id("pc"),
                position: Point::new(2000.0, -124.0),
                crossing: None,
            }
        );
    }

    #[test]
    fn the_nearest_of_several_neighbours_on_a_side_wins() {
        // No edges touch, so no seams are drawn, yet each edge facing
        // another computer leads there.
        let layout = staggered();
        assert!(layout.seams().is_empty());

        // High up the Mac's right edge, the upper computer is 100 away.
        let s = step(&layout, "mac", (1511.0, 100.0), (5.0, 0.0));
        assert_eq!(s.device, id("upper"));
        assert_near(s.position, (1516.0, -INSET));
        // Lower down, the lower one, across its gap, is nearer.
        for (y, entered) in [(500.0, 600.0), (900.0, 900.0)] {
            assert_eq!(
                step(&layout, "mac", (1511.0, y), (5.0, 0.0)),
                Step {
                    device: id("lower"),
                    position: Point::new(1704.0, entered),
                    crossing: None,
                }
            );
        }
        // A device that cannot be entered is no candidate: the next nearest
        // one is.
        let s = layout
            .step(
                &id("mac"),
                Point::new(1511.0, 100.0),
                Point::new(5.0, 0.0),
                &EdgePolicy::default(),
                |d| *d != id("upper"),
            )
            .unwrap();
        assert_eq!(
            (s.device, s.position),
            (id("lower"), Point::new(1704.0, 600.0))
        );
    }

    fn spot(device: &str, display: &str, edge: Edge, along: f64) -> EdgeSpot {
        EdgeSpot {
            device: id(device),
            display: display.to_owned(),
            edge,
            along,
        }
    }

    fn crossed(layout: &Layout, on: &str, from: (f64, f64), delta: (f64, f64)) -> Crossing {
        crossing_step(layout, on, from, delta)
            .crossing
            .expect("the step crosses")
    }

    #[test]
    fn a_crossing_says_where_the_cursor_left_and_entered() {
        // From the Mac to the PC beside it, in each one's own coordinates:
        // the Mac stands 400 lower on the canvas.
        assert_eq!(
            crossed(&desk(), "mac", (1511.0, 900.0), (5.0, 0.0)),
            Crossing {
                left: spot("mac", "builtin", Edge::Right, 500.0),
                entered: spot("win", "main", Edge::Left, 900.0),
            }
        );
        // And back.
        assert_eq!(
            crossed(&desk(), "win", (1513.0, 900.0), (-5.0, 0.0)),
            Crossing {
                left: spot("win", "main", Edge::Left, 900.0),
                entered: spot("mac", "builtin", Edge::Right, 500.0),
            }
        );
        // Up from the PC's main display, past the mini's right end: the
        // mini's bottom edge is entered at its nearest point.
        let c = crossed(&desk(), "win", (3800.0, 1.0), (0.0, -5.0));
        assert_eq!(c.left, spot("win", "main", Edge::Top, 2288.0));
        assert_eq!(
            (c.entered.device, c.entered.display.as_str(), c.entered.edge),
            (id("mini"), "ext", Edge::Bottom)
        );
        assert!((c.entered.along - (1920.0 - INSET)).abs() < 1e-9);
        // Across a gap to the PC's side display, entered at its top.
        assert_eq!(
            crossed(&desk(), "mini", (3431.0, -900.0), (5.0, 0.0)),
            Crossing {
                left: spot("mini", "ext", Edge::Right, 180.0),
                entered: spot("win", "side", Edge::Left, -300.0),
            }
        );
        // Across a gap to the nearest computer, at the nearest point of its
        // facing edge: the exit edge is still the Mac's own.
        assert_eq!(
            crossed(&staggered(), "mac", (1511.0, 500.0), (5.0, 0.0)),
            Crossing {
                left: spot("mac", "main", Edge::Right, 500.0),
                entered: spot("lower", "main", Edge::Left, 0.0),
            }
        );
    }

    #[test]
    fn the_crossing_names_the_edge_crossed_even_where_the_cursor_ends_up_further() {
        // A tall Mac beside a PC with two displays one above the other.
        let display = |id: &str, y: f64| Display {
            id: id.to_owned(),
            bounds: Rect::new(0.0, y, 1920.0, 1080.0),
            scale: 1.0,
            primary: y == 0.0,
        };
        let layout = Layout::new(vec![
            Device {
                id: id("mac"),
                name: "mac".to_owned(),
                displays: vec![Display {
                    id: "builtin".to_owned(),
                    bounds: Rect::new(0.0, 0.0, 1512.0, 2160.0),
                    scale: 1.0,
                    primary: true,
                }],
                offset: Point::new(0.0, 0.0),
            },
            Device {
                id: id("pc"),
                name: "pc".to_owned(),
                displays: vec![display("upper", 0.0), display("lower", 1080.0)],
                offset: Point::new(1512.0, 0.0),
            },
        ])
        .unwrap();
        // Crossing just above the seam between the PC's displays, the rest
        // of the movement carries the cursor down into the lower one; the
        // edge crossed is still the upper one's, where it crossed.
        let s = crossing_step(&layout, "mac", (1511.0, 1076.0), (5.0, 10.0));
        assert_eq!(s.device, id("pc"));
        assert_eq!(s.position, Point::new(1516.0, 1086.0));
        let c = s.crossing.expect("the step crosses");
        assert_eq!(c.left, spot("mac", "builtin", Edge::Right, 1078.0));
        assert_eq!(c.entered, spot("pc", "upper", Edge::Left, 1078.0));
    }

    #[test]
    fn a_step_that_stays_crosses_nothing() {
        // Within the device, against a wall, and in a corner.
        for (from, delta) in [
            ((700.0, 900.0), (5.0, 0.0)),
            ((700.0, 1381.0), (0.0, 10.0)),
            ((1511.0, 405.0), (5.0, 0.0)),
        ] {
            let s = crossing_step(&desk(), "mac", from, delta);
            assert_eq!((s.device, s.crossing), (id("mac"), None));
        }
    }

    #[test]
    fn without_a_candidate_the_cursor_stays() {
        // Nothing lies below the Mac, or above the PC's side display.
        let s = step(&desk(), "mac", (700.0, 1381.0), (0.0, 10.0));
        assert_eq!(s.device, id("mac"));
        assert!(s.position.y < 1382.0);
        assert_eq!(
            step(&desk(), "win", (4500.0, -299.0), (0.0, -5.0)),
            Step {
                device: id("win"),
                position: Point::new(4500.0, -300.0),
                crossing: None,
            }
        );
        // Nor when no device may be entered.
        let s = desk()
            .step(
                &id("mac"),
                Point::new(1511.0, 900.0),
                Point::new(5.0, 0.0),
                &EdgePolicy::default(),
                |_| false,
            )
            .unwrap();
        assert_eq!(s.device, id("mac"));
        assert!(s.position.x < 1512.0);
    }

    #[test]
    fn candidates_are_displays_and_the_device_owning_one_is_entered() {
        // Right of the Mac mini, across a gap, lies only the PC's side
        // display: the cursor enters at its top left corner.
        assert_eq!(
            step(&desk(), "mini", (3431.0, -900.0), (5.0, 0.0)),
            Step {
                device: id("win"),
                position: Point::new(4076.0, -300.0),
                crossing: None,
            }
        );
        // Along the PC's top edge past the mini, the mini is still the
        // nearest display above.
        let s = step(&desk(), "win", (3800.0, 1.0), (0.0, -5.0));
        assert_eq!(s.device, id("mini"));
        assert_near(s.position, (3432.0 - INSET, -4.0));
        // Left of the PC's side display, below its main one, the main one is
        // nearer than the mini: the cursor stays.
        let s = step(&desk(), "win", (4073.0, 1500.0), (-5.0, 0.0));
        assert_eq!(s.device, id("win"));
        assert!(s.position.x >= 4072.0);
    }

    #[test]
    fn a_nearer_display_of_this_device_keeps_the_cursor_here() {
        // A laptop with an external display to its right, whose right edge
        // reaches below the external display's bottom, and a PC to the
        // external display's right.
        let laptop = Device {
            id: id("mac"),
            name: "mac".to_owned(),
            displays: vec![
                Display {
                    id: "external".to_owned(),
                    bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0),
                    scale: 1.0,
                    primary: true,
                },
                Display {
                    id: "builtin".to_owned(),
                    bounds: Rect::new(-1512.0, 300.0, 1512.0, 982.0),
                    scale: 2.0,
                    primary: false,
                },
            ],
            offset: Point::new(0.0, 0.0),
        };
        let layout = with(
            &Layout::new(vec![laptop]).unwrap(),
            "pc",
            vec![Display {
                id: "main".to_owned(),
                bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0),
                scale: 1.0,
                primary: true,
            }],
            Point::new(1920.0, 0.0),
        );

        // Below the external display, the laptop's right edge is a wall, as
        // the Mac has it: the PC lies beyond the external display.
        let s = step(&layout, "mac", (-1.0, 1200.0), (5.0, 0.0));
        assert_eq!(s.device, id("mac"));
        assert!(s.position.x < 0.0);
        // Within its range, the cursor moves onto the external display.
        assert_eq!(
            step(&layout, "mac", (-1.0, 500.0), (5.0, 0.0)),
            Step {
                device: id("mac"),
                position: Point::new(4.0, 500.0),
                crossing: None,
            }
        );
        // And from there on to the PC.
        assert_eq!(
            step(&layout, "mac", (1919.0, 500.0), (5.0, 0.0)),
            Step {
                device: id("pc"),
                position: Point::new(1924.0, 500.0),
                crossing: None,
            }
        );
    }

    #[test]
    fn respects_can_enter() {
        let layout = desk();
        let push = |can_enter: &dyn Fn(&DeviceId) -> bool| {
            layout
                .step(
                    &id("mac"),
                    Point::new(1511.0, 900.0),
                    Point::new(5.0, 0.0),
                    &EdgePolicy::default(),
                    can_enter,
                )
                .unwrap()
                .device
        };
        // Without the PC, the Mac mini is the nearest device to the right.
        assert_eq!(push(&|d| *d != id("win")), id("mini"));
        assert_eq!(push(&|d| *d != id("win") && *d != id("mini")), id("mac"));
    }

    fn laptop() -> Vec<Display> {
        vec![Display {
            id: "builtin".to_owned(),
            bounds: Rect::new(0.0, 0.0, 1440.0, 900.0),
            scale: 2.0,
            primary: true,
        }]
    }

    fn with(layout: &Layout, id_: &str, displays: Vec<Display>, offset: Point) -> Layout {
        let mut devices = layout.devices().to_vec();
        devices.push(Device {
            id: id(id_),
            name: id_.to_owned(),
            displays,
            offset,
        });
        Layout::new(devices).unwrap()
    }

    #[test]
    fn places_a_newcomer_beside_the_anchor_with_primary_tops_aligned() {
        let only_mac = Layout::new(vec![desk().device(&id("mac")).unwrap().clone()]).unwrap();
        let right = only_mac
            .place_beside(&id("mac"), &laptop(), Side::Right)
            .unwrap();
        assert_eq!(right, Point::new(1512.0, 400.0));
        let left = only_mac
            .place_beside(&id("mac"), &laptop(), Side::Left)
            .unwrap();
        assert_eq!(left, Point::new(-1440.0, 400.0));

        // The newcomer is reachable across the seam it now shares.
        let layout = with(&only_mac, "air", laptop(), right);
        let s = step(&layout, "mac", (1511.0, 900.0), (5.0, 0.0));
        assert_eq!(s.device, id("air"));
    }

    #[test]
    fn a_taken_spot_sends_the_newcomer_past_everything() {
        // The PC already sits right of the Mac, and its side display
        // reaches out to x 5152.
        let offset = desk()
            .place_beside(&id("mac"), &laptop(), Side::Right)
            .unwrap();
        assert_eq!(offset, Point::new(5152.0, 400.0));
        with(&desk(), "air", laptop(), offset);
    }

    #[test]
    fn displays_left_of_the_primary_are_accounted_for() {
        // A device whose second display sits left of its primary one.
        let displays = vec![
            Display {
                id: "main".to_owned(),
                bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0),
                scale: 1.0,
                primary: true,
            },
            Display {
                id: "side".to_owned(),
                bounds: Rect::new(-1280.0, -200.0, 1280.0, 1024.0),
                scale: 1.0,
                primary: false,
            },
        ];
        let only_mac = Layout::new(vec![desk().device(&id("mac")).unwrap().clone()]).unwrap();
        let offset = only_mac
            .place_beside(&id("mac"), &displays, Side::Right)
            .unwrap();
        assert_eq!(offset, Point::new(1512.0 + 1280.0, 400.0));
        let layout = with(&only_mac, "pc", displays, offset);
        // The side display, now next to the Mac, is reachable from it.
        let s = step(&layout, "mac", (1511.0, 900.0), (5.0, 0.0));
        assert_eq!(s.device, id("pc"));
    }

    #[test]
    fn a_taller_side_display_does_not_shift_the_primary() {
        // The PC's portrait display reaches above its main one; the main
        // displays still line up.
        let win = desk().device(&id("win")).unwrap().displays.clone();
        let only_mac = Layout::new(vec![desk().device(&id("mac")).unwrap().clone()]).unwrap();
        let offset = only_mac
            .place_beside(&id("mac"), &win, Side::Right)
            .unwrap();
        assert_eq!(offset, Point::new(1512.0, 400.0));
    }

    #[test]
    fn placing_needs_an_anchor_and_displays() {
        assert!(
            desk()
                .place_beside(&id("nope"), &laptop(), Side::Right)
                .is_none()
        );
        assert!(desk().place_beside(&id("mac"), &[], Side::Right).is_none());
    }

    /// A device of displays given as (id, x, y, width, height) in its own
    /// desktop coordinates, at `offset`.
    fn device_of(
        name: &str,
        offset: (f64, f64),
        displays: &[(&str, f64, f64, f64, f64)],
    ) -> Device {
        Device {
            id: id(name),
            name: name.to_owned(),
            displays: displays
                .iter()
                .map(|&(display, x, y, w, h)| Display {
                    id: display.to_owned(),
                    bounds: Rect::new(x, y, w, h),
                    scale: 1.0,
                    primary: false,
                })
                .collect(),
            offset: Point::new(offset.0, offset.1),
        }
    }

    /// An exit as `exits_of` gives it: without its display's bounds, which
    /// `exits_carry_their_displays_bounds` looks at.
    fn exit(display: &str, edge: Edge, at: f64, start: f64, end: f64) -> Exit {
        Exit {
            display: display.to_owned(),
            bounds: Rect::default(),
            edge,
            at,
            start,
            end,
        }
    }

    #[test]
    fn exits_carry_their_displays_bounds() {
        let layout = desk();
        let exits = layout.exits(&id("win"), &EdgePolicy::default(), |_| true);
        assert!(!exits.is_empty());
        let win = layout.device(&id("win")).unwrap();
        for e in &exits {
            let display = win.displays.iter().find(|d| d.id == e.display).unwrap();
            assert_eq!(e.bounds, display.bounds, "{e:?}");
        }
    }

    /// How long `exits` takes for a large desk: three 4K displays side by
    /// side, with a device on every side and one more above. Run with
    /// `cargo test --release -p iw-core exits_take -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement"]
    fn exits_take_this_long_for_a_large_desk() {
        let display = |id: &str, x: f64| Display {
            id: id.to_owned(),
            bounds: Rect::new(x, 0.0, 3840.0, 2160.0),
            scale: 1.0,
            primary: x == 0.0,
        };
        let peer = |name: &str, x: f64, y: f64| Device {
            id: id(name),
            name: name.to_owned(),
            displays: vec![Display {
                id: "main".to_owned(),
                bounds: Rect::new(0.0, 0.0, 2560.0, 1440.0),
                scale: 1.0,
                primary: true,
            }],
            offset: Point::new(x, y),
        };
        let layout = Layout::new(vec![
            Device {
                id: id("big"),
                name: "big".to_owned(),
                displays: vec![
                    display("a", 0.0),
                    display("b", 3840.0),
                    display("c", 7680.0),
                ],
                offset: Point::new(0.0, 0.0),
            },
            peer("left", -2560.0, 300.0),
            peer("right", 11520.0, 300.0),
            peer("above", 4000.0, -1440.0),
            peer("below", 6000.0, 2160.0),
        ])
        .unwrap();
        let runs = 50;
        let start = std::time::Instant::now();
        let mut found = 0;
        for _ in 0..runs {
            found = exits_of(&layout, "big", |_| true).len();
        }
        let each = start.elapsed() / runs;
        println!("exits for three 4K displays and 4 peers: {each:?} each, {found} exits");
        assert!(found >= 4);
    }

    /// Every unit along every edge of every display, on several desks:
    /// a movement straight out from just inside crosses to another device
    /// exactly where `exits` says it leaves.
    #[test]
    fn exits_are_where_step_crosses() {
        let gapped = Layout::new(vec![
            device_of(
                "a",
                (0.0, 0.0),
                &[
                    ("one", 0.0, 0.0, 1600.0, 900.0),
                    ("two", 1600.0, 300.0, 800.0, 600.0),
                ],
            ),
            device_of("b", (2500.0, 0.0), &[("main", 0.0, 0.0, 1280.0, 800.0)]),
            device_of("c", (100.0, -700.0), &[("main", 0.0, 0.0, 1200.0, 700.0)]),
            device_of("d", (1700.0, 950.0), &[("main", 0.0, 0.0, 900.0, 500.0)]),
        ])
        .unwrap();
        let policy = EdgePolicy::default();
        for layout in [desk(), staggered(), gapped] {
            for device in layout.devices() {
                let exits = exits_of(&layout, device.id.as_str(), |_| true);
                for display in &device.displays {
                    let source = display.bounds.translate(device.offset);
                    for edge in Edge::ALL {
                        let (_, low, high) = edge.extent(source);
                        let (mut along, out) = (low + 0.5, edge.outward());
                        while along < high {
                            let on = edge.point(source, along);
                            let from = on - Point::new(out.x * 0.5, out.y * 0.5);
                            let crosses = layout
                                .step(&device.id, from, out, &policy, |_| true)
                                .is_some_and(|s| s.device != device.id);
                            let local = along
                                - match edge {
                                    Edge::Left | Edge::Right => device.offset.y,
                                    Edge::Top | Edge::Bottom => device.offset.x,
                                };
                            let exit = exits.iter().any(|e| {
                                e.display == display.id
                                    && e.edge == edge
                                    && e.start <= local
                                    && local < e.end
                            });
                            assert_eq!(
                                crosses, exit,
                                "{} {} {edge:?} at {along}",
                                device.id, display.id
                            );
                            along += 1.0;
                        }
                    }
                }
            }
        }
    }

    fn exits_of(layout: &Layout, on: &str, can_enter: impl Fn(&DeviceId) -> bool) -> Vec<Exit> {
        layout
            .exits(&id(on), &EdgePolicy::default(), can_enter)
            .into_iter()
            .map(|e| Exit {
                bounds: Rect::default(),
                ..e
            })
            .collect()
    }

    // The cases below are those of the Windows app's drag strips
    // (`DragTests.cs`), less the corner margin, which the strips keep.

    #[test]
    fn the_edge_facing_another_device_is_an_exit() {
        // As on the build machine: the PC left of the Mac, edge to edge.
        let layout = Layout::new(vec![
            device_of(
                "pc",
                (-1920.0, 0.0),
                &[("DISPLAY1", 0.0, 0.0, 1920.0, 1080.0)],
            ),
            device_of("mac", (0.0, 0.0), &[("1", 0.0, 0.0, 1920.0, 1080.0)]),
        ])
        .unwrap();
        // In the PC's own coordinates, short of the corners.
        assert_eq!(
            exits_of(&layout, "pc", |_| true),
            [exit("DISPLAY1", Edge::Right, 1920.0, 20.0, 1060.0)]
        );
        // Not while the Mac cannot be entered.
        assert_eq!(exits_of(&layout, "pc", |_| false), []);
        assert_eq!(exits_of(&layout, "nobody", |_| true), []);
    }

    #[test]
    fn a_gap_is_crossed_and_every_edge_facing_the_device_counts() {
        // The Mac above and to the right, apart from the PC: the cursor
        // still goes there through the right edge, and through the top.
        let layout = Layout::new(vec![
            device_of("pc", (0.0, 0.0), &[("DISPLAY1", 0.0, 0.0, 1920.0, 1080.0)]),
            device_of("mac", (2000.0, -500.0), &[("1", 0.0, 0.0, 1000.0, 400.0)]),
        ])
        .unwrap();
        assert_eq!(
            exits_of(&layout, "pc", |_| true),
            [
                exit("DISPLAY1", Edge::Right, 1920.0, 20.0, 1060.0),
                exit("DISPLAY1", Edge::Top, 0.0, 20.0, 1900.0),
            ]
        );
    }

    #[test]
    fn where_a_display_of_this_device_is_nearer_there_is_no_exit() {
        // Two displays side by side, the second lower; the Mac above the
        // first's right edge, beyond it. Along that edge the second display
        // is nearer below y = 270, so only the stretch above crosses.
        let layout = Layout::new(vec![
            device_of(
                "pc",
                (0.0, 0.0),
                &[
                    ("DISPLAY1", 0.0, 0.0, 1920.0, 1080.0),
                    ("DISPLAY2", 1920.0, 540.0, 1920.0, 1080.0),
                ],
            ),
            device_of("mac", (1920.0, -500.0), &[("1", 0.0, 0.0, 1000.0, 500.0)]),
        ])
        .unwrap();
        let exits = exits_of(&layout, "pc", |_| true);
        assert_eq!(
            exits,
            [
                exit("DISPLAY1", Edge::Right, 1920.0, 20.0, 270.0),
                exit("DISPLAY1", Edge::Top, 0.0, 20.0, 1900.0),
                // Facing the Mac across a gap.
                exit("DISPLAY2", Edge::Top, 540.0, 1940.0, 3820.0),
            ]
        );
        // As `step` has it, on either side of y = 270.
        let policy = EdgePolicy::default();
        let out = |y: f64| {
            layout
                .step(
                    &id("pc"),
                    Point::new(1919.5, y),
                    Point::new(1.0, 0.0),
                    &policy,
                    |_| true,
                )
                .unwrap()
                .device
        };
        assert_eq!(out(269.0), id("mac"));
        assert_eq!(out(271.0), id("pc"));
    }

    #[test]
    fn exits_are_in_the_devices_own_coordinates() {
        // The same as the first case, the PC's desktop origin elsewhere.
        let layout = Layout::new(vec![
            device_of(
                "pc",
                (-1920.0, 300.0),
                &[("DISPLAY1", 100.0, -300.0, 1920.0, 1080.0)],
            ),
            device_of("mac", (100.0, 0.0), &[("1", 0.0, 0.0, 1920.0, 1080.0)]),
        ])
        .unwrap();
        assert_eq!(
            exits_of(&layout, "pc", |_| true),
            [exit("DISPLAY1", Edge::Right, 2020.0, -280.0, 760.0)]
        );
    }

    #[test]
    fn an_edge_within_the_corner_margins_has_no_exit() {
        let layout = Layout::new(vec![
            device_of("pc", (0.0, 0.0), &[("small", 0.0, 0.0, 300.0, 30.0)]),
            device_of("mac", (300.0, 0.0), &[("1", 0.0, 0.0, 1920.0, 1080.0)]),
        ])
        .unwrap();
        assert_eq!(exits_of(&layout, "pc", |_| true), []);
        let tight = EdgePolicy {
            corner_margin: 0.0,
            ..EdgePolicy::default()
        };
        assert_eq!(
            layout.exits(&id("pc"), &tight, |_| true),
            [Exit {
                bounds: Rect::new(0.0, 0.0, 300.0, 30.0),
                ..exit("small", Edge::Right, 300.0, 0.0, 30.0)
            }]
        );
    }

    #[test]
    fn unknown_device_has_no_step() {
        let layout = desk();
        assert!(
            layout
                .step(
                    &id("nope"),
                    Point::default(),
                    Point::default(),
                    &EdgePolicy::default(),
                    |_| true
                )
                .is_none()
        );
    }
}
