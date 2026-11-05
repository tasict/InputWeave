namespace InputWeave.Support;

/// <summary>
/// The arguments the app understands that come from Windows or the
/// background service rather than from tests: File Explorer's Send To, a
/// click on one of the app's notifications that started it, and the
/// broker starting it for the user to see something.
/// </summary>
public static class LaunchArguments
{
    /// <summary>
    /// Started by the background service (the broker, as the user) because
    /// the user has to see or answer something, such as a pairing code or a
    /// file offer. The app then opens nothing on its own, not even
    /// first-run setup: only the icon, and the pairing code window and the
    /// notifications that what the service reports calls for. A running app
    /// that gets it from a later launch does nothing.
    /// </summary>
    public const string Background = "--background";

    /// <summary>
    /// Sends the files that follow to another computer. The Send To
    /// shortcut has it as its argument; File Explorer appends the selected
    /// items, each as an argument of its own.
    /// </summary>
    public const string Send = "--send";

    /// <summary>What Windows passes when a click on a notification starts the app.</summary>
    public const string NotificationActivated = "----AppNotificationActivated:";

    /// <summary>
    /// Takes back what the app registered for the current user: its
    /// notifications and its Send To shortcut. For the uninstaller; the
    /// app does nothing else and quits.
    /// </summary>
    public const string Unregister = "--unregister";

    /// <summary>The paths after <see cref="Send"/>, or null when it is not there.</summary>
    public static IReadOnlyList<string>? SendPaths(IReadOnlyList<string> args)
    {
        for (int i = 0; i < args.Count; i++)
        {
            if (args[i] == Send)
            {
                return args.Skip(i + 1).Where(a => a.Length != 0).ToList();
            }
        }
        return null;
    }

    /// <summary>
    /// Whether the app was started for something of its own: to send files,
    /// for a notification, or by the background service. It then opens no
    /// setup window.
    /// </summary>
    public static bool HasPurpose(IReadOnlyList<string> args) =>
        args.Any(a => a == Send || a == Background || a.StartsWith(NotificationActivated, StringComparison.Ordinal));

    /// <summary>Whether the background service started the app (<see cref="Background"/>).</summary>
    public static bool InBackground(IReadOnlyList<string> args) =>
        SendPaths(args) is null && args.Contains(Background);

    /// <summary>
    /// What a later launch, handed over by <paramref name="args"/>, asks
    /// of the app already running. <paramref name="setupDue"/>: setup has
    /// not been finished and has not opened since the app started, as when
    /// it was started for something else (in the background, by Send To or
    /// for a notification).
    /// </summary>
    public static Relaunch ForRunningApp(IReadOnlyList<string> args, bool setupDue)
    {
        if (SendPaths(args) is not null)
        {
            return Relaunch.Send;
        }
        if (args.Contains(Background))
        {
            return Relaunch.Nothing;
        }
        // Opened by the user, as from the Start menu: the first time since
        // the app started for something else, that is when setup shows.
        return args.Count == 0 && setupDue ? Relaunch.OpenSetup : Relaunch.OpenPanel;
    }

    /// <summary>
    /// The arguments as one message for the app already running, which
    /// handles them as if it had been started with them. Windows paths
    /// cannot hold control characters, so a line break separates them.
    /// </summary>
    public static string Encode(IReadOnlyList<string> args) => string.Join('\n', args);

    public static IReadOnlyList<string> Decode(string message) =>
        message.Length == 0 ? [] : message.Split('\n');
}

/// <summary>What a later launch asks of the app already running (<see cref="LaunchArguments.ForRunningApp"/>).</summary>
public enum Relaunch
{
    /// <summary>The user opened the app again: the panel opens, and a hidden icon comes back.</summary>
    OpenPanel,

    /// <summary>
    /// The user opened the app for the first time since it started for
    /// something else (in the background, by Send To, for a notification),
    /// before setup was finished.
    /// </summary>
    OpenSetup,

    /// <summary>File Explorer's Send To: the files are sent.</summary>
    Send,

    /// <summary>The background service started the app again; it runs already, and opens nothing.</summary>
    Nothing,
}
