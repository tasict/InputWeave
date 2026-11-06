//! Peer connections: dialing, accepting and running sessions.
//!
//! Between two trusted devices only the one with the smaller device id
//! dials; the other refuses sessions dialed the other way, so there is never
//! more than one session per pair. Which devices are trusted, and where to
//! reach them, comes from the [`Directory`], which pairing and discovery
//! keep up to date while the agent runs.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iw_core::layout::DeviceId;
use iw_core::safety::Liveness;
use iw_net::{Fingerprint, IncomingPeer, NetError, PeerEndpoint, Purpose, Session, close};
use iw_proto::peer::v1::{Control, Heartbeat, Input, control};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::{JoinHandle, JoinSet};
use tracing::{debug, info, warn};

use super::directory::{Directory, Origin, Peer};
use super::engine::{EngineEvent, Outbound, PeerMsg};
use super::files::Files;
use super::lazy_clipboard::PendingPulls;
use super::lazy_clipboard::is_pull;
use super::pairing::Pairing;
use super::this_device::ThisDevice;
use super::transfer::{self, Incoming};

/// How often each side sends a heartbeat on the control stream.
pub(crate) const HEARTBEAT: Duration = Duration::from_millis(250);
/// A peer not heard from for this long is considered gone.
pub(crate) const PEER_TIMEOUT: Duration = Duration::from_secs(1);
/// How often a session tells the engine its round-trip time, which clients
/// only show.
const RTT_REPORT: Duration = Duration::from_secs(5);

const BACKOFF_MIN: Duration = Duration::from_millis(200);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// Pulled clipboards read from one device at once; more wait.
const PULLED_AT_ONCE: usize = 2;

/// What clipboards arriving on transfer streams may hold.
#[derive(Clone, Debug)]
pub(crate) struct ClipboardLimits {
    /// Larger clipboards are refused.
    pub(crate) max: usize,
    /// Pulls the engine waits on: only their answers may be larger.
    pub(crate) pulls: PendingPulls,
}

pub(crate) struct PeerContext {
    pub(crate) endpoint: Arc<PeerEndpoint>,
    pub(crate) local_addr: SocketAddr,
    pub(crate) engine: mpsc::UnboundedSender<EngineEvent>,
    pub(crate) directory: Arc<Directory>,
    pub(crate) pairing: Pairing,
    pub(crate) this: Arc<ThisDevice>,
    pub(crate) clipboard: ClipboardLimits,
    pub(crate) files: Arc<Files>,
    next_session: AtomicU64,
}

impl PeerContext {
    pub(crate) fn new(
        endpoint: Arc<PeerEndpoint>,
        engine: mpsc::UnboundedSender<EngineEvent>,
        directory: Arc<Directory>,
        pairing: Pairing,
        this: Arc<ThisDevice>,
        clipboard: ClipboardLimits,
        files: Arc<Files>,
    ) -> Result<Self, NetError> {
        Ok(Self {
            local_addr: endpoint.local_addr()?,
            endpoint,
            engine,
            directory,
            pairing,
            this,
            clipboard,
            files,
            next_session: AtomicU64::new(1),
        })
    }

    /// Whether this device dials `peer` (rather than waiting for it to dial).
    fn dials(&self, peer: &str) -> bool {
        self.this.id.as_str() < peer
    }
}

pub(crate) async fn accept_loop(ctx: Arc<PeerContext>) {
    let mut handlers = JoinSet::new();
    while let Some(incoming) = ctx.endpoint.accept().await {
        let ctx = ctx.clone();
        handlers.spawn(async move {
            if let Err(e) = handle_incoming(&ctx, incoming).await {
                log_failure("incoming connection", &e);
            }
        });
        while handlers.try_join_next().is_some() {}
    }
}

