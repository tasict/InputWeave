//! The woven "Licensed" label: a label sewn into a
//! seam, the screen tile's white and edge with a running stitch and the text
//! in the thread's color. When a key is activated here its stitches sew
//! themselves around the edge once, in half a second; with animations off they are
//! there at once. With high contrast the stitch is a 2 pt solid line.

use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{cairo, pango};

use crate::style;

/// How long the stitches take to go around.
const SEW_MS: u32 = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// The settings sidebar's row.
    Small,
    /// The License page.
    Large,
}

impl Size {
    fn height(self) -> i32 {
        match self {
            Size::Small => 20,
            Size::Large => 44,
        }
    }

    fn font_px(self) -> f64 {
        match self {
            Size::Small => 11.0,
            Size::Large => 17.0,
        }
    }

    fn padding(self) -> f64 {
        match self {
            Size::Small => 8.0,
            Size::Large => 20.0,
        }
    }

    fn inset(self) -> f64 {
        match self {
            Size::Small => 2.5,
            Size::Large => 4.0,
        }
    }

    fn dash(self) -> [f64; 2] {
        match self {
            Size::Small => [2.5, 2.0],
            Size::Large => [4.0, 3.0],
        }
    }
}

pub struct Woven {
    size: Size,
    area: gtk::DrawingArea,
    text: Rc<RefCell<String>>,
    /// 0 to 1: how far around the stitches are.
    sewn: Rc<Cell<f64>>,
    animation: RefCell<Option<adw::TimedAnimation>>,
}

impl Woven {
    pub fn new(size: Size, text: &str) -> Woven {
        let area = gtk::DrawingArea::builder()
            .content_height(size.height())
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .accessible_role(gtk::AccessibleRole::Img)
            .build();
        let text = Rc::new(RefCell::new(text.to_owned()));
        let sewn = Rc::new(Cell::new(1.0));
        {
            let text = text.clone();
            let sewn = sewn.clone();
            area.set_draw_func(move |area, cr, width, height| {
                let _ = draw(area, cr, size, &text.borrow(), sewn.get(), width, height);
            });
        }
        let woven = Woven {
            size,
            area,
            text,
            sewn,
            animation: RefCell::new(None),
        };
        woven.fit();
        // Redrawn in the other appearance's colors, and measured again in
        // its weight, for as long as the label is on screen somewhere.
        let manager = adw::StyleManager::default();
        let weak = woven.area.downgrade();
        let redraw = move |_: &adw::StyleManager| {
            if let Some(area) = weak.upgrade() {
                area.queue_draw();
            }
        };
        let weak = woven.area.downgrade();
        let text = woven.text.clone();
        let remeasure = move |_: &adw::StyleManager| {
            if let Some(area) = weak.upgrade() {
                area.set_content_width(measure(&area, size, &text.borrow()));
                area.queue_draw();
            }
        };
        let handlers = RefCell::new(Some([
            manager.connect_dark_notify(redraw),
            manager.connect_high_contrast_notify(remeasure),
        ]));
        woven.area.connect_destroy(move |_| {
            if let Some(ids) = handlers.borrow_mut().take() {
                let manager = adw::StyleManager::default();
                for id in ids {
                    manager.disconnect(id);
                }
            }
        });
        woven
    }

    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    pub fn set_text(&self, text: String) {
        if *self.text.borrow() == text {
            return;
        }
        *self.text.borrow_mut() = text;
        self.fit();
    }

    /// Sizes the label for its text, and names it for assistive
    /// technologies.
    fn fit(&self) {
        let text = self.text.borrow();
        self.area
            .set_content_width(measure(&self.area, self.size, &text));
        self.area
            .update_property(&[gtk::accessible::Property::Label(&text)]);
        self.area.queue_draw();
    }

