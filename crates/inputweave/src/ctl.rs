//! `inputweave ctl`: a command-line IPC client of the local agent.

use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iw_ipc::{FrameError, IpcConnection};
use iw_proto::IPC_VERSION;
use iw_proto::app::v1::{
    self as app, AgentMessage, CancelPairing, ClearTransfers, ClientHello, ClientMessage,
    DevicePosition, GetArrangement, GetNearby, GetPairing, GetStatus, GetTransfers,
    IdentifyScreens, JumpTo, LicenseState, PairingPhase, PairingState, RemoveLicense,
    RemoveTransfer, RenameDevice, RequestFolderAccess, RequestPermission, Response, SendFiles,
    SetArrangement, SetDeviceSharing, SetDeviceSpeed, SetEdgeOptions, SetHotkeys, SetLicenseKey,
    SetLocked, SetLoginWindowAccess, SetOfflineForNow, SetRelativeAuto, SetRelativeMouse,
    SetSwitchModifier, SetWakeOnPush, StartPairing, SubmitPairingCode, SwitchModifier,
    TransferState, Unpair, WakeDevice, WakeState, agent_message, client_message::Command, event,
    response, start_pairing,
};

use crate::agent::{Access, UserFolder};
use crate::offline_file::describe_setter;

#[derive(Debug, thiserror::Error)]
pub enum CtlError {
    #[error("cannot reach the agent at {path}: {source}{hint}")]
    Connect {
        path: String,
        source: std::io::Error,
        /// More to say about `source`, with its own leading separator, or
        /// empty.
        hint: &'static str,
    },
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("the agent refused the request ({code}): {detail}")]
    Agent { code: String, detail: String },
    #[error("the agent closed the connection")]
    Closed,
    #[error("unexpected reply from the agent")]
    Unexpected,
    #[error("pairing failed: {0}")]
    Pairing(String),
    #[error("cannot read the code: {0}")]
    Input(std::io::Error),
    #[error("not every file arrived")]
    Transfer,
    /// The agent is from a version that does not know what was asked.
    #[error("the running agent does not know {0}: it is from an older version")]
    Unsupported(&'static str),
}

/// What to add when the agent's socket cannot be opened. On Windows each
/// session has its own pipe, and the service starts the agent only in the
/// session attached to the console, so a pipe that does not exist usually
/// means `ctl` runs elsewhere: over ssh, or in a remote session.
fn connect_hint(kind: std::io::ErrorKind, windows: bool) -> &'static str {
    if windows && kind == std::io::ErrorKind::NotFound {
        ". The InputWeave service runs the agent in the session at this computer's \
         console; run ctl there rather than over ssh or in a remote session, and check \
         that the service is running (sc query InputWeave)"
    } else {
        ""
    }
}

/// A connection to the agent that has said Hello.
pub struct Client {
    conn: IpcConnection,
    next_id: u64,
    /// Events that arrived while waiting for a response.
    events: VecDeque<event::Event>,
    /// What the agent said about itself in answer to Hello.
    agent: app::AgentHello,
}

impl Client {
    pub async fn connect(socket: &Path) -> Result<Self, CtlError> {
        let conn = IpcConnection::connect(socket)
            .await
            .map_err(|source| CtlError::Connect {
                path: socket.display().to_string(),
                hint: connect_hint(source.kind(), cfg!(windows)),
                source,
            })?;
        let mut client = Self {
            conn,
            next_id: 1,
            events: VecDeque::new(),
            agent: app::AgentHello::default(),
        };
        let hello = Command::Hello(ClientHello {
            ipc_version: IPC_VERSION,
            client: concat!("ctl/", env!("CARGO_PKG_VERSION")).to_owned(),
            ..ClientHello::default()
        });
        match client.request(hello).await? {
            response::Result::Hello(agent) => {
                client.agent = agent;
                Ok(client)
            }
            _ => Err(CtlError::Unexpected),
        }
    }

    /// Sends a command and waits for its response. Events that arrive in
    /// the meantime are kept for [`Client::next_event`].
    pub async fn request(&mut self, command: Command) -> Result<response::Result, CtlError> {
        let id = self.next_id;
        self.next_id += 1;
        self.conn
            .send(&ClientMessage {
                request_id: id,
                command: Some(command),
            })
            .await?;
        loop {
            let msg = self
                .conn
                .recv::<AgentMessage>()
                .await?
                .ok_or(CtlError::Closed)?;
            match msg.msg {
                Some(agent_message::Msg::Response(Response { request_id, result }))
                    if request_id == id =>
                {
                    return match result.ok_or(CtlError::Unexpected)? {
                        response::Result::Error(e) => Err(CtlError::Agent {
                            code: e.code,
                            detail: e.detail,
                        }),
                        other => Ok(other),
                    };
                }
                Some(agent_message::Msg::Event(app::Event { event: Some(e), .. })) => {
                    self.events.push_back(e);
                }
                _ => {}
            }
        }
    }

    pub async fn next_event(&mut self) -> Result<event::Event, CtlError> {
        if let Some(e) = self.events.pop_front() {
            return Ok(e);
        }
        loop {
            let msg = self
                .conn
                .recv::<AgentMessage>()
                .await?
                .ok_or(CtlError::Closed)?;
            if let Some(agent_message::Msg::Event(app::Event { event: Some(e), .. })) = msg.msg {
                return Ok(e);
            }
        }
    }

    pub async fn status(&mut self) -> Result<app::Status, CtlError> {
        self.status_request(Command::GetStatus(GetStatus {})).await
    }

    /// Locks the cursor to the device it is on, or unlocks it.
    pub async fn set_locked(&mut self, locked: bool) -> Result<app::Status, CtlError> {
        self.status_request(Command::SetLocked(SetLocked { locked }))
            .await
    }

    /// Turns relative mode on or off for the other device the cursor is
    /// on.
    pub async fn set_relative_mouse(&mut self, on: bool) -> Result<app::Status, CtlError> {
        self.status_request(Command::SetRelativeMouse(SetRelativeMouse { on }))
            .await
    }

    /// Has devices enter relative mode by themselves when an app there
    /// holds the pointer, or not.
    pub async fn set_relative_auto(&mut self, on: bool) -> Result<app::Status, CtlError> {
        self.status_request(Command::SetRelativeAuto(SetRelativeAuto { on }))
            .await
    }

    /// Sends a wake-up to an offline device that told this one how to wake
    /// it. Answered once it is sent.
    pub async fn wake(&mut self, device_id: &str) -> Result<app::Status, CtlError> {
        let wake = Command::WakeDevice(WakeDevice {
            device_id: device_id.to_owned(),
        });
        self.status_request(wake).await
    }

    /// Takes a device, this one or another, offline for now, or brings it
    /// back.
    pub async fn set_offline_for_now(
        &mut self,
        device_id: &str,
        offline: bool,
    ) -> Result<app::Status, CtlError> {
        let set = Command::SetOfflineForNow(SetOfflineForNow {
            device_id: device_id.to_owned(),
            offline,
        });
        self.status_request(set).await
    }

    /// Changes the profiles, or which is used: `command` is one of the
    /// profile commands (`AddProfile` and those after it).
    pub async fn change_profiles(&mut self, command: Command) -> Result<app::Status, CtlError> {
        self.status_request(command).await
    }

    /// Has pushing the cursor toward an offline device wake it, or not.
    pub async fn set_wake_on_push(&mut self, on: bool) -> Result<app::Status, CtlError> {
        self.status_request(Command::SetWakeOnPush(SetWakeOnPush { on }))
            .await
    }

    /// Has the agent list the folders macOS protects, so macOS asks the
    /// user about each it has not been told of. Answered at once; what came
    /// of each is in later statuses.
    pub async fn request_folder_access(&mut self) -> Result<app::Status, CtlError> {
        self.status_request(Command::RequestFolderAccess(RequestFolderAccess {}))
            .await
    }

    /// Has the desktop ask the user for `permission`, one the agent asks
    /// for itself (Wayland's portals). Answered once it is being asked
    /// about; what the user said is in later statuses.
    pub async fn request_permission(
        &mut self,
        permission: app::Permission,
    ) -> Result<app::Status, CtlError> {
        self.status_request(Command::RequestPermission(RequestPermission {
            permission: permission.into(),
        }))
        .await
    }

    /// Lets other devices control this Mac at the login window, or stops
    /// them.
    pub async fn set_login_window_access(&mut self, on: bool) -> Result<app::Status, CtlError> {
        self.status_request(Command::SetLoginWindowAccess(SetLoginWindowAccess { on }))
            .await
    }

    /// Enters a license key on this computer, for the app to activate with
    /// Polar.
    pub async fn set_license_key(&mut self, key: &str) -> Result<app::Status, CtlError> {
        let set = Command::SetLicenseKey(SetLicenseKey {
            key: key.to_owned(),
        });
        self.status_request(set).await
    }

    /// Removes the license from this computer, whatever it is; it is
    /// licensed again only by entering a key here, or by another computer
    /// applying its license here.
    pub async fn remove_license(&mut self) -> Result<app::Status, CtlError> {
        self.status_request(Command::RemoveLicense(RemoveLicense::default()))
            .await
    }

    /// Moves the cursor to the middle of a device's primary display.
    pub async fn jump_to(&mut self, device_id: &str) -> Result<app::Status, CtlError> {
        let jump = Command::JumpTo(JumpTo {
            device_id: device_id.to_owned(),
        });
        self.status_request(jump).await
    }

    /// Sets how long the cursor must push against an edge before it crosses.
    pub async fn set_edge_dwell(&mut self, dwell_ms: u32) -> Result<app::Status, CtlError> {
        self.status_request(Command::SetEdgeOptions(SetEdgeOptions { dwell_ms }))
            .await
    }

