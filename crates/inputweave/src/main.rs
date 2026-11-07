//! The single InputWeave executable. Each role runs as a subcommand.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use inputweave::agent::{self, Backends, Options};
use inputweave::config::{BackendConfig, Config};
use inputweave::paths;
use iw_core::HidUsage;
use iw_core::geometry::Point;
use iw_core::input::Button;
use iw_net::Identity;
use iw_platform::mock::{MockCapture, MockStatus, StaticDisplays};
use iw_platform::{InputEmulation, PlatformError};

#[derive(Parser)]
#[command(
    version,
    about = "Share one keyboard and mouse across computers on a LAN"
)]
struct Cli {
    #[command(subcommand)]
    role: Role,
}

#[derive(Subcommand)]
enum Role {
    /// Run the agent: input capture and injection, networking, clipboard and files.
    Agent {
        /// Configuration file to use instead of the default location.
        #[arg(long, conflicts_with = "login_window")]
        config: Option<PathBuf>,
        /// Run at the macOS login window, as launchd starts it there: as the
        /// device of the user who logged in last, if they turned it on, and
        /// only to be controlled.
        #[arg(long)]
        login_window: bool,
    },
    /// Talk to a running agent.
    Ctl {
        /// The agent's IPC socket (named pipe on Windows), if not at the
        /// default location.
        #[arg(long, global = true)]
        socket: Option<PathBuf>,
        #[command(subcommand)]
        command: CtlCommand,
    },
    /// Print this device's certificate fingerprint, creating its identity
    /// on first use. Peers can list it in their configuration to trust this
    /// device without pairing.
    Identity {
        /// Configuration file to use instead of the default location.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Print whether the permissions the agent needs are granted, as macOS
    /// answers a new process. The agent runs it to notice a permission the
    /// user grants while it runs.
    #[command(hide = true)]
    Permissions,
    /// Run as the Windows service that supervises the agent and broker.
    Service,
    /// Run as the Windows user-context broker, which does what the agent
    /// must not as SYSTEM, such as reading the desktop pictures.
    Broker,
}

#[derive(Subcommand)]
enum CtlCommand {
    /// Show sharing state and trusted devices.
    Status,
    /// List devices found on the local network.
    Nearby,
    /// Pair with a device. It shows a code, which is read from standard
    /// input.
    Pair {
        /// A nearby device's id, or an address ("192.168.1.20" or
        /// "192.168.1.20:47820").
        target: String,
    },
    /// Cancel the pairing started here, or decline the one shown here.
    Cancel,
    /// Forget a paired device.
    Unpair {
        /// The device's id, as `ctl status` lists it.
        device: String,
    },
    /// Print status and pairing changes as they happen. On the device being
    /// paired with, this is where the pairing code appears; after the mouse
    /// is shaken with the cursor here, where the cursor is.
    Watch,
    /// Show where each device sits on the shared canvas.
    Layout,
    /// Send files and folders to a device, and wait until they arrived;
    /// then list what failed, and how many files arrived.
    Send {
        device: String,
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// List file transfers, each with its id, files and folders sent
    /// together listed together, and where received files go. Finished
    /// ones stay listed until removed; removing them leaves the files
    /// where they are.
    Transfers {
        /// Take every finished transfer off the list.
        #[arg(long, conflicts_with = "remove")]
        clear: bool,
        /// Take this finished transfer off the list.
        #[arg(long, value_name = "ID")]
        remove: Option<String>,
    },
    /// Write a report for bug reports: versions, state, configuration and
    /// the end of the logs. It holds no keys or file contents.
    Diagnose {
        /// Write it to this file instead of standard output.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Move a device's desktop origin on the shared canvas.
    Move { device: String, x: f64, y: f64 },
    /// Lock the cursor to the device it is on, or unlock it, as the lock
    /// hotkey does.
    Lock { state: OnOff },
    /// Send the mouse's movements as such to the other device the cursor
    /// is on, for games that hide and hold the cursor, or stop; the cursor
    /// crosses no edge meanwhile. With --auto, whether a device enters it
    /// by itself while an app there holds the pointer.
    Relative {
        #[arg(required_unless_present = "auto")]
        state: Option<OnOff>,
        /// Enter relative mode by itself while an app on the device holds
        /// the pointer, as Windows tells; on unless turned off.
        #[arg(long, conflicts_with = "state")]
        auto: Option<OnOff>,
    },
    /// Send a wake-up over the network (Wake-on-LAN) to an offline device
    /// that told this one how to wake it, as the app's Wake does. With
    /// --push, whether pushing the cursor toward such a device wakes it.
    Wake {
        /// A device id as `ctl status` lists it.
        #[arg(required_unless_present = "push")]
        device: Option<String>,
        /// Wake an offline device when the cursor pushes against the edge
        /// toward it, at most every 30 seconds; off unless turned on.
        #[arg(long, conflicts_with = "device")]
        push: Option<OnOff>,
    },
    /// Move the cursor to the middle of a device's main display.
    Jump {
        /// A device id as `ctl status` lists it, or this device's.
        device: String,
    },
    /// Make the cursor push against an edge for this long before it
    /// crosses to another device; 0 crosses at once.
    Dwell {
        /// Milliseconds, at most 1000.
        ms: u32,
    },
    /// Show each device's name on its screens for a moment, on this device
    /// and every connected one.
    Identify,
    /// Ask macOS for the folders files are sent from and saved in
    /// (Downloads, Desktop, Documents), as first-run setup does: macOS asks
    /// about each it was not told of. Waits up to two minutes for the
    /// answers.
    Folders,
    /// Ask the desktop for a permission the agent asks for itself, as
    /// first-run setup does: on Wayland, `remote-control` lets other devices
    /// control this one (the RemoteDesktop portal), `input-capture` lets
    /// this one's keyboard and mouse control them (the InputCapture
    /// portal). Waits up to two minutes for the answer.
    Permission {
        /// remote-control or input-capture.
        name: String,
    },
    /// Give another device a name shown on this device only, or show its
    /// own name again. The device and other devices are not told.
    Rename {
        /// A device id as `ctl status` lists it.
        device: String,
        /// The name to show for it.
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        name: Option<String>,
        /// Show the name the device gives itself again.
        #[arg(long)]
        clear: bool,
    },
    /// Stop sharing the clipboard or files with a device, or share them
    /// again, or change how fast the cursor moves on it; without options,
    /// show the device. Only this device changes; the other one is not
    /// told.
    Device {
        /// A device id as `ctl status` lists it.
        device: String,
        /// Sync the clipboard with it.
        #[arg(long)]
        clipboard: Option<OnOff>,
        /// Send files to it and take its files.
        #[arg(long)]
        files: Option<OnOff>,
        /// How fast the cursor moves on the device while this one controls
        /// it, in percent of how the mouse moves it here: 50 to 300, in
        /// steps of 10; 100 is as here. Relative mouse movements are not
        /// changed.
        #[arg(long, value_name = "PERCENT")]
        speed: Option<u32>,
    },
    /// Show how the cursor crosses edges to other devices, or make it
    /// cross only while a modifier is held.
    Edge {
        /// The modifier to hold, either one; none crosses whenever the
        /// cursor reaches an edge.
        #[arg(long)]
        modifier: Option<SwitchModifierArg>,
    },
    /// Show the hotkeys and the number each device is jumped to by, or
    /// change them.
    Hotkeys {
        #[command(subcommand)]
        action: Option<HotkeysAction>,
    },
    /// Let other devices control this Mac at the login window, from the
    /// next time it shows, or stop them. It needs other devices to be able
    /// to type on this Mac's lock screen.
    LoginWindow { state: OnOff },
    /// Show this computer's license, the key masked; or enter or remove its
    /// key. The app activates a key entered here with Polar when it runs.
    License {
        #[command(subcommand)]
        action: Option<LicenseAction>,
    },
}

#[derive(Subcommand)]
enum LicenseAction {
    /// Enter a license key, read from standard input so it stays out of the
    /// shell's history and the list of processes: paste it at the prompt,
    /// or pipe it in (`echo KEY | inputweave ctl license set`). Paired
    /// devices without a key take it once the app has activated it here.
    Set,
    /// Remove the license from this computer. Free its seat in Polar's
    /// customer portal, or with the app's Remove License, which does that
    /// itself; this computer takes no key from paired devices until one is
    /// entered again.
    Remove,
}

#[derive(Subcommand)]
enum HotkeysAction {
    /// Change the hotkeys given; the others stay. A hotkey is written as
    /// modifiers (control, alt, shift, gui) and a key's UI Events code name,
    /// joined by "+", such as control+alt+shift+KeyL; taking back and the
    /// lock may also be ScrollLock, Pause or F13 to F19 alone.
    Set {
        /// Takes the keyboard and mouse back to this device.
        #[arg(long, value_name = "HOTKEY")]
        take_back: Option<String>,
        /// Control pressed twice quickly, while controlling another device,
        /// takes the keyboard and mouse back too: "double+control" turns it
        /// on, an empty value off.
        #[arg(long, value_name = "DOUBLE")]
        take_back_double: Option<String>,
        /// Locks the cursor to the device it is on, or unlocks it.
        #[arg(long, value_name = "HOTKEY")]
        lock: Option<String>,
        /// Held with a digit from 1 to 9, moves the cursor to the device with
        /// that number; modifiers alone, such as control+alt+shift. An empty
        /// value turns these hotkeys off.
        #[arg(long, value_name = "MODIFIERS")]
        jump_modifiers: Option<String>,
        /// Turns relative mode on or off for the other device the cursor is
        /// on. An empty value turns this hotkey off.
        #[arg(long, value_name = "HOTKEY")]
        relative: Option<String>,
    },
    /// Go back to the default hotkeys.
    Reset,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum SwitchModifierArg {
    None,
    Control,
    Alt,
    Shift,
    Gui,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.role {
        Role::Agent {
            login_window: true, ..
        } => run_login_window(),
        Role::Agent { config, .. } => run_agent(config),
        Role::Ctl { socket, command } => ctl(socket, command),
        Role::Identity { config } => print_identity(config),
        Role::Permissions => print_permissions(),
        #[cfg(windows)]
        Role::Service => run_service(),
        #[cfg(windows)]
        Role::Broker => {
            log_to(std::io::stderr);
            run_broker()
        }
        #[cfg(not(windows))]
        Role::Service | Role::Broker => {
            Err("the service and broker roles only exist on Windows".to_owned())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("inputweave: {message}");
            ExitCode::FAILURE
        }
    }
}

fn config_path(given: Option<PathBuf>) -> Result<PathBuf, String> {
    given
        .or_else(paths::default_config_file)
        .ok_or_else(|| "cannot determine the configuration path; pass --config".to_owned())
}

fn devices_file(config_file: &Path, config: &Config) -> PathBuf {
    config
        .paths
        .devices_file
        .clone()
        .unwrap_or_else(|| paths::default_devices_file(config_file))
}

fn identity_dir(config_file: &Path, config: Option<&Config>) -> PathBuf {
    config
        .and_then(|c| c.paths.identity_dir.clone())
        .unwrap_or_else(|| paths::default_identity_dir(config_file))
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Runtime::new().map_err(|e| format!("cannot start the async runtime: {e}"))
}

/// Logs go to the configuration's log file, to stderr, or to the log file
/// when launchd started the agent and nobody would see stderr.
fn init_logging(log_file: Option<&Path>) {
    if let Some(path) = log_file {
        match inputweave::log_file::CappedLog::open(path) {
            Ok(log) => return log_into(log, false),
            Err(e) => eprintln!(
                "inputweave: cannot write the log to {}: {e}; logging to standard error",
                path.display()
            ),
        }
    }
    let under_launchd = std::env::var_os("XPC_SERVICE_NAME").is_some_and(|v| v != "0");
    if under_launchd && let Some(file) = paths::default_log_file().and_then(|p| open_log(&p, None))
    {
        return log_to(std::sync::Mutex::new(file));
    }
    log_to(std::io::stderr);
}

/// Sends log output to `writer`. Colors only go to a terminal: on Windows
/// the service sends the agent's and broker's output to log files.
fn log_to<W>(writer: W)
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    log_into(writer, std::io::stderr().is_terminal());
}

fn log_into<W>(writer: W, colors: bool)
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(colors)
        .with_writer(writer)
        .init();
}

/// Opens the log for appending, moving a log over 5 MB aside first. With
/// a `mode`, the log has it, even one an earlier version made otherwise.
fn open_log(path: &Path, mode: Option<u32>) -> Option<std::fs::File> {
    const MAX_LEN: u64 = 5 * 1024 * 1024;
    std::fs::create_dir_all(path.parent()?).ok()?;
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_LEN) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    // A new log has the mode from the start, an existing one gets it.
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    let file = options.open(path).ok()?;
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .ok()?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    Some(file)
}

/// Runs the agent at the macOS login window (`login_window`). Whatever
/// keeps it from running there ends it successfully, so launchd does not
/// start it again until the login window next shows; only the port still
/// taken, as while another session's agent lets go of it, ends it
/// otherwise (`login_window::worth_retrying`).
#[cfg(target_os = "macos")]
fn run_login_window() -> Result<(), String> {
    use inputweave::login_window::{self, LoginWindowSession};
    use iw_platform::macos::{MacDisplays, MacEmulation};

    // It logs as root, about the user's files.
    match open_log(Path::new(login_window::LOG_FILE), Some(0o600)) {
        Some(file) => log_to(std::sync::Mutex::new(file)),
        None => log_to(std::io::stderr),
    }
    let Some(user) =
        iw_platform::macos::last_login_user().filter(|name| login_window::valid_user_name(name))
    else {
        tracing::info!("nobody has logged in on this Mac yet; nothing to do");
        return Ok(());
    };
    // Logged in still, in the background of fast user switching: their own
    // agent is this device, and holds its port.
    if iw_platform::system::console_session(&user) {
        tracing::info!(%user, "the last user is still logged in; their own agent is this device");
        return Ok(());
    }
    let Some(account) = iw_platform::system::account(&user) else {
        tracing::warn!(%user, "the last user has no account");
        return Ok(());
    };
    let state = match login_window::load(&account.home, account.uid) {
        Ok(Some(state)) => state,
        Ok(None) => {
            tracing::info!(%user, "the last user did not let other devices control this Mac here");
            return Ok(());
        }
        Err(e) => {
            tracing::warn!(%user, error = %e, "cannot read the last user's InputWeave settings");
            return Ok(());
        }
    };
    let emulation = match MacEmulation::virtual_only() {
        Ok(emulation) => emulation,
        Err(e) => {
            tracing::warn!(error = %e, "input from other devices cannot reach the login window");
            return Ok(());
        }
    };
    let backends = Backends {
        capture: Box::new(iw_platform::NoCapture),
        emulation: Box::new(emulation),
        displays: Box::new(MacDisplays),
        pictures: Box::new(iw_platform::NoDesktopPictures),
        status: Box::new(LoginWindowSession),
        clipboard: None,
    };
    let options = Options {
        log_files: vec![PathBuf::from(login_window::LOG_FILE)],
        agent_digest: agent::executable_digest().unwrap_or_default(),
        ..login_window::options(state.devices)
    };
    runtime()?.block_on(async move {
        let agent =
            match agent::start(state.config, Arc::new(state.identity), backends, options).await {
                Ok(agent) => agent,
                Err(e) if login_window::worth_retrying(&e) => return Err(e.to_string()),
                Err(e) => {
                    tracing::warn!(error = %e, "cannot run at the login window");
                    return Ok(());
                }
            };
        tracing::info!(
            %user,
            device = %agent.device_id(),
            addr = %agent.local_addr(),
            "agent running at the login window"
        );
        // launchd ends it as someone logs in.
        stop_requested().await;
        tracing::info!("shutting down");
        agent.shutdown().await;
        Ok(())
    })
}

#[cfg(not(target_os = "macos"))]
fn run_login_window() -> Result<(), String> {
    Err("the login window's agent only exists on macOS".to_owned())
}

/// How long files pasted from another device stay where they were fetched
/// into once something else is copied: the app that pasted them may still
/// be copying them from there.
const CLIPBOARD_STAGING_GRACE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// How long `ctl folders` and `ctl permission` wait for the user to answer
/// the system.
const FOLDERS_WAIT: std::time::Duration = std::time::Duration::from_secs(2 * 60);

fn run_agent(config: Option<PathBuf>) -> Result<(), String> {
    let given_config = config.is_some();
    let path = config_path(config)?;
    let config = Config::load_or_default(&path).map_err(|e| e.to_string())?;
    init_logging(config.paths.log_file.as_deref());
    let identity = Identity::load_or_generate(&identity_dir(&path, Some(&config)))
        .map_err(|e| format!("cannot load the device identity: {e}"))?;
    let backends = match &config.backend {
        BackendConfig::Mock { displays, .. } => {
            tracing::warn!("running with mock backends: no real input is captured or injected");
            Backends {
                capture: Box::new(MockCapture::new().0),
                emulation: Box::new(LoggingEmulation),
                displays: Box::new(StaticDisplays(displays.iter().map(Into::into).collect())),
                pictures: Box::new(iw_platform::NoDesktopPictures),
                status: Box::new(MockStatus::granted()),
                clipboard: None,
            }
        }
        BackendConfig::Native => {
            // What the backends remember goes next to the devices file.
            let state_dir = devices_file(&path, &config).parent().map(Path::to_path_buf);
            #[cfg(target_os = "macos")]
            let fresh_permissions = Some(std::sync::Arc::new(fresh_permissions) as _);
            #[cfg(not(target_os = "macos"))]
            let fresh_permissions = None;
            let native = iw_platform::native_with(&iw_platform::NativeOptions {
                state_dir,
                fresh_permissions,
            })
            .map_err(|e| e.to_string())?;
            let status = native.status;
            Backends {
                capture: native.capture,
                emulation: native.emulation,
                displays: native.displays,
                pictures: native.pictures,
                status,
                clipboard: native.clipboard,
            }
        }
    };
    // On macOS a permission granted while the agent runs only applies to a
    // new process, so the agent then starts over.
    let restart_on_grant =
        cfg!(target_os = "macos") && matches!(config.backend, BackendConfig::Native);
    let devices = devices_file(&path, &config);
    let options = Options {
        ipc_socket: config
            .paths
            .ipc_socket
            .clone()
            .or_else(paths::default_ipc_socket),
        transfers_file: Some(paths::transfers_file(&devices)),
        licenses: agent::Licenses::File(paths::license_file(&devices)),
        license_reread: None,
        devices_file: Some(devices),
        devices: None,
        previews_unchanged: false,
        // The login window's agent reads only the default folder.
        login_window: inputweave::login_window::support(
            cfg!(target_os = "macos") && matches!(config.backend, BackendConfig::Native),
            Path::new(inputweave::login_window::AGENT_PLIST).exists(),
            !given_config
                && config.paths.devices_file.is_none()
                && config.paths.identity_dir.is_none(),
        ),
        receive_folder: config
            .paths
            .receive_folder
            .clone()
            .or_else(paths::default_receive_folder),
        // Never this Mac's own folders for a mock agent.
        user_folders: match &config.backend {
            BackendConfig::Native => paths::user_folders(),
            BackendConfig::Mock { folders, .. } => folders.as_deref().map(paths::user_folders_in),
        },
        // A mock agent runs in tests and demos, where nobody is there to
        // answer.
        open_app: matches!(config.backend, BackendConfig::Native),
        // Linux takes no part in dragging files between devices.
        refuses_drags: cfg!(target_os = "linux") && matches!(config.backend, BackendConfig::Native),
        // The configuration's log file is the log; there is no other then.
        log_files: config
            .paths
            .log_file
            .clone()
            .map_or_else(paths::log_files, |file| vec![file]),
        journal_unit: config
            .paths
            .log_file
            .is_none()
            .then(paths::journal_unit)
            .flatten(),
        // On Windows the broker does what the agent must not as SYSTEM.
        broker_socket: matches!(config.backend, BackendConfig::Native)
            .then(paths::default_broker_pipe)
            .flatten(),
        agent_digest: agent::executable_digest().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "cannot read the agent's executable to identify the build");
            String::new()
        }),
        peer_protocol_max: None,
        clipboard_staging: paths::clipboard_staging(),
        clipboard_staging_grace: CLIPBOARD_STAGING_GRACE,
        paste_wait: None,
        app_clipboard_wait: None,
        paste_stall: None,
        wake_wait: None,
        #[cfg(feature = "test-hooks")]
        lose_relative_motion: 0,
        #[cfg(feature = "test-hooks")]
        omit_crossing_spots: false,
        #[cfg(feature = "test-hooks")]
        wake_info: None,
        #[cfg(feature = "test-hooks")]
        wake_target: None,
    };

    // AppKit asks for clipboard content promised to other apps through the
    // main thread's run loop, so on macOS the
    // runtime runs beside it.
    let main_loop = cfg!(target_os = "macos") && matches!(config.backend, BackendConfig::Native);
    let runtime = runtime()?;
    let restart = beside_main_loop(main_loop, move || {
        runtime.block_on(async move {
            let agent = agent::start(config, Arc::new(identity), backends, options)
                .await
                .map_err(|e| e.to_string())?;
            tracing::info!(
                device = %agent.device_id(),
                addr = %agent.local_addr(),
                fingerprint = %agent.fingerprint(),
                "agent running"
            );
            let restart = tokio::select! {
                () = stop_requested() => false,
                () = agent.permission_granted(), if restart_on_grant => true,
            };
            if restart {
                tracing::info!("restarting for the permission just granted");
            } else {
                tracing::info!("shutting down");
            }
            agent.shutdown().await;
            Ok::<_, String>(restart)
        })
    })?;
    if restart {
        return Err(restart_self());
    }
    Ok(())
}

