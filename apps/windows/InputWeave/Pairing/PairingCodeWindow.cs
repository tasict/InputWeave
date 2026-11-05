using Inputweave.App.V1;
using InputWeave.Panel;
using InputWeave.Support;
using Microsoft.UI;
using Microsoft.UI.Composition.SystemBackdrops;
using Microsoft.UI.Text;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;
using WinRT.Interop;

namespace InputWeave.Pairing;

/// <summary>
/// The code shown on the computer being paired:
/// a window in the middle of the main screen, above the others, that never
/// takes the focus and needs no input, since this computer may have no
/// keyboard or mouse of its own; whoever adds it reads the code here. Its
/// only button declines. The background service decides when it shows,
/// by the pairing phase (<see cref="PairingCode.ShowsCode"/>).
/// </summary>
internal sealed partial class PairingCodeWindow : Window
{
    private const double PanelWidth = 420;

    private readonly StackPanel root = new() { Width = PanelWidth, Padding = new Thickness(28), Spacing = 14 };
    private readonly TextBlock title = new()
    {
        TextWrapping = TextWrapping.Wrap,
        TextAlignment = TextAlignment.Center,
        HorizontalAlignment = HorizontalAlignment.Center,
    };
    private readonly TextBlock code = new()
    {
        FontSize = 48,
        FontWeight = FontWeights.SemiBold,
        CharacterSpacing = 80,
        HorizontalAlignment = HorizontalAlignment.Center,
        IsTextSelectionEnabled = true,
    };
    private readonly TextBlock countdown = new()
    {
        HorizontalAlignment = HorizontalAlignment.Center,
        Foreground = Brand.System("TextFillColorSecondaryBrush"),
    };
    private readonly nint hwnd;
    private PairingState state = new();

    public PairingCodeWindow(Action decline)
    {
        Title = "InputWeave";
        hwnd = WindowNative.GetWindowHandle(this);
        title.Style = (Style)Application.Current.Resources["SubtitleTextBlockStyle"];
        root.Children.Add(title);
        root.Children.Add(code);
        AutomationProperties.SetLiveSetting(countdown, AutomationLiveSetting.Off);
        root.Children.Add(countdown);
        var button = new Button { Content = Strings.Get(TextKeys.PairingDecline), HorizontalAlignment = HorizontalAlignment.Center };
        button.Click += (_, _) => decline();
        root.Children.Add(button);
        Content = root;

        var presenter = OverlappedPresenter.Create();
        presenter.IsResizable = false;
        presenter.IsMaximizable = false;
        presenter.IsMinimizable = false;
        presenter.IsAlwaysOnTop = true;
        presenter.SetBorderAndTitleBar(hasBorder: true, hasTitleBar: false);
        AppWindow.SetPresenter(presenter);
        AppWindow.IsShownInSwitchers = false;
        Native.MakeToolWindow(hwnd);
        // Shown without taking the focus from whatever the user is doing.
        Native.MakeNonActivating(hwnd);
        Native.RoundCorners(hwnd);
        if (MicaController.IsSupported())
        {
            SystemBackdrop = new MicaBackdrop();
        }
        else
        {
            root.Background = Brand.System("SolidBackgroundFillColorBaseBrush");
        }
        // Closed only when the service says the code is done with.
        AppWindow.Closing += (_, e) => e.Cancel = !closing;
    }

    private bool closing;

    /// <summary>Shows <paramref name="pairing"/>'s code, opening the window where it was not open.</summary>
    public void Show(PairingState pairing, DateTimeOffset now)
    {
        state = pairing;
        title.Text = Strings.Format(TextKeys.PairingCodeTitle, pairing.PeerName);
        code.Text = PairingCode.Grouped(pairing.Code);
        AutomationProperties.SetName(code, PairingCode.Spoken(pairing.Code));
        Tick(now);
        if (!AppWindow.IsVisible)
        {
            Place();
            AppWindow.Show(activateWindow: false);
            AppLog.Info("showing a pairing code");
        }
    }

    /// <summary>Counts down to the code's expiry.</summary>
    public void Tick(DateTimeOffset now) =>
        countdown.Text = Strings.Format(TextKeys.PairingCodeValid, PairingCode.Countdown(state.ExpiresAtUnixMs, now));

    public void Dismiss()
    {
        closing = true;
        Close();
    }

    private void Place()
    {
        var area = DisplayArea.Primary;
        var work = area.WorkArea;
        double scale = Native.DpiForMonitor(Win32Interop.GetMonitorFromDisplayId(area.DisplayId)) / 96.0;
        root.Measure(new Windows.Foundation.Size(PanelWidth, double.PositiveInfinity));
        int w = (int)Math.Ceiling(PanelWidth * scale);
        int h = (int)Math.Ceiling(Math.Max(root.DesiredSize.Height, 200) * scale);
        int x = work.X + (work.Width - w) / 2;
        int y = work.Y + (work.Height - h) / 2;
        // Without a title bar the window still has side borders around the
        // content; measured, as the panel does.
        for (int attempt = 0; attempt < 2; attempt++)
        {
            var size = AppWindow.Size;
            var inside = AppWindow.ClientSize;
            int frameWidth = Math.Max(0, size.Width - inside.Width);
            int frameHeight = Math.Max(0, size.Height - inside.Height);
            AppWindow.MoveAndResize(new RectInt32(x - frameWidth / 2, y, w + frameWidth, h + frameHeight));
            if (AppWindow.ClientSize.Width == w && AppWindow.ClientSize.Height == h)
            {
                return;
            }
        }
    }
}
