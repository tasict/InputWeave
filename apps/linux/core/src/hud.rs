//! What the HUD says, as the macOS
//! app's `HUDController` and the Windows app's `HudWatch`: why the keyboard
//! and mouse came back, the cursor being locked or unlocked, relative mode
//! starting and ending, a computer being woken and waking up, and the
//! take-back hint the first three times this computer takes control of
//! another. It compares each state with the one before, so only a change is
//! news; the first state seen after the app starts is only remembered, and
//! so are relative mode and waking in the first state after the service is
//! reached again. Where the HUD shows (an overlay, or a notification) is
//! the app's.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use iw_proto::app::v1::{ConnectionState, RelativeEndReason, ReturnReason, WakeState};

use crate::snapshot::{Link, Snapshot};
use crate::text::{UiText, keys};
use crate::ui;

/// How many times the take-back hint is shown, ever.
pub const HINT_TIMES: u32 = 3;

/// Relative mode beginning or ending by itself, as an app takes or lets go
/// of the pointer, is told for each computer the first time of each kind,
/// then at most once in this time.
pub const QUIET_FOR: Duration = Duration::from_secs(60);

/// How long a note stays.
pub const SHOWN_FOR: Duration = Duration::from_millis(2500);

#[derive(Default)]
struct Told {
    entered: bool,
    released: bool,
    at: Option<SystemTime>,
}

#[derive(Default)]
pub struct HudWatch {
    primed: bool,
    seen_returns: u64,
    was_locked: bool,
    was_controlling: bool,
    relative_primed: bool,
    relative_device: String,
    relative_ends: u64,
    seen_refusal: u64,
    told: HashMap<String, Told>,
    wake_primed: bool,
    /// Each other device's wake state as last seen.
    wakes: HashMap<String, WakeState>,
}

impl HudWatch {
    /// Takes in the latest state and returns the note to show, if any.
    /// `hints_shown` is how many take-back hints were shown so far, one
    /// more when this returns one.
    pub fn update(
        &mut self,
        snapshot: &Snapshot,
        hints_shown: &mut u32,
        now: SystemTime,
    ) -> Option<UiText> {
        if snapshot.link != Link::Connected {
            // A restarted service starts over; what it says first is no news.
            self.relative_primed = false;
            self.wake_primed = false;
            return None;
        }
        let status = &snapshot.status;
        let controlling = snapshot.controlling_other();
        let first = !self.primed;
        let returns_before = self.seen_returns;
        let locked_before = self.was_locked;
        let controlling_before = self.was_controlling;
        let relative_first = !self.relative_primed;
        let relative_before = std::mem::take(&mut self.relative_device);
        let ends_before = self.relative_ends;
        let refusal_before = self.seen_refusal;
        self.primed = true;
        self.seen_returns = status.control_returns;
        self.was_locked = status.locked;
        self.was_controlling = controlling;
        self.relative_primed = true;
        self.relative_device = status.relative_mouse.clone();
        self.relative_ends = status.relative_ends;
        self.seen_refusal = snapshot.relative_refusals;
        let wake = self.wake_message(snapshot);
        if first {
            return None;
        }
        if status.control_returns > returns_before
            && let Some(back) = return_message(snapshot)
        {
            return Some(back);
        }
        if self.seen_refusal > refusal_before {
            return Some(ui!(
                keys::HUD_RELATIVE_UNSUPPORTED,
                snapshot.name_of(&snapshot.relative_refused_device)
            ));
        }
        // A restarted service counts from 0: fewer ends than before are only
        // remembered.
        if !relative_first
            && status.relative_ends >= ends_before
            && let Some(relative) =
                self.relative_message(snapshot, &relative_before, ends_before, now)
        {
            // Relative mode as the cursor first crosses: how to take back
            // control goes with it.
            if controlling && !controlling_before && *hints_shown < HINT_TIMES {
                *hints_shown += 1;
                return Some(ui!(
                    keys::HUD_TWO_LINES,
                    snapshot.take_back_reminder(),
                    relative
                ));
            }
            return Some(relative);
        }
        if status.locked != locked_before {
            return Some(lock_message(snapshot));
        }
        if wake.is_some() {
            return wake;
        }
        let dragging = status
            .dragging
            .as_ref()
            .is_some_and(|d| !d.device_id.is_empty());
        if controlling && !controlling_before && *hints_shown < HINT_TIMES && !dragging {
            *hints_shown += 1;
            return Some(snapshot.take_back_reminder());
        }
        None
    }

