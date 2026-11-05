using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;

namespace InputWeave.Overlay;

/// <summary>What the HUD does about a drag: say something, held until taken down or for the usual time, or take down what it says.</summary>
public sealed record HudCue(UiText? Say, bool Hold = false, bool Hide = false)
{
    public static HudCue Show(UiText text, bool hold = false) => new(text, hold);

    public static readonly HudCue TakeDown = new(null, Hide: true);
}

/// <summary>
/// The HUD for files dragged between computers:
/// on the source, "Dragging 3 items to Mac" once the cursor takes the drag
/// to another computer, then whether it took the items; on the target,
/// "Drop the items where you want them" for as long as the drag offered
/// here goes on. Like <see cref="HudWatch"/> it compares each state with
/// the one before, and the first state seen is only remembered.
/// </summary>
public sealed class DragHud
{
    private bool primed;
    private string draggingTo = "";
    private ulong seenOffer;
    private ulong seenFinish;
    private string offered = "";

    public HudCue? Update(AgentSnapshot snapshot)
    {
        if (snapshot.Link != AgentLink.Connected)
        {
            return null;
        }
        var dragging = snapshot.Status.Dragging;
        var to = dragging is null || dragging.DeviceId.Length == 0 ? "" : $"{dragging.DragId}/{dragging.DeviceId}";
        bool first = !primed;
        bool newDevice = to != draggingTo;
        bool newOffer = (snapshot.DragOffer?.Serial ?? 0) != seenOffer;
        bool newFinish = (snapshot.DragFinish?.Serial ?? 0) != seenFinish;
        primed = true;
        draggingTo = to;
        seenOffer = snapshot.DragOffer?.Serial ?? 0;
        seenFinish = snapshot.DragFinish?.Serial ?? 0;
        if (newOffer && snapshot.DragOffer is { } offer)
        {
            offered = offer.Arrived.DragId;
        }
        if (first)
        {
            return null;
        }
        if (newFinish && snapshot.DragFinish is { } finish)
        {
            var ended = finish.Ended;
            var name = snapshot.NameOf(ended.DeviceId);
            switch (ended.Reason)
            {
                case DragEnd.Accepted when ended.DeviceId.Length != 0 && ended.DragId != offered:
                    return HudCue.Show(new UiText(TextKeys.HudDragSending, name));
                case DragEnd.Declined when ended.DeviceId.Length != 0 && ended.DragId != offered:
                    return HudCue.Show(new UiText(TextKeys.HudDragDeclined, name));
                case DragEnd.Withdrawn or DragEnd.Expired when ended.DragId == offered:
                    offered = "";
                    return HudCue.TakeDown;
            }
        }
        if (newOffer && snapshot.DragOffer is not null)
        {
            return HudCue.Show(new UiText(TextKeys.HudDropHere), hold: true);
        }
        if (newDevice && dragging is not null && dragging.DeviceId.Length != 0)
        {
            var name = snapshot.NameOf(dragging.DeviceId);
            return HudCue.Show(dragging.Items == 1
                ? new UiText(TextKeys.HudDraggingOne, name)
                : new UiText(TextKeys.HudDraggingMany, dragging.Items.ToString(System.Globalization.CultureInfo.CurrentCulture), name));
        }
        return null;
    }
}
