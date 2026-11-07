//! `--screenshot <folder>`: draws every page, the Add Device dialog and the
//! pairing code window with sample data into PNG files, then quits, as the
//! macOS app's snapshot tests do (`apps/linux/scripts/make-screenshots.sh`).

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use adw::prelude::*;
use inputweave_app_core::proto::WakeState;
use inputweave_app_core::service::ServiceState;
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::status_text::TrayState;
use inputweave_app_core::text::Language;
use inputweave_app_core::{log, sample, tray_icon};

use inputweave_app_core::setup::Step;
use inputweave_app_core::shortcut::Row;
use inputweave_app_core::text::keys;
use inputweave_app_core::ui;

use crate::overlay::paint;
use crate::pages::shortcuts::Spot;
use crate::pairing::{AddDevice, CodeWindow};
use crate::strings::tr;
use crate::ui::Ui;
use crate::window::Page;
use crate::{strings, style, tray_art};

/// Long enough for a page to be laid out and drawn.
const SETTLE: Duration = Duration::from_millis(600);

fn save(widget: &impl IsA<gtk::Widget>, path: &Path) -> Result<(), String> {
    let (width, height) = (widget.width(), widget.height());
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    // A part of a window is drawn on the window's background, which it
    // does not draw itself.
    if !widget.is::<gtk::Native>() {
        let shade = if adw::StyleManager::default().is_dark() {
            0x24 as f32 / 255.0
        } else {
            0xFA as f32 / 255.0
        };
        snapshot.append_color(
            &gtk::gdk::RGBA::new(shade, shade, shade, 1.0),
            &gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32),
        );
    }
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot.to_node().ok_or("nothing was drawn")?;
    let renderer = widget
        .native()
        .and_then(|n| n.renderer())
        .ok_or("no renderer")?;
    let bounds = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    let texture = renderer.render_texture(&node, Some(&bounds));
    texture.save_to_png(path).map_err(|e| e.to_string())
}

/// The tray icon in every state: the symbolic SVGs as the system's SVG
/// loader draws them (dark, as a light panel tints them) on top, the
/// pixmaps below on a dark panel.
fn tray_sheet(path: &Path) -> Result<(), String> {
    use gtk::cairo;
    use gtk::gdk::prelude::GdkCairoContextExt;
    use gtk::{gdk_pixbuf, gio, glib};
    const SIZE: i32 = 64;
    const GAP: i32 = 16;
    let columns = TrayState::ALL.len() as i32;
    let width = columns * SIZE + (columns + 1) * GAP;
    let height = 2 * SIZE + 3 * GAP;
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height)
        .map_err(|e| e.to_string())?;
    let cr = cairo::Context::new(&surface).map_err(|e| e.to_string())?;
    let half = f64::from(height) / 2.0;
    cr.set_source_rgb(0.94, 0.94, 0.94);
    cr.rectangle(0.0, 0.0, f64::from(width), half);
    cr.fill().map_err(|e| e.to_string())?;
    cr.set_source_rgb(0.12, 0.12, 0.13);
    cr.rectangle(0.0, half, f64::from(width), half);
    cr.fill().map_err(|e| e.to_string())?;
    for (i, state) in TrayState::ALL.into_iter().enumerate() {
        let x = f64::from(GAP + i as i32 * (SIZE + GAP));
        let svg = glib::Bytes::from_owned(tray_icon::svg(state).into_bytes());
        let stream = gio::MemoryInputStream::from_bytes(&svg);
        let pixbuf = gdk_pixbuf::Pixbuf::from_stream_at_scale(
            &stream,
            SIZE,
            SIZE,
            true,
            gio::Cancellable::NONE,
        )
        .map_err(|e| e.to_string())?;
        cr.set_source_pixbuf(&pixbuf, x, f64::from(GAP));
        cr.paint().map_err(|e| e.to_string())?;
        cr.save().map_err(|e| e.to_string())?;
        cr.translate(x, f64::from(2 * GAP + SIZE));
        tray_art::draw(&cr, state, f64::from(SIZE)).map_err(|e| e.to_string())?;
        cr.restore().map_err(|e| e.to_string())?;
    }
    drop(cr);
    let mut surface = surface;
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.data().map_err(|e| e.to_string())?.to_vec();
    // cairo's ARGB32 is native-endian: BGRA in memory here.
    let format = if cfg!(target_endian = "little") {
        gtk::gdk::MemoryFormat::B8g8r8a8Premultiplied
    } else {
        gtk::gdk::MemoryFormat::A8r8g8b8Premultiplied
    };
    let texture = gtk::gdk::MemoryTexture::new(
        width,
        height,
        format,
        &glib::Bytes::from_owned(data),
        stride,
    );
    texture.save_to_png(path).map_err(|e| e.to_string())
}

