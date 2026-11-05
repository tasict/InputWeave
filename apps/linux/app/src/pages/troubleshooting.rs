//! Troubleshooting: the problems or "everything is working", the
//! permissions the desktop asks for (Wayland) and the firewall, the
//! background service with starting and restarting it, versions, this
//! computer's name and id, the desktop session and what serves each part
//! of it, exporting diagnostics, and how to read the logs (the systemd
//! journal: there is no log folder on Linux).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use inputweave_app_core::agent::Answer;
use inputweave_app_core::proto::{GetDiagnostics, client_message::Command, response};
use inputweave_app_core::service::{self, AGENT_UNIT, ServiceAction, ServiceState};
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;
use inputweave_app_core::{log, status_text, trouble_text, ui};

use super::set_subtitle;
use crate::strings::{t, tr};
use crate::ui::Ui;

pub struct Troubleshooting {
    ui: std::rc::Weak<Ui>,
    page: adw::PreferencesPage,
    problems: adw::PreferencesGroup,
    problem_rows: RefCell<Vec<gtk::Widget>>,
    shown: RefCell<String>,
    /// The permissions the desktop asks for, with the clipboard's note.
    permissions: adw::PreferencesGroup,
    permission_rows: RefCell<Vec<gtk::Widget>>,
    shown_permissions: RefCell<String>,
    firewall: adw::PreferencesGroup,
    firewall_rows: RefCell<Vec<gtk::Widget>>,
    shown_firewall: RefCell<String>,
    session: adw::PreferencesGroup,
    session_rows: RefCell<Vec<gtk::Widget>>,
    shown_session: RefCell<String>,
    service: adw::ActionRow,
    service_button: gtk::Button,
    version: adw::ActionRow,
    build: adw::ActionRow,
    control: adw::ActionRow,
    name: adw::ActionRow,
    id: adw::ActionRow,
}

fn property(title: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .use_markup(false)
        .title(title)
        .subtitle_selectable(true)
        .css_classes(["property"])
        .build()
}

