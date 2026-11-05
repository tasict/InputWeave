namespace InputWeave.Settings;

/// <summary>
/// Opening the app at login, through the current user's Run key
/// (<c>HKCU\Software\Microsoft\Windows\CurrentVersion\Run</c>, value
/// <see cref="ValueName"/>). Task Manager's Startup apps page can turn the
/// entry off without removing it; it records that in
/// <c>...\Explorer\StartupApproved\Run</c> under the same value name. The
/// app reads and writes the registry; the decisions are here.
/// </summary>
public static class LoginLaunch
{
    public const string RunKey = @"Software\Microsoft\Windows\CurrentVersion\Run";
    public const string ApprovedKey = @"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

    /// <summary>The value name, also for the installer, which may set it too.</summary>
    public const string ValueName = "InputWeave";

    /// <summary>The Run value for the app at <paramref name="exePath"/>: its path, quoted.</summary>
    public static string Command(string exePath) => $"\"{exePath}\"";

    /// <summary>
    /// Whether Windows opens the app at login: the Run value names a
    /// program, and Task Manager has not turned it off. Its first byte is
    /// even while the entry is on (2, or 6 for some entries) and odd once it
    /// was turned off (3).
    /// </summary>
    public static bool IsOn(string? runValue, byte[]? approved) =>
        !string.IsNullOrWhiteSpace(runValue) && (approved is not { Length: > 0 } || approved[0] % 2 == 0);

    /// <summary>
    /// What Task Manager writes for an entry that is on: 2, then zeros where
    /// it keeps the time an entry was turned off.
    /// </summary>
    public static byte[] Approved() => [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
}
