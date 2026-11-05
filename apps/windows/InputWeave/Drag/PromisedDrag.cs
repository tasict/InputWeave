using System.Collections.Concurrent;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.Marshalling;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Overlay;
using InputWeave.Panel;
using InputWeave.Support;

namespace InputWeave.Drag;

/// <summary>How a drag offered here went, for the app to show.</summary>
internal enum PromiseOutcome
{
    /// <summary>Dropped where files go, and the background service took the drop.</summary>
    Taken,
    /// <summary>Dropped, but the background service refused the folder (the code says why).</summary>
    Refused,
    /// <summary>Not dropped, or dropped where nothing takes files.</summary>
    NotDropped,
}

/// <summary>What a drag offered here needs from the rest of the app; the background service, or a stand-in for testing.</summary>
internal interface IPromiseHost
{
    /// <summary>Takes the drop into <paramref name="folder"/>; null once accepted, else the refusal's code.</summary>
    Task<string?> AcceptAsync(string dragId, string folder);

    Task DeclineAsync(string dragId);

    /// <summary>The items of the drag that have arrived so far.</summary>
    StagedBatch Batch(string dragId, int items);

    /// <summary>Whether the offer ended before it was answered here: taken back, or expired.</summary>
    bool Ended(string dragId);

    /// <summary>Whether the cursor is still where <paramref name="arrived"/> came with it (<see cref="DragStaging.StillHere"/>).</summary>
    bool StillHere(DragArrived arrived);

    /// <summary>The drag strips, in pixels, where the cursor is when it goes back to another computer.</summary>
    IReadOnlyList<PixelRect> Strips { get; }

    /// <summary>Raised, on any thread, whenever any of the above may have changed.</summary>
    event Action? Changed;

    /// <summary>How the drag went, once the system drag is over.</summary>
    void Report(string dragId, PromiseOutcome outcome, string? code);
}

/// <summary>
/// The target side of dragging files from another computer:
/// when the cursor comes here with a drag, a
/// system drag starts where it is, of files promised to whatever they are
/// dropped on (<see cref="FilePromise"/>). File Explorer never says where
/// the user dropped, so once it asks for the files the background service
/// receives them into a staging folder of the app's
/// (<c>%LOCALAPPDATA%\InputWeave\drag\&lt;id&gt;</c>), from which File
/// Explorer copies them where they were dropped; the staging folder goes
/// once it is done. Each drag has an OLE thread of its own, which lives
/// until File Explorer has copied everything.
/// </summary>
internal static class PromisedDrag
{
    /// <summary>How long a drop target that copies on its own time is given after it last asked for anything.</summary>
    private static readonly TimeSpan CopyingLimit = TimeSpan.FromMinutes(30);

    private const nuint Watchdog = 0xD06;

    public static void Start(DragArrived arrived, IPromiseHost host, string stagingRoot)
    {
        var folder = DragStaging.FolderFor(stagingRoot, arrived.DragId);
        var thread = new OleThread($"InputWeave drag {DragStaging.SafeId(arrived.DragId)}");
        thread.Post(() => Run(arrived, host, folder, thread));
    }

