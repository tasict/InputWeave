using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;

namespace InputWeave.Settings;

/// <summary>A choice in the Profile submenu: Automatic (an empty id) or a profile, checked when it is the one chosen.</summary>
public sealed record ProfileChoice(string Id, UiText Name, bool Checked);

/// <summary>
/// The Profile submenu of the menu the tray icon and the panel's More
/// button share: Automatic, each profile, then Manage Profiles….
/// </summary>
public sealed record ProfileMenu(IReadOnlyList<ProfileChoice> Choices, bool Enabled)
{
    public bool Equals(ProfileMenu? other) =>
        other is not null && Enabled == other.Enabled && Choices.SequenceEqual(other.Choices);

    public override int GetHashCode() => HashCode.Combine(Enabled, Choices.Count);
}

/// <summary>A profile a network can be added to, checked when it lists the network already.</summary>
public sealed record ProfileTarget(string Id, UiText Name, bool Checked);

/// <summary>A paired computer in a profile: connected in it unless the profile lists it as not.</summary>
public sealed record ProfileComputer(string Id, string Name, bool Connect);

/// <summary>
/// Profiles: named sets of settings this computer picks by the network it
/// is on, or that the user picks, as the Profiles page, the panel and the
/// menus show them, the same as on the Mac and Linux. The service decides
/// which profile is in use and what it does to each computer
/// (DeviceStatus.not_in_profile); this only says what the app shows for
/// that. A service from before (Status.profiles_supported false) gets none
/// of it.
/// </summary>
public static class Profiles
{
    /// <summary>The Segoe Fluent stacked layers (MapLayers): the Profiles page, and before "Not in this profile".</summary>
    public const string Glyph = "";

    /// <summary>The Segoe Fluent Wi-Fi, ethernet and VPN glyphs, before a network.</summary>
    public const string WifiGlyph = "";
    public const string WiredGlyph = "";
    public const string VpnGlyph = "";

    /// <summary>Whether the service knows profiles.</summary>
    public static bool Supported(AgentSnapshot model) => model.ProfilesSupported;

    /// <summary>The profile in use; null from a service that does not know profiles.</summary>
    public static Profile? InUse(AgentSnapshot model) =>
        Supported(model) ? model.Status.Profiles.FirstOrDefault(p => p.Id == model.Status.ProfileId) : null;

    /// <summary>Whether the profile in use is paused: this computer neither announces itself nor connects to any other.</summary>
    public static bool Paused(AgentSnapshot model) => InUse(model) is { Paused: true };

    /// <summary>Whether the profile in use has files turned off: they go to no computer.</summary>
    public static bool FilesOff(AgentSnapshot model) => InUse(model) is { Files: false };

    /// <summary>
    /// Whether the profile in use doesn't connect another computer, so it is
    /// offline here. Being offline for now comes first.
    /// </summary>
    public static bool NotInProfile(AgentSnapshot model, string id) =>
        Supported(model) && id != model.ThisDeviceId && model.Device(id) is { NotInProfile: true }
        && !OfflineForNow.IsOffline(model, id);

    /// <summary>
    /// Whether the profile in use cuts another computer off: it doesn't
    /// connect it, or it is paused, when every other computer is just
    /// offline. Nothing can be done with it but renaming it: no moving the
    /// cursor there, no waking it, no taking it offline for now.
    /// </summary>
    public static bool CutOff(AgentSnapshot model, string id) =>
        id != model.ThisDeviceId && (NotInProfile(model, id) || Paused(model));

    /// <summary>Whether another computer's context menu on the canvas has only Rename….</summary>
    public static bool OnlyRename(AgentSnapshot model, string id) => CutOff(model, id);

    /// <summary>A profile's name: the app's own for Default and Other networks, the user's for the others.</summary>
    public static UiText Name(Profile profile) => profile.Kind switch
    {
        ProfileKind.Default => new(TextKeys.ProfileDefault),
        ProfileKind.OtherNetworks => new(TextKeys.ProfileOtherNetworks),
        _ => new(TextKeys.Verbatim, profile.Name),
    };

    /// <summary>Default and Other networks: they can't be renamed or removed.</summary>
    public static bool Fixed(Profile profile) => profile.Kind != ProfileKind.Custom;

