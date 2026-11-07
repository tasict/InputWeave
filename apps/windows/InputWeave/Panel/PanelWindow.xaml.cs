using System.Globalization;
using InputWeave.Agent;
using InputWeave.Canvas;
using InputWeave.Main;
using InputWeave.Support;
using InputWeave.Tray;
using Microsoft.UI;
using Microsoft.UI.Composition.SystemBackdrops;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;
using WinRT.Interop;

namespace InputWeave.Panel;

/// <summary>
/// The panel the tray icon opens: borderless,
/// always on top, anchored above the taskbar at the pointer, and closed as
/// soon as it loses focus.
/// </summary>
public sealed partial class PanelWindow : Window
{
    private const double PanelWidth = 340;
    /// <summary>Space between the panel and the taskbar or the screen's edge.</summary>
    private const double EdgeMargin = 12;
    /// <summary>A click on the tray icon this soon after the panel closed is the click that closed it.</summary>
    private static readonly TimeSpan ReopenGuard = TimeSpan.FromMilliseconds(300);

    /// <summary>The transfers the panel lists at most; the rest are on the File Transfers page.</summary>
    private const int TransferLimit = 4;

    private readonly Action<bool> setSharing;
    private readonly AppMenu menu;
    private readonly ArrangementCanvas map = new() { Interactive = false, ContentPadding = 12 };
    private readonly nint hwnd;
    private bool updating;
    private bool pinned;
    private (int X, int Y)? anchor;
    private DateTime hiddenAt = DateTime.MinValue;
    private AgentSnapshot? shown;
    private string? shownKey;

    internal PanelWindow(MenuActions actions, Action<bool> setSharing)
    {
        InitializeComponent();
        this.setSharing = setSharing;
        menu = new AppMenu(MoreMenu, actions);
        BringBackButton.Content = Strings.Get(TextKeys.OfflineBringBack);
        AutomationProperties.SetName(map, Strings.Get(TextKeys.NavArrangement));
        MapFrame.Child = map;
        hwnd = WindowNative.GetWindowHandle(this);

        var presenter = OverlappedPresenter.Create();
        presenter.IsResizable = false;
        presenter.IsMaximizable = false;
        presenter.IsMinimizable = false;
        presenter.IsAlwaysOnTop = true;
        presenter.SetBorderAndTitleBar(hasBorder: true, hasTitleBar: false);
        AppWindow.SetPresenter(presenter);
        AppWindow.IsShownInSwitchers = false;
        Native.MakeToolWindow(hwnd);
        Native.RoundCorners(hwnd);
        if (MicaController.IsSupported())
        {
            SystemBackdrop = new MicaBackdrop();
        }
        else if (DesktopAcrylicController.IsSupported())
        {
            SystemBackdrop = new DesktopAcrylicBackdrop();
        }
        else
        {
            Root.Background = (Brush)Application.Current.Resources["SolidBackgroundFillColorBaseBrush"];
        }

        Activated += OnActivated;
        // Alt+F4 only hides it, like losing focus.
        AppWindow.Closing += (_, e) =>
        {
            e.Cancel = true;
            Hide();
        };
        Root.SizeChanged += (_, _) =>
        {
            if (IsShown)
            {
                Place();
            }
        };
    }

    public bool IsShown => AppWindow.IsVisible;

    /// <summary>A left click on the tray icon: opens the panel there, or closes it.</summary>
    internal void Toggle((int X, int Y) pointer)
    {
        if (IsShown)
        {
            Hide();
            return;
        }
        if (DateTime.UtcNow - hiddenAt < ReopenGuard)
        {
            return;
        }
        Show(pointer, pin: false);
    }