impl Troubleshooting {
    pub fn new(ui: &Rc<Ui>) -> Troubleshooting {
        let page = adw::PreferencesPage::new();
        let problems = adw::PreferencesGroup::builder()
            .title(t(keys::TROUBLE_PROBLEMS))
            .build();
        page.add(&problems);
        let permissions = adw::PreferencesGroup::builder()
            .title(t(keys::TROUBLE_PERMISSIONS))
            .description(t(keys::PERMISSIONS_HINT))
            .visible(false)
            .build();
        page.add(&permissions);
        let firewall = adw::PreferencesGroup::builder()
            .title(t(keys::TROUBLE_FIREWALL))
            .visible(false)
            .build();
        page.add(&firewall);

        let about = adw::PreferencesGroup::builder()
            .description(t(keys::TROUBLE_RESTART_HINT))
            .build();
        let service = property(&t(keys::TROUBLE_SERVICE));
        let service_button = super::row_button(&t(keys::ACTION_RESTART_SERVICE));
        service.add_suffix(&service_button);
        about.add(&service);
        let version = property(&t(keys::TROUBLE_VERSION));
        let build = property(&t(keys::TROUBLE_BUILD));
        let control = property(&t(keys::TROUBLE_KEYBOARD_MOUSE));
        let name = property(&t(keys::GENERAL_NAME));
        let id = property(&t(keys::TROUBLE_DEVICE_ID));
        for row in [&version, &build, &control, &name, &id] {
            about.add(row);
        }
        page.add(&about);

        let session = adw::PreferencesGroup::builder()
            .title(t(keys::TROUBLE_SESSION))
            .description(t(keys::TROUBLE_SESSION_HINT))
            .visible(false)
            .build();
        page.add(&session);

        let reports = adw::PreferencesGroup::new();
        let export = adw::ActionRow::builder()
            .use_markup(false)
            .title(t(keys::TROUBLE_EXPORT))
            .subtitle(t(keys::TROUBLE_EXPORT_HINT))
            .activatable(true)
            .build();
        export.add_suffix(&gtk::Image::from_icon_name("document-save-symbolic"));
        reports.add(&export);
        page.add(&reports);
        // The journal keeps both logs; the app's may name files, which
        // diagnostics never hold, so the report only says how to read it.
        let logs = adw::PreferencesGroup::builder()
            .title(t(keys::TROUBLE_JOURNAL))
            .description(t(keys::TROUBLE_JOURNAL_HINT))
            .build();
        logs.add(&super::command_row(
            ui,
            &t(keys::TROUBLE_SERVICE),
            &service::journal_command(AGENT_UNIT),
        ));
        logs.add(&super::command_row(
            ui,
            &t(keys::TROUBLE_JOURNAL_APP),
            &service::app_journal_command(),
        ));
        page.add(&logs);

        let weak = Rc::downgrade(ui);
        service_button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade()
                && let Some(action) = ServiceAction::for_state(ui.service())
            {
                ui.control_service(action);
            }
        });
        let weak = Rc::downgrade(ui);
        export.connect_activated(move |row| {
            if let Some(ui) = weak.upgrade() {
                export_diagnostics(&ui, row.root().and_downcast::<gtk::Window>());
            }
        });

        Troubleshooting {
            ui: Rc::downgrade(ui),
            page,
            problems,
            problem_rows: RefCell::new(Vec::new()),
            shown: RefCell::new(String::new()),
            permissions,
            permission_rows: RefCell::new(Vec::new()),
            shown_permissions: RefCell::new(String::new()),
            firewall,
            firewall_rows: RefCell::new(Vec::new()),
            shown_firewall: RefCell::new(String::new()),
            session,
            session_rows: RefCell::new(Vec::new()),
            shown_session: RefCell::new(String::new()),
            service,
            service_button,
            version,
            build,
            control,
            name,
            id,
        }
    }

    pub fn widget(&self) -> &adw::PreferencesPage {
        &self.page
    }

    pub fn update(&self, model: &Snapshot, service_state: ServiceState) {
        let rows = status_text::problems(model, false);
        let good = status_text::everything_works(model, service_state);
        let key = format!("{rows:?}{good}{}", model.status.capturing);
        if *self.shown.borrow() != key {
            *self.shown.borrow_mut() = key;
            let mut shown = self.problem_rows.borrow_mut();
            super::clear_group(&self.problems, &mut shown);
            if rows.is_empty() {
                let text = if good {
                    t(keys::TROUBLE_ALL_GOOD)
                } else {
                    tr(&ui!(keys::CANNOT_CONTROL_YET))
                };
                let row = adw::ActionRow::builder()
                    .use_markup(false)
                    .title(text)
                    .build();
                row.add_prefix(&gtk::Image::from_icon_name(if good {
                    "emblem-ok-symbolic"
                } else {
                    "dialog-warning-symbolic"
                }));
                self.problems.add(&row);
                shown.push(row.upcast());
            }
            for problem in &rows {
                let row = super::plain_row();
                row.set_title(&tr(&problem.text));
                row.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
                self.problems.add(&row);
                shown.push(row.upcast());
            }
        }
        self.update_permissions(model);
        self.update_firewall(model);
        self.update_session(model);
        set_subtitle(
            &self.service,
            &tr(&service::status_text(service_state, model.link)),
        );
        // The same offer as the window's cover. While connected the unit is
        // usually running, but an agent started by hand runs beside a
        // stopped or missing one.
        let action = ServiceAction::for_state(service_state);
        if let Some(action) = action
            && self.service_button.label().as_deref() != Some(t(action.key()).as_str())
        {
            self.service_button.set_label(&t(action.key()));
        }
        self.service_button.set_visible(action.is_some());
        let connected = model.connected();
        let dash = "–".to_owned();
        let app = env!("CARGO_PKG_VERSION");
        let version = if connected && !model.agent_version.is_empty() {
            if model.agent_version == app {
                app.to_owned()
            } else {
                format!("{} / {app}", model.agent_version)
            }
        } else {
            app.to_owned()
        };
        set_subtitle(&self.version, &version);
        set_subtitle(
            &self.build,
            if connected && !model.agent_digest.is_empty() {
                &model.agent_digest
            } else {
                &dash
            },
        );
        set_subtitle(
            &self.control,
            &t(if model.status.capturing {
                keys::CAN_CONTROL
            } else {
                keys::CANNOT_CONTROL_YET
            }),
        );
        set_subtitle(&self.name, &model.status.this_device_name);
        set_subtitle(&self.id, &model.status.this_device_id);
    }
}

