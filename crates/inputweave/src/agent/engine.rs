//! The engine task: the single owner of the router, the receiver and the
//! platform backends. Everything else talks to it through `EngineEvent`s.
//!
//! When each device that told how to wake it was last seen connected is
//! kept in the wake file, through the directory; the status, made anew
//! after every event, captured input too, shows it from the engine's copy
//! (`Engine::last_seen`) rather than taking the directory's lock for each
//! device every time. The engine writes the time only through
//! `Engine::note_last_seen`, which writes the directory and takes the copy
//! anew from it; the directory changes it on its own only as a device is
//! paired, unpaired or tells how to wake it anew, each of which changes the
//! trusted devices, and `Engine::on_peers` takes the copy anew then too. So
//! the copy is only ever what the directory has.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use iw_core::clipboard::{ClipboardMove, ClipboardRelay};
use iw_core::geometry::Point;
use iw_core::hotkey::{Hotkey, HotkeyError, Hotkeys, Modifier};
use iw_core::input::InputEvent;
use iw_core::layout::{Crossing, Device, DeviceId, Display, EdgeSpot, Exit, Layout};
use iw_core::receiver::{Injection, Receiver, RemoteEvent};
use iw_core::router::{self, Action, Event, RelativeStart, Router, RouterConfig};
use iw_core::speed::CursorSpeed;
use iw_keymap::ModifierRemap;
use iw_net::Fingerprint;
use iw_platform::network::NetworkSighting;
use iw_platform::{
    Clipboard, ClipboardContent, ClipboardFormat, ClipboardPromise, Firewall, InputCapture,
    InputEmulation, PermissionState, PlatformError, SessionInfo,
};
use iw_proto::peer::v1::{
    self as peer, ClipboardFilesPull, ClipboardFilesSending, ClipboardOffer, ClipboardPart,
    ClipboardPull, ClipboardRequest, ClipboardSkip, ClipboardSkipped, Control, Datagram, Displays,
    DragItem, Identify, Input, LicenseInfo, LocalActivity, Locate, Placement, PointerAt,
    PointerCaptured, SessionState, WakeInfo, control, datagram, input,
};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tracing::{debug, info, warn};

use super::clipboard_files::{self, Patience, Staging};
use super::directory::{
    Directory, LayoutDoc, OfflineTaken, Peer, TrustedDevices, Unsaved, name_of,
};
use super::drag::{DragEvent, Drags, Effect, MAX_LINKED, REMOTE_DRAG_SINCE};
use super::files::{Carrier, Files, SendError};
use super::folder_access::FolderAccess;
use super::lazy_clipboard::{
    Delivery, FILES_SINCE, FilesWait, Held, OFFERS_SINCE, OfferedFile, OwnOffers, PULL_BIT, Part,
    PendingPulls, PromiseSource, Pulls, Serving, Waiter, busy_backoff, delivery, is_pull, lazy_max,
    offered_files, offered_formats, parts_of,
};
use super::license::{
    self, LicenseError, LicenseFile, LicenseResult, LicenseSaver, LicenseStatus, Loaded,
};
use super::listing::{MAX_ITEMS, Manifest};
use super::local_clipboard::{
    FilesServe, LocalClipboard, LocalDone, LocalOp, Purpose, Serve, Taken, Unpromised, Unwritten,
    WritePurpose,
};
use super::peers::LastHeard;
use super::previews::{self, Pictures, PreviewFiles};
use super::profiles::{self as profile_choice, Reach, Tracker};
use super::status::{
    ArrangedDevice, Arrangement, ClipboardActivity, CursorCrossed, DeviceState, LazyStats,
    OfflineChange, RelativeEnded, RelativeMode, Status, WifiNameHeld,
};
use super::this_device::ThisDevice;
use super::transfer::ClipboardTransfer;
use super::wake::{self, INFO_INTERVAL, WakeData, WakeState, Wakes};
use crate::config::{Config, PlacementConfig};
use crate::convert;
use crate::offline_file::{Entry as OfflineEntry, Full, OfflineDoc, TakenBy};
use crate::profiles_file::{Network, Profile, ProfileEdit, ProfileError, ProfilesFile};

/// Makes new hotkeys of those in effect (`EngineEvent::SetHotkeys`).
pub(crate) type HotkeysChange = Box<dyn FnOnce(&Hotkeys) -> Result<Hotkeys, HotkeyError> + Send>;

/// Says once a change is in the devices file, or why it could not be
/// written (`Engine::save_devices`).
pub(crate) type Saved = oneshot::Receiver<std::io::Result<()>>;

pub(crate) enum EngineEvent {
    Capture(InputEvent),
    /// What the platform poller saw changed.
    Platform(PlatformState),
    /// The platform poller's reading of the network, about once a second,
    /// every one that succeeded (`agent::NetworkReads`).
    Network(Option<NetworkSighting>),
    /// The platform poller's tick, about once a second whatever changed:
    /// injection catches up with the session (`InputEmulation::refresh`),
    /// and capture says whether it still captures
    /// (`InputCapture::capturing`).
    RefreshInjection,
    /// This device was renamed.
    Renamed,
    /// Clipboard sync was turned on or off.
    SetClipboardSync(bool),
    /// Sharing previews of the desktop pictures was turned on or off.
    SetPreviewSharing(bool),
    /// The cursor was locked to the device it is on, or unlocked.
    SetLocked(bool),
    /// How long the cursor pushes against an edge before crossing changed.
    SetEdgeDwell(u32),
    /// The user asked every device to show its name on its screens.
    IdentifyScreens,
    /// The modifier the cursor needs held to cross an edge changed.
    SetSwitchModifier(Option<Modifier>),
    /// The user changed the hotkeys: `change` makes the new ones of those
    /// in effect, or says why they cannot be used. The status shows them
    /// before the reply, which carries, when the devices file is to be
    /// written, what says once it is, or that it could not be.
    SetHotkeys {
        change: HotkeysChange,
        reply: oneshot::Sender<Result<Option<Saved>, HotkeyError>>,
    },
    /// Letting other devices control this Mac at the login window was
    /// turned on or off.
    SetLoginWindowAccess(bool),
    /// The user turned relative mode on or off for the device the cursor
    /// is on.
    SetRelative {
        on: bool,
        reply: oneshot::Sender<Result<(), RelativeError>>,
    },
    /// Devices entering relative mode by themselves when an app there holds
    /// the pointer was turned on or off.
    SetRelativeAuto(bool),
    /// What macOS said of the folders it protects, as they are listed
    /// (`folder_access`).
    FolderAccess(FolderAccess),
    /// The user asked for the cursor on another device, or back here.
    JumpTo {
        device: String,
        reply: oneshot::Sender<Result<(), JumpError>>,
    },
    /// The license changed here; the status shows it before the reply.
    License {
        change: LicenseChange,
        reply: oneshot::Sender<Result<(), LicenseError>>,
    },
    /// The license file that could not be read was read again.
    LicenseRead(Loaded),
    /// The user asked to wake an offline device; the reply comes once the
    /// wake-up was sent, the status showing it being woken, or says why
    /// not.
    Wake {
        device: String,
        reply: oneshot::Sender<Result<(), WakeError>>,
    },
    /// The wake-up numbered `serial` could not be sent to `device`, for
    /// `error`; `reply`, if someone asked for it, hears so once the status
    /// shows it did not wake up.
    WakeNotSent {
        device: String,
        serial: u64,
        error: String,
        reply: Option<oneshot::Sender<Result<(), WakeError>>>,
    },
    /// Waking a device by pushing the cursor toward it was turned on or
    /// off; the status shows it before the reply, which carries, when the
    /// wake file is to be written, what says once it is.
    SetWakeOnPush {
        on: bool,
        reply: oneshot::Sender<Option<Saved>>,
    },
    /// The user took a device offline for now, or brought it back; the
    /// status shows it before the reply, which carries, when the offline
    /// file is to be written, what says once it is.
    SetOfflineForNow {
        device: String,
        offline: bool,
        reply: oneshot::Sender<Result<Option<Saved>, OfflineError>>,
    },
    /// The user changed the profiles, or which is used; the status shows
    /// it before the reply, which carries, when the profiles file is to be
    /// written, what says once it is.
    Profiles {
        change: ProfileChange,
        reply: oneshot::Sender<Result<Option<Saved>, ProfileError>>,
    },
    /// An app read the Wi-Fi name of the network `network` stands for:
    /// `name`, or none when it is not on Wi-Fi or cannot read it
    /// (`ReportWifiName`). Checked already.
    WifiName {
        network: String,
        name: Option<String>,
    },
    /// The user moved devices on the canvas.
    Rearrange {
        moves: Vec<(String, Point)>,
        reply: oneshot::Sender<Result<(), RearrangeError>>,
    },
    PeerUp {
        device: DeviceId,
        fingerprint: Fingerprint,
        session: u64,
        /// The peer protocol version the session speaks.
        version: u32,
        link: mpsc::UnboundedSender<Outbound>,
        /// When the peer was last heard from, as the session keeps it.
        heard: Arc<LastHeard>,
    },
    PeerDown {
        device: DeviceId,
        session: u64,
    },
    FromPeer {
        device: DeviceId,
        session: u64,
        msg: PeerMsg,
    },
    /// A session's latest round-trip time.
    Rtt {
        device: DeviceId,
        session: u64,
        rtt: Duration,
    },
    SetSharing(bool),
    /// The app reports a drag of the files in `manifest` from here.
    BeginDrag {
        manifest: Manifest,
        reply: oneshot::Sender<Result<String, DragError>>,
    },
    /// The app's drag ended without crossing; false if it was not going
    /// on.
    EndDrag {
        id: String,
        reply: oneshot::Sender<bool>,
    },
    /// This device's user dropped a drag another device offered into
    /// `destination`; false if it is not offered here.
    AcceptDrag {
        id: String,
        destination: PathBuf,
        reply: oneshot::Sender<bool>,
    },
    /// This device's user did not drop an offered drag here.
    DeclineDrag {
        id: String,
        reply: oneshot::Sender<bool>,
    },
    /// An app pastes the clipboard offered by `origin` as `offer_id`,
    /// which this device's clipboard promises; `reply` gets the content,
    /// or `None` when it cannot be had.
    PullClipboard {
        origin: DeviceId,
        offer_id: u64,
        reply: std::sync::mpsc::Sender<Option<ClipboardContent>>,
    },
    /// A file manager here pastes the files of the offer `offer_id` from
    /// `origin`, which this device's clipboard promises; `wait` hears
    /// where they are once they arrived. They are fetched into `folder`
    /// when the platform chose one (the Windows app), otherwise into a
    /// folder of the agent's.
    PullClipboardFiles {
        origin: DeviceId,
        offer_id: u64,
        wait: Arc<FilesWait>,
        folder: Option<PathBuf>,
    },
    /// The folder those files are fetched into was made, or could not be.
    StagingMade {
        origin: DeviceId,
        offer_id: u64,
        wait: Arc<FilesWait>,
        made: std::io::Result<PathBuf>,
    },
    /// Something else was copied over that promise, the `serial`th of
    /// the offer.
    PromiseGone {
        origin: DeviceId,
        offer_id: u64,
        serial: u64,
    },
    /// The files on this device's clipboard were looked at, to offer them
    /// with the rest of it as `purpose` asks: those that can go, at the
    /// top only, with their paths.
    ClipboardListed {
        purpose: Purpose,
        content: ClipboardContent,
        change: u64,
        listed: Vec<(PathBuf, DragItem)>,
    },
    /// The files of an offer this device made were listed, the whole of
    /// them, to send them as `pull` asks; or why they cannot go.
    ClipboardFilesListed {
        pull: FilesServe,
        listed: Result<Manifest, ClipboardSkip>,
    },
    /// The files sent under pull `pull_id` for `to`, `items` of its
    /// offer, still wait their turn behind other files sent it: it is told
    /// they still come.
    ClipboardFilesWaiting {
        to: DeviceId,
        pull_id: u64,
        items: Vec<u32>,
    },
    /// Pull `pull_id`, answered busy, is asked again.
    RetryPull {
        pull_id: u64,
    },
    /// The pull of files `pull_id`, answered busy, is asked again.
    RetryFilesPull {
        pull_id: u64,
    },
    /// The files pulled under `pull_id` arrived, or will not. `late`
    /// has where they are when an app gave up waiting for them, so the
    /// clipboard promises them anew.
    ClipboardFilesEnded {
        pull_id: u64,
        late: Option<Vec<Option<PathBuf>>>,
    },
    /// The local clipboard's thread did what it was asked.
    Local(LocalDone),
    Shutdown,
}

/// Why a drag cannot begin, or be answered.
#[derive(Debug, thiserror::Error)]
pub enum DragError {
    #[error(transparent)]
    Send(#[from] super::files::SendError),
    #[error("no mouse button is down on this device")]
    NotDragging,
    /// Another device's mouse drags here: the cursor and its button are
    /// that device's.
    #[error("{0:?} is controlling this device")]
    Controlled(String),
    #[error("this device has no arrangement to drag across yet")]
    Unavailable,
    #[error("no such drag is going on or offered here")]
    NotFound,
    #[error("files cannot go into that folder: {0}")]
    InvalidFolder(String),
    #[error("files are written as the logged-on user, and no user's broker is connected")]
    NoBroker,
    #[error("the agent is stopping")]
    Stopping,
}

impl DragError {
    /// The error code clients are told.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Send(e) => e.code(),
            Self::NotDragging => "not_dragging",
            Self::Controlled(_) => "controlled",
            Self::Unavailable => "unavailable",
            Self::NotFound => "not_found",
            Self::InvalidFolder(_) => "invalid_folder",
            Self::NoBroker => "no_broker",
            Self::Stopping => "agent_stopping",
        }
    }
}

/// How often drags and offers are checked for having lapsed.
const DRAG_TICK: Duration = Duration::from_secs(1);

/// How often a license file that could not be read is read again.
pub(crate) const LICENSE_REREAD: Duration = Duration::from_secs(120);

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RearrangeError {
    #[error("{0:?} is neither this device nor a trusted one")]
    UnknownDevice(String),
    #[error("{0:?} is placed by the configuration file")]
    Configured(String),
    #[error("position of {0:?} is not a finite number")]
    NotFinite(String),
    #[error("{0}")]
    Invalid(String),
}

/// Why the cursor cannot jump to a device.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum JumpError {
    #[error("{0:?} is neither this device nor a trusted one")]
    UnknownDevice(String),
    #[error("local input is not captured, so this device cannot control others")]
    NotCapturing,
    /// The desktop hands this device's keyboard and mouse over only as the
    /// cursor pushes through an edge to another device (Wayland's
    /// InputCapture portal): it cannot be taken from here at will.
    #[error(
        "from this computer the cursor can only leave across an edge: the desktop hands \
         over its keyboard and mouse only there"
    )]
    EdgeOnly,
    /// The request came through input another device sends, as when the
    /// user clicks this device's app from there. Grabbing this device's
    /// idle keyboard and mouse would take nothing anywhere.
    #[error("{0:?} is controlling this device")]
    ControlledBy(String),
    /// The profile in use does not connect that device.
    #[error("the profile in use does not connect {0:?}")]
    NotInProfile(String),
    /// The profile in use is paused: it connects no device.
    #[error("the profile in use is paused")]
    ProfilePaused,
    #[error(transparent)]
    Refused(#[from] router::JumpError),
    #[error("the agent is stopping")]
    Stopping,
}

/// How long the agent waits, as it stops, for the devices, wake or
/// offline file writes still going on (`Engine::finish_saves`).
const SAVES_WAIT: Duration = Duration::from_secs(2);

/// How often when the devices connected now were last heard from is
/// written down (`Engine::stamp_connected`), besides as they go offline and
/// as the agent stops.
const LAST_SEEN_STAMP: Duration = Duration::from_secs(10 * 60);

/// A Wi-Fi name an app read, for the network it says (its id as the app
/// had it from the status). It holds while that network is the one now,
/// until another reading replaces it, the app gone or not; it is used only
/// once [`CONFIRM_READINGS`] readings of the network the agent made after
/// it still show that network, none of them waiting, so a name read on a
/// network the agent has not seen yet never goes to the one before. Never
/// kept in the profiles file but as the user adds the network.
#[derive(Clone, Debug, PartialEq, Eq)]
struct WifiReport {
    network: String,
    name: Option<String>,
    /// Readings since it came that showed its network, none waiting, up to
    /// `CONFIRM_READINGS`.
    readings: u32,
}

impl WifiReport {
    fn confirmed(&self) -> bool {
        self.readings >= CONFIRM_READINGS
    }
}

/// Readings of the network after an app's Wi-Fi name came that must show
/// the same network before it is used: the first may have been taken before
/// the app read the name, the second was not.
const CONFIRM_READINGS: u32 = 2;

/// The longest the agent waits, as it starts, to know the network, and so
/// the profile, before it reaches any device or announces itself
/// (`Engine::hold_until`): the first reading comes at once, and one whose
/// router is not resolved yet settles within `profiles::GRACE`. Should
/// none come (the network cannot be read), it goes on with what it knows.
const HOLD_AT_START: Duration = Duration::from_secs(10);

/// Whether the profiles were kept already (`EngineParts::profiles_kept`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kept {
    /// Not yet: the first network seen goes into Default, and they are
    /// kept from then on.
    No,
    /// In the profiles file, or the login window's copy of it.
    Yes,
    /// The profiles file is there but cannot be read: the profiles start
    /// from the defaults, and the file is left be (no first network, no
    /// save) until the user changes the profiles, which writes it anew.
    Unreadable,
}

/// Why the profile in use is picked anew (`Engine::reselect`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pick {
    /// As the agent starts: no switch.
    Start,
    /// By itself: the network changed, or the profiles did. Counted
    /// (`Status::profile_switches`) when it picks another.
    Automatic,
    /// The user chose a profile, or Automatic: not counted.
    Chosen,
}

/// Why a session's link goes (`Engine::on_peer_down`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gone {
    /// The session ended: the device went offline.
    Offline,
    /// Another session with it took the place of this one.
    Replaced,
    /// It is not trusted any more.
    Untrusted,
}

/// Why a device cannot be woken (`WakeDevice`).
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum WakeError {
    #[error("no trusted device has id {0:?}")]
    Unknown(String),
    #[error("{0:?} is connected")]
    Connected(String),
    #[error("{0:?} never told this device how to wake it")]
    NoWakeData(String),
    #[error("no wake-up could be sent: {0}")]
    NotSent(String),
    /// That device, or this one, is taken offline for now: it is not woken.
    #[error("{0:?} or this device is offline for now")]
    OfflineForNow(String),
    /// The profile in use does not connect that device.
    #[error("the profile in use does not connect {0:?}")]
    NotInProfile(String),
    /// The profile in use is paused.
    #[error("the profile in use is paused")]
    ProfilePaused,
    #[error("the agent is stopping")]
    Stopping,
}

impl WakeError {
    /// The error code clients are told.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unknown(_) => "unknown_device",
            Self::Connected(_) => "connected",
            Self::NoWakeData(_) => "no_wake_data",
            Self::NotSent(_) => "not_sent",
            Self::OfflineForNow(_) => "offline_for_now",
            Self::NotInProfile(_) => "not_in_profile",
            Self::ProfilePaused => "profile_paused",
            Self::Stopping => "agent_stopping",
        }
    }
}

/// Why a device cannot be taken offline for now or brought back
/// (`SetOfflineForNow`).
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum OfflineError {
    #[error("{0:?} is neither this device nor a trusted one")]
    UnknownDevice(String),
    /// This device is offline for now: it brings itself back, and changes
    /// nothing else meanwhile.
    #[error("this device is offline for now; it can only bring itself back")]
    OfflineHere,
    /// As many other devices are offline for now as taking one more allows.
    #[error(
        "{} devices are offline for now already; bring one back first",
        crate::offline_file::MAX_TAKEN
    )]
    TooMany,
    /// The group's list of devices taken offline is full of devices
    /// offline, merged from other devices.
    #[error("the group's list of devices taken offline is full; bring some back first")]
    ListFull,
    /// No change could come after every one listed: only entries no device
    /// takes in could bring that about.
    #[error("no change to the devices taken offline can come after those listed")]
    Exhausted,
    #[error("the agent is stopping")]
    Stopping,
}

impl OfflineError {
    /// The error code clients are told.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownDevice(_) => "unknown_device",
            Self::OfflineHere => "offline_for_now",
            Self::TooMany => "too_many",
            Self::ListFull => "list_full",
            Self::Exhausted => "exhausted",
            Self::Stopping => "agent_stopping",
        }
    }
}

/// Why relative mode cannot be turned on.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RelativeError {
    #[error(transparent)]
    Refused(#[from] router::RelativeError),
    #[error("the agent is stopping")]
    Stopping,
}

/// Why the hotkeys were not changed, or not remembered.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum HotkeysError {
    #[error(transparent)]
    Refused(#[from] HotkeyError),
    /// In effect, but the devices file could not be written: the agent
    /// forgets them as it stops, unless a later change is saved.
    #[error(
        "the hotkeys are in effect but could not be saved, so they are forgotten when the agent stops unless a later change is saved: {0}"
    )]
    NotSaved(String),
    /// Not in effect and saved in time; they may still be.
    #[error("the agent did not put the hotkeys in effect and save them in time")]
    Timeout,
    #[error("the agent is stopping")]
    Stopping,
}

impl HotkeysError {
    /// The error code clients are told.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Refused(e) => e.code(),
            Self::NotSaved(_) => "not_saved",
            Self::Timeout => "timeout",
            Self::Stopping => "agent_stopping",
        }
    }
}

/// A peer cannot make the agent hold an arrangement larger than this.
const MAX_PLACEMENTS: usize = 256;

/// The peer protocol version that knows relative mode (`Input.relative`,
/// `RelativeMotion`, `PointerCaptured`).
pub(crate) const RELATIVE_SINCE: u32 = 5;

/// The peer protocol version that passes licenses between paired devices
/// (`LicenseInfo`).
pub(crate) const LICENSE_SINCE: u32 = 6;

/// The peer protocol version that knows the devices taken offline for now
/// (`OfflineDevices`); a device before it is only told that sharing is
/// paused (`SessionState.sharing_paused`) while either end is offline.
pub(crate) const OFFLINE_SINCE: u32 = 7;

/// A change to the profiles, from the app or `ctl` (`EngineEvent::Profiles`).
#[derive(Clone, Debug)]
pub enum ProfileChange {
    Add(String),
    Edit(String, ProfileEdit),
    Remove(String),
    /// A profile to use whatever the network; none: Automatic.
    Choose(Option<String>),
    /// Lists the network this device is on now in the profile.
    AddNetwork(String),
    RemoveNetwork(String),
    RenameNetwork(String, String),
    WifiNames(bool),
}

/// A change to this computer's license, from the app or `ctl`.
#[derive(Debug)]
pub(crate) enum LicenseChange {
    SetKey(String),
    Record { key: String, result: LicenseResult },
    Remove,
}

/// A round-trip time that moved by no more than this many milliseconds is
/// not reported, so jitter does not flood clients with status events.
const RTT_SLACK_MS: u32 = 2;

/// The cursor is shown at most once in this time, however often a
/// controller asks.
const LOCATE_INTERVAL: Duration = Duration::from_secs(1);

/// How long each device shows its name on its screens when they are
/// identified, in milliseconds.
pub(crate) const IDENTIFY_MS: u32 = 2500;

fn offer_message(
    request_id: u64,
    offer_id: u64,
    origin: &DeviceId,
    change: u64,
    parts: Vec<ClipboardPart>,
    items: Vec<DragItem>,
) -> Outbound {
    Outbound::Control(Control {
        msg: Some(control::Msg::ClipboardOffer(ClipboardOffer {
            request_id,
            offer_id,
            origin_device_id: origin.to_string(),
            origin_change: change,
            parts,
            items,
        })),
    })
}

/// Set, besides [`PULL_BIT`], in the ids of pulls of files, so they never
/// meet those of pulls of content.
const FILES_PULL_BIT: u64 = 1 << 62;

/// A pull of the files of offer `offer_id` under `pull_id`.
fn files_pull_message(pull_id: u64, offer_id: u64) -> Outbound {
    Outbound::Control(Control {
        msg: Some(control::Msg::ClipboardFilesPull(ClipboardFilesPull {
            pull_id,
            offer_id,
        })),
    })
}

/// Sends a pull.
fn pull_message(pull: ClipboardPull) -> Outbound {
    Outbound::Control(Control {
        msg: Some(control::Msg::ClipboardPull(pull)),
    })
}

/// A pull of the offer `held`, for every format it promises; its id comes
/// from `Pulls::start`.
fn pull_of(held: &Held) -> ClipboardPull {
    ClipboardPull {
        pull_id: 0,
        offer_id: held.offer_id,
        origin_device_id: held.origin.to_string(),
        mimes: held.formats.iter().map(|f| f.mime().to_owned()).collect(),
    }
}

fn skipped_message(id: u64, reason: ClipboardSkip) -> Outbound {
    Outbound::Control(Control {
        msg: Some(control::Msg::ClipboardSkipped(ClipboardSkipped {
            id,
            reason: reason.into(),
        })),
    })
}

/// Permissions, secure input, the lock screen, displays and their desktop
/// pictures, as the platform reports them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PlatformState {
    pub(crate) permissions: Vec<PermissionState>,
    pub(crate) secure_input: bool,
    pub(crate) screen_locked: bool,
    /// Input from other devices reaches this device's lock screen.
    pub(crate) lock_screen_input: bool,
    /// Input from other devices reaches this device at all now.
    pub(crate) input_available: bool,
    pub(crate) displays: Vec<Display>,
    pub(crate) pictures: Pictures,
    /// Whether anything reads this device's desktop pictures now: on
    /// Windows, whether the broker is connected.
    pub(crate) pictures_available: bool,
    /// An app here holds the pointer, as games do; other devices are told.
    pub(crate) pointer_captured: bool,
    /// What kind of session this is and what serves it (Linux).
    pub(crate) session: Option<SessionInfo>,
    /// Whether a firewall lets other devices reach the agent's port, and
    /// the port.
    pub(crate) firewall: (Firewall, u16),
}

pub(crate) enum PeerMsg {
    Control(Control),
    Input(Input),
    Datagram(Datagram),
    Clipboard(ClipboardTransfer),
    /// The answer to pull `0` was refused or broke off on its way.
    PullBroken(u64),
}

/// Something to send to a peer, on the channel matching its kind.
pub(crate) enum Outbound {
    Control(Control),
    Input(Input),
    Datagram(Datagram),
    /// Sent on a transfer stream of its own.
    Clipboard(ClipboardTransfer),
}

