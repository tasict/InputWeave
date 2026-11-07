//! The GTK side, on a display: run in the Linux container on Xvfb, as
//!
//! ```sh
//! DISPLAY=:75 cargo test -p inputweave-app -- --ignored --test-threads=1
//! ```
//!
//! GTK may only be used from the thread that started it, so it is all one
//! test.

use std::rc::Rc;
use std::time::SystemTime;

use adw::prelude::*;
use inputweave_app_core::proto::{ConnectionState, PairingPhase, PairingState};
use inputweave_app_core::sample;
use inputweave_app_core::snapshot::{Link, Snapshot};

use crate::canvas::ArrangementCanvas;
use crate::pages::EntrySync;
use crate::pages::inspector::Inspector;
use crate::pages::shortcuts::{Shortcuts, Spot};
use crate::pairing::AddDevice;
use crate::ui::Ui;
use inputweave_app_core::proto::{SetArrangement, SetHotkeys, client_message::Command};
use inputweave_app_core::shortcut::{self, Row};

/// Lets GTK handle what is pending.
fn settle() {
    let context = gtk::glib::MainContext::default();
    for _ in 0..50 {
        while context.iteration(false) {}
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn window_with(child: &impl IsA<gtk::Widget>) -> adw::Window {
    let window = adw::Window::builder().content(child).build();
    window.present();
    settle();
    window
}

#[test]
#[ignore = "needs a display; run on Xvfb"]
fn gtk_behaviour() {
    adw::init().expect("a display");
    let app = adw::Application::builder()
        .application_id("cc.ctsite.inputweave.tests")
        .build();
    let ui = Ui::for_tests(&app);

    entry_rows_are_left_alone_while_edited();
    the_inspector_updates_in_place(&ui);
    pairing_codes_spread_and_wrong_ones_clear(&ui);
    a_wrong_code_gives_the_focus_back(&ui);
    a_new_attempt_is_not_the_last_ones(&ui);
    a_closed_code_window_comes_back(&ui);
    computers_move_snap_and_spring_back(&ui);
    shortcuts_are_recorded_and_refusals_said(&ui);
    the_double_press_switch_follows_the_agent(&ui);
    the_cursor_speed_row_sends_and_follows_the_agent(&ui);
    waking_is_offered_sent_and_said(&ui);
    the_wake_on_push_switch_follows_the_agent(&ui);
    the_send_picker_lists_every_computer_and_sends_to_the_one_chosen(&ui);
    synced_is_said_only_for_changes_while_shown(&ui);
    rows_show_ampersands_and_angle_brackets_as_they_are(&ui);
    the_license_page_follows_the_agent_and_sends_keys(&ui);
    the_crossed_edge_lights_up_on_x11();
}

/// The License page shows what the agent says, and Enter License Key sends
/// a plausible key and only that.
fn the_license_page_follows_the_agent_and_sends_keys(ui: &Rc<Ui>) {
    use crate::pages::license::LicensePage;
    use crate::strings::t;
    use inputweave_app_core::proto::{License, LicenseState, SetLicenseKey};
    use inputweave_app_core::text::keys;
    const KEY: &str = "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42";
    ui.agent.record_fixed();
    let page = LicensePage::new(ui);
    let window = window_with(page.widget());
    let mut model = (*ui.snapshot()).clone();
    model.status.license = None;
    page.update(&model);
    settle();
    assert!(page.trial_shown());
    let shown = labels(&page.widget().clone().upcast());
    assert!(shown.contains(&t(keys::LICENSE_TRIAL)), "{shown:?}");
    assert!(shown.contains(&t(keys::LICENSE_PRICE)));

    let dialog = page.dialog_for_screenshot(ui);
    settle();
    let sent_keys = || -> Vec<String> {
        ui.agent
            .sent()
            .iter()
            .filter_map(|c| match c {
                Command::SetLicenseKey(SetLicenseKey { key }) => Some(key.clone()),
                _ => None,
            })
            .collect()
    };
    dialog.show_key("not a key");
    dialog.activate_for_tests();
    settle();
    assert!(sent_keys().is_empty(), "an implausible key is not sent");
    dialog.show_key(&format!("  {KEY} "));
    dialog.activate_for_tests();
    settle();
    assert_eq!(sent_keys(), [KEY.to_owned()]);
    dialog.dialog.close();

    model.status.license = Some(License {
        key: KEY.into(),
        state: LicenseState::Licensed as i32,
        activation_id: "a".into(),
        customer_email: "someone@example.com".into(),
        ..Default::default()
    });
    page.update(&model);
    settle();
    let shown = labels(&page.widget().clone().upcast());
    assert!(
        shown.iter().any(|l| l.contains("s•••@example.com")),
        "{shown:?}"
    );
    assert!(!page.trial_shown());
    window.destroy();
}

/// Every label's text under `widget`.
fn labels(widget: &gtk::Widget) -> Vec<String> {
    let mut found = Vec::new();
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        found.push(label.text().to_string());
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        found.extend(labels(&c));
        child = c.next_sibling();
    }
    found
}

/// Rows with the agent's words, names or commands show them as text, not
/// as Pango markup, which `&` or `<` would break (the row went blank).
fn rows_show_ampersands_and_angle_brackets_as_they_are(ui: &Rc<Ui>) {
    use inputweave_app_core::proto::Permission;
    use inputweave_app_core::text::{UiText, keys};
    use inputweave_app_core::trouble_text::PermissionRow;
    let command = "sudo ufw allow InputWeave && sudo ufw allow 50000/udp";
    let failure = "Couldn't ask: <portal> & co";
    let list = gtk::ListBox::new();
    list.append(&crate::pages::command_row(ui, "To let A & B in:", command));
    list.append(&crate::pages::permission_row(
        ui,
        &PermissionRow {
            permission: Permission::RemoteControl,
            title: keys::PERMISSION_REMOTE_CONTROL,
            state: UiText::verbatim(failure),
            button: Some(keys::PERMISSION_ALLOW),
            note: None,
        },
    ));
    let window = window_with(&list);
    let shown = labels(&list.clone().upcast());
    for text in [command, "To let A & B in:", failure] {
        assert!(shown.iter().any(|l| l == text), "{text:?} in {shown:?}");
    }
    window.destroy();
}

/// The override-redirect windows mapped now, with their geometry and
/// whether their input shape is empty.
fn mapped_overlays() -> Vec<(i32, i32, i32, i32, bool)> {
    use x11rb::connection::Connection;
    use x11rb::protocol::shape::{self, ConnectionExt as _};
    use x11rb::protocol::xproto::{ConnectionExt as _, MapState};
    let (conn, screen) = x11rb::connect(None).expect("the X server");
    let root = conn.setup().roots[screen].root;
    let children = conn.query_tree(root).unwrap().reply().unwrap().children;
    children
        .into_iter()
        .filter_map(|w| {
            let attributes = conn.get_window_attributes(w).ok()?.reply().ok()?;
            if !attributes.override_redirect || attributes.map_state != MapState::VIEWABLE {
                return None;
            }
            let g = conn.get_geometry(w).ok()?.reply().ok()?;
            let input = conn
                .shape_get_rectangles(w, shape::SK::INPUT)
                .ok()?
                .reply()
                .ok()?;
            Some((
                i32::from(g.x),
                i32::from(g.y),
                i32::from(g.width),
                i32::from(g.height),
                input.rectangles.is_empty(),
            ))
        })
        .collect()
}

/// Paints the root window with a gradient, as a desktop to see the
/// overlays over.
fn desktop_behind() {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        ChangeWindowAttributesAux, ConnectionExt as _, CreateGCAux, ImageFormat,
    };
    let (conn, screen) = x11rb::connect(None).expect("the X server");
    let s = &conn.setup().roots[screen];
    let (root, w, h) = (s.root, s.width_in_pixels, s.height_in_pixels);
    let pixmap = conn.generate_id().unwrap();
    conn.create_pixmap(s.root_depth, pixmap, root, w, h)
        .unwrap();
    let gc = conn.generate_id().unwrap();
    conn.create_gc(gc, pixmap, &CreateGCAux::new()).unwrap();
    for y in 0..h {
        let row: Vec<u8> = (0..w)
            .flat_map(|x| {
                let t = (f64::from(x) / f64::from(w) + f64::from(y) / f64::from(h)) / 2.0;
                let mix = |a: f64, b: f64| ((a + (b - a) * t) * 255.0) as u8;
                // XRGB, little-endian.
                [mix(0.6, 0.85), mix(0.35, 0.75), mix(0.2, 0.5), 0]
            })
            .collect();
        conn.put_image(
            ImageFormat::Z_PIXMAP,
            pixmap,
            gc,
            w,
            1,
            0,
            y as i16,
            0,
            s.root_depth,
            &row,
        )
        .unwrap();
    }
    conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new().background_pixmap(pixmap),
    )
    .unwrap();
    conn.clear_area(false, root, 0, 0, 0, 0).unwrap();
    conn.get_input_focus().unwrap().reply().unwrap();
}

