using System.Globalization;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Overlay;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using InputWeave.Tray;
using Xunit;

namespace InputWeave.Tests;

/// <summary>Profiles as the service reports them, for the tests.</summary>
internal static class ProfileSample
{
    public static Network Home => new() { Id = "n-home", Kind = NetworkKind.Wired, Router = "192.168.1.1", Resolved = true };
    public static Network OfficeWifi => new() { Id = "n-office", Name = "Office-5G", Kind = NetworkKind.Wifi, Router = "172.16.0.1", Resolved = true };

    /// <summary>
    /// The sample model with Default (the home network), Office (its Wi-Fi)
    /// and Other networks, the profile <paramref name="inUse"/> in use,
    /// chosen by the user or not, on <paramref name="network"/>.
    /// </summary>
    public static AgentSnapshot Model(
        string inUse = "default", bool chosen = false, bool paused = false, bool macLeftOut = false,
        Network? network = null, bool noNetwork = false, bool files = true, bool supported = true,
        AgentLink link = AgentLink.Connected, bool macOnline = true, Action<Status>? also = null) =>
        WakeTests.With(Sample.Model(link, macOnline: macOnline && !macLeftOut && !paused, offlineSupported: true), s =>
        {
            s.ProfilesSupported = supported;
            s.Network = noNetwork ? null : network ?? Home;
            var home = new Profile { Id = "default", Kind = ProfileKind.Default, Clipboard = true, Files = true };
            home.Networks.Add(Home);
            var office = new Profile { Id = "office", Kind = ProfileKind.Custom, Name = "Office", Clipboard = true, Files = true };
            office.Networks.Add(OfficeWifi);
            var other = new Profile { Id = "other", Kind = ProfileKind.OtherNetworks, Clipboard = true, Files = true };
            s.Profiles.Add(home);
            s.Profiles.Add(office);
            s.Profiles.Add(other);
            var used = s.Profiles.First(p => p.Id == inUse);
            used.Paused = paused;
            used.Files = files;
            if (macLeftOut)
            {
                used.OffDeviceIds.Add("mac");
            }
            s.ProfileId = inUse;
            s.ProfileChosen = chosen;
            // As the service: a computer the profile leaves out is offline
            // and says so, except while the profile is paused, when every
            // other one is just offline.
            s.Devices[0].NotInProfile = macLeftOut && !paused;
            also?.Invoke(s);
        });
}

/// <summary>
/// What the app shows and offers for profiles, from the service's profile
/// fields: the same rules as on the Mac and Linux.
/// </summary>
public class ProfileTests
{
    private static readonly CultureInfo En = CultureInfo.GetCultureInfo("en-US");

    private static UiText Status(AgentSnapshot model, string id = "mac") => DeviceText.Status(model, id, Sample.Now, En);

    [Fact]
    public void NamesNetworksByTheirNameElseTheirRouter()
    {
        var wifi = ProfileSample.OfficeWifi;
        Assert.Equal(new UiText(TextKeys.Verbatim, "Office-5G"), Profiles.NetworkTitle(wifi));
        Assert.Equal(new UiText(TextKeys.ProfilesRouter, "172.16.0.1"), Profiles.NetworkDetail(wifi));
        Assert.Equal(Profiles.WifiGlyph, Profiles.NetworkGlyph(wifi));
        // Without a name: its router, and no line under it.
        var home = ProfileSample.Home;
        Assert.Equal(new UiText(TextKeys.ProfilesRouter, "192.168.1.1"), Profiles.NetworkTitle(home));
        Assert.Null(Profiles.NetworkDetail(home));
        Assert.Equal(Profiles.WiredGlyph, Profiles.NetworkGlyph(home));
        // A VPN without either: "VPN".
        var vpn = new Network { Id = "n-vpn", Kind = NetworkKind.Vpn };
        Assert.Equal(new UiText(TextKeys.ProfilesVpn), Profiles.NetworkTitle(vpn));
        Assert.Null(Profiles.NetworkDetail(vpn));
        Assert.Equal(Profiles.VpnGlyph, Profiles.NetworkGlyph(vpn));
        // Neither a name nor a router: its kind.
        Assert.Equal(new UiText(TextKeys.ProfilesWiredNetwork), Profiles.NetworkTitle(new Network { Kind = NetworkKind.Wired }));
        Assert.Equal(new UiText(TextKeys.ProfilesWifiNetwork), Profiles.NetworkTitle(new Network { Kind = NetworkKind.Wifi }));
        Assert.Equal(new UiText(TextKeys.ProfilesWiredNetwork), Profiles.NetworkTitle(new Network()));
        Assert.Null(Profiles.NetworkDetail(new Network { Kind = NetworkKind.Wifi }));
        // A VPN with a name or a router goes by them.
        Assert.Equal(new UiText(TextKeys.ProfilesRouter, "198.51.100.1"), Profiles.NetworkTitle(new Network { Kind = NetworkKind.Vpn, Router = "198.51.100.1" }));
        Assert.Equal(new UiText(TextKeys.Verbatim, "Work VPN"), Profiles.NetworkTitle(new Network { Kind = NetworkKind.Vpn, Name = "Work VPN" }));
        Assert.Null(Profiles.NetworkDetail(new Network { Kind = NetworkKind.Vpn, Name = "Work VPN" }));

        Assert.Equal(new UiText(TextKeys.ProfilesCurrentNetwork, new UiText(TextKeys.ProfilesRouter, "192.168.1.1")), Profiles.CurrentNetwork(ProfileSample.Model()));
        Assert.Equal(new UiText(TextKeys.ProfilesNoNetwork), Profiles.CurrentNetwork(ProfileSample.Model(noNetwork: true)));
    }

    [Fact]
    public void NamesTheFixedProfilesItself()
    {
        var model = ProfileSample.Model();
        var (home, office, other) = (model.Status.Profiles[0], model.Status.Profiles[1], model.Status.Profiles[2]);
        Assert.Equal(new UiText(TextKeys.ProfileDefault), Profiles.Name(home));
        Assert.Equal(new UiText(TextKeys.Verbatim, "Office"), Profiles.Name(office));
        Assert.Equal(new UiText(TextKeys.ProfileOtherNetworks), Profiles.Name(other));
        Assert.True(Profiles.Fixed(home));
        Assert.True(Profiles.Fixed(other));
        Assert.False(Profiles.Fixed(office));
    }

    [Fact]
    public void AutomaticNamesTheProfileTheNetworkPicks()
    {
        Assert.Equal(
            new UiText(TextKeys.ProfilesAutomaticNow, new UiText(TextKeys.ProfileDefault)),
            Profiles.AutomaticLabel(ProfileSample.Model()));
        Assert.Equal(
            new UiText(TextKeys.ProfilesAutomaticNow, new UiText(TextKeys.Verbatim, "Office")),
            Profiles.AutomaticLabel(ProfileSample.Model(inUse: "office")));
        Assert.Equal("", Profiles.Chosen(ProfileSample.Model(inUse: "office")));
        // A profile chosen: which one the network would pick is not known.
        Assert.Equal(new UiText(TextKeys.ProfilesAutomatic), Profiles.AutomaticLabel(ProfileSample.Model(inUse: "office", chosen: true)));
        Assert.Equal("office", Profiles.Chosen(ProfileSample.Model(inUse: "office", chosen: true)));
    }