    /// Has this device and every connected one show their names on their
    /// screens for a moment.
    pub async fn identify_screens(&mut self) -> Result<app::Status, CtlError> {
        self.status_request(Command::IdentifyScreens(IdentifyScreens {}))
            .await
    }

    /// Names another device on this device only; an empty name shows its
    /// own name again.
    pub async fn rename_device(
        &mut self,
        device_id: &str,
        name: &str,
    ) -> Result<app::Status, CtlError> {
        let rename = Command::RenameDevice(RenameDevice {
            device_id: device_id.to_owned(),
            name: name.to_owned(),
        });
        self.status_request(rename).await
    }

    /// Stops sharing the clipboard or files with a device, or shares them
    /// again.
    pub async fn set_device_sharing(
        &mut self,
        device_id: &str,
        clipboard_off: bool,
        files_off: bool,
    ) -> Result<app::Status, CtlError> {
        let set = Command::SetDeviceSharing(SetDeviceSharing {
            device_id: device_id.to_owned(),
            clipboard_off,
            files_off,
        });
        self.status_request(set).await
    }

    /// Makes the cursor move faster or slower on another device, in
    /// percent of how the mouse moves it here.
    pub async fn set_device_speed(
        &mut self,
        device_id: &str,
        percent: u32,
    ) -> Result<app::Status, CtlError> {
        let set = Command::SetDeviceSpeed(SetDeviceSpeed {
            device_id: device_id.to_owned(),
            percent,
        });
        self.status_request(set).await
    }

    /// Makes the cursor cross edges only while `modifier` is held; `None`
    /// crosses whenever it reaches one.
    pub async fn set_switch_modifier(
        &mut self,
        modifier: SwitchModifier,
    ) -> Result<app::Status, CtlError> {
        let set = Command::SetSwitchModifier(SetSwitchModifier {
            modifier: modifier.into(),
        });
        self.status_request(set).await
    }

    /// Sets every hotkey at once.
    pub async fn set_hotkeys(&mut self, hotkeys: app::Hotkeys) -> Result<app::Status, CtlError> {
        let set = Command::SetHotkeys(SetHotkeys {
            hotkeys: Some(hotkeys),
        });
        self.status_request(set).await
    }

    async fn status_request(&mut self, command: Command) -> Result<app::Status, CtlError> {
        match self.request(command).await? {
            response::Result::Status(status) => Ok(status),
            _ => Err(CtlError::Unexpected),
        }
    }

    pub async fn nearby(&mut self) -> Result<app::NearbyDevices, CtlError> {
        match self.request(Command::GetNearby(GetNearby {})).await? {
            response::Result::Nearby(nearby) => Ok(nearby),
            _ => Err(CtlError::Unexpected),
        }
    }

    pub async fn unpair(&mut self, device_id: &str) -> Result<app::Status, CtlError> {
        let unpair = Command::Unpair(Unpair {
            device_id: device_id.to_owned(),
        });
        match self.request(unpair).await? {
            response::Result::Status(status) => Ok(status),
            _ => Err(CtlError::Unexpected),
        }
    }

    pub async fn arrangement(&mut self) -> Result<app::Arrangement, CtlError> {
        match self
            .request(Command::GetArrangement(GetArrangement {}))
            .await?
        {
            response::Result::Arrangement(a) => Ok(a),
            _ => Err(CtlError::Unexpected),
        }
    }

    /// Moves one device's desktop origin to (`x`, `y`) on the canvas.
    pub async fn move_device(
        &mut self,
        device_id: &str,
        x: f64,
        y: f64,
    ) -> Result<app::Arrangement, CtlError> {
        let set = Command::SetArrangement(SetArrangement {
            positions: vec![DevicePosition {
                device_id: device_id.to_owned(),
                x,
                y,
            }],
        });
        match self.request(set).await? {
            response::Result::Arrangement(a) => Ok(a),
            _ => Err(CtlError::Unexpected),
        }
    }

    pub async fn transfers(&mut self) -> Result<app::Transfers, CtlError> {
        self.transfers_request(Command::GetTransfers(GetTransfers {}))
            .await
    }

    /// Takes a finished transfer off the list; its file stays.
    pub async fn remove_transfer(&mut self, id: &str) -> Result<app::Transfers, CtlError> {
        let remove = Command::RemoveTransfer(RemoveTransfer {
            transfer_id: id.to_owned(),
        });
        self.transfers_request(remove).await
    }

    /// Takes every finished transfer off the list; their files stay.
    pub async fn clear_transfers(&mut self) -> Result<app::Transfers, CtlError> {
        self.transfers_request(Command::ClearTransfers(ClearTransfers {}))
            .await
    }

    async fn transfers_request(&mut self, command: Command) -> Result<app::Transfers, CtlError> {
        match self.request(command).await? {
            response::Result::Transfers(t) => Ok(t),
            _ => Err(CtlError::Unexpected),
        }
    }

    /// The agent's diagnostics report.
    pub async fn diagnostics(&mut self) -> Result<String, CtlError> {
        match self
            .request(Command::GetDiagnostics(Default::default()))
            .await?
        {
            response::Result::Diagnostics(d) => Ok(d.report),
            _ => Err(CtlError::Unexpected),
        }
    }

    pub async fn pairing(&mut self) -> Result<PairingState, CtlError> {
        self.pairing_request(Command::GetPairing(GetPairing {}))
            .await
    }

    pub async fn cancel_pairing(&mut self) -> Result<PairingState, CtlError> {
        self.pairing_request(Command::CancelPairing(CancelPairing {}))
            .await
    }

    async fn pairing_request(&mut self, command: Command) -> Result<PairingState, CtlError> {
        match self.request(command).await? {
            response::Result::Pairing(state) => Ok(state),
            _ => Err(CtlError::Unexpected),
        }
    }

    /// Waits for the pairing to move past `phase`.
    async fn pairing_after(&mut self, phase: PairingPhase) -> Result<PairingState, CtlError> {
        loop {
            if let event::Event::PairingChanged(state) = self.next_event().await?
                && state.phase() != phase
            {
                return Ok(state);
            }
        }
    }
}

/// Sends files and folders to a device and waits until every one of them
/// ended, reporting through `out` how each went, what failed in folders,
/// and a summary. Fails if anything did not arrive.
pub async fn send(
    socket: &Path,
    device: &str,
    paths: Vec<String>,
    mut out: impl FnMut(&str),
) -> Result<(), CtlError> {
    let mut client = Client::connect(socket).await?;
    let before: Vec<String> = client
        .transfers()
        .await?
        .transfers
        .into_iter()
        .map(|t| t.transfer_id)
        .collect();
    let send = Command::SendFiles(SendFiles {
        device_id: device.to_owned(),
        paths,
    });
    let response::Result::Transfers(mut transfers) = client.request(send).await? else {
        return Err(CtlError::Unexpected);
    };
    let ours: Vec<String> = transfers
        .transfers
        .iter()
        .filter(|t| t.outgoing && !before.contains(&t.transfer_id))
        .map(|t| t.transfer_id.clone())
        .collect();
    loop {
        let mine: Vec<&app::FileTransfer> = transfers
            .transfers
            .iter()
            .filter(|t| ours.contains(&t.transfer_id))
            .collect();
        let finished = |t: &&app::FileTransfer| {
            matches!(
                t.state(),
                TransferState::Done
                    | TransferState::Declined
                    | TransferState::Failed
                    | TransferState::Cancelled
            )
        };
        if mine.iter().all(finished) {
            // In the order given: the list is newest first.
            let mut items = mine.clone();
            items.sort_by_key(|t| t.batch_index);
            for t in &items {
                out(&format_transfer(t));
                for failure in format_failures(t) {
                    out(&failure);
                }
            }
            let summary = Summary::of(items.iter().copied());
            out(&summary.line());
            return if summary.complete() {
                Ok(())
            } else {
                Err(CtlError::Transfer)
            };
        }
        if let event::Event::TransfersChanged(t) = client.next_event().await? {
            transfers = t;
        }
    }
}

/// The agent's answer to Hello, and its status.
pub async fn status(socket: &Path) -> Result<(app::AgentHello, app::Status), CtlError> {
    let mut client = Client::connect(socket).await?;
    let status = client.status().await?;
    Ok((client.agent, status))
}

/// Pairs with `target`, a nearby device's id or an address. The code that
/// device shows is read from standard input, as often as it allows.
pub async fn pair(socket: &Path, target: &str) -> Result<PairingState, CtlError> {
    let mut client = Client::connect(socket).await?;
    let nearby = client.nearby().await?;
    let target = if nearby.devices.iter().any(|d| d.id == target) {
        start_pairing::Target::DeviceId(target.to_owned())
    } else {
        start_pairing::Target::Address(target.to_owned())
    };
    let start = Command::StartPairing(StartPairing {
        target: Some(target),
    });
    let mut state = client.pairing_request(start).await?;
    if state.phase() == PairingPhase::Connecting {
        state = client.pairing_after(PairingPhase::Connecting).await?;
    }
    loop {
        match state.phase() {
            PairingPhase::EnterCode => {}
            PairingPhase::Paired => return Ok(state),
            _ => return Err(CtlError::Pairing(failure(&state))),
        }
        if state.error == "wrong_code" {
            eprintln!(
                "That code is not right; {} attempts left.",
                state.attempts_left
            );
        }
        let prompt = format!("Enter the pairing code shown on {}: ", state.peer_name);
        let typed = read_line(prompt).await?;
        let submit = Command::SubmitPairingCode(SubmitPairingCode { code: typed });
        state = match client.pairing_request(submit).await {
            Err(CtlError::Agent { code, .. }) if code == "invalid_code" => {
                eprintln!("A pairing code is six digits.");
                continue;
            }
            other => other?,
        };
    }
}

