//! When to notify about files coming here, as the Windows app's
//! `TransferNews`: ask about files waiting for an answer, take the question
//! back once answered elsewhere, and once a computer's files stopped
//! coming for a moment, announce what arrived in one notification.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use iw_proto::app::v1::{FileTransfer, TransferState};

use crate::snapshot::Snapshot;
use crate::transfer_text::is_finished;

/// What came from one computer together.
#[derive(Clone, Debug, PartialEq)]
pub struct ReceivedFiles {
    pub device_id: String,
    pub files: Vec<FileTransfer>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TransferUpdate {
    /// Files waiting for the user's answer.
    pub ask: Vec<FileTransfer>,
    /// Transfer ids whose question is no longer open.
    pub withdraw: Vec<String>,
    pub announce: Vec<ReceivedFiles>,
}

impl TransferUpdate {
    pub fn is_empty(&self) -> bool {
        self.ask.is_empty() && self.withdraw.is_empty() && self.announce.is_empty()
    }
}

#[derive(Debug, Default)]
pub struct TransferNews {
    states: HashMap<String, TransferState>,
    arrived: HashMap<String, (Vec<FileTransfer>, SystemTime)>,
    primed: bool,
}

impl TransferNews {
    /// Files arriving close together go in one notification.
    pub const SETTLE: Duration = Duration::from_millis(1500);

    pub fn update(&mut self, snapshot: &Snapshot, now: SystemTime) -> TransferUpdate {
        let mut update = TransferUpdate::default();
        // Drags and pastes go where the user put them, with no news.
        let incoming: Option<Vec<&FileTransfer>> = snapshot.connected().then(|| {
            snapshot
                .transfers
                .transfers
                .iter()
                .filter(|t| !t.outgoing && t.drag_id.is_empty() && !t.clipboard)
                .collect()
        });
        if let Some(incoming) = &incoming {
            if !self.primed {
                // What happened before the app connected is no news, but a
                // question still open is.
                self.primed = true;
                for transfer in incoming {
                    self.states
                        .insert(transfer.transfer_id.clone(), transfer.state());
                    if transfer.state() == TransferState::WaitingForAnswer {
                        update.ask.push((*transfer).clone());
                    }
                }
                return update;
            }
            for transfer in incoming {
                let before = self
                    .states
                    .insert(transfer.transfer_id.clone(), transfer.state());
                if before == Some(transfer.state()) {
                    continue;
                }
                if before == Some(TransferState::WaitingForAnswer) {
                    update.withdraw.push(transfer.transfer_id.clone());
                }
                match transfer.state() {
                    TransferState::WaitingForAnswer => update.ask.push((*transfer).clone()),
                    TransferState::Done | TransferState::Failed => {
                        let group = self
                            .arrived
                            .entry(transfer.device_id.clone())
                            .or_insert_with(|| (Vec::new(), now));
                        group.0.push((*transfer).clone());
                        group.1 = now;
                    }
                    _ => {}
                }
            }
            // Forget transfers the service no longer lists.
            let listed: Vec<&str> = incoming.iter().map(|t| t.transfer_id.as_str()).collect();
            let gone: Vec<String> = self
                .states
                .keys()
                .filter(|id| !listed.contains(&id.as_str()))
                .cloned()
                .collect();
            for id in gone {
                if self.states.remove(&id) == Some(TransferState::WaitingForAnswer) {
                    update.withdraw.push(id);
                }
            }
        }
        let devices: Vec<String> = self.arrived.keys().cloned().collect();
        for device in devices {
            let coming = incoming.as_ref().is_some_and(|incoming| {
                incoming
                    .iter()
                    .any(|t| t.device_id == device && !is_finished(t.state()))
            });
            let last = self.arrived[&device].1;
            if !coming && now.duration_since(last).unwrap_or_default() >= Self::SETTLE {
                let (files, _) = self.arrived.remove(&device).expect("listed");
                if files.iter().any(|f| f.state() == TransferState::Done) {
                    update.announce.push(ReceivedFiles {
                        device_id: device,
                        files,
                    });
                }
            }
        }
        update
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Link;
    use std::sync::Arc;

    fn at(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(1_000_000 + ms)
    }

    fn snapshot(transfers: Vec<FileTransfer>) -> Snapshot {
        let mut s = Snapshot {
            link: Link::Connected,
            ..Default::default()
        };
        Arc::make_mut(&mut s.transfers).transfers = transfers;
        s
    }

    fn incoming(id: &str, state: TransferState) -> FileTransfer {
        FileTransfer {
            transfer_id: id.into(),
            device_id: "mac".into(),
            state: state as i32,
            ..Default::default()
        }
    }

    #[test]
    fn the_first_look_only_asks_what_is_still_open() {
        let mut news = TransferNews::default();
        let update = news.update(
            &snapshot(vec![
                incoming("old", TransferState::Done),
                incoming("open", TransferState::WaitingForAnswer),
            ]),
            at(0),
        );
        assert_eq!(update.ask.len(), 1);
        assert!(update.announce.is_empty());
    }

    #[test]
    fn arrivals_settle_into_one_notification() {
        let mut news = TransferNews::default();
        news.update(&snapshot(vec![]), at(0));
        let going = vec![
            incoming("a", TransferState::Done),
            incoming("b", TransferState::Transferring),
        ];
        assert!(news.update(&snapshot(going), at(100)).is_empty());
        let done = vec![
            incoming("a", TransferState::Done),
            incoming("b", TransferState::Done),
        ];
        assert!(news.update(&snapshot(done.clone()), at(200)).is_empty());
        assert!(
            news.update(&snapshot(done.clone()), at(1600)).is_empty(),
            "not settled"
        );
        let update = news.update(&snapshot(done), at(1700));
        assert_eq!(update.announce.len(), 1);
        assert_eq!(update.announce[0].files.len(), 2);
    }

    #[test]
    fn answered_questions_are_taken_back() {
        let mut news = TransferNews::default();
        news.update(&snapshot(vec![]), at(0));
        let asked = news.update(
            &snapshot(vec![incoming("q", TransferState::WaitingForAnswer)]),
            at(10),
        );
        assert_eq!(asked.ask.len(), 1);
        let answered = news.update(
            &snapshot(vec![incoming("q", TransferState::Transferring)]),
            at(20),
        );
        assert_eq!(answered.withdraw, ["q"]);
        news.update(
            &snapshot(vec![incoming("r", TransferState::WaitingForAnswer)]),
            at(30),
        );
        let gone = news.update(&snapshot(vec![]), at(40));
        assert!(gone.withdraw.contains(&"r".to_owned()));
    }

    #[test]
    fn drags_pastes_and_failures_alone_are_no_news() {
        let mut news = TransferNews::default();
        news.update(&snapshot(vec![]), at(0));
        let mut dragged = incoming("d", TransferState::Done);
        dragged.drag_id = "drag".into();
        let failed = incoming("f", TransferState::Failed);
        news.update(&snapshot(vec![dragged.clone(), failed.clone()]), at(10));
        assert!(
            news.update(&snapshot(vec![dragged, failed]), at(5000))
                .is_empty()
        );
    }
}
