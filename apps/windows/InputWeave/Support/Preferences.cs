using System.Security;
using InputWeave.Settings;
using Microsoft.Win32;

namespace InputWeave.Support;

/// <summary>
/// The app's own choices, under <c>HKCU\Software\InputWeave\App</c>, and
/// opening at login through the current user's Run key. Everything the
/// background service needs to know is the service's to keep.
/// </summary>
internal static class Preferences
{
    private const string ProductKey = @"Software\InputWeave";
    private const string AppKey = ProductKey + @"\App";

    /// <summary>Whether the notification area icon shows; opening the app again brings it back.</summary>
    public static bool ShowTrayIcon
    {
        get => Read(AppKey, "ShowTrayIcon") is not int shown || shown != 0;
        set => Write(AppKey, "ShowTrayIcon", value ? 1 : 0, RegistryValueKind.DWord);
    }

    /// <summary>
    /// Whether first-run setup was finished, or put off with Later;
    /// until then the app opens it at launch.
    /// </summary>
    public static bool FinishedSetup
    {
        get => Read(AppKey, "FinishedSetup") is int done && done != 0;
        set => Write(AppKey, "FinishedSetup", value ? 1 : 0, RegistryValueKind.DWord);
    }

    /// <summary>
    /// Whether the user quit the app (Exit InputWeave…) and has not opened
    /// it since: the broker then does not start it for files copied on
    /// another computer, only for what the user has to see, such as a
    /// pairing code (docs/architecture.md §9). Set as the user quits it,
    /// cleared whenever it is opened other than by the broker.
    /// </summary>
    public static bool QuitByUser
    {
        get => Read(AppKey, "QuitByUser") is int quit && quit != 0;
        set
        {
            if (value != QuitByUser)
            {
                Write(AppKey, "QuitByUser", value ? 1 : 0, RegistryValueKind.DWord);
            }
        }
    }

    /// <summary>
    /// Whether the edge lights up where the cursor crosses to or from
    /// another computer; on unless turned off.
    /// </summary>
    public static bool ShowEdgeGlow
    {
        get => Read(AppKey, "ShowEdgeGlow") is not int shown || shown != 0;
        set => Write(AppKey, "ShowEdgeGlow", value ? 1 : 0, RegistryValueKind.DWord);
    }

    /// <summary>How many times the HUD showed the take-back hint; it stops at three.</summary>
    public static int TakeBackHints
    {
        get => Read(AppKey, "TakeBackHints") is int shown ? shown : 0;
        set => Write(AppKey, "TakeBackHints", value, RegistryValueKind.DWord);
    }

    /// <summary>
    /// Whether the app has put itself in File Explorer's Send To menu once,
    /// on its first run. After that the shortcut is the user's to keep or
    /// remove, in General or by hand.
    /// </summary>
    public static bool OfferedSendTo
    {
        get => Read(AppKey, "OfferedSendTo") is int done && done != 0;
        set => Write(AppKey, "OfferedSendTo", value ? 1 : 0, RegistryValueKind.DWord);
    }

    /// <summary>
    /// The transfer problems taken off the panel (<see cref="Panel.DismissedTransfers"/>),
    /// by transfer id, so they stay off after the app restarts. Only ids the
    /// background service still lists are kept.
    /// </summary>
    public static IReadOnlyCollection<string> DismissedTransfers
    {
        get => Read(AppKey, "DismissedTransfers") as string[] ?? [];
        set => Write(AppKey, "DismissedTransfers", value.Order(StringComparer.Ordinal).ToArray(), RegistryValueKind.MultiString);
    }

    /// <summary>
    /// Removes the app's own choices, for the uninstaller, and
    /// <c>HKCU\Software\InputWeave</c> with them once nothing else is in it.
    /// </summary>
    public static void Forget()
    {
        try
        {
            Registry.CurrentUser.DeleteSubKeyTree(AppKey, throwOnMissingSubKey: false);
            bool empty;
            using (var key = Registry.CurrentUser.OpenSubKey(ProductKey))
            {
                empty = key is { SubKeyCount: 0, ValueCount: 0 };
            }
            if (empty)
            {
                Registry.CurrentUser.DeleteSubKey(ProductKey, throwOnMissingSubKey: false);
            }
            AppLog.Info("removed the app's settings");
        }
        catch (Exception e) when (e is SecurityException or UnauthorizedAccessException or IOException)
        {
            AppLog.Warn($"cannot remove the app's settings: {e.Message}");
        }
    }

