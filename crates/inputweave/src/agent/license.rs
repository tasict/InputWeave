//! This computer's license (docs/architecture.md, "License"): every
//! feature works without one; a license bought from Polar shows the
//! computer as licensed, on up to five computers.
//!
//! The agent keeps the record and passes the key to paired devices; it
//! never talks to Polar. The app asks Polar (activate, validate,
//! deactivate) and reports what Polar answered (`RecordLicenseResult`).
//! The record lives here rather than in the app because the agent is
//! what takes keys from paired devices, so removing a license must stop
//! that here, and because on Windows the agent is one per computer while
//! an app runs once per signed-in user, so all of a PC's users share its
//! seat. On macOS and Linux the agent, and so `license.toml`, belong to
//! one user: each user account that activates takes a seat of its own.
//!
//! The record is kept in `license.toml` next to the devices file, readable
//! only by its owner, replaced in one step like the devices file, and
//! written off the engine's task ([`LicenseSaver`]):
//!
//! ```toml
//! key = "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42"
//! state = "licensed"
//! activation_id = "0f8e…"
//! customer_email = "someone@example.com"
//! validated_at = 1793800000
//! source_device_id = "win"
//! source_device_name = "Windows PC"
//! refused = ["INPUTWEAVE-0B1C2D3E-4F50-4162-8374-95A6B7C8D9E0"]
//!
//! [[peers]]
//! id = "win"
//! key = "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42"
//! licensed = true
//! ```
//!
//! Which key wins between devices: a key activated here is never replaced.
//! This computer takes a paired device's key only when it has none, or its
//! own was refused (seats full, invalid), and that device says it is
//! licensed with a different key that Polar has not refused here before
//! (`refused`, cleared when the user enters a key, so two full keys are not
//! tried in turn forever); never after its user removed the license here
//! (`opted_out`), until a key is entered here again. The same key is
//! nothing to take.
//!
//! Polar's answers are matched to the state they answer: an activation only
//! while a key waits for one, a confirmation or an "invalid" from validate
//! only for the activation this computer has, a refusal of an activation
//! only while it waits. Anything else is stale and changes nothing.
//!
//! A `license.toml` that is there but cannot be read (permissions, an I/O
//! error) makes the license unavailable rather than empty: nothing is taken
//! from or told to paired devices, changes are refused
//! ([`LicenseError::Unavailable`]), and the file is never written, until a
//! later read of it succeeds.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::config::ConfigError;

/// The prefix of InputWeave's keys at Polar, the same for every key.
pub const KEY_PREFIX: &str = "INPUTWEAVE-";
/// Polar's keys are a prefix, a hyphen and a UUID (36 characters); these
/// leave room for another prefix without taking anything that cannot be
/// one.
pub const MIN_KEY_LEN: usize = 20;
pub const MAX_KEY_LEN: usize = 96;
/// Activation ids are Polar's UUIDs; this leaves room.
pub const MAX_ACTIVATION_ID_LEN: usize = 128;
/// The longest email address there can be.
pub const MAX_EMAIL_LEN: usize = 320;
/// How many keys Polar refused here are remembered, the oldest dropped.
pub const MAX_REFUSED: usize = 8;
/// When Polar confirmed a license is never before 2020-01-01, nor more
/// than a day ahead of this computer's clock.
pub const EARLIEST_VALIDATION: u64 = 1_577_836_800;
const VALIDATION_AHEAD: u64 = 86_400;

const HEADER: &str = "# This computer's InputWeave license, written by the InputWeave agent.\n\
                      # Edit only while the agent is stopped.\n\n";

/// Where a license stands on this computer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenseState {
    /// No key.
    #[default]
    None,
    /// A key for the app to activate with Polar.
    PendingActivation,
    /// Activated here.
    Licensed,
    /// Polar refused to activate it here: every seat is taken.
    SeatsFull,
    /// Polar does not know the key, it was revoked or refunded, or this
    /// computer's activation is gone.
    Invalid,
    /// `license.toml` is there but cannot be read right now. Only the
    /// status shows it; it is never written.
    #[serde(skip_serializing)]
    Unavailable,
}

/// A state as the file names it: none when the file names none, or a name
/// this build does not know (written by a newer agent) as `Err`.
fn state_named(name: Option<&str>) -> Result<LicenseState, ()> {
    Ok(match name {
        None | Some("none") => LicenseState::None,
        Some("pending_activation") => LicenseState::PendingActivation,
        Some("licensed") => LicenseState::Licensed,
        Some("seats_full") => LicenseState::SeatsFull,
        Some("invalid") => LicenseState::Invalid,
        Some(_) => return Err(()),
    })
}

/// Why Polar refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    SeatsFull,
    Invalid,
}

/// What Polar answered about a key, as the app reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LicenseResult {
    /// `activate` answered 200.
    Activated {
        activation_id: String,
        customer_email: String,
        validated_at: u64,
    },
    /// `validate` answered 200 for `activation_id`.
    Validated {
        activation_id: String,
        validated_at: u64,
        /// Empty keeps the one recorded.
        customer_email: String,
    },
    /// Polar refused: `activate` (no `activation_id`), or `validate` for
    /// `activation_id`, which only ever says it is invalid.
    Refused {
        refusal: Refusal,
        activation_id: String,
    },
}

/// Why a license change was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LicenseError {
    #[error(
        "a license key is {MIN_KEY_LEN} to {MAX_KEY_LEN} printable ASCII characters without spaces"
    )]
    InvalidKey,
    #[error("the result has no outcome, or a field is empty, too long or does not fit it")]
    InvalidResult,
    #[error("the license key changed while Polar was asked about another")]
    Changed,
    #[error("this computer is already licensed with the key; free the new activation")]
    AlreadyLicensed,
    #[error("the answer is about a state the license is no longer in")]
    Stale,
    #[error("this agent keeps no license")]
    Unsupported,
    #[error("the license file cannot be read right now")]
    Unavailable,
    #[error("the agent is shutting down")]
    Stopping,
}

