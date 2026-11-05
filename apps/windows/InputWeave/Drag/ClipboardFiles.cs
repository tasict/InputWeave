using System.Collections.Concurrent;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.Marshalling;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Support;

namespace InputWeave.Drag;

/// <summary>What files on the clipboard need from the rest of the app; the background service, or a stand-in for testing.</summary>
internal interface IClipboardFilesHost
{
    /// <summary>Asks whether the files of a promise may still go on the clipboard; null when they may, else the refusal's code.</summary>
    Task<string?> PlaceAsync(ulong promiseId);

    /// <summary>Says whether the files of a promise are on the clipboard now.</summary>
    Task PlacedAsync(ulong promiseId, bool ok);

    /// <summary>Has the files of a promise fetched into <paramref name="folder"/>; null once asked, else the refusal's code.</summary>
    Task<string?> PasteAsync(ulong promiseId, string folder);

    /// <summary>Says the clipboard no longer holds the files of a promise, and no paste waits for them.</summary>
    Task GoneAsync(ulong promiseId);
}

/// <summary>
/// Files copied on another computer, on this computer's clipboard
/// (docs/architecture.md §9): the background service hands the app a
/// promise of them (<see cref="ClipboardFilesPromised"/>), which it puts on
/// the clipboard as a data object of its own (<see cref="ClipboardFilePromise"/>),
/// since only the process whose object it is can serve File Explorer while
/// it pastes them. It asks first, and leaves the clipboard alone when the
/// background service no longer waits for the promise or something was
/// copied since; promises are placed one at a time, in the order they came.
/// </summary>
/// <remarks>
/// The clipboard is set, cleared and watched on an OLE thread of its own,
/// which checks every half second whether the clipboard still holds the
/// promise and tells the background service once it does not. The data
/// object itself lives in the multithreaded apartment (<see cref="MtaHome"/>):
/// the OLE thread puts a proxy to it on the clipboard, so File Explorer's
/// calls, which wait for the files to arrive, run on COM's threads and never
/// hold up the OLE thread, which can put a newer promise on the clipboard
/// meanwhile. A promise copied over while a paste waits for its files stays
/// until they are there, so that paste still gets them; the folder they came
/// into goes some minutes after the promise ended and File Explorer finished
/// copying from it, and is tried again later if it cannot be removed then.
/// </remarks>
internal sealed class ClipboardFiles
{
    private const nuint WatchTimer = 0xC1B;

    /// <summary>How long the folder pasted files came into stays after the clipboard let go of them, and between attempts to remove it.</summary>
    private static readonly TimeSpan Grace = TimeSpan.FromMinutes(10);

    private readonly IClipboardFilesHost host;
    private readonly string root;
    private readonly OleThread thread = new("InputWeave clipboard");
    private readonly MtaHome home = new("InputWeave clipboard objects");
    private readonly ConcurrentDictionary<ulong, ClipboardFilePromise> promises = new();
    private readonly Placements placements = new();

    // On the OLE thread only.
    private ClipboardFilePromise? current;
    private nint currentPointer;
    private bool watching;
    /// <summary>Promises no longer on the clipboard whose paste still waits for the files.</summary>
    private readonly List<ClipboardFilePromise> lingering = [];
    private readonly List<(string Folder, ClipboardFilePromise Promise, DateTime At)> leaving = [];

    public ClipboardFiles(IClipboardFilesHost host, string root)
    {
        this.host = host;
        this.root = root;
    }

    /// <summary>Puts the files of <paramref name="promised"/> on the clipboard, if it may, after those before it; on any thread.</summary>
    public void Promise(ClipboardFilesPromised promised)
    {
        ulong id = promised.PromiseId;
        _ = placements.RunAsync(
            id,
            () => host.PlaceAsync(id),
            giveUp => OnOleThread(() => Place(promised), giveUp),
            ok => Report(() => host.PlacedAsync(id, ok), "placed"));
    }

