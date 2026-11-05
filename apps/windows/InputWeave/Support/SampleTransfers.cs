using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;

namespace InputWeave.Support;

/// <summary>
/// Transfers shown instead of the background service's, for screenshots
/// of the rows (<see cref="Launch.ShowSampleTransfersArgument"/>): folders
/// going, finished, and finished with failures, both ways, with a file of
/// the same batch queued behind one of them.
/// </summary>
internal static class SampleTransfers
{
    /// <summary>The folder whose failures are shown expanded.</summary>
    public const string Expanded = "sample-project";

    public static AgentSnapshot Over(AgentSnapshot snapshot)
    {
        var device = snapshot.Status.Devices.FirstOrDefault()?.Id ?? "sample";
        var started = (ulong)DateTimeOffset.UtcNow.AddMinutes(-2).ToUnixTimeMilliseconds();
        var transfers = new Transfers { ReceiveFolder = snapshot.Transfers.ReceiveFolder, AskBeforeReceiving = snapshot.Transfers.AskBeforeReceiving };
        var downloads = snapshot.Transfers.ReceiveFolder.Length != 0
            ? snapshot.Transfers.ReceiveFolder
            : @"C:\Users\me\Downloads\InputWeave";

        var photos = new FileTransfer
        {
            TransferId = "sample-photos", Outgoing = true, DeviceId = device, Name = "Photos", Folder = true,
            Size = 1_288_490_189, DoneBytes = 528_280_977, BytesPerSecond = 39_845_888, State = TransferState.Transferring,
            Files = 312, FilesDone = 128, BatchId = "sample-batch", BatchIndex = 0, BatchItems = 2, BatchFiles = 313,
            BatchBytes = 1_290_587_341, StartedAtUnixMs = started, Path = @"C:\Users\me\Pictures\Photos",
        };
        var report = new FileTransfer
        {
            TransferId = "sample-report", Outgoing = true, DeviceId = device, Name = "Report.pdf", Size = 2_097_152,
            State = TransferStates.Queued, Files = 1, BatchId = "sample-batch", BatchIndex = 1, BatchItems = 2,
            BatchFiles = 313, BatchBytes = 1_290_587_341, StartedAtUnixMs = started,
        };
        var project = new FileTransfer
        {
            TransferId = Expanded, Outgoing = false, DeviceId = device, Name = "Project", Folder = true,
            Size = 85_983_232, DoneBytes = 85_983_232, State = TransferState.Done, Files = 120, FilesDone = 117, FilesFailed = 3,
            BatchId = "sample-batch-2", BatchItems = 1, BatchFiles = 120, StartedAtUnixMs = started,
            Path = Path.Combine(downloads, "Project"),
        };
        project.Failures.Add(new FailedFile { RelativePath = "Project/build/cache.db", Reason = "unreadable" });
        project.Failures.Add(new FailedFile { RelativePath = "Project/docs/latest", Reason = "symlink" });
        project.Failures.Add(new FailedFile { RelativePath = "Project/con.txt", Reason = "invalid_path" });
        var music = new FileTransfer
        {
            TransferId = "sample-music", Outgoing = true, DeviceId = device, Name = "Music", Folder = true,
            Size = 412_090_368, DoneBytes = 398_458_880, State = TransferState.Done, Files = 48, FilesDone = 46, FilesFailed = 2,
            BatchId = "sample-batch-3", BatchItems = 1, BatchFiles = 48, StartedAtUnixMs = started, Path = @"C:\Users\me\Music",
        };
        music.Failures.Add(new FailedFile { RelativePath = "Music/Live/track 3.flac", Reason = "file_changed" });
        music.Failures.Add(new FailedFile { RelativePath = "Music/cover.jpg", Reason = "unreadable" });
        var designs = new FileTransfer
        {
            TransferId = "sample-designs", Outgoing = false, DeviceId = device, Name = "Designs", Folder = true,
            Size = 23_068_672, DoneBytes = 23_068_672, State = TransferState.Done, Files = 14, FilesDone = 14,
            BatchId = "sample-batch-4", BatchItems = 1, BatchFiles = 14, StartedAtUnixMs = started,
            Path = Path.Combine(downloads, "Designs"),
        };
        transfers.Transfers_.AddRange([report, photos, project, music, designs]);
        return snapshot with { Transfers = transfers };
    }
}
