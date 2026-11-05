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
use crate::pages::shortcuts::Shortcuts;
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

    // Refused as a duplicate of taking back control: said so.
    ui.agent.refuse_next("duplicate");
    shortcuts.record_row(Row::Lock);
    shortcuts.press_for_tests(Row::Lock, escape, control | alt | shift);
    settle();
    let error = shortcuts.error_shown().expect("a refusal is said");
    assert!(
        error.contains(&crate::strings::t(
            inputweave_app_core::text::keys::SHORTCUT_TAKE_BACK
        )),
        "{error}"
    );
    window.destroy();
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
