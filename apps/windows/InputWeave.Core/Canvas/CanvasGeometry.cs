using Inputweave.App.V1;

namespace InputWeave.Canvas;

public readonly record struct CanvasPoint(double X, double Y)
{
    public CanvasPoint Offset(double dx, double dy) => new(X + dx, Y + dy);
}

public readonly record struct CanvasSize(double Width, double Height);

/// <summary>A rectangle in logical units, y growing downwards as the service uses.</summary>
public readonly record struct CanvasRect(double X, double Y, double Width, double Height)
{
    public double MinX => X;
    public double MinY => Y;
    public double MaxX => X + Width;
    public double MaxY => Y + Height;
    public double MidX => X + Width / 2;
    public double MidY => Y + Height / 2;

    public CanvasRect Offset(double dx, double dy) => this with { X = X + dx, Y = Y + dy };

    /// <summary>Grows the rectangle by <paramref name="dx"/> and <paramref name="dy"/> on each side; negative shrinks it.</summary>
    public CanvasRect Inflate(double dx, double dy) => new(X - dx, Y - dy, Width + 2 * dx, Height + 2 * dy);

    public bool Contains(CanvasPoint p) => p.X >= MinX && p.X <= MaxX && p.Y >= MinY && p.Y <= MaxY;

    public bool Contains(CanvasRect r) => r.MinX >= MinX && r.MaxX <= MaxX && r.MinY >= MinY && r.MaxY <= MaxY;

    public CanvasRect Union(CanvasRect other)
    {
        var minX = Math.Min(MinX, other.MinX);
        var minY = Math.Min(MinY, other.MinY);
        return new(minX, minY, Math.Max(MaxX, other.MaxX) - minX, Math.Max(MaxY, other.MaxY) - minY);
    }
}

/// <summary>
/// A computer on the arrangement canvas: its screens in its own desktop
/// coordinates, and where its desktop origin sits on the canvas. All in
/// logical units, y growing downwards, as the background service uses.
/// </summary>
public sealed record CanvasDevice(string Id, CanvasPoint Offset, IReadOnlyList<CanvasRect> Screens, int MainScreen = 0)
{
    /// <summary>The screens on the canvas.</summary>
    public IReadOnlyList<CanvasRect> Frames => Screens.Select(s => s.Offset(Offset.X, Offset.Y)).ToList();

    public CanvasRect Bounds => CanvasGeometry.Union(Frames) ?? default;

    public CanvasDevice MovedTo(CanvasPoint offset) => this with { Offset = offset };

    public bool Equals(CanvasDevice? other) =>
        other is not null && Id == other.Id && Offset == other.Offset && MainScreen == other.MainScreen
        && Screens.SequenceEqual(other.Screens);

    public override int GetHashCode() => HashCode.Combine(Id, Offset, Screens.Count, MainScreen);
}

/// <summary>A shared edge between two computers' screens: where the cursor crosses.</summary>
public readonly record struct Seam(CanvasPoint Start, CanvasPoint End);

/// <summary>
/// Geometry for drawing and editing the arrangement, free of UI so it can
/// be tested; the same rules as the macOS app's CanvasGeometry.
/// </summary>
public static class CanvasGeometry
{
    /// <summary>Edges closer than this count as touching.</summary>
    public const double Epsilon = 0.001;

    /// <summary>The computers that have a place and screens, on the canvas.</summary>
    public static IReadOnlyList<CanvasDevice> Devices(Arrangement arrangement) =>
        arrangement.Devices
            .Where(d => d.Placed && d.Screens.Count != 0)
            .Select(d => new CanvasDevice(
                d.Id,
                new CanvasPoint(d.X, d.Y),
                d.Screens.Select(s => new CanvasRect(s.X, s.Y, s.Width, s.Height)).ToList(),
                Math.Max(0, d.Screens.ToList().FindIndex(s => s.Primary))))
            .ToList();

    public static CanvasRect? Union(IEnumerable<CanvasRect> rects)
    {
        CanvasRect? union = null;
        foreach (var rect in rects)
        {
            union = union is { } u ? u.Union(rect) : rect;
        }
        return union;
    }

    /// <summary>Maps canvas coordinates into a view and back.</summary>
    public readonly record struct Fit(double Scale, CanvasPoint Origin)
    {
        public CanvasPoint ToView(CanvasPoint p) => new(Origin.X + p.X * Scale, Origin.Y + p.Y * Scale);

        public CanvasRect ToView(CanvasRect r) =>
            new(Origin.X + r.X * Scale, Origin.Y + r.Y * Scale, r.Width * Scale, r.Height * Scale);

        public CanvasPoint ToCanvas(CanvasPoint p) => new((p.X - Origin.X) / Scale, (p.Y - Origin.Y) / Scale);
    }

