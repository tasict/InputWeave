using System.ComponentModel;
using System.Diagnostics;

namespace InputWeave.Support;

/// <summary>File Explorer, and the elevated commands that start the InputWeave service.</summary>
internal static class Shell
{
    /// <summary>Opens File Explorer with <paramref name="path"/> selected, or its folder when the file is gone.</summary>
    public static void ShowInExplorer(string path)
    {
        try
        {
            if (File.Exists(path))
            {
                Process.Start(new ProcessStartInfo("explorer.exe") { Arguments = $"/select,\"{path}\"", UseShellExecute = true })?.Dispose();
            }
            else if (Path.GetDirectoryName(path) is { } folder && Directory.Exists(folder))
            {
                OpenFolder(folder);
            }
        }
        catch (Exception e) when (e is Win32Exception or InvalidOperationException)
        {
            AppLog.Warn($"cannot open File Explorer: {e.Message}");
        }
    }

    public static void OpenFolder(string folder)
    {
        try
        {
            Process.Start(new ProcessStartInfo("explorer.exe") { Arguments = $"\"{folder}\"", UseShellExecute = true })?.Dispose();
        }
        catch (Exception e) when (e is Win32Exception or InvalidOperationException)
        {
            AppLog.Warn($"cannot open File Explorer: {e.Message}");
        }
    }

    /// <summary>
    /// Restarts the InputWeave service as an administrator; Windows asks
    /// through UAC first. <c>net stop</c> waits until the service has
    /// stopped, which <c>sc stop</c> does not.
    /// </summary>
    public static void RestartService()
    {
        var name = ServiceWatcher.ServiceName;
        // Its stopping on the way is not news.
        App.Instance.ServiceRestarting();
        RunElevated(
            Path.Combine(Environment.SystemDirectory, "cmd.exe"),
            $"/c net stop {name} & net start {name}",
            "restart the InputWeave service");
    }

    public static void RunElevated(string program, string arguments, string what)
    {
        try
        {
            using var process = Process.Start(new ProcessStartInfo
            {
                FileName = program,
                Arguments = arguments,
                Verb = "runas",
                UseShellExecute = true,
                WindowStyle = ProcessWindowStyle.Hidden,
            });
            AppLog.Info($"asked to {what}");
        }
        catch (Win32Exception e) when (e.NativeErrorCode == 1223)
        {
            // ERROR_CANCELLED: the user said no to UAC.
            AppLog.Info($"{what}: cancelled at UAC");
        }
        catch (Win32Exception e)
        {
            AppLog.Warn($"cannot {what}: {e.Message}");
        }
    }
}
