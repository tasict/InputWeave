//! What the app says about the state of sharing: the title and line under
//! it (the panel's on the other two apps; here the General page's status
//! and the tray menu's first line), the problems it lists, the tray menu's
//! items and the tray icon.

use std::time::SystemTime;

use iw_proto::app::v1::DeviceStatus;

use crate::offline;
use crate::profiles;
use crate::service::ServiceState;
use crate::snapshot::{Link, Snapshot};
use crate::text::{UiText, keys};
use crate::ui;

pub fn title(model: &Snapshot, service: ServiceState, now: SystemTime) -> UiText {
    match model.link {
        Link::Incompatible if model.app_is_older() => return ui!(keys::TITLE_APP_OLDER),
        Link::Incompatible => return ui!(keys::TITLE_UPDATING),
        Link::Connecting => {
            // The service is stopped, so nothing will start: the problem
            // row says so and offers to start it.
            if service == ServiceState::Stopped {
                return ui!(keys::TITLE_NOT_WORKING);
            }
            return if model.is_unresponsive(now) {
                ui!(keys::TITLE_NOT_RESPONDING)
            } else {
                ui!(keys::TITLE_STARTING)
            };
        }
        Link::Connected => {}
    }
    let status = &model.status;
    // Not even on the network: before everything else, naming the profile.
    if let Some(paused) = profiles::paused_title(model) {
        return paused;
    }
    // Neither controlling nor controlled, whatever sharing says.
    if offline::this_offline(model) {
        return ui!(keys::TITLE_THIS_OFFLINE);
    }
    if !status.sharing {
        return ui!(keys::TITLE_PAUSED);
    }
    if model.controlled_from_elsewhere() {
        return ui!(
            keys::TITLE_CONTROLLED_BY,
            model.name_of(&status.controlled_by_device_id)
        );
    }
    if !status.relative_mouse.is_empty() {
        return ui!(keys::TITLE_RELATIVE, model.name_of(&status.relative_mouse));
    }
    if status.locked {
        return ui!(
            keys::TITLE_LOCKED_TO,
            model.name_of(model.cursor_device_id())
        );
    }
    if model.controlling_other() {
        return ui!(
            keys::TITLE_CONTROLLING,
            model.name_of(&status.active_device_id)
        );
    }
    ui!(keys::TITLE_SHARING)
}

pub fn subtitle(model: &Snapshot) -> Option<UiText> {
    if !model.connected() {
        return None;
    }
    // Under the paused profile's title, only that the network is in no
    // profile, where the service picked it for that.
    if profiles::paused(model) {
        return profiles::paused_subtitle(model);
    }
    if let Some(offline) = offline::this_subtitle(model) {
        return Some(offline);
    }
    let status = &model.status;
    if model.controlled_from_elsewhere() {
        // On Wayland this computer's own mouse cannot take control back
        // while another controls it (the desktop hands its input over only
        // at the screen edges, and nothing crosses there meanwhile).
        if model.wayland() {
            return Some(ui!(
                keys::SUBTITLE_CONTROLLED_WAYLAND,
                model.name_of(&status.controlled_by_device_id)
            ));
        }
        return Some(ui!(keys::SUBTITLE_CONTROLLED));
    }
    if status.sharing && !status.relative_mouse.is_empty() {
        return Some(match model.relative_shortcut() {
            Some(shortcut) => ui!(keys::SUBTITLE_RELATIVE, shortcut),
            None => model.take_back_hint(),
        });
    }
    if status.sharing && status.locked {
        return Some(ui!(keys::SUBTITLE_LOCKED, model.lock_shortcut()));
    }
    if status.sharing && model.controlling_other() {
        return Some(model.take_back_hint());
    }
    if status.sharing && !status.this_device_name.is_empty() {
        return Some(ui!(
            keys::SUBTITLE_CURSOR_ON,
            status.this_device_name.clone()
        ));
    }
    None
}

