//! The License page: "Trial" with Buy a License and Enter
//! License Key, or the woven "Licensed" label with the computers on the
//! license; the Enter License Key dialog and the Remove License question.
//! Nothing else in the app speaks of the license but its sidebar row and
//! About.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use adw::prelude::*;
use inputweave_app_core::license::{Keeper, Problem, plausible_key};
use inputweave_app_core::license_text::{self, Entry, Notice, View};
use inputweave_app_core::polar;
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;

use crate::license;
use crate::strings::{t, tr};
use crate::ui::Ui;
use crate::woven::{Size, Woven};

/// How long the background service may stay silent before the dialog says
/// so, rather than spin.
const NO_SERVICE_AFTER: Duration = Duration::from_secs(10);

pub struct LicensePage {
    page: gtk::ScrolledWindow,
    trial: gtk::Box,
    trial_title: gtk::Label,
    notice: gtk::Label,
    /// Remove License's first try.
    spinner: gtk::Spinner,
    /// The customer portal, while this computer's seat waits for Polar.
    seat_portal: gtk::Button,
    trial_buttons: gtk::Box,
    /// Try Again beside them, after the license was found invalid.
    invalid_try: gtk::Button,
    full_buttons: gtk::Box,
    how: adw::PreferencesGroup,
    licensed: gtk::Box,
    woven: Rc<Woven>,
    to: gtk::Label,
    line: gtk::Label,
    computers: adw::PreferencesGroup,
    count: gtk::Label,
    rows: RefCell<Vec<adw::ActionRow>>,
    shown_rows: RefCell<Vec<(String, String)>>,
    dialog: RefCell<Option<Rc<KeyDialog>>>,
    ui: std::rc::Weak<Ui>,
}

fn pill(key: &str, suggested: bool) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(t(key))
        .css_classes(["pill"])
        .build();
    if suggested {
        button.add_css_class("suggested-action");
    }
    button
}

fn wrapped(css: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .wrap(true)
        .justify(gtk::Justification::Center)
        .max_width_chars(60)
        .css_classes(css.to_vec())
        .build()
}

