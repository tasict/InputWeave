//! Profiles (docs/architecture.md §5.7): named sets of settings this device
//! picks by the network it is on, or that the user picks; and the file they
//! are kept in (`profiles.toml`, beside the devices file). The agent writes
//! it. It is a file of its own, so the devices file stays one that agents
//! from before read, as `wake.toml` and `offline.toml` are.
//!
//! Each profile says whether this device shares at all (paused: it neither
//! announces itself nor connects to any device), which trusted devices it
//! does not connect (so a device paired later is connected in every profile
//! until the user says otherwise), whether the clipboard and files go, and
//! the networks it is used on. Two are always there, first and last, and
//! cannot be renamed or removed: Default ([`DEFAULT`]), and Other networks
//! ([`OTHER`]), used on every network no profile lists, which lists none.
//! A network is listed in one profile at most.
//!
//! A network is listed by an id that stands for it ([`network_id`]): a
//! hash of its router's address and hardware address, never the hardware
//! address itself, and what to call it: its router's address, its Wi-Fi
//! name, the name Windows gives it, or a name given here. The hash takes a
//! salt this install made at random (`salt`, made as the file is first
//! written), so an id cannot be traced back to its router by trying the
//! hardware addresses one maker's routers have. Ids are only ever used
//! here, so a file without it, or with one that is not one, is given a new
//! salt, and the networks it lists are recognized no more.
//!
//! Read leniently: a profile or network that cannot be read, as edited by
//! hand, is dropped with a warning and the rest kept; a switch that is not
//! one is its default; the two fixed profiles come back if they are
//! missing. A file that cannot be read at all is passed over with a
//! warning, and the agent starts without it.
//!
//! What this device last picked by itself by a network's Wi-Fi name, on
//! networks no profile lists by their id (`picks`), is kept too, for the
//! most recent [`MAX_PICKS`] networks: on such a network, until the Wi-Fi
//! name is known again (an app reads it, and the agent confirms it), the
//! same profile is used, as the agent starts and at the login window,
//! where no app tells it.
//!
//! ```toml
//! salt = "c2a7…"
//! wifi_names = true
//! chosen = "p1"
//!
//! [[picks]]
//! network = "77d0…"
//! profile = "p1"
//!
//! [[profiles]]
//! id = "default"
//!
//! [[profiles.networks]]
//! id = "5f0c…"
//! kind = "wired"
//! router = "192.168.1.1"
//!
//! [[profiles]]
//! id = "p1"
//! name = "Office"
//! files = false
//! off = ["mini"]
//!
//! [[profiles.networks]]
//! id = "9a41…"
//! kind = "wifi"
//! router = "198.51.100.1"
//! wifi_name = "Office-5G"
//!
//! [[profiles]]
//! id = "other"
//! paused = true
//! ```

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::path::Path;

use serde::Serialize;
use tracing::warn;

const HEADER: &str = "# Profiles, written by the InputWeave agent.\n\
                      # Edit only while the agent is stopped.\n\n";

/// The profile used on networks no other profile lists, at first the only
/// one there is.
pub const DEFAULT: &str = "default";
/// The profile used on every network no other profile lists.
pub const OTHER: &str = "other";

/// More profiles than this are refused.
pub const MAX_PROFILES: usize = 32;
/// More networks than this, in all the profiles, are refused.
pub const MAX_NETWORKS: usize = 256;
/// The picks kept (`ProfilesFile::picks`): those of the most recent
/// networks.
pub const MAX_PICKS: usize = 64;
/// A name for a profile or a network: as many characters at most.
const MAX_NAME_CHARS: usize = 64;

/// The kind of a network.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NetworkKind {
    #[default]
    Wired,
    Wifi,
    /// Reached over a link without a hardware address: a VPN.
    Vpn,
}

impl NetworkKind {
    fn key(self) -> &'static str {
        match self {
            Self::Wired => "wired",
            Self::Wifi => "wifi",
            Self::Vpn => "vpn",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        match key {
            "wired" => Some(Self::Wired),
            "wifi" => Some(Self::Wifi),
            "vpn" => Some(Self::Vpn),
            _ => None,
        }
    }
}

/// A network: one listed in a profile, or the one this device is on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Network {
    /// Stands for it ([`network_id`]).
    pub id: String,
    pub kind: NetworkKind,
    /// Its router's address; none for a VPN whose route has none.
    pub router: Option<Ipv4Addr>,
    /// Its Wi-Fi name, where known.
    pub wifi_name: Option<String>,
    /// What Windows calls it, where known.
    pub network_name: Option<String>,
    /// A name given it here, shown in place of the others.
    pub name: Option<String>,
}

impl Network {
    /// What to call it: the name given here, its Wi-Fi name, the name
    /// Windows gives it; none for the apps to call it by its router.
    pub fn shown_name(&self) -> Option<&str> {
        self.name
            .as_deref()
            .or(self.wifi_name.as_deref())
            .or(self.network_name.as_deref())
    }
}

/// A profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub id: String,
    /// Empty for the two fixed ones, which the apps name.
    pub name: String,
    /// This device neither announces itself nor connects to any device.
    pub paused: bool,
    /// The trusted devices it does not connect.
    pub off: BTreeSet<String>,
    pub clipboard: bool,
    pub files: bool,
    /// None for Other networks.
    pub networks: Vec<Network>,
}

impl Profile {
    fn new(id: &str, name: &str) -> Self {
        Self {
            id: id.to_owned(),
            name: name.to_owned(),
            paused: false,
            off: BTreeSet::new(),
            clipboard: true,
            files: true,
            networks: Vec::new(),
        }
    }

    /// Whether it is one of the two that are always there.
    pub fn fixed(&self) -> bool {
        self.id == DEFAULT || self.id == OTHER
    }

    /// Whether it connects `device`: it shares, and leaves it on.
    pub fn connects(&self, device: &str) -> bool {
        connects(self.paused, &self.off, device)
    }
}

/// Whether a profile, `paused` or not and leaving out `off`, connects
/// `device` ([`Profile::connects`]).
pub(crate) fn connects(paused: bool, off: &BTreeSet<String>, device: &str) -> bool {
    !paused && !off.contains(device)
}

/// Why a change to the profiles was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProfileError {
    #[error("no profile has id {0:?}")]
    UnknownProfile(String),
    #[error("Default and Other networks cannot be renamed or removed")]
    FixedProfile,
    #[error("a name is 1 to 64 characters without control characters")]
    InvalidName,
    #[error("another profile already has this name")]
    NameTaken,
    #[error("there are {MAX_PROFILES} profiles already")]
    TooManyProfiles,
    #[error("Other networks lists no networks: it is used on every network no other profile lists")]
    OtherNetworks,
    #[error("this device is on no network now")]
    NoNetwork,
    #[error("the network is known by its router's address alone for now")]
    NetworkUnresolved,
    #[error("{MAX_NETWORKS} networks are listed already")]
    TooManyNetworks,
    #[error("no profile lists a network with id {0:?}")]
    UnknownNetwork(String),
    #[error("{0:?} is not a trusted device, nor listed in the profile")]
    UnknownDevice(String),
    #[error("the agent is stopping")]
    Stopping,
}

