using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;

namespace InputWeave.Overlay;

/// <summary>
/// Decides what the HUD says, as the
/// macOS app's HUDController: why the keyboard and mouse came back, the
/// cursor being locked or unlocked, relative mode starting and ending,
/// and the take-back hint the first three
/// times this computer takes control of another. It compares each state
/// with the one before, so only a change is news; the first state seen
/// after the app starts is only remembered, and so is relative mode in the
/// first state after the service is reached again. Windows has no secure
/// keyboard entry, so there is no note for it.
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
        if (first)
        {
            return null;
        }
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
                return new UiText(TextKeys.HudTwoLines, new UiText(TextKeys.HudTakeBackHint, snapshot.TakeBackShortcut), relative);
            }
            return relative;
        }
        if (status.Locked != lockedBefore)
        {
            return LockMessage(snapshot);
        }
        // A drag taken across says where it goes instead (DragHud).
        bool dragging = status.Dragging is { DeviceId.Length: > 0 };
        if (controlling && !controllingBefore && hintsShown < HintTimes && !dragging)
        {
            hintsShown++;
            return new UiText(TextKeys.HudTakeBackHint, snapshot.TakeBackShortcut);
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