struct Link {
    session: u64,
    /// The certificate the session was authenticated with. If the device is
    /// unpaired, or paired again with another certificate, the session ends.
    fingerprint: Fingerprint,
    tx: mpsc::UnboundedSender<Outbound>,
    /// Sequence numbers for unreliable datagrams; never reset while the
    /// session lasts, so a late datagram can never look newer.
    motion_seq: u64,
    scroll_seq: u64,
    /// The round-trip time as last reported to clients, in milliseconds.
    rtt_ms: u32,
    /// The device's screen is locked, as it last said.
    screen_locked: bool,
    /// Its locked screen takes no input from here, as it last said; a
    /// device that does not say is reached as before.
    lock_screen_blocked: bool,
    /// No input from here reaches it now, locked or not, as it last said.
    input_unavailable: bool,
    /// It paused sharing, as it last said.
    sharing_paused: bool,
    /// The peer protocol version the session speaks.
    version: u32,
    /// Numbers relative movements sent in the session; never reset either.
    relative_seq: u64,
    /// Numbers the relative modes begun in the session.
    relative_session: u64,
    /// How far the pointer moved in relative mode, since it last began.
    relative_total: Point,
    /// An app on it holds the pointer, as it last said.
    pointer_captured: bool,
    /// When it last said how to wake it, as taken.
    wake_info_at: Option<Instant>,
    /// It sent the devices taken offline for now in this session already:
    /// its first, as the session starts, needs no answer, this device's
    /// having gone to it then.
    offline_heard: bool,
    /// When it was last heard from, by the wall clock, as the session keeps
    /// it.
    heard: Arc<LastHeard>,
}

impl Link {
    /// Its screen is locked where input from here cannot reach it, so the
    /// cursor stays off it.
    fn out_of_reach(&self) -> bool {
        self.screen_locked && self.lock_screen_blocked
    }
}

/// What the engine is built from.
pub(crate) struct EngineParts {
    pub(crate) config: Arc<Config>,
    pub(crate) this: Arc<ThisDevice>,
    pub(crate) directory: Arc<Directory>,
    pub(crate) capture: Box<dyn InputCapture>,
    /// Whether input from other devices reaches this device as the agent
    /// starts (`SessionStatus::input_available`), so devices that connect
    /// before the platform poller first reports are told the truth.
    pub(crate) input_available: bool,
    /// Where capture delivers input.
    pub(crate) capture_tx: std::sync::mpsc::Sender<InputEvent>,
    pub(crate) emulation: Box<dyn InputEmulation>,
    pub(crate) local_displays: Vec<Display>,
    pub(crate) preview_sharing: bool,
    /// Tell other devices to keep the pictures they have of this one
    /// (`Displays.previews_unchanged`), as the login window's agent does.
    pub(crate) previews_unchanged: bool,
    /// Whether other devices may control this Mac at the login window.
    pub(crate) login_window_access: bool,
    /// Whether the platform reads this device's desktop pictures at all,
    /// at first; the platform poller reports changes.
    pub(crate) desktop_pictures: bool,
    /// Where other devices' pictures are kept.
    pub(crate) preview_files: PreviewFiles,
    /// This computer's license, if the agent keeps one, and what saves it
    /// to its file (none: kept in memory).
    pub(crate) license: Option<LicenseFile>,
    pub(crate) license_saver: Option<LicenseSaver>,
    /// The license file, when it is there but could not be read: the
    /// license is unavailable (`license` is none) until a read of it,
    /// every `license_reread`, succeeds.
    pub(crate) license_unavailable: Option<PathBuf>,
    pub(crate) license_reread: Duration,
    pub(crate) status: watch::Sender<Status>,
    /// The generation of the trusted devices (`Directory::peers_now`) the
    /// status shows, sent after the status: apart from it, so a change to
    /// the devices that shows nothing new sends apps no status.
    pub(crate) peers_applied: watch::Sender<u64>,
    pub(crate) arrangement: watch::Sender<Arrangement>,
    /// Where IPC clients learn where to show the cursor.
    pub(crate) located: watch::Sender<Option<Point>>,
    /// Where IPC clients learn to show this device's name on its screens,
    /// and for how many milliseconds.
    pub(crate) identified: watch::Sender<Option<u32>>,
    /// Where IPC clients learn of the relative mode hotkey pressed on a
    /// device that cannot take relative movements.
    pub(crate) relative_unsupported: watch::Sender<Option<String>>,
    /// Where IPC clients learn that the cursor crossed an edge of this
    /// device's displays.
    pub(crate) cursor_crossed: broadcast::Sender<CursorCrossed>,
    pub(crate) clipboard: Option<Box<dyn Clipboard>>,
    pub(crate) clipboard_on: bool,
    pub(crate) clipboard_max: usize,
    pub(crate) edge_dwell_ms: u32,
    pub(crate) switch_modifier: Option<Modifier>,
    pub(crate) hotkeys: Hotkeys,
    /// Sends the files a drag carries.
    pub(crate) files: Arc<Files>,
    /// Where IPC clients hear about drags.
    pub(crate) drag_events: broadcast::Sender<DragEvent>,
    /// Apps connected; with none, drags offered here are declined.
    pub(crate) ui_clients: Arc<AtomicUsize>,
    /// The engine's own events, for the clipboard's promises to pull
    /// through and its thread to report on; weak, so they never keep it
    /// running.
    pub(crate) events: mpsc::WeakUnboundedSender<EngineEvent>,
    /// Pulls sent and not answered, which transfer streams check.
    pub(crate) pulls: PendingPulls,
    /// Where files pasted here are fetched into.
    pub(crate) clipboard_staging: Option<Staging>,
    /// How long an app pasting files waits for them.
    pub(crate) paste_wait: Duration,
    /// How long files pasted here may take to come.
    pub(crate) paste_patience: Patience,
    /// Devices enter relative mode by themselves when an app there holds
    /// the pointer.
    pub(crate) relative_auto: bool,
    /// The folders macOS protects, before any was listed.
    pub(crate) folder_access: FolderAccess,
    /// Drags of files offered here are declined (Linux).
    pub(crate) refuses_drags: bool,
    /// Pushing the cursor toward an offline device wakes it.
    pub(crate) wake_on_push: bool,
    /// How long a device being woken has to connect.
    pub(crate) wake_wait: Duration,
    /// The profiles, and whether they were kept already (a profiles file,
    /// or the login window's copy of one): until then, the first network
    /// seen goes into Default.
    pub(crate) profiles: ProfilesFile,
    pub(crate) profiles_kept: Kept,
    /// Where the dialer, the accept loop and discovery learn what the
    /// profile in use lets this device reach.
    pub(crate) reach: watch::Sender<Reach>,
    /// Every this many relative movements one is not sent, as if lost; 0
    /// sends them all. For tests.
    #[cfg(feature = "test-hooks")]
    pub(crate) lose_relative_motion: u64,
    /// Where wake-ups go in place of every broadcast address. For tests.
    #[cfg(feature = "test-hooks")]
    pub(crate) wake_target: Option<std::net::SocketAddr>,
    /// Where the cursor crossed is not sent to other devices, as an older
    /// agent does not. For tests.
    #[cfg(feature = "test-hooks")]
    pub(crate) omit_crossing_spots: bool,
}

/// A pull of the files of an offer, for an app pasting them here.
struct FilesPull {
    origin: DeviceId,
    offer_id: u64,
    /// The items promised here, by their place in the offer.
    promised: Vec<u32>,
    /// The items the origin says it sends; dropped, the pull came to
    /// nothing.
    expected: watch::Sender<Option<Vec<u32>>>,
    /// How long it waited the last time the origin answered busy.
    backoff: Option<Duration>,
}

/// The engine's part in clipboard sync: reading and writing the local
/// clipboard, and remembering requests whose answers it passes on.
struct ClipboardSync {
    /// The local clipboard, on a thread of its own.
    local: Option<LocalClipboard>,
    /// Whether sync is on here, as the user's switch says.
    on: bool,
    /// Whether the local clipboard was last told sync is on: the user's
    /// switch and the profile in use's together (`sync_local_clipboard`).
    told: bool,
    max: usize,
    relay: ClipboardRelay,
    next_request: u64,
    /// Requests sent to the device the cursor left, by id, with where the
    /// answer goes.
    requests: HashMap<u64, DeviceId>,
    last: Option<ClipboardActivity>,
    /// Offers of this device's clipboard, which pulls must name.
    own_offers: OwnOffers,
    /// Pulls sent, for apps here or passed on for other devices.
    pulls: Pulls,
    /// Pulls of this device's clipboard being read.
    serving: Serving,
    /// The offer this device's clipboard holds.
    held: Option<Held>,
    lazy_stats: LazyStats,
    /// Where files pasted here are fetched into.
    staging: Option<Staging>,
    /// Pulls of files sent for apps here, by id.
    file_pulls: HashMap<u64, FilesPull>,
    next_files_pull: u64,
    /// Offers whose files were fetched into a folder here, by origin; the
    /// folder goes when the promise does.
    staged: HashSet<(DeviceId, u64)>,
    /// The last promise's serial.
    serial: u64,
    /// The serial of the promise the clipboard took last, and its change
    /// count then: a promise made anew replaces it only if nothing was
    /// written or copied since.
    promised: Option<(u64, u64)>,
    /// What answers for the promise made last, so one made anew keeps
    /// what it pulled.
    source: Option<Arc<PromiseSource>>,
    /// How long an app pasting files waits for them.
    paste_wait: Duration,
    /// How long files pasted here may take, and how often this device
    /// says files pasted elsewhere still come.
    patience: Patience,
}

pub(crate) struct Engine {
    config: Arc<Config>,
    this: Arc<ThisDevice>,
    directory: Arc<Directory>,
    /// Trusted devices, as of the last directory change.
    peers: Arc<Vec<Peer>>,
    /// Their generation (`Directory::peers_now`).
    peers_generation: u64,
    local: DeviceId,
    capture: Box<dyn InputCapture>,
    /// Where capture delivers input; kept to start capture again once its
    /// permission is granted.
    capture_tx: std::sync::mpsc::Sender<InputEvent>,
    /// Whether capture is running. Without it this device can still be
    /// controlled, but cannot control others.
    capturing: bool,
    /// Capture is starting, or stopped by itself (`InputCapture::capturing`):
    /// started again at every platform tick until it starts
    /// (`follow_capture`), even if no tick saw its permission lapse.
    restart_capture: bool,
    permissions: Vec<PermissionState>,
    secure_input: bool,
    /// This device's screen is locked; other devices are told.
    screen_locked: bool,
    /// Input from other devices reaches this device's lock screen; other
    /// devices are told.
    lock_screen_input: bool,
    /// Input from other devices reaches this device at all now; other
    /// devices are told.
    input_available: bool,
    emulation: Box<dyn InputEmulation>,
    /// Injection that failed, reported once in a while rather than each
    /// time.
    injection_failures: InjectionFailures,
    local_displays: Vec<Display>,
    /// Previews of this device's desktop pictures, by display id.
    local_pictures: Pictures,
    /// Whether `local_pictures` go to other devices.
    preview_sharing: bool,
    /// Other devices keep the pictures they have of this one.
    previews_unchanged: bool,
    /// Other devices may control this Mac at the login window.
    login_window_access: bool,
    /// Whether the platform reads desktop pictures at all, for the status.
    desktop_pictures: bool,
    router: Option<Router>,
    /// What the router is built with; it only exists once the layout does.
    router_config: RouterConfig,
    /// Where the cursor leaves this device, as capture was last told.
    exits: Vec<Exit>,
    /// How many events capture reported, to tell it which one the router's
    /// answer is about (`InputCapture::edge_wait`).
    capture_events: u64,
    receiver: Receiver,
    sharing: bool,
    /// Kept here too, since the router may not exist yet.
    locked: bool,
    links: HashMap<DeviceId, Link>,
    peer_displays: HashMap<DeviceId, Vec<Display>>,
    /// Other devices' desktop pictures as they last sent them, kept in
    /// `preview_files` too.
    peer_pictures: HashMap<DeviceId, Pictures>,
    preview_files: PreviewFiles,
    /// This computer's license, where the agent keeps one, what saves it
    /// (trying again until it can), and what it last gave that to save.
    license: Option<LicenseFile>,
    license_saver: Option<LicenseSaver>,
    license_queued: Option<LicenseFile>,
    /// The license file, while it cannot be read: the license is
    /// unavailable, nothing is taken from or told to paired devices, and
    /// nothing is saved. Read again every `license_reread`, off this task
    /// (`license_reading` while a read is out).
    license_unavailable: Option<PathBuf>,
    license_reread: Duration,
    license_reading: bool,
    /// What paired devices said of their license while it was
    /// unavailable, the latest from each, taken in once it is read.
    license_heard: HashMap<DeviceId, LicenseInfo>,
    /// The license as the status shows it, rebuilt when it or the trusted
    /// devices change (`refresh_license_status`).
    license_status: Option<LicenseStatus>,
    /// What paired devices were last told of the license
    /// (`LicenseFile::outgoing`).
    license_sent: (String, bool),
    /// Devices in the current layout.
    arranged: HashSet<DeviceId>,
    /// The devices the jump hotkeys go to, the first by digit 1.
    jump_order: Vec<DeviceId>,
    last_return: Option<iw_core::router::ReturnReason>,
    /// How many times control came back to this device.
    returns: u64,
    /// Why relative mode last ended, and how many times it did.
    last_relative_end: Option<RelativeEnded>,
    relative_ends: u64,
    status: watch::Sender<Status>,
    peers_applied: watch::Sender<u64>,
    arrangement: watch::Sender<Arrangement>,
    located: watch::Sender<Option<Point>>,
    /// When the cursor was last shown on this device.
    last_located: Option<Instant>,
    identified: watch::Sender<Option<u32>>,
    relative_unsupported: watch::Sender<Option<String>>,
    cursor_crossed: broadcast::Sender<CursorCrossed>,
    /// When each device, this one included, last had the screens
    /// identified here.
    last_identified: HashMap<DeviceId, Instant>,
    clipboard: ClipboardSync,
    /// Files dragged from here, and offered here.
    drags: Drags,
    files: Arc<Files>,
    drag_events: broadcast::Sender<DragEvent>,
    ui_clients: Arc<AtomicUsize>,
    events: mpsc::WeakUnboundedSender<EngineEvent>,
    /// An app here holds the pointer; other devices are told.
    pointer_captured: bool,
    /// Kept here too, since the router may not exist yet.
    relative_auto: bool,
    folder_access: FolderAccess,
    /// What kind of session this is and what serves it, as the platform
    /// last said (Linux).
    session_info: Option<SessionInfo>,
    /// Drags of files offered here are declined (Linux).
    refuses_drags: bool,
    /// Whether a firewall lets other devices reach the agent, and its port.
    firewall: (Firewall, u16),
    /// Devices being woken, or that did not wake up.
    wakes: Wakes,
    /// When each device that told how to wake it was last seen connected:
    /// the directory's, as of the last change (see the module docs).
    last_seen: HashMap<String, SystemTime>,
    /// Kept here too, since the router may not exist yet.
    wake_on_push: bool,
    wake_wait: Duration,
    /// The group's document of the devices taken offline for now: the
    /// directory's, as of the last change, which only the engine makes.
    offline: OfflineDoc,
    /// How many devices were taken offline or brought back from here, and
    /// times another device took this one offline, and the last of them,
    /// for the apps to tell.
    offline_changes: u64,
    last_offline_change: Option<OfflineChange>,
    /// The profiles, which only the engine changes; whether they were kept
    /// already (see `EngineParts::profiles_kept`).
    profiles: ProfilesFile,
    profiles_kept: Kept,
    /// The network as the platform last read it, and as taken.
    reading: Option<NetworkSighting>,
    tracker: Tracker,
    /// The Wi-Fi name an app last read (`WifiReport`), and one that came
    /// while the network was being waited on, taken once the wait ends if
    /// its network is the one then.
    wifi_report: Option<WifiReport>,
    wifi_held: Option<(String, Option<String>)>,
    /// Until when nothing is reached, as the agent starts: until the first
    /// reading of the network that settles (`Tracker::waiting`), or this,
    /// whichever comes first (`HOLD_AT_START`). The profile is then put in
    /// effect, which is no switch the apps tell.
    hold_until: Option<Instant>,
    /// The profile in use, and whether a network was known as it was
    /// picked.
    active: String,
    network_known: bool,
    /// How many times the profile in use changed by itself.
    profile_switches: u64,
    /// The network and profiles as the status shows them, made anew only
    /// as they change (`show_profiles`), not for every status.
    network_shown: Option<Network>,
    profiles_shown: Arc<Vec<Profile>>,
    reach: watch::Sender<Reach>,
    /// Writes of the devices, wake, offline or profiles file going on
    /// (`save_devices`).
    saves: tokio::task::JoinSet<()>,
    #[cfg(feature = "test-hooks")]
    lose_relative_motion: u64,
    #[cfg(feature = "test-hooks")]
    omit_crossing_spots: bool,
    #[cfg(feature = "test-hooks")]
    wake_target: Option<std::net::SocketAddr>,
}

impl Engine {
    pub(crate) fn new(parts: EngineParts) -> Self {
        let EngineParts {
            config,
            this,
            directory,
            capture,
            capture_tx,
            emulation,
            local_displays,
            preview_sharing,
            previews_unchanged,
            login_window_access,
            desktop_pictures,
            preview_files,
            license,
            license_saver,
            license_unavailable,
            license_reread,
            status,
            peers_applied,
            arrangement,
            located,
            identified,
            relative_unsupported,
            cursor_crossed,
            clipboard,
            clipboard_on,
            clipboard_max,
            edge_dwell_ms,
            switch_modifier,
            hotkeys,
            files,
            drag_events,
            ui_clients,
            events,
            pulls,
            clipboard_staging,
            paste_wait,
            paste_patience,
            relative_auto,
            folder_access,
            refuses_drags,
            input_available,
            wake_on_push,
            wake_wait,
            profiles,
            profiles_kept,
            reach,
            #[cfg(feature = "test-hooks")]
            lose_relative_motion,
            #[cfg(feature = "test-hooks")]
            omit_crossing_spots,
            #[cfg(feature = "test-hooks")]
            wake_target,
        } = parts;
        if let Some(staging) = &clipboard_staging {
            staging.clean_old();
        }
        let local = DeviceId::new(&config.device.id);
        let relay_device = local.clone();
        let (peers, peers_generation) = directory.peers_now();
        let offline = directory.offline();
        // Devices seen before keep their displays and pictures while
        // offline, so the arrangement can still show them.
        let peer_displays: HashMap<DeviceId, Vec<Display>> = peers
            .iter()
            .filter_map(|p| {
                let displays = directory.known_displays(&p.id)?;
                Some((DeviceId::new(&p.id), displays))
            })
            .collect();
        preview_files.prune(peers.iter().map(|p| p.id.as_str()));
        let peer_pictures = peers
            .iter()
            .filter_map(|p| {
                let id = DeviceId::new(&p.id);
                let displays = peer_displays.get(&id)?;
                let pictures = preview_files.load(&p.id, displays.iter().map(|d| d.id.as_str()));
                (!pictures.is_empty()).then_some((id, pictures))
            })
            .collect();
        let mut clipboard = clipboard;
        if let Some(clipboard) = clipboard.as_mut() {
            clipboard.set_sync(clipboard_on);
        }
        let mut router_config = RouterConfig::default();
        router_config.edge.dwell_ms = edge_dwell_ms;
        router_config.edge.switch_modifier = switch_modifier;
        router_config.hotkeys = hotkeys;
        let mut capture = capture;
        router_config.hotkeys_held_back = capture.reserve_hotkeys(&hotkeys.reserved());
        capture.watch_hotkeys(&acted_on(&hotkeys));
        capture.watch_double_press(hotkeys.take_back_double);
        // As `start` showed it.
        let license_status = status.borrow().license.clone();
        let mut engine = Self {
            config,
            this,
            directory,
            peers,
            peers_generation,
            local,
            capture,
            capture_tx,
            capturing: false,
            restart_capture: false,
            permissions: Vec::new(),
            secure_input: false,
            screen_locked: false,
            lock_screen_input: false,
            input_available,
            emulation,
            injection_failures: InjectionFailures::default(),
            local_displays,
            // Read by the platform poller once the agent is up.
            local_pictures: Pictures::new(),
            preview_sharing,
            previews_unchanged,
            login_window_access,
            desktop_pictures,
            router: None,
            router_config,
            exits: Vec::new(),
            capture_events: 0,
            receiver: Receiver::default(),
            sharing: true,
            locked: false,
            links: HashMap::new(),
            peer_displays,
            peer_pictures,
            preview_files,
            license_sent: license
                .as_ref()
                .map(LicenseFile::outgoing)
                .unwrap_or_default(),
            license_queued: license.clone(),
            license_status,
            license_unavailable,
            license_reread,
            license_reading: false,
            license_heard: HashMap::new(),
            license,
            license_saver,
            arranged: HashSet::new(),
            jump_order: Vec::new(),
            last_return: None,
            returns: 0,
            last_relative_end: None,
            relative_ends: 0,
            status,
            peers_applied,
            arrangement,
            located,
            last_located: None,
            identified,
            relative_unsupported,
            cursor_crossed,
            last_identified: HashMap::new(),
            clipboard: ClipboardSync {
                local: clipboard.map(|c| LocalClipboard::start(c, events.clone())),
                on: clipboard_on,
                told: clipboard_on,
                max: clipboard_max,
                relay: ClipboardRelay::new(relay_device),
                next_request: 1,
                requests: HashMap::new(),
                last: None,
                own_offers: OwnOffers::default(),
                pulls: Pulls::new(pulls),
                serving: Serving::default(),
                held: None,
                lazy_stats: LazyStats::default(),
                staging: clipboard_staging,
                file_pulls: HashMap::new(),
                next_files_pull: 0,
                staged: HashSet::new(),
                serial: 0,
                promised: None,
                source: None,
                paste_wait,
                patience: paste_patience,
            },
            drags: Drags::default(),
            files,
            drag_events,
            ui_clients,
            events,
            pointer_captured: false,
            relative_auto,
            folder_access,
            session_info: None,
            refuses_drags,
            firewall: (Firewall::Unknown, 0),
            wakes: Wakes::default(),
            last_seen: HashMap::new(),
            wake_on_push,
            wake_wait,
            offline,
            offline_changes: 0,
            last_offline_change: None,
            tracker: Tracker::new(&profiles.salt),
            reading: None,
            wifi_report: None,
            wifi_held: None,
            hold_until: Some(Instant::now() + HOLD_AT_START),
            active: profile_choice::select(&profiles, None).to_owned(),
            network_known: false,
            profiles,
            profiles_kept,
            profile_switches: 0,
            network_shown: None,
            profiles_shown: Arc::new(Vec::new()),
            reach,
            saves: tokio::task::JoinSet::new(),
            #[cfg(feature = "test-hooks")]
            lose_relative_motion,
            #[cfg(feature = "test-hooks")]
            omit_crossing_spots,
            #[cfg(feature = "test-hooks")]
            wake_target,
        };
        if engine.offline_here() {
            info!("this device is offline for now, as the group last said");
        }
        // The profile as the agent starts, which is no switch.
        engine.reselect(Pick::Start);
        engine.take_last_seen();
        engine.start_capture();
        engine.rebuild_layout();
        engine.publish();
        engine
    }

    /// Starts capture if it is not running. Without permission it stays off
    /// until the platform poller reports the permission granted.
    fn start_capture(&mut self) {
        if self.capturing {
            return;
        }
        // Tried again every tick: said once.
        let again = self.restart_capture;
        match self.capture.start(self.capture_tx.clone()) {
            Ok(()) => {
                info!("capturing local input");
                self.capturing = true;
                self.restart_capture = false;
            }
            // What the user allowed before is starting (a Wayland portal
            // session at login): started as soon as it runs, which no
            // permission may show, as the backend sends no input before.
            Err(PlatformError::Starting(what)) => {
                if !again {
                    debug!(what, "local input cannot be captured yet");
                }
                self.restart_capture = true;
            }
            Err(_) if again => {}
            Err(PlatformError::PermissionDenied(permission)) => {
                warn!(?permission, "cannot capture local input without permission");
            }
            Err(e) => warn!(error = %e, "cannot capture local input"),
        }
    }

    /// Follows capture that stopped by itself, as a Wayland portal session
    /// the user or the desktop ended, or that is starting: started again
    /// as soon as it can be, as when its permission is granted again
    /// (`on_platform`) or the session comes back by itself.
    fn follow_capture(&mut self) {
        if self.capturing && !self.capture.capturing() {
            info!("local input capture stopped");
            self.capturing = false;
            self.restart_capture = true;
            // Whatever it held, it holds no more: control comes back. Not
            // capture's own report, so no answer about it goes back.
            self.route(Event::Input(InputEvent::CaptureEnded));
        } else if !self.capturing && self.restart_capture {
            self.start_capture();
        }
    }

    fn on_platform(&mut self, state: PlatformState) {
        let newly_granted = state.permissions.iter().any(|now| {
            now.granted
                && !self
                    .permissions
                    .iter()
                    .any(|before| before.permission == now.permission && before.granted)
        });
        self.permissions = state.permissions;
        self.secure_input = state.secure_input;
        let mut session_changed = false;
        if state.screen_locked != self.screen_locked
            || state.lock_screen_input != self.lock_screen_input
        {
            info!(
                locked = state.screen_locked,
                takes_input = state.lock_screen_input,
                "the screen was locked or unlocked"
            );
            self.screen_locked = state.screen_locked;
            self.lock_screen_input = state.lock_screen_input;
            session_changed = true;
        }
        if state.input_available != self.input_available {
            info!(
                available = state.input_available,
                "input from other devices can reach this device, or no longer"
            );
            self.input_available = state.input_available;
            session_changed = true;
        }
        // One message for whatever of it changed in this tick.
        if session_changed {
            self.tell_session(|_, _| true);
        }
        if state.pointer_captured != self.pointer_captured {
            debug!(
                captured = state.pointer_captured,
                "an app holds the pointer or let go of it"
            );
            self.pointer_captured = state.pointer_captured;
            let msg = self.pointer_message();
            for link in self.links.values().filter(|l| l.version >= RELATIVE_SINCE) {
                let _ = link.tx.send(Outbound::Control(msg.clone()));
            }
        }
        if newly_granted {
            self.start_capture();
        }
        if state.pictures_available != self.desktop_pictures {
            info!(
                available = state.pictures_available,
                "reading the desktop pictures changed"
            );
            self.desktop_pictures = state.pictures_available;
        }
        if state.session != self.session_info {
            debug!(session = ?state.session, "what serves the session changed");
            self.session_info = state.session;
        }
        if state.firewall != self.firewall {
            info!(firewall = ?state.firewall.0, port = state.firewall.1, "the firewall changed");
            self.firewall = state.firewall;
        }
        if state.displays.is_empty() {
            return;
        }
        let displays_changed = state.displays != self.local_displays;
        let pictures_changed = state.pictures != self.local_pictures;
        if displays_changed {
            info!(count = state.displays.len(), "displays changed");
            self.local_displays = state.displays;
        }
        if pictures_changed {
            info!(count = state.pictures.len(), "desktop pictures changed");
            self.local_pictures = state.pictures;
        }
        if displays_changed || (pictures_changed && self.preview_sharing) {
            self.send_displays();
        }
        if displays_changed {
            self.rebuild_layout();
        } else if pictures_changed {
            self.publish_arrangement();
        }
    }

    fn set_preview_sharing(&mut self, on: bool) {
        if self.preview_sharing == on {
            return;
        }
        info!(on, "sharing desktop pictures changed");
        self.preview_sharing = on;
        self.send_displays();
    }

    /// Tells every connected device about this device's displays again.
    fn send_displays(&self) {
        self.broadcast(self.displays_message());
    }

    /// Sends `msg` to every connected device.
    fn broadcast(&self, msg: Control) {
        for link in self.links.values() {
            let _ = link.tx.send(Outbound::Control(msg.clone()));
        }
    }

