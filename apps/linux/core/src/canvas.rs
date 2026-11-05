//! Geometry for drawing and editing the arrangement, free of GTK so it can
//! be tested: the same rules as the macOS app's `CanvasGeometry.swift` and
//! the Windows app's `CanvasGeometry.cs`, with their test cases. Everything
//! is in logical units, y growing downwards, as the background service has
//! it.

use iw_proto::app::v1::Arrangement;

/// Edges closer than this count as touching.
pub const EPSILON: f64 = 0.001;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Point {
        Point { x, y }
    }

    pub fn offset(self, dx: f64, dy: f64) -> Point {
        Point::new(self.x + dx, self.y + dy)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

impl Size {
    pub const fn new(width: f64, height: f64) -> Size {
        Size { width, height }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    pub fn min_x(&self) -> f64 {
        self.x
    }

    pub fn min_y(&self) -> f64 {
        self.y
    }

    pub fn max_x(&self) -> f64 {
        self.x + self.width
    }

    pub fn max_y(&self) -> f64 {
        self.y + self.height
    }

    pub fn mid_x(&self) -> f64 {
        self.x + self.width / 2.0
    }

    pub fn mid_y(&self) -> f64 {
        self.y + self.height / 2.0
    }

    pub fn offset(&self, dx: f64, dy: f64) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.width, self.height)
    }

    /// Grows the rectangle by `dx` and `dy` on each side; negative shrinks
    /// it.
    pub fn inflate(&self, dx: f64, dy: f64) -> Rect {
        Rect::new(
            self.x - dx,
            self.y - dy,
            self.width + 2.0 * dx,
            self.height + 2.0 * dy,
        )
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.min_x() && p.x <= self.max_x() && p.y >= self.min_y() && p.y <= self.max_y()
    }

    pub fn contains_rect(&self, r: &Rect) -> bool {
        r.min_x() >= self.min_x()
            && r.max_x() <= self.max_x()
            && r.min_y() >= self.min_y()
            && r.max_y() <= self.max_y()
    }

    pub fn union(&self, other: &Rect) -> Rect {
        let min_x = self.min_x().min(other.min_x());
        let min_y = self.min_y().min(other.min_y());
        Rect::new(
            min_x,
            min_y,
            self.max_x().max(other.max_x()) - min_x,
            self.max_y().max(other.max_y()) - min_y,
        )
    }
}

/// A computer on the canvas: its screens in its own desktop coordinates,
/// and where its desktop origin sits on the canvas.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: String,
    pub offset: Point,
    pub screens: Vec<Rect>,
    pub main_screen: usize,
}

impl Device {
    /// The screens on the canvas.
    pub fn frames(&self) -> Vec<Rect> {
        self.screens
            .iter()
            .map(|s| s.offset(self.offset.x, self.offset.y))
            .collect()
    }

    pub fn bounds(&self) -> Rect {
        union(&self.frames()).unwrap_or_default()
    }

    pub fn moved_to(&self, offset: Point) -> Device {
        Device {
            offset,
            ..self.clone()
        }
    }
}

/// A shared edge between two computers' screens: where the cursor crosses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seam {
    pub start: Point,
    pub end: Point,
}

/// The computers that have a place and screens, on the canvas.
pub fn devices(arrangement: &Arrangement) -> Vec<Device> {
    arrangement
        .devices
        .iter()
        .filter(|d| d.placed && !d.screens.is_empty())
        .map(|d| Device {
            id: d.id.clone(),
            offset: Point::new(d.x, d.y),
            screens: d
                .screens
                .iter()
                .map(|s| Rect::new(s.x, s.y, s.width, s.height))
                .collect(),
            main_screen: d.screens.iter().position(|s| s.primary).unwrap_or(0),
        })
        .collect()
}

pub fn union(rects: &[Rect]) -> Option<Rect> {
    rects.iter().fold(None, |acc: Option<Rect>, r| {
        Some(acc.map_or(*r, |u| u.union(r)))
    })
}

/// Maps canvas coordinates into a view and back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    pub scale: f64,
    pub origin: Point,
}