/// A made-up desktop picture: a diagonal gradient between two colors, as
/// JPEG, the way the agent sends them.
fn sample_picture(width: i32, height: i32, from: [u8; 3], to: [u8; 3]) -> Vec<u8> {
    use gtk::gdk_pixbuf::{Colorspace, Pixbuf};
    let Some(pixbuf) = Pixbuf::new(Colorspace::Rgb, false, 8, width, height) else {
        return Vec::new();
    };
    for y in 0..height {
        for x in 0..width {
            let t = (f64::from(x) / f64::from(width) + f64::from(y) / f64::from(height)) / 2.0;
            let mix = |a: u8, b: u8| (f64::from(a) + (f64::from(b) - f64::from(a)) * t) as u8;
            pixbuf.put_pixel(
                x as u32,
                y as u32,
                mix(from[0], to[0]),
                mix(from[1], to[1]),
                mix(from[2], to[2]),
                255,
            );
        }
    }
    pixbuf
        .save_to_bufferv("jpeg", &[("quality", "80")])
        .unwrap_or_default()
}

/// The sample state with desktop pictures on every screen.
fn with_pictures(mut model: Snapshot) -> Snapshot {
    let colors = [
        ([30, 90, 170], [120, 200, 230]),
        ([200, 120, 60], [250, 210, 120]),
        ([40, 120, 80], [160, 220, 150]),
        ([110, 60, 150], [220, 160, 220]),
    ];
    let mut arrangement = (*model.arrangement).clone();
    let mut n = 0;
    for device in &mut arrangement.devices {
        for screen in &mut device.screens {
            let (from, to) = colors[n % colors.len()];
            let (w, h) = if screen.width >= screen.height {
                (320, (320.0 * screen.height / screen.width) as i32)
            } else {
                ((320.0 * screen.width / screen.height) as i32, 320)
            };
            screen.preview_jpeg = sample_picture(w.max(1), h.max(1), from, to);
            n += 1;
        }
    }
    model.arrangement = std::sync::Arc::new(arrangement);
    model
}

fn save_picture(picture: &paint::Picture, path: &Path) -> Result<(), String> {
    let format = if cfg!(target_endian = "little") {
        gtk::gdk::MemoryFormat::B8g8r8a8Premultiplied
    } else {
        gtk::gdk::MemoryFormat::A8r8g8b8Premultiplied
    };
    let texture = gtk::gdk::MemoryTexture::new(
        picture.width,
        picture.height,
        format,
        &gtk::glib::Bytes::from(&picture.data),
        picture.stride as usize,
    );
    texture.save_to_png(path).map_err(|e| e.to_string())
}

fn identify_picture(path: &Path) -> Result<(), String> {
    use gtk::cairo;
    let (w, h) = (1280, 720);
    let mut surface =
        cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).map_err(|e| e.to_string())?;
    {
        let cr = cairo::Context::new(&surface).map_err(|e| e.to_string())?;
        // A desktop to see it over.
        let g = cairo::LinearGradient::new(0.0, 0.0, f64::from(w), f64::from(h));
        g.add_color_stop_rgb(0.0, 0.2, 0.35, 0.6);
        g.add_color_stop_rgb(1.0, 0.5, 0.75, 0.85);
        cr.set_source(&g).map_err(|e| e.to_string())?;
        cr.paint().map_err(|e| e.to_string())?;
        cr.push_group();
        paint::identify(
            &cr,
            f64::from(w),
            f64::from(h),
            "ubuntu-desk",
            &tr(&ui!(keys::SCREEN_NUMBERED, "1", "2560 × 1440")),
            Some("Ctrl+Alt+Shift+2"),
            &style::brand(),
        )
        .map_err(|e| e.to_string())?;
        cr.pop_group_to_source().map_err(|e| e.to_string())?;
        cr.paint().map_err(|e| e.to_string())?;
    }
    surface.flush();
    let picture = paint::Picture {
        width: w,
        height: h,
        stride: surface.stride(),
        data: surface.data().map_err(|e| e.to_string())?.to_vec(),
    };
    save_picture(&picture, path)
}

