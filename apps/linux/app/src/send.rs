//! Sending files: Send Files… (choose files or folders, then the computer
//! they go to), and what a file manager hands over with `--send`
//! (Dolphin's Actions › Send with InputWeave), as the other two apps do.
//! Both go by `send_choice::choice`: with no computer connected, a notice;
//! with one that can take the files, at once; otherwise the picker, which
//! lists every other computer, those that can't take files off with why,
//! and follows the agent while it is open.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use inputweave_app_core::proto::{SendFiles, client_message::Command};
use inputweave_app_core::send_choice::{self, Choice, Target};
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;
use inputweave_app_core::{log, ui};

use crate::strings::{t, tr};
use crate::ui::Ui;

/// How long an app the file manager just started waits for the background
/// service before saying it is not responding.
const CONNECT_WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
pub enum What {
    Files,
    Folders,
}

fn paths(list: &gio::ListModel) -> Vec<String> {
    (0..list.n_items())
        .filter_map(|i| list.item(i).and_downcast::<gio::File>())
        .filter_map(|f| f.path())
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

pub fn choose_and_send(ui: &Rc<Ui>, what: What) {
    let window = ui.present_window();
    let parent: gtk::Widget = window.window().clone().upcast();
    if send_choice::choice(&ui.snapshot()) == Choice::Nowhere {
        nowhere_dialog(&t(keys::SEND_NOWHERE)).present(Some(&parent));
        return;
    }
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let dialog = gtk::FileDialog::builder()
            .modal(true)
            .title(t(match what {
                What::Files => keys::SEND_FILES_CHOOSE_TITLE,
                What::Folders => keys::SEND_FILES_CHOOSE_FOLDERS,
            }))
            .build();
        let window = parent.downcast_ref::<gtk::Window>();
        let chosen = match what {
            What::Files => dialog.open_multiple_future(window).await,
            What::Folders => dialog.select_multiple_folders_future(window).await,
        };
        let Ok(chosen) = chosen else { return };
        let paths = paths(&chosen);
        if paths.is_empty() {
            return;
        }
        // Where they go now that they are chosen.
        deliver(&ui, Some(&parent), paths, false, Then::none());
    });
}

/// What runs once: when sending from the file manager is over (the files
/// went, the picker or a notice closed), as the app started for it then
/// checks that it can be found again (`ui::launched`).
#[derive(Clone)]
pub struct Then(Rc<RefCell<Option<Done>>>);

type Done = Box<dyn FnOnce()>;

impl Then {
    pub fn new(done: impl FnOnce() + 'static) -> Then {
        Then(Rc::new(RefCell::new(Some(Box::new(done)))))
    }

    pub fn none() -> Then {
        Then(Rc::default())
    }

    pub fn run(&self) {
        let done = self.0.borrow_mut().take();
        if let Some(done) = done {
            done();
        }
    }

    /// Runs once `dialog` closes.
    fn after(&self, dialog: &impl IsA<adw::Dialog>) {
        let then = self.clone();
        dialog.connect_closed(move |_| then.run());
    }
}

/// Sends what a file manager passed (`--send`), once the app is connected
/// to the background service; `then` runs when that is over.
pub fn from_file_manager(ui: &Rc<Ui>, paths: Vec<String>, then: Then) {
    // Counted, not named: the log never holds file names.
    log::info(&format!(
        "sending {} items from the file manager",
        paths.len()
    ));
    if paths.is_empty() {
        then.run();
        return;
    }
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        // Started by the file manager, the app is still connecting.
        if !ui.connected_within(CONNECT_WAIT).await {
            let dialog = nowhere_dialog(&t(keys::TITLE_NOT_RESPONDING));
            then.after(&dialog);
            dialog.present(None::<&gtk::Widget>);
            return;
        }
        deliver(&ui, None, paths, true, then);
    });
}

/// Where `paths` go: into the picker already open, which gets them too
/// (and says where in the HUD if either request would have); else the
/// notice when no computer is connected, at once to the only one that can
/// take them, or the picker. `announce`: say in the HUD where they went.
/// `then` runs once this request is over.
fn deliver(
    ui: &Rc<Ui>,
    parent: Option<&gtk::Widget>,
    paths: Vec<String>,
    announce: bool,
    then: Then,
) {
    if let Some(open) = ui.send_picker() {
        open.add(&paths, announce);
        open.raise();
        then.run();
        return;
    }
    let model = ui.snapshot();
    match send_choice::choice(&model) {
        Choice::Nowhere => {
            let dialog = nowhere_dialog(&t(keys::SEND_NOWHERE));
            then.after(&dialog);
            dialog.present(parent);
        }
        Choice::Only { id, name } => {
            send_files(ui, id, name, paths, announce);
            then.run();
        }
        Choice::Ask => {
            let picker = Picker::new(&model, paths, announce);
            then.after(&picker.dialog);
            picker.present(ui, parent);
        }
    }
}

