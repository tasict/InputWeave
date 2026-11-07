//! The network this computer is on, as far as telling one network from
//! another needs it (docs/architecture.md §5.7): the route a packet to the
//! internet takes, the router at its other end and that router's hardware
//! address, and what kind of link it goes over. No permission is needed on
//! any system.
//!
//! [`current`] reads it: on macOS from the routing table and the ARP
//! entries in it (one `sysctl`), on Linux from `/proc/net/route` and
//! `/proc/net/arp`, on Windows with `GetBestRoute2` and `GetIpNetTable2`.
//! The route is the one to a public address ([`PROBE`]), not merely the
//! default one, so a VPN that takes all traffic with two half routes
//! (`0.0.0.0/1` and `128.0.0.0/1`) is the network as well. On Linux the
//! Wi-Fi name comes from NetworkManager over D-Bus, on Windows the name
//! Windows gives the network from its Network List Manager; macOS and
//! Windows read the Wi-Fi name only with location access, which the app
//! asks for, so they leave it out here.
//!
//! Choosing the route, and reading the tables, is plain Rust, tested on
//! every system with made-up tables.

use std::net::Ipv4Addr;

use crate::PlatformError;

/// The address whose route is the network's: a public one, so the route to
/// it is the one to the internet.
pub const PROBE: Ipv4Addr = Ipv4Addr::new(1, 1, 1, 1);

/// What kind of link a network is reached over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkKind {
    Wired,
    Wifi,
    /// A link without a hardware address: a VPN or another tunnel.
    Tunnel,
}

/// The network this computer is on now, as [`current`] reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkSighting {
    /// The interface the route goes out on, for logs: "en0", "wlan0", an
    /// adapter's GUID on Windows.
    pub interface: String,
    pub kind: LinkKind,
    /// This computer's IPv4 address on that interface, if it has one.
    pub address: Option<Ipv4Addr>,
    /// The router the route goes through; none for a route straight onto
    /// the link, as a VPN's often is.
    pub router: Option<Ipv4Addr>,
    /// The router's hardware address, once this computer resolved it (the
    /// ARP table); none for a tunnel, and for a moment after joining a
    /// network.
    pub router_mac: Option<[u8; 6]>,
    /// The Wi-Fi name (SSID), where the system tells it without a
    /// permission: on Linux, from NetworkManager.
    pub wifi_name: Option<String>,
    /// What Windows calls the network (its Network List Manager): often
    /// the Wi-Fi name, or "Network 2".
    pub network_name: Option<String>,
}

/// A route of the routing table, as far as choosing one needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub destination: Ipv4Addr,
    pub prefix: u8,
    /// The router it goes through; none for a route onto the link.
    pub gateway: Option<Ipv4Addr>,
    pub interface: String,
    /// The interface's index; 0 when not known.
    pub index: u32,
    /// Lower wins among routes as specific.
    pub metric: u32,
}

/// The route a packet to `target` takes: the most specific one that holds
/// it, the lowest metric among those as specific, the first among equals.
pub fn route_to(routes: &[Route], target: Ipv4Addr) -> Option<&Route> {
    routes
        .iter()
        .filter(|r| r.prefix <= 32)
        .filter(|r| {
            let mask = if r.prefix == 0 {
                0
            } else {
                u32::MAX << (32 - u32::from(r.prefix))
            };
            u32::from(r.destination) & mask == u32::from(target) & mask
        })
        .fold(None, |best: Option<&Route>, r| match best {
            Some(b) if b.prefix > r.prefix || (b.prefix == r.prefix && b.metric <= r.metric) => {
                Some(b)
            }
            _ => Some(r),
        })
}

/// A Wi-Fi name (SSID) as the system gives it, if it is one to show and
/// keep: 1 to 32 bytes of UTF-8 without control characters. Names that are
/// other bytes (some networks have them) are left out rather than guessed.
pub fn wifi_name_of(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.len() > 32 {
        return None;
    }
    let name = std::str::from_utf8(bytes).ok()?;
    (!name.chars().any(char::is_control) && !name.trim().is_empty()).then(|| name.to_owned())
}

/// The most characters a name the system gives a network may have to be
/// shown and kept.
pub const MAX_NETWORK_NAME_CHARS: usize = 256;

/// A name the system gives a network (what Windows calls it), if it is one
/// to show and keep: trimmed, not empty, at most [`MAX_NETWORK_NAME_CHARS`]
/// characters, none a control character. Others are left out.
pub fn network_name_of(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty()
        && name.chars().count() <= MAX_NETWORK_NAME_CHARS
        && !name.chars().any(char::is_control))
    .then(|| name.to_owned())
}

/// An entry of the ARP table: an address on the link, the hardware address
/// it has, and the interface it was resolved on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Neighbor {
    pub ip: Ipv4Addr,
    pub mac: [u8; 6],
    pub interface: String,
    /// The interface's index; 0 when not known.
    pub index: u32,
}

/// The hardware address of `router` as resolved on the route's interface;
/// any entry for it when none says which interface. Never all zeros, which
/// stands for not resolved.
pub fn router_mac(neighbors: &[Neighbor], route: &Route, router: Ipv4Addr) -> Option<[u8; 6]> {
    let resolved = |n: &&Neighbor| n.ip == router && n.mac != [0; 6];
    let on_route = |n: &&Neighbor| {
        (route.index != 0 && n.index == route.index)
            || (!route.interface.is_empty() && n.interface == route.interface)
    };
    neighbors
        .iter()
        .filter(resolved)
        .find(on_route)
        .or_else(|| neighbors.iter().find(resolved))
        .map(|n| n.mac)
}

/// The network this computer reaches the internet on now; none while it
/// has no route there.
pub fn current() -> Result<Option<NetworkSighting>, PlatformError> {
    #[cfg(target_vendor = "apple")]
    {
        apple::current()
    }
    #[cfg(target_os = "linux")]
    {
        linux::current()
    }
    #[cfg(windows)]
    {
        win::current()
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
    {
        Err(PlatformError::Unsupported("reading the network"))
    }
}

/// This computer's first IPv4 address on the interface named `name`.
#[cfg(unix)]
fn address_of(interfaces: &[super::Interface], name: &str) -> Option<Ipv4Addr> {
    interfaces
        .iter()
        .filter(|i| i.name == name)
        .flat_map(|i| &i.addresses)
        .find_map(|a| match a.ip {
            std::net::IpAddr::V4(ip) if !ip.is_loopback() => Some(ip),
            _ => None,
        })
}

/// Parses `/proc/net/route`: its addresses are hexadecimal numbers in the
/// machine's byte order. Routes not up are left out, and so are lines that
/// say nothing readable.
#[cfg(any(target_os = "linux", test))]
pub fn parse_proc_route(text: &str) -> Vec<Route> {
    const RTF_UP: u32 = 0x1;
    const RTF_GATEWAY: u32 = 0x2;
    let hex = |s: &str| u32::from_str_radix(s, 16).ok();
    let ip = |v: u32| Ipv4Addr::from(v.to_ne_bytes());
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 8 {
                return None;
            }
            let flags = hex(fields[3])?;
            if flags & RTF_UP == 0 {
                return None;
            }
            let destination = ip(hex(fields[1])?);
            let gateway = ip(hex(fields[2])?);
            let mask = u32::from_be_bytes(hex(fields[7])?.to_ne_bytes());
            if mask.leading_ones() + mask.trailing_zeros() != 32 && mask != 0 {
                return None;
            }
            Some(Route {
                destination,
                prefix: mask.leading_ones() as u8,
                gateway: (flags & RTF_GATEWAY != 0 && !gateway.is_unspecified()).then_some(gateway),
                interface: fields[0].to_owned(),
                index: 0,
                metric: fields[6].parse().unwrap_or(0),
            })
        })
        .collect()
}

