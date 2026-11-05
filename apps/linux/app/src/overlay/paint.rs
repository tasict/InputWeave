//! What the overlays look like, drawn with cairo so every way of showing
//! them shares it: the HUD's capsule of text, and a screen's name when the
//! screens are identified.

use gtk::{cairo, pango};

use crate::style::Brand;

/// A picture in cairo's ARGB32: premultiplied, native-endian, which is
/// Wayland's ARGB8888 and an X11 32-bit pixel on little-endian machines.
pub struct Picture {
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub data: Vec<u8>,
}

impl Picture {
    /// The pixel's alpha at (x, y).
    pub fn alpha(&self, x: i32, y: i32) -> u8 {
        let at = (y * self.stride + x * 4) as usize;
        let pixel = u32::from_ne_bytes([
            self.data[at],
            self.data[at + 1],
            self.data[at + 2],
            self.data[at + 3],
        ]);
        (pixel >> 24) as u8
    }
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
}

fn layout(cr: &cairo::Context, text: &str, points: f64, bold: bool) -> pango::Layout {
    let layout = pangocairo::functions::create_layout(cr);
    let mut font = pango::FontDescription::from_string("Sans");
    font.set_size((points * f64::from(pango::SCALE)) as i32);
    if bold {
        font.set_weight(pango::Weight::Semibold);
    }
    layout.set_font_description(Some(&font));
    layout.set_text(text);
    layout
}

fn ink(cr: &cairo::Context, brand: &Brand, alpha: f64) {
    if brand.dark {
        cr.set_source_rgba(1.0, 1.0, 1.0, alpha);
    } else {
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.85 * alpha);
    }
}

/// The HUD: `text` in a capsule of material at most 520 wide, its lines
/// wrapped and centered, with the tile edge around it; drawn `scale` times
/// as large for a monitor of that scale, the picture's size a whole
/// multiple of it.
pub fn hud(text: &str, brand: &Brand, scale: i32) -> Result<Picture, cairo::Error> {
    const MAX_TEXT: i32 = 520;
    const PAD_X: f64 = 20.0;
    const PAD_Y: f64 = 12.0;
    let scale = scale.max(1);
    let probe = cairo::ImageSurface::create(cairo::Format::ARgb32, 1, 1)?;
    let (tw, th) = {
        let cr = cairo::Context::new(&probe)?;
        let l = layout(&cr, text, 11.0, true);
        l.set_width(MAX_TEXT * pango::SCALE);
        l.set_wrap(pango::WrapMode::WordChar);
        l.set_alignment(pango::Alignment::Center);
        l.pixel_size()
    };
    let width = (f64::from(tw) + 2.0 * PAD_X).ceil();
    let height = (f64::from(th) + 2.0 * PAD_Y).ceil();
    let mut surface = cairo::ImageSurface::create(
        cairo::Format::ARgb32,
        width as i32 * scale,
        height as i32 * scale,
    )?;
    {
        let cr = cairo::Context::new(&surface)?;
        cr.scale(f64::from(scale), f64::from(scale));
        rounded(
            &cr,
            0.5,
            0.5,
            width - 1.0,
            height - 1.0,
            (height / 2.0).min(18.0),
        );
        if brand.dark {
            cr.set_source_rgba(0.14, 0.14, 0.16, 0.94);
        } else {
            cr.set_source_rgba(0.98, 0.98, 0.99, 0.96);
        }
        cr.fill_preserve()?;
        let e = brand.tile_edge;
        cr.set_source_rgb(e[0], e[1], e[2]);
        cr.set_line_width(1.0);
        cr.stroke()?;
        let l = layout(&cr, text, 11.0, true);
        l.set_width(MAX_TEXT * pango::SCALE);
        l.set_wrap(pango::WrapMode::WordChar);
        l.set_alignment(pango::Alignment::Center);
        // The lines are centered within MAX_TEXT: move the block to the
        // capsule by where it starts.
        let (_, logical) = l.pixel_extents();
        ink(&cr, brand, 1.0);
        cr.move_to(PAD_X - f64::from(logical.x()), PAD_Y);
        pangocairo::functions::show_layout(&cr, &l);
    }
    surface.flush();
    let (w, h, stride) = (surface.width(), surface.height(), surface.stride());
    let data = surface
        .data()
        .map_err(|_| cairo::Error::SurfaceFinished)?
        .to_vec();
    Ok(Picture {
        width: w,
        height: h,
        stride,
        data,
    })
}

/// The layouts of a screen's name card and its size.
struct Card {
    title: pango::Layout,
    detail: pango::Layout,
    shortcut: Option<pango::Layout>,
    width: f64,
    height: f64,
}

