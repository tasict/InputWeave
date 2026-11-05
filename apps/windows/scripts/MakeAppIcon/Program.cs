// Writes the app's icon (the executable, the Start menu shortcut, Settings ›
// Apps) from the macOS app icon, which apps/macos/scripts/make-brand-assets.swift
// draws from the brand geometry at each size with the detail that size
// keeps (docs/brand/README.md): two screens and a bold stitch at 32 px and
// below, a solid stitch at 64 px, everything from 128 px.
//
// Each size of the .ico is scaled down from the smallest of those drawings
// that is at least as large, so it keeps that drawing's detail. Sizes below
// 256 px are 32-bit bitmaps, 256 px is PNG, as Windows expects.
//
//   dotnet run --project apps/windows/scripts/MakeAppIcon [output file]
//
// The default output is apps/windows/InputWeave/Assets/InputWeave.ico,
// relative to the current directory (the repository root).

using System.Runtime.InteropServices.WindowsRuntime;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

var output = args.Length > 0 ? args[0] : Path.Combine("apps", "windows", "InputWeave", "Assets", "InputWeave.ico");
var iconSet = Path.Combine("apps", "macos", "InputWeave", "Assets.xcassets", "AppIcon.appiconset");

// The drawings, by pixel size.
(int Size, string File)[] drawings =
[
    (16, "icon_16x16.png"),
    (32, "icon_32x32.png"),
    (64, "icon_32x32@2x.png"),
    (128, "icon_128x128.png"),
    (256, "icon_256x256.png"),
];
// What Windows asks for at 100–200 % scaling, small icons to the largest view.
int[] sizes = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

var images = new List<(int Size, byte[] Data)>();
foreach (var size in sizes)
{
    var source = drawings.First(d => d.Size >= size);
    var png = await File.ReadAllBytesAsync(Path.Combine(iconSet, source.File));
    var bgra = await Images.Scale(png, size);
    images.Add((size, size >= 256 ? await Images.Png(bgra, size) : Ico.Bitmap(size, bgra)));
}
Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(output))!);
await File.WriteAllBytesAsync(output, Ico.Write(images));
Console.WriteLine($"{output}: {string.Join(", ", sizes)} px");

static class Images
{
    /// <summary>Decodes a PNG, scaled to a square of the given size: straight BGRA, top row first.</summary>
    public static async Task<byte[]> Scale(byte[] png, int size)
    {
        using var stream = new InMemoryRandomAccessStream();
        await stream.WriteAsync(png.AsBuffer());
        stream.Seek(0);
        var decoder = await BitmapDecoder.CreateAsync(stream);
        var transform = new BitmapTransform
        {
            ScaledWidth = (uint)size,
            ScaledHeight = (uint)size,
            InterpolationMode = BitmapInterpolationMode.Fant,
        };
        var pixels = await decoder.GetPixelDataAsync(
            BitmapPixelFormat.Bgra8, BitmapAlphaMode.Straight, transform,
            ExifOrientationMode.IgnoreExifOrientation, ColorManagementMode.DoNotColorManage);
        return pixels.DetachPixelData();
    }

    /// <summary>Encodes straight BGRA as a PNG.</summary>
    public static async Task<byte[]> Png(byte[] bgra, int size)
    {
        using var stream = new InMemoryRandomAccessStream();
        var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, stream);
        encoder.SetPixelData(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Straight, (uint)size, (uint)size, 96, 96, bgra);
        await encoder.FlushAsync();
        stream.Seek(0);
        using var bytes = new MemoryStream();
        await stream.AsStreamForRead().CopyToAsync(bytes);
        return bytes.ToArray();
    }
}

static class Ico
{
    /// <summary>An .ico of the given images, each already in its stored form.</summary>
    public static byte[] Write(IReadOnlyList<(int Size, byte[] Data)> images)
    {
        using var stream = new MemoryStream();
        using var writer = new BinaryWriter(stream);
        writer.Write((ushort)0); // reserved
        writer.Write((ushort)1); // icon
        writer.Write((ushort)images.Count);
        int offset = 6 + 16 * images.Count;
        foreach (var (size, data) in images)
        {
            writer.Write((byte)(size >= 256 ? 0 : size));
            writer.Write((byte)(size >= 256 ? 0 : size));
            writer.Write((byte)0); // no palette
            writer.Write((byte)0); // reserved
            writer.Write((ushort)1); // planes
            writer.Write((ushort)32); // bits per pixel
            writer.Write((uint)data.Length);
            writer.Write((uint)offset);
            offset += data.Length;
        }
        foreach (var (_, data) in images)
        {
            writer.Write(data);
        }
        writer.Flush();
        return stream.ToArray();
    }

    /// <summary>
    /// A DIB as icons store it: a BITMAPINFOHEADER of twice the height,
    /// BGRA rows bottom up, then a 1-bit AND mask set where fully clear.
    /// </summary>
    public static byte[] Bitmap(int size, byte[] bgra)
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
            writer.Write(bgra, y * size * 4, size * 4);
        }
        for (int y = size - 1; y >= 0; y--)
        {
            var row = new byte[maskStride];
            for (int x = 0; x < size; x++)
            {
                if (bgra[(y * size + x) * 4 + 3] == 0)
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
