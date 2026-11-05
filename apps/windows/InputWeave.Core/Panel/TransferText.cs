using System.Globalization;
using Inputweave.App.V1;

namespace InputWeave.Panel;

public static class TransferStates
{
    /// <summary>
    /// TRANSFER_STATE_QUEUED: a file waiting its turn, as at most two files
    /// go to one computer at a time. Spelled by its number so the app
    /// handles it whether or not the generated enum names it yet.
    /// </summary>
    public const TransferState Queued = (TransferState)10;

    /// <summary>Whether the transfer has ended, one way or another.</summary>
    public static bool IsFinished(this TransferState state) =>
        state is TransferState.Done or TransferState.Declined or TransferState.Failed or TransferState.Cancelled;
}

/// <summary>
/// File sizes as File Explorer shows them: 1024 bytes to the KB, three
/// significant digits.
/// </summary>
public static class ByteSize
{
    private static readonly string[] Units = ["KB", "MB", "GB", "TB", "PB"];

    public static string Format(ulong bytes, CultureInfo culture)
    {
        if (bytes < 1024)
        {
            return string.Create(culture, $"{bytes} B");
        }
        double value = bytes;
        int unit = -1;
        while (value >= 1024 && unit < Units.Length - 1)
        {
            value /= 1024;
            unit++;
        }
        // Truncated, as Explorer does, so a file never looks larger than it is.
        string number = value switch
        {
            < 10 => (Math.Floor(value * 100) / 100).ToString("0.##", culture),
            < 100 => (Math.Floor(value * 10) / 10).ToString("0.#", culture),
            _ => Math.Floor(value).ToString("0", culture),
        };
        return $"{number} {Units[unit]}";
    }
}

/// <summary>The words for file transfers, as on the Mac.</summary>
public static class TransferText
{
    /// <summary>"Report.pdf → Windows Desktop", or "←" for a file coming here; a folder by its name too.</summary>
    public static string Title(FileTransfer transfer, string device) =>
        $"{transfer.Name} {(transfer.Outgoing ? "→" : "←")} {device}";

    /// <summary>The files in an item: a folder's files and empty folders, 1 for a file (also from agents before folders).</summary>
    public static uint FilesIn(FileTransfer transfer) => transfer.Folder ? transfer.Files : Math.Max(1, transfer.Files);

    /// <summary>
    /// A folder's line under its name: "312 files
    /// · 1.2 GB"; "8 of 312 files" once some have arrived; and once it has
    /// ended with some that did not, "310 sent, 2 failed" ("received" for
    /// one that came here). Null for a file.
    /// </summary>
    public static UiText? FolderLine(FileTransfer transfer, CultureInfo culture)
    {
        if (!transfer.Folder)
        {
            return null;
        }
        if (FolderLineIsProblem(transfer))
        {
            return new(
                transfer.Outgoing ? TextKeys.TransferSentSome : TextKeys.TransferReceivedSome,
                transfer.FilesDone.ToString(culture),
                transfer.FilesFailed.ToString(culture));
        }
        if (!transfer.State.IsFinished() && transfer.FilesDone > 0)
        {
            return new(TextKeys.TransferFolderProgress, transfer.FilesDone.ToString(culture), transfer.Files.ToString(culture));
        }
        return FileCount(transfer.Files, transfer.Size, culture);
    }

    /// <summary>"312 files · 1.2 GB".</summary>
    public static UiText FileCount(uint files, ulong bytes, CultureInfo culture) => files == 1
        ? new(TextKeys.TransferFolderFilesOne, ByteSize.Format(bytes, culture))
        : new(TextKeys.TransferFolderFiles, files.ToString(culture), ByteSize.Format(bytes, culture));

    /// <summary>Whether a folder that has ended reports files that did not make it.</summary>
    public static bool FolderLineIsProblem(FileTransfer transfer) =>
        transfer.Folder && transfer.FilesFailed > 0 && transfer.State is TransferState.Done or TransferState.Failed;

    /// <summary>
    /// A folder's failures, "Photos/raw/IMG_0001.CR3: The file could not be
    /// read.", as many as the background service lists (the first 100).
    /// </summary>
    public static IReadOnlyList<UiText> FailureLines(FileTransfer transfer) =>
        transfer.Failures
            .Select(f => new UiText(TextKeys.TransferFailureLine, f.RelativePath, FailureReason(f.Reason, transfer.Outgoing)))
            .ToList();