/// Runs `work`, with the main thread running its run loop meanwhile when
/// `main_loop` is set on macOS.
fn beside_main_loop<T: Send + 'static>(
    main_loop: bool,
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    #[cfg(target_os = "macos")]
    if main_loop {
        return iw_platform::macos::run_beside_main_loop(work);
    }
    let _ = main_loop;
    work()
}

/// Runs the Windows broker, which reads the desktop pictures and the
/// clipboard, writes received files, reads the files the user sends, keeps
/// the transfer history and starts the app, as the user, with the
/// platform's readers.
#[cfg(windows)]
fn run_broker() -> Result<(), String> {
    use iw_winsvc::broker::{self, Services};

    let read: broker::ReadPictures = match iw_platform::windows::WallpaperPreviews::new() {
        Ok(previews) => Arc::new(move || previews.read().map_err(|e| e.to_string())),
        Err(e) => {
            let reason = e.to_string();
            Arc::new(move || Err(reason.clone()))
        }
    };
    let clipboard = broker_clipboard()
        .inspect_err(|e| tracing::warn!(error = %e, "cannot reach the clipboard; it is not shared"))
        .ok();
    let receive_folder = broker::default_receive_folder()
        .inspect_err(|e| {
            tracing::warn!(error = %e, "cannot find the Downloads folder; files are not received")
        })
        .ok();
    let history_file = broker::default_history_file()
        .inspect_err(|e| {
            tracing::warn!(
                error = %e,
                "cannot find the local application data folder; the transfer history is not kept"
            )
        })
        .ok();
    broker::run(Services {
        read_pictures: read,
        clipboard,
        receive_folder,
        history_file,
        launch_app: Arc::new(broker::launch_installed_app),
    })
    .map_err(|e| e.to_string())
}

