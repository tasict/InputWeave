//! The background service as systemd reports it: the agent runs as the
//! user's systemd service, and the app starts, restarts and enables it
//! through the user manager.

use std::fmt::Write as _;
use std::io::{self, Read};
use std::path::Path;
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::snapshot::Link;
use crate::text::{UiText, keys};
use crate::ui;

/// The agent's systemd user unit, as the package installs it: an instance
/// of `app-cc.ctsite.inputweave@.service`, so xdg-desktop-portal tells the
/// app id from the unit's name.
pub const AGENT_UNIT: &str = "app-cc.ctsite.inputweave@agent.service";

/// The app's own unit, which D-Bus activation runs it in.
pub const APP_UNIT: &str = "app-cc.ctsite.inputweave.service";

/// How to read a unit's log: the journal keeps it (the agent writes no
/// log file on Linux).
pub fn journal_command(unit: &str) -> String {
    format!("journalctl --user -u {unit}")
}

/// How to read the app's log however it was started: as its own unit (D-Bus
/// activation), or as one `systemd-xdg-autostart-generator` made of its
/// autostart entry, its standard error goes to the journal under the
/// executable's name (systemd's default `SyslogIdentifier`), while its
/// units differ. From a terminal it writes there.
pub fn app_journal_command() -> String {
    "journalctl --user -t inputweave-app".to_owned()
}

/// The session's variables the agent needs to reach this session's
/// display, as the package's `start-agent` imports them: systemd's user
/// manager outlives a session and keeps what an earlier login gave it.
pub const SESSION_VARIABLES: [&str; 6] = [
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "XDG_SESSION_TYPE",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_ID",
];

/// What to tell the user manager before starting or restarting the agent:
/// the session's variables this app has (`SetEnvironment`, as `NAME=value`)
/// and those to take away (`UnsetEnvironment`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    pub set: Vec<String>,
    pub unset: Vec<String>,
}

/// What to give the user manager of this app's session before starting or
/// restarting the agent, from the app's variables (`app`) and the
/// manager's (`manager`); `None` to start it with what the manager has.
/// The manager serves every session of the user and outlives them, so:
/// - only from the local graphical session: `XDG_SESSION_TYPE` x11 with a
///   local `DISPLAY` (":0"), or wayland with `WAYLAND_DISPLAY`, and no
///   `SSH_CONNECTION`, `SSH_CLIENT` or `SSH_TTY` (an `ssh -X` shell, or a
///   launch with part of the session's variables, would pass on what the
///   session does not have);
/// - a variable the app lacks is taken away only when the manager has it
///   from this same session: its `XDG_SESSION_ID` is the app's, or it has
///   none to say it is another's.
pub fn session_import(
    app: impl Fn(&str) -> Option<String>,
    manager: impl Fn(&str) -> Option<String>,
) -> Option<Environment> {
    let app = |name: &str| app(name).filter(|v| !v.is_empty());
    let manager = |name: &str| manager(name).filter(|v| !v.is_empty());
    let remote = ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .iter()
        .any(|v| app(v).is_some());
    let local = match app("XDG_SESSION_TYPE").as_deref() {
        Some("x11") => app("DISPLAY").is_some_and(|d| d.starts_with(':')),
        Some("wayland") => app("WAYLAND_DISPLAY").is_some(),
        _ => false,
    };
    if remote || !local {
        return None;
    }
    let same_session = match manager("XDG_SESSION_ID") {
        Some(theirs) => app("XDG_SESSION_ID").as_deref() == Some(theirs.as_str()),
        None => true,
    };
    let mut environment = Environment::default();
    for name in SESSION_VARIABLES {
        match app(name) {
            Some(value) => environment.set.push(format!("{name}={value}")),
            None if same_session && manager(name).is_some() => {
                environment.unset.push(name.to_owned())
            }
            None => {}
        }
    }
    Some(environment)
}

