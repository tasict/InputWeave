//! What the Linux app decides without GTK: the IPC client of the
//! background service (the agent), the app's view of it, and every text it
//! shows. The app crate only puts these
//! on screen, as the Windows app's `InputWeave.Core` is to its WinUI half.

pub mod agent;
pub mod autostart;
pub mod canvas;
pub mod clipboard_text;
pub mod cursor_speed;
pub mod desktop_picture;
pub mod device_text;
pub mod edge_glow;
pub mod hud;
pub mod license;
pub mod license_text;
pub mod log;
pub mod news;
pub mod pairing_text;
pub mod paths;
pub mod polar;
pub mod sample;
pub mod screen_identity;
pub mod send_choice;
pub mod service;
pub mod settings;
pub mod setup;
pub mod shortcut;
pub mod size;
pub mod snapshot;
pub mod status_text;
pub mod text;
pub mod transfer_text;
pub mod tray_icon;
pub mod trouble_text;
pub mod wake;

pub use iw_proto::app::v1 as proto;

/// The app's id: its GApplication, desktop entry and icon.
pub const APP_ID: &str = "cc.ctsite.inputweave";

/// How the app introduces itself to the agent.
pub fn client_name() -> String {
    format!("linux-ui/{}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_version_is_the_root_workspaces() {
        let root = include_str!("../../../../Cargo.toml");
        let package = root
            .split("[workspace.package]")
            .nth(1)
            .expect("the root workspace's package section");
        let version = package
            .lines()
            .find_map(|l| l.trim().strip_prefix("version = "))
            .expect("a version")
            .trim_matches('"');
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
    }
}
