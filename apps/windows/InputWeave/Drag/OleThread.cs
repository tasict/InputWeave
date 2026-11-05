using System.Collections.Concurrent;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using InputWeave.Support;

namespace InputWeave.Drag;

/// <summary>
/// A thread for OLE drag and drop, apart from WinUI's: single-threaded
/// apartment, OLE initialized, pumping messages, so drop targets it
/// registers get their calls and a drag it starts runs its modal loop
/// without holding up the panel or the HUD. Work is handed to it through a
/// message-only window, which also takes its timers; messages posted to a
/// thread rather than a window would be lost in a drag's modal loop.
/// </summary>
internal sealed unsafe class OleThread
{
    private const string ClassName = "InputWeave.OleThread";
    private const uint WorkMessage = Win32.WM_APP + 1;

    private static readonly ConcurrentDictionary<nint, OleThread> Threads = new();

    private readonly ConcurrentQueue<Action> work = new();
    private readonly TaskCompletionSource started = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly Thread thread;
    private readonly Dictionary<nuint, Action> timers = [];
    private nint window;
    private bool quitting;

    public OleThread(string name)
    {
        thread = new Thread(Run) { Name = name, IsBackground = true };
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        started.Task.Wait();
    }

    /// <summary>Whether the caller is on this thread.</summary>
    public bool IsCurrent => Environment.CurrentManagedThreadId == thread.ManagedThreadId;

    /// <summary>Runs <paramref name="action"/> on this thread, in order with the rest.</summary>
    public void Post(Action action)
    {
        work.Enqueue(action);
        if (window != 0)
        {
            Win32.PostMessage(window, WorkMessage, 0, 0);
        }
    }

    /// <summary>Calls <paramref name="tick"/> on this thread every <paramref name="milliseconds"/>; call on this thread.</summary>
    public void Every(nuint id, uint milliseconds, Action tick)
    {
        timers[id] = tick;
        Win32.SetTimer(window, id, milliseconds, 0);
    }

    public void StopTimer(nuint id)
    {
        timers.Remove(id);
        Win32.KillTimer(window, id);
    }

    /// <summary>Ends the thread's message loop once the work queued so far has run.</summary>
    public void Quit() => Post(() =>
    {
        quitting = true;
        Win32.PostQuitMessage(0);
    });

    /// <summary>
    /// Pumps messages on this thread until <paramref name="done"/> says so,
    /// checking whenever a message or work arrives and at least every
    /// <paramref name="poll"/>; for waiting on a drop target that copies on
    /// its own time, whose calls arrive as messages.
    /// </summary>
    public void PumpUntil(Func<bool> done, TimeSpan poll, TimeSpan? limit = null)
    {
        const nuint wake = 0xF00D;
        var deadline = limit is { } most ? DateTime.UtcNow + most : DateTime.MaxValue;
        Every(wake, (uint)poll.TotalMilliseconds, () => { });
        try
        {
            var message = new Win32.Message();
            while (!done() && !quitting && DateTime.UtcNow < deadline)
            {
                int got = Win32.GetMessage(&message, 0, 0, 0);
                if (got <= 0)
                {
                    // For the thread's own loop too.
                    quitting = true;
                    Win32.PostQuitMessage(0);
                    break;
                }
                Win32.TranslateMessage(&message);
                Win32.DispatchMessage(&message);
            }
        }
        finally
        {
            StopTimer(wake);
        }
    }

    private void Run()
    {
        int initialized = Ole.OleInitialize(0);
        try
        {
            Win32.EnsureClass(ClassName, &Procedure);
            window = Win32.CreateWindow(0, ClassName, "", 0, 0, 0, 0, 0, Win32.HWND_MESSAGE, 0, Win32.GetModuleHandle(0), 0);
            Threads[window] = this;
            started.TrySetResult();
            var message = new Win32.Message();
            while (Win32.GetMessage(&message, 0, 0, 0) > 0)
            {
                Win32.TranslateMessage(&message);
                Win32.DispatchMessage(&message);
            }
        }
        catch (Exception e)
        {
            AppLog.Warn($"the drag thread {thread.Name} failed: {e}");
            started.TrySetResult();
        }
        finally
        {
            Threads.TryRemove(window, out _);
            if (window != 0)
            {
                Win32.DestroyWindow(window);
            }
            if (initialized >= 0)
            {
                Ole.OleUninitialize();
            }
        }
    }

    private void RunWork()
    {
        while (work.TryDequeue(out var action))
        {
            try
            {
                action();
            }
            catch (Exception e)
            {
                AppLog.Warn($"drag work on {thread.Name} failed: {e}");
            }
        }
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvStdcall)])]
    private static nint Procedure(nint hwnd, uint message, nint wParam, nint lParam)
    {
        if (Threads.TryGetValue(hwnd, out var owner))
        {
            switch (message)
            {
                case WorkMessage:
                    owner.RunWork();
                    return 0;
                case Win32.WM_TIMER when owner.timers.TryGetValue((nuint)wParam, out var tick):
                    try
                    {
                        tick();
                    }
                    catch (Exception e)
                    {
                        AppLog.Warn($"drag timer on {owner.thread.Name} failed: {e}");
                    }
                    return 0;
            }
        }
        return Win32.DefWindowProc(hwnd, message, wParam, lParam);
    }
}
