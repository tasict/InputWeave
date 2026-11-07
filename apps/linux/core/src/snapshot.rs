//! The app's view of the background service: the state it last reported,
//! and what the app derives from it, as the Windows app's `AgentSnapshot`.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use iw_proto::app::v1::{
    ArrangedDevice, Arrangement, ConnectionState, DeviceStatus, Hotkeys, NearbyDevice,
    PairingState, Permission, SessionType, Status, Transfers,
};

use crate::shortcut;
use crate::text::{UiText, keys};
use crate::ui;

/// How the app is connected to the background service.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Link {
    /// Not connected, or the connection is being set up.
    #[default]
    Connecting,
    Connected,
    /// The service speaks another IPC version: usually an update in
    /// progress, and the service restarts soon.
    Incompatible,
}

/// What the service last reported, and what the app counts of its one-off
/// events. A counter changes once per event, so the UI acts on each once.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub link: Link,
    /// Since when the service has been out of reach; `None` while
    /// connected.
    pub unreachable_since: Option<SystemTime>,
    pub status: Status,
    pub pairing: PairingState,
    pub nearby: Vec<NearbyDevice>,
    /// Behind their own `Arc`, so copying a snapshot to change it (the
    /// reading thread does while the UI holds the last one) does not copy
    /// the desktop picture previews or the transfer history.
    pub arrangement: Arc<Arrangement>,
    pub transfers: Arc<Transfers>,
    pub agent_version: String,
    /// SHA-256 of the agent's executable, in hex; empty when unknown.
    pub agent_digest: String,
    /// The IPC version of an agent that refused this app's, as it said.
    pub agent_ipc_version: Option<u32>,
    /// Event.screens_identified, counted, with how long to show the names.
    pub screens_identified: u64,
    pub identify_ms: u32,
    /// Event.cursor_located, counted.
    pub cursor_located: u64,
    /// Event.relative_unsupported, counted, and the device it was about.
    pub relative_refusals: u64,
    pub relative_refused_device: String,
    /// The last wait at the edge in effect other than 0, so turning the
    /// wait back on uses it.
    pub last_switch_delay_ms: u32,
}

impl Snapshot {
    /// A service out of reach for this long has not merely been restarted.
    pub const UNRESPONSIVE_AFTER: Duration = Duration::from_secs(10);

    pub fn connected(&self) -> bool {
        self.link == Link::Connected
    }

    pub fn is_unresponsive(&self, now: SystemTime) -> bool {
        self.link == Link::Connecting
            && self.unreachable_since.is_some_and(|since| {
                now.duration_since(since).unwrap_or_default() > Self::UNRESPONSIVE_AFTER
            })
    }

    /// The service refused this app's IPC version and speaks a newer one:
    /// the app is the one behind (left running across an upgrade).
    pub fn app_is_older(&self) -> bool {
        self.link == Link::Incompatible
            && self
                .agent_ipc_version
                .is_some_and(|v| v > crate::agent::IPC_VERSION)
    }

    pub fn this_device_id(&self) -> &str {
        &self.status.this_device_id
    }

    pub fn device(&self, id: &str) -> Option<&DeviceStatus> {
        self.status.devices.iter().find(|d| d.id == id)
    }

    pub fn arranged(&self, id: &str) -> Option<&ArrangedDevice> {
        self.arrangement.devices.iter().find(|d| d.id == id)
    }

    /// The name of a device, this one included; its id when unknown.
    pub fn name_of(&self, id: &str) -> String {
        if id == self.status.this_device_id {
            return self.status.this_device_name.clone();
        }
        self.device(id)
            .map(|d| d.name.clone())
            .or_else(|| self.arranged(id).map(|d| d.name.clone()))
            .unwrap_or_else(|| id.to_owned())
    }

