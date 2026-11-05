//! Which processes the service keeps running, and where. Free of FFI and
//! clocks (time is passed in), so it is tested on every OS.
//!
//! The agent runs in the session attached to the physical console, logged
//! on or not, so it can operate the sign-in screen. The broker runs there
//! only while a user is logged on, with that user's token. Either one is
//! restarted when it exits: after a quarter second if it had been running
//! for a while, then backing off to at most two seconds while it keeps
//! failing. The agent is restarted when a user logs on or off, so its
//! pipe admits the right user, and both move when another session takes
//! over the console.

#![cfg_attr(not(windows), allow(dead_code))]

use std::time::{Duration, Instant};

/// How long a process must run before an exit counts as a fresh failure
/// rather than part of a crash loop.
const STABLE: Duration = Duration::from_secs(10);

/// Restart delays by consecutive quick failures; the last one repeats.
const BACKOFF: [Duration; 4] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Role {
    Agent,
    Broker,
}

impl Role {
    pub(crate) const ALL: [Role; 2] = [Role::Agent, Role::Broker];

    /// The subcommand of the `inputweave` executable for this role.
    pub(crate) fn subcommand(self) -> &'static str {
        match self {
            Role::Agent => "agent",
            Role::Broker => "broker",
        }
    }
}

/// The session attached to the physical console, as last observed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Console {
    /// `None` while no session is attached, as during a switch.
    pub session: Option<u32>,
    pub user_logged_on: bool,
}

/// A session change notification (`WTS_*` in `SERVICE_CONTROL_SESSIONCHANGE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionEvent {
    ConsoleConnect,
    ConsoleDisconnect,
    Logon,
    Logoff,
    Other,
}

impl SessionEvent {
    /// From the event type of a session change notification.
    pub(crate) fn from_code(code: u32) -> Self {
        match code {
            1 => SessionEvent::ConsoleConnect,
            2 => SessionEvent::ConsoleDisconnect,
            5 => SessionEvent::Logon,
            6 => SessionEvent::Logoff,
            _ => SessionEvent::Other,
        }
    }
}

/// Something the service must do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Start the process for `role` in `session`, then report back with
    /// [`Supervisor::exited`] if it could not be started or once it exits.
    Start { role: Role, session: u32 },
    /// Terminate the running process for `role`. It needs no report.
    Stop { role: Role },
}

#[derive(Clone, Copy, Debug, Default)]
struct Slot {
    /// The session the process runs in, while it runs.
    running: Option<u32>,
    started_at: Option<Instant>,
    /// Quick exits in a row.
    failures: usize,
    /// No start before this, after an exit.
    not_before: Option<Instant>,
    restart: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Supervisor {
    console: Console,
    agent: Slot,
    broker: Slot,
}

impl Supervisor {
    fn slot(&mut self, role: Role) -> &mut Slot {
        match role {
            Role::Agent => &mut self.agent,
            Role::Broker => &mut self.broker,
        }
    }

    /// Records the console as observed now. A user logging on or off
    /// restarts the agent.
    pub(crate) fn observe(&mut self, console: Console) {
        if console.session.is_some()
            && console.session == self.console.session
            && console.user_logged_on != self.console.user_logged_on
        {
            self.agent.restart = true;
        }
        self.console = console;
    }

    /// Handles a session change notification. Logons and logoffs in the
    /// console session restart the agent, even when they happen between
    /// two observations; the broker follows the user. Console changes are
    /// picked up by [`Supervisor::observe`].
    pub(crate) fn session_changed(&mut self, event: SessionEvent, session: u32) {
        if matches!(event, SessionEvent::Logon | SessionEvent::Logoff)
            && self.console.session == Some(session)
        {
            self.agent.restart = true;
            self.broker.restart = true;
        }
    }

