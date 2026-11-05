//! The inspector beside the device lists (Devices, Arrangement): one
//! computer's name, system, status and screens, what it shares with this
//! one, the key swap, and removing it.
//!
//! Its rows are made again only when what they are changes (another
//! device, its screens, whether it is paired); otherwise every model
//! updates them in place, so focus, a name being typed and the scroll
//! position stay, and a switch whose command failed shows the model again.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use inputweave_app_core::proto::{
    RenameDevice, SetDeviceOptions, SetDeviceSharing, Unpair, client_message::Command,
};
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;
use inputweave_app_core::{device_text, ui};

use super::{EntrySync, Quiet, set_subtitle, set_switch};
use crate::strings::{t, tr};
use crate::ui::Ui;

/// The rows that change with the model, for one device.
struct Rows {
    id: String,
    name: Option<EntrySync>,
    this_name: Option<adw::ActionRow>,
    names: adw::PreferencesGroup,
    /// The name could not be changed: the group says so until the next try.
    name_refused: Rc<Cell<bool>>,
    status: adw::ActionRow,
    clipboard: Option<adw::SwitchRow>,
    files: Option<adw::SwitchRow>,
    swap: Option<adw::SwitchRow>,
}

pub struct Inspector {
    page: adw::PreferencesPage,
    groups: RefCell<Vec<adw::PreferencesGroup>>,
    /// What the rows were made for (`structure`).
    built: RefCell<String>,
    rows: RefCell<Option<Rows>>,
    quiet: Quiet,
    ui: std::rc::Weak<Ui>,
}

impl Inspector {
    pub fn new(ui: &Rc<Ui>) -> Rc<Inspector> {
        Rc::new(Inspector {
            page: adw::PreferencesPage::new(),
            groups: RefCell::new(Vec::new()),
            built: RefCell::new(String::new()),
            rows: RefCell::new(None),
            quiet: Quiet::default(),
            ui: Rc::downgrade(ui),
        })
    }

    pub fn widget(&self) -> &adw::PreferencesPage {
        &self.page
    }

    /// Shows `id`, or nothing.
    pub fn update(self: &Rc<Self>, model: &Snapshot, id: Option<&str>) {
        let key = id.map(|id| structure(model, id)).unwrap_or_default();
        if *self.built.borrow() != key {
            *self.built.borrow_mut() = key;
            for group in self.groups.borrow_mut().drain(..) {
                self.page.remove(&group);
            }
            *self.rows.borrow_mut() = None;
            if let Some(id) = id {
                let rows = self.build(model, id);
                *self.rows.borrow_mut() = Some(rows);
            }
        }
        if let Some(rows) = self.rows.borrow().as_ref() {
            self.refresh(rows, model);
        }
    }

    /// Updates the rows in place from `model`.
    fn refresh(&self, rows: &Rows, model: &Snapshot) {
        let id = rows.id.as_str();
        let name = model.name_of(id);
        if let Some(entry) = &rows.name {
            entry.show(&name);
        }
        if let Some(row) = &rows.this_name {
            set_subtitle(row, &name);
        }
        if !rows.name_refused.get() {
            let calls_itself = model
                .reported_name(id)
                .map(|r| tr(&ui!(keys::INSPECTOR_CALLS_ITSELF, r)));
            if rows.names.description().as_deref() != calls_itself.as_deref() {
                rows.names.set_description(calls_itself.as_deref());
            }
        }
        set_subtitle(&rows.status, &tr(&device_text::status(model, id)));
        if let Some(device) = model.device(id) {
            if let Some(row) = &rows.clipboard {
                set_switch(&self.quiet, row, !device.clipboard_off);
            }
            if let Some(row) = &rows.files {
                set_switch(&self.quiet, row, !device.files_off);
            }
            if let Some(row) = &rows.swap {
                set_switch(&self.quiet, row, device.swap_gui_and_control);
            }
        }
    }

    #[cfg(test)]
    pub fn status_row(&self) -> Option<adw::ActionRow> {
        self.rows.borrow().as_ref().map(|r| r.status.clone())
    }

    #[cfg(test)]
    pub fn clipboard_row(&self) -> Option<adw::SwitchRow> {
        self.rows
            .borrow()
            .as_ref()
            .and_then(|r| r.clipboard.clone())
    }

