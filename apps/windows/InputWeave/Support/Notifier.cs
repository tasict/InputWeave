using System.Globalization;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Main;
using InputWeave.Notifications;
using InputWeave.Panel;
using Microsoft.Windows.AppLifecycle;
using Microsoft.Windows.AppNotifications;
using Microsoft.Windows.AppNotifications.Builder;
using Microsoft.Win32;

namespace InputWeave.Support;

/// <summary>What the user did with one of the app's notifications.</summary>
/// <param name="Action">The button, or null when the notification itself was clicked.</param>
/// <param name="Page">The page the notification is about, opened when it is clicked.</param>
/// <param name="Values">Everything the notification carries.</param>
internal sealed record NotificationClick(string? Action, MainPage? Page, IReadOnlyDictionary<string, string> Values)
{
    public string? Value(string key) => Values.TryGetValue(key, out var value) ? value : null;
}

/// <summary>
/// The app's system notifications, through the
/// Windows App SDK's App Notifications: files received, a file offered
/// while this computer asks before receiving, and the InputWeave service
/// stopping. <see cref="TransferNews"/> and <see cref="ServiceWatch"/>
/// decide when; this posts them, takes them back, and reports clicks.
/// Registering makes Windows start the app for a click when it is not
/// running (with <see cref="LaunchArguments.NotificationActivated"/>); a
/// click while it runs arrives through <see cref="AppNotificationManager.NotificationInvoked"/>.
/// </summary>
internal sealed class Notifier
{
    public const string ActionKey = "action";
    public const string PageKey = "page";
    public const string TransferKey = "transfer";
    public const string PathKey = "path";

    public const string Accept = "accept";
    public const string Decline = "decline";
    public const string OpenFolder = "open-folder";
    public const string StartService = "start-service";
    public const string ViewUpdate = "view-update";
    public const string Later = "later";

    private const string OfferGroup = "offer";
    private const string ReceivedGroup = "received";
    private const string ProblemGroup = "problem";
    private const string ServiceTag = "service";

    private readonly TransferNews transfers = new();
    private readonly ServiceWatch service = new();
    private bool registered;
    /// <summary>Posting samples, which stay up longer so they can be screenshot and clicked.</summary>
    private bool sampling;

    /// <summary>
    /// Registers with Windows and starts reporting clicks to
    /// <paramref name="clicked"/>, on a background thread. Call early, so
    /// a click that started the app is not lost.
    /// </summary>
    public void Start(Action<NotificationClick> clicked)
    {
        try
        {
            if (!AppNotificationManager.IsSupported())
            {
                AppLog.Warn("app notifications are not supported here");
                return;
            }
            var manager = AppNotificationManager.Default;
            // Before Register: otherwise Windows starts another process for each click.
            manager.NotificationInvoked += (_, args) =>
            {
                AppLog.Info("a notification was clicked");
                clicked(Read(args.Arguments));
            };
            var icon = Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon256.png");
            manager.Register("InputWeave", new Uri(icon));
            registered = true;
            AppLog.Info($"notifications registered ({manager.Setting})");
        }
        catch (Exception e)
        {
            AppLog.Warn($"cannot register for notifications: {e.Message}");
        }
    }

    /// <summary>The click that started the app, if a click did.</summary>
    public static NotificationClick? LaunchClick()
    {
        try
        {
            var activation = AppInstance.GetCurrent().GetActivatedEventArgs();
            if (activation?.Kind == ExtendedActivationKind.AppNotification
                && activation.Data is AppNotificationActivatedEventArgs args)
            {
                return Read(args.Arguments);
            }
        }
        catch (Exception e)
        {
            AppLog.Warn($"cannot read how the app was started: {e.Message}");
        }
        return null;
    }

    /// <summary>
    /// Takes back what the app registered with Windows for this user; for
    /// the uninstaller (<see cref="LaunchArguments.Unregister"/>).
    /// </summary>
    public static void UnregisterAll()
    {
        // Found before the SDK's UnregisterAll, which may remove the values
        // that lead to them; deleted after it, whatever it did. It always
        // leaves the key that names the app by its path, and has been seen
        // to leave the registration and the COM class too.
        List<string> keys = Environment.ProcessPath is { } exe ? RegistrationKeys(exe) : [];
        try
        {
            if (AppNotificationManager.IsSupported())
            {
                AppNotificationManager.Default.UnregisterAll();
            }
        }
        catch (Exception e)
        {
            AppLog.Warn($"cannot unregister notifications: {e.Message}");
        }
        int removed = 0;
        foreach (var key in keys)
        {
            try
            {
                using (var open = Registry.CurrentUser.OpenSubKey(key))
                {
                    if (open is null)
                    {
                        continue;
                    }
                }
                Registry.CurrentUser.DeleteSubKeyTree(key, throwOnMissingSubKey: false);
                removed++;
            }
            catch (Exception e) when (e is System.Security.SecurityException or UnauthorizedAccessException or IOException)
            {
                AppLog.Warn($"cannot remove a notification registration key: {e.Message}");
            }
        }
        AppLog.Info($"notification registration removed ({removed} of {keys.Count} keys there)");
    }