const CARD_GAP: f64 = 8.0;

fn card(cr: &cairo::Context, name: &str, line: &str, jump: Option<&str>) -> Card {
    let title = layout(cr, name, 28.0, true);
    let detail = layout(cr, line, 14.0, false);
    let shortcut = jump.map(|j| layout(cr, j, 13.0, false));
    let (w1, h1) = title.pixel_size();
    let (w2, h2) = detail.pixel_size();
    let (w3, h3) = shortcut.as_ref().map_or((0, 0), |l| l.pixel_size());
    let width = (f64::from(w1.max(w2).max(w3)) + 64.0).ceil();
    let gaps = if shortcut.is_some() {
        2.0 * CARD_GAP
    } else {
        CARD_GAP
    };
    let height = (f64::from(h1 + h2 + h3) + 48.0 + gaps).ceil();
    Card {
        title,
        detail,
        shortcut,
        width,
        height,
    }
}

/// The card with this computer's name, which screen it is ("Screen 1 ·
/// 2560 × 1440") and the shortcut that jumps here, at (x, y).
fn draw_card(
    cr: &cairo::Context,
    card: &Card,
    x: f64,
    y: f64,
    brand: &Brand,
) -> Result<(), cairo::Error> {
    let t = brand.thread;
    rounded(
        cr,
        x + 1.0,
        y + 1.0,
        card.width - 2.0,
        card.height - 2.0,
        16.0,
    );
    if brand.dark {
        cr.set_source_rgba(0.14, 0.14, 0.16, 0.96);
    } else {
        cr.set_source_rgba(0.98, 0.98, 0.99, 0.97);
    }
    cr.fill_preserve()?;
    cr.set_source_rgb(t[0], t[1], t[2]);
    cr.set_line_width(2.0);
    cr.stroke()?;
    let mut top = y + 24.0;
    for (layout, alpha) in [
        (Some(&card.title), 1.0),
        (Some(&card.detail), 0.75),
        (card.shortcut.as_ref(), 0.6),
    ] {
        let Some(layout) = layout else { continue };
        let (w, h) = layout.pixel_size();
        ink(cr, brand, alpha);
        cr.move_to(x + (card.width - f64::from(w)) / 2.0, top);
        pangocairo::functions::show_layout(cr, layout);
        top += f64::from(h) + CARD_GAP;
    }
    Ok(())
}

/// A screen's name over the whole screen, `width` by `height`: a frame in
/// the thread color along its edges, 8 thick, and the name's card in the
/// middle.
pub fn identify(
    cr: &cairo::Context,
    width: f64,
    height: f64,
    name: &str,
    line: &str,
    jump: Option<&str>,
    brand: &Brand,
) -> Result<(), cairo::Error> {
    cr.set_operator(cairo::Operator::Source);
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
    cr.paint()?;
    cr.set_operator(cairo::Operator::Over);
    let t = brand.thread;
    cr.set_source_rgb(t[0], t[1], t[2]);
    cr.set_line_width(FRAME);
    cr.rectangle(FRAME / 2.0, FRAME / 2.0, width - FRAME, height - FRAME);
    cr.stroke()?;
    let card = card(cr, name, line, jump);
    draw_card(
        cr,
        &card,
        (width - card.width) / 2.0,
        (height - card.height) / 2.0,
        brand,
    )
}

/// How thick the frame along a screen's edges is.
pub const FRAME: f64 = 8.0;

/// The name's card alone, drawn `scale` times as large, for a window of
/// its own.
pub fn identify_card(
    name: &str,
    line: &str,
    jump: Option<&str>,
    brand: &Brand,
    scale: i32,
) -> Result<Picture, cairo::Error> {
    let scale = scale.max(1);
    let probe = cairo::ImageSurface::create(cairo::Format::ARgb32, 1, 1)?;
    let (width, height) = {
        let cr = cairo::Context::new(&probe)?;
        let c = card(&cr, name, line, jump);
        (c.width, c.height)
    };
    let mut surface = cairo::ImageSurface::create(
        cairo::Format::ARgb32,
        width as i32 * scale,
        height as i32 * scale,
    )?;
    {
        let cr = cairo::Context::new(&surface)?;
        cr.scale(f64::from(scale), f64::from(scale));
        let c = card(&cr, name, line, jump);
        draw_card(&cr, &c, 0.0, 0.0, brand)?;
    }
    surface.flush();
    let (w, h, stride) = (surface.width(), surface.height(), surface.stride());
    let data = surface
        .data()
        .map_err(|_| cairo::Error::SurfaceFinished)?
        .to_vec();
    Ok(Picture {
        width: w,
        height: h,
        stride,
        data,
    })
}