    /// The name field, for another computer: "Rename…" on the canvas
    /// puts the cursor there.
    pub fn name_row(&self) -> Option<adw::EntryRow> {
        self.rows
            .borrow()
            .as_ref()
            .and_then(|r| r.name.as_ref().map(|n| n.row().clone()))
    }

    fn add(&self, group: adw::PreferencesGroup) {
        self.page.add(&group);
        self.groups.borrow_mut().push(group);
    }

    fn build(self: &Rc<Self>, model: &Snapshot, id: &str) -> Rows {
        let this = id == model.this_device_id();
        let device = model.device(id);
        let arranged = model.arranged(id);
        let os = os_of(model, id);

        let names = adw::PreferencesGroup::new();
        let name_refused = Rc::new(Cell::new(false));
        let mut name = None;
        let mut this_name = None;
        if this {
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .title(t(keys::INSPECTOR_NAME))
                .css_classes(["property"])
                .build();
            names.add(&row);
            this_name = Some(row);
        } else {
            let entry = adw::EntryRow::builder()
                .use_markup(false)
                .title(t(keys::INSPECTOR_NAME))
                .text(model.name_of(id))
                .show_apply_button(true)
                .build();
            let sync = EntrySync::new(&entry);
            // Weakly: the group holds the row, whose handler this is.
            let (weak, device_id, group) = (self.ui.clone(), id.to_owned(), names.downgrade());
            let (refused, edits) = (name_refused.clone(), sync.edits());
            entry.connect_apply(move |entry| {
                let (Some(ui), Some(group)) = (weak.upgrade(), group.upgrade()) else {
                    return;
                };
                edits.applied();
                refused.set(false);
                let refused = refused.clone();
                ui.command_then(
                    Command::RenameDevice(RenameDevice {
                        device_id: device_id.clone(),
                        name: entry.text().trim().to_owned(),
                    }),
                    move |result| match result {
                        Err(e) if e.code() == Some("invalid_name") => {
                            refused.set(true);
                            group.set_description(Some(&t(keys::NAME_INVALID)));
                            true
                        }
                        _ => result.is_ok(),
                    },
                );
            });
            names.add(&entry);
            name = Some(sync);
        }
        self.add(names.clone());

        let about = adw::PreferencesGroup::new();
        let property = |title: &str| {
            adw::ActionRow::builder()
                .use_markup(false)
                .title(title)
                .subtitle_selectable(true)
                .css_classes(["property"])
                .build()
        };
        let system = property(&t(keys::INSPECTOR_SYSTEM));
        system.set_subtitle(&tr(&device_text::os(&os)));
        let status = property(&t(keys::INSPECTOR_STATUS));
        about.add(&system);
        about.add(&status);
        self.add(about);

        let screens = adw::PreferencesGroup::builder()
            .title(t(keys::INSPECTOR_SCREENS))
            .build();
        match arranged.filter(|a| !a.screens.is_empty()) {
            Some(arranged) => {
                // Named by number, as Identify Screens shows them: ids such
                // as "1" or "\\.\DISPLAY1" mean nothing to the user.
                for (number, screen) in arranged.screens.iter().enumerate() {
                    let row = adw::ActionRow::builder()
                        .use_markup(false)
                        .title(tr(&ui!(
                            keys::SCREEN_NUMBERED,
                            (number + 1).to_string(),
                            device_text::resolution(screen)
                        )))
                        .build();
                    if screen.primary {
                        row.add_suffix(&super::value_label(&t(keys::SCREEN_MAIN)));
                    }
                    screens.add(&row);
                }
            }
            None => {
                screens.add(
                    &adw::ActionRow::builder()
                        .use_markup(false)
                        .title(t(keys::INSPECTOR_SCREENS_LATER))
                        .build(),
                );
            }
        }
        self.add(screens);

        let mut rows = Rows {
            id: id.to_owned(),
            name,
            this_name,
            names,
            name_refused,
            status,
            clipboard: None,
            files: None,
            swap: None,
        };
        let Some(device) = device else { return rows };

        let sharing = adw::PreferencesGroup::builder()
            .description(t(keys::INSPECTOR_SHARING_HINT))
            .build();
        let clipboard = adw::SwitchRow::builder()
            .use_markup(false)
            .title(t(keys::INSPECTOR_SYNC_CLIPBOARD))
            .build();
        let files = adw::SwitchRow::builder()
            .use_markup(false)
            .title(t(keys::INSPECTOR_SEND_FILES))
            .build();
        for switch in [&clipboard, &files] {
            let (weak, device_id, quiet) = (self.ui.clone(), id.to_owned(), self.quiet.clone());
            // Weakly: each switch's handler reads both.
            let (c, f) = (clipboard.downgrade(), files.downgrade());
            switch.connect_active_notify(move |_| {
                if quiet.is_set() {
                    return;
                }
                let (Some(c), Some(f)) = (c.upgrade(), f.upgrade()) else {
                    return;
                };
                if let Some(ui) = weak.upgrade() {
                    ui.command(Command::SetDeviceSharing(SetDeviceSharing {
                        device_id: device_id.clone(),
                        clipboard_off: !c.is_active(),
                        files_off: !f.is_active(),
                    }));
                }
            });
        }
        sharing.add(&clipboard);
        sharing.add(&files);
        self.add(sharing);
        rows.clipboard = Some(clipboard);
        rows.files = Some(files);

        if !device.paired {
            return rows;
        }
        let options = adw::PreferencesGroup::new();
        let swap = adw::SwitchRow::builder()
            .use_markup(false)
            .title(t(device_text::swap_key(&os)))
            .build();
        let (weak, device_id, quiet) = (self.ui.clone(), id.to_owned(), self.quiet.clone());
        swap.connect_active_notify(move |row| {
            if quiet.is_set() {
                return;
            }
            if let Some(ui) = weak.upgrade() {
                ui.command(Command::SetDeviceOptions(SetDeviceOptions {
                    device_id: device_id.clone(),
                    swap_gui_and_control: row.is_active(),
                }));
            }
        });
        options.add(&swap);
        self.add(options);
        rows.swap = Some(swap);

        let removal = adw::PreferencesGroup::new();
        let remove = gtk::Button::builder()
            .label(t(keys::REMOVE_DEVICE))
            .halign(gtk::Align::Start)
            .css_classes(["destructive-action"])
            .build();
        let (weak, device_id) = (self.ui.clone(), id.to_owned());
        remove.connect_clicked(move |button| {
            let Some(ui) = weak.upgrade() else { return };
            let name = ui.snapshot().name_of(&device_id);
            let dialog = adw::AlertDialog::new(
                Some(&tr(&ui!(keys::REMOVE_DEVICE_TITLE, name))),
                Some(&t(keys::REMOVE_DEVICE_MESSAGE)),
            );
            dialog.add_response("cancel", &t(keys::CANCEL));
            dialog.add_response("remove", &t(keys::REMOVE_DEVICE_CONFIRM));
            dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let device_id = device_id.clone();
            dialog.connect_response(None, move |_, response| {
                if response == "remove" {
                    ui.command(Command::Unpair(Unpair {
                        device_id: device_id.clone(),
                    }));
                }
            });
            dialog.present(Some(button));
        });
        removal.add(&remove);
        self.add(removal);
        rows
    }
}

fn os_of(model: &Snapshot, id: &str) -> String {
    model
        .device(id)
        .map(|d| d.os.clone())
        .or_else(|| model.arranged(id).map(|a| a.os.clone()))
        .unwrap_or_else(|| {
            if id == model.this_device_id() {
                "linux".into()
            } else {
                String::new()
            }
        })
}

/// What the inspector's rows are: which device, which of them it has, and
/// its screens. Names, status and switches are updated in place.
fn structure(model: &Snapshot, id: &str) -> String {
    let screens: Vec<String> = model
        .arranged(id)
        .map(|a| {
            a.screens
                .iter()
                .map(|s| format!("{}x{}@{}:{}", s.width, s.height, s.scale, s.primary))
                .collect()
        })
        .unwrap_or_default();
    format!(
        "{id}|{}|{:?}|{}|{}",
        id == model.this_device_id(),
        model.device(id).map(|d| d.paired),
        os_of(model, id),
        screens.join(","),
    )
}
