//! File Transfers: where received files go and whether to ask first,
//! sending files, and the recent transfers.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use inputweave_app_core::proto::{ClearTransfers, SetFileSettings, client_message::Command};
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;
use inputweave_app_core::transfer_text;

use super::{Quiet, set_subtitle};
use crate::send;
use crate::strings::t;
use crate::transfer_row::TransferRow;
use crate::ui::Ui;

pub struct Files {
    page: adw::PreferencesPage,
    quiet: Quiet,
    folder: adw::ActionRow,
    choose: gtk::Button,
    not_receiving: adw::ActionRow,
    asking: adw::PreferencesGroup,
    automatically: gtk::CheckButton,
    ask: gtk::CheckButton,
    list: gtk::ListBox,
    none: gtk::Label,
    clear: gtk::Button,
    rows: RefCell<Vec<Rc<TransferRow>>>,
    ui: std::rc::Weak<Ui>,
}

impl Files {
    pub fn new(ui: &Rc<Ui>) -> Files {
        let page = adw::PreferencesPage::new();
        let receiving = adw::PreferencesGroup::builder()
            .title(t(keys::FILES_RECEIVING))
            .build();
        let choose = super::row_button(&t(keys::FILES_CHOOSE));
        let folder = adw::ActionRow::builder()
            .use_markup(false)
            .title(t(keys::FILES_SAVE_IN))
            .subtitle_selectable(true)
            .build();
        folder.add_suffix(&choose);
        let not_receiving = adw::ActionRow::builder()
            .use_markup(false)
            .title(t(keys::TRANSFER_NOT_RECEIVING))
            .build();
        receiving.add(&folder);
        receiving.add(&not_receiving);
        page.add(&receiving);

        let asking = adw::PreferencesGroup::builder()
            .title(t(keys::FILES_WHEN_SENT))
            .build();
        let automatically = gtk::CheckButton::new();
        let ask = gtk::CheckButton::builder().group(&automatically).build();
        for (check, key) in [
            (&automatically, keys::FILES_AUTOMATICALLY),
            (&ask, keys::FILES_ASK_EACH_TIME),
        ] {
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .title(t(key))
                .activatable_widget(check)
                .build();
            row.add_prefix(check);
            asking.add(&row);
        }
        page.add(&asking);

        let sending = adw::PreferencesGroup::builder()
            .title(t(keys::FILES_SENDING))
            .description(t(keys::FILES_SEND_HINT))
            .build();
        let buttons = gtk::Box::builder().spacing(12).build();
        let send_files = gtk::Button::with_label(&t(keys::FILES_SEND_BUTTON));
        let send_folders = gtk::Button::with_label(&t(keys::SEND_FILES_FOLDERS));
        buttons.append(&send_files);
        buttons.append(&send_folders);
        sending.add(&buttons);
        page.add(&sending);

        let recent = adw::PreferencesGroup::builder()
            .title(t(keys::FILES_RECENT))
            .build();
        let clear = gtk::Button::builder()
            .label(t(keys::FILES_CLEAR_HISTORY))
            .css_classes(["flat"])
            .build();
        recent.set_header_suffix(Some(&clear));
        let list = gtk::ListBox::builder()
            .css_classes(["boxed-list"])
            .selection_mode(gtk::SelectionMode::None)
            .build();
        let none = gtk::Label::builder()
            .label(t(keys::FILES_NONE))
            .css_classes(["dim-label"])
            .halign(gtk::Align::Start)
            .build();
        recent.add(&list);
        recent.add(&none);
        page.add(&recent);

        let files = Files {
            page,
            quiet: Quiet::default(),
            folder,
            choose,
            not_receiving,
            asking,
            automatically,
            ask,
            list,
            none,
            clear,
            rows: RefCell::new(Vec::new()),
            ui: Rc::downgrade(ui),
        };
        files.connect(ui, &send_files, &send_folders);
        files
    }

