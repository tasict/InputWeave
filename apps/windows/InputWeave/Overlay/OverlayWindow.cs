using InputWeave.Support;
using Microsoft.UI.Composition;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;
using WinRT.Interop;

namespace InputWeave.Overlay;

/// <summary>A backdrop of nothing: the window shows only what its content draws, and the desktop elsewhere.</summary>
internal sealed partial class ClearBackdrop : SystemBackdrop
{
    private static Windows.UI.Composition.Compositor? compositor;
    private Windows.UI.Composition.CompositionColorBrush? brush;

    protected override void OnTargetConnected(ICompositionSupportsSystemBackdrop connectedTarget, XamlRoot xamlRoot)
    {
        base.OnTargetConnected(connectedTarget, xamlRoot);
        try
        {
            Native.EnsureSystemDispatcherQueue();
            compositor ??= new Windows.UI.Composition.Compositor();
            brush = compositor.CreateColorBrush(Windows.UI.Color.FromArgb(0, 0, 0, 0));
            connectedTarget.SystemBackdrop = brush;
        }
        catch (Exception e)
        {
            // The window then shows on the theme's background instead of the desktop.
            AppLog.Warn($"cannot clear an overlay's backdrop: {e.Message}");
        }
    }

    protected override void OnTargetDisconnected(ICompositionSupportsSystemBackdrop disconnectedTarget)
    {
        base.OnTargetDisconnected(disconnectedTarget);
        disconnectedTarget.SystemBackdrop = null;
        brush?.Dispose();
        brush = null;
    }
}

/// <summary>
/// A window above all others that draws only its content on a clear
/// background, never takes the focus, stays off the taskbar and Alt+Tab,
/// and lets the pointer through to whatever is below it: the HUD and the
/// cursor locator.
/// </summary>
internal partial class OverlayWindow : Window
{
    protected OverlayWindow()
    {
        Title = "InputWeave";
        Hwnd = WindowNative.GetWindowHandle(this);
        var presenter = OverlappedPresenter.Create();
        presenter.IsResizable = false;
        presenter.IsMaximizable = false;
        presenter.IsMinimizable = false;
        presenter.IsAlwaysOnTop = true;
        presenter.SetBorderAndTitleBar(hasBorder: false, hasTitleBar: false);
        AppWindow.SetPresenter(presenter);
        AppWindow.IsShownInSwitchers = false;
        Native.MakeToolWindow(Hwnd);
        Native.MakeNonActivating(Hwnd);
        Native.MakeClickThrough(Hwnd);
        Native.RemoveFrame(Hwnd);
        // Clear where nothing is drawn takes three things: Windows composing
        // with alpha, the window's own background erased to nothing, and a
        // clear backdrop behind the content.
        Native.ComposeWithAlpha(Hwnd);
        SystemBackdrop = new ClearBackdrop();
        // Closed only with the app.
        AppWindow.Closing += (_, e) =>
        {
            e.Cancel = true;
            AppWindow.Hide();
        };
    }

    protected nint Hwnd { get; }

    public bool IsShown => AppWindow.IsVisible;

    private static readonly Windows.UI.ViewManagement.UISettings Settings = new();

    /// <summary>Animations follow Windows' "Animation effects" setting.</summary>
    internal static bool ReduceMotion => !Settings.AnimationsEnabled;

    /// <summary>Shows the window at <paramref name="frame"/>, in pixels, above everything and without taking the focus.</summary>
    protected void ShowAt(PixelRect frame)
    {
        AppWindow.MoveAndResize(new RectInt32(frame.X, frame.Y, frame.Width, frame.Height));
        if (!AppWindow.IsVisible)
        {
            AppWindow.Show(activateWindow: false);
            // Showing puts the presenter's frame back.
            Native.RemoveFrame(Hwnd);
        }
        Native.BringToTop(Hwnd);
    }

    /// <summary>
    /// Moves the shown window to <paramref name="frame"/>, in pixels, where
    /// it is among the others. Resized only when its size is not the
    /// frame's, as after Windows resized it for a monitor of another scale.
    /// </summary>
    protected void MoveTo(PixelRect frame)
    {
        var size = AppWindow.Size;
        Native.Place(Hwnd, frame.X, frame.Y, frame.Width, frame.Height, resize: size.Width != frame.Width || size.Height != frame.Height);
    }

    /// <summary>Whether the pointer is over the window now; the window itself never sees it.</summary>
    protected bool PointerOver()
    {
        var (x, y) = Native.CursorPosition();
        var at = AppWindow.Position;
        var size = AppWindow.Size;
        return x >= at.X && x < at.X + size.Width && y >= at.Y && y < at.Y + size.Height;
    }

    /// <summary>The effective scale of the monitor a pixel is on.</summary>
    protected static double ScaleAt(PointInt32 pixel)
    {
        var area = DisplayArea.GetFromPoint(pixel, DisplayAreaFallback.Primary);
        return Native.DpiForMonitor(Microsoft.UI.Win32Interop.GetMonitorFromDisplayId(area.DisplayId)) / 96.0;
    }

    /// <summary>Narrator reads <paramref name="message"/>, as VoiceOver does on the Mac.</summary>
    protected static void Announce(FrameworkElement element, string message, string activity)
    {
        try
        {
            var peer = Microsoft.UI.Xaml.Automation.Peers.FrameworkElementAutomationPeer.FromElement(element)
                ?? Microsoft.UI.Xaml.Automation.Peers.FrameworkElementAutomationPeer.CreatePeerForElement(element);
            peer?.RaiseNotificationEvent(
                Microsoft.UI.Xaml.Automation.Peers.AutomationNotificationKind.Other,
                Microsoft.UI.Xaml.Automation.Peers.AutomationNotificationProcessing.ImportantMostRecent,
                message,
                activity);
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or InvalidOperationException)
        {
            // Nobody listens, or the element is not ready yet.
        }
    }

    public void HideNow()
    {
        if (AppWindow.IsVisible)
        {
            AppWindow.Hide();
        }
    }
}
