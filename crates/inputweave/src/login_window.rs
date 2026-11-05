//! The agent at the macOS login window.
//!
//! The installer puts a LaunchAgent in /Library/LaunchAgents, which launchd
//! starts as root whenever the login window shows. It runs as the device
//! of the user who logged in last, if they let other devices control this
//! Mac there (`SetLoginWindowAccess`): their identity, their paired
//! devices and their arrangement, so other devices see the same computer
//! whether someone is logged in or not. It only takes input, through the
//! virtual keyboard and mouse, since posted events never reach the login
//! window: no capture, clipboard, files or app, and no pairing, since
//! nobody could see a code.
//!
//! Running as root, it reads files that user controls. It reads only fixed
//! names in their InputWeave folder, each a regular file they own, reached
//! without following links, and within a size limit
//! (`iw_platform::system::OwnedDir`); from their configuration it takes
//! only what makes it the same device, ignoring paths and the backend. It
//! never writes there: what changes while it runs, such as a layout a peer
//! sends, stays in memory, so it leaves no file root owns in their home.

use iw_platform::{PermissionState, SessionStatus};

use crate::agent::{AgentError, Options};
use crate::config::{BackendConfig, ClipboardConfig, Config, NetworkConfig, PathsConfig};
use crate::store::DevicesFile;

/// The user's InputWeave folder below their home, as `paths::config_dir`
/// has it on macOS.
pub const USER_FOLDER: [&str; 3] = ["Library", "Application Support", "InputWeave"];

/// Where its log goes. Only root reads it.
pub const LOG_FILE: &str = "/Library/Logs/InputWeave/login-window.log";

/// The LaunchAgent that starts it at the login window, which the installer
/// package puts in place (from apps/macos/Installer/login-window/). Builds
/// installed otherwise, such as from Xcode, have none.
pub const AGENT_PLIST: &str = "/Library/LaunchAgents/cc.ctsite.inputweave.loginwindow.plist";

/// Whether a user's agent can let other devices control this Mac at the
/// login window, and if not, why not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Support {
    /// Not a Mac, or not the real backends.
    #[default]
    Unsupported,
    /// The login window's agent is not installed ([`AGENT_PLIST`]).
    NotInstalled,
    /// The agent keeps its files outside the user's InputWeave folder,
    /// through `--config` or the configuration's paths, while the login
    /// window's agent only reads that folder.
    CustomPaths,
    Available,
}

/// Works out [`Support`] for an agent on a Mac with the real backends
/// (`mac`), with the login window's agent `installed`, keeping its files in
/// the user's InputWeave folder (`default_folder`).
pub fn support(mac: bool, installed: bool, default_folder: bool) -> Support {
    if !mac {
        Support::Unsupported
    } else if !installed {
        Support::NotInstalled
    } else if !default_folder {
        Support::CustomPaths
    } else {
        Support::Available
    }
}

/// Whether the agent at the login window failed to start for a reason that
/// may pass, so launchd should try again: only the port being taken, as
/// while another session's agent lets go of it. Anything else (an identity
/// that does not fit, a bad configuration) stays until the user changes it.
pub fn worth_retrying(error: &AgentError) -> bool {
    matches!(
        error,
        AgentError::Net(iw_net::NetError::Io(e)) if e.kind() == std::io::ErrorKind::AddrInUse
    )
}

/// The most read of each file: far more than any real one holds.
#[cfg(unix)]
const MAX_CONFIG: u64 = 1 << 20;
#[cfg(unix)]
const MAX_DEVICES: u64 = 4 << 20;
#[cfg(unix)]
const MAX_IDENTITY: u64 = 64 << 10;

