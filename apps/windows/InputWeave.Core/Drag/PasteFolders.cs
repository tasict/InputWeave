using System.Globalization;

namespace InputWeave.Drag;

/// <summary>
/// Where files copied on another computer are received when pasted here,
/// before File Explorer copies them where they were pasted
/// (docs/architecture.md §9): a folder of the app's for each paste.
/// </summary>
public static class PasteFolders
{
    /// <summary>The folder those folders are in.</summary>
    public static string Root(string localAppData) => Path.Combine(localAppData, "InputWeave", "clipboard");

    /// <summary>
    /// The folder the files of promise <paramref name="promiseId"/> are
    /// fetched into under <paramref name="root"/>: named for the promise and
    /// a random part, since the background service numbers its promises
    /// afresh each time it starts while an earlier folder may still wait
    /// to be removed.
    /// </summary>
    public static string For(string root, ulong promiseId)
    {
        var id = promiseId.ToString(CultureInfo.InvariantCulture);
        return Path.Combine(root, $"{id}-{Guid.NewGuid():N}"[..(id.Length + 9)]);
    }
}
