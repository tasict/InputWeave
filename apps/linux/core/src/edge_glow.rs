//! The parts of lighting up the edge where the cursor crosses to or from
//! another computer that need no window: which
//! monitor, the whole edge crossed, how bright along it, for how long, and
//! the pixels. As the macOS app's `EdgeGlowGeometry.swift` and the Windows
//! app's `EdgeGlow.cs`, with their test cases; pixels with y going down.

use std::time::{Duration, Instant};

use iw_proto::app::v1::{CursorCrossed, ScreenEdge};

/// Fading inward over about 24 view units.
pub const DEPTH: f64 = 24.0;

/// With high contrast, a solid line this thick instead, no fade.
pub const SOLID_DEPTH: f64 = 4.0;

/// How bright the glow stays along the edge away from where it crossed, at
/// the least, of its brightest.
pub const DIMMEST: f64 = 0.35;

/// How wide the bright part is, as a share of the edge's length (the
/// standard deviation of its bell curve).
pub const SPREAD: f64 = 0.25;

/// How opaque the glow is at the screen's edge; it fades to nothing inward.
pub const STRONGEST: f64 = 0.95;

/// How long the brightest part takes to slide to a new crossing of an edge
/// still lit.
pub const SLIDE: Duration = Duration::from_millis(150);

/// How long the glow comes up, stays and goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    pub fade_in: Duration,
    pub hold: Duration,
    pub fade_out: Duration,
}

impl Timing {
    pub fn total(self) -> Duration {
        self.fade_in + self.hold + self.fade_out
    }
}

/// In 0.1 seconds, held for 0.3, out in 0.4.
pub const ANIMATED: Timing = Timing {
    fade_in: Duration::from_millis(100),
    hold: Duration::from_millis(300),
    fade_out: Duration::from_millis(400),
};

/// With animations off: there at once for half a second, then gone.
pub const STILL: Timing = Timing {
    fade_in: Duration::ZERO,
    hold: Duration::from_millis(500),
    fade_out: Duration::ZERO,
};

/// The timing for the desktop's animation setting.
pub fn timing(reduce_motion: bool) -> Timing {
    if reduce_motion { STILL } else { ANIMATED }
}

/// How opaque the glow is `elapsed` after it lit up; `None` once it is
/// gone.
pub fn opacity(timing: Timing, elapsed: Duration) -> Option<f64> {
    if elapsed < timing.fade_in {
        return Some(elapsed.as_secs_f64() / timing.fade_in.as_secs_f64());
    }
    let Some(fading) = elapsed.checked_sub(timing.fade_in + timing.hold) else {
        return Some(1.0);
    };
    (fading < timing.fade_out).then(|| 1.0 - fading.as_secs_f64() / timing.fade_out.as_secs_f64())
}

/// One of the four edges a glow can light; anything else (unspecified, or
/// a value from a newer background service) lights nothing.
pub fn known(crossed: &CursorCrossed) -> Option<ScreenEdge> {
    match ScreenEdge::try_from(crossed.edge) {
        Ok(
            edge @ (ScreenEdge::Left | ScreenEdge::Right | ScreenEdge::Top | ScreenEdge::Bottom),
        ) => Some(edge),
        _ => None,
    }
}

/// A left or right edge, which runs down the screen.
pub fn upright(edge: ScreenEdge) -> bool {
    matches!(edge, ScreenEdge::Left | ScreenEdge::Right)
}

/// A glow's size `width` by `height` as how far it runs along `edge` and
/// how deep it goes in; its own inverse: `along_deep(edge, along, deep)`
/// is the width and height.
pub fn along_deep<T>(edge: ScreenEdge, width: T, height: T) -> (T, T) {
    if upright(edge) {
        (height, width)
    } else {
        (width, height)
    }
}

/// How often a glow is drawn anew while it fades or its brightest part
/// slides.
pub const FRAME: Duration = Duration::from_millis(16);

/// How far along its edge the crossing lies, from 0 at the top of a left
/// or right edge, or the left end of a top or bottom one, to 1 at the
/// other end.
pub fn fraction(crossed: &CursorCrossed) -> f64 {
    let (start, extent) = if upright(crossed.edge()) {
        (crossed.display_y, crossed.display_height)
    } else {
        (crossed.display_x, crossed.display_width)
    };
    if extent <= 0.0 || !crossed.along.is_finite() {
        return 0.5;
    }
    ((crossed.along - start) / extent).clamp(0.0, 1.0)
}

