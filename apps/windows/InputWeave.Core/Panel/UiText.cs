namespace InputWeave.Panel;

/// <summary>
/// A string the UI shows, as a resource key and the values that go into
/// its placeholders ({0}, {1}…). The app looks the key up in its .resw
/// files; the decisions about which text to show stay testable without
/// them. A value is a string, or another <see cref="UiText"/> that the app
/// looks up first.
/// </summary>
public sealed class UiText(string key, params object[] args) : IEquatable<UiText>
{
    public string Key { get; } = key;
    public IReadOnlyList<object> Args { get; } = args;

    public bool Equals(UiText? other) =>
        other is not null && Key == other.Key && Args.SequenceEqual(other.Args);

    public override bool Equals(object? obj) => Equals(obj as UiText);

    public override int GetHashCode() => HashCode.Combine(Key, Args.Count);

    public override string ToString() => Args.Count == 0 ? Key : $"{Key}({string.Join(", ", Args)})";
}