impl LicenseError {
    /// The error code clients are told.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidKey => "invalid_license_key",
            Self::InvalidResult => "invalid_license_result",
            Self::Changed => "license_changed",
            Self::AlreadyLicensed => "already_licensed",
            Self::Stale => "license_stale",
            Self::Unsupported => "unsupported",
            Self::Unavailable => "license_unavailable",
            Self::Stopping => "agent_stopping",
        }
    }
}

/// Whether `key` is one a license can have: [`MIN_KEY_LEN`] to
/// [`MAX_KEY_LEN`] printable ASCII characters, none of them a space.
pub fn valid_key(key: &str) -> bool {
    (MIN_KEY_LEN..=MAX_KEY_LEN).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_graphic())
}

fn valid_activation_id(id: &str) -> bool {
    id.len() <= MAX_ACTIVATION_ID_LEN && id.bytes().all(|b| b.is_ascii_graphic())
}

fn valid_email(email: &str) -> bool {
    email.chars().count() <= MAX_EMAIL_LEN && !email.chars().any(char::is_control)
}

/// Whether `at` can be when Polar confirmed a license, by this computer's
/// clock now ([`EARLIEST_VALIDATION`]).
fn plausible_validation(at: u64) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    (EARLIEST_VALIDATION..=now.saturating_add(VALIDATION_AHEAD)).contains(&at)
}

/// Keeps the newest [`MAX_REFUSED`] refused keys.
fn cap_refused(refused: &mut Vec<String>) {
    let extra = refused.len().saturating_sub(MAX_REFUSED);
    refused.drain(..extra);
}

/// `key` for printing: never more of it than its last four characters,
/// after [`KEY_PREFIX`], which every key shares, and only for a key long
/// enough that four characters say little: `INPUTWEAVE-…7E42`. Anything
/// shorter, or not ASCII, is only `…`.
pub fn masked(key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    if !key.is_ascii() || key.len() < 12 {
        return "…".to_owned();
    }
    let prefix = if key.starts_with(KEY_PREFIX) && key.len() >= KEY_PREFIX.len() + 8 {
        KEY_PREFIX
    } else {
        ""
    };
    format!("{prefix}…{}", &key[key.len() - 4..])
}

/// What a paired device last said of its license. An entry missing a field
/// reads as empty there, and is dropped as one that cannot be a device's
/// key ([`LicenseFile::parse`]), rather than failing the whole file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct PeerLicense {
    pub id: String,
    pub key: String,
    pub licensed: bool,
}

/// This computer's license, as `license.toml` holds it, and what paired
/// devices said of theirs. Read through [`LicenseFile::parse`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LicenseFile {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub key: String,
    pub state: LicenseState,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub activation_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub customer_email: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub validated_at: u64,
    /// The paired device the key came from, and its name then; empty when
    /// it was entered here.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source_device_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source_device_name: String,
    /// The license was removed here: keys from paired devices are not
    /// taken until one is entered here.
    #[serde(skip_serializing_if = "is_false")]
    pub opted_out: bool,
    /// Keys Polar refused here (seats full, invalid) since a key was last
    /// entered here, the newest last; never taken from a paired device.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub refused: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<PeerLicense>,
}

/// `license.toml` as written, its state by name, before
/// [`LicenseFile::parse`] makes sense of it.
#[derive(Default, Deserialize)]
#[serde(default)]
struct RawLicenseFile {
    key: String,
    state: Option<String>,
    activation_id: String,
    customer_email: String,
    validated_at: u64,
    source_device_id: String,
    source_device_name: String,
    opted_out: bool,
    refused: Vec<String>,
    peers: Vec<PeerLicense>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A paired device listed among the computers on this license.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LicensedDevice {
    pub id: String,
    pub name: String,
    pub licensed: bool,
}

/// The license as the status shows it. Its `Debug`, which the diagnostics
/// report and logs use, masks the key and leaves out the activation id and
/// the buyer's email.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct LicenseStatus {
    pub key: String,
    pub state: LicenseState,
    pub activation_id: String,
    pub customer_email: String,
    pub validated_at: u64,
    pub source_device_id: String,
    pub source_device_name: String,
    pub opted_out: bool,
    pub devices: Vec<LicensedDevice>,
}

impl std::fmt::Debug for LicenseStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LicenseStatus")
            .field("key", &masked(&self.key))
            .field("state", &self.state)
            .field("activated", &!self.activation_id.is_empty())
            .field("validated_at", &self.validated_at)
            .field("source_device_id", &self.source_device_id)
            .field("source_device_name", &self.source_device_name)
            .field("opted_out", &self.opted_out)
            .field("devices", &self.devices.len())
            .finish_non_exhaustive()
    }
}

/// What reading `license.toml` came to, as the agent starts.
#[derive(Debug, PartialEq, Eq)]
pub enum Loaded {
    /// Read (or missing: no license).
    File(LicenseFile),
    /// Damaged, and moved aside: no license, saved as usual.
    SetAside,
    /// There, but it could not be read (permissions): the license is
    /// unavailable, and nothing is saved over it, until a later read
    /// succeeds.
    Unreadable,
}

/// The license as the status shows it: unavailable while its file cannot
/// be read, none where the agent keeps no license, otherwise `license`
/// with devices named as `name_of` does.
pub(crate) fn shown(
    license: Option<&LicenseFile>,
    unavailable: bool,
    name_of: impl Fn(&str) -> Option<String>,
) -> Option<LicenseStatus> {
    if unavailable {
        return Some(LicenseStatus {
            state: LicenseState::Unavailable,
            ..LicenseStatus::default()
        });
    }
    license.map(|l| l.status(name_of))
}

