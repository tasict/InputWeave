//! The receiver-side state machine.
//!
//! A machine being controlled from elsewhere runs a `Receiver`. It accepts
//! input from at most one controller at a time, turns it into injections for
//! the local session, and guarantees that every key and button it pressed is
//! released when the controller leaves, disappears or the local user takes
//! over.
//!
//! In relative mode (`RemoteEvent::Relative`) the controller sends how far
//! the pointer moved in all since the mode began (`RelativeMotion`), so a
//! lost datagram costs no movement: the next one carries it. Positions
//! sent meanwhile are ignored, and buttons go down wherever the cursor is.

use crate::HidUsage;
use crate::geometry::Point;
use crate::input::Button;
use crate::layout::DeviceId;
use crate::safety::{PressedTracker, Released};

/// Input from a remote controller. Positions are in this machine's desktop
/// coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum RemoteEvent {
    Enter {
        from: DeviceId,
        position: Point,
        modifiers: Vec<HidUsage>,
        /// Buttons held as files are dragged in, pressed after moving to
        /// `position`.
        buttons: Vec<Button>,
    },
    Leave {
        from: DeviceId,
    },
    /// Unreliable datagram; only the newest `seq` is applied.
    Motion {
        from: DeviceId,
        seq: u64,
        position: Point,
    },
    /// Relative mode `session` began or ended. Its movements are datagrams
    /// of that session, so one from another never counts. Ending, `total`
    /// is how far the pointer moved in all in it, so movements whose
    /// datagrams were lost still count.
    Relative {
        from: DeviceId,
        on: bool,
        session: u64,
        total: Point,
    },
    /// Unreliable datagram in relative mode `session`: the pointer moved
    /// by `total` in all since it began. Only the newest `seq` is applied.
    RelativeMotion {
        from: DeviceId,
        session: u64,
        seq: u64,
        total: Point,
    },
    /// Unreliable datagram; older `seq` values are dropped.
    Scroll {
        from: DeviceId,
        seq: u64,
        dx: f64,
        dy: f64,
    },
    /// Without a `position`, where the pointer is.
    Button {
        from: DeviceId,
        button: Button,
        pressed: bool,
        position: Option<Point>,
    },
    Key {
        from: DeviceId,
        usage: HidUsage,
        pressed: bool,
    },
    /// The connection to this device was lost.
    ControllerLost(DeviceId),
}

/// Something to inject into the local session.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Injection {
    MoveTo(Point),
    /// Move the pointer by so much, as a mouse would: logical units.
    MoveBy(Point),
    Button {
        button: Button,
        pressed: bool,
    },
    Key {
        usage: HidUsage,
        pressed: bool,
    },
    Scroll {
        dx: f64,
        dy: f64,
    },
}

/// The most one relative movement moves the pointer on either axis, in
/// logical units: a burst after lost datagrams goes, a corrupt total does
/// not fling the cursor.
pub const MAX_RELATIVE_STEP: f64 = 4096.0;

#[derive(Debug, Default)]
pub struct Receiver {
    controller: Option<DeviceId>,
    pressed: PressedTracker,
    motion_seq: Option<u64>,
    scroll_seq: Option<u64>,
    /// Set in relative mode.
    relative: Option<Relative>,
}

/// Where relative mode stands: its session, the newest movement applied,
/// and the total it carried.
#[derive(Debug, Default)]
struct Relative {
    session: u64,
    seq: Option<u64>,
    total: Point,
}

impl Relative {
    /// The movement from what was applied to `total`, which from now on
    /// counts as applied.
    fn up_to(&mut self, total: Point) -> Vec<Injection> {
        if !total.x.is_finite() || !total.y.is_finite() {
            return Vec::new();
        }
        let step = |to: f64, from: f64| (to - from).clamp(-MAX_RELATIVE_STEP, MAX_RELATIVE_STEP);
        let delta = Point::new(step(total.x, self.total.x), step(total.y, self.total.y));
        // Clamped or not, later totals count from this one.
        self.total = total;
        if delta == Point::default() {
            Vec::new()
        } else {
            vec![Injection::MoveBy(delta)]
        }
    }
}

