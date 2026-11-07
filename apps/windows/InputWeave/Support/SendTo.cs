using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Settings;

namespace InputWeave.Support;

/// <summary>
/// File Explorer's Send To: a shortcut named
/// InputWeave in the user's SendTo folder, which starts the app with
/// <see cref="LaunchArguments.Send"/> and the selected files. The app
/// asks which computer when more than one can receive them (listing those
/// that can't too, off, with why; <see cref="SendFiles.Deliver"/>), sends
/// the files, and opens the panel, where they show as they go. A running
/// app gets the files from the second launch (<see cref="SingleInstance"/>).
/// </summary>
internal static class SendTo
{
    /// <summary>How long a freshly started app waits for the background service before saying it cannot send.</summary>
    private static readonly TimeSpan ConnectWait = TimeSpan.FromSeconds(10);

    /// <summary><c>%APPDATA%\Microsoft\Windows\SendTo\InputWeave.lnk</c>.</summary>
    public static string ShortcutPath =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.SendTo), "InputWeave.lnk");

    public static bool IsOn => File.Exists(ShortcutPath);

    /// <summary>Puts the app in the Send To menu on its first run; after that the user decides, in General.</summary>
    public static void OfferOnFirstRun()
    {
        if (Preferences.OfferedSendTo)
        {
            return;
        }
        Set(true);
        Preferences.OfferedSendTo = true;
    }

    /// <summary>Adds the shortcut, pointing at this app, or removes it.</summary>
    public static void Set(bool on)
    {
        try
        {
            if (on && Environment.ProcessPath is { } exe)
            {
                Directory.CreateDirectory(Path.GetDirectoryName(ShortcutPath)!);
                ComCalls.SaveShortcut(ShortcutPath, exe, LaunchArguments.Send, "InputWeave");
            }
            else if (!on)
            {
                File.Delete(ShortcutPath);
            }
            AppLog.Info($"Send To shortcut: {on}");
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or System.Runtime.InteropServices.COMException)
        {
            AppLog.Warn($"cannot change the Send To shortcut: {e.Message}");
        }
    }

    /// <summary>Sends what File Explorer passed to a connected computer, then shows the panel.</summary>
    public static async void Send(IReadOnlyList<string> paths)
    {
        // Counted, not named: the log never holds file names.
        AppLog.Info($"Send To with {paths.Count} items");
        if (paths.Count == 0)
        {
            return;
        }
        var model = App.Instance.Model;
        // Started by Send To, the app is still connecting to the service.
        var deadline = DateTime.UtcNow + ConnectWait;
        while (model.Snapshot.Link != AgentLink.Connected && DateTime.UtcNow < deadline)
        {
            await Task.Delay(100);
        }
        var snapshot = model.Snapshot;
        if (snapshot.Link != AgentLink.Connected)
        {
            DialogWindow.Notice(Strings.Get(TextKeys.TitleNotResponding), null);
            return;
        }
        SendFiles.Deliver(paths, App.Instance.ShowPanel);
    }
}