impl LicenseFile {
    /// Reads the file. A missing file is no license.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Reads the file, as the agent starts. A license never keeps the
    /// agent from starting: a file it cannot read as one is moved aside,
    /// to `license.toml.bad`, and the agent starts with no license; one it
    /// cannot read at all is left where it is, never written over.
    pub fn load_or_set_aside(path: &Path) -> Loaded {
        match Self::load(path) {
            Ok(file) => Loaded::File(file),
            // Not text at all is damage, like text that is not TOML.
            Err(ConfigError::Read { source, .. })
                if source.kind() != io::ErrorKind::InvalidData =>
            {
                warn!(
                    path = %path.display(), error = %source,
                    "cannot read the license; going on without one, and not saving one over it"
                );
                Loaded::Unreadable
            }
            Err(e) => {
                let aside = path.with_extension("toml.bad");
                match std::fs::rename(path, &aside) {
                    Ok(()) => warn!(
                        path = %path.display(), aside = %aside.display(), error = %e,
                        "the license file is damaged; moved it aside and starting without a license"
                    ),
                    Err(rename) => warn!(
                        path = %path.display(), error = %e, rename = %rename,
                        "the license file is damaged and cannot be moved aside; starting without a license"
                    ),
                }
                Loaded::SetAside
            }
        }
    }

