//! The inspector beside the device lists (Devices, Arrangement): one
//! computer's name, system, status (and waking it), screens, how fast the
//! cursor moves on it, what it shares with this one, the key swap, taking
//! it offline for now or bringing it back, and removing it.
//!
//! Its rows are made again only when what they are changes (another
//! device, its screens, whether it is paired); otherwise every model
//! updates them in place, so focus, a name being typed and the scroll
//! position stay, and a switch whose command failed shows the model again.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use adw::prelude::*;
use gtk::glib;
use inputweave_app_core::cursor_speed::{self, Draft};
use inputweave_app_core::license_text::{self, NotRevoked, RemoveOffer};
use inputweave_app_core::polar;
use inputweave_app_core::proto::{
    RenameDevice, SetDeviceOptions, SetDeviceSharing, SetDeviceSpeed, client_message::Command,
};
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;
use inputweave_app_core::wake::{self, Glyph, Note};
use inputweave_app_core::{device_text, log, offline, ui};

use super::{EntrySync, Quiet, row_button, set_subtitle, set_switch, set_text, value_label};
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
    /// Another computer's.
    wake: Option<WakeRow>,
    /// Another computer's.
    offline: Option<OfflineRow>,
    speed: Option<SpeedRow>,
    clipboard: Option<adw::SwitchRow>,
    files: Option<adw::SwitchRow>,
    swap: Option<adw::SwitchRow>,
}

/// What the status row of another computer shows of waking it: the moon
/// while it can be woken, a spinner in its place while it is being woken,
/// Wake, and under the status why it did not wake up (with how to turn
/// waking on) or that it never said how.
#[derive(Clone)]
pub struct WakeRow {
    pub moon: gtk::Image,
    pub spinner: gtk::Spinner,
    pub button: gtk::Button,
    pub not_woken: gtk::Box,
    pub not_woken_text: gtk::Label,
    pub not_told: gtk::Label,
}

impl WakeRow {
    fn show(&self, model: &Snapshot, id: &str, in_flight: bool) {
        let glyph = wake::glyph(model, id);
        self.moon.set_visible(glyph == Some(Glyph::Moon));
        let spinning = glyph == Some(Glyph::Spinner);
        self.spinner.set_visible(spinning);
        if self.spinner.is_spinning() != spinning {
            self.spinner.set_spinning(spinning);
        }
        let offer = wake::offer(model, id, in_flight);
        self.button.set_visible(offer.is_some());
        self.button.set_sensitive(offer == Some(true));
        let (not_woken, not_told) = match wake::note(model, id) {
            Some(Note::NotWoken(text)) => (Some(text), None),
            Some(Note::NotToldYet(text)) => (None, Some(text)),
            None => (None, None),
        };
        if let Some(text) = &not_woken {
            let markup = format!(
                "{}\n<a href=\"{}\">{}</a>",
                glib::markup_escape_text(&tr(text)),
                glib::markup_escape_text(&t(keys::WAKE_HOW_TO_URL)),
                glib::markup_escape_text(&t(keys::WAKE_HOW_TO)),
            );
            if self.not_woken_text.label() != markup {
                self.not_woken_text.set_markup(&markup);
            }
        }
        self.not_woken.set_visible(not_woken.is_some());
        if let Some(text) = &not_told {
            set_text(&self.not_told, &tr(text));
        }
        self.not_told.set_visible(not_told.is_some());
    }
}

/// What the inspector of another computer shows of taking it offline for
/// now: the icon at the end of its status while it is, under the status
/// who set it and, when it runs a version from before it, a warning for
/// each other connected computer on such a version (which still controls
/// it), and, in a group of
/// its own after the switches, Take Offline or Bring Back Online with what
/// it does.
#[derive(Clone)]
pub struct OfflineRow {
    pub icon: gtk::Image,
    pub set_by: gtk::Label,
    /// The group of the status, which the warnings go below.
    about: adw::PreferencesGroup,
    pub group: adw::PreferencesGroup,
    pub button: gtk::Button,
    /// The warnings as shown: their texts, and their lines in `about`.
    shown: Rc<RefCell<Vec<String>>>,
    lines: Rc<RefCell<Vec<gtk::Box>>>,
}