/// Whether the app runs as a systemd service (its own unit for D-Bus
/// activation, `app-cc.ctsite.inputweave.service`, or the one
/// `systemd-xdg-autostart-generator` makes of its autostart entry), from
/// `/proc/self/cgroup`: its cgroup's leaf is a `.service`, not a session's
/// or launcher's `.scope`. It has the user manager's variables then, not
/// the session's, and giving them back would only keep stale ones; it
/// relies on the session having given them at login
/// (graphical-session.target, or the package's `start-agent`).
/// `INVOCATION_ID` is no evidence: whatever a service starts inherits it.
pub fn under_systemd(cgroup: &str) -> bool {
    cgroup
        .lines()
        .filter_map(|l| l.rsplit('/').next())
        .any(|leaf| leaf.ends_with(".service"))
}

/// A variable of `NAME=value` lines (systemd's `Environment`).
pub fn variable<'a>(lines: &'a [String], name: &str) -> Option<&'a str> {
    lines
        .iter()
        .find_map(|l| l.strip_prefix(name)?.strip_prefix('='))
}

/// Whether the background service that answered is not the one packaged
/// with this app, as the macOS app's `OutdatedService`: another build (the
/// SHA-256 of its executable against the packaged agent's: the same build
/// is never outdated, whatever the versions say), or one that refuses this
/// app's IPC version with an older one of its own (`agent_ipc`, from its
/// refusal: a newer one means the app is the one behind, and restarting
/// the agent would not help; one that does not say is left alone). apt
/// replaces the files but restarts no user service, so the app restarts
/// it. Only a service too old to report a digest is judged by its
/// version; with no packaged agent to compare with (a development build),
/// one that reports a digest is left alone.
pub fn is_outdated(
    link: Link,
    agent_version: &str,
    app_version: &str,
    agent_digest: &str,
    packaged_digest: &str,
    agent_ipc: Option<u32>,
) -> bool {
    match link {
        Link::Incompatible => agent_ipc.is_some_and(|v| v < crate::agent::IPC_VERSION),
        Link::Connecting => false,
        Link::Connected if agent_digest.is_empty() => {
            !agent_version.is_empty() && agent_version != app_version
        }
        Link::Connected => {
            !packaged_digest.is_empty() && !agent_digest.eq_ignore_ascii_case(packaged_digest)
        }
    }
}

/// Restarts an outdated service once per launch, and only one the unit
/// runs: an agent started by hand is left alone. Nor again for a packaged
/// agent it was restarted for before (`restarted_for`, its digest, kept in
/// `app.toml`): a restart that changed nothing (a development app beside
/// the packaged agent) is not repeated at every launch.
#[derive(Debug, Default)]
pub struct OutdatedWatch {
    restarted: bool,
}

impl OutdatedWatch {
    /// Whether to restart the service now, for the packaged agent of
    /// `packaged` digest.
    pub fn restart_now(
        &mut self,
        service: ServiceState,
        outdated: bool,
        packaged: &str,
        restarted_for: &str,
    ) -> bool {
        if self.restarted
            || !outdated
            || service != ServiceState::Running
            || (!packaged.is_empty() && packaged == restarted_for)
        {
            return false;
        }
        self.restarted = true;
        true
    }
}

