using Inputweave.App.V1;
using InputWeave.Agent;

namespace InputWeave.Licensing;

/// <summary>The license commands the worker sends to the background service.</summary>
public interface ILicenseAgent
{
    /// <summary>The app is connected to the service.</summary>
    bool Linked { get; }

    /// <summary>The license the service last reported (Status.license), or null from a service without licenses.</summary>
    License? Current { get; }

    Task SetKeyAsync(string key);

    Task RecordAsync(RecordLicenseResult result);

    Task RemoveAsync();
}

/// <summary>The service's commands, through the app's connection to it.</summary>
public sealed class AgentLicense(AgentModel model) : ILicenseAgent
{
    public bool Linked => model.Snapshot.Link == AgentLink.Connected;

    public License? Current => model.Snapshot.Status.License;

    public Task SetKeyAsync(string key) => model.SetLicenseKeyAsync(key);

    public Task RecordAsync(RecordLicenseResult result) => model.RecordLicenseResultAsync(result);

    public Task RemoveAsync() => model.RemoveLicenseAsync();
}

/// <summary>What asking Polar to activate this computer came to, for the Enter License Key dialog and the License page.</summary>
public enum ActivationOutcome
{
    Licensed,
    SeatsFull,
    /// <summary>Unknown, revoked or refunded key.</summary>
    Invalid,
    /// <summary>Polar could not be reached; the key waits, nothing changed.</summary>
    Offline,
    /// <summary>Polar asked the app to slow down (429).</summary>
    Busy,
    /// <summary>
    /// The license could not be saved: the background service did not take
    /// what Polar said, Polar could not read the request, or its answer could
    /// not be read. A granted activation is kept and given to the service later.
    /// </summary>
    NotRecorded,
    /// <summary>The background service cannot be reached.</summary>
    ServiceNotRunning,
    /// <summary>License information cannot be read now (the service's license file, or the app's own work).</summary>
    Unavailable,
    /// <summary>
    /// The background service refused the answer for good: nothing is tried
    /// again for this key until the user enters a key or the service's
    /// license changes.
    /// </summary>
    ServiceRefused,
}

/// <summary>What the first attempt at Remove License came to.</summary>
public enum RemoveOutcome
{
    Removed,
    /// <summary>Removed here, but Polar could not be reached to free the seat; the app frees it later.</summary>
    RemovedSeatNotFreed,
    /// <summary>The background service did not take it; the app keeps asking it.</summary>
    ServicePending,
    /// <summary>The app's own license work cannot be read now, so nothing was done.</summary>
    Unavailable,
}

/// <summary>
/// Does the license work the background service's license needs from Polar
/// (docs/architecture.md §11). The service keeps the license; this keeps
/// the work (<see cref="LicenseWork"/>, per user, through
/// <see cref="ILicenseLedger"/>), saved before each step it leads to, so a
/// restart carries on rather than starting over:
/// <list type="bullet">
/// <item>Activate a key waiting for activation. Granted: the activation is
/// saved as a Report before anything else. A 403 is told apart by
/// validating the key alone (seats full, or invalid); a 404, or a 422 about
/// the key, is invalid; out of reach, or a 429, waits, longer each time. A
/// 200 whose answer cannot be read is never asked again by itself (it may
/// have taken a seat): the key is left alone until the user acts.</item>
/// <item>Report, and the other answers for the service: taken (or already
/// licensed with that activation), done; refused with any code but
/// license_unavailable and agent_stopping, the activation is freed and the
/// key left alone for that license; those two, no answer, or a broken
/// connection, tried again a minute later.</item>
/// <item>Confirm an activation at most every 30 days, while the License
/// page or About is open; a key is invalid only after two 404s a minute
/// apart, and out of reach changes nothing.</item>
/// <item>Free an activation, with a growing wait until Polar frees it or
/// answers that there is nothing to free; it needs no service.</item>
/// <item>Remove License: the service is asked to remove the license, and
/// asked again until it has; then this computer's seat is freed. Both are
/// kept before either is tried. Refused for good, the license stays, and so
/// does the seat.</item>
/// </list>
/// The work runs in two lanes, one piece at a time each, the oldest first:
/// the service's and Polar's, so the user's key never waits behind a slow
/// request to Polar. Each piece waits on its own; one waiting holds up no
/// other. The app's ticks drive the work, so nothing here wakes up by
/// itself. A change to the work that could not be kept is kept again
/// before anything but freeing runs, so nothing is reported before it would
/// survive a restart; work that cannot be read is neither replaced nor
/// acted on until it can. Polar's Retry-After is kept too, and nothing
/// (not even the user) asks Polar again sooner. What the License page shows
/// about the work comes from the work and the service's license alone.
/// </summary>
public sealed class LicenseWorker
{
    public static readonly TimeSpan ConfirmEvery = TimeSpan.FromDays(30);