impl Receiver {
    /// The device currently controlling this machine, if any.
    pub fn controller(&self) -> Option<&DeviceId> {
        self.controller.as_ref()
    }

    /// Whether a mouse button is held here for the controller.
    pub fn buttons_held(&self) -> bool {
        self.pressed.buttons_held()
    }

    /// Whether the controller has this device in relative mode.
    pub fn relative(&self) -> bool {
        self.relative.is_some()
    }

    pub fn handle(&mut self, event: RemoteEvent) -> Vec<Injection> {
        match event {
            RemoteEvent::Enter {
                from,
                position,
                modifiers,
                buttons,
            } => {
                let mut out = releases(self.pressed.release_all());
                self.controller = Some(from);
                self.motion_seq = None;
                self.scroll_seq = None;
                self.relative = None;
                out.push(Injection::MoveTo(position));
                for usage in modifiers {
                    if self.pressed.key(usage, true) {
                        out.push(Injection::Key {
                            usage,
                            pressed: true,
                        });
                    }
                }
                // Tracked like any other press, so leaving releases them.
                for button in buttons {
                    if self.pressed.button(button, true) {
                        out.push(Injection::Button {
                            button,
                            pressed: true,
                        });
                    }
                }
                out
            }
            RemoteEvent::Leave { from } | RemoteEvent::ControllerLost(from) => {
                if self.is_controller(&from) {
                    self.release_control()
                } else {
                    Vec::new()
                }
            }
            RemoteEvent::Motion {
                from,
                seq,
                position,
            } => {
                if self.is_controller(&from)
                    && self.relative.is_none()
                    && newer(&mut self.motion_seq, seq)
                {
                    vec![Injection::MoveTo(position)]
                } else {
                    Vec::new()
                }
            }
            RemoteEvent::Relative {
                from,
                on,
                session,
                total,
            } => {
                if !self.is_controller(&from) {
                    return Vec::new();
                }
                // What it had not applied of the session ending goes first.
                let rest = match self.relative.take() {
                    Some(mut relative) if !on && relative.session == session => {
                        relative.up_to(total)
                    }
                    _ => Vec::new(),
                };
                self.relative = on.then(|| Relative {
                    session,
                    ..Relative::default()
                });
                rest
            }
            RemoteEvent::RelativeMotion {
                from,
                session,
                seq,
                total,
            } => {
                if !self.is_controller(&from) {
                    return Vec::new();
                }
                // Only the session it is in: not one that ended, nor one
                // whose beginning has yet to come.
                match self.relative.as_mut() {
                    Some(relative) if relative.session == session => {
                        if newer(&mut relative.seq, seq) {
                            relative.up_to(total)
                        } else {
                            Vec::new()
                        }
                    }
                    _ => Vec::new(),
                }
            }
            RemoteEvent::Scroll { from, seq, dx, dy } => {
                if self.is_controller(&from) && newer(&mut self.scroll_seq, seq) {
                    vec![Injection::Scroll { dx, dy }]
                } else {
                    Vec::new()
                }
            }
            RemoteEvent::Button {
                from,
                button,
                pressed,
                position,
            } => {
                if !self.is_controller(&from) {
                    return Vec::new();
                }
                // In relative mode the position says nothing: the button
                // goes down wherever the pointer is, as without one.
                let mut out = match position {
                    Some(position) if self.relative.is_none() => vec![Injection::MoveTo(position)],
                    _ => Vec::new(),
                };
                if self.pressed.button(button, pressed) {
                    out.push(Injection::Button { button, pressed });
                }
                out
            }
            RemoteEvent::Key {
                from,
                usage,
                pressed,
            } => {
                if self.is_controller(&from) && self.pressed.key(usage, pressed) {
                    vec![Injection::Key { usage, pressed }]
                } else {
                    Vec::new()
                }
            }
        }
    }

