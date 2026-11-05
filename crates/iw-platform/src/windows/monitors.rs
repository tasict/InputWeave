//! Where each monitor sits in logical desktop coordinates, and the mapping
//! between those and the physical pixels Windows uses. Free of FFI so it
//! can be tested anywhere.
//!
//! A per-monitor DPI aware process sees the virtual screen in physical
//! pixels: each monitor is a pixel rectangle placed as in Display settings,
//! with its own scale (effective DPI ÷ 96). The layout works in logical
//! units, so each monitor gets a logical rectangle of its pixel size ÷ its
//! scale. With mixed scales no single linear map keeps every monitor
//! touching its neighbours, so the logical origins are chosen one monitor
//! at a time:
//!
//! - The primary monitor's top left pixel is (0, 0); its logical rectangle
//!   starts at (0, 0) too.
//! - Each further monitor is placed against a monitor already placed that
//!   it shares an edge with in pixels, on the same side, so the two still
//!   share an edge in logical units. Along that edge, one point of the
//!   shared segment maps to the same logical coordinate from both monitors:
//!   its end if the monitors are aligned at the end and not at the start
//!   (bottom- or right-aligned), otherwise its start. Top- and
//!   left-aligned, centred and bottom-aligned arrangements thus keep a
//!   shared segment, though with different scales its length differs as
//!   seen from each side.
//! - Monitors are taken primary first, then top to bottom and left to
//!   right, always preferring one that touches a placed monitor, so the
//!   result does not depend on the order Windows enumerates them in. A
//!   monitor that touches nothing placed (only a corner, say) keeps its
//!   pixel offset from the nearest placed monitor, divided by that
//!   monitor's scale.
//! - In grids of mixed scales a placement can overlap a monitor placed
//!   earlier. Then the next touching neighbour is tried, and if every one
//!   overlaps, the monitor is pushed away from a neighbour until it is
//!   clear, using the neighbour that needs the shortest push. A gap is
//!   better than an overlap: overlapping logical rectangles would make the
//!   mapping ambiguous. Such a grid can also lose a seam between two of its
//!   monitors, or gain one between monitors that only meet at a corner.
//!
//! Within a monitor the mapping is affine (logical = origin + pixel offset
//! ÷ scale), and the logical rectangles never overlap, so
//! [`DesktopMap::to_physical`] inverts [`DesktopMap::to_logical`] for every
//! pixel on a monitor. Both treat rectangles as half-open, as the layout
//! does: a pixel or point on a shared edge belongs to the monitor to its
//! right or below.

#![cfg_attr(not(windows), allow(dead_code))]

use iw_core::geometry::{EPSILON, Point, Rect};
use iw_core::layout::Display;

/// A pixel on the virtual screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pixel {
    pub x: i32,
    pub y: i32,
}

impl Pixel {
    pub(crate) const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// A rectangle of pixels; `right` and `bottom` are exclusive, as in a
/// Win32 `RECT`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PixelRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl PixelRect {
    pub(crate) const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub(crate) fn width(&self) -> i32 {
        self.right - self.left
    }

    pub(crate) fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub(crate) fn contains(&self, p: Pixel) -> bool {
        p.x >= self.left && p.x < self.right && p.y >= self.top && p.y < self.bottom
    }

    /// The pixel inside this rectangle closest to `p`.
    pub(crate) fn clamp(&self, p: Pixel) -> Pixel {
        Pixel::new(
            p.x.clamp(self.left, self.right - 1),
            p.y.clamp(self.top, self.bottom - 1),
        )
    }

    pub(crate) fn center(&self) -> Pixel {
        Pixel::new(self.left + self.width() / 2, self.top + self.height() / 2)
    }

    fn distance_squared(&self, p: Pixel) -> i64 {
        let c = self.clamp(p);
        let (dx, dy) = (i64::from(p.x - c.x), i64::from(p.y - c.y));
        dx * dx + dy * dy
    }