async fn handle_incoming(ctx: &PeerContext, incoming: IncomingPeer) -> Result<(), NetError> {
    let accepted = incoming
        .finish(|fp| ctx.directory.peer_by_fingerprint(fp).is_some())
        .await?;
    if accepted.purpose == Purpose::Pairing {
        ctx.pairing.incoming(accepted);
        return Ok(());
    }
    // The device may have been unpaired since the handshake.
    let Some(peer) = ctx.directory.peer_by_fingerprint(&accepted.peer) else {
        accepted
            .connection
            .close(close::UNTRUSTED.into(), b"device is not paired");
        return Err(NetError::Untrusted(accepted.peer));
    };
    if ctx.dials(&peer.id) {
        accepted.connection.close(
            close::DUPLICATE.into(),
            b"the device with the smaller id dials",
        );
        return Ok(());
    }
    let session = Session::accept(accepted.connection, accepted.peer, ctx.this.hello()).await?;
    run_checked(ctx, session, &peer).await
}

/// Keeps one dial loop running for every trusted device this one dials.
/// A loop ends by itself once its device is no longer trusted; the rest end
/// with this task.
pub(crate) async fn dial_all(ctx: Arc<PeerContext>) {
    let mut peers = ctx.directory.watch_peers();
    let mut loops: HashMap<String, AbortOnDrop> = HashMap::new();
    loop {
        let current = peers.borrow_and_update().list.clone();
        loops.retain(|_, task| !task.0.is_finished());
        for peer in current.iter().filter(|p| ctx.dials(&p.id)) {
            loops.entry(peer.id.clone()).or_insert_with(|| {
                AbortOnDrop(tokio::spawn(dial_loop(ctx.clone(), peer.id.clone())))
            });
        }
        if peers.changed().await.is_err() {
            break;
        }
    }
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Dials `id` until it is no longer trusted, trying each address the
/// directory knows in turn and waiting for one when it knows none.
async fn dial_loop(ctx: Arc<PeerContext>, id: String) {
    let mut peers = ctx.directory.watch_peers();
    let mut nearby = ctx.directory.watch_nearby();
    let mut backoff = BACKOFF_MIN;
    let mut attempt = 0usize;
    loop {
        peers.mark_unchanged();
        nearby.mark_unchanged();
        let Some(peer) = ctx.directory.peer(&id) else {
            return;
        };
        let addresses = ctx.directory.addresses(&peer, ctx.local_addr);
        if addresses.is_empty() {
            debug!(device = %id, "no address known; waiting for discovery");
            tokio::select! {
                _ = peers.changed() => {}
                _ = nearby.changed() => {}
            }
            continue;
        }
        let addr = addresses[attempt % addresses.len()];
        attempt += 1;
        let dialing = async {
            let connection = ctx.endpoint.connect(addr, peer.fingerprint).await?;
            Session::open(connection, peer.fingerprint, ctx.this.hello()).await
        };
        match dialing.await {
            Ok(session) => {
                backoff = BACKOFF_MIN;
                attempt = 0;
                if peer.origin == Origin::Paired {
                    ctx.directory.reached(&id, addr);
                }
                if let Err(e) = run_checked(&ctx, session, &peer).await {
                    log_failure("session", &e);
                }
            }
            Err(e) => log_failure("dial", &e),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

fn log_failure(what: &str, e: &NetError) {
    match e {
        NetError::Untrusted(_) | NetError::Incompatible { .. } => {
            warn!(error = %e, "{what} failed")
        }
        _ => debug!(error = %e, "{what} failed"),
    }
}

/// Runs a session after checking that the device id in its Hello matches the
/// trusted device whose certificate it presented.
async fn run_checked(ctx: &PeerContext, session: Session, peer: &Peer) -> Result<(), NetError> {
    if session.info.hello.device_id != peer.id {
        session.info.close(
            close::UNTRUSTED,
            b"device id does not match its certificate",
        );
        return Err(NetError::Protocol(
            "device id does not match its certificate",
        ));
    }
    let name = &session.info.hello.device_name;
    // Its own name: one this device's user gave it stays as it is.
    if peer.origin == Origin::Paired
        && *name != peer.reported_name
        && iw_net::is_valid_device_name(name)
    {
        ctx.directory.renamed(&peer.id, name);
    }
    run_session(ctx, session, DeviceId::new(&peer.id), peer.fingerprint).await;
    Ok(())
}

/// Pumps one session until it ends, then reports it down. It ends when the
/// connection closes, a stream fails, or the peer misses heartbeats for
/// `PEER_TIMEOUT`.
async fn run_session(
    ctx: &PeerContext,
    session: Session,
    device: DeviceId,
    fingerprint: Fingerprint,
) {
    let id = ctx.next_session.fetch_add(1, Ordering::Relaxed);
    let Session {
        info,
        mut control_tx,
        mut control_rx,
    } = session;
    let (tx, mut rx) = mpsc::unbounded_channel();
    if ctx
        .engine
        .send(EngineEvent::PeerUp {
            device: device.clone(),
            fingerprint,
            session: id,
            version: info.version,
            link: tx,
        })
        .is_err()
    {
        return;
    }
    info!(%device, version = info.version, "session established");
    ctx.files.session_up(&device, &info);

    let liveness = Arc::new(Mutex::new(Liveness::new(Instant::now(), PEER_TIMEOUT)));
    let seen = {
        let liveness = liveness.clone();
        move || liveness.lock().unwrap().seen(Instant::now())
    };
    let forward = {
        let engine = ctx.engine.clone();
        let device = device.clone();
        move |msg: PeerMsg| {
            engine
                .send(EngineEvent::FromPeer {
                    device: device.clone(),
                    session: id,
                    msg,
                })
                .is_ok()
        }
    };

    let mut tasks = JoinSet::new();
    tasks.spawn({
        let (seen, forward) = (seen.clone(), forward.clone());
        async move {
            while let Ok(Some(msg)) = control_rx.recv::<Control>().await {
                seen();
                let heartbeat = matches!(msg.msg, Some(control::Msg::Heartbeat(_)));
                if !heartbeat && !forward(PeerMsg::Control(msg)) {
                    break;
                }
            }
        }
    });
    tasks.spawn({
        let (info, seen, forward) = (info.clone(), seen.clone(), forward.clone());
        async move {
            while let Ok(mut stream) = info.accept_input().await {
                while let Ok(Some(msg)) = stream.recv::<Input>().await {
                    seen();
                    if !forward(PeerMsg::Input(msg)) {
                        return;
                    }
                }
            }
        }
    });
    tasks.spawn({
        let (info, forward, max) = (info.clone(), forward.clone(), ctx.clipboard.max);
        let (files, device, pulls) = (
            ctx.files.clone(),
            device.clone(),
            ctx.clipboard.pulls.clone(),
        );
        // Pulled clipboards being read, a few at a time.
        let pulled = Arc::new(Semaphore::new(PULLED_AT_ONCE));
        async move {
            // Headers are read in the order the streams came, so files
            // start in order, and so are clipboards sent whole (at most
            // the eager limit). A pulled clipboard, up to the lazy limit,
            // is read beside them instead of holding them up.
            while let Ok((tx, mut rx)) = info.accept_transfer(transfer::FRAME_LIMIT).await {
                let header = match transfer::receive(&mut rx).await {
                    Ok(Incoming::File(offer)) => {
                        files.receive(device.clone(), offer, tx, rx);
                        continue;
                    }
                    Ok(Incoming::Clipboard(header)) => header,
                    Err(e) => {
                        debug!(error = %e, "transfer refused");
                        continue;
                    }
                };
                let id = header.request_id;
                let expected = is_pull(id) && pulls.expects(id, &device);
                if let Err(e) =
                    transfer::check_clipboard(&header, max, |id| pulls.expects(id, &device))
                {
                    debug!(error = %e, "clipboard refused");
                    if expected && !forward(PeerMsg::PullBroken(id)) {
                        return;
                    }
                    continue;
                }
                if !expected {
                    match transfer::read_clipboard(&mut rx, header).await {
                        Ok(clipboard) => {
                            if !forward(PeerMsg::Clipboard(clipboard)) {
                                return;
                            }
                        }
                        Err(e) => debug!(error = %e, "clipboard transfer failed"),
                    }
                    continue;
                }
                let (forward, pulled) = (forward.clone(), pulled.clone());
                tokio::spawn(async move {
                    let Ok(_reading) = pulled.acquire_owned().await else {
                        return;
                    };
                    match transfer::read_clipboard(&mut rx, header).await {
                        Ok(clipboard) => forward(PeerMsg::Clipboard(clipboard)),
                        Err(e) => {
                            // A stream given up mid-way is reset as it is
                            // dropped; the pull fails.
                            debug!(error = %e, "pulled clipboard transfer failed");
                            forward(PeerMsg::PullBroken(id))
                        }
                    };
                });
            }
        }
    });
    tasks.spawn({
        let (info, seen, forward) = (info.clone(), seen.clone(), forward.clone());
        async move {
            loop {
                match info.recv_datagram().await {
                    Ok(msg) => {
                        seen();
                        if !forward(PeerMsg::Datagram(msg)) {
                            break;
                        }
                    }
                    Err(NetError::Decode(_)) => continue,
                    Err(_) => break,
                }
            }
        }
    });
    tasks.spawn({
        let (info, liveness) = (info.clone(), liveness.clone());
        let (engine, device, files) = (ctx.engine.clone(), device.clone(), ctx.files.clone());
        async move {
            let mut ticks = tokio::time::interval(HEARTBEAT);
            let mut seq = 0;
            let mut input_tx = None;
            let mut rtt_reported: Option<Instant> = None;
            loop {
                tokio::select! {
                    _ = ticks.tick() => {
                        let now = Instant::now();
                        if liveness.lock().unwrap().is_expired(now) {
                            warn!("peer missed its heartbeats");
                            info.close(close::TIMEOUT, b"heartbeat timeout");
                            break;
                        }
                        seq += 1;
                        let heartbeat = Control { msg: Some(control::Msg::Heartbeat(Heartbeat { seq })) };
                        if control_tx.send(&heartbeat).await.is_err() {
                            break;
                        }
                        // The first tick comes at once, when the handshake
                        // has already measured it and no file is sent yet.
                        let rtt = info.connection.rtt();
                        files.sample_rtt(&device, rtt);
                        if rtt_reported.is_none_or(|at| now.duration_since(at) >= RTT_REPORT) {
                            rtt_reported = Some(now);
                            let rtt = EngineEvent::Rtt {
                                device: device.clone(),
                                session: id,
                                rtt,
                            };
                            if engine.send(rtt).is_err() {
                                break;
                            }
                        }
                    }
                    out = rx.recv() => {
                        let sent = match out {
                            None => break,
                            Some(Outbound::Control(msg)) => control_tx.send(&msg).await.is_ok(),
                            Some(Outbound::Input(msg)) => {
                                if input_tx.is_none() {
                                    input_tx = info.open_input().await.ok();
                                }
                                match input_tx.as_mut() {
                                    Some(stream) => stream.send(&msg).await.is_ok(),
                                    None => false,
                                }
                            }
                            Some(Outbound::Datagram(msg)) => {
                                // Datagrams are best effort; a full buffer just drops one.
                                let _ = info.send_datagram(&msg);
                                true
                            }
                            Some(Outbound::Clipboard(clipboard)) => {
                                // On a stream of its own, so input goes on meanwhile.
                                let info = info.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = transfer::send_clipboard(&info, &clipboard).await {
                                        debug!(error = %e, "clipboard transfer failed");
                                    }
                                });
                                true
                            }
                        };
                        if !sent {
                            break;
                        }
                    }
                }
            }
        }
    });

    tasks.join_next().await;
    info.close(close::NORMAL, b"session ended");
    ctx.files.session_down(&device, &info);
    tasks.shutdown().await;
    info!(%device, "session ended");
    let _ = ctx.engine.send(EngineEvent::PeerDown {
        device,
        session: id,
    });
}
