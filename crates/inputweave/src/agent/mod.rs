//! The agent runtime: wires platform backends, the core state machines,
//! peer networking, pairing, discovery and the IPC server together.
//!
//! Tasks:
//! - the engine, sole owner of the router, receiver and backends;
//! - a bridge thread forwarding captured input into the engine;
//! - a poller reporting permissions, secure input, display changes and the
//!   desktop pictures on the displays;
//! - the accept loop, and one dial loop per trusted device this one dials;
//! - the pairing coordinator;
//! - discovery: the mDNS announcement and browsing, while the profile in
//!   use shares, and the bridge feeding what it finds into the directory;
//! - the IPC server (a Unix socket, or a named pipe on Windows);
//! - on Windows, the server for the broker, which reads the desktop
//!   pictures and the clipboard, writes received files, reads the files
//!   sent, keeps the transfer history, and starts the app, as the user,
//!   and a follower that keeps the history listed with the brokers as they
//!   come and go (`history`);
//! - a watcher that opens the app when a pairing code is to be shown and
//!   no app is connected to show it (`app`).

mod app;
#[cfg(any(unix, windows))]
mod app_clipboard;
#[cfg(any(unix, windows))]
mod broker;
mod clipboard_files;
mod diagnostics;
mod digest;
mod directory;
mod drag;
mod engine;
mod files;
mod folder_access;
mod history;
#[cfg(any(unix, windows))]
mod ipc;
mod lazy_clipboard;
mod license;
mod listing;
mod local_clipboard;
mod pairing;
mod peers;
mod previews;
mod private;
mod profiles;
mod queue;
mod status;
mod this_device;
mod throttle;
mod transfer;
mod wake;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

use iw_core::geometry::Point;
use iw_core::hotkey::{HotkeyError, Hotkeys, Modifier};
use iw_core::layout::Display;
use iw_net::discovery::{Announcement, Discovery, DiscoveryEvent, Found};
use iw_net::{Fingerprint, Identity, NetError, PairingCode, PeerEndpoint};
use iw_platform::{
    Clipboard, DesktopPictures, Displays, Firewall, InputCapture, InputEmulation, Permission,
    PlatformError, SessionStatus,
};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};
use tracing::{info, warn};

pub use crate::profiles_file::ProfileError;
pub(crate) use diagnostics::utc;
pub use digest::executable_digest;
pub use directory::DeviceError;
pub use drag::{ArrivedDrag, DragEnd, DragEvent, DraggedItem, Dragging, EndedDrag};
pub use engine::{
    DragError, HotkeysError, JumpError, OfflineError, RearrangeError, RelativeError, WakeError,
};
pub use files::{
    BatchInfo, Direction, FailedEntry, FileSettings, FolderProgress, RemoveError, SendError,
    TransferState, TransferStatus,
};
pub use folder_access::{Access, FolderAccess, UserFolder, UserFolders};
pub use license::{
    LicenseError, LicenseFile, LicenseResult, LicenseState, LicenseStatus, LicensedDevice,
    Licenses, Refusal, masked as masked_license_key, valid_key as valid_license_key,
};
pub use listing::{MAX_BYTES, MAX_FILES, MAX_ITEMS};
pub use pairing::{Failure, NotWaiting, PairingPeer, PairingStatus};
pub use previews::{Pictures, Preview};
pub use status::{
    ArrangedDevice, Arrangement, CursorCrossed, DeviceState, OfflineChange, RelativeEnded,
    RelativeMode, Status, WifiNameHeld, hotkeys_to_proto,
};
pub use wake::{WakeData, WakeState};

use crate::config::{Config, ConfigError};
use crate::convert;
use crate::offline_file::OfflineDoc;
use crate::profiles_file::ProfilesFile;
use crate::store::DevicesFile;
use directory::Directory;
pub use engine::ProfileChange;
use engine::{Engine, EngineEvent, EngineParts, HotkeysChange, PlatformState};
use files::Files;
use pairing::{Pairing, PairingContext};
use peers::{AbortOnDrop, PeerContext};
use previews::PreviewFiles;
use profiles::Reach;
use this_device::ThisDevice;

pub struct Backends {
    pub capture: Box<dyn InputCapture>,
    pub emulation: Box<dyn InputEmulation>,
    pub displays: Box<dyn Displays>,
    pub pictures: Box<dyn DesktopPictures>,
    pub status: Box<dyn SessionStatus>,
    /// Absent where another process owns the clipboard.
    pub clipboard: Option<Box<dyn Clipboard>>,
}

/// How often permissions, secure input and displays are checked.
const PLATFORM_POLL: Duration = Duration::from_secs(1);
/// How often the network is read (`watch_network`).
const NETWORK_POLL: Duration = Duration::from_secs(1);

/// How long a request waits for the engine to apply a change.
const APPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// How often the desktop pictures are checked, besides whenever the
/// displays change. The platform does not tell when the user picks
/// another picture.
const PICTURE_POLL: Duration = Duration::from_secs(60);

/// Drag events an IPC client may fall behind on before it misses some.
const DRAG_EVENTS: usize = 64;
/// Crossings of the edges an IPC client may fall behind on before it
/// misses some.
const CURSOR_CROSSINGS: usize = 64;

