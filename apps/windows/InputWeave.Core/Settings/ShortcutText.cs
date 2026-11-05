using Inputweave.App.V1;
using InputWeave.Generated;
using InputWeave.Panel;
using InputWeave.Support;

namespace InputWeave.Settings;

/// <summary>A hotkey, or the jumps together, as General › Shortcuts lists them.</summary>
public enum ShortcutRow
{
    TakeBack,
    Lock,
    Jump,
    Relative,
}

/// <summary>The modifier keys held as a key goes down, left or right alike.</summary>
public readonly record struct HeldModifiers(bool Control, bool Alt, bool Shift, bool Windows)
{
    public bool Any => Control || Alt || Shift || Windows;

    /// <summary>As the background service writes them, in its order: <c>control+alt+shift</c>.</summary>
    public string Names => string.Join(
        "+",
        new[] { (Control, "control"), (Alt, "alt"), (Shift, "shift"), (Windows, "gui") }.Where(m => m.Item1).Select(m => m.Item2));
}

/// <summary>What a key pressed while a shortcut is being recorded does.</summary>
public enum RecordStep
{
    /// <summary>Nothing yet: a modifier on its own, or a key without the modifiers the jumps need.</summary>
    Wait,
    /// <summary>Esc on its own: recording stops, the shortcut stays.</summary>
    Cancel,
    /// <summary>Recording stops and the shortcut becomes <see cref="Recorded.Value"/>.</summary>
    Set,
    /// <summary>A key without a name: recording stops and says it cannot be used.</summary>
    Unusable,
}

public readonly record struct Recorded(RecordStep Step, string Value = "");

/// <summary>
/// How hotkeys look in the app: spelled out in words on Windows,
/// from the text the background service writes
/// them as, such as <c>control+alt+shift+KeyL</c> (<c>Hotkeys</c> in
/// app.proto); and what a key pressed while recording one makes. Only for
/// showing and recording them; the service decides which ones it takes.
/// </summary>
public static class ShortcutText
{
    /// <summary>The modifiers as the service names them, in its order, with their words.</summary>
    private static readonly (string Name, string Word)[] Modifiers =
        [("control", "Ctrl"), ("alt", "Alt"), ("shift", "Shift"), ("gui", "Win")];

    /// <summary>The keys that are modifiers themselves, by their UI Events code names.</summary>
    private static readonly HashSet<string> ModifierKeys =
    [
        "ControlLeft", "ControlRight", "AltLeft", "AltRight", "ShiftLeft", "ShiftRight", "MetaLeft", "MetaRight",
    ];

    /// <summary><c>control+alt+shift+KeyL</c> as "Ctrl + Alt + Shift + L", as Settings, the HUD and the panel write it.</summary>
    public static string Text(string hotkey) => Join(hotkey, " + ");

    /// <summary><c>control+alt+shift+Digit2</c> as "Ctrl+Alt+Shift+2", beside a computer's name where room is short.</summary>
    public static string Compact(string hotkey) => Join(hotkey, "+");