    [Fact]
    public void ListsEachProfilesNetworksUnderIt()
    {
        var model = ProfileSample.Model();
        Assert.Equal(new UiText(TextKeys.ProfilesRouter, "192.168.1.1"), Profiles.Description(model.Status.Profiles[0]));
        Assert.Equal(new UiText(TextKeys.ProfileOtherNetworksHint), Profiles.Description(model.Status.Profiles[2]));
        // A paused one says so first, joined in a list.
        var paused = ProfileSample.Model(inUse: "office", paused: true, also: s => s.Profiles[1].Networks.Add(ProfileSample.Home));
        Assert.Equal(
            new UiText(
                TextKeys.ProfilesListJoin,
                new UiText(TextKeys.ProfilesListJoin, new UiText(TextKeys.ProfileSharingPaused), new UiText(TextKeys.Verbatim, "Office-5G")),
                new UiText(TextKeys.ProfilesRouter, "192.168.1.1")),
            Profiles.Description(paused.Status.Profiles[1]));
        var otherPaused = ProfileSample.Model(inUse: "other", paused: true);
        Assert.Equal(
            new UiText(TextKeys.ProfilesListJoin, new UiText(TextKeys.ProfileSharingPaused), new UiText(TextKeys.ProfileOtherNetworksHint)),
            Profiles.Description(otherPaused.Status.Profiles[2]));
        // None, while it shares and has no networks.
        Assert.Null(Profiles.Description(new Profile { Id = "new", Name = "New Profile" }));

        Assert.True(Profiles.IsInUse(model, model.Status.Profiles[0]));
        Assert.False(Profiles.IsInUse(model, model.Status.Profiles[1]));
    }

    [Fact]
    public void AddsTheNetworkToAnyProfileButOtherNetworks()
    {
        var model = ProfileSample.Model();
        Assert.Equal(
            [
                new ProfileTarget("default", new UiText(TextKeys.ProfileDefault), true),
                new ProfileTarget("office", new UiText(TextKeys.Verbatim, "Office"), false),
            ],
            Profiles.AddTargets(model));
        Assert.False(Profiles.CanAddCurrentNetwork(model, model.Status.Profiles[0]));
        Assert.True(Profiles.CanAddCurrentNetwork(model, model.Status.Profiles[1]));
        Assert.False(Profiles.CanAddCurrentNetwork(model, model.Status.Profiles[2]));
        // Without a network, or the service, nothing to add.
        var none = ProfileSample.Model(noNetwork: true);
        Assert.False(Profiles.CanAddCurrentNetwork(none, none.Status.Profiles[1]));
        Assert.All(Profiles.AddTargets(none), t => Assert.False(t.Checked));
        var away = ProfileSample.Model(link: AgentLink.Connecting);
        Assert.False(Profiles.CanAddCurrentNetwork(away, away.Status.Profiles[1]));
        Assert.False(Profiles.CanAddNetwork(away));
        Assert.Empty(Profiles.AddTargets(ProfileSample.Model(supported: false)));
    }

    [Fact]
    public void AddsNoNetworkKnownOnlyByItsRoutersAddress()
    {
        Assert.True(Profiles.CanAddNetwork(ProfileSample.Model()));
        // For a moment after joining it, the network is not resolved: nothing
        // adds it, here or in the panel.
        var joining = ProfileSample.Model(also: s => s.Network.Resolved = false);
        Assert.False(Profiles.CanAddNetwork(joining));
        Assert.False(Profiles.CanAddCurrentNetwork(joining, joining.Status.Profiles[1]));
        var cafe = ProfileSample.Model(
            inUse: "other", paused: true,
            network: new Network { Id = "n-cafe", Kind = NetworkKind.Wifi, Router = "198.51.100.1" });
        Assert.True(Profiles.NetworkInNoProfile(cafe));
        Assert.False(Profiles.CanAddNetwork(cafe));
        Assert.False(Profiles.CanAddNetwork(ProfileSample.Model(noNetwork: true)));
    }

    [Fact]
    public void KnowsWhichComputersAProfileConnects()
    {
        var model = ProfileSample.Model(macLeftOut: true);
        Assert.Equal([new ProfileComputer("mac", "MacBook Pro", false)], Profiles.Computers(model, model.Status.Profiles[0]));
        Assert.Equal([new ProfileComputer("mac", "MacBook Pro", true)], Profiles.Computers(model, model.Status.Profiles[1]));
    }

    [Fact]
    public void RenamesOnlyTheUsersOwnAndNeverToNothing()
    {
        var model = ProfileSample.Model();
        var office = model.Status.Profiles[1];
        Assert.Equal("Work", Profiles.Rename(office, "  Work "));
        Assert.Null(Profiles.Rename(office, "Office"));
        Assert.Null(Profiles.Rename(office, "   "));
        Assert.Null(Profiles.Rename(model.Status.Profiles[0], "Home"));
        Assert.Null(Profiles.Rename(model.Status.Profiles[2], "Elsewhere"));
    }

    [Fact]
    public void SaysWhenAnotherProfileHasTheName()
    {
        Assert.Equal(new UiText(TextKeys.ProfileNameTaken), Profiles.RenameRefused("name_taken"));
        // Other refusals only put the name back.
        Assert.Null(Profiles.RenameRefused("invalid_name"));
        Assert.Null(Profiles.RenameRefused("fixed_profile"));
        Assert.Null(Profiles.RenameRefused("unknown_profile"));
    }

    [Fact]
    public void FindsTheProfileJustAdded()
    {
        var after = ProfileSample.Model().Status.Clone();
        after.Profiles.Insert(2, new Profile { Id = "p9", Name = "New Profile" });
        Assert.Equal("p9", Profiles.Added(["default", "office", "other"], after));
        Assert.Null(Profiles.Added(["default", "office", "other", "p9"], after));
    }

    [Fact]
    public void SaysNotInThisProfileAfterOfflineForNow()
    {
        Assert.Equal(new UiText(TextKeys.ConnectionNotInProfile), Status(ProfileSample.Model(macLeftOut: true)));
        Assert.True(Profiles.NotInProfile(ProfileSample.Model(macLeftOut: true), "mac"));
        // Offline for now comes first.
        var both = ProfileSample.Model(macLeftOut: true, also: s =>
        {
            s.Devices[0].OfflineForNow = true;
            s.Devices[0].OfflineByDeviceId = "pc";
        });
        Assert.Equal(new UiText(TextKeys.ConnectionOfflineForNow), Status(both));
        Assert.False(Profiles.NotInProfile(both, "mac"));
        // Before what would say it can be woken.
        var asleep = ProfileSample.Model(macLeftOut: true, also: s =>
        {
            s.WakeSupported = true;
            s.Devices[0].CanWake = true;
            s.Devices[0].OfflineSinceUnixMs = 1;
        });
        Assert.Equal(new UiText(TextKeys.ConnectionNotInProfile), Status(asleep));
        // While the profile in use is paused, every other one is just offline.
        Assert.Equal(new UiText(TextKeys.ConnectionOffline), Status(ProfileSample.Model(macLeftOut: true, paused: true)));
        // This computer is this computer.
        Assert.Equal(new UiText(TextKeys.ConnectionThisComputer), Status(ProfileSample.Model(paused: true), "pc"));
        // A service from before says nothing of it.
        var before = ProfileSample.Model(macLeftOut: true, supported: false);
        Assert.False(Profiles.NotInProfile(before, "mac"));
        Assert.Equal(new UiText(TextKeys.ConnectionOffline), Status(before));
    }

    [Fact]
    public void OnlyRenamingIsLeftForAComputerCutOff()
    {
        var leftOut = ProfileSample.Model(macLeftOut: true, also: s =>
        {
            s.WakeSupported = true;
            s.Devices[0].CanWake = true;
            s.Devices[0].Wake = WakeState.NotWoken;
        });
        Assert.True(Profiles.OnlyRename(leftOut, "mac"));
        Assert.False(Wake.Offered(leftOut, "mac"));
        Assert.Equal(WakeNote.None, Wake.Note(leftOut, "mac"));
        Assert.False(OfflineForNow.Offered(leftOut, "mac"));
        Assert.False(leftOut.CanJump("mac"));
        // Every other one while the profile in use is paused; this one keeps its menu.
        var paused = ProfileSample.Model(paused: true, also: s => s.WakeSupported = true);
        Assert.True(Profiles.OnlyRename(paused, "mac"));
        Assert.False(Profiles.OnlyRename(paused, "pc"));
        Assert.False(OfflineForNow.Offered(paused, "mac"));
        Assert.Equal(WakeNote.None, Wake.Note(paused, "mac"));
        // A profile that connects it changes nothing.
        var connected = ProfileSample.Model();
        Assert.False(Profiles.OnlyRename(connected, "mac"));
        Assert.True(OfflineForNow.Offered(connected, "mac"));
        Assert.True(connected.CanJump("mac"));
        // Plainly offline in a profile that connects it, it still says how to wake it.
        Assert.Equal(WakeNote.NotTold, Wake.Note(ProfileSample.Model(macOnline: false, also: s => s.WakeSupported = true), "mac"));
    }

