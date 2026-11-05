using InputWeave.Agent;
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
        if (model.ControlledFromElsewhere)
        {
            return new(TextKeys.SubtitleControlled);
        }
        if (status.Sharing && status.RelativeMouse.Length != 0)
        {
            return model.RelativeShortcut is { } shortcut
                ? new(TextKeys.SubtitleRelative, shortcut)
                : new(TextKeys.SubtitleControlling, model.TakeBackShortcut);
        }
        if (status.Sharing && status.Locked)
        {
            return new(TextKeys.SubtitleLocked, model.LockShortcut);
        }
        if (status.Sharing && model.ControllingOther)
        {
            return new(TextKeys.SubtitleControlling, model.TakeBackShortcut);
        }
        if (status.Sharing && status.ThisDeviceName.Length != 0)
        {
            return new(TextKeys.SubtitleCursorOn, status.ThisDeviceName);
        }
        return null;
    }
}