/// A dialog with only OK: no computer is connected, or the service is not
/// responding.
pub(crate) fn nowhere_dialog(heading: &str) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(Some(heading), None);
    dialog.add_response("ok", &t(keys::OK));
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("ok");
    dialog
}

/// A computer in the picker: its row and radio button.
pub(crate) struct PickerRow {
    pub id: String,
    pub name: String,
    pub available: bool,
    pub row: adw::ActionRow,
    pub radio: gtk::CheckButton,
}

/// "Send to which computer?": every other computer, one radio button each,
/// those that can't take files listed but off, with why. It follows the
/// agent while it is open (`update`, from `Ui::refresh`): a computer that
/// goes offline is off, one that connects can be chosen, and the choice
/// stays while it can take the files (`send_choice::chosen`). Send is off
/// while none is chosen.
pub(crate) struct Picker {
    pub dialog: adw::AlertDialog,
    list: gtk::ListBox,
    pub rows: RefCell<Vec<PickerRow>>,
    paths: RefCell<Vec<String>>,
    /// Say in the HUD where the files went: a request from the file
    /// manager is among them.
    announce: Cell<bool>,
    /// The computer the user clicked, which comes back when it can take
    /// files again.
    clicked: Rc<RefCell<Option<String>>>,
    /// The computer shown chosen, by the user or by default.
    shown: RefCell<Option<String>>,
    /// Set while the picker sets its own radio buttons.
    setting: Rc<Cell<bool>>,
}

impl Picker {
    pub fn new(model: &Snapshot, paths: Vec<String>, announce: bool) -> Rc<Picker> {
        let dialog = adw::AlertDialog::new(
            Some(&t(keys::SEND_PICK_TITLE)),
            Some(&tr(&send_choice::summary(&paths))),
        );
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        list.update_property(&[gtk::accessible::Property::Label(&t(keys::SEND_PICK_TITLE))]);
        dialog.set_extra_child(Some(&list));
        dialog.add_response("cancel", &t(keys::CANCEL));
        dialog.add_response("send", &t(keys::SEND_FILES_SEND));
        dialog.set_response_appearance("send", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("send"));
        dialog.set_close_response("cancel");
        let picker = Rc::new(Picker {
            dialog,
            list,
            rows: RefCell::default(),
            paths: RefCell::new(paths),
            announce: Cell::new(announce),
            clicked: Rc::default(),
            shown: RefCell::default(),
            setting: Rc::default(),
        });
        picker.update(model);
        picker
    }

    /// Brings the rows up to `model`: made again when the computers or
    /// which can take files changed, else their words updated in place;
    /// then the radio buttons and Send follow the choice.
    pub fn update(&self, model: &Snapshot) {
        let targets = send_choice::targets(model);
        let same = {
            let rows = self.rows.borrow();
            rows.len() == targets.len()
                && rows
                    .iter()
                    .zip(&targets)
                    .all(|(r, t)| r.id == t.id && r.available == t.available)
        };
        if same {
            for (row, target) in self.rows.borrow_mut().iter_mut().zip(&targets) {
                row.name = target.name.clone();
                row.row.set_title(&target.name);
                row.row.set_subtitle(&tr(&target.status));
            }
        } else {
            self.rebuild(&targets);
        }
        let clicked = self.clicked.borrow().clone();
        let shown = self.shown.borrow().clone();
        let chosen = send_choice::chosen(clicked.as_deref(), shown.as_deref(), &targets)
            .map(|t| t.id.clone());
        *self.shown.borrow_mut() = chosen.clone();
        self.setting.set(true);
        for row in self.rows.borrow().iter() {
            row.radio.set_active(chosen.as_ref() == Some(&row.id));
        }
        self.setting.set(false);
        self.dialog.set_response_enabled("send", chosen.is_some());
    }