/// What a problem row offers to do about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProblemAction {
    None,
    Troubleshoot,
    AddDevice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub text: UiText,
    pub action: ProblemAction,
}

impl Problem {
    pub fn action_key(&self) -> Option<&'static str> {
        match self.action {
            ProblemAction::Troubleshoot => Some(keys::ACTION_TROUBLESHOOT),
            ProblemAction::AddDevice => Some(keys::ACTION_ADD_DEVICE),
            ProblemAction::None => None,
        }
    }
}

/// The computers out of reach: offline, but neither offline for now nor
/// left out of the profile in use (that is someone's choice, not a fault),
/// and none while this computer is offline for now or the profile in use is
/// paused, which keep every other one offline.
fn unreachable(model: &Snapshot) -> Vec<&DeviceStatus> {
    if offline::this_offline(model) || profiles::paused(model) {
        return Vec::new();
    }
    model
        .offline_devices()
        .into_iter()
        .filter(|d| !offline::is_offline(model, &d.id) && !profiles::not_in_profile(model, &d.id))
        .collect()
}

/// The problems to list while the service is connected: computers out of
/// reach, and (`compact`, outside Troubleshooting) no other computer yet.
/// A stopped or unreachable service is no row: the main window covers its
/// pages then, and offers to start or restart it.
pub fn problems(model: &Snapshot, compact: bool) -> Vec<Problem> {
    let mut rows = Vec::new();
    if model.connected() {
        for device in unreachable(model) {
            rows.push(Problem {
                text: ui!(keys::EXCEPTION_UNREACHABLE, device.name.clone()),
                action: if compact {
                    ProblemAction::Troubleshoot
                } else {
                    ProblemAction::None
                },
            });
        }
        if model.status.devices.is_empty() && compact {
            rows.push(Problem {
                text: ui!(keys::EXCEPTION_NO_DEVICES),
                action: ProblemAction::AddDevice,
            });
        }
    }
    rows
}

pub fn everything_works(model: &Snapshot, service: ServiceState) -> bool {
    matches!(service, ServiceState::Running | ServiceState::Unknown)
        && model.connected()
        && unreachable(model).is_empty()
        && model.status.capturing
}

/// The tray menu's items that change with the state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuState {
    pub pause_key: &'static str,
    pub pause_enabled: bool,
    pub lock_key: &'static str,
    pub lock_enabled: bool,
    pub identify_enabled: bool,
    pub send_enabled: bool,
    /// Taking this computer offline for now, or bringing it back: its
    /// label and whether it takes it offline; `None` from a service before
    /// it (`offline::this_item`).
    pub offline: Option<(&'static str, bool)>,
    pub offline_enabled: bool,
    /// The Profile submenu's choices (`profiles::menu`); empty from a
    /// service before profiles, when there is no submenu.
    pub profiles: Vec<profiles::Choice>,
    pub profiles_enabled: bool,
}

impl MenuState {
    pub fn of(model: &Snapshot) -> MenuState {
        let connected = model.connected();
        let status = &model.status;
        MenuState {
            pause_key: if connected && !status.sharing {
                keys::MENU_RESUME_SHARING
            } else {
                keys::MENU_PAUSE_SHARING
            },
            // While the profile in use is paused, another profile is the way
            // to share again.
            pause_enabled: connected && !profiles::paused(model),
            lock_key: if status.locked {
                keys::MENU_UNLOCK
            } else {
                keys::MENU_LOCK
            },
            // While another computer controls this one, its lock is the one
            // that holds the cursor; while this one is offline for now, or
            // the profile in use is paused, there is nowhere else for it to
            // go.
            lock_enabled: connected
                && status.sharing
                && !model.controlled_from_elsewhere()
                && !offline::this_offline(model)
                && !profiles::paused(model),
            identify_enabled: connected,
            send_enabled: connected && !model.file_receivers().is_empty(),
            offline: offline::this_item(model),
            offline_enabled: connected,
            profiles: profiles::menu(model),
            profiles_enabled: connected,
        }
    }
}

