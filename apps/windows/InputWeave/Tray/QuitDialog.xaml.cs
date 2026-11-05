using InputWeave.Panel;
using InputWeave.Support;
using Microsoft.UI;
using Microsoft.UI.Composition.SystemBackdrops;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;
using WinRT.Interop;

namespace InputWeave.Tray;

/// <summary>
/// Asks before quitting, in the macOS app's words as they apply on
/// Windows: quitting closes only the app, and the InputWeave
/// service goes on sharing.
/// </summary>
public sealed partial class QuitDialog : Window
{
    private readonly Action quit;
    private readonly nint hwnd;

    internal QuitDialog(Action quit)
    {
        InitializeComponent();
        this.quit = quit;
        hwnd = WindowNative.GetWindowHandle(this);
        TitleText.Text = Strings.Get(TextKeys.QuitTitle);
        MessageText.Text = Strings.Get(TextKeys.QuitMessage);
        QuitButton.Content = Strings.Get(TextKeys.QuitConfirm);
        CancelButton.Content = Strings.Get(TextKeys.QuitCancel);

        var presenter = OverlappedPresenter.CreateForDialog();
        presenter.IsResizable = false;
        presenter.IsMaximizable = false;
        presenter.IsMinimizable = false;
        presenter.IsAlwaysOnTop = true;
        AppWindow.SetPresenter(presenter);
        if (MicaController.IsSupported())
        {
            SystemBackdrop = new MicaBackdrop();
        }
        else
        {
            Root.Background = (Brush)Application.Current.Resources["SolidBackgroundFillColorBaseBrush"];
        }
    }

    /// <summary>Shows the dialog centered on the main screen, with Cancel focused.</summary>
    internal void Open()
    {
        Place();
        Activate();
        Native.SetForegroundWindow(hwnd);
        DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () =>
        {
            Place();
            CancelButton.Focus(FocusState.Programmatic);
        });
    }

    private void Place()
    {
        var work = DisplayArea.Primary.WorkArea;
        double scale = Native.DpiForMonitor(Win32Interop.GetMonitorFromDisplayId(DisplayArea.Primary.DisplayId)) / 96.0;
        Root.Measure(new Windows.Foundation.Size(Root.Width, double.PositiveInfinity));
        int w = (int)Math.Ceiling(Root.Width * scale);
        int h = (int)Math.Ceiling(Math.Max(Root.DesiredSize.Height, 160) * scale);
        AppWindow.ResizeClient(new SizeInt32(w, h));
        var size = AppWindow.Size;
        AppWindow.Move(new PointInt32(work.X + (work.Width - size.Width) / 2, work.Y + (work.Height - size.Height) / 2));
    }

    private void OnQuit(object sender, RoutedEventArgs e)
    {
        Close();
        quit();
    }

    private void OnCancel(object sender, RoutedEventArgs e) => Close();

    private void OnEscape(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        Close();
    }
}