impl Troubleshooting {
    /// The portals' permissions, with a button to ask the desktop again,
    /// and why the clipboard waits on GNOME.
    fn update_permissions(&self, model: &Snapshot) {
        // Only what the service says now, as the firewall and the session:
        // a button would ask nobody while it cannot be reached.
        let connected = model.connected();
        let rows = if connected {
            trouble_text::permission_rows(model)
        } else {
            Vec::new()
        };
        let clipboard = connected && trouble_text::clipboard_needs_remote_control(model);
        let key = format!("{rows:?}{clipboard}");
        if *self.shown_permissions.borrow() == key {
            return;
        }
        let Some(ui) = self.ui.upgrade() else { return };
        *self.shown_permissions.borrow_mut() = key;
        let mut shown = self.permission_rows.borrow_mut();
        super::clear_group(&self.permissions, &mut shown);
        for row in &rows {
            let widget = super::permission_row(&ui, row);
            self.permissions.add(&widget);
            shown.push(widget.upcast());
        }
        if clipboard {
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .title(t(keys::CLIPBOARD_NEEDS_REMOTE_CONTROL))
                .title_lines(0)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("dialog-information-symbolic"));
            self.permissions.add(&row);
            shown.push(row.upcast());
        }
        self.permissions.set_visible(!rows.is_empty() || clipboard);
    }

    /// What ufw keeps out, and the command that lets InputWeave in.
    fn update_firewall(&self, model: &Snapshot) {
        let hint = model
            .connected()
            .then(|| trouble_text::firewall(model.status.firewall.as_ref()))
            .flatten();
        let key = format!("{hint:?}");
        if *self.shown_firewall.borrow() == key {
            return;
        }
        let Some(ui) = self.ui.upgrade() else { return };
        *self.shown_firewall.borrow_mut() = key;
        let mut shown = self.firewall_rows.borrow_mut();
        super::clear_group(&self.firewall, &mut shown);
        if let Some(hint) = &hint {
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .title(tr(&hint.text))
                .title_lines(0)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
            self.firewall.add(&row);
            shown.push(row.upcast());
            let command = super::command_row(&ui, &t(keys::FIREWALL_COMMAND), &hint.command);
            self.firewall.add(&command);
            shown.push(command.upcast());
        }
        self.firewall.set_visible(hint.is_some());
    }

    /// The session and what serves each part of it, as the service says.
    fn update_session(&self, model: &Snapshot) {
        let session = model.status.session.as_ref().filter(|_| model.connected());
        let key = format!("{session:?}");
        if *self.shown_session.borrow() == key {
            return;
        }
        *self.shown_session.borrow_mut() = key;
        let mut shown = self.session_rows.borrow_mut();
        super::clear_group(&self.session, &mut shown);
        if let Some(session) = session {
            let row = property(&t(keys::TROUBLE_SESSION_TYPE));
            row.set_subtitle(&tr(&trouble_text::session_line(session)));
            self.session.add(&row);
            shown.push(row.upcast());
            for backend in &session.backends {
                let Some(role) = trouble_text::role_key(backend.role()) else {
                    continue;
                };
                let row = property(&t(role));
                row.set_subtitle(&tr(&trouble_text::backend_text(backend)));
                if !backend.problem.is_empty() {
                    row.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
                }
                self.session.add(&row);
                shown.push(row.upcast());
            }
        }
        self.session.set_visible(session.is_some());
    }
}

fn export_diagnostics(ui: &Rc<Ui>, window: Option<gtk::Window>) {
    let request = ui.agent.request(Command::GetDiagnostics(GetDiagnostics {}));
    let ui = ui.clone();
    gtk::glib::spawn_future_local(async move {
        let report = match request.await {
            Ok(Answer::Other(answer)) => match *answer {
                response::Result::Diagnostics(d) => d.report,
                _ => String::new(),
            },
            Ok(Answer::Applied) => String::new(),
            Err(e) => {
                log::info(&format!("no diagnostics: {e}"));
                ui.toast(&t(keys::DIAGNOSTICS_FAILED));
                return;
            }
        };
        let dialog = gtk::FileDialog::builder()
            .title(t(keys::TROUBLE_EXPORT))
            .initial_name(format!("{}.txt", t(keys::DIAGNOSTICS_FILE_NAME)))
            .modal(true)
            .build();
        let Ok(file) = dialog.save_future(window.as_ref()).await else {
            return;
        };
        let Some(path) = file.path() else { return };
        if let Err(e) = std::fs::write(&path, report) {
            log::info(&format!("cannot write the diagnostics: {e}"));
            ui.toast(&t(keys::DIAGNOSTICS_FAILED));
        }
    });
}