/// The user's clipboard, for the broker, with the changes other apps make.
#[cfg(windows)]
fn broker_clipboard() -> Result<iw_winsvc::broker::ClipboardAccess, String> {
    let (changes, watch) = tokio::sync::watch::channel(None);
    let clipboard = iw_platform::windows::WinClipboard::start(Box::new(move |sequence| {
        changes.send_replace(Some(sequence));
    }))
    .map_err(|e| e.to_string())?;
    Ok(iw_winsvc::broker::ClipboardAccess {
        clipboard: Arc::new(BrokerClipboard(std::sync::Mutex::new(clipboard))),
        changes: watch,
    })
}

/// What reading the platform's clipboard found, by MIME type.
#[cfg(windows)]
fn by_mime(read: iw_platform::ClipboardRead) -> iw_winsvc::broker::ClipboardRead {
    use iw_platform::ClipboardRead;
    use iw_winsvc::broker::ClipboardRead as Read;

    match read {
        ClipboardRead::Content(content) => Read::Content(
            content
                .items
                .into_iter()
                .map(|(format, data)| (format.mime().to_owned(), data))
                .collect(),
        ),
        ClipboardRead::Empty => Read::Empty,
        ClipboardRead::Sensitive => Read::Sensitive,
        ClipboardRead::TooLarge(size) => Read::TooLarge(size as u64),
    }
}

