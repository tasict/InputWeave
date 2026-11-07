//! Taking a computer offline for now (`SetOfflineForNow`), from what the
//! background service says: whether it knows of it at all
//! (`Status.offline_supported`: nothing of it shows from one before it),
//! which computers are offline for now and who took them offline
//! (`DeviceStatus.offline_for_now`, `offline_by_device_id`,
//! `offline_by_name`), whether this one is (`Status.offline_for_now`), and
//! which connected computers still run a version that knows nothing of it
//! (`DeviceStatus.offline_unaware`). The service keeps a computer offline
//! for now away from the others, and shows it, and every other computer
//! while this one is offline for now, as offline; this says what the app
//! adds to that.

use iw_proto::app::v1::{DeviceStatus, SetOfflineForNow, client_message::Command};

use crate::snapshot::Snapshot;
use crate::text::{UiText, keys};
use crate::ui;

/// The icon of a computer offline for now: after its name, at the end of
/// its row in the devices list and on its status in the inspector.
pub const ICON: &str = "network-offline-symbolic";

/// Whether the service knows computers offline for now.
pub fn supported(model: &Snapshot) -> bool {
    model.status.offline_supported
}

/// Whether this computer is offline for now: it neither controls the
/// others nor is controlled by them, and every other one shows offline.
pub fn this_offline(model: &Snapshot) -> bool {
    supported(model) && model.status.offline_for_now
}

/// Another device, from a service that knows computers offline for now.
fn other<'a>(model: &'a Snapshot, id: &str) -> Option<&'a DeviceStatus> {
    if !supported(model) || id == model.this_device_id() {
        return None;
    }
    model.device(id)
}

/// Whether `id`, another computer, is offline for now. While this one is,
/// the service shows none of the others so.
pub fn is_offline(model: &Snapshot, id: &str) -> bool {
    other(model, id).is_some_and(|d| d.offline_for_now)
}

/// Whether `by_id`, the computer that took one offline, is this one: never
/// when it is not known (empty).
pub fn by_this(model: &Snapshot, by_id: &str) -> bool {
    !by_id.is_empty() && by_id == model.this_device_id()
}

/// How a line says who took a computer offline.
struct Wording {
    /// This computer.
    here: &'static str,
    /// Another, by name.
    by: &'static str,
    /// Another this computer does not know.
    unknown: &'static str,
}

/// Who took a computer offline: this one ("Set on this computer."),
/// another, by the name the service gives it, what this computer calls it
/// ("Set by MacBook Pro."), or, with no name, one it does not know ("Set
/// by another computer."), whatever its id.
fn set_by(model: &Snapshot, by_id: &str, by_name: &str, wording: Wording) -> UiText {
    if by_this(model, by_id) {
        return ui!(wording.here);
    }
    if by_name.is_empty() {
        ui!(wording.unknown)
    } else {
        ui!(wording.by, by_name)
    }
}

/// The line under the status of another computer offline for now.
pub fn set_by_line(model: &Snapshot, id: &str) -> Option<UiText> {
    let d = other(model, id).filter(|d| d.offline_for_now)?;
    Some(set_by(
        model,
        &d.offline_by_device_id,
        &d.offline_by_name,
        Wording {
            here: keys::OFFLINE_SET_HERE,
            by: keys::OFFLINE_SET_BY,
            unknown: keys::OFFLINE_SET_BY_UNKNOWN,
        },
    ))
}

/// Under that, while `id` is offline for now and itself runs a version from
/// before computers offline for now: a warning for each other connected
/// computer on such a version, which still controls it, in the status's
/// order. A computer offline for now on a version that knows it keeps
/// those away itself, and one on an older version alone is kept away by
/// the others, so neither has a warning.
pub fn unaware_warnings(model: &Snapshot, id: &str) -> Vec<UiText> {
    if !other(model, id).is_some_and(|d| d.offline_for_now && d.offline_unaware) {
        return Vec::new();
    }
    model
        .status
        .devices
        .iter()
        .filter(|d| d.offline_unaware && d.id != id && d.id != model.this_device_id())
        .map(|d| {
            let name = model.name_of(&d.id);
            ui!(keys::OFFLINE_UNAWARE, name)
        })
        .collect()
}

