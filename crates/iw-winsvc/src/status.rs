//! The states the service reports to the service control manager, free of
//! FFI so the transitions are tested on every OS.

#![cfg_attr(not(windows), allow(dead_code))]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    StartPending,
    Running,
    StopPending,
    Stopped,
}

/// One status report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Report {
    pub state: State,
    /// Whether stop, shutdown and session change controls are accepted.
    /// Only a running service takes controls.
    pub accepts_controls: bool,
    /// Counts up while a pending state makes progress; zero otherwise.
    pub checkpoint: u32,
    /// How long until the next report, while pending.
    pub wait_hint_ms: u32,
}

#[derive(Debug, thiserror::Error)]
#[error("the service cannot go from {from:?} to {to:?}")]
pub(crate) struct InvalidTransition {
    from: State,
    to: State,
}

/// Tracks the reported state, so reports only move forward: starting,
/// running, stopping, stopped.
#[derive(Debug)]
pub(crate) struct Status {
    state: State,
    checkpoint: u32,
}

impl Status {
    const WAIT_HINT_MS: u32 = 5_000;

    pub(crate) fn new() -> Self {
        Self {
            state: State::StartPending,
            checkpoint: 0,
        }
    }

    /// The report for moving to `to`. Reporting the same pending state
    /// again signals progress.
    pub(crate) fn enter(&mut self, to: State) -> Result<Report, InvalidTransition> {
        use State::*;
        let allowed = matches!(
            (self.state, to),
            (StartPending, _) | (Running | StopPending, StopPending | Stopped)
        );
        if !allowed {
            return Err(InvalidTransition {
                from: self.state,
                to,
            });
        }
        let pending = matches!(to, StartPending | StopPending);
        self.checkpoint = if pending && to == self.state {
            self.checkpoint + 1
        } else if pending {
            1
        } else {
            0
        };
        self.state = to;
        Ok(Report {
            state: to,
            accepts_controls: to == Running,
            checkpoint: self.checkpoint,
            wait_hint_ms: if pending { Self::WAIT_HINT_MS } else { 0 },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_starts_runs_and_stops() {
        let mut s = Status::new();
        let start = s.enter(State::StartPending).unwrap();
        assert_eq!((start.checkpoint, start.accepts_controls), (1, false));
        assert_eq!(s.enter(State::StartPending).unwrap().checkpoint, 2);
        let running = s.enter(State::Running).unwrap();
        assert_eq!(
            running,
            Report {
                state: State::Running,
                accepts_controls: true,
                checkpoint: 0,
                wait_hint_ms: 0
            }
        );
        let stopping = s.enter(State::StopPending).unwrap();
        assert_eq!((stopping.checkpoint, stopping.accepts_controls), (1, false));
        assert_eq!(s.enter(State::StopPending).unwrap().checkpoint, 2);
        assert_eq!(s.enter(State::Stopped).unwrap().checkpoint, 0);
    }

    #[test]
    fn reports_never_go_back() {
        let mut s = Status::new();
        s.enter(State::Running).unwrap();
        assert!(s.enter(State::StartPending).is_err());
        assert!(s.enter(State::Running).is_err());
        s.enter(State::Stopped).unwrap();
        assert!(s.enter(State::Stopped).is_err());
        assert!(s.enter(State::Running).is_err());
        // A failed start goes straight to stopped.
        assert!(Status::new().enter(State::Stopped).is_ok());
    }
}