impl OfflineRow {
    fn show(&self, model: &Snapshot, id: &str) {
        self.icon.set_visible(offline::is_offline(model, id));
        let set_by = offline::set_by_line(model, id);
        if let Some(text) = &set_by {
            set_text(&self.set_by, &tr(text));
        }
        self.set_by.set_visible(set_by.is_some());
        let warnings: Vec<String> = offline::unaware_warnings(model, id)
            .iter()
            .map(tr)
            .collect();
        // Each a line of the status's group, as the lines about waking it:
        // added after those, but never shown with them (a computer offline
        // for now is not offered waking), so right under the status.
        if *self.shown.borrow() != warnings {
            for line in self.lines.borrow_mut().drain(..) {
                self.about.remove(&line);
            }
            for text in &warnings {
                let line = warning_line(text);
                self.about.add(&line);
                self.lines.borrow_mut().push(line);
            }
            *self.shown.borrow_mut() = warnings;
        }
        let offer = offline::offer(model, id);
        self.group.set_visible(offer.is_some());
        if let Some(offline) = offer {
            let label = t(offline::offer_key(offline));
            if self.button.label().as_deref() != Some(label.as_str()) {
                self.button.set_label(&label);
            }
        }
    }
}

/// A line under the status with the warning icon before it.
fn warning_line(text: &str) -> gtk::Box {
    let icon = gtk::Image::builder()
        .icon_name("dialog-warning-symbolic")
        .accessible_role(gtk::AccessibleRole::Presentation)
        .css_classes(["warning"])
        .valign(gtk::Align::Start)
        .build();
    let label = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .xalign(0.0)
        .hexpand(true)
        .build();
    let line = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(6)
        .margin_start(12)
        .margin_end(12)
        .build();
    line.append(&icon);
    line.append(&label);
    line
}

/// How long the keys must leave the slider alone before the speed it shows
/// is sent.
const SPEED_SETTLE: Duration = Duration::from_millis(300);

/// The cursor speed row of another computer: a slider from 50% to 300% in
/// steps of 10, the speed and Reset beside its title, and what it does
/// under it. The speed under the hand is shown until the agent answers for
/// it; then the agent's again, so a refused one goes back. A speed is sent
/// as the pointer lets go of the slider, or once the keys have left it
/// alone for a moment; never while it is held.
struct SpeedRow {
    scale: gtk::Scale,
    value: gtk::Label,
    reset: gtk::Button,
    description: gtk::Label,
    state: SpeedState,
}

/// What the speed row's handlers share with it.
#[derive(Clone)]
struct SpeedState {
    ui: std::rc::Weak<Ui>,
    /// The computer the row is for.
    device_id: Rc<str>,
    quiet: Quiet,
    draft: Rc<Cell<Draft>>,
    /// The agent's speed for it.
    agent: Rc<Cell<u32>>,
    /// Sends a speed chosen with the keys once they have stopped.
    pending: Rc<RefCell<Option<glib::SourceId>>>,
}

impl SpeedState {
    fn change<R>(&self, f: impl FnOnce(&mut Draft, u32) -> R) -> R {
        let mut draft = self.draft.get();
        let result = f(&mut draft, self.agent.get());
        self.draft.set(draft);
        result
    }

    /// Stops a speed chosen with the keys from going; whether one was
    /// waiting.
    fn cancel_pending(&self) -> bool {
        match self.pending.borrow_mut().take() {
            Some(source) => {
                source.remove();
                true
            }
            None => false,
        }
    }