    /// <summary>The files a paste asked for arrived, as far as they did; on any thread.</summary>
    public void Arrived(ClipboardFilesArrived arrived)
    {
        if (promises.TryGetValue(arrived.PromiseId, out var promise))
        {
            promise.Arrived(arrived.Paths);
            // One copied over meanwhile ends now that its paste has the files.
            thread.Post(() =>
            {
                if (lingering.Remove(promise))
                {
                    End(promise);
                }
            });
        }
    }

    /// <summary>The background service is gone, and with it what the promises had: the clipboard lets go of them.</summary>
    public void Withdraw() => thread.Post(() => Clear("the background service is gone"));

    /// <summary>The app is closing: the clipboard lets go of the promises, never rendering files that are not there.</summary>
    public void Close()
    {
        using var done = new ManualResetEventSlim();
        thread.Post(() =>
        {
            Clear("the app is closing");
            done.Set();
        });
        done.Wait(TimeSpan.FromSeconds(2));
        thread.Quit();
    }

    /// <summary>
    /// Removes folders left from pastes before, as by an app that ended
    /// meanwhile, made before <paramref name="startedUtc"/>; at launch.
    /// </summary>
    public static void ClearStaging(string root, DateTime startedUtc) => PromisedDrag.ClearStaging(root, startedUtc, "pastes");

    /// <summary>
    /// What <paramref name="work"/> returns on the OLE thread, or false when
    /// it throws or is given up on before it starts: the task always ends.
    /// </summary>
    private Task<bool> OnOleThread(Func<bool> work, CancellationToken giveUp)
    {
        var done = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        thread.Post(() =>
        {
            if (giveUp.IsCancellationRequested)
            {
                done.TrySetResult(false);
                return;
            }
            try
            {
                done.TrySetResult(work());
            }
            catch (Exception e)
            {
                AppLog.Warn($"clipboard files: putting them on the clipboard failed: {e.Message}");
                done.TrySetResult(false);
            }
        });
        return done.Task;
    }

    /// <summary>Puts the promise on the clipboard; on the OLE thread.</summary>
    private bool Place(ClipboardFilesPromised promised)
    {
        var promise = new ClipboardFilePromise(promised, host, root);
        nint pointer;
        try
        {
            pointer = home.Proxy<IDataObject>(promise);
        }
        catch (Exception e)
        {
            AppLog.Warn($"clipboard files {promised.PromiseId}: cannot hand them out: {e.Message}");
            return false;
        }
        int set = Ole.OleSetClipboard(pointer);
        if (set != Ole.S_OK)
        {
            AppLog.Warn($"clipboard files {promised.PromiseId}: cannot put them on the clipboard: 0x{set:X8}");
            Marshal.Release(pointer);
            return false;
        }
        // The one before is copied over by this one.
        LetGo("a newer promise replaced it");
        (current, currentPointer) = (promise, pointer);
        promises[promised.PromiseId] = promise;
        AppLog.Info($"clipboard files {promised.PromiseId}: {promised.Items.Count} items on the clipboard, with {promised.Inline.Count} other formats (from thread {Environment.CurrentManagedThreadId})");
        if (!watching)
        {
            watching = true;
            thread.Every(WatchTimer, 500, Watch);
        }
        return true;
    }

    private void Watch()
    {
        if (current is not null && Ole.OleIsCurrentClipboard(currentPointer) != Ole.S_OK)
        {
            LetGo("something else was copied over them");
        }
        var now = DateTime.UtcNow;
        foreach (var entry in leaving.Where(l => l.At <= now && !l.Promise.Copying).ToList())
        {
            leaving.Remove(entry);
            if (!Remove(entry.Folder))
            {
                leaving.Add(entry with { At = now + Grace });
            }
        }
    }

