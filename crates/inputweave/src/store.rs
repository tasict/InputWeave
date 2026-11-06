//! The devices file (`devices.toml`): paired devices, the arrangement the
//! group shares, and the displays each device last reported. Unlike the
//! configuration file, the agent writes it. Previews of other devices'
//! desktop pictures are kept beside it, in `previews/` (see
//! `agent::previews`).
//!
//! Names this device's user gave other devices are in `[aliases]`, how
//! fast the cursor moves on them (in percent, `iw_core::speed`) in
//! `[cursor_speed]`, and the clipboard or files turned off with particular
//! devices in `[[device_sharing]]`, by device id. They apply to paired
//! devices and to those listed in the configuration file (`[[peers]]`)
//! alike, stay when a device is paired again (moving with it when it comes
//! back under another id), and stay on this device: other devices are not
//! told. Unpairing a device drops its entries, and another device paired
//! under its id starts without them; a device dropped from the
//! configuration file keeps them until the file is edited.
//!
//! ```toml
//! layout_version = 7
//! layout_author = "mac"
//! login_window_access = true
//! edge_dwell_ms = 150
//! switch_modifier = "shift"
//! relative_auto_off = true
//!
//! [hotkeys]
//! take_back = "control+alt+shift+Escape"
//! lock = "control+alt+KeyL"
//! jump_modifiers = ""
//! relative = "control+alt+shift+KeyR"
//!
//! [folder_access]
//! downloads = "granted"
//! desktop = "granted"
//! documents = "denied"
//!
//! [[devices]]
//! id = "win"
//! name = "Windows PC"
//! fingerprint = "aa:bb:…"
//! os = "windows"
//! last_address = "192.168.1.20:47820"
//! swap_gui_and_control = true
//! side = "right"
//!
//! [[layout]]
//! device = "win"
//! x = 1512.0
//! y = 0.0
//!
//! [[known_displays]]
//! device = "win"
//! displays = [{ id = "main", x = 0.0, y = 0.0, width = 2560.0, height = 1440.0, scale = 1.0, primary = true }]
//!
//! [aliases]
//! win = "Study PC"
//!
//! [cursor_speed]
//! win = 150
//!
//! [[device_sharing]]
//! id = "win"
//! clipboard_off = true
//! files_off = false
//! ```

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::Path;

use iw_core::speed::CursorSpeed;
use iw_net::Fingerprint;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracing::warn;

use crate::agent::{Access, UserFolder};
use crate::config::{ConfigError, DisplayConfig, PlacementConfig};

const HEADER: &str = "# Paired devices and the arrangement, written by the InputWeave agent.\n\
                      # Edit only while the agent is stopped.\n\n";

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DevicesFile {
    /// This device's name as set from the UI; overrides `[device] name`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device_name: String,
    /// Clipboard sync was turned off from the UI.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clipboard_off: bool,
    /// Sharing previews of this device's desktop pictures was turned off
    /// from the UI.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub preview_sharing_off: bool,
    /// Other devices may control this Mac at the login window, as turned on
    /// from the UI: the login window's agent then runs as this device
    /// (`login_window`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub login_window_access: bool,
    /// How long the cursor pushes against an edge before it crosses, in
    /// milliseconds, as set from the UI.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub edge_dwell_ms: u32,
    /// The cursor crosses edges only while this modifier is held, as set
    /// from the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_modifier: Option<SwitchModifier>,
    /// Devices do not enter relative mode by themselves when an app there
    /// holds the pointer, as turned off from the UI.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub relative_auto_off: bool,
    /// Where received files go, as chosen in the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receive_folder: Option<std::path::PathBuf>,
    /// Ask before receiving each file, rather than accepting files from
    /// paired devices.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ask_before_receiving: bool,
    /// Version of the arrangement in `layout`; see `LayoutDoc`.
    #[serde(default)]
    pub layout_version: u64,
    /// The device that made that version.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub layout_author: String,
    /// The hotkeys as set from the UI, if they are not the defaults. A
    /// table, so it goes after the plain values and before the arrays of
    /// tables, as TOML needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkeys: Option<HotkeysFile>,
    /// What macOS last said of the folders it protects, when the app had
    /// the agent list them (`agent::folder_access`). A table, like
    /// `hotkeys`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_access: Option<FolderAccessFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<PairedDevice>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layout: Vec<PlacementConfig>,
    /// The displays other devices last reported, so the arrangement can
    /// show them while they are offline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub known_displays: Vec<KnownDisplays>,
    /// Names this device's user gave other devices, by device id, shown
    /// in place of the names the devices give themselves.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub aliases: BTreeMap<String, String>,
    /// How fast the cursor moves on other devices, by device id, written in
    /// percent of how the mouse moves it here, as set from the UI. A device
    /// at the normal speed (100) has no entry; one out of range, as edited
    /// by hand, is dropped as the file is read.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "speeds_out",
        deserialize_with = "speeds_in"
    )]
    pub cursor_speed: BTreeMap<String, CursorSpeed>,
    /// Devices this one does not share its clipboard or files with. A
    /// device with both on has no entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub device_sharing: Vec<DeviceSharing>,
}