    /// Sends `percent` now; once the agent answers, either way, the row
    /// shows the agent's speed again (the refresh after every command).
    fn send(&self, percent: u32) {
        let Some(ui) = self.ui.upgrade() else { return };
        let number = self.change(|draft, _| draft.sending(percent));
        let draft = self.draft.clone();
        ui.command_then(
            Command::SetDeviceSpeed(SetDeviceSpeed {
                device_id: self.device_id.to_string(),
                percent,
            }),
            move |result| {
                let mut now = draft.get();
                now.answered(number);
                draft.set(now);
                result.is_ok()
            },
        );
    }

    /// The row goes away for good (another computer is shown): what was
    /// chosen for this one and not sent yet, waiting or still held, goes
    /// now.
    fn flush(&self) {
        let waiting = self.cancel_pending();
        if (waiting || self.draft.get().editing())
            && let Some(percent) = self.change(|draft, agent| draft.ended(agent))
        {
            self.send(percent);
        }
    }

    /// The slider is made again while held: the release may go to the old
    /// one, so the drag ends here, and the speed it was held at goes.
    fn end_drag(&self) {
        if self.draft.get().editing()
            && let Some(percent) = self.change(|draft, agent| draft.ended(agent))
        {
            self.send(percent);
        }
    }

    /// Sends `percent` once the slider has been left alone for a moment.
    fn send_later(&self, percent: u32) {
        self.cancel_pending();
        let state = self.clone();
        let source = glib::timeout_add_local_once(SPEED_SETTLE, move || {
            state.pending.borrow_mut().take();
            state.send(percent);
        });
        *self.pending.borrow_mut() = Some(source);
    }
}

/// The speed row, for its own handlers: weakly, as its widgets hold them.
#[derive(Clone)]
struct WeakSpeedRow {
    scale: glib::WeakRef<gtk::Scale>,
    value: glib::WeakRef<gtk::Label>,
    reset: glib::WeakRef<gtk::Button>,
    description: glib::WeakRef<gtk::Label>,
    state: SpeedState,
}

impl WeakSpeedRow {
    fn upgrade(&self) -> Option<SpeedRow> {
        Some(SpeedRow {
            scale: self.scale.upgrade()?,
            value: self.value.upgrade()?,
            reset: self.reset.upgrade()?,
            description: self.description.upgrade()?,
            state: self.state.clone(),
        })
    }
}

impl SpeedRow {
    fn downgrade(&self) -> WeakSpeedRow {
        WeakSpeedRow {
            scale: self.scale.downgrade(),
            value: self.value.downgrade(),
            reset: self.reset.downgrade(),
            description: self.description.downgrade(),
            state: self.state.clone(),
        }
    }

    /// Shows the speed under the hand, or else the agent's.
    fn show(&self) {
        let percent = self.state.draft.get().shown(self.state.agent.get());
        let label = cursor_speed::label(percent);
        set_text(&self.value, &label);
        self.reset.set_visible(cursor_speed::shows_reset(percent));
        if cursor_speed::snapped(self.scale.value()) != percent {
            self.state
                .quiet
                .run(|| self.scale.set_value(f64::from(percent)));
        }
        self.scale
            .update_property(&[gtk::accessible::Property::ValueText(&label)]);
    }

    /// The pointer pressed the slider: nothing goes until it lets go.
    fn pressed(&self) {
        self.state.cancel_pending();
        self.state.change(|draft, _| draft.began());
    }

    /// The pointer let go of the slider: the speed it was let go at goes.
    fn released(&self) {
        if !self.state.draft.get().editing() {
            return;
        }
        let send = self.state.change(|draft, agent| draft.ended(agent));
        self.show();
        if let Some(percent) = send {
            self.state.send(percent);
        }
    }

    /// The slider moved to `position`: shown at once; moved by the keys, it
    /// is sent once they have stopped.
    fn moved(&self, position: f64) {
        let pending = self.state.change(|draft, agent| {
            draft.moved(position, agent);
            draft.pending(agent)
        });
        self.show();
        match pending {
            Some(percent) => self.state.send_later(percent),
            None => {
                self.state.cancel_pending();
            }
        }
    }