    private static void Run(DragArrived arrived, IPromiseHost host, string folder, OleThread thread)
    {
        var id = arrived.DragId;
        var pad = new DragPad();
        var promise = new FilePromise(arrived, host, folder, thread, pad.Hide);
        nint data = 0, source = 0;
        var outcome = PromiseOutcome.NotDropped;
        string? code = null;
        try
        {
            // The service presses the button on a thread of its own as it
            // tells the app; give the press a moment to land.
            thread.PumpUntil(Win32.AnyButtonDown, TimeSpan.FromMilliseconds(20), TimeSpan.FromMilliseconds(300));
            if (!Win32.AnyButtonDown())
            {
                // Let go before the drag could start here: nothing to drop.
                AppLog.Info($"drag {id}: the button is already up; DeclineDrag");
                _ = DeclineAsync(host, id);
                return;
            }
            // The service pressed the button where the cursor came in, on
            // whatever window is there. Mouse input during a drag goes to the
            // thread whose window took the press, so the press is made again
            // on a window of this thread's, under the cursor; OLE's loop then
            // sees the drag's input.
            pad.ShowAtCursor();
            bool pressed = Win32.PressAgain();
            thread.PumpUntil(() => pad.Pressed, TimeSpan.FromMilliseconds(20), TimeSpan.FromMilliseconds(500));
            AppLog.Info($"drag {id}: pressed again on the drag window ({(pressed ? "sent" : "not sent")}, {(pad.Pressed ? "landed" : "did not land")})");
            data = Ole.ComPointer<IDataObject>(promise);
            source = Ole.ComPointer<IDropSource>(promise);
            AppLog.Info($"drag {id}: starting a drag of {arrived.Items.Count} items here");
            // Without input the drag's loop never asks the source whether to
            // go on: a drag that never hears from the mouse, or whose offer
            // was taken back meanwhile, is ended by taking the mouse away.
            thread.Every(Watchdog, 100, promise.Watch);
            int result = Ole.DoDragDrop(data, source, Ole.DROPEFFECT_COPY, out uint effect);
            thread.StopTimer(Watchdog);
            pad.Hide();
            AppLog.Info($"drag {id}: the drag here ended (0x{result:X8}, effect {effect}; asked for the files: {promise.Accepted}, copying on its own: {promise.CopyingStarted})");
            var next = DragStaging.Next(promise.Accepted || promise.Refusal is not null, promise.CopyingStarted, promise.CopyingEnded);
            if (next == AfterDrop.Wait)
            {
                AppLog.Info($"drag {id}: waiting for the drop target to copy the files");
                thread.PumpUntil(() => promise.CopyingEnded || DateTime.UtcNow - promise.LastAsked > CopyingLimit, TimeSpan.FromSeconds(1));
                AppLog.Info($"drag {id}: the drop target is done ({(promise.CopyingEnded ? "it said so" : "it went quiet")}); served {promise.Served} files");
                next = DragStaging.Next(promise.Accepted || promise.Refusal is not null, promise.CopyingStarted, copyingEnded: true);
            }
            if (promise.Refusal is { } refusal)
            {
                (outcome, code) = (PromiseOutcome.Refused, refusal);
            }
            else if (next == AfterDrop.Decline)
            {
                if (!host.Ended(id))
                {
                    AppLog.Info($"drag {id}: not dropped here; DeclineDrag");
                    _ = DeclineAsync(host, id);
                }
            }
            else
            {
                outcome = PromiseOutcome.Taken;
            }
        }
        catch (Exception e)
        {
            AppLog.Warn($"drag {id}: {e}");
        }
        finally
        {
            if (source != 0)
            {
                Marshal.Release(source);
            }
            if (data != 0)
            {
                Marshal.Release(data);
            }
            pad.Close();
            promise.Finish();
            host.Report(id, outcome, code);
            thread.Quit();
        }
    }

    private static async Task DeclineAsync(IPromiseHost host, string id)
    {
        try
        {
            await host.DeclineAsync(id).ConfigureAwait(false);
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException)
        {
            AppLog.Info($"decline drag {id}: {e.Message}");
        }
    }

    /// <summary>
    /// Removes staging folders left behind, as by an app that ended
    /// mid-drag: those made before <paramref name="startedUtc"/>, the app's
    /// start, so one a drag of this run just made stays; at launch.
    /// </summary>
    public static void ClearStaging(string stagingRoot, DateTime startedUtc, string from = "drags")
    {
        try
        {
            if (!Directory.Exists(stagingRoot))
            {
                return;
            }
            int removed = 0;
            var folders = Directory.EnumerateDirectories(stagingRoot).Select(f => (f, Directory.GetCreationTimeUtc(f)));
            foreach (var folder in LeftoverFolders.FromBefore(folders, startedUtc).ToList())
            {
                Directory.Delete(folder, recursive: true);
                removed++;
            }
            if (removed != 0)
            {
                AppLog.Info($"removed {removed} staging folders left from earlier {from}");
            }
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            AppLog.Warn($"cannot clear the staging folder of {from}: {e.Message}");
        }
    }
}

