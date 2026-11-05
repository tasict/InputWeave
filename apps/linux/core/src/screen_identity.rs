//! The parts of identifying the screens that need
//! no window, as the Windows app's `ScreenIdentity`: which monitor is which
//! of this computer's screens, and for how long the names stay. A monitor
//! is matched by its connector name (`DP-1`), which is how the agent names
//! displays on X11 (RandR) and Wayland (`wl_output.name`).

use std::time::Duration;

use iw_proto::app::v1::Screen;

use crate::device_text;
use crate::snapshot::Snapshot;

/// What the service asks for, should it not say.
pub const DEFAULT_DURATION_MS: u32 = 2500;

/// How long the names stay for what the service asked.
pub fn shown_for(duration_ms: u32) -> Duration {
    Duration::from_millis(u64::from(if duration_ms == 0 {
        DEFAULT_DURATION_MS
    } else {
        duration_ms
    }))
}

/// A monitor as GDK has it, in logical units, with its scale.
#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    pub connector: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    /// The main one: the arrangement's main screen, or X's primary output.
    pub primary: bool,
}

/// What one monitor shows: which of this computer's screens it is,
/// counted from 1, and its size in pixels, such as "3840 × 2160".
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub number: usize,
    pub size: String,
}

/// A label for each of `monitors`, in their order, as the Windows app's
/// `ScreenIdentity.Labels`: the screen of this computer's in the
/// arrangement of the same name, its place among them and its size as the
/// inspector shows it. A monitor the arrangement does not name, as before
/// the service has reported the displays, is numbered by its place among
/// such monitors, the main one first and the others from left to right,
/// then top to bottom, with its own size; when the arrangement names
/// others, after all of its screens, so no two monitors get one number.
pub fn labels(arranged: &[Screen], monitors: &[Monitor]) -> Vec<Label> {
    let place = |m: &Monitor| {
        arranged
            .iter()
            .position(|s| !s.id.is_empty() && s.id.eq_ignore_ascii_case(&m.connector))
    };
    let mut unnamed: Vec<usize> = (0..monitors.len())
        .filter(|&i| place(&monitors[i]).is_none())
        .collect();
    unnamed.sort_by_key(|&i| (!monitors[i].primary, monitors[i].x, monitors[i].y));
    let base = if unnamed.len() < monitors.len() {
        arranged.len()
    } else {
        0
    };
    monitors
        .iter()
        .enumerate()
        .map(|(i, monitor)| match place(monitor) {
            Some(index) => Label {
                number: index + 1,
                size: device_text::resolution(&arranged[index]),
            },
            None => Label {
                number: base + unnamed.iter().position(|&u| u == i).unwrap_or(0) + 1,
                size: format!(
                    "{} × {}",
                    (f64::from(monitor.width) * monitor.scale).round(),
                    (f64::from(monitor.height) * monitor.scale).round()
                ),
            },
        })
        .collect()
}

/// Whether the arrangement says the monitor named `connector` is this
/// computer's main screen.
pub fn arranged_primary(arranged: &[Screen], connector: &str) -> bool {
    arranged
        .iter()
        .any(|s| s.primary && !s.id.is_empty() && s.id.eq_ignore_ascii_case(connector))
}

/// This computer's screens as the arrangement has them, for [`labels`].
pub fn this_computers_screens(snapshot: &Snapshot) -> &[Screen] {
    snapshot
        .arrangement
        .devices
        .iter()
        .find(|d| d.this_device)
        .map_or(&[], |d| d.screens.as_slice())
}

/// The jump to this computer, for a line under the screen's size; `None`
/// when it has no number or the jumps are off.
pub fn jump(snapshot: &Snapshot) -> Option<String> {
    let this = snapshot
        .arrangement
        .devices
        .iter()
        .find(|d| d.this_device)?;
    snapshot.jump_shortcut(this.jump_slot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iw_proto::app::v1::{ArrangedDevice, Arrangement};
    use std::sync::Arc;

    fn arranged(id: &str, width: f64, height: f64, scale: f64) -> Screen {
        Screen {
            id: id.into(),
            width,
            height,
            scale,
            ..Default::default()
        }
    }

    fn monitor(connector: &str, x: i32, width: i32, height: i32, scale: f64) -> Monitor {
        Monitor {
            connector: connector.into(),
            x,
            y: 0,
            width,
            height,
            scale,
            primary: false,
        }
    }

    #[test]
    fn numbers_monitors_as_the_arrangement_lists_them() {
        let screens = [
            arranged("HDMI-1", 1920.0, 1080.0, 1.0),
            arranged("DP-1", 2560.0, 1440.0, 1.5),
        ];
        let monitors = [
            monitor("DP-1", 0, 2560, 1440, 1.5),
            monitor("hdmi-1", 2560, 1920, 1080, 1.0),
        ];
        assert_eq!(
            labels(&screens, &monitors),
            [
                Label {
                    number: 2,
                    size: "3840 × 2160".into()
                },
                Label {
                    number: 1,
                    size: "1920 × 1080".into()
                },
            ]
        );
    }

    #[test]
    fn without_an_arrangement_the_main_one_comes_first() {
        let mut main = monitor("DP-1", 0, 2560, 1440, 1.0);
        main.primary = true;
        let monitors = [
            monitor("DP-3", 2560, 1920, 1080, 1.0),
            main,
            monitor("DP-2", -1920, 1920, 1080, 2.0),
        ];
        let numbers: Vec<(usize, String)> = labels(&[], &monitors)
            .into_iter()
            .map(|l| (l.number, l.size))
            .collect();
        assert_eq!(
            numbers,
            [
                (3, "1920 × 1080".to_owned()),
                (1, "2560 × 1440".to_owned()),
                (2, "3840 × 2160".to_owned())
            ]
        );
    }

    #[test]
    fn monitors_the_arrangement_does_not_name_come_after_its_screens() {
        // The arrangement knows DP-1 only; HDMI-1, left of it, is new.
        let screens = [arranged("DP-1", 2560.0, 1440.0, 1.0)];
        let monitors = [
            monitor("HDMI-1", -1920, 1920, 1080, 1.0),
            monitor("DP-1", 0, 2560, 1440, 1.0),
        ];
        let numbers: Vec<usize> = labels(&screens, &monitors)
            .into_iter()
            .map(|l| l.number)
            .collect();
        assert_eq!(numbers, [2, 1], "no two monitors share a number");
        assert!(arranged_primary(
            &[Screen {
                primary: true,
                ..arranged("DP-1", 1.0, 1.0, 1.0)
            }],
            "dp-1"
        ));
        assert!(!arranged_primary(&screens, "DP-1"));
    }

    #[test]
    fn stays_as_long_as_the_service_says() {
        assert_eq!(shown_for(0), Duration::from_millis(2500));
        assert_eq!(shown_for(4000), Duration::from_secs(4));
    }

    #[test]
    fn writes_this_computers_jump() {
        let mut snapshot = Snapshot::default();
        assert_eq!(jump(&snapshot), None);
        snapshot.arrangement = Arc::new(Arrangement {
            version: 1,
            devices: vec![ArrangedDevice {
                id: "me".into(),
                this_device: true,
                jump_slot: 2,
                ..Default::default()
            }],
        });
        assert_eq!(jump(&snapshot).as_deref(), Some("Ctrl+Alt+Shift+2"));
        assert!(this_computers_screens(&snapshot).is_empty());
    }
}
