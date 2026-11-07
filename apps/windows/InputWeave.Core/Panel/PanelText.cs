using InputWeave.Agent;
using InputWeave.Settings;
using InputWeave.Support;

namespace InputWeave.Panel;

/// <summary>
/// The panel's headline and subtitle, by
/// the same rules as the macOS app's PanelText.
/// </summary>
public static class PanelText
{
    public static UiText Title(AgentSnapshot model, ServiceState service, DateTimeOffset now)
    {
        switch (model.Link)
        {
            case AgentLink.Incompatible:
                return new(TextKeys.TitleUpdating);
            case AgentLink.Connecting:
                // The service is stopped, so nothing will start: the
                // exception row below says so and offers to start it.
                if (service == ServiceState.Stopped)
                {
                    return new(TextKeys.TitleNotWorking);
                }
                return model.IsUnresponsive(now)
                    ? new(TextKeys.TitleNotResponding)
                    : new(TextKeys.TitleStarting);
        }
        var status = model.Status;
        // A paused profile before anything else: this computer is on no
        // network of InputWeave's then.
        if (Profiles.PanelTitle(model) is { } paused)
        {
            return paused;
        }
        // Offline for now, nothing crosses whatever the sharing switch says.
        if (OfflineForNow.ThisOffline(model))
        {
            return new(TextKeys.TitleThisOffline);
        }
        // Pausing keeps the lock, but nothing crosses while paused anyway;
        // and this device's own lock means nothing while another controls it.
        if (!status.Sharing)
        {
            return new(TextKeys.TitlePaused);
        }
        if (model.ControlledFromElsewhere)
        {
            return new(TextKeys.TitleControlledBy, model.NameOf(status.ControlledByDeviceId));
        }
        if (status.RelativeMouse.Length != 0)
        {
            return new(TextKeys.TitleRelative, model.NameOf(status.RelativeMouse));
        }
        if (status.Locked)
        {
            return new(TextKeys.TitleLockedTo, model.NameOf(model.CursorDeviceId));
        }
        if (model.ControllingOther)
        {
            return new(TextKeys.TitleControlling, model.NameOf(status.ActiveDeviceId));
        }
        return new(TextKeys.TitleSharing);
    }

    public static UiText? Subtitle(AgentSnapshot model)
    {
        if (model.Link != AgentLink.Connected)
        {
            return null;
        }
        var status = model.Status;
        // Under a paused profile's title, at most that this network is in
        // no profile.
        if (Profiles.Paused(model))
        {
            return Profiles.PanelSubtitle(model);
        }
        if (OfflineForNow.PanelSubtitle(model) is { } offline)
        {
            return offline;
        }
        if (model.ControlledFromElsewhere)
        {
            return new(TextKeys.SubtitleControlled);
        }
        if (status.Sharing && status.RelativeMouse.Length != 0)
        {
            return model.RelativeShortcut is { } shortcut
                ? new(TextKeys.SubtitleRelative, shortcut)
                : model.TakeBackHint;
        }
        if (status.Sharing && status.Locked)
        {
            return new(TextKeys.SubtitleLocked, model.LockShortcut);
        }
        if (status.Sharing && model.ControllingOther)
        {
            return model.TakeBackHint;
        }
        if (status.Sharing && status.ThisDeviceName.Length != 0)
        {
            return new(TextKeys.SubtitleCursorOn, status.ThisDeviceName);
        }
        return null;
    }
}
