//! This computer's network interfaces, as far as waking it over the network
//! needs them (docs/architecture.md §5.6): which interface a connection runs
//! on, the interface's hardware address, and the broadcast address of its
//! IPv4 network.
//!
//! [`interfaces`] lists them: with `getifaddrs` on macOS (`AF_LINK` for the
//! hardware addresses) and Linux (`AF_PACKET`), with `GetAdaptersAddresses`
//! on Windows. [`wake_address`] picks from such a list how another device
//! on the same network wakes this one. The choice is plain Rust, tested on
//! every system with made-up lists.

use std::net::{IpAddr, Ipv4Addr};

use crate::PlatformError;

/// A network interface, as [`interfaces`] lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Interface {
    /// Its name, for logs: "en0", "eth0", an adapter's GUID on Windows.
    pub name: String,
    /// Its hardware (MAC) address, when it has one of 6 bytes; none for a
    /// loopback, a tunnel or most VPNs.
    pub mac: Option<[u8; 6]>,
    pub addresses: Vec<InterfaceAddress>,
    pub loopback: bool,
    /// Its index, as link-local IPv6 addresses name it in their scope; 0
    /// when not known.
    pub index: u32,
}

/// An address of an interface, with the length of its network's prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterfaceAddress {
    pub ip: IpAddr,
    pub prefix: u8,
}

/// How another device on this network wakes this one: the hardware address
/// of the interface, and the broadcast address of its IPv4 network, if it
/// has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WakeAddress {
    pub mac: [u8; 6],
    pub broadcast: Option<Ipv4Addr>,
}

/// How a device that reaches this one at `local` (this end of a
/// connection) wakes it: the interface with that address, if it has a
/// hardware address. Its IPv4 network gives the broadcast address: the
/// network of `local` itself, or over IPv6 the interface's own IPv4 network,
/// one routable before a link-local one. None over a loopback, and over an
/// interface without a hardware address, such as a VPN's: waking goes over
/// the network the device is on, not around it.
///
/// A link-local IPv6 address may be on several interfaces at once (as
/// macOS gives awdl0 and llw0 the same one): `scope`, the interface index
/// the connection's link-local address is scoped to, says which. None when
/// the interface cannot be told apart: no scope and more than one with the
/// address, or any other address on more than one.
pub fn wake_address(
    interfaces: &[Interface],
    local: IpAddr,
    scope: Option<u32>,
) -> Option<WakeAddress> {
    let local = local.to_canonical();
    if local.is_unspecified() || local.is_loopback() {
        return None;
    }
    let link_local = matches!(local, IpAddr::V6(ip) if ip.is_unicast_link_local());
    let mut carrying = interfaces
        .iter()
        .filter(|i| !i.loopback)
        .filter(|i| i.addresses.iter().any(|a| a.ip.to_canonical() == local))
        .filter(|i| !link_local || scope.is_none_or(|scope| i.index == scope));
    let interface = carrying.next()?;
    if carrying.next().is_some() {
        return None;
    }
    let mac = interface.mac?;
    let ipv4 = |a: &InterfaceAddress| match a.ip.to_canonical() {
        IpAddr::V4(ip) => Some((ip, a.prefix)),
        IpAddr::V6(_) => None,
    };
    let broadcast = match local {
        IpAddr::V4(local) => interface
            .addresses
            .iter()
            .filter_map(ipv4)
            .find(|(ip, _)| *ip == local)
            .and_then(|(ip, prefix)| broadcast_of(ip, prefix)),
        IpAddr::V6(_) => {
            let mut networks: Vec<(Ipv4Addr, u8)> =
                interface.addresses.iter().filter_map(ipv4).collect();
            // A routable network first; the order is otherwise the system's.
            networks.sort_by_key(|(ip, _)| ip.is_link_local());
            networks
                .into_iter()
                .find_map(|(ip, prefix)| broadcast_of(ip, prefix))
        }
    };
    Some(WakeAddress { mac, broadcast })
}