    /// <summary>The least time between two attempts to confirm, when Polar did not answer.</summary>
    public static readonly TimeSpan ConfirmRetry = TimeSpan.FromHours(1);

    /// <summary>A 404 from validate counts once a second one comes at least this much later.</summary>
    public static readonly TimeSpan SecondNotFound = TimeSpan.FromMinutes(1);

    /// <summary>
    /// The first wait before a key is activated again after Polar did not
    /// answer; it doubles each time, up to <see cref="MaxActivateWait"/>. An
    /// answer lost on the way may have taken a seat, so activating again is
    /// never hurried.
    /// </summary>
    public static readonly TimeSpan ActivateRetry = TimeSpan.FromMinutes(5);

    public static readonly TimeSpan MaxActivateWait = TimeSpan.FromHours(1);

    /// <summary>How soon the background service is asked again after it did not take an answer, and the work read again when it could not be.</summary>
    public static readonly TimeSpan ServiceRetry = TimeSpan.FromMinutes(1);

    /// <summary>How long the dialogs wait on the background service before saying it is not answering.</summary>
    public static readonly TimeSpan ServiceWait = TimeSpan.FromSeconds(10);

    /// <summary>The longest wait between two attempts to free an activation.</summary>
    public static readonly TimeSpan MaxFreeWait = TimeSpan.FromHours(6);

    /// <summary>The most pieces of work done in one go; each either finishes or waits, so this is only a guard.</summary>
    private const int MaxSteps = 64;

    private readonly PolarClient polar;
    private readonly ILicenseAgent agent;
    private readonly ILicenseLedger ledger;
    private readonly Func<DateTimeOffset> clock;
    private readonly Func<string> label;
    private readonly Action<string> log;
    private readonly object sync = new();
    private readonly SemaphoreSlim polarLane = new(1, 1);
    private readonly SemaphoreSlim agentLane = new(1, 1);
    private LicenseWork work = new();
    /// <summary>The last change to the work could not be kept: nothing but freeing runs until it is.</summary>
    private bool unsaved;
    /// <summary>The kept work could not be read: nothing is written or done until it can.</summary>
    private bool unreadable;
    private DateTimeOffset nextRead;
    /// <summary>When Remove License was confirmed, for saying it has not finished.</summary>
    private DateTimeOffset? removeStarted;
    /// <summary>The last confirmation asked for, so one the service did not take is not asked again at once.</summary>
    private (string Key, string ActivationId, DateTimeOffset At)? lastConfirm;

    public LicenseWorker(PolarClient polar, ILicenseAgent agent, ILicenseLedger ledger, Func<DateTimeOffset> clock, Func<string> label, Action<string> log)
    {
        this.polar = polar;
        this.agent = agent;
        this.ledger = ledger;
        this.clock = clock;
        this.label = label;
        this.log = log;
        unreadable = true;
        Read(force: true);
    }

    /// <summary>The work owed, in order; for the tests and the log.</summary>
    public IReadOnlyList<LicenseOp> Ops => work.Ops;

    /// <summary>The app's own license work cannot be read now; it is read again a minute later.</summary>
    public bool Unreadable => unreadable;

    /// <summary>Remove License is owed to the service: the license shows as a trial meanwhile.</summary>
    public bool Removing => work.Ops.Any(o => o.Kind == LicenseOpKind.Remove);

    /// <summary>
    /// Removing the license from the service has not finished: an attempt
    /// failed, or the first is taking longer than <see cref="ServiceWait"/>.
    /// The app keeps trying.
    /// </summary>
    public bool RemoveFailed => work.Ops.Any(o => o.Kind == LicenseOpKind.Remove
        && (o.Attempts > 0 || (removeStarted is { } started && clock() - started >= ServiceWait)));

    /// <summary>Freeing the seat Remove License gave up failed; it waits for Polar (or the customer portal).</summary>
    public bool SeatNotFreed => work.Ops.Any(o => o.Kind == LicenseOpKind.Free && o.Removed && o.Attempts > 0);

    /// <summary>
    /// The work takes this license away: it is removed from the service, or
    /// its activation freed with Polar. Not licensed meanwhile, whatever the
    /// service still says.
    /// </summary>
    public bool Withdrawing(License? license) => Removing
        || (license is { ActivationId.Length: > 0 } && work.Ops.Any(o => o.Kind == LicenseOpKind.Free && o.ActivationId == license.ActivationId));

