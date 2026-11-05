//! What the License page, its sidebar row, the Enter License Key dialog and
//! About say about the license, from `Status.license` and what the
//! [`Keeper`](crate::license::Keeper)'s outbox holds.

use iw_proto::app::v1::{License, LicenseState};

use crate::license::{Keeper, Problem, Removal, short_email, state};
use crate::text::{UiText, keys};
use crate::ui;

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

/// One row of "Computers on this license".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Computer {
    pub name: String,
    pub note: UiText,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    Trial {
        notice: Option<Notice>,
    },
    Licensed {
        /// "Licensed to s•••@example.com"; `None` without an address.
        to: Option<UiText>,
        /// Thanks, and where the license came from.
        line: UiText,
        computers: Vec<Computer>,
        count: UiText,
    },
}

/// What the License page shows; `this_name` is this computer's name. While
/// the outbox removes the license or frees its activation, it no longer
/// shows as licensed, even before the agent took it.
pub fn view(license: Option<&License>, this_name: &str, keeper: &Keeper) -> View {
    let removal = keeper.removal();
    match removal {
        Removal::Removing => {
            return View::Trial {
                notice: Some(Notice::Removing),
            };
        }
        Removal::Failing => {
            return View::Trial {
                notice: Some(Notice::RemoveFailed),
            };
        }
        Removal::SeatWaiting | Removal::None => {}
    }
    let Some(license) = license.filter(|l| keeper.licensed(Some(l))) else {
        let notice = license
            .and_then(|l| match state(l) {
                LicenseState::SeatsFull => Some(Notice::SeatsFull),
                LicenseState::Invalid => Some(Notice::Invalid),
                LicenseState::Unavailable => Some(Notice::Unavailable),
                _ => None,
            })
            .or((removal == Removal::SeatWaiting).then_some(Notice::SeatWaiting));
        return View::Trial { notice };
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
    let mut computers = vec![Computer {
        name: this_name.to_owned(),
        note: ui!(keys::LICENSE_THIS_COMPUTER),
    }];
    computers.extend(
        license
            .devices
            .iter()
            .filter(|d| d.licensed)
            .map(|d| Computer {
                name: d.name.clone(),
                note: ui!(keys::LICENSE_PAIRED),
            }),
    );
    View::Licensed {
        to: (!email.is_empty()).then(|| ui!(keys::LICENSE_LICENSED_TO, email)),
        line,
        count: ui!(keys::LICENSE_COUNT, computers.len().to_string()),
        computers,
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
        LicenseState::None | LicenseState::Unavailable => Entry::Idle,
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
        assert_eq!(view(None, "box", &k), View::Trial { notice: None });
        for (s, notice) in [
            (LicenseState::None, None),
            (LicenseState::PendingActivation, None),
            (LicenseState::SeatsFull, Some(Notice::SeatsFull)),
            (LicenseState::Invalid, Some(Notice::Invalid)),
            (LicenseState::Unavailable, Some(Notice::Unavailable)),
        ] {
            assert_eq!(
                view(Some(&license(s)), "box", &k),
                View::Trial { notice },
                "{s:?}"
            );
        }
        // A seat still to free shows until it is freed, unless the record
        // has a note of its own.
        assert_eq!(seat_waiting().removal(), Removal::SeatWaiting);
        assert_eq!(
            view(None, "box", &seat_waiting()),
            View::Trial {
                notice: Some(Notice::SeatWaiting)
            }
        );
        assert_eq!(
            view(
                Some(&license(LicenseState::SeatsFull)),
                "box",
                &seat_waiting()
            ),
            View::Trial {
                notice: Some(Notice::SeatsFull)
            }
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
                },
                LicensedDevice {
                    id: "pc".into(),
                    name: "Windows Desktop".into(),
                    licensed: false,
                },
            ],
            ..license(LicenseState::Licensed)
        };
        let k = Keeper::default();
        match view(Some(&l), "ubuntu-desk", &k) {
            View::Licensed {
                to,
                line,
                computers,
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
            View::Trial {
                notice: Some(Notice::Removing)
            }
        );
        assert_eq!(
            view(Some(&l), "box", &failing()),
            View::Trial {
                notice: Some(Notice::RemoveFailed)
            }
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
            View::Trial {
                notice: Some(Notice::SeatWaiting)
            }
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
}
