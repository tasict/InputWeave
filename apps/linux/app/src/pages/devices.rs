//! Devices: the other computers, with their status, latency and jump
//! shortcut (and the icon of one offline for now, or the moon of one that
//! can be woken), and the inspector for the one selected; an empty state
//! while there are none.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::SystemTime;

use adw::prelude::*;
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;
use inputweave_app_core::{device_text, offline, wake};

use super::inspector::Inspector;
use super::{set_subtitle, set_title};
use crate::strings::{t, tr};
use crate::ui::Ui;

/// A list of computers whose rows are kept while the same ones are listed.
pub struct DeviceList {
    pub list: gtk::ListBox,
    rows: RefCell<Vec<ListRow>>,
    pub selected: RefCell<Option<String>>,
}

/// One computer's row: its name and status, then at its end the network
/// struck through while it is offline for now, the moon while it can be
/// woken, the keyboard and mouse while they are on it, and its jump
/// shortcut.
struct ListRow {
    id: String,
    row: adw::ActionRow,
    shortcut: gtk::Label,
    here: gtk::Image,
    moon: gtk::Image,
    offline_icon: gtk::Image,
}

impl DeviceList {
    pub fn new() -> Rc<DeviceList> {
        let list = gtk::ListBox::builder()
            .css_classes(["boxed-list"])
            .selection_mode(gtk::SelectionMode::Single)
            .valign(gtk::Align::Start)
            .build();
        Rc::new(DeviceList {
            list,
            rows: RefCell::new(Vec::new()),
            selected: RefCell::new(None),
        })
    }

    /// Lists `ids`, with each one's subtitle, keeping the selection.
    pub fn update(&self, model: &Snapshot, ids: &[String], subtitle: impl Fn(&str) -> String) {
        let same = {
            let rows = self.rows.borrow();
            rows.len() == ids.len() && rows.iter().zip(ids).all(|(r, id)| r.id == *id)
        };
        if !same {
            let mut rows = self.rows.borrow_mut();
            for row in rows.drain(..) {
                self.list.remove(&row.row);
            }
            for id in ids {
                let row = adw::ActionRow::builder()
                    .use_markup(false)
                    .activatable(true)
                    .build();
                let here = gtk::Image::from_icon_name("input-mouse-symbolic");
                here.add_css_class("iw-here");
                let icon = gtk::Image::from_icon_name("computer-symbolic");
                row.add_prefix(&icon);
                let shortcut = super::value_label("");
                shortcut.add_css_class("iw-shortcut");
                // The status says it in words; the moon only goes with them.
                let moon = gtk::Image::builder()
                    .icon_name(wake::MOON_ICON)
                    .accessible_role(gtk::AccessibleRole::Presentation)
                    .css_classes(["dim-label"])
                    .build();
                // So does the status for a computer offline for now.
                let offline_icon = gtk::Image::builder()
                    .icon_name(offline::ICON)
                    .accessible_role(gtk::AccessibleRole::Presentation)
                    .css_classes(["dim-label"])
                    .build();
                row.add_suffix(&offline_icon);
                row.add_suffix(&moon);
                row.add_suffix(&here);
                row.add_suffix(&shortcut);
                self.list.append(&row);
                rows.push(ListRow {
                    id: id.clone(),
                    row,
                    shortcut,
                    here,
                    moon,
                    offline_icon,
                });
            }
        }
        let cursor = model.cursor_device_id().to_owned();
        let selected = self.selected.borrow().clone();
        for ListRow {
            id,
            row,
            shortcut,
            here,
            moon,
            offline_icon,
        } in self.rows.borrow().iter()
        {
            set_title(row, &model.name_of(id));
            set_subtitle(row, &subtitle(id));
            let slot = model
                .arranged(id)
                .map(|a| a.jump_slot)
                .or_else(|| model.device(id).map(|d| d.jump_slot))
                .unwrap_or(0);
            super::set_text(shortcut, &model.jump_shortcut(slot).unwrap_or_default());
            here.set_visible(*id == cursor);
            here.set_tooltip_text(Some(&t(keys::KEYBOARD_MOUSE_HERE)));
            moon.set_visible(wake::shows_moon(model, id));
            offline_icon.set_visible(offline::is_offline(model, id));
            if selected.as_deref() == Some(id.as_str())
                && self.list.selected_row().as_ref() != Some(row.upcast_ref())
            {
                self.list.select_row(Some(row));
            }
        }
    }

