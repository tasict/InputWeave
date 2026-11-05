//! The HUD, identifying the screens, finding the cursor and the edge glow:
//! what the other two apps show over
//! everything, without taking the focus or the clicks. A Wayland app cannot
//! put a window where it wants, so how depends on the session:
//! - X11: override-redirect windows of the app's own (`x11`), for the HUD,
//!   the cursor and the glow;
//! - Wayland with the layer shell (KDE Plasma, wlroots): layer surfaces
//!   for the HUD and the glow (`layer`); the cursor's place is not known
//!   there;
//! - GNOME on Wayland: the HUD as a notification; GNOME finds the cursor
//!   itself (Settings › Accessibility › Locate Pointer); no glow, which
//!   nothing could show in its place.
//!
//! Identifying the screens is done on X11 by the same kind of windows (a
//! frame of four and the name's card on each monitor); on Wayland with a
//! fullscreen window on each monitor, transparent and with an empty input
//! region, so clicks go through where the compositor honours it (it may
//! take the focus).

mod layer;
pub mod paint;
mod x11;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adw::prelude::*;
use gtk::{cairo, gdk, glib};
use inputweave_app_core::edge_glow::{self, Availability};
use inputweave_app_core::proto::CursorCrossed;
use inputweave_app_core::screen_identity::{self, Monitor};
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;
use inputweave_app_core::{hud, log, ui};

use crate::strings::tr;
use crate::style;
use crate::ui::Ui;

enum Backend {
    X11(x11::X11Overlays),
    Layer(layer::LayerOverlays),
    /// Notifications for the HUD, nothing for the cursor nor the glow:
    /// why there are no overlays.
    Plain(Availability),
}

pub struct Overlay {
    /// Notifications once an overlay thread is gone (it lost the server or
    /// the compositor), so no HUD note is dropped unseen.
    backend: RefCell<Backend>,
    identify: RefCell<Vec<gtk::Window>>,
}

/// How long the cursor shows grown.
const LOCATE_FOR: Duration = Duration::from_millis(1100);

/// Says an overlay thread ended, however it does: the GTK side then falls
/// back to notifications.
struct Ended(Arc<AtomicBool>);

impl Drop for Ended {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Overlay {
    pub fn new() -> Overlay {
        let kind = gdk::Display::default()
            .map(|d| d.type_().name().to_owned())
            .unwrap_or_default();
        let backend = match kind.as_str() {
            "GdkX11Display" => x11::X11Overlays::start()
                .map(Backend::X11)
                .unwrap_or(Backend::Plain(Availability::Unavailable)),
            "GdkWaylandDisplay" => match layer::LayerOverlays::start() {
                Ok(layer) => Backend::Layer(layer),
                Err(true) => Backend::Plain(Availability::NoLayerShell),
                Err(false) => Backend::Plain(Availability::Unavailable),
            },
            _ => Backend::Plain(Availability::Unavailable),
        };
        log::info(&format!(
            "overlays: {}",
            match backend {
                Backend::X11(_) => "X11 windows",
                Backend::Layer(_) => "the layer shell",
                Backend::Plain(_) => "notifications",
            }
        ));
        Overlay {
            backend: RefCell::new(backend),
            identify: RefCell::new(Vec::new()),
        }
    }

    /// Shows a note in the HUD.
    pub fn hud(&self, ui: &Rc<Ui>, text: &str) {
        let shown = match &*self.backend.borrow() {
            Backend::X11(x11) => x11.hud(text, style::brand(), monitor_areas(), hud::SHOWN_FOR),
            Backend::Layer(layer) => layer.hud(text, style::brand(), hud::SHOWN_FOR),
            Backend::Plain(_) => false,
        };
        if !shown {
            self.lost();
            crate::notify::hud(ui, text);
        }
    }

    /// Shows where the cursor is, where the session lets the app know.
    pub fn locate(&self) {
        let lost = matches!(&*self.backend.borrow(), Backend::X11(x11) if !x11.locate(LOCATE_FOR));
        if lost {
            self.lost();
        }
    }

    /// Whether the session lets the app light the screen's edges: not on
    /// a Wayland without the layer shell (GNOME's), nor once the overlay
    /// thread is gone or could not start.
    pub fn glows(&self) -> Availability {
        match &*self.backend.borrow() {
            Backend::Plain(why) => *why,
            _ => Availability::Shown,
        }
    }

