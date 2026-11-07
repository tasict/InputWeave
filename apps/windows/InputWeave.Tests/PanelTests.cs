using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Support;
using InputWeave.Tray;
using Xunit;

namespace InputWeave.Tests;

/// <summary>Snapshots with fixed state, like the macOS app's AgentModel.sample.</summary>
internal static class Sample
{
    public static readonly DateTimeOffset Now = new(2026, 10, 22, 18, 0, 0, TimeSpan.Zero);

    public static AgentSnapshot Model(
        AgentLink link = AgentLink.Connected, double unreachableFor = 0, bool sharing = true, bool locked = false,
        string active = "pc", string controlledBy = "", bool missingPermission = false, bool macOnline = true,
        bool noDevices = false, bool macFilesOff = false, Hotkeys? hotkeys = null, bool macScreenLocked = false,
        bool macLockScreenInput = false, bool dragsCarried = false, string relative = "", bool relativeAutomatic = false,
        ulong relativeEnds = 0, RelativeEndReason endReason = RelativeEndReason.Unspecified, bool relativeSupported = true,
        bool macInputUnavailable = false, bool macSharingPaused = false, bool wakeSupported = false, bool macCanWake = false,
        WakeState macWake = WakeState.None, ulong macOfflineSince = 0, bool offlineSupported = false,
        string macOfflineBy = "", string thisOfflineBy = "")
    {
        var status = new Status
        {
            ThisDeviceId = "pc",
            ThisDeviceName = "Windows Desktop",
            Sharing = sharing,
            Locked = locked,
            ActiveDeviceId = active,
            ControlledByDeviceId = controlledBy,
            Hotkeys = hotkeys,
            DragsCarried = dragsCarried,
            RelativeMouse = relative,
            RelativeMouseAutomatic = relativeAutomatic,
            RelativeEnds = relativeEnds,
            RelativeSupported = relativeSupported,
            WakeSupported = wakeSupported,
            OfflineSupported = offlineSupported,
            // Taken offline by this computer, or by the MacBook Pro.
            OfflineForNow = thisOfflineBy.Length != 0,
            OfflineByDeviceId = thisOfflineBy,
            OfflineByName = thisOfflineBy switch { "" => "", "pc" => "Windows Desktop", _ => "MacBook Pro" },
            LastRelativeEnd = relativeEnds == 0 ? null : new RelativeEnd { DeviceId = "mac", Reason = endReason },
        };
        if (missingPermission)
        {
            status.Permissions.Add(new PermissionStatus { Permission = Permission.Accessibility, Granted = false });
        }
        if (!noDevices)
        {
            status.Devices.Add(new DeviceStatus
            {
                Id = "mac", Name = "MacBook Pro",
                State = macOnline ? ConnectionState.Connected : ConnectionState.Offline,
                FilesOff = macFilesOff,
                ScreenLocked = macScreenLocked,
                LockScreenInput = macLockScreenInput,
                InputUnavailable = macInputUnavailable,
                SharingPaused = macSharingPaused,
                CanWake = macCanWake,
                Wake = macWake,
                OfflineSinceUnixMs = macOfflineSince,
                OfflineForNow = macOfflineBy.Length != 0,
                OfflineByDeviceId = macOfflineBy,
                OfflineByName = macOfflineBy switch { "" => "", "pc" => "Windows Desktop", _ => "Mac mini" },
            });
        }
        return new AgentSnapshot
        {
            Link = link,
            UnreachableSince = link == AgentLink.Connected ? null : Now.AddSeconds(-unreachableFor),
            Status = status,
        };
    }
}

public class PanelTextTests
{
    private static UiText Title(AgentSnapshot model, ServiceState service = ServiceState.Running) =>
        PanelText.Title(model, service, Sample.Now);

    [Fact]
    public void SaysWhatTheServiceIsDoingUntilItAnswers()
    {
        Assert.Equal(new UiText(TextKeys.TitleStarting), Title(Sample.Model(AgentLink.Connecting, unreachableFor: 9)));
        Assert.Equal(new UiText(TextKeys.TitleNotResponding), Title(Sample.Model(AgentLink.Connecting, unreachableFor: 11)));
        Assert.Equal(new UiText(TextKeys.TitleUpdating), Title(Sample.Model(AgentLink.Incompatible, unreachableFor: 60)));
        // A stopped service is not starting; the row below offers to start it.
        Assert.Equal(
            new UiText(TextKeys.TitleNotWorking),
            Title(Sample.Model(AgentLink.Connecting), ServiceState.Stopped));
        Assert.Null(PanelText.Subtitle(Sample.Model(AgentLink.Connecting)));
    }

