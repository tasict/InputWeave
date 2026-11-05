using System.Collections.Concurrent;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.Marshalling;
using InputWeave.Agent;
using InputWeave.Overlay;
using InputWeave.Support;

namespace InputWeave.Drag;

/// <summary>
/// The source side of dragging files to another computer
/// (docs/architecture.md §6.4, point 7): an
/// invisible strip over every part of this computer's screen edges the
/// cursor can leave by (<see cref="DragStrips"/>), which takes drags of
/// files. When a drag of files enters one, the app tells the background
/// service (BeginDrag), which lists them and lets the cursor cross with the
/// button held. The service lets go of the button where the cursor leaves,
/// so the drag ends with a drop on the strip, which copies nothing; the
/// files go once the other computer takes them. A drag that leaves the
/// strips without crossing ends (EndDrag).
/// </summary>
/// <remarks>
/// A strip is a layered window at 1/255 opacity, invisible but solid, above
/// all others. A window that answers HTTRANSPARENT to hit tests only lets
/// the pointer through to windows of its own thread, so the strip would
/// take clicks meant for other apps at the edges of the screen. Instead it
/// is also WS_EX_TRANSPARENT, which passes the pointer through to whatever
/// is below, except while a mouse button is down that went down outside the
/// strips (<see cref="StripPress"/>): then the strip is solid, so a drag's
/// OLE loop, which looks for the window under the cursor on every move,
/// finds it. A press always lands below the strip, and a drag of anything
/// other than files (a window, a selection) has the pointer captured and
/// never looks. A press inside a strip, as on the taskbar under it, keeps
/// the strips clear until the button is up, so what was pressed gets the
/// button up as well.
/// </remarks>
internal sealed partial class SeamStrips
{
    private const string ClassName = "InputWeave.DragStrip";
    private const nuint PollTimer = 1;
    /// <summary>How often the buttons are checked, to make the strips solid during a drag.</summary>
    private const uint PollMilliseconds = 30;
    /// <summary>Out of the strips this long without crossing, the drag ends here.</summary>
    private static readonly TimeSpan LeaveGrace = TimeSpan.FromMilliseconds(300);

    private readonly OleThread thread;
    private readonly AgentModel model;
    private readonly StripTarget dropTarget;
    private readonly List<nint> windows = [];
    private IReadOnlyList<PixelRect> shownRects = [];
    private nint target;
    private bool solid;
    private readonly StripPress press = new();
    private Session? session;

    public SeamStrips(AgentModel model)
    {
        this.model = model;
        dropTarget = new StripTarget(this);
        thread = new OleThread("InputWeave drag strips");
        thread.Post(Start);
    }

    /// <summary>Puts the strips at <paramref name="rects"/>, in pixels; none takes them down. From any thread.</summary>
    public void Show(IReadOnlyList<PixelRect> rects)
    {
        var copy = rects.ToList();
        thread.Post(() => Apply(copy));
    }

    /// <summary>For testing: a drag of these files entering a strip, and leaving it after <paramref name="stay"/>.</summary>
    public void Simulate(IReadOnlyList<string> paths, TimeSpan stay)
    {
        thread.Post(() =>
        {
            AppLog.Info($"simulating a drag of {paths.Count} items entering a strip");
            Enter(paths, Ole.DROPEFFECT_COPY);
        });
        _ = Task.Delay(stay).ContinueWith(_ => thread.Post(() =>
        {
            AppLog.Info("simulating the drag leaving the strip");
            Leave();
        }), TaskScheduler.Default);
    }

    private unsafe void Start()
    {
        Win32.EnsureClass(ClassName, &Procedure);
        target = Ole.ComPointer<IDropTarget>(dropTarget);
        thread.Every(PollTimer, PollMilliseconds, Poll);
    }

