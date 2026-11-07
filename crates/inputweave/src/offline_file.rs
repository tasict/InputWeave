//! The devices of the group taken offline for now, as this device knows
//! the group's document of them (`OfflineDevices` in the peer protocol,
//! docs/architecture.md §6.4), and the file it is kept in (`offline.toml`,
//! beside the devices file). The agent writes it. It is a file of its own,
//! so the devices file stays one that agents from before read.
//!
//! The document is the group's, not this device's: it lists devices this
//! one may not know, and keeps a device unpaired here, since others still
//! take it as offline.
//!
//! # One entry for each device, merged on its own
//!
//! Each device listed has an entry of its own: offline, or back online,
//! with the version that made it so and its author. A document another
//! device sends is merged entry by entry: for each device, the newer entry
//! wins (a higher version, or the same version by a later author), so two
//! devices taking different devices offline at once, or one apart from
//! the group for a while, never undo each other's changes; only changes
//! to the same device compete, and the later one stands. A new entry's
//! version is one more than the highest in the document (a Lamport clock),
//! so a change made after seeing another always comes after it.
//!
//! # Devices brought back, and when they are forgotten
//!
//! Bringing a device back keeps its entry, marked back online (a
//! tombstone), rather than removing it. Without it, a device that was apart
//! from the group when the device was brought back, still holding the entry
//! that took it offline, would send that entry again later, and every
//! device would take the device offline again, there being nothing newer to
//! compare it with.
//!
//! A tombstone is needed only until every device that may still hold an
//! older entry for that device has merged it. No device can tell when that
//! is: it does not know every device of the group, nor what those apart
//! from it hold. So none is forgotten by age. They are forgotten only to
//! make room, when a document would hold more than [`MAX_ENTRIES`] (far
//! more devices than a group has): the oldest first, so the one forgotten
//! is the one every device has had the longest to merge, and never one a
//! merge just compared with the other device's entry for that device, as
//! that tombstone is what overrides the other's older entry, and goes back
//! to it. Forgetting one can let an older entry for that device win again
//! only on a device that has been apart from the group since before it was
//! made and still holds such an entry.
//!
//! # Limits
//!
//! One limit, [`MAX_ENTRIES`], holds for every document: received, merged,
//! sent and kept in the file, so a document one device holds is always one
//! the others take. Past it, the oldest tombstones go first; only if
//! devices offline alone are more than that, which takes two parts of a
//! group apart from each other taking hundreds of devices offline, do the
//! oldest of them go too, with a warning. Taking another device offline
//! here is refused once [`MAX_TAKEN`] are offline, so the document never
//! fills up with them by this device's doing. A full document's file stays
//! well within what the macOS login window's agent reads of it.
//!
//! # The file
//!
//! Read leniently, with the same checks as a document another device sends
//! (`Entry::checked`), so whatever was taken in is read back: an entry under
//! an id that is none, or with an author that is none, is dropped with a
//! warning and the rest kept; a device that took another offline that is
//! not known (missing, empty, or not an id) leaves the entry offline, with
//! no one said to have done it; a name that is none is left out. A file
//! that cannot be read at all is passed over with a warning, and the agent
//! starts without it (writing a whole one at its next change).
//!
//! ```toml
//! [devices.win]
//! offline = true
//! version = 3
//! author = "mac"
//! by = "mac"
//! by_name = "MacBook Pro"
//!
//! [devices.mini]
//! offline = false
//! version = 5
//! author = "win"
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use iw_proto::peer::v1 as peer;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracing::warn;

const HEADER: &str = "# The devices taken offline for now, written by the InputWeave agent.\n\
                      # Edit only while the agent is stopped.\n\n";

/// The most entries any document holds, offline or back online: received,
/// merged, sent or kept (see the module docs). A document from another
/// device with more is ignored.
pub const MAX_ENTRIES: usize = 1024;

/// The most devices offline for now that taking another offline here
/// allows; past it, that is refused.
pub const MAX_TAKEN: usize = 256;

/// The most of the file the macOS login window's agent reads; a full
/// document's file is far smaller (see the module docs).
pub const MAX_FILE: u64 = 4 << 20;

/// The bound versions stay below: far below what the file and the
/// protocol could hold, so it is never reached. Versions count changes
/// people make, one more than the highest they knew of: a group taking a
/// device offline or bringing one back every second would need nine
/// million years. An entry at it or above, as from a device that is broken
/// or means harm, is not taken in, so it can never pin a device's entry at
/// a version no later change could pass; a change here that could not
/// come after every entry is refused rather than made.
pub const MAX_VERSION: u64 = 1 << 48;

/// The group's document of the devices taken offline for now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct OfflineDoc {
    /// By device id.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "devices_out",
        deserialize_with = "devices_in"
    )]
    pub devices: BTreeMap<String, Entry>,
}

/// One device's entry: offline, or back online, as the change numbered
/// `version` by `author` made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub version: u64,
    /// The device that made this change; empty when not known.
    pub author: String,
    pub offline: bool,
    /// The device that took it offline, while it is, when that is known.
    pub by: Option<String>,
    /// The name that device gave itself then, for devices that do not know
    /// it by another; empty when not known.
    pub by_name: String,
}

/// The device taking another offline, and its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TakenBy {
    pub device: String,
    pub name: String,
}