/// The platform's clipboard as the broker uses it, by MIME type.
#[cfg(windows)]
struct BrokerClipboard(std::sync::Mutex<iw_platform::windows::WinClipboard>);

#[cfg(windows)]
impl iw_winsvc::broker::UserClipboard for BrokerClipboard {
    fn read(&self, max_len: usize) -> Result<iw_winsvc::broker::ClipboardRead, String> {
        use iw_platform::Clipboard;

        let clipboard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        Ok(by_mime(clipboard.read(max_len).map_err(|e| e.to_string())?))
    }

    fn read_with_files(
        &self,
        max_len: usize,
    ) -> Result<(iw_winsvc::broker::ClipboardRead, Vec<String>), String> {
        let clipboard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let (read, files) = clipboard
            .read_with_files(max_len)
            .map_err(|e| e.to_string())?;
        Ok((by_mime(read), files))
    }

    fn write(&self, parts: &[(String, Vec<u8>)]) -> Result<(), String> {
        use iw_platform::{Clipboard, ClipboardContent, ClipboardFormat};

        let content = ClipboardContent {
            items: parts
                .iter()
                .filter_map(|(mime, data)| Some((ClipboardFormat::from_mime(mime)?, data.clone())))
                .collect(),
        };
        let mut clipboard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        clipboard
            .write(&content)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn promise(
        &self,
        mimes: &[String],
        size: u64,
        content: Arc<dyn iw_winsvc::broker::PromisedContent>,
    ) -> Result<u64, String> {
        use iw_platform::{Clipboard, ClipboardFormat, ClipboardPromise};

        let promise = ClipboardPromise {
            formats: mimes
                .iter()
                .filter_map(|mime| ClipboardFormat::from_mime(mime))
                .collect(),
            bytes: size,
            files: Vec::new(),
            source: Arc::new(AgentContent(content)),
        };
        let mut clipboard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        clipboard.write_promise(promise).map_err(|e| e.to_string())
    }

    fn withdraw(&self, token: u64) -> Result<(), String> {
        let clipboard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        clipboard
            .withdraw(token)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// What the agent has of promised content, as the clipboard asks for it.
#[cfg(windows)]
struct AgentContent(Arc<dyn iw_winsvc::broker::PromisedContent>);

#[cfg(windows)]
impl iw_platform::ClipboardSource for AgentContent {
    fn fetch(&self, format: iw_platform::ClipboardFormat) -> Option<Vec<u8>> {
        match self.try_fetch(format) {
            iw_platform::Fetch::Content(data) => Some(data),
            _ => None,
        }
    }

    fn try_fetch(&self, format: iw_platform::ClipboardFormat) -> iw_platform::Fetch {
        use iw_platform::Fetch;
        use iw_winsvc::broker::Render;

        match self.0.render(format.mime()) {
            Render::Content(data) => Fetch::Content(data),
            Render::Busy => Fetch::Busy,
            Render::Unavailable => Fetch::Unavailable,
        }
    }

    fn finished(&self) {
        self.0.gone();
    }
}

/// Runs this executable again in place, with the same arguments and
/// process id, so whatever supervises the agent keeps doing so. Returns
/// only if that fails.
fn restart_self() -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        match std::env::current_exe() {
            Ok(exe) => {
                let error = std::process::Command::new(exe)
                    .args(std::env::args_os().skip(1))
                    .exec();
                format!("cannot restart: {error}")
            }
            Err(e) => format!("cannot restart: {e}"),
        }
    }
    #[cfg(not(unix))]
    {
        "cannot restart in place on this platform".to_owned()
    }
}

/// macOS answers a process's permission checks once and keeps the answer,
/// so the agent would never see the user grant one. While one is missing,
/// a new process (`inputweave permissions`) is asked instead
/// (`NativeOptions::fresh_permissions`). It runs as the agent's child, so
/// macOS checks the agent's permissions.
#[cfg(target_os = "macos")]
fn fresh_permissions() -> Option<Vec<iw_platform::PermissionState>> {
    std::env::current_exe()
        .and_then(|exe| std::process::Command::new(exe).arg("permissions").output())
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| permission_lines::parse(&String::from_utf8_lossy(&output.stdout)))
}

