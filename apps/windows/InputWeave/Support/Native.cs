using System.Runtime.InteropServices;

namespace InputWeave.Support;

/// <summary>The few Win32 calls WinUI has no API for.</summary>
internal static partial class Native
{
    [StructLayout(LayoutKind.Sequential)]
    public struct Point
    {
        public int X;
        public int Y;
    }

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool GetCursorPos(out Point point);

    [LibraryImport("user32.dll")]
    public static partial uint GetDpiForWindow(nint hwnd);

    [LibraryImport("user32.dll")]
    public static partial uint GetDpiForSystem();

    [LibraryImport("user32.dll")]
    public static partial int GetSystemMetricsForDpi(int index, uint dpi);

    public const int SM_CXSMICON = 49;

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool SetForegroundWindow(nint hwnd);

    /// <summary>Lets any process take the foreground, as the one the user just started may.</summary>
    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool AllowSetForegroundWindow(uint processId);

    public const uint ASFW_ANY = uint.MaxValue;

    [LibraryImport("shcore.dll")]
    public static partial int GetDpiForMonitor(nint monitor, int type, out uint dpiX, out uint dpiY);

    private const int MDT_EFFECTIVE_DPI = 0;

    [LibraryImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static partial nint GetWindowLongPtr(nint hwnd, int index);

    [LibraryImport("user32.dll", EntryPoint = "SetWindowLongPtrW")]
    private static partial nint SetWindowLongPtr(nint hwnd, int index, nint value);

    private const int GWL_EXSTYLE = -20;
    private const nint WS_EX_TOOLWINDOW = 0x80;
    private const nint WS_EX_APPWINDOW = 0x40000;

    [LibraryImport("dwmapi.dll")]
    private static partial int DwmSetWindowAttribute(nint hwnd, int attribute, ref int value, int size);

    private const int DWMWA_WINDOW_CORNER_PREFERENCE = 33;
    private const int DWMWCP_ROUND = 2;

    /// <summary>The effective DPI of a monitor, or of the system when that fails.</summary>
    public static uint DpiForMonitor(nint monitor) =>
        GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, out uint dpi, out _) == 0 && dpi != 0 ? dpi : GetDpiForSystem();

    /// <summary>Keeps a window off the taskbar and out of Alt+Tab.</summary>
    public static void MakeToolWindow(nint hwnd)
    {
        var style = GetWindowLongPtr(hwnd, GWL_EXSTYLE);
        SetWindowLongPtr(hwnd, GWL_EXSTYLE, (style | WS_EX_TOOLWINDOW) & ~WS_EX_APPWINDOW);
    }

