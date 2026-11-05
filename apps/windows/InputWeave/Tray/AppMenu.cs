using InputWeave.Panel;
using InputWeave.Support;
using Microsoft.UI.Xaml.Controls;

namespace InputWeave.Tray;

/// <summary>What the menu's items do; the app supplies them.</summary>
internal sealed record MenuActions(
    Action TogglePause, Action ToggleLock, Action IdentifyScreens, Action AddDevice, Action RunSetup, Action Settings,
    Action CheckForUpdates, Action About, Action Quit);

/// <summary>
/// The menu the tray icon's right click and the panel's More button both
/// show: Pause or Resume Sharing, Lock to This Computer or Unlock, Identify
/// Screens, Add Device…, Run Setup Again…, Settings…, Check for Updates…,
/// About InputWeave, Quit InputWeave….
/// Each place has its own instance.
/// </summary>
internal sealed class AppMenu
{
    private readonly MenuFlyoutItem pause = new();
    private readonly MenuFlyoutItem lockItem = new();
    private readonly MenuFlyoutItem identify = new() { Text = Strings.Get(TextKeys.IdentifyScreens) };
    private readonly RelayCommand pauseCommand;
    private readonly RelayCommand lockCommand;
    private readonly RelayCommand identifyCommand;
    private MenuState state = new(TextKeys.MenuPauseSharing, false, TextKeys.MenuLock, false, false);

    public AppMenu(MenuFlyout flyout, MenuActions actions)
    {
        pauseCommand = new RelayCommand(actions.TogglePause, () => state.PauseEnabled);
        lockCommand = new RelayCommand(actions.ToggleLock, () => state.LockEnabled);
        identifyCommand = new RelayCommand(actions.IdentifyScreens, () => state.IdentifyEnabled);
        pause.Command = pauseCommand;
        lockItem.Command = lockCommand;
        identify.Command = identifyCommand;
        flyout.Items.Add(pause);
        flyout.Items.Add(lockItem);
        flyout.Items.Add(identify);
        flyout.Items.Add(Item(TextKeys.MenuAddDevice, actions.AddDevice));
        flyout.Items.Add(Item(TextKeys.MenuRunSetup, actions.RunSetup));
        flyout.Items.Add(new MenuFlyoutSeparator());
        flyout.Items.Add(Item(TextKeys.MenuSettings, actions.Settings));
        flyout.Items.Add(Item(TextKeys.MenuCheckUpdates, actions.CheckForUpdates));
        flyout.Items.Add(Item(TextKeys.MenuAbout, actions.About));
        flyout.Items.Add(new MenuFlyoutSeparator());
        flyout.Items.Add(Item(TextKeys.MenuQuit, actions.Quit));
        Update(state, force: true);
    }

    private static MenuFlyoutItem Item(string key, Action action) =>
        new() { Text = Strings.Get(key), Command = new RelayCommand(action) };

    public void Update(MenuState next, bool force = false)
    {
        if (!force && next == state)
        {
            return;
        }
        state = next;
        pause.Text = Strings.Get(next.PauseKey);
        lockItem.Text = Strings.Get(next.LockKey);
        pauseCommand.Refresh();
        lockCommand.Refresh();
        identifyCommand.Refresh();
        // The tray's Win32 menu is built from Text and IsEnabled when it
        // opens, and runs the Command; say it outright, not only through
        // the command.
        pause.IsEnabled = next.PauseEnabled;
        lockItem.IsEnabled = next.LockEnabled;
        identify.IsEnabled = next.IdentifyEnabled;
    }
}
