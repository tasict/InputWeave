using System.Runtime.InteropServices;
using Microsoft.Win32;

namespace InputWeave.Overlay;

/// <summary>
/// A larger copy of a cursor, for the locator to grow: its pixels as
/// premultiplied BGRA (<see cref="CursorPixels"/>) and the cursor's shape
/// as Windows shows it. Read through GDI, which reads the cursor of any
/// app; the cursor itself is left as it is.
/// </summary>
internal sealed unsafe partial class CursorPicture
{
    private CursorPicture(nint cursor, CursorShape shape, Image copy, string source)
    {
        Cursor = cursor;
        Shape = shape;
        Width = copy.Width;
        Height = copy.Height;
        Pixels = copy.Pixels;
        Source = source;
    }

    /// <summary>The cursor this is a copy of.</summary>
    public nint Cursor { get; }

    /// <summary>The cursor's size and hotspot, in the pixels it is shown in.</summary>
    public CursorShape Shape { get; }

    /// <summary>The copy's size in pixels.</summary>
    public int Width { get; }

    public int Height { get; }

    /// <summary>The copy, premultiplied BGRA, top-down.</summary>
    public byte[] Pixels { get; }

    /// <summary>How the copy was made, for the log.</summary>
    public string Source { get; }

    /// <summary>The cursor shown now, or the standard arrow when none is (typing hides it in some apps).</summary>
    public static nint Shown()
    {
        var info = new CursorInfo { Size = sizeof(CursorInfo) };
        return GetCursorInfo(&info) && (info.Flags & CURSOR_SHOWING) != 0 && info.Cursor != 0
            ? info.Cursor
            : LoadCursor(0, IDC_ARROW);
    }

    /// <summary>
    /// A copy of <paramref name="cursor"/> <paramref name="times"/> its size,
    /// the sharpest Windows gives: a system cursor loaded at that size from
    /// the file the user's cursor scheme draws it from; else the cursor
    /// loaded again at that size from the module it came from; else
    /// stretched by Windows; else as it is, for the window to stretch. The
    /// standard arrow's when the cursor cannot be read; null when not even
    /// that can be.
    /// </summary>
    public static CursorPicture? Of(nint cursor, double times)
    {
        nint arrow = LoadCursor(0, IDC_ARROW);
        return Copy(cursor, times, "")
            ?? (cursor != arrow && arrow != 0 ? Copy(arrow, times, "the arrow, ") : null);
    }

    private static CursorPicture? Copy(nint cursor, double times, string what)
    {
        if (Read(cursor) is not { } own)
        {
            return null;
        }
        int width = (int)Math.Round(own.Width * times);
        int height = (int)Math.Round(own.Height * times);
        // Windows keeps the system cursors as its own (USER32's), so
        // reloading one gets its built-in drawing stretched, not the
        // scheme's file, which holds larger drawings.
        var ways = new (string How, Func<nint> Make)[]
        {
            ("from the scheme", () => SchemeFile(cursor) is { } file ? LoadImage(0, file, IMAGE_CURSOR, width, height, LR_LOADFROMFILE) : 0),
            ("reloaded", () => CopyImage(cursor, IMAGE_CURSOR, width, height, LR_COPYFROMRESOURCE)),
            ("stretched", () => CopyImage(cursor, IMAGE_CURSOR, width, height, 0)),
        };
        foreach (var (how, make) in ways)
        {
            nint copy = make();
            if (copy == 0)
            {
                continue;
            }
            try
            {
                if (Read(copy) is { } large)
                {
                    // The copy's hotspot, in the cursor's pixels.
                    var shape = new CursorShape(
                        own.Width, own.Height,
                        LocatorGeometry.Hotspot(large.HotX * (double)own.Width / large.Width, own.HotX),
                        LocatorGeometry.Hotspot(large.HotY * (double)own.Height / large.Height, own.HotY));
                    return new CursorPicture(cursor, shape, large, $"{what}{how}");
                }
            }
            finally
            {
                DestroyCursor(copy);
            }
        }
        return new CursorPicture(cursor, new CursorShape(own.Width, own.Height, own.HotX, own.HotY), own, $"{what}as is");
    }

    /// <summary>
    /// The system cursors (<c>IDC_*</c>) and the names under which the
    /// user's cursor scheme keeps their files, in
    /// <c>HKCU\Control Panel\Cursors</c>.
    /// </summary>
    private static readonly (int Id, string Name)[] SystemCursors =
    [
        (32512, "Arrow"), (32513, "IBeam"), (32514, "Wait"), (32515, "Crosshair"), (32516, "UpArrow"),
        (32642, "SizeNWSE"), (32643, "SizeNESW"), (32644, "SizeWE"), (32645, "SizeNS"), (32646, "SizeAll"),
        (32648, "No"), (32649, "Hand"), (32650, "AppStarting"), (32651, "Help"), (32671, "Pin"), (32672, "Person"),
    ];

    /// <summary>
    /// The file the user's cursor scheme draws <paramref name="cursor"/>
    /// from; null when it is not a system cursor, or the scheme leaves it
    /// to Windows' own.
    /// </summary>
    private static string? SchemeFile(nint cursor)
    {
        foreach (var (id, name) in SystemCursors)
        {
            if (LoadCursor(0, id) != cursor)
            {
                continue;
            }
            var file = Registry.GetValue(@"HKEY_CURRENT_USER\Control Panel\Cursors", name, null) as string;
            return string.IsNullOrWhiteSpace(file) ? null : Environment.ExpandEnvironmentVariables(file);
        }
        return null;
    }