    /// <summary>Asks Windows 11 for rounded corners on a window without a title bar; no-op on Windows 10.</summary>
    public static void RoundCorners(nint hwnd)
    {
        int preference = DWMWCP_ROUND;
        _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, ref preference, sizeof(int));
    }

    private const nint WS_EX_NOACTIVATE = 0x08000000;

    /// <summary>Keeps a window from taking the focus when it shows or is clicked.</summary>
    public static void MakeNonActivating(nint hwnd)
    {
        var style = GetWindowLongPtr(hwnd, GWL_EXSTYLE);
        SetWindowLongPtr(hwnd, GWL_EXSTYLE, style | WS_EX_NOACTIVATE);
    }

    public static (int X, int Y) CursorPosition() =>
        GetCursorPos(out var point) ? (point.X, point.Y) : (0, 0);

    private const nint WS_EX_TRANSPARENT = 0x20;
    private const nint WS_EX_LAYERED = 0x80000;
    private const uint LWA_ALPHA = 0x2;

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool SetLayeredWindowAttributes(nint hwnd, uint key, byte alpha, uint flags);

    /// <summary>
    /// Lets clicks and the pointer pass through a window to whatever is
    /// below it. Windows only does that for layered windows, which show
    /// once given their opacity.
    /// </summary>
    public static void MakeClickThrough(nint hwnd)
    {
        var style = GetWindowLongPtr(hwnd, GWL_EXSTYLE);
        SetWindowLongPtr(hwnd, GWL_EXSTYLE, style | WS_EX_TRANSPARENT | WS_EX_LAYERED);
        SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
    }

    private const int DWMWA_BORDER_COLOR = 34;
    private const int DWMWCP_DONOTROUND = 1;
    private const int DWMWA_COLOR_NONE = unchecked((int)0xFFFFFFFE);

    private const int GWL_STYLE = -16;
    private const uint WS_POPUP = 0x80000000;
    private const uint WS_FRAME_STYLES = 0x00C00000 | 0x00040000; // WS_CAPTION (border and dialog frame), WS_THICKFRAME
    private const nint WS_EX_EDGES = 0x1 | 0x100 | 0x200 | 0x20000; // dialog, window, client and static edges
    private const int DWMWA_NCRENDERING_POLICY = 2;
    private const int DWMNCRP_DISABLED = 1;
    private const uint SWP_NOZORDER = 0x4;
    private const uint SWP_FRAMECHANGED = 0x20;

    /// <summary>
    /// No frame at all: no edges, no shadow, no rounded corners and no
    /// border from Windows 11, for a window that draws all of itself.
    /// </summary>
    public static void RemoveFrame(nint hwnd)
    {
        // A window style is 32 bits wide.
        var style = unchecked((uint)GetWindowLongPtr(hwnd, GWL_STYLE));
        SetWindowLongPtr(hwnd, GWL_STYLE, (nint)((style & ~WS_FRAME_STYLES) | WS_POPUP));
        var extended = GetWindowLongPtr(hwnd, GWL_EXSTYLE);
        SetWindowLongPtr(hwnd, GWL_EXSTYLE, extended & ~WS_EX_EDGES);
        SetWindowPos(hwnd, 0, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        int policy = DWMNCRP_DISABLED;
        _ = DwmSetWindowAttribute(hwnd, DWMWA_NCRENDERING_POLICY, ref policy, sizeof(int));
        int corner = DWMWCP_DONOTROUND;
        _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, ref corner, sizeof(int));
        int border = DWMWA_COLOR_NONE;
        _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, ref border, sizeof(int));
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct BlurBehind
    {
        public uint Flags;
        public int Enable;
        public nint Region;
        public int TransitionOnMaximized;
    }

    [LibraryImport("dwmapi.dll")]
    private static partial int DwmEnableBlurBehindWindow(nint hwnd, in BlurBehind blur);

    [LibraryImport("gdi32.dll")]
    private static partial nint CreateRectRgn(int left, int top, int right, int bottom);

    [LibraryImport("gdi32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool DeleteObject(nint handle);

    private const uint DWM_BB_ENABLE = 0x1;
    private const uint DWM_BB_BLURREGION = 0x2;

    [StructLayout(LayoutKind.Sequential)]
    private struct ClientRect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool GetClientRect(nint hwnd, out ClientRect rect);

    [LibraryImport("user32.dll")]
    private static partial int FillRect(nint hdc, in ClientRect rect, nint brush);

    [LibraryImport("gdi32.dll")]
    private static partial nint GetStockObject(int index);

    private const int BLACK_BRUSH = 4;
    private const uint WM_ERASEBKGND = 0x0014;
    private const uint WM_NCCALCSIZE = 0x0083;

    [LibraryImport("comctl32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static unsafe partial bool SetWindowSubclass(
        nint hwnd, delegate* unmanaged[Stdcall]<nint, uint, nint, nint, nuint, nuint, nint> proc, nuint id, nuint data);

    [LibraryImport("comctl32.dll")]
    private static partial nint DefSubclassProc(nint hwnd, uint message, nint wParam, nint lParam);

    [UnmanagedCallersOnly(CallConvs = [typeof(System.Runtime.CompilerServices.CallConvStdcall)])]
    private static nint ClearBackground(nint hwnd, uint message, nint wParam, nint lParam, nuint id, nuint data)
    {
        if (message == WM_ERASEBKGND && GetClientRect(hwnd, out var rect))
        {
            // Black with no alpha: nothing, once Windows composes with alpha.
            _ = FillRect(wParam, rect, GetStockObject(BLACK_BRUSH));
            return 1;
        }
        if (message == WM_NCCALCSIZE && wParam != 0)
        {
            // All of the window is client area: no frame, whatever its style says.
            return 0;
        }
        return DefSubclassProc(hwnd, message, wParam, lParam);
    }

    /// <summary>
    /// Has Windows compose the window with the alpha of what it draws, so
    /// what it leaves clear shows the desktop: the window's own background
    /// is erased to nothing instead of white, all of the window is client
    /// area so no frame is drawn, and "blur behind" is turned on for an
    /// empty region, which blurs nothing but makes Windows honor the alpha.
    /// </summary>
    public static unsafe void ComposeWithAlpha(nint hwnd)
    {
        SetWindowSubclass(hwnd, &ClearBackground, 1, 0);
        SetWindowPos(hwnd, 0, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        var region = CreateRectRgn(0, 0, -1, -1);
        _ = DwmEnableBlurBehindWindow(hwnd, new BlurBehind { Flags = DWM_BB_ENABLE | DWM_BB_BLURREGION, Enable = 1, Region = region });
        DeleteObject(region);
    }

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool SetWindowPos(nint hwnd, nint after, int x, int y, int cx, int cy, uint flags);

    private static readonly nint HWND_TOPMOST = -1;
    private const uint SWP_NOSIZE = 0x1;
    private const uint SWP_NOMOVE = 0x2;
    private const uint SWP_NOACTIVATE = 0x10;

    [LibraryImport("shell32.dll")]
    private static partial int SHQueryUserNotificationState(out int state);

    /// <summary>
    /// What Windows says the user is doing (<c>QUERY_USER_NOTIFICATION_STATE</c>):
    /// busy, in a full-screen Direct3D app, presenting, and so on; null when
    /// it does not say.
    /// </summary>
    public static int? NotificationState() => SHQueryUserNotificationState(out var state) == 0 ? state : null;

    /// <summary>Puts a window above every other, menus included, without activating it.</summary>
    public static void BringToTop(nint hwnd) =>
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);

    /// <summary>Moves a window, in pixels, and sizes it when asked, leaving it where it is among the others and inactive.</summary>
    public static void Place(nint hwnd, int x, int y, int width, int height, bool resize) =>
        SetWindowPos(hwnd, 0, x, y, width, height, SWP_NOZORDER | SWP_NOACTIVATE | (resize ? 0 : SWP_NOSIZE));

    [StructLayout(LayoutKind.Sequential)]
    private struct Rect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private unsafe struct MonitorInfoEx
    {
        public int Size;
        public Rect Monitor;
        public Rect Work;
        public uint Flags;
        public fixed char Device[32];
    }

    [LibraryImport("user32.dll", EntryPoint = "GetMonitorInfoW")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static unsafe partial bool GetMonitorInfo(nint monitor, MonitorInfoEx* info);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static unsafe partial bool EnumDisplayMonitors(
        nint hdc, nint clip, delegate* unmanaged[Stdcall]<nint, nint, Rect*, nint, int> callback, nint data);

    [ThreadStatic]
    private static List<nint>? enumerated;

    [UnmanagedCallersOnly(CallConvs = [typeof(System.Runtime.CompilerServices.CallConvStdcall)])]
    private static unsafe int CollectMonitor(nint monitor, nint hdc, Rect* clip, nint data)
    {
        enumerated?.Add(monitor);
        return 1;
    }

    /// <summary>
    /// Every monitor of the desktop in physical pixels, named as the
    /// background service names screens (<c>\\.\DISPLAY1</c>), with its scale.
    /// </summary>
    public static unsafe IReadOnlyList<Overlay.MonitorPixels> Monitors()
    {
        enumerated = [];
        try
        {
            EnumDisplayMonitors(0, 0, &CollectMonitor, 0);
            var monitors = new List<Overlay.MonitorPixels>();
            foreach (var handle in enumerated)
            {
                var info = new MonitorInfoEx { Size = sizeof(MonitorInfoEx) };
                if (!GetMonitorInfo(handle, &info))
                {
                    continue;
                }
                var name = new string(info.Device);
                var end = name.IndexOf('\0', StringComparison.Ordinal);
                monitors.Add(new Overlay.MonitorPixels(
                    end < 0 ? name : name[..end],
                    info.Monitor.Left, info.Monitor.Top, info.Monitor.Right, info.Monitor.Bottom,
                    DpiForMonitor(handle) / 96.0,
                    (info.Flags & 1) != 0));
            }
            return monitors;
        }
        finally
        {
            enumerated = null;
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct DispatcherQueueOptions
    {
        public int Size;
        public int ThreadType;
        public int ApartmentType;
    }

    [LibraryImport("CoreMessaging.dll")]
    private static partial int CreateDispatcherQueueController(DispatcherQueueOptions options, out nint controller);

    private static nint systemQueue;

    /// <summary>
    /// Gives the UI thread a Windows.System dispatcher queue, which a
    /// Windows.UI.Composition compositor needs; WinUI only makes its own
    /// kind. Kept for the life of the app.
    /// </summary>
    public static void EnsureSystemDispatcherQueue()
    {
        if (systemQueue != 0 || Windows.System.DispatcherQueue.GetForCurrentThread() is not null)
        {
            return;
        }
        // DQTYPE_THREAD_CURRENT, DQTAT_COM_NONE
        var options = new DispatcherQueueOptions { Size = Marshal.SizeOf<DispatcherQueueOptions>(), ThreadType = 2, ApartmentType = 0 };
        _ = CreateDispatcherQueueController(options, out systemQueue);
    }
}
