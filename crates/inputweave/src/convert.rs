//! Conversions between protobuf messages and core types.

use iw_core::HidUsage;
use iw_core::geometry::Rect;
use iw_core::input::Button;
use iw_core::layout::{Display, Edge};
use iw_platform::{
    BackendRole, Firewall, Permission, PermissionAsk, PermissionState, SessionInfo, SessionType,
};
use iw_proto::app::v1 as app;
use iw_proto::peer::v1 as peer;

pub fn button_to_proto(button: Button) -> peer::Button {
    match button {
        Button::Left => peer::Button::Left,
        Button::Right => peer::Button::Right,
        Button::Middle => peer::Button::Middle,
        Button::Back => peer::Button::Back,
        Button::Forward => peer::Button::Forward,
    }
}

pub fn button_from_proto(value: i32) -> Option<Button> {
    match peer::Button::try_from(value).ok()? {
        peer::Button::Left => Some(Button::Left),
        peer::Button::Right => Some(Button::Right),
        peer::Button::Middle => Some(Button::Middle),
        peer::Button::Back => Some(Button::Back),
        peer::Button::Forward => Some(Button::Forward),
        peer::Button::Unspecified => None,
    }
}

pub fn edge_to_peer(edge: Edge) -> peer::ScreenEdge {
    match edge {
        Edge::Left => peer::ScreenEdge::Left,
        Edge::Right => peer::ScreenEdge::Right,
        Edge::Top => peer::ScreenEdge::Top,
        Edge::Bottom => peer::ScreenEdge::Bottom,
    }
}

pub fn edge_from_peer(value: i32) -> Option<Edge> {
    match peer::ScreenEdge::try_from(value).ok()? {
        peer::ScreenEdge::Left => Some(Edge::Left),
        peer::ScreenEdge::Right => Some(Edge::Right),
        peer::ScreenEdge::Top => Some(Edge::Top),
        peer::ScreenEdge::Bottom => Some(Edge::Bottom),
        peer::ScreenEdge::Unspecified => None,
    }
}

pub fn edge_to_app(edge: Edge) -> app::ScreenEdge {
    match edge {
        Edge::Left => app::ScreenEdge::Left,
        Edge::Right => app::ScreenEdge::Right,
        Edge::Top => app::ScreenEdge::Top,
        Edge::Bottom => app::ScreenEdge::Bottom,
    }
}

pub fn usage_from_proto(value: u32) -> Option<HidUsage> {
    u16::try_from(value).ok().map(HidUsage::new)
}

/// A display for a peer, with the preview of its desktop picture if one
/// is given.
pub fn display_to_proto(d: &Display, preview: Option<&[u8]>) -> peer::Display {
    peer::Display {
        id: d.id.clone(),
        x: d.bounds.x,
        y: d.bounds.y,
        width: d.bounds.width,
        height: d.bounds.height,
        scale: d.scale,
        primary: d.primary,
        preview_jpeg: preview.map(<[u8]>::to_vec).unwrap_or_default(),
    }
}

/// Rejects displays a peer could use to break layout geometry.
pub fn display_from_proto(d: &peer::Display) -> Option<Display> {
    let finite = [d.x, d.y, d.width, d.height, d.scale]
        .iter()
        .all(|v| v.is_finite());
    if !finite || d.width <= 0.0 || d.height <= 0.0 || d.scale <= 0.0 {
        return None;
    }
    Some(Display {
        id: d.id.clone(),
        bounds: Rect::new(d.x, d.y, d.width, d.height),
        scale: d.scale,
        primary: d.primary,
    })
}

pub fn this_os() -> peer::Os {
    if cfg!(target_os = "macos") {
        peer::Os::Macos
    } else if cfg!(windows) {
        peer::Os::Windows
    } else if cfg!(target_os = "linux") {
        peer::Os::Linux
    } else {
        peer::Os::Unspecified
    }
}

pub fn permission_kind_to_proto(p: Permission) -> app::Permission {
    match p {
        Permission::Accessibility => app::Permission::Accessibility,
        Permission::InputMonitoring => app::Permission::InputMonitoring,
        Permission::LocalNetwork => app::Permission::LocalNetwork,
        Permission::RemoteControl => app::Permission::RemoteControl,
        Permission::InputCapture => app::Permission::InputCapture,
    }
}

/// A permission and its state, as apps hear of it.
pub fn permission_to_proto(p: &PermissionState) -> app::PermissionStatus {
    let permission = permission_kind_to_proto(p.permission);
    let ask = match p.ask {
        PermissionAsk::NotAskable => app::PermissionAsk::NotAskable,
        PermissionAsk::Askable => app::PermissionAsk::Askable,
        PermissionAsk::Asking => app::PermissionAsk::Asking,
        PermissionAsk::Denied => app::PermissionAsk::Denied,
        PermissionAsk::Failed => app::PermissionAsk::Failed,
        PermissionAsk::Restoring => app::PermissionAsk::Restoring,
        PermissionAsk::Stopped => app::PermissionAsk::Stopped,
    };
    app::PermissionStatus {
        permission: permission.into(),
        granted: p.granted,
        ask: ask.into(),
        asked: p.asked,
        failure: p.failure.clone().unwrap_or_default(),
    }
}