    /// <summary>
    /// "Automatic (now: Office)" while the network picks the profile; plain
    /// "Automatic" while the user chose one, as which one the network would
    /// pick is not known here then.
    /// </summary>
    public static UiText AutomaticLabel(AgentSnapshot model) =>
        !model.Status.ProfileChosen && InUse(model) is { } inUse
            ? new(TextKeys.ProfilesAutomaticNow, Name(inUse))
            : new(TextKeys.ProfilesAutomatic);

    /// <summary>
    /// What "Profile in use" shows chosen: the profile the user chose, or
    /// Automatic (an empty id).
    /// </summary>
    public static string Chosen(AgentSnapshot model) => model.Status.ProfileChosen ? model.Status.ProfileId : "";

    /// <summary>
    /// What a network is called: its name, else "Router 192.168.1.1", else,
    /// without either, its kind: "Wi-Fi network", "VPN", or "Wired network".
    /// </summary>
    public static UiText NetworkTitle(Network network) =>
        network.Name.Length != 0 ? new(TextKeys.Verbatim, network.Name)
        : network.Router.Length != 0 ? new(TextKeys.ProfilesRouter, network.Router)
        : network.Kind switch
        {
            NetworkKind.Wifi => new(TextKeys.ProfilesWifiNetwork),
            NetworkKind.Vpn => new(TextKeys.ProfilesVpn),
            _ => new(TextKeys.ProfilesWiredNetwork),
        };

    /// <summary>The line under a network called by its name: its router, when it has one.</summary>
    public static UiText? NetworkDetail(Network network) =>
        network.Name.Length != 0 && network.Router.Length != 0 ? new(TextKeys.ProfilesRouter, network.Router) : null;

    /// <summary>The glyph before a network: Wi-Fi, a VPN, or wired.</summary>
    public static string NetworkGlyph(Network network) => network.Kind switch
    {
        NetworkKind.Wifi => WifiGlyph,
        NetworkKind.Vpn => VpnGlyph,
        _ => WiredGlyph,
    };

    /// <summary>"Current network: Office-5G", or that this computer is on none.</summary>
    public static UiText CurrentNetwork(AgentSnapshot model) =>
        model.Status.Network is { } network
            ? new(TextKeys.ProfilesCurrentNetwork, NetworkTitle(network))
            : new(TextKeys.ProfilesNoNetwork);

    /// <summary>Whether a profile lists the network this computer is on.</summary>
    public static bool HasCurrentNetwork(AgentSnapshot model, Profile profile) =>
        model.Status.Network is { } network && profile.Networks.Any(n => n.Id == network.Id);

    /// <summary>
    /// Whether the network this computer is on can be added to a profile
    /// now: Add to Profile, the panel's Add This Network To…. Not without
    /// one, nor for the moment after joining it while it is known only by
    /// its router's address (Network.resolved), which would match no more
    /// once the router resolves.
    /// </summary>
    public static bool CanAddNetwork(AgentSnapshot model) =>
        model.Link == AgentLink.Connected && model.Status.Network is { Resolved: true };

    /// <summary>
    /// Whether "Add Current Network" can be pressed for a profile: the
    /// network can be added (<see cref="CanAddNetwork"/>), the profile
    /// doesn't list it yet, and the profile lists networks at all (Other
    /// networks lists none).
    /// </summary>
    public static bool CanAddCurrentNetwork(AgentSnapshot model, Profile profile) =>
        CanAddNetwork(model) && profile.Kind != ProfileKind.OtherNetworks && !HasCurrentNetwork(model, profile);

    /// <summary>
    /// The profiles the network this computer is on can be added to (not
    /// Other networks), the one listing it checked: "Add to Profile" and
    /// the panel's "Add This Network To…".
    /// </summary>
    public static IReadOnlyList<ProfileTarget> AddTargets(AgentSnapshot model) =>
        !Supported(model)
            ? []
            : model.Status.Profiles
                .Where(p => p.Kind != ProfileKind.OtherNetworks)
                .Select(p => new ProfileTarget(p.Id, Name(p), HasCurrentNetwork(model, p)))
                .ToList();

    /// <summary>
    /// The line under a profile in "All profiles": "Sharing paused" first
    /// for a paused one, then its networks, or what Other networks is for,
    /// in a list ("Sharing paused, Office-5G, Router 192.168.1.1"). Null for
    /// a profile without networks that shares.
    /// </summary>
    public static UiText? Description(Profile profile)
    {
        var parts = new List<UiText>();
        if (profile.Paused)
        {
            parts.Add(new(TextKeys.ProfileSharingPaused));
        }
        if (profile.Kind == ProfileKind.OtherNetworks)
        {
            parts.Add(new(TextKeys.ProfileOtherNetworksHint));
        }
        else
        {
            parts.AddRange(profile.Networks.Select(NetworkTitle));
        }
        return parts.Count == 0 ? null : parts.Aggregate((list, next) => new UiText(TextKeys.ProfilesListJoin, list, next));
    }