/// <summary>
/// The promised files of a drag offered here, as OLE takes them: the data
/// object a drop target reads (CFSTR_FILEDESCRIPTORW, then
/// CFSTR_FILECONTENTS one by one), the drag's source, which decides when it
/// drops (<see cref="DragStaging.OnQuery"/>), and its asynchronous
/// capability, so File Explorer copies on a thread of its own.
/// </summary>
/// <remarks>
/// The offer only names the items, so before the drop the descriptor lists
/// them alone. Once dropped, the first request for the files accepts the
/// drop into the staging folder and waits for everything to arrive; the
/// descriptor then lists all of it, folders before what is in them, as it
/// arrived, and each file's contents are a stream on the staged file.
/// </remarks>
[GeneratedComClass]
internal sealed unsafe partial class FilePromise : IDataObject, IDataObjectAsyncCapability, IDropSource
{
    private static readonly ushort DescriptorFormat = PromisedFormats.Descriptor;
    private static readonly ushort ContentsFormat = PromisedFormats.Contents;
    private static readonly ushort PreferredEffectFormat = PromisedFormats.PreferredEffect;

    /// <summary>Waiting for the files, this long without any of them moving gives up.</summary>
    private static readonly TimeSpan Stalled = TimeSpan.FromMinutes(10);

    private readonly DragArrived arrived;
    private readonly IPromiseHost host;
    private readonly string folder;
    private readonly OleThread thread;
    private readonly object gate = new();
    private readonly Dictionary<ushort, byte[]> stored = [];
    private readonly AutoResetEvent changed = new(false);
    private readonly Action wake;
    private bool asyncMode = true;
    private DateTime? releasedAt;
    private volatile bool dropped;
    private Task<string?>? accepting;
    private IReadOnlyList<PromisedEntry>? staged;
    private int served;
    private bool loggedRead;
    private bool loggedQuery;
    private readonly Action going;
    private DateTime? watchingSince;
    private bool gaveUp;

    /// <summary>
    /// Checked while the system drag goes on (on this drag's thread): a drag
    /// whose loop has heard nothing from the mouse for two seconds, whose
    /// offer was taken back before the drop, or where Escape is pressed, ends
    /// by losing the mouse, which OLE takes as a cancel.
    /// </summary>
    public void Watch()
    {
        var now = DateTime.UtcNow;
        watchingSince ??= now;
        bool deaf = !loggedQuery && now - watchingSince > TimeSpan.FromSeconds(2);
        bool withdrawn = !dropped && host.Ended(Id);
        // Escape goes to the window in front, not to this thread's drag loop.
        bool escape = !dropped && Win32.EscapeDown();
        if (!deaf && !withdrawn && !escape)
        {
            return;
        }
        if (!gaveUp)
        {
            gaveUp = true;
            AppLog.Info(
                $"drag {Id}: {(escape ? "escape" : withdrawn ? "the offer was taken back" : "the drag hears nothing from the mouse")}; cancelling it");
        }
        Win32.ReleaseCapture();
    }

    public FilePromise(DragArrived arrived, IPromiseHost host, string folder, OleThread thread, Action going)
    {
        this.arrived = arrived;
        this.host = host;
        this.folder = folder;
        this.thread = thread;
        this.going = going;
        wake = () => changed.Set();
        host.Changed += wake;
        LastAsked = DateTime.UtcNow;
    }

    private string Id => arrived.DragId;

    /// <summary>The drop target asked for the files, and the background service took the drop.</summary>
    public bool Accepted { get; private set; }

    /// <summary>Why the background service refused the drop, if it did.</summary>
    public string? Refusal { get; private set; }

    /// <summary>Dropped, but the release came from the cursor going back: nothing was handed over.</summary>
    public bool WentBack { get; private set; }

    public bool CopyingStarted { get; private set; }

    public bool CopyingEnded { get; private set; }

    /// <summary>When the drop target last asked for anything.</summary>
    public DateTime LastAsked { get; private set; }

    public int Served => served;

    /// <summary>Lets go of the background service's changes and removes the staging folder.</summary>
    public void Finish()
    {
        host.Changed -= wake;
        for (int attempt = 0; attempt < 5 && Directory.Exists(folder); attempt++)
        {
            try
            {
                Directory.Delete(folder, recursive: true);
                AppLog.Info($"drag {Id}: removed its staging folder");
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException)
            {
                if (attempt == 4)
                {
                    AppLog.Warn($"drag {Id}: cannot remove its staging folder: {e.Message}");
                }
                Thread.Sleep(200);
            }
        }
    }