impl ProfileError {
    /// The error code clients are told.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownProfile(_) => "unknown_profile",
            Self::FixedProfile => "fixed_profile",
            Self::InvalidName => "invalid_name",
            Self::NameTaken => "name_taken",
            Self::TooManyProfiles => "too_many_profiles",
            Self::OtherNetworks => "other_networks",
            Self::NoNetwork => "no_network",
            Self::NetworkUnresolved => "network_unresolved",
            Self::TooManyNetworks => "too_many_networks",
            Self::UnknownNetwork(_) => "unknown_network",
            Self::UnknownDevice(_) => "unknown_device",
            Self::Stopping => "agent_stopping",
        }
    }
}

/// What to change of a profile (`EditProfile`): the fields that are some.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileEdit {
    pub name: Option<String>,
    pub paused: Option<bool>,
    pub clipboard: Option<bool>,
    pub files: Option<bool>,
    /// Devices to connect (true) or not.
    pub devices: Vec<(String, bool)>,
}

/// The profiles, and how one is picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfilesFile {
    /// Default first, Other networks last, the user's own between them in
    /// the order they were added.
    pub profiles: Vec<Profile>,
    /// The profile the user chose, used whatever the network; none:
    /// Automatic.
    pub chosen: Option<String>,
    /// Networks are also recognized by their Wi-Fi name, as the app reads
    /// it (macOS, Windows).
    pub wifi_names: bool,
    /// What the ids of networks are made with ([`network_id`]): 32 lower
    /// case hexadecimal digits, made at random for each install.
    pub salt: String,
    /// The profile this device last picked by itself by the Wi-Fi name on
    /// each network no profile lists by its id: one for each network, the
    /// most recently changed last, [`MAX_PICKS`] at most.
    pub picks: Vec<LastPick>,
}

/// A profile this device picked by itself, and the network it was on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastPick {
    /// The network's id.
    pub network: String,
    pub profile: String,
}

impl Default for ProfilesFile {
    /// The two fixed profiles, with a new salt.
    fn default() -> Self {
        Self {
            profiles: vec![Profile::new(DEFAULT, ""), Profile::new(OTHER, "")],
            chosen: None,
            wifi_names: false,
            salt: new_salt(),
            picks: Vec::new(),
        }
    }
}

/// What reading the profiles file found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Loaded {
    /// No file: a first start, or an upgrade from before profiles.
    Missing,
    File(ProfilesFile),
    /// A file that cannot be read, or is not one. It may be the user's
    /// profiles all the same (a disk error, a hand edit gone wrong), so it
    /// is not written over until the user changes the profiles.
    Unreadable,
}

/// The names the apps give the two fixed profiles, in English and Chinese:
/// none of the user's own may have one.
const FIXED_NAMES: [&str; 4] = ["Default", "預設", "Other networks", "其他網路"];

/// Whether two names of profiles are one: trimmed, whatever the case.
fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Whether `name` is one the apps give a fixed profile.
fn fixed_name(name: &str) -> bool {
    FIXED_NAMES.iter().any(|fixed| same_name(fixed, name))
}

/// Whether `name` may name a profile or a network: 1 to 64 characters,
/// none a control character, not only spaces.
pub fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && !name.chars().any(char::is_control)
}

/// Whether `id` may name a profile: 1 to 32 lower case letters, digits and
/// dashes.
fn valid_profile_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Whether `id` may stand for a network, or salt the ids: 32 lower case
/// hexadecimal digits.
fn valid_network_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A salt for the ids of networks: 16 random bytes, in hexadecimal. Should
/// the system give none, which it does not, one made of the time and this
/// process, which is still this install's own.
fn new_salt() -> String {
    let mut bytes = [0u8; 16];
    if let Err(e) = getrandom::fill(&mut bytes) {
        use sha2::{Digest, Sha256};
        warn!(error = %e, "no random bytes for the networks' salt; making one of the time");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let seed = format!("{} {} {:p}", now.as_nanos(), std::process::id(), &bytes);
        bytes.copy_from_slice(&Sha256::digest(seed.as_bytes())[..16]);
    }
    hex(&bytes)
}

/// What stands for a network: the first 16 bytes of a SHA-256, in
/// hexadecimal, of this install's `salt` and what tells the network apart
/// (`what`), so a router's hardware address is never kept, or shown, as it
/// is, nor found again from the id by trying addresses.
pub fn network_id(salt: &str, what: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("inputweave network 2\n{salt}\n{what}").as_bytes());
    hex(&digest[..16])
}

/// `bytes` in lower case hexadecimal.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