fn print_permissions() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    for state in iw_platform::macos::MacStatus::kept_permissions() {
        println!("{}", permission_lines::format(state));
    }
    Ok(())
}

/// The lines `inputweave permissions` prints, "accessibility granted".
#[cfg(any(target_os = "macos", test))]
mod permission_lines {
    use iw_platform::{Permission, PermissionState};

    pub(crate) fn format(state: PermissionState) -> String {
        let granted = if state.granted { "granted" } else { "missing" };
        format!("{} {granted}", state.permission.key())
    }

    pub(crate) fn parse(text: &str) -> Option<Vec<PermissionState>> {
        text.lines()
            .map(|line| {
                let (name, state) = line.split_once(' ')?;
                let permission = Permission::from_key(name)?;
                let granted = match state {
                    "granted" => true,
                    "missing" => false,
                    _ => return None,
                };
                Some(PermissionState::new(permission, granted))
            })
            .collect()
    }
}

/// Waits for Ctrl-C, or on Unix for SIGTERM, which launchd sends when it
/// stops the agent: on logout, a restart, or when the app unregisters it.
/// Shutting down then lets peers release keys and the cursor at once rather
/// than after the heartbeat times out.
async fn stop_requested() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(any(unix, windows))]
fn ctl(socket: Option<PathBuf>, command: CtlCommand) -> Result<(), String> {
    use inputweave::ctl;

    let socket = socket
        .or_else(paths::default_ipc_socket)
        .ok_or_else(|| "cannot determine the agent's socket; pass --socket".to_owned())?;
    runtime()?.block_on(async move {
        let fail = |e: ctl::CtlError| e.to_string();
        match command {
            CtlCommand::Status => {
                let (agent, status) = ctl::status(&socket).await.map_err(fail)?;
                print!("{}", ctl::format_agent(&agent));
                print!("{}", ctl::format_status(&status));
            }
            CtlCommand::Nearby => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                print!(
                    "{}",
                    ctl::format_nearby(&client.nearby().await.map_err(fail)?)
                );
            }
            CtlCommand::Pair { target } => {
                let state = ctl::pair(&socket, &target).await.map_err(fail)?;
                println!(
                    "Paired with {} ({}).",
                    state.peer_name, state.peer_device_id
                );
            }
            CtlCommand::Cancel => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                client.cancel_pairing().await.map_err(fail)?;
            }
            CtlCommand::Unpair { device } => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                client.unpair(&device).await.map_err(fail)?;
                println!("Unpaired {device}.");
            }
            CtlCommand::Send { device, files } => {
                let paths = files
                    .iter()
                    .map(|f| std::path::absolute(f).map(|p| p.display().to_string()))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?;
                ctl::send(&socket, &device, paths, |line| println!("{line}"))
                    .await
                    .map_err(fail)?;
            }
            CtlCommand::Transfers { clear, remove } => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                let transfers = match remove {
                    Some(id) => client.remove_transfer(&id).await,
                    None if clear => client.clear_transfers().await,
                    None => client.transfers().await,
                };
                print!("{}", ctl::format_transfers(&transfers.map_err(fail)?));
            }
            CtlCommand::Diagnose { output } => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                let report = client.diagnostics().await.map_err(fail)?;
                match output {
                    Some(path) => {
                        std::fs::write(&path, report)
                            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                        println!("Wrote {}.", path.display());
                    }
                    None => print!("{report}"),
                }
            }
            CtlCommand::Layout => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                print!(
                    "{}",
                    ctl::format_arrangement(&client.arrangement().await.map_err(fail)?)
                );
            }
            CtlCommand::Move { device, x, y } => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                let arrangement = client.move_device(&device, x, y).await.map_err(fail)?;
                print!("{}", ctl::format_arrangement(&arrangement));
            }
            CtlCommand::Watch => {
                ctl::watch(&socket, |text| println!("{}", text.trim_end()))
                    .await
                    .map_err(fail)?;
            }
            CtlCommand::Lock { state } => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                let locked = matches!(state, OnOff::On);
                let status = client.set_locked(locked).await.map_err(fail)?;
                print!("{}", ctl::format_status(&status));
            }
            CtlCommand::Relative { state, auto } => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                let status = match (state, auto) {
                    (_, Some(auto)) => client.set_relative_auto(matches!(auto, OnOff::On)).await,
                    (Some(state), None) => {
                        client.set_relative_mouse(matches!(state, OnOff::On)).await
                    }
                    (None, None) => client.status().await,
                }
                .map_err(fail)?;
                println!("relative mouse: {}", ctl::format_relative(&status));
            }
            CtlCommand::Wake { device, push } => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                match (device, push) {
                    (_, Some(push)) => {
                        let status = client
                            .set_wake_on_push(matches!(push, OnOff::On))
                            .await
                            .map_err(fail)?;
                        let state = if status.wake_on_push { "on" } else { "off" };
                        println!("waking by pushing the cursor: {state}");
                    }
                    (Some(device), None) => {
                        client.wake(&device).await.map_err(fail)?;
                        println!(
                            "Sent {device} a wake-up. It is being woken for two minutes, or until \
                             it connects (`inputweave ctl status`)."
                        );
                    }
                    (None, None) => unreachable!("clap requires one"),
                }
            }
            CtlCommand::Jump { device } => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                let status = client.jump_to(&device).await.map_err(fail)?;
                print!("{}", ctl::format_status(&status));
            }
            CtlCommand::Dwell { ms } => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                let status = client.set_edge_dwell(ms).await.map_err(fail)?;
                print!("{}", ctl::format_status(&status));
            }
            CtlCommand::Identify => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                client.identify_screens().await.map_err(fail)?;
                println!("Identifying the screens of this device and every connected one.");
            }
            CtlCommand::Folders => {
                let status = ctl::ask_for_folders(&socket, FOLDERS_WAIT, || {
                    println!("Choose Allow when macOS asks about each folder.");
                })
                .await
                .map_err(fail)?;
                println!("{}", ctl::format_folder_access(&status));
                if ctl::asking_for_folders(&status) {
                    println!(
                        "Some folders are still being asked about; `inputweave ctl status` shows \
                         what comes of them."
                    );
                }
            }
            CtlCommand::Permission { name } => {
                let Some(permission) = ctl::permission_named(&name) else {
                    let names: Vec<_> = ctl::ASKED_PERMISSIONS.iter().map(|p| p.key()).collect();
                    return Err(format!(
                        "no permission called {name:?}; it is one of {}",
                        names.join(", ")
                    ));
                };
                let status = ctl::ask_for_permission(&socket, permission, FOLDERS_WAIT, || {
                    println!("Choose Allow (or Share) when the desktop asks.");
                })
                .await
                .map_err(fail)?;
                for p in status
                    .permissions
                    .iter()
                    .filter(|p| p.permission() == permission)
                {
                    println!("{}", ctl::format_permission(p));
                }
            }
            CtlCommand::Rename {
                device,
                name,
                clear: _,
            } => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                let name = name.unwrap_or_default();
                let status = client.rename_device(&device, &name).await.map_err(fail)?;
                print!("{}", ctl::format_status(&status));
            }
            CtlCommand::Device {
                device,
                clipboard,
                files,
                speed,
            } => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                let mut status = None;
                if let Some(percent) = speed {
                    status = Some(
                        client
                            .set_device_speed(&device, percent)
                            .await
                            .map_err(fail)?,
                    );
                }
                // Whatever goes wrong after the speed was set says so.
                let done = |e: String| match speed {
                    Some(percent) => format!("cursor speed set to {percent}%, but {e}"),
                    None => e,
                };
                if clipboard.is_some() || files.is_some() {
                    let now = match status.take() {
                        Some(status) => status,
                        None => client.status().await.map_err(fail)?,
                    };
                    let current = now
                        .devices
                        .iter()
                        .find(|d| d.id == device)
                        .ok_or_else(|| done(format!("no trusted device has id {device:?}")))?;
                    let off = |choice: Option<OnOff>, now: bool| match choice {
                        Some(OnOff::On) => false,
                        Some(OnOff::Off) => true,
                        None => now,
                    };
                    let clipboard_off = off(clipboard, current.clipboard_off);
                    let files_off = off(files, current.files_off);
                    status = Some(
                        client
                            .set_device_sharing(&device, clipboard_off, files_off)
                            .await
                            .map_err(|e| done(fail(e)))?,
                    );
                }
                let status = match status {
                    Some(status) => status,
                    None => client.status().await.map_err(fail)?,
                };
                print!("{}", ctl::format_status(&status));
            }
            CtlCommand::Edge { modifier } => {
                use iw_proto::app::v1::SwitchModifier;
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                let status = match modifier {
                    Some(modifier) => {
                        let modifier = match modifier {
                            SwitchModifierArg::None => SwitchModifier::None,
                            SwitchModifierArg::Control => SwitchModifier::Control,
                            SwitchModifierArg::Alt => SwitchModifier::Alt,
                            SwitchModifierArg::Shift => SwitchModifier::Shift,
                            SwitchModifierArg::Gui => SwitchModifier::Gui,
                        };
                        client.set_switch_modifier(modifier).await
                    }
                    None => client.status().await,
                }
                .map_err(fail)?;
                println!("edge dwell:     {} ms", status.edge_dwell_ms);
                println!(
                    "modifier:       {}",
                    ctl::switch_modifier_name(status.switch_modifier())
                );
            }
            CtlCommand::Hotkeys { action } => {
                let status = match action {
                    Some(HotkeysAction::Set {
                        take_back,
                        take_back_double,
                        lock,
                        jump_modifiers,
                        relative,
                    }) => {
                        ctl::set_hotkeys(
                            &socket,
                            take_back,
                            lock,
                            jump_modifiers,
                            relative,
                            take_back_double,
                        )
                        .await
                    }
                    Some(HotkeysAction::Reset) => ctl::reset_hotkeys(&socket).await,
                    None => ctl::status(&socket).await.map(|(_, status)| status),
                }
                .map_err(fail)?;
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                let arrangement = client.arrangement().await.map_err(fail)?;
                print!("{}", ctl::format_hotkeys(&status, &arrangement));
            }
            CtlCommand::LoginWindow { state } => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                let on = matches!(state, OnOff::On);
                client.set_login_window_access(on).await.map_err(fail)?;
                if on {
                    println!(
                        "Other devices can control this Mac at the login window, from the next \
                         time it shows."
                    );
                } else {
                    println!("Other devices can no longer control this Mac at the login window.");
                }
            }
            CtlCommand::License { action } => {
                let mut client = ctl::Client::connect(&socket).await.map_err(fail)?;
                let status = match action {
                    Some(LicenseAction::Set) => {
                        let key = ctl::read_license_key().await.map_err(fail)?;
                        client.set_license_key(&key).await
                    }
                    Some(LicenseAction::Remove) => client.remove_license().await,
                    None => client.status().await,
                }
                .map_err(fail)?;
                print!("{}", ctl::format_license(&status));
            }
        }
        Ok(())
    })
}