/// Parses `/proc/net/arp`: complete entries only.
#[cfg(any(target_os = "linux", test))]
pub fn parse_proc_arp(text: &str) -> Vec<Neighbor> {
    const ATF_COM: u32 = 0x2;
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 6 {
                return None;
            }
            let flags = u32::from_str_radix(fields[2].trim_start_matches("0x"), 16).ok()?;
            if flags & ATF_COM == 0 {
                return None;
            }
            Some(Neighbor {
                ip: fields[0].parse().ok()?,
                mac: parse_mac(fields[3])?,
                interface: fields[5].to_owned(),
                index: 0,
            })
        })
        .collect()
}

/// "aa:bb:cc:dd:ee:ff" as 6 bytes; macOS writes single digits without
/// their leading zero.
#[cfg(any(target_os = "linux", target_vendor = "apple", test))]
fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let mut mac = [0u8; 6];
    let mut parts = text.split(':');
    for byte in &mut mac {
        *byte = u8::from_str_radix(parts.next()?, 16).ok()?;
    }
    parts.next().is_none().then_some(mac)
}

/// The routes and ARP entries in a dump of a BSD routing table: messages
/// of an `rt_msghdr` (`header` bytes, with the flags at `flags_at`, the
/// addresses present at `addrs_at` and the interface index at 4), each
/// followed by its addresses, every one rounded up to 4 bytes. Scoped
/// routes (`RTF_IFSCOPE`), which apply only to traffic bound to their
/// interface, are left out of the routes; ARP entries
/// (`RTF_LLINFO`) are the neighbors, the scoped ones included. `name_of`
/// names an interface by its index.
#[cfg(any(target_vendor = "apple", test))]
pub(crate) fn parse_route_dump(
    dump: &[u8],
    layout: DumpLayout,
    name_of: impl Fn(u32) -> String,
) -> (Vec<Route>, Vec<Neighbor>) {
    const RTF_UP: u32 = 0x1;
    const RTF_GATEWAY: u32 = 0x2;
    const RTF_HOST: u32 = 0x4;
    const RTF_LLINFO: u32 = 0x400;
    const RTF_IFSCOPE: u32 = 0x100_0000;
    const RTA_DST: u32 = 0x1;
    const RTA_GATEWAY: u32 = 0x2;
    const RTA_NETMASK: u32 = 0x4;
    const RTAX_MAX: u32 = 8;
    const AF_INET: u8 = 2;
    const AF_LINK: u8 = 18;

    let u16_at = |b: &[u8], at: usize| b.get(at..at + 2).map(|s| u16::from_ne_bytes([s[0], s[1]]));
    let u32_at = |b: &[u8], at: usize| {
        b.get(at..at + 4)
            .map(|s| u32::from_ne_bytes([s[0], s[1], s[2], s[3]]))
    };
    let mut routes = Vec::new();
    let mut neighbors = Vec::new();
    let mut rest = dump;
    while let Some(len) = u16_at(rest, 0) {
        let len = usize::from(len);
        if len < layout.header || len > rest.len() {
            break;
        }
        let (msg, next) = rest.split_at(len);
        rest = next;
        let (Some(index), Some(flags), Some(addrs)) = (
            u16_at(msg, 4),
            u32_at(msg, layout.flags_at),
            u32_at(msg, layout.addrs_at),
        ) else {
            continue;
        };
        // The addresses present, in the order of their bits.
        let mut sockaddrs: [Option<&[u8]>; RTAX_MAX as usize] = [None; RTAX_MAX as usize];
        let mut at = layout.header;
        for bit in 0..RTAX_MAX {
            if addrs & (1 << bit) == 0 {
                continue;
            }
            let Some(&sa_len) = msg.get(at) else {
                break;
            };
            let sa_len = usize::from(sa_len);
            let rounded = if sa_len == 0 { 4 } else { (sa_len + 3) & !3 };
            let end = (at + sa_len).min(msg.len());
            sockaddrs[bit as usize] = Some(&msg[at..end]);
            at += rounded;
        }
        let family = |sa: &[u8]| sa.get(1).copied().unwrap_or(0);
        // An AF_INET address, or a netmask's bytes, which may be cut short
        // (or of no family) where they are zero.
        let inet = |sa: &[u8]| {
            let mut ip = [0u8; 4];
            for (i, byte) in ip.iter_mut().enumerate() {
                *byte = sa.get(4 + i).copied().unwrap_or(0);
            }
            Ipv4Addr::from(ip)
        };
        let Some(dst) =
            sockaddrs[RTA_DST.trailing_zeros() as usize].filter(|sa| family(sa) == AF_INET)
        else {
            continue;
        };
        let destination = inet(dst);
        let gateway = sockaddrs[RTA_GATEWAY.trailing_zeros() as usize];
        let index = u32::from(index);
        if flags & RTF_LLINFO != 0 {
            // An ARP entry: the gateway is the link-level address.
            if let Some(dl) = gateway.filter(|sa| family(sa) == AF_LINK) {
                // sockaddr_dl: the name (sdl_nlen at 5) then the address
                // (sdl_alen at 6), from byte 8.
                let (nlen, alen) = (
                    usize::from(dl.get(5).copied().unwrap_or(0)),
                    dl.get(6).copied(),
                );
                if alen == Some(6)
                    && let Some(bytes) = dl.get(8 + nlen..8 + nlen + 6)
                {
                    let mut mac = [0u8; 6];
                    mac.copy_from_slice(bytes);
                    neighbors.push(Neighbor {
                        ip: destination,
                        mac,
                        interface: name_of(index),
                        index,
                    });
                }
            }
            continue;
        }
        if flags & RTF_UP == 0 || flags & RTF_IFSCOPE != 0 {
            continue;
        }
        let prefix = if flags & RTF_HOST != 0 {
            32
        } else {
            let mask = sockaddrs[RTA_NETMASK.trailing_zeros() as usize]
                .map_or(0, |sa| u32::from(inet(sa)));
            mask.leading_ones() as u8
        };
        let router = gateway
            .filter(|sa| flags & RTF_GATEWAY != 0 && family(sa) == AF_INET)
            .map(inet)
            .filter(|ip| !ip.is_unspecified());
        routes.push(Route {
            destination,
            prefix,
            gateway: router,
            interface: name_of(index),
            index,
            metric: 0,
        });
    }
    (routes, neighbors)
}

/// What a system service last said of the Wi-Fi name on an interface
/// (NetworkManager), so it is not asked every second: asked again once the
/// interface or the router changes (its address or its hardware address:
/// two networks may have a router at one address), or what it said is
/// `fresh` old. When
/// asking fails, it is not asked again for `retry`, and meanwhile the name
/// last known on that interface and router stands: a failure is not taken
/// for a network without one, which would change the network for nothing.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Default)]
pub(crate) struct NameCache {
    /// The interface and router asked about, the answer, and when.
    known: Option<(String, Router, Option<String>, std::time::Instant)>,
    /// When asking last failed, while it fails.
    failed_at: Option<std::time::Instant>,
}

/// A router as name caches tell it: its address and hardware address,
/// where known.
#[cfg(any(target_os = "linux", test))]
pub(crate) type Router = (Option<Ipv4Addr>, Option<[u8; 6]>);

#[cfg(any(target_os = "linux", test))]
impl NameCache {
    /// The name on `interface` with `router` at `now`, asked with `ask`
    /// when it is due.
    pub(crate) fn name(
        &mut self,
        interface: &str,
        router: Router,
        now: std::time::Instant,
        fresh: std::time::Duration,
        retry: std::time::Duration,
        ask: impl FnOnce() -> Result<Option<String>, String>,
    ) -> Option<String> {
        let known = self
            .known
            .as_ref()
            .filter(|(i, r, _, _)| i == interface && *r == router);
        let last = known.and_then(|(_, _, name, _)| name.clone());
        if known.is_some_and(|(_, _, _, at)| now.saturating_duration_since(*at) < fresh) {
            return last;
        }
        if self
            .failed_at
            .is_some_and(|at| now.saturating_duration_since(at) < retry)
        {
            return last;
        }
        match ask() {
            Ok(name) => {
                self.failed_at = None;
                self.known = Some((interface.to_owned(), router, name.clone(), now));
                name
            }
            Err(_) => {
                self.failed_at = Some(now);
                last
            }
        }
    }
}

