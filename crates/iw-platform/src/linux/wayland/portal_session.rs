//! The life of a portal session the desktop may remember, as remote control
//! (the RemoteDesktop portal) and capture (the InputCapture portal)
//! run theirs: a thread of the portal's
//! own runs one session after another on a small tokio runtime, starting
//! each as the app asks or with the restore token the desktop handed out,
//! and decides from how each ended which token is kept. What sets the
//! portals apart (creating, starting and running a session, the token's
//! key, what is done with commands while no session runs) is theirs
//! (`Sessions`).
//!
//! The desktop asks the user the first time; the restore token it hands out
//! lets later starts go without asking until the user revokes it, so the
//! session resumes at login unattended. Without a token the thread waits
//! for the app to ask, so a dialog never comes out of nowhere.
//!
//! - A token the user revoked makes the desktop ask once more as the agent
//!   starts with it (no portal tells whether a token is still good without
//!   trying it); said no to, it is forgotten and not tried again.
//! - A start with a token that failed before the session was started, as
//!   when the portal is not up yet at login, is tried again a few times. One
//!   that failed after may have used the token up: the token is forgotten
//!   rather than tried again unattended, as that could ask out of nowhere.
//! - A session the desktop closed (the user stopped sharing; the portal
//!   says `Closed`) is not started again by itself, then or at the next
//!   start: its token is forgotten, and the user allows it again from the
//!   app. One that went away without that, as when the compositor, the
//!   portal or the session bus went away, is started again with the token,
//!   a few times, on a new connection to the bus if the old one broke; the
//!   tries start over only once a session ran for a while, so one that
//!   keeps going away does not start again for ever.
//! - The app asking while a start with the token is under way is kept: if
//!   that start does not get a session going, the asked one follows.
//! - Every step of a start has a time limit, and dropping the backend is
//!   heard during each. A start the app asked for waits for the user for
//!   five minutes; a start with the token, which no dialog should hold up,
//!   for half a minute, and then counts as failed after the desktop took
//!   the token, which is forgotten.
//!
//! Which tokens are kept is decided in one place, `run`, from how the
//! session ended: one the user said no to, one of a session the user
//! stopped, and one a failed start may have used up are forgotten.

use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::tokens::Tokens;
use super::translate::Portal as State;

/// How long dropping a backend waits for its session to let go of what it
/// holds.
pub(crate) const STOP_WAIT: Duration = Duration::from_secs(1);

/// The waits before a start with a token that failed is tried again.
pub(crate) const TOKEN_RETRIES: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(45),
];

/// How long a session runs before one that went away has its tries start
/// over.
pub(crate) const STEADY: Duration = Duration::from_secs(30);

/// How long the bus may take to answer whether it is still there.
const BUS_PING: Duration = Duration::from_secs(2);

/// How long each step of starting a session may take: reaching the bus,
/// each of the portal's calls, the libei handshake. The portal may be
/// started on demand at login, which takes a moment.
pub(crate) const STEP: Duration = Duration::from_secs(10);

/// How long a start the app asked for waits for the user to answer the
/// desktop's dialog.
pub(crate) const DIALOG_WAIT: Duration = Duration::from_secs(300);

/// How long a start with the token waits for the desktop, which asks
/// nobody then; one it does ask about (a token the user revoked) and that
/// nobody answers by then ends, and the token goes with it.
pub(crate) const RESTORE_WAIT: Duration = Duration::from_secs(30);

/// How long a session whose libei connection ended waits for the desktop
/// to say it closed it, before taking it to have gone away.
pub(crate) const CLOSED_WAIT: Duration = Duration::from_secs(1);

/// Where a portal's sessions reach it, and how they start again.
pub(crate) struct Lifecycle {
    /// The session bus' address; the one `DBUS_SESSION_BUS_ADDRESS` names
    /// when `None`.
    pub bus: Option<String>,
    pub tokens: Tokens,
    /// The app id to tell portals that let a host app say who it is
    /// (`org.freedesktop.host.portal.Registry`, xdg-desktop-portal 1.19).
    pub app_id: &'static str,
    /// The waits before a failed start with a token is tried again.
    pub retries: Vec<Duration>,
    /// How long a session runs before the tries start over once it went
    /// away.
    pub steady: Duration,
    /// How long a start the app asked for waits for the user's answer.
    pub dialog_wait: Duration,
    /// How long a start with the token waits for the desktop.
    pub restore_wait: Duration,
}

