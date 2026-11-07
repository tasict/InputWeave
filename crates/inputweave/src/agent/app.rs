//! Opening the app when the user has to see or decide something
//! (docs/architecture.md §3: the app is the only thing that talks to the
//! user): the pairing code, when another device asks to pair with this one,
//! and a file offer, when the user asked to be asked before receiving; the
//! license, when another device applied it here or revoked it, which only
//! the app takes to Polar. On Windows also files copied on another device,
//! which only the app can put on the clipboard (`app_clipboard`).
//!
//! If no app is connected over IPC then (`ctl` does not count: it cannot
//! ask anything), the agent opens it, at most once every [`INTERVAL`]:
//! - on macOS the agent runs as the user and opens it itself
//!   (`iw_platform::system::open_app`);
//! - on Linux the agent runs as the user too, and has the session's D-Bus
//!   start the app in a systemd unit of its own (D-Bus activation of
//!   `cc.ctsite.inputweave`, its `background` action), so the portals know
//!   it by its app id (`iw_platform::system::open_app`);
//! - on Windows the agent runs as SYSTEM and starts no process in the
//!   user's name; the broker, which runs as the user, starts the app
//!   (`broker::Broker::launch_app`) and does nothing if it runs already.
//!
//! A mock agent, in tests and demos, opens nothing: nobody is there.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tracing::{debug, info, warn};

use super::pairing::PairingStatus;

/// The shortest time between two attempts to open the app.
pub(crate) const INTERVAL: Duration = Duration::from_secs(10);

/// What the user has to see or decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Need {
    PairingCode,
    FileOffer,
    /// Files copied on another device, for the app to put on the
    /// clipboard.
    ClipboardFiles,
    /// The license: another device applied its key here, for the app to
    /// activate, or revoked it here, for the app to free its activation.
    /// The app does it in the background; nothing is shown.
    License,
}

/// Opens the app for a need, or asks for it to be opened; false if that
/// cannot be done.
pub(crate) type Open = Box<dyn Fn(Need) -> bool + Send + Sync>;

pub(crate) struct AppOpener {
    /// `None` where nobody is there to answer.
    open: Option<Open>,
    /// Apps connected over IPC.
    ui_clients: Arc<AtomicUsize>,
    /// When the app was last opened, and whether that could be done.
    last: Mutex<Option<(Instant, bool)>>,
    /// The shortest time between two attempts ([`INTERVAL`]).
    interval: Duration,
}

impl AppOpener {
    pub(crate) fn new(open: Option<Open>, ui_clients: Arc<AtomicUsize>) -> Self {
        Self {
            open,
            ui_clients,
            last: Mutex::new(None),
            interval: INTERVAL,
        }
    }

    /// With another shortest time between two attempts, for tests.
    pub(crate) fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// Opens the app for `need`, unless one is connected already or it was
    /// opened moments ago.
    pub(crate) fn needed(&self, need: Need) {
        self.needed_at(need, Instant::now());
    }

    /// Like [`Self::needed`], at `now`; returns whether it tried.
    fn needed_at(&self, need: Need, now: Instant) -> bool {
        self.open_at(need, now).is_some_and(|(at, _)| at == now)
    }

    /// Opens the app for `need` like [`Self::needed`], and says when it
    /// was asked to open, now or moments ago, for whoever waits for it to
    /// connect; `None` when it is not opened, cannot be, or could not be
    /// moments ago.
    pub(crate) fn open_for(&self, need: Need) -> Option<Instant> {
        self.open_at(need, Instant::now())
            .and_then(|(at, opened)| opened.then_some(at))
    }

    /// When the app was asked to open for `need`, at `now` or moments
    /// before, and whether that could be done.
    fn open_at(&self, need: Need, now: Instant) -> Option<(Instant, bool)> {
        let open = self.open.as_ref()?;
        if self.ui_clients.load(Ordering::SeqCst) > 0 {
            return None;
        }
        {
            let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((at, opened)) = *last
                && now.saturating_duration_since(at) < self.interval
            {
                debug!(?need, "the app was opened moments ago");
                return Some((at, opened));
            }
            *last = Some((now, true));
        }
        let opened = open(need);
        let why = match need {
            Need::ClipboardFiles => "to put files on the clipboard",
            Need::License => "to activate or free the license",
            Need::PairingCode | Need::FileOffer => "to ask the user",
        };
        if opened {
            info!(?need, "opening the app {why}");
        } else {
            warn!(?need, "cannot open the app {why}");
            if let Some(last) = self.last.lock().unwrap_or_else(|e| e.into_inner()).as_mut()
                && last.0 == now
            {
                last.1 = false;
            }
        }
        Some((now, opened))
    }
}

