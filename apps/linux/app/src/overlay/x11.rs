//! Overlays on X11: override-redirect windows the window manager leaves
//! alone, placed where they belong, cut to their picture's shape (so they
//! need no compositor), with an empty input shape so clicks go through,
//! and never focused. Their own thread owns their own connection to the X
//! server: it draws what the GTK side asks for at the scale of the monitor
//! it goes on, repaints on Expose, takes things down when their time is up
//! (also while the cursor is being shown) and drains the server's events.
//!
//! The edge glow fades, which needs a compositor
//! to blend it: with one (a `_NET_WM_CM_S<n>` owner) it is a window of a
//! 32-bit visual drawn anew every frame; without, a solid line in the
//! thread color that the server fills, as with high contrast.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use gtk::cairo;
use inputweave_app_core::edge_glow::{self, Glows, PixelRect, Timing};
use inputweave_app_core::proto::{Screen, ScreenEdge};
use inputweave_app_core::screen_identity::{self, Monitor};
use inputweave_app_core::text::keys;
use inputweave_app_core::{log, ui};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xfixes::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    ClipOrdering, ColormapAlloc, ConfigureWindowAux, ConnectionExt as _, CreateGCAux,
    CreateWindowAux, EventMask, ImageFormat, Rectangle, StackMode, VisualClass, WindowClass,
};
use x11rb::rust_connection::RustConnection;

use super::Ended;
use super::paint::{self, Picture};
use crate::strings::tr;
use crate::style::Brand;

/// A monitor's place in X's root window, in pixels, and GDK's scale for it.
#[derive(Clone, Copy, Debug)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale: i32,
}

/// What to identify one monitor with.
pub struct Identify {
    /// The monitors, in X's pixels, with what GDK says of each.
    pub monitors: Vec<(Area, Monitor)>,
    /// This computer's screens as the arrangement has them.
    pub arranged: Vec<Screen>,
    pub name: String,
    pub jump: Option<String>,
    pub brand: Brand,
    pub shown: Duration,
}

/// An edge of a monitor to light where the cursor crossed it.
pub struct Glow {
    /// The monitor, as the background service names it.
    pub monitor: String,
    /// It, in X's pixels.
    pub area: Area,
    pub edge: ScreenEdge,
    /// Where along the edge it crossed, 0 to 1.
    pub spot: f64,
    pub color: [u8; 3],
    /// High contrast: a solid line.
    pub solid: bool,
    pub timing: Timing,
}

enum Request {
    Glow(Glow),
    Hud {
        text: String,
        brand: Brand,
        monitors: Vec<Area>,
        shown: Duration,
    },
    Identify(Identify),
    Locate(Duration),
}

pub struct X11Overlays {
    requests: Sender<Request>,
    /// Whether the thread still runs: it ends when it loses the server.
    running: Arc<AtomicBool>,
}

/// How often the thread looks at the server's events while something is
/// shown, as for an Expose.
const POLL: Duration = Duration::from_millis(50);

impl X11Overlays {
    /// Connects to the X server the app is on; `None` when it cannot.
    pub fn start() -> Option<X11Overlays> {
        let (connection, screen) = x11rb::connect(None)
            .map_err(|e| log::info(&format!("no overlays: cannot reach the X server: {e}")))
            .ok()?;
        let (requests, received) = channel();
        let running = Arc::new(AtomicBool::new(true));
        let ended = Ended(running.clone());
        std::thread::Builder::new()
            .name("x11-overlays".into())
            .spawn(move || {
                let _ended = ended;
                run(connection, screen, received);
            })
            .ok()?;
        Some(X11Overlays { requests, running })
    }

    /// The HUD at the top of the monitor the pointer is on, for `shown`.
    /// False when the thread is gone, and nothing will show it.
    pub fn hud(&self, text: &str, brand: Brand, monitors: Vec<Area>, shown: Duration) -> bool {
        self.send(Request::Hud {
            text: text.to_owned(),
            brand,
            monitors,
            shown,
        })
    }

    /// This computer's name on each of its monitors; false as `hud`.
    pub fn identify(&self, identify: Identify) -> bool {
        self.send(Request::Identify(identify))
    }