impl Lifecycle {
    /// For the session bus of this process.
    pub(crate) fn new(tokens: Tokens, app_id: &'static str) -> Self {
        Self {
            bus: None,
            tokens,
            app_id,
            retries: TOKEN_RETRIES.to_vec(),
            steady: STEADY,
            dialog_wait: DIALOG_WAIT,
            restore_wait: RESTORE_WAIT,
        }
    }
}

/// A command to a portal's thread, as the sessions' life sees it.
pub(crate) enum Kind<C> {
    /// The app asks for the permission.
    Ask,
    /// The backend is dropped: end, and say so on this.
    Stop(std::sync::mpsc::Sender<()>),
    /// The portal's own, for the session that runs.
    Own(C),
}

/// A portal thread's commands, told apart.
pub(crate) trait Command: Sized {
    fn kind(self) -> Kind<Self>;
}

/// What becomes of a portal's own commands while no session runs to take
/// them: as one starts, or between sessions.
pub(crate) trait Idle<C> {
    fn idle(&mut self, command: C);
}

/// What sets a portal's sessions apart.
pub(crate) trait Sessions {
    type Command: Command;
    /// What takes the portal's own commands while no session does.
    type Idle: Idle<Self::Command>;
    /// The key of its restore token in `portal.toml`.
    const TOKEN_KEY: &'static str;
    /// Its sessions in the log, as in "the remote desktop session".
    const SESSION: &'static str;
    /// What it does, in the log, as in "remote control through the
    /// portal failed".
    const DOES: &'static str;

    /// Shows how the session is, as the app reads it.
    fn set(&self, state: State);

    /// One session, on `conn`: created, started as `start` says, and run
    /// until it ends. Which tokens are kept is `run`'s to decide from how it
    /// ended; the one the desktop hands out as it starts the session is
    /// only written down.
    async fn session(
        &mut self,
        conn: &zbus::Connection,
        start: Start,
        commands: &mut Commands<Self::Command, Self::Idle>,
    ) -> Ended;
}

/// How a session ended.
pub(crate) enum Ended {
    /// The backend is dropped.
    Stop(Option<std::sync::mpsc::Sender<()>>),
    /// The user said no.
    Denied,
    /// The desktop closed it: the user stopped sharing.
    Stopped,
    /// It went away without the desktop closing it: the compositor, the
    /// portal or the bus went away; after running for so long.
    Lost(Duration),
    /// It failed, why, and whether before the desktop was asked to start
    /// it (then a token was not used up).
    Failed { why: String, before_start: bool },
}

/// Why a session starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Why {
    /// The app asked.
    Asked,
    /// With what the user allowed before, unattended.
    Remembered,
}

/// How a session starts.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Start {
    pub why: Why,
    /// How long the desktop may take to start it: the user's answer, or the
    /// desktop's alone with the token.
    pub wait: Duration,
    /// Failing before it started, it is tried again: started with the
    /// token, unattended, with tries left.
    pub retried: bool,
}

/// What to do next, between sessions.
enum Next {
    Asked,
    /// The wait passed.
    Waited,
    Stop(Option<std::sync::mpsc::Sender<()>>),
}

/// What came of a step of starting a session.
pub(crate) enum Step<T> {
    Done(T),
    /// The desktop did not answer in time.
    TimedOut,
    /// The backend is dropped.
    Stop(Option<std::sync::mpsc::Sender<()>>),
}

/// A portal thread's commands, as its sessions start and run.
pub(crate) struct Commands<C, X> {
    rx: mpsc::UnboundedReceiver<C>,
    /// The app asked while a start was under way that did not wait for it.
    asked: bool,
    /// What takes the portal's own commands while no session does.
    pub idle: X,
}

impl<C: Command, X: Idle<C>> Commands<C, X> {
    pub(crate) fn new(rx: mpsc::UnboundedReceiver<C>, idle: X) -> Self {
        Self {
            rx,
            asked: false,
            idle,
        }
    }

    /// The next command, for a session that runs to take.
    pub(crate) async fn recv(&mut self) -> Option<C> {
        self.rx.recv().await
    }