    /// The device the cursor is on.
    pub fn cursor_device_id(&self) -> &str {
        if self.status.active_device_id.is_empty() {
            &self.status.this_device_id
        } else {
            &self.status.active_device_id
        }
    }

    pub fn controlling_other(&self) -> bool {
        !self.status.active_device_id.is_empty()
            && self.status.active_device_id != self.status.this_device_id
    }

    pub fn controlled_from_elsewhere(&self) -> bool {
        !self.status.controlled_by_device_id.is_empty()
    }

    /// Whether clicking a device moves the cursor there (JumpTo). Not to
    /// another device from one whose cursor leaves only across the screen
    /// edges (Wayland): the desktop hands the keyboard and mouse over only
    /// there.
    pub fn can_jump(&self, id: &str) -> bool {
        self.connected()
            && self.status.sharing
            && !self.status.locked
            && id != self.cursor_device_id()
            && (id == self.status.this_device_id
                || (!self.edges_only()
                    && self
                        .device(id)
                        .is_some_and(|d| d.state() == ConnectionState::Connected)))
            && !self.out_of_reach(id)
    }

    /// A device input from here cannot reach now: its screen is locked
    /// where input cannot reach, it can take no input, or its sharing is
    /// paused (as the macOS app's `outOfReach`).
    pub fn out_of_reach(&self, id: &str) -> bool {
        self.device(id).is_some_and(|d| {
            (d.screen_locked && !d.lock_screen_input) || d.input_unavailable || d.sharing_paused
        })
    }

    /// From this device the cursor leaves only across the edges of its
    /// screens (Status.cursor_leaves_at_edges_only): jumps to other devices
    /// are not offered.
    pub fn edges_only(&self) -> bool {
        self.connected() && self.status.cursor_leaves_at_edges_only
    }

