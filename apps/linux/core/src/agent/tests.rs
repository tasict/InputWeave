//! The client against a fake agent: a Unix socket that speaks the protocol,
//! as the Windows app's tests use `FakeAgent`.

use std::os::unix::net::UnixListener;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::task::{Context, Poll, Wake, Waker};

use iw_proto::app::v1::{
    AgentHello, DragArrived, Error as ProtoError, PairingPhase, PairingState, ScreensIdentified,
    SendFiles, SetSharing, Status, SubmitPairingCode, Unpair,
};

use super::*;

/// Runs a future to its end on this thread.
fn block_on<F: Future>(future: F) -> F::Output {
    struct Unpark(thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        thread::park_timeout(Duration::from_millis(50));
    }
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// How the fake answers a command, given the connection's number.
type Answers = dyn Fn(usize, &client_message::Command) -> Option<response::Result> + Send + Sync;

/// A fake agent on a socket in a temporary folder.
struct Fake {
    _dir: tempfile::TempDir,
    path: PathBuf,
    received: Arc<Mutex<Vec<client_message::Command>>>,
    connection: Arc<Mutex<Option<UnixStream>>>,
    connections: Arc<AtomicUsize>,
    /// The connection that no longer reads, by number; 0 for none.
    stalled: Arc<AtomicUsize>,
}

impl Fake {
    fn start(answer: Arc<Answers>) -> Fake {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let connection = Arc::new(Mutex::new(None));
        let connections = Arc::new(AtomicUsize::new(0));
        let stalled = Arc::new(AtomicUsize::new(0));
        let (r, c, n, s) = (
            received.clone(),
            connection.clone(),
            connections.clone(),
            stalled.clone(),
        );
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let number = n.fetch_add(1, Ordering::SeqCst) + 1;
                *c.lock().unwrap() = Some(stream.try_clone().unwrap());
                let (r, answer, s) = (r.clone(), answer.clone(), s.clone());
                thread::spawn(move || serve(stream, number, r, answer, s));
            }
        });
        Fake {
            _dir: dir,
            path,
            received,
            connection,
            connections,
            stalled,
        }
    }

    fn standard() -> Fake {
        Fake::start(Arc::new(|_, command| standard_answer(command)))
    }

    fn agent(&self) -> Agent {
        let path = self.path.clone();
        Agent::start_timed(
            Box::new(move || UnixStream::connect(&path)),
            "linux-ui/test",
            Box::new(|| {}),
            Duration::from_millis(500),
        )
    }

    fn push(&self, event: Event) {
        let message = AgentMessage {
            msg: Some(agent_message::Msg::Event(event)),
        };
        let mut buf = BytesMut::new();
        frame::encode(&message, &mut buf).unwrap();
        let mut guard = self.connection.lock().unwrap();
        guard.as_mut().unwrap().write_all(&buf).unwrap();
    }

    /// Stops reading the current connection, as an agent that hangs: the
    /// client's writes pile up until they time out.
    fn stop_reading(&self) {
        self.stalled
            .store(self.connections.load(Ordering::SeqCst), Ordering::SeqCst);
    }

    fn hang_up(&self) {
        if let Some(stream) = self.connection.lock().unwrap().take() {
            stream.shutdown(std::net::Shutdown::Both).unwrap();
        }
    }

    fn received(&self) -> Vec<client_message::Command> {
        self.received.lock().unwrap().clone()
    }
}

