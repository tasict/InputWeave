namespace InputWeave.Drag;

/// <summary>
/// What a staging folder holds that an app before this one left, removed as
/// the app starts: a folder made before it started. One made since is a
/// paste's or a drop's of this run, which may already be filling it, and
/// goes when that ends.
/// </summary>
public static class LeftoverFolders
{
    public static IEnumerable<string> FromBefore(IEnumerable<(string Folder, DateTime CreatedUtc)> folders, DateTime startedUtc) =>
        folders.Where(f => f.CreatedUtc < startedUtc).Select(f => f.Folder);
}