    private void Apply(IReadOnlyList<PixelRect> rects)
    {
        if (rects.SequenceEqual(shownRects))
        {
            return;
        }
        while (windows.Count > rects.Count)
        {
            var hwnd = windows[^1];
            windows.RemoveAt(windows.Count - 1);
            Ole.RevokeDragDrop(hwnd);
            Win32.DestroyWindow(hwnd);
        }
        while (windows.Count < rects.Count && Create() is var hwnd && hwnd != 0)
        {
            windows.Add(hwnd);
        }
        for (int i = 0; i < windows.Count; i++)
        {
            var r = rects[i];
            Win32.SetWindowPos(windows[i], Win32.HWND_TOPMOST, r.X, r.Y, r.Width, r.Height, Win32.SWP_NOACTIVATE | Win32.SWP_SHOWWINDOW);
        }
        shownRects = rects;
        AppLog.Info(rects.Count == 0
            ? "drag strips: none"
            : $"drag strips: {string.Join(", ", rects.Select(r => $"{r.Width}x{r.Height}@{r.X},{r.Y}"))}");
    }

    private nint Create()
    {
        var hwnd = Win32.CreateWindow(
            Win32.WS_EX_LAYERED | Win32.WS_EX_TRANSPARENT | Win32.WS_EX_TOOLWINDOW | Win32.WS_EX_NOACTIVATE | Win32.WS_EX_TOPMOST,
            ClassName, "InputWeave", Win32.WS_POPUP, 0, 0, 1, 1, 0, 0, Win32.GetModuleHandle(0), 0);
        if (hwnd == 0)
        {
            AppLog.Warn($"cannot create a drag strip: {Marshal.GetLastPInvokeErrorMessage()}");
            return 0;
        }
        // Invisible, yet solid wherever it is, once it is not WS_EX_TRANSPARENT.
        Win32.SetLayeredWindowAttributes(hwnd, 0, 1, Win32.LWA_ALPHA);
        int registered = Ole.RegisterDragDrop(hwnd, target);
        if (registered != Ole.S_OK)
        {
            AppLog.Warn($"cannot take drags on a strip: 0x{registered:X8}");
        }
        SetSolid(hwnd, solid);
        return hwnd;
    }

    private static void SetSolid(nint hwnd, bool solid)
    {
        var style = (uint)Win32.GetWindowLongPtr(hwnd, Win32.GWL_EXSTYLE);
        var next = solid ? style & ~Win32.WS_EX_TRANSPARENT : style | Win32.WS_EX_TRANSPARENT;
        if (next != style)
        {
            Win32.SetWindowLongPtr(hwnd, Win32.GWL_EXSTYLE, (nint)next);
        }
    }

    /// <summary>
    /// Solid while a button that went down outside the strips is down;
    /// ends a drag that left the strips without crossing.
    /// </summary>
    private void Poll()
    {
        var (x, y) = Native.CursorPosition();
        bool down = press.Solid(Win32.AnyButtonDown(), new PixelPoint(x, y), shownRects);
        if (down != solid)
        {
            solid = down;
            foreach (var hwnd in windows)
            {
                SetSolid(hwnd, down);
                if (down)
                {
                    // Above whatever topmost window came since.
                    Win32.SetWindowPos(hwnd, Win32.HWND_TOPMOST, 0, 0, 0, 0, Win32.SWP_NOMOVE | Win32.SWP_NOSIZE | Win32.SWP_NOACTIVATE);
                }
            }
        }
        if (session is { Over: false, Dropped: false, LeftAt: { } left } gone && DateTime.UtcNow - left >= LeaveGrace)
        {
            string? dragId;
            lock (gone)
            {
                if (gone.Pending)
                {
                    // Answered later; ended then.
                    return;
                }
                dragId = gone.DragId;
            }
            session = null;
            // The drag the service began, by its answer: the status naming
            // it may come later. Once the cursor took it across, the
            // service ends it.
            if (StripLeave.Ends(dragId, model.Snapshot.Status.Dragging))
            {
                AppLog.Info($"drag {dragId} left the strips without crossing; ending it");
                _ = EndAsync(dragId!);
            }
        }
    }

    private async Task EndAsync(string dragId)
    {
        try
        {
            await model.EndDragAsync(dragId).ConfigureAwait(false);
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException)
        {
            AppLog.Info($"end drag {dragId}: {e.Message}");
        }
    }

    // The drag over the strips. All on the strip thread, but for the
    // answer to BeginDrag, which comes on a thread of the pool.

    private sealed class Session(IReadOnlyList<string> paths)
    {
        public IReadOnlyList<string> Paths { get; } = paths;
        public bool Pending { get; set; } = true;
        public string? DragId { get; set; }
        public string? Refused { get; set; }
        public bool Over { get; set; } = true;
        public bool Dropped { get; set; }
        public DateTime? LeftAt { get; set; }