    /// Relative mode starting or ending since the last state, as the
    /// background service says why it ended (`Status.last_relative_end`).
    /// Having ended and begun again in between, it began. When it ended as
    /// the keyboard and mouse came back or sharing was paused, those say so
    /// themselves; what began or ended by itself is told now and then
    /// ([`QUIET_FOR`]).
    fn relative_message(
        &mut self,
        snapshot: &Snapshot,
        before: &str,
        ends_before: u64,
        now: SystemTime,
    ) -> Option<UiText> {
        let status = &snapshot.status;
        let ended = status.relative_ends > ends_before;
        let device = status.relative_mouse.as_str();
        if !device.is_empty() && (device != before || ended) {
            if status.relative_mouse_automatic && !self.allows(device, true, now) {
                return None;
            }
            let name = snapshot.name_of(device);
            return Some(match snapshot.relative_shortcut() {
                Some(shortcut) => ui!(keys::HUD_RELATIVE_ENTERED, name, shortcut),
                None => ui!(
                    keys::HUD_RELATIVE_ENTERED_TAKE_BACK,
                    name,
                    snapshot.take_back_shortcut()
                ),
            });
        }
        let end = status.last_relative_end.as_ref().filter(|_| ended)?;
        match end.reason() {
            RelativeEndReason::TurnedOff
            | RelativeEndReason::AutoOff
            | RelativeEndReason::Jumped => Some(ui!(keys::HUD_RELATIVE_ENDED)),
            RelativeEndReason::Released if self.allows(&end.device_id, false, now) => Some(ui!(
                keys::HUD_RELATIVE_RELEASED,
                snapshot.name_of(&end.device_id)
            )),
            _ => None,
        }
    }

    /// A device being woken ("Waking Mac mini…": its wake state became
    /// WAKING), or connected after it was ("Mac mini is awake."), from a
    /// service that wakes devices. Remembers every other device's state,
    /// whatever it says, and forgets devices no longer listed; in the first
    /// state after the service is reached it only remembers. Of two devices
    /// at once, the first in the list, as the other apps tell it.
    fn wake_message(&mut self, snapshot: &Snapshot) -> Option<UiText> {
        let status = &snapshot.status;
        let primed = std::mem::replace(&mut self.wake_primed, true);
        let before = std::mem::take(&mut self.wakes);
        let mut note = None;
        for device in &status.devices {
            if device.id == status.this_device_id {
                continue;
            }
            let wake = device.wake();
            self.wakes.insert(device.id.clone(), wake);
            if !primed || !status.wake_supported {
                continue;
            }
            let was = before.get(&device.id).copied().unwrap_or(WakeState::None);
            if note.is_some() {
                continue;
            }
            if wake == WakeState::Waking && was != WakeState::Waking {
                note = Some(ui!(keys::HUD_WAKING, snapshot.name_of(&device.id)));
            } else if was == WakeState::Waking && device.state() == ConnectionState::Connected {
                note = Some(ui!(keys::HUD_AWAKE, snapshot.name_of(&device.id)));
            }
        }
        note
    }

    /// Whether to tell relative mode beginning (`entered`) or ending by
    /// itself on `device` at `now`: the first time of each kind, then at
    /// most once in [`QUIET_FOR`]. If so, it counts as told.
    fn allows(&mut self, device: &str, entered: bool, now: SystemTime) -> bool {
        let seen = self.told.entry(device.to_owned()).or_default();
        let first = if entered {
            !seen.entered
        } else {
            !seen.released
        };
        if !first
            && seen
                .at
                .is_some_and(|at| now.duration_since(at).unwrap_or_default() < QUIET_FOR)
        {
            return false;
        }
        if entered {
            seen.entered = true;
        } else {
            seen.released = true;
        }
        seen.at = Some(now);
        true
    }
}

fn lock_message(snapshot: &Snapshot) -> UiText {
    if !snapshot.status.locked {
        return ui!(keys::HUD_UNLOCKED);
    }
    // Locked while the cursor is on another computer, it stays there.
    if snapshot.controlling_other() {
        ui!(
            keys::HUD_LOCKED_TO,
            snapshot.name_of(&snapshot.status.active_device_id),
            snapshot.lock_shortcut()
        )
    } else {
        ui!(keys::HUD_LOCKED_HERE, snapshot.lock_shortcut())
    }
}

