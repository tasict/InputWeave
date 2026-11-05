//! First-run setup, as the other two apps have
//! it: welcome, this computer's name, the permissions the desktop asks for
//! (Wayland's portals, only while one is not granted), connecting another
//! computer (the Add Device dialog over this window), arranging them, and
//! trying it. The permissions step joins whenever there is something to
//! do there and the user has not gone past it (the agent may answer only
//! after the window opened), and stays once shown. "Done"
//! and "Later" both count setup as finished; closing the window does not,
//! so it opens again next time.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use inputweave_app_core::proto::{SetDeviceName, client_message::Command};
use inputweave_app_core::setup::{self, Step};
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;
use inputweave_app_core::{trouble_text, ui};

use crate::canvas::ArrangementCanvas;
use crate::pairing::AddDevice;
use crate::strings::{t, tr};
use crate::ui::Ui;

pub struct Setup {
    pub window: adw::Window,
    stack: gtk::Stack,
    steps: Vec<Step>,
    /// The permissions step was shown: it stays one of the steps.
    permissions_seen: Cell<bool>,
    permissions: adw::PreferencesGroup,
    permission_rows: RefCell<Vec<gtk::Widget>>,
    shown_permissions: RefCell<String>,
    current: Cell<Step>,
    progress: gtk::ProgressBar,
    step_of: gtk::Label,
    back: gtk::Button,
    later: gtk::Button,
    primary: gtk::Button,
    name: adw::EntryRow,
    name_error: gtk::Label,
    connected: gtk::Label,
    arrange_hint: gtk::Label,
    canvas: Rc<ArrangementCanvas>,
    try_hint: gtk::Label,
    /// The keyboard and mouse came back this many times when Try It
    /// opened; one more, or the cursor elsewhere, means it worked.
    returns_before: Cell<u64>,
    naming: Cell<bool>,
    ui: Weak<Ui>,
}

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["title-2"])
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build()
}

fn body(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .max_width_chars(48)
        .build()
}

fn page() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(24)
        .margin_bottom(12)
        .margin_start(36)
        .margin_end(36)
        .valign(gtk::Align::Center)
        .build()
}

impl Setup {
    pub fn new(ui: &Rc<Ui>) -> Rc<Setup> {
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::SlideLeftRight)
            .vexpand(true)
            .build();

        // Welcome.
        let welcome = page();
        // The app's icon once installed; a system one before.
        let installed = gtk::gdk::Display::default()
            .is_some_and(|d| gtk::IconTheme::for_display(&d).has_icon(inputweave_app_core::APP_ID));
        let icon = gtk::Image::builder()
            .icon_name(if installed {
                inputweave_app_core::APP_ID
            } else {
                "input-mouse-symbolic"
            })
            .pixel_size(96)
            .build();
        welcome.append(&icon);
        welcome.append(&heading(&t(keys::SETUP_WELCOME_TITLE)));
        let benefits = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .halign(gtk::Align::Center)
            .build();
        for (icon, key) in [
            ("input-mouse-symbolic", keys::SETUP_BENEFIT_EDGES),
            ("edit-paste-symbolic", keys::SETUP_BENEFIT_CLIPBOARD),
            ("document-send-symbolic", keys::SETUP_BENEFIT_FILES),
        ] {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.append(&gtk::Image::from_icon_name(icon));
            row.append(
                &gtk::Label::builder()
                    .label(t(key))
                    .wrap(true)
                    .xalign(0.0)
                    .build(),
            );
            benefits.append(&row);
        }
        welcome.append(&benefits);
        let reassurance = body(&t(keys::SETUP_REASSURANCE));
        reassurance.add_css_class("dim-label");
        welcome.append(&reassurance);
        stack.add_named(&welcome, Some("welcome"));

