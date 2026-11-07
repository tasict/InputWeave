//! Profiles: the profile in use (Automatic, or one the user chose), the
//! network this computer is on with the profiles it can be added to, and
//! every profile, each opening a page of its own over the window's pages
//! ([`Editor`]): its name, whether it shares, the computers it connects,
//! the clipboard and files, its networks, and removing it. The service
//! keeps the profiles and picks the one in use; this shows them and sends
//! what the user changes. On Linux the service reads Wi-Fi names itself,
//! so there is no switch for them.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};
use inputweave_app_core::profiles::{self, Target};
use inputweave_app_core::proto::{Profile, ProfileKind, client_message::Command};
use inputweave_app_core::snapshot::Snapshot;
use inputweave_app_core::text::keys;

use super::{EntrySync, Quiet, set_subtitle, set_switch, set_title};
use crate::strings::{t, tr};
use crate::ui::Ui;

/// The menu of profiles the network this computer is on can be added to,
/// each item activating `action` with the profile's id.
pub fn network_menu(targets: &[Target], action: &str) -> gio::Menu {
    let menu = gio::Menu::new();
    for target in targets {
        let item = gio::MenuItem::new(Some(&tr(&target.name)), None);
        item.set_action_and_target_value(Some(action), Some(&target.id.to_variant()));
        menu.append_item(&item);
    }
    menu
}

/// What a menu of `network_menu` lists, to make it again only when that
/// changes.
pub fn menu_key(targets: &[Target]) -> String {
    targets
        .iter()
        .map(|t| format!("{}\u{1f}{}", t.id, tr(&t.name)))
        .collect::<Vec<_>>()
        .join("\u{1e}")
}

/// An action adding the network this computer is on to the profile whose
/// id it is given (AddNetworkToProfile). `checked`, its state is the id of
/// the profile that has the network, so the menu marks it.
pub fn add_network_action(ui: &Rc<Ui>, checked: bool) -> gio::SimpleAction {
    let action = if checked {
        gio::SimpleAction::new_stateful(
            "add-network",
            Some(glib::VariantTy::STRING),
            &String::new().to_variant(),
        )
    } else {
        gio::SimpleAction::new("add-network", Some(glib::VariantTy::STRING))
    };
    let weak = Rc::downgrade(ui);
    // Activating sends the command, while the network can be added; the
    // state follows the service.
    action.connect_activate(move |_, id| {
        if let (Some(ui), Some(id)) = (weak.upgrade(), id.and_then(|v| v.str().map(str::to_owned)))
            && profiles::can_add_network(&ui.snapshot())
        {
            ui.command(profiles::add_network(&id));
        }
    });
    action
}

/// One profile in the list: its name, "In use", and the arrow.
struct ProfileRow {
    id: String,
    row: adw::ActionRow,
    in_use: gtk::Label,
}

pub struct Profiles {
    page: adw::PreferencesPage,
    quiet: Quiet,
    in_use: adw::ComboRow,
    /// The combo row's choices as shown: each one's id (empty for
    /// Automatic) and label.
    choices: RefCell<Vec<(String, String)>>,
    network: adw::ActionRow,
    network_icon: gtk::Image,
    add_to: gtk::MenuButton,
    add_to_shown: RefCell<String>,
    add_network: gio::SimpleAction,
    list: adw::PreferencesGroup,
    rows: RefCell<Vec<ProfileRow>>,
    add: gtk::Button,
    /// Where a profile's page opens: the window's.
    navigation: RefCell<Option<adw::NavigationView>>,
    editor: RefCell<Option<Rc<Editor>>>,
    ui: Weak<Ui>,
}

