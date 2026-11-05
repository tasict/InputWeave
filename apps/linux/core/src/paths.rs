//! Where the app finds the agent and its files, as the agent's own
//! `paths.rs` puts them on Linux.

use std::path::{Path, PathBuf};

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `$XDG_CONFIG_HOME/inputweave`, or `~/.config/inputweave`.
pub fn config_dir() -> Option<PathBuf> {
    env_path("XDG_CONFIG_HOME")
        .or_else(|| env_path("HOME").map(|h| h.join(".config")))
        .map(|c| c.join("inputweave"))
}

/// The agent's IPC socket: `$XDG_RUNTIME_DIR/inputweave/agent.sock`, or
/// next to its configuration without a runtime directory.
pub fn agent_socket() -> Option<PathBuf> {
    if let Some(runtime) = env_path("XDG_RUNTIME_DIR") {
        return Some(runtime.join("inputweave/agent.sock"));
    }
    config_dir().map(|d| d.join("agent.sock"))
}

/// `~/.config/autostart`, where the desktop starts apps at login from.
pub fn autostart_dir() -> Option<PathBuf> {
    env_path("XDG_CONFIG_HOME")
        .or_else(|| env_path("HOME").map(|h| h.join(".config")))
        .map(|c| c.join("autostart"))
}

/// The app's own runtime folder, for what it writes for others to read,
/// such as the tray icon's theme: `$XDG_RUNTIME_DIR/inputweave/app`.
pub fn app_runtime_dir() -> Option<PathBuf> {
    env_path("XDG_RUNTIME_DIR").map(|r| r.join("inputweave/app"))
}

/// The folder a file is in, to show it there.
pub fn containing_folder(path: &Path) -> Option<&Path> {
    path.parent().filter(|p| !p.as_os_str().is_empty())
}
