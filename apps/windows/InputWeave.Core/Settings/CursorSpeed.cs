using System.Globalization;
using Inputweave.App.V1;
using InputWeave.Agent;

namespace InputWeave.Settings;

/// <summary>
/// What the inspector's cursor speed row offers for another computer
/// (SetDeviceSpeed): how fast the cursor moves there while this computer
/// controls it, in percent of how the mouse moves it here; as on the Mac.
/// </summary>
public static class CursorSpeed
{
    public const int Normal = 100;
    public const int Minimum = 50;
    public const int Maximum = 300;
    public const int Step = 10;

    /// <summary>The speeds marked under the slider.</summary>
    public static readonly IReadOnlyList<int> Ticks = [50, 100, 200, 300];

    /// <summary>
    /// The speed to show for <paramref name="id"/>, or null where the row is
    /// not offered: this computer, and a service from before cursor speeds,
    /// which says 0.
    /// </summary>
    public static int? Shown(AgentSnapshot model, string id) => Shown(model.Device(id), id == model.ThisDeviceId);

    public static int? Shown(DeviceStatus? device, bool thisDevice) =>
        !thisDevice && device is { SpeedPercent: > 0 } ? (int)device.SpeedPercent : null;

    /// <summary>The speed a position of the slider stands for: the nearest step, in range.</summary>
    public static int Snapped(double value)
    {
        var stepped = Math.Round(value / Step, MidpointRounding.AwayFromZero) * Step;
        return (int)Math.Clamp(stepped, Minimum, Maximum);
    }

    /// <summary>The speed as shown beside the slider, e.g. "150%".</summary>
    public static string Label(int percent) => percent.ToString(CultureInfo.InvariantCulture) + "%";

    /// <summary>Reset is offered unless the speed is the normal one.</summary>
    public static bool ShowsReset(int percent) => percent != Normal;

    /// <summary>Where <paramref name="percent"/> sits along the slider, from 0 to 1.</summary>
    public static double Fraction(int percent) => (double)(percent - Minimum) / (Maximum - Minimum);
}

/// <summary>
/// The speed being chosen on the slider, shown in place of the service's
/// until the service answers for it. The service's value is what counts:
/// refused, the slider goes back to it. Each speed sent is numbered; until
/// the answer to the latest one comes, the slider shows the latest speed
/// sent, and a new choice is weighed against it, not against the service's
/// older speed, so choosing the service's speed again still goes. Answers
/// to earlier sends change nothing.
/// </summary>
public sealed class CursorSpeedDraft
{
    /// <summary>Chosen, and not sent yet (or still held).</summary>
    public int? Value { get; private set; }

    /// <summary>The slider is held under the pointer.</summary>
    public bool Editing { get; private set; }

    /// <summary>The number of the latest send.</summary>
    public ulong Sent { get; private set; }

    /// <summary>The latest speed sent, until its answer comes.</summary>
    public int? InFlight { get; private set; }

    /// <summary>What a new choice is weighed against: the latest speed on its way, or else the service's.</summary>
    private int Baseline(int service) => InFlight ?? service;

    /// <summary>What the slider shows, the service having <paramref name="service"/>.</summary>
    public int Shown(int service) => Value ?? Baseline(service);

    public void Began() => Editing = true;

    /// <summary>The slider moved to <paramref name="position"/>.</summary>
    public void Moved(double position, int service)
    {
        Value = CursorSpeed.Snapped(position);
        Settle(service);
    }

    /// <summary>The pointer let go of the slider: the speed to send, if it differs from what the service has or is getting.</summary>
    public int? Ended(int service)
    {
        Editing = false;
        Settle(service);
        return Value;
    }

    /// <summary>A speed chosen without the pointer (the keyboard, Reset), to send unless the slider is held.</summary>
    public int? Pending(int service) => !Editing && Value is { } value && value != Baseline(service) ? value : null;

    /// <summary><paramref name="percent"/> is sent: the number its answer comes back with.</summary>
    public ulong Sending(int percent)
    {
        Sent++;
        InFlight = percent;
        if (!Editing && Value == percent)
        {
            Value = null;
        }
        return Sent;
    }

    /// <summary>
    /// The service answered send <paramref name="number"/>, either way. Only
    /// the latest one counts: the slider shows the service's speed again,
    /// but for a speed chosen and not sent yet.
    /// </summary>
    public void Answered(ulong number)
    {
        if (number == Sent)
        {
            InFlight = null;
        }
    }

    /// <summary>The service has <paramref name="service"/>: a speed equal to what it has or is getting, no longer held, is no longer a draft.</summary>
    public void Settle(int service)
    {
        if (!Editing && Value == Baseline(service))
        {
            Value = null;
        }
    }
}

/// <summary>
/// Each computer's <see cref="CursorSpeedDraft"/>, kept while another is
/// shown, so coming back to one shows the speed still on its way there.
/// </summary>
public sealed class CursorSpeedDrafts
{
    private readonly Dictionary<string, CursorSpeedDraft> drafts = [];

    /// <summary>The draft of <paramref name="deviceId"/>, made the first time.</summary>
    public CursorSpeedDraft For(string deviceId)
    {
        if (!drafts.TryGetValue(deviceId, out var draft))
        {
            draft = new CursorSpeedDraft();
            drafts[deviceId] = draft;
        }
        return draft;
    }

    /// <summary>Whether <paramref name="deviceId"/> has a draft.</summary>
    public bool Has(string deviceId) => drafts.ContainsKey(deviceId);

    /// <summary>
    /// Only the computers in <paramref name="deviceIds"/> are still trusted:
    /// the others lose their drafts, so paired again they show the
    /// service's speed.
    /// </summary>
    public void Keep(IEnumerable<string> deviceIds)
    {
        var kept = deviceIds.ToHashSet();
        foreach (var id in drafts.Keys.Where(id => !kept.Contains(id)).ToList())
        {
            drafts.Remove(id);
        }
    }
}