    [Fact]
    public void ProblemsLeaveOutComputersTheProfileDoesNotConnect()
    {
        var unreachable = new ExceptionRow(new UiText(TextKeys.ExceptionUnreachable, "MacBook Pro"), ExceptionAction.Troubleshoot);
        Assert.Equal([unreachable], ExceptionRows.For(ProfileSample.Model(macOnline: false), ServiceState.Running));
        Assert.Empty(ExceptionRows.For(ProfileSample.Model(macLeftOut: true), ServiceState.Running));
        // While the profile in use is paused, none of them is a problem.
        Assert.Empty(ExceptionRows.For(ProfileSample.Model(paused: true), ServiceState.Running));
        Assert.Empty(ExceptionRows.For(ProfileSample.Model(paused: true), ServiceState.Running, compact: false));
        Assert.True(ExceptionRows.EverythingWorks(ProfileSample.Model(macLeftOut: true, also: s => s.Capturing = true), ServiceState.Running));
        // From a service from before, it counts.
        Assert.Equal(
            [unreachable],
            ExceptionRows.For(ProfileSample.Model(macLeftOut: true, supported: false), ServiceState.Running));
    }

    [Fact]
    public void FilesSayWhyTheyCannotGo()
    {
        // Not in this profile, after offline for now and before offline.
        var device = new DeviceStatus { Id = "a", State = ConnectionState.Offline, NotInProfile = true, FilesOff = true };
        Assert.Equal(SendUnavailable.NotInProfile, SendTargets.Unavailable(device));
        device.OfflineForNow = true;
        Assert.Equal(SendUnavailable.OfflineForNow, SendTargets.Unavailable(device));
        Assert.Equal(new UiText(TextKeys.ConnectionNotInProfile), SendTargets.Reason(SendUnavailable.NotInProfile));
        // The profile in use with files off: no connected computer takes them.
        var connected = new DeviceStatus { Id = "a", State = ConnectionState.Connected };
        Assert.Null(SendTargets.Unavailable(connected));
        Assert.Equal(SendUnavailable.FilesOff, SendTargets.Unavailable(connected, profileFilesOff: true));
        Assert.Equal(SendUnavailable.Offline, SendTargets.Unavailable(new DeviceStatus { State = ConnectionState.Offline }, profileFilesOff: true));

        var filesOff = ProfileSample.Model(files: false);
        Assert.Empty(filesOff.FileReceivers);
        Assert.False(filesOff.CanReceiveFiles("mac"));
        Assert.IsType<SendChoice.Ask>(SendTargets.Choose(filesOff));
        Assert.Equal(
            [new SendTarget("mac", "MacBook Pro", "", new UiText(TextKeys.SendReasonFilesOff), false)],
            SendTargets.Targets(filesOff));
        Assert.Equal(["mac"], ProfileSample.Model().FileReceivers.Select(d => d.Id));
        // Left out: offline, so nowhere to send.
        Assert.IsType<SendChoice.Nowhere>(SendTargets.Choose(ProfileSample.Model(macLeftOut: true)));
        Assert.Equal(
            [new SendTarget("mac", "MacBook Pro", "", new UiText(TextKeys.ConnectionNotInProfile), false)],
            SendTargets.Targets(ProfileSample.Model(macLeftOut: true)));

        // Dragged over the canvas.
        Assert.Equal(new UiText(TextKeys.ConnectionNotInProfile), DeviceText.CannotReceive(ProfileSample.Model(macLeftOut: true), "mac", compact: true));
        Assert.Equal(new UiText(TextKeys.DeviceFilesOffShort), DeviceText.CannotReceive(filesOff, "mac", compact: true));
        Assert.Equal(new UiText(TextKeys.SendReasonFilesOff), DeviceText.CannotReceive(filesOff, "mac", compact: false));
        Assert.Equal(new UiText(TextKeys.ConnectionOffline), DeviceText.CannotReceive(ProfileSample.Model(paused: true), "mac", compact: true));
    }

    [Fact]
    public void ThePanelTitlesAPausedProfileFirst()
    {
        UiText Title(AgentSnapshot model) => PanelText.Title(model, ServiceState.Running, Sample.Now);
        var paused = ProfileSample.Model(inUse: "other", paused: true, network: new Network { Id = "n-cafe", Kind = NetworkKind.Wifi, Router = "198.51.100.1" });
        Assert.Equal(new UiText(TextKeys.TitleProfilePaused, new UiText(TextKeys.ProfileOtherNetworks)), Title(paused));
        // The network picked it, as it is in no profile: say so, and offer to add it.
        Assert.True(Profiles.NetworkInNoProfile(paused));
        Assert.Equal(new UiText(TextKeys.SubtitleNetworkInNoProfile), PanelText.Subtitle(paused));
        // Before this computer offline for now, and before sharing paused.
        var offlineToo = WakeTests.With(paused, s =>
        {
            s.OfflineForNow = true;
            s.OfflineByDeviceId = "pc";
            s.Sharing = false;
        });
        Assert.Equal(new UiText(TextKeys.TitleProfilePaused, new UiText(TextKeys.ProfileOtherNetworks)), Title(offlineToo));
        Assert.Equal(new UiText(TextKeys.SubtitleNetworkInNoProfile), PanelText.Subtitle(offlineToo));
        // Chosen by the user, or a profile of the user's own: only the title.
        var chosen = ProfileSample.Model(inUse: "other", paused: true, chosen: true);
        Assert.False(Profiles.NetworkInNoProfile(chosen));
        Assert.Null(PanelText.Subtitle(chosen));
        var office = ProfileSample.Model(inUse: "office", paused: true);
        Assert.Equal(new UiText(TextKeys.TitleProfilePaused, new UiText(TextKeys.Verbatim, "Office")), Title(office));
        Assert.Null(PanelText.Subtitle(office));
        // Without a network, there is none to add.
        Assert.Null(PanelText.Subtitle(ProfileSample.Model(inUse: "other", paused: true, noNetwork: true)));
        // A profile that shares: the usual title.
        Assert.Equal(new UiText(TextKeys.TitleSharing), Title(ProfileSample.Model(inUse: "other")));
        Assert.Equal(new UiText(TextKeys.SubtitleCursorOn, "Windows Desktop"), PanelText.Subtitle(ProfileSample.Model(inUse: "other")));
        Assert.False(Profiles.NetworkInNoProfile(ProfileSample.Model(inUse: "other")));
    }

    [Fact]
    public void TheCapsuleFollowsEveryTitleButThePausedProfiles()
    {
        Assert.Equal(new UiText(TextKeys.ProfileDefault), Profiles.Capsule(ProfileSample.Model()));
        Assert.Equal(new UiText(TextKeys.Verbatim, "Office"), Profiles.Capsule(ProfileSample.Model(inUse: "office", chosen: true)));
        // After "Sharing paused" and "This computer is offline for now" too.
        Assert.NotNull(Profiles.Capsule(ProfileSample.Model(also: s => s.Sharing = false)));
        Assert.NotNull(Profiles.Capsule(ProfileSample.Model(also: s => (s.OfflineForNow, s.OfflineByDeviceId) = (true, "pc"))));
        // Not after the paused profile's title, which names it.
        Assert.Null(Profiles.Capsule(ProfileSample.Model(paused: true)));
        // Nor while the service is out of reach, or from one from before.
        Assert.Null(Profiles.Capsule(ProfileSample.Model(link: AgentLink.Connecting)));
        Assert.Null(Profiles.Capsule(ProfileSample.Model(supported: false)));
    }

