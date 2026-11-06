//! Everything the agent knows about other devices: which ones it trusts
//! (listed in the configuration file, or paired and kept in the devices
//! file), which ones are nearby (mDNS), where devices sit in the shared
//! arrangement, and the displays they last reported.
//!
//! Pairing, discovery and the engine change it; the dialer, the accept
//! loop, the engine and IPC clients read it or watch it for changes.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use iw_core::geometry::Point;
use iw_core::hotkey::{Hotkeys, Modifier};
use iw_core::layout::Display;
use iw_core::speed::CursorSpeed;
use iw_net::Fingerprint;
use iw_net::discovery::{self, Found};
use iw_proto::peer::v1::Os;
use tokio::sync::watch;
use tracing::warn;

use super::folder_access::{Access, UserFolder};
use crate::config::{Config, PlacementConfig};
use crate::store::{
    DeviceSharing, DevicesFile, FolderAccessFile, HotkeysFile, KnownDisplays, PairedDevice, Side,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Listed in the configuration file.
    Configured,
    /// Paired, and kept in the devices file.
    Paired,
}

/// A trusted device.
#[derive(Clone, Debug, PartialEq)]
pub struct Peer {
    pub id: String,
    /// The name shown for it here: the one this device's user gave it, or
    /// else `reported_name`.
    pub name: String,
    /// The name it gives itself, or the configuration file gives it.
    pub reported_name: String,
    pub fingerprint: Fingerprint,
    pub os: Os,
    pub origin: Origin,
    /// Fixed address from the configuration file.
    pub address: Option<SocketAddr>,
    /// Where a paired device was last reached.
    pub last_address: Option<SocketAddr>,
    pub swap_gui_and_control: bool,
    /// Which side of this device it goes until it has a placement.
    pub side: Side,
    /// This device neither gives its clipboard to it nor takes its own.
    pub clipboard_off: bool,
    /// This device neither sends it files nor takes its own.
    pub files_off: bool,
    /// How fast the cursor moves on it while this device controls it.
    pub cursor_speed: CursorSpeed,
}

/// The trusted devices as published: the list, and how many times it
/// changed (its generation), which IPC requests wait on.
#[derive(Clone, Debug, Default)]
pub(crate) struct TrustedDevices {
    pub(crate) list: Arc<Vec<Peer>>,
    pub(crate) generation: u64,
}

/// The name this device shows for the trusted device `id`, if it trusts it.
pub(crate) fn name_of(peers: &[Peer], id: &str) -> Option<String> {
    peers.iter().find(|p| p.id == id).map(|p| p.name.clone())
}

/// The arrangement the group shares: where each device's desktop origin
/// sits on the canvas. Each device keeps the newest version it has seen and
/// passes newer ones on, so connected devices converge on one document.
/// Versions are ordered by number, then by author, so two versions made
/// at the same time on different devices still have a winner; the loser's
/// changes are made again on top of it if they still matter.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LayoutDoc {
    pub(crate) version: u64,
    pub(crate) author: String,
    pub(crate) placements: Vec<PlacementConfig>,
}

impl LayoutDoc {
    pub(crate) fn newer_than(&self, other: &LayoutDoc) -> bool {
        (self.version, &self.author) > (other.version, &other.author)
    }
}

/// Why a change to a trusted device was refused.
#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("no paired device has id {0:?}")]
    NotPaired(String),
    #[error("{0:?} is listed in the configuration file; change it there")]
    Configured(String),
    #[error("no trusted device has id {0:?}")]
    Unknown(String),
    #[error("a device name is 1 to 64 characters without control characters")]
    InvalidName,
    #[error(
        "a cursor speed is {} to {} percent, in steps of {}",
        CursorSpeed::MIN_PERCENT,
        CursorSpeed::MAX_PERCENT,
        CursorSpeed::STEP_PERCENT
    )]
    InvalidSpeed,
}

impl DeviceError {
    /// The error code clients are told.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotPaired(_) => "not_paired",
            Self::Configured(_) => "configured_device",
            Self::Unknown(_) => "unknown_device",
            Self::InvalidName => "invalid_name",
            Self::InvalidSpeed => "invalid_speed",
        }
    }
}

pub(crate) struct Directory {
    this_device: String,
    file: Option<PathBuf>,
    configured: Vec<Peer>,
    config_layout: Vec<PlacementConfig>,
    stored: Mutex<DevicesFile>,
    /// How many changes were made to `stored`, counted under its lock.
    changes: AtomicU64,
    /// The last of those changes saved to `file`. Held while writing, so
    /// the file is written by one caller at a time.
    written: Mutex<u64>,
    peers: watch::Sender<TrustedDevices>,
    nearby: watch::Sender<Arc<Vec<Found>>>,
    /// Saved hotkeys from before relative mode left its hotkey off; said
    /// once.
    relative_left_off_told: AtomicBool,
}

impl Directory {
    /// `stored` is what the devices file at `file` holds; with no file,
    /// changes are kept in memory only.
    pub(crate) fn new(config: &Config, file: Option<PathBuf>, stored: DevicesFile) -> Self {
        let configured = config
            .peers
            .iter()
            .map(|p| Peer {
                id: p.id.clone(),
                name: p.name.clone(),
                reported_name: p.name.clone(),
                fingerprint: p.fingerprint,
                os: Os::Unspecified,
                origin: Origin::Configured,
                address: p.address,
                last_address: None,
                swap_gui_and_control: p.swap_gui_and_control,
                side: Side::Right,
                clipboard_off: false,
                files_off: false,
                cursor_speed: CursorSpeed::NORMAL,
            })
            .collect();
        let directory = Self {
            this_device: config.device.id.clone(),
            file,
            configured,
            config_layout: config.layout.clone(),
            stored: Mutex::new(stored),
            changes: AtomicU64::new(0),
            written: Mutex::new(0),
            peers: watch::Sender::new(TrustedDevices::default()),
            nearby: watch::Sender::new(Arc::new(Vec::new())),
            relative_left_off_told: AtomicBool::new(false),
        };
        directory.publish(&directory.stored.lock().unwrap());
        directory
    }

    pub(crate) fn peers(&self) -> Arc<Vec<Peer>> {
        self.peers.borrow().list.clone()
    }

    pub(crate) fn watch_peers(&self) -> watch::Receiver<TrustedDevices> {
        self.peers.subscribe()
    }

    /// The trusted devices and their generation: a change made after one of
    /// the setters returned its generation has that generation, or a later
    /// one.
    pub(crate) fn peers_now(&self) -> (Arc<Vec<Peer>>, u64) {
        let now = self.peers.borrow();
        (now.list.clone(), now.generation)
    }

    fn generation(&self) -> u64 {
        self.peers.borrow().generation
    }