/// Whether `ip` lies in one of the IPv4 networks of `interfaces`, the
/// network's broadcast address included: so a magic packet to it stays on
/// a network this computer is on. Not the loopback's network, nor one
/// wider than a /8, which would take in a good part of the internet.
pub fn on_own_network(interfaces: &[Interface], ip: Ipv4Addr) -> bool {
    interfaces
        .iter()
        .filter(|i| !i.loopback)
        .flat_map(|i| &i.addresses)
        .filter_map(|a| match a.ip.to_canonical() {
            IpAddr::V4(own) => Some((own, a.prefix)),
            IpAddr::V6(_) => None,
        })
        .any(|(own, prefix)| {
            if !(8..=32).contains(&prefix) || own.is_loopback() {
                return false;
            }
            let mask = u32::MAX << (32 - u32::from(prefix));
            u32::from(own) & mask == u32::from(ip) & mask
        })
}

/// The interface in `list` an entry listed under `label` belongs to, added
/// if it is not there yet. Its name is the label up to a colon: Linux lists
/// an address added under an alias ("eth0:1") apart from its interface,
/// whose hardware address is listed under the interface's own name only.
#[cfg(any(unix, test))]
fn interface_for<'a>(list: &'a mut Vec<Interface>, label: &str) -> &'a mut Interface {
    let name = label.split_once(':').map_or(label, |(name, _)| name);
    let index = match list.iter().position(|i| i.name == name) {
        Some(index) => index,
        None => {
            list.push(Interface {
                name: name.to_owned(),
                ..Interface::default()
            });
            list.len() - 1
        }
    };
    &mut list[index]
}

/// The broadcast address of the IPv4 network of `ip` with a prefix of
/// `prefix` bits: none for a /31 or /32, which have none, nor for a prefix
/// that is no prefix (0, or over 32).
pub fn broadcast_of(ip: Ipv4Addr, prefix: u8) -> Option<Ipv4Addr> {
    if prefix == 0 || prefix >= 31 {
        return None;
    }
    let host = u32::MAX >> prefix;
    Some(Ipv4Addr::from(u32::from(ip) | host))
}

/// The length of the prefix a network mask says, counting its leading ones.
#[cfg(any(unix, test))]
fn prefix_of(mask: &[u8]) -> u8 {
    let mut prefix = 0u8;
    for byte in mask {
        let ones = byte.leading_ones() as u8;
        prefix += ones;
        if ones < 8 {
            break;
        }
    }
    prefix
}

/// This computer's network interfaces.
pub fn interfaces() -> Result<Vec<Interface>, PlatformError> {
    #[cfg(unix)]
    {
        unix::interfaces()
    }
    #[cfg(windows)]
    {
        win::interfaces()
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(PlatformError::Unsupported("listing network interfaces"))
    }
}

#[cfg(unix)]
mod unix {
    use std::ffi::CStr;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::{Interface, InterfaceAddress, prefix_of};
    use crate::PlatformError;

    pub(super) fn interfaces() -> Result<Vec<Interface>, PlatformError> {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: `list` is valid for writing; on success it is freed below.
        if unsafe { libc::getifaddrs(&mut list) } != 0 {
            return Err(PlatformError::Os(format!(
                "getifaddrs: {}",
                std::io::Error::last_os_error()
            )));
        }
        let mut out: Vec<Interface> = Vec::new();
        let mut cursor = list;
        while !cursor.is_null() {
            // SAFETY: an entry of the list getifaddrs made, alive until it
            // is freed.
            let entry = unsafe { &*cursor };
            cursor = entry.ifa_next;
            if entry.ifa_name.is_null() {
                continue;
            }
            // SAFETY: a NUL-terminated name in the list.
            let label = unsafe { CStr::from_ptr(entry.ifa_name) }.to_string_lossy();
            let interface = super::interface_for(&mut out, &label);
            if entry.ifa_flags & libc::IFF_LOOPBACK as libc::c_uint != 0 {
                interface.loopback = true;
            }
            // SAFETY: the addresses are null or point at sockaddrs of the
            // list, of their family's size.
            unsafe { add(interface, entry.ifa_addr, entry.ifa_netmask) };
        }
        // SAFETY: the list getifaddrs made, not used after this.
        unsafe { libc::freeifaddrs(list) };
        for interface in &mut out {
            if let Ok(name) = std::ffi::CString::new(interface.name.as_str()) {
                // SAFETY: a NUL-terminated name; 0 when there is none.
                interface.index = unsafe { libc::if_nametoindex(name.as_ptr()) };
            }
        }
        Ok(out)
    }

