//! Made-up state for screenshots (`inputweave-app --screenshot`), as the
//! Windows app's `--show-sample-devices` and `--show-sample-transfers`.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iw_proto::app::v1::{
    ArrangedDevice, Arrangement, BackendRole, ClipboardActivity, ConnectionState, DeviceStatus,
    FailedFile, FileTransfer, FirewallState, FirewallStatus, NearbyDevice, Network, NetworkKind,
    PairingPhase, PairingState, Permission, PermissionAsk, PermissionStatus, Profile, ProfileKind,
    Screen, SessionBackend, SessionInfo, SessionType, Status, TransferState, Transfers, WakeState,
};

use crate::shortcut;
use crate::snapshot::{Link, Snapshot};

fn ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn device(
    id: &str,
    name: &str,
    os: &str,
    state: ConnectionState,
    rtt: u32,
    slot: u32,
) -> DeviceStatus {
    DeviceStatus {
        id: id.into(),
        name: name.into(),
        state: state as i32,
        paired: true,
        os: os.into(),
        rtt_ms: rtt,
        jump_slot: slot,
        reported_name: name.into(),
        lock_screen_input: true,
        speed_percent: 100,
        ..Default::default()
    }
}

/// A network as a profile lists it.
fn network(id: &str, name: &str, kind: NetworkKind, router: &str) -> Network {
    Network {
        id: id.into(),
        name: name.into(),
        kind: kind as i32,
        router: router.into(),
        resolved: false,
    }
}

/// `network` as the one this computer is on, known by more than its
/// router's address, so it can be added to a profile.
fn current(network: Network) -> Network {
    Network {
        resolved: true,
        ..network
    }
}

/// The office's Wi-Fi, the network this computer is on in the sample.
fn office_wifi() -> Network {
    network("n-office", "Office-5G", NetworkKind::Wifi, "172.16.0.1")
}

/// Default (with the home network, known by its router alone), the user's
/// Office (with the office's Wi-Fi) and Other networks, every computer
/// connected in each.
fn profiles() -> Vec<Profile> {
    let profile = |id: &str, kind: ProfileKind, name: &str, networks: Vec<Network>| Profile {
        id: id.into(),
        kind: kind as i32,
        name: name.into(),
        paused: false,
        off_device_ids: Vec::new(),
        clipboard: true,
        files: true,
        networks,
    };
    vec![
        profile(
            "default",
            ProfileKind::Default,
            "",
            vec![network("n-home", "", NetworkKind::Wired, "192.168.1.1")],
        ),
        profile("office", ProfileKind::Custom, "Office", vec![office_wifi()]),
        profile("other", ProfileKind::OtherNetworks, "", Vec::new()),
    ]
}

fn screen(id: &str, x: f64, width: f64, height: f64, scale: f64, primary: bool) -> Screen {
    Screen {
        id: id.into(),
        x,
        y: 0.0,
        width,
        height,
        scale,
        primary,
        preview_jpeg: Vec::new(),
    }
}