        public bool Takes
        {
            get
            {
                lock (this)
                {
                    return Refused is null;
                }
            }
        }
    }

    private uint Enter(IReadOnlyList<string>? paths, uint allowed, PointL? at = null)
    {
        if (paths is null || paths.Count == 0 || (allowed & Ole.DROPEFFECT_COPY) == 0)
        {
            // Not files, or files that may not be copied.
            return Ole.DROPEFFECT_NONE;
        }
        if (session is { Dropped: false } same && same.Paths.SequenceEqual(paths))
        {
            // From one strip to the next, or back after a moment: the same drag.
            same.Over = true;
            same.LeftAt = null;
            return same.Takes ? Ole.DROPEFFECT_COPY : Ole.DROPEFFECT_NONE;
        }
        var begun = new Session(paths);
        session = begun;
        // Folders are listed by the service, through the broker: send now,
        // well before the cursor meets the edge.
        var items = paths.Select(p => (p, Directory.Exists(p))).ToList();
        AppLog.Info(
            $"a drag of {items.Count} items entered a strip{(at is { } p ? $" at {p.X},{p.Y}" : "")} ({items.Count(i => i.Item2)} folders); BeginDrag");
        _ = BeginAsync(begun, items);
        return Ole.DROPEFFECT_COPY;
    }

    private async Task BeginAsync(Session begun, List<(string, bool)> items)
    {
        try
        {
            var answer = await model.BeginDragAsync(items).ConfigureAwait(false);
            lock (begun)
            {
                begun.DragId = answer.DragId;
            }
            AppLog.Info(
                $"DragBegun {answer.DragId}: {answer.Items} items, {answer.Files} files, {answer.Bytes} bytes, {answer.Failures.Count} left out");
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException)
        {
            lock (begun)
            {
                begun.Refused = e is AgentRefusal refusal ? refusal.Code : "unavailable";
            }
            AppLog.Info($"BeginDrag refused: {e.Message}");
        }
        finally
        {
            lock (begun)
            {
                begun.Pending = false;
            }
        }
    }

    private uint Over(uint allowed) =>
        session is { } over && over.Takes && (allowed & Ole.DROPEFFECT_COPY) != 0 ? Ole.DROPEFFECT_COPY : Ole.DROPEFFECT_NONE;

    private void Leave()
    {
        if (session is { } left)
        {
            left.Over = false;
            left.LeftAt = DateTime.UtcNow;
        }
    }

    private uint Dropped(uint allowed)
    {
        if (session is not { } dropped)
        {
            return Ole.DROPEFFECT_NONE;
        }
        dropped.Over = false;
        dropped.Dropped = true;
        AppLog.Info($"the drag {dropped.DragId ?? "(not begun)"} was dropped on a strip");
        // Copy, never move: the files stay where they are, and go once the
        // other computer takes them.
        return dropped.Takes && (allowed & Ole.DROPEFFECT_COPY) != 0 ? Ole.DROPEFFECT_COPY : Ole.DROPEFFECT_NONE;
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvStdcall)])]
    private static nint Procedure(nint hwnd, uint message, nint wParam, nint lParam) =>
        message == Win32.WM_MOUSEACTIVATE ? Win32.MA_NOACTIVATE : Win32.DefWindowProc(hwnd, message, wParam, lParam);

    /// <summary>The strips' IDropTarget, one for all of them.</summary>
    [GeneratedComClass]
    internal sealed unsafe partial class StripTarget(SeamStrips owner) : IDropTarget
    {
        public int DragEnter(nint data, uint keys, PointL point, uint* effect)
        {
            uint allowed = *effect;
            *effect = owner.Enter(Ole.DroppedPaths(data), allowed, point);
            return Ole.S_OK;
        }

        public int DragOver(uint keys, PointL point, uint* effect)
        {
            *effect = owner.Over(*effect);
            return Ole.S_OK;
        }

        public int DragLeave()
        {
            owner.Leave();
            return Ole.S_OK;
        }

        public int Drop(nint data, uint keys, PointL point, uint* effect)
        {
            *effect = owner.Dropped(*effect);
            return Ole.S_OK;
        }
    }
}
