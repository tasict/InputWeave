using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;

namespace InputWeave.Settings;

/// <summary>
/// Computers taken offline for now (SetOfflineForNow), as the inspector, the
/// device list, the canvas with its context menu, the panel and the menu
/// show them, the same as on the Mac and Linux. The service decides all of
/// it: a computer taken offline for now arrives offline (and can't be woken),
/// and while this one is, every other one arrives plainly offline; this only
/// says what the app adds for that. A service from before (Status.offline_supported
/// false) gets none of it.
/// </summary>
public static class OfflineForNow
{
    /// <summary>The Segoe Fluent network with a slash (NetworkOffline), after the name of a computer offline for now.</summary>
    public const string Glyph = "";

    /// <summary>Whether the service knows computers taken offline for now.</summary>
    public static bool Supported(AgentSnapshot model) => model.OfflineSupported;

    /// <summary>Whether this computer is offline for now: it neither controls nor is controlled by any other.</summary>
    public static bool ThisOffline(AgentSnapshot model) => Supported(model) && model.Status.OfflineForNow;

    /// <summary>
    /// Whether another computer is offline for now: its status says so, with
    /// its glyph after its name. While this one is, the others are just
    /// offline, as the service reports them.
    /// </summary>
    public static bool IsOffline(AgentSnapshot model, string id) =>
        Supported(model) && id != model.ThisDeviceId && model.Device(id) is { OfflineForNow: true };

    /// <summary>
    /// Whether a computer is cut off by being offline for now, itself or this
    /// one: nothing is offered for waking it, and it shows as offline.
    /// </summary>
    public static bool CutOff(AgentSnapshot model, string id) => IsOffline(model, id) || ThisOffline(model);

    /// <summary>
    /// Whether another computer can be taken offline for now, or brought
    /// back, from here: one this computer trusts, while this one is not
    /// offline for now itself (it can then only bring itself back).
    /// </summary>
    public static bool Offered(AgentSnapshot model, string id) =>
        Supported(model) && !ThisOffline(model) && id != model.ThisDeviceId && model.Device(id) is not null;

    /// <summary>What the button and the context menu item say for another computer: Take Offline, or Bring Back Online.</summary>
    public static string ActionKey(AgentSnapshot model, string id) =>
        IsOffline(model, id) ? TextKeys.OfflineBringBack : TextKeys.OfflineTake;

    /// <summary>
    /// The line under an offline computer's status: who took it offline,
    /// by the name this computer gives that one, or that this one did, or
    /// "another computer" when the service doesn't know which. Null for one
    /// not offline for now.
    /// </summary>
    public static UiText? SetBy(AgentSnapshot model, string id)
    {
        if (!IsOffline(model, id) || model.Device(id) is not { } device)
        {
            return null;
        }
        return By(
            model, device.OfflineByDeviceId, device.OfflineByName,
            TextKeys.OfflineSetHere, TextKeys.OfflineSetBy, TextKeys.OfflineSetByAnother);
    }

    /// <summary>
    /// The warnings under an offline computer's status, while it still runs
    /// InputWeave 1.0 itself: one for each other connected computer on 1.0,
    /// which knows nothing of this either and still controls it. A computer
    /// on 1.1 keeps every 1.0 computer away by itself, and the 1.1 ones keep
    /// away from a 1.0 one, so otherwise there is nothing to warn of. None
    /// for one not offline for now.
    /// </summary>
    public static IReadOnlyList<UiText> Unaware(AgentSnapshot model, string id)
    {
        // The service says a computer is on 1.0 only while it is connected,
        // even when it shows offline for being offline for now itself.
        if (!IsOffline(model, id) || model.Device(id) is not { OfflineUnaware: true })
        {
            return [];
        }
        return model.Status.Devices
            .Where(d => d.OfflineUnaware && d.Id != id)
            .Select(d => new UiText(TextKeys.OfflineUnaware, model.NameOf(d.Id)))
            .ToList();
    }

    /// <summary>
    /// Whether another computer's context menu on the canvas has only
    /// Rename…: while this computer is offline for now, nothing else can be
    /// done with the others.
    /// </summary>
    public static bool OnlyRename(AgentSnapshot model, string id) => id != model.ThisDeviceId && ThisOffline(model);

    /// <summary>
    /// The item of the menu the tray icon and the panel's More button share,
    /// taking this computer offline for now or bringing it back; null from a
    /// service that does not know it.
    /// </summary>
    public static string? MenuKey(AgentSnapshot model) =>
        !Supported(model) ? null : ThisOffline(model) ? TextKeys.OfflineBringBack : TextKeys.MenuTakeThisOffline;

    /// <summary>
    /// The panel's line under its title while this computer is offline for
    /// now: who took it offline, and what that means. Null otherwise.
    /// </summary>
    public static UiText? PanelSubtitle(AgentSnapshot model) =>
        ThisOffline(model)
            ? By(
                model, model.Status.OfflineByDeviceId, model.Status.OfflineByName,
                TextKeys.SubtitleThisOfflineHere, TextKeys.SubtitleThisOfflineBy, TextKeys.SubtitleThisOfflineByAnother)
            : null;

    /// <summary>
    /// "Set on this computer" (<paramref name="here"/>) when
    /// <paramref name="by"/> is this one, else "Set by" (<paramref name="other"/>)
    /// the name the service gives the one that did it here. The service
    /// may not know which computer that was, or what it is called: an empty
    /// id is never this computer, and without a name it was "another
    /// computer" (<paramref name="unknown"/>), id or not; an id is never
    /// shown as a name.
    /// </summary>
    private static UiText By(AgentSnapshot model, string by, string name, string here, string other, string unknown)
    {
        if (by.Length != 0 && by == model.ThisDeviceId)
        {
            return new(here);
        }
        return name.Length != 0 ? new(other, name) : new(unknown);
    }
}
