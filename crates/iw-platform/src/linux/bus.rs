//! Reaching the session bus, again after it restarted: both the RemoteDesktop
//! portal's session and the screen-lock watcher connect again once their
//! connection broke.

/// A D-Bus address without the server's `guid`: each of its transports
/// (`;`-separated) with the `guid=` key (of its `,`-separated keys) left
/// out. The guid names one run of the bus, so a bus restarted at the same
/// address, with another guid, is turned away while it is in the address
/// (dbus-launch puts one in `DBUS_SESSION_BUS_ADDRESS`).
pub(crate) fn without_guid(address: &str) -> String {
    address
        .split(';')
        .map(|transport| match transport.split_once(':') {
            Some((kind, keys)) => {
                let keys: Vec<&str> = keys
                    .split(',')
                    .filter(|key| !key.starts_with("guid="))
                    .collect();
                format!("{kind}:{}", keys.join(","))
            }
            None => transport.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// A new connection to the session bus at `address`, or else the one
/// `DBUS_SESSION_BUS_ADDRESS` names, or else the default one in
/// `$XDG_RUNTIME_DIR`; without the address' guid, so a bus that restarted
/// there is reached too.
#[cfg(target_os = "linux")]
pub(crate) async fn session(address: Option<&str>) -> zbus::Result<zbus::Connection> {
    let address = address
        .map(str::to_owned)
        .or_else(|| std::env::var("DBUS_SESSION_BUS_ADDRESS").ok())
        .filter(|address| !address.is_empty());
    match address {
        Some(address) => {
            zbus::connection::Builder::address(without_guid(&address).as_str())?
                .build()
                .await
        }
        None => zbus::Connection::session().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_addresses_go_without_their_guid() {
        assert_eq!(
            without_guid("unix:path=/tmp/x/bus,guid=f6a77f65fbf3174264d2ffc16ae32a33"),
            "unix:path=/tmp/x/bus"
        );
        assert_eq!(
            without_guid("unix:guid=0123,abstract=/tmp/dbus-abc"),
            "unix:abstract=/tmp/dbus-abc"
        );
        assert_eq!(
            without_guid("unix:path=/run/user/1000/bus"),
            "unix:path=/run/user/1000/bus"
        );
        assert_eq!(
            without_guid("unix:path=/a,guid=1;tcp:host=localhost,port=1,guid=1"),
            "unix:path=/a;tcp:host=localhost,port=1"
        );
    }
}
