import Foundation

extension Inputweave_App_V1_TransferState {
    /// Whether the transfer has ended, one way or another.
    var isFinished: Bool {
        switch self {
        case .done, .declined, .failed, .cancelled: true
        default: false
        }
    }
}

/// The words for file transfers. A transfer is an
/// item: a file, or a folder with everything in it.
enum TransferText {
    /// "Report.pdf → Windows Desktop", or "←" for a file coming here.
    static func title(_ transfer: FileTransfer, device: String) -> String {
        "\(transfer.name) \(transfer.outgoing ? "→" : "←") \(device)"
    }

    static func bytes(_ count: UInt64) -> String {
        Int64(clamping: count).formatted(.byteCount(style: .file))
    }

    /// "12 files", or "1 file".
    static func files(_ count: UInt32) -> String {
        count == 1 ? String(localized: "1 file") : String(localized: "\(count) files")
    }

    /// What a folder holds: "312 files · 1.2 GB".
    static func folderSummary(_ transfer: FileTransfer) -> String {
        "\(files(transfer.files)) · \(bytes(transfer.size))"
    }

    /// How far along the item is, while that is worth a progress bar.
    static func fraction(_ transfer: FileTransfer) -> Double? {
        guard transfer.size > 0 else { return nil }
        switch transfer.state {
        case .transferring, .verifying:
            break
        case .waitingForConnection where transfer.doneBytes > 0:
            // Cut off midway: the bar stays where it will resume.
            break
        default:
            return nil
        }
        return min(1, Double(transfer.doneBytes) / Double(transfer.size))
    }

    /// The line under the name: progress, what the transfer waits for, or
    /// how it ended.
    static func detail(_ transfer: FileTransfer, device: String) -> String {
        switch transfer.state {
        case .queued:
            // Behind the files sent to the device before it; two go at once.
            return String(localized: "Waiting to send…")
        case .preparing:
            return String(localized: "Preparing…")
        case .waitingForConnection:
            return String(localized: "Waiting for connection…")
        case .waitingForAnswer:
            if transfer.outgoing {
                return String(localized: "Waiting for \(device) to accept…")
            }
            return offer(transfer, device: device)
        case .transferring:
            if transfer.folder {
                return String(localized: "\(transfer.filesDone) of \(files(transfer.files)) · \(progress(transfer))")
            }
            return progress(transfer)
        case .verifying:
            return String(localized: "Checking the file…")
        case .done where transfer.filesFailed > 0, .failed where transfer.filesFailed > 0:
            return partly(transfer)
        case .done:
            return transfer.outgoing ? String(localized: "Sent") : String(localized: "Received")
        case .declined:
            return declined(transfer, device: device)
        case .failed:
            return failed(transfer.reason)
        case .cancelled:
            return String(localized: "Cancelled")
        default:
            return ""
        }
    }

    /// "8 sent, 2 failed": a folder that ended with some of its files not
    /// going.
    static func partly(_ transfer: FileTransfer) -> String {
        transfer.outgoing
            ? String(localized: "\(transfer.filesDone) sent, \(transfer.filesFailed) failed")
            : String(localized: "\(transfer.filesDone) received, \(transfer.filesFailed) failed")
    }

    /// Asked before receiving: what comes, for the whole batch, as the
    /// answer is for all of it.
    static func offer(_ transfer: FileTransfer, device: String) -> String {
        if transfer.batchItems > 1 {
            return String(localized: "\(device) wants to send you \(transfer.batchItems) items (\(files(transfer.batchFiles)), \(bytes(transfer.batchBytes))).")
        }
        if transfer.folder {
            return String(localized: "\(device) wants to send you this folder (\(files(transfer.files)), \(bytes(transfer.size))).")
        }
        return String(localized: "\(device) wants to send you this file (\(bytes(transfer.size))).")
    }

    /// "64% · 12 MB/s · 2 min left".
    static func progress(_ transfer: FileTransfer) -> String {
        let percent = (fraction(transfer) ?? 0).formatted(.percent.precision(.fractionLength(0)))
        guard transfer.bytesPerSecond > 0 else { return percent }
        let left = Double(transfer.size - min(transfer.doneBytes, transfer.size)) / Double(transfer.bytesPerSecond)
        return String(localized: "\(percent) · \(bytes(transfer.bytesPerSecond))/s · \(remaining(left))")
    }

    static func remaining(_ seconds: Double) -> String {
        let rounded = Duration.seconds(max(1, seconds.rounded(.up)))
        let text = rounded.formatted(.units(allowed: [.hours, .minutes, .seconds], width: .abbreviated, maximumUnitCount: 1))
        return String(localized: "\(text) left")
    }

    static func declined(_ transfer: FileTransfer, device: String) -> String {
        let missing = bytes(transfer.missingBytes)
        if transfer.outgoing {
            switch transfer.reason {
            case "rejected": return String(localized: "\(device) declined the file.")
            case "no_space": return String(localized: "\(device) needs \(missing) more free space.")
            case "unavailable": return String(localized: "\(device) does not receive files.")
            case "too_large", "invalid_path": return reason(transfer.reason, outgoing: true)
            default: return String(localized: "\(device) could not take the file.")
            }
        }
        switch transfer.reason {
        case "rejected": return String(localized: "Declined")
        case "no_space": return String(localized: "Not enough free space: \(missing) more is needed.")
        case "unavailable": return String(localized: "This computer does not receive files.")
        case "too_large", "invalid_path": return reason(transfer.reason, outgoing: false)
        default: return String(localized: "Too many files at once.")
        }
    }

