namespace InputWeave.Overlay;

/// <summary>
/// A cursor's pixels as Windows keeps them, made into the premultiplied
/// BGRA of a bitmap the locator can draw. A cursor has a mask of one bit a
/// pixel (AND) and, unless it is monochrome, a color bitmap; a monochrome
/// cursor's mask is twice as tall, the AND half above the XOR half. Both
/// come top-down from <c>GetDIBits</c>: the color at 32 bits a pixel, the
/// mask at one bit, each row padded to 32 bits, the leftmost pixel in the
/// highest bit.
/// </summary>
public static class CursorPixels
{
    /// <summary>The bytes in one row of a mask <paramref name="width"/> pixels wide.</summary>
    public static int MaskStride(int width) => (width + 31) / 32 * 4;

    /// <summary>
    /// The cursor as premultiplied BGRA, <paramref name="width"/> ×
    /// <paramref name="height"/>, top-down. With alpha in its color, the
    /// color is taken as it is; with color but no alpha (every alpha byte
    /// 0), a pixel is opaque where the AND mask is clear and transparent
    /// where it is set. A monochrome cursor (no color) is black where both
    /// bits are clear, white where only XOR is set, transparent where only
    /// AND is, and black where both are, since a window above the screen
    /// cannot invert what is below it as the cursor does there.
    /// </summary>
    /// <param name="color">32-bit BGRA with straight alpha, or empty for a monochrome cursor.</param>
    /// <param name="mask">The AND mask, followed by the XOR mask for a monochrome cursor.</param>
    public static byte[] Premultiplied(int width, int height, ReadOnlySpan<byte> color, ReadOnlySpan<byte> mask)
    {
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(width);
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(height);
        bool monochrome = color.IsEmpty;
        int stride = MaskStride(width);
        if (!monochrome && color.Length < width * height * 4)
        {
            throw new ArgumentException($"{color.Length} bytes of color for {width} × {height}", nameof(color));
        }
        if (mask.Length < stride * height * (monochrome ? 2 : 1))
        {
            throw new ArgumentException($"{mask.Length} bytes of mask for {width} × {height}", nameof(mask));
        }
        var pixels = new byte[width * height * 4];
        bool alpha = false;
        for (int i = 3; !monochrome && i < width * height * 4; i += 4)
        {
            alpha |= color[i] != 0;
        }
        for (int y = 0; y < height; y++)
        {
            for (int x = 0; x < width; x++)
            {
                int at = (y * width + x) * 4;
                bool and = Bit(mask, stride, x, y);
                if (monochrome)
                {
                    bool xor = Bit(mask, stride, x, y + height);
                    if (and && !xor)
                    {
                        continue;
                    }
                    byte value = !and && xor ? (byte)255 : (byte)0;
                    pixels[at] = pixels[at + 1] = pixels[at + 2] = value;
                    pixels[at + 3] = 255;
                }
                else if (alpha)
                {
                    byte a = color[at + 3];
                    pixels[at] = Times(color[at], a);
                    pixels[at + 1] = Times(color[at + 1], a);
                    pixels[at + 2] = Times(color[at + 2], a);
                    pixels[at + 3] = a;
                }
                else if (!and)
                {
                    pixels[at] = color[at];
                    pixels[at + 1] = color[at + 1];
                    pixels[at + 2] = color[at + 2];
                    pixels[at + 3] = 255;
                }
            }
        }
        return pixels;
    }

    private static bool Bit(ReadOnlySpan<byte> mask, int stride, int x, int y) =>
        (mask[y * stride + x / 8] & (0x80 >> (x % 8))) != 0;

    private static byte Times(byte value, byte alpha) => (byte)((value * alpha + 127) / 255);
}