    /// The cursor grown and back where it is, for `shown`; false as `hud`.
    pub fn locate(&self, shown: Duration) -> bool {
        self.send(Request::Locate(shown))
    }

    /// Lights a monitor's edge; false as `hud`.
    pub fn glow(&self, glow: Glow) -> bool {
        self.send(Request::Glow(glow))
    }

    fn send(&self, request: Request) -> bool {
        self.running.load(Ordering::SeqCst) && self.requests.send(request).is_ok()
    }
}

/// A window up for a while; with a picture, drawn again on Expose (windows
/// of a plain color the server fills itself).
struct Shown {
    window: u32,
    picture: Option<Picture>,
    until: Instant,
}

/// What windows of a 32-bit visual need, for glows a compositor blends.
struct Argb {
    visual: u32,
    colormap: u32,
    /// A graphics context for that depth (the other one is the root's).
    gc: u32,
}

/// The window showing one edge's glow.
struct GlowWindow {
    key: (String, i32),
    window: u32,
    rect: PixelRect,
    edge: ScreenEdge,
    color: [u8; 3],
    /// Drawn with alpha, every frame; otherwise a solid line the server
    /// fills.
    blended: bool,
    /// Its pixels, kept between frames.
    pixels: Vec<u8>,
    /// The opacity and spot last drawn; `None` to draw the next frame
    /// whatever it is (new, or exposed).
    drawn: Option<(f64, f64)>,
}

struct Server {
    conn: RustConnection,
    root: u32,
    depth: u8,
    /// `_NET_WM_CM_S<screen>`, owned while a compositor runs.
    compositor: u32,
    /// Whether it has an owner, as XFixes last said (`tracked`), or as
    /// last asked (`asked`, again after `RECHECK` without XFixes).
    composited: bool,
    tracked: bool,
    asked: Option<Instant>,
    argb: Option<Argb>,
    glows: Glows<(String, i32)>,
    glow_windows: Vec<GlowWindow>,
    /// When a glow's frame was last drawn.
    glow_drawn: Option<Instant>,
    /// One graphics context for every picture put in a window.
    gc: u32,
    /// The name of X's primary output, as last asked: `None` until asked,
    /// and again after RandR says the outputs changed.
    primary: Option<Option<String>>,
    /// Whether an X error was logged yet; one is enough.
    told_error: bool,
    shown: Vec<Shown>,
    /// The windows of the last HUD note and of the last screens' names, so
    /// a new one replaces them.
    hud: Vec<u32>,
    names: Vec<u32>,
}

/// A 32-bit TrueColor visual of the screen's, with a colormap and a
/// graphics context for it; `None` where the server has none.
fn argb(conn: &RustConnection, screen: usize) -> Option<Argb> {
    let root = &conn.setup().roots[screen];
    let visual = root
        .allowed_depths
        .iter()
        .filter(|d| d.depth == 32)
        .flat_map(|d| d.visuals.iter())
        .find(|v| v.class == VisualClass::TRUE_COLOR)?
        .visual_id;
    let colormap = conn.generate_id().ok()?;
    conn.create_colormap(ColormapAlloc::NONE, colormap, root.root, visual)
        .ok()?;
    // A graphics context is made for a drawable of its depth.
    let pixmap = conn.generate_id().ok()?;
    conn.create_pixmap(32, pixmap, root.root, 1, 1).ok()?;
    let gc = conn.generate_id().ok()?;
    let made = conn.create_gc(gc, pixmap, &CreateGCAux::new()).is_ok();
    let _ = conn.free_pixmap(pixmap);
    made.then_some(Argb {
        visual,
        colormap,
        gc,
    })
}

