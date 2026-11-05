//! Which kind of graphical session the agent runs in, which decides the
//! backends.

/// The display server the session runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
    /// A Wayland compositor. X11 clients may run beside it (XWayland), but
    /// what they see is only their own windows, so the X11 backends are no
    /// use there.
    Wayland,
    X11,
    /// Neither: no display to share, as in a console login or a service
    /// started before the desktop told it where the display is.
    None,
}

/// The graphical session, as its environment describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub kind: SessionKind,
    /// `XDG_SESSION_TYPE` as set, such as `wayland` or `x11`.
    pub session_type: Option<String>,
    /// `XDG_CURRENT_DESKTOP`, a colon-separated list such as
    /// `ubuntu:GNOME`.
    pub desktop: Option<String>,
}

impl Session {
    /// The session described by the variables `var` looks up.
    ///
    /// `WAYLAND_DISPLAY` wins over `DISPLAY`, which a Wayland session also
    /// sets for XWayland. A session type of `wayland` without
    /// `WAYLAND_DISPLAY` is still not one the agent can reach, so only the
    /// variables naming a display count.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Self {
        let set = |name: &str| var(name).filter(|v| !v.trim().is_empty());
        let kind = if set("WAYLAND_DISPLAY").is_some() {
            SessionKind::Wayland
        } else if set("DISPLAY").is_some() {
            SessionKind::X11
        } else {
            SessionKind::None
        };
        Self {
            kind,
            session_type: set("XDG_SESSION_TYPE"),
            desktop: set("XDG_CURRENT_DESKTOP"),
        }
    }

    /// The session of this process.
    pub fn current() -> Self {
        Self::from_env(|name| std::env::var(name).ok())
    }

    /// Whether the desktop is `name` (as GNOME, KDE), which may be one of
    /// several in `XDG_CURRENT_DESKTOP`; ignoring case.
    pub fn is_desktop(&self, name: &str) -> bool {
        self.desktop
            .as_deref()
            .is_some_and(|d| d.split(':').any(|d| d.trim().eq_ignore_ascii_case(name)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(vars: &[(&str, &str)]) -> Session {
        Session::from_env(|name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        })
    }

    #[test]
    fn wayland_wins_over_xwayland() {
        let s = session(&[
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":0"),
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"),
        ]);
        assert_eq!(s.kind, SessionKind::Wayland);
        assert_eq!(s.session_type.as_deref(), Some("wayland"));
        assert!(s.is_desktop("gnome"));
        assert!(s.is_desktop("Ubuntu"));
        assert!(!s.is_desktop("KDE"));
    }

    #[test]
    fn x11_needs_a_display() {
        assert_eq!(session(&[("DISPLAY", ":1")]).kind, SessionKind::X11);
        assert_eq!(
            session(&[("DISPLAY", ":1"), ("WAYLAND_DISPLAY", " ")]).kind,
            SessionKind::X11
        );
        // A session type alone names no display to reach.
        let none = session(&[("XDG_SESSION_TYPE", "x11")]);
        assert_eq!(none.kind, SessionKind::None);
        assert_eq!(none.desktop, None);
    }
}