    /// Reset: back to the normal speed, sent at once.
    fn reset(&self) {
        self.state.cancel_pending();
        let pending = self.state.change(|draft, agent| {
            draft.moved(f64::from(cursor_speed::NORMAL), agent);
            draft.pending(agent)
        });
        self.show();
        if let Some(percent) = pending {
            self.state.send(percent);
        }
    }
}

pub struct Inspector {
    page: adw::PreferencesPage,
    groups: RefCell<Vec<adw::PreferencesGroup>>,
    /// What the rows were made for (`structure`).
    built: RefCell<String>,
    rows: RefCell<Option<Rows>>,
    /// Each computer's cursor speed state, kept while another is shown, so
    /// coming back to one shows the speed still on its way there.
    speeds: RefCell<std::collections::HashMap<String, SpeedState>>,
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
            speeds: RefCell::default(),
            quiet: Quiet::default(),
            ui: Rc::downgrade(ui),
        })
    }

    pub fn widget(&self) -> &adw::PreferencesPage {
        &self.page
    }

    /// Shows `id`, or nothing.
    pub fn update(self: &Rc<Self>, model: &Snapshot, id: Option<&str>) {
        // A computer no longer trusted (unpaired) takes its speed state
        // along: paired again, it shows the agent's speed.
        self.speeds.borrow_mut().retain(|device, state| {
            let kept = model.device(device).is_some();
            if !kept {
                state.cancel_pending();
            }
            kept
        });
        let key = id.map(|id| structure(model, id)).unwrap_or_default();
        if *self.built.borrow() != key {
            *self.built.borrow_mut() = key;
            for group in self.groups.borrow_mut().drain(..) {
                self.page.remove(&group);
            }
            // A speed row made again for the same computer carries on with
            // its state (`speed_state`); leaving a computer, what was chosen
            // for it and not sent yet goes now.
            let old = self.rows.borrow_mut().take();
            if let Some(speed) = old.and_then(|rows| rows.speed) {
                if id == Some(&*speed.state.device_id) {
                    speed.state.end_drag();
                } else {
                    speed.state.flush();
                }
            }
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
        set_subtitle(
            &rows.status,
            &tr(&device_text::status(model, id, SystemTime::now())),
        );
        if let Some(row) = &rows.offline {
            row.show(model, id);
        }
        if let Some(row) = &rows.wake {
            let in_flight = self.ui.upgrade().is_some_and(|ui| ui.waking_asked(id));
            row.show(model, id, in_flight);
        }
        if let (Some(speed), Some(agent)) = (&rows.speed, cursor_speed::shown(model, id)) {
            speed.state.agent.set(agent);
            let mut draft = speed.state.draft.get();
            draft.settle(agent);
            speed.state.draft.set(draft);
            speed.show();
            set_text(
                &speed.description,
                &tr(&cursor_speed::description(&model.name_of(id))),
            );
        }
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
    pub fn wake_widgets(&self) -> Option<WakeRow> {
        self.rows.borrow().as_ref().and_then(|r| r.wake.clone())
    }

    #[cfg(test)]
    pub fn offline_widgets(&self) -> Option<OfflineRow> {
        self.rows.borrow().as_ref().and_then(|r| r.offline.clone())
    }

    /// The warnings under the status, as shown.
    #[cfg(test)]
    pub fn offline_warnings(&self) -> Vec<String> {
        self.rows
            .borrow()
            .as_ref()
            .and_then(|r| r.offline.as_ref())
            .map(|o| o.shown.borrow().clone())
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub fn speed_scale(&self) -> Option<gtk::Scale> {
        self.rows
            .borrow()
            .as_ref()
            .and_then(|r| r.speed.as_ref().map(|s| s.scale.clone()))
    }

    #[cfg(test)]
    pub fn speed_value(&self) -> Option<(String, bool)> {
        self.rows.borrow().as_ref().and_then(|r| {
            r.speed
                .as_ref()
                .map(|s| (s.value.text().to_string(), s.reset.is_visible()))
        })
    }

    /// Presses (true) or lets go of (false) the speed slider, as the pointer
    /// does.
    #[cfg(test)]
    pub fn speed_pointer(&self, down: bool) {
        if let Some(speed) = self.rows.borrow().as_ref().and_then(|r| r.speed.as_ref()) {
            if down {
                speed.pressed();
            } else {
                speed.released();
            }
        }
    }

    #[cfg(test)]
    pub fn speed_reset(&self) -> Option<gtk::Button> {
        self.rows
            .borrow()
            .as_ref()
            .and_then(|r| r.speed.as_ref().map(|s| s.reset.clone()))
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
        // Under the status: who took it offline, before what is said of
        // waking it.
        let offline_lines = (!this).then(|| (offline_lines(&about, &status), about.clone()));
        let wake = (!this).then(|| self.wake_row(&about, &status, id));
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

        let speed = cursor_speed::shown(model, id).map(|agent| {
            let group = adw::PreferencesGroup::new();
            let speed = self.speed_row(&group, id, agent);
            self.add(group);
            speed
        });

        let mut rows = Rows {
            id: id.to_owned(),
            name,
            this_name,
            names,
            name_refused,
            status,
            wake,
            offline: None,
            speed,
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

        if device.paired {
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
        }

        if let Some(((icon, set_by), about)) = offline_lines {
            rows.offline = Some(self.offline_group(id, icon, set_by, about));
        }

        if !device.paired {
            return rows;
        }

        let removal = adw::PreferencesGroup::new();
        let remove = gtk::Button::builder()
            .label(t(keys::REMOVE_DEVICE))
            .halign(gtk::Align::Start)
            .css_classes(["destructive-action"])
            .build();
        let (weak, device_id) = (self.ui.clone(), id.to_owned());
        remove.connect_clicked(move |button| {
            if let Some(ui) = weak.upgrade() {
                RemoveQuestion::new(&ui, &device_id)
                    .dialog
                    .present(Some(button));
            }
        });
        removal.add(&remove);
        self.add(removal);
        rows
    }
}

/// The question that removes (unpairs) a computer. While this computer is
/// licensed and the computer uses its license, it also offers to revoke
/// it there ("Also revoke its license", checked), or, for one on
/// InputWeave 1.0, which cannot be told, says its place stays taken.
pub struct RemoveQuestion {
    pub dialog: adw::AlertDialog,
    /// "Also revoke its license" (the rest is for the GTK tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub revoke: Option<gtk::CheckButton>,
    /// The line for a computer on InputWeave 1.0.
    #[cfg_attr(not(test), allow(dead_code))]
    pub line: Option<gtk::Label>,
    /// The notice that its license could not be revoked, once shown.
    #[cfg_attr(not(test), allow(dead_code))]
    pub notice: Rc<RefCell<Option<adw::AlertDialog>>>,
}

impl RemoveQuestion {
    pub fn new(ui: &Rc<Ui>, device_id: &str) -> RemoveQuestion {
        let model = ui.snapshot();
        let name = model.name_of(device_id);
        let offer =
            license_text::remove_offer(&model.status, ui.license.licensed(&model), device_id);
        let dialog = adw::AlertDialog::new(
            Some(&tr(&ui!(keys::REMOVE_DEVICE_TITLE, name.as_str()))),
            Some(&t(keys::REMOVE_DEVICE_MESSAGE)),
        );
        dialog.add_response("cancel", &t(keys::CANCEL));
        dialog.add_response("remove", &t(keys::REMOVE_DEVICE_CONFIRM));
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let (mut revoke, mut line) = (None, None);
        match &offer {
            RemoveOffer::Revoke(_) => {
                let check = gtk::CheckButton::builder()
                    .label(t(keys::REMOVE_DEVICE_REVOKE))
                    .active(true)
                    .build();
                let note = gtk::Label::builder()
                    .label(t(keys::REMOVE_DEVICE_REVOKE_NOTE))
                    .wrap(true)
                    .xalign(0.0)
                    // Under the check box's label, not its box.
                    .margin_start(28)
                    .css_classes(["dim-label", "caption"])
                    .build();
                let extra = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(2)
                    .build();
                extra.append(&check);
                extra.append(&note);
                dialog.set_extra_child(Some(&extra));
                revoke = Some(check);
            }
            RemoveOffer::Legacy { .. } => {
                let label = gtk::Label::builder()
                    .label(offer.line().map(|l| tr(&l)).unwrap_or_default())
                    .wrap(true)
                    .justify(gtk::Justification::Center)
                    .css_classes(["dim-label"])
                    .build();
                dialog.set_extra_child(Some(&label));
                line = Some(label);
            }
            RemoveOffer::Unpair => {}
        }
        let notice: Rc<RefCell<Option<adw::AlertDialog>>> = Rc::default();
        let (weak, id, check, shown) = (
            Rc::downgrade(ui),
            device_id.to_owned(),
            revoke.clone(),
            notice.clone(),
        );
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade() else { return };
            if response != "remove" {
                return;
            }
            // As the agent says now, the box checked.
            let model = ui.snapshot();
            let first =
                match license_text::remove_offer(&model.status, ui.license.licensed(&model), &id) {
                    RemoveOffer::Revoke(first) if check.as_ref().is_some_and(|c| c.is_active()) => {
                        Some(first)
                    }
                    _ => None,
                };
            let name = model.name_of(&id);
            let (weak, shown) = (Rc::downgrade(&ui), shown.clone());
            crate::license::remove_device(&ui, &id, first, move |why| {
                if let Some(ui) = weak.upgrade() {
                    *shown.borrow_mut() = Some(not_revoked_notice(&ui, &name, why));
                }
            });
        });
        RemoveQuestion {
            dialog,
            revoke,
            line,
            notice,
        }
    }
}

/// Said once a computer was removed whose license could not be revoked:
/// its place stays taken, or may be once it activates the key, until it is
/// freed in Polar's customer portal; or its place is free, but it was not
/// told (OK only).
pub fn not_revoked_notice(ui: &Rc<Ui>, name: &str, why: NotRevoked) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(Some(&tr(&why.title(name))), Some(&tr(&why.body())));
    if why.manage() {
        dialog.add_response("manage", &t(keys::LICENSE_MANAGE));
    }
    dialog.add_response("ok", &t(keys::OK));
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("ok");
    let weak = Rc::downgrade(ui);
    dialog.connect_response(Some("manage"), move |_, _| {
        if let Some(ui) = weak.upgrade() {
            crate::license::open(&ui, polar::PORTAL_URL);
        }
    });
    let parent = ui.shown_window();
    dialog.present(parent.as_ref());
    dialog
}

