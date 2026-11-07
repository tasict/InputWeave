//! The app while it runs: the agent's model, the tray icon, the windows,
//! and what follows from the model changing.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

use adw::prelude::*;
use gtk::{gio, glib};
use inputweave_app_core::agent::{Agent, AgentError, Answer};
use inputweave_app_core::hud::HudWatch;
use inputweave_app_core::news::TransferNews;
use inputweave_app_core::proto::{
    IdentifyScreens, SetLocked, SetSharing, WakeDevice, client_message::Command,
};
use inputweave_app_core::service::{
    self, OutdatedWatch, ServiceAction, ServiceNews, ServiceState, ServiceWatch,
};
use inputweave_app_core::settings::{self, Settings};
use inputweave_app_core::snapshot::Link;
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::status_text::{self, MenuState, TrayState};
use inputweave_app_core::text::keys;
use inputweave_app_core::{log, offline, paths, profiles, sample, wake};

use crate::overlay::Overlay;
use crate::setup::Setup;
use crate::strings::{t, tr};
use crate::systemd;
use crate::tray::{MenuItem, Tray, TrayAction, TrayModel};
use crate::window::{MainWindow, Page};
use crate::{Launch, notify, pairing, screenshot, send, tray_art};

/// How long a background launch waits for a panel to show its icon.
const PANEL_WAIT_SECONDS: u32 = 5;

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

pub fn current() -> Option<Rc<Ui>> {
    UI.with(|ui| ui.borrow().clone())
}

pub struct Ui {
    pub app: adw::Application,
    pub agent: Agent,
    window: RefCell<Option<Rc<MainWindow>>>,
    tray: RefCell<Option<Tray>>,
    service: Cell<ServiceState>,
    /// The unit's invocation as last read, to tell when a restart happened.
    invocation: RefCell<Vec<u8>>,
    news: RefCell<TransferNews>,
    watch: RefCell<ServiceWatch>,
    pub pairing: pairing::Pairing,
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    /// Screenshots: the sample model, and no notifications or tray.
    fixed: bool,
    /// The app's own settings (`inputweave_app_core::settings`).
    settings: RefCell<Settings>,
    setup: RefCell<Option<Rc<Setup>>>,
    hud: RefCell<HudWatch>,
    /// Made when first needed: the HUD, identifying screens, the cursor.
    overlay: RefCell<Option<Rc<Overlay>>>,
    /// Event.screens_identified and Event.cursor_located as last seen; the
    /// first state seen is only remembered.
    seen_identified: Cell<Option<u64>>,
    seen_located: Cell<Option<u64>>,
    /// The agent packaged beside the app, as last read.
    packaged: RefCell<Packaged>,
    outdated: RefCell<OutdatedWatch>,
    /// That this app is older than the service was logged.
    told_stale: Cell<bool>,
    /// The license's requests to Polar and the agent.
    pub license: crate::license::Control,
    /// The About dialogs open, kept up to date with the license.
    abouts: RefCell<Vec<Rc<crate::about::About>>>,
    /// The devices asked to wake, until the service answers: shared by
    /// every inspector and arrangement, which leave Wake off meanwhile.
    wakes: RefCell<wake::Pending>,
    /// The picker of where files go while it is open, kept up to date.
    send_picker: RefCell<Option<Rc<crate::send::Picker>>>,
    /// Waiting for the agent to be connected (`connected_within`), woken by
    /// the refresh that follows a change.
    connect_waiters: RefCell<Vec<async_channel::Sender<()>>>,
}

/// The SHA-256 of the agent packaged beside the app, read when first
/// needed and again when apt replaced the file (its time or size changed)
/// while the app runs.
#[derive(Default)]
struct Packaged {
    /// The file's modification time and size when `digest` was read.
    stamp: Option<(Option<SystemTime>, u64)>,
    /// `None` until read; empty when there is no packaged agent.
    digest: Option<String>,
    reading: bool,
}

