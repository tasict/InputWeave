using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;

namespace InputWeave.Settings;

/// <summary>Why files can't go to a computer now.</summary>
public enum SendUnavailable
{
    /// <summary>It was taken offline for now, here or anywhere in the group.</summary>
    OfflineForNow,

    /// <summary>It is not connected.</summary>
    Offline,

    /// <summary>File transfer with it is turned off here (<c>SetDeviceSharing</c>).</summary>
    FilesOff,
}

/// <summary>
/// A computer in the picker: "Connected · 3 ms" for one that can take
/// files, else why not.
/// </summary>
public sealed record SendTarget(string Id, string Name, string Os, UiText Status, bool Available);

/// <summary>What sending does with the computers there are.</summary>
public abstract record SendChoice
{
    /// <summary>No computer is connected: say so, with only OK.</summary>
    public sealed record Nowhere : SendChoice;

    /// <summary>Exactly one can take files: send to it at once.</summary>
    public sealed record Only(string Id) : SendChoice;

    /// <summary>
    /// Ask which, listing every other computer (<see cref="SendTargets.Targets"/>):
    /// more than one can take files, or computers are connected but none
    /// can, each with why (and Send off).
    /// </summary>
    public sealed record Ask : SendChoice;
}

/// <summary>
/// Which computer files go to from Send To and Send Files…: the computers
/// that can take them, why the others can't, and the words for the choice,
/// as the macOS app's <c>SendTargets</c> for Finder's Services menu.
/// </summary>
public static class SendTargets
{
    /// <summary>
    /// Why files can't go to <paramref name="device"/> now, the first reason
    /// that applies; null when they can. Offline for now comes before
    /// offline, which the service reports it as too; while this computer is
    /// offline for now, every other one is just offline.
    /// </summary>
    public static SendUnavailable? Unavailable(DeviceStatus device) =>
        device.OfflineForNow ? SendUnavailable.OfflineForNow
        : device.State != ConnectionState.Connected ? SendUnavailable.Offline
        : device.FilesOff ? SendUnavailable.FilesOff
        : null;

    /// <summary>What the picker says under a computer files can't go to.</summary>
    public static UiText Reason(SendUnavailable why) => why switch
    {
        SendUnavailable.OfflineForNow => new(TextKeys.ConnectionOfflineForNow),
        SendUnavailable.FilesOff => new(TextKeys.SendReasonFilesOff),
        _ => new(TextKeys.ConnectionOffline),
    };

    /// <summary>Every other computer, those files can go to first, each group in the service's order.</summary>
    public static IReadOnlyList<SendTarget> Targets(AgentSnapshot model)
    {
        var all = model.Status.Devices.Select(d =>
        {
            var why = Unavailable(d);
            var status = why is { } reason
                ? Reason(reason)
                : DeviceText.Connected(d.RttMs, d.ScreenLocked, d.LockScreenInput, d.InputUnavailable, d.SharingPaused);
            return new SendTarget(d.Id, d.Name, d.Os, status, why is null);
        }).ToList();
        return [.. all.Where(t => t.Available), .. all.Where(t => !t.Available)];
    }

    public static SendChoice Choose(AgentSnapshot model)
    {
        if (model.ConnectedDevices.Count == 0)
        {
            return new SendChoice.Nowhere();
        }
        var available = model.Status.Devices.Where(d => Unavailable(d) is null).ToList();
        return available.Count == 1 ? new SendChoice.Only(available[0].Id) : new SendChoice.Ask();
    }

    /// <summary>
    /// The computer the picker shows chosen as the computers change: the
    /// one the user clicked (<paramref name="clicked"/>) while files can go
    /// to it, which comes back when it can again; else the one chosen until
    /// now (<paramref name="shown"/>, by the user or by default) while it
    /// can; else the first that can take files; null when none can. So a
    /// computer that connects ahead of the one shown never takes its
    /// place, and a default the choice moved away from does not come back.
    /// </summary>
    public static SendTarget? Chosen(string? clicked, string? shown, IReadOnlyList<SendTarget> targets) =>
        targets.FirstOrDefault(t => t.Id == clicked && t.Available)
        ?? targets.FirstOrDefault(t => t.Id == shown && t.Available)
        ?? targets.FirstOrDefault(t => t.Available);

    /// <summary>
    /// The items of a request that arrives while the picker is open, added
    /// to those it already has: each path once, in the order they came.
    /// Paths are compared as Windows does, without regard to case, with
    /// <c>.</c> and <c>..</c> parts resolved and no trailing separator.
    /// </summary>
    public static IReadOnlyList<string> Merged(IReadOnlyList<string> open, IReadOnlyList<string> more) =>
        open.Concat(more).DistinctBy(Standardized, StringComparer.OrdinalIgnoreCase).ToList();

    private static string Standardized(string path)
    {
        try
        {
            return Path.TrimEndingDirectorySeparator(Path.GetFullPath(path));
        }
        catch (Exception e) when (e is ArgumentException or NotSupportedException or PathTooLongException)
        {
            return path;
        }
    }

    /// <summary>
    /// Under the picker's title: "Report.pdf", "Report.pdf and 1 more
    /// item", "Report.pdf and 2 more items".
    /// </summary>
    public static UiText Summary(IReadOnlyList<string> paths)
    {
        if (paths.Count == 0)
        {
            return new(TextKeys.Verbatim, "");
        }
        var name = Path.GetFileName(paths[0].TrimEnd('\\', '/'));
        if (string.IsNullOrEmpty(name))
        {
            name = paths[0];
        }
        return (paths.Count - 1) switch
        {
            0 => new(TextKeys.Verbatim, name),
            1 => new(TextKeys.SendSummaryOneMore, name),
            var more => new(TextKeys.SendSummaryMore, name, more.ToString()),
        };
    }
}