fn run(conn: RustConnection, screen: usize, requests: Receiver<Request>) {
    let root = conn.setup().roots[screen].root;
    let depth = conn.setup().roots[screen].root_depth;
    let compositor = conn
        .intern_atom(false, format!("_NET_WM_CM_S{screen}").as_bytes())
        .ok()
        .and_then(|c| c.reply().ok())
        .map_or(x11rb::NONE, |r| r.atom);
    let argb = argb(&conn, screen);
    // XFixes answers nothing but its version until a client asks for one,
    // and SHAPE and RandR likewise want to be met first.
    if let Err(e) = conn
        .xfixes_query_version(5, 0)
        .map_err(|e| e.to_string())
        .and_then(|c| c.reply().map(|_| ()).map_err(|e| e.to_string()))
    {
        log::info(&format!("no cursor image from XFixes: {e}"));
    }
    let _ = conn.shape_query_version().map(|c| c.reply());
    // A compositor coming or going is told rather than asked for at each
    // crossing.
    let tracked = compositor != x11rb::NONE
        && conn
            .xfixes_select_selection_input(
                root,
                compositor,
                xfixes::SelectionEventMask::SET_SELECTION_OWNER
                    | xfixes::SelectionEventMask::SELECTION_WINDOW_DESTROY
                    | xfixes::SelectionEventMask::SELECTION_CLIENT_CLOSE,
            )
            .ok()
            .and_then(|c| c.check().ok())
            .is_some();
    let composited = owned(&conn, compositor);
    if conn
        .randr_query_version(1, 3)
        .ok()
        .and_then(|c| c.reply().ok())
        .is_some()
    {
        let _ = conn.randr_select_input(
            root,
            randr::NotifyMask::SCREEN_CHANGE | randr::NotifyMask::OUTPUT_CHANGE,
        );
    }
    let gc = match conn.generate_id() {
        Ok(gc) if conn.create_gc(gc, root, &CreateGCAux::new()).is_ok() => gc,
        _ => {
            log::info("no overlays: cannot make a graphics context");
            return;
        }
    };
    let mut server = Server {
        conn,
        root,
        depth,
        compositor,
        composited,
        tracked,
        asked: Some(Instant::now()),
        argb,
        glows: Glows::default(),
        glow_windows: Vec::new(),
        glow_drawn: None,
        gc,
        primary: None,
        told_error: false,
        shown: Vec::new(),
        hud: Vec::new(),
        names: Vec::new(),
    };
    loop {
        let wait = server
            .shown
            .iter()
            .map(|s| s.until.saturating_duration_since(Instant::now()))
            .min()
            .map_or(Duration::from_secs(3600), |d| d.min(POLL));
        // A glow's next change: frames while it fades or slides, else its
        // next phase (a line the server fills never changes); at once for
        // one to draw whole again (exposed), and the server's events read
        // at least every POLL meanwhile, for an Expose.
        let glow_windows = &server.glow_windows;
        let wait = server
            .glows
            .wake(Instant::now(), server.glow_drawn, |key| {
                glow_windows.iter().any(|w| &w.key == key && w.blended)
            })
            .map_or(wait, |d| wait.min(d).min(POLL));
        let wait = if glow_windows.iter().any(|w| w.blended && w.drawn.is_none()) {
            Duration::ZERO
        } else {
            wait
        };
        match requests.recv_timeout(wait) {
            Ok(Request::Glow(glow)) => {
                if let Err(e) = light(&mut server, &glow) {
                    log::info(&format!("cannot light the screen's edge: {e}"));
                }
            }
            Ok(Request::Hud {
                text,
                brand,
                monitors,
                shown,
            }) => {
                if let Err(e) = show_hud(&mut server, &text, &brand, &monitors, shown) {
                    log::info(&format!("cannot show the HUD: {e}"));
                }
            }
            Ok(Request::Identify(identify)) => {
                if let Err(e) = show_identify(&mut server, &identify) {
                    log::info(&format!("cannot show the screens' names: {e}"));
                }
            }
            Ok(Request::Locate(shown)) => {
                if let Err(e) = locate(&mut server, shown) {
                    log::info(&format!("cannot show where the cursor is: {e}"));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if let Err(e) = tick(&mut server) {
            log::info(&format!("no overlays any more: lost the X server: {e}"));
            return;
        }
    }
}

/// Takes down what is past its time, and takes the server's events:
/// repainting on Expose, forgetting the primary output when RandR says the
/// outputs changed, logging the first error. An error when the connection
/// is gone.
fn tick(server: &mut Server) -> Result<(), x11rb::errors::ConnectionError> {
    let now = Instant::now();
    let (gone, kept): (Vec<Shown>, Vec<Shown>) = std::mem::take(&mut server.shown)
        .into_iter()
        .partition(|s| now >= s.until);
    server.shown = kept;
    for s in gone {
        let _ = server.conn.destroy_window(s.window);
    }
    // Events first: an Expose has the glow drawn whole in this frame.
    events(server)?;
    glow_frame(server)?;
    server.conn.flush()
}

/// Takes the server's events read so far: repaints on Expose, forgets the
/// primary output when RandR says the outputs changed, follows the
/// compositor's selection, logs the first error.
fn events(server: &mut Server) -> Result<(), x11rb::errors::ConnectionError> {
    while let Some(event) = server.conn.poll_for_event()? {
        match event {
            Event::Expose(e) if e.count == 0 => {
                // A glow's next frame draws it whole again.
                if let Some(w) = server
                    .glow_windows
                    .iter_mut()
                    .find(|w| w.window == e.window)
                {
                    w.drawn = None;
                }
                if let Some(Shown {
                    window,
                    picture: Some(picture),
                    ..
                }) = server.shown.iter().find(|s| s.window == e.window)
                {
                    let _ = draw(server, *window, picture);
                }
            }
            Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => server.primary = None,
            Event::XfixesSelectionNotify(e) if e.selection == server.compositor => {
                server.composited = e.subtype == xfixes::SelectionEvent::SET_SELECTION_OWNER
                    && e.owner != x11rb::NONE;
            }
            Event::Error(e) if !server.told_error => {
                server.told_error = true;
                log::info(&format!("an overlay's X request failed: {e:?}"));
            }
            _ => {}
        }
    }
    Ok(())
}

type Failed = Box<dyn std::error::Error>;

/// A window at (x, y) of `width` by `height`, never focused, clicks going
/// through; filled with `pixel` by the server, or, without, left as it is
/// until a picture is put in it (no black between an Expose or a resize
/// and the drawing).
fn plain_window(
    server: &Server,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    pixel: Option<u32>,
) -> Result<u32, Failed> {
    let conn = &server.conn;
    let window = conn.generate_id()?;
    let aux = CreateWindowAux::new()
        .override_redirect(1)
        .event_mask(EventMask::EXPOSURE);
    let aux = match pixel {
        Some(pixel) => aux.background_pixel(pixel),
        None => aux.background_pixmap(x11rb::NONE),
    };
    conn.create_window(
        server.depth,
        window,
        server.root,
        x as i16,
        y as i16,
        width.max(1) as u16,
        height.max(1) as u16,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &aux,
    )?;
    // Clicks go through: no input shape at all.
    let cut = conn.shape_rectangles(
        shape::SO::SET,
        shape::SK::INPUT,
        ClipOrdering::UNSORTED,
        window,
        0,
        0,
        &[],
    );
    if let Err(e) = cut {
        let _ = conn.destroy_window(window);
        return Err(e.into());
    }
    Ok(window)
}

fn map_above(server: &Server, window: u32) -> Result<(), Failed> {
    server.conn.map_window(window)?;
    server.conn.configure_window(
        window,
        &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
    )?;
    Ok(())
}

/// A window showing `picture` at (x, y), cut to it; gone again if any of
/// that fails.
fn window_for(server: &Server, picture: &Picture, x: i32, y: i32) -> Result<u32, Failed> {
    let window = plain_window(server, x, y, picture.width, picture.height, None)?;
    let shown = shape_to(&server.conn, window, picture)
        .and_then(|()| map_above(server, window))
        .and_then(|()| draw(server, window, picture));
    if let Err(e) = shown {
        let _ = server.conn.destroy_window(window);
        let _ = server.conn.flush();
        return Err(e);
    }
    Ok(window)
}

/// Cuts the window to where the picture is mostly opaque, row by row.
fn shape_to(conn: &RustConnection, window: u32, picture: &Picture) -> Result<(), Failed> {
    let mut rects = Vec::new();
    for y in 0..picture.height {
        let mut x = 0;
        while x < picture.width {
            while x < picture.width && picture.alpha(x, y) < 128 {
                x += 1;
            }
            let start = x;
            while x < picture.width && picture.alpha(x, y) >= 128 {
                x += 1;
            }
            if x > start {
                rects.push(Rectangle {
                    x: start as i16,
                    y: y as i16,
                    width: (x - start) as u16,
                    height: 1,
                });
            }
        }
    }
    conn.shape_rectangles(
        shape::SO::SET,
        shape::SK::BOUNDING,
        ClipOrdering::UNSORTED,
        window,
        0,
        0,
        &rects,
    )?;
    Ok(())
}

/// Puts the picture's pixels in the window: 32 bits a pixel, which a depth
/// of 24 reads as XRGB, the same bytes as cairo's on little-endian.
fn draw(server: &Server, window: u32, picture: &Picture) -> Result<(), Failed> {
    Ok(put(&server.conn, window, server.gc, server.depth, picture)?)
}

/// As `draw`, at `depth` with a graphics context for it.
fn put(
    conn: &RustConnection,
    window: u32,
    gc: u32,
    depth: u8,
    picture: &Picture,
) -> Result<(), x11rb::errors::ConnectionError> {
    // Rows at a time, to stay under the request size limit.
    let rows_per = (200_000 / picture.stride.max(1)).max(1);
    let mut y = 0;
    while y < picture.height {
        let rows = rows_per.min(picture.height - y);
        let start = (y * picture.stride) as usize;
        let end = ((y + rows) * picture.stride) as usize;
        conn.put_image(
            ImageFormat::Z_PIXMAP,
            window,
            gc,
            (picture.stride / 4) as u16,
            rows as u16,
            0,
            y as i16,
            0,
            depth,
            &picture.data[start..end],
        )?;
        y += rows;
    }
    Ok(())
}

fn pointer(conn: &RustConnection, root: u32) -> Result<(i32, i32), Failed> {
    let reply = conn.query_pointer(root)?.reply()?;
    Ok((i32::from(reply.root_x), i32::from(reply.root_y)))
}

/// Takes down the windows `old` and shows `new`.
fn replace(server: &mut Server, old: &[u32], new: Vec<Shown>) {
    let (gone, kept): (Vec<Shown>, Vec<Shown>) = std::mem::take(&mut server.shown)
        .into_iter()
        .partition(|s| old.contains(&s.window));
    for s in gone {
        let _ = server.conn.destroy_window(s.window);
    }
    server.shown = kept;
    server.shown.extend(new);
}

fn show_hud(
    server: &mut Server,
    text: &str,
    brand: &Brand,
    monitors: &[Area],
    shown: Duration,
) -> Result<(), Failed> {
    let (px, py) = pointer(&server.conn, server.root)?;
    let geometry = server.conn.get_geometry(server.root)?.reply()?;
    let whole = Area {
        x: 0,
        y: 0,
        width: i32::from(geometry.width),
        height: i32::from(geometry.height),
        scale: 1,
    };
    let area = monitors
        .iter()
        .find(|m| px >= m.x && px < m.x + m.width && py >= m.y && py < m.y + m.height)
        .copied()
        .unwrap_or(whole);
    let picture = paint::hud(text, brand, area.scale)?;
    let x = area.x + (area.width - picture.width) / 2;
    let y = area.y + 48 * area.scale;
    let window = window_for(server, &picture, x, y)?;
    let old = std::mem::replace(&mut server.hud, vec![window]);
    replace(
        server,
        &old,
        vec![Shown {
            window,
            picture: Some(picture),
            until: Instant::now() + shown,
        }],
    );
    Ok(())
}

/// X's primary output, asked once and again after the outputs changed.
fn primary(server: &mut Server) -> Option<String> {
    if let Some(known) = &server.primary {
        return known.clone();
    }
    let name = (|| -> Option<String> {
        let output = server
            .conn
            .randr_get_output_primary(server.root)
            .ok()?
            .reply()
            .ok()?
            .output;
        if output == 0 {
            return None;
        }
        let info = server
            .conn
            .randr_get_output_info(output, 0)
            .ok()?
            .reply()
            .ok()?;
        String::from_utf8(info.name).ok()
    })();
    server.primary = Some(name.clone());
    name
}

/// Each monitor framed in the thread color (four windows the server fills)
/// with the name's card in the middle. Failing part way, what was made is
/// taken down again, so no frame stays on screen.
fn show_identify(server: &mut Server, identify: &Identify) -> Result<(), Failed> {
    let mut new = Vec::new();
    if let Err(e) = identify_windows(server, identify, &mut new) {
        for s in new {
            let _ = server.conn.destroy_window(s.window);
        }
        let _ = server.conn.flush();
        return Err(e);
    }
    let windows: Vec<u32> = new.iter().map(|s| s.window).collect();
    let old = std::mem::replace(&mut server.names, windows);
    replace(server, &old, new);
    Ok(())
}

/// Makes `show_identify`'s windows, each put in `new` as soon as it exists.
fn identify_windows(
    server: &mut Server,
    identify: &Identify,
    new: &mut Vec<Shown>,
) -> Result<(), Failed> {
    let primary = primary(server);
    let monitors: Vec<Monitor> = identify
        .monitors
        .iter()
        .map(|(_, m)| Monitor {
            primary: screen_identity::arranged_primary(&identify.arranged, &m.connector)
                || primary.as_deref() == Some(m.connector.as_str()),
            ..m.clone()
        })
        .collect();
    let labels = screen_identity::labels(&identify.arranged, &monitors);
    let t = identify.brand.thread;
    let pixel = (((t[0] * 255.0).round() as u32) << 16)
        | (((t[1] * 255.0).round() as u32) << 8)
        | (t[2] * 255.0).round() as u32;
    let until = Instant::now() + identify.shown;
    for ((area, _), label) in identify.monitors.iter().zip(labels) {
        let line = tr(&ui!(
            keys::SCREEN_NUMBERED,
            label.number.to_string(),
            label.size
        ));
        let frame = (paint::FRAME as i32) * area.scale;
        let (x, y, w, h) = (area.x, area.y, area.width, area.height);
        for (fx, fy, fw, fh) in [
            (x, y, w, frame),
            (x, y + h - frame, w, frame),
            (x, y, frame, h),
            (x + w - frame, y, frame, h),
        ] {
            let window = plain_window(server, fx, fy, fw, fh, Some(pixel))?;
            new.push(Shown {
                window,
                picture: None,
                until,
            });
            map_above(server, window)?;
        }
        let picture = paint::identify_card(
            &identify.name,
            &line,
            identify.jump.as_deref(),
            &identify.brand,
            area.scale,
        )?;
        let window = window_for(
            server,
            &picture,
            x + (w - picture.width) / 2,
            y + (h - picture.height) / 2,
        )?;
        new.push(Shown {
            window,
            picture: Some(picture),
            until,
        });
    }
    Ok(())
}

/// The cursor at up to four times its size and back, following the
/// pointer, as the other two apps show it. Its
/// window goes whatever happens on the way; what else is shown keeps
/// being repainted and taken down meanwhile.
fn locate(server: &mut Server, shown: Duration) -> Result<(), Failed> {
    let mut window: Option<u32> = None;
    let result = grow_cursor(server, shown, &mut window);
    if let Some(w) = window {
        let _ = server.conn.destroy_window(w);
        let _ = server.conn.flush();
    }
    result
}

fn grow_cursor(
    server: &mut Server,
    shown: Duration,
    window: &mut Option<u32>,
) -> Result<(), Failed> {
    let image = server.conn.xfixes_get_cursor_image()?.reply()?;
    let (w, h) = (i32::from(image.width), i32::from(image.height));
    if w == 0 || h == 0 {
        return Ok(());
    }
    let mut cursor = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h)?;
    {
        let stride = cursor.stride() as usize;
        let mut data = cursor.data()?;
        for (i, pixel) in image.cursor_image.iter().enumerate() {
            // XFixes gives premultiplied ARGB, as cairo has it.
            let (x, y) = (i % w as usize, i / w as usize);
            let at = y * stride + x * 4;
            data[at..at + 4].copy_from_slice(&pixel.to_ne_bytes());
        }
    }
    cursor.mark_dirty();
    let (hx, hy) = (f64::from(image.xhot), f64::from(image.yhot));
    let start = Instant::now();
    while start.elapsed() < shown {
        let t = start.elapsed().as_secs_f64() / shown.as_secs_f64();
        // Up in the first sixth, held, then back over the last third.
        let scale = if t < 0.15 {
            1.0 + 3.0 * (1.0 - (1.0 - t / 0.15).powi(3))
        } else if t < 0.65 {
            4.0
        } else {
            1.0 + 3.0 * (1.0 - ((t - 0.65) / 0.35).clamp(0.0, 1.0)).powi(2)
        };
        let (pw, ph) = (
            (f64::from(w) * scale).ceil() as i32,
            (f64::from(h) * scale).ceil() as i32,
        );
        let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, pw.max(1), ph.max(1))?;
        {
            let cr = cairo::Context::new(&surface)?;
            cr.scale(scale, scale);
            cr.set_source_surface(&cursor, 0.0, 0.0)?;
            cr.paint()?;
        }
        surface.flush();
        let picture = Picture {
            width: surface.width(),
            height: surface.height(),
            stride: surface.stride(),
            data: surface.data()?.to_vec(),
        };
        let (px, py) = pointer(&server.conn, server.root)?;
        let (x, y) = (px - (hx * scale) as i32, py - (hy * scale) as i32);
        // One window, moved, cut and drawn again for each frame.
        match *window {
            Some(win) => {
                server.conn.configure_window(
                    win,
                    &ConfigureWindowAux::new()
                        .x(x)
                        .y(y)
                        .width(picture.width as u32)
                        .height(picture.height as u32)
                        .stack_mode(StackMode::ABOVE),
                )?;
                shape_to(&server.conn, win, &picture)?;
                draw(server, win, &picture)?;
            }
            None => *window = Some(window_for(server, &picture, x, y)?),
        }
        tick(server)?;
        std::thread::sleep(Duration::from_millis(16));
    }
    Ok(())
}

/// Without XFixes to say so, how long the compositor's selection is taken
/// as last asked.
const RECHECK: Duration = Duration::from_secs(5);

/// Whether `selection` has an owner (one round trip).
fn owned(conn: &RustConnection, selection: u32) -> bool {
    selection != x11rb::NONE
        && conn
            .get_selection_owner(selection)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|r| r.owner != x11rb::NONE)
}

