//! Profiles: named sets of settings the background service picks by the
//! network this computer is on (Automatic), or that the user picks
//! (`SelectProfile`): whether this computer shares at all, which computers
//! it connects, whether the clipboard and files go, and the networks each
//! one is used on (`Status.profiles`, `profile_id`, `profile_chosen`,
//! `network`). The service decides which one is in use and keeps the
//! computers it leaves out offline (`DeviceStatus.not_in_profile`); this
//! says what the app shows of it, and makes the commands that change it.
//! From a service before profiles (`Status.profiles_supported`) none of it
//! shows.

use iw_proto::app::v1::{
    AddNetworkToProfile, AddProfile, DeviceInProfile, EditProfile, Network, NetworkKind, Profile,
    ProfileKind, RemoveNetwork, RemoveProfile, SelectProfile, client_message::Command,
};

use crate::offline;
use crate::snapshot::Snapshot;
use crate::text::{UiText, keys};
use crate::ui;

/// The Profiles page's icon in the sidebar: pages stacked on each other.
pub const ICON: &str = "view-paged-symbolic";

/// Whether the service knows profiles.
pub fn supported(model: &Snapshot) -> bool {
    model.status.profiles_supported
}

/// The profile in use.
pub fn in_use(model: &Snapshot) -> Option<&Profile> {
    if !supported(model) {
        return None;
    }
    let id = &model.status.profile_id;
    model.status.profiles.iter().find(|p| p.id == *id)
}

/// Whether the profile in use is paused: this computer neither shows up on
/// the network nor connects to any computer, and every other one is just
/// offline.
pub fn paused(model: &Snapshot) -> bool {
    in_use(model).is_some_and(|p| p.paused)
}

/// Whether the profile in use turns files off with every computer.
pub fn files_off(model: &Snapshot) -> bool {
    in_use(model).is_some_and(|p| !p.files)
}

/// Whether `id`, another computer, is one the profile in use does not
/// connect. The service says so only while that is all there is to say:
/// not while it is offline for now, nor while this one is or the profile in
/// use is paused.
pub fn not_in_profile(model: &Snapshot, id: &str) -> bool {
    supported(model)
        && id != model.this_device_id()
        && model.device(id).is_some_and(|d| d.not_in_profile)
}

/// Whether the arrangement's menu of `id`, another computer, has only
/// Rename…: while it is not in the profile in use, and for every other
/// computer while the profile in use is paused or this one is offline for
/// now. This computer's menu never changes.
pub fn rename_only(model: &Snapshot, id: &str) -> bool {
    id != model.this_device_id()
        && (offline::this_offline(model) || paused(model) || not_in_profile(model, id))
}

/// Default and Other networks: neither renamed nor removed.
pub fn is_fixed(profile: &Profile) -> bool {
    profile.kind() != ProfileKind::Custom
}

/// A profile's name: the user's own for theirs; the service gives the two
/// fixed ones none, and the app names them.
pub fn name(profile: &Profile) -> UiText {
    match profile.kind() {
        ProfileKind::Default => ui!(keys::PROFILE_DEFAULT),
        ProfileKind::OtherNetworks => ui!(keys::PROFILE_OTHER_NETWORKS),
        ProfileKind::Custom => UiText::verbatim(profile.name.clone()),
    }
}

/// The profile `id`.
pub fn profile<'a>(model: &'a Snapshot, id: &str) -> Option<&'a Profile> {
    model.status.profiles.iter().find(|p| p.id == id)
}

/// The icon before a network: Wi-Fi, wired, or a VPN's.
pub fn network_icon(network: &Network) -> &'static str {
    match network.kind() {
        NetworkKind::Wifi => "network-wireless-symbolic",
        NetworkKind::Vpn => "network-vpn-symbolic",
        NetworkKind::Wired | NetworkKind::Unspecified => "network-wired-symbolic",
    }
}