/// What the dynamic store last said of a router's hardware address: the
/// interface and router asked about, the address, and when (macOS).
#[cfg(any(target_vendor = "apple", test))]
pub(crate) type StoreAnswer = (String, Ipv4Addr, Option<[u8; 6]>, std::time::Instant);

/// Whether to ask the dynamic store for the hardware address of `router` on
/// `interface` at `now`, after it said `last`: always (an answer is not
/// kept, as two networks may have routers at one address), but within
/// `retry` of its having had nothing for that same interface and router.
#[cfg(any(target_vendor = "apple", test))]
pub(crate) fn ask_store_again(
    last: Option<&StoreAnswer>,
    interface: &str,
    router: Ipv4Addr,
    now: std::time::Instant,
    retry: std::time::Duration,
) -> bool {
    !last.is_some_and(|(i, r, mac, at)| {
        i == interface
            && *r == router
            && mac.is_none()
            && now.saturating_duration_since(*at) < retry
    })
}

/// Where the fields `parse_route_dump` reads lie in an `rt_msghdr`.
#[cfg(any(target_vendor = "apple", test))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct DumpLayout {
    pub(crate) header: usize,
    pub(crate) flags_at: usize,
    pub(crate) addrs_at: usize,
}

#[cfg(target_vendor = "apple")]
mod apple {
    use std::ffi::{CStr, c_void};
    use std::net::Ipv4Addr;
    use std::ptr::NonNull;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType};

    use super::{DumpLayout, LinkKind, NetworkSighting, PROBE, route_to, router_mac};
    use crate::PlatformError;

    /// `SIOCGIFFUNCTIONALTYPE`: `_IOWR('i', 173, struct ifreq)`.
    const SIOCGIFFUNCTIONALTYPE: libc::c_ulong = 0xC000_0000
        | ((std::mem::size_of::<libc::ifreq>() as libc::c_ulong & 0x1fff) << 16)
        | ((b'i' as libc::c_ulong) << 8)
        | 173;
    const IFRTYPE_FUNCTIONAL_WIFI_INFRA: u32 = 3;

    pub(super) fn current() -> Result<Option<NetworkSighting>, PlatformError> {
        let layout = DumpLayout {
            header: std::mem::size_of::<libc::rt_msghdr>(),
            flags_at: std::mem::offset_of!(libc::rt_msghdr, rtm_flags),
            addrs_at: std::mem::offset_of!(libc::rt_msghdr, rtm_addrs),
        };
        let routes = table(libc::AF_INET, libc::NET_RT_DUMP, 0)?;
        let (routes, _) = super::parse_route_dump(&routes, layout, name_of);
        let Some(route) = route_to(&routes, PROBE) else {
            return Ok(None);
        };
        let interfaces = super::super::interfaces()?;
        let has_mac = interfaces
            .iter()
            .any(|i| i.name == route.interface && i.mac.is_some());
        let kind = if !has_mac {
            LinkKind::Tunnel
        } else if is_wifi(&route.interface) {
            LinkKind::Wifi
        } else {
            LinkKind::Wired
        };
        let router_mac = match (kind, route.gateway) {
            (LinkKind::Tunnel, _) | (_, None) => None,
            (_, Some(router)) => resolved_router(&route.interface, router).or_else(|| {
                // The ARP entries, where macOS still lets them be read;
                // recent versions give a process without Apple's
                // entitlement none of the IPv4 ones.
                let entries = table(0, libc::NET_RT_FLAGS, libc::RTF_LLINFO).ok()?;
                let (_, neighbors) = super::parse_route_dump(&entries, layout, name_of);
                router_mac(&neighbors, route, router)
            }),
        };
        Ok(Some(NetworkSighting {
            interface: route.interface.clone(),
            kind,
            address: super::address_of(&interfaces, &route.interface),
            router: route.gateway,
            router_mac,
            wifi_name: None,
            network_name: None,
        }))
    }

    /// The routing table's messages for `family` (0: every one):
    /// `NET_RT_DUMP` for the routes, `NET_RT_FLAGS` with `RTF_LLINFO` for
    /// the ARP entries.
    fn table(
        family: libc::c_int,
        what: libc::c_int,
        flags: libc::c_int,
    ) -> Result<Vec<u8>, PlatformError> {
        let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, family, what, flags];
        // The table can grow between asking its size and reading it.
        for _ in 0..4 {
            let mut size: libc::size_t = 0;
            // SAFETY: a size query: no buffer, `size` valid for writing.
            let rc = unsafe {
                libc::sysctl(
                    mib.as_mut_ptr(),
                    mib.len() as libc::c_uint,
                    std::ptr::null_mut(),
                    &mut size,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if rc != 0 {
                return Err(sysctl_error());
            }
            let mut buf = vec![0u8; size + size / 4 + 1024];
            let mut len = buf.len();
            // SAFETY: `buf` holds `len` writable bytes.
            let rc = unsafe {
                libc::sysctl(
                    mib.as_mut_ptr(),
                    mib.len() as libc::c_uint,
                    buf.as_mut_ptr().cast(),
                    &mut len,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if rc == 0 {
                buf.truncate(len);
                return Ok(buf);
            }
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOMEM) {
                return Err(sysctl_error());
            }
        }
        Err(PlatformError::Os(
            "sysctl: the routing table kept growing".to_owned(),
        ))
    }

    fn sysctl_error() -> PlatformError {
        PlatformError::Os(format!(
            "sysctl of the routing table: {}",
            std::io::Error::last_os_error()
        ))
    }

    #[link(name = "SystemConfiguration", kind = "framework")]
    unsafe extern "C-unwind" {
        fn SCDynamicStoreCreate(
            allocator: *const c_void,
            name: NonNull<CFString>,
            callout: *const c_void,
            context: *const c_void,
        ) -> Option<NonNull<CFType>>;
        fn SCDynamicStoreCopyValue(
            store: NonNull<CFType>,
            key: NonNull<CFString>,
        ) -> Option<NonNull<CFType>>;
    }

    /// How soon a router's hardware address the dynamic store did not have
    /// is looked for again, on the same interface and router; one it had
    /// is read again at every reading, since another network's router
    /// may have the same address.
    const NOT_FOUND_AGAIN: Duration = Duration::from_secs(2);

    /// A session with the dynamic store, kept from one reading to the next.
    struct Store(CFRetained<CFType>);

    // SAFETY: a session without a callout or run loop source, only asked
    // for values, by one caller at a time (under `RESOLVED`'s lock); CF
    // reference counting is atomic.
    unsafe impl Send for Store {}

    /// The session, and what it last said: the interface and router, the
    /// hardware address, and when.
    struct Resolved {
        store: Option<Store>,
        last: Option<super::StoreAnswer>,
    }

    static RESOLVED: Mutex<Resolved> = Mutex::new(Resolved {
        store: None,
        last: None,
    });

    /// The hardware address of `router` on `interface` as the system's
    /// configuration agent resolved it for the network's signature
    /// (`ARPResolvedHardwareAddress` of the primary service's IPv4 state):
    /// readable without a permission, where the ARP table is not. None when
    /// `interface` is not the primary one, or nothing was resolved yet.
    /// Asked of one session kept open, at every reading but soon after it
    /// had nothing for the same interface and router
    /// ([`super::ask_store_again`]).
    fn resolved_router(interface: &str, router: Ipv4Addr) -> Option<[u8; 6]> {
        let mut resolved = RESOLVED.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if !super::ask_store_again(
            resolved.last.as_ref(),
            interface,
            router,
            now,
            NOT_FOUND_AGAIN,
        ) {
            return None;
        }
        if resolved.store.is_none() {
            resolved.store = open_store().map(Store);
        }
        let mac = resolved
            .store
            .as_ref()
            .and_then(|store| ask_store(&store.0, interface, router));
        resolved.last = Some((interface.to_owned(), router, mac, now));
        mac
    }

    /// A new session with the dynamic store.
    fn open_store() -> Option<CFRetained<CFType>> {
        let name = CFString::from_static_str("inputweave");
        // SAFETY: no callout or context; the name is valid for the call.
        // The store follows the create rule.
        let store = unsafe {
            SCDynamicStoreCreate(
                std::ptr::null(),
                CFRetained::as_ptr(&name),
                std::ptr::null(),
                std::ptr::null(),
            )
        }?;
        // SAFETY: owned, as above.
        Some(unsafe { CFRetained::from_raw(store) })
    }

    fn ask_store(store: &CFRetained<CFType>, interface: &str, router: Ipv4Addr) -> Option<[u8; 6]> {
        let global = store_value(store, "State:/Network/Global/IPv4")?;
        if string_in(&global, "PrimaryInterface")?.as_str() != interface {
            return None;
        }
        let service = string_in(&global, "PrimaryService")?;
        let ipv4 = store_value(store, &format!("State:/Network/Service/{service}/IPv4"))?;
        let resolved: Ipv4Addr = string_in(&ipv4, "ARPResolvedIPAddress")?.parse().ok()?;
        if resolved != router {
            return None;
        }
        super::parse_mac(&string_in(&ipv4, "ARPResolvedHardwareAddress")?)
    }

    /// The dictionary the dynamic store holds under `key`.
    fn store_value(store: &CFRetained<CFType>, key: &str) -> Option<CFRetained<CFDictionary>> {
        let key = CFString::from_str(key);
        // SAFETY: both references are valid for the call; the value
        // follows the copy rule.
        let value = unsafe {
            SCDynamicStoreCopyValue(CFRetained::as_ptr(store), CFRetained::as_ptr(&key))
        }?;
        // SAFETY: owned, as above.
        unsafe { CFRetained::from_raw(value) }
            .downcast::<CFDictionary>()
            .ok()
    }

    /// The string `dictionary` holds under `key`.
    fn string_in(dictionary: &CFDictionary, key: &'static str) -> Option<String> {
        let key = CFString::from_static_str(key);
        // SAFETY: a CFString key; the value, if any, is borrowed from the
        // dictionary, which outlives its use here.
        let value = unsafe { dictionary.value(CFRetained::as_ptr(&key).as_ptr().cast()) };
        if value.is_null() {
            return None;
        }
        // SAFETY: a value in a dictionary is a CF object.
        let value: &CFType = unsafe { &*value.cast::<CFType>() };
        value.downcast_ref::<CFString>().map(|s| s.to_string())
    }

    fn name_of(index: u32) -> String {
        let mut buf = [0 as libc::c_char; libc::IF_NAMESIZE];
        // SAFETY: `buf` holds IF_NAMESIZE bytes, as the call needs.
        let name = unsafe { libc::if_indextoname(index, buf.as_mut_ptr()) };
        if name.is_null() {
            return String::new();
        }
        // SAFETY: if_indextoname wrote a NUL-terminated name into `buf`.
        unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }

    /// Whether the interface named `name` is a Wi-Fi one, as macOS says of
    /// its function.
    fn is_wifi(name: &str) -> bool {
        // SAFETY: a plain socket call; closed below.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        if fd < 0 {
            return false;
        }
        // SAFETY: an all-zero ifreq is a valid one to fill.
        let mut req: libc::ifreq = unsafe { std::mem::zeroed() };
        let bytes = name.as_bytes();
        if bytes.len() >= req.ifr_name.len() {
            // SAFETY: `fd` is ours.
            unsafe { libc::close(fd) };
            return false;
        }
        for (slot, byte) in req.ifr_name.iter_mut().zip(bytes) {
            *slot = *byte as libc::c_char;
        }
        // SAFETY: `req` is an ifreq naming the interface, as the call takes.
        let rc = unsafe { libc::ioctl(fd, SIOCGIFFUNCTIONALTYPE, &mut req) };
        // SAFETY: `fd` is ours.
        unsafe { libc::close(fd) };
        // SAFETY: the call filled the union's functional type, a u32 at
        // its start.
        let functional = unsafe { *std::ptr::addr_of!(req.ifr_ifru).cast::<u32>() };
        rc == 0 && functional == IFRTYPE_FUNCTIONAL_WIFI_INFRA
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::{LinkKind, NetworkSighting, PROBE, route_to, router_mac};
    use crate::PlatformError;

    /// How long a Wi-Fi name read from NetworkManager is taken as still
    /// right, while the route stays on the same interface and router.
    const NAME_FRESH: Duration = Duration::from_secs(15);
    /// How long after NetworkManager could not be asked it is not asked
    /// again: where it does not run, as on a server, it is not asked every
    /// second.
    const NM_RETRY: Duration = Duration::from_secs(60);
    /// The longest a question to NetworkManager may take.
    const NM_WAIT: Duration = Duration::from_millis(500);

    pub(super) fn current() -> Result<Option<NetworkSighting>, PlatformError> {
        let read = |path: &str| {
            std::fs::read_to_string(path)
                .map_err(|e| PlatformError::Os(format!("cannot read {path}: {e}")))
        };
        let routes = super::parse_proc_route(&read("/proc/net/route")?);
        let Some(route) = route_to(&routes, PROBE) else {
            return Ok(None);
        };
        // A missing ARP table only leaves the router's address unknown.
        let neighbors = read("/proc/net/arp")
            .map(|text| super::parse_proc_arp(&text))
            .unwrap_or_default();
        let interfaces = super::super::interfaces()?;
        let has_mac = interfaces
            .iter()
            .any(|i| i.name == route.interface && i.mac.is_some());
        let sys = Path::new("/sys/class/net").join(&route.interface);
        let kind = if !has_mac {
            LinkKind::Tunnel
        } else if sys.join("wireless").exists() || sys.join("phy80211").exists() {
            LinkKind::Wifi
        } else {
            LinkKind::Wired
        };
        let router_mac = match (kind, route.gateway) {
            (LinkKind::Tunnel, _) | (_, None) => None,
            (_, Some(router)) => router_mac(&neighbors, route, router),
        };
        let wifi_name = if kind == LinkKind::Wifi {
            wifi_name(&route.interface, (route.gateway, router_mac))
        } else {
            None
        };
        Ok(Some(NetworkSighting {
            interface: route.interface.clone(),
            kind,
            address: super::address_of(&interfaces, &route.interface),
            router: route.gateway,
            router_mac,
            wifi_name,
            network_name: None,
        }))
    }

    /// What NetworkManager last said, and the connection to it.
    struct Asked {
        names: super::NameCache,
        bus: Option<Bus>,
    }

    static ASKED: Mutex<Option<Asked>> = Mutex::new(None);

    /// The Wi-Fi name of the network on `interface`, as NetworkManager
    /// says (see [`super::NameCache`]).
    pub(super) fn wifi_name(interface: &str, router: super::Router) -> Option<String> {
        let mut asked = ASKED.lock().unwrap_or_else(|e| e.into_inner());
        let asked = asked.get_or_insert_with(|| Asked {
            names: super::NameCache::default(),
            bus: None,
        });
        let bus = &mut asked.bus;
        asked.names.name(
            interface,
            router,
            Instant::now(),
            NAME_FRESH,
            NM_RETRY,
            || {
                let (answer, kept) = ask_network_manager(interface, bus.take());
                *bus = kept;
                if let Err(e) = &answer {
                    tracing::debug!(error = %e, "NetworkManager does not say the Wi-Fi name");
                }
                answer
            },
        )
    }

    type Bus = (tokio::runtime::Runtime, zbus::Connection);

    /// Asks NetworkManager on the system bus for the Wi-Fi name of the
    /// access point `interface` is connected to; keeps the connection for
    /// the next time, unless it failed.
    fn ask_network_manager(
        interface: &str,
        bus: Option<Bus>,
    ) -> (Result<Option<String>, String>, Option<Bus>) {
        let (runtime, connection) = match bus {
            Some(bus) => bus,
            None => {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(e) => return (Err(e.to_string()), None),
                };
                let connection = runtime.block_on(async {
                    tokio::time::timeout(NM_WAIT, zbus::Connection::system()).await
                });
                match connection {
                    Ok(Ok(connection)) => (runtime, connection),
                    Ok(Err(e)) => return (Err(e.to_string()), None),
                    Err(_) => return (Err("the system bus did not answer".to_owned()), None),
                }
            }
        };
        let answer = runtime.block_on(async {
            tokio::time::timeout(NM_WAIT, ssid_of(&connection, interface))
                .await
                .unwrap_or_else(|_| Err("NetworkManager did not answer".to_owned()))
        });
        let keep = answer.is_ok().then_some((runtime, connection));
        (answer, keep)
    }

    async fn ssid_of(
        connection: &zbus::Connection,
        interface: &str,
    ) -> Result<Option<String>, String> {
        use zbus::{OwnedObjectPath, OwnedVariant, Variant};

        const NM: &str = "org.freedesktop.NetworkManager";
        const DEVICE_TYPE_WIFI: u32 = 2;
        let get = |path: OwnedObjectPath, iface: &'static str, property: &'static str| async move {
            connection
                .call_method(
                    Some(NM),
                    path.as_str(),
                    Some("org.freedesktop.DBus.Properties"),
                    "Get",
                    &(iface, property),
                )
                .await?
                .body()
                .deserialize::<OwnedVariant>()
        };
        let text = |e: zbus::Error| e.to_string();
        let device = connection
            .call_method(
                Some(NM),
                "/org/freedesktop/NetworkManager",
                Some(NM),
                "GetDeviceByIpIface",
                &(interface,),
            )
            .await
            .map_err(text)?
            .body()
            .deserialize::<OwnedObjectPath>()
            .map_err(text)?;
        let kind = get(
            device.clone(),
            "org.freedesktop.NetworkManager.Device",
            "DeviceType",
        )
        .await
        .map_err(text)?;
        if u32::try_from(Variant::from(kind)).map_err(text)? != DEVICE_TYPE_WIFI {
            return Ok(None);
        }
        let ap = get(
            device,
            "org.freedesktop.NetworkManager.Device.Wireless",
            "ActiveAccessPoint",
        )
        .await
        .map_err(text)?;
        let ap = OwnedObjectPath::try_from(Variant::from(ap)).map_err(text)?;
        if ap.as_str() == "/" {
            return Ok(None);
        }
        let ssid = get(ap, "org.freedesktop.NetworkManager.AccessPoint", "Ssid")
            .await
            .map_err(text)?;
        let ssid = <Vec<u8>>::try_from(Variant::from(ssid)).map_err(text)?;
        Ok(super::wifi_name_of(&ssid))
    }
}