impl Profiles {
    pub fn new(ui: &Rc<Ui>) -> Rc<Profiles> {
        let page = adw::PreferencesPage::new();
        let top = adw::PreferencesGroup::new();
        let in_use = adw::ComboRow::builder()
            .use_markup(false)
            .title(t(keys::PROFILE_IN_USE))
            .subtitle(t(keys::PROFILE_IN_USE_HINT))
            .model(&gtk::StringList::new(&[]))
            .build();
        let network = super::plain_row();
        let network_icon = gtk::Image::builder()
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        network.add_prefix(&network_icon);
        let add_to = gtk::MenuButton::builder()
            .label(t(keys::PROFILE_ADD_TO))
            .always_show_arrow(true)
            .valign(gtk::Align::Center)
            .build();
        network.add_suffix(&add_to);
        top.add(&in_use);
        top.add(&network);
        page.add(&top);

        let list = adw::PreferencesGroup::builder()
            .title(t(keys::NAV_PROFILES))
            .build();
        let add = gtk::Button::builder()
            .label(t(keys::PROFILE_ADD))
            .halign(gtk::Align::Start)
            .margin_top(12)
            .build();
        list.add(&add);
        page.add(&list);

        let actions = gio::SimpleActionGroup::new();
        let add_network = add_network_action(ui, true);
        actions.add_action(&add_network);
        page.insert_action_group("profiles", Some(&actions));

        let profiles = Rc::new(Profiles {
            page,
            quiet: Quiet::default(),
            in_use,
            choices: RefCell::default(),
            network,
            network_icon,
            add_to,
            add_to_shown: RefCell::default(),
            add_network,
            list,
            rows: RefCell::default(),
            add,
            navigation: RefCell::default(),
            editor: RefCell::default(),
            ui: Rc::downgrade(ui),
        });
        profiles.connect();
        profiles
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.in_use.connect_selected_notify(move |row| {
            let Some(page) = weak.upgrade() else { return };
            if page.quiet.is_set() {
                return;
            }
            let id = page
                .choices
                .borrow()
                .get(row.selected() as usize)
                .map(|(id, _)| id.clone());
            if let (Some(ui), Some(id)) = (page.ui.upgrade(), id) {
                ui.command(profiles::select(&id));
            }
        });
        let weak = Rc::downgrade(self);
        self.add.connect_clicked(move |_| {
            if let Some(page) = weak.upgrade() {
                page.add_profile();
            }
        });
    }

    pub fn widget(&self) -> &adw::PreferencesPage {
        &self.page
    }

    /// Where a profile's page opens; when a page is closed (its back
    /// button, or the window going back to its pages) it is forgotten.
    pub fn set_navigation(self: &Rc<Self>, navigation: &adw::NavigationView) {
        let weak = Rc::downgrade(self);
        navigation.connect_popped(move |_, popped| {
            if let Some(page) = weak.upgrade() {
                let mut editor = page.editor.borrow_mut();
                if editor.as_ref().is_some_and(|e| e.page == *popped) {
                    editor.take();
                }
            }
        });
        *self.navigation.borrow_mut() = Some(navigation.clone());
    }

    /// The page of the profile open now.
    pub fn editor(&self) -> Option<Rc<Editor>> {
        self.editor.borrow().clone()
    }

    /// Adds a profile, named "New Profile" (the service makes the name
    /// unique), and once the service has it, opens it with its name ready
    /// to be typed over.
    fn add_profile(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else { return };
        let before = profiles::ids(&ui.snapshot());
        let weak = Rc::downgrade(self);
        ui.command_then(profiles::add(&t(keys::PROFILE_NEW_NAME)), move |result| {
            if result.is_ok()
                && let Some(page) = weak.upgrade()
                && let Some(ui) = page.ui.upgrade()
            {
                let model = ui.snapshot();
                if let Some(id) = profiles::added(&before, &model)
                    && let Some(editor) = page.open(&model, &id)
                {
                    editor.focus_name();
                }
            }
            result.is_ok()
        });
    }

