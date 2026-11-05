//! Finding other InputWeave devices on the local network (mDNS/DNS-SD).
//!
//! Each agent announces `_inputweave._udp.local.` with its device id, name,
//! OS, certificate fingerprint, protocol versions and whether it accepts new
//! pairings, and browses for the others. Nothing learned here is trusted: a
//! paired device still has to present its pinned certificate, and an
//! unpaired one has to go through pairing. Discovery only supplies names and
//! addresses.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};

use iw_proto::peer::v1::Os;
use iw_proto::{PEER_PROTOCOL_MAX, PEER_PROTOCOL_MIN};
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;

use crate::{Fingerprint, NetError};

pub const SERVICE_TYPE: &str = "_inputweave._udp.local.";

/// A TXT entry (key, `=`, value) must fit in 255 bytes.
const MAX_TXT_ENTRY: usize = 255;

/// What this device tells the network about itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Announcement {
    pub device_id: String,
    pub name: String,
    pub os: Os,
    pub fingerprint: Fingerprint,
    /// Where the endpoint listens. A wildcard IP announces every
    /// interface's addresses; a specific one announces only itself.
    pub addr: SocketAddr,
    pub pairable: bool,
}

/// Another device seen on the network.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub device_id: String,
    pub name: String,
    pub os: Os,
    pub fingerprint: Fingerprint,
    pub pairable: bool,
    pub protocol_min: u32,
    pub protocol_max: u32,
    pub addresses: Vec<SocketAddr>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiscoveryEvent {
    /// A device appeared or changed what it announces.
    Found(Found),
    /// A device stopped announcing itself.
    Lost(Fingerprint),
}

/// The running announcer and browser. Dropping it without calling
/// [`Discovery::shutdown`] leaves the announcement until it times out.
pub struct Discovery {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Discovery {
    /// Announces this device and starts browsing. Other devices are reported
    /// on the returned channel; this device's own announcement is not.
    pub fn start(
        announcement: &Announcement,
    ) -> Result<(Self, mpsc::UnboundedReceiver<DiscoveryEvent>), NetError> {
        let daemon = ServiceDaemon::new().map_err(mdns)?;
        let info = service_info(announcement)?;
        let fullname = info.get_fullname().to_owned();
        daemon.register(info).map_err(mdns)?;
        let browse = daemon.browse(SERVICE_TYPE).map_err(mdns)?;

        let (tx, rx) = mpsc::unbounded_channel();
        let own = announcement.fingerprint;
        std::thread::Builder::new()
            .name("inputweave-mdns".to_owned())
            .spawn(move || {
                // ServiceRemoved only names the instance.
                let mut instances: HashMap<String, Fingerprint> = HashMap::new();
                while let Ok(event) = browse.recv() {
                    let event = match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let Some(found) = found_from(&info) else {
                                continue;
                            };
                            if found.fingerprint == own {
                                continue;
                            }
                            instances.insert(info.get_fullname().to_owned(), found.fingerprint);
                            DiscoveryEvent::Found(found)
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            match instances.remove(&fullname) {
                                Some(fp) => DiscoveryEvent::Lost(fp),
                                None => continue,
                            }
                        }
                        _ => continue,
                    };
                    if tx.send(event).is_err() {
                        break;
                    }
                }
            })?;
        Ok((Self { daemon, fullname }, rx))
    }

    /// Announces changed details, such as whether pairing is allowed.
    pub fn update(&self, announcement: &Announcement) -> Result<(), NetError> {
        self.daemon
            .register(service_info(announcement)?)
            .map_err(mdns)
    }

    /// Withdraws the announcement and stops browsing.
    pub fn shutdown(self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

fn mdns(e: mdns_sd::Error) -> NetError {
    NetError::Discovery(e.to_string())
}

fn service_info(a: &Announcement) -> Result<ServiceInfo, NetError> {
    // The host name only has to be unique on the network; the fingerprint
    // is, even if two devices were configured with the same id.
    let hex: String = a.fingerprint.as_bytes()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let host = format!("inputweave-{hex}.local.");
    let properties: HashMap<String, String> = txt(a).into_iter().collect();
    let ip = a.addr.ip();
    if ip.is_unspecified() {
        Ok(ServiceInfo::new(
            SERVICE_TYPE,
            &a.device_id,
            &host,
            "",
            a.addr.port(),
            properties,
        )
        .map_err(mdns)?
        .enable_addr_auto())
    } else {
        ServiceInfo::new(
            SERVICE_TYPE,
            &a.device_id,
            &host,
            ip,
            a.addr.port(),
            properties,
        )
        .map_err(mdns)
    }
}

fn txt(a: &Announcement) -> Vec<(String, String)> {
    let fp: String = a
        .fingerprint
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    vec![
        ("id".to_owned(), a.device_id.clone()),
        (
            "name".to_owned(),
            truncate(&a.name, MAX_TXT_ENTRY - "name=".len()).to_owned(),
        ),
        ("os".to_owned(), os_name(a.os).to_owned()),
        ("fp".to_owned(), fp),
        (
            "pv".to_owned(),
            format!("{PEER_PROTOCOL_MIN}-{PEER_PROTOCOL_MAX}"),
        ),
        (
            "pair".to_owned(),
            if a.pairable { "1" } else { "0" }.to_owned(),
        ),
    ]
}

/// The longest prefix of `s` that fits in `max` bytes without splitting a
/// character.
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub fn os_name(os: Os) -> &'static str {
    match os {
        Os::Macos => "macos",
        Os::Windows => "windows",
        Os::Linux => "linux",
        Os::Unspecified => "",
    }
}