/// What a network is called: the name the service gives it (its Wi-Fi
/// name, as the service reads it from NetworkManager); else by its router,
/// "Router 192.168.1.1"; else by its kind, "Wi-Fi network", "Wired
/// network" (also for a kind the service does not say, as its icon) or
/// "VPN".
pub fn network_name(network: &Network) -> UiText {
    if !network.name.is_empty() {
        UiText::verbatim(network.name.clone())
    } else if !network.router.is_empty() {
        ui!(keys::NETWORK_ROUTER, network.router.clone())
    } else {
        match network.kind() {
            NetworkKind::Wifi => ui!(keys::NETWORK_WIFI),
            NetworkKind::Vpn => ui!(keys::NETWORK_VPN),
            NetworkKind::Wired | NetworkKind::Unspecified => ui!(keys::NETWORK_WIRED),
        }
    }
}

/// The line under a network's name: its router, where the name is not
/// that already.
pub fn network_detail(network: &Network) -> Option<UiText> {
    (!network.name.is_empty() && !network.router.is_empty())
        .then(|| ui!(keys::NETWORK_ROUTER, network.router.clone()))
}

/// "Current network: Office-5G", or that this computer is on none.
pub fn current_network(model: &Snapshot) -> UiText {
    match &model.status.network {
        Some(network) => ui!(keys::PROFILE_CURRENT_NETWORK, network_name(network)),
        None => ui!(keys::PROFILE_NO_NETWORK),
    }
}

/// One choice of the profile to use: Automatic (an empty id), or a
/// profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub id: String,
    pub label: UiText,
    /// The one chosen now: Automatic while the service picks by the
    /// network, else the profile the user chose.
    pub checked: bool,
}

/// The choices of the Profiles page's "Profile in use": "Automatic (now:
/// Office)" while the service picks by the network; once the user chose a
/// profile, what Automatic would pick is not known, so "Automatic" alone.
/// Then every profile, in the service's order.
pub fn choices(model: &Snapshot) -> Vec<Choice> {
    let automatic = match in_use(model) {
        Some(profile) if !model.status.profile_chosen => {
            ui!(keys::PROFILE_AUTOMATIC_NOW, name(profile))
        }
        _ => ui!(keys::PROFILE_AUTOMATIC),
    };
    menu_with(model, automatic)
}

/// The tray menu's Profile submenu: Automatic, then every profile, the one
/// chosen checked.
pub fn menu(model: &Snapshot) -> Vec<Choice> {
    menu_with(model, ui!(keys::PROFILE_AUTOMATIC))
}

fn menu_with(model: &Snapshot, automatic: UiText) -> Vec<Choice> {
    if !supported(model) {
        return Vec::new();
    }
    let status = &model.status;
    let chosen = status.profile_chosen && status.profiles.iter().any(|p| p.id == status.profile_id);
    let mut all = vec![Choice {
        id: String::new(),
        label: automatic,
        checked: !chosen,
    }];
    all.extend(status.profiles.iter().map(|p| Choice {
        id: p.id.clone(),
        label: name(p),
        checked: chosen && p.id == status.profile_id,
    }));
    all
}

/// A profile the network this computer is on can be added to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub id: String,
    pub name: UiText,
    /// It lists the network already.
    pub has_network: bool,
}

/// The profiles the network this computer is on can be added to ("Add to
/// Profile", "Add This Network To…"): every one but Other networks, which
/// lists none. None while this computer is on no network.
pub fn network_targets(model: &Snapshot) -> Vec<Target> {
    let Some(network) = model.status.network.as_ref().filter(|_| supported(model)) else {
        return Vec::new();
    };
    model
        .status
        .profiles
        .iter()
        .filter(|p| p.kind() != ProfileKind::OtherNetworks)
        .map(|p| Target {
            id: p.id.clone(),
            name: name(p),
            has_network: p.networks.iter().any(|n| n.id == network.id),
        })
        .collect()
}

/// Whether the network this computer is on can be added to a profile now
/// (AddNetworkToProfile): there is one, and the service knows it by more
/// than its router's address, which it does not for a few seconds after
/// joining it (`Network.resolved`). Until then every control that adds it
/// is off, with nothing more said.
pub fn can_add_network(model: &Snapshot) -> bool {
    supported(model) && model.status.network.as_ref().is_some_and(|n| n.resolved)
}