/// What the agent at the login window runs as: the user's device.
pub struct UserState {
    pub config: Config,
    pub identity: iw_net::Identity,
    pub devices: DevicesFile,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("cannot read {what}: {source}")]
    Read {
        what: &'static str,
        source: std::io::Error,
    },
    #[error("{what} is not valid: {detail}")]
    Invalid { what: &'static str, detail: String },
    #[error("{0} is missing")]
    Missing(&'static str),
}

/// Whether `name`, as loginwindow remembers the last user, can name an
/// account.
pub fn valid_user_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.contains(['/', '\0'])
        && name != "."
        && name != ".."
}

/// Reads the state of the user whose home is `home`, owned by `uid`; `None`
/// when they did not let other devices control this Mac at the login
/// window, or never ran InputWeave.
#[cfg(unix)]
pub fn load(home: &std::path::Path, uid: u32) -> Result<Option<UserState>, LoadError> {
    use iw_platform::system::OwnedDir;
    use std::io::ErrorKind;

    let mut dir = OwnedDir::open(home, uid).map_err(|source| LoadError::Read {
        what: "the home folder",
        source,
    })?;
    for name in USER_FOLDER {
        dir = match dir.dir(name) {
            Ok(dir) => dir,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(LoadError::Read {
                    what: "the InputWeave folder",
                    source,
                });
            }
        };
    }
    let read = |dir: &OwnedDir, name: &str, what: &'static str, max: u64| {
        dir.read(name, max)
            .map_err(|source| LoadError::Read { what, source })?
            .map(|bytes| {
                String::from_utf8(bytes).map_err(|_| LoadError::Invalid {
                    what,
                    detail: "not UTF-8".to_owned(),
                })
            })
            .transpose()
    };
    let Some(devices) = read(&dir, "devices.toml", "devices.toml", MAX_DEVICES)? else {
        return Ok(None);
    };
    let devices = DevicesFile::parse(&devices).map_err(|e| LoadError::Invalid {
        what: "devices.toml",
        detail: e.to_string(),
    })?;
    let Some(devices) = devices_for(devices) else {
        return Ok(None);
    };
    let config = match read(&dir, "config.toml", "config.toml", MAX_CONFIG)? {
        Some(text) => text.parse::<Config>().map_err(|e| LoadError::Invalid {
            what: "config.toml",
            detail: e.to_string(),
        })?,
        None => Config::default(),
    };
    let identity = dir.dir("identity").map_err(|source| match source.kind() {
        ErrorKind::NotFound => LoadError::Missing("the identity"),
        _ => LoadError::Read {
            what: "the identity",
            source,
        },
    })?;
    let file = |name: &str, what: &'static str| {
        identity
            .read(name, MAX_IDENTITY)
            .map_err(|source| LoadError::Read { what, source })?
            .ok_or(LoadError::Missing(what))
    };
    let cert = file(iw_net::CERT_FILE, "the identity's certificate")?;
    let key = file(iw_net::KEY_FILE, "the identity's key")?;
    Ok(Some(UserState {
        config: config_for(config),
        identity: iw_net::Identity::from_files(cert, key),
        devices,
    }))
}

/// What the agent at the login window keeps of the user's devices file, or
/// `None` when they did not let other devices control this Mac there.
/// Received files would be written as root, so it has no folder for them
/// and declines them.
pub fn devices_for(stored: DevicesFile) -> Option<DevicesFile> {
    stored.login_window_access.then_some(DevicesFile {
        receive_folder: None,
        ask_before_receiving: false,
        ..stored
    })
}

/// The configuration to run as: the user's device, network and trusted
/// devices and placements, with pairing off. Their paths and backend are
/// ignored.
pub fn config_for(user: Config) -> Config {
    Config {
        device: user.device,
        network: NetworkConfig {
            allow_pairing: false,
            ..user.network
        },
        paths: PathsConfig::default(),
        backend: BackendConfig::Native,
        peers: user.peers,
        layout: user.layout,
        clipboard: ClipboardConfig::default(),
    }
}

