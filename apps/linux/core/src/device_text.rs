//! The words about devices: connection state, system, screens, the key
//! swap (the Windows app's `DeviceText`).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iw_proto::app::v1::{ConnectionState, DeviceStatus, Screen, WakeState};

use crate::clipboard_text::{AgoStyle, ago};
use crate::offline;
use crate::profiles;
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

/// A device's status: this computer, that another one is offline for now
/// (before anything else, with no latency), that the profile in use leaves
/// it out, how it is connected, or that it is offline (`offline`); while
/// this computer is offline for now or the profile in use is paused, every
/// other one is just offline.
pub fn status(model: &Snapshot, id: &str, now: SystemTime) -> UiText {
    if id == model.this_device_id() {
        return ui!(keys::CONNECTION_THIS_COMPUTER);
    }
    if offline::is_offline(model, id) {
        return ui!(keys::CONNECTION_OFFLINE_FOR_NOW);
    }
    if profiles::not_in_profile(model, id) {
        return ui!(keys::CONNECTION_NOT_IN_PROFILE);
    }
    if offline::this_offline(model) || profiles::paused(model) {
        return ui!(keys::CONNECTION_OFFLINE);
    }
    match model.device(id) {
        Some(d) if d.state() == ConnectionState::Connected => connected(d),
        Some(d) => offline(model, d, now),
        None => ui!(keys::CONNECTION_OFFLINE),
    }
}

