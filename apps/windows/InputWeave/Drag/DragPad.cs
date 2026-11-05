using System.Collections.Concurrent;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using InputWeave.Support;

namespace InputWeave.Drag;

/// <summary>
/// An invisible window under the cursor, on the thread of a drag offered
/// here, that takes the press the drag starts from: OLE's drag loop only
/// hears the mouse while the button was pressed on a window of its own
/// thread. Layered at 1/255 opacity, so it is invisible yet takes the
/// press; it never takes the focus, and goes away once the drag is under
/// way, before it could stand between the cursor and where the files go.
/// </summary>
internal sealed unsafe class DragPad
{
    private const string ClassName = "InputWeave.DragPad";
    /// <summary>Its size, in pixels: room for the cursor to move a little before the press is made again.</summary>
    private const int Size = 240;

    private static readonly ConcurrentDictionary<nint, DragPad> Pads = new();
    private nint hwnd;

    /// <summary>Whether the press landed on it.</summary>
    public bool Pressed { get; private set; }

    /// <summary>Shows it centered on the cursor, above everything; on the drag's thread.</summary>
    public void ShowAtCursor()
    {
        Win32.EnsureClass(ClassName, &Procedure);
        if (hwnd == 0)
        {
            hwnd = Win32.CreateWindow(
                Win32.WS_EX_LAYERED | Win32.WS_EX_TOOLWINDOW | Win32.WS_EX_NOACTIVATE | Win32.WS_EX_TOPMOST,
                ClassName, "InputWeave", Win32.WS_POPUP, 0, 0, 1, 1, 0, 0, Win32.GetModuleHandle(0), 0);
            if (hwnd == 0)
            {
                AppLog.Warn($"cannot create the drag window: {Marshal.GetLastPInvokeErrorMessage()}");
                return;
            }
            Pads[hwnd] = this;
            Win32.SetLayeredWindowAttributes(hwnd, 0, 1, Win32.LWA_ALPHA);
        }
        var (x, y) = Win32.CursorPosition();
        Win32.SetWindowPos(hwnd, Win32.HWND_TOPMOST, x - Size / 2, y - Size / 2, Size, Size, Win32.SWP_NOACTIVATE | Win32.SWP_SHOWWINDOW);
    }

    public void Hide()
    {
        if (hwnd != 0)
        {
            Win32.ShowWindow(hwnd, Win32.SW_HIDE);
        }
    }

    public void Close()
    {
        if (hwnd != 0)
        {
            Pads.TryRemove(hwnd, out _);
            Win32.DestroyWindow(hwnd);
            hwnd = 0;
        }
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvStdcall)])]
    private static nint Procedure(nint hwnd, uint message, nint wParam, nint lParam)
    {
        switch (message)
        {
            case Win32.WM_MOUSEACTIVATE:
                return Win32.MA_NOACTIVATE;
            case Win32.WM_LBUTTONDOWN or Win32.WM_RBUTTONDOWN or Win32.WM_MBUTTONDOWN:
                if (Pads.TryGetValue(hwnd, out var pad))
                {
                    pad.Pressed = true;
                }
                return 0;
        }
        return Win32.DefWindowProc(hwnd, message, wParam, lParam);
    }
}