    /// <summary>Fits <paramref name="content"/> centered into a view, leaving <paramref name="padding"/> on every side.</summary>
    public static Fit FitInto(CanvasRect content, CanvasSize view, double padding)
    {
        if (content.Width <= 0 || content.Height <= 0)
        {
            return new Fit(1, new CanvasPoint(view.Width / 2, view.Height / 2));
        }
        var scale = Math.Min(
            (view.Width - 2 * padding) / content.Width,
            (view.Height - 2 * padding) / content.Height);
        scale = Math.Max(scale, 0.001);
        return new Fit(
            scale,
            new CanvasPoint(
                (view.Width - content.Width * scale) / 2 - content.MinX * scale,
                (view.Height - content.Height * scale) / 2 - content.MinY * scale));
    }

    /// <summary>Whether any screen of <paramref name="a"/> overlaps any screen of <paramref name="b"/>. Touching is not overlapping.</summary>
    public static bool Overlaps(IEnumerable<CanvasRect> a, IReadOnlyCollection<CanvasRect> b) =>
        a.Any(r => b.Any(s =>
            r.MinX < s.MaxX - Epsilon && s.MinX < r.MaxX - Epsilon
            && r.MinY < s.MaxY - Epsilon && s.MinY < r.MaxY - Epsilon));

    /// <summary>
    /// Moves <paramref name="moving"/> so that its edges meet or line up
    /// with the nearest edges of the other computers, if they are within
    /// <paramref name="threshold"/>, and returns its new offset. Each axis
    /// snaps on its own.
    /// </summary>
    public static CanvasPoint Snap(CanvasDevice moving, IEnumerable<CanvasDevice> others, double threshold)
    {
        double bestX = threshold + 1, dx = 0, bestY = threshold + 1, dy = 0;
        void ConsiderX(double value)
        {
            if (Math.Abs(value) <= threshold && Math.Abs(value) < bestX)
            {
                bestX = Math.Abs(value);
                dx = value;
            }
        }
        void ConsiderY(double value)
        {
            if (Math.Abs(value) <= threshold && Math.Abs(value) < bestY)
            {
                bestY = Math.Abs(value);
                dy = value;
            }
        }
        var otherFrames = others.SelectMany(o => o.Frames).ToList();
        foreach (var f in moving.Frames)
        {
            foreach (var o in otherFrames)
            {
                bool nearVertically = f.MinY < o.MaxY + threshold && o.MinY < f.MaxY + threshold;
                bool nearHorizontally = f.MinX < o.MaxX + threshold && o.MinX < f.MaxX + threshold;
                if (nearVertically)
                {
                    // Side by side.
                    ConsiderX(o.MaxX - f.MinX);
                    ConsiderX(o.MinX - f.MaxX);
                }
                if (nearHorizontally)
                {
                    // One above the other.
                    ConsiderY(o.MaxY - f.MinY);
                    ConsiderY(o.MinY - f.MaxY);
                }
                bool sideBySide = Math.Abs(o.MaxX - f.MinX) <= threshold || Math.Abs(o.MinX - f.MaxX) <= threshold;
                bool stacked = Math.Abs(o.MaxY - f.MinY) <= threshold || Math.Abs(o.MinY - f.MaxY) <= threshold;
                if (sideBySide)
                {
                    // Line up top or bottom edges.
                    ConsiderY(o.MinY - f.MinY);
                    ConsiderY(o.MaxY - f.MaxY);
                }
                if (stacked)
                {
                    ConsiderX(o.MinX - f.MinX);
                    ConsiderX(o.MaxX - f.MaxX);
                }
            }
        }
        return new CanvasPoint(moving.Offset.X + dx, moving.Offset.Y + dy);
    }

    /// <summary>The seams between screens of different computers.</summary>
    public static IReadOnlyList<Seam> Seams(IReadOnlyList<CanvasDevice> devices)
    {
        var seams = new List<Seam>();
        for (int i = 0; i < devices.Count; i++)
        {
            for (int j = i + 1; j < devices.Count; j++)
            {
                foreach (var p in devices[i].Frames)
                {
                    foreach (var q in devices[j].Frames)
                    {
                        if ((SeamBetween(p, q) ?? SeamBetween(q, p)) is { } seam)
                        {
                            seams.Add(seam);
                        }
                    }
                }
            }
        }
        return seams;
    }

    /// <summary>The computers that share a seam with <paramref name="device"/>.</summary>
    public static IReadOnlyList<string> Neighbours(CanvasDevice device, IReadOnlyList<CanvasDevice> all) =>
        all.Where(other => other.Id != device.Id && Seams([device, other]).Count != 0).Select(other => other.Id).ToList();

    /// <summary>
    /// The points of a zigzag stitch along a seam, <paramref name="pitch"/>
    /// apart and reaching <paramref name="reach"/> to either side; it starts
    /// and ends on the seam.
    /// </summary>
    public static IReadOnlyList<CanvasPoint> Stitch(CanvasPoint start, CanvasPoint end, double pitch, double reach)
    {
        var length = Math.Sqrt(Math.Pow(end.X - start.X, 2) + Math.Pow(end.Y - start.Y, 2));
        if (length <= 0)
        {
            return [];
        }
        var (ux, uy) = ((end.X - start.X) / length, (end.Y - start.Y) / length);
        var (nx, ny) = (-uy, ux);
        var count = Math.Max(1, (int)(length / pitch));
        var points = new List<CanvasPoint>(count + 1) { start };
        for (int i = 1; i <= count; i++)
        {
            var t = (double)i / count * length;
            var side = i % 2 == 0 ? -1.0 : 1.0;
            var off = i == count ? 0 : reach * side;
            points.Add(new CanvasPoint(start.X + ux * t + nx * off, start.Y + uy * t + ny * off));
        }
        return points;
    }

