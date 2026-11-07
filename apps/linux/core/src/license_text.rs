//! What the License page, its sidebar row, the Enter License Key dialog,
//! About and the remove-device question say about the license, from
//! `Status` and what the [`Keeper`](crate::license::Keeper)'s outbox holds.
//! Each decision is a plain value (a row's standing and button, the remove
//! question's offer), so another page can show the same from it.

use iw_proto::app::v1::{
    ConnectionState, DeviceStatus, License, LicenseState, LicenseUse, LicensedDevice,
    RevokeLicense, Status,
};

use crate::license::{Keeper, Problem, Removal, short_email, state};
use crate::polar::Deactivation;
use crate::text::{UiText, keys};
use crate::ui;

/// How many computers one license is for.
pub const PLACES: usize = 5;

/// A note in the trial page, above its buttons.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// Every seat is taken: Manage Computers and Try Again replace the
    /// buttons.
    SeatsFull,
    /// The key was refused (unknown, revoked, refunded) or this computer's
    /// activation is gone.
    Invalid,
    /// The agent cannot read its license file: nothing to change.
    Unavailable,
    /// Remove License is on its way, not failed yet: a spinner, nothing to
    /// change meanwhile.
    Removing,
    /// The agent did not take Remove License; it is tried again.
    RemoveFailed,
    /// Removed here; this computer's seat is freed once Polar can be
    /// reached, or in the customer portal.
    SeatWaiting,
}

impl Notice {
    /// Whether the trial's buttons and "How the license works" give way to
    /// it.
    pub fn quiet(&self) -> bool {
        matches!(
            self,
            Notice::Unavailable | Notice::Removing | Notice::RemoveFailed
        )
    }
}

/// One row of "Computers on this license" from agents before 1.1, which
/// neither apply nor revoke: this computer and the paired ones licensed
/// with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Computer {
    pub name: String,
    pub note: UiText,
}

/// How a computer stands on this computer's license.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// A green checkmark and "Licensed".
    Licensed,
    Trial,
    /// A small spinner and "Activating…".
    Activating,
    /// "Offline. Applies when it connects".
    Waiting,
    /// "No places left", in the warning color.
    NoPlaces,
    OtherLicense,
    /// Applied from here while this computer is not licensed: "Applies once
    /// this computer is licensed again".
    WaitsForLicense,
}

impl Standing {
    pub fn text(self) -> UiText {
        ui!(match self {
            Standing::Licensed => keys::LICENSE_LICENSED,
            Standing::Trial => keys::LICENSE_TRIAL,
            Standing::Activating => keys::LICENSE_ACTIVATING,
            Standing::Waiting => keys::LICENSE_WAITING,
            Standing::NoPlaces => keys::LICENSE_NO_PLACES,
            Standing::OtherLicense => keys::LICENSE_OTHER,
            Standing::WaitsForLicense => keys::LICENSE_WAITS_FOR_LICENSE,
        })
    }
}

/// A row's button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowButton {
    /// Apply License: ApplyLicense. Disabled while every place is in use.
    Apply { enabled: bool },
    /// Revoke License…: asked first, then Polar is asked once to free the
    /// activation (empty when the device never said it) and the agent told.
    Revoke { activation_id: String },
    /// Don't Apply, while what was applied can be taken back: RevokeLicense
    /// without an activation, at once.
    DontApply,
}

impl RowButton {
    pub fn text(&self) -> UiText {
        ui!(match self {
            RowButton::Apply { .. } => keys::LICENSE_APPLY,
            RowButton::Revoke { .. } => keys::LICENSE_REVOKE,
            RowButton::DontApply => keys::LICENSE_DONT_APPLY,
        })
    }
}

/// What the line under a row's name says of the computer itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    ThisComputer,
    Connected,
    Offline,
    OfflineForNow,
}

/// One row of "Computers on this license" (agents from 1.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The device's id; this computer's own for the first row.
    pub id: String,
    pub name: String,
    /// "macos", "windows", "linux" or empty (this computer's is empty).
    pub os: String,
    pub presence: Presence,
    pub standing: Standing,
    /// It runs InputWeave 1.0.
    pub legacy: bool,
    /// On InputWeave 1.0, it did not take the license applied from here.
    pub legacy_refused: bool,
    pub button: Option<RowButton>,
}

impl Row {
    /// The secondary lines under the name, in order: where the computer is
    /// (none while connected), then what its standing needs said (InputWeave
    /// 1.0, another license).
    pub fn lines(&self) -> Vec<UiText> {
        let mut lines = Vec::new();
        match self.presence {
            Presence::ThisComputer => lines.push(ui!(keys::LICENSE_THIS_COMPUTER)),
            Presence::Offline => lines.push(ui!(keys::CONNECTION_OFFLINE)),
            Presence::OfflineForNow => lines.push(ui!(keys::CONNECTION_OFFLINE_FOR_NOW)),
            Presence::Connected => {}
        }
        match self.standing {
            // Licensed, or holding the key, on InputWeave 1.0, which cannot
            // be told to give it up.
            Standing::Licensed
            | Standing::Activating
            | Standing::Waiting
            | Standing::WaitsForLicense
                if self.legacy && self.button.is_none() =>
            {
                lines.push(ui!(keys::LICENSE_LEGACY_NOTE))
            }
            Standing::Trial if self.legacy_refused => lines.push(ui!(keys::LICENSE_LEGACY_REFUSED)),
            Standing::OtherLicense => lines.push(ui!(keys::LICENSE_OTHER_NOTE)),
            _ => {}
        }
        lines
    }
}

/// "Computers on this license" while this computer is licensed with an
/// agent that applies and revokes the license.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applying {
    /// This computer first, then every computer this one trusts, in the
    /// agent's order.
    pub rows: Vec<Row>,
    /// "N of 5": this computer and the rows licensed.
    pub licensed: usize,
    /// This computer and the rows licensed, activating or waiting.
    pub places_used: usize,
    /// Every place is in use: Apply License is disabled, and the list says
    /// so under it.
    pub all_in_use: bool,
    /// A row found no place left: the warning box under the list.
    pub no_places: bool,
}