pub fn snapshot(now: SystemTime) -> Snapshot {
    let mut studio = device(
        "pc",
        "Studio PC",
        "windows",
        ConnectionState::Connected,
        5,
        3,
    );
    studio.reported_name = "DESKTOP-4F2K".into();
    // Asleep, and it said how to wake it before it went.
    let mut laptop = device(
        "laptop",
        "ThinkPad",
        "linux",
        ConnectionState::Offline,
        0,
        0,
    );
    laptop.can_wake = true;
    laptop.offline_since_unix_ms = ms(now - Duration::from_secs(2 * 3600 + 300));
    let devices = vec![
        device("mac", "Mac mini", "macos", ConnectionState::Connected, 3, 1),
        studio,
        laptop,
    ];
    let mut hotkeys = shortcut::defaults();
    hotkeys.relative = Some(shortcut::DEFAULT_RELATIVE.into());
    let status = Status {
        sharing: true,
        this_device_id: "me".into(),
        this_device_name: "ubuntu-desk".into(),
        devices,
        capturing: true,
        clipboard_sync: true,
        last_clipboard: Some(ClipboardActivity {
            at_unix_ms: ms(now - Duration::from_secs(70)),
            from_device_id: "mac".into(),
            to_device_id: "me".into(),
            kinds: vec!["text".into()],
            skipped: String::new(),
        }),
        default_dwell_ms: 150,
        preview_sharing: true,
        hotkeys: Some(hotkeys),
        relative_auto: true,
        relative_supported: true,
        wake_supported: true,
        offline_supported: true,
        profiles_supported: true,
        profiles: profiles(),
        profile_id: "office".into(),
        network: Some(current(office_wifi())),
        ..Default::default()
    };
    let arranged = |id: &str,
                    name: &str,
                    os: &str,
                    x: f64,
                    screens: Vec<Screen>,
                    connected: bool,
                    slot: u32,
                    this: bool| ArrangedDevice {
        id: id.into(),
        name: name.into(),
        os: os.into(),
        placed: true,
        x,
        y: 0.0,
        screens,
        connected,
        this_device: this,
        jump_slot: slot,
    };
    let arrangement = Arrangement {
        version: 7,
        devices: vec![
            arranged(
                "mac",
                "Mac mini",
                "macos",
                -1512.0,
                vec![screen("1", 0.0, 1512.0, 982.0, 2.0, true)],
                true,
                1,
                false,
            ),
            arranged(
                "me",
                "ubuntu-desk",
                "linux",
                0.0,
                vec![
                    screen("DP-1", 0.0, 2560.0, 1440.0, 1.0, true),
                    screen("HDMI-1", 2560.0, 1920.0, 1080.0, 1.0, false),
                ],
                true,
                2,
                true,
            ),
            arranged(
                "pc",
                "Studio PC",
                "windows",
                4480.0,
                vec![screen("\\\\.\\DISPLAY1", 0.0, 1920.0, 1080.0, 1.0, true)],
                true,
                3,
                false,
            ),
            ArrangedDevice {
                placed: false,
                ..arranged("laptop", "ThinkPad", "linux", 0.0, vec![], false, 0, false)
            },
        ],
    };
    let started = ms(now - Duration::from_secs(40));
    let transfers = Transfers {
        transfers: vec![
            FileTransfer {
                transfer_id: "t-offer".into(),
                device_id: "pc".into(),
                name: "Quarterly report.pdf".into(),
                size: 2_340_000,
                state: TransferState::WaitingForAnswer as i32,
                started_at_unix_ms: started,
                batch_id: "b-offer".into(),
                batch_items: 1,
                batch_files: 1,
                files: 1,
                ..Default::default()
            },
            FileTransfer {
                transfer_id: "t-photos".into(),
                outgoing: true,
                device_id: "mac".into(),
                name: "Photos".into(),
                size: 1_200_000_000,
                done_bytes: 420_000_000,
                bytes_per_second: 38_000_000,
                state: TransferState::Transferring as i32,
                path: "/home/me/Pictures/Photos".into(),
                started_at_unix_ms: started,
                batch_id: "b-photos".into(),
                folder: true,
                files: 312,
                files_done: 108,
                ..Default::default()
            },
            FileTransfer {
                transfer_id: "t-design".into(),
                device_id: "mac".into(),
                name: "Design".into(),
                size: 84_000_000,
                done_bytes: 84_000_000,
                state: TransferState::Done as i32,
                path: "/home/me/Downloads/InputWeave/Design".into(),
                started_at_unix_ms: started,
                batch_id: "b-design".into(),
                folder: true,
                files: 46,
                files_done: 44,
                files_failed: 2,
                failures: vec![
                    FailedFile {
                        relative_path: "Design/raw/IMG_0001.CR3".into(),
                        reason: "corrupt".into(),
                    },
                    FailedFile {
                        relative_path: "Design/raw/IMG_0007.CR3".into(),
                        reason: "write_failed".into(),
                    },
                ],
                ..Default::default()
            },
            FileTransfer {
                transfer_id: "t-notes".into(),
                device_id: "mac".into(),
                name: "notes.txt".into(),
                size: 12_400,
                done_bytes: 12_400,
                state: TransferState::Done as i32,
                path: "/home/me/Downloads/InputWeave/notes.txt".into(),
                started_at_unix_ms: started,
                batch_id: "b-notes".into(),
                files: 1,
                files_done: 1,
                ..Default::default()
            },
            FileTransfer {
                transfer_id: "t-video".into(),
                outgoing: true,
                device_id: "pc".into(),
                name: "screen-recording.mkv".into(),
                size: 6_100_000_000,
                state: TransferState::Declined as i32,
                reason: "no_space".into(),
                missing_bytes: 2_400_000_000,
                started_at_unix_ms: started,
                batch_id: "b-video".into(),
                files: 1,
                ..Default::default()
            },
        ],
        receive_folder: "/home/me/Downloads/InputWeave".into(),
        ask_before_receiving: true,
    };
    Snapshot {
        link: Link::Connected,
        status,
        nearby: vec![
            NearbyDevice {
                id: "air".into(),
                name: "MacBook Air".into(),
                os: "macos".into(),
                address: "192.168.1.24:47820".into(),
                paired: false,
                pairable: true,
            },
            NearbyDevice {
                id: "office".into(),
                name: "OFFICE-PC".into(),
                os: "windows".into(),
                address: "192.168.1.31:47820".into(),
                paired: false,
                pairable: false,
            },
        ],
        arrangement: Arc::new(arrangement),
        transfers: Arc::new(transfers),
        agent_version: env!("CARGO_PKG_VERSION").into(),
        agent_digest: "3f1c0a9b7e5d2c4f6a8b0c1d2e3f405162738495a6b7c8d9e0f1a2b3c4d5e6f7".into(),
        ..Default::default()
    }
}