    /// Another device's session changed. While its screen is locked where
    /// input from here cannot reach it, the router keeps the cursor off it,
    /// and gives control back if it had it.
    fn on_session_state(&mut self, from: DeviceId, state: SessionState) {
        let Some(link) = self.links.get_mut(&from) else {
            return;
        };
        if link.input_unavailable != state.input_unavailable {
            link.input_unavailable = state.input_unavailable;
            if state.input_unavailable {
                info!(device = %from, "input from here cannot reach it now");
            }
            self.route(Event::PeerInputUnavailable(
                from.clone(),
                state.input_unavailable,
            ));
        }
        let Some(link) = self.links.get_mut(&from) else {
            return;
        };
        if link.sharing_paused != state.sharing_paused {
            link.sharing_paused = state.sharing_paused;
            info!(device = %from, paused = state.sharing_paused, "it paused sharing, or resumed it");
            self.route(Event::PeerSharingPaused(from.clone(), state.sharing_paused));
        }
        let Some(link) = self.links.get_mut(&from) else {
            return;
        };
        if link.screen_locked == state.screen_locked
            && link.lock_screen_blocked == state.lock_screen_blocked
        {
            return;
        }
        debug!(
            device = %from,
            locked = state.screen_locked,
            blocked = state.lock_screen_blocked,
            "its screen was locked or unlocked"
        );
        let was_out_of_reach = link.out_of_reach();
        link.screen_locked = state.screen_locked;
        link.lock_screen_blocked = state.lock_screen_blocked;
        let out_of_reach = link.out_of_reach();
        if out_of_reach != was_out_of_reach {
            if out_of_reach {
                info!(device = %from, "its screen locked, and input cannot reach it");
            }
            self.route(Event::PeerScreenLocked(from, out_of_reach));
        }
    }

    /// Whether an app here holds the pointer, for other devices.
    fn pointer_message(&self) -> Control {
        Control {
            msg: Some(control::Msg::PointerCaptured(PointerCaptured {
                captured: self.pointer_captured,
            })),
        }
    }

    /// What this device tells paired devices of its license: its key, or
    /// none, and whether it is licensed with it.
    fn license_message(&self) -> Control {
        let (key, licensed) = self.license_sent.clone();
        Control {
            msg: Some(control::Msg::LicenseInfo(LicenseInfo { key, licensed })),
        }
    }

    /// The user or the app changed the license here.
    fn change_license(&mut self, change: LicenseChange) -> Result<(), LicenseError> {
        if self.license_unavailable.is_some() {
            return Err(LicenseError::Unavailable);
        }
        let Some(license) = self.license.as_mut() else {
            return Err(LicenseError::Unsupported);
        };
        match change {
            LicenseChange::SetKey(key) => {
                license.set_key(&key)?;
                info!("a license key was entered here");
            }
            LicenseChange::Record { key, result } => {
                license.record(&key, result)?;
                info!(state = ?license.state, "the app said what Polar answered about the license");
            }
            LicenseChange::Remove => {
                license.remove();
                info!("the license was removed here; keys from paired devices are not taken");
            }
        }
        self.adopt_license();
        self.license_changed();
        Ok(())
    }

    /// Reads the license file again, off this task, while it cannot be
    /// read; the engine hears back in `EngineEvent::LicenseRead`.
    fn reread_license(&mut self) {
        let Some(path) = self.license_unavailable.clone() else {
            return;
        };
        if self.license_reading {
            return;
        }
        self.license_reading = true;
        let events = self.events.clone();
        tokio::spawn(async move {
            let loaded = tokio::task::spawn_blocking(move || LicenseFile::load_or_set_aside(&path))
                .await
                .unwrap_or(Loaded::Unreadable);
            if let Some(engine) = events.upgrade() {
                let _ = engine.send(EngineEvent::LicenseRead(loaded));
            }
        });
    }

    /// The license file was read again. Once it could be, the license is
    /// available: what it holds is shown and told to paired devices, and
    /// what they said meanwhile is taken in.
    fn on_license_read(&mut self, loaded: Loaded) {
        self.license_reading = false;
        if self.license_unavailable.is_none() {
            return;
        }
        let file = match loaded {
            Loaded::Unreadable => return,
            Loaded::File(file) => file,
            // Damaged after all, and moved aside: no license.
            Loaded::SetAside => LicenseFile::default(),
        };
        info!("the license file can be read now");
        self.license_unavailable = None;
        self.license_queued = Some(file.clone());
        self.license_sent = file.outgoing();
        self.license = Some(file);
        // Paired devices heard nothing of it in this session.
        let msg = self.license_message();
        for link in self.links.values().filter(|l| l.version >= LICENSE_SINCE) {
            let _ = link.tx.send(Outbound::Control(msg.clone()));
        }
        for (from, info) in std::mem::take(&mut self.license_heard) {
            self.on_license_info(from, info);
        }
        self.adopt_license();
        self.license_changed();
    }

    /// Takes a paired device's license key, if the rules say so.
    fn adopt_license(&mut self) {
        if let Some(license) = self.license.as_mut()
            && license.adopt(|id| name_of(&self.peers, id))
        {
            info!(
                device = license.source_device_id,
                "took the license key of a paired device"
            );
        }
    }

    /// Builds the license as the status shows it again, after it or the
    /// trusted devices changed, rather than for every status published.
    fn refresh_license_status(&mut self) {
        self.license_status = license::shown(
            self.license.as_ref(),
            self.license_unavailable.is_some(),
            |id| name_of(&self.peers, id),
        );
    }

    /// The license may have changed: it is given to the saver, which writes
    /// it off this task and tries again until it can, if anything kept in
    /// its file changed, and paired devices are told if what they know of
    /// it changed. Unavailable, there is no license to save or tell.
    fn license_changed(&mut self) {
        self.refresh_license_status();
        let Some(license) = &self.license else {
            return;
        };
        if self.license_queued.as_ref() != Some(license) {
            if let Some(saver) = &self.license_saver {
                saver.save(license.clone());
            }
            self.license_queued = Some(license.clone());
        }
        let outgoing = license.outgoing();
        if outgoing == self.license_sent {
            return;
        }
        self.license_sent = outgoing;
        let msg = self.license_message();
        for link in self.links.values().filter(|l| l.version >= LICENSE_SINCE) {
            let _ = link.tx.send(Outbound::Control(msg.clone()));
        }
    }

    /// A paired device told its license.
    fn on_license_info(&mut self, from: DeviceId, info: LicenseInfo) {
        if !info.key.is_empty() && !license::valid_key(&info.key) {
            debug!(device = %from, "ignored a license key that cannot be one");
            return;
        }
        if name_of(&self.peers, from.as_str()).is_none() {
            return;
        }
        if self.license_unavailable.is_some() {
            self.license_heard.insert(from, info);
            return;
        }
        let Some(license) = self.license.as_mut() else {
            return;
        };
        let licensed = info.licensed && !info.key.is_empty();
        if !license.peer_said(from.as_str(), &info.key, licensed) {
            return;
        }
        debug!(device = %from, licensed, "a paired device told its license");
        self.adopt_license();
        self.license_changed();
    }

    /// The state of this device's session, for `device`, whose session
    /// speaks peer protocol `version`. One before devices taken offline for
    /// now is told sharing is paused while either of them is, which is all
    /// it understands of it.
    fn session_message(&self, device: &DeviceId, version: u32) -> Control {
        Control {
            msg: Some(control::Msg::SessionState(SessionState {
                screen_locked: self.screen_locked,
                lock_screen_blocked: !self.lock_screen_input,
                input_unavailable: !self.input_available,
                sharing_paused: !self.sharing
                    || (version < OFFLINE_SINCE && self.offline_between(device)),
            })),
        }
    }

    /// Tells the connected devices `which` picks the state of this device's
    /// session, each as it is for it (`session_message`).
    fn tell_session(&self, which: impl Fn(&DeviceId, &Link) -> bool) {
        for (device, link) in self
            .links
            .iter()
            .filter(|(device, link)| which(device, link))
        {
            let msg = self.session_message(device, link.version);
            let _ = link.tx.send(Outbound::Control(msg));
        }
    }

    /// Tells `controller` it controls this paused device no more: first
    /// that it paused, for which a controller that knows the field gives
    /// control back and keeps off; then that its user took over, which
    /// every controller understands, so one that does not know the field
    /// lets go too, if only until it enters again (and is turned away
    /// again). So too while either is taken offline for now, which a
    /// controller that knows it heard of first.
    fn turn_away(&self, controller: &DeviceId) {
        self.tell_session(|device, _| device == controller);
        self.took_over(controller);
    }

    /// Tells `controller` that this device's own user took over: it gives
    /// control back.
    fn took_over(&self, controller: &DeviceId) {
        self.send(
            controller,
            Outbound::Control(Control {
                msg: Some(control::Msg::LocalActivity(LocalActivity {})),
            }),
        );
    }

    /// Pauses sharing, or resumes it. Paused, this device controls none
    /// and none controls it: control by another ends at once, which is how
    /// the local user takes it back where touching this device's own mouse
    /// is not seen (a Wayland desktop hands input over only at the edges),
    /// and the others keep the cursor off it until sharing resumes.
    fn set_sharing(&mut self, enabled: bool) {
        let changed = self.sharing != enabled;
        self.sharing = enabled;
        // Every device hears that this one paused, the controller first of
        // all: it gives control back for that.
        if changed {
            self.tell_session(|_, _| true);
        }
        if !enabled && let Some((controller, releases)) = self.receiver.local_activity() {
            info!(%controller, "sharing paused while controlled; control ends");
            self.inject_all(releases);
            // After why, as `turn_away` does, for a controller that does
            // not know the field.
            self.took_over(&controller);
        }
        self.route(Event::SetSharing(enabled));
    }

    /// This device's displays, with their pictures while they are shared,
    /// as many as other devices accept.
    fn displays_message(&self) -> Control {
        let pictures = if self.preview_sharing {
            let ids = self.local_displays.iter().map(|d| d.id.as_str());
            previews::sendable(&self.local_pictures, ids)
        } else {
            Pictures::new()
        };
        Control {
            msg: Some(control::Msg::Displays(Displays {
                displays: self
                    .local_displays
                    .iter()
                    .map(|d| convert::display_to_proto(d, pictures.get(&d.id).map(|p| p.bytes())))
                    .collect(),
                previews_unchanged: self.previews_unchanged,
            })),
        }
    }