    /// <summary>
    /// The clipboard no longer holds the current promise: it ends, unless a
    /// paste still waits for its files, which then still come.
    /// </summary>
    private void LetGo(string why)
    {
        if (current is not { } promise)
        {
            return;
        }
        Marshal.Release(currentPointer);
        (current, currentPointer) = (null, 0);
        if (promise.AwaitingFiles)
        {
            AppLog.Info($"clipboard files {promise.Id}: {why}; kept until the paste waiting for them has them");
            lingering.Add(promise);
            return;
        }
        AppLog.Info($"clipboard files {promise.Id}: {why}");
        End(promise);
    }

    /// <summary>Takes the promise off the clipboard, if it is still there, and ends it and those kept for a paste.</summary>
    private void Clear(string why)
    {
        if (current is not null && Ole.OleIsCurrentClipboard(currentPointer) == Ole.S_OK)
        {
            // Empties the clipboard rather than flushing it: flushing would
            // ask for every file, which may not be here.
            Ole.OleSetClipboard(0);
            AppLog.Info($"clipboard files {current.Id}: taken off the clipboard ({why})");
        }
        if (current is { } promise)
        {
            Marshal.Release(currentPointer);
            (current, currentPointer) = (null, 0);
            End(promise);
        }
        foreach (var kept in lingering.ToList())
        {
            lingering.Remove(kept);
            End(kept);
        }
    }

    /// <summary>The promise is over: a paste still waiting stops, the background service hears, and its folder goes later.</summary>
    private void End(ClipboardFilePromise promise)
    {
        promises.TryRemove(promise.Id, out _);
        promise.Finish();
        _ = Report(() => host.GoneAsync(promise.Id), "gone");
        if (promise.Folder is { } folder)
        {
            leaving.Add((folder, promise, DateTime.UtcNow + Grace));
        }
    }

    /// <summary>Removes a folder pasted files came into; false when it is still there, as while something copies from it.</summary>
    private static bool Remove(string folder)
    {
        try
        {
            if (Directory.Exists(folder))
            {
                Directory.Delete(folder, recursive: true);
                AppLog.Info("removed the folder of files pasted earlier");
            }
            return true;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            AppLog.Warn($"cannot remove the folder of files pasted earlier, trying again later: {e.Message}");
            return false;
        }
    }

    private static async Task Report(Func<Task> say, string what)
    {
        try
        {
            await say().ConfigureAwait(false);
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException)
        {
            AppLog.Info($"clipboard files {what}: {e.Message}");
        }
    }
}

/// <summary>
/// A thread in the multithreaded apartment that COM objects are handed out
/// from, so that calls to them from other processes run on COM's own
/// threads rather than on a single-threaded apartment's: an object
/// marshaled here and unmarshaled on an STA gives that STA a proxy, and a
/// proxy marshaled on, as <c>OleSetClipboard</c> does, points its callers at
/// this apartment. The thread stays as long as the app runs, and with it the
/// apartment.
/// </summary>
internal sealed class MtaHome
{
    private readonly BlockingCollection<Action> work = [];

    public MtaHome(string name)
    {
        var thread = new Thread(Run) { Name = name, IsBackground = true };
        thread.SetApartmentState(ApartmentState.MTA);
        thread.Start();
    }

    /// <summary>
    /// A proxy, for the calling thread's apartment, to <paramref name="instance"/>
    /// as <typeparamref name="T"/> in this one, with a reference the caller
    /// releases. Waits a moment for this thread.
    /// </summary>
    public nint Proxy<T>(object instance)
    {
        var iid = typeof(T).GUID;
        nint stream = 0;
        int marshaled = Ole.E_UNEXPECTED;
        using var done = new ManualResetEventSlim();
        work.Add(() =>
        {
            var pointer = Ole.ComPointer<T>(instance);
            try
            {
                marshaled = Ole.CoMarshalInterThreadInterfaceInStream(in iid, pointer, out stream);
            }
            finally
            {
                Marshal.Release(pointer);
                done.Set();
            }
        });
        if (!done.Wait(TimeSpan.FromSeconds(5)))
        {
            throw new TimeoutException("the multithreaded apartment's thread does not answer");
        }
        Marshal.ThrowExceptionForHR(marshaled);
        Marshal.ThrowExceptionForHR(Ole.CoGetInterfaceAndReleaseStream(stream, in iid, out var proxy));
        return proxy;
    }

