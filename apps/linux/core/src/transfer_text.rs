//! The words for file transfers, as the other two
//! apps write them (the Windows app's `TransferText`).

use std::collections::{HashMap, HashSet};

use iw_proto::app::v1::{FileTransfer, TransferState};

use crate::size;
use crate::text::{UiText, keys};
use crate::ui;

/// Whether the transfer has ended, one way or another.
pub fn is_finished(state: TransferState) -> bool {
    matches!(
        state,
        TransferState::Done
            | TransferState::Declined
            | TransferState::Failed
            | TransferState::Cancelled
    )
}

/// "Report.pdf → Mac mini", or "←" for one coming here.
pub fn title(transfer: &FileTransfer, device: &str) -> String {
    let arrow = if transfer.outgoing { "→" } else { "←" };
    format!("{} {arrow} {device}", transfer.name)
}

/// A folder's files and empty folders; 1 for a file.
pub fn files_in(transfer: &FileTransfer) -> u32 {
    if transfer.folder {
        transfer.files
    } else {
        transfer.files.max(1)
    }
}

/// A folder's line under its name: "312 files · 1.2 GB"; "8 of 312
/// files" once some arrived; "310 sent, 2 failed" once it ended with some
/// that did not. `None` for a file.
pub fn folder_line(transfer: &FileTransfer) -> Option<UiText> {
    if !transfer.folder {
        return None;
    }
    if folder_line_is_problem(transfer) {
        let key = if transfer.outgoing {
            keys::TRANSFER_SENT_SOME
        } else {
            keys::TRANSFER_RECEIVED_SOME
        };
        return Some(ui!(
            key,
            transfer.files_done.to_string(),
            transfer.files_failed.to_string()
        ));
    }
    if !is_finished(transfer.state()) && transfer.files_done > 0 {
        return Some(ui!(
            keys::TRANSFER_FOLDER_PROGRESS,
            transfer.files_done.to_string(),
            transfer.files.to_string()
        ));
    }
    Some(file_count(transfer.files, transfer.size))
}

/// "312 files · 1.2 GB".
pub fn file_count(files: u32, bytes: u64) -> UiText {
    if files == 1 {
        ui!(keys::TRANSFER_FOLDER_FILES_ONE, size::format(bytes))
    } else {
        ui!(
            keys::TRANSFER_FOLDER_FILES,
            files.to_string(),
            size::format(bytes)
        )
    }
}

pub fn folder_line_is_problem(transfer: &FileTransfer) -> bool {
    transfer.folder
        && transfer.files_failed > 0
        && matches!(
            transfer.state(),
            TransferState::Done | TransferState::Failed
        )
}

/// "Photos/raw/IMG_0001.CR3: The file could not be read.", for each
/// failure the service lists (the first 100).
pub fn failure_lines(transfer: &FileTransfer) -> Vec<UiText> {
    transfer
        .failures
        .iter()
        .map(|f| {
            ui!(
                keys::TRANSFER_FAILURE_LINE,
                f.relative_path.clone(),
                failure_reason(&f.reason, transfer.outgoing)
            )
        })
        .collect()
}

/// The failures beyond those listed, for "12 more".
pub fn unlisted_failures(transfer: &FileTransfer) -> u32 {
    transfer
        .files_failed
        .saturating_sub(transfer.failures.len() as u32)
}

/// Why one file of a folder did not make it.
pub fn failure_reason(reason: &str, outgoing: bool) -> UiText {
    match reason {
        "too_large" if outgoing => ui!(keys::TRANSFER_PEER_TOO_LARGE),
        "too_large" => ui!(keys::TRANSFER_TOO_MANY),
        "invalid_path" if outgoing => ui!(keys::TRANSFER_PEER_INVALID_NAME),
        "invalid_path" => ui!(keys::TRANSFER_INVALID_NAME),
        "no_space" => ui!(keys::TRANSFER_NO_SPACE_SHORT),
        "rejected" => ui!(keys::TRANSFER_DECLINED),
        other => failed(other),
    }
}

/// How far along it is, while that is worth a progress bar.
pub fn fraction(transfer: &FileTransfer) -> Option<f64> {
    if transfer.size == 0 {
        return None;
    }
    match transfer.state() {
        TransferState::Transferring | TransferState::Verifying => {}
        // Cut off midway: the bar stays where it will resume.
        TransferState::WaitingForConnection if transfer.done_bytes > 0 => {}
        _ => return None,
    }
    Some((transfer.done_bytes as f64 / transfer.size as f64).min(1.0))
}

