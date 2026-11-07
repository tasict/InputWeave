using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Settings;
using Xunit;

namespace InputWeave.Tests;

/// <summary>
/// Where files from Send To and Send Files… go: the reason a computer
/// can't take them, the choice between none, one and several, and the
/// summary under the picker's title.
/// </summary>
public class SendTargetsTests
{
    private static DeviceStatus Device(
        string id, ConnectionState state = ConnectionState.Connected, bool filesOff = false, uint rtt = 0, bool offlineForNow = false) =>
        new() { Id = id, Name = id.ToUpperInvariant(), Os = "macos", State = state, FilesOff = filesOff, RttMs = rtt, OfflineForNow = offlineForNow };

    private static AgentSnapshot Model(params DeviceStatus[] devices)
    {
        var status = new Status { ThisDeviceId = "me", ThisDeviceName = "Windows Desktop" };
        status.Devices.Add(devices);
        return new AgentSnapshot { Link = AgentLink.Connected, Status = status };
    }

    [Fact]
    public void TheReasonIsTheFirstThatApplies()
    {
        Assert.Null(SendTargets.Unavailable(Device("a")));
        Assert.Equal(SendUnavailable.FilesOff, SendTargets.Unavailable(Device("a", filesOff: true)));
        Assert.Equal(SendUnavailable.Offline, SendTargets.Unavailable(Device("a", ConnectionState.Offline)));
        Assert.Equal(SendUnavailable.Offline, SendTargets.Unavailable(Device("a", ConnectionState.Incompatible)));
        // Offline before files off.
        Assert.Equal(SendUnavailable.Offline, SendTargets.Unavailable(Device("a", ConnectionState.Offline, filesOff: true)));
        // Offline for now before offline and files off.
        Assert.Equal(
            SendUnavailable.OfflineForNow,
            SendTargets.Unavailable(Device("a", ConnectionState.Offline, filesOff: true, offlineForNow: true)));
        Assert.Equal(new UiText(TextKeys.ConnectionOffline), SendTargets.Reason(SendUnavailable.Offline));
        Assert.Equal(new UiText(TextKeys.SendReasonFilesOff), SendTargets.Reason(SendUnavailable.FilesOff));
        Assert.Equal(new UiText(TextKeys.ConnectionOfflineForNow), SendTargets.Reason(SendUnavailable.OfflineForNow));
    }

    [Fact]
    public void AComputerOfflineForNowSaysSo()
    {
        var group = Model(
            Device("mini", ConnectionState.Offline, offlineForNow: true), Device("pc", rtt: 3), Device("off", ConnectionState.Offline));
        Assert.Equal(new SendChoice.Only("pc"), SendTargets.Choose(group));
        var targets = SendTargets.Targets(group);
        Assert.Equal(["pc", "mini", "off"], targets.Select(t => t.Id));
        Assert.Equal([true, false, false], targets.Select(t => t.Available));
        Assert.Equal(
            [
                new UiText(TextKeys.ConnectionConnectedRtt, "3"),
                new UiText(TextKeys.ConnectionOfflineForNow),
                new UiText(TextKeys.ConnectionOffline),
            ],
            targets.Select(t => t.Status));
        // This computer offline for now: the service reports every other one
        // just offline, so there is nowhere to send.
        var apart = Model(Device("mini", ConnectionState.Offline), Device("pc", ConnectionState.Offline));
        apart.Status.OfflineForNow = true;
        Assert.IsType<SendChoice.Nowhere>(SendTargets.Choose(apart));
        Assert.All(SendTargets.Targets(apart), t => Assert.Equal(new UiText(TextKeys.ConnectionOffline), t.Status));
    }

    [Fact]
    public void TheFileReceiversAreThoseWithoutAReason()
    {
        var model = Model(Device("a"), Device("b", filesOff: true), Device("c", ConnectionState.Offline));
        Assert.Equal(["a"], model.FileReceivers.Select(d => d.Id));
    }