    pub(crate) async fn run(mut self, mut events: mpsc::UnboundedReceiver<EngineEvent>) {
        // The list may have changed since the engine was made with it: a
        // device unpaired as the agent starts.
        let mut peers = self.directory.watch_peers();
        let TrustedDevices {
            list: current,
            generation,
        } = peers.borrow_and_update().clone();
        if generation != self.peers_generation {
            self.on_peers(current, generation);
            self.publish();
        }
        let mut ticks = tokio::time::interval(DRAG_TICK);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut license_rereads = tokio::time::interval_at(
            tokio::time::Instant::now() + self.license_reread,
            self.license_reread,
        );
        license_rereads.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut stamps = tokio::time::interval_at(
            tokio::time::Instant::now() + LAST_SEEN_STAMP,
            LAST_SEEN_STAMP,
        );
        stamps.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let wake_deadline = self.wakes.next_deadline();
            let hold_until = self.hold_until;
            tokio::select! {
                event = events.recv() => match event {
                    None | Some(EngineEvent::Shutdown) => break,
                    Some(event) => self.handle(event),
                },
                Ok(()) = peers.changed() => {
                    // One read already, as a session came up, is not applied
                    // twice.
                    let TrustedDevices {
                        list: current,
                        generation,
                    } = peers.borrow_and_update().clone();
                    if generation != self.peers_generation {
                        self.on_peers(current, generation);
                    }
                }
                _ = ticks.tick(), if self.drags.pending() || self.clipboard.pulls.pending() => {
                    let now = Instant::now();
                    self.forget_arriving_drags();
                    let lapsed = self.drags.expire(now);
                    self.carry_out(lapsed);
                    self.expire_pulls(now);
                }
                _ = license_rereads.tick(), if self.license_unavailable.is_some() => {
                    self.reread_license();
                }
                _ = stamps.tick() => {
                    if let Some(unsaved) = self.stamp_connected() {
                        self.save_devices(unsaved);
                    }
                }
                () = sleep_until(wake_deadline), if wake_deadline.is_some() => {
                    for device in self.wakes.expire(Instant::now()) {
                        info!(%device, "a device being woken did not connect in time");
                    }
                }
                () = sleep_until(hold_until), if hold_until.is_some() => self.end_hold(),
            }
            self.publish();
        }
        self.wind_down();
        self.publish();
        // What was being written is in its file before the agent stops,
        // keys and grabs let go of first.
        self.finish_saves().await;
        // The last change to the license is in its file before the agent
        // stops.
        if let Some(saver) = self.license_saver.as_mut() {
            saver.finish();
        }
    }

    fn handle(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Capture(input) => self.on_capture(input),
            EngineEvent::Platform(state) => self.on_platform(state),
            EngineEvent::RefreshInjection => {
                self.follow_capture();
                self.emulation.refresh();
            }
            EngineEvent::Network(reading) => {
                self.reading = reading;
                self.network_read();
            }
            EngineEvent::SetLoginWindowAccess(on) => self.login_window_access = on,
            EngineEvent::PullClipboard {
                origin,
                offer_id,
                reply,
            } => self.pull_for_app(origin, offer_id, reply),
            EngineEvent::PullClipboardFiles {
                origin,
                offer_id,
                wait,
                folder,
            } => self.pull_files_for_app(origin, offer_id, wait, folder),
            EngineEvent::StagingMade {
                origin,
                offer_id,
                wait,
                made,
            } => self.on_staging_made(origin, offer_id, wait, made),
            EngineEvent::Local(done) => self.on_local(done),
            EngineEvent::PromiseGone {
                origin,
                offer_id,
                serial,
            } => self.promise_gone(origin, offer_id, serial),
            EngineEvent::ClipboardListed {
                purpose,
                content,
                change,
                listed,
            } => self.on_clipboard_listed(purpose, content, change, listed),
            EngineEvent::ClipboardFilesListed { pull, listed } => {
                self.on_clipboard_files_listed(pull, listed)
            }
            EngineEvent::ClipboardFilesEnded { pull_id, late } => self.files_ended(pull_id, late),
            EngineEvent::ClipboardFilesWaiting { to, pull_id, items } => {
                if self.links.contains_key(&to) {
                    let note = Control {
                        msg: Some(control::Msg::ClipboardFilesSending(ClipboardFilesSending {
                            pull_id,
                            items,
                        })),
                    };
                    self.send(&to, Outbound::Control(note));
                }
            }
            EngineEvent::RetryPull { pull_id } => {
                if let Some((to, pull)) = self.clipboard.pulls.again(pull_id) {
                    self.send(&to, pull_message(pull));
                }
            }
            EngineEvent::RetryFilesPull { pull_id } => {
                if let Some(pull) = self.clipboard.file_pulls.get(&pull_id) {
                    let again = files_pull_message(pull_id, pull.offer_id);
                    self.send(&pull.origin.clone(), again);
                }
            }
            EngineEvent::Renamed => self.publish_arrangement(),
            EngineEvent::SetClipboardSync(on) => {
                self.clipboard.on = on;
                self.sync_local_clipboard();
            }
            EngineEvent::SetPreviewSharing(on) => self.set_preview_sharing(on),
            EngineEvent::SetLocked(locked) => match self.router {
                Some(_) => self.route(Event::SetLocked(locked)),
                None => self.locked = locked,
            },
            EngineEvent::SetEdgeDwell(ms) => {
                self.router_config.edge.dwell_ms = ms;
                if let Some(router) = self.router.as_mut() {
                    router.set_edge(self.router_config.edge);
                }
            }
            EngineEvent::IdentifyScreens => {
                if self.identify(self.local.clone()) {
                    info!("identifying the screens of every device");
                    self.broadcast(Control {
                        msg: Some(control::Msg::Identify(Identify {})),
                    });
                }
            }
            EngineEvent::SetSwitchModifier(modifier) => {
                info!(?modifier, "the modifier to hold for switching changed");
                self.router_config.edge.switch_modifier = modifier;
                if let Some(router) = self.router.as_mut() {
                    router.set_edge(self.router_config.edge);
                }
            }
            EngineEvent::SetHotkeys { change, reply } => {
                // What the client sends replaces what is in effect, the
                // last one to ask winning; only what it leaves out (relative
                // mode's hotkey and the double press, from clients before
                // them) is taken from the hotkeys in effect now, rather than
                // from a status it read before.
                let result = change(&self.router_config.hotkeys);
                let unsaved = match result {
                    Ok(hotkeys) => {
                        // The file may hold other hotkeys than those in
                        // effect, ones that could not be used: rewritten
                        // whenever it differs, even with nothing new here.
                        let unsaved = self.directory.change_hotkeys(&hotkeys);
                        if hotkeys != self.router_config.hotkeys {
                            self.set_hotkeys(hotkeys);
                            // Published before the reply, so the client
                            // that asked reads the change in the status it
                            // gets back.
                            self.publish();
                        }
                        unsaved
                    }
                    Err(_) => None,
                };
                let saved = unsaved.map(|unsaved| self.save_devices(unsaved));
                let _ = reply.send(result.map(|_| saved));
            }
            EngineEvent::SetRelative { on, reply } => {
                let result = match self.router.as_mut() {
                    Some(router) => router.set_relative(on),
                    None if on => Err(router::RelativeError::NotControlling),
                    None => Ok(Vec::new()),
                };
                let result = result
                    .map(|actions| self.apply(actions))
                    .map_err(RelativeError::from);
                // The client reads the status as soon as it has the reply.
                self.publish();
                let _ = reply.send(result);
            }
            EngineEvent::SetRelativeAuto(on) => {
                info!(on, "entering relative mode by itself changed");
                self.relative_auto = on;
                if let Some(router) = self.router.as_mut() {
                    let actions = router.set_relative_auto(on);
                    self.apply(actions);
                }
            }
            EngineEvent::FolderAccess(access) => self.folder_access = access,
            EngineEvent::JumpTo { device, reply } => {
                let result = self.jump_to(device);
                // The client reads the status as soon as it has the reply.
                self.publish();
                let _ = reply.send(result);
            }
            EngineEvent::Rearrange { moves, reply } => {
                let _ = reply.send(self.rearrange(moves));
            }
            EngineEvent::License { change, reply } => {
                let result = self.change_license(change);
                // Published before the reply, so the client that asked
                // reads the change in the status it gets back.
                self.publish();
                let _ = reply.send(result);
            }
            EngineEvent::LicenseRead(loaded) => self.on_license_read(loaded),
            EngineEvent::Wake { device, reply } => match self.can_wake(&device) {
                Ok(data) => {
                    info!(%device, "waking a device, as asked");
                    self.wake(device, data, Some(reply));
                }
                Err(e) => {
                    let _ = reply.send(Err(e));
                }
            },
            EngineEvent::WakeNotSent {
                device,
                serial,
                error,
                reply,
            } => {
                if self.wakes.not_sent(&device, serial) {
                    self.publish();
                }
                if let Some(reply) = reply {
                    let _ = reply.send(Err(WakeError::NotSent(error)));
                }
            }
            EngineEvent::SetWakeOnPush { on, reply } => {
                info!(
                    on,
                    "waking a device by pushing the cursor toward it changed"
                );
                self.wake_on_push = on;
                if let Some(router) = self.router.as_mut() {
                    router.set_wake_on_push(on);
                }
                let saved = self
                    .directory
                    .change_wake_on_push(on)
                    .map(|unsaved| self.save_devices(unsaved));
                // The client reads the status as soon as it has the reply.
                self.publish();
                let _ = reply.send(saved);
            }
            EngineEvent::Profiles { change, reply } => {
                let result = self.change_profiles(change);
                // The client reads the status as soon as it has the reply.
                self.publish();
                let _ = reply.send(result);
            }
            EngineEvent::WifiName { network, name } => self.on_wifi_name(network, name),
            EngineEvent::SetOfflineForNow {
                device,
                offline,
                reply,
            } => {
                let result = self.set_offline_for_now(&device, offline);
                // The client reads the status as soon as it has the reply.
                self.publish();
                let _ = reply.send(result);
            }
            EngineEvent::PeerUp {
                device,
                fingerprint,
                session,
                version,
                link,
                heard,
            } => self.on_peer_up(device, fingerprint, session, version, link, heard),
            EngineEvent::PeerDown { device, session } => {
                if self.is_current(&device, session) {
                    self.on_peer_down(device, Gone::Offline);
                }
            }
            EngineEvent::FromPeer {
                device,
                session,
                msg,
            } => {
                if self.is_current(&device, session) {
                    self.on_peer_msg(device, msg);
                }
            }
            EngineEvent::Rtt {
                device,
                session,
                rtt,
            } => {
                if let Some(link) = self.links.get_mut(&device)
                    && link.session == session
                {
                    let ms = rtt_ms(rtt);
                    if link.rtt_ms == 0 || link.rtt_ms.abs_diff(ms) > RTT_SLACK_MS {
                        link.rtt_ms = ms;
                    }
                }
            }
            EngineEvent::SetSharing(enabled) => self.set_sharing(enabled),
            EngineEvent::BeginDrag { manifest, reply } => {
                let result = self.begin_drag(manifest);
                // The client reads the status as soon as it has the reply.
                self.publish();
                let _ = reply.send(result);
            }
            EngineEvent::EndDrag { id, reply } => {
                let ended = self.drags.end(&id);
                let found = ended.is_some();
                self.carry_out(ended.unwrap_or_default());
                self.publish();
                let _ = reply.send(found);
            }
            EngineEvent::AcceptDrag {
                id,
                destination,
                reply,
            } => {
                let accepted = match self.drags.accept(&id, Instant::now()) {
                    // `from` is the device the files are on.
                    Ok((from, effects)) => {
                        info!(device = %from, drag = id, "a drag was dropped here");
                        // Expected before the source hears, so its files
                        // find where they go.
                        self.files.expect_drag(&from, &id, destination);
                        self.carry_out(effects);
                        true
                    }
                    Err(_) => false,
                };
                let _ = reply.send(accepted);
            }
            EngineEvent::DeclineDrag { id, reply } => {
                let declined = match self.drags.decline(&id) {
                    Ok(effects) => {
                        self.carry_out(effects);
                        true
                    }
                    Err(_) => false,
                };
                let _ = reply.send(declined);
            }
            EngineEvent::Shutdown => {}
        }
    }

    /// Puts new hotkeys in effect: capture holds back the keys of the new
    /// lock and jumps from now on. A key held back as part of an old one
    /// stays so until it is released, in capture and in the router alike.
    fn set_hotkeys(&mut self, hotkeys: Hotkeys) {
        info!(
            take_back = %hotkeys.take_back,
            lock = %hotkeys.lock,
            jump = %hotkeys.jump.map(|m| m.to_string()).unwrap_or_default(),
            relative = %hotkeys.relative.map(|h| h.to_string()).unwrap_or_default(),
            double = hotkeys.take_back_double_text(),
            "hotkeys changed"
        );
        let held_back = self.capture.reserve_hotkeys(&hotkeys.reserved());
        self.capture.watch_hotkeys(&acted_on(&hotkeys));
        self.capture.watch_double_press(hotkeys.take_back_double);
        self.router_config.hotkeys = hotkeys;
        self.router_config.hotkeys_held_back.take(held_back);
        if let Some(router) = self.router.as_mut() {
            router.set_hotkeys(hotkeys, held_back);
        }
    }

    /// Writes a change to the devices file or the wake file off this loop,
    /// on a blocking thread of the runtime, so it is written whether or not
    /// whoever asked for the change waits for it; a later change's write
    /// wins over an earlier one's (`Directory::save`). What it returns says
    /// once it is written, or why it could not be (logged too).
    ///
    /// The writes still going on are kept (`saves`), so the agent waits for
    /// them as it stops (`finish_saves`), and one started just before it
    /// stops is in the file once it has.
    fn save_devices(&mut self, unsaved: Unsaved) -> Saved {
        let directory = self.directory.clone();
        let (done, saved) = oneshot::channel();
        self.saves.spawn_blocking(move || {
            let _ = done.send(directory.save(unsaved));
        });
        // Those already written are let go of as others start.
        while self.saves.try_join_next().is_some() {}
        saved
    }

    /// Waits for the writes still going on to be done, up to
    /// [`SAVES_WAIT`]: as the agent stops, after everything is let go of.
    async fn finish_saves(&mut self) {
        let all = async { while self.saves.join_next().await.is_some() {} };
        if tokio::time::timeout(SAVES_WAIT, all).await.is_err() {
            warn!("a devices, wake or offline file was still being written as the agent stopped");
        }
    }

    /// Tells `device` that relative mode began or ended there: ending, with
    /// how far it went, lost datagrams too.
    fn send_relative(&mut self, device: &DeviceId, on: bool) {
        let Some(link) = self.links.get_mut(device) else {
            return;
        };
        if on {
            link.relative_session += 1;
            link.relative_total = Point::default();
        }
        let relative = peer::Relative {
            on,
            session: link.relative_session,
            total_dx: link.relative_total.x,
            total_dy: link.relative_total.y,
        };
        self.send(device, input_msg(input::Msg::Relative(relative)));
    }

    /// The jump hotkey with digit `slot` was pressed: the cursor goes to
    /// the device numbered so, as `JumpTo` takes it there. When it cannot,
    /// as when that device is offline, nothing happens: a hotkey pressed by
    /// mistake should not get in the way.
    fn jump_to_slot(&mut self, slot: u8) {
        let Some(device) = usize::from(slot)
            .checked_sub(1)
            .and_then(|i| self.jump_order.get(i))
            .cloned()
        else {
            debug!(slot, "no device has this number");
            return;
        };
        if let Err(e) = self.jump_to(device.to_string()) {
            debug!(slot, %device, error = %e, "cannot jump to the device with this number");
        }
    }

    /// The number the jump hotkeys go to `device` by, from 1; 0 for none.
    fn jump_slot(&self, device: &DeviceId) -> u32 {
        self.jump_order
            .iter()
            .position(|d| d == device)
            .map_or(0, |i| i as u32 + 1)
    }

    /// The app reports a drag of `manifest` from here, a mouse button
    /// being down: the cursor may now cross with it. While another device
    /// controls this one, the drag is reported there, and the controller
    /// carries the cursor.
    fn begin_drag(&mut self, manifest: Manifest) -> Result<String, DragError> {
        if let Some(controller) = self.receiver.controller().cloned() {
            if !self.speaks_remote_drag(&controller) || self.files_off(&controller) {
                return Err(DragError::Controlled(controller.to_string()));
            }
            if !self.receiver.buttons_held() {
                return Err(DragError::NotDragging);
            }
            let id = self.files.new_id();
            let (files, bytes) = manifest.totals();
            info!(drag = id, files, bytes, %controller, "files are dragged from here; the controller carries them");
            let linked = self
                .links
                .keys()
                .filter(|d| **d != controller && !self.files_off(d))
                .map(ToString::to_string)
                .collect();
            let effects = self.drags.begin_controlled(
                &id,
                Arc::new(manifest),
                &controller,
                linked,
                Instant::now(),
            );
            self.carry_out(effects);
            return Ok(id);
        }
        if self.router.is_none() {
            return Err(DragError::Unavailable);
        }
        self.route(Event::DragActive(true));
        if !self.router.as_ref().is_some_and(Router::dragging) {
            return Err(DragError::NotDragging);
        }
        let id = self.files.new_id();
        let (files, bytes) = manifest.totals();
        info!(drag = id, files, bytes, "files are dragged from here");
        let effects = self.drags.begin(&id, Arc::new(manifest), Instant::now());
        self.carry_out(effects);
        Ok(id)
    }

    /// Carries out what the drags call for.
    fn carry_out(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Send(device, msg) => {
                    self.send(&device, Outbound::Control(Control { msg: Some(msg) }));
                }
                Effect::Transfer {
                    device,
                    manifest,
                    drag,
                } => {
                    info!(%device, drag, "sending what was dragged");
                    self.files
                        .send_manifest(device.as_str(), &manifest, Carrier::Drag(&drag));
                }
                Effect::Event(event) => {
                    let _ = self.drag_events.send(event);
                }
                Effect::Forget { from, drag } => self.files.forget_drag(&from, &drag),
            }
        }
        // The router lets the button cross only while the drag goes on.
        let source = self.router.as_ref().and_then(Router::drag_source).cloned();
        let Some(source) = source else {
            return;
        };
        if source == self.local {
            if !self.drags.active() {
                self.route(Event::DragActive(false));
            }
        } else if self.drags.remote_source() != Some(&source) {
            self.route(Event::RemoteDrag(source, false));
        }
    }

    /// `from`, which this device controls, reports a drag of files there:
    /// the cursor carries it from here.
    fn on_remote_drag(&mut self, from: DeviceId, report: peer::RemoteDrag) {
        let id = report.drag_id.clone();
        // Only where the cursor is, a button held: a late or stray report
        // touches nothing carried now.
        if !self
            .router
            .as_ref()
            .is_some_and(|r| r.can_carry_drag_from(&from))
        {
            debug!(device = %from, drag = id, "a drag reported where the cursor is not, or no button is held; ending it there");
            self.send(
                &from,
                Outbound::Control(Control {
                    msg: Some(control::Msg::DragCancel(peer::DragCancel { drag_id: id })),
                }),
            );
            return;
        }
        if self.files_off(&from) || !self.speaks_remote_drag(&from) {
            info!(device = %from, drag = id, "not carrying a drag of files from a device files are off with");
            self.send(
                &from,
                Outbound::Control(Control {
                    msg: Some(control::Msg::DragDecline(peer::DragDecline { drag_id: id })),
                }),
            );
            return;
        }
        let linked = report
            .linked_device_ids
            .iter()
            .take(MAX_LINKED)
            .filter(|d| iw_net::is_valid_device_id(d))
            .map(DeviceId::new)
            .collect();
        let Some(effects) = self
            .drags
            .remote_began(&from, report, linked, Instant::now())
        else {
            debug!(device = %from, "a malformed drag report; ending it there");
            self.send(
                &from,
                Outbound::Control(Control {
                    msg: Some(control::Msg::DragCancel(peer::DragCancel { drag_id: id })),
                }),
            );
            return;
        };
        info!(device = %from, drag = id, "files are dragged on the device controlled; carrying them");
        self.carry_out(effects);
        self.route(Event::RemoteDrag(from, true));
    }

    /// Forgets the drags accepted here whose files started coming: a later
    /// cancel or a lost source no longer calls them off.
    fn forget_arriving_drags(&mut self) {
        let files = &self.files;
        self.drags
            .forget_arriving(|source, id| files.expects_drag(source, id));
    }

    /// `from` accepted drag `id` offered from here.
    fn accepted_by(&mut self, from: DeviceId, accept: peer::DragAccept) {
        // Files may have been turned off with it since the offer.
        let effects = if self.files_off(&from) {
            info!(device = %from, drag = accept.drag_id, "files are off with the device that accepted a drag; taking it back");
            self.drags.refused_to(&from, &accept.drag_id)
        } else {
            self.drags.accepted_by(&from, &accept.drag_id)
        };
        self.carry_out(effects);
    }

    /// `from`, which controls this device, says the device in `accept`
    /// accepted a drag reported to it from here: the files go there, if
    /// they can.
    fn accepted_for(&mut self, from: DeviceId, accept: peer::DragAccept) {
        let target = DeviceId::new(&accept.device_id);
        let effects = if !iw_net::is_valid_device_id(&accept.device_id)
            || !self.links.contains_key(&target)
            || self.files_off(&target)
            || self.files_off(&from)
        {
            info!(device = %from, drag = accept.drag_id, target = %accept.device_id, "not sending a drag where files cannot go; ending it");
            match self.drags.ended_by_controller(&from, &accept.drag_id) {
                Some(mut effects) => {
                    effects.push(Effect::Send(
                        from.clone(),
                        control::Msg::DragCancel(peer::DragCancel {
                            drag_id: accept.drag_id.clone(),
                        }),
                    ));
                    effects
                }
                None => Vec::new(),
            }
        } else {
            self.drags
                .accepted_for(&from, &accept.drag_id, &target)
                .unwrap_or_default()
        };
        self.carry_out(effects);
    }

    /// Whether `device`'s protocol knows drags of files on a controlled
    /// device.
    fn speaks_remote_drag(&self, device: &DeviceId) -> bool {
        self.links
            .get(device)
            .is_some_and(|l| l.version >= REMOTE_DRAG_SINCE)
    }

    fn is_current(&self, device: &DeviceId, session: u64) -> bool {
        self.links.get(device).is_some_and(|l| l.session == session)
    }

    fn on_capture(&mut self, input: InputEvent) {
        // Counted as capture counts what it reports (`edge_wait`): every
        // event sent on the capture channel arrives here exactly once, in
        // order, through the bridge thread.
        self.capture_events += 1;
        // Capture reports only physical input, so while another device is
        // in control a movement, a press or the wheel means the local user
        // is taking over. Not a release, nor capture letting go
        // (`CaptureEnded`): those Wayland's capture reports of itself, as
        // input it held goes back to the desktop.
        let activity = match input {
            InputEvent::PointerMotion { .. } | InputEvent::Scroll { .. } => true,
            InputEvent::Key { pressed, .. } | InputEvent::Button { pressed, .. } => pressed,
            InputEvent::DoublePressed => true,
            InputEvent::CaptureEnded | InputEvent::HotkeysHeldBack(_) => false,
        };
        // Kept for a router made later; the router in place hears it in
        // order with the keys, below.
        if let InputEvent::HotkeysHeldBack(held) = input {
            self.router_config.hotkeys_held_back.take(held);
        }
        if activity && let Some((controller, releases)) = self.receiver.local_activity() {
            info!(%controller, "local input while controlled; taking control back");
            self.inject_all(releases);
            self.took_over(&controller);
            self.refresh_exits();
        }
        self.route_captured(input);
    }

    fn on_peer_up(
        &mut self,
        device: DeviceId,
        fingerprint: Fingerprint,
        session: u64,
        version: u32,
        tx: mpsc::UnboundedSender<Outbound>,
        heard: Arc<LastHeard>,
    ) {
        if self.links.contains_key(&device) {
            // A new session in place of this one: it never went offline.
            self.on_peer_down(device.clone(), Gone::Replaced);
        }
        // A session with a just-paired device can come up before the
        // engine has seen the directory change.
        let (current, generation) = self.directory.peers_now();
        if generation != self.peers_generation {
            self.on_peers(current, generation);
        }
        if !self.trusts(&device, &fingerprint) {
            // Unpaired while the session was being set up; dropping `tx`
            // ends the session.
            return;
        }
        if !self.reach.borrow().allows(device.as_str()) {
            // The profile in use came to leave it out while the session was
            // being set up.
            debug!(%device, "the profile in use does not connect it; ending its session");
            return;
        }
        info!(%device, "peer connected");
        let _ = tx.send(Outbound::Control(self.displays_message()));
        let _ = tx.send(Outbound::Control(layout_message(&self.directory.layout())));
        // Before the session's state, which a device before them is told
        // instead.
        if version >= OFFLINE_SINCE {
            let _ = tx.send(Outbound::Control(offline_message(&self.offline)));
        }
        let _ = tx.send(Outbound::Control(self.session_message(&device, version)));
        if version >= RELATIVE_SINCE {
            let _ = tx.send(Outbound::Control(self.pointer_message()));
        }
        // Even without a key: the device may still list this one from
        // before it was removed here.
        if version >= LICENSE_SINCE && self.license.is_some() {
            let _ = tx.send(Outbound::Control(self.license_message()));
        }
        self.links.insert(
            device.clone(),
            Link {
                session,
                fingerprint,
                tx,
                motion_seq: 0,
                scroll_seq: 0,
                rtt_ms: 0,
                screen_locked: false,
                lock_screen_blocked: false,
                input_unavailable: false,
                sharing_paused: false,
                version,
                relative_seq: 0,
                relative_session: 0,
                relative_total: Point::default(),
                pointer_captured: false,
                wake_info_at: None,
                offline_heard: false,
                heard,
            },
        );
        if let Some(router) = self.router.as_mut() {
            router.set_relative_capable(device.clone(), version >= RELATIVE_SINCE);
        }
        if self.wakes.connected(device.as_str()) {
            info!(%device, "a device being woken connected");
        }
        self.route(Event::PeerConnected(device.clone()));
        // Kept off from the start, while it is offline for now.
        if self.offline_for_now(&device) {
            self.route(Event::PeerOfflineForNow(device, true));
        }
        self.publish_arrangement();
    }

    fn trusts(&self, device: &DeviceId, fingerprint: &Fingerprint) -> bool {
        self.peers
            .iter()
            .any(|p| p.id == device.as_str() && p.fingerprint == *fingerprint)
    }

    /// The set of trusted devices changed: sessions with devices no longer
    /// trusted end, their pictures are forgotten, and names, key remapping
    /// and the layout follow.
    fn on_peers(&mut self, peers: Arc<Vec<Peer>>, generation: u64) {
        self.peers_generation = generation;
        // Paired, unpaired or told how to wake anew, the directory may have
        // moved, dropped or forgotten when a device was last seen.
        self.take_last_seen();
        let wakeable_changed = wakeable_differs(&self.peers, &peers);
        // Only how fast the cursor moves, or how to wake devices, changed:
        // nothing to trust, name or place anew.
        if only_local_choices_differ(&self.peers, &peers) {
            self.peers = peers;
            let wakeable = self.wakeable_now();
            if let Some(router) = self.router.as_mut() {
                router.set_speeds(peer_speeds(&self.peers));
                if wakeable_changed {
                    router.set_wakeable(wakeable);
                }
            }
            return;
        }
        let gone: Vec<String> = self
            .peers
            .iter()
            .filter(|old| !peers.iter().any(|p| p.id == old.id))
            .map(|old| old.id.clone())
            .collect();
        self.peers = peers;
        // Not trusted any more, they are not listed in profiles either.
        if self
            .profiles
            .forget_devices(|d| gone.iter().any(|g| g == d))
        {
            self.save_profiles(false);
            self.apply_profile();
            self.show_profiles();
        }
        for id in gone {
            self.peer_pictures.remove(&DeviceId::new(&id));
            self.preview_files.remove(&id);
        }
        let untrusted: Vec<DeviceId> = self
            .links
            .iter()
            .filter(|(device, link)| !self.trusts(device, &link.fingerprint))
            .map(|(device, _)| device.clone())
            .collect();
        for device in untrusted {
            info!(%device, "no longer trusted; ending its session");
            // Removing the link drops its sender, which ends the session.
            self.on_peer_down(device, Gone::Untrusted);
        }
        let wakeable = self.wakeable_now();
        if let Some(router) = self.router.as_mut() {
            // Replaced, so devices no longer trusted keep none.
            router.set_remaps(peer_remaps(&self.peers));
            router.set_speeds(peer_speeds(&self.peers));
            if wakeable_changed {
                router.set_wakeable(wakeable);
            }
        }
        let peers = self.peers.clone();
        self.wakes.retain(|id| peers.iter().any(|p| p.id == id));
        // Devices no longer trusted leave the computers on the license, and
        // the names of the others may have changed.
        if let Some(license) = self.license.as_mut()
            && license.forget_peers(|id| self.peers.iter().any(|p| p.id == id))
        {
            self.license_changed();
        } else {
            self.refresh_license_status();
        }
        self.rebuild_layout();
    }

    fn on_peer_down(&mut self, device: DeviceId, gone: Gone) {
        info!(%device, "peer disconnected");
        let link = self.links.remove(&device);
        // Offline since it was last heard from, for the apps, while it can
        // be woken (kept apart from the trusted devices, which are not
        // published anew for it); not for a session replaced by another.
        if gone == Gone::Offline
            && let Some(link) = link
            && let Some(unsaved) = self.note_last_seen(&[(device.as_str(), link.heard.get())])
        {
            self.save_devices(unsaved);
        }
        self.route(Event::PeerLost(device.clone()));
        let releases = self
            .receiver
            .handle(RemoteEvent::ControllerLost(device.clone()));
        self.inject_all(releases);
        // Controlled by it no more.
        self.refresh_exits();
        self.end_with(&device);
        self.publish_arrangement();
    }

    /// Remembers when each connected device that told how to wake it was
    /// last heard from, as when it was last seen connected: so a stop
    /// without word (a crash, a power cut, Windows ending the agent) leaves
    /// a time that late at worst. What to write, if anything changed.
    fn stamp_connected(&mut self) -> Option<Unsaved> {
        let heard: Vec<(String, SystemTime)> = self
            .links
            .iter()
            .map(|(device, link)| (device.to_string(), link.heard.get()))
            .collect();
        if heard.is_empty() {
            return None;
        }
        let heard: Vec<(&str, SystemTime)> =
            heard.iter().map(|(id, at)| (id.as_str(), *at)).collect();
        self.note_last_seen(&heard)
    }

    /// Remembers when the devices `seen` were last seen connected, those
    /// trusted that told how to wake them (`Directory::change_last_seen`),
    /// in the directory and in the engine's copy of it: the only way the
    /// engine changes either. What to write, if anything changed.
    fn note_last_seen(&mut self, seen: &[(&str, SystemTime)]) -> Option<Unsaved> {
        let unsaved = self.directory.change_last_seen(seen)?;
        self.take_last_seen();
        Some(unsaved)
    }

    /// Takes the engine's copy of when devices were last seen anew from the
    /// directory.
    fn take_last_seen(&mut self) {
        self.last_seen = self.directory.last_seen();
    }

    /// A device told how to wake it: kept, if it is a way (`WakeData`), and
    /// it told nothing of it, a way or not, within the last
    /// [`INFO_INTERVAL`].
    fn on_wake_info(&mut self, from: DeviceId, info: WakeInfo) {
        let now = Instant::now();
        let Some(link) = self.links.get_mut(&from) else {
            return;
        };
        if link
            .wake_info_at
            .is_some_and(|at| now.saturating_duration_since(at) < INFO_INTERVAL)
        {
            debug!(device = %from, "dropping how to wake a device, said again too soon");
            return;
        }
        // Whatever it said counts toward the interval, so a device saying
        // what is no way to wake it, again and again, is warned of at most
        // that often.
        link.wake_info_at = Some(now);
        let Some(data) = WakeData::from_proto(&info) else {
            warn!(device = %from, "dropping how to wake a device: no way to wake one");
            return;
        };
        debug!(device = %from, ?data, "a device told how to wake it");
        if let Some(unsaved) = self.directory.change_how_to_wake(from.as_str(), data) {
            self.save_devices(unsaved);
        }
    }

    /// How to wake `device`, if it can be woken now: trusted, offline, and
    /// it told how.
    fn can_wake(&self, device: &str) -> Result<WakeData, WakeError> {
        let peer = self
            .peers
            .iter()
            .find(|p| p.id == device)
            .ok_or_else(|| WakeError::Unknown(device.to_owned()))?;
        if self.offline_between(&DeviceId::new(device)) {
            return Err(WakeError::OfflineForNow(device.to_owned()));
        }
        let reach = self.reach.borrow();
        if reach.paused {
            return Err(WakeError::ProfilePaused);
        }
        if !reach.connects(device) {
            return Err(WakeError::NotInProfile(device.to_owned()));
        }
        drop(reach);
        if self.links.contains_key(&DeviceId::new(device)) {
            return Err(WakeError::Connected(device.to_owned()));
        }
        peer.wake
            .ok_or_else(|| WakeError::NoWakeData(device.to_owned()))
    }

    /// Sends `device` a wake-up, off this loop: it is being woken from now,
    /// for `wake_wait`. `reply`, if any, hears once it was sent, or that it
    /// could not be once the status shows `device` did not wake up
    /// (`EngineEvent::WakeNotSent`).
    fn wake(
        &mut self,
        device: String,
        data: WakeData,
        reply: Option<oneshot::Sender<Result<(), WakeError>>>,
    ) {
        let serial = self.wakes.sent(&device, Instant::now(), self.wake_wait);
        // The client that asked reads the status as soon as it has the
        // reply, which comes after this.
        self.publish();
        let packet = wake::magic_packet(data.mac());
        #[cfg(feature = "test-hooks")]
        let target = self.wake_target;
        let events = self.events.clone();
        tokio::spawn(async move {
            // Where, as this device's networks are now.
            #[allow(unused_mut)]
            let mut destinations = wake::destinations(&data, &wake::interfaces_now().await);
            #[cfg(feature = "test-hooks")]
            if let Some(target) = target {
                destinations.iter_mut().for_each(|d| *d = target);
            }
            match wake::send(&packet, &destinations).await {
                Ok(()) => {
                    if let Some(reply) = reply {
                        let _ = reply.send(Ok(()));
                    }
                }
                Err(e) => {
                    warn!(%device, error = %e, "cannot send a wake-up");
                    // Through the engine, so whoever asked hears it once the
                    // status says so; with the engine gone, the reply goes
                    // with the event.
                    if let Some(events) = events.upgrade() {
                        let _ = events.send(EngineEvent::WakeNotSent {
                            device,
                            serial,
                            error: e.to_string(),
                            reply,
                        });
                    }
                }
            }
        });
    }

    fn on_peer_msg(&mut self, from: DeviceId, msg: PeerMsg) {
        let sender = from.clone();
        // Where the cursor crossed an edge here, coming in or going out,
        // and the device that said so.
        let mut crossed_in: Option<(peer::EdgeSpot, DeviceId)> = None;
        let mut crossed_out: Option<(peer::EdgeSpot, DeviceId)> = None;
        let msg = match msg {
            PeerMsg::Clipboard(clipboard) => return self.on_clipboard(from, clipboard),
            PeerMsg::PullBroken(id) => {
                if let Some(waiter) = self.clipboard.pulls.finish(id, &from) {
                    self.pull_failed(waiter, ClipboardSkip::Unavailable);
                }
                return;
            }
            msg => msg,
        };
        let remote = match msg {
            PeerMsg::Control(Control { msg: Some(msg) }) => {
                match msg {
                    control::Msg::Displays(d) => self.on_displays(from, d),
                    control::Msg::Layout(layout) => self.on_layout(from, layout),
                    control::Msg::ClipboardRequest(request) => {
                        self.answer_clipboard_request(from, request.id)
                    }
                    control::Msg::ClipboardOffer(offer) => self.on_clipboard_offer(from, offer),
                    control::Msg::ClipboardPull(pull) => self.on_clipboard_pull(from, pull),
                    control::Msg::ClipboardFilesPull(pull) => {
                        self.on_clipboard_files_pull(from, pull)
                    }
                    control::Msg::ClipboardFilesSending(sending) => {
                        self.on_clipboard_files_sending(from, sending)
                    }
                    control::Msg::ClipboardSkipped(skipped) => {
                        self.on_clipboard_skipped(from, skipped)
                    }
                    control::Msg::LocalActivity(_) => self.route(Event::RemoteActivity(from)),
                    control::Msg::Locate(locate) => self.on_locate(from, locate),
                    control::Msg::DragOffer(offer) => {
                        let takes = self.app_takes_drags();
                        // The files may be on a device `from` controls,
                        // which must be linked here to send them.
                        let source = match offer.source_device_id.as_str() {
                            "" => Some(from.clone()),
                            source if self.links.contains_key(&DeviceId::new(source)) => {
                                Some(DeviceId::new(source))
                            }
                            _ => None,
                        };
                        let files_off = self.files_off(&from)
                            || source.as_ref().is_none_or(|s| self.files_off(s));
                        debug!(device = %from, drag = offer.drag_id, takes, files_off, "a drag is offered here");
                        // Turned off with that device, as if no app were
                        // here to take it: declined at once.
                        let effects = self.drags.offered(
                            &from,
                            source.as_ref().unwrap_or(&from),
                            offer,
                            self.receiver.controller(),
                            takes && !files_off,
                            Instant::now(),
                        );
                        self.carry_out(effects);
                    }
                    control::Msg::RemoteDrag(report) => self.on_remote_drag(from, report),
                    control::Msg::DragAccept(accept) => {
                        if let Some(effects) =
                            self.drags
                                .remote_accepted(&from, &accept.drag_id, Instant::now())
                        {
                            // Accepted where a drag carried from here was
                            // offered: its source sends the files there.
                            self.carry_out(effects);
                        } else if !accept.device_id.is_empty() {
                            self.accepted_for(from, accept);
                        } else {
                            self.accepted_by(from, accept);
                        }
                    }
                    control::Msg::DragDecline(decline) => {
                        let effects = match self.drags.remote_declined(&from, &decline.drag_id) {
                            Some(effects) => effects,
                            None => self.drags.declined_by(&from, &decline.drag_id),
                        };
                        self.carry_out(effects);
                    }
                    control::Msg::DragCancel(cancel) => {
                        // The source of a drag carried from here ended it;
                        // the controller ended one reported to it from here;
                        // or an offer here is taken back.
                        self.forget_arriving_drags();
                        let effects = self
                            .drags
                            .remote_cancelled(&from, &cancel.drag_id)
                            .or_else(|| self.drags.ended_by_controller(&from, &cancel.drag_id))
                            .unwrap_or_else(|| self.drags.withdrawn(&from, &cancel.drag_id));
                        self.carry_out(effects);
                    }
                    control::Msg::Identify(_) => {
                        if self.identify(from.clone()) {
                            debug!(device = %from, "identifying the screens, as another device asked");
                        }
                    }
                    control::Msg::SessionState(state) => self.on_session_state(from, state),
                    control::Msg::LicenseInfo(info) => self.on_license_info(from, info),
                    control::Msg::PointerAt(at) => {
                        if at.x.is_finite() && at.y.is_finite() {
                            self.route(Event::PointerAt(from, Point::new(at.x, at.y)));
                        }
                    }
                    control::Msg::PointerCaptured(captured) => {
                        let captured = captured.captured;
                        if let Some(link) = self.links.get_mut(&from)
                            && link.pointer_captured != captured
                        {
                            debug!(device = %from, captured, "an app there holds the pointer or let go of it");
                            link.pointer_captured = captured;
                            self.route(Event::PointerCaptured(from, captured));
                        }
                    }
                    control::Msg::WakeInfo(info) => self.on_wake_info(from, info),
                    control::Msg::OfflineDevices(doc) => self.on_offline_devices(from, doc),
                    control::Msg::Hello(_) | control::Msg::Heartbeat(_) => {}
                }
                return;
            }
            PeerMsg::Input(Input { msg: Some(msg) }) => match msg {
                input::Msg::Enter(mut e) => {
                    crossed_in = e.crossed.take().map(|spot| (spot, from.clone()));
                    RemoteEvent::Enter {
                        from,
                        position: Point::new(e.x, e.y),
                        modifiers: e
                            .modifiers
                            .iter()
                            .filter_map(|&m| convert::usage_from_proto(m))
                            .filter(|u| u.is_modifier())
                            .collect(),
                        buttons: e
                            .buttons
                            .iter()
                            .filter_map(|&b| convert::button_from_proto(b))
                            .collect(),
                    }
                }
                input::Msg::Leave(l) => {
                    crossed_out = l.crossed.map(|spot| (spot, from.clone()));
                    RemoteEvent::Leave { from }
                }
                input::Msg::Relative(r) => {
                    if self.receiver.controller() == Some(&from) {
                        info!(device = %from, on = r.on, "relative mode began or ended here");
                    }
                    RemoteEvent::Relative {
                        from,
                        on: r.on,
                        session: r.session,
                        total: Point::new(r.total_dx, r.total_dy),
                    }
                }
                input::Msg::Button(b) => {
                    let Some(button) = convert::button_from_proto(b.button) else {
                        return;
                    };
                    RemoteEvent::Button {
                        from,
                        button,
                        pressed: b.pressed,
                        position: (!b.at_pointer).then(|| Point::new(b.x, b.y)),
                    }
                }
                input::Msg::Key(k) => {
                    let Some(usage) = convert::usage_from_proto(k.usage) else {
                        return;
                    };
                    RemoteEvent::Key {
                        from,
                        usage,
                        pressed: k.pressed,
                    }
                }
            },
            PeerMsg::Datagram(Datagram { msg: Some(msg) }) => match msg {
                datagram::Msg::Motion(m) => RemoteEvent::Motion {
                    from,
                    seq: m.seq,
                    position: Point::new(m.x, m.y),
                },
                datagram::Msg::Scroll(s) => RemoteEvent::Scroll {
                    from,
                    seq: s.seq,
                    dx: s.dx,
                    dy: s.dy,
                },
                datagram::Msg::RelativeMotion(m) => RemoteEvent::RelativeMotion {
                    from,
                    session: m.session,
                    seq: m.seq,
                    total: Point::new(m.total_dx, m.total_dy),
                },
            },
            _ => return,
        };
        // Paused or taken offline for now, this device takes control from
        // none, nor from a device taken offline: an entry, as from a
        // controller that does not know it yet (or ever: InputWeave 1.0) or
        // that entered as it did, is turned away, and input meant for a
        // controller that has none here is dropped, a `Leave` too. None
        // has: pausing and being cut off let go of the controller at once
        // (`set_sharing`, `offline_changed`), and nothing enters after.
        if !self.shares() || self.offline_for_now(&sender) {
            if let RemoteEvent::Enter { from, .. } = &remote {
                info!(device = %from, "it entered while sharing is paused or offline for now; turned away");
                self.turn_away(from);
            }
            return;
        }
        let entered = match &remote {
            RemoteEvent::Enter { from, .. } => Some(from.clone()),
            _ => None,
        };
        // A drag reported to the controller leaves with the cursor, and
        // comes back with it.
        match &remote {
            RemoteEvent::Leave { from } if self.receiver.controller() == Some(from) => {
                self.drags.left_source(from);
            }
            RemoteEvent::Enter { from, .. } => self.drags.back_on_source(from),
            _ => {}
        }
        // Relative mode ending with the cursor still here: the controller
        // carries on from where the pointer went.
        let relative_ended = match &remote {
            RemoteEvent::Relative {
                from, on: false, ..
            } if self.receiver.controller() == Some(from) && self.receiver.relative() => {
                Some(from.clone())
            }
            _ => None,
        };
        let controlled = self.receiver.controller().is_some();
        let was_controller = crossed_out
            .as_ref()
            .is_some_and(|(_, sender)| self.receiver.controller() == Some(sender));
        let injections = self.receiver.handle(remote);
        self.inject_all(injections);
        if self.receiver.controller().is_some() != controlled {
            self.refresh_exits();
        }
        // The edge here the cursor came in through, once it is here, or
        // went out through, from the controller it was with.
        if let Some((spot, sender)) = crossed_in
            && self.receiver.controller() == Some(&sender)
        {
            self.crossed_here_by(spot, true);
        }
        if let Some((spot, _)) = crossed_out
            && was_controller
        {
            self.crossed_here_by(spot, false);
        }
        if let Some(controller) = relative_ended {
            match self.emulation.position() {
                Some(at) => {
                    let at = PointerAt { x: at.x, y: at.y };
                    self.send(
                        &controller,
                        Outbound::Control(Control {
                            msg: Some(control::Msg::PointerAt(at)),
                        }),
                    );
                }
                None => debug!("cannot tell the controller where the pointer is"),
            }
        }
        // A drag offered ahead of the cursor shows once it is here.
        if let Some(from) = entered {
            let effects = self.drags.controller_entered(&from);
            self.carry_out(effects);
        }
    }

    /// A peer's displays and their pictures, which replace what it sent
    /// before. Pictures beyond the limits are dropped, never the session.
    fn on_displays(&mut self, from: DeviceId, msg: Displays) {
        let displays: Vec<Display> = msg
            .displays
            .iter()
            .filter_map(convert::display_from_proto)
            .collect();
        debug!(device = %from, count = displays.len(), "received displays");
        let mut pictures = if msg.previews_unchanged {
            // It cannot read its pictures now (it is at its login window):
            // the ones kept here stay.
            self.peer_pictures.get(&from).cloned().unwrap_or_default()
        } else {
            previews::accept(&msg.displays).unwrap_or_else(|e| {
                warn!(device = %from, error = %e, "dropping the desktop pictures a device sent");
                Pictures::new()
            })
        };
        pictures.retain(|id, _| displays.iter().any(|d| d.id == *id));
        let pictures_changed = self.set_peer_pictures(&from, pictures);
        if self.peer_displays.get(&from) != Some(&displays) {
            self.directory.remember_displays(from.as_str(), &displays);
            self.peer_displays.insert(from, displays);
            self.rebuild_layout();
        } else if pictures_changed {
            self.publish_arrangement();
        }
    }

    /// Keeps `pictures` as `device`'s, in memory and in files. Returns
    /// whether they changed.
    fn set_peer_pictures(&mut self, device: &DeviceId, pictures: Pictures) -> bool {
        let unchanged = match self.peer_pictures.get(device) {
            Some(current) => *current == pictures,
            None => pictures.is_empty(),
        };
        if unchanged {
            return false;
        }
        self.preview_files.save(device.as_str(), &pictures);
        if pictures.is_empty() {
            self.peer_pictures.remove(device);
        } else {
            self.peer_pictures.insert(device.clone(), pictures);
        }
        true
    }

    /// Whether the session with `device` knows clipboard offers and pulls.
    fn speaks_offers(&self, device: &DeviceId) -> bool {
        self.links
            .get(device)
            .is_some_and(|l| l.version >= OFFERS_SINCE)
    }

    /// Whether the session with `device` knows files in clipboard offers.
    fn speaks_files(&self, device: &DeviceId) -> bool {
        self.links
            .get(device)
            .is_some_and(|l| l.version >= FILES_SINCE)
    }

    /// `reason` as `device` understands it: busy only from protocol 3.
    fn skip_for(&self, device: &DeviceId, reason: ClipboardSkip) -> ClipboardSkip {
        if reason == ClipboardSkip::Busy && !self.speaks_files(device) {
            ClipboardSkip::Unavailable
        } else {
            reason
        }
    }

    /// Answers `device`'s pull or request `id`: none of it comes, and why.
    fn refuse(&self, device: &DeviceId, id: u64, reason: ClipboardSkip) {
        self.send(device, skipped_message(id, self.skip_for(device, reason)));
    }

    /// Whether the clipboard goes neither to `device` nor from it: this
    /// device's user turned it off with that device, or in the profile in
    /// use, or either is taken offline for now.
    fn clipboard_off(&self, device: &DeviceId) -> bool {
        self.offline_between(device)
            || self.profile_in_use().is_some_and(|p| !p.clipboard)
            || self
                .peers
                .iter()
                .any(|p| p.id == device.as_str() && p.clipboard_off)
    }

    /// Whether this device's user turned files off with `device`.
    /// Whether this device's app is offered drags of files: one is here,
    /// and this device takes part in dragging (not Linux,
    /// [`Options::refuses_drags`](super::Options::refuses_drags)), so a
    /// drag is declined at once, as if no app were here.
    fn app_takes_drags(&self) -> bool {
        self.ui_clients.load(Ordering::SeqCst) > 0 && !self.refuses_drags
    }

    /// Whether files go neither to `device` nor from it: this device's user
    /// turned them off with that device, or in the profile in use, or
    /// either is taken offline for now.
    fn files_off(&self, device: &DeviceId) -> bool {
        self.offline_between(device)
            || self.profile_in_use().is_some_and(|p| !p.files)
            || self
                .peers
                .iter()
                .any(|p| p.id == device.as_str() && p.files_off)
    }

    /// Has IPC clients show this device's name on its screens, as `source`
    /// asked, unless it asked less than a second ago. Returns whether they
    /// were told.
    fn identify(&mut self, source: DeviceId) -> bool {
        let now = Instant::now();
        if !locate_due(self.last_identified.get(&source).copied(), now) {
            debug!(device = %source, "ignoring a request to identify the screens so soon after the last");
            return false;
        }
        self.last_identified.insert(source, now);
        self.identified.send_replace(Some(IDENTIFY_MS));
        true
    }

    /// Moves the clipboard as the cursor moved, except to or from devices
    /// this device's user turned it off with: their clipboards neither
    /// come here nor pass through, and nothing goes to them.
    fn move_clipboard(&mut self, clipboard_move: ClipboardMove) {
        let off = match &clipboard_move {
            ClipboardMove::Push { to } => self.clipboard_off(to),
            ClipboardMove::Fetch { from, to } => {
                self.clipboard_off(from) || (*to != self.local && self.clipboard_off(to))
            }
        };
        if off {
            debug!(
                ?clipboard_move,
                "the clipboard is turned off with a device involved"
            );
            return;
        }
        match clipboard_move {
            ClipboardMove::Push { to } => {
                let (offers, files) = (self.speaks_offers(&to), self.speaks_files(&to));
                self.take_local(Purpose::Push { to, offers, files });
            }
            ClipboardMove::Fetch { from, to } => {
                if !self.clipboard.on || !self.links.contains_key(&from) {
                    return;
                }
                let id = self.clipboard.next_request;
                self.clipboard.next_request += 1;
                self.clipboard.requests.insert(id, to);
                let request = Control {
                    msg: Some(control::Msg::ClipboardRequest(ClipboardRequest { id })),
                };
                self.send(&from, Outbound::Control(request));
            }
        }
    }

    /// The controller asks for this device's clipboard as the cursor
    /// leaves it. A controller the clipboard is turned off with gets none,
    /// as if sync were off here.
    fn answer_clipboard_request(&mut self, controller: DeviceId, id: u64) {
        if self.clipboard_off(&controller) {
            self.send(&controller, skipped_message(id, ClipboardSkip::Disabled));
            return;
        }
        let (offers, files) = (
            self.speaks_offers(&controller),
            self.speaks_files(&controller),
        );
        self.take_local(Purpose::Answer {
            controller,
            id,
            offers,
            files,
        });
    }

    /// Has the local clipboard read to move on, unless sync is off or there
    /// is none: up to the lazy limit where it may be offered. The read is
    /// done on the clipboard's thread, and `on_taken` carries on.
    fn take_local(&mut self, purpose: Purpose) {
        let sync = &self.clipboard;
        let local = match &sync.local {
            Some(local) if sync.on => local,
            _ => {
                let skip = if sync.on {
                    ClipboardSkip::Unavailable
                } else {
                    ClipboardSkip::Disabled
                };
                self.on_taken(purpose, Err(skip));
                return;
            }
        };
        let limit = if purpose.offers() {
            lazy_max(sync.max)
        } else {
            sync.max
        };
        local.ask(LocalOp::Take { purpose, limit });
    }

    /// The local clipboard was read to move on, or does not move. Files
    /// copied there are looked at first, off the engine, to be offered
    /// with the rest (`on_clipboard_listed`), except to a device files are
    /// turned off with, whether it is the one the cursor went to or the
    /// controller asking.
    fn on_taken(&mut self, purpose: Purpose, result: Result<Taken, ClipboardSkip>) {
        let (content, change, paths) = match result {
            Ok(taken) => taken,
            Err(skip) => {
                self.answer_skipped(purpose, skip);
                return;
            }
        };
        let files_off = match &purpose {
            Purpose::Push { to, .. } => self.files_off(to),
            Purpose::Answer { controller, .. } => self.files_off(controller),
        };
        if paths.is_empty() || files_off {
            self.deliver(purpose, content, change, Vec::new(), Vec::new());
            return;
        }
        let events = self.events.clone();
        let files = self.files.clone();
        tokio::spawn(async move {
            let listed = files.clipboard_items(paths).await;
            if let Some(engine) = events.upgrade() {
                let _ = engine.send(EngineEvent::ClipboardListed {
                    purpose,
                    content,
                    change,
                    listed,
                });
            }
        });
    }

    /// The files on the local clipboard were looked at: those that can go
    /// are offered with the rest of it, unless this device already sends
    /// as many items as it lists at once.
    fn on_clipboard_listed(
        &mut self,
        purpose: Purpose,
        content: ClipboardContent,
        change: u64,
        mut listed: Vec<(PathBuf, DragItem)>,
    ) {
        if !listed.is_empty() && self.files.sending_items() + listed.len() > MAX_ITEMS {
            info!(
                files = listed.len(),
                "sending too many items already; the files on the clipboard are not offered"
            );
            listed.clear();
        }
        let (paths, items) = listed.into_iter().unzip();
        self.deliver(purpose, content, change, paths, items);
    }

    /// The device the local clipboard was read for hears why it does not
    /// move, or, pushed, it is noted.
    fn answer_skipped(&mut self, purpose: Purpose, skip: ClipboardSkip) {
        match purpose {
            Purpose::Push { to, .. } => self.note_skip(self.local.clone(), to, skip),
            Purpose::Answer { controller, id, .. } => {
                self.send(&controller, skipped_message(id, skip));
            }
        }
    }

    /// Moves the local clipboard on as `purpose` asks: sent whole up to the
    /// eager limit, offered beyond it where the device knows offers, or not
    /// at all, and the device it was asked for told. Files (`items`, at
    /// `paths` here) are always offered.
    fn deliver(
        &mut self,
        purpose: Purpose,
        content: ClipboardContent,
        change: u64,
        paths: Vec<PathBuf>,
        items: Vec<DragItem>,
    ) {
        if content.is_empty() && items.is_empty() {
            self.answer_skipped(purpose, ClipboardSkip::Empty);
            return;
        }
        let offers = purpose.offers();
        let (to, request_id) = match purpose {
            Purpose::Push { to, .. } => (to, 0),
            Purpose::Answer { controller, id, .. } => (controller, id),
        };
        let pushed = request_id == 0;
        let how = if items.is_empty() {
            delivery(content.len(), self.clipboard.max, offers)
        } else {
            Delivery::Offer
        };
        match how {
            Delivery::Send => {
                if pushed {
                    self.note_clipboard(self.local.clone(), to.clone(), &content, None);
                }
                let whole = ClipboardTransfer {
                    request_id,
                    origin: self.local.to_string(),
                    content,
                };
                self.send(&to, Outbound::Clipboard(whole));
            }
            Delivery::Offer => {
                let Some(offer_id) = self.clipboard.own_offers.make(change, paths) else {
                    warn!("no random numbers to offer the clipboard with");
                    if pushed {
                        let formats = content.items.iter().map(|(f, _)| *f).collect();
                        let skip = Some(ClipboardSkip::Unavailable);
                        self.note_formats(self.local.clone(), to, formats, skip);
                    } else {
                        self.send(&to, skipped_message(request_id, ClipboardSkip::Unavailable));
                    }
                    return;
                };
                debug!(%to, files = items.len(), "offering this device's clipboard rather than sending it");
                self.clipboard.lazy_stats.offered += 1;
                if pushed {
                    self.note_clipboard(self.local.clone(), to.clone(), &content, None);
                }
                let offer = offer_message(
                    request_id,
                    offer_id,
                    &self.local,
                    change,
                    parts_of(&content),
                    items,
                );
                self.send(&to, offer);
            }
            Delivery::TooLarge if pushed => {
                self.note_skip(self.local.clone(), to, ClipboardSkip::TooLarge);
            }
            Delivery::TooLarge => {
                self.send(&to, skipped_message(request_id, ClipboardSkip::TooLarge))
            }
        }
    }

    /// What the local clipboard's thread did.
    fn on_local(&mut self, done: LocalDone) {
        match done {
            LocalDone::Taken { purpose, result } => self.on_taken(purpose, result),
            LocalDone::FilesChecked { pull, result } => self.on_files_checked(pull, result),
            LocalDone::Served { serve, result } => {
                self.clipboard
                    .serving
                    .done(&serve.from, serve.offer_id, Part::Content);
                let answer = match result {
                    Ok(content) => {
                        debug!(device = %serve.from, bytes = content.len(), "answering a clipboard pull");
                        self.clipboard.lazy_stats.served += 1;
                        Outbound::Clipboard(ClipboardTransfer {
                            request_id: serve.pull_id,
                            origin: self.local.to_string(),
                            content,
                        })
                    }
                    Err(reason) => {
                        debug!(device = %serve.from, ?reason, "cannot answer a clipboard pull");
                        skipped_message(serve.pull_id, reason)
                    }
                };
                self.send(&serve.from, answer);
            }
            LocalDone::Written {
                purpose,
                formats,
                bytes,
                result,
            } => match (purpose, result) {
                (WritePurpose::Arrived { origin }, Ok(())) => {
                    self.clipboard.held = None;
                    self.note_formats(origin, self.local.clone(), formats, None);
                }
                (WritePurpose::Pulled { held }, Ok(())) => {
                    self.forget_held(&held);
                    self.pulled(&held.origin, bytes);
                    self.note_formats(held.origin, self.local.clone(), formats, None);
                }
                (WritePurpose::Pulled { held }, Err(Unwritten::Superseded)) => {
                    debug!(origin = %held.origin, "something was copied here meanwhile; the pulled clipboard is dropped");
                    self.forget_held(&held);
                    let stats = &mut self.clipboard.lazy_stats;
                    stats.superseded += 1;
                    stats.last = Some("superseded".to_owned());
                }
                (WritePurpose::Pulled { held }, Err(Unwritten::TooLarge)) => {
                    debug!(origin = %held.origin, bytes, "the pulled clipboard is too large for this one");
                    self.forget_held(&held);
                    self.count_failure(ClipboardSkip::TooLarge);
                    self.note_skip(held.origin, self.local.clone(), ClipboardSkip::TooLarge);
                }
                (WritePurpose::Pulled { held }, Err(Unwritten::Failed)) => {
                    self.forget_held(&held);
                    self.count_failure(ClipboardSkip::Unavailable);
                }
                (WritePurpose::Arrived { .. }, Err(_)) => {}
            },
            LocalDone::Promised { held, anew, result } => match result {
                Ok(change) => {
                    self.clipboard.promised = Some((held.serial, change));
                    // The same offer as before, now with its files in place.
                    if !anew {
                        self.clipboard.lazy_stats.received += 1;
                        self.note_formats(held.origin, self.local.clone(), held.formats, None);
                    }
                }
                // Not made anew after all: its files are let go as if
                // something else were copied over it.
                Err(why) if anew => {
                    debug!(origin = %held.origin, ?why, "the pasted files that arrived late are not promised anew");
                    self.end_offer(&held.origin, held.offer_id);
                }
                // Files cannot be written whole: offers of files are not
                // taken by such a clipboard, and nothing else is left.
                Err(Unpromised::Unsupported { .. }) if held.formats.is_empty() => {
                    self.forget_held(&held);
                }
                Err(Unpromised::Unsupported { change }) => {
                    debug!(origin = %held.origin, "the clipboard cannot promise; pulling the offered content now");
                    self.clipboard.lazy_stats.received += 1;
                    let via = held.via.clone();
                    let waiter = Waiter::Write {
                        held: held.clone(),
                        base: change,
                    };
                    let pull = self.clipboard.pulls.start(
                        waiter,
                        via.clone(),
                        pull_of(&held),
                        Instant::now(),
                    );
                    self.send(&via, pull_message(pull));
                }
                // Copied over here while it was promised, as while the
                // Windows app was waited for: that copy is what the
                // clipboard holds, and the offer is let go.
                Err(Unpromised::Superseded) => {
                    debug!(origin = %held.origin, "something was copied here while the offered clipboard was promised");
                    self.end_offer(&held.origin, held.offer_id);
                    let stats = &mut self.clipboard.lazy_stats;
                    stats.superseded += 1;
                    stats.last = Some("superseded".to_owned());
                }
                Err(Unpromised::TooLarge) => {
                    debug!(origin = %held.origin, bytes = held.bytes, "an offered clipboard is too large for this one");
                    self.forget_held(&held);
                    self.note_skip(held.origin, self.local.clone(), ClipboardSkip::TooLarge);
                }
                // Not on the clipboard after all, as when the broker did
                // not answer: nothing moved.
                Err(Unpromised::Failed) => {
                    self.forget_held(&held);
                    self.count_failure(ClipboardSkip::Unavailable);
                }
            },
        }
    }

    fn on_clipboard_skipped(&mut self, from: DeviceId, skipped: ClipboardSkipped) {
        let busy = skipped.reason() == ClipboardSkip::Busy;
        if let Some(pull) = self
            .clipboard
            .file_pulls
            .get_mut(&skipped.id)
            .filter(|p| p.origin == from)
        {
            if busy {
                // Asked again a little later each time, while it waits for
                // them: the origin is there, only busy.
                let wait = busy_backoff(pull.backoff);
                pull.backoff = Some(wait);
                self.files.clipboard_alive(&from, pull.offer_id);
                self.retry_later(
                    EngineEvent::RetryFilesPull {
                        pull_id: skipped.id,
                    },
                    wait,
                );
                return;
            }
            debug!(device = %from, reason = ?skipped.reason(), "the files an app pastes cannot be had");
            self.clipboard.file_pulls.remove(&skipped.id);
            return;
        }
        if is_pull(skipped.id) {
            let retry = busy
                .then(|| {
                    self.clipboard
                        .pulls
                        .retry_in(skipped.id, &from, Instant::now())
                })
                .flatten();
            if let Some(wait) = retry {
                self.retry_later(
                    EngineEvent::RetryPull {
                        pull_id: skipped.id,
                    },
                    wait,
                );
                return;
            }
            if let Some(waiter) = self.clipboard.pulls.finish(skipped.id, &from) {
                self.pull_failed(waiter, skipped.reason());
            }
            return;
        }
        if let Some(to) = self.clipboard.requests.remove(&skipped.id) {
            self.note_skip(from, to, skipped.reason());
        }
    }

    /// Sends `event` to the engine after `wait`, to ask again for what was
    /// answered busy.
    fn retry_later(&self, event: EngineEvent, wait: Duration) {
        let events = self.events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            if let Some(engine) = events.upgrade() {
                let _ = engine.send(event);
            }
        });
    }

    /// A clipboard offered rather than sent: one this device asked for, to
    /// take or pass on, or the controller's own as the cursor came here.
    fn on_clipboard_offer(&mut self, from: DeviceId, offer: ClipboardOffer) {
        let to = if offer.request_id == 0 {
            self.local.clone()
        } else {
            match self.clipboard.requests.remove(&offer.request_id) {
                Some(to) => to,
                None => return,
            }
        };
        let offered_files = offered_files(&offer.items);
        let offered = offered_files.as_ref().and_then(|files| {
            offered_formats(
                &offer.parts,
                lazy_max(self.clipboard.max),
                !files.is_empty(),
            )
        });
        let (Some(offered), Some(files)) = (
            offered.filter(|_| iw_net::is_valid_device_id(&offer.origin_device_id)),
            offered_files,
        ) else {
            warn!(device = %from, "ignoring a malformed clipboard offer");
            return;
        };
        let bytes = offered.iter().map(|(_, size)| size).sum();
        let formats: Vec<ClipboardFormat> = offered.into_iter().map(|(f, _)| f).collect();
        let origin = DeviceId::new(&offer.origin_device_id);
        if self.clipboard_off(&from) || self.clipboard_off(&origin) || self.clipboard_off(&to) {
            debug!(%from, %origin, %to, "dropping a clipboard offer: it is turned off with a device involved");
            return;
        }
        if to != self.local {
            if !self.speaks_offers(&to) {
                // That device takes clipboards only whole, up to the eager
                // limit.
                self.note_skip(origin, to, ClipboardSkip::TooLarge);
                return;
            }
            let items = if self.speaks_files(&to) {
                offer.items
            } else {
                if !offer.items.is_empty() {
                    debug!(%to, "that device takes no files in clipboard offers; passing on the rest");
                }
                if offer.parts.is_empty() {
                    // Files alone, and that device takes none: nothing
                    // goes. Noted as any skip; nobody else asked for it.
                    self.note_formats(origin, to, Vec::new(), Some(ClipboardSkip::Unavailable));
                    self.count_failure(ClipboardSkip::Unavailable);
                    return;
                }
                Vec::new()
            };
            debug!(%from, %to, "passing a clipboard offer on");
            self.note_formats(origin, to.clone(), formats, None);
            let forwarded = Control {
                msg: Some(control::Msg::ClipboardOffer(ClipboardOffer {
                    request_id: 0,
                    items,
                    ..offer
                })),
            };
            self.send(&to, Outbound::Control(forwarded));
            return;
        }
        // Files come straight from the origin, into a folder of this
        // device's, when an app pastes them.
        let files = if self.takes_files_from(&origin) {
            files
        } else {
            if !files.is_empty() {
                debug!(%origin, "the files of a clipboard offer cannot come here");
            }
            Vec::new()
        };
        if formats.is_empty() && files.is_empty() {
            return;
        }
        self.take_offer(Held {
            origin,
            offer_id: offer.offer_id,
            via: from,
            formats,
            bytes,
            files,
            serial: 0,
        });
    }

    /// Whether files offered from `origin`'s clipboard can be pasted here:
    /// fetched straight from it, into a folder kept for them (the agent's,
    /// or one the clipboard chooses itself), and promised by this device's
    /// clipboard.
    fn takes_files_from(&self, origin: &DeviceId) -> bool {
        self.clipboard.local.as_ref().is_some_and(|local| {
            local.promises_files() && (local.stages_files() || self.clipboard.staging.is_some())
        }) && self.links.contains_key(origin)
            && self.speaks_files(origin)
            && !self.files_off(origin)
    }

    /// This device's clipboard takes an offer: promised to its apps, or,
    /// where it cannot promise, pulled at once and written as it arrives
    /// (`on_local`). An offer larger than the clipboard takes in one write
    /// is not taken.
    fn take_offer(&mut self, held: Held) {
        self.promise(held, None);
    }

    /// Has the clipboard promise `held`, under a serial of its own. With
    /// `anew`, the files already arrived and the promise is made anew, only
    /// if the clipboard still has the change count (`anew.0`) the one
    /// before left it at: nothing written or copied since.
    fn promise(&mut self, mut held: Held, anew: Option<(u64, Arc<FilesWait>)>) {
        let sync = &mut self.clipboard;
        let Some(local) = sync.local.as_ref().filter(|_| sync.on) else {
            return;
        };
        sync.serial += 1;
        held.serial = sync.serial;
        // Made anew, it keeps what the one it replaces pulled.
        let (anew, files, pulled) = match anew {
            Some((change, files)) => (
                Some(change),
                files,
                sync.source.as_ref().and_then(|s| s.pulled()),
            ),
            None => (None, Arc::default(), None),
        };
        let source = Arc::new(
            PromiseSource::with_files(
                self.events.clone(),
                held.origin.clone(),
                held.offer_id,
                held.serial,
                files,
            )
            .waiting(sync.paste_wait)
            .keeping(pulled),
        );
        let promise = ClipboardPromise {
            formats: held.formats.clone(),
            bytes: held.bytes,
            files: held.files.iter().map(OfferedFile::promised).collect(),
            source: source.clone(),
        };
        local.ask(LocalOp::Promise {
            promise,
            held: held.clone(),
            anew,
        });
        sync.source = Some(source);
        sync.promised = None;
        // Known before an app can paste it; forgotten if it is not
        // promised after all.
        sync.held = Some(held);
    }

    /// The files of a pull ended. If they arrived after an app gave up
    /// waiting for them, the clipboard, if it still holds their offer,
    /// promises them anew, now with them in place: macOS never asks a
    /// promise again for what it once did not get.
    fn files_ended(&mut self, pull_id: u64, late: Option<Vec<Option<PathBuf>>>) {
        let Some(pull) = self.clipboard.file_pulls.remove(&pull_id) else {
            return;
        };
        let (Some(files), Some(held)) = (late, self.clipboard.held.clone()) else {
            return;
        };
        if !held.is(&pull.origin, pull.offer_id) {
            return;
        }
        match self.clipboard.promised {
            Some((serial, change)) if serial == held.serial => {
                debug!(origin = %pull.origin, "the pasted files arrived late; promising them anew");
                self.promise(held, Some((change, FilesWait::ready(files))));
            }
            // Not known to be on the clipboard yet: nothing to make anew.
            _ => self.end_offer(&pull.origin, pull.offer_id),
        }
    }

    /// The offer `held` is no longer this device's clipboard.
    fn forget_held(&mut self, held: &Held) {
        if self.clipboard.held.as_ref() == Some(held) {
            self.clipboard.held = None;
            self.clipboard.source = None;
        }
    }

    fn count_failure(&mut self, reason: ClipboardSkip) {
        let stats = &mut self.clipboard.lazy_stats;
        stats.failed += 1;
        stats.last = Some(reason.as_str_name().to_owned());
    }

    /// An app here pastes the offer this device's clipboard promises: it is
    /// pulled through the device the offer came through.
    fn pull_for_app(
        &mut self,
        origin: DeviceId,
        offer_id: u64,
        reply: std::sync::mpsc::Sender<Option<ClipboardContent>>,
    ) {
        let held = match &self.clipboard.held {
            Some(held) if held.is(&origin, offer_id) => held.clone(),
            _ => {
                let _ = reply.send(None);
                return;
            }
        };
        let reachable = self.clipboard.on
            && self.links.contains_key(&held.via)
            && !self.clipboard_off(&held.via)
            && !self.clipboard_off(&origin);
        if !reachable {
            self.pull_failed(Waiter::App(reply), ClipboardSkip::Unavailable);
            return;
        }
        debug!(%origin, via = %held.via, "pulling the clipboard an app pastes");
        let pull = self.clipboard.pulls.start(
            Waiter::App(reply),
            held.via.clone(),
            pull_of(&held),
            Instant::now(),
        );
        self.send(&held.via, pull_message(pull));
    }

    /// A file manager here pastes the files the clipboard promises: they
    /// are fetched straight from the origin, into `folder` when the
    /// platform chose one, which was checked, or else into a folder kept
    /// for them, made first off the engine; `wait` hears where they are
    /// once they all ended.
    fn pull_files_for_app(
        &mut self,
        origin: DeviceId,
        offer_id: u64,
        wait: Arc<FilesWait>,
        folder: Option<PathBuf>,
    ) {
        if !self.files_pullable(&origin, offer_id) {
            wait.arrived(None);
            return;
        }
        if let Some(folder) = folder {
            self.on_staging_made(origin, offer_id, wait, Ok(folder));
            return;
        }
        let Some(staging) = self.clipboard.staging.clone() else {
            wait.arrived(None);
            return;
        };
        let events = self.events.clone();
        tokio::spawn(async move {
            let from = origin.clone();
            let made = tokio::task::spawn_blocking(move || staging.make(&from, offer_id))
                .await
                .unwrap_or_else(|e| Err(std::io::Error::other(e)));
            let made = EngineEvent::StagingMade {
                origin,
                offer_id,
                wait: wait.clone(),
                made,
            };
            if events
                .upgrade()
                .is_none_or(|engine| engine.send(made).is_err())
            {
                wait.arrived(None);
            }
        });
    }

    /// Whether the files of offer `offer_id` from `origin` can be fetched
    /// when pasted: the clipboard holds that offer and they can come.
    fn files_pullable(&self, origin: &DeviceId, offer_id: u64) -> bool {
        let sync = &self.clipboard;
        sync.held.as_ref().is_some_and(|held| {
            held.is(origin, offer_id)
                && !held.files.is_empty()
                && sync.on
                && self.takes_files_from(origin)
        })
    }

    /// The folder for pasted files was made: the origin is asked for them,
    /// if the clipboard still holds their offer.
    fn on_staging_made(
        &mut self,
        origin: DeviceId,
        offer_id: u64,
        wait: Arc<FilesWait>,
        made: std::io::Result<PathBuf>,
    ) {
        let folder = match made {
            Ok(folder) => folder,
            Err(e) => {
                warn!(error = %e, "cannot make a folder to fetch pasted files into");
                wait.arrived(None);
                return;
            }
        };
        let pullable = self.files_pullable(&origin, offer_id);
        let Some(held) = self.clipboard.held.clone().filter(|_| pullable) else {
            // Copied over meanwhile: the folder goes, unless files of the
            // offer came into it already.
            let key = (origin, offer_id);
            if let Some(staging) = &self.clipboard.staging
                && !self.clipboard.staged.contains(&key)
            {
                staging.remove_later(&key.0, offer_id);
            }
            wait.arrived(None);
            return;
        };
        let arrival = self.files.expect_clipboard(&origin, offer_id, folder);
        self.clipboard.staged.insert((origin.clone(), offer_id));
        let sync = &mut self.clipboard;
        sync.next_files_pull = (sync.next_files_pull + 1) & !(PULL_BIT | FILES_PULL_BIT);
        let pull_id = sync.next_files_pull | PULL_BIT | FILES_PULL_BIT;
        let promised: Vec<u32> = held.files.iter().map(|f| f.index).collect();
        let (expected, expecting) = watch::channel(None);
        sync.file_pulls.insert(
            pull_id,
            FilesPull {
                origin: origin.clone(),
                offer_id,
                promised: promised.clone(),
                expected,
                backoff: None,
            },
        );
        let (files, events, from) = (self.files.clone(), self.events.clone(), origin.clone());
        let (count, patience) = (promised.len(), sync.patience);
        tokio::spawn(async move {
            let late = clipboard_files::await_files(
                files,
                (from, offer_id),
                promised,
                (arrival, expecting),
                wait,
                patience,
            )
            .await;
            if let Some(engine) = events.upgrade() {
                let _ = engine.send(EngineEvent::ClipboardFilesEnded { pull_id, late });
            }
        });
        debug!(%origin, files = count, "fetching the files an app pastes");
        self.send(&origin, files_pull_message(pull_id, offer_id));
    }

    /// The origin says which of the offer's files it sends.
    fn on_clipboard_files_sending(&mut self, from: DeviceId, sending: ClipboardFilesSending) {
        let Some(pull) = self
            .clipboard
            .file_pulls
            .get(&sending.pull_id)
            .filter(|p| p.origin == from)
        else {
            return;
        };
        // Heard from the origin: they are listed, or, said again, still
        // wait their turn there.
        self.files.clipboard_alive(&from, pull.offer_id);
        if pull.expected.borrow().is_some() {
            return;
        }
        match clipboard_files::sent_items(&sending.items, &pull.promised) {
            Some(items) if !items.is_empty() => {
                pull.expected.send_replace(Some(items));
            }
            _ => {
                debug!(device = %from, "none of the files an app pastes come");
                self.clipboard.file_pulls.remove(&sending.pull_id);
            }
        }
    }

    /// Something else was copied over the `serial`th promise of
    /// `offer_id` from `origin`. Its files, if they were fetched, are no
    /// longer wanted: what still comes is cancelled, and their folder goes
    /// once an app copying from it is likely done. A promise of the same
    /// offer made anew is not something else.
    fn promise_gone(&mut self, origin: DeviceId, offer_id: u64, serial: u64) {
        let replaced = self
            .clipboard
            .held
            .as_ref()
            .is_some_and(|h| h.is(&origin, offer_id) && h.serial != serial);
        if !replaced {
            self.end_offer(&origin, offer_id);
        }
    }

    /// The offer `offer_id` from `origin` is no longer this device's
    /// clipboard: forgotten, and its files let go, their folder after the
    /// grace time.
    fn end_offer(&mut self, origin: &DeviceId, offer_id: u64) {
        let sync = &mut self.clipboard;
        if sync.held.as_ref().is_some_and(|h| h.is(origin, offer_id)) {
            sync.held = None;
            sync.source = None;
        }
        sync.file_pulls
            .retain(|_, p| !(p.origin == *origin && p.offer_id == offer_id));
        if sync.staged.remove(&(origin.clone(), offer_id)) {
            self.files.cancel_clipboard(origin, offer_id);
            self.files.forget_clipboard(origin, offer_id);
            if let Some(staging) = &self.clipboard.staging {
                staging.remove_later(origin, offer_id);
            }
        }
    }

    /// Another device pastes the files of an offer this device made: sent
    /// straight to it, the very files offered, while the clipboard still
    /// holds what was offered (checked on the clipboard's thread).
    fn on_clipboard_files_pull(&mut self, from: DeviceId, pull: ClipboardFilesPull) {
        if !is_pull(pull.pull_id) {
            warn!(device = %from, "ignoring a malformed pull of clipboard files");
            return;
        }
        if self.clipboard_off(&from) || self.files_off(&from) {
            self.refuse(&from, pull.pull_id, ClipboardSkip::Disabled);
            return;
        }
        let sync = &mut self.clipboard;
        let refused = match (&sync.local, sync.own_offers.files_of(pull.offer_id)) {
            _ if !sync.on => Some(ClipboardSkip::Disabled),
            (None, _) => Some(ClipboardSkip::Unavailable),
            (_, None) => Some(ClipboardSkip::Changed),
            (Some(_), Some(_)) if !sync.serving.admit(&from, pull.offer_id, Part::Files) => {
                debug!(device = %from, "already sending the files of this device's clipboard to it");
                Some(ClipboardSkip::Busy)
            }
            (Some(local), Some((change, _))) => {
                local.ask(LocalOp::CheckFiles {
                    pull: FilesServe {
                        from: from.clone(),
                        pull_id: pull.pull_id,
                        offer_id: pull.offer_id,
                    },
                    change,
                });
                None
            }
        };
        if let Some(reason) = refused {
            self.refuse(&from, pull.pull_id, reason);
        }
    }

    /// Whether the clipboard still held the offer whose files are pulled:
    /// if so, they are listed again, off the engine, to be sent.
    fn on_files_checked(&mut self, pull: FilesServe, result: Result<(), ClipboardSkip>) {
        let paths = result.and_then(|()| {
            self.clipboard
                .own_offers
                .files_of(pull.offer_id)
                .map(|(_, paths)| paths)
                .ok_or(ClipboardSkip::Changed)
        });
        // Every item going is listed in every message to the apps: with
        // too many going already, it is not listed only to find that out.
        let paths = paths.and_then(|paths| {
            let going = self.files.sending_items();
            if going + paths.len() > MAX_ITEMS {
                debug!(
                    going,
                    "too many files going to list those pasted elsewhere now"
                );
                return Err(ClipboardSkip::Busy);
            }
            Ok(paths)
        });
        let paths = match paths {
            Ok(paths) => paths,
            Err(reason) => {
                self.clipboard
                    .serving
                    .done(&pull.from, pull.offer_id, Part::Files);
                self.refuse(&pull.from, pull.pull_id, reason);
                return;
            }
        };
        let (files, events) = (self.files.clone(), self.events.clone());
        tokio::spawn(async move {
            // Listed whole only now: offered, they were looked at the top.
            let listed = files.manifest(paths.to_vec()).await.map_err(|e| {
                // By its code: what it says names the user's files.
                debug!(
                    reason = e.code(),
                    "the files pasted elsewhere cannot go now"
                );
                match e {
                    SendError::TooManyItems(_) => ClipboardSkip::Busy,
                    _ => ClipboardSkip::Unavailable,
                }
            });
            if let Some(engine) = events.upgrade() {
                let _ = engine.send(EngineEvent::ClipboardFilesListed { pull, listed });
            }
        });
    }

    /// The files of an offer this device made were listed again: those
    /// that can go are sent as a batch for that offer, which the device
    /// pasting them writes only into the folder it keeps for it.
    fn on_clipboard_files_listed(
        &mut self,
        pull: FilesServe,
        listed: Result<Manifest, ClipboardSkip>,
    ) {
        self.clipboard
            .serving
            .done(&pull.from, pull.offer_id, Part::Files);
        let from = pull.from.clone();
        if self.clipboard_off(&from) || self.files_off(&from) {
            self.refuse(&from, pull.pull_id, ClipboardSkip::Disabled);
            return;
        }
        let items: Vec<u32> = listed
            .iter()
            .flat_map(|m| m.items.iter().enumerate())
            .filter(|(_, item)| item.sendable())
            .map(|(i, _)| i as u32)
            .collect();
        let manifest = match listed {
            Ok(manifest) if !items.is_empty() && self.links.contains_key(&from) => manifest,
            Ok(_) => {
                self.refuse(&from, pull.pull_id, ClipboardSkip::Unavailable);
                return;
            }
            Err(reason) => {
                self.refuse(&from, pull.pull_id, reason);
                return;
            }
        };
        info!(device = %from, items = items.len(), "sending the files of this device's clipboard, pasted there");
        let sending = Control {
            msg: Some(control::Msg::ClipboardFilesSending(ClipboardFilesSending {
                pull_id: pull.pull_id,
                items: items.clone(),
            })),
        };
        self.send(&from, Outbound::Control(sending));
        let rows =
            self.files
                .send_manifest(from.as_str(), &manifest, Carrier::Clipboard(pull.offer_id));
        self.clipboard.lazy_stats.served += 1;
        // Whenever they, or files of their folders, wait their turn behind
        // other files sent that device or are hashed, until all of them
        // ended, it hears now and then that they still come. This lives as
        // long as they do: rows waiting for a device that went away end
        // when it comes back or someone cancels them.
        let (files, events) = (self.files.clone(), self.events.clone());
        let every = self.clipboard.patience.note_every();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let Some(engine) = events.upgrade() else {
                    return;
                };
                match files.waiting(&rows) {
                    None => return,
                    Some(false) => continue,
                    Some(true) => {}
                }
                let waiting = EngineEvent::ClipboardFilesWaiting {
                    to: from.clone(),
                    pull_id: pull.pull_id,
                    items: items.clone(),
                };
                if engine.send(waiting).is_err() {
                    return;
                }
            }
        });
    }

    /// Another device pulls an offered clipboard: this device's own, read
    /// on the clipboard's thread while it still holds what was offered, or
    /// passed on to the device it is.
    fn on_clipboard_pull(&mut self, from: DeviceId, pull: ClipboardPull) {
        if !is_pull(pull.pull_id) || !iw_net::is_valid_device_id(&pull.origin_device_id) {
            warn!(device = %from, "ignoring a malformed clipboard pull");
            return;
        }
        let origin = DeviceId::new(&pull.origin_device_id);
        if self.clipboard_off(&from) || self.clipboard_off(&origin) {
            self.refuse(&from, pull.pull_id, ClipboardSkip::Disabled);
            return;
        }
        if origin == self.local {
            let sync = &mut self.clipboard;
            let refused = match (&sync.local, sync.own_offers.change_of(pull.offer_id)) {
                _ if !sync.on => Some(ClipboardSkip::Disabled),
                (None, _) => Some(ClipboardSkip::Unavailable),
                (_, None) => Some(ClipboardSkip::Changed),
                (Some(_), Some(_)) if !sync.serving.admit(&from, pull.offer_id, Part::Content) => {
                    debug!(device = %from, "already reading this device's clipboard for it");
                    Some(ClipboardSkip::Busy)
                }
                (Some(local), Some(change)) => {
                    local.ask(LocalOp::Serve {
                        serve: Serve {
                            from: from.clone(),
                            pull_id: pull.pull_id,
                            offer_id: pull.offer_id,
                            mimes: pull.mimes,
                        },
                        change,
                        limit: lazy_max(sync.max),
                    });
                    None
                }
            };
            if let Some(reason) = refused {
                self.refuse(&from, pull.pull_id, reason);
            }
            return;
        }
        if !self.speaks_offers(&origin) {
            self.refuse(&from, pull.pull_id, ClipboardSkip::Unavailable);
            return;
        }
        if !self.clipboard.pulls.may_relay(&from) {
            self.refuse(&from, pull.pull_id, ClipboardSkip::Busy);
            return;
        }
        debug!(%from, %origin, "passing a clipboard pull on");
        let waiter = Waiter::Relay {
            to: from,
            id: pull.pull_id,
        };
        let forwarded = self
            .clipboard
            .pulls
            .start(waiter, origin.clone(), pull, Instant::now());
        self.send(&origin, pull_message(forwarded));
    }

    /// A pulled clipboard arrived: for an app here, to write here, or to
    /// pass back to the device that pulled through this one.
    fn on_pulled(&mut self, from: DeviceId, clipboard: ClipboardTransfer) {
        let Some(waiter) = self.clipboard.pulls.finish(clipboard.request_id, &from) else {
            debug!(device = %from, "dropping a pulled clipboard nobody waits for");
            return;
        };
        let origin = DeviceId::new(&clipboard.origin);
        if self.clipboard_off(&from)
            || self.clipboard_off(&origin)
            || clipboard.content.len() > lazy_max(self.clipboard.max)
        {
            self.pull_failed(waiter, ClipboardSkip::Disabled);
            return;
        }
        let bytes = clipboard.content.len() as u64;
        match waiter {
            Waiter::App(reply) => {
                debug!(%origin, bytes, "an app here pastes the pulled clipboard");
                self.pulled(&origin, bytes);
                let _ = reply.send(Some(clipboard.content));
            }
            Waiter::Write { held, base } => {
                let sync = &self.clipboard;
                let current = sync.held.as_ref() == Some(&held);
                if let Some(local) = sync.local.as_ref().filter(|_| current && sync.on) {
                    local.ask(LocalOp::Write {
                        content: clipboard.content,
                        expect: Some(base),
                        purpose: WritePurpose::Pulled { held },
                    });
                }
            }
            Waiter::Relay { to, id } => {
                if self.clipboard_off(&to) {
                    self.send(&to, skipped_message(id, ClipboardSkip::Disabled));
                    return;
                }
                let back = ClipboardTransfer {
                    request_id: id,
                    ..clipboard
                };
                self.send(&to, Outbound::Clipboard(back));
            }
        }
    }

    fn pulled(&mut self, origin: &DeviceId, bytes: u64) {
        let stats = &mut self.clipboard.lazy_stats;
        stats.pulled += 1;
        stats.bytes_pulled += bytes;
        stats.last = Some(format!("pulled {bytes} bytes from {origin}"));
    }

    /// A pull came to nothing: a device it was passed on for hears why,
    /// and an app here gets nothing.
    fn pull_failed(&mut self, waiter: Waiter, reason: ClipboardSkip) {
        if let Waiter::Relay { to, id } = waiter {
            self.refuse(&to, id, reason);
            return;
        }
        debug!(?reason, "an offered clipboard could not be pulled");
        self.count_failure(reason);
        match waiter {
            Waiter::App(reply) => {
                let _ = reply.send(None);
            }
            Waiter::Write { held, .. } => self.forget_held(&held),
            Waiter::Relay { .. } => {}
        }
    }

    /// Pulls nobody answered in time come to nothing.
    fn expire_pulls(&mut self, now: Instant) {
        for waiter in self.clipboard.pulls.expire(now) {
            self.pull_failed(waiter, ClipboardSkip::Unavailable);
        }
    }

    /// A clipboard arrived: one this device asked for, to keep or pass on,
    /// or one the controller sent as the cursor came here.
    fn on_clipboard(&mut self, from: DeviceId, clipboard: ClipboardTransfer) {
        if is_pull(clipboard.request_id) {
            self.on_pulled(from, clipboard);
            return;
        }
        let to = if clipboard.request_id == 0 {
            self.local.clone()
        } else {
            match self.clipboard.requests.remove(&clipboard.request_id) {
                Some(to) => to,
                None => return,
            }
        };
        let origin = DeviceId::new(&clipboard.origin);
        if self.clipboard_off(&from) || self.clipboard_off(&origin) || self.clipboard_off(&to) {
            debug!(%from, %origin, %to, "dropping a clipboard: it is turned off with a device involved");
            return;
        }
        if to != self.local {
            debug!(%from, %to, "passing a clipboard on");
            self.note_clipboard(origin, to.clone(), &clipboard.content, None);
            let forwarded = ClipboardTransfer {
                request_id: 0,
                ..clipboard
            };
            self.send(&to, Outbound::Clipboard(forwarded));
            return;
        }
        if !self.clipboard.on {
            return;
        }
        if let Some(local) = &self.clipboard.local {
            local.ask(LocalOp::Write {
                content: clipboard.content,
                expect: None,
                purpose: WritePurpose::Arrived { origin },
            });
        }
    }

    fn note_clipboard(
        &mut self,
        from: DeviceId,
        to: DeviceId,
        content: &ClipboardContent,
        skipped: Option<ClipboardSkip>,
    ) {
        let formats = content.items.iter().map(|(f, _)| *f).collect();
        self.note_formats(from, to, formats, skipped);
    }

    /// Notes a clipboard moving, or offered, in `formats`.
    fn note_formats(
        &mut self,
        from: DeviceId,
        to: DeviceId,
        formats: Vec<ClipboardFormat>,
        skipped: Option<ClipboardSkip>,
    ) {
        self.clipboard.last = Some(ClipboardActivity {
            at: std::time::SystemTime::now(),
            from: from.to_string(),
            to: to.to_string(),
            formats,
            skipped,
        });
    }

    /// Only skips the user would want to know about are reported.
    fn note_skip(&mut self, from: DeviceId, to: DeviceId, reason: ClipboardSkip) {
        if matches!(reason, ClipboardSkip::Sensitive | ClipboardSkip::TooLarge) {
            self.note_clipboard(from, to, &ClipboardContent::default(), Some(reason));
        }
    }

    /// A peer's copy of the arrangement: adopt it if newer, and pass it on.
    fn on_layout(&mut self, from: DeviceId, layout: peer::Layout) {
        let finite = layout.placements.iter().all(|p| {
            p.x.is_finite() && p.y.is_finite() && iw_net::is_valid_device_id(&p.device_id)
        });
        if !finite || layout.placements.len() > MAX_PLACEMENTS {
            warn!(device = %from, "ignoring a malformed arrangement");
            return;
        }
        let doc = LayoutDoc {
            version: layout.version,
            author: layout.author,
            placements: layout
                .placements
                .into_iter()
                .map(|p| PlacementConfig {
                    device: p.device_id,
                    x: p.x,
                    y: p.y,
                })
                .collect(),
        };
        if !self.directory.adopt(doc.clone()) {
            return;
        }
        info!(version = doc.version, author = %doc.author, "arrangement updated");
        self.broadcast_layout(&doc, Some(&from));
        self.rebuild_layout();
    }

    fn broadcast_layout(&self, doc: &LayoutDoc, except: Option<&DeviceId>) {
        let msg = layout_message(doc);
        for (device, link) in &self.links {
            if Some(device) != except {
                let _ = link.tx.send(Outbound::Control(msg.clone()));
            }
        }
    }

    /// Moves devices on the canvas as the user asked, if the result is a
    /// valid layout, and shares the new arrangement.
    fn rearrange(&mut self, moves: Vec<(String, Point)>) -> Result<(), RearrangeError> {
        let configured: Vec<&str> = self
            .config
            .layout
            .iter()
            .map(|p| p.device.as_str())
            .collect();
        for (device, offset) in &moves {
            if !offset.x.is_finite() || !offset.y.is_finite() {
                return Err(RearrangeError::NotFinite(device.clone()));
            }
            if device != self.local.as_str() && !self.peers.iter().any(|p| p.id == *device) {
                return Err(RearrangeError::UnknownDevice(device.clone()));
            }
            if configured.contains(&device.as_str()) {
                return Err(RearrangeError::Configured(device.clone()));
            }
        }
        let mut placements = self.directory.placements();
        for (device, offset) in &moves {
            placements.retain(|p| p.device != *device);
            placements.push(PlacementConfig {
                device: device.clone(),
                x: offset.x,
                y: offset.y,
            });
        }
        let devices: Vec<Device> = self
            .known_devices()
            .into_iter()
            .filter_map(|(id, name, displays)| {
                let p = placements.iter().find(|p| p.device == id.as_str())?;
                Some(Device {
                    id,
                    name,
                    displays,
                    offset: Point::new(p.x, p.y),
                })
            })
            .collect();
        Layout::new(devices).map_err(|e| RearrangeError::Invalid(e.to_string()))?;
        let doc = self.directory.rearrange(&moves);
        info!(version = doc.version, "arrangement changed here");
        self.broadcast_layout(&doc, None);
        self.rebuild_layout();
        Ok(())
    }

    /// This device and every trusted device whose displays are known.
    fn known_devices(&self) -> Vec<(DeviceId, String, Vec<Display>)> {
        let mut out = vec![(
            self.local.clone(),
            self.this.name(),
            self.local_displays.clone(),
        )];
        for peer in self.peers.iter() {
            let id = DeviceId::new(&peer.id);
            if let Some(displays) = self.peer_displays.get(&id).filter(|d| !d.is_empty()) {
                out.push((id, peer.name.clone(), displays.clone()));
            }
        }
        out
    }

    fn route(&mut self, event: Event) {
        self.route_from(event, false);
    }

    /// Routes input capture reported, which capture hears an answer about
    /// (`InputCapture::edge_wait`).
    fn route_captured(&mut self, input: InputEvent) {
        self.route_from(Event::Input(input), true);
    }

    /// Routes `event`, `captured` if capture reported it: only such an event
    /// is answered, as capture counts those it reported (`capture_events`).
    fn route_from(&mut self, event: Event, captured: bool) {
        let Some(router) = self.router.as_mut() else {
            return;
        };
        // Input moves the cursor, and seldom changes where it may leave:
        // only by the lock hotkey.
        let input = matches!(event, Event::Input(_));
        let actions = router.handle(event, Instant::now());
        let locked = actions.iter().any(|a| matches!(a, Action::LockChanged(_)));
        // Capture that holds input at an edge as the desktop hands it over
        // lets go of it unless the cursor crosses or waits to.
        let waiting =
            (captured && *router.state() == router::State::Local).then(|| router.waiting_at_edge());
        self.apply(actions);
        if let Some(waiting) = waiting {
            self.capture.edge_wait(self.capture_events, waiting);
        }
        if !input || locked {
            self.refresh_exits();
        }
    }

    /// Tells capture where the cursor leaves this device, if that changed
    /// and capture wants to know: nowhere while another device controls
    /// this one, whose input reaching the edges is not this device's own.
    fn refresh_exits(&mut self) {
        if !self.capture.wants_exits() {
            return;
        }
        let exits = match &self.router {
            Some(router) if self.receiver.controller().is_none() => router.exits(),
            _ => Vec::new(),
        };
        if exits != self.exits {
            debug!(
                count = exits.len(),
                "where the cursor leaves this device changed"
            );
            self.capture.set_exits(&exits);
            self.exits = exits;
        }
    }

    fn jump_to(&mut self, device: String) -> Result<(), JumpError> {
        let id = DeviceId::new(&device);
        let here = id == self.local;
        if !here && !self.peers.iter().any(|p| p.id == device) {
            return Err(JumpError::UnknownDevice(device));
        }
        if let Some(controller) = self.receiver.controller() {
            return Err(JumpError::ControlledBy(controller.to_string()));
        }
        // Offline for now comes before every other reason, connected or not,
        // as the apps show it.
        if self.offline_here() || (!here && self.offline_for_now(&id)) {
            let which = if self.offline_here() {
                self.local.clone()
            } else {
                id
            };
            return Err(router::JumpError::OfflineForNow(which).into());
        }
        // Then the profile, connected or not, as the apps show it.
        if !here {
            let reach = self.reach.borrow();
            if reach.paused {
                return Err(JumpError::ProfilePaused);
            }
            if !reach.connects(&device) {
                return Err(JumpError::NotInProfile(device));
            }
        }
        let router = self
            .router
            .as_mut()
            .ok_or_else(|| router::JumpError::NotArranged(id.clone()))?;
        // The router's refusals first: they say what is the matter there.
        let target = router.jump_target(&id)?;
        // Grabbing without capture would hide the cursor and catch nothing,
        // as would grabbing where capture only gets input at the edges.
        if !here && !self.capturing {
            return Err(JumpError::NotCapturing);
        }
        if !here && !self.capture.grab_ready() {
            return Err(JumpError::EdgeOnly);
        }
        let actions = router.jump(target);
        info!(device = %id, "jumping to a device");
        self.apply(actions);
        Ok(())
    }

    fn apply(&mut self, actions: Vec<Action>) {
        let moves = self.clipboard.relay.follow(&actions);
        self.apply_actions(actions);
        for clipboard_move in moves {
            self.move_clipboard(clipboard_move);
        }
    }

    fn apply_actions(&mut self, actions: Vec<Action>) {
        // Where the cursor crossed an edge. The router puts it first in the
        // one batch that carries the cursor over (router test
        // `every_crossing_comes_before_the_leave_and_enter_it_goes_with`),
        // so the `Leave` and `Enter` after it here can tell the devices
        // concerned where on their screens it happened; a batch has at most
        // one crossing.
        let mut crossing: Option<Crossing> = None;
        for action in actions {
            match action {
                Action::Crossed(c) => {
                    debug_assert!(crossing.is_none(), "one crossing per batch");
                    if c.left.device == self.local {
                        self.crossed_here(&c.left, false, &c.entered.device);
                    }
                    if c.entered.device == self.local {
                        self.crossed_here(&c.entered, true, &c.left.device);
                    }
                    crossing = Some(c);
                }
                Action::Grab => {
                    if let Err(e) = self.capture.set_grabbed(true) {
                        warn!(error = %e, "could not grab local input");
                    }
                }
                Action::Ungrab { warp_to } => {
                    info!(x = warp_to.x, y = warp_to.y, "cursor came back");
                    if let Err(e) = self.capture.set_grabbed(false) {
                        warn!(error = %e, "could not release local input");
                    }
                    // Where capture cannot move the cursor now (Wayland with
                    // nothing handed over, as on a jump to here from here),
                    // input emulation does, if it can.
                    if self.capture.can_warp() {
                        if let Err(e) = self.capture.warp(warp_to) {
                            warn!(error = %e, "could not move the local cursor");
                        }
                    } else if let Err(e) = self.emulation.move_to(warp_to) {
                        debug!(error = %e, "cannot move the local cursor here");
                    }
                }
                Action::DragHere { source } => {
                    // A drag carried from a device controlled crossed back
                    // here: this device's app is offered it, and presses
                    // the button for a drag of its own.
                    if self.drags.remote_source() == Some(&source) {
                        let takes = self.app_takes_drags();
                        let local = self.local.clone();
                        let arrived =
                            self.drags
                                .remote_entered(&local, &local, false, takes, Instant::now());
                        self.carry_out(arrived);
                        if self.drags.offered_here() {
                            self.route(Event::DragHeldHere);
                        }
                    }
                }
                // A key the local session saw go down as input was grabbed,
                // now let go of there; not where grabbing took the keys
                // from the local apps already (Wayland).
                Action::LocalKey { usage, pressed } => {
                    if self.capture.releases_carried_input_locally() {
                        self.inject(Injection::Key { usage, pressed });
                    }
                }
                Action::LocalButton {
                    button,
                    pressed,
                    position,
                } => {
                    // Not where grabbing took the pointer from the local
                    // apps already (Wayland), as for keys.
                    if self.capture.releases_carried_input_locally() {
                        // Injection places buttons where it last moved to.
                        self.inject(Injection::MoveTo(position));
                        self.inject(Injection::Button { button, pressed });
                    }
                }
                Action::Enter {
                    device,
                    position,
                    modifiers,
                    buttons,
                } => {
                    info!(%device, dragging = !buttons.is_empty(), "cursor entered");
                    debug_assert!(
                        crossing.as_ref().is_none_or(|c| c.entered.device == device),
                        "a crossing goes with the entry into the device it entered"
                    );
                    let crossed = crossing
                        .as_ref()
                        .filter(|c| c.entered.device == device)
                        .and_then(|c| self.edge_spot_for(&c.entered, &c.left.device));
                    let enter = peer::Enter {
                        x: position.x,
                        y: position.y,
                        modifiers: modifiers.iter().map(|m| u32::from(m.id())).collect(),
                        buttons: buttons
                            .iter()
                            .map(|b| convert::button_to_proto(*b).into())
                            .collect(),
                        crossed,
                    };
                    self.send(&device, input_msg(input::Msg::Enter(enter)));
                    // A drag is not offered to a device files are turned
                    // off with; released there, it ends.
                    if !self.files_off(&device) {
                        let offered = self.drags.entered(&device);
                        self.carry_out(offered);
                    }
                    let offerable = !self.files_off(&device) && self.speaks_remote_drag(&device);
                    let local = self.local.clone();
                    let offered = self.drags.remote_entered(
                        &device,
                        &local,
                        offerable,
                        false,
                        Instant::now(),
                    );
                    self.carry_out(offered);
                }
                Action::Leave { device } => {
                    debug_assert!(
                        crossing.as_ref().is_none_or(|c| c.left.device == device),
                        "a crossing goes with the leaving of the device it left"
                    );
                    let crossed = crossing
                        .as_ref()
                        .filter(|c| c.left.device == device)
                        .and_then(|c| self.edge_spot_for(&c.left, &c.entered.device));
                    self.send(
                        &device,
                        input_msg(input::Msg::Leave(peer::Leave { crossed })),
                    );
                    let mut withdrawn = self.drags.left(&device);
                    withdrawn.extend(self.drags.remote_left(&device));
                    self.carry_out(withdrawn);
                }
                Action::DragReleased { device } => {
                    info!(%device, "the drag was released");
                    let mut released = self.drags.released(&device, &self.local, Instant::now());
                    let local = self.local.clone();
                    released.extend(self.drags.remote_released(&device, &local));
                    self.carry_out(released);
                }
                Action::Motion { device, position } => {
                    if let Some(link) = self.links.get_mut(&device) {
                        link.motion_seq += 1;
                        let seq = link.motion_seq;
                        let motion = peer::PointerMotion {
                            seq,
                            x: position.x,
                            y: position.y,
                        };
                        self.send(&device, datagram_msg(datagram::Msg::Motion(motion)));
                    }
                }
                Action::Scroll { device, dx, dy } => {
                    if let Some(link) = self.links.get_mut(&device) {
                        link.scroll_seq += 1;
                        let seq = link.scroll_seq;
                        let scroll = peer::Scroll { seq, dx, dy };
                        self.send(&device, datagram_msg(datagram::Msg::Scroll(scroll)));
                    }
                }
                Action::Button {
                    device,
                    button,
                    pressed,
                    position,
                } => {
                    let at = position.unwrap_or_default();
                    let button = peer::PointerButton {
                        button: convert::button_to_proto(button).into(),
                        pressed,
                        x: at.x,
                        y: at.y,
                        at_pointer: position.is_none(),
                    };
                    self.send(&device, input_msg(input::Msg::Button(button)));
                }
                Action::Key {
                    device,
                    usage,
                    pressed,
                } => {
                    let key = peer::Key {
                        usage: u32::from(usage.id()),
                        pressed,
                    };
                    self.send(&device, input_msg(input::Msg::Key(key)));
                }
                Action::ControlReturned(reason) => {
                    info!(?reason, "control returned to this device");
                    self.last_return = Some(reason);
                    self.returns += 1;
                }
                Action::LockChanged(locked) => {
                    info!(locked, "cursor lock changed");
                    self.locked = locked;
                }
                Action::Jump { slot } => self.jump_to_slot(slot),
                Action::RelativeStarted { device, cause } => {
                    info!(%device, ?cause, "relative mode began");
                    self.send_relative(&device, true);
                }
                Action::RelativeEnded { device, reason } => {
                    info!(%device, ?reason, "relative mode ended");
                    self.last_relative_end = Some(RelativeEnded {
                        device: device.to_string(),
                        reason,
                    });
                    self.relative_ends += 1;
                    self.send_relative(&device, false);
                }
                Action::Wake { device } => {
                    // One wake-up in 30 seconds at most, whatever asked for
                    // the last: a push soon after Wake was pressed sends
                    // none, nor starts the two minutes over.
                    if self.wakes.sent_lately(device.as_str(), Instant::now()) {
                        debug!(%device, "not waking a device the cursor pushes toward: a wake-up went to it lately");
                    } else if let Ok(data) = self.can_wake(device.as_str()) {
                        info!(%device, "waking a device the cursor pushes toward");
                        self.wake(device.to_string(), data, None);
                    }
                }
                Action::RelativeUnsupported { device } => {
                    info!(%device, "the relative mode hotkey went there as a key, as it cannot take relative movements");
                    self.relative_unsupported
                        .send_replace(Some(device.to_string()));
                }
                Action::RelativeMotion { device, delta } => {
                    if let Some(link) = self.links.get_mut(&device) {
                        link.relative_seq += 1;
                        link.relative_total = link.relative_total + delta;
                        let seq = link.relative_seq;
                        let motion = peer::RelativeMotion {
                            seq,
                            total_dx: link.relative_total.x,
                            total_dy: link.relative_total.y,
                            session: link.relative_session,
                        };
                        #[cfg(feature = "test-hooks")]
                        let lost = self.lose_relative_motion > 0
                            && seq.is_multiple_of(self.lose_relative_motion);
                        #[cfg(not(feature = "test-hooks"))]
                        let lost = false;
                        if !lost {
                            self.send(&device, datagram_msg(datagram::Msg::RelativeMotion(motion)));
                        }
                    }
                }
                Action::LocateCursor { device, position } => {
                    if device == self.local {
                        self.cursor_located(position);
                    } else {
                        let locate = Locate {
                            x: position.x,
                            y: position.y,
                        };
                        let msg = Control {
                            msg: Some(control::Msg::Locate(locate)),
                        };
                        self.send(&device, Outbound::Control(msg));
                    }
                }
            }
        }
    }

    /// The device controlling this one says the user shook the mouse with
    /// the cursor here. From any other device it is stale or out of turn.
    fn on_locate(&mut self, from: DeviceId, locate: Locate) {
        if self.receiver.controller() != Some(&from) {
            debug!(device = %from, "ignoring where the cursor is from a device not in control");
            return;
        }
        if !locate.x.is_finite() || !locate.y.is_finite() {
            warn!(device = %from, "ignoring a malformed cursor position");
            return;
        }
        self.cursor_located(Point::new(locate.x, locate.y));
    }

    /// Tells this device's apps that the cursor crossed an edge of one of
    /// its displays, `spot` (this device's), to or from `other`.
    fn crossed_here(&mut self, spot: &EdgeSpot, arriving: bool, other: &DeviceId) {
        let Some(display) = self.local_displays.iter().find(|d| d.id == spot.display) else {
            debug!(display = %spot.display, "the cursor crossed an edge of a display not here");
            return;
        };
        if !spot.along.is_finite() {
            return;
        }
        let other_device_name = self
            .directory
            .peer(other.as_str())
            .map(|p| p.name)
            .unwrap_or_default();
        // Every crossing, as each lights an edge; none listening is fine.
        let _ = self.cursor_crossed.send(CursorCrossed {
            display: display.id.clone(),
            edge: spot.edge,
            along: spot.along,
            bounds: display.bounds,
            arriving,
            other_device_id: other.to_string(),
            other_device_name,
        });
    }

    /// As `crossed_here`, for where a controller said the cursor crossed.
    fn crossed_here_by(&mut self, spot: peer::EdgeSpot, arriving: bool) {
        let Some(edge) = convert::edge_from_peer(spot.edge) else {
            return;
        };
        let here = EdgeSpot {
            device: self.local.clone(),
            display: spot.display_id,
            edge,
            along: spot.along,
        };
        self.crossed_here(&here, arriving, &DeviceId::new(spot.other_device_id));
    }

    /// Where the cursor crossed an edge of `spot`'s device, coming from or
    /// going to `other`, as that device is told.
    fn edge_spot_for(&self, spot: &EdgeSpot, other: &DeviceId) -> Option<peer::EdgeSpot> {
        #[cfg(feature = "test-hooks")]
        if self.omit_crossing_spots {
            return None;
        }
        Some(peer::EdgeSpot {
            display_id: spot.display.clone(),
            edge: convert::edge_to_peer(spot.edge).into(),
            along: spot.along,
            other_device_id: other.to_string(),
        })
    }

    /// Tells IPC clients to show the cursor at `position`, unless it was
    /// shown less than [`LOCATE_INTERVAL`] ago. Without a client, as on
    /// Windows for now, it is only logged.
    fn cursor_located(&mut self, position: Point) {
        let now = Instant::now();
        if !locate_due(self.last_located, now) {
            return;
        }
        self.last_located = Some(now);
        debug!(
            x = position.x,
            y = position.y,
            "showing where the cursor is"
        );
        self.located.send_replace(Some(position));
    }

    fn send(&self, device: &DeviceId, msg: Outbound) {
        if let Some(link) = self.links.get(device) {
            let _ = link.tx.send(msg);
        }
    }

    /// Whether this device is taken offline for now.
    fn offline_here(&self) -> bool {
        self.offline.contains(self.local.as_str())
    }

    /// Whether `device` is taken offline for now.
    fn offline_for_now(&self, device: &DeviceId) -> bool {
        self.offline.contains(device.as_str())
    }

    /// Whether nothing goes between this device and `device`: either is
    /// taken offline for now (`OfflineDoc::between`, the one test of it,
    /// which the directory, and so file transfers, use too).
    fn offline_between(&self, device: &DeviceId) -> bool {
        self.offline.between(self.local.as_str(), device.as_str())
    }

    /// Whether this device controls others and is controlled: sharing is
    /// on, and it is not taken offline for now.
    fn shares(&self) -> bool {
        self.sharing && !self.offline_here()
    }

    /// The device that took another offline, and what this device calls
    /// it: itself by its name; a trusted device by the name given it here;
    /// another by the name it gave itself. The name is empty when none is
    /// known (never the id, which means nothing to people: the apps then
    /// say another computer did it), and both are when the entry does not
    /// say who.
    fn taken_by(&self, entry: &OfflineEntry) -> (String, String) {
        let Some(by) = entry.by.as_deref() else {
            return (String::new(), entry.by_name.clone());
        };
        let name = if by == self.local.as_str() {
            self.this.name()
        } else {
            name_of(&self.peers, by).unwrap_or_else(|| entry.by_name.clone())
        };
        (by.to_owned(), name)
    }

    /// The devices the router may wake by a push: those that told how,
    /// but none taken offline for now or that the profile in use does not
    /// connect, and none at all while this one is offline for now or the
    /// profile is paused.
    fn wakeable_now(&self) -> Vec<DeviceId> {
        let reach = self.reach.borrow();
        if self.offline_here() || reach.paused {
            return Vec::new();
        }
        wakeable(&self.peers)
            .filter(|d| !self.offline_for_now(d) && reach.connects(d.as_str()))
            .collect()
    }

    /// The user took `device` offline for now, or brought it back: a change
    /// of its entry made here, which the other devices hear of. Taken
    /// offline, it is this device or a trusted one; brought back, also any
    /// device the group lists offline, trusted here or not. What says once
    /// the offline file has it, when it is to be written; nothing when
    /// nothing changed.
    fn set_offline_for_now(
        &mut self,
        device: &str,
        offline: bool,
    ) -> Result<Option<Saved>, OfflineError> {
        let here = device == self.local.as_str();
        let known = here
            || name_of(&self.peers, device).is_some()
            || (!offline && self.offline.contains(device));
        if !known {
            return Err(OfflineError::UnknownDevice(device.to_owned()));
        }
        if !here && self.offline_here() {
            return Err(OfflineError::OfflineHere);
        }
        let by = TakenBy {
            device: self.local.to_string(),
            name: self.this.name(),
        };
        let unsaved = match self.directory.take_offline(device, offline, by) {
            Err(Full::TooMany) => return Err(OfflineError::TooMany),
            Err(Full::ListFull) => return Err(OfflineError::ListFull),
            Err(Full::Exhausted) => return Err(OfflineError::Exhausted),
            Ok(OfflineTaken::Unchanged) => return Ok(None),
            Ok(OfflineTaken::Changed(unsaved)) => unsaved,
        };
        info!(
            device,
            offline, "a device was taken offline for now, or brought back, here"
        );
        self.note_offline_change(device, offline, (self.local.to_string(), self.this.name()));
        self.offline_changed(None);
        Ok(unsaved.map(|unsaved| self.save_devices(unsaved)))
    }

    /// Counts a device taken offline or brought back, by the device `by`
    /// (id and name), for the apps to tell.
    fn note_offline_change(&mut self, device: &str, offline: bool, by: (String, String)) {
        self.offline_changes += 1;
        self.last_offline_change = Some(OfflineChange {
            device: device.to_owned(),
            offline,
            by: by.0,
            by_name: by.1,
        });
    }

    /// Another device's document of the devices taken offline for now:
    /// merged entry by entry; passed on when it changed anything here, and
    /// this device's sent back when that one lacked something.
    fn on_offline_devices(&mut self, from: DeviceId, doc: peer::OfflineDevices) {
        let Some(doc) = OfflineDoc::from_proto(doc) else {
            warn!(device = %from, "ignoring a list of devices taken offline that is too long");
            return;
        };
        let (merged, unsaved) = self.directory.merge_offline(&doc);
        if let Some(unsaved) = unsaved {
            // Written whether or not anyone waits for it.
            self.save_devices(unsaved);
        }
        if merged.changed {
            info!(device = %from, "the devices taken offline for now changed");
            self.offline_changed(Some(&from));
        }
        // Not to the first it sends in a session: this device's went to it
        // as the session started, and every change here since went to it as
        // well (`offline_changed` passes each on to every device but the
        // one whose document made it). So once this merge is done, every
        // entry here is one that device sent, or was sent: what the merge
        // took is that device's own, and what `fit` forgot needs telling to
        // no one. Answering would only repeat what it has.
        let first = self
            .links
            .get_mut(&from)
            .is_some_and(|link| !std::mem::replace(&mut link.offline_heard, true));
        if merged.behind && !first {
            self.send(&from, Outbound::Control(offline_message(&self.offline)));
        }
    }

    /// The directory's document of the devices taken offline for now
    /// changed, here or as `from` sent it: the engine takes it anew from
    /// there (the only way the engine's copy changes, so it is always the
    /// directory's), passes it on to the other devices, and lets go of and
    /// keeps off each device taken offline (every device, when this one
    /// is), as if its session ended, while those brought back may be
    /// reached again.
    fn offline_changed(&mut self, from: Option<&DeviceId>) {
        let before = std::mem::replace(&mut self.offline, self.directory.offline());
        // Passed on before anything else, so a device this one controls,
        // or that controls it, hears why it is let go of first.
        let msg = offline_message(&self.offline);
        for (device, link) in &self.links {
            if Some(device) != from && link.version >= OFFLINE_SINCE {
                let _ = link.tx.send(Outbound::Control(msg.clone()));
            }
        }
        self.files.offline_changed(&self.offline);
        let local = self.local.clone();
        let was_here = before.contains(local.as_str());
        let here = self.offline_here();
        if here && !was_here {
            info!("this device is offline for now");
            // Another device took it offline: the apps say so, once.
            if let Some(entry) = self
                .offline
                .taken_by(local.as_str())
                .filter(|e| e.by.as_deref() != Some(local.as_str()))
            {
                // Named as it is now, so the apps tell it by that name
                // whatever comes after.
                let by = self.taken_by(entry);
                self.note_offline_change(local.as_str(), true, by);
            }
        } else if was_here && !here {
            info!("this device is back online");
        }
        // The other devices taken offline or brought back.
        let changed: Vec<(DeviceId, bool)> = before
            .devices
            .keys()
            .chain(self.offline.devices.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|id| **id != local.as_str() && before.contains(id) != self.offline.contains(id))
            .map(|id| (DeviceId::new(id), self.offline.contains(id)))
            .collect();
        // Controlled by a device cut off from this one now: control ends
        // at once, and whatever it held here is let go of.
        let mut let_go = None;
        if self
            .receiver
            .controller()
            .is_some_and(|controller| self.offline_between(controller))
            && let Some((controller, releases)) = self.receiver.local_activity()
        {
            info!(%controller, "taken offline for now while controlled; control ends");
            self.inject_all(releases);
            let_go = Some(controller);
        }
        // A device that knows nothing of it is told sharing is paused while
        // either end is offline, which keeps it off: those whose state with
        // this one changed. The controller let go of hears it before that
        // its user took over, as `turn_away` says.
        self.tell_session(|device, link| {
            link.version < OFFLINE_SINCE
                && before.between(local.as_str(), device.as_str())
                    != self.offline.between(local.as_str(), device.as_str())
        });
        if let Some(controller) = let_go {
            self.took_over(&controller);
        }
        if here != was_here {
            self.route(Event::SetOfflineForNow(here));
        }
        for (device, offline) in &changed {
            if self.links.contains_key(device) {
                self.route(Event::PeerOfflineForNow(device.clone(), *offline));
            }
        }
        // Waking a device cut off from this one is given up, so it is not
        // shown waking, or not woken, once it is brought back.
        let given_up: Vec<String> = if here && !was_here {
            self.peers.iter().map(|p| p.id.clone()).collect()
        } else {
            changed
                .iter()
                .filter(|(_, offline)| *offline)
                .map(|(id, _)| id.to_string())
                .collect()
        };
        for id in given_up {
            self.wakes.forget(&id);
        }
        // What going away ends, as for a session that ended.
        let cut_off: Vec<DeviceId> = if here && !was_here {
            self.links.keys().cloned().collect()
        } else {
            changed
                .into_iter()
                .filter(|(id, offline)| *offline && self.links.contains_key(id))
                .map(|(id, _)| id)
                .collect()
        };
        for device in cut_off {
            self.end_with(&device);
        }
        let wakeable = self.wakeable_now();
        if let Some(router) = self.router.as_mut() {
            router.set_wakeable(wakeable);
        }
        self.refresh_exits();
        self.publish_arrangement();
    }

    /// The profile in use: none while the agent, as it starts, waits to
    /// know the network (`hold_until`), when none is in effect and nothing
    /// of any shows; else Other networks should it be gone, which cannot
    /// be.
    fn profile_in_use(&self) -> Option<&Profile> {
        if self.hold_until.is_some() {
            return None;
        }
        let profile = self
            .profiles
            .profile(&self.active)
            .or_else(|| self.profiles.profiles.last())
            .expect("the profiles always hold Other networks");
        Some(profile)
    }

    /// The network as taken now, with the Wi-Fi name an app read on it
    /// (`WifiReport`) while networks are recognized by it too (the
    /// platform's own, on Linux, comes with the network), and the name
    /// given it here where it is listed.
    fn network(&self) -> Option<Network> {
        let mut network = self.tracker.current()?.clone();
        if network.wifi_name.is_none()
            && let Some(report) = &self.wifi_report
            && self.report_in_effect(report)
        {
            network.wifi_name = report.name.clone();
        }
        network.name = self
            .profiles
            .network(&network.id)
            .and_then(|n| n.name.clone());
        Some(network)
    }

    /// Whether `report` says what the network now is called: names are
    /// used, it is confirmed, it is of the network now, and a name only
    /// while that is a Wi-Fi link. An app reads the Wi-Fi name whatever the
    /// route, so one read while a VPN or a wired link carries it (a VPN
    /// taking all traffic over the home Wi-Fi) is kept, so the app is told
    /// it came (`Status::wifi_name_held`), but never says anything of that
    /// network.
    fn report_in_effect(&self, report: &WifiReport) -> bool {
        let Some(current) = self.tracker.current() else {
            return false;
        };
        self.profiles.wifi_names
            && report.confirmed()
            && report.network == current.id
            && (report.name.is_none() || current.kind == crate::profiles_file::NetworkKind::Wifi)
    }

    /// A reading of the network, about once a second: the profile follows
    /// when the network changed (`profiles::Tracker`), and the Wi-Fi name an
    /// app read is confirmed, held over or dropped (`WifiReport`).
    fn network_read(&mut self) {
        let changed = self.tracker.observe(self.reading.as_ref(), Instant::now());
        let current = self.tracker.current().map(|n| n.id.clone());
        let waiting = self.tracker.waiting();
        let mut pick = changed;
        if changed {
            // Never the names, which say where the user is.
            match self.tracker.current() {
                Some(network) => {
                    info!(network = %network.id, kind = ?network.kind, "the network changed")
                }
                None => info!("no network now"),
            }
        }
        let gone = |network: &str| current.as_deref() != Some(network);
        // Read on another network: nothing of this one.
        if self.wifi_report.as_ref().is_some_and(|r| gone(&r.network)) {
            self.wifi_report = None;
        }
        if waiting {
            // Not confirmed by a reading that waits, which may be of the
            // next network already.
            if self.wifi_report.as_ref().is_some_and(|r| !r.confirmed()) {
                self.wifi_report = None;
            }
        } else {
            // One that came while the network was waited on, still of the
            // network now: counted from here.
            if let Some((network, name)) = self.wifi_held.take()
                && !gone(&network)
            {
                pick |= self.take_wifi_name(network, name);
            }
            if let Some(report) = self.wifi_report.as_mut()
                && !report.confirmed()
            {
                report.readings += 1;
                pick |= report.confirmed();
            }
        }
        // The first reading that settles ends the hold of the start: the
        // profile it picks is put in effect, which is no switch.
        if !waiting && self.hold_until.take().is_some() {
            info!(
                network = current.is_some(),
                "the network is known; reaching other devices as the profile in use says"
            );
            self.reselect(Pick::Start);
        } else if pick {
            self.reselect(Pick::Automatic);
        }
    }

    /// Ends the hold of the start without a reading that settled: the
    /// network cannot be read, or not soon enough.
    fn end_hold(&mut self) {
        if self.hold_until.take().is_some() {
            warn!(
                "the network is not known yet; reaching other devices as the profile in use says now"
            );
            self.reselect(Pick::Start);
        }
    }

    /// An app read the Wi-Fi name of the network `network` stands for:
    /// taken while names are used and it is the network now; one that comes
    /// while a reading of the network waits (it may be of the next network
    /// already) is held until the wait ends. Used once confirmed
    /// (`WifiReport`).
    fn on_wifi_name(&mut self, network: String, name: Option<String>) {
        if !self.profiles.wifi_names {
            debug!("passing over a Wi-Fi name: names are not used");
            return;
        }
        if self.tracker.waiting() {
            debug!("holding a Wi-Fi name until the network is known");
            self.wifi_held = Some((network, name));
            return;
        }
        if self.tracker.current().map(|n| n.id.as_str()) != Some(network.as_str()) {
            debug!("passing over a Wi-Fi name read on another network");
            return;
        }
        if self.take_wifi_name(network, name) {
            self.reselect(Pick::Automatic);
        }
    }

    /// Takes a Wi-Fi name an app read on the network now, to be confirmed;
    /// whether the name in use until now goes (the profile is picked anew).
    fn take_wifi_name(&mut self, network: String, name: Option<String>) -> bool {
        if self
            .wifi_report
            .as_ref()
            .is_some_and(|r| r.network == network && r.name == name)
        {
            return false;
        }
        // "Not on Wi-Fi" while this device's own reading says it is: the app
        // lost the name for a moment (a brief disconnect). The name in use
        // stays; only another network, or names turned off, clears it.
        let on_wifi = self
            .tracker
            .current()
            .is_some_and(|n| n.kind == crate::profiles_file::NetworkKind::Wifi);
        if name.is_none()
            && on_wifi
            && self
                .wifi_report
                .as_ref()
                .is_some_and(|r| r.network == network && r.name.is_some() && r.confirmed())
        {
            debug!("keeping the Wi-Fi name in use: the app read none on Wi-Fi");
            return false;
        }
        debug!(known = name.is_some(), "an app read the Wi-Fi name");
        let in_use = self.wifi_report.as_ref().is_some_and(WifiReport::confirmed);
        self.wifi_report = Some(WifiReport {
            network,
            name,
            readings: 0,
        });
        in_use
    }

    /// Picks the profile to use, for `pick`'s reason, and puts it in
    /// effect. Until the profiles were kept, the first network seen goes
    /// into Default, so on a first start, and as InputWeave is upgraded
    /// from before profiles, nothing changes until the user changes
    /// something. A listed network takes the names it lacked.
    ///
    /// A change by itself is counted, for the apps to tell, only from one
    /// network to another (or as the profiles change on one): not as a
    /// network comes, as after starting or waking from sleep, nor as the
    /// last one goes, when nothing is reached anyway.
    fn reselect(&mut self, pick: Pick) {
        let network = self.network();
        let known_before = std::mem::replace(&mut self.network_known, network.is_some());
        let mut changed = false;
        // Kept as the platform read it with the route (NetworkManager's
        // Wi-Fi name, Windows' name for the network), never with a name an
        // app read.
        if let Some(read) = self.tracker.current().cloned() {
            // Only by its id with the router resolved, which it keeps: one
            // made of the router's address alone would match no more once
            // it resolves. Until then, Default waits for one.
            if self.profiles_kept == Kept::No
                && self.tracker.resolved()
                && self.profiles.listing(&read.id).is_none()
                && self
                    .profiles
                    .add_network(crate::profiles_file::DEFAULT, &read)
                    .unwrap_or(false)
            {
                info!(network = %read.id, "the first network seen is Default's");
                changed = true;
            }
            changed |= self.profiles.learn(&read);
            // A Wi-Fi name an app read and the agent confirmed fills in a
            // listed network that has none (one listed before names were
            // used), never over one.
            if let Some(report) = &self.wifi_report
                && self.report_in_effect(report)
                && let Some(name) = &report.name
            {
                changed |= self.profiles.fill_wifi_name(&read.id, name);
            }
        }
        let active = profile_choice::select(&self.profiles, network.as_ref()).to_owned();
        // What this device picks by itself by a Wi-Fi name is noted, to be
        // picked there again before the name is known (as the agent starts,
        // and at the login window, where no app tells it).
        if profile_choice::picked_by_name(&self.profiles, network.as_ref())
            && let Some(network) = &network
        {
            changed |= self.profiles.picked(&network.id, &active);
        }
        if changed {
            self.save_profiles(false);
        }
        if active != self.active {
            info!(
                profile = %active,
                chosen = self.profiles.chosen.is_some(),
                "the profile in use changed"
            );
            if pick == Pick::Automatic && known_before && network.is_some() {
                self.profile_switches += 1;
            }
            self.active = active;
        }
        self.apply_profile();
        self.show_profiles();
    }

    /// Makes anew what the status shows of the network and the profiles,
    /// where it changed: after anything that may change them.
    fn show_profiles(&mut self) {
        self.network_shown = self.network();
        if *self.profiles_shown != self.profiles.profiles {
            self.profiles_shown = Arc::new(self.profiles.profiles.clone());
        }
    }

    /// Puts the profile in use in effect: files follow its switch, and the
    /// devices it does not connect (every one, paused) are let go of as if
    /// their sessions ended, which they do, while those it connects again
    /// are dialed and taken again (`Reach`, which the dialer, the accept
    /// loop and discovery follow). The clipboard follows its switch where
    /// it is checked (`clipboard_off`), and the local clipboard is told
    /// (`sync_local_clipboard`).
    fn apply_profile(&mut self) {
        self.sync_local_clipboard();
        let profile = self.profile_in_use();
        let reach = profile.map_or_else(Reach::held, Reach::of);
        let files = profile.is_none_or(|p| p.files);
        self.files.profile_changed(&reach, files);
        if *self.reach.borrow() == reach {
            return;
        }
        if reach.paused != self.reach.borrow().paused {
            info!(
                paused = reach.paused,
                "the profile in use pauses sharing, or no longer"
            );
        }
        let gone: Vec<DeviceId> = self
            .links
            .keys()
            .filter(|device| !reach.allows(device.as_str()))
            .cloned()
            .collect();
        // Control comes back from them first, for that reason, and they
        // let go of everything (`Leave`), before their sessions end.
        for device in &gone {
            self.route(Event::LeaveForProfile(device.clone()));
        }
        let away: Vec<String> = self
            .peers
            .iter()
            .filter(|p| !reach.connects(&p.id))
            .map(|p| p.id.clone())
            .collect();
        self.reach.send_replace(reach);
        for device in gone {
            info!(%device, "the profile in use does not connect it; ending its session");
            self.on_peer_down(device, Gone::Offline);
        }
        // Not woken meanwhile, nor shown being woken once back.
        for id in away {
            self.wakes.forget(&id);
        }
        let wakeable = self.wakeable_now();
        if let Some(router) = self.router.as_mut() {
            router.set_wakeable(wakeable);
        }
        self.refresh_exits();
        self.publish_arrangement();
    }

    /// Tells the local clipboard whether sync is on, which takes the user's
    /// switch and the profile in use's both: one that reads ahead (the
    /// Windows broker) reads nothing while it is off, and once it is on
    /// again reads what was copied meanwhile, to be offered as the cursor
    /// next leaves. Told only as it changes.
    fn sync_local_clipboard(&mut self) {
        // None in effect while the agent starts: as the user's switch says.
        let on = self.clipboard.on && self.profile_in_use().is_none_or(|p| p.clipboard);
        if on == self.clipboard.told {
            return;
        }
        self.clipboard.told = on;
        if let Some(local) = &self.clipboard.local {
            local.ask(LocalOp::SetSync(on));
        }
    }

    /// Changes the profiles as the user asked; what says once the profiles
    /// file has it, when it is to be written.
    fn change_profiles(&mut self, change: ProfileChange) -> Result<Option<Saved>, ProfileError> {
        let mut file = self.profiles.clone();
        // Choosing, and removing the profile chosen (back to Automatic),
        // are the user's doing: no switch for the apps to tell.
        let pick = match &change {
            ProfileChange::Choose(_) => Pick::Chosen,
            ProfileChange::Remove(id) if file.chosen.as_ref() == Some(id) => Pick::Chosen,
            _ => Pick::Automatic,
        };
        let changed = match change {
            ProfileChange::Add(name) => {
                let id = file.add(&name)?;
                info!(profile = %id, "a profile was added");
                true
            }
            ProfileChange::Edit(id, edit) => {
                file.edit(&id, &edit, |d| self.peers.iter().any(|p| p.id == d))?
            }
            ProfileChange::Remove(id) => {
                file.remove(&id)?;
                info!(profile = %id, "a profile was removed");
                true
            }
            ProfileChange::Choose(id) => file.choose(id.as_deref())?,
            ProfileChange::AddNetwork(id) => {
                let network = self.network().ok_or(ProfileError::NoNetwork)?;
                // Not by the router's address alone, which would match no
                // more once it resolves.
                if !self.tracker.resolved() {
                    return Err(ProfileError::NetworkUnresolved);
                }
                file.add_network(&id, &network)?
            }
            ProfileChange::RemoveNetwork(id) => {
                file.remove_network(&id)?;
                true
            }
            ProfileChange::RenameNetwork(id, name) => file.rename_network(&id, &name)?,
            ProfileChange::WifiNames(on) => {
                let changed = file.wifi_names != on;
                file.wifi_names = on;
                if !on {
                    // Not kept once names are not used, nor what was picked
                    // by them.
                    self.wifi_report = None;
                    self.wifi_held = None;
                    file.picks.clear();
                }
                changed
            }
        };
        if !changed {
            return Ok(None);
        }
        self.profiles = file;
        let saved = self.save_profiles(true);
        self.reselect(pick);
        Ok(saved)
    }

    /// Writes the profiles, off this loop, if there is a file; from now on
    /// they are kept. A profiles file that could not be read is written
    /// over only for a change the user made (`by_user`): until then the
    /// changes the agent makes by itself (a name learned, a device
    /// forgotten) last until it stops.
    fn save_profiles(&mut self, by_user: bool) -> Option<Saved> {
        if self.profiles_kept == Kept::Unreadable && !by_user {
            debug!("not writing over the profiles file that could not be read");
            return None;
        }
        self.profiles_kept = Kept::Yes;
        let unsaved = self.directory.profiles_unsaved(&self.profiles)?;
        Some(self.save_devices(unsaved))
    }

    /// Ends what this device has going on with `device` once it is gone, or
    /// cut off from this one though its session goes on (taken offline for
    /// now): pulls through it fail, files pasted from it are given up, and
    /// drags with it end.
    fn end_with(&mut self, device: &DeviceId) {
        for waiter in self.clipboard.pulls.lost(device) {
            self.pull_failed(waiter, ClipboardSkip::Unavailable);
        }
        // Files coming from it are given up.
        self.clipboard.file_pulls.retain(|_, p| p.origin != *device);
        self.forget_arriving_drags();
        let gone = self.drags.peer_down(device);
        self.carry_out(gone);
    }

    fn inject_all(&mut self, injections: Vec<Injection>) {
        for injection in injections {
            self.inject(injection);
        }
    }

    fn inject(&mut self, injection: Injection) {
        let result = match injection {
            Injection::MoveTo(p) => self.emulation.move_to(p),
            Injection::MoveBy(d) => self.emulation.move_by(d.x, d.y),
            Injection::Button { button, pressed } => self.emulation.button(button, pressed),
            Injection::Key { usage, pressed } => self.emulation.key(usage, pressed),
            Injection::Scroll { dx, dy } => self.emulation.scroll(dx, dy),
        };
        match result {
            Ok(()) => self.injection_failures.succeeded(),
            Err(e) => self
                .injection_failures
                .failed(&e, &injection, Instant::now()),
        }
    }

    /// Builds the layout from the arrangement and the displays known so
    /// far, then tells IPC clients. Devices whose displays are unknown are
    /// left out; devices with displays but no place are put next to this
    /// one in a new version of the arrangement, which is shared. A layout
    /// that fails validation is logged and the previous one kept.
    fn rebuild_layout(&mut self) {
        self.build_layout();
        self.publish_arrangement();
    }

    fn build_layout(&mut self) {
        if self.local_displays.is_empty() {
            warn!("this device reports no displays; the cursor cannot leave it");
            return;
        }
        let placements = self.directory.placements();
        let placed = |id: &str| {
            placements
                .iter()
                .find(|p| p.device == id)
                .map(|p| Point::new(p.x, p.y))
        };
        let mut devices = vec![Device {
            id: self.local.clone(),
            name: self.this.name(),
            displays: self.local_displays.clone(),
            offset: placed(self.local.as_str()).unwrap_or_default(),
        }];
        let mut moves = Vec::new();
        let mut unplaced = Vec::new();
        for peer in self.peers.iter() {
            let id = DeviceId::new(&peer.id);
            let Some(displays) = self.peer_displays.get(&id).filter(|d| !d.is_empty()) else {
                continue;
            };
            match placed(&peer.id) {
                Some(offset) => devices.push(Device {
                    id,
                    name: peer.name.clone(),
                    displays: displays.clone(),
                    offset,
                }),
                None => unplaced.push((peer.clone(), displays.clone())),
            }
        }
        let mut layout = match Layout::new(devices) {
            Ok(layout) => layout,
            Err(e) => {
                warn!(error = %e, "invalid layout; keeping the previous one");
                return;
            }
        };
        for (peer, displays) in unplaced {
            let Some(offset) = layout.place_beside(&self.local, &displays, peer.side.into()) else {
                continue;
            };
            let mut devices = layout.devices().to_vec();
            devices.push(Device {
                id: DeviceId::new(&peer.id),
                name: peer.name.clone(),
                displays,
                offset,
            });
            match Layout::new(devices) {
                Ok(next) => {
                    info!(device = %peer.id, x = offset.x, y = offset.y, "placed a device next to this one");
                    moves.push((peer.id.clone(), offset));
                    layout = next;
                }
                Err(e) => warn!(error = %e, device = %peer.id, "cannot place device"),
            }
        }
        if !moves.is_empty() {
            // The shared arrangement must say where this device is too, or
            // the others could not tell where the newcomer sits relative to
            // it.
            if placed(self.local.as_str()).is_none() {
                moves.push((self.local.to_string(), Point::default()));
            }
            let doc = self.directory.rearrange(&moves);
            self.broadcast_layout(&doc, None);
        }
        self.arranged = layout.devices().iter().map(|d| d.id.clone()).collect();
        self.jump_order = layout.jump_order();
        let actions = match self.router.as_mut() {
            Some(router) => router.set_layout(layout).unwrap_or_default(),
            None => {
                let Ok(mut router) = Router::new(layout, self.local.clone(), self.router_config)
                else {
                    return;
                };
                router.set_remaps(peer_remaps(&self.peers));
                router.set_speeds(peer_speeds(&self.peers));
                router.set_wakeable(self.wakeable_now());
                router.set_wake_on_push(self.wake_on_push);
                // A router without relative mode yet: nothing to end.
                let _ = router.set_relative_auto(self.relative_auto);
                let now = Instant::now();
                for (device, link) in &self.links {
                    router.handle(Event::PeerConnected(device.clone()), now);
                    router.set_relative_capable(device.clone(), link.version >= RELATIVE_SINCE);
                    if link.out_of_reach() {
                        router.handle(Event::PeerScreenLocked(device.clone(), true), now);
                    }
                    if link.input_unavailable {
                        router.handle(Event::PeerInputUnavailable(device.clone(), true), now);
                    }
                    if link.sharing_paused {
                        router.handle(Event::PeerSharingPaused(device.clone(), true), now);
                    }
                    if link.pointer_captured {
                        router.handle(Event::PointerCaptured(device.clone(), true), now);
                    }
                    if self.offline.contains(device.as_str()) {
                        router.handle(Event::PeerOfflineForNow(device.clone(), true), now);
                    }
                }
                router.handle(Event::SetSharing(self.sharing), now);
                router.handle(Event::SetOfflineForNow(self.offline_here()), now);
                router.handle(Event::SetLocked(self.locked), now);
                self.router = Some(router);
                Vec::new()
            }
        };
        self.apply(actions);
        self.refresh_exits();
    }

    /// Tells IPC clients where every device sits, if that changed.
    fn publish_arrangement(&self) {
        let placements = self.directory.placements();
        let placed = |id: &str| {
            placements
                .iter()
                .find(|p| p.device == id)
                .map(|p| Point::new(p.x, p.y))
        };
        let mut devices = vec![ArrangedDevice {
            id: self.local.to_string(),
            name: self.this.name(),
            os: convert::this_os(),
            // Until the arrangement says otherwise this device is at the
            // origin.
            offset: Some(placed(self.local.as_str()).unwrap_or_default()),
            displays: self.local_displays.clone(),
            pictures: self.local_pictures.clone(),
            connected: true,
            this_device: true,
            jump_slot: self.jump_slot(&self.local),
        }];
        for peer in self.peers.iter() {
            let id = DeviceId::new(&peer.id);
            devices.push(ArrangedDevice {
                id: peer.id.clone(),
                name: peer.name.clone(),
                os: peer.os,
                offset: placed(&peer.id),
                displays: self.peer_displays.get(&id).cloned().unwrap_or_default(),
                pictures: self.peer_pictures.get(&id).cloned().unwrap_or_default(),
                // Shown offline while it or this device is offline for now.
                connected: self.links.contains_key(&id) && !self.offline_between(&id),
                this_device: false,
                jump_slot: self.jump_slot(&id),
            });
        }
        let arrangement = Arrangement {
            version: self.directory.layout().version,
            devices,
        };
        self.arrangement.send_if_modified(|current| {
            if *current == arrangement {
                false
            } else {
                *current = arrangement;
                true
            }
        });
    }

    /// Hands control back and releases everything before the agent stops.
    fn wind_down(&mut self) {
        // Nothing held, here or there, before anything else.
        self.route(Event::SetSharing(false));
        if let Some(controller) = self.receiver.controller().cloned() {
            let releases = self
                .receiver
                .handle(RemoteEvent::ControllerLost(controller));
            self.inject_all(releases);
        }
        let _ = self.capture.set_grabbed(false);
        // Then: the devices connected until now were last seen as they
        // were last heard from, as far as this device can tell once it is
        // back. The agent stops, so it is written here, once for them all,
        // last.
        if let Some(unsaved) = self.stamp_connected() {
            let _ = self.directory.save(unsaved);
        }
    }

    fn publish(&self) {
        let cursor_on = match self.router.as_ref().map(|r| r.state()) {
            Some(iw_core::router::State::Remote { device, .. }) => device.to_string(),
            _ => self.local.to_string(),
        };
        let reach = self.reach.borrow().clone();
        let status = Status {
            sharing: self.sharing,
            this_device: self.local.to_string(),
            this_name: self.this.name(),
            capturing: self.capturing,
            permissions: self.permissions.clone(),
            secure_input: self.secure_input,
            screen_locked: self.screen_locked,
            lock_screen_input: self.lock_screen_input,
            input_available: self.input_available,
            cursor_on,
            controlled_by: self.receiver.controller().map(|d| d.to_string()),
            layout_ready: self.router.is_some(),
            devices: self
                .peers
                .iter()
                .map(|p| {
                    let id = DeviceId::new(&p.id);
                    let session = self.links.get(&id);
                    // While it or this device is offline for now, it is
                    // shown offline, and not as one that can be woken.
                    let cut_off = self.offline_between(&id);
                    let link = session.filter(|_| !cut_off);
                    // While this device is offline for now, or the profile
                    // in use is paused, every other one is just offline
                    // here, taken offline or not.
                    let taken_by = self
                        .offline
                        .taken_by(&p.id)
                        .filter(|_| !self.offline_here() && !reach.paused);
                    // Nor while the profile in use leaves it out; it says
                    // so after being offline for now, which comes first,
                    // and not while the profile is paused, when every
                    // device is just offline.
                    let left_out = !reach.connects(&p.id);
                    let away = cut_off || left_out;
                    DeviceState {
                        connected: link.is_some(),
                        can_wake: session.is_none() && !away && p.wake.is_some(),
                        wake: if away {
                            WakeState::None
                        } else {
                            self.wakes.state(&p.id)
                        },
                        offline_since: if session.is_none() && !away && p.wake.is_some() {
                            self.last_seen.get(&p.id).copied()
                        } else {
                            None
                        },
                        not_in_profile: left_out
                            && !reach.paused
                            && taken_by.is_none()
                            && !self.offline_here(),
                        offline_for_now: taken_by.is_some(),
                        offline_by: taken_by.map(|entry| self.taken_by(entry)),
                        offline_unaware: session.is_some_and(|l| l.version < OFFLINE_SINCE),
                        arranged: self.arranged.contains(&id),
                        rtt_ms: link.map_or(0, |l| l.rtt_ms),
                        screen_locked: link.is_some_and(|l| l.screen_locked),
                        lock_screen_input: link.is_some_and(|l| !l.lock_screen_blocked),
                        pointer_captured: link.is_some_and(|l| l.pointer_captured),
                        input_unavailable: link.is_some_and(|l| l.input_unavailable),
                        sharing_paused: link.is_some_and(|l| l.sharing_paused),
                        jump_slot: self.jump_slot(&id),
                        ..DeviceState::of(p)
                    }
                })
                .collect(),
            last_return: self.last_return.clone(),
            returns: self.returns,
            clipboard_sync: self.clipboard.on,
            last_clipboard: self.clipboard.last.clone(),
            lazy_clipboard: self.clipboard.lazy_stats.clone(),
            locked: self.locked,
            edge_dwell_ms: self.router_config.edge.dwell_ms,
            preview_sharing: self.preview_sharing,
            desktop_pictures: self.desktop_pictures,
            dragging: self.drags.dragging(),
            hotkeys: self.router_config.hotkeys,
            switch_modifier: self.router_config.edge.switch_modifier,
            login_window_access: self.login_window_access,
            drags_carried: self
                .receiver
                .controller()
                .is_some_and(|c| self.speaks_remote_drag(c) && !self.files_off(c)),
            relative: self
                .router
                .as_ref()
                .and_then(|r| r.relative())
                .map(|(device, cause)| RelativeMode {
                    device: device.to_string(),
                    automatic: cause == RelativeStart::Captured,
                }),
            relative_auto: self.relative_auto,
            pointer_captured: self.pointer_captured,
            relative_ends: self.relative_ends,
            last_relative_end: self.last_relative_end.clone(),
            folder_access: self.folder_access.clone(),
            session: self.session_info.clone(),
            edges_only: self.capture.edges_only(),
            firewall: self.firewall,
            license: self.license_status.clone(),
            wake_on_push: self.wake_on_push,
            offline_by: self
                .offline
                .taken_by(self.local.as_str())
                .map(|entry| self.taken_by(entry)),
            offline_changes: self.offline_changes,
            last_offline_change: self.last_offline_change.clone(),
            network: self.network_shown.clone(),
            network_resolved: self.tracker.resolved(),
            profiles: self.profiles_shown.clone(),
            // None yet while the agent starts (`hold_until`).
            profile: if self.hold_until.is_some() {
                String::new()
            } else {
                self.active.clone()
            },
            profile_chosen: self.profiles.chosen.is_some(),
            profile_switches: self.profile_switches,
            wifi_names: self.profiles.wifi_names,
            wifi_name_held: self.wifi_report.as_ref().map(|report| WifiNameHeld {
                network: report.network.clone(),
                name: report.name.clone(),
                in_use: self.report_in_effect(report),
            }),
        };
        self.status.send_if_modified(|current| {
            if *current == status {
                false
            } else {
                *current = status;
                true
            }
        });
        self.peers_applied.send_if_modified(|applied| {
            let changed = *applied != self.peers_generation;
            *applied = self.peers_generation;
            changed
        });
    }
}

