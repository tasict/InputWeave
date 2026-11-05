//! The client against the real agent with its mock backends: run with the
//! agent built from the root workspace, e.g.
//!
//! ```sh
//! INPUTWEAVE_AGENT=/target/debug/inputweave cargo test -p inputweave-app-core \
//!     --test real_agent -- --ignored
//! ```

use std::future::Future;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread;
use std::time::{Duration, Instant};

use inputweave_app_core::agent::{Agent, AgentError, Answer};
use inputweave_app_core::proto::{
    GetDiagnostics, Permission, RequestPermission, SetDeviceName, SetSharing, Unpair,
    client_message::Command as C, response,
};
use inputweave_app_core::service;
use inputweave_app_core::snapshot::Link;

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
    let deadline = Instant::now() + Duration::from_secs(60);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_agent(binary: &Path, dir: &Path) -> Running {
    let config = dir.join("config.toml");
    let path = |name: &str| dir.join(name).display().to_string();
    std::fs::write(
        &config,
        format!(
            r#"[device]
id = "linux-test"
name = "Linux test"

[network]
listen = "127.0.0.1:0"
discovery = false
allow_pairing = false

[paths]
identity_dir = "{}"
ipc_socket = "{}"
devices_file = "{}"

[backend]
kind = "mock"
displays = [{{ id = "DP-1", x = 0, y = 0, width = 1920, height = 1080, scale = 1.0, primary = true }}]
"#,
            path("identity"),
            path("agent.sock"),
            path("devices.toml"),
        ),
    )
    .unwrap();
    let child = Command::new(binary)
        .args(["agent", "--config"])
        .arg(&config)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env_remove("XDG_RUNTIME_DIR")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("starting the agent");
    Running(child)
}

#[test]
#[ignore = "needs the agent; set INPUTWEAVE_AGENT"]
fn the_app_talks_to_the_real_agent() {
    let binary = std::env::var_os("INPUTWEAVE_AGENT").expect("INPUTWEAVE_AGENT");
    let dir = tempfile::tempdir().unwrap();
    let mut agent_process = start_agent(Path::new(&binary), dir.path());
    let agent = Agent::start(dir.path().join("agent.sock"), "linux-ui/test", || {});
    wait_until("the agent", || agent.snapshot().connected());

    let snapshot = agent.snapshot();
    assert_eq!(snapshot.status.this_device_id, "linux-test");
    assert_eq!(snapshot.status.this_device_name, "Linux test");
    assert_eq!(snapshot.agent_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(snapshot.agent_digest.len(), 64, "a SHA-256 in hex");
    // The app's digest of the same executable is the agent's own, so an
    // agent that is the packaged one is not taken for outdated.
    let digest = service::file_digest(Path::new(&binary)).unwrap();
    assert_eq!(digest, snapshot.agent_digest);
    assert!(!service::is_outdated(
        snapshot.link,
        &snapshot.agent_version,
        env!("CARGO_PKG_VERSION"),
        &snapshot.agent_digest,
        &digest,
        None
    ));
    assert!(service::is_outdated(
        snapshot.link,
        &snapshot.agent_version,
        env!("CARGO_PKG_VERSION"),
        &snapshot.agent_digest,
        &"0".repeat(64),
        None
    ));
    // The mock backends ask for no permission: none listed, and asking is
    // refused with a code.
    assert!(inputweave_app_core::trouble_text::permission_rows(&snapshot).is_empty());
    let refused = block_on(agent.request(C::RequestPermission(RequestPermission {
        permission: Permission::RemoteControl as i32,
    })))
    .unwrap_err();
    assert_eq!(refused.code(), Some("unsupported"), "{refused:?}");
    assert!(snapshot.status.relative_supported);
    assert!(
        snapshot
            .transfers
            .receive_folder
            .ends_with("Downloads/InputWeave"),
        "{}",
        snapshot.transfers.receive_folder
    );
    let screens = &snapshot
        .arrangement
        .devices
        .iter()
        .find(|d| d.this_device)
        .expect("this device in the arrangement")
        .screens;
    assert_eq!(screens.len(), 1);

    block_on(agent.request(C::SetSharing(SetSharing { enabled: false }))).unwrap();
    assert!(!agent.snapshot().status.sharing, "applied with the answer");
    block_on(agent.request(C::SetSharing(SetSharing { enabled: true }))).unwrap();
    assert!(agent.snapshot().status.sharing);

    block_on(agent.request(C::SetDeviceName(SetDeviceName {
        name: "Renamed box".into(),
    })))
    .unwrap();
    assert_eq!(agent.snapshot().status.this_device_name, "Renamed box");

    let refused = block_on(agent.request(C::Unpair(Unpair {
        device_id: "nobody".into(),
    })))
    .unwrap_err();
    assert!(matches!(refused, AgentError::Refused { .. }), "{refused:?}");

    let diagnostics = block_on(agent.request(C::GetDiagnostics(GetDiagnostics {}))).unwrap();
    match diagnostics {
        Answer::Other(answer) => match *answer {
            response::Result::Diagnostics(d) => assert!(d.report.contains("Renamed box")),
            other => panic!("no diagnostics: {other:?}"),
        },
        Answer::Applied => panic!("no diagnostics"),
    }

    // The agent goes away; the app notices and keeps trying.
    let _ = agent_process.0.kill();
    let _ = agent_process.0.wait();
    wait_until("the link to drop", || {
        agent.snapshot().link == Link::Connecting
    });
    assert!(agent.snapshot().unreachable_since.is_some());
    agent.stop();
}
