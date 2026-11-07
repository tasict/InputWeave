using Microsoft.UI;
using Microsoft.UI.Composition.SystemBackdrops;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;
using Windows.System;
using WinRT.Interop;

namespace InputWeave.Support;

/// <summary>A button of a <see cref="DialogWindow"/>; the dialog closes after its action.</summary>
internal sealed record DialogButton(string Text, Action? Action = null, bool Accent = false);

/// <summary>
/// A small dialog of its own, for what the panel or the main window has
/// to say or ask outside a window that could host a ContentDialog: a
/// title, a message, optional content, and buttons side by side, the last
/// one taking Enter and the first Esc when there is more than one. Like
/// the quit dialog: centered on the main screen and above other windows.
/// </summary>
internal sealed partial class DialogWindow : Window
{
    private readonly StackPanel root = new() { Width = 440, VerticalAlignment = VerticalAlignment.Top };
    private readonly List<Button> made = [];
    private readonly TextBlock? said;
    private Button? accent;
    /// <summary>Set once the dialog closed: what was queued to size it does nothing then.</summary>
    private bool closed;

    public DialogWindow(string title, string? message, UIElement? content, IReadOnlyList<DialogButton> buttons)
    {
        Title = "InputWeave";
        var body = new StackPanel { Padding = new Thickness(24, 20, 24, 24), Spacing = 12 };
        body.Children.Add(new TextBlock
        {
            Text = title,
            Style = (Style)Application.Current.Resources["SubtitleTextBlockStyle"],
            TextWrapping = TextWrapping.Wrap,
        });
        if (!string.IsNullOrEmpty(message))
        {
            said = new TextBlock
            {
                Text = message,
                Style = (Style)Application.Current.Resources["BodyTextBlockStyle"],
                TextWrapping = TextWrapping.Wrap,
                IsTextSelectionEnabled = true,
            };
            body.Children.Add(said);
        }
        if (content is not null)
        {
            body.Children.Add(content);
        }
        root.Children.Add(body);

        var bar = new Grid
        {
            Padding = new Thickness(24),
            ColumnSpacing = 8,
            Background = Brand.System("SolidBackgroundFillColorBaseBrush"),
            BorderBrush = Brand.System("CardStrokeColorDefaultBrush"),
            BorderThickness = new Thickness(0, 1, 0, 0),
        };
        for (int i = 0; i < Math.Max(buttons.Count, 2); i++)
        {
            bar.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        }
        // A single button sits on the right, as in Windows' own dialogs.
        int column = buttons.Count == 1 ? 1 : 0;
        foreach (var spec in buttons)
        {
            var button = new Button { Content = spec.Text, HorizontalAlignment = HorizontalAlignment.Stretch };
            if (spec.Accent)
            {
                button.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
                accent = button;
            }
            button.Click += (_, _) =>
            {
                Close();
                spec.Action?.Invoke();
            };
            Grid.SetColumn(button, column++);
            bar.Children.Add(button);
            made.Add(button);
        }
        root.Children.Add(bar);

        var keys = new KeyboardAccelerator { Key = VirtualKey.Escape };
        keys.Invoked += (_, e) =>
        {
            e.Handled = true;
            Close();
            if (buttons.Count > 1)
            {
                buttons[0].Action?.Invoke();
            }
        };
        root.KeyboardAccelerators.Add(keys);
        Content = root;
        Closed += (_, _) => closed = true;

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
            root.Background = Brand.System("SolidBackgroundFillColorBaseBrush");
        }
    }

    /// <summary>A notice with only OK.</summary>
    public static void Notice(string title, string? message, UIElement? content = null) =>
        new DialogWindow(title, message, content, [new DialogButton(Strings.Get(Panel.TextKeys.Ok), Accent: true)]).Open();

    /// <summary>Shows the dialog centered on the main screen, with the default button focused.</summary>
    public void Open()
    {
        Place();
        Activate();
        Native.SetForegroundWindow(WindowNative.GetWindowHandle(this));
        DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
        {
            Place();
            // The accent button, else the first, which cancels.
            (accent ?? made.FirstOrDefault())?.Focus(FocusState.Programmatic);
        });
    }

    /// <summary>Changes the message under the title, when the dialog has one.</summary>
    public void SetMessage(string message)
    {
        if (said is not null)
        {
            said.Text = message;
        }
    }

    /// <summary>Turns the button at <paramref name="index"/> (in the order given) on or off.</summary>
    public void SetEnabled(int index, bool enabled) => made[index].IsEnabled = enabled;

    /// <summary>
    /// Sizes the dialog to its content again where it is, now and once the
    /// content has been laid out (not if it closed meanwhile).
    /// </summary>
    public void Fit()
    {
        Resize();
        DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () => Resize());
    }

    /// <summary>Brings the dialog to the front, unless it closed.</summary>
    public void Raise()
    {
        if (closed)
        {
            return;
        }
        Activate();
        Native.SetForegroundWindow(WindowNative.GetWindowHandle(this));
    }

    /// <summary>Sizes the dialog to its content and centers it on the main screen, as it opens.</summary>
    private void Place()
    {
        if (closed)
        {
            return;
        }
        var work = DisplayArea.Primary.WorkArea;
        Resize(DisplayArea.Primary);
        var size = AppWindow.Size;
        AppWindow.Move(new PointInt32(work.X + (work.Width - size.Width) / 2, work.Y + (work.Height - size.Height) / 2));
    }

    /// <summary>Sizes the dialog to its content, keeping its top left where it is, at the scale of the screen it is on.</summary>
    private void Resize(DisplayArea? area = null)
    {
        if (closed)
        {
            return;
        }
        area ??= DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Primary);
        double scale = Native.DpiForMonitor(Win32Interop.GetMonitorFromDisplayId(area.DisplayId)) / 96.0;
        root.Measure(new Windows.Foundation.Size(root.Width, double.PositiveInfinity));
        int w = (int)Math.Ceiling(root.Width * scale);
        int h = (int)Math.Ceiling(Math.Max(root.DesiredSize.Height, 120) * scale);
        AppWindow.ResizeClient(new SizeInt32(w, h));
    }
}
