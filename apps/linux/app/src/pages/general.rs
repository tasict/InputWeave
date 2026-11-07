//! General: what sharing is doing (with bringing this computer back while
//! it is offline for now), this computer's name, opening at login,
//! the clipboard, desktop pictures, switching (with the edge glow, the
//! app's own setting, and waking a computer the cursor pushes toward) and
//! the shortcuts.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::SystemTime;

use adw::prelude::*;
use gtk::gio;
use inputweave_app_core::proto::{
    SetClipboardSync, SetDeviceName, SetEdgeOptions, SetPreviewSharing, SetRelativeAuto,
    SetSharing, SetSwitchModifier, SetWakeOnPush, SwitchModifier, client_message::Command,
};
use inputweave_app_core::service::ServiceState;
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::status_text::{self, ProblemAction};
use inputweave_app_core::text::keys;
use inputweave_app_core::{autostart, clipboard_text, edge_glow, log, offline, paths, wake};

use super::{EntrySync, Quiet, set_subtitle, set_switch, set_title};
use crate::strings::{t, tr};
use crate::ui::Ui;
use crate::window::Page;

/// The switch modifiers in the menu's order.
const MODIFIERS: [(SwitchModifier, &str); 5] = [
    (SwitchModifier::None, keys::SWITCH_ALWAYS),
    (SwitchModifier::Control, keys::SWITCH_HOLDING_CONTROL),
    (SwitchModifier::Alt, keys::SWITCH_HOLDING_ALT),
    (SwitchModifier::Shift, keys::SWITCH_HOLDING_SHIFT),
    (SwitchModifier::Gui, keys::SWITCH_HOLDING_SUPER),
];

pub struct General {
    page: adw::PreferencesPage,
    quiet: Quiet,
    status: adw::ActionRow,
    /// Bring Back Online, at the end of the status while this computer is
    /// offline for now: the panel's button on the other two apps.
    bring_back: gtk::Button,
    sharing: adw::SwitchRow,
    problems: adw::PreferencesGroup,
    problem_rows: RefCell<Vec<gtk::Widget>>,
    shown_problems: RefCell<String>,
    clipboard_line: adw::ActionRow,
    name: EntrySync,
    name_group: adw::PreferencesGroup,
    login: adw::SwitchRow,
    /// Whether the app opens at login, as last read from its autostart
    /// entry: when the page was made, after the switch, and whenever the
    /// file changes, not on every refresh.
    at_login: Rc<Cell<bool>>,
    _login_monitor: Option<gio::FileMonitor>,
    clipboard: adw::SwitchRow,
    pictures: adw::SwitchRow,
    pictures_unavailable: adw::ActionRow,
    wait: adw::SwitchRow,
    modifier: adw::ComboRow,
    relative_auto: adw::SwitchRow,
    /// SetRelativeAuto was refused while the service reported this; the
    /// hint says so in red until the service reports another value or the
    /// connection ends, as on the Windows app.
    relative_auto_failed: Rc<Cell<Option<bool>>>,
    glow: adw::SwitchRow,
    wake_on_push: adw::SwitchRow,
    /// SetWakeOnPush was refused while the service reported this: said in
    /// red, as for the relative mouse switch.
    wake_on_push_failed: Rc<Cell<Option<bool>>>,
    shortcuts: Rc<super::shortcuts::Shortcuts>,
}