    /// Opens the page of profile `id`, in place of any open.
    pub fn open(self: &Rc<Self>, model: &Snapshot, id: &str) -> Option<Rc<Editor>> {
        let ui = self.ui.upgrade()?;
        let navigation = self.navigation.borrow().clone()?;
        profiles::profile(model, id)?;
        // Taken first: closing it tells `set_navigation`'s handler.
        let open = self.editor.borrow_mut().take();
        if let Some(open) = open {
            navigation.pop_to_page(&open.page);
            navigation.pop();
        }
        let editor = Editor::new(&ui, id);
        editor.update(model);
        *self.editor.borrow_mut() = Some(editor.clone());
        navigation.push(&editor.page);
        Some(editor)
    }

    pub fn update(self: &Rc<Self>, model: &Snapshot) {
        self.update_in_use(model);
        self.update_network(model);
        self.update_list(model);
        let editor = self.editor.borrow().clone();
        if let Some(editor) = editor
            && !editor.update(model)
            && let Some(navigation) = self.navigation.borrow().clone()
        {
            // The profile is gone (removed, here or by another app).
            navigation.pop_to_page(&editor.page);
            navigation.pop();
        }
    }

    fn update_in_use(&self, model: &Snapshot) {
        let all = profiles::choices(model);
        let selected = all.iter().position(|c| c.checked).unwrap_or(0) as u32;
        let choices: Vec<(String, String)> =
            all.iter().map(|c| (c.id.clone(), tr(&c.label))).collect();
        if *self.choices.borrow() != choices {
            let labels: Vec<&str> = choices.iter().map(|(_, l)| l.as_str()).collect();
            let list = gtk::StringList::new(&labels);
            *self.choices.borrow_mut() = choices;
            self.quiet.run(|| self.in_use.set_model(Some(&list)));
        }
        if self.in_use.selected() != selected {
            self.quiet.run(|| self.in_use.set_selected(selected));
        }
        self.in_use.set_sensitive(model.connected());
    }

    fn update_network(&self, model: &Snapshot) {
        set_title(&self.network, &tr(&profiles::current_network(model)));
        let network = model.status.network.as_ref();
        set_subtitle(
            &self.network,
            &network
                .and_then(profiles::network_detail)
                .map(|d| tr(&d))
                .unwrap_or_default(),
        );
        match network {
            Some(network) => {
                self.network_icon
                    .set_icon_name(Some(profiles::network_icon(network)));
                self.network_icon.set_visible(true);
            }
            None => self.network_icon.set_visible(false),
        }
        let targets = profiles::network_targets(model);
        let key = menu_key(&targets);
        if *self.add_to_shown.borrow() != key {
            self.add_to
                .set_menu_model(Some(&network_menu(&targets, "profiles.add-network")));
            *self.add_to_shown.borrow_mut() = key;
        }
        let holder = targets
            .iter()
            .find(|t| t.has_network)
            .map(|t| t.id.clone())
            .unwrap_or_default();
        if self.add_network.state().and_then(|s| s.get::<String>()) != Some(holder.clone()) {
            self.add_network.set_state(&holder.to_variant());
        }
        // Off, as its items, until the service knows the network by more
        // than its router's address.
        let can = model.connected() && profiles::can_add_network(model);
        self.add_network.set_enabled(can);
        self.add_to.set_sensitive(can && !targets.is_empty());
    }