/// How device `id` stands on this computer's license: as `license.devices`
/// lists it, or else on trial.
pub fn license_use<'a>(license: &'a License, id: &str) -> (LicenseUse, Option<&'a LicensedDevice>) {
    let device = license.devices.iter().find(|d| d.id == id);
    let used = device
        .and_then(|d| LicenseUse::try_from(d.r#use).ok())
        .unwrap_or(LicenseUse::None);
    (used, device)
}

fn presence(device: &DeviceStatus) -> Presence {
    if device.offline_for_now {
        Presence::OfflineForNow
    } else if device.state == ConnectionState::Offline as i32 {
        Presence::Offline
    } else {
        Presence::Connected
    }
}

/// The rows of "Computers on this license", from `status` (this computer
/// is licensed): this computer first, then every device in
/// `Status.devices`, each as `Status.license.devices` says it stands.
pub fn applying(status: &Status) -> Applying {
    let fallback = License::default();
    let license = status.license.as_ref().unwrap_or(&fallback);
    let mut rows = vec![Row {
        id: status.this_device_id.clone(),
        name: status.this_device_name.clone(),
        os: String::new(),
        presence: Presence::ThisComputer,
        standing: Standing::Licensed,
        legacy: false,
        legacy_refused: false,
        button: None,
    }];
    let mut wanted = Vec::new();
    for device in &status.devices {
        let (used, listed) = license_use(license, &device.id);
        let legacy = listed.is_some_and(|d| d.legacy);
        let activation_id = listed.map_or("", |d| d.activation_id.as_str());
        let take_back = listed
            .is_some_and(|d| d.can_take_back)
            .then_some(RowButton::DontApply);
        let (standing, button) = match used {
            LicenseUse::None => (Standing::Trial, Some(RowButton::Apply { enabled: true })),
            LicenseUse::Licensed if legacy => (Standing::Licensed, None),
            LicenseUse::Licensed => (
                Standing::Licensed,
                Some(RowButton::Revoke {
                    activation_id: activation_id.to_owned(),
                }),
            ),
            LicenseUse::Activating => (Standing::Activating, take_back),
            LicenseUse::Waiting => (Standing::Waiting, take_back),
            LicenseUse::NoPlaces => (Standing::NoPlaces, Some(RowButton::Apply { enabled: true })),
            LicenseUse::OtherLicense => (Standing::OtherLicense, None),
        };
        wanted.push(used);
        rows.push(Row {
            id: device.id.clone(),
            name: device.name.clone(),
            os: device.os.clone(),
            presence: presence(device),
            standing,
            legacy,
            legacy_refused: listed.is_some_and(|d| d.legacy_refused),
            button,
        });
    }
    let licensed = 1 + wanted
        .iter()
        .filter(|u| **u == LicenseUse::Licensed)
        .count();
    let places_used = 1 + wanted
        .iter()
        .filter(|u| {
            matches!(
                u,
                LicenseUse::Licensed | LicenseUse::Activating | LicenseUse::Waiting
            )
        })
        .count();
    let all_in_use = places_used >= PLACES;
    if all_in_use {
        for row in &mut rows {
            if let Some(RowButton::Apply { enabled }) = &mut row.button {
                *enabled = false;
            }
        }
    }
    Applying {
        no_places: wanted.contains(&LicenseUse::NoPlaces),
        rows,
        licensed,
        places_used,
        all_in_use,
    }
}

/// "Waiting to Apply" on a computer not licensed now (seats full,
/// invalid, removing its license, …): the computers with an apply from here
/// they have not shown yet (`LicensedDevice.waiting_to_apply`), each with
/// Don't Apply while it can be taken back (otherwise none; one on
/// InputWeave 1.0 then says why). In `Status.devices` order.
pub fn waiting(status: &Status) -> Vec<Row> {
    let Some(license) = status.license.as_ref() else {
        return Vec::new();
    };
    status
        .devices
        .iter()
        .filter_map(|device| {
            let listed = license_use(license, &device.id)
                .1
                .filter(|d| d.waiting_to_apply)?;
            let button = listed.can_take_back.then_some(RowButton::DontApply);
            Some(Row {
                id: device.id.clone(),
                name: device.name.clone(),
                os: device.os.clone(),
                presence: presence(device),
                standing: Standing::WaitsForLicense,
                legacy: listed.legacy,
                legacy_refused: false,
                button,
            })
        })
        .collect()
}

/// The list under "Computers on this license".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Computers {
    /// Agents before 1.1: this computer and the paired ones licensed with
    /// it, with the note that only paired ones are listed.
    Listed(Vec<Computer>),
    /// Apply and revoke, with the note on how the license is applied.
    Applying(Applying),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    Trial {
        notice: Option<Notice>,
        /// The info box above the buttons: the paired computer named is
        /// licensed, and this one can have the license applied from there.
        elsewhere: Option<String>,
        /// "Waiting to Apply", above "How the license works" ([`waiting`]);
        /// empty while the license can't be read.
        waiting: Vec<Row>,
    },
    Licensed {
        /// "Licensed to s•••@example.com"; `None` without an address.
        to: Option<UiText>,
        /// Thanks, and where the license came from.
        line: UiText,
        computers: Computers,
        count: UiText,
    },
}

/// The paired computer a computer on trial is told to get the license
/// from: the first one `Status.license.devices` lists as licensed (with
/// this computer's key or another), unless it runs InputWeave 1.0, which
/// cannot apply it; named as this computer calls it.
pub fn licensed_elsewhere(status: &Status) -> Option<String> {
    let license = status.license.as_ref().filter(|l| l.apply_supported)?;
    let device = license.devices.iter().find(|d| {
        !d.legacy
            && matches!(
                LicenseUse::try_from(d.r#use),
                Ok(LicenseUse::Licensed | LicenseUse::OtherLicense)
            )
    })?;
    Some(name_in(status, &device.id, &device.name))
}

/// What this computer calls device `id`: its name in `Status.devices`, or
/// `listed` (as the license lists it), or its id.
fn name_in(status: &Status, id: &str, listed: &str) -> String {
    status
        .devices
        .iter()
        .find(|d| d.id == id && !d.name.is_empty())
        .map(|d| d.name.clone())
        .or_else(|| (!listed.is_empty()).then(|| listed.to_owned()))
        .unwrap_or_else(|| id.to_owned())
}

/// What the License page shows. While the outbox removes the license or
/// frees its activation, it no longer shows as licensed, even before the
/// agent took it; a license another device revoked here
/// (`LICENSE_STATE_REVOKED`) shows as trial, without a word.
pub fn view(status: &Status, keeper: &Keeper) -> View {
    let license = status.license.as_ref();
    let trial = |notice: Option<Notice>| View::Trial {
        elsewhere: licensed_elsewhere(status),
        waiting: if notice == Some(Notice::Unavailable) {
            Vec::new()
        } else {
            waiting(status)
        },
        notice,
    };
    let removal = keeper.removal();
    match removal {
        Removal::Removing => return trial(Some(Notice::Removing)),
        Removal::Failing => return trial(Some(Notice::RemoveFailed)),
        Removal::SeatWaiting | Removal::None => {}
    }
    let Some(license) = license.filter(|l| keeper.licensed(Some(l))) else {
        let notice = license
            .and_then(|l| match state(l) {
                // A key applied from another device that found every place
                // taken stays plainly on trial: that device says why.
                LicenseState::SeatsFull if l.source_device_id.is_empty() => Some(Notice::SeatsFull),
                LicenseState::Invalid => Some(Notice::Invalid),
                LicenseState::Unavailable => Some(Notice::Unavailable),
                _ => None,
            })
            .or((removal == Removal::SeatWaiting).then_some(Notice::SeatWaiting));
        return trial(notice);
    };
    let email = short_email(&license.customer_email);
    let line = if license.source_device_name.is_empty() {
        ui!(keys::LICENSE_THANKS)
    } else {
        ui!(
            keys::LICENSE_FROM_PAIRED,
            license.source_device_name.as_str()
        )
    };
    let (computers, count) = if license.apply_supported {
        let list = applying(status);
        let count = list.licensed;
        (Computers::Applying(list), count)
    } else {
        let mut listed = vec![Computer {
            name: status.this_device_name.clone(),
            note: ui!(keys::LICENSE_THIS_COMPUTER),
        }];
        listed.extend(
            license
                .devices
                .iter()
                .filter(|d| d.licensed)
                .map(|d| Computer {
                    name: d.name.clone(),
                    note: ui!(keys::LICENSE_PAIRED),
                }),
        );
        let count = listed.len();
        (Computers::Listed(listed), count)
    };
    View::Licensed {
        to: (!email.is_empty()).then(|| ui!(keys::LICENSE_LICENSED_TO, email)),
        line,
        count: ui!(keys::LICENSE_COUNT, count.to_string()),
        computers,
    }
}

/// What revoking a device's license comes to as it is removed (Remove
/// Device with "Also revoke its license").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevokeFirst {
    /// Ask Polar once to free `activation_id`, unless Polar asked the app
    /// to wait ([`Keeper::polar_waits`]), then RevokeLicense with it
    /// ([`revoke_after`]); the user is told when Polar did not free it.
    Deactivate { key: String, activation_id: String },
    /// RevokeLicense without an activation at once, not deactivated: what
    /// was applied is taken back. Nothing to free, nothing to tell.
    TakeBack,
    /// The same, for one offline that may hold the key already
    /// (`LicensedDevice.key_out_of_reach`): the take-back cannot reach it
    /// once it is removed, so the user is told it could not be revoked.
    OutOfReach,
    /// Licensed without an activation it said: RevokeLicense without one,
    /// not deactivated, which the agent turns into taking the key back; the
    /// user is told it could not be revoked unless the device was connected
    /// to hear it.
    Unknown,
}

/// What the remove-device question offers about the device's license.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoveOffer {
    /// Nothing: this computer is on trial, or the device is not on its
    /// license. Remove only unpairs.
    Unpair,
    /// The device runs InputWeave 1.0 and uses the license: no checkbox, a
    /// line between the body and the buttons ([`RemoveOffer::line`]);
    /// Remove only unpairs.
    Legacy { name: String },
    /// "Also revoke its license", checked by default, with its note under
    /// it; checked, Remove does `RevokeFirst` before it unpairs.
    Revoke(RevokeFirst),
}

