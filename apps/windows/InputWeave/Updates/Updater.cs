using System.ComponentModel;
using System.Diagnostics;
using System.Reflection;
using System.Security.Cryptography;
using InputWeave.Support;

namespace InputWeave.Updates;

/// <summary>Where the updater stands.</summary>
internal enum UpdatePhase
{
    Idle,
    Checking,
    Downloading,
    Verifying,
    /// <summary>msiexec runs; Windows Installer ends the app when it gets to its files.</summary>
    Installing,
}

/// <summary>How an install attempt ended, when the app is still running to see it.</summary>
internal enum InstallOutcome
{
    /// <summary>msiexec is running with the verified installer.</summary>
    Started,
    /// <summary>The user said no to UAC.</summary>
    Declined,
    DownloadFailed,
    /// <summary>The size, SHA-256 or signature did not match: nothing was installed.</summary>
    BadDownload,
    Cancelled,
    /// <summary>msiexec ended without installing (it would have ended the app otherwise).</summary>
    Failed,
    /// <summary>Installed, but Windows has to restart to finish (msiexec 3010 or 1641).</summary>
    RestartNeeded,
}

/// <summary>
/// Checks for a new version and installs it
/// (docs/architecture.md §12.3): the feed about once a day while General ›
/// "Check for updates automatically" is on, and whenever asked; the MSI is
/// downloaded under %LOCALAPPDATA%, checked against its size, SHA-256 and
/// InputWeave's Ed25519 signature, and kept open, so nothing else can
/// change it, until msiexec has it. msiexec runs elevated through UAC; the
/// installer ends the app and opens the new one when it is done. The
/// background service plays no part.
/// </summary>
internal sealed class Updater
{
    private const int MaxInstallerBytes = 512 * 1024 * 1024;

    private readonly HttpClient http;
    private readonly Uri feedUrl;
    private readonly int ownBuild;
    private readonly byte[] publicKey;
    private readonly Action<UpdateFeed> announce;
    private CancellationTokenSource? download;
    /// <summary>The feed being read, when it is.</summary>
    private Task? checking;

    /// <param name="announce">Posts the notification for a version found by an automatic check, once per version.</param>
    public Updater(Uri? feedOverride, Version ownVersion, Action<UpdateFeed> announce)
    {
        feedUrl = feedOverride ?? UpdateFeed.DefaultUrl;
        ownBuild = UpdateRules.BuildOf(ownVersion);
        publicKey = Convert.FromBase64String(KeyForThisBuild());
        this.announce = announce;
        http = new HttpClient { Timeout = TimeSpan.FromMinutes(10) };
        http.DefaultRequestHeaders.UserAgent.ParseAdd($"InputWeave/{App.Version}");
        if (feedOverride is not null)
        {
            AppLog.Info($"updates: reading the feed at {feedUrl}");
        }
    }

    public event Action? Changed;

    public UpdatePhase Phase { get; private set; }

    /// <summary>The newest version the last check found, when it is newer than this one.</summary>
    public UpdateFeed? Found { get; private set; }

    public UpdateVerdict? Verdict { get; private set; }

    /// <summary>The last check could not read the feed.</summary>
    public bool CheckFailed { get; private set; }

    public long Received { get; private set; }

    public long Total { get; private set; }

    /// <summary>
    /// The key update signatures are checked with: InputWeave's, unless
    /// this build was made with another for testing (the UpdatePublicKey
    /// build property, which package builds never set).
    /// </summary>
    private static string KeyForThisBuild()
    {
        var overridden = typeof(Updater).Assembly.GetCustomAttributes<AssemblyMetadataAttribute>()
            .FirstOrDefault(a => a.Key == "UpdatePublicKey" && !string.IsNullOrEmpty(a.Value))?.Value;
        if (overridden is not null)
        {
            AppLog.Warn("updates: this build checks signatures with a test key");
            return overridden;
        }
        return UpdateSignature.PublicKey;
    }

    /// <summary>An automatic check when one is due; called on the app's ticks.</summary>
    public void Tick()
    {
        if (Phase == UpdatePhase.Idle
            && UpdateRules.CheckDue(Preferences.CheckForUpdates, Preferences.LastUpdateCheck, Preferences.LastUpdateFailure, DateTimeOffset.UtcNow))
        {
            _ = CheckAsync(manual: false);
        }
    }