    /// Sews the stitches around once, as when a key was just activated.
    pub fn sew(&self) {
        let sewn = self.sewn.clone();
        let area = self.area.clone();
        let target = adw::CallbackAnimationTarget::new(move |value| {
            sewn.set(value);
            area.queue_draw();
        });
        let animation = adw::TimedAnimation::new(&self.area, 0.0, 1.0, SEW_MS, target);
        animation.set_easing(adw::Easing::EaseOutCubic);
        // Off when the desktop turns animations off: it goes straight to 1.
        animation.play();
        *self.animation.borrow_mut() = Some(animation);
    }
}

/// The width the label needs, in the weight [`draw`] uses now.
fn measure(widget: &impl IsA<gtk::Widget>, size: Size, text: &str) -> i32 {
    let layout = widget.create_pango_layout(Some(text));
    let contrast = adw::StyleManager::default().is_high_contrast();
    layout.set_font_description(Some(&font(size, contrast)));
    let (_, logical) = layout.pixel_extents();
    logical.width() + (2.0 * size.padding()).round() as i32 + 2
}

/// Semibold, or bold with high contrast.
fn font(size: Size, contrast: bool) -> pango::FontDescription {
    let mut description = pango::FontDescription::new();
    description.set_weight(if contrast {
        pango::Weight::Bold
    } else {
        pango::Weight::Semibold
    });
    description.set_absolute_size(size.font_px() * f64::from(pango::SCALE));
    description
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 1.5 * PI);
    cr.close_path();
}

fn draw(
    area: &gtk::DrawingArea,
    cr: &cairo::Context,
    size: Size,
    text: &str,
    sewn: f64,
    width: i32,
    height: i32,
) -> Result<(), cairo::Error> {
    let brand = style::brand();
    let contrast = adw::StyleManager::default().is_high_contrast();
    let (w, h) = (f64::from(width), f64::from(height));
    let set = |c: [f64; 3]| cr.set_source_rgb(c[0], c[1], c[2]);
    // The label itself: the tile's fill and edge.
    rounded(
        cr,
        0.5,
        0.5,
        w - 1.0,
        h - 1.0,
        if size == Size::Small { 3.0 } else { 5.0 },
    );
    set(brand.tile);
    cr.fill_preserve()?;
    let edge = if contrast && !brand.dark {
        [
            0x6E as f64 / 255.0,
            0x6E as f64 / 255.0,
            0x73 as f64 / 255.0,
        ]
    } else {
        brand.tile_edge
    };
    set(edge);
    cr.set_line_width(1.0);
    cr.stroke()?;
    let thread = if contrast && !brand.dark {
        [
            0x6A as f64 / 255.0,
            0x22 as f64 / 255.0,
            0xB0 as f64 / 255.0,
        ]
    } else {
        brand.thread
    };
    // The stitches, sewn as far as `sewn` around from the top.
    if sewn > 0.0 {
        cr.save()?;
        if sewn < 1.0 {
            let (cx, cy) = (w / 2.0, h / 2.0);
            let reach = w.hypot(h);
            cr.move_to(cx, cy);
            cr.arc(cx, cy, reach, -PI / 2.0, -PI / 2.0 + sewn * 2.0 * PI);
            cr.close_path();
            cr.clip();
        }
        let inset = size.inset();
        rounded(cr, inset, inset, w - 2.0 * inset, h - 2.0 * inset, 1.5);
        set(thread);
        if contrast {
            cr.set_line_width(2.0);
        } else {
            cr.set_line_width(if size == Size::Small { 1.0 } else { 1.4 });
            cr.set_dash(&size.dash(), 0.0);
        }
        cr.stroke()?;
        cr.restore()?;
    }
    // The word.
    let layout = area.create_pango_layout(Some(text));
    layout.set_font_description(Some(&font(size, contrast)));
    let (_, logical) = layout.pixel_extents();
    set(thread);
    cr.move_to(
        (w - f64::from(logical.width())) / 2.0,
        (h - f64::from(logical.height())) / 2.0,
    );
    pangocairo::functions::show_layout(cr, &layout);
    Ok(())
}
