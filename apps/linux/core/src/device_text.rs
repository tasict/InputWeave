//! The words about devices: connection state, system, screens, the key
//! swap (the Windows app's `DeviceText`).

use iw_proto::app::v1::{ConnectionState, DeviceStatus, Screen};

use crate::snapshot::Snapshot;
use crate::text::{UiText, keys};
use crate::ui;

/// "Connected · 3 ms", "Paused sharing", "Screen locked, can't take
/// input"…, as the Windows app's `DeviceText.Connected`: a paused computer
/// first, then one that can take no input, then a locked screen.
pub fn connected(d: &DeviceStatus) -> UiText {
    let rtt = d.rtt_ms.to_string();
    let with = |plain: &'static str, timed: &'static str| {
        if d.rtt_ms == 0 {
            ui!(plain)
        } else {
            ui!(timed, rtt.clone())
        }
    };
    if d.sharing_paused {
        return with(
            keys::CONNECTION_PEER_PAUSED,
            keys::CONNECTION_PEER_PAUSED_RTT,
        );
    }
    if d.input_unavailable && !d.screen_locked {
        return with(keys::CONNECTION_NO_INPUT, keys::CONNECTION_NO_INPUT_RTT);
    }
    if d.screen_locked && (!d.lock_screen_input || d.input_unavailable) {
        return with(
            keys::CONNECTION_SCREEN_LOCKED_NO_INPUT,
            keys::CONNECTION_SCREEN_LOCKED_NO_INPUT_RTT,
        );
    }
    if d.screen_locked {
        return with(
            keys::CONNECTION_SCREEN_LOCKED,
            keys::CONNECTION_SCREEN_LOCKED_RTT,
        );
    }
    with(keys::CONNECTION_CONNECTED, keys::CONNECTION_CONNECTED_RTT)
}

pub fn status(model: &Snapshot, id: &str) -> UiText {
    if id == model.this_device_id() {
        return ui!(keys::CONNECTION_THIS_COMPUTER);
    }
    match model.device(id) {
        Some(d) if d.state() == ConnectionState::Connected => connected(d),
        _ => ui!(keys::CONNECTION_OFFLINE),
    }
}

/// Whether the device is connected (not this one).
pub fn is_connected(model: &Snapshot, id: &str) -> bool {
    model
        .device(id)
        .is_some_and(|d| d.state() == ConnectionState::Connected)
}

pub fn os(os: &str) -> UiText {
    match os {
        "macos" => UiText::verbatim("macOS"),
        "windows" => UiText::verbatim("Windows"),
        "linux" => UiText::verbatim("Linux"),
        _ => ui!(keys::OS_UNKNOWN),
    }
}

/// "2560 × 1440", the display's pixels.
pub fn resolution(screen: &Screen) -> String {
    let scale = screen.scale.max(1.0);
    format!(
        "{} × {}",
        (screen.width * scale).round(),
        (screen.height * scale).round()
    )
}

/// "Synced to N devices" after the arrangement changed while other
/// computers were connected; `None` when there is nothing to say, as at the
/// first arrangement the app sees.
pub fn synced(before: u64, after: u64, model: &Snapshot) -> Option<UiText> {
    let connected = model.connected_devices().len();
    if before == 0 || after == before || connected == 0 {
        return None;
    }
    Some(ui!(keys::CANVAS_SYNCED, connected.to_string()))
}

/// Says "Synced to N devices" for changes to the arrangement made while
/// it is shown: it follows the version on every model, shown or not, and
/// starts over when the service is out of reach (a restarted one counts
/// anew), so neither opening the page nor the service coming back is news.
#[derive(Debug, Default)]
pub struct SyncedWatch {
    version: u64,
}

impl SyncedWatch {
    pub fn update(&mut self, model: &Snapshot, shown: bool) -> Option<UiText> {
        if !model.connected() {
            self.version = 0;
            return None;
        }
        let before = std::mem::replace(&mut self.version, model.arrangement.version);
        if !shown {
            return None;
        }
        synced(before, model.arrangement.version, model)
    }
}

/// The swap of Ctrl with the command key for keys sent to a device of
/// `os`, named ⌘ if either side is a Mac, otherwise after this computer's
/// own key, Super (this computer is Linux).
pub fn swap_key(os: &str) -> &'static str {
    match os {
        "macos" => keys::SWAP_COMMAND_AND_CONTROL,
        _ => keys::SWAP_SUPER_AND_CONTROL,
    }
}

pub fn files_off(name: &str) -> UiText {
    ui!(keys::DEVICE_FILES_OFF, name)
}

/// Why files cannot go to the device now.
pub fn cannot_receive(model: &Snapshot, id: &str, compact: bool) -> Option<UiText> {
    if id == model.this_device_id() {
        return None;
    }
    if model.files_off(id) {
        return Some(if compact {
            ui!(keys::DEVICE_FILES_OFF_SHORT)
        } else {
            files_off(&model.name_of(id))
        });
    }
    Some(ui!(keys::CONNECTION_OFFLINE))
}