    /// <summary>
    /// Reads the feed; a manual check also shows a version the user skipped.
    /// A manual check while an automatic one is under way waits for it and
    /// then reads the feed itself, so Check for Updates… always has an answer.
    /// </summary>
    public async Task CheckAsync(bool manual)
    {
        if (checking is { IsCompleted: false } running)
        {
            await running;
            if (!manual)
            {
                return;
            }
        }
        if (Phase != UpdatePhase.Idle)
        {
            return;
        }
        checking = ReadFeedAsync(manual);
        await checking;
    }

    private async Task ReadFeedAsync(bool manual)
    {
        Set(UpdatePhase.Checking);
        try
        {
            using var response = await http.GetAsync(feedUrl, HttpCompletionOption.ResponseHeadersRead);
            response.EnsureSuccessStatusCode();
            if (response.Content.Headers.ContentLength > UpdateFeed.MaxBytes)
            {
                throw new InvalidDataException("the feed is too big");
            }
            var json = await response.Content.ReadAsStringAsync();
            var feed = UpdateFeed.Parse(json, UpdateFeed.PrefixFor(feedUrl)) ?? throw new InvalidDataException("not an update feed");
            var verdict = UpdateRules.Judge(feed, ownBuild, manual ? null : Preferences.SkippedUpdate, Environment.OSVersion.Version);
            AppLog.Info($"updates: {feed.Version} (build {feed.Build}) is the newest; this is build {ownBuild}: {verdict}");
            Preferences.LastUpdateCheck = DateTimeOffset.UtcNow;
            CheckFailed = false;
            Verdict = verdict;
            Found = verdict == UpdateVerdict.UpToDate ? null : feed;
            if (!manual && verdict == UpdateVerdict.Available && Preferences.NotifiedUpdate != feed.Version)
            {
                Preferences.NotifiedUpdate = feed.Version;
                announce(feed);
            }
        }
        catch (Exception e) when (e is HttpRequestException or TaskCanceledException or IOException or InvalidDataException)
        {
            AppLog.Info($"updates: cannot read the feed: {e.Message}");
            Preferences.LastUpdateFailure = DateTimeOffset.UtcNow;
            CheckFailed = true;
        }
        finally
        {
            Set(UpdatePhase.Idle);
        }
    }

    /// <summary>Skip This Version: automatic checks say nothing more of it.</summary>
    public void Skip(UpdateFeed feed)
    {
        Preferences.SkippedUpdate = feed.Version;
        Verdict = UpdateVerdict.Skipped;
        Changed?.Invoke();
    }

    public void CancelDownload() => download?.Cancel();

    /// <summary>
    /// Downloads, checks and hands <paramref name="feed"/>'s installer to
    /// msiexec. Returns once msiexec has ended without the installer ending
    /// the app, or the attempt failed; a successful install ends the app
    /// before that.
    /// </summary>
    public async Task<InstallOutcome> InstallAsync(UpdateFeed feed)
    {
        // Install pressed while a check runs: the check finishes first.
        if (checking is { IsCompleted: false } running)
        {
            await running;
        }
        if (Phase != UpdatePhase.Idle || feed.Size > MaxInstallerBytes)
        {
            return InstallOutcome.Failed;
        }
        var cancel = new CancellationTokenSource();
        download = cancel;
        Total = feed.Size;
        Received = 0;
        Set(UpdatePhase.Downloading);
        FileStream? file = null;
        string? folder = null;
        try
        {
            folder = Path.Combine(UpdatesRoot, Convert.ToHexString(RandomNumberGenerator.GetBytes(8)));
            Directory.CreateDirectory(folder);
            var path = Path.Combine(folder, $"InputWeave-{feed.Version}-x64.msi");
            file = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None, 1 << 16, FileOptions.Asynchronous);
            if (!await DownloadAsync(feed, file, cancel.Token))
            {
                return InstallOutcome.DownloadFailed;
            }
            await file.DisposeAsync();
            // Open for reading and shared for reading only, from here until
            // msiexec has ended: what is checked below is what msiexec
            // reads, since no process can write, rename or delete it
            // meanwhile. (msiexec cannot open a file another handle may
            // still write to: 1619.)
            file = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read, 1 << 16, FileOptions.Asynchronous);

