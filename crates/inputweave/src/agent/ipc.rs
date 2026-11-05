//! The agent's IPC server for the native UI and `inputweave ctl`.
//!
//! A client must say Hello with a matching IPC version first. After that it
//! receives an event whenever the status, the pairing state or the list of
//! nearby devices changes, in addition to responses to its commands.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use iw_core::geometry::Point;
use iw_core::hotkey::Hotkeys;
use iw_ipc::{IpcConnection, IpcListener};
use iw_net::PairingCode;
use iw_net::discovery::reachable_from;
use iw_platform::{FilesFetch, PlatformError};
use iw_proto::IPC_VERSION;
use iw_proto::app::v1::{
    AgentHello, AgentMessage, ClientMessage, CursorLocated, Diagnostics, DragArrived, DragBegun,
    DragEnd as ProtoDragEnd, DragEnded, DraggedItem, Error, Event, FailedFile, PasteClipboardFiles,
    RelativeUnsupported, Response, ScreensIdentified, SwitchModifier, agent_message,
    client_message::Command, event, response, start_pairing,
};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinSet;
use tracing::warn;

use super::app_clipboard::{AppClipboard, AppEvent, AppSession, Pasting};
use super::directory::Directory;
use super::drag::{DragEnd, DragEvent};
use super::engine::{DragError, EngineEvent, JumpError, RearrangeError, RelativeError};
use super::files::{FileSettings, Files, RemoveError};
use super::folder_access::FolderAsker;
use super::listing::{ItemKind, Manifest};
use super::pairing::{Failure, Pairing};
use super::status::{
    Arrangement, Status, nearby_to_proto, switch_modifier_from_proto, transfers_to_proto,
};
use super::this_device::ThisDevice;
use crate::config::DEFAULT_PORT;

/// How long a request waits for the engine to apply a change.
const APPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// How long SubmitPairingCode waits for the other device's verdict.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);

const ACCEPT_RETRY: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub(crate) struct IpcContext {
    pub(crate) engine: mpsc::UnboundedSender<EngineEvent>,
    pub(crate) status: watch::Receiver<Status>,
    pub(crate) arrangement: watch::Receiver<Arrangement>,
    /// Where to show the cursor, each time the user shakes the mouse.
    pub(crate) located: watch::Receiver<Option<Point>>,
    /// How long to show this device's name on its screens, each time they
    /// are identified.
    pub(crate) identified: watch::Receiver<Option<u32>>,
    /// The device the relative mode hotkey was pressed on that cannot take
    /// relative movements, each time.
    pub(crate) relative_unsupported: watch::Receiver<Option<String>>,
    pub(crate) cursor_crossed: broadcast::Sender<super::CursorCrossed>,
    /// Drags offered here, and ended.
    pub(crate) drag_events: broadcast::Sender<DragEvent>,
    pub(crate) pairing: Pairing,
    pub(crate) directory: Arc<Directory>,
    pub(crate) this: Arc<ThisDevice>,
    pub(crate) files: Arc<Files>,
    /// Where received files go when the user has not chosen a folder.
    pub(crate) default_receive_folder: Option<PathBuf>,
    /// Asks macOS for the folders it protects; none where there are none.
    pub(crate) folders: Option<Arc<FolderAsker>>,
    /// The session's permissions, for the ones the agent asks for itself
    /// (`RequestPermission`).
    pub(crate) session: Arc<dyn iw_platform::SessionStatus>,
    /// Whether other devices may be let control this Mac at the login
    /// window, or why not.
    pub(crate) login_window: crate::login_window::Support,
    /// Apps that said Hello; with none, the app is opened when the user
    /// must see or decide something.
    pub(crate) ui_clients: Arc<AtomicUsize>,
    /// Where the broker owns the clipboard: the hold of the app that takes
    /// promises with files (`ClientHello.takes_clipboard_files`).
    pub(crate) app_clipboard: Option<Arc<AppClipboard>>,
    /// The peer endpoint's address, to pick addresses it can reach.
    pub(crate) local_addr: SocketAddr,
    pub(crate) diagnostics: Arc<super::diagnostics::Fixed>,
}

pub(crate) async fn serve(listener: IpcListener, ctx: IpcContext) {
    let mut clients = JoinSet::new();
    loop {
        match listener.accept().await {
            Ok(conn) => {
                clients.spawn(client(conn, ctx.clone()));
            }
            Err(e) => {
                warn!(error = %e, "IPC accept failed");
                // Back off so a persistent error (e.g. out of file
                // descriptors) does not turn into a busy loop.
                tokio::time::sleep(ACCEPT_RETRY).await;
            }
        }
        while clients.try_join_next().is_some() {}
    }
}