/// Why there is no computer to send to.
pub fn no_file_receiver(model: &Snapshot) -> UiText {
    let connected = model.connected_devices();
    if connected.len() == 1 && connected[0].files_off {
        files_off(&connected[0].name)
    } else {
        ui!(keys::SEND_TO_NONE_MESSAGE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Link;
    use iw_proto::app::v1::{DeviceStatus, Status};

    #[test]
    fn connection_text_with_and_without_a_round_trip() {
        let device = |rtt_ms, screen_locked, lock_screen_input| DeviceStatus {
            rtt_ms,
            screen_locked,
            lock_screen_input,
            ..Default::default()
        };
        assert_eq!(connected(&device(0, false, true)).to_string(), "Connected");
        assert_eq!(
            connected(&device(3, false, true)).to_string(),
            "Connected · 3 ms"
        );
        assert_eq!(
            connected(&device(3, true, true)).to_string(),
            "Screen locked · 3 ms"
        );
        assert_eq!(
            connected(&device(0, true, false)).to_string(),
            "Screen locked, can't take input"
        );
        // Input from here can't reach it.
        let mut no_input = device(4, false, true);
        no_input.input_unavailable = true;
        assert_eq!(connected(&no_input).to_string(), "Can't take input · 4 ms");
        no_input.screen_locked = true;
        assert_eq!(
            connected(&no_input).to_string(),
            "Screen locked, can't take input · 4 ms"
        );
        // Paused comes before everything else.
        no_input.sharing_paused = true;
        assert_eq!(connected(&no_input).to_string(), "Paused sharing · 4 ms");
        no_input.rtt_ms = 0;
        assert_eq!(connected(&no_input).to_string(), "Paused sharing");
    }

    #[test]
    fn devices_by_system() {
        assert_eq!(os("linux").to_string(), "Linux");
        assert_eq!(os("").to_string(), "Unknown system");
        // ⌘ if either side is a Mac, otherwise this computer's Super key.
        assert_eq!(swap_key("macos"), keys::SWAP_COMMAND_AND_CONTROL);
        assert_eq!(swap_key("windows"), keys::SWAP_SUPER_AND_CONTROL);
        assert_eq!(swap_key("linux"), keys::SWAP_SUPER_AND_CONTROL);
        assert_eq!(swap_key(""), keys::SWAP_SUPER_AND_CONTROL);
        let screen = Screen {
            width: 1280.0,
            height: 800.0,
            scale: 2.0,
            ..Default::default()
        };
        assert_eq!(resolution(&screen), "2560 × 1600");
    }

    #[test]
    fn statuses_and_receivers() {
        let model = Snapshot {
            link: Link::Connected,
            status: Status {
                this_device_id: "me".into(),
                devices: vec![DeviceStatus {
                    id: "mac".into(),
                    name: "Mac".into(),
                    state: ConnectionState::Connected as i32,
                    rtt_ms: 2,
                    files_off: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(status(&model, "me").to_string(), "This computer");
        assert_eq!(status(&model, "mac").to_string(), "Connected · 2 ms");
        assert_eq!(status(&model, "gone").to_string(), "Offline");
        assert_eq!(
            cannot_receive(&model, "mac", true).unwrap().to_string(),
            "Files turned off"
        );
        assert_eq!(
            no_file_receiver(&model).to_string(),
            "Files with “Mac” are turned off."
        );
    }

    #[test]
    fn says_synced_only_after_a_change_with_others_connected() {
        let mut model = Snapshot::default();
        model.status.devices.push(DeviceStatus {
            id: "mac".into(),
            state: ConnectionState::Connected as i32,
            ..Default::default()
        });
        assert_eq!(synced(0, 3, &model), None, "the first arrangement seen");
        assert_eq!(synced(3, 3, &model), None);
        assert_eq!(synced(3, 4, &model), Some(ui!(keys::CANVAS_SYNCED, "1")));
        model.status.devices[0].state = ConnectionState::Offline as i32;
        assert_eq!(synced(3, 4, &model), None);
    }

    #[test]
    fn says_synced_only_for_changes_made_while_shown() {
        use crate::snapshot::Link;
        use std::sync::Arc;
        let at = |version: u64, link: Link| {
            let mut model = Snapshot {
                link,
                ..Default::default()
            };
            model.status.devices.push(DeviceStatus {
                id: "mac".into(),
                state: ConnectionState::Connected as i32,
                ..Default::default()
            });
            model.arrangement = Arc::new(iw_proto::app::v1::Arrangement {
                version,
                devices: Vec::new(),
            });
            model
        };
        let mut watch = SyncedWatch::default();
        assert_eq!(
            watch.update(&at(3, Link::Connected), true),
            None,
            "the first seen"
        );
        // Changed while the page was hidden: not when it is shown again.
        assert_eq!(watch.update(&at(4, Link::Connected), false), None);
        assert_eq!(watch.update(&at(4, Link::Connected), true), None);
        assert_eq!(
            watch.update(&at(5, Link::Connected), true),
            Some(ui!(keys::CANVAS_SYNCED, "1"))
        );
        // The service restarted: its versions count anew.
        assert_eq!(watch.update(&at(5, Link::Connecting), true), None);
        assert_eq!(watch.update(&at(1, Link::Connected), true), None);
        assert_eq!(
            watch.update(&at(2, Link::Connected), true),
            Some(ui!(keys::CANVAS_SYNCED, "1"))
        );
    }
}