/// The SHA-256 of a file's contents, in lowercase hex, as the agent
/// reports its own (`AgentHello.agent_digest`).
pub fn file_digest(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buffer[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ServiceState {
    /// Not known yet, or asking failed.
    #[default]
    Unknown,
    Running,
    Starting,
    Stopping,
    Stopped,
    /// systemd knows no such unit: InputWeave was not installed from its
    /// package.
    NotInstalled,
}

impl ServiceState {
    /// From the unit's `LoadState` and `ActiveState` properties.
    pub fn from_systemd(load_state: &str, active_state: &str) -> ServiceState {
        if load_state == "not-found" {
            return ServiceState::NotInstalled;
        }
        match active_state {
            "active" | "reloading" | "refreshing" => ServiceState::Running,
            "activating" => ServiceState::Starting,
            "deactivating" => ServiceState::Stopping,
            "inactive" | "failed" => ServiceState::Stopped,
            _ => ServiceState::Unknown,
        }
    }
}

/// What the app offers to do about the service: start it while it is
/// stopped, restart it otherwise; nothing when it is not installed. The
/// main window's cover and Troubleshooting both show it (Troubleshooting
/// while connected too: an agent started by hand runs beside a stopped or
/// missing unit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Restart,
}

impl ServiceAction {
    pub fn for_state(service: ServiceState) -> Option<ServiceAction> {
        match service {
            ServiceState::NotInstalled => None,
            ServiceState::Stopped => Some(ServiceAction::Start),
            _ => Some(ServiceAction::Restart),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            ServiceAction::Start => keys::ACTION_START_SERVICE,
            ServiceAction::Restart => keys::ACTION_RESTART_SERVICE,
        }
    }
}

/// "Running", "Stopped"… for Troubleshooting.
pub fn status_text(service: ServiceState, link: Link) -> UiText {
    match service {
        ServiceState::NotInstalled => ui!(keys::SERVICE_NOT_INSTALLED),
        ServiceState::Stopped => ui!(keys::SERVICE_STOPPED),
        ServiceState::Stopping => ui!(keys::SERVICE_STOPPING),
        _ => match link {
            Link::Connected => ui!(keys::SERVICE_RUNNING),
            Link::Incompatible => ui!(keys::SERVICE_UPDATING),
            Link::Connecting => ui!(keys::SERVICE_STARTING),
        },
    }
}

/// What to do about the "service stopped" notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceNews {
    None,
    Raise,
    Withdraw,
}

/// When to tell the user the service stopped, as the Windows app's
/// `ServiceWatch`: only once it ran, after it stayed stopped for
/// `STOPPED_FOR`, not while a restart the app asked for goes on, and not
/// while `quiet` (first-run setup); taken back once it runs again.
///
/// A restart may come and go between two looks at the unit, so it counts
/// as done once the unit runs as another invocation than when it was asked
/// for (systemd's `InvocationID`, new each time the unit starts), not only
/// after it was seen stopping.
#[derive(Debug, Default)]
pub struct ServiceWatch {
    saw_running: bool,
    stopped_since: Option<SystemTime>,
    handled: bool,
    notified: bool,
    restart_asked: Option<SystemTime>,
    restart_stopped: bool,
    /// The invocation running when the restart was asked for.
    restart_from: Vec<u8>,
}

impl ServiceWatch {
    pub const STOPPED_FOR: Duration = Duration::from_secs(10);
    pub const RESTART_ALLOWANCE: Duration = Duration::from_secs(120);

    /// The app is restarting the service, which runs as `invocation` now:
    /// its stopping is no news.
    pub fn restarting(&mut self, now: SystemTime, invocation: &[u8]) {
        self.restart_asked = Some(now);
        self.restart_stopped = false;
        self.restart_from = invocation.to_vec();
    }

    /// The invocation read just before the restart was sent, for when none
    /// was known as it was asked for (the unit had not been read yet):
    /// without it a restart quick enough to fall between two looks would
    /// go unseen, and a stop after it unannounced until the allowance ran
    /// out.
    pub fn restarting_from(&mut self, invocation: &[u8]) {
        if self.restart_asked.is_some() && self.restart_from.is_empty() {
            self.restart_from = invocation.to_vec();
        }
    }

