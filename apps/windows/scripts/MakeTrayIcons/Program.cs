// Draws the notification area icons from the same
// geometry as the macOS menu bar icon, apps/macos/InputWeave/Panel/StatusIcon.swift:
// two screens side by side with the two stitches that join them, on a
// 22 × 16 grid. Each of the five states changes one bold element.
//
// Windows draws the icon itself in no color, so every state comes twice:
// "light" is the dark glyph for a light taskbar, "dark" the light glyph for
// a dark one. Each .ico holds the sizes the notification area asks for at
// 100–300 % scaling, each drawn at its own size (not scaled down), with
// 8 × 8 samples a pixel.
//
//   dotnet run --project apps/windows/scripts/MakeTrayIcons [output folder]
//
// The default output folder is apps/windows/InputWeave/Assets/Tray, relative
// to the current directory (the repository root).

var output = args.Length > 0 ? args[0] : Path.Combine("apps", "windows", "InputWeave", "Assets", "Tray");
Directory.CreateDirectory(output);

int[] sizes = [16, 20, 24, 32, 40, 48, 64];
// The text colors of Windows 11: black at 89.6 % on light, white on dark.
(string Name, byte R, byte G, byte B, byte A)[] themes = [("light", 0, 0, 0, 0xE4), ("dark", 0xFF, 0xFF, 0xFF, 0xFF)];

foreach (var state in Enum.GetValues<State>())
{
    foreach (var theme in themes)
    {
        var images = sizes.Select(size => (size, Glyph.Render(state, size, theme.R, theme.G, theme.B, theme.A))).ToList();
        var file = Path.Combine(output, $"{state.ToString().ToLowerInvariant()}-{theme.Name}.ico");
        File.WriteAllBytes(file, Ico.Write(images));
        Console.WriteLine(file);
    }
}

enum State
{
    Normal,
    Controlling,
    Controlled,
    Paused,
    Attention,
}

static class Glyph
{
    // The grid of StatusIcon.swift, y growing downwards.
    const double GridWidth = 22;
    const double GridHeight = 16;

    // Two equal screens, 3 units apart; the stitches cross that seam.
    static readonly (double X, double Y, double W, double H) Left = (1.5, 3.5, 8, 9);
    static readonly (double X, double Y, double W, double H) Right = (12.5, 3.5, 8, 9);
    const double Corner = 2;
    const double Edge = 1.5;
    static readonly double[] StitchYs = [6, 10];
    const double StitchWidth = 2;

    const int Samples = 8;

    /// <summary>Straight (not premultiplied) RGBA, top row first.</summary>
    public static byte[] Render(State state, int size, byte r, byte g, byte b, byte a)
    {
        // The grid's width fills the square; its height is centered.
        double scale = size / GridWidth;
        double top = (size - GridHeight * scale) / 2;
        var pixels = new byte[size * size * 4];
        for (int py = 0; py < size; py++)
        {
            for (int px = 0; px < size; px++)
            {
                int covered = 0;
                for (int sy = 0; sy < Samples; sy++)
                {
                    for (int sx = 0; sx < Samples; sx++)
                    {
                        double x = (px + (sx + 0.5) / Samples) / scale;
                        double y = (py + (sy + 0.5) / Samples - top) / scale;
                        if (Ink(state, x, y))
                        {
                            covered++;
                        }
                    }
                }
                int i = (py * size + px) * 4;
                pixels[i] = r;
                pixels[i + 1] = g;
                pixels[i + 2] = b;
                pixels[i + 3] = (byte)Math.Round(a * covered / (double)(Samples * Samples));
            }
        }
        return pixels;
    }

    /// <summary>Whether a point of the grid is inked.</summary>
    static bool Ink(State state, double x, double y)
    {
        bool screens = Outline(Left, x, y)
            || (state == State.Controlling ? Filled(Right, x, y) : Outline(Right, x, y));
        bool stitches = state != State.Paused && StitchYs.Any(sy => Segment(x, y, 9, sy, 13, sy) <= StitchWidth / 2);
        bool mark = screens || stitches;
        return state switch
        {
            // The whole icon knocked out of a tile.
            State.Controlled => RoundRect(x, y, (0, 0, GridWidth, GridHeight), 4) <= 0 && !mark,
            // A ring of clear space keeps the dot apart from the screen.
            State.Attention => (mark && Circle(x, y, 19.25, 2.75, 3.75) > 0) || Circle(x, y, 19.25, 2.75, 2.5) <= 0,
            _ => mark,
        };
    }

    /// <summary>A screen's outline, stroked on its edge.</summary>
    static bool Outline((double X, double Y, double W, double H) rect, double x, double y) =>
        Math.Abs(RoundRect(x, y, rect, Corner)) <= Edge / 2;

