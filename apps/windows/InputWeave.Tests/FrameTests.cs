using Google.Protobuf;
using Inputweave.App.V1;
using InputWeave.Agent;
using Xunit;

namespace InputWeave.Tests;

public class FrameTests
{
    [Fact]
    public void PrefixesTheBodyWithItsBigEndianLength()
    {
        var message = new ClientMessage { RequestId = 7, Hello = new ClientHello { IpcVersion = 1, Client = "windows-ui/0.1.0" } };
        var frame = Frame.Encode(message);
        var length = message.CalculateSize();
        Assert.Equal(Frame.HeaderLength + length, frame.Length);
        Assert.Equal(new byte[] { 0, 0, (byte)(length >> 8), (byte)length }, frame[..4]);
        Assert.Equal(message, ClientMessage.Parser.ParseFrom(frame[4..]));
    }

    [Fact]
    public async Task ReadsFramesBackToBackUntilTheStreamEnds()
    {
        var first = new AgentMessage { Response = new Response { RequestId = 1, Hello = new AgentHello { IpcVersion = 1, AgentVersion = "0.1.0" } } };
        var second = new AgentMessage { Event = new Event { StatusChanged = new Status { Sharing = true } } };
        // An empty message is a frame of length zero.
        var third = new AgentMessage();
        var stream = new MemoryStream([.. Frame.Encode(first), .. Frame.Encode(second), .. Frame.Encode(third)]);

        Assert.Equal(first, AgentMessage.Parser.ParseFrom((await Frame.ReadAsync(stream, default))!));
        Assert.Equal(second, AgentMessage.Parser.ParseFrom((await Frame.ReadAsync(stream, default))!));
        Assert.Equal(third, AgentMessage.Parser.ParseFrom((await Frame.ReadAsync(stream, default))!));
        Assert.Null(await Frame.ReadAsync(stream, default));
    }

    [Fact]
    public async Task RefusesALengthOverTheLimitBeforeReadingTheBody()
    {
        var header = new byte[] { 0x00, 0x40, 0x00, 0x01 }; // 4 MiB + 1
        var error = await Assert.ThrowsAsync<FrameTooLargeException>(
            () => Frame.ReadAsync(new MemoryStream(header), default));
        Assert.Equal(Frame.MaxLength + 1, error.Length);
    }

    [Fact]
    public async Task AcceptsALengthAtTheLimit()
    {
        var body = new byte[Frame.MaxLength];
        var stream = new MemoryStream([0x00, 0x40, 0x00, 0x00, .. body]);
        Assert.Equal(Frame.MaxLength, (await Frame.ReadAsync(stream, default))!.Length);
    }

    [Fact]
    public async Task ACutOffFrameIsAnError()
    {
        await Assert.ThrowsAsync<EndOfStreamException>(
            () => Frame.ReadAsync(new MemoryStream([0x00, 0x00]), default));
        await Assert.ThrowsAsync<EndOfStreamException>(
            () => Frame.ReadAsync(new MemoryStream([0x00, 0x00, 0x00, 0x05, 0x01]), default));
    }

    [Fact]
    public void DoesNotSendAFrameOverTheLimit()
    {
        var message = new SetArrangement();
        message.Positions.Add(new DevicePosition { DeviceId = new string('x', Frame.MaxLength) });
        Assert.Throws<FrameTooLargeException>(() => Frame.Encode(message));
    }
}