impl ProfilesFile {
    /// Reads the file: what it holds, or that there is none, or that it is
    /// there but cannot be read (or is not TOML), with a warning, so the
    /// agent starts all the same; it then leaves that file be (see
    /// [`Loaded::Unreadable`]).
    pub fn load(path: &Path) -> Loaded {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::Missing,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "cannot read the profiles file; using the default profiles and leaving the file as it is");
                return Loaded::Unreadable;
            }
        };
        match Self::parse(&text) {
            Ok(file) => Loaded::File(file),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "the profiles file is not one; using the default profiles and leaving the file as it is");
                Loaded::Unreadable
            }
        }
    }

    /// The file as `text` holds it, read leniently (see the module docs).
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        let value: toml::Table = toml::from_str(text)?;
        let mut file = Self {
            profiles: Vec::new(),
            chosen: None,
            wifi_names: false,
            salt: String::new(),
            picks: Vec::new(),
        };
        let salt = value.get("salt");
        match salt
            .and_then(toml::Value::as_str)
            .filter(|s| valid_network_id(s))
        {
            Some(salt) => file.salt = salt.to_owned(),
            None => {
                if salt.is_some() {
                    warn!(
                        "the networks' salt is not one; making a new one, so the networks listed are recognized no more"
                    );
                }
                file.salt = new_salt();
            }
        }
        if let Some(on) = value.get("wifi_names") {
            match on.as_bool() {
                Some(on) => file.wifi_names = on,
                None => warn!(value = %on, "taking a wifi_names that is not true or false as off"),
            }
        }
        if let Some(picks) = value.get("picks") {
            for entry in picks.as_array().map(Vec::as_slice).unwrap_or_default() {
                let field = |key: &str| entry.get(key).and_then(toml::Value::as_str);
                match (field("network"), field("profile")) {
                    (Some(network), Some(profile))
                        if valid_network_id(network) && valid_profile_id(profile) =>
                    {
                        file.picks.push(LastPick {
                            network: network.to_owned(),
                            profile: profile.to_owned(),
                        });
                    }
                    _ => warn!("dropping a profile picked that is not one"),
                }
            }
            if !picks.is_array() {
                warn!("dropping the profiles picked: not a list");
            }
        }
        if let Some(chosen) = value.get("chosen") {
            match chosen.as_str().filter(|id| valid_profile_id(id)) {
                Some(id) => file.chosen = Some(id.to_owned()),
                None => warn!(value = %chosen, "taking a chosen profile that is none as Automatic"),
            }
        }
        if let Some(profiles) = value.get("profiles") {
            match profiles.as_array() {
                Some(list) => {
                    for entry in list {
                        match profile_from(entry) {
                            Some(profile) => file.profiles.push(profile),
                            None => warn!(value = %entry, "dropping a profile that is not one"),
                        }
                    }
                }
                None => warn!("dropping the profiles: not a list"),
            }
        }
        file.fit();
        Ok(file)
    }

    /// Replaces the file in one step, readable only by its owner.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string(&Written::of(self)).map_err(std::io::Error::other)?;
        crate::store::write_private(path, HEADER, &text)
    }

    /// Puts the profiles in order: no two with one id, Default first and
    /// Other networks last (made anew when missing), no networks in Other
    /// networks, no network in two profiles, a name for each of the user's
    /// own and none for the two fixed ones, no more than the limits, and a
    /// chosen profile that is there.
    fn fit(&mut self) {
        // The profiles that stay first, so those dropped claim no network.
        let mut seen_ids = BTreeSet::new();
        let mut default = None;
        let mut other = None;
        let mut own = Vec::new();
        for mut profile in std::mem::take(&mut self.profiles) {
            if !seen_ids.insert(profile.id.clone()) {
                warn!(id = %profile.id, "dropping a second profile with the same id");
                continue;
            }
            match profile.id.as_str() {
                DEFAULT => {
                    profile.name.clear();
                    default = Some(profile);
                }
                OTHER => {
                    profile.name.clear();
                    if !profile.networks.is_empty() {
                        warn!("dropping the networks listed in Other networks");
                        profile.networks.clear();
                    }
                    other = Some(profile);
                }
                _ if !valid_name(&profile.name) => {
                    warn!(id = %profile.id, "dropping a profile without a name");
                }
                _ => own.push(profile),
            }
        }
        if own.len() > MAX_PROFILES - 2 {
            warn!("dropping the profiles past the most there may be");
            own.truncate(MAX_PROFILES - 2);
        }
        self.profiles = std::iter::once(default.unwrap_or_else(|| Profile::new(DEFAULT, "")))
            .chain(own)
            .chain(std::iter::once(
                other.unwrap_or_else(|| Profile::new(OTHER, "")),
            ))
            .collect();
        // Then their networks, in that order.
        let mut seen_networks = BTreeSet::new();
        let mut networks = 0;
        for profile in &mut self.profiles {
            profile.networks.retain(|n| {
                let keep = networks < MAX_NETWORKS && seen_networks.insert(n.id.clone());
                if keep {
                    networks += 1;
                } else {
                    warn!(id = %n.id, "dropping a network listed twice, or one too many");
                }
                keep
            });
        }
        if self
            .chosen
            .as_ref()
            .is_some_and(|id| self.profile(id).is_none())
        {
            warn!("the chosen profile is not there; back to Automatic");
            self.chosen = None;
        }
        // One pick for each network, the latest; none for a network listed
        // by its id, which is used by that.
        let mut picked = BTreeSet::new();
        let mut picks: Vec<LastPick> = std::mem::take(&mut self.picks)
            .into_iter()
            .rev()
            .filter(|p| self.network(&p.network).is_none() && picked.insert(p.network.clone()))
            .take(MAX_PICKS)
            .collect();
        picks.reverse();
        self.picks = picks;
    }

    pub fn profile(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    fn profile_mut(&mut self, id: &str) -> Result<&mut Profile, ProfileError> {
        self.profiles
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| ProfileError::UnknownProfile(id.to_owned()))
    }

    /// The profile listing the network `id`.
    pub fn listing(&self, id: &str) -> Option<&Profile> {
        self.profiles
            .iter()
            .find(|p| p.networks.iter().any(|n| n.id == id))
    }

    /// The listed network `id`.
    pub fn network(&self, id: &str) -> Option<&Network> {
        self.profiles
            .iter()
            .flat_map(|p| &p.networks)
            .find(|n| n.id == id)
    }

    fn network_count(&self) -> usize {
        self.profiles.iter().map(|p| p.networks.len()).sum()
    }

    /// Adds a profile named `name`, or with " 2", " 3"… after it when one
    /// has that name, before Other networks; returns its id.
    pub fn add(&mut self, name: &str) -> Result<String, ProfileError> {
        let name = name.trim();
        if !valid_name(name) {
            return Err(ProfileError::InvalidName);
        }
        if fixed_name(name) {
            return Err(ProfileError::NameTaken);
        }
        if self.profiles.len() >= MAX_PROFILES {
            return Err(ProfileError::TooManyProfiles);
        }
        let taken = |candidate: &str| self.profiles.iter().any(|p| same_name(&p.name, candidate));
        let mut unique = name.to_owned();
        let mut n = 2;
        while taken(&unique) {
            let suffix = format!(" {n}");
            // Kept within the longest name, the number whole.
            let room = MAX_NAME_CHARS - suffix.chars().count();
            unique = name.chars().take(room).collect::<String>() + &suffix;
            n += 1;
        }
        // After the highest number in use, so a removed one is not given
        // again while a later one is there; past the highest there is, the
        // lowest not in use.
        let used: BTreeSet<u32> = self
            .profiles
            .iter()
            .filter_map(|p| p.id.strip_prefix('p')?.parse::<u32>().ok())
            .collect();
        let next = match used.last() {
            None => 1,
            Some(&highest) => highest
                .checked_add(1)
                .unwrap_or_else(|| (1..).find(|n| !used.contains(n)).unwrap_or(1)),
        };
        let id = format!("p{next}");
        let at = self.profiles.len() - 1;
        self.profiles.insert(at, Profile::new(&id, &unique));
        Ok(id)
    }

    /// Changes what `edit` says of profile `id`; `trusted` says which
    /// devices may be connected or not. Whether anything changed.
    pub fn edit(
        &mut self,
        id: &str,
        edit: &ProfileEdit,
        trusted: impl Fn(&str) -> bool,
    ) -> Result<bool, ProfileError> {
        let profile = self
            .profiles
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| ProfileError::UnknownProfile(id.to_owned()))?;
        if let Some(name) = &edit.name {
            if profile.fixed() {
                return Err(ProfileError::FixedProfile);
            }
            if !valid_name(name.trim()) {
                return Err(ProfileError::InvalidName);
            }
            let taken = self
                .profiles
                .iter()
                .any(|p| p.id != id && same_name(&p.name, name));
            if taken || fixed_name(name) {
                return Err(ProfileError::NameTaken);
            }
        }
        if let Some((device, _)) = edit
            .devices
            .iter()
            .find(|(device, _)| !trusted(device) && !profile.off.contains(device))
        {
            return Err(ProfileError::UnknownDevice(device.clone()));
        }
        let before = profile.clone();
        let profile = self.profile_mut(id)?;
        if let Some(name) = &edit.name {
            profile.name = name.trim().to_owned();
        }
        if let Some(paused) = edit.paused {
            profile.paused = paused;
        }
        if let Some(clipboard) = edit.clipboard {
            profile.clipboard = clipboard;
        }
        if let Some(files) = edit.files {
            profile.files = files;
        }
        for (device, connect) in &edit.devices {
            if *connect {
                profile.off.remove(device);
            } else {
                profile.off.insert(device.clone());
            }
        }
        Ok(*profile != before)
    }

    /// Removes one of the user's own profiles, and the choice of it.
    pub fn remove(&mut self, id: &str) -> Result<(), ProfileError> {
        let profile = self
            .profile(id)
            .ok_or_else(|| ProfileError::UnknownProfile(id.to_owned()))?;
        if profile.fixed() {
            return Err(ProfileError::FixedProfile);
        }
        self.profiles.retain(|p| p.id != id);
        if self.chosen.as_deref() == Some(id) {
            self.chosen = None;
        }
        self.picks.retain(|p| p.profile != id);
        Ok(())
    }

    /// Uses profile `id` whatever the network, or with none goes back to
    /// Automatic. Whether anything changed.
    pub fn choose(&mut self, id: Option<&str>) -> Result<bool, ProfileError> {
        if let Some(id) = id
            && self.profile(id).is_none()
        {
            return Err(ProfileError::UnknownProfile(id.to_owned()));
        }
        let before = self.chosen.clone();
        self.chosen = id.map(str::to_owned);
        Ok(before != self.chosen)
    }

    /// Notes that this device picked `profile` by itself by the Wi-Fi name
    /// on the network `network`, which no profile lists by its id: kept as
    /// the latest, the oldest let go past [`MAX_PICKS`]. Whether that
    /// changed.
    pub fn picked(&mut self, network: &str, profile: &str) -> bool {
        if self
            .picks
            .iter()
            .any(|p| p.network == network && p.profile == profile)
        {
            return false;
        }
        self.picks.retain(|p| p.network != network);
        self.picks.push(LastPick {
            network: network.to_owned(),
            profile: profile.to_owned(),
        });
        if self.picks.len() > MAX_PICKS {
            self.picks.remove(0);
        }
        true
    }

    /// The profile last picked by itself on the network `network`
    /// ([`picked`](Self::picked)), while it is there.
    pub fn last_pick(&self, network: &str) -> Option<&Profile> {
        let pick = self.picks.iter().find(|p| p.network == network)?;
        self.profile(&pick.profile)
    }

    /// Lists `network` in profile `id`, taking it from any other that
    /// lists it, with what is known of it now: the names it was kept with
    /// go with it (the one given here, and its Wi-Fi name and the name
    /// Windows gives it where `network` has none of its own). Whether
    /// anything changed.
    pub fn add_network(&mut self, id: &str, network: &Network) -> Result<bool, ProfileError> {
        let profile = self
            .profile(id)
            .ok_or_else(|| ProfileError::UnknownProfile(id.to_owned()))?;
        if profile.id == OTHER {
            return Err(ProfileError::OtherNetworks);
        }
        if profile.networks.iter().any(|n| n.id == network.id) {
            return Ok(false);
        }
        let before = self.network(&network.id).cloned();
        if before.is_none() && self.network_count() >= MAX_NETWORKS {
            return Err(ProfileError::TooManyNetworks);
        }
        for p in &mut self.profiles {
            p.networks.retain(|n| n.id != network.id);
        }
        // Listed by its id, it is used by that from now on.
        self.picks.retain(|p| p.network != network.id);
        let mut listed = network.clone();
        if let Some(before) = before {
            listed.name = before.name;
            listed.wifi_name = listed.wifi_name.or(before.wifi_name);
            listed.network_name = listed.network_name.or(before.network_name);
        } else {
            listed.name = None;
        }
        self.profile_mut(id)?.networks.push(listed);
        Ok(true)
    }

    /// Takes network `id` out of the profile listing it.
    pub fn remove_network(&mut self, id: &str) -> Result<(), ProfileError> {
        if self.network(id).is_none() {
            return Err(ProfileError::UnknownNetwork(id.to_owned()));
        }
        for p in &mut self.profiles {
            p.networks.retain(|n| n.id != id);
        }
        Ok(())
    }

    /// Gives listed network `id` a name, or with an empty one takes it
    /// away. Whether anything changed.
    pub fn rename_network(&mut self, id: &str, name: &str) -> Result<bool, ProfileError> {
        let name = name.trim();
        if !name.is_empty() && !valid_name(name) {
            return Err(ProfileError::InvalidName);
        }
        let network = self
            .profiles
            .iter_mut()
            .flat_map(|p| &mut p.networks)
            .find(|n| n.id == id)
            .ok_or_else(|| ProfileError::UnknownNetwork(id.to_owned()))?;
        let name = (!name.is_empty()).then(|| name.to_owned());
        let changed = network.name != name;
        network.name = name;
        Ok(changed)
    }

    /// Takes what the system says of `seen` now into the network listed
    /// under its id: how it is reached (wired and Wi-Fi to one router are
    /// one network), and its Wi-Fi name and the name Windows gives it,
    /// where those are known now and differ (a name given here, `name`,
    /// stays). Whether anything changed.
    pub fn learn(&mut self, seen: &Network) -> bool {
        let Some(listed) = self
            .profiles
            .iter_mut()
            .flat_map(|p| &mut p.networks)
            .find(|n| n.id == seen.id)
        else {
            return false;
        };
        let mut changed = false;
        if listed.kind != seen.kind {
            listed.kind = seen.kind;
            changed = true;
        }
        if seen.wifi_name.is_some() && listed.wifi_name != seen.wifi_name {
            listed.wifi_name = seen.wifi_name.clone();
            changed = true;
        }
        if seen.network_name.is_some() && listed.network_name != seen.network_name {
            listed.network_name = seen.network_name.clone();
            changed = true;
        }
        changed
    }

    /// Gives the listed network `id` the Wi-Fi name `name` an app read, if
    /// it has none: an app's reading never replaces one. Whether anything
    /// changed.
    pub fn fill_wifi_name(&mut self, id: &str, name: &str) -> bool {
        let Some(listed) = self
            .profiles
            .iter_mut()
            .flat_map(|p| &mut p.networks)
            .find(|n| n.id == id && n.wifi_name.is_none())
        else {
            return false;
        };
        listed.wifi_name = Some(name.to_owned());
        true
    }

    /// Forgets the devices for which `gone` says so in every profile's
    /// list of devices not connected. Whether anything changed.
    pub fn forget_devices(&mut self, gone: impl Fn(&str) -> bool) -> bool {
        let mut changed = false;
        for p in &mut self.profiles {
            let before = p.off.len();
            p.off.retain(|d| !gone(d));
            changed |= p.off.len() != before;
        }
        changed
    }
}