impl LicensePage {
    pub fn new(ui: &Rc<Ui>) -> Rc<LicensePage> {
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(22)
            .margin_top(24)
            .margin_bottom(30)
            .margin_start(12)
            .margin_end(12)
            .build();

        // Trial.
        let trial = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .halign(gtk::Align::Center)
            .build();
        let trial_title = gtk::Label::builder()
            .label(t(keys::LICENSE_TRIAL))
            .css_classes(["title-1"])
            .build();
        let body = wrapped(&["dim-label"]);
        body.set_label(&t(keys::LICENSE_TRIAL_BODY));
        let notice = wrapped(&["card", "iw-license-notice"]);
        let spinner = gtk::Spinner::builder()
            .spinning(true)
            .halign(gtk::Align::Center)
            .visible(false)
            .build();
        let seat_portal = pill(keys::LICENSE_MANAGE, false);
        seat_portal.set_halign(gtk::Align::Center);
        seat_portal.set_visible(false);
        let price = wrapped(&[]);
        price.set_label(&t(keys::LICENSE_PRICE));
        let buy = pill(keys::LICENSE_BUY, true);
        let enter = pill(keys::LICENSE_ENTER, false);
        let trial_buttons = gtk::Box::builder()
            .spacing(10)
            .halign(gtk::Align::Center)
            .margin_top(6)
            .build();
        trial_buttons.append(&buy);
        trial_buttons.append(&enter);
        let invalid_try = pill(keys::LICENSE_TRY_AGAIN, false);
        invalid_try.set_visible(false);
        trial_buttons.append(&invalid_try);
        let manage_full = pill(keys::LICENSE_MANAGE, true);
        let try_again = pill(keys::LICENSE_TRY_AGAIN, false);
        let full_buttons = gtk::Box::builder()
            .spacing(10)
            .halign(gtk::Align::Center)
            .margin_top(6)
            .build();
        full_buttons.append(&manage_full);
        full_buttons.append(&try_again);
        trial.append(&trial_title);
        trial.append(&body);
        trial.append(&notice);
        trial.append(&spinner);
        trial.append(&seat_portal);
        trial.append(&price);
        trial.append(&trial_buttons);
        trial.append(&full_buttons);
        column.append(&trial);

        let how = adw::PreferencesGroup::builder()
            .title(t(keys::LICENSE_HOW_TITLE))
            .description(t(keys::LICENSE_POLAR_NOTE))
            .build();
        for (n, key) in [
            keys::LICENSE_HOW_BUY,
            keys::LICENSE_HOW_ENTER,
            keys::LICENSE_HOW_PAIRED,
        ]
        .into_iter()
        .enumerate()
        {
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .title(t(key))
                .title_lines(0)
                .build();
            let number = gtk::Label::builder()
                .label((n + 1).to_string())
                .css_classes(["iw-step", "iw-numeric"])
                .valign(gtk::Align::Center)
                .build();
            row.add_prefix(&number);
            how.add(&row);
        }
        column.append(&how);

        // Licensed.
        let licensed = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .halign(gtk::Align::Center)
            .build();
        let woven = Rc::new(Woven::new(Size::Large, &t(keys::LICENSE_LICENSED)));
        woven.widget().set_halign(gtk::Align::Center);
        let to = gtk::Label::builder().css_classes(["title-4"]).build();
        let line = wrapped(&["dim-label"]);
        licensed.append(woven.widget());
        licensed.append(&to);
        licensed.append(&line);
        let count = gtk::Label::builder()
            .css_classes(["dim-label", "iw-numeric"])
            .valign(gtk::Align::Center)
            .build();
        let computers = adw::PreferencesGroup::builder()
            .title(t(keys::LICENSE_COMPUTERS))
            .description(t(keys::LICENSE_LIST_NOTE))
            .header_suffix(&count)
            .build();
        let manage = pill(keys::LICENSE_MANAGE, false);
        let remove = pill(keys::LICENSE_REMOVE, false);
        remove.add_css_class("iw-remove");
        let licensed_buttons = gtk::Box::builder()
            .spacing(10)
            .halign(gtk::Align::Center)
            .build();
        licensed_buttons.append(&manage);
        licensed_buttons.append(&remove);
        let licensed_all = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(22)
            .build();
        licensed_all.append(&licensed);
        licensed_all.append(&computers);
        licensed_all.append(&licensed_buttons);
        column.append(&licensed_all);

        let clamp = adw::Clamp::builder()
            .maximum_size(640)
            .child(&column)
            .build();
        let page = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .build();

        let this = Rc::new(LicensePage {
            page,
            trial,
            trial_title,
            notice,
            spinner,
            seat_portal: seat_portal.clone(),
            trial_buttons,
            invalid_try: invalid_try.clone(),
            full_buttons,
            how,
            licensed: licensed_all,
            woven,
            to,
            line,
            computers,
            count,
            rows: RefCell::new(Vec::new()),
            shown_rows: RefCell::new(Vec::new()),
            dialog: RefCell::new(None),
            ui: Rc::downgrade(ui),
        });

        let weak = Rc::downgrade(ui);
        buy.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                license::open(&ui, polar::CHECKOUT_URL);
            }
        });
        for button in [&manage, &manage_full, &seat_portal] {
            let weak = Rc::downgrade(ui);
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    license::open(&ui, polar::PORTAL_URL);
                }
            });
        }
        for button in [&try_again, &invalid_try] {
            let weak = Rc::downgrade(ui);
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    license::try_again(&ui);
                }
            });
        }
        let (weak, page_weak) = (Rc::downgrade(ui), Rc::downgrade(&this));
        enter.connect_clicked(move |_| {
            if let (Some(ui), Some(page)) = (weak.upgrade(), page_weak.upgrade()) {
                page.open_dialog(&ui);
            }
        });
        let weak = Rc::downgrade(ui);
        remove.connect_clicked(move |button| {
            if let Some(ui) = weak.upgrade() {
                confirm_remove(&ui, button);
            }
        });
        this
    }

    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.page
    }

    /// Whether the trial part is shown (tests).
    #[cfg(test)]
    pub fn trial_shown(&self) -> bool {
        self.trial.is_visible()
    }

    pub fn update(&self, model: &Snapshot) {
        let license = model.status.license.as_ref();
        let ui = self.ui.upgrade();
        let fresh = Keeper::default();
        let keeper = ui.as_ref().map(|ui| ui.license.keeper());
        let keeper = keeper.as_deref().unwrap_or(&fresh);
        match license_text::view(license, &model.status.this_device_name, keeper) {
            View::Trial { notice } => {
                let quiet = notice.as_ref().is_some_and(Notice::quiet);
                self.trial.set_visible(true);
                self.how.set_visible(!quiet);
                self.licensed.set_visible(false);
                self.trial_title.set_label(&t(keys::LICENSE_TRIAL));
                let text = match notice {
                    Some(Notice::SeatsFull) => Some(keys::LICENSE_FULL_BODY),
                    Some(Notice::Invalid) => Some(keys::LICENSE_INVALID_BODY),
                    Some(Notice::Unavailable) => Some(keys::LICENSE_UNAVAILABLE_BODY),
                    Some(Notice::Removing) => Some(keys::LICENSE_REMOVING),
                    Some(Notice::RemoveFailed) => Some(keys::LICENSE_REMOVE_FAILED),
                    Some(Notice::SeatWaiting) => Some(keys::LICENSE_REMOVED_OFFLINE),
                    None => None,
                };
                if let Some(key) = text {
                    self.notice.set_label(&t(key));
                }
                self.notice.set_visible(text.is_some());
                self.spinner.set_visible(notice == Some(Notice::Removing));
                self.seat_portal
                    .set_visible(notice == Some(Notice::SeatWaiting));
                self.trial_buttons
                    .set_visible(!quiet && notice != Some(Notice::SeatsFull));
                self.full_buttons
                    .set_visible(notice == Some(Notice::SeatsFull));
                self.invalid_try
                    .set_visible(notice == Some(Notice::Invalid));
            }
            View::Licensed {
                to,
                line,
                computers,
                count,
            } => {
                self.trial.set_visible(false);
                self.how.set_visible(false);
                self.licensed.set_visible(true);
                self.woven.set_text(t(keys::LICENSE_LICENSED));
                self.to.set_visible(to.is_some());
                if let Some(to) = to {
                    self.to.set_label(&tr(&to));
                }
                self.line.set_label(&tr(&line));
                self.count.set_label(&tr(&count));
                let shown: Vec<(String, String)> = computers
                    .iter()
                    .map(|c| (c.name.clone(), tr(&c.note)))
                    .collect();
                if *self.shown_rows.borrow() != shown {
                    for row in self.rows.borrow_mut().drain(..) {
                        self.computers.remove(&row);
                    }
                    for (name, note) in &shown {
                        let row = adw::ActionRow::builder()
                            .use_markup(false)
                            .title(name.as_str())
                            .build();
                        row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
                        row.add_suffix(
                            &gtk::Label::builder()
                                .label(note.as_str())
                                .css_classes(["dim-label"])
                                .build(),
                        );
                        self.computers.add(&row);
                        self.rows.borrow_mut().push(row);
                    }
                    *self.shown_rows.borrow_mut() = shown;
                }
            }
        }
    }

    /// Brings the Enter License Key dialog up to date, whichever page is
    /// shown and whether or not the background service answers (the window
    /// shows its cover then, and the dialog stays over it).
    pub fn follow_dialog(&self, model: &Snapshot) {
        let dialog = self.dialog.borrow().clone();
        if let Some(dialog) = dialog
            && dialog.follow(model)
        {
            *self.dialog.borrow_mut() = None;
            self.woven.sew();
        }
    }

    fn open_dialog(self: &Rc<Self>, ui: &Rc<Ui>) {
        let dialog = KeyDialog::new(ui);
        let weak = Rc::downgrade(self);
        dialog.dialog.connect_closed(move |_| {
            if let Some(page) = weak.upgrade() {
                *page.dialog.borrow_mut() = None;
            }
        });
        dialog.dialog.present(Some(&ui.window().widget()));
        *self.dialog.borrow_mut() = Some(dialog);
    }

    /// For screenshots: the dialog, open over the page.
    pub fn dialog_for_screenshot(self: &Rc<Self>, ui: &Rc<Ui>) -> Rc<KeyDialog> {
        self.open_dialog(ui);
        self.dialog.borrow().clone().expect("just opened")
    }
}