/// Whole milliseconds, rounded up, so a measured time is never 0, which
/// stands for unknown.
fn rtt_ms(rtt: Duration) -> u32 {
    u32::try_from(rtt.as_micros().div_ceil(1000))
        .unwrap_or(u32::MAX)
        .max(1)
}

/// Whether the cursor can be shown at `now`, having last been shown at
/// `last`.
fn locate_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|at| now.saturating_duration_since(at) >= LOCATE_INTERVAL)
}

fn remap(peer: &Peer) -> ModifierRemap {
    ModifierRemap {
        swap_gui_and_control: peer.swap_gui_and_control,
    }
}

/// Each trusted device's key remapping, for the router.
fn peer_remaps(peers: &[Peer]) -> impl Iterator<Item = (DeviceId, ModifierRemap)> + '_ {
    peers.iter().map(|p| (DeviceId::new(&p.id), remap(p)))
}

/// Each trusted device's cursor speed, for the router.
fn peer_speeds(peers: &[Peer]) -> impl Iterator<Item = (DeviceId, CursorSpeed)> + '_ {
    peers.iter().map(|p| (DeviceId::new(&p.id), p.cursor_speed))
}

/// The trusted devices that told how to wake them.
fn wakeable(peers: &[Peer]) -> impl Iterator<Item = DeviceId> + '_ {
    peers
        .iter()
        .filter(|p| p.wake.is_some())
        .map(|p| DeviceId::new(&p.id))
}