/// A monitor in the background service's units: X's pixels on X11, the
/// compositor's logical units on Wayland.
#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    /// Its connector ("DP-1"), as the service names displays.
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The monitor a display is: the one of its name, as the background
/// service names them; else the one at its place and size; else the only
/// one of its size; else none.
pub fn monitor_for(crossed: &CursorCrossed, monitors: &[Monitor]) -> Option<usize> {
    if let Some(i) = monitors
        .iter()
        .position(|m| !m.id.is_empty() && m.id.eq_ignore_ascii_case(&crossed.display_id))
    {
        return Some(i);
    }
    let close = |a: f64, b: f64| (a - b).abs() < 1.0;
    let same_size = |m: &Monitor| {
        close(m.width, crossed.display_width) && close(m.height, crossed.display_height)
    };
    if let Some(i) = monitors.iter().position(|m| {
        same_size(m) && close(m.x, crossed.display_x) && close(m.y, crossed.display_y)
    }) {
        return Some(i);
    }
    let mut sized = monitors.iter().enumerate().filter(|(_, m)| same_size(m));
    match (sized.next(), sized.next()) {
        (Some((i, _)), None) => Some(i),
        _ => None,
    }
}

/// A rectangle in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// The glow's frame on a monitor of `area` pixels at `scale`: the whole of
/// `edge`, `thickness` view units deep from it, never deeper than the
/// monitor.
pub fn strip(area: PixelRect, scale: f64, edge: ScreenEdge, thickness: f64) -> PixelRect {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let deep = ((thickness * scale).round() as i32).clamp(1, area.width.min(area.height).max(1));
    match edge {
        ScreenEdge::Left => PixelRect {
            width: deep,
            ..area
        },
        ScreenEdge::Right => PixelRect {
            x: area.x + area.width - deep,
            width: deep,
            ..area
        },
        ScreenEdge::Top => PixelRect {
            height: deep,
            ..area
        },
        _ => PixelRect {
            y: area.y + area.height - deep,
            height: deep,
            ..area
        },
    }
}

/// How bright the glow is at `position` along its edge (0 to 1, as
/// [`fraction`] measures) when the cursor crossed at `spot`: all of it
/// there, falling off smoothly toward both ends but never below
/// [`DIMMEST`], so the whole edge shows; about two fifths at the ends when
/// it crossed in the middle.
pub fn brightness(position: f64, spot: f64) -> f64 {
    let away = (position - spot) / SPREAD;
    DIMMEST + (1.0 - DIMMEST) * (-away * away / 2.0).exp()
}

/// Where the brightest part is `elapsed` after it set off from `from` to a
/// new crossing at `to`: easing out, there after [`SLIDE`].
pub fn spot_at(from: f64, to: f64, elapsed: Duration) -> f64 {
    if elapsed >= SLIDE {
        return to;
    }
    let t = elapsed.as_secs_f64() / SLIDE.as_secs_f64();
    let eased = 1.0 - (1.0 - t).powi(3);
    from + (to - from) * eased
}

/// The glow of `edge` into `pixels`, `along` pixels along the edge and
/// `deep` across it (so `deep` wide and `along` high for a left or right
/// edge), in cairo's ARGB32 (premultiplied, native-endian: Wayland's
/// ARGB8888 and an X11 32-bit pixel): strongest at the screen's edge and
/// fading to nothing inward, as bright along it as [`brightness`] says for
/// `spot`, all of it times `opacity`. `solid`: the high-contrast line, the
/// color all over.
#[allow(clippy::too_many_arguments)]
pub fn paint(
    pixels: &mut [u8],
    edge: ScreenEdge,
    along: usize,
    deep: usize,
    spot: f64,
    color: [u8; 3],
    opacity: f64,
    solid: bool,
) {
    let (width, _) = along_deep(edge, along, deep);
    let opacity = opacity.clamp(0.0, 1.0);
    for a in 0..along {
        let bright = if solid {
            1.0
        } else {
            brightness((a as f64 + 0.5) / along as f64, spot)
        };
        for d in 0..deep {
            // d counts inward from the screen's edge.
            let alpha = if solid {
                opacity
            } else {
                STRONGEST * bright * (1.0 - (d as f64 + 0.5) / deep as f64) * opacity
            };
            let (x, y) = match edge {
                ScreenEdge::Left => (d, a),
                ScreenEdge::Right => (deep - 1 - d, a),
                ScreenEdge::Top => (a, d),
                _ => (a, deep - 1 - d),
            };
            let channel = |c: u8| (f64::from(c) * alpha).round() as u32;
            let pixel = ((255.0 * alpha).round() as u32) << 24
                | channel(color[0]) << 16
                | channel(color[1]) << 8
                | channel(color[2]);
            let i = (y * width + x) * 4;
            if let Some(out) = pixels.get_mut(i..i + 4) {
                out.copy_from_slice(&pixel.to_ne_bytes());
            }
        }
    }
}