/// What this device does not share with another one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSharing {
    pub id: String,
    /// Neither sends its clipboard there nor takes that device's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clipboard_off: bool,
    /// Neither sends files there nor takes that device's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub files_off: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnownDisplays {
    pub device: String,
    pub displays: Vec<DisplayConfig>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    #[serde(
        serialize_with = "fingerprint_out",
        deserialize_with = "fingerprint_in"
    )]
    pub fingerprint: Fingerprint,
    /// "macos", "windows", "linux", or empty if unknown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub os: String,
    /// Where the device was last reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_address: Option<SocketAddr>,
    #[serde(default)]
    pub swap_gui_and_control: bool,
    /// Which side of this device it goes until it has a placement.
    #[serde(default)]
    pub side: Side,
}

/// The hotkeys, written as `iw_core::hotkey` reads them. One left out is
/// the default (`relative` too, unless that clashes with the others: then
/// it is off); `jump_modifiers = ""` turns the jumps off, `relative = ""`
/// that hotkey.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HotkeysFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take_back: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump_modifiers: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative: Option<String>,
}

/// What macOS said of each folder it protects (`[folder_access]`); a
/// folder never asked about has no entry.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FolderAccessFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloads: Option<Access>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop: Option<Access>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documents: Option<Access>,
}

impl FolderAccessFile {
    pub fn get(&self, folder: UserFolder) -> Option<Access> {
        *self.entry(folder)
    }

    pub fn set(&mut self, folder: UserFolder, access: Access) {
        *self.entry_mut(folder) = Some(access);
    }

    fn entry(&self, folder: UserFolder) -> &Option<Access> {
        match folder {
            UserFolder::Downloads => &self.downloads,
            UserFolder::Desktop => &self.desktop,
            UserFolder::Documents => &self.documents,
        }
    }

    fn entry_mut(&mut self, folder: UserFolder) -> &mut Option<Access> {
        match folder {
            UserFolder::Downloads => &mut self.downloads,
            UserFolder::Desktop => &mut self.desktop,
            UserFolder::Documents => &mut self.documents,
        }
    }
}

impl HotkeysFile {
    /// Every hotkey of `hotkeys`, written down.
    pub fn of(hotkeys: &iw_core::hotkey::Hotkeys) -> Self {
        Self {
            take_back: Some(hotkeys.take_back.to_string()),
            lock: Some(hotkeys.lock.to_string()),
            jump_modifiers: Some(hotkeys.jump.map(|m| m.to_string()).unwrap_or_default()),
            relative: Some(hotkeys.relative.map(|h| h.to_string()).unwrap_or_default()),
        }
    }
}

/// A modifier the cursor needs held to cross an edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SwitchModifier {
    Control,
    Alt,
    Shift,
    Gui,
}