/// The sample with the Mac mini asleep on the arrangement, in `wake`, and
/// the ThinkPad as one that never said how to wake it.
pub fn asleep(now: SystemTime, wake: WakeState) -> Snapshot {
    let mut model = snapshot(now);
    for device in &mut model.status.devices {
        match device.id.as_str() {
            "mac" => {
                device.state = ConnectionState::Offline as i32;
                device.rtt_ms = 0;
                device.can_wake = true;
                device.wake = wake as i32;
                device.offline_since_unix_ms = ms(now - Duration::from_secs(26 * 3600));
            }
            "laptop" => {
                device.can_wake = false;
                device.offline_since_unix_ms = 0;
            }
            _ => {}
        }
    }
    let mut arrangement = (*model.arrangement).clone();
    for device in &mut arrangement.devices {
        if device.id == "mac" {
            device.connected = false;
        }
    }
    model.arrangement = Arc::new(arrangement);
    model
}

/// The sample with a fourth computer, connected but with file transfer
/// turned off, for the picker of where files go: the Mac mini and the
/// Studio PC can take them, the ThinkPad is offline.
pub fn sending(now: SystemTime) -> Snapshot {
    let mut model = snapshot(now);
    let mut media = device(
        "media",
        "Living Room PC",
        "windows",
        ConnectionState::Connected,
        7,
        0,
    );
    media.files_off = true;
    model.status.devices.push(media);
    model
}

/// The state while this computer shows a pairing code.
pub fn showing_code(now: SystemTime) -> PairingState {
    PairingState {
        phase: PairingPhase::ShowCode as i32,
        peer_device_id: "air".into(),
        peer_name: "MacBook Air".into(),
        code: "482913".into(),
        expires_at_unix_ms: ms(now + Duration::from_secs(274)),
        ..Default::default()
    }
}

/// The state while the code shown on another computer is typed here.
pub fn entering_code() -> PairingState {
    PairingState {
        phase: PairingPhase::EnterCode as i32,
        peer_device_id: "air".into(),
        peer_name: "MacBook Air".into(),
        attempts_left: 3,
        ..Default::default()
    }
}

/// The sample state on a GNOME Wayland desktop: remote control allowed,
/// input capture not yet (so the cursor leaves only across edges, and no
/// jumps), the clipboard waiting for remote control, ufw keeping the
/// agent's port shut, and the PC paused sharing; for the troubleshooting
/// page, the jumps' explanations and setup's permissions step.
pub fn wayland(now: SystemTime) -> Snapshot {
    let mut model = snapshot(now);
    let backend = |role: BackendRole, implementation: &str, problem: &str| SessionBackend {
        role: role as i32,
        implementation: implementation.into(),
        problem: problem.into(),
    };
    let status = &mut model.status;
    for device in &mut status.devices {
        device.sharing_paused = device.id == "pc";
    }
    status.capturing = false;
    status.cursor_leaves_at_edges_only = true;
    status.session = Some(SessionInfo {
        r#type: SessionType::Wayland as i32,
        desktop: "ubuntu:GNOME".into(),
        backends: vec![
            backend(BackendRole::Input, "RemoteDesktop portal", ""),
            backend(
                BackendRole::Capture,
                "InputCapture portal",
                "input capture not allowed yet",
            ),
            backend(
                BackendRole::Clipboard,
                "Clipboard portal",
                "only while remote control runs",
            ),
            backend(BackendRole::Displays, "xdg-output", ""),
            backend(BackendRole::DesktopPictures, "GNOME settings", ""),
        ],
    });
    status.firewall = Some(FirewallStatus {
        state: FirewallState::Blocks as i32,
        port: 47820,
        blocked_ports: vec![47820],
    });
    status.permissions = vec![
        PermissionStatus {
            permission: Permission::RemoteControl as i32,
            granted: true,
            ask: PermissionAsk::Askable as i32,
            asked: 1,
            failure: String::new(),
        },
        PermissionStatus {
            permission: Permission::InputCapture as i32,
            granted: false,
            ask: PermissionAsk::Askable as i32,
            asked: 0,
            failure: String::new(),
        },
    ];
    model
}

