//! What the app does about this computer's license (docs/architecture.md
//! §11): the agent keeps the record and says where it stands
//! (`Status.license`); the app asks Polar when the record needs it and
//! reports each answer back (`RecordLicenseResult`). [`Keeper`] decides
//! which request comes next and what an answer means; it holds no network
//! or UI, so every rule is a unit test.
//!
//! The work is one outbox, run one request at a time, oldest first: each
//! entry waits on its own after a failure, and every entry can be sent
//! again without harm. What must survive the app quitting is kept in
//! [`Keeper::saved`]: an activation Polar granted that the agent has not
//! taken yet (reported first on the next launch, so a key is never
//! activated twice), the activations still to free with Polar, a Remove
//! License on its way, and the licenses revoked on other devices that the
//! agent has not taken yet. What the UI shows about this work (a problem
//! with a key, Remove License on its way, a seat still to free) is read off
//! the outbox and the agent's record, never kept beside them.
//!
//! Revoke License on another device ([`Keeper::revoke`]) asks Polar once to
//! free that device's activation (not while Polar asked the app to wait),
//! then tells the agent (RevokeLicense) whether Polar did; when it did not,
//! the device frees it itself. A license another device revoked here
//! (`LICENSE_STATE_REVOKED`) is removed without a word: its activation freed
//! with Polar first (tried again until Polar answers), then RemoveLicense,
//! each only while the record is still that revoked license.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iw_proto::app::v1::{
    License, LicenseActivated, LicenseRefusal, LicenseState, LicenseValidated, RecordLicenseResult,
    record_license_result,
};
use serde_json::{Value, json};

use crate::polar::{Activation, Answered, Deactivation, Trouble, Validation};

/// How often a licensed computer asks Polar again, while the License page
/// or About is open.
pub const VALIDATE_EVERY: Duration = Duration::from_secs(30 * 24 * 3600);
/// After a validate that got no answer, the next try waits this long.
const VALIDATE_RETRY: Duration = Duration::from_secs(3600);
/// A second 404 counts only this long after the first.
const SECOND_MISSING: Duration = Duration::from_secs(60);
/// The first wait after a request failed; it doubles up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);
/// What the agent takes as `validated_at`: from 2020 to a day ahead of the
/// clock.
const EARLIEST: u64 = 1_577_836_800;
const DAY: Duration = Duration::from_secs(24 * 3600);
/// At most this many activations wait to be freed; older ones are dropped
/// (they show in Polar's customer portal).
const MAX_FREES: usize = 16;
/// At most this many revokes are read back from a saved outbox.
const MAX_REVOKES: usize = 64;
/// At most this many activations the agent refused to hear were freed are
/// remembered (the oldest forgotten first); the agent lists only a few.
const MAX_TOLD: usize = 64;

/// This computer is licensed already: done when it is with this very
/// activation.
const ALREADY: &str = "already_licensed";
/// The license RemoveLicense names is no longer this computer's: for a
/// license revoked here, nothing left to remove; for the user's Remove
/// License, asked again for the license as it is now.
const STALE: &str = "license_stale";
/// How many times the user's Remove License is sent, at most, while the
/// agent keeps answering that the license changed meanwhile.
const REMOVE_TRIES: u32 = 3;
/// The agent's refusals of a report that are for now only: tried again
/// later, keeping the activation. Every other code it answers with, known
/// or not (one from a newer agent too), is for good: the activation is
/// freed, and the key is left alone until the user acts or the record
/// changes. No answer at all (an IPC failure, an answer without a code) is
/// tried again too.
const FOR_NOW: [&str; 2] = ["license_unavailable", "agent_stopping"];

/// A request to make, to Polar or to the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Task {
    Activate {
        key: String,
        label: String,
    },
    /// A validate without an activation, after `activate` answered 403:
    /// 200 means every seat is taken, 404 that the key is invalid.
    Probe {
        key: String,
    },
    Validate {
        key: String,
        activation_id: String,
    },
    /// Frees an activation with Polar.
    Free {
        key: String,
        activation_id: String,
    },
    /// Tells the agent what Polar said.
    Report(RecordLicenseResult),
    /// RemoveLicense to the agent, for the license `key` with
    /// `activation_id` as it was when the removal began (both empty: an
    /// earlier removal that did not keep them, removing whatever is there).
    /// Answered with [`Keeper::removed`].
    Remove {
        key: String,
        activation_id: String,
    },
    /// RevokeLicense to the agent: this computer's license is revoked on
    /// `device_id`, licensed there with `activation_id` (empty: what was
    /// applied is taken back), which Polar freed if `deactivated`. Answered
    /// with [`Keeper::revoked`].
    Revoke {
        device_id: String,
        activation_id: String,
        deactivated: bool,
    },
    /// LicenseActivationFreed to the agent: Polar freed `activation_id`,
    /// which it listed in `License.to_free`. Answered with
    /// [`Keeper::freed`].
    Freed {
        activation_id: String,
    },
}

/// Polar's answer to a [`Task`], as the thread that asked brings it back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Done {
    Activated(Answered<Activation>),
    Probed(Answered<Validation>),
    Validated(Answered<Validation>),
    Deactivated(Deactivation),
}

/// Why the activation of a key gets nowhere, for the Enter License Key
/// dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// Polar could not be reached (or answered as if it could not); it is
    /// tried again.
    Offline,
    /// The agent refused to record Polar's answer: nothing more is tried
    /// with this key until the user acts or the agent's record changes.
    Refused,
    /// The app cannot keep its pending work on disk (the file can't be read,
    /// or writing it keeps failing); it keeps trying.
    Unsaved,
    /// The background service has not answered for a while.
    NoService,
}

/// Where Remove License stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removal {
    None,
    /// On its way, not failed yet.
    Removing,
    /// The agent did not take it; it is tried again.
    Failing,
    /// Removed here; this computer's seat is freed with Polar once Polar
    /// can be reached (or in the customer portal).
    SeatWaiting,
}

/// When a failed request may be tried again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Retry {
    not_before: Option<SystemTime>,
    backoff: Option<Duration>,
    failed_at: Option<SystemTime>,
    /// The wait is Polar's (429 `Retry-After`): nothing shortens it.
    polars: bool,
}

impl Retry {
    fn due(&self, now: SystemTime) -> bool {
        self.not_before.is_none_or(|t| now >= t)
    }

    fn failed(&mut self, trouble: &Trouble, now: SystemTime) {
        self.failed_at = Some(now);
        self.polars = matches!(trouble, Trouble::RateLimited(_));
        let wait = match trouble {
            Trouble::RateLimited(after) => *after,
            Trouble::Unreachable(_) => {
                let next = self
                    .backoff
                    .map(|b| (b * 2).min(MAX_BACKOFF))
                    .unwrap_or(FIRST_BACKOFF);
                self.backoff = Some(next);
                next
            }
        };
        self.not_before = Some(now + wait);
    }
}

fn unreachable_agent() -> Trouble {
    Trouble::Unreachable("the agent did not answer".into())
}

/// One piece of work in the outbox.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Op {
    Activate {
        key: String,
    },
    Probe {
        key: String,
    },
    Validate {
        key: String,
        activation_id: String,
    },
    Report(RecordLicenseResult),
    /// `seat` when it is this computer's seat, freed by Remove License.
    Free {
        key: String,
        activation_id: String,
        seat: bool,
    },
    /// RemoveLicense, for the license `key` with `activation_id` as it was.
    Remove {
        key: String,
        activation_id: String,
    },
    /// Revoke License on another device, licensed there with
    /// `activation_id` (empty: take back what was applied). `deactivated` is
    /// `None` until Polar was asked once to free it with `key`, then whether
    /// it did; only then is the agent told.
    Revoke {
        key: String,
        device_id: String,
        activation_id: String,
        deactivated: Option<bool>,
    },
    /// An activation of an earlier key the agent lists to free
    /// (`License.to_free`), or the one of a license another device revoked
    /// here: freed with Polar (tried again until Polar answers), then,
    /// `freed`, the agent told if it lists it.
    Release {
        key: String,
        activation_id: String,
        freed: bool,
    },
}

impl Op {
    fn key(&self) -> Option<&str> {
        match self {
            Op::Activate { key }
            | Op::Probe { key }
            | Op::Validate { key, .. }
            | Op::Free { key, .. }
            | Op::Release { key, .. } => Some(key),
            Op::Report(r) => Some(&r.key),
            Op::Remove { .. } | Op::Revoke { .. } => None,
        }
    }

    /// Freeing an activation is between the app and Polar; everything else
    /// waits for the agent.
    fn needs_agent(&self) -> bool {
        !matches!(
            self,
            Op::Free { .. }
                | Op::Revoke {
                    deactivated: None,
                    ..
                }
                | Op::Release { freed: false, .. }
        )
    }

    /// Requests to the agent go one at a time, and so do Polar's, each
    /// apart from the other: a slow Polar never holds up the agent.
    fn to_agent(&self) -> bool {
        matches!(
            self,
            Op::Report(_)
                | Op::Remove { .. }
                | Op::Revoke {
                    deactivated: Some(_),
                    ..
                }
                | Op::Release { freed: true, .. }
        )
    }

    /// Remove License, or this computer's seat it frees.
    fn owed_removal(&self) -> bool {
        matches!(self, Op::Remove { .. } | Op::Free { seat: true, .. })
    }

    /// Kept in [`Keeper::saved`].
    fn kept(&self) -> bool {
        match self {
            Op::Free { .. } | Op::Remove { .. } | Op::Revoke { .. } | Op::Release { .. } => true,
            Op::Report(r) => granted(r).is_some(),
            _ => false,
        }
    }

    fn task(&self, label: &str) -> Task {
        match self {
            Op::Activate { key } => Task::Activate {
                key: key.clone(),
                label: label.to_owned(),
            },
            Op::Probe { key } => Task::Probe { key: key.clone() },
            Op::Validate { key, activation_id } => Task::Validate {
                key: key.clone(),
                activation_id: activation_id.clone(),
            },
            Op::Report(r) => Task::Report(r.clone()),
            Op::Free {
                key, activation_id, ..
            } => Task::Free {
                key: key.clone(),
                activation_id: activation_id.clone(),
            },
            Op::Remove { key, activation_id } => Task::Remove {
                key: key.clone(),
                activation_id: activation_id.clone(),
            },
            // Polar first, once.
            Op::Revoke {
                key,
                activation_id,
                deactivated: None,
                ..
            } => Task::Free {
                key: key.clone(),
                activation_id: activation_id.clone(),
            },
            Op::Revoke {
                device_id,
                activation_id,
                deactivated: Some(deactivated),
                ..
            } => Task::Revoke {
                device_id: device_id.clone(),
                activation_id: activation_id.clone(),
                deactivated: *deactivated,
            },
            Op::Release {
                key,
                activation_id,
                freed: false,
            } => Task::Free {
                key: key.clone(),
                activation_id: activation_id.clone(),
            },
            Op::Release {
                activation_id,
                freed: true,
                ..
            } => Task::Freed {
                activation_id: activation_id.clone(),
            },
        }
    }

    fn is_remove(&self) -> bool {
        matches!(self, Op::Remove { .. })
    }

    /// The activation it frees with Polar, while it has yet to.
    fn freeing(&self) -> Option<&str> {
        match self {
            Op::Free { activation_id, .. }
            | Op::Release {
                activation_id,
                freed: false,
                ..
            } => Some(activation_id),
            _ => None,
        }
    }
}

/// The activation a report carries, when it is one Polar granted.
fn granted(r: &RecordLicenseResult) -> Option<&LicenseActivated> {
    match &r.result {
        Some(record_license_result::Result::Activated(a)) => Some(a),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    id: u64,
    op: Op,
    retry: Retry,
    /// Failed tries since it was queued or the user asked again.
    failures: u32,
}

/// What a request's answer does to its entry.
enum After {
    Drop,
    Become(Op),
    Fail(Trouble),
    BecomeFailed(Op, Trouble),
}

/// A key the agent refused to record, and the state its record was in
/// then; it is not activated again until the user acts or the record
/// changes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stall {
    key: String,
    state: Option<i32>,
}

/// The first 404 to validate: the activation, when, and when to ask again
/// (a minute later, then backing off while Polar can't be reached).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Missing {
    activation_id: String,
    first: SystemTime,
    retry: Retry,
}

/// The app's side of the license.
#[derive(Debug, Default)]
pub struct Keeper {
    outbox: Vec<Entry>,
    last_id: u64,
    /// The entry whose request to Polar is on its way, and that request.
    to_polar: Option<(u64, Op)>,
    /// The same for the agent.
    to_agent: Option<(u64, Op)>,
    /// When validate was last queued.
    last_validate: Option<SystemTime>,
    missing: Option<Missing>,
    stalled: Option<Stall>,
    /// The seat Remove License frees once the agent took it.
    remove_seat: Option<(String, String)>,
    /// The key and activation of a license another device revoked here,
    /// while the Remove on its way is the one that removes it without a
    /// word, once that activation is freed: nothing about it shows.
    quiet: Option<(String, String)>,
    /// The user's Remove License the agent answered `license_stale`: how many
    /// times, and whether it waits to be asked again for the license the
    /// record holds now.
    stale_removals: u32,
    reread: bool,
    /// Polar asked the app to wait until then (429 `Retry-After`): a revoke
    /// does not ask it meanwhile, nor is an activation freed.
    polar_not_before: Option<SystemTime>,
    /// Activations the agent refused to hear were freed: not freed again
    /// while this keeper runs.
    told: VecDeque<String>,
    /// Something [`Keeper::saved`] holds changed.
    dirty: bool,
    /// See [`Keeper::keep_on_disk`].
    on_disk: bool,
}

