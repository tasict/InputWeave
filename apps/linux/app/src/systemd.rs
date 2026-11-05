//! The agent's systemd user unit, through the user manager's D-Bus API
//! (`org.freedesktop.systemd1` on the session bus): its state, and
//! starting, restarting and enabling it. All of it blocks (in
//! `zbus::block_on`), so it runs on threads of its own.

use std::time::Duration;

use inputweave_app_core::log;
use inputweave_app_core::service::{self, AGENT_UNIT, ServiceAction, ServiceState};
use zbus::{Connection, OwnedObjectPath, Proxy};

const DESTINATION: &str = "org.freedesktop.systemd1";
const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";
const UNIT: &str = "org.freedesktop.systemd1.Unit";

/// How often the unit's state is read; less often while the user manager
/// cannot be asked, as in a session systemd does not run.
const POLL: Duration = Duration::from_secs(2);
const POLL_FAILING: Duration = Duration::from_secs(30);

/// A proxy that asks for properties each time rather than cache them (a
/// caching proxy subscribes to their changes, which nothing here needs).
async fn proxy(
    connection: &Connection,
    path: impl TryInto<zbus::ObjectPath<'static>, Error: Into<zbus::Error>>,
    interface: &'static str,
) -> zbus::Result<Proxy<'static>> {
    zbus::proxy::Builder::new(connection)
        .destination(DESTINATION)
        .path(path)
        .interface(interface)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
}

async fn manager(connection: &Connection) -> zbus::Result<Proxy<'static>> {
    proxy(connection, MANAGER_PATH, MANAGER).await
}

/// What reads the unit's state: one connection and its proxies, kept.
struct Watcher {
    manager: Proxy<'static>,
    unit: Option<Proxy<'static>>,
}

impl Watcher {
    async fn open() -> zbus::Result<Watcher> {
        Ok(Watcher {
            manager: manager(&Connection::session().await?).await?,
            unit: None,
        })
    }

    async fn state(&mut self) -> zbus::Result<Reading> {
        if self.unit.is_none() {
            // LoadUnit loads it if it is not, and fails for no unit file.
            let path: OwnedObjectPath = match self.manager.call("LoadUnit", &(AGENT_UNIT,)).await {
                Ok(path) => path,
                Err(zbus::Error::MethodError(name, _, _))
                    if name.as_str() == "org.freedesktop.systemd1.NoSuchUnit" =>
                {
                    return Ok(Reading {
                        state: ServiceState::NotInstalled,
                        invocation: Vec::new(),
                    });
                }
                Err(e) => return Err(e),
            };
            self.unit = Some(proxy(self.manager.connection(), path, UNIT).await?);
        }
        let unit = self.unit.as_ref().expect("made above");
        let load: String = unit.get_property("LoadState").await?;
        let active: String = unit.get_property("ActiveState").await?;
        // New each time the unit starts: tells a restart from none.
        let invocation: Vec<u8> = unit.get_property("InvocationID").await.unwrap_or_default();
        let state = ServiceState::from_systemd(&load, &active);
        if state == ServiceState::NotInstalled {
            // Installed later, it is loaded under the same path again.
            self.unit = None;
        }
        Ok(Reading { state, invocation })
    }
}

/// The unit as last read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    pub state: ServiceState,
    /// systemd's `InvocationID`, new each time the unit starts; empty while
    /// it does not run, or when unknown.
    pub invocation: Vec<u8>,
}

