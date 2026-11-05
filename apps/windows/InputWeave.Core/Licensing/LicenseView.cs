using Inputweave.App.V1;

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

/// <summary>A computer on the license, for "Computers on this license".</summary>
public sealed record LicensedComputer(string Name, bool ThisComputer, bool Licensed);

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

    /// <param name="thisName">This computer's name (Status.this_device_name).</param>
    /// <param name="removing">Remove License was confirmed and the service has not taken it yet: shown as a trial.</param>
    public static LicenseView For(License? license, string thisName, bool removing = false)
    {
        if (license is null)
        {
            return new LicenseView(LicenseVariant.Hidden, "", null, [], 0);
        }
        var variant = removing ? LicenseVariant.Trial : license.State switch
        {
            LicenseState.Licensed => LicenseVariant.Licensed,
            LicenseState.SeatsFull => LicenseVariant.SeatsFull,
            LicenseState.Invalid => LicenseVariant.Invalid,
            LicenseState.Unavailable => LicenseVariant.Unavailable,
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
        return new LicenseView(
            variant,
            Mask(license.CustomerEmail),
            license.SourceDeviceName.Length > 0 ? license.SourceDeviceName : null,
            computers,
            computers.Count(c => c.Licensed));
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
