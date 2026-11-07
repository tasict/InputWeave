//! Waking another computer from sleep over the network (`WakeDevice`), from
//! what the background service says: whether it can wake devices at all
//! (`Status.wake_supported`: nothing of waking shows from one before it),
//! which ones it can wake now (`DeviceStatus.can_wake`, only while they are
//! offline) and whether one is being woken (`DeviceStatus.wake`). This
//! computer is never offered, nor a computer offline for now or left out
//! of the profile in use, nor any while this one is offline for now or the
//! profile in use is paused. The service sends the wake-up and
//! decides when it did not work; this says what the app shows of it.

use std::collections::HashSet;

use iw_proto::app::v1::{ConnectionState, DeviceStatus, WakeState};

use crate::offline;
use crate::profiles;
use crate::snapshot::Snapshot;
use crate::text::{UiText, keys};
use crate::ui;

/// The icon after the name of a computer that can be woken.
pub const MOON_ICON: &str = "weather-clear-night-symbolic";

/// Whether the service knows how to wake devices.
pub fn supported(model: &Snapshot) -> bool {
    model.status.wake_supported
}

/// Another device, from a service that wakes devices, that neither it
/// nor this one is offline for now, that the profile in use connects, and
/// while that profile is not paused.
fn other<'a>(model: &'a Snapshot, id: &str) -> Option<&'a DeviceStatus> {
    if !supported(model)
        || id == model.this_device_id()
        || offline::this_offline(model)
        || offline::is_offline(model, id)
        || profiles::paused(model)
        || profiles::not_in_profile(model, id)
    {
        return None;
    }
    model.device(id)
}

fn offline(d: &DeviceStatus) -> bool {
    d.state() != ConnectionState::Connected
}

/// Whether the service can wake `id` now, as it says.
pub fn can_wake(model: &Snapshot, id: &str) -> bool {
    other(model, id).is_some_and(|d| d.can_wake)
}

/// Whether `id` is being woken: a wake-up went to it less than two minutes
/// ago and it has not connected since.
pub fn waking(model: &Snapshot, id: &str) -> bool {
    other(model, id).is_some_and(|d| offline(d) && d.wake() == WakeState::Waking)
}

/// Whether the moon goes after its name (the arrangement, the devices
/// list): while it can be woken, being woken or not.
pub fn shows_moon(model: &Snapshot, id: &str) -> bool {
    can_wake(model, id)
}

/// The icon of the inspector's status row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    /// It can be woken.
    Moon,
    /// It is being woken.
    Spinner,
}

pub fn glyph(model: &Snapshot, id: &str) -> Option<Glyph> {
    if waking(model, id) {
        Some(Glyph::Spinner)
    } else if can_wake(model, id) {
        Some(Glyph::Moon)
    } else {
        None
    }
}

/// Wake, beside the status and in the arrangement's menu: `None` where it
/// is not offered, else whether it can be pressed. Not while the device is
/// being woken, nor while this app's own request for it is on its way
/// (`in_flight`); pressed again once it did not wake up, it tries again.
pub fn offer(model: &Snapshot, id: &str, in_flight: bool) -> Option<bool> {
    let d = other(model, id).filter(|d| d.can_wake)?;
    Some(d.wake() != WakeState::Waking && !in_flight)
}

/// The line under the status of an offline device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// It did not connect in the two minutes after the last wake-up: why it
    /// may not have, with a link to how to turn waking on
    /// ([`keys::WAKE_HOW_TO`], to [`keys::WAKE_HOW_TO_URL`]).
    NotWoken(UiText),
    /// It never told this computer how to wake it.
    NotToldYet(UiText),
}

pub fn note(model: &Snapshot, id: &str) -> Option<Note> {
    let d = other(model, id).filter(|d| offline(d))?;
    let name = model.name_of(id);
    if !d.can_wake {
        return Some(Note::NotToldYet(ui!(keys::WAKE_NOT_TOLD_YET, name)));
    }
    (d.wake() == WakeState::NotWoken).then(|| Note::NotWoken(ui!(keys::WAKE_NOT_WOKEN, name)))
}

/// The devices this app asked the service to wake and has no answer for
/// yet: Wake is off for them meanwhile.
#[derive(Debug, Default)]
pub struct Pending {
    sent: HashSet<String>,
}

