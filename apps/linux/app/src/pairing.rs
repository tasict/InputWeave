//! Pairing, both ends: the Add Device dialog on the computer that starts
//! it (nearby computers or an address, then the code shown on the other
//! one), and the window with the code on the computer being added, as the
//! other two apps have them.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::SystemTime;

use adw::prelude::*;
use inputweave_app_core::agent::{AgentError, Answer};
use inputweave_app_core::pairing_text;
use inputweave_app_core::proto::{
    CancelPairing, PairingPhase, StartPairing, SubmitPairingCode, client_message::Command,
    start_pairing,
};
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;
use inputweave_app_core::{device_text, ui};

use crate::pages::set_text;
use crate::strings::{t, tr};
use crate::ui::Ui;

/// The pairing windows while they are open.
#[derive(Default)]
pub struct Pairing {
    add: RefCell<Option<Rc<AddDevice>>>,
    code: RefCell<Option<CodeWindow>>,
    /// The code whose window the user closed, which declines it: not shown
    /// again while the agent still shows it (CancelPairing is on its way),
    /// only a new one.
    declined: RefCell<Option<String>>,
    /// The number of the last pairing started from the app, whichever Add
    /// Device dialog started it: only that one is ever cancelled.
    latest: Cell<u64>,
}

impl Pairing {
    #[cfg(test)]
    pub fn add_dialog(&self) -> Option<Rc<AddDevice>> {
        self.add.borrow().clone()
    }

    #[cfg(test)]
    pub fn code_window(&self) -> Option<adw::Window> {
        self.code.borrow().as_ref().map(|c| c.window.clone())
    }

    /// Lets go of the code window `window`, which the user closed,
    /// declining the code it showed.
    fn forget_code(&self, window: &adw::Window) {
        if let Ok(mut shown) = self.code.try_borrow_mut()
            && shown.as_ref().is_some_and(|c| c.window == *window)
            && let Some(closed) = shown.take()
        {
            *self.declined.borrow_mut() = Some(closed.shown.take());
        }
    }

    /// Numbers a new pairing started from the app.
    fn next_attempt(&self) -> u64 {
        self.latest.set(self.latest.get() + 1);
        self.latest.get()
    }

    /// Closes the Add Device dialog if it is open.
    pub fn close_add(&self) {
        let add = self.add.borrow().clone();
        if let Some(add) = add {
            add.dialog.close();
        }
    }

    pub fn update(&self, ui: &Rc<Ui>, model: &Snapshot) {
        let add = self.add.borrow().clone();
        if let Some(add) = add {
            add.update(model);
        }
        // A code the agent showed before the connection ended is not one to
        // show: nobody could answer for it (the model drops it anyway).
        let shown = model.connected() && pairing_text::shows_code(&model.pairing);
        if !shown {
            self.declined.borrow_mut().take();
        }
        let declined = self.declined.borrow().as_deref() == Some(model.pairing.code.as_str());
        let showing = shown && !declined;
        let mut code = self.code.borrow_mut();
        match (showing, code.as_ref()) {
            (true, Some(window)) => window.update(model),
            (true, None) => {
                let window = CodeWindow::new(ui);
                window.update(model);
                window.window.present();
                *code = Some(window);
            }
            (false, Some(_)) => {
                if let Some(window) = code.take() {
                    window.window.destroy();
                }
            }
            (false, None) => {}
        }
    }
}

pub struct AddDevice {
    dialog: adw::Dialog,
    stack: gtk::Stack,
    nearby: gtk::ListBox,
    nearby_rows: RefCell<Vec<gtk::Widget>>,
    shown_nearby: RefCell<String>,
    none_found: adw::StatusPage,
    address: adw::EntryRow,
    start_error: gtk::Label,
    code_title: gtk::Label,
    code_error: gtk::Label,
    digits: Vec<gtk::Entry>,
    paired: adw::StatusPage,
    failed: adw::StatusPage,
    /// The pairing started in this dialog: until it is the agent's, the
    /// agent's state is an older attempt's.
    attempt: Cell<Attempt>,
    /// The device this dialog's pairing is with, when started from the
    /// list (an address's device is not known until it answers).
    peer: RefCell<Option<String>>,
    /// A code is with the agent; another is not sent meanwhile.
    submitting: Cell<bool>,
    /// The last code could not reach the agent: Enter sends it again.
    submit_failed: Cell<bool>,
    /// The dialog was closed: an attempt it started that the agent takes
    /// later is cancelled then. Shared with that command's answer, which
    /// may come after the dialog is gone.
    closed: Rc<Cell<bool>>,
    /// The tries left when the code was last asked for: fewer next time
    /// means the code typed was wrong.
    attempts_left: Cell<u32>,
    /// The boxes' row, which shakes for a wrong code.
    digit_row: gtk::Box,
    /// Set while the app itself fills the boxes.
    filling: crate::pages::Quiet,
    ui: std::rc::Weak<Ui>,
}