impl Fit {
    pub fn to_view(&self, p: Point) -> Point {
        Point::new(
            self.origin.x + p.x * self.scale,
            self.origin.y + p.y * self.scale,
        )
    }

    pub fn rect_to_view(&self, r: &Rect) -> Rect {
        Rect::new(
            self.origin.x + r.x * self.scale,
            self.origin.y + r.y * self.scale,
            r.width * self.scale,
            r.height * self.scale,
        )
    }

    pub fn to_canvas(&self, p: Point) -> Point {
        Point::new(
            (p.x - self.origin.x) / self.scale,
            (p.y - self.origin.y) / self.scale,
        )
    }
}

/// Fits `content` centered into a view, leaving `padding` on every side.
pub fn fit_into(content: &Rect, view: Size, padding: f64) -> Fit {
    if content.width <= 0.0 || content.height <= 0.0 {
        return Fit {
            scale: 1.0,
            origin: Point::new(view.width / 2.0, view.height / 2.0),
        };
    }
    let scale = ((view.width - 2.0 * padding) / content.width)
        .min((view.height - 2.0 * padding) / content.height)
        .max(0.001);
    Fit {
        scale,
        origin: Point::new(
            (view.width - content.width * scale) / 2.0 - content.min_x() * scale,
            (view.height - content.height * scale) / 2.0 - content.min_y() * scale,
        ),
    }
}

/// Like [`fit_into`], but never larger than `max_scale`, so one computer
/// alone does not fill the whole view; centered all the same.
pub fn fit_capped(content: &Rect, view: Size, padding: f64, max_scale: f64) -> Fit {
    let fit = fit_into(content, view, padding);
    if fit.scale <= max_scale || content.width <= 0.0 || content.height <= 0.0 {
        return fit;
    }
    let scale = max_scale;
    Fit {
        scale,
        origin: Point::new(
            (view.width - content.width * scale) / 2.0 - content.min_x() * scale,
            (view.height - content.height * scale) / 2.0 - content.min_y() * scale,
        ),
    }
}

/// Whether any screen of `a` overlaps any screen of `b`. Touching is not
/// overlapping.
pub fn overlaps(a: &[Rect], b: &[Rect]) -> bool {
    a.iter().any(|r| {
        b.iter().any(|s| {
            r.min_x() < s.max_x() - EPSILON
                && s.min_x() < r.max_x() - EPSILON
                && r.min_y() < s.max_y() - EPSILON
                && s.min_y() < r.max_y() - EPSILON
        })
    })
}

/// Moves `moving` so that its edges meet or line up with the nearest edges
/// of the other computers, if they are within `threshold`, and returns its
/// new offset. Each axis snaps on its own.
pub fn snap<'a>(
    moving: &Device,
    others: impl IntoIterator<Item = &'a Device>,
    threshold: f64,
) -> Point {
    let (mut best_x, mut dx, mut best_y, mut dy) = (threshold + 1.0, 0.0, threshold + 1.0, 0.0);
    let consider = |value: f64, best: &mut f64, delta: &mut f64| {
        if value.abs() <= threshold && value.abs() < *best {
            *best = value.abs();
            *delta = value;
        }
    };
    let other_frames: Vec<Rect> = others.into_iter().flat_map(Device::frames).collect();
    for f in moving.frames() {
        for o in &other_frames {
            let near_vertically =
                f.min_y() < o.max_y() + threshold && o.min_y() < f.max_y() + threshold;
            let near_horizontally =
                f.min_x() < o.max_x() + threshold && o.min_x() < f.max_x() + threshold;
            if near_vertically {
                // Side by side.
                consider(o.max_x() - f.min_x(), &mut best_x, &mut dx);
                consider(o.min_x() - f.max_x(), &mut best_x, &mut dx);
            }
            if near_horizontally {
                // One above the other.
                consider(o.max_y() - f.min_y(), &mut best_y, &mut dy);
                consider(o.min_y() - f.max_y(), &mut best_y, &mut dy);
            }
            let side_by_side = (o.max_x() - f.min_x()).abs() <= threshold
                || (o.min_x() - f.max_x()).abs() <= threshold;
            let stacked = (o.max_y() - f.min_y()).abs() <= threshold
                || (o.min_y() - f.max_y()).abs() <= threshold;
            if side_by_side {
                // Line up top or bottom edges.
                consider(o.min_y() - f.min_y(), &mut best_y, &mut dy);
                consider(o.max_y() - f.max_y(), &mut best_y, &mut dy);
            }
            if stacked {
                consider(o.min_x() - f.min_x(), &mut best_x, &mut dx);
                consider(o.max_x() - f.max_x(), &mut best_x, &mut dx);
            }
        }
    }
    Point::new(moving.offset.x + dx, moving.offset.y + dy)
}

