//! What the troubleshooting page and first-run setup say of this desktop:
//! what serves each part of the
//! session, the firewall, and the permissions the desktop's portals ask
//! for.

use crate::proto::{
    BackendRole, FirewallState, FirewallStatus, Permission, PermissionAsk, PermissionStatus,
    SessionBackend, SessionInfo, SessionType,
};
use iw_proto::network;

use crate::snapshot::Snapshot;
use crate::text::{UiText, keys};
use crate::ui;

/// The session in one line: "Wayland · ubuntu:GNOME", technical names as
/// they are.
pub fn session_line(session: &SessionInfo) -> UiText {
    let kind = match session.r#type() {
        SessionType::X11 => "X11",
        SessionType::Wayland => "Wayland",
        SessionType::Unspecified => return ui!(keys::SESSION_UNKNOWN),
    };
    if session.desktop.is_empty() {
        UiText::verbatim(kind)
    } else {
        UiText::verbatim(format!("{kind} · {}", session.desktop))
    }
}

/// The part a backend serves, in the app's words; `None` for one from a
/// newer background service.
pub fn role_key(role: BackendRole) -> Option<&'static str> {
    Some(match role {
        BackendRole::Input => keys::ROLE_INPUT,
        BackendRole::Capture => keys::ROLE_CAPTURE,
        BackendRole::Clipboard => keys::ROLE_CLIPBOARD,
        BackendRole::Displays => keys::ROLE_DISPLAYS,
        BackendRole::DesktopPictures => keys::ROLE_DESKTOP_PICTURES,
        BackendRole::Unspecified => return None,
    })
}

/// What serves it and what keeps it from working, as the service says
/// (its problems are in English, as in its diagnostics).
pub fn backend_text(backend: &SessionBackend) -> UiText {
    match (
        backend.implementation.is_empty(),
        backend.problem.is_empty(),
    ) {
        (false, true) => UiText::verbatim(backend.implementation.clone()),
        (false, false) => ui!(
            keys::BACKEND_WITH_PROBLEM,
            backend.implementation.clone(),
            backend.problem.clone()
        ),
        (true, false) => UiText::verbatim(backend.problem.clone()),
        (true, true) => ui!(keys::BACKEND_NONE),
    }
}

/// The name the agent gives GNOME's clipboard backend
/// (`SessionBackend.implementation`, iw-platform's Wayland session). Until
/// the agent says outright that the clipboard waits on remote control,
/// the app recognizes it by this name; it must stay the agent's.
pub const CLIPBOARD_PORTAL: &str = "Clipboard portal";

/// The clipboard is GNOME's Clipboard portal, which works only while
/// remote control runs, and it does not now: the user must allow remote
/// control (with clipboard access) for the clipboard to be shared.
pub fn clipboard_needs_remote_control(model: &Snapshot) -> bool {
    model.status.session.as_ref().is_some_and(|s| {
        s.backends.iter().any(|b| {
            b.role() == BackendRole::Clipboard
                && b.implementation == CLIPBOARD_PORTAL
                && !b.problem.is_empty()
        })
    })
}

/// What to say of the firewall, and the command to copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirewallHint {
    pub text: UiText,
    pub command: String,
}

/// Something only when ufw keeps other devices out, or may: nothing when
/// it is off, lets them in, or the service cannot tell, nor when the ports
/// it names are neither the agent's nor mDNS's. The ports and the command
/// are the agent's and `inputweave ctl`'s (`iw_proto::network`).
pub fn firewall(status: Option<&FirewallStatus>) -> Option<FirewallHint> {
    let status = status?;
    let port = network::effective_port(status.port);
    let text = match status.state() {
        FirewallState::Blocks => match network::blocked(port, &status.blocked_ports) {
            (true, true) => ui!(keys::FIREWALL_BLOCKS_BOTH, port.to_string()),
            (false, true) => ui!(keys::FIREWALL_BLOCKS_MDNS),
            (true, false) => ui!(keys::FIREWALL_BLOCKS_PORT, port.to_string()),
            (false, false) => return None,
        },
        FirewallState::OnUnreadable => ui!(keys::FIREWALL_UNREADABLE),
        FirewallState::Unspecified | FirewallState::Off | FirewallState::Allows => return None,
    };
    Some(FirewallHint {
        text,
        command: network::ufw_allow(port),
    })
}

