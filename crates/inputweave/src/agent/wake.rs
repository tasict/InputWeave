//! Waking other devices from sleep over the network, Wake-on-LAN
//! (docs/architecture.md §5.6).
//!
//! Each device tells the devices it has a session with how to wake it
//! (`WakeInfo`, sent by the session itself, off the engine): the hardware
//! address of the network interface the session runs on, and the broadcast
//! address of that interface's IPv4 network (`iw_platform::network`). A
//! device keeps what each one told it in the wake file, to wake it while
//! it is offline, and forgets it as it is unpaired.
//!
//! Waking one sends a magic packet (six 0xFF bytes, then its hardware
//! address 16 times) by UDP to port 9 of that broadcast address and of
//! 255.255.255.255, from the agent, off the engine's loop. To the broadcast
//! address only while it is on a network this device is on, as it is then
//! (`destinations`): the address is the other device's word, and the agent
//! (SYSTEM on Windows) sends nothing anywhere else on it. The device then
//! counts as being woken for two minutes, or until it connects
//! ([`Wakes`]). A push of the cursor wakes a device only if no wake-up went
//! to it in the last 30 seconds, however that one was asked for
//! ([`Wakes::sent_lately`]); one asked for from an app or the command line
//! always goes.

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

use iw_core::router::WAKE_INTERVAL;
use iw_platform::network::Interface;
use iw_proto::peer::v1::WakeInfo;

/// The port magic packets go to: discard, as most tools use.
pub(crate) const WAKE_PORT: u16 = 9;

/// How long a device being woken has to connect before it counts as not
/// woken.
pub(crate) const WAKE_WAIT: Duration = Duration::from_secs(120);

/// How soon after one `WakeInfo` from a session another is taken: a device
/// sends one as the session starts and then only as it changes, checking
/// every [`RECHECK`], so more often is a device misbehaving, whose every
/// change would be written to the disk.
pub(crate) const INFO_INTERVAL: Duration = Duration::from_secs(5);

/// How often a session looks again at how this device is woken over it.
pub(crate) const RECHECK: Duration = Duration::from_secs(60);

/// How to wake a device: its hardware address, and the broadcast address
/// of its IPv4 network if it has one. Only ever made through [`WakeData::new`],
/// so it always holds an address a magic packet can go to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct WakeData {
    mac: [u8; 6],
    broadcast: Option<Ipv4Addr>,
}

impl WakeData {
    /// None for a hardware address that is all zeros or a group address
    /// (multicast, broadcast), and for a broadcast address no network has:
    /// in 0/8 or 127/8, or from 224.0.0.0 on. 255.255.255.255, where every
    /// magic packet goes anyway, counts as none.
    pub fn new(mac: [u8; 6], broadcast: Option<Ipv4Addr>) -> Option<Self> {
        if mac == [0; 6] || mac[0] & 1 != 0 {
            return None;
        }
        let broadcast = match broadcast {
            Some(ip) if ip.is_broadcast() => None,
            Some(ip) => {
                let first = ip.octets()[0];
                if first == 0 || first == 127 || first >= 224 {
                    return None;
                }
                Some(ip)
            }
            None => None,
        };
        Some(Self { mac, broadcast })
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn broadcast(&self) -> Option<Ipv4Addr> {
        self.broadcast
    }

    /// What a peer said, if it is one ([`WakeData::new`]): a hardware
    /// address of 6 bytes, a broadcast address of 4 bytes or none.
    pub(crate) fn from_proto(info: &WakeInfo) -> Option<Self> {
        let mac: [u8; 6] = info.mac.as_slice().try_into().ok()?;
        let broadcast = match info.broadcast.len() {
            0 => None,
            4 => {
                let octets: [u8; 4] = info.broadcast.as_slice().try_into().ok()?;
                Some(Ipv4Addr::from(octets))
            }
            _ => return None,
        };
        Self::new(mac, broadcast)
    }

    pub(crate) fn to_proto(self) -> WakeInfo {
        WakeInfo {
            mac: self.mac.to_vec(),
            broadcast: self
                .broadcast
                .map(|ip| ip.octets().to_vec())
                .unwrap_or_default(),
        }
    }

    /// The hardware address as the wake file writes it, "3c:22:fb:10:20:30".
    pub fn mac_text(&self) -> String {
        let [a, b, c, d, e, f] = self.mac;
        format!("{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}")
    }

    /// The hardware address in `text`, as [`WakeData::mac_text`] writes it
    /// (either case, ':' or '-' between the bytes).
    pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
        let parts: Vec<&str> = text.split([':', '-']).collect();
        if parts.len() != 6 {
            return None;
        }
        let mut mac = [0u8; 6];
        for (byte, part) in mac.iter_mut().zip(parts) {
            if part.len() != 2 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            *byte = u8::from_str_radix(part, 16).ok()?;
        }
        Some(mac)
    }
}

