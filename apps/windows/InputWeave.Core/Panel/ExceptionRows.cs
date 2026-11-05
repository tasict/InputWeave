using InputWeave.Agent;
using InputWeave.Support;

namespace InputWeave.Panel;

/// <summary>What a row's button does.</summary>
public enum ExceptionAction
{
    None,
    /// <summary>Starts the InputWeave service, through UAC.</summary>
    StartService,
    /// <summary>Opens Troubleshooting in the main window.</summary>
    Troubleshoot,
    /// <summary>Opens Add Device in the main window.</summary>
    AddDevice,
}

public sealed record ExceptionRow(UiText Text, ExceptionAction Action = ExceptionAction.None)
{
    /// <summary>The button's words, or null for a row without one.</summary>
    public string? ActionKey => Action switch
    {
        ExceptionAction.StartService => TextKeys.ActionStartService,
        ExceptionAction.Troubleshoot => TextKeys.ActionTroubleshoot,
        ExceptionAction.AddDevice => TextKeys.ActionAddDevice,
        _ => null,
    };
}

/// <summary>
/// Everything currently keeping sharing from working, each with the one
/// action that fixes it when the app has one. Empty
/// when all is well. Windows needs no permissions, and secure input is a
/// macOS matter, so those rows of the macOS panel never show here.
/// </summary>
public static class ExceptionRows
{
    /// <param name="compact">
    /// The panel's rows, which lead on to the main window; Troubleshooting
    /// shows the same problems without those.
    /// </param>
    public static IReadOnlyList<ExceptionRow> For(AgentSnapshot model, ServiceState service, bool compact = true)
    {
        var rows = new List<ExceptionRow>();
        if (service == ServiceState.Stopped && model.Link != AgentLink.Connected)
        {
            rows.Add(new(new(TextKeys.ExceptionServiceStopped), ExceptionAction.StartService));
        }
        if (model.Link == AgentLink.Connected)
        {
            foreach (var device in model.OfflineDevices)
            {
                rows.Add(new(
                    new(TextKeys.ExceptionUnreachable, device.Name),
                    compact ? ExceptionAction.Troubleshoot : ExceptionAction.None));
            }
            if (model.Status.Devices.Count == 0 && compact)
            {
                rows.Add(new(new(TextKeys.ExceptionNoDevices), ExceptionAction.AddDevice));
            }
        }
        return rows;
    }

    /// <summary>Whether Troubleshooting can say everything is working.</summary>
    public static bool EverythingWorks(AgentSnapshot model, ServiceState service) =>
        service is ServiceState.Running or ServiceState.Unknown
        && model.Link == AgentLink.Connected
        && model.OfflineDevices.Count == 0
        && model.Status.Capturing;
}
