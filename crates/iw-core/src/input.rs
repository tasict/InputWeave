//! Input events as captured on one machine and replayed on another.

use crate::HidUsage;
use crate::geometry::Point;
use crate::hotkey::HeldHotkeys;

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
    /// Control was pressed twice quickly (`Hotkeys::take_back_double`), as
    /// told by capture that reported none of those keys: X11 while another
    /// app holds a grab, when the presses went to that app. Control comes
    /// back to this device if the double press is on; nothing goes to the
    /// device the cursor is on.
    DoublePressed,
    /// Capture holds back these of the reserved hotkeys from now on, as
    /// told by a backend that can lose some or get them back (X11, as
    /// another app grabs or lets go of one, the connection breaks or
    /// injection lets go of them meanwhile): in order with the keys, so a
    /// key that goes down after it is held back or not as it says.
    HotkeysHeldBack(HeldHotkeys),
}