    /// <summary>What stands in the way of activating <paramref name="key"/>, for the License page; null when nothing does.</summary>
    public ActivationOutcome? Problem(string key)
    {
        if (key.Length == 0)
        {
            return null;
        }
        if (work.Stalled is { } stalled && stalled.Key == key)
        {
            return stalled.Unsaved ? ActivationOutcome.NotRecorded : ActivationOutcome.ServiceRefused;
        }
        foreach (var op in work.Ops.Where(o => o.Key == key && o.Attempts > 0))
        {
            switch (op.Kind)
            {
                case LicenseOpKind.Report or LicenseOpKind.Refuse:
                    return op.LastFailure == LicenseOpFailure.LicenseUnavailable ? ActivationOutcome.Unavailable : ActivationOutcome.NotRecorded;
                case LicenseOpKind.Activate:
                    return op.LastFailure switch
                    {
                        LicenseOpFailure.RateLimited => ActivationOutcome.Busy,
                        LicenseOpFailure.Rejected => ActivationOutcome.NotRecorded,
                        _ => ActivationOutcome.Offline,
                    };
            }
        }
        return null;
    }

    /// <summary>
    /// Adds the work the service's license calls for and does whatever is
    /// due; called on every change and on the app's ticks. A lane already at
    /// work is left to it. <paramref name="shown"/>: the License page or
    /// About is open.
    /// </summary>
    public async Task TickAsync(bool shown)
    {
        if (!Read(force: false))
        {
            return;
        }
        Resave();
        bool linked = agent.Linked;
        if (linked)
        {
            Plan(shown);
        }
        await Task.WhenAll(
            DrainAsync(polarLane, shown, linked),
            DrainAsync(agentLane, shown, linked)).ConfigureAwait(false);
    }

    /// <summary>
    /// Enter License Key, and Try Again with the same key: the user acting.
    /// The key's work waits no longer, but for activating it again (a lost
    /// answer may have taken a seat) and Polar's own Retry-After; the key is
    /// no longer left alone, and Polar's earlier refusals of it are
    /// forgotten (it is asked again). A removal still owed goes first; then
    /// the key goes to the service, which starts it over as waiting for
    /// activation, and the key's work is done at once: reporting an
    /// activation Polar granted it before, or activating it. What it comes
    /// to is what happened now, never an earlier answer.
    /// </summary>
    public async Task<ActivationOutcome> EnterAsync(string key)
    {
        key = key.Trim();
        if (!Read(force: true))
        {
            return ActivationOutcome.Unavailable;
        }
        Resave();
        await agentLane.WaitAsync().ConfigureAwait(false);
        try
        {
            Mutate(w => w with
            {
                Stalled = null,
                Ops = w.Ops
                    .Where(o => !(o.Key == key && o.Kind == LicenseOpKind.Refuse))
                    .Select(o => (o.Key == key && o.Kind != LicenseOpKind.Activate) || o.Kind == LicenseOpKind.Remove
                        ? o with { NextAttempt = DateTimeOffset.MinValue }
                        : o)
                    .ToList(),
            });
            // A removal still owed goes first, or it would take away the key entered now.
            if (work.Ops.FirstOrDefault(o => o.Kind == LicenseOpKind.Remove) is { } remove)
            {
                await AttemptAsync(remove).ConfigureAwait(false);
                if (Removing)
                {
                    return agent.Linked ? ActivationOutcome.NotRecorded : ActivationOutcome.ServiceNotRunning;
                }
            }
            try
            {
                await agent.SetKeyAsync(key).ConfigureAwait(false);
            }
            catch (AgentRefusal refusal) when (refusal.Code == "invalid_license_key")
            {
                return ActivationOutcome.Invalid;
            }
            catch (AgentRefusal refusal) when (refusal.Code == "license_unavailable")
            {
                return ActivationOutcome.Unavailable;
            }
            catch (AgentUnavailableException e)
            {
                log($"license: cannot reach the background service: {e.Message}");
                return ActivationOutcome.ServiceNotRunning;
            }
            catch (AgentRefusal refusal)
            {
                log($"license: the background service did not take the key: {refusal.Code}");
                return ActivationOutcome.NotRecorded;
            }
            // The key this computer is already licensed with keeps its activation.
            if (agent.Current is { State: LicenseState.Licensed } current && current.Key == key)
            {
                return ActivationOutcome.Licensed;
            }
            Mutate(w => w.Ops.Any(o => o.Key == key && o.Kind is LicenseOpKind.Activate or LicenseOpKind.Report)
                ? w
                : w with { Ops = [.. w.Ops, new LicenseOp { Kind = LicenseOpKind.Activate, Key = key }] });
        }
        finally
        {
            agentLane.Release();
        }
        // Only the key's own work, each piece in its lane, so the dialog waits for nothing else.
        ActivationOutcome? outcome = null;
        for (int step = 0; step < MaxSteps; step++)
        {
            var now = clock();
            var op = work.Ops.FirstOrDefault(o => o.Key == key && o.Kind is not (LicenseOpKind.Confirm or LicenseOpKind.Confirmed)
                && Due(o, now, shown: false, linked: true));
            if (op is null)
            {
                break;
            }
            outcome = await RunInLaneAsync(op).ConfigureAwait(false) ?? outcome;
        }
        return outcome ?? Settled(key) ?? Problem(key) ?? (unsaved ? ActivationOutcome.NotRecorded
            : clock() < work.PolarNotBefore ? ActivationOutcome.Busy
            : ActivationOutcome.Offline);
    }

