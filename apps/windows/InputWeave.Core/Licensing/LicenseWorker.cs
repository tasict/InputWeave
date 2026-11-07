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

    /// <summary>
    /// Remove License, of the license that has <paramref name="key"/> and
    /// <paramref name="activationId"/> (empty for none): refused with
    /// "license_stale" when the service has a newer license by then.
    /// </summary>
    Task RemoveAsync(string key, string activationId);

    /// <summary>Apply License: sends this computer's key to another computer.</summary>
    Task ApplyAsync(string deviceId);

    /// <summary>
    /// Revoke License, or Don't Apply: <paramref name="activationId"/> is the
    /// computer's activation, empty to take back what was applied;
    /// <paramref name="deactivated"/> says Polar deactivated it. Returns the
    /// status the service answered with, when it did.
    /// </summary>
    Task<Status?> RevokeAsync(string deviceId, string activationId, bool deactivated);

    /// <summary>An activation the service listed to be freed (License.to_free) is freed with Polar.</summary>
    Task ActivationFreedAsync(string activationId);
}

/// <summary>The service's commands, through the app's connection to it.</summary>
public sealed class AgentLicense(AgentModel model) : ILicenseAgent
{
    public bool Linked => model.Snapshot.Link == AgentLink.Connected;

    public License? Current => model.Snapshot.Status.License;

    public Task SetKeyAsync(string key) => model.SetLicenseKeyAsync(key);

    public Task RecordAsync(RecordLicenseResult result) => model.RecordLicenseResultAsync(result);

    public Task RemoveAsync(string key, string activationId) => model.RemoveLicenseAsync(key, activationId);

    public Task ApplyAsync(string deviceId) => model.ApplyLicenseAsync(deviceId);

    public Task<Status?> RevokeAsync(string deviceId, string activationId, bool deactivated) =>
        model.RevokeLicenseAsync(deviceId, activationId, deactivated);

    public Task ActivationFreedAsync(string activationId) => model.LicenseActivationFreedAsync(activationId);
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

/// <summary>What is said after another computer was removed with "Also revoke its license".</summary>
public enum RemovalNotice
{
    /// <summary>Nothing: the license was revoked there, or there was none to revoke.</summary>
    None,
    /// <summary>Its activation could not be deactivated: free it in Polar's customer portal.</summary>
    NotRevoked,
    /// <summary>It was offline holding the key: if it activates it later, free it in the customer portal.</summary>
    Offline,
    /// <summary>
    /// Its activation was deactivated with Polar, so its place is free, but
    /// the service did not take the revoke: it was not told, and may still
    /// show as licensed until it next checks its license.
    /// </summary>
    NotTold,
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
/// <item>Remove License: the service is asked to remove the license (the one
/// the user saw; a newer one it keeps, and that is the end of it), and
/// asked again until it has; then this computer's seat is freed. Both are
/// kept before either is tried. Refused for good, the license stays, and so
/// does the seat. A license another computer revoked here is given up the
/// same way, by itself and without a word on the License page, but its
/// activation is freed first: the service keeps the license, and opens the
/// app for it, until then. The activation is freed whatever the license is
/// by then (a revoked activation is never licensed again), and only a
/// license still revoked with it is removed.</item>
/// <item>Activations of keys this computer had before, which the service
/// lists to be freed: each is freed with Polar, like a seat, and the
/// service told once it is.</item>
/// <item>Revoke License on another computer: Polar is asked once to
/// deactivate its activation, and the service is told whether it did,
/// asked again until it has taken it (a refusal that stands drops it). A
/// computer whose activation was not deactivated deactivates it itself.
/// Revoking it there goes before removing the license here, which the
/// service would otherwise refuse.</item>
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

    /// <summary>How many times in all the user's Remove License is sent when each finds the license replaced (license_stale).</summary>
    public const int MaxRemoveTries = 3;

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
    /// <summary>Activations the service was told are freed in this run, so one it still lists meanwhile is not freed again.</summary>
    private readonly HashSet<string> acknowledged = [];
    /// <summary>Activations freed with Polar in this run, so one the service lists to be freed afterwards is only told.</summary>
    private readonly HashSet<string> freedHere = [];
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
    public bool RemoveFailed => work.Ops.Any(o => o.Kind == LicenseOpKind.Remove && !o.Silent
        && (o.Attempts > 0 || (removeStarted is { } started && clock() - started >= ServiceWait)));