    private void Run()
    {
        Ole.CoInitializeEx(0, Ole.COINIT_MULTITHREADED);
        foreach (var action in work.GetConsumingEnumerable())
        {
            try
            {
                action();
            }
            catch (Exception e)
            {
                AppLog.Warn($"handing out a COM object failed: {e}");
            }
        }
    }
}

/// <summary>
/// Files copied on another computer, promised on the clipboard as File
/// Explorer takes files that do not exist yet: the descriptor of every file
/// and folder, and each file's contents, read on a thread of its own when it
/// copies asynchronously (<see cref="IDataObjectAsyncCapability"/>). The
/// first request for the descriptor fetches the files into a folder of the
/// app's and waits for them, for as long as they come when the paste is
/// asynchronous and at most <see cref="SyncLimit"/> otherwise; the
/// descriptor then lists all of it, and pasting again gets the same. The
/// clipboard's other content that came along is there too, ready as the
/// background service converted it, and the formats that mark the object
/// (<see cref="Markers"/>).
/// </summary>
[GeneratedComClass]
internal sealed unsafe partial class ClipboardFilePromise : IDataObject, IDataObjectAsyncCapability
{
    /// <summary>How long an app pasting without copying on its own time waits for the files.</summary>
    private static readonly TimeSpan SyncLimit = TimeSpan.FromSeconds(60);

    /// <summary>
    /// Formats that mark the object: InputWeave's own, so the background
    /// service does not take it for a copy, and those that tell clipboard
    /// managers, Windows' clipboard history and its cloud clipboard to leave
    /// it alone, as macOS's TransientType does: files that are not here
    /// yet, which a paste from a history later could not get.
    /// </summary>
    private static readonly Dictionary<ushort, byte[]> Markers = new()
    {
        [(ushort)Ole.RegisterClipboardFormat("InputWeave")] = BitConverter.GetBytes(1u),
        [(ushort)Ole.RegisterClipboardFormat("ExcludeClipboardContentFromMonitorProcessing")] = BitConverter.GetBytes(0u),
        [(ushort)Ole.RegisterClipboardFormat("CanIncludeInClipboardHistory")] = BitConverter.GetBytes(0u),
        [(ushort)Ole.RegisterClipboardFormat("CanUploadToCloudClipboard")] = BitConverter.GetBytes(0u),
    };

    private readonly ClipboardFilesPromised promised;
    private readonly IClipboardFilesHost host;
    private readonly string root;
    private readonly Dictionary<ushort, byte[]> inline = [];
    private readonly object gate = new();
    private readonly ManualResetEvent arrived = new(false);
    private readonly Dictionary<ushort, byte[]> stored = [];
    private Task<string?>? pasting;
    private IReadOnlyList<string>? paths;
    private IReadOnlyList<PromisedEntry>? staged;
    // Set and read on COM's threads, as File Explorer calls.
    private volatile bool asyncMode = true;
    private volatile bool copyingStarted;
    private volatile bool copyingEnded;
    private bool finished;
    private bool loggedRead;

    public ClipboardFilePromise(ClipboardFilesPromised promised, IClipboardFilesHost host, string root)
    {
        this.promised = promised;
        this.host = host;
        this.root = root;
        // Ready for the clipboard as they come: the background service
        // converts them as the broker writes them.
        foreach (var part in promised.Inline)
        {
            ushort format = part.Standard != 0
                ? (ushort)part.Standard
                : (ushort)Ole.RegisterClipboardFormat(part.Name);
            if (format != 0 && !Markers.ContainsKey(format))
            {
                inline[format] = part.Data.ToByteArray();
            }
        }
    }

    public ulong Id => promised.PromiseId;

    /// <summary>Where the files were fetched into, once a paste asked for them.</summary>
    public string? Folder { get; private set; }

