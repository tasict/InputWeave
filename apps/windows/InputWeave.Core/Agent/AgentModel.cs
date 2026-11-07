using System.Collections.Concurrent;
using Inputweave.App.V1;
using InputWeave.Settings;
using InputWeave.Support;

namespace InputWeave.Agent;

/// <summary>A request the background service refused, with its stable code.</summary>
public sealed class AgentRefusal(string code, string detail) : Exception($"{code}: {detail}")
{
    public string Code { get; } = code;
    public string Detail { get; } = detail;
}

/// <summary>The background service could not be reached, or went away.</summary>
public sealed class AgentUnavailableException(string message, Exception? inner = null)
    : IOException(message, inner);

/// <summary>
/// The app's view of the background service: the state it last reported,
/// and the commands the app sends it. Decisions stay in the service; this
/// only mirrors and forwards. It connects over the agent's pipe, says
/// Hello, reads everything once and then follows the service's events, and
/// reconnects whenever the service goes away.
/// </summary>
/// <remarks>
/// Thread-safe. <see cref="Changed"/> is raised on a background thread; the
/// UI reads <see cref="Snapshot"/> on its own thread in response.
/// </remarks>
public sealed class AgentModel : IAsyncDisposable
{
    /// <summary>The IPC version this app speaks, matching IPC_VERSION in iw-proto.</summary>
    public const uint IpcVersion = 1;

    /// <summary>A request with no answer by then counts as a lost connection.</summary>
    public static readonly TimeSpan RequestTimeout = TimeSpan.FromSeconds(10);

    /// <summary>
    /// Reconnecting starts quickly, since the service restarts the agent in
    /// well under a second, and backs off to this.
    /// </summary>
    public static readonly TimeSpan FirstRetry = TimeSpan.FromMilliseconds(250);
    public static readonly TimeSpan LastRetry = TimeSpan.FromSeconds(2);

    private readonly Func<CancellationToken, Task<Stream>> connect;
    private readonly string client;
    private readonly bool takesClipboardFiles;
    private readonly TimeProvider time;
    private readonly object gate = new();
    private readonly ConcurrentDictionary<ulong, TaskCompletionSource<Response>> pending = new();
    private readonly SemaphoreSlim writing = new(1, 1);
    private AgentSnapshot snapshot;
    private Stream? stream;
    private long lastRequestId;
    private CancellationTokenSource? stopping;
    private Task? runner;
    private string? loggedStatus;

    /// <param name="connect">Opens a connection to the service; see <see cref="AgentPipe"/>.</param>
    /// <param name="client">How this client introduces itself, e.g. "windows-ui/0.1.0".</param>
    /// <param name="time">The clock, for tests.</param>
    /// <param name="takesClipboardFiles">
    /// Whether this client puts files copied on another computer on the
    /// clipboard (<see cref="ClipboardFilesPromised"/>), as the app does.
    /// </param>
    public AgentModel(
        Func<CancellationToken, Task<Stream>> connect,
        string client,
        TimeProvider? time = null,
        bool takesClipboardFiles = false)
    {
        this.connect = connect;
        this.client = client;
        this.takesClipboardFiles = takesClipboardFiles;
        this.time = time ?? TimeProvider.System;
        // Out of reach since the app started, until the first connection.
        snapshot = new AgentSnapshot { UnreachableSince = this.time.GetUtcNow() };
    }

    /// <summary>The model for the background service of this Windows session.</summary>
    public static AgentModel ForThisSession(string client, bool takesClipboardFiles = false) =>
        new(AgentPipe.ConnectAsync, client, takesClipboardFiles: takesClipboardFiles);

    /// <summary>Raised on a background thread whenever <see cref="Snapshot"/> changes.</summary>
    public event EventHandler? Changed;

    /// <summary>
    /// Raised on a background thread when files copied on another computer
    /// are to go on the clipboard (only with <c>takesClipboardFiles</c>):
    /// ask with <see cref="ClipboardFilesPlaceAsync"/> before touching the
    /// clipboard, then answer with <see cref="ClipboardFilesPlacedAsync"/>.
    /// </summary>
    public event Action<ClipboardFilesPromised>? ClipboardFilesPromised;

    /// <summary>Raised on a background thread when files pasted from the clipboard arrived, as far as they did.</summary>
    public event Action<ClipboardFilesArrived>? ClipboardFilesArrived;

    /// <summary>
    /// Raised on a background thread each time the cursor crossed an edge
    /// of this computer's screens, to or from another computer:
    /// every crossing is an edge to light, so
    /// none is folded into the next, however close they come.
    /// </summary>
    public event Action<CursorCrossed>? CursorCrossed;

    public AgentSnapshot Snapshot
    {
        get
        {
            lock (gate)
            {
                return snapshot;
            }
        }
    }

    /// <summary>Connects to the service, and reconnects whenever it goes away.</summary>
    public void Start()
    {
        lock (gate)
        {
            if (runner is not null)
            {
                return;
            }
            stopping = new CancellationTokenSource();
            var token = stopping.Token;
            runner = Task.Run(() => RunAsync(token));
        }
    }

