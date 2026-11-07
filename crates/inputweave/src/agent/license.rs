//! This computer's license (docs/architecture.md, "License"): every
//! feature works without one; a license bought from Polar shows the
//! computer as licensed, on up to five computers.
//!
//! The agent keeps the record and tells paired devices how it stands; it
//! never talks to Polar. The app asks Polar (activate, validate,
//! deactivate) and reports what Polar answered (`RecordLicenseResult`).
//! The record lives here rather than in the app because the agent is what
//! hears from paired devices, which apply and revoke licenses here, and
//! because on Windows the agent is one per computer while an app runs once
//! per signed-in user, so all of a PC's users share its seat. On macOS and
//! Linux the agent, and so `license.toml`, belong to one user: each user
//! account that activates takes a seat of its own.
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
//!
//! [[peers]]
//! id = "win"
//! key_match = "5d1c…"
//! standing = "licensed"
//! activation_id = "9a7b…"
//!
//! [[owed]]
//! id = "mini"
//! action = "apply"
//! ```
//!
//! Applying and revoking (1.1): a key goes only to the devices a computer
//! holding it applies it to, never by itself. Devices tell each other how
//! their license stands (`LicenseInfo`): the match form of their key
//! ([`key_match`], so that no key is told), whether it waits, is licensed,
//! or was refused, and their activation at Polar, which a device holding
//! the same key can deactivate (Revoke License). What this computer owes a
//! device, applying the license there or revoking it, is kept in `owed`
//! and sent each time that device connects, until what it says shows it
//! done ([`LicenseFile::peer_said`]). A device on InputWeave 1.0 takes the
//! key of any licensed device that tells it: it is told the key only while
//! this computer counts it on the license ([`LicenseFile::keyed`]). It
//! never tells its activation, so it cannot be revoked from here.
//!
//! Polar's answers are matched to the state they answer: an activation only
//! while a key waits for one, a confirmation or an "invalid" from validate
//! only for the activation this computer has, a refusal of an activation
//! only while it waits. Anything else is stale and changes nothing.
//!
//! A `license.toml` that is there but cannot be read (permissions, an I/O
//! error) makes the license unavailable rather than empty: nothing is told
//! to or taken from paired devices, changes are refused
//! ([`LicenseError::Unavailable`]), and the file is never written, until a
//! later read of it succeeds.
//!
//! Files from InputWeave 1.0 read as well: what paired devices said of
//! their keys becomes the match form of those keys, and `opted_out` and
//! `refused`, which kept 1.0 from taking keys from paired devices by
//! itself, are dropped, since nothing is taken by itself any more.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
/// The places on a license: this computer and the devices licensed,
/// activating or waiting for it count.
pub const PLACES: usize = 5;
/// When Polar confirmed a license is never before 2020-01-01, nor more
/// than a day ahead of this computer's clock.
pub const EARLIEST_VALIDATION: u64 = 1_577_836_800;
const VALIDATION_AHEAD: u64 = 86_400;
/// What the match form of a key starts from (peer `LicenseInfo.key_match`).
const KEY_MATCH_PREFIX: &[u8] = b"InputWeave license key\n";
/// What a revoke's proof starts from (peer `LicenseRevoke.proof`).
const REVOKE_PROOF_PREFIX: &[u8] = b"InputWeave license revoke\n";
/// The longest apply id taken from a peer.
pub const MAX_APPLY_ID_LEN: usize = 64;
/// The match form's length in bytes.
pub const KEY_MATCH_LEN: usize = 32;

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
    /// Another device revoked the license here without deactivating this
    /// computer's activation: the app deactivates it, then removes the
    /// license. The key and the activation are kept until then.
    Revoked,
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
        Some("revoked") => LicenseState::Revoked,
        Some(_) => return Err(()),
    })
}

/// Where a paired device's license stands, as it said (peer
/// `LicenseStanding`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    #[default]
    None,
    Pending,
    Licensed,
    SeatsFull,
    Invalid,
}

/// How a device stands on this computer's license, as the License page
/// lists it (app `LicenseUse`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LicenseUse {
    /// On trial there as far as this computer knows, or revoked from here.
    #[default]
    None,
    /// Activated there with this computer's key.
    Licensed,
    /// The key is there, or on its way as the device is connected, and
    /// its app activates it.
    Activating,
    /// Applied from here while the device is offline.
    Waiting,
    /// Polar said every place is taken.
    NoPlaces,
    /// Licensed there with another key, or with any while this computer
    /// has none.
    OtherLicense,
}

impl LicenseUse {
    /// Whether the device takes one of the license's places as this
    /// computer counts them.
    pub fn takes_a_place(self) -> bool {
        matches!(self, Self::Licensed | Self::Activating | Self::Waiting)
    }
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
    #[error("this computer is not licensed")]
    NotLicensed,
    #[error("no such device is trusted here")]
    UnknownDevice,
    #[error("every place on the license is taken")]
    Full,
    #[error("that device is licensed with another key")]
    LicensedElsewhere,
    #[error("that device runs InputWeave 1.0, which cannot be told of a revoke")]
    Legacy,
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
            Self::NotLicensed => "not_licensed",
            Self::UnknownDevice => "unknown_device",
            Self::Full => "license_full",
            Self::LicensedElsewhere => "licensed_elsewhere",
            Self::Legacy => "license_legacy",
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

/// Whether `id` can be an activation id: at most [`MAX_ACTIVATION_ID_LEN`]
/// printable ASCII characters (empty included).
pub fn valid_activation_id(id: &str) -> bool {
    id.len() <= MAX_ACTIVATION_ID_LEN && id.bytes().all(|b| b.is_ascii_graphic())
}

fn valid_email(email: &str) -> bool {
    email.chars().count() <= MAX_EMAIL_LEN && !email.chars().any(char::is_control)
}

/// A SHA-256: a key's match form, or a revoke's proof. Kept as bytes; only
/// `license.toml` writes it, in hex.
pub type KeyHash = [u8; KEY_MATCH_LEN];

/// The match form of `key`: the SHA-256 of [`KEY_MATCH_PREFIX`] and the
/// key. Devices compare keys by it, so no key is told to a device it is
/// not applied to; a key holds a random UUID, so the digest gives it away
/// to no one.
pub fn key_match(key: &str) -> KeyHash {
    let mut hasher = Sha256::new();
    hasher.update(KEY_MATCH_PREFIX);
    hasher.update(key.as_bytes());
    hasher.finalize().into()
}

/// What proves a revoke comes from a device holding `key` and is meant
/// for device `device_id` (peer `LicenseRevoke.proof`): the SHA-256 of
/// [`REVOKE_PROOF_PREFIX`], the key, a newline and the id. A device without
/// the key cannot make it, and it is not the [`key_match`] every paired
/// device hears.
pub fn revoke_proof(key: &str, device_id: &str) -> KeyHash {
    let mut hasher = Sha256::new();
    hasher.update(REVOKE_PROOF_PREFIX);
    hasher.update(key.as_bytes());
    hasher.update(b"\n");
    hasher.update(device_id.as_bytes());
    hasher.finalize().into()
}

/// A new apply's id (peer `LicenseAssign.apply_id`): random, in hex.
pub fn new_apply_id() -> String {
    let bytes = getrandom::u64()
        .unwrap_or_else(|_| {
            // Never seen; unique enough by the clock.
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64)
        })
        .to_le_bytes();
    crate::hex::hex(&bytes)
}

/// Whether `id` can be an apply's id: 1 to [`MAX_APPLY_ID_LEN`] printable
/// ASCII characters.
pub fn valid_apply_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_APPLY_ID_LEN && id.bytes().all(|b| b.is_ascii_graphic())
}

/// A digest from a peer message: `Some(None)` when it is empty, the digest
/// when it is [`KEY_MATCH_LEN`] bytes, and `None` for anything else, which
/// a revoke is ignored for, and a key match alone.
pub fn hash_of(bytes: &[u8]) -> Option<Option<KeyHash>> {
    if bytes.is_empty() {
        return Some(None);
    }
    KeyHash::try_from(bytes).ok().map(Some)
}

/// A digest as `license.toml` writes it: [`KEY_MATCH_LEN`] bytes in hex.
fn hash_from_hex(text: &str) -> Option<KeyHash> {
    crate::hex::from_hex(text).and_then(|bytes| KeyHash::try_from(bytes.as_slice()).ok())
}

/// Whether `at` can be when Polar confirmed a license, by this computer's
/// clock now ([`EARLIEST_VALIDATION`]).
fn plausible_validation(at: u64) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    (EARLIEST_VALIDATION..=now.saturating_add(VALIDATION_AHEAD)).contains(&at)
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

/// What a paired device last said of its license.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeerLicense {
    pub id: String,
    /// The match form of its key ([`key_match`]); none when it has none.
    pub key_match: Option<KeyHash>,
    pub standing: Standing,
    /// Its activation, while it is licensed and said which.
    pub activation_id: String,
    /// It said so as InputWeave 1.0 does, which takes keys from devices
    /// that tell them and never tells its activation.
    pub legacy: bool,
    /// The apply its key came from, while it holds the key.
    pub apply_id: String,
    /// The last apply it did not take.
    pub refused_apply: String,
    /// On InputWeave 1.0, it did not take the key applied from here (its
    /// user had removed the license there, so 1.0 takes no key from other
    /// devices): applying it again waits for it to run 1.1.
    pub legacy_refused: bool,
}

impl PeerLicense {
    /// What a device on InputWeave 1.0 said: its key (empty: none) and
    /// whether it is licensed with it; none for a key that cannot be one.
    pub fn legacy(id: &str, key: &str, licensed: bool) -> Option<Self> {
        if key.is_empty() {
            return Some(Self {
                id: id.to_owned(),
                legacy: true,
                ..Self::default()
            });
        }
        if !valid_key(key) {
            return None;
        }
        Some(Self {
            id: id.to_owned(),
            key_match: Some(key_match(key)),
            standing: if licensed {
                Standing::Licensed
            } else {
                Standing::Pending
            },
            activation_id: String::new(),
            legacy: true,
            ..Self::default()
        })
    }

    /// Whether it holds the key whose match form is `matched`, waiting or
    /// licensed with it.
    fn holds(&self, matched: Option<&KeyHash>) -> bool {
        matched.is_some()
            && self.key_match.as_ref() == matched
            && matches!(self.standing, Standing::Pending | Standing::Licensed)
    }

    /// Whether it is licensed with a key other than the one whose match
    /// form is `matched` (any key, when that is empty), or, on InputWeave
    /// 1.0, holds another key at all: 1.0 takes no key while it has one
    /// that Polar has not refused.
    fn licensed_elsewhere(&self, matched: Option<&KeyHash>) -> bool {
        let other = matched.is_none() || self.key_match.as_ref() != matched;
        other
            && (self.standing == Standing::Licensed
                || (self.legacy && self.standing == Standing::Pending))
    }
}

/// A change to a device's license this computer owes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Apply this computer's license there, as apply `apply_id`. `sent`:
    /// told to it at least once, so it may hold the key.
    Apply { sent: bool, apply_id: String },
    /// Revoke the license there whose key has the match form `key_match`,
    /// naming the activation it revokes, or else the apply: `proof` shows
    /// the device it comes from a holder of the key
    /// ([`revoke_proof`]), and `free_it` that the app could not deactivate
    /// the activation, for the device to do it.
    Revoke {
        key_match: KeyHash,
        proof: KeyHash,
        activation_id: String,
        apply_id: String,
        free_it: bool,
    },
}

/// What this computer owes a device, kept until the device says it took
/// effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owed {
    pub id: String,
    pub action: Action,
}

impl Owed {
    /// Whether `said`, what the device says now, shows it done, for this
    /// computer's key whose match form is `matched`.
    fn done(&self, said: &PeerLicense, matched: Option<&KeyHash>) -> bool {
        match &self.action {
            Action::Apply { .. } => said.holds(matched),
            Action::Revoke {
                key_match,
                activation_id,
                apply_id,
                ..
            } => {
                let named = said.key_match.as_ref() == Some(key_match)
                    && if activation_id.is_empty() {
                        said.apply_id == *apply_id
                            && matches!(
                                said.standing,
                                Standing::Pending
                                    | Standing::Licensed
                                    | Standing::SeatsFull
                                    | Standing::Invalid
                            )
                    } else {
                        said.standing == Standing::Licensed && said.activation_id == *activation_id
                    };
                !named
            }
        }
    }

    /// Whether `said`, what the device says now, shows a revoke done (it
    /// no longer holds what the revoke names); never for an apply.
    pub fn shown_done(&self, said: &PeerLicense) -> bool {
        matches!(self.action, Action::Revoke { .. }) && self.done(said, None)
    }