    /// <summary>
    /// The keys of the app's notification registration (<see cref="NotificationRegistration"/>):
    /// the COM class (only when it starts this app), the registration, and
    /// the key that names the app by its path.
    /// </summary>
    private static List<string> RegistrationKeys(string exe)
    {
        var keys = new List<string>();
        try
        {
            var user = Registry.CurrentUser;
            var path = NotificationRegistration.PathKey(exe);
            if (NotificationRegistration.RegistrationKey(Value(user, path, NotificationRegistration.GuidValue)) is { } registration)
            {
                if (NotificationRegistration.ActivatorKey(Value(user, registration, NotificationRegistration.ActivatorValue)) is { } activator
                    && NotificationRegistration.StartsApp(Value(user, activator + @"\LocalServer32", null), exe))
                {
                    keys.Add(activator);
                }
                keys.Add(registration);
            }
            keys.Add(path);
        }
        catch (Exception e) when (e is System.Security.SecurityException or UnauthorizedAccessException or IOException)
        {
            AppLog.Warn($"cannot read the notification registration: {e.Message}");
        }
        return keys;
    }

    private static string? Value(RegistryKey root, string key, string? name)
    {
        using var open = root.OpenSubKey(key);
        return open?.GetValue(name) as string;
    }

    private static NotificationClick Read(IDictionary<string, string> arguments)
    {
        var values = new Dictionary<string, string>(arguments);
        MainPage? page = values.TryGetValue(PageKey, out var name) && Enum.TryParse<MainPage>(name, ignoreCase: true, out var p)
            ? p
            : null;
        return new NotificationClick(values.TryGetValue(ActionKey, out var action) ? action : null, page, values);
    }

    /// <summary>Whether Windows shows the app's notifications.</summary>
    private bool Allowed
    {
        get
        {
            try
            {
                return registered && AppNotificationManager.Default.Setting == AppNotificationSetting.Enabled;
            }
            catch (Exception)
            {
                return false;
            }
        }
    }

    /// <summary>The app is about to restart the service itself: its stopping is not news.</summary>
    public void ServiceRestarting() => service.Restarting(DateTimeOffset.UtcNow);

    /// <summary>
    /// Looks for news in the latest state; called whenever it changes and
    /// every second.
    /// </summary>
    /// <param name="askInWindow">Shows a file offer in the main window when notifications are off.</param>
    public void Update(AgentSnapshot snapshot, ServiceState serviceState, DateTimeOffset now, Action askInWindow)
    {
        var news = transfers.Update(snapshot, now);
        foreach (var id in news.Withdraw)
        {
            Withdraw(OfferTag(id), OfferGroup);
        }
        foreach (var offer in news.Ask)
        {
            if (!Ask(snapshot, offer))
            {
                askInWindow();
            }
        }
        foreach (var files in news.Announce)
        {
            Announce(snapshot, files);
        }
        switch (service.Update(serviceState, now, quiet: !Preferences.FinishedSetup))
        {
            case ServiceNews.Raise:
                ServiceStopped();
                break;
            case ServiceNews.Withdraw:
                Withdraw(ServiceTag, ProblemGroup);
                break;
        }
    }

    /// <summary>
    /// "Received 3 files (120 MB) from Mac mini", or "Received 1 folder (312
    /// files, 1.2 GB) from Mac mini; 2 files could not be sent", with the
    /// names of what arrived, and Open Folder.
    /// </summary>
    private void Announce(AgentSnapshot snapshot, ReceivedFiles files)
    {
        var culture = CultureInfo.CurrentCulture;
        if (TransferText.Received(files.Files, snapshot.NameOf(files.DeviceId), culture) is not { } received)
        {
            return;
        }
        var arrived = files.Arrived;
        var body = string.Join(Strings.Get(TextKeys.NotifyListSeparator), arrived.Select(f => f.Name));
        var builder = new AppNotificationBuilder()
            .AddArgument(PageKey, nameof(MainPage.Files))
            .AddText(Strings.Text(received))
            .AddText(body);
        if (arrived.Select(f => f.Path).FirstOrDefault(p => !string.IsNullOrEmpty(p)) is { } path)
        {
            builder.AddButton(new AppNotificationButton(Strings.Get(TextKeys.NotifyOpenFolder))
                .AddArgument(ActionKey, OpenFolder)
                .AddArgument(PathKey, path));
        }
        Post(builder, $"received-{Guid.NewGuid():N}", ReceivedGroup, $"{arrived.Count} received items");
    }

