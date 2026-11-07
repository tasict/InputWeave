using Inputweave.App.V1;
using InputWeave.Agent;

namespace InputWeave.Settings;

/// <summary>Whether this app may read the Wi-Fi name: Windows asks the user for location access first.</summary>
public enum WifiAccess
{
    /// <summary>Not asked yet: asking shows Windows' prompt.</summary>
    NotAsked,
    Granted,
    /// <summary>Denied by the user, or by Windows, as with location services off.</summary>
    Denied,
    /// <summary>Windows could not say this time; asked again at the next chance.</summary>
    Unknown,
}

/// <summary>What to do about the Wi-Fi name now (<see cref="WifiNames.Step"/>).</summary>
public enum WifiStep
{
    /// <summary>Nothing for now.</summary>
    Wait,
    /// <summary>Ask Windows for location access, which shows its prompt.</summary>
    Ask,
    /// <summary>Read the Wi-Fi name when it is due (<see cref="WifiNames.ReadDue"/>).</summary>
    Read,
    /// <summary>Turn the setting off: the user denied location access.</summary>
    TurnOff,
}

/// <summary>
/// "Also recognize networks by Wi-Fi name" on the Profiles page, and
/// telling the service the Wi-Fi name, which only the app reads: Windows
/// keeps it from apps without location access. Only when the user turns the
/// switch on does the app ask for that; once it has it, it turns the setting
/// on (SetWifiNames) and reads the name of the network this computer is on,
/// at most every <see cref="ReadEvery"/> and at once for another network or
/// connection, or once the setting was off. A reading is the network the
/// status named and its Wi-Fi name, or none when this computer is not on
/// Wi-Fi; a read that failed changes nothing. The app tells the service a
/// reading (ReportWifiName) as soon as it changes and after every new
/// connection; otherwise again only while the service holds no name for that
/// network (Status.wifi_name_held), as when a report came while it waited to
/// tell whether the network changed, at most every <see cref="ResendEvery"/>.
/// Denied, the switch goes back off; access the user takes away later turns
/// the setting off. With the setting on and access not decided yet (it was
/// turned on elsewhere, or the user's choice was reset), the app asks once
/// each time it starts, never while the setting is off, and reads nothing
/// until access is allowed. The app never logs the name itself.
/// </summary>
public sealed class WifiNames
{
    /// <summary>The Wi-Fi name is read at most this often, besides at once for another network.</summary>
    public static readonly TimeSpan ReadEvery = TimeSpan.FromSeconds(5);

    /// <summary>A reading the service holds nothing of is told again at most this often.</summary>
    public static readonly TimeSpan ResendEvery = TimeSpan.FromSeconds(5);

    private DateTimeOffset? readAt;
    private string readNetwork = "";
    private ulong readSession;
    /// <summary>The last reading that worked: the network as the status named it, and its Wi-Fi name, empty when not on Wi-Fi.</summary>
    private (string Network, string Ssid)? reading;
    /// <summary>The last reading told, on which connection, and when.</summary>
    private (ulong Session, string Network, string Ssid)? told;
    private DateTimeOffset toldAt;
    private ulong turnedOffIn;

    /// <summary>The user turned the switch on and Windows has not answered yet.</summary>
    public bool Asking { get; private set; }

    /// <summary>
    /// Location access as Windows names it, by the name of its answer:
    /// AppCapability.CheckAccess (Allowed, UserPromptRequired, DeniedByUser,
    /// DeniedBySystem, NotDeclaredByApp) or WiFiAdapter.RequestAccessAsync
    /// (Allowed, DeniedByUser, DeniedBySystem, Unspecified). Only the user's
    /// or the system's denial counts as one; anything Windows may say of an
    /// app without package identity, or adds later, is not known, so the
    /// setting is never turned off for it.
    /// </summary>
    public static WifiAccess Access(string status) => status switch
    {
        "Allowed" => WifiAccess.Granted,
        "UserPromptRequired" => WifiAccess.NotAsked,
        "DeniedByUser" or "DeniedBySystem" => WifiAccess.Denied,
        _ => WifiAccess.Unknown,
    };

    /// <summary>Whether this app, since it started, asked Windows for location access already.</summary>
    public bool AskedThisLaunch { get; private set; }

    /// <summary>
    /// What to do about the Wi-Fi name with the setting on or off
    /// (<paramref name="switchOn"/>) and location access as Windows says it:
    /// nothing while the setting is off (never asking then); read once access
    /// is allowed; turn the setting off once it is denied; ask once while it
    /// is not decided, then wait while it stays so; wait while Windows could
    /// not say, which is never taken as a denial.
    /// </summary>
    public static WifiStep Step(bool switchOn, WifiAccess access, bool askedThisLaunch) => (switchOn, access) switch
    {
        (false, _) => WifiStep.Wait,
        (_, WifiAccess.Granted) => WifiStep.Read,
        (_, WifiAccess.Denied) => WifiStep.TurnOff,
        (_, WifiAccess.NotAsked) when !askedThisLaunch => WifiStep.Ask,
        _ => WifiStep.Wait,
    };

    /// <summary>
    /// <see cref="Step"/> for the latest state: nothing while the service is
    /// out of reach or knows no profiles, or while Windows is being asked;
    /// the setting turned off at most once per connection.
    /// </summary>
    public WifiStep Next(AgentSnapshot model, WifiAccess access)
    {
        if (model.Link != AgentLink.Connected || !Profiles.Supported(model) || Asking)
        {
            return WifiStep.Wait;
        }
        var step = Step(model.Status.WifiNames, access, AskedThisLaunch);
        return step == WifiStep.TurnOff && turnedOffIn == model.Session ? WifiStep.Wait : step;
    }