/// A license key, read from standard input rather than the command line,
/// where it would stay in the shell's history and show in the list of
/// processes: typed or pasted after a prompt at a terminal, which does not
/// show it, otherwise the first line given
/// (`echo KEY | inputweave ctl license set`).
pub async fn read_license_key() -> Result<String, CtlError> {
    use std::io::IsTerminal;
    let prompt = if std::io::stdin().is_terminal() {
        "License key (not shown): ".to_owned()
    } else {
        String::new()
    };
    read_input(prompt, true).await
}

async fn read_line(prompt: String) -> Result<String, CtlError> {
    read_input(prompt, false).await
}

async fn read_input(prompt: String, hidden: bool) -> Result<String, CtlError> {
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        eprint!("{prompt}");
        std::io::stderr().flush()?;
        let mut line = String::new();
        let read = if hidden {
            iw_platform::system::read_line_hidden(&mut line)?
        } else {
            std::io::stdin().read_line(&mut line)?
        };
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "no input",
            ));
        }
        Ok(line.trim().to_owned())
    })
    .await
    .map_err(|e| CtlError::Input(std::io::Error::other(e)))?
    .map_err(CtlError::Input)
}

/// Prints status and pairing changes as they happen, until the agent goes
/// away. On a device being paired with, this is where its code shows; when
/// the user shakes the mouse with the cursor here, where it is.
pub async fn watch(socket: &Path, mut out: impl FnMut(&str)) -> Result<(), CtlError> {
    let mut client = Client::connect(socket).await?;
    out(&format_status(&client.status().await?));
    if let Some(line) = format_pairing(&client.pairing().await?) {
        out(&line);
    }
    loop {
        match client.next_event().await? {
            event::Event::StatusChanged(status) => out(&format_status(&status)),
            event::Event::PairingChanged(state) => {
                if let Some(line) = format_pairing(&state) {
                    out(&line);
                }
            }
            event::Event::NearbyChanged(nearby) => out(&format_nearby(&nearby)),
            event::Event::ArrangementChanged(a) => out(&format_arrangement(&a)),
            event::Event::TransfersChanged(t) => out(&format_transfers(&t)),
            event::Event::CursorLocated(c) => out(&format_cursor_located(&c)),
        }
    }
}

fn failure(state: &PairingState) -> String {
    match state.error.as_str() {
        "too_many_attempts" => "too many wrong codes".to_owned(),
        "expired" => "the code expired".to_owned(),
        "rejected" => format!("{} declined", or_device(&state.peer_name)),
        "not_allowed" => "the other device does not accept new pairings".to_owned(),
        "busy" => "a pairing is already in progress on one of the devices".to_owned(),
        "id_conflict" => "both devices use the same device id".to_owned(),
        "incompatible" => "the two versions of InputWeave cannot work together".to_owned(),
        "unreachable" => "the other device cannot be reached".to_owned(),
        "cancelled" => "cancelled".to_owned(),
        "" => format!("unexpected state {:?}", state.phase()),
        other => other.to_owned(),
    }
}

fn or_device(name: &str) -> &str {
    if name.is_empty() {
        "the other device"
    } else {
        name
    }
}

/// Where the cursor was shown after the user shook the mouse.
pub fn format_cursor_located(located: &app::CursorLocated) -> String {
    format!("cursor located at ({:.0}, {:.0})", located.x, located.y)
}

/// Which agent answered: its version, and the SHA-256 of its executable,
/// which tells builds of the same version apart.
pub fn format_agent(agent: &app::AgentHello) -> String {
    let digest = if agent.agent_digest.is_empty() {
        "unknown"
    } else {
        &agent.agent_digest
    };
    format!(
        "agent:          inputweave {}\nagent sha-256:  {digest}\n",
        agent.agent_version
    )
}

/// Has the agent list the folders macOS protects, then follows its status
/// for at most `wait` while macOS asks the user about them, calling
/// `asking` once if it does. Returns the last status of that round: one
/// with folders still being asked about if the time ran out.
pub async fn ask_for_folders(
    socket: &Path,
    wait: Duration,
    asking: impl FnOnce(),
) -> Result<app::Status, CtlError> {
    let mut client = Client::connect(socket).await?;
    let mut status = client.request_folder_access().await?;
    let round = status.folder_access_round;
    if asking_for_folders(&status) {
        asking();
    }
    let deadline = tokio::time::Instant::now() + wait;
    while asking_for_folders(&status) {
        let Ok(event) = tokio::time::timeout_at(deadline, client.next_event()).await else {
            break;
        };
        // Statuses from before the reply may be of an earlier round.
        if let event::Event::StatusChanged(changed) = event?
            && changed.folder_access_round >= round
        {
            status = changed;
        }
    }
    Ok(status)
}

/// The permissions `ctl permission` asks for, by the name it takes.
pub const ASKED_PERMISSIONS: [iw_platform::Permission; 2] = [
    iw_platform::Permission::RemoteControl,
    iw_platform::Permission::InputCapture,
];

/// The permission `name` stands for, as `ctl permission` takes it.
pub fn permission_named(name: &str) -> Option<app::Permission> {
    iw_platform::Permission::from_key(name)
        .filter(|p| ASKED_PERMISSIONS.contains(p))
        .map(crate::convert::permission_kind_to_proto)
}

/// A permission as `ctl status` lists it, such as "remote control missing
/// (being asked)".
pub fn format_permission(p: &app::PermissionStatus) -> String {
    crate::convert::permission_state_from_proto(p).map_or_else(
        || {
            let state = if p.granted { "granted" } else { "missing" };
            format!("unknown permission {state}")
        },
        |s| s.describe(),
    )
}

/// Has the desktop ask the user for `permission` and waits, for at most
/// `wait`, for what the user says; `asking` is called if the desktop asks.
/// Returns the last status heard of the request.
pub async fn ask_for_permission(
    socket: &Path,
    permission: app::Permission,
    wait: Duration,
    asking: impl FnOnce(),
) -> Result<app::Status, CtlError> {
    let mut client = Client::connect(socket).await?;
    let mut status = client.request_permission(permission).await?;
    let of = |status: &app::Status| {
        status
            .permissions
            .iter()
            .find(|p| p.permission() == permission)
            .cloned()
    };
    // The request the reply answers; statuses from before it are older.
    let round = of(&status).map_or(0, |p| p.asked);
    let being_asked =
        |status: &app::Status| of(status).is_some_and(|p| p.ask() == app::PermissionAsk::Asking);
    if being_asked(&status) {
        asking();
    }
    let deadline = tokio::time::Instant::now() + wait;
    while being_asked(&status) {
        let Ok(event) = tokio::time::timeout_at(deadline, client.next_event()).await else {
            break;
        };
        if let event::Event::StatusChanged(changed) = event?
            && of(&changed).is_some_and(|p| p.asked >= round)
        {
            status = changed;
        }
    }
    Ok(status)
}

/// Some folder macOS protects is being listed, macOS perhaps asking the
/// user about it.
pub fn asking_for_folders(status: &app::Status) -> bool {
    status
        .folder_access
        .iter()
        .any(|f| f.state() == app::FolderAccessState::Asking)
}