impl Pending {
    /// A request for `id` goes; false when one is on its way already.
    pub fn begin(&mut self, id: &str) -> bool {
        self.sent.insert(id.to_owned())
    }

    /// The service answered the request for `id`, either way.
    pub fn end(&mut self, id: &str) {
        self.sent.remove(id);
    }

    pub fn in_flight(&self, id: &str) -> bool {
        self.sent.contains(id)
    }

    /// Forgets devices no longer in the status (unpaired).
    pub fn prune(&mut self, model: &Snapshot) {
        self.sent.retain(|id| model.device(id).is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Link;
    use crate::text::{Catalog, Language};
    use iw_proto::app::v1::Status;

    fn model(state: ConnectionState, can_wake: bool, wake: WakeState) -> Snapshot {
        Snapshot {
            link: Link::Connected,
            status: Status {
                this_device_id: "me".into(),
                this_device_name: "ubuntu-desk".into(),
                wake_supported: true,
                devices: vec![DeviceStatus {
                    id: "mini".into(),
                    name: "Mac mini".into(),
                    state: state as i32,
                    can_wake,
                    wake: wake as i32,
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    const OFFLINE: ConnectionState = ConnectionState::Offline;

    #[test]
    fn a_device_that_can_be_woken_is_offered_waking() {
        let m = model(OFFLINE, true, WakeState::None);
        assert!(can_wake(&m, "mini"));
        assert!(shows_moon(&m, "mini"));
        assert_eq!(glyph(&m, "mini"), Some(Glyph::Moon));
        assert_eq!(offer(&m, "mini", false), Some(true));
        // While this app's request is on its way: off.
        assert_eq!(offer(&m, "mini", true), Some(false));
        assert_eq!(note(&m, "mini"), None);
    }

    #[test]
    fn being_woken_turns_wake_off_and_spins() {
        let m = model(OFFLINE, true, WakeState::Waking);
        assert!(waking(&m, "mini"));
        assert!(shows_moon(&m, "mini"), "the moon stays after the name");
        assert_eq!(glyph(&m, "mini"), Some(Glyph::Spinner));
        assert_eq!(offer(&m, "mini", false), Some(false));
        assert_eq!(note(&m, "mini"), None);
        // Being woken while it is not said to be wakeable: it spins, with
        // nothing to press.
        let m = model(OFFLINE, false, WakeState::Waking);
        assert_eq!(glyph(&m, "mini"), Some(Glyph::Spinner));
        assert_eq!(offer(&m, "mini", false), None);
    }

    #[test]
    fn not_woken_says_why_and_tries_again() {
        let m = model(OFFLINE, true, WakeState::NotWoken);
        assert_eq!(glyph(&m, "mini"), Some(Glyph::Moon));
        assert_eq!(offer(&m, "mini", false), Some(true), "tries again");
        let Some(Note::NotWoken(text)) = note(&m, "mini") else {
            panic!("the note that it did not wake up");
        };
        assert_eq!(
            text.to_string(),
            "Mac mini didn’t wake up. Wake for network access may be off on it, or it may not be on a wired network."
        );
        let zh = Catalog::for_language(Language::ChineseTaiwan);
        assert_eq!(
            zh.render(&text),
            "「Mac mini」沒有醒來。它可能沒有打開網路喚醒，或沒有接在有線網路上。"
        );
        // The link, by the app's language.
        let en = Catalog::for_language(Language::English);
        assert_eq!(
            en.get(keys::WAKE_HOW_TO_URL),
            "https://inputweave.ctsite.cc/user-guide#wake-for-network-access"
        );
        assert_eq!(
            zh.get(keys::WAKE_HOW_TO_URL),
            "https://inputweave.ctsite.cc/zh-Hant/user-guide#wake-for-network-access"
        );
        assert_eq!(
            en.get(keys::WAKE_HOW_TO),
            "How to turn on wake for network access"
        );
        assert_eq!(zh.get(keys::WAKE_HOW_TO), "怎麼打開網路喚醒");
    }

    #[test]
    fn one_that_never_said_how_says_so() {
        for wake in [WakeState::None, WakeState::NotWoken] {
            let m = model(OFFLINE, false, wake);
            assert!(!can_wake(&m, "mini"));
            assert!(!shows_moon(&m, "mini"));
            assert_eq!(glyph(&m, "mini"), None);
            assert_eq!(offer(&m, "mini", false), None);
            let Some(Note::NotToldYet(text)) = note(&m, "mini") else {
                panic!("the note that it can be woken later");
            };
            assert_eq!(
                text.to_string(),
                "Mac mini hasn’t told this computer how to wake it yet. You can wake it after it connects again."
            );
            assert_eq!(
                Catalog::for_language(Language::ChineseTaiwan).render(&text),
                "「Mac mini」還沒告訴這台怎麼喚醒它；下次連上之後就能喚醒。"
            );
        }
    }

    #[test]
    fn nothing_for_a_connected_device_this_one_or_an_older_service() {
        let connected = model(ConnectionState::Connected, false, WakeState::None);
        assert_eq!(glyph(&connected, "mini"), None);
        assert_eq!(offer(&connected, "mini", false), None);
        assert_eq!(note(&connected, "mini"), None);
        assert!(!waking(&connected, "mini"));
        // Connected while still marked as being woken: not waking.
        let connected = model(ConnectionState::Connected, false, WakeState::Waking);
        assert!(!waking(&connected, "mini"));
        assert_eq!(glyph(&connected, "mini"), None);

        // This computer, even were it in the list.
        let mut this = model(OFFLINE, true, WakeState::NotWoken);
        this.status.this_device_id = "mini".into();
        assert!(!can_wake(&this, "mini"));
        assert_eq!(glyph(&this, "mini"), None);
        assert_eq!(offer(&this, "mini", false), None);
        assert_eq!(note(&this, "mini"), None);

        // A service from before waking.
        for (can, wake) in [(true, WakeState::Waking), (false, WakeState::None)] {
            let mut old = model(OFFLINE, can, wake);
            old.status.wake_supported = false;
            assert!(!supported(&old));
            assert!(!shows_moon(&old, "mini"));
            assert_eq!(glyph(&old, "mini"), None);
            assert_eq!(offer(&old, "mini", false), None);
            assert_eq!(note(&old, "mini"), None);
        }

        // A device not in the status.
        let m = model(OFFLINE, true, WakeState::None);
        assert_eq!(offer(&m, "gone", false), None);
        assert_eq!(note(&m, "gone"), None);
    }

    #[test]
    fn nothing_for_a_computer_offline_for_now_or_while_this_one_is() {
        for wake in [WakeState::None, WakeState::Waking, WakeState::NotWoken] {
            for can in [true, false] {
                let mut m = model(OFFLINE, can, wake);
                m.status.offline_supported = true;
                m.status.devices[0].offline_for_now = true;
                assert!(!can_wake(&m, "mini") && !waking(&m, "mini"));
                assert!(!shows_moon(&m, "mini"));
                assert_eq!(glyph(&m, "mini"), None);
                assert_eq!(offer(&m, "mini", false), None);
                assert_eq!(note(&m, "mini"), None, "no line it can't be woken");
                // This computer offline for now: every other one is just
                // offline.
                m.status.devices[0].offline_for_now = false;
                m.status.offline_for_now = true;
                assert!(!shows_moon(&m, "mini"));
                assert_eq!(glyph(&m, "mini"), None);
                assert_eq!(offer(&m, "mini", false), None);
                assert_eq!(note(&m, "mini"), None);
            }
        }
    }

    #[test]
    fn requests_on_their_way_are_kept_per_device_until_answered() {
        let mut pending = Pending::default();
        assert!(pending.begin("mini"));
        assert!(!pending.begin("mini"), "one at a time");
        assert!(pending.in_flight("mini"));
        assert!(!pending.in_flight("pc"));
        pending.end("mini");
        assert!(!pending.in_flight("mini"));
        // Unpaired while on its way: forgotten.
        let m = model(OFFLINE, true, WakeState::None);
        pending.begin("mini");
        pending.begin("gone");
        pending.prune(&m);
        assert!(pending.in_flight("mini"));
        assert!(!pending.in_flight("gone"));
        pending.end("gone");
        assert!(pending.in_flight("mini"));
    }
}
