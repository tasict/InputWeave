using System.Buffers.Binary;
using System.Security.Cryptography;
using System.Text;
using Inputweave.App.V1;
using InputWeave.Panel;

namespace InputWeave.Drag;

/// <summary>
/// A file or folder promised to the drop target: its path below the
/// folder the user drops into, separated by "\", whether it is a folder,
/// its size, and, once it has arrived, where it waits to be copied there.
/// </summary>
public sealed record PromisedEntry(string RelativePath, bool IsFolder, ulong Size, string? Source = null);

/// <summary>
/// The files another computer drags here, described the way Windows takes
/// files that do not exist yet (CFSTR_FILEDESCRIPTORW): a list of entries,
/// folders before what is in them, whose contents the drop target reads one
/// by one (CFSTR_FILECONTENTS). File Explorer creates the folders and the
/// files where the user drops them.
/// </summary>
public static class FileDescriptors
{
    /// <summary>The characters of a path that fit a descriptor, before its terminating NUL (MAX_PATH).</summary>
    public const int MaxPath = 259;

    /// <summary>The size of a FILEDESCRIPTORW.</summary>
    public const int DescriptorSize = 592;

    public const uint FdAttributes = 0x4;
    public const uint FdFileSize = 0x40;
    public const uint FdProgressUi = 0x4000;
    public const uint FdUnicode = 0x80000000;
    public const uint FileAttributeDirectory = 0x10;
    public const uint FileAttributeNormal = 0x80;

    private const int AttributesOffset = 36;
    private const int SizeHighOffset = 64;
    private const int SizeLowOffset = 68;
    private const int NameOffset = 72;

    /// <summary>
    /// The items as offered, before anything has arrived: each one by its
    /// name, a folder as a folder alone. The offer names the items only, so
    /// what is in a folder is known once it has arrived
    /// (<see cref="Staged"/>).
    /// </summary>
    public static IReadOnlyList<PromisedEntry> Offered(DragArrived arrived) =>
        arrived.Items
            .Select(i => new PromisedEntry(SafeName(i.Name), i.Folder, i.Folder ? 0 : i.Size))
            .ToList();

    /// <summary>
    /// What arrived, from the files and folders the transfers saved: each
    /// one by its name, and a folder followed by what is in it, depth
    /// first, in name order. The background service's partial files are
    /// left out, and so is anything whose path does not fit a descriptor
    /// (<paramref name="tooLong"/> counts those).
    /// </summary>
    public static IReadOnlyList<PromisedEntry> Staged(IEnumerable<string> arrived, out int tooLong)
    {
        var entries = new List<PromisedEntry>();
        int skipped = 0;
        foreach (var path in arrived)
        {
            var name = Path.GetFileName(Path.TrimEndingDirectorySeparator(path));
            if (name.Length == 0 || IsPartial(name))
            {
                continue;
            }
            if (Directory.Exists(path))
            {
                AddFolder(entries, path, name, ref skipped);
            }
            else if (File.Exists(path))
            {
                Add(entries, new PromisedEntry(name, false, (ulong)new FileInfo(path).Length, path), ref skipped);
            }
        }
        tooLong = skipped;
        return entries;
    }

    private static void AddFolder(List<PromisedEntry> entries, string folder, string relative, ref int skipped)
    {
        if (!Add(entries, new PromisedEntry(relative, true, 0, folder), ref skipped))
        {
            return;
        }
        IEnumerable<string> children;
        try
        {
            children = Directory.EnumerateFileSystemEntries(folder).Order(StringComparer.OrdinalIgnoreCase).ToList();
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            return;
        }
        foreach (var child in children)
        {
            var name = Path.GetFileName(child);
            if (IsPartial(name))
            {
                continue;
            }
            var path = $"{relative}\\{name}";
            var attributes = File.GetAttributes(child);
            if ((attributes & FileAttributes.ReparsePoint) != 0)
            {
                // Never written by the service; nothing to follow.
                continue;
            }
            if ((attributes & FileAttributes.Directory) != 0)
            {
                AddFolder(entries, child, path, ref skipped);
            }
            else
            {
                Add(entries, new PromisedEntry(path, false, (ulong)new FileInfo(child).Length, child), ref skipped);
            }
        }
    }

    private static bool Add(List<PromisedEntry> entries, PromisedEntry entry, ref int skipped)
    {
        if (entry.RelativePath.Length > MaxPath)
        {
            skipped++;
            return false;
        }
        entries.Add(entry);
        return true;
    }

