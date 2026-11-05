namespace InputWeave.Support;

/// <summary>
/// The InputWeave Windows service as the service control manager reports
/// it. The service starts and watches the background service (the agent),
/// so while it is stopped nothing answers the app.
/// </summary>
public enum ServiceState
{
    /// <summary>Not known yet, or the query failed.</summary>
    Unknown,
    Running,
    Starting,
    Stopping,
    Stopped,
    /// <summary>No such service: InputWeave was not installed with its MSI.</summary>
    NotInstalled,
}
