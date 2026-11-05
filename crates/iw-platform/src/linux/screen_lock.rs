//! Whether the session's screen is locked, which logind and the desktops'
//! screen savers tell over D-Bus.
//!
//! The agent asks every second, from its async runtime, so the answer must
//! come at once: a thread of its own asks D-Bus and keeps the last answer.
//! It never starts a service: screen savers are asked only while their name
//! is on the bus.

/// Screen savers that keep their state over D-Bus, asked at `GetActive`,
/// their interface named as their bus name.
pub(crate) const SCREEN_SAVERS: &[(&str, &str)] = &[
    (
        "org.freedesktop.ScreenSaver",
        "/org/freedesktop/ScreenSaver",
    ),
    ("org.gnome.ScreenSaver", "/org/gnome/ScreenSaver"),
    ("org.cinnamon.ScreenSaver", "/org/cinnamon/ScreenSaver"),
    ("org.mate.ScreenSaver", "/org/mate/ScreenSaver"),
    ("org.xfce.ScreenSaver", "/org/xfce/ScreenSaver"),
];

/// The screen savers among the names on the session bus.
pub(crate) fn screen_savers_on(names: &[String]) -> Vec<(&'static str, &'static str)> {
    SCREEN_SAVERS
        .iter()
        .copied()
        .filter(|(name, _)| names.iter().any(|n| n == name))
        .collect()
}

/// Whether the screen is locked, from logind's `LockedHint` for this
/// session (`None` when logind has no session that is this one, or there
/// is no logind) and whether a screen saver says it is active.
///
/// logind's flag decides whenever there is one: GNOME, KDE and the lockers
/// that follow logind (xfce4-screensaver, light-locker, cinnamon-screensaver)
/// set it as they lock, and leave it alone when they only blank the screen.
/// Only without it does an active screen saver count as a lock; one set to
/// blank the screen without locking then reports a lock that is not there,
/// which on X11 costs nothing but the word on other devices (input still
/// reaches it, `X11Status::lock_screen_input`).
pub(crate) fn locked(hint: Option<bool>, screen_saver_active: impl FnOnce() -> bool) -> bool {
    hint.unwrap_or_else(screen_saver_active)
}

/// How logind is asked for this process's session, in order. A process
/// in the user's systemd service manager is in no session of its own, so
/// the last way is the session logind shows on the user's display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionLookup {
    /// `GetSession(id)`, from `XDG_SESSION_ID`.
    ById(String),
    /// `GetSessionByPID(pid)`.
    ByPid(u32),
    /// The `Display` property of `GetUser(uid)`.
    UserDisplay(u32),
}

pub(crate) fn session_lookups(session_id: Option<&str>, pid: u32, uid: u32) -> Vec<SessionLookup> {
    let mut out = Vec::new();
    if let Some(id) = session_id.map(str::trim).filter(|id| !id.is_empty()) {
        out.push(SessionLookup::ById(id.to_owned()));
    }
    out.push(SessionLookup::ByPid(pid));
    out.push(SessionLookup::UserDisplay(uid));
    out
}

/// How long a service that kept a question waiting is left alone.
pub(crate) const SLOW_PAUSE: std::time::Duration = std::time::Duration::from_secs(30);

/// Services that kept a question waiting, left alone for [`SLOW_PAUSE`]
/// so one that hangs costs a question every so often, not every second.
#[derive(Debug, Default)]
pub(crate) struct Slow {
    until: Vec<(&'static str, std::time::Instant)>,
}

impl Slow {
    pub(crate) fn timed_out(&mut self, name: &'static str, now: std::time::Instant) {
        self.answered(name);
        self.until.push((name, now + SLOW_PAUSE));
    }

    pub(crate) fn answered(&mut self, name: &str) {
        self.until.retain(|(n, _)| *n != name);
    }

    pub(crate) fn skips(&self, name: &str, now: std::time::Instant) -> bool {
        self.until
            .iter()
            .any(|(n, until)| *n == name && now < *until)
    }
}

#[cfg(target_os = "linux")]
pub(crate) use watch::ScreenLock;

#[cfg(target_os = "linux")]
mod watch {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use zbus::Connection;
    use zbus::{OwnedObjectPath, OwnedVariant, Variant};

    use super::{SessionLookup, Slow, locked, screen_savers_on, session_lookups};

