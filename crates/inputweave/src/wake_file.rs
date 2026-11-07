//! The wake file (`wake.toml`, beside the devices file): how other devices
//! told this one to wake them (`agent::wake`), when each was last seen
//! connected, and whether pushing the cursor toward an offline device
//! wakes it. The agent writes it. It is a file of its own, so the devices
//! file stays one that agents from before waking read.
//!
//! Entries are by device id, for paired devices and those listed in the
//! configuration file alike, each with the fingerprint of the device that
//! told it: another device under the same id (a configuration file edited
//! to trust another computer under it) is not taken for it. Unpairing a
//! device drops its entry, and pairing it again under another id moves the
//! entry to that id.
//!
//! Read leniently: an entry that cannot be read, as edited by hand or
//! without a fingerprint, is dropped with a warning and the rest kept, and
//! so is a switch that is not one; a file that cannot be read at all is
//! passed over with a warning, and the agent starts without it (writing a
//! whole one at its next change).
//!
//! ```toml
//! wake_on_push = true
//!
//! [devices.win]
//! fingerprint = "aa:bb:…"
//! mac = "3c:22:fb:10:20:30"
//! broadcast = "192.168.1.255"
//! last_seen = 1762480000
//! ```

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::Path;
use std::time::SystemTime;

use iw_net::Fingerprint;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracing::warn;

use crate::agent::WakeData;

const HEADER: &str = "# How to wake other devices, written by the InputWeave agent.\n\
                      # Edit only while the agent is stopped.\n\n";

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct WakeFile {
    /// Pushing the cursor toward an offline device wakes it, as turned on
    /// from the UI.
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "switch_in"
    )]
    pub wake_on_push: bool,
    /// How other devices told this one to wake them, by device id.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "devices_out",
        deserialize_with = "devices_in"
    )]
    pub devices: BTreeMap<String, KnownWake>,
}

/// How to wake another device, as the device with `fingerprint` told this
/// one, and when it was last seen connected, in seconds since 1970, if
/// since it told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnownWake {
    pub fingerprint: Fingerprint,
    pub data: WakeData,
    pub last_seen: Option<u64>,
}

impl WakeFile {
    /// Reads the file. A missing file is an empty one; one that cannot be
    /// read or is not TOML is passed over, with a warning, for an empty
    /// one, so the agent starts all the same.
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "cannot read the wake file; starting without it");
                return Self::default();
            }
        };
        Self::parse(&text).unwrap_or_else(|e| {
            warn!(path = %path.display(), error = %e, "the wake file is not one; starting without it");
            Self::default()
        })
    }

    /// The file as `text` holds it.
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Replaces the file in one step, readable only by its owner.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        crate::store::write_private(path, HEADER, &text)
    }
}

/// A switch that is not true or false, as edited by hand, is off, with a
/// warning; the rest of the file is read as usual.
fn switch_in<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    match toml::Value::deserialize(d)? {
        toml::Value::Boolean(on) => Ok(on),
        other => {
            warn!(value = %other, "taking a wake_on_push that is not true or false as off");
            Ok(false)
        }
    }
}

/// An entry as written.
#[derive(Serialize)]
struct Entry {
    fingerprint: String,
    mac: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    broadcast: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_seen: Option<u64>,
}

fn devices_out<S: Serializer>(
    wakes: &BTreeMap<String, KnownWake>,
    s: S,
) -> Result<S::Ok, S::Error> {
    s.collect_map(wakes.iter().map(|(id, wake)| {
        (
            id,
            Entry {
                fingerprint: wake.fingerprint.to_string(),
                mac: wake.data.mac_text(),
                broadcast: wake.data.broadcast().map(|ip| ip.to_string()),
                last_seen: wake.last_seen,
            },
        )
    }))
}

/// Each entry on its own: one that does not say how to wake a device (no
/// fingerprint, no hardware address or one that wakes nothing, a broadcast
/// address that is none) is dropped with a warning, and the rest of the
/// file is read as usual; a time it was last seen at that is none is not
/// known, the rest of the entry kept. Keys it does not know are left out.
fn devices_in<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, KnownWake>, D::Error> {
    let entries = match toml::Value::deserialize(d)? {
        toml::Value::Table(entries) => entries,
        other => {
            warn!(value = %other, "dropping how to wake other devices: not a table");
            return Ok(BTreeMap::new());
        }
    };
    Ok(entries
        .into_iter()
        .filter_map(|(id, value)| {
            let wake = known_wake(&value);
            if wake.is_none() {
                warn!(%id, %value, "dropping how to wake a device that is not that");
            }
            Some((id, wake?))
        })
        .collect())
}