    /// The file as `text` holds it. Values over their limits are dropped,
    /// as if the file had been edited by hand into something the protocol
    /// would refuse; what is left is made consistent. A state this build
    /// does not know, written by a newer agent, reads as licensed when an
    /// activation goes with it (the app's next validate settles it), and
    /// otherwise as waiting for activation, or none without a key.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let raw: RawLicenseFile = toml::from_str(text)?;
        let known = state_named(raw.state.as_deref());
        let mut file = Self {
            key: raw.key,
            state: known.unwrap_or(LicenseState::None),
            activation_id: raw.activation_id,
            customer_email: raw.customer_email,
            validated_at: raw.validated_at,
            source_device_id: raw.source_device_id,
            source_device_name: raw.source_device_name,
            opted_out: raw.opted_out,
            refused: raw.refused,
            peers: raw.peers,
        };
        if !file.key.is_empty() && !valid_key(&file.key) {
            file = Self {
                opted_out: file.opted_out,
                refused: file.refused,
                peers: file.peers,
                ..Self::default()
            };
        }
        if !valid_activation_id(&file.activation_id) {
            file.activation_id.clear();
        }
        if !valid_email(&file.customer_email) {
            file.customer_email.clear();
        }
        if !plausible_validation(file.validated_at) {
            file.validated_at = 0;
        }
        if known.is_err() && !file.activation_id.is_empty() {
            file.state = LicenseState::Licensed;
        }
        if file.key.is_empty() {
            file.state = LicenseState::None;
        } else if file.state == LicenseState::None
            || (file.state == LicenseState::Licensed && file.activation_id.is_empty())
        {
            // A key with no state, or licensed without the activation to
            // confirm: the app activates it again.
            file.state = LicenseState::PendingActivation;
            file.activation_id.clear();
        }
        file.refused.retain(|k| valid_key(k));
        cap_refused(&mut file.refused);
        file.peers.retain(|p| valid_key(&p.key) && !p.id.is_empty());
        Ok(file)
    }

    /// Replaces the file in one step, readable only by its owner.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string(self).map_err(io::Error::other)?;
        crate::store::write_private(path, HEADER, &text)
    }

    /// What this computer tells paired devices: its key and whether it is
    /// licensed with it; an empty key when it has none, or its key was
    /// refused as invalid.
    pub fn outgoing(&self) -> (String, bool) {
        if self.key.is_empty() || self.state == LicenseState::Invalid {
            (String::new(), false)
        } else {
            (self.key.clone(), self.state == LicenseState::Licensed)
        }
    }

    /// The user entered `key` here (or asked to try it again). It must be
    /// [`valid_key`] (trimmed). Keys Polar refused before are forgotten:
    /// the user chose to try again. The key this computer already has,
    /// licensed or waiting, keeps its record, so its activation is not lost
    /// and no second seat taken, though it now counts as entered here; a
    /// refused one starts over, for the app to activate again.
    pub fn set_key(&mut self, key: &str) -> Result<(), LicenseError> {
        let key = key.trim();
        if !valid_key(key) {
            return Err(LicenseError::InvalidKey);
        }
        self.refused.clear();
        if key == self.key
            && matches!(
                self.state,
                LicenseState::Licensed | LicenseState::PendingActivation
            )
        {
            self.opted_out = false;
            self.source_device_id.clear();
            self.source_device_name.clear();
            return Ok(());
        }
        *self = Self {
            key: key.to_owned(),
            state: LicenseState::PendingActivation,
            peers: std::mem::take(&mut self.peers),
            ..Self::default()
        };
        Ok(())
    }

    /// The app reports Polar's answer about `key`. An answer that does not
    /// fit the state it would change is stale ([`LicenseError::Stale`]); an
    /// activation for a computer already licensed is
    /// [`LicenseError::AlreadyLicensed`], and the app frees it.
    pub fn record(&mut self, key: &str, result: LicenseResult) -> Result<(), LicenseError> {
        let fits = match &result {
            LicenseResult::Activated {
                activation_id,
                customer_email,
                validated_at,
            }
            | LicenseResult::Validated {
                activation_id,
                customer_email,
                validated_at,
            } => {
                !activation_id.is_empty()
                    && valid_activation_id(activation_id)
                    && valid_email(customer_email)
                    && plausible_validation(*validated_at)
            }
            LicenseResult::Refused {
                refusal,
                activation_id,
            } => {
                valid_activation_id(activation_id)
                    // validate never says the seats are full.
                    && (activation_id.is_empty() || *refusal == Refusal::Invalid)
            }
        };
        if !fits {
            return Err(LicenseError::InvalidResult);
        }
        if self.key.is_empty() || key != self.key {
            return Err(LicenseError::Changed);
        }
        let licensed_as =
            |id: &str| self.state == LicenseState::Licensed && id == self.activation_id;
        match result {
            LicenseResult::Activated {
                activation_id,
                customer_email,
                validated_at,
            } => {
                match self.state {
                    LicenseState::PendingActivation => {}
                    // The same activation reported again (a retried
                    // report): nothing to do, and nothing for the app to
                    // free.
                    LicenseState::Licensed if activation_id == self.activation_id => {
                        return Ok(());
                    }
                    LicenseState::Licensed => return Err(LicenseError::AlreadyLicensed),
                    _ => return Err(LicenseError::Stale),
                }
                self.state = LicenseState::Licensed;
                self.activation_id = activation_id;
                self.customer_email = customer_email;
                self.validated_at = validated_at;
            }
            LicenseResult::Validated {
                activation_id,
                validated_at,
                customer_email,
            } => {
                if !licensed_as(&activation_id) {
                    return Err(LicenseError::Stale);
                }
                self.validated_at = validated_at;
                if !customer_email.is_empty() {
                    self.customer_email = customer_email;
                }
            }
            LicenseResult::Refused {
                refusal,
                activation_id,
            } => {
                let applies = if activation_id.is_empty() {
                    self.state == LicenseState::PendingActivation
                } else {
                    licensed_as(&activation_id)
                };
                if !applies {
                    return Err(LicenseError::Stale);
                }
                self.state = match refusal {
                    Refusal::SeatsFull => LicenseState::SeatsFull,
                    Refusal::Invalid => LicenseState::Invalid,
                };
                self.activation_id.clear();
                self.remember_refused();
            }
        }
        Ok(())
    }

    /// Remembers this computer's key as refused, the newest last.
    fn remember_refused(&mut self) {
        let key = self.key.clone();
        self.refused.retain(|k| *k != key);
        self.refused.push(key);
        cap_refused(&mut self.refused);
    }

    /// The user removed the license here.
    pub fn remove(&mut self) {
        *self = Self {
            opted_out: true,
            peers: std::mem::take(&mut self.peers),
            ..Self::default()
        };
    }

    /// Device `id` said it has `key` (empty: none), activated there or not.
    /// Returns whether anything changed.
    pub fn peer_said(&mut self, id: &str, key: &str, licensed: bool) -> bool {
        let before = self.peers.clone();
        self.peers.retain(|p| p.id != id);
        if !key.is_empty() {
            self.peers.push(PeerLicense {
                id: id.to_owned(),
                key: key.to_owned(),
                licensed,
            });
            self.peers.sort_by(|a, b| a.id.cmp(&b.id));
        }
        self.peers != before
    }

    /// Forgets what devices that are no longer trusted said. Returns
    /// whether any was forgotten.
    pub fn forget_peers(&mut self, trusted: impl Fn(&str) -> bool) -> bool {
        let before = self.peers.len();
        self.peers.retain(|p| trusted(&p.id));
        self.peers.len() != before
    }

    /// Takes a paired device's key if the rules say so (see the module's
    /// documentation): only devices `name_of` names, which are the
    /// trusted ones. Returns whether it took one.
    pub fn adopt(&mut self, name_of: impl Fn(&str) -> Option<String>) -> bool {
        if self.opted_out {
            return false;
        }
        let open = self.key.is_empty()
            || matches!(self.state, LicenseState::SeatsFull | LicenseState::Invalid);
        if !open {
            return false;
        }
        let Some((peer, name)) = self
            .peers
            .iter()
            .filter(|p| {
                p.licensed
                    && valid_key(&p.key)
                    && p.key != self.key
                    && !self.refused.contains(&p.key)
            })
            .find_map(|p| name_of(&p.id).map(|name| (p.clone(), name)))
        else {
            return false;
        };
        *self = Self {
            key: peer.key,
            state: LicenseState::PendingActivation,
            source_device_id: peer.id,
            source_device_name: name,
            refused: std::mem::take(&mut self.refused),
            peers: std::mem::take(&mut self.peers),
            ..Self::default()
        };
        true
    }

    /// The license as the status shows it, naming devices as `name_of`
    /// does; devices it does not name (no longer trusted) are left out.
    pub fn status(&self, name_of: impl Fn(&str) -> Option<String>) -> LicenseStatus {
        let devices = if self.key.is_empty() {
            Vec::new()
        } else {
            self.peers
                .iter()
                .filter(|p| p.key == self.key)
                .filter_map(|p| {
                    name_of(&p.id).map(|name| LicensedDevice {
                        id: p.id.clone(),
                        name,
                        licensed: p.licensed,
                    })
                })
                .collect()
        };
        // The source's name as this device calls it now, if it still knows
        // the device.
        let source_device_name = if self.source_device_id.is_empty() {
            String::new()
        } else {
            name_of(&self.source_device_id).unwrap_or_else(|| self.source_device_name.clone())
        };
        LicenseStatus {
            key: self.key.clone(),
            state: self.state,
            activation_id: self.activation_id.clone(),
            customer_email: self.customer_email.clone(),
            validated_at: self.validated_at,
            source_device_id: self.source_device_id.clone(),
            source_device_name,
            opted_out: self.opted_out,
            devices,
        }
    }
}