fn os_from_name(name: &str) -> Os {
    match name {
        "macos" => Os::Macos,
        "windows" => Os::Windows,
        "linux" => Os::Linux,
        _ => Os::Unspecified,
    }
}

fn found_from(info: &ResolvedService) -> Option<Found> {
    let addresses = info
        .get_addresses()
        .iter()
        .map(|ip| SocketAddr::new(ip.to_ip_addr(), info.get_port()))
        .collect();
    parse_txt(|key| info.get_property_val_str(key), addresses)
}

/// Reads another device's TXT record. Records missing a valid id,
/// fingerprint or protocol range are ignored.
fn parse_txt<'a>(
    get: impl Fn(&str) -> Option<&'a str>,
    mut addresses: Vec<SocketAddr>,
) -> Option<Found> {
    let device_id = get("id")?.to_owned();
    if !crate::is_valid_device_id(&device_id) {
        return None;
    }
    let fingerprint = parse_hex_fingerprint(get("fp")?)?;
    let (min, max) = get("pv")?.split_once('-')?;
    let (protocol_min, protocol_max) = (min.parse().ok()?, max.parse().ok()?);
    let name = get("name")
        .filter(|n| crate::is_valid_device_name(n))
        .unwrap_or(&device_id)
        .to_owned();
    // IPv4 first: it needs no scope id and is what the endpoint usually
    // listens on.
    addresses.sort_by_key(|a| (!a.is_ipv4(), *a));
    Some(Found {
        device_id,
        name,
        os: os_from_name(get("os").unwrap_or_default()),
        fingerprint,
        pairable: get("pair") == Some("1"),
        protocol_min,
        protocol_max,
        addresses,
    })
}

fn parse_hex_fingerprint(hex: &str) -> Option<Fingerprint> {
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(Fingerprint::from_bytes(bytes))
}