    /// Adds what `addr` (with `netmask`) says of `interface`: an address, or
    /// its hardware address.
    ///
    /// # Safety
    /// Each is null or points at a sockaddr of its family's size, as
    /// getifaddrs gives them.
    unsafe fn add(
        interface: &mut Interface,
        addr: *const libc::sockaddr,
        netmask: *const libc::sockaddr,
    ) {
        if addr.is_null() {
            return;
        }
        // SAFETY: as the caller promises.
        let family = libc::c_int::from(unsafe { (*addr).sa_family });
        match family {
            libc::AF_INET => {
                // SAFETY: an AF_INET address is a sockaddr_in.
                let sin = unsafe { &*addr.cast::<libc::sockaddr_in>() };
                let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                // SAFETY: as the caller promises.
                let mask = unsafe { mask_bytes(netmask, 4, 4) };
                interface.addresses.push(InterfaceAddress {
                    ip: IpAddr::V4(ip),
                    prefix: prefix_of(&mask),
                });
            }
            libc::AF_INET6 => {
                // SAFETY: an AF_INET6 address is a sockaddr_in6.
                let sin6 = unsafe { &*addr.cast::<libc::sockaddr_in6>() };
                let ip = Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                // SAFETY: as the caller promises.
                let mask = unsafe { mask_bytes(netmask, 8, 16) };
                interface.addresses.push(InterfaceAddress {
                    ip: IpAddr::V6(ip),
                    prefix: prefix_of(&mask),
                });
            }
            #[cfg(any(target_os = "linux", target_os = "android"))]
            libc::AF_PACKET => {
                // SAFETY: an AF_PACKET address is a sockaddr_ll.
                let ll = unsafe { &*addr.cast::<libc::sockaddr_ll>() };
                if ll.sll_halen == 6 {
                    let mut mac = [0; 6];
                    mac.copy_from_slice(&ll.sll_addr[..6]);
                    interface.mac = Some(mac);
                }
            }
            #[cfg(target_vendor = "apple")]
            libc::AF_LINK => {
                // SAFETY: as the caller promises.
                if let Some(mac) = unsafe { link_address(addr) } {
                    interface.mac = Some(mac);
                }
            }
            _ => {}
        }
    }

    /// `len` bytes of a netmask from `offset` (where a sockaddr_in or
    /// sockaddr_in6 has its address). On macOS a netmask's sockaddr may be
    /// shorter than its family's, leaving out zero bytes at the end, so only
    /// what its length says is read.
    ///
    /// # Safety
    /// `mask` is null or points at a netmask sockaddr from getifaddrs.
    unsafe fn mask_bytes(mask: *const libc::sockaddr, offset: usize, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        if mask.is_null() {
            return out;
        }
        #[cfg(target_vendor = "apple")]
        // SAFETY: as the caller promises.
        let available = usize::from(unsafe { (*mask).sa_len });
        #[cfg(not(target_vendor = "apple"))]
        let available = offset + len;
        let bytes = mask.cast::<u8>();
        for (i, byte) in out.iter_mut().enumerate() {
            if offset + i >= available {
                break;
            }
            // SAFETY: within the sockaddr's length.
            *byte = unsafe { *bytes.add(offset + i) };
        }
        out
    }

