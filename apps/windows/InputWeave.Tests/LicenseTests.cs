using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Licensing;
using InputWeave.Panel;
using Xunit;

namespace InputWeave.Tests;

/// <summary>Polar's answers as testdata/polar has them, shared with the macOS and Linux apps' tests.</summary>
public class PolarTests
{
    private static readonly DateTimeOffset Now = DateTimeOffset.FromUnixTimeSeconds(1_793_800_000);

    internal static string Fixture(string name) => File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "polar", name));

    private static PolarClient Client(IPolarTransport? transport = null) =>
        new(transport ?? new FakePolar(), PolarClient.OrganizationId, () => Now);

    [Fact]
    public void ReadsAnActivation()
    {
        var answer = Client().Read("activate", new PolarReply(200, Fixture("activate-200.json"), Now.AddMinutes(-1)));
        var activated = Assert.IsType<PolarAnswer.Activated>(answer);
        Assert.False(string.IsNullOrEmpty(activated.ActivationId));
        Assert.Contains("@", activated.CustomerEmail);
        Assert.Equal((ulong)Now.AddMinutes(-1).ToUnixTimeSeconds(), activated.ValidatedAt);
    }

    [Fact]
    public void ReadsAConfirmationAndTheKeyAlone()
    {
        var validated = Assert.IsType<PolarAnswer.Validated>(Client().Read("validate", new PolarReply(200, Fixture("validate-200.json"))));
        Assert.Contains("@", validated.CustomerEmail);
        Assert.Equal((ulong)Now.ToUnixTimeSeconds(), validated.ValidatedAt);
        Assert.IsType<PolarAnswer.KeyLive>(Client().Read("validate", new PolarReply(200, Fixture("validate-200-key-only.json")), keyOnly: true));
    }

    [Theory]
    [InlineData("activate-403-limit.json")]
    [InlineData("activate-403-revoked.json")]
    public void TakesPolarsOwn403AsARefusal(string fixture) =>
        Assert.IsType<PolarAnswer.NotPermitted>(Client().Read("activate", new PolarReply(403, Fixture(fixture))));

    [Fact]
    public void TakesCloudflares403AsUnreachable() =>
        Assert.IsType<PolarAnswer.Unreachable>(Client().Read("activate", new PolarReply(403, Fixture("403-cloudflare.json"))));

    [Theory]
    [InlineData("404-not-found.json")]
    [InlineData("404-no-longer-active.json")]
    public void TakesA404AsNotFound(string fixture) =>
        Assert.IsType<PolarAnswer.NotFound>(Client().Read("validate", new PolarReply(404, Fixture(fixture))));

    [Fact]
    public void ReadsMalformedRateLimitedAndServerErrors()
    {
        Assert.IsType<PolarAnswer.Malformed>(Client().Read("activate", new PolarReply(422, Fixture("422-validation.json"))));
        var limited = Assert.IsType<PolarAnswer.RateLimited>(
            Client().Read("activate", new PolarReply(429, Fixture("429-rate-limited.json"), RetryAfter: TimeSpan.FromSeconds(2))));
        Assert.Equal(TimeSpan.FromSeconds(2), limited.RetryAfter);
        Assert.IsType<PolarAnswer.Unreachable>(Client().Read("validate", new PolarReply(503, "")));
        // A 200 from activate that cannot be read may still have taken a seat.
        Assert.IsType<PolarAnswer.Unreadable>(Client().Read("activate", new PolarReply(200, "not json")));
        Assert.IsType<PolarAnswer.Unreachable>(Client().Read("validate", new PolarReply(200, "")));
    }

    [Fact]
    public void TellsA422AboutTheKeyFromOtherOnes()
    {
        Assert.False(Assert.IsType<PolarAnswer.Malformed>(Client().Read("validate", new PolarReply(422, Fixture("422-validation.json")))).AboutKey);
        const string aboutKey = "{\"error\":\"RequestValidationError\",\"detail\":[{\"type\":\"string_too_long\",\"loc\":[\"body\",\"key\"],\"msg\":\"too long\"}]}";
        Assert.True(Assert.IsType<PolarAnswer.Malformed>(Client().Read("activate", new PolarReply(422, aboutKey))).AboutKey);
        Assert.False(Assert.IsType<PolarAnswer.Malformed>(Client().Read("activate", new PolarReply(422, ""))).AboutKey);
        // The fixture's detail names the activation id: no such activation.
        Assert.True(Assert.IsType<PolarAnswer.Malformed>(Client().Read("validate", new PolarReply(422, Fixture("422-validation.json")))).AboutActivation);
    }

    [Fact]
    public void TakesA201FromActivateAsGranted() =>
        Assert.IsType<PolarAnswer.Activated>(Client().Read("activate", new PolarReply(201, Fixture("activate-200.json"))));

    [Fact]
    public void WaitsAMinuteForA429WithoutRetryAfterAndAnHourAtMost()
    {
        Assert.Equal(TimeSpan.FromSeconds(60), Assert.IsType<PolarAnswer.RateLimited>(Client().Read("activate", new PolarReply(429, ""))).RetryAfter);
        Assert.Equal(TimeSpan.FromHours(1), Assert.IsType<PolarAnswer.RateLimited>(
            Client().Read("activate", new PolarReply(429, "", RetryAfter: TimeSpan.FromHours(3)))).RetryAfter);
    }

    [Fact]
    public async Task ATimeoutWhileReadingTheAnswerIsNoAnswer()
    {
        var polar = new FakePolar { Throw = new OperationCanceledException("timed out reading the body") };
        Assert.IsType<PolarAnswer.Unreachable>(await Client(polar).ActivateAsync("INPUTWEAVE-X", "PC"));
    }

    [Theory]
    [InlineData("  Windows\tDesktop \n", "Windows Desktop")]
    [InlineData("\u200B", "InputWeave")]
    [InlineData("", "InputWeave")]
    public void SendsTheComputerNameOnOneLine(string name, string label) => Assert.Equal(label, PolarClient.Label(name));

    [Fact]
    public void CutsALongComputerNameWithoutSplittingACharacter()
    {
        var label = PolarClient.Label(new string('a', 63) + "\U0001F5A5\uFE0F" + "bbbb");
        Assert.Equal(new string('a', 63) + "\U0001F5A5\uFE0F", label);
        Assert.Equal(PolarClient.MaxLabel, new System.Globalization.StringInfo(label).LengthInTextElements);
    }

    [Fact]
    public void FreeingAGoneActivationIsDone()
    {
        Assert.IsType<PolarAnswer.Done>(Client().Read("deactivate", new PolarReply(204, "")));
        Assert.IsType<PolarAnswer.Done>(Client().Read("deactivate", new PolarReply(404, Fixture("404-not-found.json"))));
    }

    [Fact]
    public void UsesThisComputersClockForADateHeaderTooFarAhead()
    {
        var answer = Client().Read("activate", new PolarReply(200, Fixture("activate-200.json"), Now.AddDays(3)));
        Assert.Equal((ulong)Now.ToUnixTimeSeconds(), Assert.IsType<PolarAnswer.Activated>(answer).ValidatedAt);
    }

    [Fact]
    public async Task SendsTheKeyTheOrganizationAndTheLabelWithoutConditions()
    {
        var polar = new FakePolar();
        polar.Next("activate", 200, Fixture("activate-200.json"));
        await Client(polar).ActivateAsync("INPUTWEAVE-ABCDEFGH-1234", "Windows Desktop");
        var (endpoint, json) = Assert.Single(polar.Sent);
        Assert.Equal("activate", endpoint);
        Assert.Contains("\"key\":\"INPUTWEAVE-ABCDEFGH-1234\"", json);
        Assert.Contains($"\"organization_id\":\"{PolarClient.OrganizationId}\"", json);
        Assert.Contains("\"label\":\"Windows Desktop\"", json);
        Assert.DoesNotContain("conditions", json);
    }

    [Fact]
    public async Task ANetworkFailureIsUnreachable()
    {
        var polar = new FakePolar { Fail = true };
        Assert.IsType<PolarAnswer.Unreachable>(await Client(polar).ValidateAsync("INPUTWEAVE-ABCDEFGH-1234", "a"));
    }
}

public class LicenseWorkerTests
{
    private const string Key = "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42";
    private DateTimeOffset now = DateTimeOffset.FromUnixTimeSeconds(1_793_800_000);
    private readonly FakePolar polar = new();
    private readonly FakeLicenseAgent agent = new();
    private readonly MemoryLicenseLedger ledger = new();

    private LicenseWorker Worker() =>
        new(new PolarClient(polar, PolarClient.OrganizationId, () => now), agent, ledger, () => now, () => "Windows Desktop", _ => { });

    private static string Fixture(string name) => PolarTests.Fixture(name);