    /// <summary>The failures beyond those listed, for "12 more".</summary>
    public static uint UnlistedFailures(FileTransfer transfer) =>
        transfer.FilesFailed > (uint)transfer.Failures.Count ? transfer.FilesFailed - (uint)transfer.Failures.Count : 0;

    /// <summary>Why one file of a folder did not make it: a reason of FAILED, or of DECLINED for that file alone.</summary>
    public static UiText FailureReason(string reason, bool outgoing) => reason switch
    {
        "too_large" => new(outgoing ? TextKeys.TransferPeerTooLarge : TextKeys.TransferTooMany),
        "invalid_path" => new(outgoing ? TextKeys.TransferPeerInvalidName : TextKeys.TransferInvalidName),
        "no_space" => new(TextKeys.TransferNoSpaceShort),
        "rejected" => new(TextKeys.TransferDeclined),
        _ => Failed(reason),
    };

    /// <summary>How far along the file is, while that is worth a progress bar.</summary>
    public static double? Fraction(FileTransfer transfer)
    {
        if (transfer.Size == 0)
        {
            return null;
        }
        switch (transfer.State)
        {
            case TransferState.Transferring or TransferState.Verifying:
                break;
            // Cut off midway: the bar stays where it will resume.
            case TransferState.WaitingForConnection when transfer.DoneBytes > 0:
                break;
            default:
                return null;
        }
        return Math.Min(1, (double)transfer.DoneBytes / transfer.Size);
    }

    /// <summary>The line under the name: progress, what the transfer waits for, or how it ended.</summary>
    public static UiText? Detail(FileTransfer transfer, string device, CultureInfo culture) => transfer.State switch
    {
        TransferState.Preparing => new(TextKeys.TransferPreparing),
        TransferStates.Queued => new(TextKeys.TransferQueued),
        TransferState.WaitingForConnection => new(TextKeys.TransferWaitingForConnection),
        TransferState.WaitingForAnswer => transfer.Outgoing
            ? new(TextKeys.TransferWaitingForAccept, device)
            : new(TextKeys.TransferOffer, device, ByteSize.Format(transfer.Size, culture)),
        TransferState.Transferring => Progress(transfer, culture),
        TransferState.Verifying => new(TextKeys.TransferChecking),
        // A folder some of whose files did not make it says so on its own line instead.
        TransferState.Done when FolderLineIsProblem(transfer) => null,
        TransferState.Done => new(transfer.Outgoing ? TextKeys.TransferSent : TextKeys.TransferReceived),
        TransferState.Declined => Declined(transfer, device, culture),
        TransferState.Failed => Failed(transfer.Reason),
        TransferState.Cancelled => new(TextKeys.TransferCancelled),
        _ => null,
    };

    /// <summary>"64% · 12 MB/s · 2 min left", or just the percentage before the speed is known.</summary>
    public static UiText Progress(FileTransfer transfer, CultureInfo culture)
    {
        var percent = Percent(Fraction(transfer) ?? 0, culture);
        if (transfer.BytesPerSecond == 0)
        {
            return new(TextKeys.Verbatim, percent);
        }
        var left = (double)(transfer.Size - Math.Min(transfer.DoneBytes, transfer.Size)) / transfer.BytesPerSecond;
        return new(TextKeys.TransferProgress, percent, ByteSize.Format(transfer.BytesPerSecond, culture), Remaining(left, culture));
    }

    /// <summary>A whole percentage, "64%".</summary>
    public static string Percent(double fraction, CultureInfo culture)
    {
        var format = (NumberFormatInfo)culture.NumberFormat.Clone();
        format.PercentDecimalDigits = 0;
        return fraction.ToString("P0", format);
    }

    /// <summary>The time left, rounded up and in one unit: "8 sec left", "2 min left", "1 hr left".</summary>
    public static UiText Remaining(double seconds, CultureInfo culture)
    {
        var whole = Math.Max(1, Math.Ceiling(seconds));
        if (whole < 60)
        {
            return new(TextKeys.TransferSecondsLeft, whole.ToString("0", culture));
        }
        if (whole < 3600)
        {
            return new(TextKeys.TransferMinutesLeft, Math.Round(whole / 60, MidpointRounding.AwayFromZero).ToString("0", culture));
        }
        return new(TextKeys.TransferHoursLeft, Math.Round(whole / 3600, MidpointRounding.AwayFromZero).ToString("0", culture));
    }