    /// <summary>
    /// The background service's file still being written:
    /// <c>.inputweave-&lt;transfer id&gt;.part</c>, which a failed or
    /// unfinished transfer may leave behind.
    /// </summary>
    public static bool IsPartial(string name) =>
        name.StartsWith(".inputweave-", StringComparison.OrdinalIgnoreCase)
        && name.EndsWith(".part", StringComparison.OrdinalIgnoreCase);

    /// <summary>
    /// A name another computer sent, safe to hand to File Explorer as one
    /// file or folder name: no path separators or other characters Windows
    /// does not allow, and not "." or "..".
    /// </summary>
    public static string SafeName(string name)
    {
        var safe = new StringBuilder(name.Length);
        foreach (var c in name)
        {
            safe.Append(c < 32 || "\\/:*?\"<>|".Contains(c) ? '_' : c);
        }
        var result = safe.ToString().TrimEnd(' ', '.');
        if (result.Length == 0)
        {
            return "_";
        }
        return result.Length > MaxPath ? result[..MaxPath] : result;
    }

    /// <summary>
    /// A FILEGROUPDESCRIPTORW of the entries: their count, then one
    /// FILEDESCRIPTORW each with its attributes, size and path, asking File
    /// Explorer to show its progress dialog.
    /// </summary>
    public static byte[] Encode(IReadOnlyList<PromisedEntry> entries)
    {
        var bytes = new byte[4 + entries.Count * DescriptorSize];
        BinaryPrimitives.WriteUInt32LittleEndian(bytes, (uint)entries.Count);
        for (int i = 0; i < entries.Count; i++)
        {
            var entry = entries[i];
            var at = bytes.AsSpan(4 + i * DescriptorSize, DescriptorSize);
            uint flags = FdAttributes | FdProgressUi | FdUnicode | (entry.IsFolder ? 0 : FdFileSize);
            BinaryPrimitives.WriteUInt32LittleEndian(at, flags);
            BinaryPrimitives.WriteUInt32LittleEndian(at[AttributesOffset..], entry.IsFolder ? FileAttributeDirectory : FileAttributeNormal);
            BinaryPrimitives.WriteUInt32LittleEndian(at[SizeHighOffset..], (uint)(entry.Size >> 32));
            BinaryPrimitives.WriteUInt32LittleEndian(at[SizeLowOffset..], (uint)entry.Size);
            var name = entry.RelativePath.Length > MaxPath ? entry.RelativePath[..MaxPath] : entry.RelativePath;
            Encoding.Unicode.GetBytes(name, at[NameOffset..]);
        }
        return bytes;
    }
}

/// <summary>Where the files of a drag dropped here have arrived so far.</summary>
/// <param name="Rows">The batch's items this computer lists, one per file or folder dragged.</param>
/// <param name="Finished">Every item has arrived, failed or been cancelled.</param>
/// <param name="Arrived">The files and folders saved so far, where they were saved.</param>
public sealed record StagedBatch(IReadOnlyList<FileTransfer> Rows, bool Finished, IReadOnlyList<string> Arrived)
{
    /// <summary>Bytes arrived and in all, for the log.</summary>
    public (ulong Done, ulong Total) Bytes =>
        (Rows.Aggregate(0UL, (sum, r) => sum + r.DoneBytes), Rows.Aggregate(0UL, (sum, r) => sum + r.Size));
}

/// <summary>What a drag offered here does once the button is let go.</summary>
public enum DragRelease
{
    /// <summary>Keep the drag going.</summary>
    Continue,
    /// <summary>Drop on what is under the cursor.</summary>
    Drop,
    /// <summary>End the drag without dropping.</summary>
    Cancel,
}

/// <summary>What is left to do once the system drag of an offer has ended.</summary>
public enum AfterDrop
{
    /// <summary>Nothing was dropped, or nothing took the files: decline the offer.</summary>
    Decline,
    /// <summary>The drop target copies the files on its own time; keep serving them.</summary>
    Wait,
    /// <summary>The drop target has taken everything it wanted.</summary>
    Finish,
}

/// <summary>
/// The decisions behind a drag another computer offers here:
/// File Explorer never says where the user
/// dropped, so the files are received into a staging folder of the app's
/// and handed over from there as the drop target reads them.
/// </summary>
public static class DragStaging
{
    /// <summary>
    /// How long after the drop the files are asked for at the earliest: the
    /// release-all that ends control of this computer when the cursor goes
    /// back looks the same as the user letting go, until the service says
    /// this computer is no longer controlled (<see cref="TakesDrop"/>).
    /// </summary>
    public static readonly TimeSpan Settle = TimeSpan.FromMilliseconds(250);

