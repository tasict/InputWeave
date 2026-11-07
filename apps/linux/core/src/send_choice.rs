//! Which computer files go to when they are sent from the file manager
//! (`inputweave-app --send`, Dolphin's Actions › Send with InputWeave) or
//! with Send Files…: the computers that can take them, why the others
//! can't, and the words for the choice, as the macOS app's `SendTargets`
//! and the Windows app's `SendTargets`.

use iw_proto::app::v1::{ConnectionState, DeviceStatus};

use crate::device_text;
use crate::snapshot::Snapshot;
use crate::text::{UiText, keys};
use crate::ui;

/// Why files can't go to a computer now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// It is not connected.
    Offline,
    /// File transfer with it is turned off on this computer
    /// (`SetDeviceSharing`).
    FilesOff,
}

/// Why files can't go to `device` now, the first reason that applies;
/// `None` when they can.
pub fn unavailable(device: &DeviceStatus) -> Option<Unavailable> {
    if device.state() != ConnectionState::Connected {
        return Some(Unavailable::Offline);
    }
    if device.files_off {
        return Some(Unavailable::FilesOff);
    }
    None
}

/// What the picker says under a computer files can't go to.
pub fn reason(why: Unavailable) -> UiText {
    match why {
        Unavailable::Offline => ui!(keys::CONNECTION_OFFLINE),
        Unavailable::FilesOff => ui!(keys::SEND_REASON_FILES_OFF),
    }
}

/// A computer in the picker.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub id: String,
    pub name: String,
    pub os: String,
    /// "Connected · 3 ms" for one that can take files, else why not.
    pub status: UiText,
    pub available: bool,
}

/// Every other computer, those files can go to first, each group in the
/// service's order.
pub fn targets(model: &Snapshot) -> Vec<Target> {
    let (mut available, unavailable_ones): (Vec<Target>, Vec<Target>) = model
        .status
        .devices
        .iter()
        .map(|d| {
            let why = unavailable(d);
            Target {
                id: d.id.clone(),
                name: d.name.clone(),
                os: d.os.clone(),
                status: why.map_or_else(|| device_text::connected(d), reason),
                available: why.is_none(),
            }
        })
        .partition(|t| t.available);
    available.extend(unavailable_ones);
    available
}

/// What sending does with the computers there are.
#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    /// No computer is connected: say so, with only OK.
    Nowhere,
    /// Exactly one can take files: send to it at once.
    Only { id: String, name: String },
    /// Ask which, listing every other computer (`targets`): more than one
    /// can take files, or computers are connected but none can, each with
    /// why (and Send off).
    Ask,
}

pub fn choice(model: &Snapshot) -> Choice {
    if model.connected_devices().is_empty() {
        return Choice::Nowhere;
    }
    let mut available = model
        .status
        .devices
        .iter()
        .filter(|d| unavailable(d).is_none());
    match (available.next(), available.next()) {
        (Some(only), None) => Choice::Only {
            id: only.id.clone(),
            name: only.name.clone(),
        },
        _ => Choice::Ask,
    }
}

/// The computer the picker shows chosen as the computers change: the one
/// the user clicked (`clicked`) while files can go to it, which comes back
/// when it can again; else the one chosen until now (`shown`, by the user
/// or by default) while it can; else the first that can take files.
/// `None` when none can. So a computer that connects ahead of the one
/// shown never takes its place, and a default the choice moved away from
/// does not come back.
pub fn chosen<'a>(
    clicked: Option<&str>,
    shown: Option<&str>,
    targets: &'a [Target],
) -> Option<&'a Target> {
    let can =
        |id: Option<&str>| id.and_then(|id| targets.iter().find(|t| t.id == id && t.available));
    can(clicked)
        .or_else(|| can(shown))
        .or_else(|| targets.iter().find(|t| t.available))
}

/// A path as it is compared when requests are merged, as macOS's
/// standardized path: no trailing slash or repeated slashes, `.` parts
/// dropped and `..` taken back from the part before it, symbolic links
/// left as they are.
fn standardized(path: &str) -> String {
    use std::path::{Component, Path, PathBuf};
    let mut out = PathBuf::new();
    for part in Path::new(path).components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().into_owned()
}

