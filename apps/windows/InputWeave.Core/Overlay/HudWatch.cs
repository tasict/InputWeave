using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;

namespace InputWeave.Overlay;

/// <summary>
/// Decides what the HUD says, as the
/// macOS app's HUDController: why the keyboard and mouse came back, the
/// cursor being locked or unlocked, relative mode starting and ending,
/// the take-back hint the first three
/// times this computer takes control of another, another computer
/// being woken and waking up, and computers taken offline for now or
/// brought back. It compares each state
/// with the one before, so only a change is news; the first state seen
/// after the app starts is only remembered, and so are relative mode,
/// waking and computers taken offline in the first state after the service
/// is reached again. Windows has no secure keyboard entry, so there is no
/// note for it.
/// </summary>
public sealed class HudWatch
{
    /// <summary>How many times the take-back hint is shown, ever.</summary>
    public const int HintTimes = 3;

    /// <summary>
    /// Relative mode beginning or ending by itself, as an app takes or lets
    /// go of the pointer, is told for each computer the first time of each
    /// kind, then at most once in this time.
    /// </summary>
    public static readonly TimeSpan QuietFor = TimeSpan.FromSeconds(60);

    private bool primed;
    private ulong seenReturns;
    private bool wasLocked;
    private bool wasControlling;
    private bool relativePrimed;
    private string relativeDevice = "";
    private ulong relativeEnds;
    private ulong seenRefusal;
    private readonly Dictionary<string, Told> told = [];
    private bool wakePrimed;
    /// <summary>How far waking each other computer was in the last state; a connected one counts as not being woken.</summary>
    private readonly Dictionary<string, WakeState> wakes = [];
    private bool offlinePrimed;
    private ulong offlineChanges;

    /// <summary>What was told about relative mode beginning or ending by itself on a computer.</summary>
    private sealed class Told
    {
        public bool Entered;
        public bool Released;
        public DateTimeOffset At;
    }

    /// <summary>
    /// Takes in the latest state and returns the note to show, if any.
    /// </summary>
    /// <param name="hintsShown">How many take-back hints were shown so far; one more when this returns one.</param>
    /// <param name="now">The time now; the clock's when null.</param>
    public UiText? Update(AgentSnapshot snapshot, ref int hintsShown, DateTimeOffset? now = null)
    {
        if (snapshot.Link != AgentLink.Connected)
        {
            // A restarted service starts over; what it says first is no news.
            relativePrimed = false;
            wakePrimed = false;
            offlinePrimed = false;
            return null;
        }
        var status = snapshot.Status;
        bool controlling = snapshot.ControllingOther;
        bool first = !primed;
        ulong returnsBefore = seenReturns;
        bool lockedBefore = wasLocked;
        bool controllingBefore = wasControlling;
        bool relativeFirst = !relativePrimed;
        string relativeBefore = relativeDevice;
        ulong endsBefore = relativeEnds;
        ulong refusalBefore = seenRefusal;
        primed = true;
        seenReturns = status.ControlReturns;
        wasLocked = status.Locked;
        wasControlling = controlling;
        relativePrimed = true;
        relativeDevice = status.RelativeMouse;
        relativeEnds = status.RelativeEnds;
        seenRefusal = snapshot.RelativeRefusal?.Serial ?? 0;
        // Taken in now, so a note told below does not leave waking or a
        // computer taken offline behind.
        var woken = WakeMessage(snapshot);
        var offline = OfflineMessage(snapshot);
        if (first)
        {
            return null;
        }
        // The keyboard and mouse coming back from a computer taken offline
        // for now says so, rather than that it was taken offline.
        if (status.ControlReturns > returnsBefore && ReturnMessage(snapshot) is { } back)
        {
            return back;
        }
        if (seenRefusal > refusalBefore && snapshot.RelativeRefusal is { } refusal)
        {
            return new UiText(TextKeys.HudRelativeUnsupported, snapshot.NameOf(refusal.DeviceId));
        }
        // A restarted service counts from 0: fewer ends than before are only remembered.
        if (!relativeFirst && status.RelativeEnds >= endsBefore
            && RelativeMessage(snapshot, relativeBefore, endsBefore, now ?? DateTimeOffset.UtcNow) is { } relative)
        {
            // Relative mode as the cursor first crosses: how to take back
            // control goes with it.
            if (controlling && !controllingBefore && hintsShown < HintTimes)
            {
                hintsShown++;
                return new UiText(TextKeys.HudTwoLines, snapshot.TakeBackReminder, relative);
            }
            return relative;
        }
        if (status.Locked != lockedBefore)
        {
            return LockMessage(snapshot);
        }
        // Then a computer taken offline for now or brought back, then
        // waking, both before the take-back hint, as the other apps have it.
        if (offline is not null)
        {
            return offline;
        }
        if (woken is not null)
        {
            return woken;
        }
        // A drag taken across says where it goes instead (DragHud).
        bool dragging = status.Dragging is { DeviceId.Length: > 0 };
        if (controlling && !controllingBefore && hintsShown < HintTimes && !dragging)
        {
            hintsShown++;
            return snapshot.TakeBackReminder;
        }
        return null;
    }

