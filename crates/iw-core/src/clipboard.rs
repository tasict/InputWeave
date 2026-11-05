//! Which clipboard goes where as the cursor moves between devices.
//!
//! The device the cursor leaves hands its clipboard, if it changed, to the
//! device the cursor enters (docs/architecture.md §8). Only the
//! controller sees every move, so it decides: it reads its own clipboard,
//! or asks a remote device for its own.

use crate::layout::DeviceId;
use crate::router::Action;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardMove {
    /// Send this device's clipboard to `to`.
    Push { to: DeviceId },
    /// Ask `from` for its clipboard, to be delivered to `to`, which may be
    /// this device.
    Fetch { from: DeviceId, to: DeviceId },
}

/// Follows the cursor through the router's actions.
#[derive(Debug)]
pub struct ClipboardRelay {
    local: DeviceId,
    cursor: DeviceId,
}

impl ClipboardRelay {
    pub fn new(local: DeviceId) -> Self {
        Self {
            cursor: local.clone(),
            local,
        }
    }

    /// The clipboard moves that `actions` call for, in order.
    pub fn follow(&mut self, actions: &[Action]) -> Vec<ClipboardMove> {
        let mut moves = Vec::new();
        for action in actions {
            let to = match action {
                Action::Enter { device, .. } => device.clone(),
                Action::Ungrab { .. } => self.local.clone(),
                _ => continue,
            };
            let from = std::mem::replace(&mut self.cursor, to.clone());
            if from == to {
                continue;
            }
            moves.push(if from == self.local {
                ClipboardMove::Push { to }
            } else {
                ClipboardMove::Fetch { from, to }
            });
        }
        moves
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Point;
    use crate::test_support::id;

    fn enter(device: &str) -> Action {
        Action::Enter {
            device: id(device),
            position: Point::default(),
            modifiers: vec![],
            buttons: vec![],
        }
    }

    fn back() -> Action {
        Action::Ungrab {
            warp_to: Point::default(),
        }
    }

    #[test]
    fn leaving_this_device_pushes_its_clipboard() {
        let mut relay = ClipboardRelay::new(id("mac"));
        assert_eq!(
            relay.follow(&[Action::Grab, enter("win")]),
            vec![ClipboardMove::Push { to: id("win") }]
        );
    }

    #[test]
    fn hopping_and_returning_fetch_from_the_device_left() {
        let mut relay = ClipboardRelay::new(id("mac"));
        relay.follow(&[Action::Grab, enter("win")]);
        assert_eq!(
            relay.follow(&[Action::Leave { device: id("win") }, enter("mini")]),
            vec![ClipboardMove::Fetch {
                from: id("win"),
                to: id("mini")
            }]
        );
        assert_eq!(
            relay.follow(&[Action::Leave { device: id("mini") }, back()]),
            vec![ClipboardMove::Fetch {
                from: id("mini"),
                to: id("mac")
            }]
        );
        // Motion and keys move nothing.
        assert!(relay.follow(&[Action::Grab]).is_empty());
    }

    #[test]
    fn returning_without_having_left_moves_nothing() {
        let mut relay = ClipboardRelay::new(id("mac"));
        assert!(relay.follow(&[back()]).is_empty());
    }
}
