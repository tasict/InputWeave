//! Which profile is in use (`crate::profiles_file`), and what that lets
//! this device reach. Plain logic, with the time passed in, so every case
//! is tested without a network: the engine reads the network
//! ([`iw_platform::network::current`], through the platform poller), keeps
//! track of it here ([`Tracker`]), picks the profile ([`select`]) and tells
//! the dialer, the accept loop and discovery what may be reached
//! ([`Reach`]).
//!
//! # Telling networks apart
//!
//! A network is its router's address and the router's hardware address,
//! hashed into an id ([`seen`]). A VPN, reached over a link without a
//! hardware address, is its router's address, or where it has none, this
//! computer's own address on it. The Wi-Fi name, where known, is a second
//! way: a network no profile lists by its id is taken for one listed with
//! the same Wi-Fi name.
//!
//! # A moment without a network
//!
//! A network does not go at once: joining another, roaming, or waking from
//! sleep, the route can be gone for a while, and the router's hardware
//! address unknown for a moment after joining. No route changes the network
//! only once that lasts [`GONE_GRACE`], and a new router not resolved yet is
//! taken by its address alone only once that lasts [`GRACE`], each timed
//! from when its own condition began. A router's hardware address that goes
//! while its address stays (an ARP entry that expired) keeps the network as
//! it was for [`SAME_ROUTER_GRACE`]: after that, it may as well be another
//! network whose router has the same address (192.168.1.1 at home and at a
//! cafe), and is taken by the address alone.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iw_platform::network::{LinkKind, NetworkSighting};

use crate::hex::hex;
use crate::profiles_file::{Network, NetworkKind, OTHER, Profile, ProfilesFile, network_id};

/// How long the router's hardware address may be unknown before the
/// network is taken by the router's address alone.
pub(crate) const GRACE: Duration = Duration::from_secs(3);
/// How long a router at the current network's router's address, whose
/// hardware address is not known, is still taken for the current network.
pub(crate) const SAME_ROUTER_GRACE: Duration = Duration::from_secs(30);
/// How long the route may be gone before there is taken to be no network:
/// long enough for Wi-Fi to come back after sleep. With no network no
/// device is reached anyway.
pub(crate) const GONE_GRACE: Duration = Duration::from_secs(15);

/// What this device may reach under the profile in use. Cheap to clone:
/// the devices left out are shared.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Reach {
    /// The profile is paused: no announcing, browsing, dialing or
    /// accepting.
    pub(crate) paused: bool,
    /// The trusted devices it does not connect.
    pub(crate) off: Arc<BTreeSet<String>>,
    /// Nothing yet, as for a paused profile, and no profile in effect: the
    /// agent has just started and does not know the network, and so the
    /// profile, yet.
    pub(crate) held: bool,
}

impl Reach {
    /// Nothing reached, as the agent starts.
    pub(crate) fn held() -> Self {
        Self {
            held: true,
            ..Self::default()
        }
    }

    pub(crate) fn of(profile: &Profile) -> Self {
        Self {
            paused: profile.paused,
            off: Arc::new(profile.off.clone()),
            held: false,
        }
    }

    /// Whether the profile in use connects `device`, as the apps are told
    /// (none is in effect while held, which leaves out none).
    pub(crate) fn connects(&self, device: &str) -> bool {
        crate::profiles_file::connects(self.paused, &self.off, device)
    }

    /// Whether a session with `device` may be now.
    pub(crate) fn allows(&self, device: &str) -> bool {
        !self.held && self.connects(device)
    }

    /// Whether this device answers nothing and announces nothing.
    pub(crate) fn quiet(&self) -> bool {
        self.paused || self.held
    }
}

