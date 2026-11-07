using Inputweave.App.V1;
using InputWeave.Main;
using InputWeave.Panel;
using InputWeave.Setup;
using InputWeave.Support;
using Xunit;

namespace InputWeave.Tests;

/// <summary>The steps of first-run setup.</summary>
public class SetupStepsTests
{
    [Fact]
    public void HasSevenStepsOnlyWhileTheNetworkIsPublic()
    {
        var all = SetupSteps.For(publicNetwork: true);
        Assert.Equal(
            [SetupStep.Welcome, SetupStep.Name, SetupStep.Network, SetupStep.Connect, SetupStep.EnterCode, SetupStep.Arrange, SetupStep.TryIt],
            all);
        var usual = SetupSteps.For(publicNetwork: false);
        Assert.Equal(6, usual.Count);
        Assert.DoesNotContain(SetupStep.Network, usual);
        Assert.Equal(3, SetupSteps.Number(usual, SetupStep.Connect));
        Assert.Equal(4, SetupSteps.Number(all, SetupStep.Connect));
        Assert.Equal(SetupStep.Connect, SetupSteps.Next(usual, SetupStep.Name));
        Assert.Equal(SetupStep.Network, SetupSteps.Next(all, SetupStep.Name));
        Assert.Null(SetupSteps.Next(all, SetupStep.TryIt));
        Assert.Equal(SetupStep.Name, SetupSteps.Previous(usual, SetupStep.Connect));
        Assert.Null(SetupSteps.Previous(usual, SetupStep.Welcome));
    }

    [Fact]
    public void CountsTheCodeAsItsOwnStep()
    {
        Assert.Equal(SetupStep.Connect, SetupSteps.ForPairing(PairingPhase.Idle));
        Assert.Equal(SetupStep.Connect, SetupSteps.ForPairing(PairingPhase.Connecting));
        Assert.Equal(SetupStep.EnterCode, SetupSteps.ForPairing(PairingPhase.EnterCode));
        Assert.Equal(SetupStep.EnterCode, SetupSteps.ForPairing(PairingPhase.Verifying));
        Assert.Equal(SetupStep.EnterCode, SetupSteps.ForPairing(PairingPhase.Paired));
        Assert.Equal(SetupStep.Connect, SetupSteps.ForPairing(PairingPhase.Failed));
    }

    [Fact]
    public void WorksOnceTheCursorCrossesAfterTheStepOpened()
    {
        var here = Sample.Model();
        here.Status.ControlReturns = 7;
        Assert.False(SetupSteps.Worked(here, returnsBefore: 7));
        Assert.True(SetupSteps.Worked(Sample.Model(active: "mac"), returnsBefore: 7));
        var back = Sample.Model();
        back.Status.ControlReturns = 8;
        Assert.True(SetupSteps.Worked(back, returnsBefore: 7));
    }

    [Fact]
    public void NamesTheComputerJustPaired()
    {
        var model = Sample.Model();
        model.Status.Devices.Insert(0, new DeviceStatus { Id = "old", Name = "Old Laptop", State = ConnectionState.Offline });
        model.Status.Devices.Add(new DeviceStatus { Id = "new", Name = "New PC", State = ConnectionState.Connected });
        Assert.Equal("New PC", SetupSteps.OtherName(model, "new"));
        Assert.Equal("MacBook Pro", SetupSteps.OtherName(model, null));
        Assert.Null(SetupSteps.OtherName(Sample.Model(noDevices: true), null));
    }

    [Fact]
    public void TitlesEveryStep()
    {
        foreach (var step in Enum.GetValues<SetupStep>())
        {
            Assert.NotEmpty(SetupSteps.TitleKey(step));
        }
        Assert.Equal(TextKeys.SetupCodeTitle, SetupSteps.TitleKey(SetupStep.EnterCode));
    }

    [Fact]
    public void WarnsAboutAPublicNetwork()
    {
        Assert.True(NetworkCheck.IsPublic([NetworkCategory.Private, NetworkCategory.Public]));
        Assert.False(NetworkCheck.IsPublic([NetworkCategory.Private, NetworkCategory.DomainAuthenticated]));
        Assert.False(NetworkCheck.IsPublic([]));
    }
}

/// <summary>The arguments from File Explorer's Send To and from notifications.</summary>
public class LaunchArgumentsTests
{
    [Fact]
    public void TakesEverythingAfterSendAsPaths()
    {
        Assert.Null(LaunchArguments.SendPaths(["--show-panel"]));
        Assert.Equal([@"C:\a b.txt", @"D:\c.pdf"], LaunchArguments.SendPaths(["--send", @"C:\a b.txt", @"D:\c.pdf"]));
        Assert.Empty(LaunchArguments.SendPaths(["--send"])!);
        // Explorer appends after the shortcut's own argument; nothing after it is an option.
        Assert.Equal(["--show-panel"], LaunchArguments.SendPaths(["--send", "--show-panel"]));
    }

    [Fact]
    public void KnowsWhenTheAppWasStartedForSomething()
    {
        Assert.True(LaunchArguments.HasPurpose(["--send", @"C:\a.txt"]));
        Assert.True(LaunchArguments.HasPurpose(["----AppNotificationActivated:"]));
        Assert.True(LaunchArguments.HasPurpose(["--background"]));
        Assert.False(LaunchArguments.HasPurpose([]));
        Assert.False(LaunchArguments.HasPurpose(["--language", "en-US"]));
    }