    fn rebuild(&self, targets: &[Target]) {
        self.list.remove_all();
        let mut rows = self.rows.borrow_mut();
        rows.clear();
        let mut first: Option<gtk::CheckButton> = None;
        for target in targets {
            let radio = gtk::CheckButton::builder()
                .valign(gtk::Align::Center)
                .build();
            match &first {
                Some(first) => radio.set_group(Some(first)),
                None => first = Some(radio.clone()),
            }
            let (clicked, setting, id) = (
                self.clicked.clone(),
                self.setting.clone(),
                target.id.clone(),
            );
            radio.connect_toggled(move |radio| {
                if !setting.get() && radio.is_active() {
                    *clicked.borrow_mut() = Some(id.clone());
                }
            });
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .title(&target.name)
                .subtitle(tr(&target.status))
                .activatable_widget(&radio)
                .sensitive(target.available)
                .build();
            // Each prefix goes before the last: the radio button, then the
            // computer's icon.
            row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
            row.add_prefix(&radio);
            self.list.append(&row);
            rows.push(PickerRow {
                id: target.id.clone(),
                name: target.name.clone(),
                available: target.available,
                row,
                radio,
            });
        }
    }

    /// The computer chosen, with its name: one that can take the files,
    /// as its radio button shows.
    pub fn chosen(&self) -> Option<(String, String)> {
        self.rows
            .borrow()
            .iter()
            .find(|r| r.available && r.radio.is_active())
            .map(|r| (r.id.clone(), r.name.clone()))
    }

    /// What it sends.
    pub fn paths(&self) -> Vec<String> {
        self.paths.borrow().clone()
    }

    /// Adds the items of another request, each once, and says so under the
    /// title; `announce`: that request would have said in the HUD where
    /// they went, which then covers them all.
    pub fn add(&self, more: &[String], announce: bool) {
        if announce {
            self.announce.set(true);
        }
        let all = send_choice::merged(&self.paths.borrow(), more);
        self.dialog.set_body(&tr(&send_choice::summary(&all)));
        *self.paths.borrow_mut() = all;
    }

    /// Brings the window it is in to the front.
    pub fn raise(&self) {
        if let Some(window) = self.dialog.root().and_downcast::<gtk::Window>() {
            window.present();
        }
    }

    /// Shows the picker over `parent`, or as a window of its own, kept up to
    /// date by the app while it is open; Send sends every item to the
    /// computer chosen, saying so in the HUD once they are on their way when
    /// a request asked it to.
    pub fn present(self: &Rc<Self>, ui: &Rc<Ui>, parent: Option<&gtk::Widget>) {
        let (picker, weak) = (Rc::downgrade(self), Rc::downgrade(ui));
        self.dialog.connect_response(None, move |_, response| {
            if response != "send" {
                return;
            }
            let (Some(picker), Some(ui)) = (picker.upgrade(), weak.upgrade()) else {
                return;
            };
            let Some((id, name)) = picker.chosen() else {
                return;
            };
            send_files(&ui, id, name, picker.paths(), picker.announce.get());
        });
        ui.keep_send_picker(self);
        self.dialog.present(parent);
    }
}

pub(crate) fn send(ui: &Rc<Ui>, device_id: String, name: String, paths: Vec<String>) {
    send_files(ui, device_id, name, paths, false);
}

/// Sends, and with `announce` says where in the HUD once the service took
/// the files.
fn send_files(ui: &Rc<Ui>, device_id: String, name: String, paths: Vec<String>, announce: bool) {
    let items = paths.len();
    let hud_name = name.clone();
    send_then(ui, device_id, name, paths, move |ui| {
        if announce {
            ui.overlay()
                .hud(ui, &tr(&send_choice::sending(items, &hud_name)));
        }
    });
}

fn send_then(
    ui: &Rc<Ui>,
    device_id: String,
    name: String,
    paths: Vec<String>,
    sent: impl FnOnce(&Rc<Ui>) + 'static,
) {
    log::info(&format!("sending {} items to {device_id}", paths.len()));
    let weak = Rc::downgrade(ui);
    ui.command_then(
        Command::SendFiles(SendFiles { device_id, paths }),
        move |result| {
            let Some(ui) = weak.upgrade() else {
                return true;
            };
            if result.is_ok() {
                sent(&ui);
                return true;
            }
            let code = result.as_ref().err().and_then(|e| e.code());
            let dialog = adw::AlertDialog::new(
                Some(&tr(&ui!(keys::SEND_FILES_FAILED, name.clone()))),
                Some(&tr(&send_choice::refusal(code, &name))),
            );
            dialog.add_response("ok", &t(keys::OK));
            // Over the main window while it is shown, else a window of its
            // own: sent from the file manager, the app may have none.
            let window = ui.shown_window();
            dialog.present(window.as_ref());
            true
        },
    );
}