    public async ValueTask DisposeAsync()
    {
        CancellationTokenSource? stop;
        Task? run;
        lock (gate)
        {
            stop = stopping;
            run = runner;
        }
        if (stop is null || run is null)
        {
            return;
        }
        await stop.CancelAsync().ConfigureAwait(false);
        CloseStream();
        await run.ConfigureAwait(false);
        stop.Dispose();
    }

    private async Task RunAsync(CancellationToken stop)
    {
        var retry = FirstRetry;
        string? lastFailure = null;
        while (!stop.IsCancellationRequested)
        {
            var session = new Session();
            try
            {
                await SessionAsync(session, stop).ConfigureAwait(false);
            }
            catch (Exception) when (stop.IsCancellationRequested)
            {
                break;
            }
            catch (Exception e)
            {
                // The service is not running, restarting, or refused the
                // Hello. Say so once, not on every retry.
                var failure = Describe(e);
                if (session.Connected)
                {
                    AppLog.Info($"lost the background service: {failure}");
                    lastFailure = null;
                }
                else if (failure != lastFailure)
                {
                    AppLog.Info($"cannot reach the background service: {failure}");
                    lastFailure = failure;
                }
            }
            Disconnected();
            if (session.Connected)
            {
                retry = FirstRetry;
            }
            try
            {
                await Task.Delay(retry, time, stop).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                break;
            }
            retry = TimeSpan.FromTicks(Math.Min(retry.Ticks * 2, LastRetry.Ticks));
        }
        Disconnected();
    }

    private sealed class Session
    {
        public bool Connected;
    }

    private async Task SessionAsync(Session session, CancellationToken stop)
    {
        var connection = await connect(stop).ConfigureAwait(false);
        lock (gate)
        {
            stream = connection;
        }
        // Ends the read below even if closing the stream would not.
        using var ending = CancellationTokenSource.CreateLinkedTokenSource(stop);
        var reading = ReadAsync(connection, ending.Token);
        try
        {
            Response hello;
            try
            {
                hello = await RequestAsync(
                    new ClientMessage
                    {
                        Hello = new ClientHello
                        {
                            IpcVersion = IpcVersion,
                            Client = client,
                            TakesClipboardFiles = takesClipboardFiles,
                        },
                    },
                    stop).ConfigureAwait(false);
            }
            catch (AgentRefusal refusal) when (refusal.Code == "ipc_version_mismatch")
            {
                // Usually an update in progress: the service restarts soon.
                Update(s => s with { Link = AgentLink.Incompatible });
                throw;
            }
            var status = (await RequestAsync(new ClientMessage { GetStatus = new() }, stop).ConfigureAwait(false)).Status;
            var pairing = (await RequestAsync(new ClientMessage { GetPairing = new() }, stop).ConfigureAwait(false)).Pairing;
            var nearby = (await RequestAsync(new ClientMessage { GetNearby = new() }, stop).ConfigureAwait(false)).Nearby;
            var arrangement = (await RequestAsync(new ClientMessage { GetArrangement = new() }, stop).ConfigureAwait(false)).Arrangement;
            var transfers = (await RequestAsync(new ClientMessage { GetTransfers = new() }, stop).ConfigureAwait(false)).Transfers;
            var agentVersion = hello.Hello?.AgentVersion ?? "";
            var agentDigest = hello.Hello?.AgentDigest ?? "";
            AppLog.Info(
                $"connected to the background service {agentVersion} (IPC {hello.Hello?.IpcVersion ?? 0}) as {client}");
            session.Connected = true;
            Update(s => s with
            {
                Link = AgentLink.Connected,
                Session = s.Session + 1,
                UnreachableSince = null,
                Status = status ?? new Status(),
                Pairing = pairing ?? new PairingState(),
                Nearby = nearby?.Devices.ToList() ?? [],
                Arrangement = arrangement ?? new Arrangement(),
                Transfers = transfers ?? new Transfers(),
                AgentVersion = agentVersion,
                AgentDigest = agentDigest,
            });
            // Until the connection ends, which always throws.
            await reading.ConfigureAwait(false);
        }
        finally
        {
            await ending.CancelAsync().ConfigureAwait(false);
            CloseStream(connection);
            try
            {
                await reading.ConfigureAwait(false);
            }
            catch
            {
                // Why the session ended is reported above.
            }
        }
    }

    private async Task ReadAsync(Stream connection, CancellationToken stop)
    {
        try
        {
            while (true)
            {
                var body = await Frame.ReadAsync(connection, stop).ConfigureAwait(false)
                    ?? throw new AgentUnavailableException("the background service closed the connection");
                Handle(AgentMessage.Parser.ParseFrom(body));
            }
        }
        catch (Exception e)
        {
            FailPending(e as AgentUnavailableException ?? new AgentUnavailableException(e.Message, e));
            throw;
        }
    }