    [Fact]
    public void HandsArgumentsToTheRunningApp()
    {
        string[] args = ["--send", @"C:\Users\me\My Files\a;b=c.txt", @"\\server\share\d.txt"];
        Assert.Equal(args, LaunchArguments.Decode(LaunchArguments.Encode(args)));
        Assert.Empty(LaunchArguments.Decode(LaunchArguments.Encode([])));
    }

    [Fact]
    public void TellsTheRunningAppWhatALaterLaunchAsks()
    {
        Assert.Equal(Relaunch.OpenPanel, LaunchArguments.ForRunningApp([], setupDue: false));
        // Started in the background before setup was finished: the user's
        // first open shows setup rather than the panel.
        Assert.Equal(Relaunch.OpenSetup, LaunchArguments.ForRunningApp([], setupDue: true));
        Assert.Equal(Relaunch.Nothing, LaunchArguments.ForRunningApp(["--background"], setupDue: true));
        Assert.Equal(Relaunch.Nothing, LaunchArguments.ForRunningApp(["--background"], setupDue: false));
        Assert.Equal(Relaunch.Send, LaunchArguments.ForRunningApp(["--send", @"C:\a.txt"], setupDue: true));
        // A file that happens to be named like the option is still sent.
        Assert.Equal(Relaunch.Send, LaunchArguments.ForRunningApp(["--send", "--background"], setupDue: false));
        Assert.Equal(Relaunch.OpenPanel, LaunchArguments.ForRunningApp(["----AppNotificationActivated:"], setupDue: true));
    }
}

/// <summary>What the app opens on its own as it starts, for which arguments.</summary>
public class LaunchTests
{
    [Fact]
    public void OpensSetupAtLaunchUntilItIsFinished()
    {
        Assert.Equal(SetupStep.Welcome, Launch.Parse([]).SetupAt(finishedSetup: false));
        Assert.Null(Launch.Parse([]).SetupAt(finishedSetup: true));
        Assert.Equal(SetupStep.Welcome, Launch.Parse(["--language", "en-US"]).SetupAt(finishedSetup: false));
        var plain = Launch.Parse([]);
        Assert.False(plain.Background);
        Assert.False(plain.Testing);
        Assert.False(plain.ShowPanel);
        Assert.Null(plain.Window);
    }

    [Fact]
    public void OpensNothingWhenTheBackgroundServiceStartedIt()
    {
        var launch = Launch.Parse(["--background"]);
        Assert.True(launch.Background);
        // Not a test: nothing is kept above other windows.
        Assert.False(launch.Testing);
        Assert.Null(launch.SetupAt(finishedSetup: false));
        Assert.Null(launch.SetupAt(finishedSetup: true));
        Assert.False(launch.ShowPanel);
        Assert.Null(launch.Window);
        Assert.False(launch.AddDevice);
        // The sample code; the service's code shows however the app started.
        Assert.False(launch.ShowPairingCode);
        Assert.Null(launch.Notification);
    }

    [Fact]
    public void OpensNoSetupWhenStartedForSomethingElse()
    {
        Assert.Null(Launch.Parse(["--send", @"C:\a.txt"]).SetupAt(finishedSetup: false));
        Assert.Null(Launch.Parse(["----AppNotificationActivated:"]).SetupAt(finishedSetup: false));
        // A file sent from File Explorer that is named like the option.
        Assert.False(Launch.Parse(["--send", "--background"]).Background);
    }

    [Fact]
    public void OpensWhatATestAsksForInsteadOfSetup()
    {
        Assert.Null(Launch.Parse(["--show-panel"]).SetupAt(finishedSetup: false));
        Assert.True(Launch.Parse(["--show-panel"]).ShowPanel);
        Assert.Equal(SetupStep.Network, Launch.Parse(["--show-setup", "network"]).SetupAt(finishedSetup: true));
        Assert.Equal(SetupStep.Welcome, Launch.Parse(["--show-setup"]).SetupAt(finishedSetup: true));
        Assert.Equal(SetupStep.Name, Launch.Parse(["--background", "--show-setup", "name"]).SetupAt(finishedSetup: false));
        Assert.Equal(MainPage.Devices, Launch.Parse(["--show-window", "devices"]).Window);
        Assert.Equal(MainPage.General, Launch.Parse(["--show-window=general"]).Window);
        Assert.Equal(MainPage.General, Launch.Parse(["--show-window"]).Window);
        var add = Launch.Parse(["--show-add-device"]);
        Assert.True(add.AddDevice);
        Assert.Equal(MainPage.Devices, add.Window);
        Assert.True(Launch.Parse(["--show-pairing-code"]).ShowPairingCode);
        Assert.Equal("Hello there", Launch.Parse(["--show-hud", "Hello there"]).Hud);
        Assert.Equal("", Launch.Parse(["--show-hud"]).Hud);
        var locator = Launch.Parse(["--show-locator", "0.25"]);
        Assert.True(locator.Locator);
        Assert.Equal<double?>(0.25, locator.LocatorAt);
        Assert.Null(Launch.Parse(["--show-locator"]).LocatorAt);
        Assert.Equal("offer", Launch.Parse(["--show-notification=offer"]).Notification);
        Assert.Equal("", Launch.Parse(["--show-send-picker"]).SendPicker);
        Assert.Equal("none", Launch.Parse(["--show-send-picker", "none"]).SendPicker);
        Assert.True(Launch.Parse(["--show-send-picker"]).Testing);
        Assert.Null(Launch.Parse([]).SendPicker);
    }
}