/// The glow of a monitor's right edge, as the overlays draw it (the
/// compositor blending it over the desktop), crossed a third of the way
/// down; and, beside it, the solid line of high contrast or of a desktop
/// with no compositor.
fn glow_picture(path: &Path) -> Result<(), String> {
    use gtk::cairo;
    use inputweave_app_core::edge_glow;
    use inputweave_app_core::proto::ScreenEdge;
    let (w, h) = (960, 540);
    let mut surface =
        cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).map_err(|e| e.to_string())?;
    {
        let cr = cairo::Context::new(&surface).map_err(|e| e.to_string())?;
        let g = cairo::LinearGradient::new(0.0, 0.0, f64::from(w), f64::from(h));
        g.add_color_stop_rgb(0.0, 0.2, 0.35, 0.6);
        g.add_color_stop_rgb(1.0, 0.5, 0.75, 0.85);
        cr.set_source(&g).map_err(|e| e.to_string())?;
        cr.paint().map_err(|e| e.to_string())?;
        let color = edge_glow::rgb8(style::brand().thread);
        let half = w / 2;
        let strip = |edge, along: i32, deep: i32, spot, solid| {
            let pixels =
                edge_glow::pixels(edge, along as usize, deep as usize, spot, color, 1.0, solid);
            let (sw, sh) = edge_glow::along_deep(edge, along, deep);
            cairo::ImageSurface::create_for_data(pixels, cairo::Format::ARgb32, sw, sh, sw * 4)
        };
        // Blended: the right edge of the left half.
        let deep = edge_glow::DEPTH as i32;
        let glow =
            strip(ScreenEdge::Right, h, deep, 1.0 / 3.0, false).map_err(|e| e.to_string())?;
        cr.set_source_surface(&glow, f64::from(half - deep), 0.0)
            .map_err(|e| e.to_string())?;
        cr.paint().map_err(|e| e.to_string())?;
        // Solid: the right edge of the whole.
        let deep = edge_glow::SOLID_DEPTH as i32;
        let line = strip(ScreenEdge::Right, h, deep, 1.0 / 3.0, true).map_err(|e| e.to_string())?;
        cr.set_source_surface(&line, f64::from(w - deep), 0.0)
            .map_err(|e| e.to_string())?;
        cr.paint().map_err(|e| e.to_string())?;
    }
    surface.flush();
    let picture = paint::Picture {
        width: w,
        height: h,
        stride: surface.stride(),
        data: surface.data().map_err(|e| e.to_string())?.to_vec(),
    };
    save_picture(&picture, path)
}

/// Scrolls a preferences page to its end.
fn scroll_to_end(page: &adw::PreferencesPage) {
    let mut child = page.first_child();
    while let Some(widget) = child {
        if let Some(scrolled) = widget.downcast_ref::<gtk::ScrolledWindow>() {
            let adjustment = scrolled.vadjustment();
            adjustment.set_value(adjustment.upper());
            return;
        }
        child = widget.first_child();
    }
}

const SAMPLE_KEY: &str = "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42";

fn with_license(model: &Snapshot, license: inputweave_app_core::proto::License) -> Snapshot {
    let mut model = model.clone();
    model.status.license = Some(license);
    model
}

fn license_in(
    state: inputweave_app_core::proto::LicenseState,
) -> inputweave_app_core::proto::License {
    inputweave_app_core::proto::License {
        key: SAMPLE_KEY.into(),
        state: state as i32,
        ..Default::default()
    }
}

/// The sample, as the Wayland part of the run had it.
fn wayland_again(sample: &Snapshot) -> Snapshot {
    let mut wayland = sample::wayland(SystemTime::now());
    wayland.arrangement = sample.arrangement.clone();
    wayland
}