    private void Handle(AgentMessage message)
    {
        switch (message.MsgCase)
        {
            case AgentMessage.MsgOneofCase.Response:
                if (pending.TryRemove(message.Response.RequestId, out var waiting))
                {
                    waiting.TrySetResult(message.Response);
                }
                break;
            case AgentMessage.MsgOneofCase.Event:
                var e = message.Event;
                // Outside the event oneof (see app.proto).
                if (e.ClipboardFilesPromised is { } promised)
                {
                    AppLog.Info($"clipboard files {promised.PromiseId} promised from {promised.FromDeviceId}: {promised.Items.Count} items");
                    ClipboardFilesPromised?.Invoke(promised);
                }
                if (e.ClipboardFilesArrived is { } filesArrived)
                {
                    AppLog.Info(
                        $"clipboard files {filesArrived.PromiseId} arrived: {filesArrived.Paths.Count(p => p.Length != 0)} of {filesArrived.Paths.Count}");
                    ClipboardFilesArrived?.Invoke(filesArrived);
                }
                if (e.RelativeUnsupported is { } unsupported)
                {
                    AppLog.Info($"the relative mode hotkey reached {unsupported.DeviceId}, which has no relative mode");
                    Update(s => s with { RelativeRefusal = RelativeRefusal.After(s.RelativeRefusal, unsupported.DeviceId) });
                }
                if (e.CursorCrossed is { } crossed)
                {
                    CursorCrossed?.Invoke(crossed);
                }
                if (e.ScreensIdentified is { } identified)
                {
                    Update(s => s with
                    {
                        ScreenIdentification = ScreenIdentification.After(s.ScreenIdentification, identified.DurationMs),
                    });
                }
                switch (e.EventCase)
                {
                    case Event.EventOneofCase.StatusChanged:
                        Update(s => s with { Status = e.StatusChanged });
                        break;
                    case Event.EventOneofCase.PairingChanged:
                        Update(s => s with { Pairing = e.PairingChanged });
                        break;
                    case Event.EventOneofCase.NearbyChanged:
                        Update(s => s with { Nearby = e.NearbyChanged.Devices.ToList() });
                        break;
                    case Event.EventOneofCase.ArrangementChanged:
                        Update(s => s with { Arrangement = e.ArrangementChanged });
                        break;
                    case Event.EventOneofCase.TransfersChanged:
                        Update(s => s with { Transfers = e.TransfersChanged });
                        break;
                    case Event.EventOneofCase.CursorLocated:
                        var located = e.CursorLocated;
                        AppLog.Info($"cursor located at {located.X:0.#},{located.Y:0.#}");
                        Update(s => s with { CursorSighting = CursorSighting.After(s.CursorSighting, located.X, located.Y) });
                        break;
                    // The drag events sit outside the oneof (app.proto), so
                    // an event with none of its cases may carry one.
                    case Event.EventOneofCase.None when e.DragArrived is { } arrived:
                        AppLog.Info(
                            $"drag {arrived.DragId} arrived from {arrived.FromDeviceId}: {arrived.Items.Count} items, {arrived.Files} files, {arrived.Bytes} bytes");
                        Update(s => s with { DragOffer = DragOffer.After(s.DragOffer, arrived) });
                        break;
                    case Event.EventOneofCase.None when e.DragEnded is { } ended:
                        AppLog.Info($"drag {ended.DragId} ended: {ended.Reason} {ended.DeviceId}");
                        Update(s => s with { DragFinish = DragFinish.After(s.DragFinish, ended) });
                        break;
                }
                break;
        }
    }

    /// <summary>
    /// Sends a command and waits for its result. A refusal throws
    /// <see cref="AgentRefusal"/>; no connection, or none any more, throws
    /// <see cref="AgentUnavailableException"/>.
    /// </summary>
    public async Task<Response> RequestAsync(ClientMessage message, CancellationToken cancellation = default)
    {
        Stream connection;
        lock (gate)
        {
            connection = stream ?? throw new AgentUnavailableException("not connected to the background service");
        }
        ulong id = (ulong)Interlocked.Increment(ref lastRequestId);
        message.RequestId = id;
        var answer = new TaskCompletionSource<Response>(TaskCreationOptions.RunContinuationsAsynchronously);
        pending[id] = answer;
        Response response;
        try
        {
            var frame = Frame.Encode(message);
            await writing.WaitAsync(cancellation).ConfigureAwait(false);
            try
            {
                await connection.WriteAsync(frame, cancellation).ConfigureAwait(false);
                await connection.FlushAsync(cancellation).ConfigureAwait(false);
            }
            finally
            {
                writing.Release();
            }
            response = await answer.Task.WaitAsync(RequestTimeout, time, cancellation).ConfigureAwait(false);
        }
        catch (TimeoutException e)
        {
            throw new AgentUnavailableException("the background service did not answer", e);
        }
        catch (Exception e) when (e is (IOException or ObjectDisposedException) and not AgentUnavailableException)
        {
            throw new AgentUnavailableException(e.Message, e);
        }
        finally
        {
            pending.TryRemove(id, out _);
        }
        return response.ResultCase switch
        {
            Response.ResultOneofCase.Error => throw new AgentRefusal(response.Error.Code, response.Error.Detail),
            Response.ResultOneofCase.None => throw new AgentRefusal("unexpected", "empty response"),
            _ => response,
        };
    }

