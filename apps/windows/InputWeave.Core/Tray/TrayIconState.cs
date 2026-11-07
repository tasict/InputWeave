using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;

namespace InputWeave.Tray;

/// <summary>
/// The five states of the notification area icon.
/// Each changes one bold element of the two screens and their stitches.
/// </summary>
public enum TrayIconState
{
    Normal,
    /// <summary>Controlling another computer: the other screen is lit.</summary>
    Controlling,
    /// <summary>Controlled from elsewhere: the whole icon knocked out of a tile.</summary>
    Controlled,
    /// <summary>Sharing paused, or this computer offline for now: the stitches are gone.</summary>
    Paused,
    /// <summary>Something needs the user: a dot in the corner.</summary>
    Attention,
}

public static class TrayIcons
{
    /// <summary>
    /// The icon for what the app knows. The dot shows only for what the
    /// panel asks the user to fix: the service stopped, a missing
    /// permission, or a service that has not answered for longer than a
    /// restart takes. Not while it starts or updates.
    /// </summary>
    public static TrayIconState StateFor(AgentSnapshot model, ServiceState service, DateTimeOffset now)
    {
        if ((service == ServiceState.Stopped && model.Link != AgentLink.Connected) || model.IsUnresponsive(now))
        {
            return TrayIconState.Attention;
        }
        // Until the service answers, its last status may be old or empty.
        if (model.Link != AgentLink.Connected)
        {
            return TrayIconState.Normal;
        }
        if (model.MissingPermissions.Count != 0)
        {
            return TrayIconState.Attention;
        }
        // Offline for now, this computer neither controls another nor is
        // controlled, as when paused; the panel's title says which it is.
        if (!model.Status.Sharing || OfflineForNow.ThisOffline(model))
        {
            return TrayIconState.Paused;
        }
        if (model.ControlledFromElsewhere)
        {
            return TrayIconState.Controlled;
        }
        return model.ControllingOther ? TrayIconState.Controlling : TrayIconState.Normal;
    }

    /// <summary>
    /// The tooltip, which is also what screen readers say for the icon: with
    /// <paramref name="model"/>, the paused icon of a computer offline for
    /// now says so rather than that sharing is paused.
    /// </summary>
    public static UiText Description(TrayIconState state, AgentSnapshot? model = null) => state switch
    {
        TrayIconState.Paused when model is not null && OfflineForNow.ThisOffline(model) => new(TextKeys.TrayOfflineForNow),
        TrayIconState.Controlling => new(TextKeys.TrayControlling),
        TrayIconState.Controlled => new(TextKeys.TrayControlled),
        TrayIconState.Paused => new(TextKeys.TrayPaused),
        TrayIconState.Attention => new(TextKeys.TrayAttention),
        _ => new(TextKeys.TrayNormal),
    };

    /// <summary>
    /// The icon file for a state. <paramref name="lightTaskbar"/> picks the
    /// dark glyph drawn for a light taskbar; otherwise the light glyph for
    /// a dark one.
    /// </summary>
    public static string FileName(TrayIconState state, bool lightTaskbar) =>
        $"{state.ToString().ToLowerInvariant()}-{(lightTaskbar ? "light" : "dark")}.ico";
}