impl Keeper {
    /// The next request, if one is due and its lane is free: one request
    /// to Polar and one to the agent can be on their way at once; call it
    /// again for the other. `visible` is whether the License page or About
    /// is on screen, the only time a licensed computer validates; `linked`
    /// whether the agent answers (`license` is its record then). Without it
    /// only activations are freed. A revoke waits until Polar has tried
    /// once to free its activation, and RemoveLicense until the revokes
    /// queued went out.
    pub fn next(
        &mut self,
        license: Option<&License>,
        label: &str,
        visible: bool,
        linked: bool,
        now: SystemTime,
    ) -> Option<Task> {
        if self.to_polar.is_some() && self.to_agent.is_some() {
            return None;
        }
        if linked {
            self.follow(license, visible, now);
        }
        let running = self.running();
        // A grant not written yet waits.
        let unwritten = self.on_disk && self.dirty;
        if self.polar_waits(now) {
            self.skip_polar_for_revokes();
        }
        let (polar_free, agent_free) = (self.to_polar.is_none(), self.to_agent.is_none());
        let revoking = self.queued(|op| matches!(op, Op::Revoke { .. }));
        let polar_waits = self.polar_waits(now);
        let revoked_here = |op: &Op| {
            self.quiet.as_ref().is_some_and(
                |(_, a)| matches!(op, Op::Release { activation_id, .. } if activation_id == a),
            )
        };
        let freeing_revoked = self
            .outbox
            .iter()
            .any(|e| revoked_here(&e.op) && e.op.freeing().is_some());
        let entry = self.outbox.iter().find(|e| {
            let releasing = matches!(e.op, Op::Release { freed: false, .. });
            let held = (unwritten && matches!(&e.op, Op::Report(r) if granted(r).is_some()))
                // Revokes go first: the agent takes none once it has no key;
                // a license revoked here is freed before it is removed, and
                // only as the agent's record still says it is revoked; once
                // freed, the removal is what is left of it.
                || (e.op.is_remove() && (revoking || freeing_revoked || self.reread))
                || (revoked_here(&e.op) && (!releasing || !linked))
                // Polar asked to wait.
                || (releasing && polar_waits);
            let lane_free = if e.op.to_agent() {
                agent_free
            } else {
                polar_free
            };
            !running.contains(&Some(e.id))
                && lane_free
                && (linked || !e.op.needs_agent())
                && e.retry.due(now)
                && !held
        })?;
        let task = entry.op.task(label);
        let claimed = Some((entry.id, entry.op.clone()));
        if entry.op.to_agent() {
            self.to_agent = claimed;
        } else {
            self.to_polar = claimed;
        }
        Some(task)
    }

    /// Whether Polar asked the app to wait (429 `Retry-After`) and the wait
    /// is not over: a revoke then does not ask it to free the activation,
    /// and the device frees it itself.
    pub fn polar_waits(&self, now: SystemTime) -> bool {
        self.polar_not_before.is_some_and(|t| now < t)
    }

    /// Polar's answer to a deactivate the caller made itself (removing a
    /// device): a wait it asks for holds for the revokes too.
    pub fn polar_answered(&mut self, answer: &Deactivation, now: SystemTime) {
        if let Deactivation::Trouble(trouble) = answer {
            self.polar_trouble(trouble, now);
        }
    }

    fn polar_trouble(&mut self, trouble: &Trouble, now: SystemTime) {
        if let Trouble::RateLimited(after) = trouble {
            let until = now + *after;
            if self.polar_not_before.is_none_or(|t| t < until) {
                self.polar_not_before = Some(until);
                self.dirty = true;
            }
        }
    }

    /// While Polar asks to wait, revokes not on their way to Polar go to
    /// the agent as not freed.
    fn skip_polar_for_revokes(&mut self) {
        let running = self.running();
        for e in &mut self.outbox {
            if let Op::Revoke { deactivated, .. } = &mut e.op
                && deactivated.is_none()
                && !running.contains(&Some(e.id))
            {
                *deactivated = Some(false);
                e.retry = Retry::default();
                self.dirty = true;
            }
        }
    }

    /// The entries whose requests are on their way.
    fn running(&self) -> [Option<u64>; 2] {
        [
            self.to_polar.as_ref().map(|(id, _)| *id),
            self.to_agent.as_ref().map(|(id, _)| *id),
        ]
    }