async fn client(conn: IpcConnection, ctx: IpcContext) {
    let (mut reader, mut writer) = conn.into_split();
    let mut status = ctx.status.clone();
    let mut pairing = ctx.pairing.watch();
    let mut nearby = ctx.directory.watch_nearby();
    let mut arrangement = ctx.arrangement.clone();
    let mut transfers = ctx.files.watch();
    let mut located = ctx.located.clone();
    let mut identified = ctx.identified.clone();
    let mut relative_unsupported = ctx.relative_unsupported.clone();
    // Drags are news only as they happen, from Hello on; so is each
    // crossing of an edge here, every one of them.
    let mut drags: Option<broadcast::Receiver<DragEvent>> = None;
    let mut crossings: Option<broadcast::Receiver<super::CursorCrossed>> = None;
    let mut greeted = false;
    let mut counted = None;
    // An app that takes clipboard files: its hold, and what it hears.
    let mut holder: Option<(AppSession, mpsc::UnboundedReceiver<AppEvent>)> = None;
    loop {
        let event = tokio::select! {
            msg = reader.recv::<ClientMessage>() => {
                let Ok(Some(msg)) = msg else { break };
                let result = match msg.command {
                    Some(Command::Hello(hello)) => {
                        if hello.ipc_version != IPC_VERSION {
                            error(
                                "ipc_version_mismatch",
                                &format!("agent speaks IPC version {IPC_VERSION}"),
                            )
                        } else {
                            greeted = true;
                            // Before it is counted: a promise waiting for
                            // an app to start sees it take files.
                            if hello.takes_clipboard_files
                                && holder.is_none()
                                && let Some(hub) = &ctx.app_clipboard
                            {
                                holder = Some(hub.connect());
                            }
                            // ctl cannot ask the user anything.
                            if counted.is_none() && !hello.client.starts_with("ctl/") {
                                counted = Some(ClientCount::new(&ctx.ui_clients));
                                if holder.is_none()
                                    && let Some(hub) = &ctx.app_clipboard
                                {
                                    hub.client_connected();
                                }
                            }
                            status.mark_unchanged();
                            pairing.mark_unchanged();
                            nearby.mark_unchanged();
                            arrangement.mark_unchanged();
                            transfers.mark_unchanged();
                            // Showing the cursor, or the screens' names,
                            // is news only as it happens.
                            located.mark_unchanged();
                            identified.mark_unchanged();
                            relative_unsupported.mark_unchanged();
                            if drags.is_none() {
                                drags = Some(ctx.drag_events.subscribe());
                            }
                            if crossings.is_none() {
                                crossings = Some(ctx.cursor_crossed.subscribe());
                            }
                            agent_hello(&ctx)
                        }
                    }
                    _ if !greeted => error("hello_required", "send Hello first"),
                    Some(command) => respond(command, &ctx, holding(&ctx, &holder)).await,
                    None => error("unknown_command", "unrecognized or missing command"),
                };
                AgentMessage {
                    msg: Some(agent_message::Msg::Response(Response {
                        request_id: msg.request_id,
                        result: Some(result),
                    })),
                }
            }
            changed = status.changed(), if greeted => {
                if changed.is_err() {
                    break;
                }
                let current = status.borrow_and_update().to_proto();
                event_message(event::Event::StatusChanged(current))
            }
            changed = pairing.changed(), if greeted => {
                if changed.is_err() {
                    break;
                }
                let current = pairing.borrow_and_update().to_proto();
                event_message(event::Event::PairingChanged(current))
            }
            changed = transfers.changed(), if greeted => {
                if changed.is_err() {
                    break;
                }
                let current = transfers.borrow_and_update().clone();
                let list = transfers_to_proto(&current, &ctx.files.settings());
                event_message(event::Event::TransfersChanged(list))
            }
            changed = arrangement.changed(), if greeted => {
                if changed.is_err() {
                    break;
                }
                let current = arrangement.borrow_and_update().to_proto();
                event_message(event::Event::ArrangementChanged(current))
            }
            changed = nearby.changed(), if greeted => {
                if changed.is_err() {
                    break;
                }
                let current = nearby.borrow_and_update().clone();
                let devices = nearby_to_proto(&current, &ctx.directory.peers());
                event_message(event::Event::NearbyChanged(devices))
            }
            changed = located.changed(), if greeted => {
                if changed.is_err() {
                    break;
                }
                let Some(position) = *located.borrow_and_update() else {
                    continue;
                };
                event_message(event::Event::CursorLocated(CursorLocated {
                    x: position.x,
                    y: position.y,
                }))
            }
            changed = identified.changed(), if greeted => {
                if changed.is_err() {
                    break;
                }
                let Some(duration_ms) = *identified.borrow_and_update() else {
                    continue;
                };
                // Outside the event oneof (see app.proto).
                AgentMessage {
                    msg: Some(agent_message::Msg::Event(Event {
                        screens_identified: Some(ScreensIdentified { duration_ms }),
                        ..Event::default()
                    })),
                }
            }
            changed = relative_unsupported.changed(), if greeted => {
                if changed.is_err() {
                    break;
                }
                let Some(device_id) = relative_unsupported.borrow_and_update().clone() else {
                    continue;
                };
                // Outside the event oneof (see app.proto).
                AgentMessage {
                    msg: Some(agent_message::Msg::Event(Event {
                        relative_unsupported: Some(RelativeUnsupported { device_id }),
                        ..Event::default()
                    })),
                }
            }
            crossed = next_crossing(&mut crossings), if crossings.is_some() => {
                let Some(crossed) = crossed else {
                    continue;
                };
                // Outside the event oneof (see app.proto).
                AgentMessage {
                    msg: Some(agent_message::Msg::Event(Event {
                        cursor_crossed: Some(crossed.to_proto()),
                        ..Event::default()
                    })),
                }
            }
            drag = next_drag(&mut drags), if drags.is_some() => match drag {
                Some(drag) => drag_message(drag),
                None => continue,
            },
            app_event = next_app_event(&mut holder), if holder.is_some() => match app_event {
                // Outside the event oneof (see app.proto).
                AppEvent::Promised(promised) => AgentMessage {
                    msg: Some(agent_message::Msg::Event(Event {
                        clipboard_files_promised: Some(promised),
                        ..Event::default()
                    })),
                },
                AppEvent::Arrived(arrived) => AgentMessage {
                    msg: Some(agent_message::Msg::Event(Event {
                        clipboard_files_arrived: Some(arrived),
                        ..Event::default()
                    })),
                },
            },
        };
        if writer.send(&event).await.is_err() {
            break;
        }
    }
}