fn return_message(snapshot: &Snapshot) -> Option<UiText> {
    let name = snapshot.name_of(&snapshot.status.last_return_device_id);
    match snapshot.status.last_return() {
        ReturnReason::PeerLost => Some(ui!(keys::HUD_PEER_LOST, name)),
        ReturnReason::RemoteActivity => Some(ui!(keys::HUD_REMOTE_ACTIVITY, name)),
        ReturnReason::Hotkey => Some(ui!(keys::HUD_BACK_HERE)),
        ReturnReason::LayoutChanged => Some(ui!(keys::HUD_LAYOUT_CHANGED)),
        ReturnReason::ScreenLocked => Some(ui!(keys::HUD_SCREEN_LOCKED, name)),
        ReturnReason::InputUnavailable => Some(ui!(keys::HUD_INPUT_UNAVAILABLE, name)),
        ReturnReason::PeerPaused => Some(ui!(keys::HUD_PEER_PAUSED, name)),
        ReturnReason::CaptureEnded => Some(ui!(keys::HUD_CAPTURE_ENDED)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcut;
    use iw_proto::app::v1::{ConnectionState, DeviceStatus, Hotkeys, RelativeEnd, Status};

    #[derive(Default)]
    struct Model {
        link: Option<Link>,
        active: &'static str,
        locked: bool,
        hotkeys: Option<Hotkeys>,
        relative: &'static str,
        automatic: bool,
        ends: u64,
        reason: Option<RelativeEndReason>,
        returns: u64,
        last_return: Option<ReturnReason>,
    }

    fn model(m: Model) -> Snapshot {
        let hotkeys = m.hotkeys.unwrap_or(Hotkeys {
            relative: None,
            ..shortcut::defaults()
        });
        Snapshot {
            link: m.link.unwrap_or(Link::Connected),
            status: Status {
                sharing: true,
                this_device_id: "me".into(),
                this_device_name: "ubuntu-desk".into(),
                active_device_id: m.active.into(),
                locked: m.locked,
                relative_supported: hotkeys.relative.is_some(),
                hotkeys: Some(hotkeys),
                relative_mouse: m.relative.into(),
                relative_mouse_automatic: m.automatic,
                relative_ends: m.ends,
                last_relative_end: m.reason.map(|r| RelativeEnd {
                    device_id: "mac".into(),
                    reason: r as i32,
                }),
                control_returns: m.returns,
                last_return: m.last_return.unwrap_or_default() as i32,
                last_return_device_id: "mac".into(),
                devices: vec![DeviceStatus {
                    id: "mac".into(),
                    name: "MacBook Pro".into(),
                    state: ConnectionState::Connected as i32,
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn with_relative() -> Option<Hotkeys> {
        Some(shortcut::defaults())
    }

    fn start() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)
    }

    fn at(seconds: f64) -> SystemTime {
        start() + Duration::from_secs_f64(seconds)
    }

    #[test]
    fn remembers_the_first_state_and_tells_only_changes() {
        let mut watch = HudWatch::default();
        let mut hints = 0;
        // Already locked and on the Mac when the app starts: nothing to say.
        let first = model(Model {
            active: "mac",
            locked: true,
            returns: 4,
            ..Default::default()
        });
        assert_eq!(watch.update(&first, &mut hints, start()), None);
        assert_eq!(watch.update(&first, &mut hints, start()), None);
        assert_eq!(hints, 0);
    }

    #[test]
    fn says_why_the_keyboard_and_mouse_came_back() {
        let mut watch = HudWatch::default();
        let mut hints = 3;
        watch.update(
            &model(Model {
                active: "mac",
                ..Default::default()
            }),
            &mut hints,
            start(),
        );
        let back = |returns, reason| {
            model(Model {
                returns,
                last_return: Some(reason),
                ..Default::default()
            })
        };
        assert_eq!(
            watch.update(&back(1, ReturnReason::PeerLost), &mut hints, start()),
            Some(ui!(keys::HUD_PEER_LOST, "MacBook Pro"))
        );
        assert_eq!(
            watch.update(&back(2, ReturnReason::RemoteActivity), &mut hints, start()),
            Some(ui!(keys::HUD_REMOTE_ACTIVITY, "MacBook Pro"))
        );
        assert_eq!(
            watch.update(&back(2, ReturnReason::RemoteActivity), &mut hints, start()),
            None
        );
        assert_eq!(
            watch.update(&back(3, ReturnReason::Hotkey), &mut hints, start()),
            Some(ui!(keys::HUD_BACK_HERE))
        );
        assert_eq!(
            watch.update(&back(4, ReturnReason::LayoutChanged), &mut hints, start()),
            Some(ui!(keys::HUD_LAYOUT_CHANGED))
        );
        // Pausing says so elsewhere; no note.
        assert_eq!(
            watch.update(&back(5, ReturnReason::SharingPaused), &mut hints, start()),
            None
        );
        assert_eq!(
            watch.update(&back(6, ReturnReason::ScreenLocked), &mut hints, start()),
            Some(ui!(keys::HUD_SCREEN_LOCKED, "MacBook Pro"))
        );
        assert_eq!(
            watch.update(
                &back(7, ReturnReason::InputUnavailable),
                &mut hints,
                start()
            ),
            Some(ui!(keys::HUD_INPUT_UNAVAILABLE, "MacBook Pro"))
        );
        // The other computer paused sharing, or the desktop took the
        // keyboard and mouse back (Wayland).
        assert_eq!(
            watch.update(&back(8, ReturnReason::PeerPaused), &mut hints, start()),
            Some(ui!(keys::HUD_PEER_PAUSED, "MacBook Pro"))
        );
        assert_eq!(
            watch.update(&back(9, ReturnReason::CaptureEnded), &mut hints, start()),
            Some(ui!(keys::HUD_CAPTURE_ENDED))
        );
    }

    #[test]
    fn says_where_the_cursor_is_locked() {
        let mut watch = HudWatch::default();
        let mut hints = 3;
        watch.update(&model(Model::default()), &mut hints, start());
        assert_eq!(
            watch.update(
                &model(Model {
                    locked: true,
                    ..Default::default()
                }),
                &mut hints,
                start()
            ),
            Some(ui!(keys::HUD_LOCKED_HERE, "Ctrl+Alt+Shift+L"))
        );
        assert_eq!(
            watch.update(&model(Model::default()), &mut hints, start()),
            Some(ui!(keys::HUD_UNLOCKED))
        );
        watch.update(
            &model(Model {
                active: "mac",
                ..Default::default()
            }),
            &mut hints,
            start(),
        );
        assert_eq!(
            watch.update(
                &model(Model {
                    active: "mac",
                    locked: true,
                    ..Default::default()
                }),
                &mut hints,
                start()
            ),
            Some(ui!(keys::HUD_LOCKED_TO, "MacBook Pro", "Ctrl+Alt+Shift+L"))
        );
    }

    #[test]
    fn writes_the_hotkeys_the_service_reports() {
        let mut watch = HudWatch::default();
        let mut hints = 0;
        let hotkeys = Hotkeys {
            take_back: "control+alt+F12".into(),
            lock: "gui+shift+KeyK".into(),
            jump_modifiers: String::new(),
            relative: None,
            take_back_double: None,
        };
        watch.update(
            &model(Model {
                hotkeys: Some(hotkeys.clone()),
                ..Default::default()
            }),
            &mut hints,
            start(),
        );
        assert_eq!(
            watch.update(
                &model(Model {
                    active: "mac",
                    hotkeys: Some(hotkeys.clone()),
                    ..Default::default()
                }),
                &mut hints,
                start()
            ),
            Some(ui!(keys::HUD_TAKE_BACK_HINT, "Ctrl+Alt+F12"))
        );
        assert_eq!(
            watch.update(
                &model(Model {
                    active: "mac",
                    locked: true,
                    hotkeys: Some(hotkeys),
                    ..Default::default()
                }),
                &mut hints,
                start()
            ),
            Some(ui!(keys::HUD_LOCKED_TO, "MacBook Pro", "Shift+Super+K"))
        );
    }

    #[test]
    fn hints_how_to_take_back_control_three_times() {
        let mut watch = HudWatch::default();
        let mut hints = 0;
        let here = model(Model::default());
        let there = model(Model {
            active: "mac",
            ..Default::default()
        });
        watch.update(&here, &mut hints, start());
        for i in 1..=HINT_TIMES {
            assert_eq!(
                watch.update(&there, &mut hints, start()),
                Some(ui!(keys::HUD_TAKE_BACK_HINT, "Ctrl+Alt+Shift+Esc"))
            );
            assert_eq!(hints, i);
            assert_eq!(watch.update(&there, &mut hints, start()), None);
            watch.update(&here, &mut hints, start());
        }
        assert_eq!(watch.update(&there, &mut hints, start()), None);
        assert_eq!(hints, HINT_TIMES);
    }

    /// With the double press on, every notice that says how to take
    /// control back says it as the panel does.
    #[test]
    fn hints_name_the_double_press_while_it_is_on() {
        let double = Some(shortcut::with_double_press(&shortcut::defaults(), true));
        let hint = ui!(keys::SUBTITLE_CONTROLLING_DOUBLE, "Ctrl+Alt+Shift+Esc");
        let mut watch = HudWatch::default();
        let mut hints = 0;
        watch.update(
            &model(Model {
                hotkeys: double.clone(),
                ..Default::default()
            }),
            &mut hints,
            start(),
        );
        let there = model(Model {
            active: "mac",
            hotkeys: double.clone(),
            ..Default::default()
        });
        assert_eq!(
            watch.update(&there, &mut hints, start()),
            Some(hint.clone())
        );
        assert_eq!(there.take_back_reminder(), hint);
        assert_eq!(
            there.take_back_worked().to_string(),
            "It works. Press Ctrl+Alt+Shift+Esc or double-press Ctrl to take back control."
        );
        // Off: the words as before.
        let off = model(Model {
            active: "mac",
            ..Default::default()
        });
        assert_eq!(
            off.take_back_reminder(),
            ui!(keys::HUD_TAKE_BACK_HINT, "Ctrl+Alt+Shift+Esc")
        );
        assert_eq!(
            off.take_back_worked(),
            ui!(keys::SETUP_TRY_DONE, "Ctrl+Alt+Shift+Esc")
        );
    }

    /// In relative mouse the service ignores the double press (games often
    /// use Ctrl), so nothing that tells how to take control back names it,
    /// even with it on.
    #[test]
    fn relative_mouse_hints_leave_out_the_double_press() {
        let mut no_relative = shortcut::with_double_press(&shortcut::defaults(), true);
        no_relative.relative = Some(String::new());
        let mut watch = HudWatch::default();
        let mut hints = 0;
        watch.update(
            &model(Model {
                hotkeys: Some(no_relative.clone()),
                ..Default::default()
            }),
            &mut hints,
            start(),
        );
        let relative = model(Model {
            active: "mac",
            hotkeys: Some(no_relative),
            relative: "mac",
            ..Default::default()
        });
        assert!(relative.double_press_on());
        // Relative mouse as the cursor first crosses, without its own
        // shortcut: the reminder and the relative notice.
        let shown = watch.update(&relative, &mut hints, start());
        assert_eq!(
            shown,
            Some(ui!(
                keys::HUD_TWO_LINES,
                ui!(keys::HUD_TAKE_BACK_HINT, "Ctrl+Alt+Shift+Esc"),
                ui!(
                    keys::HUD_RELATIVE_ENTERED_TAKE_BACK,
                    "MacBook Pro",
                    "Ctrl+Alt+Shift+Esc"
                )
            ))
        );
        assert!(!shown.unwrap().to_string().contains("double-press"));
        assert_eq!(
            crate::status_text::subtitle(&relative).unwrap().to_string(),
            "Press Ctrl+Alt+Shift+Esc to take back control."
        );
        assert_eq!(
            relative.take_back_reminder(),
            ui!(keys::HUD_TAKE_BACK_HINT, "Ctrl+Alt+Shift+Esc")
        );
        assert_eq!(
            relative.take_back_worked(),
            ui!(keys::SETUP_TRY_DONE, "Ctrl+Alt+Shift+Esc")
        );
    }

    #[test]
    fn says_when_relative_mode_starts_and_ends() {
        let mut watch = HudWatch::default();
        let mut hints = 3;
        let on = |hotkeys, relative, automatic, ends, reason| {
            model(Model {
                active: "mac",
                hotkeys,
                relative,
                automatic,
                ends,
                reason,
                ..Default::default()
            })
        };
        watch.update(
            &on(with_relative(), "", false, 0, None),
            &mut hints,
            start(),
        );
        let entered = ui!(
            keys::HUD_RELATIVE_ENTERED,
            "MacBook Pro",
            "Ctrl+Alt+Shift+G"
        );
        assert_eq!(
            watch.update(
                &on(with_relative(), "mac", false, 0, None),
                &mut hints,
                start()
            ),
            Some(entered.clone())
        );
        assert_eq!(
            watch.update(
                &on(with_relative(), "mac", false, 0, None),
                &mut hints,
                start()
            ),
            None
        );
        assert_eq!(
            watch.update(
                &on(
                    with_relative(),
                    "",
                    false,
                    1,
                    Some(RelativeEndReason::TurnedOff)
                ),
                &mut hints,
                start()
            ),
            Some(ui!(keys::HUD_RELATIVE_ENDED))
        );
        // Its hotkey off: it ends as control is taken back.
        let off = Some(Hotkeys {
            relative: Some(String::new()),
            ..shortcut::defaults()
        });
        assert_eq!(
            watch.update(
                &on(
                    off.clone(),
                    "mac",
                    true,
                    1,
                    Some(RelativeEndReason::TurnedOff)
                ),
                &mut hints,
                start()
            ),
            Some(ui!(
                keys::HUD_RELATIVE_ENTERED_TAKE_BACK,
                "MacBook Pro",
                "Ctrl+Alt+Shift+Esc"
            ))
        );
        // The game let go.
        assert_eq!(
            watch.update(
                &on(off, "", false, 2, Some(RelativeEndReason::Released)),
                &mut hints,
                start()
            ),
            Some(ui!(keys::HUD_RELATIVE_RELEASED, "MacBook Pro"))
        );
        // Ended and began again between two states: it began.
        assert_eq!(
            watch.update(
                &on(with_relative(), "mac", false, 2, None),
                &mut hints,
                start()
            ),
            Some(entered.clone())
        );
        assert_eq!(
            watch.update(
                &on(
                    with_relative(),
                    "mac",
                    false,
                    3,
                    Some(RelativeEndReason::TurnedOff)
                ),
                &mut hints,
                start()
            ),
            Some(entered)
        );
        // Turning automatic relative mode off, or jumping, ends it.
        for (ends, reason) in [
            (4, RelativeEndReason::AutoOff),
            (6, RelativeEndReason::Jumped),
        ] {
            assert_eq!(
                watch.update(
                    &on(with_relative(), "", false, ends, Some(reason)),
                    &mut hints,
                    start()
                ),
                Some(ui!(keys::HUD_RELATIVE_ENDED))
            );
            watch.update(
                &on(with_relative(), "mac", false, ends, None),
                &mut hints,
                start(),
            );
        }
        // Sharing paused: said elsewhere.
        assert_eq!(
            watch.update(
                &on(
                    with_relative(),
                    "",
                    false,
                    7,
                    Some(RelativeEndReason::SharingPaused)
                ),
                &mut hints,
                start()
            ),
            None
        );
    }

    #[test]
    fn why_the_keyboard_and_mouse_came_back_comes_before_relative_mode() {
        let mut watch = HudWatch::default();
        let mut hints = 3;
        watch.update(
            &model(Model {
                active: "mac",
                hotkeys: with_relative(),
                relative: "mac",
                ..Default::default()
            }),
            &mut hints,
            start(),
        );
        let back = model(Model {
            hotkeys: with_relative(),
            ends: 1,
            reason: Some(RelativeEndReason::DeviceLost),
            returns: 1,
            last_return: Some(ReturnReason::PeerLost),
            ..Default::default()
        });
        assert_eq!(
            watch.update(&back, &mut hints, start()),
            Some(ui!(keys::HUD_PEER_LOST, "MacBook Pro"))
        );
        watch.update(
            &model(Model {
                active: "mac",
                hotkeys: with_relative(),
                relative: "mac",
                ends: 1,
                returns: 1,
                ..Default::default()
            }),
            &mut hints,
            start(),
        );
        let hotkey = model(Model {
            hotkeys: with_relative(),
            ends: 2,
            reason: Some(RelativeEndReason::ControlReturned),
            returns: 2,
            last_return: Some(ReturnReason::Hotkey),
            ..Default::default()
        });
        assert_eq!(
            watch.update(&hotkey, &mut hints, start()),
            Some(ui!(keys::HUD_BACK_HERE))
        );
        assert_eq!(watch.update(&hotkey, &mut hints, start()), None);
    }

    #[test]
    fn tells_what_a_game_does_now_and_then() {
        let mut watch = HudWatch::default();
        let mut hints = 3;
        let mut step = |seconds: f64,
                        relative: &'static str,
                        automatic: bool,
                        ends: u64,
                        reason: RelativeEndReason| {
            watch.update(
                &model(Model {
                    active: "mac",
                    hotkeys: with_relative(),
                    relative,
                    automatic,
                    ends,
                    reason: Some(reason),
                    ..Default::default()
                }),
                &mut hints,
                at(seconds),
            )
        };
        let released = RelativeEndReason::Released;
        let entered = Some(ui!(
            keys::HUD_RELATIVE_ENTERED,
            "MacBook Pro",
            "Ctrl+Alt+Shift+G"
        ));
        let gone = Some(ui!(keys::HUD_RELATIVE_RELEASED, "MacBook Pro"));
        step(0.0, "", true, 0, released);
        assert_eq!(step(1.0, "mac", true, 0, released), entered);
        assert_eq!(step(5.0, "", true, 1, released), gone);
        assert_eq!(step(10.0, "mac", true, 1, released), None);
        assert_eq!(step(20.0, "", true, 2, released), None);
        // By hand, always.
        assert_eq!(step(21.0, "mac", false, 2, released), entered);
        assert_eq!(
            step(22.0, "", true, 3, RelativeEndReason::TurnedOff),
            Some(ui!(keys::HUD_RELATIVE_ENDED))
        );
        assert_eq!(step(64.0, "mac", true, 3, released), None);
        assert_eq!(step(64.5, "", true, 4, released), None);
        // A minute after the last one told.
        assert_eq!(step(65.0, "mac", true, 4, released), entered);
        assert_eq!(step(100.0, "", true, 5, released), None);
        assert_eq!(step(110.0, "mac", true, 5, released), None);
        assert_eq!(step(126.0, "", true, 6, released), gone);
    }

    #[test]
    fn the_first_crossing_into_relative_mode_says_how_to_take_back_control_too() {
        let mut watch = HudWatch::default();
        let mut hints = 0;
        watch.update(
            &model(Model {
                hotkeys: with_relative(),
                ..Default::default()
            }),
            &mut hints,
            start(),
        );
        let crossing = model(Model {
            active: "mac",
            hotkeys: with_relative(),
            relative: "mac",
            automatic: true,
            ..Default::default()
        });
        assert_eq!(
            watch.update(&crossing, &mut hints, start()),
            Some(ui!(
                keys::HUD_TWO_LINES,
                ui!(keys::HUD_TAKE_BACK_HINT, "Ctrl+Alt+Shift+Esc"),
                ui!(
                    keys::HUD_RELATIVE_ENTERED,
                    "MacBook Pro",
                    "Ctrl+Alt+Shift+G"
                )
            ))
        );
        assert_eq!(hints, 1);
        // Once the hints are used up, relative mode alone.
        hints = HINT_TIMES;
        watch.update(
            &model(Model {
                hotkeys: with_relative(),
                ends: 1,
                reason: Some(RelativeEndReason::ControlReturned),
                ..Default::default()
            }),
            &mut hints,
            start(),
        );
        assert_eq!(
            watch.update(
                &model(Model {
                    active: "mac",
                    hotkeys: with_relative(),
                    relative: "mac",
                    ends: 1,
                    ..Default::default()
                }),
                &mut hints,
                start()
            ),
            Some(ui!(
                keys::HUD_RELATIVE_ENTERED,
                "MacBook Pro",
                "Ctrl+Alt+Shift+G"
            ))
        );
    }

    #[test]
    fn fewer_ends_than_before_are_a_restarted_service() {
        let mut watch = HudWatch::default();
        let mut hints = 3;
        let state = |relative: &'static str, ends, reason| {
            model(Model {
                active: "mac",
                hotkeys: with_relative(),
                relative,
                ends,
                reason,
                ..Default::default()
            })
        };
        watch.update(&state("mac", 5, None), &mut hints, start());
        // Counted from 0 again, with relative mode on elsewhere or off: no note.
        assert_eq!(
            watch.update(
                &state("mac", 1, Some(RelativeEndReason::TurnedOff)),
                &mut hints,
                start()
            ),
            None
        );
        assert_eq!(watch.update(&state("", 0, None), &mut hints, start()), None);
        // From there, an increase is news again.
        assert_eq!(
            watch.update(
                &state("", 1, Some(RelativeEndReason::TurnedOff)),
                &mut hints,
                start()
            ),
            Some(ui!(keys::HUD_RELATIVE_ENDED))
        );
    }

    #[test]
    fn relative_mode_after_reaching_the_service_again_is_no_news() {
        let mut watch = HudWatch::default();
        let mut hints = 3;
        let state = |relative: &'static str, automatic, ends, reason| {
            model(Model {
                active: "mac",
                hotkeys: with_relative(),
                relative,
                automatic,
                ends,
                reason,
                ..Default::default()
            })
        };
        watch.update(&state("mac", false, 4, None), &mut hints, start());
        let offline = model(Model {
            link: Some(Link::Connecting),
            ..Default::default()
        });
        assert_eq!(watch.update(&offline, &mut hints, start()), None);
        // A restarted service counts from 0, with relative mode on again.
        assert_eq!(
            watch.update(&state("mac", true, 0, None), &mut hints, start()),
            None
        );
        assert_eq!(
            watch.update(
                &state("", false, 1, Some(RelativeEndReason::TurnedOff)),
                &mut hints,
                start()
            ),
            Some(ui!(keys::HUD_RELATIVE_ENDED))
        );
    }

    #[test]
    fn says_when_a_computer_has_no_relative_mode() {
        let mut watch = HudWatch::default();
        let mut hints = 3;
        let mut there = model(Model {
            active: "mac",
            hotkeys: with_relative(),
            ..Default::default()
        });
        watch.update(&there, &mut hints, start());
        there.relative_refusals = 1;
        there.relative_refused_device = "mac".into();
        assert_eq!(
            watch.update(&there, &mut hints, start()),
            Some(ui!(keys::HUD_RELATIVE_UNSUPPORTED, "MacBook Pro"))
        );
        assert_eq!(watch.update(&there, &mut hints, start()), None);
        // Each time counts anew.
        there.relative_refusals = 2;
        assert_eq!(
            watch.update(&there, &mut hints, start()),
            Some(ui!(keys::HUD_RELATIVE_UNSUPPORTED, "MacBook Pro"))
        );
        // A refusal from before the app started is no news.
        let mut later = HudWatch::default();
        assert_eq!(later.update(&there, &mut hints, start()), None);
        assert_eq!(later.update(&there, &mut hints, start()), None);
    }

    #[test]
    fn says_nothing_while_the_service_is_out_of_reach() {
        let mut watch = HudWatch::default();
        let mut hints = 0;
        watch.update(&model(Model::default()), &mut hints, start());
        let offline = model(Model {
            link: Some(Link::Connecting),
            locked: true,
            ..Default::default()
        });
        assert_eq!(watch.update(&offline, &mut hints, start()), None);
        // A restarted service unlocks.
        watch.update(
            &model(Model {
                locked: true,
                ..Default::default()
            }),
            &mut hints,
            start(),
        );
        assert_eq!(
            watch.update(&model(Model::default()), &mut hints, start()),
            Some(ui!(keys::HUD_UNLOCKED))
        );
    }

    /// The sample with a Mac mini in `state` and `wake`, from a service
    /// that wakes devices.
    fn mini(state: ConnectionState, wake: WakeState) -> Snapshot {
        let mut m = model(Model::default());
        m.status.wake_supported = true;
        m.status.devices.push(DeviceStatus {
            id: "mini".into(),
            name: "Mac mini".into(),
            state: state as i32,
            can_wake: state != ConnectionState::Connected,
            wake: wake as i32,
            ..Default::default()
        });
        m
    }

    const ASLEEP: ConnectionState = ConnectionState::Offline;
    const AWAKE: ConnectionState = ConnectionState::Connected;

    #[test]
    fn says_a_computer_is_being_woken_and_when_it_woke_up() {
        let mut watch = HudWatch::default();
        let mut hints = 3;
        assert_eq!(
            watch.update(&mini(ASLEEP, WakeState::None), &mut hints, start()),
            None
        );
        let waking = Some(ui!(keys::HUD_WAKING, "Mac mini"));
        assert_eq!(
            watch.update(&mini(ASLEEP, WakeState::Waking), &mut hints, start()),
            waking
        );
        // Still being woken: said once.
        assert_eq!(
            watch.update(&mini(ASLEEP, WakeState::Waking), &mut hints, start()),
            None
        );
        let awake = Some(ui!(keys::HUD_AWAKE, "Mac mini"));
        assert_eq!(
            watch.update(&mini(AWAKE, WakeState::None), &mut hints, start()),
            awake
        );
        assert_eq!(
            watch.update(&mini(AWAKE, WakeState::None), &mut hints, start()),
            None
        );
        // Asleep again, woken again: said again.
        watch.update(&mini(ASLEEP, WakeState::None), &mut hints, start());
        assert_eq!(
            watch.update(&mini(ASLEEP, WakeState::Waking), &mut hints, start()),
            waking
        );
        // It did not wake up: no note (the inspector says so); woken again
        // from there, said again.
        assert_eq!(
            watch.update(&mini(ASLEEP, WakeState::NotWoken), &mut hints, start()),
            None
        );
        assert_eq!(
            watch.update(&mini(ASLEEP, WakeState::Waking), &mut hints, start()),
            waking
        );
        // Connecting after it was given up on is not waking up.
        watch.update(&mini(ASLEEP, WakeState::NotWoken), &mut hints, start());
        assert_eq!(
            watch.update(&mini(AWAKE, WakeState::None), &mut hints, start()),
            None
        );
        // In both languages.
        let zh = crate::text::Catalog::for_language(crate::text::Language::ChineseTaiwan);
        assert_eq!(
            waking.map(|t| (t.to_string(), zh.render(&t))),
            Some(("Waking Mac mini…".into(), "正在喚醒「Mac mini」…".into()))
        );
        assert_eq!(
            awake.map(|t| (t.to_string(), zh.render(&t))),
            Some(("Mac mini is awake.".into(), "「Mac mini」醒來了。".into()))
        );
    }

    #[test]
    fn waking_already_under_way_is_only_remembered() {
        // When the app starts.
        let mut watch = HudWatch::default();
        let mut hints = 3;
        assert_eq!(
            watch.update(&mini(ASLEEP, WakeState::Waking), &mut hints, start()),
            None
        );
        assert_eq!(
            watch.update(&mini(AWAKE, WakeState::None), &mut hints, start()),
            Some(ui!(keys::HUD_AWAKE, "Mac mini"))
        );
        // When the service is reached again.
        watch.update(&mini(ASLEEP, WakeState::None), &mut hints, start());
        let mut away = mini(ASLEEP, WakeState::None);
        away.link = Link::Connecting;
        assert_eq!(watch.update(&away, &mut hints, start()), None);
        assert_eq!(
            watch.update(&mini(ASLEEP, WakeState::Waking), &mut hints, start()),
            None
        );
        assert_eq!(
            watch.update(&mini(ASLEEP, WakeState::Waking), &mut hints, start()),
            None
        );
        assert_eq!(
            watch.update(&mini(AWAKE, WakeState::None), &mut hints, start()),
            Some(ui!(keys::HUD_AWAKE, "Mac mini"))
        );
    }

    #[test]
    fn a_device_gone_from_the_list_is_forgotten() {
        let mut watch = HudWatch::default();
        let mut hints = 3;
        watch.update(&mini(ASLEEP, WakeState::None), &mut hints, start());
        watch.update(&mini(ASLEEP, WakeState::Waking), &mut hints, start());
        // Unpaired while being woken, paired again: it connecting is not
        // waking up.
        watch.update(&model(Model::default()), &mut hints, start());
        assert_eq!(
            watch.update(&mini(AWAKE, WakeState::None), &mut hints, start()),
            None
        );
    }

    #[test]
    fn nothing_of_waking_from_a_service_before_it() {
        let old = |state, wake| {
            let mut m = mini(state, wake);
            m.status.wake_supported = false;
            m
        };
        let mut watch = HudWatch::default();
        let mut hints = 3;
        watch.update(&old(ASLEEP, WakeState::None), &mut hints, start());
        assert_eq!(
            watch.update(&old(ASLEEP, WakeState::Waking), &mut hints, start()),
            None
        );
        assert_eq!(
            watch.update(&old(AWAKE, WakeState::None), &mut hints, start()),
            None
        );
    }

    #[test]
    fn the_keyboard_and_mouse_coming_back_comes_first() {
        let mut watch = HudWatch::default();
        let mut hints = 3;
        watch.update(&mini(ASLEEP, WakeState::None), &mut hints, start());
        let mut both = mini(ASLEEP, WakeState::Waking);
        both.status.control_returns = 1;
        both.status.last_return = ReturnReason::Hotkey as i32;
        assert_eq!(
            watch.update(&both, &mut hints, start()),
            Some(ui!(keys::HUD_BACK_HERE))
        );
        // The waking was seen all the same: not said later.
        assert_eq!(watch.update(&both, &mut hints, start()), None);
        // Before the take-back hint, which waits for another time.
        let mut hints = 0;
        let mut watch = HudWatch::default();
        watch.update(&mini(ASLEEP, WakeState::None), &mut hints, start());
        let mut there = mini(ASLEEP, WakeState::Waking);
        there.status.active_device_id = "mac".into();
        assert_eq!(
            watch.update(&there, &mut hints, start()),
            Some(ui!(keys::HUD_WAKING, "Mac mini"))
        );
        assert_eq!(hints, 0);
    }

    #[test]
    fn of_several_at_once_the_first_in_the_status_is_named() {
        let both = |wake: WakeState, mini_first: bool| {
            let mut m = mini(ASLEEP, wake);
            let mut pc = m.status.devices.last().cloned().unwrap();
            pc.id = "pc".into();
            pc.name = "Studio PC".into();
            if mini_first {
                m.status.devices.push(pc);
            } else {
                let at = m.status.devices.len() - 1;
                m.status.devices.insert(at, pc);
            }
            m
        };
        for (mini_first, named) in [(true, "Mac mini"), (false, "Studio PC")] {
            let mut watch = HudWatch::default();
            let mut hints = 3;
            watch.update(&both(WakeState::None, mini_first), &mut hints, start());
            assert_eq!(
                watch.update(&both(WakeState::Waking, mini_first), &mut hints, start()),
                Some(ui!(keys::HUD_WAKING, named)),
                "{named}"
            );
        }
    }
}