/// Why taking a device offline was refused (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Full {
    /// [`MAX_TAKEN`] other devices are offline already.
    TooMany,
    /// The document is full of devices offline, merged from elsewhere,
    /// with none back online to forget for room.
    ListFull,
    /// No version is left above every one in the document: only entries
    /// no device takes in could bring that about.
    Exhausted,
}

/// What merging another device's document did (`OfflineDoc::merge`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Merged {
    /// Whether any entry here changed.
    pub changed: bool,
    /// Whether the other device's document lacks an entry here, or has an
    /// older one: it should hear this one.
    pub behind: bool,
}

impl Entry {
    /// Whether this entry comes after `other`: a higher version, or the
    /// same version by a later author. Two entries of the same change
    /// (version and author) should be the same; should they differ, as
    /// from a device that is broken, the one offline, then by its setter
    /// and the setter's name, comes after, so every device settles on the
    /// same one.
    pub fn newer_than(&self, other: &Entry) -> bool {
        self.order() > other.order()
    }

    fn order(&self) -> (u64, &str, bool, Option<&str>, &str) {
        (
            self.version,
            &self.author,
            self.offline,
            self.by.as_deref(),
            &self.by_name,
        )
    }

    /// The entry of `device` as another device sent it or the file holds
    /// it, if it can be one: the checks both go through, so whatever was
    /// taken in is read back. `None` for an id or an author that is none
    /// (every change has one), or a version not below [`MAX_VERSION`]. A
    /// device that took it offline that
    /// is none is not known, and a name that is none is empty; neither
    /// drops the entry, and neither is kept for a device back online.
    pub fn checked(
        device: &str,
        version: u64,
        author: &str,
        offline: bool,
        by: Option<&str>,
        by_name: &str,
    ) -> Option<Entry> {
        if !iw_net::is_valid_device_id(device)
            || !iw_net::is_valid_device_id(author)
            || version >= MAX_VERSION
        {
            return None;
        }
        let by = by
            .filter(|by| offline && iw_net::is_valid_device_id(by))
            .map(str::to_owned);
        let by_name = if offline && iw_net::is_valid_device_name(by_name) {
            by_name.to_owned()
        } else {
            String::new()
        };
        Some(Entry {
            version,
            author: author.to_owned(),
            offline,
            by,
            by_name,
        })
    }
}

impl OfflineDoc {
    /// Whether `device` is offline for now.
    pub fn contains(&self, device: &str) -> bool {
        self.devices.get(device).is_some_and(|e| e.offline)
    }

    /// The entry of `device` while it is offline for now.
    pub fn taken_by(&self, device: &str) -> Option<&Entry> {
        self.devices.get(device).filter(|e| e.offline)
    }

    /// Whether nothing goes between device `this` and `device`: either is
    /// offline for now. The one test of it.
    pub fn between(&self, this: &str, device: &str) -> bool {
        self.contains(this) || self.contains(device)
    }

    /// The devices offline for now.
    pub fn offline(&self) -> impl Iterator<Item = (&String, &Entry)> {
        self.devices.iter().filter(|(_, e)| e.offline)
    }

    /// The version the next change made here gets: above every one in the
    /// document. `None` once that would reach [`MAX_VERSION`], which only
    /// entries no device takes in could bring about.
    fn next_version(&self) -> Option<u64> {
        let next = self
            .devices
            .values()
            .map(|e| e.version)
            .max()
            .unwrap_or(0)
            .checked_add(1)?;
        (next < MAX_VERSION).then_some(next)
    }

    /// The document with `device` taken offline by `by` (`offline`) or
    /// brought back, in a change made by `author`, which only that device's
    /// entry carries. `Ok(None)` when that changes nothing, as for a device
    /// offline already, whoever took it so, or one not listed offline being
    /// brought back. Taking another device offline is refused once
    /// [`MAX_TAKEN`] are (taking `author`, this device, never is); a new
    /// entry that does not fit makes room by forgetting the oldest device
    /// back online, and is refused when there is none.
    pub fn changed(
        &self,
        device: &str,
        offline: bool,
        by: TakenBy,
        author: &str,
    ) -> Result<Option<OfflineDoc>, Full> {
        if self.contains(device) == offline {
            return Ok(None);
        }
        // Taking this device offline is never limited; another is.
        if offline && device != author && self.offline().count() >= MAX_TAKEN {
            return Err(Full::TooMany);
        }
        let mut next = self.clone();
        let entry = Entry {
            version: self.next_version().ok_or(Full::Exhausted)?,
            author: author.to_owned(),
            offline,
            by: offline.then(|| by.device.clone()),
            by_name: if offline { by.name } else { String::new() },
        };
        // Never in place of one as new or newer, which would not reach the
        // other devices.
        if self
            .devices
            .get(device)
            .is_some_and(|current| !entry.newer_than(current))
        {
            return Err(Full::Exhausted);
        }
        if !next.devices.contains_key(device) && next.devices.len() >= MAX_ENTRIES {
            let oldest = next
                .oldest_tombstone(&BTreeSet::new())
                .ok_or(Full::ListFull)?;
            next.devices.remove(&oldest);
        }
        next.devices.insert(device.to_owned(), entry);
        Ok(Some(next))
    }

    /// The oldest device back online whose id `kept` does not hold.
    fn oldest_tombstone(&self, kept: &BTreeSet<String>) -> Option<String> {
        self.devices
            .iter()
            .filter(|(id, e)| !e.offline && !kept.contains(*id))
            .min_by(|(_, a), (_, b)| a.order().cmp(&b.order()))
            .map(|(id, _)| id.clone())
    }

