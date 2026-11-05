//! The displays of a Wayland desktop: its outputs, where `xdg_output` puts
//! them in the compositor's logical space.
//!
//! A thread of its own keeps a connection to the compositor and follows the
//! outputs as they come, go and change, so the agent's poll every second
//! only reads what it last saw. Once the compositor goes away, as it does
//! when the user logs out while their systemd service lives on, the thread
//! connects again, less and less often, and the outputs last seen stand
//! meanwhile. Waiting for a compositor to describe its outputs has a time
//! limit, so one that hangs is given up on and tried again later.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iw_core::layout::Display;
use tracing::{debug, info, warn};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_callback, wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};

use super::outputs::{self, Follows, Outputs};
use super::wlroots::Globals;
use crate::linux::common::lock;
use crate::{Displays, PlatformError};

/// The first wait before connecting again, doubling up to the longest.
const RECONNECT_FIRST: Duration = Duration::from_secs(1);
const RECONNECT_LONGEST: Duration = Duration::from_secs(30);

/// How long the compositor may take to describe its outputs.
const DESCRIBE_WAIT: Duration = Duration::from_secs(5);

/// The outputs, as the compositor last described them.
pub struct WaylandDisplays {
    seen: Arc<Mutex<Vec<Display>>>,
    offers: Globals,
}

impl WaylandDisplays {
    /// Connects to the compositor `WAYLAND_DISPLAY` names, waits for it to
    /// describe the outputs, then follows them on a thread.
    pub fn connect() -> Result<Self, PlatformError> {
        let follower = Follower::open()?;
        let offers = follower.offers;
        let seen = Arc::new(Mutex::new(follower.state.outputs.displays()));
        let thread_seen = seen.clone();
        std::thread::Builder::new()
            .name("inputweave-wayland-outputs".to_owned())
            .spawn(move || follow(follower, &thread_seen))
            .map_err(|e| PlatformError::Os(format!("cannot start the outputs thread: {e}")))?;
        Ok(Self { seen, offers })
    }

    /// Which of the globals input without portals needs the compositor
    /// listed as this connected: what decides how input goes in and out
    /// (`wlroots::injection`, `wlroots::capture`).
    pub(crate) fn offers(&self) -> Globals {
        self.offers
    }
}

impl Displays for WaylandDisplays {
    fn displays(&self) -> Result<Vec<Display>, PlatformError> {
        Ok(lock(&self.seen).clone())
    }
}

/// Follows the outputs, and connects again whenever the compositor goes.
fn follow(mut follower: Follower, seen: &Mutex<Vec<Display>>) {
    loop {
        let why = follower.run(seen);
        warn!(error = why, "lost the connection to the Wayland compositor");
        let mut wait = RECONNECT_FIRST;
        follower = loop {
            std::thread::sleep(wait);
            match Follower::open() {
                Ok(follower) => {
                    info!("connected to the Wayland compositor again");
                    *lock(seen) = follower.state.outputs.displays();
                    break follower;
                }
                Err(e) => {
                    debug!(error = %e, "cannot connect to the Wayland compositor again yet");
                    wait = (wait * 2).min(RECONNECT_LONGEST);
                }
            }
        };
    }
}

/// A connection following the outputs.
struct Follower {
    queue: EventQueue<State>,
    state: State,
    /// The outputs' changes published so far.
    published: u64,
    offers: Globals,
}

impl Follower {
    /// Connects and waits, for a while, for the outputs' description.
    fn open() -> Result<Self, PlatformError> {
        let os = |what: &str, e: &dyn std::fmt::Display| PlatformError::Os(format!("{what}: {e}"));
        let conn = Connection::connect_to_env()
            .map_err(|e| os("cannot connect to the Wayland compositor", &e))?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)
            .map_err(|e| os("cannot list the compositor's globals", &e))?;
        let offers = super::wlr::offered(&globals);
        let mut state = State {
            outputs: Outputs::listed(&globals, &queue.handle()),
        };
        outputs::describe(
            &conn,
            &mut queue,
            &mut state,
            Instant::now() + DESCRIBE_WAIT,
        )
        .map_err(|e| os("the compositor did not describe its outputs", &e))?;
        Ok(Self {
            queue,
            published: state.outputs.changes,
            state,
            offers,
        })
    }

    /// Follows the outputs until the connection breaks; why it did.
    fn run(&mut self, seen: &Mutex<Vec<Display>>) -> String {
        loop {
            if let Err(e) = self.queue.blocking_dispatch(&mut self.state) {
                return e.to_string();
            }
            if self.state.outputs.changes != self.published {
                self.published = self.state.outputs.changes;
                let displays = self.state.outputs.displays();
                debug!(?displays, "the outputs changed");
                *lock(seen) = displays;
            }
        }
    }
}

/// Like `EventQueue::roundtrip`, but giving up at `deadline`.
pub(super) fn roundtrip_until<S>(
    conn: &Connection,
    queue: &mut EventQueue<S>,
    state: &mut S,
    deadline: Instant,
) -> Result<(), String>
where
    S: Dispatch<wl_callback::WlCallback, Arc<AtomicBool>> + 'static,
{
    let done = Arc::new(AtomicBool::new(false));
    conn.display().sync(&queue.handle(), done.clone());
    loop {
        queue.flush().map_err(|e| e.to_string())?;
        queue.dispatch_pending(state).map_err(|e| e.to_string())?;
        if done.load(Ordering::SeqCst) {
            return Ok(());
        }
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("timed out".to_owned());
        }
        if crate::linux::common::ready(&guard.connection_fd(), libc::POLLIN, left) {
            guard.read().map_err(|e| e.to_string())?;
        }
    }
}

impl Dispatch<wl_callback::WlCallback, Arc<AtomicBool>> for State {
    fn event(
        _: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        done: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            done.store(true, Ordering::SeqCst);
        }
    }
}

struct State {
    outputs: Outputs,
}

impl Follows for State {
    fn outputs(&mut self) -> &mut Outputs {
        &mut self.outputs
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == wl_output::WlOutput::interface().name => {
                state.outputs.add(registry, name, version, qh);
            }
            wl_registry::Event::GlobalRemove { name } => state.outputs.remove(name),
            _ => {}
        }
    }
}

wayland_client::delegate_dispatch!(State: [wl_output::WlOutput: u32] => Outputs);
wayland_client::delegate_dispatch!(State: [zxdg_output_v1::ZxdgOutputV1: u32] => Outputs);
wayland_client::delegate_dispatch!(State: [zxdg_output_manager_v1::ZxdgOutputManagerV1: ()] => Outputs);