    /// <summary>
    /// Remove License: queues the license to be removed from the service
    /// and then this computer's seat to be freed with Polar, both kept
    /// before either is tried, then makes the first attempt at both.
    /// </summary>
    public async Task<RemoveOutcome> RemoveAsync()
    {
        if (!Read(force: true))
        {
            return RemoveOutcome.Unavailable;
        }
        Resave();
        var remove = new LicenseOp { Kind = LicenseOpKind.Remove };
        await agentLane.WaitAsync().ConfigureAwait(false);
        try
        {
            removeStarted = clock();
            var license = agent.Current;
            Mutate(w =>
            {
                var ops = new List<LicenseOp>();
                foreach (var op in w.Ops)
                {
                    switch (op.Kind)
                    {
                        case LicenseOpKind.Free:
                            ops.Add(op);
                            break;
                        case LicenseOpKind.Report:
                            // Granted, never taken: a seat to free like any other.
                            ops.Add(new LicenseOp { Kind = LicenseOpKind.Free, Key = op.Key, ActivationId = op.ActivationId });
                            break;
                    }
                }
                ops.Add(remove);
                // Freed once the service has removed the license (Due holds it back until then).
                if (license is { Key.Length: > 0, ActivationId.Length: > 0 } && !ops.Any(o => o.ActivationId == license.ActivationId))
                {
                    ops.Add(new LicenseOp { Kind = LicenseOpKind.Free, Key = license.Key, ActivationId = license.ActivationId, Removed = true });
                }
                return w with { Ops = ops, Stalled = null };
            });
        }
        finally
        {
            agentLane.Release();
        }
        // The first attempt: the service, then the seats to free that are due.
        await RunInLaneAsync(remove).ConfigureAwait(false);
        foreach (var free in work.Ops.Where(o => o.Kind == LicenseOpKind.Free && Due(o, clock(), shown: false, linked: false)).ToList())
        {
            await RunInLaneAsync(free).ConfigureAwait(false);
        }
        return Removing ? RemoveOutcome.ServicePending
            : SeatNotFreed ? RemoveOutcome.RemovedSeatNotFreed
            : RemoveOutcome.Removed;
    }

    /// <summary>Reads the kept work when it could not be read before (at most once a minute unless <paramref name="force"/>); false while it still cannot be.</summary>
    private bool Read(bool force)
    {
        lock (sync)
        {
            if (!unreadable)
            {
                return true;
            }
            var now = clock();
            if (!force && now < nextRead)
            {
                return false;
            }
            if (ledger.Load() is not { } loaded)
            {
                nextRead = now + ServiceRetry;
                return false;
            }
            unreadable = false;
            // What the service did not take when the app last ran is due now;
            // Polar's waits carry on.
            work = loaded with
            {
                Ops = loaded.Ops.Select(o => UsesPolar(o.Kind) ? o : o with { NextAttempt = DateTimeOffset.MinValue }).ToList(),
            };
            return true;
        }
    }

    /// <summary>The work the service's license calls for that is not owed yet.</summary>
    private void Plan(bool shown)
    {
        if (agent.Current is not { } license)
        {
            return;
        }
        var now = clock();
        Mutate(w =>
        {
            if (w.Stalled is { } stalled && (stalled.Key != license.Key || stalled.State != license.State))
            {
                w = w with { Stalled = null };
            }
            if (w.Ops.Any(o => o.Kind == LicenseOpKind.Remove) || license.Key.Length == 0
                || (license.ActivationId.Length > 0 && w.Ops.Any(o => o.Kind == LicenseOpKind.Free && o.ActivationId == license.ActivationId)))
            {
                return w;
            }
            switch (license.State)
            {
                case LicenseState.PendingActivation when w.Stalled is null
                    && !w.Ops.Any(o => o.Key == license.Key && o.Kind is LicenseOpKind.Activate or LicenseOpKind.Report or LicenseOpKind.Refuse):
                    return w with { Ops = [.. w.Ops, new LicenseOp { Kind = LicenseOpKind.Activate, Key = license.Key }] };
                case LicenseState.Licensed when shown && license.ActivationId.Length > 0 && ConfirmDue(license, now)
                    && !w.Ops.Any(o => o.ActivationId == license.ActivationId && o.Kind is LicenseOpKind.Confirm or LicenseOpKind.Confirmed):
                    return w with { Ops = [.. w.Ops, new LicenseOp { Kind = LicenseOpKind.Confirm, Key = license.Key, ActivationId = license.ActivationId }] };
                default:
                    return w;
            }
        }, onlyIfChanged: true);
    }

