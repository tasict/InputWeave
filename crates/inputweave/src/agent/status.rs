use std::time::{SystemTime, UNIX_EPOCH};

use iw_core::geometry::{Point, Rect};
use iw_core::hotkey::{Hotkeys, Modifier};
use iw_core::layout::{Display, Edge};
use iw_core::router::{RelativeEnd, ReturnReason};
use iw_net::discovery::{Found, os_name};
use iw_net::pairing::MAX_ATTEMPTS;
use iw_platform::{ClipboardFormat, Firewall, PermissionState, SessionInfo};
use iw_proto::app::v1 as app;
use iw_proto::peer::v1::{ClipboardSkip, FileDecline, Os};

use super::directory::{Origin, Peer};
use super::drag::Dragging;
use super::files::{BatchInfo, Direction, FileSettings, TransferState, TransferStatus};
use super::folder_access::FolderAccess;
use super::license::{LicenseState, LicenseStatus};
use super::pairing::{PairingPeer, PairingStatus};
use super::previews::{MAX_ARRANGEMENT_PICTURES, Pictures};
use crate::config::Config;

/// The cursor crossed an edge of one of this device's displays, going to
/// or coming from another device (`Event.cursor_crossed`): the apps light
/// up that edge there. Only for a real crossing of an edge.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorCrossed {
    pub display: String,
    pub edge: Edge,
    /// Where along the edge, in this device's desktop coordinates: y on a
    /// left or right edge, x on a top or bottom one.
    pub along: f64,
    /// The display's bounds, in the same coordinates.
    pub bounds: Rect,
    /// Whether the cursor came in here; false: it went out.
    pub arriving: bool,
    /// The device on the other side, and what this device calls it (empty
    /// when it does not know it).
    pub other_device_id: String,
    pub other_device_name: String,
}

impl CursorCrossed {
    pub fn to_proto(&self) -> app::CursorCrossed {
        app::CursorCrossed {
            display_id: self.display.clone(),
            edge: crate::convert::edge_to_app(self.edge).into(),
            along: self.along,
            display_x: self.bounds.x,
            display_y: self.bounds.y,
            display_width: self.bounds.width,
            display_height: self.bounds.height,
            arriving: self.arriving,
            other_device_id: self.other_device_id.clone(),
            other_device_name: self.other_device_name.clone(),
        }
    }
}

