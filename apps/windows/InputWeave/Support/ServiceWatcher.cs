using System.ComponentModel;
using System.Diagnostics;
using System.ServiceProcess;

namespace InputWeave.Support;

/// <summary>
/// Watches the InputWeave Windows service, which starts the background
/// service: while it is stopped, nothing will answer the app, and the
/// panel offers to start it. Reading its state needs no administrator
/// rights; starting it goes through UAC.
/// </summary>
internal sealed class ServiceWatcher : IDisposable
{
    /// <summary>The name the MSI installs it under (packaging/windows/InputWeave.wxs).</summary>
    public const string ServiceName = "InputWeave";

    private static readonly TimeSpan Interval = TimeSpan.FromSeconds(2);

    private readonly Timer timer;
    private readonly object gate = new();
    private ServiceState state = ServiceState.Unknown;

    public ServiceWatcher()
    {
        timer = new Timer(_ => Poll(), null, TimeSpan.Zero, Interval);
    }

    /// <summary>Raised on a background thread when <see cref="State"/> changes.</summary>
    public event EventHandler? Changed;

    public ServiceState State
    {
        get
        {
            lock (gate)
            {
                return state;
            }
        }
    }

    private void Poll()
    {
        var now = Query();
        lock (gate)
        {
            if (now == state)
            {
                return;
            }
            state = now;
        }
        AppLog.Info($"InputWeave service: {now}");
        Changed?.Invoke(this, EventArgs.Empty);
    }

    private static ServiceState Query()
    {
        try
        {
            using var service = new ServiceController(ServiceName);
            return service.Status switch
            {
                ServiceControllerStatus.Running => ServiceState.Running,
                ServiceControllerStatus.StartPending or ServiceControllerStatus.ContinuePending => ServiceState.Starting,
                ServiceControllerStatus.StopPending or ServiceControllerStatus.PausePending => ServiceState.Stopping,
                ServiceControllerStatus.Stopped or ServiceControllerStatus.Paused => ServiceState.Stopped,
                _ => ServiceState.Unknown,
            };
        }
        catch (InvalidOperationException e) when (e.InnerException is Win32Exception { NativeErrorCode: 1060 })
        {
            // ERROR_SERVICE_DOES_NOT_EXIST
            return ServiceState.NotInstalled;
        }
        catch (Exception e) when (e is InvalidOperationException or Win32Exception)
        {
            return ServiceState.Unknown;
        }
    }

    /// <summary>Starts the service as an administrator; Windows asks through UAC first.</summary>
    public static void Start()
    {
        try
        {
            using var process = Process.Start(new ProcessStartInfo
            {
                FileName = Path.Combine(Environment.SystemDirectory, "sc.exe"),
                Arguments = $"start {ServiceName}",
                Verb = "runas",
                UseShellExecute = true,
                WindowStyle = ProcessWindowStyle.Hidden,
            });
            AppLog.Info("asked to start the InputWeave service");
        }
        catch (Win32Exception e) when (e.NativeErrorCode == 1223)
        {
            // ERROR_CANCELLED: the user said no to UAC.
            AppLog.Info("starting the InputWeave service was cancelled at UAC");
        }
        catch (Win32Exception e)
        {
            AppLog.Warn($"cannot start the InputWeave service: {e.Message}");
        }
    }

    public void Dispose() => timer.Dispose();
}