/// Where the agent keeps state outside its configuration.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Serve IPC clients on this socket, or named pipe on Windows.
    pub ipc_socket: Option<PathBuf>,
    /// Remember paired devices and placements in this file, and other
    /// devices' desktop pictures in `previews/` beside it. Without one,
    /// they last until the agent stops.
    pub devices_file: Option<PathBuf>,
    /// The devices to start from when there is no `devices_file`, kept in
    /// memory only: the login window's agent reads them from the user's
    /// folder, which it must never write (`login_window`).
    pub devices: Option<DevicesFile>,
    /// The devices taken offline for now to start from when there is no
    /// `devices_file`, kept in memory only, as `devices` is.
    pub offline: Option<OfflineDoc>,
    /// The profiles to start from when there is no `devices_file`, kept in
    /// memory only, as `devices` is.
    pub profiles: Option<ProfilesFile>,
    /// Tell other devices this agent cannot read its desktop pictures, so
    /// they keep the ones they have of it: the login window's agent runs as
    /// a user's device without that user's desktop.
    pub previews_unchanged: bool,
    /// Whether this agent's user may let other devices control this Mac at
    /// the login window (`SetLoginWindowAccess`), or why not.
    pub login_window: crate::login_window::Support,
    /// Remember the file transfers that finished in this file
    /// (`paths::transfers_file`). Without one, they last until the agent
    /// stops. With a broker (`broker_socket`) the broker remembers them
    /// instead, in the user's own folder, and this file, which an earlier
    /// agent may have kept, is removed.
    pub transfers_file: Option<PathBuf>,
    /// Where received files go unless the user chose elsewhere. Without
    /// one, this device declines files.
    pub receive_folder: Option<PathBuf>,
    /// The folders macOS protects that files are sent from and saved in
    /// (`paths::user_folders`), which `RequestFolderAccess` asks for.
    /// Without them, as everywhere but macOS, it is refused. With them, the
    /// agent touches none of them by itself: it removes what interrupted
    /// transfers left in the receive folder when files are next written
    /// there, rather than as it starts.
    pub user_folders: Option<UserFolders>,
    /// Open the app when the user has to see or decide something and no
    /// app is connected to show it: through the broker, when there is one,
    /// otherwise as this process (on macOS and Linux).
    pub open_app: bool,
    /// Decline every drag of files offered here, and offer none carried
    /// back here from a device controlled to this device's app, as if no
    /// app were here to take it: Linux takes no part in dragging files
    /// between devices, by design, so
    /// it is set by the system, not asked of the app. Were a Linux app ever
    /// to take drags, it would say so in its `ClientHello`, as Windows apps
    /// do of the clipboard's files (`takes_clipboard_files`), and this
    /// would give way to that.
    pub refuses_drags: bool,
    /// Logs whose ends go into diagnostics reports.
    pub log_files: Vec<PathBuf>,
    /// The agent's systemd user unit, whose journal goes into diagnostics
    /// reports (Linux, where the agent logs to the journal).
    pub journal_unit: Option<String>,
    /// Serve the Windows broker on this named pipe (a socket elsewhere,
    /// in tests). This device's desktop pictures then come from the
    /// broker, in place of the platform backend's; so does its clipboard,
    /// unless the platform backend has one; the broker writes received
    /// files, into the folder it names unless the user chose another, and
    /// reads the files sent; it keeps its user's transfer history; and it
    /// starts the app.
    pub broker_socket: Option<PathBuf>,
    /// The SHA-256 of the agent's executable in lowercase hex
    /// ([`executable_digest`]), reported to clients and in diagnostics.
    /// Empty when unknown.
    pub agent_digest: String,
    /// Speak at most this peer protocol version, as an older build would;
    /// for tests. `None` speaks every version this build knows.
    pub peer_protocol_max: Option<u32>,
    /// Where files copied on another device and pasted here are fetched
    /// into, a folder for each paste (`paths::clipboard_staging`). Without
    /// one, files are not taken from other devices' clipboards.
    pub clipboard_staging: Option<PathBuf>,
    /// How long such a folder stays once something else is copied, as the
    /// app that pasted may still be copying from it.
    pub clipboard_staging_grace: Duration,
    /// How long an app pasting files from another device waits for them
    /// before it gets nothing that time; for tests. `None` waits as long as
    /// macOS lets it (`iw_platform::PROMISE_WAIT`).
    pub paste_wait: Option<Duration>,
    /// How long a promise of files waits for the app to start and to put
    /// it on the clipboard, where the broker owns the clipboard (Windows);
    /// for tests. `None` is five seconds (`app_clipboard::APP_WAIT`).
    pub app_clipboard_wait: Option<Duration>,
    /// How long files pasted here may come without anything of them
    /// moving, once they started to (five times that before, while the
    /// origin lists them or they wait their turn there); for tests. `None`
    /// is two minutes (`clipboard_files::STALL`).
    pub paste_stall: Option<Duration>,
    /// Where this computer's license is kept (`paths::license_file`), if
    /// the agent keeps one; off, none is shown, kept or passed to paired
    /// devices, as for the login window's agent.
    pub licenses: Licenses,
    /// How often a license file that is there but cannot be read is read
    /// again; for tests. `None` is two minutes.
    pub license_reread: Option<Duration>,
    /// How long a device being woken has to connect before it counts as not
    /// woken; for tests. `None` is two minutes.
    pub wake_wait: Option<Duration>,
    /// Leave out every this many relative movements sent, as if the
    /// network lost them. 0 sends them all. Only with the `test-hooks`
    /// feature, which the tests turn on.
    #[cfg(feature = "test-hooks")]
    pub lose_relative_motion: u64,
    /// Leave out where the cursor crossed an edge from what is sent to
    /// other devices (`Enter.crossed`, `Leave.crossed`), as an older agent
    /// does. Only with the `test-hooks` feature.
    #[cfg(feature = "test-hooks")]
    pub omit_crossing_spots: bool,
    /// Tell other devices to wake this one so, in place of what its
    /// network interfaces say, which over the loopback is nothing. Only
    /// with the `test-hooks` feature.
    #[cfg(feature = "test-hooks")]
    pub wake_info: Option<WakeData>,
    /// Send every wake-up here, in place of each broadcast address it goes
    /// to. Only with the `test-hooks` feature.
    #[cfg(feature = "test-hooks")]
    pub wake_target: Option<SocketAddr>,
    /// Announce this device, where the configuration has discovery on, as
    /// if mDNS ran, without touching the network: whether it would is
    /// `AgentHandle::announcing`. Only with the `test-hooks` feature.
    #[cfg(feature = "test-hooks")]
    pub fake_discovery: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Net(#[from] NetError),
    #[error(transparent)]
    Platform(#[from] PlatformError),
    #[error("cannot open the IPC socket: {0}")]
    Ipc(std::io::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A running agent. Dropping it without calling [`AgentHandle::shutdown`]
/// leaves its tasks running until the runtime stops.
pub struct AgentHandle {
    device_id: String,
    local_addr: SocketAddr,
    login_window: crate::login_window::Support,
    fingerprint: Fingerprint,
    endpoint: Arc<PeerEndpoint>,
    directory: Arc<Directory>,
    pairing: Pairing,
    this: Arc<ThisDevice>,
    files: Arc<Files>,
    diagnostics: Arc<diagnostics::Fixed>,
    engine_tx: mpsc::UnboundedSender<EngineEvent>,
    engine_task: JoinHandle<()>,
    status: watch::Receiver<Status>,
    arrangement: watch::Receiver<Arrangement>,
    located: watch::Receiver<Option<Point>>,
    identified: watch::Receiver<Option<u32>>,
    relative_unsupported: watch::Receiver<Option<String>>,
    cursor_crossed: broadcast::Sender<CursorCrossed>,
    drag_events: broadcast::Sender<DragEvent>,
    tasks: JoinSet<()>,
}

/// Starts an agent. An unset device id or name in `config` is filled in
/// from `identity` and the computer's name.
pub async fn start(
    mut config: Config,
    identity: Arc<Identity>,
    backends: Backends,
    options: Options,
) -> Result<AgentHandle, AgentError> {
    let fingerprint = identity.fingerprint();
    let wake_source = wake_source(&options);
    config.resolve_device(&fingerprint, iw_platform::computer_name());
    config.validate()?;
    let options_receive_folder = options.receive_folder.clone();
    let stored = match (&options.devices_file, &options.devices) {
        (Some(path), _) => DevicesFile::load(path)?,
        (None, Some(devices)) => devices.clone(),
        (None, None) => DevicesFile::default(),
    };
    // A name set from the UI wins over the configuration file.
    if iw_net::is_valid_device_name(&stored.device_name) {
        config.device.name = stored.device_name.clone();
    }
    let config = Arc::new(config);
    let devices_file = options.devices_file.clone();
    let preview_files = PreviewFiles::new(
        options
            .devices_file
            .as_deref()
            .map(crate::paths::previews_dir),
    );
    // How other devices told this one to wake them, in a file of its own
    // beside the devices file; one that cannot be read never keeps the agent
    // from starting.
    let wake_path = options.devices_file.as_deref().map(crate::paths::wake_file);
    let wake = wake_path
        .as_deref()
        .map(crate::wake_file::WakeFile::load)
        .unwrap_or_default();
    // The devices of the group taken offline for now, in a file of their
    // own beside the devices file, read as leniently.
    let offline_path = options
        .devices_file
        .as_deref()
        .map(crate::paths::offline_file);
    let offline = match (&offline_path, &options.offline) {
        (Some(path), _) => OfflineDoc::load(path),
        (None, Some(doc)) => doc.clone(),
        (None, None) => OfflineDoc::default(),
    };
    // The profiles, in a file of their own beside the devices file, read
    // as leniently. Until there is one (a first start, or an upgrade from
    // before profiles), the first network seen goes into Default. One there
    // that cannot be read is left be until the user changes the profiles.
    let profiles_path = options
        .devices_file
        .as_deref()
        .map(crate::paths::profiles_file);
    let (profiles, profiles_kept) = match (&profiles_path, &options.profiles) {
        (Some(path), _) => match ProfilesFile::load(path) {
            crate::profiles_file::Loaded::File(file) => (file, engine::Kept::Yes),
            crate::profiles_file::Loaded::Missing => (ProfilesFile::default(), engine::Kept::No),
            crate::profiles_file::Loaded::Unreadable => {
                (ProfilesFile::default(), engine::Kept::Unreadable)
            }
        },
        (None, Some(file)) => (file.clone(), engine::Kept::Yes),
        (None, None) => (ProfilesFile::default(), engine::Kept::No),
    };
    let directory = Arc::new(
        Directory::with_wake(&config, options.devices_file, stored, wake_path, wake)
            .with_offline(offline_path, offline)
            .with_profiles(profiles_path),
    );
    // A damaged license file never keeps the agent from starting. One that
    // cannot be read makes the license unavailable, and is never saved
    // over, until the engine can read it.
    let (license, license_saver, license_unavailable) = match &options.licenses {
        Licenses::Off => (None, None, None),
        Licenses::Memory => (Some(LicenseFile::default()), None, None),
        Licenses::File(path) => {
            let (file, unavailable) = match LicenseFile::load_or_set_aside(path) {
                license::Loaded::File(file) => (Some(file), None),
                license::Loaded::SetAside => (Some(LicenseFile::default()), None),
                license::Loaded::Unreadable => (None, Some(path.clone())),
            };
            (
                file,
                Some(license::LicenseSaver::new(path.clone())),
                unavailable,
            )
        }
    };
    let local_displays = backends.displays.displays()?;
    if local_displays.is_empty() {
        warn!("no local displays reported");
    }
    let endpoint = Arc::new(PeerEndpoint::bind(config.network.listen, identity)?);
    let local_addr = endpoint.local_addr()?;
    let diagnostics = Arc::new(diagnostics::Fixed {
        fingerprint: fingerprint.to_string(),
        listen: local_addr.to_string(),
        config: format!("{config:#?}"),
        devices_file,
        log_files: options.log_files.clone(),
        journal_unit: options.journal_unit.clone(),
        agent_digest: options.agent_digest.clone(),
    });
    let this = Arc::new(ThisDevice::new(
        config.device.id.clone(),
        config.device.name.clone(),
        convert::this_os(),
        options.peer_protocol_max,
    ));

    let (engine_tx, engine_rx) = mpsc::unbounded_channel();
    // Pulls of offered clipboards the engine waits on, which transfer
    // streams check before reading up to the lazy limit.
    let pulls = lazy_clipboard::PendingPulls::default();
    let mut tasks = JoinSet::new();
    let brokered = options.broker_socket.is_some();
    let broker = serve_broker(
        options.broker_socket,
        config.clipboard.max_bytes,
        &mut tasks,
    );
    let pictures = pictures_backend(backends.pictures, &broker);
    let clipboard = clipboard_backend(backends.clipboard, &broker);
    let desktop_pictures = pictures.available();
    let user_folders = options.user_folders.unwrap_or_default();
    let folder_access = folder_access::initial(&user_folders, &directory.folder_access());
    let peers_now = directory.peers();
    // The engine starts from the license shown here.
    let initial = Status {
        license: license::shown(license.as_ref(), license_unavailable.is_some(), |id| {
            directory::name_of(&peers_now, id)
        }),
        ..Status::initial(
            &config,
            &peers_now,
            directory.preview_sharing(),
            desktop_pictures,
            directory.hotkeys(),
            directory.login_window_access(),
            folder_access.clone(),
        )
    };
    let (status_tx, status_rx) = watch::channel(initial);
    let (peers_applied_tx, peers_applied_rx) = watch::channel(0);
    let (arrangement_tx, arrangement_rx) = watch::channel(Arrangement::default());
    let (located_tx, located_rx) = watch::channel(None);
    let (identified_tx, identified_rx) = watch::channel(None);
    let (relative_unsupported_tx, relative_unsupported_rx) = watch::channel(None);
    let (cursor_crossed, _) = broadcast::channel(CURSOR_CROSSINGS);
    let (drag_events, _) = broadcast::channel(DRAG_EVENTS);

    let ui_clients = Arc::new(AtomicUsize::new(0));
    let opener = Arc::new(app::AppOpener::new(
        options.open_app.then(|| app_opener(&broker)),
        ui_clients.clone(),
    ));
    // Where the broker owns the clipboard, the app holds promises that have
    // files, which File Explorer pastes from a data object of its own.
    #[cfg(any(unix, windows))]
    let app_clipboard = app_clipboard(&broker, &ui_clients, &opener, options.app_clipboard_wait);
    let (chosen_folder, ask) = directory.file_settings();
    let files = Files::new(
        FileSettings {
            folder: chosen_folder.or(options.receive_folder),
            ask,
        },
        directory.clone(),
        opener.clone(),
        files_store(&broker),
        history_store(options.transfers_file, brokered, &broker),
        user_folders.is_empty(),
    );
    #[cfg(any(unix, windows))]
    if let Some(broker) = &broker {
        // Clients learn where files go as the broker comes and goes.
        let (mut folder, told) = (broker.watch_receive_folder(), files.clone());
        tasks.spawn(async move {
            while folder.changed().await.is_ok() {
                told.settings_changed();
            }
        });
        // The history is the connected broker's user's.
        tasks.spawn(history::follow_broker(broker.clone(), files.clone()));
    }
    let pairing = pairing::spawn(
        PairingContext::new(
            endpoint.clone(),
            directory.clone(),
            this.clone(),
            config.network.allow_pairing,
        ),
        &mut tasks,
    );
    tasks.spawn(app::show_pairing_codes(pairing.watch(), opener));

    let folders = (!user_folders.is_empty()).then(|| {
        folder_access::FolderAsker::new(
            user_folders,
            folder_access.clone(),
            directory.clone(),
            engine_tx.clone(),
        )
    });

    // Polled by the platform watcher, and asked for permissions through IPC.
    let session: Arc<dyn SessionStatus> = Arc::from(backends.status);
    // What the profile in use lets this device reach; the engine says.
    // Nothing until the engine knows the network, and so the profile.
    let (reach_tx, reach_rx) = watch::channel(Reach::held());

    #[cfg(any(unix, windows))]
    if let Some(path) = options.ipc_socket {
        let listener = iw_ipc::IpcListener::bind(&path).map_err(AgentError::Ipc)?;
        tasks.spawn(ipc::serve(
            listener,
            ipc::IpcContext {
                engine: engine_tx.clone(),
                status: status_rx.clone(),
                peers_applied: peers_applied_rx.clone(),
                arrangement: arrangement_rx.clone(),
                located: located_rx.clone(),
                identified: identified_rx.clone(),
                relative_unsupported: relative_unsupported_rx.clone(),
                cursor_crossed: cursor_crossed.clone(),
                drag_events: drag_events.clone(),
                pairing: pairing.clone(),
                directory: directory.clone(),
                this: this.clone(),
                files: files.clone(),
                default_receive_folder: options_receive_folder.clone(),
                folders: folders.clone(),
                session: session.clone(),
                login_window: options.login_window,
                ui_clients: ui_clients.clone(),
                app_clipboard: app_clipboard.clone(),
                diagnostics: diagnostics.clone(),
                local_addr,
            },
        ));
    }
    #[cfg(not(any(unix, windows)))]
    if options.ipc_socket.is_some() {
        warn!("IPC is not implemented on this platform yet");
    }

    let (capture_tx, capture_rx) = std::sync::mpsc::channel();
    let bridge = engine_tx.clone();
    std::thread::Builder::new()
        .name("inputweave-capture".to_owned())
        .spawn(move || {
            while let Ok(event) = capture_rx.recv() {
                if bridge.send(EngineEvent::Capture(event)).is_err() {
                    break;
                }
            }
        })?;

    let engine = Engine::new(EngineParts {
        config: config.clone(),
        this: this.clone(),
        directory: directory.clone(),
        capture: backends.capture,
        input_available: session.input_available(),
        capture_tx,
        emulation: backends.emulation,
        local_displays,
        preview_sharing: directory.preview_sharing(),
        previews_unchanged: options.previews_unchanged,
        login_window_access: directory.login_window_access(),
        desktop_pictures,
        preview_files,
        license,
        license_saver,
        license_unavailable,
        license_reread: options.license_reread.unwrap_or(engine::LICENSE_REREAD),
        status: status_tx,
        peers_applied: peers_applied_tx,
        arrangement: arrangement_tx,
        located: located_tx,
        identified: identified_tx,
        relative_unsupported: relative_unsupported_tx,
        cursor_crossed: cursor_crossed.clone(),
        clipboard,
        clipboard_on: directory.clipboard_sync(),
        clipboard_max: config.clipboard.max_bytes,
        edge_dwell_ms: directory.edge_dwell_ms().min(MAX_DWELL_MS),
        switch_modifier: directory.switch_modifier(),
        hotkeys: directory.hotkeys(),
        files: files.clone(),
        drag_events: drag_events.clone(),
        ui_clients: ui_clients.clone(),
        events: engine_tx.downgrade(),
        pulls: pulls.clone(),
        clipboard_staging: options
            .clipboard_staging
            .clone()
            .map(|dir| clipboard_files::Staging {
                dir,
                grace: options.clipboard_staging_grace,
            }),
        paste_wait: options.paste_wait.unwrap_or(iw_platform::PROMISE_WAIT),
        paste_patience: options
            .paste_stall
            .map_or_else(Default::default, clipboard_files::Patience::new),
        relative_auto: directory.relative_auto(),
        folder_access,
        refuses_drags: options.refuses_drags,
        wake_on_push: directory.wake_on_push(),
        wake_wait: options.wake_wait.unwrap_or(wake::WAKE_WAIT),
        profiles,
        profiles_kept,
        reach: reach_tx,
        #[cfg(feature = "test-hooks")]
        lose_relative_motion: options.lose_relative_motion,
        #[cfg(feature = "test-hooks")]
        omit_crossing_spots: options.omit_crossing_spots,
        #[cfg(feature = "test-hooks")]
        wake_target: options.wake_target,
    });
    let engine_task = tokio::spawn(engine.run(engine_rx));
    tasks.spawn(watch_network(session.clone(), engine_tx.clone()));
    // The desktop pictures are read by the poller, now that the endpoint
    // and IPC are up: decoding them can take seconds. Sessions that start
    // before then get Displays without pictures, and again with them.
    tasks.spawn(watch_platform(
        session,
        local_addr.port(),
        backends.displays,
        PictureWatch {
            backend: pictures,
            schedule: PictureSchedule::default(),
            current: previews::Pictures::new(),
            errors: PictureErrors::default(),
        },
        engine_tx.clone(),
    ));

    let ctx = Arc::new(PeerContext::new(
        endpoint.clone(),
        engine_tx.clone(),
        directory.clone(),
        pairing.clone(),
        this.clone(),
        peers::ClipboardLimits {
            max: config.clipboard.max_bytes,
            pulls,
        },
        files.clone(),
        wake_source,
        reach_rx.clone(),
    )?);
    tasks.spawn(peers::accept_loop(ctx.clone()));
    tasks.spawn(peers::dial_all(ctx));

    if config.network.discovery {
        this.announces(Announcement {
            device_id: config.device.id.clone(),
            name: config.device.name.clone(),
            os: convert::this_os(),
            fingerprint,
            addr: local_addr,
            pairable: config.network.allow_pairing,
        });
        #[cfg(feature = "test-hooks")]
        let fake = options.fake_discovery;
        #[cfg(not(feature = "test-hooks"))]
        let fake = false;
        tasks.spawn(advertise(this.clone(), directory.clone(), reach_rx, fake));
    }

    Ok(AgentHandle {
        device_id: config.device.id.clone(),
        local_addr,
        login_window: options.login_window,
        fingerprint,
        endpoint,
        directory,
        pairing,
        this,
        files,
        diagnostics,
        engine_tx,
        engine_task,
        status: status_rx,
        arrangement: arrangement_rx,
        located: located_rx,
        identified: identified_rx,
        relative_unsupported: relative_unsupported_rx,
        cursor_crossed,
        drag_events,
        tasks,
    })
}

#[cfg(any(unix, windows))]
type BrokerRef = Arc<broker::Broker>;
#[cfg(not(any(unix, windows)))]
type BrokerRef = std::convert::Infallible;

/// Serves the broker on `pipe`, if given: on Windows the broker reads the
/// desktop pictures and the clipboard, and writes received files, as the
/// user. If the pipe cannot be served there is no broker; the agent runs
/// on without them.
fn serve_broker(
    pipe: Option<PathBuf>,
    clipboard_max: usize,
    tasks: &mut JoinSet<()>,
) -> Option<BrokerRef> {
    let path = pipe?;
    #[cfg(any(unix, windows))]
    match iw_ipc::IpcListener::bind(&path) {
        Ok(listener) => {
            let broker = Arc::new(broker::Broker::new(clipboard_max));
            tasks.spawn(broker::serve(listener, broker.clone()));
            Some(broker)
        }
        Err(e) => {
            warn!(
                pipe = %path.display(),
                error = %e,
                "cannot serve the broker; this device has no desktop pictures or clipboard, and \
                 receives no files"
            );
            None
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, clipboard_max, tasks);
        warn!("the broker is not implemented on this platform");
        None
    }
}

/// Where this device's desktop pictures come from: the broker, when there
/// is one, otherwise the platform backend.
fn pictures_backend(
    backend: Box<dyn DesktopPictures>,
    broker: &Option<BrokerRef>,
) -> Arc<dyn DesktopPictures> {
    match broker {
        #[cfg(any(unix, windows))]
        Some(broker) => broker.clone(),
        #[cfg(not(any(unix, windows)))]
        Some(never) => match *never {},
        None => Arc::from(backend),
    }
}

/// This device's clipboard: the platform's, or where the platform leaves
/// it to the broker, the broker's.
fn clipboard_backend(
    backend: Option<Box<dyn Clipboard>>,
    broker: &Option<BrokerRef>,
) -> Option<Box<dyn Clipboard>> {
    match (backend, broker) {
        (Some(backend), _) => Some(backend),
        #[cfg(any(unix, windows))]
        (None, Some(broker)) => Some(Box::new(broker::BrokerClipboard(broker.clone()))),
        #[cfg(not(any(unix, windows)))]
        (None, Some(never)) => match *never {},
        (None, None) => None,
    }
}

/// The app's hold on promises that have files, where the broker owns the
/// clipboard; the app is started for them through `opener`.
#[cfg(any(unix, windows))]
fn app_clipboard(
    broker: &Option<BrokerRef>,
    ui_clients: &Arc<AtomicUsize>,
    opener: &Arc<app::AppOpener>,
    wait: Option<Duration>,
) -> Option<Arc<app_clipboard::AppClipboard>> {
    let broker = broker.as_ref()?;
    let opener = opener.clone();
    let hub = app_clipboard::AppClipboard::new(
        ui_clients.clone(),
        Box::new(move || opener.open_for(app::Need::ClipboardFiles)),
        wait.unwrap_or(app_clipboard::APP_WAIT),
    );
    broker.set_app_clipboard(hub.clone());
    Some(hub)
}

/// How the app is opened: by the broker, as the user, when there is one;
/// otherwise by this process, which runs as the user.
fn app_opener(broker: &Option<BrokerRef>) -> app::Open {
    match broker {
        #[cfg(any(unix, windows))]
        Some(broker) => {
            let broker = broker.clone();
            Box::new(move |need| broker.launch_app(need))
        }
        #[cfg(not(any(unix, windows)))]
        Some(never) => match *never {},
        None => Box::new(|_| iw_platform::system::open_app()),
    }
}

/// Where the finished transfers are kept across restarts. Where a broker
/// does the user's file work (`brokered`: on Windows, where the agent runs
/// as SYSTEM and the history holds the names of the user's files), the
/// broker keeps them too, in the user's own folder, and `file`, which an
/// earlier agent kept, is removed; if the broker's pipe could not be
/// served, they last until the agent stops. Elsewhere the agent keeps them
/// in `file`.
fn history_store(
    file: Option<PathBuf>,
    brokered: bool,
    broker: &Option<BrokerRef>,
) -> Option<history::HistoryStore> {
    if !brokered {
        return file.map(history::HistoryStore::File);
    }
    if let Some(file) = &file {
        history::remove_left_behind(file);
    }
    match broker {
        #[cfg(any(unix, windows))]
        Some(broker) => Some(history::HistoryStore::Broker(broker.clone())),
        #[cfg(not(any(unix, windows)))]
        Some(never) => match *never {},
        None => None,
    }
}

/// Who reads the files sent and writes the files received: the broker,
/// when there is one.
fn files_store(broker: &Option<BrokerRef>) -> files::Store {
    match broker {
        #[cfg(any(unix, windows))]
        Some(broker) => files::Store::Broker(broker.clone()),
        #[cfg(not(any(unix, windows)))]
        Some(never) => match *never {},
        None => files::Store::Local,
    }
}

/// Where sessions learn how to wake this device: its network interfaces,
/// or in tests what they were told to say.
fn wake_source(options: &Options) -> peers::WakeSource {
    #[cfg(feature = "test-hooks")]
    if let Some(data) = options.wake_info {
        return peers::WakeSource::Fixed(data);
    }
    let _ = options;
    peers::WakeSource::Interfaces
}

/// Sends a wake-up to an offline device; returns once it was sent, the
/// status showing the device being woken.
async fn wake_device(
    engine: &mpsc::UnboundedSender<EngineEvent>,
    device: &str,
) -> Result<(), WakeError> {
    let (reply, rx) = oneshot::channel();
    engine
        .send(EngineEvent::Wake {
            device: device.to_owned(),
            reply,
        })
        .map_err(|_| WakeError::Stopping)?;
    rx.await.map_err(|_| WakeError::Stopping)?
}

/// Has pushing the cursor toward an offline device wake it, or not, and
/// remembers the choice; returns once the status shows it and it is in the
/// wake file, or could not be written there (logged), waiting for the file
/// [`APPLY_TIMEOUT`] at most. False once the agent is stopping.
async fn set_wake_on_push(engine: &mpsc::UnboundedSender<EngineEvent>, on: bool) -> bool {
    let (reply, rx) = oneshot::channel();
    if engine
        .send(EngineEvent::SetWakeOnPush { on, reply })
        .is_err()
    {
        return false;
    }
    let Ok(saved) = rx.await else {
        return false;
    };
    if let Some(saved) = saved {
        let _ = tokio::time::timeout(APPLY_TIMEOUT, saved).await;
    }
    true
}

/// Takes `device` offline for now or brings it back; returns once the
/// status shows it and it is in the offline file, or could not be written
/// there (logged), waiting for the file [`APPLY_TIMEOUT`] at most.
async fn set_offline_for_now(
    engine: &mpsc::UnboundedSender<EngineEvent>,
    device: &str,
    offline: bool,
) -> Result<(), OfflineError> {
    let (reply, rx) = oneshot::channel();
    engine
        .send(EngineEvent::SetOfflineForNow {
            device: device.to_owned(),
            offline,
            reply,
        })
        .map_err(|_| OfflineError::Stopping)?;
    let saved = rx.await.map_err(|_| OfflineError::Stopping)??;
    if let Some(saved) = saved {
        let _ = tokio::time::timeout(APPLY_TIMEOUT, saved).await;
    }
    Ok(())
}

/// Changes the profiles, or which is used; returns once the status shows it
/// and it is in the profiles file, or could not be written there (logged),
/// waiting for the file [`APPLY_TIMEOUT`] at most.
async fn change_profiles(
    engine: &mpsc::UnboundedSender<EngineEvent>,
    change: ProfileChange,
) -> Result<(), ProfileError> {
    let (reply, rx) = oneshot::channel();
    engine
        .send(EngineEvent::Profiles { change, reply })
        .map_err(|_| ProfileError::Stopping)?;
    let saved = rx.await.map_err(|_| ProfileError::Stopping)??;
    if let Some(saved) = saved {
        let _ = tokio::time::timeout(APPLY_TIMEOUT, saved).await;
    }
    Ok(())
}

/// A diagnostics report of the agent as it is now.
async fn diagnostics_report(
    fixed: &Arc<diagnostics::Fixed>,
    status: &watch::Receiver<Status>,
    arrangement: &watch::Receiver<Arrangement>,
    directory: &Directory,
    files: &Files,
) -> String {
    let snapshot = diagnostics::Snapshot {
        status: status.borrow().clone(),
        arrangement: arrangement.borrow().clone(),
        peers: directory.peers().to_vec(),
        transfers: files.transfers(),
        pacing: files.pacing(),
        receive_folder: files.settings().folder,
        offline: directory.offline(),
    };
    let fixed = fixed.clone();
    tokio::task::spawn_blocking(move || {
        diagnostics::report(&fixed, &snapshot, std::time::SystemTime::now())
    })
    .await
    .unwrap_or_else(|e| format!("cannot build the report: {e}"))
}

#[derive(Debug, thiserror::Error)]
#[error("a device name is 1 to 64 characters without control characters")]
pub struct RenameError;

fn rename(
    this: &ThisDevice,
    directory: &Directory,
    engine: &mpsc::UnboundedSender<EngineEvent>,
    name: &str,
) -> Result<(), RenameError> {
    let name = name.trim();
    if !iw_net::is_valid_device_name(name) {
        return Err(RenameError);
    }
    this.rename(name);
    directory.set_device_name(name);
    let _ = engine.send(EngineEvent::Renamed);
    Ok(())
}

fn set_clipboard_sync(
    directory: &Directory,
    engine: &mpsc::UnboundedSender<EngineEvent>,
    on: bool,
) {
    directory.set_clipboard_sync(on);
    let _ = engine.send(EngineEvent::SetClipboardSync(on));
}

fn set_preview_sharing(
    directory: &Directory,
    engine: &mpsc::UnboundedSender<EngineEvent>,
    on: bool,
) {
    directory.set_preview_sharing(on);
    let _ = engine.send(EngineEvent::SetPreviewSharing(on));
}

/// Why other devices may not control this Mac at the login window.
#[derive(Debug, thiserror::Error)]
pub enum LoginWindowError {
    #[error("only a Mac running the real backends has an agent at the login window")]
    Unsupported,
    #[error("the login window's agent is not installed; it comes with the installer package")]
    NotInstalled,
    #[error(
        "this agent keeps its files outside the user's InputWeave folder, the only place the \
         login window's agent reads"
    )]
    CustomPaths,
    #[error(
        "other devices cannot type on this Mac's lock screen, and the login window takes their \
         input the same way"
    )]
    NoLockScreenInput,
}

