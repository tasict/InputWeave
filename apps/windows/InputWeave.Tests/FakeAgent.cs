using System.Threading.Channels;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;

namespace InputWeave.Tests;

/// <summary>One end of an in-memory, two-way byte stream.</summary>
internal sealed class StreamEnd(ChannelReader<byte[]> incoming, ChannelWriter<byte[]> outgoing) : Stream
{
    private readonly CancellationTokenSource closed = new();
    private byte[] current = [];
    private int offset;
    private bool disposed;

    public static (StreamEnd, StreamEnd) Pair()
    {
        var a = Channel.CreateUnbounded<byte[]>();
        var b = Channel.CreateUnbounded<byte[]>();
        return (new StreamEnd(a.Reader, b.Writer), new StreamEnd(b.Reader, a.Writer));
    }

    public override async ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken cancellation = default)
    {
        ObjectDisposedException.ThrowIf(disposed, this);
        while (offset == current.Length)
        {
            // Closing this end fails a pending read, as closing a pipe does.
            using var either = CancellationTokenSource.CreateLinkedTokenSource(cancellation, closed.Token);
            bool more;
            try
            {
                more = await incoming.WaitToReadAsync(either.Token).ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (closed.IsCancellationRequested)
            {
                throw new ObjectDisposedException(nameof(StreamEnd));
            }
            if (!more)
            {
                return 0;
            }
            if (incoming.TryRead(out var next))
            {
                current = next;
                offset = 0;
            }
        }
        var count = Math.Min(buffer.Length, current.Length - offset);
        current.AsMemory(offset, count).CopyTo(buffer);
        offset += count;
        return count;
    }

    public override ValueTask WriteAsync(ReadOnlyMemory<byte> buffer, CancellationToken cancellation = default)
    {
        ObjectDisposedException.ThrowIf(disposed, this);
        if (!outgoing.TryWrite(buffer.ToArray()))
        {
            throw new IOException("the other end is closed");
        }
        return ValueTask.CompletedTask;
    }

    public override Task<int> ReadAsync(byte[] buffer, int offset, int count, CancellationToken cancellation) =>
        ReadAsync(buffer.AsMemory(offset, count), cancellation).AsTask();

    public override Task WriteAsync(byte[] buffer, int offset, int count, CancellationToken cancellation) =>
        WriteAsync(buffer.AsMemory(offset, count), cancellation).AsTask();

    public override int Read(byte[] buffer, int offset, int count) => ReadAsync(buffer, offset, count).GetAwaiter().GetResult();

    public override void Write(byte[] buffer, int offset, int count) => WriteAsync(buffer, offset, count).GetAwaiter().GetResult();

    public override void Flush() { }

    public override Task FlushAsync(CancellationToken cancellation) => Task.CompletedTask;

    protected override void Dispose(bool disposing)
    {
        if (!disposed)
        {
            disposed = true;
            outgoing.TryComplete();
            closed.Cancel();
        }
        base.Dispose(disposing);
    }

    public override bool CanRead => true;
    public override bool CanWrite => true;
    public override bool CanSeek => false;
    public override long Length => throw new NotSupportedException();
    public override long Position { get => throw new NotSupportedException(); set => throw new NotSupportedException(); }
    public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();
    public override void SetLength(long value) => throw new NotSupportedException();
}

/// <summary>
/// A stand-in for the background service: answers the app's requests the
/// way the agent does and sends events when told to. While it is down,
/// connecting fails as it does when no pipe exists.
/// </summary>
internal sealed class FakeAgent
{
    private readonly object gate = new();
    private StreamEnd? connection;

    public FakeAgent()
    {
        Status = new Status
        {
            Sharing = true,
            ThisDeviceId = "pc",
            ThisDeviceName = "Test PC",
            ActiveDeviceId = "pc",
            Capturing = true,
        };
        Status.Devices.Add(new DeviceStatus
        {
            Id = "mac", Name = "MacBook Pro", ReportedName = "MacBook Pro", State = ConnectionState.Connected, Paired = true, Os = "macos",
            SpeedPercent = 100,
        });
        Arrangement = new Arrangement { Version = 3 };
        var pc = new ArrangedDevice { Id = "pc", Name = "Test PC", Placed = true, Connected = true, ThisDevice = true };
        pc.Screens.Add(new Screen { Id = "1", Width = 1920, Height = 1080, Scale = 1, Primary = true });
        Arrangement.Devices.Add(pc);
    }

    public bool Up { get; set; } = true;
    public uint IpcVersion { get; set; } = AgentModel.IpcVersion;
    public Status Status { get; set; }
    public Arrangement Arrangement { get; set; }
    public Transfers Transfers { get; set; } = new();
    public PairingState Pairing { get; set; } = new() { Phase = PairingPhase.Idle };