/// Taking `id` offline for now, or bringing it back, in the inspector and
/// the arrangement's menu: `None` where it is not offered (this computer,
/// one the service does not list, from a service before it, and every
/// other computer while this one is offline for now: it can only bring
/// itself back); else whether `id` is offline for now, so the button
/// brings it back.
pub fn offer(model: &Snapshot, id: &str) -> Option<bool> {
    if this_offline(model) {
        return None;
    }
    other(model, id).map(|d| d.offline_for_now)
}

/// The label of the button or menu item `offer` gives.
pub fn offer_key(offline: bool) -> &'static str {
    if offline {
        keys::OFFLINE_BRING_BACK
    } else {
        keys::OFFLINE_TAKE
    }
}

/// The tray menu's item for this computer: `None` from a service before
/// computers offline for now; else its label and whether it takes this
/// computer offline (true) or brings it back.
pub fn this_item(model: &Snapshot) -> Option<(&'static str, bool)> {
    if !supported(model) {
        return None;
    }
    Some(if model.status.offline_for_now {
        (keys::OFFLINE_BRING_BACK, false)
    } else {
        (keys::MENU_TAKE_THIS_OFFLINE, true)
    })
}

/// The line under the status title while this computer is offline for
/// now: who set it, and what it means.
pub fn this_subtitle(model: &Snapshot) -> Option<UiText> {
    if !this_offline(model) {
        return None;
    }
    let status = &model.status;
    Some(set_by(
        model,
        &status.offline_by_device_id,
        &status.offline_by_name,
        Wording {
            here: keys::SUBTITLE_THIS_OFFLINE_HERE,
            by: keys::SUBTITLE_THIS_OFFLINE_BY,
            unknown: keys::SUBTITLE_THIS_OFFLINE_BY_UNKNOWN,
        },
    ))
}