fn event_message(event: event::Event) -> AgentMessage {
    AgentMessage {
        msg: Some(agent_message::Msg::Event(Event {
            event: Some(event),
            ..Event::default()
        })),
    }
}

/// The hub and this app's place in it, if it takes clipboard files.
fn holding(
    ctx: &IpcContext,
    holder: &Option<(AppSession, mpsc::UnboundedReceiver<AppEvent>)>,
) -> Option<(Arc<AppClipboard>, u64)> {
    let hub = ctx.app_clipboard.clone()?;
    holder.as_ref().map(|(session, _)| (hub, session.id()))
}

/// What the app that takes clipboard files hears next.
async fn next_app_event(
    holder: &mut Option<(AppSession, mpsc::UnboundedReceiver<AppEvent>)>,
) -> AppEvent {
    match holder.as_mut() {
        Some((_, events)) => match events.recv().await {
            Some(event) => event,
            None => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

fn no_clipboard_files() -> response::Result {
    error("not_found", "no such promise of clipboard files")
}

/// The app pastes the files of a promise it put on the clipboard into
/// `paste.folder`: the user must be able to write there, checked as the
/// user where the broker writes. The first paste has them fetched, and the
/// others share it; the app hears when they are there
/// (`ClipboardFilesArrived`), without anything waiting for them here.
async fn paste_clipboard_files(
    ctx: &IpcContext,
    hold: Option<(Arc<AppClipboard>, u64)>,
    paste: PasteClipboardFiles,
) -> response::Result {
    let id = paste.promise_id;
    let Some((hub, app)) = hold.filter(|(hub, app)| hub.holds(*app, id)) else {
        return no_clipboard_files();
    };
    let folder = PathBuf::from(&paste.folder);
    if let Err(e) = ctx.files.check_destination(&folder).await {
        return match e.kind() {
            std::io::ErrorKind::NotConnected => error("no_broker", &e.to_string()),
            _ => error("invalid_folder", &e.to_string()),
        };
    }
    match hub.begin_paste(app, id) {
        Pasting::Unknown => return no_clipboard_files(),
        Pasting::Joined => {}
        Pasting::Start(source) => {
            let (done, fetched) = tokio::sync::oneshot::channel();
            source.fetch_files_into(
                folder,
                Box::new(move |files| {
                    let _ = done.send(files);
                }),
            );
            tokio::spawn(async move {
                let files = fetched.await.unwrap_or(FilesFetch::Unavailable);
                hub.arrived(app, id, files);
            });
        }
    }
    response::Result::Status(ctx.status.borrow().to_proto())
}

/// The next drag event; none when this client fell behind and missed
/// some, which it learns nothing more about.
async fn next_drag(drags: &mut Option<broadcast::Receiver<DragEvent>>) -> Option<DragEvent> {
    let drags = drags.as_mut()?;
    match drags.recv().await {
        Ok(event) => Some(event),
        Err(broadcast::error::RecvError::Lagged(missed)) => {
            warn!(missed, "an IPC client missed drag events");
            None
        }
        // The engine is gone: the agent is stopping.
        Err(broadcast::error::RecvError::Closed) => std::future::pending().await,
    }
}

async fn next_crossing(
    crossings: &mut Option<broadcast::Receiver<super::CursorCrossed>>,
) -> Option<super::CursorCrossed> {
    let crossings = crossings.as_mut()?;
    match crossings.recv().await {
        Ok(crossed) => Some(crossed),
        Err(broadcast::error::RecvError::Lagged(missed)) => {
            tracing::debug!(missed, "an IPC client missed crossings of the edges");
            None
        }
        // The engine is gone: the agent is stopping.
        Err(broadcast::error::RecvError::Closed) => std::future::pending().await,
    }
}

/// A drag event, outside the event oneof (see app.proto).
fn drag_message(drag: DragEvent) -> AgentMessage {
    let event = match drag {
        DragEvent::Arrived(arrived) => Event {
            drag_arrived: Some(DragArrived {
                drag_id: arrived.id,
                from_device_id: arrived.from,
                items: arrived
                    .items
                    .into_iter()
                    .map(|item| DraggedItem {
                        name: item.name,
                        folder: item.folder,
                        size: item.size,
                        files: item.files,
                    })
                    .collect(),
                files: arrived.files,
                bytes: arrived.bytes,
                at_controller: arrived.at_controller,
                source_device_id: arrived.source.unwrap_or_default(),
            }),
            ..Event::default()
        },
        DragEvent::Ended(ended) => Event {
            drag_ended: Some(DragEnded {
                drag_id: ended.id,
                reason: match ended.reason {
                    DragEnd::Accepted => ProtoDragEnd::Accepted,
                    DragEnd::Declined => ProtoDragEnd::Declined,
                    DragEnd::Released => ProtoDragEnd::Released,
                    DragEnd::Withdrawn => ProtoDragEnd::Withdrawn,
                    DragEnd::Expired => ProtoDragEnd::Expired,
                }
                .into(),
                device_id: ended.device.unwrap_or_default(),
            }),
            ..Event::default()
        },
    };
    AgentMessage {
        msg: Some(agent_message::Msg::Event(event)),
    }
}

/// Counts a connected client for as long as it is connected.
struct ClientCount<'a>(&'a AtomicUsize);

impl<'a> ClientCount<'a> {
    fn new(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}

impl Drop for ClientCount<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn transfers_result(ctx: &IpcContext) -> response::Result {
    response::Result::Transfers(transfers_to_proto(
        &ctx.files.transfers(),
        &ctx.files.settings(),
    ))
}

fn agent_hello(ctx: &IpcContext) -> response::Result {
    response::Result::Hello(AgentHello {
        ipc_version: IPC_VERSION,
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        agent_digest: ctx.diagnostics.agent_digest.clone(),
    })
}

fn error(code: &str, detail: &str) -> response::Result {
    response::Result::Error(Error {
        code: code.to_owned(),
        detail: detail.to_owned(),
    })
}

/// Answers `command`; `hold` is the hub and this client's place in it, if
/// it takes clipboard files.
async fn respond(
    command: Command,
    ctx: &IpcContext,
    hold: Option<(Arc<AppClipboard>, u64)>,
) -> response::Result {
    let status = || response::Result::Status(ctx.status.borrow().to_proto());
    match command {
        // A repeated Hello from a client that already said it.
        Command::Hello(_) => agent_hello(ctx),
        Command::ClipboardFilesPlace(place) => match hold {
            None => no_clipboard_files(),
            Some((hub, app)) => match hub.place(app, place.promise_id) {
                Ok(()) => status(),
                Err(code @ "too_late") => error(code, "the agent no longer waits for this promise"),
                Err(code @ "superseded") => error(code, "something was copied here since"),
                Err(_) => no_clipboard_files(),
            },
        },
        Command::ClipboardFilesPlaced(placed) => {
            if hold.is_some_and(|(hub, app)| hub.placed(app, placed.promise_id, placed.ok)) {
                status()
            } else {
                no_clipboard_files()
            }
        }
        Command::PasteClipboardFiles(paste) => paste_clipboard_files(ctx, hold, paste).await,
        Command::ClipboardFilesGone(gone) => {
            if hold.is_some_and(|(hub, app)| hub.gone(app, gone.promise_id)) {
                status()
            } else {
                no_clipboard_files()
            }
        }
        Command::UserQuitApp(_) => {
            if let Some(hub) = &ctx.app_clipboard {
                hub.user_quit();
            }
            status()
        }
        Command::GetStatus(_) => response::Result::Status(ctx.status.borrow().to_proto()),
        Command::SetSharing(set) => {
            if ctx
                .engine
                .send(EngineEvent::SetSharing(set.enabled))
                .is_err()
            {
                return error("agent_stopping", "the agent is shutting down");
            }
            let mut status = ctx.status.clone();
            let applied =
                tokio::time::timeout(APPLY_TIMEOUT, status.wait_for(|s| s.sharing == set.enabled))
                    .await;
            match applied {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
        Command::StartPairing(start) => {
            let address = match start.target {
                Some(start_pairing::Target::DeviceId(id)) => nearby_address(ctx, &id),
                Some(start_pairing::Target::Address(text)) => resolve(ctx, &text).await,
                None => Err(Refusal::new(
                    "invalid_target",
                    "give a device id or an address",
                )),
            };
            let address = match address {
                Ok(address) => address,
                Err(Refusal { code, detail }) => return error(code, &detail),
            };
            match ctx.pairing.start(address).await {
                Ok(()) => response::Result::Pairing(ctx.pairing.status().to_proto()),
                Err(Failure::Busy) => error("busy", "a pairing is already in progress"),
                Err(f) => error(f.code(), "pairing could not start"),
            }
        }
        Command::SubmitPairingCode(submit) => {
            let Ok(code) = submit.code.parse::<PairingCode>() else {
                return error("invalid_code", "a pairing code is six digits");
            };
            match tokio::time::timeout(VERIFY_TIMEOUT, ctx.pairing.submit(code)).await {
                Ok(Ok(state)) => response::Result::Pairing(state.to_proto()),
                Ok(Err(e)) => error("not_waiting", &e.to_string()),
                Err(_) => error("timeout", "the other device did not answer in time"),
            }
        }
        Command::CancelPairing(_) => {
            ctx.pairing.cancel();
            response::Result::Pairing(ctx.pairing.status().to_proto())
        }
        Command::Unpair(unpair) => {
            if let Err(e) = ctx.directory.remove_paired(&unpair.device_id) {
                return error(e.code(), &e.to_string());
            }
            let mut status = ctx.status.clone();
            let id = unpair.device_id;
            match tokio::time::timeout(APPLY_TIMEOUT, status.wait_for(|s| s.device(&id).is_none()))
                .await
            {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
        Command::GetPairing(_) => response::Result::Pairing(ctx.pairing.status().to_proto()),
        Command::SetDeviceName(set) => {
            if super::rename(&ctx.this, &ctx.directory, &ctx.engine, &set.name).is_err() {
                return error(
                    "invalid_name",
                    "a device name is 1 to 64 characters without control characters",
                );
            }
            let mut status = ctx.status.clone();
            let name = set.name.trim().to_owned();
            match tokio::time::timeout(APPLY_TIMEOUT, status.wait_for(|s| s.this_name == name))
                .await
            {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
        Command::GetTransfers(_) => transfers_result(ctx),
        Command::GetDiagnostics(_) => response::Result::Diagnostics(Diagnostics {
            report: super::diagnostics_report(
                &ctx.diagnostics,
                &ctx.status,
                &ctx.arrangement,
                &ctx.directory,
                &ctx.files,
            )
            .await,
        }),
        Command::SendFiles(send) => {
            let paths = send.paths.into_iter().map(PathBuf::from).collect();
            match ctx.files.send(&send.device_id, paths).await {
                Ok(_) => transfers_result(ctx),
                Err(e) => error(e.code(), &e.to_string()),
            }
        }
        Command::BeginDrag(begin) => {
            let paths = begin
                .items
                .into_iter()
                .map(|i| PathBuf::from(i.path))
                .collect();
            match super::begin_drag(&ctx.files, &ctx.engine, paths).await {
                Ok((drag_id, manifest)) => {
                    response::Result::DragBegun(drag_begun(drag_id, &manifest))
                }
                Err(e) => error(e.code(), &e.to_string()),
            }
        }
        Command::EndDrag(end) => {
            if super::end_drag(&ctx.engine, &end.drag_id).await {
                response::Result::Status(ctx.status.borrow().to_proto())
            } else {
                drag_error(&DragError::NotFound)
            }
        }
        Command::AcceptDrag(accept) => {
            let destination = PathBuf::from(&accept.destination);
            match super::accept_drag(&ctx.files, &ctx.engine, &accept.drag_id, destination).await {
                Ok(()) => transfers_result(ctx),
                Err(e) => drag_error(&e),
            }
        }
        Command::DeclineDrag(decline) => {
            if super::decline_drag(&ctx.engine, &decline.drag_id).await {
                response::Result::Status(ctx.status.borrow().to_proto())
            } else {
                drag_error(&DragError::NotFound)
            }
        }
        Command::CancelTransfer(cancel) => {
            if ctx.files.cancel(&cancel.transfer_id) {
                transfers_result(ctx)
            } else {
                error("not_found", "no such transfer is going on")
            }
        }
        Command::RemoveTransfer(remove) => match ctx.files.remove(&remove.transfer_id) {
            Ok(()) => transfers_result(ctx),
            Err(e) => {
                let code = match e {
                    RemoveError::NotFound(_) => "not_found",
                    RemoveError::NotFinished(_) => "not_finished",
                };
                error(code, &e.to_string())
            }
        },
        Command::ClearTransfers(_) => {
            ctx.files.clear();
            transfers_result(ctx)
        }
        Command::AnswerFileOffer(answer) => {
            if ctx.files.answer(&answer.transfer_id, answer.accept) {
                transfers_result(ctx)
            } else {
                error("not_waiting", "no file is waiting for an answer")
            }
        }
        Command::SetFileSettings(set) => {
            let folder = if set.receive_folder.is_empty() {
                None
            } else {
                let folder = PathBuf::from(&set.receive_folder);
                if !folder.is_absolute() {
                    return error("invalid_folder", "the folder must be an absolute path");
                }
                Some(folder)
            };
            ctx.directory
                .set_file_settings(folder.clone(), set.ask_before_receiving);
            ctx.files.set_settings(FileSettings {
                folder: folder.or_else(|| ctx.default_receive_folder.clone()),
                ask: set.ask_before_receiving,
            });
            transfers_result(ctx)
        }
        Command::SetClipboardSync(set) => {
            super::set_clipboard_sync(&ctx.directory, &ctx.engine, set.enabled);
            let mut status = ctx.status.clone();
            match tokio::time::timeout(
                APPLY_TIMEOUT,
                status.wait_for(|s| s.clipboard_sync == set.enabled),
            )
            .await
            {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
        Command::SetPreviewSharing(set) => {
            super::set_preview_sharing(&ctx.directory, &ctx.engine, set.enabled);
            let mut status = ctx.status.clone();
            match tokio::time::timeout(
                APPLY_TIMEOUT,
                status.wait_for(|s| s.preview_sharing == set.enabled),
            )
            .await
            {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
        Command::SetDeviceOptions(set) => {
            if let Err(e) = ctx
                .directory
                .set_swap(&set.device_id, set.swap_gui_and_control)
            {
                return error(e.code(), &e.to_string());
            }
            let mut status = ctx.status.clone();
            let applied = status.wait_for(|s| {
                s.device(&set.device_id)
                    .is_some_and(|d| d.swap_gui_and_control == set.swap_gui_and_control)
            });
            match tokio::time::timeout(APPLY_TIMEOUT, applied).await {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
        Command::RenameDevice(rename) => {
            if let Err(e) = ctx.directory.set_alias(&rename.device_id, &rename.name) {
                return error(e.code(), &e.to_string());
            }
            // The name shown now: the one given, or the device's own.
            let Some(name) = ctx.directory.peer(&rename.device_id).map(|p| p.name) else {
                return error("unknown_device", "the device is no longer trusted");
            };
            let mut status = ctx.status.clone();
            let applied =
                status.wait_for(|s| s.device(&rename.device_id).is_some_and(|d| d.name == name));
            match tokio::time::timeout(APPLY_TIMEOUT, applied).await {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
        Command::SetDeviceSharing(set) => {
            if let Err(e) =
                ctx.directory
                    .set_sharing(&set.device_id, set.clipboard_off, set.files_off)
            {
                return error(e.code(), &e.to_string());
            }
            let mut status = ctx.status.clone();
            let applied = status.wait_for(|s| {
                s.device(&set.device_id).is_some_and(|d| {
                    d.clipboard_off == set.clipboard_off && d.files_off == set.files_off
                })
            });
            match tokio::time::timeout(APPLY_TIMEOUT, applied).await {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
        Command::SetLoginWindowAccess(set) => {
            if let Err(e) = super::set_login_window_access(
                &ctx.directory,
                &ctx.engine,
                ctx.login_window,
                &ctx.status,
                set.on,
            ) {
                return error(e.code(), &e.to_string());
            }
            let mut status = ctx.status.clone();
            let applied = status.wait_for(|s| s.login_window_access == set.on);
            match tokio::time::timeout(APPLY_TIMEOUT, applied).await {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
        Command::IdentifyScreens(_) => {
            if ctx.engine.send(EngineEvent::IdentifyScreens).is_err() {
                return error("agent_stopping", "the agent is shutting down");
            }
            response::Result::Status(ctx.status.borrow().to_proto())
        }
        Command::GetArrangement(_) => {
            response::Result::Arrangement(ctx.arrangement.borrow().to_proto())
        }
        Command::SetArrangement(set) => {
            let moves = set
                .positions
                .into_iter()
                .map(|p| (p.device_id, Point::new(p.x, p.y)))
                .collect();
            match super::rearrange(&ctx.engine, moves).await {
                Ok(()) => response::Result::Arrangement(ctx.arrangement.borrow().to_proto()),
                Err(e) => {
                    let code = match e {
                        RearrangeError::UnknownDevice(_) => "unknown_device",
                        RearrangeError::Configured(_) => "configured_placement",
                        RearrangeError::NotFinite(_) | RearrangeError::Invalid(_) => {
                            "invalid_arrangement"
                        }
                    };
                    error(code, &e.to_string())
                }
            }
        }
        Command::GetNearby(_) => response::Result::Nearby(nearby_to_proto(
            &ctx.directory.nearby(),
            &ctx.directory.peers(),
        )),
        Command::SetLocked(set) => {
            if ctx.engine.send(EngineEvent::SetLocked(set.locked)).is_err() {
                return error("agent_stopping", "the agent is shutting down");
            }
            let mut status = ctx.status.clone();
            match tokio::time::timeout(APPLY_TIMEOUT, status.wait_for(|s| s.locked == set.locked))
                .await
            {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
        Command::SetLicenseKey(set) => {
            let change = super::engine::LicenseChange::SetKey(set.key);
            license_result(ctx, super::license(&ctx.engine, change).await)
        }
        Command::RecordLicenseResult(record) => {
            let Some(result) = license_result_from_proto(record.result, record.activation_id)
            else {
                return error("invalid_license_result", "the result has no outcome");
            };
            let change = super::engine::LicenseChange::Record {
                key: record.key,
                result,
            };
            license_result(ctx, super::license(&ctx.engine, change).await)
        }
        Command::RemoveLicense(_) => {
            let change = super::engine::LicenseChange::Remove;
            license_result(ctx, super::license(&ctx.engine, change).await)
        }
        Command::JumpTo(jump) => match super::jump_to(&ctx.engine, &jump.device_id).await {
            Ok(()) => response::Result::Status(ctx.status.borrow().to_proto()),
            Err(e) => error(jump_error_code(&e), &e.to_string()),
        },
        Command::SetRelativeMouse(set) => match super::set_relative(&ctx.engine, set.on).await {
            Ok(()) => response::Result::Status(ctx.status.borrow().to_proto()),
            Err(e) => error(relative_error_code(&e), &e.to_string()),
        },
        Command::RequestFolderAccess(_) => {
            let Some(folders) = &ctx.folders else {
                return error("unsupported", "this computer has no folders to ask for");
            };
            let round = folders.ask();
            // Answered once the status has this round, the folders being
            // asked about, not once the user has answered macOS: that may
            // take minutes, and this client's other requests and events
            // wait behind this one. Every status after this one has this
            // round, or a later one.
            let mut status = ctx.status.clone();
            let applied = status.wait_for(|s| s.folder_access.round >= round);
            match tokio::time::timeout(APPLY_TIMEOUT, applied).await {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
        Command::RequestPermission(request) => {
            let Some(permission) = request
                .permission
                .try_into()
                .ok()
                .and_then(crate::convert::permission_from_proto)
            else {
                return error("unknown_permission", "no such permission");
            };
            let round = match ctx.session.request_permission(permission) {
                Ok(round) => round,
                Err(PlatformError::Unsupported(what)) => {
                    return error("unsupported", &format!("not supported here: {what}"));
                }
                Err(e) => return error("failed", &e.to_string()),
            };
            // Answered once the status is of this request (the platform
            // poller reports within a second), whatever came of it so far;
            // not once the user answered the desktop, which may take
            // minutes.
            let mut status = ctx.status.clone();
            let applied = status.wait_for(|s| {
                s.permissions
                    .iter()
                    .any(|p| p.permission == permission && p.asked >= round)
            });
            match tokio::time::timeout(APPLY_TIMEOUT, applied).await {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
        Command::SetRelativeAuto(set) => {
            super::set_relative_auto(&ctx.directory, &ctx.engine, set.on);
            let mut status = ctx.status.clone();
            let applied = status.wait_for(|s| s.relative_auto == set.on);
            match tokio::time::timeout(APPLY_TIMEOUT, applied).await {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
        Command::SetEdgeOptions(set) => {
            if let Err(e) = super::set_edge_dwell(&ctx.directory, &ctx.engine, set.dwell_ms) {
                return error("invalid_dwell", &e.to_string());
            }
            let mut status = ctx.status.clone();
            let applied = status.wait_for(|s| s.edge_dwell_ms == set.dwell_ms);
            match tokio::time::timeout(APPLY_TIMEOUT, applied).await {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
        Command::SetSwitchModifier(set) => {
            let Ok(modifier) = SwitchModifier::try_from(set.modifier) else {
                return error("invalid_modifier", "no such modifier");
            };
            let modifier = switch_modifier_from_proto(modifier);
            super::set_switch_modifier(&ctx.directory, &ctx.engine, modifier);
            let mut status = ctx.status.clone();
            let applied = status.wait_for(|s| s.switch_modifier == modifier);
            match tokio::time::timeout(APPLY_TIMEOUT, applied).await {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
        Command::SetHotkeys(set) => {
            let keys = set.hotkeys.unwrap_or_default();
            // An app from before the relative mode hotkey leaves it out, and
            // changes the others only: it stays as it is.
            let relative = keys.relative.unwrap_or_else(|| {
                let current = ctx.status.borrow().hotkeys.relative;
                current.map(|h| h.to_string()).unwrap_or_default()
            });
            let hotkeys = match Hotkeys::parse(
                &keys.take_back,
                &keys.lock,
                &keys.jump_modifiers,
                Some(&relative),
            ) {
                Ok(hotkeys) => hotkeys,
                Err(e) => return error(e.code(), &e.to_string()),
            };
            if let Err(e) = super::set_hotkeys(&ctx.directory, &ctx.engine, hotkeys) {
                return error(e.code(), &e.to_string());
            }
            let mut status = ctx.status.clone();
            let applied = status.wait_for(|s| s.hotkeys == hotkeys);
            match tokio::time::timeout(APPLY_TIMEOUT, applied).await {
                Ok(Ok(current)) => response::Result::Status(current.to_proto()),
                _ => error("timeout", "the agent did not apply the change in time"),
            }
        }
    }
}

fn drag_error(e: &DragError) -> response::Result {
    error(e.code(), &e.to_string())
}

/// What a drag that began carries, for the app: what goes, and what
/// cannot.
fn drag_begun(drag_id: String, manifest: &Manifest) -> DragBegun {
    let (files, bytes) = manifest.totals();
    let mut failures = Vec::new();
    for item in &manifest.items {
        match &item.kind {
            ItemKind::Failed(reason) => failures.push(FailedFile {
                relative_path: item.name.clone(),
                reason: (*reason).to_owned(),
            }),
            ItemKind::Folder {
                failures: inside, ..
            } => {
                failures.extend(inside.iter().map(|f| FailedFile {
                    relative_path: f.relative.clone(),
                    reason: f.reason.to_owned(),
                }));
            }
            ItemKind::File(_) => {}
        }
    }
    failures.truncate(super::listing::MAX_LISTED_FAILURES);
    DragBegun {
        drag_id,
        items: manifest.items.iter().filter(|i| i.sendable()).count() as u32,
        files: files as u32,
        bytes,
        failures,
    }
}

fn jump_error_code(e: &JumpError) -> &'static str {
    use iw_core::router::JumpError as Refused;
    match e {
        JumpError::UnknownDevice(_) => "unknown_device",
        JumpError::NotCapturing => "not_capturing",
        JumpError::EdgeOnly => "edge_only",
        JumpError::ControlledBy(_) => "controlled",
        JumpError::Stopping => "agent_stopping",
        JumpError::Refused(Refused::SharingPaused) => "sharing_paused",
        JumpError::Refused(Refused::Locked) => "locked",
        JumpError::Refused(Refused::ButtonHeld) => "button_held",
        JumpError::Refused(Refused::NotConnected(_)) => "not_connected",
        JumpError::Refused(Refused::NotArranged(_)) => "not_arranged",
        JumpError::Refused(Refused::ScreenLocked(_)) => "screen_locked",
        JumpError::Refused(Refused::InputUnavailable(_)) => "input_unavailable",
        JumpError::Refused(Refused::PeerPaused(_)) => "peer_paused",
    }
}

/// The status after a license change, or why it was refused.
fn license_result(ctx: &IpcContext, result: Result<(), super::LicenseError>) -> response::Result {
    match result {
        Ok(()) => response::Result::Status(ctx.status.borrow().to_proto()),
        Err(e) => error(e.code(), &e.to_string()),
    }
}

/// What the app says Polar answered, as the agent keeps it; none for a
/// result with no case or an unknown refusal.
fn license_result_from_proto(
    result: Option<iw_proto::app::v1::record_license_result::Result>,
    activation_id: String,
) -> Option<super::LicenseResult> {
    use iw_proto::app::v1::LicenseRefusal;
    use iw_proto::app::v1::record_license_result::Result as Proto;
    Some(match result? {
        Proto::Activated(a) => super::LicenseResult::Activated {
            activation_id: a.activation_id,
            customer_email: a.customer_email,
            validated_at: a.validated_at,
        },
        Proto::Validated(v) => super::LicenseResult::Validated {
            activation_id,
            validated_at: v.validated_at,
            customer_email: v.customer_email,
        },
        Proto::Refused(r) => super::LicenseResult::Refused {
            refusal: match LicenseRefusal::try_from(r).ok()? {
                LicenseRefusal::SeatsFull => super::Refusal::SeatsFull,
                LicenseRefusal::Invalid => super::Refusal::Invalid,
                LicenseRefusal::Unspecified => return None,
            },
            activation_id,
        },
    })
}

fn relative_error_code(e: &RelativeError) -> &'static str {
    use iw_core::router::RelativeError as Refused;
    match e {
        RelativeError::Refused(Refused::NotControlling) => "not_controlling",
        RelativeError::Refused(Refused::NotSupported(_)) => "not_supported",
        RelativeError::Stopping => "agent_stopping",
    }
}

/// Why a request cannot be carried out: an error code and English detail.
struct Refusal {
    code: &'static str,
    detail: String,
}

impl Refusal {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

/// The address of a device discovery has seen.
fn nearby_address(ctx: &IpcContext, id: &str) -> Result<SocketAddr, Refusal> {
    let found = ctx
        .directory
        .nearby_by_id(id)
        .ok_or_else(|| Refusal::new("unknown_device", format!("no device {id:?} is nearby")))?;
    found
        .addresses
        .iter()
        .copied()
        .find(|a| reachable_from(ctx.local_addr, *a))
        .ok_or_else(|| Refusal::new("unreachable", format!("{id:?} has no reachable address")))
}

/// Accepts "host:port", "host" (default port), or a bare IP address.
async fn resolve(ctx: &IpcContext, text: &str) -> Result<SocketAddr, Refusal> {
    let text = text.trim();
    if let Ok(addr) = text.parse::<SocketAddr>() {
        return Ok(addr);
    }
    if let Ok(ip) = text.trim_matches(['[', ']']).parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, DEFAULT_PORT));
    }
    let query = if text.contains(':') {
        text.to_owned()
    } else {
        format!("{text}:{DEFAULT_PORT}")
    };
    let invalid = || Refusal::new("invalid_address", format!("cannot resolve {text:?}"));
    let mut addrs = tokio::net::lookup_host(query)
        .await
        .map_err(|_| invalid())?;
    addrs
        .find(|a| reachable_from(ctx.local_addr, *a))
        .ok_or_else(invalid)
}