impl LoginWindowError {
    /// The error code clients are told.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::NotInstalled => "not_installed",
            Self::CustomPaths => "custom_paths",
            Self::NoLockScreenInput => "no_lock_screen_input",
        }
    }
}

/// Lets other devices control this Mac at the login window, or stops them.
/// Turning it on needs the login window's agent installed, this agent's
/// files where that one reads them, and a lock screen that takes other
/// devices' input, as the login window's agent injects the same way.
/// Turning it off always works.
fn set_login_window_access(
    directory: &Directory,
    engine: &mpsc::UnboundedSender<EngineEvent>,
    support: crate::login_window::Support,
    status: &watch::Receiver<Status>,
    on: bool,
) -> Result<(), LoginWindowError> {
    use crate::login_window::Support;

    if on {
        match support {
            Support::Available => {}
            Support::Unsupported => return Err(LoginWindowError::Unsupported),
            Support::NotInstalled => return Err(LoginWindowError::NotInstalled),
            Support::CustomPaths => return Err(LoginWindowError::CustomPaths),
        }
        if !status.borrow().lock_screen_input {
            return Err(LoginWindowError::NoLockScreenInput);
        }
    }
    directory.set_login_window_access(on);
    let _ = engine.send(EngineEvent::SetLoginWindowAccess(on));
    Ok(())
}