    pub(crate) fn peer(&self, id: &str) -> Option<Peer> {
        self.peers
            .borrow()
            .list
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    pub(crate) fn peer_by_fingerprint(&self, fp: &Fingerprint) -> Option<Peer> {
        self.peers
            .borrow()
            .list
            .iter()
            .find(|p| p.fingerprint == *fp)
            .cloned()
    }

    /// Whether `id` is trusted. Called under `stored`'s lock, the devices
    /// published are those `stored` holds: every change publishes them
    /// before the lock is let go.
    fn is_trusted(&self, id: &str) -> bool {
        self.peers.borrow().list.iter().any(|p| p.id == id)
    }

    /// Devices seen on the network, sorted by name.
    pub(crate) fn nearby(&self) -> Arc<Vec<Found>> {
        self.nearby.borrow().clone()
    }

    pub(crate) fn watch_nearby(&self) -> watch::Receiver<Arc<Vec<Found>>> {
        self.nearby.subscribe()
    }

    pub(crate) fn nearby_by_id(&self, id: &str) -> Option<Found> {
        self.nearby
            .borrow()
            .iter()
            .find(|f| f.device_id == id)
            .cloned()
    }

    pub(crate) fn found(&self, found: Found) {
        self.nearby.send_if_modified(|nearby| {
            let mut list: Vec<Found> = nearby.as_ref().clone();
            match list.iter_mut().find(|f| f.fingerprint == found.fingerprint) {
                Some(existing) if *existing == found => return false,
                Some(existing) => *existing = found,
                None => list.push(found),
            }
            list.sort_by(|a, b| (&a.name, &a.device_id).cmp(&(&b.name, &b.device_id)));
            *nearby = Arc::new(list);
            true
        });
    }

    pub(crate) fn lost(&self, fp: &Fingerprint) {
        self.nearby.send_if_modified(|nearby| {
            if !nearby.iter().any(|f| f.fingerprint == *fp) {
                return false;
            }
            *nearby = Arc::new(
                nearby
                    .iter()
                    .filter(|f| f.fingerprint != *fp)
                    .cloned()
                    .collect(),
            );
            true
        });
    }

    /// Whether a device presenting `fp` may pair under `id`: the id must not
    /// be this device's or belong to another device. Pairing again with an
    /// already paired device is allowed.
    pub(crate) fn id_conflicts(&self, id: &str, fp: &Fingerprint) -> bool {
        id == self.this_device
            || self
                .peers
                .borrow()
                .list
                .iter()
                .any(|p| p.id == id && p.fingerprint != *fp)
    }

    /// Remembers a newly paired device, replacing an earlier pairing with
    /// the same device. A device already listed in the configuration file
    /// stays as configured there.
    pub(crate) fn add_paired(&self, device: PairedDevice) {
        if self
            .configured
            .iter()
            .any(|p| p.fingerprint == device.fingerprint)
        {
            return;
        }
        self.update(|stored| {
            // What was chosen here under this id is this device's only if it
            // was paired under it before; anything else (another device
            // paired under it, or one since removed from the configuration
            // file) does not pass to it.
            if !stored
                .devices
                .iter()
                .any(|d| d.id == device.id && d.fingerprint == device.fingerprint)
            {
                forget_choices(stored, &device.id);
            }
            // This device paired before under another id: the name, cursor
            // speed and clipboard and file choices given here go with it to
            // the new id, and nothing stays under the old one. Its place in
            // the arrangement and its last displays stay under the old id,
            // as before.
            let earlier: Vec<String> = stored
                .devices
                .iter()
                .filter(|d| d.fingerprint == device.fingerprint && d.id != device.id)
                .map(|d| d.id.clone())
                .collect();
            for id in &earlier {
                move_choices(stored, id, &device.id);
            }
            stored
                .devices
                .retain(|d| d.id != device.id && d.fingerprint != device.fingerprint);
            stored.devices.push(device);
            true
        });
    }

    pub(crate) fn remove_paired(&self, id: &str) -> Result<(), DeviceError> {
        if self.configured.iter().any(|p| p.id == id) {
            return Err(DeviceError::Configured(id.to_owned()));
        }
        let mut removed = false;
        self.update(|stored| {
            let before = stored.devices.len();
            stored.devices.retain(|d| d.id != id);
            removed = stored.devices.len() != before;
            stored.layout.retain(|p| p.device != id);
            stored.known_displays.retain(|k| k.device != id);
            forget_choices(stored, id);
            removed
        });
        if removed {
            Ok(())
        } else {
            Err(DeviceError::NotPaired(id.to_owned()))
        }
    }

    /// Turns swapping ⌘ and Ctrl for keys sent to a paired device on or off.
    /// Returns the generation of the trusted devices (`peers_now`) that has
    /// it.
    pub(crate) fn set_swap(&self, id: &str, swap: bool) -> Result<u64, DeviceError> {
        if self.configured.iter().any(|p| p.id == id) {
            return Err(DeviceError::Configured(id.to_owned()));
        }
        let mut found = false;
        self.update(
            |stored| match stored.devices.iter_mut().find(|d| d.id == id) {
                Some(d) => {
                    found = true;
                    let changed = d.swap_gui_and_control != swap;
                    d.swap_gui_and_control = swap;
                    changed
                }
                None => false,
            },
        );
        if found {
            Ok(self.generation())
        } else {
            Err(DeviceError::NotPaired(id.to_owned()))
        }
    }

    /// Makes the cursor move faster or slower on a trusted device than the
    /// mouse moves it here, `percent` of that: 50 to 300, in steps of 10.
    /// The normal speed (100) also clears a speed kept for a device no
    /// longer trusted. Returns the generation of the trusted devices
    /// (`peers_now`) that has it.
    pub(crate) fn set_speed(&self, id: &str, percent: u32) -> Result<u64, DeviceError> {
        let speed = CursorSpeed::from_percent(percent).ok_or(DeviceError::InvalidSpeed)?;
        let mut trusted = false;
        let mut cleared = false;
        // Trusted as of the change itself, under the lock: unpaired just
        // before, it leaves no speed behind.
        self.update(|stored| {
            trusted = self.is_trusted(id);
            if speed == CursorSpeed::NORMAL {
                // A speed left behind goes, whoever it was for.
                cleared = stored.cursor_speed.remove(id).is_some();
                cleared
            } else if trusted {
                stored.cursor_speed.insert(id.to_owned(), speed) != Some(speed)
            } else {
                false
            }
        });
        if !trusted && !cleared {
            return Err(DeviceError::Unknown(id.to_owned()));
        }
        Ok(self.generation())
    }

    /// Gives a trusted device a name shown here in place of its own, or
    /// with an empty `name` takes it away. Other devices are not told.
    pub(crate) fn set_alias(&self, id: &str, name: &str) -> Result<u64, DeviceError> {
        let name = name.trim();
        if !name.is_empty() && !iw_net::is_valid_device_name(name) {
            return Err(DeviceError::InvalidName);
        }
        let mut trusted = false;
        // Trusted as of the change itself, under the lock: unpaired just
        // before, it leaves no name behind.
        self.update(|stored| {
            trusted = self.is_trusted(id);
            if !trusted {
                false
            } else if name.is_empty() {
                stored.aliases.remove(id).is_some()
            } else {
                stored
                    .aliases
                    .insert(id.to_owned(), name.to_owned())
                    .as_deref()
                    != Some(name)
            }
        });
        if !trusted {
            return Err(DeviceError::Unknown(id.to_owned()));
        }
        Ok(self.generation())
    }

    /// Turns sharing the clipboard or files with a trusted device off or
    /// on again. Returns the generation of the trusted devices that has it.
    pub(crate) fn set_sharing(
        &self,
        id: &str,
        clipboard_off: bool,
        files_off: bool,
    ) -> Result<u64, DeviceError> {
        let mut trusted = false;
        // Trusted as of the change itself, under the lock: unpaired just
        // before, it leaves no entry behind.
        self.update(|stored| {
            trusted = self.is_trusted(id);
            if !trusted {
                return false;
            }
            let before = stored.device_sharing.clone();
            stored.device_sharing.retain(|s| s.id != id);
            if clipboard_off || files_off {
                stored.device_sharing.push(DeviceSharing {
                    id: id.to_owned(),
                    clipboard_off,
                    files_off,
                });
                stored.device_sharing.sort_by(|a, b| a.id.cmp(&b.id));
            }
            stored.device_sharing != before
        });
        if !trusted {
            return Err(DeviceError::Unknown(id.to_owned()));
        }
        Ok(self.generation())
    }

    /// A paired device introduced itself under a new name.
    pub(crate) fn renamed(&self, id: &str, name: &str) {
        self.update(
            |stored| match stored.devices.iter_mut().find(|d| d.id == id) {
                Some(d) if d.name != name => {
                    d.name = name.to_owned();
                    true
                }
                _ => false,
            },
        );
    }

    pub(crate) fn clipboard_sync(&self) -> bool {
        !self.stored.lock().unwrap().clipboard_off
    }

    pub(crate) fn set_clipboard_sync(&self, on: bool) {
        self.update(|stored| {
            let changed = stored.clipboard_off == on;
            stored.clipboard_off = !on;
            changed
        });
    }

    pub(crate) fn preview_sharing(&self) -> bool {
        !self.stored.lock().unwrap().preview_sharing_off
    }

    pub(crate) fn set_preview_sharing(&self, on: bool) {
        self.update(|stored| {
            let changed = stored.preview_sharing_off == on;
            stored.preview_sharing_off = !on;
            changed
        });
    }

    /// Whether devices enter relative mode by themselves when an app there
    /// holds the pointer.
    pub(crate) fn relative_auto(&self) -> bool {
        !self.stored.lock().unwrap().relative_auto_off
    }

    pub(crate) fn set_relative_auto(&self, on: bool) {
        self.update(|stored| {
            let changed = stored.relative_auto_off == on;
            stored.relative_auto_off = !on;
            changed
        });
    }

    /// What macOS last said of the folders it protects.
    pub(crate) fn folder_access(&self) -> FolderAccessFile {
        self.stored
            .lock()
            .unwrap()
            .folder_access
            .clone()
            .unwrap_or_default()
    }

    pub(crate) fn set_folder_access(&self, folder: UserFolder, access: Access) {
        self.update(|stored| {
            let kept = stored.folder_access.get_or_insert_with(Default::default);
            let changed = kept.get(folder) != Some(access);
            kept.set(folder, access);
            changed
        });
    }

    /// Whether other devices may control this Mac at the login window.
    pub(crate) fn login_window_access(&self) -> bool {
        self.stored.lock().unwrap().login_window_access
    }

    pub(crate) fn set_login_window_access(&self, on: bool) {
        self.update(|stored| {
            let changed = stored.login_window_access != on;
            stored.login_window_access = on;
            changed
        });
    }

    pub(crate) fn edge_dwell_ms(&self) -> u32 {
        self.stored.lock().unwrap().edge_dwell_ms
    }

    pub(crate) fn set_edge_dwell_ms(&self, ms: u32) {
        self.update(|stored| {
            let changed = stored.edge_dwell_ms != ms;
            stored.edge_dwell_ms = ms;
            changed
        });
    }

    /// The modifier the cursor needs held to cross an edge, if any.
    pub(crate) fn switch_modifier(&self) -> Option<Modifier> {
        self.stored.lock().unwrap().switch_modifier.map(Into::into)
    }

    pub(crate) fn set_switch_modifier(&self, modifier: Option<Modifier>) {
        let modifier = modifier.map(Into::into);
        self.update(|stored| {
            let changed = stored.switch_modifier != modifier;
            stored.switch_modifier = modifier;
            changed
        });
    }

    /// The hotkeys the user set, or the defaults. Ones the file holds that
    /// cannot be used, as after editing it by hand, are the defaults. A
    /// file from before relative mode has no hotkey for it: the default,
    /// unless that is the same keys as a hotkey the user set, which keeps
    /// them all and leaves relative mode's off.
    pub(crate) fn hotkeys(&self) -> Hotkeys {
        let Some(file) = self.stored.lock().unwrap().hotkeys.clone() else {
            return Hotkeys::default();
        };
        let defaults = HotkeysFile::of(&Hotkeys::default());
        let or_default =
            |set: Option<String>, default: Option<String>| set.or(default).unwrap_or_default();
        let unwritten = file.relative.is_none();
        let hotkeys = Hotkeys::parse(
            &or_default(file.take_back, defaults.take_back),
            &or_default(file.lock, defaults.lock),
            &or_default(file.jump_modifiers, defaults.jump_modifiers),
            file.relative.as_deref(),
        )
        .unwrap_or_else(|e| {
            warn!(error = %e, "the hotkeys in the devices file cannot be used; using the defaults");
            Hotkeys::default()
        });
        if unwritten
            && hotkeys.relative.is_none()
            && !self.relative_left_off_told.swap(true, Ordering::Relaxed)
        {
            warn!(
                default = %Hotkeys::DEFAULT_RELATIVE,
                "the relative mode hotkey would be the same keys as one set before; it is off until set"
            );
        }
        hotkeys
    }

    /// Remembers the hotkeys; the defaults leave nothing in the file.
    pub(crate) fn set_hotkeys(&self, hotkeys: &Hotkeys) {
        let file = (*hotkeys != Hotkeys::default()).then(|| HotkeysFile::of(hotkeys));
        self.update(|stored| {
            let changed = stored.hotkeys != file;
            stored.hotkeys = file;
            changed
        });
    }

    /// The folder and policy for received files the user chose, if any.
    pub(crate) fn file_settings(&self) -> (Option<PathBuf>, bool) {
        let stored = self.stored.lock().unwrap();
        (stored.receive_folder.clone(), stored.ask_before_receiving)
    }

    pub(crate) fn set_file_settings(&self, folder: Option<PathBuf>, ask: bool) {
        self.update(|stored| {
            let changed = stored.receive_folder != folder || stored.ask_before_receiving != ask;
            stored.receive_folder = folder;
            stored.ask_before_receiving = ask;
            changed
        });
    }

    pub(crate) fn set_device_name(&self, name: &str) {
        self.update(|stored| {
            let changed = stored.device_name != name;
            stored.device_name = name.to_owned();
            changed
        });
    }

    /// Records where a paired device was reached, so it can be found again
    /// without discovery.
    pub(crate) fn reached(&self, id: &str, addr: SocketAddr) {
        self.update(
            |stored| match stored.devices.iter_mut().find(|d| d.id == id) {
                Some(d) if d.last_address != Some(addr) => {
                    d.last_address = Some(addr);
                    true
                }
                _ => false,
            },
        );
    }

    /// Addresses to try for `peer`, best first: its configured address, where
    /// discovery sees it, then where it was last reached. Only addresses an
    /// endpoint bound to `local` can reach are included.
    pub(crate) fn addresses(&self, peer: &Peer, local: SocketAddr) -> Vec<SocketAddr> {
        let discovered = self
            .nearby
            .borrow()
            .iter()
            .find(|f| f.fingerprint == peer.fingerprint)
            .map(|f| f.addresses.clone())
            .unwrap_or_default();
        let mut out = Vec::new();
        for addr in peer
            .address
            .into_iter()
            .chain(discovered)
            .chain(peer.last_address)
        {
            if discovery::reachable_from(local, addr) && !out.contains(&addr) {
                out.push(addr);
            }
        }
        out
    }

    /// The shared arrangement as this device has it.
    pub(crate) fn layout(&self) -> LayoutDoc {
        let stored = self.stored.lock().unwrap();
        LayoutDoc {
            version: stored.layout_version,
            author: stored.layout_author.clone(),
            placements: stored.layout.clone(),
        }
    }

    /// Replaces the arrangement with `doc` if it is newer. Returns whether
    /// it was.
    pub(crate) fn adopt(&self, doc: LayoutDoc) -> bool {
        let mut adopted = false;
        self.update(|stored| {
            let current = LayoutDoc {
                version: stored.layout_version,
                author: stored.layout_author.clone(),
                placements: Vec::new(),
            };
            if !doc.newer_than(&current) {
                return false;
            }
            stored.layout_version = doc.version;
            stored.layout_author = doc.author;
            stored.layout = doc.placements;
            adopted = true;
            true
        });
        adopted
    }

    /// Moves devices on the canvas, making a new version of the arrangement
    /// authored by this device, and returns it.
    pub(crate) fn rearrange(&self, moves: &[(String, Point)]) -> LayoutDoc {
        let this_device = self.this_device.clone();
        self.update(|stored| {
            for (device, offset) in moves {
                let placement = PlacementConfig {
                    device: device.clone(),
                    x: offset.x,
                    y: offset.y,
                };
                match stored.layout.iter_mut().find(|p| p.device == *device) {
                    Some(p) => *p = placement,
                    None => stored.layout.push(placement),
                }
            }
            stored.layout_version += 1;
            stored.layout_author = this_device;
            true
        });
        self.layout()
    }

    /// The displays `device` last reported.
    pub(crate) fn known_displays(&self, device: &str) -> Option<Vec<Display>> {
        let stored = self.stored.lock().unwrap();
        stored
            .known_displays
            .iter()
            .find(|k| k.device == device)
            .map(|k| k.displays.iter().map(Into::into).collect())
    }

    /// Remembers the displays `device` reported, so the arrangement can
    /// still show it while it is offline.
    pub(crate) fn remember_displays(&self, device: &str, displays: &[Display]) {
        let displays: Vec<_> = displays.iter().map(Into::into).collect();
        self.update(|stored| {
            match stored
                .known_displays
                .iter_mut()
                .find(|k| k.device == device)
            {
                Some(k) if k.displays == displays => false,
                Some(k) => {
                    k.displays = displays;
                    true
                }
                None => {
                    stored.known_displays.push(KnownDisplays {
                        device: device.to_owned(),
                        displays,
                    });
                    true
                }
            }
        });
    }

    /// Placements from the configuration file, then those of the shared
    /// arrangement for devices it does not place.
    pub(crate) fn placements(&self) -> Vec<PlacementConfig> {
        let stored = self.stored.lock().unwrap();
        let mut out = self.config_layout.clone();
        for p in &stored.layout {
            if !out.iter().any(|c| c.device == p.device) {
                out.push(p.clone());
            }
        }
        out
    }

    /// Applies `change` to the stored devices; if it reports a change,
    /// publishes the new peer list at once, under the lock, and saves the
    /// file before returning.
    ///
    /// Published under the lock, the peer list never lags behind what is
    /// stored: the engine, still listing a device just unpaired, would put
    /// its placement back. Whoever hears of a change may so hear of it
    /// before it is in the file; whoever made it finds it there once this
    /// returns (pairing, for one, says it paired only then).
    ///
    /// Saving waits for the disk, which can take seconds when it is busy,
    /// so it happens outside the lock, which the engine and the sessions
    /// take, and off the async threads, where it would hold up the tasks
    /// waiting behind it there: every session's heartbeats among them,
    /// whose peers then give up on this device. Concurrent changes are
    /// still saved in the order they were made: each is numbered under the
    /// lock, and one that a later one's save already wrote is not written
    /// again.
    fn update(&self, change: impl FnOnce(&mut DevicesFile) -> bool) {
        let (path, saved, number) = {
            let mut stored = self.stored.lock().unwrap();
            if !change(&mut stored) {
                return;
            }
            self.publish(&stored);
            let Some(path) = &self.file else {
                return;
            };
            let number = self.changes.fetch_add(1, Ordering::Relaxed) + 1;
            (path, stored.clone(), number)
        };
        off_async_threads(|| {
            let mut written = self.written.lock().unwrap();
            if *written >= number {
                return;
            }
            // One that failed leaves the earlier changes waiting to try
            // their own.
            match saved.save(path) {
                Ok(()) => *written = number,
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "cannot save the devices file");
                }
            }
        });
    }

    fn publish(&self, stored: &DevicesFile) {
        let peers = self.compose(stored);
        self.peers.send_if_modified(|current| {
            if *current.list == peers {
                false
            } else {
                current.list = Arc::new(peers);
                current.generation += 1;
                true
            }
        });
    }

    /// Configured peers first, then paired devices the configuration does
    /// not already list by id or fingerprint; with the names and sharing
    /// this device's user chose for them.
    fn compose(&self, stored: &DevicesFile) -> Vec<Peer> {
        let mut peers = self.trusted(stored);
        for peer in &mut peers {
            if let Some(alias) = stored
                .aliases
                .get(&peer.id)
                .filter(|a| iw_net::is_valid_device_name(a))
            {
                peer.name = alias.clone();
            }
            if let Some(sharing) = stored.device_sharing.iter().find(|s| s.id == peer.id) {
                peer.clipboard_off = sharing.clipboard_off;
                peer.files_off = sharing.files_off;
            }
            if let Some(&speed) = stored.cursor_speed.get(&peer.id) {
                peer.cursor_speed = speed;
            }
        }
        peers
    }

    fn trusted(&self, stored: &DevicesFile) -> Vec<Peer> {
        let mut peers = self.configured.clone();
        let mut ids: HashSet<String> = peers.iter().map(|p| p.id.clone()).collect();
        for d in &stored.devices {
            let clash = d.id == self.this_device
                || ids.contains(&d.id)
                || peers.iter().any(|p| p.fingerprint == d.fingerprint);
            if clash || !iw_net::is_valid_device_id(&d.id) {
                warn!(id = %d.id, "ignoring a paired device that clashes with another");
                continue;
            }
            ids.insert(d.id.clone());
            peers.push(Peer {
                id: d.id.clone(),
                name: d.name.clone(),
                reported_name: d.name.clone(),
                fingerprint: d.fingerprint,
                os: os_from_name(&d.os),
                origin: Origin::Paired,
                address: None,
                last_address: d.last_address,
                swap_gui_and_control: d.swap_gui_and_control,
                side: d.side,
                clipboard_off: false,
                files_off: false,
                cursor_speed: CursorSpeed::NORMAL,
            });
        }
        peers
    }
}