/// The network as `sighting` shows it, if it can be told apart: what it
/// is called and the id that stands for it. A link with a hardware address
/// is told by its router (and the router's hardware address, when known);
/// a tunnel by its router, or without one by this computer's address on it.
/// The id is made with this install's `salt` (`ProfilesFile::salt`).
pub(crate) fn seen(salt: &str, sighting: &NetworkSighting) -> Option<Network> {
    let ip = |ip: Option<Ipv4Addr>| ip.map(|ip| ip.to_string()).unwrap_or_default();
    let (kind, what) = match sighting.kind {
        LinkKind::Tunnel => (
            NetworkKind::Vpn,
            match sighting.router {
                Some(router) => format!("vpn router {router}"),
                None => format!("vpn address {}", ip(sighting.address)),
            },
        ),
        kind => {
            let kind = if kind == LinkKind::Wifi {
                NetworkKind::Wifi
            } else {
                NetworkKind::Wired
            };
            match (sighting.router, sighting.router_mac) {
                (Some(router), Some(mac)) => (kind, format!("router {router} {}", hex(&mac))),
                (Some(router), None) => (kind, format!("router {router}")),
                (None, _) => (kind, format!("link address {}", ip(sighting.address))),
            }
        }
    };
    if sighting.router.is_none() && sighting.address.is_none() {
        return None;
    }
    Some(Network {
        id: network_id(salt, &what),
        kind,
        router: sighting.router,
        wifi_name: sighting.wifi_name.clone(),
        network_name: sighting.network_name.clone(),
        name: None,
    })
}

/// Whether `sighting` is a link that will tell its router's hardware
/// address but has not yet.
fn unresolved(sighting: &NetworkSighting) -> bool {
    sighting.kind != LinkKind::Tunnel && sighting.router.is_some() && sighting.router_mac.is_none()
}

/// Follows the network from reading to reading (see the module docs).
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    /// What the ids of networks are made with (`seen`).
    salt: String,
    /// The network as last taken, with the router it was taken by.
    current: Option<Network>,
    /// Whether it was taken with its router resolved (or has no router to
    /// resolve), rather than by the router's address alone.
    resolved: bool,
    /// Since when there has been no route, while there was a network.
    gone_since: Option<Instant>,
    /// Since when the readings have shown this router without its hardware
    /// address.
    unresolved_since: Option<(Ipv4Addr, Instant)>,
    /// The last reading was held back, waiting for one of those to last.
    waiting: bool,
}

impl Tracker {
    /// A tracker with nothing read yet, which makes the ids of networks
    /// with `salt`.
    pub(crate) fn new(salt: &str) -> Self {
        Self {
            salt: salt.to_owned(),
            ..Self::default()
        }
    }

    /// A tracker that has read `sighting` at `now`.
    #[cfg(test)]
    pub(crate) fn starting(salt: &str, sighting: Option<&NetworkSighting>, now: Instant) -> Self {
        let mut tracker = Self::new(salt);
        tracker.observe(sighting, now);
        tracker
    }

    /// The network as taken now.
    pub(crate) fn current(&self) -> Option<&Network> {
        self.current.as_ref()
    }

    /// Takes in the reading `sighting` at `now`; whether the network
    /// changed, in what stands for it or what it is called.
    pub(crate) fn observe(&mut self, sighting: Option<&NetworkSighting>, now: Instant) -> bool {
        self.waiting = false;
        match sighting {
            None => {
                self.unresolved_since = None;
                if self.current.is_none() {
                    self.gone_since = None;
                    return false;
                }
                let since = *self.gone_since.get_or_insert(now);
                if now.saturating_duration_since(since) < GONE_GRACE {
                    self.waiting = true;
                    return false;
                }
            }
            Some(s) => {
                self.gone_since = None;
                if let Some(router) = s.router.filter(|_| unresolved(s)) {
                    let since = match self.unresolved_since {
                        Some((r, since)) if r == router => since,
                        _ => now,
                    };
                    self.unresolved_since = Some((router, since));
                    // The same router whose hardware address went: the
                    // same network, for a while. Nothing of what it reads
                    // now is taken for it meanwhile: it may be another
                    // network's router at the same address.
                    let same = self.current.as_ref().is_some_and(|c| c.router == s.router);
                    let wait = if same { SAME_ROUTER_GRACE } else { GRACE };
                    if now.saturating_duration_since(since) < wait {
                        self.waiting = true;
                        return false;
                    }
                } else {
                    self.unresolved_since = None;
                }
            }
        }
        self.gone_since = None;
        let next = sighting.and_then(|s| seen(&self.salt, s));
        let resolved = sighting.is_some_and(|s| !unresolved(s));
        if next == self.current && resolved == self.resolved {
            return false;
        }
        let changed = next != self.current;
        self.current = next;
        self.resolved = resolved;
        changed
    }