    private bool ConfirmDue(License license, DateTimeOffset now)
    {
        if (lastConfirm is { } last && last.Key == license.Key && last.ActivationId == license.ActivationId
            && now - last.At < ConfirmRetry)
        {
            return false;
        }
        var confirmed = DateTimeOffset.FromUnixTimeSeconds((long)Math.Min(license.ValidatedAt, (ulong)DateTimeOffset.MaxValue.ToUnixTimeSeconds()));
        return now - confirmed >= ConfirmEvery;
    }

    /// <summary>The service's license settled for the key since it was entered, when it has.</summary>
    private ActivationOutcome? Settled(string key) => agent.Current switch
    {
        { State: LicenseState.Licensed } l when l.Key == key && !Withdrawing(l) => ActivationOutcome.Licensed,
        { State: LicenseState.SeatsFull } l when l.Key == key => ActivationOutcome.SeatsFull,
        { State: LicenseState.Invalid } l when l.Key == key => ActivationOutcome.Invalid,
        _ => null,
    };

    private SemaphoreSlim LaneOf(LicenseOpKind kind) => UsesPolar(kind) ? polarLane : agentLane;

    /// <summary>Does the lane's work that is due, the oldest first, one piece at a time; nothing when the lane is at work already.</summary>
    private async Task DrainAsync(SemaphoreSlim lane, bool shown, bool linked)
    {
        if (!lane.Wait(0))
        {
            return;
        }
        try
        {
            for (int step = 0; step < MaxSteps; step++)
            {
                var now = clock();
                var op = work.Ops.FirstOrDefault(o => LaneOf(o.Kind) == lane && Due(o, now, shown, linked));
                if (op is null)
                {
                    return;
                }
                await AttemptAsync(op).ConfigureAwait(false);
            }
        }
        finally
        {
            lane.Release();
        }
    }

    /// <summary>One attempt at <paramref name="op"/> in its lane, waiting for the lane; nothing when it is done or no longer due by then.</summary>
    private async Task<ActivationOutcome?> RunInLaneAsync(LicenseOp op)
    {
        var lane = LaneOf(op.Kind);
        await lane.WaitAsync().ConfigureAwait(false);
        try
        {
            var current = work.Ops.FirstOrDefault(o => o.Id == op.Id);
            if (current is null || !Due(current, clock(), shown: false, linked: true))
            {
                return null;
            }
            return await AttemptAsync(current).ConfigureAwait(false);
        }
        finally
        {
            lane.Release();
        }
    }

    /// <summary>
    /// The piece of work can run now: its wait is over and what it needs is
    /// there (Polar not asking to slow down, the service connected, the page
    /// open for a confirmation, the work kept).
    /// </summary>
    private bool Due(LicenseOp op, DateTimeOffset now, bool shown, bool linked)
    {
        if (unreadable || op.NextAttempt > now)
        {
            return false;
        }
        if (UsesPolar(op.Kind) && now < work.PolarNotBefore)
        {
            return false;
        }
        if (unsaved && op.Kind != LicenseOpKind.Free)
        {
            return false;
        }
        return op.Kind switch
        {
            // This computer's seat waits until the service has removed the license.
            LicenseOpKind.Free => !(op.Removed && work.Ops.Any(o => o.Kind == LicenseOpKind.Remove)),
            LicenseOpKind.Confirm => linked && shown,
            _ => linked,
        };
    }

    private static bool UsesPolar(LicenseOpKind kind) => kind is LicenseOpKind.Activate or LicenseOpKind.Confirm or LicenseOpKind.Free;

    /// <summary>One attempt at one piece of work, in the lane already held; what it came to for the key, when it says.</summary>
    private async Task<ActivationOutcome?> AttemptAsync(LicenseOp op)
    {
        try
        {
            return op.Kind switch
            {
                LicenseOpKind.Activate => await ActivateAsync(op).ConfigureAwait(false),
                LicenseOpKind.Report => await ReportAsync(op).ConfigureAwait(false),
                LicenseOpKind.Refuse => await RefuseAsync(op).ConfigureAwait(false),
                LicenseOpKind.Confirm => await ConfirmAsync(op).ConfigureAwait(false),
                LicenseOpKind.Confirmed => await ConfirmedAsync(op).ConfigureAwait(false),
                LicenseOpKind.Free => await FreeAsync(op).ConfigureAwait(false),
                LicenseOpKind.Remove => await RemoveFromServiceAsync(op).ConfigureAwait(false),
                _ => Unknown(op),
            };
        }
        catch (Exception e)
        {
            // Whatever went wrong, the work waits rather than running again at once.
            log($"license: {op.Kind} failed: {e.Message}");
            Wait(op, ServiceRetry, LicenseOpFailure.Service);
            return null;
        }
    }

    /// <summary>Work of a kind a newer app wrote: not this app's to do.</summary>
    private ActivationOutcome? Unknown(LicenseOp op)
    {
        log($"license: dropping work of an unknown kind ({(int)op.Kind})");
        Drop(op);
        return null;
    }