/// Forgets the name, cursor speed and sharing choices given here for `id`.
fn forget_choices(stored: &mut DevicesFile, id: &str) {
    stored.aliases.remove(id);
    stored.cursor_speed.remove(id);
    stored.device_sharing.retain(|s| s.id != id);
}

/// Moves the name, cursor speed and sharing choices given here for `from`
/// to `to`, in place of any `to` had.
fn move_choices(stored: &mut DevicesFile, from: &str, to: &str) {
    if let Some(alias) = stored.aliases.remove(from) {
        stored.aliases.insert(to.to_owned(), alias);
    }
    if let Some(speed) = stored.cursor_speed.remove(from) {
        stored.cursor_speed.insert(to.to_owned(), speed);
    }
    if stored.device_sharing.iter().any(|s| s.id == from) {
        stored.device_sharing.retain(|s| s.id != to);
        for sharing in &mut stored.device_sharing {
            if sharing.id == from {
                sharing.id = to.to_owned();
            }
        }
        stored.device_sharing.sort_by(|a, b| a.id.cmp(&b.id));
    }
}

pub(crate) fn os_from_name(name: &str) -> Os {
    match name {
        "macos" => Os::Macos,
        "windows" => Os::Windows,
        "linux" => Os::Linux,
        _ => Os::Unspecified,
    }
}