impl General {
    pub fn new(ui: &Rc<Ui>) -> General {
        let page = adw::PreferencesPage::new();
        let quiet = Quiet::default();

        let status_group = adw::PreferencesGroup::builder()
            .title(t(keys::STATUS_HEADER))
            .build();
        let status = adw::ActionRow::builder().use_markup(false).build();
        let bring_back = super::row_button(&t(keys::OFFLINE_BRING_BACK));
        bring_back.set_visible(false);
        status.add_suffix(&bring_back);
        let sharing = adw::SwitchRow::builder()
            .use_markup(false)
            .title(t(keys::GENERAL_SHARING))
            .build();
        let clipboard_line = adw::ActionRow::builder()
            .use_markup(false)
            .css_classes(["dim-label"])
            .visible(false)
            .build();
        status_group.add(&status);
        status_group.add(&sharing);
        status_group.add(&clipboard_line);
        page.add(&status_group);
        let problems = adw::PreferencesGroup::new();
        problems.set_visible(false);
        page.add(&problems);

        let name_group = adw::PreferencesGroup::builder()
            .description(t(keys::GENERAL_NAME_HINT))
            .build();
        let name = adw::EntryRow::builder()
            .use_markup(false)
            .title(t(keys::GENERAL_NAME))
            .show_apply_button(true)
            .build();
        name_group.add(&name);
        let login = adw::SwitchRow::builder()
            .use_markup(false)
            .title(t(keys::GENERAL_OPEN_AT_LOGIN))
            .build();
        name_group.add(&login);
        page.add(&name_group);

        let clipboard_group = adw::PreferencesGroup::builder()
            .title(t(keys::GENERAL_CLIPBOARD))
            .build();
        let clipboard = adw::SwitchRow::builder()
            .use_markup(false)
            .title(t(keys::GENERAL_CLIPBOARD_SYNC))
            .subtitle(t(keys::GENERAL_CLIPBOARD_HINT))
            .build();
        clipboard_group.add(&clipboard);
        page.add(&clipboard_group);

        let pictures_group = adw::PreferencesGroup::builder()
            .title(t(keys::GENERAL_PICTURES))
            .build();
        let pictures = adw::SwitchRow::builder()
            .use_markup(false)
            .title(t(keys::GENERAL_PICTURES_SHARE))
            .subtitle(t(keys::GENERAL_PICTURES_HINT))
            .build();
        let pictures_unavailable = adw::ActionRow::builder()
            .use_markup(false)
            .title(t(keys::GENERAL_PICTURES_UNAVAILABLE))
            .build();
        pictures_group.add(&pictures);
        pictures_group.add(&pictures_unavailable);
        page.add(&pictures_group);

        let switching = adw::PreferencesGroup::builder()
            .title(t(keys::GENERAL_SWITCHING))
            .build();
        let wait = adw::SwitchRow::builder()
            .use_markup(false)
            .title(t(keys::GENERAL_WAIT))
            .subtitle(t(keys::GENERAL_WAIT_HINT))
            .build();
        let names: Vec<String> = MODIFIERS.iter().map(|(_, key)| t(key)).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let modifier = adw::ComboRow::builder()
            .use_markup(false)
            .title(t(keys::SWITCH_MODIFIER_TITLE))
            .subtitle(t(keys::SWITCH_MODIFIER_HINT))
            .model(&gtk::StringList::new(&names))
            .build();
        let relative_auto = adw::SwitchRow::builder()
            .use_markup(false)
            .title(t(keys::GENERAL_RELATIVE_AUTO))
            .subtitle(t(keys::GENERAL_RELATIVE_AUTO_HINT))
            .build();
        let glow = adw::SwitchRow::builder()
            .use_markup(false)
            .title(t(keys::GENERAL_EDGE_GLOW))
            .subtitle(t(keys::GENERAL_EDGE_GLOW_HINT))
            .build();
        let wake_on_push = adw::SwitchRow::builder()
            .use_markup(false)
            .title(t(keys::GENERAL_WAKE_ON_PUSH))
            .subtitle(t(keys::GENERAL_WAKE_ON_PUSH_HINT))
            .build();
        switching.add(&wait);
        switching.add(&modifier);
        switching.add(&relative_auto);
        switching.add(&glow);
        switching.add(&wake_on_push);
        page.add(&switching);

        let shortcuts = super::shortcuts::Shortcuts::new(ui);
        page.add(shortcuts.widget());

        // Updates come through the system's package manager, from the
        // package source the .deb added (docs/architecture.md §12.4).
        let updates = adw::PreferencesGroup::builder()
            .title(t(keys::UPDATES_TITLE))
            .description(t(keys::UPDATES_NOTE))
            .build();
        updates.add(
            &adw::ActionRow::builder()
                .use_markup(false)
                .title(tr(&inputweave_app_core::ui!(
                    keys::UPDATES_VERSION,
                    env!("CARGO_PKG_VERSION")
                )))
                .subtitle(t(keys::UPDATES_BODY))
                .subtitle_lines(0)
                .build(),
        );
        updates.add(
            &adw::ActionRow::builder()
                .use_markup(false)
                .title(t(keys::UPDATES_SOURCE))
                .subtitle("inputweave.ctsite.cc")
                .css_classes(["iw-command"])
                .build(),
        );
        page.add(&updates);

        let at_login = Rc::new(Cell::new(read_at_login()));
        let login_monitor = watch_at_login(&at_login);
        let general = General {
            page,
            quiet,
            status,
            bring_back,
            sharing,
            problems,
            problem_rows: RefCell::new(Vec::new()),
            shown_problems: RefCell::new(String::new()),
            clipboard_line,
            name: EntrySync::new(&name),
            name_group,
            login,
            at_login,
            _login_monitor: login_monitor,
            clipboard,
            pictures,
            pictures_unavailable,
            wait,
            modifier,
            relative_auto,
            relative_auto_failed: Rc::default(),
            glow,
            wake_on_push,
            wake_on_push_failed: Rc::default(),
            shortcuts,
        };
        general.connect(ui);
        general
    }