/// The folders macOS protects and what it said of each, such as
/// "Downloads granted, Desktop denied, Documents not asked", named as the
/// agent names them.
pub fn format_folder_access(status: &app::Status) -> String {
    if !status.folder_access_asked
        && status
            .folder_access
            .iter()
            .all(|f| f.state() == app::FolderAccessState::Unknown)
    {
        return "not asked yet".to_owned();
    }
    status
        .folder_access
        .iter()
        .map(|f| {
            let folder =
                UserFolder::from_proto(f.folder()).map_or("unknown folder", UserFolder::name);
            format!("{folder} {}", Access::from_proto(f.state()).name())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether relative mode is on, for which device and why, and whether it
/// comes on by itself.
pub fn format_relative(status: &app::Status) -> String {
    let state = if status.relative_mouse.is_empty() {
        "off".to_owned()
    } else if status.relative_mouse_automatic {
        format!(
            "on for {}, as an app there holds the pointer",
            status.relative_mouse
        )
    } else {
        format!("on for {}", status.relative_mouse)
    };
    let auto = if status.relative_auto {
        ""
    } else {
        "; not by itself when an app holds the pointer"
    };
    format!("{state}{auto}")
}

/// Changes the hotkeys given, keeping the others; `jump_modifiers` empty
/// turns the jumps off, `relative` empty that hotkey, and
/// `take_back_double` empty the double press. Returns the status with the
/// hotkeys now in effect.
pub async fn set_hotkeys(
    socket: &Path,
    take_back: Option<String>,
    lock: Option<String>,
    jump_modifiers: Option<String>,
    relative: Option<String>,
    take_back_double: Option<String>,
) -> Result<app::Status, CtlError> {
    let mut client = Client::connect(socket).await?;
    let current = client.status().await?.hotkeys.unwrap_or_default();
    knows_double_press(&current, take_back_double.as_deref())?;
    let hotkeys = app::Hotkeys {
        take_back: take_back.unwrap_or(current.take_back),
        lock: lock.unwrap_or(current.lock),
        jump_modifiers: jump_modifiers.unwrap_or(current.jump_modifiers),
        // Not given, the agent keeps the ones in effect.
        relative,
        take_back_double,
    };
    client.set_hotkeys(hotkeys).await
}

/// Goes back to the default hotkeys, the double press off where the agent
/// knows it. Returns the status with the hotkeys now in effect.
pub async fn reset_hotkeys(socket: &Path) -> Result<app::Status, CtlError> {
    let mut client = Client::connect(socket).await?;
    let current = client.status().await?.hotkeys.unwrap_or_default();
    let defaults = crate::agent::hotkeys_to_proto(&iw_core::hotkey::Hotkeys::default());
    let hotkeys = app::Hotkeys {
        // An agent that does not know it has nothing to turn off.
        take_back_double: current
            .take_back_double
            .and(defaults.take_back_double.clone()),
        ..defaults
    };
    client.set_hotkeys(hotkeys).await
}

/// Refuses to change the double press with an agent that does not report
/// it (`current`, its hotkeys), which would leave it out without a word.
fn knows_double_press(current: &app::Hotkeys, asked: Option<&str>) -> Result<(), CtlError> {
    if asked.is_some() && current.take_back_double.is_none() {
        return Err(CtlError::Unsupported(
            "the double press (--take-back-double)",
        ));
    }
    Ok(())
}

/// The hotkeys, and which device each jump goes to.
pub fn format_hotkeys(status: &app::Status, arrangement: &app::Arrangement) -> String {
    let hotkeys = status.hotkeys.clone().unwrap_or_default();
    let mut out = String::new();
    out.push_str(&format!(
        "take back control:  {}
",
        hotkeys.take_back
    ));
    // Agents from before relative mode and the double press do not say.
    match hotkeys.take_back_double.as_deref() {
        None => {}
        Some("") => out.push_str("double press:       off\n"),
        Some(double) => out.push_str(&format!("double press:       {double}\n")),
    }
    out.push_str(&format!(
        "lock:               {}
",
        hotkeys.lock
    ));
    match hotkeys.relative.as_deref() {
        None => {}
        Some("") => out.push_str("relative mouse:     off\n"),
        Some(relative) => out.push_str(&format!("relative mouse:     {relative}\n")),
    }
    if hotkeys.jump_modifiers.is_empty() {
        out.push_str(
            "jump to a device:   off
",
        );
        return out;
    }
    out.push_str(&format!(
        "jump to a device:   {}+Digit1 to Digit9
",
        hotkeys.jump_modifiers
    ));
    let mut numbered: Vec<&app::ArrangedDevice> = arrangement
        .devices
        .iter()
        .filter(|d| d.jump_slot > 0)
        .collect();
    numbered.sort_by_key(|d| d.jump_slot);
    for d in numbered {
        let this = if d.this_device { " (this device)" } else { "" };
        out.push_str(&format!(
            "  {}+Digit{}  {} ({}){this}
",
            hotkeys.jump_modifiers, d.jump_slot, d.name, d.id
        ));
    }
    out
}

/// How a switch modifier is written on the command line.
pub fn switch_modifier_name(modifier: SwitchModifier) -> &'static str {
    match modifier {
        SwitchModifier::None => "none",
        SwitchModifier::Control => "control",
        SwitchModifier::Alt => "alt",
        SwitchModifier::Shift => "shift",
        SwitchModifier::Gui => "gui",
    }
}

/// The kind of session and its desktop, as `ctl status` and diagnostics
/// write it: "Wayland (ubuntu:GNOME)".
pub fn format_session(session: &app::SessionInfo) -> String {
    let kind = match session.r#type() {
        app::SessionType::X11 => "X11",
        app::SessionType::Wayland => "Wayland",
        app::SessionType::Unspecified => "unknown",
    };
    if session.desktop.is_empty() {
        kind.to_owned()
    } else {
        format!("{kind} ({})", session.desktop)
    }
}

/// What serves one part of the session: "clipboard: data-control", or
/// what keeps it from working.
pub fn format_backend(backend: &app::SessionBackend) -> String {
    let role = match backend.role() {
        app::BackendRole::Input => "input from other devices",
        app::BackendRole::Capture => "this device's keyboard and mouse",
        app::BackendRole::Clipboard => "clipboard",
        app::BackendRole::Displays => "displays",
        app::BackendRole::DesktopPictures => "desktop pictures",
        app::BackendRole::Unspecified => "other",
    };
    match (backend.implementation.as_str(), backend.problem.as_str()) {
        ("", "") => format!("{role}: none"),
        ("", problem) => format!("{role}: none ({problem})"),
        (implementation, "") => format!("{role}: {implementation}"),
        (implementation, problem) => format!("{role}: {implementation} ({problem})"),
    }
}

/// Whether the firewall lets other devices in, and what to do if not.
pub fn format_firewall(firewall: &app::FirewallStatus) -> String {
    let port = iw_proto::network::effective_port(firewall.port);
    let allow = iw_proto::network::ufw_allow(port);
    match firewall.state() {
        app::FirewallState::Off => "ufw off".to_owned(),
        app::FirewallState::Allows => format!("ufw lets UDP {port} in"),
        app::FirewallState::Blocks => {
            let mdns = iw_proto::network::MDNS_PORT;
            match iw_proto::network::blocked(port, &firewall.blocked_ports) {
                (true, true) => format!(
                    "ufw blocks UDP {port} and {mdns} (mDNS): other devices cannot find this \
                     one or connect until `{allow}`"
                ),
                (false, true) => format!(
                    "ufw blocks UDP {mdns} (mDNS): other devices cannot find this one until \
                     `{allow}`"
                ),
                (true, false) => {
                    format!("ufw blocks UDP {port}: other devices cannot connect until `{allow}`")
                }
                (false, false) => {
                    let ports: Vec<String> =
                        firewall.blocked_ports.iter().map(u32::to_string).collect();
                    format!("ufw blocks UDP {}", ports.join(", "))
                }
            }
        }
        app::FirewallState::OnUnreadable => {
            format!("ufw on, its rules unreadable: if other devices cannot connect, `{allow}`")
        }
        app::FirewallState::Unspecified => "unknown".to_owned(),
    }
}

/// This computer's license as `ctl license` prints it, the key masked.
pub fn format_license(status: &app::Status) -> String {
    let Some(license) = &status.license else {
        return "license:        not kept by this agent\n".to_owned();
    };
    let mut out = String::new();
    let state = match license.state() {
        LicenseState::None => "none (trial)",
        LicenseState::PendingActivation => "waiting for the app to activate it (trial)",
        LicenseState::Licensed => "licensed",
        LicenseState::SeatsFull => "every seat is taken (trial)",
        LicenseState::Invalid => "refused by Polar (trial)",
        LicenseState::Unavailable => "the license file cannot be read right now (trial)",
        LicenseState::Revoked => "revoked by a paired device; the app frees it (trial)",
    };
    out.push_str(&format!("license:        {state}\n"));
    if !license.key.is_empty() {
        out.push_str(&format!(
            "key:            {}\n",
            crate::agent::masked_license_key(&license.key)
        ));
    }
    if !license.source_device_id.is_empty() {
        out.push_str(&format!(
            "from:           {} ({})\n",
            license.source_device_name, license.source_device_id
        ));
    }
    if !license.customer_email.is_empty() {
        out.push_str(&format!("licensed to:    {}\n", license.customer_email));
    }
    // Whatever an agent sends: a time past what the clock can hold is left
    // out rather than overflowing.
    if license.validated_at > 0
        && let Some(at) = UNIX_EPOCH.checked_add(Duration::from_secs(license.validated_at))
    {
        out.push_str(&format!("confirmed:      {}\n", crate::agent::utc(at)));
    }
    for device in &license.devices {
        let state = match device.r#use() {
            app::LicenseUse::Licensed => "licensed",
            app::LicenseUse::Activating => "activating",
            app::LicenseUse::Waiting => "applied, offline: goes as it connects",
            app::LicenseUse::NoPlaces => "no places left",
            app::LicenseUse::OtherLicense => "another license",
            app::LicenseUse::None => "trial",
        };
        let legacy = if device.legacy {
            ", InputWeave 1.0"
        } else {
            ""
        };
        out.push_str(&format!(
            "also on:        {} ({}), {state}{legacy}\n",
            device.name, device.id
        ));
    }
    out
}

/// What to call a profile: the user's name for it, or the name of one of
/// the two fixed ones.
pub fn profile_name(profile: &app::Profile) -> &str {
    match app::ProfileKind::try_from(profile.kind) {
        Ok(app::ProfileKind::Default) => "Default",
        Ok(app::ProfileKind::OtherNetworks) => "Other networks",
        _ => &profile.name,
    }
}

/// What to call a network: its name, or its router, or with neither, its
/// kind.
fn network_name(network: &app::Network) -> String {
    match (network.name.as_str(), network.router.as_str()) {
        ("", "") => match app::NetworkKind::try_from(network.kind) {
            Ok(app::NetworkKind::Wifi) => "Wi-Fi network".to_owned(),
            Ok(app::NetworkKind::Vpn) => "VPN".to_owned(),
            _ => "wired network".to_owned(),
        },
        ("", router) => format!("router {router}"),
        (name, _) => name.to_owned(),
    }
}

/// A network as `ctl` lists it: what to call it, then its kind and router
/// where those say more.
fn network_label(network: &app::Network) -> String {
    let more = match (network.name.is_empty(), network.router.is_empty()) {
        // Called by its kind already.
        (true, true) => String::new(),
        (false, false) => format!(" ({}, router {})", network_kind(network), network.router),
        _ => format!(" ({})", network_kind(network)),
    };
    format!("{}{more}", network_name(network))
}

fn network_kind(network: &app::Network) -> &'static str {
    match app::NetworkKind::try_from(network.kind) {
        Ok(app::NetworkKind::Wired) => "wired",
        Ok(app::NetworkKind::Wifi) => "Wi-Fi",
        Ok(app::NetworkKind::Vpn) => "VPN",
        _ => "network",
    }
}

/// The profile in use, and how it was picked, as `ctl status` says it.
fn profile_line(status: &app::Status) -> Option<String> {
    let profile = status.profiles.iter().find(|p| p.id == status.profile_id)?;
    let how = if status.profile_chosen {
        "chosen"
    } else {
        "automatic"
    };
    let paused = if profile.paused {
        "; paused: not on the network, no device connected"
    } else {
        ""
    };
    Some(format!(
        "profile:        {} ({how}){paused}\n",
        profile_name(profile)
    ))
}

/// The network this device is on, as `ctl status` says it.
fn network_line(status: &app::Status) -> String {
    match &status.network {
        None => "network:        none\n".to_owned(),
        Some(network) => format!(
            "network:        {}, id {}\n",
            network_label(network),
            network.id
        ),
    }
}