/// The agent the package installs beside the app (`/usr/bin/inputweave`).
fn packaged_agent() -> Option<std::path::PathBuf> {
    let app = std::env::current_exe().ok()?;
    Some(app.parent()?.join("inputweave"))
}

/// The app was launched, or launched again (`remote`).
pub fn launched(app: &adw::Application, launch: Launch, remote: bool) {
    if let Some(dir) = launch.screenshots.clone() {
        if remote {
            return;
        }
        let ui = Ui::create(app, &launch, true);
        screenshot::run(ui, dir);
        return;
    }
    let first = current().is_none();
    let ui = match current() {
        Some(ui) => ui,
        None => Ui::create(app, &launch, false),
    };
    let page = launch.page.as_deref().and_then(Page::from_name);
    let starting = first && !remote;
    if let Some(paths) = launch.send {
        // From the file manager: only the choice of computer, without the
        // main window or setup. Started for it, the app stays in the
        // background afterwards, so once that is over, with no panel to
        // show its icon, the main window opens as after --background.
        let then = if starting {
            log::info("started to send files");
            let weak = Rc::downgrade(&ui);
            crate::send::Then::new(move || {
                if let Some(ui) = weak.upgrade() {
                    window_unless_tray(&ui, page);
                }
            })
        } else {
            crate::send::Then::none()
        };
        crate::send::from_file_manager(&ui, paths, then);
        return;
    }
    if starting {
        log::info(&format!(
            "started{}",
            if launch.background {
                " in the background"
            } else {
                ""
            }
        ));
    }
    // A plain launch, first or again, opens setup until it is finished and
    // the main window after; --setup opens setup whether finished or not.
    if launch.setup || (!launch.background && !ui.settings.borrow().finished_setup) {
        ui.open_setup();
    } else if !launch.background {
        ui.show_window(page);
    } else if starting {
        window_unless_tray(&ui, page);
    }
    if let Some(text) = launch.hud.clone() {
        ui.overlay().hud(&ui, &text);
    }
    if launch.identify {
        let model = ui.snapshot();
        ui.overlay().identify(&ui, &model, 20_000);
    }
    if launch.locate {
        ui.overlay().locate();
    }
    if let Some(edge) = launch.glow
        && let Some(crossed) = crate::overlay::crossing_for_tests(edge)
    {
        ui.overlay().glow(&crossed);
    }
}

/// Without a panel to show the tray icon, staying in the background would
/// leave nothing to find the app by: opens the main window unless the icon
/// shows within a moment (at login the panel may come up after the app).
fn window_unless_tray(ui: &Rc<Ui>, page: Option<Page>) {
    if ui.tray_shown() {
        return;
    }
    let weak = Rc::downgrade(ui);
    glib::timeout_add_seconds_local_once(PANEL_WAIT_SECONDS, move || {
        if let Some(ui) = weak.upgrade()
            && !ui.tray_shown()
        {
            log::info("no panel shows tray icons; opening the window");
            ui.show_window(page);
        }
    });
}