    /// The local user touched this machine's own keyboard or mouse. They take
    /// over immediately: everything held for the controller is released, and
    /// the returned controller should be told so it stops sending input.
    pub fn local_activity(&mut self) -> Option<(DeviceId, Vec<Injection>)> {
        let controller = self.controller.clone()?;
        Some((controller, self.release_control()))
    }

    fn is_controller(&self, device: &DeviceId) -> bool {
        self.controller.as_ref() == Some(device)
    }

    fn release_control(&mut self) -> Vec<Injection> {
        self.controller = None;
        self.relative = None;
        releases(self.pressed.release_all())
    }
}

/// Records `seq` if it is newer than the last one applied.
fn newer(last: &mut Option<u64>, seq: u64) -> bool {
    if last.is_some_and(|l| seq <= l) {
        return false;
    }
    *last = Some(seq);
    true
}

/// Buttons first, then keys in usage order, which puts modifiers last.
fn releases(released: Released) -> Vec<Injection> {
    let buttons = released
        .buttons
        .into_iter()
        .map(|button| Injection::Button {
            button,
            pressed: false,
        });
    let keys = released.keys.into_iter().map(|usage| Injection::Key {
        usage,
        pressed: false,
    });
    buttons.chain(keys).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::id;
    use iw_keymap::usage;

    fn enter(r: &mut Receiver, from: &str, modifiers: Vec<HidUsage>) -> Vec<Injection> {
        r.handle(RemoteEvent::Enter {
            from: id(from),
            position: Point::new(10.0, 20.0),
            modifiers,
            buttons: vec![],
        })
    }

    fn key(r: &mut Receiver, from: &str, usage: HidUsage, pressed: bool) -> Vec<Injection> {
        r.handle(RemoteEvent::Key {
            from: id(from),
            usage,
            pressed,
        })
    }

    fn motion(r: &mut Receiver, from: &str, seq: u64, x: f64) -> Vec<Injection> {
        r.handle(RemoteEvent::Motion {
            from: id(from),
            seq,
            position: Point::new(x, 0.0),
        })
    }

    fn relative(r: &mut Receiver, from: &str, on: bool, session: u64) -> Vec<Injection> {
        ended(r, from, on, session, 0.0, 0.0)
    }

    /// Relative mode `session` beginning or, with how far it moved in all,
    /// ending.
    fn ended(
        r: &mut Receiver,
        from: &str,
        on: bool,
        session: u64,
        x: f64,
        y: f64,
    ) -> Vec<Injection> {
        r.handle(RemoteEvent::Relative {
            from: id(from),
            on,
            session,
            total: Point::new(x, y),
        })
    }

    fn moved(
        r: &mut Receiver,
        from: &str,
        session: u64,
        seq: u64,
        x: f64,
        y: f64,
    ) -> Vec<Injection> {
        r.handle(RemoteEvent::RelativeMotion {
            from: id(from),
            session,
            seq,
            total: Point::new(x, y),
        })
    }

    fn by(x: f64, y: f64) -> Vec<Injection> {
        vec![Injection::MoveBy(Point::new(x, y))]
    }

    #[test]
    fn relative_movements_add_up_whatever_datagrams_are_lost() {
        let mut r = Receiver::default();
        enter(&mut r, "mac", vec![]);
        assert!(relative(&mut r, "mac", true, 1).is_empty());
        assert!(r.relative());
        assert_eq!(moved(&mut r, "mac", 1, 5, 3.0, -1.0), by(3.0, -1.0));
        // 6 and 7 are lost; 8 carries their movement too.
        assert_eq!(moved(&mut r, "mac", 1, 8, 10.5, 2.0), by(7.5, 3.0));
        // 7 arrives late: its movement was already counted.
        assert!(moved(&mut r, "mac", 1, 7, 9.0, 1.0).is_empty());
        // Nothing moved, nothing to do.
        assert!(moved(&mut r, "mac", 1, 9, 10.5, 2.0).is_empty());
        // A flick, then off at once, its last datagrams lost: the end
        // carries what they did.
        assert_eq!(ended(&mut r, "mac", false, 1, 60.5, 2.0), by(50.0, 0.0));
        assert!(!r.relative());
    }

    #[test]
    fn a_relative_movement_counts_only_in_its_own_session() {
        let mut r = Receiver::default();
        enter(&mut r, "mac", vec![]);
        relative(&mut r, "mac", true, 1);
        assert_eq!(moved(&mut r, "mac", 1, 1, 5.0, 0.0), by(5.0, 0.0));
        // Off, then on again at once: a datagram of the new session
        // overtakes both. Taken in the old one, from its total of 5, it
        // would fling the pointer; it is not taken at all.
        assert!(moved(&mut r, "mac", 2, 3, -400.0, 0.0).is_empty());
        assert!(ended(&mut r, "mac", false, 1, 5.0, 0.0).is_empty());
        assert!(relative(&mut r, "mac", true, 2).is_empty());
        // A late one of the old session neither.
        assert!(moved(&mut r, "mac", 1, 2, 900.0, 0.0).is_empty());
        // The next of the new session carries the one it dropped.
        assert_eq!(moved(&mut r, "mac", 2, 4, -401.0, 0.0), by(-401.0, 0.0));
        // An end that is not of the session it is in moves nothing.
        assert!(ended(&mut r, "mac", false, 7, 1e6, 0.0).is_empty());
        assert!(!r.relative());
    }

    #[test]
    fn relative_mode_ignores_positions_and_clicks_where_the_pointer_is() {
        let mut r = Receiver::default();
        enter(&mut r, "mac", vec![]);
        relative(&mut r, "mac", true, 1);
        assert!(motion(&mut r, "mac", 1, 300.0).is_empty());
        let press = |r: &mut Receiver, position: Option<Point>| {
            r.handle(RemoteEvent::Button {
                from: id("mac"),
                button: Button::Left,
                pressed: true,
                position,
            })
        };
        let pressed = vec![Injection::Button {
            button: Button::Left,
            pressed: true,
        }];
        assert_eq!(press(&mut r, Some(Point::new(1.0, 2.0))), pressed);
        // Off again: positions count.
        relative(&mut r, "mac", false, 1);
        assert!(!r.relative());
        assert!(moved(&mut r, "mac", 1, 3, 5.0, 0.0).is_empty());
        assert_eq!(
            motion(&mut r, "mac", 2, 300.0),
            vec![Injection::MoveTo(Point::new(300.0, 0.0))]
        );
        // A button without one, as the controller sends until it knows
        // where the pointer went, goes down where the pointer is.
        r.handle(RemoteEvent::Button {
            from: id("mac"),
            button: Button::Left,
            pressed: false,
            position: None,
        });
        assert_eq!(press(&mut r, None), pressed);
    }

    #[test]
    fn a_wild_relative_total_moves_the_pointer_only_so_far() {
        let mut r = Receiver::default();
        enter(&mut r, "mac", vec![]);
        relative(&mut r, "mac", true, 1);
        assert_eq!(
            moved(&mut r, "mac", 1, 1, 1e9, -1e9),
            by(MAX_RELATIVE_STEP, -MAX_RELATIVE_STEP)
        );
        // Later totals count from it.
        assert_eq!(moved(&mut r, "mac", 1, 2, 1e9 + 2.0, -1e9), by(2.0, 0.0));
        assert!(moved(&mut r, "mac", 1, 3, f64::NAN, 0.0).is_empty());
        assert!(ended(&mut r, "mac", false, 1, f64::INFINITY, 0.0).is_empty());
    }

    #[test]
    fn relative_mode_ends_with_the_controller() {
        let mut r = Receiver::default();
        enter(&mut r, "mac", vec![]);
        // Only the controller can start it.
        relative(&mut r, "win", true, 1);
        assert!(!r.relative());
        relative(&mut r, "mac", true, 1);
        r.handle(RemoteEvent::Leave { from: id("mac") });
        assert!(!r.relative());
        enter(&mut r, "mac", vec![]);
        assert!(!r.relative());
        relative(&mut r, "mac", true, 2);
        r.handle(RemoteEvent::ControllerLost(id("mac")));
        assert!(!r.relative());
        enter(&mut r, "mac", vec![]);
        relative(&mut r, "mac", true, 3);
        r.local_activity();
        assert!(!r.relative());
    }

    #[test]
    fn enter_moves_and_presses_carried_modifiers() {
        let mut r = Receiver::default();
        assert_eq!(
            enter(&mut r, "mac", vec![usage::LEFT_SHIFT]),
            vec![
                Injection::MoveTo(Point::new(10.0, 20.0)),
                Injection::Key {
                    usage: usage::LEFT_SHIFT,
                    pressed: true,
                },
            ]
        );
        assert_eq!(r.controller(), Some(&id("mac")));
    }

    #[test]
    fn a_drag_coming_in_presses_its_button_until_released_or_left() {
        let mut r = Receiver::default();
        let drag_in = |r: &mut Receiver| {
            r.handle(RemoteEvent::Enter {
                from: id("mac"),
                position: Point::new(10.0, 20.0),
                modifiers: vec![],
                buttons: vec![Button::Left],
            })
        };
        let left = |pressed| Injection::Button {
            button: Button::Left,
            pressed,
        };
        // Pressed where the cursor arrives.
        assert_eq!(
            drag_in(&mut r),
            vec![Injection::MoveTo(Point::new(10.0, 20.0)), left(true)]
        );
        // The drop.
        let drop = r.handle(RemoteEvent::Button {
            from: id("mac"),
            button: Button::Left,
            pressed: false,
            position: Some(Point::new(30.0, 40.0)),
        });
        assert_eq!(
            drop,
            vec![Injection::MoveTo(Point::new(30.0, 40.0)), left(false)]
        );
        // Leaving still dragging lets go of it.
        drag_in(&mut r);
        assert_eq!(
            r.handle(RemoteEvent::Leave { from: id("mac") }),
            vec![left(false)]
        );
    }

    #[test]
    fn ignores_everyone_but_the_controller() {
        let mut r = Receiver::default();
        assert!(key(&mut r, "mac", usage::A, true).is_empty());
        enter(&mut r, "mac", vec![]);
        assert!(key(&mut r, "mini", usage::A, true).is_empty());
        assert!(motion(&mut r, "mini", 1, 5.0).is_empty());
        assert!(r.handle(RemoteEvent::Leave { from: id("mini") }).is_empty());
        assert_eq!(r.controller(), Some(&id("mac")));
    }

    #[test]
    fn motion_is_latest_wins() {
        let mut r = Receiver::default();
        enter(&mut r, "mac", vec![]);
        assert_eq!(
            motion(&mut r, "mac", 2, 5.0),
            vec![Injection::MoveTo(Point::new(5.0, 0.0))]
        );
        assert!(motion(&mut r, "mac", 1, 4.0).is_empty());
        assert!(motion(&mut r, "mac", 2, 4.0).is_empty());
        assert_eq!(motion(&mut r, "mac", 3, 6.0).len(), 1);
    }

    #[test]
    fn sequence_numbers_restart_on_enter() {
        let mut r = Receiver::default();
        enter(&mut r, "mac", vec![]);
        motion(&mut r, "mac", 50, 1.0);
        r.handle(RemoteEvent::Leave { from: id("mac") });
        enter(&mut r, "mac", vec![]);
        assert_eq!(motion(&mut r, "mac", 1, 2.0).len(), 1);
    }

    #[test]
    fn stale_scroll_is_dropped() {
        let mut r = Receiver::default();
        enter(&mut r, "mac", vec![]);
        let scroll = |seq| RemoteEvent::Scroll {
            from: id("mac"),
            seq,
            dx: 0.0,
            dy: -1.0,
        };
        assert_eq!(r.handle(scroll(5)).len(), 1);
        assert!(r.handle(scroll(4)).is_empty());
    }

    #[test]
    fn buttons_move_to_their_position_first() {
        let mut r = Receiver::default();
        enter(&mut r, "mac", vec![]);
        let press = RemoteEvent::Button {
            from: id("mac"),
            button: Button::Left,
            pressed: true,
            position: Some(Point::new(7.0, 8.0)),
        };
        assert_eq!(
            r.handle(press),
            vec![
                Injection::MoveTo(Point::new(7.0, 8.0)),
                Injection::Button {
                    button: Button::Left,
                    pressed: true,
                },
            ]
        );
        // A release of a button that is not held only moves the cursor.
        let release_right = RemoteEvent::Button {
            from: id("mac"),
            button: Button::Right,
            pressed: false,
            position: Some(Point::new(7.0, 8.0)),
        };
        assert_eq!(
            r.handle(release_right),
            vec![Injection::MoveTo(Point::new(7.0, 8.0))]
        );
    }

    #[test]
    fn leave_releases_everything_held() {
        let mut r = Receiver::default();
        enter(&mut r, "mac", vec![usage::LEFT_GUI]);
        key(&mut r, "mac", usage::C, true);
        r.handle(RemoteEvent::Button {
            from: id("mac"),
            button: Button::Left,
            pressed: true,
            position: Some(Point::default()),
        });
        assert_eq!(
            r.handle(RemoteEvent::Leave { from: id("mac") }),
            vec![
                Injection::Button {
                    button: Button::Left,
                    pressed: false,
                },
                Injection::Key {
                    usage: usage::C,
                    pressed: false,
                },
                Injection::Key {
                    usage: usage::LEFT_GUI,
                    pressed: false,
                },
            ]
        );
        assert_eq!(r.controller(), None);
        assert!(key(&mut r, "mac", usage::C, false).is_empty());
    }

    #[test]
    fn losing_the_controller_releases_everything_held() {
        let mut r = Receiver::default();
        enter(&mut r, "mac", vec![]);
        key(&mut r, "mac", usage::A, true);
        assert_eq!(
            r.handle(RemoteEvent::ControllerLost(id("mac"))),
            vec![Injection::Key {
                usage: usage::A,
                pressed: false,
            }]
        );
        assert_eq!(r.controller(), None);
    }

    #[test]
    fn a_new_controller_releases_the_old_ones_keys_first() {
        let mut r = Receiver::default();
        enter(&mut r, "mac", vec![]);
        key(&mut r, "mac", usage::A, true);
        assert_eq!(
            enter(&mut r, "mini", vec![]),
            vec![
                Injection::Key {
                    usage: usage::A,
                    pressed: false,
                },
                Injection::MoveTo(Point::new(10.0, 20.0)),
            ]
        );
        assert_eq!(r.controller(), Some(&id("mini")));
        assert!(key(&mut r, "mac", usage::C, true).is_empty());
    }

    #[test]
    fn local_activity_hands_control_back_to_the_local_user() {
        let mut r = Receiver::default();
        assert_eq!(r.local_activity(), None);
        enter(&mut r, "mac", vec![]);
        key(&mut r, "mac", usage::A, true);
        assert_eq!(
            r.local_activity(),
            Some((
                id("mac"),
                vec![Injection::Key {
                    usage: usage::A,
                    pressed: false,
                }]
            ))
        );
        assert_eq!(r.controller(), None);
        assert!(motion(&mut r, "mac", 9, 1.0).is_empty());
    }
}
