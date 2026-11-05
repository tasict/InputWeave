using Inputweave.App.V1;
using InputWeave.Canvas;
using InputWeave.Overlay;

namespace InputWeave.Drag;

public enum StripEdge
{
    Left,
    Right,
    Top,
    Bottom,
}

/// <summary>
/// A strip along an edge of one of this computer's screens, where a drag
/// of files can leave for another computer: in this computer's desktop
/// coordinates (logical units, y down), on the screen named
/// <paramref name="ScreenId"/>.
/// </summary>
public sealed record LogicalStrip(string ScreenId, StripEdge Edge, CanvasRect Rect);

/// <summary>
/// Where the invisible strips that catch a drag of files go
/// (docs/architecture.md §6.4, point 7): along
/// every part of an edge of this computer's screens through which the
/// cursor would go to another computer. The rule is the background
/// service's (iw-core <c>Layout::cross</c>): leaving a screen through an
/// edge, the cursor goes to the nearest screen lying wholly beyond that
/// edge, gaps and all, unless that screen is one of this computer's own.
/// </summary>
public static class DragStrips
{
    /// <summary>
    /// How deep a strip reaches into the screen, in logical units: wide
    /// enough that the drag enters it well before the cursor meets the
    /// edge, since the background service lists a large folder for a second
    /// or two before the cursor may cross.
    /// </summary>
    public const double Depth = 40;

    /// <summary>The spacing along an edge at which the rule is checked.</summary>
    private const double Step = 1;

    /// <summary>Edges this close count as touching, as in the service.</summary>
    private const double Epsilon = 0.001;

    /// <summary>
    /// The strips for this computer: along its screens' edges where the
    /// cursor would go to one of the <paramref name="enterable"/> computers
    /// (connected and placed). None while this computer is not placed.
    /// </summary>
    public static IReadOnlyList<LogicalStrip> For(Arrangement arrangement, IReadOnlySet<string> enterable)
    {
        var self = arrangement.Devices.FirstOrDefault(d => d.ThisDevice);
        if (self is null || !self.Placed || self.Screens.Count == 0)
        {
            return [];
        }
        var own = self.Screens
            .Select(s => (s.Id, Frame: new CanvasRect(s.X + self.X, s.Y + self.Y, s.Width, s.Height)))
            .Where(s => s.Frame.Width > 0 && s.Frame.Height > 0)
            .ToList();
        var others = arrangement.Devices
            .Where(d => !d.ThisDevice && d.Placed && enterable.Contains(d.Id))
            .SelectMany(d => d.Screens.Select(s => new CanvasRect(s.X + d.X, s.Y + d.Y, s.Width, s.Height)))
            .Where(r => r.Width > 0 && r.Height > 0)
            .ToList();
        var strips = new List<LogicalStrip>();
        if (others.Count == 0)
        {
            return strips;
        }
        for (int i = 0; i < own.Count; i++)
        {
            var source = own[i].Frame;
            foreach (var edge in Enum.GetValues<StripEdge>())
            {
                var foreign = others.Where(r => IsBeyond(edge, source, r)).ToList();
                if (foreign.Count == 0)
                {
                    continue;
                }
                var mine = own.Where((o, j) => j != i && IsBeyond(edge, source, o.Frame)).Select(o => o.Frame).ToList();
                foreach (var (from, to) in Crossings(edge, source, foreign, mine))
                {
                    var rect = Band(edge, source, from, to).Offset(-self.X, -self.Y);
                    strips.Add(new LogicalStrip(own[i].Id, edge, rect));
                }
            }
        }
        return strips;
    }

    /// <summary>The stretches of an edge of <paramref name="source"/> where a foreign screen is nearer than any of this computer's own.</summary>
    private static List<(double From, double To)> Crossings(
        StripEdge edge, CanvasRect source, List<CanvasRect> foreign, List<CanvasRect> mine)
    {
        var (low, high) = Extent(edge, source);
        var stretches = new List<(double, double)>();
        double? start = null;
        for (double at = low; at < high; at += Step)
        {
            double end = Math.Min(at + Step, high);
            double along = (at + end) / 2;
            double nearestForeign = foreign.Min(r => DistanceSquared(edge, source, r, along));
            double nearestOwn = mine.Count == 0 ? double.PositiveInfinity : mine.Min(r => DistanceSquared(edge, source, r, along));
            bool crosses = nearestForeign < nearestOwn;
            if (crosses && start is null)
            {
                start = at;
            }
            else if (!crosses && start is { } began)
            {
                stretches.Add((began, at));
                start = null;
            }
        }
        if (start is { } open)
        {
            stretches.Add((open, high));
        }
        return stretches;
    }

    private static (double Low, double High) Extent(StripEdge edge, CanvasRect source) => edge switch
    {
        StripEdge.Left or StripEdge.Right => (source.MinY, source.MaxY),
        _ => (source.MinX, source.MaxX),
    };