    /// <summary>Whether Windows opens this app when the user logs in.</summary>
    public static bool OpensAtLogin =>
        LoginLaunch.IsOn(Read(LoginLaunch.RunKey, LoginLaunch.ValueName) as string, Read(LoginLaunch.ApprovedKey, LoginLaunch.ValueName) as byte[]);

    public static void SetOpensAtLogin(bool on)
    {
        try
        {
            using var run = Registry.CurrentUser.CreateSubKey(LoginLaunch.RunKey);
            if (on && Environment.ProcessPath is { } exe)
            {
                run.SetValue(LoginLaunch.ValueName, LoginLaunch.Command(exe), RegistryValueKind.String);
                // Turned off in Task Manager before: on again.
                using var approved = Registry.CurrentUser.CreateSubKey(LoginLaunch.ApprovedKey);
                approved.SetValue(LoginLaunch.ValueName, LoginLaunch.Approved(), RegistryValueKind.Binary);
            }
            else
            {
                run.DeleteValue(LoginLaunch.ValueName, throwOnMissingValue: false);
                using var approved = Registry.CurrentUser.OpenSubKey(LoginLaunch.ApprovedKey, writable: true);
                approved?.DeleteValue(LoginLaunch.ValueName, throwOnMissingValue: false);
            }
            AppLog.Info($"open at login: {on}");
        }
        catch (Exception e) when (e is SecurityException or UnauthorizedAccessException or IOException)
        {
            AppLog.Warn($"cannot change opening at login: {e.Message}");
        }
    }

    /// <summary>General › Updates › "Check for updates automatically"; on unless turned off.</summary>
    public static bool CheckForUpdates
    {
        get => Read(AppKey, "CheckForUpdates") is not int on || on != 0;
        set => Write(AppKey, "CheckForUpdates", value ? 1 : 0, RegistryValueKind.DWord);
    }

    /// <summary>The version the user chose Skip This Version for; automatic checks say nothing of it.</summary>
    public static string? SkippedUpdate
    {
        get => Read(AppKey, "SkippedUpdate") as string;
        set => Write(AppKey, "SkippedUpdate", value ?? "", RegistryValueKind.String);
    }

    /// <summary>The version a notification was posted for, so each version is announced once.</summary>
    public static string? NotifiedUpdate
    {
        get => Read(AppKey, "NotifiedUpdate") as string;
        set => Write(AppKey, "NotifiedUpdate", value ?? "", RegistryValueKind.String);
    }

    /// <summary>When the feed was last read, and when reading it last failed.</summary>
    public static DateTimeOffset? LastUpdateCheck
    {
        get => Time("LastUpdateCheck");
        set => Write(AppKey, "LastUpdateCheck", value?.ToUnixTimeSeconds() ?? 0, RegistryValueKind.QWord);
    }

    public static DateTimeOffset? LastUpdateFailure
    {
        get => Time("LastUpdateFailure");
        set => Write(AppKey, "LastUpdateFailure", value?.ToUnixTimeSeconds() ?? 0, RegistryValueKind.QWord);
    }

    private static DateTimeOffset? Time(string name) =>
        Read(AppKey, name) is long seconds && seconds > 0 ? DateTimeOffset.FromUnixTimeSeconds(seconds) : null;

    private static object? Read(string key, string name)
    {
        try
        {
            using var opened = Registry.CurrentUser.OpenSubKey(key);
            return opened?.GetValue(name);
        }
        catch (Exception e) when (e is SecurityException or UnauthorizedAccessException or IOException)
        {
            return null;
        }
    }

    private static void Write(string key, string name, object value, RegistryValueKind kind)
    {
        try
        {
            using var opened = Registry.CurrentUser.CreateSubKey(key);
            opened.SetValue(name, value, kind);
        }
        catch (Exception e) when (e is SecurityException or UnauthorizedAccessException or IOException)
        {
            AppLog.Warn($"cannot save {name}: {e.Message}");
        }
    }
}
