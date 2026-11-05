using System.Buffers.Binary;
using Google.Protobuf;

namespace InputWeave.Agent;

/// <summary>
/// The framing on the background service's pipe, as in iw-proto's
/// <c>frame</c> module: a big-endian u32 length, then one protobuf message.
/// </summary>
public static class Frame
{
    /// <summary>The service refuses larger frames, and so does the app.</summary>
    public const int MaxLength = 4 * 1024 * 1024;

    public const int HeaderLength = 4;

    public static byte[] Encode(IMessage message)
    {
        int length = message.CalculateSize();
        if (length > MaxLength)
        {
            throw new FrameTooLargeException(length);
        }
        var frame = new byte[HeaderLength + length];
        BinaryPrimitives.WriteUInt32BigEndian(frame, (uint)length);
        message.WriteTo(frame.AsSpan(HeaderLength));
        return frame;
    }

    /// <summary>
    /// Reads the body of the next frame, or returns null when the stream
    /// ends cleanly between frames.
    /// </summary>
    public static async Task<byte[]?> ReadAsync(Stream stream, CancellationToken cancellation)
    {
        var header = new byte[HeaderLength];
        int read = await stream
            .ReadAtLeastAsync(header, HeaderLength, throwOnEndOfStream: false, cancellation)
            .ConfigureAwait(false);
        if (read == 0)
        {
            return null;
        }
        if (read < HeaderLength)
        {
            throw new EndOfStreamException("the connection closed inside a frame header");
        }
        uint length = BinaryPrimitives.ReadUInt32BigEndian(header);
        if (length > MaxLength)
        {
            throw new FrameTooLargeException(length);
        }
        var body = new byte[length];
        await stream.ReadExactlyAsync(body, cancellation).ConfigureAwait(false);
        return body;
    }
}

public sealed class FrameTooLargeException(long length)
    : IOException($"a frame of {length} bytes is over the {Frame.MaxLength}-byte limit")
{
    public long Length { get; } = length;
}