/// Runs `f`, which blocks, on this thread; on an async thread of a
/// multi-threaded runtime, the tasks waiting there go on on another thread
/// meanwhile. A single-threaded runtime has nowhere to move them.
pub(crate) fn off_async_threads<R>(f: impl FnOnce() -> R) -> R {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(runtime) if runtime.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BackendConfig, DeviceConfig, NetworkConfig, PathsConfig, PeerConfig};

    fn fp(byte: u8) -> Fingerprint {
        Fingerprint::from_bytes([byte; 32])
    }

    fn config() -> Config {
        Config {
            device: DeviceConfig {
                id: "mac".to_owned(),
                name: "MacBook Pro".to_owned(),
            },
            network: NetworkConfig::default(),
            paths: PathsConfig::default(),
            backend: BackendConfig::Mock {
                displays: vec![],
                folders: None,
            },
            peers: vec![PeerConfig {
                id: "win".to_owned(),
                name: "Windows PC".to_owned(),
                address: Some("192.168.1.20:47820".parse().unwrap()),
                fingerprint: fp(1),
                swap_gui_and_control: true,
            }],
            layout: vec![PlacementConfig {
                device: "win".to_owned(),
                x: 1512.0,
                y: 0.0,
            }],
            clipboard: Default::default(),
        }
    }

    fn paired(id: &str, byte: u8) -> PairedDevice {
        PairedDevice {
            id: id.to_owned(),
            name: id.to_uppercase(),
            fingerprint: fp(byte),
            os: "linux".to_owned(),
            last_address: None,
            swap_gui_and_control: false,
            side: Side::Right,
        }
    }

    #[test]
    fn pairing_adds_and_unpairing_removes_and_both_persist() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("devices.toml");
        let directory = Directory::new(&config(), Some(file.clone()), DevicesFile::default());
        let mut watch = directory.watch_peers();
        watch.mark_unchanged();

        directory.add_paired(paired("mini", 2));
        assert!(watch.has_changed().unwrap());
        let mini = directory.peer_by_fingerprint(&fp(2)).unwrap();
        assert_eq!((mini.id.as_str(), mini.origin), ("mini", Origin::Paired));
        assert_eq!(mini.os, Os::Linux);
        let saved = DevicesFile::load(&file).unwrap();
        assert_eq!(saved.devices, vec![paired("mini", 2)]);

        directory.rearrange(&[("mini".to_owned(), Point::new(1.0, 2.0))]);
        assert!(matches!(
            directory.remove_paired("win"),
            Err(DeviceError::Configured(_))
        ));
        directory.set_swap("mini", true).unwrap();
        assert!(directory.peer("mini").unwrap().swap_gui_and_control);
        assert!(matches!(
            directory.set_swap("win", false),
            Err(DeviceError::Configured(_))
        ));
        directory.renamed("mini", "Studio");
        assert_eq!(directory.peer("mini").unwrap().name, "Studio");
        directory.remove_paired("mini").unwrap();
        assert!(matches!(
            directory.remove_paired("mini"),
            Err(DeviceError::NotPaired(_))
        ));
        assert!(matches!(
            directory.set_swap("mini", true),
            Err(DeviceError::NotPaired(_))
        ));
        assert!(directory.peer("mini").is_none());
        let saved = DevicesFile::load(&file).unwrap();
        assert!(saved.devices.is_empty() && saved.layout.is_empty());
    }

    #[test]
    fn changes_are_saved_in_order_on_any_runtime() {
        use tokio::runtime::Builder;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("devices.toml");
        let directory = Arc::new(Directory::new(
            &config(),
            Some(file.clone()),
            DevicesFile::default(),
        ));
        // Off the async threads of a multi-threaded runtime, which go on
        // meanwhile; as it is on a single-threaded one.
        for mut runtime in [Builder::new_multi_thread(), Builder::new_current_thread()] {
            let directory = directory.clone();
            runtime.build().unwrap().block_on(async move {
                tokio::spawn(async move { directory.add_paired(paired("mini", 2)) })
                    .await
                    .unwrap();
            });
        }
        assert_eq!(
            DevicesFile::load(&file).unwrap().devices,
            vec![paired("mini", 2)]
        );

        // Saved by many at once, the file ends with the last change.
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let directory = directory.clone();
                std::thread::spawn(move || {
                    for x in 0..20 {
                        directory.rearrange(&[(format!("d{i}"), Point::new(f64::from(x), 0.0))]);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let saved = DevicesFile::load(&file).unwrap();
        let stored = directory.stored.lock().unwrap();
        assert_eq!(saved.layout_version, 160);
        assert_eq!(
            (&saved.layout, saved.layout_version),
            (&stored.layout, stored.layout_version)
        );
        assert!(saved.layout.iter().all(|p| p.x == 19.0));
    }

    #[test]
    fn hotkeys_and_the_switch_modifier_persist_and_defaults_leave_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("devices.toml");
        let directory = Directory::new(&config(), Some(file.clone()), DevicesFile::default());
        assert_eq!(directory.hotkeys(), Hotkeys::default());
        assert_eq!(directory.switch_modifier(), None);

        let hotkeys = Hotkeys::parse(
            "control+alt+shift+Escape",
            "control+alt+KeyL",
            "",
            Some("control+alt+shift+KeyR"),
        )
        .unwrap();
        directory.set_hotkeys(&hotkeys);
        directory.set_switch_modifier(Some(Modifier::Gui));
        assert!(directory.relative_auto());
        directory.set_relative_auto(false);
        let saved = DevicesFile::load(&file).unwrap();
        assert_eq!(
            saved
                .hotkeys
                .as_ref()
                .and_then(|h| h.jump_modifiers.as_deref()),
            Some("")
        );
        assert!(saved.relative_auto_off);
        let reloaded = Directory::new(&config(), Some(file.clone()), saved);
        assert_eq!(reloaded.hotkeys(), hotkeys);
        assert_eq!(reloaded.switch_modifier(), Some(Modifier::Gui));
        assert!(!reloaded.relative_auto());
        directory.set_relative_auto(true);
        assert!(!DevicesFile::load(&file).unwrap().relative_auto_off);

        directory.set_hotkeys(&Hotkeys::default());
        directory.set_switch_modifier(None);
        let saved = DevicesFile::load(&file).unwrap();
        assert_eq!((saved.hotkeys, saved.switch_modifier), (None, None));
    }

    #[test]
    fn hotkeys_edited_into_the_file_wrongly_are_the_defaults() {
        let stored = |take_back: &str, lock: Option<&str>| DevicesFile {
            hotkeys: Some(HotkeysFile {
                take_back: Some(take_back.to_owned()),
                lock: lock.map(str::to_owned),
                jump_modifiers: None,
                relative: None,
            }),
            ..DevicesFile::default()
        };
        // One left out is the default.
        let directory = Directory::new(&config(), None, stored("control+KeyT", None));
        let hotkeys = directory.hotkeys();
        assert_eq!(hotkeys.take_back.to_string(), "control+KeyT");
        assert_eq!(hotkeys.lock, Hotkeys::default().lock);
        assert_eq!(hotkeys.jump, Hotkeys::default().jump);
        // Ones that cannot be used are all the defaults.
        for wrong in [
            stored("KeyT", None),
            stored("control+KeyL", Some("control+KeyL")),
            // The relative mode hotkey is one of them.
            DevicesFile {
                hotkeys: Some(HotkeysFile {
                    relative: Some("control+alt+shift+KeyL".to_owned()),
                    ..HotkeysFile::default()
                }),
                ..DevicesFile::default()
            },
        ] {
            let directory = Directory::new(&config(), None, wrong);
            assert_eq!(directory.hotkeys(), Hotkeys::default());
        }
    }

    /// Hotkeys saved before relative mode stay as they were, its default
    /// clashing or not.
    #[test]
    fn hotkeys_saved_before_relative_mode_are_kept() {
        let stored = |lock: &str| DevicesFile {
            hotkeys: Some(HotkeysFile {
                lock: Some(lock.to_owned()),
                ..HotkeysFile::default()
            }),
            ..DevicesFile::default()
        };
        let clashing = Directory::new(&config(), None, stored("control+alt+shift+KeyG"));
        let hotkeys = clashing.hotkeys();
        assert_eq!(hotkeys.lock.to_string(), "control+alt+shift+KeyG");
        assert_eq!(hotkeys.take_back, Hotkeys::default().take_back);
        assert_eq!(hotkeys.relative, None);
        // Written back so: off, from now on.
        clashing.set_hotkeys(&hotkeys);
        let saved = clashing.stored.lock().unwrap().hotkeys.clone().unwrap();
        assert_eq!(saved.relative.as_deref(), Some(""));
        assert_eq!(clashing.hotkeys(), hotkeys);
        let fitting = Directory::new(&config(), None, stored("control+KeyK"));
        assert_eq!(fitting.hotkeys().relative, Some(Hotkeys::DEFAULT_RELATIVE));
    }

    #[test]
    fn pairing_again_replaces_the_old_entry() {
        let directory = Directory::new(&config(), None, DevicesFile::default());
        directory.add_paired(paired("mini", 2));
        // Same device, new name.
        directory.add_paired(PairedDevice {
            name: "Studio".to_owned(),
            ..paired("mini", 2)
        });
        // A device already configured stays as configured.
        directory.add_paired(paired("pc", 1));
        let peers = directory.peers();
        assert_eq!(peers.len(), 2);
        assert_eq!(directory.peer("mini").unwrap().name, "Studio");
        assert_eq!(directory.peer("win").unwrap().origin, Origin::Configured);
    }

    #[test]
    fn a_cursor_speed_is_kept_for_a_trusted_device_until_it_is_unpaired() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("devices.toml");
        let directory = Directory::new(&config(), Some(file.clone()), DevicesFile::default());
        directory.add_paired(paired("mini", 2));
        let percent = |d: &Directory, id: &str| d.peer(id).unwrap().cursor_speed.percent();
        assert_eq!(percent(&directory, "mini"), 100);
        let (_, before) = directory.peers_now();
        let generation = directory.set_speed("mini", 150).unwrap();
        assert!(generation > before);
        let (peers, now) = directory.peers_now();
        assert_eq!(now, generation);
        assert_eq!(name_of(&peers, "mini").as_deref(), Some("MINI"));
        assert_eq!(percent(&directory, "mini"), 150);
        assert_eq!(
            DevicesFile::load(&file).unwrap().cursor_speed["mini"].percent(),
            150
        );
        // The same speed again changes nothing.
        assert_eq!(directory.set_speed("mini", 150).unwrap(), generation);
        // A device in the configuration file too.
        directory.set_speed("win", 60).unwrap();
        assert_eq!(percent(&directory, "win"), 60);
        for wrong in [0, 40, 155, 310] {
            assert!(matches!(
                directory.set_speed("mini", wrong),
                Err(DeviceError::InvalidSpeed)
            ));
        }
        assert_eq!(percent(&directory, "mini"), 150);
        // Not this device, nor one it does not trust.
        for id in ["mac", "ghost"] {
            assert!(matches!(
                directory.set_speed(id, 150),
                Err(DeviceError::Unknown(_))
            ));
        }
        // The speed is looked at first, as a name is.
        assert!(matches!(
            directory.set_speed("ghost", 155),
            Err(DeviceError::InvalidSpeed)
        ));
        // The normal speed has no entry.
        directory.set_speed("win", 100).unwrap();
        assert!(
            !DevicesFile::load(&file)
                .unwrap()
                .cursor_speed
                .contains_key("win")
        );

        // Kept across restarts.
        let stored = DevicesFile::load(&file).unwrap();
        let directory = Directory::new(&config(), None, stored);
        assert_eq!(percent(&directory, "mini"), 150);

        // Paired again, it keeps its speed; unpaired, it is gone.
        directory.set_speed("mini", 200).unwrap();
        directory.add_paired(PairedDevice {
            name: "Studio".to_owned(),
            ..paired("mini", 2)
        });
        assert_eq!(percent(&directory, "mini"), 200);
        directory.remove_paired("mini").unwrap();
        directory.add_paired(paired("mini", 2));
        assert_eq!(percent(&directory, "mini"), 100);
    }

    #[test]
    fn a_speed_left_for_a_device_no_longer_trusted_can_be_cleared() {
        let stored = DevicesFile {
            cursor_speed: [("gone".to_owned(), CursorSpeed::from_percent(150).unwrap())].into(),
            ..DevicesFile::default()
        };
        let directory = Directory::new(&config(), None, stored);
        // Only the normal speed, which clears it.
        assert!(matches!(
            directory.set_speed("gone", 200),
            Err(DeviceError::Unknown(_))
        ));
        directory.set_speed("gone", 100).unwrap();
        assert!(directory.stored.lock().unwrap().cursor_speed.is_empty());
        // Once cleared, it is a device like any other not trusted.
        assert!(matches!(
            directory.set_speed("gone", 100),
            Err(DeviceError::Unknown(_))
        ));
    }

    #[test]
    fn a_device_paired_again_under_another_id_takes_its_choices_along() {
        let directory = Directory::new(&config(), None, DevicesFile::default());
        directory.add_paired(paired("mini", 2));
        directory.set_speed("mini", 150).unwrap();
        directory.set_alias("mini", "Studio").unwrap();
        directory.set_sharing("mini", true, false).unwrap();
        // The same device, now calling itself "studio".
        directory.add_paired(PairedDevice {
            id: "studio".to_owned(),
            ..paired("mini", 2)
        });
        let studio = directory.peer("studio").unwrap();
        assert_eq!(studio.cursor_speed.percent(), 150);
        assert_eq!(studio.name, "Studio");
        assert!(studio.clipboard_off && !studio.files_off);
        let stored = directory.stored.lock().unwrap().clone();
        assert!(!stored.cursor_speed.contains_key("mini"));
        assert!(!stored.aliases.contains_key("mini"));
        assert!(stored.device_sharing.iter().all(|s| s.id != "mini"));
        // Another device that comes as "mini" starts afresh.
        directory.add_paired(paired("mini", 7));
        let mini = directory.peer("mini").unwrap();
        assert_eq!(mini.cursor_speed, CursorSpeed::NORMAL);
        assert_eq!(mini.name, "MINI");
        assert!(!mini.clipboard_off);
    }

    #[test]
    fn a_device_paired_under_an_id_left_from_the_configuration_file_starts_afresh() {
        // "laptop" was listed in the configuration file, given a name, a
        // speed and its files turned off here, then taken out of the file:
        // what was chosen for it stays until the file is edited.
        let stored = DevicesFile {
            aliases: [("laptop".to_owned(), "Old laptop".to_owned())].into(),
            cursor_speed: [("laptop".to_owned(), CursorSpeed::from_percent(200).unwrap())].into(),
            device_sharing: vec![DeviceSharing {
                id: "laptop".to_owned(),
                clipboard_off: false,
                files_off: true,
            }],
            ..DevicesFile::default()
        };
        let directory = Directory::new(&config(), None, stored);
        // Another computer pairs as "laptop": none of that is its.
        directory.add_paired(paired("laptop", 9));
        let laptop = directory.peer("laptop").unwrap();
        assert_eq!(laptop.name, "LAPTOP");
        assert_eq!(laptop.cursor_speed, CursorSpeed::NORMAL);
        assert!(!laptop.files_off);
        let stored = directory.stored.lock().unwrap().clone();
        assert!(stored.aliases.is_empty() && stored.cursor_speed.is_empty());
        assert!(stored.device_sharing.is_empty());
    }

    #[test]
    fn another_device_paired_under_an_id_takes_nothing_of_the_one_before() {
        let directory = Directory::new(&config(), None, DevicesFile::default());
        directory.add_paired(paired("mini", 2));
        directory.set_speed("mini", 150).unwrap();
        directory.set_alias("mini", "Studio").unwrap();
        directory.set_sharing("mini", false, true).unwrap();
        // A different computer, under the same id.
        directory.add_paired(paired("mini", 8));
        let mini = directory.peer("mini").unwrap();
        assert_eq!(mini.fingerprint, fp(8));
        assert_eq!(mini.cursor_speed, CursorSpeed::NORMAL);
        assert_eq!(mini.name, "MINI");
        assert!(!mini.files_off);
        // The same one again keeps what it has.
        directory.set_speed("mini", 60).unwrap();
        directory.add_paired(paired("mini", 8));
        assert_eq!(directory.peer("mini").unwrap().cursor_speed.percent(), 60);
    }

    #[test]
    fn ids_must_not_clash() {
        let directory = Directory::new(&config(), None, DevicesFile::default());
        assert!(directory.id_conflicts("mac", &fp(9)));
        assert!(directory.id_conflicts("win", &fp(9)));
        assert!(!directory.id_conflicts("win", &fp(1)));
        assert!(!directory.id_conflicts("mini", &fp(9)));

        // A devices file edited into a clash does not override the config.
        let stored = DevicesFile {
            devices: vec![paired("win", 3), paired("mac", 4), paired("ok", 5)],
            ..DevicesFile::default()
        };
        let directory = Directory::new(&config(), None, stored);
        let ids: Vec<String> = directory.peers().iter().map(|p| p.id.clone()).collect();
        assert_eq!(ids, vec!["win", "ok"]);
    }

    #[test]
    fn addresses_prefer_configuration_then_discovery_then_history() {
        let directory = Directory::new(&config(), None, DevicesFile::default());
        directory.add_paired(PairedDevice {
            last_address: Some("10.0.0.9:47820".parse().unwrap()),
            ..paired("mini", 2)
        });
        let local: SocketAddr = "0.0.0.0:47820".parse().unwrap();
        let mini = directory.peer("mini").unwrap();
        assert_eq!(
            directory.addresses(&mini, local),
            vec!["10.0.0.9:47820".parse().unwrap()]
        );

        directory.found(Found {
            device_id: "mini".to_owned(),
            name: "Mac mini".to_owned(),
            os: Os::Macos,
            fingerprint: fp(2),
            pairable: true,
            protocol_min: 1,
            protocol_max: 1,
            addresses: vec![
                "10.0.0.7:47820".parse().unwrap(),
                "[fe80::1]:47820".parse().unwrap(),
            ],
        });
        assert_eq!(
            directory.addresses(&mini, local),
            vec![
                "10.0.0.7:47820".parse().unwrap(),
                "10.0.0.9:47820".parse().unwrap()
            ]
        );
        assert_eq!(directory.nearby_by_id("mini").unwrap().name, "Mac mini");

        directory.lost(&fp(2));
        assert!(directory.nearby().is_empty());
        let win = directory.peer("win").unwrap();
        assert_eq!(
            directory.addresses(&win, local),
            vec!["192.168.1.20:47820".parse().unwrap()]
        );
    }

    #[test]
    fn the_configuration_file_places_first() {
        let stored = DevicesFile {
            layout: vec![
                PlacementConfig {
                    device: "win".to_owned(),
                    x: 9.0,
                    y: 9.0,
                },
                PlacementConfig {
                    device: "mini".to_owned(),
                    x: 5.0,
                    y: 5.0,
                },
            ],
            ..DevicesFile::default()
        };
        let directory = Directory::new(&config(), None, stored);
        let placements = directory.placements();
        assert_eq!(placements.len(), 2);
        assert_eq!((placements[0].x, placements[0].y), (1512.0, 0.0));
        assert_eq!(placements[1].device, "mini");
    }

    fn doc(version: u64, author: &str, x: f64) -> LayoutDoc {
        LayoutDoc {
            version,
            author: author.to_owned(),
            placements: vec![PlacementConfig {
                device: "mini".to_owned(),
                x,
                y: 0.0,
            }],
        }
    }

    #[test]
    fn newer_arrangements_replace_older_ones() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("devices.toml");
        let directory = Directory::new(&config(), Some(file.clone()), DevicesFile::default());
        assert!(directory.adopt(doc(2, "win", 10.0)));
        assert!(!directory.adopt(doc(2, "win", 20.0)), "same version");
        assert!(!directory.adopt(doc(1, "zzz", 20.0)), "older");
        // Same number from another device: the author breaks the tie.
        assert!(!directory.adopt(doc(2, "mac", 30.0)));
        assert!(directory.adopt(doc(2, "wiz", 40.0)));
        assert_eq!(directory.layout().placements[0].x, 40.0);

        let mine = directory.rearrange(&[("mini".to_owned(), Point::new(1.0, 2.0))]);
        assert_eq!((mine.version, mine.author.as_str()), (3, "mac"));
        assert_eq!(DevicesFile::load(&file).unwrap().layout_version, 3);
        assert!(!directory.adopt(doc(2, "zzz", 0.0)));
    }

    #[test]
    fn aliases_and_sharing_stay_here_until_unpaired() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("devices.toml");
        let directory = Directory::new(&config(), Some(file.clone()), DevicesFile::default());
        directory.add_paired(paired("mini", 2));

        // A configured device and a paired one alike.
        directory.set_alias("win", "  書房的 PC ").unwrap();
        directory.set_alias("mini", "Studio").unwrap();
        directory.set_sharing("win", true, false).unwrap();
        directory.set_sharing("mini", false, true).unwrap();
        let win = directory.peer("win").unwrap();
        assert_eq!(
            (win.name.as_str(), win.reported_name.as_str()),
            ("書房的 PC", "Windows PC")
        );
        assert!(win.clipboard_off && !win.files_off);
        let mini = directory.peer("mini").unwrap();
        assert_eq!(
            (mini.name.as_str(), mini.reported_name.as_str()),
            ("Studio", "MINI")
        );
        assert!(!mini.clipboard_off && mini.files_off);

        // The device renaming itself leaves the alias alone.
        directory.renamed("mini", "Mac mini");
        let mini = directory.peer("mini").unwrap();
        assert_eq!(
            (mini.name.as_str(), mini.reported_name.as_str()),
            ("Studio", "Mac mini")
        );

        let saved = DevicesFile::load(&file).unwrap();
        assert_eq!(saved.aliases.len(), 2);
        assert_eq!(saved.device_sharing.len(), 2);

        assert!(matches!(
            directory.set_alias("ghost", "x"),
            Err(DeviceError::Unknown(_))
        ));
        assert!(matches!(
            directory.set_alias("mac", "x"),
            Err(DeviceError::Unknown(_))
        ));
        assert!(matches!(
            directory.set_alias("win", "bad\nname"),
            Err(DeviceError::InvalidName)
        ));
        assert!(matches!(
            directory.set_sharing("ghost", true, true),
            Err(DeviceError::Unknown(_))
        ));

        // Cleared, the device's own name shows again; both on, no entry.
        directory.set_alias("win", "").unwrap();
        directory.set_sharing("win", false, false).unwrap();
        let win = directory.peer("win").unwrap();
        assert_eq!(win.name, "Windows PC");
        assert!(!win.clipboard_off && !win.files_off);
        let saved = DevicesFile::load(&file).unwrap();
        assert_eq!(saved.aliases.keys().collect::<Vec<_>>(), ["mini"]);
        assert_eq!(saved.device_sharing.len(), 1);

        directory.remove_paired("mini").unwrap();
        let saved = DevicesFile::load(&file).unwrap();
        assert!(saved.aliases.is_empty() && saved.device_sharing.is_empty());
    }

    #[test]
    fn displays_are_remembered_until_unpaired() {
        let directory = Directory::new(&config(), None, DevicesFile::default());
        directory.add_paired(paired("mini", 2));
        let displays = vec![Display {
            id: "main".to_owned(),
            bounds: iw_core::geometry::Rect::new(0.0, 0.0, 1920.0, 1080.0),
            scale: 2.0,
            primary: true,
        }];
        directory.remember_displays("mini", &displays);
        assert_eq!(directory.known_displays("mini"), Some(displays));
        directory.remove_paired("mini").unwrap();
        assert_eq!(directory.known_displays("mini"), None);
    }
}
