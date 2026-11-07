//! The agent's configuration file (TOML).
//!
//! ```toml
//! # Optional. The id defaults to one derived from this device's
//! # certificate, the name to the computer's host name.
//! [device]
//! id = "mac"
//! name = "MacBook Pro"
//!
//! [network]
//! listen = "0.0.0.0:47820"
//! discovery = true       # announce and browse with mDNS
//! allow_pairing = true   # accept pairing requests from other devices
//!
//! # Optional. Where the agent keeps its files, when not where the platform
//! # keeps them. On Linux and macOS, received files go to receive_folder
//! # unless the app chose another; the log goes to log_file, moved aside to
//! # log_file.1 past 5 MB, instead of standard error (or launchd's file).
//! [paths]
//! receive_folder = "/srv/inputweave/received"
//! log_file = "/var/log/inputweave/agent.log"
//!
//! [backend]
//! kind = "mock"
//! displays = [{ id = "builtin", x = 0, y = 0, width = 1512, height = 982, scale = 2.0, primary = true }]
//!
//! # Devices trusted without pairing. Paired devices are kept in a separate
//! # file that the agent writes (`devices.toml` next to this one).
//! [[peers]]
//! id = "win"
//! name = "Windows PC"
//! address = "192.168.1.20:47820"
//! fingerprint = "aa:bb:…"
//! swap_gui_and_control = true
//!
//! [clipboard]
//! max_bytes = 8388608    # larger clipboards are not synced
//!
//! # Optional. Devices without a placement here are placed by the agent:
//! # next to this device once their displays are known, remembered in
//! # `devices.toml`.
//! [[layout]]
//! device = "mac"
//! x = 0
//! y = 400
//!
//! [[layout]]
//! device = "win"
//! x = 1512
//! y = 0
//! ```
//!
//! Between two peers, only the one with the smaller device id dials. It
//! finds the other through `address`, mDNS, or the address it was last
//! reached at. The arrangement is shared between connected devices and kept
//! in `devices.toml`; placements in `[[layout]]` override it on this device
//! only, and cannot be moved from the UI.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use iw_core::geometry::Rect;
use iw_core::layout::Display;
use iw_net::{Fingerprint, is_valid_device_id, is_valid_device_name};
use serde::{Deserialize, Deserializer, Serialize};

pub const DEFAULT_PORT: u16 = iw_proto::network::DEFAULT_PORT;

/// Every section is optional: an empty file, or none at all, runs the
/// native backends with defaults.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub device: DeviceConfig,
    #[serde(default)]
    pub network: NetworkConfig,
    #[serde(default)]
    pub paths: PathsConfig,
    #[serde(default)]
    pub backend: BackendConfig,
    #[serde(default)]
    pub peers: Vec<PeerConfig>,
    #[serde(default)]
    pub layout: Vec<PlacementConfig>,
    #[serde(default)]
    pub clipboard: ClipboardConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClipboardConfig {
    /// Larger clipboards are not synced (docs/architecture.md §8).
    pub max_bytes: usize,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
        }
    }
}

/// This device. Empty values are filled in by [`Config::resolve_device`].
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceConfig {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    pub listen: SocketAddr,
    /// Announce this device and look for others with mDNS.
    pub discovery: bool,
    /// Accept pairing requests from devices that are not trusted yet.
    pub allow_pairing: bool,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
            discovery: true,
            allow_pairing: true,
        }
    }
}

/// Overrides for where the agent keeps its files. Unset paths use the
/// platform defaults.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathsConfig {
    pub identity_dir: Option<PathBuf>,
    pub ipc_socket: Option<PathBuf>,
    /// Paired devices and placements, written by the agent.
    pub devices_file: Option<PathBuf>,
    /// Where received files go unless the app chose another folder, for an
    /// agent with no app beside it to choose one. Not on
    /// Windows, where the broker writes received files as the user, into
    /// that user's Downloads.
    pub receive_folder: Option<PathBuf>,
    /// The agent's log, instead of standard error; moved aside when it
    /// grows too large (`main.rs`), and read into diagnostics reports.
    pub log_file: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum BackendConfig {
    /// In-memory backends: no real input is captured, injections are logged.
    Mock {
        #[serde(default)]
        displays: Vec<DisplayConfig>,
        /// A folder whose Downloads, Desktop and Documents stand in for the
        /// user's, which `RequestFolderAccess` asks for; for tests. Without
        /// it, there are none to ask for.
        #[serde(default)]
        folders: Option<std::path::PathBuf>,
    },
    /// The platform's real input backends.
    #[default]
    Native,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayConfig {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default = "one")]
    pub scale: f64,
    #[serde(default)]
    pub primary: bool,
}