    /// Shows the selected device in `inspector`.
    pub fn connect_selected(self: &Rc<Self>, inspector: &Rc<Inspector>, ui: &Rc<Ui>) {
        // Weakly: the list holds this handler.
        let (list, inspector, ui) = (
            Rc::downgrade(self),
            Rc::downgrade(inspector),
            Rc::downgrade(ui),
        );
        self.list.connect_row_selected(move |_, row| {
            let (Some(row), Some(list), Some(inspector), Some(ui)) =
                (row, list.upgrade(), inspector.upgrade(), ui.upgrade())
            else {
                return;
            };
            let id = list.id_at(row);
            *list.selected.borrow_mut() = id.clone();
            inspector.update(&ui.snapshot(), id.as_deref());
        });
    }

    pub fn id_at(&self, row: &gtk::ListBoxRow) -> Option<String> {
        self.rows
            .borrow()
            .iter()
            .find(|r| r.row.upcast_ref::<gtk::ListBoxRow>() == row)
            .map(|r| r.id.clone())
    }

    /// Whether the moon shows after `id`'s name.
    #[cfg(test)]
    pub fn moon_shown(&self, id: &str) -> Option<bool> {
        self.rows
            .borrow()
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.moon.is_visible())
    }

    /// Whether the icon of a computer offline for now shows after `id`'s
    /// name.
    #[cfg(test)]
    pub fn offline_shown(&self, id: &str) -> Option<bool> {
        self.rows
            .borrow()
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.offline_icon.is_visible())
    }

    /// `id`'s status, as its row says it.
    #[cfg(test)]
    pub fn status_of(&self, id: &str) -> Option<String> {
        self.rows
            .borrow()
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.row.subtitle().map(|s| s.to_string()))
    }
}

pub struct Devices {
    stack: gtk::Stack,
    list: Rc<DeviceList>,
    inspector: Rc<Inspector>,
}

impl Devices {
    pub fn new(ui: &Rc<Ui>) -> Devices {
        let list = DeviceList::new();
        let inspector = Inspector::new(ui);
        let add = gtk::Button::builder()
            .label(t(keys::ACTION_ADD_DEVICE))
            .halign(gtk::Align::Start)
            .build();
        let weak = Rc::downgrade(ui);
        add.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.add_device();
            }
        });
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(12)
            .width_request(300)
            .build();
        column.append(&list.list);
        column.append(&add);
        let both = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        both.append(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&column)
                .build(),
        );
        inspector.widget().set_hexpand(true);
        both.append(inspector.widget());

        let empty_add = gtk::Button::builder()
            .label(t(keys::ACTION_ADD_DEVICE))
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        let weak = Rc::downgrade(ui);
        empty_add.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.add_device();
            }
        });
        let empty = adw::StatusPage::builder()
            .icon_name("computer-symbolic")
            .title(t(keys::DEVICES_NONE_TITLE))
            .description(t(keys::DEVICES_NONE_DETAIL))
            .child(&empty_add)
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&both, Some("list"));
        stack.add_named(&empty, Some("empty"));

        let devices = Devices {
            stack,
            list,
            inspector,
        };
        devices.list.connect_selected(&devices.inspector, ui);
        devices
    }

    pub fn widget(&self) -> &gtk::Stack {
        &self.stack
    }

    /// Selects `id`, as a click on its row does; shown with the next model.
    pub fn select(&self, id: &str) {
        *self.list.selected.borrow_mut() = Some(id.to_owned());
    }

    #[cfg(test)]
    pub fn list(&self) -> &Rc<DeviceList> {
        &self.list
    }

    pub fn update(&self, model: &Snapshot) {
        let ids: Vec<String> = model.status.devices.iter().map(|d| d.id.clone()).collect();
        let empty = ids.is_empty();
        let name = if empty { "empty" } else { "list" };
        if self.stack.visible_child_name().as_deref() != Some(name) {
            self.stack.set_visible_child_name(name);
        }
        {
            let mut selected = self.list.selected.borrow_mut();
            if selected.as_ref().is_none_or(|s| !ids.contains(s)) {
                *selected = ids.first().cloned();
            }
        }
        let now = SystemTime::now();
        self.list
            .update(model, &ids, |id| tr(&device_text::status(model, id, now)));
        self.inspector
            .update(model, self.list.selected.borrow().as_deref());
    }
}