    /// <summary>Whether a paste asked for the files, which have not come yet.</summary>
    public bool AwaitingFiles
    {
        get
        {
            lock (gate)
            {
                return pasting is not null && paths is null && staged is null && !finished;
            }
        }
    }

    /// <summary>Whether File Explorer copies from the folder on its own time now.</summary>
    public bool Copying => copyingStarted && !copyingEnded;

    /// <summary>The files a paste asked for arrived, as far as they did: where each item is now, or empty.</summary>
    public void Arrived(IEnumerable<string> where)
    {
        lock (gate)
        {
            paths ??= where.ToList();
        }
        arrived.Set();
    }

    /// <summary>The clipboard let go of it: a paste still waiting stops.</summary>
    public void Finish()
    {
        lock (gate)
        {
            finished = true;
        }
        arrived.Set();
    }

    // IDataObject

    public int GetData(FormatEtc* format, StgMedium* medium)
    {
        if (format is null || medium is null)
        {
            return Ole.E_INVALIDARG;
        }
        *medium = default;
        var f = *format;
        try
        {
            if (f.Format == PromisedFormats.Descriptor && (f.Medium & Ole.TYMED_HGLOBAL) != 0)
            {
                return PromisedFormats.Global(FileDescriptors.Encode(Staged()), medium);
            }
            if (f.Format == PromisedFormats.Contents && (f.Medium & Ole.TYMED_ISTREAM) != 0)
            {
                return Contents(f.Index, medium);
            }
            if ((f.Medium & Ole.TYMED_HGLOBAL) == 0)
            {
                return Ole.DV_E_FORMATETC;
            }
            if (f.Format == PromisedFormats.PreferredEffect)
            {
                return PromisedFormats.Global(BitConverter.GetBytes(Ole.DROPEFFECT_COPY), medium);
            }
            if (Markers.TryGetValue(f.Format, out var marker))
            {
                return PromisedFormats.Global(marker, medium);
            }
            if (inline.TryGetValue(f.Format, out var data))
            {
                return PromisedFormats.Global(data, medium);
            }
            byte[]? kept;
            lock (gate)
            {
                stored.TryGetValue(f.Format, out kept);
            }
            return kept is null ? Ole.DV_E_FORMATETC : PromisedFormats.Global(kept, medium);
        }
        catch (Exception e)
        {
            AppLog.Warn($"clipboard files {Id}: GetData failed: {e.Message}");
            return Ole.E_UNEXPECTED;
        }
    }

    private int Contents(int index, StgMedium* medium)
    {
        var entries = Staged();
        if (index < 0 || index >= entries.Count || entries[index].IsFolder || entries[index].Source is not { } source)
        {
            return Ole.DV_E_LINDEX;
        }
        int opened = PromisedFormats.Stream(source, medium);
        if (opened != Ole.S_OK)
        {
            AppLog.Warn($"clipboard files {Id}: cannot open pasted file {index}: 0x{opened:X8}");
            return opened;
        }
        if (!loggedRead)
        {
            loggedRead = true;
            AppLog.Info($"clipboard files {Id}: the paste reads the files (on thread {Environment.CurrentManagedThreadId})");
        }
        return Ole.S_OK;
    }

