using System.Collections.Immutable;
using System.Globalization;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;

namespace InputWeave.Settings;

/// <summary>What the note under a computer's status says about waking it.</summary>
public enum WakeNote
{
    None,
    /// <summary>It was woken and did not connect: a warning, with how to turn on wake for network access.</summary>
    NotWoken,
    /// <summary>Offline, and it never told this computer how to wake it.</summary>
    NotTold,
}

/// <summary>
/// Waking another computer from sleep over the network (WakeDevice), as the
/// inspector, the device list, the canvas with its context menu, and the
/// panel's mini map offer it, the same as on the Mac and Linux. The service
/// decides whether a computer can be woken (DeviceStatus.can_wake) and how
/// far waking it got (DeviceStatus.wake); this only says what the app shows
/// for that. A service from before waking (Status.wake_supported false) gets
/// none of it, and this computer never does; nor does a computer offline for
/// now, nor any while this one is (<see cref="OfflineForNow.CutOff"/>), nor
/// one the profile in use doesn't connect, nor any while it is paused
/// (<see cref="Profiles.CutOff"/>).
/// </summary>
public static class Wake
{
    /// <summary>The Segoe Fluent moon (QuietHours), after the name of a computer that can be woken.</summary>
    public const string MoonGlyph = "";

    /// <summary>The Segoe Fluent warning sign, before the note that a computer didn't wake up.</summary>
    public const string WarningGlyph = "";

    /// <summary>
    /// Another computer, from a service that can wake computers, unless
    /// being offline for now or the profile in use cuts it off: the service
    /// then says it can't be woken, which is no reason to tell how to wake it.
    /// </summary>
    private static DeviceStatus? Other(AgentSnapshot model, string id) =>
        model.WakeSupported && id != model.ThisDeviceId && !OfflineForNow.CutOff(model, id) && !Profiles.CutOff(model, id)
            ? model.Device(id)
            : null;

    /// <summary>
    /// Whether a computer can be woken from here now: Wake shows beside its
    /// status and in its context menu, and a moon after its name. The
    /// service says so only while it is offline.
    /// </summary>
    public static bool Offered(AgentSnapshot model, string id) => Other(model, id) is { CanWake: true };

    /// <summary>
    /// Whether Wake, where offered, can be pressed: not while it is being
    /// woken, nor while this app's request to wake it has no answer yet.
    /// Pressed again after it didn't wake up, it tries again.
    /// </summary>
    public static bool CanSend(AgentSnapshot model, string id) =>
        Other(model, id) is { CanWake: true, Wake: not WakeState.Waking } && !model.WakesSent.Contains(id);

    /// <summary>Whether an offline computer is being woken: a spinner stands before its status.</summary>
    public static bool Waking(AgentSnapshot model, string id) =>
        Other(model, id) is { State: not ConnectionState.Connected, Wake: WakeState.Waking };

    /// <summary>
    /// Whether the moon stands before the status of a computer in the
    /// inspector: it can be woken and is not being woken, when the spinner
    /// stands there instead.
    /// </summary>
    public static bool MoonBeforeStatus(AgentSnapshot model, string id) =>
        Offered(model, id) && !Waking(model, id);

    /// <summary>
    /// The status of an offline computer, as far as waking changes it:
    /// "Waking…" while it is being woken, "Offline · 2 hours ago" when it
    /// can be woken and the service knows since when it is offline. Null
    /// where it says plain "Offline".
    /// </summary>
    public static UiText? OfflineStatus(AgentSnapshot model, string id, DateTimeOffset now, CultureInfo culture)
    {
        if (Other(model, id) is not { State: not ConnectionState.Connected } device)
        {
            return null;
        }
        if (device.Wake == WakeState.Waking)
        {
            return new(TextKeys.ConnectionWaking);
        }
        if (!device.CanWake || device.OfflineSinceUnixMs == 0)
        {
            return null;
        }
        var latest = (ulong)DateTimeOffset.MaxValue.ToUnixTimeMilliseconds();
        var since = DateTimeOffset.FromUnixTimeMilliseconds((long)Math.Min(device.OfflineSinceUnixMs, latest));
        return new(TextKeys.ConnectionOfflineSince, ClipboardText.Ago(now - since, culture));
    }

    /// <summary>Which note goes under a computer's status, if any.</summary>
    public static WakeNote Note(AgentSnapshot model, string id) => Other(model, id) switch
    {
        { CanWake: true, Wake: WakeState.NotWoken } => WakeNote.NotWoken,
        { CanWake: false, State: not ConnectionState.Connected } => WakeNote.NotTold,
        _ => WakeNote.None,
    };

    /// <summary>The words of the note under a computer's status; null without one.</summary>
    public static UiText? NoteText(AgentSnapshot model, string id) => Note(model, id) switch
    {
        WakeNote.NotWoken => new(TextKeys.WakeNotWoken, model.NameOf(id)),
        WakeNote.NotTold => new(TextKeys.WakeNotTold, model.NameOf(id)),
        _ => null,
    };

    /// <summary>Whether General › Switching has the switch that wakes a computer as the cursor is pushed toward it.</summary>
    public static bool SwitchShown(AgentSnapshot model) => model.WakeSupported;

    /// <summary>
    /// The computers in <paramref name="sent"/> still in
    /// <paramref name="status"/>; the same set when none left it.
    /// </summary>
    public static ImmutableHashSet<string> Kept(ImmutableHashSet<string> sent, Status status)
    {
        if (sent.IsEmpty)
        {
            return sent;
        }
        var gone = sent.Where(id => status.Devices.All(d => d.Id != id)).ToList();
        return gone.Count == 0 ? sent : sent.Except(gone);
    }
}

/// <summary>
/// General › Switching's switch that wakes a sleeping computer as the cursor
/// is pushed toward it (SetWakeOnPush), shown only when the service can
/// wake computers (<see cref="Wake.SwitchShown"/>). It shows the service's
/// value. Refused, or with no answer, it goes back to that and the line
/// under it says the setting could not be changed, as the relative mouse
/// switch does, until the service reports another value or goes out of
/// reach.
/// </summary>
public sealed class WakeOnPushSwitch
{
    private bool reported;

    /// <summary>The service did not take the last change.</summary>
    public bool Refused { get; private set; }

    /// <summary>Takes in what the service reports now; returns whether the switch is on.</summary>
    public bool Update(AgentSnapshot model)
    {
        var on = model.Status.WakeOnPush;
        if (on != reported || model.Link != AgentLink.Connected)
        {
            Refused = false;
        }
        reported = on;
        return on;
    }

    /// <summary>The service answered a change of the switch, taking it or not.</summary>
    public void Answered(bool taken) => Refused = !taken;

    /// <summary>The line under the switch: what it does, or that it could not be changed.</summary>
    public UiText Hint => new(Refused ? TextKeys.GeneralWakeOnPushFailed : TextKeys.GeneralWakeOnPushHint);
}