/// Whether `old` and `new` differ in which devices can be woken.
fn wakeable_differs(old: &[Peer], new: &[Peer]) -> bool {
    wakeable(old).collect::<HashSet<_>>() != wakeable(new).collect::<HashSet<_>>()
}

/// Resolves at `deadline`; never without one.
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

/// Whether `new` is `old` with only cursor speeds, or how to wake devices,
/// changed.
fn only_local_choices_differ(old: &[Peer], new: &[Peer]) -> bool {
    old.len() == new.len()
        && old
            .iter()
            .zip(new)
            .all(|(o, n)| same_but_local_choices(o, n))
}

/// Whether `a` and `b` are the same but for their cursor speed and how to
/// wake them. Every field is named, so one added later is not left out of
/// the comparison.
fn same_but_local_choices(a: &Peer, b: &Peer) -> bool {
    let Peer {
        id,
        name,
        reported_name,
        fingerprint,
        os,
        origin,
        address,
        last_address,
        swap_gui_and_control,
        side,
        clipboard_off,
        files_off,
        cursor_speed: _,
        wake: _,
    } = a;
    *id == b.id
        && *name == b.name
        && *reported_name == b.reported_name
        && *fingerprint == b.fingerprint
        && *os == b.os
        && *origin == b.origin
        && *address == b.address
        && *last_address == b.last_address
        && *swap_gui_and_control == b.swap_gui_and_control
        && *side == b.side
        && *clipboard_off == b.clipboard_off
        && *files_off == b.files_off
}