        // Name.
        let naming = page();
        naming.append(&heading(&t(keys::SETUP_NAME_TITLE)));
        let group = adw::PreferencesGroup::builder()
            .description(t(keys::SETUP_NAME_HINT))
            .build();
        let name = adw::EntryRow::builder()
            .use_markup(false)
            .title(t(keys::SETUP_NAME_LABEL))
            .build();
        group.add(&name);
        naming.append(&group);
        let name_error = gtk::Label::builder()
            .css_classes(["error"])
            .wrap(true)
            .visible(false)
            .build();
        naming.append(&name_error);
        stack.add_named(&naming, Some("name"));

        // Permissions.
        let asking = page();
        asking.append(&heading(&t(keys::SETUP_PERMISSIONS_TITLE)));
        asking.append(&body(&t(keys::PERMISSIONS_HINT)));
        let permissions = adw::PreferencesGroup::new();
        asking.append(&permissions);
        stack.add_named(&asking, Some("permissions"));

        // Connect.
        let connect = page();
        connect.append(&heading(&t(keys::SETUP_CONNECT_TITLE)));
        connect.append(&body(&t(keys::SETUP_CONNECT_HINT)));
        let add = gtk::Button::builder()
            .label(t(keys::SETUP_CONNECT_BUTTON))
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .build();
        connect.append(&add);
        let connected = gtk::Label::builder()
            .css_classes(["success"])
            .visible(false)
            .build();
        connect.append(&connected);
        stack.add_named(&connect, Some("connect"));

        // Arrange.
        let arrange = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(24)
            .margin_end(24)
            .build();
        arrange.append(&heading(&t(keys::SETUP_ARRANGE_TITLE)));
        let arrange_hint = body("");
        arrange.append(&arrange_hint);
        let canvas = ArrangementCanvas::new(ui);
        canvas.widget().set_height_request(240);
        arrange.append(canvas.widget());
        stack.add_named(&arrange, Some("arrange"));

        // Try it.
        let trying = page();
        trying.append(&heading(&t(keys::SETUP_TRY_TITLE)));
        let try_hint = body("");
        trying.append(&try_hint);
        stack.add_named(&trying, Some("try"));