    // Commands

    public async Task SetSharingAsync(bool enabled) =>
        Apply(await RequestAsync(new ClientMessage { SetSharing = new SetSharing { Enabled = enabled } })
            .ConfigureAwait(false));

    /// <summary>
    /// Locks the cursor to the computer it is on, so it crosses no edge, or
    /// unlocks it. The lock hotkey toggles the same lock.
    /// </summary>
    public async Task SetLockedAsync(bool locked) =>
        Apply(await RequestAsync(new ClientMessage { SetLocked = new SetLocked { Locked = locked } })
            .ConfigureAwait(false));

    /// <summary>
    /// Moves the cursor to the middle of a device's main screen. A refusal's
    /// code says why it cannot go there.
    /// </summary>
    public async Task JumpToAsync(string deviceId) =>
        Apply(await RequestAsync(new ClientMessage { JumpTo = new JumpTo { DeviceId = deviceId } })
            .ConfigureAwait(false));

    /// <summary>Pairs with a computer found nearby; its code is then typed here.</summary>
    public async Task StartPairingAsync(string deviceId) =>
        Apply(await RequestAsync(new ClientMessage { StartPairing = new StartPairing { DeviceId = deviceId } })
            .ConfigureAwait(false));

    /// <summary>Pairs with the computer at "host" or "host:port".</summary>
    public async Task StartPairingByAddressAsync(string address) =>
        Apply(await RequestAsync(new ClientMessage { StartPairing = new StartPairing { Address = address } })
            .ConfigureAwait(false));

    /// <summary>Tries a code; the result is also in the snapshot's pairing state.</summary>
    public async Task<PairingState> SubmitPairingCodeAsync(string code)
    {
        var response = await RequestAsync(
            new ClientMessage { SubmitPairingCode = new SubmitPairingCode { Code = code } }).ConfigureAwait(false);
        Apply(response);
        return response.Pairing ?? throw new AgentRefusal("unexpected", "no pairing state");
    }

    /// <summary>Cancels the pairing started here, or declines the one shown here.</summary>
    public async Task CancelPairingAsync() =>
        Apply(await RequestAsync(new ClientMessage { CancelPairing = new CancelPairing() }).ConfigureAwait(false));

    /// <summary>Enter License Key: the service starts the key over as waiting for activation (unless it is licensed with it already).</summary>
    public async Task SetLicenseKeyAsync(string key) =>
        Apply(await RequestAsync(new ClientMessage { SetLicenseKey = new SetLicenseKey { Key = key } })
            .ConfigureAwait(false));

    /// <summary>What Polar answered about the license, for the service to keep.</summary>
    public async Task RecordLicenseResultAsync(RecordLicenseResult result) =>
        Apply(await RequestAsync(new ClientMessage { RecordLicenseResult = result }).ConfigureAwait(false));

    /// <summary>Remove License, after the app freed the seat with Polar (or could not).</summary>
    public async Task RemoveLicenseAsync() =>
        Apply(await RequestAsync(new ClientMessage { RemoveLicense = new RemoveLicense() }).ConfigureAwait(false));

    /// <summary>Forgets a paired computer.</summary>
    public async Task UnpairAsync(string deviceId) =>
        Apply(await RequestAsync(new ClientMessage { Unpair = new Unpair { DeviceId = deviceId } })
            .ConfigureAwait(false));

    /// <summary>Renames this computer; the service refuses names that are empty or over 64 characters.</summary>
    public async Task RenameAsync(string name) =>
        Apply(await RequestAsync(new ClientMessage { SetDeviceName = new SetDeviceName { Name = name } })
            .ConfigureAwait(false));

    public async Task SetClipboardSyncAsync(bool enabled) =>
        Apply(await RequestAsync(new ClientMessage { SetClipboardSync = new SetClipboardSync { Enabled = enabled } })
            .ConfigureAwait(false));

    /// <summary>
    /// Shares previews of this computer's desktop pictures with paired
    /// computers, or stops; turned off, they drop the ones they have.
    /// </summary>
    public async Task SetPreviewSharingAsync(bool enabled) =>
        Apply(await RequestAsync(new ClientMessage { SetPreviewSharing = new SetPreviewSharing { Enabled = enabled } })
            .ConfigureAwait(false));

    /// <summary>How long the cursor rests at an edge before it crosses; 0 crosses at once.</summary>
    public async Task SetEdgeDwellAsync(uint milliseconds) =>
        Apply(await RequestAsync(new ClientMessage { SetEdgeOptions = new SetEdgeOptions { DwellMs = milliseconds } })
            .ConfigureAwait(false));

    /// <summary>
    /// Sets every hotkey at once, as the service writes them (<c>Hotkeys</c>
    /// in app.proto). A refusal's code says what is wrong:
    /// "needs_modifier", "unknown_key" or "duplicate".
    /// </summary>
    public async Task SetHotkeysAsync(Hotkeys hotkeys) =>
        Apply(await RequestAsync(new ClientMessage { SetHotkeys = new SetHotkeys { Hotkeys = hotkeys } })
            .ConfigureAwait(false));