impl AddDevice {
    pub fn present(ui: &Rc<Ui>, parent: &gtk::Widget) {
        let existing = ui.pairing.add.borrow().clone();
        let add = match existing {
            Some(add) => add,
            None => {
                let add = AddDevice::new(ui);
                *ui.pairing.add.borrow_mut() = Some(add.clone());
                add
            }
        };
        add.update(&ui.snapshot());
        add.dialog.present(Some(parent));
    }

    /// The dialog, for screenshots: shown as if pairing had reached the
    /// code.
    pub fn present_entering_code(ui: &Rc<Ui>, parent: &gtk::Widget) {
        AddDevice::present(ui, parent);
        if let Some(add) = ui.pairing.add.borrow().as_ref() {
            add.attempt.set(Attempt::Started(ui.pairing.next_attempt()));
            let mut model = (*ui.snapshot()).clone();
            model.pairing = inputweave_app_core::sample::entering_code();
            add.update(&model);
        }
    }

    pub(crate) fn new(ui: &Rc<Ui>) -> Rc<AddDevice> {
        let nearby = gtk::ListBox::builder()
            .css_classes(["boxed-list"])
            .selection_mode(gtk::SelectionMode::None)
            .build();
        let none_found = adw::StatusPage::builder()
            .icon_name("network-workgroup-symbolic")
            .title(t(keys::PAIRING_NONE_FOUND))
            .description(t(keys::PAIRING_NONE_FOUND_DETAIL))
            .css_classes(["compact"])
            .build();
        let address = adw::EntryRow::builder()
            .use_markup(false)
            .title(t(keys::PAIRING_IP_ADDRESS))
            .show_apply_button(true)
            .input_purpose(gtk::InputPurpose::Url)
            .build();
        let address_group = adw::PreferencesGroup::builder()
            .title(t(keys::PAIRING_ADD_BY_IP))
            .build();
        address_group.add(&address);
        let start_error = gtk::Label::builder()
            .css_classes(["error"])
            .wrap(true)
            .halign(gtk::Align::Start)
            .visible(false)
            .build();
        let nearby_header = gtk::Label::builder()
            .label(t(keys::PAIRING_NEARBY))
            .css_classes(["heading"])
            .halign(gtk::Align::Start)
            .build();
        let list_page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        list_page.append(&nearby_header);
        list_page.append(&nearby);
        list_page.append(&none_found);
        list_page.append(&address_group);
        list_page.append(&start_error);

        let code_title = gtk::Label::builder()
            .css_classes(["title-3"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        let row = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::Center)
            .build();
        let mut digits = Vec::new();
        for i in 0..pairing_text::CODE_LENGTH {
            let entry = gtk::Entry::builder()
                .width_chars(1)
                .max_width_chars(2)
                .xalign(0.5)
                .input_purpose(gtk::InputPurpose::Digits)
                .css_classes(["iw-code-digit"])
                .build();
            entry.update_property(&[gtk::accessible::Property::Label(&tr(&ui!(
                keys::PAIRING_CODE_ENTRY,
                (i + 1).to_string()
            )))]);
            if i == 3 {
                row.append(
                    &gtk::Separator::builder()
                        .css_classes(["spacer"])
                        .margin_start(8)
                        .build(),
                );
            }
            row.append(&entry);
            digits.push(entry);
        }
        let code_error = gtk::Label::builder()
            .css_classes(["error"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .visible(false)
            .build();
        let code_page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .valign(gtk::Align::Center)
            .build();
        code_page.append(&code_title);
        code_page.append(&row);
        code_page.append(&code_error);

        let connecting = adw::StatusPage::builder()
            .title(t(keys::PAIRING_CONNECTING))
            .child(
                &gtk::Spinner::builder()
                    .spinning(true)
                    .width_request(32)
                    .height_request(32)
                    .build(),
            )
            .build();
        let close = gtk::Button::builder()
            .label(t(keys::CLOSE))
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        let paired = adw::StatusPage::builder()
            .icon_name("emblem-ok-symbolic")
            .child(&close)
            .build();
        let again = gtk::Button::builder()
            .label(t(keys::PAIRING_START_OVER))
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .build();
        let failed = adw::StatusPage::builder()
            .icon_name("dialog-warning-symbolic")
            .child(&again)
            .build();

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .vhomogeneous(false)
            .build();
        stack.add_named(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .child(&list_page)
                .build(),
            Some("list"),
        );
        stack.add_named(&code_page, Some("code"));
        stack.add_named(&connecting, Some("connecting"));
        stack.add_named(&paired, Some("paired"));
        stack.add_named(&failed, Some("failed"));
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&stack));
        let dialog = adw::Dialog::builder()
            .title(t(keys::ADD_DEVICE_TITLE))
            .content_width(460)
            .content_height(520)
            .child(&view)
            .build();

        let add = Rc::new(AddDevice {
            dialog,
            stack,
            nearby,
            nearby_rows: RefCell::new(Vec::new()),
            shown_nearby: RefCell::new(String::new()),
            none_found,
            address,
            start_error,
            code_title,
            code_error,
            digits,
            paired,
            failed,
            attempt: Cell::new(Attempt::None),
            peer: RefCell::new(None),
            submitting: Cell::new(false),
            submit_failed: Cell::new(false),
            closed: Rc::new(Cell::new(false)),
            attempts_left: Cell::new(u32::MAX),
            digit_row: row,
            filling: crate::pages::Quiet::default(),
            ui: Rc::downgrade(ui),
        });
        add.connect(&close, &again);
        add
    }

    fn connect(self: &Rc<Self>, close: &gtk::Button, again: &gtk::Button) {
        let weak = Rc::downgrade(self);
        self.address.connect_apply(move |entry| {
            if let Some(add) = weak.upgrade() {
                let address = entry.text().trim().to_owned();
                if !address.is_empty() {
                    add.start(start_pairing::Target::Address(address));
                }
            }
        });
        // Weakly: the dialog holds the button.
        let dialog = self.dialog.downgrade();
        close.connect_clicked(move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        });
        let weak = Rc::downgrade(self);
        again.connect_clicked(move |_| {
            if let Some(add) = weak.upgrade() {
                add.attempt.set(Attempt::None);
                add.submit_failed.set(false);
                add.clear_code();
                if let Some(ui) = add.ui.upgrade() {
                    add.update(&ui.snapshot());
                }
            }
        });
        for (i, entry) in self.digits.iter().enumerate() {
            let weak = Rc::downgrade(self);
            entry.connect_changed(move |entry| {
                let Some(add) = weak.upgrade() else { return };
                if add.filling.is_set() {
                    return;
                }
                add.submit_failed.set(false);
                // A digit, a pasted code, or anything else: spread over the
                // boxes (inputweave_app_core::pairing_text::spread).
                let (boxes, focus) = pairing_text::spread(&add.boxes(), i, &entry.text());
                add.filling.run(|| {
                    for (entry, text) in add.digits.iter().zip(&boxes) {
                        if entry.text() != *text {
                            entry.set_text(text);
                        }
                    }
                });
                if !boxes[i].is_empty() {
                    add.digits[focus].grab_focus();
                    add.digits[focus].set_position(-1);
                }
                add.submit_when_full();
            });
            // Enter sends the code again, as after it could not be sent.
            let weak = Rc::downgrade(self);
            entry.connect_activate(move |_| {
                if let Some(add) = weak.upgrade() {
                    add.submit_when_full();
                }
            });
            let weak = Rc::downgrade(self);
            let keys = gtk::EventControllerKey::new();
            keys.connect_key_pressed(move |_, key, _, _| {
                let Some(add) = weak.upgrade() else {
                    return gtk::glib::Propagation::Proceed;
                };
                if key == gtk::gdk::Key::BackSpace && add.digits[i].text().is_empty() && i > 0 {
                    add.filling.run(|| add.digits[i - 1].set_text(""));
                    add.digits[i - 1].grab_focus();
                    return gtk::glib::Propagation::Stop;
                }
                gtk::glib::Propagation::Proceed
            });
            entry.add_controller(keys);
        }
        let weak = Rc::downgrade(self);
        self.dialog.connect_closed(move |_| {
            let Some(add) = weak.upgrade() else { return };
            let Some(ui) = add.ui.upgrade() else { return };
            add.closed.set(true);
            // Only what this dialog started, while the agent is still on it:
            // one still starting is cancelled when the agent takes it
            // (`start`), nothing if it refuses or the pairing is another.
            let model = ui.snapshot();
            let ours = ThisAttempt {
                attempt: add.attempt.get(),
                latest: ui.pairing.latest.get(),
                same_peer: same_peer(add.peer.borrow().as_deref(), &model.pairing.peer_device_id),
            };
            if cancel_on_close(ours, model.pairing.phase()) {
                ui.command(Command::CancelPairing(CancelPairing {}));
            }
            ui.pairing.add.borrow_mut().take();
        });
    }