    /// <summary>
    /// Which of a computer's screen tiles the mini map labels: the main
    /// screen, else the widest, as long as the tile leaves room for the
    /// name. Null when none does, so the name is left out rather than
    /// spilling over its tile.
    /// </summary>
    public static int? LabelTile(IReadOnlyList<CanvasRect> tiles, int main, CanvasSize room)
    {
        bool Fits(CanvasRect tile) => tile.Width >= room.Width && tile.Height >= room.Height;
        if (main >= 0 && main < tiles.Count && Fits(tiles[main]))
        {
            return main;
        }
        int? widest = null;
        for (int i = 0; i < tiles.Count; i++)
        {
            if (Fits(tiles[i]) && (widest is not { } w || tiles[i].Width > tiles[w].Width))
            {
                widest = i;
            }
        }
        return widest;
    }

    /// <summary>
    /// How a computer's name label shares its room with the jump shortcut
    /// beside the name: <paramref name="room"/> is
    /// how wide the label's text may be at most, <paramref name="shortcut"/>
    /// the shortcut's measured width (0 for none) and <paramref name="gap"/>
    /// the space between the two. The shortcut is shown only while the name
    /// keeps at least <paramref name="leastName"/>, as on the macOS app's
    /// mini map; the name may then take what is left, and all of the room
    /// without it.
    /// </summary>
    public static (bool ShowsShortcut, double NameWidth) NameBeside(double room, double shortcut, double gap, double leastName)
    {
        room = Math.Max(room, 0);
        if (shortcut > 0)
        {
            var left = room - gap - Math.Ceiling(shortcut);
            if (left >= leastName)
            {
                return (true, left);
            }
        }
        return (false, room);
    }

    /// <summary>
    /// Where the keyboard-and-mouse badge of <paramref name="size"/> goes on
    /// <paramref name="tile"/>: in the middle, or in the middle of the space
    /// below the name when the name, ending at <paramref name="nameBottom"/>,
    /// would cover it, keeping <paramref name="inset"/> from the tile's
    /// edges. Without that much room below the name it sits as low as the
    /// tile allows. A badge larger than the tile stays centered on it and
    /// spills over evenly, since it is the only sign of where the keyboard
    /// and mouse are.
    /// </summary>
    public static CanvasRect BadgeFrame(CanvasSize size, CanvasRect tile, double? nameBottom, double inset)
    {
        var frame = new CanvasRect(tile.MidX - size.Width / 2, tile.MidY - size.Height / 2, size.Width, size.Height);
        const double gap = 1;
        var bottom = tile.MaxY - inset;
        if (nameBottom is not { } name || frame.MinY >= name + gap || size.Height > tile.Height - 2 * inset)
        {
            return frame;
        }
        var top = name + gap;
        return frame with
        {
            Y = bottom - top >= size.Height ? (top + bottom - size.Height) / 2 : bottom - size.Height,
        };
    }

    /// <summary>
    /// The computer under <paramref name="point"/> (view coordinates), by
    /// its screens grown by <paramref name="slop"/>; the topmost, drawn
    /// last, wins.
    /// </summary>
    public static string? HitTest(IReadOnlyList<CanvasDevice> devices, Fit fit, CanvasPoint point, double slop)
    {
        for (int i = devices.Count - 1; i >= 0; i--)
        {
            if (devices[i].Frames.Any(f => fit.ToView(f).Inflate(slop, slop).Contains(point)))
            {
                return devices[i].Id;
            }
        }
        return null;
    }

    /// <summary>The edge <paramref name="p"/> shares with <paramref name="q"/> where q is right of or below p.</summary>
    private static Seam? SeamBetween(CanvasRect p, CanvasRect q)
    {
        if (Math.Abs(p.MaxX - q.MinX) < Epsilon)
        {
            var (top, bottom) = (Math.Max(p.MinY, q.MinY), Math.Min(p.MaxY, q.MaxY));
            if (bottom - top > Epsilon)
            {
                return new Seam(new CanvasPoint(q.MinX, top), new CanvasPoint(q.MinX, bottom));
            }
        }
        if (Math.Abs(p.MaxY - q.MinY) < Epsilon)
        {
            var (left, right) = (Math.Max(p.MinX, q.MinX), Math.Min(p.MaxX, q.MaxX));
            if (right - left > Epsilon)
            {
                return new Seam(new CanvasPoint(left, q.MinY), new CanvasPoint(right, q.MinY));
            }
        }
        return null;
    }
}