impl fmt::Debug for WakeData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WakeData")
            .field("mac", &self.mac_text())
            .field("broadcast", &self.broadcast)
            .finish()
    }
}

/// The magic packet that wakes the device with hardware address `mac`.
pub(crate) fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut packet = [0xff; 102];
    for copy in packet[6..].as_chunks_mut::<6>().0 {
        *copy = mac;
    }
    packet
}

/// Where a magic packet for `data` goes, this device's network interfaces
/// being `here`: the broadcast address of its network, if it has one and
/// that lies on a network this device is on (`on_own_network`), then
/// 255.255.255.255, both on [`WAKE_PORT`].
pub(crate) fn destinations(data: &WakeData, here: &[Interface]) -> Vec<SocketAddr> {
    data.broadcast
        .filter(|ip| iw_platform::network::on_own_network(here, *ip))
        .into_iter()
        .chain([Ipv4Addr::BROADCAST])
        .map(|ip| SocketAddr::V4(SocketAddrV4::new(ip, WAKE_PORT)))
        .collect()
}

/// This device's network interfaces now, for `destinations`; none when
/// they cannot be listed. Waits for the system: off the async threads.
pub(crate) async fn interfaces_now() -> Vec<Interface> {
    match tokio::task::spawn_blocking(iw_platform::network::interfaces).await {
        Ok(Ok(interfaces)) => interfaces,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "cannot list the network interfaces");
            Vec::new()
        }
        Err(_) => Vec::new(),
    }
}

/// Sends `packet` to each of `to`, from a socket of its own that may
/// broadcast. Sent if it went to any of them; the error otherwise, the last
/// one's.
pub(crate) async fn send(packet: &[u8], to: &[SocketAddr]) -> std::io::Result<()> {
    let socket = tokio::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    socket.set_broadcast(true)?;
    let mut failure = None;
    let mut sent = false;
    for destination in to {
        match socket.send_to(packet, destination).await {
            Ok(_) => sent = true,
            Err(e) => {
                tracing::debug!(%destination, error = %e, "cannot send a wake-up there");
                failure = Some(e);
            }
        }
    }
    match (sent, failure) {
        (true, _) => Ok(()),
        (false, Some(e)) => Err(e),
        (false, None) => Err(std::io::Error::other("nowhere to send a wake-up to")),
    }
}

/// How a device that reaches this one over a connection whose far end is
/// `remote` wakes it: the interface this end of it is on, which `local`
/// says when the endpoint knows it, or else the route to `remote` does. A
/// link-local address names its interface by its scope, which `remote`'s
/// (or the route's) gives. Lists the interfaces, so it waits for the
/// system: off the async threads.
pub(crate) fn here(local: Option<IpAddr>, remote: SocketAddr) -> Option<WakeData> {
    let remote = canonical(remote);
    let (local, scope) = match local {
        Some(ip) => (ip, scope_of(remote)),
        None => {
            let routed = route_to(remote)?;
            (routed.ip(), scope_of(routed).or_else(|| scope_of(remote)))
        }
    };
    let interfaces = match iw_platform::network::interfaces() {
        Ok(interfaces) => interfaces,
        Err(e) => {
            tracing::debug!(error = %e, "cannot list the network interfaces");
            return None;
        }
    };
    let address = iw_platform::network::wake_address(&interfaces, local, scope)?;
    WakeData::new(address.mac, address.broadcast)
}

/// `addr` with an IPv4 address that came mapped into IPv6 (from a dual-stack
/// socket) as plain IPv4; any other kept whole, its scope and flow
/// information included, so a link-local address still names its
/// interface.
fn canonical(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V6(v6) => match v6.ip().to_ipv4_mapped() {
            Some(v4) => SocketAddr::V4(SocketAddrV4::new(v4, v6.port())),
            None => addr,
        },
        SocketAddr::V4(_) => addr,
    }
}

/// The interface index a link-local IPv6 socket address is scoped to.
fn scope_of(addr: SocketAddr) -> Option<u32> {
    match addr {
        SocketAddr::V6(v6) if v6.scope_id() != 0 => Some(v6.scope_id()),
        _ => None,
    }
}

