//! General › Shortcuts: the four rows, each with
//! Record, the jumps and relative mode with a switch, below taking control
//! back the switch for pressing Ctrl twice to do it too, and Reset
//! Shortcuts, as the other two apps have them. Recording listens in a
//! focused box: GTK gives each key's hardware keycode (evdev + 8 on X11 and
//! Wayland), named through `iw-keymap`, and the modifiers held; the
//! decisions are `inputweave_app_core::shortcut`. The whole set goes to the
//! service with SetHotkeys, which may refuse it and say why: in red under
//! the row it is about, or under the group for Reset Shortcuts.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, glib};
use inputweave_app_core::proto::{Hotkeys, SetHotkeys, client_message::Command};
use inputweave_app_core::shortcut::{self, Held, Recorded, Row};
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;
use inputweave_app_core::ui;

use super::{Quiet, set_text, value_label};
use crate::strings::{t, tr};
use crate::ui::Ui;

struct RowWidgets {
    row: adw::ActionRow,
    value: gtk::Label,
    /// The box that has the focus while the row records.
    capture: gtk::Box,
    record: gtk::Button,
    switch: Option<gtk::Switch>,
}

/// Where a refusal is said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spot {
    /// Under the row it is about.
    Row(Row),
    /// Under the double press switch.
    DoublePress,
    /// Under the group: Reset Shortcuts.
    Group,
}

pub struct Shortcuts {
    group: adw::PreferencesGroup,
    rows: Vec<(Row, RowWidgets)>,
    /// Pressing Ctrl twice takes control back too; hidden while the service
    /// does not know it.
    double_press: adw::SwitchRow,
    reset: gtk::Button,
    error: gtk::Label,
    /// The refusal said now, and where; it stays through updates until the
    /// next change or recording.
    problem: RefCell<Option<(Spot, String)>>,
    recording: Cell<Option<Row>>,
    /// The hotkeys as last shown, which a recording changes.
    hotkeys: RefCell<Hotkeys>,
    relative: Cell<bool>,
    /// The service knows the double press.
    double_supported: Cell<bool>,
    /// The jumps do nothing where the cursor leaves only across edges.
    edges_only: Cell<bool>,
    quiet: Quiet,
    ui: Weak<Ui>,
}

impl Shortcuts {
    pub fn new(ui: &Rc<Ui>) -> Rc<Shortcuts> {
        let group = adw::PreferencesGroup::builder()
            .title(t(keys::GENERAL_SHORTCUTS))
            .description(t(keys::SHORTCUTS_HINT))
            .build();
        let double_press = adw::SwitchRow::builder()
            .use_markup(false)
            .title(t(keys::SHORTCUT_DOUBLE_PRESS))
            .subtitle(t(keys::SHORTCUT_DOUBLE_PRESS_HINT))
            .visible(false)
            .build();
        let mut rows = Vec::new();
        for row in Row::ALL {
            let value = value_label("");
            value.add_css_class("iw-shortcut");
            let prompt = gtk::Label::new(Some(&t(row.prompt())));
            let capture = gtk::Box::builder()
                .css_classes(["iw-recording"])
                .focusable(true)
                .can_focus(true)
                .valign(gtk::Align::Center)
                .visible(false)
                .build();
            capture.append(&prompt);
            // Clicking it must not take the focus from the box listening:
            // that would end the recording before the click, and Cancel
            // would start it again (as on Windows).
            let record = gtk::Button::builder()
                .label(t(keys::SHORTCUT_RECORD))
                .valign(gtk::Align::Center)
                .focus_on_click(false)
                .build();
            let switch = row.can_turn_off().then(|| {
                let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
                switch.update_property(&[gtk::accessible::Property::Label(&tr(&ui!(
                    keys::SHORTCUT_TURN_ON,
                    ui!(row.title())
                )))]);
                switch
            });
            let action = adw::ActionRow::builder()
                .use_markup(false)
                .title(t(row.title()))
                .build();
            action.add_suffix(&value);
            action.add_suffix(&capture);
            action.add_suffix(&record);
            if let Some(switch) = &switch {
                action.add_suffix(switch);
            }
            group.add(&action);
            if row == Row::TakeBack {
                group.add(&double_press);
            }
            rows.push((
                row,
                RowWidgets {
                    row: action,
                    value,
                    capture,
                    record,
                    switch,
                },
            ));
        }
        let error = gtk::Label::builder()
            .css_classes(["error"])
            .wrap(true)
            .xalign(0.0)
            .margin_top(6)
            .visible(false)
            .build();
        group.add(&error);
        let reset = gtk::Button::builder()
            .label(t(keys::SHORTCUT_RESET))
            .halign(gtk::Align::Start)
            .margin_top(12)
            .build();
        group.add(&reset);
        let shortcuts = Rc::new(Shortcuts {
            group,
            rows,
            double_press,
            reset,
            error,
            problem: RefCell::new(None),
            recording: Cell::new(None),
            hotkeys: RefCell::new(shortcut::defaults()),
            relative: Cell::new(false),
            double_supported: Cell::new(false),
            edges_only: Cell::new(false),
            quiet: Quiet::default(),
            ui: Rc::downgrade(ui),
        });
        shortcuts.connect();
        shortcuts
    }

