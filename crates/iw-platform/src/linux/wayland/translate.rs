//! The parts of the Wayland backends that need no compositor: the displays
//! the outputs describe, libei's buttons, wheel steps and regions, and what
//! the RemoteDesktop portal's session means for the permission it stands
//! for.

use iw_core::geometry::{Point, Rect};
use iw_core::input::Button;
use iw_core::layout::Display;

use crate::{Backend, BackendRole, Permission, PermissionAsk, PermissionState};

/// An output as `wl_output` and `xdg_output` describe it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Output {
    /// Its global's name in the registry, for telling outputs apart.
    pub global: u32,
    /// `wl_output.name` (version 4) or `xdg_output.name`, such as `DP-1`.
    pub name: Option<String>,
    /// Where `xdg_output` puts it in the compositor's logical space.
    pub logical_position: Option<(i32, i32)>,
    pub logical_size: Option<(i32, i32)>,
    /// `wl_output.geometry`'s position, for compositors without
    /// `xdg_output`.
    pub position: (i32, i32),
    /// The current mode, in pixels, before the output's transform.
    pub mode: Option<(i32, i32)>,
    /// `wl_output.scale`, the integer scale clients draw at.
    pub scale: i32,
    /// `wl_output.transform` turns it a quarter or three quarters.
    pub rotated: bool,
}

impl Output {
    /// The mode as the output shows it, after its transform.
    fn shown_mode(&self) -> Option<(i32, i32)> {
        self.mode
            .map(|(w, h)| if self.rotated { (h, w) } else { (w, h) })
    }

    /// Its logical bounds: `xdg_output`'s, or else the mode divided by the
    /// integer scale at `wl_output`'s position.
    fn bounds(&self) -> Option<Rect> {
        let (x, y) = self.logical_position.unwrap_or(self.position);
        let (w, h) = match self.logical_size {
            Some(size) => size,
            None => {
                let (w, h) = self.shown_mode()?;
                let scale = self.scale.max(1);
                (w / scale, h / scale)
            }
        };
        (w > 0 && h > 0).then(|| Rect::new(f64::from(x), f64::from(y), f64::from(w), f64::from(h)))
    }

    /// Pixels to a logical unit: the mode over the logical size, which is
    /// fractional with fractional scaling, or else the integer scale.
    fn pixels_per_unit(&self, bounds: Rect) -> f64 {
        match (self.shown_mode(), self.logical_size) {
            (Some((w, _)), Some(_)) if w > 0 && bounds.width > 0.0 => f64::from(w) / bounds.width,
            _ => f64::from(self.scale.max(1)),
        }
    }
}

/// The displays the outputs make, in the compositor's logical space, which
/// is the one its pointer moves in. Wayland has no primary output: the one
/// at the origin is called so, or else the first.
pub(crate) fn displays_of(outputs: &[Output]) -> Vec<Display> {
    let mut displays: Vec<Display> = outputs
        .iter()
        .filter_map(|o| {
            let bounds = o.bounds()?;
            Some(Display {
                id: o
                    .name
                    .clone()
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| format!("output-{}", o.global)),
                bounds,
                scale: o.pixels_per_unit(bounds),
                primary: false,
            })
        })
        .collect();
    let primary = displays
        .iter()
        .position(|d| d.bounds.contains(Point::new(0.0, 0.0)))
        .unwrap_or(0);
    if let Some(d) = displays.get_mut(primary) {
        d.primary = true;
    }
    displays
}

// libei's buttons are evdev's (`linux/input-event-codes.h`).
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;
const BTN_FORWARD: u32 = 0x115;
const BTN_BACK: u32 = 0x116;

/// The evdev button libei takes for `button`.
pub(crate) fn ei_button(button: Button) -> u32 {
    match button {
        Button::Left => BTN_LEFT,
        Button::Right => BTN_RIGHT,
        Button::Middle => BTN_MIDDLE,
        Button::Back => BTN_SIDE,
        Button::Forward => BTN_EXTRA,
    }
}

/// The button an evdev button libei gives is: a mouse's, its side buttons
/// as back and forward, whichever code a device gives them.
pub(crate) fn button_of_ei(code: u32) -> Option<Button> {
    match code {
        BTN_LEFT => Some(Button::Left),
        BTN_RIGHT => Some(Button::Right),
        BTN_MIDDLE => Some(Button::Middle),
        BTN_SIDE | BTN_BACK => Some(Button::Back),
        BTN_EXTRA | BTN_FORWARD => Some(Button::Forward),
        _ => None,
    }
}