    /// <summary>A lit screen: filled out to the outer side of its outline.</summary>
    static bool Filled((double X, double Y, double W, double H) rect, double x, double y) =>
        RoundRect(x, y, (rect.X - Edge / 2, rect.Y - Edge / 2, rect.W + Edge, rect.H + Edge), Corner + Edge / 2) <= 0;

    /// <summary>Signed distance to a rounded rectangle; negative inside.</summary>
    static double RoundRect(double x, double y, (double X, double Y, double W, double H) rect, double radius)
    {
        double qx = Math.Abs(x - (rect.X + rect.W / 2)) - (rect.W / 2 - radius);
        double qy = Math.Abs(y - (rect.Y + rect.H / 2)) - (rect.H / 2 - radius);
        double outside = Math.Sqrt(Math.Pow(Math.Max(qx, 0), 2) + Math.Pow(Math.Max(qy, 0), 2));
        return outside + Math.Min(Math.Max(qx, qy), 0) - radius;
    }

    /// <summary>Distance to a line segment, for strokes with round caps.</summary>
    static double Segment(double x, double y, double ax, double ay, double bx, double by)
    {
        double dx = bx - ax, dy = by - ay;
        double t = Math.Clamp(((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy), 0, 1);
        return Math.Sqrt(Math.Pow(x - (ax + t * dx), 2) + Math.Pow(y - (ay + t * dy), 2));
    }

    static double Circle(double x, double y, double cx, double cy, double radius) =>
        Math.Sqrt((x - cx) * (x - cx) + (y - cy) * (y - cy)) - radius;
}

static class Ico
{
    /// <summary>An .ico of 32-bit bitmaps, one per size.</summary>
    public static byte[] Write(IReadOnlyList<(int Size, byte[] Rgba)> images)
    {
        var bitmaps = images.Select(i => Bitmap(i.Size, i.Rgba)).ToList();
        using var stream = new MemoryStream();
        using var writer = new BinaryWriter(stream);
        writer.Write((ushort)0); // reserved
        writer.Write((ushort)1); // icon
        writer.Write((ushort)images.Count);
        int offset = 6 + 16 * images.Count;
        for (int i = 0; i < images.Count; i++)
        {
            int size = images[i].Size;
            writer.Write((byte)(size >= 256 ? 0 : size));
            writer.Write((byte)(size >= 256 ? 0 : size));
            writer.Write((byte)0); // no palette
            writer.Write((byte)0); // reserved
            writer.Write((ushort)1); // planes
            writer.Write((ushort)32); // bits per pixel
            writer.Write((uint)bitmaps[i].Length);
            writer.Write((uint)offset);
            offset += bitmaps[i].Length;
        }
        foreach (var bitmap in bitmaps)
        {
            writer.Write(bitmap);
        }
        writer.Flush();
        return stream.ToArray();
    }

    /// <summary>
    /// A DIB as icons store it: a BITMAPINFOHEADER of twice the height,
    /// BGRA rows bottom up, then a 1-bit AND mask set where fully clear.
    /// </summary>
    static byte[] Bitmap(int size, byte[] rgba)
    {
        int maskStride = (size + 31) / 32 * 4;
        using var stream = new MemoryStream();
        using var writer = new BinaryWriter(stream);
        writer.Write(40u); // biSize
        writer.Write(size); // biWidth
        writer.Write(size * 2); // biHeight: color and mask
        writer.Write((ushort)1); // biPlanes
        writer.Write((ushort)32); // biBitCount
        writer.Write(0u); // BI_RGB
        writer.Write((uint)(size * size * 4 + maskStride * size)); // biSizeImage
        writer.Write(0); // biXPelsPerMeter
        writer.Write(0); // biYPelsPerMeter
        writer.Write(0u); // biClrUsed
        writer.Write(0u); // biClrImportant
        for (int y = size - 1; y >= 0; y--)
        {
            for (int x = 0; x < size; x++)
            {
                int i = (y * size + x) * 4;
                writer.Write(rgba[i + 2]);
                writer.Write(rgba[i + 1]);
                writer.Write(rgba[i]);
                writer.Write(rgba[i + 3]);
            }
        }
        for (int y = size - 1; y >= 0; y--)
        {
            var row = new byte[maskStride];
            for (int x = 0; x < size; x++)
            {
                if (rgba[(y * size + x) * 4 + 3] == 0)
                {
                    row[x / 8] |= (byte)(0x80 >> (x % 8));
                }
            }
            writer.Write(row);
        }
        writer.Flush();
        return stream.ToArray();
    }
}