fn one() -> f64 {
    1.0
}

impl From<&Display> for DisplayConfig {
    fn from(d: &Display) -> Self {
        DisplayConfig {
            id: d.id.clone(),
            x: d.bounds.x,
            y: d.bounds.y,
            width: d.bounds.width,
            height: d.bounds.height,
            scale: d.scale,
            primary: d.primary,
        }
    }
}

impl From<&DisplayConfig> for Display {
    fn from(d: &DisplayConfig) -> Self {
        Display {
            id: d.id.clone(),
            bounds: Rect::new(d.x, d.y, d.width, d.height),
            scale: d.scale,
            primary: d.primary,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerConfig {
    pub id: String,
    pub name: String,
    pub address: Option<SocketAddr>,
    #[serde(deserialize_with = "fingerprint")]
    pub fingerprint: Fingerprint,
    #[serde(default)]
    pub swap_gui_and_control: bool,
}

fn fingerprint<'de, D: Deserializer<'de>>(d: D) -> Result<Fingerprint, D::Error> {
    String::deserialize(d)?
        .parse()
        .map_err(serde::de::Error::custom)
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementConfig {
    pub device: String,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid configuration: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

impl Config {
    /// Reads the configuration file; a missing file means the defaults,
    /// which is how the agent runs when the app starts it.
    pub fn load_or_default(path: &Path) -> Result<Self, ConfigError> {
        match Self::load(path) {
            Err(ConfigError::Read { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(Self::default())
            }
            other => other,
        }
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        text.parse()
    }

    /// Fills in an unset device id and name: the id from the first eight
    /// bytes of this device's certificate fingerprint, the name from the
    /// computer's name, falling back to the id.
    pub fn resolve_device(&mut self, fingerprint: &Fingerprint, computer_name: Option<String>) {
        if self.device.id.is_empty() {
            self.device.id = crate::hex::hex(&fingerprint.as_bytes()[..8]);
        }
        if self.device.name.is_empty() {
            self.device.name = computer_name
                .filter(|n| is_valid_device_name(n))
                .unwrap_or_else(|| self.device.id.clone());
        }
    }

    /// Checks values and references between sections that serde cannot
    /// express. An unset device id or name is accepted; see
    /// [`Config::resolve_device`].
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |msg: String| Err(ConfigError::Invalid(msg));
        if cfg!(windows) && self.paths.receive_folder.is_some() {
            return invalid(
                "paths.receive_folder is not used on Windows: received files go to the \
                 Downloads folder of the user logged in"
                    .to_owned(),
            );
        }
        let device = &self.device;
        if !device.id.is_empty() && !is_valid_device_id(&device.id) {
            return invalid(format!(
                "device id {:?} must be 1-64 letters, digits, '.', '_' or '-'",
                device.id
            ));
        }
        if !device.name.is_empty() && !is_valid_device_name(&device.name) {
            return invalid(format!(
                "device name {:?} must be 1-64 characters without control characters",
                device.name
            ));
        }
        let mut ids = HashSet::from([device.id.as_str()]);
        let mut fingerprints = HashSet::new();
        for peer in &self.peers {
            if !is_valid_device_id(&peer.id) {
                return invalid(format!(
                    "peer id {:?} must be 1-64 letters, digits, '.', '_' or '-'",
                    peer.id
                ));
            }
            if !ids.insert(peer.id.as_str()) {
                return invalid(format!("device id {:?} is used more than once", peer.id));
            }
            if !fingerprints.insert(peer.fingerprint) {
                return invalid(format!(
                    "peer {:?} repeats another peer's fingerprint",
                    peer.id
                ));
            }
        }
        // A placement may name a device that is paired rather than listed
        // here, so only its form is checked.
        let mut placed = HashSet::new();
        for p in &self.layout {
            if !is_valid_device_id(&p.device) {
                return invalid(format!("layout places invalid device id {:?}", p.device));
            }
            if !placed.insert(p.device.as_str()) {
                return invalid(format!("layout places {:?} more than once", p.device));
            }
        }
        Ok(())
    }

    pub fn peer(&self, id: &str) -> Option<&PeerConfig> {
        self.peers.iter().find(|p| p.id == id)
    }

    pub fn peer_by_fingerprint(&self, fp: &Fingerprint) -> Option<&PeerConfig> {
        self.peers.iter().find(|p| p.fingerprint == *fp)
    }
}

impl std::str::FromStr for Config {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let config: Config = toml::from_str(s)?;
        config.validate()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "ba:78:16:bf:8f:01:cf:ea:41:41:40:de:5d:ae:22:23:\
                      b0:03:61:a3:96:17:7a:9c:b4:10:ff:61:f2:00:15:ad";

    fn sample() -> String {
        format!(
            r#"
            [device]
            id = "mac"
            name = "MacBook Pro"

            [backend]
            kind = "mock"
            displays = [{{ id = "builtin", x = 0, y = 0, width = 1512, height = 982, primary = true }}]

            [[peers]]
            id = "win"
            name = "Windows PC"
            address = "192.168.1.20:47820"
            fingerprint = "{FP}"

            [[layout]]
            device = "mac"
            x = 0
            y = 400

            [[layout]]
            device = "win"
            x = 1512
            y = 0
            "#
        )
    }

    #[test]
    fn parses_a_complete_file() {
        let config: Config = sample().parse().unwrap();
        assert_eq!(config.device.id, "mac");
        assert_eq!(config.network.listen.port(), DEFAULT_PORT);
        let peer = config.peer("win").unwrap();
        assert_eq!(peer.fingerprint.to_string(), FP);
        assert!(!peer.swap_gui_and_control);
        let BackendConfig::Mock { displays, .. } = &config.backend else {
            panic!("expected the mock backend");
        };
        assert_eq!(displays[0].scale, 1.0);
        assert_eq!(config.layout.len(), 2);
    }

    #[test]
    fn no_file_means_native_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load_or_default(&dir.path().join("config.toml")).unwrap();
        assert!(matches!(config.backend, BackendConfig::Native));
        assert!(config.peers.is_empty());
        let empty: Config = "".parse().unwrap();
        assert!(matches!(empty.backend, BackendConfig::Native));
    }

    #[test]
    fn device_and_network_sections_are_optional() {
        let mut config: Config = "[backend]\nkind = \"mock\"\n".parse().unwrap();
        assert!(config.network.discovery);
        assert!(config.network.allow_pairing);
        let fp: Fingerprint = FP.parse().unwrap();
        config.resolve_device(&fp, Some("Studio".to_owned()));
        assert_eq!(config.device.id, "ba7816bf8f01cfea");
        assert_eq!(config.device.name, "Studio");
        config.validate().unwrap();

        let mut unnamed: Config = "[backend]\nkind = \"mock\"\n".parse().unwrap();
        unnamed.resolve_device(&fp, Some("\u{7}".to_owned()));
        assert_eq!(unnamed.device.name, "ba7816bf8f01cfea");
    }

    #[test]
    fn paths_may_name_the_receive_folder_and_the_log() {
        let config: Config = "[paths]\nlog_file = \"/var/log/agent.log\"\n"
            .parse()
            .unwrap();
        assert_eq!(
            config.paths.log_file.as_deref(),
            Some(Path::new("/var/log/agent.log"))
        );
        let receive = "[paths]\nreceive_folder = \"/srv/inputweave/received\"\n".parse::<Config>();
        if cfg!(windows) {
            // The broker saves received files in the user's Downloads.
            assert!(matches!(receive, Err(ConfigError::Invalid(_))));
        } else {
            assert_eq!(
                receive.unwrap().paths.receive_folder.as_deref(),
                Some(Path::new("/srv/inputweave/received"))
            );
        }
        let none: Config = "".parse().unwrap();
        assert!(none.paths.receive_folder.is_none() && none.paths.log_file.is_none());
    }

    #[test]
    fn rejects_bad_references_and_typos() {
        let malformed = sample().replace("device = \"win\"", "device = \"the pc\"");
        assert!(matches!(
            malformed.parse::<Config>(),
            Err(ConfigError::Invalid(_))
        ));
        let twice = sample().replace("device = \"win\"", "device = \"mac\"");
        assert!(matches!(
            twice.parse::<Config>(),
            Err(ConfigError::Invalid(_))
        ));
        let duplicate = sample().replace("id = \"win\"", "id = \"mac\"");
        assert!(matches!(
            duplicate.parse::<Config>(),
            Err(ConfigError::Invalid(_))
        ));
        let typo = sample().replace("address =", "adress =");
        assert!(matches!(typo.parse::<Config>(), Err(ConfigError::Parse(_))));
        let bad_id = sample().replace("id = \"win\"", "id = \"the pc\"");
        assert!(matches!(
            bad_id.parse::<Config>(),
            Err(ConfigError::Invalid(_))
        ));
        let bad_fp = sample().replace("ba:78", "zz:78");
        assert!(matches!(
            bad_fp.parse::<Config>(),
            Err(ConfigError::Parse(_))
        ));
    }
}
