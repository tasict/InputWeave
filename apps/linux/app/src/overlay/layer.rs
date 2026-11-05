//! Overlays on Wayland compositors with the layer-shell protocol (KDE
//! Plasma, sway and other wlroots ones): a layer surface on the overlay
//! layer, anchored at the top, with an empty input region so clicks go
//! through. GTK cannot make one without gtk4-layer-shell, which Ubuntu 24.04
//! does not have, so this is a Wayland client of its own, on its own
//! thread and connection, reading the compositor's events all along: the
//! outputs as they come and go, their names and scales, and what becomes
//! of the surfaces shown.
//!
//! The edge glow is a layer surface along the
//! crossed edge of the output the background service names, drawn anew
//! every frame while it fades.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use inputweave_app_core::edge_glow::{self, Glows, Timing};
use inputweave_app_core::log;
use inputweave_app_core::proto::{CursorCrossed, ScreenEdge};
use rustix::event::{EventfdFlags, PollFd, PollFlags, Timespec};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_output, wl_region, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use super::{Ended, paint};
use crate::style::Brand;

struct Hud {
    text: String,
    brand: Brand,
    shown: Duration,
}

/// An edge of an output to light where the cursor crossed it.
pub struct Glow {
    /// The crossing: the display, as the background service names it
    /// ("DP-1", or "output-<global>" for one without a name) and where it
    /// is, in the compositor's logical units.
    pub crossed: CursorCrossed,
    pub edge: ScreenEdge,
    /// Where along the edge it crossed, 0 to 1.
    pub spot: f64,
    pub color: [u8; 3],
    /// High contrast: a solid line.
    pub solid: bool,
    pub timing: Timing,
}

enum Request {
    Hud(Hud),
    Glow(Glow),
}

/// The layer surface id of the HUD's; glows take the ones after.
const HUD: u32 = 0;

pub struct LayerOverlays {
    /// `None` once dropped, which ends the thread.
    requests: Option<Sender<Request>>,
    /// Written after each request, so the thread, which waits on the
    /// compositor's socket and this, wakes for it.
    wake: Arc<OwnedFd>,
    /// Whether the thread still runs: it ends when it loses the compositor.
    running: Arc<AtomicBool>,
}

impl LayerOverlays {
    /// Connects to the compositor; `Err(true)` when it has no layer shell,
    /// as GNOME's does not, `Err(false)` when it cannot be reached.
    pub fn start() -> Result<LayerOverlays, bool> {
        let conn = Connection::connect_to_env().map_err(|_| false)?;
        let (globals, queue) = registry_queue_init::<State>(&conn).map_err(|_| false)?;
        let qh = queue.handle();
        let compositor: wl_compositor::WlCompositor =
            globals.bind(&qh, 4..=6, ()).map_err(|_| false)?;
        let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).map_err(|_| false)?;
        let shell: zwlr_layer_shell_v1::ZwlrLayerShellV1 =
            globals.bind(&qh, 1..=4, ()).map_err(|_| true)?;
        let mut state = State {
            // Where each output is, for the glow on one without a name.
            xdg: globals.bind(&qh, 1..=3, ()).ok(),
            ..State::default()
        };
        // The outputs there now; later ones arrive as registry events.
        for global in globals.contents().clone_list() {
            if global.interface == wl_output::WlOutput::interface().name {
                state.bind_output(globals.registry(), global.name, global.version, &qh);
            }
        }
        let wake = Arc::new(
            rustix::event::eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
                .map_err(|_| false)?,
        );
        let (requests, received) = channel();
        let globals = Globals {
            compositor,
            shm,
            shell,
        };
        let running = Arc::new(AtomicBool::new(true));
        let ended = Ended(running.clone());
        let woken = wake.clone();
        std::thread::Builder::new()
            .name("layer-overlays".into())
            .spawn(move || {
                let _ended = ended;
                run(conn, queue, globals, state, received, &woken);
            })
            .map_err(|_| false)?;
        Ok(LayerOverlays {
            requests: Some(requests),
            wake,
            running,
        })
    }

    /// The HUD at the top of the output the compositor picks (the one in
    /// use), for `shown`. False when the thread is gone, and nothing will
    /// show it.
    pub fn hud(&self, text: &str, brand: Brand, shown: Duration) -> bool {
        self.send(Request::Hud(Hud {
            text: text.to_owned(),
            brand,
            shown,
        }))
    }

    /// Lights an output's edge; false as `hud`.
    pub fn glow(&self, glow: Glow) -> bool {
        self.send(Request::Glow(glow))
    }

    fn send(&self, request: Request) -> bool {
        let sent = self.running.load(Ordering::SeqCst)
            && self
                .requests
                .as_ref()
                .is_some_and(|r| r.send(request).is_ok());
        if sent {
            self.wake();
        }
        sent
    }

    fn wake(&self) {
        let _ = rustix::io::write(&*self.wake, &1u64.to_ne_bytes());
    }
}

