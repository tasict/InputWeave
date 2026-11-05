//! Runs one pairing at a time, in either role, on behalf of IPC clients.
//!
//! As initiator it connects to a device, which then shows a code, and tries
//! the codes the user types. As responder it shows a code (through the
//! pairing status that IPC clients watch) when another device asks to pair.
//! A successful pairing is remembered in the [`Directory`], which is what
//! makes the dialer and the accept loop trust the new device.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use iw_net::discovery::os_name;
use iw_net::pairing::{CODE_LIFETIME, MAX_ATTEMPTS};
use iw_net::{
    Accepted, NetError, Paired, PairingCode, PairingError, PairingInitiator, PairingResponder,
    PeerEndpoint, close,
};
use iw_proto::peer::v1::{Hello, Os, PairResult};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;
use tracing::{debug, info};

use super::directory::Directory;
use super::this_device::ThisDevice;
use crate::store::{PairedDevice, Side};

/// After a pairing in which any wrong code was tried, further requests are
/// turned away for this long. That caps how fast codes can be guessed, even
/// by an initiator that reconnects before using up its attempts.
const COOLDOWN: Duration = Duration::from_secs(30);

/// Pairing requests turned away concurrently; beyond this, connections are
/// closed without an explanation.
const MAX_REFUSALS: usize = 8;

/// The other device, as it introduced itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingPeer {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingStatus {
    Idle,
    /// Initiator: reaching the other device.
    Connecting {
        address: SocketAddr,
    },
    /// Initiator: the other device shows a code for the user to type here.
    /// Below [`MAX_ATTEMPTS`], the previous code was wrong.
    EnterCode {
        peer: PairingPeer,
        attempts_left: u32,
    },
    /// Initiator: checking a code.
    Verifying {
        peer: PairingPeer,
    },
    /// Responder: show `code` until `expires_at`.
    ShowCode {
        peer: PairingPeer,
        code: PairingCode,
        expires_at: SystemTime,
    },
    Paired {
        peer: PairingPeer,
    },
    Failed {
        peer: Option<PairingPeer>,
        reason: Failure,
    },
}

/// Why a pairing did not succeed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    TooManyAttempts,
    Expired,
    /// The responder's user declined.
    Rejected,
    /// The responder does not accept new pairings.
    NotAllowed,
    /// A pairing is already running on one side.
    Busy,
    IdConflict,
    Incompatible,
    Unreachable,
    Cancelled,
    Protocol,
}