#[cfg(not(any(unix, windows)))]
fn ctl(_socket: Option<PathBuf>, _command: CtlCommand) -> Result<(), String> {
    Err("ctl is not implemented on this platform".to_owned())
}

/// The Windows service has no console; it logs to
/// `%ProgramData%\InputWeave\logs\service.log`.
#[cfg(windows)]
fn run_service() -> Result<(), String> {
    match iw_winsvc::open_log("service") {
        Ok(file) => log_to(std::sync::Mutex::new(file)),
        Err(e) => eprintln!("inputweave: cannot open the service log: {e}"),
    }
    iw_winsvc::service::run().map_err(|e| e.to_string())
}

fn print_identity(config: Option<PathBuf>) -> Result<(), String> {
    let path = config_path(config)?;
    let config = if path.exists() {
        Some(Config::load(&path).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let identity = Identity::load_or_generate(&identity_dir(&path, config.as_ref()))
        .map_err(|e| format!("cannot load the device identity: {e}"))?;
    println!("{}", identity.fingerprint());
    Ok(())
}

/// Mock emulation for headless runs: logs what would be injected.
struct LoggingEmulation;

impl InputEmulation for LoggingEmulation {
    fn move_to(&mut self, position: Point) -> Result<(), PlatformError> {
        tracing::debug!(x = position.x, y = position.y, "inject: move");
        Ok(())
    }

    fn move_by(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError> {
        tracing::debug!(dx, dy, "inject: move by");
        Ok(())
    }

    fn button(&mut self, button: Button, pressed: bool) -> Result<(), PlatformError> {
        tracing::info!(?button, pressed, "inject: button");
        Ok(())
    }

    fn key(&mut self, usage: HidUsage, pressed: bool) -> Result<(), PlatformError> {
        tracing::info!(?usage, pressed, "inject: key");
        Ok(())
    }

    fn scroll(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError> {
        tracing::debug!(dx, dy, "inject: scroll");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use iw_platform::{Permission, PermissionState};

    use super::permission_lines;

    #[test]
    fn permissions_read_back_as_printed() {
        let states = vec![
            PermissionState::new(Permission::Accessibility, true),
            PermissionState::new(Permission::InputMonitoring, false),
        ];
        let text: String = states
            .iter()
            .map(|s| permission_lines::format(s.clone()) + "\n")
            .collect();
        assert_eq!(text, "accessibility granted\ninput-monitoring missing\n");
        assert_eq!(permission_lines::parse(&text), Some(states));
        assert_eq!(permission_lines::parse("accessibility maybe\n"), None);
    }
}
