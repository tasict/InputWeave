using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Settings;
using InputWeave.Support;
using Xunit;

namespace InputWeave.Tests;

/// <summary>The IPC client against a stand-in for the background service.</summary>
public class AgentModelTests
{
    private static async Task Eventually(string what, Func<bool> condition)
    {
        var deadline = DateTime.UtcNow.AddSeconds(10);
        while (!condition())
        {
            if (DateTime.UtcNow > deadline)
            {
                Assert.Fail($"timed out waiting for {what}");
            }
            await Task.Delay(20);
        }
    }

    [Fact]
    public async Task SaysHelloThenMirrorsTheServicesState()
    {
        var agent = new FakeAgent();
        await using var model = agent.Model();
        var changes = 0;
        model.Changed += (_, _) => Interlocked.Increment(ref changes);
        model.Start();

        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);
        var snapshot = model.Snapshot;
        Assert.Null(snapshot.UnreachableSince);
        Assert.Equal("0.1.0", snapshot.AgentVersion);
        Assert.Equal("Test PC", snapshot.Status.ThisDeviceName);
        Assert.Equal(1920, snapshot.Arrangement.Devices[0].Screens[0].Width);
        Assert.True(changes > 0);

        // Hello first, then everything once, as the macOS app does.
        var hello = agent.Received[0];
        Assert.Equal(ClientMessage.CommandOneofCase.Hello, hello.CommandCase);
        Assert.Equal(AgentModel.IpcVersion, hello.Hello.IpcVersion);
        Assert.Equal("windows-ui/test", hello.Hello.Client);
        Assert.Equal(
            [
                ClientMessage.CommandOneofCase.GetStatus, ClientMessage.CommandOneofCase.GetPairing,
                ClientMessage.CommandOneofCase.GetNearby, ClientMessage.CommandOneofCase.GetArrangement,
                ClientMessage.CommandOneofCase.GetTransfers,
            ],
            agent.Received.Skip(1).Take(5).Select(m => m.CommandCase));
        // Every request has its own id.
        Assert.Equal(agent.Received.Count, agent.Received.Select(m => m.RequestId).Distinct().Count());
    }

    [Fact]
    public async Task FollowsTheServicesEvents()
    {
        var agent = new FakeAgent();
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);

        var status = agent.Status.Clone();
        status.ActiveDeviceId = "mac";
        await agent.SendAsync(new Event { StatusChanged = status });
        await Eventually("the cursor on the Mac", () => model.Snapshot.ControllingOther);

        var arrangement = agent.Arrangement.Clone();
        arrangement.Version = 4;
        await agent.SendAsync(new Event { ArrangementChanged = arrangement });
        await Eventually("the new arrangement", () => model.Snapshot.Arrangement.Version == 4);
    }

    [Fact]
    public async Task CountsEachTimeTheServiceShowsWhereTheCursorIs()
    {
        var agent = new FakeAgent();
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);
        Assert.Null(model.Snapshot.CursorSighting);

        await agent.SendAsync(new Event { CursorLocated = new CursorLocated { X = 960, Y = 540 } });
        await Eventually("a sighting", () => model.Snapshot.CursorSighting is not null);
        Assert.Equal(new CursorSighting(1, 960, 540), model.Snapshot.CursorSighting);

        // The same place again is a new sighting all the same.
        var before = model.Snapshot;
        await agent.SendAsync(new Event { CursorLocated = new CursorLocated { X = 960, Y = 540 } });
        await Eventually("a second sighting", () => model.Snapshot.CursorSighting?.Serial == 2);
        Assert.NotSame(before, model.Snapshot);
    }

    [Fact]
    public async Task SendsCommandsAndReportsRefusals()
    {
        var agent = new FakeAgent();
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);

        await model.SetSharingAsync(false);
        Assert.False(model.Snapshot.Status.Sharing);
        await model.SetLockedAsync(true);
        Assert.True(model.Snapshot.Status.Locked);

        var refusal = await Assert.ThrowsAsync<AgentRefusal>(
            () => model.RequestAsync(new ClientMessage { Unpair = new Unpair { DeviceId = "ghost" } }));
        Assert.Equal("not_paired", refusal.Code);
    }

    [Fact]
    public async Task TakesWhatCommandsAnswerAheadOfTheirEvents()
    {
        var agent = new FakeAgent();
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);
        Assert.Equal("c0ffee", model.Snapshot.AgentDigest);

        await model.MoveAsync([("pc", -1920, 0)]);
        Assert.Equal(-1920, model.Snapshot.Arranged("pc")!.X);
        Assert.Equal(4ul, model.Snapshot.Arrangement.Version);
        var move = agent.Received[^1].SetArrangement.Positions.Single();
        Assert.Equal(("pc", -1920.0, 0.0), (move.DeviceId, move.X, move.Y));

        await model.SetFileSettingsAsync(@"C:\Users\me\Downloads\InputWeave", askBeforeReceiving: true);
        Assert.True(model.Snapshot.Transfers.AskBeforeReceiving);
        await model.SendFilesAsync("mac", [@"C:\Users\me\Report.pdf"]);
        Assert.Equal("Report.pdf", model.Snapshot.Transfers.Transfers_.Single().Name);
        var notAFile = await Assert.ThrowsAsync<AgentRefusal>(() => model.SendFilesAsync("mac", [@"C:\Users\me\"]));
        Assert.Equal("not_a_file", notAFile.Code);

        await model.RenameAsync("  Desk PC ");
        Assert.Equal("Desk PC", model.Snapshot.Status.ThisDeviceName);
        Assert.Equal("invalid_name", (await Assert.ThrowsAsync<AgentRefusal>(() => model.RenameAsync(" "))).Code);

        await model.SetClipboardSyncAsync(true);
        Assert.True(model.Snapshot.Status.ClipboardSync);
        await model.SetPreviewSharingAsync(true);
        Assert.True(model.Snapshot.Status.PreviewSharing);
        await model.SetSwapGuiAndControlAsync("mac", true);
        Assert.True(model.Snapshot.Device("mac")!.SwapGuiAndControl);
        Assert.Equal("InputWeave diagnostics", await model.DiagnosticsAsync());
    }

    [Fact]
    public async Task ChangesTheHotkeysAndHowToSwitch()
    {
        var agent = new FakeAgent();
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);
        // A service that does not report its hotkeys uses the defaults.
        Assert.Null(model.Snapshot.Status.Hotkeys);
        Assert.Equal("Ctrl + Alt + Shift + Esc", model.Snapshot.TakeBackShortcut);

        var hotkeys = ShortcutText.With(Shortcut.Defaults, ShortcutRow.Lock, "control+alt+KeyK");
        await model.SetHotkeysAsync(hotkeys);
        Assert.Equal(hotkeys, agent.Received[^1].SetHotkeys.Hotkeys);
        Assert.Equal("Ctrl + Alt + K", model.Snapshot.LockShortcut);

        var noModifier = ShortcutText.With(hotkeys, ShortcutRow.Lock, "KeyK");
        Assert.Equal("needs_modifier", (await Assert.ThrowsAsync<AgentRefusal>(() => model.SetHotkeysAsync(noModifier))).Code);
        var same = ShortcutText.With(hotkeys, ShortcutRow.Lock, hotkeys.TakeBack);
        Assert.Equal("duplicate", (await Assert.ThrowsAsync<AgentRefusal>(() => model.SetHotkeysAsync(same))).Code);
        // Refused, nothing changed.
        Assert.Equal("Ctrl + Alt + K", model.Snapshot.LockShortcut);

        await model.SetSwitchModifierAsync(SwitchModifier.Shift);
        Assert.Equal(SwitchModifier.Shift, agent.Received[^1].SetSwitchModifier.Modifier);
        Assert.Equal(SwitchModifier.Shift, model.Snapshot.Status.SwitchModifier);
        await model.SetSwitchModifierAsync(SwitchModifier.None);
        Assert.Equal(SwitchModifier.None, model.Snapshot.Status.SwitchModifier);
    }

    [Fact]
    public async Task ChangesTheRelativeModeSettings()
    {
        var agent = new FakeAgent();
        agent.Status.RelativeAuto = true;
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);
        // A service that does not say it has relative mode has none.
        Assert.False(model.Snapshot.RelativeSupported);
        Assert.Null(model.Snapshot.RelativeShortcut);
        agent.Status.RelativeSupported = true;

        Assert.True(model.Snapshot.Status.RelativeAuto);
        await model.SetRelativeAutoAsync(false);
        Assert.False(agent.Received[^1].SetRelativeAuto.On);
        Assert.False(model.Snapshot.Status.RelativeAuto);

        // The hotkey: sent each time, so a service keeps it only when told to.
        await model.SetHotkeysAsync(Shortcut.Defaults);
        Assert.True(agent.Received[^1].SetHotkeys.Hotkeys.HasRelative);
        Assert.True(model.Snapshot.RelativeSupported);
        Assert.Equal("Ctrl + Alt + Shift + G", model.Snapshot.RelativeShortcut);
        var clash = ShortcutText.With(model.Snapshot.Hotkeys, ShortcutRow.Relative, Shortcut.DefaultLock);
        Assert.Equal("duplicate_relative", (await Assert.ThrowsAsync<AgentRefusal>(() => model.SetHotkeysAsync(clash))).Code);
        await model.SetHotkeysAsync(ShortcutText.With(model.Snapshot.Hotkeys, ShortcutRow.Relative, "control+alt+KeyR"));
        Assert.Equal("Ctrl + Alt + R", model.Snapshot.RelativeShortcut);
        await model.SetHotkeysAsync(ShortcutText.With(model.Snapshot.Hotkeys, ShortcutRow.Relative, ""));
        Assert.True(model.Snapshot.RelativeSupported);
        Assert.Null(model.Snapshot.RelativeShortcut);

        // The hotkey reached a computer without relative mode: each time counts anew.
        Assert.Null(model.Snapshot.RelativeRefusal);
        await agent.SendAsync(new Event { RelativeUnsupported = new RelativeUnsupported { DeviceId = "mac" } });
        await Eventually("a refusal", () => model.Snapshot.RelativeRefusal is not null);
        Assert.Equal(new RelativeRefusal(1, "mac"), model.Snapshot.RelativeRefusal);
        await agent.SendAsync(new Event { RelativeUnsupported = new RelativeUnsupported { DeviceId = "mac" } });
        await Eventually("a second refusal", () => model.Snapshot.RelativeRefusal?.Serial == 2);
    }

    [Fact]
    public async Task HearsEveryCrossingOfItsScreensEvenBackToBack()
    {
        var agent = new FakeAgent();
        await using var model = agent.Model();
        var heard = new System.Collections.Concurrent.ConcurrentQueue<CursorCrossed>();
        model.CursorCrossed += heard.Enqueue;
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);

        // Out over one edge and straight back in over another, and the
        // first again: each one is heard, in order, none folded into the
        // next.
        var left = new CursorCrossed { DisplayId = @"\\.\DISPLAY1", Edge = ScreenEdge.Left, Along = 540, Arriving = false };
        var top = new CursorCrossed { DisplayId = @"\\.\DISPLAY1", Edge = ScreenEdge.Top, Along = 960, Arriving = true };
        await agent.SendAsync(new Event { CursorCrossed = left });
        await agent.SendAsync(new Event { CursorCrossed = top });
        await agent.SendAsync(new Event { CursorCrossed = left });
        await Eventually("three crossings", () => heard.Count == 3);
        Assert.Equal([left, top, left], heard.ToArray());
    }

    [Fact]
    public async Task IdentifiesTheScreensHereToo()
    {
        var agent = new FakeAgent();
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);
        Assert.Null(model.Snapshot.ScreenIdentification);

        await model.IdentifyScreensAsync();
        Assert.Equal(ClientMessage.CommandOneofCase.IdentifyScreens, agent.Received[^1].CommandCase);
        await Eventually("the names shown here", () => model.Snapshot.ScreenIdentification is not null);
        Assert.Equal(new ScreenIdentification(1, 2500), model.Snapshot.ScreenIdentification);

        // Another computer's user identifies them: each time counts anew.
        await agent.SendAsync(new Event { ScreensIdentified = new ScreensIdentified { DurationMs = 4000 } });
        await Eventually("a second identification", () => model.Snapshot.ScreenIdentification?.Serial == 2);
        Assert.Equal(4000u, model.Snapshot.ScreenIdentification!.DurationMs);
    }

    [Fact]
    public async Task RenamesAnotherComputerHereOnly()
    {
        var agent = new FakeAgent();
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);
        Assert.Null(model.Snapshot.ReportedName("mac"));

        await model.RenameDeviceAsync("mac", "Desk Mac");
        Assert.Equal(("mac", "Desk Mac"), (agent.Received[^1].RenameDevice.DeviceId, agent.Received[^1].RenameDevice.Name));
        Assert.Equal("Desk Mac", model.Snapshot.NameOf("mac"));
        Assert.Equal("MacBook Pro", model.Snapshot.ReportedName("mac"));

        // Emptied, it goes by its own name again.
        await model.RenameDeviceAsync("mac", "");
        Assert.Equal("MacBook Pro", model.Snapshot.NameOf("mac"));
        Assert.Null(model.Snapshot.ReportedName("mac"));

        Assert.Equal("unknown_device", (await Assert.ThrowsAsync<AgentRefusal>(() => model.RenameDeviceAsync("pc", "Me"))).Code);
        Assert.Equal("invalid_name", (await Assert.ThrowsAsync<AgentRefusal>(() => model.RenameDeviceAsync("mac", new string('x', 65)))).Code);
    }

    [Fact]
    public async Task TurnsTheClipboardAndFilesOffWithOneComputer()
    {
        var agent = new FakeAgent();
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);
        Assert.True(model.Snapshot.CanReceiveFiles("mac"));

        await model.SetDeviceSharingAsync("mac", clipboardOff: true, filesOff: false);
        var sent = agent.Received[^1].SetDeviceSharing;
        Assert.Equal(("mac", true, false), (sent.DeviceId, sent.ClipboardOff, sent.FilesOff));
        Assert.True(model.Snapshot.Device("mac")!.ClipboardOff);
        Assert.True(model.Snapshot.CanReceiveFiles("mac"));

        await model.SetDeviceSharingAsync("mac", clipboardOff: true, filesOff: true);
        Assert.True(model.Snapshot.FilesOff("mac"));
        Assert.Empty(model.Snapshot.FileReceivers);
        Assert.Equal(
            "files_off",
            (await Assert.ThrowsAsync<AgentRefusal>(() => model.SendFilesAsync("mac", [@"C:\Users\me\Report.pdf"]))).Code);
        Assert.Equal("unknown_device", (await Assert.ThrowsAsync<AgentRefusal>(() => model.SetDeviceSharingAsync("ghost", false, true))).Code);
    }

    [Fact]
    public async Task TakesFinishedTransfersOffTheHistory()
    {
        var agent = new FakeAgent();
        foreach (var (id, state) in new[]
        {
            ("going", TransferState.Transferring), ("done", TransferState.Done),
            ("failed", TransferState.Failed), ("cancelled", TransferState.Cancelled),
        })
        {
            agent.Transfers.Transfers_.Add(new FileTransfer { TransferId = id, DeviceId = "mac", Name = $"{id}.txt", State = state });
        }
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);
        Assert.Equal(4, model.Snapshot.Transfers.Transfers_.Count);

        await model.RemoveTransferAsync("done");
        Assert.Equal("done", agent.Received[^1].RemoveTransfer.TransferId);
        Assert.Equal(["going", "failed", "cancelled"], model.Snapshot.Transfers.Transfers_.Select(t => t.TransferId));
        Assert.Equal("not_finished", (await Assert.ThrowsAsync<AgentRefusal>(() => model.RemoveTransferAsync("going"))).Code);
        Assert.Equal("not_found", (await Assert.ThrowsAsync<AgentRefusal>(() => model.RemoveTransferAsync("done"))).Code);

        await model.ClearTransfersAsync();
        Assert.Equal(ClientMessage.CommandOneofCase.ClearTransfers, agent.Received[^1].CommandCase);
        // Those still going stay.
        Assert.Equal(["going"], model.Snapshot.Transfers.Transfers_.Select(t => t.TransferId));
    }

    [Fact]
    public async Task RemembersTheLastSwitchDelay()
    {
        var agent = new FakeAgent();
        agent.Status.DefaultDwellMs = 150;
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);
        Assert.Equal(150u, model.Snapshot.SwitchDelayMs);

        await model.SetEdgeDwellAsync(400);
        await model.SetEdgeDwellAsync(0);
        // Turned back on, it waits as long as it last did.
        Assert.Equal(400u, model.Snapshot.SwitchDelayMs);
    }

    [Fact]
    public async Task PairsByCode()
    {
        var agent = new FakeAgent();
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);

        await model.StartPairingByAddressAsync("192.168.1.20");
        Assert.Equal("192.168.1.20", agent.Received[^1].StartPairing.Address);
        await Eventually("the code entry", () => model.Snapshot.Pairing.Phase == PairingPhase.EnterCode);
        var wrong = await model.SubmitPairingCodeAsync("111111");
        Assert.Equal(PairingPhase.EnterCode, wrong.Phase);
        Assert.Equal("wrong_code", model.Snapshot.Pairing.Error);
        var right = await model.SubmitPairingCodeAsync("382914");
        Assert.Equal(PairingPhase.Paired, right.Phase);
        Assert.Equal(PairingPhase.Paired, model.Snapshot.Pairing.Phase);
        await model.CancelPairingAsync();
        Assert.Equal(PairingPhase.Idle, model.Snapshot.Pairing.Phase);
    }

    [Fact]
    public async Task ReconnectsWhenTheServiceComesBack()
    {
        var agent = new FakeAgent();
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);

        agent.Up = false;
        agent.Disconnect();
        await Eventually("losing the connection", () => model.Snapshot.Link == AgentLink.Connecting);
        Assert.NotNull(model.Snapshot.UnreachableSince);
        await Assert.ThrowsAsync<AgentUnavailableException>(() => model.SetSharingAsync(false));

        agent.Status.ThisDeviceName = "Renamed PC";
        agent.Up = true;
        await Eventually("a new connection", () => model.Snapshot.Link == AgentLink.Connected);
        Assert.Equal("Renamed PC", model.Snapshot.Status.ThisDeviceName);
        Assert.Null(model.Snapshot.UnreachableSince);
        Assert.Equal(2, agent.Connections);
    }

    [Fact]
    public async Task AServiceOnAnotherIpcVersionIsUpdating()
    {
        var agent = new FakeAgent { IpcVersion = AgentModel.IpcVersion + 1 };
        await using var model = agent.Model();
        model.Start();
        await Eventually("the refused Hello", () => model.Snapshot.Link == AgentLink.Incompatible);
        // Nothing but the Hello was asked.
        Assert.All(agent.Received, m => Assert.Equal(ClientMessage.CommandOneofCase.Hello, m.CommandCase));

        agent.IpcVersion = AgentModel.IpcVersion;
        await Eventually("the updated service", () => model.Snapshot.Link == AgentLink.Connected);
    }

    [Fact]
    public async Task CountsTheTimeOutOfReachFromLaunch()
    {
        var agent = new FakeAgent { Up = false };
        var launched = DateTimeOffset.UtcNow;
        await using var model = agent.Model();
        model.Start();
        var since = model.Snapshot.UnreachableSince;
        Assert.NotNull(since);
        Assert.InRange(since.Value, launched.AddSeconds(-1), DateTimeOffset.UtcNow);
        Assert.False(model.Snapshot.IsUnresponsive(since.Value.AddSeconds(9)));
        Assert.True(model.Snapshot.IsUnresponsive(since.Value.AddSeconds(11)));
    }

    [Fact]
    public async Task RequestsWithoutAConnectionFailAtOnce()
    {
        var agent = new FakeAgent { Up = false };
        await using var model = agent.Model();
        await Assert.ThrowsAsync<AgentUnavailableException>(() => model.SetLockedAsync(true));
    }
}