/// Where this computer sends from to reach `remote`, as the system routes
/// it: a socket connected there, which sends nothing.
fn route_to(remote: SocketAddr) -> Option<SocketAddr> {
    let unspecified: IpAddr = match remote {
        SocketAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
        SocketAddr::V6(_) => std::net::Ipv6Addr::UNSPECIFIED.into(),
    };
    let socket = UdpSocket::bind((unspecified, 0)).ok()?;
    socket.connect(remote).ok()?;
    socket.local_addr().ok()
}

/// Whether a device is being woken, as clients see it
/// (`DeviceStatus.wake`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WakeState {
    #[default]
    None,
    /// A wake-up went to it, and it has not connected since, for
    /// [`WAKE_WAIT`] at most.
    Waking,
    /// It did not connect in time after the last one.
    NotWoken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wake {
    /// Until `until`; `serial` numbers the wake-ups, so word that one could
    /// not be sent counts only for the last.
    Waking {
        until: Instant,
        serial: u64,
    },
    NotWoken,
}

/// The devices being woken, or that were and did not connect in time, by
/// device id. The engine's; sans-IO, the time passed in.
#[derive(Debug, Default)]
pub(crate) struct Wakes {
    states: HashMap<String, Wake>,
    /// When the last wake-up went to each device, sent or not, until it
    /// connects.
    last_sent: HashMap<String, Instant>,
    serial: u64,
}

impl Wakes {
    /// A wake-up goes to `id` at `now`: it is being woken for `wait`, from
    /// now, even if it was already. Returns the wake-up's serial.
    pub(crate) fn sent(&mut self, id: &str, now: Instant, wait: Duration) -> u64 {
        self.last_sent.insert(id.to_owned(), now);
        self.serial += 1;
        self.states.insert(
            id.to_owned(),
            Wake::Waking {
                until: now + wait,
                serial: self.serial,
            },
        );
        self.serial
    }

    /// The wake-up `serial` could not be sent: if it is the last for `id`,
    /// `id` is not being woken, and did not wake up.
    pub(crate) fn not_sent(&mut self, id: &str, serial: u64) -> bool {
        match self.states.get(id) {
            Some(Wake::Waking { serial: last, .. }) if *last == serial => {
                self.states.insert(id.to_owned(), Wake::NotWoken);
                true
            }
            _ => false,
        }
    }

    /// `id` connected: whatever it was, it is awake, and once it goes again
    /// a push wakes it at once. Returns whether it was being woken.
    pub(crate) fn connected(&mut self, id: &str) -> bool {
        self.last_sent.remove(id);
        matches!(self.states.remove(id), Some(Wake::Waking { .. }))
    }

    /// Whether a wake-up went to `id` less than [`WAKE_INTERVAL`] before
    /// `now`, whatever asked for it, since it last connected: if so, a push
    /// toward it sends none, nor starts its two minutes over.
    pub(crate) fn sent_lately(&self, id: &str, now: Instant) -> bool {
        self.last_sent
            .get(id)
            .is_some_and(|at| now.saturating_duration_since(*at) < WAKE_INTERVAL)
    }

    /// Those whose time ran out by `now` did not wake up. Returns them.
    pub(crate) fn expire(&mut self, now: Instant) -> Vec<String> {
        let mut expired = Vec::new();
        for (id, state) in &mut self.states {
            if let Wake::Waking { until, .. } = *state
                && until <= now
            {
                *state = Wake::NotWoken;
                expired.push(id.clone());
            }
        }
        expired.sort();
        expired
    }

    /// When the first device being woken runs out of time.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.states
            .values()
            .filter_map(|state| match state {
                Wake::Waking { until, .. } => Some(*until),
                Wake::NotWoken => None,
            })
            .min()
    }

    pub(crate) fn state(&self, id: &str) -> WakeState {
        match self.states.get(id) {
            Some(Wake::Waking { .. }) => WakeState::Waking,
            Some(Wake::NotWoken) => WakeState::NotWoken,
            None => WakeState::None,
        }
    }

    /// Forgets the devices `keep` does not keep, as they are unpaired.
    pub(crate) fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.states.retain(|id, _| keep(id));
        self.last_sent.retain(|id, _| keep(id));
    }
}

#[cfg(test)]
mod tests {
    use iw_platform::network::InterfaceAddress;

    use super::*;

    const MAC: [u8; 6] = [0x3c, 0x22, 0xfb, 0x10, 0x20, 0x30];

