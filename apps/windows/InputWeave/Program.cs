using InputWeave.Support;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;

namespace InputWeave;

/// <summary>
/// Starts the app, once per Windows session. The arguments it understands
/// are in <see cref="LaunchArguments"/> (from Windows and the background
/// service) and <see cref="Launch"/> (for testing), which also decides what
/// opens at launch.
/// </summary>
public static class Program
{
    [STAThread]
    private static int Main(string[] args)
    {
        WinRT.ComWrappersSupport.InitializeComWrappers();
        if (args.Contains(LaunchArguments.Unregister))
        {
            // Into the user's log only when there is one: uninstalling must
            // not leave a new file behind.
            if (File.Exists(AppLog.DefaultPath))
            {
                AppLog.Open(AppLog.DefaultPath);
            }
            AppLog.Info("removing what the app registered for this user, for the uninstaller");
            App.Unregister();
            return 0;
        }
        // One app per Windows session. Opening it again shows the panel of
        // the one already running, or hands it the files to send.
        using var instance = new Mutex(initiallyOwned: true, SingleInstance.MutexName, out bool first);
        if (!first)
        {
            AppLog.Open(AppLog.DefaultPath);
            AppLog.Info($"already running; handing over {args.Length} arguments");
            SingleInstance.SignalRunningInstance(args);
            return 0;
        }
        var language = Array.IndexOf(args, Launch.LanguageArgument);
        if (language >= 0 && language + 1 < args.Length)
        {
            Microsoft.Windows.Globalization.ApplicationLanguages.PrimaryLanguageOverride = args[language + 1];
        }
        Application.Start(callback =>
        {
            var context = new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread());
            SynchronizationContext.SetSynchronizationContext(context);
            _ = new App(args);
        });
        GC.KeepAlive(instance);
        return 0;
    }
}