    /// `invocation` is the unit's `InvocationID`, empty when unknown.
    pub fn update(
        &mut self,
        state: ServiceState,
        invocation: &[u8],
        now: SystemTime,
        quiet: bool,
    ) -> ServiceNews {
        if let Some(asked) = self.restart_asked
            && now.duration_since(asked).unwrap_or_default() > Self::RESTART_ALLOWANCE
        {
            self.restart_asked = None;
        }
        match state {
            ServiceState::Running => {
                self.saw_running = true;
                self.stopped_since = None;
                self.handled = false;
                // Both known: an unknown one tells nothing.
                let another = !invocation.is_empty()
                    && !self.restart_from.is_empty()
                    && invocation != self.restart_from;
                if self.restart_stopped || another {
                    // The restart the app asked for is done.
                    self.restart_asked = None;
                    self.restart_stopped = false;
                }
                if self.notified {
                    self.notified = false;
                    return ServiceNews::Withdraw;
                }
                ServiceNews::None
            }
            ServiceState::Stopped => {
                let since = *self.stopped_since.get_or_insert(now);
                if self.restart_asked.is_some() {
                    self.restart_stopped = true;
                    return ServiceNews::None;
                }
                if self.handled
                    || !self.saw_running
                    || now.duration_since(since).unwrap_or_default() < Self::STOPPED_FOR
                {
                    return ServiceNews::None;
                }
                self.handled = true;
                if quiet {
                    return ServiceNews::None;
                }
                self.notified = true;
                ServiceNews::Raise
            }
            ServiceState::Stopping | ServiceState::Starting => {
                if self.restart_asked.is_some() {
                    self.restart_stopped = true;
                }
                ServiceNews::None
            }
            ServiceState::Unknown | ServiceState::NotInstalled => ServiceNews::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1000 + seconds)
    }

    #[test]
    fn systemd_states_map_to_service_states() {
        assert_eq!(
            ServiceState::from_systemd("loaded", "active"),
            ServiceState::Running
        );
        assert_eq!(
            ServiceState::from_systemd("loaded", "activating"),
            ServiceState::Starting
        );
        assert_eq!(
            ServiceState::from_systemd("loaded", "deactivating"),
            ServiceState::Stopping
        );
        assert_eq!(
            ServiceState::from_systemd("loaded", "failed"),
            ServiceState::Stopped
        );
        assert_eq!(
            ServiceState::from_systemd("loaded", "inactive"),
            ServiceState::Stopped
        );
        assert_eq!(
            ServiceState::from_systemd("not-found", "inactive"),
            ServiceState::NotInstalled
        );
        assert_eq!(
            ServiceState::from_systemd("loaded", "maintenance"),
            ServiceState::Unknown
        );
    }

    #[test]
    fn a_stop_is_news_after_ten_seconds_once_it_ran() {
        let mut watch = ServiceWatch::default();
        assert_eq!(
            watch.update(ServiceState::Stopped, b"a", at(0), false),
            ServiceNews::None
        );
        assert_eq!(
            watch.update(ServiceState::Stopped, b"a", at(30), false),
            ServiceNews::None,
            "never ran"
        );
        assert_eq!(
            watch.update(ServiceState::Running, b"a", at(31), false),
            ServiceNews::None
        );
        assert_eq!(
            watch.update(ServiceState::Stopped, b"a", at(40), false),
            ServiceNews::None
        );
        assert_eq!(
            watch.update(ServiceState::Stopped, b"a", at(51), false),
            ServiceNews::Raise
        );
        assert_eq!(
            watch.update(ServiceState::Stopped, b"a", at(60), false),
            ServiceNews::None,
            "once"
        );
        assert_eq!(
            watch.update(ServiceState::Running, b"a", at(61), false),
            ServiceNews::Withdraw
        );
    }

    #[test]
    fn restarts_and_setup_say_nothing() {
        let mut watch = ServiceWatch::default();
        watch.update(ServiceState::Running, b"a", at(0), false);
        watch.restarting(at(1), b"a");
        assert_eq!(
            watch.update(ServiceState::Stopped, b"a", at(2), false),
            ServiceNews::None
        );
        assert_eq!(
            watch.update(ServiceState::Stopped, b"a", at(30), false),
            ServiceNews::None
        );
        assert_eq!(
            watch.update(ServiceState::Running, b"a", at(31), false),
            ServiceNews::None
        );
        assert_eq!(
            watch.update(ServiceState::Stopped, b"a", at(40), true),
            ServiceNews::None
        );
        assert_eq!(
            watch.update(ServiceState::Stopped, b"a", at(60), true),
            ServiceNews::None,
            "quiet"
        );
    }