/// The seams between screens of different computers.
pub fn seams(devices: &[Device]) -> Vec<Seam> {
    let mut seams = Vec::new();
    for (i, a) in devices.iter().enumerate() {
        for b in &devices[i + 1..] {
            for p in a.frames() {
                for q in b.frames() {
                    if let Some(seam) = seam_between(&p, &q).or_else(|| seam_between(&q, &p)) {
                        seams.push(seam);
                    }
                }
            }
        }
    }
    seams
}

/// The computers that share a seam with `device`.
pub fn neighbours(device: &Device, all: &[Device]) -> Vec<String> {
    all.iter()
        .filter(|other| {
            other.id != device.id && !seams(&[device.clone(), (*other).clone()]).is_empty()
        })
        .map(|other| other.id.clone())
        .collect()
}

/// The points of a zigzag stitch along a seam, `pitch` apart and reaching
/// `reach` to either side; it starts and ends on the seam.
pub fn stitch(start: Point, end: Point, pitch: f64, reach: f64) -> Vec<Point> {
    let length = ((end.x - start.x).powi(2) + (end.y - start.y).powi(2)).sqrt();
    if length <= 0.0 {
        return Vec::new();
    }
    let (ux, uy) = ((end.x - start.x) / length, (end.y - start.y) / length);
    let (nx, ny) = (-uy, ux);
    let count = ((length / pitch) as usize).max(1);
    let mut points = Vec::with_capacity(count + 1);
    points.push(start);
    for i in 1..=count {
        let t = i as f64 / count as f64 * length;
        let side = if i % 2 == 0 { -1.0 } else { 1.0 };
        let off = if i == count { 0.0 } else { reach * side };
        points.push(Point::new(
            start.x + ux * t + nx * off,
            start.y + uy * t + ny * off,
        ));
    }
    points
}

/// Which of a computer's screen tiles a small view labels: the main
/// screen, else the widest, as long as the tile leaves room for the name.
/// `None` when none does, so the name is left out rather than spilling
/// over its tile.
pub fn label_tile(tiles: &[Rect], main: usize, room: Size) -> Option<usize> {
    let fits = |tile: &Rect| tile.width >= room.width && tile.height >= room.height;
    if tiles.get(main).is_some_and(fits) {
        return Some(main);
    }
    let mut widest: Option<usize> = None;
    for (i, tile) in tiles.iter().enumerate() {
        if fits(tile) && widest.is_none_or(|w| tile.width > tiles[w].width) {
            widest = Some(i);
        }
    }
    widest
}

/// How a computer's name label shares its room with the jump shortcut
/// beside the name: `room` is how wide the
/// label's text may be at most, `shortcut` the shortcut's measured width
/// (0 for none) and `gap` the space between the two. The shortcut is shown
/// only while the name keeps at least `least_name`; the name may then take
/// what is left, and all of the room without it.
pub fn name_beside(room: f64, shortcut: f64, gap: f64, least_name: f64) -> (bool, f64) {
    let room = room.max(0.0);
    if shortcut > 0.0 {
        let left = room - gap - shortcut.ceil();
        if left >= least_name {
            return (true, left);
        }
    }
    (false, room)
}