impl Drop for LayerOverlays {
    fn drop(&mut self) {
        // The thread sees the channel closed once woken.
        self.requests.take();
        self.wake();
    }
}

struct Globals {
    compositor: wl_compositor::WlCompositor,
    shm: wl_shm::WlShm,
    shell: zwlr_layer_shell_v1::ZwlrLayerShellV1,
}

/// A configure of a layer surface, not acknowledged yet.
#[derive(Clone, Copy, Debug)]
struct Configure {
    serial: u32,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct State {
    /// The last configure of each layer surface shown, by its id (`HUD`,
    /// or a glow's).
    configured: HashMap<u32, Configure>,
    closed: HashSet<u32>,
    /// The outputs by their global name, and each one's scale, name and
    /// place (xdg-output: logical x, y, width, height) by its object id.
    outputs: HashMap<u32, wl_output::WlOutput>,
    scales: HashMap<u32, i32>,
    names: HashMap<u32, String>,
    places: HashMap<u32, [i32; 4]>,
    /// Where the compositor has xdg-output, and each output's.
    xdg: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    xdg_outputs: HashMap<u32, zxdg_output_v1::ZxdgOutputV1>,
    /// Buffers attached and not released by the compositor yet, by their
    /// object id.
    busy: HashSet<u32>,
}

impl State {
    fn bind_output(
        &mut self,
        registry: &wl_registry::WlRegistry,
        name: u32,
        version: u32,
        qh: &QueueHandle<State>,
    ) {
        // The scale event is version 2's.
        if version >= 2 {
            let output = registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, ());
            if let Some(xdg) = &self.xdg {
                let id = output.id().protocol_id();
                self.xdg_outputs
                    .insert(id, xdg.get_xdg_output(&output, qh, id));
            }
            self.outputs.insert(name, output);
        }
    }

    /// The largest scale among the outputs, which the HUD is drawn for: the
    /// compositor scales it down where it shows it on a smaller one.
    fn scale(&self) -> i32 {
        self.scales.values().copied().max().unwrap_or(1).max(1)
    }

    /// The output the background service means, and its scale: by its
    /// name (wl_output version 4), else by its place and size
    /// (xdg-output), as on X11 (`edge_glow::monitor_for`).
    fn output(&self, crossed: &CursorCrossed) -> Option<(wl_output::WlOutput, i32)> {
        let outputs: Vec<&wl_output::WlOutput> = self.outputs.values().collect();
        let monitors: Vec<edge_glow::Monitor> = outputs
            .iter()
            .map(|o| {
                let id = o.id().protocol_id();
                let [x, y, width, height] = self.places.get(&id).copied().unwrap_or_default();
                edge_glow::Monitor {
                    id: self.names.get(&id).cloned().unwrap_or_default(),
                    x: f64::from(x),
                    y: f64::from(y),
                    width: f64::from(width),
                    height: f64::from(height),
                }
            })
            .collect();
        let output = outputs[edge_glow::monitor_for(crossed, &monitors)?];
        let scale = self.scales.get(&output.id().protocol_id()).copied();
        Some((output.clone(), scale.unwrap_or(1).max(1)))
    }
}