    static func failed(_ reason: String) -> String {
        switch reason {
        case "unreadable": String(localized: "The file could not be read.")
        case "file_changed": String(localized: "The file changed while it was being sent.")
        case "corrupt": String(localized: "The file arrived damaged.")
        case "write_failed": String(localized: "The file could not be saved.")
        case "symlink": String(localized: "This is a shortcut (symbolic link) and is not sent.")
        case "not_a_file": String(localized: "This is not a file or a folder.")
        default: String(localized: "The transfer failed.")
        }
    }

    /// Why one file failed or was turned away, as listed under a folder.
    static func reason(_ reason: String, outgoing: Bool) -> String {
        switch reason {
        case "too_large":
            outgoing
                ? String(localized: "The other computer does not take this much at once.")
                : String(localized: "More than this computer takes at once.")
        case "invalid_path":
            outgoing
                ? String(localized: "The other computer cannot save it under this name.")
                : String(localized: "This computer cannot save it under this name.")
        case "busy": String(localized: "Too many files at once.")
        case "no_space": String(localized: "Not enough free space.")
        case "rejected": String(localized: "Declined")
        case "unavailable":
            outgoing
                ? String(localized: "The other computer does not receive files.")
                : String(localized: "This computer does not receive files.")
        default: failed(reason)
        }
    }

    /// A folder's failures listed beyond those named (the first 100).
    static func moreFailures(_ transfer: FileTransfer) -> Int {
        max(0, Int(transfer.filesFailed) - transfer.failures.count)
    }

    /// Whether the row reports a problem: the item failed, was turned away
    /// (other than declined here), or some of a folder's files did not go.
    static func isProblem(_ transfer: FileTransfer) -> Bool {
        switch transfer.state {
        case .failed: true
        case .declined: !(transfer.reason == "rejected" && !transfer.outgoing)
        case .done: transfer.filesFailed > 0
        default: false
        }
    }

    /// What the panel lists: transfers still going, and problems until
    /// they are dismissed. Finished ones are on the File Transfers page.
    static func panelRows(_ transfers: [FileTransfer], dismissed: Set<String>) -> [FileTransfer] {
        transfers.filter { transfer in
            if !transfer.state.isFinished { return true }
            return isProblem(transfer) && !dismissed.contains(transfer.transferID)
        }
    }

    /// The problems dismissed from the panel, as kept across launches:
    /// those still listed, and, the first time
    /// the app sees the list after it starts, every finished problem in it,
    /// as those were there before and are no news.
    static func dismissed(_ dismissed: Set<String>, listed: [FileTransfer], firstLook: Bool) -> Set<String> {
        var kept = dismissed.intersection(listed.map(\.transferID))
        if firstLook {
            kept.formUnion(listed.filter { $0.state.isFinished && isProblem($0) }.map(\.transferID))
        }
        return kept
    }

    /// The notification once items from a device have arrived: "Received
    /// 3 files (120 MB) from MacBook Pro", "Received 1 folder (312 files,
    /// 1.2 GB) from MacBook Pro", with how many files did not come, as
    /// counted in the arrived folders and in `failed` more.
    static func received(_ items: [FileTransfer], device: String, failed: Int = 0) -> String {
        let folders = items.filter(\.folder)
        let size = bytes(items.reduce(0) { $0 + ($1.folder && $1.doneBytes > 0 ? $1.doneBytes : $1.size) })
        let count = UInt32(clamping: items.reduce(0) { $0 + ($1.folder ? Int($1.filesDone) : 1) })
        let missing = failed + items.reduce(0) { $0 + Int($1.filesFailed) }
        var text: String
        if folders.isEmpty {
            if items.count == 1, let file = items.first {
                text = String(localized: "Received \(file.name) (\(size)) from \(device)")
            } else {
                text = String(localized: "Received \(items.count) files (\(size)) from \(device)")
            }
        } else if items.count == 1 {
            text = String(localized: "Received 1 folder (\(files(count)), \(size)) from \(device)")
        } else if folders.count == items.count {
            text = String(localized: "Received \(items.count) folders (\(files(count)), \(size)) from \(device)")
        } else {
            text = String(localized: "Received \(items.count) items (\(files(count)), \(size)) from \(device)")
        }
        if missing == 1 {
            text += String(localized: ", 1 file could not be sent")
        } else if missing > 1 {
            text += String(localized: ", \(missing) files could not be sent")
        }
        return text
    }

    /// Asked before receiving: the notification's title and body, once for
    /// the whole batch.
    static func offerTitle(_ transfer: FileTransfer, device: String) -> String {
        transfer.batchItems > 1 || transfer.folder
            ? String(localized: "\(device) wants to send you files")
            : String(localized: "\(device) wants to send you a file")
    }

    static func offerBody(_ transfer: FileTransfer) -> String {
        if transfer.batchItems > 1 {
            return String(localized: "\(transfer.batchItems) items (\(files(transfer.batchFiles)), \(bytes(transfer.batchBytes)))")
        }
        if transfer.folder {
            return "\(transfer.name) (\(files(transfer.files)), \(bytes(transfer.size)))"
        }
        return "\(transfer.name) (\(bytes(transfer.size)))"
    }
}