    /// <summary>The drag another computer offers here, to accept or decline; empty for none.</summary>
    public string Offered { get; set; } = "";

    /// <summary>The promises of files on the clipboard the app was handed and holds.</summary>
    public HashSet<ulong> PromisedFiles { get; } = [];

    /// <summary>Promises of files on the clipboard the fake no longer waits for.</summary>
    public HashSet<ulong> GaveUpOn { get; } = [];

    /// <summary>While set, a request to wake a computer is answered only once this completes.</summary>
    public TaskCompletionSource? WakeGate { get; set; }

    /// <summary>No wake-up can be sent, as without a network.</summary>
    public bool WakeUnsent { get; set; }

    /// <summary>Whether the app said the user quit it.</summary>
    public bool UserQuit { get; private set; }
    private readonly List<ClientMessage> received = [];

    /// <summary>Every request so far, oldest first.</summary>
    public IReadOnlyList<ClientMessage> Received
    {
        get
        {
            lock (gate)
            {
                return received.ToList();
            }
        }
    }

    public int Connections { get; private set; }

    public AgentModel Model(TimeProvider? time = null, bool takesClipboardFiles = false) =>
        new(ConnectAsync, "windows-ui/test", time, takesClipboardFiles);

    private Task<Stream> ConnectAsync(CancellationToken cancellation)
    {
        if (!Up)
        {
            throw new TimeoutException("no pipe");
        }
        var (app, agent) = StreamEnd.Pair();
        lock (gate)
        {
            connection = agent;
            Connections++;
        }
        _ = Task.Run(() => ServeAsync(agent));
        return Task.FromResult<Stream>(app);
    }

    /// <summary>Drops the connection, as when the service restarts the agent.</summary>
    public void Disconnect()
    {
        lock (gate)
        {
            connection?.Dispose();
            connection = null;
        }
    }

    public async Task SendAsync(Event e)
    {
        StreamEnd? to;
        lock (gate)
        {
            to = connection;
        }
        if (to is not null)
        {
            await to.WriteAsync(Frame.Encode(new AgentMessage { Event = e }));
        }
    }

    private async Task ServeAsync(StreamEnd agent)
    {
        try
        {
            while (await Frame.ReadAsync(agent, default) is { } body)
            {
                var request = ClientMessage.Parser.ParseFrom(body);
                lock (gate)
                {
                    received.Add(request);
                }
                if (request.CommandCase == ClientMessage.CommandOneofCase.WakeDevice && WakeGate is { } held)
                {
                    await held.Task;
                }
                var response = Answer(request);
                response.RequestId = request.RequestId;
                await agent.WriteAsync(Frame.Encode(new AgentMessage { Response = response }));
                // As the agent does, after it answers: identifying the
                // screens shows them here too.
                // A wake-up that could not be sent leaves the computer not woken, which the status says.
                if (request.CommandCase == ClientMessage.CommandOneofCase.WakeDevice && response.Error?.Code == "not_sent")
                {
                    await agent.WriteAsync(Frame.Encode(new AgentMessage { Event = new Event { StatusChanged = Status } }));
                }
                if (request.CommandCase == ClientMessage.CommandOneofCase.IdentifyScreens && response.ResultCase == Response.ResultOneofCase.Status)
                {
                    await agent.WriteAsync(Frame.Encode(new AgentMessage
                    {
                        Event = new Event { ScreensIdentified = new ScreensIdentified { DurationMs = 2500 } },
                    }));
                }
            }
        }
        catch (Exception e) when (e is IOException or ObjectDisposedException)
        {
            // The app went away.
        }
        // The app closed its end: close this one too, as the agent does.
        agent.Dispose();
    }