    [Fact]
    public void NowhereOneOrAChoice()
    {
        Assert.IsType<SendChoice.Nowhere>(SendTargets.Choose(Model()));
        // None connected.
        Assert.IsType<SendChoice.Nowhere>(SendTargets.Choose(Model(Device("off", ConnectionState.Offline))));
        // Connected, but none can take files: the picker says why, with nothing to choose.
        var noneCan = Model(Device("off", ConnectionState.Offline), Device("nofiles", filesOff: true));
        Assert.IsType<SendChoice.Ask>(SendTargets.Choose(noneCan));
        Assert.Equal([false, false], SendTargets.Targets(noneCan).Select(t => t.Available));
        Assert.Null(SendTargets.Chosen(null, null, SendTargets.Targets(noneCan)));
        Assert.Equal(new SendChoice.Only("pc"), SendTargets.Choose(Model(Device("off", ConnectionState.Offline), Device("pc"))));
        var twoCan = Model(
            Device("off", ConnectionState.Offline), Device("pc", rtt: 3), Device("nofiles", filesOff: true, rtt: 4), Device("mac"));
        Assert.IsType<SendChoice.Ask>(SendTargets.Choose(twoCan));
        var targets = SendTargets.Targets(twoCan);
        Assert.Equal(["pc", "mac", "off", "nofiles"], targets.Select(t => t.Id));
        Assert.Equal([true, true, false, false], targets.Select(t => t.Available));
        Assert.Equal(
            [
                new UiText(TextKeys.ConnectionConnectedRtt, "3"),
                new UiText(TextKeys.ConnectionConnected),
                new UiText(TextKeys.ConnectionOffline),
                new UiText(TextKeys.SendReasonFilesOff),
            ],
            targets.Select(t => t.Status));
        Assert.Equal("PC", targets[0].Name);
    }

    [Fact]
    public void TheChoiceStaysWhileItCanTakeFiles()
    {
        IReadOnlyList<SendTarget> With(ConnectionState ahead, ConnectionState mac) =>
            SendTargets.Targets(Model(Device("ahead", ahead), Device("pc"), Device("mac", mac)));
        // By default the first that can.
        Assert.Equal("pc", SendTargets.Chosen(null, null, With(ConnectionState.Offline, ConnectionState.Connected))?.Id);
        // A computer that connects ahead of it does not take its place.
        var after = With(ConnectionState.Connected, ConnectionState.Connected);
        Assert.Equal("pc", SendTargets.Chosen(null, "pc", after)?.Id);
        Assert.Equal("mac", SendTargets.Chosen("mac", "mac", after)?.Id);
        // The one clicked goes offline: the first that can; it comes back when it can again.
        Assert.Equal("ahead", SendTargets.Chosen("mac", "mac", With(ConnectionState.Connected, ConnectionState.Offline))?.Id);
        Assert.Equal("mac", SendTargets.Chosen("mac", "ahead", after)?.Id);
        // A default the choice moved away from does not come back.
        var pcGone = SendTargets.Targets(Model(Device("ahead"), Device("pc", ConnectionState.Offline)));
        Assert.Equal("ahead", SendTargets.Chosen(null, "pc", pcGone)?.Id);
        Assert.Equal("ahead", SendTargets.Chosen(null, "ahead", after)?.Id);
        Assert.Equal("ahead", SendTargets.Chosen("gone", "gone", after)?.Id);
    }

    [Fact]
    public void ASecondRequestAddsItsItemsOnce()
    {
        Assert.Equal(
            [@"C:\a", @"C:\b", @"C:\c", @"C:\d"],
            SendTargets.Merged([@"C:\a", @"C:\b"], [@"C:\b", @"C:\c", @"c:\A", @"C:\d"]));
        Assert.Equal([@"C:\a"], SendTargets.Merged([], [@"C:\a"]));
        // The same folder written another way: a trailing separator, "." and ".." parts.
        Assert.Equal(
            [@"C:\Users\me\Photos\", @"C:\Users\me\Photos2"],
            SendTargets.Merged(
                [@"C:\Users\me\Photos\"],
                [@"C:\Users\me\Photos", @"C:\Users\me\.\Photos", @"C:\Users\me\x\..\Photos", @"C:\Users\me\Photos2"]));
    }

    [Fact]
    public void TheSummaryNamesTheFirstItem()
    {
        Assert.Equal(new UiText(TextKeys.Verbatim, "Report.pdf"), SendTargets.Summary([@"C:\Users\me\Report.pdf"]));
        Assert.Equal(new UiText(TextKeys.Verbatim, "Photos"), SendTargets.Summary([@"C:\Users\me\Photos\"]));
        Assert.Equal(
            new UiText(TextKeys.SendSummaryOneMore, "Report.pdf"),
            SendTargets.Summary([@"C:\Users\me\Report.pdf", @"C:\Users\me\Photos"]));
        Assert.Equal(
            new UiText(TextKeys.SendSummaryMore, "Report.pdf", "2"),
            SendTargets.Summary([@"C:\Users\me\Report.pdf", @"C:\a", @"C:\b"]));
        Assert.Equal(new UiText(TextKeys.Verbatim, @"D:\"), SendTargets.Summary([@"D:\"]));
        Assert.Equal(new UiText(TextKeys.Verbatim, ""), SendTargets.Summary([]));
    }
}