/// A snapshot of the agent's state, published after every change.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub sharing: bool,
    pub this_device: String,
    pub this_name: String,
    /// Whether local input is being captured, so this device can control
    /// others.
    pub capturing: bool,
    /// The permissions the platform needs, and whether each is granted.
    pub permissions: Vec<PermissionState>,
    /// Secure keyboard entry is on, so keystrokes stay on this device.
    pub secure_input: bool,
    /// This device's screen is locked.
    pub screen_locked: bool,
    /// Input from other devices reaches this device's lock screen.
    pub lock_screen_input: bool,
    /// Input from other devices reaches this device at all now
    /// (`SessionStatus::input_available`).
    pub input_available: bool,
    /// The device under the cursor: this one, or the one being controlled.
    pub cursor_on: String,
    /// The device controlling this one, if any.
    pub controlled_by: Option<String>,
    /// Whether this device and its displays are part of a valid layout, so
    /// the cursor can cross to other devices.
    pub layout_ready: bool,
    pub devices: Vec<DeviceState>,
    /// Why control last came back to this device.
    pub last_return: Option<ReturnReason>,
    /// How many times control came back, so a client can tell each time
    /// apart even when the reason repeats.
    pub returns: u64,
    pub clipboard_sync: bool,
    /// The last time a clipboard moved, or was held back, through here.
    pub last_clipboard: Option<ClipboardActivity>,
    /// Clipboards offered and pulled.
    pub lazy_clipboard: LazyStats,
    /// The cursor stays on the device it is on.
    pub locked: bool,
    /// How long the cursor pushes against an edge before it crosses.
    pub edge_dwell_ms: u32,
    /// Previews of this device's desktop pictures go to other devices.
    pub preview_sharing: bool,
    /// Something reads this device's desktop pictures now: the platform,
    /// or on Windows the broker while it is connected. When nothing ever
    /// has, `preview_sharing` sends nothing.
    pub desktop_pictures: bool,
    /// The drag of files going on from here.
    pub dragging: Option<Dragging>,
    /// The hotkeys in effect.
    pub hotkeys: Hotkeys,
    /// The modifier the cursor needs held to cross an edge, if any.
    pub switch_modifier: Option<Modifier>,
    /// Other devices may control this Mac at the login window.
    pub login_window_access: bool,
    /// The device controlling this one can carry a drag of files from here.
    pub drags_carried: bool,
    /// The device in relative mode, the one the cursor is on.
    pub relative: Option<RelativeMode>,
    /// Devices enter relative mode by themselves when an app there holds
    /// the pointer.
    pub relative_auto: bool,
    /// An app here holds the pointer, as other devices are told.
    pub pointer_captured: bool,
    /// How many times relative mode ended, so a client can tell each time
    /// apart even when the reason repeats.
    pub relative_ends: u64,
    /// Why relative mode last ended.
    pub last_relative_end: Option<RelativeEnded>,
    /// The folders macOS protects and what it said of each.
    pub folder_access: FolderAccess,
    /// What kind of session this is and what serves it (Linux).
    pub session: Option<SessionInfo>,
    /// The cursor leaves this device only across its edges, never by a
    /// jump (`InputCapture::edges_only`).
    pub edges_only: bool,
    /// Whether a firewall lets other devices reach the agent, and its port.
    pub firewall: (Firewall, u16),
    /// This computer's license; none where the agent keeps none
    /// (`Options::licenses`).
    pub license: Option<LicenseStatus>,
}

/// Relative mode while it is on: the device and how it started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelativeMode {
    pub device: String,
    /// It began because the device said an app there holds the pointer.
    pub automatic: bool,
}

/// Relative mode ending on a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelativeEnded {
    pub device: String,
    pub reason: RelativeEnd,
}

