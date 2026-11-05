//! A connection to a wlroots compositor for input without portals:
//! the globals, the outputs, the
//! seat's pointer and keyboard, and the events of what it creates, queued
//! for the backend that owns it (`wlr_emulation`, `wlr_capture`) to take.
//! Each backend has a connection, and a thread, of its own, which connects
//! again as the compositor goes (`keep_connected`).
//!
//! Also what is read before choosing a way at all (`wlroots`): which of
//! the globals these use the compositor lists (`offered`).

use std::collections::VecDeque;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixDatagram;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iw_core::geometry::Rect;
use tracing::{debug, warn};
use wayland_client::backend::ObjectId;
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_registry,
    wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1 as ext_data_control;
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1 as inhibit_manager,
    zwp_keyboard_shortcuts_inhibitor_v1 as inhibitor,
};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1 as relative_manager, zwp_relative_pointer_v1 as relative,
};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1 as keyboard_manager,
    zwp_virtual_keyboard_v1 as virtual_keyboard,
};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_manager_v1 as wlr_data_control;
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1 as pointer_manager, zwlr_virtual_pointer_v1 as virtual_pointer,
};

use super::outputs::{self, Follows, Outputs};
use super::translate::displays_of;
use super::wlroots::{Extent, Globals};
use crate::linux::common::{Backoff, lock, poll_each, ready};

pub(crate) use inhibitor::ZwpKeyboardShortcutsInhibitorV1 as Inhibitor;
pub(crate) use keyboard_manager::ZwpVirtualKeyboardManagerV1 as KeyboardManager;
pub(crate) use pointer_manager::ZwlrVirtualPointerManagerV1 as PointerManager;
pub(crate) use virtual_keyboard::ZwpVirtualKeyboardV1 as VirtualKeyboard;
pub(crate) use virtual_pointer::ZwlrVirtualPointerV1 as VirtualPointer;
pub(crate) use wayland_client::protocol::wl_pointer::{Axis as WlAxis, AxisSource, ButtonState};
pub(crate) use zwlr_layer_shell_v1::Layer;
pub(crate) use zwlr_layer_surface_v1::{
    Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1 as LayerSurface,
};

/// The newest versions of the protocols this knows, besides the outputs'.
/// wl_seat 8 tells wheels in 120ths of a notch (`axis_value120`).
const SEAT_VERSION: u32 = 8;
const LAYER_SHELL_VERSION: u32 = 4;
const VIRTUAL_POINTER_VERSION: u32 = 2;

/// How long the compositor may take to answer as a connection opens.
const OPEN_WAIT: Duration = Duration::from_secs(5);

/// The largest keymap read from the seat; one is some 60 KiB.
const MAX_KEYMAP: u32 = 4 << 20;

/// Which of the globals input without portals needs the compositor
/// lists in `globals`.
pub(crate) fn offered(globals: &GlobalList) -> Globals {
    globals.contents().with_list(|list| {
        let has = |name: &str| list.iter().any(|g| g.interface == name);
        Globals {
            virtual_pointer: has(pointer_manager::ZwlrVirtualPointerManagerV1::interface().name),
            virtual_keyboard: has(keyboard_manager::ZwpVirtualKeyboardManagerV1::interface().name),
            layer_shell: has(zwlr_layer_shell_v1::ZwlrLayerShellV1::interface().name),
            relative_pointer: has(relative_manager::ZwpRelativePointerManagerV1::interface().name),
            shortcuts_inhibit: has(
                inhibit_manager::ZwpKeyboardShortcutsInhibitManagerV1::interface().name,
            ),
            data_control: has(ext_data_control::ExtDataControlManagerV1::interface().name)
                || has(wlr_data_control::ZwlrDataControlManagerV1::interface().name),
        }
    })
}

/// The two ends of a backend's wake: the agent's side sends a byte to wake
/// the thread, whose side is polled and drained. Both are non-blocking: a
/// wake is the same however many are queued, and the commands wait in a
/// queue of their own, so a full socket (its queue is short) is no reason
/// for the agent to wait while the thread is busy.
pub(crate) fn wake_pair() -> Result<(UnixDatagram, UnixDatagram), crate::PlatformError> {
    let failed = |e: std::io::Error| crate::PlatformError::Os(format!("cannot make a wake: {e}"));
    let (wake, woken) = UnixDatagram::pair().map_err(failed)?;
    wake.set_nonblocking(true).map_err(failed)?;
    woken.set_nonblocking(true).map_err(failed)?;
    Ok((wake, woken))
}