/// Saves the screen as the X server shows it, to the folder in
/// `INPUTWEAVE_SHOTS` (nothing without).
fn save_screen(name: &str) {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};
    let Some(dir) = std::env::var_os("INPUTWEAVE_SHOTS") else {
        return;
    };
    let (conn, screen) = x11rb::connect(None).expect("the X server");
    let s = &conn.setup().roots[screen];
    let (w, h) = (s.width_in_pixels, s.height_in_pixels);
    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, s.root, 0, 0, w, h, !0)
        .unwrap()
        .reply()
        .unwrap();
    let mut data = image.data;
    for pixel in data.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    let texture = gtk::gdk::MemoryTexture::new(
        i32::from(w),
        i32::from(h),
        gtk::gdk::MemoryFormat::B8g8r8a8,
        &gtk::glib::Bytes::from_owned(data),
        usize::from(w) * 4,
    );
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    texture.save_to_png(&path).unwrap();
}

/// Waits up to `limit` for `seen` to hold of the mapped overlays.
fn overlays_until(
    limit: std::time::Duration,
    seen: impl Fn(&[(i32, i32, i32, i32, bool)]) -> bool,
) -> Vec<(i32, i32, i32, i32, bool)> {
    let start = std::time::Instant::now();
    loop {
        settle();
        let now = mapped_overlays();
        if seen(&now) || start.elapsed() > limit {
            return now;
        }
    }
}

/// On X11 without a compositor (Xvfb), the edge glows as a solid line
/// along the whole edge crossed, that clicks go through, for as long as
/// the glow lasts; crossing it again keeps the one window.
fn the_crossed_edge_lights_up_on_x11() {
    use inputweave_app_core::proto::{CursorCrossed, ScreenEdge};
    use std::time::Duration;
    let overlay = crate::overlay::Overlay::new();
    assert_eq!(
        overlay.glows(),
        inputweave_app_core::edge_glow::Availability::Shown,
        "X11 shows glows"
    );
    let display = gtk::gdk::Display::default().unwrap();
    let monitor = display
        .monitors()
        .item(0)
        .and_downcast::<gtk::gdk::Monitor>()
        .unwrap();
    let g = monitor.geometry();
    let s = monitor.scale_factor();
    let (x, y, w, h) = (g.x() * s, g.y() * s, g.width() * s, g.height() * s);
    let crossed = |edge: ScreenEdge, along: f64| CursorCrossed {
        display_id: monitor
            .connector()
            .map(|c| c.to_string())
            .unwrap_or_default(),
        edge: edge as i32,
        along,
        display_x: f64::from(x),
        display_y: f64::from(y),
        display_width: f64::from(w),
        display_height: f64::from(h),
        ..Default::default()
    };
    desktop_behind();
    let before = mapped_overlays().len();
    overlay.glow(&crossed(ScreenEdge::Right, f64::from(y + h / 3)));
    let line = (x + w - 4, y, 4, h, true);
    let shown = overlays_until(Duration::from_secs(1), |o| o.contains(&line));
    assert!(shown.contains(&line), "the right edge's line: {shown:?}");
    save_screen("glow-x11-no-compositor");
    // Crossed again while it shows, and the top edge too.
    overlay.glow(&crossed(ScreenEdge::Right, f64::from(y + h / 2)));
    overlay.glow(&crossed(ScreenEdge::Top, f64::from(x + w / 2)));
    let top = (x, y, w, 4, true);
    let shown = overlays_until(Duration::from_secs(1), |o| o.contains(&top));
    assert!(shown.contains(&top), "the top edge's line: {shown:?}");
    assert_eq!(
        shown.iter().filter(|o| **o == line).count(),
        1,
        "one window for the right edge"
    );
    // Gone after the glow's 0.8 s.
    let gone = overlays_until(Duration::from_secs(3), |o| o.len() <= before);
    assert!(
        !gone.contains(&line) && !gone.contains(&top),
        "the lines are gone: {gone:?}"
    );

    // With a compositor (here only its selection, owned by this test): a
    // window of a 32-bit visual, 24 deep, strongest at the edge and fading
    // to nothing inward.
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt as _, CreateWindowAux, ImageFormat, WindowClass};
    let (conn, screen) = x11rb::connect(None).expect("the X server");
    let root = conn.setup().roots[screen].root;
    let owner = conn.generate_id().unwrap();
    conn.create_window(
        0,
        owner,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    let atom = conn
        .intern_atom(false, format!("_NET_WM_CM_S{screen}").as_bytes())
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    conn.set_selection_owner(owner, atom, x11rb::CURRENT_TIME)
        .unwrap();
    conn.get_input_focus().unwrap().reply().unwrap();
    overlay.glow(&crossed(ScreenEdge::Left, f64::from(y + h / 2)));
    let glow = (x, y, 24, h, true);
    let shown = overlays_until(Duration::from_secs(1), |o| o.contains(&glow));
    assert!(shown.contains(&glow), "the left edge's glow: {shown:?}");
    // Crossed again: back at full strength after 0.1 s, and held.
    overlay.glow(&crossed(ScreenEdge::Left, f64::from(y + h / 2)));
    std::thread::sleep(Duration::from_millis(150));
    let window = conn
        .query_tree(root)
        .unwrap()
        .reply()
        .unwrap()
        .children
        .into_iter()
        .find(|w| {
            conn.get_geometry(*w)
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some_and(|g| (g.x, g.y, g.width, g.depth) == (x as i16, y as i16, 24, 32))
        })
        .expect("a window of depth 32 at the left edge");
    let row = conn
        .get_image(ImageFormat::Z_PIXMAP, window, 0, (h / 2) as i16, 24, 1, !0)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(row.depth, 32);
    let alpha = |i: usize| u32::from_ne_bytes(row.data[i * 4..i * 4 + 4].try_into().unwrap()) >> 24;
    assert!(alpha(0) > 200, "strong at the edge: {}", alpha(0));
    assert!(
        alpha(12) < alpha(0) && alpha(12) > alpha(23),
        "fading inward"
    );
    assert!(alpha(23) < 20, "nothing at the inner end: {}", alpha(23));
    save_screen("glow-x11-argb");
    conn.destroy_window(owner).unwrap();
    conn.get_input_focus().unwrap().reply().unwrap();
    let gone = overlays_until(Duration::from_secs(3), |o| !o.contains(&glow));
    assert!(!gone.contains(&glow), "the glow is gone: {gone:?}");
}

fn synced_is_said_only_for_changes_while_shown(ui: &Rc<Ui>) {
    use crate::window::Page;
    use inputweave_app_core::service::ServiceState;
    let window = ui.window();
    window.present(&ui.snapshot(), ServiceState::Running);
    window.show_page(Page::General);
    settle();
    let bump = |ui: &Rc<Ui>| {
        let mut model = (*ui.snapshot()).clone();
        let mut arrangement = (*model.arrangement).clone();
        arrangement.version += 1;
        model.arrangement = std::sync::Arc::new(arrangement);
        ui.agent.show_fixed(model);
        ui.snapshot()
    };
    window.update(&bump(ui), ServiceState::Running);
    // Changed while another page was shown: not news on opening this one.
    window.update(&bump(ui), ServiceState::Running);
    window.show_page(Page::Arrangement);
    window.update(&ui.snapshot(), ServiceState::Running);
    settle();
    assert!(
        !window.arrangement().synced_shown(),
        "not for what changed before"
    );
    window.update(&bump(ui), ServiceState::Running);
    settle();
    assert!(
        window.arrangement().synced_shown(),
        "said for a change while shown"
    );
    window.window().set_visible(false);
}

/// The SetArrangement commands sent since `before`: (device, x, y).
fn moves(ui: &Rc<Ui>, before: usize) -> Vec<(String, f64, f64)> {
    ui.agent.sent()[before..]
        .iter()
        .filter_map(|c| match c {
            Command::SetArrangement(SetArrangement { positions }) => Some(positions.clone()),
            _ => None,
        })
        .flatten()
        .map(|p| (p.device_id, p.x, p.y))
        .collect()
}

/// The sample with the Studio PC at `x`, as the service would report it.
fn with_pc_at(ui: &Rc<Ui>, x: f64) -> std::sync::Arc<Snapshot> {
    let mut model = (*ui.snapshot()).clone();
    let mut arrangement = (*model.arrangement).clone();
    for device in &mut arrangement.devices {
        if device.id == "pc" {
            device.x = x;
        }
    }
    arrangement.version += 1;
    model.arrangement = std::sync::Arc::new(arrangement);
    ui.agent.show_fixed(model);
    ui.snapshot()
}