    /// Whether it can no longer be done: the device did not take the apply,
    /// or is licensed with another key, which an applied license does not
    /// replace.
    fn refused(&self, said: &PeerLicense, matched: Option<&KeyHash>) -> bool {
        match &self.action {
            Action::Apply { apply_id, .. } => {
                (!said.refused_apply.is_empty() && said.refused_apply == *apply_id)
                    || said.licensed_elsewhere(matched)
            }
            Action::Revoke { .. } => false,
        }
    }
}

/// `owed` in the file.
#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct RawOwed {
    id: String,
    action: String,
    #[serde(skip_serializing_if = "is_false")]
    sent: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    key_match: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    proof: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    activation_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    apply_id: String,
    #[serde(skip_serializing_if = "is_false")]
    free_it: bool,
}

impl RawOwed {
    fn of(owed: &Owed) -> Self {
        match &owed.action {
            Action::Apply { sent, apply_id } => Self {
                id: owed.id.clone(),
                action: "apply".to_owned(),
                sent: *sent,
                apply_id: apply_id.clone(),
                ..Self::default()
            },
            Action::Revoke {
                key_match,
                proof,
                activation_id,
                apply_id,
                free_it,
            } => Self {
                id: owed.id.clone(),
                action: "revoke".to_owned(),
                key_match: crate::hex::hex(key_match),
                proof: crate::hex::hex(proof),
                activation_id: activation_id.clone(),
                apply_id: apply_id.clone(),
                free_it: *free_it,
                ..Self::default()
            },
        }
    }

    /// What it says, or none for an entry that cannot be one (with a
    /// warning).
    fn owed(self) -> Option<Owed> {
        let action = match self.action.as_str() {
            _ if self.id.is_empty() => None,
            "apply" if valid_apply_id(&self.apply_id) => Some(Action::Apply {
                sent: self.sent,
                apply_id: self.apply_id,
            }),
            "revoke"
                if valid_activation_id(&self.activation_id)
                    && (!self.activation_id.is_empty() || valid_apply_id(&self.apply_id)) =>
            {
                match (hash_from_hex(&self.key_match), hash_from_hex(&self.proof)) {
                    (Some(key_match), Some(proof)) => Some(Action::Revoke {
                        key_match,
                        proof,
                        activation_id: self.activation_id,
                        apply_id: self.apply_id,
                        free_it: self.free_it,
                    }),
                    _ => None,
                }
            }
            _ => None,
        };
        let Some(action) = action else {
            warn!(
                device = self.id,
                "dropped an entry of the license file that cannot be one"
            );
            return None;
        };
        Some(Owed {
            id: self.id,
            action,
        })
    }
}

/// A paired device's entry in the file: what 1.1 writes, and what 1.0
/// wrote (its key in the clear and whether it was licensed).
#[derive(Default, Deserialize)]
#[serde(default)]
struct RawPeer {
    id: String,
    key_match: String,
    standing: Option<String>,
    activation_id: String,
    legacy: bool,
    apply_id: String,
    refused_apply: String,
    legacy_refused: bool,
    /// InputWeave 1.0.
    key: String,
    licensed: bool,
}

impl RawPeer {
    /// What it says, or none for an entry that cannot be one (with a
    /// warning).
    fn peer(self) -> Option<PeerLicense> {
        if self.id.is_empty() {
            warn!("dropped a device of the license file without an id");
            return None;
        }
        if self.key_match.is_empty() && self.standing.is_none() && !self.key.is_empty() {
            // From InputWeave 1.0, which said so in the clear.
            return PeerLicense::legacy(&self.id, &self.key, self.licensed);
        }
        let key_match = if self.key_match.is_empty() {
            None
        } else if let Some(digest) = hash_from_hex(&self.key_match) {
            Some(digest)
        } else {
            warn!(
                device = self.id,
                "dropped a device of the license file whose key match is not one"
            );
            return None;
        };
        // A standing this build does not know (from a newer agent) is none
        // until the device says again.
        let standing = match (key_match, self.standing.as_deref()) {
            (None, _) => Standing::None,
            (_, Some("pending")) => Standing::Pending,
            (_, Some("licensed")) => Standing::Licensed,
            (_, Some("seats_full")) => Standing::SeatsFull,
            (_, Some("invalid")) => Standing::Invalid,
            _ => Standing::None,
        };
        let mut peer = PeerLicense {
            id: self.id,
            key_match,
            standing,
            activation_id: self.activation_id,
            legacy: self.legacy,
            apply_id: self.apply_id,
            refused_apply: self.refused_apply,
            legacy_refused: self.legacy && self.legacy_refused,
        };
        if peer.standing != Standing::Licensed || !valid_activation_id(&peer.activation_id) {
            peer.activation_id.clear();
        }
        if !valid_apply_id(&peer.apply_id) || peer.key_match.is_none() {
            peer.apply_id.clear();
        }
        if !valid_apply_id(&peer.refused_apply) {
            peer.refused_apply.clear();
        }
        Some(peer)
    }
}

/// A paired device's entry as `license.toml` writes it.
#[derive(Serialize)]
struct WrittenPeer<'a> {
    id: &'a str,
    #[serde(skip_serializing_if = "String::is_empty")]
    key_match: String,
    standing: Standing,
    #[serde(skip_serializing_if = "no_text")]
    activation_id: &'a str,
    #[serde(skip_serializing_if = "is_false")]
    legacy: bool,
    #[serde(skip_serializing_if = "no_text")]
    apply_id: &'a str,
    #[serde(skip_serializing_if = "no_text")]
    refused_apply: &'a str,
    #[serde(skip_serializing_if = "is_false")]
    legacy_refused: bool,
}

impl<'a> WrittenPeer<'a> {
    fn of(peer: &'a PeerLicense) -> Self {
        Self {
            id: &peer.id,
            key_match: peer
                .key_match
                .map(|d| crate::hex::hex(&d))
                .unwrap_or_default(),
            standing: peer.standing,
            activation_id: &peer.activation_id,
            legacy: peer.legacy,
            apply_id: &peer.apply_id,
            refused_apply: &peer.refused_apply,
            legacy_refused: peer.legacy_refused,
        }
    }
}

/// An activation another device revoked here that was not freed when the
/// license here changed to another key: the app frees it with Polar and
/// says so (`LicenseActivationFreed`), whatever the license is meanwhile.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct ToFree {
    pub key: String,
    pub activation_id: String,
}

/// The work the app has to do with Polar that the user did not start here
/// ([`LicenseFile::app_work`]): the key applied from where to activate, or
/// the revoked activation to free, and the activations of earlier keys to
/// free.
#[derive(Clone, PartialEq, Eq)]
pub struct AppWork {
    state: LicenseState,
    source_device_id: String,
    apply_id: String,
    activation_id: String,
    to_free: Vec<String>,
}

/// At most this many activations wait to be freed; older ones are dropped,
/// with a warning (they show in Polar's customer portal).
pub const MAX_TO_FREE: usize = 16;

/// This computer's license, as `license.toml` holds it, what paired devices
/// said of theirs, and what this computer owes them. Read through
/// [`LicenseFile::parse`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LicenseFile {
    pub key: String,
    pub state: LicenseState,
    pub activation_id: String,
    pub customer_email: String,
    pub validated_at: u64,
    /// The paired device that applied the key here (or, under 1.0, that it
    /// was taken from), and its name then; empty when it was entered here.
    pub source_device_id: String,
    pub source_device_name: String,
    /// The apply the key came from (peer `LicenseAssign.apply_id`), which a
    /// revoke may name; empty when it was entered here.
    pub apply_id: String,
    /// The last apply this computer did not take, for its sender; kept,
    /// whatever the license becomes, until another replaces it.
    pub refused_apply: String,
    pub peers: Vec<PeerLicense>,
    /// In the order they are to be sent.
    pub owed: Vec<Owed>,
    /// Activations of earlier keys still taking a place, for the app to
    /// free.
    pub to_free: Vec<ToFree>,
    /// The match form of `key`, kept as the key changes
    /// ([`LicenseFile::rematch`]).
    matched: Option<KeyHash>,
}

/// `license.toml` as written.
#[derive(Serialize)]
struct WrittenFile<'a> {
    #[serde(skip_serializing_if = "no_text")]
    key: &'a str,
    state: LicenseState,
    #[serde(skip_serializing_if = "no_text")]
    activation_id: &'a str,
    #[serde(skip_serializing_if = "no_text")]
    customer_email: &'a str,
    #[serde(skip_serializing_if = "is_zero")]
    validated_at: u64,
    #[serde(skip_serializing_if = "no_text")]
    source_device_id: &'a str,
    #[serde(skip_serializing_if = "no_text")]
    source_device_name: &'a str,
    #[serde(skip_serializing_if = "no_text")]
    apply_id: &'a str,
    #[serde(skip_serializing_if = "no_text")]
    refused_apply: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    peers: Vec<WrittenPeer<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    owed: Vec<RawOwed>,
    #[serde(skip_serializing_if = "no_to_free")]
    to_free: &'a [ToFree],
}

/// `license.toml` as read, its state by name, before
/// [`LicenseFile::parse`] makes sense of it. `opted_out` and `refused`,
/// which 1.0 wrote, are left unread.
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
    apply_id: String,
    refused_apply: String,
    peers: Vec<RawPeer>,
    owed: Vec<RawOwed>,
    to_free: Vec<ToFree>,
}

fn no_text(text: &&str) -> bool {
    text.is_empty()
}

fn no_to_free(to_free: &&[ToFree]) -> bool {
    to_free.is_empty()
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A trusted device as the License page lists it: one with something to
/// show (its use is not [`LicenseUse::None`]). Its `Debug`, which the
/// diagnostics report and logs use, leaves out the activation id.
#[derive(Clone, PartialEq, Eq)]
pub struct LicensedDevice {
    pub id: String,
    pub name: String,
    pub licensed_use: LicenseUse,
    /// Its activation while it is licensed with this computer's key and
    /// said which.
    pub activation_id: String,
    /// It last spoke as InputWeave 1.0.
    pub legacy: bool,
    /// What was applied there can be taken back (Don't Apply).
    pub can_take_back: bool,
    /// It is offline and may hold the key from an apply: taking it back
    /// reaches it only once it connects, so not if it is removed now.
    pub key_out_of_reach: bool,
    /// On InputWeave 1.0, it did not take the license applied from here
    /// ([`PeerLicense::legacy_refused`]); shown on trial.
    pub legacy_refused: bool,
    /// Waiting ([`LicenseUse::Waiting`]) for an apply from here that it has
    /// not shown yet, to go as it connects or as this computer is licensed
    /// again: what the License page lists under Waiting to Apply while this
    /// computer is not licensed. Not for a device waiting with the key it
    /// already holds (entered there, or an apply it took).
    pub waiting_to_apply: bool,
}

impl std::fmt::Debug for LicensedDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LicensedDevice")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("use", &self.licensed_use)
            .field("legacy", &self.legacy)
            .field("can_take_back", &self.can_take_back)
            .field("key_out_of_reach", &self.key_out_of_reach)
            .field("legacy_refused", &self.legacy_refused)
            .field("waiting_to_apply", &self.waiting_to_apply)
            .finish_non_exhaustive()
    }
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
    pub devices: Vec<LicensedDevice>,
    /// Activations of earlier keys for the app to free ([`ToFree`]).
    pub to_free: Vec<ToFree>,
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
            .field("devices", &self.devices)
            .field("to_free", &self.to_free.len())
            .finish_non_exhaustive()
    }
}

/// What reading `license.toml` came to, as the agent starts.
#[derive(Debug, PartialEq, Eq)]
pub enum Loaded {
    /// Read (or missing: no license).
    File(Box<LicenseFile>),
    /// Damaged, and moved aside: no license, saved as usual.
    SetAside,
    /// There, but it could not be read (permissions): the license is
    /// unavailable, and nothing is saved over it, until a later read
    /// succeeds.
    Unreadable,
}

/// The license as the status shows it: unavailable while its file cannot
/// be read, none where the agent keeps no license, otherwise `license`
/// with devices named as `name_of` does (the trusted ones) and connected
/// as `connected` says.
pub(crate) fn shown(
    license: Option<&LicenseFile>,
    unavailable: bool,
    name_of: impl Fn(&str) -> Option<String>,
    connected: impl Fn(&str) -> bool,
) -> Option<LicenseStatus> {
    if unavailable {
        return Some(LicenseStatus {
            state: LicenseState::Unavailable,
            ..LicenseStatus::default()
        });
    }
    license.map(|l| l.status(name_of, connected))
}

/// What this computer tells paired devices of its license (peer
/// `LicenseInfo`), without its key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Told {
    pub key_match: Option<KeyHash>,
    pub standing: Standing,
    pub activation_id: String,
    pub apply_id: String,
    pub refused_apply: String,
}