    /// Whether the network now was taken with its router resolved: one
    /// taken by the router's address alone (it has not resolved for a
    /// while) is another id than once it resolves.
    pub(crate) fn resolved(&self) -> bool {
        self.current.is_some() && self.resolved
    }

    /// Whether the last reading was held back, waiting for what it showed
    /// to last: the engine reads again at its next tick, and takes no
    /// Wi-Fi name an app read meanwhile.
    pub(crate) fn waiting(&self) -> bool {
        self.waiting
    }
}

/// The profile to use on `network` (none: no network): the one chosen,
/// whatever the network; else the one listing it by its id; else, when its
/// Wi-Fi name is known, the one listing a network with that name, or Other
/// networks; else, while networks are recognized by Wi-Fi names, the one
/// last picked by the name there (`ProfilesFile::picks`), so the agent
/// starts on it before an app tells the name again, as the agent at the
/// login window, which no app tells, does; else Other networks.
pub(crate) fn select<'a>(file: &'a ProfilesFile, network: Option<&Network>) -> &'a str {
    if let Some(chosen) = file.chosen.as_deref().and_then(|id| file.profile(id)) {
        return &chosen.id;
    }
    let Some(network) = network else {
        return OTHER;
    };
    if let Some(profile) = file.listing(&network.id) {
        return &profile.id;
    }
    if let Some(name) = &network.wifi_name {
        return file
            .profiles
            .iter()
            .find(|p| {
                p.networks
                    .iter()
                    .any(|n| n.wifi_name.as_deref() == Some(name.as_str()))
            })
            .map_or(OTHER, |p| &p.id);
    }
    if file.wifi_names
        && let Some(profile) = file.last_pick(&network.id)
    {
        return &profile.id;
    }
    OTHER
}

