using InputWeave.Support;

namespace InputWeave.Notifications;

/// <summary>What to do about the "service stopped" notification.</summary>
public enum ServiceNews
{
    None,
    /// <summary>Post it: the service was running and has stopped.</summary>
    Raise,
    /// <summary>Take it back: the service runs again.</summary>
    Withdraw,
}

/// <summary>
/// Decides when the InputWeave service stopping gets a system notification,
/// as the macOS app's ProblemWatch does for
/// its background service; Windows has no permissions to watch. Only a
/// change is news: a service that was already stopped when the app started
/// shows in the panel and in Troubleshooting instead. The service must have
/// stayed stopped for <see cref="StoppedFor"/>, longer than Windows takes to
/// restart it after a crash, and nothing is posted before setup finished or
/// while the app restarts the service itself.
/// </summary>
public sealed class ServiceWatch
{
    /// <summary>How long the service must stay stopped before it is news.</summary>
    public static readonly TimeSpan StoppedFor = TimeSpan.FromSeconds(10);

    /// <summary>How long a restart the app asked for may take, UAC included, before a stop counts again.</summary>
    public static readonly TimeSpan RestartAllowance = TimeSpan.FromMinutes(2);

    private bool sawRunning;
    private DateTimeOffset? stoppedSince;
    /// <summary>This stop has been dealt with: notified, or passed over while quiet.</summary>
    private bool handled;
    private bool notified;
    private DateTimeOffset? restartAsked;
    private bool restartStopped;

    /// <summary>The app is about to restart the service (stop, then start) itself.</summary>
    public void Restarting(DateTimeOffset now)
    {
        restartAsked = now;
        restartStopped = false;
    }

    /// <param name="quiet">Remembers without raising: before setup has finished.</param>
    public ServiceNews Update(ServiceState state, DateTimeOffset now, bool quiet)
    {
        if (restartAsked is { } asked && now - asked > RestartAllowance)
        {
            restartAsked = null;
        }
        switch (state)
        {
            case ServiceState.Running:
                sawRunning = true;
                stoppedSince = null;
                handled = false;
                if (restartStopped)
                {
                    // The restart the app asked for is done.
                    restartAsked = null;
                    restartStopped = false;
                }
                if (notified)
                {
                    notified = false;
                    return ServiceNews.Withdraw;
                }
                return ServiceNews.None;
            case ServiceState.Stopped:
                stoppedSince ??= now;
                if (restartAsked is not null)
                {
                    restartStopped = true;
                    return ServiceNews.None;
                }
                if (handled || !sawRunning || now - stoppedSince < StoppedFor)
                {
                    return ServiceNews.None;
                }
                handled = true;
                if (quiet)
                {
                    return ServiceNews.None;
                }
                notified = true;
                return ServiceNews.Raise;
            case ServiceState.Stopping:
            case ServiceState.Starting:
                if (restartAsked is not null)
                {
                    restartStopped = true;
                }
                return ServiceNews.None;
            default:
                // Not known, or not installed any more: nothing to say.
                return ServiceNews.None;
        }
    }
}