    [Fact]
    public void TheMenuHasTheProfileSubmenuAndNoPausingInAPausedProfile()
    {
        var menu = MenuState.For(ProfileSample.Model(inUse: "office"));
        Assert.Equal(
            new ProfileMenu(
                [
                    new ProfileChoice("", new UiText(TextKeys.ProfilesAutomatic), true),
                    new ProfileChoice("default", new UiText(TextKeys.ProfileDefault), false),
                    new ProfileChoice("office", new UiText(TextKeys.Verbatim, "Office"), false),
                    new ProfileChoice("other", new UiText(TextKeys.ProfileOtherNetworks), false),
                ],
                true),
            menu.Profiles);
        Assert.True(menu.PauseEnabled);
        Assert.True(menu.LockEnabled);
        // The one chosen is checked, not Automatic.
        var chosen = MenuState.For(ProfileSample.Model(inUse: "office", chosen: true)).Profiles!;
        Assert.Equal([false, false, true, false], chosen.Choices.Select(c => c.Checked));
        // In a paused profile, sharing comes back by changing the profile.
        var paused = MenuState.For(ProfileSample.Model(paused: true));
        Assert.Equal(TextKeys.MenuPauseSharing, paused.PauseKey);
        Assert.False(paused.PauseEnabled);
        Assert.False(paused.LockEnabled);
        Assert.True(paused.Profiles!.Enabled);
        // Out of reach, nothing can be chosen; from a service from before, no submenu.
        Assert.False(MenuState.For(ProfileSample.Model(link: AgentLink.Connecting)).Profiles!.Enabled);
        Assert.Null(MenuState.For(ProfileSample.Model(supported: false)).Profiles);
        // The same menu twice is equal, so it is not built again.
        Assert.Equal(MenuState.For(ProfileSample.Model()), MenuState.For(ProfileSample.Model()));
    }

    [Fact]
    public void TheTrayIconIsPausedInAPausedProfile()
    {
        var paused = ProfileSample.Model(paused: true);
        Assert.Equal(TrayIconState.Paused, TrayIcons.StateFor(paused, ServiceState.Running, Sample.Now));
        Assert.Equal(new UiText(TextKeys.TrayPaused), TrayIcons.Description(TrayIconState.Paused, paused));
        // Also offline for now: the paused profile's title comes first.
        var offlineToo = WakeTests.With(paused, s => (s.OfflineForNow, s.OfflineByDeviceId) = (true, "pc"));
        Assert.Equal(new UiText(TextKeys.TrayPaused), TrayIcons.Description(TrayIconState.Paused, offlineToo));
        Assert.Equal(TrayIconState.Normal, TrayIcons.StateFor(ProfileSample.Model(macLeftOut: true), ServiceState.Running, Sample.Now));
    }

    [Fact]
    public void ReadsAsAgreedInBothLanguages()
    {
        var en = WakeTexts.Load("en-US");
        var zh = WakeTexts.Load("zh-TW");
        void Same(string english, string chinese, UiText text)
        {
            Assert.Equal(english, WakeTexts.Say(en, text));
            Assert.Equal(chinese, WakeTexts.Say(zh, text));
        }
        Same("Not in this profile", "不在這個情境", Status(ProfileSample.Model(macLeftOut: true)));
        Same("Automatic (now: Default)", "自動（目前：預設）", Profiles.AutomaticLabel(ProfileSample.Model()));
        Same("Automatic", "自動", Profiles.AutomaticLabel(ProfileSample.Model(inUse: "office", chosen: true)));
        Same("Current network: Office-5G", "目前的網路：Office-5G", Profiles.CurrentNetwork(ProfileSample.Model(network: ProfileSample.OfficeWifi)));
        Same("Router 172.16.0.1", "路由器 172.16.0.1", Profiles.NetworkDetail(ProfileSample.OfficeWifi)!);
        Same("Current network: Router 192.168.1.1", "目前的網路：路由器 192.168.1.1", Profiles.CurrentNetwork(ProfileSample.Model()));
        Same("Not connected to a network.", "沒有連上網路。", Profiles.CurrentNetwork(ProfileSample.Model(noNetwork: true)));
        Same("VPN", "VPN", Profiles.NetworkTitle(new Network { Kind = NetworkKind.Vpn }));
        Same("Wired network", "有線網路", Profiles.NetworkTitle(new Network { Kind = NetworkKind.Wired }));
        Same("Wi-Fi network", "Wi-Fi 網路", Profiles.NetworkTitle(new Network { Kind = NetworkKind.Wifi }));
        var paused = ProfileSample.Model(inUse: "office", paused: true, also: s => s.Profiles[1].Networks.Add(ProfileSample.Home));
        Same(
            "Sharing paused, Office-5G, Router 192.168.1.1",
            "鍵鼠共享已暫停，Office-5G，路由器 192.168.1.1",
            Profiles.Description(paused.Status.Profiles[1])!);
        Same("Networks not added to any profile use this one.", "沒有加到任何情境的網路都用這個情境。", Profiles.Description(paused.Status.Profiles[2])!);
        Same("Other networks: sharing paused", "其他網路：已暫停共享", PanelText.Title(ProfileSample.Model(inUse: "other", paused: true), ServiceState.Running, Sample.Now));
        Same("This network isn’t in any profile.", "這個網路沒有加到任何情境。", new UiText(TextKeys.SubtitleNetworkInNoProfile));
        Same("Add This Network To…", "把這個網路加到…", new UiText(TextKeys.PanelAddNetworkTo));
        Same("Switched to Office.", "已切換到「Office」。", new UiText(TextKeys.HudProfileSwitched, new UiText(TextKeys.Verbatim, "Office")));
        Same(
            "Switched to Other networks. Sharing is paused.",
            "已切換到「其他網路」，鍵鼠共享已暫停。",
            new UiText(TextKeys.HudProfileSwitchedPaused, new UiText(TextKeys.ProfileOtherNetworks)));
        Same("Profiles", "情境", new UiText(TextKeys.NavProfiles));
        Same("Profile", "情境", new UiText(TextKeys.MenuProfile));
        Same("Manage Profiles…", "管理情境…", new UiText(TextKeys.MenuManageProfiles));
        Same("Profile in use", "使用的情境", new UiText(TextKeys.ProfilesInUse));
        Same(
            "Automatic picks the profile from the network you’re on. A profile you choose stays until you choose Automatic again.",
            "自動時，依所在的網路選情境。手動選的情境不會因為換網路而改變。",
            new UiText(TextKeys.ProfilesInUseHint));
        Same("Add to Profile", "加到情境", new UiText(TextKeys.ProfilesAddTo));
        Same("All profiles", "所有情境", new UiText(TextKeys.ProfilesAll));
        Same("Add Profile", "新增情境", new UiText(TextKeys.ProfilesAdd));
        Same("In use", "使用中", new UiText(TextKeys.ProfilesInUseBadge));
        Same("New Profile", "新的情境", new UiText(TextKeys.ProfileNewName));
        Same("Default", "預設", new UiText(TextKeys.ProfileDefault));
        Same("Other networks", "其他網路", new UiText(TextKeys.ProfileOtherNetworks));
        Same("Name", "名稱", new UiText(TextKeys.ProfileName));
        Same("Another profile already has this name.", "已經有同名的情境。", Profiles.RenameRefused("name_taken")!);
        Same("Sharing", "鍵鼠共享", new UiText(TextKeys.ProfileSharing));
        Same("Share", "共享", new UiText(TextKeys.ProfileShare));
        Same("Pause", "暫停", new UiText(TextKeys.ProfilePause));
        Same(
            "In this profile this computer doesn’t show up on the network or connect to any computer. Pairings are kept.",
            "這個情境下，這台不會出現在網路上，也不連線任何電腦；配對都會保留。",
            new UiText(TextKeys.ProfilePausedHint));
        Same("Computers to connect", "要連的電腦", new UiText(TextKeys.ProfileComputers));
        Same(
            "Unchecked computers are treated as offline in this profile, and they see this computer as offline too.",
            "沒勾的電腦在這個情境下視同離線，它們也會看到這台離線。",
            new UiText(TextKeys.ProfileComputersHint));
        Same("Clipboard sync", "剪貼簿同步", new UiText(TextKeys.ProfileClipboard));
        Same("Send and receive files", "互傳檔案", new UiText(TextKeys.ProfileFiles));
        Same(
            "If clipboard sync is off on the General page, no profile syncs the clipboard.",
            "一般頁關掉剪貼簿同步時，所有情境都不同步。",
            new UiText(TextKeys.ProfileClipboardHint));
        Same("Networks", "網路", new UiText(TextKeys.ProfileNetworks));
        Same("No networks in this profile yet.", "這個情境還沒有網路。", new UiText(TextKeys.ProfileNoNetworks));
        Same("Add Current Network", "把目前的網路加到這個情境", new UiText(TextKeys.ProfileAddCurrentNetwork));
        Same("Remove Office-5G", "移除 Office-5G", new UiText(TextKeys.ProfileRemoveNetwork, "Office-5G"));
        Same("Remove Profile", "移除情境", new UiText(TextKeys.ProfileRemove));
        Same("Also recognize networks by Wi-Fi name", "也以 Wi-Fi 名稱辨認網路", new UiText(TextKeys.ProfilesWifiNames));
        Same(
            "InputWeave normally recognizes a network by its router. Turn this on to match Wi-Fi names too; Windows asks for location access, used only to read the Wi-Fi name.",
            "InputWeave 平常以路由器辨認網路。打開後也比對 Wi-Fi 名稱；Windows 會詢問定位權限，只用來讀 Wi-Fi 名稱。",
            new UiText(TextKeys.ProfilesWifiNamesHint));
    }
}

