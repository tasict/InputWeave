using Inputweave.App.V1;

namespace InputWeave.Support;

/// <summary>
/// The background service's own hotkeys, as it writes them (<c>Hotkeys</c>
/// in app.proto). It handles them itself and reports those in effect
/// (<c>Status.hotkeys</c>); these are its defaults, which a service that
/// does not report them uses, and which Settings goes back to. How they
/// are shown is <see cref="Settings.ShortcutText"/>'s business.
/// </summary>
public static class Shortcut
{
    public const string DefaultTakeBack = "control+alt+shift+Escape";
    public const string DefaultLock = "control+alt+shift+KeyL";
    public const string DefaultJumpModifiers = "control+alt+shift";
    public const string DefaultRelative = "control+alt+shift+KeyG";

    /// <summary>The double press of Control that takes control back, as the service writes it while it is on (<c>Hotkeys.take_back_double</c>); off is empty.</summary>
    public const string DoubleControl = "double+control";

    /// <summary>The default hotkeys, the double press off; a new message each time, as messages can be changed.</summary>
    public static Hotkeys Defaults => new()
    {
        TakeBack = DefaultTakeBack,
        Lock = DefaultLock,
        JumpModifiers = DefaultJumpModifiers,
        Relative = DefaultRelative,
        TakeBackDouble = "",
    };
}