    #[test]
    fn a_restart_between_two_looks_is_seen_by_its_new_invocation() {
        let mut watch = ServiceWatch::default();
        watch.update(ServiceState::Running, b"a", at(0), false);
        watch.restarting(at(1), b"a");
        // Never seen stopping: running as another invocation at the next
        // look, the restart is done.
        assert_eq!(
            watch.update(ServiceState::Running, b"b", at(3), false),
            ServiceNews::None
        );
        // So a stop after it is news again, well within the allowance.
        assert_eq!(
            watch.update(ServiceState::Stopped, b"", at(10), false),
            ServiceNews::None
        );
        assert_eq!(
            watch.update(ServiceState::Stopped, b"", at(21), false),
            ServiceNews::Raise
        );
    }

    #[test]
    fn a_restart_still_running_as_before_is_not_done() {
        let mut watch = ServiceWatch::default();
        watch.update(ServiceState::Running, b"a", at(0), false);
        watch.restarting(at(1), b"a");
        // The old invocation, not restarted yet.
        watch.update(ServiceState::Running, b"a", at(2), false);
        assert_eq!(
            watch.update(ServiceState::Stopped, b"", at(4), false),
            ServiceNews::None
        );
        assert_eq!(
            watch.update(ServiceState::Stopped, b"", at(20), false),
            ServiceNews::None
        );
    }

    #[test]
    fn a_restart_asked_for_before_the_unit_was_read_learns_where_it_began() {
        let mut watch = ServiceWatch::default();
        watch.update(ServiceState::Running, b"", at(0), false);
        watch.restarting(at(1), b"");
        // Read just before the restart was sent.
        watch.restarting_from(b"a");
        assert_eq!(
            watch.update(ServiceState::Running, b"a", at(2), false),
            ServiceNews::None
        );
        // Still the old one: a stop now is the restart's.
        assert_eq!(
            watch.update(ServiceState::Stopped, b"", at(4), false),
            ServiceNews::None
        );
        assert_eq!(
            watch.update(ServiceState::Stopped, b"", at(20), false),
            ServiceNews::None
        );
        // Another restart, quick, seen only as a new invocation.
        watch.update(ServiceState::Running, b"b", at(21), false);
        watch.restarting(at(22), b"");
        watch.restarting_from(b"b");
        watch.update(ServiceState::Running, b"c", at(24), false);
        assert_eq!(
            watch.update(ServiceState::Stopped, b"", at(30), false),
            ServiceNews::None
        );
        assert_eq!(
            watch.update(ServiceState::Stopped, b"", at(41), false),
            ServiceNews::Raise,
            "the restart was done, so this stop is news"
        );
    }