    fn update_list(self: &Rc<Self>, model: &Snapshot) {
        let ids = profiles::ids(model);
        let same = {
            let rows = self.rows.borrow();
            rows.len() == ids.len() && rows.iter().zip(&ids).all(|(r, id)| r.id == *id)
        };
        if !same {
            let mut rows = self.rows.borrow_mut();
            for row in rows.drain(..) {
                self.list.remove(&row.row);
            }
            for id in &ids {
                let row = adw::ActionRow::builder()
                    .use_markup(false)
                    .activatable(true)
                    .build();
                let in_use = gtk::Label::builder()
                    .label(t(keys::PROFILE_IN_USE_TAG))
                    .css_classes(["dim-label"])
                    .build();
                row.add_suffix(&in_use);
                row.add_suffix(
                    &gtk::Image::builder()
                        .icon_name("go-next-symbolic")
                        .accessible_role(gtk::AccessibleRole::Presentation)
                        .build(),
                );
                let (weak, profile) = (Rc::downgrade(self), id.clone());
                row.connect_activated(move |_| {
                    if let Some(page) = weak.upgrade()
                        && let Some(ui) = page.ui.upgrade()
                    {
                        page.open(&ui.snapshot(), &profile);
                    }
                });
                // Rows go before the button below them.
                self.list.add(&row);
                rows.push(ProfileRow {
                    id: id.clone(),
                    row,
                    in_use,
                });
            }
        }
        let using = profiles::in_use(model).map(|p| p.id.clone());
        for row in self.rows.borrow().iter() {
            if let Some(profile) = profiles::profile(model, &row.id) {
                set_title(&row.row, &tr(&profiles::name(profile)));
            }
            row.in_use
                .set_visible(using.as_deref() == Some(row.id.as_str()));
        }
        self.add.set_sensitive(model.connected());
    }

    /// The rows of the list, by id, with whether each says "In use".
    #[cfg(test)]
    pub fn listed(&self) -> Vec<(String, String, bool)> {
        self.rows
            .borrow()
            .iter()
            .map(|r| {
                (
                    r.id.clone(),
                    r.row.title().to_string(),
                    r.in_use.is_visible(),
                )
            })
            .collect()
    }

    #[cfg(test)]
    pub fn row(&self, id: &str) -> Option<adw::ActionRow> {
        self.rows
            .borrow()
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.row.clone())
    }

    #[cfg(test)]
    pub fn in_use_row(&self) -> &adw::ComboRow {
        &self.in_use
    }

    #[cfg(test)]
    pub fn network_row(&self) -> &adw::ActionRow {
        &self.network
    }

    #[cfg(test)]
    pub fn add_to_button(&self) -> &gtk::MenuButton {
        &self.add_to
    }

    #[cfg(test)]
    pub fn add_button(&self) -> &gtk::Button {
        &self.add
    }

    /// The profile "Add to Profile" marks, as its action's state says.
    #[cfg(test)]
    pub fn add_to_checked(&self) -> Option<String> {
        self.add_network.state().and_then(|s| s.get::<String>())
    }
}

/// One computer of "Computers to connect".
struct ComputerRow {
    id: String,
    row: adw::ActionRow,
    check: gtk::CheckButton,
}

/// A profile's page: its name (only the user's own can be renamed),
/// sharing or paused, the computers it connects and whether the clipboard
/// and files go (both off while it is paused), its networks (Other
/// networks lists none), and removing it (only the user's own). Its rows
/// are kept and updated in place while the same computers and networks
/// are listed.
pub struct Editor {
    id: String,
    pub page: adw::NavigationPage,
    quiet: Quiet,
    name: EntrySync,
    /// Why the service refused the last new name, under the name until it
    /// is edited again.
    name_refused: gtk::Label,
    share: gtk::ToggleButton,
    pause: gtk::ToggleButton,
    sharing: adw::ActionRow,
    computers: adw::PreferencesGroup,
    computer_rows: RefCell<Vec<ComputerRow>>,
    options: adw::PreferencesGroup,
    clipboard: adw::SwitchRow,
    files: adw::SwitchRow,
    networks: adw::PreferencesGroup,
    network_rows: RefCell<Vec<gtk::Widget>>,
    networks_shown: RefCell<String>,
    add_current: gtk::Button,
    remove: gtk::Button,
    ui: Weak<Ui>,
}

