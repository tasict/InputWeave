using System.Globalization;
using InputWeave.Agent;

namespace InputWeave.Panel;

/// <summary>
/// The panel's one quiet line about the clipboard, shown for ten minutes
/// after it last moved or was held back, as on
/// the Mac.
/// </summary>
public static class ClipboardText
{
    public static readonly TimeSpan ShownFor = TimeSpan.FromMinutes(10);

    public static UiText? Line(AgentSnapshot model, DateTimeOffset now, CultureInfo culture)
    {
        var status = model.Status;
        if (model.Link != AgentLink.Connected || !status.ClipboardSync || status.LastClipboard is not { } activity)
        {
            return null;
        }
        var at = DateTimeOffset.FromUnixTimeMilliseconds((long)Math.Min(activity.AtUnixMs, long.MaxValue));
        if (now - at >= ShownFor)
        {
            return null;
        }
        switch (activity.Skipped)
        {
            case "sensitive":
                return new(TextKeys.ClipboardSensitive);
            case "too_large":
                return new(TextKeys.ClipboardTooLarge);
            case "":
                break;
            default:
                return null;
        }
        var when = Ago(now - at, culture);
        var kind = Kind(activity.Kinds);
        return activity.ToDeviceId == model.ThisDeviceId
            ? new(TextKeys.ClipboardSyncedFrom, model.NameOf(activity.FromDeviceId), when, kind)
            : new(TextKeys.ClipboardSentTo, model.NameOf(activity.ToDeviceId), when, kind);
    }

    /// <summary>"just now", or "3 min ago".</summary>
    public static UiText Ago(TimeSpan elapsed, CultureInfo culture)
    {
        var minutes = (int)Math.Floor(elapsed.TotalMinutes);
        return minutes < 1
            ? new(TextKeys.RelativeJustNow)
            : new(TextKeys.RelativeMinutesAgo, minutes.ToString(culture));
    }

    public static UiText Kind(IEnumerable<string> kinds)
    {
        var all = kinds.ToList();
        if (all.Contains("image"))
        {
            return new(TextKeys.ClipboardKindImage);
        }
        return all.Contains("rich_text") ? new(TextKeys.ClipboardKindRichText) : new(TextKeys.ClipboardKindText);
    }
}