fn computers_move_snap_and_spring_back(ui: &Rc<Ui>) {
    ui.agent.record_fixed();
    let canvas = ArrangementCanvas::new(ui);
    let window = adw::Window::builder()
        .default_width(900)
        .default_height(500)
        .content(canvas.widget())
        .build();
    window.present();
    settle();
    canvas.update(&with_pc_at(ui, 5000.0));
    settle();
    let scale = canvas.scale().expect("drawn");
    let (x, y) = canvas.center_of("pc").expect("the PC is on the canvas");

    // Dropped near the Linux box's right edge (4480): it snaps there.
    let before = ui.agent.sent().len();
    let near = (520.0 - 30.0) * scale;
    canvas.drag_for_tests(x, y, -near, 2.0);
    settle();
    let moved = moves(ui, before);
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert_eq!(moved[0].0, "pc");
    assert!(
        (moved[0].1 - 4480.0).abs() < 1e-6,
        "snapped beside: {moved:?}"
    );
    assert!(moved[0].2.abs() < 1e-6, "tops lined up: {moved:?}");

    // The service reports it there; Ctrl+Z puts it back where it was.
    canvas.update(&with_pc_at(ui, 4480.0));
    settle();
    let before = ui.agent.sent().len();
    assert!(canvas.key_for_tests(gtk::gdk::Key::z, gtk::gdk::ModifierType::CONTROL_MASK));
    settle();
    let undone = moves(ui, before);
    assert_eq!(undone.len(), 1, "{undone:?}");
    assert!((undone[0].1 - 5000.0).abs() < 1e-6, "{undone:?}");

    // Dropped on top of another computer: nothing sent, it springs back.
    canvas.update(&with_pc_at(ui, 5000.0));
    settle();
    let (x, y) = canvas.center_of("pc").expect("still there");
    let before = ui.agent.sent().len();
    canvas.drag_for_tests(x, y, -2500.0 * scale, 0.0);
    assert!(
        moves(ui, before).is_empty(),
        "an overlapping move is not sent"
    );
    assert!(
        canvas.springing()
            || gtk::Settings::default().is_some_and(|s| !s.is_gtk_enable_animations()),
        "it springs back"
    );

    // Arrow keys move the selected one by 10 view units.
    settle();
    canvas.select(Some("pc".into()));
    let before = ui.agent.sent().len();
    assert!(canvas.key_for_tests(gtk::gdk::Key::Right, gtk::gdk::ModifierType::empty()));
    settle();
    let nudged = moves(ui, before);
    assert_eq!(nudged.len(), 1);
    assert!(
        (nudged[0].1 - (5000.0 + 10.0 / scale)).abs() < 1e-6,
        "{nudged:?}"
    );
    window.destroy();
}

fn shortcuts_are_recorded_and_refusals_said(ui: &Rc<Ui>) {
    use inputweave_app_core::text::keys;

    ui.agent.record_fixed();
    let shortcuts = Shortcuts::new(ui);
    let window = window_with(shortcuts.widget());
    shortcuts.update(&ui.snapshot());
    let sent_hotkeys = |before: usize| -> Vec<inputweave_app_core::proto::Hotkeys> {
        ui.agent.sent()[before..]
            .iter()
            .filter_map(|c| match c {
                Command::SetHotkeys(SetHotkeys { hotkeys }) => hotkeys.clone(),
                _ => None,
            })
            .collect()
    };
    let (control, alt, shift) = (
        gtk::gdk::ModifierType::CONTROL_MASK,
        gtk::gdk::ModifierType::ALT_MASK,
        gtk::gdk::ModifierType::SHIFT_MASK,
    );
    // evdev + 8: Left Ctrl is 29, K 37, Esc 1.
    let (left_control, key_k, escape) = (29 + 8, 37 + 8, 1 + 8);

    // A modifier alone waits; then Ctrl+Alt+K records the lock.
    let before = ui.agent.sent().len();
    shortcuts.record_row(Row::Lock);
    shortcuts.press_for_tests(Row::Lock, left_control, gtk::gdk::ModifierType::empty());
    assert_eq!(shortcuts.recording(), Some(Row::Lock), "waits for the key");
    shortcuts.press_for_tests(Row::Lock, key_k, control | alt);
    settle();
    assert_eq!(shortcuts.recording(), None);
    let sent = sent_hotkeys(before);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].lock, "control+alt+KeyK");
    assert_eq!(
        sent[0].take_back,
        shortcut::DEFAULT_TAKE_BACK,
        "the whole set goes"
    );

    // Esc on its own cancels: nothing sent.
    let before = ui.agent.sent().len();
    shortcuts.record_row(Row::TakeBack);
    shortcuts.press_for_tests(Row::TakeBack, escape, gtk::gdk::ModifierType::empty());
    settle();
    assert_eq!(shortcuts.recording(), None);
    assert!(sent_hotkeys(before).is_empty());

    // Cancel stops recording, and does not start it again: the button
    // does not take the focus from the box listening.
    let cancel = shortcuts.record_button(Row::Lock);
    assert!(!cancel.gets_focus_on_click());
    shortcuts.record_row(Row::Lock);
    cancel.emit_clicked();
    settle();
    assert_eq!(shortcuts.recording(), None, "Cancel stops recording");
    cancel.emit_clicked();
    settle();
    assert_eq!(shortcuts.recording(), Some(Row::Lock), "Record starts it");
    cancel.emit_clicked();
    settle();
    assert_eq!(shortcuts.recording(), None);

    // Refused as a duplicate of taking back control: said so, in red
    // under the lock's row.
    ui.agent.refuse_next("duplicate");
    shortcuts.record_row(Row::Lock);
    shortcuts.press_for_tests(Row::Lock, escape, control | alt | shift);
    settle();
    let (spot, error) = shortcuts.error_shown().expect("a refusal is said");
    assert_eq!(spot, Spot::Row(Row::Lock));
    assert!(
        error.contains(&crate::strings::t(keys::SHORTCUT_TAKE_BACK)),
        "{error}"
    );
    assert_eq!(shortcuts.subtitle_of(Row::Lock), error);
    // It stays through updates, and recording again clears it.
    shortcuts.update(&ui.snapshot());
    assert_eq!(shortcuts.subtitle_of(Row::Lock), error);
    shortcuts.record_row(Row::TakeBack);
    assert_eq!(shortcuts.error_shown(), None);
    assert_eq!(shortcuts.subtitle_of(Row::Lock), "");

    // F13 alone goes for taking back control, as it is.
    let f13 = 183 + 8;
    let before = ui.agent.sent().len();
    shortcuts.press_for_tests(Row::TakeBack, f13, gtk::gdk::ModifierType::empty());
    settle();
    let sent = sent_hotkeys(before);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].take_back, "F13");
    // K alone is refused: the row says which keys work alone.
    ui.agent.refuse_next("needs_modifier");
    shortcuts.record_row(Row::TakeBack);
    shortcuts.press_for_tests(Row::TakeBack, key_k, gtk::gdk::ModifierType::empty());
    settle();
    assert_eq!(
        shortcuts.error_shown(),
        Some((
            Spot::Row(Row::TakeBack),
            crate::strings::t(keys::SHORTCUT_SINGLE_KEYS)
        ))
    );
    // For relative mouse, a modifier is what it needs.
    ui.agent.refuse_next("needs_modifier");
    shortcuts.record_row(Row::Relative);
    shortcuts.press_for_tests(Row::Relative, f13, gtk::gdk::ModifierType::empty());
    settle();
    assert_eq!(
        shortcuts.error_shown(),
        Some((
            Spot::Row(Row::Relative),
            crate::strings::t(keys::SHORTCUT_NEEDS_MODIFIER)
        ))
    );
    assert_eq!(shortcuts.subtitle_of(Row::TakeBack), "");
    window.destroy();
}