/// libei counts a wheel's notch as 120.
pub(crate) const STEP: f64 = 120.0;

/// libinput's: a wheel's notch scrolls 15 logical pixels.
pub(crate) const PIXELS_PER_NOTCH: f64 = 15.0;

/// A scroll as libei takes it, positive `y` down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Scroll {
    /// A wheel's notches, 120 each.
    Steps(i32, i32),
    /// Smooth scrolling, in logical pixels.
    Pixels(f32, f32),
}

/// What a scroll of notches, in the direction of `InputEvent::Scroll`
/// (positive `dy` away from the user, positive `dx` to the right), is sent
/// to libei as: whole notches, as a wheel turns, as discrete steps;
/// fractions, as from a trackpad or a smooth wheel, as smooth scrolling.
/// Nothing for none.
pub(crate) fn scroll(dx: f64, dy: f64) -> Option<Scroll> {
    if dx == 0.0 && dy == 0.0 {
        return None;
    }
    if dx.fract() == 0.0 && dy.fract() == 0.0 {
        // No wheel turns past i32 in one go.
        return Some(Scroll::Steps((dx * STEP) as i32, (-dy * STEP) as i32));
    }
    Some(Scroll::Pixels(
        (dx * PIXELS_PER_NOTCH) as f32,
        (-dy * PIXELS_PER_NOTCH) as f32,
    ))
}

/// Where in the regions of an absolute device `point` lands: the point
/// itself if a region has it, or else the nearest point of the nearest
/// region, since libei drops motion outside them. `None` without regions.
pub(crate) fn within_regions(point: Point, regions: &[Rect]) -> Option<Point> {
    let clamp = |r: &Rect| {
        // A region's far edges are not in it.
        let right = (r.x + r.width - 1.0).max(r.x);
        let bottom = (r.y + r.height - 1.0).max(r.y);
        Point::new(point.x.clamp(r.x, right), point.y.clamp(r.y, bottom))
    };
    regions
        .iter()
        .map(|r| {
            let p = clamp(r);
            (p, (p.x - point.x).powi(2) + (p.y - point.y).powi(2))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(p, _)| p)
}

/// Keys and buttons injection holds down. Wayland apps repeat a held key
/// themselves, so a press of a key already down, which a controlling device
/// sends as its own repeat, is not sent again: the key would repeat twice as
/// fast. Releases always go.
#[derive(Debug, Default)]
pub(crate) struct Held {
    down: Vec<u32>,
}

impl Held {
    /// Whether to send the press or release of `code`.
    pub(crate) fn sends(&self, code: u32, pressed: bool) -> bool {
        !pressed || !self.down.contains(&code)
    }

    /// The press or release of `code` was sent.
    pub(crate) fn sent(&mut self, code: u32, pressed: bool) {
        self.down.retain(|c| *c != code);
        if pressed {
            self.down.push(code);
        }
    }

    /// What is down, now let go of.
    pub(crate) fn take(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.down)
    }
}

/// Where a portal session stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Portal {
    /// Waiting for the app to ask: never asked since the agent started,
    /// with nothing the desktop remembers.
    #[default]
    NotAsked,
    /// Starting with what the user allowed before, as the desktop
    /// remembers it: no dialog is expected.
    Restoring,
    /// Starting because the app asked: the desktop may be asking the user.
    Starting,
    /// Running: the permission is granted.
    Running,
    /// The user said no.
    Denied,
    /// The user stopped sharing. The app may ask again.
    Stopped,
    /// The desktop has no such portal, or it failed; why.
    Failed(String),
}

impl Portal {
    /// The permission the session stands for, asked for `asked` times.
    pub(crate) fn permission(&self, permission: Permission, asked: u64) -> PermissionState {
        let ask = match self {
            Portal::NotAsked | Portal::Running => PermissionAsk::Askable,
            Portal::Restoring => PermissionAsk::Restoring,
            Portal::Starting => PermissionAsk::Asking,
            Portal::Denied => PermissionAsk::Denied,
            Portal::Stopped => PermissionAsk::Stopped,
            Portal::Failed(_) => PermissionAsk::Failed,
        };
        PermissionState {
            permission,
            granted: *self == Portal::Running,
            ask,
            asked,
            failure: match self {
                Portal::Failed(why) => Some(why.clone()),
                _ => None,
            },
        }
    }

    /// Whether asking now starts a session: not while one is starting or
    /// running already.
    pub(crate) fn may_start(&self) -> bool {
        !matches!(self, Portal::Restoring | Portal::Starting | Portal::Running)
    }
}

