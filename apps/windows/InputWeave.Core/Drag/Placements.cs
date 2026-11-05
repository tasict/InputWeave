using InputWeave.Support;

namespace InputWeave.Drag;

/// <summary>
/// Files copied on another computer go on the clipboard one promise at a
/// time, in the order they came (docs/architecture.md §9): each is asked
/// about, put there and said so before the next. However a step fails, that
/// promise ends there and the next goes on; putting one there that fails,
/// or takes longer than <see cref="PlaceLimit"/>, counts as not put there,
/// and a placement given up on is told not to start.
/// </summary>
public sealed class Placements(TimeSpan? limit = null)
{
    /// <summary>
    /// How long putting a promise on the clipboard may take: under the five
    /// seconds the background service waits to hear whether it is there.
    /// </summary>
    public static readonly TimeSpan PlaceLimit = TimeSpan.FromSeconds(4);

    private readonly SemaphoreSlim order = new(1, 1);
    private readonly TimeSpan limit = limit ?? PlaceLimit;

    /// <summary>
    /// Asks whether promise <paramref name="id"/> may go on the clipboard
    /// (<paramref name="ask"/>: null when it may, else the refusal's code),
    /// puts it there (<paramref name="place"/>, which a token cancels once
    /// it is given up on) and says whether it did (<paramref name="say"/>),
    /// after the promises before it.
    /// </summary>
    public async Task RunAsync(
        ulong id,
        Func<Task<string?>> ask,
        Func<CancellationToken, Task<bool>> place,
        Func<bool, Task> say)
    {
        await order.WaitAsync().ConfigureAwait(false);
        try
        {
            string? refused;
            try
            {
                refused = await ask().ConfigureAwait(false);
            }
            catch (Exception e)
            {
                AppLog.Warn($"clipboard files {id}: cannot ask to put them on the clipboard: {e.Message}");
                return;
            }
            if (refused is not null)
            {
                AppLog.Info($"clipboard files {id}: not put on the clipboard ({refused})");
                return;
            }
            bool ok;
            using var giveUp = new CancellationTokenSource();
            try
            {
                ok = await place(giveUp.Token).WaitAsync(limit).ConfigureAwait(false);
            }
            catch (Exception e)
            {
                giveUp.Cancel();
                AppLog.Warn($"clipboard files {id}: cannot put them on the clipboard: {e.Message}");
                ok = false;
            }
            try
            {
                await say(ok).ConfigureAwait(false);
            }
            catch (Exception e)
            {
                AppLog.Info($"clipboard files {id}: cannot say whether they are on the clipboard: {e.Message}");
            }
        }
        finally
        {
            order.Release();
        }
    }
}