impl RemoveOffer {
    /// The line between the body and the buttons, if any.
    pub fn line(&self) -> Option<UiText> {
        match self {
            RemoveOffer::Legacy { name } => Some(ui!(keys::REMOVE_DEVICE_LEGACY, name.as_str())),
            _ => None,
        }
    }
}

/// What the question that removes device `id` offers; `licensed_here` is
/// whether this computer shows as licensed (not on trial, not removing its
/// license, [`Keeper::licensed`]).
pub fn remove_offer(status: &Status, licensed_here: bool, id: &str) -> RemoveOffer {
    let Some(license) = status
        .license
        .as_ref()
        .filter(|l| licensed_here && l.apply_supported)
    else {
        return RemoveOffer::Unpair;
    };
    let (used, listed) = license_use(license, id);
    if !matches!(
        used,
        LicenseUse::Licensed | LicenseUse::Activating | LicenseUse::Waiting
    ) {
        return RemoveOffer::Unpair;
    }
    let listed = listed.cloned().unwrap_or_default();
    if listed.legacy {
        return RemoveOffer::Legacy {
            name: name_in(status, id, &listed.name),
        };
    }
    RemoveOffer::Revoke(match used {
        LicenseUse::Licensed if !listed.activation_id.is_empty() && !license.key.is_empty() => {
            RevokeFirst::Deactivate {
                key: license.key.clone(),
                activation_id: listed.activation_id,
            }
        }
        LicenseUse::Licensed => RevokeFirst::Unknown,
        LicenseUse::Waiting if listed.key_out_of_reach => RevokeFirst::OutOfReach,
        _ => RevokeFirst::TakeBack,
    })
}

/// Why a device removed with "Also revoke its license" kept its license,
/// for the notice after it is removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotRevoked {
    /// Licensed, and Polar did not free its activation.
    NotFreed,
    /// Offline holding the key, maybe to activate it later.
    Offline,
    /// Polar freed its activation, but the agent did not take the revoke:
    /// its place is free, but it was never told.
    NotTold,
}

impl NotRevoked {
    /// The notice's title, for the device `name`.
    pub fn title(self, name: &str) -> UiText {
        match self {
            NotRevoked::NotTold => ui!(keys::REMOVE_DEVICE_FREED_NOT_TOLD, name),
            NotRevoked::NotFreed | NotRevoked::Offline => {
                ui!(keys::REMOVE_DEVICE_NOT_REVOKED, name)
            }
        }
    }

    pub fn body(self) -> UiText {
        ui!(match self {
            NotRevoked::NotFreed => keys::REMOVE_DEVICE_NOT_REVOKED_BODY,
            NotRevoked::Offline => keys::REMOVE_DEVICE_OFFLINE_BODY,
            NotRevoked::NotTold => keys::REMOVE_DEVICE_FREED_NOT_TOLD_BODY,
        })
    }

    /// Whether the notice offers Manage Computers… beside OK: only while a
    /// place may still be taken.
    pub fn manage(self) -> bool {
        self != NotRevoked::NotTold
    }
}

/// Whether, and why, the user is told a device removed with its license
/// revoked (`first`, then `sent` to the agent) kept its license. `answer`
/// is the Status the agent answered with, `None` when it refused (any
/// code) or did not answer: the revoke never reached the device, and the
/// user is always told (that its place is free all the same when Polar
/// freed it).
pub fn not_revoked(
    first: &RevokeFirst,
    sent: &RevokeLicense,
    answer: Option<&Status>,
) -> Option<NotRevoked> {
    let Some(status) = answer else {
        return Some(match first {
            RevokeFirst::OutOfReach => NotRevoked::Offline,
            RevokeFirst::Deactivate { .. } if sent.deactivated => NotRevoked::NotTold,
            _ => NotRevoked::NotFreed,
        });
    };
    match first {
        RevokeFirst::OutOfReach => Some(NotRevoked::Offline),
        RevokeFirst::Deactivate { .. } if sent.deactivated => None,
        RevokeFirst::Deactivate { .. } => Some(NotRevoked::NotFreed),
        // Taken back from a device connected (offline for now keeps its
        // session): it hears it before it is removed.
        RevokeFirst::Unknown => {
            let connected = status.devices.iter().any(|d| {
                d.id == sent.device_id
                    && (d.offline_for_now || d.state == ConnectionState::Connected as i32)
            });
            (!connected).then_some(NotRevoked::NotFreed)
        }
        RevokeFirst::TakeBack => None,
    }
}

/// The RevokeLicense for device `device_id`, licensed with `activation_id`,
/// once Polar answered the deactivate of it (`None`: not asked, as Polar
/// asked the app to wait): deactivated when Polar freed it (2xx, or 404 as
/// already gone); otherwise the device frees it itself, and as a device is
/// removed the user is told its license could not be revoked.
pub fn revoke_after(
    device_id: &str,
    activation_id: &str,
    answer: Option<&Deactivation>,
) -> RevokeLicense {
    RevokeLicense {
        device_id: device_id.to_owned(),
        activation_id: activation_id.to_owned(),
        deactivated: answer.is_some_and(Deactivation::freed),
    }
}

