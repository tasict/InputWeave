//! The main window's pages. Each builds its widgets once and updates them
//! from every new model; a [`Quiet`] keeps those updates from reading as
//! the user's own changes.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;

pub mod arrangement;
pub mod devices;
pub mod files;
pub mod general;
pub mod inspector;
pub mod license;
pub mod shortcuts;
pub mod troubleshooting;

/// Set while the app itself changes a control, so its handler does not
/// send the change on as the user's.
#[derive(Clone, Default)]
pub struct Quiet(Rc<Cell<bool>>);

impl Quiet {
    pub fn run(&self, change: impl FnOnce()) {
        let before = self.0.replace(true);
        change();
        self.0.set(before);
    }

    pub fn is_set(&self) -> bool {
        self.0.get()
    }
}

/// An entry row the model keeps up to date, except while the user edits
/// it: while the focus is in it (on its inner text, not the row, so
/// `has_focus` would never tell), and from their first change until they
/// apply it or the text is the model's again.
#[derive(Clone)]
pub struct EntrySync {
    row: adw::EntryRow,
    quiet: Quiet,
    edited: Rc<Cell<bool>>,
}

impl EntrySync {
    pub fn new(row: &adw::EntryRow) -> EntrySync {
        let sync = EntrySync {
            row: row.clone(),
            quiet: Quiet::default(),
            edited: Rc::default(),
        };
        let (quiet, edited) = (sync.quiet.clone(), sync.edited.clone());
        row.connect_changed(move |_| {
            if !quiet.is_set() {
                edited.set(true);
            }
        });
        sync
    }

    pub fn row(&self) -> &adw::EntryRow {
        &self.row
    }

    pub fn editing(&self) -> bool {
        self.edited.get()
            || self
                .row
                .state_flags()
                .contains(gtk::StateFlags::FOCUS_WITHIN)
    }

    /// What the row's own handlers keep to say the text was applied, so
    /// the model's answer may show again: holding the row there would keep
    /// it alive through itself.
    pub fn edits(&self) -> Edits {
        Edits(self.edited.clone())
    }

    /// Shows the model's `text` unless the user is editing.
    pub fn show(&self, text: &str) {
        let focused = self
            .row
            .state_flags()
            .contains(gtk::StateFlags::FOCUS_WITHIN);
        if !focused && self.row.text() == text {
            // Edited back to what it is: nothing pending.
            self.edited.set(false);
        }
        if !self.editing() && self.row.text() != text {
            self.quiet.run(|| self.row.set_text(text));
        }
    }
}

/// Whether an [`EntrySync`]'s row has a change not applied yet.
#[derive(Clone)]
pub struct Edits(Rc<Cell<bool>>);

impl Edits {
    pub fn applied(&self) {
        self.0.set(false);
    }
}

pub fn set_switch(quiet: &Quiet, row: &adw::SwitchRow, on: bool) {
    if row.is_active() != on {
        quiet.run(|| row.set_active(on));
    }
}

pub fn set_text(label: &gtk::Label, text: &str) {
    if label.text() != text {
        label.set_text(text);
    }
}

pub fn set_title(row: &impl IsA<adw::PreferencesRow>, title: &str) {
    if row.title() != title {
        row.set_title(title);
    }
}

pub fn set_subtitle(row: &adw::ActionRow, subtitle: &str) {
    if row.subtitle().as_deref() != Some(subtitle) {
        row.set_subtitle(subtitle);
    }
}

/// A label at the end of a row: a value, or a shortcut.
pub fn value_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["dim-label", "iw-numeric"])
        .selectable(false)
        .build()
}

/// A flat button for the end of a row.
pub fn row_button(text: &str) -> gtk::Button {
    gtk::Button::builder()
        .label(text)
        .valign(gtk::Align::Center)
        .build()
}

/// Removes every row a group was given with `add`.
pub fn clear_group(group: &adw::PreferencesGroup, rows: &mut Vec<gtk::Widget>) {
    for row in rows.drain(..) {
        group.remove(&row);
    }
}

/// An action row whose title and subtitle are text, not Pango markup.
pub fn plain_row() -> adw::ActionRow {
    adw::ActionRow::builder().use_markup(false).build()
}

/// A row for one permission the desktop asks for: its state (and a note)
/// and the button that asks, which sends RequestPermission; the desktop
/// shows its own dialog.
pub fn permission_row(
    ui: &Rc<crate::ui::Ui>,
    row: &inputweave_app_core::trouble_text::PermissionRow,
) -> adw::ActionRow {
    use crate::strings::{t, tr};
    use inputweave_app_core::proto::{RequestPermission, client_message::Command};
    let mut subtitle = tr(&row.state);
    if let Some(note) = &row.note {
        subtitle.push('\n');
        subtitle.push_str(&tr(note));
    }
    let action = plain_row();
    action.set_title(&t(row.title));
    action.set_subtitle(&subtitle);
    if let Some(key) = row.button {
        let button = row_button(&t(key));
        let weak = Rc::downgrade(ui);
        let permission = row.permission;
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.command(Command::RequestPermission(RequestPermission {
                    permission: permission as i32,
                }));
            }
        });
        action.add_suffix(&button);
    }
    action
}

/// A row with a command to type in a terminal, in a fixed-width font,
/// and a button that copies it.
pub fn command_row(ui: &Rc<crate::ui::Ui>, title: &str, command: &str) -> adw::ActionRow {
    use crate::strings::t;
    use inputweave_app_core::text::keys;
    let row = plain_row();
    row.set_title(title);
    row.set_subtitle(command);
    row.set_subtitle_selectable(true);
    row.add_css_class("property");
    row.add_css_class("iw-command");
    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text(t(keys::COPY))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    copy.update_property(&[gtk::accessible::Property::Label(&t(keys::COPY))]);
    let weak = Rc::downgrade(ui);
    let text = command.to_owned();
    copy.connect_clicked(move |button| {
        button.clipboard().set_text(&text);
        if let Some(ui) = weak.upgrade() {
            ui.toast(&t(keys::COPIED));
        }
    });
    row.add_suffix(&copy);
    row
}
