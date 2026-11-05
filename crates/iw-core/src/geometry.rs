//! Logical-coordinate geometry.
//!
//! All coordinates are logical units (physical pixels divided by the display
//! scale factor). Rectangles are half-open: a point on the right or bottom
//! edge belongs to the neighbouring rectangle, not this one.

use std::ops::{Add, Sub};

/// Tolerance for comparing edges that should coincide.
pub const EPSILON: f64 = 1e-6;

/// How far inside a rectangle `Rect::clamp` keeps a point from the
/// exclusive right and bottom edges.
const CLAMP_INSET: f64 = 1e-3;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

impl Add for Point {
    type Output = Point;

    fn add(self, rhs: Point) -> Point {
        Point::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl Sub for Point {
    type Output = Point;

    fn sub(self, rhs: Point) -> Point {
        Point::new(self.x - rhs.x, self.y - rhs.y)
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
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn left(&self) -> f64 {
        self.x
    }

    pub fn top(&self) -> f64 {
        self.y
    }

    pub fn right(&self) -> f64 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }

    pub fn center(&self) -> Point {
        Point::new(self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.left() && p.x < self.right() && p.y >= self.top() && p.y < self.bottom()
    }

    pub fn translate(&self, by: Point) -> Rect {
        Rect::new(self.x + by.x, self.y + by.y, self.width, self.height)
    }

    /// The point inside this rectangle closest to `p`.
    pub fn clamp(&self, p: Point) -> Point {
        Point::new(
            p.x.clamp(self.left(), self.right() - CLAMP_INSET),
            p.y.clamp(self.top(), self.bottom() - CLAMP_INSET),
        )
    }

    pub fn distance_squared(&self, p: Point) -> f64 {
        let c = self.clamp(p);
        let (dx, dy) = (p.x - c.x, p.y - c.y);
        dx * dx + dy * dy
    }
}

pub fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < EPSILON
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_half_open() {
        let r = Rect::new(0.0, 0.0, 100.0, 50.0);
        assert!(r.contains(Point::new(0.0, 0.0)));
        assert!(r.contains(Point::new(99.9, 49.9)));
        assert!(!r.contains(Point::new(100.0, 10.0)));
        assert!(!r.contains(Point::new(10.0, 50.0)));
        assert!(!r.contains(Point::new(-0.1, 10.0)));
    }

    #[test]
    fn clamp_stays_inside() {
        let r = Rect::new(10.0, 10.0, 100.0, 50.0);
        let c = r.clamp(Point::new(500.0, -20.0));
        assert!(r.contains(c));
        assert!(approx_eq(c.y, 10.0));
        assert!(c.x < 110.0 && c.x > 109.9);
        assert_eq!(r.clamp(Point::new(20.0, 20.0)), Point::new(20.0, 20.0));
    }
}