/// Whether a compositor runs, which blends windows with alpha: as XFixes
/// last said, with the events read so far taken first; without XFixes, as
/// asked at most every `RECHECK`.
fn composited(server: &mut Server) -> Result<bool, x11rb::errors::ConnectionError> {
    events(server)?;
    if !server.tracked && server.asked.is_none_or(|at| at.elapsed() >= RECHECK) {
        server.composited = owned(&server.conn, server.compositor);
        server.asked = Some(Instant::now());
    }
    Ok(server.composited)
}

/// Lights an edge, or carries its glow on (`Glows::light`); its window is
/// made, or made anew when the monitor or the way it is drawn changed.
fn light(server: &mut Server, glow: &Glow) -> Result<(), Failed> {
    let key = (glow.monitor.clone(), glow.edge as i32);
    let blended = !glow.solid && server.argb.is_some() && composited(server)?;
    let area = PixelRect {
        x: glow.area.x,
        y: glow.area.y,
        width: glow.area.width,
        height: glow.area.height,
    };
    let depth = if blended {
        edge_glow::DEPTH
    } else {
        edge_glow::SOLID_DEPTH
    };
    let rect = edge_glow::strip(area, f64::from(glow.area.scale), glow.edge, depth);
    server
        .glows
        .light(key.clone(), glow.spot, Instant::now(), glow.timing);
    let same = |w: &GlowWindow| {
        w.key == key && w.rect == rect && w.blended == blended && w.color == glow.color
    };
    if server.glow_windows.iter().any(same) {
        return Ok(());
    }
    if let Some(i) = server.glow_windows.iter().position(|w| w.key == key) {
        let old = server.glow_windows.remove(i);
        let _ = server.conn.destroy_window(old.window);
    }
    let window = if blended {
        argb_window(server, rect)?
    } else {
        let [r, g, b] = glow.color.map(u32::from);
        plain_window(
            server,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            Some(r << 16 | g << 8 | b),
        )?
    };
    if let Err(e) = map_above(server, window) {
        let _ = server.conn.destroy_window(window);
        return Err(e);
    }
    server.glow_windows.push(GlowWindow {
        key,
        window,
        rect,
        edge: glow.edge,
        color: glow.color,
        blended,
        pixels: Vec::new(),
        drawn: None,
    });
    Ok(())
}

