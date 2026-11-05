using System.Text;
using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage.Pickers;
using WinRT.Interop;

namespace InputWeave.Main;

/// <summary>
/// The state of every moving part, for when something does not work:
/// the problems, the InputWeave service and
/// the background service it starts, whether this computer's keyboard and
/// mouse can control the others, this computer, diagnostics and logs.
/// </summary>
internal sealed partial class TroubleshootingPage : UserControl, IMainPage
{
    /// <summary>Where the background service writes its logs (packaging/windows/InputWeave.wxs).</summary>
    private static readonly string LogFolder = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData), "InputWeave", "logs");

    private readonly StackPanel problems = new() { Padding = new Thickness(16, 12, 16, 12), Spacing = 10 };
    private readonly TextBlock serviceStatus = Value();
    private readonly Button serviceAction = new();
    private readonly TextBlock serviceVersion = Value();
    private readonly TextBlock serviceBuild = Value(monospace: true);
    private readonly TextBlock control = Value();
    private readonly TextBlock controlHint = Ui.Secondary(Strings.Get(TextKeys.TroubleRestartHint));
    private readonly TextBlock thisName = Value();
    private readonly TextBlock deviceId = Value(monospace: true);
    private readonly Button export;
    private AgentSnapshot? model;
    private ServiceState service;
    private string shownProblems = "";

    public TroubleshootingPage()
    {
        serviceAction.Click += (_, _) => MainWindow.RunServiceAction(service);
        var serviceWords = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        serviceWords.Children.Add(new TextBlock { Text = Strings.Get(TextKeys.InspectorStatus) });
        var serviceRight = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12 };
        serviceRight.Children.Add(serviceStatus);
        serviceRight.Children.Add(serviceAction);
        serviceStatus.VerticalAlignment = VerticalAlignment.Center;

        var controlWords = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        controlWords.Children.Add(new TextBlock { Text = Strings.Get(TextKeys.TroubleKeyboardMouse), TextWrapping = TextWrapping.Wrap });
        controlWords.Children.Add(controlHint);

        export = Ui.Button(Strings.Get(TextKeys.TroubleExport), ExportDiagnostics);

        var page = new StackPanel();
        page.Children.Add(Ui.Header(Strings.Get(TextKeys.TroubleProblems)));
        page.Children.Add(Ui.Card(problems));
        page.Children.Add(Ui.Header(Strings.Get(TextKeys.TroubleService)));
        page.Children.Add(Ui.Card(
            Ui.Row(serviceWords, serviceRight),
            Ui.Row(new TextBlock { Text = Strings.Get(TextKeys.TroubleVersion) }, serviceVersion),
            Ui.Row(new TextBlock { Text = Strings.Get(TextKeys.TroubleBuild) }, serviceBuild),
            Ui.Row(controlWords, control)));
        page.Children.Add(Ui.Header(Strings.Get(TextKeys.ConnectionThisComputer)));
        page.Children.Add(Ui.Card(
            Ui.Row(new TextBlock { Text = Strings.Get(TextKeys.InspectorName) }, thisName),
            Ui.Row(new TextBlock { Text = Strings.Get(TextKeys.TroubleDeviceId) }, deviceId),
            Ui.Value(Strings.Get(TextKeys.TroubleVersion), App.Version)));
        var tools = Ui.Card(
            Ui.Row(Ui.Secondary(Strings.Get(TextKeys.TroubleExportHint), caption: false), export),
            Ui.Row(Ui.Secondary(Strings.Get(TextKeys.TroubleLogsHint), caption: false), Ui.Button(Strings.Get(TextKeys.TroubleLogs), OpenLogsFolder)));
        tools.Margin = new Thickness(0, 24, 0, 0);
        page.Children.Add(tools);
        Content = Ui.Page(page);
    }

    private static TextBlock Value(bool monospace = false)
    {
        var text = new TextBlock
        {
            Foreground = Brand.System("TextFillColorSecondaryBrush"),
            IsTextSelectionEnabled = true,
            TextWrapping = TextWrapping.Wrap,
            TextAlignment = TextAlignment.Right,
            MaxWidth = monospace ? 300 : 420,
        };
        if (monospace)
        {
            text.FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Consolas");
        }
        return text;
    }

    public void Invalidate()
    {
        model = null;
    }

    public void Update(AgentSnapshot snapshot, ServiceState serviceState, DateTimeOffset now)
    {
        if (ReferenceEquals(snapshot, model) && serviceState == service)
        {
            return;
        }
        model = snapshot;
        service = serviceState;
        bool connected = snapshot.Link == AgentLink.Connected;

        var rows = ExceptionRows.For(snapshot, serviceState, compact: false);
        bool good = ExceptionRows.EverythingWorks(snapshot, serviceState);
        var key = string.Join("|", rows.Select(r => Strings.Text(r.Text))) + good;
        if (key != shownProblems)
        {
            shownProblems = key;
            problems.Children.Clear();
            foreach (var row in rows)
            {
                problems.Children.Add(ExceptionRowView.For(row, compact: false));
            }
            if (good)
            {
                var ok = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
                ok.Children.Add(new FontIcon { Glyph = "", FontSize = 14, Foreground = Brand.System("SystemFillColorSuccessBrush") });
                ok.Children.Add(new TextBlock { Text = Strings.Get(TextKeys.TroubleAllGood) });
                problems.Children.Add(ok);
            }
            if (problems.Children.Count == 0)
            {
                // Not working yet, and nothing the app can name: the service section says more.
                problems.Children.Add(new TextBlock { Text = Strings.Text(PanelText.Title(snapshot, serviceState, now)) });
            }
        }

        serviceStatus.Text = Strings.Text(DeviceText.ServiceStatus(serviceState, snapshot.Link));
        serviceAction.Visibility = serviceState == ServiceState.NotInstalled ? Visibility.Collapsed : Visibility.Visible;
        serviceAction.Content = MainWindow.ServiceButton(serviceState);
        serviceVersion.Text = connected ? snapshot.AgentVersion : "–";
        serviceBuild.Text = connected && snapshot.AgentDigest.Length != 0 ? snapshot.AgentDigest : "–";
        control.Text = Strings.Get(snapshot.Status.Capturing ? TextKeys.CanControl : TextKeys.CannotControlYet);
        controlHint.Visibility = connected && !snapshot.Status.Capturing ? Visibility.Visible : Visibility.Collapsed;
        thisName.Text = snapshot.Status.ThisDeviceName;
        deviceId.Text = snapshot.Status.ThisDeviceId;
        export.IsEnabled = connected;
    }

    /// <summary>Saves the service's report where the user chooses, then shows it in File Explorer.</summary>
    private async void ExportDiagnostics()
    {
        var picker = new FileSavePicker
        {
            SuggestedFileName = Strings.Get(TextKeys.DiagnosticsFileName),
            SuggestedStartLocation = PickerLocationId.Desktop,
        };
        picker.FileTypeChoices.Add("Text", [".txt"]);
        InitializeWithWindow.Initialize(picker, App.Instance.MainHwnd);
        Windows.Storage.StorageFile? file;
        try
        {
            file = await picker.PickSaveFileAsync();
        }
        catch (Exception e)
        {
            AppLog.Warn($"the save picker failed: {e.Message}");
            return;
        }
        if (file is null)
        {
            return;
        }
        try
        {
            var report = await App.Instance.Model.DiagnosticsAsync();
            await File.WriteAllTextAsync(file.Path, report, new UTF8Encoding(encoderShouldEmitUTF8Identifier: false));
            AppLog.Info("exported diagnostics");
            Shell.ShowInExplorer(file.Path);
        }
        catch (Exception e)
        {
            AppLog.Info($"export diagnostics: {e.Message}");
            DialogWindow.Notice(Strings.Get(TextKeys.DiagnosticsFailed), e.Message);
        }
    }

    /// <summary>
    /// Shows the background service's logs in File Explorer. The installer
    /// lets only SYSTEM and administrators into that folder, and File
    /// Explorer would offer to change that for good; the app says where the
    /// logs are instead, and that the diagnostics hold their latest lines.
    /// </summary>
    private void OpenLogsFolder()
    {
        try
        {
            _ = Directory.EnumerateFileSystemEntries(LogFolder).Any();
        }
        catch (UnauthorizedAccessException)
        {
            AdminOnly();
            return;
        }
        catch (IOException)
        {
            // Missing: the service has not written a log yet.
            DialogWindow.Notice(Strings.Get(TextKeys.LogsMissingTitle), Strings.Get(TextKeys.LogsMissingMessage));
            return;
        }
        var log = Path.Combine(LogFolder, "agent.log");
        if (File.Exists(log))
        {
            Shell.ShowInExplorer(log);
        }
        else
        {
            Shell.OpenFolder(LogFolder);
        }
    }

    private static void AdminOnly()
    {
        var path = new TextBox { Text = LogFolder, IsReadOnly = true, FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Consolas") };
        var copy = Ui.Button(Strings.Get(TextKeys.CopyPath), () =>
        {
            var package = new DataPackage();
            package.SetText(LogFolder);
            Clipboard.SetContent(package);
        });
        var row = new Grid { ColumnSpacing = 8 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.Children.Add(path);
        Grid.SetColumn(copy, 1);
        row.Children.Add(copy);
        DialogWindow.Notice(Strings.Get(TextKeys.LogsAdminOnlyTitle), Strings.Get(TextKeys.LogsAdminOnlyMessage), row);
    }
}