    #[test]
    fn a_magic_packet_is_six_ff_and_the_address_sixteen_times() {
        let packet = magic_packet(MAC);
        assert_eq!(packet.len(), 102);
        assert_eq!(&packet[..6], &[0xff; 6]);
        for copy in packet[6..].chunks(6) {
            assert_eq!(copy, &MAC);
        }
    }

    /// A computer on 192.168.1.0/24, with its loopback.
    fn on_lan() -> Vec<Interface> {
        let address = |ip: &str, prefix| InterfaceAddress {
            ip: ip.parse().unwrap(),
            prefix,
        };
        vec![
            Interface {
                name: "lo".to_owned(),
                mac: None,
                addresses: vec![address("127.0.0.1", 8)],
                loopback: true,
                index: 0,
            },
            Interface {
                name: "eth0".to_owned(),
                mac: Some([2, 0, 0, 0, 0, 9]),
                addresses: vec![address("192.168.1.20", 24), address("fe80::9", 64)],
                loopback: false,
                index: 0,
            },
        ]
    }

    #[test]
    fn wake_ups_go_to_the_network_and_to_everyone() {
        let all = "255.255.255.255:9".parse::<SocketAddr>().unwrap();
        let data = WakeData::new(MAC, Some(Ipv4Addr::new(192, 168, 1, 255))).unwrap();
        assert_eq!(
            destinations(&data, &on_lan()),
            vec!["192.168.1.255:9".parse::<SocketAddr>().unwrap(), all]
        );
        let no_network = WakeData::new(MAC, None).unwrap();
        assert_eq!(destinations(&no_network, &on_lan()), vec![all]);
    }

    #[test]
    fn a_broadcast_address_off_this_computers_networks_gets_nothing() {
        let all = "255.255.255.255:9".parse::<SocketAddr>().unwrap();
        // Another network, a host on the internet, or no network known
        // here at all: 255.255.255.255 alone.
        for elsewhere in [[192, 168, 2, 255], [203, 0, 113, 7], [10, 0, 0, 255]] {
            let data = WakeData::new(MAC, Some(Ipv4Addr::from(elsewhere))).unwrap();
            assert_eq!(destinations(&data, &on_lan()), vec![all], "{elsewhere:?}");
        }
        let data = WakeData::new(MAC, Some(Ipv4Addr::new(192, 168, 1, 255))).unwrap();
        assert_eq!(destinations(&data, &[]), vec![all]);
        // Anywhere on this computer's network does: it stays on it.
        let data = WakeData::new(MAC, Some(Ipv4Addr::new(192, 168, 1, 77))).unwrap();
        assert_eq!(destinations(&data, &on_lan()).len(), 2);
    }

    #[test]
    fn hardware_addresses_that_wake_nothing_are_refused() {
        assert!(WakeData::new([0; 6], None).is_none());
        assert!(WakeData::new([0xff; 6], None).is_none(), "broadcast");
        assert!(
            WakeData::new([0x01, 0x00, 0x5e, 0, 0, 1], None).is_none(),
            "multicast"
        );
        assert!(
            WakeData::new([0x33, 0x33, 0, 0, 0, 1], None).is_none(),
            "IPv6 multicast"
        );
        // Locally administered ones are real (Wi-Fi's private addresses).
        assert!(WakeData::new([0x02, 0, 0, 0, 0, 1], None).is_some());
        assert!(WakeData::new(MAC, None).is_some());
    }

    #[test]
    fn broadcast_addresses_no_network_has_are_refused() {
        let with = |ip: [u8; 4]| WakeData::new(MAC, Some(Ipv4Addr::from(ip)));
        for bad in [
            [0, 0, 0, 0],
            [0, 255, 255, 255],
            [127, 255, 255, 255],
            [224, 0, 0, 1],
            [239, 255, 255, 250],
            [240, 0, 0, 1],
            [254, 255, 255, 255],
        ] {
            assert!(with(bad).is_none(), "{bad:?}");
        }
        for good in [
            [192, 168, 1, 255],
            [10, 255, 255, 255],
            [172, 16, 7, 255],
            [169, 254, 255, 255],
            [100, 64, 255, 255],
            [203, 0, 113, 127],
        ] {
            assert_eq!(with(good).unwrap().broadcast(), Some(Ipv4Addr::from(good)));
        }
        // Where every wake-up goes anyway: none of its own.
        assert_eq!(with([255; 4]).unwrap().broadcast(), None);
    }