/// A window of the 32-bit visual over `rect`, transparent until drawn,
/// clicks going through.
fn argb_window(server: &Server, rect: PixelRect) -> Result<u32, Failed> {
    let argb = server.argb.as_ref().ok_or("no 32-bit visual")?;
    let conn = &server.conn;
    let window = conn.generate_id()?;
    // A depth other than the parent's needs its own colormap and border.
    let aux = CreateWindowAux::new()
        .override_redirect(1)
        .background_pixel(0)
        .border_pixel(0)
        .colormap(argb.colormap)
        .event_mask(EventMask::EXPOSURE);
    conn.create_window(
        32,
        window,
        server.root,
        rect.x as i16,
        rect.y as i16,
        rect.width.max(1) as u16,
        rect.height.max(1) as u16,
        0,
        WindowClass::INPUT_OUTPUT,
        argb.visual,
        &aux,
    )?;
    let cut = conn.shape_rectangles(
        shape::SO::SET,
        shape::SK::INPUT,
        ClipOrdering::UNSORTED,
        window,
        0,
        0,
        &[],
    );
    if let Err(e) = cut {
        let _ = conn.destroy_window(window);
        return Err(e.into());
    }
    Ok(window)
}

/// Draws every glow still lit as it is now and takes down those gone; an
/// error when the connection is gone.
fn glow_frame(server: &mut Server) -> Result<(), x11rb::errors::ConnectionError> {
    if server.glow_windows.is_empty() && server.glows.is_empty() {
        return Ok(());
    }
    let frames = server.glows.frame(Instant::now());
    let (kept, gone): (Vec<GlowWindow>, Vec<GlowWindow>) = std::mem::take(&mut server.glow_windows)
        .into_iter()
        .partition(|w| frames.iter().any(|f| f.key == w.key));
    for w in gone {
        server.conn.destroy_window(w.window)?;
    }
    server.glow_windows = kept;
    let gc = server.argb.as_ref().map(|a| a.gc);
    for frame in &frames {
        let Some(w) = server.glow_windows.iter_mut().find(|w| w.key == frame.key) else {
            continue;
        };
        let (Some(gc), true) = (gc, w.blended) else {
            continue;
        };
        // Held, or the spot at rest: nothing new to send.
        if edge_glow::unchanged(w.drawn, (frame.opacity, frame.spot), false) {
            continue;
        }
        let (width, height) = (w.rect.width.max(1), w.rect.height.max(1));
        let (along, deep) = edge_glow::along_deep(w.edge, width as usize, height as usize);
        w.pixels.resize(along * deep * 4, 0);
        edge_glow::paint(
            &mut w.pixels,
            w.edge,
            along,
            deep,
            frame.spot,
            w.color,
            frame.opacity,
            false,
        );
        let picture = Picture {
            width,
            height,
            stride: width * 4,
            data: std::mem::take(&mut w.pixels),
        };
        let drawn = put(&server.conn, w.window, gc, 32, &picture);
        w.pixels = picture.data;
        drawn?;
        w.drawn = Some((frame.opacity, frame.spot));
        server.glow_drawn = Some(Instant::now());
    }
    Ok(())
}