/// Whether the profile picked on `network` is one to keep as picked by the
/// Wi-Fi name (`ProfilesFile::picked`): Automatic picked it on a network no
/// profile lists by its id, by a Wi-Fi name known there, while networks
/// are recognized by Wi-Fi names.
pub(crate) fn picked_by_name(file: &ProfilesFile, network: Option<&Network>) -> bool {
    file.chosen.is_none()
        && file.wifi_names
        && network.is_some_and(|n| n.wifi_name.is_some() && file.listing(&n.id).is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles_file::{DEFAULT, ProfileEdit};

    const SALT: &str = "00112233445566778899aabbccddeeff";
    const MAC: [u8; 6] = [0x0a, 0x74, 0x0b, 0xd3, 0xe8, 0x8d];

    /// The network `sighting` shows, with a salt of the tests'.
    fn seen(sighting: &NetworkSighting) -> Option<Network> {
        super::seen(SALT, sighting)
    }
    const OTHER_MAC: [u8; 6] = [0x3c, 0x22, 0xfb, 0x10, 0x20, 0x30];

    fn sighting(kind: LinkKind, router: [u8; 4], mac: Option<[u8; 6]>) -> NetworkSighting {
        NetworkSighting {
            interface: "en0".to_owned(),
            kind,
            address: Some(Ipv4Addr::new(192, 168, 1, 20)),
            router: Some(Ipv4Addr::from(router)),
            router_mac: mac,
            wifi_name: None,
            network_name: None,
        }
    }

    fn home() -> NetworkSighting {
        sighting(LinkKind::Wired, [192, 168, 1, 1], Some(MAC))
    }

    fn office() -> NetworkSighting {
        let mut s = sighting(LinkKind::Wifi, [198, 51, 100, 1], Some(OTHER_MAC));
        s.interface = "en1".to_owned();
        s
    }

    fn vpn() -> NetworkSighting {
        NetworkSighting {
            interface: "utun3".to_owned(),
            kind: LinkKind::Tunnel,
            address: Some(Ipv4Addr::new(10, 8, 0, 6)),
            router: None,
            router_mac: None,
            wifi_name: None,
            network_name: None,
        }
    }

    /// Default on the home network, Office on the office's, Other networks
    /// paused.
    fn file() -> (ProfilesFile, String) {
        let mut file = ProfilesFile::default();
        let office_id = file.add("Office").unwrap();
        file.add_network(DEFAULT, &seen(&home()).unwrap()).unwrap();
        file.add_network(&office_id, &seen(&office()).unwrap())
            .unwrap();
        file.edit(
            OTHER,
            &ProfileEdit {
                paused: Some(true),
                ..ProfileEdit::default()
            },
            |_| true,
        )
        .unwrap();
        (file, office_id)
    }

    #[test]
    fn a_network_is_its_router_and_the_routers_hardware_address() {
        let a = seen(&home()).unwrap();
        // Another router at the same address is another network.
        let b = seen(&sighting(
            LinkKind::Wired,
            [192, 168, 1, 1],
            Some(OTHER_MAC),
        ))
        .unwrap();
        assert_ne!(a.id, b.id);
        // Wired and Wi-Fi to the same router: the same network.
        let c = seen(&sighting(LinkKind::Wifi, [192, 168, 1, 1], Some(MAC))).unwrap();
        assert_eq!(a.id, c.id);
        assert_eq!(c.kind, NetworkKind::Wifi);
        // The hardware address shows nowhere.
        assert!(!format!("{a:?}").contains("0a74"));
        // Another install's salt: the same router, another id.
        let elsewhere = super::seen("ffeeddccbbaa99887766554433221100", &home()).unwrap();
        assert_ne!(elsewhere.id, a.id);
        // A VPN by its address, without a router.
        let v = seen(&vpn()).unwrap();
        assert_eq!(v.kind, NetworkKind::Vpn);
        assert_eq!(v.router, None);
        assert_ne!(v.id, a.id);
        // Nothing to tell it by.
        let mut nothing = vpn();
        nothing.address = None;
        assert_eq!(seen(&nothing), None);
    }

    #[test]
    fn switching_networks_switches_profiles() {
        let (file, office_id) = file();
        let now = Instant::now();
        let mut tracker = Tracker::starting(SALT, Some(&home()), Instant::now());
        assert_eq!(select(&file, tracker.current()), DEFAULT);
        assert!(tracker.observe(Some(&office()), now));
        assert_eq!(select(&file, tracker.current()), office_id);
        // The same reading again changes nothing.
        assert!(!tracker.observe(Some(&office()), now));
        // A network no profile lists: Other networks.
        let cafe = sighting(LinkKind::Wifi, [172, 16, 0, 1], Some([2, 2, 2, 2, 2, 2]));
        assert!(tracker.observe(Some(&cafe), now));
        assert_eq!(select(&file, tracker.current()), OTHER);
    }

    #[test]
    fn a_vpn_taking_the_route_is_another_network() {
        let (file, _) = file();
        let now = Instant::now();
        let mut tracker = Tracker::starting(SALT, Some(&home()), Instant::now());
        assert!(tracker.observe(Some(&vpn()), now));
        assert_eq!(select(&file, tracker.current()), OTHER);
        assert!(tracker.observe(Some(&home()), now));
        assert_eq!(select(&file, tracker.current()), DEFAULT);
    }

    #[test]
    fn wired_and_wifi_on_one_network_are_one() {
        let (file, _) = file();
        // The route goes out on whichever the system prefers; the router
        // is the same.
        let mut wifi = home();
        wifi.kind = LinkKind::Wifi;
        wifi.interface = "en1".to_owned();
        let mut tracker = Tracker::starting(SALT, Some(&home()), Instant::now());
        assert!(tracker.observe(Some(&wifi), Instant::now()));
        assert_eq!(select(&file, tracker.current()), DEFAULT);
    }

    #[test]
    fn no_route_for_a_while_keeps_the_network_then_it_is_other_networks() {
        let (file, _) = file();
        let start = Instant::now();
        let mut tracker = Tracker::starting(SALT, Some(&home()), Instant::now());
        assert!(!tracker.observe(None, start));
        assert!(tracker.waiting());
        assert!(!tracker.observe(None, start + GONE_GRACE / 2));
        assert_eq!(select(&file, tracker.current()), DEFAULT);
        // Back in time: nothing happened.
        assert!(!tracker.observe(Some(&home()), start + GONE_GRACE / 2));
        assert!(!tracker.waiting());
        assert!(!tracker.observe(None, start + GONE_GRACE));
        assert!(!tracker.observe(None, start + GONE_GRACE + GRACE));
        assert!(tracker.observe(None, start + GONE_GRACE * 2));
        assert_eq!(tracker.current(), None);
        assert_eq!(select(&file, tracker.current()), OTHER);
        // No network from the start either.
        assert_eq!(
            select(
                &file,
                Tracker::starting(SALT, None, Instant::now()).current()
            ),
            OTHER
        );
    }

    #[test]
    fn a_router_not_resolved_yet_waits_and_one_that_was_stays() {
        let (file, office_id) = file();
        let start = Instant::now();
        let mut tracker = Tracker::starting(SALT, Some(&home()), Instant::now());
        // Joining the office's network: its router not resolved yet.
        let mut joining = office();
        joining.router_mac = None;
        assert!(!tracker.observe(Some(&joining), start));
        assert!(tracker.observe(Some(&office()), start + Duration::from_millis(500)));
        assert_eq!(select(&file, tracker.current()), office_id);
        // Its ARP entry expired: still the office, for a while.
        let expired = start + GRACE * 10;
        assert!(!tracker.observe(Some(&joining), expired));
        assert!(!tracker.observe(Some(&joining), expired + SAME_ROUTER_GRACE / 2));
        assert_eq!(select(&file, tracker.current()), office_id);
        // Unresolved for that long, it may be another network with a router
        // at the same address: by the address alone, which no profile lists.
        assert!(tracker.observe(Some(&joining), expired + SAME_ROUTER_GRACE));
        assert_eq!(select(&file, tracker.current()), OTHER);
        assert!(!tracker.observe(Some(&joining), expired + SAME_ROUTER_GRACE * 2));
        // Resolved again: the office again.
        assert!(tracker.observe(Some(&office()), expired + SAME_ROUTER_GRACE * 3));
        assert_eq!(select(&file, tracker.current()), office_id);
        // A router that never tells its hardware address is told by its
        // address alone, once that lasts.
        let mut silent = sighting(LinkKind::Wired, [192, 168, 9, 1], None);
        silent.interface = "en0".to_owned();
        let later = expired + SAME_ROUTER_GRACE * 4;
        assert!(!tracker.observe(Some(&silent), later));
        assert!(tracker.observe(Some(&silent), later + GRACE));
        assert_eq!(tracker.current().unwrap().router, silent.router);
    }

    #[test]
    fn a_router_coming_after_an_outage_waits_its_own_time() {
        let (file, office_id) = file();
        let start = Instant::now();
        let mut tracker = Tracker::starting(SALT, Some(&home()), start);
        // No route for a while, short of giving up on home.
        assert!(!tracker.observe(None, start));
        assert!(!tracker.observe(None, start + GONE_GRACE - GRACE));
        // The office's router turns up, not resolved yet: it waits from now,
        // not from when the route went.
        let mut joining = office();
        joining.router_mac = None;
        let arrived = start + GONE_GRACE - GRACE / 2;
        assert!(!tracker.observe(Some(&joining), arrived));
        assert!(tracker.waiting());
        assert!(!tracker.observe(Some(&joining), arrived + GRACE / 2));
        assert_eq!(select(&file, tracker.current()), DEFAULT);
        // Resolved within its own wait: one change, to the office.
        assert!(tracker.observe(Some(&office()), arrived + GRACE * 3 / 4));
        assert_eq!(select(&file, tracker.current()), office_id);
        assert!(!tracker.waiting());
    }

    #[test]
    fn a_router_not_resolved_as_the_agent_starts_waits_too() {
        let start = Instant::now();
        let mut booting = home();
        booting.router_mac = None;
        let mut tracker = Tracker::starting(SALT, Some(&booting), start);
        assert_eq!(tracker.current(), None);
        assert!(tracker.waiting());
        // Resolved a moment later: that network, by its whole id.
        assert!(tracker.observe(Some(&home()), start + Duration::from_millis(300)));
        assert_eq!(tracker.current(), seen(&home()).as_ref());
        assert!(tracker.resolved());
        // Never resolved: by its address alone, once that lasts, and said
        // to be so; resolved, by its whole id.
        let mut tracker = Tracker::starting(SALT, Some(&booting), start);
        assert!(!tracker.observe(Some(&booting), start + GRACE / 2));
        assert!(!tracker.resolved());
        assert!(tracker.observe(Some(&booting), start + GRACE));
        assert_eq!(tracker.current().unwrap().router, booting.router);
        assert!(!tracker.resolved());
        assert!(tracker.observe(Some(&home()), start + GRACE * 2));
        assert!(tracker.resolved());
        // No router to resolve, as a VPN's: resolved as it is.
        let mut tracker = Tracker::starting(SALT, Some(&vpn()), start);
        assert!(tracker.resolved());
        assert!(!tracker.observe(None, start));
        assert!(tracker.observe(None, start + GONE_GRACE));
        assert!(!tracker.resolved());
    }

    #[test]
    fn the_wifi_name_matches_a_network_no_profile_lists_by_its_router() {
        let (mut file, office_id) = file();
        let mut listed = office();
        listed.wifi_name = Some("Office-5G".to_owned());
        file.learn(&seen(&listed).unwrap());
        // Another router with the same Wi-Fi name: the office, by its name.
        let mut roamed = sighting(LinkKind::Wifi, [198, 51, 101, 1], Some([4, 4, 4, 4, 4, 4]));
        roamed.wifi_name = Some("Office-5G".to_owned());
        assert_eq!(select(&file, seen(&roamed).as_ref()), office_id);
        // Without its name, Other networks.
        roamed.wifi_name = None;
        assert_eq!(select(&file, seen(&roamed).as_ref()), OTHER);
        // A name listed elsewhere loses to the router.
        let mut home_named = home();
        home_named.wifi_name = Some("Office-5G".to_owned());
        assert_eq!(select(&file, seen(&home_named).as_ref()), DEFAULT);
    }

    #[test]
    fn until_the_wifi_name_is_known_a_network_gets_the_profile_last_picked_by_it() {
        let (mut file, office_id) = file();
        file.wifi_names = true;
        let mut listed = office();
        listed.wifi_name = Some("Office-5G".to_owned());
        file.learn(&seen(&listed).unwrap());
        // Picked by the Wi-Fi name an app read on another of the office's
        // routers: kept.
        let mut roamed = sighting(LinkKind::Wifi, [198, 51, 101, 1], Some([4, 4, 4, 4, 4, 4]));
        roamed.wifi_name = Some("Office-5G".to_owned());
        let named = seen(&roamed).unwrap();
        assert!(picked_by_name(&file, Some(&named)));
        assert_eq!(select(&file, Some(&named)), office_id);
        file.picked(&named.id, &office_id);
        // The name not known (yet): the same.
        roamed.wifi_name = None;
        let unnamed = seen(&roamed).unwrap();
        assert!(!picked_by_name(&file, Some(&unnamed)));
        assert_eq!(select(&file, Some(&unnamed)), office_id);
        // Known and another, that one: here, Other networks.
        roamed.wifi_name = Some("Guest".to_owned());
        assert_eq!(select(&file, seen(&roamed).as_ref()), OTHER);
        // Names not used: by the router alone.
        file.wifi_names = false;
        assert_eq!(select(&file, Some(&unnamed)), OTHER);
        assert!(!picked_by_name(&file, Some(&named)));
        file.wifi_names = true;
        // Another network, as ever; one listed by its id, by it, and never
        // kept as picked.
        let cafe = sighting(LinkKind::Wifi, [172, 16, 0, 1], Some([5, 5, 5, 5, 5, 5]));
        assert_eq!(select(&file, seen(&cafe).as_ref()), OTHER);
        let mut home_named = home();
        home_named.wifi_name = Some("Home".to_owned());
        assert!(!picked_by_name(&file, seen(&home_named).as_ref()));
        // A chosen profile is not picked; a profile removed since is no
        // one's pick.
        file.choose(Some(DEFAULT)).unwrap();
        assert!(!picked_by_name(&file, Some(&named)));
        assert_eq!(select(&file, Some(&unnamed)), DEFAULT);
        file.choose(None).unwrap();
        file.remove(&office_id).unwrap();
        assert_eq!(select(&file, Some(&unnamed)), OTHER);
    }

    #[test]
    fn a_router_at_the_same_address_lends_its_names_to_neither_network() {
        let named = |mac: [u8; 6], name: &str| {
            let mut s = sighting(LinkKind::Wifi, [192, 168, 1, 1], Some(mac));
            s.wifi_name = Some(name.to_owned());
            s
        };
        let home = named(MAC, "Home");
        let cafe = named(OTHER_MAC, "Cafe");
        for (from, to) in [(&home, &cafe), (&cafe, &home)] {
            let start = Instant::now();
            let mut tracker = Tracker::starting(SALT, Some(from), start);
            let before = tracker.current().cloned().unwrap();
            // The next one, its router not resolved yet: the network before
            // stands, with its own names, not the next one's.
            let mut joining = to.clone();
            joining.router_mac = None;
            for second in 0..3 {
                let at = start + Duration::from_secs(second);
                assert!(!tracker.observe(Some(&joining), at));
                assert_eq!(tracker.current(), Some(&before));
            }
            // Resolved: the next one, by its own id and names.
            assert!(tracker.observe(Some(to), start + Duration::from_secs(3)));
            let after = tracker.current().unwrap();
            assert_ne!(after.id, before.id);
            assert_eq!(after.wifi_name, to.wifi_name);
        }
    }

    #[test]
    fn a_new_wifi_name_on_the_same_network_is_a_change_of_name_only() {
        let mut tracker = Tracker::starting(SALT, Some(&office()), Instant::now());
        let id = tracker.current().unwrap().id.clone();
        let mut named = office();
        named.wifi_name = Some("Office-5G".to_owned());
        assert!(tracker.observe(Some(&named), Instant::now()));
        assert_eq!(tracker.current().unwrap().id, id);
        assert_eq!(
            tracker.current().unwrap().wifi_name.as_deref(),
            Some("Office-5G")
        );
    }

    #[test]
    fn a_chosen_profile_holds_whatever_the_network() {
        let (mut file, office_id) = file();
        file.choose(Some(&office_id)).unwrap();
        assert_eq!(select(&file, seen(&home()).as_ref()), office_id);
        assert_eq!(select(&file, None), office_id);
        assert_eq!(select(&file, seen(&vpn()).as_ref()), office_id);
        // Automatic again.
        file.choose(None).unwrap();
        assert_eq!(select(&file, seen(&home()).as_ref()), DEFAULT);
        // Removed, the choice goes too.
        file.choose(Some(&office_id)).unwrap();
        file.remove(&office_id).unwrap();
        assert_eq!(select(&file, seen(&home()).as_ref()), DEFAULT);
    }

    #[test]
    fn what_a_profile_lets_this_device_reach() {
        let (mut file, office_id) = file();
        file.edit(
            &office_id,
            &ProfileEdit {
                devices: vec![("mini".to_owned(), false)],
                ..ProfileEdit::default()
            },
            |_| true,
        )
        .unwrap();
        let office = Reach::of(file.profile(&office_id).unwrap());
        assert!(office.allows("win"));
        assert!(!office.allows("mini"));
        let other = Reach::of(file.profile(OTHER).unwrap());
        assert!(!other.allows("win"));
        assert!(Reach::of(file.profile(DEFAULT).unwrap()).allows("mini"));
        // Held as the agent starts: nothing yet, and no profile in effect
        // to leave anything out.
        let held = Reach::held();
        assert!(held.connects("mini") && !held.allows("mini") && held.quiet());
        // The profile's own rule.
        for profile in &file.profiles {
            let reach = Reach::of(profile);
            for device in ["win", "mini"] {
                assert_eq!(reach.connects(device), profile.connects(device));
            }
        }
    }
}