impl Ui {
    fn create(app: &adw::Application, launch: &Launch, fixed: bool) -> Rc<Ui> {
        let (changes, changed) = async_channel::bounded::<()>(1);
        let agent = if fixed {
            Agent::fixed(sample::snapshot(SystemTime::now()))
        } else {
            match launch.socket.clone().or_else(paths::agent_socket) {
                Some(socket) => {
                    Agent::start(socket, &inputweave_app_core::client_name(), move || {
                        let _ = changes.try_send(());
                    })
                }
                None => {
                    log::info("no runtime or home folder to find the agent's socket in");
                    Agent::fixed(Snapshot::default())
                }
            }
        };
        let ui = Rc::new(Ui {
            app: app.clone(),
            agent,
            window: RefCell::new(None),
            tray: RefCell::new(None),
            service: Cell::new(ServiceState::Unknown),
            invocation: RefCell::new(Vec::new()),
            news: RefCell::new(TransferNews::default()),
            watch: RefCell::new(ServiceWatch::default()),
            pairing: pairing::Pairing::default(),
            hold: RefCell::new(None),
            fixed,
            settings: RefCell::new(
                settings::path()
                    .filter(|_| !fixed)
                    .map(|p| Settings::load(&p))
                    .unwrap_or_default(),
            ),
            setup: RefCell::new(None),
            hud: RefCell::new(HudWatch::default()),
            overlay: RefCell::new(None),
            seen_identified: Cell::new(None),
            seen_located: Cell::new(None),
            packaged: RefCell::default(),
            outdated: RefCell::new(OutdatedWatch::default()),
            told_stale: Cell::new(false),
            license: crate::license::Control::default(),
            abouts: RefCell::default(),
            wakes: RefCell::default(),
            send_picker: RefCell::default(),
            connect_waiters: RefCell::default(),
        });
        UI.with(|cell| *cell.borrow_mut() = Some(ui.clone()));
        let about = gio::SimpleAction::new("about", None);
        let weak = Rc::downgrade(&ui);
        about.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.show_about();
            }
        });
        app.add_action(&about);
        if fixed {
            return ui;
        }
        ui.license.restore();
        *ui.hold.borrow_mut() = Some(app.hold());
        notify::install_actions(&ui);
        ui.start_tray();
        let refresh = ui.clone();
        glib::spawn_future_local(async move {
            while changed.recv().await.is_ok() {
                refresh.refresh();
            }
        });
        // Crossings light the edge at once, without the rest of a refresh.
        let (crossed, crossings) = async_channel::bounded::<()>(1);
        ui.agent.on_crossing(move || {
            let _ = crossed.try_send(());
        });
        let glowing = ui.clone();
        glib::spawn_future_local(async move {
            while crossings.recv().await.is_ok() {
                glowing.glow_crossings();
            }
        });
        // The packaged agent, now and every 30 s (apt may replace it).
        ui.check_packaged();
        let checking = Rc::downgrade(&ui);
        glib::timeout_add_seconds_local(30, move || match checking.upgrade() {
            Some(ui) => {
                ui.check_packaged();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        let (states, state_changes) = async_channel::unbounded();
        systemd::watch(states);
        let watching = ui.clone();
        glib::spawn_future_local(async move {
            while let Ok(reading) = state_changes.recv().await {
                watching.service.set(reading.state);
                *watching.invocation.borrow_mut() = reading.invocation;
                watching.check_packaged();
                watching.refresh();
            }
        });
        // Times in the text (countdowns, "2 min ago") move on by themselves.
        let ticking = Rc::downgrade(&ui);
        glib::timeout_add_seconds_local(1, move || match ticking.upgrade() {
            Some(ui) => {
                ui.refresh();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        ui
    }

    /// A model that shows the sample snapshot and connects to nothing.
    #[cfg(test)]
    pub fn for_tests(app: &adw::Application) -> Rc<Ui> {
        Ui::create(app, &Launch::default(), true)
    }

    /// What the agent last reported, shared rather than copied.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.agent.snapshot()
    }

    pub fn service(&self) -> ServiceState {
        self.service.get()
    }

    fn start_tray(self: &Rc<Self>) {
        let theme = paths::app_runtime_dir().and_then(|dir| {
            tray_art::write_theme(&dir.join("icons"))
                .map_err(|e| log::info(&format!("cannot write the tray icons: {e}")))
                .ok()
        });
        let (actions, clicked) = async_channel::unbounded();
        let tray = Tray::start(
            self.tray_model(&self.snapshot()),
            tray_art::pixmaps(),
            theme,
            actions,
        );
        *self.tray.borrow_mut() = tray;
        let ui = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(action) = clicked.recv().await {
                ui.tray_action(action);
            }
        });
    }

    /// What the tray shows for `model`.
    #[cfg(test)]
    pub fn tray_model_for_tests(&self, model: &Snapshot) -> TrayModel {
        self.tray_model(model)
    }

    /// Does what choosing `action` in the tray menu does.
    #[cfg(test)]
    pub fn tray_action_for_tests(self: &Rc<Self>, action: TrayAction) {
        self.tray_action(action);
    }

    fn tray_shown(&self) -> bool {
        self.tray.borrow().as_ref().is_some_and(Tray::shown)
    }

    fn tray_model(&self, model: &Snapshot) -> TrayModel {
        let now = SystemTime::now();
        let service = self.service();
        let state = TrayState::of(model, service, now);
        let menu = MenuState::of(model);
        let item =
            |key: &str, enabled: bool, action: TrayAction| MenuItem::new(t(key), enabled, action);
        let title = tr(&status_text::title(model, service, now));
        let mut menu_items = vec![
            MenuItem::new(title.clone(), false, TrayAction::Open),
            MenuItem::separator(),
            item(
                keys::MENU_SEND_FILES,
                menu.send_enabled,
                TrayAction::SendFiles,
            ),
            MenuItem::separator(),
        ];
        // The profile to use, in a section of its own before Pause Sharing,
        // as on Windows: Automatic and every profile, the one chosen
        // checked, then Manage Profiles….
        if !menu.profiles.is_empty() {
            let mut choices: Vec<MenuItem> = menu
                .profiles
                .iter()
                .map(|choice| MenuItem {
                    checked: Some(choice.checked),
                    ..MenuItem::new(
                        tr(&choice.label),
                        menu.profiles_enabled,
                        TrayAction::SelectProfile(choice.id.clone()),
                    )
                })
                .collect();
            choices.push(MenuItem::separator());
            choices.push(item(
                keys::MENU_MANAGE_PROFILES,
                true,
                TrayAction::ManageProfiles,
            ));
            menu_items.push(MenuItem::submenu(t(keys::MENU_PROFILE), choices));
            menu_items.push(MenuItem::separator());
        }
        menu_items.push(item(
            menu.pause_key,
            menu.pause_enabled,
            TrayAction::ToggleSharing,
        ));
        // Taking this computer offline for now, or bringing it back, after
        // Pause Sharing, as on Windows.
        if let Some((key, _)) = menu.offline {
            menu_items.push(item(key, menu.offline_enabled, TrayAction::ToggleOffline));
        }
        menu_items.extend([
            item(menu.lock_key, menu.lock_enabled, TrayAction::ToggleLock),
            item(
                keys::IDENTIFY_SCREENS,
                menu.identify_enabled,
                TrayAction::IdentifyScreens,
            ),
            MenuItem::separator(),
            item(keys::MENU_ADD_DEVICE, true, TrayAction::AddDevice),
            item(keys::MENU_SETTINGS, true, TrayAction::Settings),
            item(keys::MENU_RUN_SETUP, true, TrayAction::RunSetup),
            item(keys::MENU_ABOUT, true, TrayAction::About),
            MenuItem::separator(),
            item(keys::MENU_QUIT, true, TrayAction::Quit),
        ]);
        TrayModel {
            state,
            description: tr(&state.description_of(model)),
            detail: title,
            menu: menu_items,
        }
    }

    fn tray_action(self: &Rc<Self>, action: TrayAction) {
        let model = self.snapshot();
        match action {
            TrayAction::Open | TrayAction::Settings => self.show_window(None),
            TrayAction::SendFiles => send::choose_and_send(self, send::What::Files),
            TrayAction::ToggleSharing => self.command(Command::SetSharing(SetSharing {
                enabled: !model.status.sharing,
            })),
            TrayAction::ToggleLock => self.command(Command::SetLocked(SetLocked {
                locked: !model.status.locked,
            })),
            TrayAction::ToggleOffline => {
                if let Some((_, offline)) = offline::this_item(&model) {
                    let this = model.this_device_id().to_owned();
                    self.set_offline(&this, offline);
                }
            }
            TrayAction::SelectProfile(id) => self.command(profiles::select(&id)),
            TrayAction::ManageProfiles => self.show_window(Some(Page::Profiles)),
            TrayAction::IdentifyScreens => {
                self.command(Command::IdentifyScreens(IdentifyScreens {}))
            }
            TrayAction::AddDevice => self.add_device(),
            TrayAction::RunSetup => self.open_setup(),
            TrayAction::About => {
                self.show_about();
            }
            TrayAction::Quit => self.ask_to_quit(),
        }
    }

    /// Whether the license is on screen (the License page or About): the
    /// only time a licensed computer asks Polar again.
    pub fn license_shown(&self) -> bool {
        !self.abouts.borrow().is_empty()
            || self
                .window
                .borrow()
                .as_ref()
                .is_some_and(|w| w.showing() == Some(Page::License))
    }

    /// About InputWeave, over the main window: the version, the license
    /// ("Trial" with Buy a License…, or "Licensed") and the GPL.
    pub fn show_about(self: &Rc<Self>) -> adw::Dialog {
        let model = self.snapshot();
        let about = crate::about::About::new(self, self.license.licensed(&model));
        self.abouts.borrow_mut().push(about.clone());
        let weak = Rc::downgrade(self);
        let shown = Rc::as_ptr(&about);
        about.dialog.connect_closed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.abouts
                    .borrow_mut()
                    .retain(|a| !std::ptr::eq(Rc::as_ptr(a), shown));
            }
        });
        let window = self.window();
        if !window.shown() {
            window.present(&model, self.service());
        }
        about.dialog.present(Some(&window.widget()));
        if !self.fixed {
            crate::license::pump(self);
        }
        about.dialog.clone()
    }

    /// The main window, made when first needed.
    pub fn window(self: &Rc<Self>) -> Rc<MainWindow> {
        if let Some(window) = self.window.borrow().as_ref() {
            return window.clone();
        }
        let window = MainWindow::new(self);
        *self.window.borrow_mut() = Some(window.clone());
        window.update(&self.snapshot(), self.service());
        window
    }

    /// Whether the agent is connected now, or becomes so within `limit`, as
    /// the refresh after a change says.
    pub async fn connected_within(self: &Rc<Self>, limit: std::time::Duration) -> bool {
        if self.snapshot().connected() {
            return true;
        }
        let (waiter, woken) = async_channel::bounded::<()>(1);
        self.connect_waiters.borrow_mut().push(waiter);
        let _ = glib::future_with_timeout(limit, async move { woken.recv().await }).await;
        self.snapshot().connected()
    }

    /// The picker of where files go, while it is open.
    pub fn send_picker(&self) -> Option<Rc<crate::send::Picker>> {
        self.send_picker.borrow().clone()
    }

    /// Keeps `picker` up to date with the agent until it closes.
    pub fn keep_send_picker(self: &Rc<Self>, picker: &Rc<crate::send::Picker>) {
        *self.send_picker.borrow_mut() = Some(picker.clone());
        let (weak, closing) = (Rc::downgrade(self), Rc::downgrade(picker));
        picker.dialog.connect_closed(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            let mut kept = ui.send_picker.borrow_mut();
            if kept
                .as_ref()
                .is_some_and(|p| closing.upgrade().is_some_and(|c| Rc::ptr_eq(p, &c)))
            {
                kept.take();
            }
        });
    }

    /// The main window while it is shown, for a dialog to go over.
    pub fn shown_window(&self) -> Option<gtk::Widget> {
        self.window
            .borrow()
            .as_ref()
            .filter(|w| w.shown())
            .map(|w| w.widget())
    }

    /// The main window, shown and up to date.
    pub fn present_window(self: &Rc<Self>) -> Rc<MainWindow> {
        let window = self.window();
        window.present(&self.snapshot(), self.service());
        window
    }

    pub fn show_window(self: &Rc<Self>, page: Option<Page>) {
        let window = self.window();
        if let Some(page) = page {
            window.show_page(page);
        }
        window.present(&self.snapshot(), self.service());
    }

    /// First-run setup, made when first needed.
    pub fn open_setup(self: &Rc<Self>) {
        let existing = self.setup.borrow().clone();
        let setup = existing.unwrap_or_else(|| {
            let setup = Setup::new(self);
            *self.setup.borrow_mut() = Some(setup.clone());
            setup
        });
        setup.present();
    }

    pub fn setup(&self) -> Option<Rc<Setup>> {
        self.setup.borrow().clone()
    }

    /// "Done" or "Later": setup does not open by itself again.
    pub fn finished_setup(&self) {
        self.settings.borrow_mut().finished_setup = true;
        self.save_settings();
    }

    /// Whether the screen's edge lights up where the cursor crosses.
    pub fn edge_glow(&self) -> bool {
        self.settings.borrow().edge_glow
    }

    pub fn set_edge_glow(&self, on: bool) {
        if self.settings.borrow().edge_glow != on {
            self.settings.borrow_mut().edge_glow = on;
            self.save_settings();
        }
    }

    /// The setup window closed.
    pub fn setup_closed(&self) {
        self.setup.borrow_mut().take();
    }

    fn save_settings(&self) {
        if self.fixed {
            return;
        }
        if let Some(path) = settings::path()
            && let Err(e) = self.settings.borrow().save(&path)
        {
            log::info(&format!("cannot save the app's settings: {e}"));
        }
    }

    /// Whether the glow can show, as the overlays found when they were
    /// made; `None` before anything needed them (not made just to ask).
    pub fn glow_availability(&self) -> Option<inputweave_app_core::edge_glow::Availability> {
        self.overlay.borrow().as_ref().map(|o| o.glows())
    }

    /// The overlays, made when first needed.
    pub fn overlay(&self) -> Rc<Overlay> {
        self.overlay
            .borrow_mut()
            .get_or_insert_with(|| Rc::new(Overlay::new()))
            .clone()
    }

    pub fn add_device(self: &Rc<Self>) {
        let window = self.present_window();
        pairing::AddDevice::present(self, &window.widget());
    }

    fn ask_to_quit(self: &Rc<Self>) {
        let window = self.present_window();
        let dialog =
            adw::AlertDialog::new(Some(&t(keys::QUIT_TITLE)), Some(&t(keys::QUIT_MESSAGE)));
        dialog.add_response("cancel", &t(keys::CANCEL));
        dialog.add_response("quit", &t(keys::QUIT_CONFIRM));
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "quit" {
                ui.quit();
            }
        });
        dialog.present(Some(&window.widget()));
    }

    pub fn quit(&self) {
        log::info("quit by the user");
        self.agent.stop();
        self.hold.borrow_mut().take();
        self.app.quit();
    }

    /// Sends a command; a refusal shows as a toast.
    pub fn command(self: &Rc<Self>, command: Command) {
        self.command_then(command, |result| result.is_ok());
    }

    /// Sends a command and hands the result to `done`, which says whether
    /// it dealt with a failure; one it did not deal with shows as a toast.
    pub fn command_then(
        self: &Rc<Self>,
        command: Command,
        done: impl FnOnce(&Result<Answer, AgentError>) -> bool + 'static,
    ) {
        let request = self.agent.request(command);
        let ui = self.clone();
        glib::spawn_future_local(async move {
            let result = request.await;
            let handled = done(&result);
            if let Err(e) = &result {
                log::info(&format!("command failed: {e}"));
                if !handled {
                    ui.toast(&t(keys::COMMAND_FAILED));
                }
            }
            ui.refresh();
        });
    }

    /// Wakes another computer (WakeDevice), where Wake can be pressed now.
    /// What comes of it the service's status says (Waking…, the HUD), so a
    /// refusal is only logged.
    pub fn wake_device(self: &Rc<Self>, id: &str) {
        let model = self.snapshot();
        if wake::offer(&model, id, self.waking_asked(id)) != Some(true) {
            return;
        }
        self.wakes.borrow_mut().begin(id);
        let (weak, device) = (Rc::downgrade(self), id.to_owned());
        self.command_then(
            Command::WakeDevice(WakeDevice {
                device_id: id.to_owned(),
            }),
            move |result| {
                if let Err(e) = result {
                    log::info(&format!("waking {device} was refused: {e}"));
                }
                if let Some(ui) = weak.upgrade() {
                    ui.wakes.borrow_mut().end(&device);
                }
                true
            },
        );
        // Wake goes off at once.
        self.refresh();
    }

    /// Takes `id` offline for now (`offline`), or brings it back: another
    /// computer from the inspector or the arrangement's menu, this one from
    /// the tray menu or General. No question first: it is undone as easily,
    /// and the HUD says what happened; a refusal shows as a toast.
    pub fn set_offline(self: &Rc<Self>, id: &str, offline: bool) {
        self.command(offline::command(id, offline));
    }

    /// Whether this app asked the service to wake `id` and has no answer
    /// yet.
    pub fn waking_asked(&self, id: &str) -> bool {
        self.wakes.borrow().in_flight(id)
    }

    /// Tells the user something did not work: in the main window while it
    /// is shown, otherwise (from the tray menu or a notification) as a
    /// notification of its own, taken back after a while.
    pub fn toast(self: &Rc<Self>, text: &str) {
        let window = self.window.borrow().clone();
        match window {
            Some(window) if window.shown() => window.toast(text),
            _ if self.fixed => {}
            _ => notify::failure(self, text),
        }
    }

    /// Starts or restarts the background service through systemd.
    pub fn control_service(self: &Rc<Self>, what: ServiceAction) {
        if what == ServiceAction::Restart {
            self.watch
                .borrow_mut()
                .restarting(SystemTime::now(), &self.invocation.borrow());
        }
        let ui = self.clone();
        let (verb, failed) = match what {
            ServiceAction::Start => ("start", keys::SERVICE_START_FAILED),
            ServiceAction::Restart => ("restart", keys::SERVICE_RESTART_FAILED),
        };
        glib::spawn_future_local(async move {
            let result =
                gio::spawn_blocking(move || systemd::control(what).map_err(|e| e.to_string()))
                    .await;
            match result {
                Ok(Ok(before)) => ui.watch.borrow_mut().restarting_from(&before),
                Ok(Err(e)) => {
                    log::info(&format!("cannot {verb} the background service: {e}"));
                    ui.toast(&t(failed));
                }
                Err(_) => ui.toast(&t(failed)),
            }
        });
    }

    /// Everything that follows from the model or the service changing.
    pub fn refresh(self: &Rc<Self>) {
        let model = self.snapshot();
        let service = self.service();
        let now = SystemTime::now();
        self.wakes.borrow_mut().prune(&model);
        if let Some(window) = self.window.borrow().clone() {
            window.update(&model, service);
        }
        if let Some(tray) = self.tray.borrow().as_ref() {
            tray.update(self.tray_model(&model));
        }
        if let Some(setup) = self.setup.borrow().clone() {
            setup.update(&model);
        }
        let licensed = self.license.licensed(&model);
        for about in self.abouts.borrow().iter() {
            about.update(licensed);
        }
        let picker = self.send_picker.borrow().clone();
        if let Some(picker) = picker {
            picker.update(&model);
        }
        let mut waiters = self.connect_waiters.borrow_mut();
        if model.connected() {
            for waiter in waiters.drain(..) {
                let _ = waiter.try_send(());
            }
        } else {
            waiters.retain(|w| !w.is_closed());
        }
        drop(waiters);
        if self.fixed {
            return;
        }
        crate::license::pump(self);
        self.pairing.update(self, &model);
        self.overlays(&model, now);
        self.restart_if_outdated(&model, service);
        let transfers = self.news.borrow_mut().update(&model, now);
        if !transfers.is_empty() {
            notify::transfers(self, &model, &transfers);
        }
        let invocation = self.invocation.borrow().clone();
        match self
            .watch
            .borrow_mut()
            .update(service, &invocation, now, false)
        {
            ServiceNews::Raise => notify::service_stopped(self),
            ServiceNews::Withdraw => notify::service_running(self),
            ServiceNews::None => {}
        }
    }

    /// Reads the packaged agent's digest when first needed and again when
    /// its file changed (apt replaced it while the app runs), on a thread of
    /// its own: from a 30 s timer and when the service's state changes, not
    /// on every refresh.
    fn check_packaged(self: &Rc<Self>) {
        let path = packaged_agent();
        let stamp = path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| (m.modified().ok(), m.len()));
        let mut packaged = self.packaged.borrow_mut();
        if packaged.reading || (packaged.digest.is_some() && packaged.stamp == stamp) {
            return;
        }
        packaged.reading = true;
        let ui = self.clone();
        glib::spawn_future_local(async move {
            let read = gio::spawn_blocking(move || {
                path.filter(|p| p.is_file())
                    .and_then(|p| service::file_digest(&p).ok())
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_default();
            *ui.packaged.borrow_mut() = Packaged {
                stamp,
                digest: Some(read),
                reading: false,
            };
            ui.refresh();
        });
    }

    /// Restarts the background service once when it is not the one packaged
    /// with this app (`service::is_outdated`): apt replaces the files but
    /// restarts no user service. Judged only once the packaged agent's
    /// digest is known (`check_packaged`).
    fn restart_if_outdated(self: &Rc<Self>, model: &Snapshot, service_state: ServiceState) {
        if model.link == Link::Connecting {
            return;
        }
        let Some(digest) = self.packaged.borrow().digest.clone() else {
            return;
        };
        let outdated = service::is_outdated(
            model.link,
            &model.agent_version,
            env!("CARGO_PKG_VERSION"),
            &model.agent_digest,
            &digest,
            model.agent_ipc_version,
        );
        if model.app_is_older() && !self.told_stale.replace(true) {
            log::info(&format!(
                "the background service speaks IPC version {:?}, newer than this app's: \
                 the app is the old one; quit and open it again",
                model.agent_ipc_version
            ));
        }
        let restarted_for = self.settings.borrow().restarted_for.clone();
        if self
            .outdated
            .borrow_mut()
            .restart_now(service_state, outdated, &digest, &restarted_for)
        {
            log::info(&format!(
                "the background service is not the one packaged with the app (version {:?}, build {:?}); restarting it",
                model.agent_version, model.agent_digest
            ));
            // Not again for this packaged agent, should it change nothing.
            self.settings.borrow_mut().restarted_for = digest;
            self.save_settings();
            self.control_service(ServiceAction::Restart);
        }
    }

    /// Lights the edges crossed since last time, in order, each its own
    /// event; called as crossings come, apart from the refresh.
    fn glow_crossings(self: &Rc<Self>) {
        let crossings = self.agent.take_crossings();
        if self.edge_glow() && !crossings.is_empty() {
            let overlay = self.overlay();
            for crossed in &crossings {
                overlay.glow(crossed);
            }
        }
    }

    /// What the HUD says, the screens' names and where the cursor is, as
    /// the model changes.
    fn overlays(self: &Rc<Self>, model: &Arc<Snapshot>, now: SystemTime) {
        let hints_before = self.settings.borrow().hud_hints;
        let mut hints = hints_before;
        let note = self.hud.borrow_mut().update(model, &mut hints, now);
        if hints != hints_before {
            self.settings.borrow_mut().hud_hints = hints;
            self.save_settings();
        }
        if let Some(note) = note {
            self.overlay().hud(self, &tr(&note));
        }
        if model.connected() {
            let identified = self.seen_identified.replace(Some(model.screens_identified));
            if identified.is_some_and(|seen| seen != model.screens_identified) {
                self.overlay().identify(self, model, model.identify_ms);
            }
            let located = self.seen_located.replace(Some(model.cursor_located));
            if located.is_some_and(|seen| seen != model.cursor_located) {
                self.overlay().locate();
            }
        }
    }
}