/// What a device sent this computer applies or revokes the license.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    Assign {
        key: String,
        apply_id: String,
    },
    Revoke {
        proof: KeyHash,
        activation_id: String,
        apply_id: String,
        free_it: bool,
    },
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
            Ok(file) => Loaded::File(Box::new(file)),
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
        let mut peers: Vec<PeerLicense> = Vec::new();
        for peer in raw.peers.into_iter().filter_map(RawPeer::peer) {
            if !peers.iter().any(|p| p.id == peer.id) {
                peers.push(peer);
            }
        }
        peers.sort_by(|a, b| a.id.cmp(&b.id));
        let mut file = Self {
            key: raw.key,
            state: known.unwrap_or(LicenseState::None),
            activation_id: raw.activation_id,
            customer_email: raw.customer_email,
            validated_at: raw.validated_at,
            source_device_id: raw.source_device_id,
            source_device_name: raw.source_device_name,
            apply_id: raw.apply_id,
            refused_apply: raw.refused_apply,
            peers,
            ..Self::default()
        };
        if !file.key.is_empty() && !valid_key(&file.key) {
            file = Self {
                refused_apply: file.refused_apply,
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
        if !valid_apply_id(&file.apply_id) {
            file.apply_id.clear();
        }
        if !valid_apply_id(&file.refused_apply) {
            file.refused_apply.clear();
        }
        if known.is_err() && !file.activation_id.is_empty() {
            file.state = LicenseState::Licensed;
        }
        if file.key.is_empty()
            || (file.state == LicenseState::Revoked && file.activation_id.is_empty())
        {
            // Nothing to free without an activation.
            file = Self {
                refused_apply: std::mem::take(&mut file.refused_apply),
                peers: std::mem::take(&mut file.peers),
                ..Self::default()
            };
        } else if file.state == LicenseState::None
            || (file.state == LicenseState::Licensed && file.activation_id.is_empty())
        {
            // A key with no state, or licensed without the activation to
            // confirm: the app activates it again.
            file.state = LicenseState::PendingActivation;
            file.activation_id.clear();
        }
        for owed in raw.owed.into_iter().filter_map(RawOwed::owed) {
            file.owe(owed);
        }
        for to_free in raw.to_free {
            if valid_key(&to_free.key)
                && !to_free.activation_id.is_empty()
                && valid_activation_id(&to_free.activation_id)
            {
                file.free_later(to_free);
            } else {
                warn!("dropped an activation to free from the license file that cannot be one");
            }
        }
        file.rematch();
        if file.key.is_empty() {
            file.owed
                .retain(|o| matches!(o.action, Action::Revoke { .. }));
        }
        Ok(file)
    }

    /// Replaces the file in one step, readable only by its owner.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let written = WrittenFile {
            key: &self.key,
            state: self.state,
            activation_id: &self.activation_id,
            customer_email: &self.customer_email,
            validated_at: self.validated_at,
            source_device_id: &self.source_device_id,
            source_device_name: &self.source_device_name,
            apply_id: &self.apply_id,
            refused_apply: &self.refused_apply,
            peers: self.peers.iter().map(WrittenPeer::of).collect(),
            owed: self.owed.iter().map(RawOwed::of).collect(),
            to_free: &self.to_free,
        };
        let text = toml::to_string(&written).map_err(io::Error::other)?;
        crate::store::write_private(path, HEADER, &text)
    }

    /// Whether the app has work to do with Polar that the user did not
    /// start here: a key another device applied waits to be activated, or
    /// an activation another device revoked waits to be freed (also one of
    /// an earlier key). The agent opens the app for it.
    pub fn needs_app(&self) -> bool {
        !self.to_free.is_empty()
            || match self.state {
                LicenseState::PendingActivation => !self.source_device_id.is_empty(),
                LicenseState::Revoked => true,
                _ => false,
            }
    }

    /// The work [`Self::needs_app`] stands for, to tell new work from the
    /// same; none while there is none.
    pub fn app_work(&self) -> Option<AppWork> {
        self.needs_app().then(|| AppWork {
            state: self.state,
            source_device_id: self.source_device_id.clone(),
            apply_id: self.apply_id.clone(),
            activation_id: self.activation_id.clone(),
            to_free: self
                .to_free
                .iter()
                .map(|t| t.activation_id.clone())
                .collect(),
        })
    }

    /// The match form of this computer's key; empty when it has none.
    pub fn own_match(&self) -> Option<KeyHash> {
        self.matched
    }

    /// Works the match form of the key out again, as the key changed.
    fn rematch(&mut self) {
        self.matched = (!self.key.is_empty()).then(|| key_match(&self.key));
    }

    /// What this computer tells paired devices of its license: the match
    /// form of its key (none while a revoked activation waits to be freed),
    /// how it stands, its activation while licensed, the apply the key came
    /// from, and the last apply it did not take.
    pub fn told(&self) -> Told {
        let standing = match self.state {
            LicenseState::None | LicenseState::Revoked | LicenseState::Unavailable => {
                Standing::None
            }
            LicenseState::PendingActivation => Standing::Pending,
            LicenseState::Licensed => Standing::Licensed,
            LicenseState::SeatsFull => Standing::SeatsFull,
            LicenseState::Invalid => Standing::Invalid,
        };
        let holds = standing != Standing::None;
        Told {
            key_match: if holds { self.matched } else { None },
            standing,
            activation_id: if standing == Standing::Licensed {
                self.activation_id.clone()
            } else {
                String::new()
            },
            apply_id: if holds {
                self.apply_id.clone()
            } else {
                String::new()
            },
            refused_apply: self.refused_apply.clone(),
        }
    }

    /// Whether a device on InputWeave 1.0 (`id`) is told this computer's
    /// key: while this computer is licensed and counts the device on its
    /// license, as one it applies the license to, or one that says it has
    /// the key, unless it revoked it there.
    pub fn keyed(&self, id: &str) -> bool {
        if self.state != LicenseState::Licensed {
            return false;
        }
        let owes = |apply: bool| {
            self.owed
                .iter()
                .any(|o| o.id == id && matches!(o.action, Action::Apply { .. }) == apply)
        };
        if owes(false) {
            return false;
        }
        owes(true)
            || self
                .peer(id)
                .is_some_and(|p| self.matched.is_some() && p.key_match == self.matched)
    }

    fn peer(&self, id: &str) -> Option<&PeerLicense> {
        self.peers.iter().find(|p| p.id == id)
    }

    /// The user entered `key` here (or asked to try it again). It must be
    /// [`valid_key`] (trimmed). The key this computer already has,
    /// licensed or waiting, keeps its record, so its activation is not lost
    /// and no second seat taken, though it now counts as entered here; one
    /// another device revoked here starts over, its activation left to
    /// free; a refused one starts over, for the app to activate again.
    /// Applying the license to other devices that has not reached them yet
    /// is dropped with another key.
    pub fn set_key(&mut self, key: &str) -> Result<(), LicenseError> {
        let key = key.trim();
        if !valid_key(key) {
            return Err(LicenseError::InvalidKey);
        }
        if key == self.key
            && matches!(
                self.state,
                LicenseState::Licensed | LicenseState::PendingActivation
            )
        {
            self.source_device_id.clear();
            self.source_device_name.clear();
            self.apply_id.clear();
            return Ok(());
        }
        if key != self.key {
            self.drop_applies();
        }
        self.leave_revoked();
        self.restart(key, "", "", "");
        Ok(())
    }

    /// A revoked activation is never licensed again, the app being about
    /// to deactivate it: leaving it, for whatever key, it is noted to free
    /// ([`ToFree`]).
    fn leave_revoked(&mut self) {
        if self.state == LicenseState::Revoked && !self.activation_id.is_empty() {
            self.free_later(ToFree {
                key: self.key.clone(),
                activation_id: self.activation_id.clone(),
            });
        }
    }

    /// `key`, waiting for activation, from `source` as apply `apply_id`
    /// (none: entered here).
    fn restart(&mut self, key: &str, source_id: &str, source_name: &str, apply_id: &str) {
        *self = Self {
            key: key.to_owned(),
            state: LicenseState::PendingActivation,
            source_device_id: source_id.to_owned(),
            source_device_name: source_name.to_owned(),
            apply_id: apply_id.to_owned(),
            refused_apply: std::mem::take(&mut self.refused_apply),
            peers: std::mem::take(&mut self.peers),
            owed: std::mem::take(&mut self.owed),
            to_free: std::mem::take(&mut self.to_free),
            ..Self::default()
        };
        self.rematch();
    }

    /// Forgets this computer's key and activation, keeping what devices
    /// said and the revokes owed them; applying the license that has not
    /// reached a device yet goes with the key.
    fn clear(&mut self) {
        *self = Self {
            refused_apply: std::mem::take(&mut self.refused_apply),
            peers: std::mem::take(&mut self.peers),
            owed: std::mem::take(&mut self.owed),
            to_free: std::mem::take(&mut self.to_free),
            ..Self::default()
        };
        self.drop_applies();
    }

    /// Notes an activation to free whatever the license here is
    /// ([`ToFree`]); the oldest are dropped past [`MAX_TO_FREE`].
    fn free_later(&mut self, to_free: ToFree) {
        if self
            .to_free
            .iter()
            .any(|t| t.activation_id == to_free.activation_id)
        {
            return;
        }
        self.to_free.push(to_free);
        let extra = self.to_free.len().saturating_sub(MAX_TO_FREE);
        if extra > 0 {
            warn!(
                extra,
                "too many activations to free; the oldest are dropped"
            );
            self.to_free.drain(..extra);
        }
    }

    /// The app freed `activation_id` with Polar (`LicenseActivationFreed`).
    /// Returns whether it was one to free.
    pub fn freed(&mut self, activation_id: &str) -> bool {
        let before = self.to_free.len();
        self.to_free.retain(|t| t.activation_id != activation_id);
        self.to_free.len() != before
    }

    fn drop_applies(&mut self) {
        self.owed
            .retain(|o| matches!(o.action, Action::Revoke { .. }));
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
            }
        }
        Ok(())
    }

    /// The user removed the license here, or the app freed the activation
    /// another device revoked here. Other devices keep theirs.
    pub fn remove(&mut self) {
        self.clear();
    }

    /// Like [`Self::remove`], only while the license is still `key` with
    /// activation `activation_id` (empty: none), as the app saw it when it
    /// asked; otherwise [`LicenseError::Stale`], so a removal that waited
    /// never wipes a newer license. An empty `key` removes whatever is here
    /// (`ctl`, apps before 1.1).
    pub fn remove_if(&mut self, key: &str, activation_id: &str) -> Result<(), LicenseError> {
        if !key.is_empty() && (key != self.key || activation_id != self.activation_id) {
            return Err(LicenseError::Stale);
        }
        self.remove();
        Ok(())
    }

    /// Device `said.id` said how its license stands. What this computer
    /// owed it that this shows done, or that can no longer be done, is
    /// dropped. Returns whether anything changed.
    pub fn peer_said(&mut self, mut said: PeerLicense) -> bool {
        let matched = self.matched;
        let id = said.id.clone();
        // Each in turn: an apply after a revoke is done only once the
        // revoke is, so that both go out.
        let mut revoking = false;
        let mut owed_changed = false;
        self.owed.retain(|o| {
            if o.id != id {
                return true;
            }
            let done = match o.action {
                Action::Revoke { .. } => o.done(&said, matched.as_ref()),
                Action::Apply { .. } => !revoking && o.done(&said, matched.as_ref()),
            };
            let keep = !done && !o.refused(&said, matched.as_ref());
            revoking |= keep && matches!(o.action, Action::Revoke { .. });
            owed_changed |= !keep;
            keep
        });
        // Not taking the key on 1.0 lasts until it holds the key, or the
        // license is applied to it again (`refuse_legacy_apply`).
        let was_refused = self.peer(&id).is_some_and(|p| p.legacy_refused);
        said.legacy_refused = said.legacy && !said.holds(matched.as_ref()) && was_refused;
        let peer_changed = match self.peers.binary_search_by(|p| p.id.cmp(&id)) {
            Ok(at) if self.peers[at] == said => false,
            Ok(at) => {
                self.peers[at] = said;
                true
            }
            Err(at) => {
                self.peers.insert(at, said);
                true
            }
        };
        peer_changed || owed_changed
    }

    /// Forgets what devices that are no longer trusted said, and what was
    /// owed them. Returns whether any was forgotten.
    pub fn forget_peers(&mut self, trusted: impl Fn(&str) -> bool) -> bool {
        let before = (self.peers.len(), self.owed.len());
        self.peers.retain(|p| trusted(&p.id));
        self.owed.retain(|o| trusted(&o.id));
        (self.peers.len(), self.owed.len()) != before
    }

    /// Notes `owed`, after what is owed already: one apply and one revoke
    /// a device at most, a revoke in place of the earlier one.
    fn owe(&mut self, owed: Owed) {
        let apply = matches!(owed.action, Action::Apply { .. });
        if apply
            && self
                .owed
                .iter()
                .any(|o| o.id == owed.id && matches!(o.action, Action::Apply { .. }))
        {
            return;
        }
        if !apply {
            self.owed
                .retain(|o| o.id != owed.id || matches!(o.action, Action::Apply { .. }));
        }
        self.owed.push(owed);
    }

    /// Why applying this computer's license to device `id` is refused, as
    /// far as this computer and that device go (not licensed here, licensed
    /// with another key there); the engine checks the places after.
    pub fn refuses_apply(&self, id: &str) -> Option<LicenseError> {
        if self.state != LicenseState::Licensed {
            return Some(LicenseError::NotLicensed);
        }
        self.peer(id)
            .filter(|said| said.licensed_elsewhere(self.matched.as_ref()))
            .map(|_| LicenseError::LicensedElsewhere)
    }

    /// The user applied this computer's license to device `id` (Apply
    /// License), which the engine checked is trusted and has a place.
    /// Nothing changes for a device that already waits for it or holds it.
    pub fn apply(&mut self, id: &str) -> Result<(), LicenseError> {
        if let Some(refusal) = self.refuses_apply(id) {
            return Err(refusal);
        }
        let matched = self.matched;
        let revoking = self
            .owed
            .iter()
            .any(|o| o.id == id && matches!(o.action, Action::Revoke { .. }));
        if let Some(said) = self.peer(id)
            && said.holds(matched.as_ref())
            && !revoking
        {
            return Ok(());
        }
        // Applied again: a 1.0 device that did not take it before is asked
        // again.
        if let Some(said) = self.peers.iter_mut().find(|p| p.id == id) {
            said.legacy_refused = false;
        }
        self.owe(Owed {
            id: id.to_owned(),
            action: Action::Apply {
                sent: false,
                apply_id: new_apply_id(),
            },
        });
        Ok(())
    }

    /// The user revoked this computer's license on device `id` (Revoke
    /// License), naming its activation `activation_id`, which the app
    /// deactivated at Polar or not (`deactivated`); or, with no activation,
    /// took back what was applied there (Don't Apply). An apply that never
    /// reached the device is just dropped; one that may have, or the key
    /// the device waits to activate from an apply, is revoked by naming the
    /// apply. Nothing is revoked on InputWeave 1.0, which cannot be told,
    /// nor a key the device's own user entered there.
    pub fn revoke(
        &mut self,
        id: &str,
        activation_id: &str,
        deactivated: bool,
    ) -> Result<(), LicenseError> {
        if self.key.is_empty() {
            return Err(LicenseError::NotLicensed);
        }
        if !valid_activation_id(activation_id) {
            return Err(LicenseError::InvalidResult);
        }
        let Some(matched) = self.matched else {
            return Err(LicenseError::NotLicensed);
        };
        let said = self.peer(id).cloned();
        let legacy = said.as_ref().is_some_and(|p| p.legacy);
        let licensed_as = said.as_ref().is_some_and(|p| {
            p.key_match == Some(matched)
                && p.standing == Standing::Licensed
                && !activation_id.is_empty()
                && p.activation_id == activation_id
        });
        if !activation_id.is_empty() && !licensed_as {
            return Err(LicenseError::Stale);
        }
        let sent = self.owed.iter().find_map(|o| match &o.action {
            Action::Apply {
                sent: was,
                apply_id,
                ..
            } if o.id == id => Some((*was, apply_id.clone())),
            _ => None,
        });
        let holds = said.as_ref().is_some_and(|p| p.holds(Some(&matched)));
        // InputWeave 1.0 that holds the key, or heard it, cannot give it
        // back.
        if legacy && (holds || sent.as_ref().is_some_and(|(was, _)| *was)) {
            return Err(LicenseError::Legacy);
        }
        self.owed
            .retain(|o| o.id != id || !matches!(o.action, Action::Apply { .. }));
        if legacy {
            return Ok(());
        }
        let (activation_id, apply_id, free_it) = if licensed_as {
            (activation_id.to_owned(), String::new(), !deactivated)
        } else {
            // The apply the device holds the key from, as it says, or else
            // the one sent from here that it may hold it from.
            let told = said
                .as_ref()
                .filter(|p| holds && !p.apply_id.is_empty())
                .map(|p| p.apply_id.clone());
            let ours = sent.and_then(|(was, apply_id)| was.then_some(apply_id));
            match told.or(ours) {
                Some(apply_id) => (String::new(), apply_id, true),
                None => return Ok(()),
            }
        };
        self.owe(Owed {
            id: id.to_owned(),
            action: Action::Revoke {
                key_match: matched,
                proof: revoke_proof(&self.key, id),
                activation_id,
                apply_id,
                free_it,
            },
        });
        Ok(())
    }

    /// What is owed device `id`, to send it while it is connected on a
    /// protocol that knows applying and revoking: revokes, and applies
    /// while this computer is licensed, in turn.
    pub fn deliveries(&self, id: &str) -> Vec<Delivery> {
        let licensed = self.state == LicenseState::Licensed;
        self.owed
            .iter()
            .filter(|o| o.id == id)
            .filter_map(|o| match &o.action {
                Action::Revoke {
                    proof,
                    activation_id,
                    apply_id,
                    free_it,
                    ..
                } => Some(Delivery::Revoke {
                    proof: *proof,
                    activation_id: activation_id.clone(),
                    apply_id: apply_id.clone(),
                    free_it: *free_it,
                }),
                Action::Apply { apply_id, .. } => licensed.then(|| Delivery::Assign {
                    key: self.key.clone(),
                    apply_id: apply_id.clone(),
                }),
            })
            .collect()
    }

    /// Notes that applying the license reached device `id`: the key went
    /// to it, as a delivery or, on InputWeave 1.0, in what it is told. It
    /// may hold the key from now on.
    pub fn mark_sent(&mut self, id: &str) {
        for owed in self.owed.iter_mut().filter(|o| o.id == id) {
            if let Action::Apply { sent, .. } = &mut owed.action {
                *sent = true;
            }
        }
    }

    /// Whether device `id` (on InputWeave 1.0, as the engine knows) was
    /// told the key for an apply from here and, as it last said, does not
    /// hold it yet.
    pub fn awaits_legacy_take(&self, id: &str) -> bool {
        self.owed
            .iter()
            .any(|o| o.id == id && matches!(o.action, Action::Apply { sent: true, .. }))
            && !self
                .peer(id)
                .is_some_and(|p| p.holds(self.matched.as_ref()))
    }

    /// Device `id`, on InputWeave 1.0, did not say it holds the key in the
    /// time it was given after it was told it in a session (the engine
    /// keeps that time): 1.0 tells its license at once as it takes a key,
    /// so it did not take it, its user having removed the license there,
    /// after which 1.0 takes no key. The apply is dropped and the device
    /// listed on trial, said so ([`PeerLicense::legacy_refused`]). Returns
    /// whether anything changed.
    pub fn refuse_legacy_apply(&mut self, id: &str) -> bool {
        if !self.awaits_legacy_take(id) {
            return false;
        }
        self.owed
            .retain(|o| o.id != id || !matches!(o.action, Action::Apply { .. }));
        match self.peers.binary_search_by(|p| p.id.as_str().cmp(id)) {
            Ok(at) => self.peers[at].legacy_refused = true,
            Err(at) => {
                if let Some(mut said) = PeerLicense::legacy(id, "", false) {
                    said.legacy_refused = true;
                    self.peers.insert(at, said);
                }
            }
        }
        true
    }

    /// Device `from_id`, named `from_name`, applied `key` here as apply
    /// `apply_id` (peer `LicenseAssign`). Taken unless this computer is
    /// licensed with another key, waits to activate another key its own
    /// user entered, or another device revoked a license here whose
    /// activation the app has yet to free (the apply is then noted as
    /// refused, for the sender); the same key, licensed or waiting, changes
    /// nothing, and the same key revoked here starts over, the revoked
    /// activation left to free. Returns whether anything changed.
    pub fn assigned(&mut self, from_id: &str, from_name: &str, key: &str, apply_id: &str) -> bool {
        if !valid_key(key) || !valid_apply_id(apply_id) {
            return false;
        }
        let same = key == self.key;
        match self.state {
            LicenseState::Licensed | LicenseState::PendingActivation if same => false,
            // Never the revoked activation again: it is left to free, and
            // the key waits for a new one.
            LicenseState::Revoked if same => {
                self.leave_revoked();
                self.restart(key, from_id, from_name, apply_id);
                true
            }
            // A key entered here waiting to be activated is its user's: an
            // apply never replaces it.
            LicenseState::Licensed | LicenseState::Revoked | LicenseState::Unavailable => {
                self.refuse_apply(apply_id)
            }
            LicenseState::PendingActivation if self.source_device_id.is_empty() => {
                self.refuse_apply(apply_id)
            }
            LicenseState::None
            | LicenseState::PendingActivation
            | LicenseState::SeatsFull
            | LicenseState::Invalid => {
                if !same {
                    self.drop_applies();
                }
                self.restart(key, from_id, from_name, apply_id);
                true
            }
        }
    }

    /// Notes apply `apply_id` as refused here, which this computer tells
    /// the sender (`LicenseInfo.refused_apply_id`). Returns whether that is
    /// new.
    fn refuse_apply(&mut self, apply_id: &str) -> bool {
        if self.refused_apply == apply_id {
            return false;
        }
        self.refused_apply = apply_id.to_owned();
        true
    }

    /// A device revoked the license here (peer `LicenseRevoke`), proving
    /// with `proof` that it holds this computer's key and means this
    /// computer (`local_id`), and naming the activation it revokes, or the
    /// apply. Only what is still this computer's is revoked: a revoke that
    /// waited for this computer to connect never undoes a license applied
    /// here since. Returns whether anything changed.
    pub fn revoked(
        &mut self,
        local_id: &str,
        proof: &KeyHash,
        activation_id: &str,
        apply_id: &str,
        free_it: bool,
    ) -> bool {
        if self.key.is_empty() || revoke_proof(&self.key, local_id) != *proof {
            return false;
        }
        // Still taking its place: the app frees it, then removes the
        // license.
        let to_free = |file: &mut Self| {
            file.state = LicenseState::Revoked;
            file.source_device_id.clear();
            file.source_device_name.clear();
            true
        };
        if !activation_id.is_empty() {
            if self.state != LicenseState::Licensed || self.activation_id != activation_id {
                return false;
            }
            if free_it {
                return to_free(self);
            }
            self.clear();
            return true;
        }
        if apply_id.is_empty() || self.apply_id != apply_id {
            return false;
        }
        match self.state {
            // Activated from that apply meanwhile, which the device that
            // revoked it could not deactivate.
            LicenseState::Licensed => to_free(self),
            LicenseState::PendingActivation | LicenseState::SeatsFull | LicenseState::Invalid => {
                self.clear();
                true
            }
            LicenseState::None | LicenseState::Revoked | LicenseState::Unavailable => false,
        }
    }

    /// Whether what was applied to device `id` can be taken back (Don't
    /// Apply): an apply is owed it, or it waits to activate the key from an
    /// apply. Not on InputWeave 1.0 once it holds the key.
    pub fn can_take_back(&self, id: &str) -> bool {
        let legacy = self.peer(id).is_some_and(|p| p.legacy);
        let applying = self.owed.iter().any(|o| {
            o.id == id
                && match o.action {
                    // 1.0 that heard the key cannot give it back.
                    Action::Apply { sent, .. } => !(legacy && sent),
                    Action::Revoke { .. } => false,
                }
        });
        applying
            || self.peer(id).is_some_and(|p| {
                !p.legacy
                    && self.matched.is_some()
                    && p.key_match == self.matched
                    && p.standing == Standing::Pending
                    && !p.apply_id.is_empty()
            })
    }

    /// How device `id` stands on this computer's license, connected or not
    /// (taken offline for now counts as connected: its session stays up).
    pub fn use_of(&self, id: &str, connected: bool) -> LicenseUse {
        let mut applying = false;
        let mut revoking = false;
        for owed in self.owed.iter().filter(|o| o.id == id) {
            match owed.action {
                Action::Apply { .. } => applying = true,
                Action::Revoke { .. } => revoking = true,
            }
        }
        if applying {
            // Not sent while this computer is not licensed: waiting, as
            // for an offline device.
            return if connected && self.state == LicenseState::Licensed {
                LicenseUse::Activating
            } else {
                LicenseUse::Waiting
            };
        }
        if revoking {
            return LicenseUse::None;
        }
        let Some(said) = self.peer(id) else {
            return LicenseUse::None;
        };
        let matched = self.matched.as_ref();
        if matched.is_some() && said.key_match.as_ref() == matched {
            match said.standing {
                Standing::Licensed => LicenseUse::Licensed,
                // Offline, it activates only once it is back: it waits.
                Standing::Pending if connected => LicenseUse::Activating,
                Standing::Pending => LicenseUse::Waiting,
                Standing::SeatsFull => LicenseUse::NoPlaces,
                Standing::None | Standing::Invalid => LicenseUse::None,
            }
        } else if said.licensed_elsewhere(matched) {
            LicenseUse::OtherLicense
        } else {
            LicenseUse::None
        }
    }

    /// The places taken on this computer's license as it counts them: its
    /// own, and those of the trusted devices (`trusted`) licensed,
    /// activating or waiting for it.
    pub fn places_taken<'a>(
        &self,
        trusted: impl IntoIterator<Item = &'a str>,
        connected: impl Fn(&str) -> bool,
    ) -> usize {
        usize::from(self.state == LicenseState::Licensed)
            + trusted
                .into_iter()
                .filter(|id| self.use_of(id, connected(id)).takes_a_place())
                .count()
    }

    /// The license as the status shows it, naming devices as `name_of`
    /// does; devices it does not name (no longer trusted) are left out.
    pub fn status(
        &self,
        name_of: impl Fn(&str) -> Option<String>,
        connected: impl Fn(&str) -> bool,
    ) -> LicenseStatus {
        let mut ids: Vec<&str> = self
            .peers
            .iter()
            .map(|p| p.id.as_str())
            .chain(self.owed.iter().map(|o| o.id.as_str()))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        let devices = ids
            .into_iter()
            .filter_map(|id| {
                let licensed_use = self.use_of(id, connected(id));
                let said = self.peer(id);
                let legacy_refused =
                    licensed_use == LicenseUse::None && said.is_some_and(|p| p.legacy_refused);
                if licensed_use == LicenseUse::None && !legacy_refused {
                    return None;
                }
                name_of(id).map(|name| LicensedDevice {
                    id: id.to_owned(),
                    name,
                    licensed_use,
                    activation_id: if licensed_use == LicenseUse::Licensed {
                        said.map(|p| p.activation_id.clone()).unwrap_or_default()
                    } else {
                        String::new()
                    },
                    legacy: said.is_some_and(|p| p.legacy),
                    can_take_back: matches!(
                        licensed_use,
                        LicenseUse::Activating | LicenseUse::Waiting
                    ) && self.can_take_back(id),
                    key_out_of_reach: licensed_use == LicenseUse::Waiting && self.key_may_be_at(id),
                    legacy_refused,
                    waiting_to_apply: licensed_use == LicenseUse::Waiting
                        && self
                            .owed
                            .iter()
                            .any(|o| o.id == id && matches!(o.action, Action::Apply { .. })),
                })
            })
            .collect();
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
            devices,
            to_free: self.to_free.clone(),
        }
    }

    /// Whether device `id` may hold this computer's key from an apply: one
    /// was sent to it, or it says it waits to activate the key.
    fn key_may_be_at(&self, id: &str) -> bool {
        let sent = self
            .owed
            .iter()
            .any(|o| o.id == id && matches!(o.action, Action::Apply { sent: true, .. }));
        sent || self.peer(id).is_some_and(|p| {
            !p.legacy
                && self.matched.is_some()
                && p.key_match == self.matched
                && p.standing == Standing::Pending
        })
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

    /// Licensed with `key`, activation `act`.
    fn licensed(key: &str, act: &str) -> LicenseFile {
        let mut license = LicenseFile::default();
        license.set_key(key).unwrap();
        license.record(key, activated(act)).unwrap();
        license
    }

    /// What a device on 1.1 says of its license.
    fn said(id: &str, key: &str, standing: Standing, act: &str) -> PeerLicense {
        PeerLicense {
            id: id.to_owned(),
            key_match: (!key.is_empty()).then(|| key_match(key)),
            standing,
            activation_id: act.to_owned(),
            ..PeerLicense::default()
        }
    }

    fn none(id: &str) -> PeerLicense {
        said(id, "", Standing::None, "")
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
    fn the_match_form_of_a_key_is_its_prefixed_sha256() {
        // printf 'InputWeave license key\n%s' "$K1" | shasum -a 256
        let mut hasher = Sha256::new();
        hasher.update(b"InputWeave license key\n");
        hasher.update(K1.as_bytes());
        let expected: KeyHash = hasher.finalize().into();
        assert_eq!(key_match(K1), expected);
        assert_ne!(key_match(K1), key_match(K2));
        assert!(!crate::hex::hex(&key_match(K1)).contains("7f3a2c1e"));
        assert_eq!(hash_of(&key_match(K1)), Some(Some(key_match(K1))));
        assert_eq!(hash_of(&[]), Some(None));
        assert_eq!(hash_of(&[1, 2, 3]), None);
        let hex = crate::hex::hex(&key_match(K1));
        assert_eq!(hash_from_hex(&hex), Some(key_match(K1)));
        assert_eq!(hash_from_hex(&hex[2..]), None);
        assert_eq!(hash_from_hex("zz"), None);
    }

    #[test]
    fn entering_a_key_trims_it_and_waits_for_activation() {
        let mut license = LicenseFile::default();
        license.set_key(&format!("  {K1}\n")).unwrap();
        assert_eq!(license.key, K1);
        assert_eq!(license.state, LicenseState::PendingActivation);
        assert_eq!(license.set_key("not a key"), Err(LicenseError::InvalidKey));
        assert_eq!(license.key, K1);
        // Entered here: nothing for the agent to open the app for.
        assert!(!license.needs_app());
    }

    #[test]
    fn polar_answers_move_the_state() {
        let mut license = licensed(K1, "act-1");
        assert_eq!(license.state, LicenseState::Licensed);
        assert_eq!(license.activation_id, "act-1");
        assert_eq!(
            license.told(),
            Told {
                key_match: Some(key_match(K1)),
                standing: Standing::Licensed,
                activation_id: "act-1".to_owned(),
                ..Told::default()
            }
        );
        license
            .record(K1, validated("act-1", 1_793_790_000))
            .unwrap();
        assert_eq!(license.validated_at, 1_793_790_000);
        assert_eq!(license.customer_email, "someone@example.com");
        license.record(K1, gone("act-1")).unwrap();
        assert_eq!(license.state, LicenseState::Invalid);
        assert!(license.activation_id.is_empty());
        assert_eq!(license.told().standing, Standing::Invalid);
        assert_eq!(license.told().activation_id, "");
    }

    #[test]
    fn an_activation_counts_only_while_one_is_awaited() {
        let mut license = licensed(K1, "act-1");
        // A second app, or a late reply: the app frees this one.
        assert_eq!(
            license.record(K1, activated("act-2")),
            Err(LicenseError::AlreadyLicensed)
        );
        assert_eq!(license.activation_id, "act-1");
        // The same activation told again, by a retried reply: nothing to free.
        assert_eq!(license.record(K1, activated("act-1")), Ok(()));
        assert_eq!(license.state, LicenseState::Licensed);
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
        assert_eq!(
            license.record(K1, validated("act-1", 1_793_800_000)),
            Err(LicenseError::Stale)
        );
        assert_eq!(license.record(K1, gone("act-1")), Err(LicenseError::Stale));
        license.record(K1, activated("act-1")).unwrap();
        assert_eq!(
            license.record(K1, validated("act-0", 1_793_800_000)),
            Err(LicenseError::Stale)
        );
        assert_eq!(license.record(K1, gone("act-0")), Err(LicenseError::Stale));
        assert_eq!(
            license.record(K1, refused(Refusal::SeatsFull)),
            Err(LicenseError::Stale)
        );
        assert_eq!(license.state, LicenseState::Licensed);
        // validate never says the seats are full.
        let full = LicenseResult::Refused {
            refusal: Refusal::SeatsFull,
            activation_id: "act-1".to_owned(),
        };
        assert_eq!(license.record(K1, full), Err(LicenseError::InvalidResult));
    }

    #[test]
    fn entering_the_same_key_again_keeps_its_activation_unless_it_was_refused() {
        let mut license = licensed(K1, "act");
        license.set_key(K1).unwrap();
        assert_eq!(license.state, LicenseState::Licensed);
        assert_eq!(license.activation_id, "act");
        let mut full = LicenseFile::default();
        full.set_key(K1).unwrap();
        full.record(K1, refused(Refusal::SeatsFull)).unwrap();
        full.set_key(K1).unwrap();
        assert_eq!(full.state, LicenseState::PendingActivation);
    }

    #[test]
    fn an_answer_about_another_key_is_ignored() {
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        assert_eq!(
            license.record(K2, activated("act")),
            Err(LicenseError::Changed)
        );
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
        let long_email = LicenseResult::Activated {
            activation_id: "act".to_owned(),
            customer_email: "a".repeat(MAX_EMAIL_LEN + 1),
            validated_at: 1_793_800_000,
        };
        assert_eq!(
            license.record(K1, long_email),
            Err(LicenseError::InvalidResult)
        );
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
    }

    #[test]
    fn nothing_a_device_says_gives_this_computer_a_key() {
        let mut license = LicenseFile::default();
        assert!(license.peer_said(said("win", K1, Standing::Licensed, "act-w")));
        assert!(license.peer_said(PeerLicense::legacy("mini", K2, true).unwrap()));
        assert!(license.key.is_empty());
        assert_eq!(license.state, LicenseState::None);
        // Both are licensed, with keys this computer does not have.
        assert_eq!(license.use_of("win", true), LicenseUse::OtherLicense);
        assert_eq!(license.use_of("mini", true), LicenseUse::OtherLicense);
        let status = license.status(named, |_| true);
        assert_eq!(status.devices.len(), 2);
        assert!(
            status
                .devices
                .iter()
                .find(|d| d.id == "mini")
                .unwrap()
                .legacy
        );
        assert!(
            !status
                .devices
                .iter()
                .find(|d| d.id == "win")
                .unwrap()
                .legacy
        );
    }

    #[test]
    fn applying_the_license_waits_for_an_offline_device_and_goes_as_it_connects() {
        let mut license = licensed(K1, "act");
        license.peer_said(none("win"));
        license.apply("win").unwrap();
        assert_eq!(license.use_of("win", false), LicenseUse::Waiting);
        assert_eq!(license.use_of("win", true), LicenseUse::Activating);
        assert!(license.can_take_back("win"));
        // Applying it again changes nothing.
        license.apply("win").unwrap();
        assert_eq!(license.owed.len(), 1);
        // It connects: the key goes with the apply's id, and is noted as
        // sent.
        let id = applied_id(&license, "win");
        assert_eq!(
            license.deliveries("win"),
            [Delivery::Assign {
                key: K1.to_owned(),
                apply_id: id.clone(),
            }]
        );
        license.mark_sent("win");
        assert_eq!(
            license.owed[0].action,
            Action::Apply {
                sent: true,
                apply_id: id.clone(),
            }
        );
        // An old word from it changes nothing; that it waits for the key
        // settles it.
        assert!(!license.peer_said(none("win")));
        assert_eq!(license.use_of("win", true), LicenseUse::Activating);
        license.peer_said(from_apply("win", K1, Standing::Pending, "", &id));
        assert!(license.owed.is_empty());
        assert_eq!(license.use_of("win", true), LicenseUse::Activating);
        // Still taken back while it waits, by the apply it says.
        assert!(license.can_take_back("win"));
        license.peer_said(from_apply("win", K1, Standing::Licensed, "act-w", &id));
        assert_eq!(license.use_of("win", false), LicenseUse::Licensed);
        assert!(!license.can_take_back("win"));
        let status = license.status(named, |_| false);
        assert_eq!(status.devices[0].activation_id, "act-w");
        assert_eq!(status.devices[0].licensed_use, LicenseUse::Licensed);
    }

    /// The id of the apply owed device `id`.
    fn applied_id(license: &LicenseFile, id: &str) -> String {
        license
            .owed
            .iter()
            .find_map(|o| match &o.action {
                Action::Apply { apply_id, .. } if o.id == id => Some(apply_id.clone()),
                _ => None,
            })
            .unwrap()
    }

    /// What a device says of a key applied to it as `apply_id`.
    fn from_apply(
        id: &str,
        key: &str,
        standing: Standing,
        act: &str,
        apply_id: &str,
    ) -> PeerLicense {
        PeerLicense {
            apply_id: apply_id.to_owned(),
            ..said(id, key, standing, act)
        }
    }

    #[test]
    fn apply_ids_are_random_and_printable() {
        let (a, b) = (new_apply_id(), new_apply_id());
        assert_ne!(a, b);
        assert!(valid_apply_id(&a), "{a}");
        assert!(!valid_apply_id(""));
        assert!(!valid_apply_id(&"a".repeat(MAX_APPLY_ID_LEN + 1)));
    }

    #[test]
    fn only_a_licensed_computer_applies_and_never_over_another_license() {
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        assert_eq!(license.apply("win"), Err(LicenseError::NotLicensed));
        let mut license = licensed(K1, "act");
        license.peer_said(said("win", K2, Standing::Licensed, "act-w"));
        assert_eq!(license.apply("win"), Err(LicenseError::LicensedElsewhere));
        assert_eq!(license.use_of("win", true), LicenseUse::OtherLicense);
        // Licensed with another key after it was applied: the apply is
        // dropped, as the device will not take it.
        license.peer_said(none("mini"));
        license.apply("mini").unwrap();
        license.peer_said(said("mini", K2, Standing::Licensed, "act-m"));
        assert!(license.owed.is_empty());
    }

    #[test]
    fn an_apply_the_device_did_not_take_stops_waiting() {
        // A device on 1.1 still freeing an activation another revoked
        // there says it did not take the apply: it goes back to trial
        // here, for Apply License again.
        let mut license = licensed(K1, "act");
        license.peer_said(none("win"));
        license.apply("win").unwrap();
        let id = applied_id(&license, "win");
        license.peer_said(PeerLicense {
            refused_apply: id,
            ..none("win")
        });
        assert!(license.owed.is_empty());
        assert_eq!(license.use_of("win", true), LicenseUse::None);
        // InputWeave 1.0 holding a key of its own waiting for activation
        // takes no other: shown as on another license, and not applied.
        license.peer_said(PeerLicense::legacy("old", K2, false).unwrap());
        assert_eq!(license.use_of("old", true), LicenseUse::OtherLicense);
        assert_eq!(license.apply("old"), Err(LicenseError::LicensedElsewhere));
        // Applied before it said so: the apply is dropped as it does.
        license.peer_said(PeerLicense::legacy("older", "", false).unwrap());
        license.apply("older").unwrap();
        license.peer_said(PeerLicense::legacy("older", K2, false).unwrap());
        assert!(license.owed.is_empty());
        assert_eq!(license.use_of("older", true), LicenseUse::OtherLicense);
    }

    #[test]
    fn full_places_are_tried_again_by_applying_again() {
        let mut license = licensed(K1, "act");
        license.peer_said(said("win", K1, Standing::SeatsFull, ""));
        assert_eq!(license.use_of("win", true), LicenseUse::NoPlaces);
        // Already holding the key, but full: applying sends it again.
        license.apply("win").unwrap();
        assert_eq!(license.use_of("win", true), LicenseUse::Activating);
        assert_eq!(license.deliveries("win").len(), 1);
        // Still full until it says it waits again.
        assert!(!license.peer_said(said("win", K1, Standing::SeatsFull, "")));
        assert_eq!(license.owed.len(), 1);
    }

    #[test]
    fn places_count_this_computer_and_the_devices_licensed_activating_or_waiting() {
        let mut license = licensed(K1, "act");
        license.peer_said(said("a", K1, Standing::Licensed, "1"));
        license.peer_said(said("b", K1, Standing::Pending, ""));
        license.peer_said(said("c", K1, Standing::SeatsFull, ""));
        license.peer_said(said("d", K2, Standing::Licensed, "2"));
        license.peer_said(none("e"));
        license.apply("e").unwrap();
        let ids = ["a", "b", "c", "d", "e", "f"];
        assert_eq!(license.places_taken(ids, |_| false), 4);
        let mut trial = LicenseFile::default();
        trial.peer_said(said("a", K1, Standing::Licensed, "1"));
        assert_eq!(trial.places_taken(ids, |_| true), 0);
    }

    /// The revoke owed device `id`.
    fn revoke_owed(license: &LicenseFile, id: &str) -> Action {
        license
            .owed
            .iter()
            .find(|o| o.id == id && matches!(o.action, Action::Revoke { .. }))
            .map(|o| o.action.clone())
            .unwrap()
    }

    #[test]
    fn revoking_names_the_activation_and_proves_the_key() {
        let mut license = licensed(K1, "act");
        license.peer_said(said("win", K1, Standing::Licensed, "act-w"));
        // Not an activation the device has: refused, nothing owed.
        assert_eq!(
            license.revoke("win", "act-old", true),
            Err(LicenseError::Stale)
        );
        assert!(license.owed.is_empty());
        license.revoke("win", "act-w", true).unwrap();
        // Shown on trial at once, offline or not.
        assert_eq!(license.use_of("win", false), LicenseUse::None);
        assert!(license.status(named, |_| false).devices.is_empty());
        let proof = revoke_proof(K1, "win");
        assert_eq!(
            license.deliveries("win"),
            [Delivery::Revoke {
                proof,
                activation_id: "act-w".to_owned(),
                apply_id: String::new(),
                free_it: false,
            }]
        );
        // The proof is the device's own: no other device's, and not the
        // match form every device hears.
        assert_ne!(proof, revoke_proof(K1, "mini"));
        assert_ne!(proof, revoke_proof(K2, "win"));
        assert_ne!(proof, key_match(K1));
        // Still licensed with it, as it said before hearing: kept.
        assert!(!license.peer_said(said("win", K1, Standing::Licensed, "act-w")));
        assert_eq!(license.owed.len(), 1);
        license.peer_said(none("win"));
        assert!(license.owed.is_empty());
        // Polar not reached: the device is to free it itself.
        license.peer_said(said("win", K1, Standing::Licensed, "act-w2"));
        license.revoke("win", "act-w2", false).unwrap();
        assert!(matches!(
            revoke_owed(&license, "win"),
            Action::Revoke { free_it: true, .. }
        ));
    }

    #[test]
    fn a_revoke_is_done_once_the_device_has_another_activation_or_key() {
        let mut license = licensed(K1, "act");
        license.peer_said(said("win", K1, Standing::Licensed, "act-w"));
        license.revoke("win", "act-w", true).unwrap();
        // Applied again by another device since, and activated anew.
        license.peer_said(said("win", K1, Standing::Licensed, "act-w2"));
        assert!(license.owed.is_empty());
        assert_eq!(license.use_of("win", true), LicenseUse::Licensed);
    }

    #[test]
    fn dont_apply_takes_back_what_never_reached_the_device_or_names_the_apply() {
        let mut license = licensed(K1, "act");
        license.apply("win").unwrap();
        license.revoke("win", "", false).unwrap();
        assert!(license.owed.is_empty());
        // Sent already: the device may hold the key, so it is told to drop
        // that apply.
        license.apply("win").unwrap();
        let id = applied_id(&license, "win");
        license.mark_sent("win");
        license.revoke("win", "", false).unwrap();
        assert_eq!(
            revoke_owed(&license, "win"),
            Action::Revoke {
                key_match: key_match(K1),
                proof: revoke_proof(K1, "win"),
                activation_id: String::new(),
                apply_id: id.clone(),
                free_it: true,
            }
        );
        // Done once it no longer holds the key from that apply.
        license.peer_said(from_apply("win", K1, Standing::Pending, "", &id));
        assert_eq!(license.owed.len(), 1);
        license.peer_said(none("win"));
        assert!(license.owed.is_empty());
        // A device waiting for the key from another computer's apply: the
        // apply it says.
        license.peer_said(from_apply("mini", K1, Standing::Pending, "", "theirs"));
        assert!(license.can_take_back("mini"));
        license.revoke("mini", "", false).unwrap();
        assert!(matches!(
            revoke_owed(&license, "mini"),
            Action::Revoke { ref apply_id, .. } if apply_id == "theirs"
        ));
        // Nothing to take back from a key its own user entered there.
        let mut other = licensed(K1, "act");
        other.peer_said(said("desk", K1, Standing::Pending, ""));
        assert!(!other.can_take_back("desk"));
        other.revoke("desk", "", false).unwrap();
        assert!(other.owed.is_empty());
        other.peer_said(none("win"));
        other.revoke("win", "", false).unwrap();
        assert!(other.owed.is_empty());
    }

    #[test]
    fn applying_again_after_a_revoke_sends_both_in_turn() {
        let mut license = licensed(K1, "act");
        license.peer_said(said("win", K1, Standing::Licensed, "act-w"));
        license.revoke("win", "act-w", true).unwrap();
        license.apply("win").unwrap();
        assert_eq!(license.use_of("win", false), LicenseUse::Waiting);
        assert_eq!(license.deliveries("win").len(), 2);
        // What it said before hearing settles neither.
        license.peer_said(said("win", K1, Standing::Licensed, "act-w"));
        assert_eq!(license.owed.len(), 2);
        // Revoked, then waiting for the key again.
        license.peer_said(none("win"));
        assert_eq!(license.owed.len(), 1);
        let id = applied_id(&license, "win");
        license.peer_said(from_apply("win", K1, Standing::Pending, "", &id));
        assert!(license.owed.is_empty());
    }

    #[test]
    fn a_device_on_1_0_hears_the_key_only_while_it_counts_on_the_license() {
        let mut license = licensed(K1, "act");
        // Synced under 1.0 before this computer was updated.
        license.peer_said(PeerLicense::legacy("old", K1, true).unwrap());
        assert!(license.keyed("old"));
        assert_eq!(license.use_of("old", true), LicenseUse::Licensed);
        let status = license.status(|_| Some("Old PC".to_owned()), |_| true);
        assert!(status.devices[0].legacy);
        assert_eq!(status.devices[0].activation_id, "");
        // It cannot be told of a revoke.
        assert_eq!(license.revoke("old", "", false), Err(LicenseError::Legacy));
        assert!(license.keyed("old"));
        // One without the key hears it only once applied to.
        license.peer_said(PeerLicense::legacy("new", "", false).unwrap());
        assert!(!license.keyed("new"));
        license.apply("new").unwrap();
        assert!(license.keyed("new"));
        license.mark_sent("new");
        // Taken: it counts by its key from now on, and cannot be taken
        // back from.
        license.peer_said(PeerLicense::legacy("new", K1, false).unwrap());
        assert!(license.owed.is_empty());
        assert!(license.keyed("new"));
        assert!(!license.can_take_back("new"));
        // Not while this computer is not licensed.
        license.record(K1, gone("act")).unwrap();
        assert!(!license.keyed("old"));
        // Keys that cannot be one are not taken from 1.0 either.
        assert_eq!(PeerLicense::legacy("bad", "short", true), None);
    }

    #[test]
    fn a_device_applies_its_key_here_unless_another_is_licensed() {
        let mut license = LicenseFile::default();
        assert!(license.assigned("mac", "MacBook Pro", K1, "a1"));
        assert_eq!(license.key, K1);
        assert_eq!(license.state, LicenseState::PendingActivation);
        assert_eq!(license.source_device_id, "mac");
        assert_eq!(license.source_device_name, "MacBook Pro");
        assert_eq!(license.apply_id, "a1");
        assert_eq!(license.told().apply_id, "a1");
        assert!(license.needs_app());
        // The same key again, waiting or licensed: nothing changes.
        assert!(!license.assigned("win", "Windows PC", K1, "a2"));
        assert_eq!(license.source_device_id, "mac");
        assert_eq!(license.apply_id, "a1");
        license.record(K1, activated("act")).unwrap();
        assert!(!license.needs_app());
        assert!(!license.assigned("win", "Windows PC", K1, "a2"));
        // Another key never replaces one activated here: the apply is said
        // to be refused.
        assert!(license.assigned("win", "Windows PC", K2, "a3"));
        assert_eq!(license.key, K1);
        assert_eq!(license.told().refused_apply, "a3");
        assert!(!license.assigned("win", "Windows PC", K2, "a3"));
        // But one refused, or waiting, does.
        let mut full = LicenseFile::default();
        full.set_key(K2).unwrap();
        full.record(K2, refused(Refusal::SeatsFull)).unwrap();
        assert!(full.assigned("mac", "MacBook Pro", K2, "b1"));
        assert_eq!(full.state, LicenseState::PendingActivation);
        assert!(full.assigned("win", "Windows PC", K1, "b2"));
        assert_eq!(full.key, K1);
        assert!(!full.assigned("win", "Windows PC", "not a key", "b3"));
        assert!(!full.assigned("win", "Windows PC", K2, ""));
        // Entered here: no apply.
        full.set_key(K1).unwrap();
        assert_eq!(full.apply_id, "");
    }

    #[test]
    fn an_apply_never_replaces_a_key_entered_here_waiting_to_be_activated() {
        let mut license = LicenseFile::default();
        license.set_key(K2).unwrap();
        assert_eq!(license.state, LicenseState::PendingActivation);
        // Refused, and said so; the key entered here stays, still waiting.
        assert!(license.assigned("mac", "MacBook Pro", K1, "a1"));
        assert_eq!(license.key, K2);
        assert_eq!(license.state, LicenseState::PendingActivation);
        assert!(license.source_device_id.is_empty());
        assert_eq!(license.told().refused_apply, "a1");
        assert!(!license.needs_app());
        assert!(!license.assigned("mac", "MacBook Pro", K1, "a1"));
        // The same key: nothing changes.
        assert!(!license.assigned("mac", "MacBook Pro", K2, "a2"));
        assert_eq!(license.apply_id, "");
    }

    #[test]
    fn a_revoke_here_acts_only_on_what_it_names_and_proves() {
        let here = "win";
        let proof = revoke_proof(K1, here);
        // Its activation deactivated by the device that revoked it.
        let mut license = licensed(K1, "act");
        license.source_device_id = "mac".to_owned();
        // Another key's proof, or another device's: nothing.
        assert!(!license.revoked(here, &revoke_proof(K2, here), "act", "", false));
        assert!(!license.revoked(here, &revoke_proof(K1, "mini"), "act", "", false));
        // The match form every device hears is no proof.
        assert!(!license.revoked(here, &key_match(K1), "act", "", false));
        assert!(license.revoked(here, &proof, "act", "", false));
        assert_eq!(license.state, LicenseState::None);
        assert!(license.key.is_empty());
        assert!(license.source_device_id.is_empty());
        // Not deactivated: the app frees it, then removes the license.
        let mut license = licensed(K1, "act");
        assert!(license.revoked(here, &proof, "act", "", true));
        assert_eq!(license.state, LicenseState::Revoked);
        assert_eq!(license.activation_id, "act");
        assert!(license.needs_app());
        assert_eq!(license.told(), Told::default());
        license.remove();
        assert_eq!(license.state, LicenseState::None);
        // Another activation than the one named: ignored, freed or not.
        let mut license = licensed(K1, "act-new");
        assert!(!license.revoked(here, &proof, "act-old", "", false));
        assert!(!license.revoked(here, &proof, "act-old", "", true));
        assert_eq!(license.state, LicenseState::Licensed);
        // A revoke naming no activation and no apply is none.
        assert!(!license.revoked(here, &proof, "", "", true));
        assert_eq!(license.state, LicenseState::Licensed);
    }

    #[test]
    fn a_revoke_of_an_apply_takes_the_key_back_or_frees_what_came_of_it() {
        let here = "win";
        let proof = revoke_proof(K1, here);
        let mut waiting = LicenseFile::default();
        waiting.assigned("mac", "MacBook Pro", K1, "a1");
        // Another apply: ignored.
        assert!(!waiting.revoked(here, &proof, "", "a0", false));
        assert!(waiting.revoked(here, &proof, "", "a1", false));
        assert!(waiting.key.is_empty());
        // Activated from that apply meanwhile: the app frees it.
        let mut activated_since = LicenseFile::default();
        activated_since.assigned("mac", "MacBook Pro", K1, "a1");
        activated_since.record(K1, activated("act")).unwrap();
        assert!(activated_since.revoked(here, &proof, "", "a1", false));
        assert_eq!(activated_since.state, LicenseState::Revoked);
        assert_eq!(activated_since.activation_id, "act");
        // Entered here: no apply names it.
        let mut entered = licensed(K1, "act");
        assert!(!entered.revoked(here, &proof, "", "a1", false));
    }

    #[test]
    fn a_revoke_that_waited_never_undoes_a_license_applied_since() {
        // The PC was licensed as act-1, from the Mac's apply a1. The mini
        // revokes it while it is away; meanwhile the Mac revokes and
        // applies it again (a2), and it activates anew as act-2. The
        // mini's revoke, delivered at last, names act-1 or a1: neither is
        // the PC's any more.
        let here = "win";
        let proof = revoke_proof(K1, here);
        let mut pc = LicenseFile::default();
        pc.assigned("mac", "MacBook Pro", K1, "a1");
        pc.record(K1, activated("act-1")).unwrap();
        // The Mac's revoke and new apply.
        assert!(pc.revoked(here, &proof, "act-1", "", false));
        assert!(pc.assigned("mac", "MacBook Pro", K1, "a2"));
        pc.record(K1, activated("act-2")).unwrap();
        // The mini's, late, by activation, freed or not, or by apply.
        assert!(!pc.revoked(here, &proof, "act-1", "", false));
        assert!(!pc.revoked(here, &proof, "act-1", "", true));
        assert!(!pc.revoked(here, &proof, "", "a1", false));
        assert_eq!(pc.state, LicenseState::Licensed);
        assert_eq!(pc.activation_id, "act-2");

        // On the mini's side, its revoke of act-1 is done as soon as the PC
        // says it holds act-2.
        let mut mini = licensed(K1, "act-m");
        mini.peer_said(from_apply(here, K1, Standing::Licensed, "act-1", "a1"));
        mini.revoke(here, "act-1", false).unwrap();
        assert_eq!(mini.owed.len(), 1);
        mini.peer_said(from_apply(here, K1, Standing::Licensed, "act-2", "a2"));
        assert!(mini.owed.is_empty());
        // So is a revoke of apply a1 once it says a2.
        mini.peer_said(from_apply(here, K1, Standing::Pending, "", "a1"));
        mini.revoke(here, "", false).unwrap();
        mini.peer_said(from_apply(here, K1, Standing::Pending, "", "a2"));
        assert!(mini.owed.is_empty());
    }

    #[test]
    fn a_revoked_activation_is_never_licensed_again() {
        let here = "win";
        let proof = revoke_proof(K1, here);
        let to_free = |act: &str| ToFree {
            key: K1.to_owned(),
            activation_id: act.to_owned(),
        };
        // Applied again with the same key while the app may be freeing it:
        // a new activation is waited for, the old one left to free.
        let mut license = licensed(K1, "act");
        license.revoked(here, &proof, "act", "", true);
        assert!(license.assigned("mini", "Mac mini", K1, "a1"));
        assert_eq!(license.state, LicenseState::PendingActivation);
        assert!(license.activation_id.is_empty());
        assert_eq!(license.apply_id, "a1");
        assert_eq!(license.to_free, [to_free("act")]);
        // Entered again by hand, the same.
        let mut entered = licensed(K1, "act");
        entered.revoked(here, &proof, "act", "", true);
        entered.set_key(K1).unwrap();
        assert_eq!(entered.state, LicenseState::PendingActivation);
        assert!(entered.activation_id.is_empty());
        assert_eq!(entered.to_free, [to_free("act")]);
        // Nor is another key taken by an apply while one waits to be
        // freed: the apply is said to be refused.
        let mut other = licensed(K1, "act");
        other.revoked(here, &proof, "act", "", true);
        assert!(other.assigned("mini", "Mac mini", K2, "a2"));
        assert_eq!(other.state, LicenseState::Revoked);
        assert_eq!(other.key, K1);
        assert_eq!(other.refused_apply, "a2");
    }

    #[test]
    fn a_removal_that_waited_never_wipes_a_newer_license() {
        let here = "win";
        let mut license = licensed(K1, "act");
        license.revoked(here, &revoke_proof(K1, here), "act", "", true);
        // Applied again meanwhile: the removal the app asked for, for the
        // revoked activation, is stale.
        license.assigned("mini", "Mac mini", K1, "a1");
        assert_eq!(license.remove_if(K1, "act"), Err(LicenseError::Stale));
        assert_eq!(license.state, LicenseState::PendingActivation);
        // Of the license as it is: removed.
        license.remove_if(K1, "").unwrap();
        assert!(license.key.is_empty());
        // Another key: stale; none named: removed whatever it is.
        let mut other = licensed(K2, "act-2");
        assert_eq!(other.remove_if(K1, "act-2"), Err(LicenseError::Stale));
        other.remove_if("", "").unwrap();
        assert!(other.key.is_empty());
    }

    #[test]
    fn a_new_key_or_none_drops_what_has_not_been_applied_yet() {
        let mut license = licensed(K1, "act");
        license.peer_said(said("mini", K1, Standing::Licensed, "act-m"));
        license.apply("win").unwrap();
        license.revoke("mini", "act-m", true).unwrap();
        license.remove();
        // Revoking goes on; applying went with the key.
        assert_eq!(license.owed.len(), 1);
        assert!(matches!(license.owed[0].action, Action::Revoke { .. }));
        assert!(!license.deliveries("mini").is_empty());
        let mut other = licensed(K1, "act");
        other.apply("win").unwrap();
        other.set_key(K2).unwrap();
        assert!(other.owed.is_empty());
        // Revoking needs a key, and an activation id that can be one.
        let mut none = LicenseFile::default();
        assert_eq!(
            none.revoke("win", "", false),
            Err(LicenseError::NotLicensed)
        );
        assert_eq!(
            licensed(K1, "act").revoke("win", "has space", false),
            Err(LicenseError::InvalidResult)
        );
    }

    #[test]
    fn the_match_form_follows_the_key() {
        let mut license = LicenseFile::default();
        assert_eq!(license.own_match(), None);
        license.assigned("mac", "MacBook Pro", K1, "a0");
        assert_eq!(license.own_match(), Some(key_match(K1)));
        license.assigned("win", "Windows PC", K2, "a1");
        assert_eq!(license.own_match(), Some(key_match(K2)));
        license.set_key(K1).unwrap();
        assert_eq!(license.own_match(), Some(key_match(K1)));
        license.remove();
        assert_eq!(license.own_match(), None);
        let parsed = LicenseFile::parse(&format!("key = \"{K1}\"\n")).unwrap();
        assert_eq!(parsed.own_match(), Some(key_match(K1)));
    }

    #[test]
    fn an_offline_device_waiting_to_activate_shows_as_waiting_and_out_of_reach() {
        let mut license = licensed(K1, "act");
        license.peer_said(from_apply("win", K1, Standing::Pending, "", "a1"));
        assert_eq!(license.use_of("win", true), LicenseUse::Activating);
        assert_eq!(license.use_of("win", false), LicenseUse::Waiting);
        // Still a place either way.
        assert_eq!(license.places_taken(["win"], |_| false), 2);
        let row = |connected: bool| license.status(named, |_| connected).devices[0].clone();
        assert!(!row(true).key_out_of_reach);
        // Offline with the key: taking it back waits for it to connect.
        assert!(row(false).key_out_of_reach);
        assert!(row(false).can_take_back);
        // An apply never sent is not out of reach: nothing is there.
        let mut other = licensed(K1, "act");
        other.apply("mini").unwrap();
        let row = other.status(named, |_| false).devices[0].clone();
        assert_eq!(row.licensed_use, LicenseUse::Waiting);
        assert!(!row.key_out_of_reach);
        other.mark_sent("mini");
        assert!(other.status(named, |_| false).devices[0].key_out_of_reach);
    }

    #[test]
    fn a_revoked_activation_is_still_freed_when_another_key_is_entered() {
        let here = "win";
        let mut license = licensed(K1, "act");
        assert!(license.revoked(here, &revoke_proof(K1, here), "act", "", true));
        assert!(license.needs_app());
        license.set_key(K2).unwrap();
        assert_eq!(license.key, K2);
        assert_eq!(
            license.to_free,
            [ToFree {
                key: K1.to_owned(),
                activation_id: "act".to_owned(),
            }]
        );
        // The app is opened for it, and it lasts whatever the license does.
        assert!(license.needs_app());
        license.record(K2, activated("act-2")).unwrap();
        license.remove();
        assert_eq!(license.status(named, |_| true).to_free.len(), 1);
        // Kept in the file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("license.toml");
        license.save(&path).unwrap();
        assert_eq!(LicenseFile::load(&path).unwrap().to_free, license.to_free);
        // Freed: forgotten; once.
        assert!(license.freed("act"));
        assert!(!license.freed("act"));
        assert!(!license.needs_app());
        // The same key entered again waits for a new activation; the
        // revoked one is still to free.
        let mut same = licensed(K1, "act");
        same.revoked(here, &revoke_proof(K1, here), "act", "", true);
        same.set_key(K1).unwrap();
        assert_eq!(same.to_free.len(), 1);
        assert_eq!(same.state, LicenseState::PendingActivation);
    }

    #[test]
    fn a_refused_apply_is_kept_across_restarts_and_new_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("license.toml");
        let mut license = licensed(K1, "act");
        license.assigned("mac", "MacBook Pro", K2, "a1");
        assert_eq!(license.told().refused_apply, "a1");
        license.save(&path).unwrap();
        let mut read = LicenseFile::load(&path).unwrap();
        assert_eq!(read.told().refused_apply, "a1");
        // A new key, or none, still says it, for the sender to stop.
        read.set_key(K2).unwrap();
        assert_eq!(read.told().refused_apply, "a1");
        read.remove();
        assert_eq!(read.told().refused_apply, "a1");
        // Until another refusal replaces it.
        let mut again = licensed(K1, "act");
        again.assigned("mac", "MacBook Pro", K2, "a1");
        again.assigned("mini", "Mac mini", K2, "a2");
        assert_eq!(again.told().refused_apply, "a2");
    }

    #[test]
    fn bad_hex_in_the_file_drops_the_entry_alone() {
        let good = crate::hex::hex(&key_match(K1));
        let file = LicenseFile::parse(&format!(
            "key = \"{K1}\"\nstate = \"licensed\"\nactivation_id = \"act\"\n\
             [[peers]]\nid = \"win\"\nkey_match = \"{good}\"\nstanding = \"licensed\"\n\
             [[peers]]\nid = \"mini\"\nkey_match = \"{}zz\"\nstanding = \"licensed\"\n\
             [[owed]]\nid = \"win\"\naction = \"revoke\"\nkey_match = \"{good}\"\n\
             proof = \"not hex\"\nactivation_id = \"act-w\"\n\
             [[owed]]\nid = \"mini\"\naction = \"revoke\"\nkey_match = \"{good}\"\n\
             proof = \"{good}\"\nactivation_id = \"act-m\"\n",
            &good[2..]
        ))
        .unwrap();
        assert_eq!(file.peers.len(), 1);
        assert_eq!(file.peers[0].id, "win");
        assert_eq!(file.owed.len(), 1);
        assert_eq!(file.owed[0].id, "mini");
    }

    #[test]
    fn a_device_on_1_0_that_heard_the_key_cannot_give_it_back() {
        let mut license = licensed(K1, "act");
        license.peer_said(PeerLicense::legacy("old", "", false).unwrap());
        license.apply("old").unwrap();
        // Not told yet: taken back as usual.
        assert!(license.can_take_back("old"));
        license.mark_sent("old");
        assert!(!license.can_take_back("old"));
        assert_eq!(license.revoke("old", "", false), Err(LicenseError::Legacy));
        assert_eq!(license.owed.len(), 1);
        let row = license.status(named_old, |_| true).devices[0].clone();
        assert!(row.legacy);
        assert!(!row.can_take_back);
    }

    fn named_old(id: &str) -> Option<String> {
        (id == "old").then(|| "Old PC".to_owned())
    }

    #[test]
    fn a_device_on_1_0_that_does_not_take_the_key_goes_back_to_trial() {
        let mut license = licensed(K1, "act");
        license.peer_said(PeerLicense::legacy("old", "", false).unwrap());
        license.apply("old").unwrap();
        // Not told the key yet: nothing to refuse.
        assert!(!license.awaits_legacy_take("old"));
        assert!(!license.refuse_legacy_apply("old"));
        license.mark_sent("old");
        // What it says without the key changes nothing by itself: the
        // engine gives it its time after it heard the key in a session.
        license.peer_said(PeerLicense::legacy("old", "", false).unwrap());
        license.peer_said(PeerLicense::legacy("old", "", false).unwrap());
        assert_eq!(license.owed.len(), 1);
        assert_eq!(license.use_of("old", true), LicenseUse::Activating);
        assert!(license.awaits_legacy_take("old"));
        // It did not take it (its user removed the license there).
        assert!(license.refuse_legacy_apply("old"));
        assert!(license.owed.is_empty());
        assert_eq!(license.use_of("old", true), LicenseUse::None);
        let row = license.status(named_old, |_| true).devices[0].clone();
        assert!(row.legacy_refused);
        assert_eq!(row.licensed_use, LicenseUse::None);
        // Kept in the file, until applied again.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("license.toml");
        license.save(&path).unwrap();
        let mut read = LicenseFile::load(&path).unwrap();
        assert!(read.peers[0].legacy_refused);
        read.apply("old").unwrap();
        assert!(!read.peers[0].legacy_refused);
        // Taking the key after all clears it too.
        license.peer_said(PeerLicense::legacy("old", K1, true).unwrap());
        assert!(!license.peers[0].legacy_refused);
    }

    #[test]
    fn applies_owed_while_this_computer_is_not_licensed_wait_and_show() {
        let mut license = licensed(K1, "act");
        license.peer_said(none("win"));
        license.apply("win").unwrap();
        // Its license found gone: the apply is kept, shown waiting, and can
        // be taken back, but is not sent.
        license.record(K1, gone("act")).unwrap();
        assert_eq!(license.state, LicenseState::Invalid);
        assert_eq!(license.use_of("win", true), LicenseUse::Waiting);
        assert!(license.can_take_back("win"));
        assert!(license.deliveries("win").is_empty());
        let row = license.status(named, |_| true).devices[0].clone();
        assert_eq!(row.licensed_use, LicenseUse::Waiting);
        // Licensed again: it goes.
        license.set_key(K1).unwrap();
        license.record(K1, activated("act-2")).unwrap();
        assert_eq!(license.deliveries("win").len(), 1);
        // Taken back while not licensed: gone.
        let mut other = licensed(K1, "act");
        other.apply("win").unwrap();
        other.record(K1, gone("act")).unwrap();
        other.revoke("win", "", false).unwrap();
        assert!(other.owed.is_empty());
    }

    #[test]
    fn only_an_apply_not_shown_yet_waits_to_apply() {
        let row = |license: &LicenseFile, id: &str, connected: bool| {
            license
                .status(named, |_| connected)
                .devices
                .into_iter()
                .find(|d| d.id == id)
                .unwrap()
        };
        // A device waiting to activate the key its own user entered: nothing
        // owed it from here.
        let mut license = licensed(K1, "act");
        license.peer_said(said("win", K1, Standing::Pending, ""));
        license.peer_said(none("mini"));
        license.apply("mini").unwrap();
        for licensed_here in [true, false] {
            if !licensed_here {
                license.record(K1, gone("act")).unwrap();
                assert_eq!(license.state, LicenseState::Invalid);
            }
            let connected = row(&license, "win", true);
            assert_eq!(connected.licensed_use, LicenseUse::Activating);
            assert!(!connected.waiting_to_apply);
            let offline = row(&license, "win", false);
            assert_eq!(offline.licensed_use, LicenseUse::Waiting);
            assert!(!offline.can_take_back);
            assert!(!offline.waiting_to_apply);
            // The apply from here waits, offline or (this computer not
            // licensed) whatever its connection.
            assert!(row(&license, "mini", false).waiting_to_apply);
            assert_eq!(row(&license, "mini", true).waiting_to_apply, !licensed_here);
        }
    }

    #[test]
    fn apply_refusals_name_the_device_before_the_places() {
        let mut license = licensed(K1, "act");
        license.peer_said(said("win", K2, Standing::Licensed, "x"));
        assert_eq!(
            license.refuses_apply("win"),
            Some(LicenseError::LicensedElsewhere)
        );
        assert_eq!(license.refuses_apply("mini"), None);
        let mut trial = LicenseFile::default();
        assert_eq!(trial.refuses_apply("mini"), Some(LicenseError::NotLicensed));
        assert_eq!(trial.apply("mini"), Err(LicenseError::NotLicensed));
    }

    #[test]
    fn untrusted_devices_are_neither_listed_nor_owed() {
        let mut license = licensed(K1, "act");
        license.peer_said(said("stranger", K1, Standing::Licensed, "x"));
        license.peer_said(said("win", K1, Standing::Licensed, "act-w"));
        license.apply("gone").unwrap();
        let status = license.status(named, |_| true);
        assert_eq!(status.devices.len(), 1);
        assert_eq!(status.devices[0].name, "Windows PC");
        assert!(license.forget_peers(|id| id == "win"));
        assert_eq!(license.peers.len(), 1);
        assert!(license.owed.is_empty());
    }

    #[test]
    fn keys_are_printed_masked() {
        assert_eq!(masked(K1), "INPUTWEAVE-…7E42");
        assert_eq!(masked("ABCDEFGHIJKLMNOP"), "…MNOP");
        assert_eq!(masked("INPUTWEAVE-AB"), "…E-AB");
        assert_eq!(masked("ABCDEFGH"), "…");
        assert_eq!(masked(""), "");
    }

    #[test]
    fn the_status_debug_leaves_out_activations() {
        let mut license = licensed(K1, "act-secret");
        license.peer_said(said("win", K1, Standing::Licensed, "act-w-secret"));
        let text = format!("{:#?}", license.status(named, |_| true));
        assert!(!text.contains("secret"), "{text}");
        assert!(!text.contains("58D4"), "{text}");
        assert!(text.contains("Windows PC"), "{text}");
    }

    #[test]
    fn the_file_round_trips_and_drops_what_it_cannot_hold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("license.toml");
        let mut license = licensed(K1, "act");
        license.peer_said(said("win", K1, Standing::Licensed, "act-w"));
        license.peer_said(PeerLicense::legacy("old", K1, false).unwrap());
        license.peer_said(from_apply("desk", K1, Standing::Pending, "", "a9"));
        license.revoke("win", "act-w", false).unwrap();
        license.apply("win").unwrap();
        license.apply("mini").unwrap();
        license.mark_sent("mini");
        license.revoke("desk", "", false).unwrap();
        license.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches(K1).count(), 1, "{text}");
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
        let edited = LicenseFile::parse(&format!(
            "key = \"has space\"\nstate = \"licensed\"\n\
             [[peers]]\nid = \"win\"\nkey_match = \"short\"\nstanding = \"licensed\"\n\
             [[peers]]\nid = \"mini\"\nkey_match = \"{}\"\nstanding = \"from_a_newer_agent\"\n\
             [[peers]]\nkey_match = \"{}\"\n\
             [[owed]]\nid = \"win\"\naction = \"apply\"\n\
             [[owed]]\nid = \"mini\"\naction = \"revoke\"\nkey_match = \"bad\"\n\
             [[owed]]\nid = \"mini\"\naction = \"something_newer\"\n",
            crate::hex::hex(&key_match(K1)),
            crate::hex::hex(&key_match(K1))
        ))
        .unwrap();
        // The key cannot be one, so there is nothing to apply; the bad
        // entries are gone; a standing this build does not know is none.
        assert_eq!(edited.key, "");
        assert!(edited.owed.is_empty());
        assert_eq!(edited.peers.len(), 1);
        assert_eq!(edited.peers[0].id, "mini");
        assert_eq!(edited.peers[0].standing, Standing::None);
    }

    #[test]
    fn a_file_from_1_0_reads_with_its_keys_in_match_form() {
        let file = LicenseFile::parse(&format!(
            "key = \"{K1}\"\nstate = \"licensed\"\nactivation_id = \"act\"\n\
             source_device_id = \"win\"\nsource_device_name = \"Windows PC\"\n\
             opted_out = true\nrefused = [\"{K2}\"]\n\
             [[peers]]\nid = \"win\"\nkey = \"{K1}\"\nlicensed = true\n\
             [[peers]]\nid = \"mini\"\nkey = \"{K1}\"\n\
             [[peers]]\nid = \"bad\"\nkey = \"bad key\"\nlicensed = true\n"
        ))
        .unwrap();
        // Synced from the PC under 1.0: still licensed, from it.
        assert_eq!(file.state, LicenseState::Licensed);
        assert_eq!(file.source_device_id, "win");
        assert_eq!(
            file.peers,
            [
                PeerLicense::legacy("mini", K1, false).unwrap(),
                PeerLicense::legacy("win", K1, true).unwrap(),
            ]
        );
        assert_eq!(file.use_of("win", false), LicenseUse::Licensed);
        // Waiting for activation, as it last said: it activates once it is
        // back.
        assert_eq!(file.use_of("mini", false), LicenseUse::Waiting);
        assert_eq!(file.use_of("mini", true), LicenseUse::Activating);
        // Neither the keys in the clear nor opted_out and refused are
        // written again.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("license.toml");
        file.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches(K1).count(), 1, "{text}");
        assert!(!text.contains(K2), "{text}");
        assert!(!text.contains("opted_out"), "{text}");
    }

    #[test]
    fn an_unreadable_file_shows_as_unavailable() {
        let unavailable = shown(None, true, |_| None, |_| false).unwrap();
        assert_eq!(unavailable.state, LicenseState::Unavailable);
        assert_eq!(unavailable.key, "");
        assert_eq!(shown(None, false, |_| None, |_| false), None);
        let mut license = LicenseFile::default();
        license.set_key(K1).unwrap();
        assert_eq!(
            shown(Some(&license), false, |_| None, |_| false)
                .unwrap()
                .state,
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
        let file = LicenseFile::parse(&format!("key = \"{K1}\"\nstate = \"licensed\"\n")).unwrap();
        assert_eq!(file.state, LicenseState::PendingActivation);
        let file = LicenseFile::parse("state = \"licensed\"\n").unwrap();
        assert_eq!(file.state, LicenseState::None);
        // Revoked without an activation to free: nothing left.
        let file = LicenseFile::parse(&format!("key = \"{K1}\"\nstate = \"revoked\"\n")).unwrap();
        assert_eq!(file.state, LicenseState::None);
        assert_eq!(file.key, "");
        let file = LicenseFile::parse(&format!(
            "key = \"{K1}\"\nstate = \"revoked\"\nactivation_id = \"act\"\n"
        ))
        .unwrap();
        assert_eq!(file.state, LicenseState::Revoked);
    }

    #[test]
    fn a_state_from_a_newer_agent_with_an_activation_is_licensed() {
        let file = LicenseFile::parse(&format!(
            "key = \"{K1}\"\nstate = \"from_a_newer_agent\"\nactivation_id = \"act\"\n"
        ))
        .unwrap();
        assert_eq!(file.state, LicenseState::Licensed);
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
            Loaded::File(Box::default())
        );
        std::fs::write(&path, "key = [not toml").unwrap();
        assert_eq!(LicenseFile::load_or_set_aside(&path), Loaded::SetAside);
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("license.toml.bad")).unwrap(),
            "key = [not toml"
        );
        std::fs::write(&path, "validated_at = \"soon\"\n").unwrap();
        assert_eq!(LicenseFile::load_or_set_aside(&path), Loaded::SetAside);
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