    /// Brings the document within [`MAX_ENTRIES`]: the oldest devices back
    /// online first, but none `kept` holds; then, only when devices offline
    /// alone are too many, the oldest of every entry, with a warning. The
    /// devices forgotten.
    fn fit(&mut self, kept: &BTreeSet<String>) -> Vec<String> {
        let mut forgotten = Vec::new();
        while self.devices.len() > MAX_ENTRIES {
            if let Some(id) = self.oldest_tombstone(kept) {
                self.devices.remove(&id);
                forgotten.push(id);
                continue;
            }
            let Some(oldest) = self
                .devices
                .iter()
                .min_by(|(_, a), (_, b)| a.order().cmp(&b.order()))
                .map(|(id, _)| id.clone())
            else {
                return forgotten;
            };
            warn!(
                device = oldest,
                "forgetting the oldest device taken offline for now: the group lists more than can be kept"
            );
            self.devices.remove(&oldest);
            forgotten.push(oldest);
        }
        forgotten
    }

    /// Merges `other`'s document into this one, entry by entry, the newer
    /// winning for each device, then brings it within [`MAX_ENTRIES`]
    /// (`fit`), keeping every entry the merge compared with one of
    /// `other`'s. What it did is told from the document it leaves.
    pub fn merge(&mut self, other: &OfflineDoc) -> Merged {
        let mut compared = BTreeSet::new();
        // The devices whose entry the merge took, and whether one was here
        // before.
        let mut taken = BTreeMap::new();
        for (id, theirs) in &other.devices {
            compared.insert(id.clone());
            match self.devices.get(id) {
                Some(ours) if !theirs.newer_than(ours) => {}
                _ => {
                    let was = self.devices.insert(id.clone(), theirs.clone());
                    taken.insert(id.clone(), was.is_some());
                }
            }
        }
        let forgotten = self.fit(&compared);
        // Changed: an entry taken and kept, one in place of an entry here,
        // or an entry here forgotten. Not one taken and forgotten again.
        let changed = taken
            .iter()
            .any(|(id, was)| *was || self.devices.contains_key(id))
            || forgotten.iter().any(|id| !taken.contains_key(id));
        Merged {
            changed,
            behind: self.devices.iter().any(|(id, ours)| {
                other
                    .devices
                    .get(id)
                    .is_none_or(|theirs| ours.newer_than(theirs))
            }),
        }
    }

    /// The document another device sent, if it can be one: at most
    /// [`MAX_ENTRIES`] entries, each checked as the file's are
    /// (`Entry::checked`), an entry that cannot be one left out alone; a
    /// device listed twice is taken once, as it came first.
    pub fn from_proto(doc: peer::OfflineDevices) -> Option<OfflineDoc> {
        if doc.devices.len() > MAX_ENTRIES {
            return None;
        }
        let mut devices = BTreeMap::new();
        for d in doc.devices {
            let by = (!d.by_device_id.is_empty()).then_some(d.by_device_id.as_str());
            match Entry::checked(
                &d.device_id,
                d.version,
                &d.author,
                d.offline,
                by,
                &d.by_name,
            ) {
                Some(entry) => {
                    devices.entry(d.device_id).or_insert(entry);
                }
                None => warn!(
                    device = d.device_id,
                    "ignoring an entry of the devices taken offline that is not one"
                ),
            }
        }
        Some(OfflineDoc { devices })
    }

    /// The document as other devices are sent it: never more than they
    /// take ([`MAX_ENTRIES`], which every document here is within).
    pub fn to_proto(&self) -> peer::OfflineDevices {
        peer::OfflineDevices {
            devices: self
                .devices
                .iter()
                .take(MAX_ENTRIES)
                .map(|(id, e)| peer::OfflineDevice {
                    device_id: id.clone(),
                    offline: e.offline,
                    version: e.version,
                    author: e.author.clone(),
                    by_device_id: e.by.clone().unwrap_or_default(),
                    by_name: e.by_name.clone(),
                })
                .collect(),
        }
    }

    /// Reads the file. A missing file is an empty document; one that cannot
    /// be read or is not TOML is passed over, with a warning, for an empty
    /// one, so the agent starts all the same.
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "cannot read the offline file; starting without it");
                return Self::default();
            }
        };
        Self::parse(&text).unwrap_or_else(|e| {
            warn!(path = %path.display(), error = %e, "the offline file is not one; starting without it");
            Self::default()
        })
    }

    /// The document as `text` holds it, within [`MAX_ENTRIES`] (a file
    /// edited by hand may hold more).
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        let mut doc: Self = toml::from_str(text)?;
        let _ = doc.fit(&BTreeSet::new());
        Ok(doc)
    }

    /// Replaces the file in one step, readable only by its owner.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        crate::store::write_private(path, HEADER, &text)
    }
}

/// Who took a device offline for now, for `ctl` and diagnostics reports:
/// this device; another by name and id, or by whichever of them is known;
/// or, when neither is, another computer. An empty id is never this
/// device.
pub fn describe_setter(this_device: &str, by_id: &str, by_name: &str) -> String {
    match (by_id, by_name) {
        (id, _) if !id.is_empty() && id == this_device => "set on this device".to_owned(),
        ("", "") => "set by another computer".to_owned(),
        ("", name) => format!("set by {name}"),
        (id, "") => format!("set by {id}"),
        (id, name) => format!("set by {name} ({id})"),
    }
}