    /// How often the thread asks.
    const TICK: Duration = Duration::from_secs(1);
    /// How long one question may take.
    const ASK: Duration = Duration::from_millis(500);
    /// How long until a bus that could not be reached is tried again, and
    /// how often the screen savers on the session bus are looked for.
    const RETRY: Duration = Duration::from_secs(30);
    const LOOK_FOR_SAVERS: Duration = Duration::from_secs(10);

    /// The screen lock as last seen, kept up to date by a thread.
    pub(crate) struct ScreenLock {
        locked: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
    }

    impl ScreenLock {
        pub(crate) fn start() -> Self {
            let locked = Arc::new(AtomicBool::new(false));
            let stop = Arc::new(AtomicBool::new(false));
            let (thread_locked, thread_stop) = (locked.clone(), stop.clone());
            let started = std::thread::Builder::new()
                .name("inputweave-screen-lock".to_owned())
                .spawn(move || run(&thread_locked, &thread_stop));
            if let Err(e) = started {
                tracing::warn!(error = %e, "cannot watch the screen lock");
            }
            Self { locked, stop }
        }

        pub(crate) fn locked(&self) -> bool {
            self.locked.load(Ordering::Relaxed)
        }
    }

    impl Drop for ScreenLock {
        fn drop(&mut self) {
            // The thread sees it within a tick and ends.
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    fn run(locked: &AtomicBool, stop: &AtomicBool) {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(e) => {
                tracing::warn!(error = %e, "cannot watch the screen lock");
                return;
            }
        };
        runtime.block_on(async {
            let mut watcher = Watcher::default();
            while !stop.load(Ordering::Relaxed) {
                locked.store(watcher.locked().await, Ordering::Relaxed);
                tokio::time::sleep(TICK).await;
            }
        });
    }

    /// Whether an error says the connection itself broke, as when the bus
    /// restarted: the connection is dropped and made again. Anything else,
    /// an error the other end answered with or a question that waited too
    /// long (`ask`), leaves it be: one service hanging is no broken bus.
    pub(super) fn transport_failure(e: &zbus::Error) -> bool {
        matches!(e, zbus::Error::InputOutput(_))
    }

    #[derive(Default)]
    struct Watcher {
        system: Bus,
        session: Bus,
        /// This session's object in logind, once found.
        logind_session: Option<OwnedObjectPath>,
        /// logind had no session that is this one: looked for again then.
        no_session_until: Option<Instant>,
        /// What logind last said, kept while it is slow to answer.
        last_hint: Option<bool>,
        savers: Vec<(&'static str, &'static str)>,
        savers_looked_for: Option<Instant>,
        slow: Slow,
    }

    /// A bus connection, or when it was last tried.
    #[derive(Default)]
    struct Bus {
        connection: Option<Connection>,
        tried: Option<Instant>,
    }

    impl Bus {
        async fn get(&mut self, system: bool) -> Option<Connection> {
            if self.connection.is_none() && self.tried.is_none_or(|t| t.elapsed() >= RETRY) {
                self.tried = Some(Instant::now());
                let connect = async {
                    if system {
                        Connection::system().await
                    } else {
                        // Without the address' guid, so a session bus
                        // that restarted is reached again.
                        crate::linux::bus::session(None).await
                    }
                };
                match tokio::time::timeout(ASK * 4, connect).await {
                    Ok(Ok(c)) => self.connection = Some(c),
                    Ok(Err(e)) => {
                        tracing::debug!(system, error = %e, "no D-Bus to ask about the screen lock")
                    }
                    Err(_) => tracing::debug!(system, "D-Bus did not answer"),
                }
            }
            self.connection.clone()
        }

        /// Drops the connection after `e` if it broke; the next tick
        /// connects again.
        fn failed(&mut self, e: &zbus::Error) {
            if transport_failure(e) && self.connection.take().is_some() {
                tracing::debug!(error = %e, "lost D-Bus; connecting again");
                self.tried = None;
            }
        }
    }

    impl Watcher {
        async fn locked(&mut self) -> bool {
            let hint = self.locked_hint().await;
            if hint.is_some() {
                return locked(hint, || false);
            }
            let active = self.screen_saver_active().await;
            locked(None, || active)
        }

