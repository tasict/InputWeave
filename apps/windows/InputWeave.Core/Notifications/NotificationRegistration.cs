namespace InputWeave.Notifications;

/// <summary>
/// Where the Windows App SDK registers an unpackaged app for notifications,
/// under <c>HKCU\Software\Classes</c>: a key that names the app by its
/// path and holds <c>NotificationGUID</c>; the registration under that GUID,
/// with <c>CustomActivator</c>; and that COM class, whose
/// <c>LocalServer32</c> starts the app for a click. The uninstaller takes
/// all three back (<see cref="Support.LaunchArguments.Unregister"/>) and does
/// not rely on the SDK's own <c>UnregisterAll</c> for it, which has been
/// seen to leave the last two behind. The app reads and deletes the keys;
/// which ones is decided here.
/// </summary>
public static class NotificationRegistration
{
    public const string AppUserModelIdRoot = @"Software\Classes\AppUserModelId";
    public const string ClsidRoot = @"Software\Classes\CLSID";

    /// <summary>The value of the path key that names the registration.</summary>
    public const string GuidValue = "NotificationGUID";

    /// <summary>The value of the registration that names the COM class.</summary>
    public const string ActivatorValue = "CustomActivator";

    /// <summary>
    /// The key that names the app at <paramref name="exePath"/>: the path in
    /// lower case with "\" as "." (<c>c:.program files.inputweave.app.inputweave.exe</c>).
    /// </summary>
    public static string PathKey(string exePath) =>
        $@"{AppUserModelIdRoot}\{exePath.ToLowerInvariant().Replace('\\', '.')}";

    /// <summary>The registration the path key names, or null for anything but a braced GUID.</summary>
    public static string? RegistrationKey(string? notificationGuid) =>
        IsBracedGuid(notificationGuid) ? $@"{AppUserModelIdRoot}\{notificationGuid}" : null;

    /// <summary>The COM class the registration names, or null for anything but a braced GUID.</summary>
    public static string? ActivatorKey(string? customActivator) =>
        IsBracedGuid(customActivator) ? $@"{ClsidRoot}\{customActivator}" : null;

    /// <summary>
    /// Whether a <c>LocalServer32</c> command line starts the app at
    /// <paramref name="exePath"/>, quoted or not: only then is the COM class
    /// the app's to remove.
    /// </summary>
    public static bool StartsApp(string? localServer, string exePath)
    {
        if (localServer is null || exePath.Length == 0)
        {
            return false;
        }
        var line = localServer.TrimStart();
        bool quoted = line.StartsWith('"');
        if (quoted)
        {
            line = line[1..];
        }
        if (!line.StartsWith(exePath, StringComparison.OrdinalIgnoreCase))
        {
            return false;
        }
        var rest = line[exePath.Length..];
        return quoted ? rest.StartsWith('"') : rest.Length == 0 || rest.StartsWith(' ');
    }

    private static bool IsBracedGuid(string? value) =>
        value is { Length: 38 } && value[0] == '{' && value[^1] == '}' && Guid.TryParse(value, out _);
}