/// A profile as the file holds it.
fn profile_from(value: &toml::Value) -> Option<Profile> {
    let table = value.as_table()?;
    let id = table.get("id")?.as_str()?;
    if !valid_profile_id(id) {
        return None;
    }
    let name = match table.get("name") {
        None => String::new(),
        Some(name) => name.as_str()?.trim().to_owned(),
    };
    let switch = |key: &str, default: bool| match table.get(key) {
        None => default,
        Some(value) => value.as_bool().unwrap_or_else(|| {
            warn!(key, %value, "taking a switch that is not true or false as its default");
            default
        }),
    };
    let off = match table.get("off").map(toml::Value::as_array) {
        None => BTreeSet::new(),
        Some(None) => {
            warn!(%id, "taking the devices not connected in a profile, not a list, as none");
            BTreeSet::new()
        }
        Some(Some(list)) => list
            .iter()
            .filter_map(|d| {
                let device = d.as_str().filter(|d| iw_net::is_valid_device_id(d));
                if device.is_none() {
                    warn!(value = %d, "dropping a device that is none from a profile");
                }
                device.map(str::to_owned)
            })
            .collect(),
    };
    let networks = match table.get("networks").map(toml::Value::as_array) {
        None => Vec::new(),
        Some(None) => {
            warn!(%id, "taking the networks of a profile, not a list, as none");
            Vec::new()
        }
        Some(Some(list)) => list
            .iter()
            .filter_map(|n| {
                let network = network_from(n);
                if network.is_none() {
                    warn!(%id, "dropping a network that is not one from a profile");
                }
                network
            })
            .collect(),
    };
    Some(Profile {
        id: id.to_owned(),
        name,
        paused: switch("paused", false),
        off,
        clipboard: switch("clipboard", true),
        files: switch("files", true),
        networks,
    })
}

