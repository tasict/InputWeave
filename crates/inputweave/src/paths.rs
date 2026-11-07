//! Default locations of the agent's files.

use std::path::{Path, PathBuf};

use crate::agent::{UserFolder, UserFolders};

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The directory holding the configuration file:
/// - macOS: `~/Library/Application Support/InputWeave`
/// - Windows: `%ProgramData%\InputWeave` (the agent runs as SYSTEM)
/// - Linux and others: `$XDG_CONFIG_HOME/inputweave` or `~/.config/inputweave`
pub fn config_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        env_path("HOME").map(|h| h.join("Library/Application Support/InputWeave"))
    } else if cfg!(windows) {
        env_path("ProgramData").map(|p| p.join("InputWeave"))
    } else {
        env_path("XDG_CONFIG_HOME")
            .or_else(|| env_path("HOME").map(|h| h.join(".config")))
            .map(|c| c.join("inputweave"))
    }
}

pub fn default_config_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

/// Where the agent writes its log when launchd runs it:
/// `~/Library/Logs/InputWeave/agent.log` on macOS. On Windows the service
/// sends the output of the processes it starts to
/// `%ProgramData%\InputWeave\logs`. None on Linux, where the agent logs to
/// stderr, which the user's systemd keeps in the journal
/// ([`journal_unit`]).
pub fn default_log_file() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        env_path("HOME").map(|h| h.join("Library/Logs/InputWeave/agent.log"))
    } else if cfg!(windows) {
        config_dir().map(|d| d.join("logs").join("agent.log"))
    } else {
        None
    }
}

/// The logs that go into diagnostics reports.
pub fn log_files() -> Vec<PathBuf> {
    let Some(agent) = default_log_file() else {
        return Vec::new();
    };
    if cfg!(windows)
        && let Some(dir) = agent.parent()
    {
        return ["service.log", "agent.log", "broker.log"]
            .into_iter()
            .map(|name| dir.join(name))
            .collect();
    }
    vec![agent]
}

/// The agent's systemd user unit on Linux (packaging/linux), whose output
/// the journal keeps.
pub const AGENT_UNIT: &str = "app-cc.ctsite.inputweave@agent.service";

/// The Linux app's unit. Its journal stays out of diagnostics reports:
/// GLib, GTK and the portals may write file names there, which reports
/// never hold.
pub const APP_UNIT: &str = "app-cc.ctsite.inputweave.service";

/// The systemd user unit whose journal goes into diagnostics reports: the
/// agent's, on Linux. None elsewhere.
pub fn journal_unit() -> Option<String> {
    cfg!(target_os = "linux").then(|| AGENT_UNIT.to_owned())
}

/// Where received files go unless the user chose elsewhere:
/// `~/Downloads/InputWeave`. On Windows the broker writes received files,
/// as the user, into the folder it names (the user's Downloads\InputWeave),
/// so the agent has none of its own.
pub fn default_receive_folder() -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    env_path("HOME").map(|h| h.join("Downloads/InputWeave"))
}

/// The folders macOS protects (Files & Folders) that files are sent from
/// and saved in: `~/Downloads`, `~/Desktop` and `~/Documents`, which first-run
/// setup asks for (`agent::folder_access`). None elsewhere.
pub fn user_folders() -> Option<UserFolders> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    env_path("HOME").map(|home| user_folders_in(&home))
}

/// Downloads, Desktop and Documents in `home`.
pub fn user_folders_in(home: &Path) -> UserFolders {
    vec![
        (UserFolder::Downloads, home.join("Downloads")),
        (UserFolder::Desktop, home.join("Desktop")),
        (UserFolder::Documents, home.join("Documents")),
    ]
}

/// Where files copied on another device and pasted here are fetched
/// into, a folder for each paste, before the app pasting them copies them
/// on: `~/Library/Caches/InputWeave/Clipboard` on macOS,
/// `$XDG_CACHE_HOME/inputweave/clipboard` or `~/.cache/inputweave/clipboard`
/// elsewhere. None on Windows, where the agent runs as SYSTEM and the
/// user's files are the broker's to write.
pub fn clipboard_staging() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        env_path("HOME").map(|h| h.join("Library/Caches/InputWeave/Clipboard"))
    } else if cfg!(windows) {
        None
    } else {
        env_path("XDG_CACHE_HOME")
            .or_else(|| env_path("HOME").map(|h| h.join(".cache")))
            .map(|c| c.join("inputweave/clipboard"))
    }
}

/// Paired devices are remembered next to the configuration file.
pub fn default_devices_file(config_file: &Path) -> PathBuf {
    config_file
        .parent()
        .unwrap_or(Path::new("."))
        .join("devices.toml")
}

/// Previews of other devices' desktop pictures are kept next to the
/// devices file, in `previews/<device id>/`.
pub fn previews_dir(devices_file: &Path) -> PathBuf {
    devices_file
        .parent()
        .unwrap_or(Path::new("."))
        .join("previews")
}

/// This computer's license is kept next to the devices file, in
/// `license.toml`.
pub fn license_file(devices_file: &Path) -> PathBuf {
    devices_file
        .parent()
        .unwrap_or(Path::new("."))
        .join("license.toml")
}

/// How other devices told this one to wake them, beside the devices file
/// (`wake_file`).
pub fn wake_file(devices_file: &Path) -> PathBuf {
    devices_file
        .parent()
        .unwrap_or(Path::new("."))
        .join("wake.toml")
}

/// The devices of the group taken offline for now, beside the devices file
/// (`offline_file`).
pub fn offline_file(devices_file: &Path) -> PathBuf {
    devices_file
        .parent()
        .unwrap_or(Path::new("."))
        .join("offline.toml")
}

/// The profiles, beside the devices file (`profiles_file`).
pub fn profiles_file(devices_file: &Path) -> PathBuf {
    devices_file
        .parent()
        .unwrap_or(Path::new("."))
        .join("profiles.toml")
}

/// File transfers that finished are remembered next to the devices file,
/// in `transfers.toml`. On Windows the broker remembers them instead, in
/// the user's `%LOCALAPPDATA%\InputWeave`, and the agent removes this file
/// if an earlier version left it.
pub fn transfers_file(devices_file: &Path) -> PathBuf {
    devices_file
        .parent()
        .unwrap_or(Path::new("."))
        .join("transfers.toml")
}

/// The identity is kept next to the configuration file.
pub fn default_identity_dir(config_file: &Path) -> PathBuf {
    config_file
        .parent()
        .unwrap_or(Path::new("."))
        .join("identity")
}

/// The pipe the agent serves the Windows broker on,
/// `\\.\pipe\InputWeave\broker-<SessionId>` for the session this process
/// runs in. There is no broker elsewhere.
pub fn default_broker_pipe() -> Option<PathBuf> {
    #[cfg(windows)]
    return iw_ipc::broker_pipe_name().ok();
    #[cfg(not(windows))]
    None
}

/// The agent's IPC socket. On Linux it lives in `$XDG_RUNTIME_DIR` when set.
/// On Windows it is the named pipe `\\.\pipe\InputWeave\agent-<SessionId>`
/// of the session this process runs in.
pub fn default_ipc_socket() -> Option<PathBuf> {
    #[cfg(windows)]
    return iw_ipc::agent_pipe_name().ok();
    #[cfg(not(windows))]
    {
        if !cfg!(target_os = "macos")
            && let Some(runtime) = env_path("XDG_RUNTIME_DIR")
        {
            return Some(runtime.join("inputweave/agent.sock"));
        }
        config_dir().map(|d| d.join("agent.sock"))
    }
}