    /// <summary>
    /// Asks about a file, with Accept and Decline, as urgent as Windows
    /// allows: someone is waiting at the other computer. False when
    /// notifications are off.
    /// </summary>
    private bool Ask(AgentSnapshot snapshot, FileTransfer offer)
    {
        if (!Allowed)
        {
            return false;
        }
        var builder = new AppNotificationBuilder()
            .AddArgument(PageKey, nameof(MainPage.Files))
            .AddText(Strings.Format(TextKeys.NotifyOfferTitle, snapshot.NameOf(offer.DeviceId)))
            .AddText(Strings.Format(TextKeys.NotifyOfferBody, offer.Name, ByteSize.Format(offer.Size, CultureInfo.CurrentCulture)))
            .AddButton(new AppNotificationButton(Strings.Get(TextKeys.TransferAccept))
                .AddArgument(ActionKey, Accept)
                .AddArgument(TransferKey, offer.TransferId))
            .AddButton(new AppNotificationButton(Strings.Get(TextKeys.TransferDecline))
                .AddArgument(ActionKey, Decline)
                .AddArgument(TransferKey, offer.TransferId));
        if (AppNotificationBuilder.IsUrgentScenarioSupported())
        {
            builder.SetScenario(AppNotificationScenario.Urgent);
        }
        return Post(builder, OfferTag(offer.TransferId), OfferGroup, "a file offer");
    }

    /// <summary>The sentence is the body's first line; the app's name heads the notification.</summary>
    private void ServiceStopped()
    {
        var builder = new AppNotificationBuilder()
            .AddArgument(PageKey, nameof(MainPage.Troubleshooting))
            .AddText(Strings.Get(TextKeys.NotifyServiceStopped))
            .AddButton(new AppNotificationButton(Strings.Get(TextKeys.ActionStartService)).AddArgument(ActionKey, StartService));
        Post(builder, ServiceTag, ProblemGroup, "the service stopped");
    }

    /// <summary>
    /// "InputWeave 1.0.1 is available", once per version an automatic
    /// check finds: View Update opens the update dialog.
    /// </summary>
    public void PostUpdate(InputWeave.Updates.UpdateFeed feed)
    {
        var builder = new AppNotificationBuilder()
            .AddArgument(PageKey, nameof(MainPage.General))
            .AddArgument(ActionKey, ViewUpdate)
            .AddText(Strings.Format(TextKeys.UpdatesAvailable, feed.Version))
            .AddText(Strings.Get(TextKeys.UpdateToastBody))
            .AddButton(new AppNotificationButton(Strings.Get(TextKeys.UpdatesView)).AddArgument(ActionKey, ViewUpdate))
            .AddButton(new AppNotificationButton(Strings.Get(TextKeys.UpdateLater)).AddArgument(ActionKey, Later));
        Post(builder, "update", "update", $"InputWeave {feed.Version}");
    }

    /// <summary>A tag names one notification to replace or take back; Windows keeps it to 64 characters.</summary>
    private static string OfferTag(string transfer)
    {
        var tag = $"offer-{transfer}";
        return tag.Length <= 64 ? tag : tag[..64];
    }

    private bool Post(AppNotificationBuilder builder, string tag, string group, string what)
    {
        if (!Allowed)
        {
            AppLog.Info($"not notified of {what}: notifications are off");
            return false;
        }
        try
        {
            if (sampling)
            {
                builder.SetDuration(AppNotificationDuration.Long);
            }
            var notification = builder.BuildNotification();
            notification.Tag = tag;
            notification.Group = group;
            AppNotificationManager.Default.Show(notification);
            AppLog.Info($"notified of {what}");
            return true;
        }
        catch (Exception e)
        {
            AppLog.Warn($"cannot notify of {what}: {e.Message}");
            return false;
        }
    }

    private async void Withdraw(string tag, string group)
    {
        if (!registered)
        {
            return;
        }
        try
        {
            await AppNotificationManager.Default.RemoveByTagAndGroupAsync(tag, group);
        }
        catch (Exception e)
        {
            AppLog.Info($"cannot take back a notification: {e.Message}");
        }
    }

    /// <summary>Posts a sample of each kind, for screenshots and to check the registration.</summary>
    public void PostSample(string kind, AgentSnapshot snapshot)
    {
        sampling = true;
        try
        {
            PostSampleOf(kind, snapshot);
        }
        finally
        {
            sampling = false;
        }
    }

    private void PostSampleOf(string kind, AgentSnapshot snapshot)
    {
        var device = snapshot.Status.Devices.FirstOrDefault()?.Id ?? "sample";
        switch (kind)
        {
            case "offer":
                Ask(snapshot, new FileTransfer
                {
                    TransferId = "sample", DeviceId = device, Name = "Report.pdf", Size = 2_400_000,
                    State = TransferState.WaitingForAnswer,
                });
                break;
            case "service":
                ServiceStopped();
                break;
            case "folder":
                var downloads = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), "Downloads", "InputWeave");
                Announce(snapshot, new ReceivedFiles(device,
                [
                    new FileTransfer
                    {
                        Name = "Photos", Folder = true, Size = 1_288_490_189, Files = 314, FilesDone = 312, FilesFailed = 2,
                        State = TransferState.Done, Path = Path.Combine(downloads, "Photos"),
                    },
                ]));
                break;
            default:
                var folder = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
                Announce(snapshot, new ReceivedFiles(device,
                [
                    new FileTransfer { Name = "Report.pdf", Size = 2_400_000, Path = Path.Combine(folder, "Downloads", "InputWeave", "Report.pdf") },
                    new FileTransfer { Name = "Photo.jpg", Size = 3_100_000 },
                    new FileTransfer { Name = "Notes.txt", Size = 12_000 },
                ]));
                break;
        }
    }
}