    /// The first `Stop` among the commands. The app asking is noted, for
    /// when the start under way gets no session going; the portal's own
    /// commands go to `idle`.
    pub(crate) async fn stopped(&mut self) -> Option<std::sync::mpsc::Sender<()>> {
        loop {
            match self.rx.recv().await.map(Command::kind) {
                Some(Kind::Stop(done)) => return Some(done),
                Some(Kind::Ask) => self.asked = true,
                Some(Kind::Own(command)) => self.idle.idle(command),
                None => return None,
            }
        }
    }

    /// Runs `step` for at most `limit`, still hearing the backend dropped
    /// and noting the app asking meanwhile.
    pub(crate) async fn step<T>(
        &mut self,
        limit: Duration,
        step: impl std::future::Future<Output = T>,
    ) -> Step<T> {
        tokio::select! {
            done = tokio::time::timeout(limit, step) => done.map_or(Step::TimedOut, Step::Done),
            stop = self.stopped() => Step::Stop(stop),
        }
    }

    /// Waits for the app to ask, or `wait` to pass.
    async fn next(&mut self, wait: Option<Duration>) -> Next {
        let timer = async {
            match wait {
                Some(wait) => tokio::time::sleep(wait).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(timer);
        loop {
            tokio::select! {
                command = self.rx.recv() => match command.map(Command::kind) {
                    Some(Kind::Ask) => return Next::Asked,
                    Some(Kind::Own(command)) => self.idle.idle(command),
                    Some(Kind::Stop(done)) => return Next::Stop(Some(done)),
                    None => return Next::Stop(None),
                },
                () = &mut timer => return Next::Waited,
            }
        }
    }
}

/// Runs a portal's sessions, one after another, until the backend is
/// dropped.
pub(crate) async fn run<S: Sessions>(
    sessions: &mut S,
    mut commands: Commands<S::Command, S::Idle>,
    options: Lifecycle,
) {
    let has_token = || options.tokens.load(S::TOKEN_KEY).is_some();
    let mut bus_conn: Option<zbus::Connection> = None;
    // What the user allowed before, the desktop remembers: start with it,
    // as at login.
    let mut pending = has_token().then_some(Why::Remembered);
    let mut retries = options.retries.clone();
    loop {
        let why = match pending.take() {
            Some(why) => why,
            None => match commands.next(None).await {
                Next::Asked | Next::Waited => Why::Asked,
                Next::Stop(done) => return stop(done),
            },
        };
        // Each time the app asks, a session that then goes away has its
        // tries afresh; a start it asked for that fails is not tried
        // again unattended (only remembered starts are).
        if why == Why::Asked {
            retries = options.retries.clone();
        }
        sessions.set(match why {
            Why::Asked => State::Starting,
            Why::Remembered => State::Restoring,
        });
        let ended = match bus(&mut bus_conn, &options, &mut commands).await {
            Ok(conn) => {
                let start = Start {
                    why,
                    wait: match why {
                        Why::Asked => options.dialog_wait,
                        Why::Remembered => options.restore_wait,
                    },
                    retried: why == Why::Remembered && !retries.is_empty(),
                };
                sessions.session(&conn, start, &mut commands).await
            }
            Err(ended) => ended,
        };
        // A start the app asked for answered it; one that ran a session
        // did too.
        let ran = matches!(ended, Ended::Lost(ran) if !ran.is_zero());
        if why == Why::Asked || ran || matches!(ended, Ended::Denied | Ended::Stopped) {
            commands.asked = false;
        }
        // Which tokens are kept is decided here, from how the session
        // ended, and nowhere else: one the user said no to, one of a
        // session the user stopped, and one a start may have used up are
        // forgotten. Unattended, a start that may go again does so after
        // a wait.
        let forget = || {
            let _ = options.tokens.save(S::TOKEN_KEY, None);
        };
        let mut again = false;
        match ended {
            Ended::Stop(done) => return stop(done),
            Ended::Denied => {
                forget();
                sessions.set(State::Denied);
            }
            Ended::Stopped => {
                // Not started again by itself, at the next start either.
                forget();
                sessions.set(State::Stopped);
            }
            Ended::Lost(ran) => {
                info!("the {} session went away; starting it again", S::SESSION);
                bus_conn = None;
                if has_token() {
                    // Tries start over only after a session that held.
                    if ran >= options.steady {
                        retries = options.retries.clone();
                    }
                    again = true;
                }
                sessions.set(State::Failed(format!(
                    "the desktop's {} session went away",
                    S::SESSION
                )));
            }
            Ended::Failed {
                why: reason,
                before_start,
            } => {
                warn!(error = reason, "{} through the portal failed", S::DOES);
                // Forgotten before the failure shows, as for the other ends.
                if !before_start {
                    // Perhaps used up, however the start came: not tried
                    // again unattended.
                    forget();
                } else if why == Why::Remembered {
                    again = has_token();
                }
                sessions.set(State::Failed(reason));
            }
        }
        if commands.asked {
            // The app asked meanwhile, and that start got no session going.
            commands.asked = false;
            pending = Some(Why::Asked);
        } else if again && !retries.is_empty() {
            match commands.next(Some(retries.remove(0))).await {
                Next::Asked => pending = Some(Why::Asked),
                Next::Waited => pending = Some(Why::Remembered),
                Next::Stop(done) => return stop(done),
            }
        }
    }
}

fn stop(done: Option<std::sync::mpsc::Sender<()>>) {
    if let Some(done) = done {
        let _ = done.send(());
    }
}

/// A step's outcome: what to go on with, or how the session ends; failing,
/// before or after the desktop was asked to start it.
pub(crate) fn outcome<T, E: std::fmt::Display>(
    step: Step<Result<T, E>>,
    what: &str,
    before_start: bool,
) -> Result<T, Ended> {
    match step {
        Step::Done(Ok(value)) => Ok(value),
        Step::Done(Err(e)) => Err(Ended::Failed {
            why: format!("{what}: {e}"),
            before_start,
        }),
        Step::TimedOut => Err(Ended::Failed {
            why: format!("{what}: no answer in time"),
            before_start,
        }),
        Step::Stop(done) => Err(Ended::Stop(done)),
    }
}

/// A call to the portal, for at most `wait`; why it failed, if it did.
pub(crate) async fn call<T, E: std::fmt::Display>(
    wait: Duration,
    call: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, String> {
    match tokio::time::timeout(wait, call).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("no answer in time".to_owned()),
    }
}

/// However a session ended, it is closed; nothing waits on the answer.
pub(crate) async fn close<T: ashpd::desktop::SessionPortal>(session: &ashpd::desktop::Session<T>) {
    let _ = tokio::time::timeout(STEP, session.close()).await;
}

/// The desktop closed the `session` session (`Closed`), or the bus it would
/// say so on went away, after it ran for `ran`.
pub(crate) fn closed_or_lost(closed: bool, ran: Duration, session: &str) -> Ended {
    if closed {
        info!("the desktop closed the {session} session");
        Ended::Stopped
    } else {
        Ended::Lost(ran)
    }
}

/// The connection to the session bus: the one in `kept` while the bus
/// still answers on it, or else a new one, told who this is.
async fn bus<C: Command, X: Idle<C>>(
    kept: &mut Option<zbus::Connection>,
    options: &Lifecycle,
    commands: &mut Commands<C, X>,
) -> Result<zbus::Connection, Ended> {
    if let Some(conn) = kept.clone() {
        let ping = conn.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus.Peer"),
            "Ping",
            &(),
        );
        match commands.step(BUS_PING, ping).await {
            Step::Done(Ok(_)) => return Ok(conn),
            Step::Stop(done) => return Err(Ended::Stop(done)),
            Step::Done(Err(_)) | Step::TimedOut => {
                info!("the session bus went away; connecting again")
            }
        }
    }
    let connect = crate::linux::bus::session(options.bus.as_deref());
    let conn = outcome(
        commands.step(STEP, connect).await,
        "cannot reach the session bus",
        true,
    )?;
    match commands.step(STEP, register(&conn, options.app_id)).await {
        Step::Done(()) => {}
        Step::TimedOut => debug!("the portal did not answer which app this is"),
        Step::Stop(done) => return Err(Ended::Stop(done)),
    }
    *kept = Some(conn.clone());
    Ok(conn)
}

/// Tells the portal which app this is, where it lets a host app say so.
/// Older portals take the app id from the systemd unit's name, and do not
/// know the call.
async fn register(conn: &zbus::Connection, app_id: &str) {
    let options: std::collections::HashMap<&str, zbus::Variant<'_>> = Default::default();
    let reply = conn
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.host.portal.Registry"),
            "Register",
            &(app_id, options),
        )
        .await;
    match reply {
        Ok(_) => info!(app_id, "told the portal which app this is"),
        Err(e) => debug!(error = %e, "the portal does not take an app id"),
    }
}
