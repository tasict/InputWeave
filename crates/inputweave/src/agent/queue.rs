//! The order outgoing files go in (docs/architecture.md §9): at most
//! [`MAX_ACTIVE`] files go to a device at once, and the rest wait their turn
//! in the order they were sent.
//!
//! A file keeps its place from the moment it is given a turn until it ends,
//! through hashing, waiting for the connection or for the other user's
//! answer, sending and verifying. So while a device is offline, the files
//! that had begun keep their places and the rest wait behind them; once it
//! is back, they go on in the order they were sent. Files to different
//! devices never wait for each other.
//!
//! This type only keeps the order; `files` starts the transfers it names.

use std::collections::{HashMap, VecDeque};

/// Files going to one device at once. Two keep the link busy while one of
/// them is between files (hashing the next, waiting for the answer to its
/// offer), and leave each file a fair share of it. It also keeps what file
/// streams can hold unsent well below what quinn holds for a connection
/// in all (`send_window`, 8 times a stream's flow-control window), so their
/// data never keeps keys and buttons from being written.
pub(crate) const MAX_ACTIVE: usize = 2;

#[derive(Debug, Default)]
struct Lane {
    /// Transfers that have their turn, in the order they got it.
    active: Vec<String>,
    /// Transfers waiting for a turn, first sent first.
    waiting: VecDeque<String>,
}

/// Outgoing transfers per device, by id.
#[derive(Debug, Default)]
pub(crate) struct SendQueue {
    lanes: HashMap<String, Lane>,
}

impl SendQueue {
    /// Adds a transfer to `device`'s queue. Returns whether it goes at
    /// once; otherwise it waits behind the others.
    pub(crate) fn push(&mut self, device: &str, id: &str) -> bool {
        let lane = self.lanes.entry(device.to_owned()).or_default();
        if lane.active.len() < MAX_ACTIVE && lane.waiting.is_empty() {
            lane.active.push(id.to_owned());
            true
        } else {
            lane.waiting.push_back(id.to_owned());
            false
        }
    }

    /// Takes a transfer out of `device`'s queue, whether it had its turn
    /// or was still waiting, as when it ends or is cancelled. Returns the
    /// transfers that go now in its place, in order.
    pub(crate) fn remove(&mut self, device: &str, id: &str) -> Vec<String> {
        let Some(lane) = self.lanes.get_mut(device) else {
            return Vec::new();
        };
        lane.active.retain(|a| a != id);
        lane.waiting.retain(|w| w != id);
        let mut started = Vec::new();
        while lane.active.len() < MAX_ACTIVE {
            let Some(next) = lane.waiting.pop_front() else {
                break;
            };
            lane.active.push(next.clone());
            started.push(next);
        }
        if lane.active.is_empty() && lane.waiting.is_empty() {
            self.lanes.remove(device);
        }
        started
    }

    /// The transfers that have their turn with `device`.
    #[cfg(test)]
    fn active(&self, device: &str) -> Vec<&str> {
        self.lanes.get(device).map_or_else(Vec::new, |lane| {
            lane.active.iter().map(String::as_str).collect()
        })
    }

    /// The transfers waiting for a turn with `device`, first in line first.
    #[cfg(test)]
    fn waiting(&self, device: &str) -> Vec<&str> {
        self.lanes.get(device).map_or_else(Vec::new, |lane| {
            lane.waiting.iter().map(String::as_str).collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued(queue: &mut SendQueue, device: &str, ids: &[&str]) -> Vec<bool> {
        ids.iter().map(|id| queue.push(device, id)).collect()
    }

    #[test]
    fn two_files_go_at_once_and_the_rest_wait_in_order() {
        let mut queue = SendQueue::default();
        assert_eq!(
            queued(&mut queue, "pc", &["1", "2", "3", "4", "5"]),
            [true, true, false, false, false]
        );
        assert_eq!(queue.active("pc"), ["1", "2"]);
        assert_eq!(queue.waiting("pc"), ["3", "4", "5"]);

        // Whichever ends first, the next in line takes its place.
        assert_eq!(queue.remove("pc", "2"), ["3"]);
        assert_eq!(queue.remove("pc", "1"), ["4"]);
        assert_eq!(queue.active("pc"), ["3", "4"]);
        assert_eq!(queue.remove("pc", "4"), ["5"]);
        assert_eq!(queue.remove("pc", "3"), Vec::<String>::new());
        assert_eq!(queue.remove("pc", "5"), Vec::<String>::new());
        assert!(queue.lanes.is_empty());
    }

    #[test]
    fn a_file_sent_later_waits_behind_the_queue() {
        let mut queue = SendQueue::default();
        queued(&mut queue, "pc", &["1", "2", "3"]);
        queue.remove("pc", "1");
        // A slot is free only until the waiting file takes it.
        assert_eq!(queue.active("pc"), ["2", "3"]);
        assert!(!queue.push("pc", "4"));
        assert_eq!(queue.remove("pc", "2"), ["4"]);
    }

    #[test]
    fn devices_have_queues_of_their_own() {
        let mut queue = SendQueue::default();
        assert_eq!(
            queued(&mut queue, "pc", &["1", "2", "3"]),
            [true, true, false]
        );
        assert_eq!(queued(&mut queue, "mini", &["4", "5"]), [true, true]);
        assert_eq!(queue.remove("mini", "4"), Vec::<String>::new());
        assert_eq!(queue.waiting("pc"), ["3"]);
    }

    #[test]
    fn a_waiting_file_cancelled_leaves_the_line() {
        let mut queue = SendQueue::default();
        queued(&mut queue, "pc", &["1", "2", "3", "4", "5"]);
        // Cancelling a waiting file starts nothing: no place was freed.
        assert_eq!(queue.remove("pc", "4"), Vec::<String>::new());
        assert_eq!(queue.waiting("pc"), ["3", "5"]);
        assert_eq!(queue.remove("pc", "1"), ["3"]);
        assert_eq!(queue.remove("pc", "2"), ["5"]);
        // Nor does removing what is no longer there.
        assert_eq!(queue.remove("pc", "4"), Vec::<String>::new());
        assert_eq!(queue.remove("ghost", "1"), Vec::<String>::new());
    }

    #[test]
    fn files_that_began_keep_their_places_while_the_device_is_offline() {
        let mut queue = SendQueue::default();
        // Sent while the device is offline: the first two begin (they hash
        // and then wait for the connection), the rest wait behind them,
        // and so does a file sent later while it is still offline.
        queued(&mut queue, "pc", &["1", "2", "3", "4"]);
        assert!(!queue.push("pc", "5"));
        assert_eq!(queue.active("pc"), ["1", "2"]);
        assert_eq!(queue.waiting("pc"), ["3", "4", "5"]);
        // Back online, the two that began finish first, and the rest
        // follow in the order they were sent.
        assert_eq!(queue.remove("pc", "2"), ["3"]);
        assert_eq!(queue.remove("pc", "1"), ["4"]);
        assert_eq!(queue.remove("pc", "3"), ["5"]);
    }
}