/// Writes `license.toml` on a thread of its own, so the engine, which
/// changes the license, never waits for the disk while input goes through
/// it. Saves are queued in order; when several are waiting, only the
/// newest is written. A write that fails is tried again, the newest record
/// pending, after a wait that doubles up to [`SAVE_RETRY_MAX`], until one
/// succeeds; a newer record meanwhile is tried at once. Dropping it writes
/// what is queued first, trying once more if the last write failed.
pub(crate) struct LicenseSaver {
    tx: Option<std::sync::mpsc::Sender<LicenseFile>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The first wait before a failed write is tried again, and the longest.
pub(crate) const SAVE_RETRY_FIRST: std::time::Duration = std::time::Duration::from_secs(1);
pub(crate) const SAVE_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(60);

impl LicenseSaver {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self::with_retries(path, SAVE_RETRY_FIRST, SAVE_RETRY_MAX)
    }

    pub(crate) fn with_retries(
        path: PathBuf,
        first: std::time::Duration,
        longest: std::time::Duration,
    ) -> Self {
        use std::sync::mpsc::RecvTimeoutError;
        let (tx, rx) = std::sync::mpsc::channel::<LicenseFile>();
        let thread = std::thread::Builder::new()
            .name("license-save".to_owned())
            .spawn(move || {
                let mut pending: Option<LicenseFile> = None;
                let mut wait = first;
                loop {
                    // Nothing to write: wait for a record. One that failed:
                    // wait for a newer one, or the time to try again.
                    let next = match &pending {
                        None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                        Some(_) => rx.recv_timeout(wait),
                    };
                    let stopping = match next {
                        Ok(file) => {
                            pending = Some(file);
                            wait = first;
                            false
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            wait = (wait * 2).min(longest);
                            false
                        }
                        Err(RecvTimeoutError::Disconnected) => true,
                    };
                    while let Ok(newer) = rx.try_recv() {
                        pending = Some(newer);
                    }
                    if let Some(file) = &pending {
                        match file.save(&path) {
                            Ok(()) => pending = None,
                            Err(e) => warn!(
                                path = %path.display(), error = %e, retry_in = ?wait,
                                "cannot save the license; trying again"
                            ),
                        }
                    }
                    if stopping {
                        if pending.is_some() {
                            warn!(path = %path.display(), "stopping without the last license change saved");
                        }
                        return;
                    }
                }
            })
            .ok();
        if thread.is_none() {
            warn!("cannot start the thread that saves the license; it is not saved");
        }
        Self {
            tx: Some(tx),
            thread,
        }
    }

    pub(crate) fn save(&self, file: LicenseFile) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(file);
        }
    }

    /// Writes what is queued and stops.
    pub(crate) fn finish(&mut self) {
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            super::directory::off_async_threads(|| {
                let _ = thread.join();
            });
        }
    }
}