    private async Task<ActivationOutcome?> ActivateAsync(LicenseOp op)
    {
        if (agent.Current is not { State: LicenseState.PendingActivation } license || license.Key != op.Key
            || work.Ops.Any(o => o.Key == op.Key && o.Kind == LicenseOpKind.Report))
        {
            // The key no longer waits, or an activation for it is held already.
            Drop(op);
            return null;
        }
        var answer = await polar.ActivateAsync(op.Key, label()).ConfigureAwait(false);
        log($"license: activate: {PolarLog.Describe(answer)}");
        switch (answer)
        {
            case PolarAnswer.Activated activated:
                // Kept before it is reported, so a restart reports it rather
                // than taking a second seat for the same key. When Remove
                // License came meanwhile, the seat is freed instead.
                Mutate(w => w.Ops.Any(o => o.Id == op.Id)
                    ? w with
                    {
                        Ops = w.Ops.Select(o => o.Id == op.Id
                            ? new LicenseOp
                            {
                                Kind = LicenseOpKind.Report,
                                Key = op.Key,
                                ActivationId = activated.ActivationId,
                                CustomerEmail = activated.CustomerEmail,
                                ValidatedAt = activated.ValidatedAt,
                            }
                            : o).ToList(),
                    }
                    : w with { Ops = [new LicenseOp { Kind = LicenseOpKind.Free, Key = op.Key, ActivationId = activated.ActivationId }, .. w.Ops] });
                return null;
            case PolarAnswer.Unreadable:
                // It may have taken a seat: asking again could take another.
                // Left alone until the user acts; the seat, if any, is in the portal.
                Drop(op);
                Stall(op.Key, unsaved: true);
                return ActivationOutcome.NotRecorded;
            case PolarAnswer.NotPermitted:
                return await TellFullFromInvalidAsync(op).ConfigureAwait(false);
            default:
                return KeyAnswer(op, answer, seatsFull: false);
        }
    }

    /// <summary>A 403 from activate: a validate without an activation id tells full seats (200) from a dead key (404).</summary>
    private async Task<ActivationOutcome?> TellFullFromInvalidAsync(LicenseOp op)
    {
        var answer = await polar.ValidateAsync(op.Key, null).ConfigureAwait(false);
        log($"license: validate the key alone: {PolarLog.Describe(answer)}");
        return KeyAnswer(op, answer, seatsFull: answer is PolarAnswer.KeyLive);
    }

    /// <summary>What an answer about the key alone means for the activation waiting on it.</summary>
    private ActivationOutcome? KeyAnswer(LicenseOp op, PolarAnswer answer, bool seatsFull)
    {
        switch (answer)
        {
            case PolarAnswer.KeyLive or PolarAnswer.NotFound or PolarAnswer.Malformed { AboutKey: true }:
                Replace(op, new LicenseOp { Kind = LicenseOpKind.Refuse, Key = op.Key, SeatsFull = seatsFull });
                return null;
            case PolarAnswer.Malformed:
                // Polar could not read something other than the key: not the key's fault.
                Wait(op, ActivateWait(op), LicenseOpFailure.Rejected);
                return ActivationOutcome.NotRecorded;
            case PolarAnswer.RateLimited limited:
                Wait(op, Max(SlowDown(limited), ActivateWait(op)), LicenseOpFailure.RateLimited);
                return ActivationOutcome.Busy;
            default:
                Wait(op, ActivateWait(op), LicenseOpFailure.Unreachable);
                return ActivationOutcome.Offline;
        }
    }

    private async Task<ActivationOutcome?> ReportAsync(LicenseOp op)
    {
        if (HoldsActivation(op))
        {
            Drop(op);
            return ActivationOutcome.Licensed;
        }
        var result = new RecordLicenseResult
        {
            Key = op.Key,
            Activated = new LicenseActivated
            {
                ActivationId = op.ActivationId,
                CustomerEmail = op.CustomerEmail,
                ValidatedAt = op.ValidatedAt,
            },
        };
        try
        {
            await agent.RecordAsync(result).ConfigureAwait(false);
            Drop(op);
            return ActivationOutcome.Licensed;
        }
        catch (AgentRefusal refusal) when (refusal.Code == "already_licensed" && HoldsActivation(op))
        {
            Drop(op);
            return ActivationOutcome.Licensed;
        }
        catch (AgentRefusal refusal) when (Final(refusal))
        {
            // An activation the service will never keep must not hold a seat.
            log($"license: the background service refused the activation ({refusal.Code}); freeing it");
            Replace(op, new LicenseOp { Kind = LicenseOpKind.Free, Key = op.Key, ActivationId = op.ActivationId });
            Stall(op.Key);
            return agent.Current is { State: LicenseState.Licensed } ? ActivationOutcome.Licensed : ActivationOutcome.ServiceRefused;
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException)
        {
            log($"license: the background service did not take the activation yet ({e.Message}); keeping it");
            return NotTaken(op, e);
        }
    }