/// The sample with the Mac mini taken offline for now, from this computer
/// (`from_here`) or by the ThinkPad, with it and the Studio PC still on a
/// version that knows nothing of it.
pub fn offline_for_now(now: SystemTime, from_here: bool) -> Snapshot {
    let mut model = snapshot(now);
    let (by, by_name) = if from_here {
        ("me", "ubuntu-desk")
    } else {
        ("laptop", "ThinkPad")
    };
    for device in &mut model.status.devices {
        match device.id.as_str() {
            "mac" => {
                device.state = ConnectionState::Offline as i32;
                device.rtt_ms = 0;
                device.offline_for_now = true;
                device.offline_by_device_id = by.into();
                device.offline_by_name = by_name.into();
                device.offline_unaware = true;
            }
            "pc" => device.offline_unaware = true,
            _ => {}
        }
    }
    let mut arrangement = (*model.arrangement).clone();
    for device in &mut arrangement.devices {
        if device.id == "mac" {
            device.connected = false;
        }
    }
    model.arrangement = Arc::new(arrangement);
    model
}

/// The sample with this computer taken offline for now by the Mac mini:
/// every other computer offline, and none to wake.
pub fn this_offline(now: SystemTime) -> Snapshot {
    let mut model = snapshot(now);
    model.status.offline_for_now = true;
    model.status.offline_by_device_id = "mac".into();
    model.status.offline_by_name = "Mac mini".into();
    model.status.last_clipboard = None;
    for device in &mut model.status.devices {
        device.state = ConnectionState::Offline as i32;
        device.rtt_ms = 0;
        device.can_wake = false;
        device.offline_since_unix_ms = 0;
    }
    let mut arrangement = (*model.arrangement).clone();
    for device in &mut arrangement.devices {
        if !device.this_device {
            device.connected = false;
        }
    }
    model.arrangement = Arc::new(arrangement);
    model
}

/// The sample with the Studio PC left out of the profile in use (Office).
pub fn not_in_profile(now: SystemTime) -> Snapshot {
    let mut model = snapshot(now);
    for profile in &mut model.status.profiles {
        if profile.id == "office" {
            profile.off_device_ids = vec!["pc".into()];
        }
    }
    for device in &mut model.status.devices {
        if device.id == "pc" {
            device.state = ConnectionState::Offline as i32;
            device.rtt_ms = 0;
            device.not_in_profile = true;
        }
    }
    let mut arrangement = (*model.arrangement).clone();
    for device in &mut arrangement.devices {
        if device.id == "pc" {
            device.connected = false;
        }
    }
    model.arrangement = Arc::new(arrangement);
    model
}

/// The sample at a café whose network is in no profile, so the profile in
/// use is Other networks, set to pause sharing: picked by the service, or
/// chosen by the user (`chosen`). Every other computer is offline, and none
/// to wake.
pub fn profile_paused(now: SystemTime, chosen: bool) -> Snapshot {
    let mut model = snapshot(now);
    for profile in &mut model.status.profiles {
        if profile.id == "other" {
            profile.paused = true;
        }
    }
    model.status.profile_id = "other".into();
    model.status.profile_chosen = chosen;
    model.status.network = Some(current(network(
        "n-cafe",
        "Cafe-Guest",
        NetworkKind::Wifi,
        "198.51.100.1",
    )));
    model.status.last_clipboard = None;
    for device in &mut model.status.devices {
        device.state = ConnectionState::Offline as i32;
        device.rtt_ms = 0;
        device.can_wake = false;
        device.offline_since_unix_ms = 0;
    }
    let mut arrangement = (*model.arrangement).clone();
    for device in &mut arrangement.devices {
        if !device.this_device {
            device.connected = false;
        }
    }
    model.arrangement = Arc::new(arrangement);
    model
}