    [Fact]
    public async Task ActivatesAKeyWaitingForActivationAndReportsIt()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 200, Fixture("activate-200.json"));
        await Worker().TickAsync(shown: false);
        var recorded = Assert.Single(agent.Recorded);
        Assert.Equal(RecordLicenseResult.ResultOneofCase.Activated, recorded.ResultCase);
        Assert.Equal(Key, recorded.Key);
        Assert.Equal("", recorded.ActivationId);
        Assert.Equal(LicenseState.Licensed, agent.License.State);
    }

    [Fact]
    public async Task TellsFullSeatsFromADeadKeyAfterA403()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 403, Fixture("activate-403-limit.json"));
        polar.Next("validate", 200, Fixture("validate-200-key-only.json"));
        Assert.Equal(ActivationOutcome.SeatsFull, await Worker().EnterAsync(Key));
        Assert.Equal(LicenseRefusal.SeatsFull, Assert.Single(agent.Recorded).Refused);
        Assert.DoesNotContain("activation_id", polar.Sent[1].Json);

        agent.Recorded.Clear();
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 403, Fixture("activate-403-revoked.json"));
        polar.Next("validate", 404, Fixture("404-no-longer-active.json"));
        Assert.Equal(ActivationOutcome.Invalid, await Worker().EnterAsync(Key));
        Assert.Equal(LicenseRefusal.Invalid, Assert.Single(agent.Recorded).Refused);
    }

    [Fact]
    public async Task FreesAnActivationTheServiceRefuses()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        agent.Refuse = "license_stale";
        polar.Next("activate", 200, Fixture("activate-200.json"));
        polar.Next("deactivate", 204, "");
        Assert.Equal(ActivationOutcome.ServiceRefused, await Worker().EnterAsync(Key));
        Assert.Equal("deactivate", polar.Sent[^1].Endpoint);
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task OfflineChangesNothingAndTriesAgainLater()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Fail = true;
        var worker = Worker();
        await worker.TickAsync(false);
        Assert.Equal(ActivationOutcome.Offline, worker.Problem(Key));
        Assert.Empty(agent.Recorded);
        int sent = polar.Attempts;
        await worker.TickAsync(false);
        Assert.Equal(sent, polar.Attempts);
        now += LicenseWorker.ActivateRetry;
        await worker.TickAsync(false);
        Assert.Equal(sent + 1, polar.Attempts);
    }

    [Fact]
    public async Task ConfirmsOnlyWhenShownAndDue()
    {
        agent.License = Licensed(validatedAt: now - TimeSpan.FromDays(31));
        var worker = Worker();
        await worker.TickAsync(shown: false);
        Assert.Empty(polar.Sent);
        polar.Next("validate", 200, Fixture("validate-200.json"));
        await worker.TickAsync(shown: true);
        var recorded = Assert.Single(agent.Recorded);
        Assert.Equal(RecordLicenseResult.ResultOneofCase.Validated, recorded.ResultCase);
        Assert.Equal("act-1", recorded.ActivationId);

        agent.Recorded.Clear();
        polar.Sent.Clear();
        agent.License = Licensed(validatedAt: now - TimeSpan.FromDays(3));
        await worker.TickAsync(shown: true);
        Assert.Empty(polar.Sent);
    }

    [Fact]
    public async Task TakesTheLicenseAsInvalidOnlyAfterTwo404sAMinuteApart()
    {
        agent.License = Licensed(validatedAt: now - TimeSpan.FromDays(40));
        var worker = Worker();
        polar.Next("validate", 404, Fixture("404-not-found.json"));
        await worker.TickAsync(true);
        Assert.Empty(agent.Recorded);
        now += TimeSpan.FromSeconds(30);
        await worker.TickAsync(true);
        Assert.Single(polar.Sent);
        now += TimeSpan.FromSeconds(31);
        polar.Next("validate", 404, Fixture("404-not-found.json"));
        await worker.TickAsync(true);
        var recorded = Assert.Single(agent.Recorded);
        Assert.Equal(LicenseRefusal.Invalid, recorded.Refused);
        Assert.Equal("act-1", recorded.ActivationId);
    }

    [Fact]
    public async Task StaysLicensedWhilePolarIsOutOfReach()
    {
        agent.License = Licensed(validatedAt: now - TimeSpan.FromDays(400));
        polar.Fail = true;
        await Worker().TickAsync(true);
        Assert.Empty(agent.Recorded);
        Assert.Equal(LicenseState.Licensed, agent.License.State);
    }

    [Fact]
    public async Task RemovesTheLicenseEvenWhenPolarIsOutOfReach()
    {
        agent.License = Licensed(validatedAt: now);
        polar.Fail = true;
        var worker = Worker();
        Assert.Equal(RemoveOutcome.RemovedSeatNotFreed, await worker.RemoveAsync());
        Assert.True(agent.Removed);
        Assert.False(worker.RemoveFailed);
        // The seat is freed later, once Polar can be reached; the page says so meanwhile.
        var owed = Assert.Single(ledger.Work.Ops);
        Assert.Equal((LicenseOpKind.Free, "act-1", true), (owed.Kind, owed.ActivationId, owed.Removed));
        Assert.True(worker.SeatNotFreed);
        polar.Fail = false;
        polar.Next("deactivate", 204, "");
        now = owed.NextAttempt;
        await worker.TickAsync(false);
        Assert.False(worker.SeatNotFreed);
        Assert.Empty(ledger.Work.Ops);

        ledger.Work = new LicenseWork();
        agent.Removed = false;
        polar.Fail = false;
        agent.License = Licensed(validatedAt: now);
        polar.Next("deactivate", 204, "");
        Assert.Equal(RemoveOutcome.Removed, await Worker().RemoveAsync());
        Assert.True(agent.Removed);
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task AKeyThatIsNotAKeyIsInvalidWithoutAskingPolar()
    {
        agent.RefuseKey = true;
        Assert.Equal(ActivationOutcome.Invalid, await Worker().EnterAsync("nonsense"));
        Assert.Empty(polar.Sent);
    }

    [Fact]
    public async Task WaitsAsLongAsPolarAsks()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 429, Fixture("429-rate-limited.json"), TimeSpan.FromSeconds(20));
        var worker = Worker();
        Assert.Equal(ActivationOutcome.Busy, await worker.EnterAsync(Key));
        Assert.Empty(agent.Recorded);
        // Even a key another computer handed over meanwhile waits until then.
        agent.License = new License { Key = Key + "X", State = LicenseState.PendingActivation };
        int sent = polar.Attempts;
        now += TimeSpan.FromSeconds(10);
        await worker.TickAsync(false);
        Assert.Equal(sent, polar.Attempts);
        now += TimeSpan.FromSeconds(11);
        await worker.TickAsync(false);
        Assert.Equal(sent + 1, polar.Attempts);
    }

    [Fact]
    public async Task KeepsAnActivationTheServiceCouldNotTakeAndReportsItAfterARestart()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 200, Fixture("activate-200.json"));
        agent.RecordUnavailable = true;
        var first = Worker();
        await first.TickAsync(false);
        Assert.Equal(ActivationOutcome.NotRecorded, first.Problem(Key));
        Assert.Equal(LicenseOpKind.Report, Assert.Single(ledger.Work.Ops).Kind);
        Assert.Empty(agent.Recorded);

        // The app starts again: the kept activation is reported, Polar is not asked again.
        agent.RecordUnavailable = false;
        int asked = polar.Sent.Count;
        await Worker().TickAsync(false);
        Assert.Equal(asked, polar.Sent.Count);
        Assert.Equal(RecordLicenseResult.ResultOneofCase.Activated, Assert.Single(agent.Recorded).ResultCase);
        Assert.Empty(ledger.Work.Ops);
        Assert.Equal(LicenseState.Licensed, agent.License.State);
    }

    [Fact]
    public async Task WhileTheServiceIsOutOfReachTheActivationWaitsAMinute()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 200, Fixture("activate-200.json"));
        agent.RecordUnavailable = true;
        var worker = Worker();
        await worker.TickAsync(false);
        await worker.TickAsync(false);
        Assert.Single(polar.Sent);
        agent.RecordUnavailable = false;
        await worker.TickAsync(false);
        Assert.Empty(agent.Recorded);
        now += LicenseWorker.ServiceRetry;
        await worker.TickAsync(false);
        Assert.Single(agent.Recorded);
        Assert.Single(polar.Sent);
    }

    [Fact]
    public async Task FreesARefusedActivationLaterWhenPolarCannotBeReached()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        agent.Refuse = "license_stale";
        polar.Next("activate", 200, Fixture("activate-200.json"));
        var worker = Worker();
        Assert.Equal(ActivationOutcome.ServiceRefused, await worker.EnterAsync(Key));
        // deactivate got a 500: still owed.
        var owed = Assert.Single(ledger.Work.Ops);
        Assert.Equal((LicenseOpKind.Free, 1, false), (owed.Kind, owed.Attempts, owed.Removed));

        int sent = polar.Sent.Count;
        await worker.TickAsync(false);
        Assert.Equal(sent, polar.Sent.Count);
        now = owed.NextAttempt;
        polar.Next("deactivate", 204, "");
        await worker.TickAsync(false);
        Assert.Equal("deactivate", polar.Sent[^1].Endpoint);
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task StopsTryingAKeyTheServiceRefusedUntilTheUserActs()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 404, Fixture("404-not-found.json"));
        agent.Refuse = "invalid_license_result";
        var worker = Worker();
        await worker.TickAsync(false);
        Assert.Equal(ActivationOutcome.ServiceRefused, worker.Problem(Key));
        int sent = polar.Sent.Count;
        now += LicenseWorker.ActivateRetry * 3;
        await worker.TickAsync(false);
        Assert.Equal(sent, polar.Sent.Count);

        polar.Next("activate", 200, Fixture("activate-200.json"));
        Assert.Equal(ActivationOutcome.Licensed, await worker.EnterAsync(Key));
    }

    [Fact]
    public async Task KeepsAskingTheServiceToRemoveTheLicense()
    {
        agent.License = Licensed(validatedAt: now);
        polar.Next("deactivate", 204, "");
        agent.Unavailable = true;
        var worker = Worker();
        Assert.Equal(RemoveOutcome.ServicePending, await worker.RemoveAsync());
        Assert.True(worker.Removing);
        Assert.True(worker.RemoveFailed);
        Assert.False(worker.SeatNotFreed);
        Assert.Equal([LicenseOpKind.Remove, LicenseOpKind.Free], ledger.Work.Ops.Select(o => o.Kind));
        Assert.Equal(LicenseVariant.Trial, LicenseView.For(agent.License, "PC", worker.Removing).Variant);

        agent.Unavailable = false;
        await worker.TickAsync(true);
        Assert.False(agent.Removed);
        now += LicenseWorker.ServiceRetry;
        await worker.TickAsync(true);
        Assert.True(agent.Removed);
        Assert.False(worker.Removing);
        // Removing it never asked Polar to confirm it meanwhile.
        Assert.DoesNotContain(polar.Sent, s => s.Endpoint == "validate");
    }

    [Fact]
    public async Task FreesSeatsWhileTheServiceIsOutOfReach()
    {
        ledger.Work = new LicenseWork
        {
            Ops =
            [
                new LicenseOp { Kind = LicenseOpKind.Report, Key = Key, ActivationId = "act-2" },
                new LicenseOp { Kind = LicenseOpKind.Free, Key = Key, ActivationId = "act-1" },
            ],
        };
        agent.Linked = false;
        agent.Unavailable = true;
        polar.Next("deactivate", 204, "");
        var worker = Worker();
        await worker.TickAsync(false);
        Assert.Equal("deactivate", Assert.Single(polar.Sent).Endpoint);
        // The report waits for the service; it was not even tried.
        var left = Assert.Single(worker.Ops);
        Assert.Equal((LicenseOpKind.Report, 0), (left.Kind, left.Attempts));
    }

    [Fact]
    public async Task DoesTheOldestWorkFirstOneAtATime()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        ledger.Work = new LicenseWork
        {
            Ops =
            [
                new LicenseOp { Kind = LicenseOpKind.Free, Key = "OLD", ActivationId = "act-0" },
                new LicenseOp { Kind = LicenseOpKind.Report, Key = Key, ActivationId = "act-2", CustomerEmail = "x@y.z" },
            ],
        };
        polar.Next("deactivate", 204, "");
        await Worker().TickAsync(false);
        Assert.Equal(["deactivate"], polar.Sent.Select(s => s.Endpoint));
        Assert.Equal("act-2", Assert.Single(agent.Recorded).Activated.ActivationId);
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task AlreadyLicensedWithTheSameActivationIsDone()
    {
        // Another app of this computer's reported it first.
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        agent.Refuse = "already_licensed";
        agent.LicenseOnRefusal = Licensed(validatedAt: now);
        ledger.Work = new LicenseWork { Ops = [new LicenseOp { Kind = LicenseOpKind.Report, Key = Key, ActivationId = "act-1" }] };
        await Worker().TickAsync(false);
        Assert.Empty(polar.Sent);
        Assert.Empty(ledger.Work.Ops);
        Assert.Null(ledger.Work.Stalled);
    }

    [Fact]
    public async Task AlreadyLicensedWithAnotherActivationFreesThisOne()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 200, Fixture("activate-200.json"));
        polar.Next("deactivate", 204, "");
        agent.Refuse = "already_licensed";
        agent.LicenseOnRefusal = Licensed(validatedAt: now);
        Assert.Equal(ActivationOutcome.Licensed, await Worker().EnterAsync(Key));
        Assert.Equal(["activate", "deactivate"], polar.Sent.Select(s => s.Endpoint));
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task AnUnreadableLicenseFileIsTriedAgainNotGivenUp()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 200, Fixture("activate-200.json"));
        agent.Refuse = "license_unavailable";
        var worker = Worker();
        Assert.Equal(ActivationOutcome.Unavailable, await worker.EnterAsync(Key));
        Assert.Null(ledger.Work.Stalled);
        Assert.Equal(LicenseOpKind.Report, Assert.Single(ledger.Work.Ops).Kind);
        now += LicenseWorker.ServiceRetry;
        await worker.TickAsync(false);
        Assert.Equal(LicenseState.Licensed, agent.License.State);
        Assert.Single(polar.Sent);
    }

    [Fact]
    public async Task EnteringTheKeyTriesAllItsWorkAtOnce()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        ledger.Work = new LicenseWork
        {
            Ops = [new LicenseOp { Kind = LicenseOpKind.Free, Key = Key, ActivationId = "act-0", Attempts = 6, NextAttempt = now + TimeSpan.FromHours(5) }],
        };
        polar.Next("activate", 403, Fixture("activate-403-limit.json"));
        polar.Next("validate", 200, Fixture("validate-200-key-only.json"));
        var worker = Worker();
        await worker.TickAsync(false);
        Assert.Equal(["activate", "validate"], polar.Sent.Select(s => s.Endpoint));
        Assert.Equal(LicenseState.SeatsFull, agent.License.State);

        // Try Again after freeing a seat: the seat owed for the key is freed now too, being older.
        polar.Next("activate", 200, Fixture("activate-200.json"));
        polar.Next("deactivate", 204, "");
        Assert.Equal(ActivationOutcome.Licensed, await worker.EnterAsync(Key));
        Assert.Equal(["deactivate", "activate"], polar.Sent.Skip(2).Select(s => s.Endpoint));
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task RemovingKeepsTheSeatToFreeBeforeAskingTheService()
    {
        agent.License = Licensed(validatedAt: now);
        polar.Fail = true;
        agent.Unavailable = true;
        var worker = Worker();
        Assert.Equal(RemoveOutcome.ServicePending, await worker.RemoveAsync());
        Assert.Equal([LicenseOpKind.Remove, LicenseOpKind.Free], ledger.Work.Ops.Select(o => o.Kind));
        // The seat is not freed while the service still has the license.
        Assert.Empty(polar.Sent);
        Assert.Equal(0, polar.Attempts);
        Assert.False(worker.SeatNotFreed);
        Assert.True(worker.RemoveFailed);

        // The app starts again: the service is asked at once, then the seat is freed.
        agent.Unavailable = false;
        polar.Fail = false;
        polar.Next("deactivate", 204, "");
        var again = Worker();
        await again.TickAsync(false);
        Assert.True(agent.Removed);
        Assert.False(again.Removing);
        Assert.Empty(polar.Sent);
        await again.TickAsync(false);
        Assert.Equal(["deactivate"], polar.Sent.Select(s => s.Endpoint));
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task AnOwedRemovalGoesBeforeANewKey()
    {
        agent.License = Licensed(validatedAt: now);
        polar.Next("deactivate", 204, "");
        agent.Unavailable = true;
        var worker = Worker();
        Assert.Equal(RemoveOutcome.ServicePending, await worker.RemoveAsync());
        Assert.Equal(ActivationOutcome.NotRecorded, await worker.EnterAsync(Key + "2"));
        Assert.True(worker.Removing);

        agent.Unavailable = false;
        polar.Next("activate", 200, Fixture("activate-200.json"));
        Assert.Equal(ActivationOutcome.Licensed, await worker.EnterAsync(Key + "2"));
        Assert.True(agent.Removed);
        Assert.Equal((Key + "2", LicenseState.Licensed), (agent.License.Key, agent.License.State));
        // The removed license's seat is freed with the rest of the work.
        await worker.TickAsync(false);
        Assert.Equal(["activate", "deactivate"], polar.Sent.Select(s => s.Endpoint));
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task NotLicensedWhileTheActivationIsBeingFreed()
    {
        agent.License = Licensed(validatedAt: now);
        var worker = Worker();
        Assert.Equal(LicenseVariant.Licensed, LicenseView.For(agent.License, "PC", worker.Withdrawing(agent.License)).Variant);
        ledger.Work = new LicenseWork { Ops = [new LicenseOp { Kind = LicenseOpKind.Free, Key = Key, ActivationId = "act-1", NextAttempt = now + TimeSpan.FromHours(1) }] };
        worker = Worker();
        Assert.True(worker.Withdrawing(agent.License));
        Assert.Equal(LicenseVariant.Trial, LicenseView.For(agent.License, "PC", worker.Withdrawing(agent.License)).Variant);
        await worker.TickAsync(false);
        Assert.Empty(polar.Sent);
    }

    [Fact]
    public async Task SaysTheSeatIsNotFreeOnlyAfterFreeingItFailed()
    {
        agent.License = Licensed(validatedAt: now);
        polar.Next("deactivate", 429, Fixture("429-rate-limited.json"), TimeSpan.FromMinutes(10));
        var worker = Worker();
        Assert.Equal(RemoveOutcome.RemovedSeatNotFreed, await worker.RemoveAsync());
        Assert.True(worker.SeatNotFreed);
        // The app starts again: still owed, still said.
        Assert.True(Worker().SeatNotFreed);
        ledger.Work = new LicenseWork { Ops = [new LicenseOp { Kind = LicenseOpKind.Free, Key = Key, ActivationId = "act-1", Removed = true, NextAttempt = now + TimeSpan.FromHours(1) }] };
        Assert.False(Worker().SeatNotFreed);
    }

    [Fact]
    public async Task ARefusalThisAppDoesNotKnowStandsAsWell()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 200, Fixture("activate-200.json"));
        polar.Next("deactivate", 204, "");
        agent.Refuse = "license_unsupported";
        var worker = Worker();
        Assert.Equal(ActivationOutcome.ServiceRefused, await worker.EnterAsync(Key));
        Assert.Equal(["activate", "deactivate"], polar.Sent.Select(s => s.Endpoint));
        Assert.Equal(Key, ledger.Work.Stalled?.Key);
        Assert.Empty(ledger.Work.Ops);
    }

    [Theory]
    [InlineData("agent_stopping")]
    [InlineData("unexpected")]
    public async Task TheServiceStoppingOrNotAnsweringIsTriedAgain(string code)
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 200, Fixture("activate-200.json"));
        agent.Refuse = code;
        var worker = Worker();
        Assert.Equal(ActivationOutcome.NotRecorded, await worker.EnterAsync(Key));
        // Not freed and not given up: the same activation is given again a minute later.
        Assert.Equal(LicenseOpKind.Report, Assert.Single(ledger.Work.Ops).Kind);
        Assert.Null(ledger.Work.Stalled);
        now += LicenseWorker.ServiceRetry;
        await worker.TickAsync(false);
        Assert.Equal(LicenseState.Licensed, agent.License.State);
        Assert.Equal(["activate"], polar.Sent.Select(s => s.Endpoint));
    }

    [Fact]
    public async Task NobodyAsksPolarSoonerThanItSaid()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 429, Fixture("429-rate-limited.json"), TimeSpan.FromMinutes(10));
        var worker = Worker();
        Assert.Equal(ActivationOutcome.Busy, await worker.EnterAsync(Key));
        int sent = polar.Attempts;
        now += TimeSpan.FromMinutes(1);
        // Try Again, and the app started again: Polar's wait stands.
        Assert.Equal(ActivationOutcome.Busy, await worker.EnterAsync(Key));
        Assert.Equal(ActivationOutcome.Busy, await Worker().EnterAsync(Key));
        Assert.Equal(sent, polar.Attempts);
        now += TimeSpan.FromMinutes(9);
        polar.Next("activate", 200, Fixture("activate-200.json"));
        Assert.Equal(ActivationOutcome.Licensed, await Worker().EnterAsync(Key));
    }

    [Fact]
    public async Task ReportsAGrantOnlyOnceItIsKept()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 200, Fixture("activate-200.json"));
        ledger.FailWhen = w => w.Ops.Any(o => o.Kind == LicenseOpKind.Report);
        var worker = Worker();
        await worker.TickAsync(false);
        Assert.Single(polar.Sent);
        Assert.Empty(agent.Recorded);

        ledger.FailWhen = null;
        await worker.TickAsync(false);
        Assert.Equal(RecordLicenseResult.ResultOneofCase.Activated, Assert.Single(agent.Recorded).ResultCase);
        Assert.Single(polar.Sent);
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task EnteringAKeyAsksPolarAgainRatherThanTellingAnOldRefusal()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 403, Fixture("activate-403-limit.json"));
        polar.Next("validate", 200, Fixture("validate-200-key-only.json"));
        agent.RecordUnavailable = true;
        var worker = Worker();
        await worker.TickAsync(false);
        Assert.Equal(LicenseOpKind.Refuse, Assert.Single(ledger.Work.Ops).Kind);

        // A seat was freed meanwhile; the user enters the key again.
        agent.RecordUnavailable = false;
        polar.Next("activate", 200, Fixture("activate-200.json"));
        Assert.Equal(ActivationOutcome.Licensed, await worker.EnterAsync(Key));
        Assert.DoesNotContain(agent.Recorded, r => r.ResultCase == RecordLicenseResult.ResultOneofCase.Refused);
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task WaitsLongerEachTimeBeforeActivatingAgain()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Fail = true;
        var worker = Worker();
        await worker.TickAsync(false);
        Assert.Equal(now + LicenseWorker.ActivateRetry, Assert.Single(worker.Ops).NextAttempt);
        now += LicenseWorker.ActivateRetry;
        await worker.TickAsync(false);
        Assert.Equal(now + LicenseWorker.ActivateRetry * 2, Assert.Single(worker.Ops).NextAttempt);
        for (int i = 0; i < 6; i++)
        {
            now = worker.Ops[0].NextAttempt;
            await worker.TickAsync(false);
        }
        Assert.Equal(now + LicenseWorker.MaxActivateWait, Assert.Single(worker.Ops).NextAttempt);
    }

    [Fact]
    public async Task A422AboutSomethingElseIsNotAnInvalidKey()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 422, Fixture("422-validation.json"));
        var worker = Worker();
        Assert.Equal(ActivationOutcome.NotRecorded, await worker.EnterAsync(Key));
        Assert.Empty(agent.Recorded);
        Assert.Equal(LicenseOpKind.Activate, Assert.Single(ledger.Work.Ops).Kind);

        const string aboutKey = "{\"detail\":[{\"loc\":[\"body\",\"key\"],\"msg\":\"bad\"}]}";
        polar.Next("activate", 422, aboutKey);
        now += LicenseWorker.ActivateRetry;
        Assert.Equal(ActivationOutcome.Invalid, await worker.EnterAsync(Key));
        Assert.Equal(LicenseRefusal.Invalid, Assert.Single(agent.Recorded).Refused);
    }

    [Fact]
    public async Task ForgetsALeftAloneKeyOnceTheLicenseChanges()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 404, Fixture("404-not-found.json"));
        agent.Refuse = "license_changed";
        var worker = Worker();
        await worker.TickAsync(false);
        Assert.NotNull(ledger.Work.Stalled);
        agent.License = new License { Key = Key, State = LicenseState.Invalid };
        await worker.TickAsync(false);
        Assert.Null(ledger.Work.Stalled);

        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        ledger.Work = ledger.Work with { Stalled = new StalledKey(Key, LicenseState.PendingActivation) };
        worker = Worker();
        polar.Next("deactivate", 204, "");
        await worker.RemoveAsync();
        Assert.Null(ledger.Work.Stalled);
    }

    [Fact]
    public async Task LeavesWorkItCannotReadAloneUntilItCan()
    {
        var owed = new LicenseOp { Kind = LicenseOpKind.Free, Key = Key, ActivationId = "act-1" };
        ledger.Work = new LicenseWork { Ops = [owed] };
        ledger.Unreadable = true;
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        var worker = Worker();
        Assert.True(worker.Unreadable);
        await worker.TickAsync(false);
        Assert.Equal(ActivationOutcome.Unavailable, await worker.EnterAsync(Key));
        Assert.Equal(RemoveOutcome.Unavailable, await worker.RemoveAsync());
        Assert.Empty(polar.Sent);
        Assert.Empty(agent.Keys);
        Assert.False(agent.Removed);
        // Nothing replaced what could not be read.
        Assert.Equal(owed, Assert.Single(ledger.Work.Ops));

        ledger.Unreadable = false;
        polar.Next("deactivate", 204, "");
        polar.Next("activate", 200, Fixture("activate-200.json"));
        now += LicenseWorker.ServiceRetry;
        await worker.TickAsync(false);
        Assert.False(worker.Unreadable);
        Assert.Equal(["deactivate", "activate"], polar.Sent.Select(s => s.Endpoint));
    }

    [Fact]
    public async Task AnActivationWhoseAnswerCannotBeReadIsNotAskedForAgain()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 200, "{}");
        var worker = Worker();
        Assert.Equal(ActivationOutcome.NotRecorded, await worker.EnterAsync(Key));
        Assert.Equal(ActivationOutcome.NotRecorded, worker.Problem(Key));
        now += TimeSpan.FromHours(3);
        await worker.TickAsync(false);
        Assert.Single(polar.Sent);
        Assert.Empty(agent.Recorded);
    }

    [Fact]
    public async Task ARemovalRefusedForGoodHoldsNothingBack()
    {
        agent.License = Licensed(validatedAt: now);
        agent.RefuseRemove = "license_changed";
        var worker = Worker();
        await worker.RemoveAsync();
        Assert.False(worker.Removing);
        // The license stays, and so does its seat.
        Assert.Empty(polar.Sent);
        Assert.Empty(ledger.Work.Ops);
        Assert.Equal(LicenseVariant.Licensed, LicenseView.For(agent.License, "PC", worker.Withdrawing(agent.License)).Variant);
    }

    [Fact]
    public async Task ASeatGrantedAfterRemoveLicenseIsFreed()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Next("activate", 200, Fixture("activate-200.json"));
        polar.Next("deactivate", 204, "");
        var worker = Worker();
        Task<RemoveOutcome>? removing = null;
        // Remove License while Polar is activating this computer.
        polar.OnSend = endpoint =>
        {
            if (endpoint == "activate")
            {
                removing = worker.RemoveAsync();
            }
        };
        await worker.TickAsync(false);
        Assert.NotNull(removing);
        await removing!;
        await worker.TickAsync(false);
        Assert.True(agent.Removed);
        Assert.Empty(agent.Recorded);
        Assert.Equal(["activate", "deactivate"], polar.Sent.Select(s => s.Endpoint));
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task TheKeyGoesToTheServiceWhilePolarIsSlow()
    {
        ledger.Work = new LicenseWork { Ops = [new LicenseOp { Kind = LicenseOpKind.Free, Key = "OLD", ActivationId = "act-0" }] };
        polar.Hold = new TaskCompletionSource();
        var worker = Worker();
        var ticking = worker.TickAsync(false);
        Assert.Equal("deactivate", Assert.Single(polar.Sent).Endpoint);
        var entering = worker.EnterAsync(Key);
        // Polar has not answered the earlier request, and the key is with the service already.
        Assert.Equal([Key], agent.Keys);
        Assert.False(entering.IsCompleted);
        polar.Next("deactivate", 204, "");
        polar.Next("activate", 200, Fixture("activate-200.json"));
        polar.Hold.SetResult();
        await ticking;
        Assert.Equal(ActivationOutcome.Licensed, await entering);
    }

    [Fact]
    public void KeepsWhatItCannotDecryptAndMovesAsideWhatIsNotWork()
    {
        var folder = Path.Combine(Path.GetTempPath(), Path.GetRandomFileName());
        var path = Path.Combine(folder, "license-work.dat");
        try
        {
            Directory.CreateDirectory(folder);
            File.WriteAllBytes(path, [1, 2, 3, 4]);
            Assert.Null(new FileLicenseLedger(path, _ => { }).Load());
            Assert.True(File.Exists(path));

            File.WriteAllBytes(path, FileLicenseLedger.Protect(System.Text.Encoding.UTF8.GetBytes("not the work")));
            Assert.Empty(new FileLicenseLedger(path, _ => { }).Load()!.Ops);
            Assert.False(File.Exists(path));
            Assert.True(File.Exists(path + ".bad"));
        }
        finally
        {
            if (Directory.Exists(folder))
            {
                Directory.Delete(folder, true);
            }
        }
    }

    [Fact]
    public async Task TryAgainDoesNotActivateSoonerThanTheWait()
    {
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation };
        polar.Fail = true;
        var worker = Worker();
        Assert.Equal(ActivationOutcome.Offline, await worker.EnterAsync(Key));
        int asked = polar.Attempts;
        Assert.Equal(ActivationOutcome.Offline, await worker.EnterAsync(Key));
        Assert.Equal(asked, polar.Attempts);
        now += LicenseWorker.ActivateRetry;
        polar.Fail = false;
        polar.Next("activate", 200, Fixture("activate-200.json"));
        Assert.Equal(ActivationOutcome.Licensed, await worker.EnterAsync(Key));
    }

    [Fact]
    public async Task A422AboutTheActivationCountsAsNotFound()
    {
        agent.License = Licensed(validatedAt: now - TimeSpan.FromDays(40));
        var worker = Worker();
        polar.Next("validate", 422, Fixture("422-validation.json"));
        await worker.TickAsync(true);
        now += LicenseWorker.SecondNotFound;
        polar.Next("validate", 422, Fixture("422-validation.json"));
        await worker.TickAsync(true);
        Assert.Equal(LicenseRefusal.Invalid, Assert.Single(agent.Recorded).Refused);
    }

    [Fact]
    public void KeepsTheWorkInAFile()
    {
        var folder = Path.Combine(Path.GetTempPath(), Path.GetRandomFileName());
        var path = Path.Combine(folder, "license-work.dat");
        try
        {
            var file = new FileLicenseLedger(path, _ => { });
            Assert.Empty(file.Load()!.Ops);
            file.Save(new LicenseWork
            {
                Ops =
                [
                    new LicenseOp { Kind = LicenseOpKind.Report, Key = Key, ActivationId = "act-9", CustomerEmail = "x@y.z", ValidatedAt = 1_793_800_000 },
                    new LicenseOp { Kind = LicenseOpKind.Free, Key = Key, ActivationId = "act-1", Attempts = 2, Removed = true, NextAttempt = now },
                    new LicenseOp { Kind = LicenseOpKind.Remove },
                ],
                Stalled = new StalledKey(Key, LicenseState.PendingActivation),
            });
            // Whole keys are in it: encrypted for this user, never in the clear.
            Assert.DoesNotContain("7E42", System.Text.Encoding.UTF8.GetString(File.ReadAllBytes(path)));
            var read = new FileLicenseLedger(path, _ => { }).Load()!;
            Assert.Equal([LicenseOpKind.Report, LicenseOpKind.Free, LicenseOpKind.Remove], read.Ops.Select(o => o.Kind));
            Assert.Equal(("act-9", 1_793_800_000UL), (read.Ops[0].ActivationId, read.Ops[0].ValidatedAt));
            Assert.Equal((2, true, now), (read.Ops[1].Attempts, read.Ops[1].Removed, read.Ops[1].NextAttempt));
            Assert.Equal(LicenseState.PendingActivation, read.Stalled?.State);
            file.Save(new LicenseWork());
            Assert.False(File.Exists(path));
        }
        finally
        {
            if (Directory.Exists(folder))
            {
                Directory.Delete(folder, true);
            }
        }
    }

    [Fact]
    public async Task AppliesTheLicenseAndTakesItBackWithoutPolar()
    {
        agent.License = LicensedWith();
        var worker = Worker();
        Assert.True(await worker.ApplyAsync("mac"));
        Assert.Equal(["mac"], agent.Applied);
        Assert.Equal(LicenseUse.Activating, Assert.Single(agent.License.Devices).Use);
        Assert.True(await worker.DontApplyAsync("mac"));
        Assert.Equal([("mac", "", false)], agent.Revoked);
        Assert.Empty(agent.License.Devices);
        Assert.Empty(polar.Sent);
        Assert.Empty(ledger.Work.Ops);

        // Refused: nothing is kept to try again; the row stays as the service says.
        agent.License = new License { Key = Key, State = LicenseState.PendingActivation, ApplySupported = true };
        Assert.False(await worker.ApplyAsync("mac"));
        Assert.Empty(ledger.Work.Ops);
    }

    private static LicensedDevice Mac(LicenseUse use = LicenseUse.Licensed, string activation = "act-mac") => new()
    {
        Id = "mac", Name = "MacBook Pro", Use = use, Licensed = use == LicenseUse.Licensed,
        ActivationId = use == LicenseUse.Licensed ? activation : "", CanTakeBack = use is LicenseUse.Activating or LicenseUse.Waiting,
    };

    [Theory]
    [InlineData(204)]
    [InlineData(404)]
    public async Task RevokesWithPolarOnceAndSaysItDeactivated(int status)
    {
        agent.License = LicensedWith(Mac());
        polar.OnSend = endpoint => agent.Commands.Add($"polar {endpoint}");
        polar.Next("deactivate", status, status == 404 ? Fixture("404-not-found.json") : "");
        var worker = Worker();
        Assert.True(await worker.RevokeAsync("mac", "act-mac"));
        var (endpoint, json) = Assert.Single(polar.Sent);
        Assert.Equal("deactivate", endpoint);
        Assert.Contains("\"activation_id\":\"act-mac\"", json);
        // With this computer's key: the license is this computer's.
        Assert.Contains($"\"key\":\"{Key}\"", json);
        Assert.Equal(["polar deactivate", "revoke mac"], agent.Commands);
        Assert.Equal([("mac", "act-mac", true)], agent.Revoked);
        Assert.Empty(ledger.Work.Ops);
        // The row turns to trial from the service's answer.
        Assert.Equal(LicenseRowState.Trial, LicenseView.For(StatusWith(agent.License)).Rows[1].State);
    }

    [Theory]
    [InlineData("unreachable")]
    [InlineData("refused")]
    [InlineData("malformed")]
    [InlineData("rate limited")]
    [InlineData("asked to wait")]
    public async Task LeavesTheActivationToTheComputerWhenPolarDidNotDeactivateIt(string how)
    {
        agent.License = LicensedWith(Mac());
        switch (how)
        {
            case "unreachable":
                polar.Fail = true;
                break;
            case "refused":
                polar.Next("deactivate", 403, Fixture("activate-403-limit.json"));
                break;
            case "malformed":
                polar.Next("deactivate", 422, Fixture("422-validation.json"));
                break;
            case "rate limited":
                polar.Next("deactivate", 429, Fixture("429-rate-limited.json"), TimeSpan.FromSeconds(30));
                break;
            case "asked to wait":
                ledger.Work = new LicenseWork { PolarNotBefore = now + TimeSpan.FromSeconds(30) };
                break;
        }
        var worker = Worker();
        Assert.True(await worker.RevokeAsync("mac", "act-mac"));
        Assert.Equal([("mac", "act-mac", false)], agent.Revoked);
        // Asked once at most, and never again: the computer frees it itself.
        Assert.Equal(how == "asked to wait" ? 0 : 1, polar.Attempts);
        Assert.Empty(ledger.Work.Ops);
        now += TimeSpan.FromHours(7);
        await worker.TickAsync(false);
        Assert.Equal(how == "asked to wait" ? 0 : 1, polar.Attempts);
        // Nothing is said: it is another computer's place.
        Assert.False(worker.SeatNotFreed);
        Assert.False(worker.RemoveFailed);
    }

    [Fact]
    public async Task KeepsARevokeAsDecidedUntilTheServiceTakesItAcrossARestart()
    {
        agent.License = LicensedWith(Mac());
        agent.Unavailable = true;
        polar.Next("deactivate", 204, "");
        var worker = Worker();
        Assert.True(await worker.RevokeAsync("mac", "act-mac"));
        var kept = Assert.Single(ledger.Work.Ops);
        Assert.Equal((LicenseOpKind.Revoke, "mac", "act-mac", true), (kept.Kind, kept.DeviceId, kept.ActivationId, kept.Deactivated));
        Assert.False(worker.Withdrawing(agent.License));

        // The app starts again: the service is told what was decided, and Polar is not asked again.
        agent.Unavailable = false;
        var again = Worker();
        await again.TickAsync(false);
        Assert.Equal([("mac", "act-mac", true)], agent.Revoked);
        Assert.Single(polar.Sent);
        Assert.Empty(ledger.Work.Ops);
    }

    [Theory]
    [InlineData("license_unavailable", true)]
    [InlineData("agent_stopping", true)]
    [InlineData("unknown_device", false)]
    [InlineData("license_legacy", false)]
    [InlineData("license_stale", false)]
    public async Task DropsARevokeTheServiceRefusesForGood(string code, bool kept)
    {
        agent.License = LicensedWith(Mac());
        agent.RefuseRevoke = code;
        polar.Next("deactivate", 204, "");
        var worker = Worker();
        await worker.RevokeAsync("mac", "act-mac");
        Assert.Equal(kept, ledger.Work.Ops.Any(o => o.Kind == LicenseOpKind.Revoke));
        if (kept)
        {
            agent.RefuseRevoke = null;
            await worker.TickAsync(false);
            Assert.Empty(agent.Revoked);
            now += LicenseWorker.ServiceRetry;
            await worker.TickAsync(false);
            Assert.Equal([("mac", "act-mac", true)], agent.Revoked);
            Assert.Empty(ledger.Work.Ops);
        }
        Assert.Single(polar.Sent);
    }

    [Fact]
    public async Task RevokesWithoutPolarWhenTheComputerSaidNoActivation()
    {
        agent.License = LicensedWith(Mac(activation: ""));
        await Worker().RevokeAsync("mac", "");
        Assert.Empty(polar.Sent);
        Assert.Equal([("mac", "", false)], agent.Revoked);
    }

    [Fact]
    public async Task RemovingTheLicenseHereRevokesItOnOtherComputersFirst()
    {
        agent.License = LicensedWith(Mac());
        agent.RefuseRevoke = "license_unavailable";
        polar.Next("deactivate", 204, "");
        var worker = Worker();
        await worker.RevokeAsync("mac", "act-mac");
        Assert.Equal(LicenseOpKind.Revoke, Assert.Single(ledger.Work.Ops).Kind);
        agent.RefuseRevoke = null;
        agent.Commands.Clear();
        polar.Next("deactivate", 204, "");
        Assert.Equal(RemoveOutcome.Removed, await worker.RemoveAsync());
        // Without the license here, the service could revoke it nowhere.
        Assert.Equal(["revoke mac", "remove"], agent.Commands);
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task ARemovalWaitsBehindARevokeStillOwed()
    {
        agent.License = LicensedWith();
        ledger.Work = new LicenseWork
        {
            Ops =
            [
                new LicenseOp { Kind = LicenseOpKind.Revoke, Key = Key, DeviceId = "mac", ActivationId = "act-mac" },
                new LicenseOp { Kind = LicenseOpKind.Remove },
            ],
        };
        agent.RefuseRevoke = "license_unavailable";
        var worker = Worker();
        await worker.TickAsync(false);
        Assert.Equal(["revoke mac"], agent.Commands);
        agent.RefuseRevoke = null;
        now += LicenseWorker.ServiceRetry;
        await worker.TickAsync(false);
        Assert.Equal(["revoke mac", "revoke mac", "remove"], agent.Commands);
    }

    [Fact]
    public async Task GivesUpALicenseRevokedHereOnceWithoutAWord()
    {
        agent.License = Revoked();
        Assert.Equal(LicenseVariant.Trial, LicenseView.For(agent.License, "PC").Variant);
        polar.OnSend = endpoint => agent.Commands.Add($"polar {endpoint}");
        // Polar cannot be reached.
        polar.Fail = true;
        var worker = Worker();
        await worker.TickAsync(false);
        Assert.Equal([LicenseOpKind.Remove, LicenseOpKind.Free], ledger.Work.Ops.Select(o => o.Kind));
        Assert.All(ledger.Work.Ops, o => Assert.True(o.Silent));
        Assert.Equal("act-1", ledger.Work.Ops[1].ActivationId);
        Assert.Equal(1, polar.Attempts);
        Assert.True(worker.Withdrawing(agent.License));
        // The activation is freed first: until then the service keeps the
        // license, and opens the app for it.
        Assert.Empty(agent.Commands);
        // Given up once, however often the app ticks.
        now += LicenseWorker.ServiceWait;
        await worker.TickAsync(false);
        now += LicenseWorker.ServiceRetry;
        await worker.TickAsync(true);
        Assert.Single(ledger.Work.Ops, o => o.Kind == LicenseOpKind.Remove);
        Assert.Empty(agent.Commands);
        // Nothing is said: the user here did nothing.
        Assert.False(worker.RemoveFailed);
        Assert.False(worker.SeatNotFreed);

        // Polar again; the service out of reach for a while.
        polar.Fail = false;
        polar.Next("deactivate", 204, "");
        agent.Unavailable = true;
        now = ledger.Work.Ops.Single(o => o.Kind == LicenseOpKind.Free).NextAttempt;
        await worker.TickAsync(false);
        await worker.TickAsync(false);
        Assert.Contains("act-1", Assert.Single(polar.Sent).Json);
        Assert.Equal(LicenseOpKind.Remove, Assert.Single(ledger.Work.Ops).Kind);
        Assert.False(worker.RemoveFailed);
        agent.Unavailable = false;
        now += LicenseWorker.ServiceRetry;
        await worker.TickAsync(false);
        Assert.True(agent.Removed);
        Assert.Equal(["polar deactivate", "remove", "remove"], agent.Commands);
        Assert.Empty(ledger.Work.Ops);
        await worker.TickAsync(false);
        Assert.Empty(ledger.Work.Ops);
        Assert.Single(polar.Sent);
    }

    [Fact]
    public async Task GivesUpALicenseRevokedHereInTheBackground()
    {
        agent.License = Revoked();
        polar.Next("deactivate", 404, Fixture("404-not-found.json"));
        // As when the service started the app in the background: nothing is
        // shown, and the app's ticks do the work; a 404 is freed as well.
        var worker = Worker();
        await worker.TickAsync(shown: false);
        await worker.TickAsync(shown: false);
        Assert.True(agent.Removed);
        Assert.Contains("act-1", Assert.Single(polar.Sent).Json);
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task ALicenseRevokedHereThatCannotBeRemovedIsLeftAlone()
    {
        agent.License = Revoked();
        agent.RefuseRemove = "invalid_state";
        polar.Next("deactivate", 204, "");
        var worker = Worker();
        for (int i = 0; i < 4; i++)
        {
            await worker.TickAsync(false);
            now += LicenseWorker.ServiceRetry;
        }
        // Freed, then asked once.
        Assert.Equal(1, agent.Commands.Count(c => c == "remove"));
        Assert.Equal("deactivate", Assert.Single(polar.Sent).Endpoint);
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task FreesARevokedActivationWhenTheSameKeyIsAppliedAgain()
    {
        agent.License = Revoked();
        // Queued while Polar could not be reached.
        polar.Fail = true;
        var worker = Worker();
        await worker.TickAsync(false);
        Assert.Equal(2, ledger.Work.Ops.Count(o => o.Silent));
        // The same key applied here again: never with the revoked activation,
        // which the service lists to be freed; the key waits to be activated.
        agent.License = agent.Restarted(Key);
        Assert.Single(agent.License.ToFree);
        polar.Fail = false;
        polar.Next("deactivate", 204, "");
        polar.Next("activate", 200, Fixture("activate-200.json"));
        now += TimeSpan.FromHours(1);
        await worker.TickAsync(false);
        await worker.TickAsync(false);
        // Freed once and said; nothing removed; the key activated anew.
        Assert.Contains("act-1", Assert.Single(polar.Sent, s => s.Endpoint == "deactivate").Json);
        Assert.Equal(["act-1"], agent.Freed);
        Assert.DoesNotContain("remove", agent.Commands);
        Assert.Equal(LicenseState.Licensed, agent.License.State);
        Assert.NotEqual("act-1", agent.License.ActivationId);
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task FreesARevokedActivationOnceAnotherKeyIsEnteredAndSaysSo()
    {
        agent.License = Revoked();
        polar.Fail = true;
        var worker = Worker();
        await worker.TickAsync(false);
        Assert.Equal(2, ledger.Work.Ops.Count(o => o.Silent));
        // Another key: the service lists the revoked activation to be freed.
        agent.License = new License { Key = Key + "X", State = LicenseState.PendingActivation, ApplySupported = true };
        agent.License.ToFree.Add(new LicenseActivation { Key = Key, ActivationId = "act-1" });
        polar.Fail = false;
        polar.Next("deactivate", 204, "");
        now += TimeSpan.FromHours(1);
        await worker.TickAsync(false);
        await worker.TickAsync(false);
        // Freed once, with the key it was activated with, and said; the license is not removed.
        var freed = Assert.Single(polar.Sent, s => s.Endpoint == "deactivate");
        Assert.Contains("act-1", freed.Json);
        Assert.Contains($"\"key\":\"{Key}\"", freed.Json);
        Assert.Equal(["act-1"], agent.Freed);
        Assert.DoesNotContain("remove", agent.Commands);
        Assert.Empty(agent.License.ToFree);
        Assert.DoesNotContain(ledger.Work.Ops, o => o.ActivationId == "act-1");
        await worker.TickAsync(false);
        Assert.Single(polar.Sent, s => s.Endpoint == "deactivate");
    }

    [Fact]
    public async Task FreesWhatTheServiceListsWhateverTheLicenseIs()
    {
        // The app started after another key was entered: only the list says so.
        agent.License = new License { ApplySupported = true };
        agent.License.ToFree.Add(new LicenseActivation { Key = Key, ActivationId = "act-1" });
        agent.License.ToFree.Add(new LicenseActivation { Key = Key + "Y", ActivationId = "act-2" });
        agent.Unavailable = true;
        polar.Next("deactivate", 404, Fixture("404-not-found.json"));
        polar.Fail = true;
        var worker = Worker();
        await worker.TickAsync(false);
        // Both kept; Polar could not be reached, and nothing is said of it.
        Assert.Equal([LicenseOpKind.Free, LicenseOpKind.Free], ledger.Work.Ops.Select(o => o.Kind));
        Assert.All(ledger.Work.Ops, o => Assert.True(o.ToFree));
        Assert.False(worker.SeatNotFreed);
        Assert.False(worker.Withdrawing(agent.License));
        // Listed again on the next status: not queued twice.
        await worker.TickAsync(false);
        Assert.Equal(2, ledger.Work.Ops.Count);

        // The app starts again; Polar answers (a 404 is freed too), the service later.
        polar.Fail = false;
        polar.Next("deactivate", 204, "");
        now += TimeSpan.FromHours(1);
        var again = Worker();
        await again.TickAsync(false);
        Assert.Equal(2, polar.Sent.Count);
        Assert.Contains($"\"key\":\"{Key}Y\"", polar.Sent[1].Json);
        Assert.Equal([LicenseOpKind.Freed, LicenseOpKind.Freed], ledger.Work.Ops.Select(o => o.Kind));
        // Kept until the service has it, across a restart.
        agent.Unavailable = false;
        await Worker().TickAsync(false);
        Assert.Equal(["act-1", "act-2"], agent.Freed);
        Assert.Empty(agent.License.ToFree);
        Assert.Empty(ledger.Work.Ops);
        Assert.Equal(2, polar.Sent.Count);
    }

    [Fact]
    public async Task AFreedActivationTheServiceRefusesIsDoneWith()
    {
        agent.License = new License { ApplySupported = true };
        agent.License.ToFree.Add(new LicenseActivation { Key = Key, ActivationId = "act-1" });
        agent.RefuseFreed = "unknown_activation";
        polar.Next("deactivate", 204, "");
        var worker = Worker();
        await worker.TickAsync(false);
        await worker.TickAsync(false);
        Assert.Empty(ledger.Work.Ops);
        // Still listed, as the service refused: not freed again in this run.
        Assert.Single(agent.License.ToFree);
        now += TimeSpan.FromHours(1);
        await worker.TickAsync(false);
        Assert.Single(polar.Sent);
    }

    [Fact]
    public async Task EnteringTheKeyAgainFreesTheRevokedActivationAndActivatesAnew()
    {
        agent.License = Revoked();
        polar.Fail = true;
        var worker = Worker();
        await worker.TickAsync(false);
        Assert.Equal(2, ledger.Work.Ops.Count(o => o.Silent));
        // The same key: the service starts it over, never with the revoked activation.
        polar.Fail = false;
        polar.Next("activate", 200, Fixture("activate-200.json"));
        Assert.Equal(ActivationOutcome.Licensed, await worker.EnterAsync(Key));
        Assert.NotEqual("act-1", agent.License.ActivationId);
        Assert.DoesNotContain("remove", agent.Commands);
        Assert.False(worker.Withdrawing(agent.License));
        // The revoked activation is freed all the same, once Polar's wait is over, and said.
        polar.Next("deactivate", 204, "");
        now += TimeSpan.FromHours(1);
        await worker.TickAsync(false);
        await worker.TickAsync(false);
        Assert.Contains("act-1", Assert.Single(polar.Sent, s => s.Endpoint == "deactivate").Json);
        Assert.Equal(["act-1"], agent.Freed);
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task ARevokedRemovalTheServiceFindsStaleTellsTheFreedActivationOnce()
    {
        agent.License = Revoked();
        polar.Next("deactivate", 204, "");
        // The same key applied here again just as the license is removed:
        // the service has started it over, the revoked activation listed to free.
        agent.NewerOnRemove = agent.Restarted(Key);
        var worker = Worker();
        await worker.TickAsync(false);
        await worker.TickAsync(false);
        Assert.Equal([(Key, "act-1")], agent.RemovedWith);
        Assert.Equal(["act-1"], agent.Freed);
        // Done, and not an error; Polar asked once.
        Assert.False(worker.RemoveFailed);
        Assert.Single(polar.Sent, s => s.Endpoint == "deactivate");
        now += TimeSpan.FromHours(1);
        await worker.TickAsync(false);
        Assert.Equal(1, agent.Commands.Count(c => c == "remove"));
        Assert.Single(polar.Sent, s => s.Endpoint == "deactivate");
    }

    [Fact]
    public async Task AnActivationFreedHereAndListedLaterIsOnlyTold()
    {
        agent.License = Revoked();
        polar.Next("deactivate", 204, "");
        // Stale, but the status listing the activation comes only afterwards.
        var newer = agent.Restarted(Key);
        agent.NewerOnRemove = new License { Key = Key, State = LicenseState.PendingActivation, ApplySupported = true };
        var worker = Worker();
        await worker.TickAsync(false);
        Assert.Equal([(Key, "act-1")], agent.RemovedWith);
        Assert.Empty(agent.Freed);
        Assert.DoesNotContain(ledger.Work.Ops, o => o.ActivationId == "act-1");
        agent.License = newer;
        await worker.TickAsync(false);
        Assert.Equal(["act-1"], agent.Freed);
        // Not freed twice.
        Assert.Single(polar.Sent, s => s.Endpoint == "deactivate");
    }

    [Fact]
    public async Task RemovesOnlyTheLicenseTheUserSaw()
    {
        agent.License = Licensed(validatedAt: now);
        polar.Next("deactivate", 204, "");
        Assert.Equal(RemoveOutcome.Removed, await Worker().RemoveAsync());
        Assert.Equal([(Key, "act-1")], agent.RemovedWith);

        // Out of reach when the user confirmed; another key applied here meanwhile.
        ledger.Work = new LicenseWork();
        agent.RemovedWith.Clear();
        agent.Removed = false;
        agent.License = Licensed(validatedAt: now);
        agent.Unavailable = true;
        polar.Next("deactivate", 204, "");
        polar.Next("deactivate", 204, "");
        var worker = Worker();
        Assert.Equal(RemoveOutcome.ServicePending, await worker.RemoveAsync());
        agent.Unavailable = false;
        agent.License = new License { Key = Key + "X", State = LicenseState.Licensed, ActivationId = "act-9", ApplySupported = true };
        now += LicenseWorker.ServiceRetry;
        await worker.TickAsync(false);
        // Stale: the user wants the license gone, so the one there now is removed too.
        Assert.Equal([(Key, "act-1"), (Key + "X", "act-9")], agent.RemovedWith);
        Assert.True(agent.Removed);
        Assert.False(worker.Removing);
        Assert.False(worker.RemoveFailed);
        await worker.TickAsync(false);
        Assert.Equal(2, agent.RemovedWith.Count);
        // Both seats freed: the one the user gave up, and the one there then.
        Assert.Contains(polar.Sent, s => s.Endpoint == "deactivate" && s.Json.Contains("act-1"));
        Assert.Contains(polar.Sent, s => s.Endpoint == "deactivate" && s.Json.Contains("act-9"));
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task RemovesTheLicenseThereNowWhenTheOneTheUserSawWasReplaced()
    {
        agent.License = Licensed(validatedAt: now);
        // Another activation, of another key, by the time the removal arrives.
        agent.NewerOnRemove = new License { Key = Key + "X", State = LicenseState.Licensed, ActivationId = "act-2", ApplySupported = true };
        polar.Next("deactivate", 204, "");
        polar.Next("deactivate", 204, "");
        var worker = Worker();
        Assert.Equal(RemoveOutcome.Removed, await worker.RemoveAsync());
        Assert.Equal([(Key, "act-1"), (Key + "X", "act-2")], agent.RemovedWith);
        Assert.True(agent.Removed);
        // Both seats freed, each with its own key.
        Assert.Equal(2, polar.Sent.Count(s => s.Endpoint == "deactivate"));
        Assert.Contains(polar.Sent, s => s.Json.Contains("act-1") && s.Json.Contains($"\"key\":\"{Key}\""));
        Assert.Contains(polar.Sent, s => s.Json.Contains("act-2") && s.Json.Contains($"\"key\":\"{Key}X\""));
        Assert.Empty(ledger.Work.Ops);
        Assert.False(worker.RemoveFailed);
    }

    [Fact]
    public async Task GivesUpARemovalThatKeepsFindingTheLicenseReplaced()
    {
        agent.License = Licensed(validatedAt: now);
        agent.StaleRemoves = LicenseWorker.MaxRemoveTries;
        for (int i = 0; i < LicenseWorker.MaxRemoveTries; i++)
        {
            polar.Next("deactivate", 204, "");
        }
        var worker = Worker();
        await worker.RemoveAsync();
        await worker.TickAsync(false);
        // Sent three times in all, each for the license there then.
        Assert.Equal([(Key, "act-1"), (Key, "act-2"), (Key, "act-3")], agent.RemovedWith);
        // Then as any refusal: the license there stays, and so does its seat.
        Assert.False(agent.Removed);
        Assert.Equal(("act-4", LicenseState.Licensed), (agent.License.ActivationId, agent.License.State));
        Assert.False(worker.Removing);
        Assert.Equal(LicenseVariant.Licensed, LicenseView.For(agent.License, "PC", worker.Withdrawing(agent.License)).Variant);
        // The seats of the ones replaced are freed; not the one that stays.
        Assert.Equal(3, polar.Sent.Count(s => s.Endpoint == "deactivate"));
        Assert.DoesNotContain(polar.Sent, s => s.Json.Contains("act-4"));
        Assert.Empty(ledger.Work.Ops);
        now += LicenseWorker.ServiceRetry;
        await worker.TickAsync(false);
        Assert.Equal(3, agent.RemovedWith.Count);
    }

    [Fact]
    public async Task ARemovalThatFindsNoLicenseLeftIsDone()
    {
        agent.License = Licensed(validatedAt: now);
        agent.NewerOnRemove = new License { ApplySupported = true };
        polar.Next("deactivate", 204, "");
        var worker = Worker();
        Assert.Equal(RemoveOutcome.Removed, await worker.RemoveAsync());
        Assert.Equal([(Key, "act-1")], agent.RemovedWith);
        // The seat the user gave up is freed.
        Assert.Contains("act-1", Assert.Single(polar.Sent).Json);
        Assert.Empty(ledger.Work.Ops);
        Assert.False(worker.RemoveFailed);
    }

    [Theory]
    [InlineData(ConnectionState.Connected, false, null, RemovalNotice.None)]
    [InlineData(ConnectionState.Offline, true, null, RemovalNotice.None)]
    [InlineData(ConnectionState.Offline, false, null, RemovalNotice.NotRevoked)]
    [InlineData(ConnectionState.Connected, false, "license_unavailable", RemovalNotice.NotRevoked)]
    public async Task RemovingALicensedComputerThatNeverSaidItsActivationDependsOnItsConnection(
        ConnectionState state, bool offlineForNow, string? refused, RemovalNotice said)
    {
        agent.License = LicensedWith(Mac(activation: ""));
        agent.Devices.Add(new DeviceStatus { Id = "mac", Name = "MacBook Pro", State = state, OfflineForNow = offlineForNow });
        agent.RefuseRevoke = refused;
        bool unpaired = false;
        Assert.Equal(said, await Worker().RemoveDeviceAsync("mac", revoke: true, () => Task.FromResult(unpaired = true)));
        Assert.True(unpaired);
        // Nothing for Polar: there is no activation to name.
        Assert.Empty(polar.Sent);
        if (refused is null)
        {
            Assert.Equal([("mac", "", false)], agent.Revoked);
        }
    }

    [Fact]
    public async Task AnOlderRemovalRemovesWhateverIsThere()
    {
        // Kept by an app before removals named the license.
        ledger.Work = new LicenseWork { Ops = [new LicenseOp { Kind = LicenseOpKind.Remove }] };
        agent.License = Licensed(validatedAt: now);
        await Worker().TickAsync(false);
        Assert.Equal([("", "")], agent.RemovedWith);
        Assert.True(agent.Removed);
    }

    [Fact]
    public async Task RemovingAComputerFreesItsActivationAndRevokesItOnce()
    {
        agent.License = LicensedWith(Mac());
        polar.Next("deactivate", 204, "");
        var worker = Worker();
        bool unpaired = false;
        var notice = await worker.RemoveDeviceAsync("mac", revoke: true, () => Task.FromResult(unpaired = true));
        Assert.Equal(RemovalNotice.None, notice);
        Assert.True(unpaired);
        Assert.Contains("act-mac", Assert.Single(polar.Sent).Json);
        Assert.Equal([("mac", "act-mac", true)], agent.Revoked);
        Assert.Equal(["revoke mac"], agent.Commands);
        Assert.Empty(ledger.Work.Ops);
    }

    [Fact]
    public async Task AnActivationAlreadyGoneCountsAsFreed()
    {
        agent.License = LicensedWith(Mac());
        polar.Next("deactivate", 404, Fixture("404-not-found.json"));
        Assert.Equal(RemovalNotice.None, await Worker().RemoveDeviceAsync("mac", revoke: true, () => Task.CompletedTask));
        Assert.Equal([("mac", "act-mac", true)], agent.Revoked);
    }

    [Theory]
    [InlineData("unreachable")]
    [InlineData("refused")]
    [InlineData("rate limited")]
    [InlineData("asked to wait")]
    [InlineData("no activation")]
    public async Task RemovingAComputerWhoseActivationCannotBeFreedSaysSo(string how)
    {
        agent.License = LicensedWith(Mac(activation: how == "no activation" ? "" : "act-mac"));
        switch (how)
        {
            case "unreachable":
                polar.Fail = true;
                break;
            case "refused":
                polar.Next("deactivate", 403, Fixture("activate-403-limit.json"));
                break;
            case "rate limited":
                polar.Next("deactivate", 429, Fixture("429-rate-limited.json"), TimeSpan.FromSeconds(30));
                break;
            case "asked to wait":
                ledger.Work = new LicenseWork { PolarNotBefore = now + TimeSpan.FromMinutes(5) };
                break;
        }
        var worker = Worker();
        bool unpaired = false;
        Assert.Equal(RemovalNotice.NotRevoked, await worker.RemoveDeviceAsync("mac", revoke: true, () => Task.FromResult(unpaired = true)));
        // Removed all the same, and the computer told to free it itself.
        Assert.True(unpaired);
        var expected = how == "no activation" ? "" : "act-mac";
        Assert.Equal([("mac", expected, false)], agent.Revoked);
        // Asked once at most, and not again later: the computer is gone.
        Assert.Equal(how is "no activation" or "asked to wait" ? 0 : 1, polar.Attempts);
        Assert.DoesNotContain(ledger.Work.Ops, o => o.Kind is LicenseOpKind.Free or LicenseOpKind.Revoke);
    }

    [Theory]
    [InlineData(LicenseUse.Activating, false, RemovalNotice.None)]
    [InlineData(LicenseUse.Waiting, false, RemovalNotice.None)]
    [InlineData(LicenseUse.Waiting, true, RemovalNotice.Offline)]
    public async Task RemovingAComputerOnItsWayToTheLicenseTakesItBack(LicenseUse use, bool keyOutOfReach, RemovalNotice said)
    {
        var mac = Mac(use);
        mac.KeyOutOfReach = keyOutOfReach;
        agent.License = LicensedWith(mac);
        bool unpaired = false;
        Assert.Equal(said, await Worker().RemoveDeviceAsync("mac", revoke: true, () => Task.FromResult(unpaired = true)));
        Assert.True(unpaired);
        // Taken back all the same: it reaches it if it connects before it is forgotten.
        Assert.Equal([("mac", "", false)], agent.Revoked);
        Assert.Empty(polar.Sent);
    }

    [Theory]
    [InlineData(LicenseUse.Licensed, false, false, true, RemovalNotice.NotRevoked)]
    [InlineData(LicenseUse.Licensed, true, false, true, RemovalNotice.NotRevoked)]
    [InlineData(LicenseUse.Licensed, false, true, true, RemovalNotice.None)]
    [InlineData(LicenseUse.Waiting, true, false, true, RemovalNotice.Offline)]
    [InlineData(LicenseUse.Waiting, false, false, true, RemovalNotice.None)]
    [InlineData(LicenseUse.Activating, true, false, true, RemovalNotice.None)]
    [InlineData(LicenseUse.Activating, false, false, true, RemovalNotice.None)]
    // The service did not take the revoke: it could not be revoked; but a
    // place Polar freed is free, and the computer only was not told.
    [InlineData(LicenseUse.Licensed, false, true, false, RemovalNotice.NotTold)]
    [InlineData(LicenseUse.Licensed, false, false, false, RemovalNotice.NotRevoked)]
    [InlineData(LicenseUse.Activating, false, false, false, RemovalNotice.NotRevoked)]
    [InlineData(LicenseUse.Waiting, false, false, false, RemovalNotice.NotRevoked)]
    [InlineData(LicenseUse.Waiting, true, false, false, RemovalNotice.Offline)]
    public void SaysWhatIsLeftAfterRemovingAComputer(LicenseUse use, bool keyOutOfReach, bool deactivated, bool taken, RemovalNotice said)
    {
        // Whether it is connected changes nothing once its activation is known.
        Assert.Equal(said, LicenseWorker.NoticeAfterRemoval(use, keyOutOfReach, deactivated, taken, activationUnknown: false, connected: false));
        Assert.Equal(said, LicenseWorker.NoticeAfterRemoval(use, keyOutOfReach, deactivated, taken, activationUnknown: false, connected: true));
    }

    [Theory]
    // Taken, and connected (or offline for now) as the service answered: it is told to drop the key.
    [InlineData(true, true, RemovalNotice.None)]
    [InlineData(true, false, RemovalNotice.NotRevoked)]
    [InlineData(false, true, RemovalNotice.NotRevoked)]
    [InlineData(false, false, RemovalNotice.NotRevoked)]
    public void SaysWhatIsLeftOfALicensedComputerThatNeverSaidItsActivation(bool taken, bool connected, RemovalNotice said) =>
        Assert.Equal(said, LicenseWorker.NoticeAfterRemoval(LicenseUse.Licensed, false, false, taken, activationUnknown: true, connected));


    [Theory]
    // Polar freed its place: the computer was only not told.
    [InlineData("license_unavailable", LicenseUse.Licensed, false, RemovalNotice.NotTold)]
    [InlineData("license_stale", LicenseUse.Licensed, false, RemovalNotice.NotTold)]
    [InlineData("license_legacy", LicenseUse.Licensed, false, RemovalNotice.NotTold)]
    [InlineData("no answer", LicenseUse.Licensed, false, RemovalNotice.NotTold)]
    [InlineData("license_unavailable", LicenseUse.Activating, false, RemovalNotice.NotRevoked)]
    [InlineData("no answer", LicenseUse.Waiting, false, RemovalNotice.NotRevoked)]
    [InlineData("license_unavailable", LicenseUse.Waiting, true, RemovalNotice.Offline)]
    public async Task RemovingAComputerTheServiceDidNotRevokeSaysSo(string how, LicenseUse use, bool keyOutOfReach, RemovalNotice said)
    {
        var mac = Mac(use);
        mac.KeyOutOfReach = keyOutOfReach;
        agent.License = LicensedWith(mac);
        // Polar deactivates it: only the service's answer is missing.
        polar.Next("deactivate", 204, "");
        if (how == "no answer")
        {
            agent.Unavailable = true;
        }
        else
        {
            agent.RefuseRevoke = how;
        }
        var order = new List<string>();
        polar.OnSend = endpoint => order.Add($"polar {endpoint}");
        var worker = Worker();
        Assert.Equal(said, await worker.RemoveDeviceAsync("mac", revoke: true, () =>
        {
            order.Add(agent.Commands.Contains("revoke mac") ? "unpair after the revoke" : "unpair");
            return Task.CompletedTask;
        }));
        // Asked before the computer is removed, and removed all the same.
        Assert.Equal("unpair after the revoke", order[^1]);
        Assert.Equal(use == LicenseUse.Licensed ? 1 : 0, polar.Sent.Count);
        Assert.Empty(ledger.Work.Ops);
    }

    [Theory]
    [InlineData("license_unavailable")]
    [InlineData("no answer")]
    public async Task RemovingAComputerNeitherPolarNorTheServiceRevokedSaysItCouldNotBeRevoked(string how)
    {
        agent.License = LicensedWith(Mac());
        polar.Fail = true;
        if (how == "no answer")
        {
            agent.Unavailable = true;
        }
        else
        {
            agent.RefuseRevoke = how;
        }
        bool unpaired = false;
        Assert.Equal(RemovalNotice.NotRevoked, await Worker().RemoveDeviceAsync("mac", revoke: true, () => Task.FromResult(unpaired = true)));
        Assert.True(unpaired);
        Assert.Equal(1, polar.Attempts);
    }

    [Fact]
    public async Task RemovingAComputerWithoutRevokingOnlyUnpairs()
    {
        agent.License = LicensedWith(Mac());
        bool unpaired = false;
        Assert.Equal(RemovalNotice.None, await Worker().RemoveDeviceAsync("mac", revoke: false, () => Task.FromResult(unpaired = true)));
        Assert.True(unpaired);
        Assert.Empty(agent.Revoked);
        Assert.Empty(agent.Commands);
        Assert.Empty(polar.Sent);
    }

    [Fact]
    public async Task RemovingAComputerGoesOnWhateverTheServiceSays()
    {
        agent.License = LicensedWith(Mac());
        agent.RefuseRevoke = "unknown_device";
        polar.Fail = true;
        bool unpaired = false;
        Assert.Equal(RemovalNotice.NotRevoked, await Worker().RemoveDeviceAsync("mac", revoke: true, () => Task.FromResult(unpaired = true)));
        Assert.True(unpaired);

        // A computer that could not be removed is not said to be removed.
        Assert.Equal(
            RemovalNotice.None,
            await Worker().RemoveDeviceAsync("mac", revoke: true, () => throw new AgentUnavailableException("not connected")));
    }

    [Fact]
    public void KeepsRevokesInTheFile()
    {
        var folder = Path.Combine(Path.GetTempPath(), Path.GetRandomFileName());
        var path = Path.Combine(folder, "license-work.dat");
        try
        {
            var file = new FileLicenseLedger(path, _ => { });
            file.Save(new LicenseWork
            {
                Ops =
                [
                    new LicenseOp { Kind = LicenseOpKind.Revoke, Key = Key, DeviceId = "mac", ActivationId = "act-mac", Deactivated = true },
                    new LicenseOp { Kind = LicenseOpKind.Remove, Key = Key, ActivationId = "act-1", Silent = true },
                    new LicenseOp { Kind = LicenseOpKind.Free, Key = Key, ActivationId = "act-2", ToFree = true },
                    new LicenseOp { Kind = LicenseOpKind.Freed, Key = Key, ActivationId = "act-3" },
                ],
            });
            var read = new FileLicenseLedger(path, _ => { }).Load()!;
            Assert.Equal([LicenseOpKind.Revoke, LicenseOpKind.Remove, LicenseOpKind.Free, LicenseOpKind.Freed], read.Ops.Select(o => o.Kind));
            Assert.True(read.Ops[2].ToFree);
            Assert.Equal("act-3", read.Ops[3].ActivationId);
            Assert.Equal(("mac", "act-mac", true, false), (read.Ops[0].DeviceId, read.Ops[0].ActivationId, read.Ops[0].Deactivated, read.Ops[0].Silent));
            Assert.Equal(("act-1", true), (read.Ops[1].ActivationId, read.Ops[1].Silent));
        }
        finally
        {
            if (Directory.Exists(folder))
            {
                Directory.Delete(folder, true);
            }
        }
    }

    private static License Revoked() => new() { Key = Key, State = LicenseState.Revoked, ActivationId = "act-1", ApplySupported = true };

    private License LicensedWith(params LicensedDevice[] devices)
    {
        var license = Licensed(validatedAt: now);
        license.ApplySupported = true;
        license.Devices.Add(devices);
        return license;
    }

    /// <summary>The status around <paramref name="license"/>: this computer and a connected MacBook Pro.</summary>
    internal static Status StatusWith(License license)
    {
        var status = new Status { ThisDeviceId = "pc", ThisDeviceName = "Windows Desktop", License = license };
        status.Devices.Add(new DeviceStatus { Id = "mac", Name = "MacBook Pro", Os = "macos", State = ConnectionState.Connected });
        return status;
    }

    private License Licensed(DateTimeOffset validatedAt) => new()
    {
        Key = Key,
        State = LicenseState.Licensed,
        ActivationId = "act-1",
        CustomerEmail = "tester@example.com",
        ValidatedAt = (ulong)validatedAt.ToUnixTimeSeconds(),
    };
}

public class LicenseViewTests
{
    [Theory]
    [InlineData("tester@gmail.com", "t•••@gmail.com")]
    [InlineData("  a@b.c ", "a•••@b.c")]
    [InlineData("", "")]
    [InlineData("nodomain", "•••")]
    public void ShortensTheAddress(string email, string shown) => Assert.Equal(shown, LicenseView.Mask(email));

    [Fact]
    public void ListsThisComputerFirstThenTheLicensedOnes()
    {
        var license = new License { State = LicenseState.Licensed, CustomerEmail = "x@y.z", SourceDeviceName = "MacBook Pro" };
        license.Devices.Add(new LicensedDevice { Id = "b", Name = "Mac mini", Licensed = false });
        license.Devices.Add(new LicensedDevice { Id = "a", Name = "MacBook Pro", Licensed = true });
        var view = LicenseView.For(license, "Windows Desktop");
        Assert.Equal(LicenseVariant.Licensed, view.Variant);
        Assert.Equal(["Windows Desktop", "MacBook Pro", "Mac mini"], view.Computers.Select(c => c.Name));
        Assert.True(view.Computers[0].ThisComputer);
        Assert.Equal(2, view.LicensedCount);
        Assert.Equal("MacBook Pro", view.SourceName);
    }

    [Theory]
    [InlineData(LicenseState.None, LicenseVariant.Trial)]
    [InlineData(LicenseState.PendingActivation, LicenseVariant.Trial)]
    [InlineData(LicenseState.SeatsFull, LicenseVariant.SeatsFull)]
    [InlineData(LicenseState.Invalid, LicenseVariant.Invalid)]
    [InlineData(LicenseState.Unavailable, LicenseVariant.Unavailable)]
    public void EveryStateButLicensedIsATrial(LicenseState state, LicenseVariant variant)
    {
        var view = LicenseView.For(new License { State = state }, "PC");
        Assert.Equal(variant, view.Variant);
        Assert.False(view.IsLicensed);
        Assert.Empty(view.Computers);
    }

    [Fact]
    public void AServiceWithoutLicensesShowsNone() => Assert.False(LicenseView.For(null, "PC").Shown);

    /// <summary>This computer and every computer it trusts, one of each kind of row.</summary>
    private static Status Everyone(LicenseState state = LicenseState.Licensed)
    {
        var license = new License { Key = "INPUTWEAVE-KEY", State = state, ActivationId = "act-pc", ApplySupported = true };
        var status = new Status { ThisDeviceId = "pc", ThisDeviceName = "Windows Desktop", License = license };
        void Add(string id, string name, LicenseUse? use, ConnectionState connection = ConnectionState.Connected,
            bool offlineForNow = false, bool legacy = false, string activation = "", bool takeBack = false)
        {
            status.Devices.Add(new DeviceStatus { Id = id, Name = name, Os = "macos", State = connection, OfflineForNow = offlineForNow });
            if (use is { } u)
            {
                license.Devices.Add(new LicensedDevice
                {
                    Id = id, Name = name, Use = u, Licensed = u == LicenseUse.Licensed, Legacy = legacy, ActivationId = activation,
                    CanTakeBack = takeBack,
                });
            }
        }
        Add("mac", "MacBook Pro", LicenseUse.Licensed, activation: "act-mac");
        Add("mini", "Mac mini", null, ConnectionState.Offline);
        Add("studio", "Studio PC", LicenseUse.Activating, takeBack: true);
        Add("laptop", "ThinkPad", LicenseUse.Waiting, ConnectionState.Offline, takeBack: true);
        Add("media", "Living Room PC", LicenseUse.NoPlaces);
        Add("work", "Work Laptop", LicenseUse.OtherLicense, offlineForNow: true, connection: ConnectionState.Offline);
        Add("old", "Old iMac", LicenseUse.Licensed, legacy: true);
        return status;
    }

    [Fact]
    public void ListsThisComputerThenEveryTrustedComputerAsItStands()
    {
        var view = LicenseView.For(Everyone());
        Assert.True(view.ApplySupported);
        Assert.Equal(
            [
                new LicenseRow("pc", "Windows Desktop", LicenseRowState.ThisComputer, LicenseRowLine.ThisComputer),
                new LicenseRow("mac", "MacBook Pro", LicenseRowState.Licensed, LicenseRowLine.None, ActivationId: "act-mac"),
                new LicenseRow("mini", "Mac mini", LicenseRowState.Trial, LicenseRowLine.Offline),
                new LicenseRow("studio", "Studio PC", LicenseRowState.Activating, LicenseRowLine.None, TakeBack: true),
                new LicenseRow("laptop", "ThinkPad", LicenseRowState.Waiting, LicenseRowLine.Offline, TakeBack: true),
                new LicenseRow("media", "Living Room PC", LicenseRowState.NoPlaces, LicenseRowLine.None),
                new LicenseRow("work", "Work Laptop", LicenseRowState.OtherLicense, LicenseRowLine.OfflineForNow),
                new LicenseRow("old", "Old iMac", LicenseRowState.Licensed, LicenseRowLine.None, Legacy: true),
            ],
            view.Rows);
        // What each offers.
        Assert.Equal(["mac"], view.Rows.Where(r => r.CanRevoke).Select(r => r.Id));
        Assert.Equal(["mini", "media"], view.Rows.Where(r => r.CanApply).Select(r => r.Id));
        Assert.Equal(["studio", "laptop"], view.Rows.Where(r => r.CanTakeBack).Select(r => r.Id));
        // What is said under the names.
        Assert.Equal(TextKeys.LicenseThisComputer, view.Rows[0].LineKey);
        Assert.Equal(TextKeys.ConnectionOffline, view.Rows[2].LineKey);
        Assert.Equal(TextKeys.ConnectionOfflineForNow, view.Rows[6].LineKey);
        Assert.Null(view.Rows[1].LineKey);
        Assert.Equal(TextKeys.LicenseOtherLicenseNote, view.Rows[6].NoteKey);
        Assert.Equal(TextKeys.LicenseLegacyNote, view.Rows[7].NoteKey);
        Assert.Null(view.Rows[1].NoteKey);
        // "3 of 5": this computer and the two licensed with this key.
        Assert.Equal(3, view.LicensedCount);
        // The places: those and the ones activating or waiting.
        Assert.Equal(5, view.PlacesUsed);
        Assert.True(view.AllPlacesUsed);
        Assert.True(view.NoPlaces);
    }

    [Theory]
    [InlineData(LicenseUse.Activating, true, true)]
    [InlineData(LicenseUse.Activating, false, false)]
    [InlineData(LicenseUse.Waiting, true, true)]
    [InlineData(LicenseUse.Licensed, true, false)]
    [InlineData(LicenseUse.None, true, false)]
    public void OffersDontApplyOnlyWhatCanBeTakenBack(LicenseUse use, bool canTakeBack, bool offered)
    {
        var status = Everyone();
        var studio = status.License.Devices.First(d => d.Id == "studio");
        (studio.Use, studio.CanTakeBack) = (use, canTakeBack);
        var row = LicenseView.For(status).Rows.First(r => r.Id == "studio");
        Assert.Equal(offered, row.CanTakeBack);
        // One button at most.
        Assert.True(new[] { row.CanRevoke, row.CanApply, row.CanTakeBack }.Count(b => b) <= 1);
    }

    [Theory]
    [InlineData(LicenseUse.Licensed, false, true, false)]
    [InlineData(LicenseUse.Activating, false, true, false)]
    [InlineData(LicenseUse.Waiting, false, true, false)]
    [InlineData(LicenseUse.Waiting, true, false, true)]
    public void ShowsTheNoteForInputWeave10WhenTheKeyCannotBeTakenBack(LicenseUse use, bool canTakeBack, bool note, bool dontApply)
    {
        var status = Everyone();
        var old = status.License.Devices.First(d => d.Id == "old");
        (old.Use, old.CanTakeBack) = (use, canTakeBack);
        var row = LicenseView.For(status).Rows.First(r => r.Id == "old");
        Assert.Equal(note ? TextKeys.LicenseLegacyNote : null, row.NoteKey);
        Assert.Equal(dontApply, row.CanTakeBack);
        // Never Revoke License on InputWeave 1.0.
        Assert.False(row.CanRevoke);
    }

    [Fact]
    public void SaysWhenInputWeave10DidNotTakeTheLicense()
    {
        var status = Everyone();
        status.Devices.Add(new DeviceStatus { Id = "den", Name = "Den PC", Os = "windows", State = ConnectionState.Connected });
        status.License.Devices.Add(new LicensedDevice { Id = "den", Name = "Den PC", Use = LicenseUse.None, Legacy = true, LegacyRefused = true });
        var row = LicenseView.For(status).Rows.First(r => r.Id == "den");
        Assert.Equal(LicenseRowState.Trial, row.State);
        Assert.Equal(TextKeys.LicenseLegacyRefusedNote, row.NoteKey);
        Assert.True(row.CanApply);
        // Applied as usual: not while every place is used.
        Assert.True(LicenseView.For(status).AllPlacesUsed);
        status.License.Devices.Remove(status.License.Devices.First(d => d.Id == "laptop"));
        Assert.False(LicenseView.For(status).AllPlacesUsed);
        // Without the flag, a plain trial row.
        status.License.Devices.First(d => d.Id == "den").LegacyRefused = false;
        Assert.Null(LicenseView.For(status).Rows.First(r => r.Id == "den").NoteKey);
    }

    /// <summary>This computer not licensed, with the computers it applied the license to waiting.</summary>
    private static Status Waiting(LicenseState state, string source = "")
    {
        var license = new License { Key = "INPUTWEAVE-KEY", State = state, ApplySupported = true, SourceDeviceId = source };
        var status = new Status { ThisDeviceId = "pc", ThisDeviceName = "Windows Desktop", License = license };
        status.Devices.Add(new DeviceStatus { Id = "mac", Name = "MacBook Pro", Os = "macos", State = ConnectionState.Connected });
        status.Devices.Add(new DeviceStatus { Id = "laptop", Name = "ThinkPad", Os = "linux", State = ConnectionState.Offline });
        status.Devices.Add(new DeviceStatus { Id = "studio", Name = "Studio PC", Os = "windows", State = ConnectionState.Connected });
        status.Devices.Add(new DeviceStatus { Id = "old", Name = "Old iMac", Os = "macos", State = ConnectionState.Connected });
        license.Devices.Add(new LicensedDevice { Id = "mac", Name = "MacBook Pro", Use = LicenseUse.OtherLicense });
        license.Devices.Add(new LicensedDevice { Id = "laptop", Name = "ThinkPad", Use = LicenseUse.Waiting, CanTakeBack = true, WaitingToApply = true });
        license.Devices.Add(new LicensedDevice { Id = "studio", Name = "Studio PC", Use = LicenseUse.Waiting, CanTakeBack = true, WaitingToApply = true });
        license.Devices.Add(new LicensedDevice { Id = "old", Name = "Old iMac", Use = LicenseUse.Waiting, Legacy = true, WaitingToApply = true });
        // Offline with the key its own user entered there: waiting, but not for an apply from here.
        status.Devices.Add(new DeviceStatus { Id = "own", Name = "Office PC", Os = "windows", State = ConnectionState.Offline });
        license.Devices.Add(new LicensedDevice { Id = "own", Name = "Office PC", Use = LicenseUse.Waiting });
        return status;
    }

    [Theory]
    [InlineData(LicenseState.SeatsFull, "", LicenseVariant.SeatsFull)]
    [InlineData(LicenseState.SeatsFull, "mac", LicenseVariant.Trial)]
    [InlineData(LicenseState.Invalid, "", LicenseVariant.Invalid)]
    [InlineData(LicenseState.PendingActivation, "", LicenseVariant.Trial)]
    public void ListsWhatWaitsUntilThisComputerIsLicensedAgain(LicenseState state, string source, LicenseVariant variant)
    {
        var view = LicenseView.For(Waiting(state, source));
        Assert.Equal(variant, view.Variant);
        Assert.Equal(
            [
                new LicenseRow("laptop", "ThinkPad", LicenseRowState.AwaitingLicense, LicenseRowLine.Offline, TakeBack: true),
                new LicenseRow("studio", "Studio PC", LicenseRowState.AwaitingLicense, LicenseRowLine.None, TakeBack: true),
                new LicenseRow("old", "Old iMac", LicenseRowState.AwaitingLicense, LicenseRowLine.None, Legacy: true),
            ],
            view.WaitingToApply);
        Assert.Equal([true, true, false], view.WaitingToApply.Select(r => r.CanTakeBack));
        Assert.Equal([null, null, TextKeys.LicenseLegacyNote], view.WaitingToApply.Select(r => r.NoteKey));
        // Nothing else of the licensed list, and not the one waiting with its own key.
        Assert.All(view.WaitingToApply, r => Assert.False(r.CanApply || r.CanRevoke));
        Assert.DoesNotContain(view.WaitingToApply, r => r.Id == "own");
        Assert.Empty(view.Rows);
    }

    [Fact]
    public void ListsWhatWaitsWhileThisComputersLicenseIsBeingRemoved()
    {
        // The record is still licensed while the removal is on its way; the
        // applies still owed, offline, wait for a license here again.
        var view = LicenseView.For(Waiting(LicenseState.Licensed), removing: true);
        Assert.Equal(LicenseVariant.Trial, view.Variant);
        Assert.Equal(["laptop", "studio", "old"], view.WaitingToApply.Select(r => r.Id));
        Assert.Empty(view.Rows);
    }

    [Fact]
    public void ListsNothingWaitingWhileLicensedOrUnreadable()
    {
        // Licensed: they are on the list itself, offline and waiting to connect,
        // with the one waiting with its own key.
        var licensed = LicenseView.For(Waiting(LicenseState.Licensed));
        Assert.Empty(licensed.WaitingToApply);
        Assert.Equal(LicenseRowState.Waiting, licensed.Rows.First(r => r.Id == "laptop").State);
        Assert.Equal(LicenseRowState.Waiting, licensed.Rows.First(r => r.Id == "own").State);
        // Nothing to take back there: no Don't apply at all, not a disabled one.
        Assert.False(licensed.Rows.First(r => r.Id == "own").CanTakeBack);
        Assert.True(licensed.Rows.First(r => r.Id == "laptop").CanTakeBack);
        Assert.Empty(LicenseView.For(Waiting(LicenseState.Unavailable)).WaitingToApply);
        var before = Waiting(LicenseState.Invalid);
        before.License.ApplySupported = false;
        Assert.Empty(LicenseView.For(before).WaitingToApply);
    }

    [Fact]
    public void OffersToApplyTheLicenseUntilEveryPlaceIsUsed()
    {
        var status = Everyone();
        // The one waiting for it taken back: a place is free again.
        status.License.Devices.Remove(status.License.Devices.First(d => d.Id == "laptop"));
        var view = LicenseView.For(status);
        Assert.Equal(LicenseRowState.Trial, view.Rows.First(r => r.Id == "laptop").State);
        Assert.Equal(4, view.PlacesUsed);
        Assert.False(view.AllPlacesUsed);

        // Nobody else on the license.
        status.License.Devices.Clear();
        view = LicenseView.For(status);
        Assert.Equal((1, 1, false, false), (view.LicensedCount, view.PlacesUsed, view.AllPlacesUsed, view.NoPlaces));
        Assert.All(view.Rows.Skip(1), r => Assert.Equal(LicenseRowState.Trial, r.State));
    }

    [Fact]
    public void ABackgroundServiceBefore11ListsTheComputersAsBefore()
    {
        var status = Everyone();
        status.License.ApplySupported = false;
        var view = LicenseView.For(status);
        Assert.False(view.ApplySupported);
        Assert.Empty(view.Rows);
        Assert.Equal("Windows Desktop", view.Computers[0].Name);
    }

    [Theory]
    [InlineData(LicenseState.None)]
    [InlineData(LicenseState.PendingActivation)]
    [InlineData(LicenseState.Revoked)]
    public void OnTrialNamesAComputerToApplyTheLicenseFrom(LicenseState state)
    {
        var status = Everyone(state);
        var view = LicenseView.For(status);
        Assert.Equal(LicenseVariant.Trial, view.Variant);
        Assert.Empty(view.Rows);
        // The first licensed one, never one on InputWeave 1.0.
        Assert.Equal("MacBook Pro", view.LicensedElsewhere);

        status.License.Devices.Remove(status.License.Devices.First(d => d.Id == "mac"));
        Assert.Equal("Work Laptop", LicenseView.For(status).LicensedElsewhere);
        status.License.Devices.Remove(status.License.Devices.First(d => d.Id == "work"));
        // Only one on InputWeave 1.0 is left, which cannot apply it here.
        Assert.Null(LicenseView.For(status).LicensedElsewhere);

        var before = Everyone(state);
        before.License.ApplySupported = false;
        Assert.Null(LicenseView.For(before).LicensedElsewhere);
        // Licensed here, there is nothing to apply from elsewhere.
        Assert.Null(LicenseView.For(Everyone()).LicensedElsewhere);
    }

    [Fact]
    public void SaysEveryPlaceIsTakenOnlyForAKeyEnteredHere()
    {
        var license = new License { Key = "INPUTWEAVE-KEY", State = LicenseState.SeatsFull, ApplySupported = true };
        Assert.Equal(LicenseVariant.SeatsFull, LicenseView.For(license, "PC").Variant);
        // Applied from another computer: plainly on trial; that one says why.
        license.SourceDeviceId = "mac";
        license.SourceDeviceName = "MacBook Pro";
        Assert.Equal(LicenseVariant.Trial, LicenseView.For(license, "PC").Variant);
        // A background service before 1.1 synced it: as before.
        license.ApplySupported = false;
        Assert.Equal(LicenseVariant.SeatsFull, LicenseView.For(license, "PC").Variant);
    }

    [Fact]
    public void ALicenseRevokedHereIsATrial()
    {
        var view = LicenseView.For(new License { Key = "INPUTWEAVE-KEY", State = LicenseState.Revoked, ActivationId = "act", ApplySupported = true }, "PC");
        Assert.Equal(LicenseVariant.Trial, view.Variant);
        Assert.False(view.IsLicensed);
    }

    [Fact]
    public void OffersToRevokeTheLicenseOfAComputerBeingRemoved()
    {
        var license = Everyone().License;
        Assert.Equal(RemovalOffer.Revoke, LicenseView.OnRemoval(license, thisLicensed: true, "mac"));
        Assert.Equal(RemovalOffer.Revoke, LicenseView.OnRemoval(license, thisLicensed: true, "studio"));
        Assert.Equal(RemovalOffer.Revoke, LicenseView.OnRemoval(license, thisLicensed: true, "laptop"));
        // InputWeave 1.0 keeps its place: a line instead of the box.
        Assert.Equal(RemovalOffer.Legacy, LicenseView.OnRemoval(license, thisLicensed: true, "old"));
        // Not on this computer's license.
        Assert.Equal(RemovalOffer.None, LicenseView.OnRemoval(license, thisLicensed: true, "mini"));
        Assert.Equal(RemovalOffer.None, LicenseView.OnRemoval(license, thisLicensed: true, "media"));
        Assert.Equal(RemovalOffer.None, LicenseView.OnRemoval(license, thisLicensed: true, "work"));
        // This computer on trial, or removing its license.
        Assert.Equal(RemovalOffer.None, LicenseView.OnRemoval(license, thisLicensed: false, "mac"));
        Assert.Equal(RemovalOffer.None, LicenseView.OnRemoval(null, thisLicensed: true, "mac"));
        license.ApplySupported = false;
        Assert.Equal(RemovalOffer.None, LicenseView.OnRemoval(license, thisLicensed: true, "mac"));
    }
}

/// <summary>Polar's endpoints, answering from a queue per endpoint.</summary>
internal sealed class FakePolar : IPolarTransport
{
    private readonly Dictionary<string, Queue<PolarReply>> replies = [];

    public List<(string Endpoint, string Json)> Sent { get; } = [];

    public int Attempts { get; private set; }

    public bool Fail { get; set; }

    public void Next(string endpoint, int status, string body, TimeSpan? retryAfter = null)
    {
        if (!replies.TryGetValue(endpoint, out var queue))
        {
            replies[endpoint] = queue = new Queue<PolarReply>();
        }
        queue.Enqueue(new PolarReply(status, body, RetryAfter: retryAfter));
    }

    /// <summary>Runs as a request goes out, before it is answered.</summary>
    public Action<string>? OnSend { get; set; }

    /// <summary>Requests wait for this before they are answered, as a slow Polar would.</summary>
    public TaskCompletionSource? Hold { get; set; }

    /// <summary>Thrown by every request, as a transport failure.</summary>
    public Exception? Throw { get; set; }

    public async Task<PolarReply> SendAsync(string endpoint, string json, CancellationToken cancellation)
    {
        Attempts++;
        if (Fail)
        {
            throw new HttpRequestException("no network");
        }
        if (Throw is { } failure)
        {
            throw failure;
        }
        Sent.Add((endpoint, json));
        OnSend?.Invoke(endpoint);
        if (Hold is { } hold)
        {
            await hold.Task;
        }
        return replies.TryGetValue(endpoint, out var queue) && queue.Count > 0
            ? queue.Dequeue()
            : new PolarReply(500, "");
    }
}

/// <summary>The service's side of the license: keeps it as the real agent would for the answers the tests give.</summary>
internal sealed class FakeLicenseAgent : ILicenseAgent
{
    public License License { get; set; } = new();

    public List<RecordLicenseResult> Recorded { get; } = [];

    public bool Removed { get; set; }

    /// <summary>The error code to refuse the next RecordLicenseResult with.</summary>
    public string? Refuse { get; set; }

    /// <summary>The license the service holds when it refuses, as another computer's sync could leave it.</summary>
    public License? LicenseOnRefusal { get; set; }

    public bool RefuseKey { get; set; }

    /// <summary>The service out of reach: every command throws.</summary>
    public bool Unavailable { get; set; }

    /// <summary>Only reports fail as if the service were out of reach.</summary>
    public bool RecordUnavailable { get; set; }

    public bool Linked { get; set; } = true;

    public License? Current => License;

    /// <summary>The keys given to the service, in order.</summary>
    public List<string> Keys { get; } = [];

    /// <summary>The error code to refuse Remove License with.</summary>
    public string? RefuseRemove { get; set; }

    public Task SetKeyAsync(string key)
    {
        Keys.Add(key);
        if (Unavailable)
        {
            throw new AgentUnavailableException("not connected");
        }
        if (RefuseKey)
        {
            throw new AgentRefusal("invalid_license_key", "not a key");
        }
        if (License.Key != key || License.State != LicenseState.Licensed)
        {
            License = Restarted(key);
        }
        return Task.CompletedTask;
    }

    public Task RecordAsync(RecordLicenseResult result)
    {
        if (Unavailable || RecordUnavailable)
        {
            throw new AgentUnavailableException("not connected");
        }
        if (Refuse is { } code)
        {
            Refuse = null;
            if (LicenseOnRefusal is { } license)
            {
                License = license;
            }
            throw new AgentRefusal(code, "refused");
        }
        Recorded.Add(result);
        switch (result.ResultCase)
        {
            case RecordLicenseResult.ResultOneofCase.Activated:
                License = Kept(new License
                {
                    Key = result.Key,
                    State = LicenseState.Licensed,
                    ActivationId = result.Activated.ActivationId,
                    CustomerEmail = result.Activated.CustomerEmail,
                    ValidatedAt = result.Activated.ValidatedAt,
                });
                break;
            case RecordLicenseResult.ResultOneofCase.Refused:
                License = Kept(new License
                {
                    Key = result.Key,
                    State = result.Refused == LicenseRefusal.SeatsFull ? LicenseState.SeatsFull : LicenseState.Invalid,
                });
                break;
        }
        return Task.CompletedTask;
    }

    /// <summary>The license and device commands, in order: "remove", "apply mac", "revoke mac".</summary>
    public List<string> Commands { get; } = [];

    /// <summary>The computers the license was applied to, in order.</summary>
    public List<string> Applied { get; } = [];

    /// <summary>The revokes the service took, in order.</summary>
    public List<(string DeviceId, string ActivationId, bool Deactivated)> Revoked { get; } = [];

    /// <summary>The error code to refuse Revoke License with, every time.</summary>
    public string? RefuseRevoke { get; set; }

    /// <summary>The activations the service was told are freed, in order.</summary>
    public List<string> Freed { get; } = [];

    /// <summary>The error code to refuse LicenseActivationFreed with, every time.</summary>
    public string? RefuseFreed { get; set; }

    public Task ActivationFreedAsync(string activationId)
    {
        Commands.Add($"freed {activationId}");
        if (Unavailable)
        {
            throw new AgentUnavailableException("not connected");
        }
        if (RefuseFreed is { } code)
        {
            throw new AgentRefusal(code, "refused");
        }
        Freed.Add(activationId);
        License = License.Clone();
        foreach (var listed in License.ToFree.Where(a => a.ActivationId == activationId).ToList())
        {
            License.ToFree.Remove(listed);
        }
        return Task.CompletedTask;
    }

    /// <summary>
    /// The key, waiting for activation, as the service starts it over: a
    /// revoked activation, never licensed again, is listed to be freed,
    /// whatever key it is.
    /// </summary>
    public License Restarted(string key)
    {
        var next = new License { Key = key, State = LicenseState.PendingActivation, ApplySupported = License.ApplySupported };
        next.ToFree.Add(License.ToFree);
        if (License.State == LicenseState.Revoked)
        {
            next.ToFree.Add(new LicenseActivation { Key = License.Key, ActivationId = License.ActivationId });
        }
        return next;
    }

    /// <summary>A new record, with what the service keeps across it: whether it applies licenses, and the activations to free.</summary>
    private License Kept(License next)
    {
        next.ApplySupported = License.ApplySupported;
        next.ToFree.Add(License.ToFree);
        return next;
    }

    /// <summary>The next removals find the license replaced by another activation, each time.</summary>
    public int StaleRemoves { get; set; }

    /// <summary>What each RemoveLicense named, in order.</summary>
    public List<(string Key, string ActivationId)> RemovedWith { get; } = [];

    /// <summary>The service's newer license, which it has once asked to remove: a removal that waited past it is stale.</summary>
    public License? NewerOnRemove { get; set; }

    public Task RemoveAsync(string key, string activationId)
    {
        Commands.Add("remove");
        if (Unavailable)
        {
            throw new AgentUnavailableException("not connected");
        }
        RemovedWith.Add((key, activationId));
        if (NewerOnRemove is { } newer)
        {
            License = newer;
            NewerOnRemove = null;
        }
        else if (StaleRemoves > 0)
        {
            // Another activation of the key meanwhile, every time.
            StaleRemoves--;
            License = License.Clone();
            License.ActivationId = $"act-{RemovedWith.Count + 1}";
        }
        if (RefuseRemove is { } code)
        {
            throw new AgentRefusal(code, "refused");
        }
        if (key.Length != 0 && (License.Key != key || License.ActivationId != activationId))
        {
            throw new AgentRefusal("license_stale", "a newer license");
        }
        Removed = true;
        var removed = new License { OptedOut = true, ApplySupported = License.ApplySupported };
        removed.ToFree.Add(License.ToFree);
        License = removed;
        return Task.CompletedTask;
    }

    public Task ApplyAsync(string deviceId)
    {
        Commands.Add($"apply {deviceId}");
        if (Unavailable)
        {
            throw new AgentUnavailableException("not connected");
        }
        if (License.State != LicenseState.Licensed)
        {
            throw new AgentRefusal("not_licensed", "not licensed");
        }
        Applied.Add(deviceId);
        License = License.Clone();
        var listed = License.Devices.FirstOrDefault(d => d.Id == deviceId);
        if (listed is null)
        {
            License.Devices.Add(listed = new LicensedDevice { Id = deviceId, Name = deviceId });
        }
        listed.Use = LicenseUse.Activating;
        return Task.CompletedTask;
    }

    /// <summary>The computers the service reports, as it answers a revoke with its status.</summary>
    public List<DeviceStatus> Devices { get; } = [];

    public Task<Status?> RevokeAsync(string deviceId, string activationId, bool deactivated)
    {
        Commands.Add($"revoke {deviceId}");
        if (Unavailable)
        {
            throw new AgentUnavailableException("not connected");
        }
        if (RefuseRevoke is { } code)
        {
            throw new AgentRefusal(code, "refused");
        }
        Revoked.Add((deviceId, activationId, deactivated));
        // Not on the license any more: the row turns to trial.
        License = License.Clone();
        foreach (var device in License.Devices.Where(d => d.Id == deviceId).ToList())
        {
            License.Devices.Remove(device);
        }
        var status = new Status { License = License };
        status.Devices.Add(Devices);
        return Task.FromResult<Status?>(status);
    }
}