    #[test]
    fn what_a_peer_says_is_taken_only_when_it_is_whole() {
        let info = |mac: &[u8], broadcast: &[u8]| WakeInfo {
            mac: mac.to_vec(),
            broadcast: broadcast.to_vec(),
        };
        let data = WakeData::from_proto(&info(&MAC, &[192, 168, 1, 255])).unwrap();
        assert_eq!(data.mac(), MAC);
        assert_eq!(data.broadcast(), Some(Ipv4Addr::new(192, 168, 1, 255)));
        assert_eq!(data.to_proto(), info(&MAC, &[192, 168, 1, 255]));
        assert_eq!(
            WakeData::from_proto(&info(&MAC, &[])).unwrap().broadcast(),
            None
        );
        for bad in [
            info(&[], &[]),
            info(&MAC[..5], &[]),
            info(&[1, 2, 3, 4, 5, 6, 7, 8], &[]),
            info(&MAC, &[192, 168, 1]),
            info(&MAC, &[192, 168, 1, 255, 0]),
            info(&MAC, &[0; 16]),
            info(&MAC, &[127, 0, 0, 1]),
            info(&[0; 6], &[192, 168, 1, 255]),
        ] {
            assert!(WakeData::from_proto(&bad).is_none(), "{bad:?}");
        }
        let none = WakeData::new(MAC, None).unwrap();
        assert_eq!(none.to_proto(), info(&MAC, &[]));
    }