    fn union(&self, other: &PixelRect) -> PixelRect {
        PixelRect::new(
            self.left.min(other.left),
            self.top.min(other.top),
            self.right.max(other.right),
            self.bottom.max(other.bottom),
        )
    }
}

/// A monitor as Windows reports it to a per-monitor DPI aware process.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Monitor {
    /// The display device name, such as `\\.\DISPLAY1`.
    pub id: String,
    pub pixels: PixelRect,
    /// Effective DPI ÷ 96.
    pub scale: f64,
    pub primary: bool,
}

/// The side of a placed monitor that another monitor is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Left,
    Right,
    Above,
    Below,
}

/// The sides of the desktop a pixel lies on: those where the next pixel
/// outward belongs to no monitor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Placed {
    monitor: Monitor,
    logical: Rect,
}

impl Placed {
    fn to_logical(&self, p: Pixel) -> Point {
        let m = &self.monitor;
        Point::new(
            self.logical.x + f64::from(p.x - m.pixels.left) / m.scale,
            self.logical.y + f64::from(p.y - m.pixels.top) / m.scale,
        )
    }

    fn to_physical(&self, p: Point) -> Pixel {
        let m = &self.monitor;
        let x = f64::from(m.pixels.left) + (p.x - self.logical.x) * m.scale;
        let y = f64::from(m.pixels.top) + (p.y - self.logical.y) * m.scale;
        // Float-to-int casts saturate, and NaN becomes 0; the clamp keeps
        // the result on this monitor either way.
        m.pixels
            .clamp(Pixel::new(x.round() as i32, y.round() as i32))
    }
}

/// The monitors and the logical rectangle each one occupies.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DesktopMap {
    placed: Vec<Placed>,
}

impl DesktopMap {
    /// Places the monitors as described in the module documentation.
    /// Monitors without area or with a nonsensical scale are left out.
    pub(crate) fn new(mut monitors: Vec<Monitor>) -> Self {
        monitors.retain(|m| {
            m.pixels.width() > 0 && m.pixels.height() > 0 && m.scale.is_finite() && m.scale > 0.0
        });
        monitors.sort_by_key(|m| (!m.primary, m.pixels.top, m.pixels.left));
        let mut pending = monitors.into_iter();
        let Some(root) = pending.next() else {
            return Self::default();
        };
        let mut pending: Vec<Monitor> = pending.collect();
        let mut placed = vec![Placed {
            logical: Rect::new(
                f64::from(root.pixels.left) / root.scale,
                f64::from(root.pixels.top) / root.scale,
                f64::from(root.pixels.width()) / root.scale,
                f64::from(root.pixels.height()) / root.scale,
            ),
            monitor: root,
        }];
        while !pending.is_empty() {
            let next = pending
                .iter()
                .position(|m| {
                    placed
                        .iter()
                        .any(|p| side_of(&p.monitor.pixels, &m.pixels).is_some())
                })
                .unwrap_or(0);
            let monitor = pending.remove(next);
            let logical = place(&placed, &monitor);
            placed.push(Placed { monitor, logical });
        }
        Self { placed }
    }

    /// The monitors as the layout sees them.
    pub(crate) fn displays(&self) -> Vec<Display> {
        self.placed
            .iter()
            .map(|p| Display {
                id: p.monitor.id.clone(),
                bounds: p.logical,
                scale: p.monitor.scale,
                primary: p.monitor.primary,
            })
            .collect()
    }

    /// The smallest rectangle around every monitor: what Windows calls the
    /// virtual screen.
    pub(crate) fn virtual_screen(&self) -> Option<PixelRect> {
        self.placed
            .iter()
            .map(|p| p.monitor.pixels)
            .reduce(|a, b| a.union(&b))
    }

    /// The logical point of pixel `p`. A pixel off every monitor is first
    /// moved onto the nearest one. `None` only without monitors.
    pub(crate) fn to_logical(&self, p: Pixel) -> Option<Point> {
        let placed = self.for_pixel(p)?;
        Some(placed.to_logical(placed.monitor.pixels.clamp(p)))
    }