    private static string Join(string hotkey, string separator)
    {
        var parts = hotkey.Split('+', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        if (parts.Length == 0)
        {
            return "";
        }
        return string.Join(separator, ModifierWords(parts[..^1]).Append(Key(parts[^1])));
    }

    /// <summary>The words of the modifiers among <paramref name="names"/>, in the service's order.</summary>
    private static IEnumerable<string> ModifierWords(IEnumerable<string> names)
    {
        var held = names.Select(n => n.ToLowerInvariant()).ToHashSet();
        return Modifiers.Where(m => held.Contains(m.Name)).Select(m => m.Word);
    }

    /// <summary>
    /// A key's UI Events <c>code</c> name as it is shown: the letter or
    /// digit, Esc for Escape; other names as they are, such as F5, Space or
    /// ArrowLeft.
    /// </summary>
    public static string Key(string name)
    {
        if (name.Length == 4 && name.StartsWith("Key", StringComparison.Ordinal))
        {
            return name[3..];
        }
        if (name.Length == 6 && name.StartsWith("Digit", StringComparison.Ordinal))
        {
            return name[5..];
        }
        return name == "Escape" ? "Esc" : name;
    }

    /// <summary>
    /// The jump to the computer numbered <paramref name="slot"/>, such as
    /// "Ctrl+Alt+Shift+2", or spaced as Settings writes shortcuts; null
    /// without a number or with the jumps off.
    /// </summary>
    public static string? Jump(string modifiers, uint slot, bool compact = true)
    {
        var words = ModifierWords(modifiers.Split('+', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)).ToList();
        if (words.Count == 0 || slot is < 1 or > 9)
        {
            return null;
        }
        return string.Join(compact ? "+" : " + ", words.Append(slot.ToString(System.Globalization.CultureInfo.InvariantCulture)));
    }

    /// <summary>All the jumps together, for Settings: "Ctrl + Alt + Shift + 1…9"; empty with the jumps off.</summary>
    public static string JumpRange(string modifiers)
    {
        var words = ModifierWords(modifiers.Split('+', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)).ToList();
        return words.Count == 0 ? "" : string.Join(" + ", words.Append("1…9"));
    }

    /// <summary>The name of the key with a scan code, as Windows reports it with a key press; null for one without a name.</summary>
    public static string? KeyName(uint scanCode, bool extended) => KeyCodes.DomCode(scanCode, extended);

    /// <summary>Whether a key, by its name, is a modifier itself.</summary>
    public static bool IsModifierKey(string key) => ModifierKeys.Contains(key);

    /// <summary>
    /// What a key pressed while <paramref name="row"/> is recorded does,
    /// by its name (null for a key without one) and the modifiers held, as
    /// the macOS app's recorder: Esc on its own cancels, Delete or
    /// Backspace on its own goes back to the row's default, the jumps take
    /// the modifiers held with any key, and the others take the modifiers
    /// and the key. Windows reports the modifier keys going down too; those
    /// wait for the key.
    /// </summary>
    public static Recorded Record(ShortcutRow row, string? key, HeldModifiers held)
    {
        if (key is not null && IsModifierKey(key))
        {
            return new(RecordStep.Wait);
        }
        if (!held.Any && key == "Escape")
        {
            return new(RecordStep.Cancel);
        }
        if (!held.Any && key is "Delete" or "Backspace")
        {
            return new(RecordStep.Set, Get(Shortcut.Defaults, row));
        }
        if (row == ShortcutRow.Jump)
        {
            // The digits need modifiers to go with; keep listening.
            return held.Any ? new(RecordStep.Set, held.Names) : new(RecordStep.Wait);
        }
        if (key is null)
        {
            return new(RecordStep.Unusable);
        }
        return new(RecordStep.Set, held.Any ? $"{held.Names}+{key}" : key);
    }

    /// <summary>The keys of a row: the hotkey, or the jumps' modifiers; empty for a row turned off.</summary>
    public static string Get(Hotkeys hotkeys, ShortcutRow row) => row switch
    {
        ShortcutRow.TakeBack => hotkeys.TakeBack,
        ShortcutRow.Lock => hotkeys.Lock,
        ShortcutRow.Relative => hotkeys.Relative,
        _ => hotkeys.JumpModifiers,
    };

    /// <summary>Whether a row can be turned off, with a switch beside it.</summary>
    public static bool CanTurnOff(ShortcutRow row) => row is ShortcutRow.Jump or ShortcutRow.Relative;

    /// <summary>
    /// Whether <paramref name="hotkeys"/> are the defaults, so Reset
    /// Shortcuts has nothing to do. A service that knows no relative mode
    /// hotkey (<paramref name="relative"/> false) is not asked about it.
    /// </summary>
    public static bool IsDefault(Hotkeys hotkeys, bool relative)
    {
        var shown = hotkeys.Clone();
        var defaults = Shortcut.Defaults;
        if (!relative)
        {
            shown.ClearRelative();
            defaults.ClearRelative();
        }
        return shown.Equals(defaults);
    }

    /// <summary>A copy of <paramref name="hotkeys"/> with <paramref name="row"/> set to <paramref name="value"/>.</summary>
    public static Hotkeys With(Hotkeys hotkeys, ShortcutRow row, string value)
    {
        var changed = hotkeys.Clone();
        switch (row)
        {
            case ShortcutRow.TakeBack:
                changed.TakeBack = value;
                break;
            case ShortcutRow.Lock:
                changed.Lock = value;
                break;
            case ShortcutRow.Relative:
                changed.Relative = value;
                break;
            default:
                changed.JumpModifiers = value;
                break;
        }
        return changed;
    }

    /// <summary>A row's title.</summary>
    public static string Title(ShortcutRow row) => row switch
    {
        ShortcutRow.TakeBack => TextKeys.ShortcutTakeBack,
        ShortcutRow.Lock => TextKeys.ShortcutLock,
        ShortcutRow.Relative => TextKeys.ShortcutRelative,
        _ => TextKeys.ShortcutJump,
    };

    /// <summary>What a row shows: the hotkey, or the jumps together, or Off.</summary>
    public static UiText Shown(Hotkeys hotkeys, ShortcutRow row) => row switch
    {
        ShortcutRow.Jump when hotkeys.JumpModifiers.Length == 0 => new(TextKeys.ShortcutOff),
        ShortcutRow.Jump => new(TextKeys.Verbatim, JumpRange(hotkeys.JumpModifiers)),
        ShortcutRow.Relative when hotkeys.Relative.Length == 0 => new(TextKeys.ShortcutOff),
        _ => new(TextKeys.Verbatim, Text(Get(hotkeys, row))),
    };

    /// <summary>What a row says while it records.</summary>
    public static string Prompt(ShortcutRow row) =>
        row == ShortcutRow.Jump ? TextKeys.ShortcutPressJump : TextKeys.ShortcutPressKeys;

    /// <summary>
    /// What to tell the user about the background service refusing to
    /// change <paramref name="row"/>, making <paramref name="before"/> into
    /// <paramref name="hotkeys"/>. For a combination used already, which
    /// shortcut uses it.
    /// </summary>
    public static UiText Refusal(string code, ShortcutRow? row, Hotkeys hotkeys, Hotkeys before)
    {
        switch (code)
        {
            case "needs_modifier":
                return new(TextKeys.ShortcutNeedsModifier);
            case "unknown_key":
                return new(TextKeys.ShortcutUnknownKey);
            case "duplicate" or "duplicate_relative":
                if (row is { } changed && User(Get(hotkeys, changed), changed, before) is { } other)
                {
                    return new(TextKeys.ShortcutUsedFor, new UiText(Title(other)));
                }
                // The service says the relative mode hotkey is one of the two.
                if (code == "duplicate_relative" && row != ShortcutRow.Relative)
                {
                    return new(TextKeys.ShortcutUsedFor, new UiText(TextKeys.ShortcutRelative));
                }
                return new(TextKeys.ShortcutUsedElsewhere);
            default:
                return new(TextKeys.ShortcutNotChanged);
        }
    }

    /// <summary>The other row of <paramref name="hotkeys"/> that <paramref name="keys"/>, new for <paramref name="row"/>, clash with.</summary>
    private static ShortcutRow? User(string keys, ShortcutRow row, Hotkeys hotkeys)
    {
        static IEnumerable<string> Jumps(string modifiers) =>
            modifiers.Length == 0 ? [] : Enumerable.Range(1, 9).Select(n => $"{modifiers}+Digit{n}");
        ShortcutRow[] singles = [ShortcutRow.TakeBack, ShortcutRow.Lock, ShortcutRow.Relative];
        if (row == ShortcutRow.Jump)
        {
            var jumps = Jumps(keys).ToList();
            return singles.Cast<ShortcutRow?>().FirstOrDefault(single => jumps.Any(j => Clash(j, Get(hotkeys, single!.Value))));
        }
        foreach (var other in singles.Where(s => s != row))
        {
            if (Clash(keys, Get(hotkeys, other)))
            {
                return other;
            }
        }
        return Jumps(hotkeys.JumpModifiers).Any(j => Clash(j, keys)) ? ShortcutRow.Jump : null;
    }

    /// <summary>
    /// Whether two hotkeys clash as the background service sees it
    /// (Hotkey::shadows): the same key, and one's modifiers among the
    /// other's, since modifiers held beyond a hotkey's own do not stop it.
    /// </summary>
    public static bool Clash(string a, string b)
    {
        static (HashSet<string> Modifiers, string Key)? Parts(string keys)
        {
            var names = keys.ToLowerInvariant().Split('+', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
            return names.Length == 0 ? null : (names[..^1].ToHashSet(), names[^1]);
        }
        if (Parts(a) is not { } first || Parts(b) is not { } second || first.Key != second.Key)
        {
            return false;
        }
        return first.Modifiers.IsSubsetOf(second.Modifiers) || second.Modifiers.IsSubsetOf(first.Modifiers);
    }

    /// <summary>The choices for switching only while a modifier is held, in the order they are offered, with their words.</summary>
    public static IReadOnlyList<(SwitchModifier Modifier, string Key)> SwitchChoices { get; } =
    [
        (SwitchModifier.None, TextKeys.SwitchAlways),
        (SwitchModifier.Control, TextKeys.SwitchHoldingControl),
        (SwitchModifier.Alt, TextKeys.SwitchHoldingAlt),
        (SwitchModifier.Shift, TextKeys.SwitchHoldingShift),
        (SwitchModifier.Gui, TextKeys.SwitchHoldingWindows),
    ];
}