    // IDropSource

    public int QueryContinueDrag(int escapePressed, uint keys)
    {
        bool down = (keys & (Ole.MK_LBUTTON | Ole.MK_RBUTTON | Ole.MK_MBUTTON)) != 0;
        var (x, y) = Win32.CursorPosition();
        bool inStrip = DragStrips.Contains(host.Strips, new PixelPoint(x, y));
        bool controlled = host.StillHere(arrived);
        bool ended = host.Ended(Id);
        if (!loggedQuery)
        {
            loggedQuery = true;
            AppLog.Info($"drag {Id}: going (button down {down})");
            // The window that took the press would be in the drag's way.
            going();
        }
        bool escape = escapePressed != 0 || Win32.EscapeDown();
        switch (DragStaging.OnQuery(escape, down, ended, controlled, inStrip))
        {
            case DragRelease.Drop:
                releasedAt = DateTime.UtcNow;
                dropped = true;
                AppLog.Info($"drag {Id}: released; dropping");
                return Ole.DRAGDROP_S_DROP;
            case DragRelease.Cancel:
                AppLog.Info($"drag {Id}: cancelled (escape {escape}, offer ended {ended}, button down {down}, in a strip {inStrip}, still controlled {controlled})");
                return Ole.DRAGDROP_S_CANCEL;
            default:
                return Ole.S_OK;
        }
    }

    public int GiveFeedback(uint effect) => Ole.DRAGDROP_S_USEDEFAULTCURSORS;

    // IDataObject

    public int GetData(FormatEtc* format, StgMedium* medium)
    {
        if (format is null || medium is null)
        {
            return Ole.E_INVALIDARG;
        }
        *medium = default;
        LastAsked = DateTime.UtcNow;
        var f = *format;
        try
        {
            if (f.Format == DescriptorFormat && (f.Medium & Ole.TYMED_HGLOBAL) != 0)
            {
                var entries = dropped ? Staged() : FileDescriptors.Offered(arrived);
                return Global(FileDescriptors.Encode(entries), medium);
            }
            if (f.Format == ContentsFormat && (f.Medium & Ole.TYMED_ISTREAM) != 0)
            {
                return Contents(f.Index, medium);
            }
            if (f.Format == PreferredEffectFormat && (f.Medium & Ole.TYMED_HGLOBAL) != 0)
            {
                return Global(BitConverter.GetBytes(Ole.DROPEFFECT_COPY), medium);
            }
            byte[]? kept;
            lock (gate)
            {
                stored.TryGetValue(f.Format, out kept);
            }
            if (kept is not null && (f.Medium & Ole.TYMED_HGLOBAL) != 0)
            {
                return Global(kept, medium);
            }
            return Ole.DV_E_FORMATETC;
        }
        catch (Exception e)
        {
            AppLog.Warn($"drag {Id}: GetData failed: {e.Message}");
            return Ole.E_UNEXPECTED;
        }
    }

    private static int Global(byte[] bytes, StgMedium* medium) => PromisedFormats.Global(bytes, medium);

    private int Contents(int index, StgMedium* medium)
    {
        if (!dropped)
        {
            // Read only once dropped.
            return Ole.DV_E_FORMATETC;
        }
        var entries = Staged();
        if (index < 0 || index >= entries.Count || entries[index].IsFolder || entries[index].Source is not { } source)
        {
            return Ole.DV_E_LINDEX;
        }
        int opened = PromisedFormats.Stream(source, medium);
        if (opened != Ole.S_OK)
        {
            AppLog.Warn($"drag {Id}: cannot open staged file {index}: 0x{opened:X8}");
            return opened;
        }
        if (!loggedRead)
        {
            loggedRead = true;
            AppLog.Info($"drag {Id}: the drop target reads the files (on thread {Environment.CurrentManagedThreadId})");
        }
        Interlocked.Increment(ref served);
        return Ole.S_OK;
    }