    private Response Answer(ClientMessage request)
    {
        switch (request.CommandCase)
        {
            case ClientMessage.CommandOneofCase.Hello:
                return request.Hello.IpcVersion == IpcVersion
                    ? new Response { Hello = new AgentHello { IpcVersion = IpcVersion, AgentVersion = "0.1.0", AgentDigest = "c0ffee" } }
                    : Refuse("ipc_version_mismatch", $"agent speaks IPC version {IpcVersion}");
            case ClientMessage.CommandOneofCase.GetStatus:
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.GetPairing:
                return new Response { Pairing = Pairing };
            case ClientMessage.CommandOneofCase.GetNearby:
                return new Response { Nearby = new NearbyDevices() };
            case ClientMessage.CommandOneofCase.GetArrangement:
                return new Response { Arrangement = Arrangement };
            case ClientMessage.CommandOneofCase.GetTransfers:
                return new Response { Transfers = Transfers };
            case ClientMessage.CommandOneofCase.SetSharing:
                Status = Status.Clone();
                Status.Sharing = request.SetSharing.Enabled;
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.SetLocked:
                Status = Status.Clone();
                Status.Locked = request.SetLocked.Locked;
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.Unpair:
                return Refuse("not_paired", "no such paired device");
            case ClientMessage.CommandOneofCase.SetDeviceName:
                if (request.SetDeviceName.Name.Trim().Length is 0 or > 64)
                {
                    return Refuse("invalid_name", "a device name is 1 to 64 characters");
                }
                Status = Status.Clone();
                Status.ThisDeviceName = request.SetDeviceName.Name.Trim();
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.SetClipboardSync:
                Status = Status.Clone();
                Status.ClipboardSync = request.SetClipboardSync.Enabled;
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.SetPreviewSharing:
                Status = Status.Clone();
                Status.PreviewSharing = request.SetPreviewSharing.Enabled;
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.SetEdgeOptions:
                Status = Status.Clone();
                Status.EdgeDwellMs = request.SetEdgeOptions.DwellMs;
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.SetHotkeys:
                var keys = request.SetHotkeys.Hotkeys ?? new Hotkeys();
                // As the agent: taking back and the lock may be a key typing never uses, alone.
                static bool Alone(string key) => key is "ScrollLock" or "Pause"
                    || (key.Length == 3 && key[0] == 'F' && int.TryParse(key[1..], out var n) && n is >= 13 and <= 19);
                if ((!keys.TakeBack.Contains('+') && !Alone(keys.TakeBack)) || (!keys.Lock.Contains('+') && !Alone(keys.Lock)))
                {
                    return Refuse("needs_modifier", "a hotkey needs a modifier");
                }
                if (keys.HasTakeBackDouble && keys.TakeBackDouble is not ("" or "double+control"))
                {
                    return Refuse("unknown_key", "no such double press");
                }
                var jumps = keys.JumpModifiers.Length == 0
                    ? []
                    : Enumerable.Range(1, 9).Select(n => $"{keys.JumpModifiers}+Digit{n}").ToList();
                if (keys.TakeBack == keys.Lock || jumps.Contains(keys.TakeBack) || jumps.Contains(keys.Lock))
                {
                    return Refuse("duplicate", "two hotkeys are the same keys");
                }
                // As the agent: without one, the relative mode hotkey stays as it is.
                var relative = keys.HasRelative ? keys.Relative : Status.Hotkeys?.Relative ?? "control+alt+shift+KeyG";
                if (relative.Length != 0 && !relative.Contains('+'))
                {
                    return Refuse("needs_modifier", "a hotkey needs a modifier");
                }
                if (relative.Length != 0 && (relative == keys.TakeBack || relative == keys.Lock || jumps.Contains(relative)))
                {
                    return Refuse("duplicate_relative", "the relative mode hotkey is the same keys as another");
                }
                // Without one, the double press stays as it is, or unreported as before.
                var doublePress = keys.HasTakeBackDouble ? keys.TakeBackDouble
                    : Status.Hotkeys is { HasTakeBackDouble: true } current ? current.TakeBackDouble : null;
                Status = Status.Clone();
                Status.Hotkeys = keys.Clone();
                Status.Hotkeys.Relative = relative;
                if (doublePress is null)
                {
                    Status.Hotkeys.ClearTakeBackDouble();
                }
                else
                {
                    Status.Hotkeys.TakeBackDouble = doublePress;
                }
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.SetRelativeAuto:
                Status = Status.Clone();
                Status.RelativeAuto = request.SetRelativeAuto.On;
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.SetSwitchModifier:
                Status = Status.Clone();
                Status.SwitchModifier = request.SetSwitchModifier.Modifier;
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.IdentifyScreens:
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.RenameDevice:
                var rename = request.RenameDevice;
                if (Status.Devices.All(d => d.Id != rename.DeviceId))
                {
                    return Refuse("unknown_device", "no such trusted device");
                }
                if (rename.Name.Trim().Length > 64)
                {
                    return Refuse("invalid_name", "a device name is 1 to 64 characters");
                }
                Status = Status.Clone();
                foreach (var device in Status.Devices.Where(d => d.Id == rename.DeviceId))
                {
                    device.Name = rename.Name.Trim().Length == 0 ? device.ReportedName : rename.Name.Trim();
                }
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.SetDeviceSharing:
                var sharing = request.SetDeviceSharing;
                if (Status.Devices.All(d => d.Id != sharing.DeviceId))
                {
                    return Refuse("unknown_device", "no such trusted device");
                }
                Status = Status.Clone();
                foreach (var device in Status.Devices.Where(d => d.Id == sharing.DeviceId))
                {
                    (device.ClipboardOff, device.FilesOff) = (sharing.ClipboardOff, sharing.FilesOff);
                }
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.SetDeviceSpeed:
                var speed = request.SetDeviceSpeed;
                if (Status.Devices.All(d => d.Id != speed.DeviceId))
                {
                    return Refuse("unknown_device", "no such trusted device");
                }
                if (speed.Percent is < 50 or > 300 || speed.Percent % 10 != 0)
                {
                    return Refuse("invalid_speed", "a cursor speed is 50 to 300 percent, in steps of 10");
                }
                Status = Status.Clone();
                foreach (var device in Status.Devices.Where(d => d.Id == speed.DeviceId))
                {
                    device.SpeedPercent = speed.Percent;
                }
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.WakeDevice:
                var sleeper = Status.Devices.FirstOrDefault(d => d.Id == request.WakeDevice.DeviceId);
                if (sleeper is null)
                {
                    return Refuse("unknown_device", "no such trusted device");
                }
                if (sleeper.State == ConnectionState.Connected)
                {
                    return Refuse("connected", "the device is connected");
                }
                if (!sleeper.CanWake)
                {
                    return Refuse("no_wake_data", "the device never said how to wake it");
                }
                Status = Status.Clone();
                Status.Devices.First(d => d.Id == sleeper.Id).Wake = WakeUnsent ? WakeState.NotWoken : WakeState.Waking;
                return WakeUnsent ? Refuse("not_sent", "no network to send the wake-up on") : new Response { Status = Status };
            case ClientMessage.CommandOneofCase.SetWakeOnPush:
                Status = Status.Clone();
                Status.WakeOnPush = request.SetWakeOnPush.On;
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.SendFiles when Status.Devices.Any(d => d.Id == request.SendFiles.DeviceId && d.FilesOff):
                return Refuse("files_off", "files are turned off with that device");
            case ClientMessage.CommandOneofCase.SetDeviceOptions:
                Status = Status.Clone();
                foreach (var device in Status.Devices.Where(d => d.Id == request.SetDeviceOptions.DeviceId))
                {
                    device.SwapGuiAndControl = request.SetDeviceOptions.SwapGuiAndControl;
                }
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.SetArrangement:
                Arrangement = Arrangement.Clone();
                Arrangement.Version++;
                foreach (var position in request.SetArrangement.Positions)
                {
                    foreach (var device in Arrangement.Devices.Where(d => d.Id == position.DeviceId))
                    {
                        (device.X, device.Y) = (position.X, position.Y);
                    }
                }
                return new Response { Arrangement = Arrangement };
            case ClientMessage.CommandOneofCase.SetFileSettings:
                Transfers = Transfers.Clone();
                Transfers.ReceiveFolder = request.SetFileSettings.ReceiveFolder;
                Transfers.AskBeforeReceiving = request.SetFileSettings.AskBeforeReceiving;
                return new Response { Transfers = Transfers };
            case ClientMessage.CommandOneofCase.SendFiles:
                if (request.SendFiles.Paths.Any(p => p.EndsWith('\\')))
                {
                    return Refuse("not_a_file", "only files can be sent");
                }
                Transfers = Transfers.Clone();
                foreach (var path in request.SendFiles.Paths)
                {
                    // Newest first, as the agent lists them.
                    Transfers.Transfers_.Insert(0, new FileTransfer
                    {
                        TransferId = $"t{Transfers.Transfers_.Count}",
                        Outgoing = true,
                        DeviceId = request.SendFiles.DeviceId,
                        Name = Path.GetFileName(path),
                        State = TransferState.Preparing,
                        Path = path,
                    });
                }
                return new Response { Transfers = Transfers };
            case ClientMessage.CommandOneofCase.RemoveTransfer:
                var removing = Transfers.Transfers_.FirstOrDefault(t => t.TransferId == request.RemoveTransfer.TransferId);
                if (removing is null)
                {
                    return Refuse("not_found", "no such transfer");
                }
                if (!removing.State.IsFinished())
                {
                    return Refuse("not_finished", "the transfer is still going; cancel it first");
                }
                Transfers = Transfers.Clone();
                Transfers.Transfers_.Remove(Transfers.Transfers_.First(t => t.TransferId == removing.TransferId));
                return new Response { Transfers = Transfers };
            case ClientMessage.CommandOneofCase.ClearTransfers:
                var going = Transfers.Transfers_.Where(t => !t.State.IsFinished()).ToList();
                Transfers = Transfers.Clone();
                Transfers.Transfers_.Clear();
                Transfers.Transfers_.AddRange(going);
                return new Response { Transfers = Transfers };
            case ClientMessage.CommandOneofCase.StartPairing:
                Pairing = new PairingState { Phase = PairingPhase.EnterCode, PeerName = "MacBook Pro", AttemptsLeft = 3 };
                return new Response { Pairing = Pairing };
            case ClientMessage.CommandOneofCase.SubmitPairingCode:
                Pairing = request.SubmitPairingCode.Code == "382914"
                    ? new PairingState { Phase = PairingPhase.Paired, PeerName = "MacBook Pro" }
                    : new PairingState { Phase = PairingPhase.EnterCode, PeerName = "MacBook Pro", AttemptsLeft = Pairing.AttemptsLeft - 1, Error = "wrong_code" };
                return new Response { Pairing = Pairing };
            case ClientMessage.CommandOneofCase.CancelPairing:
                Pairing = new PairingState { Phase = PairingPhase.Idle };
                return new Response { Pairing = Pairing };
            case ClientMessage.CommandOneofCase.BeginDrag:
                if (request.BeginDrag.Items.Count == 0)
                {
                    return Refuse("not_dragging", "no button is down");
                }
                Status = Status.Clone();
                Status.Dragging = new Dragging { DragId = "d1", Items = (uint)request.BeginDrag.Items.Count };
                return new Response
                {
                    DragBegun = new DragBegun { DragId = "d1", Items = (uint)request.BeginDrag.Items.Count, Files = 3, Bytes = 3072 },
                };
            case ClientMessage.CommandOneofCase.EndDrag:
                if (Status.Dragging?.DragId != request.EndDrag.DragId)
                {
                    return Refuse("not_found", "no such drag");
                }
                Status = Status.Clone();
                Status.Dragging = null;
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.AcceptDrag:
                if (!Path.IsPathFullyQualified(request.AcceptDrag.Destination))
                {
                    return Refuse("invalid_folder", "not an absolute folder");
                }
                if (request.AcceptDrag.DragId != Offered)
                {
                    return Refuse("not_found", "no such offer");
                }
                Offered = "";
                Transfers = Transfers.Clone();
                Transfers.Transfers_.Insert(0, new FileTransfer
                {
                    TransferId = "t-drag", DeviceId = "mac", Name = "Photos", Folder = true,
                    State = TransferState.WaitingForConnection, DragId = request.AcceptDrag.DragId,
                });
                return new Response { Transfers = Transfers };
            case ClientMessage.CommandOneofCase.DeclineDrag:
                if (request.DeclineDrag.DragId != Offered)
                {
                    return Refuse("not_found", "no such offer");
                }
                Offered = "";
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.ClipboardFilesPlace:
                if (GaveUpOn.Contains(request.ClipboardFilesPlace.PromiseId))
                {
                    return Refuse("too_late", "no longer waited for");
                }
                return PromisedFiles.Contains(request.ClipboardFilesPlace.PromiseId)
                    ? new Response { Status = Status }
                    : Refuse("not_found", "no such promise of clipboard files");
            case ClientMessage.CommandOneofCase.UserQuitApp:
                UserQuit = true;
                return new Response { Status = Status };
            case ClientMessage.CommandOneofCase.ClipboardFilesPlaced:
                return PromisedFiles.Contains(request.ClipboardFilesPlaced.PromiseId)
                    ? new Response { Status = Status }
                    : Refuse("not_found", "no such promise of clipboard files");
            case ClientMessage.CommandOneofCase.PasteClipboardFiles:
                if (!PromisedFiles.Contains(request.PasteClipboardFiles.PromiseId))
                {
                    return Refuse("not_found", "no such promise of clipboard files");
                }
                return Path.IsPathFullyQualified(request.PasteClipboardFiles.Folder)
                    ? new Response { Status = Status }
                    : Refuse("invalid_folder", "not an absolute folder");
            case ClientMessage.CommandOneofCase.ClipboardFilesGone:
                return PromisedFiles.Remove(request.ClipboardFilesGone.PromiseId)
                    ? new Response { Status = Status }
                    : Refuse("not_found", "no such promise of clipboard files");
            case ClientMessage.CommandOneofCase.GetDiagnostics:
                return new Response { Diagnostics = new Diagnostics { Report = "InputWeave diagnostics" } };
            default:
                return Refuse("unknown_command", "not in the fake");
        }
    }

    private static Response Refuse(string code, string detail) =>
        new() { Error = new Error { Code = code, Detail = detail } };
}