    /// <summary>
    /// Whether relative mode turns on by itself while an app on the
    /// computer the cursor is on holds the pointer, and off when it lets go.
    /// </summary>
    public async Task SetRelativeAutoAsync(bool on) =>
        Apply(await RequestAsync(new ClientMessage { SetRelativeAuto = new SetRelativeAuto { On = on } })
            .ConfigureAwait(false));

    /// <summary>
    /// Makes the cursor cross to another computer only while
    /// <paramref name="modifier"/> is held, or (<see cref="SwitchModifier.None"/>)
    /// whenever it reaches the edge.
    /// </summary>
    public async Task SetSwitchModifierAsync(SwitchModifier modifier) =>
        Apply(await RequestAsync(new ClientMessage { SetSwitchModifier = new SetSwitchModifier { Modifier = modifier } })
            .ConfigureAwait(false));

    public async Task SetSwapGuiAndControlAsync(string deviceId, bool swap) =>
        Apply(await RequestAsync(new ClientMessage
        {
            SetDeviceOptions = new SetDeviceOptions { DeviceId = deviceId, SwapGuiAndControl = swap },
        }).ConfigureAwait(false));

    /// <summary>Has this computer and every connected one show their names on their screens for a moment.</summary>
    public async Task IdentifyScreensAsync() =>
        Apply(await RequestAsync(new ClientMessage { IdentifyScreens = new IdentifyScreens() })
            .ConfigureAwait(false));

    /// <summary>
    /// Names another computer on this one only; an empty name shows the name
    /// it gives itself again. Refused with "invalid_name" and "unknown_device".
    /// </summary>
    public async Task RenameDeviceAsync(string deviceId, string name) =>
        Apply(await RequestAsync(new ClientMessage
        {
            RenameDevice = new RenameDevice { DeviceId = deviceId, Name = name },
        }).ConfigureAwait(false));

    /// <summary>
    /// Stops sharing the clipboard or files with another computer, or shares
    /// them again; only this computer stops, the other is not told.
    /// </summary>
    public async Task SetDeviceSharingAsync(string deviceId, bool clipboardOff, bool filesOff) =>
        Apply(await RequestAsync(new ClientMessage
        {
            SetDeviceSharing = new SetDeviceSharing { DeviceId = deviceId, ClipboardOff = clipboardOff, FilesOff = filesOff },
        }).ConfigureAwait(false));

    /// <summary>
    /// Makes the cursor move faster or slower on another computer while this
    /// one controls it, in percent of how the mouse moves it here; only this
    /// computer uses it. Refused with "invalid_speed" and "unknown_device".
    /// </summary>
    public async Task SetDeviceSpeedAsync(string deviceId, int percent) =>
        Apply(await RequestAsync(new ClientMessage
        {
            SetDeviceSpeed = new SetDeviceSpeed { DeviceId = deviceId, Percent = (uint)Math.Max(percent, 0) },
        }).ConfigureAwait(false));

    /// <summary>
    /// Wakes another computer from sleep over the network, as it told this
    /// one how to (DeviceStatus.can_wake); answered with the status once the
    /// wake-up is sent, which says it is being woken. Until the answer comes
    /// the computer is in <see cref="AgentSnapshot.WakesSent"/>. Refused
    /// with "unknown_device", "connected", "no_wake_data" and "not_sent"
    /// (it then did not wake up).
    /// </summary>
    public async Task WakeDeviceAsync(string deviceId)
    {
        Update(s => s with { WakesSent = s.WakesSent.Add(deviceId) });
        try
        {
            Apply(await RequestAsync(new ClientMessage { WakeDevice = new WakeDevice { DeviceId = deviceId } })
                .ConfigureAwait(false));
        }
        finally
        {
            Update(s => s with { WakesSent = s.WakesSent.Remove(deviceId) });
        }
    }

    /// <summary>
    /// Whether the cursor pushed against an edge toward a sleeping computer
    /// that can be woken wakes it, at most every 30 seconds.
    /// </summary>
    public async Task SetWakeOnPushAsync(bool on) =>
        Apply(await RequestAsync(new ClientMessage { SetWakeOnPush = new SetWakeOnPush { On = on } })
            .ConfigureAwait(false));

    /// <summary>
    /// Takes a computer offline for now, this one or another this one
    /// trusts, or brings it back: none of the group controls it or is
    /// controlled by it, and the clipboard and files go neither to it nor
    /// from it, until someone brings it back. Answered with the status once
    /// the service has the change; doing it again changes nothing. Refused
    /// with "unknown_device", and with "offline_for_now" for another
    /// computer while this one is offline for now (it can only bring itself
    /// back).
    /// </summary>
    public async Task SetOfflineForNowAsync(string deviceId, bool offline) =>
        Apply(await RequestAsync(new ClientMessage
        {
            SetOfflineForNow = new SetOfflineForNow { DeviceId = deviceId, Offline = offline },
        }).ConfigureAwait(false));