    /// <summary>
    /// What arrived, once everything has: the first call after the drop
    /// accepts it into the staging folder and waits; the others wait for
    /// the same answer.
    /// </summary>
    private IReadOnlyList<PromisedEntry> Staged()
    {
        lock (gate)
        {
            if (staged is not null)
            {
                return staged;
            }
        }
        if (!Accept())
        {
            lock (gate)
            {
                return staged ??= [];
            }
        }
        var started = DateTime.UtcNow;
        var lastMove = started;
        var lastShape = "";
        StagedBatch batch;
        while (true)
        {
            batch = host.Batch(Id, arrived.Items.Count);
            if (batch.Finished)
            {
                break;
            }
            var (done, _) = batch.Bytes;
            var shape = $"{batch.Rows.Count}/{done}/{string.Join(",", batch.Rows.Select(r => (int)r.State))}";
            var now = DateTime.UtcNow;
            if (shape != lastShape)
            {
                (lastShape, lastMove) = (shape, now);
            }
            else if (now - lastMove > Stalled)
            {
                AppLog.Warn($"drag {Id}: nothing arrived for {Stalled.TotalMinutes:0} minutes; handing over what did");
                break;
            }
            LastAsked = now;
            // Managed waits on this thread keep serving COM calls.
            changed.WaitOne(TimeSpan.FromSeconds(1));
        }
        var entries = FileDescriptors.Staged(batch.Arrived, out int tooLong);
        AppLog.Info(
            $"drag {Id}: {batch.Arrived.Count} of {arrived.Items.Count} items arrived in {(DateTime.UtcNow - started).TotalSeconds:0.0} s; "
            + $"{entries.Count(e => !e.IsFolder)} files and {entries.Count(e => e.IsFolder)} folders to hand over"
            + (tooLong == 0 ? "" : $", {tooLong} paths too long to hand over"));
        lock (gate)
        {
            return staged ??= entries;
        }
    }

    /// <summary>
    /// Accepts the drop into the staging folder, once; false when that is
    /// refused, or when the release turns out to have come from the cursor
    /// going back (<see cref="DragStaging.TakesDrop"/>).
    /// </summary>
    private bool Accept()
    {
        if (releasedAt is { } released && DragStaging.Settle - (DateTime.UtcNow - released) is { Ticks: > 0 } wait)
        {
            // A managed wait, which keeps serving COM calls on this thread.
            changed.WaitOne(wait);
        }
        Task<string?> answer;
        lock (gate)
        {
            if (accepting is null && !DragStaging.TakesDrop(host.Ended(Id), host.StillHere(arrived)))
            {
                AppLog.Info($"drag {Id}: the cursor went back as the button came up; handing nothing over");
                WentBack = true;
                accepting = Task.FromResult<string?>(null);
                return false;
            }
            if (WentBack)
            {
                return false;
            }
            if (accepting is null)
            {
                try
                {
                    Directory.CreateDirectory(folder);
                    AppLog.Info($"drag {Id}: dropped; AcceptDrag into its staging folder");
                    accepting = host.AcceptAsync(Id, folder);
                }
                catch (Exception e) when (e is IOException or UnauthorizedAccessException)
                {
                    AppLog.Warn($"drag {Id}: cannot make a staging folder: {e.Message}");
                    accepting = Task.FromResult<string?>("invalid_folder");
                }
            }
            answer = accepting;
        }
        // A managed wait, which keeps serving COM calls on this thread.
        var code = answer.GetAwaiter().GetResult();
        if (code is null)
        {
            Accepted = true;
            return true;
        }
        if (Refusal is null)
        {
            Refusal = code;
            AppLog.Info($"drag {Id}: AcceptDrag refused: {code}");
        }
        return false;
    }

    public int GetDataHere(FormatEtc* format, StgMedium* medium) => Ole.E_NOTIMPL;

    public int QueryGetData(FormatEtc* format)
    {
        if (format is null)
        {
            return Ole.E_INVALIDARG;
        }
        var f = *format;
        bool known = (f.Format == DescriptorFormat && (f.Medium & Ole.TYMED_HGLOBAL) != 0)
            || (f.Format == ContentsFormat && (f.Medium & Ole.TYMED_ISTREAM) != 0)
            || (f.Format == PreferredEffectFormat && (f.Medium & Ole.TYMED_HGLOBAL) != 0);
        if (!known)
        {
            lock (gate)
            {
                known = stored.ContainsKey(f.Format) && (f.Medium & Ole.TYMED_HGLOBAL) != 0;
            }
        }
        return known ? Ole.S_OK : Ole.DV_E_FORMATETC;
    }