/// What kind of session this device runs and what serves it, as apps hear
/// of it.
pub fn session_to_proto(info: &SessionInfo) -> app::SessionInfo {
    app::SessionInfo {
        r#type: match info.kind {
            SessionType::Unknown => app::SessionType::Unspecified,
            SessionType::X11 => app::SessionType::X11,
            SessionType::Wayland => app::SessionType::Wayland,
        }
        .into(),
        desktop: info.desktop.clone(),
        backends: info
            .backends
            .iter()
            .map(|b| app::SessionBackend {
                role: match b.role {
                    BackendRole::Input => app::BackendRole::Input,
                    BackendRole::Capture => app::BackendRole::Capture,
                    BackendRole::Clipboard => app::BackendRole::Clipboard,
                    BackendRole::Displays => app::BackendRole::Displays,
                    BackendRole::DesktopPictures => app::BackendRole::DesktopPictures,
                }
                .into(),
                implementation: b.implementation.clone(),
                problem: b.problem.clone(),
            })
            .collect(),
    }
}

/// What the firewall does to the agent's UDP `port`, for clients; nothing
/// when that is not known.
pub fn firewall_to_proto(firewall: Firewall, port: u16) -> Option<app::FirewallStatus> {
    let (state, blocked) = match firewall {
        Firewall::Unknown => return None,
        Firewall::Off => (app::FirewallState::Off, Vec::new()),
        Firewall::Allows => (app::FirewallState::Allows, Vec::new()),
        Firewall::Blocks(blocked) => (app::FirewallState::Blocks, blocked.ports(port)),
        Firewall::OnUnreadable => (app::FirewallState::OnUnreadable, Vec::new()),
    };
    Some(app::FirewallStatus {
        state: state.into(),
        port: u32::from(port),
        blocked_ports: blocked.into_iter().map(u32::from).collect(),
    })
}

/// The permission an app names (`RequestPermission`), if the agent knows it.
pub fn permission_from_proto(p: app::Permission) -> Option<Permission> {
    Some(match p {
        app::Permission::Unspecified => return None,
        app::Permission::Accessibility => Permission::Accessibility,
        app::Permission::InputMonitoring => Permission::InputMonitoring,
        app::Permission::LocalNetwork => Permission::LocalNetwork,
        app::Permission::RemoteControl => Permission::RemoteControl,
        app::Permission::InputCapture => Permission::InputCapture,
    })
}

/// A permission's state as an agent reported it to an app.
pub fn permission_state_from_proto(p: &app::PermissionStatus) -> Option<PermissionState> {
    let ask = match p.ask() {
        app::PermissionAsk::NotAskable => PermissionAsk::NotAskable,
        app::PermissionAsk::Askable => PermissionAsk::Askable,
        app::PermissionAsk::Asking => PermissionAsk::Asking,
        app::PermissionAsk::Denied => PermissionAsk::Denied,
        app::PermissionAsk::Failed => PermissionAsk::Failed,
        app::PermissionAsk::Restoring => PermissionAsk::Restoring,
        app::PermissionAsk::Stopped => PermissionAsk::Stopped,
    };
    Some(PermissionState {
        permission: permission_from_proto(p.permission())?,
        granted: p.granted,
        ask,
        asked: p.asked,
        failure: (!p.failure.is_empty()).then(|| p.failure.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_round_trip() {
        for b in [
            Button::Left,
            Button::Right,
            Button::Middle,
            Button::Back,
            Button::Forward,
        ] {
            assert_eq!(button_from_proto(button_to_proto(b).into()), Some(b));
        }
        assert_eq!(button_from_proto(0), None);
        assert_eq!(button_from_proto(99), None);
    }

    #[test]
    fn rejects_out_of_range_usages_and_degenerate_displays() {
        assert_eq!(usage_from_proto(0x04), Some(HidUsage::new(0x04)));
        assert_eq!(usage_from_proto(0x1_0000), None);

        let good = peer::Display {
            id: "main".into(),
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 50.0,
            scale: 2.0,
            primary: true,
            preview_jpeg: Vec::new(),
        };
        assert!(display_from_proto(&good).is_some());
        for bad in [
            peer::Display {
                width: 0.0,
                ..good.clone()
            },
            peer::Display {
                x: f64::NAN,
                ..good.clone()
            },
            peer::Display {
                scale: -1.0,
                ..good.clone()
            },
        ] {
            assert!(display_from_proto(&bad).is_none());
        }
    }
}