    private async Task<ActivationOutcome?> RefuseAsync(LicenseOp op)
    {
        var result = new RecordLicenseResult
        {
            Key = op.Key,
            ActivationId = op.ActivationId,
            Refused = op.SeatsFull ? LicenseRefusal.SeatsFull : LicenseRefusal.Invalid,
        };
        try
        {
            await agent.RecordAsync(result).ConfigureAwait(false);
            Drop(op);
            return op.SeatsFull ? ActivationOutcome.SeatsFull : ActivationOutcome.Invalid;
        }
        catch (AgentRefusal refusal) when (Final(refusal))
        {
            // Polar's answer was not kept, so the dialog does not tell it as if it were.
            log($"license: the background service took no refusal: {refusal.Code}");
            Drop(op);
            if (op.ActivationId.Length == 0)
            {
                Stall(op.Key);
            }
            return agent.Current is { State: LicenseState.Licensed } ? ActivationOutcome.Licensed : ActivationOutcome.NotRecorded;
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException)
        {
            log($"license: the background service did not take a refusal yet ({e.Message}); keeping it");
            return NotTaken(op, e);
        }
    }

    private async Task<ActivationOutcome?> ConfirmAsync(LicenseOp op)
    {
        if (!HoldsActivation(op))
        {
            Drop(op);
            return null;
        }
        var now = clock();
        lastConfirm = (op.Key, op.ActivationId, now);
        var answer = await polar.ValidateAsync(op.Key, op.ActivationId).ConfigureAwait(false);
        log($"license: validate: {PolarLog.Describe(answer)}");
        switch (answer)
        {
            case PolarAnswer.Validated validated:
                Replace(op, new LicenseOp
                {
                    Kind = LicenseOpKind.Confirmed,
                    Key = op.Key,
                    ActivationId = op.ActivationId,
                    CustomerEmail = validated.CustomerEmail,
                    ValidatedAt = validated.ValidatedAt,
                });
                break;
            case PolarAnswer.NotFound or PolarAnswer.Malformed { AboutKey: true } or PolarAnswer.Malformed { AboutActivation: true }:
                if (op.NotFoundAt is { } first && now - first >= SecondNotFound)
                {
                    Replace(op, new LicenseOp { Kind = LicenseOpKind.Refuse, Key = op.Key, ActivationId = op.ActivationId });
                }
                else
                {
                    Update(op, op with { NotFoundAt = op.NotFoundAt ?? now, NextAttempt = now + SecondNotFound });
                }
                break;
            case PolarAnswer.RateLimited limited:
                Wait(op, Max(SlowDown(limited), ConfirmRetry), LicenseOpFailure.RateLimited);
                break;
            default:
                // Out of reach: a licensed computer stays licensed, however long.
                Wait(op, ConfirmRetry, LicenseOpFailure.Unreachable);
                break;
        }
        return null;
    }

    private async Task<ActivationOutcome?> ConfirmedAsync(LicenseOp op)
    {
        var result = new RecordLicenseResult
        {
            Key = op.Key,
            ActivationId = op.ActivationId,
            Validated = new LicenseValidated { ValidatedAt = op.ValidatedAt, CustomerEmail = op.CustomerEmail },
        };
        try
        {
            await agent.RecordAsync(result).ConfigureAwait(false);
            Drop(op);
        }
        catch (AgentRefusal refusal) when (Final(refusal))
        {
            // The license changed meanwhile; nothing to confirm.
            log($"license: the background service took no confirmation: {refusal.Code}");
            Drop(op);
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException)
        {
            NotTaken(op, e);
        }
        return null;
    }

    private async Task<ActivationOutcome?> FreeAsync(LicenseOp op)
    {
        var answer = await polar.DeactivateAsync(op.Key, op.ActivationId).ConfigureAwait(false);
        log($"license: free an activation: {PolarLog.Describe(answer)}");
        switch (answer)
        {
            case PolarAnswer.Done or PolarAnswer.NotPermitted or PolarAnswer.Malformed:
                // Freed, or Polar says for good that there is nothing to free.
                Drop(op);
                break;
            case PolarAnswer.RateLimited limited:
                Wait(op, Max(SlowDown(limited), FreeWait(op)), LicenseOpFailure.RateLimited);
                break;
            default:
                Wait(op, FreeWait(op), LicenseOpFailure.Unreachable);
                break;
        }
        return null;
    }