    /// This device's session is Wayland, as the service says.
    pub fn wayland(&self) -> bool {
        self.status
            .session
            .as_ref()
            .is_some_and(|s| s.r#type() == SessionType::Wayland)
    }

    /// Whether one of the portals' permissions waits for the user to ask
    /// for it: a row of `trouble_text::permission_rows` with a button (not
    /// granted, and neither being asked nor restored now).
    pub fn permission_to_ask(&self) -> bool {
        crate::trouble_text::permission_rows(self)
            .iter()
            .any(|r| r.button.is_some())
    }

    /// The wait at the edge to ask for when it is turned on.
    pub fn switch_delay_ms(&self) -> u32 {
        if self.last_switch_delay_ms != 0 {
            self.last_switch_delay_ms
        } else {
            self.status.default_dwell_ms
        }
    }

    pub fn missing_permissions(&self) -> Vec<Permission> {
        self.status
            .permissions
            .iter()
            .filter(|p| !p.granted)
            .map(|p| p.permission())
            .collect()
    }

    pub fn offline_devices(&self) -> Vec<&DeviceStatus> {
        self.status
            .devices
            .iter()
            .filter(|d| d.state() != ConnectionState::Connected)
            .collect()
    }

    pub fn connected_devices(&self) -> Vec<&DeviceStatus> {
        self.status
            .devices
            .iter()
            .filter(|d| d.state() == ConnectionState::Connected)
            .collect()
    }

    /// The devices files can go to now: those without a reason they can't
    /// (`send_choice::unavailable`: connected, files not turned off).
    pub fn file_receivers(&self) -> Vec<&DeviceStatus> {
        self.status
            .devices
            .iter()
            .filter(|d| crate::send_choice::unavailable(self, d).is_none())
            .collect()
    }

    pub fn can_receive_files(&self, id: &str) -> bool {
        self.file_receivers().iter().any(|d| d.id == id)
    }

    pub fn files_off(&self, id: &str) -> bool {
        self.device(id).is_some_and(|d| d.files_off)
    }

    /// The name a renamed device gives itself, while it differs.
    pub fn reported_name(&self, id: &str) -> Option<&str> {
        self.device(id)
            .filter(|d| !d.reported_name.is_empty() && d.reported_name != d.name)
            .map(|d| d.reported_name.as_str())
    }

    /// The hotkeys in effect; the defaults from services before them.
    pub fn hotkeys(&self) -> Hotkeys {
        self.status
            .hotkeys
            .clone()
            .unwrap_or_else(shortcut::defaults)
    }

    pub fn take_back_shortcut(&self) -> String {
        shortcut::text(&self.hotkeys().take_back)
    }

    pub fn lock_shortcut(&self) -> String {
        shortcut::text(&self.hotkeys().lock)
    }

    /// "Ctrl+Alt+Shift+2" for the device with that jump number; none for
    /// other devices where jumps to them are not offered (`edges_only`),
    /// while this computer's stays: jumping back here still works.
    pub fn jump_shortcut(&self, slot: u32) -> Option<String> {
        if self.edges_only() && Some(slot) != self.this_jump_slot() {
            return None;
        }
        shortcut::jump(&self.hotkeys().jump_modifiers, slot, true)
    }

    /// This computer's jump number in the arrangement, if it has one.
    pub fn this_jump_slot(&self) -> Option<u32> {
        self.arrangement
            .devices
            .iter()
            .find(|d| d.this_device)
            .map(|d| d.jump_slot)
            .filter(|s| *s != 0)
    }

    pub fn relative_supported(&self) -> bool {
        self.status.relative_supported
    }

    /// Whether the service knows the double press of Control that takes
    /// control back: it reports it, empty when off.
    pub fn double_press_supported(&self) -> bool {
        self.status
            .hotkeys
            .as_ref()
            .is_some_and(|h| h.take_back_double.is_some())
    }

    /// Whether pressing Control twice takes control back too.
    pub fn double_press_on(&self) -> bool {
        self.double_press_supported()
            && self
                .hotkeys()
                .take_back_double
                .is_some_and(|d| !d.is_empty())
    }

    /// Whether what tells how to take control back names the double press:
    /// it is on, and relative mouse is not, as the service ignores it then
    /// (games often use Ctrl).
    fn double_press_hinted(&self) -> bool {
        self.double_press_on() && self.status.relative_mouse.is_empty()
    }

    /// How to take control back from the computer this one controls, as
    /// the panel says it: the shortcut, and the double press while it
    /// works. Every other place that tells it says this while it works.
    pub fn take_back_hint(&self) -> UiText {
        let key = if self.double_press_hinted() {
            keys::SUBTITLE_CONTROLLING_DOUBLE
        } else {
            keys::SUBTITLE_CONTROLLING
        };
        ui!(key, self.take_back_shortcut())
    }

    /// How to take control back, as a notice says it: at any time; while
    /// the double press works, as the panel does.
    pub fn take_back_reminder(&self) -> UiText {
        if self.double_press_hinted() {
            self.take_back_hint()
        } else {
            ui!(keys::HUD_TAKE_BACK_HINT, self.take_back_shortcut())
        }
    }

    /// What setup says once controlling another computer worked.
    pub fn take_back_worked(&self) -> UiText {
        if self.double_press_hinted() {
            ui!(keys::SETUP_TRY_DONE_HINT, self.take_back_hint())
        } else {
            ui!(keys::SETUP_TRY_DONE, self.take_back_shortcut())
        }
    }

    pub fn relative_shortcut(&self) -> Option<String> {
        if !self.relative_supported() {
            return None;
        }
        self.hotkeys()
            .relative
            .filter(|r| !r.is_empty())
            .map(|r| shortcut::text(&r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iw_proto::app::v1::{PermissionAsk, PermissionStatus};

    fn device(id: &str, state: ConnectionState) -> DeviceStatus {
        DeviceStatus {
            id: id.into(),
            name: format!("{id} name"),
            state: state as i32,
            ..Default::default()
        }
    }

    fn connected_snapshot() -> Snapshot {
        Snapshot {
            link: Link::Connected,
            status: Status {
                sharing: true,
                this_device_id: "me".into(),
                this_device_name: "Linux box".into(),
                devices: vec![
                    device("mac", ConnectionState::Connected),
                    device("pc", ConnectionState::Offline),
                ],
                default_dwell_ms: 150,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn names_and_the_cursor() {
        let mut s = connected_snapshot();
        assert_eq!(s.name_of("me"), "Linux box");
        assert_eq!(s.name_of("mac"), "mac name");
        assert_eq!(s.name_of("gone"), "gone");
        assert_eq!(s.cursor_device_id(), "me");
        assert!(!s.controlling_other());
        s.status.active_device_id = "mac".into();
        assert!(s.controlling_other());
        s.status.active_device_id = "me".into();
        assert!(!s.controlling_other());
    }

    #[test]
    fn jumping_needs_a_connected_device_in_reach() {
        let mut s = connected_snapshot();
        assert!(s.can_jump("mac"));
        assert!(!s.can_jump("pc"));
        assert!(!s.can_jump("me"), "the cursor is already here");
        s.status.devices[0].screen_locked = true;
        assert!(!s.can_jump("mac"));
        s.status.devices[0].lock_screen_input = true;
        assert!(s.can_jump("mac"));
        s.status.locked = true;
        assert!(!s.can_jump("mac"));
        s.status.locked = false;
        s.status.sharing = false;
        assert!(!s.can_jump("mac"));
    }

    #[test]
    fn jumping_leaves_out_devices_that_cannot_take_input_or_paused() {
        let mut s = connected_snapshot();
        s.status.devices[0].sharing_paused = true;
        assert!(s.out_of_reach("mac") && !s.can_jump("mac"));
        s.status.devices[0].sharing_paused = false;
        s.status.devices[0].input_unavailable = true;
        assert!(s.out_of_reach("mac") && !s.can_jump("mac"));
        s.status.devices[0].input_unavailable = false;
        assert!(s.can_jump("mac"));
    }

    #[test]
    fn no_jumps_from_a_device_whose_cursor_leaves_only_across_edges() {
        let mut s = connected_snapshot();
        s.status.hotkeys = Some(shortcut::defaults());
        s.status.cursor_leaves_at_edges_only = true;
        assert!(s.edges_only());
        assert!(!s.can_jump("mac"));
        assert_eq!(s.jump_shortcut(2), None);
        // This computer's own number stays.
        s.arrangement = Arc::new(Arrangement {
            devices: vec![ArrangedDevice {
                id: "me".into(),
                this_device: true,
                jump_slot: 1,
                ..Default::default()
            }],
            ..Default::default()
        });
        assert_eq!(s.this_jump_slot(), Some(1));
        assert_eq!(s.jump_shortcut(1).as_deref(), Some("Ctrl+Alt+Shift+1"));
        assert_eq!(s.jump_shortcut(2), None);
        // Back to this device still, from another one.
        s.status.active_device_id = "mac".into();
        assert!(s.can_jump("me"));
        s.status.cursor_leaves_at_edges_only = false;
        assert!(s.can_jump("me"));
        assert_eq!(s.jump_shortcut(2).as_deref(), Some("Ctrl+Alt+Shift+2"));
    }

    #[test]
    fn the_permissions_asked_for_here_are_the_askable_ones() {
        let mut s = connected_snapshot();
        let permission = |p: Permission, ask: PermissionAsk, granted: bool| PermissionStatus {
            permission: p as i32,
            granted,
            ask: ask as i32,
            ..Default::default()
        };
        s.status.permissions = vec![
            permission(Permission::LocalNetwork, PermissionAsk::NotAskable, false),
            permission(Permission::RemoteControl, PermissionAsk::Restoring, false),
            permission(Permission::InputCapture, PermissionAsk::Askable, true),
        ];
        let rows: Vec<Permission> = crate::trouble_text::permission_rows(&s)
            .iter()
            .map(|r| r.permission)
            .collect();
        assert_eq!(rows, [Permission::RemoteControl, Permission::InputCapture]);
        // Remote control is being restored: nothing to ask for now.
        assert!(!s.permission_to_ask());
        for (ask, actionable) in [
            (PermissionAsk::Askable, true),
            (PermissionAsk::Denied, true),
            (PermissionAsk::Failed, true),
            (PermissionAsk::Stopped, true),
            (PermissionAsk::Asking, false),
            (PermissionAsk::Restoring, false),
        ] {
            s.status.permissions[1].ask = ask as i32;
            assert_eq!(s.permission_to_ask(), actionable, "{ask:?}");
        }
        s.status.permissions[1].ask = PermissionAsk::Askable as i32;
        s.status.permissions[1].granted = true;
        assert!(!s.permission_to_ask());
        // macOS's own, not granted, are not asked for here.
        s.status.permissions[0].ask = PermissionAsk::Askable as i32;
        assert!(!s.permission_to_ask());
    }

    #[test]
    fn files_go_to_connected_devices_with_files_on() {
        let mut s = connected_snapshot();
        assert_eq!(s.file_receivers().len(), 1);
        assert!(s.can_receive_files("mac"));
        s.status.devices[0].files_off = true;
        assert!(!s.can_receive_files("mac"));
        assert!(s.files_off("mac"));
        assert!(!s.can_receive_files("pc"));
    }

    #[test]
    fn unresponsive_after_ten_seconds_out_of_reach() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let s = Snapshot {
            unreachable_since: Some(start),
            ..Default::default()
        };
        assert!(!s.is_unresponsive(start + Duration::from_secs(10)));
        assert!(s.is_unresponsive(start + Duration::from_secs(11)));
        let incompatible = Snapshot {
            link: Link::Incompatible,
            ..s
        };
        assert!(!incompatible.is_unresponsive(start + Duration::from_secs(60)));
    }

    #[test]
    fn hotkeys_default_for_older_services() {
        let mut s = connected_snapshot();
        assert_eq!(s.take_back_shortcut(), "Ctrl+Alt+Shift+Esc");
        assert_eq!(s.jump_shortcut(2).as_deref(), Some("Ctrl+Alt+Shift+2"));
        assert_eq!(s.relative_shortcut(), None, "relative mode unsupported");
        s.status.relative_supported = true;
        assert_eq!(s.relative_shortcut().as_deref(), Some("Ctrl+Alt+Shift+G"));
        s.status.hotkeys = Some(Hotkeys {
            take_back: "gui+F5".into(),
            lock: "alt+KeyL".into(),
            jump_modifiers: String::new(),
            relative: Some(String::new()),
            take_back_double: None,
        });
        assert_eq!(s.take_back_shortcut(), "Super+F5");
        assert_eq!(s.jump_shortcut(2), None);
        assert_eq!(s.relative_shortcut(), None);
    }

    #[test]
    fn misc_derivations() {
        let mut s = connected_snapshot();
        assert_eq!(s.switch_delay_ms(), 150);
        s.last_switch_delay_ms = 400;
        assert_eq!(s.switch_delay_ms(), 400);
        s.status.permissions = vec![PermissionStatus {
            permission: Permission::Accessibility as i32,
            granted: false,
            ..Default::default()
        }];
        assert_eq!(s.missing_permissions(), vec![Permission::Accessibility]);
        s.status.devices[0].reported_name = "Studio".into();
        assert_eq!(s.reported_name("mac"), Some("Studio"));
        s.status.devices[0].reported_name = "mac name".into();
        assert_eq!(s.reported_name("mac"), None);
        assert_eq!(s.offline_devices().len(), 1);
        assert_eq!(s.connected_devices().len(), 1);
    }
}
