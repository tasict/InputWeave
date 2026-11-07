using System.Collections.Immutable;
using Inputweave.App.V1;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;

namespace InputWeave.Agent;

/// <summary>How far the app is with the background service.</summary>
public enum AgentLink
{
    /// <summary>Trying to reach the background service.</summary>
    Connecting,
    Connected,
    /// <summary>The service speaks another IPC version, usually mid-update.</summary>
    Incompatible,
}

/// <summary>
/// Where the background service said the cursor is, after the user shook
/// the mouse with the cursor on this computer, in
/// its desktop coordinates. Each one counts anew, even at the same place.
/// </summary>
public sealed record CursorSighting(ulong Serial, double X, double Y)
{
    /// <summary>The sighting that comes after <paramref name="previous"/>.</summary>
    public static CursorSighting After(CursorSighting? previous, double x, double y) =>
        new((previous?.Serial ?? 0) + 1, x, y);
}

/// <summary>
/// The background service said to show this computer's name on each of its
/// screens for <paramref name="DurationMs"/>: its
/// user, or another computer's, asked to identify the screens. Each one
/// counts anew.
/// </summary>
public sealed record ScreenIdentification(ulong Serial, uint DurationMs)
{
    /// <summary>The identification that comes after <paramref name="previous"/>.</summary>
    public static ScreenIdentification After(ScreenIdentification? previous, uint durationMs) =>
        new((previous?.Serial ?? 0) + 1, durationMs);
}

/// <summary>
/// The relative mode hotkey was pressed with the cursor on a computer whose
/// InputWeave takes no relative movements; the
/// key went there as any other. Each time counts anew.
/// </summary>
public sealed record RelativeRefusal(ulong Serial, string DeviceId)
{
    /// <summary>The refusal that comes after <paramref name="previous"/>.</summary>
    public static RelativeRefusal After(RelativeRefusal? previous, string deviceId) => new((previous?.Serial ?? 0) + 1, deviceId);
}

/// <summary>
/// Another computer dragging files here (Event.drag_arrived): the cursor
/// came here with the button held, and the app starts a drag of the items
/// where it is. Each one counts anew.
/// </summary>
public sealed record DragOffer(ulong Serial, DragArrived Arrived)
{
    public static DragOffer After(DragOffer? previous, DragArrived arrived) => new((previous?.Serial ?? 0) + 1, arrived);
}

/// <summary>
/// How the last drag ended (Event.drag_ended): on the source, the one that
/// began here; on the target, one offered here. Each one counts anew.
/// </summary>
public sealed record DragFinish(ulong Serial, DragEnded Ended)
{
    public static DragFinish After(DragFinish? previous, DragEnded ended) => new((previous?.Serial ?? 0) + 1, ended);
}

/// <summary>
/// What the background service last reported, as one immutable value, and
/// what follows from it. The UI reads a snapshot and shows it; decisions
/// stay in the service.
/// </summary>
public sealed record AgentSnapshot
{
    /// <summary>
    /// Out of reach for longer than this, the service is not responding
    /// rather than starting.
    /// </summary>
    public static readonly TimeSpan UnresponsiveAfter = TimeSpan.FromSeconds(10);

    public AgentLink Link { get; init; } = AgentLink.Connecting;

    /// <summary>
    /// Since when the service has been out of reach, to tell a restart
    /// apart from a service that stopped responding. Null while connected.
    /// </summary>
    public DateTimeOffset? UnreachableSince { get; init; }

    public Status Status { get; init; } = new();
    public PairingState Pairing { get; init; } = new();
    public IReadOnlyList<NearbyDevice> Nearby { get; init; } = [];
    public Arrangement Arrangement { get; init; } = new();
    public Transfers Transfers { get; init; } = new();

    /// <summary>The version the service reported when it connected.</summary>
    public string AgentVersion { get; init; } = "";

    /// <summary>
    /// The SHA-256 of the service's executable, in lowercase hex, as it
    /// reported it; empty when it did not. Tells builds of one version apart.
    /// </summary>
    public string AgentDigest { get; init; } = "";

    /// <summary>The last time the service showed where the cursor is; null before any.</summary>
    public CursorSighting? CursorSighting { get; init; }

    /// <summary>The last time the service said to identify the screens; null before any.</summary>
    public ScreenIdentification? ScreenIdentification { get; init; }

    /// <summary>The last time another computer dragged files here; null before any.</summary>
    public DragOffer? DragOffer { get; init; }

    /// <summary>The last drag that ended, here or offered here; null before any.</summary>
    public DragFinish? DragFinish { get; init; }