    pub fn widget(&self) -> &adw::PreferencesGroup {
        &self.group
    }

    fn widgets(&self, row: Row) -> &RowWidgets {
        &self
            .rows
            .iter()
            .find(|(r, _)| *r == row)
            .expect("every row")
            .1
    }

    fn connect(self: &Rc<Self>) {
        for (row, widgets) in &self.rows {
            let row = *row;
            let weak = Rc::downgrade(self);
            widgets.record.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    if this.recording.get() == Some(row) {
                        this.stop();
                    } else {
                        this.start(row);
                    }
                }
            });
            let keys = gtk::EventControllerKey::new();
            let weak = Rc::downgrade(self);
            keys.connect_key_pressed(move |_, _, keycode, state| {
                if let Some(this) = weak.upgrade() {
                    this.pressed(row, keycode, state);
                }
                glib::Propagation::Stop
            });
            widgets.capture.add_controller(keys);
            // Clicking elsewhere ends it, as the focus leaves.
            let focus = gtk::EventControllerFocus::new();
            let weak = Rc::downgrade(self);
            focus.connect_leave(move |_| {
                if let Some(this) = weak.upgrade()
                    && this.recording.get() == Some(row)
                {
                    this.stop();
                }
            });
            widgets.capture.add_controller(focus);
            if let Some(switch) = &widgets.switch {
                let weak = Rc::downgrade(self);
                switch.connect_active_notify(move |switch| {
                    let Some(this) = weak.upgrade() else { return };
                    if this.quiet.is_set() {
                        return;
                    }
                    let value = if switch.is_active() {
                        shortcut::get(&shortcut::defaults(), row)
                    } else {
                        String::new()
                    };
                    this.send(Some(row), &value);
                });
            }
        }
        let weak = Rc::downgrade(self);
        self.double_press.connect_active_notify(move |switch| {
            let Some(this) = weak.upgrade() else { return };
            if this.quiet.is_set() {
                return;
            }
            this.stop();
            let changed = shortcut::with_double_press(&this.hotkeys.borrow(), switch.is_active());
            this.send_all(Spot::DoublePress, None, changed);
        });
        let weak = Rc::downgrade(self);
        self.reset.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.stop();
                let mut defaults = shortcut::defaults();
                if !this.relative.get() {
                    defaults.relative = None;
                }
                if !this.double_supported.get() {
                    defaults.take_back_double = None;
                }
                this.send_all(Spot::Group, None, defaults);
            }
        });
    }

    fn start(&self, row: Row) {
        self.stop();
        self.clear_error();
        self.recording.set(Some(row));
        let widgets = self.widgets(row);
        widgets.value.set_visible(false);
        widgets.capture.set_visible(true);
        widgets.record.set_label(&t(keys::CANCEL));
        widgets.capture.grab_focus();
    }

    fn stop(&self) {
        let Some(row) = self.recording.take() else {
            return;
        };
        let widgets = self.widgets(row);
        widgets.capture.set_visible(false);
        widgets.value.set_visible(true);
        widgets.record.set_label(&t(keys::SHORTCUT_RECORD));
    }

    /// A key pressed while `row` records.
    fn pressed(self: &Rc<Self>, row: Row, keycode: u32, state: gdk::ModifierType) {
        if self.recording.get() != Some(row) {
            return;
        }
        let held = Held {
            control: state.contains(gdk::ModifierType::CONTROL_MASK),
            alt: state.contains(gdk::ModifierType::ALT_MASK),
            shift: state.contains(gdk::ModifierType::SHIFT_MASK),
            gui: state.contains(gdk::ModifierType::SUPER_MASK),
        };
        match shortcut::record(row, shortcut::key_name(keycode), held) {
            Recorded::Wait => {}
            Recorded::Cancel => self.stop(),
            Recorded::Unusable => {
                self.stop();
                self.show_error(Spot::Row(row), &tr(&ui!(keys::SHORTCUT_UNKNOWN_KEY)));
            }
            Recorded::Set(value) => {
                self.stop();
                self.send(Some(row), &value);
            }
        }
    }

    fn send(self: &Rc<Self>, row: Option<Row>, value: &str) {
        let before = self.hotkeys.borrow().clone();
        let changed = match row {
            Some(row) => shortcut::with(&before, row, value),
            None => before,
        };
        let spot = row.map_or(Spot::Group, Spot::Row);
        self.send_all(spot, row, changed);
    }

    /// Sends the whole set, which the service takes or refuses as one; a
    /// refusal of `row`'s change is said at `spot`.
    fn send_all(self: &Rc<Self>, spot: Spot, row: Option<Row>, hotkeys: Hotkeys) {
        let Some(ui) = self.ui.upgrade() else { return };
        let before = self.hotkeys.borrow().clone();
        let this = Rc::downgrade(self);
        let tried = hotkeys.clone();
        let weak_ui = Rc::downgrade(&ui);
        ui.command_then(
            Command::SetHotkeys(SetHotkeys {
                hotkeys: Some(hotkeys),
            }),
            move |result| {
                let Some(this) = this.upgrade() else {
                    return true;
                };
                match result {
                    Ok(_) => this.clear_error(),
                    Err(e) => {
                        let why = shortcut::refusal(e.code().unwrap_or(""), row, &tried, &before);
                        this.show_error(spot, &tr(&why));
                    }
                }
                // Switches go back to what the service has.
                if let Some(ui) = weak_ui.upgrade() {
                    ui.refresh();
                }
                true
            },
        );
    }

    /// Stops recording, as Esc does.
    pub fn stop_recording(&self) {
        self.stop();
    }

    /// Says `text` in red at `spot`, in place of what was said before.
    pub fn show_error(&self, spot: Spot, text: &str) {
        self.problem.replace(Some((spot, text.to_owned())));
        self.show_problem();
    }

    fn clear_error(&self) {
        if self.problem.take().is_some() {
            self.show_problem();
        }
    }

    /// Shows the refusal said now where it belongs, and every other row's
    /// own words.
    fn show_problem(&self) {
        let problem = self.problem.borrow();
        let at = |spot: Spot| {
            problem
                .as_ref()
                .filter(|(s, _)| *s == spot)
                .map(|(_, text)| text.clone())
        };
        for (row, widgets) in &self.rows {
            // The jumps do nothing where the cursor leaves only across
            // edges; the row says why.
            let own = if *row == Row::Jump && self.edges_only.get() {
                t(keys::EDGES_ONLY)
            } else {
                String::new()
            };
            show_on(&widgets.row, at(Spot::Row(*row)), own);
        }
        show_on(
            self.double_press.upcast_ref(),
            at(Spot::DoublePress),
            t(keys::SHORTCUT_DOUBLE_PRESS_HINT),
        );
        match at(Spot::Group) {
            Some(text) => {
                self.error.set_label(&text);
                self.error.set_visible(true);
            }
            None => self.error.set_visible(false),
        }
    }

    pub fn update(&self, model: &Snapshot) {
        let hotkeys = model.hotkeys();
        let relative = model.relative_supported();
        self.relative.set(relative);
        let double_supported = model.double_press_supported();
        self.double_supported.set(double_supported);
        self.edges_only.set(model.edges_only());
        if !model.connected() {
            self.stop();
            self.problem.replace(None);
        }
        self.double_press.set_visible(double_supported);
        self.double_press.set_sensitive(model.connected());
        let on = model.double_press_on();
        if self.double_press.is_active() != on {
            self.quiet.run(|| self.double_press.set_active(on));
        }
        for (row, widgets) in &self.rows {
            let visible = *row != Row::Relative || relative;
            widgets.row.set_visible(visible);
            if !visible && self.recording.get() == Some(*row) {
                self.stop();
            }
            set_text(&widgets.value, &tr(&shortcut::shown(&hotkeys, *row)));
            let on = !shortcut::get(&hotkeys, *row).is_empty();
            if let Some(switch) = &widgets.switch
                && switch.is_active() != on
            {
                self.quiet.run(|| switch.set_active(on));
            }
            widgets.record.set_sensitive(on || !row.can_turn_off());
        }
        self.show_problem();
        self.reset
            .set_sensitive(!shortcut::is_default(&hotkeys, relative, double_supported));
        *self.hotkeys.borrow_mut() = hotkeys;
    }

    /// Records `row`, as its button does: for tests and screenshots.
    pub fn record_row(&self, row: Row) {
        self.start(row);
    }

    #[cfg(test)]
    pub fn press_for_tests(self: &Rc<Self>, row: Row, keycode: u32, state: gdk::ModifierType) {
        self.pressed(row, keycode, state);
    }

    /// The refusal said, and where.
    #[cfg(test)]
    pub fn error_shown(&self) -> Option<(Spot, String)> {
        self.problem.borrow().clone()
    }

    #[cfg(test)]
    pub fn double_press_row(&self) -> adw::SwitchRow {
        self.double_press.clone()
    }

    #[cfg(test)]
    pub fn reset_button(&self) -> gtk::Button {
        self.reset.clone()
    }

    #[cfg(test)]
    pub fn subtitle_of(&self, row: Row) -> String {
        self.widgets(row)
            .row
            .subtitle()
            .unwrap_or_default()
            .to_string()
    }

    #[cfg(test)]
    pub fn record_button(&self, row: Row) -> gtk::Button {
        self.widgets(row).record.clone()
    }

    #[cfg(test)]
    pub fn recording(&self) -> Option<Row> {
        self.recording.get()
    }
}

/// Shows `problem` in red as `row`'s subtitle, or else its own words.
fn show_on(row: &adw::ActionRow, problem: Option<String>, own: String) {
    let failed = problem.is_some();
    let text = problem.unwrap_or(own);
    if row.subtitle().as_deref().unwrap_or("") != text {
        row.set_subtitle(&text);
    }
    if failed {
        row.add_css_class("iw-failed");
    } else {
        row.remove_css_class("iw-failed");
    }
}