    public static UiText Declined(FileTransfer transfer, string device, CultureInfo culture)
    {
        var missing = ByteSize.Format(transfer.MissingBytes, culture);
        if (transfer.Outgoing)
        {
            return transfer.Reason switch
            {
                "rejected" => new(TextKeys.TransferPeerDeclined, device),
                "no_space" => new(TextKeys.TransferPeerNeedsSpace, device, missing),
                "unavailable" => new(TextKeys.TransferPeerDoesNotReceive, device),
                "too_large" => new(TextKeys.TransferPeerTooLarge),
                "invalid_path" => new(TextKeys.TransferPeerInvalidName),
                _ => new(TextKeys.TransferPeerCouldNotTake, device),
            };
        }
        return transfer.Reason switch
        {
            "rejected" => new(TextKeys.TransferDeclined),
            "no_space" => new(TextKeys.TransferNoSpace, missing),
            "unavailable" => new(TextKeys.TransferNotReceiving),
            "invalid_path" => new(TextKeys.TransferInvalidName),
            _ => new(TextKeys.TransferTooMany),
        };
    }

    public static UiText Failed(string reason) => reason switch
    {
        "unreadable" => new(TextKeys.TransferUnreadable),
        "file_changed" => new(TextKeys.TransferFileChanged),
        "corrupt" => new(TextKeys.TransferCorrupt),
        "write_failed" => new(TextKeys.TransferWriteFailed),
        "symlink" => new(TextKeys.TransferSymlink),
        "not_a_file" => new(TextKeys.TransferNotAFile),
        _ => new(TextKeys.TransferFailed),
    };

    /// <summary>Whether the row reports a problem: it failed, was turned away, or is a folder some of whose files did not make it.</summary>
    public static bool IsProblem(FileTransfer transfer) => DetailIsProblem(transfer) || FolderLineIsProblem(transfer);

    /// <summary>Whether the line under the name reports a problem.</summary>
    public static bool DetailIsProblem(FileTransfer transfer) =>
        transfer.State == TransferState.Failed
        || (transfer.State == TransferState.Declined && !(transfer.Reason == "rejected" && !transfer.Outgoing));

    /// <summary>
    /// The notification for what arrived from one computer, from its items
    /// that ended: "Received Report.pdf (2 MB) from
    /// Mac mini", "Received 3 files (120 MB) from Mac mini", or with a
    /// folder "Received 1 folder and 2 files (312 files, 1.2 GB) from Mac
    /// mini"; followed by "; 3 files could not be sent" when some did not
    /// make it. Null when nothing arrived.
    /// </summary>
    public static UiText? Received(IReadOnlyList<FileTransfer> items, string device, CultureInfo culture)
    {
        var arrived = items.Where(t => t.State == TransferState.Done).ToList();
        if (arrived.Count == 0)
        {
            return null;
        }
        var total = ByteSize.Format(arrived.Aggregate(0UL, (sum, f) => sum + f.Size), culture);
        int folders = arrived.Count(t => t.Folder);
        UiText received;
        if (folders == 0)
        {
            received = arrived.Count == 1
                ? new(TextKeys.NotifyReceivedOne, arrived[0].Name, total, device)
                : new(TextKeys.NotifyReceivedMany, arrived.Count.ToString(culture), total, device);
        }
        else
        {
            uint files = arrived.Aggregate(0u, (sum, t) => sum + (t.Folder ? t.FilesDone : 1));
            received = new(TextKeys.NotifyReceivedItems, Items(folders, arrived.Count - folders, culture), Count(files, culture), total, device);
        }
        uint failed = items.Aggregate(0u, (sum, t) => sum + t.State switch
        {
            TransferState.Done => t.Folder ? t.FilesFailed : 0,
            TransferState.Failed or TransferState.Declined => t.Folder ? Math.Max(t.FilesFailed, FilesIn(t) - Math.Min(t.FilesDone, FilesIn(t))) : 1,
            _ => 0,
        });
        return failed switch
        {
            0 => received,
            1 => new(TextKeys.NotifyFailedOne, received),
            _ => new(TextKeys.NotifyFailedMany, received, failed.ToString(culture)),
        };
    }