    /// <summary>The last time the relative mode hotkey reached a computer without relative mode; null until it first does.</summary>
    public RelativeRefusal? RelativeRefusal { get; init; }

    /// <summary>The last non-zero time the cursor had to rest at an edge; 0 before any.</summary>
    public uint LastSwitchDelayMs { get; init; }

    /// <summary>
    /// The computers this app asked the service to wake (WakeDevice) and
    /// has no answer for yet; a computer that leaves the status leaves this
    /// too. A new set whenever it changes, so a change is a new snapshot.
    /// </summary>
    public ImmutableHashSet<string> WakesSent { get; init; } = ImmutableHashSet<string>.Empty;

    /// <summary>
    /// Counts the connections to the service, from 1 for the first: each
    /// is a new session, where what the app told the service before (the
    /// Wi-Fi name) is told again.
    /// </summary>
    public ulong Session { get; init; }

    public string ThisDeviceId => Status.ThisDeviceId;

    /// <summary>
    /// Whether the service has been out of reach for longer than a restart
    /// takes, as of <paramref name="now"/>. While it updates it is not
    /// counted.
    /// </summary>
    public bool IsUnresponsive(DateTimeOffset now) =>
        Link == AgentLink.Connecting
        && UnreachableSince is { } since
        && now - since > UnresponsiveAfter;

    public DeviceStatus? Device(string id) => Status.Devices.FirstOrDefault(d => d.Id == id);

    public string NameOf(string id)
    {
        if (id == Status.ThisDeviceId)
        {
            return Status.ThisDeviceName;
        }
        return Device(id)?.Name
            ?? Arrangement.Devices.FirstOrDefault(d => d.Id == id)?.Name
            ?? id;
    }

    /// <summary>
    /// The device the cursor is on: the active one, or this one when the
    /// service has not said.
    /// </summary>
    public string CursorDeviceId =>
        Status.ActiveDeviceId.Length == 0 ? Status.ThisDeviceId : Status.ActiveDeviceId;

    /// <summary>Whether this device is controlling another one right now.</summary>
    public bool ControllingOther =>
        Status.ActiveDeviceId.Length != 0 && Status.ActiveDeviceId != Status.ThisDeviceId;

    public bool ControlledFromElsewhere => Status.ControlledByDeviceId.Length != 0;

    /// <summary>
    /// Whether the cursor can be sent to a computer: not the one it is on,
    /// and not while it could not go anywhere.
    /// </summary>
    public bool CanJump(string id) =>
        Link == AgentLink.Connected && Status.Sharing && !Status.Locked
        && id != CursorDeviceId
        && (id == ThisDeviceId || Device(id)?.State == ConnectionState.Connected)
        && !OutOfReach(id);

    /// <summary>
    /// Whether input from here cannot reach a computer: its screen is
    /// locked where it cannot, it takes none now (a Linux desktop that has
    /// not allowed remote control), or its user paused sharing there. The
    /// service keeps the cursor off it.
    /// </summary>
    public bool OutOfReach(string id) =>
        Device(id) is { ScreenLocked: true, LockScreenInput: false } or { InputUnavailable: true } or { SharingPaused: true };

    /// <summary>
    /// The time to rest at an edge when the user turns waiting on: the last
    /// one in effect, else the service's default.
    /// </summary>
    public uint SwitchDelayMs => LastSwitchDelayMs != 0 ? LastSwitchDelayMs : Status.DefaultDwellMs;

    /// <summary>A computer in the arrangement, if it is there.</summary>
    public ArrangedDevice? Arranged(string id) => Arrangement.Devices.FirstOrDefault(d => d.Id == id);

    public IReadOnlyList<Permission> MissingPermissions =>
        Status.Permissions.Where(p => !p.Granted).Select(p => p.Permission).ToList();

    public IReadOnlyList<DeviceStatus> OfflineDevices =>
        Status.Devices.Where(d => d.State != ConnectionState.Connected).ToList();

    /// <summary>The computers connected now.</summary>
    public IReadOnlyList<DeviceStatus> ConnectedDevices =>
        Status.Devices.Where(d => d.State == ConnectionState.Connected).ToList();

    /// <summary>
    /// Computers files can be sent to right now: those without a reason
    /// they can't (<see cref="SendTargets.Unavailable"/>: connected, and
    /// files turned off neither with them here, <c>SetDeviceSharing</c>, nor
    /// in the profile in use).
    /// </summary>
    public IReadOnlyList<DeviceStatus> FileReceivers =>
        Status.Devices.Where(d => SendTargets.Unavailable(d, Profiles.FilesOff(this)) is null).ToList();

