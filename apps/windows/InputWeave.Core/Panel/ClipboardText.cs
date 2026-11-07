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
        var when = Ago(now - at, culture, brief: true);
        var kind = Kind(activity.Kinds);
        return activity.ToDeviceId == model.ThisDeviceId
            ? new(TextKeys.ClipboardSyncedFrom, model.NameOf(activity.FromDeviceId), when, kind)
            : new(TextKeys.ClipboardSentTo, model.NameOf(activity.ToDeviceId), when, kind);
    }

    /// <summary>
    /// How long ago, in whole units rounded down: "just now" under a
    /// minute, then "1 minute ago" or "3 minutes ago" under an hour, hours
    /// under a day, and days. <paramref name="brief"/> says minutes as "3
    /// min ago", as the clipboard line does; it never gets to hours.
    /// </summary>
    public static UiText Ago(TimeSpan elapsed, CultureInfo culture, bool brief = false)
    {
        var minutes = (long)Math.Floor(Math.Max(elapsed.TotalMinutes, 0));
        if (minutes < 1)
        {
            return new(TextKeys.RelativeJustNow);
        }
        if (minutes < 60)
        {
            return brief
                ? new(TextKeys.RelativeMinutesAgo, minutes.ToString(culture))
                : Counted(minutes, TextKeys.AgoMinute, TextKeys.AgoMinutes, culture);
        }
        var hours = minutes / 60;
        return hours < 24
            ? Counted(hours, TextKeys.AgoHour, TextKeys.AgoHours, culture)
            : Counted(hours / 24, TextKeys.AgoDay, TextKeys.AgoDays, culture);
    }

    /// <summary>The words for one of a unit, or for <paramref name="count"/> of them.</summary>
    private static UiText Counted(long count, string one, string many, CultureInfo culture) =>
        count == 1 ? new(one) : new(many, count.ToString(culture));

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