    /// <summary>
    /// What arrived: the first call has the files fetched into a folder of
    /// the app's and waits for them; the others wait for the same answer,
    /// or get it at once once it came. Nothing while they cannot be had.
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
        if (!Paste())
        {
            return [];
        }
        var started = DateTime.UtcNow;
        while (true)
        {
            lock (gate)
            {
                if (paths is not null || finished)
                {
                    break;
                }
            }
            bool patient = asyncMode && copyingStarted;
            if (!patient && DateTime.UtcNow - started > SyncLimit)
            {
                AppLog.Warn($"clipboard files {Id}: nothing arrived within {SyncLimit.TotalSeconds:0} s for a paste that waits; handing over nothing this time");
                return [];
            }
            // A managed wait, which keeps serving COM calls on this thread.
            arrived.WaitOne(TimeSpan.FromSeconds(1));
        }
        IReadOnlyList<string> where;
        lock (gate)
        {
            where = paths ?? [];
        }
        var entries = FileDescriptors.Staged(where.Where(p => p.Length != 0), out int tooLong);
        AppLog.Info(
            $"clipboard files {Id}: {where.Count(p => p.Length != 0)} of {promised.Items.Count} items arrived in {(DateTime.UtcNow - started).TotalSeconds:0.0} s; "
            + $"{entries.Count(e => !e.IsFolder)} files and {entries.Count(e => e.IsFolder)} folders to hand over"
            + (tooLong == 0 ? "" : $", {tooLong} paths too long to hand over"));
        lock (gate)
        {
            return staged ??= entries;
        }
    }

    /// <summary>Asks for the files, once, into a folder made for them; false when that was refused.</summary>
    private bool Paste()
    {
        Task<string?> answer;
        lock (gate)
        {
            if (pasting is null)
            {
                try
                {
                    var folder = PasteFolders.For(root, Id);
                    Directory.CreateDirectory(folder);
                    Folder = folder;
                    AppLog.Info($"clipboard files {Id}: pasted; fetching them into a folder of the app's");
                    pasting = host.PasteAsync(Id, folder);
                }
                catch (Exception e) when (e is IOException or UnauthorizedAccessException)
                {
                    AppLog.Warn($"clipboard files {Id}: cannot make a folder for them: {e.Message}");
                    pasting = Task.FromResult<string?>("invalid_folder");
                }
            }
            answer = pasting;
        }
        // A managed wait, which keeps serving COM calls on this thread.
        var code = answer.GetAwaiter().GetResult();
        if (code is null)
        {
            return true;
        }
        AppLog.Info($"clipboard files {Id}: the background service refused the paste: {code}");
        lock (gate)
        {
            staged ??= [];
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
        bool known = (f.Format == PromisedFormats.Descriptor && (f.Medium & Ole.TYMED_HGLOBAL) != 0)
            || (f.Format == PromisedFormats.Contents && (f.Medium & Ole.TYMED_ISTREAM) != 0)
            || ((f.Format == PromisedFormats.PreferredEffect || Markers.ContainsKey(f.Format) || inline.ContainsKey(f.Format))
                && (f.Medium & Ole.TYMED_HGLOBAL) != 0);
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

    /// <summary>Keeps what the shell puts on it as it pastes (the effect it performed and the like).</summary>
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
        var formats = PromisedFormats.Files().ToList();
        formats.AddRange(Markers.Keys.Select(f => new FormatEtc { Format = f, Aspect = Ole.DVASPECT_CONTENT, Index = -1, Medium = Ole.TYMED_HGLOBAL }));
        formats.AddRange(inline.Keys.Select(f => new FormatEtc { Format = f, Aspect = Ole.DVASPECT_CONTENT, Index = -1, Medium = Ole.TYMED_HGLOBAL }));
        return PromisedFormats.Enumerate(formats, enumerator);
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
        copyingStarted = true;
        AppLog.Info($"clipboard files {Id}: the paste copies on its own (on thread {Environment.CurrentManagedThreadId})");
        return Ole.S_OK;
    }

    public int InOperation(int* inAsyncOp)
    {
        if (inAsyncOp is null)
        {
            return Ole.E_INVALIDARG;
        }
        *inAsyncOp = copyingStarted && !copyingEnded ? 1 : 0;
        return Ole.S_OK;
    }

    public int EndOperation(int result, nint bindContext, uint effects)
    {
        copyingEnded = true;
        AppLog.Info($"clipboard files {Id}: the paste finished copying (0x{result:X8}, effect {effects})");
        return Ole.S_OK;
    }
}