    /// <summary>Whether files can be sent to a computer right now.</summary>
    public bool CanReceiveFiles(string id) => FileReceivers.Any(d => d.Id == id);

    /// <summary>Whether files are turned off with a computer here.</summary>
    public bool FilesOff(string id) => Device(id)?.FilesOff == true;

    /// <summary>
    /// The name a computer gives itself, when this computer's user gave it
    /// another one here (<c>RenameDevice</c>); null otherwise.
    /// </summary>
    public string? ReportedName(string id) =>
        Device(id) is { } device && device.ReportedName.Length != 0 && device.ReportedName != device.Name
            ? device.ReportedName
            : null;

    /// <summary>The hotkeys in effect: as the service reports them, or the defaults while it does not.</summary>
    public Hotkeys Hotkeys => Status.Hotkeys ?? Shortcut.Defaults;

    /// <summary>Takes the keyboard and mouse back to this computer, such as "Ctrl + Alt + Shift + Esc".</summary>
    public string TakeBackShortcut => ShortcutText.Text(Hotkeys.TakeBack);

    /// <summary>Whether the service knows the double press of Control that takes control back: it reports it, empty when off.</summary>
    public bool DoublePressSupported => Status.Hotkeys?.HasTakeBackDouble == true;

    /// <summary>Whether pressing Control twice takes control back too.</summary>
    public bool DoublePressOn => DoublePressSupported && Hotkeys.TakeBackDouble.Length != 0;

    /// <summary>
    /// Whether what tells how to take control back names the double press:
    /// it is on, and relative mouse is not, as the service ignores it then
    /// (games often use Ctrl).
    /// </summary>
    private bool DoublePressHinted => DoublePressOn && Status.RelativeMouse.Length == 0;

    /// <summary>
    /// How to take control back from the computer this one controls, as the
    /// panel says it: the shortcut, and the double press while it works.
    /// Every other place that tells it says this while it works.
    /// </summary>
    public UiText TakeBackHint =>
        new(DoublePressHinted ? TextKeys.SubtitleControllingDouble : TextKeys.SubtitleControlling, TakeBackShortcut);

    /// <summary>How to take control back, as a notice says it: at any time; while the double press works, as the panel does.</summary>
    public UiText TakeBackReminder =>
        DoublePressHinted ? TakeBackHint : new(TextKeys.HudTakeBackHint, TakeBackShortcut);

    /// <summary>What setup says once controlling another computer worked.</summary>
    public UiText TakeBackWorked =>
        DoublePressHinted ? new(TextKeys.SetupTryDoneHint, TakeBackHint) : new(TextKeys.SetupTryDone, TakeBackShortcut);

    /// <summary>Locks the cursor to the computer it is on, or unlocks it, such as "Ctrl + Alt + Shift + L".</summary>
    public string LockShortcut => ShortcutText.Text(Hotkeys.Lock);

    /// <summary>
    /// The jump to the computer numbered <paramref name="slot"/>, such as
    /// "Ctrl+Alt+Shift+2"; null without a number, or with the jumps off.
    /// </summary>
    public string? JumpShortcut(uint slot, bool compact = true) => ShortcutText.Jump(Hotkeys.JumpModifiers, slot, compact);

    /// <summary>
    /// Whether the service can wake other computers (Status.wake_supported);
    /// one from before says nothing, and the app then shows nothing of
    /// waking.
    /// </summary>
    public bool WakeSupported => Status.WakeSupported;

    /// <summary>
    /// Whether the service knows computers taken offline for now
    /// (Status.offline_supported); one from before says nothing, and the app
    /// then shows nothing of it.
    /// </summary>
    public bool OfflineSupported => Status.OfflineSupported;

    /// <summary>
    /// Whether the service knows profiles (Status.profiles_supported); one
    /// from before says nothing, and the app then shows nothing of them.
    /// </summary>
    public bool ProfilesSupported => Status.ProfilesSupported;

    /// <summary>
    /// Whether the service knows relative mode,
    /// as it says (Status.relative_supported); one from before says nothing.
    /// </summary>
    public bool RelativeSupported => Status.RelativeSupported;

    /// <summary>Turns relative mode on or off, such as "Ctrl + Alt + Shift + G"; null when it is off or the service has none.</summary>
    public string? RelativeShortcut =>
        RelativeSupported && Hotkeys.Relative.Length != 0 ? ShortcutText.Text(Hotkeys.Relative) : null;
}