            Set(UpdatePhase.Verifying);
            var bytes = new byte[file.Length];
            await file.ReadExactlyAsync(bytes, cancel.Token);
            var check = await Task.Run(() => UpdateSignature.Check(bytes, feed, publicKey));
            if (check != InstallerCheck.Good)
            {
                AppLog.Warn($"updates: the downloaded {feed.Version} is not InputWeave's ({check}); nothing installed");
                return InstallOutcome.BadDownload;
            }
            AppLog.Info($"updates: {feed.Version} checked; handing it to msiexec");

            Set(UpdatePhase.Installing);
            Process? msiexec;
            try
            {
                msiexec = Process.Start(new ProcessStartInfo
                {
                    FileName = Path.Combine(Environment.SystemDirectory, "msiexec.exe"),
                    Arguments = $"/i \"{path}\" /passive",
                    Verb = "runas",
                    UseShellExecute = true,
                });
            }
            catch (Win32Exception e) when (e.NativeErrorCode == 1223)
            {
                AppLog.Info("updates: UAC was declined");
                return InstallOutcome.Declined;
            }
            if (msiexec is null)
            {
                return InstallOutcome.Failed;
            }
            using (msiexec)
            {
                // The installer ends this app when it gets to the app's files;
                // until then the file stays open. Still here when msiexec
                // ends means nothing was installed.
                await msiexec.WaitForExitAsync();
                AppLog.Info($"updates: msiexec ended with {msiexec.ExitCode}");
                return msiexec.ExitCode switch
                {
                    // Installed; Windows has to restart to finish (3010), or is restarting (1641).
                    3010 or 1641 => InstallOutcome.RestartNeeded,
                    1602 => InstallOutcome.Declined,
                    _ => InstallOutcome.Failed,
                };
            }
        }
        catch (OperationCanceledException)
        {
            return InstallOutcome.Cancelled;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or Win32Exception)
        {
            AppLog.Warn($"updates: cannot install {feed.Version}: {e.Message}");
            return InstallOutcome.Failed;
        }
        finally
        {
            file?.Dispose();
            if (folder is not null)
            {
                TryDelete(folder);
            }
            cancel.Dispose();
            download = null;
            Set(UpdatePhase.Idle);
        }
    }

    /// <summary>Leftovers of earlier attempts; called once at start.</summary>
    public static void ClearLeftovers()
    {
        try
        {
            if (Directory.Exists(UpdatesRoot))
            {
                foreach (var folder in Directory.EnumerateDirectories(UpdatesRoot))
                {
                    TryDelete(folder);
                }
            }
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            AppLog.Info($"updates: cannot clear earlier downloads: {e.Message}");
        }
    }

    private static string UpdatesRoot =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "InputWeave", "updates");

    private async Task<bool> DownloadAsync(UpdateFeed feed, FileStream file, CancellationToken cancellation)
    {
        try
        {
            using var response = await http.GetAsync(feed.Url, HttpCompletionOption.ResponseHeadersRead, cancellation);
            response.EnsureSuccessStatusCode();
            await using var body = await response.Content.ReadAsStreamAsync(cancellation);
            var buffer = new byte[1 << 16];
            long last = 0;
            while (true)
            {
                int read = await body.ReadAsync(buffer, cancellation);
                if (read == 0)
                {
                    break;
                }
                if (file.Length + read > feed.Size)
                {
                    AppLog.Warn("updates: the download is larger than the feed says");
                    return false;
                }
                await file.WriteAsync(buffer.AsMemory(0, read), cancellation);
                Received += read;
                if (Received - last >= 256 * 1024)
                {
                    last = Received;
                    Changed?.Invoke();
                }
            }
            await file.FlushAsync(cancellation);
            return file.Length == feed.Size;
        }
        catch (Exception e) when (e is HttpRequestException or IOException || e is TaskCanceledException && !cancellation.IsCancellationRequested)
        {
            AppLog.Info($"updates: cannot download {feed.Version}: {e.Message}");
            return false;
        }
    }

    private static void TryDelete(string folder)
    {
        try
        {
            Directory.Delete(folder, recursive: true);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            AppLog.Info($"updates: cannot remove {folder}: {e.Message}");
        }
    }

    private void Set(UpdatePhase phase)
    {
        Phase = phase;
        Changed?.Invoke();
    }
}
