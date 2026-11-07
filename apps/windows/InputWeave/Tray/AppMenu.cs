using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using Microsoft.UI.Xaml.Controls;

namespace InputWeave.Tray;

/// <summary>What the menu's items do; the app supplies them.</summary>
internal sealed record MenuActions(
    Action TogglePause, Action ToggleOffline, Action ToggleLock, Action IdentifyScreens, Action AddDevice, Action RunSetup,
    Action Settings, Action CheckForUpdates, Action About, Action Quit, Action<string> SelectProfile, Action ManageProfiles);

/// <summary>
/// The menu the tray icon's right click and the panel's More button both
/// show: the Profile submenu (Automatic, each profile, the chosen one
/// checked, Manage Profiles…; from a service that knows profiles), Pause or
/// Resume Sharing, Take This Computer Offline or Bring Back
/// Online (from a service that knows it), Lock to This Computer or Unlock,
/// Identify Screens, Add Device…, Run Setup Again…, Settings…, Check for
/// Updates…, About InputWeave, Quit InputWeave….
/// Each place has its own instance.
/// </summary>
internal sealed class AppMenu
{
    private readonly MenuFlyout flyout;
    private readonly MenuFlyoutItem pause = new();
    private readonly MenuFlyoutItem offline = new();
    private readonly MenuFlyoutItem lockItem = new();
    private readonly MenuFlyoutItem identify = new() { Text = Strings.Get(TextKeys.IdentifyScreens) };
    private readonly RelayCommand pauseCommand;
    private readonly RelayCommand offlineCommand;
    private readonly RelayCommand lockCommand;
    private readonly RelayCommand identifyCommand;
    private readonly MenuActions actions;
    private readonly MenuFlyoutSubItem profiles = new() { Text = Strings.Get(TextKeys.MenuProfile) };
    private readonly MenuFlyoutSeparator afterProfiles = new();
    private MenuState state = new(TextKeys.MenuPauseSharing, false, TextKeys.MenuLock, false, false);

    public AppMenu(MenuFlyout flyout, MenuActions actions)
    {
        this.flyout = flyout;
        this.actions = actions;
        pauseCommand = new RelayCommand(actions.TogglePause, () => state.PauseEnabled);
        offlineCommand = new RelayCommand(actions.ToggleOffline, () => state.OfflineEnabled);
        lockCommand = new RelayCommand(actions.ToggleLock, () => state.LockEnabled);
        identifyCommand = new RelayCommand(actions.IdentifyScreens, () => state.IdentifyEnabled);
        pause.Command = pauseCommand;
        offline.Command = offlineCommand;
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
        offlineCommand.Refresh();
        lockCommand.Refresh();
        identifyCommand.Refresh();
        // The tray's Win32 menu is built from Text and IsEnabled when it
        // opens, and runs the Command; say it outright, not only through
        // the command.
        pause.IsEnabled = next.PauseEnabled;
        lockItem.IsEnabled = next.LockEnabled;
        identify.IsEnabled = next.IdentifyEnabled;
        // Taking this computer offline comes right after pausing, and only
        // from a service that knows it: added and taken out rather than
        // hidden, as the Win32 menu is built from the items there are.
        int at = flyout.Items.IndexOf(offline);
        if (next.OfflineKey is { } key)
        {
            offline.Text = Strings.Get(key);
            offline.IsEnabled = next.OfflineEnabled;
            if (at < 0)
            {
                flyout.Items.Insert(flyout.Items.IndexOf(pause) + 1, offline);
            }
        }
        else if (at >= 0)
        {
            flyout.Items.RemoveAt(at);
        }
        ShowProfiles(next.Profiles);
    }

    /// <summary>
    /// The Profile submenu first, then a line; added and taken out rather
    /// than hidden, as the Win32 menu is built from the items there are.
    /// Its items are made again whenever they change.
    /// </summary>
    private void ShowProfiles(ProfileMenu? menu)
    {
        int at = flyout.Items.IndexOf(profiles);
        if (menu is null)
        {
            if (at >= 0)
            {
                flyout.Items.Remove(afterProfiles);
                flyout.Items.Remove(profiles);
            }
            return;
        }
        if (at < 0)
        {
            flyout.Items.Insert(0, afterProfiles);
            flyout.Items.Insert(0, profiles);
        }
        profiles.Items.Clear();
        foreach (var choice in menu.Choices)
        {
            var item = new ToggleMenuFlyoutItem { Text = Strings.Text(choice.Name), IsChecked = choice.Checked, IsEnabled = menu.Enabled };
            var id = choice.Id;
            var chosen = choice.Checked;
            item.Command = new RelayCommand(() =>
            {
                // Choosing one checks it once the service says so; the check
                // a click moved goes back until then.
                item.IsChecked = chosen;
                if (!chosen)
                {
                    actions.SelectProfile(id);
                }
            }, () => menu.Enabled);
            profiles.Items.Add(item);
        }
        profiles.Items.Add(new MenuFlyoutSeparator());
        profiles.Items.Add(Item(TextKeys.MenuManageProfiles, actions.ManageProfiles));
    }
}