#[cfg(windows)]
mod win {
    use std::net::Ipv4Addr;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use ::windows::Win32::Foundation::NO_ERROR;
    use ::windows::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GetBestRoute2, GetIfEntry2, GetIpNetTable2, IF_TYPE_IEEE80211, MIB_IF_ROW2,
        MIB_IPFORWARD_ROW2, MIB_IPNET_TABLE2,
    };
    use ::windows::Win32::Networking::NetworkListManager::{
        INetworkListManager, NetworkListManager,
    };
    use ::windows::Win32::Networking::WinSock::{
        AF_INET, IN_ADDR, IN_ADDR_0, NlnsIncomplete, NlnsUnreachable, SOCKADDR_IN, SOCKADDR_INET,
    };
    use ::windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    };
    use ::windows::core::GUID;

    use super::{LinkKind, NetworkSighting, PROBE};
    use crate::PlatformError;

    /// How long a name from the Network List Manager is taken as still
    /// right, while the route stays on the same interface and router; and
    /// how soon it is asked again when it gave none.
    const NAME_FRESH: Duration = Duration::from_secs(15);
    const NAME_AGAIN: Duration = Duration::from_secs(3);

    pub(super) fn current() -> Result<Option<NetworkSighting>, PlatformError> {
        let mut destination = SOCKADDR_INET::default();
        destination.Ipv4 = SOCKADDR_IN {
            sin_family: AF_INET,
            sin_port: 0,
            sin_addr: IN_ADDR {
                S_un: IN_ADDR_0 {
                    S_addr: u32::from(PROBE).to_be(),
                },
            },
            sin_zero: [0; 8],
        };
        let mut route = MIB_IPFORWARD_ROW2::default();
        let mut source = SOCKADDR_INET::default();
        // SAFETY: the addresses and rows are valid for the call.
        let rc = unsafe { GetBestRoute2(None, 0, None, &destination, 0, &mut route, &mut source) };
        if rc != NO_ERROR {
            // No route to the internet: no network. Any other failure says
            // nothing of the network, and the last reading stands.
            return if super::no_route(rc.0) {
                Ok(None)
            } else {
                Err(PlatformError::Os(format!("GetBestRoute2: error {}", rc.0)))
            };
        }
        let v4 = |addr: &SOCKADDR_INET| {
            // SAFETY: every case of the union starts with the family.
            (unsafe { addr.si_family } == AF_INET).then(|| {
                // SAFETY: an AF_INET address is the Ipv4 case.
                Ipv4Addr::from(u32::from_be(unsafe { addr.Ipv4.sin_addr.S_un.S_addr }))
            })
        };
        let router = v4(&route.NextHop).filter(|ip| !ip.is_unspecified());
        let address = v4(&source).filter(|ip| !ip.is_unspecified());
        let mut row = MIB_IF_ROW2 {
            InterfaceIndex: route.InterfaceIndex,
            ..Default::default()
        };
        // SAFETY: `row` names the interface to fill in.
        let rc = unsafe { GetIfEntry2(&mut row) };
        if rc != NO_ERROR {
            // Not knowing how the route goes, nothing is said.
            return Err(PlatformError::Os(format!("GetIfEntry2: error {}", rc.0)));
        }
        let kind = if row.PhysicalAddressLength != 6 {
            LinkKind::Tunnel
        } else if row.Type == IF_TYPE_IEEE80211 {
            LinkKind::Wifi
        } else {
            LinkKind::Wired
        };
        let guid = row.InterfaceGuid;
        let router_mac = match (kind, router) {
            (LinkKind::Tunnel, _) | (_, None) => None,
            (_, Some(router)) => neighbor_mac(router, route.InterfaceIndex)?,
        };
        let network_name = network_name(guid, router, router_mac);
        Ok(Some(NetworkSighting {
            interface: format!("{{{guid:?}}}"),
            kind,
            address,
            router,
            router_mac,
            wifi_name: None,
            network_name,
        }))
    }

    /// The hardware address `router` resolved to on the interface
    /// `index`, from the neighbor table: none while it is not resolved; an
    /// error when the table cannot be read, which is not the router going
    /// unresolved.
    fn neighbor_mac(router: Ipv4Addr, index: u32) -> Result<Option<[u8; 6]>, PlatformError> {
        let mut table: *mut MIB_IPNET_TABLE2 = std::ptr::null_mut();
        // SAFETY: `table` is valid for writing; freed below.
        let rc = unsafe { GetIpNetTable2(AF_INET, &mut table) };
        if rc != NO_ERROR || table.is_null() {
            return Err(PlatformError::Os(format!("GetIpNetTable2: error {}", rc.0)));
        }
        // SAFETY: the table GetIpNetTable2 made, `NumEntries` rows long.
        let rows = unsafe {
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize)
        };
        let found = rows
            .iter()
            .filter(|r| r.InterfaceIndex == index && r.PhysicalAddressLength == 6)
            .filter(|r| r.State != NlnsUnreachable && r.State != NlnsIncomplete)
            .find(|r| {
                // SAFETY: every case of the union starts with the family.
                let family = unsafe { r.Address.si_family };
                // SAFETY: an AF_INET address is the Ipv4 case.
                let ip =
                    || Ipv4Addr::from(u32::from_be(unsafe { r.Address.Ipv4.sin_addr.S_un.S_addr }));
                family == AF_INET && ip() == router
            })
            .map(|r| {
                let mut mac = [0u8; 6];
                mac.copy_from_slice(&r.PhysicalAddress[..6]);
                mac
            })
            .filter(|mac| *mac != [0; 6]);
        // SAFETY: the table GetIpNetTable2 made, not used after this.
        unsafe { FreeMibTable(table.cast()) };
        Ok(found)
    }

    struct Named {
        guid: GUID,
        router: Option<Ipv4Addr>,
        router_mac: Option<[u8; 6]>,
        name: Option<String>,
        at: Instant,
    }

    static NAMED: Mutex<Option<Named>> = Mutex::new(None);

    /// What the Network List Manager calls the network the adapter `guid`
    /// is connected to; asked again once the adapter or router (its
    /// address or hardware address: two networks may have one router
    /// address) changes, or [`NAME_FRESH`] after it was last asked. When it
    /// says nothing (as while it is still identifying the network, or it
    /// failed), that is not kept: it is asked again after [`NAME_AGAIN`],
    /// the name last known on that adapter and router standing meanwhile.
    fn network_name(
        guid: GUID,
        router: Option<Ipv4Addr>,
        router_mac: Option<[u8; 6]>,
    ) -> Option<String> {
        let mut named = NAMED.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let same = named
            .as_ref()
            .filter(|n| n.guid == guid && n.router == router && n.router_mac == router_mac);
        if let Some(n) = same {
            let fresh = if n.name.is_some() {
                NAME_FRESH
            } else {
                NAME_AGAIN
            };
            if now.duration_since(n.at) < fresh {
                return n.name.clone();
            }
        }
        let last = same.and_then(|n| n.name.clone());
        let name = ask_nlm(guid).or(last);
        *named = Some(Named {
            guid,
            router,
            router_mac,
            name: name.clone(),
            at: now,
        });
        name
    }

    /// What the Network List Manager calls the network the adapter `guid`
    /// is connected to: of its connections, the one on that adapter (a
    /// connection's id is not the adapter's, so each is asked which
    /// adapter it is on).
    fn ask_nlm(guid: GUID) -> Option<String> {
        // SAFETY: a plain call; a success, S_FALSE included, is balanced
        // below.
        let entered = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        let connections = (|| {
            let mut found = Vec::new();
            // SAFETY: plain COM calls on interfaces this function holds.
            unsafe {
                let manager: INetworkListManager =
                    CoCreateInstance(&NetworkListManager, None, CLSCTX_ALL).ok()?;
                let all = manager.GetNetworkConnections().ok()?;
                loop {
                    let mut one = [None];
                    let mut fetched = 0u32;
                    if all.Next(&mut one, Some(&mut fetched)).is_err() || fetched == 0 {
                        break;
                    }
                    let Some(connection) = one[0].take() else {
                        break;
                    };
                    let Ok(adapter) = connection.GetAdapterId() else {
                        continue;
                    };
                    let name = connection
                        .GetNetwork()
                        .and_then(|network| network.GetName())
                        .ok()
                        .map(|name| name.to_string());
                    found.push((adapter.to_u128(), name));
                    // Enough to look through, whatever the system says.
                    if found.len() >= 64 {
                        break;
                    }
                }
            }
            Some(found)
        })();
        if entered {
            // SAFETY: balances the CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
        super::name_on_adapter(&connections?, guid.to_u128())
    }
}