/// The double press switch: shown when the service knows it, sent with
/// the whole set, its refusal said under it, and Reset Shortcuts turns it
/// off.
fn the_double_press_switch_follows_the_agent(ui: &Rc<Ui>) {
    use inputweave_app_core::text::keys;

    ui.agent.record_fixed();
    let shortcuts = Shortcuts::new(ui);
    let window = window_with(shortcuts.widget());
    let sent_hotkeys = |before: usize| -> Vec<inputweave_app_core::proto::Hotkeys> {
        ui.agent.sent()[before..]
            .iter()
            .filter_map(|c| match c {
                Command::SetHotkeys(SetHotkeys { hotkeys }) => hotkeys.clone(),
                _ => None,
            })
            .collect()
    };
    let mut model = (*ui.snapshot()).clone();
    let switch = shortcuts.double_press_row();

    // A service from before it: no switch, and Reset leaves it out.
    let mut old = shortcut::defaults();
    old.take_back_double = None;
    model.status.hotkeys = Some(old);
    shortcuts.update(&model);
    assert!(!switch.is_visible());
    assert!(!shortcuts.reset_button().is_sensitive());

    // Off: shown, and turning it on sends the whole set with it.
    model.status.hotkeys = Some(shortcut::defaults());
    shortcuts.update(&model);
    assert!(switch.is_visible() && !switch.is_active());
    assert!(!shortcuts.reset_button().is_sensitive());
    let before = ui.agent.sent().len();
    switch.set_active(true);
    settle();
    let sent = sent_hotkeys(before);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].take_back_double.as_deref(), Some("double+control"));
    assert_eq!(sent[0].lock, shortcut::DEFAULT_LOCK, "the whole set goes");
    // The service still has it off: the switch goes back.
    shortcuts.update(&model);
    assert!(!switch.is_active());

    // On, as the service says: Reset Shortcuts can turn it off.
    model.status.hotkeys = Some(shortcut::with_double_press(&shortcut::defaults(), true));
    shortcuts.update(&model);
    assert!(switch.is_active());
    assert!(shortcuts.reset_button().is_sensitive());
    let before = ui.agent.sent().len();
    shortcuts.reset_button().emit_clicked();
    settle();
    let sent = sent_hotkeys(before);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].take_back_double.as_deref(), Some(""), "turned off");
    assert_eq!(sent[0].take_back, shortcut::DEFAULT_TAKE_BACK);

    // Refused: said in red under it; changed but not saved, as such.
    ui.agent.refuse_next("not_saved");
    switch.set_active(false);
    settle();
    let (spot, error) = shortcuts.error_shown().expect("a refusal is said");
    assert_eq!(spot, Spot::DoublePress);
    assert_eq!(error, crate::strings::t(keys::SHORTCUT_NOT_SAVED));
    assert_eq!(switch.subtitle().unwrap_or_default(), error);
    assert!(switch.has_css_class("iw-failed"));
    // Taken next time: its own words again.
    shortcuts.update(&model);
    assert!(switch.is_active());
    switch.set_active(false);
    settle();
    assert_eq!(shortcuts.error_shown(), None);
    assert_eq!(
        switch.subtitle().unwrap_or_default(),
        crate::strings::t(keys::SHORTCUT_DOUBLE_PRESS_HINT)
    );
    assert!(!switch.has_css_class("iw-failed"));
    window.destroy();
}

/// Sending files with more than one computer to send to: every other
/// computer listed, those that can take files first and the first of them
/// chosen, the others off with why; the rows following the agent while the
/// picker is open; a second request's items added to it; Send sending the
/// items to the one chosen. Connected computers none of which can take
/// files: every row off and Send off. With none connected, only OK.
fn the_send_picker_lists_every_computer_and_sends_to_the_one_chosen(ui: &Rc<Ui>) {
    use crate::send::{Picker, nowhere_dialog};
    use inputweave_app_core::proto::{ConnectionState, SendFiles};
    ui.agent.record_fixed();
    let model = sample::sending(SystemTime::now());
    ui.agent.show_fixed(model.clone());
    let paths = vec![
        "/home/me/Report.pdf".to_owned(),
        "/home/me/Photos".to_owned(),
    ];
    let rows = |picker: &Picker| -> Vec<(String, bool, bool, String)> {
        picker
            .rows
            .borrow()
            .iter()
            .map(|r| {
                (
                    r.id.clone(),
                    r.radio.is_active(),
                    r.row.is_sensitive(),
                    r.row.subtitle().map(|s| s.to_string()).unwrap_or_default(),
                )
            })
            .collect()
    };
    let row = |id: &str, on: bool, sensitive: bool, status: &str| {
        (id.to_owned(), on, sensitive, status.to_owned())
    };
    let picker = Picker::new(&model, paths.clone(), false);
    let window = window_with(&gtk::Box::new(gtk::Orientation::Vertical, 0));
    window.set_default_size(600, 600);
    assert_eq!(
        picker.dialog.heading().as_deref(),
        Some("Send to which computer?")
    );
    assert_eq!(picker.dialog.body().as_str(), "Report.pdf and 1 more item");
    assert_eq!(
        rows(&picker),
        [
            row("mac", true, true, "Connected · 3 ms"),
            row("pc", false, true, "Connected · 5 ms"),
            row("laptop", false, false, "Offline"),
            row(
                "media",
                false,
                false,
                "File transfer with this computer is off"
            ),
        ]
    );
    assert!(picker.dialog.is_response_enabled("send"));
    assert_eq!(picker.chosen(), Some(("mac".into(), "Mac mini".into())));
    picker.rows.borrow()[1].radio.set_active(true);
    assert_eq!(picker.chosen(), Some(("pc".into(), "Studio PC".into())));
    assert!(!picker.rows.borrow()[0].radio.is_active(), "one at a time");
    picker.present(ui, Some(window.upcast_ref()));
    settle();
    assert!(ui.send_picker().is_some(), "kept up to date while open");

    // It follows the agent: the Studio PC chosen goes offline, so the
    // choice moves to the Mac mini; the ThinkPad connects and can be
    // chosen; the Studio PC comes back and is chosen again.
    let with = |changes: &[(&str, ConnectionState, u32)]| {
        let mut model = model.clone();
        for (id, state, rtt) in changes {
            let device = model
                .status
                .devices
                .iter_mut()
                .find(|d| d.id == *id)
                .expect("in the sample");
            device.state = *state as i32;
            device.rtt_ms = *rtt;
        }
        model
    };
    ui.agent.show_fixed(with(&[
        ("pc", ConnectionState::Offline, 0),
        ("laptop", ConnectionState::Connected, 9),
    ]));
    ui.refresh();
    assert_eq!(
        rows(&picker),
        [
            row("mac", true, true, "Connected · 3 ms"),
            row("laptop", false, true, "Connected · 9 ms"),
            row("pc", false, false, "Offline"),
            row(
                "media",
                false,
                false,
                "File transfer with this computer is off"
            ),
        ]
    );
    ui.agent.show_fixed(model.clone());
    ui.refresh();
    assert_eq!(picker.chosen(), Some(("pc".into(), "Studio PC".into())));

    // A second request while it is open adds its items, each once.
    let over = Rc::new(std::cell::Cell::new(false));
    let marked = over.clone();
    crate::send::from_file_manager(
        ui,
        vec!["/home/me/Photos".into(), "/home/me/Notes.txt".into()],
        crate::send::Then::new(move || marked.set(true)),
    );
    settle();
    assert!(over.get(), "this request is over once its files joined");
    assert_eq!(picker.dialog.body().as_str(), "Report.pdf and 2 more items");

    let before = ui.agent.sent().len();
    picker.dialog.emit_by_name::<()>("response", &[&"send"]);
    settle();
    assert_eq!(
        ui.agent.sent()[before..],
        [Command::SendFiles(SendFiles {
            device_id: "pc".into(),
            paths: vec![
                "/home/me/Report.pdf".into(),
                "/home/me/Photos".into(),
                "/home/me/Notes.txt".into(),
            ],
        })]
    );
    picker.dialog.close();
    for _ in 0..20 {
        if ui.send_picker().is_none() {
            break;
        }
        settle();
    }
    assert!(ui.send_picker().is_none(), "let go once closed");

    // Chosen by default, the Studio PC stays chosen when the Mac mini,
    // listed ahead of it, connects.
    let mac_away = with(&[("mac", ConnectionState::Offline, 0)]);
    let picker = Picker::new(&mac_away, vec!["/home/me/Report.pdf".into()], false);
    assert_eq!(picker.chosen(), Some(("pc".into(), "Studio PC".into())));
    picker.update(&model);
    assert_eq!(rows(&picker)[0].0, "mac", "listed first");
    assert_eq!(
        picker.chosen(),
        Some(("pc".into(), "Studio PC".into())),
        "not taken by one that connects ahead"
    );

    // Connected, but none can take files: every row off with why, and
    // Send off.
    let none_can = with(&[
        ("mac", ConnectionState::Offline, 0),
        ("pc", ConnectionState::Offline, 0),
    ]);
    let picker = Picker::new(&none_can, vec!["/home/me/Report.pdf".into()], false);
    assert_eq!(picker.dialog.body().as_str(), "Report.pdf");
    assert!(
        rows(&picker)
            .iter()
            .all(|(_, on, sensitive, _)| !on && !sensitive)
    );
    assert!(!picker.dialog.is_response_enabled("send"));
    assert_eq!(picker.chosen(), None);
    // Cancel sends nothing.
    picker.present(ui, Some(window.upcast_ref()));
    settle();
    let before = ui.agent.sent().len();
    picker.dialog.emit_by_name::<()>("response", &[&"cancel"]);
    settle();
    assert_eq!(ui.agent.sent().len(), before);
    picker.dialog.close();
    settle();

    let none = nowhere_dialog(&crate::strings::t(
        inputweave_app_core::text::keys::SEND_NOWHERE,
    ));
    assert_eq!(
        none.heading().as_deref(),
        Some("No connected computer can receive files.")
    );
    assert!(none.has_response("ok") && !none.has_response("cancel"));
    window.close();
    ui.agent.show_fixed(sample::snapshot(SystemTime::now()));
}