    /// <summary>
    /// What a read of the Wi-Fi name gives: the name, or empty when the
    /// internet is not reached over Wi-Fi (<paramref name="onWifi"/> false);
    /// null for a read that failed, as when it threw (<paramref name="onWifi"/>
    /// null) or Wi-Fi gave no name, which is not reported.
    /// </summary>
    public static string? Outcome(bool? onWifi, string? ssid) => onWifi switch
    {
        false => "",
        true when !string.IsNullOrEmpty(ssid) => ssid,
        _ => null,
    };

    /// <summary>
    /// Takes in the latest state: while the setting is off, nothing read or
    /// told before counts, so once it is on again, from here or elsewhere,
    /// the name is read and told at once, even on the same network.
    /// </summary>
    public void Update(AgentSnapshot model)
    {
        if (!model.Status.WifiNames)
        {
            Forget();
        }
    }

    private void Forget()
    {
        reading = null;
        told = null;
        readAt = null;
    }

    /// <summary>Whether the switch shows on: as the service says, or on while asking.</summary>
    public bool SwitchOn(AgentSnapshot model) => Asking || model.Status.WifiNames;

    /// <summary>
    /// Location access is being asked for: the user turned the switch on,
    /// or the setting is on and access not decided (<see cref="WifiStep.Ask"/>).
    /// </summary>
    public void Ask()
    {
        Asking = true;
        AskedThisLaunch = true;
    }

    /// <summary>
    /// Windows answered <paramref name="access"/>: what to send.
    /// SetWifiNames(true) once access is granted while the setting is off
    /// (the user turned the switch on); SetWifiNames(false) once it is denied
    /// while the setting is on. Otherwise nothing (null): a switch the user
    /// turned on goes back off by itself, and a setting already on stays on,
    /// read only once access is allowed.
    /// </summary>
    public bool? Answered(WifiAccess access, AgentSnapshot model)
    {
        Asking = false;
        bool on = model.Status.WifiNames;
        switch (access)
        {
            case WifiAccess.Granted:
                // Read and told again, under the setting just turned on.
                Forget();
                return on ? null : true;
            case WifiAccess.Denied when on:
                turnedOffIn = model.Session;
                return false;
            default:
                return null;
        }
    }

    /// <summary>Whether the Wi-Fi name is to be told now at all: connected, the setting on, and a network.</summary>
    private static bool Wanted(AgentSnapshot model) =>
        model.Link == AgentLink.Connected && Profiles.Supported(model) && model.Status.WifiNames
        && model.Status.Network is not null;

    /// <summary>
    /// Whether to read the Wi-Fi name now, as of <paramref name="now"/>: the
    /// setting is on, the app has access, and it was not read in the last
    /// <see cref="ReadEvery"/>, or was read for another network or connection.
    /// </summary>
    public bool ReadDue(AgentSnapshot model, WifiAccess access, DateTimeOffset now)
    {
        if (!Wanted(model) || access != WifiAccess.Granted || Asking)
        {
            return false;
        }
        return readAt is not { } at || now - at >= ReadEvery
            || model.Status.Network!.Id != readNetwork || model.Session != readSession;
    }

    /// <summary>A reading starts now, for the network and connection the status names.</summary>
    public void Reading(AgentSnapshot model, DateTimeOffset now)
    {
        readAt = now;
        readNetwork = model.Status.Network?.Id ?? "";
        readSession = model.Session;
    }

    /// <summary>
    /// A reading for <paramref name="network"/> (as the status named it when
    /// it started) finished with <paramref name="ssid"/> (<see cref="Outcome"/>):
    /// the name, empty when not on Wi-Fi, or null for a read that failed,
    /// which keeps the last reading.
    /// </summary>
    public void Read(string network, string? ssid)
    {
        if (ssid is not null)
        {
            reading = (network, ssid);
        }
    }

    /// <summary>
    /// The report to send now, as of <paramref name="now"/>, if any: the
    /// reading for the network the status names, at once when it is not the
    /// one told on this connection; else again only while the service holds
    /// no name for that network, at most every <see cref="ResendEvery"/>.
    /// While it holds one for it, even another name, nothing until the
    /// reading changes.
    /// </summary>
    public ReportWifiName? Due(AgentSnapshot model, DateTimeOffset now)
    {
        if (!Wanted(model) || reading is not { } current || current.Network != model.Status.Network!.Id)
        {
            return null;
        }
        var report = new ReportWifiName { NetworkId = current.Network, Ssid = current.Ssid };
        if (told != (model.Session, current.Network, current.Ssid))
        {
            return report;
        }
        if (model.Status.WifiNameHeld is { } held && held.NetworkId == current.Network)
        {
            return null;
        }
        return now - toldAt >= ResendEvery ? report : null;
    }

    /// <summary>A report goes to the service now, on this connection; it counts as told whatever the answer.</summary>
    public void Sent(AgentSnapshot model, ReportWifiName report, DateTimeOffset now)
    {
        told = (model.Session, report.NetworkId, report.Ssid);
        toldAt = now;
    }

    /// <summary>The setting was turned off for the access taken away, on this connection.</summary>
    public void TurnedOff(AgentSnapshot model) => turnedOffIn = model.Session;
}
