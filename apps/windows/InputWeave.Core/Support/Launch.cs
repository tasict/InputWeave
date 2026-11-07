using InputWeave.Main;
using InputWeave.Setup;

namespace InputWeave.Support;

/// <summary>
/// What the app opens on its own as it starts, from its arguments: first-run
/// setup until it has been finished, unless the app was started for
/// something else (<see cref="LaunchArguments.HasPurpose"/>), and the
/// windows the arguments for testing ask for. The pairing code window and
/// notifications are not decided here: they follow what the background
/// service reports, however the app was started.
/// </summary>
public sealed record Launch(
    bool ShowPanel,
    MainPage? Window,
    bool AddDevice,
    bool ShowPairingCode,
    SetupStep? Setup = null,
    string? Hud = null,
    bool Locator = false,
    string? Notification = null,
    double? LocatorAt = null,
    bool Background = false,
    bool Purpose = false,
    IReadOnlyList<string>? SimulateDragEnter = null,
    IReadOnlyList<string>? SimulateDragArrived = null,
    bool SampleTransfers = false,
    bool Identify = false,
    double? IdentifyFor = null,
    bool SampleDevices = false,
    IReadOnlyList<string>? SimulateClipboardFiles = null,
    double? SimulateClipboardArriving = null,
    double? SimulateClipboardRecopy = null,
    string? Glow = null,
    double? GlowFor = null,
    Uri? PolarServer = null,
    Uri? UpdateFeed = null,
    bool InstallUpdate = false,
    bool ShowAbout = false,
    string? SendPicker = null)
{
    /// <summary>
    /// Checks for updates at once and installs what it finds, as if
    /// Install were chosen in the update dialog; for testing the update
    /// path (with <see cref="UpdateFeedArgument"/>).
    /// </summary>
    public const string InstallUpdateArgument = "--install-update";

    /// <summary>Opens About InputWeave over the main window; for testing and screenshots.</summary>
    public const string ShowAboutArgument = "--show-about";

    /// <summary>
    /// Asks where two items go among made-up computers, two that can take
    /// them and two that can't, as Send To does; with <c>none</c>, says
    /// that no computer can take them instead; for testing and screenshots.
    /// </summary>
    public const string ShowSendPickerArgument = "--show-send-picker";

    /// <summary>Asks this server instead of Polar's API about the license (scripts/mock-polar.py); for testing.</summary>
    public const string PolarServerArgument = "--polar-server";

    /// <summary>Reads the update feed here instead of on the website, and takes installers from beside it; for testing.</summary>
    public const string UpdateFeedArgument = "--update-feed";

    /// <summary>
    /// Lights an edge of the main monitor (<c>left</c>, <c>right</c>,
    /// <c>top</c> or <c>bottom</c>) in the middle, as when the cursor
    /// crosses there, once the app has started, held for the seconds that
    /// follow when a number does; for testing.
    /// </summary>
    public const string ShowGlowArgument = "--show-glow";

    /// <summary>
    /// Shows this computer's name on each of its screens once the app has
    /// connected, as when the screens are identified, for the seconds that
    /// follow when a number does; for testing.
    /// </summary>
    public const string ShowIdentifyArgument = "--show-identify";

    /// <summary>
    /// Shows the computers as a background service that numbers them for
    /// the jump shortcuts would, with another name given to the first other
    /// computer here, and the relative mode hotkey and its switch from a
    /// service without them, instead of what the service reports; for
    /// screenshots of the canvas, the device list, the inspector and the
    /// General page.
    /// </summary>
    public const string ShowSampleDevicesArgument = "--show-sample-devices";

    /// <summary>
    /// Shows sample transfers in the panel and on the File Transfers page
    /// instead of the background service's: folders going, finished, and
    /// finished with failures; for screenshots.
    /// </summary>
    public const string ShowSampleTransfersArgument = "--show-sample-transfers";

    /// <summary>
    /// For testing the source side of dragging files to another computer
    /// without a mouse: a drag of the files that follow enters a drag strip
    /// once the app has connected, and leaves it a few seconds later, as
    /// the strip's drop target would see it.
    /// </summary>
    public const string SimulateDragEnterArgument = "--simulate-drag-enter";

    /// <summary>
    /// For testing the target side without another computer: the files and
    /// folders that follow are offered as if dragged here from another
    /// computer, a few seconds after the app has connected, and arrive by
    /// being copied into the staging folder once dropped.
    /// </summary>
    public const string SimulateDragArrivedArgument = "--simulate-drag-arrived";

    /// <summary>
    /// For testing pasting files without another computer: the files and
    /// folders that follow are put on the clipboard as if copied on another
    /// computer, once the app has connected, and arrive by being copied into
    /// the staging folder when pasted.
    /// </summary>
    public const string SimulateClipboardFilesArgument = "--simulate-clipboard-files";

    /// <summary>
    /// With <see cref="SimulateClipboardFilesArgument"/>: how many seconds
    /// the simulated files take to arrive once pasted (2 without it).
    /// </summary>
    public const string SimulateClipboardArrivingArgument = "--simulate-clipboard-arriving";

    /// <summary>
    /// With <see cref="SimulateClipboardFilesArgument"/>: the same files are
    /// put on the clipboard again, as a new promise, this many seconds after
    /// a paste first asks for them; for testing a paste that waits while
    /// something else is copied.
    /// </summary>
    public const string SimulateClipboardRecopyArgument = "--simulate-clipboard-recopy";

    /// <summary>Opens the panel when the app starts, and keeps it open; for testing.</summary>
    public const string ShowPanelArgument = "--show-panel";

    /// <summary>
    /// Opens the main window when the app starts, on a page when one is
    /// named (<c>--show-window=devices</c> or <c>--show-window devices</c>);
    /// for testing.
    /// </summary>
    public const string ShowWindowArgument = "--show-window";

    /// <summary>Opens Add Device over the main window when the app starts; for testing.</summary>
    public const string AddDeviceArgument = "--show-add-device";

    /// <summary>Shows a sample pairing code, as when another computer adds this one; for testing.</summary>
    public const string ShowPairingCodeArgument = "--show-pairing-code";

    /// <summary>Shows the app in another language than Windows' (<c>--language en-US</c>); for testing.</summary>
    public const string LanguageArgument = "--language";

    /// <summary>
    /// Opens first-run setup when the app starts, at a step when one is
    /// named (<c>--show-setup network</c>); for testing. <c>entercode</c>
    /// shows a sample code entry.
    /// </summary>
    public const string ShowSetupArgument = "--show-setup";

    /// <summary>
    /// Shows the text that follows in the HUD once the app has started, or
    /// one of the app's strings as <c>key:&lt;key&gt;:&lt;value&gt;…</c>; for testing.
    /// </summary>
    public const string ShowHudArgument = "--show-hud";

    /// <summary>
    /// Shows the cursor locator at the cursor once the app has started, held
    /// as it is the given seconds in when a number follows; for testing.
    /// </summary>
    public const string ShowLocatorArgument = "--show-locator";

    /// <summary>
    /// Posts a sample notification once the app has started:
    /// <c>received</c>, <c>folder</c>, <c>offer</c> or <c>service</c>; for testing, and
    /// to check the app is registered for notifications.
    /// </summary>
    public const string ShowNotificationArgument = "--show-notification";

    /// <summary>
    /// Started with an argument for testing, which takes the place of
    /// first-run setup and keeps the windows it opens above the others.
    /// </summary>
    public bool Testing => ShowPanel || Window is not null || ShowPairingCode || Setup is not null
        || Hud is not null || Locator || Notification is not null || SimulateDragEnter is not null || SimulateDragArrived is not null
        || SampleTransfers || Identify || SampleDevices || SimulateClipboardFiles is not null || Glow is not null
        || InstallUpdate || ShowAbout || SendPicker is not null;

    /// <summary>
    /// The setup step to open at launch: the one an argument names, or the
    /// first while setup has not been finished and the app was neither
    /// started for something else nor for testing. Null opens no setup.
    /// </summary>
    public SetupStep? SetupAt(bool finishedSetup) =>
        Setup ?? (finishedSetup || Testing || Purpose ? null : SetupStep.Welcome);

    /// <summary>The argument after <paramref name="name"/>, or what follows "=" in it.</summary>
    private static string? Value(IReadOnlyList<string> args, string name)
    {
        for (int i = 0; i < args.Count; i++)
        {
            if (args[i] == name)
            {
                return i + 1 < args.Count && !args[i + 1].StartsWith("--", StringComparison.Ordinal) ? args[i + 1] : "";
            }
            if (args[i].StartsWith(name + "=", StringComparison.Ordinal))
            {
                return args[i][(name.Length + 1)..];
            }
        }
        return null;
    }

    /// <summary>The arguments after <paramref name="name"/> up to the next option, or null when it is not there.</summary>
    private static IReadOnlyList<string>? Values(IReadOnlyList<string> args, string name)
    {
        for (int i = 0; i < args.Count; i++)
        {
            if (args[i] == name)
            {
                return args.Skip(i + 1).TakeWhile(a => !a.StartsWith("--", StringComparison.Ordinal)).Where(a => a.Length != 0).ToList();
            }
        }
        return null;
    }

    public static Launch Parse(IReadOnlyList<string> args)
    {
        MainPage? window = null;
        bool addDevice = args.Contains(AddDeviceArgument);
        SetupStep? setup = Value(args, ShowSetupArgument) is { } step
            ? Enum.TryParse<SetupStep>(step, ignoreCase: true, out var named) ? named : SetupStep.Welcome
            : null;
        for (int i = 0; i < args.Count; i++)
        {
            var arg = args[i];
            if (arg == ShowWindowArgument)
            {
                // The page may also come as the next argument, as batch files split at "=".
                window = i + 1 < args.Count && Enum.TryParse<MainPage>(args[i + 1], ignoreCase: true, out var next)
                    ? next
                    : MainPage.General;
            }
            else if (arg.StartsWith(ShowWindowArgument + "=", StringComparison.Ordinal)
                && Enum.TryParse<MainPage>(arg[(ShowWindowArgument.Length + 1)..], ignoreCase: true, out var page))
            {
                window = page;
            }
        }
        if (addDevice)
        {
            window ??= MainPage.Devices;
        }
        return new Launch(
            args.Contains(ShowPanelArgument), window, addDevice, args.Contains(ShowPairingCodeArgument),
            setup, Value(args, ShowHudArgument), args.Contains(ShowLocatorArgument), Value(args, ShowNotificationArgument),
            double.TryParse(Value(args, ShowLocatorArgument), System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var at) ? at : null,
            LaunchArguments.InBackground(args), LaunchArguments.HasPurpose(args),
            Values(args, SimulateDragEnterArgument), Values(args, SimulateDragArrivedArgument),
            args.Contains(ShowSampleTransfersArgument),
            args.Contains(ShowIdentifyArgument),
            double.TryParse(Value(args, ShowIdentifyArgument), System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var seconds) && seconds > 0 ? seconds : null,
            args.Contains(ShowSampleDevicesArgument),
            Values(args, SimulateClipboardFilesArgument),
            Seconds(Value(args, SimulateClipboardArrivingArgument)),
            Seconds(Value(args, SimulateClipboardRecopyArgument)),
            Values(args, ShowGlowArgument) is { } glow ? (glow.Count > 0 ? glow[0] : "right") : null,
            Values(args, ShowGlowArgument) is { Count: > 1 } held ? Seconds(held[1]) : null,
            Address(Value(args, PolarServerArgument)),
            Address(Value(args, UpdateFeedArgument)),
            args.Contains(InstallUpdateArgument),
            args.Contains(ShowAboutArgument),
            Value(args, ShowSendPickerArgument));
    }

    /// <summary>An http or https address, or null.</summary>
    private static Uri? Address(string? value) =>
        Uri.TryCreate(value, UriKind.Absolute, out var uri) && uri.Scheme is "http" or "https" ? uri : null;

    /// <summary>A number of seconds, not negative, or null.</summary>
    private static double? Seconds(string? value) =>
        double.TryParse(value, System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var seconds)
            && seconds >= 0
            ? seconds
            : null;
}
