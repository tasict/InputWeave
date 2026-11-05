using System.Globalization;
using System.Reflection;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Drag;
using InputWeave.Licensing;
using InputWeave.Main;
using InputWeave.Overlay;
using InputWeave.Pairing;
using InputWeave.Panel;
using InputWeave.Setup;
using InputWeave.Support;
using InputWeave.Tray;
using InputWeave.Updates;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;

namespace InputWeave;

/// <summary>
/// The notification area app: an icon, its panel and menu, the main
/// window, first-run setup, the pairing code shown when another computer
/// adds this one, the HUD and the cursor locator, and system
/// notifications, all showing what the background service reports and
/// sending it the user's commands. There is no window at launch once setup
/// has finished.
/// </summary>
public partial class App : Application
{
    private readonly string[] args;
    private DispatcherQueue dispatcher = null!;
    private ServiceWatcher service = null!;
    private TrayIcon tray = null!;
    private DispatcherQueueTimer ticker = null!;
    private PanelWindow? panel;
    private MainWindow? main;
    private QuitDialog? quitDialog;
    private PairingCodeWindow? codeWindow;
    private SetupWindow? setup;
    private HudWindow? hud;
    private LocatorWindow? locator;
    /// <summary>The names shown when the screens are identified, one window per monitor, reused.</summary>
    private readonly List<ScreenIdentityWindow> identities = [];
    /// <summary>The identification last shown.</summary>
    private ulong seenIdentification;
    /// <summary>The edges lit where the cursor crossed, one window for each edge of each monitor (by its name as listed), each fading on its own; reused.</summary>
    private readonly Dictionary<(string Monitor, ScreenEdge Edge), EdgeGlowWindow> glowWindows = new();
    private readonly HudWatch hudWatch = new();
    private readonly DragHud dragHud = new();
    private readonly Notifier notifier = new();
    /// <summary>The strips that take drags of files to other computers; null before launch.</summary>
    private SeamStrips? strips;
    /// <summary>Where the strips are, in pixels, also while they are down: the drag offered here cancels in them.</summary>
    private IReadOnlyList<PixelRect> stripRects = [];
    /// <summary>What the strips were last placed for; they are placed again only when it changes.</summary>
    private StripInputs? stripInputs;
    private AgentPromiseHost? promiseHost;
    /// <summary>Files copied on another computer, put on the clipboard here; null before launch.</summary>
    private ClipboardFiles? clipboardFiles;
    /// <summary>Files on the clipboard as if copied on another computer, for testing (<see cref="Launch.SimulateClipboardFilesArgument"/>).</summary>
    private ClipboardFiles? simulatedClipboardFiles;
    /// <summary>1 while connected to the background service, which holds what files on the clipboard promise.</summary>
    private int clipboardConnected;
    /// <summary>The drag offer last started here.</summary>
    private ulong seenOffer;
    /// <summary>The HUD says to drop the items, until the drag offered here ends.</summary>
    private bool holdingDropNote;
    /// <summary>Sample transfers stand in for the service's, for screenshots (<see cref="Launch.ShowSampleTransfersArgument"/>).</summary>
    private bool sampleTransfers;
    /// <summary>Sample numbers and names stand in for the service's, for screenshots (<see cref="Launch.ShowSampleDevicesArgument"/>).</summary>
    private bool sampleDevices;
    /// <summary>The cursor sighting the locator last showed.</summary>
    private ulong seenSighting;
    /// <summary>What waits for the background service, e.g. an answer given in a notification that started the app.</summary>
    private readonly List<Func<Task>> whenConnected = [];
    /// <summary>A sample code shown for screenshots (<see cref="Launch.ShowPairingCodeArgument"/>); the service's are not shown meanwhile.</summary>
    private PairingState? sampleCode;
    /// <summary>Whether first-run setup has opened since the app started; one started for something else (in the background, by Send To) has not shown it yet.</summary>
    private bool setupOpened;
    private int refreshQueued;
    private bool quitting;

    public App(string[] args)
    {
        this.args = args;
        InitializeComponent();
    }

    public static App Instance => (App)Current;

    /// <summary>"0.1.0", the same as the background service's version.</summary>
    public static string Version
    {
        get
        {
            var version = Assembly.GetExecutingAssembly().GetName().Version;
            return version is null ? "0" : $"{version.Major}.{version.Minor}.{version.Build}";
        }
    }

    internal AgentModel Model { get; private set; } = null!;

    /// <summary>Asks Polar what the service's license needs.</summary>
    internal LicenseWorker License { get; private set; } = null!;