    #[cfg(test)]
    pub fn digit_entries(&self) -> &[gtk::Entry] {
        &self.digits
    }

    #[cfg(test)]
    pub fn begin_for_tests(&self, ui: &Rc<Ui>) {
        self.attempt
            .set(Attempt::Started(ui.pairing.next_attempt()));
    }

    #[cfg(test)]
    pub fn start_for_tests(self: &Rc<Self>) {
        self.start(start_pairing::Target::Address("192.0.2.1".into()));
    }

    #[cfg(test)]
    pub fn error_shown(&self) -> Option<String> {
        self.code_error
            .is_visible()
            .then(|| self.code_error.text().to_string())
    }

    #[cfg(test)]
    pub fn sending(&self) -> bool {
        self.submitting.get()
    }

    #[cfg(test)]
    pub fn page_shown(&self) -> Option<String> {
        self.stack.visible_child_name().map(|n| n.to_string())
    }

    #[cfg(test)]
    pub fn show_for_tests(self: &Rc<Self>, model: &Snapshot) {
        self.update(model);
    }

    fn boxes(&self) -> [String; pairing_text::CODE_LENGTH] {
        std::array::from_fn(|i| self.digits[i].text().to_string())
    }

    fn clear_code(&self) {
        self.filling.run(|| {
            for entry in &self.digits {
                entry.set_text("");
            }
        });
    }

