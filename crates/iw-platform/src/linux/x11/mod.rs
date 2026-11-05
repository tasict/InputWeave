//! X11 backends: XInput 2 raw events for capture, XTest for injection,
//! RandR for the displays, the
//! `CLIPBOARD` selection for the clipboard.
//!
//! Each backend keeps a connection of its own to the X server, so a
//! request of one never waits behind another's. Grabs, a hidden cursor and
//! passive key grabs all end with the connection, so the server lets go
//! of everything if the agent dies.
//!
//! This code compiles for Linux and its pure parts (`translate`) are unit
//! tested on every OS; the rest is tested against Xvfb in the Linux
//! container (`scripts/linux-x11-tests.sh`). Nothing has been tried on a
//! real X11 desktop yet.

pub(crate) mod translate;

#[cfg(target_os = "linux")]
mod capture;
#[cfg(target_os = "linux")]
mod clipboard;
#[cfg(all(test, target_os = "linux"))]
mod clipboard_tests;
#[cfg(target_os = "linux")]
mod emulate;
#[cfg(all(test, target_os = "linux"))]
mod tests;

#[cfg(target_os = "linux")]
pub use self::{
    capture::{HotkeyPause, X11Capture},
    clipboard::X11Clipboard,
    emulate::X11Emulation,
};

#[cfg(target_os = "linux")]
mod platform {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use iw_core::layout::Display;
    use x11rb::connection::Connection;
    use x11rb::protocol::Event;
    use x11rb::protocol::randr::{ConnectionExt as _, NotifyMask};
    use x11rb::protocol::xproto::{ConnectionExt as _, Window};
    use x11rb::rust_connection::RustConnection;

    use super::translate::{Monitor, displays_of};
    use crate::linux::common::{lock, os};
    use crate::linux::info::Info;
    use crate::linux::screen_lock::ScreenLock;
    use crate::{
        Backend, BackendRole, DesktopPictures, Displays, Native, PermissionState, PlatformError,
        SessionInfo, SessionStatus,
    };

    pub(crate) fn native(
        info: &Info,
        pictures: Box<dyn DesktopPictures>,
    ) -> Result<Native, PlatformError> {
        let capture = super::X11Capture::connect()?;
        let emulation = super::X11Emulation::connect()?.pausing(capture.hotkey_pause());
        // Without it, input is still shared.
        let (clipboard, clipboard_problem) = match super::X11Clipboard::connect() {
            Ok(clipboard) => (Some(Box::new(clipboard) as Box<dyn crate::Clipboard>), None),
            Err(e) => {
                tracing::warn!(error = %e, "cannot use the clipboard");
                (None, Some(e.to_string()))
            }
        };
        Ok(Native {
            capture: Box::new(capture),
            emulation: Box::new(emulation),
            displays: Box::new(X11Displays::connect()?),
            pictures,
            status: Box::new(X11Status::new().described(info.clone(), clipboard_problem)),
            clipboard,
        })
    }

    /// Wakes a thread waiting for `conn`'s next event, with a message to
    /// its own `window`.
    pub(crate) fn wake(conn: &RustConnection, window: Window) {
        use x11rb::protocol::xproto::{AtomEnum, ClientMessageEvent, EventMask};

        let event = ClientMessageEvent::new(32, window, AtomEnum::NONE, [0u32; 5]);
        let _ = conn.send_event(false, window, EventMask::NO_EVENT, event);
        let _ = conn.flush();
    }

    /// A connection to the X server `DISPLAY` names, and the root window
    /// of its default screen.
    pub(crate) fn connect() -> Result<(RustConnection, Window), PlatformError> {
        let (conn, screen) =
            RustConnection::connect(None).map_err(|e| os("cannot connect to the X server", e))?;
        let root = conn
            .setup()
            .roots
            .get(screen)
            .ok_or_else(|| PlatformError::Os("the X server has no such screen".to_owned()))?
            .root;
        Ok((conn, root))
    }

    /// The monitors of the screen whose root window is `root`. Only a
    /// server without RandR 1.5 makes the screen one display; a connection
    /// that broke is an error, for the caller to connect again.
    pub(crate) fn monitors(
        conn: &RustConnection,
        root: Window,
    ) -> Result<Vec<Display>, PlatformError> {
        use x11rb::errors::{ConnectionError, ReplyError};

        let failed = |e: &dyn std::fmt::Display| os("cannot list the monitors", e);
        // The root window as it is now, not as it was when the connection
        // was made: RandR before 1.5 resizes it too.
        let screen = || -> Result<(u16, u16), PlatformError> {
            let g = conn
                .get_geometry(root)
                .map_err(|e| failed(&e))?
                .reply()
                .map_err(|e| failed(&e))?;
            Ok((g.width, g.height))
        };
        let version = match conn.randr_query_version(1, 5) {
            Ok(cookie) => match cookie.reply() {
                Ok(v) => Some((v.major_version, v.minor_version)),
                // The server refused: as good as no RandR.
                Err(ReplyError::X11Error(_)) => None,
                Err(ReplyError::ConnectionError(e)) => return Err(failed(&e)),
            },
            Err(ConnectionError::UnsupportedExtension) => None,
            Err(e) => return Err(failed(&e)),
        };
        if !version.is_some_and(|v| v >= (1, 5)) {
            return Ok(displays_of(Vec::new(), screen()?));
        }
        let reply = conn
            .randr_get_monitors(root, true)
            .map_err(|e| failed(&e))?
            .reply()
            .map_err(|e| failed(&e))?;
        // The names all asked for before any answer is waited for.
        let names: Vec<_> = reply
            .monitors
            .iter()
            .map(|m| conn.get_atom_name(m.name).map_err(|e| failed(&e)))
            .collect::<Result<_, _>>()?;
        let mut found = Vec::new();
        for (m, name) in reply.monitors.into_iter().zip(names) {
            let name = match name.reply() {
                Ok(r) => String::from_utf8_lossy(&r.name).into_owned(),
                Err(ReplyError::X11Error(_)) => format!("monitor-{}", m.name),
                Err(ReplyError::ConnectionError(e)) => return Err(failed(&e)),
            };
            found.push(Monitor {
                name,
                primary: m.primary,
                x: m.x,
                y: m.y,
                width: m.width,
                height: m.height,
            });
        }
        let screen = if found.is_empty() { screen()? } else { (0, 0) };
        Ok(displays_of(found, screen))
    }