    [Fact]
    public void NamesTheComputerTheCursorIsLockedTo()
    {
        Assert.Equal(new UiText(TextKeys.TitleLockedTo, "Windows Desktop"), Title(Sample.Model(locked: true)));
        Assert.Equal(new UiText(TextKeys.SubtitleLocked, "Ctrl + Alt + Shift + L"), PanelText.Subtitle(Sample.Model(locked: true)));
        Assert.Equal(
            new UiText(TextKeys.SubtitleLocked, "Ctrl + Alt + Shift + K"),
            PanelText.Subtitle(Sample.Model(locked: true, hotkeys: new Hotkeys { TakeBack = "control+alt+shift+Escape", Lock = "control+alt+shift+KeyK" })));
        Assert.Equal(new UiText(TextKeys.TitleLockedTo, "MacBook Pro"), Title(Sample.Model(locked: true, active: "mac")));
        // Before the service names the active device, the cursor is here.
        Assert.Equal(new UiText(TextKeys.TitleLockedTo, "Windows Desktop"), Title(Sample.Model(locked: true, active: "")));
    }

    [Fact]
    public void PausingAndBeingControlledComeFirst()
    {
        Assert.Equal(new UiText(TextKeys.TitlePaused), Title(Sample.Model(sharing: false, locked: true)));
        Assert.Null(PanelText.Subtitle(Sample.Model(sharing: false, locked: true)));
        Assert.Equal(
            new UiText(TextKeys.TitleControlledBy, "MacBook Pro"),
            Title(Sample.Model(locked: true, controlledBy: "mac")));
        Assert.Equal(
            new UiText(TextKeys.SubtitleControlled),
            PanelText.Subtitle(Sample.Model(locked: true, controlledBy: "mac")));
    }

    [Fact]
    public void UnlockedTextNamesWhereTheCursorIs()
    {
        Assert.Equal(new UiText(TextKeys.TitleSharing), Title(Sample.Model()));
        Assert.Equal(new UiText(TextKeys.SubtitleCursorOn, "Windows Desktop"), PanelText.Subtitle(Sample.Model()));
        Assert.Equal(new UiText(TextKeys.TitleControlling, "MacBook Pro"), Title(Sample.Model(active: "mac")));
        Assert.Equal(
            new UiText(TextKeys.SubtitleControlling, "Ctrl + Alt + Shift + Esc"),
            PanelText.Subtitle(Sample.Model(active: "mac")));
    }

    [Fact]
    public void SaysWhileRelativeModeIsOn()
    {
        // The computer and how to end it.
        var withHotkey = new Hotkeys
        {
            TakeBack = Shortcut.DefaultTakeBack, Lock = Shortcut.DefaultLock,
            JumpModifiers = Shortcut.DefaultJumpModifiers, Relative = Shortcut.DefaultRelative,
        };
        var on = Sample.Model(active: "mac", relative: "mac", hotkeys: withHotkey);
        Assert.Equal(new UiText(TextKeys.TitleRelative, "MacBook Pro"), Title(on));
        Assert.Equal(new UiText(TextKeys.SubtitleRelative, "Ctrl + Alt + Shift + G"), PanelText.Subtitle(on));
        // Its hotkey off: taking back control ends it too.
        var off = withHotkey.Clone();
        off.Relative = "";
        Assert.Equal(
            new UiText(TextKeys.SubtitleControlling, "Ctrl + Alt + Shift + Esc"),
            PanelText.Subtitle(Sample.Model(active: "mac", relative: "mac", hotkeys: off)));
        // Locked too, relative mode is the news.
        Assert.Equal(new UiText(TextKeys.TitleRelative, "MacBook Pro"),
            Title(Sample.Model(active: "mac", relative: "mac", locked: true, hotkeys: withHotkey)));
        Assert.Equal(new UiText(TextKeys.TitlePaused), Title(Sample.Model(sharing: false, relative: "mac")));
    }
}

public class TrayIconStateTests
{
    private static TrayIconState State(AgentSnapshot model, ServiceState service = ServiceState.Running) =>
        TrayIcons.StateFor(model, service, Sample.Now);

    [Fact]
    public void ShowsWhatIsGoingOn()
    {
        Assert.Equal(TrayIconState.Normal, State(Sample.Model()));
        Assert.Equal(TrayIconState.Paused, State(Sample.Model(sharing: false)));
        Assert.Equal(TrayIconState.Controlling, State(Sample.Model(active: "mac")));
        Assert.Equal(TrayIconState.Controlled, State(Sample.Model(controlledBy: "mac")));
    }

    [Fact]
    public void AsksForAttentionOnlyForWhatTheUserHasToFix()
    {
        Assert.Equal(TrayIconState.Attention, State(Sample.Model(AgentLink.Connecting), ServiceState.Stopped));
        Assert.Equal(TrayIconState.Attention, State(Sample.Model(missingPermission: true)));
        Assert.Equal(TrayIconState.Attention, State(Sample.Model(AgentLink.Connecting, unreachableFor: 11)));
        // An unreachable computer is shown in the panel, not on the icon.
        Assert.Equal(TrayIconState.Normal, State(Sample.Model(macOnline: false)));
    }