/// <summary>The HUD as the profile in use changes by itself.</summary>
public class ProfileHudTests
{
    private static UiText? Update(HudWatch watch, AgentSnapshot model)
    {
        int hints = HudWatch.HintTimes;
        return watch.Update(model, ref hints);
    }

    private static AgentSnapshot Switched(ulong switches, string inUse = "office", bool paused = false, Action<Status>? also = null) =>
        ProfileSample.Model(inUse: inUse, paused: paused, also: s =>
        {
            s.ProfileSwitches = switches;
            also?.Invoke(s);
        });

    [Fact]
    public void SaysWhichProfileItSwitchedTo()
    {
        var watch = new HudWatch();
        Assert.Null(Update(watch, Switched(0, inUse: "default")));
        Assert.Equal(new UiText(TextKeys.HudProfileSwitched, new UiText(TextKeys.Verbatim, "Office")), Update(watch, Switched(1)));
        // The same count again is no news.
        Assert.Null(Update(watch, Switched(1)));
        // To a paused profile: sharing is paused too.
        Assert.Equal(
            new UiText(TextKeys.HudProfileSwitchedPaused, new UiText(TextKeys.ProfileOtherNetworks)),
            Update(watch, Switched(2, inUse: "other", paused: true)));
    }

    [Fact]
    public void TheFirstStateOnlyRecords()
    {
        var watch = new HudWatch();
        Assert.Null(Update(watch, Switched(3)));
        Assert.NotNull(Update(watch, Switched(4)));
        // The service reached again: what it says first is no news.
        Assert.Null(Update(watch, ProfileSample.Model(link: AgentLink.Connecting)));
        Assert.Null(Update(watch, Switched(5)));
        Assert.NotNull(Update(watch, Switched(6)));
        // Started again without the app noticing: a lower count only records.
        Assert.Null(Update(watch, Switched(1)));
        Assert.NotNull(Update(watch, Switched(2)));
    }

    [Fact]
    public void AServiceFromBeforeSaysNothing()
    {
        var watch = new HudWatch();
        Update(watch, WakeTests.With(Switched(0), s => s.ProfilesSupported = false));
        Assert.Null(Update(watch, WakeTests.With(Switched(1), s => s.ProfilesSupported = false)));
    }

    [Fact]
    public void TheKeyboardAndMouseComingBackAsTheProfileChangedSayOnlyTheSwitch()
    {
        var watch = new HudWatch();
        Update(watch, Switched(0, inUse: "default", also: s => s.ActiveDeviceId = "mac"));
        var back = Switched(1, inUse: "other", paused: true, also: s =>
        {
            s.ControlReturns = 1;
            s.LastReturn = ReturnReason.ProfileChanged;
            s.LastReturnDeviceId = "mac";
        });
        Assert.Equal(new UiText(TextKeys.HudProfileSwitchedPaused, new UiText(TextKeys.ProfileOtherNetworks)), Update(watch, back));
        Assert.Null(Update(watch, back));
        // Chosen by the user, the service counts no switch: nothing at all.
        var chosen = Switched(1, inUse: "office", also: s =>
        {
            s.ProfileChosen = true;
            s.ControlReturns = 2;
            s.LastReturn = ReturnReason.ProfileChanged;
            s.LastReturnDeviceId = "mac";
        });
        Assert.Null(Update(watch, chosen));
    }
}

/// <summary>When the app reads and reports the Wi-Fi name, and turns the setting off.</summary>
public class WifiNameTests
{
    private static readonly DateTimeOffset Now = Sample.Now;

    private static AgentSnapshot On(
        Network? network = null, ulong session = 1, bool wifiNames = true, bool supported = true, WifiNameHeld? held = null) =>
        ProfileSample.Model(network: network ?? ProfileSample.OfficeWifi, supported: supported, also: s =>
        {
            s.WifiNames = wifiNames;
            s.WifiNameHeld = held;
        }) with { Session = session };

    private static WifiNameHeld Held(string network = "n-office", string ssid = "Office-5G") =>
        new() { NetworkId = network, Ssid = ssid, InUse = true };

    /// <summary>Sends what is due, as the app does, and returns it.</summary>
    private static ReportWifiName? Send(WifiNames names, AgentSnapshot model, DateTimeOffset at)
    {
        var report = names.Due(model, at);
        if (report is not null)
        {
            names.Sent(model, report, at);
        }
        return report;
    }

    private static ReportWifiName Report(string network, string ssid) => new() { NetworkId = network, Ssid = ssid };

    [Fact]
    public void OnlyADenialIsADenial()
    {
        Assert.Equal(WifiAccess.Granted, WifiNames.Access("Allowed"));
        Assert.Equal(WifiAccess.NotAsked, WifiNames.Access("UserPromptRequired"));
        Assert.Equal(WifiAccess.Denied, WifiNames.Access("DeniedByUser"));
        Assert.Equal(WifiAccess.Denied, WifiNames.Access("DeniedBySystem"));
        // What else Windows may say, as of an app without package identity:
        // not known, so it never turns the setting off.
        Assert.Equal(WifiAccess.Unknown, WifiNames.Access("NotDeclaredByApp"));
        Assert.Equal(WifiAccess.Unknown, WifiNames.Access("Unspecified"));
        Assert.Equal(WifiAccess.Unknown, WifiNames.Access("SomethingNew"));
        Assert.Equal(WifiStep.Wait, new WifiNames().Next(On(), WifiNames.Access("NotDeclaredByApp")));
    }