    /// The process for `role` exited, or could not be started.
    pub(crate) fn exited(&mut self, role: Role, now: Instant) {
        let slot = self.slot(role);
        if slot.running.take().is_none() {
            return;
        }
        let stable = slot
            .started_at
            .take()
            .is_some_and(|at| now.saturating_duration_since(at) >= STABLE);
        slot.failures = if stable { 0 } else { slot.failures + 1 };
        let delay = BACKOFF[slot.failures.min(BACKOFF.len() - 1)];
        slot.not_before = Some(now + delay);
    }

    /// What to do now: stop processes in the wrong session or due for a
    /// restart, and start missing ones whose delay has passed.
    pub(crate) fn actions(&mut self, now: Instant) -> Vec<Action> {
        let console = self.console;
        let mut actions = Vec::new();
        for role in Role::ALL {
            let wanted = match role {
                Role::Agent => console.session,
                Role::Broker => console.session.filter(|_| console.user_logged_on),
            };
            let slot = self.slot(role);
            if let Some(running) = slot.running
                && (Some(running) != wanted || slot.restart)
            {
                actions.push(Action::Stop { role });
                slot.running = None;
                slot.started_at = None;
                // Stopped on purpose: start again without waiting.
                slot.not_before = None;
            }
            slot.restart = false;
            if slot.running.is_none()
                && let Some(session) = wanted
                && slot.not_before.is_none_or(|at| now >= at)
            {
                actions.push(Action::Start { role, session });
                slot.running = Some(session);
                slot.started_at = Some(now);
                slot.not_before = None;
            }
        }
        actions
    }