/// Where the Enter License Key dialog stands for the key it sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// Waiting for the user, or for the agent to take the key.
    Idle,
    Activating,
    /// Shown under the field; `manage` adds Manage Computers. The dialog
    /// stops following the key.
    Error {
        text: UiText,
        manage: bool,
    },
    /// Shown under the field while it is tried again in the background
    /// (Polar can't be reached, the license can't be read): the dialog keeps
    /// following, and closes once the key is licensed.
    Waiting {
        text: UiText,
    },
    /// Licensed with the key: the dialog closes.
    Done,
}

/// The dialog's state once `entered` was sent to the agent.
pub fn entry(entered: &str, license: Option<&License>, problem: Option<Problem>) -> Entry {
    let licensed = license.is_some_and(|l| l.key == entered && state(l) == LicenseState::Licensed);
    match problem {
        Some(Problem::NoService) => {
            return Entry::Waiting {
                text: ui!(keys::COMMAND_FAILED),
            };
        }
        Some(Problem::Unsaved) if !licensed => {
            return Entry::Waiting {
                text: ui!(keys::LICENSE_ERR_RECORD),
            };
        }
        _ => {}
    }
    if license.is_some_and(|l| state(l) == LicenseState::Unavailable) {
        return Entry::Waiting {
            text: ui!(keys::LICENSE_ERR_UNAVAILABLE),
        };
    }
    let Some(license) = license.filter(|l| l.key == entered) else {
        return Entry::Idle;
    };
    if problem == Some(Problem::Refused) && state(license) != LicenseState::Licensed {
        return Entry::Error {
            text: ui!(keys::LICENSE_ERR_RECORD),
            manage: false,
        };
    }
    match state(license) {
        LicenseState::Licensed => Entry::Done,
        LicenseState::SeatsFull => Entry::Error {
            text: ui!(keys::LICENSE_ERR_FULL),
            manage: true,
        },
        LicenseState::Invalid => Entry::Error {
            text: ui!(keys::LICENSE_ERR_INVALID),
            manage: false,
        },
        LicenseState::PendingActivation => match problem {
            Some(Problem::Offline) => Entry::Waiting {
                text: ui!(keys::LICENSE_ERR_OFFLINE),
            },
            Some(Problem::Refused | Problem::Unsaved | Problem::NoService) | None => {
                Entry::Activating
            }
        },
        LicenseState::None | LicenseState::Unavailable | LicenseState::Revoked => Entry::Idle,
    }
}