    [Fact]
    public void AFailedReadIsNotBeingOffWifi()
    {
        Assert.Equal("Office-5G", WifiNames.Outcome(true, "Office-5G"));
        // Not reaching the internet over Wi-Fi: no name.
        Assert.Equal("", WifiNames.Outcome(false, null));
        Assert.Equal("", WifiNames.Outcome(false, "Office-5G"));
        // On Wi-Fi without a name, or no answer at all: failed.
        Assert.Null(WifiNames.Outcome(true, ""));
        Assert.Null(WifiNames.Outcome(true, null));
        Assert.Null(WifiNames.Outcome(null, null));

        // A failed read keeps the last reading: nothing new to tell.
        var names = new WifiNames();
        var model = On();
        names.Read("n-office", "Office-5G");
        Assert.Equal(Report("n-office", "Office-5G"), Send(names, model, Now));
        names.Read("n-office", null);
        Assert.Null(Send(names, model, Now.AddSeconds(1)));
        // Nor is a failed first read told as being off Wi-Fi.
        var fresh = new WifiNames();
        fresh.Read("n-office", null);
        Assert.Null(fresh.Due(model, Now));
        // Truly off Wi-Fi: told.
        fresh.Read("n-office", "");
        Assert.Equal(Report("n-office", ""), fresh.Due(model, Now));
    }

    [Fact]
    public void TheSwitchShowsTheServiceUnlessAsking()
    {
        var names = new WifiNames();
        Assert.False(names.SwitchOn(On(wifiNames: false)));
        Assert.True(names.SwitchOn(On()));
        names.Ask();
        Assert.True(names.Asking);
        Assert.True(names.SwitchOn(On(wifiNames: false)));
    }

    [Fact]
    public void TurnsTheSettingOnOnlyOnceAccessIsGranted()
    {
        var names = new WifiNames();
        names.Ask();
        Assert.True(names.Answered(WifiAccess.Granted, On(wifiNames: false)));
        Assert.False(names.Asking);
        // Denied, or not decided: the switch goes back off, nothing sent.
        names.Ask();
        Assert.Null(names.Answered(WifiAccess.Denied, On(wifiNames: false)));
        Assert.False(names.SwitchOn(On(wifiNames: false)));
        names.Ask();
        Assert.Null(names.Answered(WifiAccess.NotAsked, On(wifiNames: false)));
        names.Ask();
        Assert.Null(names.Answered(WifiAccess.Unknown, On(wifiNames: false)));
        // Asked with the setting on: granted, nothing to send; denied, off.
        names.Ask();
        Assert.Null(names.Answered(WifiAccess.Granted, On()));
        names.Ask();
        Assert.False(names.Answered(WifiAccess.Denied, On()));
        // Turned off for it already on this connection: not again.
        Assert.Equal(WifiStep.Wait, names.Next(On(), WifiAccess.Denied));
        // Not decided, or Windows could not say: kept on.
        names.Ask();
        Assert.Null(names.Answered(WifiAccess.NotAsked, On(session: 2)));
        names.Ask();
        Assert.Null(names.Answered(WifiAccess.Unknown, On(session: 2)));
    }

    [Fact]
    public void DecidesWhatToDoFromTheSwitchAndAccess()
    {
        // The switch on: read once allowed, turn off once denied, ask once
        // while not decided, wait while Windows could not say.
        Assert.Equal(WifiStep.Read, WifiNames.Step(true, WifiAccess.Granted, askedThisLaunch: false));
        Assert.Equal(WifiStep.Read, WifiNames.Step(true, WifiAccess.Granted, askedThisLaunch: true));
        Assert.Equal(WifiStep.TurnOff, WifiNames.Step(true, WifiAccess.Denied, askedThisLaunch: false));
        Assert.Equal(WifiStep.TurnOff, WifiNames.Step(true, WifiAccess.Denied, askedThisLaunch: true));
        Assert.Equal(WifiStep.Ask, WifiNames.Step(true, WifiAccess.NotAsked, askedThisLaunch: false));
        Assert.Equal(WifiStep.Wait, WifiNames.Step(true, WifiAccess.NotAsked, askedThisLaunch: true));
        Assert.Equal(WifiStep.Wait, WifiNames.Step(true, WifiAccess.Unknown, askedThisLaunch: false));
        // The switch off: never ask, nor anything else.
        foreach (var access in Enum.GetValues<WifiAccess>())
        {
            Assert.Equal(WifiStep.Wait, WifiNames.Step(false, access, askedThisLaunch: false));
            Assert.Equal(WifiStep.Wait, WifiNames.Step(false, access, askedThisLaunch: true));
        }
    }

    [Fact]
    public void AsksOnceALaunchWhileTheSettingIsOnAndAccessNotDecided()
    {
        var names = new WifiNames();
        Assert.False(names.AskedThisLaunch);
        Assert.Equal(WifiStep.Ask, names.Next(On(), WifiAccess.NotAsked));
        names.Ask();
        Assert.True(names.AskedThisLaunch);
        // Not while asking.
        Assert.Equal(WifiStep.Wait, names.Next(On(), WifiAccess.NotAsked));
        // Still not decided: no second time in this launch, nor after a
        // reconnect; nothing read meanwhile.
        Assert.Null(names.Answered(WifiAccess.NotAsked, On()));
        Assert.Equal(WifiStep.Wait, names.Next(On(), WifiAccess.NotAsked));
        Assert.Equal(WifiStep.Wait, names.Next(On(session: 2), WifiAccess.NotAsked));
        Assert.False(names.ReadDue(On(), WifiAccess.NotAsked, Now));
        // Allowed later, in Settings: read.
        Assert.Equal(WifiStep.Read, names.Next(On(), WifiAccess.Granted));
        Assert.True(names.ReadDue(On(), WifiAccess.Granted, Now));
        // A new launch asks again.
        Assert.Equal(WifiStep.Ask, new WifiNames().Next(On(), WifiAccess.NotAsked));
    }

    [Fact]
    public void NeverAsksWhileTheSettingIsOff()
    {
        var names = new WifiNames();
        Assert.Equal(WifiStep.Wait, names.Next(On(wifiNames: false), WifiAccess.NotAsked));
        Assert.Equal(WifiStep.Wait, names.Next(On(wifiNames: false), WifiAccess.Denied));
        Assert.Equal(WifiStep.Wait, names.Next(On(wifiNames: false), WifiAccess.Granted));
        Assert.False(names.AskedThisLaunch);
        // Nor while the service is out of reach, or from one from before.
        Assert.Equal(WifiStep.Wait, names.Next(On() with { Link = AgentLink.Connecting }, WifiAccess.NotAsked));
        Assert.Equal(WifiStep.Wait, names.Next(On(supported: false), WifiAccess.NotAsked));
    }

    [Fact]
    public void ADenialAfterAskingTurnsTheSettingOff()
    {
        var names = new WifiNames();
        Assert.Equal(WifiStep.Ask, names.Next(On(), WifiAccess.NotAsked));
        names.Ask();
        Assert.False(names.Answered(WifiAccess.Denied, On()));
        Assert.False(names.SwitchOn(On(wifiNames: false)));
        // Once: the next steps wait, whatever Windows says.
        Assert.Equal(WifiStep.Wait, names.Next(On(), WifiAccess.Denied));
    }