    [Fact]
    public void StaysQuietWhileTheServiceStartsOrUpdates()
    {
        // A fresh launch: no status yet, so not "paused" either.
        var starting = new AgentSnapshot { UnreachableSince = Sample.Now };
        Assert.Equal(TrayIconState.Normal, State(starting));
        Assert.Equal(TrayIconState.Normal, State(Sample.Model(AgentLink.Connecting, unreachableFor: 9)));
        Assert.Equal(TrayIconState.Normal, State(Sample.Model(AgentLink.Incompatible, unreachableFor: 60)));
        Assert.Equal(TrayIconState.Normal, State(Sample.Model(AgentLink.Connecting), ServiceState.Starting));
        Assert.Equal(
            TrayIconState.Attention,
            TrayIcons.StateFor(Sample.Model(AgentLink.Connecting), ServiceState.Running, Sample.Now.AddSeconds(11)));
    }

    [Fact]
    public void PicksTheGlyphForTheTaskbarsTheme()
    {
        Assert.Equal("controlled-light.ico", TrayIcons.FileName(TrayIconState.Controlled, lightTaskbar: true));
        Assert.Equal("normal-dark.ico", TrayIcons.FileName(TrayIconState.Normal, lightTaskbar: false));
    }
}

public class ExceptionRowsTests
{
    [Fact]
    public void OffersToStartAStoppedService()
    {
        var rows = ExceptionRows.For(Sample.Model(AgentLink.Connecting), ServiceState.Stopped);
        Assert.Equal([new ExceptionRow(new UiText(TextKeys.ExceptionServiceStopped), ExceptionAction.StartService)], rows);
        Assert.Empty(ExceptionRows.For(Sample.Model(AgentLink.Connecting), ServiceState.Running));
    }

    [Fact]
    public void NamesEveryComputerThatCannotBeReached()
    {
        Assert.Empty(ExceptionRows.For(Sample.Model(), ServiceState.Running));
        Assert.Equal(
            [new ExceptionRow(new UiText(TextKeys.ExceptionUnreachable, "MacBook Pro"), ExceptionAction.Troubleshoot)],
            ExceptionRows.For(Sample.Model(macOnline: false), ServiceState.Running));
        Assert.Equal(
            [new ExceptionRow(new UiText(TextKeys.ExceptionNoDevices), ExceptionAction.AddDevice)],
            ExceptionRows.For(Sample.Model(noDevices: true), ServiceState.Running));
        Assert.Equal(TextKeys.ActionTroubleshoot, new ExceptionRow(new UiText("x"), ExceptionAction.Troubleshoot).ActionKey);
    }

    [Fact]
    public void TroubleshootingListsProblemsWithoutLeadingBackToItself()
    {
        Assert.Equal(
            [new ExceptionRow(new UiText(TextKeys.ExceptionUnreachable, "MacBook Pro"))],
            ExceptionRows.For(Sample.Model(macOnline: false), ServiceState.Running, compact: false));
        Assert.Empty(ExceptionRows.For(Sample.Model(noDevices: true), ServiceState.Running, compact: false));

        var capturing = Sample.Model() with { Status = Sample.Model().Status.Clone() };
        capturing.Status.Capturing = true;
        Assert.True(ExceptionRows.EverythingWorks(capturing, ServiceState.Running));
        Assert.False(ExceptionRows.EverythingWorks(capturing, ServiceState.Stopped));
        // Not capturing yet: this computer cannot control the others.
        Assert.False(ExceptionRows.EverythingWorks(Sample.Model(), ServiceState.Running));
        Assert.False(ExceptionRows.EverythingWorks(Sample.Model(AgentLink.Connecting), ServiceState.Running));
    }

    [Fact]
    public void LeavesOutWhatOnlyMacsNeed()
    {
        Assert.Empty(ExceptionRows.For(Sample.Model(missingPermission: true), ServiceState.Running));
    }
}

public class MenuStateTests
{
    [Fact]
    public void FollowsSharingAndTheLock()
    {
        Assert.Equal(
            new MenuState(TextKeys.MenuPauseSharing, true, TextKeys.MenuLock, true, true),
            MenuState.For(Sample.Model()));
        Assert.Equal(
            new MenuState(TextKeys.MenuResumeSharing, true, TextKeys.MenuLock, false, true),
            MenuState.For(Sample.Model(sharing: false)));
        Assert.Equal(
            new MenuState(TextKeys.MenuPauseSharing, true, TextKeys.MenuUnlock, true, true),
            MenuState.For(Sample.Model(locked: true)));
    }

    [Fact]
    public void CannotLockWhileControlledOrWithoutTheService()
    {
        Assert.False(MenuState.For(Sample.Model(controlledBy: "mac")).LockEnabled);
        var offline = MenuState.For(Sample.Model(AgentLink.Connecting));
        Assert.False(offline.PauseEnabled);
        Assert.False(offline.LockEnabled);
        Assert.False(offline.IdentifyEnabled);
        // Identifying the screens needs only the service, even while another computer controls this one.
        Assert.True(MenuState.For(Sample.Model(controlledBy: "mac", sharing: false)).IdentifyEnabled);
    }
}