    /// <summary>
    /// Adds a profile named <paramref name="name"/> (the service makes the
    /// name unique with " 2", " 3"…), sharing, with every paired computer,
    /// the clipboard and files on, and no networks. Returns its id: the
    /// profile the status the service answers with lists that it did not
    /// before; null if none. Refused with "invalid_name" and
    /// "too_many_profiles".
    /// </summary>
    public async Task<string?> AddProfileAsync(string name)
    {
        var before = Snapshot.Status.Profiles.Select(p => p.Id).ToHashSet();
        var response = await RequestAsync(new ClientMessage { AddProfile = new AddProfile { Name = name } })
            .ConfigureAwait(false);
        Apply(response);
        return response.Status is { } status ? Profiles.Added(before, status) : null;
    }

    /// <summary>
    /// Changes what <paramref name="edit"/> sets of a profile: its name,
    /// pausing it, the clipboard, files, or which computers it connects.
    /// Refused with "unknown_profile", "fixed_profile", "invalid_name" and
    /// "unknown_device".
    /// </summary>
    public async Task EditProfileAsync(EditProfile edit) =>
        Apply(await RequestAsync(new ClientMessage { EditProfile = edit }).ConfigureAwait(false));

    /// <summary>
    /// Removes a profile of the user's own, with its networks; when it was
    /// the one chosen, the choice goes back to Automatic. Refused with
    /// "unknown_profile" and "fixed_profile".
    /// </summary>
    public async Task RemoveProfileAsync(string profileId) =>
        Apply(await RequestAsync(new ClientMessage { RemoveProfile = new RemoveProfile { ProfileId = profileId } })
            .ConfigureAwait(false));

    /// <summary>Uses a profile whatever the network, or Automatic again with an empty id.</summary>
    public async Task SelectProfileAsync(string profileId) =>
        Apply(await RequestAsync(new ClientMessage { SelectProfile = new SelectProfile { ProfileId = profileId } })
            .ConfigureAwait(false));

    /// <summary>
    /// Adds the network this computer is on now to a profile, taking it from
    /// any other. Refused with "unknown_profile", "other_networks",
    /// "no_network" and "too_many_networks".
    /// </summary>
    public async Task AddNetworkToProfileAsync(string profileId) =>
        Apply(await RequestAsync(new ClientMessage
        {
            AddNetworkToProfile = new AddNetworkToProfile { ProfileId = profileId },
        }).ConfigureAwait(false));

    /// <summary>Takes a network out of the profile that lists it. Refused with "unknown_network".</summary>
    public async Task RemoveNetworkAsync(string networkId) =>
        Apply(await RequestAsync(new ClientMessage { RemoveNetwork = new RemoveNetwork { NetworkId = networkId } })
            .ConfigureAwait(false));

    /// <summary>Also recognizes networks by their Wi-Fi name, which the app then reports (<see cref="ReportWifiNameAsync"/>), or only by their router.</summary>
    public async Task SetWifiNamesAsync(bool on) =>
        Apply(await RequestAsync(new ClientMessage { SetWifiNames = new SetWifiNames { On = on } })
            .ConfigureAwait(false));

    /// <summary>
    /// Tells the service the Wi-Fi name of the network this computer is on,
    /// as the status named it (empty when not on Wi-Fi, or unknown). Taken
    /// only while the setting is on and the network is still that one.
    /// </summary>
    public async Task ReportWifiNameAsync(ReportWifiName report) =>
        Apply(await RequestAsync(new ClientMessage { ReportWifiName = report }).ConfigureAwait(false));

    /// <summary>
    /// Moves computers' desktop origins on the shared canvas. The service
    /// refuses positions that make screens overlap, as a whole.
    /// </summary>
    public async Task MoveAsync(IEnumerable<(string Id, double X, double Y)> positions)
    {
        var set = new SetArrangement();
        set.Positions.AddRange(positions.Select(p => new DevicePosition { DeviceId = p.Id, X = p.X, Y = p.Y }));
        Apply(await RequestAsync(new ClientMessage { SetArrangement = set }).ConfigureAwait(false));
    }

    /// <summary>Sends files to a computer, each as its own transfer.</summary>
    public async Task SendFilesAsync(string deviceId, IEnumerable<string> paths)
    {
        var send = new SendFiles { DeviceId = deviceId };
        send.Paths.AddRange(paths);
        Apply(await RequestAsync(new ClientMessage { SendFiles = send }).ConfigureAwait(false));
    }

    public async Task CancelTransferAsync(string transferId) =>
        Apply(await RequestAsync(new ClientMessage { CancelTransfer = new CancelTransfer { TransferId = transferId } })
            .ConfigureAwait(false));

    /// <summary>
    /// Takes a finished transfer off the history; its file stays where it
    /// is. Refused with "not_finished" while it still goes (cancel it
    /// first) and "not_found" once it is not listed.
    /// </summary>
    public async Task RemoveTransferAsync(string transferId) =>
        Apply(await RequestAsync(new ClientMessage { RemoveTransfer = new RemoveTransfer { TransferId = transferId } })
            .ConfigureAwait(false));

    /// <summary>Takes every finished transfer off the history; the files stay, and transfers still going stay listed.</summary>
    public async Task ClearTransfersAsync() =>
        Apply(await RequestAsync(new ClientMessage { ClearTransfers = new ClearTransfers() }).ConfigureAwait(false));