impl Failure {
    /// Stable identifier for IPC clients.
    pub fn code(self) -> &'static str {
        match self {
            Failure::TooManyAttempts => "too_many_attempts",
            Failure::Expired => "expired",
            Failure::Rejected => "rejected",
            Failure::NotAllowed => "not_allowed",
            Failure::Busy => "busy",
            Failure::IdConflict => "id_conflict",
            Failure::Incompatible => "incompatible",
            Failure::Unreachable => "unreachable",
            Failure::Cancelled => "cancelled",
            Failure::Protocol => "protocol",
        }
    }

    fn of(e: &PairingError) -> Self {
        match e {
            PairingError::Ended(result) => match result {
                PairResult::TooManyAttempts | PairResult::WrongCode => Failure::TooManyAttempts,
                PairResult::Expired => Failure::Expired,
                PairResult::Rejected => Failure::Rejected,
                PairResult::NotAllowed => Failure::NotAllowed,
                PairResult::Busy => Failure::Busy,
                PairResult::IdConflict => Failure::IdConflict,
                PairResult::Incompatible => Failure::Incompatible,
                PairResult::Cancelled => Failure::Cancelled,
                PairResult::Paired | PairResult::Unspecified => Failure::Protocol,
            },
            PairingError::Timeout
            | PairingError::Net(NetError::Connect(_) | NetError::Connection(_) | NetError::Io(_)) => {
                Failure::Unreachable
            }
            PairingError::WrongCode { .. } | PairingError::Net(_) => Failure::Protocol,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("no pairing is waiting for a code")]
pub struct NotWaiting;

enum Command {
    Start {
        address: SocketAddr,
        reply: oneshot::Sender<Result<(), Failure>>,
    },
    Submit {
        code: PairingCode,
        reply: oneshot::Sender<PairingStatus>,
    },
    Cancel,
    Incoming(Accepted),
}

enum Input {
    Code(PairingCode, oneshot::Sender<PairingStatus>),
    Cancel,
}

/// The agent's handle on the pairing task.
#[derive(Clone)]
pub(crate) struct Pairing {
    commands: mpsc::UnboundedSender<Command>,
    status: watch::Receiver<PairingStatus>,
}

pub(crate) struct PairingContext {
    endpoint: Arc<PeerEndpoint>,
    directory: Arc<Directory>,
    this: Arc<ThisDevice>,
    allow: bool,
    /// Requests are turned away until then.
    cooldown_until: Mutex<Option<Instant>>,
}

impl PairingContext {
    /// `allow` says whether other devices may ask to pair with this one.
    pub(crate) fn new(
        endpoint: Arc<PeerEndpoint>,
        directory: Arc<Directory>,
        this: Arc<ThisDevice>,
        allow: bool,
    ) -> Self {
        Self {
            endpoint,
            directory,
            this,
            allow,
            cooldown_until: Mutex::new(None),
        }
    }

    fn cooling_down(&self) -> bool {
        self.cooldown_until
            .lock()
            .unwrap()
            .is_some_and(|t| Instant::now() < t)
    }

    /// Turns requests away for a while if any code was guessed wrong.
    fn after_guesses(&self, wrong: u32) {
        if wrong > 0 {
            *self.cooldown_until.lock().unwrap() = Some(Instant::now() + COOLDOWN);
        }
    }
}

/// Starts the pairing task in `tasks`.
pub(crate) fn spawn(ctx: PairingContext, tasks: &mut JoinSet<()>) -> Pairing {
    let (commands, rx) = mpsc::unbounded_channel();
    let (status_tx, status) = watch::channel(PairingStatus::Idle);
    tasks.spawn(coordinate(Arc::new(ctx), rx, status_tx));
    Pairing { commands, status }
}

impl Pairing {
    pub(crate) fn status(&self) -> PairingStatus {
        self.status.borrow().clone()
    }

    pub(crate) fn watch(&self) -> watch::Receiver<PairingStatus> {
        self.status.clone()
    }

    /// Starts pairing with the device at `address`. Fails at once only if a
    /// pairing is already running; how it goes is in the status.
    pub(crate) async fn start(&self, address: SocketAddr) -> Result<(), Failure> {
        let (reply, rx) = oneshot::channel();
        let _ = self.commands.send(Command::Start { address, reply });
        rx.await.unwrap_or(Err(Failure::Cancelled))
    }

    /// Tries a code and returns the status it led to.
    pub(crate) async fn submit(&self, code: PairingCode) -> Result<PairingStatus, NotWaiting> {
        let (reply, rx) = oneshot::channel();
        let _ = self.commands.send(Command::Submit { code, reply });
        rx.await.map_err(|_| NotWaiting)
    }

    /// Cancels the pairing started here, or declines the one whose code is
    /// shown here.
    pub(crate) fn cancel(&self) {
        let _ = self.commands.send(Command::Cancel);
    }

    /// Hands over a connection that arrived with the pairing ALPN.
    pub(crate) fn incoming(&self, accepted: Accepted) {
        let _ = self.commands.send(Command::Incoming(accepted));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Initiator,
    Responder,
}

struct Active {
    role: Role,
    inputs: mpsc::UnboundedSender<Input>,
    task: JoinHandle<()>,
    /// Set just before the task publishes its final state.
    done: Arc<AtomicBool>,
}

/// How a pairing task publishes its state. The final state goes through
/// [`Progress::finish`], which first marks the task done, so whoever sees
/// that state can start the next pairing straight away.
struct Progress {
    status: watch::Sender<PairingStatus>,
    done: Arc<AtomicBool>,
}

impl Progress {
    fn update(&self, state: PairingStatus) {
        self.status.send_replace(state);
    }

    fn finish(&self, state: PairingStatus) {
        self.done.store(true, Ordering::SeqCst);
        self.status.send_replace(state);
    }
}

/// Forgets a task that has published its final state, waiting the moment
/// it takes to return. Afterwards `active` is only set while a pairing is
/// in progress.
async fn reap(active: &mut Option<Active>) {
    if let Some(a) = active.as_mut()
        && (a.done.load(Ordering::SeqCst) || a.task.is_finished())
    {
        let _ = (&mut a.task).await;
        *active = None;
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn coordinate(
    ctx: Arc<PairingContext>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    status: watch::Sender<PairingStatus>,
) {
    let mut active: Option<Active> = None;
    let mut refusals = JoinSet::new();
    loop {
        while refusals.try_join_next().is_some() {}
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                match command {
                    Command::Start { address, reply } => {
                        reap(&mut active).await;
                        if active.is_some() {
                            let _ = reply.send(Err(Failure::Busy));
                            continue;
                        }
                        let (inputs, rx) = mpsc::unbounded_channel();
                        status.send_replace(PairingStatus::Connecting { address });
                        let done = Arc::new(AtomicBool::new(false));
                        let progress = Progress { status: status.clone(), done: done.clone() };
                        let task = tokio::spawn(initiate(ctx.clone(), address, rx, progress));
                        active = Some(Active { role: Role::Initiator, inputs, task, done });
                        let _ = reply.send(Ok(()));
                    }
                    Command::Submit { code, reply } => {
                        // Dropping `reply` tells the caller nobody is waiting.
                        if let Some(a) = &active
                            && a.role == Role::Initiator
                        {
                            let _ = a.inputs.send(Input::Code(code, reply));
                        }
                    }
                    Command::Cancel => {
                        if let Some(a) = &active {
                            let _ = a.inputs.send(Input::Cancel);
                        }
                    }
                    Command::Incoming(accepted) => {
                        reap(&mut active).await;
                        let refusal = if !ctx.allow {
                            Some(PairResult::NotAllowed)
                        } else if active.is_some() || ctx.cooling_down() {
                            Some(PairResult::Busy)
                        } else {
                            None
                        };
                        match refusal {
                            Some(_) if refusals.len() >= MAX_REFUSALS => {
                                accepted.connection.close(close::NORMAL.into(), b"busy");
                            }
                            Some(result) => {
                                refusals.spawn(refuse(ctx.clone(), accepted, result));
                            }
                            None => {
                                let (inputs, rx) = mpsc::unbounded_channel();
                                let done = Arc::new(AtomicBool::new(false));
                                let progress = Progress { status: status.clone(), done: done.clone() };
                                let task = tokio::spawn(respond(ctx.clone(), accepted, rx, progress));
                                active = Some(Active { role: Role::Responder, inputs, task, done });
                            }
                        }
                    }
                }
            }
            _ = finished(&mut active) => active = None,
        }
    }
}

async fn finished(active: &mut Option<Active>) {
    match active {
        Some(a) => {
            let _ = (&mut a.task).await;
        }
        None => std::future::pending().await,
    }
}

fn peer_of(hello: &Hello) -> PairingPeer {
    PairingPeer {
        id: hello.device_id.clone(),
        name: hello.device_name.clone(),
    }
}

async fn initiate(
    ctx: Arc<PairingContext>,
    address: SocketAddr,
    mut inputs: mpsc::UnboundedReceiver<Input>,
    status: Progress,
) {
    let connecting = PairingInitiator::connect(&ctx.endpoint, address, ctx.this.hello());
    let mut initiator = tokio::select! {
        result = connecting => match result {
            Ok(initiator) => initiator,
            Err(e) => {
                debug!(%address, error = %e, "pairing failed");
                let reason = Failure::of(&e);
                status.finish(PairingStatus::Failed { peer: None, reason });
                return;
            }
        },
        _ = cancelled(&mut inputs) => {
            status.finish(PairingStatus::Failed { peer: None, reason: Failure::Cancelled });
            return;
        }
    };
    let peer = peer_of(initiator.peer_hello());
    if ctx
        .directory
        .id_conflicts(&peer.id, &initiator.peer_fingerprint())
    {
        initiator.cancel().await;
        let reason = Failure::IdConflict;
        status.finish(PairingStatus::Failed {
            peer: Some(peer),
            reason,
        });
        return;
    }
    status.update(PairingStatus::EnterCode {
        peer: peer.clone(),
        attempts_left: MAX_ATTEMPTS,
    });
    loop {
        tokio::select! {
            input = inputs.recv() => match input {
                Some(Input::Code(code, reply)) => {
                    status.update(PairingStatus::Verifying { peer: peer.clone() });
                    let next = match initiator.try_code(&code).await {
                        Ok(paired) => {
                            remember(&ctx, &paired, Side::Right);
                            PairingStatus::Paired { peer: peer.clone() }
                        }
                        Err(PairingError::WrongCode { attempts_left }) => {
                            PairingStatus::EnterCode { peer: peer.clone(), attempts_left }
                        }
                        Err(e) => {
                            debug!(error = %e, "pairing failed");
                            PairingStatus::Failed { peer: Some(peer.clone()), reason: Failure::of(&e) }
                        }
                    };
                    let more = matches!(next, PairingStatus::EnterCode { .. });
                    if more {
                        status.update(next.clone());
                    } else {
                        status.finish(next.clone());
                    }
                    let _ = reply.send(next);
                    if !more {
                        return;
                    }
                }
                Some(Input::Cancel) | None => {
                    initiator.cancel().await;
                    status.finish(PairingStatus::Failed { peer: Some(peer), reason: Failure::Cancelled });
                    return;
                }
            },
            e = initiator.ended() => {
                debug!(error = %e, "the other device ended pairing");
                status.finish(PairingStatus::Failed { peer: Some(peer), reason: Failure::of(&e) });
                return;
            }
        }
    }
}

async fn respond(
    ctx: Arc<PairingContext>,
    accepted: Accepted,
    mut inputs: mpsc::UnboundedReceiver<Input>,
    status: Progress,
) {
    let own = ctx.endpoint.fingerprint();
    let mut responder = match PairingResponder::accept(accepted, own, ctx.this.hello()).await {
        Ok(responder) => responder,
        Err(e) => {
            // Nothing was shown, so there is nothing to report.
            debug!(error = %e, "pairing request failed");
            return;
        }
    };
    let peer = peer_of(responder.peer_hello());
    if ctx
        .directory
        .id_conflicts(&peer.id, &responder.peer_fingerprint())
    {
        info!(device = %peer.name, id = %peer.id, "refusing to pair: device id in use");
        responder.refuse(PairResult::IdConflict).await;
        return;
    }
    let code = PairingCode::generate();
    let deadline = Instant::now() + CODE_LIFETIME;
    info!(device = %peer.name, "showing a pairing code");
    status.update(PairingStatus::ShowCode {
        peer: peer.clone(),
        code: code.clone(),
        expires_at: SystemTime::now() + CODE_LIFETIME,
    });
    let result = tokio::select! {
        result = responder.run(&code, deadline) => result,
        _ = cancelled(&mut inputs) => {
            ctx.after_guesses(responder.wrong_attempts());
            responder.refuse(PairResult::Rejected).await;
            status.finish(PairingStatus::Failed { peer: Some(peer), reason: Failure::Rejected });
            return;
        }
    };
    match result {
        Ok(paired) => {
            remember(&ctx, &paired, Side::Left);
            status.finish(PairingStatus::Paired { peer });
        }
        Err(e) => {
            debug!(error = %e, "pairing failed");
            ctx.after_guesses(responder.wrong_attempts());
            status.finish(PairingStatus::Failed {
                peer: Some(peer),
                reason: Failure::of(&e),
            });
        }
    }
}

/// Completes when the user cancels or the coordinator goes away. Codes are
/// only ever sent to an initiator, so any that arrive here are dropped.
async fn cancelled(inputs: &mut mpsc::UnboundedReceiver<Input>) {
    while let Some(Input::Code(..)) = inputs.recv().await {}
}

async fn refuse(ctx: Arc<PairingContext>, accepted: Accepted, result: PairResult) {
    let own = ctx.endpoint.fingerprint();
    if let Ok(responder) = PairingResponder::accept(accepted, own, ctx.this.hello()).await {
        debug!(device = %responder.peer_hello().device_name, ?result, "refusing to pair");
        responder.refuse(result).await;
    }
}

/// Trusts the newly paired device. ⌘ and Ctrl are swapped by default when
/// exactly one of the two devices is a Mac.
fn remember(ctx: &PairingContext, paired: &Paired, side: Side) {
    let ours = Os::try_from(ctx.this.hello().os).unwrap_or_default();
    let theirs = Os::try_from(paired.hello.os).unwrap_or_default();
    let known = ours != Os::Unspecified && theirs != Os::Unspecified;
    info!(
        device = %paired.hello.device_name,
        id = %paired.hello.device_id,
        fingerprint = %paired.fingerprint,
        "paired"
    );
    ctx.directory.add_paired(PairedDevice {
        id: paired.hello.device_id.clone(),
        name: paired.hello.device_name.clone(),
        fingerprint: paired.fingerprint,
        os: os_name(theirs).to_owned(),
        last_address: Some(paired.address),
        swap_gui_and_control: known && (ours == Os::Macos) != (theirs == Os::Macos),
        side,
    });
}