/// The items of a request that arrives while the picker is open, added to
/// those it already has: each path once (by `standardized`), in the order
/// they came.
pub fn merged(open: &[String], more: &[String]) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> =
        open.iter().map(|p| standardized(p)).collect();
    let mut all = open.to_vec();
    for path in more {
        if seen.insert(standardized(path)) {
            all.push(path.clone());
        }
    }
    all
}

/// The item's name as the summary shows it: the last part of its path.
fn item_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

/// Under the picker's title: "Report.pdf", "Report.pdf and 1 more item",
/// "Report.pdf and 2 more items".
pub fn summary(paths: &[String]) -> UiText {
    let Some(first) = paths.first() else {
        return UiText::verbatim("");
    };
    let name = item_name(first);
    match paths.len() - 1 {
        0 => UiText::verbatim(name),
        1 => ui!(keys::SEND_SUMMARY_ONE_MORE, name),
        more => ui!(keys::SEND_SUMMARY_MORE, name, more.to_string()),
    }
}

/// Why files could not go to `name`, from the service's refusal of
/// SendFiles, as the other two apps say it: too much at once, items that
/// can't be sent (the first one's reason), file transfer turned off with
/// it, or else a check that it is connected.
pub fn refusal(code: Option<&str>, name: &str) -> UiText {
    match code {
        Some("too_many_items" | "too_large") => ui!(keys::SEND_FILES_LIMITS),
        Some(code @ ("not_a_file" | "unreadable" | "symlink")) => {
            crate::transfer_text::failed(code)
        }
        Some("files_off") => device_text::files_off(name),
        _ => ui!(keys::SEND_FILES_CHECK_CONNECTED, name),
    }
}