impl Drop for LicenseSaver {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Where the agent keeps the license, if it keeps one at all: the login
/// window's agent does not, since it must not write the user's folder and
/// would only tell paired devices this computer is not licensed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Licenses {
    /// No license: none is shown, kept or passed on.
    #[default]
    Off,
    /// Kept until the agent stops.
    Memory,
    /// Kept in this file (`paths::license_file`).
    File(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    const K1: &str = "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42";
    const K2: &str = "INPUTWEAVE-0B1C2D3E-4F50-4162-8374-95A6B7C8D9E0";

    fn named(id: &str) -> Option<String> {
        match id {
            "win" => Some("Windows PC".to_owned()),
            "mini" => Some("Mac mini".to_owned()),
            _ => None,
        }
    }

    fn activated(id: &str) -> LicenseResult {
        LicenseResult::Activated {
            activation_id: id.to_owned(),
            customer_email: "someone@example.com".to_owned(),
            validated_at: 1_793_800_000,
        }
    }

    fn validated(id: &str, at: u64) -> LicenseResult {
        LicenseResult::Validated {
            activation_id: id.to_owned(),
            validated_at: at,
            customer_email: String::new(),
        }
    }

    /// A refusal of an activation.
    fn refused(refusal: Refusal) -> LicenseResult {
        LicenseResult::Refused {
            refusal,
            activation_id: String::new(),
        }
    }

    /// `validate` saying this activation is gone.
    fn gone(id: &str) -> LicenseResult {
        LicenseResult::Refused {
            refusal: Refusal::Invalid,
            activation_id: id.to_owned(),
        }
    }

    #[test]
    fn keys_are_polar_shaped_printable_ascii_without_spaces() {
        assert!(valid_key(K1));
        assert!(!valid_key(""));
        assert!(!valid_key("INPUTWEAVE-1234"));
        assert!(!valid_key("INPUTWEAVE- 7F3A2C1E-58D4-4B9A"));
        assert!(!valid_key("INPUTWEAVE-\u{7}7F3A2C1E-58D4-4B9A"));
        assert!(!valid_key("INPUTWEAVE-é7F3A2C1E-58D4-4B9A"));
        assert!(valid_key(&"A".repeat(MIN_KEY_LEN)));
        assert!(!valid_key(&"A".repeat(MIN_KEY_LEN - 1)));
        assert!(valid_key(&"A".repeat(MAX_KEY_LEN)));
        assert!(!valid_key(&"A".repeat(MAX_KEY_LEN + 1)));
    }

    #[test]
    fn entering_a_key_trims_it_and_waits_for_activation() {
        let mut license = LicenseFile {
            opted_out: true,
            ..LicenseFile::default()
        };
        license.set_key(&format!("  {K1}\n")).unwrap();
        assert_eq!(license.key, K1);
        assert_eq!(license.state, LicenseState::PendingActivation);
        assert!(!license.opted_out);
        assert_eq!(license.set_key("not a key"), Err(LicenseError::InvalidKey));
        assert_eq!(license.key, K1);
    }

    #[test]
    fn polar_answers_move_the_state() {
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        license.record(K1, activated("act-1")).unwrap();
        assert_eq!(license.state, LicenseState::Licensed);
        assert_eq!(license.activation_id, "act-1");
        assert_eq!(license.outgoing(), (K1.to_owned(), true));
        license
            .record(K1, validated("act-1", 1_793_790_000))
            .unwrap();
        assert_eq!(license.validated_at, 1_793_790_000);
        assert_eq!(license.customer_email, "someone@example.com");
        license.record(K1, gone("act-1")).unwrap();
        assert_eq!(license.state, LicenseState::Invalid);
        assert!(license.activation_id.is_empty());
        // An invalid key is not passed on.
        assert_eq!(license.outgoing(), (String::new(), false));
    }

    #[test]
    fn an_activation_counts_only_while_one_is_awaited() {
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        license.record(K1, activated("act-1")).unwrap();
        // A second app, or a late reply: the app frees this one.
        assert_eq!(
            license.record(K1, activated("act-2")),
            Err(LicenseError::AlreadyLicensed)
        );
        assert_eq!(license.activation_id, "act-1");
        // The same activation told again, by a retried reply: nothing to free.
        assert_eq!(license.record(K1, activated("act-1")), Ok(()));
        assert_eq!(license.state, LicenseState::Licensed);
        assert_eq!(license.activation_id, "act-1");
        license.record(K1, gone("act-1")).unwrap();
        assert_eq!(
            license.record(K1, activated("act-3")),
            Err(LicenseError::Stale)
        );
        assert_eq!(license.state, LicenseState::Invalid);
    }

    #[test]
    fn answers_about_another_activation_or_state_are_stale() {
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        // Nothing to confirm or find gone yet.
        assert_eq!(
            license.record(K1, validated("act-1", 1_793_800_000)),
            Err(LicenseError::Stale)
        );
        assert_eq!(license.record(K1, gone("act-1")), Err(LicenseError::Stale));
        license.record(K1, activated("act-1")).unwrap();
        // About an activation this computer no longer has.
        assert_eq!(
            license.record(K1, validated("act-0", 1_793_800_000)),
            Err(LicenseError::Stale)
        );
        assert_eq!(license.record(K1, gone("act-0")), Err(LicenseError::Stale));
        // A refusal of an activation, while licensed.
        assert_eq!(
            license.record(K1, refused(Refusal::SeatsFull)),
            Err(LicenseError::Stale)
        );
        assert_eq!(license.state, LicenseState::Licensed);
        assert_eq!(license.validated_at, 1_793_800_000);
        // validate never says the seats are full.
        let full = LicenseResult::Refused {
            refusal: Refusal::SeatsFull,
            activation_id: "act-1".to_owned(),
        };
        assert_eq!(license.record(K1, full), Err(LicenseError::InvalidResult));
    }

    #[test]
    fn entering_the_same_key_again_keeps_its_activation_unless_it_was_refused() {
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        license.record(K1, activated("act")).unwrap();
        license.set_key(K1).unwrap();
        assert_eq!(license.state, LicenseState::Licensed);
        assert_eq!(license.activation_id, "act");
        let mut full = LicenseFile::default();
        full.set_key(K1).unwrap();
        full.record(K1, refused(Refusal::SeatsFull)).unwrap();
        full.set_key(K1).unwrap();
        assert_eq!(full.state, LicenseState::PendingActivation);
        assert!(full.refused.is_empty());
    }

    #[test]
    fn an_answer_about_another_key_is_ignored() {
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        assert_eq!(
            license.record(K2, activated("act")),
            Err(LicenseError::Changed)
        );
        assert_eq!(license.state, LicenseState::PendingActivation);
        let mut none = LicenseFile::default();
        assert_eq!(
            none.record(K1, activated("act")),
            Err(LicenseError::Changed)
        );
    }

    #[test]
    fn results_over_their_limits_are_refused() {
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        assert_eq!(
            license.record(K1, activated("")),
            Err(LicenseError::InvalidResult)
        );
        assert_eq!(
            license.record(K1, activated(&"a".repeat(MAX_ACTIVATION_ID_LEN + 1))),
            Err(LicenseError::InvalidResult)
        );
        assert_eq!(
            license.record(K1, validated("", 1_793_800_000)),
            Err(LicenseError::InvalidResult)
        );
        let long_email = LicenseResult::Activated {
            activation_id: "act".to_owned(),
            customer_email: "a".repeat(MAX_EMAIL_LEN + 1),
            validated_at: 1_793_800_000,
        };
        assert_eq!(
            license.record(K1, long_email),
            Err(LicenseError::InvalidResult)
        );
        // When Polar answered: not before 2020, nor days ahead of the clock.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        for at in [0, EARLIEST_VALIDATION - 1, now + 2 * 86_400, u64::MAX] {
            let result = LicenseResult::Activated {
                activation_id: "act".to_owned(),
                customer_email: String::new(),
                validated_at: at,
            };
            assert_eq!(
                license.record(K1, result),
                Err(LicenseError::InvalidResult),
                "{at}"
            );
        }
        assert_eq!(license.state, LicenseState::PendingActivation);
        license.record(K1, activated("act")).unwrap();
        assert_eq!(
            license.record(K1, validated("act", u64::MAX)),
            Err(LicenseError::InvalidResult)
        );
        assert_eq!(license.validated_at, 1_793_800_000);
    }

    #[test]
    fn a_computer_without_a_key_takes_a_licensed_peers_key() {
        let mut license = LicenseFile::default();
        // A device that waits to activate is not taken from.
        assert!(license.peer_said("win", K1, false));
        assert!(!license.adopt(named));
        assert!(license.peer_said("win", K1, true));
        assert!(license.adopt(named));
        assert_eq!(license.key, K1);
        assert_eq!(license.state, LicenseState::PendingActivation);
        assert_eq!(license.source_device_id, "win");
        assert_eq!(license.source_device_name, "Windows PC");
        // The same key again is nothing to take.
        assert!(!license.adopt(named));
        // Entered here after all: it no longer came from that device.
        license.set_key(K1).unwrap();
        assert_eq!(license.state, LicenseState::PendingActivation);
        assert_eq!(
            (
                license.source_device_id.as_str(),
                license.source_device_name.as_str()
            ),
            ("", "")
        );
    }