impl Editor {
    fn new(ui: &Rc<Ui>, id: &str) -> Rc<Editor> {
        let prefs = adw::PreferencesPage::new();

        let names = adw::PreferencesGroup::new();
        let name = adw::EntryRow::builder()
            .use_markup(false)
            .title(t(keys::INSPECTOR_NAME))
            .show_apply_button(true)
            .build();
        names.add(&name);
        // Not a row: under the name, while a new name was refused.
        let name_refused = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .margin_top(6)
            .margin_start(12)
            .margin_end(12)
            .css_classes(["error", "caption"])
            .visible(false)
            .build();
        names.add(&name_refused);
        prefs.add(&names);

        let sharing_group = adw::PreferencesGroup::new();
        let sharing = super::plain_row();
        sharing.set_title(&t(keys::PROFILE_SHARING));
        let share = gtk::ToggleButton::with_label(&t(keys::PROFILE_SHARE));
        let pause = gtk::ToggleButton::with_label(&t(keys::PROFILE_PAUSE));
        pause.set_group(Some(&share));
        let segments = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(["linked"])
            .valign(gtk::Align::Center)
            .build();
        segments.append(&share);
        segments.append(&pause);
        sharing.add_suffix(&segments);
        sharing_group.add(&sharing);
        prefs.add(&sharing_group);

        let computers = adw::PreferencesGroup::builder()
            .title(t(keys::PROFILE_COMPUTERS))
            .description(t(keys::PROFILE_COMPUTERS_HINT))
            .build();
        prefs.add(&computers);

        let options = adw::PreferencesGroup::builder()
            .description(t(keys::PROFILE_CLIPBOARD_HINT))
            .build();
        let clipboard = adw::SwitchRow::builder()
            .use_markup(false)
            .title(t(keys::GENERAL_CLIPBOARD_SYNC))
            .build();
        let files = adw::SwitchRow::builder()
            .use_markup(false)
            .title(t(keys::PROFILE_FILES))
            .build();
        options.add(&clipboard);
        options.add(&files);
        prefs.add(&options);

        let networks = adw::PreferencesGroup::builder()
            .title(t(keys::PROFILE_NETWORKS))
            .build();
        let add_current = gtk::Button::builder()
            .label(t(keys::PROFILE_ADD_CURRENT_NETWORK))
            .halign(gtk::Align::Start)
            .margin_top(12)
            .build();
        networks.add(&add_current);
        prefs.add(&networks);

        let removal = adw::PreferencesGroup::new();
        let remove = gtk::Button::builder()
            .label(t(keys::PROFILE_REMOVE))
            .halign(gtk::Align::Start)
            .css_classes(["destructive-action"])
            .build();
        removal.add(&remove);
        prefs.add(&removal);

        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&prefs));
        let page = adw::NavigationPage::builder()
            .child(&view)
            .title(t(keys::NAV_PROFILES))
            .build();

        let editor = Rc::new(Editor {
            id: id.to_owned(),
            page,
            quiet: Quiet::default(),
            name: EntrySync::new(&name),
            name_refused,
            share,
            pause,
            sharing,
            computers,
            computer_rows: RefCell::default(),
            options,
            clipboard,
            files,
            networks,
            network_rows: RefCell::default(),
            networks_shown: RefCell::default(),
            add_current,
            remove,
            ui: Rc::downgrade(ui),
        });
        editor.connect();
        editor
    }

    /// Sends `command` for this profile, from a control the user changed.
    fn send(&self, command: Command) {
        if let Some(ui) = self.ui.upgrade() {
            ui.command(command);
        }
    }

    fn connect(self: &Rc<Self>) {
        let (weak, edits) = (Rc::downgrade(self), self.name.edits());
        self.name.row().connect_apply(move |entry| {
            let Some(editor) = weak.upgrade() else { return };
            edits.applied();
            let name = entry.text().trim().to_owned();
            // Emptied: the name it has comes back, and nothing is sent.
            if name.is_empty() {
                editor.reset_name();
                return;
            }
            let Some(ui) = editor.ui.upgrade() else {
                return;
            };
            let weak = Rc::downgrade(&editor);
            ui.command_then(profiles::rename(&editor.id, &name), move |result| {
                let Err(e) = result else { return true };
                let Some(why) = profiles::rename_refused(e.code()) else {
                    return false;
                };
                // The name it has comes back, and why it stays is said
                // under it, until the name is edited again.
                if let Some(editor) = weak.upgrade() {
                    editor.reset_name();
                    editor.show_name_refused(&tr(&why));
                }
                true
            });
        });
        // Editing again takes the refusal back.
        let weak = Rc::downgrade(self);
        self.name.row().connect_changed(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.show_name_refused("");
            }
        });
        for (button, paused) in [(&self.share, false), (&self.pause, true)] {
            let weak = Rc::downgrade(self);
            button.connect_toggled(move |button| {
                if let Some(editor) = weak.upgrade()
                    && !editor.quiet.is_set()
                    && button.is_active()
                {
                    editor.send(profiles::set_paused(&editor.id, paused));
                }
            });
        }
        let weak = Rc::downgrade(self);
        self.clipboard.connect_active_notify(move |row| {
            if let Some(editor) = weak.upgrade()
                && !editor.quiet.is_set()
            {
                editor.send(profiles::set_clipboard(&editor.id, row.is_active()));
            }
        });
        let weak = Rc::downgrade(self);
        self.files.connect_active_notify(move |row| {
            if let Some(editor) = weak.upgrade()
                && !editor.quiet.is_set()
            {
                editor.send(profiles::set_files(&editor.id, row.is_active()));
            }
        });
        let weak = Rc::downgrade(self);
        self.add_current.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.send(profiles::add_network(&editor.id));
            }
        });
        // No question first: the page closes once the service's answer no
        // longer lists the profile (`Profiles::update`).
        let weak = Rc::downgrade(self);
        self.remove.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.send(profiles::remove(&editor.id));
            }
        });
    }

    /// Shows the name the profile has, dropping what was typed.
    fn reset_name(&self) {
        if let Some(ui) = self.ui.upgrade()
            && let Some(profile) = profiles::profile(&ui.snapshot(), &self.id)
        {
            self.name.reset(&tr(&profiles::name(profile)));
        }
    }

    /// Says under the name why the service refused it, in the entry's
    /// error style; nothing (`""`) takes it back.
    fn show_name_refused(&self, text: &str) {
        let row = self.name.row();
        if text.is_empty() {
            row.remove_css_class("error");
            self.name_refused.set_visible(false);
            return;
        }
        super::set_text(&self.name_refused, text);
        self.name_refused.set_visible(true);
        row.add_css_class("error");
    }

    /// Puts the focus in the name, its text selected to be typed over.
    pub fn focus_name(&self) {
        let row = self.name.row().clone();
        row.grab_focus();
        // Once the page is shown, should pushing it move the focus.
        glib::idle_add_local_once(move || {
            if !row.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN) {
                row.grab_focus();
            }
        });
    }

    /// Shows the profile as `model` has it; false once it has it no more.
    fn update(&self, model: &Snapshot) -> bool {
        let Some(profile) = profiles::profile(model, &self.id) else {
            return false;
        };
        let name = tr(&profiles::name(profile));
        if self.page.title() != name {
            self.page.set_title(&name);
        }
        let fixed = profiles::is_fixed(profile);
        self.name.row().set_sensitive(!fixed);
        self.name.show(&name);
        self.remove.set_sensitive(!fixed && model.connected());

        if self.pause.is_active() != profile.paused || self.share.is_active() == profile.paused {
            self.quiet.run(|| {
                if profile.paused {
                    self.pause.set_active(true);
                } else {
                    self.share.set_active(true);
                }
            });
        }
        set_subtitle(
            &self.sharing,
            &if profile.paused {
                t(keys::PROFILE_PAUSED_HINT)
            } else {
                String::new()
            },
        );
        // Paused, nothing connects: what goes where waits until it shares.
        self.computers.set_sensitive(!profile.paused);
        self.options.set_sensitive(!profile.paused);
        self.update_computers(model, profile);
        set_switch(&self.quiet, &self.clipboard, profile.clipboard);
        set_switch(&self.quiet, &self.files, profile.files);
        self.update_networks(model, profile);
        true
    }

    /// A row for every trusted computer, checked while the profile
    /// connects it.
    fn update_computers(&self, model: &Snapshot, profile: &Profile) {
        let ids: Vec<String> = model.status.devices.iter().map(|d| d.id.clone()).collect();
        let same = {
            let rows = self.computer_rows.borrow();
            rows.len() == ids.len() && rows.iter().zip(&ids).all(|(r, id)| r.id == *id)
        };
        if !same {
            let mut rows = self.computer_rows.borrow_mut();
            for row in rows.drain(..) {
                self.computers.remove(&row.row);
            }
            for id in &ids {
                let row = super::plain_row();
                let check = gtk::CheckButton::builder()
                    .valign(gtk::Align::Center)
                    .build();
                // Each prefix goes before those added already: the check box
                // at the row's edge, then the icon.
                row.add_prefix(
                    &gtk::Image::builder()
                        .icon_name("computer-symbolic")
                        .accessible_role(gtk::AccessibleRole::Presentation)
                        .build(),
                );
                row.add_prefix(&check);
                row.set_activatable_widget(Some(&check));
                let (ui, quiet, profile_id, device) = (
                    self.ui.clone(),
                    self.quiet.clone(),
                    self.id.clone(),
                    id.clone(),
                );
                check.connect_toggled(move |check| {
                    if quiet.is_set() {
                        return;
                    }
                    if let Some(ui) = ui.upgrade() {
                        ui.command(profiles::set_connect(
                            &profile_id,
                            &device,
                            check.is_active(),
                        ));
                    }
                });
                self.computers.add(&row);
                rows.push(ComputerRow {
                    id: id.clone(),
                    row,
                    check,
                });
            }
        }
        for row in self.computer_rows.borrow().iter() {
            set_title(&row.row, &model.name_of(&row.id));
            let connects = profiles::connects(profile, &row.id);
            if row.check.is_active() != connects {
                self.quiet.run(|| row.check.set_active(connects));
            }
        }
    }

    /// The profile's networks, each with its remove button, and Add
    /// Current Network; Other networks lists none and says what it is for.
    fn update_networks(&self, model: &Snapshot, profile: &Profile) {
        let other = profile.kind() == ProfileKind::OtherNetworks;
        let description = if other {
            Some(t(keys::PROFILE_OTHER_NETWORKS_HINT))
        } else if profile.networks.is_empty() {
            Some(t(keys::PROFILE_NO_NETWORKS))
        } else {
            None
        };
        let shown = self
            .networks
            .description()
            .map(|d| d.to_string())
            .filter(|d| !d.is_empty());
        if shown != description {
            self.networks.set_description(description.as_deref());
        }
        self.add_current.set_visible(!other);
        self.add_current
            .set_sensitive(model.connected() && profiles::can_add_current(model, profile));
        let key = profile
            .networks
            .iter()
            .map(|n| format!("{}\u{1f}{}\u{1f}{}\u{1f}{}", n.id, n.name, n.router, n.kind))
            .collect::<Vec<_>>()
            .join("\u{1e}");
        if *self.networks_shown.borrow() == key {
            return;
        }
        *self.networks_shown.borrow_mut() = key;
        let mut shown = self.network_rows.borrow_mut();
        for row in shown.drain(..) {
            self.networks.remove(&row);
        }
        for network in &profile.networks {
            let row = super::plain_row();
            row.set_title(&tr(&profiles::network_name(network)));
            if let Some(detail) = profiles::network_detail(network) {
                row.set_subtitle(&tr(&detail));
            }
            row.add_prefix(
                &gtk::Image::builder()
                    .icon_name(profiles::network_icon(network))
                    .accessible_role(gtk::AccessibleRole::Presentation)
                    .build(),
            );
            let remove = gtk::Button::builder()
                .icon_name("window-close-symbolic")
                .tooltip_text(t(keys::PROFILE_REMOVE_NETWORK))
                .valign(gtk::Align::Center)
                .css_classes(["flat", "circular"])
                .build();
            remove.update_property(&[gtk::accessible::Property::Label(&t(
                keys::PROFILE_REMOVE_NETWORK,
            ))]);
            let (ui, network_id) = (self.ui.clone(), network.id.clone());
            remove.connect_clicked(move |_| {
                if let Some(ui) = ui.upgrade() {
                    ui.command(profiles::remove_network(&network_id));
                }
            });
            row.add_suffix(&remove);
            // Rows go before the button below them.
            self.networks.add(&row);
            shown.push(row.upcast());
        }
    }

    #[cfg(test)]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[cfg(test)]
    pub fn name_row(&self) -> &adw::EntryRow {
        self.name.row()
    }

    /// What is said under the name, while it shows.
    #[cfg(test)]
    pub fn name_refusal(&self) -> Option<String> {
        self.name_refused
            .is_visible()
            .then(|| self.name_refused.text().to_string())
    }

    #[cfg(test)]
    pub fn sharing_buttons(&self) -> (&gtk::ToggleButton, &gtk::ToggleButton) {
        (&self.share, &self.pause)
    }

    #[cfg(test)]
    pub fn sharing_row(&self) -> &adw::ActionRow {
        &self.sharing
    }

    /// Each computer's row: its id, name, whether it is checked, and
    /// whether it can be changed now.
    #[cfg(test)]
    pub fn computers(&self) -> Vec<(String, String, bool, bool)> {
        self.computer_rows
            .borrow()
            .iter()
            .map(|r| {
                (
                    r.id.clone(),
                    r.row.title().to_string(),
                    r.check.is_active(),
                    r.check.is_sensitive(),
                )
            })
            .collect()
    }

    #[cfg(test)]
    pub fn check(&self, id: &str) -> Option<gtk::CheckButton> {
        self.computer_rows
            .borrow()
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.check.clone())
    }

    #[cfg(test)]
    pub fn switches(&self) -> (&adw::SwitchRow, &adw::SwitchRow) {
        (&self.clipboard, &self.files)
    }

    #[cfg(test)]
    pub fn networks_group(&self) -> &adw::PreferencesGroup {
        &self.networks
    }

    /// The networks listed: each one's title and line under it.
    #[cfg(test)]
    pub fn networks(&self) -> Vec<(String, String)> {
        self.network_rows
            .borrow()
            .iter()
            .filter_map(|w| w.downcast_ref::<adw::ActionRow>())
            .map(|r| {
                (
                    r.title().to_string(),
                    r.subtitle().map(|s| s.to_string()).unwrap_or_default(),
                )
            })
            .collect()
    }

    /// The remove button of the network listed `index`th.
    #[cfg(test)]
    pub fn remove_network_button(&self, index: usize) -> Option<gtk::Button> {
        let row = self.network_rows.borrow().get(index)?.clone();
        let mut found = None;
        let mut stack = vec![row];
        while let Some(widget) = stack.pop() {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                found = Some(button.clone());
                break;
            }
            let mut child = widget.first_child();
            while let Some(c) = child {
                child = c.next_sibling();
                stack.push(c);
            }
        }
        found
    }

    #[cfg(test)]
    pub fn add_current_button(&self) -> &gtk::Button {
        &self.add_current
    }

    #[cfg(test)]
    pub fn remove_button(&self) -> &gtk::Button {
        &self.remove
    }
}
