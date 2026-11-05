//! Input events as captured on one machine and replayed on another.

use crate::HidUsage;
use crate::geometry::Point;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Button {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// An event reported by local input capture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputEvent {
    /// The pointer moved by `delta`. `position` is the local cursor position
    /// in this machine's desktop coordinates; while input is grabbed it is
    /// wherever capture holds the hidden cursor.
    PointerMotion {
        position: Point,
        delta: Point,
    },
    Button {
        button: Button,
        pressed: bool,
    },
    Key {
        usage: HidUsage,
        pressed: bool,
    },
    /// Wheel movement in notches, in the direction the wheel physically
    /// turned: positive `dy` is away from the user, positive `dx` to the
    /// right. Precise devices such as trackpads report fractions. Platform
    /// settings like macOS natural scrolling are undone on capture and
    /// applied again on injection.
    Scroll {
        dx: f64,
        dy: f64,
    },
    /// Capture no longer holds the input it was handed (Wayland's
    /// InputCapture portal): the desktop took it back while grabbed, or
    /// capture let go of it at an edge the cursor did not cross. What comes
    /// next reaches local apps unseen, so keys and buttons reported down are
    /// reported released first; the cursor no longer pushes against an
    /// edge, and control comes back to this device.
    CaptureEnded,
}