    [Fact]
    public void ReadsEveryFiveSecondsAndAtOnceForAnotherNetworkOrConnection()
    {
        var names = new WifiNames();
        var model = On();
        Assert.True(names.ReadDue(model, WifiAccess.Granted, Now));
        names.Reading(model, Now);
        Assert.False(names.ReadDue(model, WifiAccess.Granted, Now.AddSeconds(4.9)));
        Assert.True(names.ReadDue(model, WifiAccess.Granted, Now.AddSeconds(5)));
        // Another network, or a new connection: at once.
        Assert.True(names.ReadDue(On(network: ProfileSample.Home), WifiAccess.Granted, Now.AddSeconds(1)));
        Assert.True(names.ReadDue(On(session: 2), WifiAccess.Granted, Now.AddSeconds(1)));
        // Not without access, with the setting off, without a network or the service.
        Assert.False(names.ReadDue(model, WifiAccess.Denied, Now.AddSeconds(10)));
        Assert.False(names.ReadDue(model, WifiAccess.NotAsked, Now.AddSeconds(10)));
        Assert.False(names.ReadDue(model, WifiAccess.Unknown, Now.AddSeconds(10)));
        Assert.False(names.ReadDue(On(wifiNames: false), WifiAccess.Granted, Now.AddSeconds(10)));
        Assert.False(names.ReadDue(ProfileSample.Model(noNetwork: true, also: s => s.WifiNames = true), WifiAccess.Granted, Now.AddSeconds(10)));
        Assert.False(names.ReadDue(On() with { Link = AgentLink.Connecting }, WifiAccess.Granted, Now.AddSeconds(10)));
        Assert.False(names.ReadDue(On(supported: false), WifiAccess.Granted, Now.AddSeconds(10)));
        // Nor while asking.
        names.Ask();
        Assert.False(names.ReadDue(model, WifiAccess.Granted, Now.AddSeconds(10)));
    }

    [Fact]
    public void TellsAChangedReadingAtOnce()
    {
        var names = new WifiNames();
        var model = On();
        names.Read("n-office", "Office-5G");
        Assert.Equal(Report("n-office", "Office-5G"), Send(names, model, Now));
        var held = On(held: Held());
        Assert.Null(Send(names, held, Now));
        // Another name, or none: at once, whatever is held.
        names.Read("n-office", "Office-5G Guest");
        Assert.Equal(Report("n-office", "Office-5G Guest"), Send(names, held, Now.AddSeconds(0.1)));
        names.Read("n-office", "");
        Assert.Equal(Report("n-office", ""), Send(names, held, Now.AddSeconds(0.2)));
        // A reading for another network than the status names: nothing.
        names.Read("n-home", "Home Wi-Fi");
        Assert.Null(Send(names, held, Now.AddSeconds(10)));
        // Once the status names it, at once.
        Assert.Equal(Report("n-home", "Home Wi-Fi"), Send(names, On(network: ProfileSample.Home), Now.AddSeconds(10.1)));
    }

    [Fact]
    public void ANewNetworkIsToldEvenUnderTheSameName()
    {
        var names = new WifiNames();
        names.Read("n-office", "Office-5G");
        Assert.NotNull(Send(names, On(held: Held()), Now));
        // The same Wi-Fi name on another network (a new router): told.
        var other = On(network: new Network { Id = "n-office-2", Name = "Office-5G", Kind = NetworkKind.Wifi, Router = "198.51.100.1" });
        Assert.True(names.ReadDue(other, WifiAccess.Granted, Now));
        names.Read("n-office-2", "Office-5G");
        Assert.Equal(Report("n-office-2", "Office-5G"), Send(names, other, Now.AddSeconds(0.1)));
    }

    [Fact]
    public void AReportTheServiceDidNotKeepIsSentAgainAfterFiveSeconds()
    {
        var names = new WifiNames();
        var model = On();
        names.Read("n-office", "Office-5G");
        Assert.NotNull(Send(names, model, Now));
        // The service holds none for it, as when the report came while it
        // waited to tell whether the network changed: again, but not before
        // five seconds, and then not again for five more.
        Assert.Null(Send(names, model, Now.AddSeconds(1)));
        Assert.Null(Send(names, model, Now.AddSeconds(4.9)));
        Assert.Equal(Report("n-office", "Office-5G"), Send(names, model, Now.AddSeconds(5)));
        for (int ms = 5100; ms < 10000; ms += 100)
        {
            Assert.Null(Send(names, model, Now.AddMilliseconds(ms)));
        }
        Assert.NotNull(Send(names, model, Now.AddSeconds(10)));
        // Held for another network: the same.
        var elsewhere = On(held: Held(network: "n-home", ssid: ""));
        Assert.Null(Send(names, elsewhere, Now.AddSeconds(14)));
        Assert.NotNull(Send(names, elsewhere, Now.AddSeconds(15)));
    }

    [Fact]
    public void NothingIsSentAgainWhileTheServiceHoldsANameForTheNetwork()
    {
        var names = new WifiNames();
        names.Read("n-office", "");
        Assert.NotNull(Send(names, On(), Now));
        // Held for the network, even another name (an empty report does not
        // replace a name in use): nothing until the reading changes.
        foreach (var held in new[] { On(held: Held()), On(held: Held(ssid: "")), On(held: new WifiNameHeld { NetworkId = "n-office" }) })
        {
            Assert.Null(Send(names, held, Now.AddSeconds(60)));
        }
        names.Read("n-office", "");
        Assert.Null(Send(names, On(held: Held()), Now.AddSeconds(120)));
        names.Read("n-office", "Office-5G");
        Assert.NotNull(Send(names, On(held: Held()), Now.AddSeconds(121)));
    }

    [Fact]
    public void TellsAgainAfterEveryConnectionAndOnceTheSettingIsBackOn()
    {
        var names = new WifiNames();
        var held = On(held: Held());
        names.Read("n-office", "Office-5G");
        Assert.NotNull(Send(names, held, Now));
        Assert.Null(Send(names, held, Now.AddSeconds(1)));
        // Reconnected: at once, held or not.
        Assert.NotNull(Send(names, On(session: 2, held: Held()), Now.AddSeconds(2)));
        Assert.Null(Send(names, On(session: 2, held: Held()), Now.AddSeconds(3)));

        // Turned off and on again, here or elsewhere, on the same network
        // and connection: read at once, and told again.
        var on = On(session: 2, held: Held());
        names.Reading(on, Now.AddSeconds(3));
        names.Update(On(session: 2, wifiNames: false));
        names.Update(on);
        Assert.Null(names.Due(on, Now.AddSeconds(4)));
        Assert.True(names.ReadDue(on, WifiAccess.Granted, Now.AddSeconds(4)));
        names.Read("n-office", "Office-5G");
        Assert.Equal(Report("n-office", "Office-5G"), Send(names, on, Now.AddSeconds(4)));
        // While it stays on, nothing told is forgotten.
        names.Update(on);
        Assert.Null(Send(names, on, Now.AddSeconds(5)));
    }

    [Fact]
    public void NothingIsToldWithoutTheSettingOrTheService()
    {
        var names = new WifiNames();
        names.Read("n-office", "Office-5G");
        Assert.Null(names.Due(On(wifiNames: false), Now));
        Assert.Null(names.Due(On() with { Link = AgentLink.Connecting }, Now));
        Assert.Null(names.Due(On(supported: false), Now));
        Assert.Null(names.Due(ProfileSample.Model(noNetwork: true, also: s => s.WifiNames = true) with { Session = 1 }, Now));
    }

    [Fact]
    public void OnlyAPlainDenialTurnsTheSettingOff()
    {
        var names = new WifiNames();
        var model = On();
        Assert.Equal(WifiStep.Read, names.Next(model, WifiAccess.Granted));
        // Windows could not say: wait, and ask Windows again next time.
        Assert.Equal(WifiStep.Wait, names.Next(model, WifiAccess.Unknown));
        Assert.Equal(WifiStep.TurnOff, names.Next(model, WifiAccess.Denied));
        names.TurnedOff(model);
        Assert.Equal(WifiStep.Wait, names.Next(model, WifiAccess.Denied));
        Assert.Equal(WifiStep.TurnOff, names.Next(On(session: 2), WifiAccess.Denied));
        // Not while the setting is off, while asking, out of reach, or from a service from before.
        Assert.Equal(WifiStep.Wait, names.Next(On(wifiNames: false, session: 3), WifiAccess.Denied));
        Assert.Equal(WifiStep.Wait, names.Next(On(session: 3) with { Link = AgentLink.Connecting }, WifiAccess.Denied));
        Assert.Equal(WifiStep.Wait, names.Next(On(session: 3, supported: false), WifiAccess.Denied));
        names.Ask();
        Assert.Equal(WifiStep.Wait, names.Next(On(session: 3), WifiAccess.Denied));
    }
}