/// How a backend's thread is told to end, and says it did.
#[derive(Default)]
pub(crate) struct Ending {
    stop: AtomicBool,
    done: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

impl Ending {
    /// Whether the thread is to end.
    pub(crate) fn asked(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Asks the thread to end, waking it through `wake`, and waits up to
    /// `wait` for it to have.
    pub(crate) fn ask(&self, wake: &UnixDatagram, wait: Duration) {
        let (done, ended) = std::sync::mpsc::channel();
        *lock(&self.done) = Some(done);
        self.stop.store(true, Ordering::SeqCst);
        let _ = wake.send(&[1]);
        let _ = ended.recv_timeout(wait);
    }

    fn ended(&self) {
        if let Some(done) = lock(&self.done).take() {
            let _ = done.send(());
        }
    }
}

/// What became of a connection.
pub(crate) enum Served {
    /// The thread was asked to end, and it let go of what it held.
    Ended,
    /// It ran, until the compositor went.
    Lost(String),
    /// It did not open.
    Unopened(String),
}

/// Runs a backend's connections on its thread until `ending` asks it to
/// end: `connect` opens one and serves it, with `context` kept from one to
/// the next. Lost, or not opened, it waits to connect again, from half a
/// second up to ten after failures in a row (`Backoff`), calling `waiting`
/// as it starts to wait and whenever `woken` wakes it meanwhile. A warning
/// that it cannot `what` goes once for each run of failures.
pub(crate) fn keep_connected<T>(
    what: &str,
    ending: &Ending,
    woken: &UnixDatagram,
    context: &mut T,
    mut connect: impl FnMut(&mut T) -> Served,
    mut waiting: impl FnMut(&mut T),
) {
    let mut backoff = Backoff::new();
    let mut warned = false;
    while !ending.asked() {
        match connect(context) {
            Served::Ended => break,
            Served::Lost(e) => {
                backoff.succeeded();
                warned = false;
                warn!(error = e, "lost the Wayland compositor; connecting again");
            }
            Served::Unopened(e) => {
                if warned {
                    debug!(error = e, "cannot {what} yet");
                } else {
                    warn!(error = e, "cannot {what} yet");
                    warned = true;
                }
                backoff.failed();
            }
        }
        let until = Instant::now() + backoff.delay();
        while Instant::now() < until && !ending.asked() {
            waiting(context);
            let left = until.saturating_duration_since(Instant::now());
            if ready(woken, libc::POLLIN, left) {
                drain(woken);
            }
        }
    }
    ending.ended();
}

/// What a backend asks of its connection.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Wants {
    /// The seat's keyboard, for its repeat, and its keys while a surface
    /// here has the keyboard.
    pub keyboard: bool,
    /// With the keyboard: what its keymap says, as it comes.
    pub keymap: bool,
    /// The seat's pointer, and its relative motion.
    pub pointer: bool,
}

/// What the compositor said, for the backend to take in order.
#[derive(Debug)]
pub(crate) enum Event {
    PointerEnter {
        surface: ObjectId,
        serial: u32,
        x: f64,
        y: f64,
    },
    PointerLeave {
        surface: ObjectId,
    },
    PointerMotion {
        x: f64,
        y: f64,
    },
    Relative {
        dx: f64,
        dy: f64,
    },
    Button {
        button: u32,
        pressed: bool,
    },
    /// An axis moved this many logical pixels.
    Axis {
        axis: WlAxis,
        value: f64,
    },
    /// An axis turned this many 120ths of a notch (`axis_value120`, or
    /// `axis_discrete` before seat version 8, counted so).
    AxisSteps {
        axis: WlAxis,
        value120: i32,
    },
    Frame,
    KeyboardEnter {
        surface: ObjectId,
        keys: Vec<u32>,
    },
    KeyboardLeave {
        surface: ObjectId,
    },
    Key {
        key: u32,
        pressed: bool,
    },
    Configure {
        surface: ObjectId,
        serial: u32,
        width: u32,
        height: u32,
    },
    Closed {
        surface: ObjectId,
    },
    /// The compositor handled everything asked before `Link::fence(n)`.
    Fenced(u32),
}

/// A sync whose done comes as `Event::Fenced`, in order with the events.
struct Fence(u32);

/// A connection, its queue, and what it knows.
pub(crate) struct Link {
    pub conn: Connection,
    pub queue: EventQueue<State>,
    pub state: State,
    /// When it opened: times sent are from then, in milliseconds.
    opened: Instant,
    /// The outputs as `geometry` last worked them out, and how many changes
    /// of theirs it had seen.
    geometry: Option<(u64, Geometry)>,
    /// The last flush left requests the socket had no room for.
    unsent: bool,
}

/// The outputs as the backends use them.
pub(crate) struct Geometry {
    /// Each output, with its id as `WaylandDisplays` gives it and its
    /// logical bounds.
    pub outputs: Vec<(String, Rect, wl_output::WlOutput)>,
    /// The box that bounds them, for the virtual pointer's absolute motion.
    pub extent: Option<Extent>,
}

/// What a connection knows, and the events it has yet to give.
pub(crate) struct State {
    pub qh: QueueHandle<State>,
    wants: Wants,
    pub compositor: Option<wl_compositor::WlCompositor>,
    pub shm: Option<wl_shm::WlShm>,
    pub seat: Option<wl_seat::WlSeat>,
    pub layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    /// Where the compositor scales surfaces: one pixel stretched over a
    /// surface rather than a buffer of its size.
    pub viewporter: Option<wp_viewporter::WpViewporter>,
    relative_manager: Option<relative_manager::ZwpRelativePointerManagerV1>,
    pub inhibit_manager: Option<inhibit_manager::ZwpKeyboardShortcutsInhibitManagerV1>,
    pub pointer_manager: Option<PointerManager>,
    pub keyboard_manager: Option<KeyboardManager>,
    pub outputs: Outputs,
    pub pointer: Option<wl_pointer::WlPointer>,
    relative: Option<relative::ZwpRelativePointerV1>,
    pub keyboard: Option<wl_keyboard::WlKeyboard>,
    /// The seat's keymap, as its keyboard last sent it (with `Wants`'s
    /// `keymap`), and whether one came since the backend last looked.
    pub keymap: Option<Vec<u8>>,
    pub keymap_changed: bool,
    /// `repeat_info`: keys a second, and the delay in milliseconds.
    pub repeat: Option<(i32, i32)>,
    pub events: VecDeque<Event>,
}

impl Link {
    /// Connects to the compositor `WAYLAND_DISPLAY` names, binds what it
    /// offers of what the backends use, and waits a moment for its outputs
    /// and seat to be described.
    pub(crate) fn open(wants: Wants) -> Result<Self, String> {
        let conn = Connection::connect_to_env()
            .map_err(|e| format!("cannot connect to the Wayland compositor: {e}"))?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)
            .map_err(|e| format!("cannot list the compositor's globals: {e}"))?;
        let qh = queue.handle();
        let mut state = State {
            qh: qh.clone(),
            wants,
            compositor: globals.bind(&qh, 1..=4, ()).ok(),
            shm: globals.bind(&qh, 1..=1, ()).ok(),
            seat: globals.bind(&qh, 1..=SEAT_VERSION, ()).ok(),
            layer_shell: globals.bind(&qh, 1..=LAYER_SHELL_VERSION, ()).ok(),
            viewporter: globals.bind(&qh, 1..=1, ()).ok(),
            relative_manager: globals.bind(&qh, 1..=1, ()).ok(),
            inhibit_manager: globals.bind(&qh, 1..=1, ()).ok(),
            pointer_manager: globals.bind(&qh, 1..=VIRTUAL_POINTER_VERSION, ()).ok(),
            keyboard_manager: globals.bind(&qh, 1..=1, ()).ok(),
            outputs: Outputs::listed(&globals, &qh),
            pointer: None,
            relative: None,
            keyboard: None,
            keymap: None,
            keymap_changed: false,
            repeat: None,
            events: VecDeque::new(),
        };
        // The seat's capabilities and the outputs, then the xdg_outputs and
        // the keyboard's keymap.
        outputs::describe(&conn, &mut queue, &mut state, Instant::now() + OPEN_WAIT)
            .map_err(|e| format!("the compositor did not answer: {e}"))?;
        state.events.clear();
        Ok(Self {
            conn,
            queue,
            state,
            opened: Instant::now(),
            geometry: None,
            unsent: false,
        })
    }