        /// logind's `LockedHint` for this session; `None` without logind
        /// or a session of it that is this one.
        async fn locked_hint(&mut self) -> Option<bool> {
            let conn = self.system.get(true).await?;
            if self.logind_session.is_none() {
                // Looked for every so often, not every second.
                if self.no_session_until.is_some_and(|t| Instant::now() < t) {
                    return None;
                }
                match find_session(&conn).await {
                    Ok(Some(path)) => self.logind_session = Some(path),
                    Ok(None) => {
                        self.no_session_until = Some(Instant::now() + RETRY);
                        return None;
                    }
                    Err(e) => {
                        self.system.failed(&e);
                        return None;
                    }
                }
            }
            let path = self.logind_session.clone()?;
            let hint = property(
                &conn,
                "org.freedesktop.login1",
                path.as_str(),
                "org.freedesktop.login1.Session",
                "LockedHint",
            )
            .await
            .and_then(bool::try_from);
            match hint {
                Ok(hint) => {
                    self.last_hint = Some(hint);
                    Some(hint)
                }
                // Slow, not gone: the screen has not changed for that.
                Err(e) if timed_out(&e) => self.last_hint,
                Err(e) => {
                    tracing::debug!(error = %e, "logind did not say whether the screen is locked");
                    self.system.failed(&e);
                    // The session may have gone; look for it again.
                    self.logind_session = None;
                    self.last_hint = None;
                    None
                }
            }
        }

        async fn screen_saver_active(&mut self) -> bool {
            let Some(conn) = self.session.get(false).await else {
                return false;
            };
            if self
                .savers_looked_for
                .is_none_or(|t| t.elapsed() >= LOOK_FOR_SAVERS)
            {
                self.savers_looked_for = Some(Instant::now());
                let names = ask(
                    &conn,
                    "org.freedesktop.DBus",
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "ListNames",
                    &(),
                )
                .await
                .and_then(|m| m.body().deserialize::<Vec<String>>());
                match names {
                    Ok(names) => self.savers = screen_savers_on(&names),
                    Err(e) => {
                        self.session.failed(&e);
                        // Looked for again on the new connection.
                        self.savers_looked_for = None;
                        return false;
                    }
                }
            }
            // All at once, each with its own time to answer; one that kept
            // a question waiting is left alone for a while.
            let now = Instant::now();
            let mut asked = tokio::task::JoinSet::new();
            for (name, path) in self.savers.clone() {
                if self.slow.skips(name, now) {
                    continue;
                }
                let conn = conn.clone();
                asked.spawn(async move {
                    let reply = ask(&conn, name, path, name, "GetActive", &())
                        .await
                        .and_then(|m| m.body().deserialize::<bool>());
                    (name, reply)
                });
            }
            let mut active = false;
            while let Some(Ok((name, reply))) = asked.join_next().await {
                match reply {
                    Ok(on) => {
                        self.slow.answered(name);
                        active |= on;
                    }
                    Err(e) if timed_out(&e) => {
                        tracing::debug!(name, "a screen saver keeps us waiting; asking it later");
                        self.slow.timed_out(name, Instant::now());
                    }
                    Err(e) => self.session.failed(&e),
                }
            }
            active
        }
    }

    /// This process's session in logind, if it has one; an error only when
    /// the connection failed.
    async fn find_session(conn: &Connection) -> zbus::Result<Option<OwnedObjectPath>> {
        // SAFETY: getpid and getuid cannot fail.
        let (pid, uid) = (std::process::id(), unsafe { libc::getuid() });
        let id = std::env::var("XDG_SESSION_ID").ok();
        for lookup in session_lookups(id.as_deref(), pid, uid) {
            let found = match &lookup {
                SessionLookup::ById(id) => manager(conn, "GetSession", &(id.as_str(),)).await,
                SessionLookup::ByPid(pid) => manager(conn, "GetSessionByPID", &(*pid,)).await,
                SessionLookup::UserDisplay(uid) => {
                    async {
                        let user = manager(conn, "GetUser", &(*uid,)).await?;
                        let display = property(
                            conn,
                            "org.freedesktop.login1",
                            user.as_str(),
                            "org.freedesktop.login1.User",
                            "Display",
                        )
                        .await?;
                        let (_, path) =
                            <(String, OwnedObjectPath)>::try_from(Variant::from(display))?;
                        Ok(path)
                    }
                    .await
                }
            };
            match found {
                // The user's display may be no session at all.
                Ok(path) if path.as_str() != "/" => {
                    tracing::debug!(?lookup, session = %path.as_str(), "found the logind session");
                    return Ok(Some(path));
                }
                Ok(_) => {}
                Err(e) if transport_failure(&e) => return Err(e),
                Err(e) => tracing::debug!(?lookup, error = %e, "logind has no session that way"),
            }
        }
        Ok(None)
    }