    fn vars(list: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let list: Vec<(String, String)> = list
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| list.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    #[test]
    fn the_local_session_is_imported_and_its_own_leftovers_removed() {
        let wayland = [
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"),
            ("XDG_SESSION_ID", "3"),
            ("DISPLAY", ""),
        ];
        // The manager has a DISPLAY and an XAUTHORITY of this same session.
        let manager = vars(&[
            ("DISPLAY", ":0"),
            ("XAUTHORITY", "/run/user/1000/.mutter-Xwaylandauth"),
            ("XDG_SESSION_ID", "3"),
        ]);
        let env = session_import(vars(&wayland), &manager).expect("local");
        assert_eq!(
            env.set,
            [
                "WAYLAND_DISPLAY=wayland-0",
                "XDG_SESSION_TYPE=wayland",
                "XDG_CURRENT_DESKTOP=ubuntu:GNOME",
                "XDG_SESSION_ID=3"
            ]
        );
        assert_eq!(env.unset, ["DISPLAY", "XAUTHORITY"]);
        // Nothing to take away that the manager does not have.
        let env = session_import(vars(&wayland), vars(&[])).unwrap();
        assert!(env.unset.is_empty());
        // The manager's are another session's: kept.
        let other = vars(&[("DISPLAY", ":1"), ("XDG_SESSION_ID", "7")]);
        let env = session_import(vars(&wayland), other).unwrap();
        assert!(env.unset.is_empty());
        assert!(env.set.contains(&"XDG_SESSION_ID=3".to_owned()));
        // X11 with a local display.
        let x11 = vars(&[
            ("DISPLAY", ":0"),
            ("XDG_SESSION_TYPE", "x11"),
            ("XAUTHORITY", "/home/me/.Xauthority"),
        ]);
        let manager = vars(&[("WAYLAND_DISPLAY", "wayland-0")]);
        let env = session_import(x11, manager).unwrap();
        assert_eq!(env.unset, ["WAYLAND_DISPLAY"]);
    }

    #[test]
    fn nothing_is_imported_from_outside_the_local_graphical_session() {
        let manager = vars(&[
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":0"),
            ("XDG_SESSION_ID", "3"),
        ]);
        // ssh -X: a forwarded display, no session type of a desktop.
        let ssh_x = vars(&[
            ("DISPLAY", "localhost:10.0"),
            ("SSH_CONNECTION", "10.0.0.2 51000 10.0.0.5 22"),
            ("XDG_SESSION_TYPE", "tty"),
            ("XDG_SESSION_ID", "9"),
        ]);
        assert_eq!(session_import(ssh_x, &manager), None);
        // Even with the variables of a desktop, over ssh.
        let ssh_local = vars(&[
            ("DISPLAY", ":0"),
            ("XDG_SESSION_TYPE", "x11"),
            ("SSH_TTY", "/dev/pts/3"),
        ]);
        assert_eq!(session_import(ssh_local, &manager), None);
        // A partial launch: Wayland without its display, X11 with a remote one.
        let partial = vars(&[("XDG_SESSION_TYPE", "wayland"), ("DISPLAY", ":0")]);
        assert_eq!(session_import(partial, &manager), None);
        let remote = vars(&[("XDG_SESSION_TYPE", "x11"), ("DISPLAY", "host:0")]);
        assert_eq!(session_import(remote, &manager), None);
        assert_eq!(session_import(vars(&[]), &manager), None);
    }

    #[test]
    fn an_app_run_by_systemd_gives_the_manager_nothing_back() {
        let unit = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-cc.ctsite.inputweave.service";
        assert!(under_systemd(unit));
        let autostart = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-cc.ctsite.inputweave@autostart.service";
        assert!(under_systemd(autostart));
        // Started from the session: the desktop's scope, a terminal's tab.
        let scope = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-cc.ctsite.inputweave-4242.scope";
        assert!(!under_systemd(scope));
        let tab = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-org.gnome.Terminal.slice/vte-spawn-1b2c.scope";
        assert!(!under_systemd(tab));
        assert!(!under_systemd(
            "0::/user.slice/user-1000.slice/session-3.scope"
        ));
        // The user manager's own service on the way does not count.
        assert!(!under_systemd(
            "0::/user.slice/user-1000.slice/user@1000.service/init.scope"
        ));
        assert!(!under_systemd(""));
    }

    #[test]
    fn the_managers_variables_are_read_from_its_lines() {
        let lines = vec![
            "PATH=/usr/bin".to_owned(),
            "DISPLAY=:0".to_owned(),
            "DISPLAY_X=1".to_owned(),
        ];
        assert_eq!(variable(&lines, "DISPLAY"), Some(":0"));
        assert_eq!(variable(&lines, "WAYLAND_DISPLAY"), None);
    }

