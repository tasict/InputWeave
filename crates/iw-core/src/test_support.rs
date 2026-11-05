//! Shared fixtures for unit tests.

use crate::geometry::{Point, Rect};
use crate::layout::{Device, DeviceId, Display, Layout};

pub fn id(s: &str) -> DeviceId {
    DeviceId::new(s)
}

fn display(id: &str, x: f64, y: f64, w: f64, h: f64, primary: bool) -> Display {
    Display {
        id: id.to_owned(),
        bounds: Rect::new(x, y, w, h),
        scale: 1.0,
        primary,
    }
}

/// A MacBook on the left, a two-display Windows PC to its right, and a
/// Mac mini sitting on top of the PC's main display.
///
/// Canvas extents:
/// - mac:  x 0..1512,    y 400..1382
/// - win:  x 1512..4072, y 0..1440 (main) and x 4072..5152, y -300..1620 (side)
/// - mini: x 1512..3432, y -1080..0
pub fn desk() -> Layout {
    Layout::new(vec![
        Device {
            id: id("mac"),
            name: "MacBook Pro".to_owned(),
            displays: vec![display("builtin", 0.0, 0.0, 1512.0, 982.0, true)],
            offset: Point::new(0.0, 400.0),
        },
        Device {
            id: id("win"),
            name: "Windows PC".to_owned(),
            displays: vec![
                display("main", 0.0, 0.0, 2560.0, 1440.0, true),
                display("side", 2560.0, -300.0, 1080.0, 1920.0, false),
            ],
            offset: Point::new(1512.0, 0.0),
        },
        Device {
            id: id("mini"),
            name: "Mac mini".to_owned(),
            displays: vec![display("ext", 0.0, 0.0, 1920.0, 1080.0, true)],
            offset: Point::new(1512.0, -1080.0),
        },
    ])
    .expect("fixture layout is valid")
}

/// A MacBook with two computers to its right, neither level with it and
/// neither sharing an edge with it: the upper one ends where the MacBook
/// begins, the lower one stands 188 further out.
///
/// Canvas extents:
/// - mac:   x 0..1512,    y 0..982
/// - upper: x 1512..3432, y -1080..0
/// - lower: x 1700..3620, y 600..1680
pub fn staggered() -> Layout {
    let device = |name: &str, w: f64, h: f64, x: f64, y: f64| Device {
        id: id(name),
        name: name.to_owned(),
        displays: vec![display("main", 0.0, 0.0, w, h, true)],
        offset: Point::new(x, y),
    };
    Layout::new(vec![
        device("mac", 1512.0, 982.0, 0.0, 0.0),
        device("upper", 1920.0, 1080.0, 1512.0, -1080.0),
        device("lower", 1920.0, 1080.0, 1700.0, 600.0),
    ])
    .expect("fixture layout is valid")
}