/// The HUD once the files are on their way: "Sending 2 items to Mac
/// mini…".
pub fn sending(items: usize, name: &str) -> UiText {
    if items == 1 {
        ui!(keys::HUD_SENDING_ONE, name)
    } else {
        ui!(keys::HUD_SENDING_MANY, items.to_string(), name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{Catalog, Language};
    use iw_proto::app::v1::Status;

    fn device(id: &str, state: ConnectionState, files_off: bool, rtt_ms: u32) -> DeviceStatus {
        DeviceStatus {
            id: id.into(),
            name: id.to_uppercase(),
            state: state as i32,
            files_off,
            rtt_ms,
            ..Default::default()
        }
    }

    fn model(devices: Vec<DeviceStatus>) -> Snapshot {
        Snapshot {
            status: Status {
                this_device_id: "me".into(),
                devices,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn render(text: &UiText, language: Language) -> String {
        Catalog::for_language(language).render(text)
    }

    #[test]
    fn the_reason_is_the_first_that_applies() {
        use ConnectionState::{Connected, Incompatible, Offline};
        assert_eq!(unavailable(&device("a", Connected, false, 3)), None);
        assert_eq!(
            unavailable(&device("a", Connected, true, 3)),
            Some(Unavailable::FilesOff)
        );
        assert_eq!(
            unavailable(&device("a", Offline, false, 0)),
            Some(Unavailable::Offline)
        );
        assert_eq!(
            unavailable(&device("a", Incompatible, false, 0)),
            Some(Unavailable::Offline),
            "a version that can't connect counts as offline"
        );
        assert_eq!(
            unavailable(&device("a", Offline, true, 0)),
            Some(Unavailable::Offline),
            "offline before files off"
        );
        for (why, en, zh) in [
            (Unavailable::Offline, "Offline", "離線"),
            (
                Unavailable::FilesOff,
                "File transfer with this computer is off",
                "沒有和這台互傳檔案",
            ),
        ] {
            assert_eq!(render(&reason(why), Language::English), en);
            assert_eq!(render(&reason(why), Language::ChineseTaiwan), zh);
        }
    }

    #[test]
    fn the_file_receivers_are_those_without_a_reason() {
        use ConnectionState::{Connected, Offline};
        let model = model(vec![
            device("a", Connected, false, 3),
            device("b", Connected, true, 4),
            device("c", Offline, false, 0),
        ]);
        let ids: Vec<&str> = model
            .file_receivers()
            .iter()
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(ids, ["a"]);
    }

    #[test]
    fn nowhere_one_or_a_choice() {
        use ConnectionState::{Connected, Offline};
        assert_eq!(choice(&model(vec![])), Choice::Nowhere);
        assert_eq!(
            choice(&model(vec![device("off", Offline, false, 0)])),
            Choice::Nowhere,
            "none connected"
        );
        // Connected, but none can take files: the picker says why, with
        // nothing to choose.
        let none_can = model(vec![
            device("off", Offline, false, 0),
            device("nofiles", Connected, true, 3),
        ]);
        assert_eq!(choice(&none_can), Choice::Ask, "one is connected");
        let listed = targets(&none_can);
        assert_eq!(
            listed
                .iter()
                .map(|t| (t.id.as_str(), t.available))
                .collect::<Vec<_>>(),
            [("off", false), ("nofiles", false)]
        );
        assert_eq!(chosen(None, None, &listed), None);
        assert_eq!(
            choice(&model(vec![
                device("off", Offline, false, 0),
                device("pc", Connected, false, 3),
            ])),
            Choice::Only {
                id: "pc".into(),
                name: "PC".into()
            }
        );
        let two_can = model(vec![
            device("off", Offline, false, 0),
            device("pc", Connected, false, 3),
            device("nofiles", Connected, true, 4),
            device("mac", Connected, false, 0),
        ]);
        assert_eq!(choice(&two_can), Choice::Ask, "two can take files");
        let rows: Vec<(String, bool, String)> = targets(&two_can)
            .iter()
            .map(|t| {
                (
                    t.id.clone(),
                    t.available,
                    render(&t.status, Language::English),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("pc".to_owned(), true, "Connected · 3 ms".to_owned()),
                ("mac".to_owned(), true, "Connected".to_owned()),
                ("off".to_owned(), false, "Offline".to_owned()),
                (
                    "nofiles".to_owned(),
                    false,
                    "File transfer with this computer is off".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn the_choice_stays_while_it_can_take_files() {
        use ConnectionState::{Connected, Offline};
        let with = |ahead: ConnectionState, mac: ConnectionState| {
            targets(&model(vec![
                device("ahead", ahead, false, 2),
                device("pc", Connected, false, 3),
                device("mac", mac, false, 4),
            ]))
        };
        let id = |t: Option<&Target>| t.map(|t| t.id.clone());
        // By default the first that can.
        let before = with(Offline, Connected);
        assert_eq!(id(chosen(None, None, &before)), Some("pc".into()));
        // A computer that connects ahead of it does not take its place.
        let after = with(Connected, Connected);
        assert_eq!(
            id(chosen(None, Some("pc"), &after)),
            Some("pc".into()),
            "the default stays"
        );
        assert_eq!(
            id(chosen(Some("mac"), Some("mac"), &after)),
            Some("mac".into())
        );
        // The one clicked goes offline: the first that can; it comes back
        // when it can again.
        let gone = with(Connected, Offline);
        assert_eq!(
            id(chosen(Some("mac"), Some("mac"), &gone)),
            Some("ahead".into())
        );
        assert_eq!(
            id(chosen(Some("mac"), Some("ahead"), &after)),
            Some("mac".into()),
            "a click comes back"
        );
        // A default the choice moved away from does not come back.
        let pc_gone = targets(&model(vec![
            device("ahead", Connected, false, 2),
            device("pc", Offline, false, 3),
        ]));
        assert_eq!(id(chosen(None, Some("pc"), &pc_gone)), Some("ahead".into()));
        assert_eq!(
            id(chosen(None, Some("ahead"), &after)),
            Some("ahead".into()),
            "pc, back, is not chosen again"
        );
        assert_eq!(
            id(chosen(Some("gone"), Some("gone"), &after)),
            Some("ahead".into())
        );
    }

    #[test]
    fn a_second_request_adds_its_items_once() {
        let paths = |list: &[&str]| list.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        assert_eq!(
            merged(&paths(&["/a", "/b"]), &paths(&["/b", "/c", "/a", "/d"])),
            paths(&["/a", "/b", "/c", "/d"])
        );
        assert_eq!(merged(&[], &paths(&["/a"])), paths(&["/a"]));
        // The same item written another way: a trailing slash, `.` and
        // `..` parts, repeated slashes; the first spelling stays.
        assert_eq!(
            merged(
                &paths(&["/home/me/Photos/"]),
                &paths(&[
                    "/home/me/Photos",
                    "/home/me/./Photos",
                    "/home/me//Photos",
                    "/home/me/x/../Photos",
                    "/home/me/Photos2",
                ])
            ),
            paths(&["/home/me/Photos/", "/home/me/Photos2"])
        );
        assert_eq!(standardized("/home/me/./a//b/../c/"), "/home/me/a/c");
        assert_eq!(standardized("/.."), "/");
        assert_eq!(standardized("a/../../b"), "../b");
    }

    #[test]
    fn the_summary_names_the_first_item() {
        let paths = |list: &[&str]| list.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        let en = |list: &[&str]| render(&summary(&paths(list)), Language::English);
        let zh = |list: &[&str]| render(&summary(&paths(list)), Language::ChineseTaiwan);
        assert_eq!(en(&["/home/me/Report.pdf"]), "Report.pdf");
        assert_eq!(en(&["/home/me/Photos/"]), "Photos", "a folder");
        assert_eq!(
            en(&["/home/me/Report.pdf", "/home/me/Photos"]),
            "Report.pdf and 1 more item"
        );
        assert_eq!(
            en(&["/home/me/Report.pdf", "/a", "/b"]),
            "Report.pdf and 2 more items"
        );
        assert_eq!(
            zh(&["/home/me/Report.pdf", "/home/me/Photos"]),
            "Report.pdf 與另外 1 個項目"
        );
        assert_eq!(
            zh(&["/home/me/Report.pdf", "/a", "/b"]),
            "Report.pdf 與另外 2 個項目"
        );
        assert_eq!(en(&["/"]), "/");
        assert_eq!(en(&[]), "");
    }

    #[test]
    fn a_refusal_says_why() {
        let en = |code: Option<&str>| render(&refusal(code, "Mac mini"), Language::English);
        let limits = "Up to 1,000 items, 10,000 files or 100 GB can be sent at once. Send them in smaller batches.";
        assert_eq!(en(Some("too_many_items")), limits);
        assert_eq!(en(Some("too_large")), limits);
        assert_eq!(en(Some("not_a_file")), "This is not a file or a folder.");
        assert_eq!(en(Some("unreadable")), "The file could not be read.");
        assert_eq!(
            en(Some("symlink")),
            "This is a shortcut (symbolic link) and is not sent."
        );
        assert_eq!(
            en(Some("files_off")),
            "Files with “Mac mini” are turned off."
        );
        assert_eq!(
            en(Some("unknown_device")),
            "Check that Mac mini is connected, then try again."
        );
        assert_eq!(
            en(None),
            "Check that Mac mini is connected, then try again.",
            "no answer"
        );
        assert_eq!(
            render(
                &refusal(Some("too_large"), "Mac mini"),
                Language::ChineseTaiwan
            ),
            "一次最多傳 1,000 個項目、10,000 個檔案或 100 GB。請分批傳送。"
        );
    }

    #[test]
    fn the_hud_says_how_many_and_where() {
        assert_eq!(
            render(&sending(2, "Studio PC"), Language::English),
            "Sending 2 items to Studio PC…"
        );
        assert_eq!(
            render(&sending(1, "Studio PC"), Language::English),
            "Sending 1 item to Studio PC…"
        );
        assert_eq!(
            render(&sending(2, "Windows 桌機"), Language::ChineseTaiwan),
            "正在傳送 2 個項目到「Windows 桌機」…"
        );
        assert_eq!(
            render(&sending(1, "Mac mini"), Language::ChineseTaiwan),
            "正在傳送 1 個項目到「Mac mini」…"
        );
    }
}