    /// Brings the outbox in line with the agent's record: drops what no
    /// longer fits it and queues what it needs.
    fn follow(&mut self, license: Option<&License>, visible: bool, now: SystemTime) {
        if let Some(stall) = self.stalled.clone() {
            let same =
                license.filter(|l| stall.key == l.key && stall.state.is_none_or(|s| s == l.state));
            self.set_stall(same.map(|l| Stall {
                key: stall.key,
                state: Some(l.state),
            }));
        }
        let (key, st) = license.map_or(("", LicenseState::None), |l| (l.key.as_str(), state(l)));
        let held = license
            .filter(|l| state(l) == LicenseState::Licensed)
            .map(|l| l.activation_id.as_str());
        let before = self.outbox.len();
        let running = self.running();
        let quiet = self.quiet.as_ref().map(|(_, a)| a.clone());
        let listed =
            |id: &str| license.is_some_and(|l| l.to_free.iter().any(|f| f.activation_id == id));
        self.outbox.retain(|e| {
            running.contains(&Some(e.id))
                || match &e.op {
                    Op::Activate { key: k } | Op::Probe { key: k } => {
                        st == LicenseState::PendingActivation && k == key
                    }
                    Op::Validate { activation_id, .. } => held == Some(activation_id.as_str()),
                    // The agent holds it already.
                    Op::Report(r) => {
                        granted(r).is_none_or(|a| held != Some(a.activation_id.as_str()))
                    }
                    // Freed: the agent is told if it lists it; a license
                    // revoked here is removed next instead.
                    Op::Release {
                        activation_id,
                        freed: true,
                        ..
                    } => quiet.as_ref() == Some(activation_id) || listed(activation_id),
                    Op::Free { .. }
                    | Op::Remove { .. }
                    | Op::Revoke { .. }
                    | Op::Release { .. } => true,
                }
        });
        if self.outbox.len() != before {
            self.dirty = true;
        }
        self.recheck_quiet(license);
        self.reread_removal(license);
        if let Some(license) = license {
            self.release_listed(license);
        }
        if self.queued(Op::is_remove) {
            return;
        }
        let Some(license) = license else {
            return;
        };
        match st {
            // Another device revoked it here and left its activation to
            // free: freed, then removed, without a word; once, as the Remove
            // waits (or until the record changes, after the agent refused
            // it for good).
            LicenseState::Revoked if !license.key.is_empty() && self.stalled.is_none() => {
                let revoked = (license.key.clone(), license.activation_id.clone());
                self.begin_removal(revoked.clone(), None, Some(revoked));
            }
            LicenseState::PendingActivation
                if !license.key.is_empty()
                    && self.stalled.is_none()
                    && !self.queued(|op| {
                        op.key() == Some(license.key.as_str())
                            && !matches!(op, Op::Free { .. } | Op::Release { .. })
                    }) =>
            {
                self.push(Op::Activate {
                    key: license.key.clone(),
                })
            }
            LicenseState::Licensed if visible && !license.activation_id.is_empty() => {
                let id = &license.activation_id;
                if self.queued(|op| {
                    matches!(op, Op::Validate { .. })
                        || matches!(op, Op::Report(r) if r.activation_id == *id)
                }) {
                    return;
                }
                // After a first 404: asked again when its own time comes.
                let second_check = match &self.missing {
                    Some(m) if m.activation_id == *id => {
                        if !m.retry.due(now) {
                            return;
                        }
                        true
                    }
                    _ => false,
                };
                let due = second_check
                    || (since(license.validated_at, now) >= VALIDATE_EVERY
                        && self.last_validate.is_none_or(|t| {
                            now.duration_since(t).unwrap_or_default() >= VALIDATE_RETRY
                        }));
                if due {
                    self.last_validate = Some(now);
                    self.push(Op::Validate {
                        key: license.key.clone(),
                        activation_id: id.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    /// Polar's answer to the request on its way to Polar.
    pub fn finished(&mut self, done: Done, now: SystemTime) {
        let Some((id, op)) = self.to_polar.take() else {
            return;
        };
        let after = match (op, done) {
            (Op::Activate { key }, Done::Activated(answer)) => match answer.outcome {
                Activation::Activated {
                    activation_id,
                    customer_email,
                } => {
                    if self.queued(Op::is_remove) {
                        // Remove License came meanwhile: this seat is not
                        // kept.
                        self.free(key, activation_id, false);
                        After::Drop
                    } else {
                        After::Become(Op::Report(RecordLicenseResult {
                            key,
                            result: Some(record_license_result::Result::Activated(
                                LicenseActivated {
                                    activation_id,
                                    customer_email,
                                    validated_at: stamp(answer.at, now),
                                },
                            )),
                            activation_id: String::new(),
                        }))
                    }
                }
                Activation::NotPermitted => After::Become(Op::Probe { key }),
                Activation::Invalid => {
                    After::Become(refusal(key, LicenseRefusal::Invalid, String::new()))
                }
                // Asking again could take a second seat, or change nothing.
                Activation::Rejected | Activation::Unreadable => self.stall(key),
                Activation::Trouble(trouble) => After::Fail(trouble),
            },
            (Op::Probe { key }, Done::Probed(answer)) => match answer.outcome {
                Validation::Valid { .. } => {
                    After::Become(refusal(key, LicenseRefusal::SeatsFull, String::new()))
                }
                Validation::NotFound => {
                    After::Become(refusal(key, LicenseRefusal::Invalid, String::new()))
                }
                Validation::Rejected => self.stall(key),
                // Seats may be free by the next try: activate then.
                Validation::Trouble(trouble) => After::BecomeFailed(Op::Activate { key }, trouble),
            },
            (Op::Validate { key, activation_id }, Done::Validated(answer)) => {
                match answer.outcome {
                    Validation::Valid { customer_email } => {
                        self.missing = None;
                        After::Become(Op::Report(RecordLicenseResult {
                            key,
                            result: Some(record_license_result::Result::Validated(
                                LicenseValidated {
                                    validated_at: stamp(answer.at, now),
                                    customer_email,
                                },
                            )),
                            activation_id,
                        }))
                    }
                    Validation::NotFound => match &self.missing {
                        Some(m)
                            if m.activation_id == activation_id
                                && now.duration_since(m.first).unwrap_or_default()
                                    >= SECOND_MISSING =>
                        {
                            self.missing = None;
                            After::Become(refusal(key, LicenseRefusal::Invalid, activation_id))
                        }
                        Some(m) if m.activation_id == activation_id => After::Drop,
                        _ => {
                            self.missing = Some(Missing {
                                activation_id,
                                first: now,
                                retry: Retry {
                                    not_before: Some(now + SECOND_MISSING),
                                    ..Retry::default()
                                },
                            });
                            After::Drop
                        }
                    },
                    // An unreachable Polar, or a request it could not read,
                    // changes nothing: validate is queued again an hour later
                    // (last_validate), the check after a first 404 once its
                    // backoff, or Polar's Retry-After, is over.
                    Validation::Trouble(trouble) => {
                        self.missed_again(&activation_id, &trouble, now);
                        After::Drop
                    }
                    Validation::Rejected => {
                        let trouble = Trouble::Unreachable("Polar could not read it".into());
                        self.missed_again(&activation_id, &trouble, now);
                        After::Drop
                    }
                }
            }
            (Op::Free { .. }, Done::Deactivated(answer)) => match answer {
                Deactivation::Done | Deactivation::Refused => After::Drop,
                Deactivation::Trouble(trouble) => {
                    self.polar_trouble(&trouble, now);
                    After::Fail(trouble)
                }
            },
            // Freed, or Polar will not free it: what is left is telling the
            // agent, or removing a license revoked here.
            (
                Op::Release {
                    key,
                    activation_id,
                    freed: false,
                },
                Done::Deactivated(answer),
            ) => match answer {
                Deactivation::Done | Deactivation::Refused => After::Become(Op::Release {
                    key,
                    activation_id,
                    freed: true,
                }),
                Deactivation::Trouble(trouble) => {
                    self.polar_trouble(&trouble, now);
                    After::Fail(trouble)
                }
            },
            // Asked once: the agent is told whether Polar freed it, and
            // the device frees it itself if not.
            (
                Op::Revoke {
                    key,
                    device_id,
                    activation_id,
                    deactivated: None,
                },
                Done::Deactivated(answer),
            ) => {
                if let Deactivation::Trouble(trouble) = &answer {
                    self.polar_trouble(trouble, now);
                }
                After::Become(Op::Revoke {
                    key,
                    device_id,
                    activation_id,
                    deactivated: Some(answer.freed()),
                })
            }
            (op, _) => {
                // An answer to another request than the one on its way:
                // keep waiting for the right one.
                self.to_polar = Some((id, op));
                return;
            }
        };
        self.settle(id, after, now);
    }

    /// The agent's answer to a [`Task::Report`]: `Ok`, its refusal code, or
    /// `Err(None)` when it could not be reached; `license` is its record
    /// now.
    pub fn reported(
        &mut self,
        answer: Result<(), Option<&str>>,
        license: Option<&License>,
        now: SystemTime,
    ) {
        let Some((id, op)) = self.to_agent.take() else {
            return;
        };
        let Op::Report(result) = op else {
            self.to_agent = Some((id, op));
            return;
        };
        let activation = granted(&result).map(|a| a.activation_id.clone());
        let holds = |a: &str| license.is_some_and(|l| is_licensed(Some(l)) && l.activation_id == a);
        let after = match answer {
            Ok(()) => {
                if let Some(a) = activation
                    && self.queued(Op::is_remove)
                {
                    // Taken just as Remove License began: that seat goes
                    // too, once the agent took RemoveLicense.
                    match self.remove_seat {
                        None => self.remove_seat = Some((result.key.clone(), a)),
                        Some(_) => self.free(result.key.clone(), a, false),
                    }
                    self.dirty = true;
                }
                After::Drop
            }
            Err(Some(ALREADY)) if activation.as_deref().is_some_and(holds) => After::Drop,
            Err(Some(code)) if !FOR_NOW.contains(&code) => {
                // Left alone for the record as it is at the refusal.
                self.set_stall(Some(Stall {
                    key: result.key.clone(),
                    state: license.filter(|l| l.key == result.key).map(|l| l.state),
                }));
                if let Some(a) = activation {
                    self.free(result.key.clone(), a, false);
                }
                After::Drop
            }
            Err(_) => After::Fail(unreachable_agent()),
        };
        self.settle(id, after, now);
    }

    /// Starts Remove License: RemoveLicense to the agent, then, once it took
    /// it, freeing this computer's seat with Polar when it has one;
    /// [`Keeper::next`] makes the requests. An activation the agent has not
    /// taken is freed too.
    pub fn remove(&mut self, license: Option<&License>) {
        if self.queued(Op::is_remove) {
            return;
        }
        let seat = license
            .filter(|l| is_licensed(Some(l)) && !l.activation_id.is_empty())
            .map(|l| (l.key.clone(), l.activation_id.clone()));
        let of = license.map_or((String::new(), String::new()), |l| {
            (l.key.clone(), l.activation_id.clone())
        });
        self.begin_removal(of, seat, None);
    }

    /// Queues RemoveLicense of the license `of` (its key and activation as
    /// they are now), and `seat` to free once the agent took it; `quiet`
    /// (the key and activation revoked here) for the removal of a license
    /// another device revoked here, after its activation is freed. What
    /// followed from the license and is not on its way is dropped; revokes
    /// on other devices go on.
    fn begin_removal(
        &mut self,
        of: (String, String),
        seat: Option<(String, String)>,
        quiet: Option<(String, String)>,
    ) {
        let running = self.running();
        let mut unsent = Vec::new();
        self.outbox.retain(|e| {
            if running.contains(&Some(e.id)) {
                return true;
            }
            match &e.op {
                Op::Free { .. } | Op::Remove { .. } | Op::Revoke { .. } | Op::Release { .. } => {
                    true
                }
                Op::Report(r) => {
                    if let Some(a) = granted(r) {
                        unsent.push((r.key.clone(), a.activation_id.clone()));
                    }
                    false
                }
                _ => false,
            }
        });
        for (key, activation_id) in unsent {
            self.free(key, activation_id, false);
        }
        // The removal waits for them: no longer than for the agent.
        for e in &mut self.outbox {
            if matches!(e.op, Op::Revoke { .. }) && !e.retry.polars {
                e.retry.not_before = None;
            }
        }
        self.remove_seat = seat;
        if let Some((key, activation_id)) = &quiet
            && !activation_id.is_empty()
            && !self.queued(|op| op.freeing() == Some(activation_id.as_str()))
        {
            self.push(Op::Release {
                key: key.clone(),
                activation_id: activation_id.clone(),
                freed: false,
            });
        }
        self.quiet = quiet;
        self.stale_removals = 0;
        self.reread = false;
        let (key, activation_id) = of;
        self.push(Op::Remove { key, activation_id });
        self.set_stall(None);
        self.missing = None;
        self.dirty = true;
    }

    /// The removal of a license revoked here goes on only while the record
    /// is still that revoked license (its key and activation). Once it is
    /// not (the same key applied or entered again: the agent lists the
    /// revoked activation to free and starts over; another key), the
    /// removal is dropped, but the activation is still freed, and the agent
    /// told once it lists it to free. An unreadable record changes nothing.
    fn recheck_quiet(&mut self, license: Option<&License>) {
        let Some((key, activation_id)) = self.quiet.clone() else {
            return;
        };
        let st = license.map_or(LicenseState::None, state);
        let still = license.is_some_and(|l| {
            l.key == key && l.activation_id == activation_id && st == LicenseState::Revoked
        });
        if still || st == LicenseState::Unavailable {
            return;
        }
        let running = self.running();
        self.outbox
            .retain(|e| running.contains(&Some(e.id)) || !e.op.is_remove());
        self.dirty = true;
        if self.queued(Op::is_remove) {
            // On its way already: its answer settles it.
            return;
        }
        self.quiet = None;
    }

    /// The user's Remove License after `license_stale`: asked again for the
    /// license the record holds now, its seat freed once the agent took it;
    /// done when the record holds none any more. An unreadable record
    /// waits.
    fn reread_removal(&mut self, license: Option<&License>) {
        if !self.reread {
            return;
        }
        let running = self.running();
        let st = license.map_or(LicenseState::None, state);
        if st == LicenseState::Unavailable {
            return;
        }
        let Some(i) = self
            .outbox
            .iter()
            .position(|e| e.op.is_remove() && !running.contains(&Some(e.id)))
        else {
            return;
        };
        self.reread = false;
        self.dirty = true;
        match license.filter(|l| !l.key.is_empty() && st != LicenseState::None) {
            None => {
                self.outbox.remove(i);
                self.stale_removals = 0;
            }
            Some(l) => {
                self.outbox[i].op = Op::Remove {
                    key: l.key.clone(),
                    activation_id: l.activation_id.clone(),
                };
                self.remove_seat = (is_licensed(Some(l)) && !l.activation_id.is_empty())
                    .then(|| (l.key.clone(), l.activation_id.clone()));
            }
        }
    }

    /// Queues freeing each activation the agent lists to free
    /// (`License.to_free`) that nothing frees yet, unless the agent refused
    /// to hear it was freed.
    fn release_listed(&mut self, license: &License) {
        for listed in &license.to_free {
            let id = listed.activation_id.as_str();
            if id.is_empty()
                || !valid_activation(id)
                || !plausible_key(&listed.key)
                || self.told.iter().any(|t| t == id)
                || self.queued(|op| {
                    op.freeing() == Some(id)
                        || matches!(op, Op::Release { activation_id, .. } if activation_id == id)
                })
            {
                continue;
            }
            self.push(Op::Release {
                key: listed.key.clone(),
                activation_id: id.to_owned(),
                freed: false,
            });
        }
    }

    /// The agent's answer to [`Task::Remove`]: true when it refused it for
    /// good (any code but the temporary ones and `license_stale`), so Remove
    /// License is dropped and the license stays, and the user is told once.
    /// `license_stale` (the license is no longer the one it removes) frees
    /// the seat it named as after a removal; the user's Remove License is
    /// then sent again for the license the record holds now (done if it
    /// holds none), [`REMOVE_TRIES`] times in all before it counts as
    /// refused for good; for a license revoked here it is done, and the
    /// activation it freed is told freed if the agent lists it to free. The
    /// removal of a license another device revoked here is never told:
    /// refused for good, it is not tried again until the record changes.
    pub fn removed(&mut self, answer: Result<(), Option<&str>>, now: SystemTime) -> bool {
        let Some((id, op)) = self.to_agent.take() else {
            return false;
        };
        if !op.is_remove() {
            self.to_agent = Some((id, op));
            return false;
        }
        let (after, refused) = match answer {
            Ok(()) => {
                // A license revoked here was freed before it was removed.
                if let Some((_, a)) = self.quiet.take() {
                    self.drop_released(&a);
                }
                if let Some((key, activation_id)) = self.remove_seat.take() {
                    self.free(key, activation_id, true);
                }
                self.stale_removals = 0;
                (After::Drop, false)
            }
            Err(Some(STALE)) => {
                if let Some((key, activation_id)) = self.remove_seat.take() {
                    self.free(key, activation_id, true);
                }
                if self.quiet.take().is_some() {
                    // What it freed stays queued as freed: told if listed.
                    (After::Drop, false)
                } else {
                    self.stale_removals += 1;
                    if self.stale_removals < REMOVE_TRIES {
                        self.reread = true;
                        (After::Become(op), false)
                    } else {
                        self.stale_removals = 0;
                        (After::Drop, true)
                    }
                }
            }
            Err(Some(code)) if !FOR_NOW.contains(&code) => {
                self.stale_removals = 0;
                self.remove_seat = None;
                match self.quiet.take() {
                    Some((key, a)) => {
                        self.drop_released(&a);
                        self.set_stall(Some(Stall {
                            key,
                            state: Some(LicenseState::Revoked as i32),
                        }));
                        (After::Drop, false)
                    }
                    None => (After::Drop, true),
                }
            }
            Err(_) => (After::Fail(unreachable_agent()), false),
        };
        self.settle(id, after, now);
        refused
    }

    /// Revoke License on device `device_id` (or Don't Apply, with no
    /// activation): Polar is asked once to free `activation_id`, its
    /// activation with this computer's key (`license`), unless Polar asked
    /// the app to wait; then the agent is told (RevokeLicense) whether Polar
    /// freed it, and the device frees it itself if not. Kept until the agent
    /// took it, tried again while it does not answer; [`Keeper::next`] makes
    /// the requests (a [`Task::Free`], then a [`Task::Revoke`]). Once per
    /// device and activation.
    pub fn revoke(&mut self, license: Option<&License>, device_id: &str, activation_id: &str) {
        if device_id.is_empty()
            || self.queued(|op| {
                matches!(op, Op::Revoke { device_id: d, activation_id: a, .. }
                    if d == device_id && a == activation_id)
            })
        {
            return;
        }
        let key = license.map_or("", |l| l.key.as_str());
        let ask_polar = !activation_id.is_empty() && !key.is_empty();
        self.push(Op::Revoke {
            key: key.to_owned(),
            device_id: device_id.to_owned(),
            activation_id: activation_id.to_owned(),
            deactivated: (!ask_polar).then_some(false),
        });
    }

    /// The agent's answer to [`Task::Freed`]: done once it took it, or
    /// refused it for good (it is not freed again); tried again only
    /// without an answer, or for a temporary refusal.
    pub fn freed(&mut self, answer: Result<(), Option<&str>>, now: SystemTime) {
        let Some((id, op)) = self.to_agent.take() else {
            return;
        };
        let Op::Release {
            activation_id,
            freed: true,
            ..
        } = &op
        else {
            self.to_agent = Some((id, op));
            return;
        };
        let after = match answer {
            Ok(()) => After::Drop,
            Err(Some(code)) if !FOR_NOW.contains(&code) => {
                if self.told.len() >= MAX_TOLD {
                    self.told.pop_front();
                }
                self.told.push_back(activation_id.clone());
                After::Drop
            }
            Err(_) => After::Fail(unreachable_agent()),
        };
        self.settle(id, after, now);
    }

    /// Drops what is left of freeing the activation of a license revoked
    /// here, once it is removed.
    fn drop_released(&mut self, activation_id: &str) {
        let running = self.running();
        self.outbox.retain(|e| {
            running.contains(&Some(e.id))
                || !matches!(&e.op, Op::Release { activation_id: a, .. } if a == activation_id)
        });
    }

    /// The agent's answer to [`Task::Revoke`], as [`Keeper::removed`] takes
    /// it: true when it refused it for good (any code but the temporary
    /// ones), so the revoke is dropped (the caller logs it; the row shows
    /// what the agent says). No answer, or a temporary refusal, is tried
    /// again.
    pub fn revoked(&mut self, answer: Result<(), Option<&str>>, now: SystemTime) -> bool {
        let Some((id, op)) = self.to_agent.take() else {
            return false;
        };
        if !matches!(op, Op::Revoke { .. }) {
            self.to_agent = Some((id, op));
            return false;
        }
        let (after, refused) = match answer {
            Ok(()) => (After::Drop, false),
            Err(Some(code)) if !FOR_NOW.contains(&code) => (After::Drop, true),
            Err(_) => (After::Fail(unreachable_agent()), false),
        };
        self.settle(id, after, now);
        refused
    }

    /// A request that could not be made at all (no thread): as if it got
    /// no answer.
    pub fn abandoned(&mut self, now: SystemTime) {
        if let Some((id, _)) = self.to_polar.take() {
            self.settle(
                id,
                After::Fail(Trouble::Unreachable("not sent".into())),
                now,
            );
        }
    }

    /// The user entered `key`, or chose Try Again: what an earlier try of it
    /// came to (a refusal not reported yet, the agent's refusal, a problem)
    /// is dropped, and nothing the agent or a removal still owed waits any
    /// more, so the key is asked about at once. Activate alone keeps a
    /// minute after its last failure, in case that try did grant a seat,
    /// and a wait Polar asked for (429) is kept whole.
    pub fn retry_now(&mut self, key: &str) {
        let running = self.running();
        self.outbox.retain(|e| {
            running.contains(&Some(e.id))
                || !matches!(&e.op, Op::Report(r) if r.key == key && granted(r).is_none())
        });
        for e in &mut self.outbox {
            let this_key = e.op.key() == Some(key);
            if this_key {
                e.failures = 0;
            }
            if e.retry.polars {
                continue;
            }
            if let (Op::Activate { .. }, Some(failed)) = (&e.op, e.retry.failed_at) {
                let soonest = failed + FIRST_BACKOFF;
                e.retry.not_before = e.retry.not_before.map(|t| t.min(soonest));
            } else if this_key || e.op.needs_agent() || e.op.owed_removal() {
                e.retry.not_before = None;
            }
        }
        if self.stalled.as_ref().is_some_and(|s| s.key == key) {
            self.set_stall(None);
        }
    }

    /// The problem the activation of `key` runs into, if any.
    pub fn problem_for(&self, key: &str) -> Option<Problem> {
        if self.stalled.as_ref().is_some_and(|s| s.key == key) {
            return Some(Problem::Refused);
        }
        self.outbox
            .iter()
            .any(|e| {
                e.failures > 0
                    && matches!(&e.op, Op::Activate { key: k } | Op::Probe { key: k } if k == key)
            })
            .then_some(Problem::Offline)
    }

    /// Where Remove License stands; the removal of a license another device
    /// revoked here is none of the user's, so it never shows.
    pub fn removal(&self) -> Removal {
        if let Some(e) = self.outbox.iter().find(|e| e.op.is_remove())
            && self.quiet.is_none()
        {
            return if e.failures == 0 {
                Removal::Removing
            } else {
                Removal::Failing
            };
        }
        // Only once a try at it failed: the note says Polar couldn't be
        // reached.
        if self
            .outbox
            .iter()
            .any(|e| matches!(e.op, Op::Free { seat: true, .. }) && e.failures > 0)
        {
            Removal::SeatWaiting
        } else {
            Removal::None
        }
    }

    /// Remove License is on its way: the license no longer shows as
    /// licensed here.
    pub fn removing(&self) -> bool {
        matches!(self.removal(), Removal::Removing | Removal::Failing)
    }

    /// Whether the computer shows as licensed: the agent's record says so,
    /// and the app is not removing it or freeing its activation.
    pub fn licensed(&self, license: Option<&License>) -> bool {
        let Some(l) = license.filter(|l| is_licensed(Some(l))) else {
            return false;
        };
        !self.queued(|op| match op {
            Op::Remove { .. } => true,
            Op::Free { activation_id, .. } | Op::Release { activation_id, .. } => {
                *activation_id == l.activation_id
            }
            _ => false,
        })
    }

    pub fn busy(&self) -> bool {
        self.to_polar.is_some() || self.to_agent.is_some()
    }

    /// Whether what [`Keeper::saved`] holds changed since it was last
    /// [`written`](Keeper::written).
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// What [`Keeper::saved`] gave is on disk now.
    pub fn written(&mut self) {
        self.dirty = false;
    }

    /// From now on a grant is reported only once what holds it is written,
    /// so one is never reported that a restart could lose.
    pub fn keep_on_disk(&mut self) {
        self.on_disk = true;
    }

    /// What must survive the app quitting, as JSON: the outbox's entries
    /// that cannot be worked out again from the agent's record, in order.
    pub fn saved(&self) -> String {
        let outbox: Vec<Value> = self
            .outbox
            .iter()
            .filter_map(|e| match &e.op {
                Op::Report(r) => granted(r).map(|a| {
                    json!({ "report": {
                        "key": r.key,
                        "activation_id": a.activation_id,
                        "customer_email": a.customer_email,
                        "validated_at": a.validated_at,
                    }})
                }),
                Op::Free {
                    key,
                    activation_id,
                    seat,
                } => Some(json!({ "free": {
                    "key": key,
                    "activation_id": activation_id,
                    "seat": seat,
                }})),
                Op::Remove { key, activation_id } => {
                    let mut remove = self.remove_seat.as_ref().map_or(
                        json!({}),
                        |(key, activation_id)| json!({ "key": key, "activation_id": activation_id }),
                    );
                    if let Some((key, activation_id)) = &self.quiet {
                        remove["revoked_key"] = Value::from(key.as_str());
                        remove["revoked_activation_id"] = Value::from(activation_id.as_str());
                    }
                    remove["of"] = json!({ "key": key, "activation_id": activation_id });
                    if self.stale_removals > 0 {
                        remove["stale"] = Value::from(self.stale_removals);
                        remove["reread"] = Value::from(self.reread);
                    }
                    Some(json!({ "remove": remove }))
                }
                Op::Revoke {
                    key,
                    device_id,
                    activation_id,
                    deactivated,
                } => Some(json!({ "revoke": {
                    "key": key,
                    "device_id": device_id,
                    "activation_id": activation_id,
                    "deactivated": deactivated,
                }})),
                Op::Release {
                    key,
                    activation_id,
                    freed,
                } => Some(json!({ "release": {
                    "key": key,
                    "activation_id": activation_id,
                    "freed": freed,
                }})),
                _ => None,
            })
            .collect();
        let stalled = self
            .stalled
            .as_ref()
            .map(|s| json!({ "key": s.key, "state": s.state }));
        let mut saved = json!({ "outbox": outbox, "stalled": stalled });
        if let Some(t) = self.polar_not_before {
            saved["polar_not_before"] =
                Value::from(t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs());
        }
        saved.to_string()
    }

    /// A keeper with what an earlier [`Keeper::saved`] held; an entry it
    /// cannot read is left out. `None` when the text is not one at all.
    pub fn restored(text: &str) -> Option<Keeper> {
        let mut keeper = Keeper::default();
        let list = serde_json::from_str::<Value>(text)
            .ok()?
            .get("outbox")?
            .as_array()?
            .clone();
        let pair = |v: &Value| -> Option<(String, String)> {
            let key = v.get("key")?.as_str()?;
            let id = v.get("activation_id")?.as_str()?;
            (plausible_key(key) && !id.is_empty() && id.len() <= 128)
                .then(|| (key.to_owned(), id.to_owned()))
        };
        let (mut frees, mut revokes, mut releases) = (0, 0, 0);
        for item in &list {
            if let Some(r) = item.get("report")
                && let Some((key, activation_id)) = pair(r)
            {
                keeper.push(Op::Report(RecordLicenseResult {
                    key,
                    result: Some(record_license_result::Result::Activated(LicenseActivated {
                        activation_id,
                        customer_email: r
                            .get("customer_email")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        validated_at: r.get("validated_at").and_then(Value::as_u64).unwrap_or(0),
                    })),
                    activation_id: String::new(),
                }));
            } else if let Some(f) = item.get("free")
                && let Some((key, activation_id)) = pair(f)
                && frees < MAX_FREES
            {
                frees += 1;
                let seat = f.get("seat").and_then(Value::as_bool).unwrap_or(false);
                keeper.push(Op::Free {
                    key,
                    activation_id,
                    seat,
                });
                // A seat still to free was tried before: Polar couldn't be
                // reached then.
                if seat && let Some(e) = keeper.outbox.last_mut() {
                    e.failures = 1;
                }
            } else if let Some(r) = item.get("remove")
                && !keeper.queued(Op::is_remove)
            {
                keeper.remove_seat = pair(r);
                let text = |name: &str| r.get(name).and_then(Value::as_str);
                keeper.quiet = text("revoked_key")
                    .filter(|k| plausible_key(k))
                    .zip(text("revoked_activation_id").filter(|a| valid_activation(a)))
                    .map(|(k, a)| (k.to_owned(), a.to_owned()));
                // The license it removes; one saved before it was kept
                // removes whatever is there, as RemoveLicense did then.
                let of = r
                    .get("of")
                    .and_then(|o| {
                        let key = o.get("key")?.as_str()?;
                        let id = o.get("activation_id")?.as_str()?;
                        ((key.is_empty() || plausible_key(key)) && valid_activation(id))
                            .then(|| (key.to_owned(), id.to_owned()))
                    })
                    .or_else(|| keeper.quiet.clone())
                    .or_else(|| keeper.remove_seat.clone())
                    .unwrap_or_default();
                keeper.push(Op::Remove {
                    key: of.0,
                    activation_id: of.1,
                });
                keeper.stale_removals = r
                    .get("stale")
                    .and_then(Value::as_u64)
                    .map_or(0, |n| n.min(u64::from(REMOVE_TRIES)) as u32);
                keeper.reread = keeper.stale_removals > 0
                    && r.get("reread").and_then(Value::as_bool).unwrap_or(false);
            } else if let Some(r) = item.get("revoke")
                && let Some(device_id) = r.get("device_id").and_then(Value::as_str)
                && !device_id.is_empty()
                && device_id.len() <= 256
                && !device_id.chars().any(char::is_control)
                && let Some(activation_id) = r.get("activation_id").and_then(Value::as_str)
                && valid_activation(activation_id)
                && revokes < MAX_REVOKES
            {
                revokes += 1;
                let key = r.get("key").and_then(Value::as_str).unwrap_or("");
                // Without a key to ask Polar with, or decided already.
                let deactivated = match r.get("deactivated").and_then(Value::as_bool) {
                    None if !activation_id.is_empty() && plausible_key(key) => None,
                    decided => Some(decided.unwrap_or(false)),
                };
                keeper.push(Op::Revoke {
                    key: key.to_owned(),
                    device_id: device_id.to_owned(),
                    activation_id: activation_id.to_owned(),
                    deactivated,
                });
            } else if let Some(r) = item.get("release")
                && let Some((key, activation_id)) = pair(r)
                && releases < MAX_REVOKES
                && !keeper.queued(|op| op.freeing() == Some(activation_id.as_str()))
            {
                releases += 1;
                keeper.push(Op::Release {
                    key,
                    activation_id,
                    freed: r.get("freed").and_then(Value::as_bool).unwrap_or(false),
                });
            }
        }
        let saved = text_value(text)?;
        keeper.polar_not_before = saved
            .get("polar_not_before")
            .and_then(Value::as_u64)
            .map(|secs| UNIX_EPOCH + Duration::from_secs(secs));
        let stalled = saved_stall(&saved);
        keeper.stalled = stalled;
        keeper.dirty = false;
        Some(keeper)
    }

    /// Polar will not take what the app sends for `key`, or granted it
    /// something the app cannot read: it is left alone as after a final
    /// refusal, for the record waiting for its activation (what activate
    /// and the probe ask about).
    fn stall(&mut self, key: String) -> After {
        self.set_stall(Some(Stall {
            key,
            state: Some(LicenseState::PendingActivation as i32),
        }));
        After::Drop
    }

    /// A key left alone, kept on disk so a restart does not activate it
    /// again.
    fn set_stall(&mut self, stall: Option<Stall>) {
        if self.stalled != stall {
            self.stalled = stall;
            self.dirty = true;
        }
    }

    /// The check after a first 404 got no answer: it waits longer.
    fn missed_again(&mut self, activation_id: &str, trouble: &Trouble, now: SystemTime) {
        if let Some(m) = self
            .missing
            .as_mut()
            .filter(|m| m.activation_id == activation_id)
        {
            m.retry.failed(trouble, now);
        }
    }

    fn queued(&self, test: impl Fn(&Op) -> bool) -> bool {
        self.outbox.iter().any(|e| test(&e.op))
    }

    fn push(&mut self, op: Op) {
        self.last_id += 1;
        if op.kept() {
            self.dirty = true;
        }
        self.outbox.push(Entry {
            id: self.last_id,
            op,
            retry: Retry::default(),
            failures: 0,
        });
    }

    /// Queues freeing an activation; once per activation, and at most
    /// [`MAX_FREES`] of them.
    fn free(&mut self, key: String, activation_id: String, seat: bool) {
        self.dirty = true;
        for e in &mut self.outbox {
            if let Op::Free {
                activation_id: a,
                seat: s,
                ..
            } = &mut e.op
                && *a == activation_id
            {
                *s |= seat;
                return;
            }
        }
        self.last_id += 1;
        self.outbox.insert(
            self.outbox.len(),
            Entry {
                id: self.last_id,
                op: Op::Free {
                    key,
                    activation_id,
                    seat,
                },
                retry: Retry::default(),
                failures: 0,
            },
        );
        let frees = self
            .outbox
            .iter()
            .filter(|e| matches!(e.op, Op::Free { .. }))
            .count();
        if frees > MAX_FREES {
            let running = self.running();
            if let Some(i) = self.outbox.iter().position(|e| {
                matches!(e.op, Op::Free { seat: false, .. }) && !running.contains(&Some(e.id))
            }) {
                self.outbox.remove(i);
            }
        }
    }

    /// What an answer does to the entry `id`; nothing when the entry is
    /// gone (Remove License dropped it meanwhile).
    fn settle(&mut self, id: u64, after: After, now: SystemTime) {
        let removing = self.queued(Op::is_remove);
        let Some(i) = self.outbox.iter().position(|e| e.id == id) else {
            return;
        };
        let entry = &mut self.outbox[i];
        let was_kept = entry.op.kept();
        match after {
            // While Remove License is on its way, nothing that follows from
            // the license is asked any more.
            After::Drop => {
                self.outbox.remove(i);
            }
            After::Become(op) | After::BecomeFailed(op, _) if removing && !op.kept() => {
                self.outbox.remove(i);
            }
            After::Become(op) => {
                entry.op = op;
                entry.retry = Retry::default();
                entry.failures = 0;
            }
            After::Fail(trouble) => {
                entry.retry.failed(&trouble, now);
                entry.failures += 1;
            }
            After::BecomeFailed(op, trouble) => {
                entry.op = op;
                entry.retry.failed(&trouble, now);
                entry.failures += 1;
            }
        }
        let now_kept = self
            .outbox
            .iter()
            .find(|e| e.id == id)
            .is_some_and(|e| e.op.kept());
        if was_kept || now_kept {
            self.dirty = true;
        }
    }
}

fn text_value(text: &str) -> Option<Value> {
    serde_json::from_str(text).ok()
}

/// The stall kept in a saved outbox, if any and readable.
fn saved_stall(saved: &Value) -> Option<Stall> {
    let s = saved.get("stalled")?;
    let key = s.get("key")?.as_str()?;
    plausible_key(key).then(|| Stall {
        key: key.to_owned(),
        state: s
            .get("state")
            .and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok()),
    })
}

fn refusal(key: String, refusal: LicenseRefusal, activation_id: String) -> Op {
    Op::Report(RecordLicenseResult {
        key,
        result: Some(record_license_result::Result::Refused(refusal as i32)),
        activation_id,
    })
}

/// The license's state, an unknown value read as no license.
pub fn state(license: &License) -> LicenseState {
    LicenseState::try_from(license.state).unwrap_or(LicenseState::None)
}

/// Whether the record holds license work nobody started on this computer,
/// which the agent opens the app for in the background: a key another
/// device applied here, to activate, or a license it revoked here, to free.
pub fn unasked_work(license: Option<&License>) -> bool {
    license.is_some_and(|l| match state(l) {
        LicenseState::Revoked => true,
        LicenseState::PendingActivation => !l.source_device_id.is_empty(),
        _ => false,
    })
}

/// Whether the computer shows as licensed.
pub fn is_licensed(license: Option<&License>) -> bool {
    license.is_some_and(|l| state(l) == LicenseState::Licensed)
}

fn since(validated_at: u64, now: SystemTime) -> Duration {
    now.duration_since(UNIX_EPOCH + Duration::from_secs(validated_at))
        .unwrap_or_default()
}

/// `validated_at` for an answer: when Polar answered, unless that is far
/// off this computer's clock (the agent refuses more than a day ahead),
/// then this computer's clock.
pub fn stamp(answered: Option<SystemTime>, now: SystemTime) -> u64 {
    let secs = |t: SystemTime| t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let clock = secs(now);
    match answered.map(secs) {
        Some(at) if at >= EARLIEST && at <= clock + DAY.as_secs() => at,
        _ => clock,
    }
}

/// Activation ids the agent takes: up to 128 printable ASCII characters.
fn valid_activation(id: &str) -> bool {
    id.len() <= 128 && id.bytes().all(|b| b.is_ascii_graphic())
}

/// Keys the agent takes: 20 to 96 printable ASCII characters, no spaces.
pub fn plausible_key(key: &str) -> bool {
    (20..=96).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_graphic())
}

/// The buyer's address as the apps show it: its first character, then
/// "•••" and the domain.
pub fn short_email(email: &str) -> String {
    match email.split_once('@') {
        Some((name, domain)) if !name.is_empty() && !domain.is_empty() => {
            let first: String = name.chars().take(1).collect();
            format!("{first}•••@{domain}")
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iw_proto::app::v1::{LicenseActivation, LicensedDevice};

    const KEY: &str = "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42";
    const OTHER: &str = "INPUTWEAVE-00000000-0000-4000-8000-000000000001";

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    const NOW: u64 = 1_793_800_000;

    fn license(state: LicenseState) -> License {
        License {
            key: KEY.into(),
            state: state as i32,
            ..Default::default()
        }
    }

    fn licensed(validated_at: u64) -> License {
        License {
            activation_id: "act-1".into(),
            validated_at,
            customer_email: "someone@example.com".into(),
            devices: vec![LicensedDevice {
                id: "mac".into(),
                name: "MacBook Pro".into(),
                licensed: true,
                ..Default::default()
            }],
            ..license(LicenseState::Licensed)
        }
    }

    fn holding(activation_id: &str) -> License {
        License {
            activation_id: activation_id.into(),
            ..licensed(NOW)
        }
    }

    fn answered<T>(outcome: T) -> Answered<T> {
        Answered {
            outcome,
            at: Some(at(NOW - 5)),
        }
    }

    fn activated() -> Done {
        Done::Activated(answered(Activation::Activated {
            activation_id: "act-9".into(),
            customer_email: "someone@example.com".into(),
        }))
    }

    fn unreachable() -> Trouble {
        Trouble::Unreachable("x".into())
    }

    fn pending() -> License {
        license(LicenseState::PendingActivation)
    }

    fn free(activation_id: &str) -> Task {
        Task::Free {
            key: KEY.into(),
            activation_id: activation_id.into(),
        }
    }

    fn reread(text: &str) -> Keeper {
        Keeper::restored(text).expect("a saved outbox")
    }

    /// `next` with the agent linked.
    fn next(k: &mut Keeper, l: &License, visible: bool, now: u64) -> Option<Task> {
        k.next(Some(l), "Linux box", visible, true, at(now))
    }

    /// Activates, gets Polar's yes and returns the report it queues.
    fn activate(k: &mut Keeper, now: u64) -> Task {
        assert!(matches!(
            next(k, &pending(), false, now),
            Some(Task::Activate { .. })
        ));
        k.finished(activated(), at(now));
        next(k, &pending(), false, now).expect("a report")
    }

    #[test]
    fn a_waiting_key_is_activated_and_the_grant_kept_before_it_is_reported() {
        let mut k = Keeper::default();
        let l = pending();
        assert_eq!(
            next(&mut k, &l, false, NOW),
            Some(Task::Activate {
                key: KEY.into(),
                label: "Linux box".into()
            })
        );
        // Nothing else while it runs.
        assert_eq!(next(&mut k, &l, false, NOW), None);
        k.finished(activated(), at(NOW));
        assert!(k.dirty(), "the grant is kept before it is sent");
        assert!(k.saved().contains("act-9"));
        k.written();
        match next(&mut k, &l, false, NOW) {
            Some(Task::Report(RecordLicenseResult {
                key,
                result: Some(record_license_result::Result::Activated(a)),
                activation_id,
            })) => {
                assert_eq!(key, KEY);
                assert_eq!(a.activation_id, "act-9");
                assert_eq!(a.validated_at, NOW - 5);
                assert!(activation_id.is_empty());
            }
            other => panic!("{other:?}"),
        }
        k.reported(Ok(()), Some(&holding("act-9")), at(NOW));
        assert!(k.dirty());
        assert!(!k.saved().contains("act-9"));
        assert!(!k.busy());
        assert_eq!(next(&mut k, &holding("act-9"), false, NOW), None);
    }

    #[test]
    fn a_grant_is_reported_only_once_it_is_written() {
        let mut k = Keeper::default();
        k.keep_on_disk();
        next(&mut k, &pending(), false, NOW);
        k.finished(activated(), at(NOW));
        // The write failed: the grant waits, in memory and to write.
        assert_eq!(next(&mut k, &pending(), false, NOW + 5), None);
        assert!(k.dirty());
        k.written();
        assert!(matches!(
            next(&mut k, &pending(), false, NOW + 5),
            Some(Task::Report(_))
        ));
    }

    #[test]
    fn nothing_without_a_license_or_in_states_that_need_nothing() {
        let mut k = Keeper::default();
        assert_eq!(k.next(None, "x", true, true, at(NOW)), None);
        for s in [
            LicenseState::None,
            LicenseState::SeatsFull,
            LicenseState::Invalid,
            LicenseState::Unavailable,
        ] {
            assert_eq!(next(&mut k, &license(s), true, NOW), None, "{s:?}");
        }
    }

    #[test]
    fn an_activation_the_agent_refused_is_freed_and_the_key_stalled() {
        for code in [
            "already_licensed",
            "license_changed",
            "license_stale",
            "invalid_license_result",
            "license_unsupported",
            "from_a_newer_agent",
        ] {
            // Licensed with another activation, as "already_licensed" says;
            // otherwise still waiting for one.
            let record = if code == "already_licensed" {
                holding("act-1")
            } else {
                pending()
            };
            let mut k = Keeper::default();
            activate(&mut k, NOW);
            k.reported(Err(Some(code)), Some(&record), at(NOW));
            assert!(k.saved().contains("act-9"), "{code}: kept until freed");
            assert_eq!(next(&mut k, &record, false, NOW), Some(free("act-9")));
            k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
            assert!(!k.saved().contains("act-9"), "{code}: freed");
            // Not activated again for this record.
            assert_eq!(next(&mut k, &record, false, NOW + 1), None, "{code}");
            assert_eq!(k.problem_for(KEY), Some(Problem::Refused), "{code}");
        }
    }

    #[test]
    fn refusals_for_now_and_no_answer_are_tried_again() {
        for code in [Some("agent_stopping"), Some("license_unavailable"), None] {
            let mut k = Keeper::default();
            let report = activate(&mut k, NOW);
            k.reported(Err(code), Some(&pending()), at(NOW));
            let code = code.unwrap_or("no answer");
            assert!(k.saved().contains("act-9"), "{code}: kept");
            assert!(!k.saved().contains("free"), "{code}: not freed");
            assert_eq!(k.problem_for(KEY), None, "{code}");
            assert_eq!(next(&mut k, &pending(), false, NOW + 59), None);
            assert_eq!(next(&mut k, &pending(), false, NOW + 60), Some(report));
        }
    }

    #[test]
    fn a_request_polar_cannot_read_stalls_the_key() {
        let mut k = Keeper::default();
        next(&mut k, &pending(), false, NOW);
        k.finished(Done::Activated(answered(Activation::Rejected)), at(NOW));
        assert_eq!(k.problem_for(KEY), Some(Problem::Refused));
        assert_eq!(next(&mut k, &pending(), false, NOW + 3600), None);
        k.retry_now(KEY);
        assert!(matches!(
            next(&mut k, &pending(), false, NOW + 3600),
            Some(Task::Activate { .. })
        ));
    }

    #[test]
    fn entering_a_key_again_drops_what_its_last_try_came_to() {
        let mut k = Keeper::default();
        next(&mut k, &pending(), false, NOW);
        k.finished(Done::Activated(answered(Activation::Invalid)), at(NOW));
        next(&mut k, &pending(), false, NOW);
        k.reported(Err(None), Some(&pending()), at(NOW));
        // The invalid answer, not reported yet, is not this try's.
        k.retry_now(KEY);
        assert!(matches!(
            next(&mut k, &pending(), false, NOW + 1),
            Some(Task::Activate { .. })
        ));
    }

    #[test]
    fn already_licensed_with_this_very_activation_is_done() {
        let mut k = Keeper::default();
        activate(&mut k, NOW);
        k.reported(
            Err(Some("already_licensed")),
            Some(&holding("act-9")),
            at(NOW),
        );
        assert_eq!(next(&mut k, &holding("act-9"), false, NOW), None);
        assert!(!k.saved().contains("act-9"), "not freed: the agent has it");
        assert_eq!(k.problem_for(KEY), None);
    }

    #[test]
    fn a_grant_the_agent_holds_already_is_not_reported_again() {
        let mut k = Keeper::default();
        activate(&mut k, NOW);
        k.reported(Err(None), None, at(NOW));
        let mut back = reread(&k.saved());
        assert_eq!(next(&mut back, &holding("act-9"), false, NOW + 120), None);
        assert!(!back.saved().contains("act-9"));
    }

    #[test]
    fn a_stalled_key_starts_again_when_the_user_acts_or_the_record_changes() {
        let stalled = || {
            let mut k = Keeper::default();
            activate(&mut k, NOW);
            k.reported(Err(Some("license_stale")), Some(&pending()), at(NOW));
            next(&mut k, &pending(), false, NOW);
            k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
            assert_eq!(next(&mut k, &pending(), false, NOW), None);
            k
        };
        let mut k = stalled();
        k.retry_now(OTHER);
        assert_eq!(next(&mut k, &pending(), false, NOW), None, "another key");
        k.retry_now(KEY);
        assert!(next(&mut k, &pending(), false, NOW).is_some());
        // A state change lifts it too.
        let mut k = stalled();
        assert_eq!(next(&mut k, &license(LicenseState::None), false, NOW), None);
        assert!(next(&mut k, &pending(), false, NOW).is_some());
        // So does another key.
        let mut k = stalled();
        let other = License {
            key: OTHER.into(),
            ..pending()
        };
        assert!(next(&mut k, &other, false, NOW).is_some());
    }

    #[test]
    fn an_activation_that_cannot_be_freed_waits_on_its_own() {
        let mut k = Keeper::default();
        activate(&mut k, NOW);
        k.reported(Err(Some("license_changed")), None, at(NOW));
        assert_eq!(next(&mut k, &pending(), false, NOW), Some(free("act-9")));
        k.finished(
            Done::Deactivated(Deactivation::Trouble(unreachable())),
            at(NOW),
        );
        assert!(k.saved().contains("act-9"));
        // It waits; the license's own requests go on meanwhile.
        let old = licensed(NOW - VALIDATE_EVERY.as_secs() - 1);
        assert!(matches!(
            next(&mut k, &old, true, NOW + 10),
            Some(Task::Validate { .. })
        ));
        k.finished(
            Done::Validated(answered(Validation::Trouble(unreachable()))),
            at(NOW + 10),
        );
        assert_eq!(next(&mut k, &old, true, NOW + 59), None);
        assert_eq!(next(&mut k, &old, true, NOW + 60), Some(free("act-9")));
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW + 60));
        assert!(!k.saved().contains("act-9"));
    }

    #[test]
    fn activations_are_freed_without_the_agent_and_nothing_else_is_asked() {
        let mut k = Keeper::default();
        activate(&mut k, NOW);
        k.reported(Err(Some("license_stale")), None, at(NOW));
        let mut back = reread(&k.saved());
        assert_eq!(
            back.next(Some(&pending()), "x", false, false, at(NOW)),
            Some(free("act-9"))
        );
        back.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        // Without the agent nothing is activated, reported or removed.
        let mut k = Keeper::default();
        assert_eq!(k.next(Some(&pending()), "x", false, false, at(NOW)), None);
        let mut k = Keeper::default();
        let report = activate(&mut k, NOW);
        k.reported(Err(None), None, at(NOW));
        assert_eq!(
            k.next(Some(&pending()), "x", false, false, at(NOW + 60)),
            None
        );
        assert_eq!(next(&mut k, &pending(), false, NOW + 60), Some(report));
    }

    #[test]
    fn the_outbox_is_worked_oldest_first() {
        let mut k = Keeper::default();
        activate(&mut k, NOW);
        k.reported(Err(Some("license_changed")), None, at(NOW));
        // The free queued first goes before the new key's activation.
        let other = License {
            key: OTHER.into(),
            ..pending()
        };
        assert_eq!(next(&mut k, &other, false, NOW), Some(free("act-9")));
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        assert!(matches!(
            next(&mut k, &other, false, NOW),
            Some(Task::Activate { key, .. }) if key == OTHER
        ));
    }

    #[test]
    fn a_report_that_did_not_reach_the_agent_waits_and_is_never_activated_twice() {
        for code in [None, Some("license_unavailable")] {
            let mut k = Keeper::default();
            let report = activate(&mut k, NOW);
            k.reported(Err(code), Some(&pending()), at(NOW));
            let l = pending();
            // Not at once, and never a second activation meanwhile.
            assert_eq!(next(&mut k, &l, false, NOW + 30), None);
            assert_eq!(next(&mut k, &l, false, NOW + 60), Some(report.clone()));
            k.reported(Err(code), Some(&l), at(NOW + 60));
            assert_eq!(next(&mut k, &l, false, NOW + 60 + 119), None);
            assert_eq!(next(&mut k, &l, false, NOW + 180), Some(report));
            // A temporary refusal never stalls the key.
            assert_eq!(k.problem_for(KEY), None);
        }
    }

    #[test]
    fn an_unreported_activation_survives_a_restart() {
        let mut k = Keeper::default();
        let report = activate(&mut k, NOW);
        k.reported(Err(None), None, at(NOW));
        let mut again = reread(&k.saved());
        // Reported first, not activated anew.
        assert_eq!(next(&mut again, &pending(), false, NOW + 1), Some(report));
        again.reported(Ok(()), Some(&holding("act-9")), at(NOW + 1));
        assert_eq!(reread(&again.saved()).saved(), Keeper::default().saved());
    }

    #[test]
    fn what_is_saved_reads_back_and_junk_is_left_out() {
        let mut k = Keeper::default();
        activate(&mut k, NOW);
        k.reported(Err(Some("license_changed")), None, at(NOW));
        k.remove(Some(&licensed(NOW)));
        let back = reread(&k.saved());
        assert_eq!(back.saved(), k.saved());
        assert_eq!(back.removal(), Removal::Removing);
        // Entries it cannot read are left out.
        for junk in [
            r#"{"outbox":[{"free":{"key":"short","activation_id":"a"}}]}"#,
            r#"{"outbox":[{"report":{"key":"INPUTWEAVE-7F3A2C1E-58D4-4B9A","activation_id":""}}]}"#,
            r#"{"outbox":[7,"x",{"what":{}}]}"#,
        ] {
            assert_eq!(reread(junk).saved(), Keeper::default().saved(), "{junk}");
        }
        // Text that is not an outbox at all is not one.
        for junk in ["", "{", "[]", "{}", r#"{"outbox":7}"#] {
            assert!(Keeper::restored(junk).is_none(), "{junk}");
        }
    }

    #[test]
    fn a_403_is_told_apart_by_a_validate_without_activation() {
        let mut k = Keeper::default();
        let l = pending();
        next(&mut k, &l, false, NOW);
        k.finished(Done::Activated(answered(Activation::NotPermitted)), at(NOW));
        assert_eq!(
            next(&mut k, &l, false, NOW),
            Some(Task::Probe { key: KEY.into() })
        );
        k.finished(
            Done::Probed(answered(Validation::Valid {
                customer_email: String::new(),
            })),
            at(NOW),
        );
        assert_eq!(
            next(&mut k, &l, false, NOW),
            Some(refusal(KEY.into(), LicenseRefusal::SeatsFull, String::new()).task("x"))
        );
        k.reported(Ok(()), Some(&license(LicenseState::SeatsFull)), at(NOW));

        let mut k = Keeper::default();
        next(&mut k, &l, false, NOW);
        k.finished(Done::Activated(answered(Activation::NotPermitted)), at(NOW));
        next(&mut k, &l, false, NOW);
        k.finished(Done::Probed(answered(Validation::NotFound)), at(NOW));
        assert_eq!(
            next(&mut k, &l, false, NOW),
            Some(refusal(KEY.into(), LicenseRefusal::Invalid, String::new()).task("x"))
        );
        // A probe without an answer activates again later.
        let mut k = Keeper::default();
        next(&mut k, &l, false, NOW);
        k.finished(Done::Activated(answered(Activation::NotPermitted)), at(NOW));
        next(&mut k, &l, false, NOW);
        k.finished(
            Done::Probed(answered(Validation::Trouble(unreachable()))),
            at(NOW),
        );
        assert_eq!(k.problem_for(KEY), Some(Problem::Offline));
        assert_eq!(next(&mut k, &l, false, NOW + 59), None);
        assert!(matches!(
            next(&mut k, &l, false, NOW + 60),
            Some(Task::Activate { .. })
        ));
    }

    #[test]
    fn a_refusal_that_did_not_reach_the_agent_waits_and_blocks_activating_again() {
        let mut k = Keeper::default();
        let l = pending();
        next(&mut k, &l, false, NOW);
        k.finished(Done::Activated(answered(Activation::Invalid)), at(NOW));
        assert!(matches!(
            next(&mut k, &l, false, NOW),
            Some(Task::Report(_))
        ));
        k.reported(Err(None), Some(&l), at(NOW));
        assert_eq!(next(&mut k, &l, false, NOW + 1), None);
        assert!(matches!(
            next(&mut k, &l, false, NOW + 60),
            Some(Task::Report(_))
        ));
    }

    #[test]
    fn an_unreachable_polar_waits_longer_each_time_and_says_so() {
        let mut k = Keeper::default();
        let l = pending();
        next(&mut k, &l, false, NOW);
        let trouble = || Done::Activated(answered(Activation::Trouble(unreachable())));
        k.finished(trouble(), at(NOW));
        assert_eq!(k.problem_for(KEY), Some(Problem::Offline));
        assert_eq!(next(&mut k, &l, false, NOW + 59), None);
        assert!(next(&mut k, &l, false, NOW + 60).is_some());
        k.finished(trouble(), at(NOW + 60));
        assert_eq!(next(&mut k, &l, false, NOW + 60 + 119), None);
        assert!(next(&mut k, &l, false, NOW + 60 + 120).is_some());
        // Entering the key again asks a minute after the last try (it may
        // have granted a seat all the same), not after the whole wait, and
        // the problem goes until it comes back.
        k.finished(trouble(), at(NOW + 180));
        k.retry_now(KEY);
        assert_eq!(k.problem_for(KEY), None);
        assert_eq!(next(&mut k, &l, false, NOW + 239), None);
        assert!(next(&mut k, &l, false, NOW + 240).is_some());
    }

    #[test]
    fn entering_a_key_clears_the_wait_of_everything_about_it() {
        let mut k = Keeper::default();
        let report = activate(&mut k, NOW);
        k.reported(Err(None), Some(&pending()), at(NOW));
        assert_eq!(next(&mut k, &pending(), false, NOW + 1), None);
        k.retry_now(KEY);
        assert_eq!(next(&mut k, &pending(), false, NOW + 1), Some(report));
    }

    #[test]
    fn a_429_waits_as_long_as_polar_says() {
        let mut k = Keeper::default();
        let l = pending();
        next(&mut k, &l, false, NOW);
        k.finished(
            Done::Activated(answered(Activation::Trouble(Trouble::RateLimited(
                Duration::from_secs(5),
            )))),
            at(NOW),
        );
        assert_eq!(next(&mut k, &l, false, NOW + 4), None);
        assert!(next(&mut k, &l, false, NOW + 5).is_some());
        // Entering the key again does not cut Polar's wait short.
        k.finished(
            Done::Activated(answered(Activation::Trouble(Trouble::RateLimited(
                Duration::from_secs(300),
            )))),
            at(NOW + 5),
        );
        k.retry_now(KEY);
        assert_eq!(next(&mut k, &l, false, NOW + 304), None);
        assert!(next(&mut k, &l, false, NOW + 305).is_some());
    }

    #[test]
    fn a_failed_probe_during_remove_does_not_activate_ahead_of_it() {
        let mut k = Keeper::default();
        let l = pending();
        next(&mut k, &l, false, NOW);
        k.finished(Done::Activated(answered(Activation::NotPermitted)), at(NOW));
        assert!(matches!(
            next(&mut k, &l, false, NOW),
            Some(Task::Probe { .. })
        ));
        k.remove(Some(&l));
        k.finished(
            Done::Probed(answered(Validation::Trouble(unreachable()))),
            at(NOW),
        );
        assert_eq!(next(&mut k, &l, false, NOW + 120), Some(removing("")));
    }

    #[test]
    fn a_licensed_computer_validates_every_30_days_only_while_shown() {
        let mut k = Keeper::default();
        let fresh = licensed(NOW - 3600);
        assert_eq!(next(&mut k, &fresh, true, NOW), None);
        let old = licensed(NOW - VALIDATE_EVERY.as_secs());
        assert_eq!(next(&mut k, &old, false, NOW), None);
        assert_eq!(
            next(&mut k, &old, true, NOW),
            Some(Task::Validate {
                key: KEY.into(),
                activation_id: "act-1".into()
            })
        );
        k.finished(
            Done::Validated(answered(Validation::Valid {
                customer_email: "someone@example.com".into(),
            })),
            at(NOW),
        );
        match next(&mut k, &old, true, NOW) {
            Some(Task::Report(result)) => {
                assert_eq!(result.activation_id, "act-1");
                assert!(matches!(
                    result.result,
                    Some(record_license_result::Result::Validated(LicenseValidated {
                        validated_at,
                        ..
                    })) if validated_at == NOW - 5
                ));
            }
            other => panic!("{other:?}"),
        }
        // A refused validated report frees nothing.
        k.reported(Err(Some("license_stale")), Some(&old), at(NOW));
        assert!(!k.saved().contains("free"));
    }

    #[test]
    fn an_unanswered_validate_changes_nothing_and_waits_an_hour() {
        let mut k = Keeper::default();
        let old = licensed(NOW - VALIDATE_EVERY.as_secs() - 1);
        next(&mut k, &old, true, NOW);
        k.finished(
            Done::Validated(answered(Validation::Trouble(unreachable()))),
            at(NOW),
        );
        assert_eq!(next(&mut k, &old, true, NOW + 3599), None);
        assert!(next(&mut k, &old, true, NOW + 3600).is_some());
    }

    #[test]
    fn invalid_only_after_two_404s_a_minute_apart() {
        let mut k = Keeper::default();
        let old = licensed(NOW - VALIDATE_EVERY.as_secs() - 1);
        next(&mut k, &old, true, NOW);
        k.finished(Done::Validated(answered(Validation::NotFound)), at(NOW));
        assert_eq!(next(&mut k, &old, true, NOW + 59), None);
        assert!(next(&mut k, &old, true, NOW + 60).is_some());
        k.finished(
            Done::Validated(answered(Validation::NotFound)),
            at(NOW + 60),
        );
        assert_eq!(
            next(&mut k, &old, true, NOW + 60),
            Some(refusal(KEY.into(), LicenseRefusal::Invalid, "act-1".into()).task("x"))
        );
    }

    #[test]
    fn remove_tells_the_agent_then_frees_the_seat() {
        let mut k = Keeper::default();
        let l = licensed(NOW);
        k.remove(Some(&l));
        assert_eq!(k.removal(), Removal::Removing);
        assert!(k.removing());
        assert_eq!(next(&mut k, &l, true, NOW), Some(removing("act-1")));
        assert_eq!(next(&mut k, &l, true, NOW), None, "the seat waits for it");
        assert!(!k.removed(Ok(()), at(NOW)));
        assert_eq!(k.removal(), Removal::None, "not said before a try");
        let none = license(LicenseState::None);
        assert_eq!(next(&mut k, &none, true, NOW), Some(free("act-1")));
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        assert_eq!(k.removal(), Removal::None);
        assert_eq!(k.saved(), Keeper::default().saved());
    }

    #[test]
    fn remove_goes_on_without_polar_and_the_seat_waits_until_polar_answers() {
        let mut k = Keeper::default();
        let l = licensed(NOW);
        k.remove(Some(&l));
        assert_eq!(next(&mut k, &l, true, NOW), Some(removing("act-1")));
        k.removed(Ok(()), at(NOW));
        let none = license(LicenseState::None);
        assert_eq!(next(&mut k, &none, true, NOW), Some(free("act-1")));
        k.finished(
            Done::Deactivated(Deactivation::Trouble(unreachable())),
            at(NOW),
        );
        assert_eq!(k.removal(), Removal::SeatWaiting);
        assert!(!k.removing());
        // Also after a restart.
        assert_eq!(reread(&k.saved()).removal(), Removal::SeatWaiting);
        assert_eq!(next(&mut k, &none, true, NOW + 59), None);
        assert_eq!(
            k.next(Some(&none), "x", true, false, at(NOW + 60)),
            Some(free("act-1"))
        );
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW + 60));
        assert_eq!(k.removal(), Removal::None);
        // Without a seat to free, straight to the agent.
        let mut k = Keeper::default();
        k.remove(Some(&license(LicenseState::SeatsFull)));
        assert_eq!(k.next(None, "x", true, true, at(NOW)), Some(removing("")));
    }

    #[test]
    fn a_remove_the_agent_did_not_take_is_tried_again_and_shows_as_failing() {
        let mut k = Keeper::default();
        let l = licensed(NOW);
        k.remove(Some(&l));
        next(&mut k, &l, true, NOW);
        k.removed(Err(None), at(NOW));
        assert_eq!(k.removal(), Removal::Failing);
        assert!(k.removing(), "still on its way, not licensed any more");
        // Nothing else meanwhile, not even validating.
        let old = licensed(NOW - VALIDATE_EVERY.as_secs() - 1);
        assert_eq!(next(&mut k, &old, true, NOW + 30), None);
        assert_eq!(next(&mut k, &old, true, NOW + 60), Some(removing("act-1")));
        k.removed(Err(Some("license_unavailable")), at(NOW + 60));
        // It survives a restart, with the seat it frees once taken.
        let mut back = reread(&k.saved());
        assert_eq!(
            next(&mut back, &old, true, NOW + 61),
            Some(removing("act-1"))
        );
        back.removed(Ok(()), at(NOW + 61));
        let none = license(LicenseState::None);
        assert_eq!(next(&mut back, &none, true, NOW + 61), Some(free("act-1")));
    }

    #[test]
    fn a_remove_the_agent_refused_for_good_is_dropped_and_said() {
        let mut k = Keeper::default();
        let l = licensed(NOW);
        k.remove(Some(&l));
        next(&mut k, &l, true, NOW);
        assert!(k.removed(Err(Some("unsupported")), at(NOW)));
        // The license stays, its seat too, and its work goes on.
        assert_eq!(k.removal(), Removal::None);
        assert!(k.licensed(Some(&l)));
        assert!(!k.saved().contains("act-1"));
        let old = licensed(NOW - VALIDATE_EVERY.as_secs() - 1);
        assert!(matches!(
            next(&mut k, &old, true, NOW + 1),
            Some(Task::Validate { .. })
        ));
    }

    #[test]
    fn remove_frees_what_polar_grants_meanwhile_and_what_the_agent_has_not_taken() {
        // Granted while Remove License began: freed, never reported.
        let mut k = Keeper::default();
        next(&mut k, &pending(), false, NOW);
        k.remove(Some(&pending()));
        k.finished(activated(), at(NOW));
        assert_eq!(next(&mut k, &pending(), false, NOW), Some(removing("")));
        assert_eq!(next(&mut k, &pending(), false, NOW), Some(free("act-9")));
        // Granted earlier, not taken yet: freed instead of reported.
        let mut k = Keeper::default();
        activate(&mut k, NOW);
        k.reported(Err(None), None, at(NOW));
        k.remove(Some(&pending()));
        assert!(!k.saved().contains("report"));
        assert_eq!(next(&mut k, &pending(), false, NOW), Some(free("act-9")));
        // Taken by the agent just as Remove License began: that seat goes
        // too, once the agent took RemoveLicense.
        let mut k = Keeper::default();
        activate(&mut k, NOW);
        k.remove(Some(&pending()));
        k.reported(Ok(()), Some(&holding("act-9")), at(NOW));
        assert_eq!(
            next(&mut k, &holding("act-9"), false, NOW),
            Some(removing(""))
        );
        k.removed(Ok(()), at(NOW));
        let none = license(LicenseState::None);
        assert_eq!(next(&mut k, &none, false, NOW), Some(free("act-9")));
        k.finished(
            Done::Deactivated(Deactivation::Trouble(unreachable())),
            at(NOW),
        );
        assert_eq!(k.removal(), Removal::SeatWaiting);
    }

    #[test]
    fn a_new_key_does_not_wait_for_an_old_seat() {
        let mut k = Keeper::default();
        let l = licensed(NOW);
        k.remove(Some(&l));
        next(&mut k, &l, false, NOW);
        k.removed(Ok(()), at(NOW));
        let none = license(LicenseState::None);
        next(&mut k, &none, false, NOW);
        k.finished(
            Done::Deactivated(Deactivation::Trouble(unreachable())),
            at(NOW),
        );
        let other = License {
            key: OTHER.into(),
            ..pending()
        };
        // The seat's free waits; the new key is asked about meanwhile.
        assert!(matches!(
            next(&mut k, &other, false, NOW + 1),
            Some(Task::Activate { key, .. }) if key == OTHER
        ));
        // Entering the old key again asks about its seat first.
        let mut k = Keeper::default();
        k.remove(Some(&l));
        next(&mut k, &l, false, NOW);
        k.removed(Ok(()), at(NOW));
        next(&mut k, &none, false, NOW);
        k.finished(
            Done::Deactivated(Deactivation::Trouble(unreachable())),
            at(NOW),
        );
        k.retry_now(KEY);
        assert_eq!(
            next(&mut k, &pending(), false, NOW + 1),
            Some(free("act-1"))
        );
    }

    #[test]
    fn licensed_only_while_nothing_removes_or_frees_it() {
        let k = Keeper::default();
        assert!(k.licensed(Some(&licensed(NOW))));
        assert!(!k.licensed(Some(&pending())));
        assert!(!k.licensed(None));
        let k = reread(&format!(
            r#"{{"outbox":[{{"free":{{"key":"{KEY}","activation_id":"act-1","seat":true}}}}]}}"#
        ));
        assert!(!k.licensed(Some(&licensed(NOW))), "its activation is freed");
        assert!(k.licensed(Some(&holding("act-2"))));
        let mut k = Keeper::default();
        k.remove(Some(&holding("act-2")));
        assert!(!k.licensed(Some(&holding("act-2"))));
    }

    #[test]
    fn the_check_after_a_first_404_backs_off_while_polar_cannot_answer() {
        let mut k = Keeper::default();
        let old = licensed(NOW - VALIDATE_EVERY.as_secs() - 1);
        next(&mut k, &old, true, NOW);
        k.finished(Done::Validated(answered(Validation::NotFound)), at(NOW));
        assert!(next(&mut k, &old, true, NOW + 60).is_some());
        k.finished(
            Done::Validated(answered(Validation::Trouble(unreachable()))),
            at(NOW + 60),
        );
        // Not asked again at once, nor every second.
        assert_eq!(next(&mut k, &old, true, NOW + 61), None);
        assert_eq!(next(&mut k, &old, true, NOW + 119), None);
        assert!(next(&mut k, &old, true, NOW + 120).is_some());
        k.finished(
            Done::Validated(answered(Validation::Trouble(Trouble::RateLimited(
                Duration::from_secs(600),
            )))),
            at(NOW + 120),
        );
        assert_eq!(next(&mut k, &old, true, NOW + 719), None);
        assert!(next(&mut k, &old, true, NOW + 720).is_some());
        // Then a second 404 is invalid.
        k.finished(
            Done::Validated(answered(Validation::NotFound)),
            at(NOW + 720),
        );
        assert_eq!(
            next(&mut k, &old, true, NOW + 720),
            Some(refusal(KEY.into(), LicenseRefusal::Invalid, "act-1".into()).task("x"))
        );
    }

    #[test]
    fn a_stall_survives_a_restart() {
        let mut k = Keeper::default();
        activate(&mut k, NOW);
        k.reported(Err(Some("license_stale")), Some(&pending()), at(NOW));
        next(&mut k, &pending(), false, NOW);
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        let mut back = reread(&k.saved());
        assert_eq!(back.problem_for(KEY), Some(Problem::Refused));
        assert_eq!(next(&mut back, &pending(), false, NOW + 3600), None);
        // Lifted, and saved so.
        back.retry_now(KEY);
        assert!(back.dirty());
        assert_eq!(reread(&back.saved()).problem_for(KEY), None);
    }

    #[test]
    fn a_stall_is_for_the_record_as_it_was_at_the_refusal() {
        let mut k = Keeper::default();
        activate(&mut k, NOW);
        k.reported(Err(Some("license_stale")), Some(&pending()), at(NOW));
        // The record changed before the next look at it: the stall goes.
        next(&mut k, &license(LicenseState::None), false, NOW);
        assert_eq!(k.problem_for(KEY), None);
    }

    #[test]
    fn a_grant_polar_answered_unreadably_is_not_asked_for_again() {
        let mut k = Keeper::default();
        next(&mut k, &pending(), false, NOW);
        k.finished(Done::Activated(answered(Activation::Unreadable)), at(NOW));
        assert_eq!(k.problem_for(KEY), Some(Problem::Refused));
        assert_eq!(next(&mut k, &pending(), false, NOW + 3600), None);
    }

    #[test]
    fn the_agent_is_not_held_up_by_a_slow_polar() {
        let mut k = reread(&format!(
            r#"{{"outbox":[{{"free":{{"key":"{KEY}","activation_id":"act-1"}}}},{{"report":{{"key":"{OTHER}","activation_id":"act-9","customer_email":"","validated_at":{NOW}}}}}]}}"#
        ));
        let other = License {
            key: OTHER.into(),
            ..pending()
        };
        assert_eq!(next(&mut k, &other, false, NOW), Some(free("act-1")));
        assert!(matches!(
            next(&mut k, &other, false, NOW),
            Some(Task::Report(_))
        ));
        assert_eq!(next(&mut k, &other, false, NOW), None, "one each");
        k.reported(Ok(()), None, at(NOW));
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        assert!(!k.busy());
        assert_eq!(k.saved(), Keeper::default().saved());
    }

    #[test]
    fn validated_at_follows_polar_unless_it_is_far_off() {
        let now = at(NOW);
        assert_eq!(stamp(Some(at(NOW - 10)), now), NOW - 10);
        assert_eq!(stamp(Some(at(NOW + 3600)), now), NOW + 3600);
        assert_eq!(stamp(Some(at(NOW + 2 * 86_400)), now), NOW);
        assert_eq!(stamp(Some(at(1_000)), now), NOW);
        assert_eq!(stamp(None, now), NOW);
    }

    #[test]
    fn keys_and_emails() {
        assert!(plausible_key(KEY));
        assert!(!plausible_key("INPUTWEAVE-short"));
        assert!(!plausible_key(&format!("{KEY} x")));
        assert!(!plausible_key(&"A".repeat(97)));
        assert_eq!(short_email("someone@example.com"), "s•••@example.com");
        assert_eq!(short_email("小明@example.com"), "小•••@example.com");
        assert_eq!(short_email("nobody"), "");
        assert_eq!(short_email("@example.com"), "");
    }

    #[test]
    fn work_nobody_started_here_is_a_key_applied_from_elsewhere_or_a_revoke() {
        let applied = License {
            source_device_id: "mac".into(),
            ..pending()
        };
        assert!(unasked_work(Some(&applied)));
        assert!(!unasked_work(Some(&pending())), "entered here");
        assert!(unasked_work(Some(&license(LicenseState::Revoked))));
        for s in [
            LicenseState::None,
            LicenseState::Licensed,
            LicenseState::SeatsFull,
            LicenseState::Invalid,
            LicenseState::Unavailable,
        ] {
            assert!(!unasked_work(Some(&license(s))), "{s:?}");
        }
        assert!(!unasked_work(None));
    }

    #[test]
    fn an_unknown_state_reads_as_none() {
        let l = License {
            state: 99,
            ..Default::default()
        };
        assert_eq!(state(&l), LicenseState::None);
        assert!(!is_licensed(Some(&l)));
        assert!(is_licensed(Some(&licensed(NOW))));
        assert!(!is_licensed(None));
    }

    /// RemoveLicense of the license with KEY and `activation_id`.
    fn removing(activation_id: &str) -> Task {
        Task::Remove {
            key: KEY.into(),
            activation_id: activation_id.into(),
        }
    }

    fn revoke_task(device_id: &str, activation_id: &str, deactivated: bool) -> Task {
        Task::Revoke {
            device_id: device_id.into(),
            activation_id: activation_id.into(),
            deactivated,
        }
    }

    /// Every request due now, as `next` hands them out with nothing
    /// answered.
    fn due(k: &mut Keeper, l: &License, now: u64) -> Vec<Task> {
        std::iter::from_fn(|| next(k, l, false, now)).collect()
    }

    /// Revokes the license on the PC, licensed with act-pc, and has Polar
    /// answer the free with `answer`.
    fn revoke_answered(answer: Deactivation) -> Keeper {
        let l = licensed(NOW);
        let mut k = Keeper::default();
        k.revoke(Some(&l), "pc", "act-pc");
        assert!(k.dirty(), "kept on disk");
        assert_eq!(due(&mut k, &l, NOW), [free("act-pc")], "Polar first");
        k.finished(Done::Deactivated(answer), at(NOW));
        k
    }

    #[test]
    fn a_revoke_asks_polar_once_then_tells_the_agent_whether_it_freed_it() {
        let l = licensed(NOW);
        // Freed, or gone already (404 reads as freed).
        let mut k = revoke_answered(Deactivation::Done);
        assert_eq!(due(&mut k, &l, NOW), [revoke_task("pc", "act-pc", true)]);
        assert!(!k.revoked(Ok(()), at(NOW)));
        assert_eq!(due(&mut k, &l, NOW + 3600), []);
        assert_eq!(k.saved(), r#"{"outbox":[],"stalled":null}"#);
        assert!(k.licensed(Some(&l)), "this computer stays licensed");
        assert_eq!(k.removal(), Removal::None);
        // Not freed: the device frees it itself, and Polar is not asked
        // again.
        for answer in [
            Deactivation::Trouble(unreachable()),
            Deactivation::Trouble(Trouble::RateLimited(Duration::from_secs(30))),
            Deactivation::Refused,
        ] {
            let mut k = revoke_answered(answer.clone());
            assert_eq!(
                due(&mut k, &l, NOW),
                [revoke_task("pc", "act-pc", false)],
                "{answer:?}"
            );
            k.revoked(Ok(()), at(NOW));
            assert_eq!(due(&mut k, &l, NOW + 3600), [], "{answer:?}");
        }
    }

    #[test]
    fn while_polar_asks_to_wait_a_revoke_does_not_ask_it() {
        let l = licensed(NOW);
        // A 429 to the free of another revoke.
        let mut k = revoke_answered(Deactivation::Trouble(Trouble::RateLimited(
            Duration::from_secs(120),
        )));
        assert!(k.polar_waits(at(NOW + 119)));
        k.revoke(Some(&l), "mac", "act-mac");
        let tasks = due(&mut k, &l, NOW + 1);
        assert_eq!(tasks, [revoke_task("pc", "act-pc", false)]);
        k.revoked(Ok(()), at(NOW + 1));
        assert_eq!(
            due(&mut k, &l, NOW + 1),
            [revoke_task("mac", "act-mac", false)]
        );
        // The wait is kept on disk, and over in time.
        let back = reread(&k.saved());
        assert!(back.polar_waits(at(NOW + 60)));
        assert!(!back.polar_waits(at(NOW + 120)));
        let mut later = Keeper::default();
        later.revoke(Some(&l), "mac", "act-mac");
        later.polar_answered(
            &Deactivation::Trouble(Trouble::RateLimited(Duration::from_secs(60))),
            at(NOW),
        );
        assert_eq!(due(&mut later, &l, NOW + 60), [free("act-mac")]);
        // What the caller asked Polar itself counts only when it asks to
        // wait.
        let mut k = Keeper::default();
        k.polar_answered(&Deactivation::Trouble(unreachable()), at(NOW));
        k.polar_answered(&Deactivation::Refused, at(NOW));
        assert!(!k.polar_waits(at(NOW)));
    }

    #[test]
    fn a_revoke_survives_a_restart_and_waits_for_the_agent() {
        let l = licensed(NOW);
        // Not asked yet: Polar is asked after the restart.
        let mut k = Keeper::default();
        k.revoke(Some(&l), "pc", "act-pc");
        let mut back = reread(&k.saved());
        assert_eq!(due(&mut back, &l, NOW), [free("act-pc")]);
        back.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        // Decided, and the agent unreachable: kept as decided.
        let mut back = reread(&back.saved());
        assert_eq!(
            back.next(Some(&l), "", false, false, at(NOW)),
            None,
            "only Polar without the agent"
        );
        assert_eq!(due(&mut back, &l, NOW), [revoke_task("pc", "act-pc", true)]);
        assert!(!back.revoked(Err(None), at(NOW)));
        assert_eq!(due(&mut back, &l, NOW + 1), [], "it waits");
        assert_eq!(
            due(&mut back, &l, NOW + 60),
            [revoke_task("pc", "act-pc", true)]
        );
        assert!(!back.revoked(Err(Some("license_unavailable")), at(NOW + 60)));
        let mut back = reread(&back.saved());
        assert_eq!(due(&mut back, &l, NOW), [revoke_task("pc", "act-pc", true)]);
        // Refused for good (the device's activation changed meanwhile):
        // dropped, and said.
        assert!(back.revoked(Err(Some("license_stale")), at(NOW)));
        assert_eq!(due(&mut back, &l, NOW + 3600), []);
        assert_eq!(back.saved(), r#"{"outbox":[],"stalled":null}"#);
    }

    #[test]
    fn a_revoke_without_an_activation_goes_to_the_agent_alone_and_once() {
        let l = licensed(NOW);
        let mut k = Keeper::default();
        k.revoke(Some(&l), "pc", "");
        k.revoke(Some(&l), "pc", "");
        k.revoke(Some(&l), "", "act-x");
        assert_eq!(due(&mut k, &l, NOW), [revoke_task("pc", "", false)]);
        k.revoked(Ok(()), at(NOW));
        // With an activation but no key to ask Polar with: the device frees
        // it.
        let mut k = Keeper::default();
        k.revoke(Some(&License::default()), "pc", "act-pc");
        assert_eq!(due(&mut k, &l, NOW), [revoke_task("pc", "act-pc", false)]);
        // Twice the same: once.
        let mut k = Keeper::default();
        k.revoke(Some(&l), "pc", "act-pc");
        k.revoke(Some(&l), "pc", "act-pc");
        assert_eq!(due(&mut k, &l, NOW), [free("act-pc")]);
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        assert_eq!(due(&mut k, &l, NOW), [revoke_task("pc", "act-pc", true)]);
        k.revoked(Ok(()), at(NOW));
        assert_eq!(due(&mut k, &l, NOW), []);
    }

    #[test]
    fn remove_license_keeps_the_revokes_and_sends_them_first() {
        let l = licensed(NOW);
        // A revoke waiting for the agent is tried again at once.
        let mut k = Keeper::default();
        k.revoke(Some(&l), "pc", "");
        assert_eq!(due(&mut k, &l, NOW), [revoke_task("pc", "", false)]);
        k.revoked(Err(None), at(NOW));
        k.remove(Some(&l));
        assert_eq!(due(&mut k, &l, NOW + 1), [revoke_task("pc", "", false)]);
        k.revoked(Ok(()), at(NOW + 1));
        assert_eq!(due(&mut k, &l, NOW + 1), [removing("act-1")]);
        let mut k = Keeper::default();
        k.revoke(Some(&l), "pc", "act-pc");
        k.remove(Some(&l));
        // The revoke asks Polar, and the remove waits for the revoke.
        assert_eq!(due(&mut k, &l, NOW), [free("act-pc")]);
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        assert_eq!(due(&mut k, &l, NOW), [revoke_task("pc", "act-pc", true)]);
        k.revoked(Ok(()), at(NOW));
        assert_eq!(due(&mut k, &l, NOW), [removing("act-1")]);
        assert!(!k.removed(Ok(()), at(NOW)));
        assert_eq!(due(&mut k, &License::default(), NOW), [free("act-1")]);
    }

    fn revoked_here() -> License {
        License {
            state: LicenseState::Revoked as i32,
            ..licensed(NOW)
        }
    }

    #[test]
    fn a_license_revoked_here_is_freed_then_removed_once_and_without_a_word() {
        let l = revoked_here();
        let mut k = Keeper::default();
        assert!(!k.licensed(Some(&l)));
        // Validated long ago, shown: no validate, only the free.
        let old = License {
            validated_at: NOW - 40 * 24 * 3600,
            ..l.clone()
        };
        assert_eq!(
            std::iter::from_fn(|| next(&mut k, &old, true, NOW)).collect::<Vec<_>>(),
            [free("act-1")]
        );
        assert!(k.dirty());
        assert_eq!(k.removal(), Removal::None, "nothing shows");
        // Polar not reached: tried again, nothing removed meanwhile, and
        // still nothing shows, also after a restart.
        k.finished(
            Done::Deactivated(Deactivation::Trouble(unreachable())),
            at(NOW),
        );
        assert_eq!(k.removal(), Removal::None);
        assert_eq!(due(&mut k, &l, NOW + 1), []);
        let mut back = reread(&k.saved());
        assert_eq!(back.removal(), Removal::None);
        assert_eq!(due(&mut back, &l, NOW), [free("act-1")]);
        // Not without the agent's record to check it against.
        assert_eq!(back.next(Some(&l), "", false, false, at(NOW)), None);
        // Freed (404: gone already): then removed, once.
        back.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        assert_eq!(due(&mut back, &l, NOW), [removing("act-1")]);
        assert_eq!(due(&mut back, &l, NOW), []);
        // The agent did not answer: tried again, nothing shows.
        assert!(!back.removed(Err(None), at(NOW)));
        assert_eq!(back.removal(), Removal::None);
        let mut again = reread(&back.saved());
        assert_eq!(due(&mut again, &l, NOW), [removing("act-1")]);
        assert!(!again.removed(Ok(()), at(NOW)));
        // Nothing more to free afterwards.
        assert_eq!(due(&mut again, &License::default(), NOW + 3600), []);
        assert_eq!(again.removal(), Removal::None);
    }

    #[test]
    fn a_removal_of_a_revoked_license_refused_for_good_is_not_tried_again_until_it_changes() {
        let l = License {
            activation_id: String::new(),
            ..revoked_here()
        };
        let mut k = Keeper::default();
        assert_eq!(due(&mut k, &l, NOW), [removing("")], "nothing to free");
        assert!(
            !k.removed(Err(Some("unsupported")), at(NOW)),
            "not said: the user did nothing"
        );
        assert_eq!(due(&mut k, &l, NOW + 3600), []);
        assert_eq!(due(&mut reread(&k.saved()), &l, NOW), []);
        // Revoked again with another activation: freed and removed then.
        let other = License {
            activation_id: "act-2".into(),
            state: LicenseState::Licensed as i32,
            ..l.clone()
        };
        assert_eq!(due(&mut k, &other, NOW), []);
        let again = License {
            state: LicenseState::Revoked as i32,
            ..other
        };
        assert_eq!(due(&mut k, &again, NOW), [free("act-2")]);
    }

    #[test]
    fn the_work_on_a_license_revoked_here_follows_the_record() {
        let l = revoked_here();
        // The same key applied or entered again: the agent lists the
        // revoked activation to free and starts over. Freed all the same,
        // once, and the agent told; nothing removed.
        for freed_first in [false, true] {
            let mut k = Keeper::default();
            assert_eq!(due(&mut k, &l, NOW), [free("act-1")]);
            if freed_first {
                k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
                assert_eq!(due(&mut k, &l, NOW), [removing("act-1")]);
                k.removed(Err(None), at(NOW));
            } else {
                k.finished(
                    Done::Deactivated(Deactivation::Trouble(unreachable())),
                    at(NOW),
                );
            }
            let again = License {
                to_free: vec![LicenseActivation {
                    key: KEY.into(),
                    activation_id: "act-1".into(),
                }],
                ..pending()
            };
            let mut tasks = due(&mut k, &again, NOW + 60);
            if !freed_first {
                assert_eq!(tasks, [free("act-1")]);
                k.finished(Done::Deactivated(Deactivation::Done), at(NOW + 60));
                tasks = due(&mut k, &again, NOW + 60);
            }
            assert!(
                tasks.contains(&Task::Freed {
                    activation_id: "act-1".into()
                }),
                "{freed_first}: {tasks:?}"
            );
            assert!(!tasks.contains(&free("act-1")), "never twice");
            assert!(!tasks.iter().any(|t| matches!(t, Task::Remove { .. })));
        }
        // Unreadable for now: kept, and tried again.
        let mut k = Keeper::default();
        waiting_free(&mut k, &l);
        let unreadable = License {
            state: LicenseState::Unavailable as i32,
            ..Default::default()
        };
        assert_eq!(due(&mut k, &unreadable, NOW + 60), [free("act-1")]);
    }

    #[test]
    fn the_users_removal_answered_stale_is_sent_again_for_the_license_now() {
        let l = licensed(NOW);
        let removal_of = |key: &str, activation_id: &str| Task::Remove {
            key: key.into(),
            activation_id: activation_id.into(),
        };
        let now_other = License {
            key: OTHER.into(),
            activation_id: "act-2".into(),
            ..licensed(NOW)
        };
        // Stale once, then taken: sent again for the license now, and both
        // seats freed.
        let mut k = Keeper::default();
        k.remove(Some(&l));
        assert_eq!(due(&mut k, &l, NOW), [removing("act-1")]);
        assert!(!k.removed(Err(Some("license_stale")), at(NOW)));
        assert_eq!(k.removal(), Removal::Removing, "still on its way");
        assert_eq!(
            k.next(Some(&now_other), "", false, false, at(NOW)),
            Some(free("act-1")),
            "not without the record to read"
        );
        assert_eq!(due(&mut k, &now_other, NOW), [removal_of(OTHER, "act-2")]);
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        assert!(!k.removed(Ok(()), at(NOW)));
        assert_eq!(
            due(&mut k, &License::default(), NOW),
            [Task::Free {
                key: OTHER.into(),
                activation_id: "act-2".into()
            }]
        );
        // Stale three times: given up, and said once.
        let mut k = Keeper::default();
        k.remove(Some(&l));
        for n in 1..=3 {
            let tasks = due(&mut k, &now_other, NOW);
            assert!(
                tasks.iter().any(|t| matches!(t, Task::Remove { .. })),
                "{n}: {tasks:?}"
            );
            assert_eq!(
                k.removed(Err(Some("license_stale")), at(NOW)),
                n == 3,
                "{n}"
            );
        }
        assert!(!k.queued(Op::is_remove));
        assert_eq!(k.removal(), Removal::None);
        // Stale, and the record holds no license any more: done.
        let mut k = Keeper::default();
        k.remove(Some(&l));
        due(&mut k, &l, NOW);
        k.removed(Err(Some("license_stale")), at(NOW));
        assert_eq!(due(&mut k, &License::default(), NOW), [free("act-1")]);
        assert!(!k.queued(Op::is_remove));
        // Waiting to be sent again, it survives a restart.
        let mut k = Keeper::default();
        k.remove(Some(&l));
        due(&mut k, &l, NOW);
        k.removed(Err(Some("license_stale")), at(NOW));
        let mut back = reread(&k.saved());
        let tasks = due(&mut back, &now_other, NOW);
        assert!(tasks.contains(&removal_of(OTHER, "act-2")), "{tasks:?}");
    }

    #[test]
    fn a_removal_past_a_newer_license_is_done() {
        // Remove License names the license as the user confirmed it, also
        // after a restart.
        let l = licensed(NOW);
        let mut k = Keeper::default();
        k.remove(Some(&l));
        let mut back = reread(&k.saved());
        assert_eq!(due(&mut back, &l, NOW), [removing("act-1")]);
        // A removal saved before it named the license: its seat's, or
        // whatever is there without one.
        let older =
            format!(r#"{{"outbox":[{{"remove":{{"key":"{KEY}","activation_id":"act-7"}}}}]}}"#);
        assert_eq!(due(&mut reread(&older), &l, NOW), [removing("act-7")]);
        let oldest = r#"{"outbox":[{"remove":{}}]}"#;
        assert_eq!(
            due(&mut reread(oldest), &l, NOW),
            [Task::Remove {
                key: String::new(),
                activation_id: String::new()
            }]
        );
        // The removal of a license revoked here, stale as the same key came
        // again: done, and what it freed told freed once listed, without
        // freeing it again.
        let revoked = revoked_here();
        let mut k = Keeper::default();
        assert_eq!(due(&mut k, &revoked, NOW), [free("act-1")]);
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        assert_eq!(due(&mut k, &revoked, NOW), [removing("act-1")]);
        assert!(!k.removed(Err(Some("license_stale")), at(NOW)));
        let again = License {
            to_free: vec![LicenseActivation {
                key: KEY.into(),
                activation_id: "act-1".into(),
            }],
            ..pending()
        };
        let tasks = due(&mut k, &again, NOW);
        assert!(
            tasks.contains(&Task::Freed {
                activation_id: "act-1".into()
            }),
            "{tasks:?}"
        );
        assert!(!tasks.contains(&free("act-1")), "{tasks:?}");
        k.freed(Ok(()), at(NOW));
        let mut done = again.clone();
        done.to_free.clear();
        assert!(
            due(&mut k, &done, NOW)
                .iter()
                .all(|t| matches!(t, Task::Activate { .. }))
        );
    }

    /// Another key entered here, the revoked license's activation listed
    /// to free with its key.
    fn entered_after_revoked(listed: bool) -> License {
        License {
            key: OTHER.into(),
            to_free: if listed {
                vec![LicenseActivation {
                    key: KEY.into(),
                    activation_id: "act-1".into(),
                }]
            } else {
                Vec::new()
            },
            ..pending()
        }
    }

    #[test]
    fn a_license_revoked_here_and_replaced_by_another_key_is_still_freed_once() {
        let l = revoked_here();
        // Polar not reached before another key was entered: freed all the
        // same, once, and the agent told; nothing removed.
        let mut k = Keeper::default();
        assert_eq!(due(&mut k, &l, NOW), [free("act-1")]);
        k.finished(
            Done::Deactivated(Deactivation::Trouble(unreachable())),
            at(NOW),
        );
        let entered = entered_after_revoked(true);
        let activated = |tasks: &[Task]| {
            tasks
                .iter()
                .filter(|t| matches!(t, Task::Activate { key, .. } if key == OTHER))
                .count()
        };
        // The new key's activation waits behind the free (Polar's lane).
        let tasks = due(&mut k, &entered, NOW + 60);
        assert_eq!(tasks, [free("act-1")], "not queued twice");
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW + 60));
        let tasks = due(&mut k, &entered, NOW + 60);
        assert!(
            tasks.contains(&Task::Freed {
                activation_id: "act-1".into()
            }),
            "{tasks:?}"
        );
        assert_eq!(activated(&tasks), 1);
        assert!(!tasks.iter().any(|t| matches!(t, Task::Remove { .. })));
        k.freed(Ok(()), at(NOW + 60));
        let mut after = entered.clone();
        after.to_free.clear();
        k.finished(
            Done::Activated(answered(Activation::Trouble(unreachable()))),
            at(NOW + 60),
        );
        assert!(
            due(&mut k, &after, NOW + 3600)
                .iter()
                .all(|t| matches!(t, Task::Activate { .. })),
            "nothing more about act-1"
        );
        // Freed before the other key was entered, the removal not taken
        // yet: not freed again; the agent told, not removed.
        let mut k = Keeper::default();
        assert_eq!(due(&mut k, &l, NOW), [free("act-1")]);
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        assert_eq!(due(&mut k, &l, NOW), [removing("act-1")]);
        k.removed(Err(None), at(NOW));
        let mut back = reread(&k.saved());
        let tasks = due(&mut back, &entered, NOW + 60);
        assert!(!tasks.contains(&free("act-1")), "{tasks:?}");
        assert!(
            !tasks.iter().any(|t| matches!(t, Task::Remove { .. })),
            "{tasks:?}"
        );
        assert!(tasks.contains(&Task::Freed {
            activation_id: "act-1".into()
        }));
        // Not listed to free (nothing for the agent to forget): freed, and
        // nothing more.
        let mut k = Keeper::default();
        waiting_free(&mut k, &l);
        let unlisted = entered_after_revoked(false);
        let tasks = due(&mut k, &unlisted, NOW + 60);
        assert_eq!(tasks, [free("act-1")]);
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW + 60));
        let tasks = due(&mut k, &unlisted, NOW + 60);
        assert!(
            tasks.iter().all(|t| matches!(t, Task::Activate { .. })),
            "{tasks:?}"
        );
    }

    fn waiting_free(k: &mut Keeper, l: &License) {
        assert_eq!(due(k, l, NOW), [free("act-1")]);
        k.finished(
            Done::Deactivated(Deactivation::Trouble(unreachable())),
            at(NOW),
        );
    }

    #[test]
    fn an_activation_listed_to_free_is_freed_and_the_agent_told() {
        // The app started after another key was entered here.
        let l = License {
            to_free: vec![LicenseActivation {
                key: OTHER.into(),
                activation_id: "act-old".into(),
            }],
            ..licensed(NOW)
        };
        let old = || Task::Free {
            key: OTHER.into(),
            activation_id: "act-old".into(),
        };
        let told = || Task::Freed {
            activation_id: "act-old".into(),
        };
        let mut k = Keeper::default();
        assert_eq!(due(&mut k, &l, NOW), [old()]);
        assert!(k.licensed(Some(&l)), "another key's activation");
        // Kept and tried again until Polar answers, also after a restart;
        // not while Polar asks to wait.
        k.finished(
            Done::Deactivated(Deactivation::Trouble(Trouble::RateLimited(
                Duration::from_secs(120),
            ))),
            at(NOW),
        );
        let mut back = reread(&k.saved());
        assert_eq!(due(&mut back, &l, NOW + 60), []);
        assert_eq!(due(&mut back, &l, NOW + 120), [old()]);
        // Gone already (404 reads as freed): the agent is told, also after
        // a restart, until it answers.
        back.finished(Done::Deactivated(Deactivation::Done), at(NOW + 120));
        let mut back = reread(&back.saved());
        assert_eq!(
            back.next(Some(&l), "", false, false, at(NOW + 120)),
            None,
            "the agent's part waits for it"
        );
        assert_eq!(due(&mut back, &l, NOW + 120), [told()]);
        back.freed(Err(None), at(NOW + 120));
        assert_eq!(due(&mut back, &l, NOW + 180), [told()]);
        back.freed(Ok(()), at(NOW + 180));
        // Done; the agent no longer lists it.
        let mut done = l.clone();
        done.to_free.clear();
        assert_eq!(due(&mut back, &done, NOW + 3600), []);
        // Refused: done too, and not freed again while it is still listed.
        let mut k = Keeper::default();
        assert_eq!(due(&mut k, &l, NOW), [old()]);
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        assert_eq!(due(&mut k, &l, NOW), [told()]);
        k.freed(Err(Some("unsupported")), at(NOW));
        assert_eq!(due(&mut k, &l, NOW + 3600), []);
        // Listed twice, or while the revoked license's free has it: once.
        let twice = License {
            to_free: vec![l.to_free[0].clone(), l.to_free[0].clone()],
            ..l.clone()
        };
        let mut k = Keeper::default();
        assert_eq!(due(&mut k, &twice, NOW), [old()]);
        assert_eq!(due(&mut k, &twice, NOW), []);
        // A remove of this computer's license keeps it.
        k.remove(Some(&l));
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        assert!(due(&mut k, &l, NOW).contains(&told()));
    }

    #[test]
    fn revokes_and_a_quiet_removal_read_back_and_junk_is_left_out() {
        let text = format!(
            r#"{{"outbox":[
                {{"revoke":{{"key":"{KEY}","device_id":"pc","activation_id":"act-pc","deactivated":true}}}},
                {{"revoke":{{"key":"{KEY}","device_id":"mac","activation_id":"act-mac","deactivated":null}}}},
                {{"revoke":{{"device_id":"studio","activation_id":"act-s"}}}},
                {{"revoke":{{"device_id":"","activation_id":"x"}}}},
                {{"revoke":{{"device_id":"mac","activation_id":"a b"}}}},
                {{"revoke":{{"device_id":"mac"}}}},
                {{"free":{{"key":"{KEY}","activation_id":"act-1","seat":false}}}},
                {{"remove":{{"revoked_key":"{KEY}","revoked_activation_id":"act-1"}}}}
            ],"stalled":null,"polar_not_before":0}}"#
        );
        let mut k = reread(&text);
        assert_eq!(k.removal(), Removal::None, "the removal is the quiet one");
        assert!(!k.polar_waits(at(NOW)));
        let l = revoked_here();
        // Polar for the Mac's revoke; the agent for the PC's (decided) and
        // the Studio's (no key: not freed), the removal after both and the
        // free of the revoked activation.
        assert_eq!(
            due(&mut k, &l, NOW),
            [revoke_task("pc", "act-pc", true), free("act-mac")]
        );
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        k.revoked(Ok(()), at(NOW));
        assert_eq!(
            due(&mut k, &l, NOW),
            [revoke_task("mac", "act-mac", true), free("act-1")]
        );
        k.finished(Done::Deactivated(Deactivation::Done), at(NOW));
        k.revoked(Ok(()), at(NOW));
        assert_eq!(
            due(&mut k, &l, NOW),
            [revoke_task("studio", "act-s", false)]
        );
        k.revoked(Ok(()), at(NOW));
        assert_eq!(due(&mut k, &l, NOW), [removing("act-1")]);
        assert!(!k.removed(Ok(()), at(NOW)));
        assert_eq!(due(&mut k, &License::default(), NOW), []);
    }
}