/// The longest the cursor can be made to push against an edge before it
/// crosses, in milliseconds; longer would make crossing a chore.
pub const MAX_DWELL_MS: u32 = 1000;

/// The wait at an edge that turning the switch delay on in the UI asks for,
/// in milliseconds: long enough that brushing the edge does not cross.
pub const DEFAULT_DWELL_MS: u32 = 150;

#[derive(Debug, thiserror::Error)]
#[error("the time to push against an edge is at most {MAX_DWELL_MS} ms")]
pub struct DwellError;

fn set_edge_dwell(
    directory: &Directory,
    engine: &mpsc::UnboundedSender<EngineEvent>,
    ms: u32,
) -> Result<(), DwellError> {
    if ms > MAX_DWELL_MS {
        return Err(DwellError);
    }
    directory.set_edge_dwell_ms(ms);
    let _ = engine.send(EngineEvent::SetEdgeDwell(ms));
    Ok(())
}

/// Makes the cursor cross edges only while `modifier` is held, or
/// whenever it reaches one, and remembers the choice.
fn set_switch_modifier(
    directory: &Directory,
    engine: &mpsc::UnboundedSender<EngineEvent>,
    modifier: Option<Modifier>,
) {
    directory.set_switch_modifier(modifier);
    let _ = engine.send(EngineEvent::SetSwitchModifier(modifier));
}