/// The sample with the ThinkPad (asleep two hours) in `wake`, said to be
/// wakeable or not.
fn thinkpad(wake: inputweave_app_core::proto::WakeState, can_wake: bool) -> Snapshot {
    let mut model = sample::snapshot(SystemTime::now());
    let laptop = model
        .status
        .devices
        .iter_mut()
        .find(|d| d.id == "laptop")
        .expect("the sample's ThinkPad");
    laptop.wake = wake as i32;
    laptop.can_wake = can_wake;
    model
}

/// A menu's items as (label, action), a section starting with ("—", "").
fn menu_entries(menu: &gtk::gio::MenuModel) -> Vec<(String, String)> {
    use gtk::gio;
    let attribute = |i: i32, name: &str| {
        menu.item_attribute_value(i, name, Some(gtk::glib::VariantTy::STRING))
            .and_then(|v| v.get::<String>())
            .unwrap_or_default()
    };
    let mut entries = Vec::new();
    for i in 0..menu.n_items() {
        if let Some(section) = menu.item_link(i, gio::MENU_LINK_SECTION) {
            entries.push(("—".to_owned(), String::new()));
            entries.extend(menu_entries(&section));
        } else {
            entries.push((
                attribute(i, gio::MENU_ATTRIBUTE_LABEL),
                attribute(i, gio::MENU_ATTRIBUTE_ACTION),
            ));
        }
    }
    entries
}

/// Waking another computer: its status, the moon and the spinner, Wake
/// sent once and off while it is on its way or the computer is being woken,
/// the lines under the status, the list's moon, the arrangement's menu, and
/// none of it for a connected computer, this one or from a service before
/// waking.
fn waking_is_offered_sent_and_said(ui: &Rc<Ui>) {
    use crate::strings::t;
    use inputweave_app_core::proto::{WakeDevice, WakeState};
    use inputweave_app_core::text::keys;
    use std::sync::Arc;
    ui.agent.record_fixed();
    let inspector = Inspector::new(ui);
    let window = window_with(inspector.widget());
    let show = |model: &Snapshot, id: &str| {
        ui.agent.show_fixed(model.clone());
        inspector.update(model, Some(id));
    };
    let wake_laptop = || {
        Command::WakeDevice(WakeDevice {
            device_id: "laptop".into(),
        })
    };

    // Asleep, and it said how to wake it.
    let model = thinkpad(WakeState::None, true);
    show(&model, "laptop");
    settle();
    let status = inspector.status_row().expect("built");
    let row = inspector.wake_widgets().expect("another computer's");
    assert_eq!(status.subtitle().as_deref(), Some("Offline · 2 hours ago"));
    assert!(row.moon.is_visible() && !row.spinner.is_visible());
    assert!(row.button.is_visible() && row.button.is_sensitive());
    assert!(row.button.has_css_class("suggested-action"));
    assert_eq!(row.button.label().as_deref(), Some(t(keys::WAKE).as_str()));
    assert!(!row.not_woken.is_visible() && !row.not_told.is_visible());

    // Pressed: sent once, and off until the service answers.
    let before = ui.agent.sent().len();
    row.button.emit_clicked();
    inspector.update(&model, Some("laptop"));
    assert!(!row.button.is_sensitive(), "off while on its way");
    row.button.emit_clicked();
    settle();
    assert_eq!(ui.agent.sent()[before..], [wake_laptop()]);
    inspector.update(&model, Some("laptop"));
    assert!(row.button.is_sensitive(), "answered");

    // Being woken: the spinner in the moon's place, Wake off.
    let model = thinkpad(WakeState::Waking, true);
    show(&model, "laptop");
    assert_eq!(status.subtitle().as_deref(), Some("Waking…"));
    assert!(row.spinner.is_visible() && row.spinner.is_spinning());
    assert!(!row.moon.is_visible());
    assert!(row.button.is_visible() && !row.button.is_sensitive());
    let before = ui.agent.sent().len();
    ui.wake_device("laptop");
    settle();
    assert_eq!(ui.agent.sent().len(), before, "not while being woken");

    // Not woken: why, and how to turn waking on; Wake tries again.
    let model = thinkpad(WakeState::NotWoken, true);
    show(&model, "laptop");
    assert_eq!(status.subtitle().as_deref(), Some("Offline · 2 hours ago"));
    assert!(row.moon.is_visible());
    assert!(!row.spinner.is_visible() && !row.spinner.is_spinning());
    assert!(row.button.is_sensitive());
    assert!(row.not_woken.is_visible() && !row.not_told.is_visible());
    let markup = row.not_woken_text.label().to_string();
    assert!(
        markup.starts_with("ThinkPad didn’t wake up. Wake for network access may be off on it"),
        "{markup}"
    );
    assert!(
        markup.contains(
            "<a href=\"https://inputweave.ctsite.cc/user-guide#wake-for-network-access\">How to turn on wake for network access</a>"
        ),
        "{markup}"
    );
    let before = ui.agent.sent().len();
    row.button.emit_clicked();
    settle();
    assert_eq!(ui.agent.sent()[before..], [wake_laptop()]);

    // It never said how: no Wake, a line saying so.
    let model = thinkpad(WakeState::None, false);
    show(&model, "laptop");
    assert_eq!(status.subtitle().as_deref(), Some("Offline"));
    assert!(!row.moon.is_visible() && !row.spinner.is_visible());
    assert!(!row.button.is_visible());
    assert!(!row.not_woken.is_visible() && row.not_told.is_visible());
    assert_eq!(
        row.not_told.text(),
        "ThinkPad hasn’t told this computer how to wake it yet. You can wake it after it connects again."
    );

    // A service before waking: nothing of it.
    let mut old = thinkpad(WakeState::NotWoken, true);
    old.status.wake_supported = false;
    show(&old, "laptop");
    assert_eq!(status.subtitle().as_deref(), Some("Offline"));
    assert!(!row.moon.is_visible() && !row.spinner.is_visible());
    assert!(!row.button.is_visible());
    assert!(!row.not_woken.is_visible() && !row.not_told.is_visible());

    // Connected, or this computer: nothing either.
    let model = thinkpad(WakeState::None, true);
    show(&model, "mac");
    let row = inspector.wake_widgets().expect("another computer's");
    assert!(!row.moon.is_visible() && !row.spinner.is_visible());
    assert!(!row.button.is_visible());
    assert!(!row.not_woken.is_visible() && !row.not_told.is_visible());
    show(&model, "me");
    assert!(inspector.wake_widgets().is_none());
    window.destroy();

    // The devices list: the moon after the name, the status beside it.
    let devices = crate::pages::devices::Devices::new(ui);
    let window = window_with(devices.widget());
    devices.update(&model);
    assert_eq!(devices.list().moon_shown("laptop"), Some(true));
    assert_eq!(devices.list().moon_shown("mac"), Some(false));
    assert_eq!(
        devices.list().status_of("laptop").as_deref(),
        Some("Offline · 2 hours ago")
    );
    devices.update(&thinkpad(WakeState::Waking, true));
    assert_eq!(devices.list().moon_shown("laptop"), Some(true));
    assert_eq!(
        devices.list().status_of("laptop").as_deref(),
        Some("Waking…")
    );
    devices.update(&thinkpad(WakeState::None, false));
    assert_eq!(devices.list().moon_shown("laptop"), Some(false));
    devices.update(&old);
    assert_eq!(devices.list().moon_shown("laptop"), Some(false));
    assert_eq!(
        devices.list().status_of("laptop").as_deref(),
        Some("Offline")
    );
    window.destroy();

    // The arrangement's menu: Wake after a separator, off while being
    // woken; not for a connected computer or this one.
    let canvas = ArrangementCanvas::new(ui);
    let window = window_with(canvas.widget());
    let menu = |model: &Snapshot, id: &str| {
        ui.agent.show_fixed(model.clone());
        canvas.update(&Arc::new(model.clone()));
        menu_entries(canvas.menu_for_tests(id).expect("a model").upcast_ref())
    };
    let entry = |label: &str, action: &str| (label.to_owned(), action.to_owned());
    let here = t(keys::CANVAS_MOVE_CURSOR_HERE);
    let wake = t(keys::WAKE);
    let model = thinkpad(WakeState::None, true);
    assert_eq!(
        menu(&model, "laptop"),
        [
            entry(&here, "canvas.none"),
            entry("—", ""),
            entry(&wake, "canvas.wake")
        ]
    );
    let before = ui.agent.sent().len();
    canvas
        .widget()
        .activate_action("canvas.wake", Some(&"laptop".to_variant()))
        .expect("the canvas's wake");
    settle();
    assert_eq!(ui.agent.sent()[before..], [wake_laptop()]);
    assert_eq!(
        menu(&thinkpad(WakeState::Waking, true), "laptop"),
        [
            entry(&here, "canvas.none"),
            entry("—", ""),
            entry(&wake, "canvas.none")
        ]
    );
    assert_eq!(
        menu(&thinkpad(WakeState::None, false), "laptop"),
        [entry(&here, "canvas.none")]
    );
    assert_eq!(menu(&old, "laptop"), [entry(&here, "canvas.none")]);
    assert_eq!(menu(&model, "mac"), [entry(&here, "canvas.jump")]);
    assert_eq!(menu(&model, "me"), [entry(&here, "canvas.none")]);
    window.destroy();
    ui.agent.show_fixed(sample::snapshot(SystemTime::now()));
}