/// Takes `id` offline for now (`offline`), or brings it back.
pub fn command(id: &str, offline: bool) -> Command {
    Command::SetOfflineForNow(SetOfflineForNow {
        device_id: id.to_owned(),
        offline,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Link;
    use crate::text::{Catalog, Language};
    use iw_proto::app::v1::{ConnectionState, Status};

    fn device(id: &str, name: &str, state: ConnectionState) -> DeviceStatus {
        DeviceStatus {
            id: id.into(),
            name: name.into(),
            state: state as i32,
            ..Default::default()
        }
    }

    /// The Mac mini offline for now, set by `by` (named "MacBook Pro"
    /// here), and the Studio PC connected.
    fn model(by: &str) -> Snapshot {
        let mut mini = device("mini", "Mac mini", ConnectionState::Offline);
        mini.offline_for_now = true;
        mini.offline_by_device_id = by.into();
        mini.offline_by_name = if by == "me" {
            "ubuntu-desk".into()
        } else {
            "MacBook Pro".into()
        };
        Snapshot {
            link: Link::Connected,
            status: Status {
                this_device_id: "me".into(),
                this_device_name: "ubuntu-desk".into(),
                offline_supported: true,
                devices: vec![
                    mini,
                    device("pc", "Studio PC", ConnectionState::Connected),
                    device("mbp", "MacBook Pro", ConnectionState::Connected),
                ],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn both(text: &UiText) -> (String, String) {
        let zh = Catalog::for_language(Language::ChineseTaiwan);
        (text.to_string(), zh.render(text))
    }

    fn pair(en: &str, zh: &str) -> (String, String) {
        (en.to_owned(), zh.to_owned())
    }

    #[test]
    fn another_computer_offline_for_now_says_who_set_it() {
        let m = model("mbp");
        assert!(supported(&m) && !this_offline(&m));
        assert!(is_offline(&m, "mini"));
        assert!(!is_offline(&m, "pc"));
        assert_eq!(
            set_by_line(&m, "mini").as_ref().map(both),
            Some(pair("Set by MacBook Pro.", "由「MacBook Pro」設定。"))
        );
        assert_eq!(set_by_line(&m, "pc"), None);
        // Set from here.
        let m = model("me");
        assert_eq!(
            set_by_line(&m, "mini").as_ref().map(both),
            Some(pair("Set on this computer.", "由這台設定。"))
        );
        // The name is the service's, whatever this computer lists by the
        // id.
        let mut m = model("mbp");
        m.status.devices[0].offline_by_name = "Studio".into();
        assert_eq!(
            set_by_line(&m, "mini").map(|t| t.to_string()),
            Some("Set by Studio.".into())
        );
    }

    #[test]
    fn a_computer_this_one_does_not_know_is_another_computer() {
        // Neither id nor name: never this computer.
        let mut m = model("mbp");
        m.status.devices[0].offline_by_device_id.clear();
        m.status.devices[0].offline_by_name.clear();
        assert_eq!(
            set_by_line(&m, "mini").as_ref().map(both),
            Some(pair("Set by another computer.", "由另一台電腦設定。"))
        );
        // An id, but no name (the service knows no name for it), whether
        // or not this computer lists one by that id.
        for by in ["gone", "mbp"] {
            m.status.devices[0].offline_by_device_id = by.into();
            assert_eq!(
                set_by_line(&m, "mini").map(|t| t.to_string()),
                Some("Set by another computer.".into()),
                "{by}"
            );
        }
        // A name with no id: the name.
        m.status.devices[0].offline_by_device_id.clear();
        m.status.devices[0].offline_by_name = "Old iMac".into();
        assert_eq!(
            set_by_line(&m, "mini").map(|t| t.to_string()),
            Some("Set by Old iMac.".into())
        );
        // An empty id is not this computer, even one whose own id is empty.
        let mut nobody = model("");
        nobody.status.this_device_id.clear();
        nobody.status.devices[0].offline_by_name.clear();
        assert!(!by_this(&nobody, ""));
        assert_eq!(
            set_by_line(&nobody, "mini").map(|t| t.to_string()),
            Some("Set by another computer.".into())
        );
        // This computer, taken offline by one it does not know, with or
        // without an id.
        let mut m = model("mbp");
        m.status.offline_for_now = true;
        for by in ["", "mbp"] {
            m.status.offline_by_device_id = by.into();
            assert_eq!(
                this_subtitle(&m).as_ref().map(both),
                Some(pair(
                    "Set by another computer. Your other computers don’t control this one or share the clipboard and files with it.",
                    "由另一台電腦設定。其他電腦不會控制這台，也不會和它同步剪貼簿或互傳檔案。"
                )),
                "{by}"
            );
        }
        m.status.offline_by_name = "Old iMac".into();
        assert_eq!(
            this_subtitle(&m).map(|t| t.to_string()),
            Some(
                "Set by Old iMac. Your other computers don’t control this one or share the clipboard and files with it."
                    .into()
            )
        );
    }

    #[test]
    fn computers_on_an_older_version_are_warned_of() {
        let mut m = model("me");
        assert!(unaware_warnings(&m, "mini").is_empty());
        // The Mac mini, offline for now, knows it: it keeps the Studio PC
        // on 1.0 away itself.
        m.status.devices[1].offline_unaware = true;
        assert!(unaware_warnings(&m, "mini").is_empty());
        // The Mac mini on 1.0 too: the Studio PC still controls it.
        m.status.devices[0].offline_unaware = true;
        let warnings = unaware_warnings(&m, "mini");
        assert_eq!(
            warnings.iter().map(both).collect::<Vec<_>>(),
            [pair(
                "Studio PC runs an older InputWeave that doesn’t know about this and still controls it. Update Studio PC to 1.1.",
                "「Studio PC」的 InputWeave 是舊版，不認得暫時離線，仍會控制它。請把「Studio PC」更新到 1.1。"
            )]
        );
        // Each other one, in the status's order.
        m.status.devices[2].offline_unaware = true;
        let names: Vec<String> = unaware_warnings(&m, "mini")
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names[0].starts_with("Studio PC runs"), "{names:?}");
        assert!(names[1].starts_with("MacBook Pro runs"), "{names:?}");
        // The Mac mini the only one on 1.0: the others keep off it.
        m.status.devices[1].offline_unaware = false;
        m.status.devices[2].offline_unaware = false;
        assert!(unaware_warnings(&m, "mini").is_empty());
        // Only for a computer offline for now.
        m.status.devices[1].offline_unaware = true;
        m.status.devices[0].offline_for_now = false;
        assert!(unaware_warnings(&m, "mini").is_empty());
        assert!(unaware_warnings(&m, "pc").is_empty());
        assert!(unaware_warnings(&m, "me").is_empty());
    }

    #[test]
    fn taking_offline_and_bringing_back_are_offered_for_other_computers() {
        let m = model("mbp");
        assert_eq!(offer(&m, "mini"), Some(true));
        assert_eq!(offer(&m, "pc"), Some(false));
        // Offline computers too.
        let mut asleep = m.clone();
        asleep.status.devices[1].state = ConnectionState::Offline as i32;
        assert_eq!(offer(&asleep, "pc"), Some(false));
        // Not this one, nor one the service does not list.
        assert_eq!(offer(&m, "me"), None);
        assert_eq!(offer(&m, "gone"), None);
        let said = |key: &'static str| both(&ui!(key));
        assert_eq!(said(offer_key(false)), pair("Take Offline", "暫時離線"));
        assert_eq!(said(offer_key(true)), pair("Bring Back Online", "恢復連線"));
        assert_eq!(
            command("mini", false),
            Command::SetOfflineForNow(SetOfflineForNow {
                device_id: "mini".into(),
                offline: false,
            })
        );
    }

    #[test]
    fn this_computer_offline_for_now_offers_only_coming_back() {
        let mut m = model("mbp");
        assert_eq!(this_item(&m), Some((keys::MENU_TAKE_THIS_OFFLINE, true)));
        assert_eq!(this_subtitle(&m), None);
        assert_eq!(
            both(&ui!(keys::MENU_TAKE_THIS_OFFLINE)),
            pair("Take This Computer Offline", "讓這台暫時離線")
        );
        // Taken offline by the MacBook Pro: every other computer is just
        // offline, and none can be taken offline or brought back from here.
        m.status.offline_for_now = true;
        m.status.offline_by_device_id = "mbp".into();
        m.status.offline_by_name = "MacBook Pro".into();
        for d in &mut m.status.devices {
            d.state = ConnectionState::Offline as i32;
            d.offline_for_now = false;
        }
        assert!(this_offline(&m));
        assert_eq!(this_item(&m), Some((keys::OFFLINE_BRING_BACK, false)));
        assert_eq!(offer(&m, "mini"), None);
        assert_eq!(offer(&m, "pc"), None);
        assert_eq!(
            this_subtitle(&m).as_ref().map(both),
            Some(pair(
                "Set by MacBook Pro. Your other computers don’t control this one or share the clipboard and files with it.",
                "由「MacBook Pro」設定。其他電腦不會控制這台，也不會和它同步剪貼簿或互傳檔案。"
            ))
        );
        // Taken offline from here.
        m.status.offline_by_device_id = "me".into();
        m.status.offline_by_name = "ubuntu-desk".into();
        assert_eq!(
            this_subtitle(&m).as_ref().map(both),
            Some(pair(
                "Set on this computer. Your other computers don’t control this one or share the clipboard and files with it.",
                "由這台設定。其他電腦不會控制這台，也不會和它同步剪貼簿或互傳檔案。"
            ))
        );
    }

    #[test]
    fn nothing_of_it_from_a_service_before_it() {
        let mut m = model("mbp");
        m.status.offline_supported = false;
        m.status.offline_for_now = true;
        m.status.devices[1].offline_unaware = true;
        assert!(!supported(&m) && !this_offline(&m));
        assert!(!is_offline(&m, "mini"));
        assert_eq!(set_by_line(&m, "mini"), None);
        assert!(unaware_warnings(&m, "mini").is_empty());
        assert_eq!(offer(&m, "mini"), None);
        assert_eq!(offer(&m, "pc"), None);
        assert_eq!(this_item(&m), None);
        assert_eq!(this_subtitle(&m), None);
    }

    #[test]
    fn the_explanation_reads_the_same_in_both_states() {
        assert_eq!(
            both(&ui!(keys::OFFLINE_EXPLAIN)),
            pair(
                "None of your computers control it or share the clipboard and files with it until someone brings it back. It can bring itself back too.",
                "所有電腦都不會控制它，也不會和它同步剪貼簿或互傳檔案，直到有人恢復連線。它自己也能恢復。"
            )
        );
        assert_eq!(
            both(&ui!(keys::CONNECTION_OFFLINE_FOR_NOW)),
            pair("Offline for now", "暫時離線")
        );
    }
}