/// Enter License Key: a field, what went wrong, Cancel and Activate. It
/// stays open while the key is activated and closes once it is.
pub struct KeyDialog {
    pub dialog: adw::Dialog,
    entry: gtk::Entry,
    error: gtk::Label,
    busy: gtk::Box,
    manage: gtk::Button,
    activate: gtk::Button,
    ui: std::rc::Weak<Ui>,
}

impl KeyDialog {
    fn new(ui: &Rc<Ui>) -> Rc<KeyDialog> {
        ui.license.forget_entered();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(26)
            .margin_bottom(22)
            .margin_start(24)
            .margin_end(24)
            .build();
        let title = gtk::Label::builder()
            .label(t(keys::LICENSE_ENTER_TITLE))
            .css_classes(["title-2"])
            .build();
        let help = wrapped(&["dim-label"]);
        help.set_label(&t(keys::LICENSE_KEY_HELP));
        let entry = gtk::Entry::builder()
            .placeholder_text("INPUTWEAVE-…")
            .css_classes(["monospace"])
            .activates_default(true)
            .build();
        entry.update_property(&[gtk::accessible::Property::Label(&t(
            keys::LICENSE_KEY_LABEL,
        ))]);
        let error = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .css_classes(["error"])
            .visible(false)
            .build();
        let busy = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::Center)
            .visible(false)
            .build();
        busy.append(&gtk::Spinner::builder().spinning(true).build());
        busy.append(
            &gtk::Label::builder()
                .label(t(keys::LICENSE_ACTIVATING))
                .css_classes(["dim-label"])
                .build(),
        );
        let manage = pill(keys::LICENSE_MANAGE, false);
        manage.set_visible(false);
        let cancel = pill(keys::CANCEL, false);
        let activate = pill(keys::LICENSE_ACTIVATE, true);
        activate.set_sensitive(false);
        let buttons = gtk::Box::builder()
            .spacing(10)
            .homogeneous(true)
            .margin_top(6)
            .build();
        buttons.append(&cancel);
        buttons.append(&activate);
        content.append(&title);
        content.append(&help);
        content.append(&entry);
        content.append(&error);
        content.append(&busy);
        content.append(&manage);
        content.append(&buttons);
        let dialog = adw::Dialog::builder()
            .title(t(keys::LICENSE_ENTER_TITLE))
            .content_width(400)
            .child(&content)
            .default_widget(&activate)
            .focus_widget(&entry)
            .build();
        let this = Rc::new(KeyDialog {
            dialog,
            entry,
            error,
            busy,
            manage,
            activate,
            ui: Rc::downgrade(ui),
        });
        let weak = Rc::downgrade(&this);
        this.entry.connect_changed(move |entry| {
            if let Some(d) = weak.upgrade() {
                d.activate.set_sensitive(!entry.text().trim().is_empty());
                d.error.set_visible(false);
                d.manage.set_visible(false);
            }
        });
        let d = this.dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(d) = d.upgrade() {
                d.close();
            }
        });
        let weak = Rc::downgrade(ui);
        this.manage.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                license::open(&ui, polar::PORTAL_URL);
            }
        });
        let weak = Rc::downgrade(&this);
        this.activate.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.send();
            }
        });
        this
    }

    fn send(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let key = self.entry.text().trim().to_owned();
        if !plausible_key(&key) {
            self.show_error(&t(keys::LICENSE_ERR_INVALID), false);
            return;
        }
        self.set_busy(true);
        let weak = Rc::downgrade(self);
        license::enter(&ui, key, move |result| {
            if let (Some(d), Err(code)) = (weak.upgrade(), result) {
                d.set_busy(false);
                d.show_error(&tr(&license_text::refused_key(code.as_deref())), false);
            }
        });
    }

    pub fn set_busy(&self, busy: bool) {
        self.busy.set_visible(busy);
        self.activate
            .set_sensitive(!busy && !self.entry.text().trim().is_empty());
        self.entry.set_sensitive(!busy);
        if busy {
            self.error.set_visible(false);
            self.manage.set_visible(false);
        }
    }

    pub fn show_error(&self, text: &str, manage: bool) {
        self.error.set_label(text);
        self.error.set_visible(true);
        self.manage.set_visible(manage);
    }

    /// For screenshots and tests.
    pub fn show_key(&self, key: &str) {
        self.entry.set_text(key);
    }

    #[cfg(test)]
    pub fn activate_for_tests(self: &Rc<Self>) {
        self.send();
    }

    /// Follows the key it sent; true once it is licensed (and the dialog
    /// closed).
    fn follow(&self, model: &Snapshot) -> bool {
        let Some(ui) = self.ui.upgrade() else {
            return false;
        };
        let unreachable = model.unreachable_since.is_some_and(|since| {
            SystemTime::now().duration_since(since).unwrap_or_default() >= NO_SERVICE_AFTER
        });
        let Some(entered) = ui.license.entered() else {
            // The agent has not taken the key yet.
            if unreachable && self.busy.is_visible() {
                self.set_busy(false);
                self.show_error(&t(keys::COMMAND_FAILED), false);
            }
            return false;
        };
        let problem = if unreachable {
            Some(Problem::NoService)
        } else if ui.license.cannot_save() {
            Some(Problem::Unsaved)
        } else {
            ui.license.keeper().problem_for(&entered)
        };
        match license_text::entry(&entered, model.status.license.as_ref(), problem) {
            Entry::Idle => false,
            Entry::Activating => {
                self.set_busy(true);
                false
            }
            Entry::Error { text, manage } => {
                self.set_busy(false);
                self.show_error(&tr(&text), manage);
                ui.license.forget_entered();
                false
            }
            // Tried again in the background: said, and still followed.
            Entry::Waiting { text } => {
                self.set_busy(false);
                self.show_error(&tr(&text), false);
                false
            }
            Entry::Done => {
                ui.license.forget_entered();
                self.dialog.close();
                true
            }
        }
    }
}

/// The Remove License question.
fn remove_question() -> adw::AlertDialog {
    let dialog = adw::AlertDialog::builder()
        .heading(t(keys::LICENSE_REMOVE_TITLE))
        .body(t(keys::LICENSE_REMOVE_BODY))
        .default_response("cancel")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[
        ("cancel", &t(keys::CANCEL)),
        ("remove", &t(keys::LICENSE_REMOVE_CONFIRM)),
    ]);
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    dialog
}

fn confirm_remove(ui: &Rc<Ui>, from: &gtk::Button) {
    let dialog = remove_question();
    let weak = Rc::downgrade(ui);
    dialog.connect_response(None, move |_, response| {
        if response == "remove"
            && let Some(ui) = weak.upgrade()
        {
            license::remove(&ui);
        }
    });
    dialog.present(Some(from));
}

/// For screenshots: the Remove License question.
pub fn remove_question_for_screenshot(from: &impl IsA<gtk::Widget>) -> adw::AlertDialog {
    let dialog = remove_question();
    dialog.present(Some(from));
    dialog
}