    #[test]
    fn an_outdated_service_is_restarted_once_when_the_unit_runs_it() {
        let digest = "ab".repeat(32);
        let other = "cd".repeat(32);
        // Refused: restarted only for an agent that speaks an older IPC.
        let ours = crate::agent::IPC_VERSION;
        assert!(is_outdated(
            Link::Incompatible,
            "",
            "0.1.0",
            "",
            "",
            Some(ours - 1)
        ));
        assert!(
            !is_outdated(Link::Incompatible, "", "0.1.0", "", "", Some(ours + 1)),
            "the app is behind"
        );
        assert!(
            !is_outdated(Link::Incompatible, "", "0.1.0", "", "", None),
            "it does not say"
        );
        assert!(!is_outdated(
            Link::Connecting,
            "0.0.9",
            "0.1.0",
            "",
            "",
            None
        ));
        assert!(is_outdated(Link::Connected, "0.0.9", "0.1.0", "", "", None));
        // The same build is not outdated, whatever the versions say (an app
        // whose version was not bumped with the agent's).
        assert!(!is_outdated(
            Link::Connected,
            "0.2.0",
            "0.1.0",
            &digest,
            &digest,
            None
        ));
        // The version only counts without a digest from the agent.
        assert!(is_outdated(
            Link::Connected,
            "0.2.0",
            "0.1.0",
            "",
            &digest,
            None
        ));
        assert!(!is_outdated(
            Link::Connected,
            "0.2.0",
            "0.1.0",
            &digest,
            "",
            None
        ));
        assert!(!is_outdated(
            Link::Connected,
            "0.1.0",
            "0.1.0",
            &digest,
            &digest,
            None
        ));
        assert!(is_outdated(
            Link::Connected,
            "0.1.0",
            "0.1.0",
            &digest,
            &other,
            None
        ));
        assert!(!is_outdated(
            Link::Connected,
            "0.1.0",
            "0.1.0",
            &digest.to_uppercase(),
            &digest,
            None
        ));
        // Nothing to compare with: by the version alone.
        assert!(!is_outdated(
            Link::Connected,
            "0.1.0",
            "0.1.0",
            "",
            &digest,
            None
        ));
        assert!(!is_outdated(
            Link::Connected,
            "0.1.0",
            "0.1.0",
            &digest,
            "",
            None
        ));
        let mut watch = OutdatedWatch::default();
        assert!(!watch.restart_now(ServiceState::Running, false, "", ""));
        assert!(
            !watch.restart_now(ServiceState::Stopped, true, "", ""),
            "not the unit's"
        );
        assert!(watch.restart_now(ServiceState::Running, true, "", ""));
        assert!(
            !watch.restart_now(ServiceState::Running, true, "", ""),
            "once"
        );
        // Not again, in a later launch, for the packaged agent it was for.
        let packaged = "ab".repeat(32);
        let mut later = OutdatedWatch::default();
        assert!(!later.restart_now(ServiceState::Running, true, &packaged, &packaged));
        assert!(later.restart_now(ServiceState::Running, true, &packaged, &"cd".repeat(32)));
    }

    #[test]
    fn digests_are_sha256_in_lowercase_hex() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            file_digest(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(file_digest(&dir.path().join("none")).is_err());
    }

    #[test]
    fn the_units_and_their_logs() {
        assert_eq!(AGENT_UNIT, "app-cc.ctsite.inputweave@agent.service");
        assert_eq!(
            journal_command(AGENT_UNIT),
            "journalctl --user -u app-cc.ctsite.inputweave@agent.service"
        );
        assert_eq!(app_journal_command(), "journalctl --user -t inputweave-app");
    }

    #[test]
    fn actions_by_state() {
        assert_eq!(
            ServiceAction::for_state(ServiceState::Stopped),
            Some(ServiceAction::Start)
        );
        assert_eq!(
            ServiceAction::for_state(ServiceState::Running),
            Some(ServiceAction::Restart)
        );
        assert_eq!(
            ServiceAction::for_state(ServiceState::Unknown),
            Some(ServiceAction::Restart)
        );
        assert_eq!(ServiceAction::for_state(ServiceState::NotInstalled), None);
    }

    #[test]
    fn troubleshooting_names_the_state() {
        assert_eq!(
            status_text(ServiceState::Running, Link::Connected).to_string(),
            "Running"
        );
        assert_eq!(
            status_text(ServiceState::Running, Link::Connecting).to_string(),
            "Starting"
        );
        assert_eq!(
            status_text(ServiceState::Stopped, Link::Connecting).to_string(),
            "Stopped"
        );
        assert_eq!(
            status_text(ServiceState::NotInstalled, Link::Connecting).to_string(),
            "Not installed; reinstall InputWeave"
        );
    }
}