/// The tray icon's five looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrayState {
    Normal,
    Controlling,
    Controlled,
    Paused,
    Attention,
}

impl TrayState {
    pub const ALL: [TrayState; 5] = [
        TrayState::Normal,
        TrayState::Controlling,
        TrayState::Controlled,
        TrayState::Paused,
        TrayState::Attention,
    ];

    /// The dot shows only for what needs the user: the service stopped, a
    /// missing permission, or no answer for longer than a restart takes.
    pub fn of(model: &Snapshot, service: ServiceState, now: SystemTime) -> TrayState {
        if (service == ServiceState::Stopped && !model.connected()) || model.is_unresponsive(now) {
            return TrayState::Attention;
        }
        // Until the service answers, its last status may be old or empty.
        if !model.connected() {
            return TrayState::Normal;
        }
        if !model.missing_permissions().is_empty() {
            return TrayState::Attention;
        }
        // Offline for now, or in a paused profile, this computer neither
        // controls nor is controlled, as when paused; another look would
        // change a second part of it.
        if !model.status.sharing || offline::this_offline(model) || profiles::paused(model) {
            return TrayState::Paused;
        }
        if model.controlled_from_elsewhere() {
            return TrayState::Controlled;
        }
        if model.controlling_other() {
            TrayState::Controlling
        } else {
            TrayState::Normal
        }
    }

    /// What screen readers say for the icon, and its tooltip's title: as
    /// [`TrayState::description`], but while this computer is offline for
    /// now (the paused look) that, not that sharing is paused; a paused
    /// profile in use, which the title names first, is sharing paused.
    pub fn description_of(self, model: &Snapshot) -> UiText {
        if self == TrayState::Paused
            && offline::this_offline(model)
            && !profiles::paused(model)
            && model.connected()
        {
            return ui!(keys::TRAY_THIS_OFFLINE);
        }
        self.description()
    }