/// Where the keyboard-and-mouse badge of `size` goes on `tile`: in the
/// middle, or in the middle of the space below the name when the name,
/// ending at `name_bottom`, would cover it, keeping `inset` from the
/// tile's edges. Without that much room below the name it sits as low as
/// the tile allows. A badge larger than the tile stays centered on it and
/// spills over evenly, since it is the only sign of where the keyboard and
/// mouse are.
pub fn badge_frame(size: Size, tile: &Rect, name_bottom: Option<f64>, inset: f64) -> Rect {
    let frame = Rect::new(
        tile.mid_x() - size.width / 2.0,
        tile.mid_y() - size.height / 2.0,
        size.width,
        size.height,
    );
    const GAP: f64 = 1.0;
    let bottom = tile.max_y() - inset;
    let Some(name) = name_bottom else {
        return frame;
    };
    if frame.min_y() >= name + GAP || size.height > tile.height - 2.0 * inset {
        return frame;
    }
    let top = name + GAP;
    let y = if bottom - top >= size.height {
        (top + bottom - size.height) / 2.0
    } else {
        bottom - size.height
    };
    Rect { y, ..frame }
}

/// The computer under `point` (view coordinates), by its screens grown by
/// `slop`; the topmost, drawn last, wins.
pub fn hit_test(devices: &[Device], fit: &Fit, point: Point, slop: f64) -> Option<String> {
    devices
        .iter()
        .rev()
        .find(|d| {
            d.frames()
                .iter()
                .any(|f| fit.rect_to_view(f).inflate(slop, slop).contains(point))
        })
        .map(|d| d.id.clone())
}

/// The first `progress` (0 to 1) of a line through `points`, as a seam is
/// sewn shut.
pub fn sewn(points: &[Point], progress: f64) -> Vec<Point> {
    if progress >= 1.0 || points.len() < 2 {
        return points.to_vec();
    }
    let distance = |a: Point, b: Point| ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
    let total: f64 = points.windows(2).map(|w| distance(w[0], w[1])).sum();
    let mut left = total * progress.max(0.0);
    let mut out = vec![points[0]];
    for w in points.windows(2) {
        if left <= 0.0 {
            break;
        }
        let step = distance(w[0], w[1]);
        if step <= left {
            out.push(w[1]);
            left -= step;
        } else {
            let t = left / step;
            out.push(Point::new(
                w[0].x + (w[1].x - w[0].x) * t,
                w[0].y + (w[1].y - w[0].y) * t,
            ));
            left = 0.0;
        }
    }
    out
}