/// What happened to clipboards offered rather than sent
/// (docs/architecture.md §8), for diagnostics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LazyStats {
    /// Offers of this device's clipboard.
    pub offered: u64,
    /// Offers this device's clipboard took.
    pub received: u64,
    /// Pulls answered with this device's clipboard.
    pub served: u64,
    /// Pulls that brought content here.
    pub pulled: u64,
    /// Pulls from here that came to nothing.
    pub failed: u64,
    /// Content pulled for this device's clipboard after something else
    /// was copied here, and dropped; or an offer not promised at all,
    /// as something was copied here while the Windows app was waited for.
    pub superseded: u64,
    pub bytes_pulled: u64,
    /// How the last pull from here ended.
    pub last: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClipboardActivity {
    pub at: SystemTime,
    pub from: String,
    pub to: String,
    pub formats: Vec<ClipboardFormat>,
    /// Why it was held back, if it was.
    pub skipped: Option<ClipboardSkip>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceState {
    pub id: String,
    /// The name this device's user gave it, or else `reported_name`.
    pub name: String,
    /// The name it gives itself, or the configuration file gives it.
    pub reported_name: String,
    /// Paired, rather than listed in the configuration file.
    pub paired: bool,
    pub os: Os,
    pub swap_gui_and_control: bool,
    pub connected: bool,
    /// Placed in the layout with known displays, so the cursor can reach it
    /// while it is connected.
    pub arranged: bool,
    /// Round-trip time in milliseconds, rounded up; 0 while offline or not
    /// yet measured.
    pub rtt_ms: u32,
    /// The clipboard is not shared with it.
    pub clipboard_off: bool,
    /// Files are not sent to it or taken from it.
    pub files_off: bool,
    /// The number the jump hotkeys go to it by, from 1; 0 for none.
    pub jump_slot: u32,
    /// Its screen is locked, as it last said; false while offline.
    pub screen_locked: bool,
    /// Input from here reaches its lock screen, as it last said.
    pub lock_screen_input: bool,
    /// An app on it holds the pointer, as it last said; false while
    /// offline.
    pub pointer_captured: bool,
    /// No input from here reaches it now, as it last said; false while
    /// offline.
    pub input_unavailable: bool,
    /// It paused sharing, as it last said; false while offline.
    pub sharing_paused: bool,
}

impl DeviceState {
    /// A trusted device as the directory has it, not yet connected.
    pub(crate) fn of(peer: &Peer) -> Self {
        Self {
            id: peer.id.clone(),
            name: peer.name.clone(),
            reported_name: peer.reported_name.clone(),
            paired: peer.origin == Origin::Paired,
            os: peer.os,
            swap_gui_and_control: peer.swap_gui_and_control,
            connected: false,
            arranged: false,
            rtt_ms: 0,
            clipboard_off: peer.clipboard_off,
            files_off: peer.files_off,
            jump_slot: 0,
            screen_locked: false,
            lock_screen_input: false,
            pointer_captured: false,
            input_unavailable: false,
            sharing_paused: false,
        }
    }
}

impl Status {
    pub(crate) fn initial(
        config: &Config,
        peers: &[Peer],
        preview_sharing: bool,
        desktop_pictures: bool,
        hotkeys: Hotkeys,
        login_window_access: bool,
        folder_access: FolderAccess,
    ) -> Self {
        Self {
            sharing: true,
            this_device: config.device.id.clone(),
            this_name: config.device.name.clone(),
            capturing: false,
            permissions: Vec::new(),
            secure_input: false,
            screen_locked: false,
            lock_screen_input: false,
            input_available: true,
            cursor_on: config.device.id.clone(),
            controlled_by: None,
            layout_ready: false,
            devices: peers.iter().map(DeviceState::of).collect(),
            last_return: None,
            returns: 0,
            clipboard_sync: true,
            last_clipboard: None,
            lazy_clipboard: LazyStats::default(),
            locked: false,
            edge_dwell_ms: 0,
            preview_sharing,
            desktop_pictures,
            dragging: None,
            hotkeys,
            switch_modifier: None,
            login_window_access,
            drags_carried: false,
            relative: None,
            relative_auto: true,
            pointer_captured: false,
            relative_ends: 0,
            last_relative_end: None,
            folder_access,
            session: None,
            edges_only: false,
            firewall: (Firewall::Unknown, 0),
            license: None,
        }
    }

    pub fn device(&self, id: &str) -> Option<&DeviceState> {
        self.devices.iter().find(|d| d.id == id)
    }

    pub fn to_proto(&self) -> app::Status {
        app::Status {
            sharing: self.sharing,
            this_device_id: self.this_device.clone(),
            this_device_name: self.this_name.clone(),
            capturing: self.capturing,
            secure_input: self.secure_input,
            screen_locked: self.screen_locked,
            lock_screen_input: self.lock_screen_input,
            permissions: self
                .permissions
                .iter()
                .map(crate::convert::permission_to_proto)
                .collect(),
            active_device_id: self.cursor_on.clone(),
            controlled_by_device_id: self.controlled_by.clone().unwrap_or_default(),
            control_returns: self.returns,
            last_return: self
                .last_return
                .as_ref()
                .map_or(app::ReturnReason::Unspecified, return_reason_to_proto)
                .into(),
            clipboard_sync: self.clipboard_sync,
            last_clipboard: self
                .last_clipboard
                .as_ref()
                .map(|c| app::ClipboardActivity {
                    at_unix_ms: unix_ms(c.at),
                    from_device_id: c.from.clone(),
                    to_device_id: c.to.clone(),
                    kinds: clipboard_kinds(&c.formats),
                    skipped: match c.skipped {
                        Some(ClipboardSkip::Sensitive) => "sensitive".to_owned(),
                        Some(ClipboardSkip::TooLarge) => "too_large".to_owned(),
                        Some(_) => "other".to_owned(),
                        None => String::new(),
                    },
                }),
            last_return_device_id: match &self.last_return {
                Some(
                    ReturnReason::PeerLost(d)
                    | ReturnReason::RemoteActivity(d)
                    | ReturnReason::ScreenLocked(d)
                    | ReturnReason::InputUnavailable(d)
                    | ReturnReason::PeerPaused(d),
                ) => d.to_string(),
                _ => String::new(),
            },
            devices: self
                .devices
                .iter()
                .map(|d| app::DeviceStatus {
                    id: d.id.clone(),
                    name: d.name.clone(),
                    reported_name: d.reported_name.clone(),
                    clipboard_off: d.clipboard_off,
                    files_off: d.files_off,
                    paired: d.paired,
                    os: os_name(d.os).to_owned(),
                    swap_gui_and_control: d.swap_gui_and_control,
                    state: if d.connected {
                        app::ConnectionState::Connected
                    } else {
                        app::ConnectionState::Offline
                    }
                    .into(),
                    rtt_ms: d.rtt_ms,
                    jump_slot: d.jump_slot,
                    screen_locked: d.screen_locked,
                    lock_screen_input: d.lock_screen_input,
                    pointer_captured: d.pointer_captured,
                    input_unavailable: d.input_unavailable,
                    sharing_paused: d.sharing_paused,
                })
                .collect(),
            locked: self.locked,
            edge_dwell_ms: self.edge_dwell_ms,
            default_dwell_ms: super::DEFAULT_DWELL_MS,
            preview_sharing: self.preview_sharing,
            desktop_pictures_available: self.desktop_pictures,
            dragging: self.dragging.as_ref().map(|d| app::Dragging {
                drag_id: d.id.clone(),
                items: d.items,
                files: d.files,
                bytes: d.bytes,
                device_id: d.device.clone().unwrap_or_default(),
            }),
            hotkeys: Some(hotkeys_to_proto(&self.hotkeys)),
            switch_modifier: switch_modifier_to_proto(self.switch_modifier).into(),
            login_window_access: self.login_window_access,
            drags_carried: self.drags_carried,
            relative_mouse: self
                .relative
                .as_ref()
                .map(|r| r.device.clone())
                .unwrap_or_default(),
            relative_mouse_automatic: self.relative.as_ref().is_some_and(|r| r.automatic),
            relative_auto: self.relative_auto,
            pointer_captured: self.pointer_captured,
            relative_ends: self.relative_ends,
            relative_supported: true,
            last_relative_end: self.last_relative_end.as_ref().map(|end| app::RelativeEnd {
                device_id: end.device.clone(),
                reason: relative_end_to_proto(end.reason).into(),
            }),
            folder_access: self.folder_access.to_proto(),
            folder_access_asked: self.folder_access.asked,
            folder_access_round: self.folder_access.round,
            session: self.session.as_ref().map(crate::convert::session_to_proto),
            cursor_leaves_at_edges_only: self.edges_only,
            firewall: crate::convert::firewall_to_proto(self.firewall.0, self.firewall.1),
            license: self.license.as_ref().map(license_to_proto),
        }
    }
}

/// The license as clients read it.
pub fn license_to_proto(license: &LicenseStatus) -> app::License {
    app::License {
        key: license.key.clone(),
        state: license_state_to_proto(license.state).into(),
        activation_id: license.activation_id.clone(),
        customer_email: license.customer_email.clone(),
        validated_at: license.validated_at,
        source_device_id: license.source_device_id.clone(),
        source_device_name: license.source_device_name.clone(),
        opted_out: license.opted_out,
        devices: license
            .devices
            .iter()
            .map(|d| app::LicensedDevice {
                id: d.id.clone(),
                name: d.name.clone(),
                licensed: d.licensed,
            })
            .collect(),
    }
}

pub fn license_state_to_proto(state: LicenseState) -> app::LicenseState {
    match state {
        LicenseState::None => app::LicenseState::None,
        LicenseState::PendingActivation => app::LicenseState::PendingActivation,
        LicenseState::Licensed => app::LicenseState::Licensed,
        LicenseState::SeatsFull => app::LicenseState::SeatsFull,
        LicenseState::Invalid => app::LicenseState::Invalid,
        LicenseState::Unavailable => app::LicenseState::Unavailable,
    }
}

/// The hotkeys as clients read and write them.
pub fn hotkeys_to_proto(hotkeys: &Hotkeys) -> app::Hotkeys {
    app::Hotkeys {
        take_back: hotkeys.take_back.to_string(),
        lock: hotkeys.lock.to_string(),
        jump_modifiers: hotkeys.jump.map(|m| m.to_string()).unwrap_or_default(),
        relative: Some(hotkeys.relative.map(|h| h.to_string()).unwrap_or_default()),
    }
}

pub fn switch_modifier_to_proto(modifier: Option<Modifier>) -> app::SwitchModifier {
    match modifier {
        None => app::SwitchModifier::None,
        Some(Modifier::Control) => app::SwitchModifier::Control,
        Some(Modifier::Alt) => app::SwitchModifier::Alt,
        Some(Modifier::Shift) => app::SwitchModifier::Shift,
        Some(Modifier::Gui) => app::SwitchModifier::Gui,
    }
}

pub fn switch_modifier_from_proto(modifier: app::SwitchModifier) -> Option<Modifier> {
    match modifier {
        app::SwitchModifier::None => None,
        app::SwitchModifier::Control => Some(Modifier::Control),
        app::SwitchModifier::Alt => Some(Modifier::Alt),
        app::SwitchModifier::Shift => Some(Modifier::Shift),
        app::SwitchModifier::Gui => Some(Modifier::Gui),
    }
}

/// Where every known device sits, for the arrangement canvas.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arrangement {
    pub version: u64,
    /// This device first, then trusted devices.
    pub devices: Vec<ArrangedDevice>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrangedDevice {
    pub id: String,
    pub name: String,
    pub os: Os,
    /// Where its desktop origin sits on the canvas, once it has a place.
    pub offset: Option<Point>,
    /// Its displays in its own desktop coordinates, as last reported.
    pub displays: Vec<Display>,
    /// Previews of its desktop pictures, by display id, as last reported:
    /// this device's own even while it does not share them.
    pub pictures: Pictures,
    pub connected: bool,
    pub this_device: bool,
    /// The number the jump hotkeys go to it by, from 1; 0 for none.
    pub jump_slot: u32,
}

impl Arrangement {
    pub fn device(&self, id: &str) -> Option<&ArrangedDevice> {
        self.devices.iter().find(|d| d.id == id)
    }

    /// The arrangement for IPC clients. Devices whose pictures would take
    /// the total past [`MAX_ARRANGEMENT_PICTURES`] go without, so the
    /// message stays well inside an IPC frame.
    pub fn to_proto(&self) -> app::Arrangement {
        let mut budget = MAX_ARRANGEMENT_PICTURES;
        app::Arrangement {
            version: self.version,
            devices: self
                .devices
                .iter()
                .map(|d| {
                    let offset = d.offset.unwrap_or_default();
                    let shown: usize = d
                        .displays
                        .iter()
                        .filter_map(|s| d.pictures.get(&s.id))
                        .map(|p| p.len())
                        .sum();
                    let with_pictures = shown <= budget;
                    if with_pictures {
                        budget -= shown;
                    }
                    app::ArrangedDevice {
                        id: d.id.clone(),
                        name: d.name.clone(),
                        os: os_name(d.os).to_owned(),
                        placed: d.offset.is_some(),
                        x: offset.x,
                        y: offset.y,
                        screens: d
                            .displays
                            .iter()
                            .map(|s| app::Screen {
                                id: s.id.clone(),
                                x: s.bounds.x,
                                y: s.bounds.y,
                                width: s.bounds.width,
                                height: s.bounds.height,
                                scale: s.scale,
                                primary: s.primary,
                                preview_jpeg: d
                                    .pictures
                                    .get(&s.id)
                                    .filter(|_| with_pictures)
                                    .map(|p| p.bytes().to_vec())
                                    .unwrap_or_default(),
                            })
                            .collect(),
                        connected: d.connected,
                        this_device: d.this_device,
                        jump_slot: d.jump_slot,
                    }
                })
                .collect(),
        }
    }
}

/// The transfer list and settings for IPC clients.
pub fn transfers_to_proto(transfers: &[TransferStatus], settings: &FileSettings) -> app::Transfers {
    use app::TransferState as S;
    app::Transfers {
        receive_folder: settings
            .folder
            .as_ref()
            .map(|f| f.display().to_string())
            .unwrap_or_default(),
        ask_before_receiving: settings.ask,
        transfers: transfers
            .iter()
            .map(|t| {
                let state = match &t.state {
                    TransferState::Queued => S::Queued,
                    TransferState::Preparing => S::Preparing,
                    TransferState::WaitingForConnection => S::WaitingForConnection,
                    TransferState::WaitingForAnswer => S::WaitingForAnswer,
                    TransferState::Transferring => S::Transferring,
                    TransferState::Verifying => S::Verifying,
                    TransferState::Done => S::Done,
                    TransferState::Declined(_) => S::Declined,
                    TransferState::Failed(_) => S::Failed,
                    TransferState::Cancelled => S::Cancelled,
                };
                let reason = t.state.reason();
                let batch = t.batch.clone().unwrap_or(BatchInfo {
                    id: String::new(),
                    index: 0,
                    items: 1,
                    files: 1,
                    bytes: t.size,
                    drag: None,
                    clipboard: None,
                });
                // A lone file counts as one.
                let (files, files_done, files_failed, failures) = match &t.folder {
                    Some(f) => (f.entries, f.done, f.failed, f.failures.as_slice()),
                    None => (
                        1,
                        u32::from(t.state == TransferState::Done),
                        u32::from(matches!(
                            t.state,
                            TransferState::Failed(_)
                                | TransferState::Declined(FileDecline::InvalidPath)
                        )),
                        &[][..],
                    ),
                };
                app::FileTransfer {
                    transfer_id: t.id.clone(),
                    outgoing: t.direction == Direction::Send,
                    device_id: t.device.clone(),
                    name: t.name.clone(),
                    size: t.size,
                    done_bytes: t.done,
                    bytes_per_second: t.bytes_per_second,
                    state: state.into(),
                    reason: reason.replace(' ', "_"),
                    missing_bytes: t.missing_bytes,
                    path: t
                        .path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    started_at_unix_ms: unix_ms(t.started),
                    batch_id: batch.id,
                    batch_index: batch.index,
                    batch_items: batch.items,
                    batch_files: batch.files,
                    batch_bytes: batch.bytes,
                    drag_id: batch.drag.unwrap_or_default(),
                    clipboard: batch.clipboard.is_some(),
                    folder: t.folder.is_some(),
                    files,
                    files_done,
                    files_failed,
                    failures: failures
                        .iter()
                        .map(|f| app::FailedFile {
                            relative_path: f.path.clone(),
                            reason: f.reason.clone(),
                        })
                        .collect(),
                }
            })
            .collect(),
    }
}

fn unix_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// What kind of thing was copied, for a sentence such as "synced from the
/// PC (text)": "text", "rich_text" or "image".
fn clipboard_kinds(formats: &[ClipboardFormat]) -> Vec<String> {
    let mut kinds = Vec::new();
    if formats.contains(&ClipboardFormat::Png) {
        kinds.push("image".to_owned());
    }
    if formats
        .iter()
        .any(|f| matches!(f, ClipboardFormat::Rtf | ClipboardFormat::Html))
    {
        kinds.push("rich_text".to_owned());
    } else if formats.contains(&ClipboardFormat::Text) {
        kinds.push("text".to_owned());
    }
    kinds
}

fn return_reason_to_proto(reason: &ReturnReason) -> app::ReturnReason {
    match reason {
        ReturnReason::Hotkey => app::ReturnReason::Hotkey,
        ReturnReason::PeerLost(_) => app::ReturnReason::PeerLost,
        ReturnReason::RemoteActivity(_) => app::ReturnReason::RemoteActivity,
        ReturnReason::SharingPaused => app::ReturnReason::SharingPaused,
        ReturnReason::LayoutChanged => app::ReturnReason::LayoutChanged,
        ReturnReason::ScreenLocked(_) => app::ReturnReason::ScreenLocked,
        ReturnReason::InputUnavailable(_) => app::ReturnReason::InputUnavailable,
        ReturnReason::PeerPaused(_) => app::ReturnReason::PeerPaused,
        ReturnReason::CaptureEnded => app::ReturnReason::CaptureEnded,
    }
}

fn relative_end_to_proto(reason: RelativeEnd) -> app::RelativeEndReason {
    match reason {
        RelativeEnd::TurnedOff => app::RelativeEndReason::TurnedOff,
        RelativeEnd::Released => app::RelativeEndReason::Released,
        RelativeEnd::AutoOff => app::RelativeEndReason::AutoOff,
        RelativeEnd::Jumped => app::RelativeEndReason::Jumped,
        RelativeEnd::Paused => app::RelativeEndReason::SharingPaused,
        RelativeEnd::Lost => app::RelativeEndReason::DeviceLost,
        RelativeEnd::Returned => app::RelativeEndReason::ControlReturned,
    }
}

impl PairingStatus {
    pub fn to_proto(&self) -> app::PairingState {
        use app::PairingPhase as Phase;
        let with_peer = |phase: Phase, peer: &PairingPeer| app::PairingState {
            phase: phase.into(),
            peer_device_id: peer.id.clone(),
            peer_name: peer.name.clone(),
            ..Default::default()
        };
        match self {
            PairingStatus::Idle => app::PairingState {
                phase: Phase::Idle.into(),
                ..Default::default()
            },
            PairingStatus::Connecting { .. } => app::PairingState {
                phase: Phase::Connecting.into(),
                ..Default::default()
            },
            PairingStatus::EnterCode {
                peer,
                attempts_left,
            } => app::PairingState {
                attempts_left: *attempts_left,
                error: if *attempts_left < MAX_ATTEMPTS {
                    "wrong_code".to_owned()
                } else {
                    String::new()
                },
                ..with_peer(Phase::EnterCode, peer)
            },
            PairingStatus::Verifying { peer } => with_peer(Phase::Verifying, peer),
            PairingStatus::ShowCode {
                peer,
                code,
                expires_at,
            } => app::PairingState {
                code: code.as_str().to_owned(),
                expires_at_unix_ms: expires_at
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64),
                ..with_peer(Phase::ShowCode, peer)
            },
            PairingStatus::Paired { peer } => with_peer(Phase::Paired, peer),
            PairingStatus::Failed { peer, reason } => app::PairingState {
                error: reason.code().to_owned(),
                ..match peer {
                    Some(peer) => with_peer(Phase::Failed, peer),
                    None => app::PairingState {
                        phase: Phase::Failed.into(),
                        ..Default::default()
                    },
                }
            },
        }
    }
}