    /// Milliseconds since the connection opened, as the protocols time
    /// input.
    pub(crate) fn time(&self) -> u32 {
        self.opened.elapsed().as_millis() as u32
    }

    /// Sends what was asked and handles what came, without waiting. A
    /// socket the compositor has yet to read, or nothing to read yet, is no
    /// error (`WouldBlock`): what was not sent goes at the next try.
    pub(crate) fn pump(&mut self) -> Result<(), String> {
        self.unsent = match self.queue.flush() {
            Ok(()) => false,
            Err(e) => {
                would_block(e)?;
                true
            }
        };
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Sends what was asked, then waits until the compositor says
    /// something, `wake` becomes readable (and is drained), or `deadline`;
    /// while requests wait for room in the socket, until there is room too,
    /// and sends them, so what was asked (a key let go of) always goes.
    pub(crate) fn wait(
        &mut self,
        wake: &impl AsRawFd,
        deadline: Option<Instant>,
    ) -> Result<(), String> {
        self.pump()?;
        if !self.state.events.is_empty() {
            return Ok(());
        }
        let Some(guard) = self.queue.prepare_read() else {
            return self.pump();
        };
        let wait = deadline.map(|at| at.saturating_duration_since(Instant::now()));
        let [compositor, woken] = poll_each(
            [
                (guard.connection_fd().as_raw_fd(), interest(self.unsent)),
                (wake.as_raw_fd(), libc::POLLIN),
            ],
            wait,
        )
        .map_err(|e| format!("cannot wait for the compositor: {e}"))?;
        let woken = woken != 0;
        // Room to send comes as POLLOUT: the pump below sends; anything
        // else is the compositor's to read.
        if compositor & !libc::POLLOUT != 0 {
            if let Err(e) = guard.read() {
                would_block(e)?;
            }
        } else {
            drop(guard);
        }
        if woken {
            drain(wake);
        }
        self.pump()
    }

    /// Has the compositor say `Event::Fenced(n)` once it handled everything
    /// asked so far, after every event that caused.
    pub(crate) fn fence(&mut self, n: u32) {
        self.conn.display().sync(&self.state.qh, Fence(n));
    }

    /// Waits up to `wait` for the compositor to have handled everything
    /// asked so far.
    pub(crate) fn sync(&mut self, wait: Duration) -> Result<(), String> {
        super::displays::roundtrip_until(
            &self.conn,
            &mut self.queue,
            &mut self.state,
            Instant::now() + wait,
        )
    }

    /// The outputs described so far, worked out again only once they
    /// changed.
    pub(crate) fn geometry(&mut self) -> &Geometry {
        let changes = self.state.outputs.changes;
        if self
            .geometry
            .as_ref()
            .is_none_or(|(seen, _)| *seen != changes)
        {
            let outputs: Vec<(String, Rect, wl_output::WlOutput)> = self
                .state
                .outputs
                .list
                .iter()
                .filter_map(|o| {
                    let display = displays_of(std::slice::from_ref(&o.described))
                        .into_iter()
                        .next()?;
                    Some((display.id, display.bounds, o.output.clone()))
                })
                .collect();
            let bounds: Vec<Rect> = outputs.iter().map(|o| o.1).collect();
            let extent = Extent::of(&bounds);
            self.geometry = Some((changes, Geometry { outputs, extent }));
        }
        &self.geometry.as_ref().expect("worked out above").1
    }

    /// A transparent buffer of `width` × `height`, for a surface that is
    /// to take input without showing anything.
    pub(crate) fn clear_buffer(&self, width: u32, height: u32) -> Option<wl_buffer::WlBuffer> {
        let shm = self.state.shm.as_ref()?;
        let stride = width.checked_mul(4)?;
        let size = stride.checked_mul(height)?;
        let fd = memfd("inputweave-edge", u64::from(size))?;
        let pool = shm.create_pool(fd.as_fd(), i32::try_from(size).ok()?, &self.state.qh, ());
        let buffer = pool.create_buffer(
            0,
            i32::try_from(width).ok()?,
            i32::try_from(height).ok()?,
            i32::try_from(stride).ok()?,
            wl_shm::Format::Argb8888,
            &self.state.qh,
            (),
        );
        pool.destroy();
        Some(buffer)
    }
}

/// What to wait for on the compositor's socket: what it says, and room to
/// send while requests wait for it (`unsent`).
fn interest(unsent: bool) -> libc::c_short {
    if unsent {
        libc::POLLIN | libc::POLLOUT
    } else {
        libc::POLLIN
    }
}

/// `Ok` for a socket that would block, which is no error; else the error.
fn would_block(error: wayland_client::backend::WaylandError) -> Result<(), String> {
    match error {
        wayland_client::backend::WaylandError::Io(e)
            if e.kind() == std::io::ErrorKind::WouldBlock =>
        {
            Ok(())
        }
        e => Err(e.to_string()),
    }
}

/// A memory file of `size` zero bytes.
pub(crate) fn memfd(name: &str, size: u64) -> Option<OwnedFd> {
    let name = std::ffi::CString::new(name).ok()?;
    // SAFETY: a valid C string; the fd returned is owned from here on.
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // SAFETY: a file descriptor just created, owned by none else.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: a valid descriptor.
    if unsafe { libc::ftruncate(fd.as_raw_fd(), i64::try_from(size).ok()?) } != 0 {
        return None;
    }
    Some(fd)
}

/// A memory file holding `contents`.
pub(crate) fn memfd_with(name: &str, contents: &[u8]) -> Option<OwnedFd> {
    use std::io::Write as _;
    let fd = memfd(name, 0)?;
    let mut file = std::fs::File::from(fd);
    file.write_all(contents).ok()?;
    Some(OwnedFd::from(file))
}

/// The keymap the seat sent in `fd`, `size` bytes, mapped as the protocol
/// asks rather than read: the file may be the seat's for all its clients,
/// whose offset reading would move.
fn read_keymap(fd: &OwnedFd, size: u32) -> Option<Vec<u8>> {
    if size == 0 || size > MAX_KEYMAP {
        return None;
    }
    let len = size as usize;
    // SAFETY: a fresh private, read-only mapping of a valid descriptor.
    let map = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            fd.as_raw_fd(),
            0,
        )
    };
    if map == libc::MAP_FAILED {
        return None;
    }
    // SAFETY: the mapping is `len` bytes, readable, and lives until the
    // unmap below.
    let contents = unsafe { std::slice::from_raw_parts(map.cast::<u8>(), len) }.to_vec();
    // SAFETY: the mapping made above, no longer borrowed.
    unsafe { libc::munmap(map, len) };
    Some(contents)
}

