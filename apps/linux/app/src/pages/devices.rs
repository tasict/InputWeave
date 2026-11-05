//! Devices: the other computers, with their status, latency and jump
//! shortcut, and the inspector for the one selected; an empty state while
//! there are none.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use inputweave_app_core::device_text;
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;

use super::inspector::Inspector;
use super::{set_subtitle, set_title};
use crate::strings::{t, tr};
use crate::ui::Ui;

/// A list of computers whose rows are kept while the same ones are listed.
pub struct DeviceList {
    pub list: gtk::ListBox,
    rows: RefCell<Vec<(String, adw::ActionRow, gtk::Label, gtk::Image)>>,
    pub selected: RefCell<Option<String>>,
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
            rows.len() == ids.len() && rows.iter().zip(ids).all(|(r, id)| r.0 == *id)
        };
        if !same {
            let mut rows = self.rows.borrow_mut();
            for (_, row, _, _) in rows.drain(..) {
                self.list.remove(&row);
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
                row.add_suffix(&here);
                row.add_suffix(&shortcut);
                self.list.append(&row);
                rows.push((id.clone(), row, shortcut, here));
            }
        }
        let cursor = model.cursor_device_id().to_owned();
        let selected = self.selected.borrow().clone();
        for (id, row, shortcut, here) in self.rows.borrow().iter() {
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
            .find(|(_, r, _, _)| r.upcast_ref::<gtk::ListBoxRow>() == row)
            .map(|(id, ..)| id.clone())
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
        self.list
            .update(model, &ids, |id| tr(&device_text::status(model, id)));
        self.inspector
            .update(model, self.list.selected.borrow().as_deref());
    }
}