/// <summary>The background service behind files on the clipboard.</summary>
internal sealed class AgentClipboardHost(AgentModel model) : IClipboardFilesHost
{
    public async Task<string?> PlaceAsync(ulong promiseId)
    {
        try
        {
            await model.ClipboardFilesPlaceAsync(promiseId).ConfigureAwait(false);
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

    public Task PlacedAsync(ulong promiseId, bool ok) => model.ClipboardFilesPlacedAsync(promiseId, ok);

    public async Task<string?> PasteAsync(ulong promiseId, string folder)
    {
        try
        {
            await model.PasteClipboardFilesAsync(promiseId, folder).ConfigureAwait(false);
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

    public Task GoneAsync(ulong promiseId) => model.ClipboardFilesGoneAsync(promiseId);
}

/// <summary>
/// For testing without another computer (<see cref="Launch.SimulateClipboardFilesArgument"/>):
/// files on this computer put on the clipboard as if copied on another one,
/// whose files "arrive" by being copied into the folder a paste asks for,
/// <paramref name="arriving"/> later. With <paramref name="recopy"/>, the
/// same files are put on the clipboard again, as a new promise, that long
/// after a paste first asks for them.
/// </summary>
internal sealed class LocalClipboardHost(IReadOnlyList<string> paths, TimeSpan arriving, TimeSpan? recopy) : IClipboardFilesHost
{
    private int recopied;

    public ClipboardFiles? Files { get; set; }

    public Task<string?> PlaceAsync(ulong promiseId)
    {
        AppLog.Info($"simulated clipboard files {promiseId}: may be placed");
        return Task.FromResult<string?>(null);
    }

    /// <summary>The promise: the paths' names, and their names as text.</summary>
    public ClipboardFilesPromised Promise(ulong id)
    {
        var promised = new ClipboardFilesPromised { PromiseId = id, FromDeviceId = "local" };
        var names = paths.Select(p => Path.GetFileName(Path.TrimEndingDirectorySeparator(p))).ToList();
        promised.Items.AddRange(paths.Zip(names, (p, n) => new DraggedItem { Name = n, Folder = Directory.Exists(p) }));
        // The names as text, as the background service would send them:
        // CF_UNICODETEXT with CRLF line ends.
        promised.Inline.Add(new ClipboardInline
        {
            Standard = Ole.CF_UNICODETEXT,
            Data = Google.Protobuf.ByteString.CopyFrom(System.Text.Encoding.Unicode.GetBytes(string.Join("\r\n", names) + "\0")),
        });
        return promised;
    }

    public Task PlacedAsync(ulong promiseId, bool ok)
    {
        AppLog.Info($"simulated clipboard files {promiseId}: placed {ok}");
        return Task.CompletedTask;
    }

    public async Task<string?> PasteAsync(ulong promiseId, string folder)
    {
        AppLog.Info($"simulated clipboard files {promiseId}: pasted; the files arrive in {arriving.TotalSeconds:0.#} s");
        if (recopy is { } after && Interlocked.Exchange(ref recopied, 1) == 0)
        {
            _ = Task.Run(async () =>
            {
                await Task.Delay(after).ConfigureAwait(false);
                AppLog.Info($"simulated clipboard files: copied again, {after.TotalSeconds:0.#} s after the paste");
                Files?.Promise(Promise(promiseId + 1));
            });
        }
        _ = Task.Run(async () =>
        {
            await Task.Delay(arriving).ConfigureAwait(false);
            var arrived = new ClipboardFilesArrived { PromiseId = promiseId, Ok = true };
            foreach (var path in paths)
            {
                var to = Path.Combine(folder, Path.GetFileName(Path.TrimEndingDirectorySeparator(path)));
                if (Directory.Exists(path))
                {
                    CopyFolder(path, to);
                }
                else
                {
                    File.Copy(path, to);
                }
                arrived.Paths.Add(to);
            }
            Files?.Arrived(arrived);
        });
        await Task.Yield();
        return null;
    }

    public Task GoneAsync(ulong promiseId)
    {
        AppLog.Info($"simulated clipboard files {promiseId}: gone from the clipboard");
        return Task.CompletedTask;
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
}

