using System.Diagnostics;
using System.IO.Pipes;

namespace InputWeave.Agent;

/// <summary>
/// The background service's pipe for this Windows session,
/// <c>\\.\pipe\InputWeave\agent-&lt;SessionId&gt;</c> (iw-ipc's
/// <c>agent_pipe_name</c>). Its DACL lets in only SYSTEM and the user logged
/// on to that session.
/// </summary>
public static class AgentPipe
{
    /// <summary>
    /// How long one attempt waits for the pipe. With no agent listening,
    /// the attempt fails once this passes.
    /// </summary>
    public static readonly TimeSpan ConnectTimeout = TimeSpan.FromMilliseconds(500);

    public static int CurrentSessionId => Process.GetCurrentProcess().SessionId;

    /// <summary>The name relative to <c>\\.\pipe\</c>.</summary>
    public static string NameFor(int sessionId) => $@"InputWeave\agent-{sessionId}";

    public static string PathFor(int sessionId) => $@"\\.\pipe\{NameFor(sessionId)}";

    public static async Task<Stream> ConnectAsync(CancellationToken cancellation)
    {
        // Byte mode, as the agent's pipe is: frames carry their own length.
        var pipe = new NamedPipeClientStream(
            ".", NameFor(CurrentSessionId), PipeDirection.InOut, PipeOptions.Asynchronous);
        try
        {
            await pipe.ConnectAsync(ConnectTimeout, cancellation).ConfigureAwait(false);
            return pipe;
        }
        catch
        {
            await pipe.DisposeAsync().ConfigureAwait(false);
            throw;
        }
    }
}
