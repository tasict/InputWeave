//! First-run setup on Linux, as the Windows app's
//! `SetupSteps`: the order of the steps and what each decides. Pairing is
//! the Add Device dialog over the setup window, so connecting and typing
//! the code are one step here.

use iw_proto::app::v1::ConnectionState;

use crate::snapshot::Snapshot;
use crate::text::keys;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Welcome,
    Name,
    /// Access the desktop asks the user for (the Wayland portals: remote
    /// control and input capture), only while the agent reports one it
    /// asks for not granted.
    Permissions,
    Connect,
    Arrange,
    TryIt,
}

impl Step {
    const ORDER: [Step; 6] = [
        Step::Welcome,
        Step::Name,
        Step::Permissions,
        Step::Connect,
        Step::Arrange,
        Step::TryIt,
    ];

    pub fn title_key(self) -> &'static str {
        match self {
            Step::Welcome => keys::SETUP_WELCOME_TITLE,
            Step::Name => keys::SETUP_NAME_TITLE,
            Step::Permissions => keys::SETUP_PERMISSIONS_TITLE,
            Step::Connect => keys::SETUP_CONNECT_TITLE,
            Step::Arrange => keys::SETUP_ARRANGE_TITLE,
            Step::TryIt => keys::SETUP_TRY_TITLE,
        }
    }
}

/// The steps, in order: the permissions step only while one is missing.
pub fn steps(permissions_missing: bool) -> Vec<Step> {
    Step::ORDER
        .into_iter()
        .filter(|s| permissions_missing || *s != Step::Permissions)
        .collect()
}

/// The step's number, from 1, as "Step n of N" shows it.
pub fn number(steps: &[Step], step: Step) -> usize {
    steps.iter().position(|s| *s == step).map_or(0, |i| i + 1)
}

pub fn next(steps: &[Step], step: Step) -> Option<Step> {
    let i = steps.iter().position(|s| *s == step)?;
    steps.get(i + 1).copied()
}

pub fn previous(steps: &[Step], step: Step) -> Option<Step> {
    let i = steps.iter().position(|s| *s == step)?;
    i.checked_sub(1).map(|i| steps[i])
}

/// The steps as the user goes through them from `current`, "Step n of N"
/// and the progress bar counting these: the permissions step is one while
/// it is shown, once it was (`seen`), or while something there is to do
/// (`permissions`: what `Snapshot::permission_to_ask` says now) and the
/// user has not gone past it; so it joins whenever the agent's answer
/// comes, or a permission is lost, before then.
pub fn shown(steps: &[Step], current: Step, permissions: bool, seen: bool) -> Vec<Step> {
    let at = |step| steps.iter().position(|s| *s == step);
    let before = matches!((at(current), at(Step::Permissions)), (Some(c), Some(p)) if c < p);
    steps
        .iter()
        .copied()
        .filter(|s| *s != Step::Permissions || current == *s || seen || (permissions && before))
        .collect()
}

/// The step to go to from `step`, on (`forward`) or back, among those
/// `shown`.
pub fn go(
    steps: &[Step],
    step: Step,
    forward: bool,
    permissions: bool,
    seen: bool,
) -> Option<Step> {
    let shown = shown(steps, step, permissions, seen);
    if forward {
        next(&shown, step)
    } else {
        previous(&shown, step)
    }
}

/// Whether the user has crossed to another computer since the Try It step
/// opened: the cursor is on another computer now, or the keyboard and
/// mouse came back since. The service keeps running from one setup to the
/// next, so returns from before the step do not count.
pub fn worked(snapshot: &Snapshot, returns_before: u64) -> bool {
    snapshot.controlling_other() || snapshot.status.control_returns > returns_before
}