fn standard_answer(command: &client_message::Command) -> Option<response::Result> {
    use client_message::Command as C;
    Some(match command {
        C::Hello(_) => response::Result::Hello(AgentHello {
            ipc_version: IPC_VERSION,
            agent_version: "9.9.9".into(),
            agent_digest: "abc123".into(),
        }),
        C::GetStatus(_) => response::Result::Status(Status {
            this_device_id: "me".into(),
            this_device_name: "Fake".into(),
            sharing: true,
            ..Default::default()
        }),
        C::GetPairing(_) => response::Result::Pairing(Default::default()),
        C::GetNearby(_) => response::Result::Nearby(Default::default()),
        C::GetArrangement(_) => response::Result::Arrangement(Default::default()),
        C::GetTransfers(_) => response::Result::Transfers(Default::default()),
        C::SetSharing(SetSharing { enabled }) => response::Result::Status(Status {
            this_device_id: "me".into(),
            this_device_name: "Fake".into(),
            sharing: *enabled,
            ..Default::default()
        }),
        C::Unpair(_) => response::Result::Error(ProtoError {
            code: "unknown_device".into(),
            detail: "no such device".into(),
        }),
        C::DeclineDrag(_) => response::Result::Status(Status {
            this_device_id: "me".into(),
            this_device_name: "Fake".into(),
            sharing: true,
            ..Default::default()
        }),
        _ => return None,
    })
}