/// Devices seen nearby, marking the ones already trusted.
pub fn nearby_to_proto(nearby: &[Found], peers: &[Peer]) -> app::NearbyDevices {
    app::NearbyDevices {
        devices: nearby
            .iter()
            .map(|f| app::NearbyDevice {
                id: f.device_id.clone(),
                name: f.name.clone(),
                os: os_name(f.os).to_owned(),
                address: f
                    .addresses
                    .first()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                paired: peers.iter().any(|p| p.fingerprint == f.fingerprint),
                pairable: f.pairable,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use iw_core::geometry::Rect;

    use super::*;
    use crate::agent::previews::Preview;

    #[test]
    fn the_desktop_taking_input_back_has_a_reason_of_its_own() {
        assert_eq!(
            return_reason_to_proto(&ReturnReason::CaptureEnded),
            app::ReturnReason::CaptureEnded
        );
        assert_eq!(
            return_reason_to_proto(&ReturnReason::PeerPaused(iw_core::layout::DeviceId::new(
                "pc"
            ))),
            app::ReturnReason::PeerPaused
        );
        assert_eq!(
            return_reason_to_proto(&ReturnReason::InputUnavailable(
                iw_core::layout::DeviceId::new("pc")
            )),
            app::ReturnReason::InputUnavailable
        );
    }

    fn device(id: &str, picture_len: usize) -> ArrangedDevice {
        let display = |n: usize| Display {
            id: format!("{id}-{n}"),
            bounds: Rect::new(n as f64 * 100.0, 0.0, 100.0, 100.0),
            scale: 1.0,
            primary: n == 0,
        };
        ArrangedDevice {
            id: id.to_owned(),
            name: id.to_uppercase(),
            os: Os::Macos,
            offset: None,
            displays: vec![display(0), display(1)],
            // A picture for the first display only, and one for a display
            // that is gone, which is never sent.
            pictures: [
                (format!("{id}-0"), Preview::new(vec![0xff; picture_len])),
                (format!("{id}-9"), Preview::new(vec![0xff; 10])),
            ]
            .into_iter()
            .collect(),
            connected: true,
            this_device: false,
            jump_slot: 0,
        }
    }

    #[test]
    fn previews_go_on_their_screens_within_the_budget() {
        const MIB: usize = 1024 * 1024;
        let arrangement = Arrangement {
            version: 1,
            devices: vec![
                device("a", MIB + MIB / 2),
                device("b", MIB),
                device("c", MIB / 2),
            ],
        };
        let proto = arrangement.to_proto();
        let lens: Vec<Vec<usize>> = proto
            .devices
            .iter()
            .map(|d| d.screens.iter().map(|s| s.preview_jpeg.len()).collect())
            .collect();
        // "b" would take the total past 2 MiB, so it goes without.
        assert_eq!(lens, [[MIB + MIB / 2, 0], [0, 0], [MIB / 2, 0]]);
    }

    #[test]
    fn says_why_relative_mode_ended() {
        let mut status = Status::initial(
            &Config::default(),
            &[],
            true,
            false,
            Hotkeys::default(),
            false,
            FolderAccess::default(),
        );
        let proto = status.to_proto();
        assert_eq!((proto.relative_ends, proto.last_relative_end), (0, None));
        assert!(proto.relative_supported);
        for (end, reason) in [
            (RelativeEnd::TurnedOff, app::RelativeEndReason::TurnedOff),
            (RelativeEnd::Released, app::RelativeEndReason::Released),
            (RelativeEnd::AutoOff, app::RelativeEndReason::AutoOff),
            (RelativeEnd::Jumped, app::RelativeEndReason::Jumped),
            (RelativeEnd::Paused, app::RelativeEndReason::SharingPaused),
            (RelativeEnd::Lost, app::RelativeEndReason::DeviceLost),
            (
                RelativeEnd::Returned,
                app::RelativeEndReason::ControlReturned,
            ),
        ] {
            status.relative_ends += 1;
            status.last_relative_end = Some(RelativeEnded {
                device: "win".to_owned(),
                reason: end,
            });
            let proto = status.to_proto();
            assert_eq!(proto.relative_ends, status.relative_ends);
            assert_eq!(
                proto.last_relative_end,
                Some(app::RelativeEnd {
                    device_id: "win".to_owned(),
                    reason: reason.into(),
                }),
                "{end:?}"
            );
        }
    }
}