/// The line under the name: progress, what it waits for, or how it ended.
pub fn detail(transfer: &FileTransfer, device: &str) -> Option<UiText> {
    Some(match transfer.state() {
        TransferState::Preparing => ui!(keys::TRANSFER_PREPARING),
        TransferState::Queued => ui!(keys::TRANSFER_QUEUED),
        TransferState::WaitingForConnection => ui!(keys::TRANSFER_WAITING_FOR_CONNECTION),
        TransferState::WaitingForAnswer if transfer.outgoing => {
            ui!(keys::TRANSFER_WAITING_FOR_ACCEPT, device)
        }
        TransferState::WaitingForAnswer => {
            ui!(keys::TRANSFER_OFFER, device, size::format(transfer.size))
        }
        TransferState::Transferring => progress(transfer),
        TransferState::Verifying => ui!(keys::TRANSFER_CHECKING),
        // A folder some of whose files did not make it says so on its own line.
        TransferState::Done if folder_line_is_problem(transfer) => return None,
        TransferState::Done if transfer.outgoing => ui!(keys::TRANSFER_SENT),
        TransferState::Done => ui!(keys::TRANSFER_RECEIVED),
        TransferState::Declined => declined(transfer, device),
        TransferState::Failed => failed(&transfer.reason),
        TransferState::Cancelled => ui!(keys::TRANSFER_CANCELLED),
        TransferState::Unspecified => return None,
    })
}

/// "64% · 12.0 MB/s · 2 min left", or the percentage alone before the
/// speed is known.
pub fn progress(transfer: &FileTransfer) -> UiText {
    let percent = percent(fraction(transfer).unwrap_or(0.0));
    if transfer.bytes_per_second == 0 {
        return UiText::verbatim(percent);
    }
    let left = transfer
        .size
        .saturating_sub(transfer.done_bytes.min(transfer.size)) as f64
        / transfer.bytes_per_second as f64;
    ui!(
        keys::TRANSFER_PROGRESS,
        percent,
        size::format(transfer.bytes_per_second),
        remaining(left)
    )
}

/// A whole percentage, "64%".
pub fn percent(fraction: f64) -> String {
    format!("{}%", (fraction * 100.0).round() as i64)
}

/// The time left, rounded up and in one unit: "8 sec left", "2 min left".
pub fn remaining(seconds: f64) -> UiText {
    let whole = seconds.ceil().max(1.0);
    if whole < 60.0 {
        ui!(keys::TRANSFER_SECONDS_LEFT, format!("{whole:.0}"))
    } else if whole < 3600.0 {
        ui!(
            keys::TRANSFER_MINUTES_LEFT,
            format!("{:.0}", (whole / 60.0).round())
        )
    } else {
        ui!(
            keys::TRANSFER_HOURS_LEFT,
            format!("{:.0}", (whole / 3600.0).round())
        )
    }
}

pub fn declined(transfer: &FileTransfer, device: &str) -> UiText {
    let missing = size::format(transfer.missing_bytes);
    if transfer.outgoing {
        return match transfer.reason.as_str() {
            "rejected" => ui!(keys::TRANSFER_PEER_DECLINED, device),
            "no_space" => ui!(keys::TRANSFER_PEER_NEEDS_SPACE, device, missing),
            "unavailable" => ui!(keys::TRANSFER_PEER_DOES_NOT_RECEIVE, device),
            "too_large" => ui!(keys::TRANSFER_PEER_TOO_LARGE),
            "invalid_path" => ui!(keys::TRANSFER_PEER_INVALID_NAME),
            _ => ui!(keys::TRANSFER_PEER_COULD_NOT_TAKE, device),
        };
    }
    match transfer.reason.as_str() {
        "rejected" => ui!(keys::TRANSFER_DECLINED),
        "no_space" => ui!(keys::TRANSFER_NO_SPACE, missing),
        "unavailable" => ui!(keys::TRANSFER_NOT_RECEIVING),
        "invalid_path" => ui!(keys::TRANSFER_INVALID_NAME),
        _ => ui!(keys::TRANSFER_TOO_MANY),
    }
}

pub fn failed(reason: &str) -> UiText {
    match reason {
        "unreadable" => ui!(keys::TRANSFER_UNREADABLE),
        "file_changed" => ui!(keys::TRANSFER_FILE_CHANGED),
        "corrupt" => ui!(keys::TRANSFER_CORRUPT),
        "write_failed" => ui!(keys::TRANSFER_WRITE_FAILED),
        "symlink" => ui!(keys::TRANSFER_SYMLINK),
        "not_a_file" => ui!(keys::TRANSFER_NOT_A_FILE),
        _ => ui!(keys::TRANSFER_FAILED),
    }
}

