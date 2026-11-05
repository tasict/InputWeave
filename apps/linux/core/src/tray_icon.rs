//! The tray icon: two screens side by side with
//! the stitches that join them, as macOS draws it (`StatusIcon.swift`):
//! each state changes one bold element, so it reads without color. The
//! app draws it two ways from the shapes here: as symbolic SVG icons in an
//! icon theme of its own, which the panel tints like its other icons, and
//! as pixmaps for panels that do not look the name up.
//!
//! Symbolic icons are tinted by setting the fill of every `path`, so the
//! SVG uses fills only: outlines are rings, and knocking shapes out of the
//! tile is done with clip paths, whose geometry the tint leaves alone.

use crate::status_text::TrayState;

/// The drawing's own size; icons are square, with the drawing in the
/// middle.
pub const WIDTH: f64 = 22.0;
pub const HEIGHT: f64 = 16.0;
pub const SIZE: f64 = 22.0;
const TOP: f64 = (SIZE - HEIGHT) / 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoundRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub radius: f64,
}

impl RoundRect {
    fn inset(self, by: f64) -> RoundRect {
        RoundRect {
            x: self.x + by,
            y: self.y + by,
            width: self.width - 2.0 * by,
            height: self.height - 2.0 * by,
            radius: (self.radius - by).max(0.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Circle {
    pub cx: f64,
    pub cy: f64,
    pub r: f64,
}

/// A filled shape of the icon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Fill(RoundRect),
    /// `outer` with `inner` cut out.
    Ring {
        outer: RoundRect,
        inner: RoundRect,
    },
}

/// One state's icon, in the drawing's coordinates (y down).
#[derive(Clone, Debug, PartialEq)]
pub struct Icon {
    /// Controlled: everything is knocked out of this tile.
    pub tile: Option<RoundRect>,
    pub shapes: Vec<Shape>,
    /// Attention: a ring of clear space, and the dot inside it.
    pub clear: Option<Circle>,
    pub dot: Option<Circle>,
}

const LEFT: RoundRect = RoundRect {
    x: 1.5,
    y: 3.5,
    width: 8.0,
    height: 9.0,
    radius: 2.0,
};
const RIGHT: RoundRect = RoundRect {
    x: 12.5,
    y: 3.5,
    width: 8.0,
    height: 9.0,
    radius: 2.0,
};
const EDGE: f64 = 1.5;
const STITCH_YS: [f64; 2] = [6.0, 10.0];

fn screen(rect: RoundRect, lit: bool) -> Shape {
    let outer = rect.inset(-EDGE / 2.0);
    if lit {
        Shape::Fill(outer)
    } else {
        Shape::Ring {
            outer,
            inner: rect.inset(EDGE / 2.0),
        }
    }
}

pub fn icon(state: TrayState) -> Icon {
    let mut shapes = vec![
        screen(LEFT, false),
        screen(RIGHT, state == TrayState::Controlling),
    ];
    if state != TrayState::Paused {
        // A line from x 9 to 13, 2 wide with round caps.
        for y in STITCH_YS {
            shapes.push(Shape::Fill(RoundRect {
                x: 8.0,
                y: y - 1.0,
                width: 6.0,
                height: 2.0,
                radius: 1.0,
            }));
        }
    }
    let attention = state == TrayState::Attention;
    Icon {
        tile: (state == TrayState::Controlled).then_some(RoundRect {
            x: 0.0,
            y: 0.0,
            width: WIDTH,
            height: HEIGHT,
            radius: 4.0,
        }),
        shapes,
        clear: attention.then_some(Circle {
            cx: 19.25,
            cy: 2.75,
            r: 3.75,
        }),
        dot: attention.then_some(Circle {
            cx: 19.25,
            cy: 2.75,
            r: 2.5,
        }),
    }
}

/// The icon's name in the app's icon theme.
pub fn icon_name(state: TrayState) -> String {
    format!("cc.ctsite.inputweave-tray-{}-symbolic", state.name())
}

fn n(v: f64) -> String {
    let s = format!("{:.3}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".to_owned()
    } else {
        s.to_owned()
    }
}

fn rect_path(r: RoundRect) -> String {
    let (x, y, w, h, k) = (r.x, r.y + TOP, r.width, r.height, r.radius);
    format!(
        "M{} {}H{}A{k} {k} 0 0 1 {} {}V{}A{k} {k} 0 0 1 {} {}H{}A{k} {k} 0 0 1 {} {}V{}A{k} {k} 0 0 1 {} {}Z",
        n(x + k),
        n(y),
        n(x + w - k),
        n(x + w),
        n(y + k),
        n(y + h - k),
        n(x + w - k),
        n(y + h),
        n(x + k),
        n(x),
        n(y + h - k),
        n(y + k),
        n(x + k),
        n(y),
        k = n(k),
    )
}

fn circle_path(c: Circle) -> String {
    let (x, y, r) = (c.cx, c.cy + TOP, c.r);
    format!(
        "M{} {}A{r} {r} 0 1 1 {} {}A{r} {r} 0 1 1 {} {}Z",
        n(x - r),
        n(y),
        n(x + r),
        n(y),
        n(x - r),
        n(y),
        r = n(r),
    )
}

fn shape_path(shape: Shape) -> String {
    match shape {
        Shape::Fill(r) => rect_path(r),
        Shape::Ring { outer, inner } => format!("{}{}", rect_path(outer), rect_path(inner)),
    }
}

/// Everything but the shape: the whole icon with it cut out, for a clip
/// path, since a shape's outline is one region.
fn complement(shape: &str) -> String {
    format!("M-1 -1H{s}V{s}H-1Z{shape}", s = n(SIZE + 1.0))
}

/// The icon as a symbolic SVG of `SIZE` × `SIZE`.
pub fn svg(state: TrayState) -> String {
    let icon = icon(state);
    let mut defs = String::new();
    let mut body = String::new();
    let mut clips = Vec::new();
    let mut clip = |shape: String, defs: &mut String| {
        let id = format!("c{}", clips.len());
        defs.push_str(&format!(
            "<clipPath id=\"{id}\"><path clip-rule=\"evenodd\" d=\"{}\"/></clipPath>",
            complement(&shape)
        ));
        clips.push(id.clone());
        id
    };
    if let Some(tile) = icon.tile {
        // Knocked out: the tile, clipped by everything outside each shape.
        let ids: Vec<String> = icon
            .shapes
            .iter()
            .map(|s| clip(shape_path(*s), &mut defs))
            .collect();
        for id in &ids {
            body.push_str(&format!("<g clip-path=\"url(#{id})\">"));
        }
        body.push_str(&format!("<path d=\"{}\"/>", rect_path(tile)));
        for _ in &ids {
            body.push_str("</g>");
        }
    } else {
        let ring = icon.clear.map(|c| clip(circle_path(c), &mut defs));
        if let Some(id) = &ring {
            body.push_str(&format!("<g clip-path=\"url(#{id})\">"));
        }
        for shape in &icon.shapes {
            let rule = matches!(shape, Shape::Ring { .. })
                .then_some(" fill-rule=\"evenodd\"")
                .unwrap_or("");
            body.push_str(&format!("<path{rule} d=\"{}\"/>", shape_path(*shape)));
        }
        if ring.is_some() {
            body.push_str("</g>");
        }
        if let Some(dot) = icon.dot {
            body.push_str(&format!("<path d=\"{}\"/>", circle_path(dot)));
        }
    }
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{s}\" height=\"{s}\" viewBox=\"0 0 {s} {s}\">\
         <defs>{defs}</defs><g fill=\"#2e3436\">{body}</g></svg>\n",
        s = n(SIZE),
    )
}

/// Where the drawing sits in the square icon.
pub fn top() -> f64 {
    TOP
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_state_changes_one_element() {
        let normal = icon(TrayState::Normal);
        assert_eq!(normal.shapes.len(), 4);
        assert!(normal.tile.is_none() && normal.dot.is_none());
        let paused = icon(TrayState::Paused);
        assert_eq!(paused.shapes.len(), 2, "the stitches are gone");
        let controlling = icon(TrayState::Controlling);
        assert!(
            matches!(controlling.shapes[1], Shape::Fill(_)),
            "the other screen is lit"
        );
        assert!(matches!(controlling.shapes[0], Shape::Ring { .. }));
        assert!(icon(TrayState::Controlled).tile.is_some());
        let attention = icon(TrayState::Attention);
        assert!(attention.clear.is_some() && attention.dot.is_some());
    }

    #[test]
    fn the_svg_uses_fills_and_clips_only() {
        for state in TrayState::ALL {
            let svg = svg(state);
            assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"22\""));
            assert!(!svg.contains("stroke"), "{state:?}");
            assert!(!svg.contains("mask"), "{state:?}");
        }
        assert_eq!(svg(TrayState::Controlled).matches("<clipPath").count(), 4);
        assert_eq!(svg(TrayState::Attention).matches("<clipPath").count(), 1);
        assert_eq!(svg(TrayState::Normal).matches("<clipPath").count(), 0);
        // The left screen's outline, moved down to the middle.
        assert!(svg(TrayState::Normal).contains("M3.5 5.75H"));
    }

    #[test]
    fn names_follow_the_states() {
        assert_eq!(
            icon_name(TrayState::Attention),
            "cc.ctsite.inputweave-tray-attention-symbolic"
        );
    }
}