/// Human-readable rendering of the profiles (`ctl profile`).
pub fn format_profiles(status: &app::Status) -> String {
    let mut out = String::new();
    out.push_str(&network_line(status));
    if let Some(line) = profile_line(status) {
        out.push_str(&line);
    }
    let wifi = if status.wifi_names { "on" } else { "off" };
    out.push_str(&format!("Wi-Fi names:    {wifi}\n"));
    out.push_str("profiles:\n");
    let name_of = |id: &str| {
        status
            .devices
            .iter()
            .find(|d| d.id == id)
            .map_or_else(|| id.to_owned(), |d| format!("{} ({id})", d.name))
    };
    for profile in &status.profiles {
        let in_use = if profile.id == status.profile_id {
            ", in use"
        } else {
            ""
        };
        out.push_str(&format!(
            "  {} [{}]{in_use}\n",
            profile_name(profile),
            profile.id
        ));
        let sharing = if profile.paused { "paused" } else { "on" };
        let on_off = |on: bool| if on { "on" } else { "off" };
        out.push_str(&format!(
            "    sharing {sharing}, clipboard {}, files {}\n",
            on_off(profile.clipboard),
            on_off(profile.files)
        ));
        if !profile.off_device_ids.is_empty() {
            let off: Vec<String> = profile.off_device_ids.iter().map(|d| name_of(d)).collect();
            out.push_str(&format!("    not connecting: {}\n", off.join(", ")));
        }
        for network in &profile.networks {
            out.push_str(&format!(
                "    network: {}, id {}\n",
                network_label(network),
                network.id
            ));
        }
    }
    out
}

/// Human-readable rendering of a status.
pub fn format_status(status: &app::Status) -> String {
    let mut out = String::new();
    let on_off = if status.sharing { "on" } else { "paused" };
    out.push_str(&format!("sharing:        {on_off}\n"));
    if status.profiles_supported {
        if let Some(line) = profile_line(status) {
            out.push_str(&line);
        }
        out.push_str(&network_line(status));
    }
    if status.offline_for_now {
        out.push_str(&format!(
            "offline:        for now, {}; no device controls this one or is controlled by it, \
             nor shares the clipboard or files with it\n",
            describe_setter(
                &status.this_device_id,
                &status.offline_by_device_id,
                &status.offline_by_name
            )
        ));
    }
    out.push_str(&format!(
        "this device:    {} ({})\n",
        status.this_device_id, status.this_device_name
    ));
    let locked = if status.locked { " (locked)" } else { "" };
    out.push_str(&format!(
        "cursor on:      {}{locked}\n",
        status.active_device_id
    ));
    if status.edge_dwell_ms > 0 {
        out.push_str(&format!(
            "edge dwell:     {} ms before crossing\n",
            status.edge_dwell_ms
        ));
    }
    if status.switch_modifier() != SwitchModifier::None {
        out.push_str(&format!(
            "switching:      only while {} is held\n",
            switch_modifier_name(status.switch_modifier())
        ));
    }
    if status.wake_on_push {
        out.push_str("waking:         pushing the cursor toward a sleeping device wakes it\n");
    }
    if let Some(hotkeys) = &status.hotkeys {
        let jump = if hotkeys.jump_modifiers.is_empty() {
            "off".to_owned()
        } else {
            format!("{}+Digit1 to Digit9", hotkeys.jump_modifiers)
        };
        let relative = match hotkeys.relative.as_deref() {
            None => String::new(),
            Some("") => ", relative off".to_owned(),
            Some(relative) => format!(", relative {relative}"),
        };
        // Said only while on.
        let double = match hotkeys.take_back_double.as_deref() {
            None | Some("") => String::new(),
            Some(double) => format!(" or {double}"),
        };
        out.push_str(&format!(
            "hotkeys:        take back {}{double}, lock {}, jump {jump}{relative}\n",
            hotkeys.take_back, hotkeys.lock
        ));
    }
    if !status.relative_mouse.is_empty() || !status.relative_auto {
        out.push_str(&format!("relative mouse: {}\n", format_relative(status)));
    }
    let controlled_by = if status.controlled_by_device_id.is_empty() {
        "-"
    } else {
        &status.controlled_by_device_id
    };
    out.push_str(&format!("controlled by:  {controlled_by}\n"));
    let capture = if status.capturing { "on" } else { "off" };
    out.push_str(&format!("local capture:  {capture}\n"));
    let previews = if !status.desktop_pictures_available {
        "desktop pictures not available on this computer"
    } else if status.preview_sharing {
        "desktop pictures shared"
    } else {
        "desktop pictures not shared"
    };
    out.push_str(&format!("previews:       {previews}\n"));
    for p in &status.permissions {
        out.push_str(&format!("permission:     {}\n", format_permission(p)));
    }
    if status.secure_input {
        out.push_str("secure input:   on (keystrokes stay on this device)\n");
    }
    if status.screen_locked {
        out.push_str("screen:         locked\n");
    }
    let lock_screen = if status.lock_screen_input {
        "other devices can type on it"
    } else {
        "other devices cannot type on it"
    };
    out.push_str(&format!("lock screen:    {lock_screen}\n"));
    if let Some(session) = &status.session {
        out.push_str(&format!("session:        {}\n", format_session(session)));
        for backend in &session.backends {
            out.push_str(&format!("  {}\n", format_backend(backend)));
        }
    }
    if status.cursor_leaves_at_edges_only {
        out.push_str("leaving:        only across the edges of the screens (no jumps)\n");
    }
    if let Some(firewall) = &status.firewall {
        out.push_str(&format!("firewall:       {}\n", format_firewall(firewall)));
    }
    if status.login_window_access {
        out.push_str("login window:   other devices can control it there\n");
    }
    if !status.folder_access.is_empty() {
        out.push_str(&format!(
            "folders:        {}\n",
            format_folder_access(status)
        ));
    }
    out.push_str("devices:\n");
    if status.devices.is_empty() {
        out.push_str("  (none paired)\n");
    }
    for d in &status.devices {
        let state = match app::ConnectionState::try_from(d.state) {
            _ if d.offline_for_now => "offline for now".to_owned(),
            _ if d.not_in_profile => "not in this profile".to_owned(),
            Ok(app::ConnectionState::Connected) if d.rtt_ms > 0 => {
                format!("connected, {} ms", d.rtt_ms)
            }
            Ok(app::ConnectionState::Connected) => "connected".to_owned(),
            Ok(app::ConnectionState::Incompatible) => "incompatible".to_owned(),
            _ => "offline".to_owned(),
        };
        let origin = if d.paired { "paired" } else { "configured" };
        let mut notes = vec![origin.to_owned()];
        if d.offline_for_now {
            notes.push(describe_setter(
                &status.this_device_id,
                &d.offline_by_device_id,
                &d.offline_by_name,
            ));
        }
        if d.offline_unaware {
            notes.push("InputWeave 1.0: knows nothing of devices taken offline for now".to_owned());
        }
        if d.screen_locked && d.lock_screen_input {
            notes.push("screen locked".to_owned());
        } else if d.screen_locked {
            notes.push("screen locked, no input".to_owned());
        }
        if d.input_unavailable {
            notes.push("takes no input now".to_owned());
        }
        if d.sharing_paused {
            notes.push("sharing paused".to_owned());
        }
        if !d.reported_name.is_empty() && d.reported_name != d.name {
            notes.push(format!("calls itself {:?}", d.reported_name));
        }
        if d.clipboard_off {
            notes.push("clipboard off".to_owned());
        }
        if d.files_off {
            notes.push("files off".to_owned());
        }
        // 0 from agents before cursor speeds.
        if d.speed_percent != 0 && d.speed_percent != 100 {
            notes.push(format!("cursor speed {}%", d.speed_percent));
        }
        if d.jump_slot > 0 {
            notes.push(format!("jump {}", d.jump_slot));
        }
        if d.pointer_captured {
            notes.push("an app holds the pointer".to_owned());
        }
        notes.extend(format_wake(d));
        out.push_str(&format!(
            "  {:<16} {:<24} {:<18} {}\n",
            d.id,
            d.name,
            state,
            notes.join("; ")
        ));
    }
    out
}

/// What a device's status says of waking it: whether it can be, whether it
/// is being woken or did not wake up, and since when it is offline.
pub fn format_wake(device: &app::DeviceStatus) -> Vec<String> {
    let mut notes = Vec::new();
    match device.wake() {
        WakeState::Waking => notes.push("waking".to_owned()),
        WakeState::NotWoken => notes.push("did not wake up".to_owned()),
        WakeState::None if device.can_wake => notes.push("can be woken".to_owned()),
        WakeState::None => {}
    }
    // A time the system's clock cannot hold says nothing more than offline.
    if device.offline_since_unix_ms > 0
        && let Some(since) =
            UNIX_EPOCH.checked_add(Duration::from_millis(device.offline_since_unix_ms))
    {
        notes.push(format!("offline since {}", crate::agent::utc(since)));
    }
    notes
}

pub fn format_arrangement(arrangement: &app::Arrangement) -> String {
    let mut out = format!("arrangement (version {}):\n", arrangement.version);
    for d in &arrangement.devices {
        let at = if d.placed {
            format!("at ({}, {})", d.x, d.y)
        } else {
            "not placed".to_owned()
        };
        let state = if d.this_device {
            "this device"
        } else if d.connected {
            "connected"
        } else {
            "offline"
        };
        let slot = if d.jump_slot > 0 {
            format!("  jump {}", d.jump_slot)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "  {:<16} {:<24} {at:<22} {state}{slot}\n",
            d.id, d.name
        ));
        for s in &d.screens {
            let primary = if s.primary { ", primary" } else { "" };
            let picture = if s.preview_jpeg.is_empty() {
                String::new()
            } else {
                format!(", desktop picture {} bytes", s.preview_jpeg.len())
            };
            out.push_str(&format!(
                "    {} {}x{} at ({}, {}), scale {}{primary}{picture}\n",
                s.id, s.width, s.height, s.x, s.y, s.scale
            ));
        }
    }
    out
}