    /// <summary>Checks for new versions and installs them.</summary>
    internal Updater Updater { get; private set; } = null!;

    /// <summary>
    /// Transfer problems the panel no longer lists: dismissed by the user,
    /// or already there when the app first connected. Kept across restarts.
    /// </summary>
    internal DismissedTransfers DismissedTransfers { get; } = new(Preferences.DismissedTransfers);

    /// <summary>The main window's handle, an owner for the pickers its pages open.</summary>
    internal nint MainHwnd => main?.Hwnd ?? 0;

    protected override void OnLaunched(LaunchActivatedEventArgs e)
    {
        // The app lives in the notification area: closing a window must not end it.
        DispatcherShutdownMode = DispatcherShutdownMode.OnExplicitShutdown;
        dispatcher = DispatcherQueue.GetForCurrentThread();
        AppLog.Open(AppLog.DefaultPath);
        var session = AgentPipe.CurrentSessionId;
        var launch = Launch.Parse(args);
        sampleTransfers = launch.SampleTransfers;
        sampleDevices = launch.SampleDevices;
        if (sampleTransfers)
        {
            TransferRow.Expand(SampleTransfers.Expanded);
        }
        AppLog.Info($"InputWeave {Version} started in session {session}{(launch.Background ? " in the background, by the background service" : "")}; background service at {AgentPipe.PathFor(session)}");
        // Folders left in staging from before this start are an earlier app's.
        var startedUtc = System.Diagnostics.Process.GetCurrentProcess().StartTime.ToUniversalTime();
        // Opened by the user, not by the broker: no longer quit.
        if (!launch.Background)
        {
            Preferences.QuitByUser = false;
        }
        UnhandledException += (_, error) => AppLog.Warn($"unhandled: {error.Exception}");

        // Before anything else, so a click that started the app is not lost.
        notifier.Start(click => dispatcher.TryEnqueue(() => Respond(click)));

        Model = AgentModel.ForThisSession($"windows-ui/{Version}", takesClipboardFiles: true);
        Model.Changed += (_, _) => QueueRefresh();
        var polar = new HttpPolarTransport(launch.PolarServer ?? HttpPolarTransport.DefaultServer, $"InputWeave/{Version}");
        if (launch.PolarServer is { } server)
        {
            AppLog.Info($"license: asking {server} instead of Polar");
        }
        License = new LicenseWorker(
            new PolarClient(polar, PolarClient.OrganizationId, () => DateTimeOffset.UtcNow),
            new AgentLicense(Model),
            new FileLicenseLedger(
                Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "InputWeave", "license-work.dat"),
                AppLog.Info),
            () => DateTimeOffset.UtcNow,
            () => Model.Snapshot.Status.ThisDeviceName is { Length: > 0 } name ? name : Environment.MachineName,
            AppLog.Info);
        Updates.Updater.ClearLeftovers();
        Updater = new Updater(
            launch.UpdateFeed,
            Assembly.GetExecutingAssembly().GetName().Version ?? new Version(0, 0, 0),
            feed => dispatcher.TryEnqueue(() => notifier.PostUpdate(feed)));
        // Nothing is pasted before the app runs: what is left in the staging folder is from an app that ended meanwhile.
        _ = Task.Run(() => ClipboardFiles.ClearStaging(ClipboardRoot, startedUtc));
        clipboardFiles = new ClipboardFiles(new AgentClipboardHost(Model), ClipboardRoot);
        Model.ClipboardFilesPromised += clipboardFiles.Promise;
        Model.ClipboardFilesArrived += clipboardFiles.Arrived;
        // Every crossing lights its edge, however close the next one comes.
        Model.CursorCrossed += crossed => dispatcher.TryEnqueue(() => LightEdge(crossed));
        Model.Changed += (_, _) =>
        {
            // The background service went: the files it promised cannot come.
            bool connected = Model.Snapshot.Link == AgentLink.Connected;
            if (Interlocked.Exchange(ref clipboardConnected, connected ? 1 : 0) == 1 && !connected)
            {
                clipboardFiles.Withdraw();
            }
        };
        strips = new SeamStrips(Model);
        promiseHost = new AgentPromiseHost(Model, () => Volatile.Read(ref stripRects), PromiseEndedOffThread);
        // No drag goes on before the app runs: what is left in the staging folder is from an app that ended mid-drag.
        _ = Task.Run(() => PromisedDrag.ClearStaging(StagingRoot, startedUtc));
        service = new ServiceWatcher();
        service.Changed += (_, _) => QueueRefresh();
        tray = new TrayIcon(OpenFromTray, Actions, Preferences.ShowTrayIcon);

