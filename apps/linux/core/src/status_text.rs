//! What the app says about the state of sharing: the title and line under
//! it (the panel's on the other two apps; here the General page's status
//! and the tray menu's first line), the problems it lists, the tray menu's
//! items and the tray icon.

use std::time::SystemTime;

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
            None => ui!(keys::SUBTITLE_CONTROLLING, model.take_back_shortcut()),
        });
    }
    if status.sharing && status.locked {
        return Some(ui!(keys::SUBTITLE_LOCKED, model.lock_shortcut()));
    }
    if status.sharing && model.controlling_other() {
        return Some(ui!(keys::SUBTITLE_CONTROLLING, model.take_back_shortcut()));
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

/// The problems to list while the service is connected: computers out of
/// reach, and (`compact`, outside Troubleshooting) no other computer yet.
/// A stopped or unreachable service is no row: the main window covers its
/// pages then, and offers to start or restart it.
pub fn problems(model: &Snapshot, compact: bool) -> Vec<Problem> {
    let mut rows = Vec::new();
    if model.connected() {
        for device in model.offline_devices() {
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
        && model.offline_devices().is_empty()
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
            pause_enabled: connected,
            lock_key: if status.locked {
                keys::MENU_UNLOCK
            } else {
                keys::MENU_LOCK
            },
            // While another computer controls this one, its lock is the one
            // that holds the cursor.
            lock_enabled: connected && status.sharing && !model.controlled_from_elsewhere(),
            identify_enabled: connected,
            send_enabled: connected && !model.file_receivers().is_empty(),
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
        if !model.status.sharing {
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