/// Whether the row reports a problem.
pub fn is_problem(transfer: &FileTransfer) -> bool {
    detail_is_problem(transfer) || folder_line_is_problem(transfer)
}

/// Whether the line under the name reports a problem: turning files down
/// here is the user's own choice.
pub fn detail_is_problem(transfer: &FileTransfer) -> bool {
    transfer.state() == TransferState::Failed
        || (transfer.state() == TransferState::Declined
            && !(transfer.reason == "rejected" && !transfer.outgoing))
}

/// The notification for what arrived from one computer: "Received
/// Report.pdf (2.3 MB) from Mac mini", "Received 3 files…", with folders
/// "Received 1 folder and 2 files (312 files, 1.2 GB) from Mac mini", and
/// "; 3 files could not be sent" when some did not make it. `None` when
/// nothing arrived.
pub fn received(items: &[FileTransfer], device: &str) -> Option<UiText> {
    let arrived: Vec<&FileTransfer> = items
        .iter()
        .filter(|t| t.state() == TransferState::Done)
        .collect();
    if arrived.is_empty() {
        return None;
    }
    let total = size::format(arrived.iter().map(|t| t.size).sum());
    let folders = arrived.iter().filter(|t| t.folder).count();
    let received = if folders == 0 {
        if arrived.len() == 1 {
            ui!(
                keys::NOTIFY_RECEIVED_ONE,
                arrived[0].name.clone(),
                total,
                device
            )
        } else {
            ui!(
                keys::NOTIFY_RECEIVED_MANY,
                arrived.len().to_string(),
                total,
                device
            )
        }
    } else {
        let files: u32 = arrived
            .iter()
            .map(|t| if t.folder { t.files_done } else { 1 })
            .sum();
        ui!(
            keys::NOTIFY_RECEIVED_ITEMS,
            item_count(folders, arrived.len() - folders),
            count(files),
            total,
            device
        )
    };
    let failed: u32 = items
        .iter()
        .map(|t| match t.state() {
            TransferState::Done if t.folder => t.files_failed,
            TransferState::Failed | TransferState::Declined if t.folder => {
                let all = files_in(t);
                t.files_failed.max(all - t.files_done.min(all))
            }
            TransferState::Failed | TransferState::Declined => 1,
            _ => 0,
        })
        .sum();
    Some(match failed {
        0 => received,
        1 => ui!(keys::NOTIFY_FAILED_ONE, received),
        n => ui!(keys::NOTIFY_FAILED_MANY, received, n.to_string()),
    })
}

/// "1 folder", "2 folders and 3 files".
pub fn item_count(folders: usize, files: usize) -> UiText {
    let folder_part = match folders {
        0 => None,
        1 => Some(ui!(keys::ITEMS_FOLDER_ONE)),
        n => Some(ui!(keys::ITEMS_FOLDER_MANY, n.to_string())),
    };
    let file_part = (files != 0).then(|| count(files as u32));
    match (folder_part, file_part) {
        (Some(f), Some(g)) => ui!(keys::ITEMS_AND, f, g),
        (Some(f), None) => f,
        (None, Some(g)) => g,
        (None, None) => count(0),
    }
}

/// "1 file", "312 files".
pub fn count(files: u32) -> UiText {
    if files == 1 {
        ui!(keys::ITEMS_FILE_ONE)
    } else {
        ui!(keys::ITEMS_FILE_MANY, files.to_string())
    }
}

/// The rows of one batch together, in the order they were sent, each
/// batch where its first row is; rows without a batch stay where they are.
pub fn ordered(transfers: &[FileTransfer]) -> Vec<&FileTransfer> {
    // Each group's place, by batch; rows without one are groups of their own.
    let mut groups: Vec<Vec<(usize, &FileTransfer)>> = Vec::new();
    let mut by_batch: HashMap<&str, usize> = HashMap::new();
    for (index, transfer) in transfers.iter().enumerate() {
        let group = if transfer.batch_id.is_empty() {
            None
        } else {
            by_batch.get(transfer.batch_id.as_str()).copied()
        };
        match group {
            Some(group) => groups[group].push((index, transfer)),
            None => {
                if !transfer.batch_id.is_empty() {
                    by_batch.insert(&transfer.batch_id, groups.len());
                }
                groups.push(vec![(index, transfer)]);
            }
        }
    }
    groups
        .into_iter()
        .flat_map(|mut rows| {
            rows.sort_by_key(|(index, t)| (t.batch_index, *index));
            rows.into_iter().map(|(_, t)| t)
        })
        .collect()
}