    /// Clears the boxes for another try, as for a wrong code: back to the
    /// first, with a shake (which GTK leaves out with animations off).
    fn try_again(&self) {
        self.submit_failed.set(false);
        self.clear_code();
        for entry in &self.digits {
            entry.set_sensitive(true);
        }
        self.digits[0].grab_focus();
        self.digit_row.remove_css_class("iw-shake");
        self.digit_row.add_css_class("iw-shake");
        let row = self.digit_row.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
            row.remove_css_class("iw-shake");
        });
    }

    fn submit_when_full(self: &Rc<Self>) {
        let code = pairing_text::clean(&self.boxes().concat());
        if code.len() != pairing_text::CODE_LENGTH || self.submitting.get() {
            return;
        }
        let Some(ui) = self.ui.upgrade() else { return };
        self.submitting.set(true);
        self.submit_failed.set(false);
        let weak = Rc::downgrade(self);
        ui.command_then(
            Command::SubmitPairingCode(SubmitPairingCode { code }),
            move |result| {
                let Some(add) = weak.upgrade() else {
                    return result.is_ok();
                };
                add.submitting.set(false);
                let Some(ui) = add.ui.upgrade() else {
                    return result.is_ok();
                };
                // A wrong code comes back as the code asked for again, with
                // fewer tries left: `update` sees that in the model, which
                // has the answer by now (`after_submit` for the rest).
                let model = ui.snapshot();
                let now = Submitted {
                    connected: model.connected(),
                    phase: model.pairing.phase(),
                    // Tries are counted down only for codes that arrived.
                    same_tries: model.pairing.attempts_left == add.attempts_left.get(),
                };
                match after_submit(result, now) {
                    AfterSubmit::TryAgain => add.try_again(),
                    AfterSubmit::SendAgain => add.submit_failed.set(true),
                    AfterSubmit::Nothing => {}
                }
                add.update(&ui.snapshot());
                true
            },
        );
    }

    fn start(self: &Rc<Self>, target: start_pairing::Target) {
        let Some(ui) = self.ui.upgrade() else { return };
        // Until the agent answers, its pairing state is still the last
        // attempt's (Paired, Failed…): Connecting is shown meanwhile.
        let number = ui.pairing.next_attempt();
        self.attempt.set(Attempt::Starting(number));
        let peer = match &target {
            start_pairing::Target::DeviceId(id) => Some(id.clone()),
            start_pairing::Target::Address(_) => None,
        };
        *self.peer.borrow_mut() = peer.clone();
        self.submit_failed.set(false);
        self.attempts_left.set(u32::MAX);
        self.clear_code();
        self.start_error.set_visible(false);
        self.update(&ui.snapshot());
        let weak = Rc::downgrade(self);
        let (closed, ui_weak) = (self.closed.clone(), self.ui.clone());
        // No deadline of the app's (agent::timeout_for): only the agent's
        // answer, or the connection ending, says whether it started.
        ui.command_then(
            Command::StartPairing(StartPairing {
                target: Some(target),
            }),
            move |result| {
                if closed.get() {
                    // Closed meanwhile: cancel it now that it started, if it
                    // is still the one the agent is on.
                    if let Some(ui) = ui_weak.upgrade() {
                        let model = ui.snapshot();
                        let ours = ThisAttempt {
                            attempt: Attempt::Started(number),
                            latest: ui.pairing.latest.get(),
                            same_peer: same_peer(peer.as_deref(), &model.pairing.peer_device_id),
                        };
                        if result.is_ok() && cancel_on_close(ours, model.pairing.phase()) {
                            ui.command(Command::CancelPairing(CancelPairing {}));
                        }
                    }
                    return true;
                }
                let Some(add) = weak.upgrade() else {
                    return result.is_ok();
                };
                match result {
                    Ok(_) => add.attempt.set(Attempt::Started(number)),
                    Err(e) => {
                        add.attempt.set(Attempt::None);
                        if let Some(code) = e.code() {
                            add.start_error
                                .set_text(&tr(&pairing_text::start_failure(code)));
                            add.start_error.set_visible(true);
                        }
                    }
                }
                if let Some(ui) = add.ui.upgrade() {
                    add.update(&ui.snapshot());
                }
                // A refusal with a reason is shown under the list.
                result.is_ok() || result.as_ref().err().and_then(AgentError::code).is_some()
            },
        );
    }

    fn update(self: &Rc<Self>, model: &Snapshot) {
        let pairing = &model.pairing;
        // Over: the outcome stays this attempt's, whatever the agent does
        // next, and nothing of it is cancelled any more.
        if let Attempt::Started(_) = self.attempt.get()
            && let Some(outcome) = ended(pairing.phase())
        {
            self.attempt.set(Attempt::Ended(outcome));
        }
        let page = page_for(self.attempt.get(), pairing.phase());
        if self.stack.visible_child_name().as_deref() != Some(page) {
            self.stack.set_visible_child_name(page);
            if page == "code" {
                self.digits[0].grab_focus();
            }
        }
        let name = if pairing.peer_name.is_empty() {
            t(keys::PAIRING_THE_OTHER_COMPUTER)
        } else {
            pairing.peer_name.clone()
        };
        set_text(
            &self.code_title,
            &tr(&ui!(keys::PAIRING_ENTER_CODE, name.clone())),
        );
        // Before the boxes may take the focus again below.
        let sensitive = pairing.phase() != PairingPhase::Verifying;
        for entry in &self.digits {
            entry.set_sensitive(sensitive);
        }
        let wrong = pairing.error == "wrong_code";
        // Not while a code is with the agent: its answer settles it.
        if matches!(self.attempt.get(), Attempt::Started(_))
            && pairing.phase() == PairingPhase::EnterCode
            && !self.submitting.get()
        {
            if wrong && pairing.attempts_left < self.attempts_left.get() {
                self.try_again();
            }
            self.attempts_left.set(pairing.attempts_left);
        }
        if wrong {
            set_text(
                &self.code_error,
                &tr(&ui!(
                    keys::PAIRING_WRONG_CODE,
                    pairing.attempts_left.to_string()
                )),
            );
        } else if self.submit_failed.get() {
            set_text(&self.code_error, &t(keys::PAIRING_SEND_AGAIN));
        }
        self.code_error
            .set_visible(wrong || self.submit_failed.get());
        // As they ended: kept while the agent goes on to other things.
        if pairing.phase() == PairingPhase::Paired {
            let paired = tr(&ui!(keys::PAIRING_PAIRED, name));
            if self.paired.title() != paired {
                self.paired.set_title(&paired);
            }
        }
        if pairing.phase() == PairingPhase::Failed {
            let failure = tr(&pairing_text::failure(pairing));
            if self.failed.title() != failure {
                self.failed.set_title(&failure);
            }
        }
        self.update_nearby(model);
    }

    fn update_nearby(self: &Rc<Self>, model: &Snapshot) {
        let devices: Vec<_> = model.nearby.iter().filter(|d| !d.paired).collect();
        let key = format!("{devices:?}");
        if *self.shown_nearby.borrow() == key {
            return;
        }
        *self.shown_nearby.borrow_mut() = key;
        let mut rows = self.nearby_rows.borrow_mut();
        for row in rows.drain(..) {
            self.nearby.remove(&row);
        }
        for device in &devices {
            let subtitle = if device.pairable {
                tr(&device_text::os(&device.os))
            } else {
                format!(
                    "{} · {}",
                    tr(&device_text::os(&device.os)),
                    t(keys::PAIRING_NOT_ACCEPTING)
                )
            };
            // Set once markup is off: names may hold & or <.
            let row = crate::pages::plain_row();
            row.set_title(&device.name);
            row.set_subtitle(&subtitle);
            row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
            let pair = crate::pages::row_button(&t(keys::PAIRING_PAIR));
            pair.set_sensitive(device.pairable);
            let (weak, id) = (Rc::downgrade(self), device.id.clone());
            pair.connect_clicked(move |_| {
                if let Some(add) = weak.upgrade() {
                    add.start(start_pairing::Target::DeviceId(id.clone()));
                }
            });
            row.add_suffix(&pair);
            self.nearby.append(&row);
            rows.push(row.upcast());
        }
        self.nearby.set_visible(!devices.is_empty());
        self.none_found.set_visible(devices.is_empty());
    }
}