/// Whether "Add Current Network" adds anything to `profile` now: the
/// network can be added (`can_add_network`), the profile lists networks
/// (not Other networks), and not that one already.
pub fn can_add_current(model: &Snapshot, profile: &Profile) -> bool {
    let Some(network) = model
        .status
        .network
        .as_ref()
        .filter(|_| can_add_network(model))
    else {
        return false;
    };
    profile.kind() != ProfileKind::OtherNetworks
        && !profile.networks.iter().any(|n| n.id == network.id)
}

/// Whether `profile` connects `device`: every trusted computer it does not
/// leave out.
pub fn connects(profile: &Profile, device: &str) -> bool {
    !profile.off_device_ids.iter().any(|d| d == device)
}

/// The profile in use after the status's title, as the other two apps'
/// capsule: from a service that knows profiles, while it is reached, and
/// not after the paused profile's own title, which names it already.
pub fn capsule(model: &Snapshot) -> Option<UiText> {
    if !model.connected() || paused(model) {
        return None;
    }
    in_use(model).map(name)
}

/// The status's title while the profile in use is paused, before anything
/// else: "Other networks: sharing paused".
pub fn paused_title(model: &Snapshot) -> Option<UiText> {
    in_use(model)
        .filter(|p| p.paused)
        .map(|p| ui!(keys::TITLE_PROFILE_PAUSED, name(p)))
}

/// Whether the paused profile in use is Other networks, picked by the
/// service for a network in no profile: the line under the title says so,
/// and the network can be added to another profile from there. Not when
/// the user chose it, nor while this computer is on no network.
pub fn unassigned_network(model: &Snapshot) -> bool {
    in_use(model).is_some_and(|p| {
        p.paused
            && p.kind() == ProfileKind::OtherNetworks
            && !model.status.profile_chosen
            && model.status.network.is_some()
    })
}

/// The line under the paused profile's title: that the network is in no
/// profile, or nothing.
pub fn paused_subtitle(model: &Snapshot) -> Option<UiText> {
    unassigned_network(model).then(|| ui!(keys::PROFILE_NETWORK_NOT_IN_ANY))
}

/// Every profile's id, in the service's order.
pub fn ids(model: &Snapshot) -> Vec<String> {
    model.status.profiles.iter().map(|p| p.id.clone()).collect()
}

/// The profile `model` lists that was not among `before`: the one just
/// added.
pub fn added(before: &[String], model: &Snapshot) -> Option<String> {
    model
        .status
        .profiles
        .iter()
        .find(|p| !before.contains(&p.id))
        .map(|p| p.id.clone())
}

/// What the name field says when the service refused a new name with
/// `code`: that another profile has it (`name_taken`, which also covers
/// the two fixed profiles' names); `None` for any other refusal, which is
/// said as every other is.
pub fn rename_refused(code: Option<&str>) -> Option<UiText> {
    (code == Some("name_taken")).then(|| ui!(keys::PROFILE_NAME_TAKEN))
}

/// Adds a profile named `name` (the service makes it unique).
pub fn add(name: &str) -> Command {
    Command::AddProfile(AddProfile {
        name: name.to_owned(),
    })
}

/// Uses `id` whatever the network; an empty id goes back to Automatic.
pub fn select(id: &str) -> Command {
    Command::SelectProfile(SelectProfile {
        profile_id: id.to_owned(),
    })
}

pub fn remove(id: &str) -> Command {
    Command::RemoveProfile(RemoveProfile {
        profile_id: id.to_owned(),
    })
}

/// A change to profile `id` with nothing set yet.
fn edit(id: &str) -> EditProfile {
    EditProfile {
        profile_id: id.to_owned(),
        ..Default::default()
    }
}

pub fn rename(id: &str, name: &str) -> Command {
    Command::EditProfile(EditProfile {
        name: Some(name.to_owned()),
        ..edit(id)
    })
}

pub fn set_paused(id: &str, paused: bool) -> Command {
    Command::EditProfile(EditProfile {
        paused: Some(paused),
        ..edit(id)
    })
}

pub fn set_clipboard(id: &str, on: bool) -> Command {
    Command::EditProfile(EditProfile {
        clipboard: Some(on),
        ..edit(id)
    })
}