fn offline_message(doc: &OfflineDoc) -> Control {
    Control {
        msg: Some(control::Msg::OfflineDevices(doc.to_proto())),
    }
}

fn layout_message(doc: &LayoutDoc) -> Control {
    Control {
        msg: Some(control::Msg::Layout(peer::Layout {
            version: doc.version,
            author: doc.author.clone(),
            placements: doc
                .placements
                .iter()
                .map(|p| Placement {
                    device_id: p.device.clone(),
                    x: p.x,
                    y: p.y,
                })
                .collect(),
        })),
    }
}

fn input_msg(msg: input::Msg) -> Outbound {
    Outbound::Input(Input { msg: Some(msg) })
}

fn datagram_msg(msg: datagram::Msg) -> Outbound {
    Outbound::Datagram(Datagram { msg: Some(msg) })
}

/// Every hotkey the router acts on while input is grabbed
/// (`InputCapture::watch_hotkeys`).
fn acted_on(hotkeys: &Hotkeys) -> Vec<Hotkey> {
    let mut all = hotkeys.reserved();
    all.push(hotkeys.take_back);
    all.extend(hotkeys.relative);
    all
}

/// How often failing injection is reported: the first failure, then at
/// most this often while it goes on, as it fails for every event when the
/// platform cannot inject at all.
const INJECTION_REPORT: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
struct InjectionFailures {
    /// When a failure was last reported, while failures go on.
    reported: Option<Instant>,
    /// Failures since then.
    since: u64,
}