/// What a surface is made of, taken down together.
#[derive(Default)]
struct Parts {
    layer: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    surface: Option<wl_surface::WlSurface>,
    buffer: Option<wl_buffer::WlBuffer>,
    pool: Option<wl_shm_pool::WlShmPool>,
}

impl Parts {
    fn destroy(self) {
        if let Some(layer) = self.layer {
            layer.destroy();
        }
        if let Some(surface) = self.surface {
            surface.destroy();
        }
        if let Some(buffer) = self.buffer {
            buffer.destroy();
        }
        if let Some(pool) = self.pool {
            pool.destroy();
        }
    }
}

struct Shown {
    parts: Parts,
    until: Instant,
}

/// How a glow's surface looks and where it is: a surface is kept for a
/// new crossing of its edge only while it still looks so, on that output
/// (`None`: the compositor's pick), and the compositor has not closed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Look {
    output: Option<u32>,
    color: [u8; 3],
    solid: bool,
}

fn keeps(closed: bool, has: Look, wants: Look) -> bool {
    !closed && has == wants
}

/// The layer surface showing one edge's glow.
struct GlowSurface {
    key: (String, i32),
    id: u32,
    edge: ScreenEdge,
    look: Look,
    scale: i32,
    layer: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    surface: wl_surface::WlSurface,
    /// Its size in the compositor's units, once configured.
    size: Option<(u32, u32)>,
    /// Its buffers, made for that size.
    pool: Option<Pool>,
    /// Buffers of an earlier size, kept until a frame of the new one is
    /// on screen: the compositor may still read the last one.
    retired: Vec<Pool>,
    /// A frame of it failed (the error is logged once).
    failed: bool,
    /// The opacity and spot last drawn; `None` to draw the next frame
    /// whatever it is (new, or configured anew).
    drawn: Option<(f64, f64)>,
    /// Its pixels, kept between frames.
    pixels: Vec<u8>,
}

impl GlowSurface {
    fn destroy(self, state: &mut State) {
        state.configured.remove(&self.id);
        state.closed.remove(&self.id);
        self.layer.destroy();
        self.surface.destroy();
        for pool in self.pool.into_iter().chain(self.retired) {
            pool.destroy(state);
        }
    }
}

/// Two buffers in one shared-memory pool, drawn into in turn: the one the
/// compositor still reads (not released yet) is left alone.
struct Pool {
    file: std::fs::File,
    pool: wl_shm_pool::WlShmPool,
    buffers: [wl_buffer::WlBuffer; 2],
    /// Pixels, of a buffer.
    width: i32,
    height: i32,
}

impl Pool {
    fn new(
        globals: &Globals,
        qh: &QueueHandle<State>,
        width: i32,
        height: i32,
    ) -> Result<Pool, Box<dyn std::error::Error>> {
        let size = width * height * 4;
        let fd = rustix::fs::memfd_create("inputweave-glow", rustix::fs::MemfdFlags::CLOEXEC)?;
        let file = std::fs::File::from(fd);
        file.set_len(2 * size as u64)?;
        let pool = globals.shm.create_pool(file.as_fd(), 2 * size, qh, ());
        let buffer = |offset| {
            pool.create_buffer(
                offset,
                width,
                height,
                width * 4,
                wl_shm::Format::Argb8888,
                qh,
                (),
            )
        };
        let buffers = [buffer(0), buffer(size)];
        Ok(Pool {
            file,
            pool,
            buffers,
            width,
            height,
        })
    }

    /// Destroys the buffers and the pool, and forgets that the compositor
    /// held them: their ids may be another object's next.
    fn destroy(self, state: &mut State) {
        for buffer in self.buffers {
            state.busy.remove(&buffer.id().protocol_id());
            buffer.destroy();
        }
        self.pool.destroy();
    }
}