/// An entry as written.
#[derive(Serialize)]
struct Written<'a> {
    offline: bool,
    version: u64,
    #[serde(skip_serializing_if = "str::is_empty")]
    author: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    by: Option<&'a str>,
    #[serde(skip_serializing_if = "str::is_empty")]
    by_name: &'a str,
}

fn devices_out<S: Serializer>(devices: &BTreeMap<String, Entry>, s: S) -> Result<S::Ok, S::Error> {
    s.collect_map(devices.iter().map(|(id, e)| {
        (
            id,
            Written {
                offline: e.offline,
                version: e.version,
                author: &e.author,
                by: e.by.as_deref(),
                by_name: &e.by_name,
            },
        )
    }))
}

/// Each entry on its own, through `Entry::checked`: one that cannot be
/// read is dropped with a warning, and the rest of the file is read as
/// usual. `offline` missing is offline; a version missing is 0. Keys it
/// does not know are left out.
fn devices_in<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, Entry>, D::Error> {
    let entries = match toml::Value::deserialize(d)? {
        toml::Value::Table(entries) => entries,
        other => {
            warn!(value = %other, "dropping the devices taken offline: not a table");
            return Ok(BTreeMap::new());
        }
    };
    let mut devices = BTreeMap::new();
    for (id, value) in entries {
        match entry_in(&id, &value) {
            Some(entry) => {
                devices.insert(id, entry);
            }
            None => {
                warn!(%id, %value, "dropping an entry of the devices taken offline that is not one")
            }
        }
    }
    Ok(devices)
}