/// Reads the unit's state every two seconds and sends it while it changes.
pub fn watch(changes: async_channel::Sender<Reading>) {
    std::thread::Builder::new()
        .name("systemd-watch".into())
        .spawn(move || {
            let mut last = None;
            let mut failing = false;
            // One connection for as long as it works; another after a
            // failure, as when the session bus restarted.
            let mut watcher: Option<Watcher> = None;
            loop {
                let read = zbus::block_on(async {
                    match watcher.as_mut() {
                        Some(w) => w.state().await,
                        None => match Watcher::open().await {
                            Ok(mut w) => {
                                let read = w.state().await;
                                watcher = Some(w);
                                read
                            }
                            Err(e) => Err(e),
                        },
                    }
                });
                let now = match read {
                    Ok(reading) => {
                        failing = false;
                        reading
                    }
                    Err(e) => {
                        if !failing {
                            log::info(&format!("cannot read the background service's state: {e}"));
                            failing = true;
                        }
                        watcher = None;
                        Reading::default()
                    }
                };
                if last.as_ref() != Some(&now) {
                    last = Some(now.clone());
                    if changes.send_blocking(now).is_err() {
                        return;
                    }
                }
                std::thread::sleep(if failing { POLL_FAILING } else { POLL });
            }
        })
        .expect("starting the systemd thread");
}

/// Gives the user manager this session's display variables for the
/// agent's unit to start with, as the package's `start-agent` does at
/// login: the manager outlives a session and keeps what an earlier one gave
/// it. Only from the local graphical session, and taking away only what the
/// manager has from this same session (`service::session_import`); from
/// anywhere else (an `ssh -X` shell) the unit starts with what the manager
/// has, and so does an app systemd runs (`service::under_systemd`: D-Bus
/// activation), whose variables are the manager's: it relies on the
/// session having given them at login (graphical-session.target, or the
/// package's `start-agent`). A failure is logged and the unit started
/// anyway.
async fn import_session(manager: &Proxy<'static>) {
    // Run by systemd (D-Bus activation): the variables are the manager's
    // own; the session gave them at login.
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    if service::under_systemd(&cgroup) {
        log::info(&format!(
            "run by systemd: {AGENT_UNIT} starts with the user manager's variables"
        ));
        return;
    }
    let theirs: Vec<String> = manager
        .get_property("Environment")
        .await
        .unwrap_or_default();
    let Some(environment) = service::session_import(
        |name| std::env::var(name).ok(),
        |name| service::variable(&theirs, name).map(str::to_owned),
    ) else {
        log::info(&format!(
            "not in the local graphical session: {AGENT_UNIT} starts with the user manager's variables"
        ));
        return;
    };
    if !environment.unset.is_empty()
        && let Err(e) = manager
            .call::<_, _, ()>("UnsetEnvironment", &(&environment.unset,))
            .await
    {
        log::info(&format!(
            "cannot clear the session's variables for {AGENT_UNIT}: {e}"
        ));
    }
    if !environment.set.is_empty()
        && let Err(e) = manager
            .call::<_, _, ()>("SetEnvironment", &(&environment.set,))
            .await
    {
        log::info(&format!(
            "cannot pass the session's variables to {AGENT_UNIT}: {e}"
        ));
    }
}

/// Starts or restarts the unit. It does not enable it: the package
/// enables it for every user, and a
/// user who turned it off for themselves keeps it off; Start runs it for
/// this session only.
///
/// Returns, for a restart, the invocation that ran before it, so the
/// restart can be told done (`ServiceWatch::restarting_from`); empty when
/// unknown.
pub fn control(what: ServiceAction) -> zbus::Result<Vec<u8>> {
    zbus::block_on(async {
        let connection = Connection::session().await?;
        let manager = manager(&connection).await?;
        let before = match what {
            ServiceAction::Start => Vec::new(),
            ServiceAction::Restart => async {
                let path: OwnedObjectPath = manager.call("LoadUnit", &(AGENT_UNIT,)).await?;
                let unit = proxy(&connection, path, UNIT).await?;
                unit.get_property::<Vec<u8>>("InvocationID").await
            }
            .await
            .unwrap_or_default(),
        };
        import_session(&manager).await;
        let method = match what {
            ServiceAction::Start => "StartUnit",
            ServiceAction::Restart => "RestartUnit",
        };
        let _job: OwnedObjectPath = manager.call(method, &(AGENT_UNIT, "replace")).await?;
        log::info(&format!("asked systemd to {method} {AGENT_UNIT}"));
        zbus::Result::Ok(before)
    })
}