impl InjectionFailures {
    fn failed(&mut self, error: &PlatformError, injection: &Injection, now: Instant) {
        if self
            .reported
            .is_some_and(|at| now.duration_since(at) < INJECTION_REPORT)
        {
            self.since += 1;
            debug!(error = %error, ?injection, "injection failed");
            return;
        }
        warn!(error = %error, ?injection, failed_since = self.since, "injection failed");
        self.reported = Some(now);
        self.since = 0;
    }

    fn succeeded(&mut self) {
        if self.reported.take().is_some() && self.since > 0 {
            info!(failed = self.since, "injection works again");
        }
        self.since = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_to_speeds_or_how_to_wake_alone_is_told_apart() {
        use crate::agent::directory::Origin;
        use crate::store::Side;
        let peer = |id: &str, percent: u32| Peer {
            id: id.to_owned(),
            name: id.to_uppercase(),
            reported_name: id.to_uppercase(),
            fingerprint: iw_net::Fingerprint::of_certificate(id.as_bytes()),
            os: iw_proto::peer::v1::Os::Linux,
            origin: Origin::Paired,
            address: None,
            last_address: None,
            swap_gui_and_control: false,
            side: Side::Right,
            clipboard_off: false,
            files_off: false,
            cursor_speed: CursorSpeed::from_percent(percent).unwrap(),
            wake: None,
        };
        let old = [peer("mini", 100), peer("win", 100)];
        assert!(only_local_choices_differ(
            &old,
            &[peer("mini", 150), peer("win", 100)]
        ));
        assert!(only_local_choices_differ(&old, &old));
        let mut told = peer("mini", 100);
        told.wake = WakeData::new([2, 0, 0, 0, 0, 1], None);
        assert!(only_local_choices_differ(
            &old,
            &[told.clone(), peer("win", 100)]
        ));
        // Which devices can be woken, apart: not for a speed, nor for
        // another way to wake one that could be woken already.
        assert!(!wakeable_differs(
            &old,
            &[peer("mini", 150), peer("win", 100)]
        ));
        assert!(wakeable_differs(&old, &[told.clone(), peer("win", 100)]));
        let mut told_anew = told.clone();
        told_anew.wake = WakeData::new([2, 0, 0, 0, 0, 2], None);
        assert!(!wakeable_differs(
            &[told.clone(), peer("win", 100)],
            &[peer("win", 100), told_anew]
        ));
        assert!(wakeable_differs(&[told], &old));
        // Anything else is a change of its own.
        let mut renamed = peer("mini", 150);
        renamed.name = "Studio".to_owned();
        assert!(!only_local_choices_differ(
            &old,
            &[renamed, peer("win", 100)]
        ));
        assert!(!only_local_choices_differ(&old, &[peer("mini", 100)]));
        assert!(!only_local_choices_differ(
            &old,
            &[peer("win", 100), peer("mini", 100)]
        ));
    }

    #[test]
    fn the_cursor_is_shown_at_most_once_a_second() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        assert!(locate_due(None, start));
        assert!(!locate_due(Some(start), at(0)));
        assert!(!locate_due(Some(start), at(999)));
        assert!(locate_due(Some(start), at(1000)));
    }
}