/// Whether `GetBestRoute2` failing with `code` says there is no route to
/// the internet (the network or host is unreachable), rather than that the
/// route could not be read.
#[cfg(any(windows, test))]
fn no_route(code: u32) -> bool {
    // ERROR_NETWORK_UNREACHABLE, ERROR_HOST_UNREACHABLE.
    matches!(code, 1231 | 1232)
}

/// The network name the Network List Manager gives the connection on
/// adapter `adapter`, of `connections` (each its adapter's id and the name
/// of its network): the first with a name that is one.
#[cfg(any(windows, test))]
fn name_on_adapter(connections: &[(u128, Option<String>)], adapter: u128) -> Option<String> {
    connections
        .iter()
        .filter(|(on, _)| *on == adapter)
        .find_map(|(_, name)| name.as_deref().and_then(network_name_of))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(dest: [u8; 4], prefix: u8, gateway: Option<[u8; 4]>, interface: &str) -> Route {
        Route {
            destination: Ipv4Addr::from(dest),
            prefix,
            gateway: gateway.map(Ipv4Addr::from),
            interface: interface.to_owned(),
            index: 0,
            metric: 0,
        }
    }

    #[test]
    fn the_network_name_is_the_one_of_the_connection_on_the_adapter() {
        let connections = [
            (1, Some("Network 2".to_owned())),
            (2, None),
            (2, Some(" ".to_owned())),
            // What the profiles file would drop is never taken.
            (2, Some("x".repeat(MAX_NETWORK_NAME_CHARS + 1))),
            (2, Some("Lab\u{7}".to_owned())),
            (2, Some(" Office-5G ".to_owned())),
        ];
        assert_eq!(
            name_on_adapter(&connections, 1).as_deref(),
            Some("Network 2")
        );
        assert_eq!(
            name_on_adapter(&connections, 2).as_deref(),
            Some("Office-5G")
        );
        assert_eq!(name_on_adapter(&connections, 3), None);
    }

    #[test]
    fn names_of_networks_are_kept_only_as_the_profiles_file_keeps_them() {
        assert_eq!(
            network_name_of("  Network 2 ").as_deref(),
            Some("Network 2")
        );
        let longest = "x".repeat(MAX_NETWORK_NAME_CHARS);
        assert_eq!(network_name_of(&longest), Some(longest.clone()));
        assert_eq!(network_name_of(&(longest + "x")), None);
        assert_eq!(network_name_of(" "), None);
        assert_eq!(network_name_of("a\nb"), None);
    }

    #[test]
    fn a_failed_question_keeps_the_name_last_known_on_that_network() {
        use std::time::{Duration, Instant};
        let fresh = Duration::from_secs(15);
        let retry = Duration::from_secs(60);
        let router = (Some(Ipv4Addr::new(192, 168, 1, 1)), Some([2; 6]));
        let start = Instant::now();
        let mut cache = NameCache::default();
        let named = || Ok(Some("Office-5G".to_owned()));
        let failing = || Err("no answer".to_owned());
        let never = || -> Result<Option<String>, String> { panic!("asked") };
        assert_eq!(
            cache
                .name("wlan0", router, start, fresh, retry, named)
                .as_deref(),
            Some("Office-5G")
        );
        // Fresh: not asked.
        assert_eq!(
            cache
                .name("wlan0", router, start + fresh / 2, fresh, retry, never)
                .as_deref(),
            Some("Office-5G")
        );
        // Due, and failing: the name stands, and it is not asked again for
        // a while.
        let failed = start + fresh;
        assert_eq!(
            cache
                .name("wlan0", router, failed, fresh, retry, failing)
                .as_deref(),
            Some("Office-5G")
        );
        assert_eq!(
            cache
                .name("wlan0", router, failed + retry / 2, fresh, retry, never)
                .as_deref(),
            Some("Office-5G")
        );
        // Another router meanwhile: nothing known of that network; nor of
        // another router at the same address (a cafe's 192.168.1.1), nor
        // of one not resolved yet.
        let other = (Some(Ipv4Addr::new(172, 16, 0, 1)), Some([3; 6]));
        let cafe = (router.0, Some([4; 6]));
        let unresolved = (router.0, None);
        for elsewhere in [other, cafe, unresolved] {
            assert_eq!(
                cache.name("wlan0", elsewhere, failed + retry / 2, fresh, retry, never),
                None
            );
        }
        // Asked again after the wait; an answer clears the failure.
        let later = failed + retry;
        assert_eq!(
            cache.name("wlan0", router, later, fresh, retry, || Ok(None)),
            None
        );
        assert_eq!(
            cache.name(
                "wlan0",
                router,
                later + Duration::from_secs(1),
                fresh,
                retry,
                never
            ),
            None
        );
        // The cafe's router at the same address is asked of, not taken
        // for the one before.
        assert_eq!(
            cache
                .name(
                    "wlan0",
                    cafe,
                    later + Duration::from_secs(1),
                    fresh,
                    retry,
                    || { Ok(Some("Cafe".to_owned())) }
                )
                .as_deref(),
            Some("Cafe")
        );
        // Another interface is asked at once.
        assert_eq!(
            cache
                .name(
                    "wlan1",
                    router,
                    later + Duration::from_secs(2),
                    fresh,
                    retry,
                    named
                )
                .as_deref(),
            Some("Office-5G")
        );
    }

    #[test]
    fn the_dynamic_store_is_asked_at_every_reading_but_soon_after_nothing() {
        use std::time::{Duration, Instant};
        let retry = Duration::from_secs(2);
        let router = Ipv4Addr::new(192, 168, 1, 1);
        let now = Instant::now();
        assert!(ask_store_again(None, "en0", router, now, retry));
        // An answer is never kept: a cafe's router may have the address.
        let found = ("en0".to_owned(), router, Some([2; 6]), now);
        assert!(ask_store_again(Some(&found), "en0", router, now, retry));
        // Nothing, a moment ago: not asked again yet, for that router.
        let nothing = ("en0".to_owned(), router, None, now);
        assert!(!ask_store_again(
            Some(&nothing),
            "en0",
            router,
            now + retry / 2,
            retry
        ));
        assert!(ask_store_again(
            Some(&nothing),
            "en0",
            router,
            now + retry,
            retry
        ));
        let other = Ipv4Addr::new(172, 16, 0, 1);
        assert!(ask_store_again(Some(&nothing), "en0", other, now, retry));
        assert!(ask_store_again(Some(&nothing), "en1", router, now, retry));
    }

    #[test]
    fn only_an_unreachable_network_or_host_is_no_route() {
        assert!(no_route(1231) && no_route(1232));
        // ERROR_NOT_ENOUGH_MEMORY, ERROR_INVALID_PARAMETER, ERROR_NOT_FOUND:
        // a read that failed, which says nothing of the network.
        assert!(!no_route(8) && !no_route(87) && !no_route(1168));
    }

    #[test]
    fn wifi_names_are_short_text() {
        assert_eq!(wifi_name_of(b"Office-5G").as_deref(), Some("Office-5G"));
        assert_eq!(wifi_name_of("Café".as_bytes()).as_deref(), Some("Café"));
        assert_eq!(wifi_name_of(b""), None);
        assert_eq!(wifi_name_of(b"   "), None);
        assert_eq!(wifi_name_of(&[b'a'; 33]), None);
        assert_eq!(wifi_name_of(&[b'a'; 32]).map(|n| n.len()), Some(32));
        assert_eq!(wifi_name_of(b"line\nbreak"), None);
        assert_eq!(wifi_name_of(&[0xff, 0xfe]), None);
    }

    #[test]
    fn the_most_specific_route_wins_and_half_routes_are_a_vpns() {
        let lan = route([0, 0, 0, 0], 0, Some([192, 168, 1, 1]), "en0");
        let local = route([192, 168, 1, 0], 24, None, "en0");
        assert_eq!(route_to(&[lan.clone(), local.clone()], PROBE), Some(&lan));
        // A VPN takes everything with two halves, over the default route.
        let low = route([0, 0, 0, 0], 1, None, "utun3");
        let high = route([128, 0, 0, 0], 1, None, "utun3");
        let routes = [lan.clone(), local.clone(), low.clone(), high.clone()];
        assert_eq!(route_to(&routes, PROBE), Some(&low));
        assert_eq!(route_to(&routes, Ipv4Addr::new(200, 1, 1, 1)), Some(&high));
        // No route at all, or none to the internet.
        assert_eq!(route_to(&[], PROBE), None);
        assert_eq!(route_to(&[local], PROBE), None);
    }

    #[test]
    fn of_two_default_routes_the_lower_metric_wins() {
        let mut wifi = route([0, 0, 0, 0], 0, Some([10, 0, 0, 1]), "wlan0");
        wifi.metric = 600;
        let mut wired = route([0, 0, 0, 0], 0, Some([10, 0, 0, 1]), "eth0");
        wired.metric = 100;
        assert_eq!(
            route_to(&[wifi.clone(), wired.clone()], PROBE),
            Some(&wired)
        );
        assert_eq!(route_to(&[wired.clone(), wifi], PROBE), Some(&wired));
    }

    const PROC_ROUTE: &str = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0
eth0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0
tun0\t00000000\t00000000\t0000\t0\t0\t0\t00000080\t0\t0\t0
garbage
";

    #[test]
    fn linux_routes_are_read_in_the_machines_byte_order() {
        if cfg!(target_endian = "big") {
            return;
        }
        let routes = parse_proc_route(PROC_ROUTE);
        // The route not up (tun0) and the line that says nothing are out.
        assert_eq!(routes.len(), 3);
        assert_eq!(routes[0].gateway, Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(routes[0].prefix, 0);
        assert_eq!(routes[0].metric, 600);
        assert_eq!(routes[2].destination, Ipv4Addr::new(192, 168, 1, 0));
        assert_eq!(routes[2].prefix, 24);
        assert_eq!(routes[2].gateway, None);
        // Wired and Wi-Fi both up: the default route with the lower metric.
        assert_eq!(route_to(&routes, PROBE).unwrap().interface, "eth0");
    }

    const PROC_ARP: &str = "\
IP address       HW type     Flags       HW address            Mask     Device
192.168.1.1      0x1         0x2         3c:22:fb:10:20:30     *        eth0
192.168.1.1      0x1         0x2         3c:22:fb:40:50:60     *        wlan0
192.168.1.7      0x1         0x0         00:00:00:00:00:00     *        eth0
";

    #[test]
    fn linux_arp_entries_and_the_routers_on_its_interface() {
        let neighbors = parse_proc_arp(PROC_ARP);
        assert_eq!(neighbors.len(), 2);
        let router = Ipv4Addr::new(192, 168, 1, 1);
        let eth = route([0, 0, 0, 0], 0, Some([192, 168, 1, 1]), "eth0");
        let wlan = route([0, 0, 0, 0], 0, Some([192, 168, 1, 1]), "wlan0");
        assert_eq!(
            router_mac(&neighbors, &eth, router),
            Some([0x3c, 0x22, 0xfb, 0x10, 0x20, 0x30])
        );
        assert_eq!(
            router_mac(&neighbors, &wlan, router),
            Some([0x3c, 0x22, 0xfb, 0x40, 0x50, 0x60])
        );
        // Not resolved yet.
        assert_eq!(
            router_mac(&neighbors, &eth, Ipv4Addr::new(192, 168, 1, 7)),
            None
        );
        assert_eq!(parse_mac("3c:22:fb:10:20"), None);
        assert_eq!(parse_mac("3c:22:fb:10:20:30:40"), None);
    }

    /// The layout of a made-up `rt_msghdr`: 16 bytes, the flags at 8 and the
    /// addresses present at 12.
    const LAYOUT: DumpLayout = DumpLayout {
        header: 16,
        flags_at: 8,
        addrs_at: 12,
    };

    fn message(index: u16, flags: u32, sockaddrs: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut addrs = 0;
        let mut body = Vec::new();
        for (bit, sa) in sockaddrs {
            addrs |= bit;
            body.extend_from_slice(sa);
            while body.len() % 4 != 0 {
                body.push(0);
            }
            if sa.is_empty() {
                body.extend_from_slice(&[0; 4]);
            }
        }
        let mut msg = vec![0u8; 16];
        let len = (16 + body.len()) as u16;
        msg[0..2].copy_from_slice(&len.to_ne_bytes());
        msg[4..6].copy_from_slice(&index.to_ne_bytes());
        msg[8..12].copy_from_slice(&flags.to_ne_bytes());
        msg[12..16].copy_from_slice(&addrs.to_ne_bytes());
        msg.extend(body);
        msg
    }

    fn sin(ip: [u8; 4]) -> Vec<u8> {
        let mut sa = vec![16, 2, 0, 0];
        sa.extend_from_slice(&ip);
        sa.extend_from_slice(&[0; 8]);
        sa
    }

    /// A netmask cut short after its last byte that is not zero.
    fn mask(bytes: &[u8]) -> Vec<u8> {
        let mut sa = vec![(4 + bytes.len()) as u8, 0xff, 0, 0];
        sa.extend_from_slice(bytes);
        sa
    }

    fn sdl(name: &str, mac: Option<[u8; 6]>) -> Vec<u8> {
        let alen = if mac.is_some() { 6 } else { 0 };
        let mut sa = vec![0, 18, 0, 0, 6, name.len() as u8, alen, 0];
        sa.extend_from_slice(name.as_bytes());
        if let Some(mac) = mac {
            sa.extend_from_slice(&mac);
        }
        sa[0] = sa.len().max(20) as u8;
        sa.resize(sa.len().max(20), 0);
        sa
    }

    #[test]
    fn a_bsd_routing_table_dump_gives_routes_and_arp_entries() {
        const UP: u32 = 0x1;
        const GATEWAY: u32 = 0x2;
        const HOST: u32 = 0x4;
        const LLINFO: u32 = 0x400;
        const IFSCOPE: u32 = 0x100_0000;
        const DST: u32 = 0x1;
        const GW: u32 = 0x2;
        const NETMASK: u32 = 0x4;
        let router = [10, 1, 8, 1];
        let wired_mac = [0x0a, 0x74, 0x0b, 0xd3, 0xe8, 0x8d];
        let mut dump = Vec::new();
        // The default route on en0, and the same on en1, scoped to it.
        dump.extend(message(
            7,
            UP | GATEWAY,
            &[(DST, sin([0; 4])), (GW, sin(router)), (NETMASK, Vec::new())],
        ));
        dump.extend(message(
            17,
            UP | GATEWAY | IFSCOPE,
            &[(DST, sin([0; 4])), (GW, sin(router)), (NETMASK, Vec::new())],
        ));
        // The network itself, onto the link.
        dump.extend(message(
            7,
            UP,
            &[
                (DST, sin([10, 1, 8, 0])),
                (GW, sdl("en0", None)),
                (NETMASK, mask(&[255, 255, 252])),
            ],
        ));
        // The router's ARP entries, on both interfaces.
        dump.extend(message(
            7,
            UP | HOST | LLINFO,
            &[(DST, sin(router)), (GW, sdl("en0", Some(wired_mac)))],
        ));
        dump.extend(message(
            17,
            UP | HOST | LLINFO | IFSCOPE,
            &[
                (DST, sin(router)),
                (GW, sdl("en1", Some([1, 2, 3, 4, 5, 6]))),
            ],
        ));
        // A route not up.
        dump.extend(message(
            7,
            GATEWAY,
            &[
                (DST, sin([1, 0, 0, 0])),
                (GW, sin(router)),
                (NETMASK, mask(&[255])),
            ],
        ));
        let names = |index: u32| match index {
            7 => "en0".to_owned(),
            17 => "en1".to_owned(),
            _ => String::new(),
        };
        let (routes, neighbors) = parse_route_dump(&dump, LAYOUT, names);
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[1].prefix, 22);
        let best = route_to(&routes, PROBE).unwrap();
        assert_eq!(best.interface, "en0");
        assert_eq!(best.gateway, Some(Ipv4Addr::from(router)));
        assert_eq!(neighbors.len(), 2);
        assert_eq!(
            router_mac(&neighbors, best, Ipv4Addr::from(router)),
            Some(wired_mac)
        );
        // A message cut short ends the dump, without a panic.
        let (cut, _) = parse_route_dump(&dump[..dump.len() - 3], LAYOUT, |_| String::new());
        assert_eq!(cut.len(), 2);
        let (none, _) = parse_route_dump(&[0xff, 0xff, 0, 0], LAYOUT, |_| String::new());
        assert!(none.is_empty());
    }

    #[test]
    fn a_vpns_half_routes_in_a_bsd_dump_go_onto_its_link() {
        const UP: u32 = 0x1;
        const GATEWAY: u32 = 0x2;
        let mut dump = Vec::new();
        dump.extend(message(
            7,
            UP | GATEWAY,
            &[
                (1, sin([0; 4])),
                (2, sin([192, 168, 1, 1])),
                (4, Vec::new()),
            ],
        ));
        dump.extend(message(
            30,
            UP,
            &[(1, sin([0; 4])), (2, sdl("utun3", None)), (4, mask(&[128]))],
        ));
        dump.extend(message(
            30,
            UP,
            &[
                (1, sin([128, 0, 0, 0])),
                (2, sdl("utun3", None)),
                (4, mask(&[128])),
            ],
        ));
        let names = |index: u32| {
            if index == 30 {
                "utun3".to_owned()
            } else {
                "en0".to_owned()
            }
        };
        let (routes, _) = parse_route_dump(&dump, LAYOUT, names);
        let best = route_to(&routes, PROBE).unwrap();
        assert_eq!(best.interface, "utun3");
        assert_eq!(best.prefix, 1);
        assert_eq!(best.gateway, None);
    }

    /// The layout `parse_route_dump` is given on a Mac is the system's.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn this_macs_routing_table_layout() {
        assert_eq!(std::mem::offset_of!(libc::rt_msghdr, rtm_index), 4);
        assert_eq!(std::mem::offset_of!(libc::rt_msghdr, rtm_msglen), 0);
    }

    /// The agent reads the network on a blocking thread of its runtime;
    /// asking NetworkManager there, with a runtime of its own, works (here
    /// without NetworkManager, it says nothing).
    #[cfg(target_os = "linux")]
    #[test]
    fn network_manager_is_asked_from_the_runtimes_blocking_threads() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let name = runtime.block_on(async {
            tokio::task::spawn_blocking(|| linux::wifi_name("lo", (None, None)))
                .await
                .expect("no panic")
        });
        assert!(name.as_ref().is_none_or(|n| !n.is_empty()));
        // Asked again soon, it is not asked anew.
        let again = runtime.block_on(async {
            tokio::task::spawn_blocking(|| linux::wifi_name("lo", (None, None)))
                .await
                .expect("no panic")
        });
        assert_eq!(name, again);
    }

    /// Reads this computer's network, whatever it is: no panic, and a
    /// router on a link with a hardware address has one too once resolved.
    #[test]
    fn this_computers_network_can_be_read() {
        match current() {
            Ok(Some(network)) => {
                assert!(!network.interface.is_empty() || cfg!(windows));
                if network.kind == LinkKind::Tunnel {
                    assert_eq!(network.router_mac, None);
                }
            }
            Ok(None) => {}
            Err(e) => panic!("cannot read the network: {e}"),
        }
    }
}