/// The glows lit, and their surfaces.
#[derive(Default)]
struct Lit {
    glows: Glows<(String, i32)>,
    surfaces: Vec<GlowSurface>,
    next_id: u32,
    /// When the last frame was drawn: the compositor's events (a buffer
    /// let go) wake the thread far more often than frames are due.
    drawn: Option<Instant>,
}

fn run(
    conn: Connection,
    mut queue: EventQueue<State>,
    globals: Globals,
    mut state: State,
    requests: Receiver<Request>,
    wake: &OwnedFd,
) {
    let qh = queue.handle();
    // The outputs' scales arrive with the first round trip.
    let _ = queue.roundtrip(&mut state);
    let mut shown: Option<Shown> = None;
    let mut lit = Lit {
        next_id: HUD + 1,
        ..Lit::default()
    };
    loop {
        // What the app asked for meanwhile: the last note replaces the
        // rest, and every crossing lights its edge, in order.
        let mut request = None;
        loop {
            match requests.try_recv() {
                Ok(Request::Hud(r)) => request = Some(r),
                Ok(Request::Glow(glow)) => light(&qh, &globals, &mut state, &mut lit, &glow),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if let Some(old) = shown.take() {
                        old.parts.destroy();
                    }
                    for surface in lit.surfaces.drain(..) {
                        surface.destroy(&mut state);
                    }
                    let _ = conn.flush();
                    return;
                }
            }
        }
        if let Some(request) = request {
            if let Some(old) = shown.take() {
                old.parts.destroy();
            }
            let mut parts = Parts::default();
            match show(&mut queue, &qh, &globals, &mut state, &request, &mut parts) {
                Ok(()) => {
                    shown = Some(Shown {
                        parts,
                        until: Instant::now() + request.shown,
                    })
                }
                Err(e) => {
                    parts.destroy();
                    log::info(&format!("cannot show the HUD on the layer shell: {e}"));
                }
            }
        }
        let done = shown
            .as_ref()
            .is_some_and(|s| state.closed.contains(&HUD) || Instant::now() >= s.until);
        if done && let Some(old) = shown.take() {
            old.parts.destroy();
        } else if let Some(s) = &shown
            && let (Some(configure), Some(layer), Some(surface)) = (
                state.configured.remove(&HUD),
                &s.parts.layer,
                &s.parts.surface,
            )
        {
            // The compositor configured it anew, as when outputs changed.
            layer.ack_configure(configure.serial);
            surface.commit();
        }
        glow_frame(&qh, &globals, &mut state, &mut lit);
        // Until the compositor says something, the app asks for something,
        // what is shown is due to go, or a glow's next frame; no waking
        // otherwise.
        let until = shown
            .as_ref()
            .map(|s| s.until.saturating_duration_since(Instant::now()));
        // A glow's next change: frames while it fades or slides, else its
        // next phase; the compositor's events (a configure, a buffer let
        // go) wake the thread as well.
        let surfaces = &lit.surfaces;
        let glow = lit.glows.wake(Instant::now(), lit.drawn, |key| {
            surfaces.iter().any(|s| &s.key == key && !s.look.solid)
        });
        let until = match (until, glow) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if let Err(e) = wait(&conn, &mut queue, &mut state, wake, until) {
            log::info(&format!("lost the compositor for overlays: {e}"));
            return;
        }
    }
}

