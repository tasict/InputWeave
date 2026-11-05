using H.NotifyIcon;
using InputWeave.Panel;
using InputWeave.Support;
using Microsoft.UI.Xaml.Controls;

namespace InputWeave.Tray;

/// <summary>
/// The notification area icon: left click opens
/// the panel, right click the menu. Its glyph follows the taskbar's theme.
/// General › "Show the icon in the system tray" can take it away; opening
/// the app again brings it back.
/// </summary>
internal sealed class TrayIcon : IDisposable
{
    private readonly Action open;
    private readonly MenuActions actions;
    private TaskbarIcon? icon;
    private AppMenu? menu;
    private TrayIconState? shownState;
    private bool shownLight;
    private System.Drawing.Icon? shownIcon;
    private MenuState? lastMenu;

    public TrayIcon(Action open, MenuActions actions, bool shown)
    {
        this.open = open;
        this.actions = actions;
        Shown = shown;
    }

    public bool Shown
    {
        get => icon is not null;
        set
        {
            if (value == Shown)
            {
                return;
            }
            if (value)
            {
                Create();
            }
            else
            {
                icon?.Dispose();
                icon = null;
                menu = null;
                shownState = null;
            }
            AppLog.Info($"tray icon shown: {value}");
        }
    }

    private void Create()
    {
        var flyout = new MenuFlyout();
        menu = new AppMenu(flyout, actions);
        if (lastMenu is { } state)
        {
            menu.Update(state);
        }
        icon = new TaskbarIcon
        {
            ToolTipText = "InputWeave",
            // A plain Win32 menu: it opens at once and needs no window.
            ContextMenuMode = ContextMenuMode.PopupMenu,
            ContextFlyout = flyout,
            NoLeftClickDelay = true,
            LeftClickCommand = new RelayCommand(open),
        };
        icon.ForceCreate(enablesEfficiencyMode: false);
    }

    public void Update(TrayIconState state, bool lightTaskbar, MenuState menuState)
    {
        lastMenu = menuState;
        menu?.Update(menuState);
        if (icon is null || (state == shownState && lightTaskbar == shownLight))
        {
            return;
        }
        var file = Path.Combine(AppContext.BaseDirectory, "Assets", "Tray", TrayIcons.FileName(state, lightTaskbar));
        // The small icon size at the system's scale: 16 px at 100 %, 24 at 150 %.
        var size = Native.GetSystemMetricsForDpi(Native.SM_CXSMICON, Native.GetDpiForSystem());
        var next = new System.Drawing.Icon(file, size, size);
        icon.Icon = next;
        icon.ToolTipText = Strings.Text(TrayIcons.Description(state));
        shownIcon?.Dispose();
        shownIcon = next;
        if (shownState != state)
        {
            AppLog.Info($"tray icon: {state}");
        }
        shownState = state;
        shownLight = lightTaskbar;
    }

    public void Dispose()
    {
        icon?.Dispose();
        shownIcon?.Dispose();
    }
}
