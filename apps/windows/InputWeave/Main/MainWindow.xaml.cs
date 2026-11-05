using InputWeave.Agent;
using InputWeave.Licensing;
using InputWeave.Updates;
using InputWeave.Panel;
using InputWeave.Pairing;
using InputWeave.Support;
using Microsoft.UI;
using Microsoft.UI.Composition.SystemBackdrops;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;
using WinRT.Interop;

namespace InputWeave.Main;

/// <summary>A page of the main window, showing what the app knows now.</summary>
internal interface IMainPage
{
    void Update(AgentSnapshot model, ServiceState service, DateTimeOffset now);

    /// <summary>Makes the next update show everything again, e.g. to put back a switch the service did not follow.</summary>
    void Invalidate();
}

/// <summary>
/// The InputWeave window: the settings, in
/// five pages. Closing it only hides it; the app stays in the notification
/// area. While the background service is out of reach the pages are
/// covered by a message that offers to start or restart the service.
/// </summary>
public sealed partial class MainWindow : Window
{
    /// <summary>The smallest the window gets, in view units.</summary>
    private const double MinWidth = 760;
    private const double MinHeight = 520;

    private readonly Dictionary<MainPage, (FrameworkElement View, IMainPage Page)> pages = [];
    private readonly OverlappedPresenter presenter;
    private MainPage current = MainPage.General;
    private AgentSnapshot? model;
    private ServiceState service;
    private AddDeviceDialog? addDevice;
    private ContentDialog? overlay;
    private bool aboutOpen;
    private AboutDialog? about;
    private (LicenseVariant Variant, bool Shown)? licenseItemShows;
    private readonly TextBlock licenseItemText = new() { VerticalAlignment = VerticalAlignment.Center };
    private readonly Grid licenseItemContent = new() { ColumnSpacing = 8 };

    public MainWindow()
    {
        InitializeComponent();
        Hwnd = WindowNative.GetWindowHandle(this);
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(TitleBar);
        AppWindow.TitleBar.PreferredHeightOption = TitleBarHeightOption.Tall;
        var icon = Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico");
        if (File.Exists(icon))
        {
            AppWindow.SetIcon(icon);
        }
        if (MicaController.IsSupported())
        {
            SystemBackdrop = new MicaBackdrop();
        }
        else
        {
            Root.Background = Brand.System("SolidBackgroundFillColorBaseBrush");
        }

        presenter = OverlappedPresenter.Create();
        AppWindow.SetPresenter(presenter);
        // Closing only hides it, as the app lives on in the notification area.
        AppWindow.Closing += (_, e) =>
        {
            e.Cancel = true;
            AppWindow.Hide();
        };
        AppWindow.Changed += (_, e) =>
        {
            if (e.DidPresenterChange || e.DidSizeChange)
            {
                ApplyMinimumSize();
            }
        };
        ArrangementItem.Content = Strings.Get(TextKeys.NavArrangement);
        DevicesItem.Content = Strings.Get(TextKeys.NavDevices);
        FilesItem.Content = Strings.Get(TextKeys.NavFiles);
        GeneralItem.Content = Strings.Get(TextKeys.NavGeneral);
        TroubleshootingItem.Content = Strings.Get(TextKeys.NavTroubleshooting);
        licenseItemText.Text = Strings.Get(TextKeys.NavLicense);
        licenseItemContent.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        licenseItemContent.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        licenseItemContent.Children.Add(licenseItemText);
        LicenseItem.Content = licenseItemContent;
        UnavailableDetail.Text = Strings.Get(TextKeys.MainUnavailable);
        Nav.SelectedItem = GeneralItem;
        Place();
    }

    /// <summary>The window's handle, for the pickers that need an owner.</summary>
    public nint Hwnd { get; }

    public bool IsShown => AppWindow.IsVisible;