        let progress = gtk::ProgressBar::builder().hexpand(true).build();
        progress.update_property(&[gtk::accessible::Property::Label(&t(
            keys::SETUP_WINDOW_TITLE,
        ))]);
        let step_of = gtk::Label::builder().css_classes(["dim-label"]).build();
        let back = gtk::Button::with_label(&t(keys::SETUP_BACK));
        let later = gtk::Button::with_label(&t(keys::SETUP_LATER));
        let primary = gtk::Button::builder()
            .css_classes(["suggested-action"])
            .build();
        let bar = gtk::Box::builder()
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(18)
            .margin_end(18)
            .build();
        let left = gtk::Box::new(gtk::Orientation::Vertical, 6);
        left.set_hexpand(true);
        left.append(&progress);
        left.append(&step_of);
        step_of.set_xalign(0.0);
        bar.append(&left);
        bar.append(&back);
        bar.append(&later);
        bar.append(&primary);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&stack);
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        content.append(&bar);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::builder().show_title(true).build());
        view.set_content(Some(&content));
        let window = adw::Window::builder()
            .application(&ui.app)
            .title(t(keys::SETUP_WINDOW_TITLE))
            .default_width(620)
            .default_height(520)
            .content(&view)
            .build();
        window.set_default_widget(Some(&primary));

        let setup = Rc::new(Setup {
            window,
            stack,
            steps: setup::steps(true),
            permissions_seen: Cell::new(false),
            permissions,
            permission_rows: RefCell::new(Vec::new()),
            shown_permissions: RefCell::new(String::new()),
            current: Cell::new(Step::Welcome),
            progress,
            step_of,
            back,
            later,
            primary,
            name,
            name_error,
            connected,
            arrange_hint,
            canvas,
            try_hint,
            returns_before: Cell::new(0),
            naming: Cell::new(false),
            ui: Rc::downgrade(ui),
        });
        setup.connect(&add);
        setup.show_step(Step::Welcome);
        setup
    }

    fn connect(self: &Rc<Self>, add: &gtk::Button) {
        let weak = Rc::downgrade(self);
        self.back.connect_clicked(move |_| {
            if let Some(setup) = weak.upgrade()
                && let Some(step) = setup.step_from(setup.current.get(), false)
            {
                setup.show_step(step);
            }
        });
        let weak = Rc::downgrade(self);
        self.later.connect_clicked(move |_| {
            if let Some(setup) = weak.upgrade() {
                setup.finish();
            }
        });
        let weak = Rc::downgrade(self);
        self.primary.connect_clicked(move |_| {
            if let Some(setup) = weak.upgrade() {
                setup.go_on();
            }
        });
        let weak = Rc::downgrade(self);
        self.name.connect_entry_activated(move |_| {
            if let Some(setup) = weak.upgrade() {
                setup.go_on();
            }
        });
        let weak = Rc::downgrade(self);
        add.connect_clicked(move |_| {
            if let Some(setup) = weak.upgrade()
                && let Some(ui) = setup.ui.upgrade()
            {
                AddDevice::present(&ui, &setup.window.clone().upcast());
            }
        });
        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            if let Some(setup) = weak.upgrade()
                && let Some(ui) = setup.ui.upgrade()
            {
                ui.setup_closed();
            }
            gtk::glib::Propagation::Proceed
        });
    }

    pub fn present(&self) {
        self.window.present();
    }

    /// "Done" and "Later": setup counts as finished, and the window goes.
    fn finish(&self) {
        if let Some(ui) = self.ui.upgrade() {
            ui.finished_setup();
        }
        self.window.close();
    }

    /// The primary button: on to the next step, after what this one does.
    fn go_on(self: &Rc<Self>) {
        let step = self.current.get();
        if step == Step::Name {
            self.apply_name();
            return;
        }
        match self.step_from(step, true) {
            Some(next) => self.show_step(next),
            None => self.finish(),
        }
    }

    /// The step on from `step` (`forward`) or back, passing over the
    /// permissions step when nothing there is left to do now: the steps may
    /// have been settled while the desktop restored a permission.
    fn step_from(&self, step: Step, forward: bool) -> Option<Step> {
        let permissions = self
            .ui
            .upgrade()
            .is_some_and(|ui| ui.snapshot().permission_to_ask());
        setup::go(
            &self.steps,
            step,
            forward,
            permissions,
            self.permissions_seen.get(),
        )
    }

    /// Names this computer, staying on the step if the service refuses.
    fn apply_name(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else { return };
        let name = self.name.text().trim().to_owned();
        if name.is_empty() || name == ui.snapshot().status.this_device_name {
            if let Some(next) = self.step_from(Step::Name, true) {
                self.show_step(next);
            }
            return;
        }
        if self.naming.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        let asked = name.clone();
        ui.command_then(
            Command::SetDeviceName(SetDeviceName { name }),
            move |result| {
                let Some(setup) = weak.upgrade() else {
                    return true;
                };
                setup.naming.set(false);
                match result {
                    Ok(_) => {
                        setup.name_error.set_visible(false);
                        // Only from where it was asked: the user may have
                        // gone back or on since.
                        let next = setup.step_from(Step::Name, true);
                        if setup.current.get() == Step::Name
                            && let Some(next) = next
                        {
                            setup.show_step(next);
                        }
                        true
                    }
                    Err(e) => {
                        let key = if e.code() == Some("invalid_name") {
                            keys::NAME_INVALID
                        } else {
                            keys::NAME_CHANGE_FAILED
                        };
                        // The name was not taken: back to it, wherever the
                        // user went meanwhile, with what they typed and why.
                        if setup.current.get() != Step::Name {
                            setup.show_step(Step::Name);
                            setup.name.set_text(&asked);
                        }
                        setup.name_error.set_label(&t(key));
                        setup.name_error.set_visible(true);
                        true
                    }
                }
            },
        );
    }

    fn show_step(&self, step: Step) {
        self.current.set(step);
        if step == Step::Permissions {
            self.permissions_seen.set(true);
        }
        let name = match step {
            Step::Welcome => "welcome",
            Step::Name => "name",
            Step::Connect => "connect",
            Step::Arrange => "arrange",
            Step::TryIt => "try",
            Step::Permissions => "permissions",
        };
        self.stack.set_visible_child_name(name);
        if let Some(ui) = self.ui.upgrade() {
            let model = ui.snapshot();
            if step == Step::Name {
                // Fresh: no word left from an earlier refusal.
                self.name.set_text(&model.status.this_device_name);
                self.name_error.set_visible(false);
            }
            if step == Step::TryIt {
                self.returns_before.set(model.status.control_returns);
            }
            self.update(&model);
        }
    }

    pub fn update(&self, model: &Arc<Snapshot>) {
        let step = self.current.get();
        self.update_permissions(model);
        // Counted as gone through: without the permissions step once
        // nothing is left to do there, as going on and back pass it over.
        let steps = &setup::shown(
            &self.steps,
            step,
            model.permission_to_ask(),
            self.permissions_seen.get(),
        );
        let number = setup::number(steps, step);
        self.step_of.set_label(&tr(&ui!(
            keys::SETUP_STEP_OF,
            number.to_string(),
            steps.len().to_string()
        )));
        self.progress
            .set_fraction(number as f64 / steps.len().max(1) as f64);
        self.back.set_visible(self.step_from(step, false).is_some());
        self.later.set_visible(step != Step::TryIt);
        let other = setup::other_name(model);
        let primary = match step {
            Step::Welcome => keys::SETUP_GET_STARTED,
            Step::Connect if other.is_none() => keys::SETUP_SKIP,
            Step::TryIt => keys::SETUP_DONE,
            _ => keys::SETUP_CONTINUE,
        };
        self.primary.set_label(&t(primary));
        match &other {
            Some(name) if !model.connected_devices().is_empty() => {
                self.connected
                    .set_label(&tr(&ui!(keys::SETUP_CONNECTED, name.clone())));
                self.connected.set_visible(true);
            }
            _ => self.connected.set_visible(false),
        }
        let arrange = match &other {
            Some(name) => tr(&ui!(keys::SETUP_ARRANGE_HINT, name.clone())),
            None => t(keys::SETUP_ARRANGE_ALONE),
        };
        self.arrange_hint.set_label(&arrange);
        let trying = if setup::worked(model, self.returns_before.get()) {
            tr(&ui!(keys::SETUP_TRY_DONE, model.take_back_shortcut()))
        } else {
            match &other {
                Some(name) => tr(&ui!(keys::SETUP_TRY_HINT, name.clone())),
                None => t(keys::SETUP_TRY_ALONE),
            }
        };
        self.try_hint.set_label(&trying);
        if step == Step::Arrange {
            self.canvas.update(model);
        }
    }

    /// A row for each permission the desktop asks for, rebuilt when one
    /// changes.
    fn update_permissions(&self, model: &Snapshot) {
        let rows = trouble_text::permission_rows(model);
        let key = format!("{rows:?}");
        if *self.shown_permissions.borrow() == key {
            return;
        }
        let Some(ui) = self.ui.upgrade() else { return };
        *self.shown_permissions.borrow_mut() = key;
        let mut shown = self.permission_rows.borrow_mut();
        crate::pages::clear_group(&self.permissions, &mut shown);
        for row in &rows {
            let widget = crate::pages::permission_row(&ui, row);
            self.permissions.add(&widget);
            shown.push(widget.upcast());
        }
    }

    /// Shows a step, for screenshots and tests.
    pub fn show_for_tests(&self, step: Step) {
        self.show_step(step);
    }
}