async fn license_shots(
    ui: &Rc<Ui>,
    window: &Rc<crate::window::MainWindow>,
    shot: &impl Fn(&str, &gtk::Widget),
) {
    use inputweave_app_core::proto::{LicenseState, LicensedDevice};
    let base = (*ui.snapshot()).clone();
    let show = |model: Snapshot| {
        ui.agent.show_fixed(model);
        window.update(&ui.snapshot(), ServiceState::Running);
    };
    window.show_page(Page::License);
    for (name, state) in [
        ("license-full", LicenseState::SeatsFull),
        ("license-invalid", LicenseState::Invalid),
        ("license-unavailable", LicenseState::Unavailable),
    ] {
        show(with_license(&base, license_in(state)));
        gtk::glib::timeout_future(SETTLE).await;
        shot(name, &window.widget());
    }
    let licensed = inputweave_app_core::proto::License {
        activation_id: "6b1e3f9a-2c4d-4e8f-9a1b-3c5d7e9f1a2b".into(),
        customer_email: "someone@example.com".into(),
        validated_at: 1_793_800_000,
        source_device_id: "mac".into(),
        source_device_name: "MacBook Pro".into(),
        devices: vec![
            LicensedDevice {
                id: "mac".into(),
                name: "MacBook Pro".into(),
                licensed: true,
            },
            LicensedDevice {
                id: "pc".into(),
                name: "Windows Desktop".into(),
                licensed: true,
            },
        ],
        ..license_in(LicenseState::Licensed)
    };
    show(with_license(&base, licensed.clone()));
    gtk::glib::timeout_future(SETTLE).await;
    shot("license-licensed", &window.widget());
    let question = crate::pages::license::remove_question_for_screenshot(&window.widget());
    gtk::glib::timeout_future(SETTLE).await;
    shot("license-remove", &window.widget());
    question.close();
    let about = ui.show_about();
    gtk::glib::timeout_future(SETTLE).await;
    shot("about-licensed", &window.widget());
    about.close();

    // Remove License: its first try, after the agent did not take it, and
    // the seat waiting for Polar once it is done.
    {
        use inputweave_app_core::license::{Done, Keeper};
        use inputweave_app_core::polar::{Deactivation, Trouble};
        let now = std::time::SystemTime::now();
        let l = Some(&licensed);
        let mut removing = Keeper::default();
        removing.remove(l);
        let mut failing = Keeper::default();
        failing.remove(None);
        failing.next(None, "", false, true, now);
        failing.removed(Err(None), now);
        let mut waiting = Keeper::default();
        waiting.remove(l);
        waiting.next(l, "", false, true, now);
        waiting.removed(Ok(()), now);
        waiting.next(None, "", false, true, now);
        waiting.finished(
            Done::Deactivated(Deactivation::Trouble(Trouble::Unreachable(String::new()))),
            now,
        );
        for (name, keeper, model) in [
            (
                "license-removing",
                removing,
                with_license(&base, licensed.clone()),
            ),
            (
                "license-remove-failed",
                failing,
                with_license(&base, licensed.clone()),
            ),
            ("license-seat-waiting", waiting, base.clone()),
        ] {
            ui.license.replace_keeper(keeper);
            show(model);
            gtk::glib::timeout_future(SETTLE).await;
            shot(name, &window.widget());
        }
        ui.license.replace_keeper(Keeper::default());
    }

    // Trial, Enter License Key and its states, About.
    show(base.clone());
    gtk::glib::timeout_future(SETTLE).await;
    shot("license-trial", &window.widget());
    let about = ui.show_about();
    gtk::glib::timeout_future(SETTLE).await;
    shot("about-trial", &window.widget());
    about.close();
    let dialog = window.license().dialog_for_screenshot(ui);
    gtk::glib::timeout_future(SETTLE).await;
    shot("license-enter", &window.widget());
    dialog.show_key(SAMPLE_KEY);
    dialog.set_busy(true);
    gtk::glib::timeout_future(SETTLE).await;
    shot("license-activating", &window.widget());
    dialog.set_busy(false);
    for (name, key, manage) in [
        ("license-error-offline", keys::LICENSE_ERR_OFFLINE, false),
        ("license-error-full", keys::LICENSE_ERR_FULL, true),
        ("license-error-invalid", keys::LICENSE_ERR_INVALID, false),
    ] {
        dialog.show_error(&strings::t(key), manage);
        gtk::glib::timeout_future(SETTLE).await;
        shot(name, &window.widget());
    }
    dialog.dialog.close();
    show(base);
}