/// What a batch's items came to.
#[derive(Debug, Default, PartialEq, Eq)]
struct Summary {
    items: usize,
    files: u32,
    done: u32,
    failed: u32,
    /// Items that ended neither done nor failed: declined, cancelled.
    other: usize,
}

impl Summary {
    fn of<'a>(items: impl IntoIterator<Item = &'a app::FileTransfer>) -> Self {
        let mut summary = Self::default();
        for t in items {
            summary.items += 1;
            summary.files += t.files;
            summary.done += t.files_done;
            summary.failed += t.files_failed;
            if !matches!(t.state(), TransferState::Done | TransferState::Failed) {
                summary.other += 1;
            }
        }
        summary
    }

    /// Everything arrived.
    fn complete(&self) -> bool {
        self.failed == 0 && self.other == 0 && self.done == self.files
    }

    fn line(&self) -> String {
        let mut line = format!(
            "{} of {} {} sent, {} failed",
            self.done,
            self.files,
            if self.files == 1 { "file" } else { "files" },
            self.failed
        );
        if self.other > 0 {
            line.push_str(&format!(
                ", {} {} declined or cancelled",
                self.other,
                if self.other == 1 { "item" } else { "items" }
            ));
        }
        line
    }
}

pub fn format_transfers(transfers: &app::Transfers) -> String {
    let mut out = String::from("transfers:\n");
    if transfers.transfers.is_empty() {
        out.push_str("  (none)\n");
    }
    // Batches of more than one item are listed together, under a line of
    // their own, in the order they were sent.
    let mut listed = std::collections::HashSet::new();
    for t in &transfers.transfers {
        if t.batch_items <= 1 || t.batch_id.is_empty() {
            out.push_str(&format!("  {}  [{}]\n", format_transfer(t), t.transfer_id));
            for failure in format_failures(t) {
                out.push_str(&format!("  {failure}\n"));
            }
            continue;
        }
        if !listed.insert(t.batch_id.as_str()) {
            continue;
        }
        let mut items: Vec<&app::FileTransfer> = transfers
            .transfers
            .iter()
            .filter(|i| i.batch_id == t.batch_id)
            .collect();
        items.sort_by_key(|i| i.batch_index);
        let arrow = if t.outgoing { "→" } else { "←" };
        out.push_str(&format!(
            "  batch {arrow} {}: {} items, {} bytes; {}  [{}]\n",
            t.device_id,
            t.batch_items,
            t.batch_bytes,
            Summary::of(items.iter().copied()).line(),
            t.batch_id
        ));
        for item in items {
            out.push_str(&format!(
                "    {}  [{}]\n",
                format_transfer(item),
                item.transfer_id
            ));
            for failure in format_failures(item) {
                out.push_str(&format!("    {failure}\n"));
            }
        }
    }
    let folder = if transfers.receive_folder.is_empty() {
        "(this device does not receive files)"
    } else {
        &transfers.receive_folder
    };
    let ask = if transfers.ask_before_receiving {
        ", asks first"
    } else {
        ""
    };
    out.push_str(&format!("receiving into: {folder}{ask}\n"));
    out
}

fn format_transfer(t: &app::FileTransfer) -> String {
    let arrow = if t.outgoing { "→" } else { "←" };
    let state = match t.state() {
        TransferState::Queued => "queued".to_owned(),
        TransferState::Preparing => "preparing".to_owned(),
        TransferState::WaitingForConnection => "waiting for the connection".to_owned(),
        TransferState::WaitingForAnswer => "waiting for an answer".to_owned(),
        TransferState::Transferring => format!(
            "{}%",
            (t.done_bytes * 100).checked_div(t.size).unwrap_or(100)
        ),
        TransferState::Verifying => "verifying".to_owned(),
        TransferState::Done => "done".to_owned(),
        TransferState::Declined => format!("declined ({})", t.reason),
        TransferState::Failed => format!("failed ({})", t.reason),
        TransferState::Cancelled => "cancelled".to_owned(),
        TransferState::Unspecified => "unknown".to_owned(),
    };
    if !t.folder {
        return format!(
            "{} {arrow} {}  {} bytes  {state}",
            t.name, t.device_id, t.size
        );
    }
    let failed = if t.files_failed > 0 {
        format!(", {} failed", t.files_failed)
    } else {
        String::new()
    };
    format!(
        "{}/ {arrow} {}  {} bytes  {state}  ({} of {} files{failed})",
        t.name, t.device_id, t.size, t.files_done, t.files
    )
}

/// A line for each of a folder's failures listed, and how many more there
/// were.
fn format_failures(t: &app::FileTransfer) -> Vec<String> {
    let mut lines: Vec<String> = t
        .failures
        .iter()
        .map(|f| format!("  failed: {} ({})", f.relative_path, f.reason))
        .collect();
    let unlisted = (t.files_failed as usize).saturating_sub(t.failures.len());
    if t.folder && unlisted > 0 {
        lines.push(format!("  and {unlisted} more failed"));
    }
    lines
}

pub fn format_nearby(nearby: &app::NearbyDevices) -> String {
    let mut out = String::from("nearby:\n");
    if nearby.devices.is_empty() {
        out.push_str("  (none found)\n");
    }
    for d in &nearby.devices {
        let note = if d.paired {
            "paired"
        } else if d.pairable {
            "can pair"
        } else {
            "not accepting pairings"
        };
        out.push_str(&format!(
            "  {:<16} {:<24} {:<8} {:<22} {note}\n",
            d.id, d.name, d.os, d.address
        ));
    }
    out
}