    private async Task<ActivationOutcome?> RemoveFromServiceAsync(LicenseOp op)
    {
        try
        {
            await agent.RemoveAsync().ConfigureAwait(false);
            Drop(op);
        }
        catch (AgentRefusal refusal) when (Final(refusal))
        {
            // Refused for good: the license stays, and so does its seat;
            // asking again would hold everything else back forever.
            log($"license: the background service refused to remove the license ({refusal.Code})");
            Mutate(w => w with { Ops = w.Ops.Where(o => o.Id != op.Id && !(o.Kind == LicenseOpKind.Free && o.Removed)).ToList() });
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException)
        {
            log($"license: the background service did not remove the license ({e.Message}); asking again later");
            Wait(op, ServiceRetry, LicenseOpFailure.Service);
        }
        return null;
    }

    /// <summary>The service holds this activation of the key now.</summary>
    private bool HoldsActivation(LicenseOp op) =>
        agent.Current is { State: LicenseState.Licensed } current && current.Key == op.Key && current.ActivationId == op.ActivationId;

    /// <summary>
    /// The service answered with a refusal that stands (docs/architecture.md
    /// §11): any code, known or not, but an unreadable license file and the
    /// service stopping, which pass. "unexpected" is this app's own code for
    /// an empty answer, so it is no answer at all.
    /// </summary>
    private static bool Final(AgentRefusal refusal) =>
        refusal.Code is not ("license_unavailable" or "agent_stopping" or "unexpected");

    /// <summary>The service did not take an answer this time: it is given again a minute later.</summary>
    private ActivationOutcome NotTaken(LicenseOp op, Exception e)
    {
        var failure = e switch
        {
            AgentRefusal { Code: "license_unavailable" } => LicenseOpFailure.LicenseUnavailable,
            _ => LicenseOpFailure.Service,
        };
        Wait(op, ServiceRetry, failure);
        return failure == LicenseOpFailure.LicenseUnavailable ? ActivationOutcome.Unavailable
            : e is AgentUnavailableException ? ActivationOutcome.ServiceNotRunning
            : ActivationOutcome.NotRecorded;
    }

    /// <summary>Each attempt to activate a key waits twice as long as the last: 5 minutes, 10, 20…, at most <see cref="MaxActivateWait"/>.</summary>
    private static TimeSpan ActivateWait(LicenseOp op) =>
        TimeSpan.FromMinutes(Math.Min(MaxActivateWait.TotalMinutes, ActivateRetry.TotalMinutes * Math.Pow(2, Math.Min(op.Attempts, 16))));

    /// <summary>Each attempt to free one activation waits twice as long as the last: 2 minutes, 4, 8…, at most <see cref="MaxFreeWait"/>.</summary>
    private static TimeSpan FreeWait(LicenseOp op) =>
        TimeSpan.FromMinutes(Math.Min(MaxFreeWait.TotalMinutes, Math.Pow(2, Math.Min(op.Attempts + 1, 16))));

    private static TimeSpan Max(TimeSpan a, TimeSpan b) => a > b ? a : b;

    private TimeSpan SlowDown(PolarAnswer.RateLimited limited)
    {
        var until = clock() + limited.RetryAfter;
        Mutate(w => w with { PolarNotBefore = until > w.PolarNotBefore ? until : w.PolarNotBefore });
        return limited.RetryAfter;
    }

    /// <summary>Leaves the key alone while the service's license stays as it is now.</summary>
    private void Stall(string key, bool unsaved = false)
    {
        // A license that moved on to another key is not held back by this one.
        if (agent.Current is { } current && current.Key == key)
        {
            Mutate(w => w with { Stalled = new StalledKey(key, current.State) { Unsaved = unsaved } });
        }
    }

    private void Drop(LicenseOp op) => Mutate(w => w with { Ops = w.Ops.Where(o => o.Id != op.Id).ToList() });

    /// <summary>The next step of a piece of work, in its place in the order; nothing when the work is gone meanwhile.</summary>
    private void Replace(LicenseOp op, LicenseOp next) =>
        Mutate(w => w with { Ops = w.Ops.Select(o => o.Id == op.Id ? next : o).ToList() });

    private void Update(LicenseOp op, LicenseOp next) => Replace(op, next with { Id = op.Id });

    private void Wait(LicenseOp op, TimeSpan wait, LicenseOpFailure failure) =>
        Update(op, op with { Attempts = op.Attempts + 1, NextAttempt = clock() + wait, LastFailure = failure });

    /// <summary>Changes the work and keeps it; never while the kept work cannot be read, which would replace it.</summary>
    private void Mutate(Func<LicenseWork, LicenseWork> change, bool onlyIfChanged = false)
    {
        lock (sync)
        {
            var next = change(work);
            if (onlyIfChanged && ReferenceEquals(next, work))
            {
                return;
            }
            work = next;
            unsaved = unreadable || !ledger.Save(next);
        }
    }

    /// <summary>Keeps the work again when the last change could not be kept.</summary>
    private void Resave()
    {
        lock (sync)
        {
            if (unsaved && !unreadable)
            {
                unsaved = !ledger.Save(work);
            }
        }
    }
}
