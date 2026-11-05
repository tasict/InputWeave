//! One transfer in the list: its name and direction, a folder's file
//! count, what it is doing or how it ended, a progress bar while it moves,
//! a folder's failures, and the buttons for it.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use inputweave_app_core::proto::{
    AnswerFileOffer, CancelTransfer, FileTransfer, RemoveTransfer, TransferState,
    client_message::Command,
};
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;
use inputweave_app_core::{transfer_text, ui};

use crate::pages::set_text;
use crate::strings::{t, tr};
use crate::ui::Ui;

pub struct TransferRow {
    pub id: String,
    row: gtk::ListBoxRow,
    icon: gtk::Image,
    title: gtk::Label,
    line: gtk::Label,
    detail: gtk::Label,
    progress: gtk::ProgressBar,
    buttons: gtk::Box,
    failures: gtk::Expander,
    failure_list: gtk::Box,
    shown_buttons: RefCell<String>,
    shown_failures: RefCell<String>,
}

impl TransferRow {
    pub fn new(id: &str) -> Rc<TransferRow> {
        let icon = gtk::Image::builder()
            .pixel_size(32)
            .valign(gtk::Align::Start)
            .build();
        let label = |classes: &[&str]| {
            gtk::Label::builder()
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .css_classes(classes.iter().map(|c| c.to_string()).collect::<Vec<_>>())
                .build()
        };
        let title = label(&["heading"]);
        title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        title.set_wrap(false);
        let line = label(&["dim-label", "iw-numeric"]);
        let detail = label(&["dim-label", "iw-numeric"]);
        let progress = gtk::ProgressBar::builder().visible(false).build();
        let failure_list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let failures = gtk::Expander::builder()
            .label(t(keys::TRANSFER_SHOW_FAILURES))
            .child(&failure_list)
            .visible(false)
            .build();
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(3)
            .hexpand(true)
            .build();
        for w in [
            title.upcast_ref::<gtk::Widget>(),
            line.upcast_ref(),
            detail.upcast_ref(),
            progress.upcast_ref(),
            failures.upcast_ref(),
        ] {
            text.append(w);
        }
        let buttons = gtk::Box::builder()
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        let content = gtk::Box::builder()
            .spacing(12)
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&icon);
        content.append(&text);
        content.append(&buttons);
        let row = gtk::ListBoxRow::builder()
            .child(&content)
            .activatable(false)
            .selectable(false)
            .build();
        Rc::new(TransferRow {
            id: id.to_owned(),
            row,
            icon,
            title,
            line,
            detail,
            progress,
            buttons,
            failures,
            failure_list,
            shown_buttons: RefCell::new(String::new()),
            shown_failures: RefCell::new(String::new()),
        })
    }

    pub fn widget(&self) -> &gtk::ListBoxRow {
        &self.row
    }

    pub fn update(&self, ui: &Rc<Ui>, model: &Snapshot, transfer: &FileTransfer) {
        let device = model.name_of(&transfer.device_id);
        let icon = if transfer.folder {
            "folder-symbolic"
        } else {
            "text-x-generic-symbolic"
        };
        if self.icon.icon_name().as_deref() != Some(icon) {
            self.icon.set_icon_name(Some(icon));
        }
        set_text(&self.title, &transfer_text::title(transfer, &device));
        let line = transfer_text::folder_line(transfer).map(|l| tr(&l));
        set_text(&self.line, line.as_deref().unwrap_or(""));
        self.line.set_visible(line.is_some());
        let problem = transfer_text::folder_line_is_problem(transfer);
        if self.line.has_css_class("error") != problem {
            if problem {
                self.line.add_css_class("error");
            } else {
                self.line.remove_css_class("error");
            }
        }
        let detail = transfer_text::detail(transfer, &device).map(|d| tr(&d));
        set_text(&self.detail, detail.as_deref().unwrap_or(""));
        self.detail.set_visible(detail.is_some());
        let problem = transfer_text::detail_is_problem(transfer);
        if self.detail.has_css_class("error") != problem {
            if problem {
                self.detail.add_css_class("error");
            } else {
                self.detail.remove_css_class("error");
            }
        }
        match transfer_text::fraction(transfer) {
            Some(f) => {
                self.progress.set_fraction(f);
                self.progress.set_visible(true);
            }
            None => self.progress.set_visible(false),
        }
        self.update_failures(transfer);
        self.update_buttons(ui, transfer);
    }

    fn update_failures(&self, transfer: &FileTransfer) {
        let lines: Vec<String> = transfer_text::failure_lines(transfer)
            .iter()
            .map(tr)
            .collect();
        let more = transfer_text::unlisted_failures(transfer);
        let key = format!("{lines:?}{more}");
        if *self.shown_failures.borrow() == key {
            return;
        }
        *self.shown_failures.borrow_mut() = key;
        while let Some(child) = self.failure_list.first_child() {
            self.failure_list.remove(&child);
        }
        let shown = transfer_text::folder_line_is_problem(transfer) && !lines.is_empty();
        for line in &lines {
            self.failure_list.append(
                &gtk::Label::builder()
                    .label(line)
                    .halign(gtk::Align::Start)
                    .wrap(true)
                    .css_classes(["caption"])
                    .build(),
            );
        }
        if more > 0 {
            self.failure_list.append(
                &gtk::Label::builder()
                    .label(tr(&ui!(keys::TRANSFER_FAILURES_MORE, more.to_string())))
                    .halign(gtk::Align::Start)
                    .css_classes(["caption", "dim-label"])
                    .build(),
            );
        }
        self.failures.set_visible(shown);
    }

    fn update_buttons(&self, ui: &Rc<Ui>, transfer: &FileTransfer) {
        let asked = !transfer.outgoing && transfer.state() == TransferState::WaitingForAnswer;
        let actions = transfer_text::actions(transfer);
        let key = format!("{asked}{actions:?}{}", transfer.path);
        if *self.shown_buttons.borrow() == key {
            return;
        }
        *self.shown_buttons.borrow_mut() = key;
        while let Some(child) = self.buttons.first_child() {
            self.buttons.remove(&child);
        }
        let id = transfer.transfer_id.clone();
        let command_button =
            |label: String, icon: Option<&str>, classes: &[&str], command: Command| {
                let button = match icon {
                    Some(icon) => gtk::Button::builder()
                        .icon_name(icon)
                        .tooltip_text(&label)
                        .css_classes(["flat"])
                        .build(),
                    None => gtk::Button::with_label(&label),
                };
                for class in classes {
                    button.add_css_class(class);
                }
                button.update_property(&[gtk::accessible::Property::Label(&label)]);
                let weak = Rc::downgrade(ui);
                button.connect_clicked(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        ui.command(command.clone());
                    }
                });
                button
            };
        if asked {
            self.buttons.append(&command_button(
                t(keys::TRANSFER_DECLINE),
                None,
                &[],
                Command::AnswerFileOffer(AnswerFileOffer {
                    transfer_id: id.clone(),
                    accept: false,
                }),
            ));
            self.buttons.append(&command_button(
                t(keys::TRANSFER_ACCEPT),
                None,
                &["suggested-action"],
                Command::AnswerFileOffer(AnswerFileOffer {
                    transfer_id: id.clone(),
                    accept: true,
                }),
            ));
            return;
        }
        if actions.cancel {
            self.buttons.append(&command_button(
                tr(&ui!(keys::TRANSFER_CANCEL_NAMED, transfer.name.clone())),
                Some("process-stop-symbolic"),
                &[],
                Command::CancelTransfer(CancelTransfer {
                    transfer_id: id.clone(),
                }),
            ));
        }
        if actions.show {
            let label = tr(&ui!(keys::TRANSFER_SHOW_NAMED, transfer.name.clone()));
            let button = gtk::Button::builder()
                .icon_name("folder-open-symbolic")
                .tooltip_text(&label)
                .css_classes(["flat"])
                .build();
            button.update_property(&[gtk::accessible::Property::Label(&label)]);
            let path = transfer.path.clone();
            button.connect_clicked(move |button| {
                let file = gio::File::for_path(&path);
                let launcher = gtk::FileLauncher::new(Some(&file));
                let window = button.root().and_downcast::<gtk::Window>();
                launcher.open_containing_folder(window.as_ref(), gio::Cancellable::NONE, |_| {});
            });
            self.buttons.append(&button);
        }
        if actions.remove {
            self.buttons.append(&command_button(
                tr(&ui!(keys::TRANSFER_REMOVE_NAMED, transfer.name.clone())),
                Some("edit-clear-symbolic"),
                &[],
                Command::RemoveTransfer(RemoveTransfer { transfer_id: id }),
            ));
        }
    }
}