    /// <summary>A cursor's size, hotspot and pixels.</summary>
    private sealed record Image(int Width, int Height, int HotX, int HotY, byte[] Pixels);

    /// <summary>The largest cursor read: a 256-pixel cursor at four times its size.</summary>
    private const int Largest = 1024;

    private static Image? Read(nint cursor)
    {
        var info = default(IconInfo);
        if (!GetIconInfo(cursor, &info))
        {
            return null;
        }
        nint dc = 0;
        try
        {
            BitmapObject mask;
            if (info.Mask == 0 || GetObject(info.Mask, sizeof(BitmapObject), &mask) == 0)
            {
                return null;
            }
            // A monochrome cursor's mask holds both halves, AND above XOR.
            bool monochrome = info.Color == 0;
            int width = mask.Width;
            int height = monochrome ? mask.Height / 2 : mask.Height;
            if (width <= 0 || height <= 0 || width > Largest || height > Largest)
            {
                return null;
            }
            dc = GetDC(0);
            byte[]? color = monochrome ? [] : Bits(dc, info.Color, width, height, 32);
            byte[]? bits = Bits(dc, info.Mask, width, monochrome ? height * 2 : height, 1);
            if (color is null || bits is null)
            {
                return null;
            }
            return new Image(width, height, (int)info.HotX, (int)info.HotY, CursorPixels.Premultiplied(width, height, color, bits));
        }
        finally
        {
            if (dc != 0)
            {
                _ = ReleaseDC(0, dc);
            }
            // GetIconInfo made these for the caller.
            if (info.Mask != 0)
            {
                DeleteObject(info.Mask);
            }
            if (info.Color != 0)
            {
                DeleteObject(info.Color);
            }
        }
    }

    /// <summary>A bitmap's rows, top-down, at <paramref name="bitCount"/> bits a pixel (1 or 32).</summary>
    private static byte[]? Bits(nint dc, nint bitmap, int width, int height, ushort bitCount)
    {
        int stride = bitCount == 1 ? CursorPixels.MaskStride(width) : width * 4;
        var bits = new byte[stride * height];
        var info = new BitmapInfo
        {
            Header = new BitmapInfoHeader
            {
                Size = sizeof(BitmapInfoHeader),
                Width = width,
                Height = -height,
                Planes = 1,
                BitCount = bitCount,
            },
        };
        fixed (byte* at = bits)
        {
            return GetDIBits(dc, bitmap, 0, (uint)height, at, &info, DIB_RGB_COLORS) == height ? bits : null;
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct CursorInfo
    {
        public int Size;
        public uint Flags;
        public nint Cursor;
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct IconInfo
    {
        public int IsIcon;
        public uint HotX;
        public uint HotY;
        public nint Mask;
        public nint Color;
    }

    /// <summary>BITMAP.</summary>
    [StructLayout(LayoutKind.Sequential)]
    private struct BitmapObject
    {
        public int Type;
        public int Width;
        public int Height;
        public int WidthBytes;
        public ushort Planes;
        public ushort BitsPixel;
        public nint Bits;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct BitmapInfoHeader
    {
        public int Size;
        public int Width;
        public int Height;
        public ushort Planes;
        public ushort BitCount;
        public uint Compression;
        public uint SizeImage;
        public int XPelsPerMeter;
        public int YPelsPerMeter;
        public uint ColorsUsed;
        public uint ColorsImportant;
    }

    /// <summary>BITMAPINFO with room for the two colors of a one-bit bitmap.</summary>
    [StructLayout(LayoutKind.Sequential)]
    private struct BitmapInfo
    {
        public BitmapInfoHeader Header;
        public uint Color0;
        public uint Color1;
    }

    private const uint CURSOR_SHOWING = 0x1;
    private const uint IMAGE_CURSOR = 2;
    private const uint LR_COPYFROMRESOURCE = 0x4000;
    private const uint LR_LOADFROMFILE = 0x10;
    private const uint DIB_RGB_COLORS = 0;
    private static readonly nint IDC_ARROW = 32512;

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool GetCursorInfo(CursorInfo* info);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool GetIconInfo(nint icon, IconInfo* info);

    [LibraryImport("user32.dll")]
    private static partial nint CopyImage(nint image, uint type, int width, int height, uint flags);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool DestroyCursor(nint cursor);

    [LibraryImport("user32.dll", EntryPoint = "LoadCursorW")]
    private static partial nint LoadCursor(nint instance, nint name);

    [LibraryImport("user32.dll", EntryPoint = "LoadImageW", StringMarshalling = StringMarshalling.Utf16)]
    private static partial nint LoadImage(nint instance, string name, uint type, int width, int height, uint flags);

    [LibraryImport("user32.dll")]
    private static partial nint GetDC(nint hwnd);

    [LibraryImport("user32.dll")]
    private static partial int ReleaseDC(nint hwnd, nint dc);

    [LibraryImport("gdi32.dll")]
    private static partial int GetDIBits(nint dc, nint bitmap, uint start, uint lines, void* bits, BitmapInfo* info, uint usage);

    [LibraryImport("gdi32.dll", EntryPoint = "GetObjectW")]
    private static partial int GetObject(nint handle, int size, void* data);

    [LibraryImport("gdi32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool DeleteObject(nint handle);
}