pub fn set_files(id: &str, on: bool) -> Command {
    Command::EditProfile(EditProfile {
        files: Some(on),
        ..edit(id)
    })
}

/// Connects `device` in profile `id`, or leaves it out.
pub fn set_connect(id: &str, device: &str, connect: bool) -> Command {
    Command::EditProfile(EditProfile {
        devices: vec![DeviceInProfile {
            device_id: device.to_owned(),
            connect,
        }],
        ..edit(id)
    })
}

/// Adds the network this computer is on to profile `id`, taking it from
/// any other.
pub fn add_network(id: &str) -> Command {
    Command::AddNetworkToProfile(AddNetworkToProfile {
        profile_id: id.to_owned(),
    })
}

pub fn remove_network(network_id: &str) -> Command {
    Command::RemoveNetwork(RemoveNetwork {
        network_id: network_id.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample;
    use crate::snapshot::Link;
    use crate::text::{Catalog, Language};
    use iw_proto::app::v1::{ConnectionState, DeviceStatus, Status};
    use std::time::SystemTime;

    fn both(text: &UiText) -> (String, String) {
        let zh = Catalog::for_language(Language::ChineseTaiwan);
        (text.to_string(), zh.render(text))
    }

    fn pair(en: &str, zh: &str) -> (String, String) {
        (en.to_owned(), zh.to_owned())
    }

    fn model() -> Snapshot {
        sample::snapshot(SystemTime::now())
    }

    /// A network this computer may be on, known by more than its router's
    /// address.
    fn network(name: &str, kind: NetworkKind, router: &str) -> Network {
        Network {
            id: format!("n-{name}-{router}"),
            name: name.into(),
            kind: kind as i32,
            router: router.into(),
            resolved: true,
        }
    }

    #[test]
    fn the_two_fixed_profiles_are_named_by_the_app() {
        let m = model();
        let names: Vec<(String, String)> =
            m.status.profiles.iter().map(|p| both(&name(p))).collect();
        assert_eq!(
            names,
            [
                pair("Default", "預設"),
                pair("Office", "Office"),
                pair("Other networks", "其他網路")
            ]
        );
        let fixed: Vec<bool> = m.status.profiles.iter().map(is_fixed).collect();
        assert_eq!(fixed, [true, false, true]);
        assert_eq!(
            both(&ui!(keys::PROFILE_NEW_NAME)),
            pair("New Profile", "新的情境")
        );
    }

    #[test]
    fn networks_are_called_by_name_router_or_vpn() {
        let wifi = network("Office-5G", NetworkKind::Wifi, "192.168.1.1");
        assert_eq!(both(&network_name(&wifi)), pair("Office-5G", "Office-5G"));
        assert_eq!(
            network_detail(&wifi).as_ref().map(both),
            Some(pair("Router 192.168.1.1", "路由器 192.168.1.1"))
        );
        assert_eq!(network_icon(&wifi), "network-wireless-symbolic");
        // No name: the router is the name, with no second line.
        let wired = network("", NetworkKind::Wired, "172.16.0.1");
        assert_eq!(
            both(&network_name(&wired)),
            pair("Router 172.16.0.1", "路由器 172.16.0.1")
        );
        assert_eq!(network_detail(&wired), None);
        assert_eq!(network_icon(&wired), "network-wired-symbolic");
        // A VPN with neither.
        let vpn = network("", NetworkKind::Vpn, "");
        assert_eq!(both(&network_name(&vpn)), pair("VPN", "VPN"));
        assert_eq!(network_detail(&vpn), None);
        assert_eq!(network_icon(&vpn), "network-vpn-symbolic");
        // A named one with no router.
        let named = network("Work", NetworkKind::Vpn, "");
        assert_eq!(network_name(&named).to_string(), "Work");
        assert_eq!(network_detail(&named), None);
    }

    #[test]
    fn a_network_with_neither_name_nor_router_is_called_by_its_kind() {
        for (kind, en, zh) in [
            (NetworkKind::Wired, "Wired network", "有線網路"),
            (NetworkKind::Wifi, "Wi-Fi network", "Wi-Fi 網路"),
            (NetworkKind::Vpn, "VPN", "VPN"),
            (NetworkKind::Unspecified, "Wired network", "有線網路"),
        ] {
            let unnamed = network("", kind, "");
            assert_eq!(both(&network_name(&unnamed)), pair(en, zh), "{kind:?}");
            assert_eq!(network_detail(&unnamed), None, "{kind:?}");
        }
        let mut m = model();
        m.status.network = Some(network("", NetworkKind::Wifi, ""));
        assert_eq!(
            both(&current_network(&m)),
            pair("Current network: Wi-Fi network", "目前的網路：Wi-Fi 網路")
        );
    }

    #[test]
    fn the_current_network_or_none() {
        let mut m = model();
        assert_eq!(
            both(&current_network(&m)),
            pair("Current network: Office-5G", "目前的網路：Office-5G")
        );
        m.status.network = Some(network("", NetworkKind::Wired, "192.168.1.1"));
        assert_eq!(
            both(&current_network(&m)),
            pair(
                "Current network: Router 192.168.1.1",
                "目前的網路：路由器 192.168.1.1"
            )
        );
        m.status.network = None;
        assert_eq!(
            both(&current_network(&m)),
            pair("Not connected to a network.", "沒有連上網路。")
        );
        assert!(network_targets(&m).is_empty());
    }

    #[test]
    fn automatic_names_the_profile_it_picked() {
        let mut m = model();
        let labels = |m: &Snapshot| -> Vec<(String, bool)> {
            choices(m)
                .iter()
                .map(|c| (c.label.to_string(), c.checked))
                .collect()
        };
        let row = |label: &str, checked: bool| (label.to_owned(), checked);
        assert_eq!(
            labels(&m),
            [
                row("Automatic (now: Office)", true),
                row("Default", false),
                row("Office", false),
                row("Other networks", false)
            ]
        );
        assert_eq!(
            Catalog::for_language(Language::ChineseTaiwan).render(&choices(&m)[0].label),
            "自動（目前：Office）"
        );
        let ids: Vec<String> = choices(&m).into_iter().map(|c| c.id).collect();
        assert_eq!(ids, ["", "default", "office", "other"]);
        // Chosen: what Automatic would pick is not known.
        m.status.profile_chosen = true;
        m.status.profile_id = "default".into();
        assert_eq!(
            labels(&m),
            [
                row("Automatic", false),
                row("Default", true),
                row("Office", false),
                row("Other networks", false)
            ]
        );
        // The tray's says Automatic alone either way.
        m.status.profile_chosen = false;
        let tray: Vec<(String, bool)> = menu(&m)
            .iter()
            .map(|c| (c.label.to_string(), c.checked))
            .collect();
        assert_eq!(tray[0], row("Automatic", true));
        assert!(tray[1..].iter().all(|(_, checked)| !checked));
        assert_eq!(
            both(&ui!(keys::PROFILE_AUTOMATIC)),
            pair("Automatic", "自動")
        );
    }

    #[test]
    fn the_network_goes_to_any_profile_but_other_networks() {
        let mut m = model();
        let targets = network_targets(&m);
        let seen: Vec<(String, String, bool)> = targets
            .iter()
            .map(|t| (t.id.clone(), t.name.to_string(), t.has_network))
            .collect();
        assert_eq!(
            seen,
            [
                ("default".to_owned(), "Default".to_owned(), false),
                ("office".to_owned(), "Office".to_owned(), true)
            ]
        );
        let office = profile(&m, "office").cloned().expect("the sample's");
        let default = profile(&m, "default").cloned().expect("the sample's");
        let other = profile(&m, "other").cloned().expect("the sample's");
        assert!(!can_add_current(&m, &office), "it has it");
        assert!(can_add_current(&m, &default));
        assert!(!can_add_current(&m, &other), "lists none");
        m.status.network = None;
        assert!(!can_add_current(&m, &default));
    }

    #[test]
    fn a_network_known_by_its_router_address_alone_cannot_be_added_yet() {
        let mut m = model();
        let default = profile(&m, "default").cloned().expect("the sample's");
        assert!(can_add_network(&m) && can_add_current(&m, &default));
        if let Some(network) = m.status.network.as_mut() {
            network.resolved = false;
        }
        assert!(!can_add_network(&m));
        assert!(!can_add_current(&m, &default));
        // Still listed, with the profile that has it: only adding waits.
        assert_eq!(network_targets(&m).len(), 2);
        assert!(network_targets(&m)[1].has_network);
        // Resolved again.
        if let Some(network) = m.status.network.as_mut() {
            network.resolved = true;
        }
        assert!(can_add_network(&m));
        // No network, or a service before profiles.
        let mut none = model();
        none.status.network = None;
        assert!(!can_add_network(&none));
        let mut old = model();
        old.status.profiles_supported = false;
        assert!(!can_add_network(&old));
    }

    #[test]
    fn the_capsule_names_the_profile_in_use_after_every_title_but_the_paused_ones() {
        let mut m = model();
        assert_eq!(
            capsule(&m).as_ref().map(both),
            Some(pair("Office", "Office"))
        );
        assert_eq!(paused_title(&m), None);
        assert_eq!(paused_subtitle(&m), None);
        // Until the service answers, none.
        m.link = Link::Connecting;
        assert_eq!(capsule(&m), None);
        // Other networks, paused, picked by the service.
        let m = sample::profile_paused(SystemTime::now(), false);
        assert!(paused(&m));
        assert_eq!(capsule(&m), None);
        assert_eq!(
            paused_title(&m).as_ref().map(both),
            Some(pair(
                "Other networks: sharing paused",
                "其他網路：已暫停共享"
            ))
        );
        assert!(unassigned_network(&m));
        assert_eq!(
            paused_subtitle(&m).as_ref().map(both),
            Some(pair(
                "This network isn’t in any profile.",
                "這個網路沒有加到任何情境。"
            ))
        );
        assert_eq!(
            both(&ui!(keys::PROFILE_ADD_THIS_NETWORK)),
            pair("Add This Network To…", "把這個網路加到…")
        );
        // Chosen by the user: the title alone.
        let chosen = sample::profile_paused(SystemTime::now(), true);
        assert!(paused(&chosen) && !unassigned_network(&chosen));
        assert_eq!(paused_subtitle(&chosen), None);
        // On no network: nothing to add.
        let mut nowhere = sample::profile_paused(SystemTime::now(), false);
        nowhere.status.network = None;
        assert!(!unassigned_network(&nowhere));
        // Another profile paused: its own name, and nothing about the
        // network.
        let mut office = model();
        for p in &mut office.status.profiles {
            p.paused = p.id == "office";
        }
        assert_eq!(
            paused_title(&office).map(|t| t.to_string()),
            Some("Office: sharing paused".into())
        );
        assert!(!unassigned_network(&office));
    }

    #[test]
    fn computers_left_out_and_paused_profiles() {
        let mut m = sample::not_in_profile(SystemTime::now());
        assert!(not_in_profile(&m, "pc"));
        assert!(!not_in_profile(&m, "mac"));
        assert!(!not_in_profile(&m, "me"));
        assert!(rename_only(&m, "pc"));
        assert!(!rename_only(&m, "mac"));
        assert!(!rename_only(&m, "me"));
        let office = profile(&m, "office").expect("the sample's");
        assert!(!connects(office, "pc") && connects(office, "mac"));
        assert!(!files_off(&m));
        // Paused: every other computer's menu has only Rename….
        let paused = sample::profile_paused(SystemTime::now(), false);
        assert!(rename_only(&paused, "mac") && rename_only(&paused, "laptop"));
        assert!(!rename_only(&paused, "me"));
        // Files off in the profile in use.
        for p in &mut m.status.profiles {
            p.files = false;
        }
        assert!(files_off(&m));
    }

    #[test]
    fn nothing_of_it_from_a_service_before_it() {
        let mut m = sample::profile_paused(SystemTime::now(), false);
        m.status.profiles_supported = false;
        m.status.devices.push(DeviceStatus {
            id: "x".into(),
            state: ConnectionState::Offline as i32,
            not_in_profile: true,
            ..Default::default()
        });
        assert_eq!(in_use(&m), None);
        assert!(!paused(&m) && !files_off(&m));
        assert!(!not_in_profile(&m, "x"));
        assert!(!rename_only(&m, "mac"));
        assert_eq!(capsule(&m), None);
        assert_eq!(paused_title(&m), None);
        assert!(choices(&m).is_empty() && menu(&m).is_empty());
        assert!(network_targets(&m).is_empty());
        let empty = Snapshot {
            link: Link::Connected,
            status: Status::default(),
            ..Default::default()
        };
        assert_eq!(in_use(&empty), None);
    }

    #[test]
    fn a_name_another_profile_has_is_said_at_the_name() {
        assert_eq!(
            rename_refused(Some("name_taken")).as_ref().map(both),
            Some(pair(
                "Another profile already has this name.",
                "已經有同名的情境。"
            ))
        );
        for other in [Some("invalid_name"), Some("unknown_profile"), None] {
            assert_eq!(rename_refused(other), None, "{other:?}");
        }
    }

    #[test]
    fn the_profile_just_added_is_found() {
        let m = model();
        let before = vec!["default".to_owned(), "other".to_owned()];
        assert_eq!(added(&before, &m).as_deref(), Some("office"));
        assert_eq!(added(&ids(&m), &m), None);
    }

    #[test]
    fn commands_set_only_what_changed() {
        assert_eq!(
            add("New Profile"),
            Command::AddProfile(AddProfile {
                name: "New Profile".into()
            })
        );
        assert_eq!(
            select(""),
            Command::SelectProfile(SelectProfile {
                profile_id: String::new()
            })
        );
        assert_eq!(
            set_paused("office", true),
            Command::EditProfile(EditProfile {
                profile_id: "office".into(),
                paused: Some(true),
                ..Default::default()
            })
        );
        assert_eq!(
            set_connect("office", "pc", false),
            Command::EditProfile(EditProfile {
                profile_id: "office".into(),
                devices: vec![DeviceInProfile {
                    device_id: "pc".into(),
                    connect: false
                }],
                ..Default::default()
            })
        );
        let Command::EditProfile(renamed) = rename("office", "Home") else {
            panic!("an edit");
        };
        assert_eq!(renamed.name.as_deref(), Some("Home"));
        assert!(renamed.paused.is_none() && renamed.files.is_none() && renamed.clipboard.is_none());
        let Command::EditProfile(files) = set_files("office", false) else {
            panic!("an edit");
        };
        assert_eq!(files.files, Some(false));
        let Command::EditProfile(clipboard) = set_clipboard("office", true) else {
            panic!("an edit");
        };
        assert_eq!(clipboard.clipboard, Some(true));
        assert_eq!(
            add_network("default"),
            Command::AddNetworkToProfile(AddNetworkToProfile {
                profile_id: "default".into()
            })
        );
        assert_eq!(
            remove_network("n1"),
            Command::RemoveNetwork(RemoveNetwork {
                network_id: "n1".into()
            })
        );
        assert_eq!(
            remove("office"),
            Command::RemoveProfile(RemoveProfile {
                profile_id: "office".into()
            })
        );
    }

    #[test]
    fn the_page_reads_the_same_in_both_languages() {
        for (key, en, zh) in [
            (keys::NAV_PROFILES, "Profiles", "情境"),
            (keys::PROFILE_IN_USE_TAG, "In use", "使用中"),
            (keys::PROFILE_ADD, "Add Profile", "新增情境"),
            (keys::PROFILE_REMOVE, "Remove Profile", "移除情境"),
            (keys::PROFILE_ADD_TO, "Add to Profile", "加到情境"),
            (keys::MENU_PROFILE, "Profile", "情境"),
            (keys::MENU_MANAGE_PROFILES, "Manage Profiles…", "管理情境…"),
            (
                keys::CONNECTION_NOT_IN_PROFILE,
                "Not in this profile",
                "不在這個情境",
            ),
        ] {
            assert_eq!(both(&ui!(key)), pair(en, zh), "{key}");
        }
    }
}