/// The text for a refusal of SetLicenseKey, or for no answer.
pub fn refused_key(code: Option<&str>) -> UiText {
    match code {
        Some("invalid_license_key") => ui!(keys::LICENSE_ERR_INVALID),
        Some("license_unavailable") => ui!(keys::LICENSE_ERR_UNAVAILABLE),
        _ => ui!(keys::LICENSE_ERR_RECORD),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iw_proto::app::v1::LicensedDevice;

    const KEY: &str = "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42";

    /// The page as an agent with `license` and no paired devices shows it
    /// on the computer `this_name`.
    fn view(license: Option<&License>, this_name: &str, keeper: &Keeper) -> View {
        super::view(
            &Status {
                this_device_id: "this".into(),
                this_device_name: this_name.into(),
                license: license.cloned(),
                ..Default::default()
            },
            keeper,
        )
    }

    fn trial(notice: Option<Notice>) -> View {
        View::Trial {
            notice,
            elsewhere: None,
            waiting: Vec::new(),
        }
    }

    fn license(state: LicenseState) -> License {
        License {
            key: KEY.into(),
            state: state as i32,
            ..Default::default()
        }
    }

    fn removing(l: &License) -> Keeper {
        let mut k = Keeper::default();
        k.remove(Some(l));
        k
    }

    fn failing() -> Keeper {
        let now = std::time::SystemTime::now();
        let mut k = Keeper::default();
        k.remove(None);
        k.next(None, "", false, true, now);
        k.removed(Err(None), now);
        k
    }

    /// Removed here; the seat of activation "old" still to free.
    fn seat_waiting() -> Keeper {
        Keeper::restored(&format!(
            r#"{{"outbox":[{{"free":{{"key":"{KEY}","activation_id":"old","seat":true}}}}]}}"#
        ))
        .expect("an outbox")
    }

    #[test]
    fn trial_with_the_note_the_state_needs() {
        let k = Keeper::default();
        assert_eq!(view(None, "box", &k), trial(None));
        for (s, notice) in [
            (LicenseState::None, None),
            (LicenseState::PendingActivation, None),
            (LicenseState::SeatsFull, Some(Notice::SeatsFull)),
            (LicenseState::Invalid, Some(Notice::Invalid)),
            (LicenseState::Unavailable, Some(Notice::Unavailable)),
            (LicenseState::Revoked, None),
        ] {
            assert_eq!(view(Some(&license(s)), "box", &k), trial(notice), "{s:?}");
        }
        // A seat still to free shows until it is freed, unless the record
        // has a note of its own.
        assert_eq!(seat_waiting().removal(), Removal::SeatWaiting);
        assert_eq!(
            view(None, "box", &seat_waiting()),
            trial(Some(Notice::SeatWaiting))
        );
        assert_eq!(
            view(
                Some(&license(LicenseState::SeatsFull)),
                "box",
                &seat_waiting()
            ),
            trial(Some(Notice::SeatsFull))
        );
        assert!(!Notice::SeatWaiting.quiet());
        assert!(Notice::RemoveFailed.quiet());
        assert!(!k.licensed(Some(&license(LicenseState::SeatsFull))));
    }

    #[test]
    fn licensed_lists_this_computer_and_the_licensed_paired_ones() {
        let l = License {
            activation_id: "a".into(),
            customer_email: "someone@example.com".into(),
            source_device_name: "MacBook Pro".into(),
            devices: vec![
                LicensedDevice {
                    id: "mac".into(),
                    name: "MacBook Pro".into(),
                    licensed: true,
                    r#use: LicenseUse::Licensed as i32,
                    ..Default::default()
                },
                LicensedDevice {
                    id: "pc".into(),
                    name: "Windows Desktop".into(),
                    licensed: false,
                    r#use: LicenseUse::Activating as i32,
                    ..Default::default()
                },
            ],
            ..license(LicenseState::Licensed)
        };
        let k = Keeper::default();
        match view(Some(&l), "ubuntu-desk", &k) {
            View::Licensed {
                to,
                line,
                computers: Computers::Listed(computers),
                count,
            } => {
                assert_eq!(to, Some(ui!(keys::LICENSE_LICENSED_TO, "s•••@example.com")));
                assert_eq!(line, ui!(keys::LICENSE_FROM_PAIRED, "MacBook Pro"));
                assert_eq!(
                    computers
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>(),
                    ["ubuntu-desk", "MacBook Pro"]
                );
                assert_eq!(computers[0].note.key, keys::LICENSE_THIS_COMPUTER);
                assert_eq!(count, ui!(keys::LICENSE_COUNT, "2"));
            }
            other => panic!("{other:?}"),
        }
        assert!(k.licensed(Some(&l)));
        assert!(!removing(&l).licensed(Some(&l)));
        assert_eq!(
            view(Some(&l), "box", &removing(&l)),
            trial(Some(Notice::Removing))
        );
        assert_eq!(
            view(Some(&l), "box", &failing()),
            trial(Some(Notice::RemoveFailed))
        );
        // Licensed again (another activation) while the old seat waits:
        // licensed; with that very activation being freed: not.
        assert!(matches!(
            view(Some(&l), "box", &seat_waiting()),
            View::Licensed { .. }
        ));
        let old = License {
            activation_id: "old".into(),
            ..l.clone()
        };
        assert!(!seat_waiting().licensed(Some(&old)));
        assert_eq!(
            view(Some(&old), "box", &seat_waiting()),
            trial(Some(Notice::SeatWaiting))
        );
        // Entered here: thanks only; no email, no "Licensed to".
        let here = License {
            source_device_name: String::new(),
            customer_email: String::new(),
            devices: Vec::new(),
            ..l
        };
        match view(Some(&here), "box", &k) {
            View::Licensed { to, line, .. } => {
                assert_eq!(to, None);
                assert_eq!(line, ui!(keys::LICENSE_THANKS));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_dialog_follows_the_key_it_sent() {
        let pending = license(LicenseState::PendingActivation);
        assert_eq!(entry(KEY, Some(&pending), None), Entry::Activating);
        assert_eq!(
            entry(KEY, Some(&pending), Some(Problem::Offline)),
            Entry::Waiting {
                text: ui!(keys::LICENSE_ERR_OFFLINE)
            }
        );
        assert!(matches!(
            entry(KEY, Some(&license(LicenseState::SeatsFull)), None),
            Entry::Error { manage: true, .. }
        ));
        assert!(matches!(
            entry(KEY, Some(&license(LicenseState::Invalid)), None),
            Entry::Error { manage: false, .. }
        ));
        assert_eq!(
            entry(KEY, Some(&license(LicenseState::Licensed)), None),
            Entry::Done
        );
        // Another key, or none yet: still waiting.
        assert_eq!(
            entry("OTHER-KEY-0000000000000", Some(&pending), None),
            Entry::Idle
        );
        assert_eq!(entry(KEY, None, None), Entry::Idle);
        // The agent did not record Polar's answer: that, not Polar's.
        for s in [LicenseState::PendingActivation, LicenseState::SeatsFull] {
            assert_eq!(
                entry(KEY, Some(&license(s)), Some(Problem::Refused)),
                Entry::Error {
                    text: ui!(keys::LICENSE_ERR_RECORD),
                    manage: false
                },
                "{s:?}"
            );
        }
        // An unreadable license, whatever the key.
        assert_eq!(
            entry(KEY, Some(&License::default()), None),
            Entry::Idle,
            "no record yet"
        );
        assert_eq!(
            entry(
                "OTHER-KEY-0000000000000",
                Some(&license(LicenseState::Unavailable)),
                None
            ),
            Entry::Waiting {
                text: ui!(keys::LICENSE_ERR_UNAVAILABLE)
            }
        );
    }

    #[test]
    fn the_dialog_says_when_the_service_or_the_disk_fails_it() {
        let pending = license(LicenseState::PendingActivation);
        assert_eq!(
            entry(KEY, Some(&pending), Some(Problem::NoService)),
            Entry::Waiting {
                text: ui!(keys::COMMAND_FAILED)
            }
        );
        assert_eq!(
            entry(KEY, Some(&pending), Some(Problem::Unsaved)),
            Entry::Waiting {
                text: ui!(keys::LICENSE_ERR_RECORD)
            }
        );
        assert_eq!(
            entry(
                KEY,
                Some(&license(LicenseState::Licensed)),
                Some(Problem::Unsaved)
            ),
            Entry::Done
        );
    }

    #[test]
    fn refusals_of_a_key() {
        assert_eq!(
            refused_key(Some("invalid_license_key")).key,
            keys::LICENSE_ERR_INVALID
        );
        assert_eq!(
            refused_key(Some("license_unavailable")).key,
            keys::LICENSE_ERR_UNAVAILABLE
        );
        assert_eq!(refused_key(None).key, keys::LICENSE_ERR_RECORD);
        assert_eq!(
            refused_key(Some("agent_stopping")).key,
            keys::LICENSE_ERR_RECORD
        );
    }

    fn device(id: &str, name: &str, offline: bool) -> DeviceStatus {
        DeviceStatus {
            id: id.into(),
            name: name.into(),
            os: "macos".into(),
            state: if offline {
                ConnectionState::Offline
            } else {
                ConnectionState::Connected
            } as i32,
            paired: true,
            ..Default::default()
        }
    }

    /// Device `id` as `License.devices` lists it; what was applied there
    /// can be taken back while it waits or activates.
    fn uses(id: &str, used: LicenseUse, activation_id: &str, legacy: bool) -> LicensedDevice {
        LicensedDevice {
            id: id.into(),
            name: format!("{id} as listed"),
            licensed: used == LicenseUse::Licensed,
            r#use: used as i32,
            activation_id: activation_id.into(),
            legacy,
            can_take_back: matches!(used, LicenseUse::Waiting | LicenseUse::Activating),
            key_out_of_reach: false,
            legacy_refused: false,
            waiting_to_apply: false,
        }
    }

    /// This computer licensed (1.1), with `devices` paired and `listed` on
    /// its license.
    fn status_with(devices: Vec<DeviceStatus>, listed: Vec<LicensedDevice>) -> Status {
        Status {
            this_device_id: "this".into(),
            this_device_name: "ubuntu-desk".into(),
            devices,
            license: Some(License {
                activation_id: "act-this".into(),
                apply_supported: true,
                devices: listed,
                ..license(LicenseState::Licensed)
            }),
            ..Default::default()
        }
    }

    fn texts(lines: &[UiText]) -> Vec<&'static str> {
        lines.iter().map(|t| t.key).collect()
    }

    #[test]
    fn each_row_shows_how_its_computer_stands() {
        let mut later = device("later", "Later PC", false);
        later.offline_for_now = true;
        later.state = ConnectionState::Offline as i32;
        let status = status_with(
            vec![
                device("mac", "MacBook Pro", false),
                device("pc", "Windows Desktop", true),
                device("imac", "iMac", false),
                device("studio", "Studio PC", true),
                device("old", "Old Mac", false),
                device("other", "Other Laptop", false),
                device("full", "Full PC", false),
                later,
            ],
            vec![
                uses("mac", LicenseUse::Licensed, "act-mac", false),
                uses("imac", LicenseUse::Activating, "", false),
                uses("studio", LicenseUse::Waiting, "", false),
                uses("old", LicenseUse::Licensed, "", true),
                uses("other", LicenseUse::OtherLicense, "", false),
                uses("full", LicenseUse::NoPlaces, "", false),
                uses("later", LicenseUse::Licensed, "act-later", false),
                // Not trusted: not a row.
                uses("gone", LicenseUse::Licensed, "act-gone", false),
            ],
        );
        let list = applying(&status);
        let rows: Vec<_> = list
            .rows
            .iter()
            .map(|r| (r.id.as_str(), r.standing, r.button.clone()))
            .collect();
        // Every place in use: nothing to apply.
        let apply = Some(RowButton::Apply { enabled: false });
        assert_eq!(
            rows,
            [
                ("this", Standing::Licensed, None),
                (
                    "mac",
                    Standing::Licensed,
                    Some(RowButton::Revoke {
                        activation_id: "act-mac".into()
                    })
                ),
                ("pc", Standing::Trial, apply.clone()),
                ("imac", Standing::Activating, Some(RowButton::DontApply)),
                ("studio", Standing::Waiting, Some(RowButton::DontApply)),
                ("old", Standing::Licensed, None),
                ("other", Standing::OtherLicense, None),
                ("full", Standing::NoPlaces, apply),
                (
                    "later",
                    Standing::Licensed,
                    Some(RowButton::Revoke {
                        activation_id: "act-later".into()
                    })
                ),
            ]
        );
        let lines: Vec<_> = list.rows.iter().map(|r| texts(&r.lines())).collect();
        assert_eq!(
            lines,
            [
                vec![keys::LICENSE_THIS_COMPUTER],
                vec![],
                vec![keys::CONNECTION_OFFLINE],
                vec![],
                vec![keys::CONNECTION_OFFLINE],
                vec![keys::LICENSE_LEGACY_NOTE],
                vec![keys::LICENSE_OTHER_NOTE],
                vec![],
                vec![keys::CONNECTION_OFFLINE_FOR_NOW],
            ]
        );
        assert_eq!(list.rows[0].name, "ubuntu-desk");
        assert_eq!(
            list.rows[1].name, "MacBook Pro",
            "as this computer calls it"
        );
        assert_eq!(list.rows[1].os, "macos");
        // This one, the Mac, the old Mac and the one offline for now.
        assert_eq!(list.licensed, 4);
        // And the iMac activating and the Studio PC waiting.
        assert_eq!(list.places_used, 6);
        assert!(list.all_in_use);
        assert!(list.no_places);
        assert_eq!(Standing::Waiting.text().key, keys::LICENSE_WAITING);
        assert_eq!(Standing::NoPlaces.text().key, keys::LICENSE_NO_PLACES);
        assert_eq!(RowButton::DontApply.text().key, keys::LICENSE_DONT_APPLY);
    }

    #[test]
    fn five_places_in_use_turn_apply_off() {
        let four = |last: LicenseUse| {
            status_with(
                vec![
                    device("a", "A", false),
                    device("b", "B", false),
                    device("c", "C", true),
                    device("d", "D", false),
                    device("e", "E", false),
                ],
                vec![
                    uses("a", LicenseUse::Licensed, "act-a", false),
                    uses("b", LicenseUse::Activating, "", false),
                    uses("c", LicenseUse::Waiting, "", false),
                    uses("d", last, "act-d", false),
                ],
            )
        };
        let list = applying(&four(LicenseUse::NoPlaces));
        assert_eq!((list.licensed, list.places_used), (2, 4));
        assert!(!list.all_in_use);
        assert_eq!(
            list.rows[5].button,
            Some(RowButton::Apply { enabled: true })
        );
        let list = applying(&four(LicenseUse::Licensed));
        assert_eq!((list.licensed, list.places_used), (3, 5));
        assert!(list.all_in_use);
        assert!(!list.no_places);
        assert_eq!(
            list.rows[5].button,
            Some(RowButton::Apply { enabled: false })
        );
        // The count is of licensed rows only.
        match super::view(&four(LicenseUse::Licensed), &Keeper::default()) {
            View::Licensed {
                count,
                computers: Computers::Applying(list),
                ..
            } => {
                assert_eq!(count, ui!(keys::LICENSE_COUNT, "3"));
                assert_eq!(list.rows.len(), 6);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_computer_on_trial_is_told_where_the_license_is_unless_that_runs_1_0() {
        let on_trial = |listed: Vec<LicensedDevice>, supported: bool| Status {
            devices: vec![device("mac", "MacBook Pro", false)],
            license: Some(License {
                apply_supported: supported,
                devices: listed,
                ..Default::default()
            }),
            ..Default::default()
        };
        let shown = |s: &Status| match super::view(s, &Keeper::default()) {
            View::Trial { elsewhere, .. } => elsewhere,
            other => panic!("{other:?}"),
        };
        assert_eq!(shown(&on_trial(vec![], true)), None);
        let old = uses("old", LicenseUse::OtherLicense, "", true);
        assert_eq!(shown(&on_trial(vec![old.clone()], true)), None, "1.0 only");
        let mac = uses("mac", LicenseUse::OtherLicense, "", false);
        assert_eq!(
            shown(&on_trial(vec![old.clone(), mac.clone()], true)),
            Some("MacBook Pro".into())
        );
        let pc = uses("pc", LicenseUse::Licensed, "act-pc", false);
        assert_eq!(
            shown(&on_trial(vec![pc.clone(), mac.clone()], true)),
            Some("pc as listed".into()),
            "the first, named as listed when it is not paired"
        );
        assert_eq!(
            shown(&on_trial(
                vec![uses("pc", LicenseUse::Activating, "", false)],
                true
            )),
            None
        );
        assert_eq!(shown(&on_trial(vec![mac], false)), None, "an older agent");
        assert_eq!(
            ui!(keys::LICENSE_ELSEWHERE, "MacBook Pro").args.len(),
            1,
            "the name, twice in the text"
        );
    }

    #[test]
    fn every_place_taken_is_said_only_for_a_key_entered_here() {
        let entered = license(LicenseState::SeatsFull);
        assert_eq!(
            view(Some(&entered), "box", &Keeper::default()),
            trial(Some(Notice::SeatsFull))
        );
        let applied = License {
            source_device_id: "mac".into(),
            source_device_name: "MacBook Pro".into(),
            ..entered
        };
        assert_eq!(view(Some(&applied), "box", &Keeper::default()), trial(None));
    }

    #[test]
    fn a_license_revoked_here_shows_as_trial_without_a_word_while_it_is_removed() {
        let revoked = License {
            activation_id: "act".into(),
            ..license(LicenseState::Revoked)
        };
        let mut k = Keeper::default();
        assert_eq!(view(Some(&revoked), "box", &k), trial(None));
        let now = std::time::SystemTime::now();
        assert!(matches!(
            k.next(Some(&revoked), "", true, true, now),
            Some(crate::license::Task::Free { .. })
        ));
        assert_eq!(view(Some(&revoked), "box", &k), trial(None));
        k.finished(
            crate::license::Done::Deactivated(Deactivation::Trouble(
                crate::polar::Trouble::Unreachable("x".into()),
            )),
            now,
        );
        assert_eq!(view(Some(&revoked), "box", &k), trial(None));
        let later = now + std::time::Duration::from_secs(3600);
        assert!(matches!(
            k.next(Some(&revoked), "", true, true, later),
            Some(crate::license::Task::Free { .. })
        ));
        k.finished(crate::license::Done::Deactivated(Deactivation::Done), later);
        assert_eq!(
            k.next(Some(&revoked), "", true, true, later),
            Some(crate::license::Task::Remove {
                key: KEY.into(),
                activation_id: "act".into()
            })
        );
        assert_eq!(view(Some(&revoked), "box", &k), trial(None));
        k.removed(Err(None), later);
        assert_eq!(view(Some(&revoked), "box", &k), trial(None));
        assert_eq!(entry(KEY, Some(&revoked), None), Entry::Idle);
    }

    #[test]
    fn the_remove_question_offers_to_revoke_what_uses_this_license() {
        let status = status_with(
            vec![
                device("mac", "MacBook Pro", false),
                device("pc", "Windows Desktop", true),
            ],
            vec![
                uses("mac", LicenseUse::Licensed, "act-mac", false),
                uses("old", LicenseUse::Licensed, "", true),
                uses("bare", LicenseUse::Licensed, "", false),
                uses("imac", LicenseUse::Activating, "", false),
                uses("studio", LicenseUse::Waiting, "", false),
                uses("old-waiting", LicenseUse::Waiting, "", true),
                uses("full", LicenseUse::NoPlaces, "", false),
                uses("other", LicenseUse::OtherLicense, "", false),
            ],
        );
        let offer = |id: &str| remove_offer(&status, true, id);
        assert_eq!(
            offer("mac"),
            RemoveOffer::Revoke(RevokeFirst::Deactivate {
                key: KEY.into(),
                activation_id: "act-mac".into()
            })
        );
        assert_eq!(offer("bare"), RemoveOffer::Revoke(RevokeFirst::Unknown));
        for id in ["imac", "studio"] {
            assert_eq!(
                offer(id),
                RemoveOffer::Revoke(RevokeFirst::TakeBack),
                "{id}"
            );
        }
        assert_eq!(
            offer("old"),
            RemoveOffer::Legacy {
                name: "old as listed".into()
            }
        );
        assert!(matches!(offer("old-waiting"), RemoveOffer::Legacy { .. }));
        assert_eq!(
            offer("old").line(),
            Some(ui!(keys::REMOVE_DEVICE_LEGACY, "old as listed"))
        );
        for id in ["pc", "full", "other", "unknown"] {
            assert_eq!(offer(id), RemoveOffer::Unpair, "{id}");
        }
        assert_eq!(offer("mac").line(), None);
        // On trial, or with an agent before 1.1: only unpairing.
        assert_eq!(remove_offer(&status, false, "mac"), RemoveOffer::Unpair);
        let mut older = status.clone();
        if let Some(l) = older.license.as_mut() {
            l.apply_supported = false;
        }
        assert_eq!(remove_offer(&older, true, "mac"), RemoveOffer::Unpair);
        assert_eq!(
            remove_offer(&Status::default(), true, "mac"),
            RemoveOffer::Unpair
        );
    }

    #[test]
    fn removing_a_device_tells_why_its_license_was_not_revoked() {
        let offline_with_key = LicensedDevice {
            key_out_of_reach: true,
            ..uses("studio", LicenseUse::Waiting, "", false)
        };
        let status = status_with(
            vec![
                device("mac", "MacBook Pro", false),
                device("studio", "Studio PC", true),
                device("pc", "Windows Desktop", true),
                device("imac", "iMac", false),
            ],
            vec![
                uses("mac", LicenseUse::Licensed, "act-mac", false),
                offline_with_key,
                uses("pc", LicenseUse::Waiting, "", false),
                uses("imac", LicenseUse::Activating, "", false),
            ],
        );
        let first = |id: &str| match remove_offer(&status, true, id) {
            RemoveOffer::Revoke(first) => first,
            other => panic!("{id}: {other:?}"),
        };
        assert_eq!(first("studio"), RevokeFirst::OutOfReach);
        assert_eq!(
            first("pc"),
            RevokeFirst::TakeBack,
            "the key never reached it"
        );
        assert_eq!(first("imac"), RevokeFirst::TakeBack);
        let licensed_mac = first("mac");
        let told = |first: &RevokeFirst, deactivated: bool| {
            not_revoked(
                first,
                &RevokeLicense {
                    device_id: "mac".into(),
                    deactivated,
                    ..Default::default()
                },
                Some(&status),
            )
        };
        assert_eq!(told(&licensed_mac, false), Some(NotRevoked::NotFreed));
        assert_eq!(told(&licensed_mac, true), None);
        // Never said its activation: the agent takes the key back, which
        // reaches it while it is connected, or offline for now.
        assert_eq!(told(&RevokeFirst::Unknown, false), None, "connected");
        let unknown_on = |id: &str, status: &Status| {
            not_revoked(
                &RevokeFirst::Unknown,
                &RevokeLicense {
                    device_id: id.into(),
                    ..Default::default()
                },
                Some(status),
            )
        };
        assert_eq!(
            unknown_on("studio", &status),
            Some(NotRevoked::NotFreed),
            "offline"
        );
        assert_eq!(unknown_on("gone", &status), Some(NotRevoked::NotFreed));
        let mut later = status.clone();
        if let Some(d) = later.devices.iter_mut().find(|d| d.id == "studio") {
            d.offline_for_now = true;
        }
        assert_eq!(unknown_on("studio", &later), None, "offline for now");
        assert_eq!(
            told(&RevokeFirst::OutOfReach, false),
            Some(NotRevoked::Offline)
        );
        assert_eq!(told(&RevokeFirst::TakeBack, false), None);
        // The agent refused the revoke, or did not answer: told whatever
        // Polar did; the offline body stays for the one offline with the
        // key.
        let refused = |first: &RevokeFirst, deactivated: bool| {
            not_revoked(
                first,
                &RevokeLicense {
                    device_id: "mac".into(),
                    deactivated,
                    ..Default::default()
                },
                None,
            )
        };
        for (first, deactivated) in [
            (licensed_mac.clone(), false),
            (RevokeFirst::Unknown, false),
            (RevokeFirst::TakeBack, false),
        ] {
            assert_eq!(
                refused(&first, deactivated),
                Some(NotRevoked::NotFreed),
                "{first:?} {deactivated}"
            );
        }
        // Polar freed it, but the agent did not take the revoke: its place
        // is free, it was not told; OK only.
        assert_eq!(refused(&licensed_mac, true), Some(NotRevoked::NotTold));
        assert_eq!(
            NotRevoked::NotTold.title("MacBook Pro"),
            ui!(keys::REMOVE_DEVICE_FREED_NOT_TOLD, "MacBook Pro")
        );
        assert_eq!(
            NotRevoked::NotTold.body().key,
            keys::REMOVE_DEVICE_FREED_NOT_TOLD_BODY
        );
        assert!(!NotRevoked::NotTold.manage());
        assert!(NotRevoked::NotFreed.manage() && NotRevoked::Offline.manage());
        assert_eq!(
            refused(&RevokeFirst::OutOfReach, false),
            Some(NotRevoked::Offline)
        );
        assert_eq!(
            NotRevoked::NotFreed.body().key,
            keys::REMOVE_DEVICE_NOT_REVOKED_BODY
        );
        assert_eq!(
            NotRevoked::Offline.body().key,
            keys::REMOVE_DEVICE_OFFLINE_BODY
        );
        assert_eq!(
            NotRevoked::Offline.title("Studio PC"),
            ui!(keys::REMOVE_DEVICE_NOT_REVOKED, "Studio PC")
        );
    }

    #[test]
    fn a_computer_on_1_0_that_heard_the_key_cannot_be_taken_back() {
        let row = |used: LicenseUse, legacy: bool, can_take_back: bool| {
            let listed = LicensedDevice {
                can_take_back,
                ..uses("mac", used, "", legacy)
            };
            let list = applying(&status_with(
                vec![device("mac", "MacBook Pro", false)],
                vec![listed],
            ));
            let row = list.rows[1].clone();
            (row.button.clone(), texts(&row.lines()))
        };
        let note = vec![keys::LICENSE_LEGACY_NOTE];
        for used in [LicenseUse::Activating, LicenseUse::Waiting] {
            assert_eq!(row(used, true, false), (None, note.clone()), "{used:?}");
            // The key never went to it: it can be taken back.
            assert_eq!(
                row(used, true, true),
                (Some(RowButton::DontApply), vec![]),
                "{used:?}"
            );
            // A key its own user entered: nothing to take back, nothing
            // to say.
            assert_eq!(row(used, false, false), (None, vec![]), "{used:?}");
        }
        assert_eq!(row(LicenseUse::Licensed, true, false), (None, note));
        // Removing it: the 1.0 line, no check box.
        let status = status_with(
            vec![device("mac", "MacBook Pro", false)],
            vec![uses("mac", LicenseUse::Activating, "", true)],
        );
        assert!(matches!(
            remove_offer(&status, true, "mac"),
            RemoveOffer::Legacy { .. }
        ));
    }

    #[test]
    fn a_computer_on_1_0_that_did_not_take_the_key_is_on_trial_and_says_so() {
        let refused = LicensedDevice {
            legacy_refused: true,
            ..uses("mac", LicenseUse::None, "", true)
        };
        let list = applying(&status_with(
            vec![device("mac", "MacBook Pro", false)],
            vec![refused],
        ));
        let row = &list.rows[1];
        assert_eq!(row.standing, Standing::Trial);
        assert_eq!(row.button, Some(RowButton::Apply { enabled: true }));
        assert_eq!(texts(&row.lines()), [keys::LICENSE_LEGACY_REFUSED]);
        assert_eq!(list.places_used, 1, "takes no place");
    }

    #[test]
    fn applies_waiting_for_this_computer_show_while_it_is_not_licensed() {
        let owed = |id: &str, legacy: bool, can_take_back: bool| LicensedDevice {
            waiting_to_apply: true,
            can_take_back,
            ..uses(id, LicenseUse::Waiting, "", legacy)
        };
        let listed = vec![
            owed("mac", false, true),
            owed("pc", false, true),
            owed("old", true, false),
            owed("held", false, false),
            // Offline with the key it holds already: never waiting to apply.
            LicensedDevice {
                can_take_back: false,
                ..uses("own", LicenseUse::Waiting, "", false)
            },
            uses("other", LicenseUse::Licensed, "act", false),
        ];
        let status_in = |st: LicenseState| Status {
            devices: vec![
                device("mac", "MacBook Pro", false),
                device("pc", "Windows Desktop", true),
                device("old", "Old Mac", false),
                device("held", "Held PC", false),
                device("own", "Own Laptop", true),
                device("other", "Other", false),
            ],
            license: Some(License {
                activation_id: "act-this".into(),
                apply_supported: true,
                devices: listed.clone(),
                ..license(st)
            }),
            ..Default::default()
        };
        let shown = |st: LicenseState, keeper: &Keeper| match super::view(&status_in(st), keeper) {
            View::Trial { waiting, .. } => waiting,
            other => panic!("{other:?}"),
        };
        let fresh = Keeper::default();
        for st in [
            LicenseState::SeatsFull,
            LicenseState::Invalid,
            LicenseState::None,
            LicenseState::PendingActivation,
        ] {
            let rows = shown(st, &fresh);
            assert_eq!(
                rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
                ["mac", "pc", "old", "held"],
                "{st:?}: the ones waiting to apply, in order"
            );
            assert!(rows.iter().all(|r| r.standing == Standing::WaitsForLicense));
            // Don't Apply only while it can be taken back; a 1.0 computer
            // says why.
            assert_eq!(
                rows.iter().map(|r| r.button.clone()).collect::<Vec<_>>(),
                [
                    Some(RowButton::DontApply),
                    Some(RowButton::DontApply),
                    None,
                    None,
                ]
            );
            assert_eq!(texts(&rows[1].lines()), [keys::CONNECTION_OFFLINE]);
            assert_eq!(texts(&rows[2].lines()), [keys::LICENSE_LEGACY_NOTE]);
            assert!(rows[0].lines().is_empty());
            assert!(rows[3].lines().is_empty());
        }
        assert!(shown(LicenseState::Unavailable, &fresh).is_empty());
        // Removing this computer's license (still licensed meanwhile).
        let mut removing = Keeper::default();
        removing.remove(status_in(LicenseState::Licensed).license.as_ref());
        match super::view(&status_in(LicenseState::Licensed), &removing) {
            View::Trial {
                notice: Some(Notice::Removing),
                waiting,
                ..
            } => assert_eq!(waiting.len(), 4),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            Standing::WaitsForLicense.text().key,
            keys::LICENSE_WAITS_FOR_LICENSE
        );
        // Licensed: they are on the list that applies instead.
        assert!(matches!(
            super::view(&status_in(LicenseState::Licensed), &Keeper::default()),
            View::Licensed { .. }
        ));
    }

    #[test]
    fn what_can_be_taken_back_gets_dont_apply() {
        let with = |used: LicenseUse, can_take_back: bool| {
            let listed = LicensedDevice {
                can_take_back,
                ..uses("mac", used, "", false)
            };
            applying(&status_with(
                vec![device("mac", "MacBook Pro", false)],
                vec![listed],
            ))
            .rows[1]
                .button
                .clone()
        };
        for used in [LicenseUse::Activating, LicenseUse::Waiting] {
            assert_eq!(with(used, true), Some(RowButton::DontApply), "{used:?}");
            assert_eq!(with(used, false), None, "{used:?}");
        }
    }

    #[test]
    fn the_revoke_says_whether_polar_freed_the_activation() {
        use crate::polar::Trouble;
        let sent = |answer: Option<&Deactivation>| revoke_after("pc", "act", answer);
        assert_eq!(
            sent(Some(&Deactivation::Done)),
            RevokeLicense {
                device_id: "pc".into(),
                activation_id: "act".into(),
                deactivated: true,
            },
            "freed, or 404"
        );
        for answer in [
            Deactivation::Refused,
            Deactivation::Trouble(Trouble::Unreachable("x".into())),
            Deactivation::Trouble(Trouble::RateLimited(std::time::Duration::from_secs(5))),
        ] {
            let revoke = sent(Some(&answer));
            assert_eq!(revoke.activation_id, "act", "{answer:?}");
            assert!(!revoke.deactivated, "{answer:?}");
        }
        assert!(!sent(None).deactivated, "Polar asked to wait: not asked");
        assert_eq!(sent(None).activation_id, "act");
    }
}
