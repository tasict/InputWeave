using Inputweave.App.V1;
using InputWeave.Panel;

namespace InputWeave.Licensing;

/// <summary>Which License page the app shows.</summary>
public enum LicenseVariant
{
    /// <summary>A background service from before licenses: the app shows nothing of them.</summary>
    Hidden,
    /// <summary>No license, or one waiting to be activated: every feature works.</summary>
    Trial,
    /// <summary>The license is on 5 computers already; this one stays in trial.</summary>
    SeatsFull,
    /// <summary>The key is unknown, revoked or refunded, or this computer's activation is gone.</summary>
    Invalid,
    /// <summary>The service cannot read its license file right now.</summary>
    Unavailable,
    Licensed,
}

/// <summary>A computer on the license, for "Computers on this license" from a background service before 1.1.</summary>
public sealed record LicensedComputer(string Name, bool ThisComputer, bool Licensed);

/// <summary>How a computer stands on this computer's license, as its row in "Computers on this license" shows it.</summary>
public enum LicenseRowState
{
    /// <summary>This computer: licensed, nothing to do here.</summary>
    ThisComputer,
    /// <summary>Activated there with this computer's key: Revoke License (but on InputWeave 1.0).</summary>
    Licensed,
    /// <summary>Not on the license: Apply License.</summary>
    Trial,
    /// <summary>The key is there or on its way, and its app activates it; Don't Apply while it can be taken back.</summary>
    Activating,
    /// <summary>Applied while it is offline: the key goes to it as it connects; Don't Apply.</summary>
    Waiting,
    /// <summary>Polar said every place is taken: Apply License tries again.</summary>
    NoPlaces,
    /// <summary>Licensed there with another key.</summary>
    OtherLicense,

    /// <summary>
    /// Applied from here while this computer is not licensed (seats full,
    /// invalid, …): sent once it is licensed again; on the trial page, under
    /// "Waiting to apply".
    /// </summary>
    AwaitingLicense,
}

/// <summary>The line under a computer's name on the license: this one, or whether it is out of reach.</summary>
public enum LicenseRowLine
{
    None,
    ThisComputer,
    Offline,
    OfflineForNow,
}

/// <summary>
/// A row of "Computers on this license", or of "Waiting to apply": only
/// values, so two lists compare by what they show. <paramref name="TakeBack"/>:
/// what was applied there can be taken back (LicensedDevice.can_take_back);
/// <paramref name="LegacyRefused"/>: on InputWeave 1.0, it did not take the
/// license applied from here (LicensedDevice.legacy_refused).
/// </summary>
public sealed record LicenseRow(
    string Id,
    string Name,
    LicenseRowState State,
    LicenseRowLine Line,
    bool Legacy = false,
    string ActivationId = "",
    bool TakeBack = false,
    bool LegacyRefused = false)
{
    /// <summary>Revoke License… is offered: a computer licensed with this key, but not one on InputWeave 1.0, which cannot be told.</summary>
    public bool CanRevoke => State == LicenseRowState.Licensed && !Legacy;

    /// <summary>Apply License is offered (and can be pressed unless every place is used).</summary>
    public bool CanApply => State is LicenseRowState.Trial or LicenseRowState.NoPlaces;

    /// <summary>Don't Apply is offered: applied there, waiting for it or not activated yet, and the service can take it back.</summary>
    public bool CanTakeBack => TakeBack && (State is LicenseRowState.Activating or LicenseRowState.Waiting or LicenseRowState.AwaitingLicense);

    /// <summary>The line under the name for this row, if any (TextKeys).</summary>
    public string? LineKey => Line switch
    {
        LicenseRowLine.ThisComputer => TextKeys.LicenseThisComputer,
        LicenseRowLine.Offline => TextKeys.ConnectionOffline,
        LicenseRowLine.OfflineForNow => TextKeys.ConnectionOfflineForNow,
        _ => null,
    };

    /// <summary>
    /// A further note under the name: the computer runs InputWeave 1.0 and
    /// holds the key (licensed, or heard it and cannot be told to drop it),
    /// or did not take it, or uses another license.
    /// </summary>
    public string? NoteKey => State switch
    {
        LicenseRowState.Licensed when Legacy => TextKeys.LicenseLegacyNote,
        LicenseRowState.Activating or LicenseRowState.Waiting or LicenseRowState.AwaitingLicense when Legacy && !TakeBack => TextKeys.LicenseLegacyNote,
        LicenseRowState.Trial when LegacyRefused => TextKeys.LicenseLegacyRefusedNote,
        LicenseRowState.OtherLicense => TextKeys.LicenseOtherLicenseNote,
        _ => null,
    };

    /// <summary>The row takes one of the license's places.</summary>
    public bool TakesPlace => State is LicenseRowState.ThisComputer or LicenseRowState.Licensed
        or LicenseRowState.Activating or LicenseRowState.Waiting;
}