    /// <summary>
    /// Another computer starting to be woken ("Waking X…", also as Wake is
    /// pressed, since the service answers with that state), or connecting
    /// while it was: it woke up ("X is awake."). Only from a service that can wake
    /// computers; the first state after it is reached again is only
    /// remembered. Computers that left the status are forgotten.
    /// </summary>
    private UiText? WakeMessage(AgentSnapshot snapshot)
    {
        var status = snapshot.Status;
        bool primed = wakePrimed;
        wakePrimed = true;
        if (!snapshot.WakeSupported)
        {
            wakes.Clear();
            return null;
        }
        UiText? message = null;
        foreach (var device in status.Devices)
        {
            if (device.Id == status.ThisDeviceId)
            {
                continue;
            }
            bool connected = device.State == ConnectionState.Connected;
            var now = connected ? WakeState.None : device.Wake;
            var before = wakes.TryGetValue(device.Id, out var was) ? was : WakeState.None;
            wakes[device.Id] = now;
            if (!primed || message is not null)
            {
                continue;
            }
            if (before == WakeState.Waking && connected)
            {
                message = new UiText(TextKeys.HudAwake, snapshot.NameOf(device.Id));
            }
            else if (now == WakeState.Waking && before != WakeState.Waking)
            {
                message = new UiText(TextKeys.HudWaking, snapshot.NameOf(device.Id));
            }
        }
        foreach (var gone in wakes.Keys.Where(id => status.Devices.All(d => d.Id != id)).ToList())
        {
            wakes.Remove(gone);
        }
        return message;
    }

    /// <summary>
    /// A computer taken offline for now or brought back since the last
    /// state, as the service counts them (Status.offline_changes) and says
    /// which (Status.last_offline_change): taken offline or brought back
    /// from here, this one included ("X is offline for now.", "X is back
    /// online."; nothing as this one brings itself back), or this one taken
    /// offline by another ("X took this computer offline for now.", by the
    /// name this computer gave that one as the change came, whatever the
    /// status says by now, or "Another computer…" without one). Only from a
    /// service that knows
    /// it; the first state after it is reached again is only remembered, and
    /// so is a count lower than before, as from a service started again.
    /// </summary>
    private UiText? OfflineMessage(AgentSnapshot snapshot)
    {
        var status = snapshot.Status;
        bool primed = offlinePrimed;
        ulong before = offlineChanges;
        offlinePrimed = true;
        offlineChanges = status.OfflineChanges;
        if (!snapshot.OfflineSupported || !primed || status.OfflineChanges <= before
            || status.LastOfflineChange is not { } change)
        {
            return null;
        }
        var self = status.ThisDeviceId;
        // An unknown computer did it when the id is empty, never this one.
        if (change.ByDeviceId.Length != 0 && change.ByDeviceId == self)
        {
            if (change.Offline)
            {
                return new UiText(TextKeys.HudOfflineForNow, snapshot.NameOf(change.DeviceId));
            }
            return change.DeviceId == self ? null : new UiText(TextKeys.HudBackOnline, snapshot.NameOf(change.DeviceId));
        }
        if (change.DeviceId == self && change.Offline)
        {
            return change.ByName.Length != 0
                ? new UiText(TextKeys.HudTookThisOffline, change.ByName)
                : new UiText(TextKeys.HudTookThisOfflineAnother);
        }
        return null;
    }