/// Hotkeys as they come in, from clients and from the devices file,
/// checked: against each other (`Hotkeys::check`), and against this
/// computer's keyboard, refusing a hotkey on a key it never reports and so
/// could never press (`Hotkeys::check_keyboard`), as Scroll Lock and Pause
/// on a Mac.
pub(crate) fn checked_here(hotkeys: Hotkeys) -> Result<Hotkeys, HotkeyError> {
    hotkeys.check()?;
    hotkeys.check_keyboard(iw_platform::keyboard_reports)?;
    Ok(hotkeys)
}

/// Puts the hotkeys `change` makes of those in effect in effect, and
/// remembers them, unless they cannot be used; once this returns the
/// status shows them and they are in the devices file. Waits for that
/// [`APPLY_TIMEOUT`] at most; the engine puts them in effect and writes the
/// file whether or not this is waited for to the end.
async fn set_hotkeys(
    engine: &mpsc::UnboundedSender<EngineEvent>,
    change: HotkeysChange,
) -> Result<(), HotkeysError> {
    let (reply, rx) = oneshot::channel();
    engine
        .send(EngineEvent::SetHotkeys { change, reply })
        .map_err(|_| HotkeysError::Stopping)?;
    let applied = async {
        let Some(saved) = rx.await.map_err(|_| HotkeysError::Stopping)?? else {
            return Ok(());
        };
        match saved.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(HotkeysError::NotSaved(e.to_string())),
            Err(_) => Err(HotkeysError::Stopping),
        }
    };
    tokio::time::timeout(APPLY_TIMEOUT, applied)
        .await
        .unwrap_or(Err(HotkeysError::Timeout))
}

/// Turns relative mode on or off for the device the cursor is on.
async fn set_relative(
    engine: &mpsc::UnboundedSender<EngineEvent>,
    on: bool,
) -> Result<(), RelativeError> {
    let (reply, rx) = oneshot::channel();
    engine
        .send(EngineEvent::SetRelative { on, reply })
        .map_err(|_| RelativeError::Stopping)?;
    rx.await.map_err(|_| RelativeError::Stopping)?
}

/// Has devices enter relative mode by themselves when an app there holds
/// the pointer, or not, and remembers the choice.
fn set_relative_auto(directory: &Directory, engine: &mpsc::UnboundedSender<EngineEvent>, on: bool) {
    directory.set_relative_auto(on);
    let _ = engine.send(EngineEvent::SetRelativeAuto(on));
}

/// Changes this computer's license; the status shows the change once this
/// returns.
async fn license(
    engine: &mpsc::UnboundedSender<EngineEvent>,
    change: engine::LicenseChange,
) -> Result<(), LicenseError> {
    let (reply, rx) = oneshot::channel();
    engine
        .send(EngineEvent::License { change, reply })
        .map_err(|_| LicenseError::Stopping)?;
    rx.await.map_err(|_| LicenseError::Stopping)?
}

async fn jump_to(
    engine: &mpsc::UnboundedSender<EngineEvent>,
    device: &str,
) -> Result<(), JumpError> {
    let (reply, rx) = oneshot::channel();
    engine
        .send(EngineEvent::JumpTo {
            device: device.to_owned(),
            reply,
        })
        .map_err(|_| JumpError::Stopping)?;
    rx.await.map_err(|_| JumpError::Stopping)?
}

/// Lists the files and folders dragged from here, as the user where the
/// broker reads them, and has the engine let the cursor cross with them.
/// Returns the drag's id and what it carries.
async fn begin_drag(
    files: &Files,
    engine: &mpsc::UnboundedSender<EngineEvent>,
    paths: Vec<PathBuf>,
) -> Result<(String, listing::Manifest), DragError> {
    let manifest = files.manifest(paths).await?;
    let (reply, rx) = oneshot::channel();
    engine
        .send(EngineEvent::BeginDrag {
            manifest: manifest.clone(),
            reply,
        })
        .map_err(|_| DragError::Stopping)?;
    let id = rx.await.map_err(|_| DragError::Stopping)??;
    Ok((id, manifest))
}

async fn end_drag(engine: &mpsc::UnboundedSender<EngineEvent>, id: &str) -> bool {
    let (reply, rx) = oneshot::channel();
    let id = id.to_owned();
    engine.send(EngineEvent::EndDrag { id, reply }).is_ok() && rx.await.unwrap_or(false)
}

/// Checks the user can write into `destination`, as the user where the
/// broker writes files, then accepts drag `id` there.
async fn accept_drag(
    files: &Files,
    engine: &mpsc::UnboundedSender<EngineEvent>,
    id: &str,
    destination: PathBuf,
) -> Result<(), DragError> {
    files
        .check_destination(&destination)
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotConnected => DragError::NoBroker,
            _ => DragError::InvalidFolder(e.to_string()),
        })?;
    let (reply, rx) = oneshot::channel();
    engine
        .send(EngineEvent::AcceptDrag {
            id: id.to_owned(),
            destination,
            reply,
        })
        .map_err(|_| DragError::Stopping)?;
    match rx.await {
        Ok(true) => Ok(()),
        Ok(false) => Err(DragError::NotFound),
        Err(_) => Err(DragError::Stopping),
    }
}

async fn decline_drag(engine: &mpsc::UnboundedSender<EngineEvent>, id: &str) -> bool {
    let (reply, rx) = oneshot::channel();
    let id = id.to_owned();
    engine.send(EngineEvent::DeclineDrag { id, reply }).is_ok() && rx.await.unwrap_or(false)
}

async fn rearrange(
    engine: &mpsc::UnboundedSender<EngineEvent>,
    moves: Vec<(String, Point)>,
) -> Result<(), RearrangeError> {
    let (reply, rx) = oneshot::channel();
    let stopping = || RearrangeError::Invalid("the agent is stopping".to_owned());
    engine
        .send(EngineEvent::Rearrange { moves, reply })
        .map_err(|_| stopping())?;
    rx.await.map_err(|_| stopping())?
}

/// The desktop pictures as the platform poller last read them.
struct PictureWatch {
    backend: Arc<dyn DesktopPictures>,
    schedule: PictureSchedule,
    current: previews::Pictures,
    errors: PictureErrors,
}

/// When the platform poller reads the desktop pictures.
#[derive(Debug, Default)]
struct PictureSchedule {
    /// The displays of the last read that succeeded, and the backend's
    /// generation before it; none before the first.
    read_for: Option<(Vec<Display>, u64)>,
    /// When a read was last tried.
    tried_at: Option<Instant>,
}

impl PictureSchedule {
    /// Whether to read the pictures of `displays` at `now`, the backend
    /// being at `generation`: at once for displays they were not read for,
    /// which a failed read leaves as they were, so it is tried again on the
    /// next tick, and once the backend's generation moved; otherwise every
    /// [`PICTURE_POLL`]. Never for no displays, a passing state or a failure
    /// to list them, which would only empty the backend's cache of the
    /// pictures it made.
    fn due(&self, displays: &[Display], generation: u64, now: Instant) -> bool {
        if displays.is_empty() {
            return false;
        }
        match &self.read_for {
            Some((read, at)) if read.as_slice() == displays && *at == generation => {}
            _ => return true,
        }
        self.tried_at
            .is_none_or(|at| now.saturating_duration_since(at) >= PICTURE_POLL)
    }

    /// Notes a read of the pictures of `displays` at `now`, the backend
    /// having been at `generation` before it.
    fn tried(&mut self, displays: &[Display], generation: u64, now: Instant, succeeded: bool) {
        self.tried_at = Some(now);
        if succeeded {
            self.read_for = Some((displays.to_vec(), generation));
        }
    }
}

/// The last failure to read the desktop pictures, so that one that lasts
/// is logged once.
#[derive(Default)]
struct PictureErrors(Option<String>);

/// Reads the desktop pictures of `displays` off the async runtime. `None`
/// if they cannot be read.
async fn read_pictures(
    backend: &Arc<dyn DesktopPictures>,
    displays: &[Display],
    errors: &mut PictureErrors,
) -> Option<previews::Pictures> {
    let (backend, displays) = (backend.clone(), displays.to_vec());
    let failure = match tokio::task::spawn_blocking(move || backend.pictures(&displays)).await {
        Ok(Ok(pictures)) => {
            errors.0 = None;
            return Some(
                pictures
                    .into_iter()
                    .map(|(id, jpeg)| (id, Preview::new(jpeg)))
                    .collect(),
            );
        }
        Ok(Err(e)) => e.to_string(),
        Err(e) => e.to_string(),
    };
    if errors.0.as_deref() != Some(failure.as_str()) {
        warn!(error = %failure, "cannot read the desktop pictures");
        errors.0 = Some(failure);
    }
    None
}