/// A network as the file holds it. Names that are none are left out, the
/// rest of it kept.
fn network_from(value: &toml::Value) -> Option<Network> {
    let table = value.as_table()?;
    let id = table.get("id")?.as_str()?;
    if !valid_network_id(id) {
        return None;
    }
    let kind = NetworkKind::from_key(table.get("kind")?.as_str()?)?;
    let router = match table.get("router") {
        None => None,
        Some(router) => Some(router.as_str()?.parse::<Ipv4Addr>().ok()?),
    };
    let text = |key: &str, ok: fn(&str) -> bool| {
        table
            .get(key)
            .and_then(toml::Value::as_str)
            .filter(|s| ok(s))
            .map(str::to_owned)
    };
    Some(Network {
        id: id.to_owned(),
        kind,
        router,
        wifi_name: text("wifi_name", |s| {
            iw_platform::network::wifi_name_of(s.as_bytes()).is_some()
        }),
        network_name: text("network_name", valid_network_name),
        name: text("name", valid_name),
    })
}

/// Whether `name` may be what Windows calls a network, as kept.
pub fn valid_network_name(name: &str) -> bool {
    iw_platform::network::network_name_of(name).is_some()
}

/// The file as written.
#[derive(Serialize)]
struct Written<'a> {
    salt: &'a str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    wifi_names: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    chosen: Option<&'a str>,
    profiles: Vec<WrittenProfile<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    picks: Vec<WrittenPick<'a>>,
}

#[derive(Serialize)]
struct WrittenPick<'a> {
    network: &'a str,
    profile: &'a str,
}

#[derive(Serialize)]
struct WrittenProfile<'a> {
    id: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    name: &'a str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    paused: bool,
    #[serde(skip_serializing_if = "is_true")]
    clipboard: bool,
    #[serde(skip_serializing_if = "is_true")]
    files: bool,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    off: &'a BTreeSet<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    networks: Vec<WrittenNetwork<'a>>,
}

#[derive(Serialize)]
struct WrittenNetwork<'a> {
    id: &'a str,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    router: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wifi_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
}

fn is_true(on: &bool) -> bool {
    *on
}

