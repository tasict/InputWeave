//! End-to-end tests: several agents with mock backends talking over QUIC on
//! the loopback interface.
//!
//! The desk used throughout (canvas coordinates):
//! - mac:  x 0..1512,    y 400..1382
//! - win:  x 1512..4072, y 0..1440 (main) and a portrait display to its right
//! - mini: x 1512..3432, y -1080..0, on top of the PC's main display
//!
//! Device ids sort as mac < mini < win, so mac dials both others and mini
//! dials win. Agents are started in the reverse order so each dialer
//! already knows its targets' addresses.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use inputweave::agent::{
    self, AgentHandle, Backends, Failure, Options, PairingPeer, PairingStatus, Status,
};
use inputweave::config::{
    BackendConfig, Config, DeviceConfig, NetworkConfig, PathsConfig, PeerConfig, PlacementConfig,
};
use iw_core::geometry::{Point, Rect};
use iw_core::input::{Button, InputEvent};
use iw_core::layout::{DeviceId, Display, Edge, Exit};
#[cfg(unix)]
use iw_core::router::RelativeEnd;
use iw_core::router::ReturnReason;
use iw_keymap::usage;
use iw_net::{Fingerprint, Identity};
use iw_platform::Permission;
use iw_platform::mock::{
    Emulated, MockCapture, MockCaptureHandle, MockClipboard, MockDesktopPictures, MockStatus,
    RecordingEmulation, SharedDisplays,
};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Spec {
    id: &'static str,
    name: &'static str,
    displays: Vec<Display>,
    offset: (f64, f64),
    /// Whether the platform backend has a clipboard; a Windows PC's
    /// clipboard is the broker's.
    clipboard: bool,
}

fn display(id: &str, x: f64, y: f64, w: f64, h: f64, primary: bool) -> Display {
    Display {
        id: id.to_owned(),
        bounds: Rect::new(x, y, w, h),
        scale: 1.0,
        primary,
    }
}

fn mac() -> Spec {
    Spec {
        id: "mac",
        name: "MacBook Pro",
        displays: vec![display("builtin", 0.0, 0.0, 1512.0, 982.0, true)],
        offset: (0.0, 400.0),
        clipboard: true,
    }
}

fn win() -> Spec {
    Spec {
        id: "win",
        name: "Windows PC",
        displays: vec![
            display("main", 0.0, 0.0, 2560.0, 1440.0, true),
            display("side", 2560.0, -300.0, 1080.0, 1920.0, false),
        ],
        offset: (1512.0, 0.0),
        clipboard: true,
    }
}

fn mini() -> Spec {
    Spec {
        id: "mini",
        name: "Mac mini",
        displays: vec![display("ext", 0.0, 0.0, 1920.0, 1080.0, true)],
        offset: (1512.0, -1080.0),
        clipboard: true,
    }
}

struct Node {
    agent: AgentHandle,
    capture: MockCaptureHandle,
    emulation: RecordingEmulation,
    displays: SharedDisplays,
    clipboard: MockClipboard,
    pictures: MockDesktopPictures,
    /// Permissions, secure input and the lock screen, as the platform has
    /// them.
    status: MockStatus,
}

/// Peers known to a node: id, fingerprint and, if already running, address.
struct Known {
    id: &'static str,
    name: &'static str,
    fingerprint: Fingerprint,
    address: Option<SocketAddr>,
}

async fn start(spec: &Spec, identity: Identity, all: &[&Spec], known: &[Known]) -> Node {
    start_with(spec, identity, all, known, Options::default()).await
}

/// Starts an agent for `spec` that trusts `known` and places the devices in
/// `all` as their specs say.
async fn start_with(
    spec: &Spec,
    identity: Identity,
    all: &[&Spec],
    known: &[Known],
    options: Options,
) -> Node {
    start_showing(
        spec,
        identity,
        all,
        known,
        options,
        MockDesktopPictures::default(),
    )
    .await
}

/// Like [`start_with`], with `pictures` on the desktop from the start.
async fn start_showing(
    spec: &Spec,
    identity: Identity,
    all: &[&Spec],
    known: &[Known],
    options: Options,
    pictures: MockDesktopPictures,
) -> Node {
    let config = Config {
        device: DeviceConfig {
            id: spec.id.to_owned(),
            name: spec.name.to_owned(),
        },
        network: NetworkConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            discovery: false,
            ..NetworkConfig::default()
        },
        paths: PathsConfig::default(),
        backend: BackendConfig::Mock {
            displays: vec![],
            folders: None,
        },
        peers: known
            .iter()
            .filter(|k| k.id != spec.id)
            .map(|k| PeerConfig {
                id: k.id.to_owned(),
                name: k.name.to_owned(),
                address: k.address,
                fingerprint: k.fingerprint,
                swap_gui_and_control: false,
            })
            .collect(),
        layout: all
            .iter()
            .map(|s| PlacementConfig {
                device: s.id.to_owned(),
                x: s.offset.0,
                y: s.offset.1,
            })
            .collect(),
        clipboard: Default::default(),
    };
    let (capture, handle) = MockCapture::new();
    let emulation = RecordingEmulation::default();
    let displays = SharedDisplays::new(spec.displays.clone());
    let clipboard = MockClipboard::default();
    let status = MockStatus::granted();
    let backends = Backends {
        capture: Box::new(capture),
        emulation: Box::new(emulation.clone()),
        displays: Box::new(displays.clone()),
        pictures: Box::new(pictures.clone()),
        status: Box::new(status.clone()),
        clipboard: spec
            .clipboard
            .then(|| Box::new(clipboard.clone()) as Box<dyn iw_platform::Clipboard>),
    };
    let agent = agent::start(config, Arc::new(identity), backends, options)
        .await
        .expect("agent starts");
    Node {
        agent,
        capture: handle,
        emulation,
        displays,
        clipboard,
        pictures,
        status,
    }
}

/// Starts the given devices, largest id first, fully meshed.
async fn desk(specs: &[Spec]) -> Vec<Node> {
    desk_with(specs, |_| Options::default()).await
}

/// Like [`desk`], with each device's options.
async fn desk_with(specs: &[Spec], options: impl Fn(&Spec) -> Options) -> Vec<Node> {
    desk_full(specs, options, |_| MockDesktopPictures::default()).await
}

/// Like [`desk`], with each device's desktop pictures from the start.
async fn desk_showing(
    specs: &[Spec],
    pictures: impl Fn(&Spec) -> MockDesktopPictures,
) -> Vec<Node> {
    desk_full(specs, |_| Options::default(), pictures).await
}

async fn desk_full(
    specs: &[Spec],
    options: impl Fn(&Spec) -> Options,
    pictures: impl Fn(&Spec) -> MockDesktopPictures,
) -> Vec<Node> {
    let identities: Vec<Identity> = specs
        .iter()
        .map(|_| Identity::generate().unwrap())
        .collect();
    let mut known: Vec<Known> = specs
        .iter()
        .zip(&identities)
        .map(|(s, i)| Known {
            id: s.id,
            name: s.name,
            fingerprint: i.fingerprint(),
            address: None,
        })
        .collect();
    let all: Vec<&Spec> = specs.iter().collect();
    let mut order: Vec<usize> = (0..specs.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(specs[i].id));

    let mut identities: Vec<Option<Identity>> = identities.into_iter().map(Some).collect();
    let mut nodes: Vec<Option<Node>> = (0..specs.len()).map(|_| None).collect();
    for i in order {
        let identity = identities[i].take().unwrap();
        let node = start_showing(
            &specs[i],
            identity,
            &all,
            &known,
            options(&specs[i]),
            pictures(&specs[i]),
        )
        .await;
        known[i].address = Some(node.agent.local_addr());
        nodes[i] = Some(node);
    }
    let nodes: Vec<Node> = nodes.into_iter().map(Option::unwrap).collect();
    for node in &nodes {
        let peers: Vec<String> = specs
            .iter()
            .map(|s| s.id.to_owned())
            .filter(|id| *id != node.agent.status().this_device)
            .collect();
        wait_status(node, |s| {
            s.layout_ready
                && peers
                    .iter()
                    .all(|p| s.device(p).is_some_and(|d| d.connected && d.arranged))
        })
        .await;
    }
    nodes
}

async fn wait_status(node: &Node, mut ready: impl FnMut(&Status) -> bool) -> Status {
    let mut rx = node.agent.watch_status();
    let status = tokio::time::timeout(TIMEOUT, rx.wait_for(|s| ready(s)))
        .await
        .unwrap_or_else(|_| panic!("timed out; last status: {:#?}", node.agent.status()))
        .expect("agent running");
    status.clone()
}

/// `agent`'s status once `ready` says so.
async fn wait_status_of(agent: &AgentHandle, mut ready: impl FnMut(&Status) -> bool) -> Status {
    let mut rx = agent.watch_status();
    let status = tokio::time::timeout(TIMEOUT, rx.wait_for(|s| ready(s)))
        .await
        .unwrap_or_else(|_| panic!("timed out; last status: {:#?}", agent.status()))
        .expect("agent running");
    status.clone()
}

async fn wait_injected(node: &Node, expected: Emulated) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !node.emulation.calls().contains(&expected) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {expected:?}; injected: {:?}",
            node.emulation.calls()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn motion(node: &Node, position: (f64, f64), delta: (f64, f64)) {
    node.capture
        .emit(InputEvent::PointerMotion {
            position: Point::new(position.0, position.1),
            delta: Point::new(delta.0, delta.1),
        })
        .unwrap();
}

fn key(node: &Node, usage: iw_core::HidUsage, pressed: bool) {
    node.capture
        .emit(InputEvent::Key { usage, pressed })
        .unwrap();
}

/// Moves the mac's cursor across its right edge into the PC, landing at
/// (4, 900) in the PC's coordinates.
async fn mac_enters_win(mac: &Node, win: &Node) {
    motion(mac, (1511.0, 500.0), (5.0, 0.0));
    wait_injected(win, Emulated::MoveTo(Point::new(4.0, 900.0))).await;
    wait_status(win, |s| s.controlled_by.as_deref() == Some("mac")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn crosses_to_a_peer_and_forwards_input() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    mac_enters_win(mac, win).await;
    assert!(mac.capture.is_grabbed());
    assert_eq!(mac.agent.status().cursor_on, "win");

    key(mac, usage::A, true);
    wait_injected(win, Emulated::Key(usage::A, true)).await;
    mac.capture
        .emit(InputEvent::Button {
            button: Button::Left,
            pressed: true,
        })
        .unwrap();
    wait_injected(win, Emulated::Button(Button::Left, true)).await;
    motion(mac, (0.0, 0.0), (10.0, 20.0));
    wait_injected(win, Emulated::MoveTo(Point::new(14.0, 920.0))).await;
    mac.capture
        .emit(InputEvent::Button {
            button: Button::Left,
            pressed: false,
        })
        .unwrap();
    wait_injected(win, Emulated::Button(Button::Left, false)).await;

    // Back across the seam with A still held: the PC releases it.
    motion(mac, (0.0, 0.0), (-20.0, 0.0));
    wait_injected(win, Emulated::Key(usage::A, false)).await;
    wait_status(mac, |s| s.cursor_on == "mac").await;
    assert!(!mac.capture.is_grabbed());
    assert_eq!(mac.capture.warps(), vec![Point::new(1506.0, 520.0)]);
    wait_status(win, |s| s.controlled_by.is_none()).await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn hops_between_two_remote_devices() {
    let nodes = desk(&[mac(), mini(), win()]).await;
    let (mac, mini, win) = (&nodes[0], &nodes[1], &nodes[2]);

    mac_enters_win(mac, win).await;
    motion(mac, (0.0, 0.0), (484.0, -895.0)); // to canvas (2000, 5)
    wait_injected(win, Emulated::MoveTo(Point::new(488.0, 5.0))).await;
    motion(mac, (0.0, 0.0), (0.0, -10.0));
    wait_injected(mini, Emulated::MoveTo(Point::new(488.0, 1075.0))).await;
    wait_status(mini, |s| s.controlled_by.as_deref() == Some("mac")).await;
    wait_status(win, |s| s.controlled_by.is_none()).await;
    assert_eq!(mac.agent.status().cursor_on, "mini");

    for node in nodes {
        node.agent.shutdown().await;
    }
}

type Crossings = tokio::sync::broadcast::Receiver<agent::CursorCrossed>;

/// Hears where the cursor crosses the edges of `node`'s displays, from now
/// on.
fn crossings(node: &Node) -> Crossings {
    node.agent.subscribe_cursor_crossed()
}

async fn next_crossing(rx: &mut Crossings) -> agent::CursorCrossed {
    tokio::time::timeout(TIMEOUT, rx.recv())
        .await
        .expect("timed out waiting for a crossing")
        .expect("agent running")
}

/// No crossing is told for a moment.
async fn no_crossing(rx: &mut Crossings) {
    tokio::time::sleep(Duration::from_millis(300)).await;
    let next = rx.try_recv();
    assert!(
        matches!(
            next,
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ),
        "crossed: {next:?}"
    );
}

fn crossing(
    display: &str,
    edge: Edge,
    along: f64,
    bounds: Rect,
    arriving: bool,
    other: &str,
    name: &str,
) -> agent::CursorCrossed {
    agent::CursorCrossed {
        display: display.to_owned(),
        edge,
        along,
        bounds,
        arriving,
        other_device_id: other.to_owned(),
        other_device_name: name.to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn both_devices_hear_where_the_cursor_crossed_their_edges() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let (mut at_mac, mut at_win) = (crossings(mac), crossings(win));
    let mac_screen = Rect::new(0.0, 0.0, 1512.0, 982.0);
    let pc_screen = Rect::new(0.0, 0.0, 2560.0, 1440.0);

    // Out of the Mac's right edge, into the PC's left one; each in its own
    // coordinates, the Mac standing 400 lower on the canvas.
    mac_enters_win(mac, win).await;
    assert_eq!(
        next_crossing(&mut at_mac).await,
        crossing(
            "builtin",
            Edge::Right,
            500.0,
            mac_screen,
            false,
            "win",
            "Windows PC"
        )
    );
    assert_eq!(
        next_crossing(&mut at_win).await,
        crossing(
            "main",
            Edge::Left,
            900.0,
            pc_screen,
            true,
            "mac",
            "MacBook Pro"
        )
    );

    // And back by the same edge.
    motion(mac, (0.0, 0.0), (-20.0, 0.0));
    assert_eq!(
        next_crossing(&mut at_win).await,
        crossing(
            "main",
            Edge::Left,
            900.0,
            pc_screen,
            false,
            "mac",
            "MacBook Pro"
        )
    );
    assert_eq!(
        next_crossing(&mut at_mac).await,
        crossing(
            "builtin",
            Edge::Right,
            500.0,
            mac_screen,
            true,
            "win",
            "Windows PC"
        )
    );

    // A jump there and back is no crossing.
    mac.agent.jump_to("win").await.unwrap();
    wait_status(win, |s| s.controlled_by.as_deref() == Some("mac")).await;
    mac.agent.jump_to("mac").await.unwrap();
    wait_status(win, |s| s.controlled_by.is_none()).await;
    no_crossing(&mut at_mac).await;
    no_crossing(&mut at_win).await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_app_hears_where_the_cursor_crossed_its_screens() {
    use iw_proto::app::v1::{AgentMessage, CursorCrossed, ScreenEdge, agent_message};

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let nodes = desk_with(&[mac(), win()], ipc_on_mac(&socket)).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let mut app = App::connect(&socket).await;
    let mut next = async || loop {
        let msg = tokio::time::timeout(TIMEOUT, app.conn.recv::<AgentMessage>())
            .await
            .expect("the app hears of it")
            .unwrap()
            .unwrap();
        if let Some(agent_message::Msg::Event(e)) = msg.msg
            && let Some(crossed) = e.cursor_crossed
        {
            // Outside the event oneof.
            assert!(e.event.is_none());
            break crossed;
        }
    };
    let expected = |arriving| CursorCrossed {
        display_id: "builtin".to_owned(),
        edge: ScreenEdge::Right.into(),
        along: 500.0,
        display_x: 0.0,
        display_y: 0.0,
        display_width: 1512.0,
        display_height: 982.0,
        arriving,
        other_device_id: "win".to_owned(),
        other_device_name: "Windows PC".to_owned(),
    };

    mac_enters_win(mac, win).await;
    assert_eq!(next().await, expected(false));
    motion(mac, (0.0, 0.0), (-20.0, 0.0));
    assert_eq!(next().await, expected(true));

    // Out and straight back, faster than the app reads: it hears both.
    motion(mac, (1511.0, 500.0), (5.0, 0.0));
    motion(mac, (0.0, 0.0), (-20.0, 0.0));
    assert_eq!(next().await, expected(false));
    assert_eq!(next().await, expected(true));

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hop_lights_the_edges_of_the_two_devices_it_crosses() {
    let nodes = desk(&[mac(), mini(), win()]).await;
    let (mac, mini, win) = (&nodes[0], &nodes[1], &nodes[2]);
    mac_enters_win(mac, win).await;
    motion(mac, (0.0, 0.0), (484.0, -895.0)); // to canvas (2000, 5)
    wait_injected(win, Emulated::MoveTo(Point::new(488.0, 5.0))).await;
    let (mut at_mac, mut at_mini, mut at_win) = (crossings(mac), crossings(mini), crossings(win));

    // From the PC's top edge into the mini's bottom: the controller's own
    // screen is out of it. The mini knows the PC from the desk, so it has
    // its name.
    motion(mac, (0.0, 0.0), (0.0, -10.0));
    assert_eq!(
        next_crossing(&mut at_win).await,
        crossing(
            "main",
            Edge::Top,
            488.0,
            Rect::new(0.0, 0.0, 2560.0, 1440.0),
            false,
            "mini",
            "Mac mini"
        )
    );
    assert_eq!(
        next_crossing(&mut at_mini).await,
        crossing(
            "ext",
            Edge::Bottom,
            488.0,
            Rect::new(0.0, 0.0, 1920.0, 1080.0),
            true,
            "win",
            "Windows PC"
        )
    );
    no_crossing(&mut at_mac).await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_controller_that_does_not_say_where_it_crossed_lights_nothing_there() {
    let nodes = desk_with(&[mac(), win()], |spec| Options {
        omit_crossing_spots: spec.id == "mac",
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let (mut at_mac, mut at_win) = (crossings(mac), crossings(win));

    // The Mac still lights its own edge; the PC, not told, lights none.
    mac_enters_win(mac, win).await;
    assert!(!next_crossing(&mut at_mac).await.arriving);
    no_crossing(&mut at_win).await;
    motion(mac, (0.0, 0.0), (-20.0, 0.0));
    assert!(next_crossing(&mut at_mac).await.arriving);
    no_crossing(&mut at_win).await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crashed_controller_leaves_no_stuck_keys() {
    let mut nodes = desk(&[mac(), win()]).await;
    let win = nodes.pop().unwrap();
    let mac = nodes.pop().unwrap();

    mac_enters_win(&mac, &win).await;
    key(&mac, usage::LEFT_SHIFT, true);
    wait_injected(&win, Emulated::Key(usage::LEFT_SHIFT, true)).await;

    mac.agent.abort().await;
    wait_injected(&win, Emulated::Key(usage::LEFT_SHIFT, false)).await;
    wait_status(&win, |s| s.controlled_by.is_none()).await;

    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn control_returns_when_the_controlled_peer_disappears() {
    let mut nodes = desk(&[mac(), win()]).await;
    let win = nodes.pop().unwrap();
    let mac = nodes.pop().unwrap();

    mac_enters_win(&mac, &win).await;
    win.agent.abort().await;

    let status = wait_status(&mac, |s| s.cursor_on == "mac").await;
    assert_eq!(
        status.last_return,
        Some(ReturnReason::PeerLost(DeviceId::new("win")))
    );
    assert!(!mac.capture.is_grabbed());
    mac.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn local_input_on_the_controlled_device_takes_over() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    mac_enters_win(mac, win).await;
    key(mac, usage::A, true);
    wait_injected(win, Emulated::Key(usage::A, true)).await;

    // Someone moves the PC's own mouse.
    motion(win, (900.0, 700.0), (3.0, 0.0));
    wait_injected(win, Emulated::Key(usage::A, false)).await;
    let status = wait_status(mac, |s| s.cursor_on == "mac").await;
    assert_eq!(
        status.last_return,
        Some(ReturnReason::RemoteActivity(DeviceId::new("win")))
    );
    assert!(!mac.capture.is_grabbed());

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_silent_peer_is_dropped_after_missing_heartbeats() {
    // "win" is a bare endpoint that completes the handshake and then never
    // sends anything, as if its agent had hung.
    let identity = Arc::new(Identity::generate().unwrap());
    let fake =
        iw_net::PeerEndpoint::bind("127.0.0.1:0".parse().unwrap(), identity.clone()).unwrap();
    let mac_identity = Identity::generate().unwrap();
    let mac_fp = mac_identity.fingerprint();
    let known = [Known {
        id: "win",
        name: "Windows PC",
        fingerprint: identity.fingerprint(),
        address: Some(fake.local_addr().unwrap()),
    }];
    let spec = mac();
    let mac = start(&spec, mac_identity, &[&spec], &known).await;

    let accepted = tokio::time::timeout(TIMEOUT, async {
        fake.accept()
            .await
            .unwrap()
            .finish(|fp| *fp == mac_fp)
            .await
            .unwrap()
    })
    .await
    .unwrap();
    let session = iw_net::Session::accept(
        accepted.connection.clone(),
        accepted.peer,
        iw_net::hello("win", "Windows PC", iw_proto::peer::v1::Os::Windows),
    )
    .await
    .unwrap();
    wait_status(&mac, |s| s.device("win").is_some_and(|d| d.connected)).await;

    let closed = tokio::time::timeout(TIMEOUT, accepted.connection.closed())
        .await
        .expect("the agent closes the silent session");
    match closed {
        quinn::ConnectionError::ApplicationClosed(c) => {
            assert_eq!(c.error_code, iw_net::close::TIMEOUT.into());
        }
        other => panic!("unexpected close: {other:?}"),
    }
    wait_status(&mac, |s| s.device("win").is_some_and(|d| !d.connected)).await;
    drop(session);
    mac.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn ctl_reports_status_over_ipc() {
    use iw_ipc::IpcConnection;
    use iw_proto::app::v1::{
        AgentMessage, ClientHello, ClientMessage, GetArrangement, GetStatus, SetPreviewSharing,
        SetSharing, agent_message, client_message::Command, response,
    };

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let spec = mac();
    let known = [Known {
        id: "win",
        name: "Windows PC",
        fingerprint: Identity::generate().unwrap().fingerprint(),
        address: None,
    }];
    let digest = "0123456789abcdef".repeat(4);
    let options = Options {
        ipc_socket: Some(socket.clone()),
        agent_digest: digest.clone(),
        ..Options::default()
    };
    let pictures = MockDesktopPictures::default();
    pictures.set("builtin", fake_jpeg(1, 500));
    let mac = start_showing(
        &spec,
        Identity::generate().unwrap(),
        &[&spec],
        &known,
        options,
        pictures,
    )
    .await;

    // The real binary, as a user would run it.
    let output = tokio::task::spawn_blocking({
        let socket = socket.clone();
        move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_inputweave"))
                .args(["ctl", "status", "--socket"])
                .arg(&socket)
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "ctl failed: {output:?}");
    assert!(
        stdout.contains(&format!("agent sha-256:  {digest}\n")),
        "{stdout}"
    );
    assert!(stdout.contains("sharing:        on"), "{stdout}");
    assert!(stdout.contains("this device:    mac"), "{stdout}");
    assert!(
        stdout.contains("Windows PC") && stdout.contains("offline"),
        "{stdout}"
    );
    assert!(
        stdout.contains("previews:       desktop pictures shared"),
        "{stdout}"
    );

    // The protocol directly: Hello comes first, and SetSharing is applied.
    let mut conn = IpcConnection::connect(&socket).await.unwrap();
    let mut request = async |id: u64, command: Command| {
        conn.send(&ClientMessage {
            request_id: id,
            command: Some(command),
        })
        .await
        .unwrap();
        loop {
            let msg = conn.recv::<AgentMessage>().await.unwrap().unwrap();
            if let Some(agent_message::Msg::Response(r)) = msg.msg
                && r.request_id == id
            {
                return r.result.unwrap();
            }
        }
    };
    let refused = request(1, Command::GetStatus(GetStatus {})).await;
    assert!(matches!(refused, response::Result::Error(e) if e.code == "hello_required"));
    let hello = Command::Hello(ClientHello {
        ipc_version: iw_proto::IPC_VERSION,
        client: "test".into(),
        ..ClientHello::default()
    });
    assert!(matches!(
        request(2, hello).await,
        response::Result::Hello(h) if h.agent_digest == digest
    ));
    let paused = request(3, Command::SetSharing(SetSharing { enabled: false })).await;
    assert!(matches!(paused, response::Result::Status(s) if !s.sharing));
    assert!(!mac.agent.status().sharing);
    let private = request(
        4,
        Command::SetPreviewSharing(SetPreviewSharing { enabled: false }),
    )
    .await;
    // The mock backend reads desktop pictures, so the switch applies.
    assert!(matches!(
        private,
        response::Result::Status(s) if !s.preview_sharing && s.desktop_pictures_available
    ));
    // This device still sees its own picture, as the app shows it, once
    // the agent has read it.
    wait_arrangement(&mac, |a| picture(a, "mac", "builtin").is_some()).await;
    let response::Result::Arrangement(arrangement) =
        request(5, Command::GetArrangement(GetArrangement {})).await
    else {
        panic!("no arrangement");
    };
    let screen = &arrangement.devices[0].screens[0];
    assert_eq!(
        (screen.id.as_str(), screen.preview_jpeg.clone()),
        ("builtin", fake_jpeg(1, 500))
    );

    mac.agent.shutdown().await;
    assert!(!socket.exists());
}

async fn wait_pairing(node: &Node, mut done: impl FnMut(&PairingStatus) -> bool) -> PairingStatus {
    let mut rx = node.agent.watch_pairing();
    tokio::time::timeout(TIMEOUT, rx.wait_for(|s| done(s)))
        .await
        .unwrap_or_else(|_| panic!("timed out; last pairing state: {:?}", node.agent.pairing()))
        .expect("agent running")
        .clone()
}

/// The code `node` shows once another device asks to pair with it.
async fn shown_code(node: &Node) -> iw_net::PairingCode {
    match wait_pairing(node, |s| matches!(s, PairingStatus::ShowCode { .. })).await {
        PairingStatus::ShowCode { code, .. } => code,
        _ => unreachable!(),
    }
}

/// A device with no trusted peers and no layout, remembering pairings in
/// `devices_file`.
async fn unpaired(
    spec: &Spec,
    identity: Identity,
    devices_file: Option<std::path::PathBuf>,
) -> Node {
    unpaired_showing(spec, identity, devices_file, MockDesktopPictures::default()).await
}

/// Like [`unpaired`], with `pictures` on the desktop from the start.
async fn unpaired_showing(
    spec: &Spec,
    identity: Identity,
    devices_file: Option<std::path::PathBuf>,
    pictures: MockDesktopPictures,
) -> Node {
    let options = Options {
        devices_file,
        ..Options::default()
    };
    start_showing(spec, identity, &[], &[], options, pictures).await
}

/// `initiator` pairs with `responder`, typing the code `responder` shows.
async fn pair(initiator: &Node, responder: &Node) {
    initiator
        .agent
        .start_pairing(responder.agent.local_addr())
        .await
        .unwrap();
    let code = shown_code(responder).await;
    wait_pairing(initiator, |s| matches!(s, PairingStatus::EnterCode { .. })).await;
    let state = initiator.agent.submit_pairing_code(code).await.unwrap();
    assert!(matches!(state, PairingStatus::Paired { .. }), "{state:?}");
    wait_pairing(responder, |s| matches!(s, PairingStatus::Paired { .. })).await;
}

/// Waits until both devices hold the same version of the arrangement, with
/// both of them placed in it.
async fn agreed(a: &Node, b: &Node) -> inputweave::agent::Arrangement {
    let (a_id, b_id) = (
        a.agent.device_id().to_owned(),
        b.agent.device_id().to_owned(),
    );
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let (x, y) = (a.agent.arrangement(), b.agent.arrangement());
        let placed = |arr: &inputweave::agent::Arrangement| {
            [&a_id, &b_id]
                .iter()
                .all(|id| arr.device(id).is_some_and(|d| d.offset.is_some()))
        };
        let same_places = [&a_id, &b_id]
            .iter()
            .all(|id| x.device(id).map(|d| d.offset) == y.device(id).map(|d| d.offset));
        if x.version > 0 && x.version == y.version && placed(&x) && same_places {
            return x;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "arrangements never agreed: {x:?} vs {y:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn connected(node: &Node, peer: &str) -> Status {
    wait_status(node, |s| {
        s.layout_ready && s.device(peer).is_some_and(|d| d.connected && d.arranged)
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn paired_devices_connect_and_share_input() {
    let dir = tempfile::tempdir().unwrap();
    let mac_file = dir.path().join("mac-devices.toml");
    let win = unpaired(&win(), Identity::generate().unwrap(), None).await;
    let mac = unpaired(
        &mac(),
        Identity::generate().unwrap(),
        Some(mac_file.clone()),
    )
    .await;

    pair(&mac, &win).await;
    assert_eq!(
        win.agent.pairing(),
        PairingStatus::Paired {
            peer: PairingPeer {
                id: "mac".to_owned(),
                name: "MacBook Pro".to_owned(),
            }
        }
    );
    let status = connected(&mac, "win").await;
    assert!(status.device("win").unwrap().paired);
    connected(&win, "mac").await;

    // Unarranged, the PC went to the right of the Mac with tops aligned:
    // the Mac's right edge at x 1512 meets the PC's left edge.
    motion(&mac, (1511.0, 500.0), (5.0, 0.0));
    wait_injected(&win, Emulated::MoveTo(Point::new(4.0, 500.0))).await;
    wait_status(&win, |s| s.controlled_by.as_deref() == Some("mac")).await;
    key(&mac, usage::A, true);
    wait_injected(&win, Emulated::Key(usage::A, true)).await;

    // The pairing and the placement were remembered.
    let saved = inputweave::store::DevicesFile::load(&mac_file).unwrap();
    assert_eq!(saved.devices.len(), 1);
    assert_eq!(saved.devices[0].id, "win");
    assert_eq!(saved.devices[0].fingerprint, win.agent.fingerprint());
    assert_eq!(saved.devices[0].last_address, Some(win.agent.local_addr()));
    // Both devices placed each other at once; they agree on one version,
    // with the PC right of the Mac and their primary displays' tops level.
    let at = |id: &str| {
        let p = saved.layout.iter().find(|p| p.device == id).unwrap();
        (p.x, p.y)
    };
    let ((mac_x, mac_y), (win_x, win_y)) = (at("mac"), at("win"));
    assert_eq!((win_x - mac_x, win_y - mac_y), (1512.0, 0.0));
    let mine = mac.agent.arrangement();
    let theirs = win.agent.arrangement();
    assert_eq!(mine.version, theirs.version);
    assert_eq!(
        mine.device("win").unwrap().offset,
        theirs.device("win").unwrap().offset
    );

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_agent_reconnects_to_devices_it_paired_with() {
    let dir = tempfile::tempdir().unwrap();
    let mac_file = dir.path().join("devices.toml");
    let mac_key = dir.path().join("identity");
    let spec = mac();
    let win = unpaired(&win(), Identity::generate().unwrap(), None).await;
    let mac = unpaired(
        &spec,
        Identity::load_or_generate(&mac_key).unwrap(),
        Some(mac_file.clone()),
    )
    .await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    mac.agent.shutdown().await;
    wait_status(&win, |s| s.device("mac").is_some_and(|d| !d.connected)).await;

    // Same identity and devices file: no discovery, no configured address,
    // only the address the PC was last reached at.
    let mac = unpaired(
        &spec,
        Identity::load_or_generate(&mac_key).unwrap(),
        Some(mac_file),
    )
    .await;
    connected(&mac, "win").await;
    connected(&win, "mac").await;

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unpairing_ends_the_session_and_the_trust() {
    let win = unpaired(&win(), Identity::generate().unwrap(), None).await;
    let mac = unpaired(&mac(), Identity::generate().unwrap(), None).await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    motion(&mac, (1511.0, 500.0), (5.0, 0.0));
    wait_status(&win, |s| s.controlled_by.as_deref() == Some("mac")).await;
    key(&mac, usage::LEFT_SHIFT, true);
    wait_injected(&win, Emulated::Key(usage::LEFT_SHIFT, true)).await;

    mac.agent.unpair("win").unwrap();
    assert!(mac.agent.unpair("win").is_err());
    let status = wait_status(&mac, |s| s.device("win").is_none()).await;
    assert_eq!(status.cursor_on, "mac");
    assert!(!mac.capture.is_grabbed());
    // The PC sees the session end and lets go of the key.
    wait_injected(&win, Emulated::Key(usage::LEFT_SHIFT, false)).await;
    wait_status(&win, |s| s.device("mac").is_some_and(|d| !d.connected)).await;

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_codes_fail_the_pairing_and_hold_off_retries() {
    let win = unpaired(&win(), Identity::generate().unwrap(), None).await;
    let mac = unpaired(&mac(), Identity::generate().unwrap(), None).await;

    mac.agent
        .start_pairing(win.agent.local_addr())
        .await
        .unwrap();
    let code = shown_code(&win).await;
    wait_pairing(&mac, |s| matches!(s, PairingStatus::EnterCode { .. })).await;
    let wrong: iw_net::PairingCode = if code.as_str() == "000000" {
        "111111"
    } else {
        "000000"
    }
    .parse()
    .unwrap();
    let first = mac.agent.submit_pairing_code(wrong.clone()).await.unwrap();
    assert!(
        matches!(
            first,
            PairingStatus::EnterCode {
                attempts_left: 2,
                ..
            }
        ),
        "{first:?}"
    );
    mac.agent.submit_pairing_code(wrong.clone()).await.unwrap();
    let last = mac.agent.submit_pairing_code(wrong).await.unwrap();
    assert!(
        matches!(
            last,
            PairingStatus::Failed {
                reason: Failure::TooManyAttempts,
                ..
            }
        ),
        "{last:?}"
    );
    wait_pairing(&win, |s| {
        matches!(
            s,
            PairingStatus::Failed {
                reason: Failure::TooManyAttempts,
                ..
            }
        )
    })
    .await;
    assert!(win.agent.status().device("mac").is_none());

    // Right after, the PC turns further requests away for a while.
    mac.agent
        .start_pairing(win.agent.local_addr())
        .await
        .unwrap();
    wait_pairing(&mac, |s| {
        matches!(
            s,
            PairingStatus::Failed {
                reason: Failure::Busy,
                ..
            }
        )
    })
    .await;

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnecting_after_a_wrong_guess_is_held_off() {
    let win = unpaired(&win(), Identity::generate().unwrap(), None).await;
    let mac = unpaired(&mac(), Identity::generate().unwrap(), None).await;
    mac.agent
        .start_pairing(win.agent.local_addr())
        .await
        .unwrap();
    let code = shown_code(&win).await;
    wait_pairing(&mac, |s| matches!(s, PairingStatus::EnterCode { .. })).await;
    let wrong = if code.as_str() == "000000" {
        "111111"
    } else {
        "000000"
    };
    mac.agent
        .submit_pairing_code(wrong.parse().unwrap())
        .await
        .unwrap();
    // Give up before running out of attempts, and come straight back.
    mac.agent.cancel_pairing();
    wait_pairing(&win, |s| {
        matches!(
            s,
            PairingStatus::Failed {
                reason: Failure::Cancelled,
                ..
            }
        )
    })
    .await;
    wait_pairing(&mac, |s| matches!(s, PairingStatus::Failed { .. })).await;
    mac.agent
        .start_pairing(win.agent.local_addr())
        .await
        .unwrap();
    wait_pairing(&mac, |s| {
        matches!(
            s,
            PairingStatus::Failed {
                reason: Failure::Busy,
                ..
            }
        )
    })
    .await;

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_device_showing_the_code_can_decline() {
    let win = unpaired(&win(), Identity::generate().unwrap(), None).await;
    let mac = unpaired(&mac(), Identity::generate().unwrap(), None).await;
    mac.agent
        .start_pairing(win.agent.local_addr())
        .await
        .unwrap();
    shown_code(&win).await;
    // A second request while the first one is shown is turned away.
    assert_eq!(
        mac.agent.start_pairing(win.agent.local_addr()).await,
        Err(Failure::Busy)
    );
    win.agent.cancel_pairing();
    wait_pairing(&mac, |s| {
        matches!(
            s,
            PairingStatus::Failed {
                reason: Failure::Rejected,
                ..
            }
        )
    })
    .await;
    assert!(mac.agent.status().device("win").is_none());

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_id_already_in_use_cannot_pair() {
    // Both devices call themselves "mac".
    let other = Spec {
        name: "Another Mac",
        ..mac()
    };
    let first = unpaired(&mac(), Identity::generate().unwrap(), None).await;
    let second = unpaired(&other, Identity::generate().unwrap(), None).await;
    first
        .agent
        .start_pairing(second.agent.local_addr())
        .await
        .unwrap();
    wait_pairing(&first, |s| {
        matches!(
            s,
            PairingStatus::Failed {
                reason: Failure::IdConflict,
                ..
            }
        )
    })
    .await;
    assert_eq!(second.agent.pairing(), PairingStatus::Idle);

    first.agent.shutdown().await;
    second.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn ctl_pairs_with_the_code_typed_in() {
    use std::io::Write;
    use std::process::Stdio;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let win = unpaired(&win(), Identity::generate().unwrap(), None).await;
    let options = Options {
        ipc_socket: Some(socket.clone()),
        ..Options::default()
    };
    let mac = start_with(&mac(), Identity::generate().unwrap(), &[], &[], options).await;

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_inputweave"))
        .args(["ctl", "pair", "--socket"])
        .arg(&socket)
        .arg(win.agent.local_addr().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let code = shown_code(&win).await;
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{code}").unwrap();
    drop(stdin);
    let output = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "ctl failed: {output:?}");
    assert!(stdout.contains("Paired with Windows PC (win)."), "{stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Enter the pairing code shown on Windows PC"),
        "{stderr}"
    );

    connected(&mac, "win").await;
    let (_, status) = inputweave::ctl::status(&socket).await.unwrap();
    let listed = inputweave::ctl::format_status(&status);
    assert!(
        listed.contains("connected") && listed.contains("paired"),
        "{listed}"
    );

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_client_connecting_late_sees_the_code_already_shown() {
    // The UI may only be started once the request has arrived.
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let options = Options {
        ipc_socket: Some(socket.clone()),
        ..Options::default()
    };
    let win = start_with(&win(), Identity::generate().unwrap(), &[], &[], options).await;
    let mac = unpaired(&mac(), Identity::generate().unwrap(), None).await;
    mac.agent
        .start_pairing(win.agent.local_addr())
        .await
        .unwrap();
    let code = shown_code(&win).await;

    let mut ui = inputweave::ctl::Client::connect(&socket).await.unwrap();
    let state = ui.pairing().await.unwrap();
    assert_eq!(state.phase(), iw_proto::app::v1::PairingPhase::ShowCode);
    assert_eq!(state.code, code.as_str());
    assert_eq!(state.peer_name, "MacBook Pro");
    assert!(state.expires_at_unix_ms > 0);

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

/// The Mac's agent alone, capturing through `capture`, its permissions and
/// the like as `platform` has them.
async fn lone_mac(capture: MockCapture, platform: &MockStatus) -> AgentHandle {
    let spec = mac();
    let config = Config {
        device: DeviceConfig {
            id: spec.id.to_owned(),
            name: spec.name.to_owned(),
        },
        network: NetworkConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            discovery: false,
            ..NetworkConfig::default()
        },
        paths: PathsConfig::default(),
        backend: BackendConfig::Mock {
            displays: vec![],
            folders: None,
        },
        peers: vec![],
        layout: vec![],
        clipboard: Default::default(),
    };
    let backends = Backends {
        capture: Box::new(capture),
        emulation: Box::new(RecordingEmulation::default()),
        displays: Box::new(SharedDisplays::new(spec.displays.clone())),
        pictures: Box::new(MockDesktopPictures::default()),
        status: Box::new(platform.clone()),
        clipboard: None,
    };
    agent::start(
        config,
        Arc::new(Identity::generate().unwrap()),
        backends,
        Options::default(),
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn capture_starts_once_its_permission_is_granted() {
    // The agent starts without the permission: it still runs and can be
    // controlled, but captures nothing until the user grants it.
    let (capture, handle) = MockCapture::new();
    handle.deny(true);
    let platform = MockStatus::default();
    platform.set_permission(Permission::Accessibility, false);
    let agent = lone_mac(capture, &platform).await;
    let mut status = agent.watch_status();
    let denied = tokio::time::timeout(
        TIMEOUT,
        status.wait_for(|s| s.permissions.iter().any(|p| !p.granted)),
    )
    .await
    .unwrap()
    .unwrap()
    .clone();
    assert!(!denied.capturing);
    assert!(!handle.is_started());

    // The native agent restarts when this resolves.
    {
        let granted = agent.permission_granted();
        tokio::pin!(granted);
        assert!(
            tokio::time::timeout(Duration::from_millis(300), &mut granted)
                .await
                .is_err()
        );
        handle.deny(false);
        platform.set_permission(Permission::Accessibility, true);
        tokio::time::timeout(TIMEOUT, granted).await.unwrap();
    }
    tokio::time::timeout(TIMEOUT, status.wait_for(|s| s.capturing))
        .await
        .unwrap()
        .unwrap();
    assert!(handle.is_started());
    agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn capture_still_starting_is_started_once_the_permission_is_granted() {
    // As at login on Wayland: the desktop starts what the user allowed
    // before, and capture is not ready yet (not a missing permission, worth
    // no warning). It starts once the permission is granted.
    let (capture, handle) = MockCapture::new();
    handle.set_starting(true);
    let platform = MockStatus::default();
    platform.set_permission(Permission::InputCapture, false);
    let agent = lone_mac(capture, &platform).await;
    let mut status = agent.watch_status();
    let denied = tokio::time::timeout(
        TIMEOUT,
        status.wait_for(|s| s.permissions.iter().any(|p| !p.granted)),
    )
    .await
    .unwrap()
    .unwrap()
    .clone();
    assert!(!denied.capturing);
    assert!(!handle.is_started());

    // The native agent restarts when this resolves.
    {
        let granted = agent.permission_granted();
        tokio::pin!(granted);
        assert!(
            tokio::time::timeout(Duration::from_millis(300), &mut granted)
                .await
                .is_err()
        );
        handle.set_starting(false);
        platform.set_permission(Permission::InputCapture, true);
        tokio::time::timeout(TIMEOUT, granted).await.unwrap();
    }
    tokio::time::timeout(TIMEOUT, status.wait_for(|s| s.capturing))
        .await
        .unwrap()
        .unwrap();
    assert!(handle.is_started());
    agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn capture_still_starting_is_started_as_soon_as_it_runs() {
    // As at login on Wayland, before the platform poller sees the
    // permission granted: no input reaches the agent while it holds
    // capture off, and capture starts as soon as the backend can.
    let (capture, handle) = MockCapture::new();
    handle.set_starting(true);
    let platform = MockStatus::default();
    let agent = lone_mac(capture, &platform).await;
    let mut status = agent.watch_status();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!agent.status().capturing);
    assert!(
        handle
            .emit(InputEvent::Key {
                usage: usage::A,
                pressed: true
            })
            .is_err(),
        "no input before capture started"
    );
    handle.set_starting(false);
    tokio::time::timeout(TIMEOUT, status.wait_for(|s| s.capturing))
        .await
        .unwrap()
        .unwrap();
    assert!(handle.is_started());
    agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn display_changes_reach_the_controller() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // The PC's main display shrinks to 600 points tall: the Mac's right
    // edge at canvas y 900 no longer meets it, so the cursor goes to the
    // nearest point of the PC's left edge, its bottom corner.
    win.displays.set(vec![Display {
        id: "main".to_owned(),
        bounds: Rect::new(0.0, 0.0, 1024.0, 600.0),
        scale: 1.0,
        primary: true,
    }]);
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let before = win.emulation.calls().len();
        motion(mac, (1511.0, 500.0), (5.0, 0.0));
        let entered = loop {
            let moved = win.emulation.calls()[before..]
                .iter()
                .find_map(|call| match call {
                    Emulated::MoveTo(p) => Some(*p),
                    _ => None,
                });
            if let Some(p) = moved {
                break p;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the cursor never reached the PC"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        take_back(mac);
        wait_status(mac, |s| s.cursor_on == "mac").await;
        if entered.y < 600.0 {
            assert_eq!(entered.x, 4.0);
            assert!(entered.y > 599.0, "entered at {entered:?}");
            break;
        }
        // The old layout was still in use; retry.
        assert!(
            tokio::time::Instant::now() < deadline,
            "the new displays never arrived"
        );
    }
    // Higher up, the cursor passes straight through the seam.
    motion(mac, (1511.0, 100.0), (5.0, 0.0));
    wait_injected(win, Emulated::MoveTo(Point::new(4.0, 500.0))).await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rearranging_on_one_device_reaches_the_others() {
    let win = unpaired(&win(), Identity::generate().unwrap(), None).await;
    let mac = unpaired(&mac(), Identity::generate().unwrap(), None).await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    connected(&win, "mac").await;

    // Move the PC from the Mac's right side to above it.
    let mac_at = agreed(&mac, &win)
        .await
        .device("mac")
        .and_then(|d| d.offset)
        .unwrap();
    let above = Point::new(mac_at.x, mac_at.y - 1440.0);
    mac.agent
        .rearrange(vec![("win".to_owned(), above)])
        .await
        .unwrap();
    let mut on_win = win.agent.watch_arrangement();
    tokio::time::timeout(
        TIMEOUT,
        on_win.wait_for(|a| a.device("win").and_then(|d| d.offset) == Some(above)),
    )
    .await
    .expect("the PC adopts the new arrangement")
    .unwrap();

    // The new seam works, upwards from the Mac.
    motion(&mac, (700.0, 1.0), (0.0, -5.0));
    wait_injected(&win, Emulated::MoveTo(Point::new(700.0, 1436.0))).await;

    // Overlapping placements are refused as a whole.
    let overlap = mac
        .agent
        .rearrange(vec![(
            "win".to_owned(),
            Point::new(mac_at.x + 10.0, mac_at.y),
        )])
        .await;
    assert!(matches!(
        overlap,
        Err(inputweave::agent::RearrangeError::Invalid(_))
    ));
    assert!(matches!(
        mac.agent
            .rearrange(vec![("ghost".to_owned(), Point::new(0.0, 0.0))])
            .await,
        Err(inputweave::agent::RearrangeError::UnknownDevice(_))
    ));

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_devices_keep_their_displays_in_the_arrangement() {
    let dir = tempfile::tempdir().unwrap();
    let mac_file = dir.path().join("devices.toml");
    let spec = mac();
    let mac_key = dir.path().join("identity");
    let win = unpaired(&win(), Identity::generate().unwrap(), None).await;
    let mac = unpaired(
        &spec,
        Identity::load_or_generate(&mac_key).unwrap(),
        Some(mac_file.clone()),
    )
    .await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    let before = agreed(&mac, &win).await;
    win.agent.shutdown().await;
    mac.agent.shutdown().await;

    // Restarted with the PC gone: it is still on the canvas, offline.
    let mac = unpaired(
        &spec,
        Identity::load_or_generate(&mac_key).unwrap(),
        Some(mac_file),
    )
    .await;
    let mut arrangement = mac.agent.watch_arrangement();
    let now = tokio::time::timeout(
        TIMEOUT,
        arrangement.wait_for(|a| a.device("win").is_some_and(|d| !d.displays.is_empty())),
    )
    .await
    .unwrap()
    .unwrap()
    .clone();
    let win_now = now.device("win").unwrap();
    assert!(!win_now.connected);
    assert_eq!(win_now.displays.len(), 2);
    assert_eq!(win_now.offset, before.device("win").unwrap().offset);
    mac.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn renaming_and_device_options_reach_peers_and_survive_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("devices.toml");
    let key = dir.path().join("identity");
    let spec = mac();
    let win = unpaired(&win(), Identity::generate().unwrap(), None).await;
    let mac = unpaired(
        &spec,
        Identity::load_or_generate(&key).unwrap(),
        Some(file.clone()),
    )
    .await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;

    // Both test agents run on the same OS, so ⌘/Ctrl is not swapped yet.
    assert!(
        !mac.agent
            .status()
            .device("win")
            .unwrap()
            .swap_gui_and_control
    );
    mac.agent.set_swap_gui_and_control("win", true).unwrap();
    wait_status(&mac, |s| {
        s.device("win").is_some_and(|d| d.swap_gui_and_control)
    })
    .await;
    assert!(mac.agent.rename("bad\nname").is_err());
    mac.agent.rename("  Studio Mac ").unwrap();
    wait_status(&mac, |s| s.this_name == "Studio Mac").await;
    mac.agent.shutdown().await;

    // After a restart the Mac keeps its name, and the PC, which remembers
    // it from pairing, learns the new one when they reconnect.
    let mac = unpaired(&spec, Identity::load_or_generate(&key).unwrap(), Some(file)).await;
    assert_eq!(mac.agent.status().this_name, "Studio Mac");
    assert!(
        mac.agent
            .status()
            .device("win")
            .unwrap()
            .swap_gui_and_control
    );
    wait_status(&win, |s| {
        s.device("mac")
            .is_some_and(|d| d.connected && d.name == "Studio Mac")
    })
    .await;

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn each_return_of_control_is_counted() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    for round in 1..=2 {
        mac_enters_win(mac, win).await;
        for k in [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT] {
            key(mac, k, true);
        }
        key(mac, usage::ESCAPE, true);
        for k in [
            usage::ESCAPE,
            usage::LEFT_CONTROL,
            usage::LEFT_ALT,
            usage::LEFT_SHIFT,
        ] {
            key(mac, k, false);
        }
        let status = wait_status(mac, |s| s.returns == round).await;
        assert_eq!(status.last_return, Some(ReturnReason::Hotkey));
        wait_status(win, |s| s.controlled_by.is_none()).await;
    }
    for node in nodes {
        node.agent.shutdown().await;
    }
}

fn text(s: &str) -> iw_platform::ClipboardContent {
    iw_platform::ClipboardContent {
        items: vec![(iw_platform::ClipboardFormat::Text, s.as_bytes().to_vec())],
    }
}

async fn wait_clipboard(node: &Node, expected: &iw_platform::ClipboardContent) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while node.clipboard.content() != *expected {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {expected:?}; clipboard holds {:?}",
            node.clipboard.content()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn take_back(node: &Node) {
    for k in [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT] {
        key(node, k, true);
    }
    key(node, usage::ESCAPE, true);
    for k in [
        usage::ESCAPE,
        usage::LEFT_CONTROL,
        usage::LEFT_ALT,
        usage::LEFT_SHIFT,
    ] {
        key(node, k, false);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_clipboard_follows_the_cursor() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // Copy on the Mac, move to the PC: paste there.
    mac.clipboard.copy(text("from the Mac"), false);
    mac_enters_win(mac, win).await;
    wait_clipboard(win, &text("from the Mac")).await;
    let last = wait_status(win, |s| s.last_clipboard.is_some())
        .await
        .last_clipboard
        .unwrap();
    assert_eq!((last.from.as_str(), last.to.as_str()), ("mac", "win"));

    // Copy on the PC, come back: paste on the Mac.
    win.clipboard.copy(text("from the PC"), false);
    take_back(mac);
    wait_clipboard(mac, &text("from the PC")).await;

    // Nothing new copied on the Mac: going back leaves the PC's alone.
    win.clipboard.copy(text("kept on the PC"), false);
    mac_enters_win(mac, win).await;
    take_back(mac);
    wait_clipboard(mac, &text("kept on the PC")).await;
    assert_eq!(win.clipboard.content(), text("kept on the PC"));

    // A password is held back, and the Mac says why.
    mac.clipboard.copy(text("hunter2"), true);
    mac_enters_win(mac, win).await;
    let status = wait_status(mac, |s| {
        s.last_clipboard
            .as_ref()
            .is_some_and(|c| c.skipped == Some(iw_proto::peer::v1::ClipboardSkip::Sensitive))
    })
    .await;
    assert_eq!(status.last_clipboard.unwrap().to, "win");
    assert_eq!(win.clipboard.content(), text("kept on the PC"));

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn hopping_passes_the_clipboard_through_the_controller() {
    let nodes = desk(&[mac(), mini(), win()]).await;
    let (mac, mini, win) = (&nodes[0], &nodes[1], &nodes[2]);

    mac_enters_win(mac, win).await;
    win.clipboard.copy(text("copied on the PC"), false);
    motion(mac, (0.0, 0.0), (484.0, -895.0)); // to canvas (2000, 5)
    motion(mac, (0.0, 0.0), (0.0, -10.0)); // up into the mini
    wait_status(mini, |s| s.controlled_by.as_deref() == Some("mac")).await;
    wait_clipboard(mini, &text("copied on the PC")).await;
    // The Mac only passed it on.
    assert_ne!(mac.clipboard.content(), text("copied on the PC"));

    // Turned off on the mini: what is copied there stays there.
    mini.agent.set_clipboard_sync(false);
    wait_status(mini, |s| !s.clipboard_sync).await;
    mini.clipboard.copy(text("private"), false);
    take_back(mac);
    wait_status(mac, |s| s.cursor_on == "mac").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_ne!(mac.clipboard.content(), text("private"));

    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// Text over the eager limit (8 MiB), so it is offered rather than sent.
fn large(s: &str) -> iw_platform::ClipboardContent {
    let mut data = s.as_bytes().to_vec();
    data.resize(9 * 1024 * 1024, b'.');
    iw_platform::ClipboardContent {
        items: vec![(iw_platform::ClipboardFormat::Text, data)],
    }
}

/// Waits until `node`'s clipboard promises an offer.
async fn wait_promise(node: &Node) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while node.clipboard.promised().is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for a promise; clipboard holds {} bytes",
            node.clipboard.content().len()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// As if an app on `node` pasted.
async fn paste(node: &Node) -> iw_platform::ClipboardContent {
    let clipboard = node.clipboard.clone();
    tokio::task::spawn_blocking(move || clipboard.paste())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_large_clipboard_is_offered_and_pulled_when_pasted() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // Copied on the Mac, too large to send: the PC promises it, and no
    // byte of it has moved.
    let big = large("from the Mac");
    mac.clipboard.copy(big.clone(), false);
    mac_enters_win(mac, win).await;
    wait_promise(win).await;
    assert_eq!(
        win.clipboard.promised(),
        Some(vec![iw_platform::ClipboardFormat::Text])
    );
    assert!(win.clipboard.content().is_empty());
    let offered = wait_status(win, |s| s.lazy_clipboard.received == 1).await;
    assert_eq!(offered.lazy_clipboard.bytes_pulled, 0);
    let last = offered.last_clipboard.unwrap();
    assert_eq!((last.from.as_str(), last.to.as_str()), ("mac", "win"));
    let status = mac.agent.status();
    assert_eq!(
        (status.lazy_clipboard.offered, status.lazy_clipboard.served),
        (1, 0)
    );

    // An app on the PC pastes: only now does it come over.
    assert_eq!(paste(win).await, big);
    let pulled = wait_status(win, |s| s.lazy_clipboard.pulled == 1).await;
    assert_eq!(pulled.lazy_clipboard.bytes_pulled, big.len() as u64);
    wait_status(mac, |s| s.lazy_clipboard.served == 1).await;
    // Pasting again needs nothing more from the Mac.
    assert_eq!(paste(win).await, big);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(mac.agent.status().lazy_clipboard.served, 1);
    // What the PC took is not sent back as the cursor returns.
    take_back(mac);
    wait_status(mac, |s| s.cursor_on == "mac").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(mac.clipboard.content(), big);
    assert_eq!(mac.agent.status().lazy_clipboard.received, 0);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_offer_copied_over_on_its_device_cannot_be_pulled() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    mac.clipboard.copy(large("first"), false);
    mac_enters_win(mac, win).await;
    wait_promise(win).await;
    // Something else is copied on the Mac before anything is pasted.
    mac.clipboard.copy(text("second"), false);
    assert!(paste(win).await.is_empty());
    let status = wait_status(win, |s| s.lazy_clipboard.failed == 1).await;
    assert_eq!(
        status.lazy_clipboard.last.as_deref(),
        Some("CLIPBOARD_SKIP_CHANGED")
    );
    assert!(win.clipboard.content().is_empty());
    assert_eq!(mac.agent.status().lazy_clipboard.served, 0);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pull_goes_through_the_controller_to_the_device_offered_from() {
    let nodes = desk(&[mac(), mini(), win()]).await;
    let (mac, mini, win) = (&nodes[0], &nodes[1], &nodes[2]);

    mac_enters_win(mac, win).await;
    let big = large("copied on the PC");
    win.clipboard.copy(big.clone(), false);
    motion(mac, (0.0, 0.0), (484.0, -895.0)); // to canvas (2000, 5)
    motion(mac, (0.0, 0.0), (0.0, -10.0)); // up into the mini
    wait_status(mini, |s| s.controlled_by.as_deref() == Some("mac")).await;
    wait_promise(mini).await;
    assert_eq!(paste(mini).await, big);
    wait_status(win, |s| s.lazy_clipboard.served == 1).await;
    // The Mac only passed the offer and the content on.
    assert_ne!(mac.clipboard.content(), big);
    let status = mac.agent.status();
    assert_eq!(
        (status.lazy_clipboard.received, status.lazy_clipboard.pulled),
        (0, 0)
    );

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn large_clipboards_follow_the_rules_of_small_ones() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // A password manager's copy is never offered.
    mac.clipboard.copy(large("hunter2"), true);
    mac_enters_win(mac, win).await;
    let status = wait_status(mac, |s| {
        s.last_clipboard
            .as_ref()
            .is_some_and(|c| c.skipped == Some(iw_proto::peer::v1::ClipboardSkip::Sensitive))
    })
    .await;
    assert_eq!(status.lazy_clipboard.offered, 0);
    take_back(mac);
    wait_status(mac, |s| s.cursor_on == "mac").await;

    // Turned off with the PC: nothing is offered to it either.
    clipboard_with(mac, "win", false).await;
    mac.clipboard.copy(large("not for the PC"), false);
    mac_enters_win(mac, win).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(win.clipboard.promised().is_none());
    assert_eq!(mac.agent.status().lazy_clipboard.offered, 0);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_on_the_first_protocol_gets_clipboards_whole_or_not_at_all() {
    let nodes = desk_with(&[mac(), win()], |spec| Options {
        peer_protocol_max: (spec.id == "win").then_some(1),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    mac.clipboard.copy(large("too large for it"), false);
    mac_enters_win(mac, win).await;
    let status = wait_status(mac, |s| {
        s.last_clipboard
            .as_ref()
            .is_some_and(|c| c.skipped == Some(iw_proto::peer::v1::ClipboardSkip::TooLarge))
    })
    .await;
    assert_eq!(status.lazy_clipboard.offered, 0);
    assert!(win.clipboard.promised().is_none());

    // Small ones move as they always did.
    take_back(mac);
    wait_status(mac, |s| s.cursor_on == "mac").await;
    mac.clipboard.copy(text("small"), false);
    mac_enters_win(mac, win).await;
    wait_clipboard(win, &text("small")).await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clipboard_that_cannot_promise_takes_an_offer_at_once() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    win.clipboard.refuse_promises();
    let big = large("from the Mac");
    mac.clipboard.copy(big.clone(), false);
    mac_enters_win(mac, win).await;
    wait_clipboard(win, &big).await;
    let status = wait_status(win, |s| s.lazy_clipboard.pulled == 1).await;
    assert_eq!(status.lazy_clipboard.bytes_pulled, big.len() as u64);
    assert!(win.clipboard.promised().is_none());

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn content_pulled_at_once_does_not_replace_a_copy_made_meanwhile() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // The PC's clipboard cannot promise, and the Mac takes its time to
    // read: the pull is still on its way when something is copied on the
    // PC.
    win.clipboard.refuse_promises();
    mac.clipboard.delay_reads(Duration::from_millis(600));
    mac.clipboard.copy(large("from the Mac"), false);
    mac_enters_win(mac, win).await;
    wait_status(win, |s| s.lazy_clipboard.received == 1).await;
    win.clipboard.copy(text("copied on the PC"), false);
    let status = wait_status(win, |s| s.lazy_clipboard.superseded == 1).await;
    assert_eq!(status.lazy_clipboard.pulled, 0);
    assert_eq!(win.clipboard.content(), text("copied on the PC"));
    wait_status(mac, |s| s.lazy_clipboard.served == 1).await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_offer_larger_than_the_clipboard_takes_is_not_pulled() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // As the Windows broker, which takes 64 MiB at most.
    win.clipboard.refuse_promises();
    win.clipboard.limit_writes(9 * 1024 * 1024 - 1);
    win.clipboard.copy(text("kept on the PC"), false);
    mac.clipboard.copy(large("from the Mac"), false);
    mac_enters_win(mac, win).await;
    let status = wait_status(win, |s| {
        s.last_clipboard
            .as_ref()
            .is_some_and(|c| c.skipped == Some(iw_proto::peer::v1::ClipboardSkip::TooLarge))
    })
    .await;
    assert_eq!(status.lazy_clipboard.received, 0);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(mac.agent.status().lazy_clipboard.served, 0);
    assert_eq!(win.clipboard.content(), text("kept on the PC"));

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn too_large_for_one_device_is_still_offered_to_the_next() {
    // The PC speaks the first protocol and takes no offers; the mini does.
    let nodes = desk_with(&[mac(), win(), mini()], |spec| Options {
        peer_protocol_max: (spec.id == "win").then_some(1),
        ..Options::default()
    })
    .await;
    let (mac, win, mini) = (&nodes[0], &nodes[1], &nodes[2]);

    let big = large("from the Mac");
    mac.clipboard.copy(big.clone(), false);
    mac_enters_win(mac, win).await;
    wait_status(mac, |s| {
        s.last_clipboard.as_ref().is_some_and(|c| {
            c.to == "win" && c.skipped == Some(iw_proto::peer::v1::ClipboardSkip::TooLarge)
        })
    })
    .await;
    take_back(mac);
    wait_status(mac, |s| s.cursor_on == "mac").await;
    // Nothing copied since, and still offered to the mini.
    mac.agent.jump_to("mini").await.unwrap();
    wait_promise(mini).await;
    assert_eq!(paste(mini).await, big);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_promise_can_be_pasted_the_moment_it_is_made() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    let big = large("from the Mac");
    mac.clipboard.copy(big.clone(), false);
    mac_enters_win(mac, win).await;
    // An app pastes as soon as the clipboard promises, before the engine
    // has heard the promise was made.
    let clipboard = win.clipboard.clone();
    let pasted = tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + TIMEOUT;
        while clipboard.promised().is_none() {
            assert!(std::time::Instant::now() < deadline, "never promised");
            std::hint::spin_loop();
        }
        clipboard.paste()
    })
    .await
    .unwrap();
    assert_eq!(pasted, big);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// Turns the clipboard off or on with `device` on `node`, and waits until
/// it applies.
async fn clipboard_with(node: &Node, device: &str, on: bool) {
    node.agent.set_device_sharing(device, !on, false).unwrap();
    wait_status(node, |s| {
        s.device(device).is_some_and(|d| d.clipboard_off != on)
    })
    .await;
}

/// The cursor goes to the PC and back, with `on_mac` copied on the Mac
/// first and `on_pc` copied on the PC while the cursor is there.
async fn round_trip(mac: &Node, win: &Node, on_mac: &str, on_pc: &str) {
    mac.clipboard.copy(text(on_mac), false);
    mac_enters_win(mac, win).await;
    win.clipboard.copy(text(on_pc), false);
    take_back(mac);
    wait_status(mac, |s| s.cursor_on == "mac").await;
    wait_status(win, |s| s.controlled_by.is_none()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_clipboard_stays_put_with_a_device_it_is_turned_off_with() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // Turned off on the Mac, the controller: neither clipboard moves.
    clipboard_with(mac, "win", false).await;
    round_trip(mac, win, "M1", "W1").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(mac.clipboard.content(), text("M1"));
    assert_eq!(win.clipboard.content(), text("W1"));

    // On again, what was held back moves as the cursor next does.
    clipboard_with(mac, "win", true).await;
    mac_enters_win(mac, win).await;
    wait_clipboard(win, &text("M1")).await;
    win.clipboard.copy(text("W2"), false);
    take_back(mac);
    wait_clipboard(mac, &text("W2")).await;

    // Turned off on the PC, the device controlled: it neither takes the
    // Mac's clipboard nor gives its own, and the Mac needs to know nothing.
    clipboard_with(win, "mac", false).await;
    assert!(!mac.agent.status().device("win").unwrap().clipboard_off);
    round_trip(mac, win, "M3", "W3").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(mac.clipboard.content(), text("M3"));
    assert_eq!(win.clipboard.content(), text("W3"));

    // On again there too: what is copied next moves.
    clipboard_with(win, "mac", true).await;
    mac.clipboard.copy(text("M4"), false);
    mac_enters_win(mac, win).await;
    wait_clipboard(win, &text("M4")).await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// Two paired devices, each receiving files into its own folder under
/// `dir`, with fixed ports so a restarted one is found again.
async fn file_pair(dir: &std::path::Path) -> (Node, Node, std::path::PathBuf, std::path::PathBuf) {
    let mac_folder = dir.join("mac-received");
    let win_folder = dir.join("win-received");
    let mac = start_receiving(&mac(), dir, "mac", mac_folder.clone(), None).await;
    let win = start_receiving(&win(), dir, "win", win_folder.clone(), None).await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    connected(&win, "mac").await;
    (mac, win, mac_folder, win_folder)
}

/// An unarranged device whose identity, devices file and transfer history
/// live in `dir` under `name`, receiving files into `folder`.
async fn start_receiving(
    spec: &Spec,
    dir: &std::path::Path,
    name: &str,
    folder: std::path::PathBuf,
    listen: Option<SocketAddr>,
) -> Node {
    start_kept(spec, dir, name, kept_in(dir, name, folder), listen).await
}

/// Options keeping a device's devices file and transfer history in `dir`
/// under `name`, receiving files into `folder`.
fn kept_in(dir: &std::path::Path, name: &str, folder: std::path::PathBuf) -> Options {
    Options {
        devices_file: Some(dir.join(format!("{name}-devices.toml"))),
        transfers_file: Some(dir.join(format!("{name}-transfers.toml"))),
        receive_folder: Some(folder),
        ..Options::default()
    }
}

/// An unarranged device whose identity lives in `dir` under `name`.
async fn start_kept(
    spec: &Spec,
    dir: &std::path::Path,
    name: &str,
    options: Options,
    listen: Option<SocketAddr>,
) -> Node {
    let identity = Identity::load_or_generate(&dir.join(format!("{name}-identity"))).unwrap();
    let mut node = start_with_listen(spec, identity, options, listen).await;
    node.clipboard = MockClipboard::default();
    node
}

async fn start_with_listen(
    spec: &Spec,
    identity: Identity,
    options: Options,
    listen: Option<SocketAddr>,
) -> Node {
    let config = Config {
        device: DeviceConfig {
            id: spec.id.to_owned(),
            name: spec.name.to_owned(),
        },
        network: NetworkConfig {
            listen: listen.unwrap_or_else(|| "127.0.0.1:0".parse().unwrap()),
            discovery: false,
            ..NetworkConfig::default()
        },
        paths: PathsConfig::default(),
        backend: BackendConfig::Mock {
            displays: vec![],
            folders: None,
        },
        peers: vec![],
        layout: vec![],
        clipboard: Default::default(),
    };
    let (capture, handle) = MockCapture::new();
    let emulation = RecordingEmulation::default();
    let displays = SharedDisplays::new(spec.displays.clone());
    let clipboard = MockClipboard::default();
    let pictures = MockDesktopPictures::default();
    let status = MockStatus::granted();
    let backends = Backends {
        capture: Box::new(capture),
        emulation: Box::new(emulation.clone()),
        displays: Box::new(displays.clone()),
        pictures: Box::new(pictures.clone()),
        status: Box::new(status.clone()),
        clipboard: Some(Box::new(clipboard.clone())),
    };
    let agent = agent::start(config, Arc::new(identity), backends, options)
        .await
        .expect("agent starts");
    Node {
        agent,
        capture: handle,
        emulation,
        displays,
        clipboard,
        pictures,
        status,
    }
}

async fn wait_transfer(
    node: &Node,
    id: &str,
    done: impl Fn(&inputweave::agent::TransferStatus) -> bool,
) -> inputweave::agent::TransferStatus {
    let mut rx = node.agent.watch_transfers();
    let found = tokio::time::timeout(
        TIMEOUT * 3,
        rx.wait_for(|list| list.iter().any(|t| t.id == id && done(t))),
    )
    .await
    .unwrap_or_else(|_| panic!("timed out; transfers: {:#?}", node.agent.transfers()))
    .unwrap();
    found.iter().find(|t| t.id == id).unwrap().clone()
}

fn finished(t: &inputweave::agent::TransferStatus) -> bool {
    t.state.finished()
}

#[tokio::test(flavor = "multi_thread")]
async fn files_arrive_whole_and_keep_existing_ones() {
    use inputweave::agent::TransferState;

    let dir = tempfile::tempdir().unwrap();
    let (mac, win, _, win_folder) = file_pair(dir.path()).await;
    let source = dir.path().join("報告.pdf");
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&source, &data).unwrap();

    for expected in ["報告.pdf", "報告 (1).pdf"] {
        let ids = mac
            .agent
            .send_files("win", vec![source.clone()])
            .await
            .unwrap();
        let sent = wait_transfer(&mac, &ids[0], finished).await;
        assert_eq!(sent.state, TransferState::Done, "{sent:?}");
        let received = wait_transfer(&win, &ids[0], finished).await;
        assert_eq!(received.state, TransferState::Done);
        let path = received.path.unwrap();
        assert_eq!(path, win_folder.join(expected));
        assert_eq!(std::fs::read(&path).unwrap(), data);
    }
    // No part files are left behind.
    let leftovers: Vec<_> = std::fs::read_dir(&win_folder)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
        .collect();
    assert!(leftovers.is_empty());

    assert!(
        mac.agent
            .send_files("ghost", vec![source.clone()])
            .await
            .is_err()
    );
    // Nothing there: the request is refused, nothing is listed.
    let listed = mac.agent.transfers().len();
    assert!(matches!(
        mac.agent
            .send_files("win", vec![dir.path().join("gone")])
            .await,
        Err(inputweave::agent::SendError::NotAFile(_))
    ));
    assert_eq!(mac.agent.transfers().len(), listed);

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn receivers_can_ask_first_or_not_receive_at_all() {
    use inputweave::agent::{FileSettings, TransferState};
    use iw_proto::peer::v1::FileDecline;

    let dir = tempfile::tempdir().unwrap();
    let (mac, win, _, win_folder) = file_pair(dir.path()).await;
    let source = dir.path().join("notes.txt");
    std::fs::write(&source, "hello").unwrap();

    win.agent.set_file_settings(FileSettings {
        folder: Some(win_folder.clone()),
        ask: true,
    });
    let declined = mac
        .agent
        .send_files("win", vec![source.clone()])
        .await
        .unwrap();
    wait_transfer(&win, &declined[0], |t| {
        t.state == TransferState::WaitingForAnswer
    })
    .await;
    assert!(win.agent.answer_file_offer(&declined[0], false));
    let sent = wait_transfer(&mac, &declined[0], finished).await;
    assert_eq!(sent.state, TransferState::Declined(FileDecline::Rejected));

    let accepted = mac
        .agent
        .send_files("win", vec![source.clone()])
        .await
        .unwrap();
    wait_transfer(&win, &accepted[0], |t| {
        t.state == TransferState::WaitingForAnswer
    })
    .await;
    assert!(win.agent.answer_file_offer(&accepted[0], true));
    assert_eq!(
        wait_transfer(&mac, &accepted[0], finished).await.state,
        TransferState::Done
    );
    assert_eq!(
        std::fs::read(win_folder.join("notes.txt")).unwrap(),
        b"hello"
    );

    // Cancelled by the sender while the receiver's user decides.
    let cancelled = mac
        .agent
        .send_files("win", vec![source.clone()])
        .await
        .unwrap();
    wait_transfer(&win, &cancelled[0], |t| {
        t.state == TransferState::WaitingForAnswer
    })
    .await;
    assert!(mac.agent.cancel_transfer(&cancelled[0]));
    assert_eq!(
        wait_transfer(&win, &cancelled[0], finished).await.state,
        TransferState::Cancelled
    );

    // No folder: files are turned away.
    win.agent.set_file_settings(FileSettings {
        folder: None,
        ask: false,
    });
    let refused = mac.agent.send_files("win", vec![source]).await.unwrap();
    assert_eq!(
        wait_transfer(&mac, &refused[0], finished).await.state,
        TransferState::Declined(FileDecline::Unavailable)
    );

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_transfer_cut_off_midway_resumes_where_it_stopped() {
    use inputweave::agent::TransferState;

    let dir = tempfile::tempdir().unwrap();
    // A fixed port, so the Mac finds the PC again after it restarts.
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let mac_folder = dir.path().join("mac-received");
    let win_folder = dir.path().join("win-received");
    let pc_spec = win();
    let mac = start_receiving(&mac(), dir.path(), "mac", mac_folder, None).await;
    let win = start_receiving(&pc_spec, dir.path(), "win", win_folder.clone(), Some(port)).await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;

    let source = dir.path().join("big.bin");
    let data: Vec<u8> = (0..160_000_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();
    std::fs::write(&source, &data).unwrap();
    let id = mac
        .agent
        .send_files("win", vec![source])
        .await
        .unwrap()
        .remove(0);
    let partway = wait_transfer(&win, &id, |t| t.done > 0).await;
    assert!(
        partway.done < partway.size,
        "the transfer finished too fast to cut off"
    );
    win.agent.abort().await;

    let part = std::fs::read_dir(&win_folder)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().ends_with(".part"))
        .expect("the partial file is kept");
    let kept = part.metadata().unwrap().len();
    assert!(kept > 0 && kept < data.len() as u64);
    wait_transfer(&mac, &id, |t| {
        t.state == TransferState::WaitingForConnection
    })
    .await;
    // The aborted agent's endpoint lets go of its socket a moment after
    // abort returns, later still on a slow machine.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while std::net::UdpSocket::bind(port).is_err() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the port stayed taken"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let win = start_receiving(&pc_spec, dir.path(), "win", win_folder.clone(), Some(port)).await;
    // The receiver picks up from what it kept instead of starting over.
    let resumed = wait_transfer(&win, &id, |t| {
        t.state == TransferState::Transferring || t.state.finished()
    })
    .await;
    assert!(resumed.done >= kept, "{} < {kept}", resumed.done);
    assert_eq!(
        wait_transfer(&mac, &id, finished).await.state,
        TransferState::Done
    );
    let received = wait_transfer(&win, &id, finished).await;
    assert_eq!(std::fs::read(received.path.unwrap()).unwrap(), data);

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

/// `count` files of `size` bytes each, different in content, in `dir`.
fn some_files(
    dir: &std::path::Path,
    count: usize,
    size: usize,
) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    (0..count)
        .map(|n| {
            let path = dir.join(format!("file {n}.bin"));
            let data: Vec<u8> = (0..size)
                .map(|i| ((i as u32).wrapping_mul(2654435761).wrapping_add(n as u32) >> 24) as u8)
                .collect();
            std::fs::write(&path, &data).unwrap();
            (path, data)
        })
        .collect()
}

/// Checks every change of `node`'s transfer list until all of `ids`, sent
/// in that order to one device, have finished: at most two at a time have
/// begun and not finished, and those that began came first. Returns the
/// largest number of them seen waiting at once.
fn watch_the_queue(node: &Node, ids: Vec<String>) -> tokio::task::JoinHandle<usize> {
    use inputweave::agent::TransferState;

    let mut rx = node.agent.watch_transfers();
    tokio::spawn(async move {
        let mut most_queued = 0;
        loop {
            let list = rx.borrow_and_update().clone();
            let states: Vec<&TransferState> = ids
                .iter()
                .map(|id| &list.iter().find(|t| t.id == *id).unwrap().state)
                .collect();
            let began: Vec<bool> = states
                .iter()
                .map(|s| **s != TransferState::Queued)
                .collect();
            assert!(
                began.windows(2).all(|w| w[0] || !w[1]),
                "a file began before one sent earlier: {states:?}"
            );
            let going = states
                .iter()
                .filter(|s| ***s != TransferState::Queued && !s.finished())
                .count();
            assert!(going <= 2, "{going} files going at once: {states:?}");
            most_queued = most_queued.max(began.iter().filter(|b| !**b).count());
            if states.iter().all(|s| s.finished()) {
                return most_queued;
            }
            rx.changed().await.unwrap();
        }
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn queued_files_go_two_at_a_time_in_order_once_the_device_is_back() {
    use inputweave::agent::TransferState;

    let dir = tempfile::tempdir().unwrap();
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let win_folder = dir.path().join("win-received");
    let pc_spec = win();
    let mac = start_receiving(&mac(), dir.path(), "mac", dir.path().join("mac-in"), None).await;
    let win = start_receiving(&pc_spec, dir.path(), "win", win_folder.clone(), Some(port)).await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    win.agent.shutdown().await;
    wait_status(&mac, |s| s.device("win").is_some_and(|d| !d.connected)).await;

    // Sent while the PC is off: two begin and wait for it, the rest queue.
    let files = some_files(dir.path(), 10, 3_000_000);
    let ids = mac
        .agent
        .send_files("win", files.iter().map(|(p, _)| p.clone()).collect())
        .await
        .unwrap();
    let queue = watch_the_queue(&mac, ids.clone());
    for id in &ids[..2] {
        wait_transfer(&mac, id, |t| t.state == TransferState::WaitingForConnection).await;
    }
    let list = mac.agent.transfers();
    for id in &ids[2..] {
        let t = list.iter().find(|t| t.id == *id).unwrap();
        assert_eq!(t.state, TransferState::Queued, "{t:?}");
    }

    // Back: they go in the order they were sent, and all arrive whole.
    let win = start_receiving(&pc_spec, dir.path(), "win", win_folder.clone(), Some(port)).await;
    assert_eq!(queue.await.unwrap(), 8);
    for (id, (_, data)) in ids.iter().zip(&files) {
        let sent = wait_transfer(&mac, id, finished).await;
        assert_eq!(sent.state, TransferState::Done, "{sent:?}");
        let received = wait_transfer(&win, id, finished).await;
        assert_eq!(std::fs::read(received.path.unwrap()).unwrap(), *data);
    }

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queued_file_can_be_cancelled_before_its_turn() {
    use inputweave::agent::{FileSettings, TransferState};

    let dir = tempfile::tempdir().unwrap();
    let (mac, win, _, win_folder) = file_pair(dir.path()).await;
    // The PC asks first, so the first two hold their places until answered.
    win.agent.set_file_settings(FileSettings {
        folder: Some(win_folder.clone()),
        ask: true,
    });
    let files = some_files(dir.path(), 4, 100_000);
    let ids = mac
        .agent
        .send_files("win", files.iter().map(|(p, _)| p.clone()).collect())
        .await
        .unwrap();
    let asking = |t: &inputweave::agent::TransferStatus| t.state == TransferState::WaitingForAnswer;
    wait_transfer(&win, &ids[0], asking).await;
    wait_transfer(&win, &ids[1], asking).await;
    let list = mac.agent.transfers();
    for id in &ids[2..] {
        let t = list.iter().find(|t| t.id == *id).unwrap();
        assert_eq!(t.state, TransferState::Queued, "{t:?}");
    }

    assert!(mac.agent.cancel_transfer(&ids[2]));
    assert_eq!(
        wait_transfer(&mac, &ids[2], finished).await.state,
        TransferState::Cancelled
    );
    // One answer covers the batch; the cancelled file's place goes to the
    // one behind it, which is not asked about again.
    assert!(win.agent.answer_file_offer(&ids[0], true));
    assert!(!win.agent.answer_file_offer(&ids[1], true));
    for n in [0, 1, 3] {
        assert_eq!(
            wait_transfer(&mac, &ids[n], finished).await.state,
            TransferState::Done
        );
        let received = wait_transfer(&win, &ids[n], finished).await;
        assert_eq!(std::fs::read(received.path.unwrap()).unwrap(), files[n].1);
    }
    // The PC never heard of the cancelled one.
    assert!(win.agent.transfers().iter().all(|t| t.id != ids[2]));
    assert!(!mac.agent.cancel_transfer(&ids[2]));

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

/// What a transfer's history keeps of it, to the millisecond.
fn kept(t: &inputweave::agent::TransferStatus) -> impl PartialEq + std::fmt::Debug + use<> {
    let ms = |at: std::time::SystemTime| {
        at.duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    };
    (
        t.id.clone(),
        t.direction,
        t.device.clone(),
        t.name.clone(),
        t.size,
        t.state.clone(),
        t.path.clone(),
        ms(t.started),
        t.finished.map(ms),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn the_history_keeps_the_last_hundred_finished_transfers_and_those_going() {
    use inputweave::agent::{FileSettings, TransferState, TransferStatus};

    let dir = tempfile::tempdir().unwrap();
    let mac_spec = mac();
    let (mac, win, mac_folder, win_folder) = file_pair(dir.path()).await;
    let asking = |t: &TransferStatus| t.state == TransferState::WaitingForAnswer;
    // The PC's user is asked about the first file and never answers, so it
    // is still going after all the others.
    win.agent.set_file_settings(FileSettings {
        folder: Some(win_folder.clone()),
        ask: true,
    });
    let first = dir.path().join("first.txt");
    std::fs::write(&first, "first").unwrap();
    let held = mac
        .agent
        .send_files("win", vec![first])
        .await
        .unwrap()
        .remove(0);
    wait_transfer(&win, &held, asking).await;
    win.agent.set_file_settings(FileSettings {
        folder: Some(win_folder.clone()),
        ask: false,
    });

    // 105 more arrive, one after another beside the one held.
    let many = dir.path().join("many");
    std::fs::create_dir(&many).unwrap();
    let files = some_files(&many, 105, 1000);
    let ids = mac
        .agent
        .send_files("win", files.iter().map(|(p, _)| p.clone()).collect())
        .await
        .unwrap();
    let last = ids.last().unwrap();
    assert_eq!(
        wait_transfer(&mac, last, finished).await.state,
        TransferState::Done
    );
    wait_transfer(&win, last, finished).await;

    // Each side lists the 100 that finished last, newest first, and the
    // one still going; the five that finished first are gone.
    let expected: Vec<&str> = ids[5..]
        .iter()
        .rev()
        .chain([&held])
        .map(String::as_str)
        .collect();
    for node in [&mac, &win] {
        let list = node.agent.transfers();
        let listed: Vec<&str> = list.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(listed, expected);
        assert!(list[..100].iter().all(|t| t.state == TransferState::Done));
        assert!(asking(&list[100]), "{:?}", list[100]);
    }
    // Their files are not touched.
    let received = std::fs::read_dir(&win_folder).unwrap().count();
    assert_eq!(received, 105);
    assert!(files.iter().all(|(path, _)| path.exists()));

    // The history comes back when the Mac restarts, without the one that
    // was still going.
    let before: Vec<_> = mac.agent.transfers()[..100].iter().map(kept).collect();
    mac.agent.shutdown().await;
    let mac = start_receiving(&mac_spec, dir.path(), "mac", mac_folder, None).await;
    let after: Vec<_> = mac.agent.transfers().iter().map(kept).collect();
    assert_eq!(after, before);

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn finished_transfers_are_removed_from_the_history_over_ipc() {
    use inputweave::agent::{FileSettings, TransferState, TransferStatus};

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let mac_spec = mac();
    let win_folder = dir.path().join("win-received");
    let mac_options = || Options {
        ipc_socket: Some(socket.clone()),
        ..kept_in(dir.path(), "mac", dir.path().join("mac-received"))
    };
    let win = start_receiving(&win(), dir.path(), "win", win_folder.clone(), None).await;
    let mac = start_kept(&mac_spec, dir.path(), "mac", mac_options(), None).await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    let asking = |t: &TransferStatus| t.state == TransferState::WaitingForAnswer;

    // Three files arrive; the PC's user is asked about a fourth.
    let files = some_files(dir.path(), 4, 1000);
    let ids = mac
        .agent
        .send_files("win", files[..3].iter().map(|(p, _)| p.clone()).collect())
        .await
        .unwrap();
    for id in &ids {
        assert_eq!(
            wait_transfer(&mac, id, finished).await.state,
            TransferState::Done
        );
    }
    win.agent.set_file_settings(FileSettings {
        folder: Some(win_folder.clone()),
        ask: true,
    });
    let held = mac
        .agent
        .send_files("win", vec![files[3].0.clone()])
        .await
        .unwrap()
        .remove(0);
    wait_transfer(&mac, &held, asking).await;

    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    assert_eq!(refusal(client.remove_transfer(&held).await), "not_finished");
    assert_eq!(
        refusal(client.remove_transfer("18f0a-1-1").await),
        "not_found"
    );
    let left = client.remove_transfer(&ids[1]).await.unwrap();
    let listed: Vec<&str> = left
        .transfers
        .iter()
        .map(|t| t.transfer_id.as_str())
        .collect();
    assert_eq!(listed, [&held, &ids[2], &ids[0]]);
    // From the command line too.
    let output = run_ctl(&socket, &["transfers", "--remove", &ids[2]]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains(&ids[2]) && stdout.contains(&ids[0]) && stdout.contains(&held),
        "{stdout}"
    );
    drop(client);

    // After a restart only what was left comes back: not what was removed,
    // nor what was still going.
    mac.agent.shutdown().await;
    let saved = std::fs::read_to_string(dir.path().join("mac-transfers.toml")).unwrap();
    assert!(saved.contains(&ids[0]), "{saved}");
    for gone in [&ids[1], &ids[2], &held] {
        assert!(!saved.contains(gone.as_str()), "{saved}");
    }
    let mac = start_kept(&mac_spec, dir.path(), "mac", mac_options(), None).await;
    let restored: Vec<String> = mac.agent.transfers().into_iter().map(|t| t.id).collect();
    assert_eq!(restored, [ids[0].clone()]);

    // Clearing keeps what is going, here and on the PC, and every file.
    connected(&mac, "win").await;
    let going = mac
        .agent
        .send_files("win", vec![files[3].0.clone()])
        .await
        .unwrap()
        .remove(0);
    wait_transfer(&win, &going, asking).await;
    let output = run_ctl(&socket, &["transfers", "--clear"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let listed: Vec<String> = mac.agent.transfers().into_iter().map(|t| t.id).collect();
    assert_eq!(listed, std::slice::from_ref(&going));
    win.agent.clear_transfers();
    let listed: Vec<String> = win.agent.transfers().into_iter().map(|t| t.id).collect();
    assert_eq!(listed, [going]);
    for (path, data) in &files[..3] {
        assert!(path.exists());
        let name = path.file_name().unwrap();
        assert_eq!(std::fs::read(win_folder.join(name)).unwrap(), *data);
    }

    mac.agent.shutdown().await;
    let mac = start_kept(&mac_spec, dir.path(), "mac", mac_options(), None).await;
    assert!(mac.agent.transfers().is_empty());
    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn input_goes_through_promptly_while_large_files_are_sent() {
    use inputweave::agent::TransferState;

    let dir = tempfile::tempdir().unwrap();
    let nodes = desk_with(&[mac(), win()], |spec| Options {
        receive_folder: Some(dir.path().join(format!("{}-received", spec.id))),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    mac_enters_win(mac, win).await;

    let files = some_files(dir.path(), 3, 40_000_000);
    let ids = mac
        .agent
        .send_files("win", files.iter().map(|(p, _)| p.clone()).collect())
        .await
        .unwrap();
    wait_transfer(win, &ids[0], |t| t.done > 0).await;

    // Presses and releases, each one waited for before the next, until the
    // files are all sent.
    let sending = || {
        mac.agent
            .transfers()
            .iter()
            .any(|t| t.state == TransferState::Transferring)
    };
    let all_sent = || mac.agent.transfers().iter().all(finished);
    let mut delays = Vec::new();
    let mut during = 0;
    let mut pressed = false;
    while !(all_sent() && !pressed) {
        pressed = !pressed;
        let expected = Emulated::Button(Button::Left, pressed);
        let delivered = || {
            win.emulation
                .calls()
                .iter()
                .filter(|c| **c == expected)
                .count()
        };
        let before = delivered();
        let was_sending = sending();
        let started = std::time::Instant::now();
        mac.capture
            .emit(InputEvent::Button {
                button: Button::Left,
                pressed,
            })
            .unwrap();
        while delivered() == before {
            assert!(
                started.elapsed() < TIMEOUT,
                "the button never arrived; injected: {:?}",
                win.emulation.calls()
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        delays.push(started.elapsed());
        if was_sending && sending() {
            during += 1;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(during >= 10, "only {during} presses while files were sent");
    // Loopback is far faster than this; it only catches input that waits
    // for file data. The slowest tenth may have waited for a machine busy
    // with other tests instead, but none for a whole file to go.
    delays.sort();
    let mostly = delays[delays.len() * 9 / 10];
    assert!(mostly < Duration::from_millis(200), "{delays:?}");
    assert!(
        delays[delays.len() - 1] < Duration::from_secs(1),
        "{delays:?}"
    );

    for (id, (_, data)) in ids.iter().zip(&files) {
        assert_eq!(
            wait_transfer(mac, id, finished).await.state,
            TransferState::Done
        );
        let received = wait_transfer(win, id, finished).await;
        assert_eq!(std::fs::read(received.path.unwrap()).unwrap(), *data);
    }
    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn diagnostics_describe_the_agent_for_bug_reports() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let log = dir.path().join("agent.log");
    std::fs::write(&log, "an older line\nthe last line\n").unwrap();
    let spec = mac();
    let win_fingerprint = Identity::generate().unwrap().fingerprint();
    let known = [Known {
        id: "win",
        name: "Windows PC",
        fingerprint: win_fingerprint,
        address: None,
    }];
    let options = Options {
        ipc_socket: Some(socket.clone()),
        log_files: vec![log, dir.path().join("missing.log")],
        agent_digest: "ab".repeat(32),
        ..Options::default()
    };
    let mac = start_with(
        &spec,
        Identity::generate().unwrap(),
        &[&spec],
        &known,
        options,
    )
    .await;

    let report = mac.agent.diagnostics().await;
    for expected in [
        "InputWeave diagnostics",
        &format!("Agent SHA-256: {}\n", "ab".repeat(32)),
        "== Status ==",
        "Windows PC",
        &win_fingerprint.to_string(),
        "== Configuration ==",
        "== Transfer pacing ==\nFiles slow down while the round trip is more than 15.0 ms",
        "the last line",
        "missing.log ==\n(cannot read",
    ] {
        assert!(report.contains(expected), "no {expected:?} in\n{report}");
    }

    // ctl writes what the agent reports where the user asks.
    let output_file = dir.path().join("report.txt");
    let output = tokio::task::spawn_blocking({
        let output_file = output_file.clone();
        move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_inputweave"))
                .args(["ctl", "diagnose", "--socket"])
                .arg(&socket)
                .arg("--output")
                .arg(&output_file)
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    assert!(output.status.success(), "ctl failed: {output:?}");
    let written = std::fs::read_to_string(&output_file).unwrap();
    assert!(written.contains("Windows PC") && written.contains("the last line"));

    mac.agent.shutdown().await;
}

/// Presses and lets go of Control-Option-Shift-L.
fn lock_hotkey(node: &Node) {
    for k in [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT] {
        key(node, k, true);
    }
    key(node, usage::L, true);
    for k in [
        usage::L,
        usage::LEFT_CONTROL,
        usage::LEFT_ALT,
        usage::LEFT_SHIFT,
    ] {
        key(node, k, false);
    }
}

/// First-run setup has the agent ask macOS for the folders it protects,
/// here folders in a temporary home: it lists
/// each when asked, tells what came of it, and remembers what it was told.
/// It never lists them by itself, not even as it starts, and leaves the
/// receive folder in Downloads alone until files are written there.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn setup_asks_for_the_folders_macos_protects_and_nothing_else_does() {
    use inputweave::agent::{Access, FileSettings, TransferState, UserFolder};
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let home = dir.path().join("home");
    let downloads = home.join("Downloads");
    let desktop = home.join("Desktop");
    let documents = home.join("Documents");
    let received = downloads.join("InputWeave");
    std::fs::create_dir_all(&received).unwrap();
    std::fs::create_dir_all(&desktop).unwrap();
    // Part of a file whose transfer stopped weeks ago.
    let stale = received.join(".inputweave-0123456789abcdef.part");
    let leave_stale = || {
        std::fs::write(&stale, "half a file").unwrap();
        let weeks_ago = std::time::SystemTime::now() - Duration::from_secs(30 * 24 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(weeks_ago)
            .unwrap();
    };
    leave_stale();
    // As macOS refuses a folder the user did not allow; root reads it all
    // the same.
    std::fs::set_permissions(&desktop, std::fs::Permissions::from_mode(0o000)).unwrap();
    let desktop_access = if std::fs::read_dir(&desktop).is_err() {
        Access::Denied
    } else {
        Access::Granted
    };
    let spec = mac();
    let options = || Options {
        ipc_socket: Some(socket.clone()),
        user_folders: Some(vec![
            (UserFolder::Downloads, downloads.clone()),
            (UserFolder::Desktop, desktop.clone()),
            (UserFolder::Documents, documents.clone()),
        ]),
        ..kept_in(dir.path(), "mac", received.clone())
    };
    let mac = start_kept(&spec, dir.path(), "mac", options(), None).await;
    let status = mac.agent.status();
    assert_eq!(
        status.folder_access.folders,
        [
            (UserFolder::Downloads, Access::Unknown),
            (UserFolder::Desktop, Access::Unknown),
            (UserFolder::Documents, Access::Unknown),
        ]
    );
    assert!(!status.folder_access.asked);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        stale.exists(),
        "the receive folder was listed as the agent started"
    );
    let output = run_ctl(&socket, &["status"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("folders:        not asked yet\n"),
        "{stdout}"
    );

    // The reply has the new round; no status after it has an older one.
    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    let answered = client.request_folder_access().await.unwrap();
    assert_eq!(answered.folder_access_round, 1);
    assert_eq!(answered.folder_access.len(), 3);
    assert!(
        answered
            .folder_access
            .iter()
            .all(|f| f.state() != iw_proto::app::v1::FolderAccessState::Unknown),
        "{answered:?}"
    );
    let status = wait_status(&mac, |s| {
        s.folder_access.asked
            && s.folder_access
                .folders
                .iter()
                .all(|&(_, access)| access != Access::Asking)
    })
    .await;
    let answers = [
        (UserFolder::Downloads, Access::Granted),
        (UserFolder::Desktop, desktop_access),
        (UserFolder::Documents, Access::Missing),
    ];
    assert_eq!(status.folder_access.folders, answers);
    let saved = std::fs::read_to_string(dir.path().join("mac-devices.toml")).unwrap();
    assert!(
        saved.contains("[folder_access]\ndownloads = \"granted\"\n"),
        "{saved}"
    );
    // From the command line, which waits for the answers.
    let output = run_ctl(&socket, &["folders"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = format!(
        "Downloads granted, Desktop {}, Documents missing",
        desktop_access.name()
    );
    assert!(stdout.contains(&expected), "{stdout}");
    let report = mac.agent.diagnostics().await;
    assert!(
        report.contains(&format!("Protected folders: {expected}; asked yes")),
        "{report}"
    );
    drop(client);

    // Started again, the agent shows what macOS said last without listing
    // anything: Documents, made since, is still missing until asked again.
    mac.agent.shutdown().await;
    std::fs::create_dir_all(&documents).unwrap();
    let mac = start_kept(&spec, dir.path(), "mac", options(), None).await;
    let status = mac.agent.status();
    assert_eq!(status.folder_access.folders, answers);
    assert!(status.folder_access.asked);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(mac.agent.status().folder_access.folders, answers);
    assert!(
        stale.exists(),
        "the receive folder was listed as the agent started"
    );
    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    assert_eq!(
        client
            .request_folder_access()
            .await
            .unwrap()
            .folder_access_round,
        1
    );
    let status = wait_status(&mac, |s| {
        s.folder_access.folders[2] == (UserFolder::Documents, Access::Granted)
    })
    .await;
    assert_eq!(status.folder_access.folders[0].1, Access::Granted);
    std::fs::set_permissions(&desktop, std::fs::Permissions::from_mode(0o700)).unwrap();
    drop(client);

    // Asked about files, the user is asked first; the folder is tidied once
    // they are accepted, before the file is written.
    let win = start_receiving(&win(), dir.path(), "win", dir.path().join("win-in"), None).await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    mac.agent.set_file_settings(FileSettings {
        folder: Some(received.clone()),
        ask: true,
    });
    let files = some_files(dir.path(), 2, 1000);
    let asked = win
        .agent
        .send_files("mac", vec![files[0].0.clone()])
        .await
        .unwrap()
        .remove(0);
    wait_transfer(&mac, &asked, |t| t.state == TransferState::WaitingForAnswer).await;
    assert!(
        stale.exists(),
        "the receive folder was listed before the user answered"
    );
    assert!(mac.agent.answer_file_offer(&asked, true));
    assert_eq!(
        wait_transfer(&mac, &asked, finished).await.state,
        TransferState::Done
    );
    assert!(!stale.exists());
    assert_eq!(
        std::fs::read(received.join("file 0.bin")).unwrap(),
        files[0].1
    );

    // Once a run: accepted by itself, the next file finds what was left
    // since where it was.
    leave_stale();
    mac.agent.set_file_settings(FileSettings {
        folder: Some(received.clone()),
        ask: false,
    });
    let id = win
        .agent
        .send_files("mac", vec![files[1].0.clone()])
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        wait_transfer(&mac, &id, finished).await.state,
        TransferState::Done
    );
    assert!(stale.exists());
    // Started again, the first file accepted by itself tidies the folder
    // before it is written.
    mac.agent.shutdown().await;
    let mac = start_kept(&spec, dir.path(), "mac", options(), None).await;
    connected(&mac, "win").await;
    let id = win
        .agent
        .send_files("mac", vec![files[1].0.clone()])
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        wait_transfer(&mac, &id, finished).await.state,
        TransferState::Done
    );
    assert!(!stale.exists());

    // An agent with no such folders, as on Windows, refuses.
    let other_socket = dir.path().join("other.sock");
    let other = start_kept(
        &mini(),
        dir.path(),
        "mini",
        Options {
            ipc_socket: Some(other_socket.clone()),
            ..Options::default()
        },
        None,
    )
    .await;
    assert!(other.agent.status().folder_access.folders.is_empty());
    let mut client = inputweave::ctl::Client::connect(&other_socket)
        .await
        .unwrap();
    assert_eq!(refusal(client.request_folder_access().await), "unsupported");
    let output = run_ctl(&other_socket, &["status"]).await;
    assert!(!String::from_utf8_lossy(&output.stdout).contains("folders:"));
}

/// Options that serve IPC on `socket` for the Mac only.
#[cfg(unix)]
fn ipc_on_mac(socket: &std::path::Path) -> impl Fn(&Spec) -> Options + '_ {
    move |spec| Options {
        ipc_socket: (spec.id == "mac").then(|| socket.to_owned()),
        ..Options::default()
    }
}

/// Runs `inputweave ctl <args>` against the agent at `socket`, as a user
/// would.
#[cfg(unix)]
async fn run_ctl(socket: &std::path::Path, args: &[&str]) -> std::process::Output {
    let socket = socket.to_owned();
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_inputweave"))
            .arg("ctl")
            .args(&args)
            .arg("--socket")
            .arg(&socket)
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}

/// The error code the agent refused a request with.
#[cfg(unix)]
fn refusal<T: std::fmt::Debug>(result: Result<T, inputweave::ctl::CtlError>) -> String {
    match result {
        Err(inputweave::ctl::CtlError::Agent { code, .. }) => code,
        other => panic!("not a refusal: {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_app_asks_for_a_portal_permission_and_hears_the_answer() {
    use iw_platform::PermissionAsk;
    use iw_proto::app::v1 as app;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let nodes = desk_with(&[mac()], ipc_on_mac(&socket)).await;
    let mac = &nodes[0];
    let remote = |s: &inputweave::agent::Status| {
        s.permissions
            .iter()
            .find(|p| p.permission == Permission::RemoteControl)
            .cloned()
    };
    // As on Wayland: the agent may ask the desktop, and has not by itself.
    mac.status.set_askable(Permission::RemoteControl);
    wait_status(mac, |s| {
        remote(s).is_some_and(|p| p.ask == PermissionAsk::Askable)
    })
    .await;
    assert!(mac.status.requests().is_empty());
    let output = run_ctl(&socket, &["status"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(
            "permission:     remote control missing (not asked yet; `inputweave ctl permission \
             remote-control` asks)\n"
        ),
        "{stdout}"
    );

    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    assert_eq!(
        refusal(
            client
                .request_permission(app::Permission::Accessibility)
                .await
        ),
        "unsupported",
        "granted elsewhere"
    );
    assert_eq!(
        refusal(
            client
                .request_permission(app::Permission::Unspecified)
                .await
        ),
        "unknown_permission"
    );
    // Answered once it is being asked about, not once the user answered.
    let answered = client
        .request_permission(app::Permission::RemoteControl)
        .await
        .unwrap();
    let asked = answered
        .permissions
        .iter()
        .find(|p| p.permission() == app::Permission::RemoteControl)
        .unwrap();
    assert!(!asked.granted);
    assert_eq!(asked.ask(), app::PermissionAsk::Asking);
    assert_eq!(asked.asked, 1, "the request it answers");
    assert_eq!(mac.status.requests(), [Permission::RemoteControl]);
    mac.status.answer(Permission::RemoteControl, false);
    wait_status(mac, |s| {
        remote(s).is_some_and(|p| p.ask == PermissionAsk::Denied)
    })
    .await;

    // `ctl permission` asks, and waits for the answer.
    let ctl = tokio::spawn({
        let socket = socket.clone();
        async move { run_ctl(&socket, &["permission", "remote-control"]).await }
    });
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while mac.status.requests().len() < 2 {
        assert!(tokio::time::Instant::now() < deadline, "not asked");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    mac.status.answer(Permission::RemoteControl, true);
    let output = ctl.await.unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("remote control granted\n"), "{stdout}");
    let output = run_ctl(&socket, &["permission", "camera"]).await;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("remote-control, input-capture"),
        "{output:?}"
    );
    let report = client.diagnostics().await.unwrap();
    assert!(
        report.contains("\nPermissions: accessibility granted, remote control granted\n"),
        "{report}"
    );

    // Asking fails at once, twice: each request is answered, with why.
    mac.status.fail_requests(Permission::RemoteControl);
    mac.status.answer(Permission::RemoteControl, false);
    for round in [3, 4] {
        let answered = client
            .request_permission(app::Permission::RemoteControl)
            .await
            .unwrap();
        let failed = answered
            .permissions
            .iter()
            .find(|p| p.permission() == app::Permission::RemoteControl)
            .unwrap();
        assert_eq!(failed.ask(), app::PermissionAsk::Failed);
        assert_eq!(failed.asked, round);
        assert_eq!(failed.failure, "the mock fails it");
        assert_eq!(
            inputweave::ctl::format_permission(failed),
            "remote control missing (asking failed: the mock fails it)"
        );
    }
    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn locking_over_ipc_keeps_the_cursor_where_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let nodes = desk_with(&[mac(), win()], ipc_on_mac(&socket)).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // Locked from the command line, the cursor stays on the Mac.
    let output = run_ctl(&socket, &["lock", "on"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cursor on:      mac (locked)"), "{stdout}");
    motion(mac, (1511.0, 500.0), (5.0, 0.0));
    // The hotkey unlocks it. Capture delivers in order, so by then the push
    // above was handled, while locked.
    lock_hotkey(mac);
    wait_status(mac, |s| !s.locked).await;
    assert!(!mac.capture.is_grabbed());
    assert_eq!(mac.agent.status().cursor_on, "mac");
    assert!(win.emulation.calls().is_empty());

    // Unlocked, the same push crosses. Locked on the PC, the cursor stops
    // at the PC's edge instead of coming back.
    mac_enters_win(mac, win).await;
    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    assert!(client.set_locked(true).await.unwrap().locked);
    motion(mac, (0.0, 0.0), (-20.0, 0.0));
    wait_injected(win, Emulated::MoveTo(Point::new(0.0, 900.0))).await;
    assert_eq!(mac.agent.status().cursor_on, "win");
    assert!(mac.capture.is_grabbed());
    assert_eq!(refusal(client.jump_to("mac").await), "locked");

    // Taking control back still works, and leaves the lock on.
    take_back(mac);
    let status = wait_status(mac, |s| s.cursor_on == "mac").await;
    assert!(status.locked);
    assert_eq!(status.last_return, Some(ReturnReason::Hotkey));
    wait_status(win, |s| s.controlled_by.is_none()).await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn other_devices_can_be_renamed_on_this_one_only() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let nodes = desk_with(&[mac(), win()], ipc_on_mac(&socket)).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // From the command line, as a user would.
    let output = run_ctl(&socket, &["rename", "win", "書房的 PC"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("書房的 PC") && stdout.contains("calls itself \"Windows PC\""),
        "{stdout}"
    );
    let status = mac.agent.status();
    let pc = status.device("win").unwrap();
    assert_eq!(
        (pc.name.as_str(), pc.reported_name.as_str()),
        ("書房的 PC", "Windows PC")
    );
    // The arrangement uses it too; the PC itself knows nothing of it.
    wait_arrangement(mac, |a| {
        a.device("win").is_some_and(|d| d.name == "書房的 PC")
    })
    .await;
    assert_eq!(win.agent.status().this_name, "Windows PC");
    assert_eq!(
        win.agent.arrangement().device("win").unwrap().name,
        "Windows PC"
    );

    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    assert_eq!(
        refusal(client.rename_device("win", "bad\nname").await),
        "invalid_name"
    );
    // Not this device, nor one it does not trust.
    assert_eq!(
        refusal(client.rename_device("mac", "x").await),
        "unknown_device"
    );
    assert_eq!(
        refusal(client.rename_device("ghost", "x").await),
        "unknown_device"
    );

    // Cleared, the PC's own name shows again.
    let output = run_ctl(&socket, &["rename", "win", "--clear"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let pc = mac.agent.status().device("win").unwrap().clone();
    assert_eq!(
        (pc.name.as_str(), pc.reported_name.as_str()),
        ("Windows PC", "Windows PC")
    );
    wait_arrangement(mac, |a| {
        a.device("win").is_some_and(|d| d.name == "Windows PC")
    })
    .await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_cursor_moves_at_the_speed_chosen_for_a_paired_device() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let file = dir.path().join("devices.toml");
    let key = dir.path().join("identity");
    let spec = mac();
    let options = || Options {
        ipc_socket: Some(socket.clone()),
        devices_file: Some(file.clone()),
        ..Options::default()
    };
    let win = unpaired(&win(), Identity::generate().unwrap(), None).await;
    let mac = start_with(
        &spec,
        Identity::load_or_generate(&key).unwrap(),
        &[],
        &[],
        options(),
    )
    .await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    assert_eq!(mac.agent.status().device("win").unwrap().cursor_speed, 100);

    // From the command line, as a user would.
    let output = run_ctl(&socket, &["device", "win", "--speed", "200"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cursor speed 200%"), "{stdout}");
    assert_eq!(mac.agent.status().device("win").unwrap().cursor_speed, 200);
    // The PC itself is not told.
    assert_eq!(win.agent.status().device("mac").unwrap().cursor_speed, 100);

    // Unarranged, the PC is right of the Mac, tops level. Of the 5 that
    // cross, 1 is on the Mac, as the mouse moves; the other 4 go twice as
    // far on the PC, as everything after.
    motion(&mac, (1511.0, 500.0), (5.0, 0.0));
    wait_injected(&win, Emulated::MoveTo(Point::new(8.0, 500.0))).await;
    motion(&mac, (0.0, 0.0), (10.0, -3.0));
    wait_injected(&win, Emulated::MoveTo(Point::new(28.0, 494.0))).await;

    // Over IPC, as the apps do; only speeds the apps offer are taken.
    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    let status = client.set_device_speed("win", 150).await.unwrap();
    let pc = status.devices.iter().find(|d| d.id == "win").unwrap();
    assert_eq!(pc.speed_percent, 150);
    for wrong in [0, 40, 155, 310] {
        assert_eq!(
            refusal(client.set_device_speed("win", wrong).await),
            "invalid_speed",
            "{wrong}"
        );
    }
    // Not this device, nor one it does not trust.
    assert_eq!(
        refusal(client.set_device_speed("mac", 150).await),
        "unknown_device"
    );
    assert_eq!(
        refusal(client.set_device_speed("ghost", 150).await),
        "unknown_device"
    );
    let output = run_ctl(&socket, &["device", "win", "--speed", "55"]).await;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid_speed"),
        "{output:?}"
    );
    assert_eq!(mac.agent.status().device("win").unwrap().cursor_speed, 150);
    // 1.5 times as far from (28, 494).
    motion(&mac, (0.0, 0.0), (4.0, 2.0));
    wait_injected(&win, Emulated::MoveTo(Point::new(34.0, 497.0))).await;
    drop(client);
    mac.agent.shutdown().await;

    // Kept across a restart.
    let mac = start_with(
        &spec,
        Identity::load_or_generate(&key).unwrap(),
        &[],
        &[],
        options(),
    )
    .await;
    assert_eq!(mac.agent.status().device("win").unwrap().cursor_speed, 150);
    connected(&mac, "win").await;
    let output = run_ctl(&socket, &["status"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cursor speed 150%"), "{stdout}");

    // Pairing again with the same device keeps it.
    pair(&mac, &win).await;
    let status = connected(&mac, "win").await;
    assert_eq!(status.device("win").unwrap().cursor_speed, 150);

    // Gone with the pairing: paired again, the PC starts at the normal speed.
    mac.agent.unpair("win").unwrap();
    wait_status(&mac, |s| s.device("win").is_none()).await;
    let saved = inputweave::store::DevicesFile::load(&file).unwrap();
    assert!(saved.devices.is_empty() && saved.cursor_speed.is_empty());
    pair(&mac, &win).await;
    let status = connected(&mac, "win").await;
    assert_eq!(status.device("win").unwrap().cursor_speed, 100);
    let output = run_ctl(&socket, &["status"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("cursor speed"), "{stdout}");

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_in_the_configuration_file_takes_a_cursor_speed_too() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    assert_eq!(mac.agent.status().device("win").unwrap().cursor_speed, 100);
    mac_enters_win(mac, win).await;
    mac.agent.set_device_speed("win", 50).unwrap();
    assert!(matches!(
        mac.agent.set_device_speed("win", 45),
        Err(agent::DeviceError::InvalidSpeed)
    ));
    assert!(matches!(
        mac.agent.set_device_speed("ghost", 50),
        Err(agent::DeviceError::Unknown(_))
    ));
    wait_status(mac, |s| {
        s.device("win").is_some_and(|d| d.cursor_speed == 50)
    })
    .await;
    // Half as far on the PC, from (4, 900).
    motion(mac, (0.0, 0.0), (10.0, 6.0));
    wait_injected(win, Emulated::MoveTo(Point::new(9.0, 903.0))).await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn changes_to_a_device_from_two_apps_at_once_are_all_answered() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let nodes = desk_with(&[mac(), win()], ipc_on_mac(&socket)).await;
    let mac = &nodes[0];
    let mut one = inputweave::ctl::Client::connect(&socket).await.unwrap();
    let mut other = inputweave::ctl::Client::connect(&socket).await.unwrap();
    // Each change may be replaced by the other one before the status shows
    // it; neither waits for its own value, so neither times out.
    for round in 0..5 {
        let (a, b) = tokio::join!(
            one.set_device_sharing("win", round % 2 == 0, false),
            other.set_device_sharing("win", false, round % 2 == 0)
        );
        a.unwrap();
        b.unwrap();
        let (a, b) = tokio::join!(
            one.set_device_speed("win", 150),
            other.set_device_speed("win", 200)
        );
        a.unwrap();
        b.unwrap();
        let (a, b) = tokio::join!(
            one.rename_device("win", "Studio PC"),
            other.rename_device("win", "Office PC")
        );
        a.unwrap();
        b.unwrap();
    }
    let pc = mac.agent.status().device("win").unwrap().clone();
    assert!([150, 200].contains(&pc.cursor_speed));
    assert!(["Studio PC", "Office PC"].contains(&pc.name.as_str()));

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn identifying_the_screens_reaches_every_device_once_a_second() {
    use iw_proto::app::v1::{AgentMessage, agent_message};

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let nodes = desk_with(&[mac(), win()], ipc_on_mac(&socket)).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let mut app = App::connect(&socket).await;
    let mut on_mac = mac.agent.watch_screens_identified();
    let mut on_win = win.agent.watch_screens_identified();
    on_mac.mark_unchanged();
    on_win.mark_unchanged();

    // From the command line on the Mac: both show their names.
    let output = run_ctl(&socket, &["identify"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    for seen in [&mut on_mac, &mut on_win] {
        tokio::time::timeout(TIMEOUT, seen.changed())
            .await
            .expect("the screens are identified")
            .unwrap();
        assert_eq!(*seen.borrow_and_update(), Some(2500));
    }
    // The Mac's app hears of it, outside the event oneof.
    let identified = loop {
        let msg = tokio::time::timeout(TIMEOUT, app.conn.recv::<AgentMessage>())
            .await
            .expect("the app hears of it")
            .unwrap()
            .unwrap();
        if let Some(agent_message::Msg::Event(e)) = msg.msg
            && let Some(identified) = e.screens_identified
        {
            assert!(e.event.is_none());
            break identified;
        }
    };
    assert_eq!(identified.duration_ms, 2500);

    // Asked again at once: nothing more, here or there.
    mac.agent.identify_screens();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!on_mac.has_changed().unwrap());
    assert!(!on_win.has_changed().unwrap());

    // The PC asking counts on its own.
    win.agent.identify_screens();
    for seen in [&mut on_mac, &mut on_win] {
        tokio::time::timeout(TIMEOUT, seen.changed())
            .await
            .expect("the screens are identified")
            .unwrap();
    }

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn jumping_moves_the_cursor_between_devices() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let mut nodes = desk_with(&[mac(), win()], ipc_on_mac(&socket)).await;
    let win = nodes.pop().unwrap();
    let mac = nodes.pop().unwrap();

    // To the middle of the PC's main display.
    let output = run_ctl(&socket, &["jump", "win"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cursor on:      win"), "{stdout}");
    wait_injected(&win, Emulated::MoveTo(Point::new(1280.0, 720.0))).await;
    wait_status(&win, |s| s.controlled_by.as_deref() == Some("mac")).await;
    assert!(mac.capture.is_grabbed());
    key(&mac, usage::A, true);
    wait_injected(&win, Emulated::Key(usage::A, true)).await;
    // The PC's own app cannot send the cursor anywhere while the Mac
    // drives it.
    assert_eq!(
        win.agent.jump_to("mac").await,
        Err(agent::JumpError::ControlledBy("mac".to_owned()))
    );

    // Back to the middle of the Mac's: the PC lets go of A.
    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    assert_eq!(client.jump_to("mac").await.unwrap().active_device_id, "mac");
    assert!(!mac.capture.is_grabbed());
    assert_eq!(mac.capture.warps().last(), Some(&Point::new(756.0, 491.0)));
    wait_injected(&win, Emulated::Key(usage::A, false)).await;
    wait_status(&win, |s| s.controlled_by.is_none()).await;
    key(&mac, usage::A, false);

    // Refused, with a reason the app can show.
    assert_eq!(refusal(client.jump_to("ghost").await), "unknown_device");
    mac.agent.set_sharing(false);
    wait_status(&mac, |s| !s.sharing).await;
    assert_eq!(refusal(client.jump_to("win").await), "sharing_paused");
    mac.agent.set_sharing(true);
    win.agent.shutdown().await;
    wait_status(&mac, |s| {
        s.sharing && s.device("win").is_some_and(|d| !d.connected)
    })
    .await;
    assert_eq!(refusal(client.jump_to("win").await), "not_connected");
    let output = run_ctl(&socket, &["jump", "win"]).await;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not_connected"),
        "{output:?}"
    );

    mac.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connected_devices_report_their_round_trip_time() {
    let mut nodes = desk(&[mac(), win()]).await;
    let win = nodes.pop().unwrap();
    let mac = nodes.pop().unwrap();

    let status = wait_status(&mac, |s| s.device("win").is_some_and(|d| d.rtt_ms > 0)).await;
    let proto = status.to_proto();
    assert!(proto.devices.iter().any(|d| d.id == "win" && d.rtt_ms > 0));
    wait_status(&win, |s| s.device("mac").is_some_and(|d| d.rtt_ms > 0)).await;
    let report = mac.agent.diagnostics().await;
    assert!(
        report.contains("win (Windows PC): connected, round trip "),
        "{report}"
    );

    // Offline, the time is unknown again.
    win.agent.shutdown().await;
    let status = wait_status(&mac, |s| s.device("win").is_some_and(|d| !d.connected)).await;
    assert_eq!(status.device("win").unwrap().rtt_ms, 0);
    mac.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_cursor_stays_off_a_locked_screen_input_cannot_reach() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let mut nodes = desk_with(&[mac(), win()], ipc_on_mac(&socket)).await;
    let win = nodes.pop().unwrap();
    let mac = nodes.pop().unwrap();
    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();

    // The PC locks, and says input cannot reach it: the edge holds, and
    // it cannot be jumped to.
    win.status.set_screen_locked(true);
    let status = wait_status(&mac, |s| s.device("win").is_some_and(|d| d.screen_locked)).await;
    assert!(!status.device("win").unwrap().lock_screen_input);
    motion(&mac, (1511.0, 500.0), (5.0, 0.0));
    assert_eq!(refusal(client.jump_to("win").await), "screen_locked");
    let output = run_ctl(&socket, &["jump", "win"]).await;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("screen_locked"), "{stderr}");
    let output = run_ctl(&socket, &["status"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("screen locked, no input"), "{stdout}");
    assert!(
        stdout.contains("lock screen:    other devices cannot type on it"),
        "{stdout}"
    );
    assert!(!mac.capture.is_grabbed());
    assert!(
        win.emulation.calls().is_empty(),
        "{:?}",
        win.emulation.calls()
    );
    assert_eq!(mac.agent.status().cursor_on, "mac");

    // Unlocked, the cursor crosses as before.
    win.status.set_screen_locked(false);
    wait_status(&mac, |s| s.device("win").is_some_and(|d| !d.screen_locked)).await;
    mac_enters_win(&mac, &win).await;
    key(&mac, usage::A, true);
    wait_injected(&win, Emulated::Key(usage::A, true)).await;

    // Locking while controlled hands control back, and the PC lets go.
    win.status.set_screen_locked(true);
    let status = wait_status(&mac, |s| s.cursor_on == "mac").await;
    assert_eq!(
        status.last_return,
        Some(ReturnReason::ScreenLocked(DeviceId::new("win")))
    );
    assert_eq!(
        status.to_proto().last_return(),
        iw_proto::app::v1::ReturnReason::ScreenLocked
    );
    assert_eq!(status.to_proto().last_return_device_id, "win");
    assert!(!mac.capture.is_grabbed());
    wait_injected(&win, Emulated::Key(usage::A, false)).await;
    wait_status(&win, |s| s.controlled_by.is_none()).await;
    key(&mac, usage::A, false);

    // A locked screen that takes input, as a PC's does, is entered as usual.
    win.status.set_lock_screen_input(true);
    wait_status(&mac, |s| {
        s.device("win").is_some_and(|d| d.lock_screen_input)
    })
    .await;
    assert_eq!(client.jump_to("win").await.unwrap().active_device_id, "win");
    wait_status(&win, |s| s.controlled_by.as_deref() == Some("mac")).await;
    let output = run_ctl(&socket, &["status"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("screen locked"), "{stdout}");
    assert!(!stdout.contains("no input"), "{stdout}");

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_cursor_stays_off_a_device_input_cannot_reach() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let mut nodes = desk_with(&[mac(), win()], ipc_on_mac(&socket)).await;
    let win = nodes.pop().unwrap();
    let mac = nodes.pop().unwrap();
    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();

    // The PC cannot take input (as a Wayland desktop that has not allowed
    // remote control): the edge holds, and it cannot be jumped to.
    win.status.set_input_available(false);
    wait_status(&mac, |s| {
        s.device("win").is_some_and(|d| d.input_unavailable)
    })
    .await;
    motion(&mac, (1511.0, 500.0), (5.0, 0.0));
    assert_eq!(refusal(client.jump_to("win").await), "input_unavailable");
    let output = run_ctl(&socket, &["status"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("takes no input now"), "{stdout}");
    assert!(!mac.capture.is_grabbed());
    assert!(
        win.emulation.calls().is_empty(),
        "{:?}",
        win.emulation.calls()
    );
    assert_eq!(mac.agent.status().cursor_on, "mac");
    assert!(
        win.agent
            .diagnostics()
            .await
            .contains("Input from other devices: cannot reach this device now\n")
    );

    // Taking input again, it is entered as before.
    win.status.set_input_available(true);
    wait_status(&mac, |s| {
        s.device("win").is_some_and(|d| !d.input_unavailable)
    })
    .await;
    mac_enters_win(&mac, &win).await;
    key(&mac, usage::A, true);
    wait_injected(&win, Emulated::Key(usage::A, true)).await;

    // No longer, while controlled: control comes back, and the PC lets go.
    win.status.set_input_available(false);
    let status = wait_status(&mac, |s| s.cursor_on == "mac").await;
    assert_eq!(
        status.last_return,
        Some(ReturnReason::InputUnavailable(DeviceId::new("win")))
    );
    assert_eq!(
        status.to_proto().last_return(),
        iw_proto::app::v1::ReturnReason::InputUnavailable
    );
    assert_eq!(status.to_proto().last_return_device_id, "win");
    assert!(!mac.capture.is_grabbed());
    wait_status(&win, |s| s.controlled_by.is_none()).await;
    key(&mac, usage::A, false);

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_locked_screen_is_reported_to_other_devices() {
    let dir = tempfile::tempdir().unwrap();
    let mac_file = dir.path().join("devices.toml");
    let mac_key = dir.path().join("identity");
    let spec = mac();
    let win = unpaired(&win(), Identity::generate().unwrap(), None).await;
    let mac_options = || Options {
        devices_file: Some(mac_file.clone()),
        #[cfg(unix)]
        ipc_socket: Some(dir.path().join("agent.sock")),
        ..Options::default()
    };
    let mac = start_with(
        &spec,
        Identity::load_or_generate(&mac_key).unwrap(),
        &[],
        &[],
        mac_options(),
    )
    .await;
    pair(&mac, &win).await;
    let status = connected(&mac, "win").await;
    assert!(!status.device("win").unwrap().screen_locked);
    connected(&win, "mac").await;

    // The PC's screen locks: the Mac hears, and so do its apps.
    win.status.set_screen_locked(true);
    let own = wait_status(&win, |s| s.screen_locked).await;
    assert!(own.to_proto().screen_locked);
    wait_status(&mac, |s| s.device("win").is_some_and(|d| d.screen_locked)).await;
    #[cfg(unix)]
    {
        let (_, status) = inputweave::ctl::status(&dir.path().join("agent.sock"))
            .await
            .unwrap();
        assert!(!status.screen_locked);
        assert!(
            status
                .devices
                .iter()
                .any(|d| d.id == "win" && d.screen_locked)
        );
    }
    let listed = inputweave::ctl::format_status(&mac.agent.status().to_proto());
    assert!(listed.contains("screen locked"), "{listed}");
    assert!(!listed.contains("screen:         locked"), "{listed}");
    let report = mac.agent.diagnostics().await;
    assert!(report.contains("Screen: unlocked"), "{report}");
    assert!(report.contains("Lock screen input: no"), "{report}");
    assert!(report.contains("ms, screen locked"), "{report}");

    // The Mac locks too, and says so.
    mac.status.set_screen_locked(true);
    wait_status(&win, |s| s.device("mac").is_some_and(|d| d.screen_locked)).await;
    let own = wait_status(&mac, |s| s.screen_locked).await;
    let listed = inputweave::ctl::format_status(&own.to_proto());
    assert!(listed.contains("screen:         locked"), "{listed}");

    // A new session starts from what each side says, not from the last.
    mac.agent.shutdown().await;
    let gone = wait_status(&win, |s| s.device("mac").is_some_and(|d| !d.connected)).await;
    assert!(!gone.device("mac").unwrap().screen_locked);
    let mac = start_with(
        &spec,
        Identity::load_or_generate(&mac_key).unwrap(),
        &[],
        &[],
        Options {
            devices_file: Some(mac_file.clone()),
            ..Options::default()
        },
    )
    .await;
    connected(&mac, "win").await;
    // The PC never changed, so only what it says as the session starts
    // tells the new Mac.
    wait_status(&mac, |s| s.device("win").is_some_and(|d| d.screen_locked)).await;
    connected(&win, "mac").await;
    mac.status.set_screen_locked(true);
    wait_status(&win, |s| {
        s.device("mac")
            .is_some_and(|d| d.connected && d.screen_locked)
    })
    .await;

    // Unlocked again.
    win.status.set_screen_locked(false);
    wait_status(&mac, |s| {
        s.device("win")
            .is_some_and(|d| d.connected && !d.screen_locked)
    })
    .await;

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_edge_dwell_holds_the_cursor_back() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    mac.agent.set_edge_dwell(200).unwrap();
    wait_status(mac, |s| s.edge_dwell_ms == 200).await;

    // A single push only starts the clock. The hotkey after it shows when
    // the push was handled.
    motion(mac, (1511.0, 500.0), (5.0, 0.0));
    lock_hotkey(mac);
    wait_status(mac, |s| s.locked).await;
    assert!(!mac.capture.is_grabbed());
    lock_hotkey(mac);
    wait_status(mac, |s| !s.locked).await;

    // Pushing for longer than the dwell crosses.
    motion(mac, (1511.0, 500.0), (5.0, 0.0));
    tokio::time::sleep(Duration::from_millis(500)).await;
    motion(mac, (1511.0, 500.0), (5.0, 0.0));
    wait_injected(win, Emulated::MoveTo(Point::new(4.0, 900.0))).await;
    wait_status(mac, |s| s.cursor_on == "win").await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cursor_leaves_a_device_nowhere_while_another_controls_it() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let right = [Exit {
        display: "builtin".to_owned(),
        bounds: Rect::new(0.0, 0.0, 1512.0, 982.0),
        edge: Edge::Right,
        at: 1512.0,
        start: 20.0,
        end: 962.0,
    }];
    wait_exits(mac, &right).await;

    // The PC takes the Mac over: what it moves there must not reach the
    // Mac's exits as the Mac's own input.
    motion(win, (0.0, 500.0), (-5.0, 0.0));
    wait_status(mac, |s| s.controlled_by.as_deref() == Some("win")).await;
    wait_exits(mac, &[]).await;
    // It goes back.
    motion(win, (0.0, 500.0), (20.0, 0.0));
    wait_status(mac, |s| s.controlled_by.is_none()).await;
    wait_exits(mac, &right).await;

    // Over again, and the Mac's own mouse takes control back.
    motion(win, (0.0, 500.0), (-5.0, 0.0));
    wait_status(mac, |s| s.controlled_by.as_deref() == Some("win")).await;
    wait_exits(mac, &[]).await;
    motion(mac, (700.0, 500.0), (1.0, 0.0));
    wait_status(mac, |s| s.controlled_by.is_none()).await;
    wait_exits(mac, &right).await;
    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_paused_device_turns_away_a_controller_that_enters() {
    use iw_net::{PeerEndpoint, Session, hello};
    use iw_proto::peer::v1::{Control, Enter, Input, Key, Os, control, input};

    // A device the PC trusts, speaking the protocol by hand as a controller
    // that does not know the PC can pause. The PC dials it.
    let rogue = PeerEndpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    )
    .unwrap();
    let known = [Known {
        id: "zed",
        name: "Rogue",
        fingerprint: rogue.fingerprint(),
        address: Some(rogue.local_addr().unwrap()),
    }];
    let spec = win();
    let win = start(&spec, Identity::generate().unwrap(), &[&spec], &known).await;
    let win_fp = win.agent.fingerprint();
    let accepted = tokio::time::timeout(TIMEOUT, async {
        rogue
            .accept()
            .await
            .unwrap()
            .finish(|fp| *fp == win_fp)
            .await
            .unwrap()
    })
    .await
    .unwrap();
    let mut session = Session::accept(
        accepted.connection,
        accepted.peer,
        hello("zed", "Rogue", Os::Linux),
    )
    .await
    .unwrap();
    let mut input_tx = session.info.open_input().await.unwrap();
    let send = |msg| Input { msg: Some(msg) };
    let enter = || {
        send(input::Msg::Enter(Enter {
            x: 100.0,
            y: 100.0,
            ..Enter::default()
        }))
    };
    let a = |pressed| {
        send(input::Msg::Key(Key {
            usage: u32::from(usage::A.id()),
            pressed,
        }))
    };

    // Paused: an entry is turned away, its input dropped, and the
    // controller told why, and that the user took over.
    win.agent.set_sharing(false);
    wait_status(&win, |s| !s.sharing).await;
    input_tx.send(&enter()).await.unwrap();
    input_tx.send(&a(true)).await.unwrap();
    input_tx.send(&a(false)).await.unwrap();
    let (mut paused, mut took_over) = (false, 0);
    tokio::time::timeout(TIMEOUT, async {
        while !(paused && took_over > 0) {
            let msg: Control = session.control_rx.recv().await.unwrap().unwrap();
            match msg.msg {
                Some(control::Msg::SessionState(state)) => paused |= state.sharing_paused,
                Some(control::Msg::LocalActivity(_)) => took_over += 1,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(win.agent.status().controlled_by, None);
    // Entering again, it is turned away again.
    input_tx.send(&enter()).await.unwrap();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let msg: Control = session.control_rx.recv().await.unwrap().unwrap();
            if let Some(control::Msg::LocalActivity(_)) = msg.msg {
                return;
            }
        }
    })
    .await
    .unwrap();
    assert!(
        !win.emulation
            .calls()
            .iter()
            .any(|c| matches!(c, Emulated::Key(..))),
        "{:?}",
        win.emulation.calls()
    );
    assert_eq!(win.agent.status().controlled_by, None);

    // Resumed: it controls the PC.
    win.agent.set_sharing(true);
    wait_status(&win, |s| s.sharing).await;
    input_tx.send(&enter()).await.unwrap();
    input_tx.send(&a(true)).await.unwrap();
    wait_injected(&win, Emulated::Key(usage::A, true)).await;
    wait_status(&win, |s| s.controlled_by.as_deref() == Some("zed")).await;
    input_tx.send(&a(false)).await.unwrap();
    wait_injected(&win, Emulated::Key(usage::A, false)).await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_modifier_held_across_is_let_go_of_here_unless_grabbing_took_it() {
    for grab_releases_input in [false, true] {
        let nodes = desk(&[mac(), win()]).await;
        let (mac, win) = (&nodes[0], &nodes[1]);
        mac.capture.set_grab_releases_input(grab_releases_input);
        // Shift goes down here, then the cursor crosses with it held: the
        // PC gets it.
        key(mac, usage::LEFT_SHIFT, true);
        mac_enters_win(mac, win).await;
        wait_injected(win, Emulated::Key(usage::LEFT_SHIFT, true)).await;
        // Let go of over there: the PC lets go of it; here, it is let go of
        // too, unless grabbing took it from the local apps already, as on
        // Wayland, where the device injecting here never pressed it.
        key(mac, usage::LEFT_SHIFT, false);
        wait_injected(win, Emulated::Key(usage::LEFT_SHIFT, false)).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let released_here = mac
            .emulation
            .calls()
            .contains(&Emulated::Key(usage::LEFT_SHIFT, false));
        assert_eq!(released_here, !grab_releases_input);
        for node in nodes {
            node.agent.shutdown().await;
        }
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_button_dragged_across_is_let_go_of_here_unless_grabbing_took_it() {
    for grab_releases_input in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"a").unwrap();
        let nodes = desk(&[mac(), win()]).await;
        let (mac, win) = (&nodes[0], &nodes[1]);
        mac.capture.set_grab_releases_input(grab_releases_input);
        // A file dragged across: the PC presses the button where the cursor
        // comes in; here, it is let go of where the cursor left, unless
        // grabbing took it from the local apps already, as on Wayland.
        start_dragging(mac, vec![file]).await;
        mac_enters_win(mac, win).await;
        wait_injected(win, Emulated::Button(Button::Left, true)).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let released_here = mac
            .emulation
            .calls()
            .contains(&Emulated::Button(Button::Left, false));
        assert_eq!(released_here, !grab_releases_input);
        for node in nodes {
            node.agent.shutdown().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn what_capture_reports_of_itself_does_not_take_control_back() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    motion(win, (0.0, 500.0), (-5.0, 0.0));
    wait_status(mac, |s| s.controlled_by.as_deref() == Some("win")).await;

    // Capture letting go, and keys and buttons it reports up as it does:
    // not the local user.
    mac.capture.emit(InputEvent::CaptureEnded).unwrap();
    key(mac, usage::A, false);
    mac.capture
        .emit(InputEvent::Button {
            button: Button::Left,
            pressed: false,
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        mac.agent.status().controlled_by.as_deref(),
        Some("win"),
        "still controlled"
    );
    // A press is.
    key(mac, usage::A, true);
    wait_status(mac, |s| s.controlled_by.is_none()).await;
    wait_status(win, |s| s.cursor_on == "win").await;
    key(mac, usage::A, false);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pausing_sharing_on_a_controlled_device_ends_the_control() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    motion(win, (0.0, 500.0), (-5.0, 0.0));
    wait_status(mac, |s| s.controlled_by.as_deref() == Some("win")).await;
    key(win, usage::A, true);
    wait_injected(mac, Emulated::Key(usage::A, true)).await;

    // Paused on the Mac, as from its tray where touching its own mouse is
    // not seen: control ends, what was held goes up, and the PC keeps off.
    mac.agent.set_sharing(false);
    wait_injected(mac, Emulated::Key(usage::A, false)).await;
    wait_status(mac, |s| s.controlled_by.is_none()).await;
    wait_status(win, |s| {
        s.cursor_on == "win"
            && s.last_return == Some(ReturnReason::PeerPaused(DeviceId::new("mac")))
            && s.device("mac")
                .is_some_and(|d| d.sharing_paused && !d.input_unavailable)
    })
    .await;
    assert_eq!(
        win.agent.jump_to("mac").await,
        Err(agent::JumpError::Refused(
            iw_core::router::JumpError::PeerPaused(DeviceId::new("mac"))
        ))
    );
    wait_exits(mac, &[]).await;
    motion(win, (0.0, 500.0), (-5.0, 0.0));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(win.agent.status().cursor_on, "win");
    assert_eq!(mac.agent.status().controlled_by, None);

    // Resumed: the PC goes over again.
    mac.agent.set_sharing(true);
    wait_status(win, |s| s.device("mac").is_some_and(|d| !d.sharing_paused)).await;
    motion(win, (0.0, 500.0), (-5.0, 0.0));
    wait_status(mac, |s| s.controlled_by.as_deref() == Some("win")).await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_device_whose_cursor_leaves_only_across_edges_does_not_jump() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let nodes = desk_with(&[mac(), win()], ipc_on_mac(&socket)).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    // As on Wayland, where the desktop hands input over only at the edges:
    // refused with a reason of its own, not as a missing permission.
    mac.capture.set_edge_only(true);
    assert_eq!(
        mac.agent.jump_to("win").await,
        Err(agent::JumpError::EdgeOnly)
    );
    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    assert_eq!(refusal(client.jump_to("win").await), "edge_only");
    let output = run_ctl(&socket, &["jump", "win"]).await;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("edge_only") && stderr.contains("can only leave across an edge"),
        "{output:?}"
    );
    // What is the matter with the jump itself is said first.
    assert_eq!(refusal(client.jump_to("ghost").await), "unknown_device");
    mac.agent.set_locked(true);
    wait_status(mac, |s| s.locked).await;
    assert_eq!(refusal(client.jump_to("win").await), "locked");
    mac.agent.set_locked(false);
    mac.agent.set_sharing(false);
    wait_status(mac, |s| !s.sharing).await;
    assert_eq!(refusal(client.jump_to("win").await), "sharing_paused");
    mac.agent.set_sharing(true);
    win.agent.set_sharing(false);
    wait_status(mac, |s| s.device("win").is_some_and(|d| d.sharing_paused)).await;
    assert_eq!(refusal(client.jump_to("win").await), "peer_paused");
    win.agent.set_sharing(true);
    wait_status(mac, |s| s.device("win").is_some_and(|d| !d.sharing_paused)).await;
    assert_eq!(refusal(client.jump_to("win").await), "edge_only");
    // Back at will, the jump goes.
    mac.capture.set_edge_only(false);
    mac.agent.jump_to("win").await.unwrap();
    wait_status(win, |s| s.controlled_by.as_deref() == Some("mac")).await;
    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn capture_that_stops_by_itself_is_followed_and_started_again() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    wait_status(mac, |s| s.capturing).await;
    mac.agent.jump_to("win").await.unwrap();
    wait_status(win, |s| s.controlled_by.as_deref() == Some("mac")).await;

    // The user stops the desktop's sharing of input, as on Wayland, while
    // the Mac controls the PC: the session ends, and its permission with
    // it. Control comes back; no input reaches the agent, and jumps are
    // refused as without the permission.
    mac.capture.set_stopped(true);
    mac.status.set_permission(Permission::InputCapture, false);
    wait_status(mac, |s| !s.capturing && s.cursor_on == "mac").await;
    wait_status(win, |s| s.controlled_by.is_none()).await;
    assert!(
        mac.capture
            .emit(InputEvent::Key {
                usage: usage::A,
                pressed: true
            })
            .is_err()
    );
    assert_eq!(
        mac.agent.jump_to("win").await,
        Err(agent::JumpError::NotCapturing)
    );
    // Allowed again: capture starts again, and the jump goes.
    mac.capture.set_stopped(false);
    mac.status.set_permission(Permission::InputCapture, true);
    wait_status(mac, |s| s.capturing).await;
    mac.agent.jump_to("win").await.unwrap();
    wait_status(win, |s| s.controlled_by.as_deref() == Some("mac")).await;
    mac.agent.jump_to("mac").await.unwrap();
    wait_status(win, |s| s.controlled_by.is_none()).await;

    // The session goes and comes back by itself, as one the desktop
    // restarts with its token, too soon for the permission to show it:
    // followed all the same. Capture hears no answer about the end the
    // engine routes then, which capture never reported.
    let answers = mac.capture.edge_waits().len();
    mac.capture.set_stopped(true);
    wait_status(mac, |s| !s.capturing).await;
    assert_eq!(mac.capture.edge_waits().len(), answers);
    mac.capture.set_stopped(false);
    wait_status(mac, |s| s.capturing).await;
    mac.agent.jump_to("win").await.unwrap();
    wait_status(win, |s| s.controlled_by.as_deref() == Some("mac")).await;
    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_jump_here_moves_the_cursor_by_injection_where_capture_cannot() {
    let nodes = desk(&[mac(), win()]).await;
    let mac = &nodes[0];
    wait_status(mac, |s| s.capturing).await;
    // As on Wayland with nothing handed over: capture cannot move the
    // cursor, and injection does, to the middle of the main display.
    mac.capture.set_no_warp(true);
    let before = mac.capture.warps().len();
    mac.agent.jump_to("mac").await.unwrap();
    wait_injected(mac, Emulated::MoveTo(Point::new(756.0, 491.0))).await;
    assert_eq!(mac.capture.warps().len(), before, "not through capture");
    // Where capture can, it does, and nothing is injected.
    mac.capture.set_no_warp(false);
    let injected = mac.emulation.calls().len();
    mac.agent.jump_to("mac").await.unwrap();
    assert_eq!(mac.capture.warps().len(), before + 1);
    assert_eq!(mac.capture.warps().last(), Some(&Point::new(756.0, 491.0)));
    assert_eq!(mac.emulation.calls().len(), injected);
    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// Waits until capture has been told, after the `handled`th input event it
/// reported, that the cursor waits at an edge to cross, or not.
async fn wait_edge_wait(node: &Node, handled: u64, waiting: bool) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let told = node.capture.edge_waits();
        if told.last() == Some(&(handled, waiting)) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {waiting} after {handled}; told {told:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn capture_hears_whether_the_cursor_waits_at_an_edge_to_cross() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    mac.agent.set_edge_dwell(200).unwrap();
    wait_status(mac, |s| s.edge_dwell_ms == 200).await;

    // Away from the edges: no.
    motion(mac, (700.0, 500.0), (5.0, 0.0));
    wait_edge_wait(mac, 1, false).await;
    // Pushing against the PC's edge: it waits to cross.
    motion(mac, (1511.0, 500.0), (5.0, 0.0));
    wait_edge_wait(mac, 2, true).await;
    // Moving back from the edge: no more.
    motion(mac, (1511.0, 500.0), (-5.0, 0.0));
    wait_edge_wait(mac, 3, false).await;

    // A key is answered too: the push goes on.
    motion(mac, (1511.0, 500.0), (5.0, 0.0));
    wait_edge_wait(mac, 4, true).await;
    key(mac, usage::A, true);
    key(mac, usage::A, false);
    wait_edge_wait(mac, 6, true).await;

    // Capture let go of input at the edge: the wait starts over.
    mac.capture.emit(InputEvent::CaptureEnded).unwrap();
    wait_edge_wait(mac, 7, false).await;

    // Pushing for longer than the dwell crosses: grabbed, nothing is said.
    motion(mac, (1511.0, 500.0), (5.0, 0.0));
    wait_edge_wait(mac, 8, true).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    motion(mac, (1511.0, 500.0), (5.0, 0.0));
    wait_status(mac, |s| s.cursor_on == "win").await;
    assert!(mac.capture.is_grabbed());
    wait_injected(win, Emulated::MoveTo(Point::new(4.0, 900.0))).await;
    motion(mac, (1511.0, 500.0), (5.0, 0.0));
    wait_injected(win, Emulated::MoveTo(Point::new(9.0, 900.0))).await;
    assert_eq!(mac.capture.edge_waits().len(), 8);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_edge_dwell_is_set_over_ipc_and_remembered() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let spec = mac();
    let options = || Options {
        ipc_socket: Some(socket.clone()),
        devices_file: Some(dir.path().join("devices.toml")),
        ..Options::default()
    };
    let mac = start_with(
        &spec,
        Identity::generate().unwrap(),
        &[&spec],
        &[],
        options(),
    )
    .await;
    let output = run_ctl(&socket, &["dwell", "150"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("edge dwell:     150 ms"), "{stdout}");
    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    assert_eq!(refusal(client.set_edge_dwell(1001).await), "invalid_dwell");
    // Clients turn the wait on with the agent's default, whatever is set.
    for ms in [300, 0, 150] {
        let status = client.set_edge_dwell(ms).await.unwrap();
        assert_eq!(status.edge_dwell_ms, ms);
        assert_eq!(status.default_dwell_ms, inputweave::agent::DEFAULT_DWELL_MS);
    }
    assert_eq!(inputweave::agent::DEFAULT_DWELL_MS, 150);
    drop(client);
    mac.agent.shutdown().await;

    let mac = start_with(
        &spec,
        Identity::generate().unwrap(),
        &[&spec],
        &[],
        options(),
    )
    .await;
    assert_eq!(mac.agent.status().edge_dwell_ms, 150);
    mac.agent.shutdown().await;
}

/// Holds `modifiers`, presses and lets go of `usage`, then lets go of the
/// modifiers.
fn chord(node: &Node, modifiers: &[iw_core::HidUsage], usage: iw_core::HidUsage) {
    for k in modifiers {
        key(node, *k, true);
    }
    key(node, usage, true);
    key(node, usage, false);
    for k in modifiers {
        key(node, *k, false);
    }
}

const CONTROL_ALT_SHIFT: [iw_core::HidUsage; 3] =
    [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT];

/// The digit key `n` above the letters.
fn digit(n: u16) -> iw_core::HidUsage {
    iw_core::HidUsage::new(usage::DIGIT1.id() + n - 1)
}

/// Presses `usage` on `controller` and waits for `target` to inject it:
/// everything sent before has arrived by then.
async fn key_round_trip(controller: &Node, target: &Node, usage: iw_core::HidUsage) {
    key(controller, usage, true);
    key(controller, usage, false);
    wait_injected(target, Emulated::Key(usage, false)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_hotkeys_apply_at_once_and_the_old_keys_go_through() {
    use iw_core::hotkey::Hotkeys;

    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    // The lock and the nine jumps are reserved; taking back is not.
    let defaults = Hotkeys::default();
    assert_eq!(mac.capture.reserved(), defaults.reserved());
    assert_eq!(mac.capture.reserved().len(), 10);

    let k = iw_core::HidUsage::from_name("KeyK").unwrap();
    let hotkeys = Hotkeys::parse("control+alt+KeyT", "control+alt+KeyK", "", None, "").unwrap();
    mac.agent.set_hotkeys(hotkeys).await.unwrap();
    wait_status(mac, |s| s.hotkeys == hotkeys).await;
    assert_eq!(mac.capture.reserved(), vec![hotkeys.lock]);

    mac_enters_win(mac, win).await;
    // The old lock and a jump are keys for the PC now.
    chord(mac, &CONTROL_ALT_SHIFT, usage::L);
    wait_injected(win, Emulated::Key(usage::L, true)).await;
    chord(mac, &CONTROL_ALT_SHIFT, digit(2));
    wait_injected(win, Emulated::Key(digit(2), true)).await;
    assert!(!mac.agent.status().locked);
    // The new one locks, and K does not reach the PC.
    chord(mac, &[usage::LEFT_CONTROL, usage::LEFT_ALT], k);
    wait_status(mac, |s| s.locked).await;
    key_round_trip(mac, win, usage::A).await;
    assert!(!win.emulation.calls().contains(&Emulated::Key(k, true)));
    chord(mac, &[usage::LEFT_CONTROL, usage::LEFT_ALT], k);
    wait_status(mac, |s| !s.locked).await;

    // The old take-back hotkey goes to the PC; the new one comes back.
    take_back(mac);
    wait_injected(win, Emulated::Key(usage::ESCAPE, true)).await;
    assert_eq!(mac.agent.status().cursor_on, "win");
    chord(
        mac,
        &[usage::LEFT_CONTROL, usage::LEFT_ALT],
        iw_core::HidUsage::from_name("KeyT").unwrap(),
    );
    let status = wait_status(mac, |s| s.cursor_on == "mac").await;
    assert_eq!(status.last_return, Some(ReturnReason::Hotkey));

    // Hotkeys that cannot be used together are refused.
    let same = Hotkeys {
        lock: hotkeys.take_back,
        ..hotkeys
    };
    assert_eq!(
        mac.agent.set_hotkeys(same).await.unwrap_err().code(),
        "duplicate"
    );
    assert_eq!(mac.agent.status().hotkeys, hotkeys);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn capture_losing_the_hotkeys_or_getting_them_back_is_followed() {
    let mut nodes = desk(&[mac(), win()]).await;
    let win = nodes.pop().unwrap();
    let mac = nodes.pop().unwrap();
    let digit_released = |node: &Node| {
        node.emulation
            .calls()
            .iter()
            .filter(|c| **c == Emulated::Key(digit(2), false))
            .count()
    };
    let jump_there_and_back = async || {
        chord(&mac, &CONTROL_ALT_SHIFT, digit(2));
        wait_status(&mac, |s| s.cursor_on == "win").await;
        chord(&mac, &CONTROL_ALT_SHIFT, digit(1));
        wait_status(&mac, |s| s.cursor_on == "mac").await;
    };
    // The mock holds nothing back: the digit went down for the Mac's apps
    // and is let go of there as the cursor leaves.
    jump_there_and_back().await;
    let before = digit_released(&mac);
    assert!(before > 0);

    // Capture says it holds them back now, as X11's does once it grabbed
    // them again, in order with the keys: the digit pressed right after it
    // is no longer let go of here.
    let held = |but: &[iw_core::HidUsage]| {
        mac.capture
            .reserved()
            .into_iter()
            .filter(|h| !but.contains(&h.key))
            .collect()
    };
    mac.capture.set_hotkeys_held_back(held(&[]));
    jump_there_and_back().await;
    assert_eq!(digit_released(&mac), before);

    // Nor while it holds back all but another jump.
    mac.capture.set_hotkeys_held_back(held(&[digit(3)]));
    jump_there_and_back().await;
    assert_eq!(digit_released(&mac), before);

    // And when it loses the jump to 2 too.
    mac.capture
        .set_hotkeys_held_back(held(&[digit(2), digit(3)]));
    jump_there_and_back().await;
    assert!(digit_released(&mac) > before);

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

/// Waits until capture was last told the cursor leaves `node` through
/// `exits`.
async fn wait_exits(node: &Node, exits: &[Exit]) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let told = node.capture.exits();
        if told.last().map_or(exits.is_empty(), |last| last == exits) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for exits {exits:?}; told {told:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn capture_is_told_where_the_cursor_leaves_this_device() {
    let win = unpaired(&win(), Identity::generate().unwrap(), None).await;
    let mac = unpaired(&mac(), Identity::generate().unwrap(), None).await;
    // Alone, the cursor leaves the Mac nowhere.
    wait_exits(&mac, &[]).await;

    // Paired, the PC goes to its right with tops level: the Mac's right
    // edge, short of the corners, in the Mac's own coordinates.
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    let right = [Exit {
        display: "builtin".to_owned(),
        bounds: Rect::new(0.0, 0.0, 1512.0, 982.0),
        edge: Edge::Right,
        at: 1512.0,
        start: 20.0,
        end: 962.0,
    }];
    wait_exits(&mac, &right).await;
    // And the PC's left edge, all of it: below the Mac the cursor goes to
    // its bottom.
    wait_exits(
        &win,
        &[Exit {
            display: "main".to_owned(),
            bounds: Rect::new(0.0, 0.0, 2560.0, 1440.0),
            edge: Edge::Left,
            at: 0.0,
            start: 20.0,
            end: 1420.0,
        }],
    )
    .await;

    // Locked, by the app or by the hotkey, or paused: nowhere.
    mac.agent.set_locked(true);
    wait_exits(&mac, &[]).await;
    mac.agent.set_locked(false);
    wait_exits(&mac, &right).await;
    chord(&mac, &CONTROL_ALT_SHIFT, usage::L);
    wait_status(&mac, |s| s.locked).await;
    wait_exits(&mac, &[]).await;
    chord(&mac, &CONTROL_ALT_SHIFT, usage::L);
    wait_exits(&mac, &right).await;
    mac.agent.set_sharing(false);
    wait_exits(&mac, &[]).await;
    mac.agent.set_sharing(true);
    wait_exits(&mac, &right).await;
    let told = mac.capture.exits().len();

    // Moving the cursor tells capture nothing new.
    motion(&mac, (700.0, 500.0), (5.0, 0.0));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(mac.capture.exits().len(), told);

    // The PC gone, the cursor leaves the Mac nowhere again.
    win.agent.shutdown().await;
    wait_status(&mac, |s| s.device("win").is_some_and(|d| !d.connected)).await;
    wait_exits(&mac, &[]).await;
    mac.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn jump_hotkeys_go_to_the_device_with_that_number() {
    let mut nodes = desk(&[mac(), win(), mini()]).await;
    let mini = nodes.pop().unwrap();
    let win = nodes.pop().unwrap();
    let mac = nodes.pop().unwrap();

    // Left to right, then top to bottom: the Mac, then the Mac mini above
    // the PC, then the PC. Every device numbers them the same.
    let slots = |a: &inputweave::agent::Arrangement| -> Vec<(String, u32)> {
        a.devices
            .iter()
            .map(|d| (d.id.clone(), d.jump_slot))
            .collect()
    };
    let expected = |order: [&str; 3]| -> Vec<(String, u32)> {
        let slot = |id: &str| {
            [("mac", 1), ("mini", 2), ("win", 3)]
                .iter()
                .find(|(d, _)| *d == id)
                .unwrap()
                .1
        };
        order
            .iter()
            .map(|id| ((*id).to_owned(), slot(id)))
            .collect()
    };
    assert_eq!(
        slots(&mac.agent.arrangement()),
        expected(["mac", "win", "mini"])
    );
    assert_eq!(
        slots(&win.agent.arrangement()),
        expected(["win", "mac", "mini"])
    );
    let status = mac.agent.status();
    assert_eq!(status.device("mini").unwrap().jump_slot, 2);
    assert_eq!(status.device("win").unwrap().jump_slot, 3);
    let proto = status.to_proto();
    assert!(
        proto
            .devices
            .iter()
            .any(|d| d.id == "win" && d.jump_slot == 3)
    );

    // To the middle of each main screen, and back.
    chord(&mac, &CONTROL_ALT_SHIFT, digit(3));
    wait_injected(&win, Emulated::MoveTo(Point::new(1280.0, 720.0))).await;
    wait_status(&mac, |s| s.cursor_on == "win").await;
    chord(&mac, &CONTROL_ALT_SHIFT, digit(2));
    wait_injected(&mini, Emulated::MoveTo(Point::new(960.0, 540.0))).await;
    wait_status(&mac, |s| s.cursor_on == "mini").await;
    // The digits never reach the devices.
    key_round_trip(&mac, &mini, usage::A).await;
    for node in [&win, &mini] {
        let calls = node.emulation.calls();
        assert!(
            !calls.contains(&Emulated::Key(digit(2), true))
                && !calls.contains(&Emulated::Key(digit(3), true)),
            "{calls:?}"
        );
    }
    chord(&mac, &CONTROL_ALT_SHIFT, digit(1));
    wait_status(&mac, |s| s.cursor_on == "mac").await;
    assert_eq!(mac.capture.warps().last(), Some(&Point::new(756.0, 491.0)));

    // A number no device has, or an offline device's, does nothing.
    mini.agent.shutdown().await;
    wait_status(&mac, |s| s.device("mini").is_some_and(|d| !d.connected)).await;
    assert_eq!(mac.agent.status().device("mini").unwrap().jump_slot, 2);
    chord(&mac, &CONTROL_ALT_SHIFT, digit(9));
    chord(&mac, &CONTROL_ALT_SHIFT, digit(2));
    lock_hotkey(&mac);
    wait_status(&mac, |s| s.locked).await;
    assert_eq!(mac.agent.status().cursor_on, "mac");
    assert!(!mac.capture.is_grabbed());
    lock_hotkey(&mac);
    wait_status(&mac, |s| !s.locked).await;

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_switch_modifier_must_be_held_to_cross_but_not_to_jump() {
    use iw_core::hotkey::Modifier;

    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    mac.agent.set_switch_modifier(Some(Modifier::Shift));
    wait_status(mac, |s| s.switch_modifier == Some(Modifier::Shift)).await;

    // Pushing without Shift stays here. The lock hotkey after it shows
    // when the push was handled.
    motion(mac, (1511.0, 500.0), (5.0, 0.0));
    lock_hotkey(mac);
    wait_status(mac, |s| s.locked).await;
    assert!(!mac.capture.is_grabbed());
    lock_hotkey(mac);
    wait_status(mac, |s| !s.locked).await;

    // With either Shift held it crosses, and the PC never sees Shift.
    key(mac, usage::RIGHT_SHIFT, true);
    mac_enters_win(mac, win).await;
    key(mac, usage::RIGHT_SHIFT, false);
    key_round_trip(mac, win, usage::A).await;
    let shift = |calls: Vec<Emulated>| {
        calls
            .into_iter()
            .any(|c| matches!(c, Emulated::Key(u, _) if u == usage::RIGHT_SHIFT))
    };
    assert!(!shift(win.emulation.calls()));

    // Back the same way: without Shift the cursor stays on the PC.
    motion(mac, (0.0, 0.0), (-20.0, 0.0));
    wait_injected(win, Emulated::MoveTo(Point::new(0.0, 900.0))).await;
    assert_eq!(mac.agent.status().cursor_on, "win");
    key(mac, usage::LEFT_SHIFT, true);
    motion(mac, (0.0, 0.0), (-20.0, 0.0));
    wait_status(mac, |s| s.cursor_on == "mac").await;
    key(mac, usage::LEFT_SHIFT, false);

    // Jumping needs no modifier.
    mac.agent.jump_to("win").await.unwrap();
    wait_injected(win, Emulated::MoveTo(Point::new(1280.0, 720.0))).await;
    chord(mac, &CONTROL_ALT_SHIFT, digit(1));
    wait_status(mac, |s| s.cursor_on == "mac").await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn hotkeys_and_the_switch_modifier_are_set_over_ipc_and_remembered() {
    use iw_proto::app::v1::{Hotkeys, SwitchModifier};

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let spec = mac();
    let options = || Options {
        ipc_socket: Some(socket.clone()),
        devices_file: Some(dir.path().join("devices.toml")),
        ..Options::default()
    };
    let mac = start_with(
        &spec,
        Identity::generate().unwrap(),
        &[&spec],
        &[],
        options(),
    )
    .await;

    let output = run_ctl(&socket, &["hotkeys"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("take back control:  control+alt+shift+Escape\n")
            && stdout.contains("control+alt+shift+Digit1  MacBook Pro (mac) (this device)\n"),
        "{stdout}"
    );
    // Only those given change; written back the usual way.
    let output = run_ctl(
        &socket,
        &[
            "hotkeys",
            "set",
            "--lock",
            "Shift+Control+keyk",
            "--jump-modifiers",
            "alt+gui",
        ],
    )
    .await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("lock:               control+shift+KeyK\n")
            && stdout.contains("jump to a device:   alt+gui+Digit1 to Digit9\n"),
        "{stdout}"
    );
    // An empty value turns the jumps off, and back on.
    let output = run_ctl(&socket, &["hotkeys", "set", "--jump-modifiers", ""]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("jump to a device:   off\n"),
        "{output:?}"
    );
    assert_eq!(mac.agent.status().hotkeys.jump, None);
    let output = run_ctl(&socket, &["hotkeys", "set", "--jump-modifiers", "alt+gui"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let output = run_ctl(&socket, &["hotkeys", "set", "--lock", "KeyK"]).await;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("needs_modifier"),
        "{output:?}"
    );

    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    // The relative mode hotkey left out, as apps from before it do: it
    // stays as it is.
    let set = |take_back: &str, lock: &str, jump: &str| Hotkeys {
        take_back: take_back.to_owned(),
        lock: lock.to_owned(),
        jump_modifiers: jump.to_owned(),
        relative: None,
        take_back_double: None,
    };
    let tries = [
        (set("control+Kay", "control+KeyL", ""), "unknown_key"),
        (set("control+KeyT", "control+ShiftLeft", ""), "unknown_key"),
        (
            set("control+KeyT", "control+KeyL", "control+KeyL"),
            "unknown_key",
        ),
        (set("control+KeyL", "control+KeyL", ""), "duplicate"),
        (
            set("alt+control+Digit4", "control+KeyL", "control+alt"),
            "duplicate",
        ),
        (
            Hotkeys {
                relative: Some("control+KeyL".to_owned()),
                ..set("control+KeyT", "control+KeyL", "")
            },
            "duplicate_relative",
        ),
        (
            Hotkeys {
                relative: Some("KeyG".to_owned()),
                ..set("control+KeyT", "control+KeyL", "")
            },
            "needs_modifier",
        ),
    ];
    for (hotkeys, code) in tries {
        assert_eq!(
            refusal(client.set_hotkeys(hotkeys.clone()).await),
            code,
            "{hotkeys:?}"
        );
    }
    let status = client.status().await.unwrap();
    assert_eq!(
        status.hotkeys.unwrap(),
        Hotkeys {
            relative: Some("control+alt+shift+KeyG".to_owned()),
            take_back_double: Some(String::new()),
            ..set("control+alt+shift+Escape", "control+shift+KeyK", "alt+gui")
        }
    );
    // Moved elsewhere, the relative mode hotkey frees its keys for the
    // lock, and an app that leaves it empty keeps it.
    let output = run_ctl(
        &socket,
        &[
            "hotkeys",
            "set",
            "--relative",
            "control+alt+shift+KeyR",
            "--lock",
            "control+alt+shift+KeyG",
        ],
    )
    .await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("relative mouse:     control+alt+shift+KeyR\n"),
        "{output:?}"
    );
    let status = client
        .set_hotkeys(set(
            "control+alt+shift+Escape",
            "control+shift+KeyK",
            "alt+gui",
        ))
        .await
        .unwrap();
    assert_eq!(
        status.hotkeys.unwrap().relative.as_deref(),
        Some("control+alt+shift+KeyR")
    );

    let output = run_ctl(&socket, &["edge", "--modifier", "gui"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("modifier:       gui\n"),
        "{output:?}"
    );
    let status = client
        .set_switch_modifier(SwitchModifier::Control)
        .await
        .unwrap();
    assert_eq!(status.switch_modifier(), SwitchModifier::Control);
    drop(client);
    mac.agent.shutdown().await;

    let mac = start_with(
        &spec,
        Identity::generate().unwrap(),
        &[&spec],
        &[],
        options(),
    )
    .await;
    let status = mac.agent.status();
    assert_eq!(status.hotkeys.lock.to_string(), "control+shift+KeyK");
    assert_eq!(
        status.hotkeys.jump.map(|m| m.to_string()).as_deref(),
        Some("alt+gui")
    );
    assert_eq!(
        status.hotkeys.relative.map(|h| h.to_string()).as_deref(),
        Some("control+alt+shift+KeyR")
    );
    assert_eq!(
        status.switch_modifier,
        Some(iw_core::hotkey::Modifier::Control)
    );
    assert_eq!(mac.capture.reserved(), status.hotkeys.reserved());
    // Empty turns the relative mode hotkey off.
    let output = run_ctl(&socket, &["hotkeys", "set", "--relative", ""]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("relative mouse:     off\n"),
        "{output:?}"
    );
    assert_eq!(mac.agent.status().hotkeys.relative, None);

    // Back to the defaults, which leave nothing in the file.
    let output = run_ctl(&socket, &["hotkeys", "reset"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    assert_eq!(
        mac.agent.status().hotkeys,
        iw_core::hotkey::Hotkeys::default()
    );
    let saved = std::fs::read_to_string(dir.path().join("devices.toml")).unwrap();
    assert!(!saved.contains("[hotkeys]"), "{saved}");
    assert!(saved.contains("switch_modifier = \"control\""), "{saved}");
    mac.agent.shutdown().await;
}

/// Presses and lets go of the left Control key twice, as quickly as the
/// agent takes it.
fn control_twice(node: &Node) {
    for _ in 0..2 {
        key(node, usage::LEFT_CONTROL, true);
        key(node, usage::LEFT_CONTROL, false);
    }
}

/// How many times `node` pressed and let go of `usage` in its injections
/// from the `since`th on.
fn presses(node: &Node, usage: iw_core::HidUsage, since: usize) -> (usize, usize) {
    let calls = node.emulation.calls();
    let count = |pressed| {
        calls[since..]
            .iter()
            .filter(|c| **c == Emulated::Key(usage, pressed))
            .count()
    };
    (count(true), count(false))
}

/// Presses A on the Mac, controlling the PC, and waits for the PC to
/// inject it: everything sent before has arrived by then.
async fn settle(mac: &Node, win: &Node) {
    let since = win.emulation.calls().len();
    key(mac, usage::A, true);
    key(mac, usage::A, false);
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while presses(win, usage::A, since).1 == 0 {
        assert!(tokio::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Waits for the keyboard and mouse to be back on the Mac, as both see it.
async fn back_on_mac(mac: &Node, win: &Node) -> Status {
    let status = wait_status(mac, |s| s.cursor_on == "mac").await;
    wait_status(win, |s| s.controlled_by.is_none()).await;
    status
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_alone_and_control_twice_take_control_back() {
    use iw_core::hotkey::Hotkeys;

    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    // F16 for the lock, a key every platform's keyboards report.
    let (f13, f16) = (usage::F13, iw_core::HidUsage::from_name("F16").unwrap());
    let hotkeys =
        Hotkeys::parse("F13", "F16", "control+alt+shift", None, "double+control").unwrap();
    mac.agent.set_hotkeys(hotkeys).await.unwrap();
    // The reply comes once the status shows them.
    assert_eq!(mac.agent.status().hotkeys, hotkeys);
    // A lock alone is held back from local apps like any lock, and capture
    // knows of the double press, for while it waits for a grab.
    assert_eq!(mac.capture.reserved(), hotkeys.reserved());
    assert_eq!(mac.capture.reserved()[0], hotkeys.lock);
    assert!(mac.capture.watches_double_press());

    // F13 alone takes control back, and does not reach the PC.
    mac_enters_win(mac, win).await;
    key(mac, f13, true);
    key(mac, f13, false);
    let status = back_on_mac(mac, win).await;
    assert_eq!(status.last_return, Some(ReturnReason::Hotkey));
    assert!(!win.emulation.calls().contains(&Emulated::Key(f13, true)));
    // F16 alone locks and unlocks, and does not reach it either.
    mac_enters_win(mac, win).await;
    key(mac, f16, true);
    key(mac, f16, false);
    wait_status(mac, |s| s.locked).await;
    key(mac, f16, true);
    key(mac, f16, false);
    wait_status(mac, |s| !s.locked).await;
    settle(mac, win).await;
    assert!(!win.emulation.calls().contains(&Emulated::Key(f16, true)));

    // Control twice comes back too; the PC saw each press and release.
    let since = win.emulation.calls().len();
    control_twice(mac);
    let status = back_on_mac(mac, win).await;
    assert_eq!(status.last_return, Some(ReturnReason::Hotkey));
    assert!(!mac.capture.is_grabbed());
    assert_eq!(presses(win, usage::LEFT_CONTROL, since), (2, 2));

    // Told as such by capture (X11 while another app holds a grab): it
    // comes back, and the PC gets no Control.
    mac_enters_win(mac, win).await;
    let since = win.emulation.calls().len();
    mac.capture.emit(InputEvent::DoublePressed).unwrap();
    let status = back_on_mac(mac, win).await;
    assert_eq!(status.last_return, Some(ReturnReason::Hotkey));
    assert_eq!(presses(win, usage::LEFT_CONTROL, since), (0, 0));

    // Too slow: it stays on the PC, and Control still goes through.
    mac_enters_win(mac, win).await;
    let since = win.emulation.calls().len();
    key(mac, usage::LEFT_CONTROL, true);
    key(mac, usage::LEFT_CONTROL, false);
    tokio::time::sleep(Duration::from_millis(450)).await;
    key(mac, usage::LEFT_CONTROL, true);
    key(mac, usage::LEFT_CONTROL, false);
    settle(mac, win).await;
    assert_eq!(mac.agent.status().cursor_on, "win");
    assert_eq!(presses(win, usage::LEFT_CONTROL, since), (2, 2));
    // Another key between: the same.
    tokio::time::sleep(Duration::from_millis(450)).await;
    key(mac, usage::LEFT_CONTROL, true);
    key(mac, usage::LEFT_CONTROL, false);
    key(mac, usage::A, true);
    key(mac, usage::A, false);
    key(mac, usage::LEFT_CONTROL, true);
    key(mac, usage::LEFT_CONTROL, false);
    settle(mac, win).await;
    assert_eq!(mac.agent.status().cursor_on, "win");

    // Turned off, it is Control and nothing more.
    let off = Hotkeys {
        take_back_double: false,
        ..hotkeys
    };
    mac.agent.set_hotkeys(off).await.unwrap();
    assert!(!mac.capture.watches_double_press());
    tokio::time::sleep(Duration::from_millis(450)).await;
    control_twice(mac);
    settle(mac, win).await;
    assert_eq!(mac.agent.status().cursor_on, "win");
    // F13 alone takes back with Control held too, and Control is let go
    // of on the PC as the cursor leaves.
    let since = win.emulation.calls().len();
    key(mac, usage::LEFT_CONTROL, true);
    key(mac, f13, true);
    key(mac, f13, false);
    key(mac, usage::LEFT_CONTROL, false);
    back_on_mac(mac, win).await;
    assert_eq!(presses(win, usage::LEFT_CONTROL, since), (1, 1));

    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// Hotkeys put in effect that the devices file could not take are not
/// said to be remembered: the answer says they are in effect but not
/// saved, and the status shows them.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn hotkeys_the_devices_file_cannot_take_are_said_not_to_be_saved() {
    use iw_proto::app::v1::Hotkeys;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let file = dir.path().join("devices.toml");
    let spec = mac();
    let mac = start_with(
        &spec,
        Identity::generate().unwrap(),
        &[&spec],
        &[],
        Options {
            ipc_socket: Some(socket.clone()),
            devices_file: Some(file.clone()),
            ..Options::default()
        },
    )
    .await;
    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    let current = client.status().await.unwrap().hotkeys.unwrap();
    // A folder where the file goes: it cannot be replaced. A save the
    // agent makes as it starts may put the file back in between: again.
    loop {
        let _ = std::fs::remove_file(&file);
        match std::fs::create_dir(&file) {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("{e}"),
        }
    }
    let lock = Hotkeys {
        lock: "F13".to_owned(),
        ..current
    };
    match client.set_hotkeys(lock.clone()).await {
        Err(inputweave::ctl::CtlError::Agent { code, detail }) => {
            assert_eq!(code, "not_saved");
            assert!(detail.contains("in effect"), "{detail}");
        }
        other => panic!("not refused: {other:?}"),
    }
    // In effect all the same.
    let status = client.status().await.unwrap();
    assert_eq!(status.hotkeys.unwrap().lock, "F13");
    assert_eq!(mac.agent.status().hotkeys.lock.to_string(), "F13");

    // Once the file can be written, the next change is, with that one.
    std::fs::remove_dir(&file).unwrap();
    let both = Hotkeys {
        take_back: "F14".to_owned(),
        ..lock
    };
    client.set_hotkeys(both).await.unwrap();
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(
        saved.contains("\"F13\"") && saved.contains("\"F14\""),
        "{saved}"
    );
    mac.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn keys_alone_and_the_double_press_are_set_over_ipc_and_remembered() {
    use iw_proto::app::v1::Hotkeys;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let spec = mac();
    let options = || Options {
        ipc_socket: Some(socket.clone()),
        devices_file: Some(dir.path().join("devices.toml")),
        ..Options::default()
    };
    let mac = start_with(
        &spec,
        Identity::generate().unwrap(),
        &[&spec],
        &[],
        options(),
    )
    .await;
    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    // A new agent says what the double press is, off at first.
    let status = client.status().await.unwrap();
    assert_eq!(
        status.hotkeys.unwrap().take_back_double.as_deref(),
        Some("")
    );
    let set = |take_back: &str, lock: &str, relative: &str, double: Option<&str>| Hotkeys {
        take_back: take_back.to_owned(),
        lock: lock.to_owned(),
        jump_modifiers: "control+alt+shift".to_owned(),
        relative: Some(relative.to_owned()),
        take_back_double: double.map(str::to_owned),
    };
    let relative = "control+alt+shift+KeyG";
    let tries = [
        // Keys alone only where they work alone.
        (
            set("KeyK", "control+KeyL", relative, None),
            "needs_modifier",
        ),
        (set("control+KeyT", "F12", relative, None), "needs_modifier"),
        (
            set("control+KeyT", "control+KeyL", "F13", None),
            "needs_modifier",
        ),
        (set("F13", "F13", relative, None), "duplicate"),
        (set("control+F13", "F13", relative, None), "duplicate"),
        (
            set("F13", "control+KeyL", "alt+F13", None),
            "duplicate_relative",
        ),
        (
            set("F13", "control+KeyL", relative, Some("double+shift")),
            "unknown_key",
        ),
        (
            set("F13", "control+KeyL", relative, Some("on")),
            "unknown_key",
        ),
    ];
    // A Mac's keyboards never report Scroll Lock or Pause (Apple's extended
    // keyboard has F14 there): on one, a hotkey on them could never fire.
    for key in ["ScrollLock", "Pause"] {
        let result = client
            .set_hotkeys(set(key, "control+KeyL", relative, None))
            .await;
        if cfg!(target_os = "macos") {
            assert_eq!(refusal(result), "unknown_key", "{key}");
        } else {
            result.unwrap();
        }
    }
    client
        .set_hotkeys(set(
            "control+alt+shift+Escape",
            "control+alt+shift+KeyL",
            relative,
            Some(""),
        ))
        .await
        .unwrap();
    for (hotkeys, code) in tries {
        assert_eq!(
            refusal(client.set_hotkeys(hotkeys.clone()).await),
            code,
            "{hotkeys:?}"
        );
    }
    assert_eq!(
        mac.agent.status().hotkeys,
        iw_core::hotkey::Hotkeys::default()
    );

    let status = client
        .set_hotkeys(set("F13", "F14", relative, Some("Double+Control")))
        .await
        .unwrap();
    assert_eq!(
        status.hotkeys.unwrap(),
        set("F13", "F14", relative, Some("double+control"))
    );
    // F13 takes back and ⌃F13 locks: the lock, looked at first, needs
    // Control.
    client
        .set_hotkeys(set("F13", "control+F13", relative, None))
        .await
        .unwrap();
    // An app from before the double press leaves it out: it stays on.
    let status = client
        .set_hotkeys(set("F13", "F15", relative, None))
        .await
        .unwrap();
    let hotkeys = status.hotkeys.unwrap();
    assert_eq!(hotkeys.lock, "F15");
    assert_eq!(hotkeys.take_back_double.as_deref(), Some("double+control"));
    let saved = std::fs::read_to_string(dir.path().join("devices.toml")).unwrap();
    assert!(
        saved.contains("take_back_double = \"double+control\""),
        "{saved}"
    );

    // From the command line.
    let output = run_ctl(&socket, &["hotkeys"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("take back control:  F13\n")
            && stdout.contains("lock:               F15\n")
            && stdout.contains("double press:       double+control\n"),
        "{stdout}"
    );
    let output = run_ctl(&socket, &["hotkeys", "set", "--take-back-double", ""]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("double press:       off\n"),
        "{output:?}"
    );
    let output = run_ctl(
        &socket,
        &["hotkeys", "set", "--take-back-double", "double+alt"],
    )
    .await;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown_key"),
        "{output:?}"
    );
    let output = run_ctl(
        &socket,
        &[
            "hotkeys",
            "set",
            "--take-back-double",
            "double+control",
            "--lock",
            "F19",
        ],
    )
    .await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let output = run_ctl(&socket, &["status"]).await;
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("hotkeys:        take back F13 or double+control, lock F19,"),
        "{output:?}"
    );
    drop(client);
    mac.agent.shutdown().await;

    // Remembered.
    let mac = start_with(
        &spec,
        Identity::generate().unwrap(),
        &[&spec],
        &[],
        options(),
    )
    .await;
    let status = mac.agent.status();
    assert_eq!(status.hotkeys.take_back.to_string(), "F13");
    assert_eq!(status.hotkeys.lock.to_string(), "F19");
    assert!(status.hotkeys.take_back_double);
    assert_eq!(mac.capture.reserved(), status.hotkeys.reserved());
    // Back to the defaults, the double press off too, leaving nothing.
    let output = run_ctl(&socket, &["hotkeys", "reset"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    assert_eq!(
        mac.agent.status().hotkeys,
        iw_core::hotkey::Hotkeys::default()
    );
    let saved = std::fs::read_to_string(dir.path().join("devices.toml")).unwrap();
    assert!(!saved.contains("[hotkeys]"), "{saved}");
    mac.agent.shutdown().await;
}

/// Hotkeys in the devices file that cannot be used leave the defaults in
/// effect; setting the defaults then writes the file as it should be.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn setting_the_hotkeys_in_effect_mends_a_file_with_unusable_ones() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let file = dir.path().join("devices.toml");
    std::fs::write(&file, "[hotkeys]\ntake_back = \"KeyK\"\n").unwrap();
    let spec = mac();
    let mac = start_with(
        &spec,
        Identity::generate().unwrap(),
        &[&spec],
        &[],
        Options {
            ipc_socket: Some(socket.clone()),
            devices_file: Some(file.clone()),
            ..Options::default()
        },
    )
    .await;
    assert_eq!(
        mac.agent.status().hotkeys,
        iw_core::hotkey::Hotkeys::default()
    );
    let output = run_ctl(&socket, &["hotkeys", "reset"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(!saved.contains("[hotkeys]"), "{saved}");
    mac.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn hotkeys_from_two_apps_at_once_are_all_answered() {
    use iw_proto::app::v1::Hotkeys;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let nodes = desk_with(&[mac(), win()], ipc_on_mac(&socket)).await;
    let mac = &nodes[0];
    let mut one = inputweave::ctl::Client::connect(&socket).await.unwrap();
    let mut other = inputweave::ctl::Client::connect(&socket).await.unwrap();
    let set = |lock: &str, double: Option<&str>| Hotkeys {
        take_back: "control+alt+shift+Escape".to_owned(),
        lock: lock.to_owned(),
        jump_modifiers: "control+alt+shift".to_owned(),
        relative: None,
        take_back_double: double.map(str::to_owned),
    };
    // Each may be replaced by the other before the status shows it; the
    // answer does not wait for its own value, so neither times out.
    for round in 0..5 {
        let (a, b) = tokio::join!(
            one.set_hotkeys(set(
                "F13",
                Some(if round % 2 == 0 { "double+control" } else { "" })
            )),
            other.set_hotkeys(set("control+alt+KeyK", None))
        );
        a.unwrap();
        b.unwrap();
    }
    // The one that left the double press out did not undo the other's.
    let (a, b) = tokio::join!(
        one.set_hotkeys(set("F13", Some("double+control"))),
        other.set_hotkeys(set("F14", None))
    );
    a.unwrap();
    b.unwrap();
    let hotkeys = mac.agent.status().hotkeys;
    assert!(hotkeys.take_back_double);
    assert!(["F13", "F14"].contains(&hotkeys.lock.to_string().as_str()));

    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// Bytes that pass for a JPEG: the right start, then `len` bytes in all.
fn fake_jpeg(seed: u8, len: usize) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xd8, 0xff];
    bytes.resize(len.max(3), seed);
    bytes
}

/// The picture `node` has for a device's display.
fn picture(
    arrangement: &inputweave::agent::Arrangement,
    device: &str,
    display: &str,
) -> Option<Vec<u8>> {
    let pictures = &arrangement.device(device)?.pictures;
    pictures.get(display).map(|p| p.bytes().to_vec())
}

async fn wait_arrangement(
    node: &Node,
    mut ready: impl FnMut(&inputweave::agent::Arrangement) -> bool,
) -> inputweave::agent::Arrangement {
    let mut rx = node.agent.watch_arrangement();
    tokio::time::timeout(TIMEOUT, rx.wait_for(|a| ready(a)))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out; last arrangement: {:#?}",
                node.agent.arrangement()
            )
        })
        .expect("agent running")
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn desktop_pictures_reach_other_devices_and_follow_the_switch() {
    let mac_pictures = MockDesktopPictures::default();
    mac_pictures.set("builtin", fake_jpeg(1, 2000));
    let nodes = desk_showing(&[mac(), win()], |s| {
        if s.id == "mac" {
            mac_pictures.clone()
        } else {
            MockDesktopPictures::default()
        }
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // The PC has the Mac's picture on the Mac's display; the Mac has its
    // own. The PC shows none, so none of its displays has one.
    let on_win = wait_arrangement(win, |a| picture(a, "mac", "builtin").is_some()).await;
    assert_eq!(picture(&on_win, "mac", "builtin"), Some(fake_jpeg(1, 2000)));
    assert!(on_win.device("win").unwrap().pictures.is_empty());
    assert_eq!(
        picture(&mac.agent.arrangement(), "mac", "builtin"),
        Some(fake_jpeg(1, 2000))
    );
    // As the app gets it: on the screen with the same id.
    let proto = on_win.to_proto();
    let mac_device = proto.devices.iter().find(|d| d.id == "mac").unwrap();
    assert_eq!(mac_device.screens[0].id, "builtin");
    assert_eq!(mac_device.screens[0].preview_jpeg, fake_jpeg(1, 2000));
    let win_device = proto.devices.iter().find(|d| d.id == "win").unwrap();
    assert!(win_device.screens.iter().all(|s| s.preview_jpeg.is_empty()));

    // A display plugged in with another picture, and the first picture
    // changed: both are read at once and sent again.
    mac.pictures.set("builtin", fake_jpeg(2, 3000));
    mac.pictures.set("external", fake_jpeg(3, 1000));
    mac.displays.set(vec![
        display("builtin", 0.0, 0.0, 1512.0, 982.0, true),
        display("external", -1920.0, 0.0, 1920.0, 1080.0, false),
    ]);
    wait_arrangement(win, |a| {
        picture(a, "mac", "builtin") == Some(fake_jpeg(2, 3000))
            && picture(a, "mac", "external") == Some(fake_jpeg(3, 1000))
    })
    .await;

    // Turned off, the PC drops them at once and keeps the displays; the
    // Mac still has its own.
    mac.agent.set_preview_sharing(false);
    wait_status(mac, |s| !s.preview_sharing).await;
    let on_win = wait_arrangement(win, |a| {
        a.device("mac").is_some_and(|d| d.pictures.is_empty())
    })
    .await;
    assert_eq!(on_win.device("mac").unwrap().displays.len(), 2);
    assert_eq!(
        mac.agent
            .arrangement()
            .device("mac")
            .unwrap()
            .pictures
            .len(),
        2
    );

    // Back on, they are sent again.
    mac.agent.set_preview_sharing(true);
    wait_arrangement(win, |a| picture(a, "mac", "external").is_some()).await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_desktop_pictures_are_dropped_and_the_session_stays() {
    let mac_pictures = MockDesktopPictures::default();
    mac_pictures.set("builtin", fake_jpeg(1, 200 * 1024));
    let nodes = desk_showing(&[mac(), win()], |s| {
        if s.id == "mac" {
            mac_pictures.clone()
        } else {
            MockDesktopPictures::default()
        }
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // The Mac does not send a picture over the limit: the PC knows the
    // Mac's display but has no picture for it.
    let on_win = win.agent.arrangement();
    let mac_on_win = on_win.device("mac").unwrap();
    assert_eq!(mac_on_win.displays.len(), 1);
    assert!(mac_on_win.pictures.is_empty());

    // The session is still up: input crosses over.
    mac_enters_win(mac, win).await;
    key(mac, usage::A, true);
    wait_injected(win, Emulated::Key(usage::A, true)).await;
    key(mac, usage::A, false);
    motion(mac, (0.0, 0.0), (-20.0, 0.0));
    wait_status(mac, |s| s.cursor_on == "mac").await;

    // A picture that fits gets through on the next Displays.
    mac.pictures.set("builtin", fake_jpeg(4, 50 * 1024));
    mac.displays
        .set(vec![display("builtin", 0.0, 0.0, 1512.0, 980.0, true)]);
    wait_arrangement(win, |a| {
        picture(a, "mac", "builtin") == Some(fake_jpeg(4, 50 * 1024))
    })
    .await;
    assert!(win.agent.status().device("mac").unwrap().connected);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn desktop_pictures_outlast_the_connection_until_unpaired() {
    let dir = tempfile::tempdir().unwrap();
    let mac_file = dir.path().join("devices.toml");
    let mac_key = dir.path().join("identity");
    let previews = dir.path().join("previews").join("win");
    let spec = mac();
    let win_pictures = MockDesktopPictures::default();
    win_pictures.set("main", fake_jpeg(5, 4000));
    win_pictures.set("side", fake_jpeg(6, 6000));
    let win = unpaired_showing(&win(), Identity::generate().unwrap(), None, win_pictures).await;
    let mac = unpaired(
        &spec,
        Identity::load_or_generate(&mac_key).unwrap(),
        Some(mac_file.clone()),
    )
    .await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    wait_arrangement(&mac, |a| {
        picture(a, "win", "main").is_some() && picture(a, "win", "side").is_some()
    })
    .await;
    // One file per display, next to the devices file.
    assert_eq!(std::fs::read_dir(&previews).unwrap().count(), 2);
    mac.agent.set_preview_sharing(false);
    wait_status(&mac, |s| !s.preview_sharing).await;

    // The PC goes away; its pictures stay.
    win.agent.shutdown().await;
    wait_status(&mac, |s| s.device("win").is_some_and(|d| !d.connected)).await;
    assert_eq!(
        picture(&mac.agent.arrangement(), "win", "main"),
        Some(fake_jpeg(5, 4000))
    );
    mac.agent.shutdown().await;

    // Restarted without the PC: the pictures come back from the files, and
    // sharing stays off.
    let mac = unpaired(
        &spec,
        Identity::load_or_generate(&mac_key).unwrap(),
        Some(mac_file),
    )
    .await;
    assert!(!mac.agent.status().preview_sharing);
    let now = mac.agent.arrangement();
    let win_now = now.device("win").unwrap();
    assert!(!win_now.connected);
    assert_eq!(picture(&now, "win", "main"), Some(fake_jpeg(5, 4000)));
    assert_eq!(picture(&now, "win", "side"), Some(fake_jpeg(6, 6000)));

    // Unpairing forgets them, files and all.
    mac.agent.unpair("win").unwrap();
    wait_status(&mac, |s| s.device("win").is_none()).await;
    assert!(!previews.exists());
    assert!(mac.agent.arrangement().device("win").is_none());
    mac.agent.shutdown().await;
}

/// Shakes `node`'s mouse: five quick strokes of 30 back and forth, the
/// first to the right. Coming after a stroke to the right, or from rest,
/// the fifth makes the fourth turn, and so a shake. `at` is where local
/// capture reports the cursor.
fn shake(node: &Node, at: (f64, f64)) {
    let mut x = at.0;
    for i in 0..5 {
        let dx = if i % 2 == 0 { 30.0 } else { -30.0 };
        x += dx;
        motion(node, (x, at.1), (dx, 0.0));
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn shaking_the_mouse_shows_the_cursor_on_the_device_it_is_on() {
    use iw_ipc::IpcConnection;
    use iw_proto::app::v1::{
        AgentMessage, ClientHello, ClientMessage, CursorLocated, agent_message,
        client_message::Command, event, response,
    };

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("win.sock");
    let nodes = desk_with(&[mac(), win()], |spec| Options {
        ipc_socket: (spec.id == "win").then(|| socket.clone()),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // The PC's app, listening.
    let mut app = IpcConnection::connect(&socket).await.unwrap();
    app.send(&ClientMessage {
        request_id: 1,
        command: Some(Command::Hello(ClientHello {
            ipc_version: iw_proto::IPC_VERSION,
            client: "test".into(),
            ..ClientHello::default()
        })),
    })
    .await
    .unwrap();
    loop {
        let msg = app.recv::<AgentMessage>().await.unwrap().unwrap();
        if let Some(agent_message::Msg::Response(r)) = msg.msg {
            assert!(matches!(r.result, Some(response::Result::Hello(_))));
            break;
        }
    }

    mac_enters_win(mac, win).await;
    motion(mac, (0.0, 0.0), (200.0, 0.0));
    wait_injected(win, Emulated::MoveTo(Point::new(204.0, 900.0))).await;
    shake(mac, (0.0, 0.0));

    let located = tokio::time::timeout(TIMEOUT, async {
        loop {
            let msg = app.recv::<AgentMessage>().await.unwrap().unwrap();
            if let Some(agent_message::Msg::Event(e)) = msg.msg
                && let Some(event::Event::CursorLocated(located)) = e.event
            {
                return located;
            }
        }
    })
    .await
    .expect("the PC is told where the cursor is");
    assert_eq!(located, CursorLocated { x: 234.0, y: 900.0 });
    assert_eq!(
        *win.agent.watch_cursor_located().borrow(),
        Some(Point::new(234.0, 900.0))
    );
    // The controller's own screen shows nothing: the cursor is not there.
    assert_eq!(*mac.agent.watch_cursor_located().borrow(), None);
    // Shaking moved the cursor and nothing else.
    assert_eq!(mac.agent.status().cursor_on, "win");

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shaking_the_mouse_here_shows_the_cursor_here() {
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let mut located = mac.agent.watch_cursor_located();

    shake(mac, (700.0, 500.0));
    let at = *tokio::time::timeout(TIMEOUT, located.wait_for(Option::is_some))
        .await
        .expect("the cursor is shown")
        .unwrap();
    assert_eq!(at, Some(Point::new(730.0, 500.0)));
    assert_eq!(mac.agent.status().cursor_on, "mac");
    assert!(!mac.capture.is_grabbed());
    assert_eq!(*win.agent.watch_cursor_located().borrow(), None);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// Sends what the Windows broker sends: the pictures on the desktops.
#[cfg(unix)]
async fn broker_sends(broker: &mut iw_ipc::IpcConnection, pictures: &[(&str, Vec<u8>)]) {
    use iw_proto::service::v1::{BrokerMessage, DesktopPictures, DisplayPicture, broker_message};

    let pictures = pictures
        .iter()
        .map(|(id, jpeg)| DisplayPicture {
            display_id: (*id).to_owned(),
            jpeg: jpeg.clone(),
        })
        .collect();
    broker
        .send(&BrokerMessage {
            msg: Some(broker_message::Msg::DesktopPictures(DesktopPictures {
                pictures,
            })),
        })
        .await
        .unwrap();
}

/// Waits for the agent to ask the broker to read the pictures again,
/// passing over its request for the transfer history, which this broker
/// does not keep.
#[cfg(unix)]
async fn broker_asked(broker: &mut iw_ipc::IpcConnection) {
    use iw_proto::service::v1::{AgentToBroker, agent_to_broker};

    let asked = loop {
        let asked = tokio::time::timeout(TIMEOUT, broker.recv::<AgentToBroker>())
            .await
            .expect("the agent asks")
            .unwrap()
            .expect("still connected");
        if !matches!(asked.msg, Some(agent_to_broker::Msg::LoadHistory(_))) {
            break asked;
        }
    };
    assert!(matches!(
        asked.msg,
        Some(agent_to_broker::Msg::RequestDesktopPictures(_))
    ));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_broker_provides_the_desktop_pictures_of_a_windows_pc() {
    let dir = tempfile::tempdir().unwrap();
    let pipe = dir.path().join("broker.sock");
    // The PC's pictures come from its broker; its own backend has none.
    let nodes = desk_with(&[mac(), win()], |s| Options {
        broker_socket: (s.id == "win").then(|| pipe.clone()),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    assert!(mac.agent.status().desktop_pictures);
    // Nothing reads the PC's pictures until the broker connects.
    assert!(!win.agent.status().desktop_pictures);

    let mut broker = iw_ipc::IpcConnection::connect(&pipe).await.unwrap();
    wait_status(win, |s| s.desktop_pictures).await;
    broker_sends(
        &mut broker,
        &[("main", fake_jpeg(1, 2000)), ("side", fake_jpeg(2, 1000))],
    )
    .await;
    // They show on the PC and reach the Mac.
    wait_arrangement(mac, |a| {
        picture(a, "win", "main") == Some(fake_jpeg(1, 2000))
            && picture(a, "win", "side") == Some(fake_jpeg(2, 1000))
    })
    .await;
    assert_eq!(
        picture(&win.agent.arrangement(), "win", "side"),
        Some(fake_jpeg(2, 1000))
    );

    // Each time the agent reads them it asks the broker to look again;
    // what the broker answers goes out too.
    broker_asked(&mut broker).await;
    broker_sends(&mut broker, &[("main", fake_jpeg(3, 3000))]).await;
    wait_arrangement(mac, |a| {
        picture(a, "win", "main") == Some(fake_jpeg(3, 3000)) && picture(a, "win", "side").is_none()
    })
    .await;

    // The broker is held to the limits other devices are: one picture
    // over them drops them all.
    broker_sends(
        &mut broker,
        &[
            ("main", fake_jpeg(4, 2000)),
            ("side", fake_jpeg(5, 200 * 1024)),
        ],
    )
    .await;
    wait_arrangement(mac, |a| {
        a.device("win").is_some_and(|d| d.pictures.is_empty())
    })
    .await;
    assert!(
        win.agent
            .arrangement()
            .device("win")
            .unwrap()
            .pictures
            .is_empty()
    );
    broker_sends(&mut broker, &[("main", fake_jpeg(6, 2000))]).await;
    wait_arrangement(mac, |a| {
        picture(a, "win", "main") == Some(fake_jpeg(6, 2000))
    })
    .await;

    // Without the broker the pictures cannot be read any more; the last
    // ones stay, here and on the Mac.
    drop(broker);
    wait_status(win, |s| !s.desktop_pictures).await;
    assert_eq!(
        picture(&win.agent.arrangement(), "win", "main"),
        Some(fake_jpeg(6, 2000))
    );
    assert_eq!(
        picture(&mac.agent.arrangement(), "win", "main"),
        Some(fake_jpeg(6, 2000))
    );

    // A broker that starts again takes over.
    let mut broker = iw_ipc::IpcConnection::connect(&pipe).await.unwrap();
    wait_status(win, |s| s.desktop_pictures).await;
    broker_sends(&mut broker, &[("side", fake_jpeg(7, 2000))]).await;
    wait_arrangement(mac, |a| {
        picture(a, "win", "side") == Some(fake_jpeg(7, 2000)) && picture(a, "win", "main").is_none()
    })
    .await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// What a simulated Windows broker holds.
#[cfg(unix)]
#[derive(Default)]
struct BrokerDesk {
    /// What reading the user's clipboard finds.
    clipboard: Option<iw_platform::ClipboardRead>,
    sequence: u64,
    /// Reads answered.
    reads: usize,
    /// What the agent wrote to the clipboard.
    written: Vec<iw_platform::ClipboardContent>,
    /// What the agent promised on the clipboard: each promise's number,
    /// formats and size.
    promised: Vec<(u64, Vec<String>, u64)>,
    /// The promises the agent withdrew.
    withdrawn: Vec<u64>,
    /// Files and folders the user may send, by the path the user knows
    /// them by, and where they really are; only the broker reads them.
    /// What is in a folder shared so is shared too.
    sendable: std::collections::HashMap<std::path::PathBuf, std::path::PathBuf>,
    /// Paths the user may not read.
    denied: std::collections::HashSet<std::path::PathBuf>,
    /// Files and folders copied in File Explorer, by the paths the user
    /// knows them by.
    copied: Vec<std::path::PathBuf>,
    /// The paths the agent had listed, in order.
    listed: Vec<std::path::PathBuf>,
    /// The transfers whose files the broker opened to send.
    opened: Vec<String>,
    /// Times the agent asked for the app.
    launches: usize,
    /// The broker says it cannot start the app.
    launch_refused: bool,
    /// Why the agent asked for the app, each time.
    launch_reasons: Vec<iw_proto::service::v1::LaunchReason>,
    /// The user's transfer history file, as the agent last saved it.
    history: Vec<u8>,
    /// Times the agent saved it.
    saves: usize,
    /// Why the broker cannot read the file, when it cannot.
    history_error: String,
    /// Times the agent asked for the file.
    loads: usize,
}

#[cfg(unix)]
impl BrokerDesk {
    /// Where the file or folder the user knows as `path` really is.
    fn resolve(&self, path: &std::path::Path) -> Option<std::path::PathBuf> {
        self.sendable.iter().find_map(|(user, real)| {
            let inside = path.strip_prefix(user).ok()?;
            Some(if inside.as_os_str().is_empty() {
                real.clone()
            } else {
                real.join(inside)
            })
        })
    }
}

/// A stand-in for the Windows broker on an agent's broker pipe: the user's
/// clipboard in memory, and a folder it writes received files into, as
/// the real one does.
#[cfg(unix)]
struct SimBroker {
    desk: Arc<std::sync::Mutex<BrokerDesk>>,
    copies: tokio::sync::mpsc::UnboundedSender<u64>,
    renders: tokio::sync::mpsc::UnboundedSender<SimRender>,
    task: tokio::task::JoinHandle<()>,
}

/// What the simulated broker's clipboard does with a promise.
#[cfg(unix)]
enum SimRender {
    /// An app pastes a format; the agent's answer goes to `reply`.
    Paste {
        promise_id: u64,
        mime: String,
        reply: tokio::sync::oneshot::Sender<Option<Vec<u8>>>,
    },
    /// Something else is copied over it.
    Gone(u64),
}

/// A paste waiting for the agent's answer: the promise, the format, and
/// where the answer goes.
#[cfg(unix)]
type Pasting = (u64, String, tokio::sync::oneshot::Sender<Option<Vec<u8>>>);

#[cfg(unix)]
impl SimBroker {
    /// Connects to `pipe`, naming `folder` as where files go.
    async fn connect(pipe: &std::path::Path, folder: &std::path::Path) -> Self {
        Self::connect_keeping(pipe, folder, Vec::new()).await
    }

    /// Connects to `pipe` as a user whose transfer history file holds
    /// `history`, naming `folder` as where files go.
    async fn connect_keeping(
        pipe: &std::path::Path,
        folder: &std::path::Path,
        history: Vec<u8>,
    ) -> Self {
        Self::connect_with_history(pipe, folder, history, String::new()).await
    }

    /// Like `connect_keeping`, but the broker cannot read the file and
    /// answers every load with `error` instead.
    async fn connect_with_history(
        pipe: &std::path::Path,
        folder: &std::path::Path,
        history: Vec<u8>,
        history_error: String,
    ) -> Self {
        use iw_proto::service::v1::{BrokerMessage, ReceiveFolder, broker_message};

        let conn = iw_ipc::IpcConnection::connect(pipe).await.unwrap();
        let (reader, mut writer) = conn.into_split();
        writer
            .send(&BrokerMessage {
                msg: Some(broker_message::Msg::ReceiveFolder(ReceiveFolder {
                    path: folder.to_string_lossy().into_owned(),
                })),
            })
            .await
            .unwrap();
        let desk = Arc::new(std::sync::Mutex::new(BrokerDesk {
            history,
            history_error,
            ..BrokerDesk::default()
        }));
        let (copies, copied) = tokio::sync::mpsc::unbounded_channel();
        let (renders, render_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(sim_broker(
            reader,
            writer,
            desk.clone(),
            copied,
            render_rx,
            folder.to_owned(),
        ));
        Self {
            desk,
            copies,
            renders,
            task,
        }
    }

    /// Waits for the agent to promise content on the clipboard, and
    /// returns the newest promise.
    /// The `n`th promise the agent made, once it did.
    async fn wait_promises(&self, n: usize) -> (u64, Vec<String>, u64) {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            if let Some(promise) = self.desk.lock().unwrap().promised.get(n - 1) {
                return promise.clone();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the agent made {} promises, not {n}",
                self.desk.lock().unwrap().promised.len()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_promised(&self) -> (u64, Vec<String>, u64) {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            if let Some(promise) = self.desk.lock().unwrap().promised.last() {
                return promise.clone();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the agent never promised content"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// As if an app on the PC pasted `mime` of promise `promise_id`: the
    /// broker asks the agent for it, as on WM_RENDERFORMAT.
    async fn paste(&self, promise_id: u64, mime: &str) -> Option<Vec<u8>> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.renders
            .send(SimRender::Paste {
                promise_id,
                mime: mime.to_owned(),
                reply,
            })
            .unwrap();
        tokio::time::timeout(TIMEOUT, answer)
            .await
            .expect("the agent answers")
            .unwrap()
    }

    /// As if the user copied something else over promise `promise_id`.
    fn copy_over(&self, promise_id: u64) {
        self.renders.send(SimRender::Gone(promise_id)).unwrap();
    }

    /// As if the user copied on the PC; waits for the agent to read it.
    async fn copy(&self, found: iw_platform::ClipboardRead) {
        self.copy_files(Vec::new(), found).await;
    }

    /// As if the user copied `files` in File Explorer on the PC, the
    /// clipboard holding `found` besides; waits for the agent to read it.
    async fn copy_files(&self, files: Vec<std::path::PathBuf>, found: iw_platform::ClipboardRead) {
        let reads = {
            let mut desk = self.desk.lock().unwrap();
            desk.copied = files;
            desk.clipboard = Some(found);
            desk.sequence += 1;
            self.copies.send(desk.sequence).unwrap();
            desk.reads
        };
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while self.desk.lock().unwrap().reads == reads {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the agent never read the clipboard"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // For the agent to take the answer in.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    /// Lets the user send the file at `real` as `path`, a path only the
    /// broker knows.
    fn share(&self, path: &std::path::Path, real: &std::path::Path) {
        let mut desk = self.desk.lock().unwrap();
        desk.sendable.insert(path.to_owned(), real.to_owned());
    }

    async fn wait_written(&self, expected: &iw_platform::ClipboardContent) {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while self.desk.lock().unwrap().written.last() != Some(expected) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the agent never wrote {expected:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Goes away, as the broker does when the user logs off.
    async fn quit(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

#[cfg(unix)]
async fn sim_broker(
    mut reader: iw_ipc::IpcReader,
    mut writer: iw_ipc::IpcWriter,
    desk: Arc<std::sync::Mutex<BrokerDesk>>,
    mut copied: tokio::sync::mpsc::UnboundedReceiver<u64>,
    mut renders: tokio::sync::mpsc::UnboundedReceiver<SimRender>,
    _receive_folder: std::path::PathBuf,
) {
    use std::io::Write;

    use iw_platform::{ClipboardContent, ClipboardFormat, ClipboardRead};
    use iw_proto::service::v1::ClipboardPromised;
    use iw_proto::service::v1::{
        AgentToBroker, AppLaunched, BrokerMessage, ChunkData, ClipboardChanged, ClipboardContents,
        ClipboardData, ClipboardPart, ClipboardState, FileFailed, FileFinished, FileHashed,
        FileKept, FileOpened, FolderReady, HistoryLoaded, HistorySaved, OpenFailed, OpenFailure,
        OpenedForSending, PathEntry, PathKind, PathListed, PromiseGone, RenderClipboard,
        agent_to_broker, broker_message::Msg,
    };

    let says = |msg: Msg| BrokerMessage { msg: Some(msg) };
    let failed = |handle: u64, error: &str| {
        says(Msg::FileFailed(FileFailed {
            handle,
            error: error.to_owned(),
        }))
    };
    let ready = |handle: u64, path: &std::path::Path| {
        says(Msg::FolderReady(FolderReady {
            handle,
            path: path.to_string_lossy().into_owned(),
        }))
    };
    // Part files by handle: where each is, open, in which folder.
    let mut files: std::collections::HashMap<
        u64,
        (std::path::PathBuf, std::fs::File, std::path::PathBuf),
    > = Default::default();
    let mut sending: std::collections::HashMap<u64, std::path::PathBuf> = Default::default();
    // The paste waiting for the agent's answer, as the clipboard thread
    // asks for one format at a time.
    let mut pasting: Option<Pasting> = None;
    loop {
        let msg = tokio::select! {
            msg = reader.recv::<AgentToBroker>() => match msg {
                Ok(Some(AgentToBroker { msg: Some(msg) })) => msg,
                Ok(Some(_)) => continue,
                _ => return,
            },
            Some(sequence) = copied.recv() => {
                let changed = says(Msg::ClipboardChanged(ClipboardChanged { sequence }));
                writer.send(&changed).await.unwrap();
                continue;
            }
            Some(render) = renders.recv() => {
                let ask = match render {
                    SimRender::Paste { promise_id, mime, reply } => {
                        let ask = says(Msg::RenderClipboard(RenderClipboard {
                            promise_id,
                            mime: mime.clone(),
                        }));
                        pasting = Some((promise_id, mime, reply));
                        ask
                    }
                    SimRender::Gone(promise_id) => {
                        says(Msg::PromiseGone(PromiseGone { promise_id }))
                    }
                };
                writer.send(&ask).await.unwrap();
                continue;
            }
        };
        let mut answers = Vec::new();
        match msg {
            agent_to_broker::Msg::ReadClipboard(read) => {
                let (found, copied) = {
                    let desk = desk.lock().unwrap();
                    (desk.clipboard.clone(), desk.copied.clone())
                };
                // As File Explorer copies them, by the paths the user knows.
                let files: Vec<String> = copied
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                let header = |state: ClipboardState, parts, size| {
                    let files = match state {
                        ClipboardState::Sensitive => Vec::new(),
                        _ => files.clone(),
                    };
                    says(Msg::ClipboardContents(ClipboardContents {
                        state: state.into(),
                        parts,
                        size,
                        files,
                    }))
                };
                match found {
                    Some(ClipboardRead::Content(content))
                        if content.len() as u64 > read.max_bytes =>
                    {
                        answers.push(header(
                            ClipboardState::TooLarge,
                            vec![],
                            content.len() as u64,
                        ));
                    }
                    Some(ClipboardRead::Content(content)) => {
                        let parts = content
                            .items
                            .iter()
                            .map(|(format, data)| ClipboardPart {
                                mime: format.mime().to_owned(),
                                size: data.len() as u64,
                            })
                            .collect();
                        answers.push(header(ClipboardState::Content, parts, 0));
                        for (_, data) in &content.items {
                            for piece in data.chunks(1024 * 1024) {
                                answers.push(says(Msg::ClipboardData(ClipboardData {
                                    data: piece.to_vec(),
                                })));
                            }
                        }
                    }
                    Some(ClipboardRead::Sensitive) => {
                        answers.push(header(ClipboardState::Sensitive, vec![], 0));
                    }
                    _ => answers.push(header(ClipboardState::Empty, vec![], 0)),
                }
                desk.lock().unwrap().reads += 1;
            }
            agent_to_broker::Msg::WriteClipboard(write) => {
                let mut content = ClipboardContent::default();
                for part in &write.parts {
                    let mut data = Vec::new();
                    while (data.len() as u64) < part.size {
                        match reader.recv::<AgentToBroker>().await {
                            Ok(Some(AgentToBroker {
                                msg: Some(agent_to_broker::Msg::ClipboardData(piece)),
                            })) => data.extend(piece.data),
                            other => panic!("expected clipboard data, got {other:?}"),
                        }
                    }
                    let format = ClipboardFormat::from_mime(&part.mime).unwrap();
                    content.items.push((format, data));
                }
                desk.lock().unwrap().written.push(content);
            }
            agent_to_broker::Msg::PromiseClipboard(promise) => {
                answers.push(says(Msg::ClipboardPromised(ClipboardPromised {
                    promise_id: promise.promise_id,
                    ok: true,
                })));
                desk.lock().unwrap().promised.push((
                    promise.promise_id,
                    promise.mimes,
                    promise.size,
                ));
            }
            agent_to_broker::Msg::WithdrawPromise(withdrawn) => {
                desk.lock().unwrap().withdrawn.push(withdrawn.promise_id);
            }
            agent_to_broker::Msg::ClipboardRendered(rendered) => {
                let mut data = Vec::new();
                while (data.len() as u64) < rendered.size {
                    match reader.recv::<AgentToBroker>().await {
                        Ok(Some(AgentToBroker {
                            msg: Some(agent_to_broker::Msg::ClipboardData(piece)),
                        })) => data.extend(piece.data),
                        other => panic!("expected clipboard data, got {other:?}"),
                    }
                }
                match pasting.take() {
                    Some((promise_id, mime, reply))
                        if promise_id == rendered.promise_id && mime == rendered.mime =>
                    {
                        let _ = reply.send(rendered.ok.then_some(data));
                    }
                    other => panic!("rendered what nobody pastes, waiting for {other:?}"),
                }
            }
            agent_to_broker::Msg::OpenFile(open) => {
                std::fs::create_dir_all(&open.folder).unwrap();
                let path = std::path::Path::new(&open.folder)
                    .join(format!(".inputweave-{}.part", open.transfer_id));
                let file = std::fs::File::options()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .unwrap();
                let offset = file.metadata().unwrap().len();
                files.insert(open.handle, (path, file, open.folder.clone().into()));
                answers.push(says(Msg::FileOpened(FileOpened {
                    handle: open.handle,
                    offset,
                    free_bytes: u64::MAX,
                })));
            }
            agent_to_broker::Msg::FileData(data) => {
                files
                    .get_mut(&data.handle)
                    .unwrap()
                    .1
                    .write_all(&data.data)
                    .unwrap();
            }
            agent_to_broker::Msg::FinishFile(finish) => {
                let (path, file, _) = &files[&finish.handle];
                file.sync_all().unwrap();
                let bytes = std::fs::read(path).unwrap();
                answers.push(says(Msg::FileFinished(FileFinished {
                    handle: finish.handle,
                    size: bytes.len() as u64,
                    blake3: blake3::hash(&bytes).as_bytes().to_vec(),
                })));
            }
            agent_to_broker::Msg::KeepFile(keep) => {
                let (path, _, folder) = files.remove(&keep.handle).unwrap();
                // As the real broker, in the folders on the way, which it
                // checks are names Windows allows.
                let folder = keep.subfolder.split('/').filter(|s| !s.is_empty()).fold(
                    folder,
                    |folder, part| {
                        assert!(
                            !part.contains(['\\', ':', '?', '*']) && part != "..",
                            "{part:?}"
                        );
                        folder.join(part)
                    },
                );
                std::fs::create_dir_all(&folder).unwrap();
                let (stem, extension) = keep.name.rsplit_once('.').unwrap_or((&keep.name, ""));
                let target = (0..)
                    .map(|n| match (n, extension) {
                        (0, _) => folder.join(&keep.name),
                        (n, "") => folder.join(format!("{stem} ({n})")),
                        (n, extension) => folder.join(format!("{stem} ({n}).{extension}")),
                    })
                    .find(|p| !p.exists())
                    .unwrap();
                std::fs::rename(&path, &target).unwrap();
                answers.push(says(Msg::FileKept(FileKept {
                    handle: keep.handle,
                    path: target.to_string_lossy().into_owned(),
                })));
            }
            agent_to_broker::Msg::DiscardFile(discard) => {
                if let Some((path, _, _)) = files.remove(&discard.handle) {
                    std::fs::remove_file(path).unwrap();
                }
            }
            agent_to_broker::Msg::CloseFile(close) => {
                files.remove(&close.handle);
            }
            agent_to_broker::Msg::OpenForSending(open) => {
                let path = std::path::PathBuf::from(&open.path);
                let mut desk = desk.lock().unwrap();
                let refused = |reason: OpenFailure, error: &str| {
                    says(Msg::OpenFailed(OpenFailed {
                        handle: open.handle,
                        reason: reason.into(),
                        error: error.to_owned(),
                    }))
                };
                answers.push(if desk.denied.contains(&path) {
                    refused(OpenFailure::AccessDenied, "Access is denied. (os error 5)")
                } else if let Some(real) = desk.resolve(&path).filter(|r| r.is_file()) {
                    let size = std::fs::metadata(&real).unwrap().len();
                    sending.insert(open.handle, real.clone());
                    desk.opened.push(open.transfer_id.clone());
                    says(Msg::OpenedForSending(OpenedForSending {
                        handle: open.handle,
                        size,
                    }))
                } else {
                    refused(OpenFailure::NotAFile, "not found")
                });
            }
            agent_to_broker::Msg::HashFile(hash) => {
                let bytes = std::fs::read(&sending[&hash.handle]).unwrap();
                answers.push(says(Msg::FileHashed(FileHashed {
                    handle: hash.handle,
                    size: bytes.len() as u64,
                    blake3: blake3::hash(&bytes).as_bytes().to_vec(),
                })));
            }
            agent_to_broker::Msg::ReadChunk(read) => {
                use std::os::unix::fs::FileExt;

                let file = std::fs::File::open(&sending[&read.handle]).unwrap();
                let mut data = vec![0; (read.len as usize).min(1024 * 1024)];
                let mut filled = 0;
                while filled < data.len() {
                    let n = file
                        .read_at(&mut data[filled..], read.offset + filled as u64)
                        .unwrap();
                    if n == 0 {
                        break;
                    }
                    filled += n;
                }
                data.truncate(filled);
                answers.push(says(Msg::ChunkData(ChunkData {
                    handle: read.handle,
                    offset: read.offset,
                    data,
                })));
            }
            agent_to_broker::Msg::CloseSending(close) => {
                sending.remove(&close.handle);
            }
            agent_to_broker::Msg::LaunchApp(launch) => {
                let refused = {
                    let mut desk = desk.lock().unwrap();
                    desk.launches += 1;
                    desk.launch_reasons.push(launch.reason());
                    desk.launch_refused
                };
                answers.push(says(Msg::AppLaunched(AppLaunched {
                    ok: !refused,
                    error: if refused {
                        "no such file".into()
                    } else {
                        String::new()
                    },
                    started: !refused,
                })));
            }
            agent_to_broker::Msg::ListPath(list) => {
                let path = std::path::PathBuf::from(&list.path);
                let mut desk = desk.lock().unwrap();
                desk.listed.push(path.clone());
                let kind_of = |path: &std::path::Path| match std::fs::symlink_metadata(path) {
                    Ok(m) if m.file_type().is_symlink() => (PathKind::Link, 0),
                    Ok(m) if m.is_dir() => (PathKind::Folder, 0),
                    Ok(m) if m.is_file() => (PathKind::File, m.len()),
                    Ok(_) => (PathKind::Other, 0),
                    Err(_) => (PathKind::Missing, 0),
                };
                let (kind, size, real) = if desk.denied.contains(&path) {
                    (PathKind::Unreadable, 0, None)
                } else if let Some(real) = desk.resolve(&path) {
                    let (kind, size) = kind_of(&real);
                    (kind, size, Some(real))
                } else {
                    (PathKind::Missing, 0, None)
                };
                let mut entries = Vec::new();
                let mut truncated = false;
                if let (PathKind::Folder, Some(real)) = (kind, &real) {
                    for entry in std::fs::read_dir(real).unwrap().flatten() {
                        // No more than asked for, as the broker lists.
                        if entries.len() >= list.max_entries as usize {
                            truncated = true;
                            break;
                        }
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let (kind, size) = if desk.denied.contains(&path.join(&name)) {
                            (PathKind::Unreadable, 0)
                        } else {
                            kind_of(&entry.path())
                        };
                        entries.push(PathEntry {
                            name,
                            kind: kind.into(),
                            size,
                        });
                    }
                }
                let error = if kind == PathKind::Unreadable {
                    "Access is denied. (os error 5)".to_owned()
                } else {
                    String::new()
                };
                answers.push(says(Msg::PathListed(PathListed {
                    handle: list.handle,
                    kind: kind.into(),
                    size,
                    entries,
                    truncated,
                    error,
                })));
            }
            agent_to_broker::Msg::ClaimFolder(claim) => {
                let folder = std::path::Path::new(&claim.folder);
                std::fs::create_dir_all(folder).unwrap();
                let claimed = (0..)
                    .map(|n| match n {
                        0 => folder.join(&claim.name),
                        n => folder.join(format!("{} ({n})", claim.name)),
                    })
                    .find(|p| std::fs::create_dir(p).is_ok())
                    .unwrap();
                answers.push(ready(claim.handle, &claimed));
            }
            agent_to_broker::Msg::MakeFolder(make) => {
                let path = std::path::Path::new(&make.folder).join(&make.subfolder);
                std::fs::create_dir_all(&path).unwrap();
                answers.push(ready(make.handle, &path));
            }
            agent_to_broker::Msg::CheckFolder(check) => {
                let folder = std::path::Path::new(&check.folder);
                answers.push(if folder.is_dir() {
                    ready(check.handle, folder)
                } else {
                    failed(check.handle, "not a folder")
                });
            }
            agent_to_broker::Msg::LoadHistory(_) => {
                let mut desk = desk.lock().unwrap();
                desk.loads += 1;
                let (toml, error) = if desk.history_error.is_empty() {
                    (desk.history.clone(), String::new())
                } else {
                    (Vec::new(), desk.history_error.clone())
                };
                answers.push(says(Msg::HistoryLoaded(HistoryLoaded { toml, error })));
            }
            agent_to_broker::Msg::SaveHistory(save) => {
                let mut desk = desk.lock().unwrap();
                desk.history = save.toml;
                desk.saves += 1;
                answers.push(says(Msg::HistorySaved(HistorySaved {
                    error: String::new(),
                })));
            }
            agent_to_broker::Msg::RequestDesktopPictures(_)
            | agent_to_broker::Msg::ClipboardData(_) => {}
        }
        for answer in &answers {
            writer.send(answer).await.unwrap();
        }
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_windows_pc_shares_its_clipboard_through_the_broker() {
    use iw_platform::{ClipboardContent, ClipboardFormat, ClipboardRead};
    use iw_proto::peer::v1::ClipboardSkip;

    let dir = tempfile::tempdir().unwrap();
    let pipe = dir.path().join("broker.sock");
    // The PC's clipboard is the broker's.
    let pc = Spec {
        clipboard: false,
        ..win()
    };
    let nodes = desk_with(&[mac(), pc], |s| Options {
        broker_socket: (s.id == "win").then(|| pipe.clone()),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let broker = SimBroker::connect(&pipe, &dir.path().join("Downloads")).await;
    let skipped = |reason: ClipboardSkip| {
        move |s: &Status| {
            s.last_clipboard
                .as_ref()
                .is_some_and(|c| c.skipped == Some(reason) && c.from == "win")
        }
    };

    // Copy on the Mac, move to the PC: the broker puts it on the PC's
    // clipboard.
    mac.clipboard.copy(text("from the Mac"), false);
    mac_enters_win(mac, win).await;
    broker.wait_written(&text("from the Mac")).await;

    // Copy on the PC, come back: paste on the Mac.
    let picture = ClipboardContent {
        items: vec![
            (ClipboardFormat::Text, b"from the PC".to_vec()),
            (ClipboardFormat::Png, vec![0x89; 3 * 1024 * 1024]),
        ],
    };
    broker.copy(ClipboardRead::Content(picture.clone())).await;
    take_back(mac);
    wait_clipboard(mac, &picture).await;

    // A password copied on the PC stays there, and the Mac says why.
    mac_enters_win(mac, win).await;
    broker.copy(ClipboardRead::Sensitive).await;
    take_back(mac);
    wait_status(mac, skipped(ClipboardSkip::Sensitive)).await;

    // So does a clipboard over what the broker reads at once.
    mac_enters_win(mac, win).await;
    let huge = ClipboardContent {
        items: vec![(ClipboardFormat::Png, vec![1; 65 * 1024 * 1024])],
    };
    broker.copy(ClipboardRead::Content(huge)).await;
    take_back(mac);
    wait_status(mac, skipped(ClipboardSkip::TooLarge)).await;
    assert_eq!(mac.clipboard.content(), picture);
    // What the Mac sent was written once; nothing else was.
    assert_eq!(
        broker.desk.lock().unwrap().written,
        vec![text("from the Mac")]
    );

    broker.quit().await;
    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_windows_pc_promises_large_clipboards_through_the_broker() {
    use iw_platform::{ClipboardContent, ClipboardFormat};
    use iw_proto::peer::v1::ClipboardSkip;

    let dir = tempfile::tempdir().unwrap();
    let pipe = dir.path().join("broker.sock");
    let pc = Spec {
        clipboard: false,
        ..win()
    };
    let nodes = desk_with(&[mac(), pc], |s| Options {
        broker_socket: (s.id == "win").then(|| pipe.clone()),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let broker = SimBroker::connect(&pipe, &dir.path().join("Downloads")).await;
    let text_mime = ClipboardFormat::Text.mime();

    // Copied on the Mac, too large to send: the broker promises it on the
    // PC's clipboard, and nothing has moved yet.
    let big = large("from the Mac");
    mac.clipboard.copy(big.clone(), false);
    mac_enters_win(mac, win).await;
    let (promise, mimes, size) = broker.wait_promised().await;
    assert_eq!(mimes, vec![text_mime.to_owned()]);
    assert_eq!(size, big.len() as u64);
    let status = wait_status(win, |s| s.lazy_clipboard.received == 1).await;
    assert_eq!(status.lazy_clipboard.bytes_pulled, 0);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(mac.agent.status().lazy_clipboard.served, 0);
    assert!(broker.desk.lock().unwrap().written.is_empty());

    // An app on the PC pastes it: only now does it come over, whole.
    let pasted = broker.paste(promise, text_mime).await;
    assert_eq!(pasted.as_deref(), big.get(ClipboardFormat::Text));
    wait_status(win, |s| s.lazy_clipboard.pulled == 1).await;
    wait_status(mac, |s| s.lazy_clipboard.served == 1).await;
    // Pasted again, or a format it never had: nothing more is pulled.
    assert_eq!(broker.paste(promise, text_mime).await, pasted);
    assert_eq!(
        broker.paste(promise, ClipboardFormat::Png.mime()).await,
        None
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(mac.agent.status().lazy_clipboard.served, 1);

    // Copied over on the PC: the promise is gone, and nothing is
    // rendered for it any more.
    broker.copy_over(promise);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(broker.paste(promise, text_mime).await, None);

    // More than the broker takes at once is not promised at all.
    take_back(mac);
    wait_status(mac, |s| s.cursor_on == "mac").await;
    let huge = ClipboardContent {
        items: vec![(ClipboardFormat::Text, vec![b'.'; 65 * 1024 * 1024])],
    };
    mac.clipboard.copy(huge, false);
    mac_enters_win(mac, win).await;
    wait_status(win, |s| {
        s.last_clipboard
            .as_ref()
            .is_some_and(|c| c.skipped == Some(ClipboardSkip::TooLarge))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(broker.desk.lock().unwrap().promised.len(), 1);
    assert_eq!(mac.agent.status().lazy_clipboard.served, 1);
    // Nothing is ever pulled at once to be written whole.
    assert!(broker.desk.lock().unwrap().written.is_empty());

    broker.quit().await;
    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_windows_pc_offers_its_large_clipboard_through_the_broker() {
    use iw_platform::{ClipboardContent, ClipboardFormat, ClipboardRead};

    let dir = tempfile::tempdir().unwrap();
    let pipe = dir.path().join("broker.sock");
    let pc = Spec {
        clipboard: false,
        ..win()
    };
    let nodes = desk_with(&[mac(), pc], |s| Options {
        broker_socket: (s.id == "win").then(|| pipe.clone()),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let broker = SimBroker::connect(&pipe, &dir.path().join("Downloads")).await;

    // Copied on the PC, larger than the agent reads ahead: offered as the
    // cursor comes back, and pulled from the broker only when the Mac
    // pastes.
    mac_enters_win(mac, win).await;
    let big = ClipboardContent {
        items: vec![(ClipboardFormat::Png, vec![7; 20 * 1024 * 1024])],
    };
    broker.copy(ClipboardRead::Content(big.clone())).await;
    let reads = broker.desk.lock().unwrap().reads;
    take_back(mac);
    wait_promise(mac).await;
    assert_eq!(mac.clipboard.promised(), Some(vec![ClipboardFormat::Png]));
    let offered = wait_status(win, |s| s.lazy_clipboard.offered == 1).await;
    assert_eq!(offered.lazy_clipboard.served, 0);
    // Read once more, whole, to offer it.
    assert_eq!(broker.desk.lock().unwrap().reads, reads + 1);

    assert_eq!(paste(mac).await, big);
    wait_status(win, |s| s.lazy_clipboard.served == 1).await;
    wait_status(mac, |s| s.lazy_clipboard.pulled == 1).await;
    // Served from what was read to offer it: not read again.
    assert_eq!(broker.desk.lock().unwrap().reads, reads + 1);

    broker.quit().await;
    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_windows_pc_receives_files_through_the_broker() {
    use inputweave::agent::TransferState;
    use iw_proto::peer::v1::FileDecline;

    let dir = tempfile::tempdir().unwrap();
    let pipe = dir.path().join("broker.sock");
    let downloads = dir.path().join("Downloads").join("InputWeave");
    let mac = start_receiving(
        &mac(),
        dir.path(),
        "mac",
        dir.path().join("mac-received"),
        None,
    )
    .await;
    let identity = Identity::load_or_generate(&dir.path().join("win-identity")).unwrap();
    let options = Options {
        devices_file: Some(dir.path().join("win-devices.toml")),
        broker_socket: Some(pipe.clone()),
        ..Options::default()
    };
    let win = start_with_listen(&win(), identity, options, None).await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    connected(&win, "mac").await;
    let folder_is = |expected: Option<std::path::PathBuf>| {
        let win = &win;
        async move {
            let deadline = tokio::time::Instant::now() + TIMEOUT;
            while win.agent.file_settings().folder != expected {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the folder is never {expected:?}"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    };

    let source = dir.path().join("報告 v2.pdf");
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&source, &data).unwrap();

    // Nobody is logged on to the PC to write the file.
    assert_eq!(win.agent.file_settings().folder, None);
    let refused = mac
        .agent
        .send_files("win", vec![source.clone()])
        .await
        .unwrap();
    assert_eq!(
        wait_transfer(&mac, &refused[0], finished).await.state,
        TransferState::Declined(FileDecline::Unavailable)
    );

    let broker = SimBroker::connect(&pipe, &downloads).await;
    folder_is(Some(downloads.clone())).await;
    for expected in ["報告 v2.pdf", "報告 v2 (1).pdf"] {
        let ids = mac
            .agent
            .send_files("win", vec![source.clone()])
            .await
            .unwrap();
        let sent = wait_transfer(&mac, &ids[0], finished).await;
        assert_eq!(sent.state, TransferState::Done, "{sent:?}");
        let received = wait_transfer(&win, &ids[0], finished).await;
        assert_eq!(received.state, TransferState::Done);
        assert_eq!(received.path.unwrap(), downloads.join(expected));
        let written = std::fs::read(downloads.join(expected)).unwrap();
        assert_eq!(blake3::hash(&written), blake3::hash(&data));
    }
    // A name Windows does not allow is changed to one it does.
    let odd = dir.path().join("why?.txt");
    std::fs::write(&odd, "?").unwrap();
    let ids = mac.agent.send_files("win", vec![odd]).await.unwrap();
    let received = wait_transfer(&win, &ids[0], finished).await;
    assert_eq!(received.path.unwrap(), downloads.join("why_.txt"));
    let leftovers = std::fs::read_dir(&downloads)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
        .count();
    assert_eq!(leftovers, 0);

    // The user logs off: files are turned away again.
    broker.quit().await;
    folder_is(None).await;
    let refused = mac.agent.send_files("win", vec![source]).await.unwrap();
    assert_eq!(
        wait_transfer(&mac, &refused[0], finished).await.state,
        TransferState::Declined(FileDecline::Unavailable)
    );

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

/// A transfer history file holding one transfer that finished, as the
/// agent writes them.
#[cfg(unix)]
fn history_holding(id: &str, name: &str) -> Vec<u8> {
    format!(
        "[[transfers]]\nid = \"{id}\"\ndirection = \"receive\"\ndevice = \"mac\"\n\
         name = \"{name}\"\nsize = 5\nstate = \"done\"\n\
         started_unix_ms = 1761100000000\nfinished_unix_ms = 1761100002000\n"
    )
    .into_bytes()
}

/// Waits until `node` lists the transfers `ids`, in that order.
#[cfg(unix)]
async fn wait_listed(node: &Node, ids: &[&str]) {
    let mut rx = node.agent.watch_transfers();
    let listed = |list: &Vec<inputweave::agent::TransferStatus>| {
        list.iter().map(|t| t.id.as_str()).eq(ids.iter().copied())
    };
    tokio::time::timeout(TIMEOUT, rx.wait_for(listed))
        .await
        .unwrap_or_else(|_| panic!("not {ids:?}; transfers: {:#?}", node.agent.transfers()))
        .unwrap();
}

/// Waits until the user of `broker` has transfer `id` in their history
/// file.
#[cfg(unix)]
async fn wait_saved(broker: &SimBroker, id: &str) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let history = broker.desk.lock().unwrap().history.clone();
        if String::from_utf8_lossy(&history).contains(id) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{id} was never saved: {}",
            String::from_utf8_lossy(&history)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_windows_pc_keeps_each_users_transfer_history_through_the_broker() {
    use inputweave::agent::{FileSettings, TransferState};

    let dir = tempfile::tempdir().unwrap();
    let pipe = dir.path().join("broker.sock");
    let downloads = dir.path().join("Downloads").join("InputWeave");
    let mac_folder = dir.path().join("mac-received");
    let mac = start_receiving(&mac(), dir.path(), "mac", mac_folder, None).await;
    // An earlier agent kept the history itself, as SYSTEM.
    let left_behind = dir.path().join("win-transfers.toml");
    std::fs::write(&left_behind, history_holding("0a-0-0", "old.txt")).unwrap();
    let identity = Identity::load_or_generate(&dir.path().join("win-identity")).unwrap();
    let options = Options {
        devices_file: Some(dir.path().join("win-devices.toml")),
        transfers_file: Some(left_behind.clone()),
        broker_socket: Some(pipe.clone()),
        ..Options::default()
    };
    let win = start_with_listen(&win(), identity, options, None).await;
    // Gone, and nothing is listed while nobody is logged on.
    assert!(!left_behind.exists());
    assert!(win.agent.transfers().is_empty());
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    connected(&win, "mac").await;
    let note = dir.path().join("note.txt");
    std::fs::write(&note, "hello").unwrap();

    // A user logs on: their history is listed once their broker loads it.
    let first = SimBroker::connect_keeping(
        &pipe,
        &downloads,
        history_holding("1a-1-1", "first user's.txt"),
    )
    .await;
    wait_listed(&win, &["1a-1-1"]).await;

    // A file arrives and goes into their history.
    let done = mac
        .agent
        .send_files("win", vec![note.clone()])
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        wait_transfer(&win, &done, finished).await.state,
        TransferState::Done
    );
    wait_saved(&first, &done).await;

    // They are asked about another file, and log off without answering:
    // their finished transfers leave the list, the one going stays.
    win.agent.set_file_settings(FileSettings {
        folder: None,
        ask: true,
    });
    let asked = mac
        .agent
        .send_files("win", vec![note.clone()])
        .await
        .unwrap()
        .remove(0);
    wait_transfer(&win, &asked, |t| t.state == TransferState::WaitingForAnswer).await;
    let first_desk = first.desk.clone();
    first.quit().await;
    wait_listed(&win, &[&asked]).await;

    // It ends while nobody is logged on: listed, but saved nowhere.
    assert!(mac.agent.cancel_transfer(&asked));
    assert_eq!(
        wait_transfer(&win, &asked, finished).await.state,
        TransferState::Cancelled
    );

    // The next user logs on: their history takes its place.
    let next = SimBroker::connect_keeping(
        &pipe,
        &downloads,
        history_holding("2b-2-2", "next user's.txt"),
    )
    .await;
    wait_listed(&win, &["2b-2-2"]).await;
    win.agent.set_file_settings(FileSettings {
        folder: None,
        ask: false,
    });
    let theirs = mac
        .agent
        .send_files("win", vec![note.clone()])
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        wait_transfer(&win, &theirs, finished).await.state,
        TransferState::Done
    );
    wait_saved(&next, &theirs).await;
    wait_listed(&win, &[&theirs, "2b-2-2"]).await;

    // Nothing of the first user's, nor the transfer that ended in between,
    // is in the next user's file; the first user's file has only theirs.
    let next_file = String::from_utf8(next.desk.lock().unwrap().history.clone()).unwrap();
    for other in ["0a-0-0", "1a-1-1", &done, &asked] {
        assert!(!next_file.contains(other), "{other} in {next_file}");
    }
    let first_file = String::from_utf8(first_desk.lock().unwrap().history.clone()).unwrap();
    assert!(first_file.contains("1a-1-1") && first_file.contains(&done));
    for other in ["0a-0-0", &asked, &theirs] {
        assert!(!first_file.contains(other), "{other} in {first_file}");
    }

    next.quit().await;

    // A broker that cannot read its user's file: nothing is listed, and a
    // transfer finishing now stays in memory rather than being written over
    // the file that could not be read.
    let unread = SimBroker::connect_with_history(
        &pipe,
        &downloads,
        history_holding("3c-3-3", "unread user's.txt"),
        "locked by another program".to_owned(),
    )
    .await;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while unread.desk.lock().unwrap().loads == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the history was never asked for"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    wait_listed(&win, &[]).await;
    let unsaved = mac
        .agent
        .send_files("win", vec![note])
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        wait_transfer(&win, &unsaved, finished).await.state,
        TransferState::Done
    );
    wait_listed(&win, &[&unsaved]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (saves, file) = {
        let desk = unread.desk.lock().unwrap();
        (
            desk.saves,
            String::from_utf8_lossy(&desk.history).into_owned(),
        )
    };
    assert_eq!(saves, 0, "saved over a file the broker could not read");
    assert!(file.contains("3c-3-3"));

    unread.quit().await;
    mac.agent.shutdown().await;
    win.agent.shutdown().await;
    // The agent never kept a history file of its own again.
    assert!(!left_behind.exists());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_windows_pc_sends_files_the_broker_reads_as_the_user() {
    use inputweave::agent::{SendError, TransferState};

    let dir = tempfile::tempdir().unwrap();
    let pipe = dir.path().join("broker.sock");
    let mac_folder = dir.path().join("mac-received");
    let mac = start_receiving(&mac(), dir.path(), "mac", mac_folder.clone(), None).await;
    let identity = Identity::load_or_generate(&dir.path().join("win-identity")).unwrap();
    let options = Options {
        devices_file: Some(dir.path().join("win-devices.toml")),
        broker_socket: Some(pipe.clone()),
        ..Options::default()
    };
    let win = start_with_listen(&win(), identity, options, None).await;
    pair(&win, &mac).await;
    connected(&win, "mac").await;
    connected(&mac, "win").await;

    // The user's files are only where the broker looks: the agent, as
    // SYSTEM, opens none of them.
    let user = dir.path().join("C:").join("Users").join("pc");
    let report = user.join("報告 v3.pdf");
    let real = dir.path().join("real.bin");
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(&real, &data).unwrap();
    assert!(!report.exists());

    // Nobody is logged on to read it.
    match win.agent.send_files("mac", vec![report.clone()]).await {
        Err(SendError::NoBroker) => {}
        other => panic!("expected no broker, got {other:?}"),
    }
    assert!(win.agent.transfers().is_empty());

    let broker = SimBroker::connect(&pipe, &dir.path().join("Downloads")).await;
    broker.share(&report, &real);
    // Linked once the agent takes the folder the broker names.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while win.agent.file_settings().folder.is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the broker never linked"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let ids = win
        .agent
        .send_files("mac", vec![report.clone()])
        .await
        .unwrap();
    let sent = wait_transfer(&win, &ids[0], finished).await;
    assert_eq!(sent.state, TransferState::Done, "{sent:?}");
    assert_eq!(sent.size, data.len() as u64);
    let received = wait_transfer(&mac, &ids[0], finished).await;
    assert_eq!(received.state, TransferState::Done);
    let arrived = std::fs::read(mac_folder.join("報告 v3.pdf")).unwrap();
    assert_eq!(blake3::hash(&arrived), blake3::hash(&data));
    // Listed by the broker when asked, then opened to send.
    assert_eq!(broker.desk.lock().unwrap().opened, vec![ids[0].clone()]);

    // A file the user may not read fails on its own; the rest goes.
    let secret = user.join("secret.txt");
    broker.share(&secret, &real);
    broker.desk.lock().unwrap().denied.insert(secret.clone());
    let both = win
        .agent
        .send_files("mac", vec![report.clone(), secret.clone()])
        .await
        .unwrap();
    let refused = wait_transfer(&win, &both[1], finished).await;
    assert_eq!(refused.state, TransferState::Failed("unreadable"));
    assert_eq!(refused.path.as_deref(), Some(secret.as_path()));
    let sent = wait_transfer(&win, &both[0], finished).await;
    assert_eq!(sent.state, TransferState::Done);
    assert!(mac_folder.join("報告 v3 (1).pdf").exists());
    // Alone, it is refused, with the system's reason.
    match win.agent.send_files("mac", vec![secret.clone()]).await {
        Err(SendError::Unreadable { path, reason }) => {
            assert_eq!(path, secret);
            assert!(reason.contains("denied"), "{reason}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    match win
        .agent
        .send_files("mac", vec![user.join("gone.txt")])
        .await
    {
        Err(SendError::NotAFile(_)) => {}
        other => panic!("expected not a file, got {other:?}"),
    }
    assert_eq!(win.agent.transfers().len(), 3);

    broker.quit().await;
    win.agent.shutdown().await;
    mac.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_broker_opens_the_app_to_show_a_pairing_code() {
    let dir = tempfile::tempdir().unwrap();
    let pipe = dir.path().join("broker.sock");
    // No app is connected to the PC's agent.
    let options = Options {
        broker_socket: Some(pipe.clone()),
        open_app: true,
        ..Options::default()
    };
    let win = start_showing(
        &win(),
        Identity::generate().unwrap(),
        &[],
        &[],
        options,
        MockDesktopPictures::default(),
    )
    .await;
    let mac = unpaired(&mac(), Identity::generate().unwrap(), None).await;
    let broker = SimBroker::connect(&pipe, &dir.path().join("Downloads")).await;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while win.agent.file_settings().folder.is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the broker never linked"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let launches = || broker.desk.lock().unwrap().launches;
    let until = |count: usize| async move {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while launches() != count {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the app was started {} times, not {count}",
                launches()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };

    mac.agent
        .start_pairing(win.agent.local_addr())
        .await
        .unwrap();
    shown_code(&win).await;
    until(1).await;
    win.agent.cancel_pairing();
    wait_pairing(&mac, |s| matches!(s, PairingStatus::Failed { .. })).await;

    // Asked again at once: the app was just opened.
    mac.agent
        .start_pairing(win.agent.local_addr())
        .await
        .unwrap();
    shown_code(&win).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(launches(), 1);
    // To show the code: not refused after the user quit the app.
    assert_eq!(
        broker.desk.lock().unwrap().launch_reasons,
        [iw_proto::service::v1::LaunchReason::AskUser]
    );
    win.agent.cancel_pairing();

    broker.quit().await;
    win.agent.shutdown().await;
    mac.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn files_turned_off_with_a_device_go_neither_way() {
    use inputweave::agent::{SendError, TransferState};
    use iw_proto::peer::v1::FileDecline;

    let dir = tempfile::tempdir().unwrap();
    let (mac, win, _, win_folder) = file_pair(dir.path()).await;
    let source = dir.path().join("notes.txt");
    std::fs::write(&source, "hello").unwrap();

    // Turned off on the Mac: sending to the PC is refused, and nothing is
    // listed.
    mac.agent.set_device_sharing("win", false, true).unwrap();
    wait_status(&mac, |s| s.device("win").is_some_and(|d| d.files_off)).await;
    let listed = mac.agent.transfers().len();
    let refused = mac.agent.send_files("win", vec![source.clone()]).await;
    assert!(
        matches!(&refused, Err(e @ SendError::FilesOff(_)) if e.code() == "files_off"),
        "{refused:?}"
    );
    assert_eq!(mac.agent.transfers().len(), listed);
    // The PC, which knows nothing of it, sees its files declined as by a
    // computer that does not receive files.
    assert!(!win.agent.status().device("mac").unwrap().files_off);
    let ids = win
        .agent
        .send_files("mac", vec![source.clone()])
        .await
        .unwrap();
    let sent = wait_transfer(&win, &ids[0], finished).await;
    assert_eq!(
        sent.state,
        TransferState::Declined(FileDecline::Unavailable)
    );
    assert_eq!(sent.state.reason(), "unavailable");

    // On again, files go.
    mac.agent.set_device_sharing("win", false, false).unwrap();
    wait_status(&mac, |s| s.device("win").is_some_and(|d| !d.files_off)).await;
    let ids = mac
        .agent
        .send_files("win", vec![source.clone()])
        .await
        .unwrap();
    assert_eq!(
        wait_transfer(&mac, &ids[0], finished).await.state,
        TransferState::Done
    );
    assert_eq!(
        std::fs::read(win_folder.join("notes.txt")).unwrap(),
        b"hello"
    );

    // Turned off on the PC instead: the Mac's files are declined there.
    win.agent.set_device_sharing("mac", false, true).unwrap();
    wait_status(&win, |s| s.device("mac").is_some_and(|d| d.files_off)).await;
    let ids = mac.agent.send_files("win", vec![source]).await.unwrap();
    let sent = wait_transfer(&mac, &ids[0], finished).await;
    assert_eq!(
        sent.state,
        TransferState::Declined(FileDecline::Unavailable)
    );
    assert!(!win_folder.join("notes (1).txt").exists());

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

/// A device that takes no part in dragging files (Linux) declines a drag
/// at once, though its app is there: the app never hears of it.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_takes_no_drags_declines_them_at_once() {
    use inputweave::agent::DragEnd;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("win.sock");
    let nodes = desk_with(&[mac(), win()], |s| Options {
        ipc_socket: (s.id == "win").then(|| socket.clone()),
        refuses_drags: s.id == "win",
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let _app = App::connect(&socket).await;
    let mut drags = mac.agent.watch_drags();
    let mut offered = win.agent.watch_drags();
    let src = dir.path().join("src");
    tree(&src, &[("notes.txt", b"hi")], &[]);

    let transfers = mac.agent.transfers().len();
    let id = start_dragging(mac, vec![src.join("notes.txt")]).await;
    mac_enters_win(mac, win).await;
    wait_injected(win, Emulated::Button(Button::Left, true)).await;
    drop_on(mac, win).await;
    let ended = drag_ended(&mut drags).await;
    assert_eq!(ended.id, id);
    // Released before the PC's answer came, or declined by it.
    assert!(
        matches!(ended.reason, DragEnd::Released | DragEnd::Declined),
        "{ended:?}"
    );
    assert!(
        offered.try_recv().is_err(),
        "the PC's app heard of the drag"
    );
    assert_eq!(mac.agent.transfers().len(), transfers, "something was sent");

    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// A controller that takes no part in dragging files (Linux) carries a
/// drag from the device it controls, but does not offer it to its own app
/// when the cursor comes back: released there, the drag ends.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_controller_that_takes_no_drags_does_not_offer_one_carried_back() {
    use inputweave::agent::DragEnd;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mac.sock");
    let nodes = desk_with(&[mac(), win()], |s| Options {
        ipc_socket: (s.id == "mac").then(|| socket.clone()),
        refuses_drags: s.id == "mac",
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let mut app = App::connect(&socket).await;
    let mut drags = win.agent.watch_drags();

    mac_enters_win(mac, win).await;
    let id = start_dragging_on(mac, win, files_to_drag(dir.path())).await;
    push_until_on(mac, (-10.0, 0.0), "mac").await;
    wait_injected(win, Emulated::Button(Button::Left, false)).await;
    no_drag_event(&mut app).await;

    button(mac, false);
    let ended = drag_ended(&mut drags).await;
    assert_eq!(ended.id, id);
    assert_ne!(ended.reason, DragEnd::Accepted, "{ended:?}");
    no_drag_event(&mut app).await;
    assert!(mac.agent.transfers().is_empty(), "something was sent");

    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// What kind of session the platform says this is, whether the cursor
/// leaves only across edges, and what the firewall does reach the apps,
/// `ctl status` and diagnostics.
#[tokio::test(flavor = "multi_thread")]
async fn the_session_its_backends_and_the_firewall_are_told() {
    use iw_platform::{Backend, BackendRole, Blocked, Firewall, SessionInfo, SessionType};
    use iw_proto::app::v1 as app;

    let (capture, handle) = MockCapture::new();
    let platform = MockStatus::granted();
    let agent = lone_mac(capture, &platform).await;

    // Nothing said: nothing told.
    let status = agent.status().to_proto();
    assert_eq!(status.session, None);
    assert!(!status.cursor_leaves_at_edges_only);
    assert_eq!(status.firewall, None);

    platform.set_session_info(Some(SessionInfo {
        kind: SessionType::Wayland,
        desktop: "ubuntu:GNOME".to_owned(),
        backends: vec![
            Backend::held_back(
                BackendRole::Input,
                "RemoteDesktop portal",
                "remote control missing (being asked)",
            ),
            Backend::serving(BackendRole::Clipboard, "Clipboard portal"),
            Backend::missing(BackendRole::DesktopPictures, "not read on this desktop"),
        ],
    }));
    let blocked = Firewall::Blocks(Blocked {
        port: true,
        mdns: true,
    });
    platform.set_firewall(blocked);
    handle.set_edge_only(true);
    let status = wait_status_of(&agent, |s| s.session.is_some() && s.firewall.0 == blocked)
        .await
        .to_proto();
    let session = status.session.clone().unwrap();
    assert_eq!(session.r#type(), app::SessionType::Wayland);
    assert_eq!(session.desktop, "ubuntu:GNOME");
    assert_eq!(
        session
            .backends
            .iter()
            .map(|b| (b.role(), b.implementation.as_str(), b.problem.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                app::BackendRole::Input,
                "RemoteDesktop portal",
                "remote control missing (being asked)"
            ),
            (app::BackendRole::Clipboard, "Clipboard portal", ""),
            (
                app::BackendRole::DesktopPictures,
                "",
                "not read on this desktop"
            ),
        ]
    );
    assert!(status.cursor_leaves_at_edges_only);
    let firewall = status.firewall.clone().unwrap();
    assert_eq!(firewall.state(), app::FirewallState::Blocks);
    // The port the agent took connections on.
    let port = agent.local_addr().port();
    assert_eq!(firewall.port, u32::from(port));
    assert_eq!(firewall.blocked_ports, [u32::from(port), 5353]);
    assert_eq!(platform.firewall_port(), Some(port));

    let listed = inputweave::ctl::format_status(&status);
    assert!(
        listed.contains("session:        Wayland (ubuntu:GNOME)"),
        "{listed}"
    );
    assert!(
        listed.contains(
            "input from other devices: RemoteDesktop portal (remote control missing (being asked))"
        ),
        "{listed}"
    );
    assert!(
        listed.contains("desktop pictures: none (not read on this desktop)"),
        "{listed}"
    );
    assert!(listed.contains("only across the edges"), "{listed}");
    assert!(listed.contains("ufw blocks UDP"), "{listed}");
    assert!(listed.contains("sudo ufw allow"), "{listed}");
    let report = agent.diagnostics().await;
    assert!(
        report.contains("Session: Wayland (ubuntu:GNOME)"),
        "{report}"
    );
    assert!(report.contains("  clipboard: Clipboard portal"), "{report}");
    assert!(
        report.contains("Leaving this device: only across"),
        "{report}"
    );
    assert!(report.contains("Firewall: ufw blocks UDP"), "{report}");

    // A firewall that lets it in, one that cannot be read.
    platform.set_firewall(Firewall::OnUnreadable);
    let status = wait_status_of(&agent, |s| s.firewall.0 == Firewall::OnUnreadable)
        .await
        .to_proto();
    let listed = inputweave::ctl::format_status(&status);
    assert!(listed.contains("rules unreadable"), "{listed}");
    platform.set_firewall(Firewall::Unknown);
    let status = wait_status_of(&agent, |s| s.firewall.0 == Firewall::Unknown)
        .await
        .to_proto();
    assert_eq!(status.firewall, None);

    agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn drags_are_not_offered_to_or_taken_from_a_device_files_are_off_with() {
    use inputweave::agent::DragEnd;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("win.sock");
    let nodes = desk_with(&[mac(), win()], |s| Options {
        ipc_socket: (s.id == "win").then(|| socket.clone()),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    // An app is there to take drags; files being off is what stops them.
    let _app = App::connect(&socket).await;
    let mut drags = mac.agent.watch_drags();
    let mut offered = win.agent.watch_drags();
    let src = dir.path().join("src");
    tree(&src, &[("notes.txt", b"hi")], &[]);
    let dragged = vec![src.join("notes.txt")];

    // Turned off on the PC: its app never sees the drag, and released
    // there it ends with nothing sent.
    win.agent.set_device_sharing("mac", false, true).unwrap();
    wait_status(win, |s| s.device("mac").is_some_and(|d| d.files_off)).await;
    let transfers = mac.agent.transfers().len();
    let id = start_dragging(mac, dragged.clone()).await;
    mac_enters_win(mac, win).await;
    wait_injected(win, Emulated::Button(Button::Left, true)).await;
    drop_on(mac, win).await;
    let ended = drag_ended(&mut drags).await;
    assert_eq!(ended.id, id);
    // Released before the PC's answer came, or declined by it.
    assert!(
        matches!(ended.reason, DragEnd::Released | DragEnd::Declined),
        "{ended:?}"
    );
    assert!(
        offered.try_recv().is_err(),
        "the PC's app heard of the drag"
    );

    // Turned off on the Mac: the drag is not even offered.
    win.agent.set_device_sharing("mac", false, false).unwrap();
    mac.agent.set_device_sharing("win", false, true).unwrap();
    wait_status(mac, |s| s.device("win").is_some_and(|d| d.files_off)).await;
    take_back(mac);
    let id = start_dragging(mac, dragged).await;
    mac_enters_win(mac, win).await;
    assert_eq!(
        mac.agent.status().dragging.and_then(|d| d.device),
        None,
        "offered to the PC"
    );
    drop_on(mac, win).await;
    let ended = drag_ended(&mut drags).await;
    assert_eq!(
        (ended.id.as_str(), ended.reason),
        (id.as_str(), DragEnd::Released)
    );
    assert!(
        offered.try_recv().is_err(),
        "the PC's app heard of the drag"
    );
    assert_eq!(mac.agent.transfers().len(), transfers);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// Writes `files` (paths relative to `root`, with their contents) and
/// creates `folders` under `root`.
fn tree(root: &std::path::Path, files: &[(&str, &[u8])], folders: &[&str]) {
    for (path, data) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }
    for folder in folders {
        std::fs::create_dir_all(root.join(folder)).unwrap();
    }
}

/// Everything under `root`, as "path" for files and "path/" for folders
/// (parts joined with "/" on every system), sorted.
fn contents(root: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if entry.file_type().unwrap().is_dir() {
                found.push(format!("{relative}/"));
                stack.push(path);
            } else {
                found.push(relative);
            }
        }
    }
    found.sort();
    found
}

/// The items of batch `batch` on `node`, in order, once each has
/// finished.
async fn batch_finished(node: &Node, batch: &str) -> Vec<inputweave::agent::TransferStatus> {
    let in_batch =
        |t: &&inputweave::agent::TransferStatus| t.batch.as_ref().is_some_and(|b| b.id == batch);
    let mut rx = node.agent.watch_transfers();
    let list = tokio::time::timeout(
        TIMEOUT * 3,
        rx.wait_for(|list| {
            let mine: Vec<_> = list.iter().filter(in_batch).collect();
            !mine.is_empty()
                && mine.iter().all(|t| t.state.finished())
                && mine.len() as u32 == mine[0].batch.as_ref().unwrap().items
        }),
    )
    .await
    .unwrap_or_else(|_| panic!("timed out; transfers: {:#?}", node.agent.transfers()))
    .unwrap()
    .clone();
    let mut mine: Vec<_> = list.iter().filter(in_batch).cloned().collect();
    mine.sort_by_key(|t| t.batch.as_ref().unwrap().index);
    mine
}

#[tokio::test(flavor = "multi_thread")]
async fn folders_arrive_whole_and_a_name_taken_is_left_alone() {
    use inputweave::agent::TransferState;

    let dir = tempfile::tempdir().unwrap();
    let (mac, win, _, win_folder) = file_pair(dir.path()).await;
    let src = dir.path().join("src");
    let photo: Vec<u8> = (0..300_000u32).map(|i| (i % 241) as u8).collect();
    tree(
        &src,
        &[
            ("Photos/a.jpg", &photo),
            ("Photos/2024/b.jpg", b"bee"),
            ("Photos/2024/deep/c.txt", b"sea"),
            ("notes.txt", b"hello"),
        ],
        &["Photos/empty", "Photos/2024/also empty"],
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(&src, src.join("Photos").join("link")).unwrap();
    // "Photos" is taken there already.
    tree(&win_folder, &[("Photos/old.txt", b"old")], &[]);

    let ids = mac
        .agent
        .send_files("win", vec![src.join("Photos"), src.join("notes.txt")])
        .await
        .unwrap();
    assert_eq!(ids.len(), 2);
    let batch = mac
        .agent
        .transfers()
        .into_iter()
        .find(|t| t.id == ids[0])
        .unwrap()
        .batch
        .unwrap();
    assert_eq!((batch.items, batch.files), (2, 6));

    let sent = batch_finished(&mac, &batch.id).await;
    assert_eq!(sent[0].state, TransferState::Done, "{sent:#?}");
    let progress = sent[0].folder.clone().unwrap();
    let links = u32::from(cfg!(unix));
    assert_eq!(progress.entries, 5 + links);
    assert_eq!(progress.done, 5);
    assert_eq!(progress.failed, links);
    if cfg!(unix) {
        assert_eq!(progress.failures[0].path, "Photos/link");
        assert_eq!(progress.failures[0].reason, "symlink");
    }
    assert_eq!(sent[1].state, TransferState::Done);

    let received = batch_finished(&win, &batch.id).await;
    assert_eq!(received[0].id, ids[0]);
    assert_eq!(received[0].state, TransferState::Done);
    assert_eq!(
        received[0].path.as_deref(),
        Some(win_folder.join("Photos (1)").as_path())
    );
    let progress = received[0].folder.clone().unwrap();
    assert_eq!(
        (progress.entries, progress.done, progress.failed),
        (5, 5, 0)
    );
    assert_eq!(received[0].size, photo.len() as u64 + 6);
    assert_eq!(
        received[1].path.as_deref(),
        Some(win_folder.join("notes.txt").as_path())
    );
    assert_eq!(
        contents(&win_folder),
        [
            "Photos (1)/",
            "Photos (1)/2024/",
            "Photos (1)/2024/also empty/",
            "Photos (1)/2024/b.jpg",
            "Photos (1)/2024/deep/",
            "Photos (1)/2024/deep/c.txt",
            "Photos (1)/a.jpg",
            "Photos (1)/empty/",
            "Photos/",
            "Photos/old.txt",
            "notes.txt",
        ]
    );
    assert_eq!(
        std::fs::read(win_folder.join("Photos (1)").join("a.jpg")).unwrap(),
        photo
    );
    assert_eq!(
        std::fs::read(win_folder.join("Photos").join("old.txt")).unwrap(),
        b"old"
    );

    // The history keeps a folder as one entry, with its count.
    mac.agent.shutdown().await;
    win.agent.shutdown().await;
    let again = start_receiving(&self::win(), dir.path(), "win", win_folder.clone(), None).await;
    let kept = again.agent.transfers();
    let folder = kept.iter().find(|t| t.id == ids[0]).unwrap();
    assert_eq!(folder.folder.as_ref().unwrap().done, 5);
    assert_eq!(folder.batch.as_ref().unwrap().id, batch.id);
    again.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_cannot_be_read_fails_alone_and_the_rest_arrives() {
    use std::os::unix::fs::PermissionsExt;

    use inputweave::agent::TransferState;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mac.sock");
    let mac_folder = dir.path().join("mac-received");
    let win_folder = dir.path().join("win-received");
    let mac = start_kept(
        &mac(),
        dir.path(),
        "mac",
        Options {
            ipc_socket: Some(socket.clone()),
            ..kept_in(dir.path(), "mac", mac_folder)
        },
        None,
    )
    .await;
    let win = start_receiving(&win(), dir.path(), "win", win_folder.clone(), None).await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    connected(&win, "mac").await;

    let src = dir.path().join("src");
    tree(
        &src,
        &[
            ("ok.txt", b"fine"),
            ("secret.txt", b"keep out"),
            ("Docs/a.txt", b"aaa"),
            ("Docs/locked.txt", b"no"),
        ],
        &[],
    );
    let lock = |path: std::path::PathBuf| {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    };
    lock(src.join("secret.txt"));
    lock(src.join("Docs").join("locked.txt"));
    if std::fs::File::open(src.join("secret.txt")).is_ok() {
        // Root reads anything.
        mac.agent.shutdown().await;
        win.agent.shutdown().await;
        return;
    }

    let arg = |name: &str| src.join(name).to_string_lossy().into_owned();
    let (ok, secret, docs) = (arg("ok.txt"), arg("secret.txt"), arg("Docs"));
    let output = run_ctl(&socket, &["send", "win", &ok, &secret, &docs]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Not everything arrived.
    assert!(!output.status.success(), "{output:?}");
    for line in [
        "ok.txt → win  4 bytes  done",
        "secret.txt → win  0 bytes  failed (unreadable)",
        "Docs/ → win  5 bytes  done  (1 of 2 files, 1 failed)",
        "  failed: Docs/locked.txt (unreadable)",
        "2 of 4 files sent, 2 failed",
    ] {
        assert!(stdout.contains(line), "{line:?} in {stdout}");
    }
    assert_eq!(contents(&win_folder), ["Docs/", "Docs/a.txt", "ok.txt"]);
    // The receiver counts the file that never came, and is done.
    let docs = win
        .agent
        .transfers()
        .into_iter()
        .find(|t| t.name == "Docs")
        .unwrap();
    assert_eq!(docs.state, TransferState::Done);
    let progress = docs.folder.unwrap();
    assert_eq!(
        (progress.entries, progress.done, progress.failed),
        (2, 1, 1)
    );
    assert_eq!(progress.failures[0].path, "Docs/locked.txt");

    // ctl lists the batch together.
    let output = run_ctl(&socket, &["transfers"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("batch → win: 3 items, 9 bytes; 2 of 4 files sent, 2 failed"),
        "{stdout}"
    );

    // Every item unusable: refused whole.
    let output = run_ctl(&socket, &["send", "win", &secret]).await;
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("(unreadable)"),
        "{output:?}"
    );

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

/// An app connected to an agent, as far as these tests need one.
#[cfg(unix)]
struct App {
    conn: iw_ipc::IpcConnection,
    next_id: u64,
    /// Drag events that came while waiting for a response.
    drags: std::collections::VecDeque<iw_proto::app::v1::Event>,
    /// Events about files promised on the clipboard that came meanwhile.
    clipboard: std::collections::VecDeque<iw_proto::app::v1::Event>,
}

#[cfg(unix)]
impl App {
    /// Connects to the agent at `socket` and says Hello as an app, which
    /// counts as one there.
    async fn connect(socket: &std::path::Path) -> Self {
        Self::connect_as(socket, false).await
    }

    /// Like `connect`, as an app that puts files copied on another device
    /// on the clipboard (`takes_clipboard_files`), as the Windows app does.
    async fn connect_as(socket: &std::path::Path, takes_clipboard_files: bool) -> Self {
        use iw_proto::app::v1::{ClientHello, client_message::Command, response};

        let conn = iw_ipc::IpcConnection::connect(socket).await.unwrap();
        let mut app = Self {
            conn,
            next_id: 1,
            drags: Default::default(),
            clipboard: Default::default(),
        };
        let hello = Command::Hello(ClientHello {
            ipc_version: iw_proto::IPC_VERSION,
            client: "test-app/1".into(),
            takes_clipboard_files,
        });
        assert!(matches!(
            app.request(hello).await,
            response::Result::Hello(_)
        ));
        app
    }

    async fn request(
        &mut self,
        command: iw_proto::app::v1::client_message::Command,
    ) -> iw_proto::app::v1::response::Result {
        use iw_proto::app::v1::{AgentMessage, ClientMessage, agent_message};

        let id = self.next_id;
        self.next_id += 1;
        self.conn
            .send(&ClientMessage {
                request_id: id,
                command: Some(command),
            })
            .await
            .unwrap();
        loop {
            let msg = tokio::time::timeout(TIMEOUT, self.conn.recv::<AgentMessage>())
                .await
                .expect("the agent answers")
                .unwrap()
                .unwrap();
            match msg.msg {
                Some(agent_message::Msg::Response(r)) if r.request_id == id => {
                    return r.result.unwrap();
                }
                Some(agent_message::Msg::Event(e))
                    if e.drag_arrived.is_some() || e.drag_ended.is_some() =>
                {
                    self.drags.push_back(e);
                }
                Some(agent_message::Msg::Event(e))
                    if e.clipboard_files_promised.is_some()
                        || e.clipboard_files_arrived.is_some() =>
                {
                    self.clipboard.push_back(e);
                }
                _ => {}
            }
        }
    }

    /// The next event about files promised on the clipboard.
    async fn clipboard_event(&mut self) -> iw_proto::app::v1::Event {
        use iw_proto::app::v1::{AgentMessage, agent_message};

        if let Some(event) = self.clipboard.pop_front() {
            return event;
        }
        loop {
            let msg = tokio::time::timeout(TIMEOUT, self.conn.recv::<AgentMessage>())
                .await
                .expect("an event about clipboard files comes")
                .unwrap()
                .unwrap();
            if let Some(agent_message::Msg::Event(e)) = msg.msg
                && (e.clipboard_files_promised.is_some() || e.clipboard_files_arrived.is_some())
            {
                return e;
            }
        }
    }

    /// The device the next word of the relative mode hotkey pressed where
    /// it cannot work names.
    async fn relative_unsupported(&mut self) -> String {
        use iw_proto::app::v1::{AgentMessage, agent_message};

        loop {
            let msg = tokio::time::timeout(TIMEOUT, self.conn.recv::<AgentMessage>())
                .await
                .expect("word of the relative mode hotkey comes")
                .unwrap()
                .unwrap();
            if let Some(agent_message::Msg::Event(e)) = msg.msg
                && let Some(unsupported) = e.relative_unsupported
            {
                return unsupported.device_id;
            }
        }
    }

    /// The next drag event.
    async fn drag_event(&mut self) -> iw_proto::app::v1::Event {
        use iw_proto::app::v1::{AgentMessage, agent_message};

        if let Some(event) = self.drags.pop_front() {
            return event;
        }
        loop {
            let msg = tokio::time::timeout(TIMEOUT, self.conn.recv::<AgentMessage>())
                .await
                .expect("a drag event comes")
                .unwrap()
                .unwrap();
            if let Some(agent_message::Msg::Event(e)) = msg.msg
                && (e.drag_arrived.is_some() || e.drag_ended.is_some())
            {
                return e;
            }
        }
    }
}

#[cfg(unix)]
fn button(node: &Node, pressed: bool) {
    node.capture
        .emit(InputEvent::Button {
            button: Button::Left,
            pressed,
        })
        .unwrap();
}

/// Presses the button on `node` and reports a drag of `paths` from there,
/// as its app does once the drag nears an edge; returns the drag's id.
#[cfg(unix)]
async fn start_dragging(node: &Node, paths: Vec<std::path::PathBuf>) -> String {
    use inputweave::agent::DragError;

    button(node, true);
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        // The press reaches the agent on a way of its own; until it has,
        // there is no drag.
        match node.agent.begin_drag(paths.clone()).await {
            Ok(id) => return id,
            Err(DragError::NotDragging) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("the drag did not begin: {e}"),
        }
    }
}

/// Releases the button on `source`, the cursor being on `target`, and
/// waits for the release to reach `target`: by then `source` knows where
/// the drag was dropped.
#[cfg(unix)]
async fn drop_on(source: &Node, target: &Node) {
    let before = target.emulation.calls().len();
    button(source, false);
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !target.emulation.calls()[before..].contains(&Emulated::Button(Button::Left, false)) {
        assert!(tokio::time::Instant::now() < deadline, "never dropped");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The next drag that ends, as the agent tells its apps.
#[cfg(unix)]
async fn drag_ended(
    events: &mut tokio::sync::broadcast::Receiver<inputweave::agent::DragEvent>,
) -> inputweave::agent::EndedDrag {
    loop {
        match tokio::time::timeout(TIMEOUT, events.recv()).await {
            Ok(Ok(inputweave::agent::DragEvent::Ended(ended))) => return ended,
            Ok(Ok(_)) => {}
            other => panic!("no drag ended: {other:?}"),
        }
    }
}

/// Checks the app heard of drag `id` arriving from the Mac, offering
/// `names`.
#[cfg(unix)]
fn arrived(event: &iw_proto::app::v1::Event, id: &str, names: &[&str]) {
    let arrived = event
        .drag_arrived
        .as_ref()
        .unwrap_or_else(|| panic!("not an arrival: {event:?}"));
    assert_eq!(arrived.drag_id, id);
    assert_eq!(arrived.from_device_id, "mac");
    let offered: Vec<&str> = arrived.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(offered, names);
}

/// The batch that drag `id` brought to `node`, once it began coming.
#[cfg(unix)]
async fn dragged_batch(node: &Node, id: &str) -> inputweave::agent::BatchInfo {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let found = node
            .agent
            .transfers()
            .into_iter()
            .find_map(|t| t.batch.filter(|b| b.drag.as_deref() == Some(id)));
        if let Some(batch) = found {
            return batch;
        }
        assert!(tokio::time::Instant::now() < deadline, "no files came");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn files_dragged_across_land_where_they_are_dropped() {
    use inputweave::agent::{DragEnd, TransferState};
    use iw_proto::app::v1::{
        AcceptDrag, DeclineDrag, DragEnd as AppDragEnd, client_message::Command, response,
    };

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("win.sock");
    let nodes = desk_with(&[mac(), win()], |s| Options {
        ipc_socket: (s.id == "win").then(|| socket.clone()),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let mut app = App::connect(&socket).await;
    let mut drags = mac.agent.watch_drags();
    let src = dir.path().join("src");
    tree(
        &src,
        &[
            ("Photos/a.jpg", b"aaa"),
            ("Photos/2024/b.jpg", b"bb"),
            ("notes.txt", b"hi"),
        ],
        &["Photos/empty"],
    );
    let dragged = vec![src.join("Photos"), src.join("notes.txt")];

    // Dragged across the seam: the button goes up here and down there.
    let id = start_dragging(mac, dragged.clone()).await;
    let dragging = mac.agent.status().dragging.unwrap();
    assert_eq!(
        (dragging.id.as_str(), dragging.items, dragging.files),
        (id.as_str(), 2, 4)
    );
    mac_enters_win(mac, win).await;
    wait_injected(mac, Emulated::Button(Button::Left, false)).await;
    wait_injected(win, Emulated::Button(Button::Left, true)).await;
    arrived(&app.drag_event().await, &id, &["Photos", "notes.txt"]);
    assert_eq!(
        mac.agent.status().dragging.unwrap().device.as_deref(),
        Some("win")
    );

    // Dropped into a folder there.
    let desktop = dir.path().join("Desktop");
    std::fs::create_dir(&desktop).unwrap();
    drop_on(mac, win).await;
    // Only an absolute folder the user can write to.
    let refused = app
        .request(Command::AcceptDrag(AcceptDrag {
            drag_id: id.clone(),
            destination: "Desktop".into(),
        }))
        .await;
    assert!(matches!(refused, response::Result::Error(e) if e.code == "invalid_folder"));
    let accepted = app
        .request(Command::AcceptDrag(AcceptDrag {
            drag_id: id.clone(),
            destination: desktop.to_string_lossy().into_owned(),
        }))
        .await;
    assert!(
        matches!(accepted, response::Result::Transfers(_)),
        "{accepted:?}"
    );
    let ended = drag_ended(&mut drags).await;
    assert_eq!(
        (ended.id.as_str(), ended.reason),
        (id.as_str(), DragEnd::Accepted)
    );
    assert_eq!(ended.device.as_deref(), Some("win"));
    wait_status(mac, |s| s.dragging.is_none()).await;
    // The batch carries the drag, and lands in that folder only.
    let batch = dragged_batch(win, &id).await;
    let received = batch_finished(win, &batch.id).await;
    assert!(
        received.iter().all(|t| t.state == TransferState::Done),
        "{received:#?}"
    );
    assert_eq!(
        contents(&desktop),
        [
            "Photos/",
            "Photos/2024/",
            "Photos/2024/b.jpg",
            "Photos/a.jpg",
            "Photos/empty/",
            "notes.txt"
        ]
    );
    assert_eq!(std::fs::read(desktop.join("notes.txt")).unwrap(), b"hi");

    // Declined there: it ends, and nothing goes.
    let transfers = mac.agent.transfers().len();
    take_back(mac);
    let id = start_dragging(mac, dragged.clone()).await;
    mac_enters_win(mac, win).await;
    arrived(&app.drag_event().await, &id, &["Photos", "notes.txt"]);
    drop_on(mac, win).await;
    let declined = app
        .request(Command::DeclineDrag(DeclineDrag {
            drag_id: id.clone(),
        }))
        .await;
    assert!(
        matches!(declined, response::Result::Status(_)),
        "{declined:?}"
    );
    let ended = drag_ended(&mut drags).await;
    assert_eq!(
        (ended.id.as_str(), ended.reason),
        (id.as_str(), DragEnd::Declined)
    );
    // Too late to accept it now.
    let late = app
        .request(Command::AcceptDrag(AcceptDrag {
            drag_id: id.clone(),
            destination: desktop.to_string_lossy().into_owned(),
        }))
        .await;
    assert!(matches!(late, response::Result::Error(e) if e.code == "not_found"));

    // Brought back and released here: taken back there, ended here.
    take_back(mac);
    let id = start_dragging(mac, dragged).await;
    mac_enters_win(mac, win).await;
    arrived(&app.drag_event().await, &id, &["Photos", "notes.txt"]);
    motion(mac, (0.0, 0.0), (-10.0, 0.0));
    let withdrawn = app.drag_event().await.drag_ended.unwrap();
    assert_eq!(withdrawn.drag_id, id);
    assert_eq!(withdrawn.reason(), AppDragEnd::Withdrawn);
    button(mac, false);
    let ended = drag_ended(&mut drags).await;
    assert_eq!(
        (ended.id.as_str(), ended.reason),
        (id.as_str(), DragEnd::Released)
    );
    assert_eq!(mac.agent.transfers().len(), transfers);
    assert_eq!(contents(&desktop).len(), 6);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_windows_pc_sends_folders_and_takes_drags_through_the_broker() {
    use inputweave::agent::{DragEnd, TransferState};
    use iw_proto::app::v1::{AcceptDrag, client_message::Command, response};

    let dir = tempfile::tempdir().unwrap();
    let pipe = dir.path().join("broker.sock");
    let socket = dir.path().join("win.sock");
    let mac_folder = dir.path().join("mac-received");
    let pc = Spec {
        clipboard: false,
        ..win()
    };
    let nodes = desk_with(&[mac(), pc], |s| match s.id {
        "win" => Options {
            broker_socket: Some(pipe.clone()),
            ipc_socket: Some(socket.clone()),
            ..Options::default()
        },
        _ => Options {
            receive_folder: Some(mac_folder.clone()),
            ..Options::default()
        },
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let broker = SimBroker::connect(&pipe, &dir.path().join("Downloads")).await;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while win.agent.file_settings().folder.is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the broker never linked"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // A folder of the user's, which only the broker can see.
    let user = dir.path().join("C:").join("Users").join("pc");
    let real = dir.path().join("real");
    tree(
        &real,
        &[
            ("Project/src/main.rs", b"fn main() {}"),
            ("Project/README.md", b"# hi"),
        ],
        &["Project/empty"],
    );
    broker.share(&user.join("Project"), &real.join("Project"));
    let ids = win
        .agent
        .send_files("mac", vec![user.join("Project")])
        .await
        .unwrap();
    let sent = wait_transfer(win, &ids[0], finished).await;
    assert_eq!(sent.state, TransferState::Done, "{sent:#?}");
    assert_eq!(sent.folder.unwrap().done, 3);
    let received = wait_transfer(mac, &ids[0], finished).await;
    assert_eq!(received.state, TransferState::Done);
    assert_eq!(
        contents(&mac_folder),
        [
            "Project/",
            "Project/README.md",
            "Project/empty/",
            "Project/src/",
            "Project/src/main.rs"
        ]
    );

    // Dragged from the Mac and dropped in a folder of the PC's user: the
    // broker checks the folder and writes there, as the user.
    let mut app = App::connect(&socket).await;
    let mut drags = mac.agent.watch_drags();
    let src = dir.path().join("src");
    tree(
        &src,
        &[("Photos/a.jpg", b"aaa"), ("Photos/2024/b.jpg", b"bb")],
        &["Photos/empty"],
    );
    let id = start_dragging(mac, vec![src.join("Photos")]).await;
    mac_enters_win(mac, win).await;
    arrived(&app.drag_event().await, &id, &["Photos"]);
    drop_on(mac, win).await;
    let desktop = dir.path().join("Desktop");
    tree(&desktop, &[("Photos/mine.txt", b"mine")], &[]);
    let accepted = app
        .request(Command::AcceptDrag(AcceptDrag {
            drag_id: id.clone(),
            destination: desktop.to_string_lossy().into_owned(),
        }))
        .await;
    assert!(
        matches!(accepted, response::Result::Transfers(_)),
        "{accepted:?}"
    );
    assert_eq!(drag_ended(&mut drags).await.reason, DragEnd::Accepted);
    let batch = dragged_batch(win, &id).await;
    let received = batch_finished(win, &batch.id).await;
    assert_eq!(received[0].state, TransferState::Done, "{received:#?}");
    assert_eq!(
        received[0].path.as_deref(),
        Some(desktop.join("Photos (1)").as_path())
    );
    assert_eq!(
        contents(&desktop),
        [
            "Photos (1)/",
            "Photos (1)/2024/",
            "Photos (1)/2024/b.jpg",
            "Photos (1)/a.jpg",
            "Photos (1)/empty/",
            "Photos/",
            "Photos/mine.txt",
        ]
    );
    // Nothing went to the receive folder.
    assert!(!dir.path().join("Downloads").join("Photos").exists());

    broker.quit().await;
    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// Starts an agent as `config` says, with mock backends showing `displays`
/// and `status` for its session; its `MockStatus` is not the one used.
#[cfg(unix)]
async fn start_as(
    config: Config,
    identity: Identity,
    displays: Vec<Display>,
    status: Box<dyn iw_platform::SessionStatus>,
    options: Options,
) -> Node {
    let (capture, handle) = MockCapture::new();
    let emulation = RecordingEmulation::default();
    let displays = SharedDisplays::new(displays);
    let backends = Backends {
        capture: Box::new(capture),
        emulation: Box::new(emulation.clone()),
        displays: Box::new(displays.clone()),
        pictures: Box::new(iw_platform::NoDesktopPictures),
        status,
        clipboard: None,
    };
    let agent = agent::start(config, Arc::new(identity), backends, options)
        .await
        .expect("agent starts");
    Node {
        agent,
        capture: handle,
        emulation,
        displays,
        clipboard: MockClipboard::default(),
        pictures: MockDesktopPictures::default(),
        status: MockStatus::granted(),
    }
}

/// Every file and folder under `dir`, with its size, time of change and
/// contents.
#[cfg(unix)]
fn snapshot(dir: &std::path::Path) -> Vec<(std::path::PathBuf, u64, i64, Vec<u8>)> {
    use std::os::unix::fs::MetadataExt;
    let mut found = Vec::new();
    let mut pending = vec![dir.to_owned()];
    while let Some(path) = pending.pop() {
        let meta = std::fs::symlink_metadata(&path).unwrap();
        let contents = if meta.is_file() {
            std::fs::read(&path).unwrap()
        } else {
            Vec::new()
        };
        if meta.is_dir() {
            for entry in std::fs::read_dir(&path).unwrap() {
                pending.push(entry.unwrap().path());
            }
        }
        found.push((
            path,
            meta.len(),
            meta.mtime_nsec() + meta.mtime() * 1_000_000_000,
            contents,
        ));
    }
    found.sort();
    found
}

/// The macOS login window's agent (`login_window`) runs as the device of
/// the user who turned it on, from their own folder, which it never
/// writes: same id and identity, the PC reaches and controls it, and
/// nobody can pair with it.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_login_window_agent_is_the_users_device_and_writes_nothing() {
    use inputweave::login_window::{self, LoginWindowSession};
    use std::os::unix::fs::MetadataExt;

    let home = tempfile::tempdir().unwrap();
    let uid = std::fs::metadata(home.path()).unwrap().uid();
    let folder = login_window::USER_FOLDER
        .iter()
        .fold(home.path().to_owned(), |path, name| path.join(name));
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("config.toml"),
        "[device]\nid = \"mac\"\nname = \"MacBook Pro\"\n\n[network]\nlisten = \
         \"127.0.0.1:0\"\ndiscovery = false\n\n[paths]\ndevices_file = \"/nowhere/devices.toml\"\n",
    )
    .unwrap();
    let devices = folder.join("devices.toml");
    let spec = mac();
    let win_dir = tempfile::tempdir().unwrap();
    let win = unpaired(
        &win(),
        Identity::generate().unwrap(),
        Some(win_dir.path().join("devices.toml")),
    )
    .await;
    let desktop = MockDesktopPictures::default();
    desktop.set("builtin", fake_jpeg(7, 1500));
    let options = Options {
        devices_file: Some(devices.clone()),
        login_window: login_window::Support::Available,
        ..Options::default()
    };
    let mac = start_showing(
        &spec,
        Identity::load_or_generate(&folder.join("identity")).unwrap(),
        &[],
        &[],
        options,
        desktop,
    )
    .await;
    pair(&mac, &win).await;
    agreed(&mac, &win).await;
    wait_arrangement(&win, |a| picture(a, "mac", "builtin").is_some()).await;

    // Only where the lock screen takes input.
    assert_eq!(
        mac.agent.set_login_window_access(true).unwrap_err().code(),
        "no_lock_screen_input"
    );
    mac.status.set_lock_screen_input(true);
    wait_status(&mac, |s| s.lock_screen_input).await;
    mac.agent.set_login_window_access(true).unwrap();
    wait_status(&mac, |s| s.login_window_access).await;
    let listed = inputweave::ctl::format_status(&mac.agent.status().to_proto());
    assert!(listed.contains("login window:"), "{listed}");
    mac.agent.shutdown().await;
    assert!(
        inputweave::store::DevicesFile::load(&devices)
            .unwrap()
            .login_window_access
    );
    wait_status(&win, |s| s.device("mac").is_some_and(|d| !d.connected)).await;
    let before = snapshot(home.path());

    let state = login_window::load(home.path(), uid)
        .unwrap()
        .expect("turned on");
    assert_eq!(state.config.device.id, "mac");
    assert!(state.config.paths.devices_file.is_none());
    let at_login = start_as(
        state.config,
        state.identity,
        spec.displays.clone(),
        Box::new(LoginWindowSession),
        login_window::options(state.devices, state.offline),
    )
    .await;
    assert_eq!(at_login.agent.device_id(), "mac");
    connected(&at_login, "win").await;
    // Locked, as a login window is, and taking input.
    wait_status(&win, |s| {
        s.device("mac")
            .is_some_and(|d| d.connected && d.screen_locked && d.lock_screen_input)
    })
    .await;
    // It cannot read the user's desktop picture: the PC keeps the one it
    // has, in memory and in its files, as the session's displays came
    // before its lock state.
    assert_eq!(
        picture(&win.agent.arrangement(), "mac", "builtin"),
        Some(fake_jpeg(7, 1500))
    );
    let kept = win_dir.path().join("previews/mac");
    assert_eq!(std::fs::read_dir(&kept).unwrap().count(), 1, "{kept:?}");

    // The PC's cursor goes over and types.
    motion(&win, (0.0, 500.0), (-5.0, 0.0));
    wait_status(&at_login, |s| s.controlled_by.as_deref() == Some("win")).await;
    key(&win, usage::A, true);
    wait_injected(&at_login, Emulated::Key(usage::A, true)).await;
    key(&win, usage::A, false);
    wait_injected(&at_login, Emulated::Key(usage::A, false)).await;

    // Nobody could see a pairing code there.
    let mini = unpaired(&mini(), Identity::generate().unwrap(), None).await;
    mini.agent
        .start_pairing(at_login.agent.local_addr())
        .await
        .unwrap();
    wait_pairing(&mini, |s| {
        matches!(
            s,
            PairingStatus::Failed {
                reason: Failure::NotAllowed,
                ..
            }
        )
    })
    .await;

    mini.agent.shutdown().await;
    at_login.agent.shutdown().await;
    win.agent.shutdown().await;
    assert_eq!(snapshot(home.path()), before, "the user's folder changed");

    // Turned off, there is nothing to run as.
    let mut stored = inputweave::store::DevicesFile::load(&devices).unwrap();
    stored.login_window_access = false;
    stored.save(&devices).unwrap();
    assert!(login_window::load(home.path(), uid).unwrap().is_none());
}

/// Turning control at the login window on is refused, with the reason,
/// where the login window's agent could not run as this device; turning it
/// off always works.
#[tokio::test(flavor = "multi_thread")]
async fn control_at_the_login_window_is_refused_where_it_cannot_work() {
    use inputweave::login_window::Support;

    for (support, code) in [
        (Support::Unsupported, "unsupported"),
        (Support::NotInstalled, "not_installed"),
        (Support::CustomPaths, "custom_paths"),
    ] {
        let options = Options {
            login_window: support,
            ..Options::default()
        };
        let node = start_with(&mac(), Identity::generate().unwrap(), &[], &[], options).await;
        node.status.set_lock_screen_input(true);
        wait_status(&node, |s| s.lock_screen_input).await;
        let refused = node.agent.set_login_window_access(true).unwrap_err();
        assert_eq!(refused.code(), code, "{support:?}");
        node.agent.set_login_window_access(false).unwrap();
        assert!(!node.agent.status().login_window_access);
        node.agent.shutdown().await;
    }
}

/// Like [`desk_with`], except that devices `apart` neither trust nor
/// reach each other: they only meet through the others.
async fn desk_apart(
    specs: &[Spec],
    options: impl Fn(&Spec) -> Options,
    apart: (&str, &str),
) -> Vec<Node> {
    let identities: Vec<Identity> = specs
        .iter()
        .map(|_| Identity::generate().unwrap())
        .collect();
    let mut known: Vec<Known> = specs
        .iter()
        .zip(&identities)
        .map(|(s, i)| Known {
            id: s.id,
            name: s.name,
            fingerprint: i.fingerprint(),
            address: None,
        })
        .collect();
    let all: Vec<&Spec> = specs.iter().collect();
    let kept_apart = |a: &str, b: &str| (a, b) == apart || (b, a) == apart;
    let mut order: Vec<usize> = (0..specs.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(specs[i].id));
    let mut identities: Vec<Option<Identity>> = identities.into_iter().map(Some).collect();
    let mut nodes: Vec<Option<Node>> = (0..specs.len()).map(|_| None).collect();
    for i in order {
        let trusted: Vec<Known> = known
            .iter()
            .filter(|k| !kept_apart(k.id, specs[i].id))
            .map(|k| Known {
                id: k.id,
                name: k.name,
                fingerprint: k.fingerprint,
                address: k.address,
            })
            .collect();
        let node = start_with(
            &specs[i],
            identities[i].take().unwrap(),
            &all,
            &trusted,
            options(&specs[i]),
        )
        .await;
        known[i].address = Some(node.agent.local_addr());
        nodes[i] = Some(node);
    }
    let nodes: Vec<Node> = nodes.into_iter().map(Option::unwrap).collect();
    for (spec, node) in specs.iter().zip(&nodes) {
        let peers: Vec<&str> = specs
            .iter()
            .map(|s| s.id)
            .filter(|id| *id != spec.id && !kept_apart(id, spec.id))
            .collect();
        wait_status(node, |s| {
            s.layout_ready
                && peers
                    .iter()
                    .all(|p| s.device(p).is_some_and(|d| d.connected && d.arranged))
        })
        .await;
    }
    nodes
}

/// Files and folders as copied in a file manager, made in `dir`: a file,
/// and a folder with a file, a nested one and an empty folder.
fn copied_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let report = dir.join("report.txt");
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(&report, b"quarterly numbers").unwrap();
    let photos = dir.join("photos");
    std::fs::create_dir_all(photos.join("trip/day 1")).unwrap();
    std::fs::create_dir_all(photos.join("empty")).unwrap();
    std::fs::write(photos.join("cover.jpg"), vec![7u8; 300_000]).unwrap();
    std::fs::write(photos.join("trip/day 1/beach.jpg"), vec![9u8; 2_000_000]).unwrap();
    vec![report, photos]
}

/// Whether `a` and `b` hold the same files and folders, all the way down.
fn same_tree(a: &std::path::Path, b: &std::path::Path) -> bool {
    let names = |dir: &std::path::Path| {
        let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    if a.is_dir() {
        let (here, there) = (names(a), names(b));
        here == there
            && here
                .iter()
                .all(|name| same_tree(&a.join(name), &b.join(name)))
    } else {
        b.is_file() && std::fs::read(a).unwrap() == std::fs::read(b).unwrap()
    }
}

/// A desk whose devices fetch files pasted on them into folders of their
/// own in `dir`.
async fn pasting_desk(specs: &[Spec], dir: &std::path::Path) -> Vec<Node> {
    let dir = dir.to_owned();
    desk_with(specs, move |spec| Options {
        clipboard_staging: Some(dir.join(spec.id)),
        ..Options::default()
    })
    .await
}

/// Waits until `node`'s clipboard promises an offer, and returns the files
/// it promises.
async fn wait_promised_files(node: &Node) -> Vec<iw_platform::PromisedFile> {
    wait_promise(node).await;
    node.clipboard.promised_files().unwrap()
}

/// Waits until `node`'s clipboard promises files named `names`, none for
/// an offer without files.
async fn wait_promised_names(node: &Node, names: &[&str]) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let promised = node.clipboard.promised_files();
        let named = promised
            .as_ref()
            .map(|files| files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>());
        if named.as_deref() == Some(names) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {names:?}; promised {promised:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// As if a file manager on `node` pasted.
async fn paste_files(node: &Node) -> Option<iw_platform::FilesFetch> {
    let clipboard = node.clipboard.clone();
    tokio::task::spawn_blocking(move || clipboard.paste_files())
        .await
        .unwrap()
}

/// The files `node` sent so far.
fn files_sent(node: &Node) -> usize {
    node.agent
        .transfers()
        .iter()
        .filter(|t| t.direction == agent::Direction::Send)
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn files_copied_on_one_device_come_when_pasted_on_another() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");
    let nodes = pasting_desk(&[mac(), win()], &staging).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // Copied in the Mac's file manager, with their names as text: the PC
    // promises both, and nothing has moved yet.
    let copied = copied_files(&dir.path().join("source"));
    mac.clipboard
        .copy_files(copied.clone(), text("report.txt\nphotos"));
    mac_enters_win(mac, win).await;
    let promised = wait_promised_files(win).await;
    assert_eq!(
        promised,
        vec![
            iw_platform::PromisedFile {
                name: "report.txt".to_owned(),
                folder: false,
            },
            iw_platform::PromisedFile {
                name: "photos".to_owned(),
                folder: true,
            },
        ]
    );
    assert_eq!(
        win.clipboard.promised(),
        Some(vec![iw_platform::ClipboardFormat::Text])
    );
    assert!(mac.agent.transfers().is_empty());
    assert!(win.agent.transfers().is_empty());
    assert_eq!(mac.agent.status().lazy_clipboard.offered, 1);

    // The PC's file manager pastes: only now do they come, straight from
    // the Mac into a folder of the PC's.
    let Some(iw_platform::FilesFetch::Ready(paths)) = paste_files(win).await else {
        panic!("the pasted files did not arrive");
    };
    let [Some(report), Some(photos)] = &paths[..] else {
        panic!("not every item arrived: {paths:?}");
    };
    assert!(
        report.starts_with(staging.join("win")),
        "{}",
        report.display()
    );
    assert_eq!(std::fs::read(report).unwrap(), b"quarterly numbers");
    assert!(same_tree(&copied[1], photos));
    assert!(photos.join("empty").is_dir());
    let received = win.agent.transfers();
    assert_eq!(received.len(), 2);
    assert!(received.iter().all(|t| {
        t.state == agent::TransferState::Done
            && t.batch.as_ref().is_some_and(|b| b.clipboard.is_some())
    }));

    // Pasted again, nothing more comes from the Mac; the names come along
    // as any format offered.
    let sent = files_sent(mac);
    assert_eq!(
        paste_files(win).await,
        Some(iw_platform::FilesFetch::Ready(paths.clone()))
    );
    assert_eq!(paste(win).await, text("report.txt\nphotos"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(files_sent(mac), sent);

    // Something else copied on the PC: the folder they came into goes.
    let folder = report.parent().unwrap().to_owned();
    win.clipboard.copy(text("something else"), false);
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while folder.exists() {
        assert!(tokio::time::Instant::now() < deadline, "still there");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// On Windows the app holds a promise that has files, since the broker
/// cannot: it hears of it, asks, puts it on the clipboard, and when File
/// Explorer pastes, has the files fetched into a folder of its own, written
/// by the broker, once for pastes at the same time; once the user quit the
/// app it is not started for them, and with no app to hold it otherwise,
/// the broker is asked to start one and, when none comes, promises the
/// content without the files.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn files_copied_on_a_mac_are_pasted_through_the_windows_app() {
    use iw_platform::ClipboardFormat;
    use iw_proto::app::v1::{
        ClipboardFilesGone, ClipboardFilesPlace, ClipboardFilesPlaced, PasteClipboardFiles,
        UserQuitApp, client_message::Command, response,
    };

    let dir = tempfile::tempdir().unwrap();
    let pipe = dir.path().join("broker.sock");
    let socket = dir.path().join("win.sock");
    let pc = Spec {
        clipboard: false,
        ..win()
    };
    let nodes = desk_with(&[mac(), pc], |s| Options {
        broker_socket: (s.id == "win").then(|| pipe.clone()),
        ipc_socket: (s.id == "win").then(|| socket.clone()),
        app_clipboard_wait: Some(Duration::from_millis(500)),
        open_app: s.id == "win",
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let broker = SimBroker::connect(&pipe, &dir.path().join("Downloads")).await;
    let mut app = App::connect_as(&socket, true).await;
    let error_code = |result: response::Result| match result {
        response::Result::Error(e) => e.code,
        other => panic!("not refused: {other:?}"),
    };

    // Copied in the Mac's file manager: the app hears of the files, with
    // their names as text, and puts them on the clipboard.
    let copied = copied_files(&dir.path().join("source"));
    mac.clipboard
        .copy_files(copied.clone(), text("report.txt\nphotos"));
    mac_enters_win(mac, win).await;
    let promised = app
        .clipboard_event()
        .await
        .clipboard_files_promised
        .expect("files promised");
    let items: Vec<_> = promised
        .items
        .iter()
        .map(|i| (i.name.as_str(), i.folder))
        .collect();
    assert_eq!(items, [("report.txt", false), ("photos", true)]);
    assert_eq!(promised.from_device_id, "mac");
    // As the Windows clipboard holds it.
    assert_eq!(promised.inline.len(), 1);
    assert_eq!(promised.inline[0].standard, 13);
    let utf16: Vec<u8> = "report.txt\r\nphotos\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    assert_eq!(promised.inline[0].data, utf16);
    let id = promised.promise_id;
    // Placed only once allowed to.
    assert_eq!(
        error_code(
            app.request(Command::ClipboardFilesPlaced(ClipboardFilesPlaced {
                promise_id: id,
                ok: true,
            }))
            .await
        ),
        "not_found"
    );
    assert!(matches!(
        app.request(Command::ClipboardFilesPlace(ClipboardFilesPlace {
            promise_id: id
        }))
        .await,
        response::Result::Status(_)
    ));
    assert!(matches!(
        app.request(Command::ClipboardFilesPlaced(ClipboardFilesPlaced {
            promise_id: id,
            ok: true,
        }))
        .await,
        response::Result::Status(_)
    ));
    wait_status(win, |s| s.lazy_clipboard.received == 1).await;
    assert!(broker.desk.lock().unwrap().promised.is_empty());
    assert!(win.agent.transfers().is_empty());

    // A folder the user cannot write in is refused.
    let paste = |folder: &std::path::Path| {
        Command::PasteClipboardFiles(PasteClipboardFiles {
            promise_id: id,
            folder: folder.to_string_lossy().into_owned(),
        })
    };
    assert_eq!(
        error_code(app.request(paste(&dir.path().join("nowhere"))).await),
        "invalid_folder"
    );

    // File Explorer pastes, twice at once: the files come straight from
    // the Mac into the app's folder, once, and the app hears where each
    // one is, once.
    let staging = dir.path().join("AppData").join("clipboard").join("1");
    std::fs::create_dir_all(&staging).unwrap();
    for _ in 0..2 {
        assert!(matches!(
            app.request(paste(&staging)).await,
            response::Result::Status(_)
        ));
    }
    let arrived = app
        .clipboard_event()
        .await
        .clipboard_files_arrived
        .expect("files arrived");
    assert_eq!(arrived.promise_id, id);
    assert!(arrived.ok);
    let [report, photos] = &arrived.paths[..] else {
        panic!("{:?}", arrived.paths);
    };
    let (report, photos) = (std::path::Path::new(report), std::path::Path::new(photos));
    assert!(report.starts_with(&staging), "{}", report.display());
    assert_eq!(std::fs::read(report).unwrap(), b"quarterly numbers");
    assert!(same_tree(&copied[1], photos));
    assert!(win.agent.transfers().iter().all(|t| {
        t.state == agent::TransferState::Done
            && t.batch.as_ref().is_some_and(|b| b.clipboard.is_some())
    }));
    let sent = win.agent.transfers().len();
    // Pasted again, it hears the same at once, and nothing is sent again.
    assert!(matches!(
        app.request(paste(&staging)).await,
        response::Result::Status(_)
    ));
    let again = app
        .clipboard_event()
        .await
        .clipboard_files_arrived
        .expect("files arrived again");
    assert_eq!(again, arrived);
    assert!(app.clipboard.is_empty(), "{:?}", app.clipboard);
    assert_eq!(win.agent.transfers().len(), sent);

    // Copied over: the promise is gone, and nothing more can be pasted.
    assert!(matches!(
        app.request(Command::ClipboardFilesGone(ClipboardFilesGone {
            promise_id: id
        }))
        .await,
        response::Result::Status(_)
    ));
    assert_eq!(error_code(app.request(paste(&staging)).await), "not_found");

    // The user quits the app: it is not started for files on the
    // clipboard, and the broker promises the names without the files.
    assert!(matches!(
        app.request(Command::UserQuitApp(UserQuitApp {})).await,
        response::Result::Status(_)
    ));
    drop(app);
    take_back(mac);
    wait_status(mac, |s| s.cursor_on == "mac").await;
    mac.clipboard.copy_files(copied.clone(), text("again"));
    mac_enters_win(mac, win).await;
    let (_, mimes, _) = broker.wait_promises(1).await;
    assert_eq!(mimes, [ClipboardFormat::Text.mime().to_owned()]);
    assert_eq!(broker.desk.lock().unwrap().launches, 0);

    // Once an app connected again, with none connected the broker is asked
    // to start one; none comes, and the broker promises the names again.
    drop(App::connect_as(&socket, true).await);
    take_back(mac);
    wait_status(mac, |s| s.cursor_on == "mac").await;
    mac.clipboard.copy_files(copied, text("once more"));
    mac_enters_win(mac, win).await;
    let (_, mimes, _) = broker.wait_promises(2).await;
    assert_eq!(mimes, [ClipboardFormat::Text.mime().to_owned()]);
    assert_eq!(broker.desk.lock().unwrap().launches, 1);

    broker.quit().await;
    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// The app may put files on the clipboard only while the agent waits for
/// it and nothing was copied on the PC since: asking late, it is told so
/// and the names the broker promised meanwhile stay; asking after a copy on
/// the PC, nothing is promised at all. A broker that cannot start the app
/// is not waited for.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn files_on_the_clipboard_are_left_alone_late_or_copied_over() {
    use iw_platform::{ClipboardFormat, ClipboardRead};
    use iw_proto::app::v1::{ClipboardFilesPlace, client_message::Command, response};

    let dir = tempfile::tempdir().unwrap();
    let pipe = dir.path().join("broker.sock");
    let socket = dir.path().join("win.sock");
    let pc = Spec {
        clipboard: false,
        ..win()
    };
    let wait = Duration::from_secs(3);
    let nodes = desk_with(&[mac(), pc], |s| Options {
        broker_socket: (s.id == "win").then(|| pipe.clone()),
        ipc_socket: (s.id == "win").then(|| socket.clone()),
        app_clipboard_wait: Some(wait),
        open_app: s.id == "win",
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let broker = SimBroker::connect(&pipe, &dir.path().join("Downloads")).await;
    let mut app = App::connect_as(&socket, true).await;
    let place = |id| Command::ClipboardFilesPlace(ClipboardFilesPlace { promise_id: id });
    let error_code = |result: response::Result| match result {
        response::Result::Error(e) => e.code,
        other => panic!("not refused: {other:?}"),
    };
    let copied = copied_files(&dir.path().join("source"));

    // The app does not ask in time: the broker promises the names, and the
    // app asking then is told it is too late; the broker's promise stays.
    mac.clipboard
        .copy_files(copied.clone(), text("report.txt\nphotos"));
    mac_enters_win(mac, win).await;
    let late = app
        .clipboard_event()
        .await
        .clipboard_files_promised
        .expect("files promised")
        .promise_id;
    let (_, mimes, _) = broker.wait_promises(1).await;
    assert_eq!(mimes, [ClipboardFormat::Text.mime().to_owned()]);
    assert_eq!(error_code(app.request(place(late)).await), "too_late");
    {
        let desk = broker.desk.lock().unwrap();
        assert_eq!(desk.promised.len(), 1);
        assert!(desk.withdrawn.is_empty());
    }

    // Copied on the PC before the app asks: nothing is promised, and the
    // offer is let go as superseded.
    take_back(mac);
    wait_status(mac, |s| s.cursor_on == "mac").await;
    mac.clipboard.copy_files(copied.clone(), text("again"));
    mac_enters_win(mac, win).await;
    let copied_over = app
        .clipboard_event()
        .await
        .clipboard_files_promised
        .expect("files promised again")
        .promise_id;
    broker.copy(ClipboardRead::Content(text("on the pc"))).await;
    assert_eq!(
        error_code(app.request(place(copied_over)).await),
        "superseded"
    );
    let status = wait_status(win, |s| s.lazy_clipboard.superseded == 1).await;
    assert_eq!(status.lazy_clipboard.last.as_deref(), Some("superseded"));
    assert_eq!(broker.desk.lock().unwrap().promised.len(), 1);

    // No app, and the broker cannot start it: the names are promised at
    // once, without waiting for an app.
    drop(app);
    broker.desk.lock().unwrap().launch_refused = true;
    take_back(mac);
    wait_status(mac, |s| s.cursor_on == "mac").await;
    mac.clipboard.copy_files(copied, text("once more"));
    let entered = std::time::Instant::now();
    mac_enters_win(mac, win).await;
    let (_, mimes, _) = broker.wait_promises(2).await;
    assert_eq!(mimes, [ClipboardFormat::Text.mime().to_owned()]);
    assert!(entered.elapsed() < wait, "{:?}", entered.elapsed());
    assert_eq!(broker.desk.lock().unwrap().launches, 1);
    assert_eq!(
        broker.desk.lock().unwrap().launch_reasons,
        [iw_proto::service::v1::LaunchReason::ClipboardFiles]
    );

    broker.quit().await;
    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pasted_files_follow_the_rules_of_files() {
    let dir = tempfile::tempdir().unwrap();
    let nodes = pasting_desk(&[mac(), win()], &dir.path().join("staging")).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let copied = copied_files(&dir.path().join("source"));

    // Copied over on the Mac before the paste: nothing comes.
    mac.clipboard.copy_files(copied.clone(), text("files"));
    mac_enters_win(mac, win).await;
    wait_promised_files(win).await;
    mac.clipboard.copy(text("newer"), false);
    assert_eq!(
        paste_files(win).await,
        Some(iw_platform::FilesFetch::Unavailable)
    );
    assert_eq!(files_sent(mac), 0);
    take_back(mac);
    wait_status(mac, |s| s.cursor_on == "mac").await;

    // A file the Mac cannot read fails alone; the rest of its folder comes.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mixed = dir.path().join("source/mixed");
        std::fs::create_dir_all(&mixed).unwrap();
        std::fs::write(mixed.join("ok.txt"), b"fine").unwrap();
        let secret = mixed.join("secret.txt");
        std::fs::write(&secret, b"hidden").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();
        mac.clipboard.copy_files(vec![mixed], text("mixed"));
        mac_enters_win(mac, win).await;
        wait_promised_names(win, &["mixed"]).await;
        let Some(iw_platform::FilesFetch::Ready(paths)) = paste_files(win).await else {
            panic!("the readable file did not arrive");
        };
        let mixed = paths[0].clone().unwrap();
        assert_eq!(std::fs::read(mixed.join("ok.txt")).unwrap(), b"fine");
        assert!(!mixed.join("secret.txt").exists());
        let row = win
            .agent
            .transfers()
            .into_iter()
            .find(|t| t.name == "mixed")
            .unwrap();
        assert_eq!(row.folder.unwrap().failed, 1);
        take_back(mac);
        wait_status(mac, |s| s.cursor_on == "mac").await;
    }

    // Files turned off with the Mac on the PC: the names still come, the
    // files are not promised.
    win.agent.set_device_sharing("mac", false, true).unwrap();
    wait_status(win, |s| s.device("mac").is_some_and(|d| d.files_off)).await;
    mac.clipboard.copy_files(copied.clone(), text("names only"));
    mac_enters_win(mac, win).await;
    wait_promised_names(win, &[]).await;
    assert_eq!(paste(win).await, text("names only"));
    win.agent.set_device_sharing("mac", false, false).unwrap();
    take_back(mac);
    wait_status(mac, |s| s.cursor_on == "mac").await;

    // Turned off on the Mac's side, the files are not even offered: the
    // names go as any small clipboard.
    mac.agent.set_device_sharing("win", false, true).unwrap();
    wait_status(mac, |s| s.device("win").is_some_and(|d| d.files_off)).await;
    mac.clipboard.copy_files(copied, text("not offered"));
    mac_enters_win(mac, win).await;
    wait_clipboard(win, &text("not offered")).await;
    assert!(win.clipboard.promised().is_none());

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn files_copied_on_a_device_not_linked_here_are_not_promised() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");
    let nodes = desk_apart(
        &[mac(), mini(), win()],
        |spec| Options {
            clipboard_staging: Some(staging.join(spec.id)),
            ..Options::default()
        },
        ("mini", "win"),
    )
    .await;
    let (mac, mini, win) = (&nodes[0], &nodes[1], &nodes[2]);

    // Copied on the PC, pasted on the mini, which has no link of its own
    // to the PC: the names come through the Mac, the files cannot.
    mac_enters_win(mac, win).await;
    win.clipboard
        .copy_files(copied_files(&dir.path().join("source")), text("names"));
    motion(mac, (0.0, 0.0), (484.0, -895.0)); // to canvas (2000, 5)
    motion(mac, (0.0, 0.0), (0.0, -10.0)); // up into the mini
    wait_status(mini, |s| s.controlled_by.as_deref() == Some("mac")).await;
    assert!(wait_promised_files(mini).await.is_empty());
    assert_eq!(paste(mini).await, text("names"));
    assert_eq!(files_sent(win), 0);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pull_of_files_must_name_an_offer_the_origin_made() {
    use iw_net::{PeerEndpoint, Session, hello};
    use iw_proto::peer::v1::{ClipboardFilesPull, ClipboardSkip, Control, Os, control};

    // A device the Mac trusts, speaking the protocol by hand. The Mac dials
    // it, its id sorting after the Mac's.
    let rogue = PeerEndpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    )
    .unwrap();
    let known = [Known {
        id: "zed",
        name: "Rogue",
        fingerprint: rogue.fingerprint(),
        address: Some(rogue.local_addr().unwrap()),
    }];
    let spec = mac();
    let mac = start(&spec, Identity::generate().unwrap(), &[&spec], &known).await;
    let mac_fp = mac.agent.fingerprint();
    let accepted = tokio::time::timeout(TIMEOUT, async {
        rogue
            .accept()
            .await
            .unwrap()
            .finish(|fp| *fp == mac_fp)
            .await
            .unwrap()
    })
    .await
    .unwrap();
    let mut session = Session::accept(
        accepted.connection,
        accepted.peer,
        hello("zed", "Rogue", Os::Linux),
    )
    .await
    .unwrap();

    // Files copied on the Mac, offered to nobody: a pull naming any offer
    // gets nothing.
    let dir = tempfile::tempdir().unwrap();
    mac.clipboard
        .copy_files(copied_files(dir.path()), text("report.txt"));
    let pull_id = (1u64 << 63) | 1;
    let pull = Control {
        msg: Some(control::Msg::ClipboardFilesPull(ClipboardFilesPull {
            pull_id,
            offer_id: 0x1234_5678_9abc_def0,
        })),
    };
    session.control_tx.send(&pull).await.unwrap();
    let skipped = tokio::time::timeout(TIMEOUT, async {
        loop {
            let msg: Control = session.control_rx.recv().await.unwrap().unwrap();
            if let Some(control::Msg::ClipboardSkipped(skipped)) = msg.msg
                && skipped.id == pull_id
            {
                return skipped;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(skipped.reason(), ClipboardSkip::Changed);
    assert!(mac.agent.transfers().is_empty());
    let opened = tokio::time::timeout(
        Duration::from_millis(300),
        session.info.accept_transfer(1024 * 1024),
    )
    .await;
    // A closed connection opened none: this hand-written peer sends no
    // heartbeats, so the agent may have given up on it meanwhile.
    assert!(
        !matches!(opened, Ok(Ok(_))),
        "the Mac opened a transfer stream"
    );
    assert!(mac.agent.transfers().is_empty());

    mac.agent.shutdown().await;
}

/// A Mac and a PC whose clipboard and files the simulated broker holds,
/// as the PC's user; the Mac fetches pasted files into `staging`.
#[cfg(unix)]
async fn explorer_desk(dir: &std::path::Path) -> (Vec<Node>, SimBroker) {
    let pipe = dir.join("broker.sock");
    let staging = dir.join("staging");
    let pc = Spec {
        clipboard: false,
        ..win()
    };
    let nodes = desk_with(&[mac(), pc], |s| Options {
        broker_socket: (s.id == "win").then(|| pipe.clone()),
        clipboard_staging: (s.id == "mac").then(|| staging.join("mac")),
        ..Options::default()
    })
    .await;
    let broker = SimBroker::connect(&pipe, &dir.join("Downloads")).await;
    (nodes, broker)
}

/// `real`, as the PC's user knows them: paths under `user`, which only the
/// broker resolves.
#[cfg(unix)]
fn shared_as(
    broker: &SimBroker,
    user: &std::path::Path,
    real: &[std::path::PathBuf],
) -> Vec<std::path::PathBuf> {
    real.iter()
        .map(|real| {
            let path = user.join(real.file_name().unwrap());
            broker.share(&path, real);
            path
        })
        .collect()
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn files_copied_in_explorer_are_pasted_on_a_mac() {
    use iw_platform::{ClipboardRead, FilesFetch, PromisedFile};

    let dir = tempfile::tempdir().unwrap();
    let (nodes, broker) = explorer_desk(dir.path()).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // The user's files are only where the broker looks: the agent, as
    // SYSTEM, cannot find them, and opens none of them.
    let real = copied_files(&dir.path().join("real"));
    let user = dir.path().join("C:").join("Users").join("pc");
    let copied = shared_as(&broker, &user, &real);
    assert!(!copied[0].exists());

    // Copied in File Explorer: offered as the cursor comes back, the
    // broker looking at the top of each only.
    mac_enters_win(mac, win).await;
    broker
        .copy_files(copied.clone(), ClipboardRead::Empty)
        .await;
    take_back(mac);
    assert_eq!(
        wait_promised_files(mac).await,
        vec![
            PromisedFile {
                name: "report.txt".to_owned(),
                folder: false,
            },
            PromisedFile {
                name: "photos".to_owned(),
                folder: true,
            },
        ]
    );
    assert!(win.agent.transfers().is_empty());
    {
        // Asked about at once, in no particular order.
        let desk = broker.desk.lock().unwrap();
        let mut listed = desk.listed.clone();
        listed.sort();
        let mut asked = copied.clone();
        asked.sort();
        assert_eq!(listed, asked);
        assert!(desk.opened.is_empty());
    }

    // Pasted on the Mac: listed whole and read by the broker, as the user.
    let Some(FilesFetch::Ready(paths)) = paste_files(mac).await else {
        panic!("the pasted files did not arrive");
    };
    let [Some(report), Some(photos)] = &paths[..] else {
        panic!("not every item arrived: {paths:?}");
    };
    assert_eq!(std::fs::read(report).unwrap(), b"quarterly numbers");
    assert!(same_tree(&real[1], photos));
    assert!(photos.join("empty").is_dir());
    let sent = files_sent(win);
    assert_eq!(broker.desk.lock().unwrap().opened.len(), 3);

    // Pasted again: nothing more comes.
    assert_eq!(
        paste_files(mac).await,
        Some(FilesFetch::Ready(paths.clone()))
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(files_sent(win), sent);

    // Offered again, then something else copied on the PC before the
    // paste: nothing comes.
    mac_enters_win(mac, win).await;
    broker
        .copy_files(copied[..1].to_vec(), ClipboardRead::Empty)
        .await;
    take_back(mac);
    wait_promised_names(mac, &["report.txt"]).await;
    broker.copy(ClipboardRead::Content(text("newer"))).await;
    assert_eq!(paste_files(mac).await, Some(FilesFetch::Unavailable));
    assert_eq!(files_sent(win), sent);

    broker.quit().await;
    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn files_copied_in_explorer_follow_the_rules_of_files() {
    use iw_platform::{ClipboardRead, FilesFetch};

    let dir = tempfile::tempdir().unwrap();
    let (nodes, broker) = explorer_desk(dir.path()).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let real = copied_files(&dir.path().join("real"));
    let user = dir.path().join("C:").join("Users").join("pc");
    let copied = shared_as(&broker, &user, &real);

    // A file or a folder the user may not read is offered by name only and
    // fails alone, as does a file in a folder; the rest comes.
    let secret = copied[1].join("cover.jpg");
    let locked = user.join("locked.txt");
    let locked_folder = user.join("locked folder");
    broker.share(&locked, &real[0]);
    broker.share(&locked_folder, &real[1]);
    {
        let mut desk = broker.desk.lock().unwrap();
        desk.denied.insert(secret);
        desk.denied.insert(locked.clone());
        desk.denied.insert(locked_folder.clone());
    }
    mac_enters_win(mac, win).await;
    broker
        .copy_files(
            vec![locked, locked_folder, copied[1].clone()],
            ClipboardRead::Empty,
        )
        .await;
    take_back(mac);
    wait_promised_names(mac, &["locked.txt", "locked folder", "photos"]).await;
    assert_eq!(
        mac.clipboard
            .promised_files()
            .unwrap()
            .iter()
            .map(|f| f.folder)
            .collect::<Vec<_>>(),
        vec![false, false, true]
    );
    let Some(FilesFetch::Ready(paths)) = paste_files(mac).await else {
        panic!("the readable files did not arrive");
    };
    assert_eq!(paths[..2], [None, None]);
    let photos = paths[2].clone().unwrap();
    assert!(!photos.join("cover.jpg").exists());
    assert_eq!(
        std::fs::read(photos.join("trip/day 1/beach.jpg")).unwrap(),
        std::fs::read(real[1].join("trip/day 1/beach.jpg")).unwrap()
    );

    // Files turned off with the Mac on the PC: the names copied with them
    // come, the files are not offered.
    win.agent.set_device_sharing("mac", false, true).unwrap();
    wait_status(win, |s| s.device("mac").is_some_and(|d| d.files_off)).await;
    let listed = broker.desk.lock().unwrap().listed.len();
    mac_enters_win(mac, win).await;
    broker
        .copy_files(
            copied.clone(),
            ClipboardRead::Content(text("report.txt\r\nphotos")),
        )
        .await;
    take_back(mac);
    wait_clipboard(mac, &text("report.txt\r\nphotos")).await;
    assert_eq!(mac.clipboard.promised_files(), None);
    // Not even looked at.
    assert_eq!(broker.desk.lock().unwrap().listed.len(), listed);

    broker.quit().await;
    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_pull_of_files_copied_in_explorer_must_name_an_offer_the_pc_made() {
    use iw_net::{PeerEndpoint, Session, hello};
    use iw_platform::ClipboardRead;
    use iw_proto::peer::v1::{ClipboardFilesPull, ClipboardSkip, Control, Os, control};

    // A device the PC trusts, speaking the protocol by hand. The PC dials
    // it, its id sorting after the PC's.
    let dir = tempfile::tempdir().unwrap();
    let pipe = dir.path().join("broker.sock");
    let rogue = PeerEndpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    )
    .unwrap();
    let known = [Known {
        id: "zed",
        name: "Rogue",
        fingerprint: rogue.fingerprint(),
        address: Some(rogue.local_addr().unwrap()),
    }];
    let spec = Spec {
        clipboard: false,
        ..win()
    };
    let win = start_with(
        &spec,
        Identity::generate().unwrap(),
        &[&spec],
        &known,
        Options {
            broker_socket: Some(pipe.clone()),
            ..Options::default()
        },
    )
    .await;
    let broker = SimBroker::connect(&pipe, &dir.path().join("Downloads")).await;
    let win_fp = win.agent.fingerprint();
    let accepted = tokio::time::timeout(TIMEOUT, async {
        rogue
            .accept()
            .await
            .unwrap()
            .finish(|fp| *fp == win_fp)
            .await
            .unwrap()
    })
    .await
    .unwrap();
    let mut session = Session::accept(
        accepted.connection,
        accepted.peer,
        hello("zed", "Rogue", Os::Linux),
    )
    .await
    .unwrap();

    // Files copied in File Explorer, offered to nobody: a pull naming any
    // offer gets nothing, and the broker is asked to look at nothing.
    let real = copied_files(&dir.path().join("real"));
    let copied = shared_as(&broker, &dir.path().join("C:").join("Users"), &real);
    broker.copy_files(copied, ClipboardRead::Empty).await;
    let pull_id = (1u64 << 63) | 1;
    let pull = Control {
        msg: Some(control::Msg::ClipboardFilesPull(ClipboardFilesPull {
            pull_id,
            offer_id: 0x1234_5678_9abc_def0,
        })),
    };
    session.control_tx.send(&pull).await.unwrap();
    let skipped = tokio::time::timeout(TIMEOUT, async {
        loop {
            let msg: Control = session.control_rx.recv().await.unwrap().unwrap();
            if let Some(control::Msg::ClipboardSkipped(skipped)) = msg.msg
                && skipped.id == pull_id
            {
                return skipped;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(skipped.reason(), ClipboardSkip::Changed);
    let opened = tokio::time::timeout(
        Duration::from_millis(300),
        session.info.accept_transfer(1024 * 1024),
    )
    .await;
    // A closed connection opened none: this hand-written peer sends no
    // heartbeats, so the agent may have given up on it meanwhile.
    assert!(
        !matches!(opened, Ok(Ok(_))),
        "the PC opened a transfer stream"
    );
    {
        let desk = broker.desk.lock().unwrap();
        assert!(desk.listed.is_empty(), "{:?}", desk.listed);
        assert!(desk.opened.is_empty());
    }
    assert!(win.agent.transfers().is_empty());

    broker.quit().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn many_pasted_files_arrive_whatever_the_transfer_list_keeps() {
    let dir = tempfile::tempdir().unwrap();
    let nodes = pasting_desk(&[mac(), win()], &dir.path().join("staging")).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // More items than the transfer list keeps once they ended.
    let source = dir.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    let copied: Vec<std::path::PathBuf> = (0..120)
        .map(|i| {
            let path = source.join(format!("file {i:03}.txt"));
            std::fs::write(&path, format!("number {i}")).unwrap();
            path
        })
        .collect();
    mac.clipboard.copy_files(copied, text("many"));
    mac_enters_win(mac, win).await;
    assert_eq!(wait_promised_files(win).await.len(), 120);

    // Pasted while the user keeps clearing the transfer list.
    let clipboard = win.clipboard.clone();
    let pasting = tokio::task::spawn_blocking(move || clipboard.paste_files());
    while !pasting.is_finished() {
        win.agent.clear_transfers();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let Some(iw_platform::FilesFetch::Ready(paths)) = pasting.await.unwrap() else {
        panic!("the pasted files did not arrive");
    };
    assert_eq!(paths.len(), 120);
    for (i, path) in paths.iter().enumerate() {
        let path = path.as_ref().unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            format!("number {i}")
        );
    }

    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// A desk like [`pasting_desk`] whose apps give up waiting for pasted
/// files at once.
async fn impatient_desk(specs: &[Spec], dir: &std::path::Path) -> Vec<Node> {
    let dir = dir.to_owned();
    desk_with(specs, move |spec| Options {
        clipboard_staging: Some(dir.join(spec.id)),
        paste_wait: Some(Duration::ZERO),
        ..Options::default()
    })
    .await
}

/// The folder files pasted on `node` from offer go into, once made.
async fn wait_staged_folder(staging: &std::path::Path) -> std::path::PathBuf {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let found = std::fs::read_dir(staging)
            .ok()
            .and_then(|mut entries| entries.next())
            .and_then(Result::ok)
            .map(|e| e.path());
        if let Some(folder) = found {
            return folder;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no folder for the paste"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn files_late_for_a_paste_come_with_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");
    let nodes = impatient_desk(&[mac(), win()], &staging).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    let copied = copied_files(&dir.path().join("source"));
    mac.clipboard.copy_files(copied.clone(), text("files"));
    mac_enters_win(mac, win).await;
    wait_promised_files(win).await;
    // The app gives up at once: that paste gets nothing.
    assert_eq!(paste_files(win).await, Some(iw_platform::FilesFetch::Busy));
    // Once they arrived, the clipboard promises them anew, and the next
    // paste gets them.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let paths = loop {
        if let Some(iw_platform::FilesFetch::Ready(paths)) = paste_files(win).await {
            break paths;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never promised anew"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let [Some(report), Some(photos)] = &paths[..] else {
        panic!("not every item arrived: {paths:?}");
    };
    assert_eq!(std::fs::read(report).unwrap(), b"quarterly numbers");
    assert!(same_tree(&copied[1], photos));
    // The promise made anew is the same offer: its folder stays.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(report.exists());
    assert_eq!(paste(win).await, text("files"));

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn files_late_for_a_paste_and_not_promised_anew_are_let_go() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");
    let nodes = impatient_desk(&[mac(), win()], &staging).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    mac.clipboard
        .copy_files(copied_files(&dir.path().join("source")), text("files"));
    mac_enters_win(mac, win).await;
    wait_promised_files(win).await;
    // The PC's clipboard now takes too little to be promised anything.
    win.clipboard.limit_writes(1);
    assert_eq!(paste_files(win).await, Some(iw_platform::FilesFetch::Busy));
    let folder = wait_staged_folder(&staging.join("win")).await;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while folder.exists() {
        assert!(tokio::time::Instant::now() < deadline, "the folder stayed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn files_are_not_offered_to_a_controller_they_are_turned_off_with() {
    let dir = tempfile::tempdir().unwrap();
    let nodes = pasting_desk(&[mac(), win()], &dir.path().join("staging")).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // Files are off with the Mac on the PC: copied there while the Mac
    // controls it, they do not come back with the cursor; the names do.
    win.agent.set_device_sharing("mac", false, true).unwrap();
    wait_status(win, |s| s.device("mac").is_some_and(|d| d.files_off)).await;
    mac_enters_win(mac, win).await;
    win.clipboard.copy_files(
        copied_files(&dir.path().join("source")),
        text("from the PC"),
    );
    take_back(mac);
    wait_clipboard(mac, &text("from the PC")).await;
    assert!(mac.clipboard.promised().is_none());
    assert_eq!(win.agent.status().lazy_clipboard.offered, 0);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_on_the_second_protocol_gets_no_files() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");
    let nodes = desk_with(&[mac(), win()], |spec| Options {
        clipboard_staging: Some(staging.join(spec.id)),
        peer_protocol_max: (spec.id == "win").then_some(2),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // It knows offers but not files in them: the names go as any small
    // clipboard.
    mac.clipboard
        .copy_files(copied_files(&dir.path().join("source")), text("names"));
    mac_enters_win(mac, win).await;
    wait_clipboard(win, &text("names")).await;
    assert!(win.clipboard.promised().is_none());

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn files_whose_names_would_change_are_left_out_of_an_offer() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");
    let nodes = pasting_desk(&[mac(), win()], &staging).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // Beside a file and a folder, names no receiver keeps as they are:
    // 100 characters in 304 bytes, which APFS takes but no other file
    // system, and a backslash, which Windows reads as a separator.
    let source = dir.path().join("source");
    let mut copied = copied_files(&source);
    for (name, data) in [
        (format!("{}.txt", "長".repeat(100)), "long"),
        ("a\\b.txt".to_owned(), "slash"),
    ] {
        let path = source.join(name);
        if std::fs::write(&path, data).is_ok() {
            copied.insert(1, path);
        }
    }
    mac.clipboard.copy_files(copied, text("four files"));
    mac_enters_win(mac, win).await;
    // The rest is promised, and the names as text too.
    wait_promised_names(win, &["report.txt", "photos"]).await;
    assert_eq!(
        win.clipboard.promised(),
        Some(vec![iw_platform::ClipboardFormat::Text])
    );
    let Some(iw_platform::FilesFetch::Ready(paths)) = paste_files(win).await else {
        panic!("the pasted files did not arrive");
    };
    let [Some(report), Some(photos)] = &paths[..] else {
        panic!("not every item arrived: {paths:?}");
    };
    assert_eq!(std::fs::read(report).unwrap(), b"quarterly numbers");
    assert!(photos.join("cover.jpg").is_file());
    // Nothing else came.
    let mut arrived: Vec<String> = std::fs::read_dir(report.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    arrived.sort();
    assert_eq!(arrived, ["photos", "report.txt"]);
    assert_eq!(paste(win).await, text("four files"));

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pasted_files_waiting_behind_others_are_not_given_up() {
    use inputweave::agent::{FileSettings, TransferState};

    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");
    // The PC gives up on files whose origin says nothing for 3 seconds
    // before the first of them came, 600 ms once they come.
    let nodes = desk_with(&[mac(), win()], |spec| Options {
        clipboard_staging: Some(staging.join(spec.id)),
        paste_stall: Some(Duration::from_millis(600)),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // The PC asks before it takes files sent to it: two of them, waiting
    // for an answer, take every turn the Mac has for it.
    win.agent.set_file_settings(FileSettings {
        folder: Some(dir.path().join("received")),
        ask: true,
    });
    let note = dir.path().join("note.txt");
    std::fs::write(&note, "hello").unwrap();
    let mut asked = Vec::new();
    for _ in 0..2 {
        asked.extend(
            mac.agent
                .send_files("win", vec![note.clone()])
                .await
                .unwrap(),
        );
    }
    for id in &asked {
        wait_transfer(win, id, |t| t.state == TransferState::WaitingForAnswer).await;
    }

    // Files copied on the Mac and pasted on the PC wait behind those,
    // longer than the PC waits on an origin that says nothing.
    let copied = copied_files(&dir.path().join("source"));
    mac.clipboard.copy_files(copied.clone(), text("files"));
    mac_enters_win(mac, win).await;
    wait_promised_files(win).await;
    let clipboard = win.clipboard.clone();
    let pasting = tokio::task::spawn_blocking(move || clipboard.paste_files());
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(!pasting.is_finished(), "the paste was given up");
    assert!(mac.agent.transfers().iter().any(|t| {
        t.state == TransferState::Queued && t.batch.as_ref().is_some_and(|b| b.clipboard.is_some())
    }));

    // Once the PC's user answers, their turn comes.
    for id in &asked {
        assert!(win.agent.answer_file_offer(id, false));
    }
    let Some(iw_platform::FilesFetch::Ready(paths)) = pasting.await.unwrap() else {
        panic!("the pasted files did not arrive");
    };
    let [Some(report), Some(photos)] = &paths[..] else {
        panic!("not every item arrived: {paths:?}");
    };
    assert_eq!(std::fs::read(report).unwrap(), b"quarterly numbers");
    assert!(same_tree(&copied[1], photos));

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn files_alone_passed_on_to_a_device_on_the_second_protocol_are_noted() {
    use iw_proto::peer::v1::ClipboardSkip;

    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");
    let nodes = desk_with(&[mac(), mini(), win()], |spec| Options {
        clipboard_staging: Some(staging.join(spec.id)),
        peer_protocol_max: (spec.id == "mini").then_some(2),
        ..Options::default()
    })
    .await;
    let (mac, mini, win) = (&nodes[0], &nodes[1], &nodes[2]);

    // Files alone copied on the PC while the Mac controls it; the cursor
    // goes on to the mini, which takes no files: the Mac, passing the
    // PC's clipboard on, notes that nothing went.
    mac_enters_win(mac, win).await;
    win.clipboard.copy_files(
        copied_files(&dir.path().join("source")),
        iw_platform::ClipboardContent::default(),
    );
    motion(mac, (0.0, 0.0), (484.0, -895.0)); // to canvas (2000, 5)
    motion(mac, (0.0, 0.0), (0.0, -10.0)); // up into the mini
    wait_status(mini, |s| s.controlled_by.as_deref() == Some("mac")).await;
    let noted = wait_status(mac, |s| {
        s.last_clipboard
            .as_ref()
            .is_some_and(|c| c.skipped == Some(ClipboardSkip::Unavailable))
    })
    .await
    .last_clipboard
    .unwrap();
    assert_eq!((noted.from.as_str(), noted.to.as_str()), ("win", "mini"));
    assert_eq!(mac.agent.status().lazy_clipboard.failed, 1);
    assert!(mini.clipboard.promised().is_none());

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pasted_files_turned_away_as_they_come_end_the_paste_at_once() {
    use inputweave::agent::{FileSettings, TransferState};

    let dir = tempfile::tempdir().unwrap();
    let nodes = pasting_desk(&[mac(), win()], &dir.path().join("staging")).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // Two files waiting for the PC's user to answer take every turn the
    // Mac has for it, so the pasted files wait behind them.
    win.agent.set_file_settings(FileSettings {
        folder: Some(dir.path().join("received")),
        ask: true,
    });
    let note = dir.path().join("note.txt");
    std::fs::write(&note, "hello").unwrap();
    let mut asked = Vec::new();
    for _ in 0..2 {
        asked.extend(
            mac.agent
                .send_files("win", vec![note.clone()])
                .await
                .unwrap(),
        );
    }
    for id in &asked {
        wait_transfer(win, id, |t| t.state == TransferState::WaitingForAnswer).await;
    }
    mac.clipboard
        .copy_files(copied_files(&dir.path().join("source")), text("files"));
    mac_enters_win(mac, win).await;
    wait_promised_files(win).await;
    let clipboard = win.clipboard.clone();
    let pasting = tokio::task::spawn_blocking(move || clipboard.paste_files());
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !mac.agent.transfers().iter().any(|t| {
        t.state == TransferState::Queued && t.batch.as_ref().is_some_and(|b| b.clipboard.is_some())
    }) {
        assert!(tokio::time::Instant::now() < deadline, "never queued");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Files turned off with the Mac on the PC meanwhile: the first of them
    // is declined for the whole batch, before it is listed. The paste ends
    // then, with nothing, rather than once the Mac was not heard of for
    // minutes.
    win.agent.set_device_sharing("mac", false, true).unwrap();
    wait_status(win, |s| s.device("mac").is_some_and(|d| d.files_off)).await;
    for id in &asked {
        win.agent.answer_file_offer(id, false);
    }
    let started = std::time::Instant::now();
    assert_eq!(
        pasting.await.unwrap(),
        Some(iw_platform::FilesFetch::Unavailable)
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    assert!(
        !win.agent
            .transfers()
            .iter()
            .any(|t| { t.batch.as_ref().is_some_and(|b| b.clipboard.is_some()) })
    );

    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// With the button pressed through `controller`, the cursor being on
/// `source`, reports a drag of `paths` from `source`, as its app does once
/// the drag nears a seam; returns the drag's id.
#[cfg(unix)]
async fn start_dragging_on(
    controller: &Node,
    source: &Node,
    paths: Vec<std::path::PathBuf>,
) -> String {
    use inputweave::agent::DragError;

    button(controller, true);
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        // The press reaches `source` through the controller; until it has,
        // there is no drag.
        match source.agent.begin_drag(paths.clone()).await {
            Ok(id) => return id,
            Err(DragError::NotDragging) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("the drag did not begin: {e}"),
        }
    }
}

/// Pushes the controller's cursor by `delta` until it is on `device`: the
/// drag's report reaches the controller on a way of its own, and until it
/// has, the held button keeps the cursor where it is.
#[cfg(unix)]
async fn push_until_on(controller: &Node, delta: (f64, f64), device: &str) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while controller.agent.status().cursor_on != device {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the cursor never got to {device}; it is on {}",
            controller.agent.status().cursor_on
        );
        motion(controller, (0.0, 0.0), delta);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Checks no drag event comes to `app` for a moment.
#[cfg(unix)]
async fn no_drag_event(app: &mut App) {
    use iw_proto::app::v1::{AgentMessage, agent_message};

    assert!(app.drags.is_empty(), "{:?}", app.drags);
    let quiet = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let msg = app.conn.recv::<AgentMessage>().await.unwrap().unwrap();
            if let Some(agent_message::Msg::Event(e)) = msg.msg
                && (e.drag_arrived.is_some() || e.drag_ended.is_some())
            {
                return e;
            }
        }
    })
    .await;
    assert!(quiet.is_err(), "a drag event came: {quiet:?}");
}

/// Files in a temporary folder to drag: a folder with a subfolder and an
/// empty one, and a file.
#[cfg(unix)]
fn files_to_drag(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let src = dir.join("src");
    tree(
        &src,
        &[
            ("Photos/a.jpg", b"aaa"),
            ("Photos/2024/b.jpg", b"bb"),
            ("notes.txt", b"hi"),
        ],
        &["Photos/empty"],
    );
    vec![src.join("Photos"), src.join("notes.txt")]
}

#[cfg(unix)]
const DRAGGED_TREE: [&str; 6] = [
    "Photos/",
    "Photos/2024/",
    "Photos/2024/b.jpg",
    "Photos/a.jpg",
    "Photos/empty/",
    "notes.txt",
];

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn files_dragged_on_a_controlled_device_come_back_to_the_controller() {
    use inputweave::agent::{DragEnd, TransferState};
    use iw_proto::app::v1::{AcceptDrag, client_message::Command, response};

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mac.sock");
    let nodes = desk_with(&[mac(), win()], |s| Options {
        ipc_socket: (s.id == "mac").then(|| socket.clone()),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let mut app = App::connect(&socket).await;
    let mut drags = win.agent.watch_drags();
    let dragged = files_to_drag(dir.path());

    // The Mac controls the PC; its user drags files there to the seam.
    mac_enters_win(mac, win).await;
    let id = start_dragging_on(mac, win, dragged).await;
    let dragging = win.agent.status().dragging.unwrap();
    assert_eq!((dragging.id.as_str(), dragging.items), (id.as_str(), 2));
    assert_eq!(dragging.device, None);

    // Back to the Mac with the button held: the PC lets go of it there,
    // and the Mac's app is offered the PC's files.
    push_until_on(mac, (-10.0, 0.0), "mac").await;
    wait_injected(win, Emulated::Button(Button::Left, false)).await;
    let event = app.drag_event().await;
    let arrived = event.drag_arrived.as_ref().unwrap();
    assert_eq!(
        (arrived.drag_id.as_str(), arrived.from_device_id.as_str()),
        (id.as_str(), "win")
    );
    assert!(arrived.at_controller);
    assert_eq!(arrived.source_device_id, "");
    let names: Vec<&str> = arrived.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["Photos", "notes.txt"]);
    wait_status(win, |s| {
        s.dragging
            .as_ref()
            .is_some_and(|d| d.device.as_deref() == Some("mac"))
    })
    .await;

    // Dropped into a folder on the Mac: the PC sends them there.
    button(mac, false);
    let desktop = dir.path().join("Desktop");
    std::fs::create_dir(&desktop).unwrap();
    let accepted = app
        .request(Command::AcceptDrag(AcceptDrag {
            drag_id: id.clone(),
            destination: desktop.to_string_lossy().into_owned(),
        }))
        .await;
    assert!(
        matches!(accepted, response::Result::Transfers(_)),
        "{accepted:?}"
    );
    let ended = drag_ended(&mut drags).await;
    assert_eq!(
        (ended.id.as_str(), ended.reason, ended.device.as_deref()),
        (id.as_str(), DragEnd::Accepted, Some("mac"))
    );
    let batch = dragged_batch(mac, &id).await;
    let received = batch_finished(mac, &batch.id).await;
    assert!(
        received.iter().all(|t| t.state == TransferState::Done),
        "{received:#?}"
    );
    assert_eq!(contents(&desktop), DRAGGED_TREE);
    assert_eq!(std::fs::read(desktop.join("notes.txt")).unwrap(), b"hi");

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn files_dragged_on_a_controlled_device_go_on_to_a_third() {
    use inputweave::agent::{DragEnd, TransferState};
    use iw_proto::app::v1::{AcceptDrag, client_message::Command, response};

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mini.sock");
    let nodes = desk_with(&[mac(), win(), mini()], |s| Options {
        ipc_socket: (s.id == "mini").then(|| socket.clone()),
        ..Options::default()
    })
    .await;
    let (mac, win, mini) = (&nodes[0], &nodes[1], &nodes[2]);
    let mut app = App::connect(&socket).await;
    let mut drags = win.agent.watch_drags();
    let dragged = files_to_drag(dir.path());

    mac_enters_win(mac, win).await;
    let id = start_dragging_on(mac, win, dragged).await;
    // Up from the PC into the mini, which is offered the PC's files.
    motion(mac, (0.0, 0.0), (484.0, -895.0));
    push_until_on(mac, (0.0, -10.0), "mini").await;
    let event = app.drag_event().await;
    let arrived = event.drag_arrived.as_ref().unwrap();
    // From the Mac, which controls the mini; the files are on the PC.
    assert_eq!(
        (
            arrived.drag_id.as_str(),
            arrived.from_device_id.as_str(),
            arrived.source_device_id.as_str()
        ),
        (id.as_str(), "mac", "win")
    );
    assert!(!arrived.at_controller);

    // Dropped there: the Mac tells the PC, which sends them to the mini.
    let desktop = dir.path().join("Desktop");
    std::fs::create_dir(&desktop).unwrap();
    drop_on(mac, mini).await;
    let accepted = app
        .request(Command::AcceptDrag(AcceptDrag {
            drag_id: id.clone(),
            destination: desktop.to_string_lossy().into_owned(),
        }))
        .await;
    assert!(
        matches!(accepted, response::Result::Transfers(_)),
        "{accepted:?}"
    );
    let ended = drag_ended(&mut drags).await;
    assert_eq!(
        (ended.id.as_str(), ended.reason, ended.device.as_deref()),
        (id.as_str(), DragEnd::Accepted, Some("mini"))
    );
    let batch = dragged_batch(mini, &id).await;
    let received = batch_finished(mini, &batch.id).await;
    assert!(
        received.iter().all(|t| t.state == TransferState::Done),
        "{received:#?}"
    );
    assert!(received.iter().all(|t| t.device == "win"), "{received:#?}");
    assert_eq!(contents(&desktop), DRAGGED_TREE);
    // Nothing went through the Mac.
    assert!(mac.agent.transfers().is_empty());

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_drag_on_a_controlled_device_brought_back_there_ends() {
    use inputweave::agent::DragEnd;
    use iw_proto::app::v1::DragEnd as AppDragEnd;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mac.sock");
    let nodes = desk_with(&[mac(), win()], |s| Options {
        ipc_socket: (s.id == "mac").then(|| socket.clone()),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let mut app = App::connect(&socket).await;
    let mut drags = win.agent.watch_drags();

    mac_enters_win(mac, win).await;
    let id = start_dragging_on(mac, win, files_to_drag(dir.path())).await;
    push_until_on(mac, (-10.0, 0.0), "mac").await;
    assert!(app.drag_event().await.drag_arrived.is_some());
    // Back onto the PC: the offer here is taken back, the button goes down
    // there again, and released there the drag ends with nothing sent.
    let presses = win
        .emulation
        .calls()
        .iter()
        .filter(|c| **c == Emulated::Button(Button::Left, true))
        .count();
    motion(mac, (1506.0, 500.0), (10.0, 0.0));
    let withdrawn = app.drag_event().await.drag_ended.unwrap();
    assert_eq!(
        (withdrawn.drag_id.as_str(), withdrawn.reason()),
        (id.as_str(), AppDragEnd::Withdrawn)
    );
    // On the PC again, as its status says.
    wait_status(win, |s| {
        s.dragging.as_ref().is_some_and(|d| d.device.is_none())
    })
    .await;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while win
        .emulation
        .calls()
        .iter()
        .filter(|c| **c == Emulated::Button(Button::Left, true))
        .count()
        == presses
    {
        assert!(tokio::time::Instant::now() < deadline, "not pressed again");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    button(mac, false);
    let ended = drag_ended(&mut drags).await;
    assert_eq!(
        (ended.id.as_str(), ended.reason),
        (id.as_str(), DragEnd::Released)
    );
    wait_status(win, |s| s.dragging.is_none()).await;
    assert!(win.agent.transfers().is_empty());
    assert!(mac.agent.transfers().is_empty());

    for node in nodes {
        node.agent.shutdown().await;
    }
}

/// Starts `specs` with only the sessions `linked` allows, each pair of
/// devices trusting each other only if it does.
#[cfg(unix)]
async fn desk_linking(
    specs: &[Spec],
    options: impl Fn(&Spec) -> Options,
    linked: impl Fn(&str, &str) -> bool,
) -> Vec<Node> {
    let identities: Vec<Identity> = specs
        .iter()
        .map(|_| Identity::generate().unwrap())
        .collect();
    let fingerprints: Vec<Fingerprint> = identities.iter().map(Identity::fingerprint).collect();
    let mut addresses: Vec<Option<SocketAddr>> = vec![None; specs.len()];
    let all: Vec<&Spec> = specs.iter().collect();
    let mut order: Vec<usize> = (0..specs.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(specs[i].id));
    let mut identities: Vec<Option<Identity>> = identities.into_iter().map(Some).collect();
    let mut nodes: Vec<Option<Node>> = (0..specs.len()).map(|_| None).collect();
    for i in order {
        let known: Vec<Known> = specs
            .iter()
            .enumerate()
            .filter(|(j, s)| *j != i && linked(specs[i].id, s.id))
            .map(|(j, s)| Known {
                id: s.id,
                name: s.name,
                fingerprint: fingerprints[j],
                address: addresses[j],
            })
            .collect();
        let node = start_with(
            &specs[i],
            identities[i].take().unwrap(),
            &all,
            &known,
            options(&specs[i]),
        )
        .await;
        addresses[i] = Some(node.agent.local_addr());
        nodes[i] = Some(node);
    }
    let nodes: Vec<Node> = nodes.into_iter().map(Option::unwrap).collect();
    for (i, node) in nodes.iter().enumerate() {
        let peers: Vec<&str> = specs
            .iter()
            .enumerate()
            .filter(|(j, s)| *j != i && linked(specs[i].id, s.id))
            .map(|(_, s)| s.id)
            .collect();
        wait_status(node, |s| {
            s.layout_ready
                && peers
                    .iter()
                    .all(|p| s.device(p).is_some_and(|d| d.connected && d.arranged))
        })
        .await;
    }
    nodes
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_drag_on_a_controlled_device_is_not_offered_where_its_files_cannot_go() {
    use inputweave::agent::DragEnd;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mini.sock");
    // The mini knows the Mac, but not the PC.
    let nodes = desk_linking(
        &[mac(), win(), mini()],
        |s| Options {
            ipc_socket: (s.id == "mini").then(|| socket.clone()),
            ..Options::default()
        },
        |a, b| a == "mac" || b == "mac",
    )
    .await;
    let (mac, win, mini) = (&nodes[0], &nodes[1], &nodes[2]);
    let mut app = App::connect(&socket).await;
    let mut drags = win.agent.watch_drags();

    mac_enters_win(mac, win).await;
    let id = start_dragging_on(mac, win, files_to_drag(dir.path())).await;
    // The cursor still goes to the mini, but nothing is offered there.
    motion(mac, (0.0, 0.0), (484.0, -895.0));
    push_until_on(mac, (0.0, -10.0), "mini").await;
    no_drag_event(&mut app).await;
    // Released there: nothing took it.
    drop_on(mac, mini).await;
    let ended = drag_ended(&mut drags).await;
    assert_eq!(
        (ended.id.as_str(), ended.reason),
        (id.as_str(), DragEnd::Released)
    );
    assert!(win.agent.transfers().is_empty());

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_device_controlled_sends_a_drag_only_where_its_controller_says() {
    use iw_net::{PeerEndpoint, Session, hello};
    use iw_proto::peer::v1::{
        Control, DragAccept, Enter, Input, Os, PointerButton, control, input,
    };

    // A device the PC trusts, speaking the protocol by hand, which takes
    // control of the PC. The PC dials it, its id sorting after the PC's.
    let rogue = PeerEndpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    )
    .unwrap();
    let known = [Known {
        id: "zed",
        name: "Rogue",
        fingerprint: rogue.fingerprint(),
        address: Some(rogue.local_addr().unwrap()),
    }];
    let spec = win();
    let win = start(&spec, Identity::generate().unwrap(), &[&spec], &known).await;
    let win_fp = win.agent.fingerprint();
    let accepted = tokio::time::timeout(TIMEOUT, async {
        rogue
            .accept()
            .await
            .unwrap()
            .finish(|fp| *fp == win_fp)
            .await
            .unwrap()
    })
    .await
    .unwrap();
    let mut session = Session::accept(
        accepted.connection,
        accepted.peer,
        hello("zed", "Rogue", Os::Linux),
    )
    .await
    .unwrap();
    assert_eq!(session.info.version, iw_proto::PEER_PROTOCOL_MAX);
    let mut input_tx = session.info.open_input().await.unwrap();
    let send_input = |msg| Input { msg: Some(msg) };
    input_tx
        .send(&send_input(input::Msg::Enter(Enter {
            x: 100.0,
            y: 100.0,
            ..Enter::default()
        })))
        .await
        .unwrap();
    input_tx
        .send(&send_input(input::Msg::Button(PointerButton {
            button: iw_proto::peer::v1::Button::Left.into(),
            pressed: true,
            x: 100.0,
            y: 100.0,
            at_pointer: false,
        })))
        .await
        .unwrap();

    // The PC's user drags a file: it is reported to the device controlling.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("secret.txt");
    std::fs::write(&file, b"only where the controller says").unwrap();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let id = loop {
        match win.agent.begin_drag(vec![file.clone()]).await {
            Ok(id) => break id,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("the drag did not begin: {e}"),
        }
    };
    let report = tokio::time::timeout(TIMEOUT, async {
        loop {
            let msg: Control = session.control_rx.recv().await.unwrap().unwrap();
            if let Some(control::Msg::RemoteDrag(report)) = msg.msg {
                return report;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(report.drag_id, id);
    assert_eq!(report.items.len(), 1);

    // An acceptance of another drag, or for a device the PC has no session
    // with, sends nothing.
    for accept in [
        DragAccept {
            drag_id: "x-1".into(),
            device_id: String::new(),
        },
        DragAccept {
            drag_id: "x-1".into(),
            device_id: "zed".into(),
        },
        DragAccept {
            drag_id: id.clone(),
            device_id: "nobody".into(),
        },
    ] {
        let msg = Control {
            msg: Some(control::Msg::DragAccept(accept)),
        };
        session.control_tx.send(&msg).await.unwrap();
    }
    let opened = tokio::time::timeout(
        Duration::from_millis(300),
        session.info.accept_transfer(1024 * 1024),
    )
    .await;
    // A closed connection opened none: this hand-written peer sends no
    // heartbeats, so the agent may have given up on it meanwhile.
    assert!(
        !matches!(opened, Ok(Ok(_))),
        "the PC opened a transfer stream"
    );
    assert!(win.agent.transfers().is_empty());
    wait_status(&win, |s| s.dragging.is_none()).await;

    win.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_carried_drag_dropped_on_a_device_waits_for_its_answer_there() {
    use inputweave::agent::{DragEnd, TransferState};
    use iw_proto::app::v1::{AcceptDrag, client_message::Command, response};

    let dir = tempfile::tempdir().unwrap();
    let mac_socket = dir.path().join("mac.sock");
    let mini_socket = dir.path().join("mini.sock");
    let nodes = desk_with(&[mac(), win(), mini()], |s| Options {
        ipc_socket: match s.id {
            "mac" => Some(mac_socket.clone()),
            "mini" => Some(mini_socket.clone()),
            _ => None,
        },
        ..Options::default()
    })
    .await;
    let (mac, win, mini) = (&nodes[0], &nodes[1], &nodes[2]);
    let mut mac_app = App::connect(&mac_socket).await;
    let mut mini_app = App::connect(&mini_socket).await;
    let mut drags = win.agent.watch_drags();

    mac_enters_win(mac, win).await;
    let id = start_dragging_on(mac, win, files_to_drag(dir.path())).await;
    motion(mac, (0.0, 0.0), (484.0, -895.0));
    push_until_on(mac, (0.0, -10.0), "mini").await;
    assert!(mini_app.drag_event().await.drag_arrived.is_some());
    drop_on(mac, mini).await;

    // Before the mini answers, the cursor goes on, back to the Mac: the
    // drop stands, and nothing is offered on the way.
    push_until_on(mac, (0.0, 10.0), "win").await;
    motion(mac, (0.0, 0.0), (-484.0, 895.0));
    push_until_on(mac, (-10.0, 0.0), "mac").await;
    no_drag_event(&mut mac_app).await;

    let desktop = dir.path().join("Desktop");
    std::fs::create_dir(&desktop).unwrap();
    let accepted = mini_app
        .request(Command::AcceptDrag(AcceptDrag {
            drag_id: id.clone(),
            destination: desktop.to_string_lossy().into_owned(),
        }))
        .await;
    assert!(
        matches!(accepted, response::Result::Transfers(_)),
        "{accepted:?}"
    );
    let ended = drag_ended(&mut drags).await;
    assert_eq!(
        (ended.id.as_str(), ended.reason, ended.device.as_deref()),
        (id.as_str(), DragEnd::Accepted, Some("mini"))
    );
    let batch = dragged_batch(mini, &id).await;
    let received = batch_finished(mini, &batch.id).await;
    assert!(
        received.iter().all(|t| t.state == TransferState::Done),
        "{received:#?}"
    );
    assert_eq!(contents(&desktop), DRAGGED_TREE);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn control_coming_back_ends_a_carried_drag() {
    use inputweave::agent::DragEnd;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mac.sock");
    let nodes = desk_with(&[mac(), win()], |s| Options {
        ipc_socket: (s.id == "mac").then(|| socket.clone()),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    let mut app = App::connect(&socket).await;
    let mut drags = win.agent.watch_drags();

    // The take-back hotkey: back here, the drag ends instead of being
    // offered here.
    mac_enters_win(mac, win).await;
    let id = start_dragging_on(mac, win, files_to_drag(dir.path())).await;
    take_back(mac);
    let ended = drag_ended(&mut drags).await;
    assert_eq!(
        (ended.id.as_str(), ended.reason),
        (id.as_str(), DragEnd::Released)
    );
    no_drag_event(&mut app).await;
    button(mac, false);

    // Someone using the PC's own mouse: the same.
    mac_enters_win(mac, win).await;
    let id = start_dragging_on(mac, win, files_to_drag(dir.path())).await;
    motion(win, (900.0, 700.0), (3.0, 0.0));
    let ended = drag_ended(&mut drags).await;
    assert_eq!(
        (ended.id.as_str(), ended.reason),
        (id.as_str(), DragEnd::Released)
    );
    wait_status(mac, |s| s.cursor_on == "mac").await;
    no_drag_event(&mut app).await;
    assert!(win.agent.transfers().is_empty());

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_source_refusing_to_send_tells_the_device_that_accepted_at_once() {
    use iw_proto::app::v1::{AcceptDrag, DragEnd as AppDragEnd, client_message::Command};

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mini.sock");
    let nodes = desk_with(&[mac(), win(), mini()], |s| Options {
        ipc_socket: (s.id == "mini").then(|| socket.clone()),
        ..Options::default()
    })
    .await;
    let (mac, win, mini) = (&nodes[0], &nodes[1], &nodes[2]);
    let mut app = App::connect(&socket).await;

    mac_enters_win(mac, win).await;
    let id = start_dragging_on(mac, win, files_to_drag(dir.path())).await;
    motion(mac, (0.0, 0.0), (484.0, -895.0));
    push_until_on(mac, (0.0, -10.0), "mini").await;
    assert!(app.drag_event().await.drag_arrived.is_some());
    // Meanwhile the PC's user turns files off with the mini.
    win.agent.set_device_sharing("mini", false, true).unwrap();
    wait_status(win, |s| s.device("mini").is_some_and(|d| d.files_off)).await;
    drop_on(mac, mini).await;
    let desktop = dir.path().join("Desktop");
    std::fs::create_dir(&desktop).unwrap();
    app.request(Command::AcceptDrag(AcceptDrag {
        drag_id: id.clone(),
        destination: desktop.to_string_lossy().into_owned(),
    }))
    .await;
    // The PC refuses; the mini hears at once, not after ten minutes.
    let ended = app.drag_event().await.drag_ended.unwrap();
    assert_eq!(
        (ended.drag_id.as_str(), ended.reason()),
        (id.as_str(), AppDragEnd::Withdrawn)
    );
    assert!(win.agent.transfers().is_empty());
    assert!(contents(&desktop).is_empty());

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_stray_drag_report_leaves_the_drag_carried_alone() {
    use iw_net::{PeerEndpoint, Session, hello};
    use iw_proto::peer::v1::{Control, Os, RemoteDrag, control};

    // A device the Mac trusts, speaking the protocol by hand; the Mac dials
    // it, its id sorting after the Mac's.
    let rogue = PeerEndpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mac.sock");
    let (mac_spec, win_spec) = (mac(), win());
    let all = [&mac_spec, &win_spec];
    let (mac_id, win_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let (mac_fp, win_fp) = (mac_id.fingerprint(), win_id.fingerprint());
    let win = start_with(
        &win_spec,
        win_id,
        &all,
        &[Known {
            id: "mac",
            name: mac_spec.name,
            fingerprint: mac_fp,
            address: None,
        }],
        Options::default(),
    )
    .await;
    let mac = start_with(
        &mac_spec,
        mac_id,
        &all,
        &[
            Known {
                id: "win",
                name: win_spec.name,
                fingerprint: win_fp,
                address: Some(win.agent.local_addr()),
            },
            Known {
                id: "zed",
                name: "Rogue",
                fingerprint: rogue.fingerprint(),
                address: Some(rogue.local_addr().unwrap()),
            },
        ],
        Options {
            ipc_socket: Some(socket.clone()),
            ..Options::default()
        },
    )
    .await;
    let accepted = tokio::time::timeout(TIMEOUT, async {
        rogue
            .accept()
            .await
            .unwrap()
            .finish(|fp| *fp == mac_fp)
            .await
            .unwrap()
    })
    .await
    .unwrap();
    let mut session = Session::accept(
        accepted.connection,
        accepted.peer,
        hello("zed", "Rogue", Os::Linux),
    )
    .await
    .unwrap();
    wait_status(&mac, |s| {
        s.layout_ready && s.device("win").is_some_and(|d| d.connected && d.arranged)
    })
    .await;
    let mut app = App::connect(&socket).await;

    // Carrying a drag from the PC (to the Mac and back, so the Mac surely
    // carries it), a report from elsewhere comes: it is turned away, and
    // the drag carried goes on.
    mac_enters_win(&mac, &win).await;
    let id = start_dragging_on(&mac, &win, files_to_drag(dir.path())).await;
    push_until_on(&mac, (-10.0, 0.0), "mac").await;
    assert!(app.drag_event().await.drag_arrived.is_some());
    motion(&mac, (1506.0, 500.0), (10.0, 0.0));
    assert!(app.drag_event().await.drag_ended.is_some());
    wait_status(&mac, |s| s.cursor_on == "win").await;
    let stray = Control {
        msg: Some(control::Msg::RemoteDrag(RemoteDrag {
            drag_id: "z-1".into(),
            ..RemoteDrag::default()
        })),
    };
    session.control_tx.send(&stray).await.unwrap();
    let cancelled = tokio::time::timeout(TIMEOUT, async {
        loop {
            let msg: Control = session.control_rx.recv().await.unwrap().unwrap();
            if let Some(control::Msg::DragCancel(cancel)) = msg.msg {
                return cancel;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(cancelled.drag_id, "z-1");
    push_until_on(&mac, (-10.0, 0.0), "mac").await;
    let event = app.drag_event().await;
    let arrived = event.drag_arrived.as_ref().unwrap();
    assert_eq!(
        (arrived.drag_id.as_str(), arrived.from_device_id.as_str()),
        (id.as_str(), "win")
    );

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_carried_drag_no_app_here_took_up_lets_go_of_nothing_here() {
    let dir = tempfile::tempdir().unwrap();
    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // No app on the Mac: back there, nothing takes the drag up, so going on
    // to the PC again lets go of nothing on the Mac.
    mac_enters_win(mac, win).await;
    start_dragging_on(mac, win, files_to_drag(dir.path())).await;
    push_until_on(mac, (-10.0, 0.0), "mac").await;
    motion(mac, (1506.0, 500.0), (10.0, 0.0));
    wait_status(mac, |s| s.cursor_on == "win").await;
    assert!(
        !mac.emulation
            .calls()
            .contains(&Emulated::Button(Button::Left, false)),
        "{:?}",
        mac.emulation.calls()
    );

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn apps_hear_whether_the_controller_can_carry_a_drag() {
    let nodes = desk_with(&[mac(), win()], |s| Options {
        // The Mac first speaks the protocol before such drags.
        peer_protocol_max: (s.id == "mac").then_some(3),
        ..Options::default()
    })
    .await;
    let (old_mac, old_win) = (&nodes[0], &nodes[1]);
    mac_enters_win(old_mac, old_win).await;
    assert!(!old_win.agent.status().to_proto().drags_carried);
    for node in nodes {
        node.agent.shutdown().await;
    }

    let nodes = desk(&[mac(), win()]).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    assert!(!win.agent.status().to_proto().drags_carried);
    mac_enters_win(mac, win).await;
    wait_status(win, |s| s.drags_carried).await;
    // Files off with the Mac: no drag goes there.
    win.agent.set_device_sharing("mac", false, true).unwrap();
    wait_status(win, |s| !s.drags_carried).await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_drag_whose_files_came_is_not_called_off_by_its_source_going() {
    use inputweave::agent::TransferState;
    use iw_proto::app::v1::{AcceptDrag, client_message::Command};

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mac.sock");
    let mut nodes = desk_with(&[mac(), win()], |s| Options {
        ipc_socket: (s.id == "mac").then(|| socket.clone()),
        ..Options::default()
    })
    .await;
    let mut app = App::connect(&socket).await;
    {
        let (mac, win) = (&nodes[0], &nodes[1]);
        mac_enters_win(mac, win).await;
        let id = start_dragging_on(mac, win, files_to_drag(dir.path())).await;
        push_until_on(mac, (-10.0, 0.0), "mac").await;
        assert!(app.drag_event().await.drag_arrived.is_some());
        button(mac, false);
        let desktop = dir.path().join("Desktop");
        std::fs::create_dir(&desktop).unwrap();
        app.request(Command::AcceptDrag(AcceptDrag {
            drag_id: id.clone(),
            destination: desktop.to_string_lossy().into_owned(),
        }))
        .await;
        let batch = dragged_batch(mac, &id).await;
        let received = batch_finished(mac, &batch.id).await;
        assert!(received.iter().all(|t| t.state == TransferState::Done));
    }
    // The PC goes to sleep afterwards: the drag that succeeded stays so.
    let win = nodes.remove(1);
    win.agent.shutdown().await;
    let mac = nodes.remove(0);
    wait_status(&mac, |s| s.device("win").is_some_and(|d| !d.connected)).await;
    no_drag_event(&mut app).await;

    mac.agent.shutdown().await;
}

#[cfg(unix)]
/// The movements `node` injected as such, added up, and how many there
/// were.
fn moved_by(node: &Node) -> ((f64, f64), usize) {
    node.emulation
        .calls()
        .iter()
        .fold(((0.0, 0.0), 0), |((x, y), n), call| match call {
            Emulated::MoveBy(dx, dy) => ((x + dx, y + dy), n + 1),
            _ => ((x, y), n),
        })
}

#[cfg(unix)]
async fn wait_moved_by(node: &Node, expected: (f64, f64)) -> usize {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let (total, count) = moved_by(node);
        if total == expected {
            return count;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for movements adding up to {expected:?}; injected: {:?}",
            node.emulation.calls()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(unix)]
fn relative_on(device: &str, automatic: bool) -> Option<inputweave::agent::RelativeMode> {
    Some(inputweave::agent::RelativeMode {
        device: device.to_owned(),
        automatic,
    })
}

/// Whether relative mode last ended on `device` for `reason`, the `ends`th
/// time, as the status tells the apps.
#[cfg(unix)]
fn relative_ended(
    status: &inputweave::agent::Status,
    ends: u64,
    device: &str,
    reason: RelativeEnd,
) -> bool {
    status.relative_ends == ends
        && status.last_relative_end
            == Some(inputweave::agent::RelativeEnded {
                device: device.to_owned(),
                reason,
            })
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn relative_mode_moves_the_pointer_as_far_as_the_mouse_and_crosses_no_edge() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    // Every third relative movement from the Mac is lost on the way.
    let nodes = desk_with(&[mac(), win()], |spec| Options {
        ipc_socket: (spec.id == "mac").then(|| socket.clone()),
        lose_relative_motion: 3,
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);

    // Not while the cursor is on the Mac.
    let output = run_ctl(&socket, &["relative", "on"]).await;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not_controlling"),
        "{output:?}"
    );

    mac_enters_win(mac, win).await;
    let output = run_ctl(&socket, &["relative", "on"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("relative mouse: on for win\n"), "{stdout}");
    assert_eq!(mac.agent.status().relative, relative_on("win", false));
    let output = run_ctl(&socket, &["status"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("relative mouse: on for win\n"), "{stdout}");
    assert!(
        stdout.contains("relative control+alt+shift+KeyG\n"),
        "{stdout}"
    );

    // Ten movements, the last far beyond the PC's left edge, towards the
    // Mac: the PC's pointer moves by all of them, lost ones too, and the
    // cursor stays there.
    let before = win.emulation.calls().len();
    for _ in 0..9 {
        motion(mac, (0.0, 0.0), (7.5, -2.25));
    }
    motion(mac, (0.0, 0.0), (-3000.0, 0.5));
    let count = wait_moved_by(win, (67.5 - 3000.0, -20.25 + 0.5)).await;
    assert!(count <= 7, "{:?}", win.emulation.calls());
    // A click goes down wherever the PC's pointer is.
    button(mac, true);
    button(mac, false);
    wait_injected(win, Emulated::Button(Button::Left, false)).await;
    let calls = win.emulation.calls();
    assert!(
        !calls[before..]
            .iter()
            .any(|c| matches!(c, Emulated::MoveTo(_))),
        "{calls:?}"
    );
    let status = mac.agent.status();
    assert_eq!(status.cursor_on, "win");
    assert!(mac.capture.is_grabbed());

    // A flick, and off with its hotkey at once: the last movement's
    // datagram (the 12th) is lost, and the end carries it.
    motion(mac, (0.0, 0.0), (1.5, 0.25));
    motion(mac, (0.0, 0.0), (1.5, 0.25));
    chord(mac, &CONTROL_ALT_SHIFT, usage::G);
    wait_status(mac, |s| s.relative.is_none()).await;
    assert!(relative_ended(
        &mac.agent.status(),
        1,
        "win",
        RelativeEnd::TurnedOff
    ));
    wait_moved_by(win, (67.5 - 3000.0 + 3.0, -20.25 + 0.5 + 0.5)).await;
    // A click right then goes where the PC's pointer is, not where relative
    // mode began; so does the cursor from then on: from (-2925.5, 880.75),
    // brought back onto the PC's screens, not (4, 900).
    let before = win.emulation.calls().len();
    button(mac, true);
    button(mac, false);
    wait_injected(win, Emulated::Button(Button::Left, false)).await;
    let calls = win.emulation.calls();
    assert!(
        !calls[before..]
            .iter()
            .any(|c| matches!(c, Emulated::MoveTo(_))),
        "{calls:?}"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    motion(mac, (0.0, 0.0), (10.0, 0.0));
    wait_injected(win, Emulated::MoveTo(Point::new(10.0, 880.75))).await;
    chord(mac, &CONTROL_ALT_SHIFT, usage::G);
    wait_status(mac, |s| s.relative == relative_on("win", false)).await;
    key_round_trip(mac, win, usage::A).await;
    assert!(
        !win.emulation
            .calls()
            .contains(&Emulated::Key(usage::G, true))
    );

    // Taking the keyboard and mouse back ends it.
    take_back(mac);
    wait_status(mac, |s| s.cursor_on == "mac" && s.relative.is_none()).await;
    assert!(relative_ended(
        &mac.agent.status(),
        2,
        "win",
        RelativeEnd::Returned
    ));
    wait_status(win, |s| s.controlled_by.is_none()).await;
    let output = run_ctl(&socket, &["status"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("relative mouse:"), "{stdout}");
    // Back on the PC, the mouse moves the cursor to places again.
    motion(mac, (1511.0, 500.0), (5.0, 0.0));
    wait_status(win, |s| s.controlled_by.as_deref() == Some("mac")).await;
    let (moved, _) = moved_by(win);
    motion(mac, (0.0, 0.0), (10.0, 0.0));
    wait_injected(win, Emulated::MoveTo(Point::new(14.0, 900.0))).await;
    assert_eq!(moved_by(win).0, moved);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_game_holding_the_pointer_puts_its_device_in_relative_mode() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let nodes = desk_with(&[mac(), win()], ipc_on_mac(&socket)).await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    assert!(mac.agent.status().relative_auto);

    // Held before the cursor comes: relative mode as it enters.
    win.status.set_pointer_captured(true);
    wait_status(win, |s| s.pointer_captured).await;
    wait_status(mac, |s| s.device("win").is_some_and(|d| d.pointer_captured)).await;
    assert_eq!(mac.agent.status().relative, None);
    mac_enters_win(mac, win).await;
    wait_status(mac, |s| s.relative == relative_on("win", true)).await;
    let output = run_ctl(&socket, &["status"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("relative mouse: on for win, as an app there holds the pointer\n")
            && stdout.contains("an app holds the pointer"),
        "{stdout}"
    );
    motion(mac, (0.0, 0.0), (-50.0, 0.0));
    wait_moved_by(win, (-50.0, 0.0)).await;

    // The game lets go, and takes it again.
    win.status.set_pointer_captured(false);
    wait_status(mac, |s| s.relative.is_none()).await;
    assert!(relative_ended(
        &mac.agent.status(),
        1,
        "win",
        RelativeEnd::Released
    ));
    win.status.set_pointer_captured(true);
    wait_status(mac, |s| s.relative == relative_on("win", true)).await;
    // Turned off by hand, it stays off while the game holds the pointer.
    mac.agent.set_relative(false).await.unwrap();
    assert_eq!(mac.agent.status().relative, None);
    assert!(relative_ended(
        &mac.agent.status(),
        2,
        "win",
        RelativeEnd::TurnedOff
    ));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(mac.agent.status().relative, None);

    // Turned off, it ends relative mode it began, and holding the pointer
    // changes nothing.
    win.status.set_pointer_captured(false);
    wait_status(mac, |s| {
        s.device("win").is_some_and(|d| !d.pointer_captured)
    })
    .await;
    win.status.set_pointer_captured(true);
    wait_status(mac, |s| s.relative == relative_on("win", true)).await;
    let output = run_ctl(&socket, &["relative", "--auto", "off"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("relative mouse: off; not by itself when an app holds the pointer\n"),
        "{output:?}"
    );
    assert_eq!(mac.agent.status().relative, None);
    assert!(relative_ended(
        &mac.agent.status(),
        3,
        "win",
        RelativeEnd::AutoOff
    ));
    win.status.set_pointer_captured(false);
    wait_status(mac, |s| {
        s.device("win").is_some_and(|d| !d.pointer_captured)
    })
    .await;
    win.status.set_pointer_captured(true);
    wait_status(mac, |s| s.device("win").is_some_and(|d| d.pointer_captured)).await;
    assert_eq!(mac.agent.status().relative, None);
    let output = run_ctl(&socket, &["status"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("relative mouse: off; not by itself when an app holds the pointer\n"),
        "{stdout}"
    );
    // By hand it still goes; the PC going away ends it.
    mac.agent.set_relative(true).await.unwrap();
    assert_eq!(mac.agent.status().relative, relative_on("win", false));
    let mut nodes = nodes;
    let win = nodes.remove(1);
    win.agent.shutdown().await;
    let mac = nodes.remove(0);
    wait_status(&mac, |s| s.cursor_on == "mac" && s.relative.is_none()).await;
    assert!(relative_ended(
        &mac.agent.status(),
        4,
        "win",
        RelativeEnd::Lost
    ));

    mac.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_says_as_it_connects_that_an_app_holds_the_pointer() {
    let (mac_spec, win_spec) = (mac(), win());
    let (mac_identity, win_identity) =
        (Identity::generate().unwrap(), Identity::generate().unwrap());
    let mut known = vec![
        Known {
            id: "mac",
            name: mac_spec.name,
            fingerprint: mac_identity.fingerprint(),
            address: None,
        },
        Known {
            id: "win",
            name: win_spec.name,
            fingerprint: win_identity.fingerprint(),
            address: None,
        },
    ];
    let all = [&mac_spec, &win_spec];
    // The game holds the pointer before the Mac connects.
    let win = start(&win_spec, win_identity, &all, &known).await;
    win.status.set_pointer_captured(true);
    wait_status(&win, |s| s.pointer_captured).await;
    known[1].address = Some(win.agent.local_addr());
    let mac = start(&mac_spec, mac_identity, &all, &known).await;
    wait_status(&mac, |s| {
        s.layout_ready
            && s.device("win")
                .is_some_and(|d| d.connected && d.arranged && d.pointer_captured)
    })
    .await;
    mac_enters_win(&mac, &win).await;
    wait_status(&mac, |s| {
        s.relative
            .as_ref()
            .is_some_and(|r| r.device == "win" && r.automatic)
    })
    .await;

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn devices_from_before_relative_mode_are_not_put_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let nodes = desk_with(&[mac(), win()], |spec| Options {
        ipc_socket: (spec.id == "mac").then(|| socket.clone()),
        peer_protocol_max: (spec.id == "win").then_some(4),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    // It does not tell that a game holds the pointer, as it does not know
    // relative mode.
    win.status.set_pointer_captured(true);
    wait_status(win, |s| s.pointer_captured).await;

    mac_enters_win(mac, win).await;
    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    assert_eq!(
        refusal(client.set_relative_mouse(true).await),
        "not_supported"
    );
    // Nor does its hotkey put it there: the key goes to the PC, and apps
    // hear why nothing happened.
    let mut app = App::connect(&socket).await;
    chord(mac, &CONTROL_ALT_SHIFT, usage::G);
    wait_injected(win, Emulated::Key(usage::G, true)).await;
    assert_eq!(app.relative_unsupported().await, "win");
    let status = mac.agent.status();
    assert_eq!(status.relative, None);
    assert!(!status.device("win").unwrap().pointer_captured);
    // Turning it off is never refused.
    client.set_relative_mouse(false).await.unwrap();
    drop(client);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

// Licenses: the agent keeps this computer's license and passes the key to
// paired devices; the app (here, the test) reports what Polar answered.

const KEY_A: &str = "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42";
const KEY_B: &str = "INPUTWEAVE-0B1C2D3E-4F50-4162-8374-95A6B7C8D9E0";

/// A device that keeps its license as `licenses` says, with no trusted
/// peers yet, remembering pairings in `devices_file`.
async fn license_keeper(
    spec: &Spec,
    identity: Identity,
    devices_file: Option<std::path::PathBuf>,
    licenses: agent::Licenses,
    protocol_max: Option<u32>,
) -> Node {
    let options = Options {
        devices_file,
        licenses,
        peer_protocol_max: protocol_max,
        ..Options::default()
    };
    start_showing(
        spec,
        identity,
        &[],
        &[],
        options,
        MockDesktopPictures::default(),
    )
    .await
}

/// Like [`license_keeper`], in memory and with every protocol version.
async fn keeper(spec: &Spec) -> Node {
    license_keeper(
        spec,
        Identity::generate().unwrap(),
        None,
        agent::Licenses::Memory,
        None,
    )
    .await
}

fn activated(id: &str) -> agent::LicenseResult {
    agent::LicenseResult::Activated {
        activation_id: id.to_owned(),
        customer_email: "someone@example.com".to_owned(),
        validated_at: 1_793_800_000,
    }
}

/// Polar refused to activate.
fn refused(refusal: agent::Refusal) -> agent::LicenseResult {
    agent::LicenseResult::Refused {
        refusal,
        activation_id: String::new(),
    }
}

/// `node` has `key`, activated there.
async fn license_with(node: &Node, key: &str) {
    node.agent.set_license_key(key).await.unwrap();
    node.agent
        .record_license_result(key, activated("act"))
        .await
        .unwrap();
}

async fn wait_license(
    node: &Node,
    ready: impl Fn(&agent::LicenseStatus) -> bool,
) -> agent::LicenseStatus {
    wait_status(node, |s| s.license.as_ref().is_some_and(&ready))
        .await
        .license
        .unwrap()
}

/// What `node` lists of `peer` among the computers on its license.
fn listed(license: &agent::LicenseStatus, peer: &str) -> Option<bool> {
    license
        .devices
        .iter()
        .find(|d| d.id == peer)
        .map(|d| d.licensed)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_license_key_passes_to_a_device_paired_with_it() {
    let win = keeper(&win()).await;
    let mac = keeper(&mac()).await;
    license_with(&mac, KEY_A).await;
    assert_eq!(
        mac.agent.status().license.unwrap().state,
        agent::LicenseState::Licensed
    );

    pair(&mac, &win).await;
    connected(&mac, "win").await;
    // The PC takes the key, for its app to activate.
    let theirs = wait_license(&win, |l| l.key == KEY_A).await;
    assert_eq!(theirs.state, agent::LicenseState::PendingActivation);
    assert_eq!(theirs.source_device_id, "mac");
    assert_eq!(theirs.source_device_name, "MacBook Pro");
    assert!(theirs.activation_id.is_empty());
    assert_eq!(listed(&theirs, "mac"), Some(true));
    // The Mac lists the PC as waiting, then as licensed.
    wait_license(&mac, |l| listed(l, "win") == Some(false)).await;
    win.agent
        .record_license_result(KEY_A, activated("act-win"))
        .await
        .unwrap();
    let mine = wait_license(&mac, |l| listed(l, "win") == Some(true)).await;
    assert_eq!(mine.devices[0].name, "Windows PC");

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

/// Waits until everything `from` sent `to` before this has been handled
/// there: `from` pauses sharing, which `to` hears on the same ordered
/// control stream as a license.
async fn heard(from: &Node, to: &Node) {
    from.agent.set_sharing(false);
    let id = from.agent.device_id().to_owned();
    wait_status(to, |s| s.device(&id).is_some_and(|d| d.sharing_paused)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_reconnects_hears_the_license_again() {
    let dir = tempfile::tempdir().unwrap();
    let mac_file = dir.path().join("mac-devices.toml");
    let mac_key = dir.path().join("mac-identity");
    let mac_spec = mac();
    let win = keeper(&win()).await;
    let mac = license_keeper(
        &mac_spec,
        Identity::load_or_generate(&mac_key).unwrap(),
        Some(mac_file.clone()),
        agent::Licenses::Memory,
        None,
    )
    .await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    mac.agent.shutdown().await;
    wait_status(&win, |s| s.device("mac").is_some_and(|d| !d.connected)).await;

    // Licensed while the Mac was away. The Mac's license was in memory
    // only, so it comes back with none, dials the PC again and hears of
    // the PC's as they connect.
    license_with(&win, KEY_A).await;
    let mac = license_keeper(
        &mac_spec,
        Identity::load_or_generate(&mac_key).unwrap(),
        Some(mac_file),
        agent::Licenses::Memory,
        None,
    )
    .await;
    connected(&mac, "win").await;
    let mine = wait_license(&mac, |l| l.key == KEY_A).await;
    assert_eq!(mine.source_device_id, "win");
    assert_eq!(mine.source_device_name, "Windows PC");

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_whose_license_was_removed_takes_no_key_until_one_is_entered() {
    let win = keeper(&win()).await;
    let mac = keeper(&mac()).await;
    license_with(&mac, KEY_A).await;
    pair(&mac, &win).await;
    wait_license(&win, |l| l.key == KEY_A).await;
    win.agent
        .record_license_result(KEY_A, activated("act-win"))
        .await
        .unwrap();

    win.agent.remove_license().await.unwrap();
    let removed = win.agent.status().license.unwrap();
    assert!(removed.opted_out);
    assert!(removed.key.is_empty());
    assert_eq!(removed.state, agent::LicenseState::None);
    // The Mac stops listing it.
    wait_license(&mac, |l| listed(l, "win").is_none()).await;
    // The Mac tells its license again, gone, waiting and then licensed;
    // the PC takes none of it.
    mac.agent.remove_license().await.unwrap();
    mac.agent.set_license_key(KEY_A).await.unwrap();
    mac.agent
        .record_license_result(KEY_A, activated("act-again"))
        .await
        .unwrap();
    heard(&mac, &win).await;
    let still = win.agent.status().license.unwrap();
    assert!(still.key.is_empty());
    assert!(still.opted_out);

    // A key entered there takes it in again.
    win.agent.set_license_key(KEY_A).await.unwrap();
    let entered = win.agent.status().license.unwrap();
    assert!(!entered.opted_out);
    assert_eq!(entered.state, agent::LicenseState::PendingActivation);
    assert!(entered.source_device_id.is_empty());

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_key_gives_way_to_a_paired_devices_licensed_one() {
    for refusal in [agent::Refusal::SeatsFull, agent::Refusal::Invalid] {
        let win = keeper(&win()).await;
        let mac = keeper(&mac()).await;
        win.agent.set_license_key(KEY_B).await.unwrap();
        win.agent
            .record_license_result(KEY_B, refused(refusal))
            .await
            .unwrap();
        license_with(&mac, KEY_A).await;
        pair(&mac, &win).await;
        let theirs = wait_license(&win, |l| l.key == KEY_A).await;
        assert_eq!(theirs.state, agent::LicenseState::PendingActivation);
        assert_eq!(theirs.source_device_id, "mac");

        mac.agent.shutdown().await;
        win.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_activated_here_is_kept_whatever_paired_devices_have() {
    let win = keeper(&win()).await;
    let mac = keeper(&mac()).await;
    license_with(&win, KEY_B).await;
    license_with(&mac, KEY_A).await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    connected(&win, "mac").await;
    heard(&mac, &win).await;
    heard(&win, &mac).await;
    // Each kept its own key, and neither lists the other, whose key
    // differs.
    let mac_after = mac.agent.status().license.unwrap();
    let win_after = win.agent.status().license.unwrap();
    assert_eq!(mac_after.key, KEY_A);
    assert_eq!(win_after.key, KEY_B);
    assert!(mac_after.devices.is_empty());
    assert!(win_after.devices.is_empty());
    assert!(win_after.source_device_id.is_empty());

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unpairing_a_device_takes_it_off_the_license() {
    let win = keeper(&win()).await;
    let mac = keeper(&mac()).await;
    license_with(&mac, KEY_A).await;
    pair(&mac, &win).await;
    wait_license(&win, |l| l.key == KEY_A).await;
    win.agent
        .record_license_result(KEY_A, activated("act-win"))
        .await
        .unwrap();
    wait_license(&mac, |l| listed(l, "win") == Some(true)).await;

    mac.agent.unpair("win").unwrap();
    let mine = wait_license(&mac, |l| l.devices.is_empty()).await;
    // Its own license stays.
    assert_eq!(mine.state, agent::LicenseState::Licensed);

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_license_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let devices = dir.path().join("devices.toml");
    let license_file = inputweave::paths::license_file(&devices);
    let spec = mac();
    let identity = dir.path().join("identity");
    let mac = license_keeper(
        &spec,
        Identity::load_or_generate(&identity).unwrap(),
        Some(devices.clone()),
        agent::Licenses::File(license_file.clone()),
        None,
    )
    .await;
    license_with(&mac, KEY_A).await;
    let before = mac.agent.status().license.unwrap();
    mac.agent.shutdown().await;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&license_file)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let mac = license_keeper(
        &spec,
        Identity::load_or_generate(&identity).unwrap(),
        Some(devices),
        agent::Licenses::File(license_file),
        None,
    )
    .await;
    let after = mac.agent.status().license.unwrap();
    assert_eq!(after, before);
    assert_eq!(after.state, agent::LicenseState::Licensed);
    assert_eq!(after.activation_id, "act");
    assert_eq!(after.customer_email, "someone@example.com");

    mac.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_license_file_that_cannot_be_read_leaves_the_license_unavailable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let devices = dir.path().join("devices.toml");
    let license_file = inputweave::paths::license_file(&devices);
    let spec = mac();
    let identity = dir.path().join("identity");
    let start = |reread: Option<Duration>| {
        let options = Options {
            devices_file: Some(devices.clone()),
            licenses: agent::Licenses::File(license_file.clone()),
            license_reread: reread,
            ..Options::default()
        };
        start_showing(
            &spec,
            Identity::load_or_generate(&identity).unwrap(),
            &[],
            &[],
            options,
            MockDesktopPictures::default(),
        )
    };
    let mac = start(None).await;
    license_with(&mac, KEY_A).await;
    mac.agent.shutdown().await;
    let saved = std::fs::read(&license_file).unwrap();
    let set_mode = |mode| {
        std::fs::set_permissions(&license_file, std::fs::Permissions::from_mode(mode)).unwrap()
    };
    set_mode(0o000);
    if std::fs::read(&license_file).is_ok() {
        // Permissions do not keep this user from reading it.
        set_mode(0o600);
        return;
    }

    // Unavailable: nothing changes, nothing is taken from or told to the
    // paired PC, and the file is not written.
    let mac = start(None).await;
    let win = keeper(&win()).await;
    license_with(&win, KEY_A).await;
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    heard(&win, &mac).await;
    heard(&mac, &win).await;
    let license = mac.agent.status().license.unwrap();
    assert_eq!(license.state, agent::LicenseState::Unavailable);
    assert_eq!(license.key, "");
    assert_eq!(listed(&win.agent.status().license.unwrap(), "mac"), None);
    assert_eq!(
        mac.agent.set_license_key(KEY_B).await,
        Err(agent::LicenseError::Unavailable)
    );
    assert_eq!(
        mac.agent.remove_license().await,
        Err(agent::LicenseError::Unavailable)
    );
    mac.agent.shutdown().await;
    set_mode(0o600);
    assert_eq!(std::fs::read(&license_file).unwrap(), saved);

    // Readable again: read off the engine's task, shown, told to the PC,
    // and what the PC said while it could not be read is taken in.
    set_mode(0o000);
    let mac = start(Some(Duration::from_millis(50))).await;
    connected(&mac, "win").await;
    set_mode(0o600);
    let license = wait_license(&mac, |l| {
        l.state == agent::LicenseState::Licensed && listed(l, "win") == Some(true)
    })
    .await;
    assert_eq!(license.activation_id, "act");
    wait_license(&win, |l| listed(l, "mac") == Some(true)).await;

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_about_a_key_no_longer_here_is_ignored() {
    let mac = keeper(&mac()).await;
    mac.agent.set_license_key(KEY_A).await.unwrap();
    mac.agent.set_license_key(KEY_B).await.unwrap();
    assert_eq!(
        mac.agent
            .record_license_result(KEY_A, activated("stale"))
            .await,
        Err(agent::LicenseError::Changed)
    );
    let license = mac.agent.status().license.unwrap();
    assert_eq!(license.key, KEY_B);
    assert_eq!(license.state, agent::LicenseState::PendingActivation);
    assert_eq!(
        mac.agent.set_license_key("not a key").await,
        Err(agent::LicenseError::InvalidKey)
    );
    // An agent that keeps no license refuses, and shows none.
    let other = unpaired(&win(), Identity::generate().unwrap(), None).await;
    assert_eq!(other.agent.status().license, None);
    assert_eq!(
        other.agent.set_license_key(KEY_A).await,
        Err(agent::LicenseError::Unsupported)
    );

    mac.agent.shutdown().await;
    other.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_on_an_older_protocol_neither_hears_nor_tells_a_license() {
    let win = license_keeper(
        &win(),
        Identity::generate().unwrap(),
        None,
        agent::Licenses::Memory,
        Some(5),
    )
    .await;
    let mac = keeper(&mac()).await;
    license_with(&win, KEY_B).await;
    // Full here: the PC's licensed key would be taken, were it heard.
    mac.agent.set_license_key(KEY_A).await.unwrap();
    mac.agent
        .record_license_result(KEY_A, refused(agent::Refusal::SeatsFull))
        .await
        .unwrap();
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    connected(&win, "mac").await;
    heard(&mac, &win).await;
    heard(&win, &mac).await;
    let mine = mac.agent.status().license.unwrap();
    assert_eq!(mine.key, KEY_A);
    assert_eq!(mine.state, agent::LicenseState::SeatsFull);
    assert!(mine.devices.is_empty());
    let theirs = win.agent.status().license.unwrap();
    assert_eq!(theirs.key, KEY_B);
    assert!(theirs.devices.is_empty());

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn ctl_enters_and_removes_a_license_over_ipc() {
    use iw_ipc::IpcConnection;
    use iw_proto::app::v1::{
        AgentMessage, ClientHello, ClientMessage, LicenseActivated, LicenseState, LicenseValidated,
        RecordLicenseResult, SetLicenseKey, agent_message, client_message::Command,
        record_license_result, response,
    };

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let options = Options {
        ipc_socket: Some(socket.clone()),
        licenses: agent::Licenses::Memory,
        ..Options::default()
    };
    let spec = mac();
    let mac = start_with(&spec, Identity::generate().unwrap(), &[&spec], &[], options).await;

    // The real binary, the key on standard input, never on the command
    // line.
    let ctl = |args: &'static [&'static str], input: &'static str| {
        let socket = socket.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_inputweave"))
                .arg("ctl")
                .args(args)
                .arg("--socket")
                .arg(&socket)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            child.wait_with_output().unwrap()
        })
    };
    let output = ctl(
        &["license", "set"],
        "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42\n",
    )
    .await
    .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "ctl failed: {output:?}");
    assert!(
        stdout.contains("license:        waiting for the app to activate it"),
        "{stdout}"
    );
    assert!(
        stdout.contains("key:            INPUTWEAVE-…7E42"),
        "{stdout}"
    );
    assert!(!stdout.contains("58D4"), "{stdout}");

    // The protocol directly, as the app reports Polar's answer.
    let mut conn = IpcConnection::connect(&socket).await.unwrap();
    let mut request = async |id: u64, command: Command| {
        conn.send(&ClientMessage {
            request_id: id,
            command: Some(command),
        })
        .await
        .unwrap();
        loop {
            let msg = conn.recv::<AgentMessage>().await.unwrap().unwrap();
            if let Some(agent_message::Msg::Response(r)) = msg.msg
                && r.request_id == id
            {
                return r.result.unwrap();
            }
        }
    };
    let hello = Command::Hello(ClientHello {
        ipc_version: iw_proto::IPC_VERSION,
        client: "test".into(),
        ..ClientHello::default()
    });
    assert!(matches!(
        request(0, hello).await,
        response::Result::Hello(_)
    ));
    let activated = |id: &str| {
        Command::RecordLicenseResult(RecordLicenseResult {
            key: KEY_A.to_owned(),
            result: Some(record_license_result::Result::Activated(LicenseActivated {
                activation_id: id.to_owned(),
                customer_email: "someone@example.com".to_owned(),
                validated_at: 1_793_800_000,
            })),
            activation_id: String::new(),
        })
    };
    let response::Result::Status(status) = request(1, activated("act")).await else {
        panic!("no status");
    };
    let license = status.license.unwrap();
    assert_eq!(license.state(), LicenseState::Licensed);
    assert_eq!(license.activation_id, "act");
    // A second activation, which the app then frees.
    let response::Result::Error(e) = request(4, activated("act-2")).await else {
        panic!("a second activation was taken");
    };
    assert_eq!(e.code, "already_licensed");
    // A confirmation of an activation this computer does not have.
    let other = Command::RecordLicenseResult(RecordLicenseResult {
        key: KEY_A.to_owned(),
        result: Some(record_license_result::Result::Validated(LicenseValidated {
            validated_at: 1_793_790_000,
            customer_email: String::new(),
        })),
        activation_id: "act-0".to_owned(),
    });
    let response::Result::Error(e) = request(5, other).await else {
        panic!("a stale confirmation was taken");
    };
    assert_eq!(e.code, "license_stale");
    let bad = Command::SetLicenseKey(SetLicenseKey {
        key: "two words".to_owned(),
    });
    let response::Result::Error(e) = request(2, bad).await else {
        panic!("a key with a space was taken");
    };
    assert_eq!(e.code, "invalid_license_key");
    let empty = Command::RecordLicenseResult(RecordLicenseResult {
        key: KEY_A.to_owned(),
        result: None,
        activation_id: String::new(),
    });
    let response::Result::Error(e) = request(3, empty).await else {
        panic!("a result without an outcome was taken");
    };
    assert_eq!(e.code, "invalid_license_result");
    drop(conn);

    let output = ctl(&["license", "remove"], "").await.unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "ctl failed: {output:?}");
    assert!(stdout.contains("license:        none (trial)"), "{stdout}");
    assert!(stdout.contains("removed here:"), "{stdout}");
    assert!(mac.agent.status().license.unwrap().opted_out);

    mac.agent.shutdown().await;
}

/// How the PC says to wake it, in the tests: over the loopback its network
/// interfaces say nothing. Its network's broadcast address is in a range
/// kept for documentation, so on no network the test runs on.
#[cfg(unix)]
fn pc_wake() -> agent::WakeData {
    agent::WakeData::new(
        [0x3c, 0x22, 0xfb, 0x10, 0x20, 0x30],
        Some(std::net::Ipv4Addr::new(198, 51, 100, 255)),
    )
    .unwrap()
}

/// Where wake-ups go in the tests, in place of the broadcast addresses.
async fn wake_receiver() -> tokio::net::UdpSocket {
    tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap()
}

/// The next `count` packets `receiver` gets.
#[cfg(unix)]
async fn wake_ups(receiver: &tokio::net::UdpSocket, count: usize) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    let mut buf = [0u8; 512];
    for _ in 0..count {
        let (n, _) = tokio::time::timeout(TIMEOUT, receiver.recv_from(&mut buf))
            .await
            .expect("timed out waiting for a wake-up")
            .unwrap();
        packets.push(buf[..n].to_vec());
    }
    packets
}

/// No packet comes for a moment.
async fn no_wake_up(receiver: &tokio::net::UdpSocket) {
    let mut buf = [0u8; 512];
    let next = tokio::time::timeout(Duration::from_millis(300), receiver.recv_from(&mut buf)).await;
    assert!(next.is_err(), "a wake-up was sent: {next:?}");
}

/// The magic packet for `mac`: six 0xFF, then the address 16 times.
#[cfg(unix)]
fn magic(mac: [u8; 6]) -> Vec<u8> {
    let mut packet = vec![0xff; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&mac);
    }
    packet
}

/// What the wake file beside the devices file `devices` holds.
#[cfg(unix)]
fn wake_kept(devices: &std::path::Path) -> inputweave::wake_file::WakeFile {
    inputweave::wake_file::WakeFile::load(&inputweave::paths::wake_file(devices))
}

/// Waits until the wake file beside the devices file `devices` holds how to
/// wake `id`; the devices file itself never does.
#[cfg(unix)]
async fn kept_how_to_wake(devices: &std::path::Path, id: &str) -> inputweave::wake_file::KnownWake {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        if let Some(known) = wake_kept(devices).devices.get(id) {
            let text = std::fs::read_to_string(devices).unwrap_or_default();
            assert!(!text.contains("wake"), "{text}");
            return *known;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} never said how to wake {id}",
            devices.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// How the Mac says to wake it, in the tests.
#[cfg(unix)]
fn mac_wake() -> agent::WakeData {
    agent::WakeData::new([0x02, 0x11, 0x22, 0x33, 0x44, 0x55], None).unwrap()
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_paired_device_says_how_to_wake_it_and_is_woken_while_offline() {
    use inputweave::agent::WakeState;
    use iw_proto::app::v1 as app;

    // The Mac sleeps and is woken; it dials, so once awake it finds the PC
    // where it was.
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    // Each in a folder of its own, as the files beside them are.
    let mac_file = dir.path().join("mac").join("devices.toml");
    let win_file = dir.path().join("win").join("devices.toml");
    let (mac_key, win_key) = (dir.path().join("mac-id"), dir.path().join("win-id"));
    let receiver = wake_receiver().await;
    let target = receiver.local_addr().unwrap();
    let win_spec = win();
    let win_options = || Options {
        ipc_socket: Some(socket.clone()),
        devices_file: Some(win_file.clone()),
        wake_target: Some(target),
        wake_wait: Some(Duration::from_secs(3)),
        ..Options::default()
    };
    let start_mac = || async {
        start_with(
            &mac(),
            Identity::load_or_generate(&mac_key).unwrap(),
            &[],
            &[],
            Options {
                devices_file: Some(mac_file.clone()),
                wake_info: Some(mac_wake()),
                ..Options::default()
            },
        )
        .await
    };
    let win = start_with(
        &win_spec,
        Identity::load_or_generate(&win_key).unwrap(),
        &[],
        &[],
        win_options(),
    )
    .await;
    let mac = start_mac().await;
    pair(&mac, &win).await;
    connected(&win, "mac").await;

    // The Mac said how to wake it; the PC keeps it. Connected, it is not
    // offered.
    let known = kept_how_to_wake(&win_file, "mac").await;
    assert_eq!(known.data, mac_wake());
    let status = win.agent.status();
    let laptop = status.device("mac").unwrap();
    assert!(!laptop.can_wake && laptop.offline_since.is_none());
    assert_eq!(laptop.wake, WakeState::None);
    // Over the loopback the PC's own interfaces say nothing.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(wake_kept(&mac_file).devices.is_empty());
    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    assert_eq!(refusal(client.wake("mac").await), "connected");
    assert_eq!(refusal(client.wake("win").await), "unknown_device");
    assert_eq!(refusal(client.wake("ghost").await), "unknown_device");

    // The Mac goes to sleep.
    let before = std::time::SystemTime::now();
    mac.agent.shutdown().await;
    let status = wait_status(&win, |s| s.device("mac").is_some_and(|d| d.can_wake)).await;
    let since = status
        .device("mac")
        .unwrap()
        .offline_since
        .expect("offline since it went");
    // When it was last heard from, kept in whole seconds: up to the
    // heartbeat timeout before it went, and the second rounded down.
    assert!(since >= before - Duration::from_secs(3), "{since:?}");
    assert!(since <= std::time::SystemTime::now(), "{since:?}");
    let proto = client.status().await.unwrap();
    let laptop = proto.devices.iter().find(|d| d.id == "mac").unwrap();
    assert!(laptop.can_wake && proto.wake_supported);
    assert!(laptop.offline_since_unix_ms > 0);
    assert_eq!(laptop.wake(), app::WakeState::None);

    // Woken over IPC, as the apps do: the magic packet goes to
    // 255.255.255.255 alone (here the test's), as the Mac named no network
    // of its own, and the Mac is being woken.
    let status = client.wake("mac").await.unwrap();
    let laptop = status.devices.iter().find(|d| d.id == "mac").unwrap();
    assert_eq!(laptop.wake(), app::WakeState::Waking);
    assert_eq!(wake_ups(&receiver, 1).await, vec![magic(mac_wake().mac())]);
    no_wake_up(&receiver).await;
    // It does not connect in time: it did not wake up.
    let status = wait_status(&win, |s| {
        s.device("mac")
            .is_some_and(|d| d.wake == WakeState::NotWoken)
    })
    .await;
    assert!(status.device("mac").unwrap().can_wake);
    let output = run_ctl(&socket, &["status"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("did not wake up; offline since"),
        "{stdout}"
    );

    // Again from the command line, as a user would; this time it wakes up
    // and connects in time.
    let output = run_ctl(&socket, &["wake", "mac"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    assert_eq!(wake_ups(&receiver, 1).await.len(), 1);
    assert_eq!(
        win.agent.status().device("mac").unwrap().wake,
        WakeState::Waking
    );
    let mac = start_mac().await;
    let status = connected(&win, "mac").await;
    let laptop = status.device("mac").unwrap();
    assert_eq!(laptop.wake, WakeState::None);
    assert!(!laptop.can_wake && laptop.offline_since.is_none());
    let output = run_ctl(&socket, &["wake", "mac"]).await;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("connected"),
        "{output:?}"
    );
    drop(client);

    // Asleep again, and the PC restarts: it still knows how, and since when.
    mac.agent.shutdown().await;
    let status = wait_status(&win, |s| s.device("mac").is_some_and(|d| d.can_wake)).await;
    let since = status.device("mac").unwrap().offline_since.unwrap();
    win.agent.shutdown().await;
    let win = start_with(
        &win_spec,
        Identity::load_or_generate(&win_key).unwrap(),
        &[],
        &[],
        win_options(),
    )
    .await;
    let laptop = win.agent.status().device("mac").cloned().unwrap();
    assert!(laptop.can_wake && laptop.wake == WakeState::None);
    let secs =
        |t: std::time::SystemTime| t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    assert_eq!(laptop.offline_since.map(secs), Some(secs(since)));

    // Unpaired, it is forgotten.
    win.agent.unpair("mac").unwrap();
    wait_status(&win, |s| s.device("mac").is_none()).await;
    assert!(wake_kept(&win_file).devices.is_empty());
    assert!(matches!(
        win.agent.wake("mac").await,
        Err(inputweave::agent::WakeError::Unknown(_))
    ));
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_never_said_how_to_wake_it_is_not_woken() {
    use inputweave::agent::WakeError;

    let receiver = wake_receiver().await;
    let target = receiver.local_addr().unwrap();
    let nodes = desk_with(&[mac(), win()], |spec| Options {
        wake_target: (spec.id == "mac").then_some(target),
        ..Options::default()
    })
    .await;
    assert_eq!(
        nodes[0].agent.wake("win").await,
        Err(WakeError::Connected("win".to_owned()))
    );
    let [mac, win] = <[Node; 2]>::try_from(nodes).ok().unwrap();
    win.agent.shutdown().await;
    let status = wait_status(&mac, |s| s.device("win").is_some_and(|d| !d.connected)).await;
    let pc = status.device("win").unwrap();
    assert!(!pc.can_wake && pc.offline_since.is_none());
    assert_eq!(
        mac.agent.wake("win").await,
        Err(WakeError::NoWakeData("win".to_owned()))
    );
    // Nor by pushing toward it.
    mac.agent.set_wake_on_push(true).await;
    assert!(mac.agent.status().wake_on_push);
    motion(&mac, (1511.0, 500.0), (5.0, 0.0));
    no_wake_up(&receiver).await;
    mac.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn pushing_the_cursor_toward_a_sleeping_device_wakes_it_when_turned_on() {
    use inputweave::agent::WakeState;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let file = dir.path().join("devices.toml");
    let receiver = wake_receiver().await;
    let target = receiver.local_addr().unwrap();
    let nodes = desk_with(&[mac(), win()], |spec| Options {
        ipc_socket: (spec.id == "mac").then(|| socket.clone()),
        devices_file: (spec.id == "mac").then(|| file.clone()),
        wake_target: (spec.id == "mac").then_some(target),
        wake_info: (spec.id == "win").then(pc_wake),
        ..Options::default()
    })
    .await;
    kept_how_to_wake(&file, "win").await;
    let [mac, win] = <[Node; 2]>::try_from(nodes).ok().unwrap();
    win.agent.shutdown().await;
    wait_status(&mac, |s| s.device("win").is_some_and(|d| d.can_wake)).await;

    // Off by default: pushing toward it wakes nothing.
    assert!(!mac.agent.status().wake_on_push);
    motion(&mac, (1511.0, 500.0), (5.0, 0.0));
    no_wake_up(&receiver).await;

    // Turned on from the command line, and remembered.
    let output = run_ctl(&socket, &["wake", "--push", "on"]).await;
    assert!(output.status.success(), "ctl failed: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("waking by pushing the cursor: on"),
        "{output:?}"
    );
    assert!(mac.agent.status().wake_on_push);
    assert!(wake_kept(&file).wake_on_push);
    motion(&mac, (1511.0, 500.0), (5.0, 0.0));
    // To 255.255.255.255 alone (here the test's): the broadcast address
    // the PC named is on no network of the Mac's.
    assert_eq!(wake_ups(&receiver, 1).await, vec![magic(pc_wake().mac())]);
    no_wake_up(&receiver).await;
    wait_status(&mac, |s| {
        s.device("win").is_some_and(|d| d.wake == WakeState::Waking)
    })
    .await;
    assert_eq!(mac.agent.status().cursor_on, "mac");
    // Again at once: not before 30 seconds.
    motion(&mac, (1511.0, 500.0), (5.0, 0.0));
    no_wake_up(&receiver).await;
    // Nor once the layout is made anew, a display plugged in on the left.
    mac.displays.set(vec![
        display("builtin", 0.0, 0.0, 1512.0, 982.0, true),
        display("external", -1920.0, 0.0, 1920.0, 1080.0, false),
    ]);
    wait_arrangement(&mac, |a| {
        a.device("mac").is_some_and(|d| d.displays.len() == 2)
    })
    .await;
    motion(&mac, (1511.0, 500.0), (5.0, 0.0));
    no_wake_up(&receiver).await;

    // Off again over IPC, as the apps do.
    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    let status = client.set_wake_on_push(false).await.unwrap();
    assert!(!status.wake_on_push);
    assert!(!wake_kept(&file).wake_on_push);
    mac.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_push_soon_after_wake_was_pressed_sends_none() {
    use inputweave::agent::WakeState;

    let receiver = wake_receiver().await;
    let target = receiver.local_addr().unwrap();
    let nodes = desk_with(&[mac(), win()], |spec| Options {
        wake_target: (spec.id == "mac").then_some(target),
        wake_info: (spec.id == "win").then(pc_wake),
        ..Options::default()
    })
    .await;
    let [mac, win] = <[Node; 2]>::try_from(nodes).ok().unwrap();
    win.agent.shutdown().await;
    wait_status(&mac, |s| s.device("win").is_some_and(|d| d.can_wake)).await;
    mac.agent.set_wake_on_push(true).await;

    // Wake pressed in an app: sent.
    mac.agent.wake("win").await.unwrap();
    assert_eq!(wake_ups(&receiver, 1).await, vec![magic(pc_wake().mac())]);
    // Pushing toward it seconds later, as the router has not woken it:
    // the wake-up that went counts, and none goes again.
    motion(&mac, (1511.0, 500.0), (5.0, 0.0));
    no_wake_up(&receiver).await;
    assert_eq!(
        mac.agent.status().device("win").unwrap().wake,
        WakeState::Waking
    );
    // Asked for again, from an app or the command line: it goes.
    mac.agent.wake("win").await.unwrap();
    assert_eq!(wake_ups(&receiver, 1).await, vec![magic(pc_wake().mac())]);
    mac.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_wake_up_that_cannot_be_sent_is_answered_once_the_status_says_so() {
    use inputweave::agent::{WakeError, WakeState};

    // An IPv6 address, from the IPv4 socket wake-ups go from: no wake-up
    // can be sent.
    let nowhere: SocketAddr = "[::1]:9".parse().unwrap();
    let nodes = desk_with(&[mac(), win()], |spec| Options {
        wake_target: (spec.id == "mac").then_some(nowhere),
        wake_info: (spec.id == "win").then(pc_wake),
        ..Options::default()
    })
    .await;
    let [mac, win] = <[Node; 2]>::try_from(nodes).ok().unwrap();
    win.agent.shutdown().await;
    wait_status(&mac, |s| s.device("win").is_some_and(|d| d.can_wake)).await;
    let refused = mac.agent.wake("win").await;
    assert!(matches!(refused, Err(WakeError::NotSent(_))), "{refused:?}");
    // The status already says so when the answer comes.
    assert_eq!(
        mac.agent.status().device("win").unwrap().wake,
        WakeState::NotWoken
    );
    mac.agent.shutdown().await;
}

/// Takes `device` offline for now from `node`, or brings it back, and waits
/// until `node` shows it so.
async fn offline(node: &Node, device: &str, on: bool) {
    node.agent.set_offline_for_now(device, on).await.unwrap();
    let here = node.agent.status().this_device == device;
    wait_status(node, |s| {
        if here {
            s.offline_by.is_some() == on
        } else {
            s.device(device).is_some_and(|d| d.offline_for_now == on)
        }
    })
    .await;
}

/// Whether `status` shows `device` taken offline for now by `by`, shown
/// offline.
fn shows_offline(status: &Status, device: &str, by: (&str, &str)) -> bool {
    if status.this_device == device {
        return status
            .offline_by
            .as_ref()
            .map(|(id, name)| (id.as_str(), name.as_str()))
            == Some(by)
            && status.devices.iter().all(|d| !d.connected);
    }
    status.device(device).is_some_and(|d| {
        d.offline_for_now
            && !d.connected
            && !d.can_wake
            && d.rtt_ms == 0
            && d.offline_by
                .as_ref()
                .map(|(id, name)| (id.as_str(), name.as_str()))
                == Some(by)
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_taken_offline_is_cut_off_from_the_whole_group_until_brought_back() {
    use inputweave::agent::OfflineChange;

    let nodes = desk(&[mac(), mini(), win()]).await;
    let (mac, mini, win) = (&nodes[0], &nodes[1], &nodes[2]);

    // Taken offline on the Mac: every device shows it so, and says who.
    mac.agent.set_offline_for_now("win", true).await.unwrap();
    let by_mac = ("mac", "MacBook Pro");
    for node in [mac, mini, win] {
        wait_status(node, |s| shows_offline(s, "win", by_mac)).await;
    }
    // Only the Mac, where it was done, and the PC, taken offline by
    // another, count it, for their apps to tell.
    let change = |device: &str, offline, by: &str| OfflineChange {
        device: device.to_owned(),
        offline,
        by: by.to_owned(),
        by_name: match by {
            "mac" => "MacBook Pro",
            "win" => "Windows PC",
            _ => "",
        }
        .to_owned(),
    };
    let status = mac.agent.status();
    assert_eq!(status.offline_changes, 1);
    assert_eq!(status.last_offline_change, Some(change("win", true, "mac")));
    let status = win.agent.status();
    assert_eq!(status.offline_changes, 1);
    assert_eq!(status.last_offline_change, Some(change("win", true, "mac")));
    assert_eq!(mini.agent.status().offline_changes, 0);
    // The others are still reached.
    assert!(mini.agent.status().device("mac").unwrap().connected);
    assert!(
        mac.agent
            .arrangement()
            .devices
            .iter()
            .any(|d| d.id == "win" && !d.connected)
    );

    // Neither way does the cursor go: not by a jump, nor across an edge.
    let refused = |device: &str| {
        Err(agent::JumpError::Refused(
            iw_core::router::JumpError::OfflineForNow(DeviceId::new(device)),
        ))
    };
    assert_eq!(mac.agent.jump_to("win").await, refused("win"));
    assert_eq!(win.agent.jump_to("mac").await, refused("win"));
    motion(win, (0.0, 500.0), (-5.0, 0.0));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(win.agent.status().cursor_on, "win");
    assert_eq!(mac.agent.status().controlled_by, None);
    // Changing another device is not the PC's to do now; unknown devices
    // are no one's.
    assert_eq!(
        win.agent.set_offline_for_now("mini", true).await,
        Err(agent::OfflineError::OfflineHere)
    );
    assert_eq!(
        mac.agent.set_offline_for_now("ghost", true).await,
        Err(agent::OfflineError::UnknownDevice("ghost".to_owned()))
    );
    // Taking it offline again changes nothing, whoever does.
    mini.agent.set_offline_for_now("win", true).await.unwrap();
    assert_eq!(mini.agent.status().offline_changes, 0);
    assert!(shows_offline(&win.agent.status(), "win", by_mac));
    // Another taken offline meanwhile: the PC, offline itself, shows it as
    // it shows every other device, just offline.
    offline(mac, "mini", true).await;
    wait_status(mini, |s| shows_offline(s, "mini", by_mac)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let pc = win.agent.status();
    let seen = pc.device("mini").unwrap();
    assert!(!seen.connected && !seen.offline_for_now && seen.offline_by.is_none());
    offline(mac, "mini", false).await;
    wait_status(mini, |s| s.offline_by.is_none()).await;

    // The Mac controls the mini all the same, and the clipboard goes with
    // the cursor between them.
    mac.clipboard.copy(text("for the mini"), false);
    mac.agent.jump_to("mini").await.unwrap();
    wait_status(mini, |s| s.controlled_by.as_deref() == Some("mac")).await;
    wait_clipboard(mini, &text("for the mini")).await;
    mac.agent.jump_to("mac").await.unwrap();
    wait_status(mini, |s| s.controlled_by.is_none()).await;

    // The PC brings itself back: every device hears it, and the Mac goes
    // over again.
    win.agent.set_offline_for_now("win", false).await.unwrap();
    for node in [mac, mini, win] {
        wait_status(node, |s| {
            s.offline_by.is_none()
                && s.devices
                    .iter()
                    .all(|d| d.connected && !d.offline_for_now && d.offline_by.is_none())
        })
        .await;
    }
    // Brought back on the PC itself, it counts there, and nowhere else.
    assert_eq!(win.agent.status().offline_changes, 2);
    assert_eq!(
        win.agent.status().last_offline_change,
        Some(change("win", false, "win"))
    );
    assert_eq!(mac.agent.status().offline_changes, 3);
    mac.agent.jump_to("win").await.unwrap();
    wait_status(win, |s| s.controlled_by.as_deref() == Some("mac")).await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_taken_offline_while_controlled_lets_go_of_every_key() {
    // Taken offline by a third device, by the controller, by itself; and
    // the controller taken offline.
    for (setter, taken) in [
        ("mini", "win"),
        ("mac", "win"),
        ("win", "win"),
        ("mini", "mac"),
    ] {
        let nodes = desk(&[mac(), mini(), win()]).await;
        let (mac, mini, win) = (&nodes[0], &nodes[1], &nodes[2]);
        mac.clipboard.copy(text("on the Mac"), false);
        mac_enters_win(mac, win).await;
        wait_clipboard(win, &text("on the Mac")).await;
        key(mac, usage::LEFT_SHIFT, true);
        key(mac, usage::A, true);
        wait_injected(win, Emulated::Key(usage::A, true)).await;
        // Copied there while controlled: coming back would bring it here.
        win.clipboard.copy(text("copied on the PC"), false);

        let node = match setter {
            "mac" => mac,
            "mini" => mini,
            _ => win,
        };
        node.agent.set_offline_for_now(taken, true).await.unwrap();
        // Everything held goes up on the PC, and the keyboard and mouse
        // come back to the Mac, which says why.
        wait_injected(win, Emulated::Key(usage::A, false)).await;
        wait_injected(win, Emulated::Key(usage::LEFT_SHIFT, false)).await;
        wait_status(win, |s| s.controlled_by.is_none()).await;
        wait_status(mac, |s| {
            s.cursor_on == "mac"
                && s.last_return == Some(ReturnReason::OfflineForNow(DeviceId::new(taken)))
        })
        .await;
        // Only the device that did it counts a change for its app to tell,
        // and the device taken offline; the Mac, controlling it, says why
        // the keyboard and mouse came back instead.
        if setter == "mini" && taken == "win" {
            assert_eq!(mac.agent.status().offline_changes, 0);
        }
        // Keys typed on the Mac now stay here.
        key(mac, usage::A, false);
        key(mac, usage::LEFT_SHIFT, false);
        key(mac, usage::C, true);
        key(mac, usage::C, false);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !win.emulation
                .calls()
                .iter()
                .any(|c| matches!(c, Emulated::Key(u, _) if *u == usage::C)),
            "{setter} took {taken} offline: {:?}",
            win.emulation.calls()
        );
        // Nor did the PC's clipboard come with the keyboard and mouse.
        assert_eq!(
            mac.clipboard.content(),
            text("on the Mac"),
            "{setter} took {taken} offline"
        );
        for node in nodes {
            node.agent.shutdown().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn taken_offline_survives_a_restart_on_every_device() {
    let dir = tempfile::tempdir().unwrap();
    // Each in a folder of its own, the PC on a fixed port so the Mac finds
    // it again after it restarts.
    let (mac_dir, win_dir) = (dir.path().join("mac"), dir.path().join("win"));
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let start_mac = || async {
        let options = kept_in(&mac_dir, "mac", mac_dir.join("received"));
        start_kept(&mac(), &mac_dir, "mac", options, None).await
    };
    let start_win = || async {
        // The port a stopped agent held is let go of a moment later.
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while std::net::UdpSocket::bind(port).is_err() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the port stayed taken"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let options = kept_in(&win_dir, "win", win_dir.join("received"));
        start_kept(&win(), &win_dir, "win", options, Some(port)).await
    };
    let (mac, win) = (start_mac().await, start_win().await);
    pair(&mac, &win).await;
    connected(&mac, "win").await;
    offline(&mac, "win", true).await;
    let by_mac = ("mac", "MacBook Pro");
    wait_status(&win, |s| shows_offline(s, "win", by_mac)).await;
    mac.agent.shutdown().await;
    win.agent.shutdown().await;
    // Each kept it in a file of its own, the devices file left as an
    // older agent reads it.
    for folder in [&mac_dir, &win_dir] {
        let doc = inputweave::offline_file::OfflineDoc::load(&folder.join("offline.toml"));
        assert!(doc.contains("win"), "{doc:?}");
        let entry = &doc.devices["win"];
        assert_eq!((entry.version, entry.author.as_str()), (1, "mac"));
        assert_eq!(entry.by.as_deref(), Some("mac"));
        let devices = std::fs::read_to_string(folder.join(format!(
            "{}-devices.toml",
            folder.file_name().unwrap().to_string_lossy()
        )))
        .unwrap();
        assert!(!devices.contains("offline"), "{devices}");
    }

    // Started again, alone: the PC is still offline for now, and the Mac
    // still shows it so.
    let win = start_win().await;
    assert!(shows_offline(&win.agent.status(), "win", by_mac));
    win.agent.shutdown().await;
    let mac = start_mac().await;
    assert!(mac.agent.status().device("win").unwrap().offline_for_now);
    let win = start_win().await;
    wait_status(&win, |s| shows_offline(s, "win", by_mac)).await;
    // Until it brings itself back.
    offline(&win, "win", false).await;
    wait_status(&mac, |s| {
        s.device("win")
            .is_some_and(|d| d.connected && !d.offline_for_now)
    })
    .await;
    mac.agent.shutdown().await;
    win.agent.shutdown().await;
    for folder in [&mac_dir, &win_dir] {
        let doc = inputweave::offline_file::OfflineDoc::load(&folder.join("offline.toml"));
        // Kept, back online, so an older entry cannot take it offline again.
        assert!(!doc.contains("win"), "{doc:?}");
        let entry = &doc.devices["win"];
        assert_eq!(
            (entry.version, entry.author.as_str(), entry.offline),
            (2, "win", false)
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_on_an_older_protocol_is_told_sharing_is_paused() {
    // The PC speaks as InputWeave 1.0 does: it knows nothing of devices
    // taken offline for now.
    let nodes = desk_with(&[mac(), win()], |spec| Options {
        peer_protocol_max: (spec.id == "win").then_some(6),
        ..Options::default()
    })
    .await;
    let (mac, win) = (&nodes[0], &nodes[1]);
    wait_status(mac, |s| s.device("win").is_some_and(|d| d.offline_unaware)).await;

    // The Mac taken offline: the PC never hears of it, but is told the Mac
    // paused, so it keeps off.
    offline(mac, "mac", true).await;
    wait_status(win, |s| s.device("mac").is_some_and(|d| d.sharing_paused)).await;
    assert_eq!(win.agent.status().offline_by, None);
    assert!(!mac.agent.status().device("win").unwrap().sharing_paused);
    motion(win, (0.0, 500.0), (-5.0, 0.0));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(mac.agent.status().controlled_by, None);
    // Back: the PC goes over again.
    offline(mac, "mac", false).await;
    wait_status(win, |s| s.device("mac").is_some_and(|d| !d.sharing_paused)).await;
    motion(win, (0.0, 500.0), (-5.0, 0.0));
    wait_status(mac, |s| s.controlled_by.as_deref() == Some("win")).await;
    key(win, usage::A, true);
    wait_injected(mac, Emulated::Key(usage::A, true)).await;

    // The PC itself taken offline while it controls the Mac: the Mac lets
    // go of it, and tells it the Mac paused, which it understands.
    offline(mac, "win", true).await;
    wait_injected(mac, Emulated::Key(usage::A, false)).await;
    wait_status(mac, |s| s.controlled_by.is_none()).await;
    wait_status(win, |s| {
        s.cursor_on == "win" && s.device("mac").is_some_and(|d| d.sharing_paused)
    })
    .await;
    key(win, usage::A, false);
    // Still listed as the one knowing nothing of it.
    assert!(mac.agent.status().device("win").unwrap().offline_unaware);

    for node in nodes {
        node.agent.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn files_stop_while_a_device_is_offline_and_go_on_once_it_is_back() {
    use inputweave::agent::{SendError, TransferState};

    let dir = tempfile::tempdir().unwrap();
    let (mac, win, _, _) = file_pair(dir.path()).await;
    let source = dir.path().join("big.bin");
    let data: Vec<u8> = (0..160_000_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();
    std::fs::write(&source, &data).unwrap();
    let id = mac
        .agent
        .send_files("win", vec![source.clone()])
        .await
        .unwrap()
        .remove(0);
    let partway = wait_transfer(&win, &id, |t| t.done > 0).await;
    assert!(
        partway.done < partway.size,
        "the transfer finished too fast to cut off"
    );

    // The PC takes itself offline midway: both sides stop, keeping what
    // arrived, and wait as for a device not connected.
    offline(&win, "win", true).await;
    let waiting =
        |t: &inputweave::agent::TransferStatus| t.state == TransferState::WaitingForConnection;
    wait_transfer(&mac, &id, waiting).await;
    let stopped = wait_transfer(&win, &id, waiting).await;
    assert!(stopped.done < stopped.size);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        wait_transfer(&mac, &id, waiting).await.state,
        TransferState::WaitingForConnection
    );
    // Nothing new goes either way.
    let small = dir.path().join("small.txt");
    std::fs::write(&small, "small").unwrap();
    for (node, to) in [(&mac, "win"), (&win, "mac")] {
        let refused = node.agent.send_files(to, vec![small.clone()]).await;
        assert!(
            matches!(refused, Err(SendError::OfflineForNow(_))),
            "{refused:?}"
        );
    }

    // Back: it goes on from where it stopped, and arrives whole.
    offline(&win, "win", false).await;
    assert_eq!(
        wait_transfer(&mac, &id, finished).await.state,
        TransferState::Done
    );
    let received = wait_transfer(&win, &id, finished).await;
    assert_eq!(std::fs::read(received.path.unwrap()).unwrap(), data);

    mac.agent.shutdown().await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_device_taken_offline_is_turned_away_when_it_enters() {
    use iw_net::{PeerEndpoint, Session, hello};
    use iw_proto::peer::v1::{Control, Enter, Input, Key, Os, control, input};

    // A device the PC trusts, speaking the protocol by hand as InputWeave
    // 1.0 does (protocol 6): it never hears it was taken offline, and
    // enters all the same. The PC dials it.
    let rogue = PeerEndpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    )
    .unwrap();
    let known = [Known {
        id: "zed",
        name: "Rogue",
        fingerprint: rogue.fingerprint(),
        address: Some(rogue.local_addr().unwrap()),
    }];
    let spec = win();
    let win = start(&spec, Identity::generate().unwrap(), &[&spec], &known).await;
    let win_fp = win.agent.fingerprint();
    let accepted = tokio::time::timeout(TIMEOUT, async {
        rogue
            .accept()
            .await
            .unwrap()
            .finish(|fp| *fp == win_fp)
            .await
            .unwrap()
    })
    .await
    .unwrap();
    let mut ours = hello("zed", "Rogue", Os::Linux);
    ours.protocol_max = 6;
    let mut session = Session::accept(accepted.connection, accepted.peer, ours)
        .await
        .unwrap();
    let mut input_tx = session.info.open_input().await.unwrap();
    let send = |msg| Input { msg: Some(msg) };
    let enter = || {
        send(input::Msg::Enter(Enter {
            x: 100.0,
            y: 100.0,
            ..Enter::default()
        }))
    };
    let a = |pressed| {
        send(input::Msg::Key(Key {
            usage: u32::from(usage::A.id()),
            pressed,
        }))
    };
    wait_status(&win, |s| s.device("zed").is_some_and(|d| d.offline_unaware)).await;

    // Taken offline on the PC: it is told the PC paused, the only thing it
    // understands of it, and never the list itself; entering, it is turned
    // away, its input dropped.
    win.agent.set_offline_for_now("zed", true).await.unwrap();
    input_tx.send(&enter()).await.unwrap();
    input_tx.send(&a(true)).await.unwrap();
    input_tx.send(&a(false)).await.unwrap();
    let (mut paused, mut took_over) = (false, 0);
    tokio::time::timeout(TIMEOUT, async {
        while !(paused && took_over > 0) {
            let msg: Control = session.control_rx.recv().await.unwrap().unwrap();
            match msg.msg {
                Some(control::Msg::SessionState(state)) => paused |= state.sharing_paused,
                Some(control::Msg::LocalActivity(_)) => took_over += 1,
                Some(control::Msg::OfflineDevices(_)) => panic!("sent to an older device"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !win.emulation
            .calls()
            .iter()
            .any(|c| matches!(c, Emulated::Key(..))),
        "{:?}",
        win.emulation.calls()
    );
    assert_eq!(win.agent.status().controlled_by, None);

    // Back: it is told the PC shares again, and controls it.
    win.agent.set_offline_for_now("zed", false).await.unwrap();
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let msg: Control = session.control_rx.recv().await.unwrap().unwrap();
            if let Some(control::Msg::SessionState(state)) = msg.msg
                && !state.sharing_paused
            {
                return;
            }
        }
    })
    .await
    .unwrap();
    input_tx.send(&enter()).await.unwrap();
    input_tx.send(&a(true)).await.unwrap();
    wait_injected(&win, Emulated::Key(usage::A, true)).await;
    wait_status(&win, |s| s.controlled_by.as_deref() == Some("zed")).await;
    input_tx.send(&a(false)).await.unwrap();
    wait_injected(&win, Emulated::Key(usage::A, false)).await;
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn documents_merge_entry_by_entry_and_the_newer_one_goes_back() {
    use iw_net::{PeerEndpoint, Session, hello};
    use iw_proto::peer::v1::{Control, OfflineDevice, OfflineDevices, Os, control};

    // A device the PC trusts, speaking the protocol by hand.
    let rogue = PeerEndpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    )
    .unwrap();
    let known = [Known {
        id: "zed",
        name: "Rogue",
        fingerprint: rogue.fingerprint(),
        address: Some(rogue.local_addr().unwrap()),
    }];
    let spec = win();
    let win = start(&spec, Identity::generate().unwrap(), &[&spec], &known).await;
    let win_fp = win.agent.fingerprint();
    let accepted = tokio::time::timeout(TIMEOUT, async {
        rogue
            .accept()
            .await
            .unwrap()
            .finish(|fp| *fp == win_fp)
            .await
            .unwrap()
    })
    .await
    .unwrap();
    let mut session = Session::accept(
        accepted.connection,
        accepted.peer,
        hello("zed", "Rogue", Os::Linux),
    )
    .await
    .unwrap();
    let entry = |id: &str, offline, version, author: &str| OfflineDevice {
        device_id: id.into(),
        offline,
        version,
        author: author.into(),
        by_device_id: if offline {
            author.into()
        } else {
            String::new()
        },
        by_name: if offline && author == "win" {
            "Windows PC".into()
        } else {
            String::new()
        },
    };
    async fn next_doc(session: &mut Session) -> OfflineDevices {
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let msg: Control = session.control_rx.recv().await.unwrap().unwrap();
                if let Some(control::Msg::OfflineDevices(doc)) = msg.msg {
                    return doc;
                }
            }
        })
        .await
        .unwrap()
    }
    let send = |doc| Control {
        msg: Some(control::Msg::OfflineDevices(OfflineDevices {
            devices: doc,
        })),
    };
    // Its first, as the session starts: none; and the other's, as a real
    // agent sends it then, which needs no answer.
    assert!(next_doc(&mut session).await.devices.is_empty());
    session.control_tx.send(&send(Vec::new())).await.unwrap();

    // The PC takes it offline; it hears.
    win.agent.set_offline_for_now("zed", true).await.unwrap();
    let doc = next_doc(&mut session).await;
    assert_eq!(doc.devices, [entry("zed", true, 1, "win")]);

    // It sends an older entry for itself and a new one for a device the PC
    // does not know: the older one loses alone, the new one is kept, and
    // the PC sends its own back, which the other lacked.
    session
        .control_tx
        .send(&send(vec![
            entry("zed", false, 0, "zed"),
            entry("ghost", true, 1, "zed"),
        ]))
        .await
        .unwrap();
    let back = next_doc(&mut session).await;
    assert_eq!(
        back.devices,
        [entry("ghost", true, 1, "zed"), entry("zed", true, 1, "win")]
    );
    let status = win.agent.status();
    assert!(status.device("zed").unwrap().offline_for_now);

    // A newer entry wins, and leaves nothing to send back.
    session
        .control_tx
        .send(&send(vec![
            entry("zed", false, 2, "zed"),
            entry("ghost", true, 1, "zed"),
        ]))
        .await
        .unwrap();
    wait_status(&win, |s| {
        s.device("zed").is_some_and(|d| !d.offline_for_now)
    })
    .await;
    // Taken offline again by a device the PC does not know, which gave no
    // name: the PC says who by id only, and no name, never the id as one.
    let mut by_stranger = entry("zed", true, 3, "stranger");
    by_stranger.by_name = String::new();
    session
        .control_tx
        .send(&send(vec![by_stranger, entry("ghost", true, 1, "zed")]))
        .await
        .unwrap();
    let status = wait_status(&win, |s| s.device("zed").is_some_and(|d| d.offline_for_now)).await;
    assert_eq!(
        status.device("zed").unwrap().offline_by,
        Some(("stranger".to_owned(), String::new()))
    );
    session
        .control_tx
        .send(&send(vec![
            entry("zed", false, 4, "zed"),
            entry("ghost", true, 1, "zed"),
        ]))
        .await
        .unwrap();
    wait_status(&win, |s| {
        s.device("zed").is_some_and(|d| !d.offline_for_now)
    })
    .await;

    // A device the group lists offline that the PC does not trust can be
    // brought back from here; one it does not know cannot be taken offline.
    win.agent.set_offline_for_now("ghost", false).await.unwrap();
    let doc = next_doc(&mut session).await;
    assert!(
        doc.devices.contains(&entry("ghost", false, 5, "win")),
        "{doc:?}"
    );
    assert_eq!(
        win.agent.set_offline_for_now("other", true).await,
        Err(agent::OfflineError::UnknownDevice("other".to_owned()))
    );
    assert_eq!(
        win.agent.set_offline_for_now("other", false).await,
        Err(agent::OfflineError::UnknownDevice("other".to_owned()))
    );
    win.agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_session_exchanges_the_document_once_each_way() {
    use inputweave::offline_file::{Entry, OfflineDoc};
    use iw_net::{PeerEndpoint, Session, hello};
    use iw_proto::peer::v1::{Control, OfflineDevices, Os, control};

    let rogue = PeerEndpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    )
    .unwrap();
    let known = [Known {
        id: "zed",
        name: "Rogue",
        fingerprint: rogue.fingerprint(),
        address: Some(rogue.local_addr().unwrap()),
    }];
    // The PC starts with a device offline that the other lacks.
    let offline = OfflineDoc {
        devices: [(
            "ghost".to_owned(),
            Entry {
                version: 3,
                author: "win".to_owned(),
                offline: true,
                by: Some("win".to_owned()),
                by_name: String::new(),
            },
        )]
        .into(),
    };
    let spec = win();
    let options = Options {
        offline: Some(offline),
        ..Options::default()
    };
    let win = start_with(
        &spec,
        Identity::generate().unwrap(),
        &[&spec],
        &known,
        options,
    )
    .await;
    let win_fp = win.agent.fingerprint();
    let accepted = tokio::time::timeout(TIMEOUT, async {
        rogue
            .accept()
            .await
            .unwrap()
            .finish(|fp| *fp == win_fp)
            .await
            .unwrap()
    })
    .await
    .unwrap();
    let mut session = Session::accept(
        accepted.connection,
        accepted.peer,
        hello("zed", "Rogue", Os::Linux),
    )
    .await
    .unwrap();
    let empty = || Control {
        msg: Some(control::Msg::OfflineDevices(OfflineDevices::default())),
    };
    // Each side's own, as the session starts; the other's answers nothing
    // though it lacks the PC's entry, which it has just been sent.
    session.control_tx.send(&empty()).await.unwrap();
    async fn docs(session: &mut Session, wait: Duration) -> Vec<OfflineDevices> {
        let mut docs = Vec::new();
        let _ = tokio::time::timeout(wait, async {
            loop {
                let msg: Control = session.control_rx.recv().await.unwrap().unwrap();
                if let Some(control::Msg::OfflineDevices(doc)) = msg.msg {
                    docs.push(doc);
                }
            }
        })
        .await;
        docs
    }
    let opening = docs(&mut session, Duration::from_millis(700)).await;
    assert_eq!(opening.len(), 1, "{opening:?}");
    assert_eq!(opening[0].devices.len(), 1);
    // Later in the session, one that lacks it is answered.
    session.control_tx.send(&empty()).await.unwrap();
    let answer = docs(&mut session, Duration::from_millis(700)).await;
    assert_eq!(answer.len(), 1, "{answer:?}");
    win.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_device_taken_offline_while_being_woken_is_not_woken_any_more() {
    use inputweave::agent::WakeState;

    let receiver = wake_receiver().await;
    let target = receiver.local_addr().unwrap();
    let nodes = desk_with(&[mac(), win()], |spec| Options {
        wake_target: (spec.id == "mac").then_some(target),
        wake_info: (spec.id == "win").then(pc_wake),
        ..Options::default()
    })
    .await;
    let [mac, win] = <[Node; 2]>::try_from(nodes).ok().unwrap();
    wait_status(&mac, |s| s.device("win").is_some_and(|d| d.connected)).await;
    // Told how to wake it before it went.
    tokio::time::sleep(Duration::from_millis(300)).await;
    win.agent.shutdown().await;
    wait_status(&mac, |s| s.device("win").is_some_and(|d| d.can_wake)).await;
    mac.agent.wake("win").await.unwrap();
    assert_eq!(
        mac.agent.status().device("win").unwrap().wake,
        WakeState::Waking
    );

    // Taken offline for now meanwhile: neither waking nor wakeable, and
    // waking it is given up, so back online it is just offline.
    offline(&mac, "win", true).await;
    let pc = mac.agent.status().device("win").unwrap().clone();
    assert_eq!((pc.wake, pc.can_wake), (WakeState::None, false));
    assert!(matches!(
        mac.agent.wake("win").await,
        Err(agent::WakeError::OfflineForNow(_))
    ));
    // Not connected, and offline for now: a jump says the latter, as the
    // apps show it.
    assert_eq!(
        mac.agent.jump_to("win").await,
        Err(agent::JumpError::Refused(
            iw_core::router::JumpError::OfflineForNow(DeviceId::new("win"))
        ))
    );
    offline(&mac, "win", false).await;
    let pc = mac.agent.status().device("win").unwrap().clone();
    assert_eq!((pc.wake, pc.can_wake), (WakeState::None, true));
    mac.agent.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn ctl_takes_a_device_offline_and_brings_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let nodes = desk_with(&[mac(), win()], ipc_on_mac(&socket)).await;
    let win = &nodes[1];

    let output = run_ctl(&socket, &["offline", "win", "on"]).await;
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("offline for now") && stdout.contains("set on this device"),
        "{stdout}"
    );
    wait_status(win, |s| shows_offline(s, "win", ("mac", "MacBook Pro"))).await;
    let mut client = inputweave::ctl::Client::connect(&socket).await.unwrap();
    assert_eq!(refusal(client.jump_to("win").await), "offline_for_now");
    let status = client.status().await.unwrap();
    let pc = status.devices.iter().find(|d| d.id == "win").unwrap();
    assert!(pc.offline_for_now && pc.offline_by_device_id == "mac");
    assert_eq!(pc.state(), iw_proto::app::v1::ConnectionState::Offline);
    assert_eq!(
        status.last_return(),
        iw_proto::app::v1::ReturnReason::Unspecified
    );
    assert_eq!(status.offline_changes, 1);
    assert!(status.offline_supported && !status.offline_for_now);

    // Back, then this device itself: every other device shows offline,
    // and it changes none of them until it is back.
    let output = run_ctl(&socket, &["offline", "win", "off"]).await;
    assert!(output.status.success(), "{output:?}");
    let output = run_ctl(&socket, &["offline", "mac", "on"]).await;
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("offline:        for now, set on this device"),
        "{stdout}"
    );
    let status = client.status().await.unwrap();
    assert!(status.offline_for_now && status.offline_by_device_id == "mac");
    assert!(
        status
            .devices
            .iter()
            .all(|d| d.state() == iw_proto::app::v1::ConnectionState::Offline)
    );
    let output = run_ctl(&socket, &["offline", "win", "on"]).await;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("offline_for_now"),
        "{output:?}"
    );
    let output = run_ctl(&socket, &["offline", "mac", "off"]).await;
    assert!(output.status.success(), "{output:?}");
    wait_status(win, |s| s.device("mac").is_some_and(|d| d.connected)).await;

    for node in nodes {
        node.agent.shutdown().await;
    }
}