/// The edge `p` shares with `q` where q is right of or below p.
fn seam_between(p: &Rect, q: &Rect) -> Option<Seam> {
    if (p.max_x() - q.min_x()).abs() < EPSILON {
        let (top, bottom) = (p.min_y().max(q.min_y()), p.max_y().min(q.max_y()));
        if bottom - top > EPSILON {
            return Some(Seam {
                start: Point::new(q.min_x(), top),
                end: Point::new(q.min_x(), bottom),
            });
        }
    }
    if (p.max_y() - q.min_y()).abs() < EPSILON {
        let (left, right) = (p.min_x().max(q.min_x()), p.max_x().min(q.max_x()));
        if right - left > EPSILON {
            return Some(Seam {
                start: Point::new(left, q.min_y()),
                end: Point::new(right, q.min_y()),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    //! The same cases as the macOS and Windows apps' CanvasGeometryTests.

    use super::*;
    use iw_proto::app::v1::{ArrangedDevice, Screen};

    fn mac() -> Device {
        Device {
            id: "mac".into(),
            offset: Point::new(0.0, 0.0),
            screens: vec![Rect::new(0.0, 0.0, 1512.0, 982.0)],
            main_screen: 0,
        }
    }

    fn pc(x: f64, y: f64) -> Device {
        Device {
            id: "pc".into(),
            offset: Point::new(x, y),
            screens: vec![Rect::new(0.0, 0.0, 2560.0, 1440.0)],
            main_screen: 0,
        }
    }

    fn at(id: &str, screens: Vec<Rect>) -> Device {
        Device {
            id: id.into(),
            offset: Point::default(),
            screens,
            main_screen: 0,
        }
    }

    fn arranged(
        id: &str,
        x: f64,
        y: f64,
        screens: &[(f64, f64, f64, f64, bool)],
    ) -> ArrangedDevice {
        ArrangedDevice {
            id: id.into(),
            name: id.into(),
            placed: true,
            x,
            y,
            screens: screens
                .iter()
                .map(|&(x, y, width, height, primary)| Screen {
                    id: format!("{id}-{x}"),
                    x,
                    y,
                    width,
                    height,
                    primary,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn places_each_screen_at_its_devices_origin() {
        let arrangement = Arrangement {
            version: 1,
            devices: vec![
                arranged("mac", 0.0, 0.0, &[(0.0, 0.0, 1512.0, 982.0, true)]),
                arranged(
                    "pc",
                    1512.0,
                    0.0,
                    &[
                        (-1920.0, 0.0, 1920.0, 1080.0, false),
                        (0.0, 0.0, 1920.0, 1080.0, true),
                    ],
                ),
                // Not placed yet: left out.
                ArrangedDevice {
                    id: "new".into(),
                    placed: false,
                    ..Default::default()
                },
            ],
        };
        let devices = devices(&arrangement);
        let ids: Vec<&str> = devices.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["mac", "pc"]);
        assert_eq!(
            devices[1].frames()[0],
            Rect::new(-408.0, 0.0, 1920.0, 1080.0)
        );
        assert_eq!(devices[1].main_screen, 1);
        assert_eq!(devices[1].bounds(), Rect::new(-408.0, 0.0, 3840.0, 1080.0));
    }

    #[test]
    fn snaps_beside_and_lines_up_tops() {
        // Dropped a little right of the Mac and a little low.
        assert_eq!(
            snap(&pc(1520.0, 6.0), &[mac()], 10.0),
            Point::new(1512.0, 0.0)
        );
    }

    #[test]
    fn leaves_far_away_devices_alone() {
        assert_eq!(
            snap(&pc(1700.0, 300.0), &[mac()], 10.0),
            Point::new(1700.0, 300.0)
        );
    }

    #[test]
    fn snaps_on_top() {
        assert_eq!(
            snap(&pc(4.0, -1436.0), &[mac()], 10.0),
            Point::new(0.0, -1440.0)
        );
    }

    #[test]
    fn touching_is_not_overlapping() {
        assert!(!overlaps(&mac().frames(), &pc(1512.0, 0.0).frames()));
        assert!(overlaps(&mac().frames(), &pc(1500.0, 0.0).frames()));
    }

    #[test]
    fn finds_the_shared_edge() {
        assert_eq!(
            seams(&[mac(), pc(1512.0, 400.0)]),
            [Seam {
                start: Point::new(1512.0, 400.0),
                end: Point::new(1512.0, 982.0)
            }]
        );
        assert!(seams(&[mac(), pc(1600.0, 0.0)]).is_empty());
        assert_eq!(neighbours(&mac(), &[mac(), pc(1512.0, 400.0)]), ["pc"]);
        assert!(neighbours(&mac(), &[mac(), pc(1600.0, 0.0)]).is_empty());
    }

    #[test]
    fn stitches_only_edges_that_touch() {
        let mac = at("mac", vec![Rect::new(0.0, 100.0, 1512.0, 982.0)]);
        let pc = at("pc", vec![Rect::new(1512.0, 0.0, 1920.0, 1080.0)]);
        let below = at("tv", vec![Rect::new(1600.0, 1080.0, 800.0, 600.0)]);
        let apart = at("far", vec![Rect::new(5000.0, 0.0, 100.0, 100.0)]);
        assert_eq!(
            seams(&[mac, pc, below, apart]),
            [
                Seam {
                    start: Point::new(1512.0, 100.0),
                    end: Point::new(1512.0, 1080.0)
                },
                Seam {
                    start: Point::new(1600.0, 1080.0),
                    end: Point::new(2400.0, 1080.0)
                },
            ]
        );
    }

    #[test]
    fn a_stitch_starts_and_ends_on_the_seam() {
        let points = stitch(Point::new(10.0, 0.0), Point::new(10.0, 20.0), 4.0, 2.0);
        assert_eq!(points.len(), 6);
        assert_eq!(points[0], Point::new(10.0, 0.0));
        assert_eq!(points[5], Point::new(10.0, 20.0));
        // Across the seam, alternately to either side.
        assert!((points[1].x - 8.0).abs() < 1e-6);
        assert!((points[2].x - 12.0).abs() < 1e-6);
        assert!(stitch(Point::new(1.0, 1.0), Point::new(1.0, 1.0), 4.0, 2.0).is_empty());
    }

    #[test]
    fn a_seam_is_sewn_along_its_length() {
        let line = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
        ];
        assert_eq!(sewn(&line, 1.0), line);
        assert_eq!(
            sewn(&line, 0.5),
            [Point::new(0.0, 0.0), Point::new(10.0, 0.0)]
        );
        assert_eq!(
            sewn(&line, 0.75),
            [
                Point::new(0.0, 0.0),
                Point::new(10.0, 0.0),
                Point::new(10.0, 5.0)
            ]
        );
        assert_eq!(sewn(&line, 0.0), [Point::new(0.0, 0.0)]);
    }

    #[test]
    fn fit_centers_content() {
        let fit = fit_into(
            &Rect::new(0.0, 0.0, 200.0, 100.0),
            Size::new(420.0, 420.0),
            10.0,
        );
        assert_eq!(fit.scale, 2.0);
        assert_eq!(fit.to_view(Point::new(0.0, 0.0)), Point::new(10.0, 110.0));
        assert_eq!(
            fit.to_view(Point::new(200.0, 100.0)),
            Point::new(410.0, 310.0)
        );
        assert_eq!(
            fit.to_canvas(Point::new(410.0, 310.0)),
            Point::new(200.0, 100.0)
        );

        let panel = fit_into(
            &Rect::new(0.0, 0.0, 3000.0, 1000.0),
            Size::new(320.0, 120.0),
            10.0,
        );
        assert!((panel.scale - 0.1).abs() < 1e-6);
        let r = panel.rect_to_view(&Rect::new(0.0, 0.0, 3000.0, 1000.0));
        assert!((r.x - 10.0).abs() < 1e-9 && (r.y - 10.0).abs() < 1e-9);
        assert!((r.width - 300.0).abs() < 1e-9 && (r.height - 100.0).abs() < 1e-9);
    }

    #[test]
    fn a_capped_fit_stays_centered_and_no_larger() {
        let content = Rect::new(0.0, 0.0, 1000.0, 500.0);
        let view = Size::new(800.0, 600.0);
        let free = fit_into(&content, view, 40.0);
        assert!(free.scale > 0.5);
        let capped = fit_capped(&content, view, 40.0, 0.25);
        assert_eq!(capped.scale, 0.25);
        let r = capped.rect_to_view(&content);
        assert!((r.mid_x() - 400.0).abs() < 1e-9 && (r.mid_y() - 300.0).abs() < 1e-9);
        // Content that needs less than the cap is fitted as before.
        let wide = Rect::new(0.0, 0.0, 8000.0, 1440.0);
        assert_eq!(
            fit_capped(&wide, view, 40.0, 0.25),
            fit_into(&wide, view, 40.0)
        );
    }

    #[test]
    fn labels_the_main_screen_or_the_widest_that_has_room() {
        let room = Size::new(36.0, 20.0);
        let roomy = Rect::new(0.0, 0.0, 90.0, 50.0);
        let narrow = Rect::new(90.0, -10.0, 30.0, 70.0);
        let wide = Rect::new(120.0, 0.0, 60.0, 40.0);
        assert_eq!(label_tile(&[roomy, narrow, wide], 0, room), Some(0));
        assert_eq!(label_tile(&[roomy, narrow, wide], 2, room), Some(2));
        assert_eq!(label_tile(&[roomy, narrow, wide], 1, room), Some(0));
        assert_eq!(label_tile(&[narrow], 0, room), None);
    }

    #[test]
    fn centers_the_badge_when_the_name_leaves_the_middle_free() {
        let badge = Size::new(30.0, 14.0);
        assert_eq!(
            badge_frame(badge, &Rect::new(10.0, 0.0, 80.0, 60.0), Some(17.0), 2.0),
            Rect::new(35.0, 23.0, 30.0, 14.0)
        );
        // No name on this tile: the middle, whatever the tile's size.
        assert_eq!(
            badge_frame(badge, &Rect::new(10.0, 0.0, 50.0, 34.0), None, 2.0),
            Rect::new(20.0, 10.0, 30.0, 14.0)
        );
    }

    #[test]
    fn moves_the_badge_below_a_name_that_covers_the_middle() {
        let badge = Size::new(30.0, 14.0);
        // The space below the name runs from 18 (a point under it) to 32.
        let tile = Rect::new(10.0, 0.0, 50.0, 34.0);
        let frame = badge_frame(badge, &tile, Some(17.0), 2.0);
        assert_eq!(frame, Rect::new(20.0, 18.0, 30.0, 14.0));
        assert!(tile.inflate(-2.0, -2.0).contains_rect(&frame));
        // Taller: centered in that space.
        assert_eq!(
            badge_frame(badge, &Rect::new(10.0, 0.0, 50.0, 42.0), Some(17.0), 2.0),
            Rect::new(20.0, 22.0, 30.0, 14.0)
        );
    }

    #[test]
    fn keeps_the_badge_inside_a_tile_without_room_below_the_name() {
        let badge = Size::new(30.0, 14.0);
        // As low as the tile allows: over the name's last points.
        assert_eq!(
            badge_frame(badge, &Rect::new(10.0, 0.0, 50.0, 26.0), Some(17.0), 2.0),
            Rect::new(20.0, 10.0, 30.0, 14.0)
        );
        // Larger than the tile: centered on it, spilling over evenly.
        assert_eq!(
            badge_frame(badge, &Rect::new(10.0, 0.0, 20.0, 12.0), Some(17.0), 2.0),
            Rect::new(5.0, -1.0, 30.0, 14.0)
        );
    }

    #[test]
    fn finds_the_computer_under_the_pointer() {
        let fit = Fit {
            scale: 0.1,
            origin: Point::new(10.0, 10.0),
        };
        let devices = [mac(), pc(1512.0, 0.0)];
        assert_eq!(
            hit_test(&devices, &fit, Point::new(20.0, 20.0), 0.0).as_deref(),
            Some("mac")
        );
        assert_eq!(
            hit_test(&devices, &fit, Point::new(200.0, 20.0), 0.0).as_deref(),
            Some("pc")
        );
        assert_eq!(hit_test(&devices, &fit, Point::new(20.0, 150.0), 0.0), None);
        // Just outside the Mac's screen, within the slop.
        assert_eq!(
            hit_test(&devices, &fit, Point::new(20.0, 110.0), 4.0).as_deref(),
            Some("mac")
        );
    }

    #[test]
    fn shows_the_shortcut_while_the_name_keeps_room() {
        // 200 of room: the shortcut 90 wide and a gap of 4 leave 106 for the name.
        assert_eq!(name_beside(200.0, 90.0, 4.0, 24.0), (true, 106.0));
        // Measured widths are rounded up, as text is laid out.
        assert_eq!(name_beside(200.0, 90.2, 4.0, 24.0), (true, 105.0));
        // Too little left: the name takes it all.
        assert_eq!(name_beside(100.0, 90.0, 4.0, 24.0), (false, 100.0));
        assert_eq!(name_beside(118.0, 90.0, 4.0, 24.0), (true, 24.0));
        // No shortcut, or no room at all.
        assert_eq!(name_beside(200.0, 0.0, 4.0, 24.0), (false, 200.0));
        assert_eq!(name_beside(-5.0, 90.0, 4.0, 24.0), (false, 0.0));
    }
}