    /// A method of logind's manager that answers with an object.
    async fn manager<B>(conn: &Connection, method: &str, body: &B) -> zbus::Result<OwnedObjectPath>
    where
        B: zbus::export::serde::Serialize + zbus::DynamicType,
    {
        ask(
            conn,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            method,
            body,
        )
        .await?
        .body()
        .deserialize::<OwnedObjectPath>()
    }

    async fn ask<B>(
        conn: &Connection,
        destination: &str,
        path: &str,
        interface: &str,
        method: &str,
        body: &B,
    ) -> zbus::Result<zbus::Message>
    where
        B: zbus::export::serde::Serialize + zbus::DynamicType,
    {
        tokio::time::timeout(
            ASK,
            conn.call_method(Some(destination), path, Some(interface), method, body),
        )
        .await
        .map_err(|_| zbus::Error::Failure(format!("{TIMED_OUT}: {destination} {method}")))?
    }

    /// What `ask` says when an answer took too long.
    const TIMED_OUT: &str = "no answer in time";

    fn timed_out(e: &zbus::Error) -> bool {
        matches!(e, zbus::Error::Failure(m) if m.starts_with(TIMED_OUT))
    }

    async fn property(
        conn: &Connection,
        destination: &str,
        path: &str,
        interface: &str,
        name: &str,
    ) -> zbus::Result<OwnedVariant> {
        ask(
            conn,
            destination,
            path,
            "org.freedesktop.DBus.Properties",
            "Get",
            &(interface, name),
        )
        .await?
        .body()
        .deserialize::<OwnedVariant>()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_broken_connection_is_made_again_but_not_for_a_refusal_or_a_wait() {
            let io = std::io::Error::from(std::io::ErrorKind::BrokenPipe);
            assert!(transport_failure(&zbus::Error::InputOutput(Arc::new(io))));
            let waited = zbus::Error::Failure(format!("{TIMED_OUT}: org.gnome.ScreenSaver"));
            assert!(!transport_failure(&waited));
            assert!(timed_out(&waited));
            let refused = zbus::fdo::Error::ServiceUnknown("org.freedesktop.login1".to_owned());
            assert!(!transport_failure(&zbus::Error::FDO(Box::new(refused))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_screen_savers_on_the_bus_are_asked() {
        let names = vec![
            ":1.42".to_owned(),
            "org.gnome.ScreenSaver".to_owned(),
            "org.freedesktop.Notifications".to_owned(),
        ];
        assert_eq!(
            screen_savers_on(&names),
            vec![("org.gnome.ScreenSaver", "/org/gnome/ScreenSaver")]
        );
        assert!(screen_savers_on(&[]).is_empty());
    }

    #[test]
    fn logind_decides_and_screen_savers_count_only_without_it() {
        // A screen saver that only blanks the screen is no lock.
        assert!(!locked(Some(false), || true));
        assert!(locked(Some(true), || false));
        // Nothing else to go by.
        assert!(locked(None, || true));
        assert!(!locked(None, || false));
        // Not even asked while logind answers.
        assert!(!locked(Some(false), || panic!("asked the screen saver")));
    }

    #[test]
    fn a_service_that_kept_us_waiting_is_left_alone_for_a_while() {
        let t0 = std::time::Instant::now();
        let mut slow = Slow::default();
        assert!(!slow.skips("org.mate.ScreenSaver", t0));
        slow.timed_out("org.mate.ScreenSaver", t0);
        assert!(slow.skips("org.mate.ScreenSaver", t0 + SLOW_PAUSE / 2));
        assert!(!slow.skips("org.gnome.ScreenSaver", t0));
        assert!(!slow.skips("org.mate.ScreenSaver", t0 + SLOW_PAUSE));
        slow.timed_out("org.mate.ScreenSaver", t0);
        slow.answered("org.mate.ScreenSaver");
        assert!(!slow.skips("org.mate.ScreenSaver", t0));
    }

    #[test]
    fn the_session_is_looked_up_by_id_then_process_then_user() {
        assert_eq!(
            session_lookups(Some("c2"), 10, 1000),
            vec![
                SessionLookup::ById("c2".to_owned()),
                SessionLookup::ByPid(10),
                SessionLookup::UserDisplay(1000),
            ]
        );
        // A service of the user's systemd has no session id.
        assert_eq!(
            session_lookups(Some(" "), 10, 1000),
            vec![SessionLookup::ByPid(10), SessionLookup::UserDisplay(1000)]
        );
    }
}