/// The code this computer shows for the other one to type, in a window of
/// its own: whoever sees the screen can finish pairing without a keyboard
/// or mouse here.
pub struct CodeWindow {
    pub window: adw::Window,
    /// The code it shows.
    shown: RefCell<String>,
    title: gtk::Label,
    code: gtk::Label,
    valid: gtk::Label,
}

impl CodeWindow {
    pub fn new(ui: &Rc<Ui>) -> CodeWindow {
        let title = gtk::Label::builder()
            .css_classes(["title-4"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        let code = gtk::Label::builder()
            .css_classes(["iw-code"])
            .selectable(false)
            .build();
        let valid = gtk::Label::builder()
            .css_classes(["dim-label", "iw-numeric"])
            .build();
        let decline = gtk::Button::builder()
            .label(t(keys::PAIRING_DECLINE))
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .build();
        let weak = Rc::downgrade(ui);
        decline.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.command(Command::CancelPairing(CancelPairing {}));
            }
        });
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        content.append(&title);
        content.append(&code);
        content.append(&valid);
        content.append(&decline);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::builder().show_title(false).build());
        view.set_content(Some(&content));
        let window = adw::Window::builder()
            .title(t(keys::PAIRING_CODE_NAME))
            .content(&view)
            .default_width(436)
            .resizable(false)
            .build();
        window.set_application(Some(&ui.app));
        let weak = Rc::downgrade(ui);
        window.connect_close_request(move |window| {
            let Some(ui) = weak.upgrade() else {
                return gtk::glib::Propagation::Proceed;
            };
            // Closing it declines, as there is no other way back to it; a
            // window comes again only for another code.
            ui.pairing.forget_code(window);
            if pairing_text::shows_code(&ui.snapshot().pairing) {
                ui.command(Command::CancelPairing(CancelPairing {}));
            }
            gtk::glib::Propagation::Proceed
        });
        CodeWindow {
            window,
            shown: RefCell::new(String::new()),
            title,
            code,
            valid,
        }
    }

    pub fn update(&self, model: &Snapshot) {
        let pairing = &model.pairing;
        let name = if pairing.peer_name.is_empty() {
            t(keys::PAIRING_THE_OTHER_COMPUTER)
        } else {
            pairing.peer_name.clone()
        };
        set_text(&self.title, &tr(&ui!(keys::PAIRING_CODE_TITLE, name)));
        set_text(&self.code, &pairing_text::grouped(&pairing.code));
        *self.shown.borrow_mut() = pairing.code.clone();
        self.code
            .update_property(&[gtk::accessible::Property::Label(&pairing_text::spoken(
                &pairing.code,
            ))]);
        let countdown = pairing_text::countdown(pairing.expires_at_unix_ms, SystemTime::now());
        set_text(&self.valid, &tr(&ui!(keys::PAIRING_CODE_VALID, countdown)));
    }
}