    /// Lights the edge where the cursor crossed to or from another
    /// computer, on the monitor the background
    /// service names; nothing where the session has no way to.
    pub fn glow(&self, crossed: &CursorCrossed) {
        let Some(edge) = edge_glow::known(crossed) else {
            return;
        };
        let reduce_motion = gtk::Settings::default().is_some_and(|s| !s.is_gtk_enable_animations());
        let timing = edge_glow::timing(reduce_motion);
        let solid = adw::StyleManager::default().is_high_contrast();
        let color = edge_glow::rgb8(style::brand().thread);
        let spot = edge_glow::fraction(crossed);
        let sent = match &*self.backend.borrow() {
            Backend::X11(x11) => {
                // The background service's units on X11 are X's pixels.
                let areas = monitor_areas_named();
                let monitors: Vec<edge_glow::Monitor> = areas
                    .iter()
                    .map(|(id, a)| edge_glow::Monitor {
                        id: id.clone(),
                        x: f64::from(a.x),
                        y: f64::from(a.y),
                        width: f64::from(a.width),
                        height: f64::from(a.height),
                    })
                    .collect();
                let (monitor, area) = match edge_glow::monitor_for(crossed, &monitors) {
                    Some(i) => (areas[i].0.clone(), areas[i].1),
                    None => (
                        crossed.display_id.clone(),
                        x11::Area {
                            x: crossed.display_x.round() as i32,
                            y: crossed.display_y.round() as i32,
                            width: crossed.display_width.round() as i32,
                            height: crossed.display_height.round() as i32,
                            scale: 1,
                        },
                    ),
                };
                x11.glow(x11::Glow {
                    monitor,
                    area,
                    edge,
                    spot,
                    color,
                    solid,
                    timing,
                })
            }
            Backend::Layer(layer) => layer.glow(layer::Glow {
                crossed: crossed.clone(),
                edge,
                spot,
                color,
                solid,
                timing,
            }),
            Backend::Plain(_) => return,
        };
        if !sent {
            self.lost();
        }
    }

    /// The overlay thread is gone: notifications from now on, said once.
    fn lost(&self) {
        let mut backend = self.backend.borrow_mut();
        if !matches!(*backend, Backend::Plain(_)) {
            log::info("the overlays stopped; the HUD is shown as notifications from now on");
            *backend = Backend::Plain(Availability::Unavailable);
        }
    }

