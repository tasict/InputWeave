//! The main window: a sidebar of five pages in the order the other two apps
//! have them, the License row pinned at its foot, and a cover over all of
//! them while the background service cannot be reached. Closing it only
//! hides it; the tray icon stays.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

use adw::prelude::*;
use inputweave_app_core::service::{ServiceAction, ServiceState};
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::status_text;
use inputweave_app_core::text::keys;

use gtk::gio;

use crate::pages::{self, Quiet};
use crate::strings::{t, tr};
use crate::ui::Ui;
use crate::woven::{Size as WovenSize, Woven};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    General,
    Devices,
    Arrangement,
    Files,
    Troubleshooting,
    /// Pinned at the sidebar's foot, apart from the others.
    License,
}

impl Page {
    /// In the sidebar's order, the License row last.
    pub const ALL: [Page; 6] = [
        Page::General,
        Page::Devices,
        Page::Arrangement,
        Page::Files,
        Page::Troubleshooting,
        Page::License,
    ];
    /// The pages in the sidebar's list (License has a row of its own).
    const LISTED: [Page; 5] = [
        Page::General,
        Page::Devices,
        Page::Arrangement,
        Page::Files,
        Page::Troubleshooting,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Page::General => "general",
            Page::Devices => "devices",
            Page::Arrangement => "arrangement",
            Page::Files => "files",
            Page::Troubleshooting => "troubleshooting",
            Page::License => "license",
        }
    }

    pub fn from_name(name: &str) -> Option<Page> {
        Page::ALL.into_iter().find(|p| p.name() == name)
    }

    fn title_key(self) -> &'static str {
        match self {
            Page::General => keys::NAV_GENERAL,
            Page::Devices => keys::NAV_DEVICES,
            Page::Arrangement => keys::NAV_ARRANGEMENT,
            Page::Files => keys::NAV_FILES,
            Page::Troubleshooting => keys::NAV_TROUBLESHOOTING,
            Page::License => keys::LICENSE_NAV,
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Page::General => "preferences-system-symbolic",
            Page::Devices => "computer-symbolic",
            Page::Arrangement => "view-grid-symbolic",
            Page::Files => "folder-download-symbolic",
            Page::Troubleshooting => "dialog-question-symbolic",
            Page::License => "dialog-password-symbolic",
        }
    }
}

pub struct MainWindow {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    sidebar: gtk::ListBox,
    /// The License row at the sidebar's foot, and what it says.
    foot: gtk::ListBox,
    foot_trial: gtk::Label,
    foot_licensed: Rc<Woven>,
    content: adw::NavigationPage,
    stack: gtk::Stack,
    unavailable: adw::StatusPage,
    start: gtk::Button,
    general: pages::general::General,
    devices: pages::devices::Devices,
    arrangement: Rc<pages::arrangement::Arrangement>,
    files: pages::files::Files,
    troubleshooting: pages::troubleshooting::Troubleshooting,
    license: Rc<pages::license::LicensePage>,
    current: Cell<Page>,
    /// The last model, to bring a page up to date when it is chosen.
    last: RefCell<Option<(Arc<Snapshot>, ServiceState)>>,
    quiet: Quiet,
    ui: std::rc::Weak<Ui>,
}