/// As [`paint`], into new pixels.
#[allow(clippy::too_many_arguments)]
pub fn pixels(
    edge: ScreenEdge,
    along: usize,
    deep: usize,
    spot: f64,
    color: [u8; 3],
    opacity: f64,
    solid: bool,
) -> Vec<u8> {
    let mut pixels = vec![0; along * deep * 4];
    paint(&mut pixels, edge, along, deep, spot, color, opacity, solid);
    pixels
}

/// When a crossing of an edge at `now` lights its glow from, the glow of
/// that same edge having lit at `previous`, if it did. While that is still
/// to be seen (coming up, held or fading), it carries on from how bright it
/// is: up to full again if it was fading, held anew, never dark in between,
/// so crossing back and forth does not flicker. Otherwise it lights up
/// anew, from now.
pub fn lit_at(previous: Option<Instant>, now: Instant, timing: Timing) -> Instant {
    match previous.and_then(|at| opacity(timing, now.saturating_duration_since(at))) {
        Some(opacity) => now
            .checked_sub(timing.fade_in.mul_f64(opacity))
            .unwrap_or(now),
        None => now,
    }
}

/// One edge of one monitor lit, on its own clock.
#[derive(Clone, Debug)]
struct Lit<K> {
    key: K,
    timing: Timing,
    /// When it lit up, as [`lit_at`] carries a glow still seen on.
    lit: Instant,
    /// The brightest part sliding from `from` to `to` since `moved`.
    from: f64,
    to: f64,
    moved: Instant,
}

impl<K> Lit<K> {
    fn spot(&self, now: Instant) -> f64 {
        spot_at(
            self.from,
            self.to,
            now.saturating_duration_since(self.moved),
        )
    }

    fn opacity(&self, now: Instant) -> Option<f64> {
        opacity(self.timing, now.saturating_duration_since(self.lit))
    }
}

/// What a lit edge shows at a moment.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame<K> {
    pub key: K,
    pub opacity: f64,
    /// Where along the edge it is brightest, 0 to 1.
    pub spot: f64,
}

/// The edges lit, each (monitor, edge) apart:
/// crossing an edge still seen carries its glow on from how bright it is
/// and slides the brightest part from where it is now to the new
/// crossing; with animations off it jumps there.
#[derive(Clone, Debug)]
pub struct Glows<K> {
    lit: Vec<Lit<K>>,
}

impl<K> Default for Glows<K> {
    fn default() -> Self {
        Glows { lit: Vec::new() }
    }
}

impl<K: PartialEq + Clone> Glows<K> {
    /// Lights `key` for a crossing at `spot` (0 to 1 along the edge).
    pub fn light(&mut self, key: K, spot: f64, now: Instant, timing: Timing) {
        match self
            .lit
            .iter_mut()
            .find(|l| l.key == key && l.opacity(now).is_some())
        {
            Some(lit) => {
                let from = if timing.fade_in.is_zero() {
                    spot
                } else {
                    lit.spot(now)
                };
                lit.lit = lit_at(Some(lit.lit), now, timing);
                lit.timing = timing;
                lit.from = from;
                lit.to = spot;
                lit.moved = now;
            }
            None => {
                self.lit.retain(|l| l.key != key);
                self.lit.push(Lit {
                    key,
                    timing,
                    lit: now,
                    from: spot,
                    to: spot,
                    moved: now,
                });
            }
        }
    }

    /// What every edge still lit shows at `now`; those gone are dropped.
    pub fn frame(&mut self, now: Instant) -> Vec<Frame<K>> {
        self.lit.retain(|l| l.opacity(now).is_some());
        self.lit
            .iter()
            .filter_map(|l| {
                Some(Frame {
                    key: l.key.clone(),
                    opacity: l.opacity(now)?,
                    spot: l.spot(now),
                })
            })
            .collect()
    }

    /// How long the overlay may sleep from `now` before a glow looks
    /// otherwise: a frame while one fades or slides (and one more, for its
    /// last step), else until its hold ends, or until it is gone for one
    /// that never changes on screen (`animated` false: a solid line); at
    /// once for one gone but not taken down yet (`frame` does). `None` with
    /// nothing lit.
    /// `drawn` is when the last frame was drawn: the next is due a frame
    /// after it, not a frame after whatever woke the overlay.
    pub fn wake(
        &self,
        now: Instant,
        drawn: Option<Instant>,
        animated: impl Fn(&K) -> bool,
    ) -> Option<Duration> {
        let frame = drawn.map_or(FRAME, |at| (at + FRAME).saturating_duration_since(now));
        self.lit
            .iter()
            .map(|l| {
                if l.opacity(now).is_none() {
                    return Duration::ZERO;
                }
                let t = l.timing;
                let end = l.lit + t.total();
                if !animated(&l.key) {
                    return end.saturating_duration_since(now);
                }
                let elapsed = now.saturating_duration_since(l.lit);
                let sliding =
                    l.from != l.to && now.saturating_duration_since(l.moved) < SLIDE + FRAME;
                if elapsed < t.fade_in + FRAME || sliding {
                    frame
                } else if elapsed < t.fade_in + t.hold {
                    (l.lit + t.fade_in + t.hold).saturating_duration_since(now)
                } else {
                    frame
                }
            })
            .min()
    }

