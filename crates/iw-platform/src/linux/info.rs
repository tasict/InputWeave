//! What the Linux backends tell the app about the session
//! ([`crate::SessionStatus::session_info`]): its kind, the desktop, and
//! what serves each part of it, for the troubleshooting page.

use super::desktop::{Desktop, desktop_of};
use super::session::{Session, SessionKind};
use crate::{Backend, BackendRole, SessionInfo, SessionType};

/// What the session's backends share of it: its kind and desktop, and
/// what reads the desktop pictures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Info {
    kind: SessionType,
    desktop: String,
    pictures: Desktop,
}

impl Info {
    pub(crate) fn of(session: &Session) -> Self {
        let desktop = session.desktop.clone().unwrap_or_default();
        Self {
            kind: match session.kind {
                SessionKind::X11 => SessionType::X11,
                SessionKind::Wayland => SessionType::Wayland,
                SessionKind::None => SessionType::Unknown,
            },
            pictures: desktop_of(&desktop),
            desktop,
        }
    }

    /// Whose settings the desktop pictures come from.
    pub(crate) fn desktop(&self) -> Desktop {
        self.pictures
    }

    /// The session with `backends`, and what reads its desktop pictures.
    pub(crate) fn with(&self, mut backends: Vec<Backend>) -> SessionInfo {
        backends.push(match self.pictures {
            Desktop::Gnome => Backend::serving(BackendRole::DesktopPictures, "GNOME settings"),
            Desktop::Kde => Backend::serving(BackendRole::DesktopPictures, "KDE Plasma settings"),
            Desktop::Other => {
                Backend::missing(BackendRole::DesktopPictures, "not read on this desktop")
            }
        });
        SessionInfo {
            kind: self.kind,
            desktop: self.desktop.clone(),
            backends,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(kind: SessionKind, desktop: Option<&str>) -> Session {
        Session {
            kind,
            session_type: None,
            desktop: desktop.map(str::to_owned),
        }
    }

    #[test]
    fn the_session_says_its_kind_desktop_and_pictures() {
        let info = Info::of(&session(SessionKind::Wayland, Some("ubuntu:GNOME")));
        assert_eq!(info.desktop(), Desktop::Gnome);
        let input = Backend::serving(BackendRole::Input, "XTest");
        assert_eq!(
            info.with(vec![input.clone()]),
            SessionInfo {
                kind: SessionType::Wayland,
                desktop: "ubuntu:GNOME".to_owned(),
                backends: vec![
                    input,
                    Backend::serving(BackendRole::DesktopPictures, "GNOME settings"),
                ],
            }
        );
        let sway = Info::of(&session(SessionKind::Wayland, Some("sway")));
        assert_eq!(
            sway.with(Vec::new()).backends,
            [Backend::missing(
                BackendRole::DesktopPictures,
                "not read on this desktop"
            )]
        );
        assert_eq!(
            Info::of(&session(SessionKind::X11, None))
                .with(Vec::new())
                .kind,
            SessionType::X11
        );
    }
}
