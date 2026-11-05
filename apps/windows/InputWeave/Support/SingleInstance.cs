using System.IO.Pipes;
using System.Text;
using InputWeave.Agent;

namespace InputWeave.Support;

/// <summary>
/// One app per Windows session. A second launch hands its arguments to the
/// first, which handles them as if it had been started with them (opening
/// the panel, or sending files from File Explorer's Send To), and exits.
/// The arguments go over a pipe only the same user can open; an older app
/// that only listens for the signal still opens its panel.
/// </summary>
internal static class SingleInstance
{
    public const string MutexName = @"Local\InputWeave.App";
    private const string EventName = @"Local\InputWeave.App.Show";
    /// <summary>Arguments are paths, so this is plenty.</summary>
    private const int MaxMessage = 1 << 20;

    private static EventWaitHandle? signal;
    private static RegisteredWaitHandle? registration;
    private static CancellationTokenSource? stopping;

    private static string PipeName => $"InputWeave.App.{AgentPipe.CurrentSessionId}";

    public static void SignalRunningInstance(IReadOnlyList<string> args)
    {
        // This launch came from the user, so it may hand the foreground
        // on: the panel can then take focus, and close when it loses it.
        Native.AllowSetForegroundWindow(Native.ASFW_ANY);
        if (Forward(args))
        {
            return;
        }
        try
        {
            using var show = EventWaitHandle.OpenExisting(EventName);
            show.Set();
        }
        catch (WaitHandleCannotBeOpenedException)
        {
            // The first instance is still starting, or just quit.
        }
    }

    private static bool Forward(IReadOnlyList<string> args)
    {
        try
        {
            using var pipe = new NamedPipeClientStream(".", PipeName, PipeDirection.Out, PipeOptions.CurrentUserOnly);
            pipe.Connect(1000);
            pipe.Write(Encoding.UTF8.GetBytes(LaunchArguments.Encode(args)));
            pipe.Flush();
            return true;
        }
        catch (Exception e) when (e is TimeoutException or IOException or UnauthorizedAccessException)
        {
            return false;
        }
    }

    /// <summary>Calls <paramref name="opened"/> on a pool thread with the arguments of every later launch.</summary>
    public static void Listen(Action<IReadOnlyList<string>> opened)
    {
        signal = new EventWaitHandle(false, EventResetMode.AutoReset, EventName);
        registration = ThreadPool.RegisterWaitForSingleObject(
            signal, (_, _) => opened([]), null, Timeout.Infinite, executeOnlyOnce: false);
        stopping = new CancellationTokenSource();
        var token = stopping.Token;
        _ = Task.Run(() => ServeAsync(opened, token));
    }

    private static async Task ServeAsync(Action<IReadOnlyList<string>> opened, CancellationToken stop)
    {
        while (!stop.IsCancellationRequested)
        {
            try
            {
                using var server = new NamedPipeServerStream(
                    PipeName, PipeDirection.In, 1, PipeTransmissionMode.Byte,
                    PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
                await server.WaitForConnectionAsync(stop).ConfigureAwait(false);
                using var message = new MemoryStream();
                var buffer = new byte[16 * 1024];
                int read;
                while ((read = await server.ReadAsync(buffer, stop).ConfigureAwait(false)) > 0 && message.Length < MaxMessage)
                {
                    message.Write(buffer, 0, read);
                }
                opened(LaunchArguments.Decode(Encoding.UTF8.GetString(message.GetBuffer(), 0, (int)Math.Min(message.Length, MaxMessage))));
            }
            catch (OperationCanceledException)
            {
                break;
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException)
            {
                AppLog.Warn($"another launch could not reach this one: {e.Message}");
                try
                {
                    await Task.Delay(1000, stop).ConfigureAwait(false);
                }
                catch (OperationCanceledException)
                {
                    break;
                }
            }
        }
    }

    public static void Stop()
    {
        stopping?.Cancel();
        registration?.Unregister(null);
        signal?.Dispose();
    }
}