    /// Whether `key` is still lit at `now`.
    pub fn is_lit(&self, key: &K, now: Instant) -> bool {
        self.lit
            .iter()
            .any(|l| &l.key == key && l.opacity(now).is_some())
    }

    pub fn is_empty(&self) -> bool {
        self.lit.is_empty()
    }
}

/// Whether a frame would look as the one drawn last (`drawn`: its opacity
/// and brightest spot), so it need not be drawn again: while held, or once
/// the spot came to rest. A solid line looks the same throughout.
pub fn unchanged(drawn: Option<(f64, f64)>, now: (f64, f64), solid: bool) -> bool {
    const STEP: f64 = 1.0 / 512.0;
    drawn.is_some_and(|(opacity, spot)| {
        solid || ((opacity - now.0).abs() < STEP && (spot - now.1).abs() < STEP)
    })
}

/// Whether the app can light the screen's edges where it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Shown,
    /// A Wayland session without the layer shell, as GNOME's.
    NoLayerShell,
    /// The overlays could not start, or lost the display server.
    Unavailable,
}

/// Whether the glow can show, as far as can be told before the overlays
/// are made: not on GNOME's Wayland, which has no layer shell (from the
/// agent's session: `wayland`, `desktop`); else as if it shows.
pub fn expected(wayland: bool, desktop: &str) -> Availability {
    if wayland && crate::trouble_text::is_gnome(desktop) {
        Availability::NoLayerShell
    } else {
        Availability::Shown
    }
}

/// The line under the glow's switch: what it does, or why it does not
/// show here (`desktop` is `XDG_CURRENT_DESKTOP`).
pub fn hint_key(availability: Availability, desktop: &str) -> &'static str {
    match availability {
        Availability::Shown => crate::text::keys::GENERAL_EDGE_GLOW_HINT,
        Availability::NoLayerShell if crate::trouble_text::is_gnome(desktop) => {
            crate::text::keys::GENERAL_EDGE_GLOW_NONE
        }
        _ => crate::text::keys::GENERAL_EDGE_GLOW_UNAVAILABLE,
    }
}