/// How the agent runs at the login window: the user's devices in memory
/// only, and no socket, broker, receive folder, transfer history or app.
/// Other devices keep the pictures of this Mac's desktop they have, since
/// this agent cannot read the user's.
pub fn options(devices: DevicesFile) -> Options {
    Options {
        devices: Some(devices),
        open_app: false,
        previews_unchanged: true,
        ..Options::default()
    }
}

/// The login window's session, as other devices are told of it: locked,
/// as a login window is, and taking their input.
#[derive(Debug, Default)]
pub struct LoginWindowSession;

impl SessionStatus for LoginWindowSession {
    fn permissions(&self) -> Vec<PermissionState> {
        Vec::new()
    }

    fn screen_locked(&self) -> bool {
        true
    }

    fn lock_screen_input(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_names_of_accounts_are_taken() {
        assert!(valid_user_name("ctsai"));
        assert!(valid_user_name("first.last"));
        for name in ["", ".", "..", "a/b", "a\0b"] {
            assert!(!valid_user_name(name), "{name:?}");
        }
        assert!(!valid_user_name(&"a".repeat(256)));
    }

    #[test]
    fn only_an_installed_mac_keeping_its_files_in_place_can_turn_it_on() {
        assert_eq!(support(false, true, true), Support::Unsupported);
        assert_eq!(support(true, false, true), Support::NotInstalled);
        assert_eq!(support(true, true, false), Support::CustomPaths);
        assert_eq!(support(true, true, true), Support::Available);
        assert_eq!(Support::default(), Support::Unsupported);
    }

    #[test]
    fn only_a_taken_port_is_worth_another_try() {
        use std::io::{Error, ErrorKind};
        let io = |kind| AgentError::Net(iw_net::NetError::Io(Error::from(kind)));
        assert!(worth_retrying(&io(ErrorKind::AddrInUse)));
        assert!(!worth_retrying(&io(ErrorKind::PermissionDenied)));
        assert!(!worth_retrying(&AgentError::Config(
            crate::config::ConfigError::Invalid("bad".to_owned())
        )));
        assert!(!worth_retrying(&AgentError::Io(Error::from(
            ErrorKind::AddrInUse
        ))));
    }

    #[test]
    fn only_a_turned_on_devices_file_is_used_and_it_takes_no_files() {
        let off = DevicesFile {
            receive_folder: Some("/Users/me/Downloads".into()),
            ..DevicesFile::default()
        };
        assert!(devices_for(off.clone()).is_none());
        let on = devices_for(DevicesFile {
            login_window_access: true,
            ask_before_receiving: true,
            layout_version: 4,
            ..off
        })
        .unwrap();
        assert_eq!(on.receive_folder, None);
        assert!(!on.ask_before_receiving);
        assert_eq!(on.layout_version, 4);
    }

    #[test]
    fn the_users_paths_and_backend_are_ignored_and_pairing_is_off() {
        let user: Config = r#"
            [device]
            id = "mac"
            name = "Studio"

            [network]
            listen = "0.0.0.0:47999"
            discovery = false

            [paths]
            identity_dir = "/etc"
            ipc_socket = "/etc/agent.sock"
            devices_file = "/etc/devices.toml"

            [backend]
            kind = "mock"
        "#
        .parse()
        .unwrap();
        let config = config_for(user);
        assert_eq!(config.device.id, "mac");
        assert_eq!(config.device.name, "Studio");
        assert_eq!(config.network.listen.port(), 47999);
        assert!(!config.network.discovery);
        assert!(!config.network.allow_pairing);
        assert!(config.paths.identity_dir.is_none());
        assert!(config.paths.ipc_socket.is_none());
        assert!(config.paths.devices_file.is_none());
        assert!(matches!(config.backend, BackendConfig::Native));
    }

    #[test]
    fn the_login_window_takes_input_while_locked() {
        let session = LoginWindowSession;
        assert!(session.screen_locked());
        assert!(session.lock_screen_input());
        assert!(session.permissions().is_empty());
    }

    #[cfg(unix)]
    mod files {
        use super::super::*;
        use std::path::{Path, PathBuf};

        /// The user these tests run as, who owns what they create.
        fn me() -> u32 {
            use std::os::unix::fs::MetadataExt;
            let probe = tempfile::tempdir().unwrap();
            std::fs::metadata(probe.path()).unwrap().uid()
        }

        /// A home with an InputWeave folder: its identity, a devices file
        /// with access `on`, and `config`.
        fn home(on: bool, config: &str) -> (tempfile::TempDir, PathBuf) {
            let home = tempfile::tempdir().unwrap();
            let folder = USER_FOLDER
                .iter()
                .fold(home.path().to_owned(), |path, name| path.join(name));
            std::fs::create_dir_all(&folder).unwrap();
            iw_net::Identity::load_or_generate(&folder.join("identity")).unwrap();
            DevicesFile {
                login_window_access: on,
                device_name: "Studio".to_owned(),
                ..DevicesFile::default()
            }
            .save(&folder.join("devices.toml"))
            .unwrap();
            std::fs::write(folder.join("config.toml"), config).unwrap();
            (home, folder)
        }

        fn error(home: &Path, uid: u32) -> String {
            match load(home, uid) {
                Err(e) => e.to_string(),
                Ok(state) => panic!("loaded: {}", state.is_some()),
            }
        }

        #[test]
        fn a_users_folder_is_read_as_their_device() {
            let (home, folder) = home(true, "[device]\nid = \"mac\"\n");
            let state = load(home.path(), me()).unwrap().unwrap();
            assert_eq!(state.config.device.id, "mac");
            assert!(!state.config.network.allow_pairing);
            assert_eq!(state.devices.device_name, "Studio");
            let identity = iw_net::Identity::load_or_generate(&folder.join("identity")).unwrap();
            assert_eq!(state.identity.fingerprint(), identity.fingerprint());
        }

        #[test]
        fn nothing_is_run_when_it_is_off_or_never_set_up() {
            let (home, folder) = home(false, "");
            assert!(load(home.path(), me()).unwrap().is_none());
            std::fs::remove_file(folder.join("devices.toml")).unwrap();
            assert!(load(home.path(), me()).unwrap().is_none());
            let empty = tempfile::tempdir().unwrap();
            assert!(load(empty.path(), me()).unwrap().is_none());
        }

        #[test]
        fn links_other_owners_and_large_files_are_refused() {
            let (home, folder) = home(true, "");
            assert!(error(home.path(), me() + 1).contains("another user"));

            let elsewhere = tempfile::tempdir().unwrap();
            let config = folder.join("config.toml");
            std::fs::remove_file(&config).unwrap();
            std::fs::write(elsewhere.path().join("secret"), "[device]\nid = \"x\"\n").unwrap();
            std::os::unix::fs::symlink(elsewhere.path().join("secret"), &config).unwrap();
            assert!(error(home.path(), me()).contains("is a link"));

            std::fs::remove_file(&config).unwrap();
            std::fs::write(&config, vec![b'#'; MAX_CONFIG as usize + 1]).unwrap();
            assert!(error(home.path(), me()).contains("larger than"));
            std::fs::remove_file(&config).unwrap();

            // The folder itself, moved elsewhere and linked to.
            let moved = elsewhere.path().join("InputWeave");
            std::fs::rename(&folder, &moved).unwrap();
            std::os::unix::fs::symlink(&moved, &folder).unwrap();
            assert!(error(home.path(), me()).contains("is a link"));
        }

        #[test]
        fn an_identity_is_never_made_up() {
            let (home, folder) = home(true, "");
            std::fs::remove_file(folder.join("identity").join(iw_net::KEY_FILE)).unwrap();
            assert!(error(home.path(), me()).contains("key is missing"));
            std::fs::remove_dir_all(folder.join("identity")).unwrap();
            assert!(error(home.path(), me()).contains("the identity is missing"));
            assert!(!folder.join("identity").exists());
        }
    }
}