/// Reads all a wake socket holds.
pub(crate) fn drain(wake: &impl AsRawFd) {
    let mut buf = [0u8; 64];
    loop {
        // SAFETY: a valid descriptor and a buffer of its length.
        let n = unsafe {
            libc::recv(
                wake.as_raw_fd(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                libc::MSG_DONTWAIT,
            )
        };
        if n <= 0 {
            return;
        }
    }
}

impl Follows for State {
    fn outputs(&mut self) -> &mut Outputs {
        &mut self.outputs
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

impl Dispatch<wl_callback::WlCallback, Fence> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        fence: &Fence,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.events.push_back(Event::Fenced(fence.0));
        }
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

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities { capabilities } = event else {
            return;
        };
        let caps = match capabilities {
            WEnum::Value(caps) => caps,
            WEnum::Unknown(bits) => wl_seat::Capability::from_bits_truncate(bits),
        };
        let has_pointer = caps.contains(wl_seat::Capability::Pointer);
        let has_keyboard = caps.contains(wl_seat::Capability::Keyboard);
        if state.wants.pointer && has_pointer && state.pointer.is_none() {
            let pointer = seat.get_pointer(qh, ());
            state.relative = state
                .relative_manager
                .as_ref()
                .map(|m| m.get_relative_pointer(&pointer, qh, ()));
            state.pointer = Some(pointer);
        } else if !has_pointer && let Some(pointer) = state.pointer.take() {
            if let Some(relative) = state.relative.take() {
                relative.destroy();
            }
            if pointer.version() >= 3 {
                pointer.release();
            }
        }
        if state.wants.keyboard && has_keyboard && state.keyboard.is_none() {
            state.keyboard = Some(seat.get_keyboard(qh, ()));
        } else if !has_keyboard && let Some(keyboard) = state.keyboard.take() {
            if keyboard.version() >= 3 {
                keyboard.release();
            }
            state.keymap = None;
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let ev = match event {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => Event::PointerEnter {
                surface: surface.id(),
                serial,
                x: surface_x,
                y: surface_y,
            },
            wl_pointer::Event::Leave { surface, .. } => Event::PointerLeave {
                surface: surface.id(),
            },
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => Event::PointerMotion {
                x: surface_x,
                y: surface_y,
            },
            wl_pointer::Event::Button {
                button, state: s, ..
            } => Event::Button {
                button,
                pressed: s == WEnum::Value(wl_pointer::ButtonState::Pressed),
            },
            wl_pointer::Event::Axis {
                axis: WEnum::Value(axis),
                value,
                ..
            } => Event::Axis { axis, value },
            wl_pointer::Event::AxisDiscrete {
                axis: WEnum::Value(axis),
                discrete,
            } => Event::AxisSteps {
                axis,
                value120: discrete.saturating_mul(120),
            },
            wl_pointer::Event::AxisValue120 {
                axis: WEnum::Value(axis),
                value120,
            } => Event::AxisSteps { axis, value120 },
            wl_pointer::Event::Frame => Event::Frame,
            _ => return,
        };
        state.events.push_back(ev);
    }
}

impl Dispatch<relative::ZwpRelativePointerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &relative::ZwpRelativePointerV1,
        event: relative::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let relative::Event::RelativeMotion { dx, dy, .. } = event {
            state.events.push_back(Event::Relative { dx, dy });
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                if state.wants.keymap && format == WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) {
                    match read_keymap(&fd, size) {
                        Some(keymap) => {
                            state.keymap = Some(keymap);
                            state.keymap_changed = true;
                        }
                        None => debug!(size, "cannot read the seat's keymap"),
                    }
                }
            }
            wl_keyboard::Event::Enter { surface, keys, .. } => {
                let keys = keys
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| u32::from_ne_bytes(*c))
                    .collect();
                state.events.push_back(Event::KeyboardEnter {
                    surface: surface.id(),
                    keys,
                });
            }
            wl_keyboard::Event::Leave { surface, .. } => {
                state.events.push_back(Event::KeyboardLeave {
                    surface: surface.id(),
                });
            }
            wl_keyboard::Event::Key { key, state: s, .. } => {
                state.events.push_back(Event::Key {
                    key,
                    pressed: s == WEnum::Value(wl_keyboard::KeyState::Pressed),
                });
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => state.repeat = Some((rate, delay)),
            _ => {}
        }
    }
}