    fn connect(&self, ui: &Rc<Ui>, send_files: &gtk::Button, send_folders: &gtk::Button) {
        let weak = Rc::downgrade(ui);
        send_files.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                send::choose_and_send(&ui, send::What::Files);
            }
        });
        let weak = Rc::downgrade(ui);
        send_folders.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                send::choose_and_send(&ui, send::What::Folders);
            }
        });
        let (weak, quiet) = (Rc::downgrade(ui), self.quiet.clone());
        self.ask.connect_toggled(move |ask| {
            if quiet.is_set() {
                return;
            }
            if let Some(ui) = weak.upgrade() {
                let folder = ui.snapshot().transfers.receive_folder.clone();
                ui.command(Command::SetFileSettings(SetFileSettings {
                    receive_folder: folder,
                    ask_before_receiving: ask.is_active(),
                }));
            }
        });
        let weak = Rc::downgrade(ui);
        self.choose.connect_clicked(move |button| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.snapshot();
            let dialog = gtk::FileDialog::builder()
                .title(t(keys::FILES_CHOOSE_TITLE))
                .modal(true)
                .build();
            if !model.transfers.receive_folder.is_empty() {
                dialog.set_initial_folder(Some(&gio::File::for_path(
                    &model.transfers.receive_folder,
                )));
            }
            let window = button.root().and_downcast::<gtk::Window>();
            let ask = model.transfers.ask_before_receiving;
            gtk::glib::spawn_future_local(async move {
                let Ok(folder) = dialog.select_folder_future(window.as_ref()).await else {
                    return;
                };
                let Some(path) = folder.path() else { return };
                ui.command(Command::SetFileSettings(SetFileSettings {
                    receive_folder: path.to_string_lossy().into_owned(),
                    ask_before_receiving: ask,
                }));
            });
        });
        let weak = Rc::downgrade(ui);
        self.clear.connect_clicked(move |button| {
            let Some(ui) = weak.upgrade() else { return };
            let dialog = adw::AlertDialog::new(
                Some(&t(keys::FILES_CLEAR_TITLE)),
                Some(&t(keys::FILES_CLEAR_MESSAGE)),
            );
            dialog.add_response("cancel", &t(keys::CANCEL));
            dialog.add_response("clear", &t(keys::FILES_CLEAR_CONFIRM));
            dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            dialog.connect_response(None, move |_, response| {
                if response == "clear" {
                    ui.command(Command::ClearTransfers(ClearTransfers {}));
                }
            });
            dialog.present(Some(button));
        });
    }

    pub fn widget(&self) -> &adw::PreferencesPage {
        &self.page
    }

    pub fn update(&self, model: &Snapshot) {
        let transfers = &model.transfers;
        let receives = !transfers.receive_folder.is_empty();
        self.folder.set_visible(receives);
        self.not_receiving.set_visible(!receives);
        self.asking.set_visible(receives);
        set_subtitle(&self.folder, &transfers.receive_folder);
        if self.ask.is_active() != transfers.ask_before_receiving {
            self.quiet.run(|| {
                if transfers.ask_before_receiving {
                    self.ask.set_active(true);
                } else {
                    self.automatically.set_active(true);
                }
            });
        } else if !self.ask.is_active() && !self.automatically.is_active() {
            self.quiet.run(|| self.automatically.set_active(true));
        }

        // By reference: nothing of the list is copied on a refresh.
        let ordered = transfer_text::ordered(&transfers.transfers);
        self.clear
            .set_sensitive(transfer_text::can_clear_history(&transfers.transfers));
        self.none.set_visible(ordered.is_empty());
        self.list.set_visible(!ordered.is_empty());
        let Some(ui) = self.ui.upgrade() else { return };
        let mut rows = self.rows.borrow_mut();
        let same = rows.len() == ordered.len()
            && rows
                .iter()
                .zip(&ordered)
                .all(|(r, t)| r.id == t.transfer_id);
        if !same {
            let mut kept: HashMap<String, Rc<TransferRow>> = HashMap::new();
            for row in rows.drain(..) {
                self.list.remove(row.widget());
                kept.insert(row.id.clone(), row);
            }
            for transfer in &ordered {
                let row = kept
                    .remove(&transfer.transfer_id)
                    .unwrap_or_else(|| TransferRow::new(&transfer.transfer_id));
                self.list.append(row.widget());
                rows.push(row);
            }
        }
        for (row, transfer) in rows.iter().zip(&ordered) {
            row.update(&ui, model, transfer);
        }
    }
}