    /// <summary>Whether <paramref name="r"/> lies wholly beyond this edge of <paramref name="source"/>.</summary>
    private static bool IsBeyond(StripEdge edge, CanvasRect source, CanvasRect r) => edge switch
    {
        StripEdge.Right => r.MinX >= source.MaxX - Epsilon,
        StripEdge.Left => r.MaxX <= source.MinX + Epsilon,
        StripEdge.Bottom => r.MinY >= source.MaxY - Epsilon,
        _ => r.MaxY <= source.MinY + Epsilon,
    };

    /// <summary>
    /// How far it is from the point <paramref name="along"/> the edge to the
    /// nearest point of the edge of <paramref name="r"/> that faces it:
    /// across the gap, and along into its extent.
    /// </summary>
    private static double DistanceSquared(StripEdge edge, CanvasRect source, CanvasRect r, double along)
    {
        double across, aside;
        switch (edge)
        {
            case StripEdge.Right:
                (across, aside) = (r.MinX - source.MaxX, Outside(along, r.MinY, r.MaxY));
                break;
            case StripEdge.Left:
                (across, aside) = (source.MinX - r.MaxX, Outside(along, r.MinY, r.MaxY));
                break;
            case StripEdge.Bottom:
                (across, aside) = (r.MinY - source.MaxY, Outside(along, r.MinX, r.MaxX));
                break;
            default:
                (across, aside) = (source.MinY - r.MaxY, Outside(along, r.MinX, r.MaxX));
                break;
        }
        return across * across + aside * aside;
    }

    private static double Outside(double v, double low, double high) =>
        v < low ? low - v : v > high ? v - high : 0;

    /// <summary>The strip along an edge of <paramref name="source"/> between two points along it, reaching into the screen.</summary>
    private static CanvasRect Band(StripEdge edge, CanvasRect source, double from, double to)
    {
        double depth = Math.Min(Depth, edge is StripEdge.Left or StripEdge.Right ? source.Width / 2 : source.Height / 2);
        return edge switch
        {
            StripEdge.Left => new CanvasRect(source.MinX, from, depth, to - from),
            StripEdge.Right => new CanvasRect(source.MaxX - depth, from, depth, to - from),
            StripEdge.Top => new CanvasRect(from, source.MinY, to - from, depth),
            _ => new CanvasRect(from, source.MaxY - depth, to - from, depth),
        };
    }

    /// <summary>This computer's screens as the arrangement has them, for <see cref="ToPixels"/>.</summary>
    public static IReadOnlyList<LogicalScreen> ThisComputersScreens(Arrangement arrangement) =>
        arrangement.Devices.FirstOrDefault(d => d.ThisDevice)?.Screens
            .Select(s => new LogicalScreen(s.Id, s.X, s.Y, s.Width, s.Height, s.Scale))
            .ToList()
        ?? [];

    /// <summary>
    /// The strips in physical pixels, each on the monitor of its screen's
    /// name, as the background service maps its coordinates: every screen
    /// has a logical rectangle (its pixels ÷ its scale), mapped on its own,
    /// so a strip of a 150 % screen is one and a half times as large in
    /// pixels. Rounded outwards and kept on the monitor. A strip on a
    /// screen no monitor has the name of is placed on the main monitor from
    /// the desktop's origin, which is right for one monitor.
    /// </summary>
    public static IReadOnlyList<PixelRect> ToPixels(
        IReadOnlyList<LogicalStrip> strips, IReadOnlyList<LogicalScreen> screens, IReadOnlyList<MonitorPixels> monitors)
    {
        var pixels = new List<PixelRect>();
        foreach (var strip in strips)
        {
            var screen = screens.FirstOrDefault(s => s.Id == strip.ScreenId);
            var monitor = screen is null
                ? null
                : monitors.FirstOrDefault(m => string.Equals(m.Id, screen.Id, StringComparison.OrdinalIgnoreCase));
            double originX, originY, scale;
            if (screen is not null && monitor is not null)
            {
                scale = screen.Scale > 0 ? screen.Scale : monitor.Scale;
                originX = monitor.Left - screen.X * scale;
                originY = monitor.Top - screen.Y * scale;
            }
            else
            {
                monitor = monitors.FirstOrDefault(m => m.Primary) ?? monitors.FirstOrDefault();
                if (monitor is null)
                {
                    continue;
                }
                scale = monitor.Scale > 0 ? monitor.Scale : 1;
                (originX, originY) = (0, 0);
            }
            int left = Math.Max(monitor.Left, (int)Math.Floor(originX + strip.Rect.MinX * scale + 1e-6));
            int top = Math.Max(monitor.Top, (int)Math.Floor(originY + strip.Rect.MinY * scale + 1e-6));
            int right = Math.Min(monitor.Right, (int)Math.Ceiling(originX + strip.Rect.MaxX * scale - 1e-6));
            int bottom = Math.Min(monitor.Bottom, (int)Math.Ceiling(originY + strip.Rect.MaxY * scale - 1e-6));
            if (right > left && bottom > top)
            {
                pixels.Add(new PixelRect(left, top, right - left, bottom - top));
            }
        }
        return pixels;
    }