    #[test]
    fn a_key_activated_here_is_never_replaced() {
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        license.record(K1, activated("act")).unwrap();
        license.peer_said("win", K2, true);
        assert!(!license.adopt(named));
        assert_eq!(license.key, K1);
        // Nor one still waiting to be activated.
        let mut waiting = LicenseFile::default();
        waiting.set_key(K1).unwrap();
        waiting.peer_said("win", K2, true);
        assert!(!waiting.adopt(named));
        assert_eq!(waiting.key, K1);
    }

    #[test]
    fn a_refused_key_gives_way_to_a_licensed_peers_other_key() {
        for refusal in [Refusal::SeatsFull, Refusal::Invalid] {
            let mut license = LicenseFile::default();
            license.set_key(K1).unwrap();
            license.record(K1, refused(refusal)).unwrap();
            // The same key, licensed elsewhere, is still full here.
            license.peer_said("mini", K1, true);
            assert!(!license.adopt(named));
            license.peer_said("win", K2, true);
            assert!(license.adopt(named));
            assert_eq!(license.key, K2);
            assert_eq!(license.source_device_id, "win");
        }
    }

    #[test]
    fn keys_refused_here_are_not_tried_in_turn() {
        let mut license = LicenseFile::default();
        license.peer_said("mini", K1, true);
        license.peer_said("win", K2, true);
        assert!(license.adopt(named));
        assert_eq!(license.key, K1);
        license.record(K1, refused(Refusal::SeatsFull)).unwrap();
        assert!(license.adopt(named));
        assert_eq!(license.key, K2);
        license.record(K2, refused(Refusal::SeatsFull)).unwrap();
        // K1 was refused here: not taken again.
        assert!(!license.adopt(named));
        assert_eq!(license.key, K2);
        assert_eq!(license.refused, vec![K1.to_owned(), K2.to_owned()]);
        // Until the user enters a key.
        license.set_key(K2).unwrap();
        assert!(license.refused.is_empty());
    }

    #[test]
    fn only_the_newest_refused_keys_are_remembered() {
        let mut license = LicenseFile::default();
        for i in 0..MAX_REFUSED + 2 {
            let key = format!("INPUTWEAVE-{i:036}");
            license.key = key;
            license.state = LicenseState::PendingActivation;
            let key = license.key.clone();
            license.record(&key, refused(Refusal::Invalid)).unwrap();
        }
        assert_eq!(license.refused.len(), MAX_REFUSED);
        assert_eq!(license.refused[0], format!("INPUTWEAVE-{:036}", 2));
    }

    #[test]
    fn a_removed_license_takes_no_key_until_one_is_entered() {
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        license.record(K1, activated("act")).unwrap();
        license.remove();
        assert!(license.opted_out);
        assert!(license.key.is_empty());
        assert_eq!(license.state, LicenseState::None);
        license.peer_said("win", K1, true);
        assert!(!license.adopt(named));
        license.set_key(K2).unwrap();
        assert!(!license.opted_out);
    }

    #[test]
    fn untrusted_devices_are_neither_taken_from_nor_listed() {
        let mut license = LicenseFile::default();
        license.peer_said("stranger", K1, true);
        assert!(!license.adopt(named));
        license.set_key(K1).unwrap();
        license.peer_said("win", K1, true);
        license.peer_said("mini", K2, true);
        let status = license.status(named);
        assert_eq!(
            status.devices,
            vec![LicensedDevice {
                id: "win".to_owned(),
                name: "Windows PC".to_owned(),
                licensed: true,
            }]
        );
        assert!(license.forget_peers(|id| id != "stranger"));
        assert!(!license.peers.iter().any(|p| p.id == "stranger"));
    }

    #[test]
    fn a_device_that_has_no_key_any_more_leaves_the_list() {
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        license.peer_said("win", K1, true);
        assert!(license.peer_said("win", "", false));
        assert!(license.status(named).devices.is_empty());
        assert!(!license.peer_said("win", "", false));
    }

    #[test]
    fn keys_are_printed_masked() {
        assert_eq!(masked(K1), "INPUTWEAVE-…7E42");
        assert_eq!(masked("ABCDEFGHIJKLMNOP"), "…MNOP");
        // Too short to show the prefix with it.
        assert_eq!(masked("INPUTWEAVE-AB"), "…E-AB");
        assert_eq!(masked("ABCDEFGH"), "…");
        assert_eq!(masked("AB"), "…");
        assert_eq!(masked(""), "");
    }