    /// <summary>
    /// Opens the window, on <paramref name="page"/> when given, and Add
    /// Device over it when asked. Pinned, it stays above other windows even
    /// when Windows keeps it from the foreground, as for a launch from a
    /// scheduled task (for testing).
    /// </summary>
    internal void Open(MainPage? page, bool addDevice, bool pin = false)
    {
        if (pin)
        {
            presenter.IsAlwaysOnTop = true;
        }
        if (page is { } p)
        {
            Navigate(p);
        }
        AppWindow.Show(activateWindow: true);
        Activate();
        Native.SetForegroundWindow(Hwnd);
        if (addDevice)
        {
            // After the window has laid out, which a ContentDialog needs.
            DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, ShowAddDevice);
        }
    }

    internal void Navigate(MainPage page)
    {
        Nav.SelectedItem = page switch
        {
            MainPage.Devices => DevicesItem,
            MainPage.Arrangement => ArrangementItem,
            MainPage.Files => FilesItem,
            MainPage.Troubleshooting => TroubleshootingItem,
            MainPage.License when LicenseItem.Visibility == Visibility.Visible => LicenseItem,
            _ => GeneralItem,
        };
    }

    /// <summary>The License page or About is on screen: the license may be confirmed with Polar now.</summary>
    internal bool ShowsLicense => IsShown && (aboutOpen || current == MainPage.License);

    /// <summary>Shows "Trial" or the woven label beside the License item; hidden for a service without licenses.</summary>
    private void ShowLicenseItem(AgentSnapshot snapshot)
    {
        var view = LicenseView.For(snapshot.Status.License, snapshot.Status.ThisDeviceName, App.Instance.License.Withdrawing(snapshot.Status.License));
        if (licenseItemShows is { } before && before.Variant == view.Variant && before.Shown == view.Shown)
        {
            return;
        }
        licenseItemShows = (view.Variant, view.Shown);
        LicenseItem.Visibility = view.Shown ? Visibility.Visible : Visibility.Collapsed;
        if (!view.Shown && current == MainPage.License)
        {
            // A service without licenses: the page it was on is gone.
            Navigate(MainPage.General);
        }
        if (licenseItemContent.Children.Count > 1)
        {
            licenseItemContent.Children.RemoveAt(1);
        }
        var status = view.IsLicensed ? WovenLabel.Create(WovenLabel.Small) : WovenLabel.TrialPill();
        status.Margin = new Thickness(0, 0, 8, 0);
        Grid.SetColumn(status, 1);
        licenseItemContent.Children.Add(status);
    }

    /// <summary>About InputWeave, over this window.</summary>
    internal async void ShowAbout()
    {
        if (overlay is not null || Content.XamlRoot is null)
        {
            return;
        }
        var snapshot = App.Instance.Model.Snapshot;
        var dialog = new AboutDialog(LicenseView.For(snapshot.Status.License, snapshot.Status.ThisDeviceName, App.Instance.License.Withdrawing(snapshot.Status.License))) { XamlRoot = Content.XamlRoot };
        aboutOpen = true;
        about = dialog;
        App.Instance.RefreshNow();
        await ShowOverlay(dialog);
        about = null;
        aboutOpen = false;
    }

    /// <summary>The update dialog for <paramref name="feed"/>, over this window.</summary>
    internal async void ShowUpdate(Updater updater, UpdateFeed feed, bool install = false)
    {
        if (overlay is not null || Content.XamlRoot is null)
        {
            return;
        }
        await ShowOverlay(new UpdateDialog(updater, feed, install) { XamlRoot = Content.XamlRoot });
    }

    private async Task ShowOverlay(ContentDialog dialog)
    {
        overlay = dialog;
        try
        {
            await dialog.ShowAsync();
        }
        catch (Exception e)
        {
            // Another dialog is open over the window.
            AppLog.Info($"dialog: {e.Message}");
        }
        overlay = null;
    }

    /// <summary>Shows what the app knows now. Cheap when nothing changed.</summary>
    internal void Update(AgentSnapshot snapshot, ServiceState serviceState, DateTimeOffset now)
    {
        model = snapshot;
        service = serviceState;
        about?.Update(LicenseView.For(snapshot.Status.License, snapshot.Status.ThisDeviceName, App.Instance.License.Withdrawing(snapshot.Status.License)));
        bool connected = snapshot.Link == AgentLink.Connected;
        Unavailable.Visibility = connected ? Visibility.Collapsed : Visibility.Visible;
        if (!connected)
        {
            UnavailableTitle.Text = Strings.Text(PanelText.Title(snapshot, serviceState, now));
            UnavailableAction.Visibility = serviceState == ServiceState.NotInstalled ? Visibility.Collapsed : Visibility.Visible;
            UnavailableAction.Content = ServiceButton(serviceState);
        }
        if (pages.TryGetValue(current, out var shown))
        {
            shown.Page.Update(snapshot, serviceState, now);
        }
        addDevice?.Update(snapshot);
        ShowLicenseItem(snapshot);
    }

    internal void Invalidate()
    {
        foreach (var (_, page) in pages.Values)
        {
            page.Invalidate();
        }
    }

    /// <summary>"Start Service" or "Restart Service", with the shield: both go through UAC.</summary>
    internal static StackPanel ServiceButton(ServiceState service)
    {
        var content = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        content.Children.Add(new FontIcon { Glyph = "", FontSize = 14 });
        content.Children.Add(new TextBlock
        {
            Text = Strings.Get(service == ServiceState.Stopped ? TextKeys.ActionStartService : TextKeys.ActionRestartService),
        });
        return content;
    }

    internal static void RunServiceAction(ServiceState service)
    {
        if (service == ServiceState.Stopped)
        {
            ServiceWatcher.Start();
        }
        else
        {
            Shell.RestartService();
        }
    }

    private void OnUnavailableAction(object sender, RoutedEventArgs e) => RunServiceAction(service);

    private void OnNavigationChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        if (args.SelectedItem is not NavigationViewItem { Tag: string tag } item || !Enum.TryParse<MainPage>(tag, out var page))
        {
            return;
        }
        current = page;
        if (!pages.TryGetValue(page, out var shown))
        {
            FrameworkElement view = page switch
            {
                MainPage.Devices => new DevicesPage(),
                MainPage.Arrangement => new ArrangementPage(),
                MainPage.Files => new FilesPage(),
                MainPage.Troubleshooting => new TroubleshootingPage(),
                MainPage.License => new LicensePage(),
                _ => new GeneralPage(),
            };
            shown = (view, (IMainPage)view);
            pages[page] = shown;
        }
        PageHost.Content = shown.View;
        Nav.Header = page == MainPage.License ? Strings.Get(TextKeys.NavLicense) : item.Content;
        if (model is not null)
        {
            shown.Page.Update(model, service, DateTimeOffset.UtcNow);
        }
    }

    /// <summary>Add Device, over this window.</summary>
    internal async void ShowAddDevice()
    {
        if (addDevice is not null || Content.XamlRoot is null)
        {
            return;
        }
        addDevice = new AddDeviceDialog(App.Instance.Model) { XamlRoot = Content.XamlRoot };
        if (model is not null)
        {
            addDevice.Update(model);
        }
        var dialog = addDevice;
        try
        {
            await dialog.ShowAsync();
        }
        catch (Exception e)
        {
            // Another dialog is open over the window.
            AppLog.Info($"add device: {e.Message}");
        }
        addDevice = null;
        if (dialog.Paired)
        {
            Navigate(MainPage.Arrangement);
        }
    }

    /// <summary>Opens at a comfortable size in the middle of the main screen.</summary>
    private void Place()
    {
        var area = DisplayArea.Primary;
        double scale = Native.DpiForMonitor(Win32Interop.GetMonitorFromDisplayId(area.DisplayId)) / 96.0;
        var work = area.WorkArea;
        int w = Math.Min((int)(1000 * scale), work.Width);
        int h = Math.Min((int)(680 * scale), work.Height);
        AppWindow.MoveAndResize(new RectInt32(work.X + (work.Width - w) / 2, work.Y + (work.Height - h) / 2, w, h));
        ApplyMinimumSize();
    }

    private void ApplyMinimumSize()
    {
        double scale = Native.GetDpiForWindow(Hwnd) / 96.0;
        if (scale <= 0)
        {
            scale = 1;
        }
        int w = (int)Math.Ceiling(MinWidth * scale);
        int h = (int)Math.Ceiling(MinHeight * scale);
        if (presenter.PreferredMinimumWidth != w || presenter.PreferredMinimumHeight != h)
        {
            presenter.PreferredMinimumWidth = w;
            presenter.PreferredMinimumHeight = h;
        }
    }
}