    /// The monitors RandR reports, in root window coordinates.
    /// The agent asks every second, from its async runtime: the monitors
    /// are read again only once RandR says they changed, or after
    /// [`DISPLAYS_STALE`], as RandR says nothing of monitors laid out by
    /// hand (`xrandr --setmonitor`).
    pub struct X11Displays {
        state: Mutex<DisplaysState>,
    }

    /// How long the monitors read are trusted without word from RandR.
    pub(crate) const DISPLAYS_STALE: Duration = Duration::from_secs(30);

    #[derive(Default)]
    struct DisplaysState {
        conn: Option<(RustConnection, Window)>,
        read: Option<(Vec<Display>, Instant)>,
    }

    /// A connection that hears when the monitors change.
    fn connect_listening() -> Result<(RustConnection, Window), PlatformError> {
        let (conn, root) = connect()?;
        // Without RandR there is nothing to hear: the screen stays as it is.
        let _ = conn.randr_select_input(
            root,
            NotifyMask::SCREEN_CHANGE | NotifyMask::CRTC_CHANGE | NotifyMask::OUTPUT_CHANGE,
        );
        let _ = conn.flush();
        Ok((conn, root))
    }

    impl X11Displays {
        pub fn connect() -> Result<Self, PlatformError> {
            Ok(Self {
                state: Mutex::new(DisplaysState {
                    conn: Some(connect_listening()?),
                    read: None,
                }),
            })
        }
    }

    impl Displays for X11Displays {
        fn displays(&self) -> Result<Vec<Display>, PlatformError> {
            let mut state = lock(&self.state);
            let state = &mut *state;
            if state.conn.is_none() {
                state.conn = Some(connect_listening()?);
                state.read = None;
            }
            let (conn, root) = state.conn.as_ref().expect("connected above");
            // What RandR said meanwhile, without waiting.
            loop {
                match conn.poll_for_event() {
                    Ok(Some(Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_))) => {
                        state.read = None;
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(e) => {
                        state.conn = None;
                        state.read = None;
                        return Err(os("cannot list the monitors", e));
                    }
                }
            }
            if let Some((displays, at)) = &state.read
                && at.elapsed() < DISPLAYS_STALE
            {
                return Ok(displays.clone());
            }
            match monitors(conn, *root) {
                Ok(displays) => {
                    state.read = Some((displays.clone(), Instant::now()));
                    Ok(displays)
                }
                Err(e) => {
                    // Connect again next time: the server may have restarted.
                    state.conn = None;
                    state.read = None;
                    Err(e)
                }
            }
        }
    }

    /// X11 asks for no permission: any client of the display may read and
    /// inject input.
    pub struct X11Status {
        lock: ScreenLock,
        /// The session, for the app (`session_info`); `None` for one made
        /// alone, as in tests.
        info: Option<Info>,
        /// Why there is no clipboard, if there is none.
        clipboard_problem: Option<String>,
    }

    impl X11Status {
        pub fn new() -> Self {
            Self {
                lock: ScreenLock::start(),
                info: None,
                clipboard_problem: None,
            }
        }

        /// Telling the app about `info`'s session, whose clipboard could
        /// not be reached for `clipboard_problem`, if any.
        pub(crate) fn described(self, info: Info, clipboard_problem: Option<String>) -> Self {
            Self {
                info: Some(info),
                clipboard_problem,
                ..self
            }
        }
    }

    impl Default for X11Status {
        fn default() -> Self {
            Self::new()
        }
    }

    impl SessionStatus for X11Status {
        fn permissions(&self) -> Vec<PermissionState> {
            Vec::new()
        }

        fn screen_locked(&self) -> bool {
            self.lock.locked()
        }

        /// XTest's input reaches the screen lockers of X11, which are X
        /// clients grabbing the keyboard and pointer like any other.
        fn lock_screen_input(&self) -> bool {
            true
        }

        fn session_info(&self) -> Option<SessionInfo> {
            let clipboard = match &self.clipboard_problem {
                None => Backend::serving(BackendRole::Clipboard, "X11 CLIPBOARD selection"),
                Some(problem) => Backend::missing(BackendRole::Clipboard, problem),
            };
            Some(self.info.as_ref()?.with(vec![
                Backend::serving(BackendRole::Input, "XTest"),
                Backend::serving(BackendRole::Capture, "XInput 2"),
                clipboard,
                Backend::serving(BackendRole::Displays, "RandR"),
            ]))
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) use self::platform::native;
#[cfg(target_os = "linux")]
pub use self::platform::{X11Displays, X11Status};