/// Opens the app each time this device starts showing a pairing code.
pub(crate) async fn show_pairing_codes(
    mut pairing: watch::Receiver<PairingStatus>,
    opener: Arc<AppOpener>,
) {
    let mut showing = false;
    loop {
        let shows = matches!(*pairing.borrow_and_update(), PairingStatus::ShowCode { .. });
        if shows && !showing {
            opener.needed(Need::PairingCode);
        }
        showing = shows;
        if pairing.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counting() -> (Option<Open>, Arc<AtomicUsize>) {
        let opened = Arc::new(AtomicUsize::new(0));
        let open: Open = Box::new({
            let opened = opened.clone();
            move |_| {
                opened.fetch_add(1, Ordering::SeqCst);
                true
            }
        });
        (Some(open), opened)
    }

    #[test]
    fn opens_only_without_an_app_and_once_in_a_while() {
        let (open, opened) = counting();
        let clients = Arc::new(AtomicUsize::new(1));
        let opener = AppOpener::new(open, clients.clone());
        let start = Instant::now();

        // An app is connected: it asks the user itself.
        assert!(!opener.needed_at(Need::PairingCode, start));
        clients.store(0, Ordering::SeqCst);
        assert!(opener.needed_at(Need::PairingCode, start));
        assert!(!opener.needed_at(Need::FileOffer, start + Duration::from_secs(1)));
        assert!(!opener.needed_at(Need::FileOffer, start + INTERVAL - Duration::from_millis(1)));
        assert!(opener.needed_at(Need::FileOffer, start + INTERVAL));
        assert_eq!(opened.load(Ordering::SeqCst), 2);
    }

    /// Files on the clipboard wait for the app opened moments ago rather
    /// than open it again, and not for one that could not be opened.
    #[test]
    fn clipboard_files_wait_for_the_app_opened_moments_ago() {
        let (open, opened) = counting();
        let opener = AppOpener::new(open, Arc::new(AtomicUsize::new(0)));
        let start = Instant::now();
        assert!(opener.needed_at(Need::PairingCode, start));
        let later = start + Duration::from_secs(3);
        assert_eq!(
            opener.open_at(Need::ClipboardFiles, later),
            Some((start, true))
        );
        assert_eq!(opened.load(Ordering::SeqCst), 1);
        let past = start + INTERVAL;
        assert_eq!(
            opener.open_at(Need::ClipboardFiles, past),
            Some((past, true))
        );
        assert_eq!(opened.load(Ordering::SeqCst), 2);

        let failing = AppOpener::new(Some(Box::new(|_| false)), Arc::new(AtomicUsize::new(0)));
        assert_eq!(failing.open_for(Need::ClipboardFiles), None);
        // Nor moments later.
        assert_eq!(failing.open_for(Need::ClipboardFiles), None);
    }

    #[test]
    fn a_mock_agent_opens_nothing() {
        let opener = AppOpener::new(None, Arc::new(AtomicUsize::new(0)));
        assert!(!opener.needed_at(Need::PairingCode, Instant::now()));
    }

    #[tokio::test]
    async fn each_code_shown_opens_the_app() {
        use iw_net::PairingCode;

        use super::super::pairing::{Failure, PairingPeer};

        let (open, opened) = counting();
        let opener = Arc::new(AppOpener::new(open, Arc::new(AtomicUsize::new(0))));
        let (status, pairing) = watch::channel(PairingStatus::Idle);
        let task = tokio::spawn(show_pairing_codes(pairing, opener.clone()));
        let peer = PairingPeer {
            id: "mac".to_owned(),
            name: "MacBook Pro".to_owned(),
        };
        let shown = || PairingStatus::ShowCode {
            peer: peer.clone(),
            code: PairingCode::generate(),
            expires_at: std::time::SystemTime::now(),
        };
        let settled = || async {
            for _ in 0..50 {
                tokio::task::yield_now().await;
            }
        };

        status.send_replace(shown());
        settled().await;
        assert_eq!(opened.load(Ordering::SeqCst), 1);
        status.send_replace(PairingStatus::Failed {
            peer: Some(peer.clone()),
            reason: Failure::Rejected,
        });
        settled().await;
        // The next code, past the interval, opens it again.
        *opener.last.lock().unwrap() = None;
        status.send_replace(shown());
        settled().await;
        assert_eq!(opened.load(Ordering::SeqCst), 2);

        drop(status);
        task.await.unwrap();
    }
}