/// The computer to name in the arrangement and Try It steps: a connected
/// one, else any; `None` when there is none.
pub fn other_name(snapshot: &Snapshot) -> Option<String> {
    let devices = &snapshot.status.devices;
    devices
        .iter()
        .find(|d| d.state() == ConnectionState::Connected)
        .or_else(|| devices.first())
        .map(|d| d.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Link;
    use iw_proto::app::v1::{DeviceStatus, Status};

    fn model(devices: Vec<(&str, ConnectionState)>) -> Snapshot {
        Snapshot {
            link: Link::Connected,
            status: Status {
                this_device_id: "me".into(),
                control_returns: 7,
                devices: devices
                    .into_iter()
                    .map(|(name, state)| DeviceStatus {
                        id: name.to_lowercase(),
                        name: name.into(),
                        state: state as i32,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn has_the_permissions_step_only_while_one_is_missing() {
        let usual = steps(false);
        let all = steps(true);
        assert_eq!(usual.len(), 5);
        assert!(!usual.contains(&Step::Permissions));
        assert_eq!(all.len(), 6);
        assert_eq!(number(&usual, Step::Connect), 3);
        assert_eq!(number(&all, Step::Connect), 4);
        assert_eq!(next(&usual, Step::Name), Some(Step::Connect));
        assert_eq!(next(&all, Step::Name), Some(Step::Permissions));
        assert_eq!(next(&all, Step::TryIt), None);
        assert_eq!(previous(&usual, Step::Connect), Some(Step::Name));
        assert_eq!(previous(&usual, Step::Welcome), None);
    }

    #[test]
    fn works_once_the_cursor_crosses_after_the_step_opened() {
        let mut here = model(vec![("Mac", ConnectionState::Connected)]);
        assert!(!worked(&here, 7));
        here.status.active_device_id = "mac".into();
        assert!(worked(&here, 7));
        here.status.active_device_id = String::new();
        here.status.control_returns = 8;
        assert!(worked(&here, 7));
    }

    #[test]
    fn names_a_connected_computer_first() {
        let model1 = model(vec![
            ("Old PC", ConnectionState::Offline),
            ("Mac", ConnectionState::Connected),
        ]);
        assert_eq!(other_name(&model1).as_deref(), Some("Mac"));
        let offline = model(vec![("Old PC", ConnectionState::Offline)]);
        assert_eq!(other_name(&offline).as_deref(), Some("Old PC"));
        assert_eq!(other_name(&model(vec![])), None);
    }

    #[test]
    fn titles_every_step() {
        for step in steps(true) {
            assert!(!step.title_key().is_empty());
        }
        assert_eq!(Step::TryIt.title_key(), keys::SETUP_TRY_TITLE);
    }

    #[test]
    fn the_steps_counted_are_the_ones_gone_through() {
        let all = steps(true);
        assert_eq!(all.len(), 6);
        let without = shown(&all, Step::Name, false, false);
        assert_eq!(without.len(), 5);
        assert_eq!(number(&without, Step::Connect), 3);
        assert_eq!(
            number(&shown(&all, Step::Name, true, false), Step::Connect),
            4
        );
        // Still counted while it is the step shown (granted meanwhile).
        let here = shown(&all, Step::Permissions, false, true);
        assert_eq!(here.len(), 6);
        assert_eq!(number(&here, Step::Permissions), 3);
        // Going on and back agree with what is counted.
        for step in without.iter().copied() {
            assert_eq!(go(&all, step, true, false, false), next(&without, step));
            assert_eq!(
                go(&all, step, false, false, false),
                previous(&without, step)
            );
        }
    }

    #[test]
    fn the_permissions_step_joins_until_passed_and_stays_once_seen() {
        let all = steps(true);
        // The agent's status came late: before the step, it joins.
        assert_eq!(
            go(&all, Step::Name, true, false, false),
            Some(Step::Connect)
        );
        assert_eq!(
            go(&all, Step::Name, true, true, false),
            Some(Step::Permissions)
        );
        assert_eq!(go(&all, Step::Welcome, true, true, false), Some(Step::Name));
        // Gone past it unseen: it does not join behind the user.
        assert_eq!(
            go(&all, Step::Connect, false, true, false),
            Some(Step::Name)
        );
        assert_eq!(shown(&all, Step::Connect, true, false).len(), 5);
        // Seen: back to it, granted or not.
        assert_eq!(
            go(&all, Step::Connect, false, false, true),
            Some(Step::Permissions)
        );
        assert_eq!(shown(&all, Step::Arrange, false, true).len(), 6);
        assert_eq!(go(&all, Step::TryIt, true, true, true), None);
        assert_eq!(go(&all, Step::Welcome, false, true, false), None);
    }
}