    /// The pixel at logical point `p`. A point off every monitor is first
    /// moved onto the nearest one. `None` only without monitors.
    pub(crate) fn to_physical(&self, p: Point) -> Option<Pixel> {
        let placed = self
            .placed
            .iter()
            .find(|placed| placed.logical.contains(p))
            .or_else(|| {
                self.placed.iter().min_by(|a, b| {
                    a.logical
                        .distance_squared(p)
                        .total_cmp(&b.logical.distance_squared(p))
                })
            })?;
        Some(placed.to_physical(p))
    }

    /// How far the pointer moved, in logical units, going from pixel `from`
    /// to pixel `to`. When `to` is on a monitor this is the difference of
    /// the two logical points, so moving onto a monitor of another scale
    /// gives the logical distance. When `to` is off every monitor, as when
    /// Windows reports motion past the edge of the desktop, the movement is
    /// measured with the scale of the monitor `from` is on.
    pub(crate) fn logical_delta(&self, from: Pixel, to: Pixel) -> Point {
        let Some(origin) = self.for_pixel(from) else {
            return Point::default();
        };
        let start = origin.to_logical(origin.monitor.pixels.clamp(from));
        let end = match self.placed.iter().find(|p| p.monitor.pixels.contains(to)) {
            Some(target) => target.to_logical(to),
            None => origin.to_logical(to),
        };
        end - start
    }

    /// The scale of the monitor pixel `p` is on, or of the nearest one; 1
    /// without monitors.
    pub(crate) fn scale_at(&self, p: Pixel) -> f64 {
        self.for_pixel(p).map_or(1.0, |placed| placed.monitor.scale)
    }

    /// The sides of the desktop pixel `p` lies on. Empty for pixels off the
    /// desktop and for pixels with a monitor on every side.
    pub(crate) fn outer_edges(&self, p: Pixel) -> Edges {
        if !self.on_desktop(p) {
            return Edges::default();
        }
        Edges {
            left: !self.on_desktop(Pixel::new(p.x - 1, p.y)),
            right: !self.on_desktop(Pixel::new(p.x + 1, p.y)),
            top: !self.on_desktop(Pixel::new(p.x, p.y - 1)),
            bottom: !self.on_desktop(Pixel::new(p.x, p.y + 1)),
        }
    }

    /// The centre pixel of the monitor `p` is on, or of the nearest one.
    pub(crate) fn monitor_center(&self, p: Pixel) -> Option<Pixel> {
        self.for_pixel(p)
            .map(|placed| placed.monitor.pixels.center())
    }

    fn on_desktop(&self, p: Pixel) -> bool {
        self.placed
            .iter()
            .any(|placed| placed.monitor.pixels.contains(p))
    }

    fn for_pixel(&self, p: Pixel) -> Option<&Placed> {
        self.placed
            .iter()
            .find(|placed| placed.monitor.pixels.contains(p))
            .or_else(|| {
                self.placed
                    .iter()
                    .min_by_key(|placed| placed.monitor.pixels.distance_squared(p))
            })
    }
}

/// Where `m` is relative to `p`, if the two share an edge segment of
/// positive length.
fn side_of(p: &PixelRect, m: &PixelRect) -> Option<Side> {
    let overlap_y = m.bottom.min(p.bottom) - m.top.max(p.top);
    let overlap_x = m.right.min(p.right) - m.left.max(p.left);
    if overlap_y > 0 && m.left == p.right {
        Some(Side::Right)
    } else if overlap_y > 0 && m.right == p.left {
        Some(Side::Left)
    } else if overlap_x > 0 && m.top == p.bottom {
        Some(Side::Below)
    } else if overlap_x > 0 && m.bottom == p.top {
        Some(Side::Above)
    } else {
        None
    }
}