pub fn run(ui: Rc<Ui>, dir: PathBuf) {
    let language = match strings::language() {
        Language::English => "en",
        Language::ChineseTaiwan => "zh-TW",
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("inputweave-app: cannot make {}: {e}", dir.display());
        ui.app.quit();
        return;
    }
    let app = ui.app.clone();
    let hold = app.hold();
    gtk::glib::spawn_future_local(async move {
        let shot = |name: &str, widget: &gtk::Widget| {
            let path = dir.join(format!("{language}-{name}.png"));
            match save(widget, &path) {
                Ok(()) => log::info(&format!("wrote {}", path.display())),
                Err(e) => eprintln!("inputweave-app: {}: {e}", path.display()),
            }
        };
        let sheet = dir.join(format!("{language}-tray-icons.png"));
        if let Err(e) = tray_sheet(&sheet) {
            eprintln!("inputweave-app: {}: {e}", sheet.display());
        }
        ui.agent.show_fixed(with_pictures((*ui.snapshot()).clone()));
        let window = ui.window();
        // Tall enough for the longest pages.
        window.window().set_default_size(1100, 980);
        window.present(&ui.snapshot(), ServiceState::Running);
        gtk::glib::timeout_future(SETTLE).await;
        for page in Page::ALL {
            window.show_page(page);
            gtk::glib::timeout_future(SETTLE).await;
            shot(page.name(), &window.widget());
        }

        // Waking: the ThinkPad asleep, being woken and not woken; one that
        // never said how; the Mac mini asleep on the arrangement.
        let base = (*ui.snapshot()).clone();
        let asleep = |wake: WakeState| {
            let mut model = base.clone();
            for device in &mut model.status.devices {
                if device.id == "laptop" {
                    device.wake = wake as i32;
                }
            }
            model
        };
        window.show_page(Page::Devices);
        window.devices().select("laptop");
        for (name, model) in [
            ("devices-wake", asleep(WakeState::None)),
            ("devices-waking", asleep(WakeState::Waking)),
            ("devices-not-woken", asleep(WakeState::NotWoken)),
            (
                "devices-wake-unknown",
                with_pictures(sample::asleep(SystemTime::now(), WakeState::None)),
            ),
        ] {
            ui.agent.show_fixed(model);
            window.update(&ui.snapshot(), ServiceState::Running);
            gtk::glib::timeout_future(SETTLE).await;
            shot(name, &window.widget());
        }
        ui.agent.show_fixed(with_pictures(sample::asleep(
            SystemTime::now(),
            WakeState::NotWoken,
        )));
        window.show_page(Page::Arrangement);
        window.arrangement().select("mac");
        window.update(&ui.snapshot(), ServiceState::Running);
        gtk::glib::timeout_future(SETTLE).await;
        shot("arrangement-asleep", &window.widget());
        window.devices().select("mac");
        ui.agent.show_fixed(base);
        window.update(&ui.snapshot(), ServiceState::Running);

        // Shortcuts while recording, and after the service refused one.
        window.show_page(Page::General);
        let shortcuts = window.general().shortcuts().clone();
        shortcuts.record_row(Row::Lock);
        gtk::glib::timeout_future(SETTLE).await;
        shot("shortcuts-recording", &shortcuts.widget().clone().upcast());
        shortcuts.stop_recording();
        let before = inputweave_app_core::shortcut::defaults();
        let after = inputweave_app_core::shortcut::with(&before, Row::Lock, &before.take_back);
        shortcuts.show_error(
            Spot::Row(Row::Lock),
            &tr(&inputweave_app_core::shortcut::refusal(
                "duplicate",
                Some(Row::Lock),
                &after,
                &before,
            )),
        );
        gtk::glib::timeout_future(SETTLE).await;
        shot("shortcuts-refused", &shortcuts.widget().clone().upcast());

        // HUD notes and a screen's name.
        let brand = style::brand();
        for (name, text) in [
            (
                "hud",
                tr(&ui!(keys::HUD_TAKE_BACK_HINT, "Ctrl+Alt+Shift+Esc")),
            ),
            (
                "hud-two-lines",
                tr(&ui!(
                    keys::HUD_TWO_LINES,
                    ui!(keys::HUD_TAKE_BACK_HINT, "Ctrl+Alt+Shift+Esc"),
                    ui!(keys::HUD_RELATIVE_ENTERED, "Mac mini", "Ctrl+Alt+Shift+G")
                )),
            ),
            ("hud-waking", tr(&ui!(keys::HUD_WAKING, "Mac mini"))),
        ] {
            let path = dir.join(format!("{language}-{name}.png"));
            match paint::hud(&text, &brand, 1)
                .map_err(|e| e.to_string())
                .and_then(|p| save_picture(&p, &path))
            {
                Ok(()) => log::info(&format!("wrote {}", path.display())),
                Err(e) => eprintln!("inputweave-app: {}: {e}", path.display()),
            }
        }
        let path = dir.join(format!("{language}-identify.png"));
        if let Err(e) = identify_picture(&path) {
            eprintln!("inputweave-app: {}: {e}", path.display());
        }
        let path = dir.join(format!("{language}-glow.png"));
        if let Err(e) = glow_picture(&path) {
            eprintln!("inputweave-app: {}: {e}", path.display());
        }

        // On GNOME's Wayland: the permissions, the firewall, the session,
        // and the arrangement where the cursor leaves only across edges.
        let sample = with_pictures((*ui.snapshot()).clone());
        let mut wayland = sample::wayland(SystemTime::now());
        wayland.arrangement = sample.arrangement.clone();
        ui.agent.show_fixed(wayland);
        window.present(&ui.snapshot(), ServiceState::Running);
        window.show_page(Page::Troubleshooting);
        gtk::glib::timeout_future(SETTLE).await;
        shot("troubleshooting-wayland", &window.widget());
        scroll_to_end(window.troubleshooting().widget());
        gtk::glib::timeout_future(SETTLE).await;
        shot("troubleshooting-wayland-end", &window.widget());
        window.troubleshooting().widget().scroll_to_top();
        for page in [Page::Arrangement, Page::Devices] {
            window.show_page(page);
            gtk::glib::timeout_future(SETTLE).await;
            shot(&format!("{}-wayland", page.name()), &window.widget());
        }
        // General's end: the switching (with the edge glow) and the
        // shortcuts, the jumps saying why they do nothing here.
        window.show_page(Page::General);
        scroll_to_end(window.general().widget());
        gtk::glib::timeout_future(SETTLE).await;
        shot("general-wayland-end", &window.widget());
        window.general().widget().scroll_to_top();

        // The license in every state, its dialogs, and About.
        ui.agent.show_fixed(sample.clone());
        license_shots(&ui, &window, &shot).await;

        // Where files from the file manager go: the picker, with two
        // computers that can't take them, and the notice when none can.
        window.show_page(Page::Files);
        let sending = sample::sending(SystemTime::now());
        let picker = crate::send::Picker::new(
            &sending,
            vec![
                "/home/me/Report.pdf".to_owned(),
                "/home/me/Photos".to_owned(),
            ],
            false,
        );
        picker.dialog.present(Some(&window.widget()));
        gtk::glib::timeout_future(SETTLE).await;
        shot("send-picker", &window.widget());
        picker.dialog.close();
        let none = crate::send::nowhere_dialog(&strings::t(keys::SEND_NOWHERE));
        none.present(Some(&window.widget()));
        gtk::glib::timeout_future(SETTLE).await;
        shot("send-nowhere", &window.widget());
        none.close();
        let path = dir.join(format!("{language}-hud-sending.png"));
        match paint::hud(
            &tr(&inputweave_app_core::send_choice::sending(2, "Studio PC")),
            &brand,
            1,
        )
        .map_err(|e| e.to_string())
        .and_then(|p| save_picture(&p, &path))
        {
            Ok(()) => log::info(&format!("wrote {}", path.display())),
            Err(e) => eprintln!("inputweave-app: {}: {e}", path.display()),
        }
        ui.agent.show_fixed(wayland_again(&sample));

        // First-run setup, step by step, with the permissions on Wayland.
        ui.open_setup();
        if let Some(setup) = ui.setup() {
            for (step, name) in [
                (Step::Welcome, "setup-welcome"),
                (Step::Name, "setup-name"),
                (Step::Permissions, "setup-permissions"),
                (Step::Connect, "setup-connect"),
                (Step::Arrange, "setup-arrange"),
                (Step::TryIt, "setup-try"),
            ] {
                setup.show_for_tests(step);
                gtk::glib::timeout_future(SETTLE).await;
                shot(name, &setup.window.clone().upcast());
            }
            setup.window.close();
        }
        ui.agent.show_fixed(sample);

        window.show_page(Page::Devices);
        AddDevice::present(&ui, &window.widget());
        gtk::glib::timeout_future(SETTLE).await;
        shot("add-device", &window.widget());
        AddDevice::present_entering_code(&ui, &window.widget());
        gtk::glib::timeout_future(SETTLE).await;
        shot("add-device-code", &window.widget());

        let code = CodeWindow::new(&ui);
        let mut model = (*ui.snapshot()).clone();
        model.pairing = sample::showing_code(SystemTime::now());
        code.update(&model);
        code.window.present();
        gtk::glib::timeout_future(SETTLE).await;
        shot("pairing-code", &code.window.clone().upcast());
        code.window.destroy();

        ui.pairing.close_add();
        // The cover while the service cannot be reached.
        let offline = Snapshot {
            unreachable_since: Some(SystemTime::now()),
            ..Default::default()
        };
        window.update(&std::sync::Arc::new(offline), ServiceState::Stopped);
        gtk::glib::timeout_future(SETTLE).await;
        shot("unavailable", &window.widget());

        drop(hold);
        app.quit();
    });
}
