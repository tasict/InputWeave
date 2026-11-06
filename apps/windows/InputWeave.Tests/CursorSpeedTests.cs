using Inputweave.App.V1;
using InputWeave.Settings;
using Xunit;

namespace InputWeave.Tests;

/// <summary>
/// The inspector's cursor speed row: where it is offered, what it shows, and
/// the speed under the hand giving way to the service's.
/// </summary>
public class CursorSpeedTests
{
    private static DeviceStatus Device(uint speed, bool paired = true) =>
        new() { Id = "mac", Paired = paired, SpeedPercent = speed, State = ConnectionState.Connected };

    [Fact]
    public void OfferedForAnotherComputerWhoseServiceKnowsSpeeds()
    {
        Assert.Equal<int?>(150, CursorSpeed.Shown(Device(150), thisDevice: false));
        // Offline computers keep theirs: it is this computer's own choice.
        var offline = Device(200);
        offline.State = ConnectionState.Offline;
        Assert.Equal<int?>(200, CursorSpeed.Shown(offline, thisDevice: false));
        Assert.Null(CursorSpeed.Shown(Device(100), thisDevice: true));
        // One from the configuration file too.
        Assert.Equal<int?>(120, CursorSpeed.Shown(Device(120, paired: false), thisDevice: false));
        // A service from before cursor speeds says 0.
        Assert.Null(CursorSpeed.Shown(Device(0), thisDevice: false));
        Assert.Null(CursorSpeed.Shown(null, thisDevice: false));
    }

    [Fact]
    public void SnapsToStepsInRangeAndSaysThePercent()
    {
        Assert.Equal(150, CursorSpeed.Snapped(154));
        Assert.Equal(160, CursorSpeed.Snapped(155));
        Assert.Equal(50, CursorSpeed.Snapped(12));
        Assert.Equal(300, CursorSpeed.Snapped(999));
        Assert.Equal("150%", CursorSpeed.Label(150));
        Assert.Equal("50%", CursorSpeed.Label(50));
        Assert.True(CursorSpeed.ShowsReset(150));
        Assert.False(CursorSpeed.ShowsReset(100));
        Assert.Equal(new[] { 0, 0.2, 0.6, 1 }, CursorSpeed.Ticks.Select(CursorSpeed.Fraction));
    }

    [Fact]
    public void ADragSendsWhereItIsLetGoAndThenShowsTheService()
    {
        var draft = new CursorSpeedDraft();
        Assert.Equal(100, draft.Shown(100));
        draft.Began();
        draft.Moved(131, 100);
        Assert.Equal(130, draft.Shown(100));
        // Nothing goes while the slider is held.
        Assert.Null(draft.Pending(100));
        draft.Moved(152, 100);
        Assert.Equal<int?>(150, draft.Ended(100));
        var sent = draft.Sending(150);
        Assert.Equal(150, draft.Shown(100));
        draft.Answered(sent);
        Assert.Null(draft.Value);
        Assert.Equal(150, draft.Shown(150));
    }

    [Fact]
    public void ARefusedSpeedGoesBackToTheService()
    {
        var draft = new CursorSpeedDraft();
        draft.Began();
        draft.Moved(250, 100);
        Assert.Equal<int?>(250, draft.Ended(100));
        var sent = draft.Sending(250);
        draft.Answered(sent);
        Assert.Equal(100, draft.Shown(100));
    }

    [Fact]
    public void LettingGoWhereItStartedSendsNothing()
    {
        var draft = new CursorSpeedDraft();
        draft.Began();
        draft.Moved(180, 150);
        draft.Moved(149, 150);
        Assert.Null(draft.Ended(150));
        Assert.Equal(200, draft.Shown(200));
    }

    [Fact]
    public void TheKeyboardAndResetSendWithoutTheSliderHeld()
    {
        var draft = new CursorSpeedDraft();
        draft.Moved(110, 100);
        Assert.Equal<int?>(110, draft.Pending(100));
        draft.Moved(100, 100);
        Assert.Null(draft.Pending(100));
        Assert.Null(draft.Value);
        // Reset, from 150.
        draft.Moved(CursorSpeed.Normal, 150);
        Assert.Equal<int?>(100, draft.Pending(150));
        // An answer while a newer choice waits leaves that choice.
        var sent = draft.Sending(130);
        draft.Answered(sent);
        Assert.Equal(100, draft.Shown(150));
        draft.Settle(100);
        Assert.Null(draft.Value);
    }

    [Fact]
    public void TheServicesSpeedChosenAgainWhileAnotherIsOnItsWayStillGoes()
    {
        var draft = new CursorSpeedDraft();
        draft.Moved(110, 100);
        Assert.Equal<int?>(110, draft.Pending(100));
        var first = draft.Sending(110);
        // On its way, it is what shows.
        Assert.Equal(110, draft.Shown(100));
        // Back to 100 before the service answered: that is a change too.
        draft.Moved(100, 100);
        Assert.Equal<int?>(100, draft.Pending(100));
        var second = draft.Sending(100);
        // The answer to the first leaves the second on its way.
        draft.Answered(first);
        Assert.Equal(100, draft.Shown(110));
        draft.Answered(second);
        Assert.Equal(100, draft.Shown(100));
        Assert.Null(draft.Value);
        // Choosing what is on its way again is no change.
        draft.Moved(130, 100);
        draft.Sending(130);
        draft.Moved(130, 100);
        Assert.Null(draft.Pending(100));
        // Held, the same goes on letting go.
        draft.Began();
        draft.Moved(100, 100);
        Assert.Equal<int?>(100, draft.Ended(100));
    }

    [Fact]
    public void AComputerNoLongerTrustedLosesItsDraft()
    {
        var drafts = new CursorSpeedDrafts();
        var pc = drafts.For("pc");
        var sent = pc.Sending(150);
        drafts.For("mini").Moved(120, 100);
        drafts.Keep(["mini"]);
        Assert.False(drafts.Has("pc"));
        Assert.True(drafts.Has("mini"));
        // Paired again: the service's speed, not what was on its way.
        Assert.Equal(100, drafts.For("pc").Shown(100));
        // The answer to the old send changes nothing for the new draft.
        pc.Answered(sent);
        Assert.Equal(100, drafts.For("pc").Shown(100));
    }

    [Fact]
    public void OnlyTheAnswerToTheLatestSendCounts()
    {
        var draft = new CursorSpeedDraft();
        var sends = new[] { 110, 120, 110 }.Select(draft.Sending).ToArray();
        // The answers to the first two change nothing: 110 stays.
        draft.Answered(sends[0]);
        Assert.Equal(110, draft.Shown(100));
        draft.Answered(sends[1]);
        Assert.Equal(110, draft.Shown(100));
        // The latest one's: the service's speed shows, 110 if it took it,
        // 100 if it refused.
        draft.Answered(sends[2]);
        Assert.Equal(110, draft.Shown(110));
        Assert.Equal(100, draft.Shown(100));
    }
}