/// Whether `XDG_CURRENT_DESKTOP` ("ubuntu:GNOME") names GNOME.
pub fn is_gnome(desktop: &str) -> bool {
    desktop.split(':').any(|d| d.eq_ignore_ascii_case("GNOME"))
}

/// One permission the desktop asks for, as a row shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionRow {
    pub permission: Permission,
    pub title: &'static str,
    pub state: UiText,
    /// The button that asks the desktop, if asking now makes sense.
    pub button: Option<&'static str>,
    /// Shown under it as well.
    pub note: Option<UiText>,
}

/// The permissions the service asks for itself (the Wayland portals), in
/// its order; none for one it cannot ask for.
pub fn permission_rows(model: &Snapshot) -> Vec<PermissionRow> {
    // Only what the service says now: a button would ask nobody while it
    // cannot be reached.
    if !model.connected() {
        return Vec::new();
    }
    let gnome = model
        .status
        .session
        .as_ref()
        .is_some_and(|s| is_gnome(&s.desktop));
    model
        .status
        .permissions
        .iter()
        .filter_map(|p| permission_row(p, gnome))
        .collect()
}

fn permission_row(status: &PermissionStatus, gnome: bool) -> Option<PermissionRow> {
    let permission = status.permission();
    let title = match permission {
        Permission::RemoteControl => keys::PERMISSION_REMOTE_CONTROL,
        Permission::InputCapture => keys::PERMISSION_INPUT_CAPTURE,
        _ => return None,
    };
    let (state, button) = match status.ask() {
        // Granted elsewhere (macOS's settings): no row here.
        PermissionAsk::NotAskable => return None,
        PermissionAsk::Asking => (ui!(keys::PERMISSION_ASKING), None),
        _ if status.granted => (ui!(keys::PERMISSION_GRANTED), None),
        PermissionAsk::Restoring => (ui!(keys::PERMISSION_RESTORING), None),
        PermissionAsk::Denied => (ui!(keys::PERMISSION_DENIED), Some(keys::PERMISSION_ALLOW)),
        PermissionAsk::Failed if status.failure.is_empty() => (
            ui!(keys::PERMISSION_FAILED_PLAIN),
            Some(keys::PERMISSION_ALLOW),
        ),
        PermissionAsk::Failed => (
            ui!(keys::PERMISSION_FAILED, status.failure.clone()),
            Some(keys::PERMISSION_ALLOW),
        ),
        PermissionAsk::Stopped => (
            ui!(keys::PERMISSION_STOPPED),
            Some(keys::PERMISSION_ALLOW_AGAIN),
        ),
        PermissionAsk::Askable => (
            ui!(keys::PERMISSION_NOT_ASKED),
            Some(keys::PERMISSION_ALLOW),
        ),
    };
    // GNOME's InputCapture portal (version 1) keeps no grant across logins.
    let note = (gnome && permission == Permission::InputCapture)
        .then(|| ui!(keys::PERMISSION_EVERY_LOGIN));
    Some(PermissionRow {
        permission,
        title,
        state,
        button,
        note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::Status;
    use crate::snapshot::Link;
    use crate::text::{Catalog, Language};

    fn en(text: &UiText) -> String {
        Catalog::for_language(Language::English).render(text)
    }

    fn model(status: Status) -> Snapshot {
        Snapshot {
            link: Link::Connected,
            status,
            ..Snapshot::default()
        }
    }

    fn backend(role: BackendRole, implementation: &str, problem: &str) -> SessionBackend {
        SessionBackend {
            role: role as i32,
            implementation: implementation.into(),
            problem: problem.into(),
        }
    }

    fn session(desktop: &str, backends: Vec<SessionBackend>) -> SessionInfo {
        SessionInfo {
            r#type: SessionType::Wayland as i32,
            desktop: desktop.into(),
            backends,
        }
    }

    #[test]
    fn the_session_and_its_parts_in_a_line_each() {
        assert_eq!(
            en(&session_line(&session("ubuntu:GNOME", vec![]))),
            "Wayland · ubuntu:GNOME"
        );
        let x11 = SessionInfo {
            r#type: SessionType::X11 as i32,
            ..SessionInfo::default()
        };
        assert_eq!(en(&session_line(&x11)), "X11");
        assert_eq!(en(&session_line(&SessionInfo::default())), "Unknown");
        assert_eq!(
            role_key(BackendRole::Capture)
                .map(|k| en(&ui!(k)))
                .as_deref(),
            Some("Controlling other computers")
        );
        assert_eq!(role_key(BackendRole::Unspecified), None);
        let b = |i, p| en(&backend_text(&backend(BackendRole::Input, i, p)));
        assert_eq!(b("XTest", ""), "XTest");
        assert_eq!(
            b(
                "RemoteDesktop portal",
                "starting: remote control being asked"
            ),
            "RemoteDesktop portal: starting: remote control being asked"
        );
        assert_eq!(b("", "no portal"), "no portal");
        assert_eq!(b("", ""), "Nothing");
    }

    #[test]
    fn the_gnome_clipboard_waits_for_remote_control() {
        let with = |b: SessionBackend| {
            model(Status {
                session: Some(session("GNOME", vec![b])),
                ..Status::default()
            })
        };
        assert!(clipboard_needs_remote_control(&with(backend(
            BackendRole::Clipboard,
            "Clipboard portal",
            "only while remote control runs"
        ))));
        assert!(!clipboard_needs_remote_control(&with(backend(
            BackendRole::Clipboard,
            "Clipboard portal",
            ""
        ))));
        assert!(!clipboard_needs_remote_control(&with(backend(
            BackendRole::Clipboard,
            "not used yet",
            ""
        ))));
        assert!(!clipboard_needs_remote_control(&with(backend(
            BackendRole::Input,
            "Clipboard portal",
            "x"
        ))));
        assert!(!clipboard_needs_remote_control(&model(Status::default())));
    }

    fn fw(state: FirewallState, port: u32, blocked: &[u32]) -> FirewallStatus {
        FirewallStatus {
            state: state as i32,
            port,
            blocked_ports: blocked.to_vec(),
        }
    }

    #[test]
    fn the_firewall_is_mentioned_only_when_it_keeps_others_out_or_may() {
        let hint = |s: FirewallStatus| firewall(Some(&s)).map(|h| (en(&h.text), h.command));
        assert_eq!(firewall(None), None);
        for state in [
            FirewallState::Unspecified,
            FirewallState::Off,
            FirewallState::Allows,
        ] {
            assert_eq!(hint(fw(state, 47820, &[])), None);
        }
        assert_eq!(
            hint(fw(FirewallState::Blocks, 47820, &[])),
            Some((
                "ufw blocks UDP 47820, so other computers can't connect to this one.".into(),
                "sudo ufw allow InputWeave".into()
            ))
        );
        assert_eq!(
            hint(fw(FirewallState::Blocks, 47820, &[5353])).unwrap().0,
            "ufw blocks UDP 5353, so other computers can't find this one on the network."
        );
        assert_eq!(
            hint(fw(FirewallState::Blocks, 47820, &[47820, 5353]))
                .unwrap()
                .0,
            "ufw blocks UDP 47820 and 5353, so other computers can neither find this one nor connect to it."
        );
        assert_eq!(
            hint(fw(FirewallState::Blocks, 50000, &[50000])),
            Some((
                "ufw blocks UDP 50000, so other computers can't connect to this one.".into(),
                "sudo ufw allow InputWeave && sudo ufw allow 50000/udp".into()
            ))
        );
        assert_eq!(
            hint(fw(FirewallState::OnUnreadable, 47820, &[])),
            Some((
                "ufw is on, and only an administrator can read its rules. If other computers can't connect to this one, let InputWeave in.".into(),
                "sudo ufw allow InputWeave".into()
            ))
        );
    }

    fn permission(p: Permission, granted: bool, ask: PermissionAsk) -> PermissionStatus {
        PermissionStatus {
            permission: p as i32,
            granted,
            ask: ask as i32,
            asked: 1,
            failure: String::new(),
        }
    }

    #[test]
    fn each_portal_permission_says_where_it_is_and_what_asks_again() {
        let rows = |desktop: &str, permissions: Vec<PermissionStatus>| {
            permission_rows(&model(Status {
                session: Some(session(desktop, vec![])),
                permissions,
                ..Status::default()
            }))
            .into_iter()
            .map(|r| {
                (
                    en(&ui!(r.title)),
                    en(&r.state),
                    r.button.map(|b| en(&ui!(b))),
                    r.note.map(|n| en(&n)),
                )
            })
            .collect::<Vec<_>>()
        };
        // None from a service that cannot be reached now.
        let mut away = model(Status {
            permissions: vec![permission(
                Permission::InputCapture,
                false,
                PermissionAsk::Askable,
            )],
            ..Status::default()
        });
        away.link = crate::snapshot::Link::Connecting;
        assert!(permission_rows(&away).is_empty());
        // macOS's permissions are granted elsewhere: none listed.
        assert!(
            rows(
                "",
                vec![permission(
                    Permission::Accessibility,
                    false,
                    PermissionAsk::NotAskable
                )]
            )
            .is_empty()
        );
        let listed = rows(
            "ubuntu:GNOME",
            vec![
                permission(Permission::RemoteControl, true, PermissionAsk::Askable),
                permission(Permission::InputCapture, false, PermissionAsk::Askable),
            ],
        );
        assert_eq!(
            listed,
            [
                (
                    "Other computers control this one".into(),
                    "Allowed".into(),
                    None,
                    None
                ),
                (
                    "This computer's keyboard and mouse control others".into(),
                    "Not allowed yet".into(),
                    Some("Allow…".into()),
                    Some("GNOME asks again at every login.".into())
                ),
            ]
        );
        let state = |ask, granted| {
            let mut p = permission(Permission::RemoteControl, granted, ask);
            p.failure = if ask == PermissionAsk::Failed {
                "no RemoteDesktop portal".into()
            } else {
                String::new()
            };
            let r = rows("KDE", vec![p]).remove(0);
            (r.1, r.2)
        };
        assert_eq!(
            state(PermissionAsk::Asking, false),
            ("The desktop is asking…".into(), None)
        );
        assert_eq!(
            state(PermissionAsk::Restoring, false),
            ("Starting with what you allowed before…".into(), None)
        );
        assert_eq!(
            state(PermissionAsk::Denied, false),
            ("Not allowed".into(), Some("Allow…".into()))
        );
        assert_eq!(
            state(PermissionAsk::Failed, false),
            (
                "Couldn't ask: no RemoteDesktop portal".into(),
                Some("Allow…".into())
            )
        );
        assert_eq!(
            state(PermissionAsk::Stopped, false),
            (
                "Stopped from the desktop".into(),
                Some("Allow Again…".into())
            )
        );
        // KDE remembers input capture: no note there.
        assert_eq!(
            rows(
                "KDE",
                vec![permission(
                    Permission::InputCapture,
                    false,
                    PermissionAsk::Askable
                )]
            )[0]
            .3,
            None
        );
    }

    #[test]
    fn ports_neither_ours_nor_mdns_are_not_said_to_be_ours() {
        assert_eq!(
            firewall(Some(&fw(FirewallState::Blocks, 47820, &[12345]))),
            None
        );
        assert!(is_gnome("ubuntu:GNOME") && is_gnome("GNOME-Classic:GNOME"));
        assert!(!is_gnome("KDE") && !is_gnome(""));
    }
}