/// Whether `secs` since 1970 is a time a device may have been last seen at,
/// `now` being now: from 2020 to a day from now (clocks differ a little).
fn plausible(secs: u64, now: SystemTime) -> bool {
    const SINCE: u64 = 1_577_836_800; // 2020-01-01
    let latest = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        + 24 * 60 * 60;
    (SINCE..=latest).contains(&secs)
}

fn known_wake(value: &toml::Value) -> Option<KnownWake> {
    let entry = value.as_table()?;
    let fingerprint = entry.get("fingerprint")?.as_str()?.parse().ok()?;
    let mac = WakeData::parse_mac(entry.get("mac")?.as_str()?)?;
    let broadcast = match entry.get("broadcast") {
        None => None,
        Some(ip) => Some(ip.as_str()?.parse::<Ipv4Addr>().ok()?),
    };
    let last_seen = match entry.get("last_seen") {
        None => None,
        // A time no device was ever seen at, or none at all, as edited by
        // hand: not known, rather than shown, the rest of the entry kept.
        Some(at) => match at.as_integer().and_then(|secs| u64::try_from(secs).ok()) {
            Some(secs) if plausible(secs, SystemTime::now()) => Some(secs),
            _ => {
                warn!(
                    %at,
                    "taking a last_seen that is no time a device was seen as not known"
                );
                None
            }
        },
    };
    Some(KnownWake {
        fingerprint,
        data: WakeData::new(mac, broadcast)?,
        last_seen,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(byte: u8) -> Fingerprint {
        Fingerprint::from_bytes([byte; 32])
    }

    fn sample() -> WakeFile {
        WakeFile {
            wake_on_push: true,
            devices: [(
                "win".to_owned(),
                KnownWake {
                    fingerprint: fp(1),
                    data: WakeData::new(
                        [0x3c, 0x22, 0xfb, 0x10, 0x20, 0x30],
                        Some(Ipv4Addr::new(192, 168, 1, 255)),
                    )
                    .unwrap(),
                    last_seen: Some(1_762_480_000),
                },
            )]
            .into(),
        }
    }

    #[test]
    fn saves_and_loads_how_to_wake_devices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("wake.toml");
        assert_eq!(WakeFile::load(&path), WakeFile::default());
        let mut file = sample();
        file.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(HEADER), "{text}");
        assert!(text.contains("wake_on_push = true\n"), "{text}");
        let win = format!(
            "[devices.win]\nfingerprint = \"{}\"\nmac = \"3c:22:fb:10:20:30\"\nbroadcast = \"192.168.1.255\"\nlast_seen = 1762480000\n",
            fp(1)
        );
        assert!(text.contains(&win), "{text}");
        assert_eq!(WakeFile::load(&path), file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Without a broadcast address or a time, they are left out; off,
        // the switch is too.
        file.devices.insert(
            "mini".to_owned(),
            KnownWake {
                fingerprint: fp(2),
                data: WakeData::new([2, 0, 0, 0, 0, 1], None).unwrap(),
                last_seen: None,
            },
        );
        file.wake_on_push = false;
        file.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let mini = format!(
            "[devices.mini]\nfingerprint = \"{}\"\nmac = \"02:00:00:00:00:01\"\n\n",
            fp(2)
        );
        assert!(text.contains(&mini), "{text}");
        assert!(!text.contains("wake_on_push"), "{text}");
        assert_eq!(WakeFile::load(&path), file);
    }

    #[test]
    fn an_entry_that_cannot_be_read_is_dropped_alone() {
        let with = |rest: &str| format!("fingerprint = \"{}\"\n{rest}", fp(1));
        let text = [
            "wake_on_push = true\nlater = \"kept out\"\n\n".to_owned(),
            format!(
                "[devices.win]\n{}\n",
                with("mac = \"3C-22-FB-10-20-30\"\nbroadcast = \"192.168.1.255\"\nlast_seen = 1762480000\nlater = \"kept out\"\n")
            ),
            "[devices.nofingerprint]\nmac = \"3c:22:fb:10:20:3f\"\n\n".to_owned(),
            "[devices.badfingerprint]\nfingerprint = \"not one\"\nmac = \"3c:22:fb:10:20:3e\"\n\n"
                .to_owned(),
            format!("[devices.nomac]\n{}\n", with("broadcast = \"192.168.1.255\"\n")),
            format!("[devices.zero]\n{}\n", with("mac = \"00:00:00:00:00:00\"\n")),
            format!("[devices.group]\n{}\n", with("mac = \"01:00:5e:00:00:01\"\n")),
            format!("[devices.short]\n{}\n", with("mac = \"3c:22:fb:10:20\"\n")),
            format!("[devices.number]\n{}\n", with("mac = 5\n")),
            format!(
                "[devices.badcast]\n{}\n",
                with("mac = \"3c:22:fb:10:20:31\"\nbroadcast = \"127.255.255.255\"\n")
            ),
            format!(
                "[devices.v6]\n{}\n",
                with("mac = \"3c:22:fb:10:20:32\"\nbroadcast = \"ff02::1\"\n")
            ),
            format!(
                "[devices.mini]\n{}",
                with("mac = \"3c:22:fb:10:20:35\"\nbroadcast = \"255.255.255.255\"\n")
            ),
        ]
        .concat();
        let file = WakeFile::parse(&text).unwrap();
        assert!(file.wake_on_push);
        let ids: Vec<&str> = file.devices.keys().map(String::as_str).collect();
        assert_eq!(ids, ["mini", "win"]);
        let win = file.devices["win"];
        assert_eq!(win.fingerprint, fp(1));
        assert_eq!(win.data.mac(), [0x3c, 0x22, 0xfb, 0x10, 0x20, 0x30]);
        assert_eq!(win.data.broadcast(), Some(Ipv4Addr::new(192, 168, 1, 255)));
        assert_eq!(win.last_seen, Some(1_762_480_000));
        // Where every wake-up goes anyway counts as none.
        assert_eq!(file.devices["mini"].data.broadcast(), None);
        // Not a table: only it goes.
        let file = WakeFile::parse("wake_on_push = true\ndevices = 5\n").unwrap();
        assert!(file.wake_on_push && file.devices.is_empty());
        // A switch that is not one is off; the devices stay.
        let file = WakeFile::parse(&format!(
            "wake_on_push = \"yes\"\n\n[devices.mini]\n{}",
            with("mac = \"02:00:00:00:00:01\"\n")
        ))
        .unwrap();
        assert!(!file.wake_on_push && file.devices.contains_key("mini"));
    }

    #[test]
    fn a_time_no_device_was_seen_at_is_not_known() {
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_762_480_000);
        assert!(plausible(1_762_480_000, now));
        assert!(
            plausible(1_762_480_000 + 3600, now),
            "a clock a little ahead"
        );
        assert!(plausible(1_577_836_800, now));
        for absurd in [
            0,
            5,
            1_577_836_799,
            1_762_480_000 + 2 * 86_400,
            u64::MAX / 2,
        ] {
            assert!(!plausible(absurd, now), "{absurd}");
        }
        // Read: the entry stays, without the time, whether it is one no
        // device was seen at or none at all.
        for (n, at) in [
            "9223372036854775807",
            "-1",
            "\"yesterday\"",
            "1762480000.5",
            "true",
            "2025-11-07T02:00:00Z",
            "[1762480000]",
        ]
        .into_iter()
        .enumerate()
        {
            let file = WakeFile::parse(&format!(
                "[devices.win]\nfingerprint = \"{}\"\nmac = \"02:00:00:00:00:0{n}\"\nbroadcast = \"192.168.1.255\"\nlast_seen = {at}\n",
                fp(1)
            ))
            .unwrap();
            let win = file.devices.get("win").unwrap_or_else(|| panic!("{at}"));
            assert_eq!(win.last_seen, None, "{at}");
            assert_eq!(win.fingerprint, fp(1), "{at}");
            assert_eq!(win.data.mac(), [2, 0, 0, 0, 0, n as u8], "{at}");
            assert_eq!(
                win.data.broadcast(),
                Some(Ipv4Addr::new(192, 168, 1, 255)),
                "{at}"
            );
        }
    }

    #[test]
    fn a_file_that_cannot_be_read_at_all_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wake.toml");
        for broken in ["this is not toml [", "devices = [1, 2\n", "\u{0}\u{1}"] {
            std::fs::write(&path, broken).unwrap();
            assert_eq!(WakeFile::load(&path), WakeFile::default(), "{broken:?}");
        }
        // Not text at all.
        std::fs::write(&path, [0xff, 0xfe, 0x00, 0x80]).unwrap();
        assert_eq!(WakeFile::load(&path), WakeFile::default());
        // A folder where the file goes.
        let folder = dir.path().join("folder.toml");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(WakeFile::load(&folder), WakeFile::default());
    }
}