impl From<SwitchModifier> for iw_core::hotkey::Modifier {
    fn from(modifier: SwitchModifier) -> Self {
        match modifier {
            SwitchModifier::Control => Self::Control,
            SwitchModifier::Alt => Self::Alt,
            SwitchModifier::Shift => Self::Shift,
            SwitchModifier::Gui => Self::Gui,
        }
    }
}

impl From<iw_core::hotkey::Modifier> for SwitchModifier {
    fn from(modifier: iw_core::hotkey::Modifier) -> Self {
        use iw_core::hotkey::Modifier;
        match modifier {
            Modifier::Control => Self::Control,
            Modifier::Alt => Self::Alt,
            Modifier::Shift => Self::Shift,
            Modifier::Gui => Self::Gui,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    #[default]
    Right,
    Left,
}

impl From<Side> for iw_core::layout::Side {
    fn from(side: Side) -> Self {
        match side {
            Side::Right => Self::Right,
            Side::Left => Self::Left,
        }
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn speeds_out<S: Serializer>(
    speeds: &BTreeMap<String, CursorSpeed>,
    s: S,
) -> Result<S::Ok, S::Error> {
    s.collect_map(speeds.iter().map(|(id, speed)| (id, speed.percent())))
}

/// Each entry on its own: one that is not a speed, as edited by hand (out of
/// range, negative, a fraction, text), is dropped with a warning, and the
/// rest of the file is read as usual.
fn speeds_in<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, CursorSpeed>, D::Error> {
    // Not a table at all: dropped as a whole, the rest of the file read.
    let entries = match toml::Value::deserialize(d)? {
        toml::Value::Table(entries) => entries,
        other => {
            warn!(value = %other, "dropping cursor speeds that are not a table");
            return Ok(BTreeMap::new());
        }
    };
    Ok(entries
        .into_iter()
        .filter_map(|(id, value)| {
            let speed = value
                .as_integer()
                .and_then(|n| u32::try_from(n).ok())
                .and_then(CursorSpeed::from_percent);
            if speed.is_none() {
                warn!(%id, %value, "dropping a cursor speed that is not one");
            }
            Some((id, speed?))
        })
        .collect())
}

fn fingerprint_out<S: Serializer>(fp: &Fingerprint, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(fp)
}

fn fingerprint_in<'de, D: Deserializer<'de>>(d: D) -> Result<Fingerprint, D::Error> {
    String::deserialize(d)?
        .parse()
        .map_err(serde::de::Error::custom)
}

impl DevicesFile {
    /// Reads the file. A missing file is an empty one.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// The file as `text` holds it, read some other way.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        Ok(toml::from_str(text)?)
    }

    /// Replaces the file in one step, readable only by its owner. It holds
    /// the list of trusted devices, so a crash must not leave half of it.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string(self).map_err(io::Error::other)?;
        write_private(path, HEADER, &text)
    }
}

/// Replaces the file at `path` with `header` and `text` in one step,
/// readable only by its owner (on Windows the folder's permissions keep it
/// to the agent), so a crash never leaves half of it.
pub(crate) fn write_private(path: &Path, header: &str, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(header.as_bytes())?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DevicesFile {
        DevicesFile {
            device_name: "Studio".to_owned(),
            clipboard_off: true,
            preview_sharing_off: true,
            login_window_access: true,
            edge_dwell_ms: 150,
            switch_modifier: Some(SwitchModifier::Shift),
            relative_auto_off: true,
            hotkeys: Some(HotkeysFile {
                take_back: Some("control+alt+shift+Escape".to_owned()),
                lock: Some("control+alt+KeyL".to_owned()),
                jump_modifiers: Some(String::new()),
                relative: Some("control+alt+shift+KeyR".to_owned()),
            }),
            folder_access: Some(FolderAccessFile {
                downloads: Some(Access::Granted),
                desktop: Some(Access::Granted),
                documents: Some(Access::Denied),
            }),
            receive_folder: Some("/Users/me/Downloads/InputWeave".into()),
            ask_before_receiving: true,
            layout_version: 3,
            layout_author: "mac".to_owned(),
            known_displays: vec![KnownDisplays {
                device: "win".to_owned(),
                displays: vec![DisplayConfig {
                    id: "main".to_owned(),
                    x: 0.0,
                    y: 0.0,
                    width: 2560.0,
                    height: 1440.0,
                    scale: 1.5,
                    primary: true,
                }],
            }],
            devices: vec![PairedDevice {
                id: "win".to_owned(),
                name: "Windows 桌機".to_owned(),
                fingerprint: Fingerprint::of_certificate(b"abc"),
                os: "windows".to_owned(),
                last_address: Some("192.168.1.20:47820".parse().unwrap()),
                swap_gui_and_control: true,
                side: Side::Left,
            }],
            layout: vec![PlacementConfig {
                device: "win".to_owned(),
                x: -2560.0,
                y: 0.0,
            }],
            aliases: [("win".to_owned(), "書房的 PC".to_owned())].into(),
            cursor_speed: [("win".to_owned(), CursorSpeed::from_percent(150).unwrap())].into(),
            device_sharing: vec![DeviceSharing {
                id: "win".to_owned(),
                clipboard_off: true,
                files_off: false,
            }],
        }
    }

    #[test]
    fn saves_and_loads_the_same_devices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("devices.toml");
        assert_eq!(DevicesFile::load(&path).unwrap(), DevicesFile::default());

        sample().save(&path).unwrap();
        assert_eq!(DevicesFile::load(&path).unwrap(), sample());
        assert!(!path.with_extension("toml.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn optional_fields_have_defaults() {
        let text = format!(
            "[[devices]]\nid = \"win\"\nname = \"PC\"\nfingerprint = \"{}\"\n",
            Fingerprint::of_certificate(b"abc")
        );
        let file: DevicesFile = toml::from_str(&text).unwrap();
        let d = &file.devices[0];
        assert_eq!(
            (d.os.as_str(), d.last_address, d.side),
            ("", None, Side::Right)
        );
        assert!(!d.swap_gui_and_control);
        assert!(file.aliases.is_empty() && file.device_sharing.is_empty());
        assert!(file.cursor_speed.is_empty());
        assert!(toml::from_str::<DevicesFile>("[[devices]]\nid = \"x\"\n").is_err());

        let text =
            "[aliases]\nwin = \"PC\"\n\n[[device_sharing]]\nid = \"win\"\nfiles_off = true\n";
        let file: DevicesFile = toml::from_str(text).unwrap();
        assert_eq!(file.aliases["win"], "PC");
        assert_eq!(
            file.device_sharing,
            vec![DeviceSharing {
                id: "win".to_owned(),
                clipboard_off: false,
                files_off: true,
            }]
        );
    }

    #[test]
    fn cursor_speeds_are_a_table_by_device_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.toml");
        let mut file = sample();
        file.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[cursor_speed]\nwin = 150\n"), "{text}");
        file.cursor_speed.clear();
        file.save(&path).unwrap();
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("cursor_speed")
        );
        assert_eq!(DevicesFile::load(&path).unwrap(), file);
        // One edited out of range, or into something not a number of
        // percent, is dropped as the file is read; the rest stays.
        let file: DevicesFile = toml::from_str(
            "[cursor_speed]\nwin = 200\nmini = 50\nnas = 155\npc = -10\nmac = 150.0\nlaptop = \"fast\"\n",
        )
        .unwrap();
        let percents: Vec<(&str, u32)> = file
            .cursor_speed
            .iter()
            .map(|(id, speed)| (id.as_str(), speed.percent()))
            .collect();
        assert_eq!(percents, [("mini", 50), ("win", 200)]);
        // Not a table: only it goes.
        let file: DevicesFile =
            toml::from_str("cursor_speed = 150\n\n[aliases]\nwin = \"PC\"\n").unwrap();
        assert!(file.cursor_speed.is_empty());
        assert_eq!(file.aliases["win"], "PC");
    }
}
