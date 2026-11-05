using Inputweave.App.V1;
using InputWeave.Settings;

namespace InputWeave.Overlay;

/// <summary>
/// What one monitor shows when the screens are identified: which of this
/// computer's screens it is, counted from 1 in the arrangement's order, and
/// its size in pixels, such as "3840 × 2160".
/// </summary>
public sealed record ScreenLabel(MonitorPixels Monitor, int Number, string Size);

/// <summary>
/// The parts of identifying the screens that need
/// no window: which monitor is which of this computer's screens, and for
/// how long the names stay. As the macOS app's ScreenIdentity.swift, except
/// that a monitor is matched by its name (<c>\\.\DISPLAY1</c>), as the
/// drag strips do, rather than by its frame.
/// </summary>
public static class ScreenIdentity
{
    /// <summary>What the service asks for, should it not say.</summary>
    public const uint DefaultDurationMs = 2500;

    /// <summary>How long the names stay for what the service asked.</summary>
    public static TimeSpan ShownFor(uint durationMs) =>
        TimeSpan.FromMilliseconds(durationMs == 0 ? DefaultDurationMs : durationMs);

    /// <summary>
    /// A label for each monitor: the screen of this computer's in the
    /// arrangement (<paramref name="arranged"/>) of the same name, its place
    /// among them and its size as the inspector shows it. A monitor the
    /// arrangement does not name, as before the service has reported the
    /// displays, is numbered by its place among the monitors, the main one
    /// first and the others from left to right, with its own size.
    /// </summary>
    public static IReadOnlyList<ScreenLabel> Labels(IReadOnlyList<Screen> arranged, IReadOnlyList<MonitorPixels> monitors)
    {
        var ordered = monitors
            .OrderByDescending(m => m.Primary)
            .ThenBy(m => m.Left)
            .ThenBy(m => m.Top)
            .ToList();
        var labels = new List<ScreenLabel>();
        foreach (var monitor in monitors)
        {
            int index = -1;
            for (int i = 0; i < arranged.Count; i++)
            {
                if (string.Equals(arranged[i].Id, monitor.Id, StringComparison.OrdinalIgnoreCase))
                {
                    index = i;
                    break;
                }
            }
            labels.Add(index >= 0
                ? new ScreenLabel(monitor, index + 1, DeviceText.Resolution(arranged[index]))
                : new ScreenLabel(monitor, ordered.IndexOf(monitor) + 1, $"{monitor.Right - monitor.Left} × {monitor.Bottom - monitor.Top}"));
        }
        return labels;
    }

    /// <summary>This computer's screens as the arrangement has them, for <see cref="Labels"/>.</summary>
    public static IReadOnlyList<Screen> ThisComputersScreens(Arrangement arrangement) =>
        arrangement.Devices.FirstOrDefault(d => d.ThisDevice)?.Screens.ToList() ?? [];

    /// <summary>
    /// The jump to this computer, spelled out as Settings writes it, for a
    /// line under the screen's size; null when it has no number or the
    /// jumps are off.
    /// </summary>
    public static string? Jump(Agent.AgentSnapshot snapshot) =>
        snapshot.Arrangement.Devices.FirstOrDefault(d => d.ThisDevice) is { } self
            ? snapshot.JumpShortcut(self.JumpSlot, compact: false)
            : null;
}