/// The General switch for waking a computer the cursor pushes toward: shown
/// from a service that wakes devices, sent, and back with the refusal said
/// in red when refused, until the service reports another value.
fn the_wake_on_push_switch_follows_the_agent(ui: &Rc<Ui>) {
    use crate::strings::t;
    use inputweave_app_core::proto::SetWakeOnPush;
    use inputweave_app_core::service::ServiceState;
    use inputweave_app_core::text::keys;
    ui.agent.record_fixed();
    let general = crate::pages::general::General::new(ui);
    let window = window_with(general.widget());
    let switch = general.wake_on_push_row().clone();
    let mut model = sample::snapshot(SystemTime::now());
    let show = |model: &Snapshot| {
        ui.agent.show_fixed(model.clone());
        general.update(model, ServiceState::Running);
    };
    let sent = |before: usize| -> Vec<bool> {
        ui.agent.sent()[before..]
            .iter()
            .filter_map(|c| match c {
                Command::SetWakeOnPush(SetWakeOnPush { on }) => Some(*on),
                _ => None,
            })
            .collect()
    };
    show(&model);
    assert!(switch.is_visible() && !switch.is_active());
    assert_eq!(
        switch.subtitle().unwrap_or_default(),
        t(keys::GENERAL_WAKE_ON_PUSH_HINT)
    );

    // Turned on: sent; the service still says off, so it goes back.
    let before = ui.agent.sent().len();
    switch.set_active(true);
    settle();
    assert_eq!(sent(before), [true]);
    show(&model);
    assert!(!switch.is_active());
    // Taken: it follows the service.
    model.status.wake_on_push = true;
    show(&model);
    assert!(switch.is_active());

    // Refused: back, and said in red until the service reports another
    // value.
    ui.agent.refuse_next("timeout");
    let before = ui.agent.sent().len();
    switch.set_active(false);
    settle();
    assert_eq!(sent(before), [false]);
    show(&model);
    assert!(switch.is_active(), "back to what the service says");
    assert_eq!(
        switch.subtitle().unwrap_or_default(),
        t(keys::GENERAL_RELATIVE_AUTO_FAILED)
    );
    assert!(switch.has_css_class("iw-failed"));
    show(&model);
    assert!(switch.has_css_class("iw-failed"), "still said");
    model.status.wake_on_push = false;
    show(&model);
    assert!(!switch.is_active());
    assert!(!switch.has_css_class("iw-failed"));
    assert_eq!(
        switch.subtitle().unwrap_or_default(),
        t(keys::GENERAL_WAKE_ON_PUSH_HINT)
    );

    // On Wayland a push never reaches the service: off limits, saying why.
    model.status.cursor_leaves_at_edges_only = true;
    show(&model);
    assert!(switch.is_visible() && !switch.is_sensitive());
    assert_eq!(
        switch.subtitle().unwrap_or_default(),
        t(keys::GENERAL_WAKE_ON_PUSH_EDGES_ONLY)
    );
    model.status.cursor_leaves_at_edges_only = false;
    show(&model);
    assert!(switch.is_sensitive());
    assert_eq!(
        switch.subtitle().unwrap_or_default(),
        t(keys::GENERAL_WAKE_ON_PUSH_HINT)
    );

    // A service before waking: no switch.
    model.status.wake_supported = false;
    show(&model);
    assert!(!switch.is_visible());
    window.destroy();
    ui.agent.show_fixed(sample::snapshot(SystemTime::now()));
}

fn entry_rows_are_left_alone_while_edited() {
    let row = adw::EntryRow::new();
    let other = gtk::Button::with_label("elsewhere");
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&row);
    column.append(&other);
    let window = window_with(&column);
    // A new window focuses its first entry; start with the focus elsewhere.
    other.grab_focus();
    settle();
    let sync = EntrySync::new(&row);
    sync.show("Mac mini");
    assert_eq!(row.text(), "Mac mini");

    // Focus is on the row's inner text, not the row: has_focus would not
    // tell, the edit guard does.
    row.grab_focus();
    settle();
    assert!(!row.has_focus(), "the row itself never has the focus");
    assert!(sync.editing(), "focus within the row");
    sync.show("Renamed elsewhere");
    assert_eq!(row.text(), "Mac mini", "not while the focus is in it");

    // Typed into and left without applying: the edit stays.
    row.set_text("Studio");
    other.grab_focus();
    settle();
    sync.show("Mac mini");
    assert_eq!(row.text(), "Studio");
    // Applied: the model's answer shows again.
    sync.edits().applied();
    sync.show("Studio Mac");
    assert_eq!(row.text(), "Studio Mac");
    window.destroy();
}

fn the_inspector_updates_in_place(ui: &Rc<Ui>) {
    let inspector = Inspector::new(ui);
    let window = window_with(inspector.widget());
    let mut model = sample::snapshot(SystemTime::now());
    inspector.update(&model, Some("mac"));
    settle();
    let status = inspector.status_row().expect("built");
    let name = inspector.name_row().expect("a name to edit");
    assert_eq!(status.subtitle().as_deref(), Some("Connected · 3 ms"));

    // The round trip changes: the same rows, updated.
    model.status.devices[0].rtt_ms = 9;
    inspector.update(&model, Some("mac"));
    let same = inspector.status_row().unwrap();
    assert_eq!(same, status, "not made again");
    assert_eq!(inspector.name_row().unwrap(), name);
    assert_eq!(status.subtitle().as_deref(), Some("Connected · 9 ms"));

    // Half a name typed survives the updates.
    name.set_text("Mac mi");
    inspector.update(&model, Some("mac"));
    assert_eq!(name.text(), "Mac mi");

    // The user turns the clipboard off; the command fails, so the model
    // still says on, and the next update shows that again.
    let clipboard = inspector.clipboard_row().unwrap();
    clipboard.set_active(false);
    settle();
    inspector.update(&model, Some("mac"));
    assert!(clipboard.is_active(), "back to what the model says");
    // And it follows the model when the model changes.
    model.status.devices[0].clipboard_off = true;
    inspector.update(&model, Some("mac"));
    assert!(!clipboard.is_active());

    // Another device: rows made again.
    model.status.devices[1].state = ConnectionState::Connected as i32;
    inspector.update(&model, Some("pc"));
    assert_ne!(inspector.status_row().unwrap(), status);
    window.destroy();
}