    /// <summary><c>%LOCALAPPDATA%\InputWeave\drag</c>.</summary>
    public static string Root(string localAppData) => Path.Combine(localAppData, "InputWeave", "drag");

    /// <summary>The staging folder of one drag. Its id comes from the other computer, so only a safe one is used as it is.</summary>
    public static string FolderFor(string root, string dragId) => Path.Combine(root, SafeId(dragId));

    /// <summary>The drag's id when it is short and only letters, digits, "-" and "_"; otherwise a hash of it.</summary>
    public static string SafeId(string dragId)
    {
        bool plain = dragId.Length is > 0 and <= 64
            && dragId.All(c => c is (>= 'a' and <= 'z') or (>= 'A' and <= 'Z') or (>= '0' and <= '9') or '-' or '_');
        return plain ? dragId : Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(dragId)))[..32];
    }

    /// <summary>
    /// The batch a drag dropped here brings: the transfers the background
    /// service lists for it, finished once there are as many as were
    /// offered (<paramref name="items"/>) and each one has ended.
    /// </summary>
    public static StagedBatch Batch(Transfers transfers, string dragId, int items)
    {
        var rows = transfers.Transfers_
            .Where(t => !t.Outgoing && t.DragId == dragId)
            .OrderBy(t => t.BatchIndex)
            .ToList();
        int expected = Math.Max(items, rows.Select(r => (int)r.BatchItems).DefaultIfEmpty(0).Max());
        bool finished = rows.Count != 0 && rows.Count >= expected && rows.All(r => r.State.IsFinished());
        var arrived = rows
            .Where(r => r.State == TransferState.Done && r.Path.Length != 0)
            .Select(r => r.Path)
            .ToList();
        return new StagedBatch(rows, finished, arrived);
    }

    /// <summary>
    /// What the drag does as it goes (IDropSource::QueryContinueDrag, which
    /// OLE calls on input only). Escape, or the offer ending, cancels.
    /// Letting go of the button drops, unless the cursor is in a strip
    /// along an edge the cursor leaves by, or this computer is no longer
    /// controlled by the one that offered: then the cursor went back and
    /// the release came from that, not from the user.
    /// </summary>
    public static DragRelease OnQuery(bool escape, bool buttonDown, bool offerEnded, bool controlledBySource, bool inStrip)
    {
        if (escape || offerEnded)
        {
            return DragRelease.Cancel;
        }
        if (buttonDown)
        {
            return DragRelease.Continue;
        }
        return inStrip || !controlledBySource ? DragRelease.Cancel : DragRelease.Drop;
    }

    /// <summary>
    /// Whether the cursor is still where the drag came with it: on this
    /// computer, still controlled by the one that offered the drag; or, for
    /// a drag carried back here from a computer this one controls
    /// (<c>at_controller</c>), controlled by none, with the cursor on this
    /// computer rather than gone on to another.
    /// </summary>
    public static bool StillHere(DragArrived arrived, Agent.AgentSnapshot snapshot) =>
        arrived.AtController
            ? !snapshot.ControlledFromElsewhere && snapshot.CursorDeviceId == snapshot.ThisDeviceId
            : snapshot.Status.ControlledByDeviceId == arrived.FromDeviceId;

    /// <summary>
    /// Whether a drop is taken once <see cref="Settle"/> has passed: the
    /// offer still stands and this computer is still controlled by the one
    /// that offered. Otherwise the release came from the cursor going back,
    /// which the service said only after the drop, and nothing is handed
    /// over.
    /// </summary>
    public static bool TakesDrop(bool offerEnded, bool controlledBySource) => !offerEnded && controlledBySource;

    /// <summary>
    /// What follows the system drag. A drop target that copies on its own
    /// time (as File Explorer does, IDataObjectAsyncCapability) is waited
    /// for until it says it is done; one that read the files while it took
    /// the drop (<paramref name="accepted"/>: it asked for them) has them
    /// all. Without a drop, dropped where nothing takes files, or taken by
    /// a target that asked for nothing, the offer is declined.
    /// </summary>
    public static AfterDrop Next(bool accepted, bool copyingStarted, bool copyingEnded) =>
        copyingStarted && !copyingEnded ? AfterDrop.Wait
        : accepted ? AfterDrop.Finish
        : AfterDrop.Decline;

    /// <summary>Why the files could not be taken, for the HUD.</summary>
    public static UiText AcceptRefused(string code) => code switch
    {
        "invalid_folder" => new UiText(TextKeys.HudDragInvalidFolder),
        "not_found" => new UiText(TextKeys.HudDragGone),
        _ => new UiText(TextKeys.HudDragNotReceived),
    };
}