    #[test]
    fn the_file_round_trips_and_drops_what_it_cannot_hold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("license.toml");
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        license.record(K1, activated("act")).unwrap();
        license.peer_said("win", K1, true);
        license.refused.push(K2.to_owned());
        license.save(&path).unwrap();
        assert_eq!(LicenseFile::load(&path).unwrap(), license);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(
            LicenseFile::load(&dir.path().join("missing.toml")).unwrap(),
            LicenseFile::default()
        );
        let edited = LicenseFile::parse(
            "key = \"has space\"\nstate = \"licensed\"\nopted_out = true\n\
             refused = [\"bad\"]\n\
             [[peers]]\nid = \"win\"\nkey = \"bad key\"\nlicensed = true\n",
        )
        .unwrap();
        assert_eq!(
            edited,
            LicenseFile {
                opted_out: true,
                ..LicenseFile::default()
            }
        );
        // A device entry missing a field, and a time that cannot be when
        // Polar answered, are dropped; the rest of the file is kept.
        let edited = LicenseFile::parse(&format!(
            "key = \"{K1}\"\nstate = \"licensed\"\nactivation_id = \"act\"\n\
             validated_at = 99999999999\n\
             [[peers]]\nid = \"win\"\n\
             [[peers]]\nkey = \"{K1}\"\nlicensed = true\n\
             [[peers]]\nid = \"mac\"\nkey = \"{K1}\"\n"
        ))
        .unwrap();
        assert_eq!(edited.state, LicenseState::Licensed);
        assert_eq!(edited.validated_at, 0);
        assert_eq!(
            edited.peers,
            [PeerLicense {
                id: "mac".to_owned(),
                key: K1.to_owned(),
                licensed: false,
            }]
        );
        let early = LicenseFile::parse(&format!("key = \"{K1}\"\nvalidated_at = 1000\n")).unwrap();
        assert_eq!(early.validated_at, 0);
    }

    #[test]
    fn an_unreadable_file_shows_as_unavailable() {
        let unavailable = shown(None, true, |_| None).unwrap();
        assert_eq!(unavailable.state, LicenseState::Unavailable);
        assert_eq!(unavailable.key, "");
        assert_eq!(shown(None, false, |_| None), None);
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        assert_eq!(
            shown(Some(&license), false, |_| None).unwrap().state,
            LicenseState::PendingActivation
        );
        // Never written.
        let file = LicenseFile {
            state: LicenseState::Unavailable,
            ..license
        };
        let dir = tempfile::tempdir().unwrap();
        assert!(file.save(&dir.path().join("license.toml")).is_err());
    }

    #[test]
    fn a_key_with_no_usable_state_waits_for_activation() {
        for state in ["", "state = \"\"\n", "state = \"from_a_newer_agent\"\n"] {
            let file = LicenseFile::parse(&format!("key = \"{K1}\"\n{state}")).unwrap();
            assert_eq!(file.state, LicenseState::PendingActivation, "{state:?}");
        }
        // Licensed, without the activation to confirm.
        let file = LicenseFile::parse(&format!("key = \"{K1}\"\nstate = \"licensed\"\n")).unwrap();
        assert_eq!(file.state, LicenseState::PendingActivation);
        // A state without a key is none.
        let file = LicenseFile::parse("state = \"licensed\"\n").unwrap();
        assert_eq!(file.state, LicenseState::None);
        let file = LicenseFile::parse("state = \"from_a_newer_agent\"\n").unwrap();
        assert_eq!(file.state, LicenseState::None);
    }

    #[test]
    fn a_state_from_a_newer_agent_with_an_activation_is_licensed() {
        let file = LicenseFile::parse(&format!(
            "key = \"{K1}\"\nstate = \"from_a_newer_agent\"\nactivation_id = \"act\"\n"
        ))
        .unwrap();
        assert_eq!(file.state, LicenseState::Licensed);
        assert_eq!(file.activation_id, "act");
        // A state this build knows keeps its meaning.
        let file = LicenseFile::parse(&format!(
            "key = \"{K1}\"\nstate = \"seats_full\"\nactivation_id = \"act\"\n"
        ))
        .unwrap();
        assert_eq!(file.state, LicenseState::SeatsFull);
    }

    #[test]
    fn a_damaged_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("license.toml");
        assert_eq!(
            LicenseFile::load_or_set_aside(&path),
            Loaded::File(LicenseFile::default())
        );
        std::fs::write(&path, "key = [not toml").unwrap();
        assert_eq!(LicenseFile::load_or_set_aside(&path), Loaded::SetAside);
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("license.toml.bad")).unwrap(),
            "key = [not toml"
        );
        // A value of the wrong type is damage too, and so is what is not
        // text.
        std::fs::write(&path, "validated_at = \"soon\"\n").unwrap();
        assert_eq!(LicenseFile::load_or_set_aside(&path), Loaded::SetAside);
        assert!(!path.exists());
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(LicenseFile::load_or_set_aside(&path), Loaded::SetAside);
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_cannot_be_read_is_left_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("license.toml");
        let text = format!("key = \"{K1}\"\nstate = \"licensed\"\nactivation_id = \"act\"\n");
        std::fs::write(&path, &text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_ok() {
            // Run as root: permissions do not keep it from reading.
            return;
        }
        assert_eq!(LicenseFile::load_or_set_aside(&path), Loaded::Unreadable);
        assert!(!dir.path().join("license.toml.bad").exists());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let Loaded::File(file) = LicenseFile::load_or_set_aside(&path) else {
            panic!("the file is readable again");
        };
        assert_eq!(file.state, LicenseState::Licensed);
    }

    #[test]
    fn the_saver_writes_the_newest_and_finishes_before_it_goes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("license.toml");
        let mut saver = LicenseSaver::new(path.clone());
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        saver.save(license.clone());
        license.record(K1, activated("act")).unwrap();
        saver.save(license.clone());
        saver.finish();
        assert_eq!(LicenseFile::load(&path).unwrap(), license);
    }

    #[cfg(unix)]
    #[test]
    fn the_saver_tries_again_until_the_file_can_be_written() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("config");
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o500)).unwrap();
        let path = folder.join("license.toml");
        if std::fs::write(folder.join("probe"), "").is_ok() {
            // Run as root: permissions do not keep it from writing.
            return;
        }
        let mut saver = LicenseSaver::with_retries(
            path.clone(),
            Duration::from_millis(20),
            Duration::from_millis(100),
        );
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        saver.save(license.clone());
        std::thread::sleep(Duration::from_millis(150));
        assert!(!path.exists());
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(LicenseFile::load(&path).unwrap(), license);
        saver.finish();
    }
}