/// The cursor speed row shows the agent's speed, sends the one chosen as
/// the pointer lets go (never while it is held) or once the keys stop, goes
/// back to the agent's when refused, and is offered for another computer
/// whose agent knows speeds.
fn the_cursor_speed_row_sends_and_follows_the_agent(ui: &Rc<Ui>) {
    use inputweave_app_core::cursor_speed;
    use inputweave_app_core::proto::SetDeviceSpeed;
    ui.agent.record_fixed();
    let inspector = Inspector::new(ui);
    // Shorter than the inspector, so its page scrolls.
    let window = adw::Window::builder()
        .content(inspector.widget())
        .title("cursor speed test")
        .default_width(600)
        .default_height(600)
        .build();
    window.present();
    settle();
    let mut model = sample::snapshot(SystemTime::now());
    model.status.devices[0].speed_percent = 150;
    inspector.update(&model, Some("mac"));
    settle();
    let scale = inspector.speed_scale().expect("a speed for the Mac mini");
    assert_eq!(scale.value(), 150.0);
    assert_eq!(inspector.speed_value(), Some(("150%".to_owned(), true)));
    let speed = |percent| {
        Command::SetDeviceSpeed(SetDeviceSpeed {
            device_id: "mac".into(),
            percent,
        })
    };

    // Moved by the keys: in steps of 10, and sent once they stop.
    let before = ui.agent.sent().len();
    scale.emit_by_name::<bool>("change-value", &[&gtk::ScrollType::Jump, &203.0]);
    assert_eq!(scale.value(), 200.0);
    assert_eq!(inspector.speed_value(), Some(("200%".to_owned(), true)));
    assert_eq!(ui.agent.sent().len(), before, "not while it moves");
    settle();
    settle();
    assert_eq!(ui.agent.sent()[before..], [speed(200)]);
    model.status.devices[0].speed_percent = 200;
    inspector.update(&model, Some("mac"));
    assert_eq!(scale.value(), 200.0);

    // Dragged: nothing goes while it is held, however long it rests there;
    // it goes as the pointer lets go.
    let before = ui.agent.sent().len();
    inspector.speed_pointer(true);
    scale.emit_by_name::<bool>("change-value", &[&gtk::ScrollType::Jump, &250.0]);
    settle();
    settle();
    inspector.update(&model, Some("mac"));
    assert_eq!(inspector.speed_value(), Some(("250%".to_owned(), true)));
    assert_eq!(ui.agent.sent().len(), before, "not while it is held");
    inspector.speed_pointer(false);
    settle();
    assert_eq!(ui.agent.sent()[before..], [speed(250)]);
    model.status.devices[0].speed_percent = 250;
    inspector.update(&model, Some("mac"));
    assert_eq!(scale.value(), 250.0);

    // A real drag on the X server: the scale's own gestures leave the
    // press and the release to be seen.
    let speeds = |from: usize| -> Vec<u32> {
        ui.agent.sent()[from..]
            .iter()
            .filter_map(|c| match c {
                Command::SetDeviceSpeed(s) => Some(s.percent),
                _ => None,
            })
            .collect()
    };
    let (x0, x1, y) = drag_points(&window, &scale);
    let before = ui.agent.sent().len();
    xdotool(&[
        "mousemove",
        &x0.to_string(),
        &y.to_string(),
        "mousedown",
        "1",
    ]);
    settle();
    xdotool(&["mousemove", &x1.to_string(), &y.to_string()]);
    settle();
    settle();
    assert!(speeds(before).is_empty(), "not while it is held");
    let dragged = cursor_speed::snapped(scale.value());
    assert!(dragged > 250, "dragged to {dragged}");
    xdotool(&["mouseup", "1"]);
    settle();
    assert_eq!(speeds(before), [dragged]);
    model.status.devices[0].speed_percent = dragged;
    inspector.update(&model, Some("mac"));
    // Back to 250 the same way, for what follows.
    let before = ui.agent.sent().len();
    let (x250, _, _) = drag_points(&window, &scale);
    xdotool(&["mousemove", &x250.to_string(), &y.to_string(), "click", "1"]);
    settle();
    assert_eq!(speeds(before), [cursor_speed::snapped(scale.value())]);
    model.status.devices[0].speed_percent = cursor_speed::snapped(scale.value());
    inspector.update(&model, Some("mac"));
    scale.emit_by_name::<bool>("change-value", &[&gtk::ScrollType::Jump, &250.0]);
    settle();
    settle();
    model.status.devices[0].speed_percent = 250;
    inspector.update(&model, Some("mac"));
    assert_eq!(scale.value(), 250.0);

    // The wheel over the slider scrolls the page and leaves the speed.
    let page = scale
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast::<gtk::ScrolledWindow>()
        .expect("the page scrolls")
        .vadjustment();
    assert!(
        page.upper() > page.page_size(),
        "the page is taller than the window"
    );
    let (x, _, y) = drag_points(&window, &scale);
    let before = ui.agent.sent().len();
    let top = page.value();
    xdotool(&["mousemove", &x.to_string(), &y.to_string()]);
    settle();
    xdotool(&["click", "--repeat", "3", "5"]);
    settle();
    settle();
    assert_eq!(scale.value(), 250.0, "the wheel left the speed");
    assert!(speeds(before).is_empty(), "and sent nothing");
    assert!(page.value() > top, "the page scrolled");
    page.set_value(top);
    settle();

    // Refused: back to what the agent has.
    ui.agent.refuse_next("invalid_speed");
    scale.emit_by_name::<bool>("change-value", &[&gtk::ScrollType::Jump, &120.0]);
    assert_eq!(inspector.speed_value(), Some(("120%".to_owned(), true)));
    settle();
    settle();
    inspector.update(&model, Some("mac"));
    assert_eq!(scale.value(), 250.0);
    assert_eq!(inspector.speed_value(), Some(("250%".to_owned(), true)));

    // Reset goes at once.
    let before = ui.agent.sent().len();
    inspector.speed_reset().unwrap().emit_clicked();
    assert_eq!(inspector.speed_value(), Some(("100%".to_owned(), false)));
    settle();
    assert_eq!(ui.agent.sent()[before..], [speed(100)]);
    model.status.devices[0].speed_percent = 100;
    inspector.update(&model, Some("mac"));
    assert_eq!(inspector.speed_value(), Some(("100%".to_owned(), false)));

    // Made again for the same computer (another screen came) while what
    // the keys chose waits: the new row shows it, and it goes once.
    let sent_speeds = |from: usize| -> Vec<(String, u32)> {
        ui.agent.sent()[from..]
            .iter()
            .filter_map(|c| match c {
                Command::SetDeviceSpeed(s) => Some((s.device_id.clone(), s.percent)),
                _ => None,
            })
            .collect()
    };
    let before = ui.agent.sent().len();
    scale.emit_by_name::<bool>("change-value", &[&gtk::ScrollType::Jump, &140.0]);
    let mut rebuilt = model.clone();
    let mac = std::sync::Arc::make_mut(&mut rebuilt.arrangement)
        .devices
        .iter_mut()
        .find(|d| d.id == "mac")
        .expect("the Mac mini is arranged");
    let mut screen = mac.screens[0].clone();
    screen.id = "second".into();
    mac.screens.push(screen);
    inspector.update(&rebuilt, Some("mac"));
    let scale = inspector.speed_scale().expect("made again");
    assert_eq!(inspector.speed_value(), Some(("140%".to_owned(), true)));
    settle();
    settle();
    assert_eq!(sent_speeds(before), [("mac".to_owned(), 140)]);
    rebuilt.status.devices[0].speed_percent = 140;
    inspector.update(&rebuilt, Some("mac"));

    // Made again while the slider is held: that ends the drag, and what it
    // was held at goes once; letting go of the new slider sends nothing more.
    let before = ui.agent.sent().len();
    inspector.speed_pointer(true);
    scale.emit_by_name::<bool>("change-value", &[&gtk::ScrollType::Jump, &170.0]);
    let mac = std::sync::Arc::make_mut(&mut rebuilt.arrangement)
        .devices
        .iter_mut()
        .find(|d| d.id == "mac")
        .unwrap();
    let mut screen = mac.screens[0].clone();
    screen.id = "third".into();
    mac.screens.push(screen);
    inspector.update(&rebuilt, Some("mac"));
    assert_eq!(sent_speeds(before), [("mac".to_owned(), 170)]);
    let scale = inspector.speed_scale().expect("made again");
    inspector.speed_pointer(false);
    settle();
    assert_eq!(sent_speeds(before), [("mac".to_owned(), 170)]);
    rebuilt.status.devices[0].speed_percent = 170;
    inspector.update(&rebuilt, Some("mac"));

    // Another computer shown while a choice waits: it goes at once, for the
    // computer it was chosen for.
    let before = ui.agent.sent().len();
    scale.emit_by_name::<bool>("change-value", &[&gtk::ScrollType::Jump, &160.0]);
    inspector.update(&rebuilt, Some("pc"));
    assert_eq!(sent_speeds(before), [("mac".to_owned(), 160)]);
    settle();
    settle();
    assert_eq!(sent_speeds(before), [("mac".to_owned(), 160)]);

    // Unpaired while a choice waits, it goes nowhere; paired again, the
    // agent's speed shows.
    inspector.update(&rebuilt, Some("mac"));
    let scale = inspector.speed_scale().unwrap();
    let before = ui.agent.sent().len();
    scale.emit_by_name::<bool>("change-value", &[&gtk::ScrollType::Jump, &210.0]);
    let mut unpaired = rebuilt.clone();
    unpaired.status.devices.retain(|d| d.id != "mac");
    inspector.update(&unpaired, Some("pc"));
    settle();
    settle();
    assert_eq!(sent_speeds(before), []);
    inspector.update(&rebuilt, Some("mac"));
    assert_eq!(inspector.speed_value(), Some(("170%".to_owned(), true)));

    // A computer in the configuration file has one too; an agent from
    // before speeds and this computer have none.
    model.status.devices[0].paired = false;
    inspector.update(&model, Some("mac"));
    assert!(inspector.speed_scale().is_some());
    model.status.devices[0].speed_percent = 0;
    inspector.update(&model, Some("mac"));
    assert!(inspector.speed_scale().is_none());
    model.status.devices[0].speed_percent = 100;
    inspector.update(&model, Some("me"));
    assert!(inspector.speed_scale().is_none());
    window.destroy();
}