/// Reports permissions, secure input, the lock screen, the display
/// configuration and the desktop pictures to the engine whenever they
/// change. The pictures are read when the displays change, and every
/// [`PICTURE_POLL`] (see [`PictureSchedule`]); the rest is reported first,
/// so a slow read never holds it up.
async fn watch_platform(
    status: Arc<dyn SessionStatus>,
    port: u16,
    displays: Box<dyn Displays>,
    mut pictures: PictureWatch,
    engine: mpsc::UnboundedSender<EngineEvent>,
) {
    let mut last: Option<PlatformState> = None;
    let mut ticks = tokio::time::interval(PLATFORM_POLL);
    let checks_firewall = status.checks_firewall();
    loop {
        ticks.tick().await;
        if engine.send(EngineEvent::RefreshInjection).is_err() {
            break;
        }
        let displays = match displays.displays() {
            Ok(displays) => displays,
            Err(e) => {
                warn!(error = %e, "cannot read the displays");
                last.as_ref()
                    .map(|l| l.displays.clone())
                    .unwrap_or_default()
            }
        };
        // The firewall may be read from files: not on the runtime, and
        // only where there is one to read.
        let firewall = if checks_firewall {
            let status = status.clone();
            tokio::task::spawn_blocking(move || status.firewall(port))
                .await
                .unwrap_or_else(|_| last.as_ref().map_or(Firewall::Unknown, |l| l.firewall.0))
        } else {
            Firewall::Unknown
        };
        let mut state = PlatformState {
            permissions: status.permissions(),
            secure_input: status.secure_input(),
            screen_locked: status.screen_locked(),
            lock_screen_input: status.lock_screen_input(),
            input_available: status.input_available(),
            displays,
            pictures: pictures.current.clone(),
            pictures_available: pictures.backend.available(),
            pointer_captured: status.pointer_captured(),
            session: status.session_info(),
            firewall: (firewall, port),
        };
        if !report(&engine, &mut last, &state) {
            break;
        }
        let now = Instant::now();
        let generation = pictures.backend.generation();
        if !pictures.schedule.due(&state.displays, generation, now) {
            continue;
        }
        let read = read_pictures(&pictures.backend, &state.displays, &mut pictures.errors).await;
        pictures
            .schedule
            .tried(&state.displays, generation, now, read.is_some());
        // Unchanged pictures keep their shared bytes, which compare
        // without being read.
        if let Some(read) = read.filter(|read| *read != pictures.current) {
            pictures.current = read;
            state.pictures = pictures.current.clone();
            if !report(&engine, &mut last, &state) {
                break;
            }
        }
    }
}