/// Where pairing started in the Add Device dialog is. Pairings started
/// from the app are numbered (`Pairing::next_attempt`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Attempt {
    /// None started here (or the user started over).
    None,
    /// StartPairing is with the agent, whose state is still the last
    /// attempt's.
    Starting(u64),
    /// The agent took it: its state is this attempt's.
    Started(u64),
    /// It ended so: the dialog shows that until started over.
    Ended(Outcome),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Paired,
    Failed,
    /// Cancelled, or gone with the connection.
    Gone,
}

/// How a pairing the agent was on ended, if it did.
fn ended(phase: PairingPhase) -> Option<Outcome> {
    match phase {
        PairingPhase::Connecting | PairingPhase::EnterCode | PairingPhase::Verifying => None,
        PairingPhase::Paired => Some(Outcome::Paired),
        PairingPhase::Failed => Some(Outcome::Failed),
        PairingPhase::Unspecified | PairingPhase::Idle | PairingPhase::ShowCode => {
            Some(Outcome::Gone)
        }
    }
}

/// What tells this dialog's pairing from another the agent may be on.
#[derive(Clone, Copy, Debug)]
struct ThisAttempt {
    attempt: Attempt,
    /// The last pairing started from the app.
    latest: u64,
    /// The agent's pairing is with the device this one started with, as
    /// far as either is known.
    same_peer: bool,
}

