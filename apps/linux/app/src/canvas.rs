//! The arrangement, as the other two apps
//! draw it: every computer as a group of its screens at their real
//! relative sizes on the dotted canvas, with its desktop picture under a
//! veil of the tile color, its name and the shortcut that jumps there, and
//! the stitched seams where computers meet; the keyboard-and-mouse badge on
//! the one they are on. The user drags whole computers, which snap to each
//! other's edges and spring back if they would overlap; arrow keys nudge
//! the selected one and Ctrl+Z undoes a move; the right-click menu moves
//! the cursor there, renames another computer and wakes one asleep. Files
//! dropped on a computer that takes them go to it. The geometry is
//! `inputweave_app_core::canvas`; this draws it with cairo.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{cairo, gdk, gio, glib, pango};
use inputweave_app_core::canvas::{self, Device, Fit, Point, Rect, Size};
use inputweave_app_core::desktop_picture::PictureCache;
use inputweave_app_core::proto::{
    DevicePosition, JumpTo, Screen, SetArrangement, client_message::Command,
};
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;
use inputweave_app_core::{device_text, log, wake};

use crate::strings::{t, tr};
use crate::style::{self, Brand};
use crate::ui::Ui;

/// Snap distance in view units.
const SNAP_DISTANCE: f64 = 8.0;
/// Arrow keys move a computer this many view units.
const NUDGE: f64 = 10.0;
/// The drag starts once the pointer has moved this far.
const DRAG_START: f64 = 2.0;
const PADDING: f64 = 40.0;
/// The most a logical unit is drawn as, so one computer alone does not
/// fill the canvas: a 1920-wide screen at most 384 wide.
const MAX_SCALE: f64 = 0.2;
const OFFLINE_OPACITY: f64 = 0.55;
const NAME_INSET: f64 = 6.0;
const NAME_HEIGHT: f64 = 20.0;
const NAME_PADDING: f64 = 7.0;
/// The least the name keeps before the jump shortcut beside it is left out.
const LEAST_NAME: f64 = 40.0;
/// The moon after the name of a computer asleep, square.
const MOON_SIZE: f64 = 10.0;
const SPRING_TIME: Duration = Duration::from_millis(200);
const SHAKE_TIME: Duration = Duration::from_millis(300);
const SEW_TIME: Duration = Duration::from_millis(240);
/// The badge grows to this and back when the service shows where the
/// cursor is.
const PULSE_SCALE: f64 = 1.15;
const PULSE_TIME: Duration = Duration::from_millis(400);

/// What goes after a computer's name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NameGlyph {
    /// Its screen is locked.
    Padlock,
    /// It is asleep and can be woken.
    Moon,
}

struct Drag {
    id: String,
    start: Point,
    offset: Point,
    fit: Fit,
    moving: bool,
    /// Released and sent to the service; shown where it was dropped until
    /// it answers.
    committed: bool,
}

struct Spring {
    id: String,
    from: Point,
    to: Point,
    start: Instant,
    fit: Fit,
}

#[derive(Default)]
struct State {
    model: Option<Arc<Snapshot>>,
    selection: Option<String>,
    drag: Option<Drag>,
    spring: Option<Spring>,
    shake: Option<(String, Instant)>,
    undo: Vec<(String, Point)>,
    /// When each seam, by its place in the list, started being sewn.
    sewn: Vec<Option<Instant>>,
    drawn_seams: bool,
    dragging_files: bool,
    drop_target: Option<String>,
    fit: Option<Fit>,
    pictures: PictureCache<cairo::ImageSurface>,
    /// The cursor sighting the badge last pulsed for; the first one seen
    /// is only remembered.
    pulsed_for: Option<u64>,
    pulse_start: Option<Instant>,
}

pub struct ArrangementCanvas {
    area: gtk::DrawingArea,
    state: RefCell<State>,
    ticking: Cell<bool>,
    menu: gtk::PopoverMenu,
    on_selected: RefCell<Option<Handler<Option<String>>>>,
    on_rename: RefCell<Option<Handler<String>>>,
    ui: Weak<Ui>,
    /// The moon as last drawn, for the scale and color it was drawn at:
    /// looked up in the icon theme only when either changes.
    moon_node: RefCell<Option<(MoonKey, gtk::gsk::RenderNode)>>,
}

/// What the moon is drawn for: the scale factor and the text color.
type MoonKey = (i32, [f32; 4]);

/// libadwaita 1.6's StyleManager property with the user's accent color.
const ACCENT_PROPERTY: &str = "accent-color-rgba";

/// What the page beside the canvas hears from it.
type Handler<T> = Box<dyn Fn(T)>;

fn reduce_motion() -> bool {
    !gtk::Settings::default().is_some_and(|s| s.is_gtk_enable_animations())
}

fn high_contrast() -> bool {
    adw::StyleManager::default().is_high_contrast()
}

impl ArrangementCanvas {
    pub fn new(ui: &Rc<Ui>) -> Rc<ArrangementCanvas> {
        let area = gtk::DrawingArea::builder()
            .hexpand(true)
            .vexpand(true)
            .height_request(280)
            .focusable(true)
            .can_focus(true)
            .build();
        area.update_property(&[gtk::accessible::Property::Label(&t(keys::CANVAS_LABEL))]);
        let menu = gtk::PopoverMenu::from_model(None::<&gio::MenuModel>);
        menu.set_parent(&area);
        menu.set_has_arrow(false);
        // A widget's popover goes before the widget does, or GTK warns
        // and keeps it: as the setup window closes with its canvas.
        let popover = menu.downgrade();
        area.connect_destroy(move |_| {
            if let Some(menu) = popover.upgrade() {
                menu.unparent();
            }
        });
        let canvas = Rc::new(ArrangementCanvas {
            area,
            state: RefCell::default(),
            ticking: Cell::new(false),
            menu,
            on_selected: RefCell::new(None),
            on_rename: RefCell::new(None),
            ui: Rc::downgrade(ui),
            moon_node: RefCell::new(None),
        });
        canvas.connect();
        canvas
    }

    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    /// The selected computer changed by a click on the canvas.
    pub fn connect_selected(&self, f: impl Fn(Option<String>) + 'static) {
        *self.on_selected.borrow_mut() = Some(Box::new(f));
    }

    /// "Rename…" was chosen for a computer, now selected.
    pub fn connect_rename(&self, f: impl Fn(String) + 'static) {
        *self.on_rename.borrow_mut() = Some(Box::new(f));
    }