/// Reads the network at once and then every [`NETWORK_POLL`], on a task of
/// its own, so a slow read (NetworkManager on the system bus) holds up
/// nothing else; the engine waits for the first that settles before it
/// reaches any device (`Engine::hold_until`). Every reading goes to the
/// engine, which counts them; none for a read that failed, which leaves the
/// last one standing.
async fn watch_network(status: Arc<dyn SessionStatus>, engine: mpsc::UnboundedSender<EngineEvent>) {
    let mut ticks = tokio::time::interval(NETWORK_POLL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut reads = NetworkReads::default();
    loop {
        ticks.tick().await;
        if let Some(network) = read_network(&status, &mut reads).await
            && engine.send(EngineEvent::Network(network)).is_err()
        {
            return;
        }
    }
}

/// Sends `state` to the engine unless it was the last sent. False once the
/// engine is gone.
fn report(
    engine: &mpsc::UnboundedSender<EngineEvent>,
    last: &mut Option<PlatformState>,
    state: &PlatformState,
) -> bool {
    if last.as_ref() == Some(state) {
        return true;
    }
    if engine.send(EngineEvent::Platform(state.clone())).is_err() {
        return false;
    }
    *last = Some(state.clone());
    true
}

/// Announces this device and browses for others (mDNS) while the profile
/// in use shares; while it is paused, neither, and the devices seen are
/// forgotten. `fake` stands in for mDNS in tests.
async fn advertise(
    this: Arc<ThisDevice>,
    directory: Arc<Directory>,
    mut reach: watch::Receiver<Reach>,
    fake: bool,
) {
    loop {
        if reach.wait_for(|r| !r.quiet()).await.is_err() {
            return;
        }
        let Some(announcement) = this.announcement() else {
            return;
        };
        let started = if fake {
            #[cfg(feature = "test-hooks")]
            {
                Ok((this_device::Running::Fake, None))
            }
            #[cfg(not(feature = "test-hooks"))]
            unreachable!("a fake discovery only with the test hooks")
        } else {
            Discovery::start(&announcement)
                .map(|(discovery, events)| (this_device::Running::Mdns(discovery), Some(events)))
        };
        match started {
            Ok((running, events)) => {
                this.announce(running);
                let feed = events
                    .map(|events| AbortOnDrop(tokio::spawn(discover(directory.clone(), events))));
                let ended = reach.wait_for(|r| r.quiet()).await.is_err();
                this.stop_announcing();
                drop(feed);
                directory.clear_nearby();
                if ended {
                    return;
                }
                info!(
                    "the profile in use is paused: not announcing this device or looking for others"
                );
            }
            Err(e) => {
                warn!(error = %e, "mDNS is unavailable; other devices must be added by address");
                // Tried again once the profile in use paused and shares
                // again.
                if reach.wait_for(|r| r.quiet()).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// How often a read of the network that keeps failing the same way is
/// logged again.
const NETWORK_ERROR_LOG: Duration = Duration::from_secs(10 * 60);

/// Reads of the network as the platform poller takes them. A read that
/// fails leaves the last good one standing: only a read that succeeds and
/// shows no route means no network, or a lasting error (a system call
/// failing, a file that cannot be read) would switch the profile and drop
/// every session. A failure is logged once, and again after
/// [`NETWORK_ERROR_LOG`] or when it says something else.
#[derive(Debug, Default)]
struct NetworkReads {
    error: Option<(String, Instant)>,
}

impl NetworkReads {
    /// What to give the engine of `read` at `now`: the reading, or none
    /// for one that failed; and the failure to log, when that is due.
    fn take(
        &mut self,
        read: Result<Option<iw_platform::network::NetworkSighting>, String>,
        now: Instant,
    ) -> (
        Option<Option<iw_platform::network::NetworkSighting>>,
        Option<String>,
    ) {
        match read {
            Ok(network) => {
                self.error = None;
                (Some(network), None)
            }
            Err(e) => {
                let due = self.error.as_ref().is_none_or(|(last, at)| {
                    *last != e || now.saturating_duration_since(*at) >= NETWORK_ERROR_LOG
                });
                if due {
                    self.error = Some((e.clone(), now));
                }
                (None, due.then_some(e))
            }
        }
    }
}

/// Reads the network off the async runtime: the reading, or none when it
/// cannot be read (`NetworkReads`).
async fn read_network(
    status: &Arc<dyn SessionStatus>,
    reads: &mut NetworkReads,
) -> Option<Option<iw_platform::network::NetworkSighting>> {
    let status = status.clone();
    let read = match tokio::task::spawn_blocking(move || status.network()).await {
        Ok(Ok(network)) => Ok(network),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    let (reading, failure) = reads.take(read, Instant::now());
    if let Some(e) = failure {
        warn!(error = %e, "cannot read the network; keeping the last reading");
    }
    reading
}

/// Feeds what mDNS sees into the directory.
async fn discover(directory: Arc<Directory>, mut events: mpsc::UnboundedReceiver<DiscoveryEvent>) {
    while let Some(event) = events.recv().await {
        match event {
            DiscoveryEvent::Found(found) => directory.found(found),
            DiscoveryEvent::Lost(fp) => directory.lost(&fp),
        }
    }
}

impl AgentHandle {
    /// This device's id, as resolved at start.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// The UDP address peers connect to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    pub fn watch_status(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }

    /// Resolves once the user grants a permission that was missing.
    pub async fn permission_granted(&self) {
        let mut status = self.status.clone();
        let missing = |s: &Status| -> Vec<Permission> {
            s.permissions
                .iter()
                .filter(|p| !p.granted)
                .map(|p| p.permission)
                .collect()
        };
        let mut before = missing(&status.borrow_and_update());
        while status.changed().await.is_ok() {
            let now = status.borrow_and_update().clone();
            let granted = |p: &Permission| {
                now.permissions
                    .iter()
                    .any(|q| q.permission == *p && q.granted)
            };
            if before.iter().any(granted) {
                return;
            }
            before = missing(&now);
        }
        std::future::pending().await
    }

    pub fn arrangement(&self) -> Arrangement {
        self.arrangement.borrow().clone()
    }

    pub fn watch_arrangement(&self) -> watch::Receiver<Arrangement> {
        self.arrangement.clone()
    }

    /// Where the cursor was last shown on this device, in its desktop
    /// coordinates, after the user shook the mouse; changes each time it
    /// is shown, even at the same place.
    pub fn watch_cursor_located(&self) -> watch::Receiver<Option<Point>> {
        self.located.clone()
    }

    /// Has this device and every connected one show their names on their
    /// screens, unless it was asked less than a second ago.
    pub fn identify_screens(&self) {
        let _ = self.engine_tx.send(EngineEvent::IdentifyScreens);
    }

    /// How many milliseconds this device's apps were last told to show its
    /// name on its screens; changes each time they are told, even to the
    /// same.
    pub fn watch_screens_identified(&self) -> watch::Receiver<Option<u32>> {
        self.identified.clone()
    }

    /// The device the relative mode hotkey was last pressed on that cannot
    /// take relative movements; changes each time, even to the same.
    pub fn watch_relative_unsupported(&self) -> watch::Receiver<Option<String>> {
        self.relative_unsupported.clone()
    }

    /// Each time the cursor crosses an edge of this device's displays,
    /// going out or coming in, from now on.
    pub fn subscribe_cursor_crossed(&self) -> broadcast::Receiver<CursorCrossed> {
        self.cursor_crossed.subscribe()
    }

    /// Moves devices on the canvas and shares the new arrangement.
    pub async fn rearrange(&self, moves: Vec<(String, Point)>) -> Result<(), RearrangeError> {
        rearrange(&self.engine_tx, moves).await
    }

    pub fn set_sharing(&self, enabled: bool) {
        let _ = self.engine_tx.send(EngineEvent::SetSharing(enabled));
    }

    /// Locks the cursor to the device it is on, or unlocks it.
    pub fn set_locked(&self, locked: bool) {
        let _ = self.engine_tx.send(EngineEvent::SetLocked(locked));
    }

    /// Moves the cursor to the middle of a device's primary display.
    pub async fn jump_to(&self, device: &str) -> Result<(), JumpError> {
        jump_to(&self.engine_tx, device).await
    }

    /// Changes the profiles, or which is used, as the profile commands do;
    /// the status shows it once this returns.
    pub async fn change_profiles(&self, change: ProfileChange) -> Result<(), ProfileError> {
        change_profiles(&self.engine_tx, change).await
    }

    /// An app read the Wi-Fi name of the network `network` stands for
    /// (`ReportWifiName`).
    pub fn report_wifi_name(&self, network: &str, name: Option<&str>) {
        let _ = self.engine_tx.send(EngineEvent::WifiName {
            network: network.to_owned(),
            name: name.map(str::to_owned),
        });
    }

    /// Whether this device announces itself on the network now (mDNS):
    /// where discovery is on, while the profile in use shares.
    pub fn announcing(&self) -> bool {
        self.this.announcing()
    }

    /// Sends a wake-up to an offline device that told how to wake it
    /// (`WakeDevice`).
    pub async fn wake(&self, device: &str) -> Result<(), WakeError> {
        wake_device(&self.engine_tx, device).await
    }

    /// Has pushing the cursor toward an offline device wake it, or not
    /// (`SetWakeOnPush`).
    pub async fn set_wake_on_push(&self, on: bool) {
        set_wake_on_push(&self.engine_tx, on).await;
    }

    /// Takes a device, this one or a trusted one, offline for now, or
    /// brings it back (`SetOfflineForNow`).
    pub async fn set_offline_for_now(
        &self,
        device: &str,
        offline: bool,
    ) -> Result<(), OfflineError> {
        set_offline_for_now(&self.engine_tx, device, offline).await
    }

    /// Enters a license key here, for the app to activate (`SetLicenseKey`).
    pub async fn set_license_key(&self, key: &str) -> Result<(), LicenseError> {
        license(
            &self.engine_tx,
            engine::LicenseChange::SetKey(key.to_owned()),
        )
        .await
    }

    /// Records what Polar answered about `key` (`RecordLicenseResult`).
    pub async fn record_license_result(
        &self,
        key: &str,
        result: LicenseResult,
    ) -> Result<(), LicenseError> {
        let change = engine::LicenseChange::Record {
            key: key.to_owned(),
            result,
        };
        license(&self.engine_tx, change).await
    }

    /// Removes the license from this computer (`RemoveLicense`).
    pub async fn remove_license(&self) -> Result<(), LicenseError> {
        license(&self.engine_tx, engine::LicenseChange::Remove).await
    }

    /// Sets how long the cursor must push against an edge before it
    /// crosses, and remembers it.
    pub fn set_edge_dwell(&self, ms: u32) -> Result<(), DwellError> {
        set_edge_dwell(&self.directory, &self.engine_tx, ms)
    }

    /// Makes the cursor cross edges only while `modifier` is held, or
    /// whenever it reaches one, and remembers the choice.
    pub fn set_switch_modifier(&self, modifier: Option<Modifier>) {
        set_switch_modifier(&self.directory, &self.engine_tx, modifier);
    }

    /// Puts new hotkeys in effect, and remembers them, unless they cannot
    /// be used (`Hotkeys::check`).
    pub async fn set_hotkeys(&self, hotkeys: Hotkeys) -> Result<(), HotkeysError> {
        let change: HotkeysChange = Box::new(move |_| checked_here(hotkeys));
        set_hotkeys(&self.engine_tx, change).await
    }

    /// Turns relative mode on or off for the other device the cursor is
    /// on.
    pub async fn set_relative(&self, on: bool) -> Result<(), RelativeError> {
        set_relative(&self.engine_tx, on).await
    }

    /// Has devices enter relative mode by themselves when an app there
    /// holds the pointer, or not, and remembers the choice.
    pub fn set_relative_auto(&self, on: bool) {
        set_relative_auto(&self.directory, &self.engine_tx, on);
    }

    /// Devices seen on the network.
    pub fn nearby(&self) -> Vec<Found> {
        self.directory.nearby().as_ref().clone()
    }

    pub fn pairing(&self) -> PairingStatus {
        self.pairing.status()
    }

    pub fn watch_pairing(&self) -> watch::Receiver<PairingStatus> {
        self.pairing.watch()
    }

    /// Starts pairing with the device at `address`, which then shows a
    /// code. Fails at once only if a pairing is already running.
    pub async fn start_pairing(&self, address: SocketAddr) -> Result<(), Failure> {
        self.pairing.start(address).await
    }

    /// Tries the code shown on the other device.
    pub async fn submit_pairing_code(
        &self,
        code: PairingCode,
    ) -> Result<PairingStatus, NotWaiting> {
        self.pairing.submit(code).await
    }

    /// A plain-text report for bug reports; it holds nothing secret.
    pub async fn diagnostics(&self) -> String {
        diagnostics_report(
            &self.diagnostics,
            &self.status,
            &self.arrangement,
            &self.directory,
            &self.files,
        )
        .await
    }

    /// Cancels the pairing started here, or declines the one shown here.
    pub fn cancel_pairing(&self) {
        self.pairing.cancel();
    }

    /// Forgets a paired device and ends any session with it.
    pub fn unpair(&self, id: &str) -> Result<(), DeviceError> {
        self.directory.remove_paired(id)
    }

    /// Swaps ⌘ and Ctrl in keys sent to a paired device, or stops doing so.
    pub fn set_swap_gui_and_control(&self, id: &str, swap: bool) -> Result<(), DeviceError> {
        self.directory.set_swap(id, swap).map(|_| ())
    }

    /// Names a trusted device on this device only, or with an empty name
    /// shows its own name again.
    pub fn rename_device(&self, id: &str, name: &str) -> Result<(), DeviceError> {
        self.directory.set_alias(id, name).map(|_| ())
    }

    /// Makes the cursor move faster or slower on a trusted device, in
    /// percent of how the mouse moves it here; remembered until it is
    /// unpaired.
    pub fn set_device_speed(&self, id: &str, percent: u32) -> Result<(), DeviceError> {
        self.directory.set_speed(id, percent).map(|_| ())
    }

    /// Stops sharing the clipboard or files with a trusted device, or
    /// shares them again; remembered.
    pub fn set_device_sharing(
        &self,
        id: &str,
        clipboard_off: bool,
        files_off: bool,
    ) -> Result<(), DeviceError> {
        self.directory
            .set_sharing(id, clipboard_off, files_off)
            .map(|_| ())
    }

    /// Starts sending files and folders to a device, as a batch; returns
    /// its items' ids, in order. Where the broker reads files, it lists
    /// them first, as the user. Items that cannot be sent are listed as
    /// failed; the request is refused only when none can be.
    pub async fn send_files(
        &self,
        device: &str,
        paths: Vec<PathBuf>,
    ) -> Result<Vec<String>, SendError> {
        self.files.send(device, paths).await
    }

    /// An app reports a drag of the files and folders at `paths` from
    /// here, a mouse button being down; returns the drag's id.
    pub async fn begin_drag(&self, paths: Vec<PathBuf>) -> Result<String, DragError> {
        begin_drag(&self.files, &self.engine_tx, paths)
            .await
            .map(|(id, _)| id)
    }

    /// The drag that began here ended without crossing; false if it was
    /// not going on.
    pub async fn end_drag(&self, id: &str) -> bool {
        end_drag(&self.engine_tx, id).await
    }

    /// This device's user dropped a drag another device offered into
    /// `destination`.
    pub async fn accept_drag(&self, id: &str, destination: PathBuf) -> Result<(), DragError> {
        accept_drag(&self.files, &self.engine_tx, id, destination).await
    }

    /// This device's user did not drop a drag another device offered;
    /// false if it is not offered here.
    pub async fn decline_drag(&self, id: &str) -> bool {
        decline_drag(&self.engine_tx, id).await
    }

    /// What the apps hear about drags, from now on.
    pub fn watch_drags(&self) -> broadcast::Receiver<DragEvent> {
        self.drag_events.subscribe()
    }

    pub fn transfers(&self) -> Vec<TransferStatus> {
        self.files.transfers()
    }

    pub fn watch_transfers(&self) -> watch::Receiver<Vec<TransferStatus>> {
        self.files.watch()
    }

    pub fn cancel_transfer(&self, id: &str) -> bool {
        self.files.cancel(id)
    }

    /// Takes a finished transfer off the list and out of the history; its
    /// file stays.
    pub fn remove_transfer(&self, id: &str) -> Result<(), RemoveError> {
        self.files.remove(id)
    }

    /// Takes every finished transfer off the list and out of the history;
    /// their files stay.
    pub fn clear_transfers(&self) {
        self.files.clear();
    }

    /// Accepts or declines an incoming file this device asked about.
    pub fn answer_file_offer(&self, id: &str, accept: bool) -> bool {
        self.files.answer(id, accept)
    }

    /// Where received files go now, and whether the user is asked.
    pub fn file_settings(&self) -> FileSettings {
        self.files.settings()
    }

    pub fn set_file_settings(&self, settings: FileSettings) {
        self.directory
            .set_file_settings(settings.folder.clone(), settings.ask);
        self.files.set_settings(settings);
    }

    /// Turns clipboard sync on or off, and remembers the choice.
    pub fn set_clipboard_sync(&self, on: bool) {
        set_clipboard_sync(&self.directory, &self.engine_tx, on);
    }

    /// Shares previews of this device's desktop pictures with connected
    /// devices, or stops and has them drop the ones they have; remembers
    /// the choice.
    pub fn set_preview_sharing(&self, on: bool) {
        set_preview_sharing(&self.directory, &self.engine_tx, on);
    }

    /// Lets other devices control this Mac at the login window, or stops
    /// them (see [`LoginWindowError`] for when that is refused).
    pub fn set_login_window_access(&self, on: bool) -> Result<(), LoginWindowError> {
        set_login_window_access(
            &self.directory,
            &self.engine_tx,
            self.login_window,
            &self.status,
            on,
        )
    }

    /// Renames this device. Sessions opened from now on, the mDNS
    /// announcement and the status use the new name.
    pub fn rename(&self, name: &str) -> Result<(), RenameError> {
        rename(&self.this, &self.directory, &self.engine_tx, name)
    }

    /// Hands control back to its owner, releases everything held, then stops.
    pub async fn shutdown(mut self) {
        let _ = self.engine_tx.send(EngineEvent::Shutdown);
        let _ = (&mut self.engine_task).await;
        // Give the session writers a moment to flush the final Leave.
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.stop().await;
    }

    /// Stops abruptly, as a crash would: no Leave, no release of held keys.
    /// Peers only learn about it from the closed connection.
    pub async fn abort(mut self) {
        self.engine_task.abort();
        self.stop().await;
    }

    async fn stop(&mut self) {
        // Transfers cut off by stopping are not history.
        self.files.close_history().await;
        // The tasks first, so none announces this device after; one that
        // did meanwhile is refused (`ThisDevice::stop`).
        self.tasks.shutdown().await;
        self.this.stop();
        self.endpoint.close();
        let _ = tokio::time::timeout(Duration::from_secs(1), self.endpoint.wait_idle()).await;
    }
}

#[cfg(test)]
mod tests {
    use iw_core::geometry::Rect;

    use super::*;

    fn display(id: &str) -> Display {
        Display {
            id: id.to_owned(),
            bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            scale: 1.0,
            primary: true,
        }
    }

    const TICK: Duration = PLATFORM_POLL;

    #[test]
    fn a_failed_read_of_the_network_keeps_the_last_and_is_logged_now_and_then() {
        use iw_platform::network::{LinkKind, NetworkSighting};
        let home = NetworkSighting {
            interface: "en0".to_owned(),
            kind: LinkKind::Wired,
            address: None,
            router: Some(std::net::Ipv4Addr::new(192, 168, 1, 1)),
            router_mac: Some([2; 6]),
            wifi_name: None,
            network_name: None,
        };
        let start = Instant::now();
        let mut reads = NetworkReads::default();
        assert_eq!(
            reads.take(Ok(Some(home.clone())), start),
            (Some(Some(home)), None)
        );
        // Failing: nothing for the engine, so the last reading stands;
        // logged once, then again only later or when it says something else.
        let failed = || Err("sysctl: cannot allocate memory".to_owned());
        assert_eq!(
            reads.take(failed(), start),
            (None, Some("sysctl: cannot allocate memory".to_owned()))
        );
        assert_eq!(reads.take(failed(), start + TICK), (None, None));
        assert_eq!(
            reads.take(failed(), start + NETWORK_ERROR_LOG),
            (None, Some("sysctl: cannot allocate memory".to_owned()))
        );
        assert_eq!(
            reads
                .take(Err("other".to_owned()), start + NETWORK_ERROR_LOG + TICK)
                .1
                .as_deref(),
            Some("other")
        );
        // A read that succeeds and shows no route is no network.
        assert_eq!(
            reads.take(Ok(None), start + NETWORK_ERROR_LOG * 2),
            (Some(None), None)
        );
        assert_eq!(
            reads
                .take(failed(), start + NETWORK_ERROR_LOG * 2 + TICK)
                .1
                .as_deref(),
            Some("sysctl: cannot allocate memory")
        );
    }

    #[test]
    fn pictures_are_read_first_thing_then_every_poll() {
        let start = Instant::now();
        let one = vec![display("1")];
        let mut schedule = PictureSchedule::default();
        assert!(schedule.due(&one, 0, start));
        schedule.tried(&one, 0, start, true);
        assert!(!schedule.due(&one, 0, start + TICK));
        assert!(!schedule.due(&one, 0, start + PICTURE_POLL - TICK));
        assert!(schedule.due(&one, 0, start + PICTURE_POLL));
    }

    #[test]
    fn changed_displays_are_read_at_once() {
        let start = Instant::now();
        let one = vec![display("1")];
        let two = vec![display("1"), display("2")];
        let mut schedule = PictureSchedule::default();
        schedule.tried(&one, 0, start, true);
        assert!(schedule.due(&two, 0, start + TICK));
        schedule.tried(&two, 0, start + TICK, true);
        assert!(!schedule.due(&two, 0, start + 2 * TICK));
        // Back to the first displays is a change too.
        assert!(schedule.due(&one, 0, start + 2 * TICK));
    }

    #[test]
    fn a_failed_read_after_a_change_is_tried_again_on_the_next_tick() {
        let start = Instant::now();
        let one = vec![display("1")];
        let two = vec![display("1"), display("2")];
        let mut schedule = PictureSchedule::default();
        // Even the first read.
        schedule.tried(&one, 0, start, false);
        assert!(schedule.due(&one, 0, start + TICK));
        schedule.tried(&one, 0, start + TICK, true);

        schedule.tried(&two, 0, start + 2 * TICK, false);
        assert!(schedule.due(&two, 0, start + 3 * TICK));
        schedule.tried(&two, 0, start + 3 * TICK, true);
        assert!(!schedule.due(&two, 0, start + 4 * TICK));
    }

    #[test]
    fn a_failed_poll_waits_for_the_next_one() {
        let start = Instant::now();
        let one = vec![display("1")];
        let mut schedule = PictureSchedule::default();
        schedule.tried(&one, 0, start, true);
        schedule.tried(&one, 0, start + PICTURE_POLL, false);
        assert!(!schedule.due(&one, 0, start + PICTURE_POLL + TICK));
        assert!(schedule.due(&one, 0, start + 2 * PICTURE_POLL));
    }

    #[test]
    fn pictures_are_read_again_once_the_backend_has_new_ones() {
        let start = Instant::now();
        let one = vec![display("1")];
        let mut schedule = PictureSchedule::default();
        schedule.tried(&one, 0, start, true);
        assert!(!schedule.due(&one, 0, start + TICK));
        // The broker sent pictures.
        assert!(schedule.due(&one, 1, start + TICK));
        schedule.tried(&one, 1, start + TICK, true);
        assert!(!schedule.due(&one, 1, start + 2 * TICK));
        // A read that fails leaves the generation as it was read before.
        schedule.tried(&one, 2, start + 2 * TICK, false);
        assert!(schedule.due(&one, 2, start + 3 * TICK));
    }

    #[test]
    fn no_displays_never_reach_the_backend() {
        let start = Instant::now();
        let one = vec![display("1")];
        let mut schedule = PictureSchedule::default();
        assert!(!schedule.due(&[], 0, start));
        schedule.tried(&one, 0, start, true);
        // An error listing the displays, or a moment without any, leaves
        // the pictures read for the displays before.
        assert!(!schedule.due(&[], 0, start + TICK));
        assert!(!schedule.due(&[], 0, start + 2 * PICTURE_POLL));
        assert!(!schedule.due(&one, 0, start + 2 * TICK));
    }

    /// Answers the next change of the hotkeys as the engine does, putting
    /// it in effect, with `saved` to say once the devices file is written.
    async fn answer(events: &mut mpsc::UnboundedReceiver<EngineEvent>, saved: engine::Saved) {
        match events.recv().await {
            Some(EngineEvent::SetHotkeys { change, reply }) => {
                let result = change(&Hotkeys::default()).map(|_| Some(saved));
                let _ = reply.send(result);
            }
            _ => panic!("not a change of the hotkeys"),
        }
    }

    /// The answer to a change of the hotkeys waits for the devices file,
    /// but not for ever, and says when the file could not be written.
    #[tokio::test]
    async fn a_change_of_the_hotkeys_is_answered_once_saved_or_not_or_too_late() {
        let (engine, mut events) = mpsc::unbounded_channel();
        let change = || -> HotkeysChange {
            Box::new(|current| {
                Ok(Hotkeys {
                    take_back_double: true,
                    ..*current
                })
            })
        };
        // Written.
        let (done, saved) = oneshot::channel();
        let (result, ()) = tokio::join!(set_hotkeys(&engine, change()), async {
            answer(&mut events, saved).await;
            let _ = done.send(Ok(()));
        });
        assert_eq!(result, Ok(()));
        // In effect, but not written.
        let (done, saved) = oneshot::channel();
        let (result, ()) = tokio::join!(set_hotkeys(&engine, change()), async {
            answer(&mut events, saved).await;
            let _ = done.send(Err(std::io::Error::other("no space left")));
        });
        let error = result.unwrap_err();
        assert_eq!(error.code(), "not_saved");
        assert!(error.to_string().contains("in effect"), "{error}");
        assert!(error.to_string().contains("no space left"), "{error}");
        // Still being written after the bound: the answer goes, the write
        // goes on.
        let (done, saved) = oneshot::channel();
        let started = Instant::now();
        let (result, ()) = tokio::join!(set_hotkeys(&engine, change()), answer(&mut events, saved));
        assert_eq!(result, Err(HotkeysError::Timeout));
        assert_eq!(HotkeysError::Timeout.code(), "timeout");
        assert!(started.elapsed() >= APPLY_TIMEOUT);
        drop(done);
        // And an engine that does not get to it in time.
        let result = set_hotkeys(&engine, change()).await;
        assert_eq!(result, Err(HotkeysError::Timeout));
        assert!(events.try_recv().is_ok());
        // Refused: nothing to wait for.
        let refused: HotkeysChange = Box::new(|_| Err(HotkeyError::NeedsModifier("K".into())));
        let (result, ()) = tokio::join!(set_hotkeys(&engine, refused), async {
            let (_done, saved) = oneshot::channel();
            answer(&mut events, saved).await;
        });
        assert_eq!(result.unwrap_err().code(), "needs_modifier");
    }
}