    public int GetCanonicalFormatEtc(FormatEtc* format, FormatEtc* result)
    {
        if (format is null || result is null)
        {
            return Ole.E_INVALIDARG;
        }
        *result = *format;
        result->Device = 0;
        return Ole.DATA_S_SAMEFORMATETC;
    }

    /// <summary>
    /// Keeps what the shell puts on the drag (its drag image's context,
    /// drop descriptions, the effect it performed), so it can read it back.
    /// </summary>
    public int SetData(FormatEtc* format, StgMedium* medium, int release)
    {
        if (format is null || medium is null)
        {
            return Ole.E_INVALIDARG;
        }
        if (medium->Medium != Ole.TYMED_HGLOBAL || medium->Handle == 0)
        {
            return Ole.E_NOTIMPL;
        }
        var bytes = Ole.FromGlobal(medium->Handle);
        lock (gate)
        {
            stored[format->Format] = bytes;
        }
        if (release != 0)
        {
            Ole.ReleaseStgMedium(medium);
        }
        return Ole.S_OK;
    }

    public int EnumFormatEtc(uint direction, nint* enumerator)
    {
        if (enumerator is null)
        {
            return Ole.E_INVALIDARG;
        }
        *enumerator = 0;
        if (direction != Ole.DATADIR_GET)
        {
            return Ole.E_NOTIMPL;
        }
        return PromisedFormats.Enumerate(PromisedFormats.Files().ToList(), enumerator);
    }

    public int DAdvise(FormatEtc* format, uint flags, nint sink, uint* connection) => Ole.OLE_E_ADVISENOTSUPPORTED;

    public int DUnadvise(uint connection) => Ole.OLE_E_ADVISENOTSUPPORTED;

    public int EnumDAdvise(nint* enumerator) => Ole.OLE_E_ADVISENOTSUPPORTED;

    // IDataObjectAsyncCapability

    public int SetAsyncMode(int doOpAsync)
    {
        asyncMode = doOpAsync != 0;
        return Ole.S_OK;
    }

    public int GetAsyncMode(int* isOpAsync)
    {
        if (isOpAsync is null)
        {
            return Ole.E_INVALIDARG;
        }
        *isOpAsync = asyncMode ? 1 : 0;
        return Ole.S_OK;
    }

    public int StartOperation(nint bindContext)
    {
        CopyingStarted = true;
        dropped = true;
        LastAsked = DateTime.UtcNow;
        AppLog.Info($"drag {Id}: the drop target copies on its own (on thread {Environment.CurrentManagedThreadId})");
        return Ole.S_OK;
    }

    public int InOperation(int* inAsyncOp)
    {
        if (inAsyncOp is null)
        {
            return Ole.E_INVALIDARG;
        }
        *inAsyncOp = CopyingStarted && !CopyingEnded ? 1 : 0;
        return Ole.S_OK;
    }

    public int EndOperation(int result, nint bindContext, uint effects)
    {
        CopyingEnded = true;
        AppLog.Info($"drag {Id}: the drop target finished copying (0x{result:X8}, effect {effects})");
        changed.Set();
        // Wakes the thread waiting for this.
        thread.Post(() => { });
        return Ole.S_OK;
    }
}

/// <summary>The background service behind a drag offered here.</summary>
internal sealed class AgentPromiseHost : IPromiseHost
{
    private readonly AgentModel model;
    private readonly Func<IReadOnlyList<PixelRect>> strips;
    private readonly Action<string, PromiseOutcome, string?> report;
    private readonly ConcurrentDictionary<string, bool> ended = new();

    public AgentPromiseHost(AgentModel model, Func<IReadOnlyList<PixelRect>> strips, Action<string, PromiseOutcome, string?> report)
    {
        this.model = model;
        this.strips = strips;
        this.report = report;
        model.Changed += (_, _) =>
        {
            if (model.Snapshot.DragFinish?.Ended is { } finish)
            {
                ended[finish.DragId] = true;
            }
            Changed?.Invoke();
        };
    }

    public event Action? Changed;

    public IReadOnlyList<PixelRect> Strips => strips();

    public async Task<string?> AcceptAsync(string dragId, string folder)
    {
        try
        {
            await model.AcceptDragAsync(dragId, folder).ConfigureAwait(false);
            return null;
        }
        catch (AgentRefusal refusal)
        {
            return refusal.Code;
        }
        catch (AgentUnavailableException)
        {
            return "unavailable";
        }
    }