    /// What screen readers say for the icon.
    pub fn description(self) -> UiText {
        match self {
            TrayState::Normal => ui!(keys::TRAY_NORMAL),
            TrayState::Controlling => ui!(keys::TRAY_CONTROLLING),
            TrayState::Controlled => ui!(keys::TRAY_CONTROLLED),
            TrayState::Paused => ui!(keys::TRAY_PAUSED),
            TrayState::Attention => ui!(keys::TRAY_ATTENTION),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            TrayState::Normal => "normal",
            TrayState::Controlling => "controlling",
            TrayState::Controlled => "controlled",
            TrayState::Paused => "paused",
            TrayState::Attention => "attention",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iw_proto::app::v1::{ConnectionState, DeviceStatus, Permission, PermissionStatus, Status};
    use std::time::Duration;

    fn model() -> Snapshot {
        Snapshot {
            link: Link::Connected,
            status: Status {
                sharing: true,
                capturing: true,
                this_device_id: "me".into(),
                this_device_name: "Linux box".into(),
                devices: vec![DeviceStatus {
                    id: "mac".into(),
                    name: "Mac mini".into(),
                    state: ConnectionState::Connected as i32,
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(10_000)
    }

    #[test]
    fn titles_say_where_the_keyboard_and_mouse_are() {
        let mut m = model();
        let service = ServiceState::Running;
        assert_eq!(
            title(&m, service, now()).to_string(),
            "Sharing keyboard and mouse"
        );
        assert_eq!(subtitle(&m).unwrap().to_string(), "Cursor on Linux box");
        m.status.active_device_id = "mac".into();
        assert_eq!(
            title(&m, service, now()).to_string(),
            "Controlling Mac mini"
        );
        assert_eq!(
            subtitle(&m).unwrap().to_string(),
            "Press Ctrl+Alt+Shift+Esc to take back control."
        );
        // With the double press on, it says so too; a single key reads as
        // itself.
        let mut double = m.clone();
        double.status.hotkeys = Some(crate::shortcut::with_double_press(
            &crate::shortcut::defaults(),
            true,
        ));
        assert!(double.double_press_on());
        assert_eq!(
            subtitle(&double).unwrap().to_string(),
            "Press Ctrl+Alt+Shift+Esc or double-press Ctrl to take back control."
        );
        double.status.hotkeys.as_mut().unwrap().take_back = "F13".into();
        assert_eq!(
            subtitle(&double).unwrap().to_string(),
            "Press F13 or double-press Ctrl to take back control."
        );
        // Off, or from a service that does not know it: the shortcut alone.
        double.status.hotkeys = Some(crate::shortcut::defaults());
        assert!(double.double_press_supported() && !double.double_press_on());
        assert_eq!(
            subtitle(&double).unwrap().to_string(),
            "Press Ctrl+Alt+Shift+Esc to take back control."
        );
        double.status.hotkeys.as_mut().unwrap().take_back_double = None;
        assert!(!double.double_press_supported());
        m.status.locked = true;
        assert_eq!(title(&m, service, now()).to_string(), "Locked to Mac mini");
        assert_eq!(
            subtitle(&m).unwrap().to_string(),
            "Press Ctrl+Alt+Shift+L to unlock."
        );
        m.status.relative_mouse = "mac".into();
        m.status.relative_supported = true;
        assert_eq!(
            title(&m, service, now()).to_string(),
            "Relative mouse on Mac mini"
        );
        assert_eq!(
            subtitle(&m).unwrap().to_string(),
            "Press Ctrl+Alt+Shift+G to end relative mouse."
        );
        m.status.controlled_by_device_id = "mac".into();
        assert_eq!(
            title(&m, service, now()).to_string(),
            "Controlled by Mac mini"
        );
        assert_eq!(
            subtitle(&m).unwrap().to_string(),
            "Move this computer's mouse to take it back."
        );
        // On Wayland the mouse here cannot take it back.
        m.status.session = Some(iw_proto::app::v1::SessionInfo {
            r#type: iw_proto::app::v1::SessionType::Wayland as i32,
            ..Default::default()
        });
        assert_eq!(
            subtitle(&m).unwrap().to_string(),
            "Press the take-back shortcut on “Mac mini”, or pause sharing here, to take the keyboard and mouse back."
        );
        m.status.sharing = false;
        assert_eq!(title(&m, service, now()).to_string(), "Sharing paused");
    }

    #[test]
    fn titles_before_the_service_answers() {
        let start = now();
        let m = Snapshot {
            unreachable_since: Some(start),
            ..Default::default()
        };
        assert_eq!(
            title(&m, ServiceState::Running, start).to_string(),
            "Starting the background service…"
        );
        assert_eq!(
            title(&m, ServiceState::Running, start + Duration::from_secs(11)).to_string(),
            "The background service is not responding."
        );
        assert_eq!(
            title(&m, ServiceState::Stopped, start).to_string(),
            "Sharing isn't working"
        );
        let updating = Snapshot {
            link: Link::Incompatible,
            ..m
        };
        assert_eq!(
            title(&updating, ServiceState::Running, start).to_string(),
            "Updating the background service…"
        );
        assert_eq!(subtitle(&updating), None);
    }

    #[test]
    fn problems_and_what_they_offer() {
        assert!(
            problems(&Snapshot::default(), true).is_empty(),
            "not connected"
        );
        let mut m = model();
        m.status.devices[0].state = ConnectionState::Offline as i32;
        let rows = problems(&m, true);
        assert_eq!(rows[0].text.to_string(), "Mac mini can't be reached.");
        assert_eq!(rows[0].action, ProblemAction::Troubleshoot);
        assert_eq!(problems(&m, false)[0].action, ProblemAction::None);
        assert!(!everything_works(&m, ServiceState::Running));
        m.status.devices.clear();
        assert_eq!(problems(&m, true)[0].action, ProblemAction::AddDevice);
        assert!(problems(&m, false).is_empty());
        assert!(everything_works(&m, ServiceState::Running));
        m.status.capturing = false;
        assert!(!everything_works(&m, ServiceState::Running));
    }

    #[test]
    fn the_menu_follows_the_state() {
        let mut m = model();
        let menu = MenuState::of(&m);
        assert_eq!(menu.pause_key, keys::MENU_PAUSE_SHARING);
        assert!(menu.lock_enabled && menu.send_enabled);
        m.status.sharing = false;
        m.status.locked = true;
        let menu = MenuState::of(&m);
        assert_eq!(menu.pause_key, keys::MENU_RESUME_SHARING);
        assert_eq!(menu.lock_key, keys::MENU_UNLOCK);
        assert!(!menu.lock_enabled);
        let offline = MenuState::of(&Snapshot::default());
        assert!(!offline.pause_enabled && !offline.identify_enabled && !offline.send_enabled);
    }

    #[test]
    fn the_tray_icon_has_one_look_at_a_time() {
        let mut m = model();
        assert_eq!(
            TrayState::of(&m, ServiceState::Running, now()),
            TrayState::Normal
        );
        m.status.active_device_id = "mac".into();
        assert_eq!(
            TrayState::of(&m, ServiceState::Running, now()),
            TrayState::Controlling
        );
        m.status.controlled_by_device_id = "mac".into();
        assert_eq!(
            TrayState::of(&m, ServiceState::Running, now()),
            TrayState::Controlled
        );
        m.status.sharing = false;
        assert_eq!(
            TrayState::of(&m, ServiceState::Running, now()),
            TrayState::Paused
        );
        m.status.permissions = vec![PermissionStatus {
            permission: Permission::Accessibility as i32,
            granted: false,
            ..Default::default()
        }];
        assert_eq!(
            TrayState::of(&m, ServiceState::Running, now()),
            TrayState::Attention
        );
        assert_eq!(
            TrayState::of(&Snapshot::default(), ServiceState::Stopped, now()),
            TrayState::Attention
        );
        assert_eq!(
            TrayState::of(&Snapshot::default(), ServiceState::Running, now()),
            TrayState::Normal
        );
    }

    /// The model with this computer offline for now, set by the Mac mini,
    /// from a service that knows it: every other computer offline.
    fn this_offline() -> Snapshot {
        let mut m = model();
        m.status.offline_supported = true;
        m.status.offline_for_now = true;
        m.status.offline_by_device_id = "mac".into();
        m.status.offline_by_name = "Mac mini".into();
        m.status.devices[0].state = ConnectionState::Offline as i32;
        m
    }

    #[test]
    fn this_computer_offline_for_now_says_so_and_who_set_it() {
        let service = ServiceState::Running;
        let mut m = this_offline();
        // Before paused, controlled and the rest.
        m.status.sharing = false;
        m.status.controlled_by_device_id = "mac".into();
        assert_eq!(
            title(&m, service, now()).to_string(),
            "This computer is offline for now"
        );
        assert_eq!(
            subtitle(&m).unwrap().to_string(),
            "Set by Mac mini. Your other computers don’t control this one or share the clipboard and files with it."
        );
        let zh = crate::text::Catalog::for_language(crate::text::Language::ChineseTaiwan);
        assert_eq!(zh.render(&title(&m, service, now())), "這台暫時離線");
        m.status.offline_by_device_id = "me".into();
        assert_eq!(
            subtitle(&m).unwrap().to_string(),
            "Set on this computer. Your other computers don’t control this one or share the clipboard and files with it."
        );
        // From a service before it: as ever.
        m.status.offline_supported = false;
        assert_eq!(title(&m, service, now()).to_string(), "Sharing paused");
    }

    #[test]
    fn computers_offline_for_now_are_no_problem() {
        let mut m = model();
        m.status.offline_supported = true;
        m.status.devices.push(DeviceStatus {
            id: "pc".into(),
            name: "Studio PC".into(),
            state: ConnectionState::Offline as i32,
            offline_for_now: true,
            ..Default::default()
        });
        assert!(problems(&m, true).is_empty());
        assert!(everything_works(&m, ServiceState::Running));
        // Asleep instead: out of reach.
        m.status.devices[1].offline_for_now = false;
        assert_eq!(
            problems(&m, true)[0].text.to_string(),
            "Studio PC can't be reached."
        );
        assert!(!everything_works(&m, ServiceState::Running));
        // This computer offline for now: every other one is offline, and
        // none of it is a problem.
        let m = this_offline();
        assert!(problems(&m, true).is_empty());
        assert!(problems(&m, false).is_empty());
        assert!(everything_works(&m, ServiceState::Running));
        // A service before it: every offline computer is listed.
        let mut old = this_offline();
        old.status.offline_supported = false;
        assert_eq!(problems(&old, true).len(), 1);
    }

    #[test]
    fn the_menu_takes_this_computer_offline_and_brings_it_back() {
        let mut m = model();
        assert_eq!(MenuState::of(&m).offline, None, "a service before it");
        m.status.offline_supported = true;
        let menu = MenuState::of(&m);
        assert_eq!(menu.offline, Some((keys::MENU_TAKE_THIS_OFFLINE, true)));
        assert!(menu.offline_enabled && menu.lock_enabled);
        let m = this_offline();
        let menu = MenuState::of(&m);
        assert_eq!(menu.offline, Some((keys::OFFLINE_BRING_BACK, false)));
        assert!(menu.offline_enabled);
        assert!(!menu.lock_enabled, "nowhere else for the cursor to go");
        assert!(menu.pause_enabled && !menu.send_enabled);
        assert_eq!(menu.pause_key, keys::MENU_PAUSE_SHARING, "its own choice");
        // Until the service answers, off.
        let mut away = m.clone();
        away.link = Link::Connecting;
        assert!(!MenuState::of(&away).offline_enabled);
    }

    #[test]
    fn the_tray_icon_is_paused_while_this_computer_is_offline_for_now() {
        let mut m = this_offline();
        let state = TrayState::of(&m, ServiceState::Running, now());
        assert_eq!(state, TrayState::Paused);
        let zh = crate::text::Catalog::for_language(crate::text::Language::ChineseTaiwan);
        let said = state.description_of(&m);
        assert_eq!(
            (said.to_string(), zh.render(&said)),
            (
                "InputWeave, this computer is offline for now".to_owned(),
                "InputWeave，這台暫時離線".to_owned()
            )
        );
        // Paused as well: still offline for now first.
        m.status.sharing = false;
        assert_eq!(
            TrayState::of(&m, ServiceState::Running, now()).description_of(&m),
            ui!(keys::TRAY_THIS_OFFLINE)
        );
        // Only paused.
        m.status.offline_for_now = false;
        assert_eq!(
            TrayState::of(&m, ServiceState::Running, now()).description_of(&m),
            ui!(keys::TRAY_PAUSED)
        );
        // A missing permission still needs the user first.
        let mut m = this_offline();
        m.status.permissions = vec![PermissionStatus {
            permission: Permission::Accessibility as i32,
            granted: false,
            ..Default::default()
        }];
        let state = TrayState::of(&m, ServiceState::Running, now());
        assert_eq!(state, TrayState::Attention);
        assert_eq!(state.description_of(&m), ui!(keys::TRAY_ATTENTION));
        // Other states say what they always do.
        let m = model();
        let state = TrayState::of(&m, ServiceState::Running, now());
        assert_eq!(state.description_of(&m), ui!(keys::TRAY_NORMAL));
    }

    #[test]
    fn a_paused_profile_comes_before_every_other_title() {
        let service = ServiceState::Running;
        let zh = crate::text::Catalog::for_language(crate::text::Language::ChineseTaiwan);
        let mut m = crate::sample::profile_paused(now(), false);
        // Before this computer offline for now, paused, controlled…
        m.status.offline_supported = true;
        m.status.offline_for_now = true;
        m.status.offline_by_device_id = "mac".into();
        m.status.offline_by_name = "Mac mini".into();
        m.status.sharing = false;
        let said = title(&m, service, now());
        assert_eq!(
            (said.to_string(), zh.render(&said)),
            (
                "Other networks: sharing paused".to_owned(),
                "其他網路：已暫停共享".to_owned()
            )
        );
        assert_eq!(
            subtitle(&m).map(|s| s.to_string()),
            Some("This network isn’t in any profile.".into())
        );
        // Chosen by the user: the title alone.
        m.status.profile_chosen = true;
        assert_eq!(subtitle(&m), None);
        // Not paused: as ever.
        let m = crate::sample::snapshot(now());
        assert_eq!(
            title(&m, service, now()).to_string(),
            "Sharing keyboard and mouse"
        );
        // From a service before profiles: as ever too.
        let mut old = crate::sample::profile_paused(now(), false);
        old.status.profiles_supported = false;
        assert_eq!(
            title(&old, service, now()).to_string(),
            "Sharing keyboard and mouse"
        );
    }

    #[test]
    fn computers_left_out_are_no_problem_nor_any_while_the_profile_is_paused() {
        let m = crate::sample::not_in_profile(now());
        // The ThinkPad is asleep: out of reach; the Studio PC is left out.
        let listed: Vec<String> = problems(&m, true)
            .iter()
            .map(|p| p.text.to_string())
            .collect();
        assert_eq!(listed, ["ThinkPad can't be reached."]);
        let paused = crate::sample::profile_paused(now(), false);
        assert!(problems(&paused, true).is_empty());
        assert!(problems(&paused, false).is_empty());
        let mut old = paused.clone();
        old.status.profiles_supported = false;
        assert_eq!(problems(&old, true).len(), 3);
    }

    #[test]
    fn a_paused_profile_turns_pause_and_lock_off_and_shows_the_paused_icon() {
        let m = crate::sample::snapshot(now());
        let menu = MenuState::of(&m);
        assert!(menu.pause_enabled && menu.lock_enabled && menu.profiles_enabled);
        let ids: Vec<&str> = menu.profiles.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["", "default", "office", "other"]);
        let paused = crate::sample::profile_paused(now(), true);
        let menu = MenuState::of(&paused);
        assert!(!menu.pause_enabled && !menu.lock_enabled);
        assert_eq!(menu.pause_key, keys::MENU_PAUSE_SHARING);
        let checked: Vec<&str> = menu
            .profiles
            .iter()
            .filter(|c| c.checked)
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(checked, ["other"]);
        let state = TrayState::of(&paused, ServiceState::Running, now());
        assert_eq!(state, TrayState::Paused);
        assert_eq!(state.description_of(&paused), ui!(keys::TRAY_PAUSED));
        // This computer offline for now as well: still sharing paused, as
        // the title says.
        let mut both = paused.clone();
        both.status.offline_supported = true;
        both.status.offline_for_now = true;
        assert_eq!(both_description(&both), ui!(keys::TRAY_PAUSED));
        // A service before profiles: no submenu.
        let mut old = crate::sample::snapshot(now());
        old.status.profiles_supported = false;
        assert!(MenuState::of(&old).profiles.is_empty());
    }

    fn both_description(m: &Snapshot) -> UiText {
        TrayState::of(m, ServiceState::Running, now()).description_of(m)
    }

    #[test]
    fn an_app_behind_its_service_says_so() {
        let mut model = Snapshot {
            link: Link::Incompatible,
            ..Snapshot::default()
        };
        let now = SystemTime::now();
        assert_eq!(
            title(&model, ServiceState::Running, now).to_string(),
            "Updating the background service…"
        );
        model.agent_ipc_version = Some(crate::agent::IPC_VERSION + 1);
        assert_eq!(
            title(&model, ServiceState::Running, now).to_string(),
            "This app is older than the background service"
        );
        model.agent_ipc_version = Some(crate::agent::IPC_VERSION.saturating_sub(1));
        assert_eq!(
            title(&model, ServiceState::Running, now).to_string(),
            "Updating the background service…"
        );
    }
}
