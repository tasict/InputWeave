//! Arrangement: the canvas and, for the
//! selected computer, the inspector; Identify Screens, "Synced to N
//! devices" for a moment after the arrangement changed, and Add Device….
//! "Rename…" on the canvas puts the cursor in the inspector's name field.
//! Where the cursor leaves only across edges (Wayland), a line says why
//! "Move Cursor Here" is off.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use inputweave_app_core::device_text;
use inputweave_app_core::proto::{IdentifyScreens, client_message::Command};
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;

use super::inspector::Inspector;
use crate::canvas::ArrangementCanvas;
use crate::strings::{t, tr};
use crate::ui::Ui;

const SYNCED_FOR: Duration = Duration::from_secs(2);

pub struct Arrangement {
    root: gtk::Box,
    canvas: Rc<ArrangementCanvas>,
    inspector: Rc<Inspector>,
    synced: gtk::Label,
    edges_only: gtk::Label,
    /// Follows the arrangement's version on every model, shown or not.
    synced_watch: RefCell<device_text::SyncedWatch>,
    /// The timer that hides "Synced…", while it runs.
    hide_synced: Rc<RefCell<Option<glib::SourceId>>>,
    last: RefCell<Option<Arc<Snapshot>>>,
}

impl Arrangement {
    pub fn new(ui: &Rc<Ui>) -> Rc<Arrangement> {
        let canvas = ArrangementCanvas::new(ui);
        let inspector = Inspector::new(ui);
        let identify = gtk::Button::builder()
            .label(t(keys::IDENTIFY_SCREENS))
            .tooltip_text(t(keys::IDENTIFY_SCREENS_HINT))
            .build();
        let weak = Rc::downgrade(ui);
        identify.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.command(Command::IdentifyScreens(IdentifyScreens {}));
            }
        });
        let add = gtk::Button::builder()
            .label(t(keys::ACTION_ADD_DEVICE))
            .build();
        let weak = Rc::downgrade(ui);
        add.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.add_device();
            }
        });
        let synced = gtk::Label::builder()
            .css_classes(["dim-label"])
            .visible(false)
            .hexpand(true)
            .xalign(0.0)
            .build();
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.append(&identify);
        buttons.append(&synced);
        buttons.append(&add);
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(16)
            .margin_end(16)
            .hexpand(true)
            .build();
        let edges_only = gtk::Label::builder()
            .label(t(keys::EDGES_ONLY))
            .css_classes(["dim-label"])
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .build();
        column.append(canvas.widget());
        column.append(&edges_only);
        column.append(&buttons);
        // As the Windows app: the canvas takes the room, the inspector a
        // column of 260 beside a line.
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.append(&column);
        root.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        inspector.widget().set_width_request(260);
        inspector.widget().set_hexpand(false);
        root.append(inspector.widget());
        let page = Rc::new(Arrangement {
            root,
            canvas,
            inspector,
            synced,
            edges_only,
            synced_watch: RefCell::default(),
            hide_synced: Rc::default(),
            last: RefCell::new(None),
        });
        let weak = Rc::downgrade(&page);
        page.canvas.connect_selected(move |id| {
            if let Some(page) = weak.upgrade()
                && let Some(model) = page.last.borrow().clone()
            {
                page.inspector.update(&model, id.as_deref());
            }
        });
        let weak = Rc::downgrade(&page);
        page.canvas.connect_rename(move |id| {
            let Some(page) = weak.upgrade() else { return };
            page.canvas.select(Some(id.clone()));
            if let Some(model) = page.last.borrow().clone() {
                page.inspector.update(&model, Some(&id));
            }
            if let Some(row) = page.inspector.name_row() {
                row.grab_focus();
                if let Some(text) = row.delegate() {
                    text.select_region(0, -1);
                }
            }
        });
        page
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// Takes in every model, whether the page is `shown` or not, so
    /// "Synced to N devices" is said only for changes made while it is.
    pub fn follow(&self, model: &Snapshot, shown: bool) {
        let Some(text) = self.synced_watch.borrow_mut().update(model, shown) else {
            return;
        };
        let text = tr(&text);
        self.synced.set_label(&text);
        self.synced.set_visible(true);
        // Said to screen readers too, as the other two apps' live regions.
        self.synced
            .announce(&text, gtk::AccessibleAnnouncementPriority::Medium);
        if let Some(source) = self.hide_synced.borrow_mut().take() {
            source.remove();
        }
        let label = self.synced.downgrade();
        let running = self.hide_synced.clone();
        let source = glib::timeout_add_local_once(SYNCED_FOR, move || {
            running.borrow_mut().take();
            if let Some(label) = label.upgrade() {
                label.set_visible(false);
            }
        });
        *self.hide_synced.borrow_mut() = Some(source);
    }

    #[cfg(test)]
    pub fn synced_shown(&self) -> bool {
        self.synced.is_visible()
    }

    pub fn update(&self, model: &Arc<Snapshot>) {
        *self.last.borrow_mut() = Some(model.clone());
        // The selection: what the user picked, else this computer.
        let ids: Vec<&str> = model
            .arrangement
            .devices
            .iter()
            .map(|d| d.id.as_str())
            .collect();
        if self
            .canvas
            .selection()
            .is_none_or(|s| !ids.contains(&s.as_str()))
        {
            let this = model.this_device_id().to_owned();
            self.canvas
                .select(ids.contains(&this.as_str()).then_some(this));
        }
        self.canvas.update(model);
        self.edges_only.set_visible(model.edges_only());
        self.inspector
            .update(model, self.canvas.selection().as_deref());
    }
}

impl Drop for Arrangement {
    fn drop(&mut self) {
        if let Some(source) = self.hide_synced.borrow_mut().take() {
            source.remove();
        }
    }
}