        // The icon's dot appears once the service has been silent for 10
        // seconds, the code's countdown runs, the clipboard line expires and
        // the taskbar's theme can change; nothing else says when.
        ticker = dispatcher.CreateTimer();
        ticker.Interval = TimeSpan.FromSeconds(1);
        ticker.Tick += (_, _) => Refresh();
        ticker.Start();

        Model.Start();
        Refresh();

        SingleInstance.Listen(next => dispatcher.TryEnqueue(() => Opened(next)));
        SendTo.OfferOnFirstRun();
        if (launch.SetupAt(Preferences.FinishedSetup) is { } step)
        {
            // A step asked for by a test stays above the other windows.
            ShowSetup(step, pin: launch.Setup is not null);
        }
        if (Notifier.LaunchClick() is { } click)
        {
            AppLog.Info("started from a notification");
            Respond(click);
        }
        if (LaunchArguments.SendPaths(args) is { } paths)
        {
            SendTo.Send(paths);
        }
        ShowSamples(launch);
        if (launch.ShowPanel)
        {
            Panel.Show(null, pin: true);
        }
        if (launch.Window is { } page)
        {
            ShowMain(page, launch.AddDevice, pin: true);
        }
        if (launch.InstallUpdate)
        {
            InstallFoundUpdate();
        }
        if (launch.ShowAbout)
        {
            ShowMain(null, pin: true);
            dispatcher.TryEnqueue(DispatcherQueuePriority.Low, () => main?.ShowAbout());
        }
        if (launch.ShowPairingCode)
        {
            sampleCode = new PairingState
            {
                Phase = PairingPhase.ShowCode,
                PeerName = "MacBook Pro",
                Code = "382914",
                ExpiresAtUnixMs = (ulong)DateTimeOffset.UtcNow.AddMinutes(2).ToUnixTimeMilliseconds(),
            };
            Refresh();
        }
    }

    private MenuActions Actions => new(
        TogglePause,
        ToggleLock,
        IdentifyScreens,
        () => ShowMain(MainPage.Devices, addDevice: true),
        () => ShowSetup(SetupStep.Welcome),
        () => ShowMain(null),
        () => _ = CheckForUpdatesAsync(),
        ShowAbout,
        AskToQuit);

    /// <summary>The HUD, the locator, a notification and drags of files, shown or simulated on request, for testing.</summary>
    private async void ShowSamples(Launch launch)
    {
        if (launch.Hud is null && !launch.Locator && launch.Notification is null
            && launch.SimulateDragEnter is null && launch.SimulateDragArrived is null && !launch.Identify
            && launch.SimulateClipboardFiles is null && launch.Glow is null)
        {
            return;
        }
        // Once connected, so names are known; the HUD and the locator after
        // anything else at launch has settled.
        for (int i = 0; i < 50 && Model.Snapshot.Link != AgentLink.Connected; i++)
        {
            await Task.Delay(100);
        }
        await Task.Delay(1000);
        if (launch.Hud is { } text)
        {
            Hud.Say(SampleHudText(text), TimeSpan.FromSeconds(10));
        }
        if (launch.SimulateDragEnter is { Count: > 0 } entering)
        {
            strips?.Simulate(entering, TimeSpan.FromSeconds(4));
        }
        if (launch.SimulateDragArrived is { Count: > 0 } arriving)
        {
            // Time for a test to press the button, as the service does as the cursor comes here.
            await Task.Delay(2000);
            var local = new LocalPromiseHost(arriving, PromiseEndedOffThread);
            var offer = local.Offer(Model.Snapshot.Status.Devices.FirstOrDefault()?.Id ?? "test");
            AppLog.Info($"simulating drag {offer.DragId} arriving with {offer.Items.Count} items, {offer.Files} files");
            Hud.Say(Strings.Get(TextKeys.HudDropHere), TimeSpan.FromMinutes(10));
            holdingDropNote = true;
            PromisedDrag.Start(offer, local, StagingRoot);
        }
        if (launch.SimulateClipboardFiles is { Count: > 0 } copied)
        {
            var local = new LocalClipboardHost(
                copied,
                TimeSpan.FromSeconds(launch.SimulateClipboardArriving ?? 2),
                launch.SimulateClipboardRecopy is { } recopy ? TimeSpan.FromSeconds(recopy) : null);
            simulatedClipboardFiles = new ClipboardFiles(local, ClipboardRoot);
            local.Files = simulatedClipboardFiles;
            AppLog.Info($"simulating {copied.Count} items copied on another computer, on the clipboard");
            simulatedClipboardFiles.Promise(local.Promise(1));
        }
        if (launch.Locator)
        {
            AppLog.Info("showing a sample of the cursor locator");
            Locator.Show(launch.LocatorAt);
        }
        if (launch.Notification is { } kind)
        {
            notifier.PostSample(kind, Model.Snapshot);
        }
        if (launch.Identify)
        {
            AppLog.Info("showing a sample of the identified screens");
            IdentifyHere(launch.IdentifyFor is { } seconds ? TimeSpan.FromSeconds(seconds) : ScreenIdentity.ShownFor(0));
        }
        if (launch.Glow is { } edgeName
            && Native.Monitors().OrderByDescending(m => m.Primary).FirstOrDefault() is { } main)
        {
            var edge = edgeName.ToLowerInvariant() switch
            {
                "left" => ScreenEdge.Left,
                "top" => ScreenEdge.Top,
                "bottom" => ScreenEdge.Bottom,
                _ => ScreenEdge.Right,
            };
            var scale = main.Scale > 0 ? main.Scale : 1;
            var width = (main.Right - main.Left) / scale;
            var height = (main.Bottom - main.Top) / scale;
            AppLog.Info($"showing a sample of the edge glow, on the {edge} edge of {main.Id}");
            LightEdge(
                new CursorCrossed
                {
                    DisplayId = main.Id,
                    Edge = edge,
                    Along = edge is ScreenEdge.Left or ScreenEdge.Right ? height / 2 : width / 2,
                    DisplayWidth = width,
                    DisplayHeight = height,
                    Arriving = true,
                },
                launch.GlowFor is { } held ? TimeSpan.FromSeconds(held) : null);
        }
    }

    /// <summary>
    /// The text of a sample HUD: as given, the take-back hint when empty, or
    /// a string of the app's as <c>key:HudDraggingMany:3:Mac mini</c>, its
    /// values after the key.
    /// </summary>
    private string SampleHudText(string text)
    {
        if (text.Length == 0)
        {
            return Strings.Text(new UiText(TextKeys.HudTakeBackHint, Model.Snapshot.TakeBackShortcut));
        }
        if (text.StartsWith("key:", StringComparison.Ordinal))
        {
            var parts = text[4..].Split(':');
            return Strings.Format(parts[0], parts.Skip(1).Cast<object>().ToArray());
        }
        return text;
    }

    /// <summary>Where files pasted from the clipboard are received before File Explorer copies them where they were pasted.</summary>
    private static string ClipboardRoot =>
        PasteFolders.Root(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData));

    /// <summary>Where drags from other computers are received before File Explorer copies them where they were dropped.</summary>
    private static string StagingRoot =>
        DragStaging.Root(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData));

    /// <summary>
    /// The strips along the edges a drag of files can leave by, up while
    /// files can leave from here; and where they are, also while they are
    /// down, for the drag offered here.
    /// </summary>
    private void UpdateStrips(AgentSnapshot snapshot)
    {
        if (strips is null)
        {
            return;
        }
        // Not again for every transfer's progress or round-trip time.
        var inputs = StripInputs.Of(snapshot);
        if (inputs.Equals(stripInputs))
        {
            return;
        }
        stripInputs = inputs;
        IReadOnlyList<PixelRect> rects = snapshot.Link == AgentLink.Connected
            ? DragStrips.ToPixels(
                DragStrips.For(snapshot.Arrangement, DragStrips.Enterable(snapshot)),
                DragStrips.ThisComputersScreens(snapshot.Arrangement),
                Native.Monitors())
            : [];
        Volatile.Write(ref stripRects, rects);
        strips.Show(DragStrips.Shown(snapshot) ? rects : []);
    }

    /// <summary>Another computer drags files here: a drag of them starts where the cursor is.</summary>
    private void StartPromises(AgentSnapshot snapshot)
    {
        if (snapshot.DragOffer is not { } offer || offer.Serial == seenOffer || promiseHost is null)
        {
            return;
        }
        seenOffer = offer.Serial;
        PromisedDrag.Start(offer.Arrived, promiseHost, StagingRoot);
    }

    private void PromiseEndedOffThread(string dragId, PromiseOutcome outcome, string? code) =>
        dispatcher.TryEnqueue(() => PromiseEnded(dragId, outcome, code));

    /// <summary>The drag offered here is over: the HUD stops asking for the drop, or says why it was refused.</summary>
    private void PromiseEnded(string dragId, PromiseOutcome outcome, string? code)
    {
        AppLog.Info($"drag {dragId}: {outcome}");
        if (outcome == PromiseOutcome.Refused)
        {
            holdingDropNote = false;
            Hud.Say(Strings.Text(DragStaging.AcceptRefused(code ?? "")));
        }
        else if (holdingDropNote)
        {
            holdingDropNote = false;
            hud?.HideNow();
        }
    }

    private HudWindow Hud => hud ??= new HudWindow();

    private LocatorWindow Locator => locator ??= new LocatorWindow();

    /// <summary>Opens first-run setup, or brings it forward when it is open.</summary>
    internal void ShowSetup(SetupStep step, bool pin = false)
    {
        if (setup is not null)
        {
            setup.Activate();
            return;
        }
        panel?.Hide();
        setupOpened = true;
        setup = new SetupWindow(Model, step);
        setup.Dismissed += (_, _) => setup = null;
        setup.Update(Model.Snapshot);
        setup.Open(pin);
    }

    /// <summary>The panel, opened above the end of the taskbar, e.g. after sending from File Explorer.</summary>
    internal void ShowPanel() => Panel.Show(null, pin: false);

    /// <summary>
    /// Another launch of the app: files to send from File Explorer, the
    /// panel (or setup, not shown yet) to open, or nothing when the
    /// background service started it again.
    /// </summary>
    private void Opened(IReadOnlyList<string> launched)
    {
        if (!LaunchArguments.InBackground(launched))
        {
            Preferences.QuitByUser = false;
        }
        var relaunch = LaunchArguments.ForRunningApp(launched, setupDue: !setupOpened && !Preferences.FinishedSetup);
        AppLog.Info($"opened again with {launched.Count} arguments: {relaunch}");
        switch (relaunch)
        {
            case Relaunch.Send:
                SendTo.Send(LaunchArguments.SendPaths(launched)!);
                break;
            case Relaunch.OpenSetup:
                if (!tray.Shown)
                {
                    SetTrayIconShown(true);
                }
                ShowSetup(SetupStep.Welcome);
                break;
            case Relaunch.OpenPanel:
                Reopen();
                break;
        }
    }

    /// <summary>
    /// What the user did with a notification: answered a file offer,
    /// opened the folder of received files, started the service, or
    /// clicked it, which opens the window on the page it is about.
    /// </summary>
    private void Respond(NotificationClick click)
    {
        AppLog.Info($"notification clicked: {click.Action ?? $"open {click.Page}"}");
        switch (click.Action)
        {
            case Notifier.Accept or Notifier.Decline when click.Value(Notifier.TransferKey) is { } transfer:
                bool accept = click.Action == Notifier.Accept;
                WhenConnected(() => Model.AnswerFileOfferAsync(transfer, accept));
                break;
            case Notifier.OpenFolder when click.Value(Notifier.PathKey) is { } path:
                Shell.ShowInExplorer(path);
                break;
            case Notifier.StartService:
                ServiceWatcher.Start();
                break;
            case Notifier.ViewUpdate:
                ShowUpdate();
                break;
            case Notifier.Later:
                break;
            default:
                if (!tray.Shown)
                {
                    SetTrayIconShown(true);
                }
                ShowMain(click.Page);
                break;
        }
    }

    /// <summary>Runs a command once the background service is reachable, at once if it is.</summary>
    private void WhenConnected(Func<Task> command)
    {
        whenConnected.Add(command);
        Refresh();
    }

    /// <summary>The app is about to restart the service itself; its stopping is not news.</summary>
    internal void ServiceRestarting() => notifier.ServiceRestarting();

    /// <summary>
    /// Takes back what the app put in place for the current user: its
    /// notifications, the Send To shortcut, opening at login, and its
    /// settings. The uninstaller runs the app for this (<see cref="LaunchArguments.Unregister"/>).
    /// </summary>
    internal static void Unregister()
    {
        Notifier.UnregisterAll();
        SendTo.Set(false);
        Preferences.SetOpensAtLogin(false);
        Preferences.Forget();
    }

    private PanelWindow Panel
    {
        get
        {
            if (panel is null)
            {
                panel = new PanelWindow(Actions, SetSharing);
                Refresh();
            }
            return panel;
        }
    }

    private void OpenFromTray() => Panel.Toggle(Native.CursorPosition());

    /// <summary>
    /// The app was opened again: a hidden icon comes back, and the panel
    /// opens, at the end of the taskbar.
    /// </summary>
    private void Reopen()
    {
        if (!tray.Shown)
        {
            SetTrayIconShown(true);
        }
        Panel.Show(null, pin: false);
    }

    /// <summary>General › "Show the icon in the system tray".</summary>
    internal void SetTrayIconShown(bool shown)
    {
        Preferences.ShowTrayIcon = shown;
        tray.Shown = shown;
        Refresh();
    }

    /// <summary>Opens the main window, on <paramref name="page"/> when given, with Add Device over it when asked.</summary>
    internal void ShowMain(MainPage? page, bool addDevice = false, bool pin = false)
    {
        main ??= new MainWindow();
        main.Update(Model.Snapshot, service.State, DateTimeOffset.UtcNow);
        panel?.Hide();
        main.Open(page, addDevice, pin);
    }

    /// <summary>About InputWeave, over the main window.</summary>
    private void ShowAbout()
    {
        ShowMain(null);
        dispatcher.TryEnqueue(DispatcherQueuePriority.Low, () => main?.ShowAbout());
    }

    /// <summary>The update dialog for the version the last check found, over General.</summary>
    internal async void ShowUpdate(bool install = false)
    {
        if (Updater.Found is null)
        {
            // A notification from before the app started again: check first.
            ShowMain(MainPage.General, pin: install);
            await Updater.CheckAsync(manual: true);
        }
        if (Updater.Found is not { } feed || Updater.Verdict is not (UpdateVerdict.Available or UpdateVerdict.Skipped))
        {
            // Nothing to install after all: General › Updates says what the check found.
            ShowMain(MainPage.General);
            return;
        }
        ShowMain(MainPage.General, pin: install);
        dispatcher.TryEnqueue(DispatcherQueuePriority.Low, () => main?.ShowUpdate(Updater, feed, install));
    }

    /// <summary><see cref="Launch.InstallUpdateArgument"/>: checks now and installs what it finds.</summary>
    private async void InstallFoundUpdate()
    {
        await Updater.CheckAsync(manual: true);
        if (Updater.Found is not null && Updater.Verdict is UpdateVerdict.Available or UpdateVerdict.Skipped)
        {
            ShowUpdate(install: true);
        }
    }

    /// <summary>Check Now and Check for Updates…: reads the feed, and offers what it finds.</summary>
    internal async Task CheckForUpdatesAsync()
    {
        await Updater.CheckAsync(manual: true);
        if (Updater.Found is not null && Updater.Verdict is UpdateVerdict.Available or UpdateVerdict.Skipped)
        {
            ShowUpdate();
        }
        else
        {
            ShowMain(MainPage.General);
        }
    }

    /// <summary>
    /// Whatever the license work needs now; never in the way of anything
    /// else. Runs while the service is out of reach too: freeing a seat with
    /// Polar needs no service.
    /// </summary>
    private async void TickLicense()
    {
        try
        {
            await License.TickAsync(main?.ShowsLicense == true);
        }
        catch (Exception e)
        {
            AppLog.Info($"license: {e.Message}");
        }
    }

    /// <summary>Coalesces change notifications from background threads into one refresh.</summary>
    private void QueueRefresh()
    {
        if (Interlocked.Exchange(ref refreshQueued, 1) == 0)
        {
            dispatcher.TryEnqueue(Refresh);
        }
    }

    /// <summary>Takes a transfer problem off the panel, for good; the history keeps it.</summary>
    internal void DismissTransfer(string transferId)
    {
        if (DismissedTransfers.Dismiss(transferId))
        {
            Preferences.DismissedTransfers = DismissedTransfers.Ids;
        }
        RefreshNow();
    }

    /// <summary>Shows the latest at once, after a change only the app knows about.</summary>
    internal void RefreshNow()
    {
        panel?.Invalidate();
        Refresh();
    }

    private void Refresh()
    {
        Interlocked.Exchange(ref refreshQueued, 0);
        if (quitting)
        {
            return;
        }
        var snapshot = Model.Snapshot;
        var now = DateTimeOffset.UtcNow;
        var serviceState = service.State;
        // Before the panel, so problems from before the app connected never show.
        if (DismissedTransfers.Update(snapshot))
        {
            Preferences.DismissedTransfers = DismissedTransfers.Ids;
        }
        var menu = MenuState.For(snapshot);
        tray.Update(TrayIcons.StateFor(snapshot, serviceState, now), TaskbarTheme.IsLight, menu);
        var shown = sampleTransfers && snapshot.Link == AgentLink.Connected ? SampleTransfers.Over(snapshot) : snapshot;
        if (sampleDevices && snapshot.Link == AgentLink.Connected)
        {
            shown = SampleDevices.Over(shown);
        }
        panel?.Update(shown, serviceState, now, menu);
        if (main?.IsShown == true)
        {
            main.Update(shown, serviceState, now);
        }
        setup?.Update(snapshot);
        ShowPairingCode(snapshot, now);
        UpdateStrips(snapshot);
        ShowOverlays(snapshot);
        StartPromises(snapshot);
        notifier.Update(snapshot, serviceState, now, () => ShowMain(MainPage.Files));
        TickLicense();
        Updater.Tick();
        if (snapshot.Link == AgentLink.Connected && whenConnected.Count != 0)
        {
            var due = whenConnected.ToList();
            whenConnected.Clear();
            foreach (var command in due)
            {
                Send("a command from a notification", command);
            }
        }
    }

    /// <summary>The HUD when there is news (<see cref="HudWatch"/>), and the locator for a new cursor sighting.</summary>
    private void ShowOverlays(AgentSnapshot snapshot)
    {
        int hints = Preferences.TakeBackHints;
        int shown = hints;
        if (hudWatch.Update(snapshot, ref hints) is { } note)
        {
            if (hints != shown)
            {
                Preferences.TakeBackHints = hints;
            }
            Hud.Say(Strings.Text(note));
            holdingDropNote = false;
        }
        // After the notes above, so where a drag goes wins over the take-back hint.
        if (dragHud.Update(snapshot) is { } cue)
        {
            if (cue.Say is { } say)
            {
                Hud.Say(Strings.Text(say), cue.Hold ? TimeSpan.FromMinutes(10) : null);
                holdingDropNote = cue.Hold;
            }
            else if (cue.Hide && holdingDropNote)
            {
                holdingDropNote = false;
                hud?.HideNow();
            }
        }
        // Where the cursor is now, rather than where the service saw it:
        // the copy that grows follows the cursor as it moves on.
        if (snapshot.CursorSighting is { } sighting && sighting.Serial != seenSighting)
        {
            seenSighting = sighting.Serial;
            Locator.Show();
        }
        if (snapshot.ScreenIdentification is { } identification && identification.Serial != seenIdentification)
        {
            seenIdentification = identification.Serial;
            IdentifyHere(ScreenIdentity.ShownFor(identification.DurationMs));
        }
    }

    /// <summary>
    /// Lights up the edge the cursor crossed to or from another computer,
    /// unless the user turned it off, or Windows
    /// says the user is playing a full-screen game or presenting. Each edge
    /// of each monitor fades on its own; crossing the same edge again at
    /// once keeps it on.
    /// </summary>
    private void LightEdge(CursorCrossed crossed, TimeSpan? held = null)
    {
        if (!Preferences.ShowEdgeGlow || !EdgeGlow.Known(crossed.Edge)
            || EdgeGlow.HoldsBack(Native.NotificationState()))
        {
            return;
        }
        // Listed anew each time, which is cheap and crossings are few: a
        // resolution or scale changed, or a monitor plugged in again, is
        // where it is now.
        if (EdgeGlow.MonitorFor(crossed, Native.Monitors()) is not { } monitor)
        {
            return;
        }
        bool solid = Brand.HighContrastOn;
        var frame = EdgeGlow.Strip(monitor, crossed.Edge, solid ? EdgeGlow.SolidDepth : EdgeGlow.Depth);
        var timing = EdgeGlow.For(OverlayWindow.ReduceMotion);
        if (held is { } hold)
        {
            timing = timing with { Hold = hold };
        }
        var key = (monitor.Id, crossed.Edge);
        if (!glowWindows.TryGetValue(key, out var window))
        {
            window = new EdgeGlowWindow();
            glowWindows[key] = window;
        }
        window.Show(monitor, frame, crossed.Edge, solid, EdgeGlow.Fraction(crossed), timing);
    }

    /// <summary>
    /// This computer's name on each of its monitors, with the screen's
    /// number and size: the service said to
    /// identify the screens. Shown again, the new names replace these.
    /// </summary>
    private void IdentifyHere(TimeSpan shownFor)
    {
        var snapshot = sampleDevices ? SampleDevices.Over(Model.Snapshot) : Model.Snapshot;
        var labels = ScreenIdentity.Labels(ScreenIdentity.ThisComputersScreens(snapshot.Arrangement), Native.Monitors());
        var jump = ScreenIdentity.Jump(snapshot);
        AppLog.Info($"identifying {labels.Count} screens for {shownFor.TotalSeconds:0.#} s");
        while (identities.Count < labels.Count)
        {
            identities.Add(new ScreenIdentityWindow());
        }
        for (int i = 0; i < identities.Count; i++)
        {
            if (i >= labels.Count)
            {
                identities[i].Dismiss();
                continue;
            }
            var label = labels[i];
            var monitor = label.Monitor;
            identities[i].Show(
                snapshot.Status.ThisDeviceName,
                Strings.Format(TextKeys.IdentifyScreenLabel, label.Number.ToString(CultureInfo.CurrentCulture), label.Size),
                jump,
                new PixelRect(monitor.Left, monitor.Top, monitor.Right - monitor.Left, monitor.Bottom - monitor.Top),
                monitor.Scale,
                shownFor,
                announce: i == 0);
        }
    }

    /// <summary>
    /// The code for another computer to type, while the service says so;
    /// gone as soon as the phase moves on.
    /// </summary>
    private void ShowPairingCode(AgentSnapshot snapshot, DateTimeOffset now)
    {
        var pairing = sampleCode ?? (snapshot.Link == AgentLink.Connected ? snapshot.Pairing : null);
        if (pairing is not null && PairingCode.ShowsCode(pairing))
        {
            codeWindow ??= new PairingCodeWindow(DeclinePairing);
            codeWindow.Show(pairing, now);
        }
        else if (codeWindow is not null)
        {
            codeWindow.Dismiss();
            codeWindow = null;
        }
    }

    private void DeclinePairing()
    {
        if (sampleCode is not null)
        {
            sampleCode = null;
            Refresh();
            return;
        }
        Send("decline pairing", Model.CancelPairingAsync);
    }

    private void SetSharing(bool on) => Send($"set sharing {on}", () => Model.SetSharingAsync(on));

    private void TogglePause()
    {
        var on = !Model.Snapshot.Status.Sharing;
        Send($"set sharing {on}", () => Model.SetSharingAsync(on));
    }

    /// <summary>Has this computer and every connected one show their names on their screens.</summary>
    internal void IdentifyScreens() => Send("identify screens", Model.IdentifyScreensAsync);

    private void ToggleLock()
    {
        var locked = !Model.Snapshot.Status.Locked;
        Send($"set locked {locked}", () => Model.SetLockedAsync(locked));
    }

    /// <summary>Sends a command; a refusal shows in the state the service reports.</summary>
    internal async void Send(string what, Func<Task> command)
    {
        try
        {
            await command();
        }
        catch (Exception e)
        {
            AppLog.Info($"{what}: {e.Message}");
            // A switch may have moved without the service agreeing.
            panel?.Invalidate();
            main?.Invalidate();
            Refresh();
        }
    }

    private void AskToQuit()
    {
        if (quitDialog is not null)
        {
            quitDialog.Activate();
            return;
        }
        quitDialog = new QuitDialog(Quit);
        quitDialog.Closed += (_, _) => quitDialog = null;
        quitDialog.Open();
    }

    private async void Quit()
    {
        if (quitting)
        {
            return;
        }
        // From here on nothing is shown: closing the connection below still
        // reports a change, which must not reach the disposed icon.
        quitting = true;
        AppLog.Info("quitting");
        ticker.Stop();
        SingleInstance.Stop();
        codeWindow?.Dismiss();
        setup?.Close();
        hud?.HideNow();
        locator?.Dismiss();
        foreach (var glow in glowWindows.Values)
        {
            glow.Dismiss();
        }
        foreach (var identity in identities)
        {
            identity.Dismiss();
        }
        // The user chose to quit: the background service does not start the
        // app again for files on the clipboard until it connects again, and
        // the broker, after the background service restarts too, until the
        // user opens it.
        Preferences.QuitByUser = true;
        try
        {
            await Model.UserQuitAppAsync().WaitAsync(TimeSpan.FromSeconds(1));
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException or TimeoutException)
        {
            AppLog.Info($"cannot tell the background service the user quit: {e.Message}");
        }
        // Off the clipboard before going: nothing would be there to hand the files over.
        clipboardFiles?.Close();
        simulatedClipboardFiles?.Close();
        await Model.DisposeAsync();
        service.Dispose();
        tray.Dispose();
        Exit();
    }
}