    /// <summary>Freeing the seat Remove License gave up failed; it waits for Polar (or the customer portal).</summary>
    public bool SeatNotFreed => work.Ops.Any(o => o.Kind == LicenseOpKind.Free && o.Removed && !o.Silent && o.Attempts > 0);

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
                    .Select(o => (o.Key == key && o.Kind != LicenseOpKind.Activate && !o.Silent && !o.ToFree) || (o.Kind == LicenseOpKind.Remove && !o.Silent)
                        ? o with { NextAttempt = DateTimeOffset.MinValue }
                        : o)
                    .ToList(),
            });
            // A removal still owed goes first, or it would take away the key
            // entered now. Not one of a license revoked here: entering a key,
            // the same or another, starts the license over (waiting for
            // activation, without an activation), and the service lists the
            // revoked activation to be freed, which the app frees.
            if (work.Ops.FirstOrDefault(o => o.Kind == LicenseOpKind.Remove && !o.Silent) is { } remove)
            {
                await AttemptAsync(remove).ConfigureAwait(false);
                if (work.Ops.Any(o => o.Kind == LicenseOpKind.Remove && !o.Silent))
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
            // A license revoked here is started over by the key, whichever it
            // is: nothing left to remove; its activation is freed all the same.
            Mutate(w => WithoutStaleRemoval(w, agent.Current), onlyIfChanged: true);
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
            var op = work.Ops.FirstOrDefault(o => o.Key == key && !o.Silent && !o.ToFree
                && o.Kind is not (LicenseOpKind.Confirm or LicenseOpKind.Confirmed or LicenseOpKind.Revoke or LicenseOpKind.Remove or LicenseOpKind.Freed)
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
        // The license as the user saw it when they confirmed: the service
        // removes only that one.
        var license = agent.Current;
        var remove = new LicenseOp { Kind = LicenseOpKind.Remove, Key = license?.Key ?? "", ActivationId = license?.ActivationId ?? "" };
        await agentLane.WaitAsync().ConfigureAwait(false);
        try
        {
            removeStarted = clock();
            // Revoking it on other computers, still owed, goes first and at once: the user is acting.
            Mutate(w =>
            {
                var next = Withdraw(w, license, remove);
                return next with
                {
                    Ops = next.Ops.Select(o => o.Kind == LicenseOpKind.Revoke ? o with { NextAttempt = DateTimeOffset.MinValue } : o).ToList(),
                };
            });
        }
        finally
        {
            agentLane.Release();
        }
        // The first attempt: what is owed to other computers, the service, then the seats to free that are due.
        foreach (var revoke in work.Ops.Where(o => o.Kind == LicenseOpKind.Revoke).ToList())
        {
            await RunInLaneAsync(revoke).ConfigureAwait(false);
        }
        // Again, at once, while each finds the license replaced and this app knows the newer one.
        for (int i = 0; i < MaxRemoveTries && work.Ops.Any(o => o.Id == remove.Id); i++)
        {
            await RunInLaneAsync(remove).ConfigureAwait(false);
        }
        foreach (var free in work.Ops.Where(o => o.Kind == LicenseOpKind.Free && Due(o, clock(), shown: false, linked: false)).ToList())
        {
            await RunInLaneAsync(free).ConfigureAwait(false);
        }
        return Removing ? RemoveOutcome.ServicePending
            : SeatNotFreed ? RemoveOutcome.RemovedSeatNotFreed
            : RemoveOutcome.Removed;
    }

    /// <summary>
    /// The work that gives up this computer's license with
    /// <paramref name="remove"/>: what was owed for the license is dropped
    /// (an activation Polar granted becomes a seat to free), seats to free
    /// (with telling the service of those freed) and revoking it on other
    /// computers stay ahead of the removal, and
    /// this computer's seat is freed once the service has removed the
    /// license (Due holds it back until then), or, for a license another
    /// computer revoked here, before.
    /// </summary>
    private static LicenseWork Withdraw(LicenseWork w, License? license, LicenseOp remove)
    {
        var ops = new List<LicenseOp>();
        foreach (var op in w.Ops)
        {
            switch (op.Kind)
            {
                case LicenseOpKind.Free or LicenseOpKind.Revoke or LicenseOpKind.Freed:
                    ops.Add(op);
                    break;
                case LicenseOpKind.Report:
                    // Granted, never taken: a seat to free like any other.
                    ops.Add(new LicenseOp { Kind = LicenseOpKind.Free, Key = op.Key, ActivationId = op.ActivationId });
                    break;
            }
        }
        ops.Add(remove);
        if (license is { Key.Length: > 0, ActivationId.Length: > 0 }
            && !ops.Any(o => o.Kind == LicenseOpKind.Free && o.ActivationId == license.ActivationId))
        {
            ops.Add(new LicenseOp
            {
                Kind = LicenseOpKind.Free,
                Key = license.Key,
                ActivationId = license.ActivationId,
                Removed = true,
                Silent = remove.Silent,
            });
        }
        return w with { Ops = ops, Stalled = null };
    }

    /// <summary>
    /// The license is still revoked here with the activation the silent
    /// work gives up (<see cref="LicenseOp.Silent"/>).
    /// </summary>
    private static bool StillRevoked(License? license, LicenseOp op) =>
        license is { State: LicenseState.Revoked } && license.Key == op.Key && license.ActivationId == op.ActivationId;

    /// <summary>The service lists the activation to be freed (License.to_free).</summary>
    private static bool ListedToFree(License? license, string activationId) =>
        license is not null && license.ToFree.Any(a => a.ActivationId == activationId);

    /// <summary>
    /// Without the removal of a license revoked here once the license is no
    /// longer revoked with that activation (the same key, or another, applied
    /// or entered here meanwhile): there is nothing left to remove. Its
    /// activation is freed all the same, and told to the service if it lists
    /// it. The same work when there is none to drop.
    /// </summary>
    private LicenseWork WithoutStaleRemoval(LicenseWork w, License? license)
    {
        if (!w.Ops.Any(o => o.Silent && o.Kind == LicenseOpKind.Remove && !StillRevoked(license, o)))
        {
            return w;
        }
        log("license: no longer revoked here; not removing the license");
        return w with { Ops = w.Ops.Where(o => !(o.Silent && o.Kind == LicenseOpKind.Remove && !StillRevoked(license, o))).ToList() };
    }

    /// <summary>
    /// The activations the service lists to be freed that are not owed yet
    /// (nor freed already, in this run): a free each, whatever the license is.
    /// </summary>
    private LicenseWork WithToFree(LicenseWork w, License license)
    {
        // One freed here already (a revoked one, before the service listed
        // it) is only told: Polar is not asked twice.
        var owed = license.ToFree
            .Where(a => a.ActivationId.Length > 0 && a.Key.Length > 0 && !acknowledged.Contains(a.ActivationId)
                && !w.Ops.Any(o => (o.Kind is LicenseOpKind.Free or LicenseOpKind.Freed) && o.ActivationId == a.ActivationId))
            .GroupBy(a => a.ActivationId)
            .Select(g => freedHere.Contains(g.Key)
                ? new LicenseOp { Kind = LicenseOpKind.Freed, Key = g.First().Key, ActivationId = g.Key }
                : new LicenseOp { Kind = LicenseOpKind.Free, Key = g.First().Key, ActivationId = g.Key, ToFree = true })
            .ToList();
        if (owed.Count == 0)
        {
            return w;
        }
        log($"license: {owed.Count} activations of an earlier key to free");
        return w with { Ops = [.. w.Ops, .. owed] };
    }

    /// <summary>
    /// Apply License on another computer: the service sends it this
    /// computer's key, and its app activates it. False when the service did
    /// not take it (logged); the computer's row shows what the service says.
    /// </summary>
    public async Task<bool> ApplyAsync(string deviceId)
    {
        try
        {
            await agent.ApplyAsync(deviceId).ConfigureAwait(false);
            return true;
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException)
        {
            log($"license: apply to {deviceId}: {(e is AgentRefusal refusal ? refusal.Code : e.Message)}");
            return false;
        }
    }

    /// <summary>
    /// Don't Apply: the service takes back applying the license to a
    /// computer it has not reached yet. Nothing for Polar to free.
    /// </summary>
    public async Task<bool> DontApplyAsync(string deviceId)
    {
        try
        {
            await agent.RevokeAsync(deviceId, "", deactivated: false).ConfigureAwait(false);
            return true;
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException)
        {
            log($"license: don't apply to {deviceId}: {(e is AgentRefusal refusal ? refusal.Code : e.Message)}");
            return false;
        }
    }

    /// <summary>
    /// Revoke License on another computer: Polar is asked once to deactivate
    /// its activation (this computer's key), unless Polar asked to wait; then
    /// the service is told which activation and whether Polar deactivated
    /// it, kept and asked again until the service has taken it, so a restart
    /// in between still tells it, as decided. Polar is not asked again: a
    /// computer whose activation was not deactivated deactivates it itself.
    /// False when the app's own license work cannot be read now, so nothing
    /// was done.
    /// </summary>
    public async Task<bool> RevokeAsync(string deviceId, string activationId)
    {
        if (!Read(force: true))
        {
            log($"license: revoke on {deviceId}: the license work cannot be read now");
            return false;
        }
        Resave();
        var key = agent.Current?.Key ?? "";
        bool deactivated = await DeactivateOnceAsync(key, activationId).ConfigureAwait(false);
        var revoke = new LicenseOp
        {
            Kind = LicenseOpKind.Revoke,
            Key = key,
            DeviceId = deviceId,
            ActivationId = activationId,
            Deactivated = deactivated,
        };
        // A revoke on the same computer takes the place of one still owed.
        Mutate(w => w with { Ops = [.. w.Ops.Where(o => !(o.Kind == LicenseOpKind.Revoke && o.DeviceId == deviceId)), revoke] });
        await RunInLaneAsync(revoke).ConfigureAwait(false);
        return true;
    }

    /// <summary>
    /// Asks Polar once to deactivate another computer's activation, unless
    /// Polar asked to wait (its Retry-After still runs); true when Polar did,
    /// or said it was gone already.
    /// </summary>
    private async Task<bool> DeactivateOnceAsync(string key, string activationId)
    {
        if (key.Length == 0 || activationId.Length == 0)
        {
            return false;
        }
        if (clock() < work.PolarNotBefore)
        {
            log("license: Polar asked to wait; the other computer deactivates its activation itself");
            return false;
        }
        var answer = await polar.DeactivateAsync(key, activationId).ConfigureAwait(false);
        log($"license: deactivate another computer's activation: {PolarLog.Describe(answer)}");
        if (answer is PolarAnswer.RateLimited limited)
        {
            SlowDown(limited);
        }
        return answer is PolarAnswer.Done;
    }

    /// <summary>
    /// Removing another computer with "Also revoke its license" checked,
    /// before it is unpaired: as Revoke License, a licensed one's activation
    /// is deactivated with Polar once and the service told whether it was,
    /// but at once and not kept (once it is removed, the two computers no
    /// longer reach each other); applying the license to one activating or
    /// waiting for it is taken back. The service's answer is awaited, then
    /// <paramref name="unpair"/> runs, whatever came of that. Once the
    /// computer is removed, what to say (<see cref="NoticeAfterRemoval"/>):
    /// nothing when it could not be.
    /// </summary>
    public async Task<RemovalNotice> RemoveDeviceAsync(string deviceId, bool revoke, Func<Task> unpair)
    {
        var notice = revoke ? await RevokeForRemovalAsync(deviceId).ConfigureAwait(false) : RemovalNotice.None;
        try
        {
            await unpair().ConfigureAwait(false);
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException)
        {
            log($"license: remove {deviceId}: {(e is AgentRefusal refusal ? refusal.Code : e.Message)}");
            return RemovalNotice.None;
        }
        return notice;
    }

    /// <summary>
    /// What is said once a computer was removed with "Also revoke its
    /// license". The service did not take the revoke (<paramref name="taken"/>
    /// false: refused, or no answer): it could not be revoked, said as for an
    /// offline one that may hold the key when that is what it was; a licensed
    /// one Polar deactivated has its place free all the same, and was only
    /// not told. Taken: a
    /// licensed one that never said its activation
    /// (<paramref name="activationUnknown"/>) is told to drop the key, which
    /// reaches it if it is connected (<paramref name="connected"/>, as the
    /// service answered: connected, or offline for now, which keeps the
    /// connection), and otherwise is left to the customer portal; so is a
    /// licensed one whose activation was not deactivated, and an offline one
    /// that may hold the key (LicensedDevice.key_out_of_reach), which taking
    /// the apply back no longer reaches. Nothing for the rest.
    /// </summary>
    public static RemovalNotice NoticeAfterRemoval(
        LicenseUse use, bool keyOutOfReach, bool deactivated, bool taken, bool activationUnknown, bool connected) => use switch
    {
        LicenseUse.Waiting when keyOutOfReach => RemovalNotice.Offline,
        LicenseUse.Licensed when !taken && deactivated => RemovalNotice.NotTold,
        _ when !taken => RemovalNotice.NotRevoked,
        LicenseUse.Licensed when activationUnknown => connected ? RemovalNotice.None : RemovalNotice.NotRevoked,
        LicenseUse.Licensed when !deactivated => RemovalNotice.NotRevoked,
        _ => RemovalNotice.None,
    };

    /// <summary>Revokes the license on a computer about to be removed, and says what is to be said once it is.</summary>
    private async Task<RemovalNotice> RevokeForRemovalAsync(string deviceId)
    {
        if (agent.Current is not { } license || license.Devices.FirstOrDefault(d => d.Id == deviceId) is not { Legacy: false } device)
        {
            return RemovalNotice.None;
        }
        string activation;
        bool deactivated;
        switch (device.Use)
        {
            case LicenseUse.Licensed:
                activation = device.ActivationId;
                deactivated = await DeactivateOnceAsync(license.Key, activation).ConfigureAwait(false);
                break;
            case LicenseUse.Activating or LicenseUse.Waiting:
                (activation, deactivated) = ("", false);
                break;
            default:
                return RemovalNotice.None;
        }
        bool taken;
        bool connected = false;
        try
        {
            var answer = await agent.RevokeAsync(deviceId, activation, deactivated).ConfigureAwait(false);
            taken = true;
            connected = answer?.Devices.FirstOrDefault(d => d.Id == deviceId) is { } there
                && (there.State == ConnectionState.Connected || there.OfflineForNow);
        }
        catch (Exception e)
        {
            // Whatever went wrong, the computer is removed all the same, and
            // the user is told its license could not be revoked.
            log($"license: revoke on the removed {deviceId}: {(e is AgentRefusal refusal ? refusal.Code : e.Message)}");
            taken = false;
        }
        return NoticeAfterRemoval(
            device.Use, device.KeyOutOfReach, deactivated, taken,
            activationUnknown: device.Use == LicenseUse.Licensed && activation.Length == 0, connected);
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
            w = WithToFree(WithoutStaleRemoval(w, license), license);
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
                case LicenseState.Revoked when w.Stalled is null:
                    // Another computer revoked it here: given up as Remove
                    // License does, without asking and without a word, its
                    // activation freed first.
                    log("license: revoked here by another computer; freeing its activation and removing it");
                    return Withdraw(w, license, new LicenseOp
                    {
                        Kind = LicenseOpKind.Remove,
                        Key = license.Key,
                        ActivationId = license.ActivationId,
                        Silent = true,
                    });
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
            // This computer's seat waits until the service has removed the
            // license; an activation another computer revoked here does not,
            // but the service's license is looked at before it is freed.
            LicenseOpKind.Free when op.Silent => linked,
            LicenseOpKind.Free => !(op.Removed && work.Ops.Any(o => o.Kind == LicenseOpKind.Remove)),
            LicenseOpKind.Confirm => linked && shown,
            // Without the license here, the service could revoke it nowhere.
            // A license revoked here is removed once its activation is freed:
            // until then the service keeps it, and opens the app for it.
            LicenseOpKind.Remove => linked
                && !work.Ops.TakeWhile(o => o.Id != op.Id).Any(o => o.Kind == LicenseOpKind.Revoke)
                && !(op.Silent && op.ActivationId.Length > 0 && work.Ops.Any(o => o.Kind == LicenseOpKind.Free && o.ActivationId == op.ActivationId)),
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
                LicenseOpKind.Revoke => await RevokeAtServiceAsync(op).ConfigureAwait(false),
                LicenseOpKind.Freed => await FreedAtServiceAsync(op).ConfigureAwait(false),
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
                Freed(op);
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

    /// <summary>
    /// What follows an activation freed (or that Polar says there is nothing
    /// to free of): one the service lists to be freed is told to it; that of
    /// a license revoked here is removed if the license is still revoked with
    /// it, told to the service if it lists it to be freed by then (another key
    /// was entered meanwhile), and nothing more otherwise.
    /// </summary>
    private void Freed(LicenseOp op)
    {
        lock (sync)
        {
            freedHere.Add(op.ActivationId);
        }
        var license = agent.Current;
        if (op.Silent && StillRevoked(license, op))
        {
            // The removal held back for it goes next.
            Drop(op);
            return;
        }
        bool tell = op.ToFree || (op.Silent && ListedToFree(license, op.ActivationId));
        Mutate(w =>
        {
            var ops = new List<LicenseOp>();
            foreach (var o in w.Ops)
            {
                if (o.Id == op.Id)
                {
                    if (tell)
                    {
                        ops.Add(new LicenseOp { Kind = LicenseOpKind.Freed, Key = op.Key, ActivationId = op.ActivationId });
                    }
                }
                else if (!(o.Silent && o.Kind == LicenseOpKind.Remove && o.ActivationId == op.ActivationId))
                {
                    ops.Add(o);
                }
            }
            return w with { Ops = ops };
        });
    }

    /// <summary>Done with telling the service of a freed activation: not freed again in this run, should the service still list it.</summary>
    private void Acknowledge(LicenseOp op)
    {
        lock (sync)
        {
            acknowledged.Add(op.ActivationId);
        }
        Drop(op);
    }

    /// <summary>Tells the service an activation it listed to be freed is; a refusal, or one no longer listed, ends it too.</summary>
    private async Task<ActivationOutcome?> FreedAtServiceAsync(LicenseOp op)
    {
        try
        {
            await agent.ActivationFreedAsync(op.ActivationId).ConfigureAwait(false);
            Acknowledge(op);
        }
        catch (AgentRefusal refusal) when (Final(refusal))
        {
            log($"license: the background service took no freed activation ({refusal.Code})");
            Acknowledge(op);
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException)
        {
            log($"license: the background service did not take a freed activation yet ({e.Message}); telling it again later");
            Wait(op, ServiceRetry, LicenseOpFailure.Service);
        }
        return null;
    }

    private async Task<ActivationOutcome?> RemoveFromServiceAsync(LicenseOp op)
    {
        if (op.Silent && !StillRevoked(agent.Current, op))
        {
            NoLongerRevoked(op);
            return null;
        }
        try
        {
            await agent.RemoveAsync(op.Key, op.ActivationId).ConfigureAwait(false);
            Drop(op);
        }
        catch (AgentRefusal refusal) when (refusal.Code == "license_stale" && op.Silent)
        {
            // The license revoked here moved on meanwhile: nothing to remove.
            log("license: the license to remove was replaced meanwhile; keeping the newer one");
            NoLongerRevoked(op);
        }
        catch (AgentRefusal refusal) when (refusal.Code == "license_stale")
        {
            RemoveAgain(op);
        }
        catch (AgentRefusal refusal) when (Final(refusal))
        {
            // Refused for good: the license stays, and so does its seat;
            // asking again would hold everything else back forever. A license
            // revoked here, its activation freed already, is left alone until
            // it changes, rather than given up again on every tick.
            log($"license: the background service refused to remove the license ({refusal.Code})");
            if (op.Silent)
            {
                Drop(op);
                Stall(op.Key);
            }
            else
            {
                KeepLicense(op);
            }
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException)
        {
            log($"license: the background service did not remove the license ({e.Message}); asking again later");
            Wait(op, ServiceRetry, LicenseOpFailure.Service);
        }
        return null;
    }

    /// <summary>
    /// The user's Remove License found a newer license than the one they saw
    /// (license_stale): they want the license gone, so the one there now is
    /// removed instead, its seat freed once the service has removed it, like
    /// the one before; at most <see cref="MaxRemoveTries"/> times in all, and
    /// then as any refusal. No license left: the removal is done.
    /// </summary>
    private void RemoveAgain(LicenseOp op)
    {
        var license = agent.Current;
        int tries = op.StaleAnswers + 1;
        if (license is not { Key.Length: > 0 })
        {
            log("license: no license left to remove");
            Drop(op);
            return;
        }
        if (tries >= MaxRemoveTries)
        {
            log($"license: the license to remove was replaced {tries} times; keeping it");
            KeepLicense(op);
            return;
        }
        log("license: the license to remove was replaced meanwhile; removing the one there now");
        // Asked again at once when this app already knows of the newer license; otherwise once it has heard of it.
        var next = license.Key == op.Key && license.ActivationId == op.ActivationId
            ? clock() + ServiceRetry
            : DateTimeOffset.MinValue;
        Mutate(w =>
        {
            var ops = w.Ops
                .Select(o => o.Id == op.Id
                    ? o with { Key = license.Key, ActivationId = license.ActivationId, StaleAnswers = tries, NextAttempt = next }
                    : o)
                .ToList();
            if (license.ActivationId.Length > 0 && !ops.Any(o => o.Kind == LicenseOpKind.Free && o.ActivationId == license.ActivationId))
            {
                ops.Add(new LicenseOp { Kind = LicenseOpKind.Free, Key = license.Key, ActivationId = license.ActivationId, Removed = true });
            }
            return w with { Ops = ops };
        });
    }

    /// <summary>
    /// The user's Remove License refused for good: the license there stays,
    /// and so does its seat; seats of licenses that were there before it, a
    /// removal that found them replaced, are freed all the same.
    /// </summary>
    private void KeepLicense(LicenseOp op)
    {
        var stays = agent.Current?.ActivationId ?? op.ActivationId;
        Mutate(w => w with
        {
            Ops = w.Ops
                .Where(o => o.Id != op.Id && !(o.Kind == LicenseOpKind.Free && o.Removed && !o.Silent
                    && (stays.Length == 0 || o.ActivationId == stays || op.StaleAnswers == 0)))
                .ToList(),
        });
    }

    /// <summary>
    /// The removal of a license revoked here, its activation freed, when the
    /// license is no longer that one: nothing to remove; the service is told
    /// the activation is freed if it lists it to be freed by now (the same
    /// key applied or entered here again), once.
    /// </summary>
    private void NoLongerRevoked(LicenseOp op)
    {
        log("license: no longer revoked here; not removing the license");
        bool tell = ListedToFree(agent.Current, op.ActivationId) && !acknowledged.Contains(op.ActivationId)
            && !work.Ops.Any(o => o.Id != op.Id && (o.Kind is LicenseOpKind.Free or LicenseOpKind.Freed) && o.ActivationId == op.ActivationId);
        if (tell)
        {
            Replace(op, new LicenseOp { Kind = LicenseOpKind.Freed, Key = op.Key, ActivationId = op.ActivationId });
        }
        else
        {
            Drop(op);
        }
    }

    private async Task<ActivationOutcome?> RevokeAtServiceAsync(LicenseOp op)
    {
        try
        {
            await agent.RevokeAsync(op.DeviceId, op.ActivationId, op.Deactivated).ConfigureAwait(false);
            Drop(op);
        }
        catch (AgentRefusal refusal) when (Final(refusal))
        {
            log($"license: the background service refused to revoke the license on {op.DeviceId} ({refusal.Code})");
            Drop(op);
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException)
        {
            log($"license: the background service did not take revoking the license on {op.DeviceId} ({e.Message}); asking again later");
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