    #[test]
    fn hardware_addresses_as_text() {
        let data = WakeData::new(MAC, None).unwrap();
        assert_eq!(data.mac_text(), "3c:22:fb:10:20:30");
        assert_eq!(WakeData::parse_mac("3c:22:fb:10:20:30"), Some(MAC));
        assert_eq!(WakeData::parse_mac("3C-22-FB-10-20-30"), Some(MAC));
        for bad in [
            "",
            "3c:22:fb:10:20",
            "3c:22:fb:10:20:30:40",
            "3c:22:fb:10:20:3",
            "3c:22:fb:10:20:300",
            "3c:22:fb:10:20:zz",
            "3c22fb102030",
            "+c:22:fb:10:20:30",
        ] {
            assert_eq!(WakeData::parse_mac(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_device_is_waking_until_it_connects_or_its_time_runs_out() {
        let start = Instant::now();
        let mut wakes = Wakes::default();
        assert_eq!(wakes.state("mini"), WakeState::None);
        assert_eq!(wakes.next_deadline(), None);
        wakes.sent("mini", start, WAKE_WAIT);
        assert_eq!(wakes.state("mini"), WakeState::Waking);
        assert_eq!(wakes.next_deadline(), Some(start + WAKE_WAIT));
        assert!(
            wakes
                .expire(start + WAKE_WAIT - Duration::from_millis(1))
                .is_empty()
        );
        // Once more: the two minutes start over.
        let later = start + Duration::from_secs(30);
        wakes.sent("mini", later, WAKE_WAIT);
        assert!(wakes.expire(start + WAKE_WAIT).is_empty());
        assert_eq!(wakes.expire(later + WAKE_WAIT), vec!["mini".to_owned()]);
        assert_eq!(wakes.state("mini"), WakeState::NotWoken);
        assert_eq!(wakes.next_deadline(), None);
        assert!(wakes.expire(later + WAKE_WAIT * 2).is_empty(), "said once");
        // Woken again after it did not wake up.
        wakes.sent("mini", later + WAKE_WAIT, WAKE_WAIT);
        assert_eq!(wakes.state("mini"), WakeState::Waking);
        assert!(wakes.connected("mini"));
        assert_eq!(wakes.state("mini"), WakeState::None);
        assert!(!wakes.connected("mini"));
    }

    #[test]
    fn connecting_after_the_time_ran_out_clears_it_too() {
        let start = Instant::now();
        let mut wakes = Wakes::default();
        wakes.sent("mini", start, WAKE_WAIT);
        wakes.expire(start + WAKE_WAIT);
        assert!(!wakes.connected("mini"), "it was not being woken any more");
        assert_eq!(wakes.state("mini"), WakeState::None);
    }

    #[test]
    fn only_the_last_wake_up_not_sent_counts() {
        let start = Instant::now();
        let mut wakes = Wakes::default();
        let first = wakes.sent("mini", start, WAKE_WAIT);
        let second = wakes.sent("mini", start, WAKE_WAIT);
        assert!(!wakes.not_sent("mini", first));
        assert_eq!(wakes.state("mini"), WakeState::Waking);
        assert!(wakes.not_sent("mini", second));
        assert_eq!(wakes.state("mini"), WakeState::NotWoken);
        assert!(!wakes.not_sent("mini", second));
        assert!(!wakes.not_sent("pc", second));
    }

    #[test]
    fn each_device_on_its_own_and_forgotten_as_it_goes() {
        let start = Instant::now();
        let mut wakes = Wakes::default();
        wakes.sent("mini", start, WAKE_WAIT);
        wakes.sent("pc", start + Duration::from_secs(10), WAKE_WAIT);
        assert_eq!(wakes.next_deadline(), Some(start + WAKE_WAIT));
        assert_eq!(wakes.expire(start + WAKE_WAIT), vec!["mini".to_owned()]);
        assert_eq!(wakes.state("pc"), WakeState::Waking);
        wakes.retain(|id| id != "pc");
        assert_eq!(wakes.state("pc"), WakeState::None);
        assert_eq!(wakes.state("mini"), WakeState::NotWoken);
        assert_eq!(wakes.next_deadline(), None);
    }

    #[test]
    fn a_wake_up_sent_lately_is_one_in_the_last_30_seconds_until_it_connects() {
        let start = Instant::now();
        let mut wakes = Wakes::default();
        assert!(!wakes.sent_lately("mini", start));
        wakes.sent("mini", start, WAKE_WAIT);
        assert!(wakes.sent_lately("mini", start));
        assert!(wakes.sent_lately("mini", start + Duration::from_millis(29_999)));
        assert!(!wakes.sent_lately("mini", start + WAKE_INTERVAL));
        assert!(!wakes.sent_lately("pc", start));
        // Not sent, or its time ran out: it still went lately.
        let serial = wakes.sent("mini", start, WAKE_WAIT);
        assert!(wakes.not_sent("mini", serial));
        assert!(wakes.sent_lately("mini", start + Duration::from_secs(1)));
        // Connected: a push wakes it at once once it goes again.
        wakes.connected("mini");
        assert!(!wakes.sent_lately("mini", start + Duration::from_secs(1)));
        // Unpaired: forgotten.
        wakes.sent("pc", start, WAKE_WAIT);
        wakes.retain(|id| id != "pc");
        assert!(!wakes.sent_lately("pc", start));
        assert_eq!(WAKE_INTERVAL, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn sends_the_packet_to_each_destination() {
        let receiver = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let to = receiver.local_addr().unwrap();
        let packet = magic_packet(MAC);
        send(&packet, &[to, to]).await.unwrap();
        let mut buf = [0u8; 200];
        for _ in 0..2 {
            let (n, _) = receiver.recv_from(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], &packet[..]);
        }
    }

    #[test]
    fn a_link_local_address_keeps_its_scope_and_a_mapped_one_is_ipv4() {
        let link_local: SocketAddr = "[fe80::1c2b:3c4d:5e6f:7081%7]:47820".parse().unwrap();
        let SocketAddr::V6(v6) = link_local else {
            unreachable!()
        };
        let with_flow = SocketAddr::V6(std::net::SocketAddrV6::new(*v6.ip(), 47820, 3, 7));
        assert_eq!(canonical(with_flow), with_flow, "kept whole");
        assert_eq!(scope_of(canonical(with_flow)), Some(7));
        let mapped: SocketAddr = "[::ffff:192.168.1.20]:47820".parse().unwrap();
        assert_eq!(
            canonical(mapped),
            "192.168.1.20:47820".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(scope_of(canonical(mapped)), None);
        let v4: SocketAddr = "10.0.0.2:9".parse().unwrap();
        assert_eq!(canonical(v4), v4);
        let global: SocketAddr = "[2001:db8::20]:47820".parse().unwrap();
        assert_eq!((canonical(global), scope_of(global)), (global, None));
    }

    #[tokio::test]
    async fn a_wake_up_that_goes_nowhere_is_not_sent() {
        // An IPv6 address, from the IPv4 socket wake-ups go from: refused
        // everywhere.
        let nowhere: SocketAddr = "[::1]:9".parse().unwrap();
        assert!(send(&magic_packet(MAC), &[nowhere]).await.is_err());
        assert!(send(&magic_packet(MAC), &[]).await.is_err());
    }

    #[test]
    fn nothing_tells_how_to_wake_this_computer_over_the_loopback() {
        assert_eq!(here(None, "127.0.0.1:47820".parse().unwrap()), None);
        assert_eq!(
            here(
                Some("127.0.0.1".parse().unwrap()),
                "127.0.0.1:1".parse().unwrap()
            ),
            None
        );
    }
}