    fn connect(&self, ui: &Rc<Ui>) {
        let weak = Rc::downgrade(ui);
        let on = move |quiet: &Quiet, make: fn(bool) -> Command| {
            let weak = weak.clone();
            let quiet = quiet.clone();
            move |row: &adw::SwitchRow| {
                if quiet.is_set() {
                    return;
                }
                if let Some(ui) = weak.upgrade() {
                    ui.command(make(row.is_active()));
                }
            }
        };
        self.sharing
            .connect_active_notify(on(&self.quiet, |enabled| {
                Command::SetSharing(SetSharing { enabled })
            }));
        let weak = Rc::downgrade(ui);
        self.bring_back.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                let this = ui.snapshot().this_device_id().to_owned();
                ui.set_offline(&this, false);
            }
        });
        self.clipboard
            .connect_active_notify(on(&self.quiet, |enabled| {
                Command::SetClipboardSync(SetClipboardSync { enabled })
            }));
        self.pictures
            .connect_active_notify(on(&self.quiet, |enabled| {
                Command::SetPreviewSharing(SetPreviewSharing { enabled })
            }));
        let (weak, quiet, failed) = (
            Rc::downgrade(ui),
            self.quiet.clone(),
            self.relative_auto_failed.clone(),
        );
        self.relative_auto.connect_active_notify(move |row| {
            if quiet.is_set() {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            let reported = ui.snapshot().status.relative_auto;
            let failed = failed.clone();
            let refresh = Rc::downgrade(&ui);
            ui.command_then(
                Command::SetRelativeAuto(SetRelativeAuto {
                    on: row.is_active(),
                }),
                move |result| {
                    if result.is_err() {
                        failed.set(Some(reported));
                    }
                    if let Some(ui) = refresh.upgrade() {
                        ui.refresh();
                    }
                    true
                },
            );
        });
        let (weak, quiet, failed) = (
            Rc::downgrade(ui),
            self.quiet.clone(),
            self.wake_on_push_failed.clone(),
        );
        self.wake_on_push.connect_active_notify(move |row| {
            if quiet.is_set() {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            let reported = ui.snapshot().status.wake_on_push;
            let failed = failed.clone();
            let refresh = Rc::downgrade(&ui);
            ui.command_then(
                Command::SetWakeOnPush(SetWakeOnPush {
                    on: row.is_active(),
                }),
                move |result| {
                    if result.is_err() {
                        failed.set(Some(reported));
                    }
                    if let Some(ui) = refresh.upgrade() {
                        ui.refresh();
                    }
                    true
                },
            );
        });
        let (weak, quiet) = (Rc::downgrade(ui), self.quiet.clone());
        self.glow.connect_active_notify(move |row| {
            if !quiet.is_set()
                && let Some(ui) = weak.upgrade()
            {
                ui.set_edge_glow(row.is_active());
            }
        });
        let (weak, quiet) = (Rc::downgrade(ui), self.quiet.clone());
        self.wait.connect_active_notify(move |row| {
            if quiet.is_set() {
                return;
            }
            if let Some(ui) = weak.upgrade() {
                let dwell_ms = if row.is_active() {
                    ui.snapshot().switch_delay_ms()
                } else {
                    0
                };
                ui.command(Command::SetEdgeOptions(SetEdgeOptions { dwell_ms }));
            }
        });
        let (weak, quiet) = (Rc::downgrade(ui), self.quiet.clone());
        self.modifier.connect_selected_notify(move |row| {
            if quiet.is_set() {
                return;
            }
            if let (Some(ui), Some((modifier, _))) =
                (weak.upgrade(), MODIFIERS.get(row.selected() as usize))
            {
                ui.command(Command::SetSwitchModifier(SetSwitchModifier {
                    modifier: *modifier as i32,
                }));
            }
        });
        let weak = Rc::downgrade(ui);
        // Weakly: the group holds the row, whose handler this is.
        let group = self.name_group.downgrade();
        let edits = self.name.edits();
        self.name.row().connect_apply(move |entry| {
            let (Some(ui), Some(group)) = (weak.upgrade(), group.upgrade()) else {
                return;
            };
            edits.applied();
            ui.command_then(
                Command::SetDeviceName(SetDeviceName {
                    name: entry.text().trim().to_owned(),
                }),
                move |result| match result {
                    Ok(_) => {
                        group.set_description(Some(&t(keys::GENERAL_NAME_HINT)));
                        true
                    }
                    Err(e) if e.code() == Some("invalid_name") => {
                        group.set_description(Some(&t(keys::NAME_INVALID)));
                        true
                    }
                    Err(_) => false,
                },
            );
        });
        let quiet = self.quiet.clone();
        let weak = Rc::downgrade(ui);
        let at_login = self.at_login.clone();
        self.login.connect_active_notify(move |row| {
            if quiet.is_set() {
                return;
            }
            let (Some(dir), Ok(exe)) = (paths::autostart_dir(), std::env::current_exe()) else {
                return;
            };
            if let Err(e) = autostart::set(&dir, row.is_active(), &exe) {
                log::info(&format!("cannot change opening at login: {e}"));
                if let Some(ui) = weak.upgrade() {
                    ui.toast(&t(keys::FILES_SETTINGS_FAILED));
                }
            }
            at_login.set(read_at_login());
            if let Some(ui) = weak.upgrade() {
                ui.refresh();
            }
        });
    }

    pub fn widget(&self) -> &adw::PreferencesPage {
        &self.page
    }

    /// Reads the autostart entry again, as when the window is shown.
    pub fn reread_login(&self) {
        self.at_login.set(read_at_login());
    }

    pub fn update(&self, model: &Snapshot, service: ServiceState) {
        let now = SystemTime::now();
        let status = &model.status;
        set_title(&self.status, &tr(&status_text::title(model, service, now)));
        let subtitle = status_text::subtitle(model)
            .map(|s| tr(&s))
            .unwrap_or_default();
        set_subtitle(&self.status, &subtitle);
        // Sharing keeps the user's own choice, and can be changed, while
        // this computer is offline for now.
        self.bring_back
            .set_visible(model.connected() && offline::this_offline(model));
        set_switch(&self.quiet, &self.sharing, status.sharing);
        match clipboard_text::line(model, now) {
            Some(line) => {
                set_title(&self.clipboard_line, &tr(&line));
                self.clipboard_line.set_visible(true);
            }
            None => self.clipboard_line.set_visible(false),
        }
        self.update_problems(model);

        self.name.show(&status.this_device_name);
        set_switch(&self.quiet, &self.login, self.at_login.get());
        set_switch(&self.quiet, &self.clipboard, status.clipboard_sync);
        self.pictures.set_visible(status.desktop_pictures_available);
        self.pictures_unavailable
            .set_visible(!status.desktop_pictures_available);
        set_switch(&self.quiet, &self.pictures, status.preview_sharing);
        set_switch(&self.quiet, &self.wait, status.edge_dwell_ms != 0);
        let selected = MODIFIERS
            .iter()
            .position(|(m, _)| *m as i32 == status.switch_modifier)
            .unwrap_or(0) as u32;
        if self.modifier.selected() != selected {
            self.quiet.run(|| self.modifier.set_selected(selected));
        }
        let relative = model.relative_supported();
        self.relative_auto.set_visible(relative);
        set_switch(&self.quiet, &self.relative_auto, status.relative_auto);
        // A refusal stays said until the service reports another value or
        // can no longer be reached.
        if let Some(when) = self.relative_auto_failed.get()
            && (when != status.relative_auto || !model.connected())
        {
            self.relative_auto_failed.set(None);
        }
        let failed = self.relative_auto_failed.get().is_some();
        let hint = if failed {
            keys::GENERAL_RELATIVE_AUTO_FAILED
        } else if model.relative_shortcut().is_some() {
            keys::GENERAL_RELATIVE_AUTO_HINT
        } else {
            keys::GENERAL_RELATIVE_AUTO_HINT_NO_SHORTCUT
        };
        if self.relative_auto.subtitle().as_deref() != Some(t(hint).as_str()) {
            self.relative_auto.set_subtitle(&t(hint));
        }
        if failed {
            self.relative_auto.add_css_class("iw-failed");
        } else {
            self.relative_auto.remove_css_class("iw-failed");
        }
        // The app's own setting; GNOME's Wayland has no way to show it.
        if let Some(ui) = crate::ui::current() {
            set_switch(&self.quiet, &self.glow, ui.edge_glow());
            // The agent's: an app started over D-Bus may lack the variable.
            let desktop = model
                .status
                .session
                .as_ref()
                .map(|s| s.desktop.clone())
                .filter(|d| !d.is_empty())
                .or_else(|| std::env::var("XDG_CURRENT_DESKTOP").ok())
                .unwrap_or_default();
            // Until the overlays are first needed, as far as the session
            // tells: GNOME's Wayland cannot show it.
            let shows = ui
                .glow_availability()
                .unwrap_or_else(|| edge_glow::expected(model.wayland(), &desktop));
            self.glow
                .set_sensitive(shows == edge_glow::Availability::Shown);
            let hint = t(edge_glow::hint_key(shows, &desktop));
            if self.glow.subtitle().as_deref() != Some(hint.as_str()) {
                self.glow.set_subtitle(&hint);
            }
        }
        self.update_wake_on_push(model);
        self.shortcuts.update(model);
    }

    /// Waking a computer the cursor pushes toward: from a service that
    /// wakes devices; a refusal is said as the relative mouse switch's.
    /// Where the cursor leaves only across edges toward connected computers
    /// (Wayland), a push never reaches the service: insensitive, saying
    /// why, as the edge glow's switch where no glow shows.
    fn update_wake_on_push(&self, model: &Snapshot) {
        let status = &model.status;
        self.wake_on_push.set_visible(wake::supported(model));
        let edges_only = model.edges_only();
        self.wake_on_push.set_sensitive(!edges_only);
        set_switch(&self.quiet, &self.wake_on_push, status.wake_on_push);
        if let Some(when) = self.wake_on_push_failed.get()
            && (when != status.wake_on_push || !model.connected())
        {
            self.wake_on_push_failed.set(None);
        }
        let failed = self.wake_on_push_failed.get().is_some();
        let hint = t(if failed {
            keys::GENERAL_RELATIVE_AUTO_FAILED
        } else if edges_only {
            keys::GENERAL_WAKE_ON_PUSH_EDGES_ONLY
        } else {
            keys::GENERAL_WAKE_ON_PUSH_HINT
        });
        if self.wake_on_push.subtitle().as_deref() != Some(hint.as_str()) {
            self.wake_on_push.set_subtitle(&hint);
        }
        if failed {
            self.wake_on_push.add_css_class("iw-failed");
        } else {
            self.wake_on_push.remove_css_class("iw-failed");
        }
    }

    #[cfg(test)]
    pub fn wake_on_push_row(&self) -> &adw::SwitchRow {
        &self.wake_on_push
    }

    #[cfg(test)]
    pub fn status_row(&self) -> &adw::ActionRow {
        &self.status
    }

    #[cfg(test)]
    pub fn bring_back_button(&self) -> &gtk::Button {
        &self.bring_back
    }

    pub fn shortcuts(&self) -> &Rc<super::shortcuts::Shortcuts> {
        &self.shortcuts
    }

    fn update_problems(&self, model: &Snapshot) {
        let rows = status_text::problems(model, true);
        let key = format!("{rows:?}");
        if *self.shown_problems.borrow() == key {
            return;
        }
        *self.shown_problems.borrow_mut() = key;
        let mut shown = self.problem_rows.borrow_mut();
        super::clear_group(&self.problems, &mut shown);
        for problem in &rows {
            let row = super::plain_row();
            row.set_title(&tr(&problem.text));
            row.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
            if let Some(key) = problem.action_key() {
                let button = super::row_button(&t(key));
                let action = problem.action;
                button.connect_clicked(move |_| {
                    let Some(ui) = crate::ui::current() else {
                        return;
                    };
                    match action {
                        ProblemAction::Troubleshoot => ui.show_window(Some(Page::Troubleshooting)),
                        ProblemAction::AddDevice => ui.add_device(),
                        ProblemAction::None => {}
                    }
                });
                row.add_suffix(&button);
            }
            self.problems.add(&row);
            shown.push(row.upcast());
        }
        self.problems.set_visible(!rows.is_empty());
    }
}

fn read_at_login() -> bool {
    paths::autostart_dir().is_some_and(|d| autostart::read(&d))
}

/// Watches the autostart entry, which the desktop's own settings may change
/// too; the next refresh shows what it says.
fn watch_at_login(at_login: &Rc<Cell<bool>>) -> Option<gio::FileMonitor> {
    let path = autostart::path_in(&paths::autostart_dir()?);
    let monitor = gio::File::for_path(path)
        .monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        .map_err(|e| log::info(&format!("cannot watch the autostart entry: {e}")))
        .ok()?;
    let at_login = at_login.clone();
    monitor.connect_changed(move |_, _, _, _| at_login.set(read_at_login()));
    Some(monitor)
}