    /// Shows this computer's name on each of its screens, for as long as
    /// the service asked (`duration_ms`).
    pub fn identify(&self, ui: &Rc<Ui>, model: &Snapshot, duration_ms: u32) {
        for window in self.identify.borrow_mut().drain(..) {
            window.destroy();
        }
        let Some(display) = gdk::Display::default() else {
            return;
        };
        let monitors = monitors(&display);
        let arranged = screen_identity::this_computers_screens(model);
        let name = model.status.this_device_name.clone();
        let jump = screen_identity::jump(model);
        let shown = screen_identity::shown_for(duration_ms);
        // On X11 in windows of the app's own, which need no compositor and
        // take no focus; the overlay thread knows X's primary output. With
        // the thread gone, the windows below.
        let sent = match &*self.backend.borrow() {
            Backend::X11(x11) => Some(
                x11.identify(x11::Identify {
                    monitors: monitors
                        .iter()
                        .map(|m| (x11_area(m), core_monitor(m)))
                        .collect(),
                    arranged: arranged.to_vec(),
                    name: name.clone(),
                    jump: jump.clone(),
                    brand: style::brand(),
                    shown,
                }),
            ),
            _ => None,
        };
        match sent {
            Some(true) => return,
            Some(false) => self.lost(),
            None => {}
        }
        let core: Vec<Monitor> = monitors
            .iter()
            .map(|m| Monitor {
                primary: screen_identity::arranged_primary(arranged, &core_monitor(m).connector),
                ..core_monitor(m)
            })
            .collect();
        let labels = screen_identity::labels(arranged, &core);
        let mut windows = Vec::new();
        for (monitor, label) in monitors.iter().zip(labels) {
            let line = tr(&ui!(
                keys::SCREEN_NUMBERED,
                label.number.to_string(),
                label.size
            ));
            let window = identify_window(ui, &name, &line, jump.as_deref());
            // Its size too, for a session without a window manager to make
            // it fullscreen.
            let geometry = monitor.geometry();
            window.set_default_size(geometry.width(), geometry.height());
            window.fullscreen_on_monitor(monitor);
            window.present();
            // Clicks go through, where the compositor honours it.
            if let Some(surface) = window.surface() {
                surface.set_input_region(Some(&cairo::Region::create()));
            }
            windows.push(window);
        }
        *self.identify.borrow_mut() = windows;
        let gone = self
            .identify
            .borrow()
            .iter()
            .map(|w| w.downgrade())
            .collect::<Vec<_>>();
        glib::timeout_add_local_once(shown, move || {
            for window in gone.iter().filter_map(|w| w.upgrade()) {
                window.destroy();
            }
        });
    }
}

fn identify_window(ui: &Rc<Ui>, name: &str, line: &str, jump: Option<&str>) -> gtk::Window {
    let area = gtk::DrawingArea::new();
    let (name, line, jump) = (name.to_owned(), line.to_owned(), jump.map(str::to_owned));
    area.set_draw_func(move |_, cr, width, height| {
        if let Err(e) = paint::identify(
            cr,
            f64::from(width),
            f64::from(height),
            &name,
            &line,
            jump.as_deref(),
            &style::brand(),
        ) {
            log::info(&format!("cannot draw a screen's name: {e}"));
        }
    });
    gtk::Window::builder()
        .application(&ui.app)
        .decorated(false)
        .css_classes(["iw-identify"])
        .child(&area)
        .build()
}

fn monitors(display: &gdk::Display) -> Vec<gdk::Monitor> {
    let list = display.monitors();
    (0..list.n_items())
        .filter_map(|i| list.item(i).and_downcast::<gdk::Monitor>())
        .collect()
}

fn core_monitor(m: &gdk::Monitor) -> Monitor {
    let g = m.geometry();
    Monitor {
        connector: m.connector().map(|c| c.to_string()).unwrap_or_default(),
        x: g.x(),
        y: g.y(),
        width: g.width(),
        height: g.height(),
        scale: m.scale(),
        primary: false,
    }
}

/// A monitor in X's pixels, with GDK's scale for it.
fn x11_area(m: &gdk::Monitor) -> x11::Area {
    let g = m.geometry();
    let s = m.scale_factor();
    x11::Area {
        x: g.x() * s,
        y: g.y() * s,
        width: g.width() * s,
        height: g.height() * s,
        scale: s,
    }
}

/// A crossing of `edge` of the first monitor, a third of the way along,
/// for `--show-glow`.
pub fn crossing_for_tests(edge: inputweave_app_core::proto::ScreenEdge) -> Option<CursorCrossed> {
    let display = gdk::Display::default()?;
    let monitor = monitors(&display).into_iter().next()?;
    let a = x11_area(&monitor);
    let upright = edge_glow::upright(edge);
    Some(CursorCrossed {
        display_id: monitor
            .connector()
            .map(|c| c.to_string())
            .unwrap_or_default(),
        edge: edge as i32,
        along: if upright {
            f64::from(a.y) + f64::from(a.height) / 3.0
        } else {
            f64::from(a.x) + f64::from(a.width) / 3.0
        },
        display_x: f64::from(a.x),
        display_y: f64::from(a.y),
        display_width: f64::from(a.width),
        display_height: f64::from(a.height),
        ..Default::default()
    })
}

/// The monitors in X's pixels with their connectors, for the glow to
/// find the one the background service names.
fn monitor_areas_named() -> Vec<(String, x11::Area)> {
    let Some(display) = gdk::Display::default() else {
        return Vec::new();
    };
    monitors(&display)
        .iter()
        .map(|m| {
            (
                m.connector().map(|c| c.to_string()).unwrap_or_default(),
                x11_area(m),
            )
        })
        .collect()
}

/// The monitors in X's pixels, for the HUD to pick the one with the
/// pointer.
fn monitor_areas() -> Vec<x11::Area> {
    let Some(display) = gdk::Display::default() else {
        return Vec::new();
    };
    monitors(&display).iter().map(x11_area).collect()
}