impl Dispatch<LayerSurface, ObjectId> for State {
    fn event(
        state: &mut Self,
        _: &LayerSurface,
        event: zwlr_layer_surface_v1::Event,
        surface: &ObjectId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let ev = match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => Event::Configure {
                surface: surface.clone(),
                serial,
                width,
                height,
            },
            zwlr_layer_surface_v1::Event::Closed => Event::Closed {
                surface: surface.clone(),
            },
            _ => return,
        };
        state.events.push_back(ev);
    }
}

impl Dispatch<Inhibitor, ()> for State {
    fn event(
        _: &mut Self,
        _: &Inhibitor,
        _: inhibitor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

wayland_client::delegate_noop!(State: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(State: ignore wl_surface::WlSurface);
wayland_client::delegate_noop!(State: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(State: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(State: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
wayland_client::delegate_noop!(State: ignore wp_viewporter::WpViewporter);
wayland_client::delegate_noop!(State: ignore wp_viewport::WpViewport);
wayland_client::delegate_noop!(State: ignore relative_manager::ZwpRelativePointerManagerV1);
wayland_client::delegate_noop!(State: ignore inhibit_manager::ZwpKeyboardShortcutsInhibitManagerV1);
wayland_client::delegate_noop!(State: ignore PointerManager);
wayland_client::delegate_noop!(State: ignore VirtualPointer);
wayland_client::delegate_noop!(State: ignore KeyboardManager);
wayland_client::delegate_noop!(State: ignore VirtualKeyboard);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_waiting_for_room_wait_for_room_too() {
        assert_eq!(interest(false), libc::POLLIN);
        assert_eq!(interest(true), libc::POLLIN | libc::POLLOUT);
    }

    #[test]
    fn waking_a_busy_thread_never_waits() {
        let (wake, woken) = wake_pair().unwrap();
        // Far more than the socket's queue holds, nobody reading.
        let started = Instant::now();
        for _ in 0..1000 {
            let _ = wake.send(&[1]);
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        drain(&woken);
        assert!(!ready(&woken, libc::POLLIN, Duration::from_millis(20)));
        // Drained, it wakes again.
        wake.send(&[1]).unwrap();
        assert!(ready(&woken, libc::POLLIN, Duration::from_secs(1)));
    }
}
