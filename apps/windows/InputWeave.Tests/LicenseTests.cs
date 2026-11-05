using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Licensing;
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
            License = new License { Key = key, State = LicenseState.PendingActivation };
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
                License = new License
                {
                    Key = result.Key,
                    State = LicenseState.Licensed,
                    ActivationId = result.Activated.ActivationId,
                    CustomerEmail = result.Activated.CustomerEmail,
                    ValidatedAt = result.Activated.ValidatedAt,
                };
                break;
            case RecordLicenseResult.ResultOneofCase.Refused:
                License = new License
                {
                    Key = result.Key,
                    State = result.Refused == LicenseRefusal.SeatsFull ? LicenseState.SeatsFull : LicenseState.Invalid,
                };
                break;
        }
        return Task.CompletedTask;
    }

    public Task RemoveAsync()
    {
        if (Unavailable)
        {
            throw new AgentUnavailableException("not connected");
        }
        if (RefuseRemove is { } code)
        {
            throw new AgentRefusal(code, "refused");
        }
        Removed = true;
        License = new License { OptedOut = true };
        return Task.CompletedTask;
    }
}