    /// <summary>
    /// Relative mode starting or ending since the last state, as the
    /// background service says why it ended (Status.last_relative_end).
    /// Having ended and begun again in between, it began. When it ended as
    /// the keyboard and mouse came back or sharing was paused, those say so
    /// themselves; what began or ended by itself is told now and then
    /// (<see cref="QuietFor"/>).
    /// </summary>
    private UiText? RelativeMessage(AgentSnapshot snapshot, string before, ulong endsBefore, DateTimeOffset now)
    {
        var status = snapshot.Status;
        bool ended = status.RelativeEnds > endsBefore;
        var device = status.RelativeMouse;
        if (device.Length != 0 && (device != before || ended))
        {
            if (status.RelativeMouseAutomatic && !Allows(device, entered: true, now))
            {
                return null;
            }
            var name = snapshot.NameOf(device);
            return snapshot.RelativeShortcut is { } shortcut
                ? new UiText(TextKeys.HudRelativeEntered, name, shortcut)
                : new UiText(TextKeys.HudRelativeEnteredTakeBack, name, snapshot.TakeBackShortcut);
        }
        if (!ended || status.LastRelativeEnd is not { } end)
        {
            return null;
        }
        switch (end.Reason)
        {
            case RelativeEndReason.TurnedOff or RelativeEndReason.AutoOff or RelativeEndReason.Jumped:
                return new UiText(TextKeys.HudRelativeEnded);
            case RelativeEndReason.Released when Allows(end.DeviceId, entered: false, now):
                return new UiText(TextKeys.HudRelativeReleased, snapshot.NameOf(end.DeviceId));
            default:
                return null;
        }
    }

    /// <summary>
    /// Whether to tell relative mode beginning (<paramref name="entered"/>)
    /// or ending by itself on <paramref name="device"/> at
    /// <paramref name="now"/>: the first time of each kind, then at most
    /// once in <see cref="QuietFor"/>. If so, it counts as told.
    /// </summary>
    private bool Allows(string device, bool entered, DateTimeOffset now)
    {
        if (!told.TryGetValue(device, out var seen))
        {
            seen = new Told { At = DateTimeOffset.MinValue };
            told[device] = seen;
        }
        bool first = entered ? !seen.Entered : !seen.Released;
        if (!first && now - seen.At < QuietFor)
        {
            return false;
        }
        if (entered)
        {
            seen.Entered = true;
        }
        else
        {
            seen.Released = true;
        }
        seen.At = now;
        return true;
    }

    private static UiText LockMessage(AgentSnapshot snapshot)
    {
        if (!snapshot.Status.Locked)
        {
            return new UiText(TextKeys.HudUnlocked);
        }
        // Locked while the cursor is on another computer, it stays there.
        return snapshot.ControllingOther
            ? new UiText(TextKeys.HudLockedTo, snapshot.NameOf(snapshot.Status.ActiveDeviceId), snapshot.LockShortcut)
            : new UiText(TextKeys.HudLockedHere, snapshot.LockShortcut);
    }

    private static UiText? ReturnMessage(AgentSnapshot snapshot)
    {
        var name = snapshot.NameOf(snapshot.Status.LastReturnDeviceId);
        return snapshot.Status.LastReturn switch
        {
            // This computer taken offline while it controlled another: what
            // happened to it says enough (OfflineMessage).
            ReturnReason.OfflineForNow when snapshot.Status.LastReturnDeviceId == snapshot.ThisDeviceId => null,
            ReturnReason.OfflineForNow => new UiText(TextKeys.HudOfflineReturned, name),
            ReturnReason.PeerLost => new UiText(TextKeys.HudPeerLost, name),
            ReturnReason.RemoteActivity => new UiText(TextKeys.HudRemoteActivity, name),
            ReturnReason.Hotkey => new UiText(TextKeys.HudBackHere),
            ReturnReason.LayoutChanged => new UiText(TextKeys.HudLayoutChanged),
            ReturnReason.ScreenLocked => new UiText(TextKeys.HudScreenLocked, name),
            ReturnReason.InputUnavailable => new UiText(TextKeys.HudInputUnavailable, name),
            ReturnReason.PeerPaused => new UiText(TextKeys.HudPeerPaused, name),
            ReturnReason.CaptureEnded => new UiText(TextKeys.HudCaptureEnded),
            _ => null,
        };
    }
}
