using System.Buffers.Binary;

namespace InputWeave.Canvas;

/// <summary>
/// The checks on a desktop picture (<c>Screen.preview_jpeg</c>) before the
/// app hands it to a decoder. Other computers send these bytes, so nothing
/// is assumed about them: only a JPEG of a sensible size, whose frame
/// header says a sensible picture size, gets decoded at all.
/// </summary>
public static class DesktopPicture
{
    /// <summary>The background service passes on previews of at most 96 KB; anything far larger is not one.</summary>
    public const int MaxBytes = 512 * 1024;

    /// <summary>
    /// Previews are at most 320 pixels on their long edge. A JPEG claiming
    /// far more would take memory out of all proportion once drawn.
    /// </summary>
    public const int MaxPixels = 4096;

    /// <summary>Whether <paramref name="data"/> is worth decoding: a JPEG within the limits.</summary>
    public static bool Acceptable(ReadOnlySpan<byte> data) =>
        data.Length <= MaxBytes && Size(data) is { Width: > 0 and <= MaxPixels, Height: > 0 and <= MaxPixels };

    /// <summary>
    /// The picture size a JPEG's frame header declares, or null when the
    /// bytes are not a JPEG or end before the frame header.
    /// </summary>
    public static (int Width, int Height)? Size(ReadOnlySpan<byte> data)
    {
        // SOI, then the first marker.
        if (data.Length < 4 || data[0] != 0xFF || data[1] != 0xD8 || data[2] != 0xFF)
        {
            return null;
        }
        int at = 2;
        while (at < data.Length)
        {
            if (data[at] != 0xFF)
            {
                return null;
            }
            // Fill bytes may precede a marker.
            while (at < data.Length && data[at] == 0xFF)
            {
                at++;
            }
            if (at >= data.Length)
            {
                return null;
            }
            byte marker = data[at++];
            // Markers without a segment.
            if (marker is 0x01 or (>= 0xD0 and <= 0xD7))
            {
                continue;
            }
            // The image data or its end before any frame header.
            if (marker is 0xD9 or 0xDA || at + 2 > data.Length)
            {
                return null;
            }
            int length = BinaryPrimitives.ReadUInt16BigEndian(data[at..]);
            if (length < 2 || at + length > data.Length)
            {
                return null;
            }
            // SOF0–SOF15, except DHT (C4), JPG (C8) and DAC (CC).
            if (marker is >= 0xC0 and <= 0xCF and not 0xC4 and not 0xC8 and not 0xCC)
            {
                if (length < 7)
                {
                    return null;
                }
                int height = BinaryPrimitives.ReadUInt16BigEndian(data[(at + 3)..]);
                int width = BinaryPrimitives.ReadUInt16BigEndian(data[(at + 5)..]);
                return (width, height);
            }
            at += length;
        }
        return null;
    }
}

/// <summary>
/// Decoded desktop pictures by screen, so a tile decodes its picture once
/// rather than on every redraw. A screen's entry is replaced when its bytes
/// change; empty bytes or ones that do not pass <see cref="DesktopPicture"/>
/// give no picture.
/// </summary>
/// <typeparam name="T">The decoded picture.</typeparam>
public sealed class PictureCache<T>(Func<byte[], T?> decode) where T : class
{
    /// <summary>
    /// Far more screens than any group of computers has; past it, the cache
    /// starts over rather than keep screens that are long gone.
    /// </summary>
    public const int Capacity = 64;

    private sealed record Entry(byte[] Data, T? Picture);

    private readonly Dictionary<(string Device, string Screen), Entry> entries = [];

    /// <summary>How many times bytes were decoded, for tests.</summary>
    public int Decodes { get; private set; }

    /// <summary>Screen ids come from each computer, so the key is both.</summary>
    public T? Picture(string device, string screen, ReadOnlySpan<byte> data)
    {
        var key = (device, screen);
        if (entries.TryGetValue(key, out var entry) && data.SequenceEqual(entry.Data))
        {
            return entry.Picture;
        }
        if (data.IsEmpty)
        {
            entries.Remove(key);
            return null;
        }
        if (entries.Count >= Capacity)
        {
            entries.Clear();
        }
        var bytes = data.ToArray();
        T? picture = null;
        if (DesktopPicture.Acceptable(bytes))
        {
            Decodes++;
            picture = decode(bytes);
        }
        entries[key] = new Entry(bytes, picture);
        return picture;
    }

    /// <summary>Forgets a picture that turned out not to decode, so the tile shows its color.</summary>
    public void Fail(T picture)
    {
        foreach (var (key, entry) in entries.Where(e => ReferenceEquals(e.Value.Picture, picture)).ToList())
        {
            entries[key] = entry with { Picture = null };
        }
    }
}