/// <summary>What removing another computer offers about its license.</summary>
public enum RemovalOffer
{
    /// <summary>Nothing: this computer is on trial, or the other one is not on its license.</summary>
    None,
    /// <summary>It runs InputWeave 1.0 and keeps its place: a line says how to free it.</summary>
    Legacy,
    /// <summary>"Also revoke its license", checked to begin with.</summary>
    Revoke,
}

/// <summary>What the License page, its sidebar item and About show, from the service's license.</summary>
public sealed record LicenseView(
    LicenseVariant Variant,
    string Email,
    string? SourceName,
    IReadOnlyList<LicensedComputer> Computers,
    int LicensedCount)
{
    /// <summary>A license covers this many computers.</summary>
    public const int Seats = 5;

    public bool IsLicensed => Variant == LicenseVariant.Licensed;

    /// <summary>The License page can be shown: the service keeps licenses.</summary>
    public bool Shown => Variant != LicenseVariant.Hidden;

    /// <summary>The service applies and revokes the license (License.apply_supported): the rows below, not <see cref="Computers"/>.</summary>
    public bool ApplySupported { get; init; }

    /// <summary>Licensed, with a service that applies licenses: this computer, then every computer it trusts.</summary>
    public IReadOnlyList<LicenseRow> Rows { get; init; } = [];

    /// <summary>The places the license takes: this computer and the rows licensed, activating or waiting for it.</summary>
    public int PlacesUsed => Rows.Count(r => r.TakesPlace);

    /// <summary>Every place is used: Apply License is not offered on any row.</summary>
    public bool AllPlacesUsed => Rows.Count != 0 && PlacesUsed >= Seats;

    /// <summary>Polar said every place is taken for some row: the warning under the list.</summary>
    public bool NoPlaces => Rows.Any(r => r.State == LicenseRowState.NoPlaces);

    /// <summary>On trial: a computer that holds a license and can apply it here (not one on InputWeave 1.0), named in the info box; null for none.</summary>
    public string? LicensedElsewhere { get; init; }

    /// <summary>
    /// Not licensed (on trial, seats full or invalid, also while this
    /// computer's license is being removed; not unreadable): the computers
    /// with an apply from here they have not shown yet
    /// (LicensedDevice.waiting_to_apply), under "Waiting to apply". Not one
    /// waiting with the key it holds already.
    /// </summary>
    public IReadOnlyList<LicenseRow> WaitingToApply { get; init; } = [];

    /// <summary>For the License page: also the computers this one trusts, from the whole status.</summary>
    /// <param name="removing">The work takes the license away (<see cref="LicenseWorker.Withdrawing"/>): shown as a trial.</param>
    public static LicenseView For(Status status, bool removing = false) =>
        For(status.License, status.ThisDeviceName, removing, status.Devices, status.ThisDeviceId);

    /// <param name="thisName">This computer's name (Status.this_device_name).</param>
    /// <param name="removing">Remove License was confirmed and the service has not taken it yet: shown as a trial.</param>
    /// <param name="devices">The computers this one trusts (Status.devices), in their order, for the rows; none for the sidebar and About.</param>
    /// <param name="thisId">This computer's id, never one of the rows below it.</param>
    public static LicenseView For(License? license, string thisName, bool removing = false, IEnumerable<DeviceStatus>? devices = null, string thisId = "")
    {
        if (license is null)
        {
            return new LicenseView(LicenseVariant.Hidden, "", null, [], 0);
        }
        var variant = removing ? LicenseVariant.Trial : license.State switch
        {
            LicenseState.Licensed => LicenseVariant.Licensed,
            // From 1.1, a key another computer applied here that found every
            // place taken stays plainly on trial: that computer says why.
            LicenseState.SeatsFull when !license.ApplySupported || license.SourceDeviceId.Length == 0 => LicenseVariant.SeatsFull,
            LicenseState.Invalid => LicenseVariant.Invalid,
            LicenseState.Unavailable => LicenseVariant.Unavailable,
            // Revoked here by another computer, too: the app gives it up by itself.
            _ => LicenseVariant.Trial,
        };
        var computers = new List<LicensedComputer>();
        if (variant == LicenseVariant.Licensed)
        {
            computers.Add(new LicensedComputer(thisName, ThisComputer: true, Licensed: true));
        }
        foreach (var device in license.Devices.OrderByDescending(d => d.Licensed).ThenBy(d => d.Name, StringComparer.CurrentCultureIgnoreCase))
        {
            computers.Add(new LicensedComputer(device.Name, ThisComputer: false, device.Licensed));
        }
        static LicenseRowLine LineOf(DeviceStatus device) =>
            device.OfflineForNow ? LicenseRowLine.OfflineForNow
            : device.State != ConnectionState.Connected ? LicenseRowLine.Offline
            : LicenseRowLine.None;
        var rows = new List<LicenseRow>();
        if (license.ApplySupported && variant == LicenseVariant.Licensed)
        {
            rows.Add(new LicenseRow(thisId, thisName, LicenseRowState.ThisComputer, LicenseRowLine.ThisComputer));
            foreach (var device in devices ?? [])
            {
                if (device.Id == thisId && thisId.Length != 0)
                {
                    continue;
                }
                var listed = license.Devices.FirstOrDefault(d => d.Id == device.Id);
                var state = listed?.Use switch
                {
                    LicenseUse.Licensed => LicenseRowState.Licensed,
                    LicenseUse.Activating => LicenseRowState.Activating,
                    LicenseUse.Waiting => LicenseRowState.Waiting,
                    LicenseUse.NoPlaces => LicenseRowState.NoPlaces,
                    LicenseUse.OtherLicense => LicenseRowState.OtherLicense,
                    _ => LicenseRowState.Trial,
                };
                rows.Add(new LicenseRow(
                    device.Id,
                    device.Name,
                    state,
                    LineOf(device),
                    listed?.Legacy ?? false,
                    state == LicenseRowState.Licensed ? listed!.ActivationId : "",
                    listed?.CanTakeBack ?? false,
                    state == LicenseRowState.Trial && (listed?.LegacyRefused ?? false)));
            }
        }
        var waiting = new List<LicenseRow>();
        if (license.ApplySupported && variant is LicenseVariant.Trial or LicenseVariant.SeatsFull or LicenseVariant.Invalid)
        {
            foreach (var device in devices ?? [])
            {
                if (license.Devices.FirstOrDefault(d => d.Id == device.Id) is { WaitingToApply: true } listed)
                {
                    waiting.Add(new LicenseRow(
                        device.Id, device.Name, LicenseRowState.AwaitingLicense, LineOf(device), listed.Legacy, "", listed.CanTakeBack));
                }
            }
        }
        string? elsewhere = null;
        if (license.ApplySupported && variant != LicenseVariant.Licensed
            && license.Devices.FirstOrDefault(d => (d.Use is LicenseUse.Licensed or LicenseUse.OtherLicense) && !d.Legacy) is { } holder)
        {
            elsewhere = holder.Name.Length != 0 ? holder.Name
                : devices?.FirstOrDefault(d => d.Id == holder.Id)?.Name ?? "";
            elsewhere = elsewhere.Length != 0 ? elsewhere : null;
        }
        return new LicenseView(
            variant,
            Mask(license.CustomerEmail),
            license.SourceDeviceName.Length > 0 ? license.SourceDeviceName : null,
            computers,
            license.ApplySupported && variant == LicenseVariant.Licensed
                ? rows.Count(r => r.State is LicenseRowState.ThisComputer or LicenseRowState.Licensed)
                : computers.Count(c => c.Licensed))
        {
            ApplySupported = license.ApplySupported,
            Rows = rows,
            LicensedElsewhere = elsewhere,
            WaitingToApply = waiting,
        };
    }

    /// <summary>
    /// What the confirmation to remove another computer offers about its
    /// license: while this computer is licensed (<paramref name="thisLicensed"/>,
    /// as <see cref="IsLicensed"/> says) and the other one licensed,
    /// activating or waiting for the key, "Also revoke its license", or for
    /// one on InputWeave 1.0 a line saying it keeps its place; nothing otherwise.
    /// </summary>
    public static RemovalOffer OnRemoval(License? license, bool thisLicensed, string deviceId)
    {
        if (!thisLicensed || license is not { ApplySupported: true }
            || license.Devices.FirstOrDefault(d => d.Id == deviceId) is not { } device
            || device.Use is not (LicenseUse.Licensed or LicenseUse.Activating or LicenseUse.Waiting))
        {
            return RemovalOffer.None;
        }
        return device.Legacy ? RemovalOffer.Legacy : RemovalOffer.Revoke;
    }

    /// <summary>
    /// The buyer's address shortened to its first character and its domain,
    /// "t•••@gmail.com"; empty for none, and "•••" for one without a domain.
    /// </summary>
    public static string Mask(string email)
    {
        email = email.Trim();
        if (email.Length == 0)
        {
            return "";
        }
        int at = email.LastIndexOf('@');
        if (at <= 0 || at == email.Length - 1)
        {
            return "•••";
        }
        return $"{char.ConvertFromUtf32(char.ConvertToUtf32(email, 0))}•••{email[at..]}";
    }
}