/// The status row's part in taking another computer offline for now: the
/// icon at its end, and below the group (`group`) who set it, hidden until
/// the model says (the warnings follow it there as they come).
fn offline_lines(
    group: &adw::PreferencesGroup,
    status: &adw::ActionRow,
) -> (gtk::Image, gtk::Label) {
    // The status says it in words; the icon only goes with them.
    let icon = gtk::Image::builder()
        .icon_name(offline::ICON)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .css_classes(["dim-label"])
        .visible(false)
        .build();
    status.add_suffix(&icon);
    let set_by = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .margin_top(6)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["dim-label", "caption"])
        .visible(false)
        .build();
    // Not a row: below the group's list, under the status.
    group.add(&set_by);
    (icon, set_by)
}

impl Inspector {
    /// Take Offline or Bring Back Online for another computer, `id`, in a
    /// group of its own, with what it does under it; with the lines made
    /// by `offline_lines` in `about`.
    fn offline_group(
        &self,
        id: &str,
        icon: gtk::Image,
        set_by: gtk::Label,
        about: adw::PreferencesGroup,
    ) -> OfflineRow {
        let group = adw::PreferencesGroup::builder().visible(false).build();
        let button = gtk::Button::builder()
            .label(t(keys::OFFLINE_TAKE))
            .halign(gtk::Align::Start)
            .build();
        let (weak, device_id) = (self.ui.clone(), id.to_owned());
        button.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            // As the service says now, whatever the label still shows.
            if let Some(offline) = offline::offer(&ui.snapshot(), &device_id) {
                ui.set_offline(&device_id, !offline);
            }
        });
        let explanation = gtk::Label::builder()
            .label(t(keys::OFFLINE_EXPLAIN))
            .wrap(true)
            .xalign(0.0)
            .margin_top(6)
            .css_classes(["dim-label", "caption"])
            .build();
        group.add(&button);
        group.add(&explanation);
        self.add(group.clone());
        OfflineRow {
            icon,
            set_by,
            about,
            group,
            button,
            shown: Rc::default(),
            lines: Rc::default(),
        }
    }

    /// The status row's part in waking another computer, `id`: the glyph
    /// and Wake at its end, and the lines under it below the group.
    fn wake_row(
        &self,
        group: &adw::PreferencesGroup,
        status: &adw::ActionRow,
        id: &str,
    ) -> WakeRow {
        // The status says it in words; the moon only goes with them.
        let moon = gtk::Image::builder()
            .icon_name(wake::MOON_ICON)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .css_classes(["dim-label"])
            .visible(false)
            .build();
        let spinner = gtk::Spinner::builder()
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let button = row_button(&t(keys::WAKE));
        button.add_css_class("suggested-action");
        button.set_visible(false);
        let (weak, device_id) = (self.ui.clone(), id.to_owned());
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.wake_device(&device_id);
            }
        });
        status.add_suffix(&moon);
        status.add_suffix(&spinner);
        status.add_suffix(&button);

        let warning = gtk::Image::builder()
            .icon_name("dialog-warning-symbolic")
            .accessible_role(gtk::AccessibleRole::Presentation)
            .css_classes(["warning"])
            .valign(gtk::Align::Start)
            .build();
        let not_woken_text = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .hexpand(true)
            .build();
        not_woken_text.connect_activate_link(|label, uri| {
            let window = label.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(uri).launch(
                window.as_ref(),
                gtk::gio::Cancellable::NONE,
                |result| {
                    if let Err(e) = result {
                        log::info(&format!("cannot open a link: {e}"));
                    }
                },
            );
            glib::Propagation::Stop
        });
        let not_woken = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_top(6)
            .margin_start(12)
            .margin_end(12)
            .visible(false)
            .build();
        not_woken.append(&warning);
        not_woken.append(&not_woken_text);
        let not_told = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .margin_top(6)
            .margin_start(12)
            .margin_end(12)
            .css_classes(["dim-label", "caption"])
            .visible(false)
            .build();
        // Not rows: below the group's list, under the status.
        group.add(&not_woken);
        group.add(&not_told);
        WakeRow {
            moon,
            spinner,
            button,
            not_woken,
            not_woken_text,
            not_told,
        }
    }

    /// The cursor speed state of `id`, as it was when last shown.
    fn speed_state(&self, id: &str) -> SpeedState {
        self.speeds
            .borrow_mut()
            .entry(id.to_owned())
            .or_insert_with(|| SpeedState {
                ui: self.ui.clone(),
                device_id: id.into(),
                quiet: self.quiet.clone(),
                draft: Rc::default(),
                agent: Rc::default(),
                pending: Rc::default(),
            })
            .clone()
    }

    /// Makes the cursor speed row of `id`, whose speed the agent says is
    /// `agent`, in `group`.
    fn speed_row(self: &Rc<Self>, group: &adw::PreferencesGroup, id: &str, agent: u32) -> SpeedRow {
        let title = t(keys::INSPECTOR_CURSOR_SPEED);
        let heading = gtk::Label::builder()
            .label(&title)
            .xalign(0.0)
            .wrap(true)
            .build();
        let description = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label", "caption"])
            .build();
        let words = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        words.append(&heading);
        words.append(&description);
        let value = value_label(&cursor_speed::label(agent));
        let reset = row_button(&t(keys::INSPECTOR_CURSOR_SPEED_RESET));
        reset.add_css_class("flat");
        let header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .build();
        header.append(&words);
        header.append(&value);
        header.append(&reset);

        let adjustment = gtk::Adjustment::new(
            f64::from(agent),
            f64::from(cursor_speed::MIN),
            f64::from(cursor_speed::MAX),
            f64::from(cursor_speed::STEP),
            50.0,
            0.0,
        );
        let scale = gtk::Scale::builder()
            .orientation(gtk::Orientation::Horizontal)
            .adjustment(&adjustment)
            .draw_value(false)
            .round_digits(0)
            .build();
        for tick in cursor_speed::TICKS {
            scale.add_mark(
                f64::from(tick),
                gtk::PositionType::Bottom,
                Some(&cursor_speed::label(tick)),
            );
        }
        scale.update_property(&[gtk::accessible::Property::Label(&title)]);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(12)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&header);
        content.append(&scale);
        // A preferences row, so the group puts it in its boxed list.
        let row = adw::PreferencesRow::builder()
            .title(&title)
            .activatable(false)
            .selectable(false)
            .child(&content)
            .build();
        group.add(&row);

        let speed = SpeedRow {
            scale: scale.clone(),
            value,
            reset: reset.clone(),
            description,
            state: self.speed_state(id),
        };
        speed.state.agent.set(agent);
        // Steps of 10, wherever the pointer or the keys put it.
        scale.connect_change_value(|range, _, position| {
            range.set_value(f64::from(cursor_speed::snapped(position)));
            glib::Propagation::Stop
        });
        let row = speed.downgrade();
        scale.connect_value_changed(move |scale| {
            if row.state.quiet.is_set() {
                return;
            }
            if let Some(speed) = row.upgrade() {
                speed.moved(scale.value());
            }
        });
        // The pointer pressing and letting go, seen before the scale's own
        // gestures take them: GTK's scale has no signal for either.
        let pointer = gtk::EventControllerLegacy::new();
        pointer.set_propagation_phase(gtk::PropagationPhase::Capture);
        let row = speed.downgrade();
        pointer.connect_event(move |_, event| {
            use gtk::gdk::EventType;
            if let Some(speed) = row.upgrade() {
                match event.event_type() {
                    EventType::ButtonPress | EventType::TouchBegin => speed.pressed(),
                    EventType::ButtonRelease | EventType::TouchEnd | EventType::TouchCancel => {
                        speed.released()
                    }
                    _ => {}
                }
            }
            glib::Propagation::Proceed
        });
        scale.add_controller(pointer);
        // The wheel and the touchpad scroll the page, never the speed: the
        // scale's own scroll handling is turned off, so scrolling over it
        // goes on to the page as anywhere else.
        let controllers = scale.observe_controllers();
        for i in 0..controllers.n_items() {
            if let Some(scroll) = controllers
                .item(i)
                .and_downcast::<gtk::EventControllerScroll>()
            {
                scroll.set_propagation_phase(gtk::PropagationPhase::None);
            }
        }
        let row = speed.downgrade();
        reset.connect_clicked(move |_| {
            if let Some(speed) = row.upgrade() {
                speed.reset();
            }
        });
        speed.show();
        speed
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
        "{id}|{}|{:?}|{}|{}|{}",
        id == model.this_device_id(),
        model.device(id).map(|d| d.paired),
        os_of(model, id),
        screens.join(","),
        cursor_speed::shown(model, id).is_some(),
    )
}