    /// <summary>
    /// Files are being dragged from here and entered the strip along an edge
    /// the cursor can cross (docs/architecture.md §6.4, point 7): the service
    /// lists them and lets the cursor cross with the button held. Refused
    /// like <see cref="SendFilesAsync"/>, and with "not_dragging",
    /// "controlled" or "unavailable".
    /// </summary>
    public async Task<DragBegun> BeginDragAsync(IEnumerable<(string Path, bool IsDirectory)> items)
    {
        var begin = new BeginDrag();
        begin.Items.AddRange(items.Select(i => new DragPath { Path = i.Path, IsDir = i.IsDirectory }));
        var response = await RequestAsync(new ClientMessage { BeginDrag = begin }).ConfigureAwait(false);
        return response.DragBegun ?? throw new AgentRefusal("unexpected", "no drag");
    }

    /// <summary>The drag that began here ended without crossing to another computer.</summary>
    public async Task EndDragAsync(string dragId) =>
        Apply(await RequestAsync(new ClientMessage { EndDrag = new EndDrag { DragId = dragId } }).ConfigureAwait(false));

    /// <summary>
    /// The drag another computer offered here was dropped: its files go into
    /// <paramref name="destination"/>, a folder the user can write to.
    /// </summary>
    public async Task AcceptDragAsync(string dragId, string destination) =>
        Apply(await RequestAsync(new ClientMessage
        {
            AcceptDrag = new AcceptDrag { DragId = dragId, Destination = destination },
        }).ConfigureAwait(false));

    /// <summary>
    /// Asks whether the files of promise <paramref name="promiseId"/> may
    /// still go on the clipboard; refused (<see cref="AgentRefusal"/>) with
    /// <c>too_late</c> or <c>superseded</c> when they may not.
    /// </summary>
    public async Task ClipboardFilesPlaceAsync(ulong promiseId) =>
        Apply(await RequestAsync(new ClientMessage
        {
            ClipboardFilesPlace = new ClipboardFilesPlace { PromiseId = promiseId },
        }).ConfigureAwait(false));

    /// <summary>Says whether the files of promise <paramref name="promiseId"/> are on the clipboard now.</summary>
    public async Task ClipboardFilesPlacedAsync(ulong promiseId, bool ok) =>
        Apply(await RequestAsync(new ClientMessage
        {
            ClipboardFilesPlaced = new ClipboardFilesPlaced { PromiseId = promiseId, Ok = ok },
        }).ConfigureAwait(false));

    /// <summary>
    /// Has the files of promise <paramref name="promiseId"/>, which an app
    /// pastes, fetched into <paramref name="folder"/>, a folder of the
    /// user's; <see cref="ClipboardFilesArrived"/> tells when they are there.
    /// </summary>
    public async Task PasteClipboardFilesAsync(ulong promiseId, string folder) =>
        Apply(await RequestAsync(new ClientMessage
        {
            PasteClipboardFiles = new PasteClipboardFiles { PromiseId = promiseId, Folder = folder },
        }).ConfigureAwait(false));

    /// <summary>Says the clipboard no longer holds the files of promise <paramref name="promiseId"/>.</summary>
    public async Task ClipboardFilesGoneAsync(ulong promiseId) =>
        Apply(await RequestAsync(new ClientMessage
        {
            ClipboardFilesGone = new ClipboardFilesGone { PromiseId = promiseId },
        }).ConfigureAwait(false));

    /// <summary>
    /// Says the user quit the app, as it goes, so the background service
    /// does not start it again for files on the clipboard until it connects
    /// again.
    /// </summary>
    public async Task UserQuitAppAsync() =>
        Apply(await RequestAsync(new ClientMessage { UserQuitApp = new UserQuitApp() }).ConfigureAwait(false));

    /// <summary>The drag another computer offered here was not dropped here.</summary>
    public async Task DeclineDragAsync(string dragId) =>
        Apply(await RequestAsync(new ClientMessage { DeclineDrag = new DeclineDrag { DragId = dragId } }).ConfigureAwait(false));

    /// <summary>Accepts or declines a file offered to this computer.</summary>
    public async Task AnswerFileOfferAsync(string transferId, bool accept) =>
        Apply(await RequestAsync(new ClientMessage
        {
            AnswerFileOffer = new AnswerFileOffer { TransferId = transferId, Accept = accept },
        }).ConfigureAwait(false));

    /// <summary>Where received files go (empty for the default), and whether to ask first.</summary>
    public async Task SetFileSettingsAsync(string folder, bool askBeforeReceiving) =>
        Apply(await RequestAsync(new ClientMessage
        {
            SetFileSettings = new SetFileSettings { ReceiveFolder = folder, AskBeforeReceiving = askBeforeReceiving },
        }).ConfigureAwait(false));