/// <summary>The profile commands against a stand-in for the background service.</summary>
public class ProfileModelTests
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
    public async Task AddsEditsChoosesAndRemovesProfiles()
    {
        var agent = new FakeAgent();
        agent.UseProfiles();
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);
        Assert.True(model.Snapshot.ProfilesSupported);

        // Added before Other networks, under a name made unique.
        var first = await model.AddProfileAsync("New Profile");
        var second = await model.AddProfileAsync("New Profile");
        Assert.Equal("New Profile", agent.Received[^2].AddProfile.Name);
        var profiles = model.Snapshot.Status.Profiles;
        Assert.Equal(["default", "p1", first!, second!, "other"], profiles.Select(p => p.Id));
        Assert.Equal("New Profile 2", profiles.First(p => p.Id == second).Name);

        // Only what changed is set.
        await model.EditProfileAsync(new EditProfile { ProfileId = first!, Paused = true });
        var edit = agent.Received[^1].EditProfile;
        Assert.True(edit.HasPaused);
        Assert.False(edit.HasName || edit.HasClipboard || edit.HasFiles);
        var connect = new EditProfile { ProfileId = first! };
        connect.Devices.Add(new DeviceInProfile { DeviceId = "mac", Connect = false });
        await model.EditProfileAsync(connect);
        Assert.Equal(["mac"], model.Snapshot.Status.Profiles.First(p => p.Id == first).OffDeviceIds);
        Assert.Equal(
            "fixed_profile",
            (await Assert.ThrowsAsync<AgentRefusal>(() => model.EditProfileAsync(new EditProfile { ProfileId = "default", Name = "Home" }))).Code);
        // A name another profile has, or one of the fixed two's, whatever the case.
        foreach (var taken in new[] { "office", "New Profile 2", "default", "其他網路" })
        {
            var refusal = await Assert.ThrowsAsync<AgentRefusal>(() => model.EditProfileAsync(new EditProfile { ProfileId = first!, Name = taken }));
            Assert.Equal(TextKeys.ProfileNameTaken, Profiles.RenameRefused(refusal.Code)?.Key);
        }
        Assert.Equal("name_taken", (await Assert.ThrowsAsync<AgentRefusal>(() => model.AddProfileAsync("Other Networks"))).Code);
        // Its own name, in another case, is not taken.
        await model.EditProfileAsync(new EditProfile { ProfileId = first!, Name = "NEW PROFILE" });
        Assert.Equal("NEW PROFILE", model.Snapshot.Status.Profiles.First(p => p.Id == first).Name);

        // Chosen, then removed: back to Automatic.
        await model.SelectProfileAsync(first!);
        Assert.Equal((first, true), (model.Snapshot.Status.ProfileId, model.Snapshot.Status.ProfileChosen));
        Assert.Equal(first, Profiles.Chosen(model.Snapshot));
        await model.RemoveProfileAsync(first!);
        Assert.False(model.Snapshot.Status.ProfileChosen);
        Assert.Equal("", Profiles.Chosen(model.Snapshot));
        Assert.Equal("fixed_profile", (await Assert.ThrowsAsync<AgentRefusal>(() => model.RemoveProfileAsync("other"))).Code);
        await model.SelectProfileAsync("p1");
        await model.SelectProfileAsync("");
        Assert.Equal("", agent.Received[^1].SelectProfile.ProfileId);
        Assert.False(model.Snapshot.Status.ProfileChosen);
    }

    [Fact]
    public async Task MovesTheCurrentNetworkBetweenProfiles()
    {
        var agent = new FakeAgent();
        agent.UseProfiles();
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);
        var office = model.Snapshot.Status.Profiles[1];
        Assert.True(Profiles.CanAddCurrentNetwork(model.Snapshot, office));

        // Known only by its router's address, it can't be added yet.
        agent.Status = agent.Status.Clone();
        agent.Status.Network.Resolved = false;
        await agent.SendAsync(new Event { StatusChanged = agent.Status });
        await Eventually("an unresolved network", () => model.Snapshot.Status.Network?.Resolved == false);
        Assert.False(Profiles.CanAddCurrentNetwork(model.Snapshot, model.Snapshot.Status.Profiles[1]));
        Assert.Equal("network_unresolved", (await Assert.ThrowsAsync<AgentRefusal>(() => model.AddNetworkToProfileAsync("p1"))).Code);
        agent.Status = agent.Status.Clone();
        agent.Status.Network.Resolved = true;
        await agent.SendAsync(new Event { StatusChanged = agent.Status });
        await Eventually("a resolved network", () => model.Snapshot.Status.Network?.Resolved == true);

        await model.AddNetworkToProfileAsync("p1");
        Assert.Equal("p1", agent.Received[^1].AddNetworkToProfile.ProfileId);
        Assert.Empty(model.Snapshot.Status.Profiles[0].Networks);
        Assert.Equal(["n-office", "n-home"], model.Snapshot.Status.Profiles[1].Networks.Select(n => n.Id));
        Assert.False(Profiles.CanAddCurrentNetwork(model.Snapshot, model.Snapshot.Status.Profiles[1]));
        Assert.Equal(
            [false, true],
            Profiles.AddTargets(model.Snapshot).Select(t => t.Checked));
        Assert.Equal("other_networks", (await Assert.ThrowsAsync<AgentRefusal>(() => model.AddNetworkToProfileAsync("other"))).Code);

        await model.RemoveNetworkAsync("n-office");
        Assert.Equal("n-office", agent.Received[^1].RemoveNetwork.NetworkId);
        Assert.Equal(["n-home"], model.Snapshot.Status.Profiles[1].Networks.Select(n => n.Id));
        Assert.Equal("unknown_network", (await Assert.ThrowsAsync<AgentRefusal>(() => model.RemoveNetworkAsync("n-office"))).Code);
    }

    [Fact]
    public async Task TurnsWifiNamesOnAndReportsTheName()
    {
        var agent = new FakeAgent();
        agent.UseProfiles();
        await using var model = agent.Model();
        model.Start();
        await Eventually("a connection", () => model.Snapshot.Link == AgentLink.Connected);
        Assert.Equal(1ul, model.Snapshot.Session);

        await model.SetWifiNamesAsync(true);
        Assert.True(agent.Received[^1].SetWifiNames.On);
        Assert.True(model.Snapshot.Status.WifiNames);
        var names = new WifiNames();
        Assert.True(names.ReadDue(model.Snapshot, WifiAccess.Granted, Sample.Now));
        names.Reading(model.Snapshot, Sample.Now);
        names.Read("n-home", "Home Wi-Fi");
        var report = names.Due(model.Snapshot, Sample.Now)!;
        names.Sent(model.Snapshot, report, Sample.Now);
        await model.ReportWifiNameAsync(report);
        Assert.Equal(("n-home", "Home Wi-Fi"), (agent.Reports[^1].NetworkId, agent.Reports[^1].Ssid));
        // The service names the network by it now, and holds it: nothing more to tell.
        Assert.Equal(new UiText(TextKeys.Verbatim, "Home Wi-Fi"), Profiles.NetworkTitle(model.Snapshot.Status.Network!));
        Assert.Equal("n-home", model.Snapshot.Status.WifiNameHeld!.NetworkId);
        Assert.Null(names.Due(model.Snapshot, Sample.Now.AddSeconds(60)));

        // Reconnected: a new session, where it is told again.
        agent.Disconnect();
        await Eventually("a new connection", () => model.Snapshot.Session == 2 && model.Snapshot.Link == AgentLink.Connected);
        Assert.True(names.ReadDue(model.Snapshot, WifiAccess.Granted, Sample.Now.AddSeconds(61)));
        Assert.Equal(report, names.Due(model.Snapshot, Sample.Now.AddSeconds(61)));
    }
}