    /// <summary>Whether a profile is the one in use: "In use" on its row.</summary>
    public static bool IsInUse(AgentSnapshot model, Profile profile) => profile.Id == model.Status.ProfileId;

    /// <summary>The paired computers, each connected in the profile unless it lists them as not.</summary>
    public static IReadOnlyList<ProfileComputer> Computers(AgentSnapshot model, Profile profile) =>
        model.Status.Devices
            .Select(d => new ProfileComputer(d.Id, d.Name, !profile.OffDeviceIds.Contains(d.Id)))
            .ToList();

    /// <summary>
    /// The name to send for a profile after the user typed
    /// <paramref name="typed"/>: trimmed, and null when nothing changed, for
    /// Default and Other networks, or when it was emptied (the name it has
    /// comes back).
    /// </summary>
    public static string? Rename(Profile profile, string typed)
    {
        var name = typed.Trim();
        return Fixed(profile) || name.Length == 0 || name == profile.Name ? null : name;
    }

    /// <summary>
    /// What a profile's page says under the name after a new one was refused
    /// with <paramref name="code"/>: that another profile has it
    /// ("name_taken", Default and Other networks included, whatever the
    /// case). Null for other refusals, after which the name it has comes
    /// back with nothing said.
    /// </summary>
    public static UiText? RenameRefused(string code) => code == "name_taken" ? new(TextKeys.ProfileNameTaken) : null;

    /// <summary>
    /// The profile Add Profile made: the one listed after it that was not
    /// before. Null when there is none.
    /// </summary>
    public static string? Added(IReadOnlyCollection<string> before, Status after) =>
        after.Profiles.FirstOrDefault(p => !before.Contains(p.Id))?.Id;

    /// <summary>
    /// The panel's title while the profile in use is paused: "Other
    /// networks: sharing paused", before anything else. Null otherwise.
    /// </summary>
    public static UiText? PanelTitle(AgentSnapshot model) =>
        InUse(model) is { Paused: true } paused ? new(TextKeys.TitleProfilePaused, Name(paused)) : null;

    /// <summary>
    /// The capsule after the panel's title: the profile in use, after every
    /// title but the paused profile's own, which names it already. Null
    /// while the service is out of reach, or knows no profiles.
    /// </summary>
    public static UiText? Capsule(AgentSnapshot model) =>
        model.Link == AgentLink.Connected && InUse(model) is { Paused: false } inUse ? Name(inUse) : null;

    /// <summary>
    /// Whether the network picked Other networks, paused, as this network is
    /// in no profile: the panel says so and offers to add it to one. Not when
    /// the user chose the profile, nor without a network.
    /// </summary>
    public static bool NetworkInNoProfile(AgentSnapshot model) =>
        model.Link == AgentLink.Connected && InUse(model) is { Paused: true, Kind: ProfileKind.OtherNetworks }
        && !model.Status.ProfileChosen && model.Status.Network is not null;

    /// <summary>
    /// The line under the paused profile's title: that this network is in no
    /// profile, or nothing. Null while the profile in use is not paused.
    /// </summary>
    public static UiText? PanelSubtitle(AgentSnapshot model) =>
        NetworkInNoProfile(model) ? new(TextKeys.SubtitleNetworkInNoProfile) : null;

    /// <summary>
    /// The Profile submenu: Automatic, then each profile, the chosen one
    /// checked (Automatic while none is); null from a service that does not
    /// know profiles.
    /// </summary>
    public static ProfileMenu? Menu(AgentSnapshot model)
    {
        if (!Supported(model))
        {
            return null;
        }
        var chosen = Chosen(model);
        var choices = new List<ProfileChoice> { new("", new(TextKeys.ProfilesAutomatic), chosen.Length == 0) };
        choices.AddRange(model.Status.Profiles.Select(p => new ProfileChoice(p.Id, Name(p), chosen.Length != 0 && p.Id == chosen)));
        return new ProfileMenu(choices, model.Link == AgentLink.Connected);
    }
}