/// "Offline", from a service that can wake devices "Waking…" while one is
/// being woken, and "Offline · 2 hours ago" for one it can wake that went
/// offline then (the time it is not told: just "Offline").
pub fn offline(model: &Snapshot, d: &DeviceStatus, now: SystemTime) -> UiText {
    if model.status.wake_supported {
        if d.wake() == WakeState::Waking {
            return ui!(keys::CONNECTION_WAKING);
        }
        if d.can_wake && d.offline_since_unix_ms != 0 {
            let since =
                UNIX_EPOCH + Duration::from_millis(d.offline_since_unix_ms.min(u64::MAX / 2));
            let elapsed = now.duration_since(since).unwrap_or_default();
            return ui!(keys::CONNECTION_OFFLINE_AGO, ago(elapsed, AgoStyle::Full));
        }
    }
    ui!(keys::CONNECTION_OFFLINE)
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

/// Why files cannot go to the device now: as the picker of where files go
/// says it (`send_choice::unavailable`), the profile in use turning files
/// off said as for the device itself.
pub fn cannot_receive(model: &Snapshot, id: &str, compact: bool) -> Option<UiText> {
    if id == model.this_device_id() {
        return None;
    }
    if offline::is_offline(model, id) {
        return Some(ui!(keys::CONNECTION_OFFLINE_FOR_NOW));
    }
    if profiles::not_in_profile(model, id) {
        return Some(ui!(keys::CONNECTION_NOT_IN_PROFILE));
    }
    if model.files_off(id) || (profiles::files_off(model) && is_connected(model, id)) {
        return Some(if compact {
            ui!(keys::DEVICE_FILES_OFF_SHORT)
        } else {
            files_off(&model.name_of(id))
        });
    }
    Some(ui!(keys::CONNECTION_OFFLINE))
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
        let now = SystemTime::now();
        assert_eq!(status(&model, "me", now).to_string(), "This computer");
        assert_eq!(status(&model, "mac", now).to_string(), "Connected · 2 ms");
        assert_eq!(status(&model, "gone", now).to_string(), "Offline");
        assert_eq!(
            cannot_receive(&model, "mac", true).unwrap().to_string(),
            "Files turned off"
        );
    }

    #[test]
    fn offline_devices_say_since_when_and_being_woken() {
        use crate::text::{Catalog, Language};
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let two_hours_ago = (1_000_000 - 7_200) * 1000;
        let mut model = Snapshot {
            link: Link::Connected,
            status: Status {
                this_device_id: "me".into(),
                wake_supported: true,
                devices: vec![DeviceStatus {
                    id: "mini".into(),
                    name: "Mac mini".into(),
                    state: ConnectionState::Offline as i32,
                    can_wake: true,
                    offline_since_unix_ms: two_hours_ago,
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let zh = Catalog::for_language(Language::ChineseTaiwan);
        let said = |model: &Snapshot| {
            let text = status(model, "mini", now);
            (text.to_string(), zh.render(&text))
        };
        let pair = |en: &str, zh: &str| (en.to_owned(), zh.to_owned());
        assert_eq!(
            said(&model),
            pair("Offline · 2 hours ago", "離線・2 小時前")
        );
        // Gone a moment ago, or later than this computer's clock says.
        model.status.devices[0].offline_since_unix_ms = 999_990_000;
        assert_eq!(said(&model), pair("Offline · just now", "離線・剛剛"));
        model.status.devices[0].offline_since_unix_ms = 1_000_060_000;
        assert_eq!(said(&model), pair("Offline · just now", "離線・剛剛"));
        // Since when not known.
        model.status.devices[0].offline_since_unix_ms = 0;
        assert_eq!(said(&model), pair("Offline", "離線"));
        // Being woken, whether or not it says how.
        model.status.devices[0].offline_since_unix_ms = two_hours_ago;
        model.status.devices[0].wake = WakeState::Waking as i32;
        assert_eq!(said(&model), pair("Waking…", "正在喚醒…"));
        model.status.devices[0].can_wake = false;
        assert_eq!(said(&model), pair("Waking…", "正在喚醒…"));
        // Not woken: offline as before.
        model.status.devices[0].can_wake = true;
        model.status.devices[0].wake = WakeState::NotWoken as i32;
        assert_eq!(
            said(&model),
            pair("Offline · 2 hours ago", "離線・2 小時前")
        );
        // One it cannot wake: just offline.
        model.status.devices[0].can_wake = false;
        assert_eq!(said(&model), pair("Offline", "離線"));
        // Connected: as ever.
        model.status.devices[0].state = ConnectionState::Connected as i32;
        model.status.devices[0].rtt_ms = 3;
        assert_eq!(said(&model), pair("Connected · 3 ms", "已連線・3 ms"));
        // A service before waking: nothing of it.
        model.status.devices[0].state = ConnectionState::Offline as i32;
        model.status.devices[0].can_wake = true;
        model.status.devices[0].wake = WakeState::Waking as i32;
        model.status.wake_supported = false;
        assert_eq!(said(&model), pair("Offline", "離線"));
    }

    #[test]
    fn offline_for_now_comes_before_everything_else() {
        use crate::text::{Catalog, Language};
        let now = SystemTime::now();
        let mut model = Snapshot {
            link: Link::Connected,
            status: Status {
                this_device_id: "me".into(),
                wake_supported: true,
                offline_supported: true,
                devices: vec![DeviceStatus {
                    id: "mini".into(),
                    name: "Mac mini".into(),
                    state: ConnectionState::Offline as i32,
                    offline_for_now: true,
                    offline_by_device_id: "me".into(),
                    // Whatever else it says, as an older service might.
                    wake: WakeState::Waking as i32,
                    sharing_paused: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let zh = Catalog::for_language(Language::ChineseTaiwan);
        let said = |model: &Snapshot| {
            let text = status(model, "mini", now);
            (text.to_string(), zh.render(&text))
        };
        let pair = |en: &str, zh: &str| (en.to_owned(), zh.to_owned());
        assert_eq!(said(&model), pair("Offline for now", "暫時離線"));
        assert_eq!(
            cannot_receive(&model, "mini", true).map(|t| t.to_string()),
            Some("Offline for now".into())
        );
        // Connected all the same (the service keeps a connection to bring
        // it back): still offline for now, with no latency.
        model.status.devices[0].state = ConnectionState::Connected as i32;
        model.status.devices[0].rtt_ms = 3;
        assert_eq!(said(&model), pair("Offline for now", "暫時離線"));
        // This computer offline for now: every other one is just offline.
        model.status.devices[0].offline_for_now = false;
        model.status.devices[0].state = ConnectionState::Offline as i32;
        model.status.offline_for_now = true;
        assert_eq!(said(&model), pair("Offline", "離線"));
        // From a service before it: as before.
        model.status.offline_supported = false;
        model.status.offline_for_now = false;
        model.status.devices[0].offline_for_now = true;
        model.status.devices[0].wake = WakeState::None as i32;
        assert_eq!(said(&model), pair("Offline", "離線"));
        assert_eq!(
            cannot_receive(&model, "mini", true).map(|t| t.to_string()),
            Some("Offline".into())
        );
    }

    #[test]
    fn not_in_this_profile_comes_after_offline_for_now() {
        use crate::sample;
        use crate::text::{Catalog, Language};
        let now = SystemTime::now();
        let zh = Catalog::for_language(Language::ChineseTaiwan);
        let said = |model: &Snapshot, id: &str| {
            let text = status(model, id, now);
            (text.to_string(), zh.render(&text))
        };
        let pair = |en: &str, zh: &str| (en.to_owned(), zh.to_owned());
        let mut model = sample::not_in_profile(now);
        assert_eq!(
            said(&model, "pc"),
            pair("Not in this profile", "不在這個情境")
        );
        assert_eq!(
            cannot_receive(&model, "pc", true).map(|t| t.to_string()),
            Some("Not in this profile".into())
        );
        assert_eq!(
            said(&model, "mac"),
            pair("Connected · 3 ms", "已連線・3 ms")
        );
        // Offline for now comes first, as the service says only one.
        model.status.devices[1].offline_for_now = true;
        assert_eq!(said(&model, "pc"), pair("Offline for now", "暫時離線"));
        model.status.devices[1].offline_for_now = false;
        // The profile in use turning files off: every connected computer
        // as one files are turned off with; one offline is offline.
        for profile in &mut model.status.profiles {
            profile.files = false;
        }
        assert_eq!(
            cannot_receive(&model, "mac", true),
            Some(ui!(keys::DEVICE_FILES_OFF_SHORT))
        );
        assert_eq!(
            cannot_receive(&model, "laptop", true),
            Some(ui!(keys::CONNECTION_OFFLINE))
        );
        // The profile in use paused: every other computer just offline,
        // whatever else it says (the ThinkPad could be woken otherwise).
        let paused = sample::profile_paused(now, false);
        for id in ["mac", "pc", "laptop"] {
            assert_eq!(said(&paused, id), pair("Offline", "離線"), "{id}");
        }
        let mut wakeable = paused.clone();
        wakeable.status.devices[2].can_wake = true;
        assert_eq!(said(&wakeable, "laptop"), pair("Offline", "離線"));
        // A service before profiles: as ever.
        let mut old = sample::not_in_profile(now);
        old.status.profiles_supported = false;
        assert_eq!(said(&old, "pc"), pair("Offline", "離線"));
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