    /// <summary>"1 folder", "2 folders and 3 files".</summary>
    public static UiText Items(int folders, int files, CultureInfo culture)
    {
        UiText? folderPart = folders switch
        {
            0 => null,
            1 => new(TextKeys.ItemsFolderOne),
            _ => new(TextKeys.ItemsFolderMany, folders.ToString(culture)),
        };
        UiText? filePart = files == 0 ? null : Count((uint)files, culture);
        return (folderPart, filePart) switch
        {
            ({ } f, { } g) => new(TextKeys.ItemsAnd, f, g),
            ({ } f, null) => f,
            (null, { } g) => g,
            _ => Count(0, culture),
        };
    }

    /// <summary>"1 file", "312 files".</summary>
    public static UiText Count(uint files, CultureInfo culture) =>
        files == 1 ? new(TextKeys.ItemsFileOne) : new(TextKeys.ItemsFileMany, files.ToString(culture));

    /// <summary>
    /// What the panel lists: transfers still going, and problems until they
    /// are dismissed. Finished ones are on the File Transfers page.
    /// </summary>
    public static IReadOnlyList<FileTransfer> PanelRows(IEnumerable<FileTransfer> transfers, IReadOnlySet<string> dismissed) =>
        Ordered(transfers.Where(t => !t.State.IsFinished() || (IsProblem(t) && !dismissed.Contains(t.TransferId))));

    /// <summary>
    /// The rows of one batch together, in the order they were sent
    /// (batch_index), each batch where its first row is; transfers without
    /// a batch stay where they are.
    /// </summary>
    public static IReadOnlyList<FileTransfer> Ordered(IEnumerable<FileTransfer> transfers) =>
        transfers
            .Select((transfer, index) => (Transfer: transfer, Index: index))
            .GroupBy(x => x.Transfer.BatchId.Length == 0 ? "\u0001" + x.Transfer.TransferId : x.Transfer.BatchId)
            .OrderBy(batch => batch.Min(x => x.Index))
            .SelectMany(batch => batch.OrderBy(x => x.Transfer.BatchIndex).ThenBy(x => x.Index).Select(x => x.Transfer))
            .ToList();

    /// <summary>
    /// The buttons at the end of a row, as on the
    /// Mac: Cancel while the file goes or waits; once it has ended, Show in
    /// File Explorer for a file that made it, then Remove from History where
    /// the row is <paramref name="removable"/> (the File Transfers page), or
    /// Dismiss for a problem where it is <paramref name="dismissible"/> (the
    /// panel).
    /// </summary>
    public static TransferActions Actions(FileTransfer transfer, bool dismissible, bool removable)
    {
        if (!transfer.State.IsFinished())
        {
            return TransferActions.Cancel;
        }
        var actions = TransferActions.None;
        // A drag dropped here, or files pasted here, went wherever the user
        // dropped or pasted them, through a staging folder that is gone by
        // then: there is nothing to show.
        bool dropped = (transfer.DragId.Length != 0 || transfer.Clipboard) && !transfer.Outgoing;
        bool showable = transfer.State == TransferState.Done && transfer.Path.Length != 0 && !dropped;
        if (showable)
        {
            actions |= TransferActions.Show;
        }
        if (removable)
        {
            actions |= TransferActions.Remove;
        }
        else if (dismissible && (!showable || IsProblem(transfer)))
        {
            // A folder some of whose files failed is shown and dismissed both.
            actions |= TransferActions.Dismiss;
        }
        return actions;
    }

    /// <summary>Whether Clear History has anything to take off: only finished transfers go.</summary>
    public static bool CanClearHistory(IEnumerable<FileTransfer> transfers) =>
        transfers.Any(t => t.State.IsFinished());
}

/// <summary>What can be done about one transfer, from the end of its row.</summary>
[Flags]
public enum TransferActions
{
    None = 0,
    /// <summary>Stops a file going or waiting.</summary>
    Cancel = 1,
    /// <summary>Shows the file in File Explorer.</summary>
    Show = 2,
    /// <summary>Takes the transfer off the history; the file stays.</summary>
    Remove = 4,
    /// <summary>Takes a problem off the panel; the history keeps it.</summary>
    Dismiss = 8,
}