/// Runs xdotool on this display.
fn xdotool(args: &[&str]) -> String {
    let output = std::process::Command::new("xdotool")
        .args(args)
        .output()
        .expect("xdotool, as in the Linux container");
    assert!(output.status.success(), "xdotool {args:?}: {output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Where on the screen the scale's 150% and 280% are, and its middle's y:
/// the window's place from the X server, the scale's within it from GTK.
fn drag_points(window: &adw::Window, scale: &gtk::Scale) -> (i32, i32, i32) {
    let title = window.title().expect("a title to find it by");
    let found = xdotool(&["search", "--sync", "--onlyvisible", "--name", &title]);
    let xid = found.lines().last().expect("the window").to_owned();
    let geometry = xdotool(&["getwindowgeometry", "--shell", &xid]);
    let field = |name: &str| -> i32 {
        geometry
            .lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix('='))
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("no {name} in {geometry}"))
    };
    let bounds = scale.compute_bounds(window).expect("the scale is shown");
    // The slider's middle keeps about its half width from either end.
    let inset = 10.0;
    let at = |percent: f32| {
        let fraction = (percent - 50.0) / 250.0;
        field("X") + (bounds.x() + inset + (bounds.width() - 2.0 * inset) * fraction) as i32
    };
    let y = field("Y") + (bounds.y() + bounds.height() / 2.0) as i32;
    (at(150.0), at(280.0), y)
}

fn pairing_codes_spread_and_wrong_ones_clear(ui: &Rc<Ui>) {
    let add = AddDevice::new(ui);
    add.begin_for_tests(ui);
    let mut model = sample::snapshot(SystemTime::now());
    model.pairing = sample::entering_code();
    // As the agent says: the code is asked for.
    ui.agent.show_fixed(model.clone());
    add.show_for_tests(&model);
    let entries = add.digit_entries().to_vec();
    let texts = || {
        entries
            .iter()
            .map(|e| e.text().to_string())
            .collect::<Vec<_>>()
    };

    // A whole code pasted into the first box fills them all.
    entries[0].set_text("482 913");
    assert_eq!(texts(), ["4", "8", "2", "9", "1", "3"]);
    settle();

    // The agent asks again with fewer tries left: wrong, so they clear.
    model.pairing = PairingState {
        attempts_left: 2,
        error: "wrong_code".into(),
        ..sample::entering_code()
    };
    add.show_for_tests(&model);
    assert_eq!(texts(), ["", "", "", "", "", ""]);

    // Typed one by one.
    for (entry, digit) in entries.iter().zip(["1", "2", "3"]) {
        entry.set_text(digit);
    }
    assert_eq!(texts(), ["1", "2", "3", "", "", ""]);
    // The same state again is no new wrong code.
    add.show_for_tests(&model);
    assert_eq!(texts(), ["1", "2", "3", "", "", ""]);
    ui.agent.show_fixed(sample::snapshot(SystemTime::now()));
}

fn a_wrong_code_gives_the_focus_back(ui: &Rc<Ui>) {
    let window = adw::Window::builder()
        .default_width(600)
        .default_height(600)
        .build();
    window.present();
    AddDevice::present(ui, &window.clone().upcast());
    settle();
    let add = ui.pairing.add_dialog().expect("presented");
    add.begin_for_tests(ui);
    let mut model = sample::snapshot(SystemTime::now());
    model.pairing = sample::entering_code();
    add.show_for_tests(&model);
    settle();
    assert_eq!(add.page_shown().as_deref(), Some("code"));
    let entries = add.digit_entries().to_vec();
    // The agent still asks for the code, tries as before, when the code
    // gets no answer: it never arrived.
    ui.agent.show_fixed(model.clone());
    entries[0].set_text("482913");
    assert!(add.sending());
    // This test's agent never answers: the code stays, and says Enter
    // sends it again.
    settle();
    assert!(!add.sending());
    assert_eq!(entries[5].text(), "3", "the code stays");
    assert!(
        add.error_shown().is_some_and(|e| e.contains("Enter")),
        "{:?}",
        add.error_shown()
    );
    entries[5].emit_activate();
    assert!(add.sending(), "sent again on Enter");
    settle();
    // The agent checking it when no answer comes: it did arrive, so no
    // offer to send it again; the model goes on.
    let mut verifying = model.clone();
    verifying.pairing.phase = PairingPhase::Verifying as i32;
    ui.agent.show_fixed(verifying);
    entries[5].emit_activate();
    settle();
    assert_eq!(add.error_shown(), None, "not offered again while checking");
    ui.agent.show_fixed(sample::snapshot(SystemTime::now()));
    // Checking it: the boxes wait.
    model.pairing.phase = PairingPhase::Verifying as i32;
    add.show_for_tests(&model);
    assert!(!entries[0].is_sensitive());
    // Wrong: cleared, and the first box takes the focus again.
    model.pairing = PairingState {
        attempts_left: 2,
        error: "wrong_code".into(),
        ..sample::entering_code()
    };
    add.show_for_tests(&model);
    settle();
    assert!(entries.iter().all(|e| e.text().is_empty()));
    assert!(entries[0].is_sensitive());
    assert!(
        entries[0]
            .state_flags()
            .contains(gtk::StateFlags::FOCUS_WITHIN),
        "the focus is back in the first box"
    );
    // A code that got no answer again, then a new attempt: the offer to
    // send it again is the old attempt's, and goes.
    // (The tries as the dialog last saw them, and no wrong code shown.)
    let mut entering = sample::snapshot(SystemTime::now());
    entering.pairing = PairingState {
        attempts_left: 2,
        ..sample::entering_code()
    };
    ui.agent.show_fixed(entering);
    entries[0].set_text("482913");
    settle();
    assert!(add.error_shown().is_some_and(|e| e.contains("Enter")));
    ui.agent.show_fixed(sample::snapshot(SystemTime::now()));
    add.start_for_tests();
    assert_eq!(add.error_shown(), None, "a new attempt starts clean");
    settle();
    ui.pairing.close_add();
    window.destroy();
    settle();
}

fn a_new_attempt_is_not_the_last_ones(ui: &Rc<Ui>) {
    let add = AddDevice::new(ui);
    let mut model = sample::snapshot(SystemTime::now());
    // The agent still reports how the last attempt ended.
    model.pairing = PairingState {
        phase: PairingPhase::Failed as i32,
        error: "rejected".into(),
        ..Default::default()
    };
    add.show_for_tests(&model);
    assert_eq!(add.page_shown().as_deref(), Some("list"));
    add.start_for_tests();
    add.show_for_tests(&model);
    assert_eq!(
        add.page_shown().as_deref(),
        Some("connecting"),
        "not the last attempt's failure"
    );
    // This test's agent never answers it: back to the list.
    settle();
    assert_eq!(add.page_shown().as_deref(), Some("list"));
}

fn a_closed_code_window_comes_back(ui: &Rc<Ui>) {
    let mut model = sample::snapshot(SystemTime::now());
    model.pairing = sample::showing_code(SystemTime::now());
    // Not while the agent cannot be reached.
    let offline = Snapshot {
        link: Link::Connecting,
        ..model.clone()
    };
    ui.pairing.update(ui, &offline);
    assert!(ui.pairing.code_window().is_none());
    ui.pairing.update(ui, &model);
    settle();
    let first = ui.pairing.code_window().expect("shown");
    first.close();
    settle();
    assert!(ui.pairing.code_window().is_none(), "let go of once closed");
    // The agent still shows that code until the decline reaches it: not
    // shown again.
    ui.pairing.update(ui, &model);
    assert!(ui.pairing.code_window().is_none(), "declined");
    // Another code: shown.
    model.pairing.code = "135790".into();
    ui.pairing.update(ui, &model);
    settle();
    let second = ui.pairing.code_window().expect("shown for a new code");
    assert_ne!(second, first);
    // Done showing it: gone.
    let showing = model.pairing.clone();
    model.pairing = PairingState {
        phase: PairingPhase::Idle as i32,
        ..Default::default()
    };
    ui.pairing.update(ui, &model);
    assert!(ui.pairing.code_window().is_none());
    // A later pairing may use the declined code again: shown.
    model.pairing = PairingState {
        code: "482913".into(),
        ..showing
    };
    ui.pairing.update(ui, &model);
    assert!(ui.pairing.code_window().is_some());
    model.pairing = PairingState::default();
    ui.pairing.update(ui, &model);
}