    /// <summary>Whether a pixel is in one of the strips.</summary>
    public static bool Contains(IEnumerable<PixelRect> strips, PixelPoint pixel) =>
        strips.Any(r => pixel.X >= r.X && pixel.X < r.X + r.Width && pixel.Y >= r.Y && pixel.Y < r.Y + r.Height);

    /// <summary>
    /// Whether the strips are up: while files can leave from here, that is
    /// while sharing and not locked. While another computer controls this
    /// one, only if it can carry a drag from here (<c>drags_carried</c>).
    /// A drag another computer offers here
    /// starts at the edge, inside a strip, which lets that press through
    /// (<see cref="StripPress"/>).
    /// </summary>
    public static bool Shown(Agent.AgentSnapshot snapshot) =>
        snapshot.Link == Agent.AgentLink.Connected
        && snapshot.Status.Sharing && !snapshot.Status.Locked
        && (!snapshot.ControlledFromElsewhere || snapshot.Status.DragsCarried);

    /// <summary>
    /// The computers a drag can go to now: connected ones that files are
    /// not turned off with (<c>SetDeviceSharing</c>); the service does not
    /// offer a drag to the others.
    /// </summary>
    public static IReadOnlySet<string> Enterable(Agent.AgentSnapshot snapshot) =>
        snapshot.FileReceivers.Select(d => d.Id).ToHashSet();
}

/// <summary>
/// When the strips are solid, so a drag's OLE loop finds them under the
/// cursor: while a mouse button is down that went
/// down outside them, as for a drag coming towards the edge. A press inside
/// a strip is meant for what lies below it, such as the taskbar, which must
/// get its button up too; the strips stay clear of the pointer for that
/// press. Where the button went down is known within one check of the
/// buttons (every 30 ms), which is near enough: a drag starts well away
/// from the edge.
/// </summary>
public sealed class StripPress
{
    private bool down;
    private bool inside;

    /// <summary>Takes in whether a button is down and where the cursor is now; returns whether the strips are solid.</summary>
    public bool Solid(bool buttonDown, PixelPoint cursor, IReadOnlyList<PixelRect> strips)
    {
        if (!buttonDown)
        {
            down = false;
            return false;
        }
        if (!down)
        {
            // A new press, seen for the first time here.
            down = true;
            inside = DragStrips.Contains(strips, cursor);
        }
        return !inside;
    }
}

/// <summary>
/// What the strips depend on: the arrangement, the computers a drag can go
/// to and whether files can leave from here. Placing them steps along every
/// edge and asks Windows for the monitors, so the app does it again only
/// when one of these changes, not for every transfer's progress or
/// round-trip time.
/// </summary>
public sealed class StripInputs(Arrangement arrangement, IReadOnlySet<string> enterable, bool shown, bool connected)
    : IEquatable<StripInputs>
{
    public Arrangement Arrangement { get; } = arrangement;
    public IReadOnlySet<string> Enterable { get; } = enterable;
    public bool Shown { get; } = shown;
    public bool Connected { get; } = connected;

    public static StripInputs Of(Agent.AgentSnapshot snapshot) => new(
        snapshot.Arrangement,
        DragStrips.Enterable(snapshot),
        DragStrips.Shown(snapshot),
        snapshot.Link == Agent.AgentLink.Connected);

    public bool Equals(StripInputs? other) =>
        other is not null
        && Shown == other.Shown && Connected == other.Connected
        && Enterable.SetEquals(other.Enterable)
        // The same message unless the service sent a new arrangement.
        && (ReferenceEquals(Arrangement, other.Arrangement) || Arrangement.Equals(other.Arrangement));

    public override bool Equals(object? obj) => Equals(obj as StripInputs);

    public override int GetHashCode() => HashCode.Combine(Shown, Connected, Enterable.Count, Arrangement.Version);
}

/// <summary>Whether a drag that left the strips without being dropped is ended here (EndDrag).</summary>
public static class StripLeave
{
    /// <summary>
    /// Ends the drag the service began (<paramref name="dragId"/>, from its
    /// answer to BeginDrag) unless the service already says the cursor took
    /// it to another computer, which ends it there. The status may lag
    /// behind the answer, so a drag it does not name yet is ended too. Null
    /// when the drag never began.
    /// </summary>
    public static bool Ends(string? dragId, Dragging? dragging) =>
        dragId is not null && !(dragging is { } now && now.DragId == dragId && now.DeviceId.Length != 0);
}