/// A backend served through a portal session, from one reading of its
/// permission (`granted` is set exactly while the session runs, under the
/// same lock): served while it runs; until then held back by what it waits
/// for, never by the permission being granted. `ran_before` says a session
/// of it ran since the agent started, so a start with what the user
/// allowed is a restart.
pub(crate) fn portal_backend(
    role: BackendRole,
    implementation: &str,
    permission: &PermissionState,
    ran_before: bool,
) -> Backend {
    if permission.granted {
        return Backend::serving(role, implementation);
    }
    let waiting = match permission.ask {
        PermissionAsk::Restoring if ran_before => {
            "restarting with what the user allowed before".to_owned()
        }
        PermissionAsk::Restoring => "starting with what the user allowed before".to_owned(),
        PermissionAsk::Asking => format!("starting: {} being asked", permission.permission.name()),
        _ => permission.describe(),
    };
    Backend::held_back(role, implementation, &waiting)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(global: u32, name: &str) -> Output {
        Output {
            global,
            name: Some(name.to_owned()),
            scale: 1,
            ..Default::default()
        }
    }

    #[test]
    fn displays_are_the_logical_outputs() {
        let mut laptop = output(3, "eDP-1");
        laptop.logical_position = Some((0, 0));
        laptop.logical_size = Some((1536, 960));
        laptop.mode = Some((2880, 1800));
        laptop.scale = 2;
        let mut side = output(4, "DP-1");
        side.logical_position = Some((1536, -200));
        side.logical_size = Some((1080, 1920));
        side.mode = Some((1920, 1080));
        side.rotated = true;
        let displays = displays_of(&[side, laptop]);
        assert_eq!(displays.len(), 2);
        let side = &displays[0];
        assert_eq!(side.id, "DP-1");
        assert_eq!(side.bounds, Rect::new(1536.0, -200.0, 1080.0, 1920.0));
        // Turned a quarter: its 1920 pixels run down, over 1920 units.
        assert_eq!(side.scale, 1.0);
        assert!(!side.primary);
        let laptop = &displays[1];
        assert_eq!(laptop.scale, 1.875, "fractional scaling");
        assert!(laptop.primary, "the one at the origin");
    }

    #[test]
    fn outputs_without_xdg_output_use_the_integer_scale() {
        let mut o = output(7, "HDMI-A-1");
        o.position = (100, 0);
        o.mode = Some((3840, 2160));
        o.scale = 2;
        let displays = displays_of(&[o]);
        assert_eq!(displays[0].bounds, Rect::new(100.0, 0.0, 1920.0, 1080.0));
        assert_eq!(displays[0].scale, 2.0);
        assert!(displays[0].primary, "the first, with none at the origin");
    }

    #[test]
    fn unnamed_and_empty_outputs() {
        let mut unnamed = output(9, "");
        unnamed.logical_size = Some((800, 600));
        let empty = output(10, "X");
        let displays = displays_of(&[unnamed, empty]);
        assert_eq!(displays.len(), 1, "no mode, no size: nothing to show");
        assert_eq!(displays[0].id, "output-9");
    }

    #[test]
    fn buttons_are_evdevs() {
        assert_eq!(ei_button(Button::Left), 0x110);
        assert_eq!(ei_button(Button::Right), 0x111);
        assert_eq!(ei_button(Button::Middle), 0x112);
        assert_eq!(ei_button(Button::Back), 0x113);
        assert_eq!(ei_button(Button::Forward), 0x114);
    }

    #[test]
    fn evdev_buttons_are_read_back() {
        for button in [
            Button::Left,
            Button::Right,
            Button::Middle,
            Button::Back,
            Button::Forward,
        ] {
            assert_eq!(button_of_ei(ei_button(button)), Some(button));
        }
        // BTN_FORWARD and BTN_BACK, as some mice give them.
        assert_eq!(button_of_ei(0x115), Some(Button::Forward));
        assert_eq!(button_of_ei(0x116), Some(Button::Back));
        assert_eq!(button_of_ei(0x14a), None, "BTN_TOUCH");
    }

    #[test]
    fn wheel_notches_are_120_steps_with_y_down() {
        // One notch away from the user scrolls up: negative y.
        assert_eq!(scroll(0.0, 1.0), Some(Scroll::Steps(0, -120)));
        assert_eq!(scroll(1.0, 0.0), Some(Scroll::Steps(120, 0)));
        assert_eq!(scroll(0.0, -2.0), Some(Scroll::Steps(0, 240)));
        assert_eq!(scroll(0.0, 0.0), None);
    }

    #[test]
    fn fractions_scroll_smoothly() {
        assert_eq!(scroll(0.0, 0.25), Some(Scroll::Pixels(0.0, -3.75)));
        assert_eq!(scroll(-0.5, 2.0), Some(Scroll::Pixels(-7.5, -30.0)));
    }

    #[test]
    fn points_are_kept_within_the_regions() {
        let regions = [
            Rect::new(0.0, 0.0, 1920.0, 1080.0),
            Rect::new(1920.0, 0.0, 1280.0, 1024.0),
        ];
        let inside = Point::new(2000.0, 500.0);
        assert_eq!(within_regions(inside, &regions), Some(inside));
        assert_eq!(
            within_regions(Point::new(2500.0, 1060.0), &regions),
            Some(Point::new(2500.0, 1023.0)),
            "below the shorter display: onto its bottom row"
        );
        assert_eq!(
            within_regions(Point::new(-5.0, -5.0), &regions),
            Some(Point::new(0.0, 0.0))
        );
        assert_eq!(
            within_regions(Point::new(5000.0, 10.0), &regions),
            Some(Point::new(3199.0, 10.0))
        );
        assert_eq!(within_regions(inside, &[]), None);
    }

    #[test]
    fn held_keys_are_pressed_once() {
        let mut held = Held::default();
        assert!(held.sends(30, true));
        held.sent(30, true);
        assert!(!held.sends(30, true), "a repeat");
        assert!(held.sends(30, false));
        held.sent(30, false);
        assert!(held.sends(30, true));
        held.sent(30, true);
        held.sent(0x110, true);
        assert_eq!(held.take(), vec![30, 0x110]);
        assert!(held.sends(30, true));
    }

    #[test]
    fn the_portal_session_makes_the_permission() {
        let state = |p: Portal| p.permission(Permission::RemoteControl, 3);
        let granted = state(Portal::Running);
        assert!(granted.granted);
        assert_eq!(granted.ask, PermissionAsk::Askable);
        assert_eq!(granted.asked, 3);
        for (portal, ask) in [
            (Portal::NotAsked, PermissionAsk::Askable),
            (Portal::Restoring, PermissionAsk::Restoring),
            (Portal::Starting, PermissionAsk::Asking),
            (Portal::Denied, PermissionAsk::Denied),
            (Portal::Stopped, PermissionAsk::Stopped),
        ] {
            let s = state(portal);
            assert!(!s.granted);
            assert_eq!(s.ask, ask);
            assert_eq!(s.failure, None);
        }
        let failed = state(Portal::Failed("no portal".into()));
        assert_eq!(failed.ask, PermissionAsk::Failed);
        assert_eq!(failed.failure.as_deref(), Some("no portal"));
        assert!(!Portal::Restoring.may_start());
        assert!(!Portal::Starting.may_start());
        assert!(!Portal::Running.may_start());
        assert!(Portal::Denied.may_start());
        assert!(Portal::Stopped.may_start());
        assert!(Portal::Failed(String::new()).may_start());
    }

    #[test]
    fn a_portal_backend_is_held_back_by_what_it_waits_for() {
        let backend = |p: Portal, ran: bool| {
            portal_backend(
                BackendRole::Input,
                "RemoteDesktop portal",
                &p.permission(Permission::RemoteControl, 1),
                ran,
            )
        };
        let problem = |p: Portal, ran: bool| backend(p, ran).problem;
        let serving = backend(Portal::Running, true);
        assert_eq!(
            (serving.implementation.as_str(), serving.problem.as_str()),
            ("RemoteDesktop portal", "")
        );
        assert_eq!(
            problem(Portal::Restoring, false),
            "starting with what the user allowed before"
        );
        assert_eq!(
            problem(Portal::Restoring, true),
            "restarting with what the user allowed before"
        );
        assert_eq!(
            problem(Portal::Starting, false),
            "starting: remote control being asked"
        );
        assert_eq!(
            problem(Portal::Denied, true),
            "remote control missing (the user said no)"
        );
        assert_eq!(
            problem(Portal::Failed("the session went away".into()), true),
            "remote control missing (asking failed: the session went away)"
        );
        assert!(problem(Portal::NotAsked, false).contains("not asked yet"));
        // Never the permission being granted given as what is wrong.
        for portal in [
            Portal::NotAsked,
            Portal::Restoring,
            Portal::Starting,
            Portal::Denied,
            Portal::Stopped,
            Portal::Failed(String::new()),
        ] {
            for ran in [false, true] {
                let p = problem(portal.clone(), ran);
                assert!(!p.is_empty() && !p.contains("granted"), "{portal:?}: {p}");
            }
        }
    }
}