fn entry_in(id: &str, value: &toml::Value) -> Option<Entry> {
    let entry = value.as_table()?;
    let offline = match entry.get("offline") {
        None => true,
        Some(v) => v.as_bool()?,
    };
    let version = match entry.get("version") {
        None => 0,
        Some(v) => u64::try_from(v.as_integer()?).ok()?,
    };
    let author = match entry.get("author") {
        None => "",
        Some(v) => v.as_str()?,
    };
    let by = entry.get("by").and_then(toml::Value::as_str);
    let by_name = entry
        .get("by_name")
        .and_then(toml::Value::as_str)
        .unwrap_or_default();
    Entry::checked(id, version, author, offline, by, by_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by(device: &str, name: &str) -> TakenBy {
        TakenBy {
            device: device.to_owned(),
            name: name.to_owned(),
        }
    }

    fn off(version: u64, author: &str, by: Option<&str>, name: &str) -> Entry {
        Entry {
            version,
            author: author.to_owned(),
            offline: true,
            by: by.map(str::to_owned),
            by_name: name.to_owned(),
        }
    }

    fn back(version: u64, author: &str) -> Entry {
        Entry {
            version,
            author: author.to_owned(),
            offline: false,
            by: None,
            by_name: String::new(),
        }
    }

    fn doc(entries: &[(&str, Entry)]) -> OfflineDoc {
        OfflineDoc {
            devices: entries
                .iter()
                .map(|(id, e)| ((*id).to_owned(), e.clone()))
                .collect(),
        }
    }

    fn sample() -> OfflineDoc {
        doc(&[
            ("win", off(3, "mac", Some("mac"), "MacBook Pro")),
            ("mini", back(5, "win")),
            ("tv", off(4, "mini", None, "")),
        ])
    }

    #[test]
    fn saves_and_loads_the_devices_taken_offline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("offline.toml");
        assert_eq!(OfflineDoc::load(&path), OfflineDoc::default());
        let saved = sample();
        saved.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(HEADER), "{text}");
        assert!(
            text.contains(
                "[devices.win]\noffline = true\nversion = 3\nauthor = \"mac\"\nby = \"mac\"\nby_name = \"MacBook Pro\"\n"
            ),
            "{text}"
        );
        // Back online, and offline without anyone known to have done it:
        // what is not known is left out.
        assert!(
            text.contains("[devices.mini]\noffline = false\nversion = 5\nauthor = \"win\"\n"),
            "{text}"
        );
        assert!(
            text.contains("[devices.tv]\noffline = true\nversion = 4\n"),
            "{text}"
        );
        assert_eq!(OfflineDoc::load(&path), saved);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        OfflineDoc::default().save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("[devices"), "{text}");
        assert_eq!(OfflineDoc::load(&path), OfflineDoc::default());
    }

    #[test]
    fn whatever_another_device_sent_is_read_back_from_the_file() {
        // Every kind of entry that can be received, odd ones included,
        // goes through the same checks into the file and back.
        let received = peer::OfflineDevices {
            devices: vec![
                peer::OfflineDevice {
                    device_id: "a".into(),
                    offline: true,
                    version: 2,
                    author: "zed".into(),
                    by_device_id: String::new(),
                    by_name: String::new(),
                },
                // Without an author: no change, not taken in.
                peer::OfflineDevice {
                    device_id: "f".into(),
                    offline: true,
                    version: 2,
                    author: String::new(),
                    by_device_id: "mac".into(),
                    by_name: String::new(),
                },
                peer::OfflineDevice {
                    device_id: "b".into(),
                    offline: true,
                    version: 0,
                    author: "mac".into(),
                    by_device_id: "../x".into(),
                    by_name: "\u{7}".into(),
                },
                peer::OfflineDevice {
                    device_id: "c".into(),
                    offline: false,
                    version: MAX_VERSION - 1,
                    author: "win".into(),
                    by_device_id: "mac".into(),
                    by_name: "MacBook Pro".into(),
                },
                // At the limit: not taken in at all.
                peer::OfflineDevice {
                    device_id: "e".into(),
                    offline: true,
                    version: MAX_VERSION + 1,
                    author: "mac".into(),
                    by_device_id: "mac".into(),
                    by_name: String::new(),
                },
                peer::OfflineDevice {
                    device_id: "d".into(),
                    offline: true,
                    version: 9,
                    author: "mac".into(),
                    by_device_id: "mac".into(),
                    by_name: "MacBook Pro".into(),
                },
            ],
        };
        let doc = OfflineDoc::from_proto(received).unwrap();
        assert_eq!(doc.devices.len(), 4);
        assert!(!doc.devices.contains_key("f"), "no author: no change");
        // A device that took it offline that is not known keeps it offline.
        assert!(doc.contains("a") && doc.contains("b"));
        assert_eq!(doc.devices["b"].by, None);
        assert!(doc.devices["b"].by_name.is_empty());
        // Back online keeps no one.
        assert_eq!(doc.devices["c"], back(MAX_VERSION - 1, "win"));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offline.toml");
        doc.save(&path).unwrap();
        assert_eq!(OfflineDoc::load(&path), doc);
        // And back out the same.
        assert_eq!(OfflineDoc::from_proto(doc.to_proto()), Some(doc));
    }

    #[test]
    fn an_entry_that_cannot_be_read_is_dropped_alone() {
        let text = "later = \"kept out\"\n\n\
                    [devices.win]\noffline = true\nversion = 7\nauthor = \"mac\"\nby = \"mac\"\nby_name = \"MacBook Pro\"\nlater = 1\n\n\
                    [devices.noby]\nversion = 2\nauthor = \"mac\"\n\n\
                    [devices.badby]\nauthor = \"mac\"\nby = \"not one\"\nby_name = \"\"\n\n\
                    [devices.numberby]\nauthor = \"mac\"\nby = 5\n\n\
                    [devices.badauthor]\nauthor = \"not one\"\n\n\
                    [devices.noauthor]\nversion = 3\nby = \"mac\"\n\n\
                    [devices.badversion]\nauthor = \"mac\"\nversion = -1\n\n\
                    [devices.textversion]\nauthor = \"mac\"\nversion = \"three\"\n\n\
                    [devices.badoffline]\nauthor = \"mac\"\noffline = \"yes\"\n\n\
                    [devices.\"not an id\"]\nauthor = \"mac\"\nby = \"mac\"\n\n\
                    [devices.flat]\nauthor = \"mac\"\n";
        let doc = OfflineDoc::parse(text).unwrap();
        let ids: Vec<&str> = doc.devices.keys().map(String::as_str).collect();
        assert_eq!(ids, ["badby", "flat", "noby", "numberby", "win"]);
        assert_eq!(
            doc.devices["win"],
            off(7, "mac", Some("mac"), "MacBook Pro")
        );
        // What it does not say is offline, version 0, by no one known.
        for id in ["badby", "flat", "noby", "numberby"] {
            assert!(doc.contains(id), "{id}");
            assert_eq!(doc.devices[id].by, None, "{id}");
        }
        assert_eq!(doc.devices["noby"].version, 2);
        // Not a table: only it goes.
        let doc = OfflineDoc::parse("devices = 5\n").unwrap();
        assert!(doc.devices.is_empty());
    }

    #[test]
    fn a_file_that_cannot_be_read_at_all_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offline.toml");
        for broken in ["this is not toml [", "devices = [1, 2\n", "\u{0}\u{1}"] {
            std::fs::write(&path, broken).unwrap();
            assert_eq!(OfflineDoc::load(&path), OfflineDoc::default(), "{broken:?}");
        }
        std::fs::write(&path, [0xff, 0xfe, 0x00, 0x80]).unwrap();
        assert_eq!(OfflineDoc::load(&path), OfflineDoc::default());
        let folder = dir.path().join("folder.toml");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(OfflineDoc::load(&folder), OfflineDoc::default());
    }

    #[test]
    fn a_version_at_the_limit_is_not_taken_in_and_changes_here_keep_winning() {
        let mut ours = doc(&[("x", off(5, "mac", Some("mac"), "Mac"))]);
        // A device broken or meaning harm sends the PC back online at the
        // limit itself, and another entry just below it.
        let theirs = OfflineDoc::from_proto(peer::OfflineDevices {
            devices: vec![
                peer::OfflineDevice {
                    device_id: "x".into(),
                    offline: false,
                    version: MAX_VERSION,
                    author: "zed".into(),
                    ..Default::default()
                },
                peer::OfflineDevice {
                    device_id: "y".into(),
                    offline: true,
                    version: MAX_VERSION - 2,
                    author: "zed".into(),
                    ..Default::default()
                },
            ],
        })
        .unwrap();
        assert!(!theirs.devices.contains_key("x"), "at the limit: dropped");
        ours.merge(&theirs);
        assert!(ours.contains("x") && ours.contains("y"));
        // A change here still comes after everything: bringing x back.
        let next = ours
            .changed("x", false, by("mac", "Mac"), "mac")
            .unwrap()
            .unwrap();
        assert_eq!(next.devices["x"].version, MAX_VERSION - 1);
        let mut theirs_after = theirs.clone();
        theirs_after.merge(&next);
        assert!(!theirs_after.contains("x"));
        // Once none could come after every one, a change is refused rather
        // than made in place of an entry as new or newer.
        assert_eq!(
            next.changed("x", true, by("mac", "Mac"), "mac"),
            Err(Full::Exhausted)
        );
        // The file refuses the same.
        let text = format!("[devices.z]\nauthor = \"mac\"\nversion = {MAX_VERSION}\n");
        assert!(OfflineDoc::parse(&text).unwrap().devices.is_empty());
    }

    #[test]
    fn entries_order_by_version_then_author_then_what_they_say() {
        assert!(off(2, "a", None, "").newer_than(&back(1, "z")));
        assert!(back(2, "b").newer_than(&off(2, "a", None, "")));
        assert!(off(1, "a", None, "").newer_than(&back(0, "z")));
        // The same change, saying different things (as from a device that
        // is broken): offline, then by its setter, then by the name, comes
        // after; one the same as another is not newer.
        assert!(off(2, "a", None, "").newer_than(&back(2, "a")));
        assert!(off(2, "a", Some("b"), "").newer_than(&off(2, "a", None, "")));
        assert!(off(2, "a", Some("b"), "B").newer_than(&off(2, "a", Some("b"), "")));
        assert!(!off(2, "a", Some("b"), "B").newer_than(&off(2, "a", Some("b"), "B")));
    }

    #[test]
    fn entries_of_the_same_change_saying_different_things_converge() {
        let one = doc(&[("pc", off(4, "mac", Some("mac"), "Mac"))]);
        let other = doc(&[("pc", back(4, "mac"))]);
        let mut a = one.clone();
        let mut b = other.clone();
        // Each merges the other's: both settle on the same entry, and the
        // side that had the other one hears it is behind.
        assert_eq!(
            a.merge(&other),
            Merged {
                changed: false,
                behind: true
            }
        );
        assert_eq!(
            b.merge(&one),
            Merged {
                changed: true,
                behind: false
            }
        );
        assert_eq!(a, b);
        assert!(a.contains("pc"));
    }

    #[test]
    fn a_change_is_one_entry_newer_than_every_other() {
        let doc = sample();
        let next = doc
            .changed("pc", true, by("win", "Windows"), "win")
            .unwrap()
            .unwrap();
        assert_eq!(
            next.devices["pc"],
            off(6, "win", Some("win"), "Windows"),
            "above the highest version listed"
        );
        // Only that entry changes.
        for id in ["win", "mini", "tv"] {
            assert_eq!(next.devices[id], doc.devices[id], "{id}");
        }
        // Offline already, whoever took it so, or not listed offline:
        // nothing.
        assert_eq!(doc.changed("win", true, by("tv", "TV"), "tv"), Ok(None));
        assert_eq!(doc.changed("mini", false, by("tv", "TV"), "tv"), Ok(None));
        assert_eq!(doc.changed("other", false, by("tv", "TV"), "tv"), Ok(None));
        // Brought back: its entry stays, back online, by no one.
        let back_doc = doc
            .changed("win", false, by("mini", "Mini"), "mini")
            .unwrap()
            .unwrap();
        assert_eq!(back_doc.devices["win"], back(6, "mini"));
        assert!(!back_doc.contains("win"));
        // A device back online taken offline again keeps its one entry.
        let again = doc
            .changed("mini", true, by("mac", "Mac"), "mac")
            .unwrap()
            .unwrap();
        assert_eq!(again.devices.len(), 3);
        assert!(again.contains("mini"));
    }

    #[test]
    fn taking_offline_stops_at_the_most_devices_offline_allowed_here() {
        let mut doc = OfflineDoc::default();
        for n in 0..MAX_TAKEN {
            doc = doc
                .changed(&format!("d{n}"), true, by("mac", ""), "mac")
                .unwrap()
                .unwrap();
        }
        // As many offline as taking another allows: refused, nothing changed;
        // taking this device offline is not limited.
        assert_eq!(
            doc.changed("one-more", true, by("mac", ""), "mac"),
            Err(Full::TooMany)
        );
        let this_one = doc
            .changed("mac", true, by("mac", ""), "mac")
            .unwrap()
            .unwrap();
        assert!(this_one.contains("mac"));
        // Bringing one back still works, in place, and makes way for one.
        let doc = doc
            .changed("d7", false, by("mac", ""), "mac")
            .unwrap()
            .unwrap();
        assert_eq!(doc.devices.len(), MAX_TAKEN);
        let doc = doc
            .changed("one-more", true, by("mac", ""), "mac")
            .unwrap()
            .unwrap();
        assert_eq!(doc.offline().count(), MAX_TAKEN);
        assert!(
            doc.devices.contains_key("d7"),
            "nothing to make room for yet"
        );
    }

    /// A document of `n` devices back online, `b0` the oldest.
    fn tombstones(n: usize) -> OfflineDoc {
        OfflineDoc {
            devices: (0..n)
                .map(|i| (format!("b{i}"), back(i as u64 + 10, "mac")))
                .collect(),
        }
    }

    #[test]
    fn a_full_document_forgets_the_oldest_device_back_online_to_make_room() {
        let doc = tombstones(MAX_ENTRIES);
        let next = doc
            .changed("new", true, by("mac", ""), "mac")
            .unwrap()
            .unwrap();
        assert_eq!(next.devices.len(), MAX_ENTRIES);
        assert!(next.contains("new"));
        assert!(!next.devices.contains_key("b0") && next.devices.contains_key("b1"));
        // Full of devices offline (as two parts of a group merging can
        // leave it), with none back online to forget: refused.
        let full = OfflineDoc {
            devices: (0..MAX_ENTRIES)
                .map(|i| (format!("o{i}"), off(i as u64 + 1, "pc", None, "")))
                .collect(),
        };
        // Another device: as many offline as that allows already. This
        // device: no room, none back online to forget.
        assert_eq!(
            full.changed("new", true, by("mac", ""), "mac"),
            Err(Full::TooMany)
        );
        assert_eq!(
            full.changed("mac", true, by("mac", ""), "mac"),
            Err(Full::ListFull)
        );
        let back_one = full
            .changed("o3", false, by("mac", ""), "mac")
            .unwrap()
            .unwrap();
        assert_eq!(back_one.devices.len(), MAX_ENTRIES, "brought back in place");
    }

    #[test]
    fn changes_to_different_devices_at_once_both_stand() {
        // The Mac takes the TV offline while the PC takes the mini offline,
        // from the same document: merged either way, both are offline.
        let start = doc(&[("old", back(4, "mac"))]);
        let mac = start
            .changed("tv", true, by("mac", "Mac"), "mac")
            .unwrap()
            .unwrap();
        let pc = start
            .changed("mini", true, by("pc", "PC"), "pc")
            .unwrap()
            .unwrap();
        assert_eq!(mac.devices["tv"].version, pc.devices["mini"].version);
        let mut on_mac = mac.clone();
        let merged = on_mac.merge(&pc);
        assert_eq!(
            merged,
            Merged {
                changed: true,
                behind: true
            }
        );
        let mut on_pc = pc.clone();
        assert_eq!(
            on_pc.merge(&mac),
            Merged {
                changed: true,
                behind: true
            }
        );
        assert_eq!(on_mac, on_pc);
        assert!(on_mac.contains("tv") && on_mac.contains("mini"));
        // Merging again changes nothing, and neither is behind.
        assert_eq!(on_mac.clone().merge(&on_pc), Merged::default());
    }

    #[test]
    fn a_device_back_with_an_older_change_loses_only_that_one() {
        // Apart from the group, the laptop still has the PC offline; the
        // group brought the PC back since, and took the hub offline.
        let apart = doc(&[
            ("pc", off(3, "mac", Some("mac"), "Mac")),
            ("tv", off(2, "laptop", Some("laptop"), "Laptop")),
        ]);
        let mut group = doc(&[
            ("pc", back(5, "mini")),
            ("hub", off(6, "mac", Some("mac"), "Mac")),
        ]);
        let merged = group.merge(&apart);
        // Its change of the PC loses; the one only it made stands.
        assert_eq!(
            merged,
            Merged {
                changed: true,
                behind: true
            }
        );
        assert!(!group.contains("pc") && group.contains("hub") && group.contains("tv"));
        let mut laptop = apart.clone();
        laptop.merge(&group);
        assert_eq!(laptop, group);
    }

    #[test]
    fn bringing_back_after_seeing_it_wins_over_the_older_take_offline() {
        // The Mac takes the PC offline; the mini, a third device, sees that
        // and brings it back: whoever sends what in which order, it is back.
        let taken = OfflineDoc::default()
            .changed("pc", true, by("mac", "Mac"), "mac")
            .unwrap()
            .unwrap();
        let brought = taken
            .changed("pc", false, by("mini", "Mini"), "mini")
            .unwrap()
            .unwrap();
        let mut on_mac = taken.clone();
        on_mac.merge(&brought);
        assert!(!on_mac.contains("pc"));
        let mut on_mini = brought.clone();
        assert_eq!(
            on_mini.merge(&taken),
            Merged {
                changed: false,
                behind: true
            }
        );
        assert!(!on_mini.contains("pc"));
        // Even a take-offline by a later author, made before seeing it.
        let late_author = OfflineDoc::default()
            .changed("pc", true, by("zed", "Zed"), "zed")
            .unwrap()
            .unwrap();
        on_mini.merge(&late_author);
        assert!(
            !on_mini.contains("pc"),
            "version 1 by zed is older than version 2"
        );
    }

    #[test]
    fn merging_past_the_limit_forgets_the_oldest_devices_back_online_first() {
        let mut ours = tombstones(MAX_ENTRIES);
        let theirs = doc(&[("x", off(5000, "pc", Some("pc"), "PC"))]);
        assert_eq!(
            ours.merge(&theirs),
            Merged {
                changed: true,
                behind: true
            }
        );
        assert_eq!(ours.devices.len(), MAX_ENTRIES);
        assert!(ours.contains("x") && !ours.devices.contains_key("b0"));
    }

    #[test]
    fn a_tombstone_the_merge_compared_is_kept_and_goes_back() {
        // The oldest tombstone here says D is back online at version 10; the
        // other device still has D offline at version 5, and a new entry
        // that needs room. D's tombstone stays (another goes), D stays back
        // online, and the other device is behind: it gets the tombstone.
        let mut ours = tombstones(MAX_ENTRIES - 1);
        ours.devices.insert("d".into(), back(9, "mac"));
        ours.devices.get_mut("d").unwrap().version = 10;
        ours.devices.get_mut("b0").unwrap().version = 12;
        let theirs = doc(&[
            ("d", off(5, "pc", Some("pc"), "PC")),
            ("x", off(5000, "pc", Some("pc"), "PC")),
        ]);
        let merged = ours.merge(&theirs);
        assert_eq!(
            merged,
            Merged {
                changed: true,
                behind: true
            }
        );
        assert_eq!(ours.devices.len(), MAX_ENTRIES);
        assert_eq!(ours.devices["d"], back(10, "mac"));
        assert!(ours.contains("x"));
        assert!(
            !ours.devices.contains_key("b1"),
            "the oldest not compared went"
        );
        // The other device takes what comes back: D is back online there.
        let mut on_theirs = theirs.clone();
        on_theirs.merge(&ours);
        assert!(!on_theirs.contains("d"));
        // An entry taken in and forgotten again in the same merge is no
        // change: an older one than every entry here, with no room.
        let mut full = OfflineDoc {
            devices: (0..MAX_ENTRIES)
                .map(|i| (format!("o{i}"), off(i as u64 + 100, "pc", None, "")))
                .collect(),
        };
        let stale = doc(&[("old", off(1, "a", None, ""))]);
        let kept = full.clone();
        assert_eq!(
            full.merge(&stale),
            Merged {
                changed: false,
                behind: true
            }
        );
        assert_eq!(full, kept);
    }

    #[test]
    fn two_full_parts_of_the_group_merge_within_the_limit() {
        // Two parts apart from each other, each with as many devices offline
        // as a document holds: the merge keeps the newest of them, within
        // the limit, so every device takes what it sends.
        let part = |prefix: &str, author: &str| OfflineDoc {
            devices: (0..MAX_ENTRIES)
                .map(|i| (format!("{prefix}{i}"), off(i as u64 + 1, author, None, "")))
                .collect(),
        };
        let mut ours = part("a", "mac");
        let theirs = part("b", "pc");
        let merged = ours.merge(&theirs);
        assert_eq!(
            merged,
            Merged {
                changed: true,
                behind: true
            }
        );
        assert_eq!(ours.devices.len(), MAX_ENTRIES);
        // The oldest went first, by version then author: both halves keep
        // their newer halves.
        assert!(!ours.devices.contains_key("a0") && !ours.devices.contains_key("b0"));
        assert!(ours.contains(&format!("a{}", MAX_ENTRIES - 1)));
        assert!(ours.contains(&format!("b{}", MAX_ENTRIES - 1)));
        let sent = ours.to_proto();
        assert!(sent.devices.len() <= MAX_ENTRIES);
        let mut on_theirs = OfflineDoc::from_proto(sent).expect("taken whole");
        on_theirs.merge(&theirs);
        assert_eq!(on_theirs, ours);
        // And what it keeps reads back from its file within the limit.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offline.toml");
        ours.save(&path).unwrap();
        assert_eq!(OfflineDoc::load(&path), ours);
    }

    #[test]
    fn a_full_document_s_file_stays_well_within_what_the_login_window_reads() {
        // Every field as long as it can be: ids of 64 characters, names of
        // 64 characters of 4 bytes each, the highest version.
        let id = |n: usize| format!("{n:0>64}");
        let name = "\u{1D538}".repeat(64);
        let doc = OfflineDoc {
            devices: (0..MAX_ENTRIES)
                .map(|n| {
                    (
                        id(n),
                        Entry {
                            version: MAX_VERSION - 1,
                            author: id(n + 1),
                            offline: true,
                            by: Some(id(n + 2)),
                            by_name: name.clone(),
                        },
                    )
                })
                .collect(),
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offline.toml");
        doc.save(&path).unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size < MAX_FILE / 4, "{size} bytes");
        assert_eq!(OfflineDoc::load(&path), doc);
    }

    #[test]
    fn a_document_from_another_device_is_checked() {
        let proto = sample().to_proto();
        assert_eq!(OfflineDoc::from_proto(proto.clone()), Some(sample()));
        // An entry that cannot be one goes alone.
        let mut odd = proto.clone();
        odd.devices[0].device_id = "not an id".into();
        odd.devices[1].author = "../x".into();
        let doc = OfflineDoc::from_proto(odd).unwrap();
        assert_eq!(doc.devices.len(), 1);
        // Too many: none of it.
        let mut many = proto.clone();
        many.devices = (0..=MAX_ENTRIES)
            .map(|n| peer::OfflineDevice {
                device_id: format!("d{n}"),
                offline: true,
                ..Default::default()
            })
            .collect();
        assert_eq!(OfflineDoc::from_proto(many), None);
        // A device listed twice is taken once, as it came first.
        let mut twice = proto.clone();
        let mut again = twice.devices[0].clone();
        again.version += 10;
        twice.devices.push(again);
        let doc = OfflineDoc::from_proto(twice).unwrap();
        assert_eq!(doc, sample());
    }

    #[test]
    fn who_set_it_is_said_whatever_is_known() {
        assert_eq!(
            describe_setter("mac", "mac", "MacBook Pro"),
            "set on this device"
        );
        assert_eq!(describe_setter("mac", "win", "PC"), "set by PC (win)");
        assert_eq!(describe_setter("mac", "", "PC"), "set by PC");
        assert_eq!(describe_setter("mac", "win", ""), "set by win");
        assert_eq!(describe_setter("mac", "", ""), "set by another computer");
        // An empty id is not this device, even with no id of its own.
        assert_eq!(describe_setter("", "", ""), "set by another computer");
    }

    #[test]
    fn the_test_of_whether_anything_goes_between_two_devices() {
        let doc = sample();
        assert!(doc.between("mac", "win"), "the other is offline");
        assert!(doc.between("win", "mac"), "this one is");
        assert!(!doc.between("mac", "mini"), "back online");
        assert!(!doc.between("mac", "unknown"));
        assert_eq!(
            doc.offline().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ["tv", "win"]
        );
    }
}