/// Waits for the compositor's events or `wake`, for at most `until`
/// (forever without), and handles what the compositor sent.
fn wait(
    conn: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    wake: &OwnedFd,
    until: Option<Duration>,
) -> Result<(), Box<dyn std::error::Error>> {
    conn.flush()?;
    // Events read already (as by a round trip) come first.
    let Some(guard) = queue.prepare_read() else {
        queue.dispatch_pending(state)?;
        return Ok(());
    };
    let timeout = until.map(|d| Timespec {
        tv_sec: d.as_secs() as _,
        tv_nsec: d.subsec_nanos() as _,
    });
    let (compositor, woken) = {
        let fd = guard.connection_fd();
        let mut fds = [
            PollFd::new(&fd, PollFlags::IN),
            PollFd::new(wake, PollFlags::IN),
        ];
        match rustix::event::poll(&mut fds, timeout.as_ref()) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
        (!fds[0].revents().is_empty(), !fds[1].revents().is_empty())
    };
    if compositor {
        match guard.read() {
            Ok(_) => {}
            // Read already, as by a round trip just before.
            Err(wayland_client::backend::WaylandError::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
    } else {
        drop(guard);
    }
    if woken {
        let mut count = [0u8; 8];
        let _ = rustix::io::read(wake, &mut count);
    }
    queue.dispatch_pending(state)?;
    Ok(())
}

fn show(
    queue: &mut EventQueue<State>,
    qh: &QueueHandle<State>,
    globals: &Globals,
    state: &mut State,
    request: &Hud,
    parts: &mut Parts,
) -> Result<(), Box<dyn std::error::Error>> {
    let scale = state.scale();
    let picture = paint::hud(&request.text, &request.brand, scale)?;
    let surface = globals.compositor.create_surface(qh, ());
    parts.surface = Some(surface.clone());
    let layer = globals.shell.get_layer_surface(
        &surface,
        None,
        zwlr_layer_shell_v1::Layer::Overlay,
        "inputweave-hud".into(),
        qh,
        HUD,
    );
    parts.layer = Some(layer.clone());
    layer.set_anchor(zwlr_layer_surface_v1::Anchor::Top);
    layer.set_margin(48, 0, 0, 0);
    layer.set_size(
        (picture.width / scale) as u32,
        (picture.height / scale) as u32,
    );
    surface.set_buffer_scale(scale);
    layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
    // Clicks go through: an empty input region.
    let region = globals.compositor.create_region(qh, ());
    surface.set_input_region(Some(&region));
    region.destroy();
    state.configured.remove(&HUD);
    state.closed.remove(&HUD);
    surface.commit();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !state.configured.contains_key(&HUD) && !state.closed.contains(&HUD) {
        queue.roundtrip(state)?;
        if Instant::now() > deadline {
            return Err("the compositor did not configure the surface".into());
        }
    }
    if state.closed.contains(&HUD) {
        return Err("the compositor closed the surface".into());
    }
    if let Some(configure) = state.configured.remove(&HUD) {
        layer.ack_configure(configure.serial);
    }
    let fd = rustix::fs::memfd_create("inputweave-hud", rustix::fs::MemfdFlags::CLOEXEC)?;
    let mut file = std::fs::File::from(fd);
    file.write_all(&picture.data)?;
    let pool = globals
        .shm
        .create_pool(file.as_fd(), picture.data.len() as i32, qh, ());
    parts.pool = Some(pool.clone());
    let buffer = pool.create_buffer(
        0,
        picture.width,
        picture.height,
        picture.stride,
        wl_shm::Format::Argb8888,
        qh,
        (),
    );
    parts.buffer = Some(buffer.clone());
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, picture.width, picture.height);
    surface.commit();
    queue.flush()?;
    Ok(())
}

/// Lights an edge, or carries its glow on (`Glows::light`); its surface is
/// made when there is none, and shows once the compositor configured it.
fn light(
    qh: &QueueHandle<State>,
    globals: &Globals,
    state: &mut State,
    lit: &mut Lit,
    glow: &Glow,
) {
    let key = (glow.crossed.display_id.clone(), glow.edge as i32);
    lit.glows
        .light(key.clone(), glow.spot, Instant::now(), glow.timing);
    // On that output; the compositor picks one where none is found.
    let (output, scale) = match state.output(&glow.crossed) {
        Some((output, scale)) => (Some(output), scale),
        None => (None, state.scale()),
    };
    let look = Look {
        output: output.as_ref().map(|o| o.id().protocol_id()),
        color: glow.color,
        solid: glow.solid,
    };
    // The surface there carries on, unless closed by the compositor, on
    // another output (outputs changed) or drawn otherwise.
    if let Some(i) = lit.surfaces.iter().position(|s| s.key == key) {
        let had = &lit.surfaces[i];
        if keeps(state.closed.contains(&had.id), had.look, look) {
            return;
        }
        lit.surfaces.remove(i).destroy(state);
    }
    let id = lit.next_id;
    lit.next_id += 1;
    let surface = globals.compositor.create_surface(qh, ());
    let layer = globals.shell.get_layer_surface(
        &surface,
        output.as_ref(),
        zwlr_layer_shell_v1::Layer::Overlay,
        "inputweave-glow".into(),
        qh,
        id,
    );
    use zwlr_layer_surface_v1::Anchor;
    let deep = if glow.solid {
        edge_glow::SOLID_DEPTH
    } else {
        edge_glow::DEPTH
    } as u32;
    let (anchor, width, height) = match glow.edge {
        ScreenEdge::Left => (Anchor::Left | Anchor::Top | Anchor::Bottom, deep, 0),
        ScreenEdge::Right => (Anchor::Right | Anchor::Top | Anchor::Bottom, deep, 0),
        ScreenEdge::Top => (Anchor::Top | Anchor::Left | Anchor::Right, 0, deep),
        _ => (Anchor::Bottom | Anchor::Left | Anchor::Right, 0, deep),
    };
    layer.set_anchor(anchor);
    layer.set_size(width, height);
    // At the very edge, over panels that keep space for themselves.
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
    surface.set_buffer_scale(scale);
    let region = globals.compositor.create_region(qh, ());
    surface.set_input_region(Some(&region));
    region.destroy();
    surface.commit();
    lit.surfaces.push(GlowSurface {
        key,
        id,
        edge: glow.edge,
        look,
        scale,
        layer,
        surface,
        size: None,
        pool: None,
        retired: Vec::new(),
        failed: false,
        drawn: None,
        pixels: Vec::new(),
    });
}

/// Draws every glow still lit as it is now, and takes down those gone or
/// closed by the compositor.
fn glow_frame(qh: &QueueHandle<State>, globals: &Globals, state: &mut State, lit: &mut Lit) {
    if lit.surfaces.is_empty() && lit.glows.is_empty() {
        return;
    }
    let frames = lit.glows.frame(Instant::now());
    let (kept, gone): (Vec<GlowSurface>, Vec<GlowSurface>) = std::mem::take(&mut lit.surfaces)
        .into_iter()
        .partition(|s| !state.closed.contains(&s.id) && frames.iter().any(|f| f.key == s.key));
    for s in gone {
        s.destroy(state);
    }
    lit.surfaces = kept;
    let now = Instant::now();
    let configured = lit
        .surfaces
        .iter()
        .any(|s| state.configured.contains_key(&s.id));
    if !configured
        && lit
            .drawn
            .is_some_and(|at| now.saturating_duration_since(at) < edge_glow::FRAME)
    {
        return;
    }
    lit.drawn = Some(now);
    for frame in &frames {
        let Some(s) = lit.surfaces.iter_mut().find(|s| s.key == frame.key) else {
            continue;
        };
        if let Some(configure) = state.configured.remove(&s.id) {
            s.layer.ack_configure(configure.serial);
            s.size = Some((configure.width, configure.height));
            s.drawn = None;
        }
        // One glow's failure is its own: the others are still configured
        // and drawn, and it is said once.
        if let Err(e) = draw(qh, globals, state, s, frame) {
            if !s.failed {
                log::info(&format!("cannot draw the screen's edge: {e}"));
            }
            s.failed = true;
        }
    }
}

/// Draws a frame of one glow, if it looks other than the last: into
/// whichever of its two buffers the compositor is not reading.
fn draw(
    qh: &QueueHandle<State>,
    globals: &Globals,
    state: &mut State,
    s: &mut GlowSurface,
    frame: &edge_glow::Frame<(String, i32)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some((width, height)) = s.size else {
        return Ok(());
    };
    let (width, height) = (
        (width.max(1) as i32) * s.scale,
        (height.max(1) as i32) * s.scale,
    );
    let (along, deep) = edge_glow::along_deep(s.edge, width, height);
    let opacity = if s.look.solid { 1.0 } else { frame.opacity };
    // Held, or the spot at rest: the compositor shows it as it is.
    if edge_glow::unchanged(s.drawn, (opacity, frame.spot), s.look.solid) {
        return Ok(());
    }
    // Another size: new buffers, the old ones kept until one of the new
    // is on screen.
    if s.pool
        .as_ref()
        .is_some_and(|p| (p.width, p.height) != (width, height))
        && let Some(old) = s.pool.take()
    {
        s.retired.push(old);
    }
    if s.pool.is_none() {
        s.pool = Some(Pool::new(globals, qh, width, height)?);
    }
    let pool = s.pool.as_ref().expect("made above");
    // Both still read: the next frame instead.
    let Some(free) = (0..2).find(|i| !state.busy.contains(&pool.buffers[*i].id().protocol_id()))
    else {
        return Ok(());
    };
    s.pixels.resize(along as usize * deep as usize * 4, 0);
    edge_glow::paint(
        &mut s.pixels,
        s.edge,
        along as usize,
        deep as usize,
        frame.spot,
        s.look.color,
        opacity,
        s.look.solid,
    );
    let offset = (free as u64) * s.pixels.len() as u64;
    rustix::io::pwrite(&pool.file, &s.pixels, offset)?;
    let buffer = &pool.buffers[free];
    s.surface.attach(Some(buffer), 0, 0);
    s.surface.damage_buffer(0, 0, width, height);
    s.surface.commit();
    state.busy.insert(buffer.id().protocol_id());
    s.drawn = Some((opacity, frame.spot));
    // A buffer of the new size is on screen: the old ones are done with.
    for old in std::mem::take(&mut s.retired) {
        old.destroy(state);
    }
    Ok(())
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
                state.bind_output(registry, name, version, qh);
            }
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(output) = state.outputs.remove(&name) {
                    let id = output.id().protocol_id();
                    state.scales.remove(&id);
                    state.names.remove(&id);
                    state.places.remove(&id);
                    if let Some(xdg) = state.xdg_outputs.remove(&id) {
                        xdg.destroy();
                    }
                    if output.version() >= 3 {
                        output.release();
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_output::Event::Scale { factor } => {
                state.scales.insert(output.id().protocol_id(), factor);
            }
            wl_output::Event::Name { name } => {
                state.names.insert(output.id().protocol_id(), name);
            }
            _ => {}
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, u32> for State {
    fn event(
        state: &mut Self,
        _: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                state.configured.insert(
                    *id,
                    Configure {
                        serial,
                        width,
                        height,
                    },
                );
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.closed.insert(*id);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(
        state: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            state.busy.remove(&buffer.id().protocol_id());
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, u32> for State {
    fn event(
        state: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        output: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let place = state.places.entry(*output).or_default();
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                place[0] = x;
                place[1] = y;
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                place[2] = width;
                place[3] = height;
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore zxdg_output_manager_v1::ZxdgOutputManagerV1);
delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_region::WlRegion);
delegate_noop!(State: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_glows_surface_is_made_anew_when_closed_moved_or_drawn_otherwise() {
        let look = Look {
            output: Some(6),
            color: [142, 63, 216],
            solid: false,
        };
        assert!(keeps(false, look, look));
        assert!(!keeps(true, look, look), "closed by the compositor");
        let moved = Look {
            output: Some(9),
            ..look
        };
        assert!(!keeps(false, look, moved), "another output");
        let unknown = Look {
            output: None,
            ..look
        };
        assert!(!keeps(false, unknown, look), "found its output since");
        let solid = Look {
            solid: true,
            ..look
        };
        assert!(!keeps(false, look, solid), "high contrast now");
    }
}
