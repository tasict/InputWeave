using System.Globalization;
using InputWeave.Panel;
using Microsoft.Windows.ApplicationModel.Resources;

namespace InputWeave.Support;

/// <summary>
/// The app's strings, from Strings/&lt;language&gt;/Resources.resw. English
/// is the source; the language follows Windows' display language.
/// </summary>
internal static class Strings
{
    private static readonly ResourceLoader Loader = new();

    public static string Get(string key)
    {
        try
        {
            var value = Loader.GetString(key);
            if (!string.IsNullOrEmpty(value))
            {
                return value;
            }
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or ArgumentException)
        {
        }
        AppLog.Warn($"no string for {key}");
        return key;
    }

    /// <summary>The text with its values put in; a value that is a text itself is looked up first.</summary>
    public static string Text(UiText text) =>
        text.Args.Count == 0
            ? Get(text.Key)
            : string.Format(
                CultureInfo.CurrentCulture,
                Get(text.Key),
                text.Args.Select(a => a is UiText inner ? Text(inner) : a).ToArray());

    /// <summary>A text with values, by key.</summary>
    public static string Format(string key, params object[] args) => Text(new UiText(key, args));
}
