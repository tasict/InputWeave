//! The tray icon's pictures, drawn from the shapes in
//! `inputweave_app_core::tray_icon`: symbolic SVGs written to an icon
//! theme folder of the app's own, and pixmaps drawn with cairo.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gtk::cairo;
use inputweave_app_core::status_text::TrayState;
use inputweave_app_core::tray_icon::{self, Circle, RoundRect, Shape};

/// Writes the symbolic icons where the panel finds them through
/// `IconThemePath`: in the hicolor layout, and flat for panels that look
/// in the folder itself. Returns the folder.
pub fn write_theme(dir: &Path) -> std::io::Result<PathBuf> {
    let status = dir.join("hicolor/scalable/status");
    std::fs::create_dir_all(&status)?;
    for state in TrayState::ALL {
        let svg = tray_icon::svg(state);
        let file = format!("{}.svg", tray_icon::icon_name(state));
        std::fs::write(status.join(&file), &svg)?;
        std::fs::write(dir.join(&file), &svg)?;
    }
    Ok(dir.to_owned())
}

fn round_rect(cr: &cairo::Context, r: RoundRect) {
    let (x, y, w, h, k) = (r.x, r.y, r.width, r.height, r.radius);
    use std::f64::consts::PI;
    cr.new_sub_path();
    cr.arc(x + w - k, y + k, k, -PI / 2.0, 0.0);
    cr.arc(x + w - k, y + h - k, k, 0.0, PI / 2.0);
    cr.arc(x + k, y + h - k, k, PI / 2.0, PI);
    cr.arc(x + k, y + k, k, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

fn circle(cr: &cairo::Context, c: Circle) {
    cr.new_sub_path();
    cr.arc(c.cx, c.cy, c.r, 0.0, 2.0 * std::f64::consts::PI);
    cr.close_path();
}

fn shape(cr: &cairo::Context, s: Shape) -> Result<(), cairo::Error> {
    match s {
        Shape::Fill(r) => {
            cr.set_fill_rule(cairo::FillRule::Winding);
            round_rect(cr, r);
        }
        Shape::Ring { outer, inner } => {
            cr.set_fill_rule(cairo::FillRule::EvenOdd);
            round_rect(cr, outer);
            round_rect(cr, inner);
        }
    }
    cr.fill()
}

/// Draws one state in white, in the drawing's coordinates scaled to `size`.
pub fn draw(cr: &cairo::Context, state: TrayState, size: f64) -> Result<(), cairo::Error> {
    let icon = tray_icon::icon(state);
    cr.save()?;
    let scale = size / tray_icon::SIZE;
    cr.scale(scale, scale);
    cr.translate(0.0, tray_icon::top());
    // Light ink: most panels that ignore the icon's name are dark.
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.push_group();
    if let Some(tile) = icon.tile {
        round_rect(cr, tile);
        cr.fill()?;
        cr.set_operator(cairo::Operator::DestOut);
    }
    for s in &icon.shapes {
        shape(cr, *s)?;
    }
    if let Some(clear) = icon.clear {
        cr.set_operator(cairo::Operator::DestOut);
        circle(cr, clear);
        cr.fill()?;
        cr.set_operator(cairo::Operator::Over);
    }
    if let Some(dot) = icon.dot {
        circle(cr, dot);
        cr.fill()?;
    }
    cr.pop_group_to_source()?;
    cr.paint()?;
    cr.restore()
}

/// One state at `size` pixels square, as SNI pixmaps are: ARGB32 in
/// network byte order, not premultiplied.
fn render(state: TrayState, size: i32) -> Result<(i32, i32, Vec<u8>), cairo::Error> {
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, size, size)?;
    {
        let cr = cairo::Context::new(&surface)?;
        draw(&cr, state, f64::from(size))?;
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.data().map_err(|_| cairo::Error::SurfaceFinished)?;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for row in 0..size as usize {
        for column in 0..size as usize {
            let at = row * stride + column * 4;
            let pixel = u32::from_ne_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
            let a = pixel >> 24;
            let unpremultiply = |c: u32| (c * 255 + a / 2).checked_div(a).unwrap_or(0);
            let (r, g, b) = (
                unpremultiply((pixel >> 16) & 0xff),
                unpremultiply((pixel >> 8) & 0xff),
                unpremultiply(pixel & 0xff),
            );
            out.extend_from_slice(&((a << 24) | (r << 16) | (g << 8) | b).to_be_bytes());
        }
    }
    Ok((size, size, out))
}

/// Every state at 22 and 44 pixels.
pub fn pixmaps() -> HashMap<TrayState, crate::tray::Pixmaps> {
    TrayState::ALL
        .into_iter()
        .map(|state| {
            let images = [22, 44]
                .into_iter()
                .filter_map(|size| render(state, size).ok())
                .collect();
            (state, images)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixmaps_are_square_argb() {
        let all = pixmaps();
        assert_eq!(all.len(), 5);
        for images in all.values() {
            assert_eq!(images.len(), 2);
            for (w, h, data) in images {
                assert_eq!(w, h);
                assert_eq!(data.len(), (w * h * 4) as usize);
            }
        }
        // The controlled tile is mostly ink; the normal icon mostly clear.
        let ink = |state| {
            let (_, _, data) = &all[&state][0];
            data.chunks(4).filter(|p| p[0] > 128).count()
        };
        assert!(ink(TrayState::Controlled) > ink(TrayState::Normal) * 2);
        assert!(ink(TrayState::Controlling) > ink(TrayState::Normal));
        assert!(ink(TrayState::Paused) < ink(TrayState::Normal));
    }

    #[test]
    fn the_theme_has_every_state() {
        let dir = tempfile::tempdir().unwrap();
        write_theme(dir.path()).unwrap();
        for state in TrayState::ALL {
            let name = format!("{}.svg", tray_icon::icon_name(state));
            assert!(
                dir.path()
                    .join("hicolor/scalable/status")
                    .join(&name)
                    .is_file()
            );
        }
    }
}