    /// <summary>
    /// The service's report for bug reports: versions, state, settings and
    /// the end of its logs, without keys, file names or clipboard contents.
    /// </summary>
    public async Task<string> DiagnosticsAsync()
    {
        var response = await RequestAsync(new ClientMessage { GetDiagnostics = new GetDiagnostics() })
            .ConfigureAwait(false);
        return response.Diagnostics?.Report ?? throw new AgentRefusal("unexpected", "no diagnostics");
    }

    /// <summary>Takes what a command answers with, ahead of its event.</summary>
    private void Apply(Response response)
    {
        switch (response.ResultCase)
        {
            case Response.ResultOneofCase.Status:
                Update(s => s with { Status = response.Status });
                break;
            case Response.ResultOneofCase.Arrangement:
                Update(s => s with { Arrangement = response.Arrangement });
                break;
            case Response.ResultOneofCase.Transfers:
                Update(s => s with { Transfers = response.Transfers });
                break;
            case Response.ResultOneofCase.Pairing:
                Update(s => s with { Pairing = response.Pairing });
                break;
        }
    }

    private void Update(Func<AgentSnapshot, AgentSnapshot> change)
    {
        AgentSnapshot before, after;
        lock (gate)
        {
            before = snapshot;
            after = change(before);
            // Turning waiting at the edge back on uses the last time in effect.
            if (after.Status.EdgeDwellMs != 0 && after.Status.EdgeDwellMs != after.LastSwitchDelayMs)
            {
                after = after with { LastSwitchDelayMs = after.Status.EdgeDwellMs };
            }
            // A computer that left the status is not being asked to wake any more.
            if (Wake.Kept(after.WakesSent, after.Status) is var kept && !ReferenceEquals(kept, after.WakesSent))
            {
                after = after with { WakesSent = kept };
            }
            snapshot = after;
        }
        if (after.Equals(before))
        {
            return;
        }
        if (after.Link != before.Link)
        {
            AppLog.Info($"link: {after.Link}");
        }
        LogStatus(after);
        Changed?.Invoke(this, EventArgs.Empty);
    }

    /// <summary>Logs what the panel shows, when that changes; not the round-trip times.</summary>
    private void LogStatus(AgentSnapshot current)
    {
        if (current.Link != AgentLink.Connected)
        {
            return;
        }
        var status = current.Status;
        var devices = string.Join(", ", status.Devices.Select(d => $"{d.Name} {d.State}{(d.NotInProfile ? " not in profile" : "")}"));
        var line = $"status: sharing={status.Sharing}, locked={status.Locked}, capturing={status.Capturing}, "
            + $"this={status.ThisDeviceName}, cursor on {current.NameOf(current.CursorDeviceId)}, "
            + $"controlled by {(current.ControlledFromElsewhere ? current.NameOf(status.ControlledByDeviceId) : "none")}, "
            + $"devices: [{devices}], arranged: [{Arranged(current.Arrangement)}]"
            // The profile by its id: a network's name may be a Wi-Fi name, which is never logged.
            + (current.ProfilesSupported
                ? $", profile {status.ProfileId}{(status.ProfileChosen ? " (chosen)" : "")}{(Profiles.Paused(current) ? " paused" : "")}"
                : "")
            + (status.Dragging is { } dragging
                ? $", dragging {dragging.DragId} ({dragging.Items} items) {(dragging.DeviceId.Length == 0 ? "here" : $"to {current.NameOf(dragging.DeviceId)}")}"
                : "");
        lock (gate)
        {
            if (line == loggedStatus)
            {
                return;
            }
            loggedStatus = line;
        }
        AppLog.Info(line);
    }

    /// <summary>Where each computer is and its screens' sizes, for the log.</summary>
    private static string Arranged(Arrangement arrangement) =>
        string.Join("; ", arrangement.Devices.Select(d =>
            $"{d.Name} {(d.Placed ? $"at {d.X:0},{d.Y:0}" : "unplaced")} "
            + string.Join(" ", d.Screens.Select(s => $"{s.Width:0}x{s.Height:0}@{s.X:0},{s.Y:0}"))));

    private void Disconnected()
    {
        CloseStream();
        FailPending(new AgentUnavailableException("not connected to the background service"));
        var now = time.GetUtcNow();
        Update(s => s with
        {
            Link = s.Link == AgentLink.Connected ? AgentLink.Connecting : s.Link,
            UnreachableSince = s.UnreachableSince ?? now,
        });
    }

    /// <summary>Closes the current connection, or only <paramref name="only"/> if it is still the current one.</summary>
    private void CloseStream(Stream? only = null)
    {
        Stream? closing;
        lock (gate)
        {
            closing = only ?? stream;
            if (closing is not null && ReferenceEquals(closing, stream))
            {
                stream = null;
            }
        }
        closing?.Dispose();
    }

    private void FailPending(Exception reason)
    {
        foreach (var id in pending.Keys)
        {
            if (pending.TryRemove(id, out var waiting))
            {
                waiting.TrySetException(reason);
            }
        }
    }

    private static string Describe(Exception e) => e switch
    {
        AgentRefusal refusal => $"refused: {refusal.Code}",
        TimeoutException => "nothing is listening on the pipe",
        _ => $"{e.GetType().Name}: {e.Message}",
    };
}