impl<'a> Written<'a> {
    fn of(file: &'a ProfilesFile) -> Self {
        Self {
            salt: &file.salt,
            wifi_names: file.wifi_names,
            chosen: file.chosen.as_deref(),
            picks: file
                .picks
                .iter()
                .map(|p| WrittenPick {
                    network: &p.network,
                    profile: &p.profile,
                })
                .collect(),
            profiles: file
                .profiles
                .iter()
                .map(|p| WrittenProfile {
                    id: &p.id,
                    name: &p.name,
                    paused: p.paused,
                    clipboard: p.clipboard,
                    files: p.files,
                    off: &p.off,
                    networks: p
                        .networks
                        .iter()
                        .map(|n| WrittenNetwork {
                            id: &n.id,
                            kind: n.kind.key(),
                            router: n.router.map(|ip| ip.to_string()),
                            wifi_name: n.wifi_name.as_deref(),
                            network_name: n.network_name.as_deref(),
                            name: n.name.as_deref(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SALT: &str = "00112233445566778899aabbccddeeff";

    /// The id of what `what` says, with a salt of the tests'.
    fn network_id(what: &str) -> String {
        super::network_id(SALT, what)
    }

    fn network(what: &str, kind: NetworkKind, router: [u8; 4]) -> Network {
        Network {
            id: network_id(what),
            kind,
            router: Some(Ipv4Addr::from(router)),
            ..Network::default()
        }
    }

    fn sample() -> ProfilesFile {
        let mut file = ProfilesFile::default();
        let office = file.add("Office").unwrap();
        file.edit(
            &office,
            &ProfileEdit {
                files: Some(false),
                devices: vec![("mini".to_owned(), false)],
                ..ProfileEdit::default()
            },
            |_| true,
        )
        .unwrap();
        let mut wifi = network("office", NetworkKind::Wifi, [198, 51, 100, 1]);
        wifi.wifi_name = Some("Office-5G".to_owned());
        file.add_network(&office, &wifi).unwrap();
        file.add_network(
            DEFAULT,
            &network("home", NetworkKind::Wired, [192, 168, 1, 1]),
        )
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
        file.rename_network(&network_id("home"), "Home").unwrap();
        file.choose(Some(&office)).unwrap();
        file.wifi_names = true;
        assert!(file.picked(&network_id("office annex"), &office));
        file
    }

    #[test]
    fn the_profile_picked_on_each_network_is_kept_as_it_changes() {
        let mut file = sample();
        let annex = network_id("office annex");
        assert_eq!(file.last_pick(&annex).unwrap().name, "Office");
        // The same again changes nothing, so nothing is written.
        assert!(!file.picked(&annex, "p1"));
        let cafe = network_id("cafe");
        assert!(file.picked(&cafe, OTHER));
        assert!(file.picked(&cafe, DEFAULT));
        assert_eq!(file.last_pick(&cafe).unwrap().id, DEFAULT);
        assert_eq!(file.picks.len(), 2);
        assert!(file.last_pick(&network_id("elsewhere")).is_none());
        // The most recent networks only; changed, one is the latest.
        assert!(file.picked(&annex, OTHER));
        for n in 0..MAX_PICKS {
            file.picked(&network_id(&format!("n{n}")), DEFAULT);
        }
        assert_eq!(file.picks.len(), MAX_PICKS);
        assert!(file.last_pick(&cafe).is_none() && file.last_pick(&annex).is_none());
        // Listed by its id, a network is used by that; a profile removed
        // is no one's pick.
        let mut file = sample();
        assert!(file.picked(&cafe, DEFAULT));
        file.add_network("p1", &network("cafe", NetworkKind::Wifi, [172, 16, 0, 1]))
            .unwrap();
        assert!(file.last_pick(&cafe).is_none());
        file.remove("p1").unwrap();
        assert!(file.last_pick(&annex).is_none());
        assert!(file.picks.is_empty());
    }

    #[test]
    fn picks_that_cannot_be_read_or_are_of_listed_networks_are_dropped() {
        let home = network_id("home");
        let annex = network_id("office annex");
        let text = format!(
            "salt = \"{SALT}\"\n\
             [[profiles]]\nid = \"default\"\n\
             [[profiles.networks]]\nid = \"{home}\"\nkind = \"wired\"\n\
             [[picks]]\nnetwork = \"home\"\nprofile = \"default\"\n\
             [[picks]]\nnetwork = \"{home}\"\nprofile = \"other\"\n\
             [[picks]]\nnetwork = \"{annex}\"\nprofile = \"default\"\n\
             [[picks]]\nnetwork = \"{annex}\"\nprofile = \"other\"\n"
        );
        let read = ProfilesFile::parse(&text).unwrap();
        assert_eq!(
            read.picks,
            [LastPick {
                network: annex,
                profile: OTHER.to_owned()
            }]
        );
        assert_eq!(read.salt, SALT);
    }

    #[test]
    fn saved_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.toml");
        assert_eq!(ProfilesFile::load(&path), Loaded::Missing);
        let file = sample();
        file.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Profiles, written by the InputWeave agent."));
        // Never a hardware address, only what stands for it.
        assert!(!text.contains("mac"));
        assert!(text.contains("wifi_name = \"Office-5G\""));
        assert!(text.contains("[[picks]]"));
        assert_eq!(ProfilesFile::load(&path), Loaded::File(file));
        // There but not one: unreadable, not missing.
        std::fs::write(&path, "not toml [").unwrap();
        assert_eq!(ProfilesFile::load(&path), Loaded::Unreadable);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn the_defaults_are_default_and_other_networks_sharing_with_every_device() {
        let file = ProfilesFile::default();
        let ids: Vec<&str> = file.profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, [DEFAULT, OTHER]);
        for p in &file.profiles {
            assert!(p.connects("win") && p.clipboard && p.files && p.networks.is_empty());
        }
        assert_eq!(file.chosen, None);
        assert!(!file.wifi_names);
        // An empty file is the same, but for its salt, made anew.
        let empty = ProfilesFile::parse("").unwrap();
        assert_ne!(empty.salt, file.salt);
        assert_eq!(
            empty,
            ProfilesFile {
                salt: empty.salt.clone(),
                ..file
            }
        );
    }

    #[test]
    fn new_profiles_get_numbered_names_and_go_before_other_networks() {
        let mut file = ProfilesFile::default();
        assert_eq!(file.add("New Profile").unwrap(), "p1");
        assert_eq!(file.add("New Profile").unwrap(), "p2");
        assert_eq!(file.add(" New Profile ").unwrap(), "p3");
        let names: Vec<&str> = file.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["", "New Profile", "New Profile 2", "New Profile 3", ""]
        );
        assert_eq!(file.profiles.last().unwrap().id, OTHER);
        // Removed, its id is not given again while a later one is there.
        file.remove("p2").unwrap();
        assert_eq!(file.add("New Profile").unwrap(), "p4");
        assert_eq!(file.profile("p4").unwrap().name, "New Profile 2");
        // The longest name keeps its number whole.
        let long = "x".repeat(64);
        file.add(&long).unwrap();
        let id = file.add(&long).unwrap();
        let name = &file.profile(&id).unwrap().name;
        assert_eq!(name.chars().count(), 64);
        assert!(name.ends_with(" 2"));
        assert_eq!(file.add(""), Err(ProfileError::InvalidName));
        assert_eq!(file.add("a\nb"), Err(ProfileError::InvalidName));
        while file.profiles.len() < MAX_PROFILES {
            file.add("More").unwrap();
        }
        assert_eq!(file.add("More"), Err(ProfileError::TooManyProfiles));
    }

    #[test]
    fn a_name_another_profile_has_or_a_fixed_ones_is_taken() {
        let mut file = ProfilesFile::default();
        // The fixed ones' names, in either language and whatever the case.
        for name in ["Default", " default ", "預設", "OTHER NETWORKS", "其他網路"] {
            assert_eq!(file.add(name), Err(ProfileError::NameTaken), "{name}");
        }
        let office = file.add("Office").unwrap();
        // Added again, numbered, as ever, whatever the case.
        let again = file.add("office").unwrap();
        assert_eq!(file.profile(&again).unwrap().name, "office 2");
        let rename = |name: &str| ProfileEdit {
            name: Some(name.to_owned()),
            ..ProfileEdit::default()
        };
        assert_eq!(
            file.edit(&again, &rename("OFFICE"), |_| true),
            Err(ProfileError::NameTaken)
        );
        assert_eq!(
            file.edit(&again, &rename("Other Networks"), |_| true),
            Err(ProfileError::NameTaken)
        );
        assert_eq!(file.profile(&again).unwrap().name, "office 2");
        // Its own name, in another case, is its own.
        assert_eq!(file.edit(&office, &rename("OFFICE"), |_| true), Ok(true));
        assert_eq!(file.edit(&again, &rename("Lab"), |_| true), Ok(true));
        assert_eq!(ProfileError::NameTaken.code(), "name_taken");
    }

    #[test]
    fn ids_go_on_past_the_highest_and_never_overflow() {
        let text = "[[profiles]]\nid = \"p4294967295\"\nname = \"Last\"\n";
        let mut file = ProfilesFile::parse(text).unwrap();
        assert_eq!(file.add("One").unwrap(), "p1");
        assert_eq!(file.add("Two").unwrap(), "p2");
        let text = "[[profiles]]\nid = \"p7\"\nname = \"Seven\"\n";
        let mut file = ProfilesFile::parse(text).unwrap();
        assert_eq!(file.add("Eight").unwrap(), "p8");
    }

    #[test]
    fn a_network_moved_to_another_profile_keeps_its_names() {
        let mut file = sample();
        let home = network_id("home");
        file.fill_wifi_name(&home, "Home-5G");
        file.learn(&Network {
            network_name: Some("Network 2".to_owned()),
            ..file.network(&home).unwrap().clone()
        });
        // As read now, without the names the system said before.
        let read = network("home", NetworkKind::Wired, [192, 168, 1, 1]);
        assert!(file.add_network("p1", &read).unwrap());
        let moved = file.network(&home).unwrap();
        assert_eq!(file.listing(&home).unwrap().id, "p1");
        assert_eq!(moved.name.as_deref(), Some("Home"));
        assert_eq!(moved.wifi_name.as_deref(), Some("Home-5G"));
        assert_eq!(moved.network_name.as_deref(), Some("Network 2"));
        // A reading with names of its own: those.
        let mut named = read.clone();
        named.wifi_name = Some("Home-6G".to_owned());
        assert!(file.add_network(DEFAULT, &named).unwrap());
        let moved = file.network(&home).unwrap();
        assert_eq!(moved.wifi_name.as_deref(), Some("Home-6G"));
        assert_eq!(moved.network_name.as_deref(), Some("Network 2"));
    }

    #[test]
    fn a_listed_network_moves_even_at_the_most_there_may_be() {
        let mut file = ProfilesFile::default();
        let office = file.add("Office").unwrap();
        for i in 0..MAX_NETWORKS {
            let n = network(
                &format!("n{i}"),
                NetworkKind::Wired,
                [10, 0, (i / 250) as u8, 1],
            );
            file.add_network(DEFAULT, &n).unwrap();
        }
        let first = network("n0", NetworkKind::Wired, [10, 0, 0, 1]);
        assert_eq!(file.add_network(&office, &first), Ok(true));
        assert_eq!(file.listing(&first.id).unwrap().id, office);
        let more = network("more", NetworkKind::Wired, [10, 9, 9, 1]);
        assert_eq!(
            file.add_network(&office, &more),
            Err(ProfileError::TooManyNetworks)
        );
    }

    #[test]
    fn profiles_dropped_from_the_file_claim_no_networks() {
        let id = network_id("home");
        // A profile without a name, listing the network first, is dropped:
        // the network stays with the one after it.
        let text = format!(
            "[[profiles]]\nid = \"p1\"\n[[profiles.networks]]\nid = \"{id}\"\nkind = \"wired\"\n\n\
             [[profiles]]\nid = \"p2\"\nname = \"Home\"\n[[profiles.networks]]\nid = \"{id}\"\nkind = \"wired\"\n"
        );
        let file = ProfilesFile::parse(&text).unwrap();
        assert_eq!(file.listing(&id).unwrap().id, "p2");
        // Profiles past the most there may be are dropped, their networks
        // with them, before the networks are counted.
        let mut text = String::new();
        for p in 1..=MAX_PROFILES + 5 {
            text.push_str(&format!("[[profiles]]\nid = \"p{p}\"\nname = \"P{p}\"\n"));
            text.push_str(&format!(
                "[[profiles.networks]]\nid = \"{}\"\nkind = \"wired\"\n",
                network_id(&format!("n{p}"))
            ));
        }
        let file = ProfilesFile::parse(&text).unwrap();
        assert_eq!(file.profiles.len(), MAX_PROFILES);
        assert_eq!(file.network_count(), MAX_PROFILES - 2);
        assert!(
            file.network(&network_id(&format!("n{}", MAX_PROFILES + 1)))
                .is_none()
        );
    }

    #[test]
    fn the_fixed_profiles_stay() {
        let mut file = sample();
        let rename = ProfileEdit {
            name: Some("Home".to_owned()),
            ..ProfileEdit::default()
        };
        assert_eq!(
            file.edit(DEFAULT, &rename, |_| true),
            Err(ProfileError::FixedProfile)
        );
        assert_eq!(
            file.edit(OTHER, &rename, |_| true),
            Err(ProfileError::FixedProfile)
        );
        assert_eq!(file.remove(DEFAULT), Err(ProfileError::FixedProfile));
        assert_eq!(file.remove(OTHER), Err(ProfileError::FixedProfile));
        assert_eq!(
            file.remove("nope"),
            Err(ProfileError::UnknownProfile("nope".to_owned()))
        );
        // Other networks lists none.
        let net = network("cafe", NetworkKind::Wifi, [172, 16, 0, 1]);
        assert_eq!(
            file.add_network(OTHER, &net),
            Err(ProfileError::OtherNetworks)
        );
    }

    #[test]
    fn removing_the_chosen_profile_goes_back_to_automatic() {
        let mut file = sample();
        let office = file.chosen.clone().unwrap();
        file.remove(&office).unwrap();
        assert_eq!(file.chosen, None);
        // Its networks go with it.
        assert!(file.network(&network_id("office")).is_none());
        assert_eq!(
            file.choose(Some("nope")),
            Err(ProfileError::UnknownProfile("nope".to_owned()))
        );
        assert_eq!(file.choose(None), Ok(false));
    }

    #[test]
    fn a_network_is_in_one_profile_and_its_name_goes_with_it() {
        let mut file = sample();
        let home = network("home", NetworkKind::Wired, [192, 168, 1, 1]);
        let office = file.profiles[1].id.clone();
        assert_eq!(file.add_network(&office, &home), Ok(true));
        assert!(file.profile(DEFAULT).unwrap().networks.is_empty());
        assert_eq!(file.listing(&home.id).unwrap().id, office);
        assert_eq!(
            file.network(&home.id).unwrap().name.as_deref(),
            Some("Home")
        );
        assert_eq!(file.add_network(&office, &home), Ok(false));
        assert_eq!(file.rename_network(&home.id, ""), Ok(true));
        assert_eq!(file.network(&home.id).unwrap().shown_name(), None);
        assert_eq!(
            file.rename_network(&home.id, "a\u{7}b"),
            Err(ProfileError::InvalidName)
        );
        assert_eq!(
            file.rename_network("nope", "x"),
            Err(ProfileError::UnknownNetwork("nope".to_owned()))
        );
        file.remove_network(&home.id).unwrap();
        assert!(file.listing(&home.id).is_none());
        assert_eq!(
            file.remove_network(&home.id),
            Err(ProfileError::UnknownNetwork(home.id.clone()))
        );
    }

    #[test]
    fn what_the_system_says_of_a_listed_network_is_learned_and_kept_up_to_date() {
        let mut file = sample();
        let mut seen = network("home", NetworkKind::Wired, [192, 168, 1, 1]);
        assert!(!file.learn(&seen));
        // A name Windows gave as it was still finding out, then the real one.
        seen.network_name = Some("Identifying...".to_owned());
        assert!(file.learn(&seen));
        assert!(!file.learn(&seen));
        seen.network_name = Some("Network 2".to_owned());
        assert!(file.learn(&seen));
        let listed = file.network(&seen.id).unwrap();
        assert_eq!(listed.network_name.as_deref(), Some("Network 2"));
        // A reading that says nothing takes nothing away, and the name given
        // here stays.
        seen.network_name = None;
        assert!(!file.learn(&seen));
        assert_eq!(listed_name(&file, &seen.id), Some("Home"));
        // Reached over Wi-Fi now, to the same router: as it is now.
        seen.kind = NetworkKind::Wifi;
        assert!(file.learn(&seen));
        assert_eq!(file.network(&seen.id).unwrap().kind, NetworkKind::Wifi);
        assert!(!file.learn(&seen));
    }

    fn listed_name<'a>(file: &'a ProfilesFile, id: &str) -> Option<&'a str> {
        file.network(id).and_then(|n| n.name.as_deref())
    }

    #[test]
    fn a_wifi_name_an_app_read_fills_in_only() {
        let mut file = sample();
        let home = network_id("home");
        assert!(file.fill_wifi_name(&home, "Home-Wi-Fi"));
        assert_eq!(
            file.network(&home).unwrap().wifi_name.as_deref(),
            Some("Home-Wi-Fi")
        );
        // Never over one there is.
        assert!(!file.fill_wifi_name(&home, "Other"));
        assert!(!file.fill_wifi_name(&network_id("office"), "Other"));
        assert!(!file.fill_wifi_name("nope", "Other"));
    }

    #[test]
    fn devices_are_connected_until_turned_off_and_forgotten_once_untrusted() {
        let mut file = sample();
        let office = file.profiles[1].id.clone();
        assert!(!file.profile(&office).unwrap().connects("mini"));
        assert!(file.profile(&office).unwrap().connects("win"));
        // A device neither trusted nor listed cannot be turned off.
        let off = |d: &str| ProfileEdit {
            devices: vec![(d.to_owned(), false)],
            ..ProfileEdit::default()
        };
        assert_eq!(
            file.edit(&office, &off("stranger"), |d| d != "stranger"),
            Err(ProfileError::UnknownDevice("stranger".to_owned()))
        );
        assert!(file.forget_devices(|d| d == "mini"));
        assert!(file.profile(&office).unwrap().connects("mini"));
        assert!(!file.forget_devices(|d| d == "mini"));
        // Paused, it connects none.
        assert!(!file.profile(OTHER).unwrap().connects("win"));
    }

    #[test]
    fn whatever_cannot_be_read_is_dropped_and_the_rest_kept() {
        let id = network_id("home");
        let text = format!(
            r#"
wifi_names = "yes"
chosen = "p9"

[[profiles]]
id = "p1"
name = "Office"
paused = "no"
off = ["mini", "", 3]

[[profiles.networks]]
id = "{id}"
kind = "wired"
router = "192.168.1.1"
wifi_name = "a\nb"
name = "Home"

[[profiles.networks]]
id = "not hex"
kind = "wired"

[[profiles.networks]]
id = "{id}"
kind = "wifi"

[[profiles]]
id = "Bad Id"
name = "x"

[[profiles]]
id = "p2"

[[profiles]]
id = "other"
name = "Elsewhere"

[[profiles.networks]]
id = "{id}"
kind = "vpn"
"#
        );
        let file = ProfilesFile::parse(&text).unwrap();
        let ids: Vec<&str> = file.profiles.iter().map(|p| p.id.as_str()).collect();
        // Default made anew first, Other networks last, the profile without
        // a name and the one with a bad id dropped.
        assert_eq!(ids, [DEFAULT, "p1", OTHER]);
        let office = file.profile("p1").unwrap();
        assert!(!office.paused);
        assert_eq!(office.off, BTreeSet::from(["mini".to_owned()]));
        assert_eq!(office.networks.len(), 1);
        assert_eq!(office.networks[0].wifi_name, None);
        assert_eq!(office.networks[0].name.as_deref(), Some("Home"));
        let other = file.profile(OTHER).unwrap();
        assert!(other.name.is_empty() && other.networks.is_empty());
        assert!(!file.wifi_names);
        assert_eq!(file.chosen, None);
        assert!(ProfilesFile::parse("profiles = 3").is_ok());
        assert!(ProfilesFile::parse("not toml [").is_err());
    }

    #[test]
    fn network_ids_stand_for_what_they_were_made_of() {
        assert_eq!(network_id("a"), network_id("a"));
        assert_ne!(network_id("a"), network_id("b"));
        assert!(valid_network_id(&network_id("a")));
    }

    #[test]
    fn each_install_salts_the_ids_of_networks_its_own_way() {
        let (one, two) = (ProfilesFile::default(), ProfilesFile::default());
        assert!(valid_network_id(&one.salt) && valid_network_id(&two.salt));
        assert_ne!(one.salt, two.salt);
        // The same router, another id.
        let what = "router 192.168.1.1 3cfb22000001";
        assert_ne!(
            super::network_id(&one.salt, what),
            super::network_id(&two.salt, what)
        );
        assert_ne!(super::network_id(&one.salt, what), network_id(what));

        // Kept with the file: the same id once read again.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.toml");
        one.save(&path).unwrap();
        let Loaded::File(again) = ProfilesFile::load(&path) else {
            panic!("not read back");
        };
        assert_eq!(again.salt, one.salt);
        assert_eq!(
            super::network_id(&again.salt, what),
            super::network_id(&one.salt, what)
        );

        // None, or one that is not one: a new one.
        let text = std::fs::read_to_string(&path).unwrap();
        let without: String = text
            .lines()
            .filter(|l| !l.starts_with("salt"))
            .collect::<Vec<_>>()
            .join("\n");
        let fresh = ProfilesFile::parse(&without).unwrap();
        assert!(valid_network_id(&fresh.salt) && fresh.salt != one.salt);
        let fresh = ProfilesFile::parse("salt = \"ABC\"").unwrap();
        assert!(valid_network_id(&fresh.salt));
    }
}