/// The 8-bit color of the thread from its components.
pub fn rgb8(color: [f64; 3]) -> [u8; 3] {
    color.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crossed(edge: ScreenEdge, along: f64) -> CursorCrossed {
        crossed_at(edge, along, 0.0, 0.0, 1920.0, 1080.0, "DP-1")
    }

    fn crossed_at(
        edge: ScreenEdge,
        along: f64,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        id: &str,
    ) -> CursorCrossed {
        CursorCrossed {
            display_id: id.into(),
            edge: edge as i32,
            along,
            display_x: x,
            display_y: y,
            display_width: w,
            display_height: h,
            ..Default::default()
        }
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    const MAIN: PixelRect = PixelRect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };

    #[test]
    fn places_the_crossing_along_the_display() {
        assert_eq!(fraction(&crossed(ScreenEdge::Right, 540.0)), 0.5);
        assert_eq!(fraction(&crossed(ScreenEdge::Top, 480.0)), 0.25);
        // On a display that does not start at the origin.
        assert_eq!(
            fraction(&crossed_at(
                ScreenEdge::Bottom,
                2190.0,
                1920.0,
                0.0,
                1080.0,
                1920.0,
                "x"
            )),
            0.25
        );
        // Outside it, at its nearest end.
        assert_eq!(fraction(&crossed(ScreenEdge::Left, -50.0)), 0.0);
        assert_eq!(fraction(&crossed(ScreenEdge::Left, 5000.0)), 1.0);
    }

    #[test]
    fn runs_the_whole_edge() {
        let rect = |x, y, width, height| PixelRect {
            x,
            y,
            width,
            height,
        };
        assert_eq!(
            strip(MAIN, 1.0, ScreenEdge::Left, DEPTH),
            rect(0, 0, 24, 1080)
        );
        assert_eq!(
            strip(MAIN, 1.0, ScreenEdge::Right, DEPTH),
            rect(1896, 0, 24, 1080)
        );
        assert_eq!(
            strip(MAIN, 1.0, ScreenEdge::Top, DEPTH),
            rect(0, 0, 1920, 24)
        );
        assert_eq!(
            strip(MAIN, 1.0, ScreenEdge::Bottom, SOLID_DEPTH),
            rect(0, 1076, 1920, 4)
        );
        // At 150%, in pixels, on a monitor not at the origin.
        let scaled = rect(1920, -200, 2160, 3840);
        assert_eq!(
            strip(scaled, 1.5, ScreenEdge::Left, DEPTH),
            rect(1920, -200, 36, 3840)
        );
        assert_eq!(
            strip(scaled, 1.5, ScreenEdge::Bottom, DEPTH),
            rect(1920, 3604, 2160, 36)
        );
        // Never deeper than the monitor.
        let tiny = rect(0, 0, 120, 20);
        assert_eq!(strip(tiny, 1.0, ScreenEdge::Top, DEPTH), tiny);
    }

    #[test]
    fn is_brightest_where_the_cursor_crossed_and_shows_all_along() {
        assert!(approx(brightness(0.3, 0.3), 1.0));
        // Crossed in the middle: about two fifths at both ends.
        assert!(approx(brightness(0.0, 0.5), brightness(1.0, 0.5)));
        assert!((0.35..=0.45).contains(&brightness(0.0, 0.5)));
        // Crossed at one end: the other end still shows.
        assert!((0.35..=0.36).contains(&brightness(1.0, 0.0)));
        // Dimmer the further from it.
        assert!(brightness(0.6, 0.2) > brightness(0.8, 0.2));
    }

    fn alpha(pixels: &[u8], width: usize, x: usize, y: usize) -> u8 {
        let i = (y * width + x) * 4;
        (u32::from_ne_bytes(pixels[i..i + 4].try_into().unwrap()) >> 24) as u8
    }

    fn red(pixels: &[u8], width: usize, x: usize, y: usize) -> u8 {
        let i = (y * width + x) * 4;
        (u32::from_ne_bytes(pixels[i..i + 4].try_into().unwrap()) >> 16) as u8
    }

    #[test]
    fn draws_the_glow_strongest_at_the_edge_and_where_it_crossed() {
        let thread = [0x8a, 0x46, 0xd3];
        // A right edge: 4 wide, 8 high; the screen's edge on the right.
        let right = pixels(ScreenEdge::Right, 8, 4, 0.5, thread, 1.0, false);
        assert_eq!(right.len(), 8 * 4 * 4);
        assert!(alpha(&right, 4, 3, 4) > alpha(&right, 4, 2, 4));
        assert!(alpha(&right, 4, 2, 4) > alpha(&right, 4, 0, 4));
        // Brighter where it crossed than at the ends, which still show.
        assert!(alpha(&right, 4, 3, 4) > alpha(&right, 4, 3, 0));
        assert!(alpha(&right, 4, 3, 0) > 0);
        // Premultiplied.
        let a = alpha(&right, 4, 3, 4);
        let expected = f64::from(0x8a) * f64::from(a) / 255.0;
        assert!((f64::from(red(&right, 4, 3, 4)) - expected).abs() <= 1.0);
        assert!(red(&right, 4, 3, 4) <= a);
        // A top edge: 8 wide, 4 high; the screen's edge on top.
        let top = pixels(ScreenEdge::Top, 8, 4, 0.0, thread, 1.0, false);
        assert!(alpha(&top, 8, 0, 0) > alpha(&top, 8, 0, 3));
        assert!(alpha(&top, 8, 0, 0) > alpha(&top, 8, 7, 0));
        // Painted again into the same pixels, as while the brightest part
        // slides: all of them, as new.
        let mut again = vec![0; right.len()];
        paint(&mut again, ScreenEdge::Right, 8, 4, 0.1, thread, 1.0, false);
        paint(&mut again, ScreenEdge::Right, 8, 4, 0.5, thread, 1.0, false);
        assert_eq!(right, again);
        // Left and bottom are mirrored.
        let left = pixels(ScreenEdge::Left, 8, 4, 0.5, thread, 1.0, false);
        assert_eq!(alpha(&right, 4, 3, 4), alpha(&left, 4, 0, 4));
        let bottom = pixels(ScreenEdge::Bottom, 8, 4, 0.0, thread, 1.0, false);
        assert_eq!(alpha(&top, 8, 0, 0), alpha(&bottom, 8, 0, 3));
        // Half as opaque as it fades.
        let half = pixels(ScreenEdge::Right, 8, 4, 0.5, thread, 0.5, false);
        assert!(alpha(&half, 4, 3, 4).abs_diff(alpha(&right, 4, 3, 4) / 2) <= 1);
    }

    #[test]
    fn high_contrast_is_a_solid_line() {
        let thread = [0x8e, 0x3f, 0xd8];
        let solid = pixels(ScreenEdge::Top, 8, 4, 0.0, thread, 1.0, true);
        for y in 0..4 {
            for x in 0..8 {
                assert_eq!(alpha(&solid, 8, x, y), 255);
                assert_eq!(red(&solid, 8, x, y), 0x8e);
            }
        }
    }

    #[test]
    fn the_brightest_part_slides_to_a_new_crossing() {
        assert!(approx(spot_at(0.2, 0.8, Duration::ZERO), 0.2));
        // Easing out: more than halfway at half the time.
        assert!(spot_at(0.2, 0.8, SLIDE / 2) > 0.5);
        assert_eq!(spot_at(0.2, 0.8, SLIDE), 0.8);
        assert_eq!(spot_at(0.2, 0.8, Duration::from_secs(1)), 0.8);
    }

    fn monitor(id: &str, x: f64, y: f64, width: f64, height: f64) -> Monitor {
        Monitor {
            id: id.into(),
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn finds_the_monitor_by_name_else_by_its_only_size() {
        let monitors = [
            monitor("DP-1", 0.0, 0.0, 1920.0, 1080.0),
            monitor("HDMI-1", 1920.0, 0.0, 1920.0, 1080.0),
            monitor("eDP-1", -1280.0, 0.0, 1280.0, 720.0),
        ];
        let named = crossed_at(ScreenEdge::Left, 10.0, 0.0, 0.0, 1920.0, 1080.0, "hdmi-1");
        assert_eq!(monitor_for(&named, &monitors), Some(1));
        // Not a name of these: the only one of its size.
        let sized = crossed_at(ScreenEdge::Left, 10.0, 0.0, 0.0, 1280.0, 720.0, "gone");
        assert_eq!(monitor_for(&sized, &monitors), Some(2));
    }

    #[test]
    fn an_output_without_a_name_is_found_by_where_it_is() {
        // The agent names such a Wayland output "output-<global>"; the
        // app's own connection knows it only by its place and size
        // (xdg-output), and maybe another name.
        let monitors = [
            monitor("", 0.0, 0.0, 1920.0, 1080.0),
            monitor("", 1920.0, 0.0, 1920.0, 1080.0),
            monitor("DP-2", 3840.0, 0.0, 1280.0, 1024.0),
        ];
        let crossed = crossed_at(
            ScreenEdge::Left,
            500.0,
            1920.0,
            0.0,
            1920.0,
            1080.0,
            "output-44",
        );
        assert_eq!(monitor_for(&crossed, &monitors), Some(1));
        let crossed = crossed_at(
            ScreenEdge::Top,
            4000.0,
            9000.0,
            0.0,
            1280.0,
            1024.0,
            "output-7",
        );
        assert_eq!(monitor_for(&crossed, &monitors), Some(2), "its only size");
        let crossed = crossed_at(
            ScreenEdge::Top,
            10.0,
            9000.0,
            0.0,
            1920.0,
            1080.0,
            "output-9",
        );
        assert_eq!(monitor_for(&crossed, &monitors), None, "two of that size");
    }

    #[test]
    fn tells_two_identical_monitors_apart_by_where_they_are() {
        let monitors = [
            monitor("DP-1", 0.0, 0.0, 1920.0, 1080.0),
            monitor("DP-2", 1920.0, 0.0, 1920.0, 1080.0),
        ];
        let at = |x: f64, id: &str| crossed_at(ScreenEdge::Left, 540.0, x, 0.0, 1920.0, 1080.0, id);
        assert_eq!(monitor_for(&at(1920.0, "gone"), &monitors), Some(1));
        assert_eq!(monitor_for(&at(0.0, "gone"), &monitors), Some(0));
        // The name comes first, wherever the display was said to be.
        assert_eq!(monitor_for(&at(1920.0, "DP-1"), &monitors), Some(0));
        // Neither by name nor by place, and two of its size: none.
        assert_eq!(monitor_for(&at(700.0, "gone"), &monitors), None);
    }

    #[test]
    fn lights_only_the_four_edges() {
        for edge in [
            ScreenEdge::Left,
            ScreenEdge::Right,
            ScreenEdge::Top,
            ScreenEdge::Bottom,
        ] {
            assert_eq!(known(&crossed(edge, 0.0)), Some(edge));
        }
        assert_eq!(known(&crossed(ScreenEdge::Unspecified, 0.0)), None);
        // One a newer background service might send.
        let newer = CursorCrossed {
            edge: 7,
            ..Default::default()
        };
        assert_eq!(known(&newer), None);
    }

    #[test]
    fn crossing_the_same_edge_while_it_shows_carries_on_without_going_dark() {
        let timing = ANIMATED;
        let start = Instant::now();
        let ms = Duration::from_millis;
        let relit = |at: u64| lit_at(Some(start), start + ms(at), timing);
        let shown = |lit: Instant, at: u64| opacity(timing, (start + ms(at)) - lit);
        // Held: full at once, and held anew from now.
        let held = relit(300);
        assert_eq!(shown(held, 300), Some(1.0));
        assert_eq!(shown(held, 600), Some(1.0));
        // Fading, halfway out: from half as bright, up again.
        let fading = relit(600);
        assert!(approx(shown(fading, 600).unwrap(), 0.5));
        assert!(approx(shown(fading, 650).unwrap(), 1.0));
        // Coming up: on from there, not from dark.
        let rising = relit(50);
        assert!(approx(shown(rising, 50).unwrap(), 0.5));
        // Gone, or never lit: anew.
        assert_eq!(relit(900), start + ms(900));
        assert_eq!(lit_at(None, start, timing), start);
        // Animations off: a crossing while it shows keeps it there, for
        // half a second again.
        assert_eq!(lit_at(Some(start), start + ms(400), STILL), start + ms(400));
    }

    #[test]
    fn each_edge_glows_on_its_own_clock_and_carries_on_when_crossed_again() {
        let start = Instant::now();
        let ms = |n: u64| start + Duration::from_millis(n);
        let mut glows = Glows::default();
        glows.light(("DP-1", ScreenEdge::Right), 0.2, start, ANIMATED);
        let first = glows.frame(ms(50));
        assert_eq!(first.len(), 1);
        assert!(approx(first[0].opacity, 0.5));
        assert!(approx(first[0].spot, 0.2));
        // Back in across the left edge of the other monitor: both lit.
        glows.light(("HDMI-1", ScreenEdge::Left), 0.7, ms(60), ANIMATED);
        assert_eq!(glows.frame(ms(100)).len(), 2);
        // The right edge fading at 600 ms is crossed again further down: it
        // carries on from its opacity, and its spot slides from where it is.
        let fading = glows.frame(ms(600));
        let right = fading
            .iter()
            .find(|f| f.key.0 == "DP-1")
            .expect("still lit");
        let before = right.opacity;
        assert!(before > 0.0 && before < 1.0);
        glows.light(("DP-1", ScreenEdge::Right), 0.8, ms(600), ANIMATED);
        let now = glows.frame(ms(600));
        let right = now.iter().find(|f| f.key.0 == "DP-1").unwrap();
        assert!(approx(right.opacity, before), "no step down or up");
        assert!(approx(right.spot, 0.2));
        // Crossed a third time before the slide ended: from where it was.
        glows.light(("DP-1", ScreenEdge::Right), 0.4, ms(675), ANIMATED);
        let mid = spot_at(0.2, 0.8, Duration::from_millis(75));
        let right = glows
            .frame(ms(675))
            .into_iter()
            .find(|f| f.key.0 == "DP-1")
            .unwrap();
        assert!(approx(right.spot, mid), "{} vs {mid}", right.spot);
        let slid = glows
            .frame(ms(675) + SLIDE)
            .into_iter()
            .find(|f| f.key.0 == "DP-1")
            .unwrap();
        assert!(approx(slid.spot, 0.4));
        // The left edge went on its own clock.
        assert!(!glows.is_lit(&("HDMI-1", ScreenEdge::Left), ms(60 + 800)));
        assert!(glows.is_lit(&("DP-1", ScreenEdge::Right), ms(60 + 800)));
        assert!(glows.frame(ms(5000)).is_empty());
        assert!(glows.is_empty());
    }

    #[test]
    fn without_animations_the_glow_jumps_to_a_new_crossing() {
        let start = Instant::now();
        let mut glows = Glows::default();
        glows.light("top", 0.1, start, STILL);
        let shown = glows.frame(start);
        assert!(approx(shown[0].opacity, 1.0));
        glows.light("top", 0.9, start + Duration::from_millis(200), STILL);
        let shown = glows.frame(start + Duration::from_millis(200));
        assert!(approx(shown[0].spot, 0.9));
        assert!(approx(shown[0].opacity, 1.0));
        // Held anew from the second crossing.
        assert!(glows.is_lit(&"top", start + Duration::from_millis(650)));
        assert!(!glows.is_lit(&"top", start + Duration::from_millis(701)));
    }

    #[test]
    fn a_glow_gone_lights_anew_where_crossed() {
        let start = Instant::now();
        let mut glows = Glows::default();
        glows.light(1, 0.1, start, ANIMATED);
        let later = start + Duration::from_secs(2);
        glows.light(1, 0.9, later, ANIMATED);
        let shown = glows.frame(later + Duration::from_millis(50));
        assert_eq!(shown.len(), 1);
        assert!(approx(shown[0].spot, 0.9));
        assert!(approx(shown[0].opacity, 0.5));
    }

    #[test]
    fn sleeps_while_nothing_changes_on_screen() {
        let start = Instant::now();
        let ms = |n: u64| start + Duration::from_millis(n);
        let mut glows = Glows::default();
        assert_eq!(glows.wake(start, None, |_| true), None);
        glows.light("right", 0.5, start, ANIMATED);
        // Fading in, and one frame past it.
        assert_eq!(glows.wake(ms(50), None, |_| true), Some(FRAME));
        assert_eq!(glows.wake(ms(110), None, |_| true), Some(FRAME));
        // Held: until the hold ends (at 400 ms).
        assert_eq!(
            glows.wake(ms(150), None, |_| true),
            Some(Duration::from_millis(250))
        );
        // Fading out.
        assert_eq!(glows.wake(ms(500), None, |_| true), Some(FRAME));
        // A solid line: until it is gone (800 ms).
        assert_eq!(
            glows.wake(ms(50), None, |_| false),
            Some(Duration::from_millis(750))
        );
        // A frame drawn 10 ms ago: the next is due in 6.
        assert_eq!(
            glows.wake(ms(50), Some(ms(40)), |_| true),
            Some(Duration::from_millis(6))
        );
        assert_eq!(
            glows.wake(ms(60), Some(ms(40)), |_| true),
            Some(Duration::ZERO)
        );
        // Sliding to a new crossing while held: frames again.
        glows.light("right", 0.9, ms(200), ANIMATED);
        assert_eq!(glows.wake(ms(250), None, |_| true), Some(FRAME));
        // Gone but still there: at once, for the frame that takes it down
        // (else the overlay would sleep with its window still up).
        assert_eq!(glows.wake(ms(1100), None, |_| true), Some(Duration::ZERO));
        glows.frame(ms(1100));
        assert_eq!(glows.wake(ms(1100), None, |_| true), None);
    }

    #[test]
    fn a_glows_size_is_along_and_deep_and_back() {
        assert_eq!(along_deep(ScreenEdge::Left, 24, 1080), (1080, 24));
        assert_eq!(along_deep(ScreenEdge::Top, 1920, 24), (1920, 24));
        assert_eq!(along_deep(ScreenEdge::Right, 1080, 24), (24, 1080));
    }

    #[test]
    fn frames_that_look_the_same_are_not_drawn_again() {
        assert!(!unchanged(None, (1.0, 0.5), false), "the first is drawn");
        assert!(unchanged(Some((1.0, 0.5)), (1.0, 0.5), false), "held");
        assert!(!unchanged(Some((0.9, 0.5)), (1.0, 0.5), false), "fading");
        assert!(!unchanged(Some((1.0, 0.4)), (1.0, 0.5), false), "sliding");
        assert!(
            unchanged(Some((1.0, 0.5)), (0.3, 0.9), true),
            "a solid line"
        );
        assert!(!unchanged(None, (1.0, 0.5), true));
    }

    #[test]
    fn gnomes_wayland_is_known_before_the_overlays_are_made() {
        assert_eq!(expected(true, "ubuntu:GNOME"), Availability::NoLayerShell);
        assert_eq!(expected(true, "KDE"), Availability::Shown);
        assert_eq!(expected(false, "ubuntu:GNOME"), Availability::Shown);
    }

    #[test]
    fn says_why_the_glow_does_not_show() {
        use crate::text::keys;
        assert_eq!(
            hint_key(Availability::Shown, "ubuntu:GNOME"),
            keys::GENERAL_EDGE_GLOW_HINT
        );
        assert_eq!(
            hint_key(Availability::NoLayerShell, "ubuntu:GNOME"),
            keys::GENERAL_EDGE_GLOW_NONE
        );
        // Not GNOME's sentence on another desktop, nor for an overlay lost.
        assert_eq!(
            hint_key(Availability::NoLayerShell, "Weston"),
            keys::GENERAL_EDGE_GLOW_UNAVAILABLE
        );
        for desktop in ["ubuntu:GNOME", "KDE", "XFCE", "sway"] {
            assert_eq!(
                hint_key(Availability::Unavailable, desktop),
                keys::GENERAL_EDGE_GLOW_UNAVAILABLE
            );
        }
    }

    #[test]
    fn fades_in_holds_and_fades_out_unless_animations_are_off() {
        let ms = Duration::from_millis;
        let t = timing(false);
        assert_eq!(t, ANIMATED);
        assert_eq!(opacity(t, Duration::ZERO), Some(0.0));
        assert!(approx(opacity(t, ms(50)).unwrap(), 0.5));
        assert_eq!(opacity(t, ms(350)), Some(1.0));
        assert!(approx(opacity(t, ms(600)).unwrap(), 0.5));
        assert_eq!(opacity(t, ms(800)), None);
        // Animations off: there at once for half a second.
        let still = timing(true);
        assert_eq!(opacity(still, Duration::ZERO), Some(1.0));
        assert_eq!(opacity(still, ms(499)), Some(1.0));
        assert_eq!(opacity(still, ms(500)), None);
    }
}