/// One line about a pairing, or `None` when there is nothing to say.
pub fn format_pairing(state: &PairingState) -> Option<String> {
    let peer = or_device(&state.peer_name);
    Some(match state.phase() {
        PairingPhase::Unspecified | PairingPhase::Idle => return None,
        PairingPhase::Connecting => "pairing: connecting…".to_owned(),
        PairingPhase::EnterCode => format!("pairing: enter the code shown on {peer}"),
        PairingPhase::Verifying => "pairing: checking the code…".to_owned(),
        PairingPhase::ShowCode => {
            let left = Duration::from_millis(state.expires_at_unix_ms)
                .saturating_sub(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default(),
                )
                .as_secs();
            let code = &state.code;
            let grouped = if code.len() == 6 {
                format!("{} {}", &code[..3], &code[3..])
            } else {
                code.clone()
            };
            format!(
                "pairing: {peer} wants to pair. Enter {grouped} on {peer} \
                 (valid for {}:{:02}); `inputweave ctl cancel` declines.",
                left / 60,
                left % 60
            )
        }
        PairingPhase::Paired => format!("pairing: paired with {peer}"),
        PairingPhase::Failed => format!("pairing: failed, {}", failure(state)),
    })
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::*;

    #[test]
    fn what_serves_the_session_is_written_with_what_keeps_it_from_working() {
        let backend = |role: app::BackendRole, implementation: &str, problem: &str| {
            let mut b = app::SessionBackend {
                implementation: implementation.to_owned(),
                problem: problem.to_owned(),
                ..Default::default()
            };
            b.set_role(role);
            b
        };
        assert_eq!(
            format_backend(&backend(app::BackendRole::Input, "XTest", "")),
            "input from other devices: XTest"
        );
        assert_eq!(
            format_backend(&backend(
                app::BackendRole::Clipboard,
                "Clipboard portal",
                "only while remote control runs"
            )),
            "clipboard: Clipboard portal (only while remote control runs)"
        );
        assert_eq!(
            format_backend(&backend(
                app::BackendRole::DesktopPictures,
                "",
                "not read on this desktop"
            )),
            "desktop pictures: none (not read on this desktop)"
        );
        assert_eq!(
            format_backend(&backend(app::BackendRole::Displays, "", "")),
            "displays: none"
        );
        let mut session = app::SessionInfo {
            desktop: "KDE".to_owned(),
            ..Default::default()
        };
        session.set_type(app::SessionType::Wayland);
        assert_eq!(format_session(&session), "Wayland (KDE)");
        session.desktop.clear();
        session.set_type(app::SessionType::X11);
        assert_eq!(format_session(&session), "X11");
    }

    #[test]
    fn the_firewall_line_says_how_to_let_other_devices_in() {
        let firewall = |state: app::FirewallState, port: u32| {
            let mut f = app::FirewallStatus {
                port,
                ..Default::default()
            };
            f.set_state(state);
            f
        };
        assert_eq!(
            format_firewall(&firewall(app::FirewallState::Off, 47820)),
            "ufw off"
        );
        assert_eq!(
            format_firewall(&firewall(app::FirewallState::Allows, 47820)),
            "ufw lets UDP 47820 in"
        );
        let blocks = |ports: &[u32]| app::FirewallStatus {
            blocked_ports: ports.to_vec(),
            ..firewall(app::FirewallState::Blocks, 47820)
        };
        assert_eq!(
            format_firewall(&blocks(&[47820])),
            "ufw blocks UDP 47820: other devices cannot connect until `sudo ufw allow InputWeave`"
        );
        // From an agent that does not say which.
        assert_eq!(
            format_firewall(&blocks(&[])),
            format_firewall(&blocks(&[47820]))
        );
        assert_eq!(
            format_firewall(&blocks(&[5353])),
            "ufw blocks UDP 5353 (mDNS): other devices cannot find this one until \
             `sudo ufw allow InputWeave`"
        );
        assert_eq!(
            format_firewall(&blocks(&[47820, 5353])),
            "ufw blocks UDP 47820 and 5353 (mDNS): other devices cannot find this one or \
             connect until `sudo ufw allow InputWeave`"
        );
        // The profile has the default port only.
        assert_eq!(
            format_firewall(&firewall(app::FirewallState::OnUnreadable, 50000)),
            "ufw on, its rules unreadable: if other devices cannot connect, \
             `sudo ufw allow InputWeave && sudo ufw allow 50000/udp`"
        );
        // A port not known is the default one.
        assert_eq!(
            format_firewall(&firewall(app::FirewallState::Allows, 0)),
            "ufw lets UDP 47820 in"
        );
        // Ports the agent does not name as its own or mDNS's, as they are.
        assert_eq!(format_firewall(&blocks(&[12345])), "ufw blocks UDP 12345");
        // iw-platform reads ufw for the same mDNS port the apps name.
        assert_eq!(iw_platform::MDNS_PORT, iw_proto::network::MDNS_PORT);
    }

    #[test]
    fn a_permission_this_ctl_does_not_know_keeps_its_state() {
        let unknown = |granted| app::PermissionStatus {
            permission: 99,
            granted,
            ..Default::default()
        };
        assert_eq!(
            format_permission(&unknown(true)),
            "unknown permission granted"
        );
        assert_eq!(
            format_permission(&unknown(false)),
            "unknown permission missing"
        );
    }

    #[test]
    fn says_where_to_run_ctl_only_when_a_windows_pipe_is_missing() {
        let hint = connect_hint(ErrorKind::NotFound, true);
        assert!(hint.contains("console"), "{hint}");
        // Denied by the pipe's DACL: the agent is there.
        assert_eq!(connect_hint(ErrorKind::PermissionDenied, true), "");
        assert_eq!(connect_hint(ErrorKind::NotFound, false), "");

        let error = CtlError::Connect {
            path: "/tmp/agent.sock".to_owned(),
            source: std::io::Error::from(ErrorKind::NotFound),
            hint: connect_hint(ErrorKind::NotFound, false),
        };
        assert_eq!(
            error.to_string(),
            format!(
                "cannot reach the agent at /tmp/agent.sock: {}",
                std::io::Error::from(ErrorKind::NotFound)
            )
        );
    }

    fn hotkeys(jump_modifiers: &str) -> app::Hotkeys {
        app::Hotkeys {
            take_back: "control+alt+shift+Escape".to_owned(),
            lock: "control+alt+KeyL".to_owned(),
            jump_modifiers: jump_modifiers.to_owned(),
            relative: None,
            take_back_double: None,
        }
    }

    #[test]
    fn lists_the_hotkeys_and_the_device_each_jump_goes_to() {
        let device = |id: &str, name: &str, slot: u32, this_device: bool| app::ArrangedDevice {
            id: id.to_owned(),
            name: name.to_owned(),
            jump_slot: slot,
            this_device,
            ..Default::default()
        };
        let arrangement = app::Arrangement {
            devices: vec![
                device("mac", "MacBook Pro", 1, true),
                device("win", "Windows PC", 3, false),
                device("mini", "Mac mini", 2, false),
                device("new", "Not placed", 0, false),
            ],
            ..Default::default()
        };
        let mut status = app::Status {
            hotkeys: Some(hotkeys("control+alt+shift")),
            ..Default::default()
        };
        assert_eq!(
            format_hotkeys(&status, &arrangement),
            "take back control:  control+alt+shift+Escape\n\
             lock:               control+alt+KeyL\n\
             jump to a device:   control+alt+shift+Digit1 to Digit9\n  \
             control+alt+shift+Digit1  MacBook Pro (mac) (this device)\n  \
             control+alt+shift+Digit2  Mac mini (mini)\n  \
             control+alt+shift+Digit3  Windows PC (win)\n"
        );
        status.hotkeys = Some(hotkeys(""));
        assert!(
            format_hotkeys(&status, &arrangement).ends_with("jump to a device:   off\n"),
            "{}",
            format_hotkeys(&status, &arrangement)
        );
        // A key alone, and the double press from an agent that knows it.
        status.hotkeys = Some(app::Hotkeys {
            take_back: "F13".to_owned(),
            take_back_double: Some("double+control".to_owned()),
            ..hotkeys("")
        });
        assert!(
            format_hotkeys(&status, &arrangement).starts_with(
                "take back control:  F13\n\
                 double press:       double+control\n\
                 lock:               control+alt+KeyL\n"
            ),
            "{}",
            format_hotkeys(&status, &arrangement)
        );
        status.hotkeys.as_mut().unwrap().take_back_double = Some(String::new());
        assert!(
            format_hotkeys(&status, &arrangement).contains("double press:       off\n"),
            "{}",
            format_hotkeys(&status, &arrangement)
        );
    }

    #[test]
    fn the_double_press_is_not_set_on_an_agent_that_does_not_know_it() {
        let old = hotkeys("");
        let error = knows_double_press(&old, Some("double+control")).unwrap_err();
        assert!(matches!(error, CtlError::Unsupported(_)));
        assert_eq!(
            error.to_string(),
            "the running agent does not know the double press (--take-back-double): it is from an older version"
        );
        assert!(knows_double_press(&old, None).is_ok());
        let new = app::Hotkeys {
            take_back_double: Some(String::new()),
            ..old
        };
        assert!(knows_double_press(&new, Some("double+control")).is_ok());
        assert!(knows_double_press(&new, Some("")).is_ok());
    }

    #[test]
    fn profiles_and_networks_are_named_as_people_call_them() {
        let network = |name: &str, kind: app::NetworkKind, router: &str| app::Network {
            id: "5f0c".to_owned(),
            name: name.to_owned(),
            kind: kind.into(),
            router: router.to_owned(),
            resolved: true,
        };
        let profile = |id: &str, kind: app::ProfileKind, name: &str| app::Profile {
            id: id.to_owned(),
            kind: kind.into(),
            name: name.to_owned(),
            clipboard: true,
            files: true,
            ..Default::default()
        };
        let mut office = profile("p1", app::ProfileKind::Custom, "Office");
        office.files = false;
        office.off_device_ids = vec!["win".to_owned(), "gone".to_owned()];
        office.networks = vec![network("Office-5G", app::NetworkKind::Wifi, "198.51.100.1")];
        let mut other = profile("other", app::ProfileKind::OtherNetworks, "");
        other.paused = true;
        let mut status = app::Status {
            profiles_supported: true,
            profiles: vec![
                profile("default", app::ProfileKind::Default, ""),
                office,
                other,
            ],
            profile_id: "p1".to_owned(),
            network: Some(network("", app::NetworkKind::Wired, "192.168.1.1")),
            devices: vec![app::DeviceStatus {
                id: "win".to_owned(),
                name: "Windows PC".to_owned(),
                not_in_profile: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let text = format_profiles(&status);
        for line in [
            "network:        router 192.168.1.1 (wired), id 5f0c\n",
            "profile:        Office (automatic)\n",
            "Wi-Fi names:    off\n",
            "  Default [default]\n",
            "  Office [p1], in use\n",
            "    sharing on, clipboard on, files off\n",
            "    not connecting: Windows PC (win), gone\n",
            "    network: Office-5G (Wi-Fi, router 198.51.100.1), id 5f0c\n",
            "  Other networks [other]\n",
            "    sharing paused, clipboard on, files on\n",
        ] {
            assert!(text.contains(line), "{line}: {text}");
        }
        assert!(format_status(&status).contains(" not in this profile "));
        // A VPN without a router, and no network at all.
        status.network = Some(network("", app::NetworkKind::Vpn, ""));
        status.profile_id = "other".to_owned();
        status.profile_chosen = true;
        let text = format_status(&status);
        assert!(text.contains("network:        VPN, id 5f0c\n"), "{text}");
        status.network = Some(network("", app::NetworkKind::Wired, ""));
        assert!(format_status(&status).contains("network:        wired network, id 5f0c\n"));
        status.network = Some(network("", app::NetworkKind::Wifi, ""));
        assert!(format_status(&status).contains("network:        Wi-Fi network, id 5f0c\n"));
        status.network = Some(network("Office-5G", app::NetworkKind::Wifi, "198.51.100.1"));
        assert!(
            format_status(&status)
                .contains("network:        Office-5G (Wi-Fi, router 198.51.100.1), id 5f0c\n")
        );
        assert!(
            text.contains("profile:        Other networks (chosen); paused: not on the network, no device connected\n"),
            "{text}"
        );
        status.network = None;
        assert!(format_status(&status).contains("network:        none\n"));
        // An agent from before profiles: nothing of them.
        status.profiles_supported = false;
        assert!(!format_status(&status).contains("profile:"));
    }

    #[test]
    fn the_status_says_who_took_a_device_offline_whatever_is_known() {
        let device = |id: &str, by_id: &str, by_name: &str| app::DeviceStatus {
            id: id.to_owned(),
            name: id.to_owned(),
            paired: true,
            offline_for_now: true,
            offline_by_device_id: by_id.to_owned(),
            offline_by_name: by_name.to_owned(),
            ..Default::default()
        };
        let mut status = app::Status {
            this_device_id: "mac".to_owned(),
            this_device_name: "MacBook Pro".to_owned(),
            devices: vec![
                device("a", "", ""),
                device("b", "", "Studio PC"),
                device("c", "mac", "MacBook Pro"),
                device("d", "win", "Windows PC"),
            ],
            ..Default::default()
        };
        let text = format_status(&status);
        for line in [
            "paired; set by another computer\n",
            "paired; set by Studio PC\n",
            "paired; set on this device\n",
            "paired; set by Windows PC (win)\n",
        ] {
            assert!(text.contains(line), "{line}: {text}");
        }
        assert!(
            !text.contains("set by \n") && !text.contains("()"),
            "{text}"
        );
        // This device, taken offline by a device no one knows.
        status.offline_for_now = true;
        let text = format_status(&status);
        assert!(
            text.contains("offline:        for now, set by another computer;"),
            "{text}"
        );
    }

    #[test]
    fn the_status_says_how_the_cursor_switches_and_each_device_number() {
        let mut status = app::Status {
            hotkeys: Some(hotkeys("")),
            devices: vec![app::DeviceStatus {
                id: "win".to_owned(),
                name: "Windows PC".to_owned(),
                paired: true,
                jump_slot: 2,
                ..Default::default()
            }],
            ..Default::default()
        };
        let text = format_status(&status);
        assert!(!text.contains("switching:"), "{text}");
        assert!(
            text.contains(
                "hotkeys:        take back control+alt+shift+Escape, lock control+alt+KeyL, jump off\n"
            ),
            "{text}"
        );
        assert!(text.contains("paired; jump 2\n"), "{text}");
        status.hotkeys.as_mut().unwrap().take_back_double = Some("double+control".to_owned());
        assert!(
            format_status(&status).contains(
                "hotkeys:        take back control+alt+shift+Escape or double+control, lock control+alt+KeyL, jump off\n"
            ),
            "{}",
            format_status(&status)
        );
        status.hotkeys.as_mut().unwrap().take_back_double = Some(String::new());
        status.devices[0].sharing_paused = true;
        assert!(
            format_status(&status).contains("paired; sharing paused; jump 2\n"),
            "{}",
            format_status(&status)
        );
        status.devices[0].sharing_paused = false;
        status.devices[0].can_wake = true;
        status.devices[0].offline_since_unix_ms = 1_762_480_000_000;
        assert!(
            format_status(&status)
                .contains("paired; jump 2; can be woken; offline since 2025-11-07 01:46:40 UTC\n"),
            "{}",
            format_status(&status)
        );
        // A time out of the clock's range (Windows' is the smallest) is no
        // more than offline, and never a panic.
        status.devices[0].offline_since_unix_ms = u64::MAX;
        assert!(format_status(&status).contains("paired; jump 2; can be woken"));
        status.devices[0].offline_since_unix_ms = 1_762_480_000_000;
        status.devices[0].set_wake(WakeState::Waking);
        assert!(format_status(&status).contains("paired; jump 2; waking; offline since"));
        status.devices[0].set_wake(WakeState::NotWoken);
        assert!(format_status(&status).contains("paired; jump 2; did not wake up; offline"));
        status.devices[0] = app::DeviceStatus {
            can_wake: false,
            offline_since_unix_ms: 0,
            ..status.devices[0].clone()
        };
        status.devices[0].set_wake(WakeState::None);
        assert!(!format_status(&status).contains("waking:"));
        status.wake_on_push = true;
        assert!(
            format_status(&status)
                .contains("waking:         pushing the cursor toward a sleeping device wakes it\n")
        );
        status.wake_on_push = false;
        status.set_switch_modifier(SwitchModifier::Shift);
        assert!(
            format_status(&status).contains("switching:      only while shift is held\n"),
            "{}",
            format_status(&status)
        );
    }

    #[test]
    fn says_where_the_cursor_was_shown() {
        let located = app::CursorLocated { x: 204.0, y: 899.6 };
        assert_eq!(
            format_cursor_located(&located),
            "cursor located at (204, 900)"
        );
    }

    #[test]
    fn tells_a_computer_without_desktop_pictures_apart() {
        let line = |status: &app::Status| {
            format_status(status)
                .lines()
                .find(|l| l.starts_with("previews:"))
                .unwrap()
                .to_owned()
        };
        let mut status = app::Status {
            preview_sharing: true,
            desktop_pictures_available: true,
            ..Default::default()
        };
        assert!(line(&status).ends_with("desktop pictures shared"));
        status.preview_sharing = false;
        assert!(line(&status).ends_with("desktop pictures not shared"));
        status.desktop_pictures_available = false;
        status.preview_sharing = true;
        assert!(
            line(&status).ends_with("desktop pictures not available on this computer"),
            "{}",
            line(&status)
        );
    }

    #[test]
    fn says_which_devices_are_renamed_or_share_less() {
        let device = |id: &str, name: &str, reported: &str| app::DeviceStatus {
            id: id.to_owned(),
            name: name.to_owned(),
            reported_name: reported.to_owned(),
            paired: true,
            ..Default::default()
        };
        let status = app::Status {
            devices: vec![
                app::DeviceStatus {
                    clipboard_off: true,
                    files_off: true,
                    ..device("win", "書房的 PC", "Windows PC")
                },
                device("mini", "Mac mini", "Mac mini"),
                // From an agent before names given here.
                device("old", "Old Mac", ""),
            ],
            ..Default::default()
        };
        let text = format_status(&status);
        let line = |id: &str| {
            text.lines()
                .find(|l| l.trim_start().starts_with(id))
                .unwrap()
                .trim_end()
                .to_owned()
        };
        assert!(
            line("win").ends_with("paired; calls itself \"Windows PC\"; clipboard off; files off"),
            "{text}"
        );
        // Its own name: nothing to say.
        assert!(line("mini").ends_with(" paired"), "{text}");
        assert!(line("old").ends_with(" paired"), "{text}");
    }

    #[test]
    fn lists_files_waiting_their_turn() {
        let file = |name: &str, state: TransferState, done_bytes: u64| app::FileTransfer {
            outgoing: true,
            device_id: "win".to_owned(),
            name: name.to_owned(),
            size: 1000,
            done_bytes,
            state: state.into(),
            ..Default::default()
        };
        let mut transfers = app::Transfers {
            transfers: vec![
                file("b.zip", TransferState::Queued, 0),
                file("a.zip", TransferState::Transferring, 250),
            ],
            ..Default::default()
        };
        transfers.transfers[0].transfer_id = "18f0a-1c2-2".to_owned();
        transfers.transfers[1].transfer_id = "18f0a-1c2-1".to_owned();
        let text = format_transfers(&transfers);
        assert!(
            text.contains(
                "  b.zip → win  1000 bytes  queued  [18f0a-1c2-2]\n  \
                 a.zip → win  1000 bytes  25%  [18f0a-1c2-1]\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn batches_are_listed_together_with_what_failed() {
        let item = |id: &str, index: u32, name: &str| app::FileTransfer {
            transfer_id: id.to_owned(),
            outgoing: true,
            device_id: "win".to_owned(),
            name: name.to_owned(),
            batch_id: "b-1".to_owned(),
            batch_index: index,
            batch_items: 2,
            batch_files: 4,
            batch_bytes: 300,
            ..Default::default()
        };
        let photos = app::FileTransfer {
            size: 200,
            state: TransferState::Done.into(),
            folder: true,
            files: 3,
            files_done: 2,
            files_failed: 1,
            failures: vec![app::FailedFile {
                relative_path: "Photos/link".to_owned(),
                reason: "symlink".to_owned(),
            }],
            ..item("p-1", 0, "Photos")
        };
        let notes = app::FileTransfer {
            size: 100,
            state: TransferState::Done.into(),
            files: 1,
            files_done: 1,
            ..item("n-1", 1, "notes.txt")
        };
        // Newest first, as the agent lists them.
        let transfers = app::Transfers {
            transfers: vec![notes.clone(), photos.clone()],
            ..Default::default()
        };
        let text = format_transfers(&transfers);
        assert!(
            text.contains(
                "  batch → win: 2 items, 300 bytes; 3 of 4 files sent, 1 failed  [b-1]\n    \
                 Photos/ → win  200 bytes  done  (2 of 3 files, 1 failed)  [p-1]\n      \
                 failed: Photos/link (symlink)\n    \
                 notes.txt → win  100 bytes  done  [n-1]\n"
            ),
            "{text}"
        );
        let summary = Summary::of([&photos, &notes]);
        assert!(!summary.complete());
        assert_eq!(summary.line(), "3 of 4 files sent, 1 failed");
    }

    #[test]
    fn the_license_prints_with_its_key_masked() {
        let status = app::Status {
            license: Some(app::License {
                key: "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42".to_owned(),
                state: LicenseState::Licensed.into(),
                customer_email: "someone@example.com".to_owned(),
                validated_at: 1_793_800_000,
                devices: vec![
                    app::LicensedDevice {
                        id: "win".to_owned(),
                        name: "Windows PC".to_owned(),
                        licensed: true,
                        r#use: app::LicenseUse::Licensed.into(),
                        activation_id: "act-secret".to_owned(),
                        legacy: false,
                        can_take_back: false,
                        key_out_of_reach: false,
                        legacy_refused: false,
                        waiting_to_apply: false,
                    },
                    app::LicensedDevice {
                        id: "old".to_owned(),
                        name: "Old PC".to_owned(),
                        r#use: app::LicenseUse::Waiting.into(),
                        legacy: true,
                        ..app::LicensedDevice::default()
                    },
                ],
                ..app::License::default()
            }),
            ..app::Status::default()
        };
        let out = format_license(&status);
        assert!(out.contains("license:        licensed\n"), "{out}");
        assert!(out.contains("key:            INPUTWEAVE-…7E42\n"), "{out}");
        assert!(!out.contains("58D4"), "{out}");
        assert!(
            out.contains("confirmed:      2026-11-04 13:46:40 UTC\n"),
            "{out}"
        );
        assert!(
            out.contains("also on:        Windows PC (win), licensed\n"),
            "{out}"
        );
        assert!(
            out.contains(
                "also on:        Old PC (old), applied, offline: goes as it connects, InputWeave 1.0\n"
            ),
            "{out}"
        );
        assert!(!out.contains("act-secret"), "{out}");
        assert_eq!(
            format_license(&app::Status::default()),
            "license:        not kept by this agent\n"
        );
        // Whatever an agent sends prints without overflowing.
        let odd = app::Status {
            license: Some(app::License {
                state: LicenseState::Unavailable.into(),
                validated_at: u64::MAX,
                ..app::License::default()
            }),
            ..app::Status::default()
        };
        assert_eq!(
            format_license(&odd),
            "license:        the license file cannot be read right now (trial)\n"
        );
    }
}