fn serve(
    mut stream: UnixStream,
    number: usize,
    received: Arc<Mutex<Vec<client_message::Command>>>,
    answer: Arc<Answers>,
    stalled: Arc<AtomicUsize>,
) {
    let mut buf = BytesMut::new();
    let mut chunk = [0u8; 4096];
    loop {
        if stalled.load(Ordering::SeqCst) == number {
            // Hangs, holding the connection open, until the test ends.
            thread::sleep(Duration::from_secs(3600));
        }
        while let Ok(Some(message)) = frame::decode::<ClientMessage>(&mut buf) {
            let Some(command) = message.command else {
                continue;
            };
            received.lock().unwrap().push(command.clone());
            if let Some(result) = answer(number, &command) {
                let reply = AgentMessage {
                    msg: Some(agent_message::Msg::Response(Response {
                        request_id: message.request_id,
                        result: Some(result),
                    })),
                };
                let mut out = BytesMut::new();
                frame::encode(&reply, &mut out).unwrap();
                if stream.write_all(&out).is_err() {
                    return;
                }
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

#[test]
fn connects_says_hello_and_reads_everything_once() {
    let fake = Fake::standard();
    let agent = fake.agent();
    wait_until("connected", || agent.snapshot().connected());
    let snapshot = agent.snapshot();
    assert_eq!(snapshot.status.this_device_name, "Fake");
    assert_eq!(snapshot.agent_version, "9.9.9");
    assert_eq!(snapshot.agent_digest, "abc123");
    assert_eq!(snapshot.unreachable_since, None);
    let received = fake.received();
    match &received[0] {
        client_message::Command::Hello(hello) => {
            assert_eq!(hello.ipc_version, IPC_VERSION);
            assert_eq!(hello.client, "linux-ui/test");
            assert!(!hello.takes_clipboard_files);
        }
        other => panic!("first came {other:?}"),
    }
    assert_eq!(received.len(), 6);
    agent.stop();
}

#[test]
fn an_answer_is_applied_before_the_request_resolves() {
    let fake = Fake::standard();
    let agent = fake.agent();
    wait_until("connected", || agent.snapshot().connected());
    let response = block_on(
        agent.request(client_message::Command::SetSharing(SetSharing {
            enabled: false,
        })),
    )
    .unwrap();
    // Moved into the snapshot, which has it before the caller hears.
    assert!(matches!(response, Answer::Applied));
    assert!(!agent.snapshot().status.sharing);
    agent.stop();
}

#[test]
fn a_refusal_carries_its_code() {
    let fake = Fake::standard();
    let agent = fake.agent();
    wait_until("connected", || agent.snapshot().connected());
    let refused = block_on(agent.request(client_message::Command::Unpair(Unpair {
        device_id: "nobody".into(),
    })))
    .unwrap_err();
    assert_eq!(refused.code(), Some("unknown_device"));
    agent.stop();
}

#[test]
fn commands_fail_at_once_while_not_connected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nothing.sock");
    let agent = Agent::start_timed(
        Box::new(move || UnixStream::connect(&path)),
        "linux-ui/test",
        Box::new(|| {}),
        Duration::from_millis(500),
    );
    let error =
        block_on(agent.request(client_message::Command::GetStatus(GetStatus {}))).unwrap_err();
    assert!(matches!(error, AgentError::Unavailable(_)));
    assert_eq!(agent.snapshot().link, Link::Connecting);
    assert!(agent.snapshot().unreachable_since.is_some());
    agent.stop();
}

#[test]
fn a_command_with_no_answer_times_out() {
    // Answers everything but SetLocked.
    let fake = Fake::standard();
    let agent = fake.agent();
    wait_until("connected", || agent.snapshot().connected());
    let started = Instant::now();
    let error = block_on(agent.request(client_message::Command::SetLocked(
        iw_proto::app::v1::SetLocked { locked: true },
    )))
    .unwrap_err();
    assert!(matches!(error, AgentError::Unavailable(_)), "{error:?}");
    assert!(started.elapsed() >= Duration::from_millis(400));
    // The connection itself carries on.
    assert!(agent.snapshot().connected());
    agent.stop();
}

#[test]
fn reconnects_after_the_service_goes_away() {
    let fake = Fake::standard();
    let changes = Arc::new(AtomicUsize::new(0));
    let counted = changes.clone();
    let path = fake.path.clone();
    let agent = Agent::start_timed(
        Box::new(move || UnixStream::connect(&path)),
        "linux-ui/test",
        Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
        Duration::from_millis(500),
    );
    wait_until("connected", || agent.snapshot().connected());
    assert!(changes.load(Ordering::SeqCst) > 0);
    fake.hang_up();
    wait_until("a second connection", || {
        fake.connections.load(Ordering::SeqCst) == 2 && agent.snapshot().connected()
    });
    let hellos = fake
        .received()
        .iter()
        .filter(|c| matches!(c, client_message::Command::Hello(_)))
        .count();
    assert_eq!(hellos, 2);
    agent.stop();
}

#[test]
fn a_failed_write_ends_the_connection_which_starts_over() {
    let fake = Fake::standard();
    let agent = fake.agent();
    wait_until("connected", || agent.snapshot().connected());
    fake.stop_reading();
    // More than the socket holds; written on another thread, so these
    // calls never wait on the socket.
    let started = Instant::now();
    let requests: Vec<_> = (0..8)
        .map(|_| {
            agent.request(client_message::Command::SendFiles(SendFiles {
                device_id: "mac".into(),
                paths: vec!["/x".repeat(100_000)],
            }))
        })
        .collect();
    assert!(started.elapsed() < Duration::from_millis(200));
    for request in requests {
        let error = block_on(request).unwrap_err();
        assert!(matches!(error, AgentError::Unavailable(_)), "{error:?}");
    }
    wait_until("a second connection", || {
        fake.connections.load(Ordering::SeqCst) == 2 && agent.snapshot().connected()
    });
    agent.stop();
}

#[test]
fn a_pairing_goes_with_the_connection() {
    let fake = Fake::standard();
    let agent = fake.agent();
    wait_until("connected", || agent.snapshot().connected());
    fake.push(Event {
        event: Some(event::Event::PairingChanged(PairingState {
            phase: PairingPhase::ShowCode as i32,
            code: "482913".into(),
            ..Default::default()
        })),
        ..Default::default()
    });
    wait_until("the code", || {
        agent.snapshot().pairing.phase() == PairingPhase::ShowCode
    });
    fake.hang_up();
    // Nothing shows that code once nobody can answer for it.
    wait_until("dropped", || agent.snapshot().link == Link::Connecting);
    assert_eq!(agent.snapshot().pairing, PairingState::default());
    agent.stop();
}

#[test]
fn commands_the_agent_takes_long_over_wait_longer() {
    // Answers a code, and starting a pairing, after twice the test's usual
    // timeout, as a real agent takes up to 30 s waiting for the other
    // device, and as long as the resolver takes to start.
    let fake = Fake::start(Arc::new(|_, command| match command {
        client_message::Command::SubmitPairingCode(_) => {
            thread::sleep(Duration::from_millis(1000));
            Some(response::Result::Pairing(PairingState {
                phase: PairingPhase::Paired as i32,
                ..Default::default()
            }))
        }
        client_message::Command::StartPairing(_) => {
            thread::sleep(Duration::from_millis(1000));
            Some(response::Result::Pairing(PairingState {
                phase: PairingPhase::Connecting as i32,
                ..Default::default()
            }))
        }
        other => standard_answer(other),
    }));
    let agent = fake.agent();
    wait_until("connected", || agent.snapshot().connected());
    block_on(agent.request(client_message::Command::SubmitPairingCode(
        SubmitPairingCode {
            code: "482913".into(),
        },
    )))
    .unwrap();
    assert_eq!(agent.snapshot().pairing.phase(), PairingPhase::Paired);
    block_on(agent.request(client_message::Command::StartPairing(Default::default()))).unwrap();
    assert_eq!(agent.snapshot().pairing.phase(), PairingPhase::Connecting);
    agent.stop();
}

#[test]
fn long_commands_have_their_own_timeouts() {
    use client_message::Command as C;
    let submit = C::SubmitPairingCode(SubmitPairingCode::default());
    assert!(
        matches!(timeout_for(&submit), Wait::Up(t) if t > Duration::from_secs(30)),
        "past the agent's 30 s"
    );
    assert_eq!(
        timeout_for(&C::StartPairing(Default::default())),
        Wait::Unlimited
    );
    assert!(matches!(
        timeout_for(&C::SendFiles(Default::default())),
        Wait::Up(t) if t >= Duration::from_secs(60)
    ));
    assert!(matches!(
        timeout_for(&C::GetDiagnostics(Default::default())),
        Wait::Up(_)
    ));
    assert_eq!(
        timeout_for(&C::SetSharing(SetSharing { enabled: true })),
        Wait::Usual
    );
}

#[test]
fn commands_time_out_while_events_keep_coming() {
    // Never answers SetLocked, and keeps sending events.
    let fake = Fake::standard();
    let agent = fake.agent();
    wait_until("connected", || agent.snapshot().connected());
    let stop = Arc::new(AtomicBool::new(false));
    let chatter = {
        let (stop, connection) = (stop.clone(), fake.connection.clone());
        thread::spawn(move || {
            let message = AgentMessage {
                msg: Some(agent_message::Msg::Event(Event {
                    event: Some(event::Event::StatusChanged(Status::default())),
                    ..Default::default()
                })),
            };
            let mut buf = BytesMut::new();
            frame::encode(&message, &mut buf).unwrap();
            while !stop.load(Ordering::SeqCst) {
                if let Some(stream) = connection.lock().unwrap().as_mut() {
                    let _ = stream.write_all(&buf);
                }
                thread::sleep(Duration::from_millis(20));
            }
        })
    };
    let started = Instant::now();
    let error = block_on(agent.request(client_message::Command::SetLocked(
        iw_proto::app::v1::SetLocked { locked: true },
    )))
    .unwrap_err();
    assert!(matches!(error, AgentError::Unavailable(_)), "{error:?}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "timed out in {:?} with the socket never quiet",
        started.elapsed()
    );
    stop.store(true, Ordering::SeqCst);
    chatter.join().unwrap();
    agent.stop();
}

#[test]
fn another_ipc_version_is_an_update_in_progress() {
    let fake = Fake::start(Arc::new(|_, command| match command {
        client_message::Command::Hello(_) => Some(response::Result::Error(ProtoError {
            code: "ipc_version_mismatch".into(),
            detail: "speaks 2".into(),
        })),
        other => standard_answer(other),
    }));
    let agent = fake.agent();
    wait_until("incompatible", || {
        agent.snapshot().link == Link::Incompatible
    });
    agent.stop();
}

#[test]
fn updating_ends_when_the_agent_is_gone() {
    let fake = Fake::start(Arc::new(|_, command| match command {
        client_message::Command::Hello(_) => Some(response::Result::Error(ProtoError {
            code: "ipc_version_mismatch".into(),
            detail: "speaks 2".into(),
        })),
        other => standard_answer(other),
    }));
    let agent = fake.agent();
    wait_until("incompatible", || {
        agent.snapshot().link == Link::Incompatible
    });
    // Refused again on the next tries: still updating.
    wait_until("a few tries", || {
        fake.connections.load(Ordering::SeqCst) >= 3
    });
    assert_eq!(agent.snapshot().link, Link::Incompatible);
    // Then nothing listens any more: out of reach, counted from now.
    let gone = SystemTime::now();
    std::fs::remove_file(&fake.path).unwrap();
    wait_until("connecting", || agent.snapshot().link == Link::Connecting);
    assert!(agent.snapshot().unreachable_since.unwrap() >= gone);
    agent.stop();
}

#[test]
fn every_crossing_is_kept_for_the_ui_and_a_drag_offered_is_declined() {
    let fake = Fake::standard();
    let agent = fake.agent();
    wait_until("connected", || agent.snapshot().connected());
    // The agent declines drags itself on Linux and offers the app none;
    // one that does all the same is declined (below).
    fake.push(Event {
        drag_arrived: Some(DragArrived {
            drag_id: "d-7".into(),
            from_device_id: "mac".into(),
            ..Default::default()
        }),
        ..Default::default()
    });
    // One from before the callback is registered is dropped, not lit
    // later beside another.
    fake.push(Event {
        cursor_crossed: Some(CursorCrossed {
            display_id: "early".into(),
            ..Default::default()
        }),
        ..Default::default()
    });
    wait_until("the early crossing", || {
        !agent.queued_crossings().is_empty()
    });
    // Each crossing calls the crossings' own callback.
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = calls.clone();
    agent.on_crossing(move || {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    assert!(agent.queued_crossings().is_empty(), "dropped");
    // Out and straight back, in a row: both kept, in order.
    for (arriving, along) in [(false, 300.0), (true, 310.0)] {
        fake.push(Event {
            cursor_crossed: Some(CursorCrossed {
                display_id: "DP-1".into(),
                edge: iw_proto::app::v1::ScreenEdge::Right as i32,
                along,
                arriving,
                ..Default::default()
            }),
            ..Default::default()
        });
    }
    let mut taken = Vec::new();
    wait_until("both crossings", || {
        taken.extend(agent.take_crossings());
        taken.len() >= 2
    });
    assert_eq!(
        taken.iter().map(|c| c.arriving).collect::<Vec<_>>(),
        [false, true]
    );
    assert!(agent.take_crossings().is_empty(), "taken once");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    // A drag offered all the same is declined at once.
    wait_until("the drag declined", || {
        fake.received()
            .iter()
            .any(|c| matches!(c, client_message::Command::DeclineDrag(d) if d.drag_id == "d-7"))
    });
    agent.stop();
}

#[test]
fn events_outside_the_oneof_are_counted() {
    let fake = Fake::standard();
    let agent = fake.agent();
    wait_until("connected", || agent.snapshot().connected());
    fake.push(Event {
        screens_identified: Some(ScreensIdentified { duration_ms: 2500 }),
        ..Default::default()
    });
    fake.push(Event {
        event: Some(event::Event::StatusChanged(Status {
            this_device_name: "Renamed".into(),
            ..Default::default()
        })),
        ..Default::default()
    });
    wait_until("both", || {
        let s = agent.snapshot();
        s.screens_identified == 1 && s.status.this_device_name == "Renamed"
    });
    assert_eq!(agent.snapshot().identify_ms, 2500);
    agent.stop();
}

#[test]
fn a_fixed_model_shows_its_snapshot_and_refuses_commands() {
    let agent = Agent::fixed(Snapshot {
        link: Link::Connected,
        ..Default::default()
    });
    assert!(agent.snapshot().connected());
    let error =
        block_on(agent.request(client_message::Command::GetStatus(GetStatus {}))).unwrap_err();
    assert!(matches!(error, AgentError::Unavailable(_)));
}

#[test]
fn a_refusal_tells_the_agents_ipc_version() {
    assert_eq!(ipc_version_in("agent speaks IPC version 2"), Some(2));
    assert_eq!(ipc_version_in("agent speaks IPC version  1 "), Some(1));
    assert_eq!(ipc_version_in("no"), None);
}
