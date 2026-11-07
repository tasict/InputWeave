using System.Globalization;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Support;

namespace InputWeave.Settings;

/// <summary>
/// How computers are described in the device list, the inspector and the
/// canvas, as on the Mac.
/// </summary>
public static class DeviceText
{
    /// <summary>
    /// "Connected", with the round-trip time once the service has measured
    /// it; "Screen locked" instead while the computer's screen is locked,
    /// and "Screen locked, can't take input" when input from here cannot
    /// reach it then, so the cursor stays off it. "Can't take input" when no
    /// input from here reaches it at all now, locked or not, as a Linux
    /// desktop that has not allowed remote control. "Paused sharing" when
    /// its user paused sharing there, which keeps the cursor off it too.
    /// </summary>
    public static UiText Connected(
        uint rttMs, bool screenLocked = false, bool lockScreenInput = true, bool inputUnavailable = false,
        bool sharingPaused = false) =>
        sharingPaused
            ? rttMs == 0 ? new(TextKeys.ConnectionPeerPaused) : new(TextKeys.ConnectionPeerPausedRtt, rttMs.ToString())
            : (inputUnavailable && !screenLocked, screenLocked && (!lockScreenInput || inputUnavailable), screenLocked, rttMs) switch
    {
        (true, _, _, 0) => new(TextKeys.ConnectionNoInput),
        (true, _, _, _) => new(TextKeys.ConnectionNoInputRtt, rttMs.ToString()),
        (_, true, _, 0) => new(TextKeys.ConnectionScreenLockedNoInput),
        (_, true, _, _) => new(TextKeys.ConnectionScreenLockedNoInputRtt, rttMs.ToString()),
        (_, _, true, 0) => new(TextKeys.ConnectionScreenLocked),
        (_, _, true, _) => new(TextKeys.ConnectionScreenLockedRtt, rttMs.ToString()),
        (_, _, false, 0) => new(TextKeys.ConnectionConnected),
        _ => new(TextKeys.ConnectionConnectedRtt, rttMs.ToString()),
    };

    /// <summary>The Segoe Fluent pause glyph, for a computer whose user paused sharing.</summary>
    public const string PausedGlyph = "\uE769";

    /// <summary>The Segoe Fluent padlock, for a computer whose screen is locked.</summary>
    public const string LockedGlyph = "\uE72E";

    /// <summary>
    /// The glyph beside a computer's status in the device list, by the same
    /// priority as its text: a pause when its user paused sharing, a
    /// padlock while its screen is locked; null for the plain dot (or the
    /// dashed ring when offline).
    /// </summary>
    public static string? StatusGlyph(bool connected, bool screenLocked, bool sharingPaused) =>
        !connected ? null : sharingPaused ? PausedGlyph : screenLocked ? LockedGlyph : null;

    /// <summary>Whether a connected computer's screen is locked, as it last said.</summary>
    public static bool ScreenLocked(AgentSnapshot model, string id) =>
        model.Device(id) is { State: ConnectionState.Connected, ScreenLocked: true };

    /// <summary>
    /// A computer's status: this one, offline for now (before anything
    /// else, without a latency), connected with its latency or locked, or
    /// offline; being woken, or since when it is offline when it can be
    /// woken (<see cref="Wake.OfflineStatus"/>), as of <paramref name="now"/>.
    /// </summary>
    public static UiText Status(AgentSnapshot model, string id, DateTimeOffset now, CultureInfo culture)
    {
        if (id == model.ThisDeviceId)
        {
            return new(TextKeys.ConnectionThisComputer);
        }
        if (OfflineForNow.IsOffline(model, id))
        {
            return new(TextKeys.ConnectionOfflineForNow);
        }
        return model.Device(id) is { State: ConnectionState.Connected } device
            ? Connected(device.RttMs, device.ScreenLocked, device.LockScreenInput, device.InputUnavailable, device.SharingPaused)
            : Wake.OfflineStatus(model, id, now, culture) ?? new(TextKeys.ConnectionOffline);
    }

    /// <summary>The operating system's name; the device's type is not known, so no logo either.</summary>
    public static UiText Os(string os) => os switch
    {
        "macos" => new(TextKeys.Verbatim, "macOS"),
        "windows" => new(TextKeys.Verbatim, "Windows"),
        "linux" => new(TextKeys.Verbatim, "Linux"),
        _ => new(TextKeys.OsUnknown),
    };

    /// <summary>"2560 × 1440": a screen's size in pixels.</summary>
    public static string Resolution(Screen screen) =>
        $"{Math.Round(screen.Width * Math.Max(screen.Scale, 1))} × {Math.Round(screen.Height * Math.Max(screen.Scale, 1))}";

    /// <summary>
    /// The modifier swap, named for the computer receiving the keys: ⌘ on
    /// a Mac, the Windows key elsewhere.
    /// </summary>
    public static string SwapKey(string os) =>
        os == "macos" ? TextKeys.SwapCommandAndControl : TextKeys.SwapWindowsAndControl;

    /// <summary>
    /// What a screen reader says for a computer on the canvas: its name,
    /// how many screens, and which computers it meets at a seam.
    /// </summary>
    public static UiText CanvasLabel(string name, int screens, IReadOnlyList<string> neighbours) =>
        neighbours.Count == 0
            ? new(TextKeys.CanvasDeviceAlone, name, screens.ToString())
            : new(TextKeys.CanvasDeviceNextTo, name, screens.ToString(), string.Join(", ", neighbours));

    /// <summary>"Files with “X” are turned off.": why files cannot go to or come from a computer.</summary>
    public static UiText FilesOff(string name) => new(TextKeys.DeviceFilesOff, name);

    /// <summary>
    /// Why files dragged over the canvas would not go to a computer: it is
    /// offline for now, files are turned off with it ("Files turned off" on
    /// the mini map, where room is short), or it is offline. Null for this
    /// computer, which says nothing.
    /// </summary>
    public static UiText? CannotReceive(AgentSnapshot model, string id, bool compact)
    {
        if (id == model.ThisDeviceId)
        {
            return null;
        }
        if (OfflineForNow.IsOffline(model, id))
        {
            return new(TextKeys.ConnectionOfflineForNow);
        }
        if (model.FilesOff(id))
        {
            return compact ? new(TextKeys.DeviceFilesOffShort) : FilesOff(model.NameOf(id));
        }
        return new(TextKeys.ConnectionOffline);
    }

    /// <summary>The background service's state, as Troubleshooting shows it.</summary>
    public static UiText ServiceStatus(ServiceState service, AgentLink link) => service switch
    {
        ServiceState.NotInstalled => new(TextKeys.ServiceNotInstalled),
        ServiceState.Stopped => new(TextKeys.ServiceStopped),
        ServiceState.Stopping => new(TextKeys.ServiceStopping),
        _ => link switch
        {
            AgentLink.Connected => new(TextKeys.ServiceRunning),
            AgentLink.Incompatible => new(TextKeys.ServiceUpdating),
            _ => new(TextKeys.ServiceStarting),
        },
    };

    /// <summary>
    /// "Synced to N devices" after the arrangement changed while other
    /// computers were connected; null when there is nothing to say, as at
    /// the first arrangement the app sees.
    /// </summary>
    public static UiText? Synced(ulong before, ulong after, AgentSnapshot model)
    {
        var connected = model.ConnectedDevices.Count;
        if (before == 0 || after == before || connected == 0)
        {
            return null;
        }
        return new(TextKeys.CanvasSynced, connected.ToString());
    }
}