    public Task DeclineAsync(string dragId) => model.DeclineDragAsync(dragId);

    public StagedBatch Batch(string dragId, int items) => DragStaging.Batch(model.Snapshot.Transfers, dragId, items);

    public bool Ended(string dragId) => ended.ContainsKey(dragId);

    public bool StillHere(DragArrived arrived) => DragStaging.StillHere(arrived, model.Snapshot);

    public void Report(string dragId, PromiseOutcome outcome, string? code) => report(dragId, outcome, code);
}

/// <summary>
/// For testing without another computer (<see cref="Launch.SimulateDragArrivedArgument"/>):
/// a drag of files on this computer offered as if from another one, whose
/// files "arrive" by being copied into the staging folder a moment after
/// the drop is accepted.
/// </summary>
internal sealed class LocalPromiseHost(IReadOnlyList<string> paths, Action<string, PromiseOutcome, string?> report) : IPromiseHost
{
    private static readonly TimeSpan Arriving = TimeSpan.FromSeconds(2);
    private readonly ConcurrentDictionary<string, StagedBatch> batches = new();

    public event Action? Changed;

    public IReadOnlyList<PixelRect> Strips => [];

    /// <summary>The offer: the paths' names, sizes and file counts.</summary>
    public DragArrived Offer(string fromDeviceId)
    {
        var offer = new DragArrived { DragId = $"local-{Guid.NewGuid():N}"[..14], FromDeviceId = fromDeviceId };
        foreach (var path in paths)
        {
            bool folder = Directory.Exists(path);
            var files = folder
                ? new DirectoryInfo(path).EnumerateFiles("*", SearchOption.AllDirectories).ToList()
                : [new FileInfo(path)];
            var item = new DraggedItem
            {
                Name = Path.GetFileName(Path.TrimEndingDirectorySeparator(path)),
                Folder = folder,
                Size = (ulong)files.Sum(f => f.Length),
                Files = (uint)files.Count,
            };
            offer.Items.Add(item);
            offer.Files += item.Files;
            offer.Bytes += item.Size;
        }
        return offer;
    }

    public async Task<string?> AcceptAsync(string dragId, string folder)
    {
        AppLog.Info($"simulated drag {dragId}: accepted; the files arrive in {Arriving.TotalSeconds:0} s");
        batches[dragId] = new StagedBatch([], false, []);
        _ = Task.Run(async () =>
        {
            await Task.Delay(Arriving).ConfigureAwait(false);
            var rows = new List<FileTransfer>();
            var arrived = new List<string>();
            foreach (var path in paths)
            {
                var name = Path.GetFileName(Path.TrimEndingDirectorySeparator(path));
                var to = Path.Combine(folder, name);
                if (Directory.Exists(path))
                {
                    CopyFolder(path, to);
                }
                else
                {
                    File.Copy(path, to);
                }
                arrived.Add(to);
                rows.Add(new FileTransfer { TransferId = name, DragId = dragId, Name = name, State = TransferState.Done, Path = to });
            }
            batches[dragId] = new StagedBatch(rows, true, arrived);
            Changed?.Invoke();
        });
        await Task.Yield();
        return null;
    }

    private static void CopyFolder(string from, string to)
    {
        Directory.CreateDirectory(to);
        foreach (var file in Directory.EnumerateFiles(from))
        {
            File.Copy(file, Path.Combine(to, Path.GetFileName(file)));
        }
        foreach (var folder in Directory.EnumerateDirectories(from))
        {
            CopyFolder(folder, Path.Combine(to, Path.GetFileName(folder)));
        }
    }

    public Task DeclineAsync(string dragId)
    {
        AppLog.Info($"simulated drag {dragId}: declined");
        return Task.CompletedTask;
    }

    public StagedBatch Batch(string dragId, int items) =>
        batches.TryGetValue(dragId, out var batch) ? batch : new StagedBatch([], false, []);

    public bool Ended(string dragId) => false;

    public bool StillHere(DragArrived arrived) => true;

    public void Report(string dragId, PromiseOutcome outcome, string? code) => report(dragId, outcome, code);
}