    pub fn selection(&self) -> Option<String> {
        self.state.borrow().selection.clone()
    }

    /// Selects a computer, as the list beside the canvas does.
    pub fn select(&self, id: Option<String>) {
        let mut state = self.state.borrow_mut();
        if state.selection != id {
            state.selection = id;
            drop(state);
            self.area.queue_draw();
        }
    }

    /// Shows what the service reports now.
    pub fn update(self: &Rc<Self>, model: &Arc<Snapshot>) {
        let mut state = self.state.borrow_mut();
        if state.model.as_ref().is_some_and(|m| Arc::ptr_eq(m, model)) {
            return;
        }
        let sighting = model.cursor_located;
        match state.pulsed_for {
            None => state.pulsed_for = Some(sighting),
            Some(seen) if seen != sighting => {
                state.pulsed_for = Some(sighting);
                if !reduce_motion() {
                    state.pulse_start = Some(Instant::now());
                }
            }
            _ => {}
        }
        // A computer no longer there is no longer selected.
        if let Some(selected) = &state.selection
            && !model.arrangement.devices.iter().any(|d| &d.id == selected)
        {
            state.selection = None;
        }
        state.model = Some(model.clone());
        drop(state);
        self.animate_if_needed();
        self.area.queue_draw();
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.area.set_draw_func(move |_, cr, width, height| {
            if let Some(canvas) = weak.upgrade()
                && let Err(e) = canvas.draw(cr, f64::from(width), f64::from(height))
            {
                log::info(&format!("cannot draw the arrangement: {e}"));
            }
        });

        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        let weak = Rc::downgrade(self);
        drag.connect_drag_begin(move |_, x, y| {
            if let Some(canvas) = weak.upgrade() {
                canvas.area.grab_focus();
                canvas.press(x, y);
            }
        });
        let weak = Rc::downgrade(self);
        drag.connect_drag_update(move |_, dx, dy| {
            if let Some(canvas) = weak.upgrade() {
                canvas.drag_to(dx, dy);
            }
        });
        let weak = Rc::downgrade(self);
        drag.connect_drag_end(move |_, _, _| {
            if let Some(canvas) = weak.upgrade() {
                canvas.release();
            }
        });
        self.area.add_controller(drag);

        let secondary = gtk::GestureClick::new();
        secondary.set_button(gdk::BUTTON_SECONDARY);
        let weak = Rc::downgrade(self);
        secondary.connect_pressed(move |_, _, x, y| {
            if let Some(canvas) = weak.upgrade() {
                canvas.context_menu(x, y);
            }
        });
        self.area.add_controller(secondary);

        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| match weak.upgrade() {
            Some(canvas) if canvas.key(key, modifiers) => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        self.area.add_controller(keys);

        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let weak = Rc::downgrade(self);
        drop.connect_motion(move |_, x, y| match weak.upgrade() {
            Some(canvas) => canvas.files_over(x, y),
            None => gdk::DragAction::empty(),
        });
        let weak = Rc::downgrade(self);
        drop.connect_leave(move |_| {
            if let Some(canvas) = weak.upgrade() {
                canvas.end_file_drag();
            }
        });
        let weak = Rc::downgrade(self);
        drop.connect_drop(move |_, value, x, y| match weak.upgrade() {
            Some(canvas) => canvas.files_dropped(value, x, y),
            None => false,
        });
        self.area.add_controller(drop);

        let actions = gio::SimpleActionGroup::new();
        let jump = gio::SimpleAction::new("jump", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        jump.connect_activate(move |_, id| {
            if let (Some(canvas), Some(id)) =
                (weak.upgrade(), id.and_then(|v| v.str().map(str::to_owned)))
            {
                canvas.jump(id);
            }
        });
        actions.add_action(&jump);
        let rename = gio::SimpleAction::new("rename", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        rename.connect_activate(move |_, id| {
            if let (Some(canvas), Some(id)) =
                (weak.upgrade(), id.and_then(|v| v.str().map(str::to_owned)))
                && let Some(f) = canvas.on_rename.borrow().as_ref()
            {
                f(id);
            }
        });
        actions.add_action(&rename);
        let wake = gio::SimpleAction::new("wake", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        wake.connect_activate(move |_, id| {
            if let (Some(canvas), Some(id)) =
                (weak.upgrade(), id.and_then(|v| v.str().map(str::to_owned)))
                && let Some(ui) = canvas.ui.upgrade()
            {
                ui.wake_device(&id);
            }
        });
        actions.add_action(&wake);
        self.area.insert_action_group("canvas", Some(&actions));

        // Redraw for the theme: the brand colors and high contrast.
        let manager = adw::StyleManager::default();
        let redraw = |canvas: &Rc<Self>| {
            let weak = Rc::downgrade(canvas);
            move || {
                if let Some(canvas) = weak.upgrade() {
                    canvas.area.queue_draw();
                }
            }
        };
        let mut handlers = Vec::new();
        let dark = redraw(self);
        handlers.push(manager.connect_dark_notify(move |_| dark()));
        let contrast = redraw(self);
        handlers.push(manager.connect_high_contrast_notify(move |_| contrast()));
        // The accent the selection is drawn in, where libadwaita has one.
        if manager.find_property(ACCENT_PROPERTY).is_some() {
            let accent = redraw(self);
            handlers
                .push(manager.connect_notify_local(Some(ACCENT_PROPERTY), move |_, _| accent()));
        }
        // The style manager outlives every canvas (setup makes one each time
        // it opens): let go of it with the drawing area.
        let handlers = RefCell::new(handlers);
        self.area.connect_destroy(move |_| {
            for handler in handlers.take() {
                manager.disconnect(handler);
            }
        });
    }

    // Where things are

    /// The computers as they are now, with the one being dragged or
    /// springing back where it is on screen.
    fn devices(state: &State) -> Vec<Device> {
        let Some(model) = &state.model else {
            return Vec::new();
        };
        canvas::devices(&model.arrangement)
            .into_iter()
            .map(|d| match (&state.drag, &state.spring) {
                (Some(g), _) if g.id == d.id => d.moved_to(g.offset),
                (_, Some(s)) if s.id == d.id => d.moved_to(spring_offset(s)),
                _ => d,
            })
            .collect()
    }

    fn hit(&self, x: f64, y: f64, slop: f64) -> Option<String> {
        let state = self.state.borrow();
        let fit = state.fit?;
        canvas::hit_test(&Self::devices(&state), &fit, Point::new(x, y), slop)
    }

    // Moving computers

    fn press(&self, x: f64, y: f64) {
        let Some(id) = self.hit(x, y, 0.0) else {
            return;
        };
        self.set_selection(Some(id.clone()));
        let mut state = self.state.borrow_mut();
        if state.drag.as_ref().is_some_and(|d| d.committed) || state.spring.is_some() {
            return;
        }
        let (Some(fit), Some(device)) = (
            state.fit,
            Self::devices(&state).into_iter().find(|d| d.id == id),
        ) else {
            return;
        };
        state.drag = Some(Drag {
            id,
            start: device.offset,
            offset: device.offset,
            fit,
            moving: false,
            committed: false,
        });
    }

    fn drag_to(&self, dx: f64, dy: f64) {
        let mut state = self.state.borrow_mut();
        let Some(model) = state.model.clone() else {
            return;
        };
        let Some(drag) = state.drag.as_mut().filter(|d| !d.committed) else {
            return;
        };
        if !drag.moving && dx.abs() < DRAG_START && dy.abs() < DRAG_START {
            return;
        }
        drag.moving = true;
        let all = canvas::devices(&model.arrangement);
        let Some(device) = all.iter().find(|d| d.id == drag.id) else {
            return;
        };
        let scale = drag.fit.scale;
        let moving = device.moved_to(Point::new(
            drag.start.x + dx / scale,
            drag.start.y + dy / scale,
        ));
        drag.offset = canvas::snap(
            &moving,
            all.iter().filter(|d| d.id != drag.id),
            SNAP_DISTANCE / scale,
        );
        drop(state);
        self.area.queue_draw();
    }

    fn release(self: &Rc<Self>) {
        let mut state = self.state.borrow_mut();
        let Some(drag) = state.drag.as_ref().filter(|d| !d.committed) else {
            return;
        };
        if !drag.moving {
            state.drag = None;
            return;
        }
        let (id, from, to, fit) = (drag.id.clone(), drag.start, drag.offset, drag.fit);
        drop(state);
        self.commit(id, from, to, fit, false);
    }

    /// Applies a move unless it makes screens overlap, in which case the
    /// computer springs back. The service shares the change with every
    /// connected computer.
    fn commit(self: &Rc<Self>, id: String, from: Point, to: Point, fit: Fit, undoing: bool) {
        let mut state = self.state.borrow_mut();
        let Some(model) = state.model.clone() else {
            state.drag = None;
            return;
        };
        let all = canvas::devices(&model.arrangement);
        let Some(device) = all.iter().find(|d| d.id == id) else {
            state.drag = None;
            drop(state);
            self.area.queue_draw();
            return;
        };
        if from == to {
            state.drag = None;
            drop(state);
            self.area.queue_draw();
            return;
        }
        let others: Vec<Rect> = all
            .iter()
            .filter(|d| d.id != id)
            .flat_map(Device::frames)
            .collect();
        if canvas::overlaps(&device.moved_to(to).frames(), &others) {
            drop(state);
            self.spring_back(&id, to, from, fit);
            self.flag(&id);
            return;
        }
        // Shown where it was dropped until the service answers.
        state.drag = Some(Drag {
            id: id.clone(),
            start: from,
            offset: to,
            fit,
            moving: true,
            committed: true,
        });
        drop(state);
        self.area.queue_draw();
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        let moved = id.clone();
        ui.command_then(
            Command::SetArrangement(SetArrangement {
                positions: vec![DevicePosition {
                    device_id: id,
                    x: to.x,
                    y: to.y,
                }],
            }),
            move |result| {
                let Some(canvas) = weak.upgrade() else {
                    return true;
                };
                match result {
                    Ok(_) => {
                        let mut state = canvas.state.borrow_mut();
                        if !undoing {
                            state.undo.push((moved, from));
                        }
                        state.drag = None;
                        drop(state);
                        canvas.area.queue_draw();
                    }
                    Err(e) => {
                        log::info(&format!("move {moved}: {e}"));
                        canvas.spring_back(&moved, to, from, fit);
                        canvas.flag(&moved);
                    }
                }
                true
            },
        );
    }

    fn spring_back(self: &Rc<Self>, id: &str, from: Point, to: Point, fit: Fit) {
        let mut state = self.state.borrow_mut();
        state.drag = None;
        state.spring = (!reduce_motion() && from != to).then(|| Spring {
            id: id.to_owned(),
            from,
            to,
            start: Instant::now(),
            fit,
        });
        drop(state);
        self.animate_if_needed();
        self.area.queue_draw();
    }

    /// A short horizontal shake, for a refused move or jump.
    fn flag(self: &Rc<Self>, id: &str) {
        if reduce_motion() {
            return;
        }
        self.state.borrow_mut().shake = Some((id.to_owned(), Instant::now()));
        self.animate_if_needed();
    }

    fn key(self: &Rc<Self>, key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
        let control = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
        let state = self.state.borrow();
        let (Some(model), Some(fit)) = (state.model.clone(), state.fit) else {
            return false;
        };
        let busy = state.drag.is_some() || state.spring.is_some();
        if control && (key == gdk::Key::z || key == gdk::Key::Z) {
            drop(state);
            if busy {
                return true;
            }
            let last = self.state.borrow_mut().undo.pop();
            if let Some((id, from)) = last
                && let Some(moved) = canvas::devices(&model.arrangement)
                    .into_iter()
                    .find(|d| d.id == id)
            {
                self.commit(id, moved.offset, from, fit, true);
            }
            return true;
        }
        let step = NUDGE / fit.scale;
        let delta = match key {
            gdk::Key::Left => (-step, 0.0),
            gdk::Key::Right => (step, 0.0),
            gdk::Key::Up => (0.0, -step),
            gdk::Key::Down => (0.0, step),
            _ => return false,
        };
        let Some(id) = state.selection.clone() else {
            return false;
        };
        drop(state);
        if busy {
            return true;
        }
        if let Some(device) = canvas::devices(&model.arrangement)
            .into_iter()
            .find(|d| d.id == id)
        {
            self.commit(
                id,
                device.offset,
                device.offset.offset(delta.0, delta.1),
                fit,
                false,
            );
        }
        true
    }

    fn set_selection(&self, id: Option<String>) {
        let changed = {
            let mut state = self.state.borrow_mut();
            let changed = state.selection != id;
            state.selection = id.clone();
            changed
        };
        if changed {
            self.area.queue_draw();
            if let Some(f) = self.on_selected.borrow().as_ref() {
                f(id);
            }
        }
    }

    // The right-click menu

    fn context_menu(&self, x: f64, y: f64) {
        let Some(id) = self.hit(x, y, 0.0) else {
            return;
        };
        self.set_selection(Some(id.clone()));
        let Some(model) = self.state.borrow().model.clone() else {
            return;
        };
        let menu = self.menu_for(&model, &id);
        self.menu.set_menu_model(Some(&menu));
        self.menu
            .set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        self.menu.popup();
    }

    /// The right-click menu of `id`: Move Cursor Here, Rename… for another
    /// computer, and below them Wake for one asleep that can be woken (off
    /// while it is being woken).
    fn menu_for(&self, model: &Snapshot, id: &str) -> gio::Menu {
        let menu = gio::Menu::new();
        let jump = gio::MenuItem::new(Some(&t(keys::CANVAS_MOVE_CURSOR_HERE)), None);
        jump.set_action_and_target_value(
            Some(if model.can_jump(id) {
                "canvas.jump"
            } else {
                "canvas.none"
            }),
            Some(&id.to_variant()),
        );
        menu.append_item(&jump);
        // Another computer this one trusts can be given a name where the
        // canvas has an inspector to type it in; this one is renamed in
        // General.
        if self.on_rename.borrow().is_some()
            && id != model.this_device_id()
            && model.device(id).is_some()
        {
            let rename = gio::MenuItem::new(Some(&t(keys::CANVAS_RENAME)), None);
            rename.set_action_and_target_value(Some("canvas.rename"), Some(&id.to_variant()));
            menu.append_item(&rename);
        }
        let in_flight = self.ui.upgrade().is_some_and(|ui| ui.waking_asked(id));
        if let Some(enabled) = wake::offer(model, id, in_flight) {
            let item = gio::MenuItem::new(Some(&t(keys::WAKE)), None);
            item.set_action_and_target_value(
                Some(if enabled {
                    "canvas.wake"
                } else {
                    "canvas.none"
                }),
                Some(&id.to_variant()),
            );
            // A section of its own: after a separator.
            let section = gio::Menu::new();
            section.append_item(&item);
            menu.append_section(None, &section);
        }
        menu
    }

    /// Moves the cursor to a computer. Refusals the window already
    /// explains (sharing paused, cursor locked) are left at that; others
    /// shake it, and one because the cursor leaves only across edges
    /// (Wayland) says so too.
    fn jump(self: &Rc<Self>, id: String) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        let target = id.clone();
        let toasts = Rc::downgrade(&ui);
        ui.command_then(Command::JumpTo(JumpTo { device_id: id }), move |result| {
            if let Err(e) = result
                && !matches!(e.code(), Some("sharing_paused" | "locked"))
                && let Some(canvas) = weak.upgrade()
            {
                canvas.flag(&target);
                if e.code() == Some("edge_only")
                    && let Some(ui) = toasts.upgrade()
                {
                    ui.toast(&t(keys::EDGES_ONLY));
                }
            }
            true
        });
    }

    // Sending files

    fn files_over(&self, x: f64, y: f64) -> gdk::DragAction {
        let receives = |id: &String| {
            self.state
                .borrow()
                .model
                .as_ref()
                .is_some_and(|m| m.connected() && m.can_receive_files(id))
        };
        let target = self.hit(x, y, 4.0).filter(receives);
        let mut state = self.state.borrow_mut();
        if !state.dragging_files || state.drop_target != target {
            state.dragging_files = true;
            state.drop_target = target.clone();
            drop(state);
            self.area.queue_draw();
        }
        if target.is_some() {
            gdk::DragAction::COPY
        } else {
            gdk::DragAction::empty()
        }
    }

    fn end_file_drag(&self) {
        let mut state = self.state.borrow_mut();
        state.dragging_files = false;
        state.drop_target = None;
        drop(state);
        self.area.queue_draw();
    }

    fn files_dropped(&self, value: &glib::Value, x: f64, y: f64) -> bool {
        let target = self.state.borrow().drop_target.clone();
        self.end_file_drag();
        let target = target.or_else(|| self.hit(x, y, 4.0));
        let (Some(target), Ok(list)) = (target, value.get::<gdk::FileList>()) else {
            return false;
        };
        let paths: Vec<String> = list
            .files()
            .iter()
            .filter_map(|f| f.path())
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let Some(ui) = self.ui.upgrade() else {
            return false;
        };
        if paths.is_empty() || !ui.snapshot().can_receive_files(&target) {
            return false;
        }
        let name = ui.snapshot().name_of(&target);
        crate::send::send(&ui, target, name, paths);
        true
    }

    // Animation: the spring back, the shake of a refused move, a seam
    // being sewn, the badge's pulse.

    fn animating(&self) -> bool {
        let state = self.state.borrow();
        let now = Instant::now();
        state
            .spring
            .as_ref()
            .is_some_and(|s| now - s.start < SPRING_TIME)
            || state
                .shake
                .as_ref()
                .is_some_and(|(_, s)| now - *s < SHAKE_TIME)
            || state.sewn.iter().flatten().any(|s| now - *s < SEW_TIME)
            || state.pulse_start.is_some_and(|s| now - s < PULSE_TIME)
    }

    fn animate_if_needed(self: &Rc<Self>) {
        if self.ticking.get() || !self.animating() {
            return;
        }
        self.ticking.set(true);
        let weak = Rc::downgrade(self);
        self.area.add_tick_callback(move |area, _| {
            let Some(canvas) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            {
                let mut state = canvas.state.borrow_mut();
                let now = Instant::now();
                if state
                    .spring
                    .as_ref()
                    .is_some_and(|s| now - s.start >= SPRING_TIME)
                {
                    state.spring = None;
                }
                if state
                    .shake
                    .as_ref()
                    .is_some_and(|(_, s)| now - *s >= SHAKE_TIME)
                {
                    state.shake = None;
                }
            }
            area.queue_draw();
            if canvas.animating() {
                glib::ControlFlow::Continue
            } else {
                canvas.ticking.set(false);
                glib::ControlFlow::Break
            }
        });
    }

    // Drawing

    fn draw(
        self: &Rc<Self>,
        cr: &cairo::Context,
        width: f64,
        height: f64,
    ) -> Result<(), cairo::Error> {
        let brand = style::brand();
        let contrast = high_contrast();
        // The canvas: a rounded card of the canvas color with its dots.
        rounded(cr, &Rect::new(0.0, 0.0, width, height), 12.0);
        set(cr, brand.canvas, 1.0);
        cr.fill_preserve()?;
        saved(cr, |cr| {
            cr.clip();
            set(cr, brand.grid, 1.0);
            let spacing = 16.0;
            let mut y = spacing / 2.0;
            while y < height {
                let mut x = spacing / 2.0;
                while x < width {
                    cr.new_sub_path();
                    cr.arc(x, y, 1.0, 0.0, std::f64::consts::TAU);
                    x += spacing;
                }
                y += spacing;
            }
            cr.fill()
        })?;

        let mut state = self.state.borrow_mut();
        let Some(model) = state.model.clone().filter(|m| m.connected()) else {
            return Ok(());
        };
        let devices = Self::devices(&state);
        let frames: Vec<Rect> = devices.iter().flat_map(Device::frames).collect();
        let Some(content) = canvas::union(&frames) else {
            return Ok(());
        };
        let fit = state
            .drag
            .as_ref()
            .map(|d| d.fit)
            .or_else(|| state.spring.as_ref().map(|s| s.fit))
            .unwrap_or_else(|| {
                canvas::fit_capped(&content, Size::new(width, height), PADDING, MAX_SCALE)
            });
        state.fit = Some(fit);

        for device in &devices {
            self.draw_device(cr, &mut state, &model, device, &brand, contrast, &fit)?;
        }

        // On top: the stitches cross both screens at the seam. A seam is
        // sewn shut when it appears, as when a computer snaps into place;
        // those there at the first drawing are simply there.
        let seams = canvas::seams(&devices);
        let now = Instant::now();
        let animate = state.drawn_seams && !reduce_motion();
        state.sewn.truncate(seams.len());
        while state.sewn.len() < seams.len() {
            state.sewn.push(animate.then_some(now));
        }
        state.drawn_seams = true;
        set(cr, brand.thread, 1.0);
        cr.set_line_width(if contrast { 3.0 } else { 2.0 });
        cr.set_line_join(cairo::LineJoin::Round);
        cr.set_line_cap(cairo::LineCap::Round);
        for (seam, started) in seams.iter().zip(&state.sewn) {
            let start = fit.to_view(seam.start);
            let end = fit.to_view(seam.end);
            let points = if contrast {
                vec![start, end]
            } else {
                canvas::stitch(start, end, 6.0, 3.5)
            };
            let progress = started.map_or(1.0, |s| {
                ((now - s).as_secs_f64() / SEW_TIME.as_secs_f64()).clamp(0.0, 1.0)
            });
            let points = canvas::sewn(&points, progress);
            if let Some((first, rest)) = points.split_first() {
                cr.move_to(first.x, first.y);
                for p in rest {
                    cr.line_to(p.x, p.y);
                }
                cr.stroke()?;
            }
        }

        // Above the seams too, so nothing covers it.
        if !state.dragging_files {
            self.draw_badge(cr, &state, &model, &devices, &brand, &fit)?;
        }
        drop(state);
        self.animate_if_needed();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_device(
        &self,
        cr: &cairo::Context,
        state: &mut State,
        model: &Snapshot,
        device: &Device,
        brand: &Brand,
        contrast: bool,
        fit: &Fit,
    ) -> Result<(), cairo::Error> {
        let info = model.arranged(&device.id);
        let online = info.is_some_and(|d| d.connected || d.this_device);
        let selected = state.selection.as_deref() == Some(device.id.as_str());
        let active = model.controlling_other() && model.status.active_device_id == device.id;
        let bounds = fit.rect_to_view(&device.bounds());
        let receives = model.can_receive_files(&device.id);
        let target = state.drop_target.as_deref() == Some(device.id.as_str());
        let shake = state
            .shake
            .as_ref()
            .filter(|(id, _)| *id == device.id)
            .map_or(0.0, |(_, start)| shake_offset(*start));

        saved(cr, |cr| {
            cr.translate(shake, 0.0);
            if state.dragging_files && target {
                // The computer the files would go to grows a little.
                cr.translate(bounds.mid_x(), bounds.mid_y());
                cr.scale(1.06, 1.06);
                cr.translate(-bounds.mid_x(), -bounds.mid_y());
            }
            let dimmed = !online || (state.dragging_files && !receives);
            grouped(cr, |cr| {
                let tiles: Vec<Rect> = device
                    .frames()
                    .iter()
                    .map(|f| fit.rect_to_view(f))
                    .collect();
                for (i, tile) in tiles.iter().enumerate() {
                    let screen = info.and_then(|d| d.screens.get(i));
                    self.draw_tile(cr, state, &device.id, screen, tile, brand, contrast)?;
                }
                let name = info.map_or_else(|| device.id.clone(), |d| d.name.clone());
                let jump = info.and_then(|d| model.jump_shortcut(d.jump_slot));
                let glyph = if model.device(&device.id).is_some_and(|d| d.screen_locked) {
                    Some(NameGlyph::Padlock)
                } else if wake::shows_moon(model, &device.id) {
                    Some(NameGlyph::Moon)
                } else {
                    None
                };
                if let Some(tile) = tiles.get(device.main_screen) {
                    self.draw_name(cr, &name, jump.as_deref(), glyph, tile, brand)?;
                }
                Ok(())
            })?;
            let opacity = if state.dragging_files && !receives {
                0.35
            } else if dimmed {
                OFFLINE_OPACITY
            } else {
                1.0
            };
            cr.paint_with_alpha(opacity)?;

            // The outline: selected in the accent color, the computer being
            // controlled in the thread color, offline dashed.
            if selected || active || !online || (state.dragging_files && target) {
                rounded(cr, &bounds.inflate(3.0, 3.0), 6.0);
                if selected || (state.dragging_files && target) {
                    let accent = self.accent();
                    cr.set_source_rgba(
                        f64::from(accent.red()),
                        f64::from(accent.green()),
                        f64::from(accent.blue()),
                        1.0,
                    );
                    cr.set_line_width(2.0);
                } else if active {
                    set(cr, brand.thread, 1.0);
                    cr.set_line_width(2.0);
                } else {
                    set(cr, brand.tile_edge, 1.0);
                    cr.set_line_width(1.0);
                    cr.set_dash(&[4.0, 3.0], 0.0);
                }
                cr.stroke()?;
            }

            // While files are dragged over the canvas, the computers say
            // whether they take them.
            if state.dragging_files {
                let why = if receives {
                    Some(t(keys::CANVAS_DROP_TO_SEND))
                } else if info.is_some_and(|d| d.this_device) {
                    None
                } else {
                    device_text::cannot_receive(model, &device.id, false).map(|w| tr(&w))
                };
                if let Some(why) = why {
                    let layout = self.area.create_pango_layout(Some(&why));
                    bold(&layout, 12.0);
                    let (w, h) = layout.pixel_size();
                    let pill = Rect::new(
                        bounds.mid_x() - f64::from(w) / 2.0 - 8.0,
                        bounds.mid_y() - 11.0,
                        f64::from(w) + 16.0,
                        22.0,
                    );
                    material(cr, &pill, brand)?;
                    self.text_color(cr);
                    cr.move_to(pill.x + 8.0, pill.mid_y() - f64::from(h) / 2.0);
                    pangocairo::functions::show_layout(cr, &layout);
                }
            }
            Ok(())
        })
    }

    /// A screen: the tile color, or the computer's desktop picture filling
    /// it under a veil of the tile color that keeps names, seams and the
    /// selection clear; then its edge and its resolution.
    #[allow(clippy::too_many_arguments)]
    fn draw_tile(
        &self,
        cr: &cairo::Context,
        state: &mut State,
        device: &str,
        screen: Option<&Screen>,
        tile: &Rect,
        brand: &Brand,
        contrast: bool,
    ) -> Result<(), cairo::Error> {
        let tile = Rect::new(tile.x, tile.y, tile.width.max(1.0), tile.height.max(1.0));
        rounded(cr, &tile, 3.0);
        set(cr, brand.tile, 1.0);
        cr.fill()?;
        let picture = match screen {
            Some(screen) if !contrast => {
                state
                    .pictures
                    .picture(device, &screen.id, &screen.preview_jpeg, decode)
            }
            _ => None,
        };
        if let Some(picture) = &picture {
            // The whole picture is sent, whatever the screen's shape: it
            // fills the screen and is cropped to it.
            saved(cr, |cr| {
                rounded(cr, &tile, 3.0);
                cr.clip();
                let (pw, ph) = (f64::from(picture.width()), f64::from(picture.height()));
                let scale = (tile.width / pw).max(tile.height / ph);
                cr.translate(
                    tile.mid_x() - pw * scale / 2.0,
                    tile.mid_y() - ph * scale / 2.0,
                );
                cr.scale(scale, scale);
                cr.set_source_surface(picture, 0.0, 0.0)?;
                cr.source().set_filter(cairo::Filter::Good);
                cr.paint()
            })?;
            rounded(cr, &tile, 3.0);
            set(cr, brand.tile, if brand.dark { 0.35 } else { 0.25 });
            cr.fill()?;
        }
        rounded(cr, &tile.inflate(-0.5, -0.5), 3.0);
        set(cr, brand.tile_edge, 1.0);
        cr.set_line_width(1.0);
        cr.stroke()?;

        // Only where it leaves room for the name and the badge above it;
        // the inspector lists every screen's resolution anyway.
        if let Some(screen) = screen
            && tile.width > 140.0
            && tile.height > 110.0
        {
            let mut lines = Vec::new();
            if screen.primary {
                lines.push(t(keys::SCREEN_MAIN));
            }
            lines.push(device_text::resolution(screen));
            let layout = self.area.create_pango_layout(Some(&lines.join("\n")));
            layout.set_alignment(pango::Alignment::Right);
            small(&layout, 9.0);
            let (w, h) = layout.pixel_size();
            let (x, y) = (
                tile.max_x() - f64::from(w) - 6.0,
                tile.max_y() - f64::from(h) - 5.0,
            );
            if picture.is_some() {
                // On a picture, on material of its own to stay readable.
                rounded(
                    cr,
                    &Rect::new(x - 4.0, y - 2.0, f64::from(w) + 8.0, f64::from(h) + 4.0),
                    4.0,
                );
                if brand.dark {
                    cr.set_source_rgba(0.16, 0.16, 0.18, 0.85);
                } else {
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.85);
                }
                cr.fill()?;
            }
            self.text_color_alpha(cr, 0.7);
            cr.move_to(x, y);
            pangocairo::functions::show_layout(cr, &layout);
        }
        Ok(())
    }

    /// The computer's name, a padlock while its screen is locked or the
    /// moon while it is asleep and can be woken and, beside them dimmer,
    /// the shortcut that jumps there, where the name keeps room enough
    /// (`canvas::name_beside`).
    fn draw_name(
        &self,
        cr: &cairo::Context,
        name: &str,
        jump: Option<&str>,
        glyph: Option<NameGlyph>,
        tile: &Rect,
        brand: &Brand,
    ) -> Result<(), cairo::Error> {
        let label_width = (tile.width - 2.0 * NAME_INSET).max(40.0);
        let mut room = label_width - 2.0 * NAME_PADDING;
        let spacing = 5.0;
        let glyph_width = match glyph {
            Some(NameGlyph::Padlock) => 8.0,
            Some(NameGlyph::Moon) => MOON_SIZE,
            None => 0.0,
        };
        if glyph.is_some() {
            room -= glyph_width + spacing;
        }
        let shortcut = jump.map(|j| {
            let layout = self.area.create_pango_layout(Some(j));
            bold(&layout, 11.0);
            layout
        });
        let shortcut_width = shortcut
            .as_ref()
            .map_or(0.0, |l| f64::from(l.pixel_size().0));
        let (shows_shortcut, name_width) =
            canvas::name_beside(room, shortcut_width, spacing, LEAST_NAME);
        // Too small a tile for even a few letters: no label rather than "…".
        if name_width < 24.0 {
            return Ok(());
        }
        let name_layout = self.area.create_pango_layout(Some(name));
        bold(&name_layout, 11.0);
        name_layout.set_width((name_width.max(1.0) * f64::from(pango::SCALE)) as i32);
        name_layout.set_ellipsize(pango::EllipsizeMode::End);
        let name_drawn = f64::from(name_layout.pixel_size().0).min(name_width.max(1.0));
        let mut width = name_drawn + 2.0 * NAME_PADDING;
        if glyph.is_some() {
            width += glyph_width + spacing;
        }
        if shows_shortcut {
            width += shortcut_width.ceil() + spacing;
        }
        let pill = Rect::new(
            tile.x + NAME_INSET,
            tile.y + NAME_INSET,
            width.min(label_width),
            NAME_HEIGHT,
        );
        saved(cr, |cr| {
            rounded(cr, &tile.inflate(-1.0, -1.0), 3.0);
            cr.clip();
            material(cr, &pill, brand)?;
            let mut x = pill.x + NAME_PADDING;
            let center =
                |layout: &pango::Layout| pill.mid_y() - f64::from(layout.pixel_size().1) / 2.0;
            self.text_color(cr);
            cr.move_to(x, center(&name_layout));
            pangocairo::functions::show_layout(cr, &name_layout);
            x += name_drawn + spacing;
            match glyph {
                Some(NameGlyph::Padlock) => padlock(cr, x, pill.mid_y())?,
                Some(NameGlyph::Moon) => self.moon(cr, x, pill.mid_y())?,
                None => {}
            }
            if glyph.is_some() {
                x += glyph_width + spacing;
            }
            if shows_shortcut && let Some(layout) = &shortcut {
                self.text_color_alpha(cr, 0.6);
                cr.move_to(x, center(layout));
                pangocairo::functions::show_layout(cr, layout);
            }
            Ok(())
        })
    }

    /// The keyboard and mouse on the main screen of the computer they are
    /// on, saying so in words when the screen has room, clear of the name.
    fn draw_badge(
        &self,
        cr: &cairo::Context,
        state: &State,
        model: &Snapshot,
        devices: &[Device],
        brand: &Brand,
        fit: &Fit,
    ) -> Result<(), cairo::Error> {
        let id = model.cursor_device_id();
        let Some(device) = devices.iter().find(|d| d.id == id) else {
            return Ok(());
        };
        let Some(tile) = device
            .frames()
            .get(device.main_screen)
            .map(|f| fit.rect_to_view(f))
        else {
            return Ok(());
        };
        let glyphs = 30.0;
        let words = self
            .area
            .create_pango_layout(Some(&t(keys::KEYBOARD_MOUSE_HERE)));
        bold(&words, 11.0);
        let words_width = f64::from(words.pixel_size().0);
        let with_words = glyphs + 4.0 + words_width + 14.0 <= tile.width - 2.0 * NAME_INSET;
        let size = Size::new(
            if with_words {
                glyphs + 4.0 + words_width + 14.0
            } else {
                glyphs + 12.0
            },
            22.0,
        );
        let frame = canvas::badge_frame(
            size,
            &tile,
            Some(tile.y + NAME_INSET + NAME_HEIGHT),
            NAME_INSET,
        );
        let shake = state
            .shake
            .as_ref()
            .filter(|(s, _)| s == id)
            .map_or(0.0, |(_, start)| shake_offset(*start));
        let grown = pulse(state.pulse_start);
        saved(cr, |cr| {
            cr.translate(frame.mid_x() + shake, frame.mid_y());
            cr.scale(grown, grown);
            cr.translate(-frame.mid_x(), -frame.mid_y());
            material(cr, &frame, brand)?;
            rounded(cr, &frame.inflate(-0.5, -0.5), frame.height / 2.0);
            set(cr, brand.tile_edge, 1.0);
            cr.set_line_width(1.0);
            cr.stroke()?;
            self.text_color(cr);
            keyboard_and_mouse(cr, frame.x + 7.0, frame.mid_y())?;
            if with_words {
                cr.move_to(
                    frame.x + 7.0 + glyphs + 4.0,
                    frame.mid_y() - f64::from(words.pixel_size().1) / 2.0,
                );
                pangocairo::functions::show_layout(cr, &words);
            }
            Ok(())
        })
    }

    /// The system's accent color, which the selection is drawn in: the
    /// user's accent from libadwaita 1.6 (GNOME 47) on, read at run time
    /// as the app builds against 1.5; before it, the theme's
    /// `accent_bg_color`, which GTK only lets the deprecated style context
    /// look up; failing both, the text color.
    #[allow(deprecated)]
    fn accent(&self) -> gdk::RGBA {
        let manager = adw::StyleManager::default();
        if manager.find_property(ACCENT_PROPERTY).is_some() {
            return manager.property::<gdk::RGBA>(ACCENT_PROPERTY);
        }
        self.area
            .style_context()
            .lookup_color("accent_bg_color")
            .unwrap_or_else(|| self.area.color())
    }

    fn text_color(&self, cr: &cairo::Context) {
        self.text_color_alpha(cr, 1.0);
    }

    /// The moon (`wake::MOON_ICON`, from the icon theme) in the text color,
    /// [`MOON_SIZE`] square, from `x` and centered on `y`. Made once for
    /// each scale factor and color, not for every tile at every draw.
    fn moon(&self, cr: &cairo::Context, x: f64, y: f64) -> Result<(), cairo::Error> {
        let scale = self.area.scale_factor();
        let color = self.area.color();
        let key = (
            scale,
            [color.red(), color.green(), color.blue(), color.alpha()],
        );
        let mut cached = self.moon_node.borrow_mut();
        if cached
            .as_ref()
            .is_none_or(|(drawn_for, _)| *drawn_for != key)
        {
            let icon = gtk::IconTheme::for_display(&self.area.display()).lookup_icon(
                wake::MOON_ICON,
                &[],
                MOON_SIZE as i32,
                scale,
                gtk::TextDirection::None,
                gtk::IconLookupFlags::empty(),
            );
            let snapshot = gtk::Snapshot::new();
            icon.snapshot_symbolic(&snapshot, MOON_SIZE, MOON_SIZE, &[color]);
            *cached = snapshot.to_node().map(|node| (key, node));
        }
        if let Some((_, node)) = cached.as_ref() {
            cr.save()?;
            cr.translate(x, y - MOON_SIZE / 2.0);
            node.draw(cr);
            cr.restore()?;
        }
        Ok(())
    }

    fn text_color_alpha(&self, cr: &cairo::Context, alpha: f64) {
        let c = self.area.color();
        cr.set_source_rgba(
            f64::from(c.red()),
            f64::from(c.green()),
            f64::from(c.blue()),
            f64::from(c.alpha()) * alpha,
        );
    }
}

#[cfg(test)]
impl ArrangementCanvas {
    /// The middle of a computer's screens in the view, as last drawn.
    pub fn center_of(&self, id: &str) -> Option<(f64, f64)> {
        let state = self.state.borrow();
        let fit = state.fit?;
        let device = Self::devices(&state).into_iter().find(|d| d.id == id)?;
        let b = fit.rect_to_view(&device.bounds());
        Some((b.mid_x(), b.mid_y()))
    }

    pub fn scale(&self) -> Option<f64> {
        self.state.borrow().fit.map(|f| f.scale)
    }

    /// Presses at (x, y), moves by (dx, dy) and lets go, as a drag does.
    pub fn drag_for_tests(self: &Rc<Self>, x: f64, y: f64, dx: f64, dy: f64) {
        self.press(x, y);
        self.drag_to(dx, dy);
        self.release();
    }

    pub fn key_for_tests(self: &Rc<Self>, key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
        self.key(key, modifiers)
    }

    /// Whether a computer is springing back after a refused move.
    pub fn springing(&self) -> bool {
        self.state.borrow().spring.is_some()
    }

    /// The right-click menu of `id`, as it would open now.
    pub fn menu_for_tests(&self, id: &str) -> Option<gio::Menu> {
        let model = self.state.borrow().model.clone()?;
        Some(self.menu_for(&model, id))
    }
}

fn spring_offset(s: &Spring) -> Point {
    let t = (s.start.elapsed().as_secs_f64() / SPRING_TIME.as_secs_f64()).clamp(0.0, 1.0);
    let eased = 1.0 - (1.0 - t).powi(3);
    Point::new(
        s.from.x + (s.to.x - s.from.x) * eased,
        s.from.y + (s.to.y - s.from.y) * eased,
    )
}

fn shake_offset(start: Instant) -> f64 {
    let amount = (start.elapsed().as_secs_f64() / SHAKE_TIME.as_secs_f64()).clamp(0.0, 1.0);
    6.0 * (amount * std::f64::consts::PI * 4.0).sin()
}

/// The badge's scale now: up to [`PULSE_SCALE`] and back over
/// [`PULSE_TIME`].
fn pulse(start: Option<Instant>) -> f64 {
    let Some(start) = start else { return 1.0 };
    let t = start.elapsed().as_secs_f64() / PULSE_TIME.as_secs_f64();
    if !(0.0..1.0).contains(&t) {
        return 1.0;
    }
    let half = if t < 0.5 { t * 2.0 } else { (1.0 - t) * 2.0 };
    1.0 + (PULSE_SCALE - 1.0) * (1.0 - (1.0 - half).powi(3))
}

fn set(cr: &cairo::Context, color: [f64; 3], alpha: f64) {
    cr.set_source_rgba(color[0], color[1], color[2], alpha);
}

/// Draws with `f` between a save and a restore, restoring however `f`
/// ends, so an error part way leaves the context as it was.
fn saved(
    cr: &cairo::Context,
    f: impl FnOnce(&cairo::Context) -> Result<(), cairo::Error>,
) -> Result<(), cairo::Error> {
    cr.save()?;
    let drawn = f(cr);
    let restored = cr.restore();
    drawn.and(restored)
}

/// Draws with `f` into a group, popped as the source however `f` ends.
fn grouped(
    cr: &cairo::Context,
    f: impl FnOnce(&cairo::Context) -> Result<(), cairo::Error>,
) -> Result<(), cairo::Error> {
    cr.push_group();
    let drawn = f(cr);
    let popped = cr.pop_group_to_source();
    drawn.and(popped)
}

fn rounded(cr: &cairo::Context, r: &Rect, radius: f64) {
    let radius = radius.min(r.width / 2.0).min(r.height / 2.0).max(0.0);
    use std::f64::consts::{FRAC_PI_2, PI};
    cr.new_sub_path();
    cr.arc(r.max_x() - radius, r.y + radius, radius, -FRAC_PI_2, 0.0);
    cr.arc(
        r.max_x() - radius,
        r.max_y() - radius,
        radius,
        0.0,
        FRAC_PI_2,
    );
    cr.arc(r.x + radius, r.max_y() - radius, radius, FRAC_PI_2, PI);
    cr.arc(r.x + radius, r.y + radius, radius, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
}

/// The material under names and badges, so they read on any desktop
/// picture.
fn material(cr: &cairo::Context, r: &Rect, brand: &Brand) -> Result<(), cairo::Error> {
    rounded(cr, r, r.height / 2.0);
    if brand.dark {
        cr.set_source_rgba(0.16, 0.16, 0.18, 0.88);
    } else {
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.88);
    }
    cr.fill()
}

fn bold(layout: &pango::Layout, points: f64) {
    let mut font = pango::FontDescription::new();
    font.set_weight(pango::Weight::Semibold);
    font.set_size((points * f64::from(pango::SCALE)) as i32);
    layout.set_font_description(Some(&font));
}

fn small(layout: &pango::Layout, points: f64) {
    let mut font = pango::FontDescription::new();
    font.set_size((points * f64::from(pango::SCALE)) as i32);
    layout.set_font_description(Some(&font));
}

/// A keyboard and a mouse, 30 by 12, drawn in the current color, starting
/// at `x` and centered on `y`.
fn keyboard_and_mouse(cr: &cairo::Context, x: f64, y: f64) -> Result<(), cairo::Error> {
    cr.set_line_width(1.2);
    rounded(cr, &Rect::new(x + 0.6, y - 5.4, 17.0, 10.8), 2.0);
    cr.stroke()?;
    for row in 0..2 {
        for col in 0..4 {
            cr.rectangle(
                x + 3.0 + f64::from(col) * 3.4,
                y - 3.2 + f64::from(row) * 3.0,
                1.8,
                1.6,
            );
        }
    }
    cr.rectangle(x + 5.0, y + 2.6, 8.0, 1.4);
    cr.fill()?;
    rounded(cr, &Rect::new(x + 21.6, y - 6.0, 8.0, 12.0), 4.0);
    cr.stroke()?;
    cr.move_to(x + 25.6, y - 6.0);
    cr.line_to(x + 25.6, y - 2.0);
    cr.stroke()
}

/// A padlock 8 wide, drawn in the current color, from `x` and centered on
/// `y`.
fn padlock(cr: &cairo::Context, x: f64, y: f64) -> Result<(), cairo::Error> {
    cr.set_line_width(1.3);
    cr.new_sub_path();
    cr.arc(x + 4.0, y - 2.0, 2.4, std::f64::consts::PI, 0.0);
    cr.stroke()?;
    rounded(cr, &Rect::new(x, y - 2.0, 8.0, 6.5), 1.2);
    cr.fill()
}

/// A desktop picture as a cairo surface, through GDK's own loaders.
fn decode(bytes: &[u8]) -> Option<cairo::ImageSurface> {
    let texture = gdk::Texture::from_bytes(&glib::Bytes::from(bytes)).ok()?;
    let (width, height) = (texture.width(), texture.height());
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).ok()?;
    let stride = surface.stride() as usize;
    {
        let mut data = surface.data().ok()?;
        // GDK's default format is cairo's ARGB32: premultiplied, native-endian.
        texture.download(&mut data, stride);
    }
    surface.mark_dirty();
    Some(surface)
}