/// Whether `addr` can be dialed from an endpoint bound to `local`: an IPv4
/// socket cannot reach IPv6 addresses, and link-local IPv6 needs a scope.
pub fn reachable_from(local: SocketAddr, addr: SocketAddr) -> bool {
    match (local.ip(), addr.ip()) {
        (IpAddr::V4(_), IpAddr::V4(_)) => true,
        (IpAddr::V6(_), IpAddr::V6(v6)) => !v6.is_unicast_link_local(),
        (IpAddr::V6(l), IpAddr::V4(_)) => l.is_unspecified(),
        (IpAddr::V4(_), IpAddr::V6(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announcement() -> Announcement {
        Announcement {
            device_id: "mac".to_owned(),
            name: "Tasict 的 MacBook Pro".to_owned(),
            os: Os::Macos,
            fingerprint: Fingerprint::of_certificate(b"abc"),
            addr: "0.0.0.0:47820".parse().unwrap(),
            pairable: true,
        }
    }

    fn parse(entries: &[(String, String)], addresses: Vec<SocketAddr>) -> Option<Found> {
        parse_txt(
            |key| {
                entries
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.as_str())
            },
            addresses,
        )
    }

    #[test]
    fn txt_round_trips() {
        let a = announcement();
        let addrs = vec![
            "[fe80::1]:47820".parse().unwrap(),
            "192.168.1.20:47820".parse().unwrap(),
        ];
        let found = parse(&txt(&a), addrs).unwrap();
        assert_eq!(found.device_id, "mac");
        assert_eq!(found.name, a.name);
        assert_eq!(found.os, Os::Macos);
        assert_eq!(found.fingerprint, a.fingerprint);
        assert!(found.pairable);
        assert_eq!(
            (found.protocol_min, found.protocol_max),
            (PEER_PROTOCOL_MIN, PEER_PROTOCOL_MAX)
        );
        assert_eq!(
            found.addresses[0],
            "192.168.1.20:47820".parse::<SocketAddr>().unwrap()
        );

        let closed = Announcement {
            pairable: false,
            ..announcement()
        };
        assert!(!parse(&txt(&closed), vec![]).unwrap().pairable);
    }

    #[test]
    fn ignores_records_it_cannot_trust_the_shape_of() {
        let good = txt(&announcement());
        let without = |key: &str| -> Vec<(String, String)> {
            good.iter().filter(|(k, _)| k != key).cloned().collect()
        };
        let replacing = |key: &str, value: &str| -> Vec<(String, String)> {
            good.iter()
                .map(|(k, v)| {
                    if k == key {
                        (k.clone(), value.to_owned())
                    } else {
                        (k.clone(), v.clone())
                    }
                })
                .collect()
        };
        assert!(parse(&without("id"), vec![]).is_none());
        assert!(parse(&without("fp"), vec![]).is_none());
        assert!(parse(&without("pv"), vec![]).is_none());
        assert!(parse(&replacing("id", "bad id"), vec![]).is_none());
        assert!(parse(&replacing("fp", "abcd"), vec![]).is_none());
        assert!(parse(&replacing("pv", "one"), vec![]).is_none());
        // A missing or unusable name falls back to the id.
        assert_eq!(parse(&without("name"), vec![]).unwrap().name, "mac");
        assert_eq!(
            parse(&replacing("name", "a\u{7}b"), vec![]).unwrap().name,
            "mac"
        );
    }

    #[test]
    fn long_names_are_cut_at_a_character_boundary() {
        let a = Announcement {
            name: "名".repeat(100),
            ..announcement()
        };
        let entries = txt(&a);
        let (_, name) = entries.iter().find(|(k, _)| k == "name").unwrap();
        assert!("name=".len() + name.len() <= MAX_TXT_ENTRY);
        assert!(name.chars().all(|c| c == '名'));
    }

    #[test]
    fn only_dials_addresses_the_socket_can_reach() {
        let v4: SocketAddr = "0.0.0.0:47820".parse().unwrap();
        let v6: SocketAddr = "[::]:47820".parse().unwrap();
        let lan: SocketAddr = "192.168.1.20:47820".parse().unwrap();
        let link_local: SocketAddr = "[fe80::1]:47820".parse().unwrap();
        let global: SocketAddr = "[2001:db8::1]:47820".parse().unwrap();
        assert!(reachable_from(v4, lan));
        assert!(!reachable_from(v4, global));
        assert!(reachable_from(v6, lan));
        assert!(reachable_from(v6, global));
        assert!(!reachable_from(v6, link_local));
    }

    /// Announces and browses on the real network, so it depends on the
    /// host's multicast setup.
    #[tokio::test]
    #[ignore = "uses the local network"]
    async fn two_devices_find_each_other() {
        let a = announcement();
        let b = Announcement {
            device_id: "win".to_owned(),
            name: "Windows PC".to_owned(),
            os: Os::Windows,
            fingerprint: Fingerprint::of_certificate(b"def"),
            addr: "0.0.0.0:47821".parse().unwrap(),
            pairable: false,
        };
        let (da, mut rx) = Discovery::start(&a).unwrap();
        let (db, _rx_b) = Discovery::start(&b).unwrap();
        let found = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Some(DiscoveryEvent::Found(f)) = rx.recv().await
                    && f.fingerprint == b.fingerprint
                {
                    return f;
                }
            }
        })
        .await
        .expect("found within 10 seconds");
        assert_eq!(found.device_id, "win");
        assert!(!found.pairable);
        da.shutdown();
        db.shutdown();
    }
}