impl MainWindow {
    pub fn new(ui: &Rc<Ui>) -> Rc<MainWindow> {
        let window = adw::ApplicationWindow::builder()
            .application(&ui.app)
            .title("InputWeave")
            .icon_name(inputweave_app_core::APP_ID)
            .default_width(1100)
            .default_height(720)
            .width_request(760)
            .height_request(520)
            .hide_on_close(true)
            .build();
        let sidebar = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        for page in Page::LISTED {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(12)
                .margin_top(6)
                .margin_bottom(6)
                .margin_start(6)
                .margin_end(6)
                .build();
            row.append(&gtk::Image::from_icon_name(page.icon()));
            row.append(&gtk::Label::new(Some(&t(page.title_key()))));
            sidebar.append(&row);
        }
        let foot = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        let foot_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .build();
        foot_row.append(&gtk::Image::from_icon_name(Page::License.icon()));
        foot_row.append(
            &gtk::Label::builder()
                .label(t(Page::License.title_key()))
                .hexpand(true)
                .xalign(0.0)
                .build(),
        );
        let foot_trial = gtk::Label::builder()
            .label(t(keys::LICENSE_TRIAL))
            .css_classes(["iw-trial-pill"])
            .valign(gtk::Align::Center)
            .build();
        let foot_licensed = Rc::new(Woven::new(WovenSize::Small, &t(keys::LICENSE_LICENSED)));
        foot_licensed.widget().set_visible(false);
        foot_row.append(&foot_trial);
        foot_row.append(foot_licensed.widget());
        foot.append(&foot_row);
        let foot_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        foot_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        foot_box.append(&foot);
        let menu = gio::Menu::new();
        menu.append(Some(&t(keys::MENU_ABOUT)), Some("app.about"));
        let header = adw::HeaderBar::builder().show_title(true).build();
        header.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("open-menu-symbolic")
                .menu_model(&menu)
                .primary(true)
                .tooltip_text(t(keys::MENU_ABOUT))
                .build(),
        );
        let sidebar_view = adw::ToolbarView::new();
        sidebar_view.add_top_bar(&header);
        sidebar_view.add_bottom_bar(&foot_box);
        sidebar_view.set_content(Some(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&sidebar)
                .build(),
        ));
        let sidebar_page = adw::NavigationPage::new(&sidebar_view, "InputWeave");

        let general = pages::general::General::new(ui);
        let devices = pages::devices::Devices::new(ui);
        let arrangement = pages::arrangement::Arrangement::new(ui);
        let files = pages::files::Files::new(ui);
        let troubleshooting = pages::troubleshooting::Troubleshooting::new(ui);
        let license = pages::license::LicensePage::new(ui);
        let stack = gtk::Stack::new();
        stack.add_named(general.widget(), Some(Page::General.name()));
        stack.add_named(devices.widget(), Some(Page::Devices.name()));
        stack.add_named(arrangement.widget(), Some(Page::Arrangement.name()));
        stack.add_named(files.widget(), Some(Page::Files.name()));
        stack.add_named(troubleshooting.widget(), Some(Page::Troubleshooting.name()));
        stack.add_named(license.widget(), Some(Page::License.name()));

        let start = gtk::Button::builder()
            .label(t(keys::ACTION_START_SERVICE))
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        let unavailable = adw::StatusPage::builder()
            .icon_name("network-offline-symbolic")
            .description(t(keys::MAIN_UNAVAILABLE))
            .child(&start)
            .build();
        stack.add_named(&unavailable, Some("unavailable"));

        let content_view = adw::ToolbarView::new();
        content_view.add_top_bar(&adw::HeaderBar::new());
        content_view.set_content(Some(&stack));
        let content = adw::NavigationPage::new(&content_view, &t(keys::NAV_GENERAL));

        let split = adw::NavigationSplitView::builder()
            .sidebar(&sidebar_page)
            .content(&content)
            .min_sidebar_width(200.0)
            .build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&split));
        window.set_content(Some(&toasts));

        let main = Rc::new(MainWindow {
            window,
            toasts,
            sidebar,
            foot,
            foot_trial,
            foot_licensed,
            content,
            stack,
            unavailable,
            start,
            general,
            devices,
            arrangement,
            files,
            troubleshooting,
            license,
            current: Cell::new(Page::General),
            last: RefCell::new(None),
            quiet: Quiet::default(),
            ui: Rc::downgrade(ui),
        });
        let weak = Rc::downgrade(&main);
        main.sidebar.connect_row_selected(move |_, row| {
            if let (Some(main), Some(row)) = (weak.upgrade(), row)
                && let Some(page) = Page::LISTED.get(row.index() as usize)
                && !main.quiet.is_set()
            {
                main.quiet.run(|| main.foot.unselect_all());
                main.select(*page);
            }
        });
        let weak = Rc::downgrade(&main);
        main.foot.connect_row_selected(move |_, row| {
            if let (Some(main), Some(_)) = (weak.upgrade(), row)
                && !main.quiet.is_set()
            {
                main.quiet.run(|| main.sidebar.unselect_all());
                main.select(Page::License);
            }
        });
        let ui_weak = Rc::downgrade(ui);
        main.start.connect_clicked(move |_| {
            if let Some(ui) = ui_weak.upgrade()
                && let Some(action) = ServiceAction::for_state(ui.service())
            {
                ui.control_service(action);
            }
        });
        main.show_page(Page::General);
        main
    }

    pub fn widget(&self) -> gtk::Widget {
        self.window.clone().upcast()
    }

    pub fn window(&self) -> &adw::ApplicationWindow {
        &self.window
    }

    pub fn arrangement(&self) -> &Rc<pages::arrangement::Arrangement> {
        &self.arrangement
    }

    pub fn general(&self) -> &pages::general::General {
        &self.general
    }

    pub fn devices(&self) -> &pages::devices::Devices {
        &self.devices
    }

    pub fn troubleshooting(&self) -> &pages::troubleshooting::Troubleshooting {
        &self.troubleshooting
    }

    /// Shows the window, brought up to date first: while hidden it is not.
    pub fn present(&self, model: &Arc<Snapshot>, service: ServiceState) {
        // The autostart entry may have changed in the desktop's settings.
        self.general.reread_login();
        self.show(model, service);
        self.window.present();
    }

    /// Whether the window is on screen; hidden, it is not kept up to date.
    pub fn shown(&self) -> bool {
        self.window.is_visible()
    }

    pub fn toast(&self, text: &str) {
        // Plain text: names and the agent's words may hold & or <.
        let toast = adw::Toast::new(text);
        toast.set_use_markup(false);
        self.toasts.add_toast(toast);
    }

    pub fn show_page(&self, page: Page) {
        if page == Page::License {
            self.quiet.run(|| {
                self.sidebar.unselect_all();
                self.foot.select_row(self.foot.row_at_index(0).as_ref());
            });
        } else if let Some(row) = self
            .sidebar
            .row_at_index(Page::LISTED.iter().position(|p| *p == page).unwrap_or(0) as i32)
        {
            self.quiet.run(|| {
                self.foot.unselect_all();
                self.sidebar.select_row(Some(&row));
            });
        }
        self.select(page);
    }

    /// The page the window shows, while it is on screen.
    pub fn showing(&self) -> Option<Page> {
        self.shown().then(|| self.current.get())
    }

    pub fn license(&self) -> &Rc<pages::license::LicensePage> {
        &self.license
    }

    fn select(&self, page: Page) {
        self.current.set(page);
        self.content.set_title(&t(page.title_key()));
        if self.stack.visible_child_name().as_deref() != Some("unavailable") {
            self.stack.set_visible_child_name(page.name());
        }
        // Only the page shown is kept up to date: bring this one up now.
        let last = self.last.borrow().clone();
        if let Some((model, service)) = last {
            self.update_page(page, &model, service);
        }
    }

    /// Updates the window while it is shown; hidden, it only remembers the
    /// model for when it is shown again.
    pub fn update(&self, model: &Arc<Snapshot>, service: ServiceState) {
        *self.last.borrow_mut() = Some((model.clone(), service));
        // The arrangement's version is followed while hidden too.
        self.arrangement.follow(
            model,
            self.shown() && self.current.get() == Page::Arrangement,
        );
        if self.shown() {
            self.show(model, service);
        }
    }

    fn update_page(&self, page: Page, model: &Arc<Snapshot>, service: ServiceState) {
        match page {
            Page::General => self.general.update(model, service),
            Page::Devices => self.devices.update(model),
            Page::Arrangement => self.arrangement.update(model),
            Page::Files => self.files.update(model),
            Page::Troubleshooting => self.troubleshooting.update(model, service),
            Page::License => self.license.update(model),
        }
    }

    /// Shows `model`: the cover, or the page shown.
    fn show(&self, model: &Arc<Snapshot>, service: ServiceState) {
        *self.last.borrow_mut() = Some((model.clone(), service));
        let licensed = self
            .ui
            .upgrade()
            .is_some_and(|ui| ui.license.licensed(model));
        self.foot_trial.set_visible(!licensed);
        self.foot_licensed.widget().set_visible(licensed);
        self.license.follow_dialog(model);
        let now = SystemTime::now();
        if model.connected() {
            if self.stack.visible_child_name().as_deref() != Some(self.current.get().name()) {
                self.stack.set_visible_child_name(self.current.get().name());
            }
        } else {
            let title = tr(&status_text::title(model, service, now));
            if self.unavailable.title() != title {
                self.unavailable.set_title(&title);
            }
            let action = ServiceAction::for_state(service);
            if let Some(action) = action {
                self.start.set_label(&t(action.key()));
            }
            self.start.set_visible(action.is_some());
            let description = if service == ServiceState::NotInstalled {
                t(keys::SERVICE_NOT_INSTALLED)
            } else if model.app_is_older() {
                t(keys::APP_OLDER_HINT)
            } else {
                t(keys::MAIN_UNAVAILABLE)
            };
            if self.unavailable.description().as_deref() != Some(description.as_str()) {
                self.unavailable.set_description(Some(&description));
            }
            if self.stack.visible_child_name().as_deref() != Some("unavailable") {
                self.stack.set_visible_child_name("unavailable");
            }
            return;
        }
        self.update_page(self.current.get(), model, service);
    }
}
