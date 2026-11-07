using InputWeave.Agent;
using InputWeave.Settings;
using Microsoft.UI.Dispatching;
using Windows.Devices.WiFi;
using Windows.Networking.Connectivity;
using Windows.Security.Authorization.AppCapabilityAccess;

namespace InputWeave.Support;

/// <summary>
/// Reads the Wi-Fi name for "Also recognize networks by Wi-Fi name" and
/// tells the background service, by <see cref="WifiNames"/>'s rules. Windows
/// gives the name of the network this computer is on only to apps with
/// location access, which the app asks for (the one prompt Windows shows per
/// app) as the user turns the switch on, and once a launch while the setting
/// is on and access is not decided; never while it is off. The name itself
/// is never logged, only its length.
/// </summary>
internal sealed class WifiNameReporter(AgentModel model, DispatcherQueue dispatcher, Action refresh)
{
    /// <summary>Location access is checked again at most this often while the setting is on.</summary>
    private static readonly TimeSpan AccessEvery = TimeSpan.FromSeconds(5);

    private readonly WifiNames state = new();
    private WifiAccess access = WifiAccess.NotAsked;
    private DateTimeOffset accessAt = DateTimeOffset.MinValue;
    private bool reading;
    private bool sending;

    /// <summary>The user turned the switch on and Windows has not answered yet.</summary>
    public bool Asking => state.Asking;

    /// <summary>Whether the switch shows on.</summary>
    public bool SwitchOn(AgentSnapshot snapshot) => state.SwitchOn(snapshot);

    /// <summary>
    /// The switch turned on: asks Windows for access, then turns the setting
    /// on, or, denied, leaves the switch to go back off.
    /// </summary>
    public void TurnOn() => Ask();

    /// <summary>
    /// Asks Windows for location access, as the user turns the switch on, or
    /// once a launch while the setting is on and access is not decided; sends
    /// what the answer calls for (<see cref="WifiNames.Answered"/>).
    /// </summary>
    private async void Ask()
    {
        if (state.Asking)
        {
            return;
        }
        state.Ask();
        refresh();
        access = await RequestAccessAsync();
        accessAt = DateTimeOffset.UtcNow;
        AppLog.Info($"wifi names: location access {access}");
        if (state.Answered(access, model.Snapshot) is { } on)
        {
            App.Instance.Send($"wifi names {on}", () => model.SetWifiNamesAsync(on));
        }
        refresh();
    }

    /// <summary>The switch turned off.</summary>
    public void TurnOff() => App.Instance.Send("wifi names false", () => model.SetWifiNamesAsync(false));

    /// <summary>
    /// Takes in the latest state, about once a second: asks for access, turns
    /// the setting off, or reads the Wi-Fi name, as <see cref="WifiNames.Next"/>
    /// says, and tells the service what it has to be told.
    /// </summary>
    public void Tick(AgentSnapshot snapshot)
    {
        state.Update(snapshot);
        // Never asked, nor read, while the setting is off.
        if (!snapshot.ProfilesSupported || !snapshot.Status.WifiNames || state.Asking)
        {
            return;
        }
        var now = DateTimeOffset.UtcNow;
        // Windows could not say last time: asked again now.
        if (access == WifiAccess.Unknown || now - accessAt >= AccessEvery)
        {
            access = Access();
            accessAt = now;
        }
        switch (state.Next(snapshot, access))
        {
            case WifiStep.Ask:
                AppLog.Info("wifi names: on, location access not decided; asking once");
                Ask();
                return;
            case WifiStep.TurnOff:
                state.TurnedOff(snapshot);
                AppLog.Info("wifi names: location access denied, turning them off");
                App.Instance.Send("wifi names false", () => model.SetWifiNamesAsync(false));
                return;
            case WifiStep.Read when !reading && state.ReadDue(snapshot, access, now):
                reading = true;
                state.Reading(snapshot, now);
                var network = snapshot.Status.Network!.Id;
                _ = Task.Run(() =>
                {
                    var ssid = ReadSsid();
                    dispatcher.TryEnqueue(() =>
                    {
                        reading = false;
                        state.Read(network, ssid);
                        Report();
                    });
                });
                break;
        }
        Report();
    }

    /// <summary>Tells the service the reading, when it is due (<see cref="WifiNames.Due"/>).</summary>
    private async void Report()
    {
        var snapshot = model.Snapshot;
        var now = DateTimeOffset.UtcNow;
        if (sending || state.Due(snapshot, now) is not { } report)
        {
            return;
        }
        state.Sent(snapshot, report, now);
        sending = true;
        try
        {
            await model.ReportWifiNameAsync(report);
            AppLog.Info(report.Ssid.Length == 0
                ? "wifi names: reported not being on Wi-Fi"
                : $"wifi names: reported a name of {System.Text.Encoding.UTF8.GetByteCount(report.Ssid)} bytes");
        }
        catch (Exception e)
        {
            // Sent again once the service holds nothing for the network.
            AppLog.Info($"wifi names: report: {e.Message}");
        }
        finally
        {
            sending = false;
        }
    }

    /// <summary>
    /// Asks Windows for location access, which reading Wi-Fi names needs:
    /// the first time, Windows asks the user. Denied only by the user or by
    /// Windows (location services off); anything else Windows answers, or
    /// failing to ask, is not known (<see cref="WifiNames.Access"/>), and
    /// checked again at the next tick.
    /// </summary>
    private static async Task<WifiAccess> RequestAccessAsync()
    {
        try
        {
            return WifiNames.Access((await WiFiAdapter.RequestAccessAsync()).ToString());
        }
        catch (Exception e)
        {
            AppLog.Info($"wifi names: cannot ask for location access: {e.Message}");
            return WifiAccess.Unknown;
        }
    }

    /// <summary>Whether the app may read Wi-Fi names now, as the user last decided in Windows' location settings.</summary>
    private static WifiAccess Access()
    {
        try
        {
            return WifiNames.Access(AppCapability.Create("wiFiControl").CheckAccess().ToString());
        }
        catch (Exception e)
        {
            // Not a denial: asked again at the next tick.
            AppLog.Info($"wifi names: cannot check location access: {e.Message}");
            return WifiAccess.Unknown;
        }
    }

    /// <summary>
    /// The Wi-Fi name of the network this computer reaches the internet by,
    /// empty when that is not over Wi-Fi; null when it can't be read
    /// (<see cref="WifiNames.Outcome"/>), which is not reported.
    /// </summary>
    private static string? ReadSsid()
    {
        try
        {
            var profile = NetworkInformation.GetInternetConnectionProfile();
            bool? onWifi = profile?.IsWlanConnectionProfile;
            return WifiNames.Outcome(onWifi, onWifi == true ? profile!.WlanConnectionProfileDetails?.GetConnectedSsid() : null);
        }
        catch (Exception e)
        {
            AppLog.Info($"wifi names: cannot read the Wi-Fi name: {e.Message}");
            return null;
        }
    }
}