/// The logical rectangle for `m`, given the monitors placed so far.
fn place(placed: &[Placed], m: &Monitor) -> Rect {
    let mut candidates: Vec<(Rect, Side)> = placed
        .iter()
        .filter_map(|p| {
            side_of(&p.monitor.pixels, &m.pixels).map(|side| (beside(p, m, side), side))
        })
        .collect();
    if candidates.is_empty() {
        candidates.push(detached(placed, m));
    }
    if let Some(&(rect, _)) = candidates.iter().find(|(rect, _)| is_free(placed, *rect)) {
        return rect;
    }
    // Every placement overlaps something: push each one clear and keep the
    // one that had to move least.
    candidates
        .into_iter()
        .map(|(rect, side)| {
            let pushed = push_clear(placed, rect, side);
            let moved = (pushed.x - rect.x).abs() + (pushed.y - rect.y).abs();
            (pushed, moved)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(rect, _)| rect)
        .expect("there is at least one candidate")
}

/// The logical rectangle for `m` on `side` of `p`.
fn beside(p: &Placed, m: &Monitor, side: Side) -> Rect {
    let width = f64::from(m.pixels.width()) / m.scale;
    let height = f64::from(m.pixels.height()) / m.scale;
    let (pp, mp) = (&p.monitor.pixels, &m.pixels);
    let (ps, ms) = (p.monitor.scale, m.scale);
    match side {
        Side::Left | Side::Right => {
            let y = along(pp.top, pp.bottom, p.logical.y, ps, mp.top, mp.bottom, ms);
            let x = if side == Side::Right {
                p.logical.right()
            } else {
                p.logical.left() - width
            };
            Rect::new(x, y, width, height)
        }
        Side::Above | Side::Below => {
            let x = along(pp.left, pp.right, p.logical.x, ps, mp.left, mp.right, ms);
            let y = if side == Side::Below {
                p.logical.bottom()
            } else {
                p.logical.top() - height
            };
            Rect::new(x, y, width, height)
        }
    }
}

/// The logical start of a monitor spanning `m_start..m_end` pixels along
/// an edge it shares with a placed monitor spanning `p_start..p_end` that
/// starts at logical `p_logical`: the anchor pixel of the shared segment
/// lands on the same logical coordinate from both sides.
fn along(
    p_start: i32,
    p_end: i32,
    p_logical: f64,
    p_scale: f64,
    m_start: i32,
    m_end: i32,
    m_scale: f64,
) -> f64 {
    let anchor = if m_start != p_start && m_end == p_end {
        m_end
    } else {
        m_start.max(p_start)
    };
    p_logical + f64::from(anchor - p_start) / p_scale - f64::from(anchor - m_start) / m_scale
}

/// A rectangle for `m`, which shares no edge with a placed monitor: its
/// pixel offset from the nearest placed monitor, in that monitor's units,
/// and the side it lies on for [`push_clear`].
fn detached(placed: &[Placed], m: &Monitor) -> (Rect, Side) {
    let center = m.pixels.center();
    let p = placed
        .iter()
        .min_by_key(|p| p.monitor.pixels.distance_squared(center))
        .expect("the primary monitor is always placed");
    let pp = &p.monitor.pixels;
    let rect = Rect::new(
        p.logical.x + f64::from(m.pixels.left - pp.left) / p.monitor.scale,
        p.logical.y + f64::from(m.pixels.top - pp.top) / p.monitor.scale,
        f64::from(m.pixels.width()) / m.scale,
        f64::from(m.pixels.height()) / m.scale,
    );
    let from = pp.center();
    let (dx, dy) = (center.x - from.x, center.y - from.y);
    let side = match (dx.abs() >= dy.abs(), dx >= 0, dy >= 0) {
        (true, true, _) => Side::Right,
        (true, false, _) => Side::Left,
        (false, _, true) => Side::Below,
        (false, _, false) => Side::Above,
    };
    (rect, side)
}

/// Moves `rect` towards `side` until it overlaps no placed monitor. Each
/// step moves it past the far edge of everything it overlapped, so this
/// ends after at most one step per placed monitor.
fn push_clear(placed: &[Placed], mut rect: Rect, side: Side) -> Rect {
    for _ in 0..=placed.len() {
        let hits: Vec<Rect> = placed
            .iter()
            .map(|p| p.logical)
            .filter(|o| overlaps(rect, *o))
            .collect();
        if hits.is_empty() {
            break;
        }
        match side {
            Side::Right => rect.x = hits.iter().map(Rect::right).fold(rect.x, f64::max),
            Side::Left => {
                rect.x = hits.iter().map(Rect::left).fold(rect.right(), f64::min) - rect.width
            }
            Side::Below => rect.y = hits.iter().map(Rect::bottom).fold(rect.y, f64::max),
            Side::Above => {
                rect.y = hits.iter().map(Rect::top).fold(rect.bottom(), f64::min) - rect.height
            }
        }
    }
    rect
}

fn is_free(placed: &[Placed], rect: Rect) -> bool {
    placed.iter().all(|p| !overlaps(rect, p.logical))
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

    fn monitor(id: &str, left: i32, top: i32, width: i32, height: i32, scale: f64) -> Monitor {
        Monitor {
            id: id.to_owned(),
            pixels: PixelRect::new(left, top, left + width, top + height),
            scale,
            primary: left == 0 && top == 0,
        }
    }

    fn rects(map: &DesktopMap) -> Vec<(String, Rect)> {
        map.displays()
            .into_iter()
            .map(|d| (d.id, d.bounds))
            .collect()
    }

    /// Every pixel maps to a logical point and back to itself, and lands in
    /// the logical rectangle of its own monitor.
    fn assert_round_trips(map: &DesktopMap) {
        for placed in &map.placed {
            let r = placed.monitor.pixels;
            let step = (r.width().max(r.height()) / 97).max(1);
            let xs = (r.left..r.right)
                .step_by(step as usize)
                .chain([r.right - 1]);
            for x in xs {
                let ys = (r.top..r.bottom)
                    .step_by(step as usize)
                    .chain([r.bottom - 1]);
                for y in ys {
                    let p = Pixel::new(x, y);
                    let logical = map.to_logical(p).unwrap();
                    assert!(
                        placed.logical.contains(logical),
                        "{p:?} -> {logical:?} is outside {:?}",
                        placed.logical
                    );
                    assert_eq!(map.to_physical(logical), Some(p), "via {logical:?}");
                }
            }
        }
        for (i, a) in map.placed.iter().enumerate() {
            for b in &map.placed[i + 1..] {
                assert!(!overlaps(a.logical, b.logical), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn a_single_scaled_monitor_divides_by_its_scale() {
        let map = DesktopMap::new(vec![monitor("A", 0, 0, 3840, 2160, 1.5)]);
        assert_eq!(
            rects(&map),
            vec![("A".to_owned(), Rect::new(0.0, 0.0, 2560.0, 1440.0))]
        );
        assert_eq!(
            map.to_logical(Pixel::new(1500, 300)),
            Some(Point::new(1000.0, 200.0))
        );
        assert_eq!(
            map.to_physical(Point::new(1000.0, 200.0)),
            Some(Pixel::new(1500, 300))
        );
        assert_round_trips(&map);
    }

    #[test]
    fn mixed_scales_in_a_row_still_touch() {
        // 4K at 200% left of a 1080p primary at 100%, a 1440p at 125% on
        // the right, all top-aligned.
        let map = DesktopMap::new(vec![
            monitor("C", 1920, 0, 2560, 1440, 1.25),
            monitor("B", -3840, 0, 3840, 2160, 2.0),
            monitor("A", 0, 0, 1920, 1080, 1.0),
        ]);
        assert_eq!(
            rects(&map),
            vec![
                ("A".to_owned(), Rect::new(0.0, 0.0, 1920.0, 1080.0)),
                ("B".to_owned(), Rect::new(-1920.0, 0.0, 1920.0, 1080.0)),
                ("C".to_owned(), Rect::new(1920.0, 0.0, 2048.0, 1152.0)),
            ]
        );
        assert_round_trips(&map);

        // The pixels on either side of each seam map to the logical points
        // on either side of the shared logical edge.
        assert_eq!(
            map.to_logical(Pixel::new(1919, 500)),
            Some(Point::new(1919.0, 500.0))
        );
        assert_eq!(
            map.to_logical(Pixel::new(1920, 500)),
            Some(Point::new(1920.0, 400.0))
        );
        assert_eq!(
            map.to_logical(Pixel::new(-1, 500)),
            Some(Point::new(-0.5, 250.0))
        );
        assert_eq!(
            map.to_logical(Pixel::new(0, 500)),
            Some(Point::new(0.0, 500.0))
        );
        // A logical point exactly on a shared edge belongs to the monitor on
        // its right, like a pixel on a shared edge.
        assert_eq!(
            map.to_physical(Point::new(1920.0, 400.0)),
            Some(Pixel::new(1920, 500))
        );
        assert_eq!(
            map.to_physical(Point::new(0.0, 250.0)),
            Some(Pixel::new(0, 250))
        );
        assert_eq!(
            map.to_physical(Point::new(-0.25, 250.0)),
            Some(Pixel::new(-1, 500))
        );
    }

    #[test]
    fn bottom_aligned_monitors_stay_bottom_aligned() {
        // A taller 4K at 150% to the right of a 1080p primary, aligned at
        // the bottom as on a desk.
        let map = DesktopMap::new(vec![
            monitor("A", 0, 0, 1920, 1080, 1.0),
            monitor("B", 1920, -1080, 3840, 2160, 1.5),
        ]);
        let b = rects(&map)[1].1;
        assert_eq!(b, Rect::new(1920.0, -360.0, 2560.0, 1440.0));
        assert_eq!(b.bottom(), 1080.0);
        assert_round_trips(&map);
    }

    #[test]
    fn a_monitor_above_keeps_its_offset_along_the_edge() {
        // A laptop panel at 150% below a 1440p monitor at 100%, centred.
        let map = DesktopMap::new(vec![
            monitor("Laptop", 0, 0, 2880, 1800, 1.5),
            monitor("Desk", -160, -1440, 3200, 1440, 1.0),
        ]);
        // The shared segment starts at pixel x 0, which is logical 0 for the
        // laptop; the desk monitor starts 160 pixels further left.
        assert_eq!(rects(&map)[1].1, Rect::new(-160.0, -1440.0, 3200.0, 1440.0));
        assert_round_trips(&map);
    }

    #[test]
    fn an_overlapping_placement_is_pushed_clear() {
        // A 4K primary at 200% with two 1080p monitors at 100% side by side
        // below it, and a tall one to its right. In logical units the
        // primary is only 1920 wide, so the second monitor below fits
        // neither under the primary nor beside the first one.
        let map = DesktopMap::new(vec![
            monitor("P", 0, 0, 3840, 2160, 2.0),
            monitor("Below", 0, 2160, 1920, 1080, 1.0),
            monitor("BelowRight", 1920, 2160, 1920, 1080, 1.0),
            monitor("Right", 3840, 0, 1920, 2160, 1.0),
        ]);
        assert_round_trips(&map);
        let r = rects(&map);
        assert_eq!(
            r,
            vec![
                ("P".to_owned(), Rect::new(0.0, 0.0, 1920.0, 1080.0)),
                ("Right".to_owned(), Rect::new(1920.0, 0.0, 1920.0, 2160.0)),
                ("Below".to_owned(), Rect::new(0.0, 1080.0, 1920.0, 1080.0)),
                // Under the primary it would cover Below; beside Below it
                // would cover Right. Pushed down past Below is the shorter
                // move.
                (
                    "BelowRight".to_owned(),
                    Rect::new(0.0, 2160.0, 1920.0, 1080.0)
                ),
            ]
        );
    }

    #[test]
    fn a_monitor_touching_only_at_a_corner_is_placed_by_offset() {
        let map = DesktopMap::new(vec![
            monitor("A", 0, 0, 1920, 1080, 1.0),
            monitor("B", 1920, 1080, 2560, 1440, 2.0),
        ]);
        assert_eq!(rects(&map)[1].1, Rect::new(1920.0, 1080.0, 1280.0, 720.0));
        assert_round_trips(&map);
    }

    #[test]
    fn the_result_does_not_depend_on_enumeration_order() {
        let monitors = vec![
            monitor("A", 0, 0, 1920, 1080, 1.0),
            monitor("B", 1920, 0, 3840, 2160, 2.0),
            monitor("C", 0, 1080, 1920, 1200, 1.25),
            monitor("D", -1280, 200, 1280, 1024, 1.0),
        ];
        let forward = DesktopMap::new(monitors.clone());
        let mut reversed = monitors;
        reversed.reverse();
        assert_eq!(forward, DesktopMap::new(reversed));
        assert_round_trips(&forward);
    }

    #[test]
    fn points_off_the_desktop_move_onto_the_nearest_monitor() {
        let map = DesktopMap::new(vec![
            monitor("A", 0, 0, 1920, 1080, 1.0),
            monitor("B", 1920, 0, 2560, 1440, 2.0),
        ]);
        // Below the shorter monitor, in the gap beside the taller one.
        assert_eq!(
            map.to_logical(Pixel::new(500, 1300)),
            Some(Point::new(500.0, 1079.0))
        );
        assert_eq!(
            map.to_physical(Point::new(-50.0, -50.0)),
            Some(Pixel::new(0, 0))
        );
        // The far right edge is exclusive, so it maps to the last pixel.
        assert_eq!(
            map.to_physical(Point::new(3200.0, 10.0)),
            Some(Pixel::new(4479, 20))
        );
        assert_eq!(
            map.to_physical(Point::new(f64::NAN, 10.0)),
            Some(Pixel::new(0, 10))
        );
        assert_eq!(
            DesktopMap::new(Vec::new()).to_logical(Pixel::new(0, 0)),
            None
        );
        assert_eq!(map.virtual_screen(), Some(PixelRect::new(0, 0, 4480, 1440)));
    }

    #[test]
    fn deltas_are_logical_even_past_the_edge() {
        let map = DesktopMap::new(vec![
            monitor("A", 0, 0, 1920, 1080, 1.0),
            monitor("B", 1920, 0, 3840, 2160, 2.0),
        ]);
        // Within the 200% monitor.
        assert_eq!(
            map.logical_delta(Pixel::new(2000, 100), Pixel::new(2010, 90)),
            Point::new(5.0, -5.0)
        );
        // From the last pixel of A onto B.
        assert_eq!(
            map.logical_delta(Pixel::new(1919, 100), Pixel::new(1922, 100)),
            Point::new(2.0, -50.0)
        );
        // Past the right edge of the desktop, at B's scale.
        assert_eq!(
            map.logical_delta(Pixel::new(5759, 100), Pixel::new(5765, 100)),
            Point::new(3.0, 0.0)
        );
    }

    #[test]
    fn outer_edges_are_where_no_monitor_continues() {
        let map = DesktopMap::new(vec![
            monitor("A", 0, 0, 1920, 1080, 1.0),
            monitor("B", 1920, 0, 2560, 1440, 1.0),
        ]);
        assert_eq!(map.outer_edges(Pixel::new(1919, 500)), Edges::default());
        assert_eq!(
            map.outer_edges(Pixel::new(4479, 500)),
            Edges {
                right: true,
                ..Edges::default()
            }
        );
        // Bottom of the shorter monitor: below it is no monitor.
        assert_eq!(
            map.outer_edges(Pixel::new(1919, 1079)),
            Edges {
                bottom: true,
                ..Edges::default()
            }
        );
        assert_eq!(
            map.outer_edges(Pixel::new(0, 0)),
            Edges {
                left: true,
                top: true,
                ..Edges::default()
            }
        );
        assert_eq!(map.outer_edges(Pixel::new(-5, 0)), Edges::default());
        assert_eq!(
            map.monitor_center(Pixel::new(3000, 10)),
            Some(Pixel::new(3200, 720))
        );
    }
}