    /// <summary>
    /// Opens the panel near <paramref name="pointer"/>, or above the end of
    /// the taskbar. Pinned, it stays open without focus (for testing).
    /// </summary>
    internal void Show((int X, int Y)? pointer, bool pin)
    {
        pinned = pin;
        anchor = pointer;
        Place();
        AppWindow.Show(activateWindow: true);
        Activate();
        Native.SetForegroundWindow(hwnd);
        // Once laid out, its real height is known.
        DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, Place);
    }

    internal void Hide()
    {
        if (!IsShown)
        {
            return;
        }
        AppWindow.Hide();
        hiddenAt = DateTime.UtcNow;
    }

    private void OnActivated(object sender, WindowActivatedEventArgs args)
    {
        if (args.WindowActivationState == WindowActivationState.Deactivated && !pinned)
        {
            Hide();
        }
    }

    private void OnEscape(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        Hide();
    }

    /// <summary>
    /// Sizes the panel to its content and puts it next to the taskbar,
    /// centered on the pointer as far as the screen allows.
    /// </summary>
    private void Place()
    {
        var area = anchor is { } at
            ? DisplayArea.GetFromPoint(new PointInt32(at.X, at.Y), DisplayAreaFallback.Primary)
            : DisplayArea.Primary;
        var work = area.WorkArea;
        var outer = area.OuterBounds;
        // Without a pointer, the notification area's end of the taskbar.
        var (px, py) = anchor ?? (work.X + work.Width, work.Y + work.Height);
        double scale = Native.DpiForMonitor(Win32Interop.GetMonitorFromDisplayId(area.DisplayId)) / 96.0;

        Root.Measure(new Windows.Foundation.Size(PanelWidth, double.PositiveInfinity));
        double height = Math.Max(Root.DesiredSize.Height, 100);
        int w = (int)Math.Ceiling(PanelWidth * scale);
        int h = (int)Math.Ceiling(height * scale);
        int margin = (int)Math.Round(EdgeMargin * scale);
        int left = work.X + margin;
        int right = Math.Max(left, work.X + work.Width - w - margin);
        int top = work.Y + margin;
        int bottom = Math.Max(top, work.Y + work.Height - h - margin);

        // Above a taskbar at the bottom, below one at the top, beside one at a side.
        int x = Math.Clamp(px - w / 2, left, right);
        int y = bottom;
        if (work.Y > outer.Y)
        {
            y = top;
        }
        else if (work.X > outer.X)
        {
            (x, y) = (left, Math.Clamp(py - h / 2, top, bottom));
        }
        else if (work.X + work.Width < outer.X + outer.Width)
        {
            (x, y) = (right, Math.Clamp(py - h / 2, top, bottom));
        }
        MoveClient(new RectInt32(x, y, w, h));
    }

    /// <summary>
    /// Puts the client area, where the content is, at <paramref name="client"/>.
    /// The window around it has invisible resize borders, and without a title
    /// bar <see cref="AppWindow.ResizeClient"/> still counts one, so the frame
    /// is measured instead.
    /// </summary>
    private void MoveClient(RectInt32 client)
    {
        for (int attempt = 0; attempt < 2; attempt++)
        {
            var size = AppWindow.Size;
            var inside = AppWindow.ClientSize;
            int frameWidth = Math.Max(0, size.Width - inside.Width);
            int frameHeight = Math.Max(0, size.Height - inside.Height);
            // The side borders are even; without a title bar the top has none.
            AppWindow.MoveAndResize(new RectInt32(
                client.X - frameWidth / 2, client.Y, client.Width + frameWidth, client.Height + frameHeight));
            if (AppWindow.ClientSize.Width == client.Width && AppWindow.ClientSize.Height == client.Height)
            {
                return;
            }
        }
    }

    /// <summary>Shows what the app knows now. Cheap when nothing changed.</summary>
    internal void Update(AgentSnapshot model, ServiceState service, DateTimeOffset now, MenuState menuState)
    {
        menu.Update(menuState);
        var title = Strings.Text(PanelText.Title(model, service, now));
        var clipboard = ClipboardText.Line(model, now, CultureInfo.CurrentCulture) is { } line ? Strings.Text(line) : null;
        var key = $"{title}|{service}|{clipboard}|{App.Instance.DismissedTransfers.Version}";
        if (ReferenceEquals(model, shown) && key == shownKey)
        {
            return;
        }
        shown = model;
        shownKey = key;

        var connected = model.Link == AgentLink.Connected;
        TitleText.Text = title;
        var subtitle = PanelText.Subtitle(model);
        SubtitleText.Text = subtitle is null ? "" : Strings.Text(subtitle);
        SubtitleText.Visibility = subtitle is null ? Visibility.Collapsed : Visibility.Visible;

        // Offline for now, the switch still shows the user's own choice.
        updating = true;
        SharingSwitch.IsOn = connected && model.Status.Sharing;
        SharingSwitch.IsEnabled = connected;
        updating = false;
        BringBackButton.Visibility = connected && Settings.OfflineForNow.ThisOffline(model) ? Visibility.Visible : Visibility.Collapsed;

        MapFrame.Visibility = connected ? Visibility.Visible : Visibility.Collapsed;
        map.Update(model);

        Exceptions.Children.Clear();
        foreach (var row in ExceptionRows.For(model, service))
        {
            Exceptions.Children.Add(ExceptionRowView.For(row, compact: true));
        }
        Exceptions.Visibility = Exceptions.Children.Count == 0 ? Visibility.Collapsed : Visibility.Visible;

        ShowTransfers(model);
        ClipboardLabel.Text = clipboard ?? "";
        ClipboardRow.Visibility = clipboard is null ? Visibility.Collapsed : Visibility.Visible;
        SendFilesButton.IsEnabled = connected && model.FileReceivers.Count != 0;

        if (IsShown)
        {
            Place();
        }
    }

    /// <summary>Makes the next <see cref="Update"/> redraw everything, e.g. to put back a switch the service did not follow.</summary>
    internal void Invalidate() => shown = null;

    private void OnSharingToggled(object sender, RoutedEventArgs e)
    {
        if (!updating)
        {
            setSharing(SharingSwitch.IsOn);
        }
    }

    /// <summary>The files going and waiting, and problems until dismissed.</summary>
    private void ShowTransfers(AgentSnapshot model)
    {
        Transfers.Children.Clear();
        var rows = model.Link == AgentLink.Connected
            ? TransferText.PanelRows(model.Transfers.Transfers_, App.Instance.DismissedTransfers.Ids)
            : [];
        if (rows.Count != 0)
        {
            Transfers.Children.Add(new TextBlock
            {
                Text = Strings.Get(TextKeys.TransfersHeader),
                Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
                Foreground = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"],
                FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            });
            foreach (var transfer in rows.Take(TransferLimit))
            {
                Transfers.Children.Add(TransferRow.For(model, transfer, dismissible: true));
            }
            if (rows.Count > TransferLimit)
            {
                var more = new HyperlinkButton
                {
                    Content = Strings.Format(TextKeys.TransfersMore, (rows.Count - TransferLimit).ToString(CultureInfo.CurrentCulture)),
                    Padding = new Thickness(0),
                    FontSize = 12,
                };
                more.Click += (_, _) => App.Instance.ShowMain(MainPage.Files);
                Transfers.Children.Add(more);
            }
        }
        Transfers.Visibility = rows.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>Bring Back Online, while this computer is offline for now.</summary>
    private void OnBringBack(object sender, RoutedEventArgs e)
    {
        if (shown is { } model && Settings.OfflineForNow.ThisOffline(model))
        {
            App.Instance.SetOfflineForNow(model.ThisDeviceId, offline: false);
        }
    }

    private void OnSendFiles(object sender, RoutedEventArgs e) => SendFiles.Start(hwnd);

    private void OnSettings(object sender, RoutedEventArgs e) => App.Instance.ShowMain(null);
}