fn same_peer(ours: Option<&str>, agents: &str) -> bool {
    ours.is_none_or(|ours| agents.is_empty() || agents == ours)
}

/// Whether to cancel the agent's pairing as Add Device closes, or as the
/// answer to StartPairing comes after it closed: only this dialog's, the
/// agent took it, no later one was started from the app, it is with the
/// same device and still going. Never one still starting (its answer
/// decides), one the agent refused, or one that ended.
fn cancel_on_close(this: ThisAttempt, phase: PairingPhase) -> bool {
    matches!(this.attempt, Attempt::Started(n) if n == this.latest)
        && this.same_peer
        && ended(phase).is_none()
}

/// What follows SubmitPairingCode's answer, besides the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AfterSubmit {
    /// Clear the boxes, shake, focus the first.
    TryAgain,
    /// The code never reached the agent: keep it, Enter sends it again.
    SendAgain,
    Nothing,
}

/// Where things stand as SubmitPairingCode's answer comes.
#[derive(Clone, Copy, Debug)]
struct Submitted {
    connected: bool,
    phase: PairingPhase,
    /// The tries left are as before the code was sent.
    same_tries: bool,
}

/// Refused while the code is still asked for, the boxes clear; refused as
/// pairing ended, the failure page says why, without a shake. Not
/// answered: only when the agent still asks for the code with the same
/// tries left did the code not reach it, and Enter sends it again; checking
/// it, or past that, the model goes on, and with the connection gone the
/// pairing is too. A wrong code comes back as an answer, which the model
/// shows (`AddDevice::update`).
fn after_submit(result: &Result<Answer, AgentError>, now: Submitted) -> AfterSubmit {
    let entering = now.phase == PairingPhase::EnterCode;
    match result {
        Err(AgentError::Refused { .. }) if entering => AfterSubmit::TryAgain,
        Err(AgentError::Unavailable(_)) if now.connected && entering && now.same_tries => {
            AfterSubmit::SendAgain
        }
        _ => AfterSubmit::Nothing,
    }
}