    /// When [`Supervisor::actions`] next has something to start, if a
    /// restart is waiting out its delay.
    pub(crate) fn next_start(&self) -> Option<Instant> {
        [self.agent, self.broker]
            .iter()
            .filter(|slot| slot.running.is_none())
            .filter_map(|slot| slot.not_before)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn console(session: u32, user: bool) -> Console {
        Console {
            session: Some(session),
            user_logged_on: user,
        }
    }

    fn start(role: Role, session: u32) -> Action {
        Action::Start { role, session }
    }

    fn stop(role: Role) -> Action {
        Action::Stop { role }
    }

    #[test]
    fn the_agent_runs_at_the_sign_in_screen_and_the_broker_only_with_a_user() {
        let t = Instant::now();
        let mut s = Supervisor::default();
        assert_eq!(s.actions(t), vec![]);
        s.observe(console(1, false));
        assert_eq!(s.actions(t), vec![start(Role::Agent, 1)]);
        assert_eq!(s.actions(t), vec![]);

        // Logging on restarts the agent, so its pipe admits the user, and
        // starts the broker.
        s.session_changed(SessionEvent::Logon, 1);
        s.observe(console(1, true));
        assert_eq!(
            s.actions(t),
            vec![
                stop(Role::Agent),
                start(Role::Agent, 1),
                start(Role::Broker, 1)
            ]
        );
        assert_eq!(s.actions(t), vec![]);

        // Logging off stops the broker and restarts the agent.
        s.session_changed(SessionEvent::Logoff, 1);
        s.observe(console(1, false));
        assert_eq!(
            s.actions(t),
            vec![stop(Role::Agent), start(Role::Agent, 1), stop(Role::Broker)]
        );
    }

    #[test]
    fn a_logon_seen_only_in_the_snapshot_still_restarts_the_agent() {
        let t = Instant::now();
        let mut s = Supervisor::default();
        s.observe(console(1, false));
        s.actions(t);
        s.observe(console(1, true));
        assert_eq!(
            s.actions(t),
            vec![
                stop(Role::Agent),
                start(Role::Agent, 1),
                start(Role::Broker, 1)
            ]
        );
    }

    #[test]
    fn both_follow_the_console_to_another_session() {
        let t = Instant::now();
        let mut s = Supervisor::default();
        s.observe(console(1, true));
        s.actions(t);
        // Switching users: the console detaches, then another session
        // attaches.
        s.session_changed(SessionEvent::ConsoleDisconnect, 1);
        s.observe(Console::default());
        assert_eq!(s.actions(t), vec![stop(Role::Agent), stop(Role::Broker)]);
        s.session_changed(SessionEvent::ConsoleConnect, 2);
        s.observe(console(2, true));
        assert_eq!(
            s.actions(t),
            vec![start(Role::Agent, 2), start(Role::Broker, 2)]
        );
    }

    #[test]
    fn events_for_other_sessions_change_nothing() {
        let t = Instant::now();
        let mut s = Supervisor::default();
        s.observe(console(1, true));
        s.actions(t);
        s.session_changed(SessionEvent::Logon, 3);
        s.session_changed(SessionEvent::Other, 1);
        assert_eq!(s.actions(t), vec![]);
    }

    #[test]
    fn exits_restart_within_two_seconds_with_backoff() {
        let t0 = Instant::now();
        let mut s = Supervisor::default();
        s.observe(console(1, false));
        s.actions(t0);

        // Killed after running a while: back in a quarter second.
        let t = t0 + Duration::from_secs(60);
        s.exited(Role::Agent, t);
        assert_eq!(s.next_start(), Some(t + Duration::from_millis(250)));
        assert_eq!(s.actions(t + Duration::from_millis(100)), vec![]);
        let t = t + Duration::from_millis(250);
        assert_eq!(s.actions(t), vec![start(Role::Agent, 1)]);
        assert_eq!(s.next_start(), None);

        // Crashing right after each start backs off, up to two seconds.
        let mut t = t;
        let mut delays = Vec::new();
        for _ in 0..6 {
            s.exited(Role::Agent, t);
            let next = s.next_start().unwrap();
            delays.push(next - t);
            t = next;
            assert_eq!(s.actions(t), vec![start(Role::Agent, 1)]);
        }
        let ms = |ms| Duration::from_millis(ms);
        assert_eq!(
            delays,
            vec![ms(500), ms(1000), ms(2000), ms(2000), ms(2000), ms(2000)]
        );

        // A process that fails to start counts the same; a stale report
        // for a process already stopped is ignored.
        s.exited(Role::Broker, t);
        assert_eq!(s.next_start(), None);
    }

    #[test]
    fn a_process_backing_off_starts_in_the_new_session_when_due() {
        let t = Instant::now();
        let mut s = Supervisor::default();
        s.observe(console(1, true));
        s.actions(t);
        s.exited(Role::Broker, t);
        // The broker is waiting to restart when the console moves.
        s.observe(console(2, true));
        assert_eq!(s.actions(t), vec![stop(Role::Agent), start(Role::Agent, 2)]);
        assert_eq!(
            s.actions(t + Duration::from_secs(2)),
            vec![start(Role::Broker, 2)]
        );
    }

    #[test]
    fn session_event_codes() {
        assert_eq!(SessionEvent::from_code(1), SessionEvent::ConsoleConnect);
        assert_eq!(SessionEvent::from_code(2), SessionEvent::ConsoleDisconnect);
        assert_eq!(SessionEvent::from_code(5), SessionEvent::Logon);
        assert_eq!(SessionEvent::from_code(6), SessionEvent::Logoff);
        assert_eq!(SessionEvent::from_code(7), SessionEvent::Other);
    }

    #[cfg(windows)]
    #[test]
    fn session_event_codes_match_the_windows_crate() {
        use windows::Win32::UI::WindowsAndMessaging as wm;
        assert_eq!(
            SessionEvent::from_code(wm::WTS_CONSOLE_CONNECT),
            SessionEvent::ConsoleConnect
        );
        assert_eq!(
            SessionEvent::from_code(wm::WTS_CONSOLE_DISCONNECT),
            SessionEvent::ConsoleDisconnect
        );
        assert_eq!(
            SessionEvent::from_code(wm::WTS_SESSION_LOGON),
            SessionEvent::Logon
        );
        assert_eq!(
            SessionEvent::from_code(wm::WTS_SESSION_LOGOFF),
            SessionEvent::Logoff
        );
    }
}