    /// The hardware address in an `AF_LINK` sockaddr, when it is 6 bytes.
    ///
    /// # Safety
    /// `addr` points at a sockaddr_dl from getifaddrs, `sdl_len` bytes.
    #[cfg(target_vendor = "apple")]
    unsafe fn link_address(addr: *const libc::sockaddr) -> Option<[u8; 6]> {
        // SAFETY: as the caller promises.
        let dl = unsafe { &*addr.cast::<libc::sockaddr_dl>() };
        if dl.sdl_alen != 6 {
            return None;
        }
        // Where sdl_data begins: after the 8 bytes before it.
        let start = 8 + usize::from(dl.sdl_nlen);
        if start + 6 > usize::from(dl.sdl_len) {
            return None;
        }
        let bytes = addr.cast::<u8>();
        let mut mac = [0u8; 6];
        for (i, byte) in mac.iter_mut().enumerate() {
            // SAFETY: within `sdl_len`, checked above.
            *byte = unsafe { *bytes.add(start + i) };
        }
        Some(mac)
    }
}

#[cfg(windows)]
mod win {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use ::windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, ERROR_SUCCESS};
    use ::windows::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_FRIENDLY_NAME,
        GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IF_TYPE_SOFTWARE_LOOPBACK,
        IP_ADAPTER_ADDRESSES_LH,
    };
    use ::windows::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6,
    };

    use super::{Interface, InterfaceAddress};
    use crate::PlatformError;

    pub(super) fn interfaces() -> Result<Vec<Interface>, PlatformError> {
        let flags = GAA_FLAG_SKIP_ANYCAST
            | GAA_FLAG_SKIP_MULTICAST
            | GAA_FLAG_SKIP_DNS_SERVER
            | GAA_FLAG_SKIP_FRIENDLY_NAME;
        // Microsoft's advice: start with 15 KB, and grow it as asked while
        // adapters come along meanwhile.
        let mut size: u32 = 15 * 1024;
        for _ in 0..4 {
            // u64s, for the alignment the structures need.
            let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
            let first = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
            // SAFETY: `first` points at `size` writable bytes.
            let rc = unsafe {
                GetAdaptersAddresses(u32::from(AF_UNSPEC.0), flags, None, Some(first), &mut size)
            };
            if rc == ERROR_SUCCESS.0 {
                // SAFETY: GetAdaptersAddresses filled the buffer, which
                // lives until this returns.
                return Ok(unsafe { read(first) });
            }
            if rc == ERROR_NO_DATA.0 {
                return Ok(Vec::new());
            }
            if rc != ERROR_BUFFER_OVERFLOW.0 {
                return Err(PlatformError::Os(format!(
                    "GetAdaptersAddresses: {}",
                    std::io::Error::from_raw_os_error(rc as i32)
                )));
            }
        }
        Err(PlatformError::Os(
            "GetAdaptersAddresses: the adapters kept changing".to_owned(),
        ))
    }

    /// The adapters in the list GetAdaptersAddresses made at `first`.
    ///
    /// # Safety
    /// `first` is null or the first of such a list, still alive.
    unsafe fn read(first: *const IP_ADAPTER_ADDRESSES_LH) -> Vec<Interface> {
        let mut out = Vec::new();
        let mut adapter = first;
        while !adapter.is_null() {
            // SAFETY: an adapter of the list.
            let a = unsafe { &*adapter };
            adapter = a.Next;
            // SAFETY: a NUL-terminated ANSI name, or null.
            let name = if a.AdapterName.is_null() {
                String::new()
            } else {
                unsafe { a.AdapterName.to_string() }.unwrap_or_default()
            };
            let mac = (a.PhysicalAddressLength == 6).then(|| {
                let mut mac = [0u8; 6];
                mac.copy_from_slice(&a.PhysicalAddress[..6]);
                mac
            });
            let mut addresses = Vec::new();
            let mut unicast = a.FirstUnicastAddress;
            while !unicast.is_null() {
                // SAFETY: an address of the adapter's list.
                let u = unsafe { &*unicast };
                unicast = u.Next;
                let sockaddr = u.Address.lpSockaddr;
                if sockaddr.is_null() {
                    continue;
                }
                // SAFETY: a sockaddr of `iSockaddrLength` bytes, at least
                // its family.
                let family = unsafe { (*sockaddr).sa_family };
                let ip = if family == AF_INET {
                    // SAFETY: an AF_INET address is a SOCKADDR_IN.
                    let sin = unsafe { &*sockaddr.cast::<SOCKADDR_IN>() };
                    // SAFETY: the union's every case is the same 4 bytes.
                    IpAddr::V4(Ipv4Addr::from(u32::from_be(unsafe {
                        sin.sin_addr.S_un.S_addr
                    })))
                } else if family == AF_INET6 {
                    // SAFETY: an AF_INET6 address is a SOCKADDR_IN6.
                    let sin6 = unsafe { &*sockaddr.cast::<SOCKADDR_IN6>() };
                    // SAFETY: the union's every case is the same 16 bytes.
                    IpAddr::V6(Ipv6Addr::from(unsafe { sin6.sin6_addr.u.Byte }))
                } else {
                    continue;
                };
                addresses.push(InterfaceAddress {
                    ip,
                    prefix: u.OnLinkPrefixLength,
                });
            }
            // SAFETY: the union's every case holds the IPv4 index here.
            let ipv4_index = unsafe { a.Anonymous1.Anonymous.IfIndex };
            out.push(Interface {
                name,
                mac,
                addresses,
                loopback: a.IfType == IF_TYPE_SOFTWARE_LOOPBACK,
                // The index link-local IPv6 addresses are scoped to; the
                // IPv4 one where IPv6 is off.
                index: if a.Ipv6IfIndex != 0 {
                    a.Ipv6IfIndex
                } else {
                    ipv4_index
                },
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIRED: [u8; 6] = [0x3c, 0x22, 0xfb, 0x10, 0x20, 0x30];
    const WIFI: [u8; 6] = [0x3c, 0x22, 0xfb, 0x40, 0x50, 0x60];

    fn v4(ip: [u8; 4], prefix: u8) -> InterfaceAddress {
        InterfaceAddress {
            ip: IpAddr::V4(Ipv4Addr::from(ip)),
            prefix,
        }
    }

    fn v6(ip: &str, prefix: u8) -> InterfaceAddress {
        InterfaceAddress {
            ip: IpAddr::V6(ip.parse().unwrap()),
            prefix,
        }
    }

    fn interface(name: &str, mac: Option<[u8; 6]>, addresses: Vec<InterfaceAddress>) -> Interface {
        Interface {
            name: name.to_owned(),
            mac,
            addresses,
            loopback: false,
            index: 0,
        }
    }

    /// A Mac with a wired and a Wi-Fi interface on two networks, a VPN, an
    /// interface with IPv6 only, and the loopback.
    fn mac_with_everything() -> Vec<Interface> {
        vec![
            Interface {
                name: "lo0".to_owned(),
                mac: None,
                addresses: vec![v4([127, 0, 0, 1], 8), v6("::1", 128)],
                loopback: true,
                index: 0,
            },
            interface(
                "en0",
                Some(WIRED),
                vec![
                    v6("fe80::1c2b:3c4d:5e6f:7081", 64),
                    v4([192, 168, 1, 20], 24),
                    v6("2001:db8::20", 64),
                ],
            ),
            interface("en1", Some(WIFI), vec![v4([10, 0, 7, 33], 16)]),
            interface(
                "utun3",
                None,
                vec![v4([100, 64, 0, 5], 32), v6("fd7a::5", 48)],
            ),
            interface(
                "en5",
                Some([2, 0, 0, 0, 0, 5]),
                vec![v6("2001:db8:5::5", 64)],
            ),
        ]
    }

    #[test]
    fn the_interface_carrying_the_connection_and_its_network() {
        let all = mac_with_everything();
        assert_eq!(
            wake_address(&all, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)), None),
            Some(WakeAddress {
                mac: WIRED,
                broadcast: Some(Ipv4Addr::new(192, 168, 1, 255)),
            })
        );
        // Over Wi-Fi, the Wi-Fi interface: each connection on its own.
        assert_eq!(
            wake_address(&all, IpAddr::V4(Ipv4Addr::new(10, 0, 7, 33)), None),
            Some(WakeAddress {
                mac: WIFI,
                broadcast: Some(Ipv4Addr::new(10, 0, 255, 255)),
            })
        );
    }

    #[test]
    fn nothing_over_a_vpn_the_loopback_or_an_address_not_here() {
        let all = mac_with_everything();
        assert_eq!(
            wake_address(&all, "100.64.0.5".parse().unwrap(), None),
            None
        );
        assert_eq!(wake_address(&all, "fd7a::5".parse().unwrap(), None), None);
        assert_eq!(wake_address(&all, "127.0.0.1".parse().unwrap(), None), None);
        assert_eq!(wake_address(&all, "::1".parse().unwrap(), None), None);
        assert_eq!(wake_address(&all, "0.0.0.0".parse().unwrap(), None), None);
        assert_eq!(
            wake_address(&all, "192.168.1.21".parse().unwrap(), None),
            None
        );
        assert_eq!(
            wake_address(&[], "192.168.1.20".parse().unwrap(), None),
            None
        );
        // A loopback with a hardware address all the same (some drivers).
        let lo = vec![Interface {
            name: "lo".to_owned(),
            mac: Some([0; 6]),
            addresses: vec![v4([127, 0, 0, 1], 8)],
            loopback: true,
            index: 0,
        }];
        assert_eq!(wake_address(&lo, "127.0.0.1".parse().unwrap(), None), None);
    }

    #[test]
    fn over_ipv6_the_interfaces_own_ipv4_network() {
        let all = mac_with_everything();
        for local in ["fe80::1c2b:3c4d:5e6f:7081", "2001:db8::20"] {
            assert_eq!(
                wake_address(&all, local.parse().unwrap(), None),
                Some(WakeAddress {
                    mac: WIRED,
                    broadcast: Some(Ipv4Addr::new(192, 168, 1, 255)),
                }),
                "{local}"
            );
        }
        // IPv6 only: the hardware address, and no network to broadcast to.
        assert_eq!(
            wake_address(&all, "2001:db8:5::5".parse().unwrap(), None),
            Some(WakeAddress {
                mac: [2, 0, 0, 0, 0, 5],
                broadcast: None,
            })
        );
        // A routable IPv4 network before a link-local one, whatever the
        // order.
        let both = vec![interface(
            "eth0",
            Some(WIRED),
            vec![
                v4([169, 254, 10, 2], 16),
                v6("fe80::2", 64),
                v4([172, 16, 4, 9], 22),
            ],
        )];
        assert_eq!(
            wake_address(&both, "fe80::2".parse().unwrap(), None).and_then(|w| w.broadcast),
            Some(Ipv4Addr::new(172, 16, 7, 255))
        );
        // Link-local only: that one's.
        let link_local = vec![interface(
            "eth0",
            Some(WIRED),
            vec![v4([169, 254, 10, 2], 16), v6("fe80::2", 64)],
        )];
        assert_eq!(
            wake_address(&link_local, "fe80::2".parse().unwrap(), None).and_then(|w| w.broadcast),
            Some(Ipv4Addr::new(169, 254, 255, 255))
        );
    }

    #[test]
    fn a_link_local_address_on_several_interfaces_needs_its_scope() {
        // As macOS gives awdl0 and llw0 the same link-local address.
        let with_index = |name: &str, mac: [u8; 6], index: u32| Interface {
            index,
            ..interface(name, Some(mac), vec![v6("fe80::1c2b:3c4d:5e6f:7081", 64)])
        };
        let all = vec![
            with_index("awdl0", [2, 0, 0, 0, 0, 9], 9),
            with_index("llw0", [2, 0, 0, 0, 0, 10], 10),
        ];
        let local: IpAddr = "fe80::1c2b:3c4d:5e6f:7081".parse().unwrap();
        // Without a scope, or with one neither has, it cannot be told.
        assert_eq!(wake_address(&all, local, None), None);
        assert_eq!(wake_address(&all, local, Some(3)), None);
        // With its scope, that one.
        assert_eq!(
            wake_address(&all, local, Some(10)).map(|w| w.mac),
            Some([2, 0, 0, 0, 0, 10])
        );
        assert_eq!(
            wake_address(&all, local, Some(9)).map(|w| w.mac),
            Some([2, 0, 0, 0, 0, 9])
        );
        // On one interface alone, no scope is needed; one that names
        // another interface still finds nothing.
        let one = vec![with_index("en0", WIRED, 4)];
        assert_eq!(wake_address(&one, local, None).map(|w| w.mac), Some(WIRED));
        assert_eq!(wake_address(&one, local, Some(5)), None);
        // Any other address on two interfaces cannot be told apart either;
        // a scope says nothing of it.
        let twice = vec![
            interface("en0", Some(WIRED), vec![v4([192, 168, 1, 20], 24)]),
            interface("bridge0", Some(WIFI), vec![v4([192, 168, 1, 20], 24)]),
        ];
        assert_eq!(
            wake_address(&twice, "192.168.1.20".parse().unwrap(), None),
            None
        );
        assert_eq!(
            wake_address(&twice, "192.168.1.20".parse().unwrap(), Some(4)),
            None
        );
    }

    #[test]
    fn a_link_local_address_has_its_own_network() {
        let all = vec![interface(
            "en0",
            Some(WIRED),
            vec![v4([169, 254, 33, 7], 16), v4([192, 168, 1, 20], 24)],
        )];
        assert_eq!(
            wake_address(&all, "169.254.33.7".parse().unwrap(), None).and_then(|w| w.broadcast),
            Some(Ipv4Addr::new(169, 254, 255, 255))
        );
    }

    #[test]
    fn an_ipv4_connection_on_a_dual_stack_socket() {
        let all = mac_with_everything();
        let mapped = IpAddr::V6(Ipv4Addr::new(192, 168, 1, 20).to_ipv6_mapped());
        assert_eq!(
            wake_address(&all, mapped, None).and_then(|w| w.broadcast),
            Some(Ipv4Addr::new(192, 168, 1, 255))
        );
        // A system that lists the address mapped matches too.
        let listed_mapped = vec![interface(
            "eth0",
            Some(WIRED),
            vec![InterfaceAddress {
                ip: IpAddr::V6(Ipv4Addr::new(10, 9, 2, 3).to_ipv6_mapped()),
                prefix: 120,
            }],
        )];
        assert_eq!(
            wake_address(&listed_mapped, "10.9.2.3".parse().unwrap(), None).map(|w| w.mac),
            Some(WIRED)
        );
    }

    #[test]
    fn point_to_point_networks_have_no_broadcast_address() {
        let all = vec![interface(
            "eth0",
            Some(WIRED),
            vec![v4([10, 9, 8, 7], 32), v4([10, 9, 9, 1], 31)],
        )];
        assert_eq!(
            wake_address(&all, "10.9.8.7".parse().unwrap(), None),
            Some(WakeAddress {
                mac: WIRED,
                broadcast: None,
            })
        );
        assert_eq!(
            wake_address(&all, "10.9.9.1".parse().unwrap(), None).and_then(|w| w.broadcast),
            None
        );
    }

    #[test]
    fn a_broadcast_address_counts_only_on_a_network_this_computer_is_on() {
        let all = mac_with_everything();
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        // The networks of en0 (/24) and en1 (/16), their broadcast
        // addresses included.
        for on in ["192.168.1.255", "192.168.1.7", "10.0.255.255", "10.0.0.1"] {
            assert!(on_own_network(&all, ip(on)), "{on}");
        }
        // Elsewhere: another network, the internet, a neighbouring /24.
        for off in ["192.168.2.255", "203.0.113.255", "8.8.8.8", "10.2.0.255"] {
            assert!(!on_own_network(&all, ip(off)), "{off}");
        }
        // The VPN's /32 is that one address alone; the loopback's network
        // never counts.
        assert!(on_own_network(&all, ip("100.64.0.5")));
        assert!(!on_own_network(&all, ip("100.64.0.255")));
        assert!(!on_own_network(&all, ip("127.255.255.255")));
        // Nor a network wider than a /8, nor no prefix at all.
        let wide = vec![interface(
            "eth0",
            Some(WIRED),
            vec![v4([10, 1, 2, 3], 4), v4([172, 16, 0, 1], 0)],
        )];
        assert!(!on_own_network(&wide, ip("8.8.8.8")));
        assert!(!on_own_network(&wide, ip("172.16.0.255")));
        assert!(!on_own_network(&[], ip("192.168.1.255")));
    }

    #[test]
    fn an_address_under_an_alias_label_goes_with_its_interface() {
        // As Linux lists them: the alias's address apart, and before the
        // interface's hardware address.
        let mut list = Vec::new();
        interface_for(&mut list, "eth0:1")
            .addresses
            .push(v4([192, 168, 5, 9], 24));
        interface_for(&mut list, "eth0").mac = Some(WIRED);
        interface_for(&mut list, "eth0")
            .addresses
            .push(v4([10, 0, 0, 2], 8));
        interface_for(&mut list, "wlan0").mac = Some(WIFI);
        interface_for(&mut list, "eth0:backup")
            .addresses
            .push(v4([172, 16, 1, 1], 16));
        let names: Vec<&str> = list.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["eth0", "wlan0"]);
        assert_eq!(list[0].addresses.len(), 3);
        // So a connection over the alias's address finds the hardware
        // address.
        assert_eq!(
            wake_address(&list, "192.168.5.9".parse().unwrap(), None),
            Some(WakeAddress {
                mac: WIRED,
                broadcast: Some(Ipv4Addr::new(192, 168, 5, 255)),
            })
        );
    }

    #[test]
    fn broadcast_addresses() {
        let ip = Ipv4Addr::new(192, 168, 1, 20);
        assert_eq!(broadcast_of(ip, 24), Some(Ipv4Addr::new(192, 168, 1, 255)));
        assert_eq!(broadcast_of(ip, 23), Some(Ipv4Addr::new(192, 168, 1, 255)));
        assert_eq!(broadcast_of(ip, 22), Some(Ipv4Addr::new(192, 168, 3, 255)));
        assert_eq!(broadcast_of(ip, 30), Some(Ipv4Addr::new(192, 168, 1, 23)));
        assert_eq!(broadcast_of(ip, 8), Some(Ipv4Addr::new(192, 255, 255, 255)));
        assert_eq!(broadcast_of(ip, 1), Some(Ipv4Addr::new(255, 255, 255, 255)));
        for prefix in [0, 31, 32, 33, 255] {
            assert_eq!(broadcast_of(ip, prefix), None, "/{prefix}");
        }
    }

    #[test]
    fn prefixes_from_masks() {
        assert_eq!(prefix_of(&[255, 255, 255, 0]), 24);
        assert_eq!(prefix_of(&[255, 255, 252, 0]), 22);
        assert_eq!(prefix_of(&[255, 255, 255, 255]), 32);
        assert_eq!(prefix_of(&[0, 0, 0, 0]), 0);
        // Not contiguous: as far as the ones run.
        assert_eq!(prefix_of(&[255, 0, 255, 0]), 8);
        assert_eq!(prefix_of(&[0xff; 16]), 128);
        let mut v6 = [0u8; 16];
        v6[..8].fill(0xff);
        assert_eq!(prefix_of(&v6), 64);
    }

    /// The real list: whatever it holds, it reads without failing, and no
    /// interface is listed twice.
    #[test]
    fn lists_this_computers_interfaces() {
        let listed = interfaces().unwrap();
        let mut names: Vec<&str> = listed.iter().map(|i| i.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), listed.len());
        for interface in &listed {
            for address in &interface.addresses {
                let most = if address.ip.is_ipv4() { 32 } else { 128 };
                assert!(address.prefix <= most, "{interface:?}");
            }
        }
    }
}