/// The dialog's page for an attempt and the agent's phase.
fn page_for(attempt: Attempt, phase: PairingPhase) -> &'static str {
    match attempt {
        Attempt::None | Attempt::Ended(Outcome::Gone) => "list",
        Attempt::Ended(Outcome::Paired) => "paired",
        Attempt::Ended(Outcome::Failed) => "failed",
        Attempt::Starting(_) => "connecting",
        Attempt::Started(_) => match phase {
            PairingPhase::Connecting => "connecting",
            PairingPhase::EnterCode | PairingPhase::Verifying => "code",
            PairingPhase::Paired => "paired",
            PairingPhase::Failed => "failed",
            _ => "list",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_attempt_hides_the_last_ones_outcome_until_the_agent_takes_it() {
        // The agent still reports how the last attempt ended.
        assert_eq!(
            page_for(Attempt::Starting(2), PairingPhase::Failed),
            "connecting"
        );
        assert_eq!(
            page_for(Attempt::Starting(2), PairingPhase::Paired),
            "connecting"
        );
        assert_eq!(
            page_for(Attempt::Started(2), PairingPhase::Connecting),
            "connecting"
        );
        assert_eq!(
            page_for(Attempt::Started(2), PairingPhase::EnterCode),
            "code"
        );
        assert_eq!(
            page_for(Attempt::Started(2), PairingPhase::Verifying),
            "code"
        );
        assert_eq!(
            page_for(Attempt::Started(2), PairingPhase::Failed),
            "failed"
        );
        assert_eq!(page_for(Attempt::None, PairingPhase::Failed), "list");
        // Once over, the outcome stays whatever the agent does next.
        assert_eq!(
            page_for(Attempt::Ended(Outcome::Failed), PairingPhase::ShowCode),
            "failed"
        );
        assert_eq!(
            page_for(Attempt::Ended(Outcome::Paired), PairingPhase::Idle),
            "paired"
        );
        assert_eq!(ended(PairingPhase::EnterCode), None);
        assert_eq!(ended(PairingPhase::Idle), Some(Outcome::Gone));
    }

    fn this(attempt: Attempt, latest: u64, same_peer: bool) -> ThisAttempt {
        ThisAttempt {
            attempt,
            latest,
            same_peer,
        }
    }

    #[test]
    fn only_this_dialogs_pairing_is_cancelled() {
        let going = PairingPhase::EnterCode;
        assert!(cancel_on_close(this(Attempt::Started(3), 3, true), going));
        // Still starting: its answer decides.
        assert!(!cancel_on_close(this(Attempt::Starting(3), 3, true), going));
        // A later pairing was started from the app (another dialog): the
        // agent is on that one now.
        assert!(!cancel_on_close(this(Attempt::Started(2), 3, true), going));
        // Ended: what the agent is on now is not this one.
        assert!(!cancel_on_close(
            this(Attempt::Ended(Outcome::Failed), 3, true),
            going
        ));
        assert!(!cancel_on_close(
            this(Attempt::Started(3), 3, true),
            PairingPhase::Paired
        ));
        // With another device.
        assert!(!cancel_on_close(this(Attempt::Started(3), 3, false), going));
        assert!(!cancel_on_close(this(Attempt::None, 3, true), going));
        assert!(same_peer(Some("mac"), "mac"));
        assert!(same_peer(Some("mac"), ""), "not introduced yet");
        assert!(!same_peer(Some("mac"), "pc"));
        assert!(same_peer(None, "pc"), "started by address");
    }

    #[test]
    fn what_a_submitted_code_leads_to() {
        let at = |connected, phase, same_tries| Submitted {
            connected,
            phase,
            same_tries,
        };
        let entering = at(true, PairingPhase::EnterCode, true);
        let refused = Err(AgentError::Refused {
            code: "not_waiting".into(),
            detail: String::new(),
        });
        assert_eq!(after_submit(&refused, entering), AfterSubmit::TryAgain);
        assert_eq!(
            after_submit(&refused, at(true, PairingPhase::Failed, true)),
            AfterSubmit::Nothing,
            "pairing ended: the failure page, no shake"
        );
        let unanswered = Err(AgentError::Unavailable("no answer".into()));
        assert_eq!(after_submit(&unanswered, entering), AfterSubmit::SendAgain);
        // The agent got it: checking it, or the tries counted down.
        assert_eq!(
            after_submit(&unanswered, at(true, PairingPhase::Verifying, true)),
            AfterSubmit::Nothing
        );
        assert_eq!(
            after_submit(&unanswered, at(true, PairingPhase::EnterCode, false)),
            AfterSubmit::Nothing
        );
        // Disconnected: the pairing went with the connection.
        assert_eq!(
            after_submit(&unanswered, at(false, PairingPhase::Unspecified, true)),
            AfterSubmit::Nothing
        );
        assert_eq!(
            after_submit(&Ok(Answer::Applied), entering),
            AfterSubmit::Nothing
        );
    }
}