/// What can be done about a transfer from the end of its row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Actions {
    pub cancel: bool,
    /// Show the file in its folder.
    pub show: bool,
    /// Take it off the history; the file stays.
    pub remove: bool,
}

/// The buttons at the end of a row: Cancel while it goes or waits; once
/// ended, Show in Folder for a file that made it, then Remove from History.
pub fn actions(transfer: &FileTransfer) -> Actions {
    if !is_finished(transfer.state()) {
        return Actions {
            cancel: true,
            ..Default::default()
        };
    }
    // A drag dropped here or files pasted here went through a staging
    // folder that is gone by then: there is nothing to show.
    let dropped = (!transfer.drag_id.is_empty() || transfer.clipboard) && !transfer.outgoing;
    Actions {
        cancel: false,
        show: transfer.state() == TransferState::Done && !transfer.path.is_empty() && !dropped,
        remove: true,
    }
}

/// Whether Clear History has anything to take off.
pub fn can_clear_history(transfers: &[FileTransfer]) -> bool {
    transfers.iter().any(|t| is_finished(t.state()))
}

/// The transfers still going and the problems not yet seen, for the tray
/// menu's count; finished ones are on the File Transfers page.
pub fn going(transfers: &[FileTransfer], seen: &HashSet<String>) -> usize {
    transfers
        .iter()
        .filter(|t| !is_finished(t.state()) || (is_problem(t) && !seen.contains(&t.transfer_id)))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iw_proto::app::v1::FailedFile;

    fn file(state: TransferState) -> FileTransfer {
        FileTransfer {
            transfer_id: "t".into(),
            name: "Report.pdf".into(),
            size: 2_000_000,
            state: state as i32,
            files: 1,
            ..Default::default()
        }
    }

    #[test]
    fn titles_point_the_way_the_file_goes() {
        let mut t = file(TransferState::Done);
        assert_eq!(title(&t, "Mac"), "Report.pdf ← Mac");
        t.outgoing = true;
        assert_eq!(title(&t, "Mac"), "Report.pdf → Mac");
    }

    #[test]
    fn details_follow_the_state() {
        let mut t = file(TransferState::WaitingForAnswer);
        assert_eq!(
            detail(&t, "Mac").unwrap().to_string(),
            "Mac wants to send you this file (2.0 MB)."
        );
        t.outgoing = true;
        assert_eq!(
            detail(&t, "Mac").unwrap().to_string(),
            "Waiting for Mac to accept…"
        );
        t.state = TransferState::Queued as i32;
        assert_eq!(detail(&t, "Mac").unwrap().to_string(), "Waiting to send…");
        t.state = TransferState::Transferring as i32;
        t.done_bytes = 1_280_000;
        assert_eq!(detail(&t, "Mac").unwrap().to_string(), "64%");
        t.bytes_per_second = 100_000;
        assert_eq!(
            detail(&t, "Mac").unwrap().to_string(),
            "64% · 100.0 kB/s · 8 sec left"
        );
        t.state = TransferState::Done as i32;
        assert_eq!(detail(&t, "Mac").unwrap().to_string(), "Sent");
        t.state = TransferState::Declined as i32;
        t.reason = "no_space".into();
        t.missing_bytes = 5_000_000;
        assert_eq!(
            detail(&t, "Mac").unwrap().to_string(),
            "Mac needs 5.0 MB more free space."
        );
        t.outgoing = false;
        assert_eq!(
            detail(&t, "Mac").unwrap().to_string(),
            "Not enough free space: 5.0 MB more is needed."
        );
        t.state = TransferState::Failed as i32;
        t.reason = "corrupt".into();
        assert_eq!(
            detail(&t, "Mac").unwrap().to_string(),
            "The file arrived damaged."
        );
    }

    #[test]
    fn the_time_left_rounds_up_in_one_unit() {
        assert_eq!(remaining(0.2).to_string(), "1 sec left");
        assert_eq!(remaining(59.1).to_string(), "1 min left");
        assert_eq!(remaining(150.0).to_string(), "3 min left");
        assert_eq!(remaining(5400.0).to_string(), "2 hr left");
    }

    #[test]
    fn progress_bars_only_while_moving_or_cut_off() {
        let mut t = file(TransferState::Transferring);
        t.done_bytes = 500_000;
        assert_eq!(fraction(&t), Some(0.25));
        t.state = TransferState::WaitingForConnection as i32;
        assert_eq!(fraction(&t), Some(0.25));
        t.done_bytes = 0;
        assert_eq!(fraction(&t), None);
        t.state = TransferState::Done as i32;
        assert_eq!(fraction(&t), None);
    }

    #[test]
    fn folders_count_their_files() {
        let mut folder = FileTransfer {
            name: "Photos".into(),
            folder: true,
            files: 312,
            size: 1_200_000_000,
            state: TransferState::Transferring as i32,
            ..Default::default()
        };
        assert_eq!(
            folder_line(&folder).unwrap().to_string(),
            "312 files · 1.2 GB"
        );
        folder.files_done = 8;
        assert_eq!(folder_line(&folder).unwrap().to_string(), "8 of 312 files");
        folder.state = TransferState::Done as i32;
        folder.files_done = 310;
        folder.files_failed = 2;
        folder.failures = vec![FailedFile {
            relative_path: "Photos/a.raw".into(),
            reason: "unreadable".into(),
        }];
        assert_eq!(
            folder_line(&folder).unwrap().to_string(),
            "310 received, 2 failed"
        );
        assert!(is_problem(&folder));
        assert_eq!(detail(&folder, "Mac"), None);
        assert_eq!(
            failure_lines(&folder)[0].to_string(),
            "Photos/a.raw: The file could not be read."
        );
        assert_eq!(unlisted_failures(&folder), 1);
        assert_eq!(folder_line(&file(TransferState::Done)), None);
    }

    #[test]
    fn declining_here_is_no_problem() {
        let mut t = file(TransferState::Declined);
        t.reason = "rejected".into();
        assert!(!is_problem(&t));
        t.outgoing = true;
        assert!(is_problem(&t));
    }

    #[test]
    fn notifications_sum_up_what_arrived() {
        let mut a = file(TransferState::Done);
        a.size = 2_340_000;
        assert_eq!(
            received(&[a.clone()], "Mac").unwrap().to_string(),
            "Received Report.pdf (2.3 MB) from Mac"
        );
        let mut b = a.clone();
        b.name = "b.txt".into();
        assert_eq!(
            received(&[a.clone(), b.clone()], "Mac")
                .unwrap()
                .to_string(),
            "Received 2 files (4.7 MB) from Mac"
        );
        let folder = FileTransfer {
            folder: true,
            files: 312,
            files_done: 309,
            files_failed: 3,
            size: 1_200_000_000,
            state: TransferState::Done as i32,
            ..Default::default()
        };
        assert_eq!(
            received(&[folder, a.clone()], "Mac").unwrap().to_string(),
            "Received 1 folder and 1 file (310 files, 1.2 GB) from Mac; 3 files could not be sent"
        );
        let failed = file(TransferState::Failed);
        assert_eq!(
            received(&[a, failed.clone()], "Mac").unwrap().to_string(),
            "Received Report.pdf (2.3 MB) from Mac; 1 file could not be sent"
        );
        assert_eq!(received(&[failed], "Mac"), None);
    }

    #[test]
    fn batches_stay_together_in_order() {
        let row = |id: &str, batch: &str, index: u32| FileTransfer {
            transfer_id: id.into(),
            batch_id: batch.into(),
            batch_index: index,
            ..Default::default()
        };
        let rows = [
            row("a1", "a", 1),
            row("x", "", 0),
            row("a0", "a", 0),
            row("b0", "b", 0),
        ];
        let ids: Vec<&str> = ordered(&rows)
            .into_iter()
            .map(|t| t.transfer_id.as_str())
            .collect();
        assert_eq!(ids, ["a0", "a1", "x", "b0"]);
    }

    #[test]
    fn rows_offer_what_can_be_done() {
        let going = file(TransferState::Transferring);
        assert_eq!(
            actions(&going),
            Actions {
                cancel: true,
                ..Default::default()
            }
        );
        let mut done = file(TransferState::Done);
        done.path = "/home/u/Downloads/InputWeave/Report.pdf".into();
        assert_eq!(
            actions(&done),
            Actions {
                cancel: false,
                show: true,
                remove: true
            }
        );
        done.drag_id = "d".into();
        assert!(!actions(&done).show, "dropped through a staging folder");
        let failed = file(TransferState::Failed);
        assert_eq!(
            actions(&failed),
            Actions {
                cancel: false,
                show: false,
                remove: true
            }
        );
        assert!(can_clear_history(&[going.clone(), failed]));
        assert!(!can_clear_history(&[going]));
    }
}
